# Release native mappings when a worker exits

Each per-thread JIT engine owns the executable mappings for its compiled
functions. Cranelift deliberately leaves those mappings allocated on ordinary
drop. WeavePy now explicitly releases them when the engine is destroyed.
Previously, a process that repeatedly created compiling workers accumulated
native code after those workers had exited.

The change preserves native code throughout the engine's lifetime. Suspended
Python generators carry data and a compilation identity; moving one to a new
thread materializes its state instead of entering the former worker's code.
Worker teardown already abandons suspended greenlet stacks before destroying
thread-local JIT state. This change does not reclaim individual compiled
functions while their engine is still alive or resolve recursive metadata
ownership cycles.

## Measurement

Paired measurements use a production-source build of 7837679,
including the accepted process-locale initialization fix. The candidate differs
only in JIT engine destruction, apart from tests and their dev dependency.

The reusable tools/bench_jit_worker_churn.py probe starts and joins 100 workers.
Each defines 20 private numeric functions, runs a 2,000-iteration loop in each,
and clears its namespace. It verifies every worker's combined result. Peak RSS
includes interpreter state, thread stacks, compilation, and native code.

Seven paired cycles at 100 workers give these modified/baseline ratios:

| Metric | JIT | Interpreter |
| --- | ---: | ---: |
| Workload time | 1.012 | 1.009 |
| Process elapsed | 1.010 | 1.005 |
| Process CPU | 1.033 | 0.998 |
| Peak RSS | 0.766 | 1.010 |

Marginal median peak RSS falls from 33.62 MB to 25.63 MB. Reclaiming mappings
costs 1.2% workload time and 3.3% process CPU in this compilation-heavy probe.
The modified runtime still takes 4.801 times CPython's workload time and 1.869
times its peak RSS. A two-worker diagnostic confirms 40 compiled kernels.
The three-cycle full suite gives these geometric means. Workload time covers
23 fixtures; process metrics cover all 24, including startup.

| Metric | JIT/baseline | Interpreter/baseline | JIT/CPython |
| --- | ---: | ---: | ---: |
| Workload time | 0.997 | 0.999 | 1.087 |
| Process elapsed | 1.007 | 0.998 | 1.325 |
| Process CPU | 1.009 | 0.997 | 1.306 |
| Peak RSS | 1.002 | 0.998 | 1.625 |

The first 31-cycle startup run shows JIT elapsed ratios of 1.030 for normal
startup, 1.057 without site, 1.033 in isolated mode, and 1.011 for the import
batch. CPU ratios are 1.024, 1.020, 1.037, and 1.014. Interpreter-only startup
stays near baseline. A second 31-cycle run retains elapsed increases of 3.2%,
6.2%, 3.2%, and 1.4%, respectively. Its CPU increases are 3.0%, 12.2%, 4.4%,
and 1.0%; the small no-site CPU result varies between runs. Startup traces
show no compiled frames, and the native-cache lookup's generated stack frame
is unchanged, so these checks do not establish the cause of the slowdown.
The startup regression remains a limitation of this change.

Seven-cycle rechecks reduce the initial sum/loop process CPU increases from
7.0%/4.6% to 2.8%/2.3%. Both retain about 3.0% more process elapsed time;
workload ratios are 1.003 and 1.017.

At 1,000 workers, three paired cycles give JIT ratios of 0.997 for workload
time, 0.994 for process elapsed, 1.008 for process CPU, and 0.276 for peak RSS.
Marginal median RSS falls from 113.36 MB to 31.27 MB, a 72.4% paired reduction.
This removes the native-page accumulation from exited workers, though other
process memory still grows. The modified runtime still takes 4.804 times
CPython's workload time and 2.155 times its peak RSS in this case.

## Validation

All 63 JIT crate tests pass, including an isolated process that verifies a
native mapping is executable while its engine lives and unmapped after it dies.
The test never reads or calls unmapped memory. All 153 JIT integration tests
pass. The new worker regression verifies shared code and suspended generators
survive eight compiling workers, requiring native parking, resumption, and
cross-thread materialization. It also passes on CPython 3.14.5.

Clippy passes with the two preexisting local exclusions documented in preceding
reports. Thirty-eight selected Python regression scripts pass in JIT, interpreter-only, and free-threaded modes (114 runs), including worker exit, greenlets, shared heaps, calls, generators, collection, finalizers, and the preceding performance regressions. Paired sampling runs after all builds, tests, and lint checks finish.

The preceding retirement test is isolated from other unit tests in bbef24b.
Its assertions require completed collections, but the process-global collector
can skip collection while a parallel test is collecting. A macOS CI collection
assertion failed; isolation prevents parallel tests from occupying the collector
while retaining every assertion.
The baseline's broad local VM run also overflowed the default Rust test-thread
stack in an existing pickle test. The full suite passes all 362 tests with
RUST_MIN_STACK=8388608; no production stack setting changes.

## Reproduction

The macOS x86-64 host, optimized CPython 3.14.5 reference, release profile,
alternating paired samples, and separate frozen caches match the preceding
reports. Cache snapshots stay unchanged during sampling. Ratios are medians
of paired ratios; absolute values above are marginal medians, so their
quotients need not match the paired ratios.

```sh
WEAVEPY_STDLIB_CACHE="$PWD/target/performance/stdlib-cache" \
WEAVEPY_BENCH_LAUNCH_CONTEXT=sandboxed \
python3.14 tools/bench_compare.py \
  --base /path/to/7837679/weavepy --new target/release/weavepy \
  --probe tools/bench_jit_worker_churn.py --work 100 --samples 7 \
  --frozen-cache-root target/performance/fresh-worker-churn-caches \
  --out target/performance/worker-churn.json
```

Repeat with `--work 1000 --samples 3` and fresh output/cache paths for the
scaling case. Raw samples and diagnostics remain under
`target/performance/engine-lifetime-*` and
`target/performance/jit-engine-lifetime/`. Standard fixtures, work sizes,
CI baselines, and gate thresholds are unchanged.

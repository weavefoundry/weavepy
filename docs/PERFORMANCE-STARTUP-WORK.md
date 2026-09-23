# Avoid repeated startup work

Startup imported `site`, which invokes its own `main()`, and then called
`site.main()` again. This repeated path processing and executed `.pth` import
lines twice. Startup now relies on the module's normal initialization.
Explicit calls to `site.main()` still work, and `-S` still suppresses automatic
site processing.

The lean-call path also forced its compilation counter past the startup limit
after only a few dozen calls. A thread-local scope now defers compilation
through `run_site`, including nested calls. Deferred lean counters reset so
the same functions can compile later. Embedders outside this explicit scope
retain the existing hot-code fallback without reporting startup completion.

## Measurement

The baseline is `ec7d1ca`. These measurements use the same macOS x86-64 host,
release configuration, and CPython 3.14.5 reference as the
[attribute-pin measurements](PERFORMANCE-ATTRIBUTE-PINS.md). Paired process
order alternates, one warmup cycle is discarded, and each binary/mode has its
own frozen cache, verified unchanged during measured cycles. Builds, tests,
and profiles finish before timing begins.

Thirty-one cycles of the startup probe give these modified/baseline ratios.
The import case loads `json`, `datetime`, `collections`, and `pathlib`.

| Default JIT | Process elapsed | Process CPU | Peak RSS |
| --- | ---: | ---: | ---: |
| `-c pass` | 0.881 | 0.871 | 0.823 |
| `-I -c pass` | 0.880 | 0.858 | 0.823 |
| `-S -c pass` | 1.028 | 1.038 | 1.001 |
| Imports | 0.972 | 0.973 | 0.989 |

Normal startup's marginal medians fall from 42.83 ms to 37.43 ms and from
15.75 MB to 12.96 MB of peak RSS. Interpreter-only normal startup takes 0.961
times the preceding runtime's elapsed time and 0.986 times its RSS.

Normal startup still takes 1.378 times CPython's elapsed time and 1.237 times
its RSS. Imports remain farther behind at 2.691 times elapsed time and 2.262
times RSS. The `-S` case is faster and smaller than CPython (0.744 times
elapsed time and 0.759 times RSS), but regresses slightly against WeavePy's
preceding revision. No cold filesystem-cache or fresh stdlib extraction claim
is made.

The three-cycle full suite gives these geometric means. Workload time covers
23 fixtures; process metrics include startup for 24 fixtures.

| Metric | JIT/baseline | Interpreter/baseline | JIT/CPython |
| --- | ---: | ---: | ---: |
| Workload time | 1.023 | 1.000 | 1.086 |
| Process elapsed | 0.953 | 0.990 | 1.326 |
| Process CPU | 0.950 | 0.992 | 1.303 |
| Peak RSS | 0.942 | 0.994 | 1.609 |

Moving compiler initialization out of startup exposes its cost in the first
timed invocation of small JIT workloads. Seven-cycle rechecks retain cold
workload ratios of 1.162 for `sumvm`, 1.101 for `nested_loops`, and 1.088 for
`jitloop`. Their complete process elapsed ratios still improve to 0.892,
0.907, and 0.920. The first two workload timers increase by about 0.7 ms.
After an untimed invocation, their workload ratios are 1.004, 1.004, and 1.014;
the corresponding workload CPU ratios are 1.001, 1.000, and 1.011.

Some throughput costs remain: warmed N-body JIT time and CPU are 4.8% higher,
and warmed interpreter-only fannkuch time is 4.1% higher. N-body traces compile
the same workload functions with identical operation counts; the difference
isn't explained by a lost compilation. Warmed spectral-norm JIT time is 2.3%
higher. PyAES's cold JIT slowdown repeats at 5.1%, while its warmed time is
1.0% lower. The initial pi-digits interpreter RSS increase shrinks to 1.6%
in seven-cycle remeasurement. These tradeoffs remain part of the result.

## Validation

The new startup regression passes on CPython and fails on the baseline's
extra `site.main()` call. It checks normal startup, explicit main calls,
`.pth` side effects, `-s`, and `-S`; child processes inherit a requested
`gil=0` mode. An independent diagnostic records one `.pth` execution on
CPython versus two on the baseline.

Two Rust tests cover nested/unwound startup scopes and the same deferred
functions compiling after startup. All 151 JIT unit tests pass. The 33 selected
Python regression scripts pass in JIT, interpreter-only, and free-threaded
modes (99 runs). Traces show no compilation during `-c pass` and successful
compilation of a subsequent user numeric loop. The release CLI build,
formatting, and targeted Clippy pass with the preceding reports' two local
lint exclusions. A no-JIT check passes with the preexisting unused
`code_is_pure_leaf_pub` warning.

```sh
cargo build --release -p weavepy-cli --bin weavepy
WEAVEPY_STDLIB_CACHE="$PWD/target/performance/stdlib-cache" \
WEAVEPY_BENCH_LAUNCH_CONTEXT=sandboxed \
python3.14 tools/bench_startup.py \
  --base /path/to/ec7d1ca/weavepy --new target/release/weavepy \
  --samples 31 --out target/performance/startup-work.json \
  --frozen-cache-root target/performance/fresh-startup-work-caches
```

Use `tools/bench_compare.py` for the standard fixtures and its `--warm` flag
for steady-state checks. Raw samples and traces remain under
`target/performance/startup-defer-*`. Standard fixtures, work sizes, CI
baselines, and thresholds are unchanged.

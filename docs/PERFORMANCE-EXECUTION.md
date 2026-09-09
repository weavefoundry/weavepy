# Execution and compilation performance

The September 8, 2026, execution pass reduces compilation time by 89% to 95%
for the measured large generated modules and reduces the standard string
benchmark's execution time by 45% with the JIT and 48% without it. Across all
23 timed execution fixtures, the geometric mean of paired elapsed-time ratios
improves by 3.2% with the JIT and 2.8% in interpreter mode.

Large joins and bounded whitespace splitting also avoid substantial temporary
storage. The bounded-split probe's peak RSS falls from 217.6 MiB to 32.6 MiB.
General process memory and warm-cache startup are essentially unchanged.
Large-source compilation trades about 3% more peak memory for its speedup;
thread scaling shows no reliable gain. These are measurements on one macOS
ARM64 host, not a claim that every workload or metric improves.

The baseline is an unmodified release build of `971d752`, which already
includes the [startup and memory optimizations](PERFORMANCE.md). Both binaries
use Rust 1.98.1, the same release profile, and the default JIT feature. CPython
3.14.7 is the reference interpreter.
[Raw samples, checksums, environment, probes, and validation](../crates/weavepy-bench/census/2026-09-execution/)
are retained. No existing benchmark fixture, work parameter, expectation, or
regression threshold was removed or relaxed.

The implementation changes are:

- Index compiler names and constants once a pool reaches 32 entries. Small
  pools retain an allocation-free linear search. The temporary indexes hold
  slot numbers, resolve hash collisions with the existing equality rules, and
  preserve the first matching slot. Signed zero, NaN, nested code constants,
  and unordered frozen-set constants keep their existing semantics. The
  flowgraph optimizer resets its index when it compacts the constant pool.
  Indexes belong to compilation and aren't stored in runtime code objects.
- Use ASCII operations for case conversion and whitespace classification,
  retaining the existing Unicode tables and contextual sigma handling for
  non-ASCII input.
- Split directly into reference-counted strings. Bounded whitespace
  splitting scans only the required words instead of allocating an index of
  every character in the input.
- Join exact built-in lists and tuples of strings with one sizing pass and
  one output-building pass. This avoids the intermediate iterator, object
  clones, and copies of individual strings. The general iterator, subclass,
  and surrogate paths remain available, and single-item identity is preserved.
- Replace redundant atomic read-modify-write operations on the internal
  borrow counter with relaxed loads and stores while the owner lock is held.
  The owner's acquire/release operations still synchronize threads; reentrant
  borrow checks remain active. A contended eight-thread Rust regression
  checks shared reads, rejected nested borrows, and publication of mutations.

The main suite runs all 24 existing fixtures, with five measured cycles after
one discarded cycle. Each cycle interleaves the baseline, modified binary,
both interpreter-only variants, and CPython; order reverses on alternate
cycles. Workload timers exclude startup except for the `startup` fixture.
CPU time and peak RSS include initialization.

Every relative result is the median of `modified / baseline` within matched
cycles. Values below 1.000 improve the metric. Geometric means give each
fixture equal weight; the execution aggregate excludes startup but includes
the deque, datetime, and pickle census fixtures.

| Aggregate | JIT | Interpreter |
| --- | ---: | ---: |
| Timed execution, 23 fixtures | 0.968 | 0.972 |
| Process elapsed time, 24 fixtures | 0.977 | 0.979 |
| Process CPU time, 24 fixtures | 0.975 | 0.977 |
| Peak RSS, 24 fixtures | 1.000 | 0.997 |

| Fixture | JIT time | Interpreter time | JIT RSS | Interpreter RSS |
| --- | ---: | ---: | ---: | ---: |
| `fannkuch` | 1.018 | 0.987 | 0.998 | 0.993 |
| `nbody` | 1.037 | 1.004 | 1.000 | 0.997 |
| `fib` | 0.987 | 0.969 | 1.001 | 0.996 |
| `pidigits` | 1.030 | 1.007 | 0.992 | 0.991 |
| `pyaes` | 1.012 | 0.994 | 1.000 | 0.994 |
| `richards` | 0.980 | 0.945 | 1.002 | 0.997 |
| `sumvm` | 1.001 | 0.977 | 1.003 | 0.997 |
| `nested_loops` | 0.957 | 0.998 | 0.999 | 0.999 |
| `jitloop` | 1.036 | 0.984 | 1.004 | 0.995 |
| `jitkernels` | 0.946 | 1.006 | 0.999 | 0.996 |
| `deltablue` | 0.877 | 1.057 | 0.997 | 0.996 |
| `float_math` | 1.033 | 1.046 | 1.001 | 1.000 |
| `spectral_norm` | 1.015 | 0.982 | 1.002 | 0.996 |
| `json_bench` | 1.021 | 1.031 | 0.995 | 0.999 |
| `str_methods` | 0.547 | 0.525 | 1.000 | 0.992 |
| `dict_ops` | 1.006 | 0.994 | 0.997 | 0.996 |
| `list_ops` | 0.998 | 1.003 | 0.994 | 0.993 |
| `attr_access` | 1.014 | 1.010 | 1.001 | 0.998 |
| `call_overhead` | 0.954 | 1.057 | 0.997 | 0.995 |
| `generators` | 0.996 | 0.973 | 0.999 | 0.996 |
| `deque_ops` | 0.946 | 0.993 | 0.999 | 1.000 |
| `datetime_ops` | 1.019 | 0.996 | 1.002 | 1.003 |
| `pickle_bench` | 0.996 | 0.996 | 1.003 | 1.001 |
| `startup` | 1.000 | 0.988 | 1.001 | 0.998 |

The largest elapsed-time increase in this run is 5.7% on interpreter-mode
`call_overhead`. Host speed varied substantially, especially during
`deltablue`: individual timings for the same binary differed by more than a
factor of two. Pairing reduces drift artifacts but doesn't eliminate changes
between adjacent processes. Small differences aren't strong evidence of a
speedup or slowdown. The large string improvement is consistent across the
original and warm measurements.

The follow-up uses seven measured cycles. Each subprocess invokes `bench(n)`
once before timing a second invocation at the original work size. It also
measures CPU time within that second invocation. These results don't reproduce
the original 4% to 6% elapsed-time slowdowns in calls and object workloads.
The 1.9% workload-CPU increase for interpreter-mode `float_math` remains in the
data; uniform throughput improvement isn't established.

| Warm fixture | JIT wall | JIT CPU | Interpreter wall | Interpreter CPU |
| --- | ---: | ---: | ---: | ---: |
| `deltablue` | 0.991 | 0.991 | 0.990 | 0.990 |
| `call_overhead` | 0.996 | 0.996 | 1.001 | 1.001 |
| `float_math` | 0.965 | 0.969 | 0.996 | 1.019 |
| `nbody` | 1.001 | 1.001 | 0.994 | 0.994 |
| `json_bench` | 1.005 | 1.005 | 1.002 | 1.002 |
| `str_methods` | 0.502 | 0.502 | 0.522 | 0.522 |

Supplemental probes run in interpreter mode with seven measured cycles after
one discarded cycle. Startup and import probes use 31 measured cycles instead.
The fresh-cache probe uses seven measured cycles with a new
`WEAVEPY_STDLIB_CACHE` directory for every process, including extraction of the
embedded library. It doesn't flush the operating system's file cache.

The small compilation probe compiles a class/comprehension source 1,500 times.
The larger probes compile 6,000 assignments three times or 20,000 assignments
once. The join probe performs ten joins of 100,000 64-character strings.
Bounded splitting performs ten `split(None, 1)` calls over a long Unicode
string. Case probes apply `lower`, `upper`, and `title` to large ASCII or
Unicode inputs. Exact sources and parameters are in `probes.py`.

Times below are marginal medians; relative ratios remain paired estimates, so
they needn't equal the quotient of the displayed medians. Peak RSS includes
setup and startup.

| Probe | Baseline ms | Modified ms | Time ratio | Baseline MiB | Modified MiB | RSS ratio |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `startup` | 42.13 | 41.86 | 0.989 | 27.89 | 27.75 | 0.996 |
| `startup_no_site` | 13.21 | 13.17 | 1.010 | 18.64 | 18.48 | 0.993 |
| `imports` | 104.48 | 103.14 | 0.993 | 42.00 | 41.89 | 0.997 |
| `compile_small` | 102.98 | 102.44 | 1.000 | 28.22 | 28.20 | 0.997 |
| `compile_6000` | 301.18 | 33.28 | 0.110 | 46.47 | 46.62 | 1.005 |
| `compile_20000` | 927.32 | 50.04 | 0.053 | 78.41 | 80.75 | 1.030 |
| `join_large` | 66.45 | 10.60 | 0.155 | 61.41 | 49.03 | 0.798 |
| `split_bounded` | 30.36 | 0.50 | 0.017 | 217.59 | 32.59 | 0.150 |
| `case_ascii` | 241.42 | 16.09 | 0.067 | 50.23 | 48.23 | 0.960 |
| `case_unicode` | 17.25 | 17.37 | 1.001 | 34.33 | 34.19 | 0.996 |
| `startup_fresh_cache` | 162.06 | 188.49 | 1.038 | 39.44 | 39.33 | 0.997 |

Fresh-cache startup is 3.8% slower in this small-sample run. The warm-cache
startup and import probes are within about 1% of baseline, and small-source
compilation and non-ASCII case conversion are essentially unchanged. The
largest compiler probe uses about 2.3 MiB more peak memory because the new
indexes trade temporary storage for faster lookups.

The existing eight-thread fixture runs at both 1,000,000 and 3,000,000 work
units per worker, with five measured cycles at each size and both GIL modes.
No thread-scaling improvement is claimed. The free-threaded parallel-time
increase shrinks from 5.6% to 1.8% at the larger size, while the GIL-enabled
parallel-time increase grows from 0.2% to 7.3%. Absolute timings also varied
substantially between the runs. These observations don't establish a reliable
change in scaling, and the slower samples aren't discarded.

| Work per worker | GIL | Serial time ratio | Parallel time ratio | Baseline scaling | Modified scaling |
| --- | ---: | ---: | ---: | ---: | ---: |
| 1,000,000 | 1 | 1.014 | 1.002 | 0.93 | 0.94 |
| 1,000,000 | 0 | 1.013 | 1.056 | 4.15 | 4.12 |
| 3,000,000 | 1 | 1.025 | 1.073 | 0.87 | 0.85 |
| 3,000,000 | 0 | 1.004 | 1.018 | 3.93 | 3.83 |

Executable size is essentially unchanged: 44,120,992 bytes before and
44,121,520 bytes after, an increase of 528 bytes. Rust build time,
energy consumption, allocation counts, tail latency under load, and other
hardware platforms weren't measured under controlled conditions.

Validation passes 260 VM unit tests, 25 compiler unit tests, four benchmark
reporting tests, the full run-fixture integration harness, stdlib synchronization,
Clippy with all features and warnings denied, formatting, and the build check
without the JIT. The integration harness uses a real release executable for
subprocess tests and requires localhost socket access.

Both new Python regressions pass on CPython and the final release with the JIT
enabled, disabled, and under `-X gil=0`. The existing free-threading contract
suite passes all 13 tests, and the cross-thread heap regression passes under
`gil=0`.

Twelve targeted CPython suites pass: `test_str`, `test_string`, `test_compile`,
`test_code`, `test_dis`, `test_peepholer`, `test_builtin`, `test_list`, `test_dict`,
`test_gc`, `test_threading`, and `test_threading_local`. `test_marshal` matches
its four preexisting, enumerated divergences, also confirmed on the baseline.
All thirteen gates report zero unexpected results. This is targeted validation,
not a complete rerun of the vendored CPython suite.

To reproduce from the repository root, retain a baseline release executable,
build the modified CLI, and run the following commands while the host is idle:

```sh
cargo build --release -p weavepy-cli --bin weavepy
python3.14 tools/bench_compare.py \
    --base target/release/weavepy-perf-971d752 --new target/release/weavepy \
    --samples 5 --out target/execution-suite.json
python3.14 crates/weavepy-bench/census/2026-09-execution/probes.py \
    --base target/release/weavepy-perf-971d752 --new target/release/weavepy \
    --out target/execution-probes.json
python3.14 tools/bench_compare.py \
    --base target/release/weavepy-perf-971d752 --new target/release/weavepy \
    --warm --samples 7 \
    --fixtures deltablue call_overhead float_math nbody json_bench str_methods \
    --out target/execution-warm.json
python3.14 crates/weavepy-bench/census/2026-09-execution/scaling.py \
    --base target/release/weavepy-perf-971d752 --new target/release/weavepy \
    --work 3000000 --samples 5 --out target/execution-scaling.json
```

Use `--work 1000000` to reproduce the smaller scaling run. The saved probe
results separate fresh-cache startup into `fresh-cache.json`; `probes.py` now
includes it by default. Finish compilation and correctness tests before timing.

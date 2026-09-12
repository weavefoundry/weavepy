# Performance measurements

For the subsequent compiler and string optimizations measured against this
release, see [Execution and compilation performance](PERFORMANCE-EXECUTION.md).
For the later JSON and string work, including comparisons with CPython,
see [JSON and string performance](PERFORMANCE-JSON.md).
For the latest object metadata, numeric text, and enumeration changes,
see [Runtime metadata and numeric text performance](PERFORMANCE-RUNTIME-METADATA.md).

The September 8, 2026, optimization pass reduces warm-cache process startup
from 50.7 ms to 32.3 ms on the measured macOS ARM64 host. Across all 24 existing
benchmark fixtures, peak resident memory falls by 22.2% with the JIT and 23.4%
without it; whole-process CPU time falls by 12.1% and 6.9%, respectively.
These aggregates are geometric means of paired ratios.

Timed execution is mostly unchanged: excluding the startup fixture, the
geometric mean is 0.7% slower with the JIT and 0.6% faster without it. Focused
runs with an untimed workload invocation show modest improvements, but the
measurements don't establish a universal throughput gain. The largest reliable
benefit is avoiding work and memory faults during process initialization.

The baseline is an unmodified release build of `2c6348a`. The modified binary
uses the same release profile and default JIT feature. Measurements use
CPython 3.14.7 as a reference and Rust 1.98.1 to build both binaries.
[Raw samples, binary checksums, and supplemental probes](../crates/weavepy-bench/census/2026-09-performance/)
are retained alongside the existing performance census.

The implementation changes are:

- Compute the embedded standard library's cache fingerprint at build time.
  The build script and VM share the source manifest, data manifest, and hash
  algorithm. Cargo tracks both manifests and the embedded source directory.
  The existing cache key, `c7d92875f7fdf095`, is preserved.
- Probe the module cache with a borrowed string key, removing a temporary
  reference-counted string allocation from each lookup and removal.
- Clear recycled frame metadata into vacant slots, removing placeholder
  reference increments and decrements while releasing user objects promptly.
  Escaped frames and suspended generators retain their live metadata.
- Copy primitive bytecode operands inline at local, constant, and cached
  attribute loads. General object cloning keeps its existing implementation.
- Restore compilation without the JIT by guarding backend-specific GC and
  allocation code with the JIT feature.

The fingerprint change benefits the default extracted standard library layout.
Installed layouts already bypassed the runtime hash. A fresh cache still has
to materialize the library and touch its source pages, so its peak memory
doesn't decrease. Subsequent launches avoid rescanning all embedded sources;
the no-op probes show roughly 10 MiB lower peak RSS.

Startup probes interleave the binaries, reverse their order on alternate
cycles, discard one warmup cycle, and retain 31 measured cycles. "Fresh cache"
uses seven runs per binary, each with a new `WEAVEPY_STDLIB_CACHE` directory;
it doesn't flush the operating system's file cache. Table times are medians,
and relative reductions use the median of paired ratios.

| Launch | Baseline | Modified | Latency reduction | CPython |
| --- | ---: | ---: | ---: | ---: |
| `-c pass` | 50.70 ms | 32.27 ms | 36.4% | 22.08 ms |
| `-S -c pass` | 27.83 ms | 9.53 ms | 66.0% | 14.85 ms |
| Import `json`, `datetime`, `collections`, `pathlib` | 101.60 ms | 82.80 ms | 18.9% | 25.96 ms |
| `-c pass`, fresh cache | 218.48 ms | 158.69 ms | 27.4% | N/A |

For `-c pass`, the observed 95th percentile falls from 56.66 ms to 35.86 ms
(nearest rank, 31 samples). Peak RSS falls from 39.20 MiB to 29.72 MiB.
The no-site probe falls from 28.55 MiB to 18.58 MiB; imports fall from
53.75 MiB to 45.20 MiB. Fresh-cache extraction uses 41.14 MiB before and
41.25 MiB after, effectively unchanged.

The main suite uses every existing work parameter in `fixtures.rs`, five
measured cycles, and one discarded warmup cycle. Each cycle interleaves the
baseline and modified binaries, both with and without the JIT, plus CPython.
Cycle order alternates. No benchmarks or expectations were removed or relaxed.
The JSON retains every sample and each variant's marginal median.

Each ratio below is the median of `modified / baseline` within matched cycles;
values below 1.000 are improvements. Aggregates give each fixture equal weight.
Workload timers exclude startup except for the `startup` fixture, which measures
the entire process. Process CPU and RSS always include initialization.

| Aggregate | JIT | Interpreter |
| --- | ---: | ---: |
| Timed workload, 23 fixtures | 1.007 | 0.994 |
| Whole-process elapsed time, 24 fixtures | 0.884 | 0.932 |
| Whole-process CPU time, 24 fixtures | 0.879 | 0.931 |
| Peak RSS, 24 fixtures | 0.778 | 0.766 |

The executable is essentially the same size: 44,123,104 bytes before and
44,120,992 bytes after, a reduction of 2,112 bytes. Rust build time wasn't
measured under controlled conditions. The new build script also compiles the
shared embedded-source table; these results don't claim a build-time gain.

| Fixture | Work | JIT time | Interpreter time | JIT RSS | Interpreter RSS |
| --- | ---: | ---: | ---: | ---: | ---: |
| `fannkuch` | 100,000 | 1.036 | 0.983 | 0.759 | 0.749 |
| `nbody` | 20,000 | 1.008 | 0.996 | 0.762 | 0.749 |
| `fib` | 27 | 1.024 | 0.968 | 0.759 | 0.748 |
| `pidigits` | 500,000 | 0.991 | 1.016 | 0.769 | 0.757 |
| `pyaes` | 400 | 1.047 | 1.009 | 0.758 | 0.748 |
| `richards` | 50,000 | 1.002 | 0.952 | 0.760 | 0.748 |
| `sumvm` | 2,000,000 | 1.006 | 0.976 | 0.759 | 0.747 |
| `nested_loops` | 120 | 1.002 | 0.977 | 0.758 | 0.748 |
| `jitloop` | 1,000 | 1.005 | 1.003 | 0.757 | 0.749 |
| `jitkernels` | 2,000 | 1.022 | 0.960 | 0.761 | 0.749 |
| `deltablue` | 50 | 0.952 | 1.006 | 0.797 | 0.775 |
| `float_math` | 100,000 | 1.041 | 1.012 | 0.925 | 0.924 |
| `spectral_norm` | 100 | 0.999 | 0.958 | 0.760 | 0.748 |
| `json_bench` | 150 | 1.071 | 1.004 | 0.828 | 0.818 |
| `str_methods` | 15,000 | 0.990 | 1.046 | 0.766 | 0.749 |
| `dict_ops` | 100,000 | 0.985 | 0.989 | 0.758 | 0.750 |
| `list_ops` | 10,000 | 1.013 | 1.000 | 0.760 | 0.749 |
| `attr_access` | 200,000 | 1.023 | 0.964 | 0.769 | 0.748 |
| `call_overhead` | 150,000 | 0.978 | 0.978 | 0.759 | 0.750 |
| `generators` | 300,000 | 0.995 | 1.012 | 0.758 | 0.750 |
| `deque_ops` | 200,000 | 1.014 | 1.044 | 0.840 | 0.834 |
| `datetime_ops` | 60,000 | 0.988 | 1.013 | 0.780 | 0.766 |
| `pickle_bench` | 40 | 0.983 | 0.998 | 0.821 | 0.808 |
| `startup` | 1 | 0.640 | 0.625 | 0.759 | 0.746 |

Host speed varied during the long run. For example, the datetime interpreter
samples dropped from about 15 seconds to about 11 seconds for both binaries.
Dividing their independent medians produces an apparent 28% slowdown, while
the median paired ratio is 1.013. Pairing is used consistently for every row;
it reduces this drift artifact but cannot eliminate variation between adjacent
processes. CPU time avoids descheduling noise but still depends on core speed.
Treat small differences as inconclusive rather than guaranteed gains or losses.

Focused follow-up runs execute `bench(n)` once before timing a second call in
the same process. They also measure CPU time inside the second call. This
separates warmed execution from process startup and first-entry JIT compilation.
The original work parameters are retained except for datetime, explicitly
reduced to 6,000 iterations for this follow-up. Five measured cycles follow
one discarded cycle.

| Warmed fixture | Work | JIT wall | JIT CPU | Interpreter wall | Interpreter CPU |
| --- | ---: | ---: | ---: | ---: | ---: |
| `fannkuch` | 100,000 | 0.990 | 0.990 | 0.996 | 0.995 |
| `nested_loops` | 120 | 1.002 | 1.003 | 0.980 | 0.980 |
| `attr_access` | 200,000 | 0.969 | 0.969 | 0.981 | 0.981 |
| `call_overhead` | 150,000 | 1.008 | 1.005 | 0.981 | 0.981 |
| `generators` | 300,000 | 0.999 | 0.999 | 0.991 | 0.990 |
| `str_methods` | 15,000 | 0.979 | 0.979 | 0.985 | 0.985 |
| `json_bench` | 150 | 0.994 | 0.995 | 0.994 | 0.994 |
| `datetime_ops` | 6,000 | 0.992 | 0.992 | 0.985 | 0.986 |

The JSON and datetime slowdowns didn't reproduce in these warmed checks.
This doesn't establish that every cold invocation is unaffected. In particular,
the JSON time in the main JIT suite remains 7.1% higher in the recorded data.

Supplemental interpreter-only probes use seven measured cycles after one
warmup cycle. They time 500,000 identity calls and additions, 1,500 compilations
of a small class/comprehension source, or 100,000 imports of already-loaded
`math`. The call probe's variability makes its marginal medians and paired
ratio differ noticeably; it doesn't establish a reliable call-speed gain.

| Probe | Baseline median | Modified median | Paired time ratio | CPython median |
| --- | ---: | ---: | ---: | ---: |
| `calls` | 180.44 ms | 180.16 ms | 0.958 | 18.41 ms |
| `compile` | 79.49 ms | 82.38 ms | 1.008 | 74.11 ms |
| `cached_import` | 34.07 ms | 28.35 ms | 0.826 | 7.83 ms |

The existing eight-thread scaling fixture was also run with 1,000,000 work
units per worker. Three interleaved measured cycles follow one discarded cycle.
These are small-sample observations, not a new scaling guarantee. Absolute
serial and parallel times matter alongside the ratio: the default GIL mode
uses the JIT and finishes this kernel much sooner than free-threaded mode.

| Binary and mode | Serial median | Eight-thread median | Serial / parallel |
| --- | ---: | ---: | ---: |
| Baseline, `gil=1` | 32.30 ms | 34.79 ms | 0.93 |
| Modified, `gil=1` | 32.29 ms | 34.84 ms | 0.93 |
| Baseline, `gil=0` | 1208.42 ms | 246.27 ms | 4.91 |
| Modified, `gil=0` | 1174.33 ms | 225.65 ms | 5.20 |

Validation covers the VM's 259 unit tests, the run-fixture integration harness,
the stdlib synchronization integration test (467 verbatim files, no drift),
Clippy with all features and warnings denied, formatting, and a build check
without default features. Four benchmark-reporting tests check paired estimates,
host-speed drift, mismatched sample counts, and separate workload CPU timing.
The new frame-recycling regression passes on CPython and the final release
binary with the JIT enabled, disabled, and under `-X gil=0`. It checks reference
identity, escaped frame state, closures, weak-reference lifetimes, suspended
generators, and direct `sys.modules` changes with a Unicode name.

The final release also passes twelve targeted CPython 3.14 suites:
`test_frame`, `test_sys_settrace`, `test_sys_setprofile`, `test_monitoring`,
`test_traceback`, `test_inspect`, `test_generators`, `test_gc`, `test_weakref`,
`test_builtin`, `test_import`, and `test_threading`. The census includes their
per-suite status and reported test counts. This is targeted validation, not a
rerun of the entire vendored CPython suite.

To reproduce the main comparison after saving an unmodified release binary:

```sh
cargo build --release -p weavepy-cli --bin weavepy
python3.14 tools/bench_compare.py \
    --base /path/to/original/weavepy \
    --new target/release/weavepy \
    --samples 5 \
    --frozen-cache-root target/performance-comparison-caches \
    --out target/performance-comparison.json
```

Use `--fixtures call_overhead attr_access generators` for a focused run. Add
`--warm` for an untimed invocation before each timed workload and a separate
`work_cpu_ns` measurement. The warmup and module wrapper are included in
whole-process CPU and RSS. `--work N` overrides the work parameter only when
exactly one fixture is selected, and the JSON records the actual work value.

Use a fresh `--frozen-cache-root` for each comparison. It gives each executable
its own frozen standard-library cache and checks that the artifacts stay
unchanged after the discarded preparation cycle. Builds with different embedded
Python sources otherwise invalidate the same module cache file when alternating,
charging recompilation time and peak memory to whichever variant runs next.
Historical runs without this option retain that limitation; their raw results
shouldn't be interpreted as isolated runtime costs when the sources differ.

The supplemental startup and microbenchmark scripts in the census directory
reproduce their workloads and alternating order. Run them from the repository
root with the saved original binary at `target/release/weavepy-perf-base`;
results go to `tmp/performance-20260908`. They record macOS resource-usage
units. The general comparison tool supports Unix `wait4` and normalizes RSS
units on macOS and Linux.

Finish compilation and tests before timing, keep the host otherwise idle, and
keep each binary beside its distribution files when testing subprocess or
embedding behavior. These results cover one host and a targeted compatibility
suite. They don't measure every application, platform, energy budget, allocation
count, or long-running memory profile. Compilation throughput and sustained
execution remain areas for further profiling; the recorded results don't
support claiming that every meaningful metric improved.

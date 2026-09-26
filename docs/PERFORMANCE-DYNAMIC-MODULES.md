# Dynamic-module pickle and full-suite comparison

Classes defined in an exact `types.ModuleType` instance previously missed the
native pickle path because the runtime represents those modules differently
from imported modules. Global resolution now reads either representation's
namespace directly. It still rejects module subclasses, missing attributes,
and native extension storage, leaving their hooks to the existing pickler.
The same resolution serves encoding and decoding.

## Dynamic-module workload

The existing `pickle_bench` fixture is loaded into a module created with
`types.ModuleType`, as `tools/bench_compare.py --warm` normally does. Each
process runs `bench(40)` once before timing a second invocation. Seven
interleaved measured cycles follow a discarded cycle. The preceding release
is `5acfb8e`; the CPython configuration is recorded in
[Small-call performance](PERFORMANCE-CALL-RETURNS.md).

| Modified/preceding release | Default JIT | Interpreter |
| --- | ---: | ---: |
| Workload elapsed time | 0.00897 | 0.00943 |
| Workload CPU time | 0.00897 | 0.00942 |
| Whole-process elapsed time | 0.02341 | 0.02329 |
| Whole-process CPU time | 0.02260 | 0.02249 |
| Peak RSS | 0.840 | 0.853 |

The default-JIT median workload time falls from 2.669 seconds to 24.3
milliseconds. Ratios are medians of matched cycles, so they needn't equal the
quotient of marginal medians. The new workload still takes 2.445 times
CPython's time and 2.030 times its peak RSS. The final executable is 51,830,016
bytes, unchanged by this step and 408 bytes smaller than the original release.

The earlier 400-iteration warmed run exposed the fallback and was stopped
before it completed; it isn't counted as a finished comparison. The
400-iteration direct-script measurements in
[Pickle bookkeeping performance](PERFORMANCE-PICKLE-MEMO.md) are separate,
completed comparisons.

## Full standard suite

All 24 existing fixtures retain their default work sizes. Three interleaved
measured cycles follow a discarded cycle, with separate warmed frozen caches
for each executable. Cache contents are checked for stability during timing.
The original baseline is `82f7237`, before all three changes.

| Geometric mean, modified/original | Default JIT | Interpreter |
| --- | ---: | ---: |
| Workload elapsed time, excluding startup | 0.996 | 0.998 |
| Whole-process elapsed time | 0.996 | 0.997 |
| Whole-process CPU time | 0.994 | 1.000 |
| Peak RSS | 1.004 | 1.004 |

These broad results are essentially flat. Against CPython, the default-JIT
workload geometric mean is 1.080, process elapsed time is 1.389, process CPU
is 1.382, and peak RSS is 1.727. There are eight workload-time wins among 23
workloads and no peak-RSS wins among the 24 processes. The target of beating
CPython across all measured metrics hasn't been reached.

A final seven-cycle warmed recheck of `tools/bench_leaf_returns.py` at
500,000 iterations confirms the focused call improvement on the final binary:
elapsed-time ratios are 0.906 with the JIT and 0.905 without it, relative to
the original release. Workload CPU ratios are 0.906 and 0.905, process CPU
ratios are 0.903 and 0.909, and peak-RSS ratios are 0.995 and 0.987. The
default-JIT workload remains 3.198 times CPython's time. These final measurements
are in `target/performance/final-return-probe.json`; the earlier call report
records the first commit's measurement separately.

The following default-JIT rows retain every fixture, including the library
census. Smaller ratios indicate less cost. The startup row uses process
elapsed time in the workload columns.

| Fixture | Time/original | Time/CPython | Process CPU/CPython | RSS/CPython |
| --- | ---: | ---: | ---: | ---: |
| fannkuch | 1.027 | 1.624 | 1.616 | 1.708 |
| nbody | 0.980 | 1.091 | 1.231 | 1.774 |
| fib | 0.975 | 1.459 | 1.553 | 1.581 |
| pidigits | 1.009 | 0.749 | 0.758 | 1.576 |
| pyaes | 1.009 | 0.693 | 1.081 | 1.582 |
| richards | 0.983 | 3.238 | 2.387 | 1.744 |
| sumvm | 0.984 | 0.057 | 0.421 | 1.550 |
| nested_loops | 1.010 | 0.100 | 0.461 | 1.586 |
| jitloop | 0.991 | 0.088 | 0.375 | 1.587 |
| jitkernels | 1.001 | 0.780 | 1.023 | 1.588 |
| deltablue | 1.014 | 6.705 | 5.322 | 2.153 |
| float_math | 0.978 | 3.185 | 2.851 | 2.359 |
| spectral_norm | 1.000 | 0.450 | 0.759 | 1.577 |
| json_bench | 0.954 | 1.462 | 1.814 | 2.390 |
| str_methods | 0.977 | 1.603 | 1.584 | 1.686 |
| dict_ops | 0.990 | 1.857 | 1.781 | 1.541 |
| list_ops | 1.010 | 1.556 | 1.565 | 2.225 |
| attr_access | 0.987 | 2.637 | 2.314 | 1.788 |
| call_overhead | 1.005 | 2.974 | 2.687 | 1.626 |
| generators | 1.032 | 2.914 | 2.529 | 1.561 |
| deque_ops | 1.002 | 3.213 | 2.857 | 1.495 |
| datetime_ops | 1.056 | 0.262 | 0.338 | 1.611 |
| pickle_bench | 0.934 | 2.799 | 2.420 | 2.072 |
| startup | 1.013 | 1.513 | 1.543 | 1.529 |

Seven-cycle rechecks don't reproduce the initial 6.2% interpreter slowdown
in `str_methods` or the 6.2% interpreter startup CPU increase. Their repeated
ratios are 0.984 for string workload time and 0.980 for startup CPU.
`datetime_ops` retains a 4.0% default-JIT first-run slowdown relative to the
original release; its warmed time ratio is 1.001. Its interpreter ratios are
1.027 on first execution and 1.028 when warm. These are retained tradeoffs,
not hidden by the suite average.

The largest remaining standard-workload gap is DeltaBlue, at 6.705 times
CPython's time. The removed identity experiment and the profile evidence in
the earlier reports show why compiling more operations alone didn't solve
that method-dispatch cost. Startup and memory remain separate gaps.

## Validation and reproduction

Five regression scripts pass in default JIT, interpreter-only, and
free-threaded modes: built-in pickle data, native instances, callables,
dynamic modules, and simple returns. The new dynamic-module test also passes
on CPython. It checks reference bytes, aliases, nested classes, lookup hooks,
class rebinding, and profiler evidence that supported cases use native code.
Its native-path assertion fails on the preceding WeavePy release; its behavior
checks pass there.

The release CLI builds successfully. Targeted Clippy with all VM features and
targets passes when two existing lint categories are excluded. Strict local
Clippy on Rust 1.94 reports one `let_and_return` warning in unchanged
`object.rs` and four `cast_ptr_alignment` warnings in unchanged
`stdlib/socket_mod.rs`. No lint suppressions were added to source code.

```sh
cargo build --release -p weavepy-cli
WEAVEPY_STDLIB_CACHE="$PWD/target/performance/stdlib-cache" \
python3.14 tools/bench_compare.py \
  --base /path/to/preceding/weavepy --new target/release/weavepy \
  --fixtures pickle_bench --warm --samples 7 \
  --frozen-cache-root target/performance/fresh-module-caches \
  --out target/performance/modules.json

WEAVEPY_STDLIB_CACHE="$PWD/target/performance/stdlib-cache" \
python3.14 tools/bench_compare.py \
  --base /path/to/original/weavepy --new target/release/weavepy \
  --samples 3 --frozen-cache-root target/performance/fresh-suite-caches \
  --out target/performance/suite.json
```

Raw measurements are retained under `target/performance/`: `modules-focused`,
`final-suite`, `final-recheck`, `final-startup`, and `final-datetime-warm` JSON
files. No CI baselines, standard fixture work sizes, or gate thresholds were
changed.

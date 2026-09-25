# Compiled scalar field-update plans

Compiled bound methods that add an exact integer to one instance field and
return that field can now complete without another frame, argument buffer, or
pin table. The compiled artifact owns an update plan, and its attribute guards
supply the class version, field name, and dictionary index. Other functions
allocate no update plan. Interpreter dispatch, bytecode cache encoding, and
pure-leaf classification retain their preceding implementation.

The ordinary native-call preflight still validates function and code identity,
binding, defaults, argument lanes, namespaces, observers, recursion, and
retirement accounting. The helper checks the live class, field key, exact
integer operands, and overflow before one final store. A result-lane mismatch
preserves the completed value and resumes after the call; it never replays the
store. GIL, dictionary-watcher, observer, and unusual-key guards remain in force.
Slots, hooks, properties, mutable class values, and large integers retain their
ordinary paths. No admission, density, pin, benchmark, or CI thresholds change.

Plan construction excludes class values whose own type could later acquire
descriptor hooks. It rejects unusual class dictionary keys before lookup, even
when ordinary attribute access has already been cached. Plans own no instance
or class and follow their compiled artifact's lifetime. Every compiled artifact
adds an optional plan pointer, and eligible artifacts own a boxed plan. This
isn't a claim of zero additional memory cost.

## Measurement

The baseline is accepted runtime `4377ce1`, with fixture and documentation
changes through `0d721e6`. Measurements use standard release settings, Intel
macOS, optimized CPython 3.14.5, separate frozen caches, alternating paired runs,
and discarded warmup. No owned build, test, profile, or other benchmark overlaps
timing. Desktop indexing and media-analysis activity was recorded; small
differences remain provisional. All samples, including losses, are retained.
Ratios below are modified/baseline, with lower values indicating less cost.

Seven-cycle first-invocation and sustained comparisons give:

| Workload | Standard JIT | Sustained JIT | Sustained interpreter | Sustained JIT/CPython |
| --- | ---: | ---: | ---: | ---: |
| Richards-style fixture | 0.523 | 0.479 | 1.037 | 1.615 |
| Attribute access | 0.636 | 0.589 | 0.964 | 1.503 |
| Mixed calls | 0.945 | 0.978 | 0.999 | 2.788 |
| DeltaBlue | 0.980 | 0.995 | 1.008 | 6.678 |

Sustained work counts are 250,000 for Richards, 500,000 for attribute access,
750,000 for mixed calls, and 250 for DeltaBlue. A separate warmed Richards run
at 250,000 gives 0.454 workload time and 0.453 workload CPU, with an interpreter
time ratio of 1.009. Its JIT workload still takes 1.680 times CPython.

The unchanged six-mode update probe records JIT time ratios of 0.452 for
parameters, 0.521 for constants, 0.480 for defaults, and 0.610 for saved methods.
Their CPython ratios are 0.779, 1.044, 1.007, and 1.648. The separate class-default
probe remains interpreted. Twenty-one existing call, field-argument, and
fallback controls also ran for seven cycles.

The three-cycle full suite gives these geometric means. Workload time covers
23 fixtures; process metrics cover all 24, including startup.

| Metric | JIT/baseline | Interpreter/baseline | JIT/CPython |
| --- | ---: | ---: | ---: |
| Workload time | 0.954 | 1.020 | 1.023 |
| Process elapsed time | 0.974 | 1.012 | 1.392 |
| Process CPU | 0.972 | 1.008 | 1.253 |
| Peak RSS | 1.016 | 1.005 | 1.574 |

These results don't establish overall CPython parity. The change is retained
for its substantial compiled field-update gains, with the following costs and
limits rather than an assertion that every workload improves.

## Rechecks and retained costs

Seven-cycle million-call controls give JIT/interpreter ratios of 1.005/0.997
for custom setters, 1.028/0.976 for slots, 0.985/1.003 for mutable class values,
and 1.001/1.022 for large integers. The preceding native-only draft's roughly
9% custom-setter interpreter regression isn't present in this candidate.

Eleven full-suite rows were independently repeated for seven cycles. The large
initial N-body, AES, loop-kernel, and list-RSS increases shrink or disappear.
N-body's interpreter time remains 1.022, the JIT loop's time 1.026, and JSON's
JIT peak RSS 1.041. JSON's original RSS ratio was 1.052. List RSS is 1.002 on
repeat, while its interpreter time increases to 1.095; a separate warmed run
at 50,000 gives 0.998 interpreter time and workload CPU. These different
scenarios don't replace the original rows.

Warmed static slot reads retain time and workload-CPU ratios of about 1.026
in JIT and 1.028 in the interpreter. Warmed string-method ratios are 1.014 and
1.025, also reflected in workload CPU. A million-call first-invocation global
scalar probe records 1.064 JIT time; its warmed counterpart records 0.913 time
and 0.915 workload CPU. Initial short controls and longer runs don't support
claiming a uniform gain or loss for this case.

Two 31-cycle startup batches cover normal, no-site, isolated, and import
startup. The first JIT elapsed ratios are 1.028/1.018/1.010/1.001; the repeat
records 1.007/1.019/1.008/1.017. Repeat normal CPU is 1.021, and normal/import
elapsed remain 1.682/2.631 times CPython. Startup and memory remain unfinished
performance targets.

## Validation and reproduction

All 392 VM tests pass, along with embedding on the existing 1 MiB worker stack,
VM Clippy with the established local exclusions, no-default compilation, and
formatting. The frozen release passes 297 runs across 99 regression fixtures,
1,048 semantic probes, twelve CPython fixtures, and seven extra constant/shape
checks. All 53 probe and three application compilation-decision traces match
the accepted runtime with statistics disabled.

Tests count actual native completions, require unsupported calls to bypass the
evaluator, and exercise an integer update returned to a caller compiled for
floats. They cover overflow, hooks, descriptor and MRO changes, dictionary
replacement, observers, defaults, code replacement, saved-method ownership,
weak references, and final release. Isolated guard tests cover independent
descriptor-class mutation and unusual keys after cached ordinary attribute
access. An earlier plan-construction draft omitted the latter guard; it was
corrected before any performance measurements were collected.

```sh
cargo build --release -p weavepy-cli --bin weavepy
WEAVEPY_STDLIB_CACHE="$PWD/target/performance/stdlib-cache" \
python3.14 -B tools/bench_compare.py \
  --base /path/to/4377ce1/weavepy --new target/release/weavepy \
  --samples 7 --fixtures richards attr_access call_overhead deltablue \
  --frozen-cache-root target/performance/fresh-compiled-update-caches \
  --out target/performance/compiled-updates.json
```

The frozen CLI is 51,881,896 bytes, SHA-256
`0133dab389b4f038a07f88f134216f52d6458e778be3501c997f3d581a058a30`.
Its source patch against `0d721e6`, before this report, is
`d4be86d4f11c42fe1aa23bc770d9da027743544db50e84363a8aa7487faf772a`.
Exact sources, profiles, traces, and logs remain under
`target/performance/compiled-field-update-guard-investigation/`; raw comparisons
use the `target/performance/compiled-field-guard-r0d721e6-*` prefix.
The earlier interpreter-fused and native-only cache-based drafts remain
unadopted. This implementation keeps its optimization metadata in compiled
artifacts instead.

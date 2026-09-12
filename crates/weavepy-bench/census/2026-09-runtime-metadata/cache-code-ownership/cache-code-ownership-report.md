# Ownership of cached code

Both bytecode-cache loaders now take ownership of freshly decoded code through Arc::unwrap_or_clone. The decoder has already released its reference table, so the normal case can reuse the root and relocate nested filenames in place. Shared roots retain the existing clone fallback, and shared nested code still uses copy-on-write. No new unsafe code, object layout, bytecode format, or public Rust API is introduced.

Candidate SHA-256: `a6ff21200ed9d5d0d1eaedd103a136b4a8eb5600bb0e3ead863364f3f7304f8d`. The executable is 44,212,288 bytes, 384 bytes above the preceding range-formula release. All 12 measured layouts are unchanged. The snapshot records 124 source files and 37 measurement inputs.

All 323 VM tests, 52 JIT tests, Clippy, and workspace/all-feature/no-JIT checks pass. Two new VM tests cover unique nested ownership, relocation, and externally shared code. The permanent relocation fixture covers closures, methods, generators, coroutine returns, traceback filenames, repeated cache loads, and corrupt-cache fallback. It passes on CPython, the preceding release, and the candidate in all three configured parent VM modes. Child imports inherit the JIT environment; the parent GIL-disabled flag is not explicitly passed to those children.

The expanded compatibility run passes 268 of 270 checks. One new check selects a configured marshal divergence; another import prefix also selects test_importlib, which fails. Exact before/after reruns show identical marshal and importlib failures, while test_import.py passes on both releases. Comparison normalizes only reported durations and addresses in object representations, retaining complete raw details. The validation controller now selects the import filename exactly and keeps marshal/importlib diagnostics separate. Both controller versions are retained; the compiled candidate did not change. These results do not establish full CPython compatibility.

Five independent benchmark return checks match CPython on first and warmed calls. Existing native execution, builder, and scalar-leaf checks pass. Initial test-fixture path aliasing, test-module placement, and trailing-whitespace failures were fixed before release validation; their initial artifacts remain available.

An identical-binary startup calibration found executable-dependent stdlib metadata rewrites on all 15 shared-cache launches preceded by a different executable. Median paired elapsed and CPU ratios versus self-preceded launches are 1.0311 and 1.0297. Separate caches avoid rewrites; the corresponding elapsed ratio is 1.0058. This controlled result does not retroactively alter historical measurements or assign causes to unrelated timing shifts.

The standard cold/warm controls retain the historical launch/cache arrangement and use seven alternating paired cycles after a discarded cycle. Their workload timers exclude process startup. All timing runs use the declared outside-sandbox condition. The focused preceding release is range-formula 337990; checkpoint comparisons retain 9a69c41.

The ownership change has a measurable tradeoff. Relocated startup is about 1.9 percent faster and relocated imports about 2.9 percent faster, while peak RSS rises about 1.1 and 3.4 percent. Matching-path startup and imports are essentially unchanged. The no-site control takes about 2.7 percent more elapsed time. Standard list-operation timers improve about 3.9 percent cold and 1.7 percent warm, but all ten standard controls use about 1.0 to 1.3 percent more peak RSS. These regressions remain explicit.

## Standard controls

| Condition | Fixture | Time/previous | CPU/previous | RSS/previous | Time/CPython | RSS/CPython |
|---|---|---:|---:|---:|---:|---:|
| cold | str_methods | 1.007604 | 0.994577 | 1.011858 | 2.016684 | 2.202151 |
| cold | list_ops | 0.961456 | 0.963928 | 1.010953 | 13.611029 | 1.987083 |
| cold | attr_access | 0.997958 | 0.991427 | 1.011659 | 3.140062 | 2.042965 |
| cold | call_overhead | 1.000403 | 0.998391 | 1.012678 | 9.025575 | 2.054662 |
| cold | jitkernels | 0.999598 | 0.978018 | 1.010232 | 0.868159 | 1.993617 |
| warm | str_methods | 0.994872 | 0.989690 | 1.013132 | 1.930592 | 2.171728 |
| warm | list_ops | 0.982541 | 0.984861 | 1.009683 | 13.542398 | 1.963504 |
| warm | attr_access | 0.990228 | 0.988130 | 1.010983 | 3.100679 | 2.014553 |
| warm | call_overhead | 0.999636 | 0.997405 | 1.013014 | 9.048518 | 2.028096 |
| warm | jitkernels | 1.002758 | 0.984365 | 1.010627 | 0.840413 | 1.964876 |

## Controlled startup and imports

These supplemental probes use equal-length executable paths, separate stdlib metadata caches, and separate frozen-code caches. Matching conditions serialize code under the same stdlib path used when measuring. Relocated conditions serialize under an equally long seed path before loading under the target path. The probe verifies actual serialized filenames, target stdlib paths, executable hashes, and unchanged frozen artifacts across timing. Each case uses 31 alternating paired cycles after a discarded cycle. Native compilation is disabled for these process-startup probes. OS caches are not flushed; this is warm-cache startup, not first extraction.

| Condition | Elapsed/previous | CPU/previous | RSS/previous | Elapsed/CPython | CPU/CPython | RSS/CPython |
|---|---:|---:|---:|---:|---:|---:|
| startup_matched | 0.999318 | 0.998871 | 1.001177 | 1.390946 | 1.438694 | 1.854189 |
| startup_relocated | 0.981250 | 0.977656 | 1.010682 | 1.387786 | 1.434122 | 1.855277 |
| no_site_matched | 1.026566 | 1.018512 | 0.994078 | 0.627104 | 0.591783 | 1.550132 |
| imports_matched | 0.998438 | 0.999366 | 1.000805 | 2.799130 | 2.971518 | 2.519757 |
| imports_relocated | 0.970518 | 0.968623 | 1.033624 | 2.830641 | 3.006007 | 2.526849 |

## Full census

The 24-fixture refresh uses five alternating paired cycles after a discarded cycle, with the preceding range-formula release and checkpoint 9a69c41. Aggregates are geometric means of per-fixture median paired ratios. Workload time excludes startup, leaving 23 timers; process elapsed time, CPU time, and actual OS peak RSS include all 24. All improvements and regressions remain in the raw samples.

| Metric | JIT/checkpoint | Interpreter/checkpoint | JIT/previous | Interpreter/previous | JIT/CPython | Interpreter/CPython |
|---|---:|---:|---:|---:|---:|---:|
| ns | 0.828193 | 0.961177 | 0.996320 | 0.994126 | 3.552027 | 9.571193 |
| wall_ns | 0.883657 | 0.969479 | 0.982657 | 0.994052 | 3.366654 | 5.883273 |
| cpu_ns | 0.881610 | 0.969414 | 0.983332 | 0.993658 | 3.449547 | 6.093438 |
| rss_bytes | 0.950892 | 0.945261 | 1.005125 | 1.012411 | 2.104921 | 1.938585 |

Against the preceding full range-formula census, aggregate JIT workload time falls about 0.4 percent, process elapsed time about 1.7 percent, and CPU time about 1.7 percent. Peak RSS rises about 0.5 percent. Interpreter workload time falls about 0.6 percent and peak RSS rises about 1.2 percent. These results improve elapsed time while retaining a memory regression; they do not meet the overall objective.

JIT workload-time wins against CPython: 6/23. Peak-RSS wins: 0/24. Superiority across every meaningful metric remains unachieved.

| Fixture | Time/previous | CPU/previous | RSS/previous | Time/CPython | RSS/CPython |
|---|---:|---:|---:|---:|---:|
| fannkuch | 0.988794 | 0.985456 | 1.010405 | 8.884407 | 1.991370 |
| nbody | 0.988789 | 0.983732 | 1.010782 | 8.918698 | 1.983033 |
| fib | 0.991154 | 0.981748 | 1.010315 | 2.914741 | 2.009719 |
| pidigits | 0.996421 | 0.995628 | 1.007886 | 0.891403 | 1.993795 |
| pyaes | 0.987302 | 0.972600 | 1.012386 | 0.644605 | 1.995754 |
| richards | 0.989677 | 0.983591 | 1.009793 | 8.335672 | 1.998926 |
| sumvm | 1.005470 | 0.975865 | 1.007058 | 0.057217 | 2.003240 |
| nested_loops | 1.017932 | 0.971569 | 1.009783 | 0.082423 | 2.018418 |
| jitloop | 1.008274 | 0.965395 | 1.009772 | 0.073716 | 2.018378 |
| jitkernels | 1.003633 | 0.979887 | 1.009698 | 0.872541 | 1.993590 |
| deltablue | 0.994366 | 0.993597 | 1.008501 | 19.793201 | 2.179981 |
| float_math | 0.990059 | 0.988139 | 1.003188 | 7.584394 | 3.001819 |
| spectral_norm | 0.991844 | 0.984058 | 1.010177 | 2.146084 | 2.019272 |
| json_bench | 1.004316 | 0.937279 | 0.955094 | 1.163971 | 2.606591 |
| str_methods | 1.003984 | 0.993478 | 1.011397 | 2.023496 | 2.201726 |
| dict_ops | 0.985550 | 0.984946 | 1.009290 | 5.521870 | 1.980728 |
| list_ops | 0.984152 | 0.982301 | 1.008738 | 13.432935 | 1.989247 |
| attr_access | 0.996902 | 0.990287 | 1.009014 | 3.156292 | 2.048387 |
| call_overhead | 0.999982 | 0.997748 | 1.010026 | 9.109579 | 2.061290 |
| generators | 0.988032 | 0.987390 | 1.010281 | 9.752026 | 2.011853 |
| deque_ops | 0.993576 | 0.990947 | 1.009569 | 16.767581 | 2.033840 |
| datetime_ops | 1.015240 | 1.014949 | 1.011466 | 155.260141 | 2.165422 |
| pickle_bench | 0.990876 | 0.991294 | 0.961034 | 332.284610 | 2.493976 |
| startup | 0.968538 | 0.970510 | 1.009885 | 1.453568 | 1.998913 |

## Memory diagnostics and limits

The vmmap captures inspect idle child processes after importing os and time. Their physical footprints, live allocation totals, and mapped shared pages are different measurements from the census peak RSS. MallocStackLogging changes memory use, so its allocation stacks are diagnostic evidence only. Stack reservations and shared mappings must not be counted as private resident allocations. Compressed raw profiles preserve their original hashes and byte counts.

The new interpreter-mode idle capture has 6,503 KiB of live malloc allocations versus 6,076 KiB in the preceding capture. A standalone diagnostic linked against the recorded release libraries counts 1,621,236 spare vector bytes across 2,703 code objects in the 56 prepared import-cache artifacts. Column tables contribute 932,256 bytes; line tables 310,752; names 185,304; local names 122,160; remaining vectors 70,764. These requested capacities are not an RSS measurement. They support the hypothesis that avoiding clones retains decoder capacity that the old clones compacted. A subsequent compaction trial is needed to test whether speed and memory can both improve.

Cold stdlib extraction, parallel throughput, explicit GC-pause distributions, energy, and controlled build latency are not repeated here. CPython is the recorded 3.14.7 GIL build; WeavePy native execution remains gated with the GIL disabled. The illustrative fannkuch fixture is not canonical fannkuch, and pyaes is an XOR scrambler. All prior archives remain unchanged.

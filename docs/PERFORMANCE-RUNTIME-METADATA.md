# Runtime performance comparisons

Measurement note: earlier full and focused comparisons without explicit isolation used a
shared frozen standard-library cache. Alternating builds with different embedded
Python sources can include cache invalidation and recompilation costs. The
datetime parser investigation exposed an approximately 8 MiB alternating RSS
increase from this effect. New comparisons use separate per-binary caches and
verify that their artifacts stay unchanged during measured cycles. The dedicated
controlled startup comparisons already used isolated caches. Original data and
historical reports remain available with this limitation.

The later [lazy finalization metadata experiment](../crates/weavepy-bench/census/2026-09-runtime-metadata/gc-lazy-finalization/REPORT.md) was rejected. It reduced ordinary retained-heap RSS by about 2 percent, but increased finalizer-heap RSS by about 4.2 percent in both modes and regressed callback-heavy collection. All seven paired samples, load telemetry, allocation profiles, and passing validation results are preserved. This experiment does not replace the latest complete census below.

The [collector candidate-position experiment](../crates/weavepy-bench/census/2026-09-runtime-metadata/gc-candidate-positions/REPORT.md) is deferred and its runtime change is removed. All correctness checks pass, but high and changing host load prevents a reliable performance conclusion. The archive retains all twelve cases, seven paired samples per case, observed regressions, OS memory context, and exact allocation-site evidence. It does not replace the latest complete census.

The latest [collector scratch-memory stage](../crates/weavepy-bench/census/2026-09-runtime-metadata/gc-traversal-lists/REPORT.md) removes copied discovery and marking lists and reserves the generation snapshot at its required size. On the 100,000-node slotted graph, collection time falls about 16 percent and peak process RSS falls about 1.7 to 2.4 percent versus 8c8. On the deeply nested tuple graph, RSS falls about 4.3 to 5.6 percent; interpreted collection time improves, while JIT collection time is essentially unchanged. Several retained construction batches take about 1 to 2 percent longer. These are workload-specific tradeoffs, not a universal improvement.

Full-census timing remains provisional. A late-run snapshot recorded one-, five-, and fifteen-minute load averages of 12.9, 17.4, and 14.3 on an eight-core host. Both process CPU times and elapsed/CPU ratios vary across samples. For datetime, the preceding JIT workload samples range from 8.6 to 20.5 seconds and the candidate samples from 6.7 to 17.6 seconds. The observed aggregate ratios below do not establish a broad causal speedup. The apparent large datetime gain, interpreted string-method and JIT-kernel regressions, and list-operation regressions need quieter confirmation. All original samples and a separate variation record are retained.

All 341 VM tests, formatting, Clippy, and the no-default-features check pass. All 27 targeted checks and all 275 compatibility checks pass. A new regression exercises nested tuple and iterator discovery, live roots across generations, weak references, and finalizer resurrection. No unsafe code or object layout changes. Source, measurement-input, and extra-file snapshot hashes all match. These checks use the existing conformance expectations and do not establish complete CPython compatibility.

The original 29 focused cases use seven measured pairs. The controls show substantial elapsed/CPU variation in both releases and CPython. All original samples are preserved alongside one predeclared fresh run of the ten controls with 15 pairs, and two longer setter loops with seven pairs. The longer loops use the same timed source but 200,000 iterations instead of 2,000, so they do not replace the original controls. The full report includes workload CPU time separately from process CPU time to make that distinction visible.

The full census uses five pairs and nine startup conditions use 31 pairs. All cross-build measurements use separate stable frozen caches. Large-graph workload timers cover five full collections of prebuilt retained graphs, not individual pause percentiles. RSS includes setup and warmup as well as execution. Ratios below one mean less time or memory.

| Metric | JIT/checkpoint | Interpreter/checkpoint | JIT/8c8 | Interpreter/8c8 | JIT/CPython | Interpreter/CPython |
|---|---:|---:|---:|---:|---:|---:|
| Workload time, 23 workloads | 0.7863 | 0.9396 | 0.9849 | 0.9934 | 3.3974 | 9.4744 |
| Process time, 24 workloads | 0.8583 | 0.9502 | 0.9882 | 0.9927 | 3.1383 | 5.5606 |
| CPU time, 24 workloads | 0.8578 | 0.9530 | 0.9946 | 0.9964 | 3.2220 | 5.8008 |
| Peak RSS, 24 workloads | 0.9431 | 0.9334 | 0.9980 | 0.9999 | 2.0720 | 1.9008 |

WeavePy wins 6/23 JIT workload timers and 0/24 JIT peak-RSS comparisons. The overall objective remains unachieved. Every favorable and unfavorable row remains in the full report.

The executable is 44,289,168 bytes, 272 bytes smaller than 8c8. Energy, controlled build latency, parallel throughput, and individual GC-pause distributions were not established.

## Historical collector candidate-index stage

The [collector candidate-index stage](../crates/weavepy-bench/census/2026-09-runtime-metadata/gc-candidate-index/REPORT.md) reuses the existing address-mixing hasher for temporary GC candidate lookups. Batches of explicit full collections take about 40 to 58 percent less time than ab743, and several retained-object construction workloads improve about 10 to 17 percent. Peak RSS is broadly unchanged. Interpreted retained datetime construction regresses 9.3 percent and interpreted slot reinsertion 4.5 percent in the focused run. All raw pairs and all favorable and unfavorable rows are preserved.

All 341 VM tests, formatting, Clippy, and the no-default-features check pass. All 24 targeted checks and all 275 compatibility checks pass, including GIL-disabled correctness and the existing collector, weak-reference, threading, and C-API expectations. Unchanged JIT/compiler package tests were not repeated for this GC-only change. The corrected source snapshot is authoritative; the initial extra-file naming collision and its correction are preserved. No object layout or unsafe code changes.

Each of the 11 retained-batch, 4 collection, and 10 control cases uses seven measured pairs. The full census uses five pairs; nine startup conditions use 31 pairs. Every cross-build comparison uses separate stable frozen caches. Explicit-collection timers cover 20 full collections of prebuilt graphs, not individual pause percentiles. Ratios below one mean less time or memory.

| Metric | JIT/checkpoint | Interpreter/checkpoint | JIT/ab743 | Interpreter/ab743 | JIT/CPython | Interpreter/CPython |
|---|---:|---:|---:|---:|---:|---:|
| Workload time, 23 workloads | 0.7924 | 0.9331 | 0.9991 | 0.9937 | 3.4145 | 9.3320 |
| Process time, 24 workloads | 0.8521 | 0.9453 | 0.9936 | 0.9950 | 3.1702 | 5.5764 |
| CPU time, 24 workloads | 0.8502 | 0.9435 | 0.9895 | 0.9968 | 3.2482 | 5.8149 |
| Peak RSS, 24 workloads | 0.9436 | 0.9335 | 1.0003 | 1.0001 | 2.0779 | 1.9037 |

WeavePy wins 6/23 JIT workload timers and 0/24 JIT peak-RSS comparisons. The overall objective remains unachieved. See the full report for every row and controlled startup condition, including regressions.

The executable is 44,289,440 bytes, 624 bytes smaller than ab743. Independent warm CPU profiles show fewer unsigned-integer hash samples in the collector, supporting the optimization lead. They are diagnostic samples, not time ratios. Energy, controlled build latency, parallel throughput, and individual GC-pause distributions were not established.

## Historical shared-slot-name stage

The [shared-slot-name stage](../crates/weavepy-bench/census/2026-09-runtime-metadata/shared-slot-keys/REPORT.md) reuses code-owned strings during cached bytecode and JIT slot creation. Retained batches of 10,000 objects with 8, 9, or 16 slots take about 14 to 16 percent less JIT workload time and use about 6 to 8 percent less peak RSS than 8f1c86d7. Retained datetime RSS falls about 6 percent. Live profiles independently show 27,000 fewer key allocations for 3,000 nine-slot objects and 29,550 fewer for 3,000 datetimes.

All 341 VM tests, 52 JIT package tests, Clippy, formatting, and the no-JIT build check pass. All 18 targeted Python checks and all 275 compatibility checks pass, including descriptor, tracing, GC, and threading coverage. Initial failed integration tests and the corrected allocation parser are preserved. These checks use the existing conformance expectations and do not establish complete CPython equivalence.

The full census has a 0.3 percent aggregate JIT workload-time improvement, a 0.6 percent interpreted workload-time regression, and peak-RSS increases of 0.6 percent with JIT and 0.3 percent interpreted. The full datetime workload takes 3.2 percent less time with JIT and 7.2 percent less interpreted, but remains about 124 times CPython with JIT. Interpreted string methods regress 3.0 percent and JIT dictionary operations 1.6 percent. The retained-batch memory gains do not establish a general process-memory improvement.

The 11 retained-batch, 12 datetime-component, and 10 control cases use seven paired samples. The full census uses five pairs, and the nine controlled-startup conditions use 31 pairs. All cross-build measurements isolate frozen caches and verify stable artifacts. Workload time includes preceding-batch cleanup for the retained-batch cases. Below one means less time or memory.

| Metric | JIT/checkpoint | Interpreter/checkpoint | JIT/8f1c | Interpreter/8f1c | JIT/CPython | Interpreter/CPython |
|---|---:|---:|---:|---:|---:|---:|
| Workload time, 23 workloads | 0.7917 | 0.9433 | 0.9969 | 1.0056 | 3.4204 | 9.3864 |
| Process time, 24 workloads | 0.8538 | 0.9528 | 0.9965 | 1.0070 | 3.2005 | 5.6224 |
| CPU time, 24 workloads | 0.8521 | 0.9515 | 0.9979 | 1.0079 | 3.2907 | 5.8307 |
| Peak RSS, 24 workloads | 0.9427 | 0.9352 | 1.0056 | 1.0035 | 2.0795 | 1.9068 |

WeavePy wins 6/23 JIT workload timers and 0/24 JIT peak-RSS comparisons. The overall objective remains unachieved. Cold-code timers stay within about 1.2 percent of the preceding build, but the interpreted property-setter control takes 5.2 percent longer. Controlled no-site startup takes 1.8 percent longer, and startup RSS rises slightly. All other favorable and unfavorable rows remain in the full report.

The executable is 44,290,064 bytes, 416 bytes larger than 8f1c86d7. Instance and slot-storage layouts are unchanged. The native name lookup retains the existing activation-owned code-pointer lifetime contract and clones its shared name before arbitrary Python dispatch. Energy, controlled build latency, parallel throughput, and GC-pause distributions were not measured.

## Historical datetime field-validation stage

The [datetime field-validation stage](../crates/weavepy-bench/census/2026-09-runtime-metadata/datetime-fields/REPORT.md) validates ordinary exact-integer fields in Rust. Focused date and datetime construction take about 40 percent less workload time than 21b59ded. Time construction takes about 35 percent less and datetime addition about 20 percent less. Component peak RSS falls roughly 0.5 to 1.2 percent. Custom-index and Boolean fallbacks take about 2 to 5 percent longer.

The full datetime fixture takes 14.5 percent less time with JIT enabled and 15.8 percent less interpreted than 21b59ded. Its JIT workload time is still about 127 times CPython. Controlled no-site startup takes 2.9 percent longer, and unchanged nested loops retain a 1.7 percent JIT workload regression. All measured tradeoffs remain in the report.

All 337 VM tests, Clippy, and all 275 compatibility checks passed. The 6,448-case constructor/field differential and 5,139-case ISO differential introduce no regressions across JIT, interpreted, and GIL-disabled modes. A separate low-recursion comparison has no regressions and three CPython outcome convergences. Existing CPython type, callback, success/error, and wording differences remain. Both initial verification failures and their driver corrections are preserved in the report.

The full census uses five paired samples with separate per-binary frozen caches, verified unchanged through every measured row. Below one means less time or memory.

| Metric | JIT/checkpoint | Interpreter/checkpoint | JIT/21b5 | Interpreter/21b5 | JIT/CPython | Interpreter/CPython |
|---|---:|---:|---:|---:|---:|---:|
| Workload time, 23 workloads | 0.7955 | 0.9338 | 0.9929 | 0.9891 | 3.4045 | 9.2682 |
| Process time, 24 workloads | 0.8499 | 0.9421 | 0.9908 | 0.9908 | 3.2223 | 5.6989 |
| CPU time, 24 workloads | 0.8484 | 0.9419 | 0.9910 | 0.9901 | 3.2952 | 5.8919 |
| Peak RSS, 24 workloads | 0.9366 | 0.9317 | 0.9943 | 0.9962 | 2.0607 | 1.8974 |

WeavePy wins 6/23 JIT workload timers and 0/24 JIT peak-RSS comparisons. The overall objective remains unachieved. All component, fallback, full-workload, and controlled-startup results remain part of the assessment.

The executable is 44,289,648 bytes, 448 bytes larger than 21b59ded. This stage changes no object-layout sources and adds no unsafe code. Energy, controlled build latency, parallel throughput, and GC-pause distributions were not measured.

## Historical datetime parser stage

The [datetime parser stage](../crates/weavepy-bench/census/2026-09-runtime-metadata/datetime-time-parts/REPORT.md) uses native parsing for supported ASCII time components. The isolated ISO component takes about 60 percent less workload time than e910f2dd. Six- and nine-digit fractional-time controls improve roughly four to six times; peak RSS falls about 1 to 2 percent. The whole datetime fixture remains essentially unchanged with JIT enabled and takes 2.1 percent longer interpreted. It is still about 153 times slower than CPython.

All 337 VM tests, Clippy, and all 275 compatibility checks passed. The 5,139-case parsing differential preserves preceding outcomes in JIT, interpreted, and GIL-disabled modes. The separate low-recursion differential records nine CPython outcome convergences and six RecursionError wording changes; its exact-comparison failure and separate assessment are retained. Existing CPython success/error and callback differences remain.

The full census uses separate per-binary frozen caches, verified unchanged throughout every measured row. Five paired samples compare checkpoint 9a69c41, preceding release e910f2dd, and CPython 3.14.7. Below one means less time or memory.

| Metric | JIT/checkpoint | Interpreter/checkpoint | JIT/e910 | Interpreter/e910 | JIT/CPython | Interpreter/CPython |
|---|---:|---:|---:|---:|---:|---:|
| Workload time, 23 workloads | 0.7957 | 0.9471 | 0.9960 | 1.0112 | 3.4400 | 9.4436 |
| Process time, 24 workloads | 0.8552 | 0.9545 | 0.9990 | 1.0101 | 3.2600 | 5.7837 |
| CPU time, 24 workloads | 0.8535 | 0.9545 | 0.9992 | 1.0100 | 3.3330 | 5.9768 |
| Peak RSS, 24 workloads | 0.9420 | 0.9345 | 0.9999 | 0.9972 | 2.0734 | 1.9047 |

WeavePy wins 6/23 JIT workload timers and 0/24 JIT peak-RSS comparisons. The overall objective remains unachieved. Short-fraction fallbacks take about 1 to 2 percent longer; unchanged timedelta multiplication retains a 4.2 percent JIT component regression, and controlled no-site startup takes 3.3 percent longer. The report records all favorable and unfavorable results.

The executable is 44,289,200 bytes, 144 bytes larger than e910f2dd. This stage changes no object-layout sources and adds no unsafe code. Energy, controlled build latency, parallel throughput, and GC-pause distributions were not measured.

## Historical pickle allocation stage

The [pickle allocation stage](../crates/weavepy-bench/census/2026-09-runtime-metadata/pickle-borrowed-strings/REPORT.md) removes an allocation and copy when decoding borrowed UTF-8 strings and retains the preceding bounded-list encoder. Against the bounded-list release, large-string peak RSS falls about 11 to 12 percent for single results and 6 percent for retained batches. ASCII workload time falls about 9 to 16 percent; multibyte gains are smaller. These string cases remain slower than CPython.

All 335 VM tests, Clippy, both import orders, and all 275 compatibility checks pass. Encoder, decoder, and recursion differentials introduce no regressions; existing CPython error-wording and low-recursion differences remain. Unchanged compiler/JIT/workspace/no-JIT checks were last run at the preceding complete encoder stage. The rejected direct-byte-copy experiment is not retained.

The full census below compares with checkpoint 9a69c41, preceding complete release f4630431, and CPython. It measures both retained allocation changes. Focused string comparisons instead use bounded-list release 763d9a56.

| Metric | JIT/checkpoint | Interpreter/checkpoint | JIT/f463 | Interpreter/f463 | JIT/CPython | Interpreter/CPython |
|---|---:|---:|---:|---:|---:|---:|
| Workload time, 23 workloads | 0.8038 | 0.9427 | 1.0065 | 0.9975 | 3.4706 | 9.4814 |
| Process elapsed time, 24 workloads | 0.8588 | 0.9545 | 0.9968 | 1.0016 | 3.2093 | 5.7428 |
| CPU time, 24 workloads | 0.8558 | 0.9541 | 0.9975 | 1.0010 | 3.3055 | 5.9542 |
| Peak RSS, 24 workloads | 0.9391 | 0.9328 | 0.9973 | 1.0062 | 2.0791 | 1.9150 |

These geometric means use median paired ratios from five measured cycles after a discarded cycle, outside the tool filesystem sandbox. Below one means lower time or memory. CPython is the recorded 3.14.7 GIL build. WeavePy wins 6/23 JIT workload timers and 0/24 JIT peak-RSS comparisons. The overall objective remains unachieved.

The report retains every favorable and unfavorable result across 38 focused controls and nine controlled startup/import conditions. Configured encoder fallbacks include regressions of about 5 to 6 percent against 763d9a56. The separate bounded-list archive retains its earlier control regressions.

The executable SHA-256 is `e910f2dd2f1d87427f7613de9c304b42a3d1b87d8b2b0823c102f32d348c132b`, 44,289,056 bytes, unchanged from both comparison releases. Object layouts were not remeasured; their sources did not change in this stage. Energy, controlled build latency, parallel throughput, and GC-pause distributions were not measured. GIL-disabled correctness does not establish free-threaded performance.

## Historical native pickle encoder stage

The [native pickle encoder stage](../crates/weavepy-bench/census/2026-09-runtime-metadata/pickle-native-encode/REPORT.md) serializes supported protocol-4/5 built-in data. Ordinary payload encoding improves about 231 times versus the preceding decoder release, but still takes about 3.3 times CPython workload time. Importing _pickle first now exposes the same public accelerator exports as importing pickle first.

All 38 compiler tests, 332 VM tests, 52 JIT tests, and static checks pass. Of 275 positive compatibility checks, 273 passed initially and two sandbox-blocked loopback socket fixtures passed on authorized retries outside the sandbox. Original failures and exact retries are retained. A 1,266-case encoder differential accepts 642 cases per execution mode, with no new regressions or CPython value differences. Decoder and low-recursion differentials also pass, retaining existing CPython differences.

| Metric | JIT/checkpoint | Interpreter/checkpoint | JIT/previous | Interpreter/previous | JIT/CPython | Interpreter/CPython |
|---|---:|---:|---:|---:|---:|---:|
| Workload time, 23 fixtures | 0.8011 | 0.9458 | 0.9824 | 0.9895 | 3.4976 | 9.7516 |
| Process elapsed time, 24 fixtures | 0.8527 | 0.9555 | 0.9765 | 0.9924 | 3.1478 | 5.6756 |
| CPU time, 24 fixtures | 0.8519 | 0.9552 | 0.9777 | 0.9895 | 3.2662 | 5.9461 |
| Peak RSS, 24 fixtures | 0.9369 | 0.9287 | 0.9928 | 0.9978 | 2.0761 | 1.9065 |

Geometric means use median paired ratios from five measured cycles after a discarded cycle, outside the tool filesystem sandbox. Below one means lower time or memory. The comparison uses the recorded CPython 3.14.7 GIL build. WeavePy wins 6/23 JIT workload timers and 0/24 JIT peak-RSS comparisons. The overall objective remains unachieved.

Seven paired cycles cover 12 component/large-graph probes, 10 encoder-option controls, two ordinary-call controls, and six decoder controls. Large-list encoding raises peak RSS about 10 percent versus the preceding release; nested-dictionary encoding lowers it about 11 percent. Some configured fallback calls regress about 4 to 11 percent. Custom record encoding and decoding remain over 300 times CPython workload time. All unfavorable results remain in the report. Thirty-one paired cycles cover nine controlled startup/import conditions, including both pickle import orders.

The executable SHA-256 is `f46304310b505814c85e6258b59e10274975ccb12108e339b9df4f9789f987a9`, 44,289,056 bytes, up 39,040 bytes from the preceding release. All 12 object layouts are unchanged. Energy, controlled build latency, parallel throughput, and GC-pause distributions were not measured in this stage. GIL-disabled correctness does not establish free-threaded performance.

## Historical native pickle decoder stage

The [native pickle decoder stage](../crates/weavepy-bench/census/2026-09-runtime-metadata/pickle-native-entry-guards/REPORT.md) accelerates supported protocol-4/5 built-in data. The ordinary payload decodes about 105 times faster than the preceding release, but still takes about 4.5 times CPython workload time. Encoding and custom reconstruction retain the existing implementation.

All 38 compiler tests, 328 VM tests, 52 JIT tests, 275 positive compatibility checks, and static checks pass. A 1,546-stream differential passes in JIT, interpreted, and GIL-disabled modes, with 282 streams accepted natively per mode. Existing malformed-input error wording differences remain. The archive includes all intermediate guard experiments and their regressions.

| Metric | JIT/checkpoint | Interpreter/checkpoint | JIT/previous | Interpreter/previous | JIT/CPython | Interpreter/CPython |
|---|---:|---:|---:|---:|---:|---:|
| Workload time, 23 fixtures | 0.799 | 0.952 | 1.003 | 0.995 | 3.558 | 9.808 |
| Process elapsed time, 24 fixtures | 0.853 | 0.963 | 0.998 | 0.994 | 3.268 | 5.784 |
| CPU time, 24 fixtures | 0.866 | 0.965 | 0.998 | 0.997 | 3.351 | 5.986 |
| Peak RSS, 24 fixtures | 0.938 | 0.934 | 0.999 | 1.008 | 2.082 | 1.916 |

These are geometric means of median paired ratios over five alternating measured cycles after a discarded cycle, outside the tool filesystem sandbox. Below one means lower time or memory. CPython is the recorded 3.14.7 GIL build. WeavePy wins 6/23 workload timers and 0/24 peak-RSS comparisons. The overall objective remains unachieved.

Compared with the preceding release, aggregate JIT workload time rises about 0.35 percent, while interpreted time falls about 0.48 percent. Interpreted peak RSS rises about 0.79 percent; JIT peak RSS is nearly unchanged. N-body, Fibonacci, generator, and interpreted JIT-kernel controls retain regressions of about 5 to 9 percent.

Seven paired cycles cover eight encoding/decoding workloads and six small/fallback controls. A 15-cycle repeat retains an approximately 5 percent batch-tuple decoding regression versus the initial native decoder. Byte decoding does not consistently beat CPython, and some legacy or configured-input controls regress by a few percent. Thirty-one paired cycles cover seven controlled startup/import cases. Every raw measurement is retained.

The executable SHA-256 is `247f3eb9b1c1bde87c77ccb9958c7b80430ef4bf652b3facd99408a70a1502cd`, 44,250,016 bytes, up 38,000 bytes from the preceding release. All 12 measured object layouts are unchanged. Cold extraction, parallel throughput, GC-pause distributions, energy, and controlled build latency were not repeated. GIL-disabled correctness checks do not establish free-threaded performance.

## Historical empty native-table stage

The [empty native-table stage](../crates/weavepy-bench/census/2026-09-runtime-metadata/native-empty-tables/REPORT.md) skips unnecessary callee-table lookups. Attribute access improves about 17 percent and the nested-loop workload about 16 percent versus the exact-decoder release. Aggregate JIT workload time falls about 1.9 percent, process elapsed time about 1.8 percent, and CPU time about 1.6 percent. Measured peak RSS falls about 0.8 percent with JIT enabled and 0.5 percent in interpreted mode. Interpreter workload time is nearly unchanged, while process elapsed and CPU time rise about 0.4 percent.

All 38 compiler tests, 324 VM tests, 52 JIT tests, 272 positive compatibility checks, and static checks pass. Separate marshal/importlib diagnostics retain the preceding failures. Corrected call-shape probes verify ordinary native calls before timing and match CPython returns. The no-inner-call method timer improves about 23 percent; the recursive scalar control regresses about 1.2 percent. All gains, regressions, initial probe-gate failures, and raw samples are retained.

| Metric | JIT/checkpoint | Interpreter/checkpoint | JIT/exact decoder | Interpreter/exact decoder | JIT/CPython | Interpreter/CPython |
|---|---:|---:|---:|---:|---:|---:|
| Workload time, 23 fixtures | 0.817 | 0.960 | 0.981 | 1.000 | 3.523 | 9.593 |
| Process elapsed time, 24 fixtures | 0.873 | 0.967 | 0.982 | 1.004 | 3.341 | 5.885 |
| CPU time, 24 fixtures | 0.872 | 0.967 | 0.984 | 1.004 | 3.419 | 6.085 |
| Peak RSS, 24 fixtures | 0.937 | 0.928 | 0.992 | 0.995 | 2.072 | 1.903 |

The full census uses five alternating paired cycles after a discarded cycle, outside the tool filesystem sandbox. Aggregates are geometric means of per-fixture median paired ratios; below one means lower time or memory. CPython is the recorded 3.14.7 GIL build. WeavePy still wins 6/23 workload timers and 0/24 peak-RSS comparisons. Datetime and pickle remain about 154 and 336 times CPython workload time. The objective remains unachieved.

The executable SHA-256 is `988d15d15f83af8a51e42028753650b0693ca3dcc130f3081167554e5c5b79d3`, 44,212,016 bytes. Executable size, all 12 measured object layouts, and the native-call stack-frame size are unchanged. Nineteen focused cases include controlled startup and imports; their elapsed times are nearly unchanged except for a 1.2 percent no-site startup increase. Cold extraction, parallel throughput, GC-pause distributions, energy, and controlled build latency weren't repeated.

## Historical exact decoder allocation stage

The subsequent [native-call scratch screen](../crates/weavepy-bench/census/2026-09-runtime-metadata/native-call-scratch/REPORT.md) is preserved but not retained. It improves attribute access by about 2 to 3 percent while regressing some controls. Follow-up coverage also shows that its original isolated shape probes do not exercise the changed native-to-native buffer path. The exact-decoder stage below predates the empty-table stage.

The [exact decoder allocation stage](../crates/weavepy-bench/census/2026-09-runtime-metadata/cache-decode-capacity/REPORT.md) reserves code buffers at their final lengths, replacing the shrink-after-decoding trial. Aggregate peak RSS falls about 1.9 percent with JIT enabled and 1.0 percent in interpreted mode versus ownership. Workload time rises about 0.4 percent in both modes. Matching and relocated imports use about 3.45 percent less peak RSS with similar elapsed time.

The focused list and interpreted-string controls recover the compaction trial's larger slowdowns. Native attribute access still regresses about 2.8 percent, and the no-site startup control regresses slightly. These losses remain explicit. All 38 compiler tests, 323 VM tests, 52 JIT tests, 269 positive compatibility checks, and static checks pass. Separate marshal/importlib diagnostics retain the preceding failures, and first/warm benchmark returns match CPython.

| Metric | JIT/checkpoint | Interpreter/checkpoint | JIT/ownership | Interpreter/ownership | JIT/CPython | Interpreter/CPython |
|---|---:|---:|---:|---:|---:|---:|
| Workload time, 23 fixtures | 0.829 | 0.963 | 1.004 | 1.004 | 3.553 | 9.582 |
| Process elapsed time, 24 fixtures | 0.884 | 0.971 | 0.996 | 1.005 | 3.364 | 5.883 |
| CPU time, 24 fixtures | 0.882 | 0.971 | 0.996 | 1.005 | 3.440 | 6.084 |
| Peak RSS, 24 fixtures | 0.938 | 0.934 | 0.981 | 0.990 | 2.075 | 1.915 |

These are geometric means of per-fixture median paired ratios, measured outside the tool filesystem sandbox with five alternating paired cycles after a discarded cycle. Below one means a reduction. CPython is the recorded 3.14.7 GIL build. WeavePy wins 6/23 workload timers and 0/24 peak-RSS comparisons; the overall objective remains unachieved.

The executable SHA-256 is `43634fbb8b1a5c5d836b1ba59b54b69ab0c3373ea3620d69a975d74b8860bbe4`, 44,212,016 bytes. All 12 measured layouts are unchanged. The archive retains 15 focused cases, 24 standard workloads, exact inputs, return checks, and regressions. A diagnostic reduces spare vector capacity from 1,621,236 bytes to 26,396 bytes across 2,703 decoded code objects. These capacities aren't RSS measurements. Cold extraction, parallel throughput, explicit GC-pause distributions, energy, and controlled build latency weren't repeated.

## Historical cached-code compaction trial

The [cached-code compaction trial](../crates/weavepy-bench/census/2026-09-runtime-metadata/cache-code-compaction/REPORT.md) reduces aggregate JIT peak RSS by about 0.8 percent versus the ownership release, while JIT workload time rises about 0.6 percent and interpreter workload time rises about 1.3 percent. Relocated-import RSS falls about 2.25 percent, but remains about 1.04 percent above the range-formula release. The trial retains meaningful speed regressions and needed further work.

All 323 VM tests, 52 JIT tests, 269 positive compatibility checks, and static checks pass. Separate marshal/importlib diagnostics retain the preceding failures. The full census still wins 6/23 workload timers and 0/24 peak-RSS comparisons against CPython. JIT aggregate workload time is 3.612 times CPython, process elapsed time 3.394 times, CPU time 3.479 times, and peak RSS 2.100 times. These results don't achieve the overall objective.

The executable SHA-256 is `63ed378aa7d6eeeadd43f7bc6e1867ae4b6548d7deb5e4c2ca750061ea301856`, 44,212,096 bytes. The archive retains all 15 focused cases, 24 standard workloads, exact inputs, return checks, memory diagnostics, and regressions. The relocated cache loader reduces requested spare vector capacity from 1,621,236 bytes to 160 bytes across 2,703 code objects. These buffer capacities aren't RSS measurements.

## Historical cached-code ownership stage

The [cached-code ownership stage](../crates/weavepy-bench/census/2026-09-runtime-metadata/cache-code-ownership/REPORT.md) avoids cloning freshly decoded code before relocating filenames. The full census measures about 1.7 percent lower process elapsed time and 0.4 percent lower workload time versus the preceding range-formula release, with about 0.5 percent higher peak RSS. The memory regression remained unresolved at that stage.

Controlled probes use equal-length executable paths and isolated stdlib and frozen-code caches. Relocated startup is about 1.9 percent faster and relocated imports about 2.9 percent faster, while peak RSS rises about 1.1 and 3.4 percent. Matching-path timings are essentially unchanged. A diagnostic finds 1.62 MB of spare vector capacity across the prepared import-cache artifacts, motivating the subsequent compaction trial. These capacities are not an RSS measurement.

All 323 VM tests, 52 JIT tests, and static checks pass. The expanded compatibility run passes 268 of 270 checks. The added marshal and importlib failures are identical on the preceding and candidate releases, and the exact import test passes on both. Complete diagnostics, the corrected import filter, relocation tests, and five benchmark return checks remain in the archive.

The full census uses five alternating paired cycles after a discarded cycle, outside the tool filesystem sandbox. The table shows geometric means of per-fixture median paired ratios; below one means lower time or memory. CPython is the recorded 3.14.7 GIL build.

| Metric | JIT/checkpoint | Interpreter/checkpoint | JIT/range formula | Interpreter/range formula | JIT/CPython | Interpreter/CPython |
|---|---:|---:|---:|---:|---:|---:|
| Workload time, 23 fixtures | 0.828 | 0.961 | 0.996 | 0.994 | 3.552 | 9.571 |
| Process elapsed time, 24 fixtures | 0.884 | 0.969 | 0.983 | 0.994 | 3.367 | 5.883 |
| CPU time, 24 fixtures | 0.882 | 0.969 | 0.983 | 0.994 | 3.450 | 6.093 |
| Peak RSS, 24 fixtures | 0.951 | 0.945 | 1.005 | 1.012 | 2.105 | 1.939 |

WeavePy still wins 6/23 standard workload timers and 0/24 standard peak-RSS comparisons. The objective of beating CPython across every meaningful metric remains unachieved. The executable SHA-256 is `a6ff21200ed9d5d0d1eaedd103a136b4a8eb5600bb0e3ead863364f3f7304f8d`, 44,212,288 bytes. All 12 measured layouts are unchanged. The archive retains all 15 focused cases, 24 standard workloads, raw profiles, and observed regressions. Cold extraction, parallel throughput, explicit GC-pause distributions, energy, and controlled build latency weren't repeated.

## Historical range-sum stage

The [range-sum and iterator stage](../crates/weavepy-bench/census/2026-09-runtime-metadata/range-formula/REPORT.md) computes exact integer range sums with checked arithmetic and full-precision fallback. It also repairs wide-range iteration that could loop indefinitely after integer overflow, along with incorrect length hints. All 321 VM tests, 52 JIT tests, 265 configured compatibility checks, and static checks pass. All 40 forward-iteration cases and three huge hints match CPython. The 284-case sum oracle retains one unchanged empty-float identity difference; the existing numeric and argument-error differences remain recorded.

The 10,000-element integer range timer takes about 1.03 percent of CPython time. At two million elements, the timer takes about 0.079 percent, and complete process elapsed time is about 8 percent lower. Peak RSS remains about 84 percent higher. Empty, one-element, and 32-element range controls remain slower than CPython. Float-start range sums still take about 7.85 times CPython workload time. The large integer-list population retains its elapsed-time and memory wins, while tuples still lose in peak RSS.

The focused controls retain a 4.3 to 4.4 percent list-operation slowdown and a 1.2 to 1.4 percent attribute-access improvement versus the numeric-sum release. The refreshed full census compares against checkpoint 9a69c41, the preceding complete streaming-sum release, and CPython 3.14.7 with the GIL. It uses five alternating paired cycles after a discarded cycle, consistently outside the tool filesystem sandbox. Each report records its declared launch context. Aggregates are geometric means of per-fixture median paired ratios; below one means lower time or memory.

| Metric | JIT/checkpoint | Interpreter/checkpoint | JIT/streaming sum | Interpreter/streaming sum | JIT/CPython | Interpreter/CPython |
|---|---:|---:|---:|---:|---:|---:|
| Workload time, 23 fixtures | 0.828 | 0.968 | 1.003 | 1.009 | 3.564 | 9.695 |
| Process elapsed time, 24 fixtures | 0.890 | 0.980 | 0.994 | 1.011 | 3.390 | 5.910 |
| CPU time, 24 fixtures | 0.890 | 0.979 | 0.996 | 1.011 | 3.471 | 6.121 |
| Peak RSS, 24 fixtures | 0.941 | 0.935 | 0.986 | 0.991 | 2.080 | 1.915 |

WeavePy still wins 6/23 standard workload timers and 0/24 standard peak-RSS comparisons. The objective of beating CPython across every meaningful metric remains unachieved. All 72 focused cases, 24 standard workloads, observed regressions, raw samples, and exact inputs are retained in the archive. The executable SHA-256 is `337990883d962526498cd2f6c88d9de676babed1b7f4d0c347b5c55b60bbf789`, 44,211,904 bytes. All 12 measured layouts are unchanged.

Dedicated startup/import variants, parallel throughput, and explicit GC-pause distributions weren't repeated. Native execution remains gated in WeavePy's GIL-disabled mode. These measurements establish no energy, controlled build-latency, or free-threaded CPython result.

## Historical numeric and streaming-sum stages

The subsequent [numeric-sum stage](../crates/weavepy-bench/census/2026-09-runtime-metadata/sum-numeric/REPORT.md) extends direct reduction to exact numeric lists and tuples and fixes 36 numerical oracle differences in each execution mode. In a 15-cycle repeat, float, mixed-number, and large-integer sum timers take 3.4 to 3.9 percent of the preceding release time and 56 to 79 percent of CPython time. Their complete processes still take 1.37 to 1.45 times CPython elapsed time and use 1.97 to 2.12 times its peak RSS. The large integer-list workload retains its elapsed-time and memory wins.

All 319 VM tests, 52 JIT tests, 259 configured compatibility checks, and static checks pass. The 764-case numeric oracle retains 46 existing differences, with no new differences. All 12 measured layouts remain unchanged. The executable SHA-256 is `f655a1bdaeaaa26cfaaddb3fce7ad358586fe3453d3283e0e98216d976899b30`, 44,211,792 bytes. The repeated controls retain slowdowns of 4.4 percent for overflow-list sums, 2.8 percent for range sums, and 1.5 to 1.8 percent for attribute access. Earlier tuple-sum slowdowns do not repeat. All original and repeat samples remain in the archive, with their declared launch context. This focused stage does not repeat the full census below or achieve the CPython-wide objective.

The [streaming-sum stage](../crates/weavepy-bench/census/2026-09-runtime-metadata/sum-streaming/REPORT.md) was the preceding full refresh. It reduces exact integer lists and tuples directly and streams other iterables. It removes the temporary container copy and repairs four callback/iteration-order defects. The complete two-million-integer list workload takes about 40 percent less process elapsed time and 20 percent less peak RSS than CPython. Its isolated sum timer takes about 2.8 percent of the preceding release time. Generic float, mixed, and large-integer sum timers regress 13 to 20 percent, and range sums regress 8.4 percent. These regressions remain recorded and require further work.

All 318 VM tests, 52 JIT tests, 256 configured compatibility checks, and static checks pass. The release oracle matches CPython on 42/47 cases in native, interpreted, and GIL-disabled modes. Three existing error wording differences and two compensated-float differences remain. The executable SHA-256 is `7ef09c5d3e724632e034bec5c4c3a8cd7a8780beac258bff59a96c09e1db4ae0`, 44,211,280 bytes. All 12 measured layouts are unchanged.

That full census uses five alternating paired cycles after a discarded cycle. Aggregates are geometric means of per-fixture median paired ratios. Ratios below one mean lower time or memory. Workload time excludes startup, leaving 23 timers; process elapsed time, CPU, and peak RSS include all 24 fixtures.

| Metric | JIT/checkpoint | Interpreter/checkpoint | JIT/string args | Interpreter/string args | JIT/CPython | Interpreter/CPython |
|---|---:|---:|---:|---:|---:|---:|
| Workload time | 0.817 | 0.961 | 0.993 | 0.992 | 3.551 | 9.563 |
| Process elapsed time | 0.881 | 0.972 | 0.986 | 0.995 | 3.376 | 5.868 |
| CPU time | 0.887 | 0.972 | 0.988 | 0.996 | 3.457 | 6.075 |
| Peak RSS | 0.947 | 0.944 | 0.994 | 1.003 | 2.096 | 1.935 |

WeavePy wins 6/23 standard workload timers and 0/24 standard peak-RSS comparisons. The large-list gains above belong to additional population workloads. The objective of beating CPython across every meaningful metric remains unachieved.

The streaming-sum census ran outside the tool filesystem sandbox; the preceding string-argument census ran under the default sandbox. Controlled default/outside/default probes reproduce CPython datetime medians of 250.688, 25.133, and 247.635 ms with identical executable and fixture hashes and captured environment. This launch-mode effect explains the CPython datetime reference shift between these two censuses. Within-run paired comparisons share the launch condition; their CPython ratios shouldn't be compared across stages as if conditions were identical. [Raw probes and controllers](../crates/weavepy-bench/census/2026-09-runtime-metadata/sum-streaming/REPORT.md) preserve all samples. Earlier unidentified variations remain historical observations.

The recorded CPython baseline is 3.14.7 with the GIL. Dedicated startup/import variants, parallel throughput, explicit GC-pause distributions, energy, and controlled build latency weren't repeated in this stage. Native execution remains gated in WeavePy's GIL-disabled mode.

## Historical range and string stages

The [exact-range collection stage](../crates/weavepy-bench/census/2026-09-runtime-metadata/range-collection/REPORT.md) reserves list and tuple capacity once and fills inline integers directly. Large list construction takes 20 to 29 percent of the preceding release time; tuples take 40 to 47 percent. Those construction timers beat CPython at every measured population from 10,000 to 2 million elements. Small constructions improve by 5 to 61 percent but remain slower than CPython. Peak process RSS improves by up to about 5 percent, yet still exceeds CPython in every measured case. Its 316 VM tests, 52 JIT tests, 253 configured compatibility checks, and 19-case release oracle pass. The executable grows by 144 bytes, with all 12 measured layouts unchanged. All 42 performance cases and observed regressions are retained. This focused stage doesn't repeat the full census below or achieve the CPython-wide objective.

The range population measurements include a checksum after construction. At that stage, the VM's `sum` path copied list and tuple contents before reducing them, so those process peaks include more than construction. The archive retains that limitation and the preexisting generic tuple length-hook difference discovered by the compatibility probes.

The [native string argument stage](../crates/weavepy-bench/census/2026-09-runtime-metadata/native-string-args/REPORT.md) keeps common native string arguments on the stack and repairs `replace` argument validation. Its refreshed 24-workload census includes all changes since the GC-index release. The objective of beating CPython across every meaningful metric remains unachieved.

Across 23 workload timers, the geometric mean of JIT/CPython ratios is 3.251; across 24 peak-RSS measurements, it is 2.096. WeavePy wins 6/23 workload-time comparisons and 0/24 peak-RSS comparisons. These historical results superseded the GC-index table below; later full refreshes appear above.

All 313 VM tests, 52 JIT tests, 247 configured compatibility checks, and static checks pass. The 58-case string oracle matches CPython except for one explicitly retained excess-split error wording difference. Separate first-call and warmed benchmark return values agree in native, interpreted, and GIL-disabled modes. The measured executable is `weavepy-runtime-native-string-args`, SHA-256 `76d06d7a7b0e76779dc79fc8e4bf5dacbe1eb21eed7fec3fb458250568131fb0`, 44,211,008 bytes. All 12 measured layouts are unchanged.

The focused comparison against the preceding split/activation cleanup release improves string workload time by 3.3 percent cold and 3.9 percent warm, with roughly unchanged peak RSS. List time improves 3.6 and 4.8 percent. Attribute access regresses 1.6 and 1.5 percent. At 60,000 string iterations, time improves 4.7 and 3.4 percent. Every sample and observed regression is retained.

The full census uses five alternating paired cycles after a discarded cycle. Ratios below one mean lower time or memory. Each aggregate is a geometric mean of per-workload median paired ratios. Workload time excludes the startup fixture; process wall time, CPU, and peak RSS include all 24 fixtures.

| Metric | JIT/checkpoint | Interpreter/checkpoint | JIT/GC-index | Interpreter/GC-index | JIT/CPython | Interpreter/CPython |
|---|---:|---:|---:|---:|---:|---:|
| ns | 0.833 | 0.969 | 0.988 | 0.998 | 3.251 | 8.764 |
| wall_ns | 0.897 | 0.979 | 0.988 | 1.001 | 3.150 | 5.459 |
| cpu_ns | 0.893 | 0.979 | 0.988 | 1.001 | 3.235 | 5.673 |
| rss_bytes | 0.947 | 0.942 | 1.000 | 1.001 | 2.096 | 1.931 |

The full [per-workload tables and raw data](../crates/weavepy-bench/census/2026-09-runtime-metadata/native-string-args/REPORT.md) preserve both improvements and regressions. Dedicated startup/import variants, parallel throughput, retained-population scaling, and explicit GC-pause distributions were not repeated in this stage. The CPython baseline is the recorded 3.14.7 GIL build. These results make no free-threaded CPython, energy, or controlled build-latency claim.

## Historical GC-index measurements

The [split-list tracking and activation cleanup stage](../crates/weavepy-bench/census/2026-09-runtime-metadata/string-split-gc/REPORT.md) registers split results with the collector, retires obsolete native entry pins, and releases locals when the last escaped frame alias disappears. All 311 VM tests, 52 JIT tests, and 244 configured compatibility checks pass. Original leak probes and benchmark return values match CPython. String workload time falls 3.1 percent cold and 3.5 percent warm relative to the callback repair, with approximately unchanged peak RSS. Cold list time regresses 5.7 percent, warm lists 0.9 percent, and attribute access 1.7 to 1.9 percent. All measurements and initial failures are preserved. Strings still take about twice CPython workload time and use about 2.2 times its peak RSS. This focused stage doesn't repeat the full census or achieve the CPython-wide objective.

The [native string callback repair](../crates/weavepy-bench/census/2026-09-runtime-metadata/native-string-callbacks/REPORT.md) corrects global invalidation and caller frames for `str.replace` count callbacks. Its 308 VM tests, 52 JIT tests, static checks, and focused release probes pass; no performance measurements or full compatibility batch were run for that intermediate. At that intermediate, a further probe found untracked split-result lists that leaked self-cycles in both WeavePy modes. Earlier zero-result sentinel probes couldn't detect those untracked lists and don't establish absence of retention. The subsequent split-list stage above repairs that defect.

The [completed string-result budget stage](../crates/weavepy-bench/census/2026-09-runtime-metadata/string-pin-budget/REPORT.md) reduces string peak RSS another 9 percent relative to loop-poll pressure. Main string time changes by -1.2 percent cold and +0.6 percent warm. Cold RSS remains 5.6 percent above GC-index; warm RSS is about 44 percent below it. All 235 existing compatibility checks, 307 VM tests, and 52 JIT tests pass. Subsequent probes expose preexisting `str.replace` callback and argument-validation defects outside those checks; their failing results are preserved. The CPython-wide objective remains unachieved.

The later [temporary-pin pressure stage](../crates/weavepy-bench/census/2026-09-runtime-metadata/native-pin-pressure/REPORT.md) passes 235 compatibility checks, 306 VM tests, and 52 JIT tests. Against the GC-index release, string workload time falls about 31 percent cold and 26 percent warm, and warm peak RSS falls about 38 percent. Cold RSS remains about 16 percent higher. Repeated attribute controls show 1.5 to 1.8 percent slower execution with about 4 percent lower RSS. All original and repeat samples are retained. This focused stage doesn't repeat the full census or achieve the CPython-wide objective.

A subsequent [native-pin retirement experiment](../crates/weavepy-bench/census/2026-09-runtime-metadata/native-pin-retirement/REPORT.md) passes 232 compatibility checks, 305 VM tests, and 52 JIT tests. It improves string workload time by about 26 percent cold and 22 percent warm, but cold peak RSS increases about 86 percent. Its exact inputs, raw results, and regression evidence are archived; it isn't an improvement across all metrics. At that stage, the GC-index measurements below were the latest complete 24-fixture census.

The private collector index now mixes the high half of its word hash into the low half. The existing final multiplication preserves allocation-alignment bits, concentrating home buckets for regularly spaced object addresses. The final XOR spreads those buckets while preserving the full 64-bit hash one-to-one. Python dictionary and set hashing, object identity, index locking, and collector ownership remain unchanged.

Measured executable: `target/release/weavepy-runtime-gc-index-mix`, SHA-256 `8e86b528cdc3dd1f5c5db14e8a76fc111eba277dd2cabbfba76de28eb3874cd9`, 44,194,128 bytes. The paired preceding release is the native slice repair, SHA-256 `9f0745479837e48c31b46957bce04a23cb3ba0108c36f4758743013287e7e092`. The original PR checkpoint is `9a69c4161ade6527a5f4457d7e7272075cb1f0d9`.

All 229 compatibility checks pass, including the native slice repair. All 303 VM and 52 JIT tests pass, along with Clippy, workspace/all-feature/no-JIT checks, and debug/release lifecycle checks. The new alignment regression exercises multiple allocation strides and address regions. Existing checks cover rooted and frozen cycles, weak references, finalizers, generation promotion, cross-thread heaps, and repeated removals.

No new unsafe code or public Rust API change is introduced. Matching release layouts remain unchanged, including 80-byte tracked handles, 24-byte Object, 176-byte instance headers, and 480-byte CodeObject. Process measurements below determine whether allocation behavior changes peak memory.

## Index experiment

The isolated Rust benchmark uses 12 captured sets of 100,000 retained dictionary addresses from two earlier releases. Fifteen alternating paired cycles follow a discarded cycle. It measures contains-and-insert, successful lookup, and reverse removal, verifying contents and checksums. The map has the same key/value widths as the collector index. This experiment is not VM speed or proof of the cause of an earlier empty-dictionary regression.

| Address set | Insert ratio | Lookup ratio | Remove ratio | Combined time ratio |
|---|---:|---:|---:|---:|
| base_False_0-addresses | 0.423 | 0.402 | 1.585 | 0.583 |
| base_False_1-addresses | 0.415 | 0.378 | 1.529 | 0.574 |
| base_False_2-addresses | 0.403 | 0.394 | 1.520 | 0.561 |
| base_True_0-addresses | 0.405 | 0.394 | 1.573 | 0.567 |
| base_True_1-addresses | 0.436 | 0.404 | 1.658 | 0.614 |
| base_True_2-addresses | 0.410 | 0.386 | 1.569 | 0.574 |
| new_False_0-addresses | 0.440 | 0.373 | 1.549 | 0.595 |
| new_False_1-addresses | 0.262 | 0.208 | 0.794 | 0.340 |
| new_False_2-addresses | 0.428 | 0.364 | 1.617 | 0.592 |
| new_True_0-addresses | 0.439 | 0.383 | 1.588 | 0.606 |
| new_True_1-addresses | 0.249 | 0.183 | 0.766 | 0.327 |
| new_True_2-addresses | 0.460 | 0.404 | 1.589 | 0.627 |

Insertion and lookup improve in the isolated test, while removal becomes slower for most captured sets. The following VM measurements include their combined effects and all observed regressions.

## Standard suite

Five alternating paired cycles retain all 24 fixtures and their work sizes. Workload time excludes startup, leaving 23 workload timers; process elapsed time, CPU time, and peak RSS include startup. Aggregates are geometric means of per-fixture median paired ratios. Ratios below one mean less time or memory.

| Metric | JIT/checkpoint | Interpreter/checkpoint | JIT/previous | Interpreter/previous | JIT/CPython | Interpreter/CPython |
|---|---:|---:|---:|---:|---:|---:|
| ns | 0.845 | 0.973 | 0.995 | 0.995 | 3.293 | 8.803 |
| wall_ns | 0.904 | 0.984 | 0.989 | 0.998 | 3.198 | 5.551 |
| cpu_ns | 0.902 | 0.984 | 0.990 | 0.998 | 3.267 | 5.728 |
| rss_bytes | 0.947 | 0.942 | 1.001 | 1.004 | 2.096 | 1.932 |

JIT workload-time wins against CPython: 6/23. Peak-RSS wins: 0/24. The objective of superiority across every meaningful workload and metric remains unachieved.

| Fixture | JIT time/previous | Interpreter time/previous | JIT CPU/previous | JIT RSS/previous | JIT time/CPython | JIT RSS/CPython |
|---|---:|---:|---:|---:|---:|---:|
| fannkuch | 0.986 | 0.987 | 0.983 | 0.998 | 10.299 | 1.986 |
| nbody | 1.000 | 1.006 | 1.000 | 0.999 | 8.943 | 1.978 |
| fib | 1.005 | 1.005 | 0.997 | 1.004 | 3.027 | 2.009 |
| pidigits | 1.000 | 1.005 | 0.999 | 0.998 | 0.888 | 1.991 |
| pyaes | 0.991 | 0.949 | 0.980 | 1.005 | 0.660 | 1.997 |
| richards | 1.005 | 1.011 | 1.004 | 0.998 | 8.153 | 1.989 |
| sumvm | 0.990 | 0.996 | 0.982 | 0.998 | 0.057 | 2.001 |
| nested_loops | 0.996 | 0.993 | 0.975 | 1.005 | 0.082 | 2.012 |
| jitloop | 0.998 | 0.999 | 0.976 | 1.001 | 0.074 | 2.011 |
| jitkernels | 0.993 | 1.002 | 0.981 | 1.001 | 0.859 | 1.999 |
| deltablue | 0.998 | 0.996 | 0.999 | 1.006 | 19.378 | 2.176 |
| float_math | 0.947 | 0.958 | 0.948 | 1.002 | 7.456 | 3.044 |
| spectral_norm | 1.009 | 1.004 | 0.998 | 0.999 | 2.179 | 2.005 |
| json_bench | 0.995 | 0.999 | 0.994 | 1.001 | 1.149 | 2.532 |
| str_methods | 0.983 | 0.998 | 0.986 | 1.003 | 3.149 | 2.085 |
| dict_ops | 0.996 | 0.992 | 0.996 | 0.999 | 5.477 | 1.971 |
| list_ops | 0.999 | 0.996 | 0.998 | 0.999 | 13.846 | 1.981 |
| attr_access | 1.002 | 1.003 | 0.997 | 1.005 | 3.010 | 2.126 |
| call_overhead | 1.010 | 1.002 | 1.009 | 1.002 | 9.025 | 2.042 |
| generators | 0.993 | 0.993 | 0.988 | 1.002 | 9.642 | 2.006 |
| deque_ops | 1.002 | 1.006 | 1.002 | 1.003 | 16.753 | 2.030 |
| datetime_ops | 0.988 | 1.004 | 0.988 | 1.000 | 15.906 | 2.161 |
| pickle_bench | 0.996 | 0.983 | 0.997 | 1.003 | 338.233 | 2.471 |
| startup | 0.985 | 1.024 | 0.986 | 1.001 | 1.486 | 1.986 |

The illustrative fannkuch fixture is not canonical fannkuch; pyaes is an XOR scrambler. Earlier full censuses showed substantial unexplained CPython datetime timing variation despite matching recorded executable, library, module, and harness hashes. All samples remain available; changes in CPython-relative ratios between stages alone do not establish a WeavePy improvement. Paired comparisons within this stage are reported above.

## Focused measurements

Dictionary and set allocations are repeated for 15 paired cycles after the initial nine-cycle controls, retaining both normal and disabled-GC cases. This repeat checks the large improvements against the address-layout variability observed in earlier releases. Both runs remain available.

Focused allocation and GC controls use interpreter mode. Native-specific controls retain separate interpreter samples. Ratios below are medians of paired cycle ratios. CPU time and peak RSS cover the process. Cold means no explicit workload warmup, not flushed operating-system caches; warm controls run the workload before timing.

### allocation-repeat

15 measured cycles; every raw sample is retained.

| Workload | Time/previous | CPU/previous | RSS/previous | Time/CPython | RSS/CPython |
|---|---:|---:|---:|---:|---:|
| retained_dicts | 0.971 | 0.978 | 1.001 | 10.145 | 2.717 |
| retained_dicts_gc_disabled | 0.776 | 0.884 | 1.002 | 8.829 | 2.540 |
| retained_sets | 0.920 | 0.941 | 1.000 | 3.535 | 1.470 |
| retained_sets_gc_disabled | 0.814 | 0.887 | 1.001 | 6.078 | 1.470 |

### code-cache-probes

9 measured cycles; every raw sample is retained.

| Workload | Time/previous | CPU/previous | RSS/previous | Time/CPython | RSS/CPython |
|---|---:|---:|---:|---:|---:|
| retained_cold_code | 0.999 | 0.995 | 1.004 | 0.968 | 1.937 |
| retained_warm_code | 0.999 | 0.995 | 1.000 | 1.008 | 2.087 |
| code_compile_churn | 1.005 | 0.995 | 1.001 | 1.001 | 2.141 |
| class_version_churn | 0.991 | 0.996 | 1.005 | 5.993 | 1.857 |
| type_creation | 0.958 | 0.966 | 1.001 | 1.207 | 1.179 |

### controls

9 measured cycles; every raw sample is retained.

| Workload | Time/previous | CPU/previous | RSS/previous | Time/CPython | RSS/CPython |
|---|---:|---:|---:|---:|---:|
| str_methods | 0.990 | 0.991 | 1.008 | 3.089 | 2.087 |
| list_ops | 0.996 | 0.998 | 1.002 | 13.911 | 1.981 |
| attr_access | 0.999 | 1.005 | 1.004 | 3.036 | 2.119 |
| call_overhead | 0.997 | 0.999 | 1.006 | 9.097 | 2.044 |
| jitkernels | 1.001 | 1.013 | 1.003 | 0.871 | 1.996 |

### gc-index-mix-probes

9 measured cycles; every raw sample is retained.

| Workload | Time/previous | CPU/previous | RSS/previous | Time/CPython | RSS/CPython |
|---|---:|---:|---:|---:|---:|
| retained_lists | 0.983 | 0.988 | 1.002 | 9.955 | 2.579 |
| retained_lists_gc_disabled | 0.943 | 0.954 | 1.000 | 8.819 | 2.388 |
| retained_dicts | 0.926 | 0.937 | 1.002 | 9.675 | 2.721 |
| retained_dicts_gc_disabled | 0.783 | 0.865 | 1.002 | 8.726 | 2.536 |
| retained_sets | 0.823 | 0.897 | 1.001 | 3.483 | 1.470 |
| retained_sets_gc_disabled | 0.823 | 0.886 | 1.003 | 5.878 | 1.469 |
| retained_tuples | 0.986 | 0.985 | 0.963 | 8.171 | 2.618 |
| retained_tuples_gc_disabled | 0.967 | 0.973 | 1.002 | 7.331 | 2.278 |
| rooted_full_scans | 1.017 | 0.994 | 1.000 | 8.403 | 1.823 |
| unreachable_cycle_scans | 0.974 | 0.980 | 1.002 | 18.694 | 3.500 |
| freeze_unfreeze_scans | 0.996 | 0.977 | 1.001 | 3.533 | 1.711 |

### Explicit GC pauses

Seven alternating process cycles collect a 10,000-node graph 51 times after one discarded collection. Automatic GC is disabled. p95 is the 49th ordered pause of 51. Ratios compare paired per-process summaries, including per-process maxima. These are explicit collection pauses, not application-wide or automatic-GC tail latency.

| Graph | Median/previous | p95/previous | Maximum/previous | RSS/previous | Median/CPython | p95/CPython | RSS/CPython |
|---|---:|---:|---:|---:|---:|---:|---:|
| rooted | 0.996 | 0.996 | 0.995 | 0.999 | 9.683 | 9.428 | 2.323 |
| unreachable | 0.988 | 0.986 | 1.005 | 0.999 | 11.706 | 11.889 | 3.802 |
| frozen | 0.968 | 0.936 | 1.013 | 1.000 | 193.664 | 128.005 | 2.274 |

### instance-dict-probes

9 measured cycles; every raw sample is retained.

| Workload | Time/previous | CPU/previous | RSS/previous | Time/CPython | RSS/CPython |
|---|---:|---:|---:|---:|---:|
| retained_cold_plain | 0.921 | 0.934 | 1.002 | 13.901 | 2.928 |
| retained_cold_empty_slots | 0.924 | 0.935 | 1.001 | 16.415 | 3.674 |
| retained_exported_empty_dict | 0.937 | 0.941 | 1.001 | 9.708 | 2.637 |
| retained_populated_dict | 0.932 | 0.941 | 1.000 | 14.014 | 3.826 |
| retained_cold_native_int | 0.927 | 0.937 | 1.001 | 15.483 | 3.151 |
| retained_cold_native_list | 0.937 | 0.949 | 1.001 | 13.686 | 3.003 |
| churn_plain | 0.970 | 0.977 | 1.005 | 14.335 | 1.862 |
| churn_empty_slots | 0.970 | 0.983 | 1.003 | 17.911 | 1.859 |
| class_attribute_cold_dict | 1.007 | 0.987 | 1.002 | 10.610 | 1.858 |
| method_cold_dict | 1.013 | 1.004 | 1.002 | 14.720 | 1.853 |

### jit-slot-probes

9 measured cycles; every raw sample is retained.

| Workload | Time/previous | CPU/previous | RSS/previous | Time/CPython | RSS/CPython |
|---|---:|---:|---:|---:|---:|
| native_slots_1 | 0.999 | 1.002 | 1.002 | 1.254 | 1.976 |
| native_slots_8 | 0.998 | 1.016 | 1.004 | 1.236 | 1.973 |
| native_slots_16 | 1.010 | 1.012 | 1.004 | 1.258 | 1.971 |
| native_alternating_slot_orders | 1.009 | 1.009 | 1.001 | 1.365 | 1.964 |

### scalar-result-probes

9 measured cycles; every raw sample is retained.

| Workload | Time/previous | CPU/previous | RSS/previous | Time/CPython | RSS/CPython |
|---|---:|---:|---:|---:|---:|
| integer_callback_sum | 0.999 | 0.999 | 1.003 | 2.407 | 1.977 |
| integer_callback_left | 0.998 | 1.002 | 1.003 | 2.359 | 1.971 |
| keyword_callback_sum | 0.974 | 0.978 | 1.003 | 13.965 | 1.974 |
| callable_instance_sum | 1.005 | 1.004 | 1.002 | 9.969 | 1.974 |

### slot-index-probes

9 measured cycles; every raw sample is retained.

| Workload | Time/previous | CPU/previous | RSS/previous | Time/CPython | RSS/CPython |
|---|---:|---:|---:|---:|---:|
| retained_slots_1 | 0.950 | 0.954 | 1.001 | 15.513 | 3.067 |
| last_slot_access_1 | 1.004 | 0.996 | 1.002 | 7.811 | 1.850 |
| retained_slots_2 | 0.942 | 0.947 | 1.002 | 15.011 | 3.177 |
| last_slot_access_2 | 1.005 | 0.988 | 1.003 | 7.922 | 1.855 |
| retained_slots_8 | 0.949 | 0.950 | 1.001 | 13.732 | 2.640 |
| last_slot_access_8 | 1.007 | 0.988 | 1.004 | 7.788 | 1.851 |
| retained_slots_9 | 0.950 | 0.953 | 1.001 | 12.701 | 2.971 |
| last_slot_access_9 | 1.001 | 0.990 | 1.001 | 7.717 | 1.847 |
| retained_slots_16 | 0.963 | 0.963 | 1.000 | 14.151 | 4.037 |
| last_slot_access_16 | 1.008 | 1.000 | 1.006 | 7.917 | 1.851 |
| slot_delete_reinsert | 0.997 | 0.988 | 1.001 | 14.057 | 1.852 |
| slot_access_8_index_0 | 0.993 | 0.989 | 1.004 | 7.779 | 1.853 |
| slot_access_8_index_3 | 1.006 | 0.991 | 1.005 | 7.901 | 1.851 |
| alternating_slot_orders | 1.004 | 1.000 | 1.003 | 15.366 | 1.850 |

### type-cache-probes

9 measured cycles; every raw sample is retained.

| Workload | Time/previous | CPU/previous | RSS/previous | Time/CPython | RSS/CPython |
|---|---:|---:|---:|---:|---:|
| cached_short_attribute | 0.991 | 0.998 | 1.001 | 8.996 | 1.973 |
| cached_missing_attribute | 0.998 | 0.998 | 1.000 | 31.721 | 1.972 |
| cached_long_attribute | 0.999 | 0.998 | 0.998 | 8.569 | 1.972 |
| many_class_namespaces | 0.996 | 0.999 | 1.003 | 7.986 | 1.863 |

### warm

9 measured cycles; every raw sample is retained.

| Workload | Time/previous | CPU/previous | RSS/previous | Time/CPython | RSS/CPython |
|---|---:|---:|---:|---:|---:|
| str_methods | 0.996 | 0.996 | 1.002 | 2.759 | 3.836 |
| list_ops | 1.001 | 1.003 | 1.001 | 13.901 | 1.952 |
| attr_access | 1.001 | 1.001 | 1.006 | 2.921 | 2.090 |
| call_overhead | 1.004 | 1.006 | 1.008 | 9.040 | 2.013 |
| jitkernels | 0.996 | 1.006 | 1.003 | 0.833 | 1.958 |

## probes

7 measured cycles.

| Workload | Time/checkpoint | CPU/checkpoint | RSS/checkpoint | Time/CPython | CPU/CPython | RSS/CPython |
|---|---:|---:|---:|---:|---:|---:|
| plain_instances | 0.815 | 0.832 | 0.869 | 15.642 | 7.517 | 3.826 |
| slotted_instances | 0.695 | 0.721 | 0.598 | 16.227 | 7.240 | 3.067 |
| memoryviews | 0.925 | 0.946 | 0.722 | 4.169 | 2.838 | 1.513 |
| memoryview_access | 0.963 | 0.988 | 0.953 | 6.233 | 2.622 | 1.856 |
| materialized_frames | 0.976 | 0.993 | 0.879 | 8.181 | 2.368 | 2.049 |
| bytesio_streams | 0.889 | 0.944 | 0.825 | 10.389 | 2.697 | 2.921 |
| type_creation | 0.945 | 0.969 | 0.931 | 1.204 | 1.151 | 1.179 |
| float_repr | 0.465 | 0.652 | 0.958 | 1.017 | 1.246 | 1.630 |
| float_str | 0.459 | 0.636 | 0.958 | 1.048 | 1.259 | 1.638 |
| int_repr | 0.813 | 0.912 | 0.954 | 3.954 | 2.047 | 1.632 |
| complex_repr | 0.375 | 0.545 | 0.953 | 0.767 | 1.048 | 1.669 |
| json_float_array | 0.148 | 0.476 | 0.845 | 0.229 | 0.974 | 2.169 |
| json_int_array | 0.296 | 0.702 | 0.852 | 0.727 | 2.127 | 2.135 |
| json_repeated_keys | 0.699 | 0.817 | 0.851 | 1.019 | 1.677 | 2.215 |
| json_unique_keys | 0.864 | 0.897 | 0.825 | 1.174 | 1.947 | 2.271 |

## startup

31 measured cycles.

| Workload | Time/checkpoint | CPU/checkpoint | RSS/checkpoint | Time/CPython | CPU/CPython | RSS/CPython |
|---|---:|---:|---:|---:|---:|---:|
| startup | 1.012 | 1.014 | 0.952 | 1.419 | 1.474 | 1.851 |
| startup_no_site | 1.035 | 1.041 | 1.001 | 0.605 | 0.581 | 1.545 |
| imports | 0.921 | 0.920 | 0.842 | 2.894 | 3.075 | 2.460 |

## parallel

5 measured cycles.

| Workload | Time/previous | CPU/previous | RSS/previous | Time/CPython | CPU/CPython | RSS/CPython |
|---|---:|---:|---:|---:|---:|---:|
| 1 | 1.004 | 1.004 | 1.001 | 0.119 | 0.119 | 3.433 |
| 0 | 0.978 | 1.012 | 0.999 | 0.764 | 5.664 | 3.197 |

## class-parallel

5 measured cycles.

| Workload | Time/previous | CPU/previous | RSS/previous | Time/CPython | CPU/CPython | RSS/CPython |
|---|---:|---:|---:|---:|---:|---:|
| 1 | 1.000 | 1.000 | 1.000 | 6.228 | 6.208 | 3.525 |
| 0 | 1.007 | 1.001 | 0.997 | 7.780 | 59.882 | 3.270 |

CPython is the installed GIL build; a free-threaded CPython comparison is unavailable. WeavePy GIL-disabled measurements use the interpreter because native execution remains gated. CPU time includes participating threads. No complete free-threaded safety claim is made.

Build latency was not measured under controlled conditions. No build-time improvement is claimed. Frozen source and measurement inputs, release and library hashes, validation output, isolated index samples, all VM measurements, and regressions are preserved in the census archive.

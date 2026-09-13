# Consolidate nested native scratch buffers

Decision: Retain nested-buffer consolidation as an incremental speed improvement, with a measured deep/wide retention regression; test a byte budget next.

Against 6810, the 41-control cold/warm JIT time means are 0.956232248 and 0.937979662. All ten new affected warm shapes win every pair. Depth-64 recursion takes 0.449510096 times the predecessor time; depth 256 takes 0.604907678. Twelve warm and sixteen cold rows regress, including some roughly 1% to 2% losses. Every row remains.
Against 6810, the unchanged 23-workload JIT mean is 0.984982864. The 24-process wall/CPU/RSS means are 0.996752959, 0.995085377, and 0.998867684. Fibonacci takes 0.786167697 times the predecessor time. Nine workload rows and eight RSS rows regress; the small mean RSS difference is not a substantial memory win.
Against retained bf2, the 23-workload JIT mean is 0.984608399. The 24-process wall/CPU/RSS means are 0.996101732, 0.994239842, and 0.998510165. Seven workload rows and four RSS rows regress. Fibonacci is 0.795521516 and call overhead is 0.976547464; datetime is 1.009990000 and pickle is 1.003092164.
The retained comparison is still 3.326884448 times CPython for the 23 workloads, with six wins; peak RSS is 2.003992435 times CPython, with no wins. Historical 21-row time is 2.051095819 times CPython. Keep cohorts distinct and do not multiply ratios across independent runs.
The retained focused cold/warm means are 0.935320765 and 0.918927741, including inherited formatter/default-binding effects. The depth-64 and depth-256 warm ratios reproduce at 0.449977595 and 0.604763817, both seven wins. The scalar-leaf and 64-argument fallback controls are slightly slower against bf2.
Binary size remains 44,097,264 bytes. Static nested native entry stack shrinks from 1,104 to 1,040 bytes. Direct entry remains 960 bytes and both dynamic-call wrappers remain 448 bytes. Fewer pool operations are not a fixed allocator-call saving.
A separate post-timing allocation diagnostic combines 128 locals with recursion depths 8, 64, and 256. All CPython and runtime checksums and intended native paths pass; all six profiled child processes exit normally with unchanged frozen caches. MallocStackLogging attributes 29,168 versus 13,264 live bytes to the scratch take sites at depth 8, but 33,072 versus 84,992 bytes at both depths 64 and 256. The larger entries retain 51,920 extra allocator-reported bytes after deep/wide calls. This is a real retention tradeoff, distinct from uninstrumented peak RSS and cumulative allocator churn.
The first allocation parser searched only twelve stack frames; its corrected version searches all frames. Both analyses and raw captures remain. All overall malloc totals and scratch-site totals agree between versions. No workload, snapshot, or timing was rerun for the analysis correction.
All 351 VM tests, 156 C API tests, 99 targeted checks, 275 compatibility checks, and 41 fixture paths pass. All 340 source and 197 declared method/input identities remain unchanged. Four timing stages complete sequentially under the exclusive lease. The supplemental diagnostic runs only afterward under the same lease. The preexisting native-frame identity limitation remains unresolved. The user goal remains unachieved.

Next step: Keep the 6ba candidate as the immediate experimental reference. Add and measure a capacity-byte budget for reusable scratch storage while preserving the count cap and all ownership/continuation behavior. Include combined wide/deep timing and repeat the separately labeled allocation diagnostic. Then test reusing the existing artifact bundle in activation contexts.

Candidate: `6ba383e534d29c796f1823d75c78308c0b03d7e1498ef1514412bd926e8dae8c`, 44,097,264 bytes.
Direct reference: time formatter 6810,
`681043b9f62cbd4fdb3f73756ecdf8ac877c1923b6a509f11ff1128dc973443c`.
Retained acceptance reference: thin value storage bf2,
`bf2db3c58aae37ad8dee45c0859585a32af2cd44c97a2dcaed7a61f9f0df40e4`.

Phase 77 shell reuse was deferred and archived. All 339 predecessor source
hashes and the 6810 CLI matched before this experiment. The prior default-
binding and formatter changes remain experimental in the direct reference.

Only nested non-leaf scratch storage changes: two pooled owners replace five.
Disjoint u64 slices hold locals, stack spill, and arguments; disjoint u32 slices
hold spill and argument tags. The existing direct-entry layout is reused.
Owners remain live through native execution and deoptimized continuations,
and return to the pools before pin draining in the original lifetime order.
No pool borrow crosses Python. Scalar-leaf entry, direct entry, receivers,
defaults, guards, recursion accounting, exceptions, and pin semantics remain.
The pools are LIFO with a 64-entry cap. Supplemental post-timing allocation
captures and both attribution analyses remain under retention. They use the
same exclusive lease after all four stages, with no runtime source changes.
Fewer pool operations do not prove fewer
allocator calls; combined capacities and stack frames are measured tradeoffs.

All 351 VM tests, 156 C API tests, 99 targeted Python checks, 275 outside-sandbox
compatibility checks, strict format/lint, and no-JIT checks pass. The new oracle
covers alternating scratch widths, deep recursion, integer overflow and float
fallback, object identity and weak lifetimes, callbacks without replay, and
default mutation. CPython and predecessor JIT/interpreter/GIL0 agree. GIL0
disables JIT and provides correctness evidence only. The separate frame audit
preserves the known native identity mismatch; it is not fixed by this change.

All 340 source and 197 declared method/input identities are frozen.
All preceding 29 controls remain byte-identical. Twelve added controls cover
argument counts, local counts, self/mutual recursion, methods, and scalar-leaf
entry. Initial argument controls compiled but used generic/direct entry, so v2
adds post-call arithmetic to establish a native-compatible result lane. Original
inputs, CPython results, and untimed traces remain. No v1 timing occurred.
All 41 independent CPython checksums pass twice. Ten new affected shapes each
execute more than 30,000 non-leaf native calls. The 64-argument nested function
remains interpreted due to its LIST_APPEND shape; scalar-leaf entry remains
an unaffected control. Candidate coverage must satisfy the declared baseline
paths. Static machine code is preserved separately from timing.

Four stages run sequentially outside the filesystem sandbox under the exclusive
lease and unchanged load gate. No owned build/test/profile/install or runtime/
active-harness edit overlaps a gate that can launch or measure. Each dependent
launch follows inspected session and gate completion. Focused stages retain
seven alternating pairs for 41 cold and warm controls; full stages retain 31
startup/import samples across nine controls and five pairs for the unchanged
24-fixture census. Keep all launched samples, failures, losses, cache assertions,
load observations, and VM/swap snapshots. Ratios below 1 favor the candidate.
Means aggregate per-fixture medians of paired ratios. Keep the historical
21-row, 23-workload, and 24-process cohorts distinct. Retained comparisons
are measured directly, not multiplied across unrelated runs.

## Direct comparison with 6810

| Metric | Candidate / 6810 | Candidate / CPython | Wins over CPython |
| --- | ---: | ---: | ---: |
| JIT workload time (23) | 0.984982864 | 3.317852313 | 6 |
| Interpreter workload time (23) | 0.998654961 | 9.260933334 | 1 |
| JIT process wall time (24) | 0.996752959 | 3.195862537 | 4 |
| JIT CPU time (24) | 0.995085377 | 3.257849187 | 4 |
| JIT peak RSS (24) | 0.998867684 | 2.009256460 | 0 |
| Interpreter process wall time (24) | 0.997080245 | 5.691235082 | 1 |
| Interpreter CPU time (24) | 0.997215798 | 5.879756125 | 1 |
| Interpreter peak RSS (24) | 0.997812858 | 1.833141322 | 0 |

| Focused control | JIT time / 6810 | JIT time / CPython | RSS / 6810 | Time wins (7 pairs) |
| --- | ---: | ---: | ---: | ---: |
| cold/generic_pos_0 | 0.995417 | 11.682573 | 1.000560 | 4 |
| cold/generic_pos_1 | 1.011652 | 10.952467 | 0.997204 | 1 |
| cold/generic_pos_2 | 0.991478 | 10.185795 | 1.001120 | 6 |
| cold/generic_pos_3 | 1.004279 | 9.364453 | 0.997764 | 3 |
| cold/generic_pos_4 | 0.983939 | 8.747017 | 1.001688 | 5 |
| cold/generic_pos_5 | 0.988890 | 8.179556 | 0.998880 | 5 |
| cold/generic_pos_8 | 1.002923 | 7.572448 | 1.000000 | 2 |
| cold/generic_pos_16 | 0.999132 | 6.320753 | 1.000558 | 4 |
| cold/generic_pos_32 | 1.004452 | 8.062050 | 1.001124 | 2 |
| cold/generic_kw_0 | 1.005961 | 7.491814 | 0.999439 | 1 |
| cold/generic_kw_1 | 1.011643 | 7.303885 | 1.001690 | 2 |
| cold/generic_kw_4 | 1.012776 | 6.653241 | 1.000000 | 1 |
| cold/generic_kw_8 | 0.992058 | 6.015664 | 0.998325 | 4 |
| cold/generic_method_1 | 1.001587 | 15.280809 | 1.002250 | 3 |
| cold/generic_method_4 | 1.006287 | 14.704574 | 1.000000 | 3 |
| cold/compiled_alias_1 | 0.990162 | 3.552651 | 0.999440 | 6 |
| cold/compiled_alias_4 | 1.003483 | 2.259003 | 1.000562 | 2 |
| cold/compiled_alias_8 | 1.023772 | 1.653708 | 1.000562 | 0 |
| cold/generic_object_1 | 0.994330 | 12.459189 | 0.998884 | 4 |
| cold/generic_object_4 | 0.992924 | 9.471841 | 0.999438 | 6 |
| cold/generic_object_4_compiled | 1.009869 | 9.803716 | 0.996126 | 2 |
| cold/time_iso_auto | 1.005651 | 8.433641 | 1.001066 | 3 |
| cold/format_callback_fallback | 0.995505 | 7.882756 | 1.000000 | 4 |
| cold/override_64_all_supplied | 1.004554 | 5.530287 | 1.001114 | 3 |
| cold/kw_compiled_64_omitted | 0.996952 | 16.844580 | 0.995568 | 5 |
| cold/kw_override_16_omitted | 0.995471 | 4.867233 | 1.003361 | 4 |
| cold/native_recursive_callback | 0.840284 | 10.459209 | 0.998350 | 7 |
| cold/native_single_callback | 0.972011 | 9.373291 | 1.000000 | 7 |
| cold/native_observed_callback | 0.998336 | 10.793324 | 1.001673 | 4 |
| cold/scratch_args_1 | 0.922246 | 5.702045 | 1.000560 | 7 |
| cold/scratch_args_4 | 0.927720 | 3.329959 | 0.998319 | 7 |
| cold/scratch_args_16 | 0.956483 | 1.481310 | 1.000000 | 7 |
| cold/scratch_args_64 | 1.003076 | 4.433320 | 0.997352 | 2 |
| cold/scratch_locals_32 | 0.944861 | 0.955024 | 0.996470 | 7 |
| cold/scratch_locals_128 | 0.992587 | 2.018204 | 1.000000 | 5 |
| cold/scratch_recursive_8 | 0.858071 | 2.494456 | 0.997791 | 7 |
| cold/scratch_recursive_64 | 0.559956 | 3.062650 | 1.000000 | 7 |
| cold/scratch_recursive_256 | 0.685758 | 3.667880 | 0.998364 | 7 |
| cold/scratch_mutual_32 | 0.776563 | 5.153509 | 1.001109 | 7 |
| cold/scratch_bound | 0.953222 | 8.527656 | 0.997259 | 7 |
| cold/scratch_scalar_leaf_control | 1.017696 | 1.821716 | 0.998883 | 1 |
| warm/generic_pos_0 | 0.984654 | 11.877905 | 1.000551 | 6 |
| warm/generic_pos_1 | 0.991537 | 11.219267 | 0.997256 | 4 |
| warm/generic_pos_2 | 1.005342 | 10.432135 | 1.001103 | 3 |
| warm/generic_pos_3 | 0.995863 | 9.389423 | 0.998900 | 4 |
| warm/generic_pos_4 | 0.974723 | 8.970537 | 0.999448 | 6 |
| warm/generic_pos_5 | 1.012882 | 8.043022 | 1.000552 | 1 |
| warm/generic_pos_8 | 1.004611 | 7.528231 | 0.998350 | 3 |
| warm/generic_pos_16 | 0.997446 | 6.293009 | 0.997806 | 4 |
| warm/generic_pos_32 | 0.992059 | 7.926689 | 1.000000 | 5 |
| warm/generic_kw_0 | 1.011633 | 7.453816 | 1.000553 | 1 |
| warm/generic_kw_1 | 1.013289 | 7.351079 | 1.001104 | 3 |
| warm/generic_kw_4 | 1.001490 | 6.645987 | 1.001103 | 3 |
| warm/generic_kw_8 | 1.003852 | 6.087652 | 0.998351 | 3 |
| warm/generic_method_1 | 0.996943 | 15.666508 | 0.998899 | 5 |
| warm/generic_method_4 | 1.000390 | 15.223402 | 0.998346 | 3 |
| warm/compiled_alias_1 | 0.997058 | 2.411564 | 1.000000 | 5 |
| warm/compiled_alias_4 | 1.016961 | 1.411308 | 1.000000 | 2 |
| warm/compiled_alias_8 | 0.988937 | 1.044205 | 0.999450 | 4 |
| warm/generic_object_1 | 0.998718 | 12.641458 | 0.998351 | 4 |
| warm/generic_object_4 | 0.987641 | 9.292335 | 0.998900 | 6 |
| warm/generic_object_4_compiled | 0.971165 | 9.417650 | 0.995093 | 6 |
| warm/time_iso_auto | 1.015697 | 8.073802 | 1.000000 | 2 |
| warm/format_callback_fallback | 1.000396 | 8.056686 | 1.002637 | 3 |
| warm/override_64_all_supplied | 0.998228 | 5.723233 | 0.998906 | 4 |
| warm/kw_compiled_64_omitted | 1.000932 | 17.346266 | 1.002194 | 3 |
| warm/kw_override_16_omitted | 0.999048 | 4.673363 | 1.002755 | 4 |
| warm/native_recursive_callback | 0.826842 | 9.467501 | 0.998374 | 7 |
| warm/native_single_callback | 0.972645 | 8.753112 | 0.998350 | 7 |
| warm/native_observed_callback | 0.984367 | 11.828063 | 0.997260 | 4 |
| warm/scratch_args_1 | 0.907768 | 4.844432 | 0.997803 | 7 |
| warm/scratch_args_4 | 0.921547 | 2.755401 | 1.002212 | 7 |
| warm/scratch_args_16 | 0.912903 | 1.116769 | 0.996239 | 7 |
| warm/scratch_args_64 | 0.993413 | 4.309760 | 0.998442 | 6 |
| warm/scratch_locals_32 | 0.882148 | 0.359410 | 0.997004 | 7 |
| warm/scratch_locals_128 | 0.926583 | 0.140576 | 0.999239 | 7 |
| warm/scratch_recursive_8 | 0.814406 | 1.639933 | 0.996187 | 7 |
| warm/scratch_recursive_64 | 0.449510 | 1.986131 | 1.001631 | 7 |
| warm/scratch_recursive_256 | 0.604908 | 2.656224 | 0.995179 | 7 |
| warm/scratch_mutual_32 | 0.737013 | 4.129516 | 0.994574 | 7 |
| warm/scratch_bound | 0.950000 | 8.541585 | 0.998397 | 7 |
| warm/scratch_scalar_leaf_control | 0.990576 | 0.971673 | 0.996701 | 6 |

## Direct comparison with bf2

| Metric | Candidate / bf2 | Candidate / CPython | Wins over CPython |
| --- | ---: | ---: | ---: |
| JIT workload time (23) | 0.984608399 | 3.326884448 | 6 |
| Interpreter workload time (23) | 0.999312346 | 9.300850264 | 1 |
| JIT process wall time (24) | 0.996101732 | 3.179744157 | 4 |
| JIT CPU time (24) | 0.994239842 | 3.244317677 | 4 |
| JIT peak RSS (24) | 0.998510165 | 2.003992435 | 0 |
| Interpreter process wall time (24) | 0.997869687 | 5.692029744 | 1 |
| Interpreter CPU time (24) | 0.998118954 | 5.886642640 | 1 |
| Interpreter peak RSS (24) | 0.998988050 | 1.832063298 | 0 |

| Focused control | JIT time / bf2 | JIT time / CPython | RSS / bf2 | Time wins (7 pairs) |
| --- | ---: | ---: | ---: | ---: |
| cold/generic_pos_0 | 0.973934 | 11.692886 | 1.000000 | 7 |
| cold/generic_pos_1 | 0.994737 | 10.817713 | 0.997764 | 6 |
| cold/generic_pos_2 | 0.997224 | 10.338070 | 0.999441 | 4 |
| cold/generic_pos_3 | 0.988612 | 9.215092 | 0.999439 | 6 |
| cold/generic_pos_4 | 0.995397 | 8.914122 | 0.999440 | 5 |
| cold/generic_pos_5 | 1.013302 | 8.080596 | 1.000000 | 2 |
| cold/generic_pos_8 | 1.028457 | 7.536512 | 0.998324 | 1 |
| cold/generic_pos_16 | 1.016611 | 6.089935 | 0.996098 | 1 |
| cold/generic_pos_32 | 1.019081 | 7.958678 | 0.998322 | 1 |
| cold/generic_kw_0 | 0.987992 | 7.467494 | 0.998881 | 5 |
| cold/generic_kw_1 | 0.990926 | 7.195390 | 1.000559 | 6 |
| cold/generic_kw_4 | 0.999989 | 6.800672 | 1.000000 | 4 |
| cold/generic_kw_8 | 0.998329 | 6.052839 | 1.000559 | 4 |
| cold/generic_method_1 | 1.001376 | 15.364341 | 1.001685 | 3 |
| cold/generic_method_4 | 0.992893 | 14.392232 | 1.000561 | 5 |
| cold/compiled_alias_1 | 0.992313 | 3.552900 | 1.000561 | 5 |
| cold/compiled_alias_4 | 0.989586 | 2.173222 | 0.997768 | 6 |
| cold/compiled_alias_8 | 1.018370 | 1.702361 | 0.998884 | 1 |
| cold/generic_object_1 | 1.009699 | 12.566066 | 0.999441 | 1 |
| cold/generic_object_4 | 0.983700 | 9.562411 | 0.998314 | 6 |
| cold/generic_object_4_compiled | 0.998213 | 9.726692 | 0.996118 | 4 |
| cold/time_iso_auto | 0.621359 | 8.548403 | 1.003205 | 7 |
| cold/format_callback_fallback | 1.054708 | 8.049178 | 1.003753 | 0 |
| cold/override_64_all_supplied | 1.008864 | 6.062687 | 0.992778 | 1 |
| cold/kw_compiled_64_omitted | 1.012461 | 17.047809 | 1.001117 | 1 |
| cold/kw_override_16_omitted | 0.620083 | 4.940472 | 1.000000 | 7 |
| cold/native_recursive_callback | 0.831393 | 10.398798 | 0.996703 | 7 |
| cold/native_single_callback | 0.974902 | 9.163155 | 0.994986 | 6 |
| cold/native_observed_callback | 1.021731 | 10.939044 | 1.001679 | 0 |
| cold/scratch_args_1 | 0.932663 | 5.550865 | 1.001115 | 7 |
| cold/scratch_args_4 | 0.930001 | 3.301207 | 0.997214 | 7 |
| cold/scratch_args_16 | 0.951183 | 1.478380 | 0.999450 | 7 |
| cold/scratch_args_64 | 1.009168 | 4.479826 | 0.999471 | 1 |
| cold/scratch_locals_32 | 0.948833 | 0.955838 | 0.994955 | 7 |
| cold/scratch_locals_128 | 0.991056 | 2.035921 | 1.000000 | 5 |
| cold/scratch_recursive_8 | 0.858744 | 2.391105 | 0.999444 | 7 |
| cold/scratch_recursive_64 | 0.553863 | 3.010288 | 0.999448 | 7 |
| cold/scratch_recursive_256 | 0.692295 | 3.683548 | 0.997818 | 7 |
| cold/scratch_mutual_32 | 0.779635 | 5.360196 | 0.997785 | 7 |
| cold/scratch_bound | 0.944482 | 8.634001 | 0.995079 | 7 |
| cold/scratch_scalar_leaf_control | 0.999054 | 1.840756 | 0.996092 | 4 |
| warm/generic_pos_0 | 0.999031 | 12.008372 | 0.999447 | 4 |
| warm/generic_pos_1 | 0.996574 | 11.179088 | 1.000553 | 5 |
| warm/generic_pos_2 | 0.992478 | 10.384452 | 1.000552 | 5 |
| warm/generic_pos_3 | 0.989230 | 9.200744 | 1.001657 | 5 |
| warm/generic_pos_4 | 0.981769 | 8.886636 | 0.997795 | 5 |
| warm/generic_pos_5 | 0.989589 | 8.275728 | 1.001106 | 4 |
| warm/generic_pos_8 | 0.997170 | 7.562523 | 1.000000 | 4 |
| warm/generic_pos_16 | 1.022404 | 6.277054 | 0.995628 | 0 |
| warm/generic_pos_32 | 1.000889 | 8.053289 | 0.999449 | 3 |
| warm/generic_kw_0 | 0.999995 | 7.629619 | 0.998899 | 4 |
| warm/generic_kw_1 | 0.991538 | 7.305372 | 0.997790 | 5 |
| warm/generic_kw_4 | 0.973540 | 6.523661 | 0.997795 | 7 |
| warm/generic_kw_8 | 0.996288 | 6.022066 | 0.999448 | 4 |
| warm/generic_method_1 | 1.007151 | 16.316460 | 1.002206 | 2 |
| warm/generic_method_4 | 1.009643 | 15.275454 | 0.999448 | 1 |
| warm/compiled_alias_1 | 0.992578 | 2.411782 | 0.999448 | 5 |
| warm/compiled_alias_4 | 1.019368 | 1.416438 | 0.997248 | 1 |
| warm/compiled_alias_8 | 1.036573 | 1.080800 | 0.999449 | 2 |
| warm/generic_object_1 | 0.948589 | 12.495646 | 0.997245 | 7 |
| warm/generic_object_4 | 0.994965 | 9.500530 | 0.998898 | 5 |
| warm/generic_object_4_compiled | 0.997094 | 9.774319 | 0.997812 | 5 |
| warm/time_iso_auto | 0.614242 | 8.198778 | 0.998431 | 7 |
| warm/format_callback_fallback | 1.064466 | 7.942208 | 0.997890 | 0 |
| warm/override_64_all_supplied | 1.027089 | 5.530808 | 0.997818 | 0 |
| warm/kw_compiled_64_omitted | 1.014612 | 16.554938 | 0.996181 | 0 |
| warm/kw_override_16_omitted | 0.612902 | 4.736714 | 0.998899 | 7 |
| warm/native_recursive_callback | 0.814915 | 9.376300 | 0.996224 | 7 |
| warm/native_single_callback | 0.947147 | 8.476082 | 0.996712 | 7 |
| warm/native_observed_callback | 1.014261 | 11.515326 | 0.996156 | 1 |
| warm/scratch_args_1 | 0.917301 | 4.820956 | 0.999450 | 7 |
| warm/scratch_args_4 | 0.918056 | 2.767160 | 0.994536 | 7 |
| warm/scratch_args_16 | 0.925815 | 1.134302 | 1.000000 | 7 |
| warm/scratch_args_64 | 1.006523 | 4.331865 | 1.000000 | 1 |
| warm/scratch_locals_32 | 0.889387 | 0.366045 | 0.996509 | 7 |
| warm/scratch_locals_128 | 0.925683 | 0.142235 | 0.999746 | 7 |
| warm/scratch_recursive_8 | 0.815026 | 1.640552 | 0.997818 | 7 |
| warm/scratch_recursive_64 | 0.449978 | 1.908581 | 0.995671 | 7 |
| warm/scratch_recursive_256 | 0.604764 | 2.651671 | 0.993579 | 7 |
| warm/scratch_mutual_32 | 0.745884 | 4.106517 | 0.995652 | 7 |
| warm/scratch_bound | 0.948770 | 8.757794 | 0.998927 | 7 |
| warm/scratch_scalar_leaf_control | 1.007043 | 0.984538 | 0.997794 | 2 |

All raw samples and development outcomes remain. Large text uses lossless
gzip; index.json records stored and original hashes. Executables and frozen
caches remain local. Intermediate status notes are historical. Observed release
build duration is not a controlled build-time metric. The user goal remains
unachieved. No universal, energy, portability, or native parallel-scaling claim
follows.

# Scalar reads and iteration on heterogeneous native lists

The validated candidate substantially speeds the six focused scalar-read workloads compared with phase 93, with a small increase in measured peak RSS. Warm indexed reads average about 2.17 times faster than CPython across the three scalar types. Cold indexing, list iteration, and peak memory still trail CPython. Full-suite and retained-baseline performance remain unmeasured, and the candidate has not been promoted to the primary source checkout or CLI.

The production change admits exact machine integers, floats, and Booleans through the existing object-pin branches of wpjit_list_get and wpjit_list_next. None encoding, exact homogeneous lanes, bounds checks, live iteration length, pin caps, consumer guards, helper ABI, and frame layout are unchanged. Only two VM source files differ from phase 93; counters exist only in test builds.

## Validation and native execution

The unchanged read guards produce correct values but zero completed native scalar reads in the baseline regression. The candidate completes 19,996 indexed reads and 19,999 iteration reads in each single-call, 20,000-read int/float/bool case. The tests require at least 18,000 reads and exercise three or four temporary-pin pressure exits. Exact expected values match interpreter execution. Separate tests require native prefixes before list mutation, unsupported large-integer fallback, and out-of-range reads, preserving type, identity, and exactly-once callbacks.

The original mutation test passed its Python assertions but recorded zero native scalar reads. Its trace showed compilation; code inspection identified managed locals initialized only after 128 setup-loop backedges. Moving those initializations before setup allowed the same native-prefix assertion to pass. Source01, its failure and trace, the revised CPython oracle, and final source02 are preserved. The production helper change did not change between those test versions.

Validation passed: 79 JIT tests, 372 VM tests, 156 C API tests with JIT enabled, formatting, strict all-target clippy, no-JIT checks, 99 targeted release checks across 44 fixture paths, all 275 compatibility checks, and all 24 census checksums. All 35 focused release probes match CPython; two additional semantic probes pass across five engine modes. The release pipeline ran outside the filesystem sandbox. Preexisting staticmethod class-subscription and frame-identity differences remain explicit in the inherited evidence.

Each new release scalar-read probe reduces ordinary deopts from 144 to 80. Temporary-pin pressure exits change from zero to seven for indexing and eight for iteration. These counts establish native execution and bounded reconstruction; runtime and RSS claims below come from separate samples.

## Timing provenance and limitations

The original gate expired after 600 seconds with no child or samples. Attempt 02 qualified and collected all seven pairs for the first 21 cold cases. It then stopped because the first list-store input lacked its executable __main__ timer. All 35 value oracles had passed, but that did not exercise the executable timing contract. The same wrapper omission affected eight inherited store cases and six new read cases. No valid timing sample had been collected for any of those 14 cases.

New input snapshots append the standard executable timer to those 14 files. Their complete previous workload text remains an identical prefix, and the first 21 source bodies remain byte-identical. A separate 70-command preflight checks the corrected timer contract across five engines; those printed durations are not performance samples. The corrected gated run measures only the 14 missing cold cases and all 35 previously unmeasured warm cases. The combined cold report copies every original and continuation row verbatim with source-report and hash provenance. No completed row or sample was rerun, filtered, replaced, or discarded. The cold cohort spans two qualified gate runs and should be interpreted with that limitation.

The corrected focused stage completed with all 35 cold and 35 warm cases, seven paired samples per engine and case, fixed per-binary caches, and preserved load telemetry. The subsequent full-suite gate expired after 600 seconds without a child or samples. Neither retained-baseline stage was attempted. Full-census correctness checks are not full-census performance measurements.

An additional audit initially expected geometric means across sample pairs and failed. The frozen benchmark method uses the median of paired ratios; the corrected audit checks that definition. Geometric means are used across workload rows. Both audit versions and their outcomes are retained; no benchmark metric or sample changed.

All ratios below are new divided by the reference, so lower is better. Workload ratios are medians of the seven paired ratios per case; group ratios are geometric means across their cases. Cold and warm cohorts remain separate. The new read groups are not substituted for an older cohort when describing progress. GIL-disabled runs are correctness controls only.

## Focused subgroup results against phase 93

| Mode/group | Cases | Workload time/base | Process wall/base | Process CPU/base | Peak RSS/base | Workload time/CPython | Peak RSS/CPython |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| cold/inherited | 9 | 1.004792 | 1.008139 | 1.008860 | 1.003843 | 13.087947 | 2.026752 |
| cold/arithmetic | 6 | 1.000187 | 1.020470 | 1.022462 | 1.003762 | 3.745218 | 1.960138 |
| cold/fallback | 4 | 1.006268 | 1.008960 | 1.009258 | 1.002937 | 9.932676 | 1.929774 |
| cold/changed | 2 | 1.021739 | 1.022942 | 1.026535 | 1.003037 | 9.382795 | 1.952103 |
| cold/scalar_store | 6 | 0.970416 | 1.000196 | 1.003332 | 1.001859 | 1.350546 | 1.942189 |
| cold/typed_store_control | 2 | 1.009268 | 1.012934 | 1.010785 | 1.003634 | 1.758736 | 1.945286 |
| cold/scalar_get | 3 | 0.120267 | 0.776483 | 0.765104 | 1.005365 | 1.325065 | 1.953932 |
| cold/scalar_next | 3 | 0.443166 | 0.931549 | 0.920237 | 1.002548 | 8.262854 | 1.989929 |
| warm/inherited | 9 | 1.008308 | 1.013898 | 1.013293 | 1.000434 | 12.805634 | 2.039482 |
| warm/arithmetic | 6 | 0.990950 | 1.014278 | 1.012220 | 1.001530 | 2.807277 | 1.940828 |
| warm/fallback | 4 | 1.000588 | 1.002982 | 1.004312 | 0.999864 | 9.956587 | 1.899006 |
| warm/changed | 2 | 0.996921 | 0.998646 | 0.998332 | 1.001909 | 8.411931 | 1.924079 |
| warm/scalar_store | 6 | 1.021102 | 1.015442 | 1.011919 | 1.000822 | 0.230558 | 1.910316 |
| warm/typed_store_control | 2 | 1.055819 | 1.024910 | 1.020734 | 1.000275 | 0.294446 | 1.911903 |
| warm/scalar_get | 3 | 0.046237 | 0.645975 | 0.627236 | 1.008192 | 0.460709 | 1.934377 |
| warm/scalar_next | 3 | 0.145146 | 0.878082 | 0.862118 | 1.003218 | 1.685636 | 1.961659 |

The unchanged 29 inherited cases have the following aggregate ratios versus phase 93. This keeps the comparison independent of the six newly added read cases.

| Mode | Workload time/base | Process wall/base | Process CPU/base | Peak RSS/base |
| --- | ---: | ---: | ---: | ---: |
| cold | 0.998292 | 1.010484 | 1.011909 | 1.003220 |
| warm | 1.008659 | 1.012478 | 1.011017 | 1.000753 |

## Every cold case against phase 93

| Case | Workload time/base | Peak RSS/base | Time wins/7 | RSS wins/7 |
| --- | ---: | ---: | ---: | ---: |
| keyword_constructor | 1.003148 | 1.002781 | 3 | 1 |
| keyword_default_constructor | 1.007028 | 1.006111 | 2 | 1 |
| keyword_only_control | 1.007214 | 1.001107 | 3 | 3 |
| keyword_deque | 1.031614 | 1.004532 | 1 | 1 |
| keyword_dict_control | 1.005054 | 1.001702 | 3 | 0 |
| metaclass_control | 1.011877 | 1.002221 | 1 | 2 |
| positional_constructor_control | 1.003820 | 1.005562 | 3 | 1 |
| keyword_gap_control | 0.995645 | 1.005565 | 4 | 1 |
| keyword_error_control | 0.978509 | 1.005017 | 5 | 1 |
| arithmetic_add | 0.993009 | 1.003848 | 4 | 0 |
| arithmetic_sub | 1.022678 | 1.002208 | 2 | 0 |
| arithmetic_mul | 0.990603 | 1.003867 | 4 | 0 |
| arithmetic_div | 1.012266 | 1.003299 | 3 | 1 |
| arithmetic_floordiv | 1.002405 | 1.003853 | 2 | 2 |
| arithmetic_mod | 0.980750 | 1.005504 | 5 | 1 |
| float_control | 1.007838 | 1.003361 | 2 | 1 |
| bool_control | 1.005431 | 1.003930 | 2 | 0 |
| bigint_control | 1.012641 | 1.005590 | 2 | 2 |
| object_control | 0.999208 | 0.998881 | 4 | 4 |
| changed_left | 1.012743 | 1.003309 | 1 | 0 |
| changed_right | 1.030815 | 1.002765 | 2 | 1 |
| scalar_store_int_positive | 0.951217 | 1.002237 | 4 | 3 |
| scalar_store_int_negative | 1.016850 | 1.000556 | 2 | 3 |
| scalar_store_float_positive | 0.914499 | 1.001110 | 5 | 3 |
| scalar_store_float_negative | 0.948463 | 1.000558 | 5 | 3 |
| scalar_store_bool_positive | 1.005953 | 1.003341 | 3 | 1 |
| scalar_store_bool_negative | 0.989533 | 1.003354 | 4 | 1 |
| typed_store_int | 0.997159 | 1.002796 | 4 | 1 |
| typed_store_float | 1.021524 | 1.004472 | 0 | 1 |
| scalar_get_int | 0.135683 | 1.004437 | 7 | 0 |
| scalar_next_int | 0.446727 | 1.000543 | 7 | 2 |
| scalar_get_float | 0.114943 | 1.006104 | 7 | 0 |
| scalar_next_float | 0.441484 | 1.004932 | 7 | 0 |
| scalar_get_bool | 0.111542 | 1.005556 | 7 | 1 |
| scalar_next_bool | 0.441308 | 1.002175 | 7 | 3 |

## Every warm case against phase 93

| Case | Workload time/base | Peak RSS/base | Time wins/7 | RSS wins/7 |
| --- | ---: | ---: | ---: | ---: |
| keyword_constructor | 1.021888 | 1.001639 | 3 | 2 |
| keyword_default_constructor | 0.992416 | 1.000547 | 5 | 3 |
| keyword_only_control | 1.000360 | 0.998908 | 3 | 4 |
| keyword_deque | 0.998156 | 1.000498 | 4 | 2 |
| keyword_dict_control | 1.036553 | 1.000679 | 1 | 2 |
| metaclass_control | 1.003345 | 1.000000 | 3 | 3 |
| positional_constructor_control | 0.991042 | 1.000000 | 4 | 2 |
| keyword_gap_control | 1.023406 | 1.000000 | 1 | 1 |
| keyword_error_control | 1.008570 | 1.001639 | 2 | 3 |
| arithmetic_add | 0.983551 | 1.001622 | 5 | 2 |
| arithmetic_sub | 1.014672 | 1.002703 | 3 | 3 |
| arithmetic_mul | 0.987293 | 1.001080 | 6 | 1 |
| arithmetic_div | 1.018508 | 1.001082 | 2 | 1 |
| arithmetic_floordiv | 0.973856 | 0.999461 | 6 | 4 |
| arithmetic_mod | 0.968909 | 1.003235 | 6 | 1 |
| float_control | 1.004321 | 1.000000 | 2 | 2 |
| bool_control | 1.017336 | 1.002753 | 1 | 1 |
| bigint_control | 0.973562 | 0.998900 | 5 | 6 |
| object_control | 1.007674 | 0.997808 | 3 | 5 |
| changed_left | 0.989667 | 1.000000 | 4 | 3 |
| changed_right | 1.004228 | 1.003821 | 3 | 3 |
| scalar_store_int_positive | 1.064622 | 1.002191 | 1 | 3 |
| scalar_store_int_negative | 1.027074 | 1.001647 | 2 | 1 |
| scalar_store_float_positive | 0.989456 | 1.000549 | 5 | 3 |
| scalar_store_float_negative | 1.000375 | 1.000547 | 3 | 3 |
| scalar_store_bool_positive | 1.021059 | 0.999454 | 3 | 4 |
| scalar_store_bool_negative | 1.025667 | 1.000549 | 0 | 3 |
| typed_store_int | 1.051592 | 1.001098 | 1 | 2 |
| typed_store_float | 1.060064 | 0.999452 | 0 | 4 |
| scalar_get_int | 0.052649 | 1.008719 | 7 | 0 |
| scalar_next_int | 0.144459 | 1.002145 | 7 | 1 |
| scalar_get_float | 0.043947 | 1.006554 | 7 | 0 |
| scalar_next_float | 0.140881 | 1.003757 | 7 | 0 |
| scalar_get_bool | 0.042722 | 1.009305 | 7 | 0 |
| scalar_next_bool | 0.150250 | 1.003751 | 7 | 1 |

## Size, preservation, and remaining work

The release CLI is 44,114,880 bytes, 16,512 bytes larger than phase 93. Its SHA-256 is 9317461584be4fb9409cee7cef035de6bf82a323ed457b9938d3dc807ad1fb8e. The observed 5m 44s build duration is not a controlled build-performance measurement. No energy, scaling, portability, or universal performance claim follows from this experiment.

The archive retains all 340 final source identities, both test-source versions, baseline instrumentation and failure, complete native/release validation, raw native traces, codegen, both timing-input versions, wrapper preflight, every launched sample, the verbatim cold-row provenance, all gates, failed methods, corrected audits, and the explicit missing comparisons. Executables and regenerable dependency/build/frozen caches are excluded. Inventoried cleanup removed only completed predecessor project artifacts, preserving current source, logs, references, and the active target.

The goal remains unmet. Remaining work includes peak-memory reduction, controlled full/retained comparisons, the reproduced unbound-local entry restriction, and larger datetime and ordinary-instance pickle costs identified in prior source audits. None of those follow-ups is implemented by this candidate.

# Arithmetic call-result fusion

This validated experiment reduces the execution time of six arithmetic probes, but it does not establish an overall census speed or peak-memory improvement. The primary CLI and source checkout were not replaced. Two comparisons against phase 90 completed with every sample retained. The retained-reference focused gate timed out after 600 seconds without launching a child or recording samples. The retained full comparison was not attempted.

The analyzer guards the right operand first when both opaque operands have an observed exact-integer specialization. This preserves the existing immediate dynamic-call integer-result fusion, followed by an exact-integer guard for the left operand. Each iteration converts one opaque operand, so at most two guards are needed. Inference, cache eligibility, cold rejection, fallback semantics, and the public JIT ABI are unchanged.

The baseline compiler test fails because neither dynamic call requests a direct integer result; the candidate fuses the last call and retains the separate left-operand guard. All six release arithmetic probes keep the same compilation eligibility. Native-to-native calls increase from 80,696 to 80,708, while pin-pressure exits fall from 12 to 6 per complete probe. Four noninteger controls remain uncompiled, and both changed-type cases preserve fallback results. Full list and deque functions still reject with MixedArithTypes, so their complete native execution is not established.

Validation passed: 77 JIT tests, 365 VM tests, 156 C API tests, formatting, strict all-target clippy, no-JIT checks, 99 targeted checks, 44 fixture paths, 275 compatibility checks, and all 24 census results. Inherited arithmetic tests cover six operators, both operand positions, bools, floats, bigints, custom callbacks, None errors, and exactly-once operand callbacks. Inherited keyword, local-state, indexing, and formatting checks passed. Preexisting staticmethod class-subscription and frame-identity differences remain explicit.

The release binary is 44,098,368 bytes, 16,512 bytes smaller than phase 90. This file-size reduction is not a peak-RSS improvement. The observed 5m 34s build duration is not a controlled build-performance measurement. Static prologues are unchanged from phase 90.

All ratios below are new/reference; lower is better. The 21 focused case bodies are byte-for-byte identical to phase 90: six arithmetic operators, four noninteger controls, two changing-operand cases, and nine construction controls. Subgroups are reported separately. Full workload time excludes startup (23 fixtures), while process metrics include it (24 fixtures). Focused rows have seven paired samples, full rows have five, and startup/import probes have 31. Host load qualified before each completed stage; all later telemetry and samples are retained.

## Comparison with phase 90 observed integer arithmetic

| Cohort / metric | New / reference | New / CPython | WeavePy wins vs. CPython | Reference regressions |
|---|---:|---:|---:|---:|
| all_23_workloads_excluding_startup / ns | 1.003742 | 3.497465 | 6 | 15 |
| all_processes_including_startup / wall_ns | 1.007238 | 3.144000 | 4 | 17 |
| all_processes_including_startup / cpu_ns | 1.006268 | 3.228331 | 4 | 17 |
| all_processes_including_startup / rss_bytes | 1.003688 | 2.021833 | 0 | 22 |
| historical_21_including_startup / ns | 1.004930 | 2.157785 | 6 | 14 |

The historical ratio-of-medians aggregation is 2.150345 times CPython. It must not be compared with the 23-workload aggregate as if the cohorts were identical.

| Focused case | Time / reference | RSS / reference | Time wins / 7 | RSS wins / 7 |
|---|---:|---:|---:|---:|
| cold/keyword_constructor | 1.007977 | 1.004462 | 3 | 0 |
| cold/keyword_default_constructor | 1.008512 | 1.002221 | 2 | 1 |
| cold/keyword_only_control | 1.005781 | 1.006656 | 3 | 0 |
| cold/keyword_deque | 0.985569 | 1.001003 | 6 | 2 |
| cold/keyword_dict_control | 0.995609 | 1.005113 | 4 | 0 |
| cold/metaclass_control | 0.993130 | 1.005014 | 4 | 0 |
| cold/positional_constructor_control | 0.996131 | 1.003902 | 4 | 1 |
| cold/keyword_gap_control | 1.018007 | 1.005011 | 3 | 0 |
| cold/keyword_error_control | 1.008933 | 1.003337 | 2 | 1 |
| cold/arithmetic_add | 0.967401 | 1.001100 | 7 | 2 |
| cold/arithmetic_sub | 0.958927 | 0.999449 | 6 | 4 |
| cold/arithmetic_mul | 0.935790 | 1.001654 | 7 | 0 |
| cold/arithmetic_div | 0.954607 | 1.000551 | 7 | 3 |
| cold/arithmetic_floordiv | 0.958620 | 1.001647 | 7 | 2 |
| cold/arithmetic_mod | 0.952876 | 1.001100 | 7 | 2 |
| cold/float_control | 0.999496 | 1.009556 | 4 | 2 |
| cold/bool_control | 0.989103 | 1.007307 | 5 | 0 |
| cold/bigint_control | 0.991488 | 1.003906 | 5 | 0 |
| cold/object_control | 0.994497 | 1.004487 | 5 | 1 |
| cold/changed_left | 1.003535 | 1.001104 | 3 | 2 |
| cold/changed_right | 0.985719 | 1.001104 | 5 | 2 |
| warm/keyword_constructor | 1.004035 | 1.000000 | 3 | 2 |
| warm/keyword_default_constructor | 1.000451 | 1.003823 | 3 | 1 |
| warm/keyword_only_control | 0.991289 | 1.001646 | 5 | 0 |
| warm/keyword_deque | 0.984706 | 1.002505 | 6 | 1 |
| warm/keyword_dict_control | 0.991641 | 1.003070 | 4 | 2 |
| warm/metaclass_control | 0.996923 | 1.001646 | 4 | 3 |
| warm/positional_constructor_control | 1.022672 | 1.001639 | 2 | 2 |
| warm/keyword_gap_control | 1.011125 | 1.006597 | 2 | 1 |
| warm/keyword_error_control | 1.003751 | 1.000547 | 2 | 0 |
| warm/arithmetic_add | 0.965998 | 1.001622 | 5 | 2 |
| warm/arithmetic_sub | 0.923473 | 0.998922 | 6 | 4 |
| warm/arithmetic_mul | 0.875011 | 0.999463 | 6 | 4 |
| warm/arithmetic_div | 0.972912 | 0.998924 | 5 | 6 |
| warm/arithmetic_floordiv | 0.960486 | 1.001076 | 6 | 2 |
| warm/arithmetic_mod | 0.920667 | 0.998385 | 4 | 6 |
| warm/float_control | 1.013893 | 1.004964 | 3 | 0 |
| warm/bool_control | 1.029484 | 1.001105 | 2 | 0 |
| warm/bigint_control | 1.027772 | 1.004972 | 1 | 1 |
| warm/object_control | 0.994828 | 1.002761 | 6 | 1 |
| warm/changed_left | 1.002236 | 0.998911 | 3 | 4 |
| warm/changed_right | 0.991299 | 1.002728 | 5 | 2 |

| Focused subgroup | Cases | Time / reference | RSS / reference | Time / CPython |
|---|---:|---:|---:|---:|
| cold/inherited | 9 | 1.002138 | 1.004078 | 13.139958 |
| cold/arithmetic | 6 | 0.954655 | 1.000916 | 3.767000 |
| cold/fallback | 4 | 0.993638 | 1.006312 | 10.107880 |
| cold/changed | 2 | 0.994587 | 1.001104 | 8.937862 |
| warm/inherited | 9 | 1.000674 | 1.002384 | 12.692697 |
| warm/arithmetic | 6 | 0.935794 | 0.999731 | 2.849573 |
| warm/fallback | 4 | 1.016399 | 1.003449 | 10.285189 |
| warm/changed | 2 | 0.996752 | 1.000817 | 8.296256 |

| Full fixture | Time / reference | Time / CPython | RSS / reference | RSS / CPython |
|---|---:|---:|---:|---:|
| fannkuch | 1.001782 | 9.592286 | 1.003926 | 1.912111 |
| nbody | 1.010705 | 8.964367 | 1.003874 | 1.929101 |
| fib | 0.999421 | 2.476969 | 1.007275 | 1.944805 |
| pidigits | 1.005461 | 0.908713 | 1.000000 | 1.935451 |
| pyaes | 1.003500 | 0.684245 | 1.005519 | 1.935313 |
| richards | 1.001939 | 9.362142 | 1.006149 | 1.941748 |
| sumvm | 0.990232 | 0.073516 | 1.003906 | 1.946970 |
| nested_loops | 1.057675 | 0.103323 | 1.004427 | 1.948276 |
| jitloop | 0.998406 | 0.076541 | 1.006674 | 1.955676 |
| jitkernels | 0.979871 | 0.859084 | 1.001106 | 1.933690 |
| deltablue | 0.997831 | 20.158116 | 1.000462 | 2.092843 |
| float_math | 1.006998 | 6.611562 | 1.000510 | 2.667574 |
| spectral_norm | 1.010010 | 2.217698 | 1.001642 | 1.951872 |
| json_bench | 1.003465 | 1.165335 | 1.000823 | 2.460993 |
| str_methods | 1.001686 | 2.097336 | 1.001523 | 2.123789 |
| dict_ops | 1.008611 | 5.591063 | 1.006726 | 1.924973 |
| list_ops | 1.011061 | 13.832091 | 1.009540 | 1.925966 |
| attr_access | 0.987329 | 2.674831 | 0.999460 | 1.978541 |
| call_overhead | 0.991003 | 8.047192 | 1.001620 | 1.984962 |
| generators | 1.015445 | 9.795271 | 1.006104 | 1.938841 |
| deque_ops | 1.003755 | 17.676029 | 1.005686 | 1.827135 |
| datetime_ops | 0.991737 | 124.182516 | 1.001508 | 2.144695 |
| pickle_bench | 1.010446 | 200.394794 | 1.005087 | 2.387324 |
| startup | 1.023484 | 1.416369 | 1.005059 | 1.933911 |

| Startup/import case | Wall / reference | CPU / reference | RSS / reference | Wall / CPython | RSS / CPython |
|---|---:|---:|---:|---:|---:|
| startup_matched | 1.000371 | 1.000675 | 1.005525 | 1.327968 | 1.779587 |
| startup_relocated | 0.998396 | 0.996765 | 1.004908 | 1.334209 | 1.784314 |
| no_site_matched | 1.009087 | 1.009082 | 1.005186 | 0.619495 | 1.533246 |
| imports_matched | 0.999642 | 0.998301 | 1.000863 | 2.609935 | 2.349899 |
| imports_relocated | 1.004598 | 1.003699 | 1.002162 | 2.632783 | 2.350859 |
| pickle_import_matched | 1.000481 | 0.999181 | 1.005374 | 2.083538 | 2.152151 |
| pickle_import_relocated | 0.991516 | 0.991902 | 1.005877 | 2.027397 | 2.157895 |
| accelerator_first_matched | 0.999841 | 1.001551 | 1.003925 | 2.032250 | 2.150210 |
| accelerator_first_relocated | 1.001152 | 0.999580 | 1.005376 | 2.039686 | 2.159664 |

## Interpretation and evidence

The six arithmetic probes take 4.53% less time cold and 6.42% less time warm than phase 90. Their peak-RSS ratios are 1.000916 and 0.999731. The other focused subgroups are near parity, with the warm noninteger fallback subgroup 1.64% slower. Every individual loss is retained above.

The complete 23-workload time ratio is 1.003742 versus phase 90 and 3.497465 versus CPython. The 24-process peak-RSS ratio is 1.003688 versus phase 90 and 2.021833 versus CPython. Six workloads beat CPython on workload time; none of the 24 processes beats it on peak RSS. These measurements do not establish an overall gain. The retained-reference comparison is missing and must not be inferred from earlier phases.

Complete source snapshots, the expected baseline regression, candidate tests, native and release checks, unchanged workload bodies, all raw samples, startup/import samples, complete native traces, codegen, stage telemetry, and reporting methods are preserved. Executable binaries and regenerable build/stdlib/frozen caches are excluded.

The first release-cache seed failed its disk-space precondition before copying or building. A cleanup removed the completed phase-89 test cache, then Cargo refused the cloned phase-90 cache because its CACHEDIR.TAG was missing or invalid. The initial failure, successful partial cleanup, exact refusal, independent audit, and fresh successful seed attempt are preserved; the cache guard was not bypassed. All primary and standalone CLIs and source snapshots remained intact.

These measurements do not establish universal superiority or results for energy, scaling, portability, or controlled build time. The full-census and universal performance targets remain unmet. Further work will diagnose cold arithmetic rejection in the full workloads while preserving the measured candidate.

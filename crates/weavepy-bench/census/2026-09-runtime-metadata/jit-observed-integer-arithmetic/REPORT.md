# Observed opaque integer arithmetic

This validated experiment substantially accelerates six newly admitted arithmetic paths, but it does not establish an overall census speed or peak-memory improvement. The primary CLI and source checkout were not replaced. All four planned comparisons completed, with every sample retained.

The analyzer reuses the existing coherent adaptive-cache observation for an exact integer pair at the same arithmetic instruction. It supports addition, subtraction, multiplication, true division, floor division, and modulo. Both opaque operands receive exact-integer guards before the original operation. Cold, noninteger, mismatched-operator, and unsupported observations remain conservative. No new per-op profiling state or allocation is added.

The baseline compiler regression fails with MixedArithTypes; the candidate passes. Release traces confirm that all six arithmetic kernels now compile, with 80,696 native-to-native calls per complete probe. Four noninteger controls remain uncompiled; two changed-type cases compile their integer prefix and preserve fallback results. Each arithmetic probe still records 12 pin-pressure exits. The full list and deque census functions still reject with MixedArithTypes, so their complete native execution is not established.

Validation passed: 76 JIT tests, 365 VM tests, 156 C API tests, formatting, strict all-target clippy, no-JIT checks, 99 targeted checks, 44 fixture paths, 275 compatibility checks, and all 24 census results. The two new VM tests cover all six arithmetic operators, native calls, both guard positions, bools, floats, bigints, custom arithmetic callbacks, None errors, and exactly-once operand callbacks. Inherited keyword, local-state, indexing, and formatting checks passed. Preexisting staticmethod class-subscription and frame-identity differences remain explicit.

The release binary is 44,114,880 bytes, 16,608 bytes larger than phase 89. The observed 5m 50s build duration is not a controlled build-performance measurement. Static prologue captures are retained without treating them as cumulative stack or RSS measurements.

All ratios below are new/reference; lower is better. Focused measurements contain 21 cases: six arithmetic operators, four noninteger controls, two changing-operand cases, and nine exactly preserved construction controls. These subgroups are reported separately. The new aggregate must not be confused with the earlier nine-case focused cohort. Full workload time excludes startup (23 fixtures), and process metrics include it (24 fixtures). Each focused row has seven paired samples; full rows have five; startup/import probes have 31. Host load qualified before each stage, and all later telemetry is retained.

## Comparison with phase 89 direct dynamic integer results

| Cohort / metric | New / reference | New / CPython | WeavePy wins vs. CPython | Reference regressions |
|---|---:|---:|---:|---:|
| all_23_workloads_excluding_startup / ns | 1.002109 | 3.503527 | 6 | 9 |
| all_processes_including_startup / wall_ns | 0.999915 | 3.165178 | 4 | 10 |
| all_processes_including_startup / cpu_ns | 1.001662 | 3.246356 | 4 | 11 |
| all_processes_including_startup / rss_bytes | 0.998471 | 2.017464 | 0 | 5 |
| historical_21_including_startup / ns | 1.002746 | 2.156880 | 6 | 8 |

The historical ratio-of-medians aggregation is 2.149837 times CPython. It must not be compared with the 23-workload aggregate as if the cohorts were identical.

| Focused case | Time / reference | RSS / reference | Time wins / 7 | RSS wins / 7 |
|---|---:|---:|---:|---:|
| cold/keyword_constructor | 0.996595 | 0.998340 | 4 | 4 |
| cold/keyword_default_constructor | 1.003579 | 0.997231 | 2 | 5 |
| cold/keyword_only_control | 0.994737 | 0.995580 | 4 | 7 |
| cold/keyword_deque | 1.006986 | 0.996502 | 3 | 6 |
| cold/keyword_dict_control | 1.014539 | 0.997879 | 2 | 5 |
| cold/metaclass_control | 0.999987 | 0.996667 | 4 | 6 |
| cold/positional_constructor_control | 0.994733 | 0.997225 | 4 | 6 |
| cold/keyword_gap_control | 1.003369 | 0.995006 | 3 | 5 |
| cold/keyword_error_control | 1.002442 | 0.997774 | 3 | 5 |
| cold/arithmetic_add | 0.635319 | 1.010597 | 7 | 0 |
| cold/arithmetic_sub | 0.636488 | 1.013385 | 7 | 0 |
| cold/arithmetic_mul | 0.622867 | 1.013333 | 7 | 0 |
| cold/arithmetic_div | 0.619338 | 1.012229 | 7 | 0 |
| cold/arithmetic_floordiv | 0.628076 | 1.018364 | 7 | 0 |
| cold/arithmetic_mod | 0.624012 | 1.013904 | 7 | 0 |
| cold/float_control | 1.012150 | 0.996657 | 2 | 5 |
| cold/bool_control | 1.007393 | 0.995513 | 2 | 7 |
| cold/bigint_control | 0.990139 | 0.996092 | 5 | 7 |
| cold/object_control | 0.999321 | 0.995528 | 4 | 6 |
| cold/changed_left | 1.124771 | 1.010609 | 0 | 0 |
| cold/changed_right | 1.140752 | 1.009492 | 0 | 0 |
| warm/keyword_constructor | 0.997713 | 0.996189 | 5 | 5 |
| warm/keyword_default_constructor | 0.960721 | 0.996185 | 5 | 6 |
| warm/keyword_only_control | 0.976018 | 0.998908 | 5 | 5 |
| warm/keyword_deque | 1.002793 | 1.000500 | 3 | 1 |
| warm/keyword_dict_control | 1.018052 | 0.997964 | 1 | 6 |
| warm/metaclass_control | 0.998984 | 0.994544 | 4 | 7 |
| warm/positional_constructor_control | 0.993979 | 0.997274 | 5 | 6 |
| warm/keyword_gap_control | 0.981724 | 0.995643 | 5 | 7 |
| warm/keyword_error_control | 1.003181 | 0.995626 | 3 | 6 |
| warm/arithmetic_add | 0.472042 | 1.018529 | 7 | 0 |
| warm/arithmetic_sub | 0.488344 | 1.017553 | 7 | 0 |
| warm/arithmetic_mul | 0.479094 | 1.019147 | 7 | 0 |
| warm/arithmetic_div | 0.475566 | 1.014803 | 7 | 0 |
| warm/arithmetic_floordiv | 0.479841 | 1.017014 | 7 | 0 |
| warm/arithmetic_mod | 0.486414 | 1.017573 | 7 | 0 |
| warm/float_control | 1.000494 | 0.996154 | 3 | 6 |
| warm/bool_control | 1.021389 | 0.998353 | 1 | 5 |
| warm/bigint_control | 1.021562 | 0.996701 | 2 | 5 |
| warm/object_control | 1.020895 | 0.992877 | 1 | 7 |
| warm/changed_left | 1.008814 | 1.010405 | 1 | 0 |
| warm/changed_right | 1.031357 | 1.013736 | 3 | 0 |

| Focused subgroup | Cases | Time / reference | RSS / reference | Time / CPython |
|---|---:|---:|---:|---:|
| cold/inherited | 9 | 1.001867 | 0.996911 | 12.940675 |
| cold/arithmetic | 6 | 0.627651 | 1.013633 | 3.960073 |
| cold/fallback | 4 | 1.002216 | 0.995947 | 9.869196 |
| cold/changed | 2 | 1.132733 | 1.010050 | 9.231593 |
| warm/inherited | 9 | 0.992442 | 0.996980 | 12.613850 |
| warm/arithmetic | 6 | 0.480183 | 1.017435 | 2.965244 |
| warm/fallback | 4 | 1.016045 | 0.996019 | 9.995102 |
| warm/changed | 2 | 1.020023 | 1.012069 | 7.957785 |

| Full fixture | Time / reference | Time / CPython | RSS / reference | RSS / CPython |
|---|---:|---:|---:|---:|
| fannkuch | 0.990636 | 9.583790 | 0.994404 | 1.917115 |
| nbody | 0.995780 | 8.844924 | 0.997253 | 1.917373 |
| fib | 1.004682 | 2.570547 | 0.997768 | 1.939262 |
| pidigits | 0.997193 | 0.897613 | 0.999473 | 1.940513 |
| pyaes | 0.986319 | 0.679853 | 0.996158 | 1.925769 |
| richards | 1.000150 | 8.937750 | 0.997772 | 1.925966 |
| sumvm | 0.999507 | 0.076235 | 0.998322 | 1.935205 |
| nested_loops | 1.094422 | 0.100645 | 1.000554 | 1.954545 |
| jitloop | 0.982503 | 0.074683 | 0.998891 | 1.941810 |
| jitkernels | 0.992990 | 0.883485 | 0.995044 | 1.924388 |
| deltablue | 1.004136 | 19.678555 | 1.006443 | 2.114341 |
| float_math | 0.982124 | 6.848596 | 0.998471 | 2.667574 |
| spectral_norm | 1.018882 | 2.304637 | 0.994527 | 1.949571 |
| json_bench | 0.994842 | 1.167107 | 1.001243 | 2.447686 |
| str_methods | 1.016154 | 1.994269 | 1.001015 | 2.127546 |
| dict_ops | 0.996472 | 5.523054 | 0.993868 | 1.912206 |
| list_ops | 0.977034 | 14.263261 | 0.998880 | 1.908602 |
| attr_access | 1.018228 | 2.720637 | 1.000000 | 1.984962 |
| call_overhead | 0.998158 | 8.088516 | 0.998377 | 1.982814 |
| generators | 1.020115 | 9.988574 | 0.997779 | 1.938511 |
| deque_ops | 0.991763 | 17.451215 | 0.998487 | 1.826870 |
| datetime_ops | 1.005972 | 123.503369 | 1.003027 | 2.128480 |
| pickle_bench | 0.986211 | 205.701410 | 0.999578 | 2.387940 |
| startup | 0.992960 | 1.359755 | 0.996065 | 1.925244 |

| Startup/import case | Wall / reference | CPU / reference | RSS / reference | Wall / CPython | RSS / CPython |
|---|---:|---:|---:|---:|---:|
| startup_matched | 0.995404 | 0.994092 | 0.992073 | 1.342638 | 1.771987 |
| startup_relocated | 1.003867 | 1.002792 | 0.992107 | 1.342534 | 1.773667 |
| no_site_matched | 1.023025 | 1.015884 | 0.996567 | 0.607456 | 1.526938 |
| imports_matched | 1.004741 | 1.004753 | 0.995692 | 2.690845 | 2.346193 |
| imports_relocated | 0.999232 | 1.000472 | 0.994841 | 2.694917 | 2.344828 |
| pickle_import_matched | 0.999193 | 0.997548 | 0.994626 | 2.106583 | 2.140609 |
| pickle_import_relocated | 0.998131 | 0.995828 | 0.995129 | 2.141401 | 2.144806 |
| accelerator_first_matched | 0.990364 | 0.989838 | 0.996587 | 2.096823 | 2.141807 |
| accelerator_first_relocated | 1.012265 | 1.009084 | 0.996098 | 2.092226 | 2.143908 |

## Comparison with phase 69 thin value storage

| Cohort / metric | New / reference | New / CPython | WeavePy wins vs. CPython | Reference regressions |
|---|---:|---:|---:|---:|
| all_23_workloads_excluding_startup / ns | 1.012135 | 3.471086 | 6 | 16 |
| all_processes_including_startup / wall_ns | 1.004028 | 3.131528 | 4 | 16 |
| all_processes_including_startup / cpu_ns | 1.003631 | 3.210339 | 4 | 16 |
| all_processes_including_startup / rss_bytes | 1.001954 | 2.011208 | 0 | 16 |
| historical_21_including_startup / ns | 1.012410 | 2.130290 | 6 | 14 |

The historical ratio-of-medians aggregation is 2.129193 times CPython. It must not be compared with the 23-workload aggregate as if the cohorts were identical.

| Focused case | Time / reference | RSS / reference | Time wins / 7 | RSS wins / 7 |
|---|---:|---:|---:|---:|
| cold/keyword_constructor | 1.076196 | 0.982007 | 0 | 7 |
| cold/keyword_default_constructor | 1.059756 | 0.984144 | 0 | 7 |
| cold/keyword_only_control | 0.906383 | 1.002231 | 7 | 2 |
| cold/keyword_deque | 1.070099 | 1.001508 | 0 | 0 |
| cold/keyword_dict_control | 1.007990 | 0.999573 | 3 | 4 |
| cold/metaclass_control | 0.810721 | 0.999443 | 7 | 4 |
| cold/positional_constructor_control | 0.857787 | 1.000556 | 7 | 2 |
| cold/keyword_gap_control | 0.800478 | 0.999444 | 7 | 4 |
| cold/keyword_error_control | 0.942750 | 1.003902 | 7 | 0 |
| cold/arithmetic_add | 0.636336 | 1.019674 | 7 | 0 |
| cold/arithmetic_sub | 0.642489 | 1.017386 | 7 | 0 |
| cold/arithmetic_mul | 0.643817 | 1.017877 | 7 | 0 |
| cold/arithmetic_div | 0.615583 | 1.016283 | 7 | 0 |
| cold/arithmetic_floordiv | 0.636153 | 1.020752 | 7 | 0 |
| cold/arithmetic_mod | 0.642376 | 1.019696 | 7 | 0 |
| cold/float_control | 1.017195 | 1.001123 | 1 | 2 |
| cold/bool_control | 1.024018 | 1.001682 | 2 | 3 |
| cold/bigint_control | 1.003995 | 1.006190 | 2 | 1 |
| cold/object_control | 1.013645 | 0.999439 | 1 | 4 |
| cold/changed_left | 1.127207 | 1.016210 | 0 | 0 |
| cold/changed_right | 1.140723 | 1.019718 | 0 | 0 |
| warm/keyword_constructor | 1.056094 | 0.984946 | 0 | 7 |
| warm/keyword_default_constructor | 1.046517 | 0.983342 | 0 | 7 |
| warm/keyword_only_control | 0.886834 | 1.000548 | 7 | 3 |
| warm/keyword_deque | 1.053093 | 1.004012 | 0 | 1 |
| warm/keyword_dict_control | 1.009008 | 0.999659 | 3 | 4 |
| warm/metaclass_control | 0.796319 | 1.003291 | 7 | 1 |
| warm/positional_constructor_control | 0.829790 | 0.998902 | 7 | 5 |
| warm/keyword_gap_control | 0.797028 | 1.000550 | 7 | 2 |
| warm/keyword_error_control | 0.932915 | 1.002743 | 6 | 1 |
| warm/arithmetic_add | 0.495853 | 1.020971 | 7 | 0 |
| warm/arithmetic_sub | 0.497881 | 1.026534 | 7 | 0 |
| warm/arithmetic_mul | 0.496068 | 1.022652 | 7 | 0 |
| warm/arithmetic_div | 0.503177 | 1.023639 | 7 | 0 |
| warm/arithmetic_floordiv | 0.490912 | 1.021476 | 7 | 0 |
| warm/arithmetic_mod | 0.476146 | 1.025910 | 7 | 0 |
| warm/float_control | 1.030733 | 1.000552 | 1 | 3 |
| warm/bool_control | 1.008789 | 1.002764 | 3 | 3 |
| warm/bigint_control | 1.001258 | 0.999449 | 2 | 4 |
| warm/object_control | 1.004056 | 0.998894 | 2 | 4 |
| warm/changed_left | 1.006663 | 1.012135 | 3 | 0 |
| warm/changed_right | 1.037385 | 1.016593 | 0 | 0 |

| Focused subgroup | Cases | Time / reference | RSS / reference | Time / CPython |
|---|---:|---:|---:|---:|
| cold/inherited | 9 | 0.942199 | 0.996950 | 13.007639 |
| cold/arithmetic | 6 | 0.636051 | 1.018610 | 3.916521 |
| cold/fallback | 4 | 1.014688 | 1.002105 | 9.961980 |
| cold/changed | 2 | 1.133945 | 1.017963 | 9.080554 |
| warm/inherited | 9 | 0.928259 | 0.997528 | 12.613889 |
| warm/arithmetic | 6 | 0.493265 | 1.023528 | 2.968669 |
| warm/fallback | 4 | 1.011143 | 1.000414 | 9.862149 |
| warm/changed | 2 | 1.021908 | 1.014361 | 7.976866 |

| Full fixture | Time / reference | Time / CPython | RSS / reference | RSS / CPython |
|---|---:|---:|---:|---:|
| fannkuch | 1.002138 | 9.189955 | 1.001697 | 1.910560 |
| nbody | 1.002889 | 8.904484 | 0.998897 | 1.919492 |
| fib | 0.818959 | 2.547588 | 0.998878 | 1.930661 |
| pidigits | 0.991176 | 0.890945 | 1.001609 | 1.922998 |
| pyaes | 1.024310 | 0.679026 | 1.001103 | 1.920635 |
| richards | 1.020731 | 9.129879 | 1.007320 | 1.933118 |
| sumvm | 1.280549 | 0.076161 | 1.001680 | 1.938178 |
| nested_loops | 1.084209 | 0.100746 | 1.006149 | 1.938578 |
| jitloop | 1.020126 | 0.076065 | 0.997779 | 1.940476 |
| jitkernels | 1.015314 | 0.861779 | 0.999445 | 1.910828 |
| deltablue | 1.013819 | 20.213484 | 1.000923 | 2.087838 |
| float_math | 0.995720 | 6.714824 | 1.000000 | 2.667877 |
| spectral_norm | 1.026961 | 2.183728 | 1.004415 | 1.943194 |
| json_bench | 1.039495 | 1.164444 | 1.010408 | 2.452069 |
| str_methods | 1.022673 | 2.084363 | 1.006098 | 2.115756 |
| dict_ops | 1.004594 | 5.544566 | 1.004507 | 1.905882 |
| list_ops | 0.998437 | 14.007294 | 0.997745 | 1.905579 |
| attr_access | 0.971420 | 2.406800 | 1.000545 | 1.976344 |
| call_overhead | 0.997120 | 7.981686 | 0.998917 | 1.975322 |
| generators | 0.982102 | 9.690657 | 0.998331 | 1.923901 |
| deque_ops | 1.007803 | 17.592435 | 1.001520 | 1.816678 |
| datetime_ops | 1.014993 | 123.571642 | 1.005536 | 2.134615 |
| pickle_bench | 1.000248 | 205.867543 | 1.001272 | 2.379798 |
| startup | 1.004499 | 1.310217 | 1.002260 | 1.927095 |

| Startup/import case | Wall / reference | CPU / reference | RSS / reference | Wall / CPython | RSS / CPython |
|---|---:|---:|---:|---:|---:|
| startup_matched | 0.998642 | 1.004051 | 0.996946 | 1.323873 | 1.777899 |
| startup_relocated | 1.001281 | 1.001387 | 0.996946 | 1.310683 | 1.775843 |
| no_site_matched | 1.018317 | 1.036132 | 1.001730 | 0.616447 | 1.528947 |
| imports_matched | 1.006748 | 1.008803 | 1.000432 | 2.666674 | 2.344478 |
| imports_relocated | 1.000038 | 1.000876 | 0.999569 | 2.619294 | 2.347870 |
| pickle_import_matched | 0.992179 | 0.991710 | 0.997077 | 2.044641 | 2.139706 |
| pickle_import_relocated | 0.994028 | 0.994597 | 0.996589 | 2.055973 | 2.146905 |
| accelerator_first_matched | 1.002896 | 1.002971 | 0.999023 | 2.063256 | 2.146751 |
| accelerator_first_relocated | 1.000543 | 1.000172 | 0.997557 | 2.051049 | 2.146597 |

## Interpretation and evidence

The retained comparison includes all intervening experiments and does not isolate this analyzer change. The arithmetic subgroup improves about 37% cold and 52% warm against phase 89, with roughly 1-2% higher peak RSS. Changing-operand cases regress about 13% cold; all individual losses are retained above. No tested census process beats CPython on peak RSS. The full-census and universal performance targets remain unmet.

Complete source snapshots, both source/test versions, expected baseline rejection, native and release checks, unchanged inherited workloads, all new arithmetic/control sources, raw samples, startup/import samples, complete native traces, codegen, stage telemetry, and reporting methods are preserved in the accompanying archives. Executable binaries and regenerable build/stdlib/frozen caches are excluded.

These measurements do not establish universal superiority or results for energy, scaling, portability, or controlled build time. Further work should address the large library and JIT-coverage gaps while checking regressions across the same cohorts.

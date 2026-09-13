# Direct dynamic integer results

This validated experiment removes temporary-integer pin pressure but does not establish an overall speed or peak-memory improvement. The primary CLI and source checkout were not replaced. All four planned comparisons completed; no sample was discarded or selectively retried.

At an integer-result dynamic call site, the native relay now requests the existing exact-integer return lane after the original callee eligibility checks. This avoids pinning an object-tagged integer and immediately unboxing it. Generic result sites, callee guards, completed-result parking, exceptions, and exactly-once callbacks remain covered.

The baseline 20,000-operation regression produced the correct 199990000 checksum and four pin-pressure exits; the candidate produces the same checksum and zero exits. Both full constructor probes changed from eight to zero pressure exits while preserving CPython checksums and over 40,000 native calls each. Their regular deopt counts remain 80, so this is not a claim that all exits disappeared.

Validation passed: six focused dynamic-return tests, 74 JIT tests, 363 VM tests, 156 C API tests, formatting, strict all-target clippy, no-JIT build checks, 99 targeted runtime checks, 44 fixture paths, 275 compatibility checks, 16 keyword semantic cases, 14 local-state cases, 34 indexing cases, construction/indexing/format controls, and all 24 authoritative census results. The preexisting staticmethod class-subscription and frame-identity differences remain explicitly recorded.

The release binary is 44,098,272 bytes, unchanged from the immediate candidate. Captured ARM64 prologue reservations are also unchanged. The observed 5m 08s release build is not a controlled build-performance measurement.

All ratios below are new/reference; lower is better. Full workload time excludes startup (23 fixtures); process metrics include it (24 fixtures). The historical 21-fixture cohort is shown separately. Each focused row has seven paired samples; full rows have five; startup/import probes have 31. Host load qualified before each stage, and all later load telemetry remains preserved.

## Comparison with phase 88 dynamic object returns

| Cohort / metric | New / reference | New / CPython | WeavePy wins vs. CPython | Reference regressions |
|---|---:|---:|---:|---:|
| all_23_workloads_excluding_startup / ns | 1.002292 | 3.439154 | 6 | 15 |
| all_processes_including_startup / wall_ns | 1.007537 | 3.178904 | 4 | 21 |
| all_processes_including_startup / cpu_ns | 1.007716 | 3.256455 | 4 | 19 |
| all_processes_including_startup / rss_bytes | 0.999758 | 2.020455 | 0 | 11 |
| historical_21_including_startup / ns | 0.999597 | 2.103845 | 6 | 13 |

The historical ratio-of-medians aggregation is 2.103575 times CPython. It must not be compared with the 23-workload aggregate as if the cohorts were identical.

| Focused case | Time / reference | RSS / reference | Time wins / 7 | RSS wins / 7 |
|---|---:|---:|---:|---:|
| cold/keyword_constructor | 0.992962 | 0.996135 | 6 | 7 |
| cold/keyword_default_constructor | 0.998265 | 0.993936 | 4 | 6 |
| cold/keyword_only_control | 0.995533 | 0.995575 | 4 | 7 |
| cold/keyword_deque | 1.005337 | 0.998997 | 1 | 5 |
| cold/keyword_dict_control | 1.011634 | 1.000425 | 3 | 3 |
| cold/metaclass_control | 0.995572 | 0.994463 | 6 | 5 |
| cold/positional_constructor_control | 0.999556 | 0.997788 | 4 | 6 |
| cold/keyword_gap_control | 0.992208 | 0.995025 | 6 | 7 |
| cold/keyword_error_control | 0.989898 | 0.998894 | 6 | 6 |
| warm/keyword_constructor | 0.999359 | 0.995140 | 4 | 7 |
| warm/keyword_default_constructor | 1.009115 | 0.997290 | 3 | 6 |
| warm/keyword_only_control | 0.976515 | 0.994592 | 6 | 7 |
| warm/keyword_deque | 1.012689 | 1.001490 | 2 | 2 |
| warm/keyword_dict_control | 0.974921 | 1.001020 | 6 | 3 |
| warm/metaclass_control | 0.977384 | 0.996732 | 4 | 6 |
| warm/positional_constructor_control | 0.992496 | 0.997289 | 4 | 6 |
| warm/keyword_gap_control | 0.986519 | 0.996208 | 4 | 7 |
| warm/keyword_error_control | 0.992105 | 0.994035 | 7 | 7 |

| Full fixture | Time / reference | Time / CPython | RSS / reference | RSS / CPython |
|---|---:|---:|---:|---:|
| fannkuch | 1.020034 | 9.302114 | 0.997763 | 1.928803 |
| nbody | 1.009405 | 8.808357 | 0.998354 | 1.917548 |
| fib | 0.992103 | 2.452271 | 1.002225 | 1.948969 |
| pidigits | 1.000839 | 0.896855 | 0.994235 | 1.953656 |
| pyaes | 0.996574 | 0.674717 | 1.000551 | 1.935381 |
| richards | 0.997468 | 8.381455 | 0.998331 | 1.923820 |
| sumvm | 0.981641 | 0.074551 | 1.002787 | 1.925054 |
| nested_loops | 1.001740 | 0.089408 | 1.001109 | 1.953362 |
| jitloop | 1.001088 | 0.074929 | 1.000554 | 1.949134 |
| jitkernels | 1.012977 | 0.852917 | 1.002217 | 1.936898 |
| deltablue | 0.998940 | 19.913660 | 1.004136 | 2.113153 |
| float_math | 1.011063 | 6.726587 | 0.999660 | 2.669841 |
| spectral_norm | 0.984419 | 2.104202 | 0.997811 | 1.945571 |
| json_bench | 1.006106 | 1.144144 | 0.996709 | 2.448589 |
| str_methods | 1.000365 | 2.059434 | 1.005081 | 2.126882 |
| dict_ops | 0.979455 | 5.455069 | 1.003352 | 1.920771 |
| list_ops | 0.989447 | 14.295014 | 1.001681 | 1.921674 |
| attr_access | 1.002086 | 2.662616 | 0.998923 | 1.986022 |
| call_overhead | 1.000756 | 7.756849 | 1.002167 | 1.984979 |
| generators | 1.003065 | 9.623614 | 1.000000 | 1.937634 |
| deque_ops | 1.002511 | 17.631350 | 0.998106 | 1.825637 |
| datetime_ops | 1.005025 | 127.032497 | 0.993991 | 2.124330 |
| pickle_bench | 1.058211 | 217.778677 | 0.994547 | 2.380522 |
| startup | 1.002979 | 1.358675 | 1.000000 | 1.933696 |

| Startup/import case | Wall / reference | CPU / reference | RSS / reference | Wall / CPython | RSS / CPython |
|---|---:|---:|---:|---:|---:|
| startup_matched | 1.003338 | 1.002628 | 1.001224 | 1.334492 | 1.785870 |
| startup_relocated | 0.999824 | 0.998891 | 1.000000 | 1.331363 | 1.784783 |
| no_site_matched | 1.017706 | 1.020132 | 0.998282 | 0.604012 | 1.531662 |
| imports_matched | 1.001526 | 1.001415 | 1.000859 | 2.712897 | 2.355623 |
| imports_relocated | 0.997951 | 0.997492 | 1.003011 | 2.706056 | 2.355556 |
| pickle_import_matched | 0.997999 | 0.996945 | 0.998052 | 2.135729 | 2.151102 |
| pickle_import_relocated | 0.995879 | 0.994946 | 0.998056 | 2.148705 | 2.153684 |
| accelerator_first_matched | 1.001970 | 1.001509 | 0.997561 | 2.133819 | 2.149003 |
| accelerator_first_relocated | 1.000548 | 0.998242 | 0.998539 | 2.141305 | 2.154250 |

## Comparison with phase 69 thin value storage

| Cohort / metric | New / reference | New / CPython | WeavePy wins vs. CPython | Reference regressions |
|---|---:|---:|---:|---:|
| all_23_workloads_excluding_startup / ns | 1.009040 | 3.473152 | 6 | 15 |
| all_processes_including_startup / wall_ns | 0.997458 | 3.148832 | 4 | 13 |
| all_processes_including_startup / cpu_ns | 0.998990 | 3.221221 | 4 | 15 |
| all_processes_including_startup / rss_bytes | 1.005066 | 2.017985 | 0 | 21 |
| historical_21_including_startup / ns | 1.006020 | 2.137305 | 6 | 13 |

The historical ratio-of-medians aggregation is 2.149363 times CPython. It must not be compared with the 23-workload aggregate as if the cohorts were identical.

| Focused case | Time / reference | RSS / reference | Time wins / 7 | RSS wins / 7 |
|---|---:|---:|---:|---:|
| cold/keyword_constructor | 1.075270 | 0.985808 | 0 | 7 |
| cold/keyword_default_constructor | 1.059259 | 0.987452 | 0 | 7 |
| cold/keyword_only_control | 0.906574 | 1.006711 | 7 | 0 |
| cold/keyword_deque | 1.066284 | 1.005547 | 0 | 0 |
| cold/keyword_dict_control | 0.998961 | 1.002134 | 4 | 0 |
| cold/metaclass_control | 0.808036 | 1.006149 | 7 | 0 |
| cold/positional_constructor_control | 0.855346 | 1.007238 | 7 | 0 |
| cold/keyword_gap_control | 0.806230 | 1.003361 | 7 | 1 |
| cold/keyword_error_control | 0.954774 | 1.006149 | 7 | 0 |
| warm/keyword_constructor | 1.035633 | 0.984417 | 1 | 7 |
| warm/keyword_default_constructor | 1.066189 | 0.987138 | 0 | 7 |
| warm/keyword_only_control | 0.905132 | 1.003293 | 7 | 1 |
| warm/keyword_deque | 1.042752 | 1.005489 | 0 | 0 |
| warm/keyword_dict_control | 0.987603 | 1.003753 | 5 | 0 |
| warm/metaclass_control | 0.798252 | 1.006054 | 7 | 0 |
| warm/positional_constructor_control | 0.799765 | 1.008791 | 7 | 0 |
| warm/keyword_gap_control | 0.744332 | 1.004948 | 7 | 0 |
| warm/keyword_error_control | 0.924407 | 1.007139 | 7 | 0 |

| Full fixture | Time / reference | Time / CPython | RSS / reference | RSS / CPython |
|---|---:|---:|---:|---:|
| fannkuch | 1.007445 | 9.356766 | 1.003386 | 1.911828 |
| nbody | 0.996124 | 8.857933 | 1.005549 | 1.922669 |
| fib | 0.792980 | 2.502429 | 1.004482 | 1.939590 |
| pidigits | 0.993495 | 0.901360 | 1.014957 | 1.945473 |
| pyaes | 1.028907 | 0.676279 | 1.005528 | 1.930085 |
| richards | 1.001309 | 8.788759 | 1.008974 | 1.934267 |
| sumvm | 1.270072 | 0.073993 | 1.006169 | 1.934196 |
| nested_loops | 1.166118 | 0.101325 | 1.006152 | 1.952381 |
| jitloop | 1.035483 | 0.074208 | 1.003895 | 1.951193 |
| jitkernels | 1.013640 | 0.865481 | 1.006118 | 1.926518 |
| deltablue | 1.031808 | 20.313750 | 1.003233 | 2.085577 |
| float_math | 0.929782 | 6.403403 | 0.998473 | 2.670449 |
| spectral_norm | 1.030524 | 2.200023 | 1.007735 | 1.952840 |
| json_bench | 1.017013 | 1.126937 | 1.002467 | 2.443883 |
| str_methods | 0.984991 | 2.158098 | 1.006626 | 2.130529 |
| dict_ops | 1.001929 | 5.438511 | 1.009561 | 1.918630 |
| list_ops | 1.016479 | 14.716789 | 1.006768 | 1.921421 |
| attr_access | 0.942871 | 2.675093 | 1.005456 | 1.979657 |
| call_overhead | 0.989908 | 7.943774 | 0.998380 | 1.984946 |
| generators | 1.000524 | 9.844571 | 1.002784 | 1.941810 |
| deque_ops | 0.997694 | 17.251677 | 1.002280 | 1.825276 |
| datetime_ops | 1.012577 | 123.249022 | 0.994972 | 2.123126 |
| pickle_bench | 1.020825 | 206.154354 | 1.009338 | 2.378000 |
| startup | 0.951098 | 1.356309 | 1.008494 | 1.927489 |

| Startup/import case | Wall / reference | CPU / reference | RSS / reference | Wall / CPython | RSS / CPython |
|---|---:|---:|---:|---:|---:|
| startup_matched | 1.004311 | 1.004427 | 1.003054 | 1.321273 | 1.784079 |
| startup_relocated | 1.000676 | 0.999190 | 1.003067 | 1.318871 | 1.785637 |
| no_site_matched | 1.019275 | 1.011915 | 1.004325 | 0.611124 | 1.532982 |
| imports_matched | 1.000959 | 1.001672 | 1.004756 | 2.644615 | 2.355915 |
| imports_relocated | 1.000150 | 1.004112 | 1.005177 | 2.673432 | 2.359026 |
| pickle_import_matched | 1.009937 | 1.009066 | 1.002445 | 2.136496 | 2.156513 |
| pickle_import_relocated | 1.000238 | 1.002481 | 1.000977 | 2.109413 | 2.154088 |
| accelerator_first_matched | 1.001955 | 1.003059 | 1.000977 | 2.107939 | 2.150368 |
| accelerator_first_relocated | 1.000716 | 1.004595 | 1.000975 | 2.101651 | 2.153361 |

## Interpretation and evidence

The retained comparison includes all intervening experiments and does not isolate the latest eight-line production change. Constructor-specific rows still regress against that retained checkpoint, even where the focused aggregate improves. No tested census process beats CPython on peak RSS. The optimization target remains unmet.

Complete source snapshots, both source/test versions, expected baseline failure, initial disk-space failure, native and release checks, cache-cleanup inventories, unchanged workload sources, raw samples, startup/import samples, complete native traces, codegen, stage telemetry, and reporting methods are preserved in the accompanying archives. Executable binaries and regenerable build/stdlib/frozen caches are excluded.

These measurements do not establish universal superiority or results for energy, scaling, portability, or controlled build time. Further work should address the large library and JIT-coverage gaps while checking regressions across the same cohorts.

# Datetime slot initialization

Ordinary construction gains about 1-2% and retained-object peak RSS falls about 2.4%, but custom-metaclass fallback slows about 6-7%. Full-suite workload time and RSS show no overall gain. Keep the primary phase-79 source and CLI unchanged while testing lower guard overhead.

Candidate SHA-256: 1266e0f1a67246c9d15c380bccc29620bd17467ca6bdc8d8d8807562f3399f66. CLI size: 44,119,120 bytes, +4,240 bytes versus phase 94.

The implementation preserves Python validation and object.__new__(cls), then initializes an empty ordinary instance in its final slot-storage representation. Native/C bodies, custom metaclasses/setters/descriptors, mismatched or read-only slots, and populated slots decline before mutation. Original stores handle fallback. Fixed names and values preserve insertion order and aliases. Four source files differ from phase 94.

Validation: 79 JIT tests, 376 VM tests, 156 JIT-enabled C API tests, strict lint/format checks, and a check with JIT disabled passed. Release validation passed 99 targeted executions, 44 coverage paths, 275 compatibility checks, all 24 authoritative census results, and six-engine checks for 12 constructor cases, including actual cold timer contracts. Test-only counters confirmed at least 2,000 successful initializations for each constructor kind.

Two new Rust test compile errors and the original public fold-index failure are preserved in source versions 01-03. Version 04 corrects the tests and uses integer fold in the public parity fixture after confirming the index-object difference also exists in phase 94. Production validation was not changed. The initial oracle filename shadowed datetime; the preserved corrected oracle filenames avoid that collision.

All four serial stages passed the unchanged host-load gate and completed. No measured sample was dropped or replaced. Each focused row has seven paired samples, each census row has five, and each startup row has 31. Ratios are candidate/comparator, lower is better. Per-case ratios are medians of paired ratios; group aggregates are geometric means across cases. Small differences are observations, not proof of a repeatable gain.

## Comparison with phase 94

### Cold constructor groups

| Group | Time / baseline | RSS / baseline | Time / CPython | RSS / CPython |
| --- | ---: | ---: | ---: | ---: |
| construct | 0.984271 | 0.998072 | 18.479955 | 2.249768 |
| retained | 0.987812 | 0.977238 | 18.351834 | 2.758408 |
| fallback | 1.061167 | 1.004291 | 20.445390 | 2.266272 |

| Case | Time / baseline | Wall / baseline | CPU / baseline | RSS / baseline | Time / CPython | RSS / CPython |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| construct_date | 1.014762 | 1.012102 | 1.009769 | 1.007890 | 34.018343 | 2.206934 |
| retained_date | 1.019977 | 1.014156 | 1.011239 | 1.003157 | 32.992707 | 2.580547 |
| fallback_date | 1.070498 | 1.047245 | 1.045859 | 1.002950 | 36.789133 | 2.192017 |
| construct_time | 1.014464 | 1.006601 | 1.006666 | 1.002425 | 19.522004 | 2.229773 |
| retained_time | 0.997625 | 1.001515 | 1.004307 | 0.988501 | 19.909891 | 2.704985 |
| fallback_time | 1.060655 | 1.041819 | 1.042762 | 1.002412 | 21.337910 | 2.238147 |
| construct_datetime | 0.962405 | 0.971021 | 0.970764 | 0.979775 | 23.209229 | 2.344456 |
| retained_datetime | 0.969098 | 0.980740 | 0.979829 | 0.930466 | 23.322103 | 3.138308 |
| fallback_datetime | 1.051584 | 1.040576 | 1.039852 | 1.004054 | 25.683203 | 2.406048 |
| construct_timedelta | 0.947326 | 0.966016 | 0.966281 | 1.002435 | 7.566680 | 2.220541 |
| retained_timedelta | 0.965542 | 0.976887 | 0.977391 | 0.988450 | 7.403949 | 2.642786 |
| fallback_timedelta | 1.062014 | 1.036336 | 1.036658 | 1.007756 | 8.666844 | 2.234661 |

### Warm constructor groups

| Group | Time / baseline | RSS / baseline | Time / CPython | RSS / CPython |
| --- | ---: | ---: | ---: | ---: |
| construct | 0.987587 | 0.997088 | 18.343040 | 2.202048 |
| retained | 0.991718 | 0.976176 | 18.226103 | 2.690493 |
| fallback | 1.066941 | 1.004293 | 19.134514 | 2.218362 |

| Case | Time / baseline | Wall / baseline | CPU / baseline | RSS / baseline | Time / CPython | RSS / CPython |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| construct_date | 1.025066 | 1.018984 | 1.018247 | 1.004390 | 35.621586 | 2.153361 |
| retained_date | 1.030856 | 1.020397 | 1.019276 | 1.002732 | 34.452152 | 2.524510 |
| fallback_date | 1.077421 | 1.062531 | 1.063559 | 1.006357 | 35.628082 | 2.154088 |
| construct_time | 0.999128 | 0.999771 | 0.999956 | 1.001435 | 20.086268 | 2.182292 |
| retained_time | 1.008341 | 1.001058 | 1.000923 | 0.987505 | 19.833971 | 2.633595 |
| fallback_time | 1.065828 | 1.057097 | 1.056980 | 1.002391 | 19.992795 | 2.191223 |
| construct_datetime | 0.973619 | 0.975135 | 0.975101 | 0.981258 | 23.186292 | 2.302301 |
| retained_datetime | 0.963534 | 0.967139 | 0.966649 | 0.928300 | 22.987202 | 3.055019 |
| fallback_datetime | 1.065350 | 1.053755 | 1.052680 | 1.002673 | 23.763214 | 2.346597 |
| construct_timedelta | 0.953980 | 0.968282 | 0.968662 | 1.001443 | 6.824038 | 2.173278 |
| retained_timedelta | 0.965787 | 0.973745 | 0.972837 | 0.987872 | 7.025271 | 2.579808 |
| fallback_timedelta | 1.059248 | 1.046026 | 1.046641 | 1.005758 | 7.919515 | 2.186458 |

### Full-suite cohorts

| Cohort | Metric | / baseline | / CPython | WeavePy wins vs. CPython | Baseline regressions |
| --- | --- | ---: | ---: | ---: | ---: |
| historical_21_including_startup | ns | 1.002622 | 2.114167 | 6 | 16 |
| all_23_workloads_excluding_startup | ns | 1.002289 | 3.433032 | 6 | 17 |
| all_processes_including_startup | wall_ns | 1.002879 | 3.151081 | 4 | 18 |
| all_processes_including_startup | cpu_ns | 1.002275 | 3.232073 | 4 | 16 |
| all_processes_including_startup | rss_bytes | 1.002001 | 2.022067 | 0 | 19 |

The historical cohort contains 21 fixtures including startup. The 23-workload cohort excludes startup, and the process cohort contains all 24 fixtures. Cohorts must not be compared interchangeably.

| Fixture | Time / baseline | Wall / baseline | CPU / baseline | RSS / baseline | Time / CPython | RSS / CPython |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| fannkuch | 1.000373 | 0.994278 | 0.994883 | 1.005624 | 9.308602 | 1.908218 |
| nbody | 0.997642 | 0.998491 | 0.999085 | 1.006071 | 8.849106 | 1.934043 |
| fib | 0.996554 | 1.003075 | 1.001464 | 1.000000 | 2.342911 | 1.943784 |
| pidigits | 1.004022 | 1.004060 | 1.003824 | 0.995783 | 0.900670 | 1.943416 |
| pyaes | 1.005094 | 1.002980 | 1.004537 | 0.999450 | 0.638948 | 1.937234 |
| richards | 1.000241 | 1.002551 | 1.002585 | 1.005022 | 8.785878 | 1.943905 |
| sumvm | 1.001945 | 1.008641 | 1.008226 | 1.001673 | 0.074200 | 1.933118 |
| nested_loops | 1.002682 | 1.005727 | 0.999149 | 1.004437 | 0.100599 | 1.956710 |
| jitloop | 1.012202 | 1.009372 | 1.009111 | 1.002216 | 0.076338 | 1.955628 |
| jitkernels | 1.006337 | 1.008415 | 1.009997 | 1.001104 | 0.861279 | 1.932692 |
| deltablue | 1.006273 | 1.006296 | 1.006236 | 0.995858 | 19.439693 | 2.085659 |
| float_math | 1.011856 | 1.012163 | 1.011364 | 1.001698 | 6.660334 | 2.674376 |
| spectral_norm | 1.005137 | 1.006701 | 1.005195 | 1.004372 | 2.128942 | 1.961456 |
| json_bench | 1.001490 | 1.004030 | 1.003001 | 1.003717 | 1.144098 | 2.451613 |
| str_methods | 0.990729 | 0.992295 | 0.993050 | 1.000000 | 2.054522 | 2.131607 |
| dict_ops | 1.003347 | 1.004552 | 1.004060 | 1.000557 | 5.576201 | 1.919872 |
| list_ops | 1.004726 | 1.003781 | 0.998980 | 1.004482 | 14.026944 | 1.919528 |
| attr_access | 1.004776 | 0.999835 | 0.998663 | 1.002706 | 2.577291 | 1.992473 |
| call_overhead | 1.005337 | 1.006284 | 1.005258 | 1.004331 | 7.869311 | 1.988172 |
| generators | 0.997265 | 1.000213 | 1.000215 | 1.000554 | 9.850611 | 1.941935 |
| deque_ops | 0.998549 | 1.000492 | 1.001018 | 1.002663 | 17.494614 | 1.821748 |
| datetime_ops | 0.992347 | 0.992572 | 0.992416 | 1.001004 | 119.411380 | 2.137192 |
| pickle_bench | 1.004016 | 1.005326 | 1.005104 | 1.002545 | 206.546121 | 2.378270 |
| startup | 0.997283 | 0.997283 | 0.997462 | 1.002247 | 1.387725 | 1.937976 |

### Startup and imports

| Case | Wall / baseline | CPU / baseline | RSS / baseline | Wall / CPython | CPU / CPython | RSS / CPython |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| startup_matched | 0.997869 | 0.998096 | 1.001838 | 1.338873 | 1.391371 | 1.784079 |
| startup_relocated | 1.009006 | 1.006598 | 1.003062 | 1.345787 | 1.399111 | 1.783460 |
| no_site_matched | 0.999973 | 0.998872 | 1.004296 | 0.615084 | 0.588716 | 1.540789 |
| imports_matched | 0.995914 | 0.996610 | 1.000431 | 2.690341 | 2.883886 | 2.346154 |
| imports_relocated | 0.998346 | 1.000642 | 1.000000 | 2.715759 | 2.892525 | 2.348178 |
| pickle_import_matched | 1.002872 | 1.000572 | 1.003920 | 2.151732 | 2.289639 | 2.157398 |
| pickle_import_relocated | 0.996787 | 0.998245 | 1.004878 | 2.138523 | 2.281342 | 2.161222 |
| accelerator_first_matched | 1.000074 | 0.999185 | 1.004900 | 2.125990 | 2.254266 | 2.155299 |
| accelerator_first_relocated | 0.997666 | 0.996773 | 1.006345 | 2.131971 | 2.262376 | 2.159329 |

## Comparison with phase 69

### Cold constructor groups

| Group | Time / baseline | RSS / baseline | Time / CPython | RSS / CPython |
| --- | ---: | ---: | ---: | ---: |
| construct | 0.993466 | 0.999683 | 18.594931 | 2.249266 |
| retained | 0.995455 | 0.980747 | 18.504618 | 2.756529 |
| fallback | 1.071212 | 1.007338 | 20.679374 | 2.267222 |

| Case | Time / baseline | Wall / baseline | CPU / baseline | RSS / baseline | Time / CPython | RSS / CPython |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| construct_date | 1.027493 | 1.021369 | 1.025313 | 1.006920 | 33.177370 | 2.198919 |
| retained_date | 1.030843 | 1.021409 | 1.023052 | 1.008300 | 34.038115 | 2.583927 |
| fallback_date | 1.075399 | 1.051005 | 1.051505 | 1.008420 | 37.855147 | 2.199353 |
| construct_time | 1.010857 | 1.010623 | 1.009692 | 1.004380 | 20.197437 | 2.226537 |
| retained_time | 1.008828 | 1.007683 | 1.008776 | 0.993673 | 19.621795 | 2.703553 |
| fallback_time | 1.080955 | 1.059329 | 1.060279 | 1.006780 | 22.049319 | 2.239482 |
| construct_datetime | 0.978766 | 0.978707 | 0.978270 | 0.984199 | 23.566249 | 2.347357 |
| retained_datetime | 0.975522 | 0.985358 | 0.984522 | 0.932093 | 23.642774 | 3.131944 |
| fallback_datetime | 1.051866 | 1.041082 | 1.042705 | 1.005864 | 25.421386 | 2.400000 |
| construct_timedelta | 0.958218 | 0.978025 | 0.976859 | 1.003398 | 7.570937 | 2.227126 |
| retained_timedelta | 0.967921 | 0.983814 | 0.984183 | 0.990682 | 7.425365 | 2.638889 |
| fallback_timedelta | 1.076871 | 1.050843 | 1.054204 | 1.008293 | 8.618471 | 2.235231 |

### Warm constructor groups

| Group | Time / baseline | RSS / baseline | Time / CPython | RSS / CPython |
| --- | ---: | ---: | ---: | ---: |
| construct | 0.985400 | 0.997696 | 18.385632 | 2.200199 |
| retained | 0.985318 | 0.977499 | 18.102178 | 2.688064 |
| fallback | 1.067911 | 1.004885 | 19.010749 | 2.218581 |

| Case | Time / baseline | Wall / baseline | CPU / baseline | RSS / baseline | Time / CPython | RSS / CPython |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| construct_date | 1.015283 | 1.016243 | 1.015596 | 1.004876 | 35.741676 | 2.151357 |
| retained_date | 1.013571 | 1.018619 | 1.018306 | 1.001568 | 33.950064 | 2.514230 |
| fallback_date | 1.074649 | 1.059779 | 1.061509 | 1.004880 | 34.971895 | 2.152560 |
| construct_time | 0.996219 | 0.999450 | 0.999576 | 1.003837 | 20.653427 | 2.183716 |
| retained_time | 1.002518 | 1.002478 | 1.004658 | 0.992271 | 20.070228 | 2.639489 |
| fallback_time | 1.068073 | 1.069625 | 1.069225 | 1.005769 | 19.593958 | 2.192268 |
| construct_datetime | 0.971920 | 0.979826 | 0.978998 | 0.981292 | 22.689984 | 2.295407 |
| retained_datetime | 0.965617 | 0.979306 | 0.978505 | 0.930670 | 22.895133 | 3.051774 |
| fallback_datetime | 1.059723 | 1.049822 | 1.049477 | 1.003121 | 24.095990 | 2.352142 |
| construct_timedelta | 0.959131 | 0.969385 | 0.968257 | 1.000965 | 6.822021 | 2.173097 |
| retained_timedelta | 0.960628 | 0.972210 | 0.972025 | 0.987094 | 6.883150 | 2.577990 |
| fallback_timedelta | 1.069254 | 1.055465 | 1.055554 | 1.005772 | 7.910625 | 2.182672 |

### Full-suite cohorts

| Cohort | Metric | / baseline | / CPython | WeavePy wins vs. CPython | Baseline regressions |
| --- | --- | ---: | ---: | ---: | ---: |
| historical_21_including_startup | ns | 1.006035 | 2.105536 | 6 | 11 |
| all_23_workloads_excluding_startup | ns | 1.006436 | 3.425415 | 6 | 13 |
| all_processes_including_startup | wall_ns | 0.998569 | 3.151903 | 4 | 14 |
| all_processes_including_startup | cpu_ns | 0.997917 | 3.229414 | 4 | 14 |
| all_processes_including_startup | rss_bytes | 1.007831 | 2.022824 | 0 | 24 |

The historical cohort contains 21 fixtures including startup. The 23-workload cohort excludes startup, and the process cohort contains all 24 fixtures. Cohorts must not be compared interchangeably.

| Fixture | Time / baseline | Wall / baseline | CPU / baseline | RSS / baseline | Time / CPython | RSS / CPython |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| fannkuch | 1.007510 | 1.013387 | 1.007964 | 1.009050 | 9.147705 | 1.929881 |
| nbody | 0.995640 | 0.997740 | 0.998355 | 1.008884 | 8.718663 | 1.922751 |
| fib | 0.813760 | 0.884635 | 0.879098 | 1.008964 | 2.385566 | 1.946970 |
| pidigits | 0.996179 | 0.996490 | 0.996435 | 1.010221 | 0.894001 | 1.941116 |
| pyaes | 0.980241 | 0.997002 | 0.998593 | 1.006084 | 0.626163 | 1.933050 |
| richards | 1.021653 | 1.019552 | 1.019721 | 1.008989 | 8.780458 | 1.933118 |
| sumvm | 1.325102 | 1.020770 | 1.020566 | 1.006159 | 0.073744 | 1.942703 |
| nested_loops | 1.056996 | 1.014326 | 1.008959 | 1.011732 | 0.100664 | 1.958874 |
| jitloop | 1.022080 | 0.999058 | 0.996872 | 1.010045 | 0.075633 | 1.952535 |
| jitkernels | 0.999246 | 1.009069 | 1.008415 | 1.006652 | 0.858999 | 1.926596 |
| deltablue | 1.011100 | 1.011548 | 1.011424 | 1.005074 | 19.802052 | 2.107454 |
| float_math | 0.989230 | 0.990696 | 0.990721 | 1.000681 | 6.594261 | 2.667726 |
| spectral_norm | 1.028755 | 1.020317 | 1.022064 | 1.011050 | 2.121791 | 1.955080 |
| json_bench | 1.032267 | 1.029657 | 1.030404 | 1.011148 | 1.159126 | 2.448137 |
| str_methods | 1.001996 | 1.001637 | 1.002249 | 1.005601 | 2.031568 | 2.125671 |
| dict_ops | 1.007611 | 1.008054 | 1.008853 | 1.010124 | 5.539424 | 1.922912 |
| list_ops | 0.996323 | 0.997017 | 0.996818 | 1.010710 | 13.956505 | 1.924650 |
| attr_access | 0.926542 | 0.948559 | 0.947364 | 1.007634 | 2.588387 | 1.989236 |
| call_overhead | 0.985910 | 0.985828 | 0.985719 | 1.007054 | 7.982059 | 1.989305 |
| generators | 0.989572 | 0.992099 | 0.991795 | 1.007791 | 9.658890 | 1.951509 |
| deque_ops | 1.003074 | 1.003622 | 1.003694 | 1.004559 | 17.475284 | 1.824707 |
| datetime_ops | 1.003809 | 1.003727 | 1.003638 | 1.004527 | 119.529866 | 2.138116 |
| pickle_bench | 1.023047 | 1.022808 | 1.023007 | 1.006800 | 209.627773 | 2.384693 |
| startup | 1.008480 | 1.008480 | 1.008439 | 1.008484 | 1.360069 | 1.930661 |

### Startup and imports

| Case | Wall / baseline | CPU / baseline | RSS / baseline | Wall / CPython | CPU / CPython | RSS / CPython |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| startup_matched | 0.999156 | 1.000178 | 1.004297 | 1.341612 | 1.392382 | 1.781046 |
| startup_relocated | 1.002364 | 1.002851 | 1.002446 | 1.356344 | 1.400355 | 1.784314 |
| no_site_matched | 1.003735 | 1.007440 | 1.011236 | 0.618242 | 0.590768 | 1.541502 |
| imports_matched | 1.004416 | 1.002875 | 1.001730 | 2.709157 | 2.890196 | 2.349239 |
| imports_relocated | 1.003107 | 1.002577 | 1.000863 | 2.725517 | 2.888642 | 2.353299 |
| pickle_import_matched | 1.004435 | 1.003490 | 1.003431 | 2.138343 | 2.272066 | 2.153361 |
| pickle_import_relocated | 1.000104 | 0.999300 | 1.004388 | 2.140627 | 2.276783 | 2.158281 |
| accelerator_first_matched | 0.998405 | 0.998533 | 1.003425 | 2.124234 | 2.250556 | 2.151674 |
| accelerator_first_relocated | 0.996796 | 0.996757 | 1.002933 | 2.129867 | 2.250378 | 2.154250 |

## Limits and next step

- No energy/scaling/portability/controlled build-time claim.
- Native validation ran on ARM64; the added 32-bit storage constructor has not been cross-compiled or run in this experiment.
- Existing fold __index__ coercion and native-frame identity differences remain unresolved.
- Allocation count and live-allocation profiling were not performed; retained-object peak RSS is a process measurement, not an allocator count.

The full 23-workload geometric mean remains about 3.43 times CPython and all 24 process RSS ratios remain above CPython. The requested goal is unachieved. Next, test a positive layout cache keyed by the existing globally unique class-resolution version and a cheaper fallback entry. Preserve mutation checks and compare unchanged workloads against this candidate and phase 94 before integration.

Raw evidence preserves interpreter measurements, every metric (including warmed workload CPU time), every sample and cache identity, full traces, all source versions, validation failures and corrections, and all load observations. The standalone CLIs and build/frozen caches are not included in the source archive.

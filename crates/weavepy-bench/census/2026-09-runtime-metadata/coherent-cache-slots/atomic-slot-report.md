# Publish coherent instruction-cache values

Instruction-cache values now use coherent atomic snapshots. Targets with always-lock-free 128-bit atomics store each value in one wide atomic. Other targets use three atomic 64-bit words: checked epoch and payload halves. Contention or an interrupted writer produces a cache miss without waiting; the epoch never wraps. Safe explicit encoding avoids reading enum padding. This repairs the former shared-slot UnsafeCell race while retaining lazy table allocation through release/acquire pointer publication.

Measured executable: `target/release/weavepy-runtime-atomic-slots`, SHA-256 `ca6b66ef23195444dacac40c87ccc64f85291a67c6a18fe5dcf76fbfcce84c0a`, 44,176,016 bytes.

Release-library metadata reports a 480-byte CodeObject header and 16-byte CacheSlot on this host, unchanged from the preceding atomic-table release. The nonblocking fallback uses 24-byte slots on targets without always-lock-free wide atomics. Removing the shared-slot race does not resolve every other free-threaded runtime invariant or enable native JIT execution with the GIL disabled.

The checkpoint is `9a69c4161ade6527a5f4457d7e7272075cb1f0d9`. The paired preceding release is atomic table publication, identified in the frozen environment. All 220 compatibility checks pass, along with 36 compiler tests, 49 JIT tests, 289 VM tests, Clippy, workspace/feature checks, and debug/release execution checks. The new Python regression shares the same attribute and call code across four threads using different receiver and callable shapes.

Fourteen tests using the actual production cache modules pass with Rust 1.93 and Miri strict provenance under default and Tree Borrows on ARM64 and i686. Under Miri the storage selector exercises the non-native fallback, so this does not emulate the hardware wide-atomic assembly. Two Loom models cover reader/writer and competing-writer scenarios using the actual fallback operations, without preemption limits. These bounded models supplement the ordering argument; Loom documents limits on relaxed-ordering exploration. The historical failing Miri diagnostic for the old UnsafeCell remains preserved.

## Standard suite

Ratios below one mean less time or memory. Five alternating measured cycles retain every existing fixture and work parameter. Workload time excludes startup; process elapsed time, process CPU time, and peak RSS include it. Startup has no separate workload timer, leaving 23 workload-time comparisons. The table aggregates paired cycle ratios geometrically.

| Metric | JIT/checkpoint | Interpreter/checkpoint | JIT/previous | Interpreter/previous | JIT/CPython | Interpreter/CPython |
|---|---:|---:|---:|---:|---:|---:|
| Workload time | 0.850 | 0.974 | 1.000 | 1.010 | 3.625 | 9.650 |
| Process elapsed time | 0.907 | 0.984 | 0.993 | 1.013 | 3.468 | 5.980 |
| Process CPU time | 0.907 | 0.984 | 0.994 | 1.013 | 3.547 | 6.180 |
| Peak RSS | 0.946 | 0.939 | 0.994 | 0.999 | 2.095 | 1.924 |

JIT workload-time wins against CPython: 6/23. Peak-RSS wins: 0/24. These results do not establish superiority across all meaningful workloads or metrics.

| Fixture | JIT time/previous | Interpreter time/previous | JIT time/CPython | JIT RSS/previous | JIT RSS/CPython |
|---|---:|---:|---:|---:|---:|
| fannkuch | 1.019 | 1.044 | 10.485 | 0.999 | 1.981 |
| nbody | 1.015 | 1.014 | 8.873 | 0.998 | 1.971 |
| fib | 1.006 | 1.015 | 2.915 | 0.996 | 2.002 |
| pidigits | 0.997 | 1.002 | 0.889 | 1.004 | 2.010 |
| pyaes | 0.991 | 1.001 | 0.651 | 0.998 | 1.988 |
| richards | 1.000 | 1.010 | 7.725 | 0.998 | 1.994 |
| sumvm | 0.996 | 1.019 | 0.057 | 0.999 | 1.996 |
| nested_loops | 1.019 | 1.002 | 0.081 | 0.999 | 2.009 |
| jitloop | 1.005 | 1.071 | 0.073 | 0.997 | 2.011 |
| jitkernels | 1.012 | 1.012 | 0.825 | 1.001 | 1.993 |
| deltablue | 0.989 | 1.007 | 19.509 | 1.002 | 2.181 |
| float_math | 1.004 | 1.012 | 7.949 | 0.999 | 3.091 |
| spectral_norm | 1.009 | 1.011 | 2.179 | 1.001 | 2.008 |
| json_bench | 0.992 | 1.000 | 1.137 | 0.940 | 2.517 |
| str_methods | 0.973 | 0.956 | 3.174 | 1.000 | 2.077 |
| dict_ops | 1.004 | 0.997 | 5.391 | 1.001 | 1.974 |
| list_ops | 0.997 | 1.009 | 13.719 | 0.998 | 1.981 |
| attr_access | 1.008 | 1.040 | 3.173 | 0.996 | 2.119 |
| call_overhead | 0.983 | 1.006 | 9.113 | 1.000 | 2.033 |
| generators | 0.984 | 1.002 | 9.607 | 0.998 | 2.009 |
| deque_ops | 0.999 | 1.000 | 17.026 | 0.996 | 2.019 |
| datetime_ops | 1.010 | 0.999 | 151.381 | 0.998 | 2.146 |
| pickle_bench | 0.999 | 1.010 | 336.388 | 0.937 | 2.467 |
| startup | 0.983 | 1.022 | 1.492 | 0.997 | 1.991 |

The illustrative `fannkuch` loop is not canonical fannkuch; `pyaes` is an XOR scrambler. The unchanged fixture set includes standard-library workloads with native CPython implementations. CPython datetime measurements varied substantially between earlier stages; no cause has been established, and all samples remain.

## Focused controls

All focused workloads use nine cycles and the preserved atomic-table release as their baseline. Tables below use ratios of sample medians. CPU time and peak RSS cover the whole process. The allocation and code-storage probes use interpreter mode; the native slot, exact-name cache, and scalar-call probes also retain separate interpreter controls in their raw files.

### code-cache-probes

| Workload | Interpreter time/base | Interpreter CPU/base | Interpreter RSS/base | Interpreter time/CPython | Interpreter RSS/CPython |
|---|---:|---:|---:|---:|---:|
| retained_cold_code | 1.002 | 1.003 | 1.000 | 0.964 | 1.936 |
| retained_warm_code | 0.999 | 1.004 | 0.999 | 0.998 | 2.089 |
| code_compile_churn | 0.999 | 0.998 | 0.998 | 0.988 | 2.130 |
| class_version_churn | 1.006 | 0.998 | 0.996 | 6.077 | 1.850 |
| type_creation | 0.978 | 0.988 | 0.998 | 1.258 | 1.181 |

### type-cache-probes

| Workload | JIT time/base | JIT CPU/base | JIT RSS/base | JIT time/CPython | JIT RSS/CPython |
|---|---:|---:|---:|---:|---:|
| cached_short_attribute | 1.009 | 1.006 | 1.001 | 9.251 | 1.978 |
| cached_missing_attribute | 1.009 | 1.003 | 1.001 | 32.727 | 1.975 |
| cached_long_attribute | 1.014 | 1.008 | 1.000 | 8.951 | 1.976 |
| many_class_namespaces | 0.987 | 0.992 | 0.999 | 7.957 | 1.864 |

### slot-index-probes

| Workload | Interpreter time/base | Interpreter CPU/base | Interpreter RSS/base | Interpreter time/CPython | Interpreter RSS/CPython |
|---|---:|---:|---:|---:|---:|
| retained_slots_1 | 1.003 | 1.001 | 0.999 | 18.455 | 3.533 |
| last_slot_access_1 | 1.006 | 0.990 | 0.998 | 7.903 | 1.848 |
| retained_slots_2 | 0.986 | 0.988 | 0.999 | 17.594 | 3.588 |
| last_slot_access_2 | 1.002 | 1.001 | 0.998 | 7.686 | 1.846 |
| retained_slots_8 | 1.005 | 1.003 | 1.000 | 15.235 | 2.856 |
| last_slot_access_8 | 1.004 | 1.005 | 0.995 | 7.806 | 1.843 |
| retained_slots_9 | 1.005 | 1.004 | 1.000 | 14.005 | 3.169 |
| last_slot_access_9 | 1.001 | 0.996 | 0.997 | 7.837 | 1.840 |
| retained_slots_16 | 1.008 | 1.009 | 0.999 | 15.163 | 4.172 |
| last_slot_access_16 | 0.990 | 1.003 | 0.997 | 7.729 | 1.840 |
| slot_delete_reinsert | 1.016 | 1.007 | 0.998 | 14.458 | 1.847 |
| slot_access_8_index_0 | 1.003 | 0.997 | 0.997 | 7.675 | 1.844 |
| slot_access_8_index_3 | 1.012 | 1.006 | 0.997 | 7.872 | 1.848 |
| alternating_slot_orders | 1.007 | 1.001 | 0.998 | 15.113 | 1.848 |

### jit-slot-probes

| Workload | JIT time/base | JIT CPU/base | JIT RSS/base | JIT time/CPython | JIT RSS/CPython |
|---|---:|---:|---:|---:|---:|
| native_slots_1 | 1.001 | 0.999 | 0.998 | 1.218 | 1.964 |
| native_slots_8 | 1.000 | 1.003 | 1.000 | 1.212 | 1.971 |
| native_slots_16 | 0.997 | 1.005 | 0.998 | 1.227 | 1.972 |
| native_alternating_slot_orders | 1.027 | 1.012 | 0.997 | 1.324 | 1.967 |

### scalar-result-probes

| Workload | JIT time/base | JIT CPU/base | JIT RSS/base | JIT time/CPython | JIT RSS/CPython |
|---|---:|---:|---:|---:|---:|
| integer_callback_sum | 1.006 | 1.014 | 0.998 | 2.485 | 1.974 |
| integer_callback_left | 1.002 | 1.008 | 1.001 | 2.360 | 1.975 |
| keyword_callback_sum | 1.003 | 1.002 | 0.998 | 14.005 | 1.968 |
| callable_instance_sum | 1.028 | 1.033 | 0.998 | 10.380 | 1.970 |

## Cold regression controls

Nine paired cycles compare compact caches, atomic table publication, this release, and CPython. Warm controls execute the workload once before timing it. Cold denotes the absence of that explicit workload warmup, not a flushed operating-system page cache.

| Fixture | JIT/compact | Interpreter/compact | JIT/previous | Interpreter/previous | JIT RSS/previous | Interpreter RSS/previous | JIT/CPython | JIT RSS/CPython |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| str_methods | 1.005 | 1.019 | 0.974 | 1.005 | 1.002 | 0.998 | 3.124 | 2.076 |
| list_ops | 1.005 | 1.013 | 1.003 | 1.006 | 0.999 | 0.997 | 13.774 | 1.980 |
| generators | 1.010 | 1.015 | 0.995 | 1.002 | 0.999 | 0.998 | 9.694 | 2.005 |
| call_overhead | 0.994 | 1.009 | 0.979 | 1.010 | 1.001 | 0.999 | 9.043 | 2.045 |

## Warm regression controls

Nine paired cycles compare compact caches, atomic table publication, this release, and CPython. Warm controls execute the workload once before timing it. Cold denotes the absence of that explicit workload warmup, not a flushed operating-system page cache.

| Fixture | JIT/compact | Interpreter/compact | JIT/previous | Interpreter/previous | JIT RSS/previous | Interpreter RSS/previous | JIT/CPython | JIT RSS/CPython |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| str_methods | 1.001 | 1.008 | 0.977 | 0.967 | 1.001 | 0.999 | 2.757 | 3.851 |
| list_ops | 1.010 | 0.999 | 1.010 | 1.001 | 0.998 | 1.001 | 13.798 | 1.951 |
| generators | 1.000 | 1.014 | 0.992 | 1.004 | 1.000 | 1.001 | 9.588 | 1.975 |
| call_overhead | 1.012 | 1.010 | 1.002 | 1.008 | 0.999 | 1.000 | 9.026 | 2.011 |

## Startup

| Workload | Time/checkpoint | RSS/checkpoint | Time/CPython | RSS/CPython |
|---|---:|---:|---:|---:|
| startup | 1.017 | 0.949 | 1.420 | 1.843 |
| startup_no_site | 1.027 | 1.004 | 0.607 | 1.553 |
| imports | 0.947 | 0.844 | 2.975 | 2.458 |

## Probes

| Workload | Time/checkpoint | RSS/checkpoint | Time/CPython | RSS/CPython |
|---|---:|---:|---:|---:|
| plain_instances | 0.901 | 0.881 | 17.528 | 3.882 |
| slotted_instances | 0.840 | 0.689 | 19.237 | 3.533 |
| memoryviews | 0.951 | 0.721 | 4.226 | 1.507 |
| memoryview_access | 0.958 | 0.950 | 6.303 | 1.850 |
| materialized_frames | 0.952 | 0.878 | 8.172 | 2.041 |
| bytesio_streams | 0.904 | 0.822 | 10.402 | 2.902 |
| type_creation | 1.001 | 0.932 | 1.270 | 1.180 |
| float_repr | 0.464 | 0.952 | 1.010 | 1.625 |
| float_str | 0.452 | 0.951 | 1.039 | 1.627 |
| int_repr | 0.810 | 0.953 | 4.002 | 1.627 |
| complex_repr | 0.375 | 0.954 | 0.764 | 1.658 |
| json_float_array | 0.149 | 0.849 | 0.228 | 2.171 |
| json_int_array | 0.287 | 0.858 | 0.745 | 2.138 |
| json_repeated_keys | 0.697 | 0.888 | 1.030 | 2.297 |
| json_unique_keys | 0.863 | 0.829 | 1.165 | 2.277 |

## Parallel execution

CPython is the installed GIL build. WeavePy modes refer to its GIL setting; this is not a free-threaded CPython comparison. CPU time includes all participating threads. WeavePy disables tier-2 native execution when free threading is requested, so its GIL-disabled rows run the interpreter.

| GIL setting | Parallel time/previous | Parallel CPU/previous | Peak RSS/previous | Parallel time/CPython | Parallel CPU/CPython | Peak RSS/CPython |
|---|---:|---:|---:|---:|---:|---:|
| 1 | 1.002 | 1.002 | 1.000 | 0.122 | 0.122 | 3.430 |
| 0 | 0.977 | 1.016 | 1.001 | 0.629 | 4.580 | 3.204 |

### Independent class mutations

Eight threads update and read separate classes. Five cycles compare this release with atomic table publication. All workers share the same kernel code, exercising coherent instruction-cache publication as receiver classes vary.

| GIL setting | Parallel time/previous | Parallel CPU/previous | Peak RSS/previous | Parallel time/CPython | Peak RSS/CPython |
|---|---:|---:|---:|---:|---:|
| 1 | 1.011 | 1.011 | 0.999 | 6.195 | 3.511 |
| 0 | 1.029 | 1.021 | 1.001 | 7.881 | 3.284 |

Build latency was not measured under controlled conditions. No build-time improvement is claimed. All regressions, raw samples, source hashes, execution proofs, and intermediate reports remain available.

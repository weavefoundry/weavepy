# Allocate instruction caches on first write

This release records each code object's logical cache length without immediately allocating cache slots. Its first valid cache write publishes a contiguous array through OnceLock. Cold reads, clearing, cloning, and resizing preserve the unallocated state. Warm clones own independent slot arrays, and resizing preserves in-range entries. Existing per-slot execution-lock requirements and cache guards remain active. The change adds no unsafe runtime code.

Measured executable: `target/release/weavepy-runtime-lazy-caches`, SHA-256 `2efdd33d8500b2273c679942f0ba322e40ed84013518e201c5d78c2365cdbf40`, 44,177,312 bytes.

Release-library metadata reports a 496-byte CodeObject header, compared with 488 bytes in the preceding release. Each allocated inline-cache slot remains 16 bytes. The measurements below include this header cost, first-write allocation, and the publication check on hot cache reads.

The checkpoint is `9a69c4161ade6527a5f4457d7e7272075cb1f0d9`. The paired preceding release is compact class-resolution caches, which has a complete frozen census. All 214 compatibility checks pass, along with 28 compiler tests, 49 JIT tests, 289 VM tests, Clippy, workspace/feature checks, and debug/release execution proofs. The new regression checks code-copy and marshal independence, stable wire code, tracing transitions, global rebinding, and first execution on separate threads.

## Standard suite

Ratios below one mean less time or memory. Five alternating measured cycles retain every existing fixture and work parameter. Workload time excludes startup; process elapsed time, process CPU time, and peak RSS include it. Startup has no separate workload timer, leaving 23 workload-time comparisons. The table aggregates paired cycle ratios geometrically.

| Metric | JIT/checkpoint | Interpreter/checkpoint | JIT/previous | Interpreter/previous | JIT/CPython | Interpreter/CPython |
|---|---:|---:|---:|---:|---:|---:|
| Workload time | 0.852 | 0.976 | 1.003 | 1.016 | 3.654 | 9.723 |
| Process elapsed time | 0.912 | 0.985 | 0.995 | 1.017 | 3.472 | 5.993 |
| Process CPU time | 0.910 | 0.986 | 0.995 | 1.016 | 3.553 | 6.200 |
| Peak RSS | 0.948 | 0.940 | 0.977 | 0.977 | 2.100 | 1.930 |

JIT workload-time wins against CPython: 6/23. Peak-RSS wins: 0/24. These results do not establish superiority across all meaningful workloads or metrics.

| Fixture | JIT time/previous | Interpreter time/previous | JIT time/CPython | JIT RSS/previous | JIT RSS/CPython |
|---|---:|---:|---:|---:|---:|
| fannkuch | 1.004 | 1.002 | 10.348 | 0.980 | 1.981 |
| nbody | 0.995 | 1.000 | 8.688 | 0.983 | 1.984 |
| fib | 1.008 | 0.990 | 2.999 | 0.984 | 2.003 |
| pidigits | 1.004 | 1.009 | 0.892 | 0.994 | 2.003 |
| pyaes | 0.995 | 1.044 | 0.657 | 0.985 | 2.004 |
| richards | 0.991 | 1.007 | 8.565 | 0.980 | 2.003 |
| sumvm | 0.998 | 1.039 | 0.057 | 0.980 | 1.992 |
| nested_loops | 0.995 | 1.011 | 0.082 | 0.982 | 2.008 |
| jitloop | 0.994 | 1.015 | 0.073 | 0.982 | 2.009 |
| jitkernels | 0.996 | 1.024 | 0.829 | 0.981 | 1.991 |
| deltablue | 1.003 | 1.002 | 19.495 | 0.990 | 2.172 |
| float_math | 1.014 | 0.993 | 7.951 | 0.995 | 3.092 |
| spectral_norm | 0.997 | 1.001 | 2.162 | 0.981 | 2.006 |
| json_bench | 1.007 | 1.010 | 1.148 | 0.909 | 2.575 |
| str_methods | 1.029 | 1.036 | 3.241 | 0.979 | 2.084 |
| dict_ops | 1.001 | 1.006 | 5.523 | 0.983 | 1.981 |
| list_ops | 1.044 | 1.049 | 14.382 | 0.980 | 1.982 |
| attr_access | 1.000 | 1.007 | 3.187 | 0.984 | 2.122 |
| call_overhead | 0.996 | 1.009 | 8.868 | 0.980 | 2.045 |
| generators | 1.009 | 1.112 | 9.642 | 0.983 | 2.005 |
| deque_ops | 1.007 | 0.999 | 16.977 | 0.980 | 2.023 |
| datetime_ops | 0.982 | 0.997 | 152.515 | 0.977 | 2.150 |
| pickle_bench | 0.991 | 1.005 | 330.016 | 0.915 | 2.492 |
| startup | 0.991 | 1.019 | 1.501 | 0.980 | 1.992 |

The illustrative `fannkuch` loop is not canonical fannkuch; `pyaes` is an XOR scrambler. The unchanged fixture set includes standard-library workloads with native CPython implementations. CPython datetime measurements varied substantially between earlier stages; no cause has been established, and all samples remain.

## Focused controls

All focused workloads use nine cycles and the preserved compact-cache release as their baseline. Tables below use ratios of sample medians. CPU time and peak RSS cover the whole process. The allocation and code-storage probes use interpreter mode; the native slot, exact-name cache, and scalar-call probes also retain separate interpreter controls in their raw files.

### code-cache-probes

| Workload | Interpreter time/base | Interpreter CPU/base | Interpreter RSS/base | Interpreter time/CPython | Interpreter RSS/CPython |
|---|---:|---:|---:|---:|---:|
| retained_cold_code | 1.005 | 0.998 | 0.912 | 0.962 | 1.947 |
| retained_warm_code | 1.006 | 0.998 | 0.981 | 1.001 | 2.095 |
| code_compile_churn | 1.005 | 0.999 | 0.979 | 0.990 | 2.136 |
| class_version_churn | 0.946 | 0.969 | 0.974 | 6.168 | 1.848 |
| type_creation | 0.994 | 0.994 | 0.988 | 1.261 | 1.183 |

### type-cache-probes

| Workload | JIT time/base | JIT CPU/base | JIT RSS/base | JIT time/CPython | JIT RSS/CPython |
|---|---:|---:|---:|---:|---:|
| cached_short_attribute | 0.992 | 0.997 | 0.981 | 9.010 | 1.980 |
| cached_missing_attribute | 0.992 | 0.986 | 0.982 | 32.357 | 1.980 |
| cached_long_attribute | 0.999 | 1.003 | 0.980 | 8.573 | 1.977 |
| many_class_namespaces | 1.007 | 1.014 | 0.980 | 8.459 | 1.870 |

### slot-index-probes

| Workload | Interpreter time/base | Interpreter CPU/base | Interpreter RSS/base | Interpreter time/CPython | Interpreter RSS/CPython |
|---|---:|---:|---:|---:|---:|
| retained_slots_1 | 0.986 | 0.984 | 0.992 | 18.325 | 3.527 |
| last_slot_access_1 | 1.021 | 0.997 | 0.975 | 7.927 | 1.849 |
| retained_slots_2 | 0.999 | 0.999 | 0.993 | 17.596 | 3.586 |
| last_slot_access_2 | 1.024 | 0.999 | 0.976 | 7.908 | 1.846 |
| retained_slots_8 | 0.992 | 0.991 | 0.995 | 15.161 | 2.856 |
| last_slot_access_8 | 1.023 | 1.004 | 0.975 | 7.675 | 1.847 |
| retained_slots_9 | 1.030 | 1.027 | 0.996 | 14.381 | 3.170 |
| last_slot_access_9 | 1.020 | 1.004 | 0.977 | 7.678 | 1.845 |
| retained_slots_16 | 1.020 | 1.020 | 0.998 | 15.409 | 4.171 |
| last_slot_access_16 | 1.025 | 1.004 | 0.977 | 8.065 | 1.843 |
| slot_delete_reinsert | 1.012 | 1.000 | 0.973 | 14.140 | 1.847 |
| slot_access_8_index_0 | 1.028 | 1.005 | 0.976 | 7.954 | 1.848 |
| slot_access_8_index_3 | 1.026 | 1.008 | 0.974 | 8.082 | 1.848 |
| alternating_slot_orders | 1.026 | 1.017 | 0.974 | 15.009 | 1.850 |

### jit-slot-probes

| Workload | JIT time/base | JIT CPU/base | JIT RSS/base | JIT time/CPython | JIT RSS/CPython |
|---|---:|---:|---:|---:|---:|
| native_slots_1 | 1.008 | 1.002 | 0.983 | 1.207 | 1.977 |
| native_slots_8 | 0.997 | 0.999 | 0.981 | 1.224 | 1.981 |
| native_slots_16 | 1.002 | 0.999 | 0.982 | 1.232 | 1.981 |
| native_alternating_slot_orders | 0.987 | 1.001 | 0.979 | 1.310 | 1.973 |

### scalar-result-probes

| Workload | JIT time/base | JIT CPU/base | JIT RSS/base | JIT time/CPython | JIT RSS/CPython |
|---|---:|---:|---:|---:|---:|
| integer_callback_sum | 1.034 | 1.023 | 0.981 | 2.566 | 1.981 |
| integer_callback_left | 1.042 | 1.025 | 0.978 | 2.383 | 1.977 |
| keyword_callback_sum | 1.020 | 1.025 | 0.983 | 14.162 | 1.974 |
| callable_instance_sum | 0.999 | 1.001 | 0.982 | 10.099 | 1.979 |

## Startup

| Workload | Time/checkpoint | RSS/checkpoint | Time/CPython | RSS/CPython |
|---|---:|---:|---:|---:|
| startup | 1.019 | 0.950 | 1.427 | 1.845 |
| startup_no_site | 1.021 | 0.996 | 0.604 | 1.539 |
| imports | 0.938 | 0.843 | 2.923 | 2.453 |

## Probes

| Workload | Time/checkpoint | RSS/checkpoint | Time/CPython | RSS/CPython |
|---|---:|---:|---:|---:|
| plain_instances | 0.904 | 0.881 | 17.508 | 3.881 |
| slotted_instances | 0.843 | 0.688 | 19.436 | 3.530 |
| memoryviews | 0.924 | 0.722 | 4.177 | 1.511 |
| memoryview_access | 0.946 | 0.953 | 6.221 | 1.850 |
| materialized_frames | 0.957 | 0.879 | 8.179 | 2.041 |
| bytesio_streams | 0.918 | 0.823 | 10.597 | 2.908 |
| type_creation | 0.995 | 0.931 | 1.253 | 1.182 |
| float_repr | 0.472 | 0.954 | 1.029 | 1.623 |
| float_str | 0.456 | 0.956 | 1.045 | 1.628 |
| int_repr | 0.800 | 0.956 | 3.945 | 1.633 |
| complex_repr | 0.377 | 0.953 | 0.767 | 1.659 |
| json_float_array | 0.153 | 0.848 | 0.234 | 2.178 |
| json_int_array | 0.325 | 0.852 | 0.818 | 2.130 |
| json_repeated_keys | 0.701 | 0.877 | 1.041 | 2.268 |
| json_unique_keys | 0.861 | 0.826 | 1.175 | 2.268 |

## Parallel execution

CPython is the installed GIL build. WeavePy modes refer to its GIL setting; this is not a free-threaded CPython comparison. CPU time includes all participating threads. WeavePy disables tier-2 native execution when free threading is requested, so its GIL-disabled rows run the interpreter.

| GIL setting | Parallel time/CPython | Parallel CPU/CPython | Peak RSS/CPython |
|---|---:|---:|---:|
| 1 | 0.117 | 0.117 | 3.448 |
| 0 | 0.597 | 4.426 | 3.209 |

Build latency was not measured under controlled conditions. No build-time improvement is claimed. All regressions, raw samples, source hashes, execution proofs, and intermediate reports remain available.

## Repeated timing controls

Nine additional alternating cycles repeat the larger regressions against compact caches. Cold runs time the first workload invocation; warm runs invoke the workload once before its timer. These are separate results, and all original samples remain.

| Mode | Workload | JIT time/previous | Interpreter time/previous | JIT RSS/previous |
|---|---|---:|---:|---:|
| cold | str_methods | 1.044 | 1.034 | 0.980 |
| cold | list_ops | 1.049 | 1.053 | 0.984 |
| cold | generators | 1.008 | 1.115 | 0.983 |
| cold | call_overhead | 1.005 | 1.007 | 0.984 |
| warm | str_methods | 1.033 | 1.044 | 0.988 |
| warm | list_ops | 1.062 | 1.054 | 0.981 |
| warm | generators | 0.995 | 1.109 | 0.979 |
| warm | call_overhead | 1.010 | 1.012 | 0.981 |

The interpreter generator slowdown repeats at roughly11% in both modes. List and string slowdowns also repeat. The candidate remains an intermediate design while its hot cache-read path is investigated.

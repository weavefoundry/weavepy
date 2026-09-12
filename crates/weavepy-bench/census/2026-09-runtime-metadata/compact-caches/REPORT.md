# Compact class-resolution caches

This release halves each per-instruction inline cache from 32 to 16 bytes. Narrow fields precede eight-byte fields, and attribute caches use one process-unique class-resolution token. The token allocator uses checked 64-bit increments and cannot wrap into an earlier state. Class mutations still invalidate the class and its subclasses; attribute-name and indexed-key checks remain active. The exact-name type cache also drops its redundant stored class address. No new unsafe code is needed for these changes.

Measured executable: `target/release/weavepy-runtime-compact-caches`, SHA-256 `f245859193906c745f3950dfb83dba6affc4d781efd15b9a7721eb73bf4932c8`, 44,159,200 bytes.

The checkpoint is `9a69c4161ade6527a5f4457d7e7272075cb1f0d9`. The paired preceding release is indexed slot access, which has its own complete focused measurements and compatibility results. All 211 compatibility checks pass. Compiler, JIT, VM, feature, and native-execution checks are retained with the results. The original native method coverage driver remained interpreted in both releases; a separate driver proves the native path, and the initial coverage failure remains available.

## Standard suite

Ratios below one mean less time or memory. Five alternating measured cycles retain every existing fixture and work parameter. Workload time excludes startup; process elapsed time, process CPU time, and peak RSS include it. Startup has no separate workload timer, leaving 23 workload-time comparisons. The table aggregates paired cycle ratios geometrically.

| Metric | JIT/checkpoint | Interpreter/checkpoint | JIT/previous | Interpreter/previous | JIT/CPython | Interpreter/CPython |
|---|---:|---:|---:|---:|---:|---:|
| Workload time | 0.848 | 0.961 | 0.994 | 1.001 | 3.632 | 9.543 |
| Process elapsed time | 0.909 | 0.975 | 0.984 | 1.002 | 3.455 | 5.908 |
| Process CPU time | 0.908 | 0.974 | 0.990 | 1.003 | 3.539 | 6.107 |
| Peak RSS | 0.967 | 0.962 | 0.973 | 0.977 | 2.138 | 1.972 |

JIT workload-time wins against CPython: 6/23. Peak-RSS wins: 0/24. These results do not establish superiority across all meaningful workloads or metrics.

| Fixture | JIT time/previous | Interpreter time/previous | JIT time/CPython | JIT RSS/previous | JIT RSS/CPython |
|---|---:|---:|---:|---:|---:|
| fannkuch | 1.001 | 1.002 | 10.382 | 0.979 | 2.028 |
| nbody | 0.991 | 0.996 | 8.764 | 0.979 | 2.019 |
| fib | 0.999 | 1.003 | 2.970 | 0.978 | 2.038 |
| pidigits | 1.000 | 1.001 | 0.891 | 0.976 | 2.001 |
| pyaes | 0.993 | 1.007 | 0.650 | 0.978 | 2.033 |
| richards | 0.979 | 1.005 | 7.491 | 0.978 | 2.036 |
| sumvm | 0.974 | 0.994 | 0.058 | 0.979 | 2.038 |
| nested_loops | 0.991 | 1.012 | 0.082 | 0.979 | 2.053 |
| jitloop | 0.994 | 1.013 | 0.072 | 0.974 | 2.041 |
| jitkernels | 0.953 | 1.001 | 0.822 | 0.975 | 2.023 |
| deltablue | 0.992 | 0.992 | 19.547 | 0.984 | 2.202 |
| float_math | 1.005 | 0.998 | 8.050 | 1.015 | 3.105 |
| spectral_norm | 0.993 | 1.004 | 2.170 | 0.986 | 2.057 |
| json_bench | 0.997 | 0.995 | 1.148 | 0.904 | 2.611 |
| str_methods | 1.003 | 0.999 | 3.144 | 0.984 | 2.122 |
| dict_ops | 1.003 | 1.004 | 5.511 | 0.977 | 2.018 |
| list_ops | 1.002 | 1.000 | 13.675 | 0.972 | 2.020 |
| attr_access | 0.985 | 0.990 | 3.268 | 0.977 | 2.163 |
| call_overhead | 1.006 | 1.001 | 9.134 | 0.977 | 2.091 |
| generators | 1.000 | 1.001 | 9.521 | 0.974 | 2.042 |
| deque_ops | 0.992 | 1.000 | 16.701 | 0.976 | 2.061 |
| datetime_ops | 1.016 | 1.001 | 156.769 | 0.974 | 2.203 |
| pickle_bench | 1.003 | 1.003 | 339.035 | 0.912 | 2.555 |
| startup | 0.974 | 1.011 | 1.462 | 0.976 | 2.036 |

The illustrative `fannkuch` loop is not canonical fannkuch; `pyaes` is an XOR scrambler. The unchanged fixture set includes standard-library workloads with native CPython implementations. CPython datetime measurements varied substantially between earlier stages; no cause has been established, and all samples remain.

## Focused controls

All focused workloads use nine cycles and the preserved indexed-slot release as their baseline. Tables below use ratios of sample medians. CPU time and peak RSS cover the whole process. The allocation and code-storage probes use interpreter mode; the native slot, exact-name cache, and scalar-call probes also retain separate interpreter controls in their raw files.

### code-cache-probes

| Workload | Interpreter time/base | Interpreter CPU/base | Interpreter RSS/base | Interpreter time/CPython | Interpreter RSS/CPython |
|---|---:|---:|---:|---:|---:|
| retained_cold_code | 0.994 | 0.997 | 0.915 | 0.965 | 2.140 |
| retained_warm_code | 0.993 | 0.992 | 0.915 | 0.995 | 2.134 |
| code_compile_churn | 1.005 | 1.001 | 0.930 | 0.988 | 2.177 |
| class_version_churn | 1.007 | 1.006 | 0.979 | 6.607 | 1.901 |
| type_creation | 1.004 | 1.005 | 0.987 | 1.272 | 1.199 |

### type-cache-probes

| Workload | JIT time/base | JIT CPU/base | JIT RSS/base | JIT time/CPython | JIT RSS/CPython |
|---|---:|---:|---:|---:|---:|
| cached_short_attribute | 1.001 | 1.000 | 0.976 | 8.927 | 2.020 |
| cached_missing_attribute | 1.005 | 1.011 | 0.978 | 33.372 | 2.018 |
| cached_long_attribute | 1.004 | 0.999 | 0.979 | 8.856 | 2.021 |
| many_class_namespaces | 1.003 | 1.003 | 0.979 | 8.117 | 1.910 |

### slot-index-probes

| Workload | Interpreter time/base | Interpreter CPU/base | Interpreter RSS/base | Interpreter time/CPython | Interpreter RSS/CPython |
|---|---:|---:|---:|---:|---:|
| retained_slots_1 | 1.006 | 1.006 | 1.014 | 18.478 | 3.559 |
| last_slot_access_1 | 0.979 | 0.989 | 0.977 | 7.783 | 1.894 |
| retained_slots_2 | 1.011 | 1.010 | 1.012 | 17.793 | 3.614 |
| last_slot_access_2 | 0.982 | 0.990 | 0.978 | 7.704 | 1.894 |
| retained_slots_8 | 0.997 | 0.996 | 1.008 | 15.168 | 2.870 |
| last_slot_access_8 | 0.987 | 0.994 | 0.979 | 7.936 | 1.894 |
| retained_slots_9 | 0.993 | 0.994 | 1.007 | 14.053 | 3.182 |
| last_slot_access_9 | 0.983 | 0.991 | 0.978 | 7.668 | 1.895 |
| retained_slots_16 | 0.995 | 0.995 | 1.003 | 15.231 | 4.180 |
| last_slot_access_16 | 0.983 | 0.991 | 0.976 | 7.626 | 1.888 |
| slot_delete_reinsert | 0.995 | 0.989 | 0.977 | 14.292 | 1.898 |
| slot_access_8_index_0 | 0.981 | 0.998 | 0.978 | 7.718 | 1.900 |
| slot_access_8_index_3 | 0.979 | 0.987 | 0.977 | 7.846 | 1.894 |
| alternating_slot_orders | 1.005 | 0.993 | 0.978 | 14.800 | 1.899 |

### jit-slot-probes

| Workload | JIT time/base | JIT CPU/base | JIT RSS/base | JIT time/CPython | JIT RSS/CPython |
|---|---:|---:|---:|---:|---:|
| native_slots_1 | 0.893 | 0.973 | 0.978 | 1.216 | 2.016 |
| native_slots_8 | 0.913 | 0.971 | 0.976 | 1.231 | 2.013 |
| native_slots_16 | 0.914 | 0.974 | 0.973 | 1.228 | 2.019 |
| native_alternating_slot_orders | 0.913 | 0.956 | 0.978 | 1.323 | 2.018 |

### scalar-result-probes

| Workload | JIT time/base | JIT CPU/base | JIT RSS/base | JIT time/CPython | JIT RSS/CPython |
|---|---:|---:|---:|---:|---:|
| integer_callback_sum | 1.000 | 1.009 | 0.978 | 2.497 | 2.015 |
| integer_callback_left | 1.009 | 1.002 | 0.979 | 2.322 | 2.017 |
| keyword_callback_sum | 1.001 | 0.997 | 0.980 | 13.711 | 2.016 |
| callable_instance_sum | 0.983 | 0.991 | 0.978 | 10.071 | 2.016 |

## Startup

| Workload | Time/checkpoint | RSS/checkpoint | Time/CPython | RSS/CPython |
|---|---:|---:|---:|---:|
| startup | 1.029 | 0.975 | 1.425 | 1.894 |
| startup_no_site | 1.033 | 1.014 | 0.611 | 1.569 |
| imports | 0.953 | 0.886 | 2.988 | 2.580 |

## Probes

| Workload | Time/checkpoint | RSS/checkpoint | Time/CPython | RSS/CPython |
|---|---:|---:|---:|---:|
| plain_instances | 0.918 | 0.886 | 17.575 | 3.905 |
| slotted_instances | 0.854 | 0.694 | 19.950 | 3.561 |
| memoryviews | 0.929 | 0.728 | 4.189 | 1.524 |
| memoryview_access | 0.951 | 0.977 | 6.109 | 1.896 |
| materialized_frames | 0.964 | 0.894 | 8.255 | 2.079 |
| bytesio_streams | 0.897 | 0.836 | 10.326 | 2.951 |
| type_creation | 0.982 | 0.946 | 1.249 | 1.199 |
| float_repr | 0.469 | 0.977 | 1.019 | 1.669 |
| float_str | 0.448 | 0.975 | 1.041 | 1.664 |
| int_repr | 0.812 | 0.978 | 3.909 | 1.668 |
| complex_repr | 0.375 | 0.979 | 0.765 | 1.700 |
| json_float_array | 0.148 | 0.880 | 0.228 | 2.249 |
| json_int_array | 0.295 | 0.888 | 0.753 | 2.230 |
| json_repeated_keys | 0.703 | 0.915 | 1.039 | 2.370 |
| json_unique_keys | 0.861 | 0.857 | 1.179 | 2.352 |

## Parallel execution

CPython is the installed GIL build. WeavePy modes refer to its GIL setting; this is not a free-threaded CPython comparison. CPU time includes all participating threads. WeavePy currently disables tier-2 native execution when free threading is requested, so its GIL-disabled rows run the interpreter.

| GIL setting | Parallel time/CPython | Parallel CPU/CPython | Peak RSS/CPython |
|---|---:|---:|---:|
| 1 | 0.115 | 0.115 | 3.606 |
| 0 | 0.578 | 4.295 | 3.381 |

Build latency was not measured under controlled conditions. No build-time improvement is claimed. All regressions, raw samples, source hashes, execution proofs, and intermediate reports remain available.

## Parallel class mutation

Eight threads mutate independent classes, exercising the shared token allocator. Each performs 20,000 checked updates. Five alternating cycles follow a discarded cycle. Ratios use paired cycle medians. The baseline is indexed slots; CPython is the installed GIL build.

| GIL setting | Parallel time/previous | Parallel CPU/previous | RSS/previous | Parallel time/CPython | RSS/CPython |
|---|---:|---:|---:|---:|---:|
| 1 | 0.999 | 0.999 | 0.937 | 6.666 | 3.618 |
| 0 | 1.025 | 1.021 | 0.932 | 8.961 | 3.412 |

This supplemental workload was added after the main census to check contention in the new global token allocator. Its script and full samples are retained alongside the frozen main-census inputs.

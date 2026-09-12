# Reduce per-object collector metadata

The collector stores color and generation in separate atomic bytes. Reference counts and position counters retain their full widths, and all existing memory orders and lock relationships remain. Constructor bounds checks reject invalid generation indices before narrowing; promotion saturates at the oldest generation.

Measured executable: `target/release/weavepy-runtime-compact-gc`, SHA-256 `b63eb22dcb78258971fe10ec36c4ee85a87c5b1adb1cf1a0c500590523292cf5`, 44,193,984 bytes.

The measured collector handle falls from 96 to 80 bytes on this host. Object remains 24 bytes, the instance header remains 176 bytes, CodeObject remains 480 bytes, and CacheSlot remains 16 bytes. The following process measurements include allocator behavior and collection overhead; the header change alone does not establish a peak-memory or speed improvement.

The checkpoint is `9a69c4161ade6527a5f4457d7e7272075cb1f0d9`. The paired preceding release is lazy instance dictionaries, identified in the frozen environment. All 226 compatibility checks pass, along with 300 VM tests, 49 JIT tests, Clippy, workspace/feature checks, and debug/release execution checks.

New lifecycle tests exercise rooted cycles through partial and full collections, frozen unreachable objects, weak-reference reclamation, and finalizers running exactly once. Rust tests inspect generation promotion and capping, frozen exclusion, unfreeze resets, and position updates after removal. Invalid constructor indices cannot silently truncate. These controls passed against CPython and the preceding release before the runtime change. No new unsafe code is introduced.

The public Rust TrackedHandle.color and TrackedHandle.generation fields now use AtomicU8; the public color constants use u8. Python generation arguments remain unchanged. This change does not establish complete free-threaded VM safety; native execution with the GIL disabled remains gated.

A separate diagnostic found an existing native slice defect in both the preceding release and this candidate: an explicit stop of -2**63 can collide with the missing-bound sentinel and return a full slice after compilation. The interpreter and CPython return an empty slice. A second diagnostic confirms that valid two-bound slice bytecode reconstructs an extra stack operand during native fallback on Unicode input, raising TypeError instead of returning the slice. Both defects occur in the preceding release and candidate. Reproducers and native compilation/deoptimization traces are preserved; the 226-check suite does not cover these edges, and this collector change does not repair them.

## Standard suite

Ratios below one mean less time or memory. Five alternating measured cycles retain every existing fixture and work parameter. Workload time excludes startup; process elapsed time, process CPU time, and peak RSS include it. Startup has no separate workload timer, leaving 23 workload-time comparisons. The table aggregates paired cycle ratios geometrically.

| Metric | JIT/checkpoint | Interpreter/checkpoint | JIT/previous | Interpreter/previous | JIT/CPython | Interpreter/CPython |
|---|---:|---:|---:|---:|---:|---:|
| Workload time | 0.848 | 0.971 | 0.998 | 1.001 | 3.280 | 8.712 |
| Process elapsed time | 0.906 | 0.980 | 0.992 | 1.002 | 3.194 | 5.488 |
| Process CPU time | 0.906 | 0.981 | 0.994 | 1.003 | 3.261 | 5.668 |
| Peak RSS | 0.947 | 0.943 | 0.996 | 1.000 | 2.094 | 1.933 |

JIT workload-time wins against CPython: 6/23. Peak-RSS wins: 0/24. These results do not establish superiority across all meaningful workloads or metrics.

| Fixture | JIT time/previous | Interpreter time/previous | JIT time/CPython | JIT RSS/previous | JIT RSS/CPython |
|---|---:|---:|---:|---:|---:|
| fannkuch | 1.006 | 1.003 | 10.243 | 0.996 | 1.988 |
| nbody | 1.003 | 0.998 | 8.917 | 0.994 | 1.979 |
| fib | 0.983 | 1.004 | 2.882 | 0.992 | 2.008 |
| pidigits | 1.002 | 1.002 | 0.891 | 0.991 | 1.975 |
| pyaes | 0.994 | 0.998 | 0.649 | 0.997 | 1.993 |
| richards | 1.001 | 1.007 | 7.866 | 1.000 | 1.995 |
| sumvm | 0.987 | 1.008 | 0.056 | 0.999 | 2.007 |
| nested_loops | 1.010 | 0.999 | 0.082 | 0.995 | 2.005 |
| jitloop | 1.003 | 1.001 | 0.075 | 0.995 | 2.003 |
| jitkernels | 1.009 | 1.007 | 0.868 | 0.994 | 1.989 |
| deltablue | 1.007 | 1.006 | 19.597 | 0.992 | 2.159 |
| float_math | 0.974 | 0.992 | 7.907 | 0.985 | 3.046 |
| spectral_norm | 1.001 | 0.996 | 2.162 | 0.995 | 2.005 |
| json_bench | 0.989 | 1.002 | 1.143 | 1.000 | 2.531 |
| str_methods | 1.003 | 0.995 | 3.164 | 0.995 | 2.082 |
| dict_ops | 1.021 | 1.007 | 5.555 | 0.996 | 1.973 |
| list_ops | 0.989 | 1.001 | 13.504 | 0.997 | 1.983 |
| attr_access | 0.989 | 1.006 | 2.998 | 0.999 | 2.120 |
| call_overhead | 1.012 | 0.999 | 9.287 | 0.996 | 2.044 |
| generators | 0.988 | 1.001 | 9.341 | 0.998 | 2.006 |
| deque_ops | 0.985 | 0.991 | 16.829 | 0.997 | 2.023 |
| datetime_ops | 0.996 | 1.000 | 15.598 | 0.996 | 2.149 |
| pickle_bench | 0.999 | 1.000 | 329.492 | 0.995 | 2.462 |
| startup | 0.990 | 1.008 | 1.442 | 0.999 | 2.002 |

The illustrative `fannkuch` loop is not canonical fannkuch; `pyaes` is an XOR scrambler. The unchanged fixture set includes standard-library workloads with native CPython implementations. CPython datetime measurements varied substantially between stages. At the unchanged 60,000-iteration workload, its median workload timer rose from about 24.6 ms in the preceding census to 258.1 ms here. Captured CPython executable, shared-library, datetime-module, and harness hashes match; no cause has been established. All samples remain. Changes between those CPython-relative ratios do not by themselves establish a WeavePy improvement; paired preceding-WeavePy comparisons remain available.

## Focused controls

All focused workloads use nine cycles and the preserved lazy-dictionary release as their baseline. Tables below use ratios of sample medians. CPU time and peak RSS cover the whole process. The allocation and code-storage probes use interpreter mode; the native slot, exact-name cache, and scalar-call probes also retain separate interpreter controls in their raw files.

### gc-metadata-probes

| Workload | Interpreter time/base | Interpreter CPU/base | Interpreter RSS/base | Interpreter time/CPython | Interpreter RSS/CPython |
|---|---:|---:|---:|---:|---:|
| retained_lists | 0.979 | 0.980 | 0.977 | 10.120 | 2.584 |
| retained_lists_gc_disabled | 0.958 | 0.975 | 0.972 | 9.282 | 2.396 |
| retained_dicts | 1.060 | 1.033 | 0.976 | 11.307 | 2.720 |
| retained_dicts_gc_disabled | 1.145 | 1.081 | 0.975 | 11.346 | 2.544 |
| retained_sets | 0.965 | 0.985 | 0.975 | 3.713 | 1.473 |
| retained_sets_gc_disabled | 0.977 | 0.988 | 0.976 | 6.560 | 1.474 |
| retained_tuples | 0.975 | 0.974 | 0.967 | 8.219 | 2.723 |
| retained_tuples_gc_disabled | 0.963 | 0.975 | 0.979 | 7.392 | 2.281 |
| rooted_full_scans | 0.983 | 0.984 | 0.986 | 8.327 | 1.831 |
| unreachable_cycle_scans | 0.970 | 0.974 | 0.997 | 19.282 | 3.498 |
| freeze_unfreeze_scans | 0.996 | 0.995 | 0.984 | 3.603 | 1.718 |

### instance-dict-probes

| Workload | Interpreter time/base | Interpreter CPU/base | Interpreter RSS/base | Interpreter time/CPython | Interpreter RSS/CPython |
|---|---:|---:|---:|---:|---:|
| retained_cold_plain | 0.993 | 0.992 | 0.979 | 15.119 | 2.944 |
| retained_cold_empty_slots | 1.000 | 0.997 | 0.979 | 18.008 | 3.681 |
| retained_exported_empty_dict | 0.987 | 0.990 | 0.981 | 10.537 | 2.638 |
| retained_populated_dict | 0.996 | 0.989 | 0.986 | 15.116 | 3.831 |
| retained_cold_native_int | 0.986 | 0.990 | 0.979 | 16.667 | 3.154 |
| retained_cold_native_list | 0.992 | 0.998 | 0.980 | 14.572 | 3.008 |
| churn_plain | 0.996 | 1.003 | 0.999 | 15.006 | 1.857 |
| churn_empty_slots | 1.013 | 1.006 | 1.005 | 18.581 | 1.863 |
| class_attribute_cold_dict | 0.997 | 0.998 | 1.004 | 10.684 | 1.872 |
| method_cold_dict | 0.995 | 0.991 | 0.999 | 14.012 | 1.862 |

### code-cache-probes

| Workload | Interpreter time/base | Interpreter CPU/base | Interpreter RSS/base | Interpreter time/CPython | Interpreter RSS/CPython |
|---|---:|---:|---:|---:|---:|
| retained_cold_code | 0.998 | 0.998 | 0.999 | 0.965 | 1.944 |
| retained_warm_code | 0.998 | 0.996 | 0.996 | 1.003 | 2.092 |
| code_compile_churn | 0.998 | 0.999 | 1.000 | 1.003 | 2.142 |
| class_version_churn | 1.007 | 0.992 | 1.001 | 6.000 | 1.860 |
| type_creation | 0.984 | 0.994 | 0.999 | 1.293 | 1.181 |

### type-cache-probes

| Workload | JIT time/base | JIT CPU/base | JIT RSS/base | JIT time/CPython | JIT RSS/CPython |
|---|---:|---:|---:|---:|---:|
| cached_short_attribute | 1.008 | 1.009 | 0.997 | 8.928 | 1.977 |
| cached_missing_attribute | 1.005 | 0.999 | 0.998 | 32.418 | 1.972 |
| cached_long_attribute | 1.004 | 1.006 | 0.997 | 8.595 | 1.982 |
| many_class_namespaces | 1.007 | 1.002 | 0.998 | 8.003 | 1.865 |

### slot-index-probes

| Workload | Interpreter time/base | Interpreter CPU/base | Interpreter RSS/base | Interpreter time/CPython | Interpreter RSS/CPython |
|---|---:|---:|---:|---:|---:|
| retained_slots_1 | 0.991 | 0.991 | 0.981 | 16.162 | 3.071 |
| last_slot_access_1 | 0.998 | 1.012 | 1.001 | 7.750 | 1.853 |
| retained_slots_2 | 0.992 | 0.992 | 0.984 | 15.696 | 3.181 |
| last_slot_access_2 | 0.998 | 0.994 | 1.004 | 7.833 | 1.860 |
| retained_slots_8 | 0.997 | 0.996 | 0.988 | 14.277 | 2.636 |
| last_slot_access_8 | 0.988 | 1.003 | 1.002 | 7.624 | 1.857 |
| retained_slots_9 | 1.002 | 1.002 | 0.991 | 13.328 | 2.972 |
| last_slot_access_9 | 0.989 | 0.989 | 1.002 | 7.707 | 1.851 |
| retained_slots_16 | 0.997 | 0.996 | 0.996 | 14.555 | 4.038 |
| last_slot_access_16 | 0.994 | 0.997 | 1.003 | 7.917 | 1.855 |
| slot_delete_reinsert | 0.985 | 0.993 | 1.001 | 14.011 | 1.855 |
| slot_access_8_index_0 | 0.986 | 1.005 | 0.998 | 7.885 | 1.851 |
| slot_access_8_index_3 | 0.982 | 0.983 | 1.002 | 7.572 | 1.859 |
| alternating_slot_orders | 1.000 | 1.003 | 0.999 | 14.413 | 1.858 |

### jit-slot-probes

| Workload | JIT time/base | JIT CPU/base | JIT RSS/base | JIT time/CPython | JIT RSS/CPython |
|---|---:|---:|---:|---:|---:|
| native_slots_1 | 1.020 | 1.015 | 1.001 | 1.234 | 1.981 |
| native_slots_8 | 1.011 | 1.016 | 0.999 | 1.243 | 1.975 |
| native_slots_16 | 1.016 | 1.010 | 0.999 | 1.248 | 1.968 |
| native_alternating_slot_orders | 1.048 | 1.034 | 0.999 | 1.376 | 1.965 |

### scalar-result-probes

| Workload | JIT time/base | JIT CPU/base | JIT RSS/base | JIT time/CPython | JIT RSS/CPython |
|---|---:|---:|---:|---:|---:|
| integer_callback_sum | 0.991 | 0.997 | 1.001 | 2.458 | 1.974 |
| integer_callback_left | 1.000 | 1.002 | 1.002 | 2.248 | 1.981 |
| keyword_callback_sum | 0.995 | 0.997 | 1.000 | 13.854 | 1.977 |
| callable_instance_sum | 0.986 | 0.988 | 1.002 | 10.140 | 1.979 |

## Explicit collection pauses

Each process records 51 explicit full-collection pauses after one discarded collection on a 10,000-node cyclic graph. Automatic collection is disabled. Seven alternating process cycles follow a discarded process cycle. The p95 uses nearest rank: the 49th ordered pause of 51. The table divides medians of per-process summaries; its maximum column therefore compares typical per-process maxima. Every individual pause remains in the raw data. These controls measure explicit collection latency, not application-wide or automatic-collection tail latency.

| Graph | Median pause/previous | p95 pause/previous | Maximum pause/previous | RSS/previous | Median pause/CPython | p95 pause/CPython | RSS/CPython |
|---|---:|---:|---:|---:|---:|---:|---:|
| rooted | 0.990 | 0.989 | 0.992 | 0.997 | 9.591 | 9.371 | 2.329 |
| unreachable | 0.990 | 0.984 | 0.973 | 0.997 | 12.018 | 12.146 | 3.820 |
| frozen | 0.998 | 0.977 | 0.946 | 0.997 | 200.664 | 125.803 | 2.286 |

## Empty-dictionary repeat

Fifteen additional paired cycles repeat the two empty-dictionary allocation controls after the initial measurements. These ratios retain pairing within each cycle. The normal-collection slowdown repeats; the GC-disabled result changes direction between runs. Both runs and every sample remain available. No cause has been established.

| Workload | Time/previous | CPU/previous | RSS/previous | Time/CPython | RSS/CPython |
|---|---:|---:|---:|---:|---:|
| retained_dicts | 1.084 | 1.056 | 0.976 | 11.430 | 2.723 |
| retained_dicts_gc_disabled | 0.973 | 0.996 | 0.974 | 9.852 | 2.544 |

## Cold regression controls

Nine paired cycles compare lazy instance dictionaries, this release, and CPython. Warm controls execute the workload once before timing it. Cold denotes the absence of that explicit workload warmup, not a flushed operating-system page cache.

| Fixture | JIT/previous | Interpreter/previous | JIT RSS/previous | Interpreter RSS/previous | JIT/CPython | JIT RSS/CPython |
|---|---:|---:|---:|---:|---:|---:|
| str_methods | 0.995 | 0.989 | 0.995 | 0.999 | 3.114 | 2.074 |
| list_ops | 1.000 | 1.000 | 1.001 | 1.001 | 13.949 | 1.983 |
| attr_access | 1.009 | 1.000 | 0.998 | 1.000 | 3.055 | 2.119 |
| call_overhead | 0.990 | 0.999 | 0.999 | 0.999 | 8.839 | 2.047 |
| jitkernels | 1.006 | 1.001 | 0.995 | 1.001 | 0.872 | 1.995 |

## Warm regression controls

Nine paired cycles compare lazy instance dictionaries, this release, and CPython. Warm controls execute the workload once before timing it. Cold denotes the absence of that explicit workload warmup, not a flushed operating-system page cache.

| Fixture | JIT/previous | Interpreter/previous | JIT RSS/previous | Interpreter RSS/previous | JIT/CPython | JIT RSS/CPython |
|---|---:|---:|---:|---:|---:|---:|
| str_methods | 0.985 | 0.978 | 0.997 | 0.999 | 2.774 | 3.838 |
| list_ops | 0.995 | 1.047 | 1.002 | 1.001 | 13.875 | 1.956 |
| attr_access | 1.007 | 0.995 | 1.000 | 1.000 | 2.961 | 2.086 |
| call_overhead | 1.000 | 0.991 | 0.997 | 1.001 | 8.883 | 2.008 |
| jitkernels | 1.001 | 0.987 | 1.001 | 1.001 | 0.847 | 1.960 |

## Startup

| Workload | Time/checkpoint | RSS/checkpoint | Time/CPython | RSS/CPython |
|---|---:|---:|---:|---:|
| startup | 1.019 | 0.955 | 1.394 | 1.853 |
| startup_no_site | 1.024 | 1.003 | 0.604 | 1.552 |
| imports | 0.923 | 0.844 | 2.886 | 2.468 |

## Probes

| Workload | Time/checkpoint | RSS/checkpoint | Time/CPython | RSS/CPython |
|---|---:|---:|---:|---:|
| plain_instances | 0.891 | 0.870 | 17.514 | 3.835 |
| slotted_instances | 0.738 | 0.600 | 17.126 | 3.077 |
| memoryviews | 0.929 | 0.723 | 4.131 | 1.514 |
| memoryview_access | 0.945 | 0.956 | 6.253 | 1.868 |
| materialized_frames | 0.961 | 0.882 | 7.894 | 2.055 |
| bytesio_streams | 0.910 | 0.826 | 10.464 | 2.917 |
| type_creation | 0.992 | 0.931 | 1.267 | 1.179 |
| float_repr | 0.470 | 0.960 | 1.023 | 1.636 |
| float_str | 0.453 | 0.957 | 1.055 | 1.633 |
| int_repr | 0.805 | 0.957 | 3.961 | 1.636 |
| complex_repr | 0.376 | 0.957 | 0.767 | 1.667 |
| json_float_array | 0.148 | 0.851 | 0.227 | 2.179 |
| json_int_array | 0.292 | 0.857 | 0.734 | 2.156 |
| json_repeated_keys | 0.713 | 0.887 | 1.039 | 2.308 |
| json_unique_keys | 0.864 | 0.826 | 1.167 | 2.266 |

## Parallel execution

CPython is the installed GIL build. WeavePy modes refer to its GIL setting; this is not a free-threaded CPython comparison. CPU time includes all participating threads. WeavePy disables tier-2 native execution when free threading is requested, so its GIL-disabled rows run the interpreter.

| GIL setting | Parallel time/previous | Parallel CPU/previous | Peak RSS/previous | Parallel time/CPython | Parallel CPU/CPython | Peak RSS/CPython |
|---|---:|---:|---:|---:|---:|---:|
| 1 | 0.999 | 0.999 | 0.997 | 0.114 | 0.115 | 3.428 |
| 0 | 0.962 | 0.965 | 0.999 | 0.608 | 4.310 | 3.202 |

### Independent class mutations

Eight threads update and read separate classes. Five cycles compare this release with lazy instance dictionaries. All workers share the same kernel code while receiver classes vary.

| GIL setting | Parallel time/previous | Parallel CPU/previous | Peak RSS/previous | Parallel time/CPython | Peak RSS/CPython |
|---|---:|---:|---:|---:|---:|
| 1 | 1.007 | 1.007 | 0.998 | 6.158 | 3.511 |
| 0 | 0.998 | 1.009 | 0.997 | 7.783 | 3.285 |

Build latency was not measured under controlled conditions. No build-time improvement is claimed. All regressions, raw samples, source hashes, execution proofs, and intermediate reports remain available.

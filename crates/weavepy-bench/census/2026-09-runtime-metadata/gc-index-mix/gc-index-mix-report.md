# Distribute aligned addresses across the GC index

The private collector index now mixes the high half of its word hash into the low half. The existing final multiplication preserves allocation-alignment bits, concentrating home buckets for regularly spaced object addresses. The final XOR spreads those buckets while preserving the full 64-bit hash one-to-one. Python dictionary and set hashing, object identity, index locking, and collector ownership remain unchanged.

Measured executable: `target/release/weavepy-runtime-gc-index-mix`, SHA-256 `8e86b528cdc3dd1f5c5db14e8a76fc111eba277dd2cabbfba76de28eb3874cd9`, 44,194,128 bytes. The paired preceding release is the native slice repair, SHA-256 `9f0745479837e48c31b46957bce04a23cb3ba0108c36f4758743013287e7e092`. The original PR checkpoint is `9a69c4161ade6527a5f4457d7e7272075cb1f0d9`.

All 229 compatibility checks pass, including the native slice repair. All 303 VM and 52 JIT tests pass, along with Clippy, workspace/all-feature/no-JIT checks, and debug/release lifecycle checks. The new alignment regression exercises multiple allocation strides and address regions. Existing checks cover rooted and frozen cycles, weak references, finalizers, generation promotion, cross-thread heaps, and repeated removals.

No new unsafe code or public Rust API change is introduced. Matching release layouts remain unchanged, including 80-byte tracked handles, 24-byte Object, 176-byte instance headers, and 480-byte CodeObject. Process measurements below determine whether allocation behavior changes peak memory.

## Index experiment

The isolated Rust benchmark uses 12 captured sets of 100,000 retained dictionary addresses from two earlier releases. Fifteen alternating paired cycles follow a discarded cycle. It measures contains-and-insert, successful lookup, and reverse removal, verifying contents and checksums. The map has the same key/value widths as the collector index. This experiment is not VM speed or proof of the cause of an earlier empty-dictionary regression.

| Address set | Insert ratio | Lookup ratio | Remove ratio | Combined time ratio |
|---|---:|---:|---:|---:|
| base_False_0-addresses | 0.423 | 0.402 | 1.585 | 0.583 |
| base_False_1-addresses | 0.415 | 0.378 | 1.529 | 0.574 |
| base_False_2-addresses | 0.403 | 0.394 | 1.520 | 0.561 |
| base_True_0-addresses | 0.405 | 0.394 | 1.573 | 0.567 |
| base_True_1-addresses | 0.436 | 0.404 | 1.658 | 0.614 |
| base_True_2-addresses | 0.410 | 0.386 | 1.569 | 0.574 |
| new_False_0-addresses | 0.440 | 0.373 | 1.549 | 0.595 |
| new_False_1-addresses | 0.262 | 0.208 | 0.794 | 0.340 |
| new_False_2-addresses | 0.428 | 0.364 | 1.617 | 0.592 |
| new_True_0-addresses | 0.439 | 0.383 | 1.588 | 0.606 |
| new_True_1-addresses | 0.249 | 0.183 | 0.766 | 0.327 |
| new_True_2-addresses | 0.460 | 0.404 | 1.589 | 0.627 |

Insertion and lookup improve in the isolated test, while removal becomes slower for most captured sets. The following VM measurements include their combined effects and all observed regressions.

## Standard suite

Five alternating paired cycles retain all 24 fixtures and their work sizes. Workload time excludes startup, leaving 23 workload timers; process elapsed time, CPU time, and peak RSS include startup. Aggregates are geometric means of per-fixture median paired ratios. Ratios below one mean less time or memory.

| Metric | JIT/checkpoint | Interpreter/checkpoint | JIT/previous | Interpreter/previous | JIT/CPython | Interpreter/CPython |
|---|---:|---:|---:|---:|---:|---:|
| ns | 0.845 | 0.973 | 0.995 | 0.995 | 3.293 | 8.803 |
| wall_ns | 0.904 | 0.984 | 0.989 | 0.998 | 3.198 | 5.551 |
| cpu_ns | 0.902 | 0.984 | 0.990 | 0.998 | 3.267 | 5.728 |
| rss_bytes | 0.947 | 0.942 | 1.001 | 1.004 | 2.096 | 1.932 |

JIT workload-time wins against CPython: 6/23. Peak-RSS wins: 0/24. The objective of superiority across every meaningful workload and metric remains unachieved.

| Fixture | JIT time/previous | Interpreter time/previous | JIT CPU/previous | JIT RSS/previous | JIT time/CPython | JIT RSS/CPython |
|---|---:|---:|---:|---:|---:|---:|
| fannkuch | 0.986 | 0.987 | 0.983 | 0.998 | 10.299 | 1.986 |
| nbody | 1.000 | 1.006 | 1.000 | 0.999 | 8.943 | 1.978 |
| fib | 1.005 | 1.005 | 0.997 | 1.004 | 3.027 | 2.009 |
| pidigits | 1.000 | 1.005 | 0.999 | 0.998 | 0.888 | 1.991 |
| pyaes | 0.991 | 0.949 | 0.980 | 1.005 | 0.660 | 1.997 |
| richards | 1.005 | 1.011 | 1.004 | 0.998 | 8.153 | 1.989 |
| sumvm | 0.990 | 0.996 | 0.982 | 0.998 | 0.057 | 2.001 |
| nested_loops | 0.996 | 0.993 | 0.975 | 1.005 | 0.082 | 2.012 |
| jitloop | 0.998 | 0.999 | 0.976 | 1.001 | 0.074 | 2.011 |
| jitkernels | 0.993 | 1.002 | 0.981 | 1.001 | 0.859 | 1.999 |
| deltablue | 0.998 | 0.996 | 0.999 | 1.006 | 19.378 | 2.176 |
| float_math | 0.947 | 0.958 | 0.948 | 1.002 | 7.456 | 3.044 |
| spectral_norm | 1.009 | 1.004 | 0.998 | 0.999 | 2.179 | 2.005 |
| json_bench | 0.995 | 0.999 | 0.994 | 1.001 | 1.149 | 2.532 |
| str_methods | 0.983 | 0.998 | 0.986 | 1.003 | 3.149 | 2.085 |
| dict_ops | 0.996 | 0.992 | 0.996 | 0.999 | 5.477 | 1.971 |
| list_ops | 0.999 | 0.996 | 0.998 | 0.999 | 13.846 | 1.981 |
| attr_access | 1.002 | 1.003 | 0.997 | 1.005 | 3.010 | 2.126 |
| call_overhead | 1.010 | 1.002 | 1.009 | 1.002 | 9.025 | 2.042 |
| generators | 0.993 | 0.993 | 0.988 | 1.002 | 9.642 | 2.006 |
| deque_ops | 1.002 | 1.006 | 1.002 | 1.003 | 16.753 | 2.030 |
| datetime_ops | 0.988 | 1.004 | 0.988 | 1.000 | 15.906 | 2.161 |
| pickle_bench | 0.996 | 0.983 | 0.997 | 1.003 | 338.233 | 2.471 |
| startup | 0.985 | 1.024 | 0.986 | 1.001 | 1.486 | 1.986 |

The illustrative fannkuch fixture is not canonical fannkuch; pyaes is an XOR scrambler. Earlier full censuses showed substantial unexplained CPython datetime timing variation despite matching recorded executable, library, module, and harness hashes. All samples remain available; changes in CPython-relative ratios between stages alone do not establish a WeavePy improvement. Paired comparisons within this stage are reported above.

## Focused measurements

Dictionary and set allocations are repeated for 15 paired cycles after the initial nine-cycle controls, retaining both normal and disabled-GC cases. This repeat checks the large improvements against the address-layout variability observed in earlier releases. Both runs remain available.

Focused allocation and GC controls use interpreter mode. Native-specific controls retain separate interpreter samples. Ratios below are medians of paired cycle ratios. CPU time and peak RSS cover the process. Cold means no explicit workload warmup, not flushed operating-system caches; warm controls run the workload before timing.

### allocation-repeat

15 measured cycles; every raw sample is retained.

| Workload | Time/previous | CPU/previous | RSS/previous | Time/CPython | RSS/CPython |
|---|---:|---:|---:|---:|---:|
| retained_dicts | 0.971 | 0.978 | 1.001 | 10.145 | 2.717 |
| retained_dicts_gc_disabled | 0.776 | 0.884 | 1.002 | 8.829 | 2.540 |
| retained_sets | 0.920 | 0.941 | 1.000 | 3.535 | 1.470 |
| retained_sets_gc_disabled | 0.814 | 0.887 | 1.001 | 6.078 | 1.470 |

### code-cache-probes

9 measured cycles; every raw sample is retained.

| Workload | Time/previous | CPU/previous | RSS/previous | Time/CPython | RSS/CPython |
|---|---:|---:|---:|---:|---:|
| retained_cold_code | 0.999 | 0.995 | 1.004 | 0.968 | 1.937 |
| retained_warm_code | 0.999 | 0.995 | 1.000 | 1.008 | 2.087 |
| code_compile_churn | 1.005 | 0.995 | 1.001 | 1.001 | 2.141 |
| class_version_churn | 0.991 | 0.996 | 1.005 | 5.993 | 1.857 |
| type_creation | 0.958 | 0.966 | 1.001 | 1.207 | 1.179 |

### controls

9 measured cycles; every raw sample is retained.

| Workload | Time/previous | CPU/previous | RSS/previous | Time/CPython | RSS/CPython |
|---|---:|---:|---:|---:|---:|
| str_methods | 0.990 | 0.991 | 1.008 | 3.089 | 2.087 |
| list_ops | 0.996 | 0.998 | 1.002 | 13.911 | 1.981 |
| attr_access | 0.999 | 1.005 | 1.004 | 3.036 | 2.119 |
| call_overhead | 0.997 | 0.999 | 1.006 | 9.097 | 2.044 |
| jitkernels | 1.001 | 1.013 | 1.003 | 0.871 | 1.996 |

### gc-index-mix-probes

9 measured cycles; every raw sample is retained.

| Workload | Time/previous | CPU/previous | RSS/previous | Time/CPython | RSS/CPython |
|---|---:|---:|---:|---:|---:|
| retained_lists | 0.983 | 0.988 | 1.002 | 9.955 | 2.579 |
| retained_lists_gc_disabled | 0.943 | 0.954 | 1.000 | 8.819 | 2.388 |
| retained_dicts | 0.926 | 0.937 | 1.002 | 9.675 | 2.721 |
| retained_dicts_gc_disabled | 0.783 | 0.865 | 1.002 | 8.726 | 2.536 |
| retained_sets | 0.823 | 0.897 | 1.001 | 3.483 | 1.470 |
| retained_sets_gc_disabled | 0.823 | 0.886 | 1.003 | 5.878 | 1.469 |
| retained_tuples | 0.986 | 0.985 | 0.963 | 8.171 | 2.618 |
| retained_tuples_gc_disabled | 0.967 | 0.973 | 1.002 | 7.331 | 2.278 |
| rooted_full_scans | 1.017 | 0.994 | 1.000 | 8.403 | 1.823 |
| unreachable_cycle_scans | 0.974 | 0.980 | 1.002 | 18.694 | 3.500 |
| freeze_unfreeze_scans | 0.996 | 0.977 | 1.001 | 3.533 | 1.711 |

### Explicit GC pauses

Seven alternating process cycles collect a 10,000-node graph 51 times after one discarded collection. Automatic GC is disabled. p95 is the 49th ordered pause of 51. Ratios compare paired per-process summaries, including per-process maxima. These are explicit collection pauses, not application-wide or automatic-GC tail latency.

| Graph | Median/previous | p95/previous | Maximum/previous | RSS/previous | Median/CPython | p95/CPython | RSS/CPython |
|---|---:|---:|---:|---:|---:|---:|---:|
| rooted | 0.996 | 0.996 | 0.995 | 0.999 | 9.683 | 9.428 | 2.323 |
| unreachable | 0.988 | 0.986 | 1.005 | 0.999 | 11.706 | 11.889 | 3.802 |
| frozen | 0.968 | 0.936 | 1.013 | 1.000 | 193.664 | 128.005 | 2.274 |

### instance-dict-probes

9 measured cycles; every raw sample is retained.

| Workload | Time/previous | CPU/previous | RSS/previous | Time/CPython | RSS/CPython |
|---|---:|---:|---:|---:|---:|
| retained_cold_plain | 0.921 | 0.934 | 1.002 | 13.901 | 2.928 |
| retained_cold_empty_slots | 0.924 | 0.935 | 1.001 | 16.415 | 3.674 |
| retained_exported_empty_dict | 0.937 | 0.941 | 1.001 | 9.708 | 2.637 |
| retained_populated_dict | 0.932 | 0.941 | 1.000 | 14.014 | 3.826 |
| retained_cold_native_int | 0.927 | 0.937 | 1.001 | 15.483 | 3.151 |
| retained_cold_native_list | 0.937 | 0.949 | 1.001 | 13.686 | 3.003 |
| churn_plain | 0.970 | 0.977 | 1.005 | 14.335 | 1.862 |
| churn_empty_slots | 0.970 | 0.983 | 1.003 | 17.911 | 1.859 |
| class_attribute_cold_dict | 1.007 | 0.987 | 1.002 | 10.610 | 1.858 |
| method_cold_dict | 1.013 | 1.004 | 1.002 | 14.720 | 1.853 |

### jit-slot-probes

9 measured cycles; every raw sample is retained.

| Workload | Time/previous | CPU/previous | RSS/previous | Time/CPython | RSS/CPython |
|---|---:|---:|---:|---:|---:|
| native_slots_1 | 0.999 | 1.002 | 1.002 | 1.254 | 1.976 |
| native_slots_8 | 0.998 | 1.016 | 1.004 | 1.236 | 1.973 |
| native_slots_16 | 1.010 | 1.012 | 1.004 | 1.258 | 1.971 |
| native_alternating_slot_orders | 1.009 | 1.009 | 1.001 | 1.365 | 1.964 |

### scalar-result-probes

9 measured cycles; every raw sample is retained.

| Workload | Time/previous | CPU/previous | RSS/previous | Time/CPython | RSS/CPython |
|---|---:|---:|---:|---:|---:|
| integer_callback_sum | 0.999 | 0.999 | 1.003 | 2.407 | 1.977 |
| integer_callback_left | 0.998 | 1.002 | 1.003 | 2.359 | 1.971 |
| keyword_callback_sum | 0.974 | 0.978 | 1.003 | 13.965 | 1.974 |
| callable_instance_sum | 1.005 | 1.004 | 1.002 | 9.969 | 1.974 |

### slot-index-probes

9 measured cycles; every raw sample is retained.

| Workload | Time/previous | CPU/previous | RSS/previous | Time/CPython | RSS/CPython |
|---|---:|---:|---:|---:|---:|
| retained_slots_1 | 0.950 | 0.954 | 1.001 | 15.513 | 3.067 |
| last_slot_access_1 | 1.004 | 0.996 | 1.002 | 7.811 | 1.850 |
| retained_slots_2 | 0.942 | 0.947 | 1.002 | 15.011 | 3.177 |
| last_slot_access_2 | 1.005 | 0.988 | 1.003 | 7.922 | 1.855 |
| retained_slots_8 | 0.949 | 0.950 | 1.001 | 13.732 | 2.640 |
| last_slot_access_8 | 1.007 | 0.988 | 1.004 | 7.788 | 1.851 |
| retained_slots_9 | 0.950 | 0.953 | 1.001 | 12.701 | 2.971 |
| last_slot_access_9 | 1.001 | 0.990 | 1.001 | 7.717 | 1.847 |
| retained_slots_16 | 0.963 | 0.963 | 1.000 | 14.151 | 4.037 |
| last_slot_access_16 | 1.008 | 1.000 | 1.006 | 7.917 | 1.851 |
| slot_delete_reinsert | 0.997 | 0.988 | 1.001 | 14.057 | 1.852 |
| slot_access_8_index_0 | 0.993 | 0.989 | 1.004 | 7.779 | 1.853 |
| slot_access_8_index_3 | 1.006 | 0.991 | 1.005 | 7.901 | 1.851 |
| alternating_slot_orders | 1.004 | 1.000 | 1.003 | 15.366 | 1.850 |

### type-cache-probes

9 measured cycles; every raw sample is retained.

| Workload | Time/previous | CPU/previous | RSS/previous | Time/CPython | RSS/CPython |
|---|---:|---:|---:|---:|---:|
| cached_short_attribute | 0.991 | 0.998 | 1.001 | 8.996 | 1.973 |
| cached_missing_attribute | 0.998 | 0.998 | 1.000 | 31.721 | 1.972 |
| cached_long_attribute | 0.999 | 0.998 | 0.998 | 8.569 | 1.972 |
| many_class_namespaces | 0.996 | 0.999 | 1.003 | 7.986 | 1.863 |

### warm

9 measured cycles; every raw sample is retained.

| Workload | Time/previous | CPU/previous | RSS/previous | Time/CPython | RSS/CPython |
|---|---:|---:|---:|---:|---:|
| str_methods | 0.996 | 0.996 | 1.002 | 2.759 | 3.836 |
| list_ops | 1.001 | 1.003 | 1.001 | 13.901 | 1.952 |
| attr_access | 1.001 | 1.001 | 1.006 | 2.921 | 2.090 |
| call_overhead | 1.004 | 1.006 | 1.008 | 9.040 | 2.013 |
| jitkernels | 0.996 | 1.006 | 1.003 | 0.833 | 1.958 |

## probes

7 measured cycles.

| Workload | Time/checkpoint | CPU/checkpoint | RSS/checkpoint | Time/CPython | CPU/CPython | RSS/CPython |
|---|---:|---:|---:|---:|---:|---:|
| plain_instances | 0.815 | 0.832 | 0.869 | 15.642 | 7.517 | 3.826 |
| slotted_instances | 0.695 | 0.721 | 0.598 | 16.227 | 7.240 | 3.067 |
| memoryviews | 0.925 | 0.946 | 0.722 | 4.169 | 2.838 | 1.513 |
| memoryview_access | 0.963 | 0.988 | 0.953 | 6.233 | 2.622 | 1.856 |
| materialized_frames | 0.976 | 0.993 | 0.879 | 8.181 | 2.368 | 2.049 |
| bytesio_streams | 0.889 | 0.944 | 0.825 | 10.389 | 2.697 | 2.921 |
| type_creation | 0.945 | 0.969 | 0.931 | 1.204 | 1.151 | 1.179 |
| float_repr | 0.465 | 0.652 | 0.958 | 1.017 | 1.246 | 1.630 |
| float_str | 0.459 | 0.636 | 0.958 | 1.048 | 1.259 | 1.638 |
| int_repr | 0.813 | 0.912 | 0.954 | 3.954 | 2.047 | 1.632 |
| complex_repr | 0.375 | 0.545 | 0.953 | 0.767 | 1.048 | 1.669 |
| json_float_array | 0.148 | 0.476 | 0.845 | 0.229 | 0.974 | 2.169 |
| json_int_array | 0.296 | 0.702 | 0.852 | 0.727 | 2.127 | 2.135 |
| json_repeated_keys | 0.699 | 0.817 | 0.851 | 1.019 | 1.677 | 2.215 |
| json_unique_keys | 0.864 | 0.897 | 0.825 | 1.174 | 1.947 | 2.271 |

## startup

31 measured cycles.

| Workload | Time/checkpoint | CPU/checkpoint | RSS/checkpoint | Time/CPython | CPU/CPython | RSS/CPython |
|---|---:|---:|---:|---:|---:|---:|
| startup | 1.012 | 1.014 | 0.952 | 1.419 | 1.474 | 1.851 |
| startup_no_site | 1.035 | 1.041 | 1.001 | 0.605 | 0.581 | 1.545 |
| imports | 0.921 | 0.920 | 0.842 | 2.894 | 3.075 | 2.460 |

## parallel

5 measured cycles.

| Workload | Time/previous | CPU/previous | RSS/previous | Time/CPython | CPU/CPython | RSS/CPython |
|---|---:|---:|---:|---:|---:|---:|
| 1 | 1.004 | 1.004 | 1.001 | 0.119 | 0.119 | 3.433 |
| 0 | 0.978 | 1.012 | 0.999 | 0.764 | 5.664 | 3.197 |

## class-parallel

5 measured cycles.

| Workload | Time/previous | CPU/previous | RSS/previous | Time/CPython | CPU/CPython | RSS/CPython |
|---|---:|---:|---:|---:|---:|---:|
| 1 | 1.000 | 1.000 | 1.000 | 6.228 | 6.208 | 3.525 |
| 0 | 1.007 | 1.001 | 0.997 | 7.780 | 59.882 | 3.270 |

CPython is the installed GIL build; a free-threaded CPython comparison is unavailable. WeavePy GIL-disabled measurements use the interpreter because native execution remains gated. CPU time includes participating threads. No complete free-threaded safety claim is made.

Build latency was not measured under controlled conditions. No build-time improvement is claimed. Frozen source and measurement inputs, release and library hashes, validation output, isolated index samples, all VM measurements, and regressions are preserved in the census archive.

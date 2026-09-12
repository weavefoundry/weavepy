# Allocate instance dictionaries on demand

Instances begin without an allocated attribute dictionary. Paths that need dictionary storage publish one owning Arc through an atomic pointer; concurrent losing allocations are released. Ordinary attribute and method lookups, garbage collection, and empty-state queries preserve an unused dictionary's unallocated state. Exported handles keep stable identity and ownership. Native and internal objects can still initialize dictionary state when required.

Measured executable: `target/release/weavepy-runtime-lazy-instance-dicts`, SHA-256 `67e7ce37fdec5fe38c99753f7dd0e918117d3816c7303eef0221ffc825a97ebe`, 44,193,984 bytes.

The measured instance header remains 176 bytes. Lazy ownership occupies one eight-byte pointer on this host, avoiding an unused 80-byte dictionary cell plus Arc reference-count storage. CodeObject remains 480 bytes and CacheSlot remains 16 bytes. Peak-memory measurements below include allocator behavior, startup, populated dictionaries, and delayed initialization costs.

The checkpoint is `9a69c4161ade6527a5f4457d7e7272075cb1f0d9`. The paired preceding release is coherent cache slots, identified in the frozen environment. All 223 compatibility checks pass, along with 298 VM tests, 49 JIT tests, Clippy, workspace/feature checks, and debug/release execution checks. The Python regression covers dictionary identity and lifetime, slot layouts, native subclasses, copy/pickle, class reassignment, concurrent first exports, weak references, and finalizers.

Seven ownership tests using the actual production LazyArc module pass with Rust 1.93 and Miri strict provenance under default and Tree Borrows on ARM64 and i686. Integration tests inspect actual instance storage after ordinary and native-enabled execution, mixed populated/cold cache accesses, and garbage collection. They require cold dictionaries to remain absent and written/exported dictionaries to be initialized. These tests do not establish complete free-threaded VM safety; native execution with the GIL disabled remains gated.

The public Rust PyInstance.dict field now uses sync::LazyArc. Existing borrow calls continue through Deref; Rust struct initializers can convert an existing Arc with .into(), and callers needing an exported Arc use .share(). Python and C-API exports preserve the underlying dictionary identity. Other internal read paths may initialize storage through Deref.

## Standard suite

Ratios below one mean less time or memory. Five alternating measured cycles retain every existing fixture and work parameter. Workload time excludes startup; process elapsed time, process CPU time, and peak RSS include it. Startup has no separate workload timer, leaving 23 workload-time comparisons. The table aggregates paired cycle ratios geometrically.

| Metric | JIT/checkpoint | Interpreter/checkpoint | JIT/previous | Interpreter/previous | JIT/CPython | Interpreter/CPython |
|---|---:|---:|---:|---:|---:|---:|
| Workload time | 0.847 | 0.972 | 0.998 | 0.999 | 3.641 | 9.686 |
| Process elapsed time | 0.905 | 0.982 | 0.991 | 1.003 | 3.456 | 5.962 |
| Process CPU time | 0.904 | 0.983 | 0.991 | 1.002 | 3.542 | 6.165 |
| Peak RSS | 0.950 | 0.945 | 0.998 | 1.007 | 2.103 | 1.936 |

JIT workload-time wins against CPython: 6/23. Peak-RSS wins: 0/24. These results do not establish superiority across all meaningful workloads or metrics.

| Fixture | JIT time/previous | Interpreter time/previous | JIT time/CPython | JIT RSS/previous | JIT RSS/CPython |
|---|---:|---:|---:|---:|---:|
| fannkuch | 0.993 | 0.980 | 10.256 | 1.003 | 1.984 |
| nbody | 0.995 | 1.000 | 8.871 | 1.005 | 1.982 |
| fib | 1.013 | 1.003 | 2.968 | 1.003 | 2.011 |
| pidigits | 1.003 | 1.002 | 0.889 | 1.002 | 2.007 |
| pyaes | 1.006 | 1.013 | 0.663 | 1.002 | 1.996 |
| richards | 0.998 | 0.984 | 8.282 | 1.004 | 2.001 |
| sumvm | 0.995 | 0.994 | 0.057 | 1.005 | 2.012 |
| nested_loops | 0.989 | 1.002 | 0.081 | 1.003 | 2.006 |
| jitloop | 0.986 | 0.998 | 0.073 | 1.002 | 2.019 |
| jitkernels | 1.056 | 0.994 | 0.862 | 1.003 | 1.995 |
| deltablue | 1.009 | 0.996 | 19.641 | 1.008 | 2.181 |
| float_math | 1.006 | 1.006 | 7.918 | 1.000 | 3.093 |
| spectral_norm | 0.995 | 1.004 | 2.140 | 1.005 | 2.018 |
| json_bench | 1.020 | 1.022 | 1.167 | 0.941 | 2.531 |
| str_methods | 0.982 | 1.032 | 3.132 | 1.001 | 2.082 |
| dict_ops | 0.996 | 0.994 | 5.481 | 1.001 | 1.982 |
| list_ops | 0.998 | 1.003 | 13.949 | 0.999 | 1.980 |
| attr_access | 0.948 | 0.991 | 3.009 | 1.002 | 2.123 |
| call_overhead | 0.984 | 0.994 | 9.043 | 1.003 | 2.053 |
| generators | 1.000 | 0.994 | 9.657 | 1.005 | 2.014 |
| deque_ops | 0.998 | 0.996 | 17.012 | 1.003 | 2.034 |
| datetime_ops | 0.987 | 0.991 | 152.832 | 1.005 | 2.169 |
| pickle_bench | 0.994 | 0.982 | 332.605 | 0.942 | 2.484 |
| startup | 0.991 | 1.033 | 1.428 | 1.003 | 2.002 |

The illustrative `fannkuch` loop is not canonical fannkuch; `pyaes` is an XOR scrambler. The unchanged fixture set includes standard-library workloads with native CPython implementations. CPython datetime measurements varied substantially between earlier stages; no cause has been established, and all samples remain.

## Focused controls

All focused workloads use nine cycles and the preserved coherent-slot release as their baseline. Tables below use ratios of sample medians. CPU time and peak RSS cover the whole process. The allocation and code-storage probes use interpreter mode; the native slot, exact-name cache, and scalar-call probes also retain separate interpreter controls in their raw files.

### instance-dict-probes

| Workload | Interpreter time/base | Interpreter CPU/base | Interpreter RSS/base | Interpreter time/CPython | Interpreter RSS/CPython |
|---|---:|---:|---:|---:|---:|
| retained_cold_plain | 0.819 | 0.840 | 0.883 | 15.307 | 3.005 |
| retained_cold_empty_slots | 0.809 | 0.832 | 0.883 | 17.846 | 3.757 |
| retained_exported_empty_dict | 0.899 | 0.912 | 1.001 | 10.541 | 2.689 |
| retained_populated_dict | 0.890 | 0.901 | 1.001 | 14.977 | 3.884 |
| retained_cold_native_int | 0.823 | 0.846 | 0.882 | 16.720 | 3.219 |
| retained_cold_native_list | 0.837 | 0.860 | 0.891 | 14.706 | 3.070 |
| churn_plain | 0.953 | 0.964 | 1.005 | 14.879 | 1.861 |
| churn_empty_slots | 0.953 | 0.966 | 1.004 | 18.691 | 1.858 |
| class_attribute_cold_dict | 0.941 | 0.979 | 1.002 | 10.695 | 1.865 |
| method_cold_dict | 0.976 | 0.987 | 0.997 | 13.920 | 1.853 |

### code-cache-probes

| Workload | Interpreter time/base | Interpreter CPU/base | Interpreter RSS/base | Interpreter time/CPython | Interpreter RSS/CPython |
|---|---:|---:|---:|---:|---:|
| retained_cold_code | 1.002 | 1.001 | 1.006 | 0.964 | 1.943 |
| retained_warm_code | 1.006 | 1.000 | 1.005 | 1.002 | 2.095 |
| code_compile_churn | 1.005 | 1.004 | 1.002 | 0.996 | 2.141 |
| class_version_churn | 0.994 | 0.993 | 1.005 | 6.265 | 1.861 |
| type_creation | 1.007 | 1.008 | 1.003 | 1.281 | 1.184 |

### type-cache-probes

| Workload | JIT time/base | JIT CPU/base | JIT RSS/base | JIT time/CPython | JIT RSS/CPython |
|---|---:|---:|---:|---:|---:|
| cached_short_attribute | 0.980 | 0.982 | 1.003 | 8.957 | 1.979 |
| cached_missing_attribute | 0.970 | 0.973 | 1.004 | 31.289 | 1.984 |
| cached_long_attribute | 0.959 | 0.971 | 1.002 | 8.598 | 1.979 |
| many_class_namespaces | 0.972 | 0.980 | 1.006 | 7.668 | 1.869 |

### slot-index-probes

| Workload | Interpreter time/base | Interpreter CPU/base | Interpreter RSS/base | Interpreter time/CPython | Interpreter RSS/CPython |
|---|---:|---:|---:|---:|---:|
| retained_slots_1 | 0.889 | 0.900 | 0.887 | 16.352 | 3.129 |
| last_slot_access_1 | 1.010 | 0.995 | 1.005 | 7.811 | 1.855 |
| retained_slots_2 | 0.898 | 0.908 | 0.902 | 15.810 | 3.239 |
| last_slot_access_2 | 1.005 | 1.005 | 1.005 | 7.899 | 1.855 |
| retained_slots_8 | 0.934 | 0.939 | 0.935 | 14.335 | 2.670 |
| last_slot_access_8 | 1.002 | 0.994 | 1.004 | 7.682 | 1.851 |
| retained_slots_9 | 0.937 | 0.940 | 0.946 | 13.312 | 2.999 |
| last_slot_access_9 | 1.006 | 1.005 | 1.005 | 7.612 | 1.855 |
| retained_slots_16 | 0.958 | 0.961 | 0.972 | 14.639 | 4.055 |
| last_slot_access_16 | 0.997 | 0.999 | 1.002 | 7.762 | 1.851 |
| slot_delete_reinsert | 1.000 | 0.999 | 1.004 | 14.295 | 1.852 |
| slot_access_8_index_0 | 1.012 | 0.999 | 1.005 | 7.900 | 1.858 |
| slot_access_8_index_3 | 0.997 | 0.999 | 1.005 | 7.711 | 1.858 |
| alternating_slot_orders | 1.007 | 1.001 | 1.003 | 15.406 | 1.852 |

### jit-slot-probes

| Workload | JIT time/base | JIT CPU/base | JIT RSS/base | JIT time/CPython | JIT RSS/CPython |
|---|---:|---:|---:|---:|---:|
| native_slots_1 | 0.997 | 1.016 | 1.005 | 1.199 | 1.983 |
| native_slots_8 | 1.002 | 1.011 | 1.006 | 1.235 | 1.981 |
| native_slots_16 | 0.998 | 1.004 | 1.004 | 1.245 | 1.980 |
| native_alternating_slot_orders | 1.007 | 1.009 | 1.004 | 1.331 | 1.975 |

### scalar-result-probes

| Workload | JIT time/base | JIT CPU/base | JIT RSS/base | JIT time/CPython | JIT RSS/CPython |
|---|---:|---:|---:|---:|---:|
| integer_callback_sum | 1.003 | 1.010 | 1.005 | 2.444 | 1.982 |
| integer_callback_left | 0.986 | 0.998 | 1.003 | 2.336 | 1.977 |
| keyword_callback_sum | 0.998 | 0.996 | 1.002 | 13.764 | 1.977 |
| callable_instance_sum | 0.998 | 0.997 | 1.004 | 10.031 | 1.977 |

## Cold regression controls

Nine paired cycles compare coherent cache slots, this release, and CPython. Warm controls execute the workload once before timing it. Cold denotes the absence of that explicit workload warmup, not a flushed operating-system page cache.

| Fixture | JIT/previous | Interpreter/previous | JIT RSS/previous | Interpreter RSS/previous | JIT/CPython | JIT RSS/CPython |
|---|---:|---:|---:|---:|---:|---:|
| str_methods | 1.014 | 1.003 | 1.003 | 1.002 | 3.197 | 2.087 |
| list_ops | 1.007 | 1.001 | 1.002 | 1.009 | 13.959 | 1.985 |
| attr_access | 0.941 | 0.996 | 1.005 | 1.006 | 3.024 | 2.128 |
| call_overhead | 1.002 | 0.997 | 1.006 | 1.005 | 9.117 | 2.054 |

## Warm regression controls

Nine paired cycles compare coherent cache slots, this release, and CPython. Warm controls execute the workload once before timing it. Cold denotes the absence of that explicit workload warmup, not a flushed operating-system page cache.

| Fixture | JIT/previous | Interpreter/previous | JIT RSS/previous | Interpreter RSS/previous | JIT/CPython | JIT RSS/CPython |
|---|---:|---:|---:|---:|---:|---:|
| str_methods | 0.997 | 1.015 | 1.001 | 1.005 | 2.803 | 3.854 |
| list_ops | 1.002 | 1.001 | 1.004 | 1.003 | 13.876 | 1.958 |
| attr_access | 0.950 | 0.999 | 1.004 | 1.004 | 2.947 | 2.093 |
| call_overhead | 0.994 | 0.993 | 1.004 | 1.002 | 8.912 | 2.017 |

## Startup

| Workload | Time/checkpoint | RSS/checkpoint | Time/CPython | RSS/CPython |
|---|---:|---:|---:|---:|
| startup | 1.016 | 0.953 | 1.409 | 1.852 |
| startup_no_site | 1.032 | 0.999 | 0.611 | 1.544 |
| imports | 0.941 | 0.849 | 2.936 | 2.479 |

## Probes

| Workload | Time/checkpoint | RSS/checkpoint | Time/CPython | RSS/CPython |
|---|---:|---:|---:|---:|
| plain_instances | 0.894 | 0.881 | 17.342 | 3.880 |
| slotted_instances | 0.740 | 0.612 | 17.226 | 3.133 |
| memoryviews | 0.924 | 0.722 | 4.199 | 1.510 |
| memoryview_access | 0.953 | 0.955 | 6.392 | 1.854 |
| materialized_frames | 0.987 | 0.879 | 8.236 | 2.046 |
| bytesio_streams | 0.903 | 0.827 | 10.419 | 2.920 |
| type_creation | 1.003 | 0.932 | 1.285 | 1.179 |
| float_repr | 0.467 | 0.956 | 1.013 | 1.631 |
| float_str | 0.452 | 0.957 | 1.043 | 1.630 |
| int_repr | 0.813 | 0.957 | 3.940 | 1.637 |
| complex_repr | 0.372 | 0.956 | 0.755 | 1.665 |
| json_float_array | 0.151 | 0.855 | 0.231 | 2.225 |
| json_int_array | 0.311 | 0.861 | 0.799 | 2.155 |
| json_repeated_keys | 0.717 | 0.896 | 1.057 | 2.316 |
| json_unique_keys | 0.881 | 0.836 | 1.189 | 2.293 |

## Parallel execution

CPython is the installed GIL build. WeavePy modes refer to its GIL setting; this is not a free-threaded CPython comparison. CPU time includes all participating threads. WeavePy disables tier-2 native execution when free threading is requested, so its GIL-disabled rows run the interpreter.

| GIL setting | Parallel time/previous | Parallel CPU/previous | Peak RSS/previous | Parallel time/CPython | Parallel CPU/CPython | Peak RSS/CPython |
|---|---:|---:|---:|---:|---:|---:|
| 1 | 1.002 | 1.002 | 1.004 | 0.118 | 0.118 | 3.451 |
| 0 | 1.033 | 1.011 | 1.003 | 0.638 | 4.515 | 3.197 |

### Independent class mutations

Eight threads update and read separate classes. Five cycles compare this release with coherent cache slots. All workers share the same kernel code while receiver classes vary.

| GIL setting | Parallel time/previous | Parallel CPU/previous | Peak RSS/previous | Parallel time/CPython | Peak RSS/CPython |
|---|---:|---:|---:|---:|---:|
| 1 | 1.005 | 1.005 | 1.003 | 6.173 | 3.535 |
| 0 | 0.979 | 0.988 | 1.003 | 7.731 | 3.297 |

Build latency was not measured under controlled conditions. No build-time improvement is claimed. All regressions, raw samples, source hashes, execution proofs, and intermediate reports remain available.

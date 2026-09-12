# Runtime metadata and numeric text performance

These changes reduce object metadata storage, dictionary bookkeeping, float-formatting allocations, and JSON key allocations. They also extend compiled enumeration, list builders, constant loads, and arithmetic on generic call results, and cache successful tuple hashes. WeavePy still does not outperform CPython across all workloads or all measured metrics.

The baseline is commit `9a69c4161ade6527a5f4457d7e7272075cb1f0d9`. The candidate uses the same release profile and default JIT feature on macOS ARM64, with CPython 3.14.7 as the reference. The standard suite also pairs the immediately preceding small-slot release with the candidate in the same measurement cycles. [Raw samples, checksums, and reproduction instructions](./) include intermediate stages as well as these measured results.

The measured executable is `target/release/weavepy-runtime-scalar-guards` with SHA-256 `66ddb00fa9f1a39b886c0f4f907ea6ef2cb583770d760de798e089629fef257f`.

## Changes

- Replace lock-backed cells for small copyable metadata with native atomic cells. Borrowed payloads and fields without native atomic support retain their existing locks, including fork recovery. A boolean cell occupies one byte and a 64-bit cell occupies eight.
- Keep dictionary comparison bookkeeping and built-in type registry handles in ordinary thread-local cells. Shared Python objects retain their synchronization, and nested key comparisons preserve outer errors.
- Format float digits with the already locked Zmij dependency, adapting its notation to Python's rules. Write exact JSON integers and finite floats directly into the output buffer. This also fixes shortest-decimal rounding ties that previously differed from CPython.
- Construct Python strings for native scalars directly from stack buffers. Scalar subclasses retain their protocol dispatch, and large integers retain their conversion limits.
- Look up unescaped UTF-8 JSON keys before allocating string storage. Escaped spellings still share identity with equal literal keys; surrogate-bearing strings keep their existing representation.
- Build decoded JSON dictionaries as fields arrive. Only the pairs hook stages a pair vector. Overwritten values are released before the next field, matching CPython's callback timing.
- Infer integer index and byte lanes for certified `enumerate(bytes)` calls. Generic enumeration retains an integer index and an object value. Existing runtime guards validate consumed values and preserve them when execution returns to the interpreter. Generic pair loops can keep immutable primitive values boxed without an immediate exit.
- Read native byte enumeration into the compiled pair lanes without allocating temporary Python tuples. Shared iterator positions remain current, and counter overflow falls back before consuming a byte. Known local objects also retain their inferred enumeration lanes.
- Enumerate exact tuples of immutable values directly into integer and object lanes. Shared cursors advance together, and unsupported values, counter overflow, and pin pressure retain the generic path.
- Align three interpreter entry points independently on macOS ARM64. Paired experiments recovered code-placement regressions without the executable growth of aligning every function. Other targets retain their existing layout.
- Stage scalar type tags for compiled list appends, allowing fresh generic lists to accept integers, floats, and booleans without leaving native code. Typed lists keep their existing element constraints, and failures resume before mutating the list.
- Reap temporary list pins on native frame exit. Previously, the collector retained those lists until a later collection; cleanup now releases them and their dead children promptly, including when cyclic collection is disabled. Escaped lists stay alive.
- Cache Python hashes in eight bytes with a native atomic word. Hash protocol results normalize the reserved -1 value, preserve machine-sized integers, and hash larger integers without invoking conversion hooks on integer subclasses. Temporary results are released before hash() returns. Instances and frozen sets previously paid for 40-byte locked optional hashes.
- Propagate key hash and equality exceptions from dictionary and set construction, including comprehensions and mapping inputs. Exact dictionary copies retain the stored hashes instead of calling key hash methods again.
- Store the first instance slot inline on 64-bit targets, avoiding a table allocation. Two to eight populated slots use a small ordered vector; the ninth promotes to an ordered table. Updates reuse the existing key, and new slot names go directly into string storage without a temporary String. Smaller targets retain ordinary table storage.

- Construct fixed-size tuples directly from arrays, removing the intermediate vector allocation. Bytecode tuples with up to three elements retain the existing free list, and immutable scalar enumeration values use the inline operand copier.
- Store successful tuple hashes beside the elements in the same allocation. Failed hashes remain uncached, unique free-list reuse clears the cache, and callback exceptions propagate from dictionary and set lookup. The hash word adds eight bytes per tuple.
- Use cached tuple keys directly in functools caches. Hash arguments once per call, surface hash and equality failures before invoking the wrapped function, and remove the Python fallback key wrapper.
- Load canonical string and tuple constants into compiled frames, preserving interpreter identity across activations. Constant pins are reused within each activation. Guarded tuple length accepts exact tuples and resumes the interpreter for other objects.
- Guard boxed integer operands before native arithmetic, allowing generic call results to stay in compiled loops. Other types, overflow, and errors retain their original operands and callback results when execution resumes in the interpreter. Calls that skip a defaulted parameter use the existing generic keyword binder.
- Guard native integer true division before converting either operand to binary64. Values outside the exact conversion range resume the interpreter for correctly rounded division.

## Validation

All 196 compatibility checks pass, including threading, forking, tracing, GC, pickle, JSON, floats, complex numbers, and dictionary comparisons, tuple storage, functools, and tuple C-API operations. VM unit tests, JIT tests, Clippy, compilation without the JIT, and workspace checks also pass. The tuple allocation module also passes three Miri checks on ARM64 and 32-bit x86 under both tested aliasing models; the [standalone harness and its limits](../tuple-cache-miri/) are retained.

The float oracle checks 262,376 binary64 values in `repr`, `str`, complex text, and JSON output. It finds no differences from CPython; the checkpoint differed on 66 values. Seeded datetime comparisons retain the checkpoint's five known timedelta microsecond differences.

## Standard suite

Values are candidate/reference ratios; smaller values mean less time or memory. Geometric means give each fixture equal weight. The workload aggregate includes all 23 timed workloads; the process aggregates include all 24 fixtures, including startup.

| Metric | New/base JIT | New/base interpreter | New/previous JIT | New/previous interpreter | New/CPython JIT | New/CPython interpreter |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Timed workload | 0.857 | 0.967 | 1.012 | 0.996 | 3.730 | 9.659 |
| Process elapsed time | 0.918 | 0.979 | 1.002 | 1.001 | 3.474 | 5.891 |
| Process CPU time | 0.916 | 0.978 | 1.000 | 1.000 | 3.557 | 6.103 |
| Peak RSS | 0.987 | 0.981 | 0.999 | 1.002 | 2.183 | 2.013 |

| Fixture | New/base JIT time | New/base interpreter time | New/CPython JIT time | New/CPython JIT RSS |
| --- | ---: | ---: | ---: | ---: |
| `fannkuch` | 0.961 | 0.985 | 10.499 | 2.064 |
| `nbody` | 0.991 | 0.990 | 8.609 | 2.052 |
| `fib` | 0.992 | 1.005 | 2.845 | 2.086 |
| `pidigits` | 0.993 | 0.986 | 0.895 | 2.048 |
| `pyaes` | 0.053 | 0.975 | 0.666 | 2.074 |
| `richards` | 0.994 | 0.987 | 8.147 | 2.073 |
| `sumvm` | 1.000 | 1.003 | 0.057 | 2.068 |
| `nested_loops` | 1.001 | 1.019 | 0.093 | 2.087 |
| `jitloop` | 1.003 | 0.997 | 0.071 | 2.084 |
| `jitkernels` | 0.990 | 0.986 | 0.858 | 2.060 |
| `deltablue` | 0.986 | 0.982 | 19.420 | 2.246 |
| `float_math` | 0.957 | 0.918 | 8.034 | 3.120 |
| `spectral_norm` | 0.991 | 0.987 | 2.641 | 2.081 |
| `json_bench` | 0.765 | 0.753 | 1.187 | 2.735 |
| `str_methods` | 1.000 | 1.008 | 3.225 | 2.143 |
| `dict_ops` | 0.917 | 0.935 | 5.582 | 2.057 |
| `list_ops` | 1.020 | 0.990 | 14.043 | 2.060 |
| `attr_access` | 0.929 | 0.985 | 3.481 | 2.198 |
| `call_overhead` | 1.172 | 0.983 | 9.280 | 2.198 |
| `generators` | 0.980 | 0.949 | 9.672 | 2.080 |
| `deque_ops` | 0.922 | 0.941 | 16.933 | 2.117 |
| `datetime_ops` | 0.926 | 0.932 | 152.948 | 2.251 |
| `pickle_bench` | 0.959 | 0.982 | 352.940 | 2.657 |
| `startup` | 1.054 | 1.044 | 1.426 | 2.070 |

The incremental comparison below uses the preceding small-slot release from the same alternating process cycles. It includes every standard fixture, whether or not it uses instance slots.

| Fixture | New/previous JIT time | New/previous interpreter time | New/previous JIT CPU | New/previous interpreter CPU | New/previous JIT RSS |
| --- | ---: | ---: | ---: | ---: | ---: |
| `fannkuch` | 1.026 | 1.008 | 1.024 | 0.998 | 1.000 |
| `nbody` | 0.998 | 0.982 | 0.998 | 0.981 | 1.004 |
| `fib` | 0.997 | 0.985 | 0.998 | 0.989 | 1.004 |
| `pidigits` | 0.999 | 1.000 | 0.999 | 1.005 | 0.989 |
| `pyaes` | 1.005 | 1.009 | 0.992 | 1.010 | 1.005 |
| `richards` | 0.991 | 0.999 | 0.992 | 1.000 | 1.000 |
| `sumvm` | 0.985 | 0.991 | 0.982 | 0.994 | 1.001 |
| `nested_loops` | 1.179 | 1.006 | 0.990 | 1.001 | 1.000 |
| `jitloop` | 0.986 | 1.008 | 0.981 | 1.010 | 0.997 |
| `jitkernels` | 0.998 | 1.012 | 0.993 | 1.015 | 1.002 |
| `deltablue` | 0.989 | 0.989 | 0.990 | 0.991 | 1.005 |
| `float_math` | 0.982 | 0.989 | 0.983 | 0.994 | 1.000 |
| `spectral_norm` | 0.992 | 0.992 | 0.991 | 0.994 | 0.998 |
| `json_bench` | 1.010 | 1.000 | 0.959 | 1.032 | 0.950 |
| `str_methods` | 1.014 | 0.958 | 1.014 | 0.971 | 0.995 |
| `dict_ops` | 0.987 | 1.008 | 0.990 | 1.011 | 1.003 |
| `list_ops` | 0.972 | 0.955 | 0.976 | 0.957 | 1.005 |
| `attr_access` | 0.997 | 0.993 | 0.994 | 0.995 | 0.999 |
| `call_overhead` | 1.180 | 0.984 | 1.168 | 0.986 | 1.064 |
| `generators` | 1.006 | 0.994 | 1.004 | 0.997 | 1.003 |
| `deque_ops` | 0.975 | 0.994 | 0.977 | 0.997 | 0.999 |
| `datetime_ops` | 1.016 | 0.994 | 1.016 | 0.994 | 1.003 |
| `pickle_bench` | 1.014 | 1.070 | 1.011 | 1.067 | 0.946 |
| `startup` | 0.998 | 1.017 | 0.992 | 1.011 | 1.006 |

The CPython datetime reference varies substantially between measurement runs, including process CPU time. An earlier post-census diagnostic confirms native datetime constructors and arithmetic in its processes, but doesn't identify what earlier processes loaded or explain the timing difference. [Diagnostic samples](../before-tuple-hash-cache/cpython-datetime-diagnostic.json) and the [original-file repeat](../before-tuple-hash-cache/cpython-datetime-original-repeat.json) are retained alongside the original census. Treat CPython ratios as measurements of the recorded runs, rather than stable ratios across censuses.

A separate nine-cycle repeat checks the apparent regressions in the byte scrambler, list operations, and string methods. It retains the standard work values and includes first-run execution, as the main suite does.

| Fixture | Repeated new/base JIT time | Repeated new/base interpreter time |
| --- | ---: | ---: |
| `pyaes` | 0.054 | 0.943 |
| `list_ops` | 0.999 | 0.996 |
| `str_methods` | 1.019 | 1.016 |

## Supplemental probes

Elapsed times are workload medians in milliseconds. CPU and RSS ratios are medians of paired candidate/reference samples. Workload CPU excludes setup; peak RSS covers the entire process.

| Probe | Baseline ms | New ms | CPython ms | New/base workload CPU | New/base RSS | New/CPython RSS |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `plain_instances` | 284.4625 | 257.0098 | 14.1778 | 0.903 | 0.890 | 3.927 |
| `slotted_instances` | 289.6282 | 241.5658 | 12.0212 | 0.848 | 0.698 | 3.569 |
| `memoryviews` | 95.4721 | 86.3758 | 22.1911 | 0.908 | 0.733 | 1.532 |
| `memoryview_access` | 38.7949 | 36.6467 | 5.8028 | 0.938 | 0.991 | 1.930 |
| `materialized_frames` | 28.7060 | 28.4126 | 3.4826 | 0.984 | 0.903 | 2.106 |
| `bytesio_streams` | 34.1933 | 29.8608 | 2.9377 | 0.887 | 0.843 | 2.983 |
| `type_creation` | 52.2847 | 50.8967 | 40.1130 | 1.008 | 0.954 | 1.208 |
| `float_repr` | 57.9675 | 26.9760 | 26.2638 | 0.463 | 0.992 | 1.690 |
| `float_str` | 64.8179 | 28.6393 | 26.3202 | 0.456 | 0.994 | 1.695 |
| `int_repr` | 25.8345 | 21.2430 | 5.3757 | 0.809 | 0.993 | 1.692 |
| `complex_repr` | 93.7930 | 33.8771 | 44.5752 | 0.375 | 0.993 | 1.731 |
| `json_float_array` | 120.3575 | 17.8367 | 77.7355 | 0.148 | 0.933 | 2.385 |
| `json_int_array` | 45.5449 | 13.5505 | 17.6097 | 0.296 | 0.932 | 2.343 |
| `json_repeated_keys` | 68.6694 | 49.1169 | 48.1142 | 0.715 | 0.951 | 2.460 |
| `json_unique_keys` | 38.6124 | 33.0139 | 27.8178 | 0.859 | 0.880 | 2.446 |

## Instance and hash storage

These interpreter-mode probes compare compact caches and inline slots with the preceding native-list-cleanup build. Nine paired cycles retain allocation, destruction, repeated access, and hash workloads. RSS covers the whole process.

| Probe | New/previous time | New/previous workload CPU | New/previous RSS | New/CPython time | New/CPython RSS |
| --- | ---: | ---: | ---: | ---: | ---: |
| `empty_instances` | 0.944 | 0.944 | 0.963 | 18.942 | 3.452 |
| `one_slot_instances` | 0.887 | 0.887 | 0.760 | 19.723 | 3.588 |
| `two_slot_instances` | 0.932 | 0.932 | 0.849 | 17.869 | 3.633 |
| `many_slot_instances` | 0.851 | 0.852 | 0.674 | 14.816 | 2.879 |
| `singleton_frozensets` | 0.968 | 0.968 | 0.949 | 5.249 | 1.437 |
| `cached_frozenset_hashes` | 0.971 | 0.971 | 0.969 | 4.083 | 1.230 |
| `cached_weakref_hashes` | 0.944 | 0.943 | 0.929 | 17.120 | 4.059 |
| `slot_access_1` | 1.008 | 1.008 | 1.001 | 8.161 | 1.924 |
| `slot_access_2` | 0.941 | 0.941 | 1.001 | 7.913 | 1.927 |
| `slot_access_8` | 0.926 | 0.926 | 1.003 | 7.810 | 1.925 |

The retained preceding comparison isolates the eight-byte cache and hash protocol fixes against the immediately preceding small-tuple release, before tuple hash caching. Each row retains nine paired cycles. This historical comparison includes the larger-slot workload even where elapsed time doesn't improve.

| Probe | New/previous time | New/previous workload CPU | New/previous RSS | New/CPython time | New/CPython RSS |
| --- | ---: | ---: | ---: | ---: | ---: |
| `empty_instances` | 0.969 | 0.969 | 0.964 | 19.000 | 3.450 |
| `one_slot_instances` | 0.964 | 0.963 | 0.966 | 20.713 | 3.580 |
| `two_slot_instances` | 0.966 | 0.965 | 0.974 | 19.314 | 4.159 |
| `many_slot_instances` | 1.004 | 1.004 | 0.986 | 17.114 | 4.207 |
| `singleton_frozensets` | 0.979 | 0.980 | 0.977 | 5.396 | 1.438 |
| `cached_weakref_hashes` | 0.982 | 0.981 | 0.986 | 17.919 | 4.054 |

## Tuple construction

These interpreter-mode probes compare direct tuple allocation with the preceding tuple-enumeration build. Nine paired cycles cover retained tuples, allocation and reuse, dictionary items, enumeration, and string partitioning. The eight-element literal retains the general bytecode construction path.

| Probe | New/previous time | New/previous workload CPU | New/previous RSS | New/CPython time | New/CPython RSS |
| --- | ---: | ---: | ---: | ---: | ---: |
| `tuple_singletons` | 0.943 | 0.943 | 1.000 | 6.684 | 1.842 |
| `tuple_pairs` | 0.954 | 0.954 | 1.034 | 7.616 | 1.751 |
| `tuple_triples` | 0.980 | 0.980 | 1.003 | 6.347 | 1.547 |
| `tuple_eight_items` | 1.011 | 1.011 | 1.001 | 8.474 | 2.124 |
| `tuple_pair_churn` | 0.906 | 0.906 | 1.001 | 6.507 | 1.920 |
| `dictionary_items` | 0.882 | 0.882 | 1.050 | 5.829 | 1.556 |
| `enumeration_items` | 0.885 | 0.885 | 1.013 | 2.689 | 1.549 |
| `string_partition` | 0.974 | 0.974 | 1.001 | 7.781 | 1.832 |

## Preceding tuple hash caching

Repeated hashes, dictionary lookups, fresh keys, and functools calls compare the tuple-cache build with the immediately preceding hash-normalization release. These historical measurements precede the small-slot change. Nine paired interpreter-mode cycles retain every workload, including regressions.

| Probe | New/previous time | New/previous workload CPU | New/previous RSS | New/CPython time | New/CPython RSS |
| --- | ---: | ---: | ---: | ---: | ---: |
| `repeated_pair_hash` | 0.777 | 0.777 | 1.000 | 6.090 | 1.927 |
| `repeated_wide_tuple_hash` | 0.187 | 0.187 | 0.997 | 6.462 | 1.923 |
| `repeated_shared_nested_hash` | 0.013 | 0.013 | 1.002 | 7.003 | 1.933 |
| `tuple_dictionary_lookups` | 0.676 | 0.676 | 0.999 | 8.065 | 1.824 |
| `fresh_pair_hashes` | 0.941 | 0.941 | 0.999 | 5.822 | 1.704 |
| `retained_tuple_keys` | 0.968 | 0.968 | 1.029 | 3.320 | 1.479 |
| `bounded_cache_tuple_hits` | 1.003 | 1.002 | 0.916 | 14.762 | 2.248 |
| `unbounded_cache_tuple_misses` | 0.958 | 0.958 | 0.939 | 7.034 | 2.112 |

## Preceding tuple cache allocation costs

Unhashed tuple workloads use the same immediately preceding release to expose the cost of adding the hash word, including retained one-, two-, three-, and eight-element tuples. These historical measurements precede the small-slot change. Nine paired interpreter-mode cycles retain every workload, including regressions.

| Probe | New/previous time | New/previous workload CPU | New/previous RSS | New/CPython time | New/CPython RSS |
| --- | ---: | ---: | ---: | ---: | ---: |
| `tuple_singletons` | 1.000 | 1.000 | 1.000 | 7.031 | 1.845 |
| `tuple_pairs` | 1.001 | 1.001 | 1.032 | 7.798 | 1.752 |
| `tuple_triples` | 1.003 | 1.003 | 1.000 | 6.937 | 1.548 |
| `tuple_eight_items` | 1.011 | 1.011 | 1.000 | 8.561 | 2.124 |
| `tuple_pair_churn` | 0.998 | 0.998 | 1.000 | 6.414 | 1.930 |
| `dictionary_items` | 1.008 | 1.008 | 1.051 | 6.183 | 1.554 |
| `enumeration_items` | 0.993 | 0.993 | 1.013 | 3.004 | 1.550 |
| `string_partition` | 1.040 | 1.040 | 0.999 | 8.075 | 1.829 |

## Small-slot allocation and access

These eleven interpreter-mode workloads compare the current candidate with the earlier tuple-cache release. Nine paired cycles cover one, two, eight, nine, and sixteen slots. Repeated access targets the last populated field, where the vector search costs the most; deletion and reinsertion retain order.

| Probe | New/previous time | New/previous workload CPU | New/previous RSS | New/CPython time | New/CPython RSS |
| --- | ---: | ---: | ---: | ---: | ---: |
| `retained_slots_1` | 0.974 | 0.974 | 0.999 | 18.641 | 3.568 |
| `last_slot_access_1` | 0.981 | 0.981 | 1.003 | 7.911 | 1.925 |
| `retained_slots_2` | 0.951 | 0.951 | 0.873 | 18.212 | 3.626 |
| `last_slot_access_2` | 0.948 | 0.948 | 1.002 | 8.283 | 1.927 |
| `retained_slots_8` | 0.850 | 0.850 | 0.684 | 15.535 | 2.879 |
| `last_slot_access_8` | 1.143 | 1.143 | 1.004 | 10.176 | 1.922 |
| `retained_slots_9` | 0.922 | 0.922 | 0.817 | 14.462 | 3.191 |
| `last_slot_access_9` | 0.980 | 0.980 | 0.996 | 8.319 | 1.925 |
| `retained_slots_16` | 0.967 | 0.965 | 1.000 | 15.295 | 4.187 |
| `last_slot_access_16` | 1.000 | 1.001 | 0.998 | 8.278 | 1.916 |
| `slot_delete_reinsert` | 0.977 | 0.977 | 1.000 | 14.358 | 1.922 |

The preceding small-tuple candidate had a focused partition run with higher elapsed time despite slightly lower CPU use. Its separate 19-cycle repeat used 0.984 times the preceding elapsed time, 0.983 times its workload CPU, and 0.999 times its peak RSS. The [initial and repeated samples](../small-tuples-initial/) remain available.

## Enumeration

These warm probes isolate the JIT change against the preserved metadata and JSON build. The interpreter comparison is a control. Each workload processes 2,048 elements per call and calls the checksum function 200 times.

| Probe | New/previous JIT time | New/previous interpreter time | New/CPython JIT time | New/CPython JIT RSS |
| --- | ---: | ---: | ---: | ---: |
| `enumerate_bytes` | 0.050 | 0.902 | 0.515 | 2.056 |
| `enumerate_indices` | 0.107 | 0.843 | 1.040 | 2.042 |
| `enumerate_local_indices` | 0.107 | 0.838 | 1.038 | 2.043 |

## List builders

These checked warm workloads build 1,024-element results 200 times. The previous build includes the targeted interpreter alignment, so this comparison covers tagged appends and native list cleanup. Nine paired cycles measure time, CPU use, and peak RSS. Each builder compiles without repeated deoptimizations in the separate trace check.

| Probe | New/previous JIT time | New/previous interpreter time | New/CPython JIT time | New/CPython workload CPU | New/CPython JIT RSS |
| --- | ---: | ---: | ---: | ---: | ---: |
| `list_int_builder` | 0.029 | 1.002 | 0.338 | 0.337 | 2.007 |
| `list_float_builder` | 0.026 | 1.027 | 0.331 | 0.330 | 1.999 |
| `list_bool_builder` | 0.030 | 1.017 | 0.511 | 0.510 | 2.017 |
| `byte_list_builder` | 0.048 | 0.971 | 0.573 | 0.573 | 2.029 |

## Compiled constants and tuple length

Five warm workloads cover tuple constants, guarded tuple lengths, repeated string loads, and short activations. Nine paired cycles compare the candidate with the preceding small-slot release and CPython. Separate traces confirm each measured kernel compiles without repeated exits.

| Probe | New/previous JIT time | New/previous interpreter time | New/previous workload CPU | New/previous RSS | New/CPython JIT time | New/CPython JIT RSS |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `tuple_literal_lengths` | 0.010 | 0.996 | 0.010 | 1.008 | 0.101 | 2.052 |
| `tuple_parameter_lengths` | 0.009 | 1.009 | 0.009 | 1.007 | 0.097 | 2.046 |
| `tuple_constant_returns` | 0.037 | 0.993 | 0.037 | 1.009 | 0.262 | 2.042 |
| `string_constant_returns` | 0.993 | 1.011 | 0.993 | 0.999 | 0.282 | 2.039 |
| `short_string_activations` | 0.842 | 1.002 | 0.842 | 1.002 | 4.309 | 2.048 |

## Generic call results

Four warm workloads consume integer results from callback parameters, keyword calls, and callable instances. Nine paired cycles compare the candidate with the preceding small-slot release and CPython. Separate traces confirm each measured kernel compiles without repeated exits.

| Probe | New/previous JIT time | New/previous interpreter time | New/previous workload CPU | New/previous RSS | New/CPython JIT time | New/CPython JIT RSS |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `integer_callback_sum` | 0.659 | 0.996 | 0.659 | 1.012 | 6.261 | 2.055 |
| `integer_callback_left` | 0.610 | 0.974 | 0.610 | 1.013 | 5.012 | 2.064 |
| `keyword_callback_sum` | 1.014 | 1.020 | 1.014 | 1.015 | 13.825 | 2.063 |
| `callable_instance_sum` | 1.011 | 0.994 | 1.011 | 1.010 | 10.093 | 2.061 |

The [initial constant-load candidate](../constant-loads-initial/) regressed repeated string loads by 12 percent. Its samples remain available. The current candidate forces the shared constant helper to inline. An intermediate scalar-result candidate also exposed incorrect rounding in existing native integer division; its [failing regression and source](../scalar-guard-precision-failure/) are retained without performance claims.

## Startup and imports

These interpreter-mode probes retain 31 paired samples after a discarded warmup cycle, using the staged standard library. Times include the whole process. The 95th percentile uses the nearest-rank sample; it is a local observation, not a latency guarantee.

| Probe | Baseline median ms | New median ms | CPython median ms | Baseline p95 ms | New p95 ms | CPython p95 ms | New/base RSS | New/CPython RSS |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| `startup` | 28.168 | 28.677 | 20.314 | 29.028 | 29.798 | 21.871 | 0.990 | 1.924 |
| `startup_no_site` | 8.259 | 8.703 | 14.006 | 9.206 | 9.148 | 14.873 | 1.002 | 1.548 |
| `imports` | 74.621 | 75.578 | 24.017 | 84.387 | 88.761 | 29.186 | 0.931 | 2.724 |

## Eight-thread execution

The checked concurrency probe uses the existing arithmetic kernel with 1,000,000 iterations per worker. It starts timing before releasing an event gate and verifies all eight results. Thread creation is outside the timer; event release, worker execution, and joins are inside. New worker threads retain their normal JIT initialization costs. Each GIL mode runs separately with five measured cycles and one discarded cycle.

Both WeavePy modes request the JIT; runtime gating still applies. The installed CPython is a GIL build, so this is not a comparison with free-threaded CPython. Larger serial/parallel speedups are better; smaller time, CPU, and memory ratios are better.

| WeavePy mode | New/base parallel time | New/CPython parallel time | New/base parallel CPU | New/CPython parallel CPU | New/base RSS | New/CPython RSS | New serial/parallel speedup | CPython speedup |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| GIL enabled | 1.000 | 0.112 | 1.000 | 0.112 | 0.945 | 3.856 | 0.918 | 1.000 |
| GIL disabled | 1.176 | 0.991 | 1.367 | 6.700 | 0.939 | 3.569 | 3.132 | 1.005 |

## Remaining gaps

With the JIT enabled, the candidate uses less workload time than CPython in 6 of 23 standard timed fixtures and less peak process memory in 0. The largest remaining time ratios are below. These gaps remain part of the measurement set, including workloads that use pure-Python standard-library implementations where CPython has native implementations.

| Fixture | New/CPython JIT time | New/CPython JIT RSS |
| --- | ---: | ---: |
| `pickle_bench` | 352.940 | 2.657 |
| `datetime_ops` | 152.948 | 2.251 |
| `deltablue` | 19.420 | 2.246 |
| `deque_ops` | 16.933 | 2.117 |
| `list_ops` | 14.043 | 2.060 |

The executable contains 44,139,600 bytes, compared with 44,184,864 at the checkpoint. Build latency was not measured under controlled conditions. The memory figures are process peak RSS, not isolated object sizes or allocation counts. These measurements cover this host and these workloads; they cannot establish superiority for every Python program.

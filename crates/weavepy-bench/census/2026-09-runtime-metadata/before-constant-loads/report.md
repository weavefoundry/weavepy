# Runtime metadata and numeric text performance

These changes reduce object metadata storage, dictionary bookkeeping, float-formatting allocations, and JSON key allocations. They also extend compiled enumeration and list builders, and cache successful tuple hashes. WeavePy still does not outperform CPython across all workloads or all measured metrics.

The baseline is commit `9a69c4161ade6527a5f4457d7e7272075cb1f0d9`. The candidate uses the same release profile and default JIT feature on macOS ARM64, with CPython 3.14.7 as the reference. The standard suite also pairs the immediately preceding tuple-cache release with the candidate in the same measurement cycles. [Raw samples, checksums, and reproduction instructions](./) include intermediate stages as well as these measured results.

The measured executable is `target/release/weavepy-runtime-small-slots` with SHA-256 `78f309f65565be93dc7a5cb725df2243640c566199374110515207191998edd5`.

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

## Validation

All 190 compatibility checks pass, including threading, forking, tracing, GC, pickle, JSON, floats, complex numbers, and dictionary comparisons, tuple storage, functools, and tuple C-API operations. VM unit tests, JIT tests, Clippy, compilation without the JIT, and workspace checks also pass. The tuple allocation module also passes three Miri checks on ARM64 and 32-bit x86 under both tested aliasing models; the [standalone harness and its limits](../tuple-cache-miri/) are retained.

The float oracle checks 262,376 binary64 values in `repr`, `str`, complex text, and JSON output. It finds no differences from CPython; the checkpoint differed on 66 values. Seeded datetime comparisons retain the checkpoint's five known timedelta microsecond differences.

## Standard suite

Values are candidate/reference ratios; smaller values mean less time or memory. Geometric means give each fixture equal weight. The workload aggregate includes all 23 timed workloads; the process aggregates include all 24 fixtures, including startup.

| Metric | New/base JIT | New/base interpreter | New/previous JIT | New/previous interpreter | New/CPython JIT | New/CPython interpreter |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Timed workload | 0.853 | 0.961 | 0.978 | 0.982 | 3.278 | 8.714 |
| Process elapsed time | 0.912 | 0.972 | 0.979 | 0.985 | 3.124 | 5.364 |
| Process CPU time | 0.910 | 0.972 | 0.978 | 0.984 | 3.197 | 5.548 |
| Peak RSS | 0.982 | 0.982 | 0.999 | 1.001 | 2.174 | 2.011 |

| Fixture | New/base JIT time | New/base interpreter time | New/CPython JIT time | New/CPython JIT RSS |
| --- | ---: | ---: | ---: | ---: |
| `fannkuch` | 0.965 | 0.957 | 11.252 | 2.053 |
| `nbody` | 1.044 | 0.969 | 8.688 | 2.049 |
| `fib` | 0.995 | 1.008 | 2.829 | 2.081 |
| `pidigits` | 1.000 | 0.996 | 0.906 | 2.058 |
| `pyaes` | 0.053 | 0.938 | 0.652 | 2.057 |
| `richards` | 1.006 | 0.979 | 8.028 | 2.070 |
| `sumvm` | 0.996 | 1.009 | 0.056 | 2.078 |
| `nested_loops` | 1.000 | 1.005 | 0.080 | 2.080 |
| `jitloop` | 1.004 | 0.984 | 0.074 | 2.088 |
| `jitkernels` | 0.987 | 0.988 | 0.868 | 2.058 |
| `deltablue` | 0.997 | 0.991 | 19.646 | 2.220 |
| `float_math` | 0.959 | 0.945 | 7.754 | 3.115 |
| `spectral_norm` | 1.000 | 0.997 | 2.591 | 2.076 |
| `json_bench` | 0.773 | 0.766 | 1.138 | 2.737 |
| `str_methods` | 1.012 | 1.035 | 3.189 | 2.152 |
| `dict_ops` | 0.924 | 0.926 | 5.591 | 2.044 |
| `list_ops` | 1.009 | 0.994 | 14.375 | 2.052 |
| `attr_access` | 0.947 | 0.948 | 3.507 | 2.185 |
| `call_overhead` | 0.920 | 0.983 | 6.410 | 2.068 |
| `generators` | 0.974 | 0.951 | 9.168 | 2.078 |
| `deque_ops` | 0.940 | 0.916 | 17.998 | 2.116 |
| `datetime_ops` | 0.902 | 0.907 | 14.732 | 2.254 |
| `pickle_bench` | 0.976 | 0.948 | 304.845 | 2.653 |
| `startup` | 1.039 | 1.038 | 1.432 | 2.060 |

The incremental comparison below uses the preceding tuple-cache release from the same alternating process cycles. It includes every standard fixture, whether or not it uses instance slots.

| Fixture | New/previous JIT time | New/previous interpreter time | New/previous JIT CPU | New/previous interpreter CPU | New/previous JIT RSS |
| --- | ---: | ---: | ---: | ---: | ---: |
| `fannkuch` | 0.990 | 0.981 | 0.976 | 0.988 | 0.999 |
| `nbody` | 0.994 | 0.959 | 0.993 | 0.962 | 0.996 |
| `fib` | 1.007 | 1.074 | 0.982 | 1.057 | 0.997 |
| `pidigits` | 0.982 | 1.023 | 0.975 | 1.023 | 1.007 |
| `pyaes` | 0.997 | 0.951 | 0.985 | 0.957 | 0.996 |
| `richards` | 0.993 | 0.999 | 0.988 | 1.005 | 0.997 |
| `sumvm` | 1.001 | 1.003 | 0.978 | 1.003 | 0.998 |
| `nested_loops` | 0.855 | 0.981 | 0.956 | 0.992 | 0.993 |
| `jitloop` | 0.998 | 0.988 | 0.990 | 0.993 | 0.996 |
| `jitkernels` | 0.997 | 0.966 | 0.976 | 0.969 | 0.999 |
| `deltablue` | 0.999 | 0.990 | 0.999 | 0.977 | 0.996 |
| `float_math` | 0.975 | 0.989 | 0.973 | 0.987 | 0.999 |
| `spectral_norm` | 1.000 | 0.993 | 0.996 | 0.997 | 1.001 |
| `json_bench` | 1.000 | 0.996 | 1.003 | 0.996 | 1.003 |
| `str_methods` | 1.002 | 1.017 | 1.004 | 1.012 | 0.998 |
| `dict_ops` | 0.974 | 0.985 | 0.980 | 0.991 | 0.996 |
| `list_ops` | 0.944 | 0.970 | 0.949 | 0.977 | 0.995 |
| `attr_access` | 0.938 | 0.954 | 0.942 | 0.963 | 1.000 |
| `call_overhead` | 0.986 | 0.980 | 0.985 | 0.982 | 0.997 |
| `generators` | 1.010 | 0.996 | 1.002 | 0.997 | 0.997 |
| `deque_ops` | 0.934 | 0.926 | 0.944 | 0.932 | 1.004 |
| `datetime_ops` | 0.939 | 0.928 | 0.939 | 0.929 | 0.998 |
| `pickle_bench` | 0.987 | 0.949 | 0.986 | 0.948 | 1.006 |
| `startup` | 0.986 | 0.991 | 0.989 | 0.993 | 0.997 |

The CPython datetime reference varies substantially between measurement runs, including process CPU time. An earlier post-census diagnostic confirms native datetime constructors and arithmetic in its processes, but doesn't identify what earlier processes loaded or explain the timing difference. [Diagnostic samples](../before-tuple-hash-cache/cpython-datetime-diagnostic.json) and the [original-file repeat](../before-tuple-hash-cache/cpython-datetime-original-repeat.json) are retained alongside the original census. Treat CPython ratios as measurements of the recorded runs, rather than stable ratios across censuses.

A separate nine-cycle repeat checks the apparent regressions in the byte scrambler, list operations, and string methods. It retains the standard work values and includes first-run execution, as the main suite does.

| Fixture | Repeated new/base JIT time | Repeated new/base interpreter time |
| --- | ---: | ---: |
| `pyaes` | 0.054 | 0.960 |
| `list_ops` | 0.998 | 0.998 |
| `str_methods` | 0.987 | 0.988 |

## Supplemental probes

Elapsed times are workload medians in milliseconds. CPU and RSS ratios are medians of paired candidate/reference samples. Workload CPU excludes setup; peak RSS covers the entire process.

| Probe | Baseline ms | New ms | CPython ms | New/base workload CPU | New/base RSS | New/CPython RSS |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `plain_instances` | 307.5523 | 281.3067 | 15.0732 | 0.895 | 0.891 | 3.923 |
| `slotted_instances` | 322.7056 | 270.3952 | 13.4488 | 0.825 | 0.698 | 3.580 |
| `memoryviews` | 113.3885 | 102.7491 | 25.4704 | 0.928 | 0.732 | 1.532 |
| `memoryview_access` | 43.4478 | 41.8496 | 6.6818 | 0.953 | 0.987 | 1.924 |
| `materialized_frames` | 37.3860 | 36.0018 | 4.5274 | 0.971 | 0.905 | 2.112 |
| `bytesio_streams` | 38.9977 | 34.8342 | 3.3150 | 0.864 | 0.843 | 2.984 |
| `type_creation` | 58.3779 | 62.4235 | 45.9864 | 1.013 | 0.954 | 1.207 |
| `float_repr` | 66.6780 | 32.3439 | 31.5624 | 0.472 | 0.992 | 1.690 |
| `float_str` | 73.8017 | 35.4948 | 33.5762 | 0.466 | 0.990 | 1.695 |
| `int_repr` | 27.2544 | 21.7653 | 5.7394 | 0.825 | 0.991 | 1.690 |
| `complex_repr` | 96.1623 | 36.0089 | 47.3670 | 0.375 | 0.989 | 1.722 |
| `json_float_array` | 117.0005 | 17.6045 | 75.8406 | 0.150 | 0.930 | 2.389 |
| `json_int_array` | 42.7595 | 13.0801 | 17.1986 | 0.300 | 0.928 | 2.335 |
| `json_repeated_keys` | 73.1565 | 50.9558 | 49.8531 | 0.711 | 0.944 | 2.455 |
| `json_unique_keys` | 37.3859 | 31.8500 | 28.0480 | 0.855 | 0.886 | 2.434 |

## Instance and hash storage

These interpreter-mode probes compare compact caches and inline slots with the preceding native-list-cleanup build. Nine paired cycles retain allocation, destruction, repeated access, and hash workloads. RSS covers the whole process.

| Probe | New/previous time | New/previous workload CPU | New/previous RSS | New/CPython time | New/CPython RSS |
| --- | ---: | ---: | ---: | ---: | ---: |
| `empty_instances` | 0.995 | 0.995 | 0.964 | 18.919 | 3.453 |
| `one_slot_instances` | 0.909 | 0.916 | 0.763 | 20.222 | 3.588 |
| `two_slot_instances` | 0.875 | 0.876 | 0.849 | 19.388 | 3.640 |
| `many_slot_instances` | 0.922 | 0.892 | 0.673 | 16.657 | 2.876 |
| `singleton_frozensets` | 0.974 | 0.969 | 0.953 | 5.272 | 1.452 |
| `cached_frozenset_hashes` | 0.980 | 0.981 | 0.961 | 3.684 | 1.223 |
| `cached_weakref_hashes` | 0.929 | 0.932 | 0.927 | 17.873 | 4.079 |
| `slot_access_1` | 0.973 | 0.983 | 1.000 | 7.571 | 1.919 |
| `slot_access_2` | 0.908 | 0.905 | 0.996 | 8.022 | 1.908 |
| `slot_access_8` | 0.935 | 0.938 | 1.006 | 7.422 | 1.927 |

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
| `tuple_singletons` | 0.949 | 0.958 | 1.003 | 6.399 | 1.845 |
| `tuple_pairs` | 0.972 | 0.968 | 1.030 | 7.380 | 1.746 |
| `tuple_triples` | 0.969 | 0.970 | 1.002 | 6.885 | 1.551 |
| `tuple_eight_items` | 0.991 | 0.991 | 1.000 | 8.470 | 2.126 |
| `tuple_pair_churn` | 0.910 | 0.909 | 0.996 | 6.443 | 1.928 |
| `dictionary_items` | 0.828 | 0.828 | 1.047 | 6.085 | 1.556 |
| `enumeration_items` | 0.881 | 0.880 | 1.012 | 2.913 | 1.552 |
| `string_partition` | 0.990 | 0.991 | 0.998 | 7.781 | 1.825 |

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

These eleven interpreter-mode workloads isolate the ordered slot vector and direct key allocation against the preceding tuple-cache release. Nine paired cycles cover one, two, eight, nine, and sixteen slots. Repeated access targets the last populated field, where the vector search costs the most; deletion and reinsertion retain order.

| Probe | New/previous time | New/previous workload CPU | New/previous RSS | New/CPython time | New/CPython RSS |
| --- | ---: | ---: | ---: | ---: | ---: |
| `retained_slots_1` | 0.989 | 0.990 | 1.000 | 18.441 | 3.584 |
| `last_slot_access_1` | 0.996 | 0.995 | 1.001 | 8.099 | 1.922 |
| `retained_slots_2` | 0.955 | 0.955 | 0.871 | 17.799 | 3.634 |
| `last_slot_access_2` | 0.960 | 0.960 | 0.999 | 8.228 | 1.930 |
| `retained_slots_8` | 0.856 | 0.855 | 0.684 | 14.510 | 2.881 |
| `last_slot_access_8` | 1.152 | 1.152 | 0.995 | 9.689 | 1.916 |
| `retained_slots_9` | 0.912 | 0.912 | 0.817 | 13.785 | 3.191 |
| `last_slot_access_9` | 1.001 | 1.001 | 0.999 | 8.346 | 1.930 |
| `retained_slots_16` | 0.972 | 0.973 | 1.000 | 14.786 | 4.188 |
| `last_slot_access_16` | 0.996 | 0.995 | 1.001 | 8.993 | 1.918 |
| `slot_delete_reinsert` | 0.958 | 0.956 | 1.001 | 14.878 | 1.921 |

The preceding small-tuple candidate had a focused partition run with higher elapsed time despite slightly lower CPU use. Its separate 19-cycle repeat used 0.984 times the preceding elapsed time, 0.983 times its workload CPU, and 0.999 times its peak RSS. The [initial and repeated samples](../small-tuples-initial/) remain available.

## Enumeration

These warm probes isolate the JIT change against the preserved metadata and JSON build. The interpreter comparison is a control. Each workload processes 2,048 elements per call and calls the checksum function 200 times.

| Probe | New/previous JIT time | New/previous interpreter time | New/CPython JIT time | New/CPython JIT RSS |
| --- | ---: | ---: | ---: | ---: |
| `enumerate_bytes` | 0.049 | 0.885 | 0.523 | 2.050 |
| `enumerate_indices` | 0.112 | 0.838 | 1.064 | 2.051 |
| `enumerate_local_indices` | 0.118 | 0.846 | 1.054 | 2.049 |

## List builders

These checked warm workloads build 1,024-element results 200 times. The previous build includes the targeted interpreter alignment, so this comparison covers tagged appends and native list cleanup. Nine paired cycles measure time, CPU use, and peak RSS. Each builder compiles without repeated deoptimizations in the separate trace check.

| Probe | New/previous JIT time | New/previous interpreter time | New/CPython JIT time | New/CPython workload CPU | New/CPython JIT RSS |
| --- | ---: | ---: | ---: | ---: | ---: |
| `list_int_builder` | 0.027 | 1.038 | 0.324 | 0.324 | 2.003 |
| `list_float_builder` | 0.024 | 1.056 | 0.302 | 0.304 | 1.996 |
| `list_bool_builder` | 0.029 | 1.003 | 0.521 | 0.520 | 2.019 |
| `byte_list_builder` | 0.047 | 1.000 | 0.549 | 0.549 | 2.031 |

## Startup and imports

These interpreter-mode probes retain 31 paired samples after a discarded warmup cycle, using the staged standard library. Times include the whole process. The 95th percentile uses the nearest-rank sample; it is a local observation, not a latency guarantee.

| Probe | Baseline median ms | New median ms | CPython median ms | Baseline p95 ms | New p95 ms | CPython p95 ms | New/base RSS | New/CPython RSS |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| `startup` | 30.744 | 31.181 | 22.565 | 34.838 | 35.484 | 67.748 | 0.989 | 1.921 |
| `startup_no_site` | 10.003 | 10.077 | 16.673 | 13.266 | 13.184 | 22.665 | 1.000 | 1.547 |
| `imports` | 82.979 | 77.967 | 27.350 | 91.689 | 89.075 | 29.532 | 0.928 | 2.710 |

## Eight-thread execution

The checked concurrency probe uses the existing arithmetic kernel with 1,000,000 iterations per worker. It starts timing before releasing an event gate and verifies all eight results. Thread creation is outside the timer; event release, worker execution, and joins are inside. New worker threads retain their normal JIT initialization costs. Each GIL mode runs separately with five measured cycles and one discarded cycle.

Both WeavePy modes request the JIT; runtime gating still applies. The installed CPython is a GIL build, so this is not a comparison with free-threaded CPython. Larger serial/parallel speedups are better; smaller time, CPU, and memory ratios are better.

| WeavePy mode | New/base parallel time | New/CPython parallel time | New/base parallel CPU | New/CPython parallel CPU | New/base RSS | New/CPython RSS | New serial/parallel speedup | CPython speedup |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| GIL enabled | 1.020 | 0.107 | 1.019 | 0.106 | 0.940 | 3.833 | 0.936 | 0.977 |
| GIL disabled | 0.967 | 1.072 | 0.944 | 3.896 | 0.942 | 3.550 | 3.146 | 0.999 |

## Remaining gaps

With the JIT enabled, the candidate uses less workload time than CPython in 6 of 23 standard timed fixtures and less peak process memory in 0. The largest remaining time ratios are below. These gaps remain part of the measurement set, including workloads that use pure-Python standard-library implementations where CPython has native implementations.

| Fixture | New/CPython JIT time | New/CPython JIT RSS |
| --- | ---: | ---: |
| `pickle_bench` | 304.845 | 2.653 |
| `deltablue` | 19.646 | 2.220 |
| `deque_ops` | 17.998 | 2.116 |
| `datetime_ops` | 14.732 | 2.254 |
| `list_ops` | 14.375 | 2.052 |

The executable contains 44,139,104 bytes, compared with 44,184,864 at the checkpoint. Build latency was not measured under controlled conditions. The memory figures are process peak RSS, not isolated object sizes or allocation counts. These measurements cover this host and these workloads; they cannot establish superiority for every Python program.

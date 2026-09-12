# Runtime metadata and numeric text performance

These changes reduce object metadata storage, dictionary bookkeeping, float-formatting allocations, and JSON key allocations. They also extend compiled enumeration and list builders, and cache successful tuple hashes. WeavePy still does not outperform CPython across all workloads or all measured metrics.

The baseline is commit `9a69c4161ade6527a5f4457d7e7272075cb1f0d9`. The candidate uses the same release profile and default JIT feature on macOS ARM64, with CPython 3.14.7 as the reference. [Raw samples, checksums, and reproduction instructions](./) include intermediate stages as well as these measured results.

The measured executable is `target/release/weavepy-runtime-tuple-hash-cache` with SHA-256 `a8faed6241e637f921ad1bb22f7ea16d48bae1b93a5ad1bf78a590e87670610b`.

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
- Store the first instance slot inline on 64-bit targets, avoiding a table allocation. A second distinct slot promotes to an ordered table; updates reuse the existing key. Smaller targets retain ordinary table storage.

- Construct fixed-size tuples directly from arrays, removing the intermediate vector allocation. Bytecode tuples with up to three elements retain the existing free list, and immutable scalar enumeration values use the inline operand copier.
- Store successful tuple hashes beside the elements in the same allocation. Failed hashes remain uncached, unique free-list reuse clears the cache, and callback exceptions propagate from dictionary and set lookup. The hash word adds eight bytes per tuple.
- Use cached tuple keys directly in functools caches. Hash arguments once per call, surface hash and equality failures before invoking the wrapped function, and remove the Python fallback key wrapper.

## Validation

All 187 compatibility checks pass, including threading, forking, tracing, GC, pickle, JSON, floats, complex numbers, and dictionary comparisons, tuple storage, functools, and tuple C-API operations. VM unit tests, JIT tests, Clippy, compilation without the JIT, and workspace checks also pass. The tuple allocation module also passes three Miri checks on ARM64 and 32-bit x86 under both tested aliasing models; the [standalone harness and its limits](../tuple-cache-miri/) are retained.

The float oracle checks 262,376 binary64 values in `repr`, `str`, complex text, and JSON output. It finds no differences from CPython; the checkpoint differed on 66 values. Seeded datetime comparisons retain the checkpoint's five known timedelta microsecond differences.

## Standard suite

Values are candidate/reference ratios; smaller values mean less time or memory. Geometric means give each fixture equal weight. The workload aggregate includes all 23 timed workloads; the process aggregates include all 24 fixtures, including startup.

| Metric | New/base JIT | New/base interpreter | New/CPython JIT | New/CPython interpreter |
| --- | ---: | ---: | ---: | ---: |
| Timed workload | 0.856 | 0.996 | 3.478 | 9.247 |
| Process elapsed time | 0.915 | 1.002 | 3.293 | 5.610 |
| Process CPU time | 0.914 | 1.001 | 3.350 | 5.778 |
| Peak RSS | 0.983 | 0.981 | 2.176 | 2.009 |

| Fixture | New/base JIT time | New/base interpreter time | New/CPython JIT time | New/CPython JIT RSS |
| --- | ---: | ---: | ---: | ---: |
| `fannkuch` | 0.964 | 0.969 | 11.073 | 2.052 |
| `nbody` | 1.018 | 1.005 | 9.579 | 2.042 |
| `fib` | 1.013 | 0.996 | 3.156 | 2.076 |
| `pidigits` | 1.000 | 1.005 | 0.893 | 2.046 |
| `pyaes` | 0.051 | 0.991 | 0.644 | 2.053 |
| `richards` | 1.021 | 1.003 | 8.170 | 2.074 |
| `sumvm` | 0.983 | 1.022 | 0.056 | 2.081 |
| `nested_loops` | 0.987 | 1.003 | 0.083 | 2.084 |
| `jitloop` | 1.001 | 1.013 | 0.073 | 2.082 |
| `jitkernels` | 0.996 | 1.071 | 0.933 | 2.068 |
| `deltablue` | 0.997 | 0.947 | 20.933 | 2.243 |
| `float_math` | 0.990 | 0.954 | 6.782 | 3.131 |
| `spectral_norm` | 1.010 | 1.006 | 2.530 | 2.070 |
| `json_bench` | 0.709 | 0.764 | 1.145 | 2.725 |
| `str_methods` | 0.992 | 1.025 | 3.298 | 2.155 |
| `dict_ops` | 0.933 | 0.935 | 5.977 | 2.058 |
| `list_ops` | 1.041 | 1.050 | 15.018 | 2.056 |
| `attr_access` | 0.910 | 1.064 | 4.004 | 2.205 |
| `call_overhead` | 1.002 | 1.029 | 8.830 | 2.073 |
| `generators` | 1.045 | 1.096 | 9.217 | 2.093 |
| `deque_ops` | 0.940 | 1.073 | 15.595 | 2.110 |
| `datetime_ops` | 0.908 | 0.941 | 19.119 | 2.253 |
| `pickle_bench` | 1.006 | 1.004 | 479.394 | 2.638 |
| `startup` | 0.982 | 0.992 | 1.462 | 2.059 |

The CPython datetime reference varies substantially between measurement runs, including process CPU time. A separate post-census diagnostic confirms native datetime constructors and arithmetic in its processes, but doesn't identify what earlier processes loaded or explain the timing difference. [Diagnostic samples](../before-tuple-hash-cache/cpython-datetime-diagnostic.json) and the [original-file repeat](../before-tuple-hash-cache/cpython-datetime-original-repeat.json) are retained alongside the original census. Treat CPython ratios as measurements of the recorded runs, rather than stable ratios across censuses.

A separate nine-cycle repeat checks the apparent regressions in the byte scrambler, list operations, and string methods. It retains the standard work values and includes first-run execution, as the main suite does.

| Fixture | Repeated new/base JIT time | Repeated new/base interpreter time |
| --- | ---: | ---: |
| `pyaes` | 0.053 | 1.077 |
| `list_ops` | 1.034 | 1.049 |
| `str_methods` | 1.031 | 0.997 |

## Supplemental probes

Elapsed times are workload medians in milliseconds. CPU and RSS ratios are medians of paired candidate/reference samples. Workload CPU excludes setup; peak RSS covers the entire process.

| Probe | Baseline ms | New ms | CPython ms | New/base workload CPU | New/base RSS | New/CPython RSS |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `plain_instances` | 1296.9935 | 1185.9748 | 36.9182 | 0.932 | 0.895 | 3.917 |
| `slotted_instances` | 1174.7726 | 955.1085 | 27.9537 | 0.831 | 0.704 | 3.580 |
| `memoryviews` | 250.2521 | 232.2580 | 54.2055 | 0.923 | 0.734 | 1.532 |
| `memoryview_access` | 118.7261 | 90.5664 | 13.4929 | 0.921 | 0.988 | 1.909 |
| `materialized_frames` | 68.5257 | 64.5721 | 10.4949 | 0.950 | 0.909 | 2.097 |
| `bytesio_streams` | 88.4844 | 76.0152 | 7.5932 | 0.928 | 0.842 | 2.976 |
| `type_creation` | 130.6065 | 115.8113 | 92.6096 | 0.939 | 0.954 | 1.205 |
| `float_repr` | 134.6694 | 61.6430 | 59.2465 | 0.457 | 0.989 | 1.679 |
| `float_str` | 135.6254 | 58.8489 | 57.2997 | 0.440 | 0.992 | 1.689 |
| `int_repr` | 64.7953 | 52.5200 | 14.7710 | 0.838 | 0.991 | 1.701 |
| `complex_repr` | 231.2466 | 75.0536 | 109.6423 | 0.366 | 0.987 | 1.728 |
| `json_float_array` | 311.9015 | 43.6935 | 191.4795 | 0.148 | 0.928 | 2.359 |
| `json_int_array` | 100.2430 | 30.7279 | 45.2619 | 0.318 | 0.926 | 2.273 |
| `json_repeated_keys` | 166.9550 | 115.3365 | 113.9785 | 0.713 | 0.949 | 2.474 |
| `json_unique_keys` | 95.3512 | 85.5442 | 68.6361 | 0.900 | 0.897 | 2.454 |

## Instance and hash storage

These interpreter-mode probes compare compact caches and inline slots with the preceding native-list-cleanup build. Nine paired cycles retain allocation, destruction, repeated access, and hash workloads. RSS covers the whole process.

| Probe | New/previous time | New/previous workload CPU | New/previous RSS | New/CPython time | New/CPython RSS |
| --- | ---: | ---: | ---: | ---: | ---: |
| `empty_instances` | 1.039 | 0.976 | 0.956 | 19.889 | 3.397 |
| `one_slot_instances` | 0.866 | 0.876 | 0.765 | 21.116 | 3.591 |
| `two_slot_instances` | 0.938 | 0.944 | 0.980 | 19.415 | 4.174 |
| `many_slot_instances` | 1.004 | 1.005 | 0.986 | 18.036 | 4.215 |
| `singleton_frozensets` | 0.979 | 0.975 | 0.945 | 5.511 | 1.440 |
| `cached_frozenset_hashes` | 0.989 | 0.986 | 0.953 | 3.674 | 1.231 |
| `cached_weakref_hashes` | 0.872 | 0.883 | 0.929 | 18.132 | 4.076 |
| `slot_access_1` | 0.986 | 0.986 | 0.999 | 8.136 | 1.915 |
| `slot_access_2` | 1.019 | 1.018 | 0.997 | 8.624 | 1.920 |
| `slot_access_8` | 1.029 | 1.028 | 1.001 | 8.657 | 1.930 |

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
| `tuple_singletons` | 0.947 | 0.947 | 1.002 | 6.934 | 1.847 |
| `tuple_pairs` | 0.972 | 0.971 | 1.034 | 7.298 | 1.752 |
| `tuple_triples` | 0.980 | 0.980 | 1.002 | 6.540 | 1.549 |
| `tuple_eight_items` | 1.006 | 1.007 | 1.002 | 8.685 | 2.125 |
| `tuple_pair_churn` | 0.929 | 0.930 | 1.004 | 6.550 | 1.935 |
| `dictionary_items` | 0.866 | 0.866 | 1.050 | 5.664 | 1.553 |
| `enumeration_items` | 0.896 | 0.894 | 1.014 | 2.805 | 1.549 |
| `string_partition` | 0.999 | 0.999 | 1.001 | 8.036 | 1.831 |

## Tuple hash caching

Repeated hashes, dictionary lookups, fresh keys, and functools calls compare the tuple-cache build with the immediately preceding hash-normalization release. Nine paired interpreter-mode cycles retain every workload, including regressions.

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

## Tuple cache allocation costs

Unhashed tuple workloads use the same immediately preceding release to expose the cost of adding the hash word, including retained one-, two-, three-, and eight-element tuples. Nine paired interpreter-mode cycles retain every workload, including regressions.

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

The preceding small-tuple candidate had a focused partition run with higher elapsed time despite slightly lower CPU use. Its separate 19-cycle repeat used 0.984 times the preceding elapsed time, 0.983 times its workload CPU, and 0.999 times its peak RSS. The [initial and repeated samples](../small-tuples-initial/) remain available.

## Enumeration

These warm probes isolate the JIT change against the preserved metadata and JSON build. The interpreter comparison is a control. Each workload processes 2,048 elements per call and calls the checksum function 200 times.

| Probe | New/previous JIT time | New/previous interpreter time | New/CPython JIT time | New/CPython JIT RSS |
| --- | ---: | ---: | ---: | ---: |
| `enumerate_bytes` | 0.050 | 0.918 | 0.506 | 2.051 |
| `enumerate_indices` | 0.104 | 0.859 | 1.004 | 2.041 |
| `enumerate_local_indices` | 0.104 | 0.868 | 0.974 | 2.043 |

## List builders

These checked warm workloads build 1,024-element results 200 times. The previous build includes the targeted interpreter alignment, so this comparison covers tagged appends and native list cleanup. Nine paired cycles measure time, CPU use, and peak RSS. Each builder compiles without repeated deoptimizations in the separate trace check.

| Probe | New/previous JIT time | New/previous interpreter time | New/CPython JIT time | New/CPython workload CPU | New/CPython JIT RSS |
| --- | ---: | ---: | ---: | ---: | ---: |
| `list_int_builder` | 0.030 | 1.132 | 0.346 | 0.345 | 2.009 |
| `list_float_builder` | 0.026 | 1.099 | 0.331 | 0.331 | 2.009 |
| `list_bool_builder` | 0.030 | 1.107 | 0.524 | 0.523 | 2.018 |
| `byte_list_builder` | 0.048 | 1.006 | 0.566 | 0.566 | 2.032 |

## Startup and imports

These interpreter-mode probes retain 31 paired samples after a discarded warmup cycle, using the staged standard library. Times include the whole process. The 95th percentile uses the nearest-rank sample; it is a local observation, not a latency guarantee.

| Probe | Baseline median ms | New median ms | CPython median ms | Baseline p95 ms | New p95 ms | CPython p95 ms | New/base RSS | New/CPython RSS |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| `startup` | 76.137 | 76.036 | 58.299 | 85.681 | 88.448 | 68.733 | 0.992 | 1.915 |
| `startup_no_site` | 23.561 | 24.769 | 43.306 | 42.686 | 44.781 | 59.267 | 1.009 | 1.551 |
| `imports` | 217.282 | 207.841 | 75.476 | 261.501 | 272.580 | 94.828 | 0.923 | 2.692 |

## Eight-thread execution

The checked concurrency probe uses the existing arithmetic kernel with 1,000,000 iterations per worker. It starts timing before releasing an event gate and verifies all eight results. Thread creation is outside the timer; event release, worker execution, and joins are inside. New worker threads retain their normal JIT initialization costs. Each GIL mode runs separately with five measured cycles and one discarded cycle.

Both WeavePy modes request the JIT; runtime gating still applies. The installed CPython is a GIL build, so this is not a comparison with free-threaded CPython. Larger serial/parallel speedups are better; smaller time, CPU, and memory ratios are better.

| WeavePy mode | New/base parallel time | New/CPython parallel time | New/base parallel CPU | New/CPython parallel CPU | New/base RSS | New/CPython RSS | New serial/parallel speedup | CPython speedup |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| GIL enabled | 1.197 | 0.092 | 1.181 | 0.094 | 0.941 | 3.824 | 1.231 | 1.180 |
| GIL disabled | 0.936 | 1.150 | 0.960 | 3.858 | 0.938 | 3.543 | 3.580 | 1.434 |

## Remaining gaps

With the JIT enabled, the candidate uses less workload time than CPython in 6 of 23 standard timed fixtures and less peak process memory in 0. The largest remaining time ratios are below. These gaps remain part of the measurement set, including workloads that use pure-Python standard-library implementations where CPython has native implementations.

| Fixture | New/CPython JIT time | New/CPython JIT RSS |
| --- | ---: | ---: |
| `pickle_bench` | 479.394 | 2.638 |
| `deltablue` | 20.933 | 2.243 |
| `datetime_ops` | 19.119 | 2.253 |
| `deque_ops` | 15.595 | 2.110 |
| `list_ops` | 15.018 | 2.056 |

The executable contains 44,138,272 bytes, compared with 44,184,864 at the checkpoint. Build latency was not measured under controlled conditions. The memory figures are process peak RSS, not isolated object sizes or allocation counts. These measurements cover this host and these workloads; they cannot establish superiority for every Python program.

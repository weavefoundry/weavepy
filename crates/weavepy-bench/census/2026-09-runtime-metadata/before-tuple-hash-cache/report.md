# Runtime metadata and numeric text performance

These changes reduce object metadata storage, dictionary bookkeeping, float-formatting allocations, and JSON key allocations. They also extend compiled enumeration and list builders. WeavePy still does not outperform CPython across all workloads or all measured metrics.

The baseline is commit `9a69c4161ade6527a5f4457d7e7272075cb1f0d9`. The candidate uses the same release profile and default JIT feature on macOS ARM64, with CPython 3.14.7 as the reference. [Raw samples, checksums, and reproduction instructions](./) include intermediate stages as well as these measured results.

The measured executable is `target/release/weavepy-runtime-hash-normalization` with SHA-256 `cd9d7362f00fb642f99a3c980811b0f794cf420cf24de6be7c332ce633d5eb01`.

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

## Validation

All 181 compatibility checks pass, including threading, forking, tracing, GC, pickle, JSON, floats, complex numbers, and dictionary comparisons. VM unit tests, JIT tests, Clippy, compilation without the JIT, and workspace checks also pass.

The float oracle checks 262,376 binary64 values in `repr`, `str`, complex text, and JSON output. It finds no differences from CPython; the checkpoint differed on 66 values. Seeded datetime comparisons retain the checkpoint's five known timedelta microsecond differences.

## Standard suite

Values are candidate/reference ratios; smaller values mean less time or memory. Geometric means give each fixture equal weight. The workload aggregate includes all 23 timed workloads; the process aggregates include all 24 fixtures, including startup.

| Metric | New/base JIT | New/base interpreter | New/CPython JIT | New/CPython interpreter |
| --- | ---: | ---: | ---: | ---: |
| Timed workload | 0.855 | 0.966 | 3.712 | 9.846 |
| Process elapsed time | 0.909 | 0.973 | 3.388 | 5.741 |
| Process CPU time | 0.911 | 0.973 | 3.517 | 5.963 |
| Peak RSS | 0.989 | 0.985 | 2.181 | 2.013 |

| Fixture | New/base JIT time | New/base interpreter time | New/CPython JIT time | New/CPython JIT RSS |
| --- | ---: | ---: | ---: | ---: |
| `fannkuch` | 0.991 | 0.917 | 10.570 | 2.065 |
| `nbody` | 0.940 | 0.983 | 8.950 | 2.058 |
| `fib` | 0.997 | 1.013 | 2.974 | 2.085 |
| `pidigits` | 1.013 | 0.977 | 0.911 | 2.063 |
| `pyaes` | 0.055 | 0.980 | 0.680 | 2.060 |
| `richards` | 1.033 | 1.004 | 8.936 | 2.069 |
| `sumvm` | 1.001 | 1.048 | 0.052 | 2.088 |
| `nested_loops` | 0.997 | 1.008 | 0.088 | 2.080 |
| `jitloop` | 0.999 | 0.958 | 0.068 | 2.073 |
| `jitkernels` | 0.980 | 1.025 | 0.756 | 2.057 |
| `deltablue` | 0.954 | 0.985 | 18.189 | 2.251 |
| `float_math` | 0.997 | 0.930 | 8.380 | 3.124 |
| `spectral_norm` | 0.979 | 0.973 | 2.547 | 2.089 |
| `json_bench` | 0.785 | 0.773 | 1.112 | 2.762 |
| `str_methods` | 1.025 | 0.975 | 3.084 | 2.169 |
| `dict_ops` | 0.919 | 0.905 | 5.856 | 2.049 |
| `list_ops` | 1.076 | 0.939 | 15.270 | 2.057 |
| `attr_access` | 1.009 | 0.988 | 3.830 | 2.206 |
| `call_overhead` | 0.970 | 1.003 | 7.577 | 2.073 |
| `generators` | 0.983 | 0.954 | 9.997 | 2.079 |
| `deque_ops` | 0.789 | 0.963 | 18.970 | 2.120 |
| `datetime_ops` | 0.960 | 0.979 | 155.087 | 2.252 |
| `pickle_bench` | 0.981 | 0.983 | 336.489 | 2.672 |
| `startup` | 0.999 | 0.957 | 1.419 | 2.071 |

The CPython datetime reference varies substantially between measurement runs, including process CPU time. A separate post-census diagnostic confirms native datetime constructors and arithmetic in its processes, but doesn't identify what earlier processes loaded or explain the timing difference. [Diagnostic samples](cpython-datetime-diagnostic.json) and the [original-file repeat](cpython-datetime-original-repeat.json) are retained alongside the original census. Treat CPython ratios as measurements of the recorded runs, rather than stable ratios across censuses.

A separate nine-cycle repeat checks the apparent regressions in the byte scrambler, list operations, and string methods. It retains the standard work values and includes first-run execution, as the main suite does.

| Fixture | Repeated new/base JIT time | Repeated new/base interpreter time |
| --- | ---: | ---: |
| `pyaes` | 0.054 | 0.944 |
| `list_ops` | 0.994 | 1.002 |
| `str_methods` | 0.985 | 0.984 |

## Supplemental probes

Elapsed times are workload medians in milliseconds. CPU and RSS ratios are medians of paired candidate/reference samples. Workload CPU excludes setup; peak RSS covers the entire process.

| Probe | Baseline ms | New ms | CPython ms | New/base workload CPU | New/base RSS | New/CPython RSS |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `plain_instances` | 279.6148 | 249.7363 | 13.7050 | 0.887 | 0.890 | 3.921 |
| `slotted_instances` | 291.0565 | 240.5169 | 11.8263 | 0.834 | 0.698 | 3.583 |
| `memoryviews` | 90.6352 | 86.4073 | 22.1695 | 0.943 | 0.734 | 1.534 |
| `memoryview_access` | 38.7909 | 35.0407 | 5.7142 | 0.930 | 0.992 | 1.921 |
| `materialized_frames` | 28.5678 | 28.1865 | 3.7027 | 0.952 | 0.905 | 2.107 |
| `bytesio_streams` | 40.1652 | 35.6488 | 3.5368 | 0.862 | 0.843 | 2.975 |
| `type_creation` | 56.6890 | 54.7810 | 40.7870 | 1.002 | 0.954 | 1.212 |
| `float_repr` | 55.4015 | 25.6441 | 26.3895 | 0.458 | 0.991 | 1.691 |
| `float_str` | 57.8147 | 26.2945 | 25.1984 | 0.461 | 0.991 | 1.693 |
| `int_repr` | 25.0654 | 20.1550 | 5.1098 | 0.810 | 0.994 | 1.698 |
| `complex_repr` | 86.9730 | 32.8081 | 42.9369 | 0.378 | 0.992 | 1.731 |
| `json_float_array` | 116.1026 | 17.5672 | 75.2892 | 0.153 | 0.997 | 2.405 |
| `json_int_array` | 42.5129 | 13.5153 | 16.4228 | 0.316 | 0.997 | 2.354 |
| `json_repeated_keys` | 66.4110 | 46.8743 | 46.7032 | 0.711 | 0.993 | 2.461 |
| `json_unique_keys` | 36.8437 | 31.8488 | 26.4238 | 0.862 | 0.944 | 2.449 |

## Instance and hash storage

These interpreter-mode probes compare compact caches and inline slots with the preceding native-list-cleanup build. Nine paired cycles retain allocation, destruction, repeated access, and hash workloads. RSS covers the whole process.

| Probe | New/previous time | New/previous workload CPU | New/previous RSS | New/CPython time | New/CPython RSS |
| --- | ---: | ---: | ---: | ---: | ---: |
| `empty_instances` | 0.969 | 0.969 | 0.962 | 19.248 | 3.450 |
| `one_slot_instances` | 0.915 | 0.914 | 0.759 | 20.081 | 3.578 |
| `two_slot_instances` | 0.988 | 0.988 | 0.972 | 19.977 | 4.164 |
| `many_slot_instances` | 0.998 | 0.999 | 0.985 | 17.615 | 4.208 |
| `singleton_frozensets` | 0.980 | 0.981 | 0.955 | 4.903 | 1.440 |
| `cached_frozenset_hashes` | 0.995 | 0.985 | 0.966 | 3.274 | 1.250 |
| `cached_weakref_hashes` | 0.969 | 0.961 | 0.927 | 18.424 | 4.049 |
| `slot_access_1` | 0.989 | 0.976 | 0.997 | 8.052 | 1.910 |
| `slot_access_2` | 1.043 | 1.019 | 0.999 | 10.332 | 1.909 |
| `slot_access_8` | 1.046 | 1.054 | 0.998 | 8.902 | 1.912 |

A separate comparison isolates the eight-byte cache and hash protocol fixes against the immediately preceding small-tuple release. Each row retains nine paired cycles. This comparison includes the larger-slot workload even where elapsed time doesn't improve.

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
| `tuple_singletons` | 0.949 | 0.945 | 0.998 | 6.438 | 1.860 |
| `tuple_pairs` | 0.972 | 0.968 | 0.994 | 7.409 | 1.719 |
| `tuple_triples` | 0.965 | 0.976 | 1.004 | 5.746 | 1.566 |
| `tuple_eight_items` | 1.007 | 0.996 | 0.999 | 7.482 | 2.131 |
| `tuple_pair_churn` | 0.934 | 0.949 | 1.001 | 6.079 | 1.921 |
| `dictionary_items` | 0.871 | 0.887 | 0.980 | 5.494 | 1.528 |
| `enumeration_items` | 0.870 | 0.891 | 0.989 | 2.735 | 1.663 |
| `string_partition` | 1.029 | 0.982 | 1.005 | 6.943 | 1.841 |

The preceding small-tuple candidate had a focused partition run with higher elapsed time despite slightly lower CPU use. Its separate 19-cycle repeat used 0.984 times the preceding elapsed time, 0.983 times its workload CPU, and 0.999 times its peak RSS. The [initial and repeated samples](../small-tuples-initial/) remain available.

## Enumeration

These warm probes isolate the JIT change against the preserved metadata and JSON build. The interpreter comparison is a control. Each workload processes 2,048 elements per call and calls the checksum function 200 times.

| Probe | New/previous JIT time | New/previous interpreter time | New/CPython JIT time | New/CPython JIT RSS |
| --- | ---: | ---: | ---: | ---: |
| `enumerate_bytes` | 0.049 | 0.902 | 0.501 | 2.056 |
| `enumerate_indices` | 0.109 | 0.826 | 1.042 | 2.058 |
| `enumerate_local_indices` | 0.109 | 0.843 | 0.979 | 2.064 |

## List builders

These checked warm workloads build 1,024-element results 200 times. The previous build includes the targeted interpreter alignment, so this comparison covers tagged appends and native list cleanup. Nine paired cycles measure time, CPU use, and peak RSS. Each builder compiles without repeated deoptimizations in the separate trace check.

| Probe | New/previous JIT time | New/previous interpreter time | New/CPython JIT time | New/CPython workload CPU | New/CPython JIT RSS |
| --- | ---: | ---: | ---: | ---: | ---: |
| `list_int_builder` | 0.029 | 1.000 | 0.356 | 0.355 | 2.013 |
| `list_float_builder` | 0.025 | 1.003 | 0.321 | 0.323 | 2.017 |
| `list_bool_builder` | 0.029 | 0.999 | 0.496 | 0.502 | 2.030 |
| `byte_list_builder` | 0.047 | 0.953 | 0.577 | 0.576 | 2.045 |

## Startup and imports

These interpreter-mode probes retain 31 paired samples after a discarded warmup cycle, using the staged standard library. Times include the whole process. The 95th percentile uses the nearest-rank sample; it is a local observation, not a latency guarantee.

| Probe | Baseline median ms | New median ms | CPython median ms | Baseline p95 ms | New p95 ms | CPython p95 ms | New/base RSS | New/CPython RSS |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| `startup` | 29.468 | 29.348 | 21.462 | 32.072 | 32.646 | 23.988 | 0.992 | 1.930 |
| `startup_no_site` | 9.344 | 9.290 | 14.625 | 10.005 | 10.214 | 16.039 | 0.997 | 1.545 |
| `imports` | 69.159 | 68.908 | 24.963 | 77.768 | 73.588 | 26.519 | 0.994 | 2.765 |

## Eight-thread execution

The checked concurrency probe uses the existing arithmetic kernel with 1,000,000 iterations per worker. It starts timing before releasing an event gate and verifies all eight results. Thread creation is outside the timer; event release, worker execution, and joins are inside. New worker threads retain their normal JIT initialization costs. Each GIL mode runs separately with five measured cycles and one discarded cycle.

Both WeavePy modes request the JIT; runtime gating still applies. The installed CPython is a GIL build, so this is not a comparison with free-threaded CPython. Larger serial/parallel speedups are better; smaller time, CPU, and memory ratios are better.

| WeavePy mode | New/base parallel time | New/CPython parallel time | New/base parallel CPU | New/CPython parallel CPU | New/base RSS | New/CPython RSS | New serial/parallel speedup | CPython speedup |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| GIL enabled | 1.000 | 0.108 | 1.000 | 0.108 | 0.994 | 3.943 | 0.924 | 0.984 |
| GIL disabled | 0.920 | 0.834 | 0.941 | 5.831 | 0.993 | 3.657 | 3.977 | 1.029 |

## Remaining gaps

With the JIT enabled, the candidate uses less workload time than CPython in 6 of 23 standard timed fixtures and less peak process memory in 0. The largest remaining time ratios are below. These gaps remain part of the measurement set, including workloads that use pure-Python standard-library implementations where CPython has native implementations.

| Fixture | New/CPython JIT time | New/CPython JIT RSS |
| --- | ---: | ---: |
| `pickle_bench` | 336.489 | 2.672 |
| `datetime_ops` | 155.087 | 2.252 |
| `deque_ops` | 18.970 | 2.120 |
| `deltablue` | 18.189 | 2.251 |
| `list_ops` | 15.270 | 2.057 |

The executable contains 44,138,384 bytes, compared with 44,184,864 at the checkpoint. Build latency was not measured under controlled conditions. The memory figures are process peak RSS, not isolated object sizes or allocation counts. These measurements cover this host and these workloads; they cannot establish superiority for every Python program.

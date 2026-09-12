# Runtime metadata and numeric text performance

These changes reduce object metadata storage, dictionary bookkeeping, float-formatting allocations, and JSON key allocations. They also extend compiled enumeration and list builders. WeavePy still does not outperform CPython across all workloads or all measured metrics.

The baseline is commit `9a69c4161ade6527a5f4457d7e7272075cb1f0d9`. The candidate uses the same release profile and default JIT feature on macOS ARM64, with CPython 3.14.7 as the reference. [Raw samples, checksums, and reproduction instructions](../) include intermediate stages as well as these measured results.

The measured executable is `target/release/weavepy-runtime-small-tuples` with SHA-256 `43c70898ea0a57a7aa9ce2db7e6e2aeced2b5e3076c642f28dde525329920181`.

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
- Cache optional hashes in 16 bytes with atomic values and publication bits, preserving the full integer range. Instances and frozen sets previously paid for 40-byte locked optional hashes.
- Store the first instance slot inline on 64-bit targets, avoiding a table allocation. A second distinct slot promotes to an ordered table; updates reuse the existing key. Smaller targets retain ordinary table storage.

- Construct fixed-size tuples directly from arrays, removing the intermediate vector allocation. Bytecode tuples with up to three elements retain the existing free list, and immutable scalar enumeration values use the inline operand copier.

## Validation

All 177 compatibility checks pass, including threading, forking, tracing, GC, pickle, JSON, floats, complex numbers, and dictionary comparisons. VM unit tests, JIT tests, Clippy, compilation without the JIT, and workspace checks also pass.

The float oracle checks 262,376 binary64 values in `repr`, `str`, complex text, and JSON output. It finds no differences from CPython; the checkpoint differed on 66 values. Seeded datetime comparisons retain the checkpoint's five known timedelta microsecond differences.

## Standard suite

Values are candidate/reference ratios; smaller values mean less time or memory. Geometric means give each fixture equal weight. The workload aggregate includes all 23 timed workloads; the process aggregates include all 24 fixtures, including startup.

| Metric | New/base JIT | New/base interpreter | New/CPython JIT | New/CPython interpreter |
| --- | ---: | ---: | ---: | ---: |
| Timed workload | 0.857 | 1.032 | 3.659 | 10.436 |
| Process elapsed time | 0.910 | 1.035 | 3.371 | 6.186 |
| Process CPU time | 0.911 | 0.984 | 3.518 | 6.084 |
| Peak RSS | 0.988 | 0.983 | 2.180 | 2.010 |

| Fixture | New/base JIT time | New/base interpreter time | New/CPython JIT time | New/CPython JIT RSS |
| --- | ---: | ---: | ---: | ---: |
| `fannkuch` | 0.966 | 0.966 | 10.424 | 2.065 |
| `nbody` | 0.980 | 0.981 | 8.848 | 2.049 |
| `fib` | 1.005 | 0.987 | 2.993 | 2.082 |
| `pidigits` | 0.981 | 1.001 | 0.898 | 2.065 |
| `pyaes` | 0.054 | 0.925 | 0.653 | 2.050 |
| `richards` | 1.000 | 0.967 | 8.498 | 2.070 |
| `sumvm` | 0.989 | 0.993 | 0.056 | 2.077 |
| `nested_loops` | 1.018 | 1.012 | 0.085 | 2.081 |
| `jitloop` | 1.008 | 0.982 | 0.073 | 2.082 |
| `jitkernels` | 0.991 | 0.987 | 0.891 | 2.073 |
| `deltablue` | 0.985 | 0.947 | 20.081 | 2.227 |
| `float_math` | 0.973 | 0.954 | 8.196 | 3.206 |
| `spectral_norm` | 0.997 | 0.991 | 2.554 | 2.079 |
| `json_bench` | 0.765 | 0.758 | 1.137 | 2.720 |
| `str_methods` | 0.991 | 0.994 | 3.107 | 2.154 |
| `dict_ops` | 0.916 | 0.922 | 5.389 | 2.049 |
| `list_ops` | 0.987 | 0.986 | 13.686 | 2.062 |
| `attr_access` | 0.925 | 3.718 | 3.417 | 2.263 |
| `call_overhead` | 0.969 | 1.281 | 6.368 | 2.076 |
| `generators` | 1.026 | 0.961 | 9.966 | 2.078 |
| `deque_ops` | 0.984 | 1.073 | 15.799 | 2.108 |
| `datetime_ops` | 1.008 | 0.904 | 161.102 | 2.231 |
| `pickle_bench` | 0.942 | 0.935 | 349.712 | 2.639 |
| `startup` | 0.993 | 0.988 | 1.420 | 2.071 |

A separate nine-cycle repeat checks the apparent regressions in the byte scrambler, list operations, and string methods. It retains the standard work values and includes first-run execution, as the main suite does.

| Fixture | Repeated new/base JIT time | Repeated new/base interpreter time |
| --- | ---: | ---: |
| `pyaes` | 0.054 | 0.940 |
| `list_ops` | 0.937 | 1.005 |
| `str_methods` | 0.986 | 0.984 |

## Supplemental probes

Elapsed times are workload medians in milliseconds. CPU and RSS ratios are medians of paired candidate/reference samples. Workload CPU excludes setup; peak RSS covers the entire process.

| Probe | Baseline ms | New ms | CPython ms | New/base workload CPU | New/base RSS | New/CPython RSS |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `plain_instances` | 297.9851 | 288.2432 | 15.6961 | 0.936 | 0.916 | 4.040 |
| `slotted_instances` | 344.6402 | 298.4171 | 14.3782 | 0.867 | 0.723 | 3.717 |
| `memoryviews` | 105.4045 | 96.5703 | 25.0627 | 0.913 | 0.733 | 1.534 |
| `memoryview_access` | 43.8941 | 41.3436 | 6.6784 | 0.940 | 0.984 | 1.929 |
| `materialized_frames` | 33.2050 | 31.7450 | 4.0584 | 0.959 | 0.903 | 2.106 |
| `bytesio_streams` | 39.4080 | 34.4076 | 3.1977 | 0.877 | 0.842 | 2.983 |
| `type_creation` | 58.0839 | 56.2456 | 44.7686 | 0.969 | 0.953 | 1.211 |
| `float_repr` | 64.9345 | 29.9256 | 29.9403 | 0.461 | 0.988 | 1.695 |
| `float_str` | 68.0354 | 30.4535 | 28.8782 | 0.449 | 0.982 | 1.687 |
| `int_repr` | 29.4869 | 23.7402 | 6.1048 | 0.806 | 0.988 | 1.699 |
| `complex_repr` | 103.3094 | 38.5863 | 50.6823 | 0.379 | 0.987 | 1.729 |
| `json_float_array` | 133.5180 | 20.0320 | 87.8171 | 0.151 | 0.992 | 2.381 |
| `json_int_array` | 51.0654 | 15.1881 | 20.0895 | 0.302 | 0.995 | 2.335 |
| `json_repeated_keys` | 103.4288 | 73.4088 | 70.2882 | 0.696 | 0.997 | 2.460 |
| `json_unique_keys` | 53.2075 | 45.8000 | 39.7482 | 0.868 | 0.965 | 2.465 |

## Instance and hash storage

These interpreter-mode probes compare compact caches and inline slots with the preceding native-list-cleanup build. Nine paired cycles retain allocation, destruction, repeated access, and hash workloads. RSS covers the whole process.

| Probe | New/previous time | New/previous workload CPU | New/previous RSS | New/CPython time | New/CPython RSS |
| --- | ---: | ---: | ---: | ---: | ---: |
| `empty_instances` | 1.009 | 1.010 | 1.000 | 20.475 | 3.584 |
| `one_slot_instances` | 0.961 | 0.961 | 0.787 | 20.554 | 3.714 |
| `two_slot_instances` | 0.990 | 0.996 | 0.988 | 19.562 | 4.264 |
| `many_slot_instances` | 1.059 | 1.007 | 0.999 | 20.664 | 4.256 |
| `singleton_frozensets` | 0.984 | 0.971 | 0.976 | 4.978 | 1.476 |
| `cached_frozenset_hashes` | 0.964 | 0.961 | 0.996 | 4.039 | 1.279 |
| `cached_weakref_hashes` | 0.946 | 0.943 | 0.941 | 17.225 | 4.139 |
| `slot_access_1` | 0.969 | 0.984 | 1.001 | 8.014 | 1.917 |
| `slot_access_2` | 0.950 | 0.947 | 1.000 | 9.662 | 1.917 |
| `slot_access_8` | 1.087 | 1.027 | 1.003 | 8.377 | 1.920 |

## Tuple construction

These interpreter-mode probes compare direct tuple allocation with the preceding tuple-enumeration build. Nine paired cycles cover retained tuples, allocation and reuse, dictionary items, enumeration, and string partitioning. The eight-element literal retains the general bytecode construction path.

| Probe | New/previous time | New/previous workload CPU | New/previous RSS | New/CPython time | New/CPython RSS |
| --- | ---: | ---: | ---: | ---: | ---: |
| `tuple_singletons` | 0.900 | 0.918 | 0.995 | 6.755 | 1.843 |
| `tuple_pairs` | 0.955 | 0.959 | 1.008 | 8.571 | 1.690 |
| `tuple_triples` | 0.942 | 0.969 | 0.992 | 6.141 | 1.573 |
| `tuple_eight_items` | 1.043 | 1.004 | 1.000 | 9.732 | 2.130 |
| `tuple_pair_churn` | 1.011 | 0.946 | 0.996 | 8.356 | 1.914 |
| `dictionary_items` | 0.880 | 0.892 | 1.012 | 9.678 | 1.568 |
| `enumeration_items` | 0.874 | 0.891 | 0.990 | 4.314 | 1.654 |
| `string_partition` | 0.975 | 0.979 | 0.994 | 11.764 | 1.816 |

An earlier focused partition run had higher elapsed time despite slightly lower CPU use. The separate 19-cycle repeat uses 0.984 times the preceding elapsed time, 0.983 times its workload CPU, and 0.999 times its peak RSS. The [initial and repeated samples](../small-tuples-initial/) remain available.

## Enumeration

These warm probes isolate the JIT change against the preserved metadata and JSON build. The interpreter comparison is a control. Each workload processes 2,048 elements per call and calls the checksum function 200 times.

| Probe | New/previous JIT time | New/previous interpreter time | New/CPython JIT time | New/CPython JIT RSS |
| --- | ---: | ---: | ---: | ---: |
| `enumerate_bytes` | 0.051 | 0.880 | 0.526 | 2.056 |
| `enumerate_indices` | 0.111 | 0.846 | 1.049 | 2.048 |
| `enumerate_local_indices` | 0.111 | 0.839 | 1.038 | 2.048 |

## List builders

These checked warm workloads build 1,024-element results 200 times. The previous build includes the targeted interpreter alignment, so this comparison covers tagged appends and native list cleanup. Nine paired cycles measure time, CPU use, and peak RSS. Each builder compiles without repeated deoptimizations in the separate trace check.

| Probe | New/previous JIT time | New/previous interpreter time | New/CPython JIT time | New/CPython workload CPU | New/CPython JIT RSS |
| --- | ---: | ---: | ---: | ---: | ---: |
| `list_int_builder` | 0.030 | 0.996 | 0.352 | 0.351 | 2.003 |
| `list_float_builder` | 0.026 | 1.005 | 0.326 | 0.327 | 2.006 |
| `list_bool_builder` | 0.030 | 0.998 | 0.525 | 0.524 | 2.023 |
| `byte_list_builder` | 0.049 | 0.941 | 0.585 | 0.582 | 2.033 |

## Startup and imports

These interpreter-mode probes retain 31 paired samples after a discarded warmup cycle, using the staged standard library. Times include the whole process. The 95th percentile uses the nearest-rank sample; it is a local observation, not a latency guarantee.

| Probe | Baseline median ms | New median ms | CPython median ms | Baseline p95 ms | New p95 ms | CPython p95 ms | New/base RSS | New/CPython RSS |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| `startup` | 40.160 | 39.594 | 28.764 | 47.584 | 52.953 | 34.219 | 0.986 | 1.927 |
| `startup_no_site` | 12.599 | 12.207 | 19.882 | 14.469 | 14.438 | 22.496 | 0.990 | 1.540 |
| `imports` | 115.915 | 114.055 | 40.240 | 175.008 | 155.696 | 66.734 | 0.993 | 2.705 |

## Eight-thread execution

The checked concurrency probe uses the existing arithmetic kernel with 1,000,000 iterations per worker. It starts timing before releasing an event gate and verifies all eight results. Thread creation is outside the timer; event release, worker execution, and joins are inside. New worker threads retain their normal JIT initialization costs. Each GIL mode runs separately with five measured cycles and one discarded cycle.

Both WeavePy modes request the JIT; runtime gating still applies. The installed CPython is a GIL build, so this is not a comparison with free-threaded CPython. Larger serial/parallel speedups are better; smaller time, CPU, and memory ratios are better.

| WeavePy mode | New/base parallel time | New/CPython parallel time | New/base parallel CPU | New/CPython parallel CPU | New/base RSS | New/CPython RSS | New serial/parallel speedup | CPython speedup |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| GIL enabled | 1.023 | 0.104 | 1.024 | 0.104 | 0.990 | 3.826 | 0.944 | 0.992 |
| GIL disabled | 0.988 | 0.717 | 0.955 | 4.173 | 0.996 | 3.589 | 4.632 | 1.029 |

## Remaining gaps

With the JIT enabled, the candidate uses less workload time than CPython in 6 of 23 standard timed fixtures and less peak process memory in 0. The largest remaining time ratios are below. These gaps remain part of the measurement set, including workloads that use pure-Python standard-library implementations where CPython has native implementations.

| Fixture | New/CPython JIT time | New/CPython JIT RSS |
| --- | ---: | ---: |
| `pickle_bench` | 349.712 | 2.639 |
| `datetime_ops` | 161.102 | 2.231 |
| `deltablue` | 20.081 | 2.227 |
| `deque_ops` | 15.799 | 2.108 |
| `list_ops` | 13.686 | 2.062 |

The executable contains 44,119,520 bytes, compared with 44,184,864 at the checkpoint. Build latency was not measured under controlled conditions. The memory figures are process peak RSS, not isolated object sizes or allocation counts. These measurements cover this host and these workloads; they cannot establish superiority for every Python program.

## Scheduling anomaly repeats

The original five-cycle suite contains large scheduling delays. These separate nine-cycle repeats retain every original sample and use the same work sizes. Ratios compare the small-tuple candidate with the original checkpoint; values below one mean less time. CPU columns include the whole process.

| Fixture | Original JIT time | Repeat JIT time | Original interpreter time | Repeat interpreter time | Repeat JIT CPU | Repeat interpreter CPU |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `attr_access` | 0.925 | 1.067 | 3.718 | 0.991 | 1.022 | 0.996 |
| `call_overhead` | 0.969 | 0.954 | 1.281 | 1.007 | 0.982 | 0.981 |
| `deque_ops` | 0.984 | 1.013 | 1.073 | 1.007 | 1.002 | 0.995 |
| `datetime_ops` | 1.008 | 0.970 | 0.904 | 0.941 | 0.973 | 0.962 |
| `deltablue` | 0.985 | 0.918 | 0.947 | 0.937 | 0.967 | 0.963 |

The large interpreted attribute-access and call-overhead slowdowns do not reproduce. Attribute access still has a 6.7 percent JIT elapsed-time increase in the repeat, with a 2.2 percent process CPU increase. Those regressions remain visible.

CPython datetime CPU time also varies substantially between these runs. Earlier samples are around 300 ms of process CPU, while some initial small-tuple samples use under 50 ms; the datetime workload size remains 60,000. Scheduling delay alone does not explain that CPU difference. The older runs did not capture the loaded datetime module identity, so the cause is unresolved. Ratios to CPython from different runs must not be interpreted as implementation deltas.

A further nine-cycle comparison isolates small-tuple construction against the preceding tuple-enumeration executable.

| Fixture | JIT time | Interpreter time | JIT CPU | Interpreter CPU | JIT RSS |
| --- | ---: | ---: | ---: | ---: | ---: |
| `attr_access` | 1.034 | 1.003 | 1.009 | 1.004 | 1.000 |
| `call_overhead` | 1.010 | 1.009 | 1.008 | 0.994 | 1.004 |

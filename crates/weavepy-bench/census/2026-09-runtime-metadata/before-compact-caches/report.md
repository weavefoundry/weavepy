# Runtime metadata and numeric text performance

These changes reduce object metadata storage, dictionary bookkeeping, float-formatting allocations, and JSON key allocations. They also extend compiled enumeration and list builders. WeavePy still does not outperform CPython across all workloads or all measured metrics.

The baseline is commit `9a69c4161ade6527a5f4457d7e7272075cb1f0d9`. The candidate uses the same release profile and default JIT feature on macOS ARM64, with CPython 3.14.7 as the reference. [Raw samples, checksums, and reproduction instructions](../crates/weavepy-bench/census/2026-09-runtime-metadata/) include intermediate stages as well as these final results.

## Changes

- Replace lock-backed cells for small copyable metadata with native atomic cells. Borrowed payloads and fields without native atomic support retain their existing locks, including fork recovery. A boolean cell occupies one byte and a 64-bit cell occupies eight.
- Keep dictionary comparison bookkeeping and built-in type registry handles in ordinary thread-local cells. Shared Python objects retain their synchronization, and nested key comparisons preserve outer errors.
- Format float digits with the already locked Zmij dependency, adapting its notation to Python's rules. Write exact JSON integers and finite floats directly into the output buffer. This also fixes shortest-decimal rounding ties that previously differed from CPython.
- Construct Python strings for native scalars directly from stack buffers. Scalar subclasses retain their protocol dispatch, and large integers retain their conversion limits.
- Look up unescaped UTF-8 JSON keys before allocating string storage. Escaped spellings still share identity with equal literal keys; surrogate-bearing strings keep their existing representation.
- Build decoded JSON dictionaries as fields arrive. Only the pairs hook stages a pair vector. Overwritten values are released before the next field, matching CPython's callback timing.
- Infer integer index and byte lanes for certified `enumerate(bytes)` calls. Generic enumeration retains an integer index and an object value. Existing runtime guards validate consumed values and preserve them when execution returns to the interpreter. Generic pair loops can keep immutable primitive values boxed without an immediate exit.
- Read native byte enumeration into the compiled pair lanes without allocating temporary Python tuples. Shared iterator positions remain current, and counter overflow falls back before consuming a byte. Known local objects also retain their inferred enumeration lanes.
- Align three interpreter entry points independently on macOS ARM64. Paired experiments recovered code-placement regressions without the executable growth of aligning every function. Other targets retain their existing layout.
- Stage scalar type tags for compiled list appends, allowing fresh generic lists to accept integers, floats, and booleans without leaving native code. Typed lists keep their existing element constraints, and failures resume before mutating the list.
- Reap temporary list pins on native frame exit. Previously, the collector retained those lists until a later collection; cleanup now releases them and their dead children promptly, including when cyclic collection is disabled. Escaped lists stay alive.

## Validation

All 169 compatibility checks pass, including threading, forking, tracing, GC, pickle, JSON, floats, complex numbers, and dictionary comparisons. VM unit tests, JIT tests, Clippy, compilation without the JIT, and workspace checks also pass.

The float oracle checks 262,376 binary64 values in `repr`, `str`, complex text, and JSON output. It finds no differences from CPython; the checkpoint differed on 66 values. Seeded datetime comparisons retain the checkpoint's five known timedelta microsecond differences.

## Standard suite

Values are candidate/reference ratios; smaller values mean less time or memory. Geometric means give each fixture equal weight. The workload aggregate includes all 23 timed workloads; the process aggregates include all 24 fixtures, including startup.

| Metric | New/base JIT | New/base interpreter | New/CPython JIT | New/CPython interpreter |
| --- | ---: | ---: | ---: | ---: |
| Timed workload | 0.860 | 0.950 | 3.343 | 9.147 |
| Process elapsed time | 0.908 | 0.953 | 3.138 | 5.350 |
| Process CPU time | 0.906 | 0.968 | 3.275 | 5.578 |
| Peak RSS | 0.987 | 0.982 | 2.171 | 2.001 |

| Fixture | New/base JIT time | New/base interpreter time | New/CPython JIT time | New/CPython JIT RSS |
| --- | ---: | ---: | ---: | ---: |
| `fannkuch` | 0.966 | 0.966 | 11.767 | 2.056 |
| `nbody` | 0.993 | 0.982 | 8.977 | 2.041 |
| `fib` | 1.011 | 0.991 | 3.418 | 2.081 |
| `pidigits` | 0.984 | 0.907 | 0.950 | 2.045 |
| `pyaes` | 0.049 | 0.936 | 0.645 | 2.048 |
| `richards` | 1.029 | 0.846 | 6.797 | 2.063 |
| `sumvm` | 0.957 | 0.965 | 0.056 | 2.064 |
| `nested_loops` | 1.057 | 0.972 | 0.060 | 2.062 |
| `jitloop` | 1.003 | 0.981 | 0.050 | 2.073 |
| `jitkernels` | 0.995 | 1.018 | 0.713 | 2.055 |
| `deltablue` | 1.049 | 0.956 | 20.575 | 2.237 |
| `float_math` | 0.976 | 1.031 | 6.978 | 3.202 |
| `spectral_norm` | 1.035 | 0.937 | 2.766 | 2.064 |
| `json_bench` | 0.792 | 0.815 | 1.168 | 2.729 |
| `str_methods` | 1.055 | 1.033 | 3.401 | 2.145 |
| `dict_ops` | 0.949 | 0.818 | 5.737 | 2.023 |
| `list_ops` | 1.019 | 0.997 | 13.931 | 2.045 |
| `attr_access` | 1.019 | 0.971 | 4.077 | 2.280 |
| `call_overhead` | 0.988 | 0.958 | 8.313 | 2.056 |
| `generators` | 0.924 | 0.965 | 10.925 | 2.077 |
| `deque_ops` | 0.891 | 0.967 | 22.263 | 2.097 |
| `datetime_ops` | 0.982 | 0.921 | 17.508 | 2.215 |
| `pickle_bench` | 0.917 | 0.970 | 344.247 | 2.640 |
| `startup` | 1.020 | 0.989 | 1.470 | 2.061 |

A separate nine-cycle repeat checks the apparent regressions in the byte scrambler, list operations, and string methods. It retains the standard work values and includes first-run execution, as the main suite does.

| Fixture | Repeated new/base JIT time | Repeated new/base interpreter time |
| --- | ---: | ---: |
| `pyaes` | 0.053 | 1.001 |
| `list_ops` | 0.969 | 0.996 |
| `str_methods` | 0.986 | 1.001 |

## Supplemental probes

Elapsed times are workload medians in milliseconds. CPU and RSS ratios are medians of paired candidate/reference samples. Workload CPU excludes setup; peak RSS covers the entire process.

| Probe | Baseline ms | New ms | CPython ms | New/base workload CPU | New/base RSS | New/CPython RSS |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `plain_instances` | 344.2409 | 302.9860 | 16.5858 | 0.919 | 0.916 | 4.040 |
| `slotted_instances` | 319.3149 | 290.3879 | 12.8847 | 0.913 | 0.919 | 4.713 |
| `memoryviews` | 101.1697 | 99.0106 | 24.7149 | 0.975 | 0.732 | 1.534 |
| `memoryview_access` | 39.8488 | 38.1975 | 6.0992 | 0.938 | 0.985 | 1.922 |
| `materialized_frames` | 29.3000 | 28.0053 | 3.6172 | 0.951 | 0.899 | 2.103 |
| `bytesio_streams` | 36.4180 | 30.8092 | 2.9453 | 0.854 | 0.841 | 2.978 |
| `type_creation` | 49.7052 | 48.3372 | 39.1415 | 0.968 | 0.951 | 1.208 |
| `float_repr` | 57.4287 | 27.0827 | 26.4576 | 0.474 | 0.987 | 1.693 |
| `float_str` | 67.7939 | 29.4892 | 29.4102 | 0.455 | 0.984 | 1.689 |
| `int_repr` | 26.9879 | 22.1156 | 5.5844 | 0.819 | 0.986 | 1.694 |
| `complex_repr` | 87.4419 | 33.2358 | 43.0602 | 0.379 | 0.985 | 1.728 |
| `json_float_array` | 113.4760 | 17.4702 | 74.9766 | 0.153 | 0.996 | 2.396 |
| `json_int_array` | 43.9175 | 12.6984 | 17.2586 | 0.288 | 0.994 | 2.334 |
| `json_repeated_keys` | 71.6007 | 50.6595 | 49.7109 | 0.707 | 0.994 | 2.451 |
| `json_unique_keys` | 36.9348 | 31.9050 | 27.5389 | 0.869 | 0.982 | 2.442 |

## Enumeration

These warm probes isolate the JIT change against the preserved metadata and JSON build. The interpreter comparison is a control. Each workload processes 2,048 elements per call and calls the checksum function 200 times.

| Probe | New/previous JIT time | New/previous interpreter time | New/CPython JIT time | New/CPython JIT RSS |
| --- | ---: | ---: | ---: | ---: |
| `enumerate_bytes` | 0.047 | 0.998 | 0.397 | 2.038 |
| `enumerate_indices` | 0.475 | 0.994 | 4.806 | 2.039 |
| `enumerate_local_indices` | 0.527 | 1.018 | 4.460 | 2.045 |

## List builders

These checked warm workloads build 1,024-element results 200 times. The previous build includes the targeted interpreter alignment, so this comparison covers tagged appends and native list cleanup. Nine paired cycles measure time, CPU use, and peak RSS. Each builder compiles without repeated deoptimizations in the separate trace check.

| Probe | New/previous JIT time | New/previous interpreter time | New/CPython JIT time | New/CPython workload CPU | New/CPython JIT RSS |
| --- | ---: | ---: | ---: | ---: | ---: |
| `list_int_builder` | 0.029 | 0.992 | 0.347 | 0.346 | 2.004 |
| `list_float_builder` | 0.026 | 1.006 | 0.332 | 0.333 | 2.003 |
| `list_bool_builder` | 0.028 | 0.988 | 0.496 | 0.499 | 2.021 |
| `byte_list_builder` | 0.046 | 0.947 | 0.631 | 0.600 | 2.031 |

## Startup and imports

These interpreter-mode probes retain 31 paired samples after a discarded warmup cycle, using the staged standard library. Times include the whole process. The 95th percentile uses the nearest-rank sample; it is a local observation, not a latency guarantee.

| Probe | Baseline median ms | New median ms | CPython median ms | Baseline p95 ms | New p95 ms | CPython p95 ms | New/base RSS | New/CPython RSS |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| `startup` | 30.157 | 30.027 | 21.434 | 32.181 | 31.913 | 23.084 | 0.987 | 1.926 |
| `startup_no_site` | 9.533 | 9.643 | 15.010 | 10.085 | 10.103 | 15.593 | 1.000 | 1.558 |
| `imports` | 75.504 | 74.691 | 26.150 | 77.345 | 77.183 | 27.156 | 0.995 | 2.705 |

## Eight-thread execution

The checked concurrency probe uses the existing arithmetic kernel with 1,000,000 iterations per worker. It starts timing before releasing an event gate and verifies all eight results. Thread creation is outside the timer; event release, worker execution, and joins are inside. New worker threads retain their normal JIT initialization costs. Each GIL mode runs separately with five measured cycles and one discarded cycle.

Both WeavePy modes request the JIT; runtime gating still applies. The installed CPython is a GIL build, so this is not a comparison with free-threaded CPython. Larger serial/parallel speedups are better; smaller time, CPU, and memory ratios are better.

| WeavePy mode | New/base parallel time | New/CPython parallel time | New/base parallel CPU | New/CPython parallel CPU | New/base RSS | New/CPython RSS | New serial/parallel speedup | CPython speedup |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| GIL enabled | 1.010 | 0.104 | 1.010 | 0.104 | 0.994 | 3.849 | 0.934 | 0.945 |
| GIL disabled | 0.861 | 0.782 | 0.873 | 5.313 | 0.993 | 3.589 | 4.021 | 0.987 |

## Remaining gaps

With the JIT enabled, the candidate uses less workload time than CPython in 6 of 23 standard timed fixtures and less peak process memory in 0. The largest remaining time ratios are below. These gaps remain part of the measurement set, including workloads that use pure-Python standard-library implementations where CPython has native implementations.

| Fixture | New/CPython JIT time | New/CPython JIT RSS |
| --- | ---: | ---: |
| `pickle_bench` | 344.247 | 2.640 |
| `deque_ops` | 22.263 | 2.097 |
| `deltablue` | 20.575 | 2.237 |
| `datetime_ops` | 17.508 | 2.215 |
| `list_ops` | 13.931 | 2.045 |

The executable contains 44,099,696 bytes, compared with 44,184,864 at the checkpoint. Build latency was not measured under controlled conditions. The memory figures are process peak RSS, not isolated object sizes or allocation counts. These measurements cover this host and these workloads; they cannot establish superiority for every Python program.

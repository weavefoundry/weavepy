# Runtime metadata and numeric text performance

These changes reduce object metadata storage, dictionary bookkeeping, float-formatting allocations, and JSON key allocations. They also extend JIT type inference for enumeration. WeavePy still does not outperform CPython across all workloads or all measured metrics.

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

## Validation

All 162 compatibility checks pass, including threading, forking, tracing, GC, pickle, JSON, floats, complex numbers, and dictionary comparisons. VM unit tests, JIT tests, Clippy, compilation without the JIT, and workspace checks also pass.

The float oracle checks 262,376 binary64 values in `repr`, `str`, complex text, and JSON output. It finds no differences from CPython; the checkpoint differed on 66 values. Seeded datetime comparisons retain the checkpoint's five known timedelta microsecond differences.

## Standard suite

Values are candidate/reference ratios; smaller values mean less time or memory. Geometric means give each fixture equal weight. The workload aggregate includes all 23 timed workloads; the process aggregates include all 24 fixtures, including startup.

| Metric | New/base JIT | New/base interpreter | New/CPython JIT | New/CPython interpreter |
| --- | ---: | ---: | ---: | ---: |
| Timed workload | 0.985 | 0.986 | 3.868 | 8.927 |
| Process elapsed time | 0.992 | 0.989 | 3.542 | 5.511 |
| Process CPU time | 0.993 | 0.989 | 3.627 | 5.704 |
| Peak RSS | 0.984 | 0.979 | 2.169 | 2.003 |

| Fixture | New/base JIT time | New/base interpreter time | New/CPython JIT time | New/CPython JIT RSS |
| --- | ---: | ---: | ---: | ---: |
| `fannkuch` | 0.961 | 0.970 | 10.466 | 2.044 |
| `nbody` | 0.998 | 1.009 | 9.042 | 2.047 |
| `fib` | 1.001 | 1.017 | 2.975 | 2.080 |
| `pidigits` | 1.002 | 1.004 | 0.905 | 2.043 |
| `pyaes` | 1.049 | 1.055 | 12.570 | 2.022 |
| `richards` | 0.992 | 0.973 | 8.791 | 2.059 |
| `sumvm` | 0.990 | 1.007 | 0.058 | 2.070 |
| `nested_loops` | 1.009 | 1.005 | 0.078 | 2.069 |
| `jitloop` | 1.026 | 1.024 | 0.075 | 2.071 |
| `jitkernels` | 0.992 | 1.047 | 0.874 | 2.064 |
| `deltablue` | 0.971 | 0.982 | 19.802 | 2.230 |
| `float_math` | 0.945 | 0.961 | 8.082 | 3.204 |
| `spectral_norm` | 1.015 | 0.990 | 2.596 | 2.074 |
| `json_bench` | 0.764 | 0.763 | 1.144 | 2.721 |
| `str_methods` | 1.043 | 1.030 | 3.230 | 2.152 |
| `dict_ops` | 0.939 | 0.943 | 5.558 | 2.044 |
| `list_ops` | 1.062 | 1.058 | 14.418 | 2.046 |
| `attr_access` | 1.009 | 0.997 | 3.835 | 2.189 |
| `call_overhead` | 0.994 | 0.999 | 8.205 | 2.056 |
| `generators` | 1.007 | 0.950 | 9.630 | 2.072 |
| `deque_ops` | 0.997 | 0.991 | 17.675 | 2.106 |
| `datetime_ops` | 0.961 | 0.971 | 17.568 | 2.225 |
| `pickle_bench` | 0.976 | 0.977 | 336.017 | 2.639 |
| `startup` | 1.004 | 0.979 | 1.522 | 2.062 |

A separate nine-cycle repeat checks the apparent regressions in the byte scrambler, list operations, and string methods. It retains the standard work values and includes first-run execution, as the main suite does.

| Fixture | Repeated new/base JIT time | Repeated new/base interpreter time |
| --- | ---: | ---: |
| `pyaes` | 1.048 | 1.051 |
| `list_ops` | 1.052 | 1.066 |
| `str_methods` | 1.041 | 1.046 |

## Supplemental probes

Elapsed times are workload medians in milliseconds. CPU and RSS ratios are medians of paired candidate/reference samples. Workload CPU excludes setup; peak RSS covers the entire process.

| Probe | Baseline ms | New ms | CPython ms | New/base workload CPU | New/base RSS | New/CPython RSS |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `plain_instances` | 230.4302 | 211.5826 | 12.0639 | 0.918 | 0.916 | 4.039 |
| `slotted_instances` | 241.4324 | 222.0005 | 10.5520 | 0.920 | 0.918 | 4.712 |
| `memoryviews` | 77.5331 | 71.9432 | 17.2316 | 0.928 | 0.731 | 1.531 |
| `memoryview_access` | 32.4855 | 30.8384 | 4.8515 | 0.948 | 0.983 | 1.922 |
| `materialized_frames` | 24.2649 | 23.4695 | 2.8574 | 0.962 | 0.899 | 2.093 |
| `bytesio_streams` | 28.8182 | 26.7543 | 2.5040 | 0.927 | 0.842 | 2.981 |
| `type_creation` | 41.8174 | 40.6554 | 32.9770 | 0.969 | 0.951 | 1.207 |
| `float_repr` | 48.0033 | 22.5164 | 22.0420 | 0.469 | 0.983 | 1.685 |
| `float_str` | 50.6870 | 23.0020 | 21.9153 | 0.454 | 0.983 | 1.684 |
| `int_repr` | 21.6694 | 17.5929 | 4.4343 | 0.812 | 0.985 | 1.692 |
| `complex_repr` | 75.9097 | 28.5630 | 37.2348 | 0.377 | 0.987 | 1.723 |
| `json_float_array` | 100.2355 | 15.1866 | 65.3224 | 0.150 | 0.991 | 2.373 |
| `json_int_array` | 37.9085 | 11.3911 | 14.9411 | 0.302 | 0.991 | 2.326 |
| `json_repeated_keys` | 59.0735 | 41.3706 | 39.8132 | 0.700 | 0.993 | 2.445 |
| `json_unique_keys` | 31.6359 | 27.5498 | 23.8128 | 0.869 | 0.980 | 2.438 |

## Enumeration

These warm probes isolate the JIT change against the preserved metadata and JSON build. The interpreter comparison is a control. Each workload processes 2,048 elements per call and calls the checksum function 200 times.

| Probe | New/previous JIT time | New/previous interpreter time | New/CPython JIT time | New/CPython JIT RSS |
| --- | ---: | ---: | ---: | ---: |
| `enumerate_bytes` | 0.047 | 0.999 | 0.484 | 2.045 |
| `enumerate_indices` | 0.500 | 0.995 | 4.705 | 2.046 |
| `enumerate_local_indices` | 0.502 | 0.996 | 4.695 | 2.043 |

## Startup and imports

These interpreter-mode probes retain 31 paired samples after a discarded warmup cycle, using the staged standard library. Times include the whole process. The 95th percentile uses the nearest-rank sample; it is a local observation, not a latency guarantee.

| Probe | Baseline median ms | New median ms | CPython median ms | Baseline p95 ms | New p95 ms | CPython p95 ms | New/base RSS | New/CPython RSS |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| `startup` | 24.916 | 24.843 | 17.352 | 25.610 | 25.543 | 18.230 | 0.984 | 1.924 |
| `startup_no_site` | 7.455 | 7.328 | 11.844 | 7.868 | 8.141 | 12.830 | 0.996 | 1.553 |
| `imports` | 62.099 | 61.762 | 20.845 | 63.703 | 63.372 | 21.356 | 0.990 | 2.696 |

## Eight-thread execution

The checked concurrency probe uses the existing arithmetic kernel with 1,000,000 iterations per worker. It starts timing before releasing an event gate and verifies all eight results. Thread creation is outside the timer; event release, worker execution, and joins are inside. New worker threads retain their normal JIT initialization costs. Each GIL mode runs separately with five measured cycles and one discarded cycle.

Both WeavePy modes request the JIT; runtime gating still applies. The installed CPython is a GIL build, so this is not a comparison with free-threaded CPython. Larger serial/parallel speedups are better; smaller time, CPU, and memory ratios are better.

| WeavePy mode | New/base parallel time | New/CPython parallel time | New/base parallel CPU | New/CPython parallel CPU | New/base RSS | New/CPython RSS | New serial/parallel speedup | CPython speedup |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| GIL enabled | 1.000 | 0.113 | 1.000 | 0.113 | 0.992 | 3.830 | 0.921 | 1.015 |
| GIL disabled | 0.986 | 0.644 | 1.008 | 4.513 | 0.989 | 3.570 | 5.322 | 1.044 |

## Remaining gaps

With the JIT enabled, the candidate uses less workload time than CPython in 5 of 23 standard timed fixtures and less peak process memory in 0. The largest remaining time ratios are below. These gaps remain part of the measurement set, including workloads that use pure-Python standard-library implementations where CPython has native implementations.

| Fixture | New/CPython JIT time | New/CPython JIT RSS |
| --- | ---: | ---: |
| `pickle_bench` | 336.017 | 2.639 |
| `deltablue` | 19.802 | 2.230 |
| `deque_ops` | 17.675 | 2.106 |
| `datetime_ops` | 17.568 | 2.225 |
| `list_ops` | 14.418 | 2.046 |

The executable contains 44,100,496 bytes, compared with 44,184,864 at the checkpoint. Build latency was not measured under controlled conditions. The memory figures are process peak RSS, not isolated object sizes or allocation counts. These measurements cover this host and these workloads; they cannot establish superiority for every Python program.

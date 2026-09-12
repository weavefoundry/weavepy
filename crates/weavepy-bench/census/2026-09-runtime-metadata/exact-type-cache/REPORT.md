# Native calls and exact attribute-cache names

This candidate adds a small-function call path with bounded stack buffers, retains immediately consumed integers without pinning, shares immutable compiled metadata, rejects missing scalar/constant helper registrations, and compares exact attribute names on cache hits. The exact-name check fixes a reproduced bug where a negative lookup could hide a different existing attribute with the same hash and length. Only single-block numeric functions with no calls, helpers, object access, or polls use the new path. Argument lanes, defaults, recursion limits, tracing gates, and code guards still apply.

Measured binary: `target/release/weavepy-runtime-exact-type-cache`, SHA-256 `4b847eba0bfe9a61bc9eb3a361fc989d04cb64aadee676a2295a39893cadca37`, 44,158,480 bytes.

The baseline is checkpoint `9a69c4161ade6527a5f4457d7e7272075cb1f0d9`; the paired previous release is the completed scalar-result guard stage. Focused probes compare the frozen predecessor recorded with each measurement file. Full compatibility validation passed all 205 checks. The raw data retains every sample and regression. This does not establish superiority over CPython across all meaningful workloads or metrics.

## Standard suite

Ratios below one mean less time or memory. The suite retains all 24 existing fixtures and work parameters, with five alternating measured cycles. Workload time excludes startup; elapsed time, process CPU time, and peak RSS include it. Startup has no workload timer, leaving 23 workload-time comparisons; its fixture row uses process elapsed time.

| Metric | JIT/checkpoint | Interpreter/checkpoint | JIT/previous | Interpreter/previous | JIT/CPython | Interpreter/CPython |
|---|---:|---:|---:|---:|---:|---:|
| Workload time | 0.854 | 0.966 | 0.994 | 1.005 | 3.699 | 9.625 |
| Process elapsed time | 0.915 | 0.978 | 0.992 | 1.010 | 3.489 | 5.943 |
| Process CPU time | 0.914 | 0.978 | 0.992 | 1.010 | 3.578 | 6.150 |
| Peak RSS | 0.988 | 0.987 | 0.996 | 1.007 | 2.183 | 2.023 |

JIT workload-time wins against CPython: 6/23. Peak-RSS wins: 0/24.

| Fixture | JIT time/previous | Interpreter time/previous | JIT time/CPython | JIT RSS/CPython |
|---|---:|---:|---:|---:|
| fannkuch | 1.008 | 1.022 | 10.285 | 2.078 |
| nbody | 1.009 | 1.004 | 8.850 | 2.061 |
| fib | 1.024 | 1.002 | 3.059 | 2.091 |
| pidigits | 1.000 | 0.995 | 0.882 | 2.055 |
| pyaes | 1.005 | 1.008 | 0.668 | 2.076 |
| richards | 0.999 | 1.016 | 8.640 | 2.081 |
| sumvm | 1.001 | 1.022 | 0.058 | 2.081 |
| nested_loops | 1.005 | 0.974 | 0.093 | 2.095 |
| jitloop | 0.999 | 1.008 | 0.073 | 2.083 |
| jitkernels | 0.993 | 1.007 | 0.869 | 2.071 |
| deltablue | 1.018 | 1.011 | 19.694 | 2.247 |
| float_math | 1.015 | 1.008 | 8.091 | 3.062 |
| spectral_norm | 0.839 | 1.007 | 2.176 | 2.082 |
| json_bench | 0.988 | 0.991 | 1.134 | 2.749 |
| str_methods | 0.985 | 0.987 | 3.090 | 2.150 |
| dict_ops | 1.003 | 1.004 | 5.495 | 2.055 |
| list_ops | 1.006 | 1.006 | 13.826 | 2.060 |
| attr_access | 1.006 | 1.012 | 3.467 | 2.189 |
| call_overhead | 0.936 | 0.999 | 9.062 | 2.118 |
| generators | 0.998 | 0.998 | 9.628 | 2.082 |
| deque_ops | 1.029 | 1.027 | 17.121 | 2.119 |
| datetime_ops | 1.017 | 1.018 | 150.372 | 2.281 |
| pickle_bench | 1.002 | 1.002 | 334.876 | 2.664 |
| startup | 0.992 | 1.040 | 1.487 | 2.069 |

The illustrative `fannkuch` loop is not canonical fannkuch, and `pyaes` is an XOR scrambler rather than AES. CPython datetime timings varied substantially between earlier stages; all samples remain available, and no cause has been established.

## Focused calls and controls

These tables use nine cycles and ratios of sample medians against the frozen baseline identified in each section. Every included probe is shown. Workload time, process CPU time, and peak RSS are separate measurements.

### scalar-result-probes

Baseline: `target/release/weavepy-runtime-shared-artifacts`.

| Workload | JIT time/base | JIT CPU/base | JIT RSS/base | Interpreter time/base | JIT time/CPython |
|---|---:|---:|---:|---:|---:|
| integer_callback_sum | 0.991 | 1.012 | 1.003 | 1.004 | 2.446 |
| integer_callback_left | 0.997 | 1.005 | 1.004 | 1.006 | 2.383 |
| keyword_callback_sum | 0.961 | 0.962 | 1.000 | 0.984 | 13.799 |
| callable_instance_sum | 1.005 | 1.007 | 1.003 | 1.000 | 10.301 |

### long-scalar-probes

Baseline: `target/release/weavepy-runtime-shared-artifacts`.

| Workload | JIT time/base | JIT CPU/base | JIT RSS/base | Interpreter time/base | JIT time/CPython |
|---|---:|---:|---:|---:|---:|
| integer_callback_sum | 0.999 | 1.019 | 1.003 | 1.008 | 2.073 |
| integer_callback_left | 0.983 | 1.010 | 1.000 | 1.007 | 1.982 |
| keyword_callback_sum | 0.965 | 0.969 | 1.010 | 0.977 | 13.021 |
| callable_instance_sum | 1.008 | 1.005 | 1.001 | 1.007 | 9.556 |

### scalar-leaf-probes

Baseline: `target/release/weavepy-runtime-shared-artifacts`.

| Workload | JIT time/base | JIT CPU/base | JIT RSS/base | Interpreter time/base | JIT time/CPython |
|---|---:|---:|---:|---:|---:|
| scalar_pair | 0.987 | 0.999 | 1.003 | 1.000 | 1.969 |
| trailing_scalar_defaults | 0.989 | 1.007 | 1.001 | 1.008 | 1.760 |
| float_scalar_pair | 1.007 | 1.008 | 1.004 | 1.007 | 5.621 |
| keyword_gap_control | 0.999 | 0.997 | 1.004 | 1.004 | 14.877 |
| overflow_scalar_control | 1.001 | 1.005 | 1.002 | 0.999 | 12.922 |

### constant-probes

Baseline: `target/release/weavepy-runtime-shared-artifacts`.

| Workload | JIT time/base | JIT CPU/base | JIT RSS/base | Interpreter time/base | JIT time/CPython |
|---|---:|---:|---:|---:|---:|
| tuple_literal_lengths | 0.999 | 1.028 | 1.003 | 1.005 | 0.101 |
| tuple_parameter_lengths | 1.000 | 1.016 | 1.001 | 1.009 | 0.101 |
| tuple_constant_returns | 1.004 | 1.024 | 1.003 | 0.999 | 0.260 |
| string_constant_returns | 0.996 | 1.011 | 1.003 | 0.998 | 0.301 |
| short_string_activations | 0.997 | 1.022 | 1.001 | 0.992 | 4.270 |

### regression-repeat

Baseline: `target/release/weavepy-runtime-shared-artifacts`.

| Workload | JIT time/base | JIT CPU/base | JIT RSS/base | Interpreter time/base | JIT time/CPython |
|---|---:|---:|---:|---:|---:|
| call_overhead | 0.998 | 0.998 | 0.994 | 0.997 | 9.079 |
| nested_loops | 1.005 | 1.018 | 1.001 | 0.999 | 0.081 |
| pickle_bench | 1.006 | 1.006 | 1.000 | 1.002 | 337.754 |

### warm-call-repeat

Baseline: `target/release/weavepy-runtime-shared-artifacts`.

| Workload | JIT time/base | JIT CPU/base | JIT RSS/base | Interpreter time/base | JIT time/CPython |
|---|---:|---:|---:|---:|---:|
| call_overhead | 0.997 | 0.998 | 1.006 | 0.999 | 8.989 |

### type-cache-probes

Baseline: `target/release/weavepy-runtime-shared-artifacts`.

| Workload | JIT time/base | JIT CPU/base | JIT RSS/base | Interpreter time/base | JIT time/CPython |
|---|---:|---:|---:|---:|---:|
| cached_short_attribute | 1.003 | 1.004 | 1.003 | 1.008 | 9.024 |
| cached_missing_attribute | 1.002 | 1.001 | 1.003 | 0.999 | 32.440 |
| cached_long_attribute | 0.995 | 1.002 | 1.005 | 1.019 | 8.811 |
| many_class_namespaces | 1.028 | 1.023 | 1.005 | 1.019 | 8.014 |

## Startup

| Workload | Time/checkpoint | RSS/checkpoint | Time/CPython | RSS/CPython |
|---|---:|---:|---:|---:|
| startup | 1.023 | 0.997 | 1.423 | 1.937 |
| startup_no_site | 1.051 | 1.013 | 0.610 | 1.567 |
| imports | 0.946 | 0.941 | 2.945 | 2.736 |

## Probes

| Workload | Time/checkpoint | RSS/checkpoint | Time/CPython | RSS/CPython |
|---|---:|---:|---:|---:|
| plain_instances | 0.921 | 0.877 | 17.579 | 3.865 |
| slotted_instances | 0.843 | 0.684 | 19.312 | 3.508 |
| memoryviews | 0.929 | 0.736 | 4.163 | 1.539 |
| memoryview_access | 0.950 | 0.998 | 6.222 | 1.944 |
| materialized_frames | 0.969 | 0.909 | 7.951 | 2.113 |
| bytesio_streams | 0.903 | 0.846 | 10.379 | 2.995 |
| type_creation | 0.998 | 0.961 | 1.260 | 1.216 |
| float_repr | 0.469 | 0.997 | 1.026 | 1.698 |
| float_str | 0.455 | 0.998 | 1.051 | 1.704 |
| int_repr | 0.816 | 0.998 | 3.953 | 1.703 |
| complex_repr | 0.379 | 0.996 | 0.767 | 1.737 |
| json_float_array | 0.151 | 0.937 | 0.230 | 2.397 |
| json_int_array | 0.283 | 0.940 | 0.735 | 2.349 |
| json_repeated_keys | 0.710 | 0.950 | 1.044 | 2.470 |
| json_unique_keys | 0.866 | 0.887 | 1.181 | 2.434 |

## Parallel execution

CPython is the installed GIL build. WeavePy modes below refer to its GIL setting; this comparison is not a free-threaded CPython comparison. CPU time includes all participating threads.

| GIL setting | Parallel time/CPython | Parallel CPU/CPython | Peak RSS/CPython |
|---|---:|---:|---:|
| 1 | 0.118 | 0.118 | 3.867 |
| 0 | 0.638 | 4.439 | 3.607 |

Build latency was not measured under controlled conditions. No build-time improvement is claimed. Focused correctness and trace proofs, compatibility results, executable and source hashes, and measurement scripts accompany this report.

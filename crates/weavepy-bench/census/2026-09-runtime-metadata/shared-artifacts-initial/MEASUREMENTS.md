# Focused measurements

Nine alternating measured cycles follow a discarded cycle. Ratios below one mean less time or memory. These tables use ratios of sample medians; all raw samples are retained.

## constant-probes

Baseline: `target/release/weavepy-runtime-scalar-leaves`.

| Workload | JIT time/base | JIT CPU/base | JIT RSS/base | Interpreter time/base | JIT time/CPython |
|---|---:|---:|---:|---:|---:|
| tuple_literal_lengths | 1.010 | 1.021 | 1.005 | 1.004 | 0.102 |
| tuple_parameter_lengths | 0.985 | 1.011 | 1.002 | 0.998 | 0.099 |
| tuple_constant_returns | 0.998 | 1.004 | 1.003 | 0.993 | 0.264 |
| string_constant_returns | 0.993 | 1.011 | 1.003 | 0.980 | 0.296 |
| short_string_activations | 1.010 | 1.007 | 1.004 | 0.996 | 4.289 |

## long-scalar-probes

Baseline: `target/release/weavepy-runtime-scalar-leaves`.

| Workload | JIT time/base | JIT CPU/base | JIT RSS/base | Interpreter time/base | JIT time/CPython |
|---|---:|---:|---:|---:|---:|
| integer_callback_sum | 0.889 | 0.975 | 1.002 | 0.966 | 2.125 |
| integer_callback_left | 0.898 | 0.973 | 1.004 | 0.967 | 1.986 |
| keyword_callback_sum | 1.045 | 1.042 | 1.005 | 1.040 | 13.850 |
| callable_instance_sum | 1.046 | 1.031 | 1.005 | 0.986 | 9.549 |

## regression-repeat

Baseline: `target/release/weavepy-runtime-scalar-leaves`.

| Workload | JIT time/base | JIT CPU/base | JIT RSS/base | Interpreter time/base | JIT time/CPython |
|---|---:|---:|---:|---:|---:|
| call_overhead | 1.004 | 1.005 | 1.005 | 1.027 | 9.139 |
| nested_loops | 1.036 | 1.026 | 1.006 | 1.008 | 0.084 |
| pickle_bench | 1.009 | 1.008 | 1.005 | 1.000 | 334.769 |

## scalar-leaf-probes

Baseline: `target/release/weavepy-runtime-scalar-leaves`.

| Workload | JIT time/base | JIT CPU/base | JIT RSS/base | Interpreter time/base | JIT time/CPython |
|---|---:|---:|---:|---:|---:|
| scalar_pair | 0.889 | 0.935 | 1.004 | 0.966 | 2.015 |
| trailing_scalar_defaults | 0.888 | 0.927 | 1.001 | 0.972 | 1.754 |
| float_scalar_pair | 1.002 | 1.002 | 1.005 | 0.975 | 5.607 |
| keyword_gap_control | 1.016 | 1.014 | 1.003 | 1.020 | 14.764 |
| overflow_scalar_control | 0.986 | 0.986 | 1.006 | 0.985 | 12.549 |

## scalar-result-probes

Baseline: `target/release/weavepy-runtime-scalar-leaves`.

| Workload | JIT time/base | JIT CPU/base | JIT RSS/base | Interpreter time/base | JIT time/CPython |
|---|---:|---:|---:|---:|---:|
| integer_callback_sum | 0.895 | 0.932 | 1.007 | 0.969 | 2.502 |
| integer_callback_left | 0.909 | 0.943 | 1.002 | 0.971 | 2.391 |
| keyword_callback_sum | 1.037 | 1.038 | 1.002 | 1.042 | 14.309 |
| callable_instance_sum | 1.052 | 1.048 | 1.005 | 0.992 | 10.230 |

## warm-call-repeat

Baseline: `target/release/weavepy-runtime-scalar-leaves`.

| Workload | JIT time/base | JIT CPU/base | JIT RSS/base | Interpreter time/base | JIT time/CPython |
|---|---:|---:|---:|---:|---:|
| call_overhead | 1.012 | 1.009 | 1.002 | 1.013 | 9.061 |

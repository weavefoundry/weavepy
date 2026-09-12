# Focused measurements

Nine alternating measured cycles follow a discarded cycle. Ratios below one mean less time or memory. These tables use ratios of sample medians; all raw samples are retained.

## constant-probes

Baseline: `target/release/weavepy-runtime-unboxed-inline`.

| Workload | JIT time/base | JIT CPU/base | JIT RSS/base | Interpreter time/base | JIT time/CPython |
|---|---:|---:|---:|---:|---:|
| tuple_literal_lengths | 1.011 | 1.008 | 0.997 | 1.007 | 0.102 |
| tuple_parameter_lengths | 1.002 | 1.012 | 0.994 | 0.996 | 0.101 |
| tuple_constant_returns | 0.862 | 1.016 | 0.998 | 0.990 | 0.262 |
| string_constant_returns | 1.145 | 1.024 | 1.000 | 1.001 | 0.303 |
| short_string_activations | 1.017 | 1.003 | 0.997 | 0.994 | 4.256 |

## long-scalar-probes

Baseline: `target/release/weavepy-runtime-unboxed-inline`.

| Workload | JIT time/base | JIT CPU/base | JIT RSS/base | Interpreter time/base | JIT time/CPython |
|---|---:|---:|---:|---:|---:|
| integer_callback_sum | 0.467 | 0.726 | 1.000 | 0.996 | 2.383 |
| integer_callback_left | 0.489 | 0.739 | 1.000 | 0.999 | 2.244 |
| keyword_callback_sum | 1.012 | 1.007 | 1.001 | 0.991 | 13.196 |
| callable_instance_sum | 0.990 | 0.991 | 0.999 | 0.998 | 9.145 |

## regression-repeat

Baseline: `target/release/weavepy-runtime-unboxed-inline`.

| Workload | JIT time/base | JIT CPU/base | JIT RSS/base | Interpreter time/base | JIT time/CPython |
|---|---:|---:|---:|---:|---:|
| call_overhead | 0.942 | 0.947 | 0.995 | 0.983 | 9.012 |
| nested_loops | 1.166 | 1.008 | 0.994 | 0.973 | 0.095 |
| pickle_bench | 1.001 | 1.000 | 1.000 | 0.999 | 336.835 |

## scalar-leaf-probes

Baseline: `target/release/weavepy-runtime-unboxed-inline`.

| Workload | JIT time/base | JIT CPU/base | JIT RSS/base | Interpreter time/base | JIT time/CPython |
|---|---:|---:|---:|---:|---:|
| scalar_pair | 0.473 | 0.574 | 0.999 | 1.001 | 2.352 |
| trailing_scalar_defaults | 0.497 | 0.588 | 1.001 | 1.007 | 2.045 |
| float_scalar_pair | 1.005 | 1.006 | 0.998 | 0.998 | 5.604 |
| keyword_gap_control | 0.998 | 0.996 | 0.997 | 1.002 | 14.577 |
| overflow_scalar_control | 1.009 | 1.010 | 0.995 | 0.997 | 12.270 |

## scalar-result-probes

Baseline: `target/release/weavepy-runtime-unboxed-inline`.

| Workload | JIT time/base | JIT CPU/base | JIT RSS/base | Interpreter time/base | JIT time/CPython |
|---|---:|---:|---:|---:|---:|
| integer_callback_sum | 0.468 | 0.574 | 0.995 | 0.999 | 2.883 |
| integer_callback_left | 0.493 | 0.590 | 1.000 | 0.996 | 2.575 |
| keyword_callback_sum | 0.998 | 0.997 | 1.002 | 1.000 | 13.774 |
| callable_instance_sum | 0.991 | 0.993 | 0.998 | 1.000 | 9.800 |

## warm-call-repeat

Baseline: `target/release/weavepy-runtime-unboxed-inline`.

| Workload | JIT time/base | JIT CPU/base | JIT RSS/base | Interpreter time/base | JIT time/CPython |
|---|---:|---:|---:|---:|---:|
| call_overhead | 0.945 | 0.942 | 1.001 | 0.992 | 8.932 |

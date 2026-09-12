# Unboxed-call helper inlining

The only runtime change from the initial unboxed-call release is `inline(always)` on `finish_dyn_native_result`. The measured binary is `weavepy-runtime-unboxed-inline`, SHA-256 `88636884dd06e9ba987964ebd5a8f9cb685957bf23c73bc0f30af943a4db1d8d`, 44,156,592 bytes, 16 bytes smaller than the initial release. Its helper has no separate symbol in the release binary.

Focused correctness checks pass for constants, scalar results, completed calls, long native loops, enumeration, and list builders/lifetimes. This annotation-only candidate has no separate full compatibility run or full 24-fixture census. The initial release has the full 199-check validation. Exact measured sources and scripts are in `inputs/`; subsequent scalar-leaf work is excluded.

All measurements use nine alternating cycles, with no concurrent build, test, or profiler. Ratios below use medians; lower is better. The baseline is the initial unboxed-call release (`f5fb1dd1`). All samples, elapsed time, CPU time, and RSS are retained in the JSON. CPython reference metadata is inherited from the same host environment.

## Short callbacks

| Workload | JIT time/base | JIT RSS/base | Interpreted time/base | JIT time/CPython | JIT time/small slots |
|---|---:|---:|---:|---:|---:|
| integer_callback_sum | 0.898 | 1.009 | 0.988 | 5.925 | - |
| integer_callback_left | 0.930 | 1.014 | 0.992 | 5.035 | - |
| keyword_callback_sum | 0.990 | 1.010 | 0.997 | 13.060 | - |
| callable_instance_sum | 1.024 | 1.008 | 1.001 | 9.630 | - |

## Long callbacks

| Workload | JIT time/base | JIT RSS/base | Interpreted time/base | JIT time/CPython | JIT time/small slots |
|---|---:|---:|---:|---:|---:|
| integer_callback_sum | 0.907 | 1.011 | 1.002 | 5.277 | - |
| integer_callback_left | 0.952 | 1.009 | 0.998 | 4.610 | - |
| keyword_callback_sum | 1.000 | 1.008 | 1.011 | 13.563 | - |
| callable_instance_sum | 1.005 | 1.007 | 0.996 | 9.454 | - |

## Constant controls

| Workload | JIT time/base | JIT RSS/base | Interpreted time/base | JIT time/CPython | JIT time/small slots |
|---|---:|---:|---:|---:|---:|
| tuple_literal_lengths | 0.994 | 1.005 | 1.008 | 0.090 | - |
| tuple_parameter_lengths | 0.983 | 1.008 | 0.996 | 0.093 | - |
| tuple_constant_returns | 1.008 | 1.008 | 1.009 | 0.302 | - |
| string_constant_returns | 0.999 | 1.009 | 0.989 | 0.263 | - |
| short_string_activations | 1.011 | 1.008 | 1.011 | 4.180 | - |

## Standard regression repeat

| Workload | JIT time/base | JIT RSS/base | Interpreted time/base | JIT time/CPython | JIT time/small slots |
|---|---:|---:|---:|---:|---:|
| call_overhead | 0.998 | 1.008 | 1.002 | 9.442 | 1.160 |
| nested_loops | 1.004 | 1.006 | 1.012 | 0.074 | 1.004 |
| pickle_bench | 0.942 | 1.005 | 1.010 | 335.173 | 0.958 |

## Warm call matrix

| Workload | JIT time/base | JIT RSS/base | Interpreted time/base | JIT time/CPython | JIT time/small slots |
|---|---:|---:|---:|---:|---:|
| call_overhead | 0.991 | 1.007 | 0.977 | 9.235 | 1.130 |

The direct integer callback improves by about 9–10%, and the unfused left-operand callback improves by 5–7%. The broader call matrix is essentially unchanged against the initial release and remains slower than the small-slot release. Peak RSS increases by roughly 1% across the focused set. These results do not establish universal speed or memory superiority over CPython.

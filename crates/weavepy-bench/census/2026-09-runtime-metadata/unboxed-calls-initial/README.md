# Initial unboxed-call candidate

This release fuses immediately consumed integer returns into generic calls.
All 199 compatibility checks, 46 JIT tests, 283 VM tests, Clippy, workspace
checks, and compilation without the JIT pass. Three direct long-call probes
complete 102,400 iterations without pin-limit exits. The unfused left-hand
result case still exits once. Wrong types and changed guards preserve the
completed return and resume the interpreter. The native return-normalization
helper remains out of line in this build.

The executable is `target/release/weavepy-runtime-unboxed-calls`, identified
by environment.json. It contains 44,156,608 bytes, 17,008 more than the preceding
scalar-guard release. The frozen input sources, full validation, direct traces,
and all focused measurements are retained here. This stage has no separate
complete standard-suite census. Inlining the return helper is the next
experiment; these files retain the initial regressions.

Each focused workload retains nine alternating cycles after a discarded
cycle. Ratios below are candidate/reference, with smaller values better.
Peak RSS covers the whole process. The previous reference in the first
three tables is the scalar-guard release.

## Short callbacks

| Workload | Time/previous | CPU/previous | RSS/previous | Time/CPython | RSS/CPython |
| --- | ---: | ---: | ---: | ---: | ---: |
| `integer_callback_sum` | 1.034 | 1.034 | 0.992 | 6.698 | 2.045 |
| `integer_callback_left` | 1.102 | 1.102 | 0.993 | 5.743 | 2.045 |
| `keyword_callback_sum` | 0.992 | 0.990 | 0.991 | 13.886 | 2.039 |
| `callable_instance_sum` | 0.968 | 0.968 | 0.992 | 9.846 | 2.037 |

## Long callbacks

| Workload | Time/previous | CPU/previous | RSS/previous | Time/CPython | RSS/CPython |
| --- | ---: | ---: | ---: | ---: | ---: |
| `integer_callback_sum` | 1.050 | 1.050 | 0.922 | 5.825 | 2.044 |
| `integer_callback_left` | 1.071 | 1.071 | 0.994 | 4.914 | 2.199 |
| `keyword_callback_sum` | 0.991 | 0.991 | 0.923 | 12.959 | 2.035 |
| `callable_instance_sum` | 0.999 | 0.999 | 0.923 | 9.037 | 2.033 |

## Constant controls

| Workload | Time/previous | CPU/previous | RSS/previous | Time/CPython | RSS/CPython |
| --- | ---: | ---: | ---: | ---: | ---: |
| `tuple_literal_lengths` | 1.004 | 1.003 | 0.992 | 0.103 | 2.035 |
| `tuple_parameter_lengths` | 0.996 | 0.998 | 0.993 | 0.100 | 2.038 |
| `tuple_constant_returns` | 1.163 | 1.164 | 0.993 | 0.307 | 2.035 |
| `string_constant_returns` | 0.870 | 0.866 | 0.992 | 0.258 | 2.031 |
| `short_string_activations` | 1.003 | 1.003 | 0.992 | 4.206 | 2.035 |

## Original-work regression repeat

This comparison includes both preceding releases and CPython.

| Workload | Time/scalar guard | RSS/scalar guard | Time/small slots | RSS/small slots | Time/CPython | RSS/CPython |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `call_overhead` | 0.986 | 0.959 | 1.170 | 1.020 | 9.721 | 2.104 |
| `nested_loops` | 0.997 | 0.995 | 0.999 | 0.995 | 0.089 | 2.066 |
| `pickle_bench` | 1.002 | 0.995 | 0.999 | 0.997 | 350.988 | 2.639 |

## Warm call matrix

This comparison includes both preceding releases and CPython.

| Workload | Time/scalar guard | RSS/scalar guard | Time/small slots | RSS/small slots | Time/CPython | RSS/CPython |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `call_overhead` | 0.975 | 0.923 | 1.155 | 1.020 | 9.017 | 2.081 |

The startup memory maps are separate one-shot diagnostics with the JIT disabled. Their physical-footprint and allocator figures are not the benchmark peak-RSS metric. CPython uses its own small-object allocator, so malloc-zone counts do not represent directly comparable runtime object counts.

# Focused measurements

Nine alternating measured cycles follow a discarded cycle. Ratios below one mean less time or memory. These tables use ratios of sample medians; all raw samples are retained.

## warm-120

Baseline: `target/release/weavepy-runtime-unboxed-inline`.

| Workload | JIT time/base | JIT CPU/base | JIT RSS/base | Interpreter time/base | JIT time/CPython |
|---|---:|---:|---:|---:|---:|
| nested_loops | 1.219 | 1.018 | 0.999 | 0.973 | 0.080 |

## warm-240

Baseline: `target/release/weavepy-runtime-unboxed-inline`.

| Workload | JIT time/base | JIT CPU/base | JIT RSS/base | Interpreter time/base | JIT time/CPython |
|---|---:|---:|---:|---:|---:|
| nested_loops | 1.014 | 1.007 | 1.000 | 0.974 | 0.040 |

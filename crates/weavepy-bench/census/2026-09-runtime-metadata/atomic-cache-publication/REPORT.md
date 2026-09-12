# Publish lazy instruction caches through one atomic pointer

The cache table stores an owning atomic pointer and its logical length. Acquire loads both check initialization and obtain the published array. First writes publish fully initialized storage with release semantics; losing concurrent allocations are released. Resizing and destruction require exclusive access and reconstruct the original slice allocation.

Executable: `target/release/weavepy-runtime-atomic-caches`, SHA-256 `7782b8b763cd4eb2c0eec4344278b6df4c3f67e3f2ef561a69322cfcc2dbb779`, 44,175,840 bytes.

The measured CodeObject header is 480 bytes, down from 496 in the initial lazy-cache release. InlineCache remains 16 bytes. Isolated optimized assembly removes one load from the read path; workload measurements below determine the practical effect.

All 217 compatibility checks, 31 compiler tests, 49 JIT tests, 289 VM tests, Clippy and workspace/feature checks pass. Debug and release execution checks cover lazy allocation, indexed slots, and the trailing-default repair. Nine standalone ownership/publication tests also pass with Rust 1.93 and Miri strict provenance under default and Tree Borrows on ARM64 and i686.

The table publication change does not make individual CacheSlot accesses thread-safe. A separate isolated Miri diagnostic reproduces the existing shared-slot race without the GIL. The worker code-sharing audit and remaining repair options are preserved with these results. Concurrent initialization tests mutate disjoint slots, while shared-slot safety is a separate requirement.

## Code and class controls

Nine paired cycles compare against the trailing-default correctness release, which otherwise retains the initial lazy-cache implementation. Ratios below one mean less time or memory. CPU time and peak RSS cover the whole process.

| Workload | Time/previous | CPU/previous | RSS/previous | Time/CPython | RSS/CPython |
|---|---:|---:|---:|---:|---:|
| retained_cold_code | 1.001 | 0.993 | 1.002 | 0.962 | 1.939 |
| retained_warm_code | 1.000 | 0.991 | 1.002 | 0.998 | 2.092 |
| code_compile_churn | 1.003 | 0.998 | 1.000 | 0.992 | 2.134 |
| class_version_churn | 0.986 | 0.987 | 1.000 | 6.198 | 1.858 |
| type_creation | 1.028 | 1.016 | 0.998 | 1.287 | 1.181 |

## Cold regression controls

Each row retains nine paired cycles with compact caches, the subsequent default-suffix release, this candidate, and CPython. Warm controls execute the same workload once before timing it.

| Fixture | JIT/compact | Interpreter/compact | JIT/previous | Interpreter/previous | JIT RSS/previous | Interpreter RSS/previous | JIT/CPython |
|---|---:|---:|---:|---:|---:|---:|---:|
| str_methods | 1.037 | 1.038 | 1.037 | 1.033 | 0.996 | 1.001 | 3.251 |
| list_ops | 0.999 | 1.004 | 0.965 | 0.997 | 1.002 | 0.999 | 13.639 |
| generators | 0.997 | 1.016 | 1.017 | 0.914 | 0.998 | 1.000 | 9.608 |
| call_overhead | 0.994 | 0.997 | 0.996 | 0.995 | 1.001 | 1.000 | 8.972 |

## Warm regression controls

Each row retains nine paired cycles with compact caches, the subsequent default-suffix release, this candidate, and CPython. Warm controls execute the same workload once before timing it.

| Fixture | JIT/compact | Interpreter/compact | JIT/previous | Interpreter/previous | JIT RSS/previous | Interpreter RSS/previous | JIT/CPython |
|---|---:|---:|---:|---:|---:|---:|---:|
| str_methods | 1.031 | 1.020 | 1.026 | 1.020 | 0.999 | 1.001 | 2.863 |
| list_ops | 1.005 | 1.000 | 1.001 | 0.997 | 0.999 | 1.002 | 13.841 |
| generators | 1.005 | 1.010 | 1.019 | 0.917 | 1.000 | 1.003 | 9.575 |
| call_overhead | 1.009 | 1.004 | 1.004 | 0.992 | 0.997 | 1.001 | 9.116 |

These focused results do not establish superiority over CPython across all workloads or metrics. All samples, failures, and execution proofs are retained. Build latency was not measured under controlled conditions.

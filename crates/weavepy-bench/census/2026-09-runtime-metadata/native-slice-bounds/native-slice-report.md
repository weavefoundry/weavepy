# Preserve native slice bounds and fallback operands

Native list and string slices now distinguish an omitted stop from an explicit -2**63 stop. The helper receives zero for that explicit bound, which clamps to the beginning for every valid container length. Fallback retains the original bound. Native fallback also restores the exact operand count of the slice producer: constant, two-bound, or three-bound.

Both defects were reproduced in the preceding compact-GC and lazy-dictionary releases. CPython and the interpreter produced the expected results. Unicode fallback previously raised TypeError after ASCII warmup for valid two-bound slice bytecode. The regression preserves byte offsets when constructing that bytecode and now verifies the correct Unicode result.

Measured executable: `target/release/weavepy-runtime-native-slice-fix`, SHA-256 `9f0745479837e48c31b46957bce04a23cb3ba0108c36f4758743013287e7e092`, 44,193,984 bytes. The paired preceding executable is the compact-GC release. All 229 compatibility checks, 302 VM tests, and 52 JIT tests pass, along with Clippy, workspace/all-feature/no-JIT checks, and debug/release native execution proofs.

The native proof requires all seven slice functions to compile, counts 8,386 framed entries, 1,991 direct calls, and 153 native-to-native calls, and confirms the intended two-bound Unicode deoptimization. Bounds cover minimum/maximum integers, omitted endpoints, big integers, changing list lanes, ASCII, Unicode, surrogates, and index callbacks executing once.

No helper ABI, object layout, or unsafe code changes are introduced. The public Rust TOp slice fields replace the boolean konst field with SliceOrigin; IR consumers must select Constant, TwoBounds, or ThreeBounds. Matching release-library layouts remain unchanged, including 80-byte collector handles, 24-byte Object, 176-byte instance headers, and 480-byte CodeObject.

## Focused measurements

Cold list time and warm interpreted JIT-kernel time were repeated for 15 paired cycles after their initial nine-cycle runs showed slowdowns. Both runs are retained, including discrepancies.

Each comparison uses nine alternating paired process cycles after discarding one cycle. Ratios below one mean less time or memory. Slice controls use 200 calls of a 128-slice function, with an untimed workload run before timing. All eight controls produce the same result in CPython and both Weave releases, and their calculate functions compile in both releases. They avoid the incorrect minimum-stop case in the reference, so incorrect results cannot masquerade as a speed improvement.

Workload time excludes startup; process CPU time, elapsed time, and peak RSS include it. Warm runs also retain workload CPU time. These controls measure the cost of a correctness repair. The full 24-fixture census, startup, GC pauses, and parallel throughput were not repeated in this stage; their earlier results are not measurements of this executable.

### Native slices

| Workload | JIT time/base | Interpreter time/base | JIT CPU/base | JIT RSS/base | JIT time/CPython | JIT RSS/CPython |
|---|---:|---:|---:|---:|---:|---:|
| list_dynamic | 1.005 | 1.003 | 1.015 | 0.999 | 3.076 | 1.971 |
| list_open | 0.994 | 0.992 | 1.001 | 0.997 | 2.783 | 1.980 |
| list_constant | 1.015 | 0.997 | 1.011 | 0.996 | 3.863 | 1.969 |
| str_dynamic | 1.006 | 1.011 | 1.015 | 0.997 | 1.022 | 1.972 |
| str_open | 0.958 | 1.017 | 1.010 | 0.999 | 0.996 | 1.965 |
| str_constant | 1.004 | 1.008 | 1.004 | 0.998 | 1.537 | 1.972 |
| list_parameter | 1.009 | 0.996 | 1.012 | 0.998 | 3.785 | 1.979 |
| str_parameter | 1.004 | 1.004 | 1.019 | 1.001 | 1.509 | 1.969 |

### Cold standard controls

| Workload | JIT time/base | Interpreter time/base | JIT CPU/base | JIT RSS/base | JIT time/CPython | JIT RSS/CPython |
|---|---:|---:|---:|---:|---:|---:|
| str_methods | 0.999 | 1.000 | 0.998 | 1.002 | 3.124 | 2.072 |
| list_ops | 1.049 | 1.003 | 1.044 | 0.996 | 14.424 | 1.980 |
| attr_access | 1.003 | 1.008 | 1.006 | 0.997 | 3.044 | 2.118 |
| call_overhead | 0.997 | 0.998 | 0.999 | 0.995 | 8.953 | 2.037 |
| jitkernels | 0.997 | 1.013 | 1.006 | 0.998 | 0.875 | 1.990 |

### Warm standard controls

| Workload | JIT time/base | Interpreter time/base | JIT CPU/base | JIT RSS/base | JIT time/CPython | JIT RSS/CPython |
|---|---:|---:|---:|---:|---:|---:|
| str_methods | 0.981 | 1.004 | 0.979 | 1.000 | 2.795 | 3.832 |
| list_ops | 1.006 | 1.008 | 1.004 | 0.999 | 13.808 | 1.948 |
| attr_access | 1.008 | 1.008 | 1.004 | 1.002 | 2.973 | 2.085 |
| call_overhead | 0.992 | 1.011 | 0.994 | 0.997 | 8.893 | 2.004 |
| jitkernels | 1.002 | 1.033 | 1.009 | 0.996 | 0.837 | 1.952 |

### Cold list repeat

| Workload | JIT time/base | Interpreter time/base | JIT CPU/base | JIT RSS/base | JIT time/CPython | JIT RSS/CPython |
|---|---:|---:|---:|---:|---:|---:|
| list_ops | 1.003 | 1.002 | 1.004 | 0.997 | 13.813 | 1.976 |

### Warm kernel repeat

| Workload | JIT time/base | Interpreter time/base | JIT CPU/base | JIT RSS/base | JIT time/CPython | JIT RSS/CPython |
|---|---:|---:|---:|---:|---:|---:|
| jitkernels | 1.002 | 1.004 | 1.009 | 0.999 | 0.840 | 1.956 |

The initial cold list slowdown of 4.9 percent falls to 0.3 percent in the 15-cycle repeat; the initial warm interpreted JIT-kernel slowdown of 3.3 percent falls to 0.4 percent. Both original and repeat results remain visible. These repeats do not establish the cause of the first-run differences. Native slice timings range from 0.958 to 1.015 times the preceding release, with peak RSS near unchanged.

All samples, including regressions and variability, are retained. These results do not establish superiority to CPython across all meaningful workloads or metrics. The preceding full census and its CPython datetime variation remain documented in the compact-GC archive.

## Reproduction and evidence

The native-slice-bounds census archive contains 110 frozen source files, 37 measurement inputs, exact script and binary hashes, matching release-library layout evidence, complete compatibility results, native traces, and raw process measurements. Initial analyzer test setup and coverage-counter failures are preserved separately from the corrected passing checks. No runtime fix was changed merely to satisfy those test setup failures.

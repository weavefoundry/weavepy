# Native pin retirement and string-list entry

**This measured intermediate improves string execution but regresses cold peak memory. The CPython-wide objective remains unachieved.**

Exact string elements now pass the ListObj loop-entry guard, matching the existing access helper. After a frameless native side exit rebuilds its complete interpreter frame, obsolete runtime pins are released before the continuation runs. Finalizable objects use the existing pending-drop queue so callbacks run with the continuation frame and recursion depth installed. Entry pins retain their existing behavior. No new unsafe code, helper ABI, or public Rust API change is introduced.

Candidate SHA-256: `568a44a4f4af0138a357a4457b782edf7630282a9cdd8c8b60f0d8c7f7f99bd0`; 44,194,128 bytes. The preceding GC-index release is `8e86b528cdc3dd1f5c5db14e8a76fc111eba277dd2cabbfba76de28eb3874cd9`, the same size. All 12 measured release-library layouts remain unchanged.

All 232 compatibility checks, 305 VM tests, and 52 JIT tests pass, as do Clippy and workspace/all-feature/no-JIT checks. The focused regression covers retained lists, detached-node finalizers, weak references, escaped values, callback frame identity, and a global mutated by callbacks before a second loop. The native-entry and retirement tests failed before their respective fixes; those logs remain. A first finalizer probe did not exercise the required native exit and is retained as diagnostic history, not coverage proof.

Nine alternating paired cycles follow a discarded cycle. Ratios below are candidate/preceding; values below one are lower. CPU and RSS cover the full process. Workload time excludes startup. Warm work-CPU ratios are additionally preserved in the JSON. All raw samples and CPython comparisons remain in `focused/`.

| Mode | Fixture | JIT time | JIT wall | JIT CPU | JIT RSS | Interp time | Interp CPU | Interp RSS | CP time | CP RSS |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| cold | str_methods | 0.7372 | 0.7973 | 0.7928 | 1.8588 | 1.0042 | 0.9945 | 0.9983 | 2.2961 | 3.8751 |
| cold | list_ops | 1.0029 | 1.0028 | 1.0031 | 1.0054 | 0.9841 | 0.9782 | 0.9959 | 14.2031 | 1.9818 |
| cold | attr_access | 1.0018 | 1.0034 | 1.0033 | 1.0010 | 0.9808 | 0.9836 | 0.9977 | 3.0523 | 2.1235 |
| cold | call_overhead | 1.0013 | 1.0009 | 1.0030 | 1.0005 | 0.9850 | 0.9852 | 1.0006 | 8.9860 | 2.0407 |
| cold | jitkernels | 1.0002 | 1.0011 | 1.0021 | 1.0011 | 0.9911 | 0.9908 | 1.0012 | 0.8815 | 1.9926 |
| warm | str_methods | 0.7765 | 0.7904 | 0.7884 | 1.0011 | 1.0013 | 0.9985 | 1.0040 | 2.1560 | 3.8485 |
| warm | list_ops | 1.0244 | 1.0185 | 1.0201 | 1.0027 | 1.0262 | 1.0277 | 1.0023 | 13.9016 | 1.9541 |
| warm | attr_access | 0.9994 | 0.9999 | 1.0007 | 1.0010 | 0.9975 | 0.9982 | 0.9971 | 2.9508 | 2.0885 |
| warm | call_overhead | 0.9902 | 0.9950 | 0.9948 | 0.9995 | 0.9811 | 0.9834 | 0.9983 | 8.9244 | 2.0104 |
| warm | jitkernels | 0.9993 | 0.9954 | 0.9980 | 1.0000 | 0.9933 | 0.9947 | 1.0011 | 0.8440 | 1.9565 |

String workload time falls about 26 percent cold and 22 percent warm, but cold RSS rises about 86 percent. Warm string RSS remains approximately unchanged at almost four times CPython. Warm list time rises about 2.4 percent in JIT mode and 2.6 percent interpreted; the original observations remain. This intermediate is not an across-metric performance improvement.

A GC-disabled probe of repeated 50,000-iteration string-list loops found no accumulating lists after return in either release or CPython. A separate chain diagnostic without an explicit caller root exposed an existing argument-retention difference: CPython observed four callbacks inside the continuation, the preceding native path zero, and this candidate three. The regression explicitly retains the caller root to test obsolete runtime pins; this stage does not claim to repair entry-argument lifetime.

This stage does not repeat the full 24-fixture census, startup, parallel, or GC-pause measurements. The preceding archive remains the latest complete census. No build-latency or energy claim is made. Frozen inputs contain 111 sources and 37 measurement inputs.

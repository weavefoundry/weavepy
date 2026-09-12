# Split-list tracking and activation cleanup

This stage repairs collector and lifetime defects. Its measurements below retain both improvements and regressions. It does not establish superiority over CPython across all workloads or metrics.

String split, rsplit, and splitlines results now register with the collector, including empty and surrogate-containing results. Their mutable self-cycles are visible and collectable. Native activations retire entry and runtime pins after reconstructing every live value; frameless continuations and materialized generators defer callbacks until a safe interpreter point. Healthy native generator suspensions keep their complete pin table. Escaped frames use the existing prompt-reap cascade when their last alias disappears, preserving live frame locals and releasing dead ones. No new unsafe code, public Rust API, or helper ABI is introduced.

Candidate SHA-256: `ad126463aaa0fb7e79a0fa6b3ed3e0aa9974ab9cb0747f6bd54564e2b3401ae8`; 44,211,008 bytes. The frozen inputs contain 115 sources and 37 measurement inputs. Matching release layouts are recorded separately.

All 311 VM tests, 52 JIT tests, 244 compatibility checks, Clippy, and workspace/all-feature/no-JIT checks pass. Native counters establish parked resumes and materialization. Release diagnostics match CPython in native, interpreted, and GIL-disabled modes for split tracking, cycle collection, completed-call cleanup, generator observation/close/throw/drop/exhaustion, and escaped-frame weakref/finalizer behavior.

The original failures are preserved. Tracking alone revealed 23 to 24 retained entry lists per completed native call, where CPython and interpreted execution retained zero. A parked-generator probe retained 40 lists after observation, 39 after throw, and one after exhaustion. Entry and parked cleanup removed those native roots but left one list after an observed frame was deleted, also present in interpreted execution. A separate escaped-frame weakref assertion failed before the frame cascade change. The final release must pass the original probes, not only the later permanent regressions.

The first full VM run had two failures while heap-snapshot tests ran concurrently; each passed in isolation. The collector is process-wide, so gc.get_objects snapshots from one fixture can keep another fixture's objects alive. The four heap-lifetime tests now share a test-only mutex. Their assertions are unchanged. The initial failure log and isolated outcomes are retained.

Nine alternating paired cycles cover five cold and five warm fixtures. Five paired cycles cover string workloads from 1,000 to 60,000 iterations. Each follows a discarded cycle. Ratios below one mean less time or memory. Workload time excludes startup; CPU and peak RSS cover the process. GC-index is the 8e86 release; previous is the d804 callback repair. All interpreted samples and raw measurements are retained in focused/.

| Mode | Fixture | Time/GC-index | CPU/GC-index | RSS/GC-index | Time/previous | CPU/previous | RSS/previous | Time/CPython | RSS/CPython |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|
| cold-1000 | str_methods | 0.7981 | 0.9375 | 1.0536 | 0.9905 | 0.9967 | 0.9990 | 3.4314 | 2.2002 |
| cold-15000 | str_methods | 0.6599 | 0.7277 | 1.0510 | 0.9689 | 0.9817 | 0.9966 | 2.0867 | 2.1931 |
| cold-4000 | str_methods | 0.7076 | 0.8507 | 1.0526 | 0.9928 | 1.0107 | 0.9995 | 2.4326 | 2.1957 |
| cold-60000 | str_methods | 0.6572 | 0.6770 | 1.0548 | 0.9691 | 0.9701 | 0.9971 | 2.0207 | 2.2030 |
| cold | str_methods | 0.6742 | 0.7375 | 1.0538 | 0.9689 | 0.9824 | 0.9966 | 2.0842 | 2.1953 |
| cold | list_ops | 1.0420 | 1.0394 | 1.0005 | 1.0567 | 1.0541 | 0.9914 | 14.5604 | 1.9838 |
| cold | attr_access | 1.0180 | 1.0166 | 0.9626 | 1.0168 | 1.0101 | 0.9974 | 3.1278 | 2.0465 |
| cold | call_overhead | 1.0119 | 1.0161 | 1.0000 | 1.0183 | 1.0200 | 0.9922 | 9.0858 | 2.0430 |
| cold | jitkernels | 1.0127 | 1.0149 | 1.0027 | 1.0125 | 1.0082 | 0.9963 | 0.8880 | 1.9926 |
| warm-1000 | str_methods | 0.8934 | 0.9366 | 0.9155 | 0.9627 | 0.9826 | 0.9995 | 2.0290 | 2.1620 |
| warm-15000 | str_methods | 0.7195 | 0.7347 | 0.5629 | 0.9630 | 0.9714 | 0.9962 | 2.0077 | 2.1573 |
| warm-4000 | str_methods | 0.8606 | 0.8494 | 0.6417 | 0.9635 | 0.9855 | 0.9966 | 2.0120 | 2.1667 |
| warm-60000 | str_methods | 0.6473 | 0.6565 | 0.5666 | 0.9563 | 0.9721 | 0.9986 | 2.0044 | 2.1688 |
| warm | str_methods | 0.7191 | 0.7295 | 0.5662 | 0.9651 | 0.9720 | 1.0024 | 2.0281 | 2.1701 |
| warm | list_ops | 0.9958 | 0.9945 | 1.0032 | 1.0087 | 1.0090 | 0.9957 | 14.1396 | 1.9541 |
| warm | attr_access | 1.0333 | 1.0285 | 0.9640 | 1.0194 | 1.0166 | 0.9990 | 3.0822 | 2.0052 |
| warm | call_overhead | 1.0116 | 1.0117 | 1.0000 | 1.0142 | 1.0102 | 0.9964 | 9.0317 | 2.0199 |
| warm | jitkernels | 1.0169 | 1.0113 | 1.0048 | 1.0106 | 1.0090 | 0.9979 | 0.8458 | 1.9618 |

Main string workload time falls 3.1 percent cold and 3.5 percent warm relative to the preceding release, with approximately unchanged peak RSS. Relative to GC-index, string time falls 32.6 percent cold and 28.1 percent warm; cold RSS is still 5.4 percent higher and warm RSS is 43.4 percent lower. Cold list time regresses 5.7 percent and warm list time 0.9 percent against the preceding release. Attribute time regresses 1.7 to 1.9 percent, calls 1.4 to 1.8 percent, and numeric kernels 1.1 to 1.2 percent. All of these results remain in the table. Strings still use about twice CPython workload time and 2.2 times its peak RSS.

A separate untimed check compares each of the five benchmark functions on both its first and second invocation. Native, interpreted, and GIL-disabled results match CPython exactly. This check validates returned results and is not included in timing samples.

The previous release did not track split results, so its negative gc.get_objects sentinel probes were insensitive to their retention. Its apparently faster allocation path also omitted required collector work. Comparisons retain that historical behavior rather than silently replacing the reference binary.

The full 24-fixture census, startup/imports, parallel throughput, and explicit GC-pause measurements are not repeated here. GC-index remains the latest complete census. Missing replace-argument validation remains a known separate defect. No controlled build-latency, energy, or free-threaded CPython claim is made.

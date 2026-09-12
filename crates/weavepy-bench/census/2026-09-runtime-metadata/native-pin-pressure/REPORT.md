# Native temporary-pin pressure

**The CPython-wide objective remains unachieved. String memory improves substantially over the intermediate retirement release, but cold RSS remains above the GC-index release and attribute execution regresses slightly.**

At existing loop polls, 4,096 temporary pin handles request a complete interpreter reconstruction. Cleanup happens after reconstruction, so native registers never reference freed pins. Resource exits have separate accounting and do not exhaust the speculative deoptimization budget or inflate the generic-call denominator. Entry roots retain their existing behavior. The 65,536 hard cap and 1,024-iteration poll stride are unchanged. This is a handle limit with possible overshoot between polls, not a byte limit.

Candidate SHA-256: `eb52a06d77312d2339276f921bb36856e26d5f15012500878ee5917f7e94c420`; 44,194,256 bytes, 128 bytes above the two references. All 12 measured release-library layouts remain unchanged. No helper ABI, public Rust API, or new unsafe code is introduced.

All 235 compatibility checks, 306 VM tests, and 52 JIT tests pass, together with Clippy, workspace/all-feature/no-JIT checks, and focused native proofs. Debug traces establish resource reconstruction in ordinary string, split/join, nested, generator, and retained-pair loops. The formatting callback checks pass, but that function did not show a resource-rebuild trace. A separate stress test verifies at least 80 resource exits followed by native calls. Two initial test-setup failures remain in the archive; neither required a runtime implementation change.

The main controls use nine paired cycles, length scaling uses five, and attribute/numeric repeats use 15, each following a discarded cycle. Ratios below are candidate/reference medians of paired ratios; values below one are lower. GC refers to the 8e86 GC-index release and retirement to 568a. CPU and RSS cover the process, while workload time excludes startup. Every raw sample and interpreted comparison is preserved in focused/.

| Mode | Fixture | Time/GC | CPU/GC | RSS/GC | Time/retirement | RSS/retirement | Time/CPython | RSS/CPython |
|---|---|---:|---:|---:|---:|---:|---:|---:|
| cold-1000 | str_methods | 0.8029 | 0.9536 | 1.1426 | 1.0004 | 0.9946 | 3.5567 | 2.3903 |
| cold-15000 | str_methods | 0.6875 | 0.7589 | 1.1584 | 0.9454 | 0.6204 | 2.1692 | 2.4179 |
| cold-4000 | str_methods | 0.7222 | 0.8556 | 1.1541 | 0.9124 | 0.7141 | 2.4296 | 2.4022 |
| cold-60000 | str_methods | 0.6888 | 0.7085 | 1.1584 | 0.9407 | 0.6214 | 2.1284 | 2.4241 |
| cold | str_methods | 0.6946 | 0.7572 | 1.1592 | 0.9354 | 0.6223 | 2.1634 | 2.4136 |
| cold | list_ops | 0.9918 | 0.9917 | 1.0016 | 0.9863 | 0.9984 | 13.8153 | 1.9828 |
| cold | attr_access | 1.0168 | 1.0156 | 0.9565 | 1.0197 | 0.9546 | 3.0833 | 2.0300 |
| cold | call_overhead | 1.0020 | 1.0027 | 0.9974 | 0.9982 | 0.9958 | 8.9235 | 2.0408 |
| cold | jitkernels | 1.0074 | 1.0067 | 0.9973 | 1.0009 | 0.9920 | 0.8875 | 1.9872 |
| repeat-cold | attr_access | 1.0151 | 1.0176 | 0.9591 | 1.0211 | 0.9581 | 3.0736 | 2.0366 |
| repeat-cold | jitkernels | 0.9982 | 1.0015 | 0.9973 | 1.0011 | 0.9936 | 0.8810 | 1.9850 |
| repeat-warm | attr_access | 1.0176 | 1.0144 | 0.9586 | 1.0223 | 0.9585 | 3.0453 | 2.0042 |
| repeat-warm | jitkernels | 1.0056 | 1.0101 | 0.9979 | 1.0058 | 0.9968 | 0.8502 | 1.9495 |
| warm-1000 | str_methods | 0.9364 | 0.9422 | 0.9969 | 1.0060 | 0.9960 | 2.1396 | 2.3406 |
| warm-15000 | str_methods | 0.7478 | 0.7578 | 0.6143 | 0.9506 | 0.6139 | 2.0515 | 2.3640 |
| warm-4000 | str_methods | 0.8988 | 0.8724 | 0.7023 | 0.9281 | 0.6991 | 2.1232 | 2.3615 |
| warm-60000 | str_methods | 0.6746 | 0.6858 | 0.6164 | 0.9739 | 0.6131 | 1.8699 | 2.3682 |
| warm | str_methods | 0.7384 | 0.7499 | 0.6156 | 0.9570 | 0.6159 | 2.0627 | 2.3595 |
| warm | list_ops | 0.9913 | 0.9896 | 1.0021 | 0.9672 | 0.9952 | 13.8492 | 1.9500 |
| warm | attr_access | 1.0115 | 1.0137 | 0.9585 | 1.0169 | 0.9571 | 3.0263 | 1.9990 |
| warm | call_overhead | 1.0017 | 0.9970 | 0.9974 | 0.9987 | 0.9969 | 8.8513 | 2.0083 |
| warm | jitkernels | 1.0118 | 1.0104 | 0.9963 | 1.0119 | 0.9958 | 0.8490 | 1.9536 |

Against GC-index, main string workload time falls about 31 percent cold and 26 percent warm. Warm RSS falls about 38 percent. Cold RSS remains about 16 percent higher, though about 38 percent below retirement. Across 1,000 to 60,000 iterations, candidate peak RSS stays near 35 MiB. Strings still take about 2.1 times CPython workload time and 2.4 times its peak RSS in the main controls.

Attribute workload time rises about 1.7 percent cold and 1.1 percent warm in the initial run. Repeats retain 1.5 and 1.8 percent slowdowns, with roughly 4 percent lower RSS. Numeric repeats are approximately flat cold and 0.6 percent slower warm; the initial 0.7/1.2 percent slowdowns remain recorded. No causal claim is made for the varying numeric results.

The full 24-fixture census, startup/imports, parallel workloads, and explicit GC pauses are not repeated at this stage. GC-index remains the latest complete census. No controlled build-latency or energy claim is made. Frozen inputs contain 112 sources and 37 measurement inputs.

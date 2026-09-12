# Completed string-result pin budget

**This stage reduces string peak memory further, but it does not achieve the CPython-wide objective. Follow-up probes also expose preexisting native callback and replace-arity defects, preserved below.**

Exact string/list method results check the existing 4,096-temporary-handle limit at production. At the limit, the already-computed result is parked and the existing completed-call exit reconstructs the interpreter stack after CALL. The method is not repeated. The poll stride, hard cap, entry roots, and resource-exit accounting remain unchanged. Other pin producers still rely on loop polls and the hard cap; this is not a global byte bound.

Candidate SHA-256: `ffaec90e21e4ebfc68bbfa6e43bd0ade4c407e1dbd4ffdf12b3cce44576e095f`; 44,194,256 bytes, unchanged from the loop-poll pressure release. All 12 measured release-library layouts remain unchanged. No new unsafe code, public Rust API, or helper ABI is introduced.

All 235 existing compatibility checks, 307 VM tests, and 52 JIT tests pass, along with Clippy and workspace/all-feature/no-JIT checks. The new Rust expression test establishes resource exits with a live arithmetic result; its simple split arm exits at loop polls. A separate retained probe establishes completed string and list exits with two live arithmetic operands: candidate text/parts traces reconstruct at PCs 21/23 with stack length two, while the preceding release exits at loop PC 8. Both match CPython and the interpreter.

Nine alternating paired cycles cover five cold and five warm fixtures. Five paired cycles cover string workloads from 1,000 to 60,000 iterations. Each follows a discarded cycle. Ratios are medians of paired candidate/reference samples; below one is lower. Workload time excludes startup; CPU and RSS cover the process. GC is the 8e86 GC-index release; poll is eb52. Interpreted ratios, warm workload CPU, and every raw sample remain in focused/.

| Mode | Fixture | Time/GC | CPU/GC | RSS/GC | Time/poll | CPU/poll | RSS/poll | Time/CPython | RSS/CPython |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|
| cold-1000 | str_methods | 0.7931 | 0.9530 | 1.0517 | 0.9759 | 0.9859 | 0.9188 | 3.4163 | 2.1950 |
| cold-15000 | str_methods | 0.6866 | 0.7558 | 1.0558 | 1.0015 | 1.0052 | 0.9128 | 2.1484 | 2.1957 |
| cold-4000 | str_methods | 0.7206 | 0.8533 | 1.0542 | 0.9932 | 0.9979 | 0.9140 | 2.3931 | 2.2030 |
| cold-60000 | str_methods | 0.6791 | 0.6992 | 1.0559 | 1.0034 | 1.0012 | 0.9128 | 2.0699 | 2.2028 |
| cold | str_methods | 0.6926 | 0.7493 | 1.0559 | 0.9876 | 0.9918 | 0.9106 | 2.1538 | 2.2004 |
| cold | list_ops | 0.9871 | 0.9894 | 1.0060 | 1.0029 | 1.0018 | 1.0076 | 13.8117 | 1.9882 |
| cold | attr_access | 1.0121 | 1.0093 | 0.9626 | 0.9905 | 0.9925 | 1.0048 | 3.0408 | 2.0462 |
| cold | call_overhead | 0.9912 | 0.9928 | 1.0021 | 0.9970 | 0.9974 | 1.0053 | 8.8435 | 2.0538 |
| cold | jitkernels | 1.0094 | 1.0095 | 1.0075 | 1.0031 | 0.9926 | 1.0070 | 0.8705 | 2.0000 |
| warm-1000 | str_methods | 0.9408 | 0.9380 | 0.9136 | 1.0023 | 0.9938 | 0.9197 | 2.0588 | 2.1592 |
| warm-15000 | str_methods | 0.7335 | 0.7481 | 0.5645 | 1.0012 | 1.0016 | 0.9168 | 2.0475 | 2.1696 |
| warm-4000 | str_methods | 0.8971 | 0.8606 | 0.6397 | 0.9969 | 0.9960 | 0.9167 | 2.0349 | 2.1503 |
| warm-60000 | str_methods | 0.6926 | 0.6981 | 0.5623 | 1.0107 | 1.0054 | 0.9144 | 2.0512 | 2.1616 |
| warm | str_methods | 0.7433 | 0.7533 | 0.5620 | 1.0062 | 1.0063 | 0.9145 | 2.0579 | 2.1592 |
| warm | list_ops | 0.9830 | 0.9857 | 1.0043 | 0.9983 | 0.9971 | 1.0075 | 13.5821 | 1.9582 |
| warm | attr_access | 1.0138 | 1.0171 | 0.9670 | 0.9990 | 1.0006 | 1.0068 | 3.0108 | 2.0104 |
| warm | call_overhead | 1.0083 | 1.0056 | 1.0047 | 1.0041 | 1.0053 | 1.0052 | 8.8549 | 2.0156 |
| warm | jitkernels | 1.0019 | 1.0132 | 1.0042 | 1.0032 | 0.9958 | 1.0074 | 0.8419 | 1.9720 |

Main string peak RSS is about 9 percent below loop-poll pressure in both modes. Cold workload time falls about 1.2 percent and warm time rises about 0.6 percent. Against GC-index, cold RSS remains about 5.6 percent higher and warm RSS is about 44 percent lower. Main strings still take about 2.1 times CPython workload time and use about 2.2 times its RSS. Small regressions among the controls remain in the table; no across-metric improvement is claimed.

After those checks and measurements, a new __index__ probe shows that native str.replace can use a stale global after its count callback mutates it: the first result is 3,980,724 instead of 4,504,500. The GC-index, loop-poll pressure, and candidate releases all fail identically; CPython and interpreted execution agree. A second warmed-call probe records <module> as a callback caller where CPython and the interpreter report replace_once. The helper invokes a callback-capable builtin body without its required activation shell or post-call invalidation. These are preexisting defects outside the 235 checks, not repaired by this memory stage.

A separate invalid-arity probe finds that replace accepts four positional arguments in both WeavePy modes, while CPython raises TypeError. Excess upper/find/split arguments raise in all tested modes, though split wording differs. These observations are retained separately from performance results. A non-owning ASCII-cache lifetime concern was identified by source inspection but has not been reproduced or fixed at this stage.

The full 24-fixture census, startup/imports, parallel, and explicit GC-pause measurements are not repeated here. GC-index remains the latest complete census. No controlled build-latency or energy claim is made. Frozen inputs contain 112 sources and 37 measurement inputs.

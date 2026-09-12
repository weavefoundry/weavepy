# Weakref dictionary-key checkpoint

This is a checkpoint of ongoing optimization, not a completed performance claim. Weakref wrappers share seven immutable dictionary-key strings per thread, and common native getters use borrowed string probes. Per-target closures and traced callback ownership remain intact. The preceding collector-position candidate is deferred and lazy finalization metadata is rejected; neither runtime change is present.

The candidate executable is `8899e3679267f571bc5d50167197c8a558e19c5f3278bf3e74df0a8607213284`, 44,290,336 bytes, 1,024 bytes larger than baseline `36989234372e807ab95dc79686d9e5dd3bb51b458a9f7a1d92992357c30373a0`. That baseline includes the borrowed-handle collector change, which remains separately unmeasured.

All 341 VM tests, formatting, Clippy, no-default-features compilation, 42 targeted checks, 275 configured compatibility checks, and 114 performance-fixture checks pass. These checks use the existing compatibility expectations and do not establish complete CPython compatibility.

## Focused observations

The original quiet gate expired without launching a benchmark. A separate background-load diagnostic was declared before any samples. It retains nineteen cases, seven paired cycles per case, alternating order, one warm cycle, and separate unchanged frozen caches per binary. No samples were excluded or repeated. Work timers and process wall/CPU/RSS have different scopes; constructor replacement includes cleanup and collection. See both preserved protocols.

Observed one-minute load stayed between 3.164 and 3.497 on eight logical CPUs; five-minute load ranged from 3.988 to 4.288. Timing remains provisional under the declared diagnostic. Raw OS memory observations are included. Retained weakref and proxy heaps use about 6 to 7 percent less peak RSS. Several access and construction timers improve about 6 to 9 percent, while small control regressions remain. Ratios below one favor the candidate.

| Group / case | Mode | Work/baseline | Work CPU/baseline | Wall/baseline | CPU/baseline | RSS/baseline | Work/CPython | RSS/CPython |
|---|---|---:|---:|---:|---:|---:|---:|---:|
| heaps / ordinary_30000 | jit | 1.0004 | 0.9993 | 1.0090 | 1.0088 | 1.0011 | 5.1378 | 2.3730 |
| heaps / ordinary_30000 | interp | 0.9968 | 0.9957 | 0.9973 | 0.9973 | 0.9989 | 5.1103 | 2.2651 |
| heaps / ref_30000 | jit | 1.0040 | 1.0030 | 0.9891 | 0.9900 | 0.9324 | 5.6707 | 4.2457 |
| heaps / ref_30000 | interp | 0.9990 | 1.0004 | 0.9858 | 0.9860 | 0.9306 | 5.7917 | 4.1518 |
| heaps / callback_ref_30000 | jit | 0.9782 | 0.9787 | 0.9728 | 0.9774 | 0.9379 | 12.2485 | 4.5991 |
| heaps / callback_ref_30000 | interp | 0.9507 | 0.9497 | 0.9609 | 0.9606 | 0.9352 | 12.3557 | 4.4800 |
| heaps / proxy_30000 | jit | 0.9688 | 0.9698 | 0.9815 | 0.9813 | 0.9314 | 5.5964 | 4.2313 |
| heaps / proxy_30000 | interp | 0.9924 | 0.9927 | 0.9867 | 0.9867 | 0.9293 | 5.6193 | 4.1424 |
| heaps / callable_proxy_30000 | jit | 0.9973 | 0.9970 | 0.9901 | 0.9893 | 0.9321 | 5.1913 | 4.2329 |
| heaps / callable_proxy_30000 | interp | 0.9874 | 0.9890 | 0.9872 | 0.9902 | 0.9303 | 5.1954 | 4.1450 |
| access / ordinary_attr | jit | 0.9953 | 0.9949 | 1.0019 | 1.0026 | 1.0020 | 8.6493 | 2.0490 |
| access / ordinary_attr | interp | 1.0084 | 1.0060 | 1.0029 | 1.0004 | 0.9989 | 7.0646 | 1.8892 |
| access / ref_call | jit | 0.9221 | 0.9221 | 0.9450 | 0.9446 | 1.0005 | 17.5411 | 2.0607 |
| access / ref_call | interp | 0.9087 | 0.9079 | 0.9369 | 0.9364 | 0.9972 | 12.5872 | 1.8870 |
| access / ref_explicit_call | jit | 1.0002 | 0.9994 | 1.0014 | 1.0025 | 0.9995 | 12.0441 | 2.0616 |
| access / ref_explicit_call | interp | 1.0088 | 1.0101 | 1.0034 | 1.0039 | 0.9978 | 5.1011 | 1.8863 |
| access / callback_read | jit | 0.9249 | 0.9251 | 0.9563 | 0.9546 | 1.0015 | 15.5838 | 2.0344 |
| access / callback_read | interp | 0.9290 | 0.9287 | 0.9452 | 0.9439 | 0.9956 | 15.7141 | 1.8841 |
| access / proxy_attr | jit | 0.9650 | 0.9654 | 0.9721 | 0.9715 | 0.9995 | 32.4672 | 2.0418 |
| access / proxy_attr | interp | 0.9744 | 0.9742 | 0.9753 | 0.9757 | 0.9978 | 34.2962 | 1.8952 |
| access / proxy_call | jit | 0.9622 | 0.9627 | 0.9688 | 0.9683 | 1.0015 | 12.8650 | 2.0520 |
| access / proxy_call | interp | 0.9668 | 0.9668 | 0.9653 | 0.9688 | 0.9989 | 11.3518 | 1.8862 |
| access / weak_dict_get | jit | 0.9797 | 0.9790 | 0.9794 | 0.9791 | 1.0031 | 16.5072 | 2.0323 |
| access / weak_dict_get | interp | 0.9663 | 0.9668 | 0.9695 | 0.9689 | 0.9972 | 16.2908 | 1.8833 |
| access / dead_ref_call | jit | 0.9460 | 0.9448 | 0.9661 | 0.9640 | 1.0010 | 16.0424 | 2.0437 |
| access / dead_ref_call | interp | 0.9297 | 0.9292 | 0.9475 | 0.9489 | 0.9994 | 14.3840 | 1.8926 |
| construction / ordinary_3000 | jit | 1.0084 | 1.0082 | 1.0159 | 1.0158 | 1.0025 | 3.0332 | 2.0564 |
| construction / ordinary_3000 | interp | 0.9999 | 1.0000 | 1.0049 | 0.9985 | 1.0005 | 3.0741 | 1.9073 |
| construction / ref_3000 | jit | 0.9200 | 0.9196 | 0.9614 | 0.9587 | 0.9831 | 9.6174 | 2.2790 |
| construction / ref_3000 | interp | 0.9101 | 0.9127 | 0.9447 | 0.9451 | 0.9801 | 9.3205 | 2.1375 |
| construction / callback_ref_3000 | jit | 0.9409 | 0.9398 | 0.9613 | 0.9608 | 0.9838 | 13.4988 | 2.3169 |
| construction / callback_ref_3000 | interp | 0.9374 | 0.9380 | 0.9515 | 0.9525 | 0.9787 | 13.3744 | 2.1768 |
| construction / proxy_3000 | jit | 0.9249 | 0.9255 | 0.9567 | 0.9556 | 0.9852 | 10.7516 | 2.2842 |
| construction / proxy_3000 | interp | 0.9174 | 0.9177 | 0.9414 | 0.9417 | 0.9811 | 10.6657 | 2.1381 |
| construction / callable_proxy_3000 | jit | 0.9196 | 0.9198 | 0.9624 | 0.9608 | 0.9843 | 10.5835 | 2.2828 |
| construction / callable_proxy_3000 | interp | 0.9213 | 0.9211 | 0.9516 | 0.9515 | 0.9779 | 10.5477 | 2.1426 |
| construction / callback_churn_3000 | jit | 0.9392 | 0.9392 | 0.9539 | 0.9538 | 0.9716 | 15.6481 | 2.5831 |
| construction / callback_churn_3000 | interp | 0.9420 | 0.9429 | 0.9473 | 0.9469 | 0.9681 | 15.1288 | 2.4438 |

## Remaining work

Allocation profiling, startup controls, and a fresh full census remain outstanding for this candidate. The first profiling launch was rejected by automatic approval review because the account usage limit had been reached; no profile child launched. A draft that moves rare cache-fallback allocation code out of the factory is unapplied.

WeavePy remains slower and uses more peak RSS than CPython on every case in this focused screen. The latest complete full census is still gc-traversal-lists, whose broad timing is provisional because of host load. No energy, controlled build-time, or universal performance advantage is established.

The raw pairs, protocols, checks, source identities, and focused fixtures included here are a compact checkpoint. Full source snapshots, disassembly, frozen caches, and historical profiles remain in the local research archives. See the parent CHECKPOINT.md for the Git selection policy. Existing scripts retain their original target paths and require the corresponding local binaries and prepared inputs.

## Workspace checkpoint validation

The workspace-wide format, all-target/all-feature Clippy, Python-library build, all-target/all-feature tests, and documentation tests pass. The initial sandboxed test attempt stopped at a socket permission error; the full outside-sandbox run passes. Both logs and the aggregate results are preserved in workspace-validation. No runtime source edits occurred between the attempts.

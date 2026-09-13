# Compiler scratch retention in the complete runtime

The initial compiler cleanup candidate releases about 8.6 MiB of instrumented live heap after compiling the wide recursive workload. All 60 JIT, 351 VM, 156 C API, 99 targeted runtime, 275 compatibility, and 44 benchmark-path checks passed. This version is being refined before timing because its ordinary cleanup helper reserves 10,592 bytes of stack. No timing stage launched.

| Recursion depth | Reference live bytes | Candidate live bytes | Difference | Candidate/reference | Reference blocks | Candidate blocks |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 8 | 16,004,768 | 6,935,600 | -9,069,168 | 0.433346 | 54,100 | 54,126 |
| 64 | 16,210,624 | 7,137,632 | -9,072,992 | 0.440306 | 54,392 | 54,329 |
| 256 | 16,209,456 | 7,140,144 | -9,069,312 | 0.440493 | 54,357 | 54,383 |

The workload uses 128 local fields, an independently checked integer result, 120 calls, and verified nonleaf native recursion. All six declared captures completed, every owned child exited normally, and every per-binary frozen cache stayed unchanged. The raw full-stack histories and their decoded SHA-256 hashes are preserved. The table includes all captures; small differences in block counts remain visible.

The previous runtime is phase 78, SHA 6ba383e534d29c796f1823d75c78308c0b03d7e1498ef1514412bd926e8dae8c. The candidate is SHA 09891a1c6d5f1993e2d3e5a698f577e984644257a907b1e2ab1a730f11d5e169. It changes compiler scratch retention only; the primary phase 79 byte-budget experiment is excluded. Native scratch allocation totals remain 13,264 bytes at depth 8 and 84,992 bytes at depths 64 and 256 in both runtimes.

The large compiler-origin groups disappeared from the candidate capture. Allocation-origin attribution is evidence about live malloc blocks, not a full reachability proof or an exhaustive ownership partition. Instrumented malloc bytes exclude non-malloc executable mappings and are not uninstrumented RSS, peak RSS, elapsed/CPU time, allocator churn, or CPython object counts. The standalone requested-allocation probe found unchanged wide-compilation peak bytes and more allocation requests during subsequent compilations.

The current measured full-suite baseline remains substantially slower and larger than CPython. These retained-heap results do not establish the user's broader goal. The cold-reset revision will preserve the memory policy and move replacement temporaries off ordinary cleanup before timed evaluation.

Source changes are in changes.patch. The source and evidence archives preserve member paths and original-byte hashes listed in MANIFEST.json. The evidence includes the standalone allocation probe, all six full-stack malloc histories, validation, static code, initial-draft provenance, and the decision to refine before any timing samples.

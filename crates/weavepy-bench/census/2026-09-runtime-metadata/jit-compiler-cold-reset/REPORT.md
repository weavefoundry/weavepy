# Compiler scratch retention and cold reset

The candidate releases approximately 8.6 MiB of instrumented live heap after compiling three wide recursive workloads. Its ordinary cleanup frame uses 64 bytes of stack, versus 10,592 bytes in the initial compiler-scratch candidate. Full-suite execution time is nearly unchanged against the direct predecessor, while process wall time and peak RSS are slightly higher. Preserve this measured candidate for the next compiler-peak experiment; it is not an overall performance winner.

The primary phase 79 source and default CLI remain unchanged. This independent candidate is based on phase 78 and excludes the unmeasured native-scratch byte budget. The next independent change will investigate shared side-exit writeback to reduce compilation work and allocation peak.

| Comparison | 23-workload execution | 24-process wall time | 24-process CPU time | 24-process peak RSS |
| --- | ---: | ---: | ---: | ---: |
| Direct predecessor, 6ba383e5 | 0.998271230 | 1.002939183 | 1.001179689 | 1.002423913 |
| Earlier retained reference, bf2db3c5 | 0.979988880 | 0.987082841 | 0.986184852 | 1.000538869 |

Ratios are candidate/reference; below 1 is better. The retained-reference comparison includes earlier native-scratch, default-binding, and formatter changes. The direct comparison isolates the compiler-scratch policy and cold outlining. Direct comparison has 9 of 23 execution-time regressions and 23 of 24 peak-RSS regressions; the retained comparison has 8 and 13, respectively. Every row, pair, regression, and load observation is preserved.

The final retained-reference run measures 3.334055297 times CPython's execution time across all 23 workloads and 2.008605840 times CPython's peak RSS across 24 processes. Six workloads beat CPython on time; none beat its peak RSS. The historical 21-fixture cohort, including startup, measures 2.035787260 times CPython's time (2.038367618 as a ratio of medians). These cohorts remain separate. The user's broad goal is unachieved.

The 44 focused workloads have direct-predecessor execution ratios of 1.004662966 cold and 1.000409222 warm, with peak-RSS ratios of 1.001727982 and 1.000827827. Cumulative retained-reference ratios are 0.942109637 cold and 0.910694840 warm; their peak-RSS ratios are 1.000082590 and 0.999566281. Wide-depth-64 cold execution regressed by about 1.75% in every direct-comparison pair. Startup/import timings are close to the direct reference; the no-site CPU ratio is 1.012754767. These losses remain visible in the raw results.

The final candidate is 69f8c5cf55e305d02a127cb77f8f5e356c01726c6204fe860972f81ab589725b, 44,097,328 bytes, 64 bytes larger than the direct reference. Its compile_tfunc frame is 1,568 bytes, 16 above the direct reference. The rare reset retains a 10,592-byte frame. Native-call helper frames are unchanged. Individual static prologue sizes do not establish cumulative peak stack or RSS.

All 60 JIT, 351 VM, 156 C API, 99 targeted runtime, 275 compatibility, and 44 fixture-path checks passed. Strict compiler/runtime lint and no-JIT checking passed. Existing exact-PC, old-code lifetime, failed-definition recovery, callbacks, generators, and ownership checks remain intact. The known frame-identity mismatch is unchanged and explicitly preserved. No runtime unsafe code was added.

Six full-stack live-allocation captures completed with normal child exit, independent checksums, verified native paths, and unchanged per-binary frozen caches. Live allocator bytes at depths 8/64/256 were 16,005,120 / 16,209,152 / 16,209,936 for the reference and 6,934,000 / 7,141,264 / 7,138,752 for the candidate. Native scratch totals are unchanged. These are instrumented live malloc bytes, not peak allocations, RSS, allocator churn, or CPython object counts. The initial standalone probe found unchanged large-compilation peak requested bytes and more allocation requests in later compilations; that earlier version is preserved separately.

Each timing stage used the exclusive lease and the unchanged load gate. Host load varied after launch, and all samples were retained. The first launcher attempt, session 1450, failed during binary hashing because of an extra suffix in its executable path. It produced no workload-verification process or timing sample. Its telemetry, copied inputs, traceback, and original methods are preserved. Corrected v2 launchers changed only that path and used fresh output and telemetry names. Successful sessions: 20762 focused previous, 58118 full previous, 51999 focused retained, and 31718 full retained. Every gate closed complete/0 and every stage finished before the next started.

The archives preserve frozen source overlays, methods, validation, static code, allocation histories, every timing sample, and the pre-measurement launcher failure. MANIFEST.json records original member bytes and archive hashes. Build caches, executable binaries, frozen caches, and raw stack-log intermediates are excluded.

Generated standard-library trees copied by the startup harness stay on disk and are represented by SHA-256 inventories in this curated archive. This avoids archiving the same dependency sources twice. All preparation, validation, timing samples, cache checks, and load telemetry remain preserved. The first broader packaging is retained locally with its original manifest and hashes.

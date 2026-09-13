# Early sealing of synthetic guard blocks

The four-line compiler change seals each private guard block and continuation once their sole incoming edge exists. Real control-flow joins, OSR joins, helper joins, and shared terminal exits retain their previous sealing behavior. All 41 guard callers satisfy this condition. The candidate is preserved for its measured compiler-memory and cold-compilation benefits; it does not establish a broad runtime win. The primary runtime remains unchanged.

Against the immediately preceding shared-exit candidate, the standalone allocation probe reduced peak requested Rust allocation bytes by 0.4%, 3.0%, and 4.6% at widths 4, 32, and 128. Medium retained bytes fell 16.2%; wide retained bytes were unchanged. Three alternating pairs gave identical counts, and previously compiled entries and overflow state remained valid. The first wide reallocation count increased by two. These are allocator-request measurements, not peak RSS or CPython comparisons.

All three wide cold-runtime probes were 8.4% to 9.1% faster in all seven pairs, with 4.2% to 4.4% lower peak RSS. Warm workload execution did not improve, although process wall time and RSS benefited from compilation savings. The full census was effectively flat against the immediate predecessor.

All ratios below are candidate/reference, with smaller values better. Workload time uses the fixed 23-workload cohort; process peak RSS uses all 24 processes, including startup.

| Comparison | Workload time | Peak RSS | Time/CPython | RSS/CPython |
| --- | ---: | ---: | ---: | ---: |
| Immediate predecessor (phase 82) | 1.001077 | 1.000343 | 3.337551 | 2.016604 |
| Retained runtime (bf2), cumulative | 0.985996 | 1.003723 | 3.307478 | 2.014671 |

The cumulative comparison has six of 23 workloads faster than CPython and none of 24 processes using less peak memory. Ten workload rows and 19 RSS rows regress against bf2. Startup wall time also has small regressions. The historical 21-workload time/CPython ratio is 2.034318 in the retained comparison; it must not be mixed with the expanded 23-workload cohort. Cumulative changes include earlier default binding, formatting, native buffers, compiler cleanup, and shared exits.

Validation passed: 64 JIT tests, 351 VM tests, 156 C API tests, 99 targeted checks, 44 native paths, 275 compatibility checks, formatting, strict compiler/runtime lints, and the no-JIT build check. Static compiler and native prologue sizes are unchanged from phase 82. A known frame-identity mismatch remains. A separate post-timing oracle also confirmed a preexisting native-exit local-binding defect in bf2 and both newer binaries. This defect is retained as a distinct follow-up, not concealed by the passing existing suites.

Each of the four timing stages completed under the serial lease and fixed load gate. Every sample, checksum, cache check, binary identity, method identity, control, and regression is preserved. There were no retries or overlapping owned workloads in these stages. The archived standard-library inventories identify every excluded generated dependency file; binaries, build caches, and duplicate dependency trees are not committed. Source archives contain all 340 captured inputs. Controlled build time, energy, portability, scaling, and universal superiority were not measured.

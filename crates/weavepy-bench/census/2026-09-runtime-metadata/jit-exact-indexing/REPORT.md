# Exact built-in indexing: validated, unmeasured candidate

The candidate avoids callback setup for exact list, tuple, bytes, bytearray, and range subscription with an exact machine-integer index. It passed correctness and coverage checks, but no controlled timing sample was collected. The first load gate expired without launching a child. No speed, RSS, or overall CPython-superiority claim follows from these results.

The two-file patch is relative to the phase 85 object-indexing candidate. The complete source snapshot contains 340 files. The CLI SHA-256 is a18aab1fee0153421e1160b63b28c8cc46eadb9beaa47c766742b37a94444306, with 44,098,272 bytes, the same size as phase 85. The primary checkout and default CLI remain unchanged.

## Implementation and validation

The generic object-indexing helper retains receiver ownership and its integer-result speculation. For the five audited exact built-in receiver types, it calls the existing basic subscription implementation without creating an activation shell, marking the context dirty, or rechecking callback-sensitive guards. All other receivers retain the full protocol. Subclasses, dictionaries, and memoryviews are excluded from this shortcut. No arithmetic, ABI, eligibility threshold, pin representation, or result representation changes.

The candidate passed 71 JIT tests, 355 VM tests, 156 C API tests, formatting, strict compiler and runtime lint, and a build check without JIT support. Release validation passed 99 targeted checks, 44 inherited execution-path checks, and all 275 compatibility cases.

The expanded 34-case indexing oracle covers exact values and identity, negative and oversized indices, range arithmetic beyond machine width, bounds errors, subclass overrides, colliding dictionary keys, callback counts, global guard invalidation, recursion, and lifetimes. All cases preserve the prior interpreter's results; 33 match CPython. The existing staticmethod class-subscription mismatch remains explicit. Required indexing functions compile. All 14 prior local-state cases match CPython in the candidate's interpreter, JIT, and GIL-disabled configurations. GIL-disabled execution is a correctness control only.

All nine inherited focused workloads preserve their exact results. The formatting fallback control compiles in both phase 85 and this candidate and retains the expected results and reconstruction trace. Its previous-build compilation expectation was corrected before any runtime validation; the initial unexecuted preflight files remain preserved.

All 24 authoritative census results match CPython. Original benchmark definitions and work values are unchanged, with only the diagnostic timer footer replaced by exact result output. Complete compressed native traces remain available. The recorded static compiler and native-call stack prologues are unchanged; they do not measure cumulative stack use or RSS.

## Measurement status and retained failures

The focused comparison against phase 85 ran outside the filesystem sandbox under the exclusive workload lease. Its gate required three ten-second-spaced observations with one-minute and five-minute load averages at most four. It expired after 600 seconds with status `not_started`, process exit code 75, no child PID, no measurement directory, and no samples. The other three planned comparison stages were not launched. Any future attempt needs a fresh gate/output path. No unfavorable sample was discarded.

The first release-cache seed stopped before copying because free disk was below its declared floor. After verifying that no compiler was active and preserving all source and binary identities, only regenerable Cargo development artifacts were removed. A subsequent COW seed and release build succeeded. Both attempts and the cleanup evidence remain.

During the following experiment, 36 inherited input bytecode cache files were found missing. Their cause of removal is unknown. They were restored byte-for-byte from phase 85's verified evidence archive, matching this phase's frozen hashes; no existing file was overwritten. Phase 85's complete performance verifier and this phase's timing-readiness verifier then passed again. No benchmark source, method, or raw timing result changed.

## Archive

The curated archive preserves the complete source snapshot, baseline-relative patch, validation methods and outputs, semantic comparisons, native traces, failed preflight/seed/gate records, and the cache-restoration record. It excludes executable binaries, build outputs, generated standard-library trees, and per-runtime frozen caches. This is a preserved experiment, not a promoted overall performance win. No commit or push was performed during this continuation.

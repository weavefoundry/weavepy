# Lazy instruction caches

See [REPORT.md](REPORT.md) for the full 24-fixture suite, all 31 focused
controls, allocation probes, startup, and parallel execution. All 214
compatibility checks, 28 compiler tests, 49 JIT tests, and 289 VM tests pass.
Clippy, workspace/feature checks, and debug/release execution proofs pass.

The candidate records logical cache lengths and allocates slots on the first
valid write. Reads, clearing, cloning, and resizing keep cold code unallocated.
Actual release-library metadata shows a 496-byte code-object header, up from
488 bytes; initialized cache entries remain 16 bytes. The release executable
grows by 18,112 bytes. All time and memory regressions remain in the samples.

The initial build runner had a malformed cache-path assignment and stopped
before debug execution checks. Its source and explanation are preserved.
The corrected continuation passed debug/release checks before the candidate
was frozen for compatibility and timing. No benchmark used the malformed path.

Requesting free threading disables tier-2 native execution in this runtime.
GIL-disabled rows measure the interpreter even though the harness requests JIT.
No universal performance or faster-build claim is made.

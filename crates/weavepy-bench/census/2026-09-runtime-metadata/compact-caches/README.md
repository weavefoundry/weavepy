# Compact class-resolution caches

See [REPORT.md](REPORT.md) for the full 24-fixture suite, all 31 focused
controls, allocation probes, startup, and parallel execution. All 211
compatibility checks, 26 compiler tests, 49 JIT tests, and 289 VM tests pass.
Clippy, workspace/feature checks, and debug/release execution proofs also pass.
The original native coverage assertion failure and its corrected driver are
retained; the initial driver stayed interpreted in both releases.

The actual release-library layout proof reports 16-byte InlineCache and
CacheSlot entries, a 488-byte CodeObject, and a 416-byte TypeObject. The previous
release's corresponding sizes are 32, 32, 488, and 408 bytes. Layout metadata,
all raw samples, scripts, and exact source hashes are retained. No claim of
universal superiority to CPython or faster builds is made.

The report clarifies the existing tier-2 gate: requesting free threading
disables native execution. The GIL-disabled rows measure the interpreter,
even though the harness requests JIT execution in its environment.

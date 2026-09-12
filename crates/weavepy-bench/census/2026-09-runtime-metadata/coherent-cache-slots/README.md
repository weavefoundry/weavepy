# Coherent instruction-cache slots

This archive contains the complete 24-fixture census for the coherent-slot
release, all 220 compatibility checks, code and class allocation controls,
cold and warm regression controls, startup/import probes, native/interpreted
slot and call controls, and parallel execution measurements. Its
[report](REPORT.md) identifies the candidate binary and every comparison.
The frozen inputs contain 104 source files and 32 measurement inputs.

The release uses a native 128-bit atomic on this host and a nonblocking
three-word fallback on targets without always-lock-free wide atomics. The
production-module harness covers both compiler modules directly. Its Miri
configuration selects the fallback even on ARM64; it does not emulate native
wide-atomic instructions. The Loom model substitutes atomics in the exact
fallback source. Its source hash and dependency lock accompany the model.

Historical failures remain as evidence. The old UnsafeCell diagnostic is an
expected failure for the old implementation. The initial shell runner failed
before tests or builds began, and the first Clippy attempt rejected formatting
of two test literals. Both were corrected before the measured build. Initial
prototype files are superseded experiments, not the measured runtime inputs.
Use inputs/environment.json and the frozen sources to reproduce this release.

The cache repair does not establish complete free-threaded safety or enable
native execution with the GIL disabled. Remaining runtime limitations are
described in the frozen FREETHREADING documentation. All measurements and
regressions remain visible; the CPython-wide performance goal is unachieved.

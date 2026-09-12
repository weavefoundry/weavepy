# Exact attribute-cache names and native calls

This is a complete frozen measurement stage. See [REPORT.md](REPORT.md) for
all standard fixtures, focused controls, allocation probes, startup, and
parallel measurements. Source and measurement hashes are in
`inputs/environment.json`; compatibility results are in `compatibility/`.
All 205 compatibility checks and 284 VM tests pass. The unchanged JIT sources
passed 49 tests at the scalar-leaf stage; that log is retained here. Clippy,
workspace and feature checks, debug/release trace proofs, and explicit float,
Boolean, and integer-division scalar-leaf entry proofs also pass.

The scalar-leaf and metadata-sharing stages have their own focused samples in
the adjacent archives. They do not have separate complete standard-suite
censuses. The collision diagnostic reproduces a preexisting semantic bug in
the checkpoint and the intermediate candidates; this final candidate fixes it.

WeavePy wins 6 of 23 workload timers and 0 of 24 peak-RSS comparisons against
CPython. The broad performance objective remains unmet. No build-latency or
universal performance claim is made.

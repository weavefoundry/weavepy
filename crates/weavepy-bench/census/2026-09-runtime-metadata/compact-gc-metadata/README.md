# Compact collector metadata

This archive contains the full 24-fixture census, all 226 compatibility checks,
52 focused controls, explicit collection-pause distributions, cold and warm
regression controls, allocation/numeric probes, startup/import probes, and
parallel execution measurements. The [report](REPORT.md) identifies the binary
and all comparison releases. Frozen inputs contain 109 source files and
35 measurement inputs.

The collector uses separate atomic bytes for bounded color and generation
state. Counts, positions, memory orders, and locks retain their behavior.
Actual matching release-library layouts record 96-byte preceding handles and
80-byte candidate handles. Process measurements cover allocator behavior,
collection overhead, retained objects, and regressions.

All 300 VM tests and 49 JIT tests pass. New lifecycle checks cover rooted and
frozen cycles, weak references, finalizers, promotion and its cap, unfreeze,
and position changes after removal. Constructor indices are checked before
narrowing. This change adds no unsafe code and does not establish complete
free-threaded safety or enable native execution without the GIL.

Pause controls retain all 51 explicit full-collection times per process for
rooted, unreachable, and frozen 10,000-node graphs. Automatic GC is disabled.
Seven paired process cycles follow a discarded process cycle. The report
compares per-process median, nearest-rank p95, and maximum summaries. These
are explicit collector pauses, not application-wide tail latency.

Draft candidate and control scripts are preserved as development history.
The production inputs and environment hashes identify the measured code.
The public Rust color constants and two TrackedHandle field types change to
byte-sized state. Python generation arguments remain unchanged. Two native slice defects diagnosed in reference and candidate are retained
with reproducers and compilation/deoptimization traces: a minimum-integer
stop collides with the missing-bound sentinel, and two-bound fallback restores
an extra operand. Neither is repaired by the collector change. Address-distribution
diagnostics retain actual dictionary pointers and hash-bucket occupancy; they
do not measure actual probe counts or establish the cause of the regression. All measured regressions remain visible; the CPython-wide objective is unachieved.

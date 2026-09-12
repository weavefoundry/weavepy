# Atomic cache-table publication controls

This stage replaces OnceLock table publication with an owning AtomicPtr.
The [report](REPORT.md) includes all 217 compatibility checks and nine-cycle
code-storage, cold, and warm regression controls. It has no separate full
24-fixture census. The lazy-caches-initial archive remains the latest complete
standard-suite census. The executable is identified by the inputs environment
and release-library layout files. Source snapshots and measurement inputs are
frozen; later workspace edits do not reproduce this candidate.

The table change recovers most of the interpreted generator slowdown and
returns list controls to approximately the compact-cache baseline. String
controls remain slower, type creation regresses, and measured peak RSS is
approximately unchanged versus the preceding lazy-cache/default-suffix build.
All samples and regressions remain visible.

The per-slot UnsafeCell race is preexisting and remains unfixed in this
candidate. An isolated Miri diagnostic and the worker-sharing audit accompany
the results. The publication Miri tests use disjoint writes or initialization
only; they must not be represented as proof of shared-slot safety.

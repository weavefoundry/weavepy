# Keep deferred-instance calls independent of filter collisions

The cached-predicate tests can execute correctly while never reaching the intended
fast path. On `949d5df`, both Windows and macOS CI record successful classification,
warmup, call-slot matching, and argument-binding checks, followed by zero calls
passing the instance-release guard. Direct evaluation of the cached predicate
succeeds.

A local repeated-process diagnostic reproduces the rejected state: an instance
has two strong owners, its tracking is still deferred, and the exact collector
registry says it is untracked. The collector's probabilistic filter nevertheless
reports a possible match. The general drop grader conservatively marks that case,
and the pure-call guard rejects the otherwise safe release. Allocator placement
can therefore decide whether a small function uses the optimization.

`core_droppable` now accepts a shared instance whose existing deferred-tracking
flag is set. The flag proves that neither the collector nor the weakref registry
holds an owner. The required strong count greater than one still rejects the
final-owner release. Non-atomic stores, weakref creation, explicit tracking queries,
and other tracking operations revoke the flag before publishing a collector owner.
Tracked instances retain the existing grader. The change adds no registry lookup,
metadata, retained owner, allocator adjustment, or JIT admission.

The general drop grader and collector remain unchanged. Existing predicate and
slot coverage assertions still require more than 1,000 interpreter-path hits;
semantic checks still run with the JIT both enabled and disabled.

## Validation

A new isolated Rust regression models stale filter bits at a live instance's
address. It verifies that the exact registry is empty and two program owners
remain. The old guard fails this test; the new guard passes. The same test verifies
that the final owner is rejected and that a non-atomic store revokes the proof,
leaving an operand plus collector handle that still requires prompt cleanup.

The new Python fixture passes CPython 3.14.5 and accepted `5066f29` in JIT,
interpreter-only, and GIL-disabled modes before the change. It checks aliases,
explicit tracking, weakrefs, non-atomic stores, cycles, and temporary finalizers.
All 388 VM tests, the embedding lifecycle, and VM Clippy with the two established
exclusions pass locally. Release validation, paired throughput/CPU/RSS measurements,
startup measurements, and platform CI are in progress. No aggregate performance improvement
is claimed yet. Local logs, source snapshots, and measurements belong under
`target/performance/deferred-instance-drop-investigation/`; the original failure
logs remain under `target/performance/embedding-stack-bounds-investigation/`.

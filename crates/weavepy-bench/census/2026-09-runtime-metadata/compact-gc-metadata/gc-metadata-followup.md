# Narrow bounded collector state

Unimplemented and unmeasured. Runtime remains frozen at67e7 during census6966.
The next candidate changes only TrackedHandle's color AtomicI64 and generation
AtomicUsize to AtomicU8. Four colors are0..3, generations0..2. Keep gc_refs
signed64, all position/reference counts usize, every atomic memory order,
index/generation lock ordering, and all separate flags unchanged.

Constructor must reject generation >= N_GENERATIONS before conversion, never
silently truncate. Add a compile-time bound for a nonempty generation range
representable byu8. Promote with saturating addition and the bounded maximum;
convert to usize only for indexing. Public Rust field and color constant types
change. No new unsafe operations or shared bitfield read-modify-write scheme.

Audited live crates: all color and generation operations are in gc_trace.rs;
external Rust code holds Arc<TrackedHandle>, and no C-API layout/offset consumer
was found. Track/new temporary child handles start at0. Frozen uses a separate
color sentinel3, not an out-of-range generation. Untrack clamps a generation
under its vector lock and falls back to identity search. Promotion and unfreeze
are the only generation stores. Do not substitute narrower counts elsewhere.

Actual old/new layouts must be compiled from matching release rlibs before/after
editing, using target/record_gc_metadata_layout.py. Estimated96->80 bytes is
NOT measured evidence. Keep old67e7 binary and frozen snapshot.

Useful validation: independent GcState tests for live roots promoting0->1->2,
remaining capped, freeze exclusion, unfreeze reset, swap-remove positions, and
weak-backed reclamation after dropping the roots. Exercise cycle/finalizer and
freeze APIs in CPython and both releases, JIT/interpreter/GIL0. Full compatibility
includes GC, weakref, finalization, cross-thread heap, and multiprocessing checks.
No Miri run solely for using standard AtomicU8 is needed; no new unsafe added.

Measure retained list/dict/set/tuple containers, cold/populated/native instances,
normal and GC-disabled allocation, rooted full scans, unreachable cycle scans,
freeze/unfreeze, and GC latency distributions. Keep wall/CPU/peakRSS, exact work,
assertions, paired processes, CPython, and the preceding67e7 binary. Keep all
regressions. Do not compare CPython gen1 population (3.14 changed generations)
or use its shaped sys.getsizeof estimate as actual WeavePy memory.

Draft prepared: target/prepare_gc_metadata_candidate.py writes only
 target/gc_metadata_candidate.rs and before/candidate hashes. It has two new
local-state Rust tests for promotion/freeze/swap-remove and invalid constructor
indices. It has not changed runtime sources or been compiled.
Additional drafts: gc_metadata_regression.py checks rooted/frozen cyclic nodes,
weakref reclamation, finalizers exactly once through0/1/2/fullcollections.
gc_metadata_probes.py has11 controls. gc_pause_probes.py records all51 explicit
full-GC pauses per process after one discarded collection, rooted/unreachable/
frozen10000-node graphs, seven alternating process cycles. Reports median,
nearest-rank p95 (49th/51), max, elapsed, CPU, and RSS. Automatic GC disabled;
these are explicit-collection latency controls, not application-wide tail latency.
All drafts remain unexecuted while6966 is measuring.

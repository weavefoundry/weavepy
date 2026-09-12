# Exact range collection and population scaling

Unimplemented lead from source inspection. lib.rs::do_list_or_tuple_call calls
collect_iterable. That function clones exact lists/tuples and directly collects
sets, but exact Range reaches its generic make_iter/iter_next loop with an
initially empty Vec. builtins.rs::b_list and b_tuple likewise build an empty Vec
and repeatedly push iterator results. Verify actual dispatch/profiles before
claiming which route dominates a workload. Generic iterables have observable
__len__/__length_hint__/__iter__/__next__ behavior; do not skip or reorder it.

A narrowly typed exact-range path could reserve the known number of elements
and fill inline integer Objects without repeated interpreter iterator calls.
Audit i64/i128/BigInt bounds, negative steps, empty ranges, length overflow,
fallible capacity reservation, tracking/tracemalloc parity, and current GIL /
pending-work polling before changing it. Preserve mutable/overridden iterator
and range-iterator behavior separately. Do not test the old runtime on enormous
ranges that it might try to allocate indefinitely; use small boundary-value
ranges and bounded population workloads for comparisons.

Potential measurement expansion: exact integer list/tuple populations across
10k,100k,500k,1m,2m retained elements, measuring construction time, wall/CPU and
actual process RSS against the same CPython executable, with checksums and exact
length/endpoints. The inline24-byte Object layout may amortize its larger
startup footprint at large populations, but this is a hypothesis, not a result.
Dynamic Vec growth may also change peak allocation. Keep sizes and both losing
and winning regions rather than selecting only a favorable large population.
No such benchmark has run yet, and no allocation/constructor optimization has
been implemented. Separate this from the current pin-pressure experiment.

Further bounded design audit (not implemented or measured): an exact Range
whose start, stop, and step fit i64 can compute its population in i128 without
overflow, reserve precisely, and fill inline Object::Int values. Keep BigInt
and wider-bound ranges on the existing route initially. Increment an i128
cursor so the last addition can cross an i64 endpoint safely. A population
above isize::MAX needs the CPython length-overflow behavior; failed capacity
reservation must become MemoryError, not panic. Avoid huge-allocation probes.
Audit both collect_iterable and b_list/b_tuple and keep result tracking and
tracemalloc treatment on their existing paths. Exact tuple identity remains.
Neither runtime dispatch nor polling behavior has been changed. The list_ops
fixture constructs list(range(256)) only once, so population gains would not
establish a large list_ops improvement.

Follow-up after applying the exact-range Vec collector: Object::new_tuple still
moves the finished Vec into TupleStorage::from_vec's final Arc allocation, so
large tuple construction can retain two element buffers briefly. The population
experiment must establish the actual peak before attributing a regression or
claiming an opportunity. A later direct integer-tuple fill could avoid the
intermediate Vec, but would extend the existing DST initialization code and
requires a separate layout, lifetime, impossible-capacity, panic-safety, and
Miri audit. Don't generalize uninitialized storage to arbitrary ExactSizeIterator
implementations: their length promises and iteration can be wrong or panic.
No direct tuple fill implementation or measurement has run.

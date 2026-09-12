# Native temporary retention and warm string RSS

Unimplemented. The nine-cycle native-slice controls record string-method RSS:
new cold JIT 30.11 MiB, warm JIT 57.28 MiB; interpreter cold26.80/warm27.08;
CPython cold14.48/warm14.95. Precedingb63 JITcold30.11/warm57.38. This is an
existing memory difference, not a regression introduced by the slice repair.
Raw target/native-slice-focused/cold.json and warm.json include every sample.
Warm workload time newJIT89.01ms, interp99.85ms, CP31.94ms (marginal medians,
not paired ratios). Do not claim the pin table causes this without diagnostics.

VM tier2.rs~1698: PinTable=Vec<Pin>. Runtime cap65536 appends bounds memory.
At cap helper deopts, activation exits/drains runtime pins, OSR may reenter.
Per-activation retention can keep many already-dead string/list temporaries.
This is a plausible opportunity for speed and memory improvement together.

Need trace native execution and actual allocations/retention before changing.
A cap-only experiment would trade deoptimization cost against peak memory;
keep original controls and compare several lengths, not only this fixture.
A stronger solution would reclaim dead pins at loop safe points using complete
liveness of typed locals, SSA/block arguments, spills, and nested native calls.
Pin ids cannot shift while any live native value may refer to them. Entry pins,
constant-pin memoization, parked results, helper arguments, iterator roots,
and deopt reconstruction must remain valid. A free-slot scheme needs exact
liveness and must never reuse a live alias's pin. Pins may hold values with
finalizers/weakrefs: draining can run Python, mutate burned-in resolutions,
and requires invalidation/revalidation before resuming. Leaf strings/bytes
and exact numeric lists could be an initially narrower recycling target,
but complete native liveness is still needed. No production changes/prototype.

Read-only audit: lower.rs emit_poll (~2564) currently calls wpjit_poll(frame)
without spilling current native locals or the boundary stack; emit_exit writes
those only on the pending/deopt branch. wpjit_poll (~2693) yields the GIL and
rechecks hot gates and guarded resolutions; its safety contract explicitly
says it never runs Python or touches exchange buffers. Therefore the existing
poll cannot safely reclaim pins merely by inspecting frame buffers, which may
be stale. A reclamation design would need an explicit live-root exchange before
the call, including comprehension boundary stacks and managed iterator locals.
Clearing dead slots without shifting ids could reduce retained object memory,
but Vec<Pin> still reaches the 65536-slot cap unless helper allocation gains a
correct free-slot mechanism. Every pin-producing helper currently relies on
append indices. Merely lowering the cap trades memory for more deoptimization;
it is not the same as reclaiming dead temporaries while remaining native.

A potentially smaller design to investigate: request a distinct resource
safepoint at an OSR-capable loop header once runtime pins exceed a threshold,
use the existing full deopt snapshot/rebuild to retain only live frame values,
drain temporary pins with existing callback handling, then reenter the same
native loop through its existing OSR entry. This could avoid implementing pin
free lists or moving pin ids inside an active native activation. It must not
count resource pressure as a speculative type failure or retire good code.
The current generic deopt path's accounting and OSR reentry behavior need audit.
Reentry must recheck observers, GIL/native gates, code identity, globals, defaults,
cells, guarded resolutions, and effects of Python run during prompt reaping.
Do not reexecute a completed call, consume an iterator twice, lose exceptions,
or reconstruct an invalid comprehension boundary stack. Initially restrict
resource safepoints to existing OSR entries with complete reconstruction data.
A prototype must measure warm/cold memory and time over multiple workload sizes;
this is only a design lead, not implemented or validated.

Accounting audit: tier2.rs note_native_exit (~7540) increments both visible
native-entry statistics and CodeEntry.native_entries, then applies the generic
call ratio backoff and charges every JitStatus::Deopt to ce.deopts. Direct-call
exit handling (~6808) independently charges Deopt. A resource-safepoint design
must cover both paths and nested calls; simply changing the framed path would
still retire direct callees. Adding compaction entries to the denominator can
also weaken generic-call backoff, so preserve its intended logical accounting.
finish_dyn_native_result (~5684) explicitly preserves existing pin ownership
when reading an integer from a pin; it does not recycle that pin because local
aliases can still refer to it. Preserve this invariant during any compaction.

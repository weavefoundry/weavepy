# Batch native scratch-pool access

Unimplemented follow-up from the native attribute profile. Every ordinary
native-to-native call takes three u64 vectors and two u32 vectors, then returns
them through five independent TLS lookups. The reverted one-slot stack trial
changed stack size and pool circulation and had mixed control results.

A separate candidate could take all five buffers under one JIT_BUFS borrow,
then clear and resize each after releasing the borrow. Returning them could
likewise use one borrow. Preserve the exact per-type pop and push order,
the existing 64-vector cap per type, zero initialization, and all exit cleanup.
Primitive buffer drops cannot call Python. Never hold the RefCell borrow
across native entry, stack growth, callbacks, or interpreter reconstruction.

This could remove repeated TLS access without enlarging the retained pool
or adding inline storage to each native frame. It still needs actual code
generation, stack-size, throughput, and RSS measurement; fewer source-level
TLS calls do not establish lower machine cost. Use the corrected outer-loop
coverage gate, deep native recursion, exceptions, overflow, and controls.
No implementation or measured benefit exists yet.

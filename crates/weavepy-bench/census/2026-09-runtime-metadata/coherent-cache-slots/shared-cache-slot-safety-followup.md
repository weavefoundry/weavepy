# Shared instruction-cache slot race

The preexisting CacheSlot uses UnsafeCell<InlineCache> with unsafe Send/Sync
requiring the GIL. Worker threads share original function/code objects, and
the interpreter continues reading and writing code.caches with gil=0. The
native JIT gate does not prevent this tier1 access. FREETHREADING.md section2c
already identifies the invariant mismatch.

The isolated diagnostic target/cache-slot-race-probe/probe.rs copies the
actual bytecode module and adds two threads writing one shared slot without
a GIL. Miri strict provenance reports a non-atomic write/write data race at
CacheSlot::set. Full result: target/cache-slot-race-miri.txt. This proves the
slot API's behavior under unsynchronized access, not an end-to-end VM Miri run.
The worker sharing path statically supplies that access in GIL-disabled mode.

The new AtomicPtr CacheTable publication is separately tested: concurrent
initialization and disjoint-slot writes pass nine tests under Miri default
and Tree Borrows on ARM64 and i686. These tests do not prove shared-slot
safety, and the table publication change does not fix this existing race.

AtomicU128 remains unstable in both local Rust1.98.1 and workspaceMSRV1.93,
despite target_has_atomic=128. target/atomic-u128-support.json records actual
compiler failures. Do not enable unstable library features or weaken release/
acquire publication for speed. portable-atomic's AtomicU128 is a possible
follow-up; no dependency or atomic slot implementation has been added.

A wide atomic representation must encode every valid InlineCache variant
without reading uninitialized enum padding. Prefer safe explicit encoding/
decoding first; only consider layout-dependent unsafe conversion with a
complete representation proof, cross-width/endian tests, and Miri. Benchmark
the read/write costs and fallback platforms, preserve every regression.
Thread-confined caches are an alternative but require code-sharing, cloning,
GC, tracing, fork, and memory audits. Fixing one cache does not establish all
free-threaded runtime invariants; CpCache/VmExt and type guards remain open.

An isolated safe-encoding prototype now exists in target/atomic-slot-probe.
All56 variants encode/decode through initialized u128 fields. Local11tests
and ARM64 strict-provenance Miri pass, including same-slot concurrent writes.
The workspace dependency/source is unchanged. Further work must inspect
portable-atomic's fallback lock and fork behavior before adopting it across
platforms: a lock inherited from a vanished thread must not hang cache access
in a forked child. Host lock-freedom alone is not a portability proof. Consider
a bounded, nonblocking coherent fallback on targets without wide atomics;
never spin forever on a cache writer left behind at fork, and do not use
wrapping small seqlock epochs. Inspect optimized encoding/decoding code too;
safe enum reconstruction may add dispatch overhead. Neither performance nor
cross-platform readiness has been established for the slot prototype.

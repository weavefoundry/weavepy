# Keep one-slot native-call scratch buffers on the stack

The warm attribute CPU profiles identify native call setup and take/put buffer
pool functions as substantial work on the VM thread. The launcher's ulock_wait
samples describe its waiting join thread, not a VM bottleneck. These two
five-second profiles are diagnostic, not paired timing evidence. The separate
counter probe produces identical native/method/fallback/pin-pressure counts
across ownership, compaction, and decoder releases, with identical CPython
results. It does not diagnose the cause of the remaining 2.8 percent regression.

The candidate changes only try_native_call's argument exchange buffers. A
call_cap of one uses initialized [u64; 1] and [u32; 1] locals. Larger capacities
retain pooled vectors. The original max(1) capacity is preserved. Local and
spill buffers, pin ownership, native tables, guards, recursion accounting, and
all exit reconstruction remain as before. No pin deduplication is implemented.

Native callees exclude generator/coroutine bodies, so these buffers aren't
parked for a later resume. Both arrays outlive the synchronous compiled call,
stacker's temporary stack switch, and any deopt completion. No new unsafe block
or public API is added. The additional stack storage is small but deep native
recursion still needs its existing checks; performance and memory require
measurement rather than inference from fewer pool operations.

The new permanent fixture exercises zero, one, and three-argument nested method
shapes, recursive methods, integer overflow, and exception propagation without
repeating completed side effects. It passes on CPython and the preceding release.
The VM test requires more than1,000 native calls and native exits and compares
against interpreter output; it passes on the draft implementation.

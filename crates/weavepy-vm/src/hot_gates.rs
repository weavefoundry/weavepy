//! The unified eval-breaker word — RFC 0059 WS2.
//!
//! The dispatch loop used to probe six independent "is any deferred
//! work pending?" gates before every instruction: parked `__del__`
//! finalizers, C-extension drop queues, deferred `ResourceWarning`s,
//! cross-thread async exceptions, tripped signals, and the
//! interpreter-finalizing flag — each "just one relaxed atomic load",
//! collectively a measurable slice of dispatch (the RFC 0059 profile).
//! CPython folds the same requests into a single `eval_breaker` word;
//! this module is that word.
//!
//! # Discipline
//!
//! Every subsystem keeps its existing precise gate (counter / flag /
//! queue) as the source of truth; the bit here is a *scheduling hint*:
//!
//! - **A spurious set bit is always safe.** The dispatch loop re-checks
//!   the subsystem's precise gate before doing work, so a stale bit
//!   costs one cold probe, never correctness.
//! - **A cleared bit while work is pending is never allowed.** Queue
//!   producers enqueue *first*, then set the bit. Queue consumers use
//!   clear-drain-recheck: clear the bit, drain, then re-set if the
//!   precise gate is still hot (covering a push that raced the drain).
//!   Level-style flags (async-exc, finalizing) are maintained on state
//!   transitions under their owners' existing synchronization.
//!
//! `Relaxed` ordering throughout: the loop only needs eventual
//! visibility (worst case, a few instructions of latency — the same
//! contract the granular gates had), and every consumer performs an
//! `Acquire` read of the precise gate before touching queue payloads.

use std::sync::atomic::{AtomicU32, Ordering};

/// Parked `__del__` requests exist on some thread's queue
/// (`vm_singletons::PENDING_FINALIZERS`).
pub const PENDING_FINALIZERS: u32 = 1 << 0;
/// C-extension drop queues are non-empty (`PENDING_CEXT_DROPS`).
pub const PENDING_CEXT: u32 = 1 << 1;
/// Deferred destructor `ResourceWarning`s are queued.
pub const RESOURCE_WARNINGS: u32 = 1 << 2;
/// A `PyThreadState_SetAsyncExc` exception is scheduled somewhere.
pub const ASYNC_EXC: u32 = 1 << 3;
/// A signal tripped and handlers are due on the main thread.
pub const SIGNALS: u32 = 1 << 4;
/// `Py_Finalize` has begun: spawned daemon workers must unwind.
pub const FINALIZING: u32 = 1 << 5;

/// The word itself. Process-global, like the granular gates it fuses
/// (all of them were process-wide statics; per-thread queues keep
/// their thread-local precision behind the shared hint).
static HOT: AtomicU32 = AtomicU32::new(0);

/// One relaxed load — the dispatch loop's entire pending-work probe.
#[inline]
pub fn load() -> u32 {
    HOT.load(Ordering::Relaxed)
}

/// Raise `bits`. Producers call this *after* publishing the work the
/// bit advertises.
#[inline]
pub fn set(bits: u32) {
    HOT.fetch_or(bits, Ordering::Relaxed);
}

/// Lower `bits`. Consumers call this *before* draining, then re-`set`
/// if the precise gate is still hot (clear-drain-recheck).
///
/// No unit test mutates this word: it is process-global and the crate's
/// tests run concurrently — behavior is covered end-to-end by the VM
/// finalizer/signal/async-exc suites.
#[inline]
pub fn clear(bits: u32) {
    HOT.fetch_and(!bits, Ordering::Relaxed);
}

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

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

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
/// Python-level pending calls (`Py_AddPendingCall` /
/// `_PyEval_AddPendingCall`) are queued — RFC 0068 WS3
/// (`vm_singletons::push_pending_py_call`).
pub const PENDING_PYCALLS: u32 = 1 << 6;
/// RFC 0068 WS3 — cross-interpreter `pending_identify` probes
/// (test_capi.test_misc TestPendingCalls.test_isolated_subinterpreter):
/// armed while a `_testinternalcapi.pending_identify` waiter is queued.
pub const PENDING_IDENTIFY: u32 = 1 << 7;

/// The word itself. Process-global, like the granular gates it fuses
/// (all of them were process-wide statics; per-thread queues keep
/// their thread-local precision behind the shared hint).
static HOT: AtomicU32 = AtomicU32::new(0);

/// RFC 0065 (WS1): the dispatch loop's *generation* word. Bumped by
/// every mutation that can change the loop prologue's decisions —
/// hot-gate bits ([`set`]/[`clear`] below), observer registration
/// (`trace::bump_observer_gen`), the GC finalizable/suspect
/// population transitions (`gc_trace`), and frame materialization
/// (`FrameShell::materialize`). The dispatch loop caches a snapshot
/// of the prologue's inputs keyed by this generation: while the
/// generation is unchanged *and* the snapshot says "quiet" (no
/// pending work, no finalizables, no suspects, no observers, no
/// materialized frame), the loop runs one relaxed load + compare per
/// instruction instead of the full ten-plus-probe prologue.
///
/// Same producer/consumer discipline as [`HOT`]: a spurious bump is
/// always safe (one cold re-snapshot); a missed bump is never
/// allowed, which each producer guarantees by bumping inside the
/// same critical section that publishes its state change. `Relaxed`
/// suffices — cross-thread visibility rides the GIL hand-off, whose
/// lock is a full barrier (the granular gates' existing contract).
static LOOP_GEN: AtomicU64 = AtomicU64::new(1);

/// One relaxed load — the dispatch loop's entire pending-work probe.
#[inline]
pub fn load() -> u32 {
    HOT.load(Ordering::Relaxed)
}

/// The current loop generation (RFC 0065 WS1).
#[inline]
pub fn loop_gen() -> u64 {
    LOOP_GEN.load(Ordering::Relaxed)
}

/// Invalidate every dispatch loop's cached prologue snapshot
/// (RFC 0065 WS1). Cheap (one relaxed RMW); call from any mutation
/// site whose state the loop prologue consults.
#[inline]
pub fn bump_loop_gen() {
    LOOP_GEN.fetch_add(1, Ordering::Relaxed);
}

/// Raise `bits`. Producers call this *after* publishing the work the
/// bit advertises.
#[inline]
pub fn set(bits: u32) {
    HOT.fetch_or(bits, Ordering::Relaxed);
    bump_loop_gen();
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
    bump_loop_gen();
}

/// RFC 0077 (WS2): once-read debug/bisection environment flags for
/// paths hot enough that a `getenv` per call showed up in the census
/// (`__findenv_locked` on `list_ops`). Each is read on first use and
/// cached for the process lifetime, which is also how CPython treats
/// its `PYTHON*` variables.
pub mod env_flags {
    macro_rules! once_flag {
        ($(#[$m:meta])* $name:ident, $var:literal) => {
            $(#[$m])*
            #[inline]
            pub fn $name() -> bool {
                static FLAG: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
                *FLAG.get_or_init(|| std::env::var_os($var).is_some())
            }
        };
    }
    once_flag!(
        /// `WEAVEPY_REAP_TRACE`: prompt-reap tracing.
        reap_trace,
        "WEAVEPY_REAP_TRACE"
    );
    once_flag!(
        /// `WEAVEPY_NO_QUIET`: pin the dispatch loop to its full prologue.
        no_quiet,
        "WEAVEPY_NO_QUIET"
    );
    once_flag!(
        /// `WP_DBG_SAMPLE`: periodic frame-entry sampling to stderr.
        dbg_sample,
        "WP_DBG_SAMPLE"
    );
    once_flag!(
        /// `WP_REAP_DBG`: reap-cascade debugging.
        reap_dbg,
        "WP_REAP_DBG"
    );
    once_flag!(
        /// `WEAVEPY_CMP_BT`: backtrace on unsupported comparisons.
        cmp_bt,
        "WEAVEPY_CMP_BT"
    );
    once_flag!(
        /// `WEAVEPY_LEN_DBG`: `len()` fallback debugging.
        len_dbg,
        "WEAVEPY_LEN_DBG"
    );
    once_flag!(
        /// `WEAVEPY_TRACE_INIT`: builtin-kwargs rejection tracing.
        trace_init,
        "WEAVEPY_TRACE_INIT"
    );
}

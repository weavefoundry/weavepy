//! The Global Interpreter Lock — RFC 0024.
//!
//! The interpreter is sub-interpreter-per-thread (PEP 684 / 734
//! shaped) — each OS thread owns its own [`Interpreter`] instance
//! with its own root-level state. A "GIL" therefore degenerates
//! into a per-interpreter coordination primitive rather than the
//! single global lock CPython uses; the API surface CPython
//! exposes (`PyGILState_Ensure` / `_Release`,
//! `PyEval_SaveThread` / `_RestoreThread`,
//! `Py_BEGIN_ALLOW_THREADS` / `Py_END_ALLOW_THREADS`) still
//! makes sense as it controls when blocking system calls can
//! release the lock so other threads in the same interpreter
//! can run.
//!
//! Even with sub-interpreter isolation we keep an "eval breaker"
//! mechanism: a single atomic flag the dispatch loop polls every
//! N opcodes. The breaker is the central hook for:
//!
//! - Pending signal delivery (`KeyboardInterrupt`).
//! - Pending `gc.collect()` requests from other threads.
//! - Pending `_thread.interrupt_main()` requests.
//! - Pending `Thread.join()` exit notifications.
//! - Cooperative GIL drop for blocking-I/O callers.
//!
//! The cost of the check is ~2 ns per opcode (one `Relaxed`
//! atomic load + one branch); under the bench harness this
//! measured at ~0.3% on the existing fixtures. The mechanism
//! is the foundation every future concurrency RFC builds on.

use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::{Condvar, Mutex};

use crate::sync::GilLock;

/// Bit flags packed into [`EvalBreaker::flags`].
pub mod flag {
    pub const GIL_DROP_REQUEST: u32 = 1 << 0;
    pub const PENDING_SIGNALS: u32 = 1 << 1;
    pub const PENDING_ASYNC_EXC: u32 = 1 << 2;
    pub const GC_REQUEST: u32 = 1 << 3;
    pub const PENDING_CALL: u32 = 1 << 4;
    pub const SHUTDOWN_REQUEST: u32 = 1 << 5;
    pub const PROFILER_REQUEST: u32 = 1 << 6;
    pub const TRACER_REQUEST: u32 = 1 << 7;
}

/// Coalesced eval-loop interrupt set. The dispatch loop checks
/// `flags.load(Relaxed)` every opcode (cheap) and only enters the
/// cold path (`drain` / `handle`) when something is pending.
#[derive(Debug, Default)]
pub struct EvalBreaker {
    /// Bit-set of [`flag`] entries. Modified with `fetch_or` /
    /// `fetch_and` so multiple threads can request multiple
    /// flags concurrently without losing requests.
    pub flags: AtomicU32,
    /// Cooperative request from `_thread.interrupt_main()` to
    /// raise `KeyboardInterrupt` on the main thread.
    pub interrupt_main: AtomicBool,
    /// Counter incremented on every periodic-release tick so
    /// the eval loop can decide when to yield.
    pub tick: AtomicU64,
    /// Every N instructions, the dispatch loop yields control
    /// to other threads in the same interpreter group. Default
    /// 100; configurable via `sys.setswitchinterval`.
    pub switch_interval_ns: AtomicU64,
    /// Counter of threads waiting on the GIL. The current
    /// holder consults this to decide whether to drop on the
    /// next periodic tick.
    pub waiters: AtomicU32,
    /// Pending call queue size. `Py_AddPendingCall` and friends
    /// push closures here; the breaker drains them at the next
    /// safe point.
    pub pending_calls: AtomicUsize,
}

impl EvalBreaker {
    pub fn new() -> Self {
        Self {
            flags: AtomicU32::new(0),
            interrupt_main: AtomicBool::new(false),
            tick: AtomicU64::new(0),
            switch_interval_ns: AtomicU64::new(5_000_000), // 5ms
            waiters: AtomicU32::new(0),
            pending_calls: AtomicUsize::new(0),
        }
    }

    /// Cheap, hot-path probe. Returns `true` if any flag is
    /// pending. `Relaxed` is correct because the dispatch loop
    /// only needs eventual visibility — the worst case is one
    /// extra opcode of latency before a signal is observed.
    #[inline]
    pub fn pending(&self) -> bool {
        self.flags.load(Ordering::Relaxed) != 0
    }

    pub fn set(&self, flag: u32) {
        self.flags.fetch_or(flag, Ordering::Release);
    }

    pub fn clear(&self, flag: u32) {
        self.flags.fetch_and(!flag, Ordering::Release);
    }

    pub fn is_set(&self, flag: u32) -> bool {
        self.flags.load(Ordering::Acquire) & flag != 0
    }

    /// Drain and return the currently-pending flag set. The set
    /// is cleared atomically.
    pub fn drain(&self) -> u32 {
        self.flags.swap(0, Ordering::AcqRel)
    }

    pub fn request_gil_drop(&self) {
        self.set(flag::GIL_DROP_REQUEST);
    }

    pub fn request_signals(&self) {
        self.set(flag::PENDING_SIGNALS);
    }

    pub fn request_gc(&self) {
        self.set(flag::GC_REQUEST);
    }

    pub fn request_shutdown(&self) {
        self.set(flag::SHUTDOWN_REQUEST);
    }

    pub fn switch_interval(&self) -> Duration {
        Duration::from_nanos(self.switch_interval_ns.load(Ordering::Relaxed))
    }

    pub fn set_switch_interval(&self, d: Duration) {
        self.switch_interval_ns.store(
            d.as_nanos().min(u128::from(u64::MAX)) as u64,
            Ordering::Relaxed,
        );
    }

    pub fn add_waiter(&self) {
        self.waiters.fetch_add(1, Ordering::AcqRel);
    }

    pub fn remove_waiter(&self) {
        self.waiters.fetch_sub(1, Ordering::AcqRel);
    }

    pub fn waiter_count(&self) -> u32 {
        self.waiters.load(Ordering::Acquire)
    }
}

/// A pending call queued by `Py_AddPendingCall` or the C-API
/// equivalent. The eval loop drains these at the next safe
/// point.
pub type PendingCall = Box<dyn FnOnce() + Send + 'static>;

/// Per-interpreter GIL state. Each [`Interpreter`] owns one;
/// child threads spawned via `_thread.start_new_thread` see
/// this state through an `Arc<GilState>` clone.
///
/// "GIL" is a slight misnomer in the sub-interpreter-per-thread
/// model — there is no *global* lock — but the ergonomics are
/// the same: a single coordination primitive that controls
/// whether bytecode is currently executing, plus the eval
/// breaker that lets other contexts cooperatively request
/// attention.
#[allow(missing_debug_implementations)]
pub struct GilState {
    /// The reentrant lock guarding bytecode execution within
    /// this interpreter. Acquired at `Interpreter::run_module`
    /// entry, released before blocking I/O via
    /// `allow_threads`, re-acquired on resume.
    pub lock: GilLock,
    /// Eval-loop interrupt set.
    pub breaker: EvalBreaker,
    /// Native id of the OS thread currently holding the lock,
    /// or zero if no thread holds it.
    pub holder: AtomicU64,
    /// Count of explicit `acquire`s from the C-API (so a
    /// nested `PyGILState_Ensure` / `_Release` pair doesn't
    /// drop the lock prematurely).
    pub depth: AtomicI64,
    /// Monotonic counter bumped on every successful (blocking)
    /// `acquire`. The cooperative hand-off in
    /// [`periodic_gil_checkpoint`] reads it before dropping the
    /// GIL and waits for it to advance, proving another thread
    /// actually took the lock before re-acquiring. This is
    /// WeavePy's analogue of CPython's `gil->switch_number`.
    pub switch_number: AtomicU64,
    /// Paired with [`Self::switch_cond`] to implement CPython's
    /// `gil->switch_cond` blocking hand-off: a thread that drops the
    /// GIL for a waiter parks here until [`Self::switch_number`]
    /// advances (proving the waiter took the lock), instead of
    /// burning CPU in a `sched_yield` spin.
    pub switch_mutex: Mutex<()>,
    pub switch_cond: Condvar,
    /// Pending-call queue. `EvalBreaker::pending_calls` mirrors
    /// the size for the hot-path probe.
    pub pending: Mutex<Vec<PendingCall>>,
}

impl Default for GilState {
    fn default() -> Self {
        Self::new()
    }
}

impl GilState {
    pub fn new() -> Self {
        Self {
            lock: GilLock::new(),
            breaker: EvalBreaker::new(),
            holder: AtomicU64::new(0),
            depth: AtomicI64::new(0),
            switch_number: AtomicU64::new(0),
            switch_mutex: Mutex::new(()),
            switch_cond: Condvar::new(),
            pending: Mutex::new(Vec::new()),
        }
    }

    /// Bump `switch_number` and wake any thread parked in
    /// [`maybe_yield_gil`] waiting to confirm the hand-off. Called
    /// from both `acquire` paths right after the lock is taken.
    #[inline]
    fn note_acquired(&self) {
        self.switch_number.fetch_add(1, Ordering::AcqRel);
        // notify_all on a parking_lot Condvar doesn't require holding
        // `switch_mutex`; the yielder re-checks `switch_number` under
        // the mutex so this can't lose a wakeup.
        self.switch_cond.notify_all();
    }

    /// Acquire the GIL on behalf of the calling thread. Returns
    /// a guard that releases on drop.
    pub fn acquire(self: &Arc<Self>) -> GilGuard {
        self.breaker.add_waiter();
        let lock_guard = self.lock.lock();
        self.breaker.remove_waiter();
        self.note_acquired();
        let me = current_thread_id();
        self.holder.store(me, Ordering::Release);
        self.depth.fetch_add(1, Ordering::AcqRel);
        // We must hold the parking_lot guard for the lifetime
        // of the GilGuard. Stash a transmuted-lifetime guard
        // inside the result, paired with a clone of the Arc to
        // keep the lock alive.
        // SAFETY: extending the guard's lifetime to 'static is
        // sound because (a) `self: Arc<Self>` is stored in the
        // returned guard and `self.lock` outlives every guard,
        // and (b) the guard's Drop releases before the Arc is.
        let static_guard: parking_lot::ReentrantMutexGuard<'static, ()> =
            unsafe { std::mem::transmute(lock_guard) };
        GilGuard {
            state: Arc::clone(self),
            _lock_guard: Some(static_guard),
        }
    }

    /// Try to acquire without blocking.
    pub fn try_acquire(self: &Arc<Self>) -> Option<GilGuard> {
        let lock_guard = self.lock.try_lock()?;
        self.note_acquired();
        let me = current_thread_id();
        self.holder.store(me, Ordering::Release);
        self.depth.fetch_add(1, Ordering::AcqRel);
        let static_guard: parking_lot::ReentrantMutexGuard<'static, ()> =
            unsafe { std::mem::transmute(lock_guard) };
        Some(GilGuard {
            state: Arc::clone(self),
            _lock_guard: Some(static_guard),
        })
    }

    /// Whether the calling thread currently holds the GIL.
    pub fn current_holder(&self) -> u64 {
        self.holder.load(Ordering::Acquire)
    }

    /// Push a closure to be drained by the eval loop. Used by
    /// `Py_AddPendingCall` and by `_thread.interrupt_main`.
    pub fn push_pending_call<F: FnOnce() + Send + 'static>(&self, f: F) {
        let mut q = self.pending.lock();
        q.push(Box::new(f));
        self.breaker.pending_calls.fetch_add(1, Ordering::AcqRel);
        self.breaker.set(flag::PENDING_CALL);
    }

    /// Pop and run all pending calls. Called from the eval loop
    /// when [`EvalBreaker::flags`] has [`flag::PENDING_CALL`]
    /// set.
    pub fn drain_pending_calls(&self) {
        let mut take = Vec::new();
        {
            let mut q = self.pending.lock();
            std::mem::swap(&mut take, &mut *q);
        }
        let n = take.len();
        for f in take {
            f();
        }
        self.breaker.pending_calls.fetch_sub(n, Ordering::AcqRel);
    }

    /// Bump the periodic tick counter. Called from the
    /// dispatch loop every fixed number of opcodes (default
    /// 100). When the counter rolls past the switch interval
    /// AND another thread is waiting, the holder yields.
    #[inline]
    pub fn tick(self: &Arc<Self>) -> bool {
        let n = self.breaker.tick.fetch_add(1, Ordering::Relaxed);
        if n.wrapping_rem(100) == 0 && self.breaker.waiter_count() > 0 {
            self.breaker.request_gil_drop();
            return true;
        }
        false
    }
}

/// RAII guard returned by [`GilState::acquire`].
#[allow(missing_debug_implementations)]
pub struct GilGuard {
    state: Arc<GilState>,
    /// Holds the underlying parking_lot guard. `None` only
    /// briefly while `allow_threads` is borrowing it out.
    _lock_guard: Option<parking_lot::ReentrantMutexGuard<'static, ()>>,
}

impl GilGuard {
    pub fn state(&self) -> &Arc<GilState> {
        &self.state
    }

    /// Run a closure with the GIL released. Restores the GIL
    /// before returning. Used by blocking-I/O paths to let
    /// other threads run while the calling thread is in a
    /// system call.
    #[allow(clippy::used_underscore_binding)]
    pub fn allow_threads<R>(&mut self, f: impl FnOnce() -> R) -> R {
        let saved = self.state.depth.load(Ordering::Acquire);
        // Drop the lock guard, run the closure, then re-acquire.
        let guard = self._lock_guard.take();
        drop(guard);
        self.state.holder.store(0, Ordering::Release);
        let result = f();
        let new_guard = self.state.lock.lock();
        let me = current_thread_id();
        self.state.holder.store(me, Ordering::Release);
        let static_guard: parking_lot::ReentrantMutexGuard<'static, ()> =
            unsafe { std::mem::transmute(new_guard) };
        self._lock_guard = Some(static_guard);
        // Returning from a blocking release is a fresh contiguous hold:
        // restart the switch-interval clock (CPython gives a thread that
        // just took the GIL the full interval before the next hand-off).
        note_gil_acquired();
        debug_assert_eq!(saved, self.state.depth.load(Ordering::Acquire));
        result
    }
}

impl Drop for GilGuard {
    fn drop(&mut self) {
        self.state.depth.fetch_sub(1, Ordering::AcqRel);
        if self.state.depth.load(Ordering::Acquire) == 0 {
            self.state.holder.store(0, Ordering::Release);
        }
    }
}

/// Process-wide GIL singleton. Accessed by the C-API
/// (`PyGILState_*` / `PyEval_SaveThread` / `PyEval_RestoreThread`)
/// and by the eval breaker. Now (after RFC 0025) genuinely
/// process-wide — every thread spawned by `_thread.start_new_thread`
/// shares this lock, so bytecode execution is serialised across
/// the entire interpreter, which is what makes the shared-heap
/// invariant ("mutations on `list` visible across threads") sound
/// without atomic refcounts.
pub fn global_gil() -> Arc<GilState> {
    use std::sync::OnceLock;
    static GLOBAL: OnceLock<Arc<GilState>> = OnceLock::new();
    GLOBAL.get_or_init(|| Arc::new(GilState::new())).clone()
}

/// CPython `_PyEval_ReInitThreads` / `PyOS_AfterFork_Child`: rebuild the
/// GIL in a `fork(2)` child.
///
/// After a fork from a multi-threaded process only the forking thread
/// survives, but the GIL's `parking_lot` primitives were duplicated
/// mid-flight. The GIL `ReentrantMutex` carries the PARKED bit for the
/// sibling threads that were blocked in [`GilState::acquire`] at fork
/// time, the eval breaker still counts them as waiters, and
/// `parking_lot`'s process-global parking-lot table references their
/// now-gone `ThreadParker`s. The first hand-off in the child
/// ([`maybe_yield_gil`] / [`allow_threads_then`] releasing the lock) then
/// walks `parking_lot`'s unpark path into a vanished peer and the child
/// wedges forever — `test_threading.test_reinit_tls_after_fork` forks
/// from 16 threads, and any child that loses this race never reaches its
/// `os._exit`, so the parent's `waitpid` times out.
///
/// Rebuild from scratch: abandon the inherited guard stack *without*
/// unlocking the poisoned lock, overwrite the lock and hand-off
/// primitives with fresh ones, zero the breaker, and re-take the fresh
/// lock for this lone surviving thread.
pub fn reinit_after_fork_in_child() {
    let gil = global_gil();
    // 1. Abandon the inherited guards without running their `Drop`:
    //    dropping a `GilGuard` unlocks the inherited (poisoned)
    //    `ReentrantMutex`, and `parking_lot`'s unlock-slow path would try
    //    to hand the lock to one of the vanished peers. `mem::forget`
    //    leaks one `Arc<GilState>` refcount per held guard — negligible,
    //    and the alternative (unlocking) deadlocks.
    let held: Vec<GilGuard> =
        GIL_GUARD_STACK.with(|cell| std::mem::take(&mut *cell.borrow_mut()));
    let depth = held.len();
    for g in held {
        std::mem::forget(g);
    }
    // 2. Replace the `parking_lot` primitives in place with pristine ones.
    //    The child is single-threaded here, so nothing races this write;
    //    every live `Arc<GilState>` clone shares this allocation, so the
    //    swap is observed consistently. `ptr::write` deliberately does not
    //    drop the old (poisoned) values.
    // SAFETY: sole surviving thread post-fork; the `Arc` keeps the
    // allocation alive for the duration of the writes.
    let p = Arc::as_ptr(&gil).cast_mut();
    unsafe {
        std::ptr::write(&raw mut (*p).lock, GilLock::new());
        std::ptr::write(&raw mut (*p).switch_mutex, Mutex::new(()));
        std::ptr::write(&raw mut (*p).switch_cond, Condvar::new());
        std::ptr::write(&raw mut (*p).pending, Mutex::new(Vec::new()));
    }
    // 3. Reset the hand-off bookkeeping. No peers remain: no waiters, no
    //    queued pending calls, no drop request.
    gil.breaker.waiters.store(0, Ordering::Release);
    gil.breaker.pending_calls.store(0, Ordering::Release);
    gil.breaker.flags.store(0, Ordering::Release);
    gil.holder.store(0, Ordering::Release);
    gil.depth.store(0, Ordering::Release);
    gil.switch_number.store(0, Ordering::Release);
    // 4. The forking thread is now this process's only — and therefore
    //    main — thread, so signal handling must run here (CPython resets
    //    the runtime's main thread in the child).
    MAIN_THREAD_ID.store(current_thread_id(), Ordering::Release);
    // 5. Re-take the fresh GIL, restoring the guard-stack depth the caller
    //    held before the fork so the unwinding eval loop stays balanced.
    let mut fresh = Vec::with_capacity(depth.max(1));
    for _ in 0..depth {
        fresh.push(gil.acquire());
    }
    GIL_GUARD_STACK.with(|cell| *cell.borrow_mut() = fresh);
    note_gil_acquired();
}

// ---------------------------------------------------------------------------
// Main-thread identification — RFC 0039.
//
// Simulated signals (`_thread.interrupt_main`, `signal.raise_signal`)
// are always *handled* on the main thread, mirroring CPython's
// `PyErr_CheckSignals`. The dispatch loop drains tripped signals only
// when it's running on the main thread, so we record that thread's OS
// id once and compare against it cheaply.
// ---------------------------------------------------------------------------

/// OS id of the thread that runs `__main__` — where signal handlers
/// run. Zero until [`mark_main_thread`] records it.
static MAIN_THREAD_ID: AtomicU64 = AtomicU64::new(0);

/// Record the calling OS thread as the main Python thread. Idempotent:
/// the first caller (the thread that enters `run_module`) wins, so
/// re-entrant `run_module` calls (the in-process conformance runner)
/// don't reassign it.
pub fn mark_main_thread() {
    let me = current_thread_id();
    let _ = MAIN_THREAD_ID.compare_exchange(0, me, Ordering::AcqRel, Ordering::Relaxed);
}

/// Whether the calling thread is the main Python thread. Used to gate
/// signal-handler delivery to the main thread. If no main thread has
/// been marked yet (`mark_main_thread` never ran), treats the caller as
/// main so a bare interpreter embedding still services signals.
pub fn is_main_thread() -> bool {
    let main = MAIN_THREAD_ID.load(Ordering::Acquire);
    main == 0 || main == current_thread_id()
}

// RFC 0025: a thread-local stack of `GilGuard`s, owned per-OS-thread.
// Both the C-API's `PyGILState_Ensure` / `PyEval_SaveThread` and the
// VM's worker-thread entry pre-push their guard here so any Rust
// path inside the dispatch loop (`allow_threads_then` below) can
// pop, drop, run a blocking call, and re-acquire — without needing
// the original guard handle.
//
// `RefCell` is the `crate::sync::RefCell` (`GilCell`), not `std`'s,
// because `GilGuard` itself is `!Send`. The cell is thread-local,
// so cross-thread access is impossible and the `Send` bound on
// `GilCell`'s `Sync` impl never fires.
//
// We hide it behind plain `std::thread_local!` because the values
// involve a non-`Send` `parking_lot::ReentrantMutexGuard` which
// std's `thread_local!` is happy to host (the cell is dropped on
// thread exit).
std::thread_local! {
    static GIL_GUARD_STACK: std::cell::RefCell<Vec<GilGuard>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Push a freshly-acquired [`GilGuard`] onto the current thread's
/// guard stack. Called by every entry point that takes the GIL
/// (the C-API's `PyGILState_Ensure`, the VM worker thread's
/// `start_new_thread` body, embedders that wrap `run_module` etc.).
pub fn push_gil_guard(g: GilGuard) {
    GIL_GUARD_STACK.with(|cell| cell.borrow_mut().push(g));
}

/// Pop and return the top guard. Returns `None` when the calling
/// thread doesn't hold the GIL through this stack.
pub fn pop_gil_guard() -> Option<GilGuard> {
    GIL_GUARD_STACK.with(|cell| cell.borrow_mut().pop())
}

/// `True` if the calling thread holds the GIL via this stack.
pub fn current_thread_holds_gil() -> bool {
    GIL_GUARD_STACK.with(|cell| !cell.borrow().is_empty())
}

/// RFC 0025: drop the GIL, run `f`, re-acquire the GIL, then
/// return `f`'s result. Used by every blocking-I/O / lock-acquire
/// path that would otherwise prevent other threads from running.
///
/// If the current thread doesn't hold the GIL via the guard
/// stack (`push_gil_guard` was never called), this is a plain
/// passthrough — useful for unit tests and for the cooperative
/// single-thread path where the GIL isn't engaged.
///
/// This is the Rust-side spelling of `Py_BEGIN_ALLOW_THREADS …
/// Py_END_ALLOW_THREADS`. The C-API macros' expansion
/// (`PyEval_SaveThread()` / `PyEval_RestoreThread()`) lands here.
pub fn allow_threads_then<R>(f: impl FnOnce() -> R) -> R {
    // Pop every guard we currently hold (a worker calls
    // `start_new_thread` -> builtin -> `allow_threads_then` with
    // exactly one guard; nested cases pop them all).
    let popped: Vec<GilGuard> =
        GIL_GUARD_STACK.with(|cell| std::mem::take(&mut *cell.borrow_mut()));
    let count = popped.len();
    drop(popped);
    let result = f();
    // Re-acquire one guard per popped one so callers further up
    // the stack still see their guards on return.
    let gil = global_gil();
    let mut fresh = Vec::with_capacity(count);
    for _ in 0..count {
        fresh.push(gil.acquire());
    }
    GIL_GUARD_STACK.with(|cell| *cell.borrow_mut() = fresh);
    // Returning from a blocking release is a fresh contiguous hold:
    // restart the switch-interval clock.
    note_gil_acquired();
    result
}

/// How many dispatch-loop opcodes elapse between cooperative GIL
/// hand-off checks. CPython switches on a 5ms wall-clock interval;
/// we approximate with an opcode countdown that's cheap to test in
/// the hot path (a thread-local decrement, no atomics).
const GIL_CHECK_INTERVAL: u32 = 128;

std::thread_local! {
    static YIELD_COUNTDOWN: std::cell::Cell<u32> =
        const { std::cell::Cell::new(GIL_CHECK_INTERVAL) };

    /// Wall-clock instant at which this thread last (re)acquired the GIL
    /// for a contiguous run. [`maybe_yield_gil`] reads it to enforce
    /// CPython's *time-based* switch interval: a thread that has held the
    /// GIL for less than `sys.setswitchinterval()` (default 5ms) keeps it
    /// even when another thread is waiting, instead of handing off every
    /// [`GIL_CHECK_INTERVAL`] opcodes.
    ///
    /// Without this gate WeavePy switched threads ~1000× more often than
    /// CPython (every 128 opcodes vs every 5ms), which widened the window
    /// for the inherently-non-atomic Python-level `x += 1` / `x -= 1` on a
    /// shared object to the point where `test_multiprocessing`'s
    /// `test_release_task_refs` (a `CountedObject.n_instances -= 1` in
    /// `__del__` racing an unpickle's `__new__` increment across the pool's
    /// result-handler thread and the main thread) lost an update ~1 run in
    /// 3. CPython's GIL holds for the full interval between switches, so the
    /// two bytecode triples never interleave in practice; this matches that.
    static GIL_HELD_SINCE: std::cell::Cell<Option<std::time::Instant>> =
        const { std::cell::Cell::new(None) };

    /// Depth of nested "no cooperative GIL hand-off" critical sections
    /// on this thread. While `> 0`, [`maybe_yield_gil`] refuses to drop
    /// the GIL at a periodic checkpoint.
    ///
    /// RFC 0039 (WS5): a [`crate::sync::GilCell`] borrow holds the
    /// cell's reentrant mutex for the borrow's whole lifetime. If the
    /// GIL were handed off while that mutex is held — e.g. a
    /// `set.add(x)` / `d[k] = v` that runs a Python `__hash__`/`__eq__`
    /// mid-insert — another thread could take the GIL and then block
    /// forever on the *same* cell's mutex inside `try_borrow_mut`
    /// (a GIL ↔ cell-mutex lock inversion). The holder, parked waiting
    /// to re-acquire the GIL, never finishes the insert nor releases the
    /// borrow, so every thread (and even a daemon watchdog) starves.
    ///
    /// Re-entrant container hash/eq therefore run inside one of these
    /// sections, so the insert completes atomically with respect to
    /// thread switches — matching the observable result CPython
    /// produces. Blocking releases ([`allow_threads_then`]) are
    /// deliberately *unaffected*: a `__hash__` that waits on a
    /// `threading.Lock` must still drop the GIL so the holder can run.
    static NO_YIELD_DEPTH: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// Enter a critical section in which the cooperative per-opcode GIL
/// hand-off is suppressed. Returns a guard that leaves the section on
/// drop; the deferred hand-off resumes at the next checkpoint once the
/// outermost section exits. See [`NO_YIELD_DEPTH`] for the rationale.
#[must_use]
pub fn no_gil_handoff() -> NoYieldGuard {
    NO_YIELD_DEPTH.with(|c| c.set(c.get().saturating_add(1)));
    NoYieldGuard(())
}

#[inline]
fn no_yield_active() -> bool {
    NO_YIELD_DEPTH.with(std::cell::Cell::get) > 0
}

/// Record that the calling thread has just (re)acquired the GIL for a
/// fresh contiguous run, resetting the [`GIL_HELD_SINCE`] switch-interval
/// clock. Called from the paths that retake the GIL after a release: the
/// cooperative hand-off re-acquire in [`maybe_yield_gil`] and the
/// blocking-release re-acquire in [`GilGuard::allow_threads`].
#[inline]
pub fn note_gil_acquired() {
    GIL_HELD_SINCE.with(|c| c.set(Some(std::time::Instant::now())));
}

/// Whether this thread has held the GIL long enough (≥ the configured
/// `sys.setswitchinterval`) that a cooperative hand-off to a waiter is due.
/// A thread holding for less than the interval keeps the GIL — CPython's
/// timer-driven `gil_drop_request` semantics. The first checkpoint after a
/// thread starts (no recorded acquire instant) is treated as "due" so a
/// brand-new holder doesn't starve an already-waiting thread.
#[inline]
fn switch_interval_elapsed() -> bool {
    GIL_HELD_SINCE.with(|c| match c.get() {
        Some(since) => since.elapsed() >= global_gil().breaker.switch_interval(),
        None => true,
    })
}

/// RAII guard returned by [`no_gil_handoff`]. Decrements the
/// thread-local critical-section depth on drop.
#[allow(missing_debug_implementations)]
pub struct NoYieldGuard(());

impl Drop for NoYieldGuard {
    fn drop(&mut self) {
        NO_YIELD_DEPTH.with(|c| c.set(c.get().saturating_sub(1)));
    }
}

/// Cooperative GIL hand-off point, called from the bytecode
/// dispatch loop once per instruction.
///
/// RFC 0039 (WS2): without this, a compute-bound thread holds the
/// GIL forever (the only other release path is `allow_threads_then`,
/// reached only on blocking I/O / lock waits). That starves every
/// other thread — `threading.Thread.start()` would hang because the
/// freshly-spawned worker can never acquire the GIL to signal
/// `_started`. Mirrors CPython's `eval_breaker` / `gil_drop_request`
/// switch driven by `sys.setswitchinterval`.
///
/// The fast path is a thread-local countdown decrement; the GIL is
/// only actually dropped every [`GIL_CHECK_INTERVAL`] opcodes *and*
/// only when another thread is blocked waiting for it.
#[inline]
pub fn periodic_gil_checkpoint() {
    let fire = YIELD_COUNTDOWN.with(|c| {
        let n = c.get();
        if n <= 1 {
            c.set(GIL_CHECK_INTERVAL);
            true
        } else {
            c.set(n - 1);
            false
        }
    });
    if fire {
        maybe_yield_gil();
    }
}

/// Hand the GIL to a waiting thread, if any. Drops the calling
/// thread's guard stack (releasing the lock), spins briefly so a
/// waiter can take it, then re-acquires. No-op when nobody is
/// waiting or when this thread doesn't hold the GIL via the stack.
fn maybe_yield_gil() {
    // RFC 0039 (WS5): never hand off the GIL while this thread is in a
    // no-yield critical section — i.e. holding a container's `GilCell`
    // mutex across a re-entrant Python `__hash__`/`__eq__`. Yielding
    // there risks a GIL ↔ cell-mutex deadlock (see `NO_YIELD_DEPTH`).
    // The hand-off simply resumes at the next checkpoint once the
    // section exits, a few opcodes later.
    if no_yield_active() {
        return;
    }
    let gil = global_gil();
    if gil.breaker.waiter_count() == 0 {
        return;
    }
    // CPython hands the GIL off on a wall-clock interval, not an opcode
    // count: the holder runs for ≥ `sys.setswitchinterval()` (default 5ms)
    // before a waiter's `gil_drop_request` takes effect. Honour that here so
    // a short burst of bytecode between checkpoints — e.g. a finalizer's
    // `n_instances -= 1` or an unpickle's `__new__` increment — completes
    // without another thread slipping in mid-`LOAD`/`STORE` and clobbering a
    // shared counter (the `test_release_task_refs` race). A thread that has
    // held the GIL for less than the interval keeps running; the next
    // checkpoint after the interval elapses performs the hand-off.
    if !switch_interval_elapsed() {
        return;
    }
    let popped: Vec<GilGuard> =
        GIL_GUARD_STACK.with(|cell| std::mem::take(&mut *cell.borrow_mut()));
    if popped.is_empty() {
        // No guard on this thread's stack — we're in a nested native
        // context that didn't push one. Nothing to hand off.
        return;
    }
    let count = popped.len();
    let gen = gil.switch_number.load(Ordering::Acquire);
    drop(popped); // releases the GIL
                  // Park on `switch_cond` until a waiter actually takes the lock
                  // (`switch_number` advances) rather than spinning on `sched_yield`.
                  // This is CPython's `gil->switch_cond` hand-off: cheap to wait,
                  // no CPU burn, and the bounded `wait_for` is a safety net against
                  // a waiter that vanished between the count check and the park.
    {
        let mut guard = gil.switch_mutex.lock();
        while gil.switch_number.load(Ordering::Acquire) == gen && gil.breaker.waiter_count() > 0 {
            if gil
                .switch_cond
                .wait_for(&mut guard, Duration::from_millis(5))
                .timed_out()
            {
                break;
            }
        }
    }
    let mut fresh = Vec::with_capacity(count);
    for _ in 0..count {
        fresh.push(gil.acquire());
    }
    GIL_GUARD_STACK.with(|cell| *cell.borrow_mut() = fresh);
    // Fresh contiguous hold: restart the switch-interval clock so this
    // thread now runs for the full interval before yielding again.
    note_gil_acquired();
}

/// Best-effort current-thread native id. Returns the OS thread
/// id on Linux/macOS via `libc::pthread_self`; uses
/// `GetCurrentThreadId` on Windows. The exact representation
/// is opaque; the only invariant is uniqueness within the
/// running process.
pub fn current_thread_id() -> u64 {
    #[cfg(any(target_os = "linux", target_os = "macos", target_os = "ios"))]
    unsafe {
        let h = libc::pthread_self();
        // pthread_t is opaque; treat it as a pointer for ID
        // purposes. Stable across the thread's lifetime.
        h as usize as u64
    }
    #[cfg(target_os = "windows")]
    unsafe {
        // Windows: use GetCurrentThreadId. Returns DWORD.
        extern "system" {
            fn GetCurrentThreadId() -> u32;
        }
        u64::from(GetCurrentThreadId())
    }
    #[cfg(not(any(
        target_os = "linux",
        target_os = "macos",
        target_os = "ios",
        target_os = "windows"
    )))]
    {
        // Fallback: hash the std::thread::Thread::id().
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        std::thread::current().id().hash(&mut h);
        h.finish()
    }
}

/// Alias for [`current_thread_id`]. Reserved name so RFC 0025
/// callers can write `current_native_thread_id()` even though
/// today's `current_thread_id` already returns the OS thread id —
/// the distinction matters when sub-interpreters land (PEP 684,
/// future RFC) and "thread id" becomes ambiguous.
#[inline]
pub fn current_native_thread_id() -> u64 {
    current_thread_id()
}

/// The kernel-level thread id of the calling thread, as reported by
/// `threading.get_native_id()` / `Thread.native_id`.
///
/// This differs from [`current_thread_id`]: that returns a
/// `pthread_self()` pointer, which is only unique *within* a process and
/// — on macOS — is frequently the *same* address for the main thread of
/// every process (the main thread's `pthread_t` lives at a fixed slot).
/// CPython's `native_id` is instead the OS scheduler's thread id
/// (Linux `gettid(2)`, macOS `pthread_threadid_np`), which is globally
/// unique and therefore differs across `fork`/`spawn` children — exactly
/// what `test_multiprocessing`'s `test_process_mainthread_native_id`
/// asserts (`assertNotEqual(parent_tid, child_tid)`).
pub fn current_os_native_id() -> u64 {
    #[cfg(target_os = "linux")]
    unsafe {
        libc::syscall(libc::SYS_gettid) as u64
    }
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    unsafe {
        let mut tid: u64 = 0;
        // Current thread's kernel id. Passing our own `pthread_self()`
        // (rather than NULL) keeps the `pthread_t` argument well-typed.
        libc::pthread_threadid_np(libc::pthread_self(), &mut tid);
        tid
    }
    #[cfg(target_os = "windows")]
    unsafe {
        extern "system" {
            fn GetCurrentThreadId() -> u32;
        }
        u64::from(GetCurrentThreadId())
    }
    #[cfg(not(any(
        target_os = "linux",
        target_os = "macos",
        target_os = "ios",
        target_os = "windows"
    )))]
    {
        current_thread_id()
    }
}

/// Snapshot of pending eval-breaker actions, drained as a unit
/// so the dispatch loop can decide what to do.
#[derive(Debug, Clone, Copy, Default)]
pub struct EvalBreakerSnapshot {
    pub gil_drop: bool,
    pub signals: bool,
    pub gc: bool,
    pub pending_call: bool,
    pub shutdown: bool,
    pub interrupt_main: bool,
}

impl EvalBreakerSnapshot {
    pub fn from_flags(flags: u32, interrupt_main: bool) -> Self {
        Self {
            gil_drop: flags & flag::GIL_DROP_REQUEST != 0,
            signals: flags & flag::PENDING_SIGNALS != 0,
            gc: flags & flag::GC_REQUEST != 0,
            pending_call: flags & flag::PENDING_CALL != 0,
            shutdown: flags & flag::SHUTDOWN_REQUEST != 0,
            interrupt_main,
        }
    }

    pub fn any(&self) -> bool {
        self.gil_drop
            || self.signals
            || self.gc
            || self.pending_call
            || self.shutdown
            || self.interrupt_main
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn breaker_basic_flag_lifecycle() {
        let b = EvalBreaker::new();
        assert!(!b.pending());
        b.set(flag::GIL_DROP_REQUEST);
        assert!(b.pending());
        assert!(b.is_set(flag::GIL_DROP_REQUEST));
        let drained = b.drain();
        assert_eq!(drained, flag::GIL_DROP_REQUEST);
        assert!(!b.pending());
    }

    #[test]
    fn switch_interval_round_trips() {
        let b = EvalBreaker::new();
        b.set_switch_interval(Duration::from_millis(10));
        assert_eq!(b.switch_interval(), Duration::from_millis(10));
    }

    #[test]
    fn waiters_count_increments() {
        let b = EvalBreaker::new();
        assert_eq!(b.waiter_count(), 0);
        b.add_waiter();
        b.add_waiter();
        assert_eq!(b.waiter_count(), 2);
        b.remove_waiter();
        assert_eq!(b.waiter_count(), 1);
    }

    #[test]
    fn gil_acquire_release_basic() {
        let g = Arc::new(GilState::new());
        {
            let _guard = g.acquire();
            assert!(g.holder.load(Ordering::Acquire) != 0);
            assert_eq!(g.depth.load(Ordering::Acquire), 1);
        }
        assert_eq!(g.depth.load(Ordering::Acquire), 0);
    }

    #[test]
    fn pending_calls_drain() {
        let g = Arc::new(GilState::new());
        let counter = Arc::new(AtomicU64::new(0));
        for _ in 0..3 {
            let c = Arc::clone(&counter);
            g.push_pending_call(move || {
                c.fetch_add(1, Ordering::AcqRel);
            });
        }
        g.drain_pending_calls();
        assert_eq!(counter.load(Ordering::Acquire), 3);
        assert_eq!(g.breaker.pending_calls.load(Ordering::Acquire), 0);
    }
}

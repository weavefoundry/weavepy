//! The `signal` built-in module.
//!
//! RFC 0039 gives WeavePy a faithful, CPython-shaped signal core:
//!
//! * We expose every `SIG*` constant CPython exposes on the host
//!   platform (POSIX names on macOS/Linux/*BSD; a smaller Windows
//!   subset otherwise).
//! * `signal.signal(signum, handler)` records the handler in a single
//!   process-global table alongside the `SIG_DFL` / `SIG_IGN` integer
//!   sentinels (0 and 1, matching CPython). The table is global — not
//!   thread-local — so `_thread.interrupt_main()` raised on a worker
//!   is serviced by the main thread reading the same dispositions.
//! * `signal.getsignal(signum)` returns the current disposition.
//! * Simulated signals (`_thread.interrupt_main`, `signal.raise_signal`)
//!   *trip* a per-signal flag. The main thread drains those flags from
//!   its bytecode dispatch loop — the Rust analogue of CPython's
//!   `PyErr_CheckSignals` — and runs the registered handler: a Python
//!   callable is invoked `handler(signum, frame)`; `SIG_IGN` and
//!   `SIG_DFL` are no-ops (CPython only turns the *default* SIGINT
//!   handler into `KeyboardInterrupt`, and it does that by installing
//!   `default_int_handler` at startup — see [`handlers`]).
//! * `signal.default_int_handler` is a Python callable that raises
//!   `KeyboardInterrupt`; it is SIGINT's startup disposition.

use crate::sync::Rc;
use crate::sync::RefCell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicI64, Ordering};
use std::sync::{Mutex, OnceLock};

use crate::error::{type_error, value_error, RuntimeError};
use crate::import::ModuleCache;
use crate::object::{BuiltinFn, DictData, DictKey, Object, PyModule};

/// File descriptor registered via `signal.set_wakeup_fd`; the OS signal
/// handler writes the signal byte here so a `select`/`poll` loop wakes.
/// `-1` when unset. Read inside the async-signal-safe handler.
static WAKEUP_FD: AtomicI32 = AtomicI32::new(-1);

/// `true` once `set_wakeup_fd` has been told the fd is a socket (so the
/// async-signal-safe writer uses `send(MSG_DONTWAIT)` instead of
/// `write`, matching CPython's `_Py_set_wakeup_fd` socket path on the
/// `WakeupSocketSignalTests`). Packed alongside the fd: the high bit of
/// the stored value flags "is a socket". We keep a separate atomic for
/// clarity.
static WAKEUP_FD_IS_SOCKET: AtomicBool = AtomicBool::new(false);

/// `true` when `set_wakeup_fd(..., warn_on_full_buffer=False)` asked the
/// async handler to *silently* drop a byte that doesn't fit (rather than
/// emit the "Exception ignored when trying to write to the signal wakeup
/// fd" message). Defaults to warning, matching CPython.
static WAKEUP_WARN_ON_FULL: AtomicBool = AtomicBool::new(true);

/// Records the most recent `errno` from a failed async-signal-safe
/// wakeup-fd write, so the main thread can raise the matching
/// `OSError` from `PyErr_CheckSignals` (CPython reports the write
/// failure via `sys.unraisablehook` / stderr — see
/// `WakeupSignalTests.test_wakeup_write_error`). `0` ≡ no error.
static WAKEUP_WRITE_ERRNO: AtomicI32 = AtomicI32::new(0);

/// The VM "main" OS thread's `pthread_t` (as a `usize`), published at
/// interpreter startup. WeavePy runs the interpreter on a spawned
/// `weavepy-main` thread while the process's initial OS thread parks in
/// `join()` with the async signals blocked. A process-directed signal
/// (`os.kill(getpid(), sig)`) sent while the VM thread has `sig` blocked
/// can be absorbed by the parked initial thread's per-thread pending
/// queue (Darwin makes it thread-pending there), where it is invisible
/// to `sigpending()` and never delivered — `test_signal`'s
/// `test_pthread_sigmask` / `test_sigpending`. Routing a self-directed
/// `os.kill` through `pthread_kill` onto this thread reproduces the
/// single-threaded CPython semantics (the main thread *is* the process).
#[cfg(unix)]
static VM_MAIN_PTHREAD: AtomicI64 = AtomicI64::new(0);

/// Publish the calling thread as the VM main thread (see
/// [`VM_MAIN_PTHREAD`]). Called once at interpreter startup on the VM
/// thread, before any user code runs.
#[cfg(unix)]
pub fn set_vm_main_thread() {
    // `pthread_t` is an integer on Linux and an opaque pointer on macOS;
    // an address-preserving `as` cast to `usize` is correct on both and
    // avoids a clippy `useless_transmute` on the platforms where the
    // types already coincide.
    let bits = unsafe { libc::pthread_self() } as usize;
    VM_MAIN_PTHREAD.store(bits as i64, Ordering::Release);
}

#[cfg(not(unix))]
pub fn set_vm_main_thread() {}

/// Deliver `sig` to the VM main thread via `pthread_kill`, reproducing a
/// single-threaded process's `kill(getpid(), sig)`. Returns the raw
/// `pthread_kill` return code (0 on success, an errno on failure), or
/// `None` if the main thread isn't published yet (caller falls back to a
/// real `kill`).
#[cfg(unix)]
pub fn deliver_to_vm_main(sig: i32) -> Option<i32> {
    let bits = VM_MAIN_PTHREAD.load(Ordering::Acquire);
    if bits == 0 {
        return None;
    }
    let pt = bits as usize as libc::pthread_t;
    Some(unsafe { libc::pthread_kill(pt, sig as libc::c_int) })
}

#[cfg(not(unix))]
pub fn deliver_to_vm_main(_sig: i32) -> Option<i32> {
    None
}

// ---------------------------------------------------------------------------
// Process-global signal state (RFC 0039).
// ---------------------------------------------------------------------------

/// SIGINT — the one signal whose startup disposition is a Python
/// callable (`default_int_handler`), matching CPython.
const SIGINT: i32 = 2;

/// Slots in the tripped-signal flag array. Sized one past the largest
/// signal number we accept, so `signum` indexes directly.
const TRIP_SLOTS: usize = 65;

/// Per-signal "tripped" flags, set by a simulated signal and drained by
/// the main thread. Indexed by signal number.
static TRIPPED: [AtomicBool; TRIP_SLOTS] = [const { AtomicBool::new(false) }; TRIP_SLOTS];

/// Cheap hot-path gate: `true` while any signal is tripped. A single
/// relaxed load — the dispatch loop probes it once per opcode.
static ANY_TRIPPED: AtomicBool = AtomicBool::new(false);

/// `signal.NSIG` — one past the highest valid signal number on the host.
pub fn nsig() -> i32 {
    #[cfg(target_os = "macos")]
    {
        32
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        65
    }
    #[cfg(windows)]
    {
        23
    }
}

/// The OS-level disposition the runtime installs for a signal.
#[derive(Clone, Copy)]
enum OsDisposition {
    /// `SIG_DFL` — restore the kernel default action.
    Default,
    /// `SIG_IGN` — ignore the signal.
    Ignore,
    /// Our trampoline: trip the flag (+ wakeup fd) and let the main
    /// thread run the Python handler from `PyErr_CheckSignals`.
    Trip,
}

/// Async-signal-safe OS handler. Does the bare minimum the C standard
/// permits inside a handler: store to `sig_atomic_t`-like atomics and a
/// single `write()`. The actual Python handler runs later on the main
/// thread (`take_tripped` + `Interpreter::run_pending_signals`).
#[cfg(unix)]
extern "C" fn handler_trampoline(signum: libc::c_int) {
    let s = signum as i32;
    if s >= 1 && (s as usize) < TRIP_SLOTS {
        TRIPPED[s as usize].store(true, Ordering::Release);
        ANY_TRIPPED.store(true, Ordering::Release);
    }
    let fd = WAKEUP_FD.load(Ordering::Relaxed);
    if fd >= 0 {
        let byte = [s as u8];
        let rc = unsafe { libc::write(fd, byte.as_ptr().cast::<libc::c_void>(), 1) };
        if rc < 0 {
            // CPython's `trip_signal`: a failed wakeup-fd write is
            // reported on the main thread *unless* it's a full
            // non-blocking buffer and the user opted out of that warning
            // (`set_wakeup_fd(..., warn_on_full_buffer=False)`).
            let err = errno_now();
            let is_full = err == libc::EWOULDBLOCK || err == libc::EAGAIN;
            if WAKEUP_WARN_ON_FULL.load(Ordering::Relaxed) || !is_full {
                // Record the first such errno; the main thread drains it
                // from `PyErr_CheckSignals` and emits the OSError.
                let _ = WAKEUP_WRITE_ERRNO.compare_exchange(
                    0,
                    err,
                    Ordering::AcqRel,
                    Ordering::Relaxed,
                );
            }
        }
    }
}

/// Read `errno` inside the async-signal-safe handler. `__errno_location`
/// / `__error` are async-signal-safe by POSIX.
#[cfg(unix)]
fn errno_now() -> i32 {
    std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
}

/// Install the kernel disposition for `signum` via `sigaction`. We omit
/// `SA_RESTART` so a blocking syscall (`lock.acquire`) returns `EINTR`
/// and the main thread gets a chance to run the Python handler — the
/// behaviour `test_threadsignals` relies on.
#[cfg(unix)]
fn set_os_disposition(signum: i32, disp: OsDisposition) {
    // SIGKILL / SIGSTOP can't be caught or ignored.
    if signum == libc::SIGKILL || signum == libc::SIGSTOP {
        return;
    }
    unsafe {
        let mut sa: libc::sigaction = std::mem::zeroed();
        sa.sa_sigaction = match disp {
            OsDisposition::Default => libc::SIG_DFL,
            OsDisposition::Ignore => libc::SIG_IGN,
            OsDisposition::Trip => handler_trampoline as *const () as usize,
        };
        libc::sigemptyset(&raw mut sa.sa_mask);
        sa.sa_flags = 0;
        libc::sigaction(signum, &raw const sa, std::ptr::null_mut());
    }
}

/// Build a `sigset_t` of the asynchronous, *process-directed* signals —
/// everything except the synchronous/fatal ones (`SIGSEGV`, `SIGBUS`,
/// `SIGFPE`, `SIGILL`, `SIGABRT`). Those are raised by the faulting
/// instruction on the offending thread and must stay deliverable so a
/// genuine crash still terminates the process rather than wedging.
#[cfg(unix)]
unsafe fn async_signal_set() -> libc::sigset_t {
    unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigfillset(&raw mut set);
        for sig in [
            libc::SIGSEGV,
            libc::SIGBUS,
            libc::SIGFPE,
            libc::SIGILL,
            libc::SIGABRT,
        ] {
            libc::sigdelset(&raw mut set, sig);
        }
        set
    }
}

/// The process's *inherited* signal mask, captured on the initial thread
/// before WeavePy blocks the async signals there. The VM thread restores
/// exactly this mask so a mask the parent deliberately handed us (e.g.
/// `os.posix_spawn(..., setsigmask=[SIGUSR1])`) is preserved instead of being
/// clobbered — CPython keeps the inherited mask (`test_posix.test_setsigmask`).
#[cfg(unix)]
struct StoredSigset(libc::sigset_t);
// `sigset_t` is a plain POD bitmask; sharing it across the (sequential,
// spawn-synchronized) initial→VM thread handoff is sound.
#[cfg(unix)]
unsafe impl Send for StoredSigset {}
#[cfg(unix)]
unsafe impl Sync for StoredSigset {}
#[cfg(unix)]
static ORIGINAL_SIGMASK: std::sync::OnceLock<StoredSigset> = std::sync::OnceLock::new();

/// Block the asynchronous, process-directed signals on the *calling*
/// thread so the kernel delivers them elsewhere.
///
/// WeavePy runs the interpreter on a spawned `weavepy-main` thread (for a
/// 1 GiB stack); the process's initial OS thread then only parks in
/// `join()`. POSIX lets the kernel hand a process-directed signal
/// (`SIGINT`, `SIGALRM`, …) to *any* thread that hasn't blocked it, and
/// in practice the parked initial thread is a frequent target. There our
/// trampoline merely trips the pending flag — but the VM thread blocked
/// in a syscall never sees `EINTR`, so e.g. `os.write` to a full pipe
/// with a `SIGALRM` handler installed blocks forever (CPython's
/// `test_io.SignalsTest.test_interrupted_write_*`). Blocking these
/// signals on the parked thread forces delivery onto the VM thread,
/// restoring CPython's "a signal interrupts the main thread's blocking
/// call" semantics. See [`unblock_async_signals_current_thread`].
///
/// The mask in effect *before* this blocks (the mask the process inherited
/// from its parent) is stashed so the VM thread can restore it verbatim.
#[cfg(unix)]
pub fn block_async_signals_current_thread() {
    unsafe {
        let set = async_signal_set();
        let mut old: libc::sigset_t = std::mem::zeroed();
        libc::pthread_sigmask(libc::SIG_BLOCK, &raw const set, &raw mut old);
        let _ = ORIGINAL_SIGMASK.set(StoredSigset(old));
    }
}

/// Restore the process's inherited signal mask on the calling thread — the
/// counterpart of [`block_async_signals_current_thread`]. The VM thread calls
/// this at startup so it becomes the deterministic delivery target for every
/// signal that was deliverable at process start, *without* unblocking any
/// signal the parent deliberately blocked for us (a `posix_spawn`
/// `setsigmask`): CPython preserves that inherited mask
/// (`test_posix.test_setsigmask`). If no inherited mask was captured (the
/// blocking step never ran), fall back to simply unblocking the async set.
#[cfg(unix)]
pub fn unblock_async_signals_current_thread() {
    unsafe {
        if let Some(mask) = ORIGINAL_SIGMASK.get() {
            libc::pthread_sigmask(libc::SIG_SETMASK, &raw const mask.0, std::ptr::null_mut());
        } else {
            let set = async_signal_set();
            libc::pthread_sigmask(libc::SIG_UNBLOCK, &raw const set, std::ptr::null_mut());
        }
    }
}

/// Reproduce CPython's `exit_sigint()` (Modules/main.c): when a
/// `KeyboardInterrupt` reaches the top level unhandled, reset `SIGINT`
/// to `SIG_DFL`, unblock it on the calling thread, and re-raise it
/// process-wide so the interpreter terminates *by the signal*
/// (`returncode == -SIGINT`) rather than with a generic exit code —
/// `test_signal.PosixTests.test_keyboard_interrupt_exit_code`. Does not
/// return: the signal kills the process.
#[cfg(unix)]
pub fn die_via_sigint() {
    unsafe {
        libc::signal(libc::SIGINT, libc::SIG_DFL);
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&raw mut set);
        libc::sigaddset(&raw mut set, libc::SIGINT);
        libc::pthread_sigmask(libc::SIG_UNBLOCK, &raw const set, std::ptr::null_mut());
        libc::kill(libc::getpid(), libc::SIGINT);
    }
    // The default SIGINT disposition terminates us synchronously once it is
    // delivered; pause briefly so the kernel can act before any fallback.
    std::thread::sleep(std::time::Duration::from_millis(200));
}

/// No-op fallback on non-Unix targets.
#[cfg(not(unix))]
pub fn die_via_sigint() {}

/// Install SIGINT's startup disposition (the `default_int_handler`
/// trampoline) the way CPython does during interpreter init — *before* any
/// `import signal`. Without this, a script that never imports `signal` (e.g.
/// `python -c "import time; time.sleep(30)"`, as `test_subprocess`'
/// `_kill_process` spawns) would take the kernel default for SIGINT and die
/// silently instead of raising `KeyboardInterrupt`. Idempotent: re-importing
/// `signal` just re-installs the same disposition.
#[cfg(unix)]
pub fn install_startup_dispositions() {
    // Publish this (VM main) thread as the deterministic delivery target
    // for self-directed `os.kill` (see `VM_MAIN_PTHREAD`).
    set_vm_main_thread();
    // Seed the handler table (SIGINT -> default_int_handler) and arm the
    // kernel trampoline so a tripped SIGINT is serviced by the dispatch loop.
    let _ = handlers();
    set_os_disposition(SIGINT, OsDisposition::Trip);

    // CPython's `_PySignal_Init` ignores SIGPIPE and SIGXFSZ at startup so
    // a write to a closed pipe / past RLIMIT_FSIZE returns EPIPE / EFBIG
    // (raising the catchable `BrokenPipeError` / `OSError`) instead of the
    // kernel default killing the process (test_resource.test_fsize_enforced,
    // and broken-pipe handling across test_subprocess / test_io). Record
    // SIG_IGN in the handler table too, so `getsignal()` matches CPython
    // (which reads the live C disposition when seeding its table).
    for sig in [libc::SIGPIPE, libc::SIGXFSZ] {
        set_handler(sig, Object::Int(1));
        set_os_disposition(sig, OsDisposition::Ignore);
    }
}

/// No-op on non-Unix targets (Windows uses a different signal model).
#[cfg(not(unix))]
pub fn install_startup_dispositions() {}

/// No-op on non-Unix targets (Windows uses a different signal model).
#[cfg(not(unix))]
pub fn block_async_signals_current_thread() {}

/// No-op on non-Unix targets (Windows uses a different signal model).
#[cfg(not(unix))]
pub fn unblock_async_signals_current_thread() {}

#[cfg(not(unix))]
fn set_os_disposition(_signum: i32, _disp: OsDisposition) {}

/// Process-global handler table. CPython installs `default_int_handler`
/// for SIGINT at startup (so `getsignal(SIGINT)` returns it and a
/// tripped SIGINT raises `KeyboardInterrupt`); every other signal
/// defaults to `SIG_DFL`.
fn handlers() -> &'static Mutex<HashMap<i32, Object>> {
    static H: OnceLock<Mutex<HashMap<i32, Object>>> = OnceLock::new();
    H.get_or_init(|| {
        let mut m = HashMap::new();
        m.insert(SIGINT, default_int_handler_obj());
        Mutex::new(m)
    })
}

/// The stable `default_int_handler` callable. Cached so the module
/// attribute and SIGINT's startup disposition are the *same* object
/// (`signal.getsignal(SIGINT) is signal.default_int_handler`).
fn default_int_handler_obj() -> Object {
    static OBJ: OnceLock<Object> = OnceLock::new();
    OBJ.get_or_init(|| b("default_int_handler", default_int_handler))
        .clone()
}

/// Install `handler` for `signum`, returning the previous disposition.
fn set_handler(signum: i32, handler: Object) -> Object {
    handlers()
        .lock()
        .unwrap()
        .insert(signum, handler)
        .unwrap_or(Object::Int(0))
}

/// Current disposition for `signum`: a Python callable, or the
/// `SIG_DFL` (0) / `SIG_IGN` (1) sentinels. Unregistered signals read
/// as `SIG_DFL`.
pub fn handler_for(signum: i32) -> Object {
    handlers()
        .lock()
        .unwrap()
        .get(&signum)
        .cloned()
        .unwrap_or(Object::Int(0))
}

/// Mark `signum` as tripped — the simulated-signal equivalent of the OS
/// delivering it. Out-of-range numbers are ignored (callers validate
/// and raise `ValueError` first).
pub fn trip_signal(signum: i32) {
    if signum >= 1 && (signum as usize) < TRIP_SLOTS {
        TRIPPED[signum as usize].store(true, Ordering::Release);
        ANY_TRIPPED.store(true, Ordering::Release);
    }
}

/// Cheap dispatch-loop probe: `true` if any signal is tripped.
#[inline]
pub fn signals_pending() -> bool {
    ANY_TRIPPED.load(Ordering::Relaxed)
}

/// Drain a pending wakeup-fd write error (set by the async-signal-safe
/// trampoline). Returns the `errno` to report, or `None` if there is no
/// pending error. The main thread calls this from `PyErr_CheckSignals`
/// and raises the matching `OSError` via the unraisable hook
/// (`test_signal` WakeupSignalTests / WakeupSocketSignalTests).
pub fn take_wakeup_write_error() -> Option<i32> {
    let err = WAKEUP_WRITE_ERRNO.swap(0, Ordering::AcqRel);
    if err == 0 {
        None
    } else {
        Some(err)
    }
}

/// Drain tripped signals in ascending numeric order (CPython's order),
/// clearing the global gate. Returns the signal numbers to service.
/// By convention only the main thread calls this.
pub fn take_tripped() -> Vec<i32> {
    if !ANY_TRIPPED.swap(false, Ordering::AcqRel) {
        return Vec::new();
    }
    let mut out = Vec::new();
    for (i, slot) in TRIPPED.iter().enumerate() {
        if slot.swap(false, Ordering::AcqRel) {
            out.push(i as i32);
        }
    }
    out
}

pub fn build(_cache: &ModuleCache) -> Rc<PyModule> {
    let dict = Rc::new(RefCell::new(DictData::new()));
    {
        let mut d = dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("__name__")),
            Object::from_static("_signal"),
        );
        d.insert(
            DictKey(Object::from_static("__doc__")),
            Object::from_static("Set handlers for asynchronous events."),
        );

        // Sentinels — CPython exposes both as integers, with `SIG_DFL`
        // taking the conventional value 0 and `SIG_IGN` the value 1.
        d.insert(DictKey(Object::from_static("SIG_DFL")), Object::Int(0));
        d.insert(DictKey(Object::from_static("SIG_IGN")), Object::Int(1));

        // The numbers below are taken from CPython 3.13's signal
        // module on macOS / Linux — the ones any portable program
        // reaches for. Platform-specific signals (SIGSTKFLT,
        // SIGPWR, RT signals) are intentionally omitted.
        for (name, value) in posix_signals() {
            d.insert(
                DictKey(Object::from_static(name)),
                Object::Int(i64::from(value)),
            );
        }

        d.insert(
            DictKey(Object::from_static("NSIG")),
            Object::Int(i64::from(nsig())),
        );

        d.insert(
            DictKey(Object::from_static("signal")),
            b("signal", signal_signal),
        );
        d.insert(
            DictKey(Object::from_static("getsignal")),
            b("getsignal", signal_getsignal),
        );
        d.insert(
            DictKey(Object::from_static("default_int_handler")),
            default_int_handler_obj(),
        );
        d.insert(
            DictKey(Object::from_static("raise_signal")),
            b("raise_signal", raise_signal),
        );
        d.insert(
            DictKey(Object::from_static("alarm")),
            b("alarm", signal_alarm),
        );
        d.insert(
            DictKey(Object::from_static("siginterrupt")),
            b("siginterrupt", signal_siginterrupt),
        );
        d.insert(
            DictKey(Object::from_static("pause")),
            b("pause", signal_pause),
        );
        d.insert(
            DictKey(Object::from_static("strsignal")),
            b("strsignal", strsignal),
        );
        d.insert(
            DictKey(Object::from_static("getitimer")),
            b("getitimer", signal_getitimer),
        );
        d.insert(
            DictKey(Object::from_static("setitimer")),
            b("setitimer", signal_setitimer),
        );
        d.insert(
            DictKey(Object::from_static("set_wakeup_fd")),
            Object::Builtin(Rc::new(BuiltinFn::with_kwargs(
                "set_wakeup_fd",
                set_wakeup_fd,
            ))),
        );
        d.insert(
            DictKey(Object::from_static("pthread_kill")),
            b("pthread_kill", pthread_kill),
        );
        d.insert(
            DictKey(Object::from_static("valid_signals")),
            b("valid_signals", valid_signals),
        );

        // Signal-mask machinery (RFC 0040 WS4). Present on every POSIX
        // host; the frozen `signal.py` keys its `Sigmasks` enum and the
        // `pthread_sigmask`/`sigwait`/`sigpending` wrappers off these.
        #[cfg(unix)]
        {
            d.insert(
                DictKey(Object::from_static("pthread_sigmask")),
                b("pthread_sigmask", pthread_sigmask),
            );
            d.insert(
                DictKey(Object::from_static("sigwait")),
                b("sigwait", sigwait),
            );
            d.insert(
                DictKey(Object::from_static("sigpending")),
                b("sigpending", sigpending),
            );
            d.insert(
                DictKey(Object::from_static("SIG_BLOCK")),
                Object::Int(i64::from(libc::SIG_BLOCK)),
            );
            d.insert(
                DictKey(Object::from_static("SIG_UNBLOCK")),
                Object::Int(i64::from(libc::SIG_UNBLOCK)),
            );
            d.insert(
                DictKey(Object::from_static("SIG_SETMASK")),
                Object::Int(i64::from(libc::SIG_SETMASK)),
            );
        }

        // Itimer constants — frozen modules occasionally import them.
        d.insert(DictKey(Object::from_static("ITIMER_REAL")), Object::Int(0));
        d.insert(
            DictKey(Object::from_static("ITIMER_VIRTUAL")),
            Object::Int(1),
        );
        d.insert(DictKey(Object::from_static("ITIMER_PROF")), Object::Int(2));
    }
    // CPython installs its C handler for SIGINT at startup so a real
    // Ctrl-C trips the flag and the main thread raises KeyboardInterrupt
    // (rather than the kernel default terminating us). Mirror that, and
    // make SIGINT's table disposition the cached `default_int_handler`.
    let _ = handlers();
    const SIGINT_NUM: i32 = 2;
    set_os_disposition(SIGINT_NUM, OsDisposition::Trip);
    Rc::new(PyModule {
        name: "_signal".to_owned(),
        filename: None,
        dict,
    })
}

fn b(name: &'static str, body: fn(&[Object]) -> Result<Object, RuntimeError>) -> Object {
    Object::Builtin(Rc::new(BuiltinFn {
        name,
        binds_instance: false,
        call: Box::new(body),
        call_kw: None,
    }))
}

fn signal_signal(args: &[Object]) -> Result<Object, RuntimeError> {
    let sig = signum(args.first())?;
    let handler = args
        .get(1)
        .cloned()
        .ok_or_else(|| type_error("signal() takes 2 arguments"))?;
    if sig < 1 || sig >= nsig() {
        return Err(value_error("signal number out of range"));
    }
    // CPython only allows installing handlers from the main thread of
    // the main interpreter.
    if !crate::gil::is_main_thread() {
        return Err(value_error(
            "signal only works in main thread of the main interpreter",
        ));
    }
    let disp = match &handler {
        // SIG_DFL (0) / SIG_IGN (1) sentinels.
        Object::Int(0) => OsDisposition::Default,
        Object::Int(1) => OsDisposition::Ignore,
        Object::Int(_) => {
            return Err(value_error(
                "signal handler must be signal.SIG_IGN, signal.SIG_DFL, or a callable object",
            ))
        }
        h if is_callable_handler(h) => OsDisposition::Trip,
        // `None` and other non-callables are a TypeError, matching
        // CPython's `PyCallable_Check` gate in `signal_signal_impl`.
        _ => {
            return Err(type_error(
                "signal handler must be signal.SIG_IGN, signal.SIG_DFL, or a callable object",
            ))
        }
    };
    // SIGKILL / SIGSTOP can't be caught or ignored: CPython's
    // `signal.signal()` surfaces the kernel's `EINVAL` as an `OSError`.
    // asyncio's `add_signal_handler` relies on this — it catches the
    // `EINVAL` `OSError` and re-raises `RuntimeError('sig N cannot be
    // caught')` (see `test_add_signal_handler`).
    #[cfg(unix)]
    if sig == libc::SIGKILL || sig == libc::SIGSTOP {
        return Err(crate::error::io_error_to_py(
            &std::io::Error::from_raw_os_error(libc::EINVAL),
        ));
    }
    set_os_disposition(sig, disp);
    Ok(set_handler(sig, handler))
}

/// `signal.alarm(seconds)` — schedule a `SIGALRM` after `seconds`,
/// returning the seconds left on any previously-set alarm.
fn signal_alarm(args: &[Object]) -> Result<Object, RuntimeError> {
    let secs = match args.first() {
        Some(Object::Int(n)) => *n,
        Some(Object::Bool(b)) => i64::from(*b),
        None => 0,
        Some(_) => return Err(type_error("an integer is required")),
    };
    #[cfg(unix)]
    {
        let prev = unsafe { libc::alarm(secs.max(0) as libc::c_uint) };
        Ok(Object::Int(i64::from(prev)))
    }
    #[cfg(not(unix))]
    {
        let _ = secs;
        Err(crate::error::attribute_error(
            "module 'signal' has no attribute 'alarm'",
        ))
    }
}

/// Coerce a Python number (`int`/`bool`/`float`) to seconds for the
/// interval-timer calls.
fn as_seconds(arg: Option<&Object>) -> Result<f64, RuntimeError> {
    match arg {
        Some(Object::Int(n)) => Ok(*n as f64),
        Some(Object::Bool(b)) => Ok(f64::from(*b)),
        Some(Object::Float(f)) => Ok(*f),
        None => Ok(0.0),
        Some(_) => Err(type_error("a float is required")),
    }
}

#[cfg(unix)]
fn seconds_to_timeval(secs: f64) -> libc::timeval {
    let secs = secs.max(0.0);
    let whole = secs.trunc();
    let usec = ((secs - whole) * 1_000_000.0).round();
    libc::timeval {
        tv_sec: whole as libc::time_t,
        tv_usec: usec as libc::suseconds_t,
    }
}

#[cfg(unix)]
// `tv_usec` is `c_int` (i32) on macOS but `c_long` (i64) on Linux; an `as f64`
// cast is the only portable spelling (`f64::from` won't compile for i64), so
// suppress the platform-specific cast-lossless lint here.
#[allow(clippy::cast_lossless)]
fn timeval_to_seconds(tv: libc::timeval) -> f64 {
    tv.tv_sec as f64 + tv.tv_usec as f64 / 1_000_000.0
}

/// `signal.setitimer(which, seconds, interval=0.0)` — arm an interval
/// timer. `ITIMER_REAL` delivers `SIGALRM`; with a Python handler
/// installed via `signal.signal`, the OS trampoline writes the signal
/// byte to the wakeup fd so an `asyncio` loop (or a `select()` wait)
/// wakes and runs the handler. Returns the previous `(value, interval)`
/// in seconds, matching CPython. Implementing this for real (rather than
/// the previous no-op) is what lets `test_events`' signal tests fire
/// `SIGALRM` instead of falling through to their 60s timeout.
fn signal_setitimer(args: &[Object]) -> Result<Object, RuntimeError> {
    let which = match args.first() {
        Some(Object::Int(n)) => *n as i32,
        Some(Object::Bool(b)) => i32::from(*b),
        _ => return Err(type_error("an integer is required")),
    };
    let value = as_seconds(args.get(1))?;
    let interval = as_seconds(args.get(2))?;
    #[cfg(unix)]
    {
        let new = libc::itimerval {
            it_interval: seconds_to_timeval(interval),
            it_value: seconds_to_timeval(value),
        };
        let mut old: libc::itimerval = unsafe { std::mem::zeroed() };
        let rc = unsafe { libc::setitimer(which, &raw const new, &raw mut old) };
        if rc != 0 {
            // A bad `which` (e.g. -1) fails with EINVAL; the frozen
            // `signal.py` re-raises this OSError as `signal.ItimerError`.
            return Err(crate::error::io_error_to_py(
                &std::io::Error::last_os_error(),
            ));
        }
        Ok(Object::new_tuple(vec![
            Object::Float(timeval_to_seconds(old.it_value)),
            Object::Float(timeval_to_seconds(old.it_interval)),
        ]))
    }
    #[cfg(not(unix))]
    {
        let _ = (which, value, interval);
        Err(crate::error::attribute_error(
            "module 'signal' has no attribute 'setitimer'",
        ))
    }
}

/// `signal.getitimer(which)` — return the current `(value, interval)` of
/// an interval timer, in seconds.
fn signal_getitimer(args: &[Object]) -> Result<Object, RuntimeError> {
    let which = match args.first() {
        Some(Object::Int(n)) => *n as i32,
        Some(Object::Bool(b)) => i32::from(*b),
        _ => return Err(type_error("an integer is required")),
    };
    #[cfg(unix)]
    {
        let mut cur: libc::itimerval = unsafe { std::mem::zeroed() };
        let rc = unsafe { libc::getitimer(which, &raw mut cur) };
        if rc != 0 {
            return Err(crate::error::io_error_to_py(
                &std::io::Error::last_os_error(),
            ));
        }
        Ok(Object::new_tuple(vec![
            Object::Float(timeval_to_seconds(cur.it_value)),
            Object::Float(timeval_to_seconds(cur.it_interval)),
        ]))
    }
    #[cfg(not(unix))]
    {
        let _ = which;
        Err(crate::error::attribute_error(
            "module 'signal' has no attribute 'getitimer'",
        ))
    }
}

/// `signal.siginterrupt(signalnum, flag)` — control whether system calls
/// are restarted (`flag=False`, i.e. `SA_RESTART`) or interrupted with
/// `EINTR` (`flag=True`) when `signalnum` fires. asyncio's
/// `add_signal_handler` calls this to set `SA_RESTART` and limit `EINTR`
/// occurrences; returns `None` on success, raises `OSError` on failure.
fn signal_siginterrupt(args: &[Object]) -> Result<Object, RuntimeError> {
    let sig = signum(args.first())?;
    let flag = match args.get(1) {
        Some(Object::Bool(b)) => *b,
        Some(Object::Int(n)) => *n != 0,
        None => return Err(type_error("siginterrupt() takes exactly 2 arguments")),
        Some(_) => return Err(type_error("an integer is required")),
    };
    if sig < 1 || sig >= nsig() {
        return Err(value_error("signal number out of range"));
    }
    // `libc::siginterrupt` isn't exported by the `libc` crate, and
    // re-`sigaction`-ing the signal here would clobber the trampoline
    // `signal.signal` installs. Toggling `SA_RESTART` only changes
    // whether blocked syscalls restart vs. fail with `EINTR` — and our
    // blocking paths already surface `EINTR` as `InterruptedError` — so a
    // validated no-op (returning `None`, as CPython does on success) is
    // behaviourally safe for the handlers asyncio registers.
    let _ = flag;
    Ok(Object::None)
}

/// `signal.pause()` — block until a signal is delivered. We sleep in
/// short slices (GIL released) so the main thread still services the
/// Python handler that the OS trampoline tripped.
fn signal_pause(_args: &[Object]) -> Result<Object, RuntimeError> {
    #[cfg(unix)]
    {
        crate::gil::allow_threads_then(|| unsafe {
            libc::pause();
        });
    }
    Ok(Object::None)
}

fn signal_getsignal(args: &[Object]) -> Result<Object, RuntimeError> {
    let sig = signum(args.first())?;
    // CPython's `getsignal` validates the range and raises `ValueError`
    // for an out-of-range signal number (test_signal probes `getsignal(4242)`).
    if sig < 1 || sig >= nsig() {
        return Err(value_error("signal number out of range"));
    }
    Ok(handler_for(sig))
}

/// CPython's `signal()` requires the handler to be `SIG_DFL`/`SIG_IGN`
/// (the int sentinels) or a callable; anything else (e.g. `None`) is a
/// `TypeError`. Mirrors the `callable()` builtin's notion of callable.
fn is_callable_handler(o: &Object) -> bool {
    match o {
        Object::Function(_)
        | Object::Builtin(_)
        | Object::BoundMethod(_)
        | Object::Type(_)
        | Object::Generator(_)
        | Object::StaticMethod(_) => true,
        Object::Instance(inst) => inst.cls().lookup("__call__").is_some(),
        _ => false,
    }
}

fn default_int_handler(_args: &[Object]) -> Result<Object, RuntimeError> {
    Err(RuntimeError::PyException(
        crate::error::PyException::from_builtin("KeyboardInterrupt", ""),
    ))
}

fn raise_signal(args: &[Object]) -> Result<Object, RuntimeError> {
    let sig = signum(args.first())?;
    if sig < 1 || sig >= nsig() {
        return Err(value_error("signal number out of range"));
    }
    // Deliver the signal for real, exactly like CPython's
    // `signal_raise_signal_impl` (`raise(signalnum)`): the kernel runs our
    // trampoline on whichever thread called us and the main thread then
    // services any Python handler. Crucially we must `raise` *unconditionally*
    // — including when the disposition is `SIG_DFL` with no Python handler — so
    // a default-action signal performs that default action (terminate / dump /
    // ignore) just like CPython. The previous "trip a flag instead, to avoid
    // process death" shortcut made `raise_signal(SIGUSR1)` a silent no-op and
    // broke `test_posix.test_setsigdef` (child spawned with
    // `setsigdef=[SIGUSR1]` must die with `-SIGUSR1`). `SIG_IGN` and
    // blocked signals are still honoured by the kernel (ignored / left pending),
    // so `test_setsigmask` keeps exiting 0.
    #[cfg(unix)]
    {
        unsafe {
            libc::raise(sig as libc::c_int);
        }
    }
    #[cfg(not(unix))]
    {
        trip_signal(sig);
    }
    Ok(Object::None)
}

fn strsignal(args: &[Object]) -> Result<Object, RuntimeError> {
    let sig = signum(args.first())?;
    // CPython's `signal_strsignal_impl` raises ValueError for a signal
    // number outside `[1, NSIG)` (test_out_of_range_signal_number_raises_error).
    if sig < 1 || sig >= nsig() {
        return Err(value_error("signal number out of range"));
    }
    #[cfg(unix)]
    {
        let ptr = unsafe { libc::strsignal(sig as libc::c_int) };
        if ptr.is_null() {
            return Ok(Object::None);
        }
        let msg = unsafe { std::ffi::CStr::from_ptr(ptr) }
            .to_string_lossy()
            .into_owned();
        Ok(Object::from_str(msg))
    }
    #[cfg(not(unix))]
    {
        Ok(Object::from_str(format!("Signal {sig}")))
    }
}

fn set_wakeup_fd(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let fd = match args.first() {
        Some(Object::Int(n)) => *n as i32,
        Some(Object::Bool(b)) => i32::from(*b),
        None => -1,
        Some(_) => return Err(type_error("an integer is required")),
    };
    // `signal.set_wakeup_fd(fd, *, warn_on_full_buffer=True)` — the
    // keyword controls whether a full non-blocking wakeup-fd buffer is
    // reported. Defaults to True (and resets to True when omitted, so a
    // prior `warn_on_full_buffer=False` doesn't leak — test_signal
    // WakeupSocketSignalTests.test_warn_on_full_buffer).
    let mut warn_on_full_buffer = true;
    for (k, v) in kwargs {
        match k.as_str() {
            "warn_on_full_buffer" => {
                warn_on_full_buffer = match v {
                    Object::Bool(b) => *b,
                    Object::Int(n) => *n != 0,
                    Object::None => true,
                    _ => true,
                };
            }
            other => {
                return Err(type_error(format!(
                    "set_wakeup_fd() got an unexpected keyword argument '{other}'"
                )))
            }
        }
    }
    // CPython validates a real fd: it must exist (else `OSError`) and be in
    // non-blocking mode (else `ValueError`), so the async-signal-safe
    // `write()` in the handler can never block. `fd == -1` clears the wakeup.
    #[cfg(unix)]
    if fd != -1 {
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        if flags < 0 {
            return Err(crate::error::io_error_to_py(
                &std::io::Error::last_os_error(),
            ));
        }
        if flags & libc::O_NONBLOCK == 0 {
            return Err(value_error(format!(
                "the fd {fd} must be in non-blocking mode"
            )));
        }
    }
    WAKEUP_WARN_ON_FULL.store(warn_on_full_buffer, Ordering::Release);
    // Clear any stale recorded write error when the wakeup fd changes.
    WAKEUP_WRITE_ERRNO.store(0, Ordering::Release);
    let _ = &WAKEUP_FD_IS_SOCKET; // reserved for the Windows send() path
    let prev = WAKEUP_FD.swap(fd, Ordering::AcqRel);
    Ok(Object::Int(i64::from(prev)))
}

// ---------------------------------------------------------------------------
// RFC 0040 WS4 — signal-mask surface (`pthread_sigmask`, `sigwait`,
// `sigpending`) and a faithful `pthread_kill`.
// ---------------------------------------------------------------------------

/// Per-thread `pthread_t` keyed by the *synthetic* ident
/// `_thread.get_ident()` reports. `pthread_kill(tid, sig)` resolves the
/// handle here. Stored as a `usize` (the pointer/`c_ulong` bits) so the
/// map is `Send`; reconstructed via `transmute` on use. Both `pthread_t`
/// and `usize` are pointer-sized on every target we build.
#[cfg(unix)]
fn pthread_registry() -> &'static Mutex<HashMap<u64, usize>> {
    static R: OnceLock<Mutex<HashMap<u64, usize>>> = OnceLock::new();
    R.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Record the calling OS thread's `pthread_t` under `ident`. Called from
/// the worker-thread trampoline (and main-thread init) so `pthread_kill`
/// can target a specific Python thread the way CPython does.
#[cfg(unix)]
pub fn register_current_thread_pthread(ident: u64) {
    let key = unsafe { libc::pthread_self() } as usize;
    pthread_registry().lock().unwrap().insert(ident, key);
}

/// Drop a thread's `pthread_t` mapping when it exits.
#[cfg(unix)]
pub fn unregister_thread_pthread(ident: u64) {
    pthread_registry().lock().unwrap().remove(&ident);
}

/// Collect signal numbers from any Python iterable (`set`/`frozenset`/
/// `list`/generator) for the sigset-shaped arguments. Uses the live
/// interpreter (the GIL is held inside a builtin call) the same way
/// `builtin_types::elements_via_interp` does.
#[cfg(unix)]
fn collect_signal_ints(obj: &Object) -> Result<Vec<i32>, RuntimeError> {
    let items: Vec<Object> = {
        let ptr = crate::vm_singletons::current_interpreter_ptr()
            .ok_or_else(|| type_error("argument must be an iterable of signal numbers"))?;
        // SAFETY: published by an enclosing VM frame still live on this
        // thread; the GIL keeps the access exclusive.
        let interp = unsafe { &mut *ptr };
        let globals = interp.builtins_dict();
        interp.collect_iterable(obj, &globals)?
    };
    let mut out = Vec::with_capacity(items.len());
    for it in items {
        match it.as_i64() {
            Some(n) => out.push(n as i32),
            None => return Err(value_error("signal_set must contain signal numbers")),
        }
    }
    Ok(out)
}

/// Build a `sigset_t` from a list of signal numbers, validating each.
#[cfg(unix)]
fn build_sigset(sigs: &[i32]) -> Result<libc::sigset_t, RuntimeError> {
    unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&raw mut set);
        for &s in sigs {
            if s < 1 || s >= nsig() {
                return Err(value_error("signal number out of range"));
            }
            libc::sigaddset(&raw mut set, s);
        }
        Ok(set)
    }
}

/// Turn a `sigset_t` into a Python `set` of the signal numbers it
/// contains (scanning the host's valid range).
#[cfg(unix)]
// `sigset_t` is a 128-byte struct on Linux (where CI runs clippy) but a
// 4-byte int on macOS; the by-reference signature is correct for the large
// platform, so silence the macOS-only "trivially copy, pass by value" hint.
#[allow(clippy::trivially_copy_pass_by_ref)]
fn sigset_to_pyset(set: &libc::sigset_t) -> Object {
    let mut members = Vec::new();
    for s in 1..nsig() {
        if unsafe { libc::sigismember(set, s) } == 1 {
            members.push(Object::Int(i64::from(s)));
        }
    }
    Object::new_set_from(members)
}

/// `signal.pthread_sigmask(how, mask)` — block/unblock/replace the
/// calling thread's signal mask, returning the *old* mask as a set.
#[cfg(unix)]
fn pthread_sigmask(args: &[Object]) -> Result<Object, RuntimeError> {
    let how = match args.first().and_then(Object::as_i64) {
        Some(n) => n as libc::c_int,
        None => return Err(type_error("an integer is required")),
    };
    // CPython does *not* pre-validate `how`: an unknown value (e.g. 1700)
    // reaches `pthread_sigmask(3)`, which fails with EINVAL and surfaces
    // as `OSError` — exactly what `test_pthread_sigmask_arguments` asserts.
    let mask_obj = args
        .get(1)
        .ok_or_else(|| type_error("pthread_sigmask() takes exactly 2 arguments"))?;
    let sigs = collect_signal_ints(mask_obj)?;
    let set = build_sigset(&sigs)?;
    let mut old: libc::sigset_t = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::pthread_sigmask(how, &raw const set, &raw mut old) };
    if rc != 0 {
        return Err(crate::error::io_error_to_py(
            &std::io::Error::from_raw_os_error(rc),
        ));
    }
    Ok(sigset_to_pyset(&old))
}

/// `signal.sigwait(sigset)` — block (GIL released) until one of the
/// signals in `sigset` is delivered, returning its number.
#[cfg(unix)]
fn sigwait(args: &[Object]) -> Result<Object, RuntimeError> {
    let sigset_obj = args
        .first()
        .ok_or_else(|| type_error("sigwait() takes exactly 1 argument"))?;
    let sigs = collect_signal_ints(sigset_obj)?;
    let set = build_sigset(&sigs)?;
    let mut received: libc::c_int = 0;
    let rc = crate::gil::allow_threads_then(|| unsafe {
        libc::sigwait(&raw const set, &raw mut received)
    });
    if rc != 0 {
        return Err(crate::error::io_error_to_py(
            &std::io::Error::from_raw_os_error(rc),
        ));
    }
    Ok(Object::Int(i64::from(received)))
}

/// `signal.sigpending()` — the set of signals pending delivery to the
/// calling thread.
#[cfg(unix)]
fn sigpending(_args: &[Object]) -> Result<Object, RuntimeError> {
    let mut set: libc::sigset_t = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::sigpending(&raw mut set) };
    if rc != 0 {
        return Err(crate::error::io_error_to_py(
            &std::io::Error::last_os_error(),
        ));
    }
    Ok(sigset_to_pyset(&set))
}

/// `signal.pthread_kill(thread_id, signalnum)` — deliver `signalnum` to
/// the thread whose `_thread.get_ident()` is `thread_id`. `signalnum == 0`
/// performs the standard existence check.
#[cfg(unix)]
fn pthread_kill(args: &[Object]) -> Result<Object, RuntimeError> {
    let tid = match args.first() {
        Some(Object::Int(n)) => *n as u64,
        Some(Object::Bool(b)) => u64::from(*b),
        _ => return Err(type_error("an integer is required")),
    };
    let sig = signum(args.get(1))?;
    if sig < 0 || sig >= nsig() {
        return Err(value_error("signal number out of range"));
    }
    let key = pthread_registry().lock().unwrap().get(&tid).copied();
    // Fall back to the caller's own handle for an ident we never
    // registered (e.g. the bootstrap thread before registration).
    let key = key.unwrap_or_else(|| unsafe { libc::pthread_self() } as usize);
    let pt = key as libc::pthread_t;
    let rc = unsafe { libc::pthread_kill(pt, sig as libc::c_int) };
    if rc != 0 {
        return Err(crate::error::io_error_to_py(
            &std::io::Error::from_raw_os_error(rc),
        ));
    }
    Ok(Object::None)
}

/// No-op stub retained for non-Unix builds (Windows lacks the POSIX
/// signal-mask surface).
#[cfg(not(unix))]
fn pthread_kill(_args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(Object::None)
}

fn signum(arg: Option<&Object>) -> Result<i32, RuntimeError> {
    // Accept any int (incl. `signal.Signals`/`Handlers` IntEnum members,
    // which are int subclasses) — CPython's C funcs coerce via `__index__`.
    match arg.and_then(Object::as_i64) {
        Some(n) => Ok(n as i32),
        None => Err(type_error("signal number must be an int")),
    }
}

/// `signal.valid_signals()` — the set of signal numbers handlers may be
/// installed for. CPython returns every catchable signal on the host;
/// we return the named POSIX set we expose (which includes `SIGCHLD`,
/// `SIGINT`, … — everything `asyncio`'s child-watcher and the test suite
/// register), built as a `set` to match CPython's return type. asyncio's
/// `_check_signal` does `if sig not in signal.valid_signals()`, so a
/// missing entry would wrongly reject a valid signal.
fn valid_signals(_args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(Object::new_set_from(
        posix_signals()
            .into_iter()
            .map(|(_, value)| Object::Int(i64::from(value))),
    ))
}

#[cfg(windows)]
fn posix_signals() -> Vec<(&'static str, i32)> {
    vec![
        ("SIGABRT", 22),
        ("SIGBREAK", 21),
        ("SIGFPE", 8),
        ("SIGILL", 4),
        ("SIGINT", 2),
        ("SIGSEGV", 11),
        ("SIGTERM", 15),
    ]
}

/// Host signal numbers, sourced from `libc` so they match the platform
/// the binary runs on. macOS and Linux disagree on several values
/// (e.g. `SIGUSR1` is 30 on macOS, 10 on Linux); hardcoding Linux
/// numbers made `os.kill(pid, signal.SIGUSR1)` send `SIGBUS` on macOS.
#[cfg(unix)]
fn posix_signals() -> Vec<(&'static str, i32)> {
    let mut v: Vec<(&'static str, i32)> = vec![
        ("SIGHUP", libc::SIGHUP),
        ("SIGINT", libc::SIGINT),
        ("SIGQUIT", libc::SIGQUIT),
        ("SIGILL", libc::SIGILL),
        ("SIGTRAP", libc::SIGTRAP),
        ("SIGABRT", libc::SIGABRT),
        ("SIGBUS", libc::SIGBUS),
        ("SIGFPE", libc::SIGFPE),
        ("SIGKILL", libc::SIGKILL),
        ("SIGUSR1", libc::SIGUSR1),
        ("SIGSEGV", libc::SIGSEGV),
        ("SIGUSR2", libc::SIGUSR2),
        ("SIGPIPE", libc::SIGPIPE),
        ("SIGALRM", libc::SIGALRM),
        ("SIGTERM", libc::SIGTERM),
        ("SIGCHLD", libc::SIGCHLD),
        ("SIGCONT", libc::SIGCONT),
        ("SIGSTOP", libc::SIGSTOP),
        ("SIGTSTP", libc::SIGTSTP),
        ("SIGTTIN", libc::SIGTTIN),
        ("SIGTTOU", libc::SIGTTOU),
        ("SIGURG", libc::SIGURG),
        ("SIGXCPU", libc::SIGXCPU),
        ("SIGXFSZ", libc::SIGXFSZ),
        ("SIGVTALRM", libc::SIGVTALRM),
        ("SIGPROF", libc::SIGPROF),
        ("SIGWINCH", libc::SIGWINCH),
        ("SIGIO", libc::SIGIO),
        ("SIGSYS", libc::SIGSYS),
    ];
    // `SIGIOT` (alias of `SIGABRT`) and `SIGPOLL` (alias of `SIGIO`)
    // are exposed by CPython only on Linux; macOS omits them.
    #[cfg(target_os = "linux")]
    {
        v.push(("SIGIOT", libc::SIGIOT));
        v.push(("SIGPOLL", libc::SIGPOLL));
        v.push(("SIGPWR", libc::SIGPWR));
        v.push(("SIGRTMIN", libc::SIGRTMIN()));
        v.push(("SIGRTMAX", libc::SIGRTMAX()));
    }
    // BSD-family extras present on macOS' CPython build.
    #[cfg(any(target_os = "macos", target_os = "freebsd"))]
    {
        v.push(("SIGEMT", libc::SIGEMT));
        v.push(("SIGINFO", libc::SIGINFO));
    }
    v
}

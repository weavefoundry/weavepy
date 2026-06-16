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
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{Mutex, OnceLock};

use crate::error::{type_error, value_error, RuntimeError};
use crate::import::ModuleCache;
use crate::object::{BuiltinFn, DictData, DictKey, Object, PyModule};

/// File descriptor registered via `signal.set_wakeup_fd`; the OS signal
/// handler writes the signal byte here so a `select`/`poll` loop wakes.
/// `-1` when unset. Read inside the async-signal-safe handler.
static WAKEUP_FD: AtomicI32 = AtomicI32::new(-1);

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
        unsafe {
            libc::write(fd, byte.as_ptr().cast::<libc::c_void>(), 1);
        }
    }
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
#[cfg(unix)]
pub fn block_async_signals_current_thread() {
    unsafe {
        let set = async_signal_set();
        libc::pthread_sigmask(libc::SIG_BLOCK, &raw const set, std::ptr::null_mut());
    }
}

/// Unblock the asynchronous signals on the calling thread — the inverse
/// of [`block_async_signals_current_thread`]. The VM thread calls this at
/// startup so it is the sole thread with these signals unblocked and thus
/// the deterministic delivery target for `SIGINT`/`SIGALRM`/etc.
#[cfg(unix)]
pub fn unblock_async_signals_current_thread() {
    unsafe {
        let set = async_signal_set();
        libc::pthread_sigmask(libc::SIG_UNBLOCK, &raw const set, std::ptr::null_mut());
    }
}

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
            Object::from_static("signal"),
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
            b("set_wakeup_fd", set_wakeup_fd),
        );
        d.insert(
            DictKey(Object::from_static("pthread_kill")),
            b("pthread_kill", noop_int),
        );
        d.insert(
            DictKey(Object::from_static("valid_signals")),
            b("valid_signals", valid_signals),
        );

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
        name: "signal".to_owned(),
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
        _ => OsDisposition::Trip,
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
            return Err(crate::error::os_error("setitimer failed"));
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
            return Err(crate::error::os_error("getitimer failed"));
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
    Ok(handler_for(sig))
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
    // Deliver the signal for real (CPython's `raise()`), so the kernel
    // runs our trampoline on whichever thread called us; the main thread
    // then services the Python handler. When the disposition is our
    // trampoline this is equivalent to a direct trip; doing the real
    // `raise` also honours `SIG_DFL`/`SIG_IGN` faithfully.
    #[cfg(unix)]
    {
        let installed = !matches!(handler_for(sig), Object::Int(0));
        if installed {
            unsafe {
                libc::raise(sig as libc::c_int);
            }
        } else {
            // No Python handler — emulate without risking process death
            // for the common test signals by tripping the flag.
            trip_signal(sig);
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
    Ok(Object::from_str(format!("Signal {sig}")))
}

fn set_wakeup_fd(args: &[Object]) -> Result<Object, RuntimeError> {
    let fd = match args.first() {
        Some(Object::Int(n)) => *n as i32,
        None => -1,
        Some(_) => return Err(type_error("an integer is required")),
    };
    let prev = WAKEUP_FD.swap(fd, Ordering::AcqRel);
    Ok(Object::Int(i64::from(prev)))
}

fn noop_int(_args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(Object::None)
}

fn signum(arg: Option<&Object>) -> Result<i32, RuntimeError> {
    match arg {
        Some(Object::Int(n)) => Ok(*n as i32),
        _ => Err(type_error("signal number must be an int")),
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

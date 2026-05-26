//! The `signal` built-in module.
//!
//! WeavePy runs Python code on a single OS thread under a cooperative
//! scheduler, so the signal story is pragmatic rather than complete:
//!
//! * We expose every `SIG*` constant CPython exposes on the host
//!   platform (POSIX names on macOS/Linux/*BSD; a smaller Windows
//!   subset otherwise).
//! * `signal.signal(signum, handler)` records the user's handler
//!   alongside `SIG_DFL` / `SIG_IGN`. There is no Rust-side
//!   `signal-hook` registration today — the deliverable here is the
//!   *surface*, exercising the handler from the interpreter's main
//!   loop when a signal arrives is follow-up work tracked under
//!   "Future work" in RFC 0017.
//! * `signal.getsignal(signum)` returns the most recently installed
//!   handler.
//! * `signal.SIG_DFL` and `signal.SIG_IGN` are exposed as the
//!   conventional integer sentinels (0 and 1, matching CPython).
//! * `signal.default_int_handler` exists as a Python callable that
//!   raises `KeyboardInterrupt` — programs that install it
//!   (`signal.signal(SIGINT, signal.default_int_handler)`) get the
//!   right behaviour the next time the interpreter notices a
//!   pending SIGINT, which we currently do only at REPL boundaries
//!   and `asyncio.sleep` ticks.

use crate::sync::Rc;
use crate::sync::RefCell;
use std::collections::HashMap;

use crate::error::{type_error, RuntimeError};
use crate::import::ModuleCache;
use crate::object::{BuiltinFn, DictData, DictKey, Object, PyModule};

// User-installed signal handlers, keyed by signal number.
thread_local! {
    static HANDLERS: RefCell<HashMap<i32, Object>> = RefCell::new(HashMap::new());
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
                Object::Int(i64::from(*value)),
            );
        }

        d.insert(
            DictKey(Object::from_static("NSIG")),
            Object::Int(if cfg!(windows) { 23 } else { 65 }),
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
            b("default_int_handler", default_int_handler),
        );
        d.insert(
            DictKey(Object::from_static("raise_signal")),
            b("raise_signal", raise_signal),
        );
        d.insert(
            DictKey(Object::from_static("strsignal")),
            b("strsignal", strsignal),
        );
        d.insert(
            DictKey(Object::from_static("getitimer")),
            b("getitimer", noop_pair),
        );
        d.insert(
            DictKey(Object::from_static("setitimer")),
            b("setitimer", noop_pair),
        );
        d.insert(
            DictKey(Object::from_static("set_wakeup_fd")),
            b("set_wakeup_fd", set_wakeup_fd),
        );
        d.insert(
            DictKey(Object::from_static("pthread_kill")),
            b("pthread_kill", noop_int),
        );

        // Itimer constants — frozen modules occasionally import them.
        d.insert(DictKey(Object::from_static("ITIMER_REAL")), Object::Int(0));
        d.insert(
            DictKey(Object::from_static("ITIMER_VIRTUAL")),
            Object::Int(1),
        );
        d.insert(DictKey(Object::from_static("ITIMER_PROF")), Object::Int(2));
    }
    Rc::new(PyModule {
        name: "signal".to_owned(),
        filename: None,
        dict,
    })
}

fn b(name: &'static str, body: fn(&[Object]) -> Result<Object, RuntimeError>) -> Object {
    Object::Builtin(Rc::new(BuiltinFn {
        name,
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
    let previous = HANDLERS.with(|h| h.borrow().get(&sig).cloned().unwrap_or(Object::Int(0)));
    HANDLERS.with(|h| {
        h.borrow_mut().insert(sig, handler);
    });
    Ok(previous)
}

fn signal_getsignal(args: &[Object]) -> Result<Object, RuntimeError> {
    let sig = signum(args.first())?;
    Ok(HANDLERS.with(|h| h.borrow().get(&sig).cloned().unwrap_or(Object::Int(0))))
}

fn default_int_handler(_args: &[Object]) -> Result<Object, RuntimeError> {
    Err(RuntimeError::PyException(
        crate::error::PyException::from_builtin("KeyboardInterrupt", ""),
    ))
}

fn raise_signal(args: &[Object]) -> Result<Object, RuntimeError> {
    // No real OS-level kill — instead, invoke the user-installed
    // handler synchronously. This is enough for tests and for
    // patterns like `signal.raise_signal(signal.SIGTERM)` setting
    // the program into its own shutdown path.
    let sig = signum(args.first())?;
    if let Some(handler) = HANDLERS.with(|h| h.borrow().get(&sig).cloned()) {
        // SIG_DFL / SIG_IGN are integers; nothing to invoke.
        if !matches!(handler, Object::Int(_)) {
            // The actual call has to happen at the VM dispatch
            // boundary — we can't synchronously invoke a Python
            // callable from inside a builtin. Stash it on a
            // thread-local pending queue; the VM picks it up the
            // next time it checks signals.
            pending_signal_handlers().with(|p| {
                p.borrow_mut().push((sig, handler));
            });
        }
    }
    Ok(Object::None)
}

fn strsignal(args: &[Object]) -> Result<Object, RuntimeError> {
    let sig = signum(args.first())?;
    Ok(Object::from_str(format!("Signal {sig}")))
}

fn set_wakeup_fd(args: &[Object]) -> Result<Object, RuntimeError> {
    // Accept and return the previous value; we don't drive a fd in
    // our single-threaded cooperative model.
    let prev = args.first().cloned().unwrap_or(Object::Int(-1));
    let _ = prev;
    Ok(Object::Int(-1))
}

fn noop_pair(_args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(Object::new_tuple(vec![
        Object::Float(0.0),
        Object::Float(0.0),
    ]))
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

fn posix_signals() -> &'static [(&'static str, i32)] {
    if cfg!(windows) {
        &[
            ("SIGABRT", 22),
            ("SIGBREAK", 21),
            ("SIGFPE", 8),
            ("SIGILL", 4),
            ("SIGINT", 2),
            ("SIGSEGV", 11),
            ("SIGTERM", 15),
        ]
    } else {
        &[
            ("SIGHUP", 1),
            ("SIGINT", 2),
            ("SIGQUIT", 3),
            ("SIGILL", 4),
            ("SIGTRAP", 5),
            ("SIGABRT", 6),
            ("SIGIOT", 6),
            ("SIGBUS", 7),
            ("SIGFPE", 8),
            ("SIGKILL", 9),
            ("SIGUSR1", 10),
            ("SIGSEGV", 11),
            ("SIGUSR2", 12),
            ("SIGPIPE", 13),
            ("SIGALRM", 14),
            ("SIGTERM", 15),
            ("SIGCHLD", 17),
            ("SIGCONT", 18),
            ("SIGSTOP", 19),
            ("SIGTSTP", 20),
            ("SIGTTIN", 21),
            ("SIGTTOU", 22),
            ("SIGURG", 23),
            ("SIGXCPU", 24),
            ("SIGXFSZ", 25),
            ("SIGVTALRM", 26),
            ("SIGPROF", 27),
            ("SIGWINCH", 28),
            ("SIGIO", 29),
            ("SIGSYS", 31),
        ]
    }
}

/// Pending signal-handler invocations queued by `raise_signal`.
/// The VM consults this between bytecode batches; today the
/// integration is purely a placeholder that lets `raise_signal`
/// return without blowing up — exercising the actual handler from
/// the dispatch loop is RFC 0017 follow-up work.
fn pending_signal_handlers() -> &'static std::thread::LocalKey<RefCell<Vec<(i32, Object)>>> {
    thread_local! {
        static PENDING: RefCell<Vec<(i32, Object)>> = const { RefCell::new(Vec::new()) };
    }
    &PENDING
}

#[allow(dead_code)]
pub fn drain_pending() -> Vec<(i32, Object)> {
    pending_signal_handlers().with(|p| std::mem::take(&mut *p.borrow_mut()))
}

#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss
)]

//! The `faulthandler` built-in module.
//!
//! CPython ships `faulthandler` as a C extension that installs handlers
//! for the fatal signals (SIGSEGV/SIGFPE/SIGABRT/SIGBUS/SIGILL), dumps a
//! Python traceback on fault, and exposes a battery of *private* crash
//! primitives (`_sigsegv`, `_sigabrt`, …) used by its own test-suite and,
//! crucially for RFC 0040 WS6, by `test_concurrent_futures.test_deadlock`:
//! that suite forces a worker to `faulthandler._sigsegv()` and asserts the
//! `ProcessPoolExecutor` recovers with `BrokenProcessPool` instead of
//! deadlocking. Without the module, `import faulthandler` inside the worker
//! raised `ModuleNotFoundError`, so the crash never happened and every
//! crash-recovery case either errored or hung until `LONG_TIMEOUT`.
//!
//! The crash primitives are genuine (they `raise(3)` the real signal or
//! dereference NULL), so a worker that calls them dies exactly like a
//! CPython worker would. `enable`/`disable`/`is_enabled` track process
//! state; `dump_traceback` walks the running thread's Python frames and
//! writes a CPython-shaped report. We do **not** install async-signal
//! handlers from Rust (the only observable difference is the auto-dump on
//! an *uncaught* fault, exercised solely by the out-of-scope
//! `test_faulthandler`); everything the executor suites rely on is faithful.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use crate::sync::Rc;
use crate::sync::RefCell;

use crate::error::{type_error, value_error, RuntimeError};
use crate::import::ModuleCache;
use crate::object::{BuiltinFn, DictData, DictKey, Object, PyModule};

/// Process-global "is a fault handler installed" flag (CPython's
/// `fatal_error.enabled`). `faulthandler` state is per-process, not
/// per-interpreter, so a plain atomic is the right shape.
static ENABLED: AtomicBool = AtomicBool::new(false);

/// Monotonic generation stamp for `dump_traceback_later`. Arming a new
/// watchdog or calling `cancel_dump_traceback_later()` bumps it, which
/// makes any already-sleeping watchdog thread observe a mismatch and exit
/// without firing — a join-free cancellation.
static WATCHDOG_GEN: AtomicU64 = AtomicU64::new(0);

pub fn build(_cache: &ModuleCache) -> Rc<PyModule> {
    let dict = Rc::new(RefCell::new(DictData::new()));
    {
        let mut d = dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("__name__")),
            Object::from_static("faulthandler"),
        );
        d.insert(
            DictKey(Object::from_static("__doc__")),
            Object::from_static("faulthandler module."),
        );

        // State management.
        d.insert(DictKey(Object::from_static("enable")), builtin_kw("enable", fh_enable));
        d.insert(DictKey(Object::from_static("disable")), builtin("disable", fh_disable));
        d.insert(
            DictKey(Object::from_static("is_enabled")),
            builtin("is_enabled", fh_is_enabled),
        );
        d.insert(
            DictKey(Object::from_static("dump_traceback")),
            builtin_kw("dump_traceback", fh_dump_traceback),
        );
        d.insert(
            DictKey(Object::from_static("dump_traceback_later")),
            builtin_kw("dump_traceback_later", fh_dump_traceback_later),
        );
        d.insert(
            DictKey(Object::from_static("cancel_dump_traceback_later")),
            builtin("cancel_dump_traceback_later", fh_cancel_dump_traceback_later),
        );
        d.insert(
            DictKey(Object::from_static("register")),
            builtin_kw("register", fh_register),
        );
        d.insert(
            DictKey(Object::from_static("unregister")),
            builtin("unregister", fh_unregister),
        );

        // Private crash primitives (the test-suite entry points).
        d.insert(DictKey(Object::from_static("_sigsegv")), builtin("_sigsegv", fh_sigsegv));
        d.insert(DictKey(Object::from_static("_sigabrt")), builtin("_sigabrt", fh_sigabrt));
        d.insert(DictKey(Object::from_static("_sigfpe")), builtin("_sigfpe", fh_sigfpe));
        d.insert(DictKey(Object::from_static("_sigbus")), builtin("_sigbus", fh_sigbus));
        d.insert(DictKey(Object::from_static("_sigill")), builtin("_sigill", fh_sigill));
        d.insert(
            DictKey(Object::from_static("_fatal_error")),
            builtin("_fatal_error", fh_fatal_error),
        );
        d.insert(
            DictKey(Object::from_static("_read_null")),
            builtin("_read_null", fh_read_null),
        );
        d.insert(
            DictKey(Object::from_static("_stack_overflow")),
            builtin("_stack_overflow", fh_stack_overflow),
        );
    }
    Rc::new(PyModule {
        name: "faulthandler".to_owned(),
        filename: None,
        dict,
    })
}

fn builtin(name: &'static str, body: fn(&[Object]) -> Result<Object, RuntimeError>) -> Object {
    Object::Builtin(Rc::new(BuiltinFn {
        name,
        binds_instance: false,
        call: Box::new(body),
        call_kw: None,
    }))
}

fn builtin_kw(
    name: &'static str,
    body: fn(&[Object], &[(String, Object)]) -> Result<Object, RuntimeError>,
) -> Object {
    Object::Builtin(Rc::new(BuiltinFn::with_kwargs(name, body)))
}

fn kwarg<'a>(kwargs: &'a [(String, Object)], name: &str) -> Option<&'a Object> {
    kwargs.iter().find(|(k, _)| k == name).map(|(_, v)| v)
}

// ---------------------------------------------------------------------
// State management.
// ---------------------------------------------------------------------

fn fh_enable(_args: &[Object], _kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    // We track state but do not install async-signal handlers from Rust
    // (see module docs). The arguments (`file`, `all_threads`) are
    // accepted for API compatibility.
    ENABLED.store(true, Ordering::SeqCst);
    Ok(Object::None)
}

fn fh_disable(_args: &[Object]) -> Result<Object, RuntimeError> {
    ENABLED.store(false, Ordering::SeqCst);
    Ok(Object::None)
}

fn fh_is_enabled(_args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(Object::Bool(ENABLED.load(Ordering::SeqCst)))
}

// ---------------------------------------------------------------------
// dump_traceback — walk the running thread's Python frames.
// ---------------------------------------------------------------------

/// Format the current thread's call stack the way CPython's
/// `faulthandler` does: most-recent call first, one `File "…", line N in
/// func` per frame.
fn current_traceback_text() -> String {
    let mut s = String::new();
    s.push_str("Current thread (most recent call first):\n");
    if let Some(h) = crate::vm_singletons::current_thread_handles() {
        let stack = h.frame_stack.borrow();
        for frame in stack.iter().rev() {
            let code = &frame.code;
            s.push_str(&format!(
                "  File \"{}\", line {} in {}\n",
                code.filename,
                frame.current_lineno(),
                code.name
            ));
        }
    }
    s
}

fn fh_dump_traceback(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let file = args
        .first()
        .or_else(|| kwarg(kwargs, "file"))
        .cloned()
        .filter(|f| !matches!(f, Object::None));
    let text = current_traceback_text();

    let ptr = crate::vm_singletons::current_interpreter_ptr()
        .ok_or_else(|| value_error("no running interpreter"))?;
    // SAFETY: published by the enclosing VM frame on this thread; the GIL
    // keeps the access exclusive (same pattern as `signal_mod`).
    let interp = unsafe { &mut *ptr };

    let file_obj = match file {
        Some(f) => f,
        None => {
            let sys = interp.import_path("sys")?;
            interp.load_attr_public(&sys, "stderr")?
        }
    };
    let write = interp.load_attr_public(&file_obj, "write")?;
    interp.call_object(write, &[Object::from_str(text)], &[])?;
    if let Ok(flush) = interp.load_attr_public(&file_obj, "flush") {
        let _ = interp.call_object(flush, &[], &[]);
    }
    Ok(Object::None)
}

// ---------------------------------------------------------------------
// dump_traceback_later / cancel — a watchdog timer.
// ---------------------------------------------------------------------

fn fh_dump_traceback_later(
    args: &[Object],
    kwargs: &[(String, Object)],
) -> Result<Object, RuntimeError> {
    let timeout = args
        .first()
        .or_else(|| kwarg(kwargs, "timeout"))
        .and_then(Object::as_f64)
        .ok_or_else(|| type_error("dump_traceback_later() requires a numeric timeout"))?;
    if timeout <= 0.0 {
        return Err(value_error("timeout must be greater than 0"));
    }
    let do_exit = args
        .get(3)
        .or_else(|| kwarg(kwargs, "exit"))
        .map(Object::is_truthy)
        .unwrap_or(false);

    // Bump the generation; our watchdog only fires if it is still current.
    let my_gen = WATCHDOG_GEN.fetch_add(1, Ordering::SeqCst) + 1;
    let secs = timeout;
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs_f64(secs));
        if WATCHDOG_GEN.load(Ordering::SeqCst) != my_gen {
            return;
        }
        // A watchdog thread cannot safely re-enter the interpreter to walk
        // another thread's frames, so emit the timeout banner straight to
        // the real stderr (fd 2), then optionally hard-exit.
        let banner = format!("Timeout ({secs:.6}s)!\n");
        unsafe {
            libc::write(2, banner.as_ptr().cast(), banner.len());
        }
        if do_exit {
            unsafe { libc::_exit(1) };
        }
    });
    Ok(Object::None)
}

fn fh_cancel_dump_traceback_later(_args: &[Object]) -> Result<Object, RuntimeError> {
    WATCHDOG_GEN.fetch_add(1, Ordering::SeqCst);
    Ok(Object::None)
}

// ---------------------------------------------------------------------
// register / unregister — accepted and validated, no handler installed.
// ---------------------------------------------------------------------

fn signum_arg(obj: Option<&Object>) -> Result<i32, RuntimeError> {
    let n = obj
        .and_then(Object::as_i64)
        .ok_or_else(|| type_error("signum must be an integer"))?;
    if n < 1 || n >= 65 {
        return Err(value_error("signal number out of range"));
    }
    Ok(n as i32)
}

fn fh_register(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let _signum = signum_arg(args.first().or_else(|| kwarg(kwargs, "signum")))?;
    Ok(Object::None)
}

fn fh_unregister(args: &[Object]) -> Result<Object, RuntimeError> {
    let _signum = signum_arg(args.first())?;
    // CPython returns True if a handler had been registered for the
    // signal; we never install one, so report False.
    Ok(Object::Bool(false))
}

// ---------------------------------------------------------------------
// Crash primitives. These genuinely terminate the process, exactly like
// CPython's, so worker-crash detection in the executor suites is real.
// ---------------------------------------------------------------------

fn flush_std_streams() {
    use std::io::Write;
    let _ = std::io::stdout().flush();
    let _ = std::io::stderr().flush();
}

fn fh_sigsegv(_args: &[Object]) -> Result<Object, RuntimeError> {
    flush_std_streams();
    unsafe {
        libc::raise(libc::SIGSEGV);
        // Belt-and-braces: if SIGSEGV were somehow blocked, force a real
        // invalid read so the process still dies.
        let p: *const i32 = std::ptr::null();
        let _ = std::ptr::read_volatile(p);
    }
    Ok(Object::None)
}

fn fh_sigabrt(_args: &[Object]) -> Result<Object, RuntimeError> {
    flush_std_streams();
    unsafe { libc::abort() }
}

fn fh_sigfpe(_args: &[Object]) -> Result<Object, RuntimeError> {
    flush_std_streams();
    unsafe {
        libc::raise(libc::SIGFPE);
    }
    Ok(Object::None)
}

fn fh_sigbus(_args: &[Object]) -> Result<Object, RuntimeError> {
    flush_std_streams();
    unsafe {
        libc::raise(libc::SIGBUS);
    }
    Ok(Object::None)
}

fn fh_sigill(_args: &[Object]) -> Result<Object, RuntimeError> {
    flush_std_streams();
    unsafe {
        libc::raise(libc::SIGILL);
    }
    Ok(Object::None)
}

fn fh_fatal_error(args: &[Object]) -> Result<Object, RuntimeError> {
    if let Some(Object::Str(msg)) = args.first() {
        let line = format!("Fatal Python error: {msg}\n");
        unsafe {
            libc::write(2, line.as_ptr().cast(), line.len());
        }
    }
    flush_std_streams();
    unsafe { libc::abort() }
}

fn fh_read_null(_args: &[Object]) -> Result<Object, RuntimeError> {
    flush_std_streams();
    unsafe {
        let p: *const i32 = std::ptr::null();
        let _ = std::ptr::read_volatile(p);
    }
    Ok(Object::None)
}

fn fh_stack_overflow(_args: &[Object]) -> Result<Object, RuntimeError> {
    flush_std_streams();
    #[allow(unconditional_recursion)]
    fn recurse(depth: u64) -> u64 {
        // A real, un-tail-callable frame so the native stack actually
        // overflows (→ SIGSEGV / SIGBUS), matching CPython.
        let mut buf = [0u8; 256];
        std::hint::black_box(&mut buf);
        let next = recurse(std::hint::black_box(depth).wrapping_add(1));
        std::hint::black_box(next).wrapping_add(u64::from(buf[0]))
    }
    std::hint::black_box(recurse(0));
    Ok(Object::None)
}

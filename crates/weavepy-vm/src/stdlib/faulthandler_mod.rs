#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    clippy::cast_possible_wrap
)]

//! The `faulthandler` built-in module — RFC 0023, byte-parity dumps per
//! RFC 0057 WS6.
//!
//! Mirrors CPython's `Modules/faulthandler.c` + `Python/traceback.c`:
//!
//! * `enable()` installs real `sigaction` handlers (with an alternate
//!   signal stack, `SA_ONSTACK`) for SIGSEGV/SIGFPE/SIGABRT/SIGBUS/SIGILL
//!   that write `Fatal Python error: <name>\n\n` plus the CPython-shaped
//!   thread dump to the configured fd, then re-raise so the process dies
//!   with the original signal.
//! * `dump_traceback(file, all_threads)` reproduces
//!   `_Py_DumpTracebackThreads` / `_Py_DumpTraceback` exactly:
//!   `Current thread 0x… (most recent call first):` / `Thread 0x…` /
//!   `Stack (most recent call first):` headers, `  File "…", line N in
//!   <name>` frames (most recent first), 500-char string truncation and
//!   the 100-frame `  ...` cap.
//! * `dump_traceback_later(timeout)` arms a watchdog thread that writes
//!   `Timeout (H:MM:SS.ffffff)!` plus an all-threads dump.
//! * `register(signum)` installs a user-signal handler that dumps and
//!   (optionally) chains to the previous handler.
//!
//! Cross-thread dumps read a process-global registry of per-thread frame
//! stacks (`note_thread_start`, fed by
//! `vm_singletons::activate_thread_handles`). Frame stacks are
//! `Arc<GilCell<…>>`, so a watchdog / crashing thread can walk a parked
//! peer's Python stack exactly like CPython walks its `PyThreadState`
//! list.

#[cfg(unix)]
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use crate::sync::Rc;
use crate::sync::RefCell;

use crate::error::{type_error, value_error, RuntimeError};
use crate::import::ModuleCache;
use crate::object::{BuiltinFn, DictData, DictKey, Object, PyFrame, PyModule};

/// Process-global "is a fault handler installed" flag (CPython's
/// `fatal_error.enabled`).
static ENABLED: AtomicBool = AtomicBool::new(false);
/// The fd fatal dumps write to (CPython stores the file + fd; tests keep
/// the file open so caching the fd is faithful).
static FATAL_FD: AtomicI32 = AtomicI32::new(2);
static FATAL_ALL_THREADS: AtomicBool = AtomicBool::new(true);

/// Monotonic generation stamp for `dump_traceback_later`. Arming a new
/// watchdog or calling `cancel_dump_traceback_later()` bumps it, which
/// makes any already-sleeping watchdog thread observe a mismatch and exit
/// without firing — a join-free cancellation.
static WATCHDOG_GEN: AtomicU64 = AtomicU64::new(0);

// ---------------------------------------------------------------------
// Per-thread frame-stack registry (CPython's tstate list analogue).
// ---------------------------------------------------------------------

struct RegisteredThread {
    ident: u64,
    frame_stack: Rc<RefCell<Vec<Rc<PyFrame>>>>,
}

/// Registration order == thread creation order; CPython's
/// `PyInterpreterState_ThreadHead` list is newest-first, so dumps iterate
/// this in reverse.
static THREADS: Mutex<Vec<RegisteredThread>> = Mutex::new(Vec::new());

/// Called (once per OS thread) by `vm_singletons::activate_thread_handles`.
pub fn note_thread_start(ident: u64, frame_stack: Rc<RefCell<Vec<Rc<PyFrame>>>>) {
    let mut g = THREADS.lock().unwrap();
    if g.iter().any(|t| t.ident == ident) {
        return;
    }
    g.push(RegisteredThread { ident, frame_stack });
}

/// Called from the thread-local guard's `Drop` at OS-thread exit.
pub fn note_thread_exit(ident: u64) {
    if let Ok(mut g) = THREADS.lock() {
        g.retain(|t| t.ident != ident);
    }
}

// ---------------------------------------------------------------------
// Extension-modules context for the fatal dump's trailing line.
// ---------------------------------------------------------------------

struct ExtModulesCtx {
    /// `sys.modules` (the module cache dict).
    modules: Rc<RefCell<DictData>>,
    /// Names registered as native (Rust) built-in modules — WeavePy's
    /// analogue of CPython's `_PyModule_IsExtension`.
    native_names: Vec<&'static str>,
    /// The `sys` module dict, for a crash-time read of
    /// `sys.stdlib_module_names` (test_dump_ext_modules empties it).
    sys_dict: Option<Rc<RefCell<DictData>>>,
}

static EXT_CTX: Mutex<Option<ExtModulesCtx>> = Mutex::new(None);

/// Snapshot the module-cache handles the fatal dump needs. Called at
/// `enable()` and at startup flag application.
pub fn set_module_context(cache: &ModuleCache) {
    let native_names: Vec<&'static str> = cache.builtins.borrow().keys().copied().collect();
    let sys_dict = match cache.get("sys") {
        Some(Object::Module(m)) => Some(m.dict.clone()),
        _ => None,
    };
    *EXT_CTX.lock().unwrap() = Some(ExtModulesCtx {
        modules: cache.modules.clone(),
        native_names,
        sys_dict,
    });
}

// ---------------------------------------------------------------------
// Dump formatting — `Python/traceback.c` semantics.
// ---------------------------------------------------------------------

/// `_Py_DumpASCII`'s truncation: at most 500 chars, then "...".
const MAX_STRING_LENGTH: usize = 500;
/// `dump_traceback`'s frame cap, then "  ...".
const MAX_FRAME_DEPTH: usize = 100;

fn put_truncated(out: &mut String, s: &str) {
    let n = s.chars().count();
    if n > MAX_STRING_LENGTH {
        out.extend(s.chars().take(MAX_STRING_LENGTH));
        out.push_str("...");
    } else {
        out.push_str(s);
    }
}

/// One `  File "<file>", line N in <name>` line (CPython `dump_frame`).
fn dump_frame_line(out: &mut String, frame: &PyFrame) {
    out.push_str("  File \"");
    put_truncated(out, &frame.code.filename);
    out.push_str(&format!("\", line {} in ", frame.current_lineno()));
    put_truncated(out, &frame.code.name);
    out.push('\n');
}

/// CPython `dump_traceback(fd, tstate, write_header=0)`: frames most
/// recent first, capped at [`MAX_FRAME_DEPTH`].
fn dump_frames(out: &mut String, frame_stack: &Rc<RefCell<Vec<Rc<PyFrame>>>>) {
    // `try_borrow`, not `borrow`: at crash time the owning thread may
    // have the stack mutably borrowed; a headerless dump beats a panic
    // inside the signal handler.
    let Ok(stack) = frame_stack.try_borrow() else {
        return;
    };
    if stack.is_empty() {
        out.push_str("  <no Python frame>\n");
        return;
    }
    for (depth, frame) in stack.iter().rev().enumerate() {
        if depth >= MAX_FRAME_DEPTH {
            out.push_str("  ...\n");
            break;
        }
        dump_frame_line(out, frame);
    }
}

/// CPython `write_thread_id`: `0x` + the thread id zero-padded to
/// `sizeof(unsigned long) * 2` hex digits.
fn thread_header(out: &mut String, ident: u64, is_current: bool) {
    if is_current {
        out.push_str("Current thread 0x");
    } else {
        out.push_str("Thread 0x");
    }
    out.push_str(&format!("{ident:016x}"));
    out.push_str(" (most recent call first):\n");
}

/// CPython `_Py_DumpTracebackThreads`: every registered thread,
/// newest-first, blocks separated by a blank line, the current thread
/// (when known) marked `Current thread`. bpo-44466: the current thread
/// gets a `  Garbage-collecting` marker while the cycle GC is running.
fn dump_all_threads(out: &mut String, current_ident: Option<u64>) {
    let Ok(threads) = THREADS.lock() else {
        return;
    };
    for (i, t) in threads.iter().rev().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        let is_current = Some(t.ident) == current_ident;
        thread_header(out, t.ident, is_current);
        if is_current && crate::gc_trace::collection_in_progress() {
            out.push_str("  Garbage-collecting\n");
        }
        dump_frames(out, &t.frame_stack);
    }
}

/// CPython `_Py_DumpTraceback` (the `all_threads=False` shape).
fn dump_current_stack(out: &mut String) {
    out.push_str("Stack (most recent call first):\n");
    if let Some(h) = crate::vm_singletons::current_thread_handles() {
        dump_frames(out, &h.frame_stack);
    }
}

/// CPython `_Py_DumpExtensionModules`: the native modules currently in
/// `sys.modules`, minus `sys.stdlib_module_names`. Silent when the
/// filtered list is empty (the common case with the stdlib names set).
fn dump_ext_modules(out: &mut String) {
    let Ok(ctx_guard) = EXT_CTX.lock() else {
        return;
    };
    let Some(ctx) = ctx_guard.as_ref() else {
        return;
    };
    // `sys.stdlib_module_names` may have been replaced by user code
    // (test_dump_ext_modules sets it to an empty frozenset).
    let stdlib_names: Vec<String> = ctx
        .sys_dict
        .as_ref()
        .and_then(|d| {
            d.try_borrow().ok().map(|d| {
                match d.get(&DictKey(Object::from_static("stdlib_module_names"))) {
                    Some(Object::FrozenSet(fs)) => fs
                        .iter()
                        .filter_map(|k| match &k.0 {
                            Object::Str(s) => Some(s.as_ref().to_owned()),
                            _ => None,
                        })
                        .collect(),
                    _ => Vec::new(),
                }
            })
        })
        .unwrap_or_default();
    let Ok(modules) = ctx.modules.try_borrow() else {
        return;
    };
    let mut names: Vec<String> = Vec::new();
    for (k, v) in modules.iter() {
        let Object::Str(name) = &k.0 else { continue };
        if !matches!(v, Object::Module(_)) {
            continue;
        }
        let name = name.as_ref();
        if !ctx.native_names.contains(&name) {
            continue;
        }
        if stdlib_names.iter().any(|s| s == name) {
            continue;
        }
        names.push(name.to_owned());
    }
    if names.is_empty() {
        return;
    }
    out.push_str("\nExtension modules: ");
    out.push_str(&names.join(", "));
    out.push_str(&format!(" (total: {})\n", names.len()));
}

fn current_ident() -> u64 {
    crate::vm_singletons::current_worker_thread_id()
}

// ---------------------------------------------------------------------
// fd plumbing.
// ---------------------------------------------------------------------

/// Raw `write(2)` straight to a descriptor. The byte-count parameter is
/// `size_t` on POSIX but `c_uint` on Windows; narrow per platform.
fn write_fd(fd: libc::c_int, bytes: &[u8]) {
    #[cfg(unix)]
    let count = bytes.len();
    #[cfg(not(unix))]
    let count = bytes.len() as libc::c_uint;
    unsafe {
        libc::write(fd, bytes.as_ptr().cast(), count);
    }
}

/// CPython `faulthandler_get_fileno`: `None`/omitted means `sys.stderr`
/// (a `None` stderr is a RuntimeError with this exact text — bpo-21497);
/// an int is used as-is; anything else must have `fileno()`, and its
/// Python-level buffer is flushed so the raw fd write lands after
/// buffered output.
fn resolve_fd(interp: &mut crate::Interpreter, file: Option<Object>) -> Result<i32, RuntimeError> {
    let file_obj = match file {
        Some(f) if !matches!(f, Object::None) => f,
        _ => {
            let sys = interp.import_path("sys")?;
            let stderr = interp.load_attr_public(&sys, "stderr")?;
            if matches!(stderr, Object::None) {
                return Err(crate::error::runtime_error("sys.stderr is None"));
            }
            stderr
        }
    };
    match file_obj {
        Object::Int(fd) => {
            if fd < 0 {
                return Err(value_error("file is not a valid file descriptor"));
            }
            Ok(fd as i32)
        }
        obj => {
            let fileno = interp.load_attr_public(&obj, "fileno")?;
            let fd = match interp.call_object(fileno, &[], &[])? {
                Object::Int(fd) if fd >= 0 => fd as i32,
                _ => {
                    return Err(crate::error::runtime_error(
                        "file.fileno() is not a valid file descriptor",
                    ))
                }
            };
            if let Ok(flush) = interp.load_attr_public(&obj, "flush") {
                let _ = interp.call_object(flush, &[], &[]);
            }
            Ok(fd)
        }
    }
}

fn current_interp(what: &str) -> Result<&'static mut crate::Interpreter, RuntimeError> {
    let ptr = crate::vm_singletons::current_interpreter_ptr()
        .ok_or_else(|| crate::error::runtime_error(format!("{what}: no running interpreter")))?;
    // SAFETY: published by the enclosing VM frame on this thread; the GIL
    // keeps the access exclusive (same pattern as `signal_mod`).
    Ok(unsafe { &mut *ptr })
}

// ---------------------------------------------------------------------
// Fatal-signal handlers (`faulthandler.enable`).
// ---------------------------------------------------------------------

#[cfg(unix)]
const FATAL_SIGNALS: [(libc::c_int, &str); 5] = [
    // CPython `faulthandler_handlers` order (SIGSEGV last so it's the
    // first restored on disable — order only matters for messages here).
    (libc::SIGBUS, "Bus error"),
    (libc::SIGILL, "Illegal instruction"),
    (libc::SIGFPE, "Floating-point exception"),
    (libc::SIGABRT, "Aborted"),
    (libc::SIGSEGV, "Segmentation fault"),
];

#[cfg(unix)]
static OLD_FATAL_ACTIONS: Mutex<Vec<(libc::c_int, libc::sigaction)>> = Mutex::new(Vec::new());

/// One-time alternate signal stack so the SIGSEGV of a stack overflow
/// can still run the handler (CPython allocates `stack.ss_size =
/// SIGSTKSZ` in `_PyFaulthandler_Init`).
#[cfg(unix)]
fn ensure_altstack() {
    use std::sync::Once;
    static ONCE: Once = Once::new();
    ONCE.call_once(|| unsafe {
        // A generous fixed size (Rust's `String` formatting in the
        // handler needs more headroom than CPython's write(2)-only path).
        const ALT_SIZE: usize = 256 * 1024;
        let ptr = libc::malloc(ALT_SIZE);
        if ptr.is_null() {
            return;
        }
        let stack = libc::stack_t {
            ss_sp: ptr,
            ss_size: ALT_SIZE,
            ss_flags: 0,
        };
        libc::sigaltstack(&raw const stack, std::ptr::null_mut());
    });
}

#[cfg(unix)]
extern "C" fn fatal_signal_handler(sig: libc::c_int) {
    // CPython `faulthandler_fatal_error`: disable (restore the previous
    // handlers) first so the re-raise below terminates the process and a
    // crash *inside this handler* can't recurse.
    if !ENABLED.swap(false, Ordering::SeqCst) {
        unsafe { libc::raise(sig) };
        return;
    }
    restore_fatal_handlers();
    let fd = FATAL_FD.load(Ordering::SeqCst);
    let name = FATAL_SIGNALS
        .iter()
        .find(|(s, _)| *s == sig)
        .map_or("Fatal error", |(_, n)| n);
    let mut out = String::new();
    out.push_str("Fatal Python error: ");
    out.push_str(name);
    out.push_str("\n\n");
    if FATAL_ALL_THREADS.load(Ordering::SeqCst) {
        dump_all_threads(&mut out, Some(current_ident()));
    } else {
        dump_current_stack(&mut out);
    }
    dump_ext_modules(&mut out);
    // Diagnostic escape hatch: a native backtrace of the faulting thread
    // (not async-signal-safe — allocates — so it is strictly opt-in).
    if std::env::var("WEAVEPY_NATIVE_TRACE").is_ok() {
        out.push_str(&format!(
            "\nNative backtrace:\n{}\n",
            std::backtrace::Backtrace::force_capture()
        ));
    }
    write_fd(fd, out.as_bytes());
    // Re-raise with the *default* disposition so the process dies with the
    // signal, as CPython's child does. Merely restoring the pre-enable
    // handler is not enough here: the Rust runtime installs its own
    // SIGSEGV/SIGBUS stack-overflow probe, which swallows a `raise(2)`d
    // signal (no faulting instruction to re-execute) and lets the process
    // continue — observed as `signal.raise_signal(SIGBUS)` exiting 0
    // (test_faulthandler.test_sigbus).
    unsafe {
        let mut dfl: libc::sigaction = std::mem::zeroed();
        dfl.sa_sigaction = libc::SIG_DFL;
        libc::sigemptyset(&raw mut dfl.sa_mask);
        libc::sigaction(sig, &raw const dfl, std::ptr::null_mut());
        libc::raise(sig);
    }
}

/// Diagnostic-only (`WEAVEPY_NATIVE_TRACE`) SA_SIGINFO handler: report the
/// faulting PC and walk the arm64 frame-pointer chain so a native crash in
/// an extension module can be symbolicated offline (`atos`) — the regular
/// handler runs on the alternate signal stack, which breaks Rust's own
/// unwinder at the signal frame. Not async-signal-safe (allocates); strictly
/// an opt-in debugging aid.
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[allow(deprecated)] // the dyld image-list accessors are fine for a debug dump
extern "C" fn fatal_signal_handler_native_trace(
    sig: libc::c_int,
    info: *mut libc::siginfo_t,
    ctx: *mut libc::c_void,
) {
    unsafe {
        let uc = ctx.cast::<libc::ucontext_t>();
        if !uc.is_null() && !(*uc).uc_mcontext.is_null() {
            let ss = &(*(*uc).uc_mcontext).__ss;
            let mut msg = format!(
                "\n[native-trace] sig={} fault_addr={:p} pc={:#x} lr={:#x} fp={:#x}\n[native-trace] frames: {:#x} {:#x}",
                sig,
                (*info).si_addr,
                ss.__pc,
                ss.__lr,
                ss.__fp,
                ss.__pc,
                ss.__lr,
            );
            let mut fp = ss.__fp;
            for _ in 0..48 {
                if fp < 0x1000 || fp % 16 != 0 {
                    break;
                }
                let next = *(fp as *const u64);
                let lr = *((fp + 8) as *const u64);
                if lr < 0x1000 {
                    break;
                }
                msg.push_str(&format!(" {lr:#x}"));
                if next <= fp {
                    break;
                }
                fp = next;
            }
            msg.push_str("\n[native-trace] images:");
            let n = libc::_dyld_image_count();
            for i in 0..n {
                let name_p = libc::_dyld_get_image_name(i);
                if name_p.is_null() {
                    continue;
                }
                let name = std::ffi::CStr::from_ptr(name_p).to_string_lossy();
                if name.contains("weavepy") || name.contains("site-packages") {
                    msg.push_str(&format!(
                        "\n  {:#x} {}",
                        libc::_dyld_get_image_header(i) as usize,
                        name
                    ));
                }
            }
            msg.push('\n');
            write_fd(2, msg.as_bytes());
        }
    }
    fatal_signal_handler(sig);
}

#[cfg(unix)]
fn install_fatal_handlers() {
    ensure_altstack();
    let mut saved = OLD_FATAL_ACTIONS.lock().unwrap();
    if !saved.is_empty() {
        return; // already installed
    }
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    let native_trace = std::env::var("WEAVEPY_NATIVE_TRACE").is_ok();
    for (sig, _) in FATAL_SIGNALS {
        unsafe {
            let mut new_action: libc::sigaction = std::mem::zeroed();
            new_action.sa_sigaction = fatal_signal_handler as *const () as usize;
            new_action.sa_flags = libc::SA_ONSTACK | libc::SA_NODEFER;
            #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
            if native_trace {
                new_action.sa_sigaction = fatal_signal_handler_native_trace as *const () as usize;
                new_action.sa_flags |= libc::SA_SIGINFO;
            }
            libc::sigemptyset(&raw mut new_action.sa_mask);
            let mut old_action: libc::sigaction = std::mem::zeroed();
            if libc::sigaction(sig, &raw const new_action, &raw mut old_action) == 0 {
                saved.push((sig, old_action));
            }
        }
    }
}

#[cfg(unix)]
fn restore_fatal_handlers() {
    if let Ok(mut saved) = OLD_FATAL_ACTIONS.lock() {
        for (sig, old) in saved.drain(..) {
            unsafe {
                libc::sigaction(sig, &raw const old, std::ptr::null_mut());
            }
        }
    }
}

/// CPython `Py_FatalError` with a reporting C function name — the shape
/// `_testcapi.fatal_error(message)` produces
/// (test_faulthandler.test_fatal_error). Native entry point so the dump
/// carries no wrapper frame of its own.
pub fn py_fatal_error(func: &str, msg: &str) -> ! {
    flush_std_streams();
    py_fatal_error_and_abort(&format!("{func}: {msg}"), Some(current_ident()));
}

/// Startup path (`-X faulthandler` / `PYTHONFAULTHANDLER` / dev mode):
/// enable against fd 2 before any user code runs.
pub fn enable_startup(cache: &ModuleCache) {
    set_module_context(cache);
    FATAL_FD.store(2, Ordering::SeqCst);
    FATAL_ALL_THREADS.store(true, Ordering::SeqCst);
    #[cfg(unix)]
    install_fatal_handlers();
    ENABLED.store(true, Ordering::SeqCst);
}

// ---------------------------------------------------------------------
// Module surface.
// ---------------------------------------------------------------------

pub fn build(_cache: &ModuleCache) -> Rc<PyModule> {
    let dict = Rc::new(RefCell::new(DictData::default()));
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
        d.insert(
            DictKey(Object::from_static("enable")),
            builtin_kw("enable", fh_enable),
        );
        d.insert(
            DictKey(Object::from_static("disable")),
            builtin("disable", fh_disable),
        );
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
            builtin(
                "cancel_dump_traceback_later",
                fh_cancel_dump_traceback_later,
            ),
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
        d.insert(
            DictKey(Object::from_static("_sigsegv")),
            builtin("_sigsegv", fh_sigsegv),
        );
        d.insert(
            DictKey(Object::from_static("_sigabrt")),
            builtin("_sigabrt", fh_sigabrt),
        );
        d.insert(
            DictKey(Object::from_static("_sigfpe")),
            builtin("_sigfpe", fh_sigfpe),
        );
        d.insert(
            DictKey(Object::from_static("_sigbus")),
            builtin("_sigbus", fh_sigbus),
        );
        d.insert(
            DictKey(Object::from_static("_sigill")),
            builtin("_sigill", fh_sigill),
        );
        d.insert(
            DictKey(Object::from_static("_fatal_error")),
            builtin("_fatal_error", fh_fatal_error),
        );
        d.insert(
            DictKey(Object::from_static("_fatal_error_c_thread")),
            builtin("_fatal_error_c_thread", fh_fatal_error_c_thread),
        );
        d.insert(
            DictKey(Object::from_static("_weave_py_fatal_error")),
            builtin("_weave_py_fatal_error", fh_py_fatal_error),
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

fn fh_enable(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let interp = current_interp("faulthandler.enable()")?;
    let file = args.first().or_else(|| kwarg(kwargs, "file")).cloned();
    let all_threads = args
        .get(1)
        .or_else(|| kwarg(kwargs, "all_threads"))
        .is_none_or(Object::is_truthy);
    let fd = resolve_fd(interp, file)?;
    set_module_context(&interp.cache);
    FATAL_FD.store(fd, Ordering::SeqCst);
    FATAL_ALL_THREADS.store(all_threads, Ordering::SeqCst);
    #[cfg(unix)]
    install_fatal_handlers();
    ENABLED.store(true, Ordering::SeqCst);
    Ok(Object::None)
}

fn fh_disable(_args: &[Object]) -> Result<Object, RuntimeError> {
    if ENABLED.swap(false, Ordering::SeqCst) {
        #[cfg(unix)]
        restore_fatal_handlers();
    }
    Ok(Object::None)
}

fn fh_is_enabled(_args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(Object::Bool(ENABLED.load(Ordering::SeqCst)))
}

// ---------------------------------------------------------------------
// dump_traceback.
// ---------------------------------------------------------------------

fn fh_dump_traceback(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let interp = current_interp("faulthandler.dump_traceback()")?;
    let file = args.first().or_else(|| kwarg(kwargs, "file")).cloned();
    let all_threads = args
        .get(1)
        .or_else(|| kwarg(kwargs, "all_threads"))
        .is_none_or(Object::is_truthy);
    let fd = resolve_fd(interp, file)?;
    let mut out = String::new();
    if all_threads {
        dump_all_threads(&mut out, Some(current_ident()));
    } else {
        dump_current_stack(&mut out);
    }
    write_fd(fd, out.as_bytes());
    Ok(Object::None)
}

// ---------------------------------------------------------------------
// dump_traceback_later / cancel — the watchdog timer.
// ---------------------------------------------------------------------

/// CPython `format_timeout`: `Timeout (H:MM:SS[.ffffff])!`.
fn format_timeout(us: u64) -> String {
    let mut sec = us / 1_000_000;
    let frac = us % 1_000_000;
    let mut min = sec / 60;
    sec %= 60;
    let hour = min / 60;
    min %= 60;
    if frac != 0 {
        format!("Timeout ({hour}:{min:02}:{sec:02}.{frac:06})!\n")
    } else {
        format!("Timeout ({hour}:{min:02}:{sec:02})!\n")
    }
}

fn fh_dump_traceback_later(
    args: &[Object],
    kwargs: &[(String, Object)],
) -> Result<Object, RuntimeError> {
    let interp = current_interp("faulthandler.dump_traceback_later()")?;
    let timeout = args
        .first()
        .or_else(|| kwarg(kwargs, "timeout"))
        .and_then(Object::as_f64)
        .ok_or_else(|| type_error("dump_traceback_later() requires a numeric timeout"))?;
    if timeout <= 0.0 {
        return Err(value_error("timeout must be greater than 0"));
    }
    let repeat = args
        .get(1)
        .or_else(|| kwarg(kwargs, "repeat"))
        .map(Object::is_truthy)
        .unwrap_or(false);
    let file = args.get(2).or_else(|| kwarg(kwargs, "file")).cloned();
    let do_exit = args
        .get(3)
        .or_else(|| kwarg(kwargs, "exit"))
        .map(Object::is_truthy)
        .unwrap_or(false);
    let fd = resolve_fd(interp, file)?;

    // Bump the generation; the watchdog only fires while it is current.
    let my_gen = WATCHDOG_GEN.fetch_add(1, Ordering::SeqCst) + 1;
    let timeout_us = (timeout * 1e6).round() as u64;
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_micros(timeout_us));
        if WATCHDOG_GEN.load(Ordering::SeqCst) != my_gen {
            return;
        }
        // CPython `faulthandler_thread`: banner + all-threads dump with
        // no known current thread (every block reads `Thread 0x…`).
        let mut out = format_timeout(timeout_us);
        dump_all_threads(&mut out, None);
        write_fd(fd, out.as_bytes());
        if do_exit {
            unsafe { libc::_exit(1) };
        }
        if !repeat {
            return;
        }
    });
    Ok(Object::None)
}

fn fh_cancel_dump_traceback_later(_args: &[Object]) -> Result<Object, RuntimeError> {
    WATCHDOG_GEN.fetch_add(1, Ordering::SeqCst);
    Ok(Object::None)
}

// ---------------------------------------------------------------------
// register / unregister — user-signal dump handlers.
// ---------------------------------------------------------------------

#[cfg(unix)]
struct UserSignal {
    fd: i32,
    all_threads: bool,
    chain: bool,
    old_action: libc::sigaction,
}

// SAFETY: `libc::sigaction` is plain data (fn pointer + mask + flags).
#[cfg(unix)]
unsafe impl Send for UserSignal {}

#[cfg(unix)]
static USER_SIGNALS: Mutex<Option<HashMap<i32, UserSignal>>> = Mutex::new(None);

#[cfg(unix)]
extern "C" fn user_signal_handler(sig: libc::c_int) {
    let (fd, all_threads, chain, old_action) = {
        let Ok(guard) = USER_SIGNALS.lock() else {
            return;
        };
        let Some(map) = guard.as_ref() else { return };
        let Some(u) = map.get(&sig) else { return };
        (u.fd, u.all_threads, u.chain, u.old_action)
    };
    let mut out = String::new();
    if all_threads {
        dump_all_threads(&mut out, Some(current_ident()));
    } else {
        dump_current_stack(&mut out);
    }
    write_fd(fd, out.as_bytes());
    if chain {
        // CPython `faulthandler_user`: restore the previous handler,
        // re-raise so it runs synchronously, then re-install ours.
        unsafe {
            libc::sigaction(sig, &raw const old_action, std::ptr::null_mut());
            libc::raise(sig);
        }
        install_user_handler(sig, chain);
    }
}

#[cfg(unix)]
fn install_user_handler(sig: libc::c_int, chain: bool) -> libc::sigaction {
    unsafe {
        let mut new_action: libc::sigaction = std::mem::zeroed();
        new_action.sa_sigaction = user_signal_handler as *const () as usize;
        new_action.sa_flags = libc::SA_ONSTACK | libc::SA_RESTART;
        if chain {
            // Without SA_NODEFER the signal stays blocked inside the
            // handler, so the chained `raise()` only goes *pending* and is
            // redelivered — to our freshly re-installed handler — after
            // return: an unbounded signal loop (CPython sets SA_NODEFER
            // for chained registrations; test_register_chain).
            new_action.sa_flags |= libc::SA_NODEFER;
        }
        libc::sigemptyset(&raw mut new_action.sa_mask);
        let mut old_action: libc::sigaction = std::mem::zeroed();
        libc::sigaction(sig, &raw const new_action, &raw mut old_action);
        old_action
    }
}

fn signum_arg(obj: Option<&Object>) -> Result<i32, RuntimeError> {
    let n = obj
        .and_then(Object::as_i64)
        .ok_or_else(|| type_error("signum must be an integer"))?;
    if !(1..65).contains(&n) {
        return Err(value_error("signal number out of range"));
    }
    Ok(n as i32)
}

fn fh_register(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let signum = signum_arg(args.first().or_else(|| kwarg(kwargs, "signum")))?;
    let interp = current_interp("faulthandler.register()")?;
    let file = args.get(1).or_else(|| kwarg(kwargs, "file")).cloned();
    let all_threads = args
        .get(2)
        .or_else(|| kwarg(kwargs, "all_threads"))
        .is_none_or(Object::is_truthy);
    let chain = args
        .get(3)
        .or_else(|| kwarg(kwargs, "chain"))
        .map(Object::is_truthy)
        .unwrap_or(false);
    let fd = resolve_fd(interp, file)?;
    // Windows has no user-signal registration; the arguments are still
    // validated above, exactly like CPython's stub.
    #[cfg(not(unix))]
    let _ = (signum, all_threads, chain, fd);
    #[cfg(unix)]
    {
        ensure_altstack();
        let mut guard = USER_SIGNALS.lock().unwrap();
        let map = guard.get_or_insert_with(HashMap::new);
        let old_action = install_user_handler(signum, chain);
        match map.entry(signum) {
            std::collections::hash_map::Entry::Occupied(mut e) => {
                // Re-registration keeps the *original* previous handler
                // (ours was installed in between).
                let prev = e.get().old_action;
                *e.get_mut() = UserSignal {
                    fd,
                    all_threads,
                    chain,
                    old_action: prev,
                };
            }
            std::collections::hash_map::Entry::Vacant(v) => {
                v.insert(UserSignal {
                    fd,
                    all_threads,
                    chain,
                    old_action,
                });
            }
        }
    }
    Ok(Object::None)
}

fn fh_unregister(args: &[Object]) -> Result<Object, RuntimeError> {
    let signum = signum_arg(args.first())?;
    #[cfg(not(unix))]
    let _ = signum;
    #[cfg(unix)]
    {
        let mut guard = USER_SIGNALS.lock().unwrap();
        if let Some(map) = guard.as_mut() {
            if let Some(u) = map.remove(&signum) {
                unsafe {
                    libc::sigaction(signum, &raw const u.old_action, std::ptr::null_mut());
                }
                return Ok(Object::Bool(true));
            }
        }
    }
    Ok(Object::Bool(false))
}

// ---------------------------------------------------------------------
// Py_FatalError-shaped dumps (`_testcapi.fatal_error`,
// `_fatal_error_c_thread`).
// ---------------------------------------------------------------------

/// CPython `Py_FatalError` body: header, `Python runtime state:
/// initialized`, blank line, all-threads dump (current marked when
/// known), extension modules, `abort()`. Any enabled fault handler is
/// disabled first so the SIGABRT doesn't double-dump.
fn py_fatal_error_and_abort(header: &str, current: Option<u64>) -> ! {
    if ENABLED.swap(false, Ordering::SeqCst) {
        #[cfg(unix)]
        restore_fatal_handlers();
    }
    let mut out = String::new();
    out.push_str("Fatal Python error: ");
    out.push_str(header);
    out.push('\n');
    out.push_str("Python runtime state: initialized\n\n");
    dump_all_threads(&mut out, current);
    dump_ext_modules(&mut out);
    write_fd(2, out.as_bytes());
    flush_std_streams();
    unsafe { libc::abort() }
}

/// `faulthandler._weave_py_fatal_error(func, msg)` — backs the frozen
/// `_testcapi.fatal_error` (CPython's `Py_FatalError` with `__func__`
/// = `_testcapi_fatal_error_impl`).
fn fh_py_fatal_error(args: &[Object]) -> Result<Object, RuntimeError> {
    let func = match args.first() {
        Some(Object::Str(s)) => s.as_ref().to_owned(),
        _ => "unknown".to_owned(),
    };
    let msg = match args.get(1) {
        Some(Object::Str(s)) => s.as_ref().to_owned(),
        _ => String::new(),
    };
    flush_std_streams();
    py_fatal_error_and_abort(&format!("{func}: {msg}"), Some(current_ident()));
}

/// `faulthandler._fatal_error_c_thread()` — CPython spawns a bare C
/// thread that calls `Py_FatalError("in new thread")`; from that thread
/// no tstate is current, so every block in the dump reads `Thread 0x…`.
fn fh_fatal_error_c_thread(_args: &[Object]) -> Result<Object, RuntimeError> {
    flush_std_streams();
    std::thread::spawn(|| {
        py_fatal_error_and_abort("faulthandler_fatal_error_thread: in new thread", None);
    });
    // CPython blocks the calling thread on a never-released lock; the C
    // thread aborts the whole process.
    loop {
        std::thread::sleep(Duration::from_hours(1));
    }
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
    // `SIGBUS` is POSIX-only; on Windows raise `SIGSEGV` so the crash primitive
    // still terminates the process.
    #[cfg(unix)]
    let sig = libc::SIGBUS;
    #[cfg(not(unix))]
    let sig = libc::SIGSEGV;
    unsafe {
        libc::raise(sig);
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
    let msg = match args.first() {
        Some(Object::Str(s)) => s.as_ref().to_owned(),
        Some(Object::Bytes(b)) => String::from_utf8_lossy(b).into_owned(),
        _ => String::new(),
    };
    flush_std_streams();
    py_fatal_error_and_abort(
        &format!("faulthandler_fatal_error_py: {msg}"),
        Some(current_ident()),
    );
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

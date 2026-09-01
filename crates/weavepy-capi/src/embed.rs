//! RFC 0075 WS1/WS3/WS4 — the C embedding lifecycle.
//!
//! Historically `Py_Initialize` was a shim: the host binary *was* the
//! interpreter, so "initialize" meant "wire the static type bridges".
//! This module gives the C-API an **owned-interpreter mode**: when a
//! plain C program (or a `libpython313`-linking embedder) calls
//! `Py_Initialize` / `Py_InitializeFromConfig` and no WeavePy host is
//! running, the capi crate constructs a real
//! [`weavepy_vm::Interpreter`], publishes it exactly as the CLI does,
//! and `Py_FinalizeEx` later tears it down — atexit callbacks,
//! non-daemon thread join, shutdown finalizers, stream flush — leaving
//! the process re-initialisable (init → fini → init cycles are the
//! `test_embed` bread and butter).
//!
//! Host mode (the `weavepy` CLI, the conformance runners) is
//! unchanged: `Py_Initialize` from an extension stays a bridge-wiring
//! no-op and `Py_Finalize*` does not tear down an interpreter the
//! embedding layer does not own.

use std::os::raw::{c_char, c_int};
use std::sync::atomic::{AtomicPtr, Ordering};
use std::sync::Mutex;

use weavepy_vm::object::{DictKey, Object};
use weavepy_vm::{Interpreter, InterpreterFlags};

use crate::initconfig::{EmbedConfig, PyStatus};
use crate::object::PyObject;

// ---------------------------------------------------------------------------
// Lifecycle state
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum State {
    Uninitialised,
    /// Initialized by an embedder; the interpreter in [`OWNED`] is ours.
    OwnedInit,
    /// A WeavePy host (CLI/test harness) is the interpreter; lifecycle
    /// calls are bridge-wiring no-ops.
    HostInit,
    Finalising,
}

static STATE: Mutex<State> = Mutex::new(State::Uninitialised);
static OWNED: AtomicPtr<Interpreter> = AtomicPtr::new(std::ptr::null_mut());

/// The decoded config `Py_InitializeFromConfig` ran with — consumed by
/// [`run_main`] for its `run_command` / `run_module` / `run_filename`.
static RUN_CONFIG: Mutex<Option<EmbedConfig>> = Mutex::new(None);

/// Truthful `Py_IsInitialized`: owned or host interpreter live.
pub fn is_initialized() -> bool {
    match *STATE.lock().unwrap() {
        State::OwnedInit | State::HostInit => true,
        State::Finalising => false,
        State::Uninitialised => {
            // A host interpreter that never called Py_Initialize (the
            // CLI does not) still counts: extensions probe this.
            weavepy_vm::vm_singletons::current_interpreter_ptr().is_some()
                || crate::interp::effective_interpreter_mut().is_some()
        }
    }
}

/// Run `body` with a generous stack. Embedder main threads carry the
/// platform default (8 MiB), which is not enough for `site`
/// bootstrap in unoptimized builds; the CLI reserves 1 GiB up front,
/// and this is the embedding twin of that reservation.
fn with_embed_stack<R>(body: impl FnOnce() -> R) -> R {
    stacker::grow(64 * 1024 * 1024, body)
}

/// Map the PEP 587 config onto the VM's flag set.
fn flags_from(config: &EmbedConfig) -> InterpreterFlags {
    // `PYTHONIOENCODING=<encoding>[:<errors>]` fills whichever stdio
    // half the embedder left unset (CPython's config_init_stdio_encoding).
    // The C `PyConfig` path gets this from `PyConfig_Read`; a Rust-level
    // `EmbedConfig` (the `_testembed` twin, `Py_Initialize` defaults)
    // arrives unread, so the fill lives here too.
    let mut io_encoding = config.stdio_encoding.clone();
    let mut io_errors = config.stdio_errors.clone();
    if config.use_environment {
        let (env_enc, env_err) = crate::initconfig::env_pythonioencoding();
        if io_encoding.is_none() {
            io_encoding = env_enc;
        }
        if io_errors.is_none() {
            io_errors = env_err;
        }
    }
    InterpreterFlags {
        optimize: config.optimization_level,
        dont_write_bytecode: !config.write_bytecode,
        inspect: config.inspect,
        verbose: config.verbose,
        no_site: !config.site_import,
        no_user_site: !config.user_site_directory,
        ignore_environment: !config.use_environment,
        isolated: config.isolated,
        quiet: config.quiet,
        unbuffered: !config.buffered_stdio,
        skip_first_line: config.skip_source_first_line,
        bytes_warning: config.bytes_warning,
        safe_path: config.safe_path,
        xoptions: config.xoptions.clone(),
        warning_filters: config.warnoptions.clone(),
        io_encoding,
        io_errors,
        utf8_mode: None,
        pycache_prefix: config.pycache_prefix.clone(),
        int_max_str_digits: config.int_max_str_digits,
        cpu_count: None,
        tracemalloc: config.tracemalloc,
        faulthandler: config.faulthandler,
        ..InterpreterFlags::default()
    }
}

/// Point the stdlib resolver at the embedder's home before the
/// interpreter is constructed. CPython's getpath honours
/// `PyConfig.home`, then `PYTHONHOME`, then — when the program is a
/// foreign embedder binary whose ancestors carry no landmark —
/// computes the prefix from the *shared library's* location. The
/// WS5 twin of that last step is a dladdr probe over this module's
/// own address: `libpython3.13.{so,dylib}` ships inside the artifact
/// at `{prefix}/lib/`, so its on-disk path self-locates the stdlib
/// for any embedder anywhere on the filesystem. The result is
/// published through `WEAVEPYHOME`, the resolver's highest-priority
/// input (set before the first `stdlib_dir()` call caches).
fn point_stdlib_home(config: &EmbedConfig) {
    if let Some(home) = &config.home {
        std::env::set_var("WEAVEPYHOME", home);
        return;
    }
    let has = |k: &str| std::env::var_os(k).is_some_and(|v| !v.is_empty());
    if has("WEAVEPYHOME") || has("PYTHONHOME") {
        return;
    }
    #[cfg(unix)]
    if let Some(prefix) = library_home_prefix() {
        std::env::set_var("WEAVEPYHOME", prefix);
    }
}

/// The installation prefix implied by the shared object this code
/// lives in (`{prefix}/lib/libpython3.13.*` → `{prefix}`), walking
/// ancestors so a statically linked embedder whose exe sits in
/// `{prefix}/bin` resolves too. `None` when no ancestor carries a
/// complete stdlib tree — the resolver then falls back to its own
/// exe-relative walk and materialize cache.
#[cfg(unix)]
fn library_home_prefix() -> Option<std::path::PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    let mut info: libc::Dl_info = unsafe { std::mem::zeroed() };
    let addr = point_stdlib_home as *const std::ffi::c_void;
    if unsafe { libc::dladdr(addr, &mut info) } == 0 || info.dli_fname.is_null() {
        return None;
    }
    let fname = unsafe { std::ffi::CStr::from_ptr(info.dli_fname) };
    let path = std::path::Path::new(std::ffi::OsStr::from_bytes(fname.to_bytes()));
    let path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    path.ancestors()
        .skip(1)
        .find(|dir| weavepy_vm::stdlib_tree::is_home_prefix(dir))
        .map(std::path::Path::to_path_buf)
}

/// The embedding initializer behind `Py_Initialize`,
/// `Py_InitializeEx`, and `Py_InitializeFromConfig`.
pub fn initialize(config: Option<EmbedConfig>) -> PyStatus {
    let mut state = STATE.lock().unwrap();
    match *state {
        State::OwnedInit | State::HostInit => return PyStatus::OK, // idempotent
        State::Finalising => return PyStatus::error("Py_Initialize: interpreter is finalizing"),
        State::Uninitialised => {}
    }
    // Wire the static bridges first in every mode.
    crate::interp::ensure_initialised();

    // Host detection: a live published interpreter means the process
    // is already a WeavePy host — do not construct a second runtime.
    if weavepy_vm::vm_singletons::current_interpreter_ptr().is_some()
        || crate::interp::effective_interpreter_mut().is_some()
    {
        *state = State::HostInit;
        return PyStatus::OK;
    }

    let config = config.unwrap_or_else(|| EmbedConfig {
        use_environment: true,
        site_import: true,
        user_site_directory: true,
        write_bytecode: true,
        buffered_stdio: true,
        install_signal_handlers: true,
        argv: vec![String::new()],
        ..EmbedConfig::default()
    });

    point_stdlib_home(&config);
    // getpath seeds program_full_path (→ sys.executable) from the
    // configured program name, not the host process's argv[0].
    weavepy_vm::stdlib_tree::set_program_name_override(
        config.program_name.as_deref().map(std::path::PathBuf::from),
    );
    let interp = with_embed_stack(|| {
        crate::loader::install_vm_extension_loader();
        weavepy_vm::install_parser_unicode_hook();
        let mut interp = Box::new(Interpreter::default());
        interp.apply_run_options(&flags_from(&config));
        if let Some(paths) = &config.module_search_paths {
            for p in paths {
                interp.append_path(std::path::PathBuf::from(p));
            }
        }
        if let Some(pp) = &config.pythonpath_env {
            let sep = if cfg!(windows) { ';' } else { ':' };
            for p in pp.split(sep).filter(|p| !p.is_empty()) {
                interp.append_path(std::path::PathBuf::from(p));
            }
        }
        let argv = if config.argv.is_empty() {
            vec![String::new()]
        } else {
            config.argv.clone()
        };
        interp.set_argv(argv);
        if config.site_import {
            // Best-effort, like CPython: a broken `.pth` file must not
            // abort embedding.
            let _ = interp.run_site();
        }
        interp
    });

    crate::initconfig::record_orig_argv(if config.orig_argv.is_empty() {
        &config.argv
    } else {
        &config.orig_argv
    });
    let ptr = Box::into_raw(interp);
    OWNED.store(ptr, Ordering::SeqCst);
    crate::interp::note_interpreter(ptr);
    *RUN_CONFIG.lock().unwrap() = Some(config);
    *state = State::OwnedInit;
    PyStatus::OK
}

/// The owned interpreter, if the embedding layer created one.
pub fn owned_interpreter() -> Option<*mut Interpreter> {
    let p = OWNED.load(Ordering::SeqCst);
    if p.is_null() {
        None
    } else {
        Some(p)
    }
}

// ---------------------------------------------------------------------------
// Py_AtExit — the 32-slot C callback table
// ---------------------------------------------------------------------------

type AtExitFn = unsafe extern "C" fn();

/// CPython caps the table at 32 (`NEXITFUNCS`); registration past
/// that fails with -1.
const NEXITFUNCS: usize = 32;

struct AtExitTable(Vec<AtExitFn>);
// SAFETY: plain fn pointers behind a mutex.
unsafe impl Send for AtExitTable {}

static ATEXIT_C: Mutex<AtExitTable> = Mutex::new(AtExitTable(Vec::new()));

#[no_mangle]
pub unsafe extern "C" fn Py_AtExit(func: Option<AtExitFn>) -> c_int {
    let Some(func) = func else { return -1 };
    let mut table = ATEXIT_C.lock().unwrap();
    if table.0.len() >= NEXITFUNCS {
        return -1;
    }
    table.0.push(func);
    0
}

fn run_c_atexit_table() {
    // LIFO, drained so an init→fini→init cycle starts fresh.
    let funcs = std::mem::take(&mut ATEXIT_C.lock().unwrap().0);
    for f in funcs.into_iter().rev() {
        unsafe { f() };
    }
}

// ---------------------------------------------------------------------------
// Finalization
// ---------------------------------------------------------------------------

/// The real `Py_FinalizeEx` body. Returns 0, or -1 when flushing the
/// std streams failed (the documented contract). Host mode is a
/// no-op: the host owns its own shutdown drain.
pub fn finalize() -> c_int {
    let mut state = STATE.lock().unwrap();
    match *state {
        State::OwnedInit => {}
        State::HostInit => {
            // The host (CLI) drives its own shutdown; an extension's
            // defensive Py_Finalize must not tear it down.
            return 0;
        }
        State::Uninitialised | State::Finalising => return 0,
    }
    *state = State::Finalising;
    drop(state); // atexit callbacks may re-enter capi entry points

    let ptr = OWNED.swap(std::ptr::null_mut(), Ordering::SeqCst);
    let mut ok = true;
    if !ptr.is_null() {
        // SAFETY: we are the unique owner; embedder threads calling in
        // during fini are the same hazard CPython documents (and the
        // Arc heap makes object access safe even then).
        let mut interp = unsafe { Box::from_raw(ptr) };
        with_embed_stack(|| {
            // 1. threading._shutdown() — join non-daemon threads —
            //    then the Python atexit callbacks. The VM bundles the
            //    two in its shutdown drain, matching CPython's order.
            interp.run_interpreter_shutdown();
            // 2. The Py_AtExit C table (LIFO, post-Python-atexit).
            run_c_atexit_table();
            // 3. Shutdown finalizers (module-global __del__ etc.).
            interp.run_shutdown_finalizers();
            // 4. Flush and report: a failed stdout flush is exit
            //    status material for Py_FinalizeEx's caller.
            ok = interp.flush_streams();
        });
        drop(interp);
    }
    crate::interp::clear_interpreter();
    weavepy_vm::stdlib_tree::set_program_name_override(None);
    *RUN_CONFIG.lock().unwrap() = None;
    *STATE.lock().unwrap() = State::Uninitialised;
    if ok {
        0
    } else {
        -1
    }
}

// ---------------------------------------------------------------------------
// Py_RunMain
// ---------------------------------------------------------------------------

/// Fetch a builtin callable from the owned interpreter.
fn builtin_of(interp: &mut Interpreter, name: &'static str) -> Option<Object> {
    let builtins = interp.builtins_dict();
    let d = builtins.borrow();
    d.get(&DictKey(Object::from_static(name))).cloned()
}

/// Exit-code semantics for a completed top-level run: `SystemExit`
/// honoured, anything else printed CPython-style with code 1.
fn conclude(interp: &mut Interpreter, result: Result<Object, weavepy_vm::RuntimeError>) -> c_int {
    match result {
        Ok(_) => 0,
        Err(weavepy_vm::RuntimeError::PyException(exc)) => {
            if let Some(code) = exc.system_exit_code() {
                return match &code {
                    Object::None => 0,
                    Object::Int(i) => *i as c_int,
                    Object::Bool(b) => c_int::from(*b),
                    other => {
                        let msg = interp
                            .str_object(other)
                            .unwrap_or_else(|_| "<exit>".to_owned());
                        eprintln!("{msg}");
                        1
                    }
                };
            }
            if !interp.print_uncaught_exception(&exc) {
                eprintln!("{}: {}", exc.type_name(), exc.message());
            }
            1
        }
        Err(err) => {
            eprintln!("InternalError: {err}");
            1
        }
    }
}

/// Execute `source` as the `__main__` module body of the owned
/// interpreter (the `PyRun_SimpleString` / `run_command` engine).
/// `filename` feeds `__file__` when the source came from a real file.
pub fn exec_in_main(
    interp: &mut Interpreter,
    source: &str,
    filename: Option<&str>,
) -> Result<Object, weavepy_vm::RuntimeError> {
    let main_dict = crate::pythonrun::main_module_dict(interp)?;
    if let Some(f) = filename {
        main_dict.borrow_mut().insert(
            DictKey(Object::from_static("__file__")),
            Object::from_str(f.to_owned()),
        );
    }
    let compile = builtin_of(interp, "compile").ok_or_else(|| {
        weavepy_vm::error::runtime_error("embedding: compile builtin unavailable")
    })?;
    let exec = builtin_of(interp, "exec")
        .ok_or_else(|| weavepy_vm::error::runtime_error("embedding: exec builtin unavailable"))?;
    let code = interp.call_object(
        compile,
        &[
            Object::from_str(source.to_owned()),
            Object::from_str(filename.unwrap_or("<string>").to_owned()),
            Object::from_static("exec"),
        ],
        &[],
    )?;
    interp.call_object(
        exec,
        &[
            code,
            Object::Dict(main_dict.clone()),
            Object::Dict(main_dict),
        ],
        &[],
    )
}

/// `Py_RunMain()` — run the config's command/module/filename in the
/// owned interpreter, finalize, and return the exit code.
#[no_mangle]
pub unsafe extern "C" fn Py_RunMain() -> c_int {
    let config = RUN_CONFIG.lock().unwrap().clone();
    let Some(config) = config else {
        eprintln!("Fatal Python error: Py_RunMain: interpreter not initialized from config");
        return 1;
    };
    let Some(ptr) = owned_interpreter() else {
        eprintln!("Fatal Python error: Py_RunMain: no owned interpreter");
        return 1;
    };
    // SAFETY: owned pointer, exclusive by the embedding contract
    // (Py_RunMain is called from the thread that initialized).
    let interp = unsafe { &mut *ptr };
    let code = with_embed_stack(|| run_main_body(interp, &config));
    let fini = finalize();
    if code == 0 && fini != 0 {
        // CPython: a failed finalize flush turns a clean run into 120.
        return 120;
    }
    code
}

fn run_main_body(interp: &mut Interpreter, config: &EmbedConfig) -> c_int {
    if let Some(cmd) = &config.run_command {
        let r = exec_in_main(interp, cmd, None);
        return conclude(interp, r);
    }
    if let Some(module) = &config.run_module {
        // runpy handles sys.argv[0] rewriting and pkg __main__ exactly
        // like `python -m`.
        let snippet =
            format!("import runpy as _weavepy_rp\n_weavepy_rp._run_module_as_main({module:?})\n");
        let r = exec_in_main(interp, &snippet, None);
        return conclude(interp, r);
    }
    if let Some(filename) = &config.run_filename {
        let source = match std::fs::read_to_string(filename) {
            Ok(mut s) => {
                if config.skip_source_first_line {
                    s = s
                        .split_once('\n')
                        .map(|x| x.1.to_owned())
                        .unwrap_or_default();
                }
                s
            }
            Err(e) => {
                let prog = config
                    .program_name
                    .clone()
                    .unwrap_or_else(|| "python3".to_owned());
                eprintln!("{prog}: can't open file '{filename}': {e}");
                return 2;
            }
        };
        let r = exec_in_main(interp, &source, Some(filename));
        return conclude(interp, r);
    }
    // No command/module/filename: CPython falls back to stdin (REPL on
    // a tty). The embedding twin reads piped stdin; an interactive
    // embedder wanting the full REPL calls PyRun_InteractiveLoop.
    use std::io::Read;
    let mut buf = String::new();
    if std::io::stdin().read_to_string(&mut buf).is_ok() && !buf.is_empty() {
        let r = exec_in_main(interp, &buf, Some("<stdin>"));
        return conclude(interp, r);
    }
    0
}

// ---------------------------------------------------------------------------
// PyImport_AppendInittab / PyImport_ExtendInittab
// ---------------------------------------------------------------------------

pub type PyInitFn = unsafe extern "C" fn() -> *mut PyObject;

/// CPython's `struct _inittab`.
#[repr(C)]
pub struct PyInittabEntry {
    pub name: *const c_char,
    pub initfunc: Option<PyInitFn>,
}

struct Inittab(Vec<(String, PyInitFn)>);
// SAFETY: fn pointers + owned strings behind a mutex.
unsafe impl Send for Inittab {}

static INITTAB: Mutex<Inittab> = Mutex::new(Inittab(Vec::new()));

/// Register an inittab entry from Rust (the PEP 741
/// `PyInitConfig_AddModule` path; pre-init only, like the C entry
/// points).
pub(crate) fn inittab_push(name: String, initfunc: PyInitFn) -> bool {
    if matches!(*STATE.lock().unwrap(), State::OwnedInit | State::HostInit) {
        return false;
    }
    INITTAB.lock().unwrap().0.push((name, initfunc));
    true
}

/// A clone of the config `Py_InitializeFromConfig` ran with, for the
/// PEP 741 runtime read surface (`PyConfig_Get`).
pub(crate) fn run_config_snapshot() -> Option<EmbedConfig> {
    RUN_CONFIG.lock().unwrap().clone()
}

/// Look up an embedder-registered built-in module init function.
pub fn inittab_lookup(name: &str) -> Option<PyInitFn> {
    INITTAB
        .lock()
        .unwrap()
        .0
        .iter()
        .find(|(n, _)| n == name)
        .map(|(_, f)| *f)
}

#[no_mangle]
pub unsafe extern "C" fn PyImport_AppendInittab(
    name: *const c_char,
    initfunc: Option<PyInitFn>,
) -> c_int {
    if name.is_null() {
        return -1;
    }
    // CPython refuses inittab mutation after Py_Initialize.
    if matches!(*STATE.lock().unwrap(), State::OwnedInit | State::HostInit) {
        return -1;
    }
    let Some(initfunc) = initfunc else { return -1 };
    let name = unsafe { std::ffi::CStr::from_ptr(name) }
        .to_string_lossy()
        .into_owned();
    INITTAB.lock().unwrap().0.push((name, initfunc));
    0
}

#[no_mangle]
pub unsafe extern "C" fn PyImport_ExtendInittab(newtab: *mut PyInittabEntry) -> c_int {
    if newtab.is_null() {
        return -1;
    }
    if matches!(*STATE.lock().unwrap(), State::OwnedInit | State::HostInit) {
        return -1;
    }
    let mut i = 0usize;
    loop {
        // SAFETY: the table is NULL-name terminated per CPython.
        let entry = unsafe { &*newtab.add(i) };
        if entry.name.is_null() {
            break;
        }
        if let Some(f) = entry.initfunc {
            let name = unsafe { std::ffi::CStr::from_ptr(entry.name) }
                .to_string_lossy()
                .into_owned();
            INITTAB.lock().unwrap().0.push((name, f));
        }
        i += 1;
    }
    0
}

// ---------------------------------------------------------------------------
// Py_NewInterpreter / Py_EndInterpreter — PEP 684 from C
// ---------------------------------------------------------------------------

use crate::lifecycle::PyThreadState;

// The stack of sub-interpreter ids created through the C API on this
// thread. `Py_NewInterpreter` pushes; `Py_EndInterpreter` pops.
// PyRun_* entry points consult the top to route execution.
thread_local! {
    static EMBED_SUBINTERPS: std::cell::RefCell<Vec<i64>> = const { std::cell::RefCell::new(Vec::new()) };
}

pub fn current_embed_subinterp() -> Option<i64> {
    EMBED_SUBINTERPS.with(|s| s.borrow().last().copied())
}

/// Call `_xxsubinterpreters.<method>(args…)` through the C-API's own
/// import + call surface (exactly what an extension would do).
unsafe fn call_xxsubinterpreters(method: &str, args: &[Object]) -> Option<Object> {
    let module = unsafe {
        crate::module::PyImport_ImportModule(b"_xxsubinterpreters\0".as_ptr() as *const c_char)
    };
    if module.is_null() {
        return None;
    }
    let module_obj = unsafe { crate::object::clone_object(module) };
    unsafe { crate::object::Py_DecRef(module) };
    let Object::Module(m) = module_obj else {
        return None;
    };
    let func = m
        .dict
        .borrow()
        .get(&DictKey(Object::from_str(method.to_owned())))
        .cloned()?;
    crate::interp::with_interp_mut(|interp| interp.call_object(func, args, &[]).ok())?
}

#[no_mangle]
pub unsafe extern "C" fn Py_NewInterpreter() -> *mut PyThreadState {
    if !is_initialized() {
        return std::ptr::null_mut();
    }
    let created = unsafe { call_xxsubinterpreters("create", &[]) };
    let Some(id_obj) = created else {
        return std::ptr::null_mut();
    };
    let id = match id_obj {
        Object::Int(i) => i,
        other => match crate::interp::with_interp_mut(|interp| {
            interp
                .str_object(&other)
                .ok()
                .and_then(|s| s.parse::<i64>().ok())
        })
        .flatten()
        {
            Some(i) => i,
            None => return std::ptr::null_mut(),
        },
    };
    EMBED_SUBINTERPS.with(|s| s.borrow_mut().push(id));
    // The returned tstate is the calling thread's state; the routing
    // to the sub-interpreter happens through the thread-local stack.
    crate::pystate::current_threadstate()
}

#[no_mangle]
pub unsafe extern "C" fn Py_EndInterpreter(_tstate: *mut PyThreadState) {
    let popped = EMBED_SUBINTERPS.with(|s| s.borrow_mut().pop());
    let Some(id) = popped else {
        // CPython fatals when handed the main interpreter; the twin
        // reports and continues (the Arc heap makes this safe).
        eprintln!("Py_EndInterpreter: not a sub-interpreter thread state");
        return;
    };
    unsafe { call_xxsubinterpreters("destroy", &[Object::Int(id)]) };
}

/// `PyInterpreterConfig` (3.12+): accepted, with own-GIL requests
/// coerced to the shared GIL (WeavePy has one GIL; PEP 684 isolation
/// semantics are what consumers depend on).
#[repr(C)]
pub struct PyInterpreterConfig {
    pub use_main_obmalloc: c_int,
    pub allow_fork: c_int,
    pub allow_exec: c_int,
    pub allow_threads: c_int,
    pub allow_daemon_threads: c_int,
    pub check_multi_interp_extensions: c_int,
    pub gil: c_int,
}

#[no_mangle]
pub unsafe extern "C" fn Py_NewInterpreterFromConfig(
    tstate_p: *mut *mut PyThreadState,
    _config: *const PyInterpreterConfig,
) -> PyStatus {
    if tstate_p.is_null() {
        return PyStatus::error("Py_NewInterpreterFromConfig: NULL tstate pointer");
    }
    let ts = unsafe { Py_NewInterpreter() };
    if ts.is_null() {
        return PyStatus::error("interpreter creation failed");
    }
    unsafe { *tstate_p = ts };
    PyStatus::OK
}

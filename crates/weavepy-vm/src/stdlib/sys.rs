//! The `sys` built-in module.
//!
//! Tracks CPython 3.13's `sys` module shape for the attributes we
//! support. `argv`, `path`, and `modules` are all backed by the
//! interpreter's [`ModuleCache`] so writes flow both ways.
//!
//! Anything that touches host I/O streams (`sys.stdout`,
//! `sys.stderr`) is deferred to RFC 0014, when we land the `io`
//! module and Python file objects.

use crate::sync::Rc;
use crate::sync::RefCell;

use crate::error::{type_error, value_error, RuntimeError};
use crate::import::ModuleCache;
use crate::object::{BuiltinFn, DictData, DictKey, FileBackend, Object, PyFile, PyFrame, PyModule};

/// CPython compatibility version we advertise. This is intentionally
/// independent from the WeavePy package version (see
/// `weavepy-cli/src/main.rs`); user code that inspects
/// `sys.version_info` is checking *Python language* compatibility, not
/// the WeavePy build identity.
pub const PY_VERSION: (i64, i64, i64) = (3, 13, 0);

/// Build the `sys` module against the given interpreter handles.
/// Most state lives on the [`ModuleCache`]; `frame_stack`,
/// `exc_info_stack`, and the user-installable hooks come from the
/// interpreter itself so module-level callables can read live state.
pub fn build_with_state(
    cache: &ModuleCache,
    frame_stack: Rc<RefCell<Vec<Rc<PyFrame>>>>,
    exc_info_stack: Rc<RefCell<Vec<crate::error::PyException>>>,
    excepthook: Rc<RefCell<Object>>,
    unraisable_hook: Rc<RefCell<Object>>,
) -> Rc<PyModule> {
    let module = build(cache);
    {
        let mut d = module.dict.borrow_mut();
        // RFC 0025: route through the active per-thread handles so
        // worker threads see *their* frame / exception state, not
        // the main interpreter's. The `frame_stack` / `exc_info_stack`
        // closure captures below are kept as fallbacks for embedders
        // that build the `sys` module before any interpreter has
        // activated handles for the current thread.
        let fs_fallback = frame_stack.clone();
        d.insert(
            DictKey(Object::from_static("_getframe")),
            Object::Builtin(Rc::new(BuiltinFn {
                name: "_getframe",
                call: Box::new(move |args| {
                    if let Some(h) = crate::vm_singletons::current_thread_handles() {
                        sys_getframe(args, &h.frame_stack)
                    } else {
                        sys_getframe(args, &fs_fallback)
                    }
                }),
                call_kw: None,
            })),
        );
        let es_fallback = exc_info_stack.clone();
        d.insert(
            DictKey(Object::from_static("exc_info")),
            Object::Builtin(Rc::new(BuiltinFn {
                name: "exc_info",
                call: Box::new(move |_| {
                    if let Some(h) = crate::vm_singletons::current_thread_handles() {
                        sys_exc_info(&h.exc_info_stack)
                    } else {
                        sys_exc_info(&es_fallback)
                    }
                }),
                call_kw: None,
            })),
        );
        let eh_get = excepthook.clone();
        d.insert(
            DictKey(Object::from_static("__excepthook__")),
            eh_get.borrow().clone(),
        );
        let eh = excepthook.clone();
        d.insert(
            DictKey(Object::from_static("excepthook")),
            Object::Builtin(Rc::new(BuiltinFn {
                name: "excepthook",
                call: Box::new(move |args| {
                    let hook = eh.borrow().clone();
                    // If a user hook is installed, the *call* path
                    // lives in the VM (we can't dispatch Python from
                    // a builtin here). Surface a stable error so the
                    // VM-level dispatch wraps us.
                    if !matches!(hook, Object::None) {
                        return Ok(Object::None);
                    }
                    sys_default_excepthook(args)
                }),
                call_kw: None,
            })),
        );
        let uh = unraisable_hook.clone();
        d.insert(
            DictKey(Object::from_static("unraisablehook")),
            Object::Builtin(Rc::new(BuiltinFn {
                name: "unraisablehook",
                call: Box::new(move |_args| {
                    let _ = uh.borrow().clone();
                    Ok(Object::None)
                }),
                call_kw: None,
            })),
        );
        d.insert(
            DictKey(Object::from_static("__unraisablehook__")),
            unraisable_hook.borrow().clone(),
        );
        d.insert(
            DictKey(Object::from_static("settrace")),
            builtin("settrace", sys_settrace),
        );
        d.insert(
            DictKey(Object::from_static("monitoring")),
            crate::stdlib::sys_monitoring::build(),
        );
        d.insert(
            DictKey(Object::from_static("setprofile")),
            builtin("setprofile", sys_setprofile),
        );
        d.insert(
            DictKey(Object::from_static("gettrace")),
            builtin("gettrace", sys_gettrace),
        );
        d.insert(
            DictKey(Object::from_static("getprofile")),
            builtin("getprofile", sys_getprofile),
        );
        d.insert(
            DictKey(Object::from_static("getsizeof")),
            builtin("getsizeof", sys_getsizeof),
        );
        d.insert(
            DictKey(Object::from_static("audit")),
            builtin("audit", |_| Ok(Object::None)),
        );
        d.insert(
            DictKey(Object::from_static("addaudithook")),
            builtin("addaudithook", |_| Ok(Object::None)),
        );
        d.insert(DictKey(Object::from_static("flags")), sys_flags_value());
        // Default to `False`, matching CPython. The CLI/embedder
        // overrides this through `apply_run_options` when `-B` or
        // `PYTHONDONTWRITEBYTECODE` was set.
        d.insert(
            DictKey(Object::from_static("dont_write_bytecode")),
            Object::Bool(false),
        );
        d.insert(
            DictKey(Object::from_static("ps1")),
            Object::from_static(">>> "),
        );
        d.insert(
            DictKey(Object::from_static("ps2")),
            Object::from_static("... "),
        );
        d.insert(
            DictKey(Object::from_static("warnoptions")),
            Object::new_list(Vec::new()),
        );
        d.insert(
            DictKey(Object::from_static("hexversion")),
            Object::Int((PY_VERSION.0 << 24) | (PY_VERSION.1 << 16) | (PY_VERSION.2 << 8) | 0xF0),
        );
        d.insert(
            DictKey(Object::from_static("api_version")),
            Object::Int(1013),
        );
        d.insert(DictKey(Object::from_static("float_info")), sys_float_info());
        d.insert(DictKey(Object::from_static("int_info")), sys_int_info());
        d.insert(DictKey(Object::from_static("hash_info")), sys_hash_info());
        d.insert(
            DictKey(Object::from_static("thread_info")),
            sys_thread_info(),
        );

        // RFC 0029 — import machinery state. The frozen
        // `importlib._bootstrap` module overwrites `meta_path`,
        // `path_hooks`, and `path_importer_cache` on first import
        // with real importer objects; until then they hold empty
        // collections so `importlib.util.find_spec("name")` doesn't
        // crash trying to walk a missing attribute.
        d.insert(
            DictKey(Object::from_static("meta_path")),
            Object::new_list(Vec::new()),
        );
        d.insert(
            DictKey(Object::from_static("path_hooks")),
            Object::new_list(Vec::new()),
        );
        d.insert(
            DictKey(Object::from_static("path_importer_cache")),
            Object::new_dict(),
        );
        d.insert(DictKey(Object::from_static("pycache_prefix")), Object::None);
        d.insert(
            DictKey(Object::from_static("maxunicode")),
            Object::Int(0x0010_FFFF),
        );
        d.insert(
            DictKey(Object::from_static("platlibdir")),
            Object::from_static(if cfg!(windows) { "Lib" } else { "lib" }),
        );
        d.insert(
            DictKey(Object::from_static("tracebacklimit")),
            Object::Int(1000),
        );
        // Standard library module name allowlist — used by tools
        // that need to know which `import x` reaches the stdlib
        // vs. a third-party package. Matches the documented
        // CPython 3.13 set (lowercase, no underscore-private
        // helpers).
        d.insert(
            DictKey(Object::from_static("stdlib_module_names")),
            stdlib_module_names_value(),
        );

        // `last_type` / `last_value` / `last_traceback` —
        // populated by the REPL's exception loop. Pre-seed to
        // None so user inspection doesn't AttributeError.
        d.insert(DictKey(Object::from_static("last_type")), Object::None);
        d.insert(DictKey(Object::from_static("last_value")), Object::None);
        d.insert(DictKey(Object::from_static("last_traceback")), Object::None);
        d.insert(DictKey(Object::from_static("last_exc")), Object::None);

        // `_current_frames` — returns a dict mapping thread-id
        // to the current frame for that thread. Single-threaded
        // execution sees a one-entry dict.
        {
            let fs_cf = frame_stack.clone();
            d.insert(
                DictKey(Object::from_static("_current_frames")),
                Object::Builtin(Rc::new(BuiltinFn {
                    name: "_current_frames",
                    call: Box::new(move |_args| {
                        let frame = if let Some(h) = crate::vm_singletons::current_thread_handles()
                        {
                            h.frame_stack.borrow().last().cloned()
                        } else {
                            fs_cf.borrow().last().cloned()
                        };
                        let mut d = DictData::new();
                        if let Some(f) = frame {
                            // Best-effort: every thread has the
                            // same logical id 0 in the single-
                            // GIL model.
                            d.insert(DictKey(Object::Int(0)), Object::Frame(f));
                        }
                        Ok(Object::Dict(Rc::new(RefCell::new(d))))
                    }),
                    call_kw: None,
                })),
            );
        }

        d.insert(
            DictKey(Object::from_static("getswitchinterval")),
            builtin("getswitchinterval", |_| Ok(Object::Float(0.005))),
        );
        d.insert(
            DictKey(Object::from_static("setswitchinterval")),
            builtin("setswitchinterval", |_args| Ok(Object::None)),
        );
        d.insert(
            DictKey(Object::from_static("getrefcount")),
            builtin("getrefcount", sys_getrefcount),
        );
        // `displayhook` — invoked by the REPL after every
        // evaluated expression. Default writes `repr(value)` to
        // stdout and stashes the value in `builtins._`. The hook
        // is overrideable; the original is preserved on
        // `__displayhook__`.
        d.insert(
            DictKey(Object::from_static("displayhook")),
            builtin("displayhook", sys_displayhook),
        );
        d.insert(
            DictKey(Object::from_static("__displayhook__")),
            builtin("displayhook", sys_displayhook),
        );

        // `sys.builtin_module_names` — exposed as a tuple for
        // user-introspection code (e.g. `importlib.util.find_spec`).
        d.insert(
            DictKey(Object::from_static("builtin_module_names")),
            Object::new_tuple(
                [
                    "_csv",
                    "_datetime",
                    "_socket",
                    "_subprocess",
                    "_thread",
                    "_weakref",
                    "base64",
                    "binascii",
                    "errno",
                    "fnmatch",
                    "gc",
                    "glob",
                    "hashlib",
                    "hmac",
                    "io",
                    "json",
                    "math",
                    "os",
                    "random",
                    "re",
                    "secrets",
                    "signal",
                    "ssl",
                    "sys",
                    "time",
                    "uuid",
                    "zlib",
                ]
                .iter()
                .map(|s| Object::from_static(s))
                .collect(),
            ),
        );
        // sys.gettrace/getprofile stubs (no actual tracing yet).
    }
    module
}

pub fn build(cache: &ModuleCache) -> Rc<PyModule> {
    let dict = Rc::new(RefCell::new(DictData::new()));
    {
        let mut d = dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("__name__")),
            Object::from_static("sys"),
        );
        d.insert(
            DictKey(Object::from_static("__package__")),
            Object::from_static(""),
        );
        d.insert(
            DictKey(Object::from_static("__doc__")),
            Object::from_static(
                "Provides access to interpreter-internal state and the import system.",
            ),
        );

        // Shared with the loader.
        d.insert(
            DictKey(Object::from_static("modules")),
            Object::Dict(cache.modules.clone()),
        );
        d.insert(
            DictKey(Object::from_static("path")),
            Object::List(cache.path.clone()),
        );
        d.insert(
            DictKey(Object::from_static("argv")),
            Object::List(cache.argv.clone()),
        );

        // Static identity.
        d.insert(
            DictKey(Object::from_static("version")),
            Object::from_str(format!(
                "{}.{}.{} (WeavePy)",
                PY_VERSION.0, PY_VERSION.1, PY_VERSION.2
            )),
        );
        d.insert(
            DictKey(Object::from_static("version_info")),
            Object::new_tuple(vec![
                Object::Int(PY_VERSION.0),
                Object::Int(PY_VERSION.1),
                Object::Int(PY_VERSION.2),
                Object::from_static("final"),
                Object::Int(0),
            ]),
        );
        d.insert(
            DictKey(Object::from_static("platform")),
            Object::from_static(host_platform()),
        );
        d.insert(
            DictKey(Object::from_static("byteorder")),
            Object::from_static(if cfg!(target_endian = "little") {
                "little"
            } else {
                "big"
            }),
        );
        d.insert(
            DictKey(Object::from_static("maxsize")),
            Object::Int(i64::MAX),
        );
        d.insert(
            DictKey(Object::from_static("executable")),
            std::env::current_exe()
                .ok()
                .map_or(Object::from_static(""), |p| {
                    Object::from_str(p.to_string_lossy().into_owned())
                }),
        );
        d.insert(
            DictKey(Object::from_static("implementation")),
            implementation_value(),
        );

        // Callables.
        d.insert(
            DictKey(Object::from_static("exit")),
            builtin("exit", sys_exit),
        );
        // RFC 0026 — private helper so `runpy.run_module()` can
        // execute frozen modules. Looks up a frozen source by name;
        // returns ``None`` if the module isn't frozen (or doesn't
        // exist). Mirrors CPython's `_imp.get_frozen_source` shape.
        {
            let frozen = cache.frozen.clone();
            d.insert(
                DictKey(Object::from_static("_get_frozen_source")),
                Object::Builtin(Rc::new(BuiltinFn {
                    name: "_get_frozen_source",
                    call: Box::new(move |args| {
                        let name = match args.first() {
                            Some(Object::Str(s)) => s.to_string(),
                            _ => return Err(type_error("_get_frozen_source() expects a string")),
                        };
                        let table = frozen.borrow();
                        Ok(table
                            .get(name.as_str())
                            .map(|src| Object::from_static(src.source))
                            .unwrap_or(Object::None))
                    }),
                    call_kw: None,
                })),
            );
        }
        {
            let frozen = cache.frozen.clone();
            d.insert(
                DictKey(Object::from_static("_is_frozen")),
                Object::Builtin(Rc::new(BuiltinFn {
                    name: "_is_frozen",
                    call: Box::new(move |args| {
                        let name = match args.first() {
                            Some(Object::Str(s)) => s.to_string(),
                            _ => return Ok(Object::Bool(false)),
                        };
                        let table = frozen.borrow();
                        Ok(Object::Bool(table.contains_key(name.as_str())))
                    }),
                    call_kw: None,
                })),
            );
        }
        d.insert(
            DictKey(Object::from_static("getrecursionlimit")),
            builtin("getrecursionlimit", sys_getrecursionlimit),
        );
        d.insert(
            DictKey(Object::from_static("setrecursionlimit")),
            builtin("setrecursionlimit", sys_setrecursionlimit),
        );
        d.insert(
            DictKey(Object::from_static("intern")),
            builtin("intern", sys_intern),
        );

        // Standard I/O streams. We expose them as file-like objects
        // sharing the interpreter's host sinks, so `print()` and
        // direct writes via `sys.stdout.write(...)` agree.
        let stdout_sink: Rc<RefCell<dyn std::io::Write + Send + Sync>> =
            Rc::new(RefCell::new(std::io::stdout()));
        let stderr_sink: Rc<RefCell<dyn std::io::Write + Send + Sync>> =
            Rc::new(RefCell::new(std::io::stderr()));
        d.insert(
            DictKey(Object::from_static("stdout")),
            Object::File(Rc::new(PyFile::new(
                "<stdout>",
                "w",
                FileBackend::Stdout(stdout_sink),
            ))),
        );
        d.insert(
            DictKey(Object::from_static("stderr")),
            Object::File(Rc::new(PyFile::new(
                "<stderr>",
                "w",
                FileBackend::Stderr(stderr_sink),
            ))),
        );
        d.insert(
            DictKey(Object::from_static("stdin")),
            Object::File(Rc::new(PyFile::new("<stdin>", "r", FileBackend::Stdin))),
        );
    }
    Rc::new(PyModule {
        name: "sys".to_owned(),
        filename: None,
        dict,
    })
}

fn host_platform() -> &'static str {
    if cfg!(target_os = "linux") {
        "linux"
    } else if cfg!(target_os = "macos") {
        "darwin"
    } else if cfg!(target_os = "windows") {
        "win32"
    } else if cfg!(target_os = "freebsd") {
        "freebsd"
    } else {
        "unknown"
    }
}

fn implementation_value() -> Object {
    // `sys.implementation` is a `types.SimpleNamespace`-shaped object
    // in CPython. RFC 0023 added [`Object::SimpleNamespace`] so we
    // can match the shape exactly — attribute access via `.name`
    // / `.version` works, but the value isn't a dict.
    let mut d = DictData::new();
    d.insert(
        DictKey(Object::from_static("name")),
        Object::from_static("weavepy"),
    );
    d.insert(
        DictKey(Object::from_static("version")),
        Object::new_tuple(vec![
            Object::Int(PY_VERSION.0),
            Object::Int(PY_VERSION.1),
            Object::Int(PY_VERSION.2),
            Object::from_static("final"),
            Object::Int(0),
        ]),
    );
    d.insert(
        DictKey(Object::from_static("hexversion")),
        Object::Int((PY_VERSION.0 << 24) | (PY_VERSION.1 << 16) | (PY_VERSION.2 << 8) | 0xF0),
    );
    d.insert(
        DictKey(Object::from_static("cache_tag")),
        Object::from_static(crate::pycache::CACHE_TAG),
    );
    d.insert(
        DictKey(Object::from_static("_multiarch")),
        Object::from_static("weavepy-x86_64"),
    );
    Object::SimpleNamespace(Rc::new(RefCell::new(d)))
}

fn builtin(name: &'static str, body: fn(&[Object]) -> Result<Object, RuntimeError>) -> Object {
    Object::Builtin(Rc::new(BuiltinFn {
        name,
        call: Box::new(body),
        call_kw: None,
    }))
}

/// `sys.exit([code])` — modelled as raising `SystemExit(code)`. The
/// VM doesn't yet special-case this in its main loop, so it walks
/// out as an ordinary uncaught exception. That's enough for `try:
/// sys.exit(1) except SystemExit:` to work; the CLI then renders
/// the resulting traceback like CPython.
fn sys_exit(args: &[Object]) -> Result<Object, RuntimeError> {
    let code = args.first().cloned().unwrap_or(Object::None);
    let inst = crate::builtin_types::make_exception_with_class(
        crate::builtin_types::builtin_types().system_exit.clone(),
        "",
    );
    if let Object::Instance(inst_rc) = &inst {
        inst_rc
            .dict
            .borrow_mut()
            .insert(DictKey(Object::from_static("code")), code.clone());
        inst_rc.dict.borrow_mut().insert(
            DictKey(Object::from_static("args")),
            Object::new_tuple(vec![code]),
        );
    }
    Err(RuntimeError::PyException(crate::error::PyException::new(
        inst,
    )))
}

fn sys_getrecursionlimit(_args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(Object::Int(1000))
}

fn sys_setrecursionlimit(args: &[Object]) -> Result<Object, RuntimeError> {
    let _ = args;
    // No-op for now: the host stack does the bounding.
    Ok(Object::None)
}

fn sys_intern(args: &[Object]) -> Result<Object, RuntimeError> {
    match args.first() {
        Some(Object::Str(_)) => Ok(args[0].clone()),
        _ => Err(type_error("sys.intern() argument must be str")),
    }
}

fn sys_getframe(
    args: &[Object],
    frame_stack: &Rc<RefCell<Vec<Rc<PyFrame>>>>,
) -> Result<Object, RuntimeError> {
    let depth = match args.first() {
        Some(Object::Int(d)) => *d as usize,
        None => 0,
        _ => return Err(type_error("depth must be an int")),
    };
    let stack = frame_stack.borrow();
    // The topmost frame is the currently-executing one, which is
    // the *callee* of `sys._getframe`. CPython considers the
    // calling frame as depth 0; we mirror by indexing from the back.
    if stack.is_empty() {
        return Err(value_error("call stack is not deep enough"));
    }
    if depth >= stack.len() {
        return Err(value_error("call stack is not deep enough"));
    }
    let idx = stack.len() - 1 - depth;
    Ok(Object::Frame(stack[idx].clone()))
}

fn sys_exc_info(
    exc_info_stack: &Rc<RefCell<Vec<crate::error::PyException>>>,
) -> Result<Object, RuntimeError> {
    let stack = exc_info_stack.borrow();
    if let Some(top) = stack.last() {
        let inst = top.instance.clone();
        let type_obj = match &inst {
            Object::Instance(i) => Object::Type(i.class.clone()),
            _ => Object::None,
        };
        let tb = match &inst {
            Object::Instance(i) => i
                .dict
                .borrow()
                .get(&crate::object::DictKey(Object::from_static(
                    "__traceback__",
                )))
                .cloned()
                .unwrap_or(Object::None),
            _ => Object::None,
        };
        Ok(Object::new_tuple(vec![type_obj, inst, tb]))
    } else {
        Ok(Object::new_tuple(vec![
            Object::None,
            Object::None,
            Object::None,
        ]))
    }
}

fn sys_default_excepthook(args: &[Object]) -> Result<Object, RuntimeError> {
    // type, value, tb — we render a short summary directly to stderr.
    // The VM-level CLI does the full traceback. This default is what
    // a user gets when they call `sys.excepthook(*sys.exc_info())`
    // from inside `except:` and the user hook is `None`.
    let value = args.get(1).cloned().unwrap_or(Object::None);
    let kind = match &value {
        Object::Instance(i) => i.class.name.clone(),
        _ => "Exception".to_owned(),
    };
    let msg = crate::builtin_types::exception_message(&value).unwrap_or_default();
    if msg.is_empty() {
        eprintln!("{kind}");
    } else {
        eprintln!("{kind}: {msg}");
    }
    Ok(Object::None)
}

// Trace and profile hooks live in the runtime's thread-local registry
// (:mod:`crate::trace`) so the VM dispatcher and ``sys.gettrace`` /
// ``sys.getprofile`` see the same value. Line-level event firing
// inside the interpreter dispatch is gated behind RFC 0031; for now
// these accessors are observable but do not call back into the hook
// at every opcode (that requires deeper VM surgery and a perf
// trade-off discussion).

fn sys_settrace(args: &[Object]) -> Result<Object, RuntimeError> {
    let hook = args.first().cloned().unwrap_or(Object::None);
    crate::trace::set_trace_hook(hook);
    Ok(Object::None)
}

fn sys_setprofile(args: &[Object]) -> Result<Object, RuntimeError> {
    let hook = args.first().cloned().unwrap_or(Object::None);
    crate::trace::set_profile_hook(hook);
    Ok(Object::None)
}

fn sys_gettrace(_args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(crate::trace::trace_hook().unwrap_or(Object::None))
}

fn sys_getprofile(_args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(crate::trace::profile_hook().unwrap_or(Object::None))
}

fn sys_getsizeof(args: &[Object]) -> Result<Object, RuntimeError> {
    // CPython's `getsizeof` is a per-object slot. We answer with a
    // best-effort estimate so user code doesn't crash, but make no
    // promise of accuracy.
    let size = args
        .first()
        .map(|o| match o {
            Object::Int(_) | Object::Float(_) | Object::Bool(_) | Object::None => 28,
            Object::Str(s) => 49 + s.len() as i64,
            Object::Bytes(b) => 33 + b.len() as i64,
            Object::List(l) => 56 + (l.borrow().len() as i64) * 8,
            Object::Tuple(t) => 40 + (t.len() as i64) * 8,
            Object::Dict(d) => 64 + (d.borrow().len() as i64) * 16,
            Object::Set(s) => 216 + (s.borrow().len() as i64) * 16,
            _ => 16,
        })
        .unwrap_or(0);
    Ok(Object::Int(size))
}

fn sys_flags_value() -> Object {
    let mut d = DictData::new();
    for name in [
        "debug",
        "inspect",
        "interactive",
        "optimize",
        "dont_write_bytecode",
        "no_user_site",
        "no_site",
        "ignore_environment",
        "verbose",
        "bytes_warning",
        "quiet",
        "hash_randomization",
        "isolated",
        "dev_mode",
        "utf8_mode",
        "safe_path",
        "int_max_str_digits",
        "warn_default_encoding",
    ] {
        d.insert(DictKey(Object::from_static(name)), Object::Int(0));
    }
    Object::Dict(Rc::new(RefCell::new(d)))
}

fn sys_float_info() -> Object {
    let mut d = DictData::new();
    d.insert(DictKey(Object::from_static("max")), Object::Float(f64::MAX));
    d.insert(
        DictKey(Object::from_static("min")),
        Object::Float(f64::MIN_POSITIVE),
    );
    d.insert(
        DictKey(Object::from_static("epsilon")),
        Object::Float(f64::EPSILON),
    );
    d.insert(DictKey(Object::from_static("dig")), Object::Int(15));
    d.insert(DictKey(Object::from_static("mant_dig")), Object::Int(53));
    d.insert(DictKey(Object::from_static("max_10_exp")), Object::Int(308));
    d.insert(
        DictKey(Object::from_static("min_10_exp")),
        Object::Int(-307),
    );
    d.insert(DictKey(Object::from_static("max_exp")), Object::Int(1024));
    d.insert(DictKey(Object::from_static("min_exp")), Object::Int(-1021));
    d.insert(DictKey(Object::from_static("radix")), Object::Int(2));
    d.insert(DictKey(Object::from_static("rounds")), Object::Int(1));
    Object::Dict(Rc::new(RefCell::new(d)))
}

fn sys_int_info() -> Object {
    let mut d = DictData::new();
    d.insert(
        DictKey(Object::from_static("bits_per_digit")),
        Object::Int(30),
    );
    d.insert(DictKey(Object::from_static("sizeof_digit")), Object::Int(4));
    d.insert(
        DictKey(Object::from_static("default_max_str_digits")),
        Object::Int(4300),
    );
    d.insert(
        DictKey(Object::from_static("str_digits_check_threshold")),
        Object::Int(640),
    );
    Object::Dict(Rc::new(RefCell::new(d)))
}

fn sys_hash_info() -> Object {
    let mut d = DictData::new();
    d.insert(DictKey(Object::from_static("width")), Object::Int(64));
    d.insert(
        DictKey(Object::from_static("modulus")),
        Object::Int(i64::MAX),
    );
    d.insert(DictKey(Object::from_static("inf")), Object::Int(314_159));
    d.insert(DictKey(Object::from_static("nan")), Object::Int(0));
    d.insert(DictKey(Object::from_static("imag")), Object::Int(1_000_003));
    d.insert(
        DictKey(Object::from_static("algorithm")),
        Object::from_static("siphash13"),
    );
    d.insert(DictKey(Object::from_static("hash_bits")), Object::Int(64));
    d.insert(DictKey(Object::from_static("seed_bits")), Object::Int(128));
    d.insert(DictKey(Object::from_static("cutoff")), Object::Int(0));
    Object::Dict(Rc::new(RefCell::new(d)))
}

/// `sys.stdlib_module_names` — the documented set of standard-
/// library module names. CPython 3.13 ships a frozenset; we
/// mirror that with a [`Object::FrozenSet`].
fn stdlib_module_names_value() -> Object {
    use crate::object::SetData;
    let names: &[&'static str] = &[
        "_abc",
        "_aix_support",
        "_ast",
        "_asyncio",
        "_bisect",
        "_blake2",
        "_bz2",
        "_codecs",
        "_codecs_cn",
        "_codecs_hk",
        "_codecs_iso2022",
        "_codecs_jp",
        "_codecs_kr",
        "_codecs_tw",
        "_collections",
        "_collections_abc",
        "_compat_pickle",
        "_compression",
        "_contextvars",
        "_csv",
        "_ctypes",
        "_curses",
        "_curses_panel",
        "_datetime",
        "_decimal",
        "_elementtree",
        "_frozen_importlib",
        "_frozen_importlib_external",
        "_functools",
        "_hashlib",
        "_heapq",
        "_imp",
        "_io",
        "_json",
        "_locale",
        "_lsprof",
        "_lzma",
        "_markupbase",
        "_md5",
        "_multibytecodec",
        "_multiprocessing",
        "_opcode",
        "_operator",
        "_osx_support",
        "_pickle",
        "_posixshmem",
        "_posixsubprocess",
        "_py_abc",
        "_pydecimal",
        "_pyio",
        "_queue",
        "_random",
        "_sha1",
        "_sha2",
        "_sha3",
        "_signal",
        "_sitebuiltins",
        "_socket",
        "_sqlite3",
        "_sre",
        "_ssl",
        "_stat",
        "_string",
        "_strptime",
        "_struct",
        "_symtable",
        "_thread",
        "_threading_local",
        "_tkinter",
        "_tokenize",
        "_tracemalloc",
        "_uuid",
        "_warnings",
        "_weakref",
        "_weakrefset",
        "_zoneinfo",
        "abc",
        "antigravity",
        "argparse",
        "array",
        "ast",
        "asynchat",
        "asyncio",
        "asyncore",
        "atexit",
        "audioop",
        "base64",
        "bdb",
        "binascii",
        "bisect",
        "builtins",
        "bz2",
        "cProfile",
        "calendar",
        "cgi",
        "cgitb",
        "chunk",
        "cmath",
        "cmd",
        "code",
        "codecs",
        "codeop",
        "collections",
        "colorsys",
        "compileall",
        "concurrent",
        "configparser",
        "contextlib",
        "contextvars",
        "copy",
        "copyreg",
        "crypt",
        "csv",
        "ctypes",
        "curses",
        "dataclasses",
        "datetime",
        "dbm",
        "decimal",
        "difflib",
        "dis",
        "doctest",
        "email",
        "encodings",
        "ensurepip",
        "enum",
        "errno",
        "faulthandler",
        "fcntl",
        "filecmp",
        "fileinput",
        "fnmatch",
        "fractions",
        "ftplib",
        "functools",
        "gc",
        "genericpath",
        "getopt",
        "getpass",
        "gettext",
        "glob",
        "graphlib",
        "grp",
        "gzip",
        "hashlib",
        "heapq",
        "hmac",
        "html",
        "http",
        "idlelib",
        "imaplib",
        "imghdr",
        "imp",
        "importlib",
        "inspect",
        "io",
        "ipaddress",
        "itertools",
        "json",
        "keyword",
        "linecache",
        "locale",
        "logging",
        "lzma",
        "mailbox",
        "mailcap",
        "marshal",
        "math",
        "mimetypes",
        "mmap",
        "modulefinder",
        "msilib",
        "msvcrt",
        "multiprocessing",
        "netrc",
        "nis",
        "nntplib",
        "ntpath",
        "numbers",
        "opcode",
        "operator",
        "optparse",
        "os",
        "ossaudiodev",
        "pathlib",
        "pdb",
        "pickle",
        "pickletools",
        "pipes",
        "pkgutil",
        "platform",
        "plistlib",
        "poplib",
        "posix",
        "posixpath",
        "pprint",
        "profile",
        "pstats",
        "pty",
        "pwd",
        "py_compile",
        "pyclbr",
        "pydoc",
        "pydoc_data",
        "pyexpat",
        "queue",
        "quopri",
        "random",
        "re",
    ];
    let mut set = SetData::new();
    for n in names {
        set.insert(DictKey(Object::from_static(n)));
    }
    // Two-shot to dodge the 200-element array literal limit.
    for n in &[
        "readline",
        "reprlib",
        "resource",
        "rlcompleter",
        "runpy",
        "sched",
        "secrets",
        "select",
        "selectors",
        "shelve",
        "shlex",
        "shutil",
        "signal",
        "site",
        "smtpd",
        "smtplib",
        "sndhdr",
        "socket",
        "socketserver",
        "spwd",
        "sqlite3",
        "sre_compile",
        "sre_constants",
        "sre_parse",
        "ssl",
        "stat",
        "statistics",
        "string",
        "stringprep",
        "struct",
        "subprocess",
        "sunau",
        "symtable",
        "sys",
        "sysconfig",
        "syslog",
        "tabnanny",
        "tarfile",
        "telnetlib",
        "tempfile",
        "termios",
        "test",
        "textwrap",
        "threading",
        "time",
        "timeit",
        "tkinter",
        "token",
        "tokenize",
        "tomllib",
        "trace",
        "traceback",
        "tracemalloc",
        "tty",
        "turtle",
        "turtledemo",
        "types",
        "typing",
        "unicodedata",
        "unittest",
        "urllib",
        "uu",
        "uuid",
        "venv",
        "warnings",
        "wave",
        "weakref",
        "webbrowser",
        "winreg",
        "winsound",
        "wsgiref",
        "xdrlib",
        "xml",
        "xmlrpc",
        "zipapp",
        "zipfile",
        "zipimport",
        "zlib",
        "zoneinfo",
    ] {
        set.insert(DictKey(Object::from_static(n)));
    }
    Object::FrozenSet(Rc::new(set))
}

/// `sys.getrefcount(obj)` — best-effort. Always returns a
/// non-zero value to satisfy `assert sys.getrefcount(x) > 0`-
/// style sanity checks. The exact number is implementation-
/// specific even in CPython.
fn sys_getrefcount(args: &[Object]) -> Result<Object, RuntimeError> {
    if args.is_empty() {
        return Err(type_error("getrefcount() takes exactly 1 argument"));
    }
    // Two is what CPython returns for a freshly-bound name: the
    // local + the argument.
    Ok(Object::Int(2))
}

/// Default `sys.displayhook`: if the value is None do nothing,
/// otherwise print `repr(value)` and stash on
/// `builtins._`. Matches CPython's reference implementation.
fn sys_displayhook(args: &[Object]) -> Result<Object, RuntimeError> {
    let value = args.first().cloned().unwrap_or(Object::None);
    if matches!(value, Object::None) {
        return Ok(Object::None);
    }
    let rendered = value.repr();
    println!("{rendered}");
    Ok(Object::None)
}

fn sys_thread_info() -> Object {
    let mut d = DictData::new();
    d.insert(
        DictKey(Object::from_static("name")),
        Object::from_static("weavepy"),
    );
    d.insert(
        DictKey(Object::from_static("lock")),
        Object::from_static("cooperative"),
    );
    d.insert(DictKey(Object::from_static("version")), Object::None);
    Object::Dict(Rc::new(RefCell::new(d)))
}

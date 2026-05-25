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
            DictKey(Object::from_static("setprofile")),
            builtin("setprofile", sys_setprofile),
        );
        d.insert(
            DictKey(Object::from_static("gettrace")),
            builtin("gettrace", |_| Ok(Object::None)),
        );
        d.insert(
            DictKey(Object::from_static("getprofile")),
            builtin("getprofile", |_| Ok(Object::None)),
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

fn sys_settrace(_args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(Object::None)
}

fn sys_setprofile(_args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(Object::None)
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

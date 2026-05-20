//! The `sys` built-in module.
//!
//! Tracks CPython 3.13's `sys` module shape for the attributes we
//! support. `argv`, `path`, and `modules` are all backed by the
//! interpreter's [`ModuleCache`] so writes flow both ways.
//!
//! Anything that touches host I/O streams (`sys.stdout`,
//! `sys.stderr`) is deferred to RFC 0014, when we land the `io`
//! module and Python file objects.

use std::cell::RefCell;
use std::rc::Rc;

use crate::error::{type_error, RuntimeError};
use crate::import::ModuleCache;
use crate::object::{BuiltinFn, DictData, DictKey, FileBackend, Object, PyFile, PyModule};

/// CPython compatibility version we advertise. This is intentionally
/// independent from the WeavePy package version (see
/// `weavepy-cli/src/main.rs`); user code that inspects
/// `sys.version_info` is checking *Python language* compatibility, not
/// the WeavePy build identity.
pub const PY_VERSION: (i64, i64, i64) = (3, 13, 0);

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
        let stdout_sink: Rc<RefCell<dyn std::io::Write>> = Rc::new(RefCell::new(std::io::stdout()));
        let stderr_sink: Rc<RefCell<dyn std::io::Write>> = Rc::new(RefCell::new(std::io::stderr()));
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
    // CPython exposes a `types.SimpleNamespace`-flavoured object on
    // `sys.implementation`. We approximate with a `dict` until
    // `SimpleNamespace` exists — close enough for the typical
    // `sys.implementation.name == "cpython"` check, which our build
    // intentionally answers `"weavepy"`.
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
        Object::from_static("weavepy-0"),
    );
    Object::Dict(Rc::new(RefCell::new(d)))
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

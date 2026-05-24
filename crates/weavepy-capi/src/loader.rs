//! dlopen-based loader for compiled extension modules.
//!
//! Given a path to a shared library (`.so` / `.dylib` / `.pyd`)
//! and a fully-qualified module name, this module:
//!
//! 1. Calls [`libloading::Library::new`] to load the library into
//!    the process. Symbols the extension imports (everything in
//!    `Python.h`) resolve against the host `weavepy` binary, which
//!    statically links this crate.
//! 2. Looks up `PyInit_<leaf-name>`. The leaf name is the
//!    last `.`-delimited component of the module name, matching
//!    CPython's convention.
//! 3. Sets up an [`crate::interp::ActiveContext`] so the C function
//!    can call back into the runtime, then invokes the init
//!    function.
//! 4. Translates the returned `PyObject *` (which the extension
//!    obtained via [`crate::module::PyModule_Create2`]) into a
//!    Rust [`Object::Module`] suitable for caching in `sys.modules`.

use std::ffi::CString;
use std::path::Path;
use std::rc::Rc;

use libloading::{Library, Symbol};
use weavepy_vm::object::{DictData, DictKey, Object, PyModule};

use crate::interp::ActiveContext;
use crate::module::PyMethodDef;
use crate::object::PyObject;

/// Type of the entry-point a CPython extension exports.
type PyInitFn = unsafe extern "C" fn() -> *mut PyObject;

/// Loaded library handle. Kept alive for the lifetime of the
/// running interpreter so the symbols stay resolved.
pub struct LoadedLibrary {
    pub _library: Library,
    pub module: Object,
}

/// Errors a load attempt can surface.
#[derive(Debug, thiserror::Error)]
pub enum LoadError {
    #[error("dlopen failed: {0}")]
    Dlopen(String),
    #[error("missing init symbol {0}")]
    MissingInit(String),
    #[error("init function returned NULL{}", .pending.as_deref().map(|s| format!(": {s}")).unwrap_or_default())]
    NullInit { pending: Option<String> },
    #[error("init function returned non-module value")]
    NotAModule,
}

/// Load `path` as a CPython-style extension module named
/// `module_name`. Returns the `Object::Module` to register in
/// `sys.modules`.
pub fn load_extension_module(
    interp: *mut weavepy_vm::Interpreter,
    path: &Path,
    module_name: &str,
) -> Result<Object, LoadError> {
    crate::interp::ensure_initialised();

    let lib =
        unsafe { Library::new(path) }.map_err(|e| LoadError::Dlopen(format!("{path:?}: {e}")))?;

    let leaf = module_name.rsplit('.').next().unwrap_or(module_name);
    let init_name = format!("PyInit_{leaf}");
    let init: Symbol<PyInitFn> = unsafe {
        lib.get(init_name.as_bytes())
            .map_err(|_| LoadError::MissingInit(init_name.clone()))?
    };

    let init_fn: PyInitFn = unsafe { std::mem::transmute::<_, PyInitFn>(*init) };
    drop(init);

    // Provide an empty globals + module placeholder so the C side's
    // PyImport_AddModule has a sensible cache target. The real
    // module value is filled in once the init function returns.
    let placeholder = Object::Module(Rc::new(PyModule {
        name: module_name.to_owned(),
        filename: Some(path.display().to_string()),
        dict: Rc::new(std::cell::RefCell::new(DictData::new())),
    }));
    let ctx = ActiveContext {
        interp,
        globals: None,
        current_module: Some(placeholder.clone()),
    };

    let raw = crate::interp::enter_extension_call(ctx, || unsafe { init_fn() });

    if raw.is_null() {
        let pending = crate::errors::take_pending().map(|p| {
            format!(
                "{}: {:?}",
                p.ty.as_ref()
                    .map(|t| t.name.clone())
                    .unwrap_or_else(|| "Exception".to_owned()),
                p.value
            )
        });
        return Err(LoadError::NullInit { pending });
    }

    let module_obj = unsafe { crate::object::clone_object(raw) };
    unsafe { crate::object::Py_DecRef(raw) };

    let module = match module_obj {
        Object::Module(m) => m,
        _ => return Err(LoadError::NotAModule),
    };

    // Copy in __file__ / __loader__ stubs.
    {
        let mut d = module.dict.borrow_mut();
        d.entry(DictKey(Object::from_static("__file__")))
            .or_insert_with(|| Object::from_str(path.display().to_string()));
        d.entry(DictKey(Object::from_static("__name__")))
            .or_insert_with(|| Object::from_str(module_name.to_owned()));
    }

    let result = Object::Module(module);
    // The library must stay loaded for the lifetime of the
    // process; otherwise its symbols (and therefore the module's
    // function pointers) would dangle. Leaking is correct here.
    let _: &'static Library = Box::leak(Box::new(lib));

    Ok(result)
}

/// Helper used by the higher-level frozen importlib stub. Returns
/// `Some(module)` on success; `None` if `path` doesn't exist.
pub fn try_load(
    interp: *mut weavepy_vm::Interpreter,
    path: &Path,
    module_name: &str,
) -> Option<Result<Object, LoadError>> {
    if !path.is_file() {
        return None;
    }
    Some(load_extension_module(interp, path, module_name))
}

/// Locate an extension on `sys.path` for the given module name.
/// Mirrors CPython's `_bootstrap_external.ExtensionFileLoader`
/// search: try `<dir>/<module-leaf>.<ext>` for each known extension.
pub fn find_extension_on_path(
    interp: &weavepy_vm::Interpreter,
    module_name: &str,
) -> Option<std::path::PathBuf> {
    let leaf = module_name.rsplit('.').next().unwrap_or(module_name);
    let exts = extension_suffixes();
    for dir in interp.module_cache().search_dirs() {
        for ext in exts {
            let candidate = dir.join(format!("{leaf}{ext}"));
            if candidate.is_file() {
                return Some(candidate);
            }
            let nested = dir
                .join(module_name.replace('.', "/"))
                .with_extension(&ext[1..]);
            if nested.is_file() {
                return Some(nested);
            }
        }
    }
    None
}

/// Extension-file suffixes the loader recognises, in priority order.
pub fn extension_suffixes() -> &'static [&'static str] {
    if cfg!(target_os = "macos") {
        &[".cpython-313-darwin.so", ".abi3.so", ".so", ".dylib"]
    } else if cfg!(target_os = "linux") {
        &[
            ".cpython-313-x86_64-linux-gnu.so",
            ".cpython-313-aarch64-linux-gnu.so",
            ".abi3.so",
            ".so",
        ]
    } else if cfg!(target_os = "windows") {
        &[".pyd", ".dll"]
    } else {
        &[".so"]
    }
}

/// Convenience for tests: run a closure with a freshly initialised
/// interpreter pointer and the given module pre-populated.
#[allow(dead_code)]
pub(crate) unsafe fn _interp_smoke(
    interp: *mut weavepy_vm::Interpreter,
    name: &str,
    methods: &[PyMethodDef],
) -> Object {
    let dict = Rc::new(std::cell::RefCell::new(DictData::new()));
    {
        let mut d = dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("__name__")),
            Object::from_str(name.to_owned()),
        );
        for m in methods {
            if m.ml_name.is_null() {
                break;
            }
            let mname = unsafe { std::ffi::CStr::from_ptr(m.ml_name) }
                .to_string_lossy()
                .into_owned();
            // Just stash a None — used for type tests only.
            d.insert(DictKey(Object::from_str(mname)), Object::None);
        }
    }
    let _ = interp;
    let _: CString = CString::new(name).unwrap();
    Object::Module(Rc::new(PyModule {
        name: name.to_owned(),
        filename: None,
        dict,
    }))
}

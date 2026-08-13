//! dlopen-based loader for compiled extension modules.
//!
//! Given a path to a shared library (`.so` / `.dylib` / `.pyd`)
//! and a fully-qualified module name, this module:
//!
//! 1. Loads the library into the process. On POSIX that is
//!    [`libloading::Library::new`] (dlopen), and the extension's
//!    C-API imports resolve against the host `weavepy` binary,
//!    which statically links this crate. On Windows (RFC 0064 WS2)
//!    it is `LoadLibraryExW` with CPython's `dynload_win.c` flags —
//!    `LOAD_LIBRARY_SEARCH_DEFAULT_DIRS | LOAD_LIBRARY_SEARCH_
//!    DLL_LOAD_DIR`, so a wheel's `.pyd` resolves vendored
//!    dependent DLLs from its own directory and `AddDllDirectory`
//!    cookies but never from `PATH`/CWD (bpo-36085) — and the
//!    C-API imports resolve against the already-loaded
//!    `python313.dll` (RFC 0064 WS1).
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
use weavepy_vm::sync::Rc;

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
    /// Windows load failure, pre-shaped as CPython's
    /// `Python/dynload_win.c` ImportError text (the message tooling
    /// and users pattern-match on). The caller surfaces it verbatim.
    #[error("DLL load failed while importing {leaf}: {message}")]
    DllLoadFailed { leaf: String, message: String },
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

    let trace_loader = std::env::var_os("WEAVEPY_TRACE_LOADER").is_some();
    if trace_loader {
        eprintln!("[LOADER] dlopen path={path:?} module={module_name}");
    }
    let leaf = module_name.rsplit('.').next().unwrap_or(module_name);
    let lib = open_extension_library(path, leaf)?;

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
        dict: Rc::new(weavepy_vm::sync::RefCell::new(DictData::default())),
    }));
    let ctx = ActiveContext {
        interp,
        globals: None,
        current_module: Some(placeholder.clone()),
    };

    let raw = crate::interp::enter_extension_call(ctx, || {
        let r = unsafe { init_fn() };
        if r.is_null() {
            return r;
        }
        // PEP 489: a tagged `PyModuleDef` means multi-phase init — run
        // the create/exec slots ourselves to get the populated module.
        if unsafe { crate::module::is_module_def(r) } {
            match unsafe {
                crate::module::run_multiphase_init(
                    r as *mut crate::module::PyModuleDef,
                    module_name,
                )
            } {
                Ok(m) => m,
                Err(e) => {
                    crate::errors::set_runtime_error(format!("multi-phase init failed: {e}"));
                    std::ptr::null_mut()
                }
            }
        } else {
            r
        }
    });

    if raw.is_null() {
        if trace_loader {
            let peek = crate::errors::take_pending().map(|p| {
                let ty =
                    p.ty.as_ref()
                        .map(|t| t.name.clone())
                        .unwrap_or_else(|| "Exception".to_owned());
                let msg = crate::errors::message_for(&p.value);
                let s = format!("{ty}: {msg}");
                crate::errors::set_pending(p.ty, p.value);
                s
            });
            eprintln!("[LOADER] FAILED (null init) module={module_name} pending={peek:?}");
        }
        let pending = crate::errors::take_pending().map(|p| {
            let ty =
                p.ty.as_ref()
                    .map(|t| t.name.clone())
                    .unwrap_or_else(|| "Exception".to_owned());
            let msg = crate::errors::message_for(&p.value);
            if msg.is_empty() {
                ty
            } else {
                format!("{ty}: {msg}")
            }
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
    if trace_loader {
        eprintln!("[LOADER] OK (lib leaked) module={module_name}");
    }

    Ok(result)
}

/// Load the shared library with the platform's CPython semantics.
///
/// POSIX: plain dlopen (`RTLD_NOW | RTLD_LOCAL`, libloading's
/// default, matching CPython's `dlopenflags` default).
#[cfg(not(windows))]
fn open_extension_library(path: &Path, _leaf: &str) -> Result<Library, LoadError> {
    unsafe { Library::new(path) }.map_err(|e| LoadError::Dlopen(format!("{path:?}: {e}")))
}

/// Windows: `LoadLibraryExW` with CPython's `dynload_win.c` flag set,
/// and failures shaped as CPython's `ImportError` message with the
/// `FormatMessageW` strerror (via the RFC 0063 error bridge).
#[cfg(windows)]
fn open_extension_library(path: &Path, leaf: &str) -> Result<Library, LoadError> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::GetLastError;
    use windows_sys::Win32::System::LibraryLoader::{
        LoadLibraryExW, LOAD_LIBRARY_SEARCH_DEFAULT_DIRS, LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR,
    };
    let wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    // SAFETY: `wide` is a valid NUL-terminated UTF-16 path; the flag
    // combination is the one CPython passes for absolute .pyd paths.
    let handle = unsafe {
        LoadLibraryExW(
            wide.as_ptr(),
            std::ptr::null_mut(),
            LOAD_LIBRARY_SEARCH_DEFAULT_DIRS | LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR,
        )
    };
    if handle.is_null() {
        // SAFETY: trivially safe; reads this thread's last-error slot.
        let code = unsafe { GetLastError() } as i32;
        return Err(LoadError::DllLoadFailed {
            leaf: leaf.to_owned(),
            message: weavepy_vm::stdlib::nt_support::format_message(code),
        });
    }
    // SAFETY: `handle` is a live HMODULE we own; libloading's Drop
    // would FreeLibrary it, but the caller leaks the Library for the
    // process lifetime (extension modules are never unloaded).
    // (libloading spells HMODULE as `isize`; windows-sys as a pointer.)
    Ok(unsafe { libloading::os::windows::Library::from_raw(handle as isize) }.into())
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
        // glibc (manylinux) and musl (musllinux, RFC 0047 wave 5) carry
        // distinct SOABI suffixes; the loader probes both so a wheel from
        // either Linux ABI resolves to its `.so`.
        &[
            ".cpython-313-x86_64-linux-gnu.so",
            ".cpython-313-aarch64-linux-gnu.so",
            ".cpython-313-x86_64-linux-musl.so",
            ".cpython-313-aarch64-linux-musl.so",
            ".abi3.so",
            ".so",
        ]
    } else if cfg!(target_os = "windows") {
        // The tagged name is what wheels actually install
        // (`EXT_SUFFIX` = `.cp313-win_amd64.pyd`); bare `.pyd` and
        // `.dll` are the CPython fallbacks (RFC 0064 WS2).
        &[".cp313-win_amd64.pyd", ".pyd", ".dll"]
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
    let dict = Rc::new(weavepy_vm::sync::RefCell::new(DictData::default()));
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

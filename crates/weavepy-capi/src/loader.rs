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

/// RFC 3492 punycode encoding (encode-only), matching Python's
/// `"punycode"` codec — CPython's `get_encoded_name` runs a module's
/// non-ASCII leaf name through it to derive the `PyInitU_` symbol.
fn punycode_encode(input: &str) -> String {
    const BASE: u32 = 36;
    const TMIN: u32 = 1;
    const TMAX: u32 = 26;
    const SKEW: u32 = 38;
    const DAMP: u32 = 700;
    const INITIAL_BIAS: u32 = 72;
    const INITIAL_N: u32 = 128;

    fn adapt(mut delta: u32, num_points: u32, first_time: bool) -> u32 {
        delta /= if first_time { DAMP } else { 2 };
        delta += delta / num_points;
        let mut k = 0;
        while delta > ((BASE - TMIN) * TMAX) / 2 {
            delta /= BASE - TMIN;
            k += BASE;
        }
        k + (((BASE - TMIN + 1) * delta) / (delta + SKEW))
    }

    fn digit_char(d: u32) -> char {
        if d < 26 {
            (b'a' + d as u8) as char
        } else {
            (b'0' + (d - 26) as u8) as char
        }
    }

    let code_points: Vec<u32> = input.chars().map(|c| c as u32).collect();
    let mut out: String = input.chars().filter(|c| c.is_ascii()).collect();
    let basic_len = out.chars().count() as u32;
    if basic_len > 0 {
        out.push('-');
    }

    let mut n = INITIAL_N;
    let mut delta: u32 = 0;
    let mut bias = INITIAL_BIAS;
    let mut handled = basic_len;
    let total = code_points.len() as u32;

    while handled < total {
        let m = code_points
            .iter()
            .copied()
            .filter(|&c| c >= n)
            .min()
            .unwrap();
        delta += (m - n) * (handled + 1);
        n = m;
        for &c in &code_points {
            if c < n {
                delta += 1;
            }
            if c == n {
                let mut q = delta;
                let mut k = BASE;
                loop {
                    let t = if k <= bias {
                        TMIN
                    } else if k >= bias + TMAX {
                        TMAX
                    } else {
                        k - bias
                    };
                    if q < t {
                        break;
                    }
                    out.push(digit_char(t + ((q - t) % (BASE - t))));
                    q = (q - t) / (BASE - t);
                    k += BASE;
                }
                out.push(digit_char(q));
                bias = adapt(delta, handled + 1, handled == basic_len);
                delta = 0;
                handled += 1;
            }
        }
        delta += 1;
        n += 1;
    }
    out
}

/// A `SystemError` RuntimeError for loader-detected init protocol
/// violations (CPython's `_PyImport_LoadDynamicModuleWithSpec` shapes).
fn system_error_runtime(message: String) -> weavepy_vm::error::RuntimeError {
    weavepy_vm::error::RuntimeError::PyException(weavepy_vm::error::PyException::from_builtin(
        "SystemError",
        message,
    ))
}

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
    /// The init function (or the PEP 489 slot machinery) raised: the
    /// original exception must propagate as-is — extension.test_loader
    /// asserts the type (`assertRaises(SystemError)` for bad slots),
    /// not an ImportError wrapper.
    #[error("{0}")]
    Raised(weavepy_vm::error::RuntimeError),
    #[error("init function returned non-module value")]
    NotAModule,
}

/// CPython's process-wide single-phase extensions cache
/// (`_PyRuntime.imports.extensions`, keyed by `(filename, name)`).
/// Stores a snapshot of the module dict taken right after init —
/// CPython's `def->m_base.m_copy` — so a re-import (same interpreter
/// or, under the legacy setting, a sub-interpreter) gets a fresh
/// module object whose dict *values* are the very same objects
/// (test_capi test_misc's test_module_state_shared_in_global asserts
/// `id(module.Error)` matches across interpreters, bpo-44050).
/// `Object` is Send + Sync under the GIL, like the other
/// mutex-guarded VM singletons.
static SINGLEPHASE_EXTENSIONS: std::sync::Mutex<Vec<((String, String), DictData)>> =
    std::sync::Mutex::new(Vec::new());

fn singlephase_cached(path: &Path, module_name: &str) -> Option<DictData> {
    let key = (path.display().to_string(), module_name.to_owned());
    SINGLEPHASE_EXTENSIONS
        .lock()
        .ok()?
        .iter()
        .find(|(k, _)| *k == key)
        .map(|(_, d)| d.clone())
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

    // CPython `import_find_extension`: a single-phase module already in
    // the process-wide cache is *not* re-initialised — a fresh module
    // object is built around the stored dict snapshot (m_copy). The
    // sub-interpreter incompatibility gate still applies first, exactly
    // like `_PyImport_CheckSubinterpIncompatibleExtensionAllowed`.
    if let Some(snapshot) = singlephase_cached(path, module_name) {
        let gate = weavepy_vm::vm_singletons::current_interpreter_ptr()
            // SAFETY: published by an enclosing VM frame live on this
            // thread; the GIL keeps the access exclusive.
            .and_then(|p| unsafe { (*p).subinterp_extension_gate() });
        if gate.is_some() {
            return Err(LoadError::Raised(weavepy_vm::error::import_error(format!(
                "module {module_name} does not support loading in subinterpreters"
            ))));
        }
        return Ok(Object::Module(Rc::new(PyModule {
            name: module_name.to_owned(),
            filename: Some(path.display().to_string()),
            dict: Rc::new(weavepy_vm::sync::RefCell::new(snapshot)),
        })));
    }

    let trace_loader = std::env::var_os("WEAVEPY_TRACE_LOADER").is_some();
    if trace_loader {
        eprintln!("[LOADER] dlopen path={path:?} module={module_name}");
    }
    let leaf = module_name.rsplit('.').next().unwrap_or(module_name);
    let lib = open_extension_library(path, leaf)?;

    // CPython's `importdl.c` hook naming: ASCII names use `PyInit_<leaf>`;
    // non-ASCII names use `PyInitU_<punycode(leaf) with '-' → '_'>`
    // (extension.test_loader's test_nonascii).
    let init_name = if leaf.is_ascii() {
        format!("PyInit_{leaf}")
    } else {
        format!("PyInitU_{}", punycode_encode(leaf).replace('-', "_"))
    };
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

    let single_phase = std::cell::Cell::new(false);
    let raw = crate::interp::enter_extension_call(ctx, || {
        let r = unsafe { init_fn() };
        if r.is_null() {
            return r;
        }
        // A def the extension forgot to pass through `PyModuleDef_Init`
        // has a zeroed object header (extension.test_loader's
        // `export_uninitialized` variant asserts SystemError).
        if unsafe { (*r).ob_type.is_null() } {
            crate::errors::set_pending(
                Some(
                    weavepy_vm::builtin_types::builtin_types()
                        .system_error
                        .clone(),
                ),
                Object::from_str(format!(
                    "init function of {module_name} returned uninitialized object"
                )),
            );
            return std::ptr::null_mut();
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
                    // Preserve a pending exception the slot machinery set
                    // (SystemError for bad slot IDs, the original error from
                    // a raising create/exec slot — extension.test_loader
                    // asserts the exception *type*); only synthesize a
                    // RuntimeError when nothing is pending.
                    if !crate::errors::has_pending() {
                        crate::errors::set_runtime_error(format!("multi-phase init failed: {e}"));
                    }
                    std::ptr::null_mut()
                }
            }
        } else {
            // Single-phase init. CPython's
            // `_PyImport_CheckSubinterpIncompatibleExtensionAllowed`:
            // in a sub-interpreter with the multi-interp extension check
            // in effect, single-phase modules refuse to load regardless
            // of GIL configuration (test_util's
            // IncompatibleExtensionModuleRestrictionsTests).
            let gate = weavepy_vm::vm_singletons::current_interpreter_ptr()
                // SAFETY: published by the enclosing extension-call
                // context; the GIL keeps access exclusive.
                .and_then(|p| unsafe { (*p).subinterp_extension_gate() });
            if gate.is_some() {
                unsafe { crate::object::Py_DecRef(r) };
                crate::errors::set_pending(
                    Some(
                        weavepy_vm::builtin_types::builtin_types()
                            .import_error
                            .clone(),
                    ),
                    Object::from_str(format!(
                        "module {module_name} does not support loading in subinterpreters"
                    )),
                );
                std::ptr::null_mut()
            } else {
                single_phase.set(true);
                // PEP 703 / RFC 0076 WS11: a single-phase module can
                // never declare `Py_mod_gil_not_used` — under `-X
                // gil=0` its import re-enables the GIL (with the
                // RuntimeWarning), unless `PYTHON_GIL=0` forced.
                crate::module::note_extension_gil_declaration(module_name, false);
                r
            }
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
        if let Some(p) = crate::errors::take_pending() {
            return Err(LoadError::Raised(crate::errors::to_runtime_error(p)));
        }
        // export_null: NULL without an exception → SystemError (CPython's
        // `_PyImport_LoadDynamicModuleWithSpec` wording).
        return Err(LoadError::Raised(system_error_runtime(format!(
            "initialization of {leaf} failed without raising an exception"
        ))));
    }

    // export_unreported_exception: init returned a module but left an
    // exception pending — CPython converts this to SystemError chained
    // via `__cause__` (`_PyErr_FormatFromCause`).
    if crate::errors::has_pending() {
        unsafe { crate::object::Py_DecRef(raw) };
        crate::errors::set_pending_system_error_from_cause(format!(
            "initialization of {leaf} raised unreported exception"
        ));
        let p = crate::errors::take_pending().expect("just set");
        return Err(LoadError::Raised(crate::errors::to_runtime_error(p)));
    }

    let module_obj = unsafe { crate::object::clone_object(raw) };
    unsafe { crate::object::Py_DecRef(raw) };

    let module = match module_obj {
        Object::Module(m) => m,
        // PEP 489: a `Py_mod_create` slot may return any object
        // (extension.test_loader's `nonmodule` variants); hand it back
        // as-is — `sys.modules` accepts arbitrary objects.
        other => {
            // Keep the library resident (same leak the module path does).
            let _: &'static Library = Box::leak(Box::new(lib));
            return Ok(other);
        }
    };

    // Copy in __file__ / __loader__ stubs.
    {
        let mut d = module.dict.borrow_mut();
        d.entry(DictKey(Object::from_static("__file__")))
            .or_insert_with(|| Object::from_str(path.display().to_string()));
        // Always the *dotted import name*, not the def's `m_name` leaf:
        // CPython swaps `_Py_PackageContext` around the init call, so a
        // single-phase `PyModule_Create2` names the fresh module
        // `package._leaf` even when `m_name` is just `_leaf`. psycopg2
        // asserts `psycopg2._psycopg.__name__ == "psycopg2._psycopg"`
        // (RFC 0076 WS5); pickle's `whichmodule` needs the dotted
        // spelling to resolve C functions back to their module.
        d.insert(
            DictKey(Object::from_static("__name__")),
            Object::from_str(module_name.to_owned()),
        );
        // `PyModule_Create2` seeds the `__spec__`/`__loader__` = None
        // placeholders CPython's `module_init_dict` puts in every fresh
        // module; `_bootstrap._load` normally replaces them. The native
        // importer synthesizes the pair lazily on first read instead
        // (`_weave_spec`), which only fires for *missing* keys — drop
        // the `__spec__` placeholder so the module gets a real
        // ExtensionFileLoader spec (`importlib.util.find_spec` raises
        // "__spec__ is None" otherwise — test_capi test_misc's
        // test_module_state_shared_in_global find_spec's the fixture).
        // `__loader__` must STAY None: `_init_module_attrs` only
        // installs the loading spec's loader over a None/missing
        // `__loader__`, and a Python-level load must win over the lazy
        // synthesis (test_importlib's Source_LoaderTests.test_module
        // loads with the *source-variant* ExtensionFileLoader and
        // asserts `module.__loader__` is that class). The synthesis
        // pass overwrites the None alongside `__spec__` when it fires.
        if let Some(Object::None) = d.get(&DictKey(Object::from_static("__spec__"))) {
            d.shift_remove(&DictKey(Object::from_static("__spec__")));
        }
    }
    // The lazy spec taxonomy classifies by the module's native filename
    // (`.so` → ExtensionFileLoader); the extension-created module has
    // none. SAFETY: plain field replacement through the shared handle
    // under the GIL, before the module is published — same pattern as
    // `_imp._fix_co_filename`.
    if module.filename.is_none() {
        unsafe {
            let f = std::ptr::addr_of!(module.filename).cast_mut();
            *f = Some(path.display().to_string());
        }
    }
    // Keep the struct-level name coherent with the dict's `__name__`
    // (same package-context rationale as above; same publication-safety
    // argument as the `filename` patch).
    if module.name != module_name {
        unsafe {
            let n = std::ptr::addr_of!(module.name).cast_mut();
            *n = module_name.to_owned();
        }
    }

    // Single-phase init: record the post-init dict snapshot in the
    // process-wide extensions cache (CPython's `update_global_state` /
    // `def->m_base.m_copy`) so re-imports reuse it without re-running
    // the init function.
    if single_phase.get() {
        if let Ok(mut cache) = SINGLEPHASE_EXTENSIONS.lock() {
            let key = (path.display().to_string(), module_name.to_owned());
            if !cache.iter().any(|(k, _)| *k == key) {
                cache.push((key, module.dict.borrow().clone()));
            }
        }
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

// ---------------------------------------------------------------------------
// RFC 0075: the VM-facing extension loader
// ---------------------------------------------------------------------------
//
// Historically this function lived in the `weavepy` umbrella crate
// (`install_capi_loader`), which meant a pure-capi embedder (a C
// program linking `libpython313`) could not import extensions. It
// only uses `weavepy_vm` + this crate, so it lives here now; the
// umbrella delegates.

/// Wire the C-extension loader into the VM. Idempotent.
pub fn install_vm_extension_loader() {
    static INIT: std::sync::Once = std::sync::Once::new();
    INIT.call_once(|| {
        crate::force_link();
        weavepy_vm::ext_loader::install_extension_loader(vm_load_extension);
        // RFC 0075 WS8: serve bridged types' *current* C `tp_doc`
        // through `type.__doc__`/`__text_signature__` (numpy's
        // add_newdoc writes it post-ready).
        weavepy_vm::descr_registry::install_ext_type_doc_hook(crate::types::live_tp_doc);
        weavepy_vm::descr_registry::install_ext_type_flags_hook(crate::types::live_tp_flags);
    });
}

fn vm_load_extension(
    interp: &mut weavepy_vm::Interpreter,
    full_name: &str,
) -> Result<Option<Object>, weavepy_vm::RuntimeError> {
    // Embedder-registered built-ins (`PyImport_AppendInittab`) win over
    // the filesystem scan, matching CPython's builtin-module priority.
    if let Some(init_fn) = crate::embed::inittab_lookup(full_name) {
        let interp_ptr: *mut weavepy_vm::Interpreter = interp;
        return match load_inittab_module(interp_ptr, full_name, init_fn) {
            Ok(module) => Ok(Some(module)),
            Err(LoadError::Raised(err)) => Err(err),
            Err(err) => Err(weavepy_vm::error::import_error(format!(
                "could not initialize built-in module '{full_name}': {err}"
            ))),
        };
    }
    // An explicit path stashed by `_imp.create_dynamic` /
    // `_imp._load_dynamic` wins over the `sys.path` scan — the spec's
    // origin may name a multi-init `.so` whose filename doesn't match
    // the module (CPython's `_testmultiphase` exports thirty-odd
    // `PyInit_<variant>` symbols from one file; RFC 0068 WS4).
    let path = match weavepy_vm::ext_loader::take_explicit_path(full_name)
        .or_else(|| find_extension_on_path(interp, full_name))
    {
        Some(p) => p,
        None => return Ok(None),
    };
    let interp_ptr: *mut weavepy_vm::Interpreter = interp;
    match load_extension_module(interp_ptr, &path, full_name) {
        Ok(module) => Ok(Some(module)),
        // The init function (or PEP 489 slot machinery) raised: propagate
        // the original exception verbatim — extension.test_loader asserts
        // SystemError (not an ImportError wrapper) for the bad-module
        // fixtures (RFC 0068 WS4).
        //
        // …unless we're importing from inside a sub-interpreter. CPython
        // runs the init function while switched to the *main* interpreter,
        // and an exception object cannot be handed across the boundary:
        // it's reported through the main interpreter's unraisable hook and
        // replaced by a blanket ImportError in the calling interpreter
        // (gh-144601; test_import SubinterpImportTests
        // test_pyinit_function_raises_exception).
        Err(LoadError::Raised(err)) => {
            // The sub-interpreter compatibility gate
            // (`_PyImport_CheckSubinterpIncompatibleExtensionAllowed` /
            // the PEP 489 multi-interpreter slot checks) raises *in the
            // importing interpreter*, before any switch to main — CPython
            // propagates that ImportError verbatim (test_import
            // SubinterpImportTests check_incompatible_here asserts the
            // exact "does not support loading in subinterpreters" text).
            let is_compat_gate = matches!(
                &err,
                weavepy_vm::RuntimeError::PyException(exc)
                    if exc.type_name() == "ImportError"
                        && exc
                            .message()
                            .ends_with("does not support loading in subinterpreters")
            );
            if !is_compat_gate && weavepy_vm::stdlib::interpreters_mod::in_subinterpreter() {
                if let Some(main_ptr) =
                    weavepy_vm::stdlib::interpreters_mod::main_host_interpreter_ptr()
                {
                    // SAFETY: the host interpreter is parked on this
                    // thread's call stack for the duration of the
                    // sub-interpreter call; the GIL makes the access
                    // exclusive.
                    let main = unsafe { &mut *main_ptr };
                    main.write_unraisable_msg(
                        &err,
                        &Object::None,
                        "",
                        Some("Exception while importing from subinterpreter"),
                    );
                }
                let e =
                    weavepy_vm::RuntimeError::PyException(weavepy_vm::PyException::from_builtin(
                        "ImportError",
                        "failed to import from subinterpreter due to exception".to_owned(),
                    ));
                return Err(e);
            }
            Err(err)
        }
        Err(err) => {
            // The Windows load-failure variant is already CPython's
            // exact ImportError text ("DLL load failed while
            // importing X: …" — RFC 0064 WS2); surface it verbatim.
            // Other failures keep the WeavePy-prefixed shape.
            let message = match &err {
                LoadError::DllLoadFailed { .. } => err.to_string(),
                // CPython's dynload wording; `.name` is asserted by
                // extension.test_loader's test_unloadable.
                LoadError::MissingInit(sym) => {
                    format!("dynamic module does not define module export function ({sym})")
                }
                _ => format!("could not load extension '{full_name}': {err}"),
            };
            let e = weavepy_vm::RuntimeError::PyException(weavepy_vm::PyException::from_builtin(
                "ImportError",
                message,
            ));
            // ImportError carries the module name and origin path
            // (CPython sets both on every dynload failure).
            weavepy_vm::error::set_exception_attr(
                &e,
                "name",
                Object::from_str(full_name.to_owned()),
            );
            weavepy_vm::error::set_exception_attr(
                &e,
                "path",
                Object::from_str(path.display().to_string()),
            );
            Err(e)
        }
    }
}

/// RFC 0075 WS2 — initialise an embedder-registered built-in module
/// from its `PyInit_*` function pointer (`PyImport_AppendInittab`).
/// The post-init discipline mirrors [`load_extension_module`] minus
/// the dlopen/leak: multi-phase defs run their slots, NULL-without-
/// exception is a SystemError, unreported exceptions chain.
pub fn load_inittab_module(
    interp: *mut weavepy_vm::Interpreter,
    module_name: &str,
    init_fn: unsafe extern "C" fn() -> *mut PyObject,
) -> Result<Object, LoadError> {
    crate::interp::ensure_initialised();
    let placeholder = Object::Module(Rc::new(PyModule {
        name: module_name.to_owned(),
        filename: None,
        dict: Rc::new(weavepy_vm::sync::RefCell::new(DictData::default())),
    }));
    let ctx = ActiveContext {
        interp,
        globals: None,
        current_module: Some(placeholder),
    };
    let raw = crate::interp::enter_extension_call(ctx, || {
        let r = unsafe { init_fn() };
        if r.is_null() {
            return r;
        }
        if unsafe { (*r).ob_type.is_null() } {
            crate::errors::set_pending(
                Some(
                    weavepy_vm::builtin_types::builtin_types()
                        .system_error
                        .clone(),
                ),
                Object::from_str(format!(
                    "init function of {module_name} returned uninitialized object"
                )),
            );
            return std::ptr::null_mut();
        }
        if unsafe { crate::module::is_module_def(r) } {
            match unsafe {
                crate::module::run_multiphase_init(
                    r as *mut crate::module::PyModuleDef,
                    module_name,
                )
            } {
                Ok(m) => m,
                Err(e) => {
                    if !crate::errors::has_pending() {
                        crate::errors::set_runtime_error(format!("multi-phase init failed: {e}"));
                    }
                    std::ptr::null_mut()
                }
            }
        } else {
            r
        }
    });
    if raw.is_null() {
        if let Some(p) = crate::errors::take_pending() {
            return Err(LoadError::Raised(crate::errors::to_runtime_error(p)));
        }
        return Err(LoadError::Raised(system_error_runtime(format!(
            "initialization of {module_name} failed without raising an exception"
        ))));
    }
    if crate::errors::has_pending() {
        unsafe { crate::object::Py_DecRef(raw) };
        crate::errors::set_pending_system_error_from_cause(format!(
            "initialization of {module_name} raised unreported exception"
        ));
        let p = crate::errors::take_pending().expect("just set");
        return Err(LoadError::Raised(crate::errors::to_runtime_error(p)));
    }
    let module_obj = unsafe { crate::object::clone_object(raw) };
    unsafe { crate::object::Py_DecRef(raw) };
    if let Object::Module(module) = &module_obj {
        let mut d = module.dict.borrow_mut();
        d.entry(DictKey(Object::from_static("__name__")))
            .or_insert_with(|| Object::from_str(module_name.to_owned()));
    }
    Ok(module_obj)
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

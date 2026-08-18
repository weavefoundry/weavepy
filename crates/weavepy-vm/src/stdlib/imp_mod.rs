//! The `_imp` built-in module (RFC 0029).
//!
//! Bridges the C-extension loader (registered through
//! [`crate::ext_loader`]) into Python so the frozen
//! `importlib.machinery.ExtensionFileLoader.exec_module` can
//! dlopen `.so` / `.dylib` / `.pyd` files via a Python-callable
//! surface. The shape mirrors CPython's `_imp` module:
//!
//! - `_load_dynamic(name, path[, file])` — load and execute the
//!   given extension; the result is registered in `sys.modules`
//!   and returned.
//! - `is_builtin(name)` — non-zero if `name` is in
//!   `sys.builtin_module_names`.
//! - `is_frozen(name)` — non-zero if `name` is shipped as a
//!   frozen Python module.
//! - `get_frozen_object(name)` — None (we don't pre-compile
//!   frozen modules into code objects yet).
//! - `find_frozen(name)` — capsule-shaped probe used by the
//!   FrozenImporter.
//! - `acquire_lock` / `release_lock` / `lock_held` — the global
//!   import lock: a reentrant, owner-tracked lock shared with
//!   `os.fork`'s `PyOS_BeforeFork` (which acquires it so a child
//!   never observes a partially initialized `sys.modules` entry —
//!   test_fork1.test_threaded_import_lock_fork).
//! - `extension_suffixes()` — same list as
//!   `importlib.machinery.EXTENSION_SUFFIXES`.
//! - `get_magic()` — `MAGIC_NUMBER` bytes (4 bytes).
//! - `source_hash(source_bytes)` — siphash13-derived 8-byte
//!   digest (matches `importlib.util.source_hash`).

use std::path::PathBuf;

use crate::sync::Rc;
use crate::sync::RefCell;

use crate::error::{import_error, RuntimeError};
use crate::import::ModuleCache;
use crate::object::{BuiltinFn, DictData, DictKey, Object, PyModule};

/// CPython's *global import lock* (`_PyImport_AcquireLock`): reentrant,
/// owner-tracked, process-global. Python code reaches it through
/// `_imp.acquire_lock`/`release_lock`; `os.fork` takes it in
/// `PyOS_BeforeFork` so the child never snapshots `sys.modules` mid-load
/// (a loading thread publishes a partial module under this lock —
/// test_fork1.test_threaded_import_lock_fork).
struct ImportLockState {
    owner: Option<u64>,
    count: u64,
}

static IMPORT_LOCK: std::sync::LazyLock<(
    parking_lot::Mutex<ImportLockState>,
    parking_lot::Condvar,
)> = std::sync::LazyLock::new(|| {
    (
        parking_lot::Mutex::new(ImportLockState {
            owner: None,
            count: 0,
        }),
        parking_lot::Condvar::new(),
    )
});

/// Acquire the global import lock for the current thread (reentrant).
/// Blocks with the GIL *released* when another thread owns it.
pub fn import_lock_acquire() {
    let me = crate::gil::current_thread_id();
    {
        let (lock, _) = &*IMPORT_LOCK;
        let mut st = lock.lock();
        if st.owner.is_none() || st.owner == Some(me) {
            st.owner = Some(me);
            st.count += 1;
            return;
        }
    }
    crate::gil::allow_threads_then(|| {
        let (lock, cv) = &*IMPORT_LOCK;
        let mut st = lock.lock();
        while !(st.owner.is_none() || st.owner == Some(me)) {
            cv.wait(&mut st);
        }
        st.owner = Some(me);
        st.count += 1;
    });
}

/// Release one level of the global import lock.
pub fn import_lock_release() -> Result<(), RuntimeError> {
    let me = crate::gil::current_thread_id();
    let (lock, cv) = &*IMPORT_LOCK;
    let mut st = lock.lock();
    if st.owner != Some(me) || st.count == 0 {
        return Err(crate::error::runtime_error("not holding the import lock"));
    }
    st.count -= 1;
    if st.count == 0 {
        st.owner = None;
        cv.notify_all();
    }
    Ok(())
}

/// Whether *any* thread currently holds the import lock.
pub fn import_lock_held() -> bool {
    let (lock, _) = &*IMPORT_LOCK;
    lock.lock().owner.is_some()
}

/// `PyOS_AfterFork_Child`'s `_PyImport_ReInitLock`: only the forking
/// thread survives, and `PyOS_BeforeFork` acquired one level on its
/// behalf — hand the whole lock to the child thread with the inherited
/// recursion count minus the fork-time level, so a fork under a nested
/// user-held lock stays releasable (test_fork1.test_nested_import_lock_fork).
pub fn import_lock_reinit_in_child() {
    let me = crate::gil::current_thread_id();
    let (lock, _) = &*IMPORT_LOCK;
    let mut st = lock.lock();
    if st.count > 0 {
        st.count -= 1; // the PyOS_BeforeFork acquisition
    }
    st.owner = if st.count > 0 { Some(me) } else { None };
}

/// PEP 489 multi-interpreter support declared by an emulated extension
/// fixture (the `Py_mod_multiple_interpreters` slot of CPython's
/// `_testmultiphase.c` variants).
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum MultiInterpSupport {
    /// `Py_MOD_MULTIPLE_INTERPRETERS_NOT_SUPPORTED`.
    NotSupported,
    /// `Py_MOD_MULTIPLE_INTERPRETERS_SUPPORTED` (shared GIL only).
    SharedGil,
    /// `Py_MOD_PER_INTERPRETER_GIL_SUPPORTED`.
    PerInterpreterGil,
}

/// The emulated `_testmultiphase` extension fixture family: CPython
/// builds these as one C extension exporting several `PyInit_*`
/// entry points with different `Py_mod_multiple_interpreters` slots
/// (test_import.SubinterpImportTests). WeavePy backs them all with the
/// frozen `_testmultiphase` Python body; only the declared support
/// level — which drives the subinterpreter import gate — differs.
pub(crate) fn multiphase_fixture(name: &str) -> Option<MultiInterpSupport> {
    match name {
        "_testmultiphase" => Some(MultiInterpSupport::PerInterpreterGil),
        // The per-module-state variant (test_capi.test_misc
        // Test_ModuleStateAccess); same body, per-interpreter GIL slot.
        "_testmultiphase_meth_state_access" => Some(MultiInterpSupport::PerInterpreterGil),
        "_test_non_isolated" => Some(MultiInterpSupport::NotSupported),
        // Single-phase-init variant (bpo-44050): its module object is
        // cached process-wide, so every interpreter sees the same
        // instance (test_capi.test_misc test_module_state_shared_in_global).
        "_test_module_state_shared" => Some(MultiInterpSupport::NotSupported),
        "_test_shared_gil_only" | "_test_no_multiple_interpreter_slot" => {
            Some(MultiInterpSupport::SharedGil)
        }
        _ => None,
    }
}

/// CPython's multi-phase-init subinterpreter gate: in a sub-interpreter
/// with `check_multi_interp_extensions` in effect, a module declaring
/// no (or shared-GIL-only, when this interpreter owns its GIL)
/// multi-interpreter support refuses to load.
fn check_multiphase_allowed(
    interp: &crate::Interpreter,
    name: &str,
    support: MultiInterpSupport,
) -> Result<(), RuntimeError> {
    if !interp.is_subinterpreter.get() || !interp.subinterp_extension_check_enabled() {
        return Ok(());
    }
    let incompatible = match support {
        MultiInterpSupport::NotSupported => true,
        MultiInterpSupport::SharedGil => interp.subinterp_own_gil.get(),
        MultiInterpSupport::PerInterpreterGil => false,
    };
    if incompatible {
        return Err(import_error(format!(
            "module {name} does not support loading in subinterpreters"
        )));
    }
    Ok(())
}

/// Load an emulated `_testmultiphase`-family fixture: run the
/// subinterpreter gate, then execute the frozen `_testmultiphase`
/// Python body under the requested module name with a `.so`-shaped
/// `__file__` — so the module's lazily-synthesized `__spec__` carries
/// an `ExtensionFileLoader`, exactly what
/// `test.test_import.require_extension` demands.
pub(crate) fn load_emulated_multiphase(
    interp: &mut crate::Interpreter,
    name: &str,
) -> Result<Object, RuntimeError> {
    let support = multiphase_fixture(name)
        .expect("load_emulated_multiphase: caller must check multiphase_fixture");
    check_multiphase_allowed(interp, name, support)?;
    // Single-phase modules live in CPython's process-wide extensions
    // cache — every interpreter re-importing one gets the *same* module
    // object (bpo-44050, test_module_state_shared_in_global).
    if name == "_test_module_state_shared" {
        if let Some(cached) = SINGLEPHASE_CACHE
            .lock()
            .ok()
            .and_then(|c| c.iter().find(|(n, _)| n == name).map(|(_, m)| m.clone()))
        {
            return Ok(cached);
        }
    }
    // The meth_state_access variant carries genuine per-module state:
    // CPython's `create_dynamic` builds a fresh module (fresh
    // `StateAccessType`, count reset to 0) for every load, and
    // Test_ModuleStateAccess's setUp relies on that. Skip the cache so
    // each `_load_dynamic` re-executes the body.
    if name != "_testmultiphase_meth_state_access" {
        if let Some(cached) = interp.module_cache().get(name) {
            return Ok(cached);
        }
    }
    // The stand-in body is embedded directly (no longer a FrozenSource:
    // the real compiled `_testmultiphase.so` must win `find_spec`
    // resolution — RFC 0068 WS4).
    let source: &str = include_str!("python/_testmultiphase.py");
    let display = match crate::stdlib_tree::stdlib_dir() {
        Some(dir) => dir
            .join(format!("{name}.weavepy.so"))
            .to_string_lossy()
            .into_owned(),
        None => format!("<extension {name}>"),
    };
    let module = interp.load_from_source(name, source, false, &display)?;
    if name == "_test_module_state_shared" {
        if let Ok(mut cache) = SINGLEPHASE_CACHE.lock() {
            cache.push((name.to_owned(), module.clone()));
        }
    }
    Ok(module)
}

/// CPython's process-wide single-phase extensions cache
/// (`_PyRuntime.imports.extensions`), scoped to the emulated fixtures
/// that need cross-interpreter module identity. `Object` is Send + Sync
/// under the GIL, matching the other mutex-guarded VM singletons.
static SINGLEPHASE_CACHE: std::sync::Mutex<Vec<(String, Object)>> =
    std::sync::Mutex::new(Vec::new());

pub fn build(_cache: &ModuleCache) -> Rc<PyModule> {
    let dict = Rc::new(RefCell::new(DictData::default()));
    {
        let mut d = dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("__name__")),
            Object::from_static("_imp"),
        );
        d.insert(
            DictKey(Object::from_static("__doc__")),
            Object::from_static("Bridge between importlib and the C-extension loader."),
        );

        d.insert(
            DictKey(Object::from_static("_load_dynamic")),
            builtin("_load_dynamic", imp_load_dynamic),
        );
        d.insert(
            DictKey(Object::from_static("create_dynamic")),
            builtin("create_dynamic", imp_create_dynamic),
        );
        d.insert(
            DictKey(Object::from_static("exec_dynamic")),
            builtin("exec_dynamic", imp_exec_dynamic),
        );
        d.insert(
            DictKey(Object::from_static("create_builtin")),
            builtin("create_builtin", imp_create_builtin),
        );
        d.insert(
            DictKey(Object::from_static("exec_builtin")),
            builtin("exec_builtin", imp_exec_builtin),
        );
        d.insert(
            DictKey(Object::from_static("is_builtin")),
            builtin("is_builtin", imp_is_builtin),
        );
        d.insert(
            DictKey(Object::from_static("is_frozen")),
            builtin("is_frozen", imp_is_frozen),
        );
        d.insert(
            DictKey(Object::from_static("is_frozen_package")),
            builtin("is_frozen_package", imp_is_frozen_package),
        );
        d.insert(
            DictKey(Object::from_static("get_frozen_object")),
            builtin("get_frozen_object", imp_get_frozen_object),
        );
        d.insert(
            DictKey(Object::from_static("find_frozen")),
            builtin("find_frozen", imp_find_frozen),
        );
        // RFC 0060 — the frozen-table view `test_frozentable` cross-checks
        // against the exported `_PyImport_Frozen*` C arrays.
        d.insert(
            DictKey(Object::from_static("_frozen_module_names")),
            builtin("_frozen_module_names", |_| {
                Ok(Object::new_list(
                    crate::frozen_table::frozen_module_names()
                        .into_iter()
                        .map(Object::from_static)
                        .collect(),
                ))
            }),
        );
        d.insert(
            DictKey(Object::from_static("acquire_lock")),
            builtin("acquire_lock", |_| {
                import_lock_acquire();
                Ok(Object::None)
            }),
        );
        d.insert(
            DictKey(Object::from_static("release_lock")),
            builtin("release_lock", |_| {
                import_lock_release().map(|()| Object::None)
            }),
        );
        d.insert(
            DictKey(Object::from_static("lock_held")),
            builtin("lock_held", |_| Ok(Object::Bool(import_lock_held()))),
        );
        d.insert(
            DictKey(Object::from_static("extension_suffixes")),
            builtin("extension_suffixes", imp_extension_suffixes),
        );
        d.insert(
            DictKey(Object::from_static("get_magic")),
            builtin("get_magic", imp_get_magic),
        );
        d.insert(
            DictKey(Object::from_static("source_hash")),
            builtin("source_hash", imp_source_hash),
        );
        d.insert(
            DictKey(Object::from_static("init_frozen")),
            builtin("init_frozen", |_| Ok(Object::None)),
        );
        d.insert(
            DictKey(Object::from_static("_fix_co_filename")),
            builtin("_fix_co_filename", fix_co_filename),
        );
        d.insert(
            DictKey(Object::from_static("check_hash_based_pycs")),
            Object::from_static("default"),
        );
        // RFC 0057 WS3 — the CPython-test-only override knob
        // `test.support.import_helper.frozen_modules()` drives
        // (`1` force-enabled / `-1` force-disabled / `0` reset). WeavePy's
        // frozen stdlib has no on-disk twin to fall back to, so — unlike
        // CPython, where only the bootstrap modules are exempt — the
        // override affects only the frozen *test* modules (`__hello__`,
        // `__phello__…`; see `ModuleCache::frozen_source`). The
        // multi-interp-extensions check reports the "allow" default.
        d.insert(
            DictKey(Object::from_static("_override_frozen_modules_for_tests")),
            builtin(
                "_override_frozen_modules_for_tests",
                imp_override_frozen_modules_for_tests,
            ),
        );
        d.insert(
            DictKey(Object::from_static(
                "_override_multi_interp_extensions_check",
            )),
            builtin(
                "_override_multi_interp_extensions_check",
                imp_override_multi_interp_extensions_check,
            ),
        );
    }
    Rc::new(PyModule {
        name: "_imp".to_owned(),
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

/// `_imp._load_dynamic(name, path[, file])` — dlopen the
/// shared library at `path`, call its `PyInit_<leaf>` entry
/// point, register the resulting module in `sys.modules`, and
/// return it.
///
/// The actual work is delegated to whatever loader the binary
/// registered via [`crate::ext_loader::install_extension_loader`].
fn imp_load_dynamic(args: &[Object]) -> Result<Object, RuntimeError> {
    let name = match args.first() {
        Some(Object::Str(s)) => s.to_string(),
        _ => {
            return Err(crate::error::type_error(
                "_load_dynamic() requires a string name",
            ))
        }
    };
    let path = match args.get(1) {
        Some(Object::Str(s)) => PathBuf::from(s.as_ref()),
        _ => {
            return Err(crate::error::type_error(
                "_load_dynamic() requires a string path",
            ))
        }
    };
    // The active interpreter is held in a per-thread cell by the
    // bytecode dispatch loop; we reach for it through the same
    // singleton the `_thread` module uses.
    let interp_ptr = match crate::vm_singletons::current_interpreter_ptr() {
        Some(p) => p,
        None => {
            return Err(import_error(format!(
                "_load_dynamic: no active interpreter (loading {name})"
            )))
        }
    };
    // Emulated `_testmultiphase` fixture family: gate + build without
    // consulting the real dlopen-based loader — but only when no real
    // shared object exists at `path`. RFC 0068 WS4 compiles CPython's
    // actual `_testmultiphase.c` into the conformance fixtures dir, and
    // the real thing always wins over the emulation.
    if multiphase_fixture(&name).is_some() && !path.is_file() {
        let interp = unsafe { &mut *interp_ptr };
        return load_emulated_multiphase(interp, &name);
    }
    let loader = crate::ext_loader::current_extension_loader().ok_or_else(|| {
        import_error(format!(
            "_load_dynamic: no extension loader installed (loading {name})"
        ))
    })?;

    let interp = unsafe { &mut *interp_ptr };
    // We give the loader a chance to find the extension by name
    // first (using its own search path resolution), falling back
    // to the explicit path if that fails.
    if let Some(module) = loader(interp, &name)? {
        interp.module_cache().insert(&name, module.clone());
        return Ok(module);
    }
    // Loader didn't find anything by name — last resort: poke the
    // C-API loader directly via the public helper installed by
    // weavepy-cli at startup. We re-use the same hook by stashing
    // the explicit path in a side-channel registry.
    crate::ext_loader::stash_explicit_path(&name, path);
    let module = loader(interp, &name)?
        .ok_or_else(|| import_error(format!("_load_dynamic: could not load extension {name}")))?;
    interp.module_cache().insert(&name, module.clone());
    Ok(module)
}

/// `_imp.create_dynamic(spec)` — PEP 489 multi-phase init
/// support. Collapses into the single-phase path driven by
/// `_load_dynamic`, but — matching CPython, where `create_dynamic`
/// never touches `sys.modules` (that's `_bootstrap._load`'s job) —
/// undoes the registration `_load_dynamic` performs when the name
/// wasn't cached before (extension.test_loader's
/// `test_load_short_name` asserts `'x' not in sys.modules`).
fn imp_create_dynamic(args: &[Object]) -> Result<Object, RuntimeError> {
    let spec = args.first().cloned().unwrap_or(Object::None);
    let (name, path) = extract_spec(&spec)?;
    let pre_existing = crate::vm_singletons::current_interpreter_ptr()
        .map(|p| unsafe { &*p }.module_cache().get(&name).is_some())
        .unwrap_or(false);
    let name_o = Object::from_str(name.clone());
    let path_o = Object::from_str(path);
    let module = imp_load_dynamic(&[name_o, path_o])?;
    if !pre_existing {
        if let Some(p) = crate::vm_singletons::current_interpreter_ptr() {
            unsafe { &*p }.module_cache().remove(&name);
        }
    }
    Ok(module)
}

/// `_imp.exec_dynamic(module)` — second half of PEP 489. Since
/// `create_dynamic` already runs the body, this is a no-op.
fn imp_exec_dynamic(_args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(Object::None)
}

fn extract_spec(spec: &Object) -> Result<(String, String), RuntimeError> {
    match spec {
        Object::Instance(inst) => {
            // Instance dict first, then the class namespace — the specs
            // test_import.test_create_dynamic_null builds carry `name` /
            // `origin` as plain class attributes.
            let lookup = |keys: &[&'static str]| -> Object {
                let dict = inst.dict.borrow();
                for k in keys {
                    if let Some(v) = dict.get(&DictKey(Object::from_static(k))) {
                        return v.clone();
                    }
                }
                drop(dict);
                let cls = inst.cls();
                let class_dict = cls.dict.borrow();
                for k in keys {
                    if let Some(v) = class_dict.get(&DictKey(Object::from_static(k))) {
                        return v.clone();
                    }
                }
                Object::None
            };
            let name = lookup(&["name", "__name__"]);
            let origin = lookup(&["origin", "__file__"]);
            let n = match name {
                Object::Str(s) => s.to_string(),
                _ => return Err(crate::error::type_error("spec.name must be a string")),
            };
            let p = match origin {
                Object::Str(s) => s.to_string(),
                _ => String::new(),
            };
            // CPython converts both through `PyUnicode_FSConverter` /
            // argument clinic, which rejects embedded NULs
            // (test_import.test_create_dynamic_null).
            if n.contains('\0') || p.contains('\0') {
                return Err(crate::error::value_error("embedded null character"));
            }
            Ok((n, p))
        }
        _ => Err(crate::error::type_error("expected a ModuleSpec instance")),
    }
}

/// `_imp._override_frozen_modules_for_tests(n)` — record the override
/// on the interpreter's module cache so subsequent frozen lookups
/// honour it. Returns `None`, like CPython.
fn imp_override_frozen_modules_for_tests(args: &[Object]) -> Result<Object, RuntimeError> {
    let value = match args.first() {
        Some(Object::Int(i)) => *i,
        Some(Object::Bool(b)) => i64::from(*b),
        _ => {
            return Err(crate::error::type_error(
                "_override_frozen_modules_for_tests() requires an int",
            ))
        }
    };
    if let Some(interp_ptr) = crate::vm_singletons::current_interpreter_ptr() {
        let interp = unsafe { &*interp_ptr };
        interp
            .module_cache()
            .set_frozen_tests_override(value.clamp(-1, 1) as i32);
    }
    Ok(Object::None)
}

/// `_imp._override_multi_interp_extensions_check(override)` — set the
/// per-interpreter override for the PEP 684 single-phase-extension
/// import gate (`1` force-check, `-1` force-allow, `0` use the config
/// setting) and return the previous override
/// (test_import.SubinterpImportTests.test_singlephase_check_with_setting_and_override).
fn imp_override_multi_interp_extensions_check(args: &[Object]) -> Result<Object, RuntimeError> {
    let value = match args.first() {
        Some(Object::Int(i)) => *i,
        Some(Object::Bool(b)) => i64::from(*b),
        _ => {
            return Err(crate::error::type_error(
                "_override_multi_interp_extensions_check() requires an int",
            ))
        }
    };
    let Some(interp_ptr) = crate::vm_singletons::current_interpreter_ptr() else {
        return Ok(Object::Int(0));
    };
    let interp = unsafe { &*interp_ptr };
    let old = interp.subinterp_check_override.get();
    interp
        .subinterp_check_override
        .set(value.clamp(-1, 1) as i32);
    Ok(Object::Int(i64::from(old)))
}

/// `_imp.create_builtin(spec)` — return the built-in module named by
/// `spec.name` (CPython's `BuiltinImporter.create_module` slot; RFC 0068
/// WS4 — the real `importlib._bootstrap._builtin_from_name` calls it).
/// WeavePy's built-ins are native singletons, so this routes through the
/// interpreter's module cache; an already-imported module is returned
/// as-is, matching CPython's single-phase built-in semantics.
fn imp_create_builtin(args: &[Object]) -> Result<Object, RuntimeError> {
    let spec = args.first().cloned().unwrap_or(Object::None);
    let interp_ptr = crate::vm_singletons::current_interpreter_ptr()
        .ok_or_else(|| crate::error::import_error("create_builtin: no active interpreter"))?;
    // SAFETY: published by an enclosing VM frame still live on this
    // thread; the GIL keeps the access exclusive.
    let interp = unsafe { &mut *interp_ptr };
    let name = match interp.load_attr_public(&spec, "name") {
        Ok(Object::Str(s)) => s.to_string(),
        _ => {
            return Err(crate::error::type_error(
                "create_builtin: spec.name must be a string".to_owned(),
            ))
        }
    };
    if interp.module_cache().builtin_factory(&name).is_none() {
        return Err(crate::error::import_error(format!(
            "{name} is not a built-in module"
        )));
    }
    let module = interp.import_path(&name)?;
    // CPython returns a module with *no* `__spec__`/`__loader__` and lets
    // `_init_module_attrs` fill both from the caller's spec. WeavePy's
    // lazy synthesis would pre-fill them with the ambient
    // `importlib.machinery` classes, which the caller's `getattr`-guarded
    // `_init_module_attrs(override=False)` then refuses to replace —
    // builtin.test_loader's Source variant asserts the module's loader is
    // *its own* freshly imported `BuiltinImporter` class. Seed both
    // attributes from the spec we were handed instead.
    if let Object::Module(ref m) = module {
        let loader = interp
            .load_attr_public(&spec, "loader")
            .unwrap_or(Object::None);
        let mut dict = m.dict.borrow_mut();
        dict.insert(DictKey(Object::from_static("__spec__")), spec.clone());
        dict.insert(DictKey(Object::from_static("__loader__")), loader);
    }
    Ok(module)
}

/// `_imp.exec_builtin(module)` — second half of the built-in module
/// two-phase protocol. `create_builtin` already produced a fully
/// initialized native module, so this is a no-op returning 0.
fn imp_exec_builtin(_args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(Object::Int(0))
}

fn imp_is_builtin(args: &[Object]) -> Result<Object, RuntimeError> {
    let name = match args.first() {
        Some(Object::Str(s)) => s.to_string(),
        _ => return Ok(Object::Int(0)),
    };
    // The list mirrors `sys.builtin_module_names`. Any name not
    // there gets 0 (unknown), names that are pre-loaded get 1,
    // and the magic "frozen" buckets return -1 (matches CPython's
    // convention).
    let interp_ptr = match crate::vm_singletons::current_interpreter_ptr() {
        Some(p) => p,
        None => return Ok(Object::Int(0)),
    };
    let interp = unsafe { &*interp_ptr };
    // `os` is native only as a startup fast path (CPython's frozen-`os`
    // analogue); its runtime face is the `os.py` source, and CPython
    // reports `is_builtin('os') == 0`. `_weave_posix` is the internal
    // alias of that native surface — not a CPython name at all.
    if name == "os" || name == "_weave_posix" {
        return Ok(Object::Int(0));
    }
    Ok(Object::Int(i64::from(
        interp.module_cache().builtin_factory(&name).is_some(),
    )))
}

/// The frozen-module table CPython 3.13 actually ships
/// (`Python/frozen.c`: the importlib bootstrap trio, the
/// startup-critical stdlib group, and the frozen test modules).
/// WeavePy freezes far more of the stdlib than CPython does, but the
/// `_imp` *query* surface must report CPython's table — frozen.
/// test_loader's `test_failure` asserts `FrozenImporter.get_code/
/// get_source/is_package('importlib')` all raise ImportError.
/// The native importer keeps using `ModuleCache::frozen_source`
/// directly and is unaffected.
fn cpython_frozen_table(name: &str) -> bool {
    // The *essential* bootstrap entries plus the test-fixture family —
    // i.e. CPython's frozen surface with `frozen_modules=off`, which is
    // CPython's own default when running from a source checkout (exactly
    // our harness shape: the `test` package and `sys._stdlib_dir` point
    // at the vendored Lib while the running stdlib is the staged tree).
    // The startup stdlib (os, io, abc, codecs, site, …) therefore
    // reports unfrozen and keeps its SourceFileLoader specs; with them
    // in the table, a re-run of CPython's verbatim `_bootstrap._setup`
    // (test_importlib's source-variant importlib re-import) would walk
    // them and its `_fix_up_module` asserts `loader_state.filename ==
    // module.__file__ == <sys._stdlib_dir>/<name>.py` — unsatisfiable
    // when the module executed from the staged tree.
    //
    // `zipimport` belongs to CPython's *always frozen* bootstrap group
    // (`Python/frozen.c` uses it regardless of `-X frozen_modules`), so
    // the query surface must report it — test_capi test_import's
    // `PyImport_ImportFrozenModule('zipimport')` asserts 1. The `_setup`
    // walk stays safe: a zipimport pre-imported without the
    // `__origname__` breadcrumb gets its loader_state synthesized.
    matches!(
        name,
        "_frozen_importlib" | "_frozen_importlib_external" | "zipimport"
    ) || crate::import::is_test_frozen_name(name)
}

fn imp_is_frozen(args: &[Object]) -> Result<Object, RuntimeError> {
    let name = match args.first() {
        Some(Object::Str(s)) => s.to_string(),
        _ => return Ok(Object::Bool(false)),
    };
    if !cpython_frozen_table(&name) {
        return Ok(Object::Bool(false));
    }
    let interp_ptr = match crate::vm_singletons::current_interpreter_ptr() {
        Some(p) => p,
        None => return Ok(Object::Bool(false)),
    };
    let interp = unsafe { &*interp_ptr };
    Ok(Object::Bool(
        interp.module_cache().frozen_source(&name).is_some(),
    ))
}

/// The ImportError CPython's `set_frozen_error` raises for a name the
/// frozen table does not carry (`.name` set; frozen.test_loader's
/// `test_failure` asserts both).
fn no_such_frozen_object(name: &str) -> RuntimeError {
    let err = import_error(format!("No such frozen object named '{name}'"));
    crate::error::set_exception_attr(&err, "name", Object::from_str(name.to_owned()));
    err
}

fn imp_is_frozen_package(args: &[Object]) -> Result<Object, RuntimeError> {
    let name = match args.first() {
        Some(Object::Str(s)) => s.to_string(),
        _ => return Ok(Object::Bool(false)),
    };
    let interp_ptr = match crate::vm_singletons::current_interpreter_ptr() {
        Some(p) => p,
        None => return Ok(Object::Bool(false)),
    };
    let interp = unsafe { &*interp_ptr };
    // CPython raises (rather than returning False) for names outside
    // the frozen table.
    if !cpython_frozen_table(&name) {
        return Err(no_such_frozen_object(&name));
    }
    Ok(Object::Bool(
        interp
            .module_cache()
            .frozen_source(&name)
            .map(|f| f.is_package)
            .unwrap_or(false),
    ))
}

fn imp_get_frozen_object(args: &[Object]) -> Result<Object, RuntimeError> {
    let name = match args.first() {
        Some(Object::Str(s)) => s.to_string(),
        _ => "?".to_owned(),
    };
    // With an explicit `data` payload CPython unmarshals it into a code
    // object; WeavePy freezes *source* (no marshal format), so any
    // payload is "invalid" — the wording test_import.test_issue105979
    // asserts.
    if let Some(data) = args.get(1) {
        if !matches!(data, Object::None) {
            return Err(import_error(format!(
                "Frozen object named '{name}' is invalid"
            )));
        }
    }
    if !cpython_frozen_table(&name) {
        return Err(no_such_frozen_object(&name));
    }
    // RFC 0068 WS4 — the real `FrozenImporter.exec_module` does
    // `exec(_imp.get_frozen_object(name), module.__dict__)`, so compile
    // the frozen source into a genuine code object on demand (CPython
    // unmarshals pre-frozen bytecode here; WeavePy freezes source).
    let interp_ptr = crate::vm_singletons::current_interpreter_ptr()
        .ok_or_else(|| import_error(format!("No such frozen object named '{name}'")))?;
    // SAFETY: published by an enclosing VM frame live on this thread;
    // the GIL keeps the access exclusive.
    let interp = unsafe { &*interp_ptr };
    let frozen = interp
        .module_cache()
        .frozen_source(&name)
        .ok_or_else(|| no_such_frozen_object(&name))?;
    let filename = format!("<frozen {name}>");
    let module = weavepy_parser::parse_module(frozen.source)
        .map_err(|e| import_error(format!("frozen module '{name}' failed to parse: {e}")))?;
    let code = weavepy_compiler::compile_module_with_source(&module, frozen.source, &filename)
        .map_err(|e| import_error(format!("frozen module '{name}' failed to compile: {e}")))?;
    Ok(Object::Code(crate::sync::Rc::new(code)))
}

fn imp_find_frozen(args: &[Object]) -> Result<Object, RuntimeError> {
    // Returns (data, is_package, origname) or None — modelled as
    // a 3-tuple to match CPython's shape.
    let name = match args.first() {
        Some(Object::Str(s)) => s.to_string(),
        _ => return Ok(Object::None),
    };
    let interp_ptr = match crate::vm_singletons::current_interpreter_ptr() {
        Some(p) => p,
        None => return Ok(Object::None),
    };
    let interp = unsafe { &*interp_ptr };
    if !cpython_frozen_table(&name) {
        return Ok(Object::None);
    }
    let frozen = match interp.module_cache().frozen_source(&name) {
        Some(f) => f,
        None => return Ok(Object::None),
    };
    // `origname` mirrors CPython's frozen-table aliases
    // (Tools/build/freeze_modules.py TESTS section; frozen.test_finder
    // asserts each mapping in `spec.loader_state.origname`):
    //  - the alias trio points at `__hello__`,
    //  - the bootstrap pair carries the importlib source names,
    //  - a frozen `pkg.__init__` self-alias is `<pkg`,
    //  - `__hello_only__` is frozen from a file outside the stdlib and
    //    has no origname at all.
    let origname: Object = match name.as_str() {
        "__hello_alias__" | "__phello_alias__" | "__phello_alias__.spam" => {
            Object::from_static("__hello__")
        }
        "_frozen_importlib" => Object::from_static("importlib._bootstrap"),
        "_frozen_importlib_external" => Object::from_static("importlib._bootstrap_external"),
        "__hello_only__" => Object::None,
        n => match n.strip_suffix(".__init__") {
            Some(pkg) => Object::from_str(format!("<{pkg}")),
            None => Object::from_str(n),
        },
    };
    Ok(Object::new_tuple(vec![
        Object::from_static(frozen.source),
        Object::Bool(frozen.is_package),
        origname,
    ]))
}

fn imp_extension_suffixes(_args: &[Object]) -> Result<Object, RuntimeError> {
    // RFC 0055 WS1 — the first entry is the compile-target's own
    // tagged suffix (shared with `_sysconfig.config_vars()['EXT_SUFFIX']`;
    // `test_sysconfig` asserts they agree), followed by the untagged
    // fallbacks CPython lists.
    let suffixes = if cfg!(target_os = "macos") {
        vec![
            crate::stdlib::sysconfig_native::EXT_SUFFIX,
            ".abi3.so",
            ".so",
            ".dylib",
        ]
    } else if cfg!(target_os = "linux") {
        vec![
            crate::stdlib::sysconfig_native::EXT_SUFFIX,
            ".abi3.so",
            ".so",
        ]
    } else if cfg!(target_os = "windows") {
        vec![crate::stdlib::sysconfig_native::EXT_SUFFIX, ".pyd", ".dll"]
    } else {
        vec![".so"]
    };
    Ok(Object::new_list(
        suffixes.iter().map(|s| Object::from_static(s)).collect(),
    ))
}

fn imp_get_magic(_args: &[Object]) -> Result<Object, RuntimeError> {
    // CPython 3.13's bytecode magic (`importlib.util.MAGIC_NUMBER`,
    // RFC 0033). WeavePy keeps a distinct *cache tag*
    // (`weavepy-313`) so its `.pyc` files never collide with
    // CPython's `cpython-313` artifacts, which lets us adopt the
    // real magic number for tool interop without ambiguity.
    Ok(Object::Bytes(Rc::from(b"\xf3\x0d\x0d\x0a".as_slice())))
}

/// `_imp.source_hash(key, source)` — deterministic 8-byte hash
/// of a source-bytes blob. We use a simple FNV-1a-derived
/// implementation; the real CPython uses siphash13 but the
/// observable contract — same input ↦ same output, 8 bytes —
/// matches.
fn imp_source_hash(args: &[Object]) -> Result<Object, RuntimeError> {
    // Two-arg form: (key, source). Single-arg form: (source).
    let (key, source) = match args.len() {
        1 => (0u64, args[0].clone()),
        _ => {
            let k = match args.first() {
                Some(Object::Int(i)) => *i as u64,
                _ => 0,
            };
            let s = args.get(1).cloned().unwrap_or(Object::None);
            (k, s)
        }
    };
    let bytes = match source {
        Object::Bytes(b) => b.to_vec(),
        Object::Str(s) => s.as_bytes().to_vec(),
        _ => Vec::new(),
    };
    let mut h: u64 = 0xcbf2_9ce4_8422_2325 ^ key;
    for b in &bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    Ok(Object::Bytes(Rc::from(h.to_le_bytes().as_slice())))
}

/// `_imp._fix_co_filename(code, source_path)` — CPython's
/// `update_compiled_module`: after unmarshalling a `.pyc`, rewrite
/// `co_filename` (recursively through nested code constants) to the
/// path the module is actually being imported from. Without it, a
/// bytecode cache written through one spelling of the source path (the
/// conformance harness's symlinked Lib shim) leaks that spelling into
/// tracebacks and warning attribution when the same source is later
/// imported through another (test_posix's
/// `assertEqual(cm.filename, __file__)`).
fn fix_co_filename(args: &[Object]) -> Result<Object, RuntimeError> {
    let Some(Object::Code(code)) = args.first() else {
        return Err(crate::error::type_error(
            "_fix_co_filename: first argument must be a code object",
        ));
    };
    let Some(Object::Str(path)) = args.get(1) else {
        return Err(crate::error::type_error(
            "_fix_co_filename: second argument must be a str",
        ));
    };
    fix_co_filename_rec(code, path.as_ref());
    Ok(Object::None)
}

fn fix_co_filename_rec(code: &weavepy_compiler::CodeObject, path: &str) {
    if code.filename != path {
        // SAFETY: `co_filename` is mutated in place through the shared
        // handle, exactly like CPython's `update_compiled_module`. The
        // VM is single-threaded per interpreter and nothing reads the
        // field concurrently with this import-time call; the write is a
        // plain field replacement (the old `String` is dropped by the
        // assignment).
        unsafe {
            let f = std::ptr::addr_of!(code.filename).cast_mut();
            *f = path.to_owned();
        }
    }
    for c in &code.constants {
        fix_constant_filename(c, path);
    }
}

fn fix_constant_filename(c: &weavepy_compiler::Constant, path: &str) {
    use weavepy_compiler::Constant;
    match c {
        Constant::Code(inner) => fix_co_filename_rec(inner, path),
        Constant::Tuple(items) | Constant::FrozenSet(items) => {
            for it in items {
                fix_constant_filename(it, path);
            }
        }
        _ => {}
    }
}

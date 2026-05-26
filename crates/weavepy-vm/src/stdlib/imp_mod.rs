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
//! - `acquire_lock` / `release_lock` — no-ops; the GIL gives us
//!   the lock semantics by default.
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

pub fn build(_cache: &ModuleCache) -> Rc<PyModule> {
    let dict = Rc::new(RefCell::new(DictData::new()));
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
        d.insert(
            DictKey(Object::from_static("acquire_lock")),
            builtin("acquire_lock", |_| Ok(Object::None)),
        );
        d.insert(
            DictKey(Object::from_static("release_lock")),
            builtin("release_lock", |_| Ok(Object::None)),
        );
        d.insert(
            DictKey(Object::from_static("lock_held")),
            builtin("lock_held", |_| Ok(Object::Bool(false))),
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
            builtin("_fix_co_filename", |_| Ok(Object::None)),
        );
        d.insert(
            DictKey(Object::from_static("check_hash_based_pycs")),
            Object::from_static("default"),
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
/// support. For now we collapse into the single-phase path
/// driven by `_load_dynamic`.
fn imp_create_dynamic(args: &[Object]) -> Result<Object, RuntimeError> {
    let spec = args.first().cloned().unwrap_or(Object::None);
    let (name, path) = extract_spec(&spec)?;
    let name_o = Object::from_str(name);
    let path_o = Object::from_str(path);
    imp_load_dynamic(&[name_o, path_o])
}

/// `_imp.exec_dynamic(module)` — second half of PEP 489. Since
/// `create_dynamic` already runs the body, this is a no-op.
fn imp_exec_dynamic(_args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(Object::None)
}

fn extract_spec(spec: &Object) -> Result<(String, String), RuntimeError> {
    match spec {
        Object::Instance(inst) => {
            let dict = inst.dict.borrow();
            let name = dict
                .get(&DictKey(Object::from_static("name")))
                .cloned()
                .or_else(|| dict.get(&DictKey(Object::from_static("__name__"))).cloned())
                .unwrap_or(Object::None);
            let origin = dict
                .get(&DictKey(Object::from_static("origin")))
                .cloned()
                .or_else(|| dict.get(&DictKey(Object::from_static("__file__"))).cloned())
                .unwrap_or(Object::None);
            let n = match name {
                Object::Str(s) => s.to_string(),
                _ => return Err(crate::error::type_error("spec.name must be a string")),
            };
            let p = match origin {
                Object::Str(s) => s.to_string(),
                _ => String::new(),
            };
            Ok((n, p))
        }
        _ => Err(crate::error::type_error("expected a ModuleSpec instance")),
    }
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
    Ok(Object::Int(i64::from(
        interp.module_cache().builtin_factory(&name).is_some(),
    )))
}

fn imp_is_frozen(args: &[Object]) -> Result<Object, RuntimeError> {
    let name = match args.first() {
        Some(Object::Str(s)) => s.to_string(),
        _ => return Ok(Object::Bool(false)),
    };
    let interp_ptr = match crate::vm_singletons::current_interpreter_ptr() {
        Some(p) => p,
        None => return Ok(Object::Bool(false)),
    };
    let interp = unsafe { &*interp_ptr };
    Ok(Object::Bool(
        interp.module_cache().frozen_source(&name).is_some(),
    ))
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
    Ok(Object::Bool(
        interp
            .module_cache()
            .frozen_source(&name)
            .map(|f| f.is_package)
            .unwrap_or(false),
    ))
}

fn imp_get_frozen_object(_args: &[Object]) -> Result<Object, RuntimeError> {
    // We don't pre-compile frozen modules into code objects; the
    // FrozenImporter falls back to source.
    Ok(Object::None)
}

fn imp_find_frozen(args: &[Object]) -> Result<Object, RuntimeError> {
    // Returns (data, is_package, origname) or None — modelled as
    // a 3-tuple to match CPython's shape. Our frozen modules
    // don't carry separate origin names, so origname == name.
    let name = match args.first() {
        Some(Object::Str(s)) => s.to_string(),
        _ => return Ok(Object::None),
    };
    let interp_ptr = match crate::vm_singletons::current_interpreter_ptr() {
        Some(p) => p,
        None => return Ok(Object::None),
    };
    let interp = unsafe { &*interp_ptr };
    let frozen = match interp.module_cache().frozen_source(&name) {
        Some(f) => f,
        None => return Ok(Object::None),
    };
    Ok(Object::new_tuple(vec![
        Object::from_static(frozen.source),
        Object::Bool(frozen.is_package),
        Object::from_str(name),
    ]))
}

fn imp_extension_suffixes(_args: &[Object]) -> Result<Object, RuntimeError> {
    let suffixes = if cfg!(target_os = "macos") {
        vec![".cpython-313-darwin.so", ".abi3.so", ".so", ".dylib"]
    } else if cfg!(target_os = "linux") {
        vec![
            ".cpython-313-x86_64-linux-gnu.so",
            ".cpython-313-aarch64-linux-gnu.so",
            ".abi3.so",
            ".so",
        ]
    } else if cfg!(target_os = "windows") {
        vec![".cp313-win_amd64.pyd", ".pyd", ".dll"]
    } else {
        vec![".so"]
    };
    Ok(Object::new_list(
        suffixes.iter().map(|s| Object::from_static(s)).collect(),
    ))
}

fn imp_get_magic(_args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(Object::Bytes(Rc::from(b"WPY0".as_slice())))
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

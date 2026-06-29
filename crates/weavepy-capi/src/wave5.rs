//! RFC 0047 (wave 5): the CPython 3.13 C-API leaf tail that
//! **Cython-generated** extensions (and pandas, which is ~70% Cython)
//! link but that waves 1-4 had not yet exported.
//!
//! These were found the same way wave 4 found numpy's tail: diff the
//! undefined `Py*`/`_Py*` symbols of a Cython-generated
//! `*.cpython-313-*.so` (`nm -u`) against the symbols the host binary
//! already exports. Cython's `Cython/Utility/*.c` runtime leans on a
//! characteristic cluster of helpers — the "optional attribute" probes
//! (`PyObject_GetOptionalAttr*`), the fast method-call path
//! (`_PyObject_GetMethod` + `PyObject_CallMethodOneArg`), the interned
//! string fast-compare (`PyUnicode_EqualToUTF8*`), the presized-dict
//! constructor (`_PyDict_NewPresized`), the in-body `__dict__` pointer
//! (`_PyObject_GetDictPtr`), and the relative-import entry
//! (`PyImport_ImportModuleLevelObject`).
//!
//! Every function here is a thin delegator onto the wave-1/2/3 surface
//! (`crate::abstract_`, `crate::containers`, `crate::numbers`,
//! `crate::strings`, `crate::module`) or a sound no-op under WeavePy's
//! object model. None introduces new behaviour; they exist so a stock
//! Cython `.so` *links and runs*.

#![allow(clippy::missing_safety_doc)]

use core::ffi::{c_char, c_int, c_long, c_ulong, c_void};
use std::collections::HashMap;
use std::ptr;
use std::sync::{Mutex, OnceLock};

use weavepy_vm::object::Object;

use crate::object::PyObject;
use crate::types::PyTypeObject;

// ---------------------------------------------------------------------------
// Instance `__dict__` access
// ---------------------------------------------------------------------------

/// `_PyObject_GetDictPtr(obj)` — pointer to the in-body `__dict__` slot.
///
/// WeavePy keeps an instance's `__dict__` in a managed Rust cell, not at a
/// fixed `tp_dictoffset` inside the object body, so there is no in-body
/// `PyObject **dictptr` to hand back. CPython itself returns NULL for any
/// type with `tp_dictoffset == 0`, and every caller — Cython's
/// `__Pyx_GetAttr` / `__Pyx_PyObject_GetAttrStr`, CPython's own
/// `_PyObject_GenericGetAttrWithDict` — treats NULL as "no fast dict" and
/// falls back to `tp_getattro` / `PyObject_GenericGetAttr`, which WeavePy
/// services correctly. (A faithful in-body `tp_dictoffset` dict is a
/// documented wave-5 Non-goal.)
#[no_mangle]
pub extern "C" fn _PyObject_GetDictPtr(_obj: *mut PyObject) -> *mut *mut PyObject {
    ptr::null_mut()
}

// ---------------------------------------------------------------------------
// Optional attribute / item probes (CPython 3.13 additions)
// ---------------------------------------------------------------------------

/// Shared tail for the `*Optional*` probes: a present value is handed back
/// (1), a missing one clears the error and reports absence (0). Any failure
/// is treated as "missing", matching `crate::wave4::PyObject_GetOptionalAttr`.
unsafe fn optional_result(v: *mut PyObject, result: *mut *mut PyObject) -> c_int {
    if !v.is_null() {
        if !result.is_null() {
            unsafe { *result = v };
        } else {
            unsafe { crate::object::Py_DecRef(v) };
        }
        return 1;
    }
    crate::errors::clear_thread_local();
    if !result.is_null() {
        unsafe { *result = ptr::null_mut() };
    }
    0
}

#[no_mangle]
pub unsafe extern "C" fn PyObject_GetOptionalAttrString(
    obj: *mut PyObject,
    name: *const c_char,
    result: *mut *mut PyObject,
) -> c_int {
    let v = unsafe { crate::abstract_::PyObject_GetAttrString(obj, name) };
    unsafe { optional_result(v, result) }
}

#[no_mangle]
pub unsafe extern "C" fn PyMapping_GetOptionalItem(
    obj: *mut PyObject,
    key: *mut PyObject,
    result: *mut *mut PyObject,
) -> c_int {
    let v = unsafe { crate::abstract_::PyObject_GetItem(obj, key) };
    unsafe { optional_result(v, result) }
}

#[no_mangle]
pub unsafe extern "C" fn PyMapping_GetOptionalItemString(
    obj: *mut PyObject,
    key: *const c_char,
    result: *mut *mut PyObject,
) -> c_int {
    let v = unsafe { crate::abstract_::PyMapping_GetItemString(obj, key) };
    unsafe { optional_result(v, result) }
}

// ---------------------------------------------------------------------------
// Fast method call path
// ---------------------------------------------------------------------------

/// `_PyObject_GetMethod(obj, name, *method)` — the fast method-load path.
///
/// CPython returns 1 when `name` resolves to an *unbound* method on the
/// type (so the caller invokes it with `obj` as the first argument) and 0
/// when it is an ordinary (already-bound) attribute. WeavePy resolves the
/// attribute through the VM's binding protocol, which yields a value that
/// is *already* callable with the user arguments — so the universally
/// correct answer is 0 ("bound"): `*method` is the bound attribute and the
/// caller invokes `(*method)(args...)`. Cython's `__Pyx_PyObject_GetMethod`
/// handles both return values, so this is sound (it just never takes the
/// micro-optimised unbound branch).
#[no_mangle]
pub unsafe extern "C" fn _PyObject_GetMethod(
    obj: *mut PyObject,
    name: *mut PyObject,
    method: *mut *mut PyObject,
) -> c_int {
    let v = unsafe { crate::abstract_::PyObject_GetAttr(obj, name) };
    if !method.is_null() {
        unsafe { *method = v };
    } else if !v.is_null() {
        unsafe { crate::object::Py_DecRef(v) };
    }
    0
}

/// `PyObject_CallMethodOneArg(self, name, arg)` — `self.name(arg)`.
#[no_mangle]
pub unsafe extern "C" fn PyObject_CallMethodOneArg(
    self_: *mut PyObject,
    name: *mut PyObject,
    arg: *mut PyObject,
) -> *mut PyObject {
    let meth = unsafe { crate::abstract_::PyObject_GetAttr(self_, name) };
    if meth.is_null() {
        return ptr::null_mut();
    }
    let r = unsafe { crate::abstract_::PyObject_CallOneArg(meth, arg) };
    unsafe { crate::object::Py_DecRef(meth) };
    r
}

// ---------------------------------------------------------------------------
// Dict / number tail
// ---------------------------------------------------------------------------

/// `_PyDict_NewPresized(minused)` — a private hint-sized constructor.
/// WeavePy's dict grows on demand, so the presize hint is informational.
#[no_mangle]
pub unsafe extern "C" fn _PyDict_NewPresized(_minused: isize) -> *mut PyObject {
    unsafe { crate::containers::PyDict_New() }
}

/// `PyLong_AsInt(obj)` — the 3.13 public spelling of the bounds-checked
/// `int` conversion Cython reaches for (`__Pyx_PyInt_As_int`).
#[no_mangle]
pub unsafe extern "C" fn PyLong_AsInt(obj: *mut PyObject) -> c_int {
    let v = unsafe { crate::numbers::PyLong_AsLong(obj) };
    // A pending error from the inner `long` conversion (overflow, wrong
    // type) propagates unchanged.
    if !unsafe { crate::errors::PyErr_Occurred() }.is_null() {
        return -1;
    }
    if v < c_long::from(c_int::MIN) || v > c_long::from(c_int::MAX) {
        crate::errors::set_overflow_error("Python int too large to convert to C int");
        return -1;
    }
    v as c_int
}

// ---------------------------------------------------------------------------
// Import tail
// ---------------------------------------------------------------------------

/// Read a `str`-valued entry out of a globals **dict** (a module's
/// `__dict__`, which is what Cython's `__Pyx_Import` passes). Returns
/// `None` when the dict lacks the key or its value is not a string.
fn globals_str(globals: *mut PyObject, key: &'static str) -> Option<String> {
    if globals.is_null() {
        return None;
    }
    match unsafe { crate::object::clone_object(globals) } {
        Object::Dict(d) => match d
            .borrow()
            .get(&weavepy_vm::object::DictKey(Object::from_static(key)))
        {
            Some(Object::Str(s)) => Some(s.to_string()),
            _ => None,
        },
        // A module object can also be handed in (CPython accepts either);
        // resolve through its dict.
        Object::Module(m) => match m
            .dict
            .borrow()
            .get(&weavepy_vm::object::DictKey(Object::from_static(key)))
        {
            Some(Object::Str(s)) => Some(s.to_string()),
            _ => None,
        },
        _ => None,
    }
}

/// True iff a globals dict (or module) defines `key` (any value). Used to
/// detect `__path__` (a module that is itself a package).
fn globals_has_key(globals: *mut PyObject, key: &'static str) -> bool {
    if globals.is_null() {
        return false;
    }
    match unsafe { crate::object::clone_object(globals) } {
        Object::Dict(d) => d
            .borrow()
            .get(&weavepy_vm::object::DictKey(Object::from_static(key)))
            .is_some(),
        Object::Module(m) => m
            .dict
            .borrow()
            .get(&weavepy_vm::object::DictKey(Object::from_static(key)))
            .is_some(),
        _ => false,
    }
}

/// Determine the importing module's package, mirroring CPython's
/// `importlib._bootstrap._calc___package__`: prefer an explicit, non-empty
/// `__package__`; otherwise derive it from `__name__` (the name itself if
/// the module is a package — it has `__path__` — else the name with its
/// last dotted component stripped).
fn calc_package(globals: *mut PyObject) -> Option<String> {
    if let Some(pkg) = globals_str(globals, "__package__") {
        if !pkg.is_empty() {
            return Some(pkg);
        }
    }
    let name = globals_str(globals, "__name__")?;
    if globals_has_key(globals, "__path__") {
        Some(name)
    } else {
        Some(match name.rsplit_once('.') {
            Some((head, _)) => head.to_string(),
            None => String::new(),
        })
    }
}

/// Resolve a relative import to its absolute dotted name, mirroring
/// CPython's `importlib._bootstrap._resolve_name`: strip `level - 1`
/// trailing components off `package`, then append `name` (which may be
/// empty for `from . import x`).
fn resolve_relative_name(name: &str, package: &str, level: c_int) -> Result<String, String> {
    if package.is_empty() {
        return Err("attempted relative import with no known parent package".to_owned());
    }
    let level = level as usize;
    // `package.rsplitn(level, '.')` yields at most `level` pieces, the last
    // of which is the surviving left-hand base. Fewer pieces than `level`
    // means the relative import reaches above the top-level package.
    let pieces: Vec<&str> = package.rsplitn(level, '.').collect();
    if pieces.len() < level {
        return Err("attempted relative import beyond top-level package".to_owned());
    }
    let base = *pieces.last().unwrap();
    if name.is_empty() {
        Ok(base.to_owned())
    } else {
        Ok(format!("{base}.{name}"))
    }
}

/// `PyImport_ImportModuleLevelObject(name, globals, locals, fromlist, level)`
/// — the entry point behind Cython's `__Pyx_Import` and CPython's `import`
/// statement.
///
/// `level == 0` is a plain absolute import of `name`. `level > 0` is a
/// relative import resolved against the importing module's package
/// (`globals['__package__']`, else derived from `__name__`), exactly as
/// `importlib._bootstrap._gcd_import` does — numpy's `numpy/random/*.pyx`
/// and pandas both `from ._sibling cimport …`, so this must work. When a
/// non-empty `fromlist` names submodules that are not already attributes
/// of the imported package (`from . import _pcg64`), each is imported as
/// `package.sub`, matching CPython's `_handle_fromlist`.
#[no_mangle]
pub unsafe extern "C" fn PyImport_ImportModuleLevelObject(
    name: *mut PyObject,
    globals: *mut PyObject,
    _locals: *mut PyObject,
    fromlist: *mut PyObject,
    level: c_int,
) -> *mut PyObject {
    // `name` may be NULL/empty for `from . import x`; treat as "".
    let name_str = if name.is_null() {
        String::new()
    } else {
        match unsafe { crate::object::clone_object(name) } {
            Object::Str(s) => s.to_string(),
            _ => {
                let utf8 = unsafe { crate::strings::PyUnicode_AsUTF8(name) };
                if utf8.is_null() {
                    return ptr::null_mut();
                }
                unsafe { core::ffi::CStr::from_ptr(utf8) }
                    .to_string_lossy()
                    .into_owned()
            }
        }
    };

    let abs_name = if level > 0 {
        let package = match calc_package(globals) {
            Some(p) => p,
            None => {
                crate::errors::set_import_error(
                    "attempted relative import with no known parent package",
                );
                return ptr::null_mut();
            }
        };
        match resolve_relative_name(&name_str, &package, level) {
            Ok(a) => a,
            Err(e) => {
                crate::errors::set_import_error(e);
                return ptr::null_mut();
            }
        }
    } else {
        name_str
    };

    let cname = match std::ffi::CString::new(abs_name.clone()) {
        Ok(c) => c,
        Err(_) => {
            crate::errors::set_value_error("PyImport_ImportModuleLevelObject: embedded NUL in name");
            return ptr::null_mut();
        }
    };
    let trace_imp = std::env::var_os("WEAVEPY_TRACE_IMPORT").is_some();
    if trace_imp {
        let fl = if fromlist.is_null() {
            "<none>".to_string()
        } else {
            format!("{:?}", unsafe { crate::object::clone_object(fromlist) })
        };
        eprintln!("[IMP] name={abs_name:?} level={level} fromlist={fl}");
    }
    let module = unsafe { crate::module::PyImport_ImportModule(cname.as_ptr()) };
    if module.is_null() {
        if trace_imp {
            eprintln!("[IMP] name={abs_name:?} -> MODULE NULL");
        }
        return ptr::null_mut();
    }

    // `_handle_fromlist`: ensure any submodule named in `fromlist` that is
    // not already an attribute of the imported package is imported as
    // `abs_name.sub`. A failure here is non-fatal (the name may be a plain
    // attribute the module defines itself, which Cython resolves with its
    // own `GetAttr`), so swallow the error and let the caller decide.
    if !fromlist.is_null() {
        let items = match unsafe { crate::object::clone_object(fromlist) } {
            Object::Tuple(t) => t.iter().cloned().collect::<Vec<_>>(),
            Object::List(l) => l.borrow().iter().cloned().collect::<Vec<_>>(),
            _ => Vec::new(),
        };
        for item in items {
            let Object::Str(sub) = item else { continue };
            let sub = sub.to_string();
            if sub == "*" {
                continue;
            }
            let has_attr = match unsafe { crate::object::clone_object(module) } {
                Object::Module(m) => m
                    .dict
                    .borrow()
                    .get(&weavepy_vm::object::DictKey(Object::from_str(sub.clone())))
                    .is_some(),
                _ => true,
            };
            if trace_imp {
                eprintln!("[IMP]   fromlist item {sub:?} has_attr={has_attr}");
                if !has_attr {
                    if let Object::Module(m) = unsafe { crate::object::clone_object(module) } {
                        let d = m.dict.borrow();
                        let n = d.len();
                        let has_via_iter = d.iter().any(|(k, _)| {
                            matches!(&k.0, Object::Str(s) if s.as_ref() == sub.as_str())
                        });
                        let sample: Vec<String> = d
                            .keys()
                            .filter_map(|k| match &k.0 {
                                Object::Str(s) => Some(s.to_string()),
                                _ => None,
                            })
                            .take(8)
                            .collect();
                        eprintln!(
                            "[IMP]     module={:?} nkeys={n} has_via_iter={has_via_iter} sample={sample:?}",
                            m.name
                        );
                    }
                }
            }
            if has_attr {
                continue;
            }
            if let Ok(subc) = std::ffi::CString::new(format!("{abs_name}.{sub}")) {
                let subm = unsafe { crate::module::PyImport_ImportModule(subc.as_ptr()) };
                if subm.is_null() {
                    crate::errors::clear_thread_local();
                } else {
                    unsafe { crate::object::Py_DecRef(subm) };
                }
            }
        }
    }

    module
}

// ---------------------------------------------------------------------------
// Type MRO lookup
// ---------------------------------------------------------------------------

/// Dedup cache for [`_PyType_Lookup`] results, keyed by
/// `(type pointer, attribute name)`. CPython's `_PyType_Lookup` returns a
/// *borrowed* reference owned by the type's (immortal) MRO cache, so callers
/// `Py_INCREF`/`Py_DECREF` around their use and never free the base
/// reference. WeavePy has no such C-level descriptor table, so we mint one
/// box per `(type, name)` and keep it here: that both honours the borrowed
/// contract (no per-call leak on the method-call fast path that calls this)
/// and gives the pointer identity stability `__Pyx_setup_reduce` compares on.
/// Entries live for the process (types are immortal); a runtime
/// reassignment of a cached attribute is a documented bound (CPython would
/// invalidate via `PyType_Modified`).
fn type_lookup_cache() -> &'static Mutex<HashMap<(usize, String), usize>> {
    static CACHE: OnceLock<Mutex<HashMap<(usize, String), usize>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// `_PyType_Lookup(type, name)` — walk `type`'s MRO for `name`, returning the
/// raw descriptor (no `__get__` binding) as a borrowed reference, or NULL
/// (no error set) when absent. Delegates to the VM's `TypeObject::lookup`,
/// which is the faithful MRO walk.
#[no_mangle]
pub unsafe extern "C" fn _PyType_Lookup(
    tp: *mut PyTypeObject,
    name: *mut PyObject,
) -> *mut PyObject {
    if tp.is_null() || name.is_null() {
        return ptr::null_mut();
    }
    let class = match unsafe { crate::object::clone_object(tp as *mut PyObject) } {
        Object::Type(t) => t,
        _ => return ptr::null_mut(),
    };
    let name_s = match unsafe { crate::object::clone_object(name) } {
        Object::Str(s) => s.to_string(),
        _ => return ptr::null_mut(),
    };
    let key = (tp as usize, name_s.clone());
    if let Some(&p) = type_lookup_cache().lock().unwrap().get(&key) {
        return p as *mut PyObject;
    }
    let Some(descr) = class.lookup(&name_s) else {
        return ptr::null_mut();
    };
    let p = crate::object::into_owned(descr);
    type_lookup_cache().lock().unwrap().insert(key, p as usize);
    p
}

// ---------------------------------------------------------------------------
// Argument validation
// ---------------------------------------------------------------------------

/// `PyArg_ValidateKeywordArguments(kwds)` — assert every key in the keyword
/// dict is a string (1 on success, 0 with a `TypeError` otherwise). Cython
/// guards `tp_init` with this.
#[no_mangle]
pub unsafe extern "C" fn PyArg_ValidateKeywordArguments(kwds: *mut PyObject) -> c_int {
    if kwds.is_null() {
        return 1;
    }
    match unsafe { crate::object::clone_object(kwds) } {
        Object::Dict(d) => {
            for key in d.borrow().keys() {
                if !matches!(key.0, Object::Str(_)) {
                    crate::errors::set_type_error("keywords must be strings");
                    return 0;
                }
            }
            1
        }
        _ => {
            crate::errors::set_type_error("keyword arguments must be a dict");
            0
        }
    }
}

// ---------------------------------------------------------------------------
// GC / managed-dict tail — sound no-ops under WeavePy's object model.
//
// WeavePy's cycle collector is driven from the VM side (RFC 0044), and an
// instance's `__dict__` lives in a managed Rust cell, not an in-body
// `tp_dictoffset` slot. The CPython entry points Cython emits for these
// concepts therefore have nothing to do here; each is a sound no-op.
// ---------------------------------------------------------------------------

type Visitproc = unsafe extern "C" fn(*mut PyObject, *mut c_void) -> c_int;

/// `PyObject_VisitManagedDict(obj, visit, arg)` — GC traversal of an
/// object's managed dict. WeavePy's GC traces the dict natively, so the
/// C-level traversal visits nothing.
#[no_mangle]
pub unsafe extern "C" fn PyObject_VisitManagedDict(
    _obj: *mut PyObject,
    _visit: Option<Visitproc>,
    _arg: *mut c_void,
) -> c_int {
    0
}

/// `PyObject_ClearManagedDict(obj)` — clear an object's managed dict.
/// WeavePy owns the dict's lifetime on the VM side; nothing to clear here.
#[no_mangle]
pub unsafe extern "C" fn PyObject_ClearManagedDict(_obj: *mut PyObject) {}

/// `PyObject_GC_IsFinalized(obj)` — whether `obj`'s finalizer has run.
/// WeavePy doesn't expose tp_finalize resurrection through this probe.
#[no_mangle]
pub unsafe extern "C" fn PyObject_GC_IsFinalized(_obj: *mut PyObject) -> c_int {
    0
}

/// `PyObject_CallFinalizerFromDealloc(obj)` — run `obj`'s finalizer during
/// deallocation, returning -1 if it resurrected the object. WeavePy runs
/// finalizers through the VM; report "no resurrection" (0).
#[no_mangle]
pub unsafe extern "C" fn PyObject_CallFinalizerFromDealloc(_obj: *mut PyObject) -> c_int {
    0
}

// ---------------------------------------------------------------------------
// Version data symbol
// ---------------------------------------------------------------------------

/// `Py_Version` — the runtime `PY_VERSION_HEX` as a data symbol (3.11+).
/// Cython's `__Pyx_check_binary_version` masks `Py_Version & ~0xFFUL` and
/// compares it against the compile-time hex; WeavePy targets CPython
/// 3.13.0, so we publish `0x030d00f0` (3.13.0 final).
#[no_mangle]
pub static Py_Version: c_ulong = 0x030d_00f0;

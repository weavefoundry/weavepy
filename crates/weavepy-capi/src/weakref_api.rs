//! Faithful `weakref.ref` C type shell (RFC 0072 WS2).
//!
//! gevent's compiled `_gevent_c_ident` does exactly this in its
//! generated module init:
//!
//! ```c
//! /* from `ctypedef class weakref.ref [object PyWeakReference]` */
//! __pyx_ptype_ref = __Pyx_ImportType(module_weakref, "weakref", "ref",
//!                                    sizeof(PyWeakReference), Error);
//! /* then subclasses it: */
//! /* cdef class ValuedWeakRef(ref): cdef object value  */
//! __pyx_type_ValuedWeakRef.tp_base = __pyx_ptype_ref;
//! PyType_Ready(&__pyx_type_ValuedWeakRef);
//! ```
//!
//! so the Python-visible `weakref.ref` class must read back as a
//! `PyTypeObject*` whose `tp_basicsize == sizeof(PyWeakReference)` —
//! 64 on LP64 CPython 3.13 (`PyObject_HEAD` + `wr_object` +
//! `wr_callback` + `hash` + `wr_prev` + `wr_next` + `vectorcall`) —
//! and the Cython subclass places its cdef fields at offset 64. The
//! generic identity-box mirror reports the Rust box size (56) and
//! fails the `__Pyx_ImportType` size check
//! (`ValueError: weakref.ref size changed`).
//!
//! This module mints the byte-faithful shell (the RFC 0029/0072
//! datetime/greenlet discipline): `tp_basicsize = 64`, `BASETYPE`,
//! registered as an inline-instance type so VM weakref instances (and
//! C-subclass instances) cross with stable, size-correct bodies. The
//! `PyWeakReference` fields themselves stay zeroed — the referent /
//! callback / registry state lives VM-side in
//! `weavepy_vm::stdlib::weakref_real`, and Cython consumers declare
//! the struct opaque (`pass`) — only the *size* and the Python-level
//! behaviour are load-bearing. (A consumer of the deprecated
//! `PyWeakref_GET_OBJECT` field macro would read NULL; the supported
//! `PyWeakref_GetRef`/`GetObject` functions route through the VM.)
//!
//! The VM `ReferenceType` class is per-interpreter (thread-local), so
//! resolution follows the `dt_identity` pattern: every live class that
//! validates as the genuine builtin maps to the one process-global
//! shell.

use std::ffi::CString;
use std::ptr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use weavepy_vm::object::Object;
use weavepy_vm::stdlib::weakref_real;
use weavepy_vm::sync::Rc;
use weavepy_vm::types::TypeObject;

use crate::layout::tpflags;
use crate::object::{PyObject, PySsizeT};
use crate::types::PyTypeObject;
use std::ffi::c_void;
use weavepy_vm::object::DictKey;

/// `sizeof(PyWeakReference)` on LP64 CPython 3.13.
const SIZE_WEAKREF: PySsizeT = 64;

/// The minted shell (as `usize`; 0 = not yet minted).
static PTR_REF: AtomicUsize = AtomicUsize::new(0);
/// Mint lock.
static INIT_LOCK: Mutex<()> = Mutex::new(());

/// Identity map: a live VM `ReferenceType` class (`Rc::as_ptr`, per
/// interpreter) → the global shell. Same rationale as `dt_identity`:
/// correct across multiple interpreters in one process, and the keyed
/// `Rc` keeps the class alive while instances can reach C.
fn ref_identity() -> &'static Mutex<std::collections::HashMap<usize, usize>> {
    static MAP: std::sync::OnceLock<Mutex<std::collections::HashMap<usize, usize>>> =
        std::sync::OnceLock::new();
    MAP.get_or_init(|| Mutex::new(std::collections::HashMap::new()))
}

/// `tp_new` for the shell — CPython's `weakref___new__` shape. A
/// Cython `cdef class V(ref)` generated `tp_new` chains here directly
/// (`tp_base->tp_new(subtype, args, kwds)`), so this must mint a
/// **fully-wired** ref of the subclass: positional `(ob, callback=None)`,
/// keywords silently ignored (a subclass forwards its kwargs to its own
/// `__init__`; the base ignores them here, exactly like CPython).
unsafe extern "C" fn weakref_tp_new(
    type_: *mut PyTypeObject,
    args: *mut PyObject,
    _kwds: *mut PyObject,
) -> *mut PyObject {
    let cls = unsafe {
        let bridge = (*type_).bridge;
        if bridge.is_null() {
            crate::errors::set_type_error("weakref.__new__(): unbridged type");
            return ptr::null_mut();
        }
        (*bridge).clone()
    };
    let argv: Vec<Object> = if args.is_null() {
        Vec::new()
    } else {
        match unsafe { crate::object::clone_object(args) } {
            Object::Tuple(items) => items.iter().cloned().collect(),
            other => vec![other],
        }
    };
    if argv.is_empty() || argv.len() > 2 {
        crate::errors::set_type_error("__new__ expected at least 1 and at most 2 arguments");
        return ptr::null_mut();
    }
    let target = argv[0].clone();
    let callback = match argv.get(1) {
        None | Some(Object::None) => None,
        Some(cb) => Some(cb.clone()),
    };
    match weakref_real::capi_subclass_new(cls, target, callback) {
        Ok(v) => crate::object::into_owned(v),
        Err(e) => {
            crate::errors::set_pending_from_runtime(e);
            ptr::null_mut()
        }
    }
}

/// Idempotently mint the `PyWeakReference` shell.
fn ensure_weakref_type() -> *mut PyTypeObject {
    let existing = PTR_REF.load(Ordering::Acquire);
    if existing != 0 {
        return existing as *mut PyTypeObject;
    }
    let _guard = INIT_LOCK.lock();
    let existing = PTR_REF.load(Ordering::Acquire);
    if existing != 0 {
        return existing as *mut PyTypeObject;
    }
    let cname = CString::new("weakref.ReferenceType").expect("static name");
    let mut ty = PyTypeObject::new_zeroed();
    ty.head.ob_type = crate::types::PyType_Type.as_ptr();
    ty.tp_name = cname.into_raw();
    ty.tp_basicsize = SIZE_WEAKREF;
    ty.tp_itemsize = 0;
    ty.tp_dealloc = Some(crate::object::_PyWeavePy_Dealloc);
    ty.tp_flags = tpflags::DEFAULT | tpflags::BASETYPE | tpflags::READY;
    ty.tp_base = crate::types::PyBaseObject_Type.as_ptr();
    ty.tp_new = weakref_tp_new as *mut c_void;
    ty.bridge = ptr::null_mut();
    let p = Box::into_raw(Box::new(ty));
    crate::types::register_heap_type(p);
    crate::types::maybe_register_inline_type(p);
    PTR_REF.store(p as usize, Ordering::Release);
    p
}

/// Is `t` genuinely the builtin `weakref.ref` class? Decided by the
/// builtin flag + name + `__module__` — a user class merely named
/// `ReferenceType` is never `is_builtin`.
fn class_is_weakref_ref(t: &Rc<TypeObject>) -> bool {
    if !t.flags.is_builtin || t.name != "ReferenceType" {
        return false;
    }
    let key = DictKey(Object::from_static("__module__"));
    matches!(
        t.dict.borrow().get(&key),
        Some(Object::Str(s)) if &**s == "weakref"
    )
}

/// The faithful C type for the VM `weakref.ref` class —
/// [`crate::types::find_type_ptr`]'s hook (the datetime/greenlet
/// discipline). Wires the shell's `bridge` to the first live class
/// that resolves (it backs the C-side `tp_new` chain and `tp_base`
/// harvesting), and records every interpreter's class in the identity
/// map.
pub fn faithful_type_for_class(t: &Rc<TypeObject>) -> Option<*mut PyTypeObject> {
    if t.name != "ReferenceType" {
        return None;
    }
    let key = Rc::as_ptr(t) as usize;
    let cached = ref_identity()
        .lock()
        .ok()
        .and_then(|m| m.get(&key).copied());
    if let Some(p) = cached {
        return Some(p as *mut PyTypeObject);
    }
    if !class_is_weakref_ref(t) {
        return None;
    }
    let p = ensure_weakref_type();
    if let Ok(mut map) = ref_identity().lock() {
        map.insert(key, p as usize);
        unsafe {
            if (*p).bridge.is_null() {
                (*p).bridge = Box::into_raw(Box::new(t.clone()));
            }
        }
    }
    Some(p)
}

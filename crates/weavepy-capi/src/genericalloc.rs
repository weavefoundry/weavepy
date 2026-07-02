//! Generic object lifecycle helpers — `PyType_GenericAlloc`,
//! `PyType_GenericNew`, `_PyObject_New`, and the family.
//!
//! Extension types defined via [`PyType_FromSpec`] don't normally
//! ship their own `tp_alloc` — they rely on the runtime to allocate
//! the per-instance storage block, set up the `ob_refcnt`/`ob_type`
//! header, and (for variable-sized types) reserve `nitems`'s worth
//! of items behind the header. CPython exposes that boilerplate
//! through `PyType_GenericAlloc`; we mirror the surface here.
//!
//! ## Layout
//!
//! WeavePy's [`PyObjectBox`](crate::object::PyObjectBox) carries the
//! Python `Object` payload alongside the C-visible `(ob_refcnt,
//! ob_type)` header. Extensions, however, often want a *fixed-size
//! C struct* whose first field is `PyObject_HEAD` and whose
//! subsequent fields they index into directly. The `_smalltest`
//! `Counter` example does exactly this. Our generic alloc supports
//! both shapes by choosing the wider of:
//!
//! 1. `tp_basicsize` (the extension's declared size), or
//! 2. `sizeof(PyObjectBox)` (our internal box).
//!
//! and zero-initialising the storage. The first
//! `sizeof(PyObjectBox)` bytes look like a regular box (so we can
//! still walk the type, refcount, and `obj` payload through normal
//! Rust code paths) while the trailing `tp_basicsize -
//! sizeof(PyObjectBox)` bytes are the extension's playground.

use std::alloc::{alloc_zeroed, Layout};
use std::os::raw::c_int;
use std::ptr;

use weavepy_vm::object::Object;

use crate::object::{PayloadCell, PyObject, PyObjectBox, PySsizeT};
use crate::types::PyTypeObject;

/// `PyType_GenericAlloc(type, nitems)` — allocate a fresh instance.
///
/// Returns a pointer with refcount 1. Extension code that overrides
/// `tp_new` is expected to call `tp_alloc` (which defaults to this
/// helper) before initialising the payload.
#[no_mangle]
pub unsafe extern "C" fn PyType_GenericAlloc(
    ty: *mut PyTypeObject,
    nitems: PySsizeT,
) -> *mut PyObject {
    if ty.is_null() {
        crate::errors::set_runtime_error("PyType_GenericAlloc: NULL type");
        return ptr::null_mut();
    }
    crate::interp::ensure_initialised();

    if std::env::var_os("WEAVEPY_TRACE_CTOR").is_some() {
        eprintln!(
            "[CTOR] genericalloc name={} ty={:p} inline={} readied={} basicsize={} nitems={}",
            crate::types::ctor_trace_name(ty),
            ty,
            crate::types::is_inline_instance_type(ty),
            crate::types::is_readied_type(ty),
            unsafe { (*ty).tp_basicsize },
            nitems,
        );
    }

    // RFC 0045 (wave 3): an inline-storage type (`tp_basicsize >
    // sizeof(PyObject)`) gets a faithful `tp_basicsize`-wide body whose
    // fields live at their declared offsets — the `PyArrayObject` shape —
    // rather than the legacy `PyObjectBox` (whose Rust payload would sit
    // exactly where the extension expects `self->field`). The fresh
    // instance is owned by the VM and pinned for C's borrow.
    if crate::types::is_inline_instance_type(ty) {
        let body = crate::instance::make_inline_instance(ty, nitems);
        if body.is_null() {
            crate::errors::set_runtime_error("PyType_GenericAlloc: type is not bridged");
            return ptr::null_mut();
        }
        unsafe { crate::object::Py_IncRef(ty as *mut PyObject) };
        return body;
    }

    let basicsize = unsafe { (*ty).tp_basicsize };
    let itemsize = unsafe { (*ty).tp_itemsize };
    let total = basicsize.max(std::mem::size_of::<PyObjectBox>() as PySsizeT)
        + nitems.max(0) * itemsize.max(0);

    // Allocate raw zeroed storage with at least 8-byte alignment.
    let layout = match Layout::from_size_align(total as usize, std::mem::align_of::<PyObjectBox>())
    {
        Ok(l) => l,
        Err(_) => {
            crate::errors::set_runtime_error("PyType_GenericAlloc: invalid layout");
            return ptr::null_mut();
        }
    };
    let raw = unsafe { alloc_zeroed(layout) };
    if raw.is_null() {
        crate::errors::set_runtime_error("PyType_GenericAlloc: out of memory");
        return ptr::null_mut();
    }

    // Seed the payload with a real `Object::Instance` bound to the bridged
    // class, so the extension's `tp_new`/`tp_init` and
    // `PyObject_SetAttrString` operate on a genuine instance whose
    // `__dict__` round-trips through `clone_object`. This covers both stock
    // types finalised via `PyType_Ready` (RFC 0044, WS5) and the heap
    // mirrors minted by `install_user_type` for a *VM* class that a foreign
    // C `tp_new` allocates — pandas' `class NAType(C_NAType)`, whose cdef
    // base's `__pyx_tp_new` calls `NAType->tp_alloc(NAType, 0)` and expects
    // back a real `NAType` instance (not the `Object::None` placeholder).
    // A foreign (un-bridged) extension type keeps the historical `None`.
    let payload_obj = match unsafe { crate::types::bridge_type(ty) } {
        Some(cls) => Object::Instance(weavepy_vm::sync::Rc::new(
            weavepy_vm::types::PyInstance::new(cls),
        )),
        None => Object::None,
    };

    // Use placement-style initialisation: write a fresh PyObjectBox
    // header into the start of the allocation. We use ptr::write
    // (not deref-assign) because the underlying storage is
    // uninitialised.
    let bx = raw as *mut PyObjectBox;
    unsafe {
        ptr::write(
            bx,
            PyObjectBox {
                head: PyObject {
                    ob_refcnt: 1,
                    ob_type: ty,
                },
                payload: PayloadCell::from_object(payload_obj),
            },
        );
        crate::object::Py_IncRef(ty as *mut PyObject);
    }
    crate::object::register_minted(raw as *mut PyObject);
    if crate::object::freebox_trace_enabled() {
        let tyname = unsafe { crate::object::debug_type_name(raw as *mut PyObject) };
        if tyname.contains("Engine") {
            eprintln!("[ALLOC] genericalloc p=0x{:x} type={}", raw as usize, tyname);
        }
    }
    raw as *mut PyObject
}

/// `PyType_GenericNew(type, args, kwds)` — default `tp_new`. Calls
/// `tp_alloc` (or [`PyType_GenericAlloc`] if no custom alloc is
/// registered).
#[no_mangle]
pub unsafe extern "C" fn PyType_GenericNew(
    ty: *mut PyTypeObject,
    _args: *mut PyObject,
    _kwds: *mut PyObject,
) -> *mut PyObject {
    if ty.is_null() {
        crate::errors::set_runtime_error("PyType_GenericNew: NULL type");
        return ptr::null_mut();
    }
    if let Some(slot_table) = unsafe { crate::slottable::slot_table_for(ty) } {
        let alloc_slot = slot_table.get(crate::slottable::Py_tp_alloc);
        if !alloc_slot.is_null() {
            let f: unsafe extern "C" fn(*mut PyTypeObject, PySsizeT) -> *mut PyObject =
                unsafe { alloc_slot.cast() };
            return unsafe { f(ty, 0) };
        }
    }
    unsafe { PyType_GenericAlloc(ty, 0) }
}

/// `_PyObject_New(type)` — non-variadic alloc helper.
#[no_mangle]
pub unsafe extern "C" fn _PyObject_New(ty: *mut PyTypeObject) -> *mut PyObject {
    unsafe { PyType_GenericAlloc(ty, 0) }
}

/// `_PyObject_NewVar(type, nitems)` — variadic alloc helper.
#[no_mangle]
pub unsafe extern "C" fn _PyObject_NewVar(
    ty: *mut PyTypeObject,
    nitems: PySsizeT,
) -> *mut PyObject {
    unsafe { PyType_GenericAlloc(ty, nitems) }
}

/// `PyObject_Init(o, type)` — fill the header on a pre-allocated
/// block.
#[no_mangle]
pub unsafe extern "C" fn PyObject_Init(o: *mut PyObject, ty: *mut PyTypeObject) -> *mut PyObject {
    if o.is_null() {
        return ptr::null_mut();
    }
    unsafe {
        let head = &mut *o;
        head.ob_refcnt = 1;
        head.ob_type = ty;
    }
    o
}

/// `PyObject_InitVar(o, type, size)` — variadic init helper.
#[no_mangle]
pub unsafe extern "C" fn PyObject_InitVar(
    o: *mut PyObject,
    ty: *mut PyTypeObject,
    _size: PySsizeT,
) -> *mut PyObject {
    unsafe { PyObject_Init(o, ty) }
}

/// `PyObject_GenericGetAttr(o, name)` — default `__getattribute__`:
/// data descriptor → instance dict → class attr, the `object.__getattribute__`
/// body.
///
/// Crucially this is the **generic** body, not full attribute dispatch: a C
/// extension type routinely sets `tp_getattro = PyObject_GenericGetAttr` (or
/// calls it as the fallback inside its own `tp_getattro`, e.g. a proxy that
/// special-cases one name). Delegating to [`PyObject_GetAttr`] would re-enter
/// that very slot — the VM's `LOAD_ATTR` dispatches the type's
/// `__getattribute__`, which is the C slot — and recurse until the stack
/// overflows. For a bridged instance we therefore call the VM's *default*
/// `object.__getattribute__` body, which never re-dispatches the override.
#[no_mangle]
pub unsafe extern "C" fn PyObject_GenericGetAttr(
    o: *mut PyObject,
    name: *mut PyObject,
) -> *mut PyObject {
    if o.is_null() || name.is_null() {
        return ptr::null_mut();
    }
    let obj = unsafe { crate::object::clone_object(o) };
    // Only a bridged *instance* can route attribute access back through its
    // type's `tp_getattro`; every other shape keeps the historical full
    // dispatch (and so its `__getattr__` fallback, special getsets, …).
    if matches!(obj, Object::Instance(_)) {
        let key = match unsafe { crate::object::clone_object(name) } {
            Object::Str(s) => s.to_string(),
            _ => {
                crate::errors::set_pending(
                    Some(weavepy_vm::builtin_types::builtin_types().type_error.clone()),
                    Object::from_str("attribute name must be string"),
                );
                return ptr::null_mut();
            }
        };
        if let Some(res) = crate::interp::ensure_active(|| {
            crate::interp::with_interp_mut(|interp| interp.generic_getattr_public(&obj, &key))
        }) {
            return match res {
                Ok(v) => crate::object::into_owned(v),
                Err(e) => {
                    crate::errors::set_pending_from_runtime(e);
                    ptr::null_mut()
                }
            };
        }
    }
    unsafe { crate::abstract_::PyObject_GetAttr(o, name) }
}

/// `PyObject_GenericSetAttr(o, name, value)` — default `__setattr__` /
/// `__delattr__` (`value == NULL` deletes).
///
/// See [`PyObject_GenericGetAttr`]: this is the generic `object.__setattr__`
/// body, *not* full dispatch, so a C type whose `tp_setattro` calls
/// `PyObject_GenericSetAttr` does not recurse back into its own slot.
#[no_mangle]
pub unsafe extern "C" fn PyObject_GenericSetAttr(
    o: *mut PyObject,
    name: *mut PyObject,
    value: *mut PyObject,
) -> c_int {
    if o.is_null() || name.is_null() {
        return -1;
    }
    let obj = unsafe { crate::object::clone_object(o) };
    if matches!(obj, Object::Instance(_)) {
        let key = match unsafe { crate::object::clone_object(name) } {
            Object::Str(s) => s.to_string(),
            _ => {
                crate::errors::set_pending(
                    Some(weavepy_vm::builtin_types::builtin_types().type_error.clone()),
                    Object::from_str("attribute name must be string"),
                );
                return -1;
            }
        };
        let val = if value.is_null() {
            None
        } else {
            Some(unsafe { crate::object::clone_object(value) })
        };
        if let Some(res) = crate::interp::ensure_active(|| {
            crate::interp::with_interp_mut(|interp| {
                interp.generic_setattr_public(&obj, &key, val.clone())
            })
        }) {
            return match res {
                Ok(()) => 0,
                Err(e) => {
                    crate::errors::set_pending_from_runtime(e);
                    -1
                }
            };
        }
    }
    unsafe { crate::abstract_::PyObject_SetAttr(o, name, value) }
}

/// `PyObject_GenericGetDict(o, closure)` — return `o.__dict__`. Used
/// as the default getter for the `__dict__` descriptor on heap types.
#[no_mangle]
pub unsafe extern "C" fn PyObject_GenericGetDict(
    o: *mut PyObject,
    _closure: *mut std::ffi::c_void,
) -> *mut PyObject {
    if o.is_null() {
        return ptr::null_mut();
    }
    let obj = unsafe { crate::object::clone_object(o) };
    let dict = match &obj {
        Object::Module(m) => m.dict.clone(),
        Object::Instance(inst) => inst.dict.clone(),
        Object::Type(t) => t.dict.clone(),
        _ => {
            crate::errors::set_attribute_error("object has no __dict__");
            return ptr::null_mut();
        }
    };
    crate::object::into_owned(Object::Dict(dict))
}

/// `PyObject_GenericSetDict(o, value, closure)` — set `o.__dict__`.
#[no_mangle]
pub unsafe extern "C" fn PyObject_GenericSetDict(
    o: *mut PyObject,
    value: *mut PyObject,
    _closure: *mut std::ffi::c_void,
) -> c_int {
    if o.is_null() {
        return -1;
    }
    let obj = unsafe { crate::object::clone_object(o) };
    let new_dict = match unsafe { crate::object::clone_object(value) } {
        Object::Dict(rc) => rc,
        _ => {
            crate::errors::set_type_error("__dict__ must be a dict");
            return -1;
        }
    };
    match obj {
        Object::Instance(inst) => {
            // Replace contents (we can't replace the Rc since
            // PyInstance::dict is borrowed by reference elsewhere).
            let mut g = inst.dict.borrow_mut();
            g.clear();
            for (k, v) in new_dict.borrow().iter() {
                g.insert(k.clone(), v.clone());
            }
            0
        }
        Object::Module(m) => {
            let mut g = m.dict.borrow_mut();
            g.clear();
            for (k, v) in new_dict.borrow().iter() {
                g.insert(k.clone(), v.clone());
            }
            0
        }
        _ => {
            crate::errors::set_type_error("object has no settable __dict__");
            -1
        }
    }
}

/// `PyObject_HashNotImplemented(o)` — raise `TypeError("unhashable
/// type")` and return -1. Used as the canonical "this type is not
/// hashable" sentinel.
#[no_mangle]
pub unsafe extern "C" fn PyObject_HashNotImplemented(o: *mut PyObject) -> isize {
    if o.is_null() {
        return -1;
    }
    let obj = unsafe { crate::object::clone_object(o) };
    let name = match &obj {
        Object::Instance(inst) => inst.cls().name.clone(),
        _ => "object".to_owned(),
    };
    crate::errors::set_type_error(format!("unhashable type: '{name}'"));
    -1
}

/// `_Py_HashPointer(p)` — pointer-identity hash. Mirrors CPython's
/// implementation: rotate the high bits down so consecutive
/// pointers don't collide on the low-bit modulus.
#[no_mangle]
pub unsafe extern "C" fn _Py_HashPointer(p: *const std::ffi::c_void) -> isize {
    if p.is_null() {
        return 0;
    }
    let v = p as usize;
    let rotated = v.rotate_right(4);
    rotated as isize
}

/// Same as [`_Py_HashPointer`] but without the leading underscore;
/// CPython 3.13 promoted the symbol to the public ABI.
#[no_mangle]
pub unsafe extern "C" fn Py_HashPointer(p: *const std::ffi::c_void) -> isize {
    unsafe { _Py_HashPointer(p) }
}

/// `_Py_HashBytes(src, len)` — bytewise hash. We use Rust's default
/// hasher; the actual numeric value isn't observable through the
/// C-API (it's reduced modulo something that doesn't expose it).
#[no_mangle]
pub unsafe extern "C" fn _Py_HashBytes(src: *const std::ffi::c_void, len: PySsizeT) -> isize {
    if src.is_null() || len <= 0 {
        return 0;
    }
    use std::hash::{Hash, Hasher};
    let bytes = unsafe { std::slice::from_raw_parts(src as *const u8, len as usize) };
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut hasher);
    let h = hasher.finish() as isize;
    if h == -1 {
        -2
    } else {
        h
    }
}

/// `Py_GenericAlias(a, b)` — fallback for `list.__class_getitem__`-
/// style aliasing. Returns a fresh tuple `(a, b)` mimicking the
/// `types.GenericAlias` shape that CPython would build.
#[no_mangle]
pub unsafe extern "C" fn Py_GenericAlias(a: *mut PyObject, b: *mut PyObject) -> *mut PyObject {
    if a.is_null() || b.is_null() {
        return ptr::null_mut();
    }
    let oa = unsafe { crate::object::clone_object(a) };
    let ob = unsafe { crate::object::clone_object(b) };
    crate::object::into_owned(Object::new_tuple(vec![oa, ob]))
}

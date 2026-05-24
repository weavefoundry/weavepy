//! `PyObject` layout and the bridge to WeavePy's native [`Object`].
//!
//! Every C-extension-visible value is a heap-allocated [`PyObjectBox`]
//! whose first two fields ([`ob_refcnt`](PyObject::ob_refcnt) and
//! [`ob_type`](PyObject::ob_type)) match `struct _object` from
//! `Python.h` exactly. The remainder is private to this crate and
//! holds the [`weavepy_vm::object::Object`] payload that backs the
//! value.
//!
//! Pointers handed to C code are always `*mut PyObject` — i.e. a
//! pointer to the prefix of the box. Casting back to
//! `*mut PyObjectBox` is sound because the prefix is the first
//! field; we never move or reshape a live box.
//!
//! ## Reference counting
//!
//! - Newly-built boxes start at refcount **1**: the caller owns the
//!   reference. Returning the pointer to C "transfers" the
//!   reference; receiving a pointer back from C is implicitly
//!   "borrowing" unless documented otherwise.
//! - [`Py_IncRef`] bumps; [`Py_DecRef`] decrements; refcount zero
//!   drops the box (which in turn drops the underlying Rust
//!   `Object`).
//! - Singletons (`Py_None`, `Py_True`, `Py_False`,
//!   `Py_NotImplemented`, `Py_Ellipsis`) live in `static` storage
//!   with a sentinel "immortal" refcount; refcount mutations are
//!   no-ops on them.
//! - Static type objects (the bridged built-ins:
//!   `int`/`str`/`type`/etc.) are also immortal.

use std::ffi::c_void;
use std::ptr;

use weavepy_vm::object::Object;

use crate::types::PyTypeObject;

/// Layout matches `struct _object` in `Python.h` exactly.
///
/// The fields are deliberately `pub` and named to mirror CPython.
/// The C compiler dereferences `ob_refcnt` and `ob_type` directly
/// through this view (via [`Py_TYPE`]/[`Py_REFCNT`] macros).
#[repr(C)]
#[derive(Debug)]
pub struct PyObject {
    pub ob_refcnt: PySsizeT,
    pub ob_type: *mut PyTypeObject,
}

pub type PySsizeT = isize;
pub type PyHashT = isize;

/// Refcount value used to mark an object as immortal. Chosen to be
/// large enough that no realistic refcount churn ever decrements
/// the value to zero, matching CPython's
/// `_Py_IMMORTAL_REFCNT` sentinel.
pub const IMMORTAL_REFCNT: PySsizeT = (PySsizeT::MAX / 2) - 1;

/// Heap-allocated extended box.
///
/// The first field shadows [`PyObject`] exactly so a `*mut
/// PyObjectBox` is interchangeable with a `*mut PyObject` for the
/// fields the C ABI cares about.
#[repr(C)]
pub struct PyObjectBox {
    pub head: PyObject,
    pub payload: PayloadCell,
}

impl std::fmt::Debug for PyObjectBox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PyObjectBox")
            .field("ob_refcnt", &self.head.ob_refcnt)
            .field("ob_type", &self.head.ob_type)
            .field("payload", &self.payload)
            .finish()
    }
}

/// Per-box payload. Most boxes carry a single [`Object`]; some
/// (capsules, modules with C-side state) carry an additional
/// `void*` slot.
#[derive(Debug)]
pub struct PayloadCell {
    /// The bridged Rust object. `Object::None` is the sentinel
    /// "no payload" value used for static types whose identity
    /// does not depend on a wrapped object.
    pub obj: Object,
    /// Extra C-side state (capsule pointer, module-state, etc.).
    pub user_data: *mut c_void,
    /// Optional destructor invoked when the box is freed. Used by
    /// capsules.
    pub destructor: Option<unsafe extern "C" fn(*mut PyObject)>,
}

impl PayloadCell {
    pub fn from_object(obj: Object) -> Self {
        Self {
            obj,
            user_data: ptr::null_mut(),
            destructor: None,
        }
    }
}

/// Build a fresh box wrapping `obj`. Caller owns one reference.
///
/// SAFETY: the returned pointer must be released via
/// [`Py_DecRef`] (or by being handed off to the runtime, which
/// arranges its own decref).
#[allow(clippy::missing_safety_doc)]
pub fn into_owned(obj: Object) -> *mut PyObject {
    let ty = crate::types::type_for_object(&obj);
    let boxed = Box::new(PyObjectBox {
        head: PyObject {
            ob_refcnt: 1,
            ob_type: ty,
        },
        payload: PayloadCell::from_object(obj),
    });
    Box::into_raw(boxed) as *mut PyObject
}

/// Build a box that wraps `obj` and is associated with the given
/// type pointer (used when [`type_for_object`](crate::types::type_for_object)
/// alone isn't precise enough — e.g. when constructing an instance
/// of a heap-allocated user type from `PyType_FromSpec`).
pub fn into_owned_with_type(obj: Object, ty: *mut PyTypeObject) -> *mut PyObject {
    let boxed = Box::new(PyObjectBox {
        head: PyObject {
            ob_refcnt: 1,
            ob_type: ty,
        },
        payload: PayloadCell::from_object(obj),
    });
    Box::into_raw(boxed) as *mut PyObject
}

/// Clone the wrapped [`Object`] out of a box. The C-side reference
/// count is unchanged; the returned [`Object`] participates in the
/// usual `Rc`-driven sharing on the Rust side.
///
/// Singletons are short-circuited: the well-known
/// `Py_None` / `Py_True` / `Py_False` pointers map to the
/// corresponding [`Object`] variants without dereferencing the
/// box (which doesn't exist for statics).
#[allow(clippy::missing_safety_doc)]
pub unsafe fn clone_object(p: *mut PyObject) -> Object {
    if p.is_null() {
        return Object::None;
    }
    let head = unsafe { &*(p as *const PyObject) };
    if std::ptr::eq(head, crate::singletons::_Py_NoneStruct.as_ptr()) {
        return Object::None;
    }
    if std::ptr::eq(head, crate::singletons::_Py_TrueStruct.as_ptr()) {
        return Object::Bool(true);
    }
    if std::ptr::eq(head, crate::singletons::_Py_FalseStruct.as_ptr()) {
        return Object::Bool(false);
    }
    if std::ptr::eq(head, crate::singletons::_Py_NotImplementedStruct.as_ptr()) {
        return Object::None;
    }
    if std::ptr::eq(head, crate::singletons::_Py_EllipsisObject.as_ptr()) {
        return Object::None;
    }
    // PyTypeObject extends PyObject; static type slots short-circuit
    // here because their bridge field carries the native Rc. We
    // *must* verify the metaclass FIRST — reading `(*ty).bridge`
    // requires that `p` actually point at a `PyTypeObjectBox`.
    if !head.ob_type.is_null() && std::ptr::eq(head.ob_type, crate::types::PyType_Type.as_ptr()) {
        if let Some(t) = unsafe { crate::types::bridge_type(p as *mut crate::types::PyTypeObject) }
        {
            return Object::Type(t);
        }
    }
    let bx = unsafe { &*(p as *const PyObjectBox) };
    bx.payload.obj.clone()
}

/// Drop a box's storage, running its destructor (if any) first.
///
/// SAFETY: `p` must be a heap-allocated box previously produced by
/// [`into_owned`] / [`into_owned_with_type`] / capsule / module
/// helpers. Static singletons short-circuit through the immortal
/// check in [`Py_DecRef`].
unsafe fn free_box(p: *mut PyObject) {
    let bx = unsafe { Box::from_raw(p as *mut PyObjectBox) };
    if let Some(d) = bx.payload.destructor {
        // We need to give the destructor a `*mut PyObject` view; it
        // expects to operate on the live box. Re-pack the contents
        // briefly so the destructor sees the same address it stored
        // when the capsule was created.
        let raw = Box::into_raw(bx);
        unsafe { d(raw as *mut PyObject) };
        // Drop the (now-empty) box.
        let _ = unsafe { Box::from_raw(raw) };
    } else {
        drop(bx);
    }
}

/// Increment the C-visible refcount of `op`. No-op on null and on
/// immortal singletons.
///
/// # Safety
///
/// `op` must be either null or a valid pointer into a live [`PyObjectBox`]
/// or a static singleton struct.
#[no_mangle]
pub unsafe extern "C" fn Py_IncRef(op: *mut PyObject) {
    if op.is_null() {
        return;
    }
    let head = unsafe { &mut *op };
    if head.ob_refcnt >= IMMORTAL_REFCNT {
        return;
    }
    head.ob_refcnt += 1;
}

/// Decrement the C-visible refcount of `op`; on hitting zero the
/// box is freed. No-op on null or immortal singletons.
///
/// # Safety
///
/// Same constraints as [`Py_IncRef`].
#[no_mangle]
pub unsafe extern "C" fn Py_DecRef(op: *mut PyObject) {
    if op.is_null() {
        return;
    }
    let head = unsafe { &mut *op };
    if head.ob_refcnt >= IMMORTAL_REFCNT {
        return;
    }
    head.ob_refcnt -= 1;
    if head.ob_refcnt == 0 {
        unsafe { free_box(op) };
    }
}

/// CPython 3.10+ helper: bump-and-return.
#[no_mangle]
pub unsafe extern "C" fn Py_NewRef(op: *mut PyObject) -> *mut PyObject {
    unsafe { Py_IncRef(op) };
    op
}

/// Same as [`Py_NewRef`] but tolerates null.
#[no_mangle]
pub unsafe extern "C" fn Py_XNewRef(op: *mut PyObject) -> *mut PyObject {
    if !op.is_null() {
        unsafe { Py_IncRef(op) };
    }
    op
}

/// True if `op` points at a [`PyObjectBox`] (rather than a static
/// singleton). Mostly useful for assertions in test code.
pub fn is_heap_object(op: *mut PyObject) -> bool {
    if op.is_null() {
        return false;
    }
    let head = unsafe { &*op };
    head.ob_refcnt < IMMORTAL_REFCNT
}

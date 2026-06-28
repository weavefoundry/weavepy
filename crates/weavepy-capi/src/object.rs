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

/// Refcount value used to mark an object as immortal.
///
/// This mirrors CPython 3.13's `_Py_IMMORTAL_REFCNT` **exactly**: on a
/// 64-bit build it is `UINT_MAX` (`0xFFFF_FFFF`), i.e. all of the *low*
/// 32 bits set. The precise value matters for binary-ABI compatibility
/// (RFC 0043): a stock CPython extension compiled against the real
/// headers carries an *inlined* `Py_INCREF`/`Py_DECREF` that the host
/// cannot intercept, and those inline forms decide immortality by
/// reading the low 32-bit half-word (`_Py_IsImmortal` tests
/// `(int32_t)ob_refcnt < 0`, true for `0xFFFF_FFFF`). With the old
/// `isize::MAX/2 - 1` sentinel the low half-word was `0xFFFF_FFFE`, so a
/// stock inlined refcount op would *not* recognise a WeavePy singleton /
/// static type as immortal and could mutate (and ultimately free) it.
///
/// On 64-bit the high 32 bits are zero, so a `>= IMMORTAL_REFCNT` test
/// still cleanly separates the (immortal) statics from realistic mortal
/// counts, and [`is_immortal_refcnt`] additionally accepts any value
/// whose low-32 sign bit is set (matching `_Py_IsImmortal`).
pub const IMMORTAL_REFCNT: PySsizeT = 0xFFFF_FFFF;

/// CPython-faithful immortality predicate (`_Py_IsImmortal`).
///
/// On 64-bit, an object is immortal iff the low 32 bits, read as a
/// signed `i32`, are negative — i.e. bit 31 is set. This matches the
/// inline check stock extensions compile in, so the function-call and
/// inlined refcount paths agree on the same object.
#[inline]
pub fn is_immortal_refcnt(refcnt: PySsizeT) -> bool {
    ((refcnt as u32) as i32) < 0
}

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
    // Faithful built-in types cross into C as layout-faithful mirrors
    // (RFC 0043) so a stock extension's *inlined* field reads land on
    // real CPython-shaped memory. Everything else keeps the legacy
    // `PyObjectBox` (head + Rust payload) representation.
    if crate::mirror::obj_is_faithful(&obj) {
        return crate::mirror::mirror_out(obj);
    }
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
    // If the *advertised* type is a faithful built-in (e.g. the
    // tuple-staging case where `obj` is an `Object::List` but the type
    // is `PyTuple_Type`), mint a mirror so the public pointer stays
    // byte-faithful and resolves back through the prefix.
    if crate::mirror::type_is_faithful(ty) {
        return crate::mirror::mirror_out_with_type(obj, ty);
    }
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
    let raw = if unsafe { crate::mirror::is_mirror(p) } {
        unsafe { crate::mirror::native_of(p) }
    } else {
        let bx = unsafe { &*(p as *const PyObjectBox) };
        bx.payload.obj.clone()
    };
    // `PyTuple_New` allocates a mutable staging List but advertises
    // `PyTuple_Type` so it round-trips as a tuple. Freeze the list
    // into an immutable tuple on every external clone — this is the
    // moment a C extension hands the staged tuple back to the
    // runtime. `PyTuple_SetItem` reaches the staged list through
    // [`raw_payload`] to bypass this freeze.
    if !head.ob_type.is_null() && std::ptr::eq(head.ob_type, crate::types::PyTuple_Type.as_ptr()) {
        if let weavepy_vm::object::Object::List(rc) = &raw {
            let snapshot = rc.borrow().clone();
            return weavepy_vm::object::Object::new_tuple(snapshot);
        }
    }
    raw
}

/// Read the raw `Object` payload of a box without applying the
/// tuple-staging freeze that [`clone_object`] performs. Internal
/// helper used by `PyTuple_SetItem`.
#[allow(clippy::missing_safety_doc)]
pub unsafe fn raw_payload(p: *mut PyObject) -> Option<Object> {
    if p.is_null() {
        return None;
    }
    let head = unsafe { &*(p as *const PyObject) };
    if std::ptr::eq(head, crate::singletons::_Py_NoneStruct.as_ptr())
        || std::ptr::eq(head, crate::singletons::_Py_TrueStruct.as_ptr())
        || std::ptr::eq(head, crate::singletons::_Py_FalseStruct.as_ptr())
    {
        return None;
    }
    if unsafe { crate::mirror::is_mirror(p) } {
        return Some(unsafe { crate::mirror::native_of(p) });
    }
    let bx = unsafe { &*(p as *const PyObjectBox) };
    Some(bx.payload.obj.clone())
}

/// Overwrite the native object backing `p` (its prefix for a mirror, or
/// its payload for a legacy box). Used by `PyTuple_SetItem` when it must
/// rewrite an already-frozen tuple in place.
///
/// # Safety
/// `p` must be a heap object produced by [`into_owned`] /
/// [`into_owned_with_type`] (not a static singleton/type).
#[allow(clippy::missing_safety_doc)]
pub unsafe fn set_payload(p: *mut PyObject, obj: Object) {
    if unsafe { crate::mirror::is_mirror(p) } {
        let pre = unsafe { crate::mirror::prefix_of(p) };
        unsafe { (*pre).obj = obj };
    } else {
        let bx = unsafe { &mut *(p as *mut PyObjectBox) };
        bx.payload.obj = obj;
    }
}

/// Default `tp_dealloc` for WeavePy's faithful built-in and heap types.
///
/// Stock CPython's *inlined* `Py_DECREF` calls `_Py_Dealloc(op)` when an
/// object's refcount reaches zero, which reads `Py_TYPE(op)->tp_dealloc`
/// and invokes it. Because that path is compiled into the wheel and the
/// host cannot intercept it, every type WeavePy exposes installs this as
/// its `tp_dealloc` (at the CPython-faithful offset 48) so a stock
/// extension dropping the last reference to one of our objects releases
/// the storage correctly instead of jumping through a garbage slot.
///
/// # Safety
/// `op` must be a live heap object (mirror or legacy box) with a zero
/// refcount, exactly as `_Py_Dealloc` guarantees.
#[no_mangle]
pub unsafe extern "C" fn _PyWeavePy_Dealloc(op: *mut PyObject) {
    if op.is_null() {
        return;
    }
    unsafe { free_box(op) };
}

/// `_Py_Dealloc(op)` — CPython's object-deallocation entry point.
///
/// Stock release-build headers compile an *inlined* `Py_DECREF` that, on
/// reaching refcount zero, calls this external symbol; it must therefore
/// exist in the host. Faithfully, it dispatches to `Py_TYPE(op)->tp_dealloc`
/// (which WeavePy points at [`_PyWeavePy_Dealloc`] for every type it
/// exposes), falling back to the direct free path.
///
/// # Safety
/// `op` must be a live heap object whose refcount has reached zero.
#[no_mangle]
pub unsafe extern "C" fn _Py_Dealloc(op: *mut PyObject) {
    if op.is_null() {
        return;
    }
    let ty = unsafe { (*op).ob_type };
    if !ty.is_null() {
        if let Some(dealloc) = unsafe { (*ty).tp_dealloc } {
            unsafe { dealloc(op) };
            return;
        }
    }
    unsafe { free_box(op) };
}

/// Drop a box's storage, running its destructor (if any) first.
///
/// SAFETY: `p` must be a heap-allocated box previously produced by
/// [`into_owned`] / [`into_owned_with_type`] / capsule / module
/// helpers. Static singletons short-circuit through the immortal
/// check in [`Py_DecRef`].
unsafe fn free_box(p: *mut PyObject) {
    // Invalidate any borrowed-item cache entries pinned to this
    // box's address so subsequent reuse of the slab doesn't return
    // stale items from the old container.
    crate::containers::invalidate_borrowed_cache(p);

    // Faithful mirrors are raw-allocated with a negative-offset prefix;
    // free them through the mirror bridge (which runs any destructor,
    // drops the owning native object, and releases the block + any
    // out-of-line buffer).
    if unsafe { crate::mirror::is_mirror(p) } {
        unsafe { crate::mirror::free_mirror(p) };
        return;
    }

    let bx = unsafe { Box::from_raw(p as *mut PyObjectBox) };
    if let Some(d) = bx.payload.destructor {
        let raw = Box::into_raw(bx);
        unsafe { d(raw as *mut PyObject) };
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
    if is_immortal_refcnt(head.ob_refcnt) {
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
    if is_immortal_refcnt(head.ob_refcnt) {
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
    !is_immortal_refcnt(head.ob_refcnt)
}

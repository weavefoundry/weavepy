//! Foreign (cpyext-style) object proxy — RFC 0046, wave 4.
//!
//! WeavePy's binary-ABI layer ([`weavepy-capi`]) mints layout-faithful
//! mirrors and `PyObjectBox`es for values that *originate in the VM*.
//! A real C extension such as numpy, however, also creates objects of
//! its **own** — a builtin `numpy.zeros` function, a static
//! `PyArray_Descr`, an `ndarray` instance, the `numpy._core` type
//! objects — by allocating them itself (often as static C storage or
//! via `PyObject_Malloc` + `PyObject_Init`, bypassing WeavePy's
//! allocator entirely). The VM cannot interpret those bytes: they are
//! not a `PyObjectBox`, not a mirror, not a capsule.
//!
//! Following PyPy's `cpyext`, such a pointer crosses into the VM as a
//! **foreign proxy**: an opaque, identity-stable handle ([`Object::Foreign`])
//! that holds the raw `*mut PyObject` and routes every operation
//! (`repr`, call, attribute access, the number protocol, …) back
//! through the binary-ABI layer via the function-pointer table
//! installed here at interpreter start ([`install`]). The VM never
//! dereferences the pointer; the cpyext layer owns its lifetime.
//!
//! The hook table is empty in a pure-VM build (no extension can run, so
//! no foreign object is ever created), so this module is inert unless
//! `weavepy-capi` has installed its bridge.

use std::sync::OnceLock;

use weavepy_compiler::{BinOpKind, CompareKind};

use crate::error::RuntimeError;
use crate::object::Object;
use crate::sync::Rc;

/// VM-side soul of a foreign `PyObject` (see [`Object::Foreign`]).
///
/// `ptr` is stored as a `usize` (not a pointer) so [`Object`] stays
/// `Send + Sync` — exactly like [`crate::object::PyCapsuleSoul`]. The
/// VM never dereferences it; it is only ever handed back to the cpyext
/// layer through the [`ForeignHooks`].
pub struct PyForeignSoul {
    /// The raw `*mut PyObject`, as an integer.
    pub ptr: usize,
    /// Cached `Py_TYPE(ptr)->tp_name`, so `type(x).__name__`, `repr`
    /// fallbacks and error messages need no C round-trip.
    pub type_name: Rc<str>,
}

impl std::fmt::Debug for PyForeignSoul {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "<foreign {} at 0x{:x}>", self.type_name, self.ptr)
    }
}

impl Drop for PyForeignSoul {
    fn drop(&mut self) {
        if let Some(h) = HOOKS.get() {
            (h.decref)(self.ptr);
        }
    }
}

/// Bridge installed by `weavepy-capi` at interpreter start. Every entry
/// receives/returns plain VM types; the cpyext side performs the
/// `Object <-> *mut PyObject` marshalling and turns a pending C
/// exception into a [`RuntimeError`].
#[derive(Debug)]
pub struct ForeignHooks {
    /// `Py_INCREF(ptr)` — pin a fresh reference (used when a foreign
    /// pointer is wrapped into a new soul).
    pub incref: fn(usize),
    /// `Py_DECREF(ptr)` — release the reference a soul held.
    pub decref: fn(usize),
    /// `PyObject_Repr(ptr)`.
    pub repr: fn(usize) -> Result<String, RuntimeError>,
    /// `PyObject_Str(ptr)`.
    pub str: fn(usize) -> Result<String, RuntimeError>,
    /// `PyObject_Hash(ptr)`.
    pub hash: fn(usize) -> Result<i64, RuntimeError>,
    /// `PyObject_IsTrue(ptr)`.
    pub is_true: fn(usize) -> Result<bool, RuntimeError>,
    /// `PyObject_Call(ptr, args, kwargs)`.
    pub call: fn(usize, &[Object], &[(String, Object)]) -> Result<Object, RuntimeError>,
    /// `PyObject_GetAttrString(ptr, name)`.
    pub getattr: fn(usize, &str) -> Result<Object, RuntimeError>,
    /// `PyObject_SetAttrString(ptr, name, value)` (value `None` ⇒ delete).
    pub setattr: fn(usize, &str, Option<&Object>) -> Result<(), RuntimeError>,
    /// `PyObject_GetItem(ptr, key)`.
    pub getitem: fn(usize, &Object) -> Result<Object, RuntimeError>,
    /// `PyObject_SetItem` / `PyObject_DelItem` (value `None` ⇒ delete).
    pub setitem: fn(usize, &Object, Option<&Object>) -> Result<(), RuntimeError>,
    /// `PyObject_Length(ptr)`.
    pub length: fn(usize) -> Result<isize, RuntimeError>,
    /// `PyObject_GetIter(ptr)`.
    pub iter: fn(usize) -> Result<Object, RuntimeError>,
    /// `PyIter_Next(ptr)` — `Ok(None)` at exhaustion.
    pub iternext: fn(usize) -> Result<Option<Object>, RuntimeError>,
    /// `PyNumber_*`/sequence binary op. Either operand may be foreign;
    /// returns the VM `NotImplemented` singleton when C declines so the
    /// VM's dispatcher can keep looking.
    pub binop: fn(BinOpKind, &Object, &Object) -> Result<Object, RuntimeError>,
    /// `PyObject_RichCompare`. Returns `NotImplemented` when C declines.
    pub compare: fn(CompareKind, &Object, &Object) -> Result<Object, RuntimeError>,
    /// Resolve `type(ptr)` to a VM object (an [`Object::Type`] when the
    /// type is bridged; falls back to a foreign proxy of the type).
    pub get_type: fn(usize) -> Object,
}

static HOOKS: OnceLock<ForeignHooks> = OnceLock::new();

/// Install the cpyext bridge. Idempotent; a second call is ignored.
pub fn install(hooks: ForeignHooks) {
    let _ = HOOKS.set(hooks);
}

/// True once the binary-ABI layer has installed its bridge.
pub fn is_installed() -> bool {
    HOOKS.get().is_some()
}

fn hooks() -> Result<&'static ForeignHooks, RuntimeError> {
    HOOKS
        .get()
        .ok_or_else(|| RuntimeError::Internal("foreign-object bridge not installed".to_owned()))
}

/// Construct a foreign proxy soul for `ptr`, pinning one reference.
/// `type_name` is the foreign type's `tp_name`. Returns the raw soul;
/// the caller wraps it in [`Object::Foreign`].
pub fn wrap(ptr: usize, type_name: Rc<str>) -> Rc<PyForeignSoul> {
    if let Some(h) = HOOKS.get() {
        (h.incref)(ptr);
    }
    Rc::new(PyForeignSoul { ptr, type_name })
}

// --- VM-facing operations (thin wrappers that surface a clean error
//     when the bridge is absent). ---

pub fn repr(s: &PyForeignSoul) -> Result<String, RuntimeError> {
    match hooks() {
        Ok(h) => (h.repr)(s.ptr),
        Err(_) => Ok(format!("<{} object at 0x{:x}>", s.type_name, s.ptr)),
    }
}

pub fn str_(s: &PyForeignSoul) -> Result<String, RuntimeError> {
    match hooks() {
        Ok(h) => (h.str)(s.ptr),
        Err(_) => repr(s),
    }
}

pub fn hash(s: &PyForeignSoul) -> Result<i64, RuntimeError> {
    (hooks()?.hash)(s.ptr)
}

pub fn is_true(s: &PyForeignSoul) -> bool {
    match hooks() {
        Ok(h) => (h.is_true)(s.ptr).unwrap_or(true),
        Err(_) => true,
    }
}

pub fn call(
    s: &PyForeignSoul,
    args: &[Object],
    kwargs: &[(String, Object)],
) -> Result<Object, RuntimeError> {
    (hooks()?.call)(s.ptr, args, kwargs)
}

pub fn getattr(s: &PyForeignSoul, name: &str) -> Result<Object, RuntimeError> {
    (hooks()?.getattr)(s.ptr, name)
}

pub fn setattr(s: &PyForeignSoul, name: &str, value: Option<&Object>) -> Result<(), RuntimeError> {
    (hooks()?.setattr)(s.ptr, name, value)
}

pub fn getitem(s: &PyForeignSoul, key: &Object) -> Result<Object, RuntimeError> {
    (hooks()?.getitem)(s.ptr, key)
}

pub fn setitem(
    s: &PyForeignSoul,
    key: &Object,
    value: Option<&Object>,
) -> Result<(), RuntimeError> {
    (hooks()?.setitem)(s.ptr, key, value)
}

pub fn length(s: &PyForeignSoul) -> Result<isize, RuntimeError> {
    (hooks()?.length)(s.ptr)
}

pub fn iter(s: &PyForeignSoul) -> Result<Object, RuntimeError> {
    (hooks()?.iter)(s.ptr)
}

pub fn iternext(s: &PyForeignSoul) -> Result<Option<Object>, RuntimeError> {
    (hooks()?.iternext)(s.ptr)
}

pub fn binop(op: BinOpKind, a: &Object, b: &Object) -> Result<Object, RuntimeError> {
    (hooks()?.binop)(op, a, b)
}

pub fn compare(op: CompareKind, a: &Object, b: &Object) -> Result<Object, RuntimeError> {
    (hooks()?.compare)(op, a, b)
}

pub fn get_type(s: &PyForeignSoul) -> Object {
    match hooks() {
        Ok(h) => (h.get_type)(s.ptr),
        Err(_) => Object::None,
    }
}

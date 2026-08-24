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
    /// The *bare* type name (the tail of `tp_name` after the last `.`), i.e.
    /// what Python's `type(x).__name__` reports (`float64`, `Nano`). Cached so
    /// `repr` fallbacks and `__name__` need no C round-trip.
    pub type_name: Rc<str>,
    /// The full, unmodified `Py_TYPE(ptr)->tp_name` (`numpy.float64`,
    /// `pandas._libs.tslibs.offsets.Nano`, but bare `Timestamp` when the C
    /// type itself sets no module prefix). This is exactly the string CPython
    /// interpolates into `tp_name`-based `TypeError` messages
    /// (`unsupported operand type(s) for /: 'float' and 'X'`, `'X' object is
    /// not iterable`, …), so error text must use *this*, never `type_name`.
    pub tp_name: Rc<str>,
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
    /// `PySequence_Check(ptr)` — true iff the foreign type exposes
    /// `tp_as_sequence->sq_item`. Used to replicate CPython's
    /// `PyObject_GetIter` legacy-sequence fallback: an object with no
    /// `tp_iter` but a sequence `__getitem__` (numpy's `_array_converter`,
    /// reached by `np.unique`/`np.quantile`) is still iterable via
    /// `PySeqIter`. Without this the VM reported such an object as
    /// "not iterable" and unpacking (`ar_, = conv`) raised.
    pub sequence_check: fn(usize) -> bool,
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
    /// `PyNumber_Float(ptr)` — drive the foreign type's `nb_float`
    /// (then `nb_index`) conversion. Returns an [`Object::Float`]. Lets
    /// `float(np.float64(x))` and friends round-trip a numpy scalar
    /// without WeavePy having to interpret its bytes.
    pub as_float: fn(usize) -> Result<Object, RuntimeError>,
    /// `PyNumber_Long(ptr)` — drive `nb_int` (then `nb_index`). Returns an
    /// `Object::Int`/`Long`/`Bool` (`int(np.uint32(x))`).
    pub as_int: fn(usize) -> Result<Object, RuntimeError>,
    /// `PyNumber_Index(ptr)` — drive `nb_index` (loss-less integer view).
    /// Returns an `Object::Int`/`Long`/`Bool`; errors when the type has no
    /// `nb_index` (e.g. a float scalar used as an index).
    pub as_index: fn(usize) -> Result<Object, RuntimeError>,
    /// `memoryview(ptr)` — acquire the foreign object's buffer
    /// (`PyObject_GetBuffer` with `PyBUF_FULL_RO`) and wrap it in a VM
    /// [`Object::MemoryView`] that faithfully carries the exporter's
    /// `format`/`itemsize`/`shape`/`strides` (e.g. numpy's `'O'`/8 object
    /// arrays). Errors when the type does not export the buffer protocol.
    pub get_buffer: fn(usize) -> Result<Object, RuntimeError>,
    /// `memoryview(obj)` for an arbitrary VM object that crosses into C with
    /// its own buffer protocol — a numpy `ndarray` crosses as a faithful
    /// [`Object::Instance`] (wearing its real C type), not an
    /// [`Object::Foreign`], so it has no raw soul pointer. The bridge marshals
    /// the object to a `*mut PyObject` ([`crate::object::into_owned`]) and
    /// drives `PyMemoryView_FromObject` on it. Errors (with the C-side
    /// `TypeError`) when the type does not export the buffer protocol.
    pub get_buffer_obj: fn(&Object) -> Result<Object, RuntimeError>,
    /// Convert an *owned* `PyObject*` — a `new` reference returned by a C
    /// function — into a VM object, consuming the reference. NULL raises
    /// the pending C exception. Backs ctypes' `py_object` restype
    /// (`pythonapi.PyBytes_FromFormat(...)` in `test_bytes.test_from_format`).
    pub steal_object: fn(usize) -> Result<Object, RuntimeError>,
    /// Marshal a VM object to a *new owned* `PyObject*` reference — the
    /// ctypes `py_object` argument direction. Pair every call with a
    /// [`Self::release_object_ptr`] once the C call has returned.
    pub object_to_owned_ptr: fn(&Object) -> usize,
    /// Release an owned `PyObject*` minted by [`Self::object_to_owned_ptr`]
    /// (a plain `Py_DECREF`).
    pub release_object_ptr: fn(usize),
    /// `Py_TYPE(descr)->tp_descr_get(descr, obj, owner)` — bind a foreign
    /// descriptor found in a class MRO (RFC 0066 WS3). `instance` is the
    /// VM `None` for class access (crossing as C `NULL`, per CPython's
    /// `type_getattro`). Returns `Ok(None)` when the foreign type carries
    /// no `tp_descr_get`, in which case the caller uses the descriptor
    /// value unchanged — CPython's plain class attribute. pybind11 wraps
    /// every registered method in a C `instancemethod` whose `tp_descr_get`
    /// yields the bound method; without this hook such attributes crossed
    /// unbound and calls lost `self`.
    pub descr_get: fn(usize, &Object, &Object) -> Result<Option<Object>, RuntimeError>,
    /// The C-API layer's *handled* exception (`tstate->exc_info->exc_value`,
    /// the slot `PyErr_SetHandledException` / Cython's `__Pyx_GetException`
    /// maintain), or `None` when clear. `sys.exc_info()` consults this when
    /// the VM's own handled-exception stack is empty: Python code invoked
    /// from inside a compiled `except` block (gevent's Cython
    /// `_notify_link_list` does `hub.handle_error((link, self),
    /// *sys.exc_info())`) has no VM-side handler frame, so without this
    /// bridge the caught exception is invisible and gevent reports
    /// `issubclass() arg 1 must be a class` from `handle_error(None, None,
    /// None)` (RFC 0072 WS2).
    pub handled_exception: fn() -> Option<Object>,
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
/// `type_name` is the *bare* tail; `tp_name` is the full C `tp_name`.
/// Returns the raw soul; the caller wraps it in [`Object::Foreign`].
pub fn wrap(ptr: usize, type_name: Rc<str>, tp_name: Rc<str>) -> Rc<PyForeignSoul> {
    if let Some(h) = HOOKS.get() {
        (h.incref)(ptr);
    }
    Rc::new(PyForeignSoul {
        ptr,
        type_name,
        tp_name,
    })
}

// --- VM-facing operations (thin wrappers that surface a clean error
//     when the bridge is absent). ---

/// Consume an owned `PyObject*` returned by a C function and produce the
/// VM object it denotes (ctypes `py_object` restype). NULL surfaces the
/// pending C exception.
pub fn steal_object(ptr: usize) -> Result<Object, RuntimeError> {
    (hooks()?.steal_object)(ptr)
}

/// Mint a new owned `PyObject*` for `obj` (ctypes `py_object` argument).
/// Release it with [`release_object_ptr`] after the call returns.
pub fn object_to_owned_ptr(obj: &Object) -> Result<usize, RuntimeError> {
    Ok((hooks()?.object_to_owned_ptr)(obj))
}

/// Release an owned `PyObject*` minted by [`object_to_owned_ptr`].
pub fn release_object_ptr(ptr: usize) {
    if let Some(h) = HOOKS.get() {
        (h.release_object_ptr)(ptr);
    }
}

/// The C-API layer's handled exception, or `None` when the slot is clear
/// (or the bridge is not installed). See [`ForeignHooks::handled_exception`].
pub fn capi_handled_exception() -> Option<Object> {
    HOOKS.get().and_then(|h| (h.handled_exception)())
}

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

/// `PyObject_IsTrue(ptr)` that *propagates* a pending C exception as a
/// `RuntimeError` rather than swallowing it to `true`. CPython truth-tests
/// with `PyObject_IsTrue`, and a *multi-element* numpy array's `nb_bool`
/// raises `ValueError` ("The truth value of an array with more than one
/// element is ambiguous"). Every boolean context that can surface an error
/// — `PyObject_RichCompareBool` (membership / `list.index` / equality
/// containment), `any`/`all`/`filter` — must see that raise; the infallible
/// [`is_true`] above is only for the short-circuit sites (`if`, `and`/`or`)
/// whose ambient error check catches the pending exception separately.
pub fn is_true_checked(s: &PyForeignSoul) -> Result<bool, RuntimeError> {
    (hooks()?.is_true)(s.ptr)
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

/// True iff the foreign object's C type is a sequence (`sq_item` set).
/// Falls back to `false` when the bridge is absent (pure-VM build has no
/// foreign objects). Mirrors `PySequence_Check` for the legacy-iterator
/// fallback in [`crate::Interpreter::make_iter`].
pub fn sequence_check(s: &PyForeignSoul) -> bool {
    match hooks() {
        Ok(h) => (h.sequence_check)(s.ptr),
        Err(_) => false,
    }
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

pub fn as_float(s: &PyForeignSoul) -> Result<Object, RuntimeError> {
    (hooks()?.as_float)(s.ptr)
}

pub fn as_int(s: &PyForeignSoul) -> Result<Object, RuntimeError> {
    (hooks()?.as_int)(s.ptr)
}

pub fn as_index(s: &PyForeignSoul) -> Result<Object, RuntimeError> {
    (hooks()?.as_index)(s.ptr)
}

pub fn get_buffer(s: &PyForeignSoul) -> Result<Object, RuntimeError> {
    (hooks()?.get_buffer)(s.ptr)
}

pub fn get_buffer_obj(obj: &Object) -> Result<Object, RuntimeError> {
    (hooks()?.get_buffer_obj)(obj)
}

/// Bind a foreign descriptor through its C type's `tp_descr_get`
/// (RFC 0066 WS3). `Ok(None)` ⇒ the foreign type is not a descriptor
/// (no slot) and the caller should use the value unchanged. Absent
/// bridge (pure-VM build) ⇒ `Ok(None)` for the same pass-through.
pub fn descr_get(
    s: &PyForeignSoul,
    instance: &Object,
    owner: &Object,
) -> Result<Option<Object>, RuntimeError> {
    match hooks() {
        Ok(h) => (h.descr_get)(s.ptr, instance, owner),
        Err(_) => Ok(None),
    }
}

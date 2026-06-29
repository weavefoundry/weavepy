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

// ---------------------------------------------------------------------------
// WeavePy-minted pointer registry (RFC 0046, wave 4).
//
// A real C extension (numpy) allocates many objects of its *own* — static
// `PyArray_Descr`s, builtin function objects, type objects — by paths that
// never touch WeavePy's allocator. Such a "foreign" `*mut PyObject` is not a
// `PyObjectBox`, a mirror, an instance body, or a capsule; interpreting its
// bytes as any of those corrupts memory ([`clone_object`] reading a bogus
// payload; [`free_box`] `Box::from_raw`-ing foreign storage).
//
// To tell ours from foreign *soundly* (no speculative reads at guessed
// offsets) we record every public pointer WeavePy hands to C in this set and
// remove it when the storage is released. A pointer that is **not** present
// (and is neither a static singleton nor a type object) is foreign, and is
// proxied into the VM as [`weavepy_vm::object::Object::Foreign`].
// ---------------------------------------------------------------------------

use std::collections::HashSet;
use std::sync::Mutex;

static MINTED: Mutex<Option<HashSet<usize>>> = Mutex::new(None);

/// Record `p` as a WeavePy-minted public pointer. Called by every mint
/// site (box, mirror body, instance body, capsule) so [`is_weavepy_owned`]
/// can later distinguish it from a foreign extension object.
pub fn register_minted(p: *mut PyObject) {
    if p.is_null() {
        return;
    }
    if let Ok(mut g) = MINTED.lock() {
        g.get_or_insert_with(HashSet::new).insert(p as usize);
    }
}

/// Drop `p` from the minted set when its storage is released.
pub fn unregister_minted(p: *mut PyObject) {
    if p.is_null() {
        return;
    }
    if let Ok(mut g) = MINTED.lock() {
        if let Some(set) = g.as_mut() {
            set.remove(&(p as usize));
        }
    }
}

/// True iff `p` is a live pointer WeavePy itself minted (box / mirror /
/// instance body / capsule). A non-owned, non-singleton, non-type
/// pointer is a *foreign* extension object.
pub fn is_weavepy_owned(p: *mut PyObject) -> bool {
    if p.is_null() {
        return false;
    }
    MINTED
        .lock()
        .ok()
        .and_then(|g| g.as_ref().map(|s| s.contains(&(p as usize))))
        .unwrap_or(false)
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
    // RFC 0046 (wave 4): `None` crosses into C as the canonical
    // `&_Py_NoneStruct` singleton, never a fresh box. Stock extensions
    // test for it by pointer identity — the header's `Py_None` macro is
    // `(&_Py_NoneStruct)` and code writes `if (x == Py_None)` (numpy's
    // `_ArrayFunctionDispatcher.__new__` does exactly this on its first
    // argument). A minted box would compare unequal and silently take the
    // wrong branch. The singleton is immortal, so it needs no refcount
    // bump and is never freed.
    if matches!(obj, Object::None) {
        return crate::singletons::none_ptr();
    }
    // RFC 0046 (wave 4): `Ellipsis` and `NotImplemented` are likewise
    // pointer-identity singletons on the C side. numpy's index parser
    // (`prepare_index` in `mapping.c`) recognises the ellipsis with a bare
    // `op == Py_Ellipsis` test; a freshly-minted box would compare unequal,
    // so `arr[-1, ...] = x` (numpy's own `linspace`) would raise "only
    // integers, slices (`:`), ellipsis (`...`) … are valid indices". Hand C
    // the static singletons (immortal, never freed) instead.
    if weavepy_vm::vm_singletons::is_ellipsis(&obj) {
        return crate::singletons::ellipsis_ptr();
    }
    if weavepy_vm::vm_singletons::is_not_implemented(&obj) {
        return crate::singletons::not_implemented_ptr();
    }
    // RFC 0046 (wave 4): a foreign proxy round-trips back to the *same*
    // `PyObject*` the extension first gave us (identity is load-bearing —
    // numpy compares descrs/types by pointer). Hand C a fresh reference.
    if let Object::Foreign(s) = &obj {
        let p = s.ptr as *mut PyObject;
        if p.is_null() && std::env::var_os("WEAVEPY_DEBUG_TUPLE").is_some() {
            eprintln!("[into_owned] FOREIGN with NULL ptr!");
        }
        unsafe { Py_IncRef(p) };
        return p;
    }
    // RFC 0046 (wave 4): a type object's canonical `PyObject*` is the
    // `PyTypeObject` itself — numpy compares DType classes by pointer and
    // validates them with `Py_IS_TYPE(cls, &PyArrayDTypeMeta_Type)` (a
    // direct `cls->ob_type` read). Boxing an `Object::Type` would instead
    // mint an *instance* whose `ob_type` is the class, so resolve it to the
    // registered `PyTypeObject*` (static, heap, or readied) and hand C a
    // fresh reference to that.
    if let Object::Type(t) = &obj {
        if let Some(p) = crate::types::type_ptr_for_class(t) {
            let p = p as *mut PyObject;
            unsafe { Py_IncRef(p) };
            return p;
        }
    }
    // Faithful built-in types cross into C as layout-faithful mirrors
    // (RFC 0043) so a stock extension's *inlined* field reads land on
    // real CPython-shaped memory. Everything else keeps the legacy
    // `PyObjectBox` (head + Rust payload) representation.
    if crate::mirror::obj_is_faithful(&obj) {
        return crate::mirror::mirror_out(obj);
    }
    // RFC 0045 (wave 3): a capsule round-trips as its original retained box
    // (the same pointer C first saw), not a fresh per-crossing box.
    if let Object::Capsule(rc) = &obj {
        return crate::capsule::capsule_box_from_soul(rc);
    }
    let ty = crate::types::type_for_object(&obj);
    // RFC 0045 (wave 3): an instance of an inline-storage extension type
    // crosses into C as its single, stable faithful body (so `self->field`
    // reads the same bytes on every crossing), not a fresh per-crossing
    // box. Every other object keeps the legacy `PyObjectBox`.
    if let Object::Instance(inst) = &obj {
        if crate::types::is_inline_instance_type(ty) {
            return crate::instance::instance_body_out(inst, ty);
        }
        // RFC 0046 (wave 4): a *non-inline* instance crosses as a single,
        // stable identity box cached in `c_body`. Stock extensions cache an
        // object by pointer and test it with `==`: numpy stashes
        // `npy_static_pydata._NoValue` at import and a ufunc reduction
        // detects "no initial value given" with `initial == _NoValue`. A
        // fresh per-crossing box would compare unequal, so numpy would treat
        // the `_NoValue` *sentinel* as a real initial value and try to coerce
        // it to the output dtype (`float(_NoValue)` → "a float is required").
        // Returning the same pointer every time makes the identity test hold.
        // The box still owns the instance strongly and is freed by C's
        // refcount exactly like the legacy box — `free_box` clears the cache.
        if let Some(p) = cached_instance_box(inst) {
            return p;
        }
        return mint_instance_box(inst, ty);
    }
    let boxed = Box::new(PyObjectBox {
        head: PyObject {
            ob_refcnt: 1,
            ob_type: ty,
        },
        payload: PayloadCell::from_object(obj),
    });
    let raw = Box::into_raw(boxed) as *mut PyObject;
    register_minted(raw);
    raw
}

/// Return the cached identity box for a non-inline `inst` if one already
/// exists, with a fresh C reference (RFC 0046, wave 4). The box outlives
/// any single C reference because it owns the instance strongly; it is
/// reclaimed only when C's refcount reaches zero (see [`free_box`]).
fn cached_instance_box(
    inst: &weavepy_vm::sync::Rc<weavepy_vm::types::PyInstance>,
) -> Option<*mut PyObject> {
    let cached = inst.c_body.get();
    if cached == 0 {
        return None;
    }
    let p = cached as *mut PyObject;
    unsafe { Py_IncRef(p) };
    Some(p)
}

/// Mint the single identity box for a non-inline `inst`, record it in
/// `inst.c_body`, and return it with one C reference (RFC 0046, wave 4).
/// The payload holds a strong clone of the instance, so the box pins the
/// instance for as long as C holds a reference.
fn mint_instance_box(
    inst: &weavepy_vm::sync::Rc<weavepy_vm::types::PyInstance>,
    ty: *mut PyTypeObject,
) -> *mut PyObject {
    let boxed = Box::new(PyObjectBox {
        head: PyObject {
            ob_refcnt: 1,
            ob_type: ty,
        },
        payload: PayloadCell::from_object(Object::Instance(inst.clone())),
    });
    let raw = Box::into_raw(boxed) as *mut PyObject;
    register_minted(raw);
    inst.c_body.set(raw as usize);
    raw
}

/// Like [`into_owned_with_type`] but for a *non-inline* instance, never
/// consults or populates the identity cache (`c_body`): it always mints a
/// **fresh** box.
///
/// RFC 0046 (wave 4): the cycle collector's `tp_traverse` / `tp_clear`
/// bridge must borrow an instance into C *without* perturbing the
/// refcount of the cached identity box a C-held cycle edge points at. A
/// stock GC type breaks a cycle by `Py_CLEAR`-ing the child it owns; that
/// stock, inlined `Py_DECREF` drives the child box to zero and runs the
/// extension's `tp_dealloc` (e.g. `Node_dealloc`, which decrements a live
/// counter and frees the node) via [`_Py_Dealloc`]. If the bridge handed
/// `tp_clear` the *cached* box (with the usual `+1`), that extra reference
/// would stop the cascade from reaching zero, so the node would instead be
/// reclaimed later through [`free_box`] — which is `tp_free`, not
/// `tp_dealloc`, and therefore skips the extension's cleanup, leaking the
/// node and desyncing its counter. A fresh, uncached box keeps the cached
/// edge at exactly the refcount the extension expects.
pub fn into_owned_with_type_uncached(obj: Object, ty: *mut PyTypeObject) -> *mut PyObject {
    if let Object::Instance(inst) = &obj {
        if !crate::types::is_inline_instance_type(ty) && !crate::mirror::type_is_faithful(ty) {
            let boxed = Box::new(PyObjectBox {
                head: PyObject {
                    ob_refcnt: 1,
                    ob_type: ty,
                },
                payload: PayloadCell::from_object(Object::Instance(inst.clone())),
            });
            let raw = Box::into_raw(boxed) as *mut PyObject;
            register_minted(raw);
            return raw;
        }
    }
    into_owned_with_type(obj, ty)
}

/// Build a box that wraps `obj` and is associated with the given
/// type pointer (used when [`type_for_object`](crate::types::type_for_object)
/// alone isn't precise enough — e.g. when constructing an instance
/// of a heap-allocated user type from `PyType_FromSpec`).
pub fn into_owned_with_type(obj: Object, ty: *mut PyTypeObject) -> *mut PyObject {
    // RFC 0046 (wave 4): a foreign proxy ignores the advertised type and
    // round-trips to its original pointer (see [`into_owned`]).
    if let Object::Foreign(s) = &obj {
        let p = s.ptr as *mut PyObject;
        unsafe { Py_IncRef(p) };
        return p;
    }
    // RFC 0046 (wave 4): a type object round-trips to its own
    // `PyTypeObject*` (see [`into_owned`]); the advertised `ty` (the
    // metaclass) is irrelevant to a class's canonical pointer.
    if let Object::Type(t) = &obj {
        if let Some(p) = crate::types::type_ptr_for_class(t) {
            let p = p as *mut PyObject;
            unsafe { Py_IncRef(p) };
            return p;
        }
    }
    // If the *advertised* type is a faithful built-in (e.g. the
    // tuple-staging case where `obj` is an `Object::List` but the type
    // is `PyTuple_Type`), mint a mirror so the public pointer stays
    // byte-faithful and resolves back through the prefix.
    if crate::mirror::type_is_faithful(ty) {
        return crate::mirror::mirror_out_with_type(obj, ty);
    }
    // RFC 0045 (wave 3): a capsule round-trips as its original retained box
    // regardless of the advertised type (see [`into_owned`]).
    if let Object::Capsule(rc) = &obj {
        return crate::capsule::capsule_box_from_soul(rc);
    }
    // RFC 0045 (wave 3): inline-storage extension instances cross as their
    // stable faithful body (see [`into_owned`]).
    if let Object::Instance(inst) = &obj {
        if crate::types::is_inline_instance_type(ty) {
            return crate::instance::instance_body_out(inst, ty);
        }
        // RFC 0046 (wave 4): non-inline instances cross as their single,
        // stable identity box (see [`into_owned`]).
        if let Some(p) = cached_instance_box(inst) {
            return p;
        }
        return mint_instance_box(inst, ty);
    }
    let boxed = Box::new(PyObjectBox {
        head: PyObject {
            ob_refcnt: 1,
            ob_type: ty,
        },
        payload: PayloadCell::from_object(obj),
    });
    let raw = Box::into_raw(boxed) as *mut PyObject;
    register_minted(raw);
    raw
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
        return weavepy_vm::vm_singletons::not_implemented();
    }
    if std::ptr::eq(head, crate::singletons::_Py_EllipsisObject.as_ptr()) {
        return weavepy_vm::vm_singletons::ellipsis();
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
    // RFC 0046 (wave 4): an extension type whose metaclass is *not* `type`
    // (numpy's DType classes carry `ob_type == &PyArrayDTypeMeta_Type`)
    // still resolves to its bridged `Object::Type` if we readied it — a
    // safe pointer-keyed lookup that must precede the foreign fallback so
    // such a class is not opaquely proxied.
    if let Some(t) = crate::types::readied_bridge(p as *mut crate::types::PyTypeObject) {
        return Object::Type(t);
    }
    // RFC 0046 (wave 4): a pointer WeavePy did not mint — a static numpy
    // `PyArray_Descr`, an extension-built function object, an un-bridged
    // type — is *foreign*. It is none of the shapes below, so interpreting
    // it as one corrupts memory. Decided BEFORE the capsule/mirror checks
    // because `is_mirror` is type-based and would mis-claim a foreign object
    // whose (readied) type was registered for inline storage. Proxy it
    // opaquely; it round-trips back to the same pointer via `into_owned`.
    if !crate::object::is_weavepy_owned(p) {
        return unsafe { crate::foreign::wrap_foreign(p) };
    }
    // RFC 0045 (wave 3): a capsule carries its state in `user_data`, not in
    // `payload.obj` (which is `None`) — without this it would collapse to
    // `None` on crossing into the VM and break `import_array()`. Resolve it
    // to its identity-stable soul, which round-trips back to the same box.
    if unsafe { crate::capsule::is_capsule(p) } {
        return unsafe { crate::capsule::capsule_soul(p) };
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
    // RFC 0046 (wave 4): a faithful *instance body*'s lifetime is owned by
    // its native `PyInstance`, not by C's refcount (RFC 0045). A stock
    // extension compiles CPython's *inlined* `Py_DECREF`, which on
    // reaching zero calls this symbol **directly** — bypassing
    // [`Py_DecRef`]/[`free_box`]. Running the type's `tp_dealloc` here
    // (e.g. numpy's `array_dealloc`) would free the live object's payload
    // — its `data`/`dimensions`/`descr` — out from under the VM instance
    // that still owns it: the block is absorbed by [`crate::memory::
    // PyObject_Free`] and survives, but every field is gone, so the next
    // VM access reads a half-destroyed array (a NULL `descr` crashed
    // numpy's `convert_ufunc_arguments`). This is the exact refcount cycle
    // a temporary view drives: `v[:, ::-1]` incref's its base `v`, and the
    // view's collection decref's `v` back through zero. Route through
    // `free_box`, which ends *C's* borrow (drops the strong pin) and keeps
    // the body intact; the real `tp_dealloc` runs only when the owning
    // instance is collected (the `free_instance_body` hook).
    //
    // The `is_weavepy_owned` guard is load-bearing: `is_instance_body` is
    // type-keyed and reads a `MirrorPrefix` at a *negative* offset, so on a
    // foreign numpy pointer it would interpret numpy's bytes as our prefix.
    // A foreign object is never one of our bodies; let its own `tp_dealloc`
    // (below) run.
    if unsafe { is_weavepy_owned(op) && crate::mirror::is_instance_body(op) } {
        unsafe { free_box(op) };
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
pub(crate) unsafe fn free_box(p: *mut PyObject) {
    // Invalidate any borrowed-item cache entries pinned to this
    // box's address so subsequent reuse of the slab doesn't return
    // stale items from the old container.
    crate::containers::invalidate_borrowed_cache(p);

    // RFC 0046 (wave 4): a *foreign* object (extension-minted, never in our
    // registry) must never be `Box::from_raw`-d or `free_mirror`-d as one of
    // our objects. This check MUST precede `is_instance_body`/`is_mirror`:
    // those are *type-keyed* (a deref-free discriminator), so a foreign numpy
    // object whose type WeavePy readied for inline storage (or a faithful
    // built-in type) is mis-claimed as a mirror — and `free_mirror` then
    // `dealloc`s a pointer numpy allocated, aborting the process
    // (`POINTER_BEING_FREED_WAS_NOT_ALLOCATED`, seen dropping `numpy.eye`'s
    // flatiter temporaries). `clone_object` decides foreign-ness first for
    // exactly this reason. When a foreign proxy's last VM reference drops,
    // dispatch to the extension's own `tp_dealloc` (numpy frees its array
    // data, etc.); with no `tp_dealloc` we leak rather than corrupt.
    if !is_weavepy_owned(p) {
        let ty = unsafe { (*p).ob_type };
        if !ty.is_null() {
            if let Some(dealloc) = unsafe { (*ty).tp_dealloc } {
                unsafe { dealloc(p) };
            }
        }
        return;
    }

    // RFC 0045 (wave 3): a faithful *instance body*'s lifetime is owned by
    // its native `PyInstance`, not by C's refcount. Reaching zero here
    // only ends *C's* borrow (drops the strong pin); the block is freed
    // when the instance is collected (via the free hook). Checked before
    // `free_mirror`, since an instance body is also a mirror.
    if unsafe { crate::mirror::is_instance_body(p) } {
        unsafe { crate::instance::release_c_ownership(p) };
        return;
    }

    // Faithful mirrors are raw-allocated with a negative-offset prefix;
    // free them through the mirror bridge (which runs any destructor,
    // drops the owning native object, and releases the block + any
    // out-of-line buffer).
    if unsafe { crate::mirror::is_mirror(p) } {
        unsafe { crate::mirror::free_mirror(p) };
        return;
    }

    unregister_minted(p);

    let bx = unsafe { Box::from_raw(p as *mut PyObjectBox) };
    // RFC 0046 (wave 4): if this is an instance's cached identity box, drop
    // the `c_body` cache so a subsequent crossing re-mints a fresh box
    // rather than handing C this about-to-be-freed pointer (use-after-free).
    if let Object::Instance(inst) = &bx.payload.obj {
        if inst.c_body.get() == p as usize {
            inst.c_body.set(0);
        }
    }
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

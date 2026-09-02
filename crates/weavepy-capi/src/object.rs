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
    /// C-visible exception tail (RFC 0072 WS3): the fields of CPython's
    /// `PyBaseExceptionObject` (+ `PyStopIterationObject.value`) at their
    /// exact ABI offsets. Compiled Cython reads them *directly* — its
    /// generator/coroutine return path fetches the result with
    /// `((PyStopIterationObject *)ev)->value` when `Py_IS_TYPE(ev,
    /// PyExc_StopIteration)` holds, and `__Pyx_ErrRestore` compares
    /// `((PyBaseExceptionObject *)value)->traceback` before calling
    /// `PyException_SetTraceback`. With the payload at offset 16 those
    /// reads landed inside the Rust `PayloadCell` (or past the
    /// allocation), so every awaited uvloop coroutine "returned" None.
    /// The tail costs 64 bytes per box and is all-NULL except for
    /// exception instances, which [`mint_instance_box`] fills.
    pub exc: ExcFields,
    pub payload: PayloadCell,
}

/// See [`PyObjectBox::exc`]. Field order and padding mirror CPython
/// 3.13's `PyBaseExceptionObject` after `PyObject_HEAD` (offsets 16–64)
/// plus the single-pointer tail subclasses append at offset 72
/// (`StopIteration.value` / `SystemExit.code`).
#[repr(C)]
pub struct ExcFields {
    pub dict: *mut PyObject,      // +16
    pub args: *mut PyObject,      // +24
    pub notes: *mut PyObject,     // +32
    pub traceback: *mut PyObject, // +40
    pub context: *mut PyObject,   // +48
    pub cause: *mut PyObject,     // +56
    pub suppress_context: u8,     // +64
    _pad: [u8; 7],
    pub value: *mut PyObject, // +72
}

impl Default for ExcFields {
    fn default() -> Self {
        Self {
            dict: std::ptr::null_mut(),
            args: std::ptr::null_mut(),
            notes: std::ptr::null_mut(),
            traceback: std::ptr::null_mut(),
            context: std::ptr::null_mut(),
            cause: std::ptr::null_mut(),
            suppress_context: 0,
            _pad: [0; 7],
            value: std::ptr::null_mut(),
        }
    }
}

impl ExcFields {
    /// The owned references the tail holds (fill-time `into_owned`
    /// results); released by [`free_box`].
    fn owned_fields(&self) -> [*mut PyObject; 2] {
        [self.args, self.value]
    }
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

use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::Mutex;

static MINTED: Mutex<Option<HashSet<usize>>> = Mutex::new(None);

// ---- TEMP foreign-soul liveness tracker (WEAVEPY_TRACK_SOULS) ------------
// Precise premature-free detector for foreign proxies. `fwd_incref` /
// `fwd_decref` (the ForeignHooks incref/decref) are called *exactly* when a
// `PyForeignSoul` is born / dies, so this map counts how many live souls
// reference each foreign pointer. If `free_box` frees a foreign box while its
// soul count is still > 0, some path over-decref'd the C refcount and the VM
// still holds a dangling proxy — the merge UAF. Gated on an env var.
static SOULS: Mutex<Option<HashMap<usize, u32>>> = Mutex::new(None);

pub fn track_souls_enabled() -> bool {
    use std::sync::OnceLock;
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("WEAVEPY_TRACK_SOULS").is_some())
}

pub fn soul_inc(p: usize) {
    if !track_souls_enabled() || p == 0 {
        return;
    }
    if let Ok(mut g) = SOULS.lock() {
        *g.get_or_insert_with(HashMap::new).entry(p).or_insert(0) += 1;
    }
}

/// Decrement the live-soul count for `p`. Must be called *before* the
/// underlying `Py_DecRef`, so that the last soul's own decref (which frees
/// the box) sees a zero count and is not flagged.
pub fn soul_dec(p: usize) {
    if !track_souls_enabled() || p == 0 {
        return;
    }
    if let Ok(mut g) = SOULS.lock() {
        if let Some(m) = g.as_mut() {
            if let Some(c) = m.get_mut(&p) {
                *c = c.saturating_sub(1);
                if *c == 0 {
                    m.remove(&p);
                }
            }
        }
    }
}

fn soul_count(p: usize) -> u32 {
    if !track_souls_enabled() || p == 0 {
        return 0;
    }
    SOULS
        .lock()
        .ok()
        .and_then(|g| g.as_ref().and_then(|m| m.get(&p).copied()))
        .unwrap_or(0)
}

pub fn freebox_trace_enabled() -> bool {
    use std::sync::OnceLock;
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("WEAVEPY_FREEBOX_TRACE").is_some())
}

thread_local! {
    static FREEBOX_DEPTH: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

struct FreeBoxDepthGuard;
impl FreeBoxDepthGuard {
    fn enter() -> Self {
        FREEBOX_DEPTH.with(|c| c.set(c.get() + 1));
        FreeBoxDepthGuard
    }
}
impl Drop for FreeBoxDepthGuard {
    fn drop(&mut self) {
        FREEBOX_DEPTH.with(|c| c.set(c.get().saturating_sub(1)));
    }
}

pub(crate) unsafe fn debug_type_name(p: *mut PyObject) -> String {
    if p.is_null() {
        return "<null>".to_string();
    }
    let ty = unsafe { (*p).ob_type };
    if ty.is_null() {
        return "<null-type>".to_string();
    }
    let np = unsafe { (*(ty as *mut crate::layout::PyTypeObjectFull)).tp_name };
    if np.is_null() {
        return "<null-name>".to_string();
    }
    unsafe { std::ffi::CStr::from_ptr(np) }
        .to_string_lossy()
        .into_owned()
}

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
            .finish_non_exhaustive()
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
    // RFC 0076 WS5: a VM code object crosses as a faithful `PyCodeObject`
    // facade (identity-cached), so compiled `PyCode_Check` passes —
    // torch._dynamo's `skip_code(fn.__code__)` guards on exactly that.
    if matches!(obj, Object::Code(_)) {
        if let Some(p) = crate::code_obj::facade_for_vm_code(&obj) {
            unsafe { Py_IncRef(p) };
            return p;
        }
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
        // A type's canonical `PyObject*` is always a `PyTypeObject`. If
        // none is registered yet (a Python-defined class — e.g. a stdlib
        // type like `enum.Enum` — crossing into C for the first time),
        // mint one on demand rather than falling through to the generic
        // instance-box path, which would hand C a box wearing bare `type`
        // and lose the real metaclass (`Py_TYPE(cls)`).
        //
        // Use the *same* resolver order as `type_for_object` does for an
        // instance — `find → synth → install` — so a protocol class (one
        // whose instances drive a C-level `tp_iter`/`tp_iternext`/… read,
        // e.g. `itertools.cycle`) registers its slot-bearing synth type,
        // not a bare `install_user_type` shell. Otherwise the bare type
        // pollutes the registry and a later instance crossing finds it
        // first, losing `tp_iternext` ("cycle object is not an iterator").
        let p = crate::types::type_ptr_for_class(t)
            .or_else(|| crate::types::synth_type_for_class(t))
            .unwrap_or_else(|| crate::types::install_user_type(t));
        let p = p as *mut PyObject;
        unsafe { Py_IncRef(p) };
        return p;
    }
    // RFC 0066 WS6: a module crosses into C as one *stable, immortal*
    // box per module (keyed by native `Rc` identity), never a fresh
    // per-crossing box. Extensions routinely keep *borrowed* module
    // handles past the owning reference: pybind11's chained accessor
    // (`py::module_::import("builtins").attr("int")` held in a local,
    // matplotlib's ft2font init) stores just a borrowed handle while
    // the temporary that owned the module is decref'd at the end of
    // the statement — safe on CPython where modules sit in
    // `sys.modules` effectively forever, a dangling pointer with
    // per-crossing boxes. Modules are process-immortal here too, so
    // one immortal box is the faithful representation (and module
    // pointer identity across crossings now holds, as extensions
    // expect).
    if let Object::Module(m) = &obj {
        return stable_module_box(m);
    }
    // RFC 0076 WS1: a `property` crosses as one canonical box per native
    // property (keyed by `Rc` identity), like builtins do through the
    // mirror path's canonical cache. Extensions compare descriptors by
    // *pointer*: numpy's `PyArray_View` decides its dtype-propagation
    // path by testing `getattr(subtype, "dtype")` against the descriptor
    // it captured at import (`npy_static_pydata.ndarray_dtype_descr`);
    // a fresh box per crossing made every ndarray subclass look like it
    // overrode `dtype`, silently bypassing subclass `dtype` setters
    // (test_view_dtype_property_setter). The box is refcounted like any
    // legacy box; `free_box` evicts the cache entry.
    if let Object::Property(rc) = &obj {
        let key = weavepy_vm::sync::Rc::as_ptr(rc) as usize;
        // RFC 0076 WS5: a *harvested C getset* crosses as a byte-faithful
        // `getset_descriptor` (see [`GetSetDescrBox`]) — extensions
        // classify descriptors by `tp_name` and poke the struct directly
        // (torch's `add_docstr`).
        if let Some(meta) = weavepy_vm::descr_registry::lookup(&obj) {
            if matches!(meta.kind, weavepy_vm::descr_registry::DescrKind::GetSet) {
                let p = getset_descr_box(key, &obj, &meta);
                unsafe { Py_IncRef(p) };
                return p;
            }
        }
        if let Some(p) = cached_property_box(key) {
            return p;
        }
        let ty = crate::types::type_for_object(&obj);
        let boxed = Box::new(PyObjectBox {
            head: PyObject {
                ob_refcnt: 1,
                ob_type: ty,
            },
            exc: ExcFields::default(),
            payload: PayloadCell::from_object(obj),
        });
        let raw = Box::into_raw(boxed) as *mut PyObject;
        register_minted(raw);
        register_property_box(key, raw);
        return raw;
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
        if std::env::var_os("WEAVEPY_TRACE_CTOR").is_some() {
            let path = if crate::types::is_inline_instance_type(ty) {
                "inline"
            } else if inst.c_body.get() != 0 {
                "cached_box"
            } else {
                "mint_box"
            };
            eprintln!(
                "[CTOR] into_owned name={} ty={:p} inline={} basicsize={} cls={} path={} c_body=0x{:x}",
                crate::types::ctor_trace_name(ty),
                ty,
                crate::types::is_inline_instance_type(ty),
                unsafe { (*ty).tp_basicsize },
                inst.cls().name,
                path,
                inst.c_body.get(),
            );
        }
        if crate::types::is_inline_instance_type(ty) {
            return crate::instance::instance_body_out(inst, ty);
        }
        // RFC 0047 (wave 5): a list/tuple-subclass instance crosses as a
        // faithful container body (see `instance::container_body_out`) so
        // stock `PyList_Check` + `PyList_GET_ITEM`/`Py_SIZE` macro reads
        // land on real slots.
        if let Some(p) = crate::instance::container_body_out(inst, ty) {
            return p;
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
        exc: ExcFields::default(),
        payload: PayloadCell::from_object(obj),
    });
    let raw = Box::into_raw(boxed) as *mut PyObject;
    register_minted(raw);
    raw
}

/// CPython's `PyGetSetDef` — the writable slot record a faithful
/// getset-descriptor box points its `d_getset` at.
#[repr(C)]
pub struct PyGetSetDefC {
    pub name: *const std::os::raw::c_char,
    pub get: *mut std::ffi::c_void,
    pub set: *mut std::ffi::c_void,
    pub doc: *const std::os::raw::c_char,
    pub closure: *mut std::ffi::c_void,
}

/// A byte-faithful `PyGetSetDescrObject` (RFC 0076 WS5). A harvested C
/// getset crosses back into C wearing `getset_descriptor` with a real
/// `d_getset` record: torch's `add_docstr(torch.Generator.device, …)`
/// gates on `strcmp(Py_TYPE(obj)->tp_name, "getset_descriptor")` and then
/// writes `((PyGetSetDescrObject *) obj)->d_getset->doc` straight into
/// the struct — a generic `property` box both failed the name check
/// ("don't know how to add docstring to type 'property'") and carries no
/// memory at offset 40 to take the write. The `PyGetSetDef` lives inline
/// so the box is self-contained; VM recovery goes through the
/// [`GETSET_DESCR_PAYLOAD`] side table (the layout has no payload slot).
#[repr(C)]
struct GetSetDescrBox {
    head: PyObject,                          // + 0
    d_type: *mut crate::types::PyTypeObject, // +16
    d_name: *mut PyObject,                   // +24
    d_qualname: *mut PyObject,               // +32
    d_getset: *mut PyGetSetDefC,             // +40
    getset: PyGetSetDefC,                    // +48 (d_getset points here)
}

/// One immortal faithful box per harvested getset descriptor, keyed by
/// the property's `Rc` identity. Descriptors live exactly as long as
/// their (immortal, readied) owning class, so the boxes are immortal too.
static GETSET_DESCR_BOXES: std::sync::Mutex<Option<std::collections::HashMap<usize, usize>>> =
    std::sync::Mutex::new(None);

/// Box pointer → the VM `Object::Property` it stands for, consulted by
/// [`clone_object`] (the faithful layout carries no Rust payload).
static GETSET_DESCR_PAYLOAD: std::sync::Mutex<Option<std::collections::HashMap<usize, Object>>> =
    std::sync::Mutex::new(None);

/// The faithful `getset_descriptor` box for `obj` (a registry-tagged
/// harvested getset property), minting on first crossing.
fn getset_descr_box(
    rc_key: usize,
    obj: &Object,
    meta: &weavepy_vm::descr_registry::DescrMeta,
) -> *mut PyObject {
    if let Ok(g) = GETSET_DESCR_BOXES.lock() {
        if let Some(&p) = g.as_ref().and_then(|m| m.get(&rc_key)) {
            return p as *mut PyObject;
        }
    }
    let d_type = crate::types::type_ptr_for_class(&meta.objclass).unwrap_or(std::ptr::null_mut());
    let name_c = std::ffi::CString::new(meta.name.as_str())
        .unwrap_or_default()
        .into_raw();
    let d_name = crate::object::into_owned(Object::from_str(meta.name.clone()));
    let mut bx = Box::new(GetSetDescrBox {
        head: PyObject {
            ob_refcnt: IMMORTAL_REFCNT,
            ob_type: crate::types::PyGetSetDescr_Type.as_ptr(),
        },
        d_type,
        d_name,
        d_qualname: std::ptr::null_mut(),
        d_getset: std::ptr::null_mut(),
        getset: PyGetSetDefC {
            name: name_c,
            get: std::ptr::null_mut(),
            set: std::ptr::null_mut(),
            doc: meta
                .doc
                .and_then(|d| std::ffi::CString::new(d).ok())
                .map_or(std::ptr::null(), |c| c.into_raw() as *const _),
            closure: std::ptr::null_mut(),
        },
    });
    bx.d_getset = &mut bx.getset as *mut PyGetSetDefC;
    let raw = Box::into_raw(bx) as *mut PyObject;
    register_minted(raw);
    if let Ok(mut g) = GETSET_DESCR_BOXES.lock() {
        g.get_or_insert_with(Default::default)
            .insert(rc_key, raw as usize);
    }
    if let Ok(mut g) = GETSET_DESCR_PAYLOAD.lock() {
        g.get_or_insert_with(Default::default)
            .insert(raw as usize, obj.clone());
    }
    raw
}

/// The VM object behind a faithful getset-descriptor box, or `None` for
/// every other pointer.
pub(crate) fn getset_descr_payload(p: *mut PyObject) -> Option<Object> {
    let g = GETSET_DESCR_PAYLOAD.lock().ok()?;
    g.as_ref()?.get(&(p as usize)).cloned()
}

/// Live `d_getset->doc` of the faithful box minted for `prop` — an
/// extension may have written it after harvest (numpy's
/// `add_docstring(np.ndarray.flat, …)` stores the docstring straight into
/// the C struct at import time — test_umath TestAddDocstring, RFC 0076
/// WS1). `None` when no box exists or the slot is still NULL.
pub(crate) fn getset_live_doc(prop: &Object) -> Option<String> {
    let Object::Property(rc) = prop else {
        return None;
    };
    let key = weavepy_vm::sync::Rc::as_ptr(rc) as usize;
    let bp = {
        let g = GETSET_DESCR_BOXES.lock().ok()?;
        *g.as_ref()?.get(&key)?
    };
    let bx = unsafe { &*(bp as *mut GetSetDescrBox) };
    let doc = unsafe { (*bx.d_getset).doc };
    if doc.is_null() {
        return None;
    }
    Some(
        unsafe { std::ffi::CStr::from_ptr(doc) }
            .to_string_lossy()
            .into_owned(),
    )
}

/// One canonical box per native `property`, keyed by `Rc` identity (see
/// the `Object::Property` arm of [`into_owned`]). Unlike [`MODULE_BOXES`]
/// these are ordinary refcounted boxes; [`free_box`] evicts the entry
/// when the box is released, so the map only ever holds live pointers.
static PROPERTY_BOXES: std::sync::Mutex<Option<std::collections::HashMap<usize, usize>>> =
    std::sync::Mutex::new(None);

/// Live canonical box for property identity `key`, as a fresh reference
/// (matching the mint path's "+1" contract); `None` when uncached.
fn cached_property_box(key: usize) -> Option<*mut PyObject> {
    let g = PROPERTY_BOXES.lock().ok()?;
    let p = *g.as_ref()?.get(&key)? as *mut PyObject;
    unsafe { Py_IncRef(p) };
    Some(p)
}

fn register_property_box(key: usize, p: *mut PyObject) {
    if let Ok(mut g) = PROPERTY_BOXES.lock() {
        g.get_or_insert_with(Default::default)
            .insert(key, p as usize);
    }
}

/// Evict `p` from the canonical property cache (no-op when a racing mint
/// already replaced the entry).
fn evict_property_box(key: usize, p: *mut PyObject) {
    if let Ok(mut g) = PROPERTY_BOXES.lock() {
        if let Some(map) = g.as_mut() {
            if map.get(&key) == Some(&(p as usize)) {
                map.remove(&key);
            }
        }
    }
}

/// One immortal box per module, keyed by the module's native `Rc`
/// identity (see the `Object::Module` arm of [`into_owned`]). The map
/// holds the boxes for the process lifetime — exactly the lifetime of
/// the modules themselves.
static MODULE_BOXES: std::sync::Mutex<Option<std::collections::HashMap<usize, usize>>> =
    std::sync::Mutex::new(None);

fn stable_module_box(m: &weavepy_vm::sync::Rc<weavepy_vm::object::PyModule>) -> *mut PyObject {
    let key = weavepy_vm::sync::Rc::as_ptr(m) as usize;
    if let Ok(g) = MODULE_BOXES.lock() {
        if let Some(&p) = g.as_ref().and_then(|map| map.get(&key)) {
            // Immortal: no refcount traffic.
            return p as *mut PyObject;
        }
    }
    let module = Object::Module(m.clone());
    let boxed = Box::new(PyObjectBox {
        head: PyObject {
            ob_refcnt: IMMORTAL_REFCNT,
            ob_type: crate::types::type_for_object(&module),
        },
        exc: ExcFields::default(),
        payload: PayloadCell::from_object(module),
    });
    let raw = Box::into_raw(boxed) as *mut PyObject;
    register_minted(raw);
    if let Ok(mut g) = MODULE_BOXES.lock() {
        // A racing thread may have minted its own box; last write wins
        // and both stay valid (immortal, never freed).
        g.get_or_insert_with(std::collections::HashMap::new)
            .insert(key, raw as usize);
    }
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
    if crate::mirror::arg_pin_active() {
        pin_instance_arg_box(p);
    }
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
        exc: ExcFields::default(),
        payload: PayloadCell::from_object(Object::Instance(inst.clone())),
    });
    let raw = Box::into_raw(boxed) as *mut PyObject;
    register_minted(raw);
    inst.c_body.set(raw as usize);
    fill_exception_tail(raw, inst);
    if crate::mirror::arg_pin_active() {
        pin_instance_arg_box(raw);
    }
    if ibox_trace_enabled() {
        eprintln!(
            "[IBOX-MINT] p=0x{:x} cls={} rc=1 argpin={}",
            raw as usize,
            inst.cls().name,
            crate::mirror::arg_pin_active(),
        );
        if let Some(filter) = std::env::var_os("WEAVEPY_IBOX_BT") {
            if filter.to_string_lossy() == inst.cls().name.as_str() {
                eprintln!("{}", std::backtrace::Backtrace::force_capture());
            }
        }
    }
    raw
}

/// Fill the C-visible exception tail of a freshly minted identity box
/// (see [`PyObjectBox::exc`]). Only `args` and — for `StopIteration` —
/// `value` are populated: those are the fields compiled Cython reads
/// through struct offsets. `traceback`/`context`/`cause` stay NULL,
/// which is both a valid CPython state and what keeps the fill cheap
/// and cycle-free (a chained `__context__` would otherwise recurse the
/// mint down the whole chain); consumers reach those fields through the
/// `PyException_Get*` functions, which read the live VM attributes.
///
/// Runs *after* `inst.c_body` is set, so a self-referential `args`
/// crossing back through [`into_owned`] reuses this box instead of
/// recursing.
fn fill_exception_tail(
    raw: *mut PyObject,
    inst: &weavepy_vm::sync::Rc<weavepy_vm::types::PyInstance>,
) {
    let bt = weavepy_vm::builtin_types::builtin_types();
    let cls = inst.cls();
    if !cls.is_subclass_of(&bt.base_exception) {
        return;
    }
    let bx = raw as *mut PyObjectBox;
    if let Some(args @ Object::Tuple(_)) = inst.slot_get("args") {
        unsafe { (*bx).exc.args = into_owned(args) };
    }
    if cls.is_subclass_of(&bt.stop_iteration) {
        // Never NULL for a constructed StopIteration on CPython (`__init__`
        // stores `args[0]` or `Py_None`), and Cython increfs the read
        // unconditionally — so fill None as the None singleton, not NULL.
        let value = inst.slot_get("value").unwrap_or(Object::None);
        unsafe { (*bx).exc.value = into_owned(value) };
    }
    if std::env::var_os("WEAVEPY_TRACE_EXCTAIL").is_some() {
        unsafe {
            eprintln!(
                "[EXCTAIL] mint p=0x{:x} cls={} args=0x{:x} value=0x{:x}",
                raw as usize,
                cls.name,
                (*bx).exc.args as usize,
                (*bx).exc.value as usize,
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Argument-pinned instance identity boxes (RFC 0069 WS5).
//
// numpy's `PyArrayIdentityHash` stores raw `PyObject*` keys and values
// **borrowed** — no incref, the caller guarantees the lifetime through its
// own references (CPython: the test's `keys_vals` list keeps the objects,
// and therefore the pointers, alive). On WeavePy the VM object and its
// identity box have separate lifetimes: the box died with C's refcount when
// the args tuple was released at call end, so the table dangled — the next
// `identity_hash_get_item` incref'd freed memory (SIGSEGV, the
// `test_hashtable` census row).
//
// Fix, mirroring the scalar pin cache (`mirror::ScalarPinKey`): an instance
// identity box that crosses while marshaling VM arguments into a C call is
// recorded here. At C refcount zero, `free_box` *parks* it instead of
// freeing — the box (and `inst.c_body`, so pointer identity stays stable
// for the next crossing) survives while the VM instance is reachable
// elsewhere. Once only the box's own payload clone keeps the instance
// alive, the entry is reclaimed (at the next refcount-zero event or by the
// high-water-mark sweep).
// ---------------------------------------------------------------------------

static INSTANCE_ARG_PINS: std::sync::Mutex<Option<weavepy_vm::fasthash::FxHashSet<usize>>> =
    std::sync::Mutex::new(None);
/// Eviction sweep threshold (entries), matching the scalar pin cache.
const INSTANCE_PIN_HWM: usize = 1 << 16;

/// Record `p` (a live instance identity box) as argument-pinned, sweeping
/// parked-and-dead entries first when past the high-water mark.
fn pin_instance_arg_box(p: *mut PyObject) {
    let victims: Vec<usize> = {
        let mut g = match INSTANCE_ARG_PINS.lock() {
            Ok(g) => g,
            Err(_) => return,
        };
        let set = g.get_or_insert_with(weavepy_vm::fasthash::FxHashSet::default);
        let victims = if set.len() >= INSTANCE_PIN_HWM {
            let dead: Vec<usize> = set
                .iter()
                .copied()
                .filter(|&bp| unsafe { pinned_box_is_dead(bp as *mut PyObject) })
                .collect();
            for bp in &dead {
                set.remove(bp);
            }
            dead
        } else {
            Vec::new()
        };
        set.insert(p as usize);
        victims
    };
    // Free outside the lock: `free_box` re-consults the registry (the
    // victim is already evicted, so it takes the ordinary free path).
    for bp in victims {
        unsafe { free_box(bp as *mut PyObject) };
    }
}

/// True iff a parked pinned box is reclaimable: C holds no reference and
/// the box's own payload clone is the *only* strong reference left to the
/// instance (the program can never cross it again).
///
/// # Safety
/// `bp` must be a live [`PyObjectBox`] whose payload is `Object::Instance`.
unsafe fn pinned_box_is_dead(bp: *mut PyObject) -> bool {
    if unsafe { (*bp).ob_refcnt } > 0 {
        return false;
    }
    let bx = unsafe { &*(bp as *mut PyObjectBox) };
    match &bx.payload.obj {
        Object::Instance(inst) => weavepy_vm::sync::Rc::strong_count(inst) <= 1,
        _ => true,
    }
}

/// Count the `Object` clones of `target` held *only* by the C-API pin
/// caches — a **parked** argument-pinned identity box whose payload is
/// `target`, or a dead-except-pin scalar/tuple pin that is (or directly
/// contains) `target`. `sys.getrefcount` discounts these (RFC 0076 WS1):
/// on CPython the corresponding memory is already freed, so the clones
/// must not read as program-visible references
/// (test_cleanup_with_refs_non_contig asserts the count returns to
/// baseline after the arrays die, while the pin caches deliberately hold
/// their boxes past C refcount zero).
pub(crate) fn pin_clone_count_hook(target: &Object) -> usize {
    let mut n = 0;
    if let Ok(g) = INSTANCE_ARG_PINS.lock() {
        if let Some(set) = g.as_ref() {
            for &bp in set.iter() {
                let p = bp as *mut PyObject;
                let bx = unsafe { &*(p as *mut PyObjectBox) };
                if !same_native_identity(&bx.payload.obj, target) {
                    continue;
                }
                // Parked (refcount ≤ 0), or held *only* by dead
                // scalar-pinned tuple mirrors — storage the program can no
                // longer reach (a released args tuple's `ob_item` slot). A
                // box with any genuinely live C reference plays the role of
                // CPython's own C-side refs and stays visible.
                let rc = unsafe { (*p).ob_refcnt };
                if ibox_trace_enabled() {
                    eprintln!(
                        "[PIN-COUNT] box=0x{:x} rc={} deadrefs={}",
                        bp,
                        rc,
                        crate::mirror::dead_pin_c_refs_to(p)
                    );
                }
                if rc <= 0 || (rc as usize) <= crate::mirror::dead_pin_c_refs_to(p) {
                    n += 1;
                }
            }
        }
    }
    n + crate::mirror::dead_pin_clones_of(target)
}

/// Surplus raw C references to an instance's faithful body: `ob_refcnt`
/// beyond the single `Rc` the [`crate::instance::STRONG`] pin holds while
/// any C reference exists, minus refs held only by dead scalar-pinned
/// tuple mirrors (a released args tuple's `ob_item` slot). An extension
/// bumping the body with the inline `Py_INCREF` macro (numpy's
/// `NpyIter_Copy` increfs its operands this way) never mints an `Rc`
/// clone, so `sys.getrefcount` adds this on top of the `Rc` count
/// (test_nditer's test_iter_refcount — RFC 0076 WS1).
pub(crate) fn extra_c_refs_hook(target: &Object) -> usize {
    let Object::Instance(inst) = target else {
        return 0;
    };
    let body = inst.c_body.get();
    if body == 0 {
        return 0;
    }
    let p = body as *mut PyObject;
    let rc = unsafe { (*p).ob_refcnt };
    if rc <= 1 {
        return 0;
    }
    (rc as usize - 1).saturating_sub(unsafe { crate::mirror::dead_pin_c_refs_to(p) })
}

/// Identity (`is`) comparison for the payload variants the pin caches can
/// hold. Conservative: unknown variants compare unequal.
pub(crate) fn same_native_identity(a: &Object, b: &Object) -> bool {
    use weavepy_vm::sync::Rc;
    match (a, b) {
        (Object::Instance(x), Object::Instance(y)) => Rc::ptr_eq(x, y),
        (Object::Tuple(x), Object::Tuple(y)) => Rc::ptr_eq(x, y),
        (Object::Bytes(x), Object::Bytes(y)) => Rc::ptr_eq(x, y),
        (Object::Str(x), Object::Str(y)) => Rc::ptr_eq(x, y),
        (Object::Long(x), Object::Long(y)) => Rc::ptr_eq(x, y),
        (Object::Complex(x), Object::Complex(y)) => Rc::ptr_eq(x, y),
        _ => false,
    }
}

/// Park decision for `free_box`: keep an argument-pinned identity box
/// alive past C refcount zero while the VM instance is still reachable
/// elsewhere. Returns `true` when the box must survive (caller returns
/// without freeing). When the instance is no longer reachable outside the
/// box, the entry is evicted and `false` is returned so the ordinary free
/// path reclaims it.
fn instance_pin_parks(
    p: *mut PyObject,
    inst: &weavepy_vm::sync::Rc<weavepy_vm::types::PyInstance>,
) -> bool {
    let mut g = match INSTANCE_ARG_PINS.lock() {
        Ok(g) => g,
        Err(_) => return false,
    };
    let Some(set) = g.as_mut() else {
        return false;
    };
    if !set.contains(&(p as usize)) {
        return false;
    }
    // The payload's own clone is one strong count; anything above it means
    // the program (or another C reference chain) can still reach the
    // instance and re-cross it — keep the pointer valid and stable.
    if weavepy_vm::sync::Rc::strong_count(inst) > 1 {
        return true;
    }
    set.remove(&(p as usize));
    false
}

/// Cached `WEAVEPY_IBOX_TRACE` gate: trace identity-box mint/incref/free
/// for non-inline instances (leak triage; RFC 0047, wave 5).
pub(crate) fn ibox_trace_enabled() -> bool {
    use std::sync::OnceLock;
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("WEAVEPY_IBOX_TRACE").is_some())
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
        // Container-body types (list/tuple subclasses, RFC 0047 wave 5) are
        // excluded like inline types: a plain box carrying their `ob_type`
        // would be misclassified as a mirror by the type-keyed `is_mirror`.
        if !crate::types::is_inline_instance_type(ty)
            && !crate::mirror::type_is_faithful(ty)
            && !crate::types::is_container_body_type(ty)
        {
            let boxed = Box::new(PyObjectBox {
                head: PyObject {
                    ob_refcnt: 1,
                    ob_type: ty,
                },
                exc: ExcFields::default(),
                payload: PayloadCell::from_object(Object::Instance(inst.clone())),
            });
            let raw = Box::into_raw(boxed) as *mut PyObject;
            register_minted(raw);
            fill_exception_tail(raw, inst);
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
        let p = crate::types::type_ptr_for_class(t)
            .or_else(|| crate::types::synth_type_for_class(t))
            .unwrap_or_else(|| crate::types::install_user_type(t));
        let p = p as *mut PyObject;
        unsafe { Py_IncRef(p) };
        return p;
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
        // RFC 0047 (wave 5): container-subclass instances cross as their
        // faithful list/tuple-shaped body (see [`into_owned`]).
        if let Some(p) = crate::instance::container_body_out(inst, ty) {
            return p;
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
        exc: ExcFields::default(),
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
    // PyTypeObject extends PyObject; resolve any WeavePy-owned type box
    // (static, heap, or readied) back to its bridged `Object::Type`.
    //
    // `bridge_type` is pointer-identity-safe: it checks the readied side
    // table and the static/heap registries *before* reading the private
    // `bridge` field, so it never dereferences offset 424 of a foreign or
    // non-type box. It must NOT be gated on `ob_type == PyType_Type`: a
    // WeavePy class whose metaclass is not bare `type` (e.g. `enum.Enum`,
    // whose `ob_type` is now its real `EnumType` mirror, or numpy's DType
    // classes carrying `ob_type == &PyArrayDTypeMeta_Type`) would
    // otherwise fail this check and be opaquely proxied as a foreign
    // `'object'` — breaking `cls.__mro__`, `isinstance(x, cls)`, etc.
    // RFC 0066 WS2: a faithful datetime type *shell* resolves to the
    // **current interpreter's** live `datetime` class, so the attribute
    // protocol — `DateType.today()`, `DeltaType.resolution`, … — answers
    // through the bridged VM class instead of an opaque foreign proxy.
    // Checked BEFORE the generic `bridge_type` read: the shell's `bridge`
    // is process-global and wired to whichever interpreter's class
    // crossed first, which under an interpreter-per-test host hands back
    // a class whose builtins have foreign identity (`isinstance(x, int)`
    // inside `_pydatetime` then fails). Cheap for every other pointer:
    // six pointer compares behind a ready-flag load.
    if let Some(t) = crate::datetime_api::resolve_shell_class(p as *mut crate::types::PyTypeObject)
    {
        return Object::Type(t);
    }
    if let Some(t) = unsafe { crate::types::bridge_type(p as *mut crate::types::PyTypeObject) } {
        return Object::Type(t);
    }
    // RFC 0076 WS5: a faithful getset-descriptor box carries no Rust
    // payload (byte-faithful `PyGetSetDescrObject` layout); resolve it
    // through the side table before any payload-offset read.
    if let Some(o) = getset_descr_payload(p) {
        return o;
    }
    // RFC 0046 (wave 4): a pointer WeavePy did not mint — a static numpy
    // `PyArray_Descr`, an extension-built function object, an un-bridged
    // type — is *foreign*. It is none of the shapes below, so interpreting
    // it as one corrupts memory. Decided BEFORE the capsule/mirror checks
    // because `is_mirror` is type-based and would mis-claim a foreign object
    // whose (readied) type was registered for inline storage. Proxy it
    // opaquely; it round-trips back to the same pointer via `into_owned`.
    if !crate::object::is_weavepy_owned(p) {
        // RFC 0055 WS5: a `_PyLong_New` block is a genuine `PyLongObject`
        // whose digits the extension wrote after allocation — decode the
        // value rather than proxying the pointer.
        if crate::mypyc_tail::is_raw_long(p) {
            return unsafe { crate::mypyc_tail::decode_raw_long(p) };
        }
        // RFC 0076 WS5: a facade minted for a *VM* code object crossing
        // into C hands back the original `Object::Code` (identity), not
        // a decode of the facade bytes.
        if let Some(code) = crate::code_obj::vm_code_payload(p) {
            return code;
        }
        // RFC 0066 WS3: a C-minted facade code object (a cyfunction's
        // `__code__`) decodes into a genuine `types.CodeType` instance —
        // `inspect.signature` needs the isinstance to pass.
        if let Some(code) = unsafe { crate::code_obj::native_code_object(p) } {
            return code;
        }
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
        // RFC 0075 WS9: an *orphaned* instance body — the owning
        // `PyInstance` died while C still held inline-acquired references
        // (see `instance::free_instance_body_hook`) — is now a C-owned
        // object. Its dead `Weak` makes `native_of` collapse to `None`;
        // proxy it opaquely instead, like any foreign pointer, so a read
        // through it (lxml's `iterparse.root` after iteration) reaches
        // the live extension object.
        // A dead `Weak` alone is *not* proof of orphanhood: the body may be
        // **mid-dealloc** right now — `dealloc_and_free_body` is running the
        // extension's `tp_dealloc` above us on this very stack (a Cython
        // generator's scope-struct dealloc closes the generator, whose
        // `finally:` code crosses `self` back in; psycopg's failed-connect
        // teardown). Wrapping that body as a foreign proxy would pin a block
        // that is freed the moment the dealloc returns — the proxy's later
        // decref lands on freed memory (glibc's `free(): invalid pointer`
        // abort on the psycopg ecosystem row). The in-flight list names
        // exactly the bodies in that window; collapse their crossings to
        // `None` via `native_of`'s dead-`Weak` arm, the pre-orphan behavior.
        if unsafe { crate::mirror::is_orphaned_instance_body(p) }
            && !crate::instance::body_free_in_flight(p as usize)
        {
            if crate::mirror::body_trace_enabled() {
                eprintln!(
                    "[ORPHAN-CROSS] p=0x{:x} refcnt={}\n{}",
                    p as usize,
                    unsafe { (*p).ob_refcnt },
                    std::backtrace::Backtrace::force_capture()
                );
            }
            return unsafe { crate::foreign::wrap_foreign(p) };
        }
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

/// [`clone_object`], then unwrap a builtin-subclass instance to the
/// primitive value it *is* (its seeded `native` payload) — numpy's
/// `str_`/`bytes_`, a Python `class S(str)`, an `int` subclass, ….
///
/// This is the value-semantics twin of [`clone_object`] for C-API
/// functions that operate on the underlying `str`/`bytes`/numeric value
/// and, per CPython, accept subtypes (`PyUnicode_AsUTF8`,
/// `PyBytes_AsString`, `PyUnicode_Compare`, …) — CPython reads the value
/// straight off the subtype's layout-compatible struct. Identity- and
/// type-sensitive callers (`type()`, `isinstance`, boxing) must keep
/// using [`clone_object`].
#[allow(clippy::missing_safety_doc)]
pub unsafe fn clone_object_value(p: *mut PyObject) -> Object {
    let o = unsafe { clone_object(p) };
    if let Object::Instance(inst) = &o {
        if let Some(n) = inst.native.get() {
            return n.clone();
        }
    }
    o
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

/// Release the storage of a WeavePy-minted pointer through its owning
/// path (mirror bridge, instance-body borrow release, or plain box
/// drop). Public entry for [`crate::memory::PyObject_Free`], which a
/// stock extension `tp_dealloc` chain invokes as `tp_free(self)`; the
/// raw system `free` would corrupt Rust-allocated storage.
///
/// # Safety
/// `p` must be a live WeavePy-minted object (`is_weavepy_owned`).
pub unsafe fn free_owned_storage(p: *mut PyObject) {
    unsafe { free_box(p) }
}

/// Drop a box's storage, running its destructor (if any) first.
///
/// SAFETY: `p` must be a heap-allocated box previously produced by
/// [`into_owned`] / [`into_owned_with_type`] / capsule / module
/// helpers. Static singletons short-circuit through the immortal
/// check in [`Py_DecRef`].
pub(crate) unsafe fn free_box(p: *mut PyObject) {
    // Thread/process-teardown guard. At exit Rust destroys thread-local
    // storage; a deep exception pinned in *another* thread-local (e.g. a
    // `RecursionError` whose ~1000-frame traceback is retained to exit) is
    // then dropped in that window and decref-s its C mirror pointers back
    // through here. The caches and type registry `free_box` unconditionally
    // consults (`BORROWED_ITEM_CACHE`, `DICT_BOX_CACHE`, `INLINE_TYPES`) may
    // already be destroyed; touching a destroyed thread-local panics with
    // `AccessError` → `abort()`, killing the process *after* it has already
    // finished (and printed) its work — which, under the oracle sweep, turns
    // an otherwise-complete run into an unparseable "crash". When any of them
    // is gone we are unambiguously mid-teardown: leak `p` (the OS reclaims all
    // memory at exit) rather than risk that abort — or, worse, a misclassified
    // free against a half-destroyed `INLINE_TYPES` (which could `free_mirror`
    // a plain box and corrupt the heap). Teardown is synchronous per thread,
    // so once these are confirmed live at entry they stay live for the whole
    // (single-threaded) call.
    if !crate::containers::caches_alive() || !crate::types::inline_types_alive() {
        return;
    }
    if crate::mirror::is_watched(p as usize) {
        eprintln!("[WATCH] FREE-BOX 0x{:x} type={}", p as usize, unsafe {
            debug_type_name(p)
        });
        crate::mirror::unwatch_ptr(p as usize);
    }
    // Invalidate any borrowed-item cache entries pinned to this
    // box's address so subsequent reuse of the slab doesn't return
    // stale items from the old container.
    crate::containers::invalidate_borrowed_cache(p);

    // Release any argument references `PyArg_ParseTuple` tethered to this
    // owner's lifetime (RFC 0047, wave 5); no-op unless tethers exist.
    crate::argparse::drop_tethered(p);

    if freebox_trace_enabled() {
        let tyname = unsafe { debug_type_name(p) };
        if tyname.contains("Engine") || tyname.contains("index.") {
            let owned = is_weavepy_owned(p);
            let is_body = unsafe { crate::mirror::is_instance_body(p) };
            let is_mir = unsafe { crate::mirror::is_mirror(p) };
            eprintln!(
                "[FREEBOX-ENTRY] p=0x{:x} type={} owned={} body={} mirror={} depth={}",
                p as usize,
                tyname,
                owned,
                is_body,
                is_mir,
                FREEBOX_DEPTH.with(|c| c.get()),
            );
        }
    }

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
        // RFC 0055 WS5: `_PyLong_New` blocks are libc-allocated; freeing
        // them through a type's tp_dealloc would misclassify the block.
        if crate::mypyc_tail::take_raw_long(p) {
            unsafe { libc::free(p as *mut core::ffi::c_void) };
            return;
        }
        let live = soul_count(p as usize);
        if live > 0 {
            let tyname = unsafe { debug_type_name(p) };
            eprintln!(
                "[SOUL-UAF] freeing foreign p=0x{:x} type={} while {} soul(s) alive",
                p as usize, tyname, live
            );
            eprintln!("{}", std::backtrace::Backtrace::force_capture());
        }
        if crate::object::freebox_trace_enabled() {
            let tyname = unsafe { debug_type_name(p) };
            if tyname.contains("Engine")
                || tyname.contains("Index")
                || tyname.contains("ndarray")
                || FREEBOX_DEPTH.with(|c| c.get()) > 0
            {
                eprintln!(
                    "[FREEBOX] depth={} FOREIGN-FREE p=0x{:x} type={} souls={}",
                    FREEBOX_DEPTH.with(|c| c.get()),
                    p as usize,
                    tyname,
                    live,
                );
            }
        }
        let ty = unsafe { (*p).ob_type };
        if !ty.is_null() {
            if let Some(dealloc) = unsafe { (*ty).tp_dealloc } {
                let _g = FreeBoxDepthGuard::enter();
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
        // RFC 0047 (wave 5): a canonical *pinned scalar* box outlives a
        // zero C refcount — an extension may have stored the pointer
        // borrowed (pandas' khash keys). The pin-cache sweep frees it.
        if unsafe { crate::mirror::is_scalar_pinned(p) } {
            return;
        }
        unsafe { crate::mirror::free_mirror(p) };
        return;
    }

    // TEMP (RFC 0047 wave 5): detect freeing a capsule box that still has a
    // live VM-side soul (`Object::Capsule.handle`). The soul is supposed to
    // hold a lifelong retain, so this reaching zero is a refcount imbalance
    // that leaves the soul's `handle` dangling — the pandas `_pandas_*_CAPI`
    // capsules (stored on the top-level pandas VM module) hit exactly this,
    // making `PyCapsule_Import` fail and crashing datetime code.
    if crate::capsule::cap_trace_enabled() && unsafe { crate::capsule::is_capsule(p) } {
        eprintln!(
            "[CAP] FREE-CAPSULE-BOX p=0x{:x} refcnt={} soul_alive={}",
            p as usize,
            unsafe { (*p).ob_refcnt as i64 },
            crate::capsule::soul_alive(p),
        );
        if crate::capsule::soul_alive(p) {
            eprintln!(
                "[CAP] *** UAF: freeing capsule box with live soul ***\n{}",
                std::backtrace::Backtrace::force_capture()
            );
        }
    }

    // RFC 0069 WS5: an argument-pinned instance identity box outlives a
    // zero C refcount while its VM instance is reachable — an extension may
    // have stored the pointer borrowed with no incref (numpy's
    // `PyArrayIdentityHash` keys/values). Parking it here keeps that
    // borrowed pointer valid and, because `inst.c_body` still holds it,
    // pointer-stable for the next crossing, so C-side identity lookups hit
    // exactly as they do on CPython. See [`INSTANCE_ARG_PINS`].
    {
        let bx = unsafe { &*(p as *mut PyObjectBox) };
        if let Object::Instance(inst) = &bx.payload.obj {
            let parks = inst.c_body.get() == p as usize && instance_pin_parks(p, inst);
            if ibox_trace_enabled() {
                eprintln!(
                    "[IBOX-FREE] p=0x{:x} cls={} c_body=0x{:x} parks={}",
                    p as usize,
                    inst.cls().name,
                    inst.c_body.get(),
                    parks,
                );
            }
            if parks {
                return;
            }
        }
    }

    unregister_minted(p);

    let bx = unsafe { Box::from_raw(p as *mut PyObjectBox) };
    // Release the exception tail's owned references (see
    // [`PyObjectBox::exc`]); all-NULL for non-exception boxes.
    for field in bx.exc.owned_fields() {
        if !field.is_null() {
            unsafe { Py_DecRef(field) };
        }
    }
    // RFC 0046 (wave 4): if this is an instance's cached identity box, drop
    // the `c_body` cache so a subsequent crossing re-mints a fresh box
    // rather than handing C this about-to-be-freed pointer (use-after-free).
    if let Object::Instance(inst) = &bx.payload.obj {
        if inst.c_body.get() == p as usize {
            inst.c_body.set(0);
        }
    }
    // RFC 0076 WS1: likewise drop a canonical property box from its cache
    // so the next crossing re-mints instead of handing out freed memory.
    if let Object::Property(rc) = &bx.payload.obj {
        evict_property_box(weavepy_vm::sync::Rc::as_ptr(rc) as usize, p);
    }
    // RFC 0047 (wave 5): C is dropping what may be the payload's last
    // program-visible reference, from inside an extension call the VM's
    // prompt reaper cannot see. Park instance/container payloads for a
    // refcount-guarded reap at the next eval-loop safe point so anything
    // that died here gets its weakrefs cleared with CPython's timing
    // (pandas' `BlockValuesRefs.has_reference` prunes dead block
    // weakrefs the very next Python-level call after a Cython-internal
    // `self.blocks = ...` rebind). A payload that is still alive
    // elsewhere fails the reap's deadness test untouched.
    weavepy_vm::vm_singletons::queue_cext_dropped(&bx.payload.obj);
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
    if ibox_trace_enabled() && ibox_trace_matches(op) {
        eprintln!(
            "[IBOX-INC] p=0x{:x} type={} rc={}\n{}",
            op as usize,
            unsafe { debug_type_name(op) },
            head.ob_refcnt,
            std::backtrace::Backtrace::force_capture()
        );
    }
}

/// Does `op`'s type name match the `WEAVEPY_IBOX_TYPE` trace filter?
/// (Leak triage; only reachable behind [`ibox_trace_enabled`].)
fn ibox_trace_matches(op: *mut PyObject) -> bool {
    let Ok(filter) = std::env::var("WEAVEPY_IBOX_TYPE") else {
        return false;
    };
    unsafe { debug_type_name(op) }.contains(&filter)
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
    if ibox_trace_enabled() && ibox_trace_matches(op) {
        eprintln!(
            "[IBOX-DEC] p=0x{:x} type={} rc={}\n{}",
            op as usize,
            unsafe { debug_type_name(op) },
            head.ob_refcnt,
            std::backtrace::Backtrace::force_capture()
        );
    }
    if head.ob_refcnt <= 0 && crate::mirror::is_watched(op as usize) {
        eprintln!(
            "[WATCH] FREE-AT-DEC 0x{:x} refcnt={}\n{}",
            op as usize,
            head.ob_refcnt,
            std::backtrace::Backtrace::force_capture()
        );
    }
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

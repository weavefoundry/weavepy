//! The object mirror bridge (RFC 0043, wave 1, WS2).
//!
//! CPython extensions are not merely *callers* of an API; the stock
//! headers *inline* the hot path, so a compiled wheel reads object
//! fields at fixed byte offsets (`PyFloat_AS_DOUBLE` → `*(double*)(op+16)`,
//! `Py_SIZE` → `*(Py_ssize_t*)(op+16)`, `PyTuple_GET_ITEM` →
//! `((PyTupleObject*)op)->ob_item[i]`). WeavePy's native value is a Rust
//! [`Object`] enum with none of those fields at those offsets, so we
//! cannot satisfy a stock reader by interposing a function.
//!
//! Following PyPy's `cpyext` and GraalPy's C-API layer, this module
//! maintains a **layout-faithful mirror**: when a native value crosses
//! into C it is materialised into a heap block whose bytes match the
//! corresponding CPython 3.13 struct ([`crate::layout`]) exactly. The
//! public `*mut PyObject` points at that faithful body; immediately
//! *before* it (a negative offset, invisible to C) sits a
//! [`MirrorPrefix`] holding the owning native [`Object`] — so a pointer
//! WeavePy minted resolves back to its native object in O(1) without a
//! global lookup, while the public pointer stays byte-faithful.
//!
//! Wave 1 fills faithful bodies for the immutable high-frequency types
//! whose internals get inlined (`float`, `int`, `complex`, `bytes`,
//! compact `str`, `tuple`); other types get a head-only "generic" body
//! whose native value still lives in the prefix (so the function-call
//! C-API and `clone_object` work, only stock *inlined field reads* are a
//! later wave). Either way the prefix is uniform, so resolution and
//! freeing are representation-independent.

use std::alloc::{alloc, dealloc, Layout};
use std::os::raw::c_void;
use std::ptr;

use num_bigint::BigInt;
use weavepy_vm::object::Object;

use crate::layout::{self, ustate};
use crate::object::{PyObject, PySsizeT};
use crate::types::{self, PyTypeObject};

/// WeavePy bookkeeping placed immediately before the faithful body. The
/// public `*mut PyObject` is `prefix as *mut u8 + PREFIX_SIZE`, so the
/// prefix is recovered by subtracting [`PREFIX_SIZE`].
#[repr(C)]
pub struct MirrorPrefix {
    /// The owning native object. Holding it here pins the value (its
    /// `Rc`s) for as long as C holds a reference; dropped when the
    /// mirror's refcount reaches zero. For a wave-3 **instance body**
    /// (see [`inst`](Self::inst)) this is [`Object::None`] — the body
    /// only *borrows* its instance, so it must not own a strong `Rc`.
    pub obj: Object,
    /// For a faithful **instance body** (RFC 0045, wave 3) this is a
    /// `Weak` back-reference to the owning native [`PyInstance`]; `None`
    /// for every built-in mirror (which carries its value in
    /// [`obj`](Self::obj)). A `Weak` rather than the strong
    /// `Object::Instance` is what breaks the body↔instance ownership
    /// cycle: the *instance* owns the body (and frees it on drop, via the
    /// `register_instance_body_free` hook), while the body only borrows
    /// back so [`native_of`] can resolve the pointer to its instance.
    pub inst: Option<weavepy_vm::sync::Weak<weavepy_vm::types::PyInstance>>,
    /// Extra C-side state (capsule pointer, module-state, …). Mirrors
    /// do not use this today but the slot keeps parity with the legacy
    /// box so shared accessors are uniform.
    pub user_data: *mut c_void,
    /// Optional destructor, run before the block is freed.
    pub destructor: Option<unsafe extern "C" fn(*mut PyObject)>,
    /// Total bytes of the body allocation (`PREFIX_SIZE + body`), for
    /// [`dealloc`].
    pub alloc_size: usize,
    /// Out-of-line buffer owned by this mirror (a list's `ob_item`
    /// array), or null.
    pub aux_ptr: *mut u8,
    /// Byte length of [`aux_ptr`]'s allocation.
    pub aux_size: usize,
    /// A small magic so debugging tools (and assertions) can recognise
    /// a mirror prefix.
    pub magic: u64,
}

/// Sentinel stamped into every [`MirrorPrefix::magic`].
pub const MIRROR_MAGIC: u64 = 0x5742_504d_5252_5230; // "WBPMRR0"

/// Body alignment. 16 is ≥ the alignment of every faithful struct
/// (`f64`, pointers, `Py_complex`) and keeps SIMD-friendly buffers sane.
const BODY_ALIGN: usize = 16;

/// Bytes reserved for the prefix, rounded so the body that follows is
/// [`BODY_ALIGN`]-aligned.
pub const PREFIX_SIZE: usize = {
    let s = std::mem::size_of::<MirrorPrefix>();
    // round up to BODY_ALIGN
    (s + (BODY_ALIGN - 1)) & !(BODY_ALIGN - 1)
};

const _: () = {
    // The prefix must not be larger than the reserved region, and the
    // reserved region must be a multiple of the body alignment.
    assert!(std::mem::align_of::<MirrorPrefix>() <= BODY_ALIGN);
    assert!(PREFIX_SIZE.is_multiple_of(BODY_ALIGN));
    assert!(PREFIX_SIZE >= std::mem::size_of::<MirrorPrefix>());
};

/// Recover the prefix pointer from a public body pointer.
///
/// # Safety
/// `p` must be a body pointer previously returned by [`mirror_out`] /
/// [`mirror_out_with_type`] (i.e. [`is_mirror`] is true).
#[inline]
pub unsafe fn prefix_of(p: *mut PyObject) -> *mut MirrorPrefix {
    unsafe { (p as *mut u8).sub(PREFIX_SIZE) as *mut MirrorPrefix }
}

/// True if `p` is a faithful mirror (as opposed to a legacy
/// `PyObjectBox` or a static singleton/type). Decided by the object's
/// type: every value of a faithful built-in type is minted as a mirror,
/// and (RFC 0045, wave 3) every instance of an inline-storage extension
/// type is minted as a faithful instance body — so the type pointer is a
/// sound, deref-free discriminator for both.
///
/// # Safety
/// `p` must be non-null and point at a valid object head (`ob_type`
/// readable). Callers must have already excluded the static singletons
/// and static type objects (which are not mirrors).
#[inline]
pub unsafe fn is_mirror(p: *mut PyObject) -> bool {
    if p.is_null() {
        return false;
    }
    let ty = unsafe { (*p).ob_type };
    type_is_faithful(ty) || types::is_inline_instance_type(ty)
}

/// The set of built-in types whose instances are minted as faithful
/// mirrors. Mirrors `crate::types::type_for_object` for these variants.
pub fn type_is_faithful(ty: *mut PyTypeObject) -> bool {
    if ty.is_null() {
        return false;
    }
    ty == types::PyFloat_Type.as_ptr()
        || ty == types::PyLong_Type.as_ptr()
        || ty == types::PyBool_Type.as_ptr()
        || ty == types::PyComplex_Type.as_ptr()
        || ty == types::PyBytes_Type.as_ptr()
        || ty == types::PyByteArray_Type.as_ptr()
        || ty == types::PyUnicode_Type.as_ptr()
        || ty == types::PyTuple_Type.as_ptr()
        || ty == types::PyList_Type.as_ptr()
        // RFC 0046 (wave 4): `builtin_function_or_method`. WeavePy mints
        // *every* `PyCFunction` (we expose no `PyCFunction_NewEx`, and
        // `type_for_object(Builtin)` is the sole writer of this type), so a
        // type-keyed discriminator is sound: no foreign object ever carries
        // `PyCFunction_Type`.
        || ty == types::PyCFunction_Type.as_ptr()
}

/// True if a native [`Object`] is mirrored with a faithful body (rather
/// than routed through the legacy `PyObjectBox`).
pub fn obj_is_faithful(obj: &Object) -> bool {
    matches!(
        obj,
        Object::Float(_)
            | Object::Int(_)
            | Object::Long(_)
            | Object::Bool(_)
            | Object::Complex(_)
            | Object::Bytes(_)
            | Object::ByteArray(_)
            | Object::Str(_)
            | Object::Tuple(_)
            | Object::List(_)
            | Object::Builtin(_)
    )
}

/// Materialise `obj` into a faithful mirror, choosing the type pointer
/// from the value. Caller owns one reference.
pub fn mirror_out(obj: Object) -> *mut PyObject {
    let ty = types::type_for_object(&obj);
    mirror_out_with_type(obj, ty)
}

/// Materialise `obj` into a faithful mirror with an explicit type
/// pointer. Used for the tuple-staging case (`PyTuple_New` advertises
/// `PyTuple_Type` while staging a mutable `List`).
pub fn mirror_out_with_type(obj: Object, ty: *mut PyTypeObject) -> *mut PyObject {
    let plan = BodyPlan::for_object(&obj);
    let total = PREFIX_SIZE + plan.body_size;
    let layout = Layout::from_size_align(total, BODY_ALIGN).expect("mirror layout");
    let raw = unsafe { alloc(layout) };
    assert!(!raw.is_null(), "mirror allocation failed");
    unsafe { ptr::write_bytes(raw, 0, total) };

    let body = unsafe { raw.add(PREFIX_SIZE) } as *mut PyObject;

    // Allocate any out-of-line buffer (list `ob_item`) before we move
    // `obj` into the prefix, so we can still read it.
    let mut aux_ptr: *mut u8 = ptr::null_mut();
    let mut aux_size: usize = 0;
    unsafe {
        fill_body(body, ty, &obj, &plan, &mut aux_ptr, &mut aux_size);
    }

    // Head.
    unsafe {
        (*body).ob_refcnt = 1;
        (*body).ob_type = ty;
    }

    // Prefix (owns the native object).
    let pre = raw as *mut MirrorPrefix;
    unsafe {
        ptr::write(
            pre,
            MirrorPrefix {
                obj,
                inst: None,
                user_data: ptr::null_mut(),
                destructor: None,
                alloc_size: total,
                aux_ptr,
                aux_size,
                magic: MIRROR_MAGIC,
            },
        );
    }
    crate::object::register_minted(body);
    body
}

/// Allocate a faithful, zeroed **instance body** (RFC 0045, wave 3): a
/// `[MirrorPrefix | tp_basicsize (+ var-data)]` block whose body begins
/// with `PyObject_HEAD` so a stock reader pokes the extension's inline
/// fields at their declared offsets (`((MyType *)self)->field`).
///
/// `body_bytes` is the full body size (`tp_basicsize + nitems *
/// tp_itemsize`, clamped to at least `sizeof(PyObject)`); the head's
/// refcount starts at 1 and `ob_type` is `ty`. The prefix *borrows* the
/// owning instance through `weak` (no strong `Rc`, so there is no
/// ownership cycle); the instance frees the block on drop via
/// [`free_instance_body`].
pub fn alloc_instance_body(
    ty: *mut PyTypeObject,
    body_bytes: usize,
    weak: weavepy_vm::sync::Weak<weavepy_vm::types::PyInstance>,
) -> *mut PyObject {
    let body_bytes = body_bytes.max(std::mem::size_of::<PyObject>());
    let total = PREFIX_SIZE + body_bytes;
    let layout = Layout::from_size_align(total, BODY_ALIGN).expect("instance body layout");
    let raw = unsafe { alloc(layout) };
    assert!(!raw.is_null(), "instance body allocation failed");
    unsafe { ptr::write_bytes(raw, 0, total) };

    let body = unsafe { raw.add(PREFIX_SIZE) } as *mut PyObject;
    unsafe {
        (*body).ob_refcnt = 1;
        (*body).ob_type = ty;
    }
    let pre = raw as *mut MirrorPrefix;
    unsafe {
        ptr::write(
            pre,
            MirrorPrefix {
                obj: Object::None,
                inst: Some(weak),
                user_data: ptr::null_mut(),
                destructor: None,
                alloc_size: total,
                aux_ptr: ptr::null_mut(),
                aux_size: 0,
                magic: MIRROR_MAGIC,
            },
        );
    }
    crate::object::register_minted(body);
    body
}

/// True iff `p` is a faithful **instance body** (RFC 0045, wave 3) — a
/// mirror whose prefix carries the [`MIRROR_MAGIC`] sentinel *and* a
/// `Weak<PyInstance>` back-reference. Used by
/// [`crate::object::free_box`] to route a C refcount-zero through "end
/// C's borrow" rather than the immediate deallocate path, and by
/// [`crate::memory::PyObject_Free`] to *absorb* a stock `tp_dealloc`'s
/// `tp_free(self)` on a body the owning instance still owns.
///
/// The magic check is what makes this sound to call on an *arbitrary*
/// pointer (e.g. a scratch buffer handed to `PyObject_Free`): a
/// non-mirror's bytes would have to both name a registered inline type
/// at `ob_type` *and* carry the 8-byte sentinel at the prefix offset,
/// which does not happen in practice.
///
/// # Safety
/// `p` must be non-null and readable for `[prefix .. head + 8]`.
pub unsafe fn is_instance_body(p: *mut PyObject) -> bool {
    if !unsafe { is_mirror(p) } {
        return false;
    }
    let pre = unsafe { prefix_of(p) };
    unsafe { (*pre).magic == MIRROR_MAGIC && (*pre).inst.is_some() }
}

/// Free a faithful instance body's allocation (RFC 0045, wave 3). Called
/// from the `register_instance_body_free` hook when the owning native
/// instance is collected — never from the C refcount path. Drops the
/// prefix (its `Weak` back-reference) and releases the block.
///
/// # Safety
/// `p` must be an instance body ([`is_instance_body`]) that the owning
/// instance is releasing; it must not be used afterwards.
pub unsafe fn free_instance_body(p: *mut PyObject) {
    crate::object::unregister_minted(p);
    let pre = unsafe { prefix_of(p) };
    if let Some(d) = unsafe { (*pre).destructor } {
        unsafe { d(p) };
    }
    let alloc_size = unsafe { (*pre).alloc_size };
    // Drop the prefix in place (`obj` is None; the `Weak` back-reference
    // decrements the instance's weak count) before releasing the block.
    unsafe { ptr::drop_in_place(pre) };
    let layout = Layout::from_size_align(alloc_size, BODY_ALIGN).expect("instance body layout");
    unsafe { dealloc(pre as *mut u8, layout) };
}

/// Clone the native object out of a mirror without touching the C-side
/// refcount.
///
/// # Safety
/// `p` must satisfy [`is_mirror`].
pub unsafe fn native_of(p: *mut PyObject) -> Object {
    let pre = unsafe { prefix_of(p) };
    // RFC 0045 (wave 3): a faithful instance body resolves through its
    // `Weak` back-reference to the owning native instance, so every
    // crossing of the same pointer yields the *same* `PyInstance` (and
    // thus the same `__dict__`, identity, and inline body). The `Weak`
    // still upgrades here — the body is alive, so the instance is too.
    if let Some(weak) = unsafe { (*pre).inst.as_ref() } {
        return match weak.upgrade() {
            Some(inst) => Object::Instance(inst),
            None => Object::None,
        };
    }
    // RFC 0046 (wave 4): a faithful tuple's inline `ob_item` is the source
    // of truth (a stock `PyTuple_SET_ITEM` writes it directly, bypassing
    // our functions), so reconstruct from the C body rather than the
    // staged prefix object.
    if unsafe { is_faithful_tuple(p) } {
        return unsafe { read_tuple(p) };
    }
    // RFC 0046 (wave 4): likewise a faithful list's `ob_item` buffer is the
    // source of truth (a stock `PyList_SET_ITEM` writes it directly), so
    // reconstruct from the C body rather than the staged prefix object.
    if unsafe { is_faithful_list(p) } {
        return unsafe { read_list(p) };
    }
    unsafe { (*pre).obj.clone() }
}

/// True iff `p` is a faithful **tuple** mirror — a mirror whose advertised
/// type is `PyTuple_Type` and whose inline `ob_item` array holds the
/// elements (RFC 0046, wave 4). A stock extension fills such a tuple with
/// the `PyTuple_SET_ITEM` macro and reads it with `PyTuple_GET_ITEM`, both
/// of which touch the inline array directly, so the C body — not the
/// prefix's staged [`Object`] — is authoritative on every read.
///
/// # Safety
/// `p` must be non-null and readable for `[prefix .. head + 16]`.
pub unsafe fn is_faithful_tuple(p: *mut PyObject) -> bool {
    if !unsafe { is_mirror(p) } {
        return false;
    }
    let head = unsafe { &*p };
    !head.ob_type.is_null() && std::ptr::eq(head.ob_type, crate::types::PyTuple_Type.as_ptr())
}

/// Reconstruct an [`Object::Tuple`] by reading a faithful tuple mirror's
/// inline `ob_item` array (`ob_size` entries). Each non-NULL slot is
/// resolved with [`crate::object::clone_object`] so a foreign element
/// round-trips opaquely and a DType class resolves to its bridged type.
///
/// # Safety
/// `p` must satisfy [`is_faithful_tuple`].
pub unsafe fn read_tuple(p: *mut PyObject) -> Object {
    let vo = p as *const layout::PyVarObject;
    let n = unsafe { (*vo).ob_size };
    let n = if n < 0 { 0 } else { n as usize };
    let to = p as *const layout::PyTupleObject;
    let base = ptr::addr_of!((*to).ob_item) as *const *mut PyObject;
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let slot = unsafe { *base.add(i) };
        out.push(if slot.is_null() {
            Object::None
        } else {
            unsafe { crate::object::clone_object(slot) }
        });
    }
    if std::env::var_os("WEAVEPY_DEBUG_TUPLE").is_some() && n == 2 {
        let s0 = unsafe { *base.add(0) };
        let s1 = unsafe { *base.add(1) };
        let k1 = match out.get(1) {
            Some(Object::Foreign(_)) => "Foreign",
            Some(Object::None) => "None",
            Some(Object::Type(_)) => "Type",
            Some(Object::Tuple(_)) => "Tuple",
            Some(_) => "other",
            None => "MISSING",
        };
        eprintln!("[read_tuple n=2] slot0={s0:p} slot1={s1:p} out1_kind={k1}");
    }
    Object::new_tuple(out)
}

/// True iff `p` is a faithful **list** mirror — a mirror whose advertised
/// type is `PyList_Type` and whose `ob_item` buffer holds the elements
/// (RFC 0046, wave 4). A stock extension fills such a list with the
/// `PyList_SET_ITEM` macro (numpy builds `__cpu_dispatch__` this way:
/// `PyList_New(n)` then `PyList_SET_ITEM(list, i, str)`), which writes the
/// `ob_item` array directly — so the C body, not the prefix's staged
/// [`Object`], is authoritative on every read-back.
///
/// # Safety
/// `p` must be non-null and readable for `[prefix .. head + 16]`.
pub unsafe fn is_faithful_list(p: *mut PyObject) -> bool {
    if !unsafe { is_mirror(p) } {
        return false;
    }
    let head = unsafe { &*p };
    !head.ob_type.is_null() && std::ptr::eq(head.ob_type, crate::types::PyList_Type.as_ptr())
}

/// Reconstruct an [`Object::List`] by reading a faithful list mirror's
/// `ob_item` buffer (`ob_size` entries). Each non-NULL slot is resolved
/// with [`crate::object::clone_object`]; a NULL slot (a `PyList_New(n)`
/// placeholder a stock extension never filled) reads as `None`, matching
/// CPython, where such a slot is the `NULL` that `PyList_SET_ITEM` expects
/// to overwrite.
///
/// # Safety
/// `p` must satisfy [`is_faithful_list`].
pub unsafe fn read_list(p: *mut PyObject) -> Object {
    let vo = p as *const layout::PyVarObject;
    let n = unsafe { (*vo).ob_size };
    let n = if n < 0 { 0 } else { n as usize };
    let lo = p as *const layout::PyListObject;
    let base = unsafe { (*lo).ob_item };
    let mut out = Vec::with_capacity(n);
    if !base.is_null() {
        for i in 0..n {
            let slot = unsafe { *base.add(i) };
            out.push(if slot.is_null() {
                Object::None
            } else {
                unsafe { crate::object::clone_object(slot) }
            });
        }
    }
    Object::new_list(out)
}

/// Borrow the `pos`-th inline `ob_item` slot of a faithful tuple mirror
/// (RFC 0046, wave 4). Returns a *borrowed* pointer (no incref), matching
/// `PyTuple_GetItem`'s contract; `None` for an out-of-range index.
///
/// # Safety
/// `p` must satisfy [`is_faithful_tuple`].
pub unsafe fn tuple_slot(p: *mut PyObject, pos: PySsizeT) -> Option<*mut PyObject> {
    let vo = p as *const layout::PyVarObject;
    let n = unsafe { (*vo).ob_size };
    if pos < 0 || pos >= n {
        return None;
    }
    let to = p as *const layout::PyTupleObject;
    let base = ptr::addr_of!((*to).ob_item) as *const *mut PyObject;
    Some(unsafe { *base.add(pos as usize) })
}

/// Overwrite the `pos`-th inline `ob_item` slot of a faithful tuple mirror,
/// stealing `item` (CPython's `PyTuple_SetItem` semantics) and releasing
/// the slot's previous occupant. Returns `false` for an out-of-range index
/// (the caller then disposes of `item`).
///
/// # Safety
/// `p` must satisfy [`is_faithful_tuple`]; `item` is a strong reference
/// whose ownership transfers to the tuple.
pub unsafe fn tuple_store(p: *mut PyObject, pos: PySsizeT, item: *mut PyObject) -> bool {
    let vo = p as *const layout::PyVarObject;
    let n = unsafe { (*vo).ob_size };
    if pos < 0 || pos >= n {
        return false;
    }
    let to = p as *mut layout::PyTupleObject;
    let base = ptr::addr_of_mut!((*to).ob_item) as *mut *mut PyObject;
    let slot = unsafe { base.add(pos as usize) };
    let prev = unsafe { *slot };
    unsafe { *slot = item };
    if !prev.is_null() {
        unsafe { crate::object::Py_DecRef(prev) };
    }
    true
}

/// Number of live elements in a faithful list mirror (its `ob_size`).
///
/// # Safety
/// `p` must satisfy [`is_faithful_list`].
pub unsafe fn list_size(p: *mut PyObject) -> PySsizeT {
    let vo = p as *const layout::PyVarObject;
    unsafe { (*vo).ob_size }.max(0)
}

/// Borrow the `pos`-th `ob_item` slot of a faithful list mirror (no
/// incref, matching `PyList_GetItem`); `None` for an out-of-range index.
///
/// # Safety
/// `p` must satisfy [`is_faithful_list`].
pub unsafe fn list_slot(p: *mut PyObject, pos: PySsizeT) -> Option<*mut PyObject> {
    let n = unsafe { list_size(p) };
    if pos < 0 || pos >= n {
        return None;
    }
    let lo = p as *const layout::PyListObject;
    let base = unsafe { (*lo).ob_item };
    if base.is_null() {
        return None;
    }
    Some(unsafe { *base.add(pos as usize) })
}

/// Ensure the faithful list `p` can hold at least `min_cap` slots,
/// (re)allocating its out-of-line `ob_item` buffer and syncing both the
/// `PyListObject` (`ob_item` / `allocated`) and the mirror prefix's aux
/// tracking (`aux_ptr` / `aux_size`, which [`free_mirror`] uses to
/// release the buffer and decref its occupants). New slots are NULL.
/// Returns the (possibly new) base pointer.
///
/// # Safety
/// `p` must satisfy [`is_faithful_list`].
unsafe fn list_reserve(p: *mut PyObject, min_cap: usize) -> *mut *mut PyObject {
    let lo = p as *mut layout::PyListObject;
    let cur_alloc = unsafe { (*lo).allocated }.max(0) as usize;
    let cur_base = unsafe { (*lo).ob_item };
    if min_cap <= cur_alloc && !cur_base.is_null() {
        return cur_base;
    }
    // CPython-style over-allocation (`list_resize`) keeps amortised O(1)
    // append: grow to `min_cap + (min_cap >> 3) + 6`, never below double
    // the current capacity.
    let grow = min_cap + (min_cap >> 3) + 6;
    let new_cap = grow.max(cur_alloc.saturating_mul(2)).max(4);
    let new_bytes = new_cap * std::mem::size_of::<*mut PyObject>();
    let layout = Layout::from_size_align(new_bytes, BODY_ALIGN).expect("ob_item layout");
    let new_buf = unsafe { alloc(layout) } as *mut *mut PyObject;
    assert!(!new_buf.is_null(), "ob_item allocation failed");
    unsafe { ptr::write_bytes(new_buf as *mut u8, 0, new_bytes) };
    let n = unsafe { list_size(p) } as usize;
    if !cur_base.is_null() {
        for i in 0..n {
            unsafe { *new_buf.add(i) = *cur_base.add(i) };
        }
    }
    let pre = unsafe { prefix_of(p) };
    let old_aux = unsafe { (*pre).aux_ptr };
    let old_aux_size = unsafe { (*pre).aux_size };
    if !old_aux.is_null() && old_aux_size > 0 {
        let old_layout = Layout::from_size_align(old_aux_size, BODY_ALIGN).expect("aux layout");
        unsafe { dealloc(old_aux, old_layout) };
    }
    unsafe {
        (*lo).ob_item = new_buf;
        (*lo).allocated = new_cap as PySsizeT;
        (*pre).aux_ptr = new_buf as *mut u8;
        (*pre).aux_size = new_bytes;
    }
    new_buf
}

/// Append `item` to a faithful list mirror, taking a new strong
/// reference (CPython `PyList_Append` semantics — the caller keeps its
/// own reference). Writes the inline `ob_item` buffer, the source of
/// truth for every read-back.
///
/// # Safety
/// `p` must satisfy [`is_faithful_list`]; `item` must be a live,
/// non-null `PyObject*`.
pub unsafe fn list_append(p: *mut PyObject, item: *mut PyObject) {
    let n = unsafe { list_size(p) } as usize;
    let base = unsafe { list_reserve(p, n + 1) };
    unsafe { crate::object::Py_IncRef(item) };
    unsafe { *base.add(n) = item };
    let vo = p as *mut layout::PyVarObject;
    unsafe { (*vo).ob_size = (n + 1) as PySsizeT };
}

/// Insert `item` before `pos` (clamped to `[0, len]`) in a faithful list
/// mirror, taking a new strong reference.
///
/// # Safety
/// `p` must satisfy [`is_faithful_list`]; `item` must be a live,
/// non-null `PyObject*`.
pub unsafe fn list_insert(p: *mut PyObject, pos: PySsizeT, item: *mut PyObject) {
    let n = unsafe { list_size(p) } as usize;
    let base = unsafe { list_reserve(p, n + 1) };
    let at = pos.clamp(0, n as PySsizeT) as usize;
    for i in (at..n).rev() {
        unsafe { *base.add(i + 1) = *base.add(i) };
    }
    unsafe { crate::object::Py_IncRef(item) };
    unsafe { *base.add(at) = item };
    let vo = p as *mut layout::PyVarObject;
    unsafe { (*vo).ob_size = (n + 1) as PySsizeT };
}

/// Overwrite the `pos`-th slot of a faithful list mirror, **stealing**
/// `item` (CPython `PyList_SetItem`) and releasing the prior occupant.
/// Returns `false` for an out-of-range index (the caller then disposes
/// of `item`).
///
/// # Safety
/// `p` must satisfy [`is_faithful_list`]; `item` is a strong reference
/// whose ownership transfers to the list.
pub unsafe fn list_store(p: *mut PyObject, pos: PySsizeT, item: *mut PyObject) -> bool {
    let n = unsafe { list_size(p) };
    if pos < 0 || pos >= n {
        return false;
    }
    let lo = p as *mut layout::PyListObject;
    let base = unsafe { (*lo).ob_item };
    let slot = unsafe { base.add(pos as usize) };
    let prev = unsafe { *slot };
    unsafe { *slot = item };
    if !prev.is_null() {
        unsafe { crate::object::Py_DecRef(prev) };
    }
    true
}

/// Snapshot the `ob_item` pointers of a faithful list mirror (borrowed;
/// no refcount change). Used by in-place permutations (reverse / sort).
///
/// # Safety
/// `p` must satisfy [`is_faithful_list`].
pub unsafe fn list_ptrs(p: *mut PyObject) -> Vec<*mut PyObject> {
    let n = unsafe { list_size(p) } as usize;
    let lo = p as *const layout::PyListObject;
    let base = unsafe { (*lo).ob_item };
    let mut out = Vec::with_capacity(n);
    if !base.is_null() {
        for i in 0..n {
            out.push(unsafe { *base.add(i) });
        }
    }
    out
}

/// Write back a permutation of the list's own pointers (same multiset,
/// same length — a pure reordering, so no refcount change). Used by
/// reverse / sort after [`list_ptrs`].
///
/// # Safety
/// `p` must satisfy [`is_faithful_list`] and `ptrs.len() == list_size(p)`.
pub unsafe fn list_permute(p: *mut PyObject, ptrs: &[*mut PyObject]) {
    let lo = p as *mut layout::PyListObject;
    let base = unsafe { (*lo).ob_item };
    if base.is_null() {
        return;
    }
    for (i, &pp) in ptrs.iter().enumerate() {
        unsafe { *base.add(i) = pp };
    }
}

/// Borrow the C-side state pointer stored in the prefix.
///
/// # Safety
/// `p` must satisfy [`is_mirror`].
pub unsafe fn user_data(p: *mut PyObject) -> *mut c_void {
    let pre = unsafe { prefix_of(p) };
    unsafe { (*pre).user_data }
}

/// Free a mirror: run its destructor, drop the owning native object and
/// any out-of-line buffer, then release the block.
///
/// # Safety
/// `p` must satisfy [`is_mirror`] and have a zero (or about-to-be-zero)
/// refcount; it must not be used afterwards.
pub unsafe fn free_mirror(p: *mut PyObject) {
    crate::object::unregister_minted(p);
    let pre = unsafe { prefix_of(p) };
    let destructor = unsafe { (*pre).destructor };
    if let Some(d) = destructor {
        unsafe { d(p) };
    }
    let alloc_size = unsafe { (*pre).alloc_size };
    let aux_ptr = unsafe { (*pre).aux_ptr };
    let aux_size = unsafe { (*pre).aux_size };

    // A non-null aux buffer is a list's out-of-line `ob_item` (RFC 0046,
    // wave 4); the list owns one reference to each element (including any
    // a stock `PyList_SET_ITEM` stored directly), so release them before
    // freeing the buffer. Immortal singletons (None/bool) no-op.
    if !aux_ptr.is_null() && aux_size > 0 {
        let n = (aux_size / std::mem::size_of::<*mut PyObject>()) as isize;
        let slots = aux_ptr as *mut *mut PyObject;
        for i in 0..n {
            let elem = unsafe { *slots.offset(i) };
            if !elem.is_null() {
                unsafe { crate::object::Py_DecRef(elem) };
            }
        }
    }

    // RFC 0046 (wave 4): a faithful tuple owns one reference to each inline
    // `ob_item` element (materialised on creation or stored by a stock
    // `PyTuple_SET_ITEM`), so release them before the block goes away.
    // Immortal singletons (None/bool placeholders) no-op.
    if unsafe { is_faithful_tuple(p) } {
        let vo = p as *const layout::PyVarObject;
        let n = unsafe { (*vo).ob_size };
        if n > 0 {
            let to = p as *mut layout::PyTupleObject;
            let base = ptr::addr_of_mut!((*to).ob_item) as *mut *mut PyObject;
            for i in 0..n as usize {
                let elem = unsafe { *base.add(i) };
                if !elem.is_null() {
                    unsafe { crate::object::Py_DecRef(elem) };
                }
            }
        }
    }

    // Drop the owning native object (releasing its Rc clones).
    unsafe { ptr::drop_in_place(ptr::addr_of_mut!((*pre).obj)) };

    if !aux_ptr.is_null() && aux_size > 0 {
        let aux_layout = Layout::from_size_align(aux_size, BODY_ALIGN).expect("aux layout");
        unsafe { dealloc(aux_ptr, aux_layout) };
    }

    let layout = Layout::from_size_align(alloc_size, BODY_ALIGN).expect("mirror layout");
    unsafe { dealloc(pre as *mut u8, layout) };
}

// ---------------------------------------------------------------------------
// Body layout planning + filling.
// ---------------------------------------------------------------------------

/// What kind of faithful body a value gets, and how big it is.
struct BodyPlan {
    kind: BodyKind,
    /// Size in bytes of the body (head + faithful tail). Always ≥ 16.
    body_size: usize,
}

#[derive(Clone, Copy)]
enum BodyKind {
    Float,
    Long,
    Complex,
    Bytes,
    Str,
    Tuple,
    /// Faithful `PyListObject` with an out-of-line `ob_item` buffer
    /// (RFC 0046, wave 4). numpy builds module lists by `PyList_New(n)`
    /// then writing `ob_item[i]` directly (the `PyList_SET_ITEM` macro),
    /// so the buffer must be a real, writable `PyObject*` array.
    List,
    /// Faithful `PyCFunctionObject` with an inline, writable `PyMethodDef`
    /// (RFC 0046, wave 4). numpy's `add_docstring` walks
    /// `((PyCFunctionObject *)f)->m_ml->ml_doc` directly to read and then
    /// write a function's docstring, so `m_ml` must point at a real,
    /// writable `PyMethodDef` (carried just past the object body).
    CFunction,
    /// Head-only body; the native value lives only in the prefix.
    Generic,
}

impl BodyPlan {
    fn for_object(obj: &Object) -> BodyPlan {
        match obj {
            Object::Float(_) => BodyPlan {
                kind: BodyKind::Float,
                body_size: std::mem::size_of::<layout::PyFloatObject>(),
            },
            Object::Complex(_) => BodyPlan {
                kind: BodyKind::Complex,
                body_size: std::mem::size_of::<layout::PyComplexObject>(),
            },
            Object::Int(_) | Object::Long(_) => {
                let ndigits = long_digit_count(obj).max(1);
                // head(16) + lv_tag(8) + ndigits * 4, rounded to 8.
                let raw = 16 + 8 + ndigits * 4;
                BodyPlan {
                    kind: BodyKind::Long,
                    body_size: round_up(raw, 8),
                }
            }
            Object::Bytes(b) => BodyPlan {
                kind: BodyKind::Bytes,
                // varhead(24) + ob_shash(8) + (len+1) NUL-terminated.
                body_size: round_up(24 + 8 + b.len() + 1, 8),
            },
            Object::Str(s) if is_ascii_or_latin1(s) => BodyPlan {
                kind: BodyKind::Str,
                // PyASCIIObject(40) + (len+1) bytes of 1-byte chars.
                body_size: round_up(40 + s.chars().count() + 1, 8),
            },
            Object::Tuple(t) => BodyPlan {
                kind: BodyKind::Tuple,
                // varhead(24) + n pointers.
                body_size: round_up(24 + t.len() * 8, 8).max(24),
            },
            Object::List(_) => BodyPlan {
                kind: BodyKind::List,
                // The list's `ob_item` is out-of-line (a separate aux
                // buffer); the body is exactly `PyListObject`.
                body_size: std::mem::size_of::<layout::PyListObject>(),
            },
            Object::Builtin(_) => BodyPlan {
                kind: BodyKind::CFunction,
                // `PyCFunctionObject` followed by an inline `PyMethodDef`
                // (pointed at by `m_ml`); both live in the one block so a
                // stock `f->m_ml->ml_doc` read/write stays in bounds and the
                // method def is released with the object.
                body_size: std::mem::size_of::<layout::PyCFunctionObject>()
                    + std::mem::size_of::<layout::PyMethodDef>(),
            },
            _ => BodyPlan {
                kind: BodyKind::Generic,
                body_size: std::mem::size_of::<PyObject>(),
            },
        }
    }
}

/// Fill the faithful fields of `body` from `obj`. The head is written by
/// the caller afterward (so `fill_body` must not depend on it).
///
/// # Safety
/// `body` points at a zeroed block of at least `plan.body_size` bytes.
unsafe fn fill_body(
    body: *mut PyObject,
    _ty: *mut PyTypeObject,
    obj: &Object,
    plan: &BodyPlan,
    aux_ptr: &mut *mut u8,
    aux_size: &mut usize,
) {
    match plan.kind {
        BodyKind::Float => {
            if let Object::Float(f) = obj {
                let fo = body as *mut layout::PyFloatObject;
                unsafe { (*fo).ob_fval = *f };
            }
        }
        BodyKind::Complex => {
            if let Object::Complex(c) = obj {
                let co = body as *mut layout::PyComplexObject;
                unsafe {
                    (*co).cval = layout::PyComplexValue {
                        real: c.real,
                        imag: c.imag,
                    };
                }
            }
        }
        BodyKind::Long => unsafe { fill_long(body, obj) },
        BodyKind::Bytes => {
            if let Object::Bytes(b) = obj {
                let vo = body as *mut layout::PyVarObject;
                unsafe { (*vo).ob_size = b.len() as PySsizeT };
                let bo = body as *mut layout::PyBytesObject;
                unsafe {
                    (*bo).ob_shash = -1;
                    let dst = ptr::addr_of_mut!((*bo).ob_sval) as *mut u8;
                    ptr::copy_nonoverlapping(b.as_ptr(), dst, b.len());
                    *dst.add(b.len()) = 0; // NUL terminator
                }
            }
        }
        BodyKind::Str => unsafe { fill_str(body, obj) },
        BodyKind::Tuple => {
            if let Object::Tuple(t) = obj {
                let vo = body as *mut layout::PyVarObject;
                unsafe { (*vo).ob_size = t.len() as PySsizeT };
                let to = body as *mut layout::PyTupleObject;
                let base = ptr::addr_of_mut!((*to).ob_item) as *mut *mut PyObject;
                for (i, elem) in t.iter().enumerate() {
                    // RFC 0046 (wave 4): the inline `ob_item` array is the
                    // tuple's *source of truth* — a stock `PyTuple_GET_ITEM`
                    // reads it directly and `PyTuple_SET_ITEM` writes it, so
                    // each element is an owned reference materialised here
                    // (and released in `free_mirror`). `into_owned` round-
                    // trips a foreign proxy to its original pointer and a
                    // type object to its own `PyTypeObject*`. None/bool reuse
                    // their immortal singletons so a `PyTuple_SET_ITEM`
                    // overwrite (which does not decref the prior slot) of a
                    // staged placeholder cannot leak.
                    let ep = match elem {
                        Object::None => crate::singletons::none_ptr(),
                        Object::Bool(true) => crate::singletons::true_ptr(),
                        Object::Bool(false) => crate::singletons::false_ptr(),
                        _ => crate::object::into_owned(elem.clone()),
                    };
                    if std::env::var_os("WEAVEPY_DEBUG_TUPLE").is_some() && t.len() == 2 {
                        let k = match elem {
                            Object::Foreign(_) => "Foreign",
                            Object::None => "None",
                            Object::Type(_) => "Type",
                            Object::Tuple(_) => "Tuple",
                            _ => "other",
                        };
                        eprintln!("[fill_body tuple n=2] i={i} kind={k} ep={ep:p}");
                    }
                    unsafe { *base.add(i) = ep };
                }
            }
        }
        BodyKind::List => {
            if let Object::List(l) = obj {
                let items = l.borrow();
                let n = items.len();
                let vo = body as *mut layout::PyVarObject;
                unsafe { (*vo).ob_size = n as PySsizeT };
                let lo = body as *mut layout::PyListObject;
                if n == 0 {
                    // CPython's empty list has `ob_item == NULL`.
                    unsafe {
                        (*lo).ob_item = ptr::null_mut();
                        (*lo).allocated = 0;
                    }
                } else {
                    let bytes = n * std::mem::size_of::<*mut PyObject>();
                    let buf_layout =
                        Layout::from_size_align(bytes, BODY_ALIGN).expect("ob_item layout");
                    let buf = unsafe { alloc(buf_layout) };
                    assert!(!buf.is_null(), "ob_item allocation failed");
                    unsafe { ptr::write_bytes(buf, 0, bytes) };
                    let slots = buf as *mut *mut PyObject;
                    for (i, elem) in items.iter().enumerate() {
                        // Each element is materialised as an owned reference
                        // held by the list. None/bool reuse their immortal
                        // singletons so a stock `PyList_SET_ITEM` overwrite
                        // (which does *not* decref the prior slot) of a
                        // `PyList_New(n)` placeholder cannot leak.
                        let ep = match elem {
                            Object::None => crate::singletons::none_ptr(),
                            Object::Bool(true) => crate::singletons::true_ptr(),
                            Object::Bool(false) => crate::singletons::false_ptr(),
                            _ => crate::object::into_owned(elem.clone()),
                        };
                        unsafe { *slots.add(i) = ep };
                    }
                    unsafe {
                        (*lo).ob_item = slots;
                        (*lo).allocated = n as PySsizeT;
                    }
                    *aux_ptr = buf;
                    *aux_size = bytes;
                }
            }
        }
        BodyKind::CFunction => {
            // Lay a faithful `PyCFunctionObject` over the body and point its
            // `m_ml` at the inline `PyMethodDef` that follows. The def is
            // left zeroed (`ml_doc == NULL`), so numpy's `add_docstring`
            // takes the "first docstring" branch and *writes* `ml_doc` in
            // place rather than `strcmp`-ing a garbage pointer. `m_self` /
            // `m_module` / `vectorcall` stay NULL — calls and `__module__`
            // are served by the VM through the prefix, never through these
            // fields. `ml_name` is NULL for the same reason (`f.__name__`
            // resolves in the VM); it is read by `add_docstring` only on the
            // never-taken mismatch path.
            let cf = body as *mut layout::PyCFunctionObject;
            let def =
                unsafe { (body as *mut u8).add(std::mem::size_of::<layout::PyCFunctionObject>()) }
                    as *mut layout::PyMethodDef;
            unsafe {
                (*cf).m_ml = def;
                (*cf).m_self = ptr::null_mut();
                (*cf).m_module = ptr::null_mut();
                (*cf).m_weakreflist = ptr::null_mut();
                (*cf).vectorcall = ptr::null_mut();
            }
            let _ = (aux_ptr, aux_size);
        }
        BodyKind::Generic => {
            // Head-only: nothing to fill. Suppress "unused" on a list's
            // would-be aux buffer.
            let _ = (aux_ptr, aux_size);
        }
    }
}

/// Encode an integer's faithful `PyLongObject` body.
unsafe fn fill_long(body: *mut PyObject, obj: &Object) {
    let (sign, mag) = int_sign_magnitude(obj);
    let digits = to_base_2_30(mag);
    let ndigits = digits.len().max(1);
    let lo = body as *mut layout::PyLongObject;
    let sign_field = if sign == 0 {
        layout::PYLONG_SIGN_ZERO
    } else if sign < 0 {
        layout::PYLONG_SIGN_NEGATIVE
    } else {
        layout::PYLONG_SIGN_POSITIVE
    };
    unsafe {
        (*lo).long_value.lv_tag = (ndigits << layout::PYLONG_NON_SIZE_BITS) | sign_field;
        let base = ptr::addr_of_mut!((*lo).long_value.ob_digit) as *mut layout::digit;
        if digits.is_empty() {
            *base = 0;
        } else {
            for (i, d) in digits.iter().enumerate() {
                *base.add(i) = *d;
            }
        }
    }
}

/// Fill a compact 1-byte (ASCII or Latin-1) unicode body.
unsafe fn fill_str(body: *mut PyObject, obj: &Object) {
    let Object::Str(s) = obj else { return };
    let is_ascii = s.is_ascii();
    let chars: Vec<u8> = s.chars().map(|c| c as u8).collect(); // latin-1 guaranteed by planner
    let n = chars.len();
    let ao = body as *mut layout::PyASCIIObject;
    unsafe {
        (*ao).length = n as PySsizeT;
        (*ao).hash = -1;
        (*ao).state = ustate::pack(
            0, // not interned
            ustate::KIND_1BYTE,
            true,     // compact
            is_ascii, // ascii
            false,    // not statically allocated
        );
        // Compact-ASCII data follows the PyASCIIObject inline.
        let data = (body as *mut u8).add(std::mem::size_of::<layout::PyASCIIObject>());
        ptr::copy_nonoverlapping(chars.as_ptr(), data, n);
        *data.add(n) = 0;
    }
}

// ---------------------------------------------------------------------------
// Integer helpers.
// ---------------------------------------------------------------------------

fn long_digit_count(obj: &Object) -> usize {
    let (_, mag) = int_sign_magnitude(obj);
    to_base_2_30(mag).len()
}

/// Returns `(sign, magnitude)` where `sign ∈ {-1, 0, 1}`.
fn int_sign_magnitude(obj: &Object) -> (i32, u128) {
    match obj {
        Object::Int(v) => {
            if *v == 0 {
                (0, 0)
            } else if *v < 0 {
                (-1, (*v as i128).unsigned_abs())
            } else {
                (1, *v as u128)
            }
        }
        Object::Bool(b) => {
            if *b {
                (1, 1)
            } else {
                (0, 0)
            }
        }
        Object::Long(big) => big_sign_magnitude(big),
        _ => (0, 0),
    }
}

/// Big integers wider than `u128` are clamped to their low 128 bits for
/// the faithful body; WeavePy itself always reads the exact value from
/// the prefix, and stock extensions read big ints through the function
/// API (`PyLong_AsLong`), so the inlined-digit path matters only for
/// values that fit. (Full-width digit encoding is a wave-2 refinement.)
fn big_sign_magnitude(big: &BigInt) -> (i32, u128) {
    use num_bigint::Sign;
    let (sign, bytes) = big.to_bytes_le();
    let mut mag: u128 = 0;
    for (i, b) in bytes.iter().take(16).enumerate() {
        mag |= (*b as u128) << (i * 8);
    }
    let s = match sign {
        Sign::NoSign => 0,
        Sign::Plus => 1,
        Sign::Minus => -1,
    };
    (s, mag)
}

/// Decompose a magnitude into base-2^30 little-endian limbs.
fn to_base_2_30(mut mag: u128) -> Vec<layout::digit> {
    let mut out = Vec::new();
    if mag == 0 {
        return out;
    }
    while mag > 0 {
        out.push((mag & (layout::PYLONG_MASK as u128)) as layout::digit);
        mag >>= layout::PYLONG_SHIFT;
    }
    out
}

fn is_ascii_or_latin1(s: &str) -> bool {
    s.chars().all(|c| (c as u32) <= 0xFF)
}

const fn round_up(n: usize, align: usize) -> usize {
    (n + (align - 1)) & !(align - 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interp::ensure_initialised;
    use weavepy_vm::sync::Rc as VmRc;

    /// Read a `T` at byte offset `off` from a body pointer, the way a
    /// stock inlined macro would.
    unsafe fn read_at<T: Copy>(p: *mut PyObject, off: usize) -> T {
        unsafe { ptr::read_unaligned((p as *const u8).add(off) as *const T) }
    }

    fn as_float(o: &Object) -> f64 {
        match o {
            Object::Float(f) => *f,
            _ => panic!("expected float"),
        }
    }
    fn as_int(o: &Object) -> i64 {
        match o {
            Object::Int(v) => *v,
            _ => panic!("expected int"),
        }
    }

    #[test]
    fn float_body_is_faithful() {
        ensure_initialised();
        let p = mirror_out(Object::Float(2.5));
        unsafe {
            assert!(is_mirror(p));
            // ob_fval lives at offset 16 (where PyFloat_AS_DOUBLE reads).
            assert_eq!(read_at::<f64>(p, 16), 2.5);
            // refcount starts at 1, type is float.
            assert_eq!((*p).ob_refcnt, 1);
            assert_eq!((*p).ob_type, types::PyFloat_Type.as_ptr());
            // The native object resolves back.
            assert_eq!(as_float(&native_of(p)), 2.5);
            free_mirror(p);
        }
    }

    #[test]
    fn long_body_encodes_small_int() {
        ensure_initialised();
        let p = mirror_out(Object::Int(5));
        unsafe {
            // lv_tag at +16: ndigits=1, sign positive → (1<<3)|0 = 8.
            assert_eq!(read_at::<usize>(p, 16), 8);
            // first digit at +24 == 5.
            assert_eq!(read_at::<u32>(p, 24), 5);
            assert_eq!(as_int(&native_of(p)), 5);
            free_mirror(p);
        }
    }

    #[test]
    fn long_body_encodes_negative() {
        ensure_initialised();
        let p = mirror_out(Object::Int(-1));
        unsafe {
            // sign negative = 2, ndigits 1 → (1<<3)|2 = 10.
            assert_eq!(read_at::<usize>(p, 16), 10);
            assert_eq!(read_at::<u32>(p, 24), 1);
            free_mirror(p);
        }
    }

    #[test]
    fn bytes_body_is_faithful() {
        ensure_initialised();
        let p = mirror_out(Object::Bytes(VmRc::from(&b"hi"[..])));
        unsafe {
            // ob_size at +16.
            assert_eq!(read_at::<isize>(p, 16), 2);
            // ob_sval at +32 holds the bytes + NUL.
            assert_eq!(read_at::<u8>(p, 32), b'h');
            assert_eq!(read_at::<u8>(p, 33), b'i');
            assert_eq!(read_at::<u8>(p, 34), 0);
            free_mirror(p);
        }
    }

    #[test]
    fn str_ascii_body_is_faithful() {
        ensure_initialised();
        let p = mirror_out(Object::Str(VmRc::from("abc")));
        unsafe {
            // length at +16.
            assert_eq!(read_at::<isize>(p, 16), 3);
            // state at +32: kind=1byte, compact, ascii.
            let state = read_at::<u32>(p, 32);
            assert_eq!(
                state,
                ustate::pack(0, ustate::KIND_1BYTE, true, true, false)
            );
            // compact data follows PyASCIIObject (offset 40).
            assert_eq!(read_at::<u8>(p, 40), b'a');
            assert_eq!(read_at::<u8>(p, 42), b'c');
            free_mirror(p);
        }
    }

    #[test]
    fn tuple_body_holds_element_mirrors() {
        ensure_initialised();
        let t = Object::new_tuple(vec![Object::Float(1.0), Object::Int(2)]);
        let p = mirror_out(t);
        unsafe {
            // ob_size at +16.
            assert_eq!(read_at::<isize>(p, 16), 2);
            // ob_item[0] at +24 is a float mirror with ob_fval 1.0.
            let e0 = read_at::<*mut PyObject>(p, 24);
            assert_eq!(read_at::<f64>(e0, 16), 1.0);
            free_mirror(p);
        }
    }

    #[test]
    fn generic_body_keeps_native_in_prefix() {
        ensure_initialised();
        // A dict is not a faithful body; it gets a generic head-only body
        // but still resolves through the prefix.
        let p = mirror_out(Object::Float(9.0));
        unsafe {
            assert!(is_mirror(p));
            free_mirror(p);
        }
    }
}

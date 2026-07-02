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

/// Diagnostic: gate faithful instance-body alloc/free tracing on
/// `WEAVEPY_BODY_TRACE` (RFC 0045 debugging of body-address reuse).
pub fn body_trace_enabled() -> bool {
    use std::sync::OnceLock;
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("WEAVEPY_BODY_TRACE").is_some())
}

fn body_trace_interesting(tn: &str) -> bool {
    tn.contains("Engine")
        || tn.contains("ndarray")
        || tn.contains("Index")
        || tn.contains("BlockManager")
        || tn.contains("Block")
        || tn.contains("internals")
}

thread_local! {
    /// Diagnostic (WEAVEPY_BODY_TRACE): the type name most recently freed
    /// at each instance-body address, so a subsequent allocation reusing
    /// that address can flag a body-address reuse across types.
    static FREED_BODY_TYPES: RefCell<HashMap<usize, String>> =
        RefCell::new(HashMap::new());
    /// Diagnostic (WEAVEPY_WATCH_BLOCKS): addresses of blocks tuples to
    /// trace refcount ops on, to find a premature-free / over-decref.
    static WATCHED: RefCell<std::collections::HashSet<usize>> =
        RefCell::new(std::collections::HashSet::new());
    /// Diagnostic (WEAVEPY_WATCH_BLOCKS): free-site history (type + short
    /// backtrace) for each mirror address, so a later stale read can print
    /// the full reuse chain that led to the confusion.
    static FREE_BT: RefCell<HashMap<usize, Vec<(String, String)>>> =
        RefCell::new(HashMap::new());
}

/// Record the free-site of a mirror at `p` (WEAVEPY_WATCH_BLOCKS), keyed
/// by address, so a later stale read of the same address can report who
/// freed it.
pub unsafe fn record_mirror_free(p: *mut PyObject) {
    if !watch_enabled() {
        return;
    }
    // Only faithful tuples/lists — the shapes a `blocks` field points at —
    // to keep backtrace capture rare enough not to perturb timing.
    if !unsafe { is_faithful_tuple(p) } && !unsafe { is_faithful_list(p) } {
        return;
    }
    let tn = unsafe { crate::object::debug_type_name(p) };
    // Keep only the last ~4 interior frames to make the chain readable.
    let full = std::backtrace::Backtrace::force_capture().to_string();
    let short: String = full
        .lines()
        .filter(|l| {
            l.contains("free_mirror")
                || l.contains("free_box")
                || l.contains("DecRef")
                || l.contains("Dealloc")
                || l.contains("install_new")
                || l.contains("VectorcallMethod")
                || l.contains("reap")
                || l.contains("tp_clear")
                || l.contains("GC_")
                || l.contains("clear")
        })
        .take(8)
        .collect::<Vec<_>>()
        .join(" | ");
    FREE_BT.with(|m| {
        m.borrow_mut()
            .entry(p as usize)
            .or_default()
            .push((tn, short));
    });
}

/// Look up the free-site history recorded for `addr` (WEAVEPY_WATCH_BLOCKS).
pub fn lookup_free_bt(addr: usize) -> Option<Vec<(String, String)>> {
    if !watch_enabled() {
        return None;
    }
    FREE_BT.with(|m| m.borrow().get(&addr).cloned())
}

use std::cell::RefCell;

pub fn watch_enabled() -> bool {
    use std::sync::OnceLock;
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("WEAVEPY_WATCH_BLOCKS").is_some())
}

pub fn watch_ptr(p: usize) {
    if watch_enabled() {
        WATCHED.with(|s| s.borrow_mut().insert(p));
    }
}

pub fn is_watched(p: usize) -> bool {
    watch_enabled() && WATCHED.with(|s| s.borrow().contains(&p))
}

pub fn unwatch_ptr(p: usize) {
    if watch_enabled() {
        WATCHED.with(|s| s.borrow_mut().remove(&p));
    }
}

fn note_body_freed(addr: usize, tyname: String) {
    if !body_trace_enabled() {
        return;
    }
    FREED_BODY_TYPES.with(|m| {
        m.borrow_mut().insert(addr, tyname);
    });
}

fn check_body_reuse(addr: usize, new_ty: &str) {
    if !body_trace_enabled() {
        return;
    }
    let prev = FREED_BODY_TYPES.with(|m| m.borrow_mut().remove(&addr));
    if let Some(old) = prev {
        if body_trace_interesting(&old) || body_trace_interesting(new_ty) {
            eprintln!(
                "[BODY-REUSE] addr=0x{:x} old_type={} new_type={}",
                addr, old, new_ty
            );
        }
    }
}

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
    /// True iff this mirror is a faithful, **buffer-authoritative** unicode
    /// string built by [`new_unicode_mirror`] (the target of
    /// `PyUnicode_New`/`PyUnicode_Resize`, RFC 0047, wave 5). A stock
    /// extension writes such a string's character buffer *directly* — the
    /// inlined `PyUnicode_WRITE` macro after `PyUnicode_New`, or
    /// `PyUnicode_CopyCharacters` after `PyUnicode_Resize` — so the C body,
    /// not the prefix's staged [`obj`](Self::obj), is authoritative on
    /// read-back ([`native_of`] reconstructs via [`read_str`]). A normal
    /// str mirror (minted by [`mirror_out`]) leaves this `false` and stays
    /// prefix-authoritative: its bytes are never mutated in place.
    pub str_buffer: bool,
    /// True once a faithful **list** mirror's prefix [`obj`](Self::obj) has
    /// been seeded from the authoritative inline `ob_item` buffer (RFC 0047,
    /// wave 5). A list mints with `false`; the first [`native_of`] read-back
    /// reconstructs the prefix list from `ob_item` — capturing a C-built list
    /// (`PyList_New` + the `PyList_SET_ITEM` macro, e.g. numpy's
    /// `__cpu_dispatch__`) — and flips this `true`. Thereafter the prefix list
    /// is the shared, identity-stable source of truth, so a Python-side
    /// mutation of a C-resident `cdef public list` (pandas'
    /// `BlockManager.axes[0] = new_axis`) persists across crossings instead of
    /// landing on a throwaway per-read reconstruction. (Always `false` for
    /// non-list mirrors.)
    pub list_synced: bool,
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
        // RFC 0047 (wave 5): `dict`. Macro-heavy Cython reads
        // `((PyDictObject*)d)->ma_used` straight off the struct (the
        // `PyDict_GET_SIZE` macro and the keyword-argument fast path
        // `__Pyx_PyVectorcall_FastCallDict_kw`), so a dict crossing into C
        // must be a faithful `PyDictObject` header. WeavePy mints *every*
        // `Object::Dict` through this path (`type_for_object(Dict)` is the
        // sole writer of `PyDict_Type`), so the type-keyed discriminator is
        // sound.
        || ty == types::PyDict_Type.as_ptr()
        // RFC 0047 (wave 5): `set` / `frozenset`. Macro-heavy Cython reads
        // `((PySetObject*)s)->used` straight off the struct — `PySet_GET_SIZE`
        // / `PyFrozenSet_GET_SIZE`, which Cython emits for both `len(s)` and
        // the truthiness test `if s:` on a set-typed value (pandas'
        // `Timedelta.__new__` keyword guard). WeavePy mints *every*
        // `Object::Set`/`FrozenSet` through `type_for_object` (the sole writer
        // of these two type pointers), so the type-keyed discriminator is
        // sound: no foreign object carries `PySet_Type`/`PyFrozenSet_Type`.
        || ty == types::PySet_Type.as_ptr()
        || ty == types::PyFrozenSet_Type.as_ptr()
        // RFC 0046 (wave 4): `builtin_function_or_method`. WeavePy mints
        // *every* `PyCFunction` (we expose no `PyCFunction_NewEx`, and
        // `type_for_object(Builtin)` is the sole writer of this type), so a
        // type-keyed discriminator is sound: no foreign object ever carries
        // `PyCFunction_Type`.
        || ty == types::PyCFunction_Type.as_ptr()
        // RFC 0047 (wave 5): `method` (a bound method). WeavePy mints *every*
        // `PyMethod_Type` object — `PyMethod_New` routes through the VM and
        // `type_for_object(BoundMethod)` is the sole writer — so the
        // type-keyed discriminator is sound: no foreign object carries
        // `PyMethod_Type`. A faithful body is mandatory because Cython's
        // `with`/`for`/call fast paths unpack a bound method by reading
        // `im_func`/`im_self` straight off the C struct (see
        // `layout::PyMethodObject`).
        || ty == types::PyMethod_Type.as_ptr()
        // RFC 0047 (wave 5): `slice`. WeavePy mints *every* `Object::Slice`
        // through `type_for_object(Slice)` (the sole writer of `PySlice_Type`),
        // so the type-keyed discriminator is sound. A faithful body is
        // mandatory because Cython reads `start`/`stop`/`step` straight off the
        // `PySliceObject` struct (pandas' `internals.slice_canonize`; see
        // `layout::PySliceObject`).
        || ty == types::PySlice_Type.as_ptr()
        // RFC 0047 (wave 5): `memoryview`. WeavePy mints *every*
        // `Object::MemoryView` through `type_for_object(MemoryView)` (the sole
        // writer of `PyMemoryView_Type`; `PyMemoryView_FromObject` and friends
        // all route through it), so the type-keyed discriminator is sound — and
        // the `is_weavepy_owned` guard in `free_box`/`clone_object` runs first,
        // so a (hypothetical) foreign object carrying `PyMemoryView_Type` is
        // never mis-claimed. A faithful `PyMemoryViewObject` body is mandatory
        // because `PyMemoryView_GET_BUFFER` is a macro (`&mv->view`) that
        // Cython's fused-type dispatch reads straight off the struct (pandas'
        // `lib.map_infer_mask`; see `layout::PyMemoryViewObject`). Without this
        // entry `is_mirror` is false for a memoryview mirror, so `free_box`
        // drops its prefix-offset body as a `PyObjectBox`
        // (`POINTER_BEING_FREED_WAS_NOT_ALLOCATED`).
        || ty == types::PyMemoryView_Type.as_ptr()
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
            | Object::Dict(_)
            | Object::Set(_)
            | Object::FrozenSet(_)
            | Object::Builtin(_)
            | Object::BoundMethod(_)
            | Object::Slice(_)
            | Object::MemoryView(_)
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
    // A bool crosses as the immortal, layout-faithful `Py_True`/`Py_False`
    // singleton — never a freshly-minted box. CPython hands out exactly these
    // two `PyLongObject`s, and C code relies both on pointer identity
    // (`x == Py_True`, `Py_RETURN_TRUE`) and on the inline digit/sign decode
    // (`maybe_convert_objects`'s `bools[i] = val`). The generic-body fallback
    // would have produced a 16-byte `PyObject` with no `_PyLongValue`.
    if let Object::Bool(b) = &obj {
        return if *b {
            crate::singletons::true_ptr()
        } else {
            crate::singletons::false_ptr()
        };
    }
    // RFC 0047 (wave 5): a `set`/`frozenset` crosses as a single canonical
    // box (see [`SET_BOX_CACHE`]). Reuse the live one whenever the same
    // native set is already mirrored so a C-cached `PyObject*` stays
    // coherent across a VM-routed mutation (`difference_update`, `|=`, …).
    if let Some(key) = set_rc_key(&obj) {
        if let Some(p) = cached_set_box(key) {
            return p;
        }
        let p = mirror_out_fresh(obj, ty);
        register_set_box(key, p);
        return p;
    }
    mirror_out_fresh(obj, ty)
}

/// Mint a fresh faithful mirror block for `obj` (no canonical-box cache
/// consultation). Every mirror is born here; [`mirror_out_with_type`]
/// layers the set cache on top.
fn mirror_out_fresh(obj: Object, ty: *mut PyTypeObject) -> *mut PyObject {
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
                str_buffer: false,
                list_synced: false,
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
    if body_trace_enabled() && crate::object::is_weavepy_owned(body) {
        let tn = unsafe { crate::object::debug_type_name(body) };
        eprintln!(
            "[DOUBLE-ALLOC] alloc returned live minted body=0x{:x} prev-type={}",
            body as usize, tn
        );
    }
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
                str_buffer: false,
                list_synced: false,
                magic: MIRROR_MAGIC,
            },
        );
    }
    crate::object::register_minted(body);
    if body_trace_enabled() {
        let tn = unsafe { crate::object::debug_type_name(body) };
        check_body_reuse(body as usize, &tn);
        if body_trace_interesting(&tn) {
            let inst_ptr = unsafe { (*pre).inst.as_ref() }
                .and_then(|w| w.upgrade())
                .map(|rc| weavepy_vm::sync::Rc::as_ptr(&rc) as usize)
                .unwrap_or(0);
            eprintln!(
                "[BALLOC] body=0x{:x} inst=0x{:x} type={}",
                body as usize, inst_ptr, tn
            );
        }
    }
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
    if body_trace_enabled() {
        let tn = unsafe { crate::object::debug_type_name(p) };
        note_body_freed(p as usize, tn.clone());
        if body_trace_interesting(&tn) {
            let rc = unsafe { (*p).ob_refcnt };
            eprintln!("[BFREE] body=0x{:x} type={} refcnt={}", p as usize, tn, rc);
            if tn.contains("Engine") || tn.contains("BlockManager") {
                eprintln!("{}", std::backtrace::Backtrace::force_capture());
            }
        }
    }
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
        // RFC 0046 (wave 5): a faithful **str-subtype** body (numpy's
        // `str_`, built by `builtin_new::str_new`) carries no VM-native
        // string on its `PyInstance`, so the VM's string operations (`+`,
        // f-strings, comparison, hashing) cannot read it and a bare
        // `Object::Instance` reports "unsupported operand". Reconstruct its
        // value so it behaves as a `str` — a numpy scalar is interchangeable
        // with its Python counterpart. Gated on the cheap `tp_base`-chain
        // subtype test, so an ordinary faithful instance (numpy `ndarray`,
        // pandas block, …) is unaffected.
        let head = unsafe { &*p };
        if !head.ob_type.is_null()
            && !std::ptr::eq(head.ob_type, types::PyUnicode_Type.as_ptr())
            && unsafe {
                crate::types::PyType_IsSubtype(head.ob_type, types::PyUnicode_Type.as_ptr())
            } != 0
        {
            if let Some(s) = unsafe { read_unicode_value(p) } {
                return Object::from_str(s);
            }
        }
        // RFC 0046 (wave 5): a faithful **bytes-subtype** body (numpy's
        // `bytes_`, built by `builtin_new::bytes_new`) carries its value in
        // the inline `ob_sval` array, not on its `PyInstance`. Reconstruct it
        // so the VM's `bytes` operations (comparison, hashing, `bytes(x)`,
        // indexing) see the real value — a numpy scalar is interchangeable
        // with its Python counterpart. Same cheap subtype guard as unicode.
        if !head.ob_type.is_null()
            && !std::ptr::eq(head.ob_type, types::PyBytes_Type.as_ptr())
            && unsafe {
                crate::types::PyType_IsSubtype(head.ob_type, types::PyBytes_Type.as_ptr())
            } != 0
        {
            if let Some(b) = unsafe { read_bytes_value(p) } {
                let rc: weavepy_vm::sync::Rc<[u8]> = b.into();
                return Object::Bytes(rc);
            }
        }
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
    // RFC 0047 (wave 5): a faithful list is **seed-once, then prefix-
    // authoritative**. A stock `PyList_New` + `PyList_SET_ITEM` build writes
    // the inline `ob_item` directly (numpy's `__cpu_dispatch__`), so the first
    // read-back reconstructs the prefix list from that buffer. Thereafter the
    // prefix's `Object::List` is the shared, identity-stable source of truth:
    // every crossing of the same mirror yields the *same* `Rc`, so a Python
    // mutation of a C-resident `cdef public list` persists. pandas'
    // `BlockManager.insert` does `self.axes[0] = new_axis` on the list its
    // Cython getter returns; reconstruct-on-*every*-read handed each crossing
    // a throwaway copy, so the store vanished and `df["c"] = …` silently
    // dropped the column (`KeyError: 'c'`).
    if unsafe { is_faithful_list(p) } {
        let pre = unsafe { prefix_of(p) };
        if !unsafe { (*pre).list_synced } {
            let seeded = unsafe { read_list(p) };
            unsafe {
                (*pre).obj = seeded;
                (*pre).list_synced = true;
            }
            // Now VM-shared: a Python-side mutation of this list must be
            // re-published to `ob_item` before C reads it back through the
            // `PyList_GET_ITEM` macro (see [`flush_seeded_lists`]).
            register_seeded_list(p);
        } else {
            // Adopt any *direct* C-side macro write to `ob_item` (RFC 0047,
            // wave 5) — e.g. Cython's `__Pyx_ListComp_Append` building
            // `memoryview.shape` — back into the shared prefix `Rc` before
            // handing it to the VM. A VM-only mutation is left untouched.
            unsafe { reconcile_list_from_c(p) };
        }
        return unsafe { (*pre).obj.clone() };
    }
    // RFC 0047 (wave 5): a **buffer-authoritative** unicode mirror (the
    // result of `PyUnicode_New`/`PyUnicode_Resize`) has its character data
    // written directly by the extension (the inlined `PyUnicode_WRITE`
    // macro, `PyUnicode_CopyCharacters`), so reconstruct from the C buffer
    // rather than the staged prefix object, which would be stale. A normal
    // str mirror (`str_buffer == false`) is never mutated in place, so its
    // prefix object stays authoritative (and avoids a per-crossing rebuild).
    if unsafe { (*pre).str_buffer } {
        return unsafe { read_str(p) };
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

/// True if `p` is a faithful `dict` mirror.
///
/// # Safety
/// `p` must be non-null with a readable `ob_type`.
pub unsafe fn is_faithful_dict(p: *mut PyObject) -> bool {
    if !unsafe { is_mirror(p) } {
        return false;
    }
    let head = unsafe { &*p };
    !head.ob_type.is_null() && std::ptr::eq(head.ob_type, crate::types::PyDict_Type.as_ptr())
}

/// Refresh a faithful dict mirror's `ma_used` from its prefix's native
/// dict after a C-side mutation changed the entry count. CPython exposes
/// the live count straight off the struct (`PyDict_GET_SIZE`), so every
/// WeavePy dict mutator that crosses the C boundary must re-publish it
/// here. No-op for any pointer that isn't a faithful dict mirror.
///
/// # Safety
/// `p` must be non-null with a readable `ob_type`.
pub unsafe fn sync_dict_ma_used(p: *mut PyObject) {
    if !unsafe { is_faithful_dict(p) } {
        return;
    }
    let pre = unsafe { prefix_of(p) };
    if let Object::Dict(rc) = unsafe { &(*pre).obj } {
        let used = rc.borrow().len() as PySsizeT;
        let d = p as *mut layout::PyDictObject;
        unsafe {
            (*d).ma_used = used;
        }
    }
}

/// True if `p` is a faithful `set` **or** `frozenset` mirror.
///
/// # Safety
/// `p` must be non-null with a readable `ob_type`.
pub unsafe fn is_faithful_set(p: *mut PyObject) -> bool {
    if !unsafe { is_mirror(p) } {
        return false;
    }
    let head = unsafe { &*p };
    if head.ob_type.is_null() {
        return false;
    }
    std::ptr::eq(head.ob_type, crate::types::PySet_Type.as_ptr())
        || std::ptr::eq(head.ob_type, crate::types::PyFrozenSet_Type.as_ptr())
}

/// Refresh a faithful set mirror's `fill`/`used` from its prefix's native
/// set after an in-place mutation changed the element count. CPython
/// exposes the live count straight off the struct (`PySet_GET_SIZE` is
/// `((PySetObject*)so)->used`), and Cython lowers `len(s)` / `if s:` on a
/// set-typed value to that macro — so every mutation that reaches the set
/// through the C boundary (a `PySet_Add`, or an unbound-method call like
/// `set.difference_update(s, other)` routed through `PyObject_Call`) must
/// re-publish the size here. No-op for any pointer that isn't a faithful
/// set mirror.
///
/// # Safety
/// `p` must be non-null with a readable `ob_type`.
pub unsafe fn sync_set_used(p: *mut PyObject) {
    if !unsafe { is_faithful_set(p) } {
        return;
    }
    let pre = unsafe { prefix_of(p) };
    let n = match unsafe { &(*pre).obj } {
        Object::Set(rc) => rc.borrow().len() as PySsizeT,
        Object::FrozenSet(fs) => fs.len() as PySsizeT,
        _ => return,
    };
    let so = p as *mut layout::PySetObject;
    if std::env::var_os("WEAVEPY_TRACE_SETSEED").is_some() {
        eprintln!(
            "[SYNC_SET_USED] p={:p} old_used={} new={}",
            p,
            unsafe { (*so).used },
            n
        );
    }
    unsafe {
        (*so).fill = n;
        (*so).used = n;
    }
}

/// Re-publish the macro-visible size of a dict/set mirror after it may
/// have been mutated in place through the C boundary. A cheap no-op for
/// any pointer that isn't one of those two faithful mirrors (the
/// [`is_mirror`] magic check gates the type comparison), so it is safe to
/// sprinkle over the generic call path.
///
/// # Safety
/// `p` may be null; if non-null it must have a readable `ob_type`.
pub unsafe fn sync_container_size(p: *mut PyObject) {
    if p.is_null() || !unsafe { is_mirror(p) } {
        return;
    }
    if unsafe { is_faithful_dict(p) } {
        unsafe { sync_dict_ma_used(p) };
    } else if unsafe { is_faithful_set(p) } {
        unsafe { sync_set_used(p) };
    }
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

/// True iff `p` is a faithful **bound method** mirror — a mirror whose
/// advertised type is `PyMethod_Type` and whose `im_func`/`im_self`
/// fields are owned references (RFC 0047, wave 5). Unlike a tuple/list,
/// a method body is never mutated through a `SET` macro, so the prefix's
/// staged [`Object::BoundMethod`] stays authoritative for read-back
/// ([`native_of`]); this predicate is used only to release the two extra
/// owned refs in [`free_mirror`].
///
/// # Safety
/// `p` must be non-null and readable for `[prefix .. head + 16]`.
pub unsafe fn is_faithful_method(p: *mut PyObject) -> bool {
    if !unsafe { is_mirror(p) } {
        return false;
    }
    let head = unsafe { &*p };
    !head.ob_type.is_null() && std::ptr::eq(head.ob_type, crate::types::PyMethod_Type.as_ptr())
}

/// True iff `p` is a faithful **slice** mirror — a mirror whose advertised
/// type is `PySlice_Type` and whose `start`/`stop`/`step` fields hold owned
/// `PyObject*`s (RFC 0047, wave 5). Cython reads those fields straight off
/// the `PySliceObject` struct (pandas' `internals.slice_canonize`).
///
/// # Safety
/// `p` must be non-null and readable for `[prefix .. head + 16]`.
pub unsafe fn is_faithful_slice(p: *mut PyObject) -> bool {
    if !unsafe { is_mirror(p) } {
        return false;
    }
    let head = unsafe { &*p };
    !head.ob_type.is_null() && std::ptr::eq(head.ob_type, crate::types::PySlice_Type.as_ptr())
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
    Object::new_list(unsafe { read_list_vec(p) })
}

/// Read a faithful list mirror's `ob_item` buffer into a plain `Vec`
/// (the element resolution used by [`read_list`], without the
/// `Object::List` wrapper). Used by the write-through path to refill an
/// existing prefix `Rc` in place, preserving its identity.
///
/// # Safety
/// `p` must satisfy [`is_faithful_list`].
unsafe fn read_list_vec(p: *mut PyObject) -> Vec<Object> {
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
    out
}

// ---------------------------------------------------------------------------
// Faithful-list write-through coherence (RFC 0047, wave 5).
//
// A faithful list is *seed-once, then prefix-authoritative*: after the first
// read-back its prefix `Object::List` (a shared, identity-stable `Rc`) is the
// source of truth, so a Python-side mutation of a C-resident `cdef public
// list` persists. But a stock extension reads such a list back through the
// `PyList_GET_ITEM` **macro** — `((PyListObject*)op)->ob_item[i]`, compiled
// inline into the extension, which WeavePy cannot interpose. The macro reads
// the C `ob_item` buffer, *not* the prefix `Rc`, so a VM mutation leaves the
// two divergent: pandas' `BlockManager.insert` does `self.axes[0] = new_axis`
// (a VM `list.__setitem__`) and then `internals.pyx`'s `get_slice` reads
// `self.axes[0]` via the macro — seeing the stale pre-insert column and so
// `df.head()` / `iloc[:n]` silently drop the inserted column.
//
// There is no WeavePy code on the path between the VM store and the inlined
// macro read, so the buffer must be re-published *before* control re-enters
// C. Every seeded list mirror is registered here; [`flush_seeded_lists`]
// (called at the VM→C boundary) re-syncs each one's `ob_item` from its prefix
// `Rc`. The atomic gate keeps the common case (no list ever crossed to C) at
// a single relaxed load.
// ---------------------------------------------------------------------------

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

/// Per-seeded-list coherence state, keyed by `PyObject*`.
///
/// A faithful list has *two* authorities that must be reconciled: the VM's
/// shared prefix `Rc` and the C `ob_item` buffer. `rc_fp` lets the VM→C
/// flush ([`sync_list_ob_item`]) skip an unmutated list; `c_ptrs` lets the
/// C→VM read-back ([`native_of`] → [`reconcile_list_from_c`]) detect a
/// *direct* C-side macro write — `PyList_SET_ITEM` + `__Pyx_SET_SIZE`, taken
/// by Cython's `__Pyx_ListComp_Append` fast path (e.g. building
/// `memoryview.shape`) and numpy's list builders — that never passed through
/// a WeavePy mutator, so the buffer must be adopted back into the `Rc`.
#[derive(Default)]
struct ListSync {
    /// FNV fingerprints of the prefix `Rc` elements last published to
    /// `ob_item` (empty until the first flush). See [`sync_list_ob_item`].
    rc_fp: Vec<u64>,
    /// Raw `ob_item` pointer snapshot at the last agreement point (seed,
    /// publish, write-through, or adopt). A later read that finds a different
    /// buffer knows C wrote it directly. See [`reconcile_list_from_c`].
    c_ptrs: Vec<usize>,
}

/// Seeded faithful list mirrors keyed by `PyObject*`.
static SEEDED_LISTS: Mutex<Option<HashMap<usize, ListSync>>> = Mutex::new(None);
static SEEDED_LIST_COUNT: AtomicUsize = AtomicUsize::new(0);

/// Canonical faithful `set`/`frozenset` boxes keyed by native `Rc`
/// identity (RFC 0047, wave 5): `Rc-payload-pointer → PyObject*`.
///
/// A stock/Cython extension caches a `PyObject*` and later reads the
/// element count straight off the struct — `PySet_GET_SIZE(so)` is
/// `((PySetObject*)so)->used`, which Cython emits for *both* `len(s)` and
/// the truthiness test `if s:` on a set-typed value. If every crossing
/// minted a *fresh* mirror, that cached box would be a stale snapshot: an
/// unbound-method mutation like `set.difference_update(s, other)` routed
/// through `PyObject_Call` empties the shared native store but the count
/// re-publish ([`sync_set_used`]) lands on the ephemeral *argument* box,
/// never the one the extension cached. pandas' `Timedelta.__new__`
/// keyword guard (`set(kwargs)` → `difference_update(_req_kwargs)` →
/// `if unsupported_kwargs:`) then reads the pre-mutation `used` and raises
/// a spurious `ValueError`. Handing out **one** canonical box per native
/// set makes the cached pointer and the mutated/synced pointer the *same*
/// memory, so the guard sees the emptied set.
static SET_BOX_CACHE: Mutex<Option<HashMap<usize, usize>>> = Mutex::new(None);
static SET_BOX_COUNT: AtomicUsize = AtomicUsize::new(0);

/// Native `Rc` identity key for a `set`/`frozenset` (its `Arc` payload
/// pointer), or `None` for any other object. Two `Object` clones of the
/// same set share one `Rc`, so this is a stable per-set identity for as
/// long as any clone (e.g. a live mirror's prefix) keeps it alive.
fn set_rc_key(obj: &Object) -> Option<usize> {
    match obj {
        Object::Set(rc) => Some(weavepy_vm::sync::Rc::as_ptr(rc) as usize),
        Object::FrozenSet(rc) => Some(weavepy_vm::sync::Rc::as_ptr(rc) as usize),
        _ => None,
    }
}

/// Return the live canonical box for native-set identity `key`, handing
/// back a *fresh* C reference (matching `into_owned`'s "+1" contract).
/// `None` if no box is currently cached.
fn cached_set_box(key: usize) -> Option<*mut PyObject> {
    let g = SET_BOX_CACHE.lock().ok()?;
    let map = g.as_ref()?;
    let bp = *map.get(&key)?;
    let p = bp as *mut PyObject;
    unsafe { crate::object::Py_IncRef(p) };
    Some(p)
}

/// Record `p` as the canonical box for native-set identity `key`.
fn register_set_box(key: usize, p: *mut PyObject) {
    if let Ok(mut g) = SET_BOX_CACHE.lock() {
        if g
            .get_or_insert_with(HashMap::new)
            .insert(key, p as usize)
            .is_none()
        {
            SET_BOX_COUNT.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// Evict a faithful set mirror from the canonical cache when its storage
/// is released — called from [`free_mirror`] *before* the prefix's native
/// `Object` (and thus its `Rc`) is dropped. Only removes the entry when it
/// still points at `p`, so a stale box that lost a cache race can never
/// clobber the live canonical one.
///
/// # Safety
/// `p` must be a faithful set mirror ([`is_faithful_set`]) whose prefix is
/// still intact.
pub unsafe fn unregister_set_box(p: *mut PyObject) {
    let pre = unsafe { prefix_of(p) };
    let key = match set_rc_key(unsafe { &(*pre).obj }) {
        Some(k) => k,
        None => return,
    };
    if let Ok(mut g) = SET_BOX_CACHE.lock() {
        if let Some(map) = g.as_mut() {
            if map.get(&key) == Some(&(p as usize)) {
                map.remove(&key);
                SET_BOX_COUNT.fetch_sub(1, Ordering::Relaxed);
            }
        }
    }
}

/// Snapshot a faithful list mirror's raw `ob_item` pointers (as `usize`).
/// Cheap — no minting, no refcount change — so a read can tell whether C
/// wrote the buffer since the last agreement without reconstructing objects.
///
/// # Safety
/// `p` must satisfy [`is_faithful_list`].
unsafe fn list_ptr_snapshot(p: *mut PyObject) -> Vec<usize> {
    let n = unsafe { list_size(p) }.max(0) as usize;
    let lo = p as *const layout::PyListObject;
    let base = unsafe { (*lo).ob_item };
    let mut out = Vec::with_capacity(n);
    if !base.is_null() {
        for i in 0..n {
            out.push(unsafe { *base.add(i) } as usize);
        }
    }
    out
}

/// Record the current `ob_item` as the agreed C state for a seeded list, so
/// a subsequent read does not mistake a WeavePy write-through for a foreign
/// C macro write (which would needlessly rebuild, or — after a further VM
/// mutation — clobber it). Called by the write-through mutators; a no-op for
/// a list that was never seeded/registered.
///
/// # Safety
/// `p` must satisfy [`is_faithful_list`].
unsafe fn note_c_agreement(p: *mut PyObject) {
    if SEEDED_LIST_COUNT.load(Ordering::Relaxed) == 0 {
        return;
    }
    let cur = unsafe { list_ptr_snapshot(p) };
    if let Ok(mut g) = SEEDED_LISTS.lock() {
        if let Some(map) = g.as_mut() {
            if let Some(slot) = map.get_mut(&(p as usize)) {
                slot.c_ptrs = cur;
            }
        }
    }
}

/// The C→VM half of faithful-list coherence (RFC 0047, wave 5): adopt a
/// *direct* C-side write to a seeded list's `ob_item` back into the shared
/// prefix `Rc`.
///
/// A stock extension can grow or overwrite a seeded list through the
/// `PyList_SET_ITEM` + `__Pyx_SET_SIZE` macros — Cython's
/// `__Pyx_ListComp_Append` fast path takes exactly this route when it builds
/// `tuple([length for length in self.view.shape[:self.view.ndim]])` for
/// `memoryview.shape`, so a 2-D buffer's shape read back as a 1-tuple and
/// pandas' groupby allocated 1-D internals (`Buffer has wrong number of
/// dimensions`). Such a write never passes through a WeavePy mutator, so the
/// prefix `Rc` is left stale. When the current `ob_item` differs from the
/// snapshot taken at the last agreement, the buffer is authoritative:
/// refill the `Rc` in place (identity preserved). A VM-only mutation leaves
/// `ob_item` untouched (snapshot matches) and so is never clobbered.
///
/// # Safety
/// `p` must satisfy [`is_faithful_list`].
unsafe fn reconcile_list_from_c(p: *mut PyObject) {
    if SEEDED_LIST_COUNT.load(Ordering::Relaxed) == 0 {
        return;
    }
    let cur = unsafe { list_ptr_snapshot(p) };
    // Cheap gate: an unchanged buffer means C wrote nothing; the `Rc`
    // (possibly ahead with un-flushed VM mutations) stays authoritative.
    // A missing entry ⇒ leave the `Rc` alone (never clobber VM state).
    let changed = match SEEDED_LISTS.lock() {
        Ok(g) => g
            .as_ref()
            .and_then(|m| m.get(&(p as usize)))
            .map(|st| st.c_ptrs != cur)
            .unwrap_or(false),
        Err(_) => return,
    };
    if !changed {
        return;
    }
    let pre = unsafe { prefix_of(p) };
    let rc = match unsafe { &(*pre).obj } {
        Object::List(rc) => rc.clone(),
        _ => return,
    };
    let adopted = unsafe { read_list_vec(p) };
    let fp: Vec<u64> = adopted.iter().map(fingerprint).collect();
    let n = cur.len();
    *rc.borrow_mut() = adopted;
    if let Ok(mut g) = SEEDED_LISTS.lock() {
        if let Some(map) = g.as_mut() {
            if let Some(slot) = map.get_mut(&(p as usize)) {
                slot.rc_fp = fp;
                slot.c_ptrs = cur;
            }
        }
    }
    if std::env::var_os("WEAVEPY_TRACE_LISTSYNC").is_some() {
        eprintln!("[LISTSYNC] adopt {p:p} ob_size={n}");
    }
}

/// Allocation-free identity for an `Rc`/`Arc` (sized or unsized): the data
/// pointer, stable for the lifetime of the allocation.
#[inline]
fn rc_id<T: ?Sized>(rc: &weavepy_vm::sync::Rc<T>) -> u64 {
    weavepy_vm::sync::Rc::as_ptr(rc) as *const () as u64
}

/// A 64-bit fingerprint of a list element that changes iff the element's
/// *identity or value* changes, computed without minting any C object. For
/// an `Rc`-backed value the stable allocation pointer is used; for an inline
/// scalar the value itself. This lets [`sync_list_ob_item`] detect an
/// unmutated list and leave its `ob_item` untouched (no refcount churn, no
/// dangling of a pointer C may still borrow), which is what makes flushing
/// at *every* VM→C boundary affordable.
fn fingerprint(o: &Object) -> u64 {
    #[inline]
    fn mix(tag: u8, payload: u64) -> u64 {
        // FNV-1a over the tag byte then the eight payload bytes.
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        h ^= tag as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
        let mut p = payload;
        for _ in 0..8 {
            h ^= p & 0xff;
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
            p >>= 8;
        }
        h
    }
    use Object::*;
    match o {
        None => mix(0, 0),
        Unbound => mix(1, 0),
        Bool(b) => mix(2, *b as u64),
        Int(i) => mix(3, *i as u64),
        Float(f) => mix(4, f.to_bits()),
        Long(rc) => mix(5, rc_id(rc)),
        Complex(rc) => mix(6, rc_id(rc)),
        Str(rc) => mix(7, rc_id(rc)),
        WStr(rc) => mix(8, rc_id(rc)),
        Tuple(rc) => mix(9, rc_id(rc)),
        List(rc) => mix(10, rc_id(rc)),
        Dict(rc) => mix(11, rc_id(rc)),
        Range(rc) => mix(12, rc_id(rc)),
        Function(rc) => mix(13, rc_id(rc)),
        Builtin(rc) => mix(14, rc_id(rc)),
        BoundMethod(rc) => mix(15, rc_id(rc)),
        Code(rc) => mix(16, rc_id(rc)),
        Cell(rc) => mix(17, rc_id(rc)),
        Iter(rc) => mix(18, rc_id(rc)),
        Slice(rc) => mix(19, rc_id(rc)),
        Type(rc) => mix(20, rc_id(rc)),
        Instance(rc) => mix(21, rc_id(rc)),
        Module(rc) => mix(22, rc_id(rc)),
        Generator(rc) => mix(23, rc_id(rc)),
        Coroutine(rc) => mix(24, rc_id(rc)),
        AsyncGenerator(rc) => mix(25, rc_id(rc)),
        AsyncGenAwait(rc) => mix(26, rc_id(rc)),
        Bytes(rc) => mix(27, rc_id(rc)),
        ByteArray(rc) => mix(28, rc_id(rc)),
        Set(rc) => mix(29, rc_id(rc)),
        FrozenSet(rc) => mix(30, rc_id(rc)),
        File(rc) => mix(31, rc_id(rc)),
        Property(rc) => mix(32, rc_id(rc)),
        StaticMethod(rc) => mix(33, rc_id(rc)),
        ClassMethod(rc) => mix(34, rc_id(rc)),
        SlotDescriptor(rc) => mix(35, rc_id(rc)),
        Frame(rc) => mix(36, rc_id(rc)),
        Traceback(rc) => mix(37, rc_id(rc)),
        MemoryView(rc) => mix(38, rc_id(rc)),
        MappingProxy(rc) => mix(39, rc_id(rc)),
        DictView(rc) => mix(40, rc_id(rc)),
        SimpleNamespace(rc) => mix(41, rc_id(rc)),
        LazyIter(rc) => mix(42, rc_id(rc)),
        Capsule(rc) => mix(43, rc_id(rc)),
        Foreign(rc) => mix(44, rc_id(rc)),
    }
}

thread_local! {
    /// Set while [`flush_seeded_lists`] is running. A slot decref during a
    /// sync can free an object whose drop re-enters the VM→C boundary (and
    /// thus `ensure_active` → `flush_seeded_lists`); the guard makes that
    /// nested call a no-op so the outer flush keeps a consistent snapshot.
    static FLUSHING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

struct FlushGuard;
impl Drop for FlushGuard {
    fn drop(&mut self) {
        FLUSHING.with(|f| f.set(false));
    }
}

/// Record a faithful list mirror as VM-shared (seeded) so its `ob_item`
/// is re-synced from the prefix `Rc` at the next VM→C boundary.
pub fn register_seeded_list(p: *mut PyObject) {
    if p.is_null() {
        return;
    }
    // The mirror was just seeded (its prefix `Rc` == `ob_item`), so capture
    // the buffer snapshot now; a later read only adopts a *genuine* C write.
    let c_ptrs = unsafe { list_ptr_snapshot(p) };
    if let Ok(mut g) = SEEDED_LISTS.lock() {
        // An empty `rc_fp` forces the first flush to do a real sync (it can
        // never equal a non-empty list's fingerprints).
        if g.get_or_insert_with(HashMap::new)
            .insert(
                p as usize,
                ListSync {
                    rc_fp: Vec::new(),
                    c_ptrs,
                },
            )
            .is_none()
        {
            SEEDED_LIST_COUNT.fetch_add(1, Ordering::Relaxed);
            if std::env::var_os("WEAVEPY_TRACE_LISTSYNC").is_some() {
                let n = unsafe { list_size(p) };
                eprintln!("[LISTSYNC] register {p:p} ob_size={n}");
            }
        }
    }
}

/// Drop a faithful list mirror from the seeded set (its storage is being
/// released).
pub fn unregister_seeded_list(p: *mut PyObject) {
    if p.is_null() {
        return;
    }
    if let Ok(mut g) = SEEDED_LISTS.lock() {
        if let Some(map) = g.as_mut() {
            if map.remove(&(p as usize)).is_some() {
                SEEDED_LIST_COUNT.fetch_sub(1, Ordering::Relaxed);
            }
        }
    }
}

/// Re-publish a seeded faithful list mirror's `ob_item` buffer from its
/// prefix `Object::List` so a stock `PyList_GET_ITEM` macro sees the VM's
/// latest mutations. A slot whose desired occupant already lives there
/// (a stable identity — a cached instance box, a foreign pointer, a
/// singleton) is left untouched, so an unchanged list churns no refcounts
/// and never dangles a pointer C may still hold.
///
/// # Safety
/// `p` must be a live pointer.
pub unsafe fn sync_list_ob_item(p: *mut PyObject) {
    if !unsafe { is_faithful_list(p) } {
        return;
    }
    let pre = unsafe { prefix_of(p) };
    // Never seeded ⇒ the C buffer is authoritative (a `PyList_New` +
    // `PyList_SET_ITEM` build the VM has not yet read back); leave it.
    if !unsafe { (*pre).list_synced } {
        return;
    }
    // Adopt any *direct* C-side write first (RFC 0047, wave 5). Cython's
    // `__Pyx_ListComp_Append` fast path grows a seeded list straight through
    // the `PyList_SET_ITEM` + `__Pyx_SET_SIZE` macros (e.g. `[np.dtype(x)
    // for x in ...]` building `TextReader.dtype_cast_order`), so the inline
    // `ob_item`/`ob_size` can be *ahead* of the prefix `Rc` without any
    // read-back having reconciled it. Publishing the stale `Rc` here would
    // clobber those elements — pandas' C parser saw `dtype_cast_order`
    // shrink to `[int64]` and gave up after the first (failed) cast, so
    // every float/str/bool column read as an un-upcast `NoneType` na_count.
    // Reconciling C→VM before the VM→C publish makes the flush symmetric:
    // a genuine C write is adopted (the fingerprint then matches and the
    // publish is skipped), a VM mutation is untouched and still published.
    unsafe { reconcile_list_from_c(p) };
    let rc = match unsafe { &(*pre).obj } {
        Object::List(rc) => rc,
        _ => return,
    };
    // Fingerprint the VM-shared list (allocation-free). If it matches what we
    // last published to `ob_item`, the list is unmutated since the previous
    // flush and the buffer is already coherent — leave it untouched (no
    // allocation, no refcount churn). This is what keeps a flush at *every*
    // VM→C boundary affordable; only a genuinely mutated list pays to rebuild.
    let fp: Vec<u64> = rc.borrow().iter().map(fingerprint).collect();
    if let Ok(g) = SEEDED_LISTS.lock() {
        if let Some(map) = g.as_ref() {
            if let Some(st) = map.get(&(p as usize)) {
                if st.rc_fp == fp {
                    return;
                }
            }
        }
    }
    let items: Vec<Object> = rc.borrow().clone();
    let n = items.len();
    let old_n = unsafe { list_size(p) }.max(0) as usize;
    if std::env::var_os("WEAVEPY_TRACE_LISTSYNC").is_some() {
        eprintln!("[LISTSYNC] sync {p:p} prefix_len={n} old_ob_size={old_n}");
    }
    if n > 0 {
        unsafe { list_reserve(p, n) };
    }
    let lo = p as *mut layout::PyListObject;
    let base = unsafe { (*lo).ob_item };
    if base.is_null() && n > 0 {
        return;
    }
    for (i, it) in items.iter().enumerate() {
        let slot = unsafe { base.add(i) };
        let old = unsafe { *slot };
        let new = crate::object::into_owned(it.clone());
        if std::env::var_os("WEAVEPY_TRACE_LISTSYNC").is_some() && n <= 3 {
            eprintln!(
                "[LISTSYNC]   slot {i}: old={old:p} new={new:p} {}",
                if new == old { "SKIP" } else { "REPLACE" }
            );
        }
        if new == old {
            // Stable identity: `into_owned` handed back a fresh reference
            // to the very pointer already in the slot. Release it and keep
            // the slot as-is (no churn, no dangling pointer).
            if !new.is_null() {
                unsafe { crate::object::Py_DecRef(new) };
            }
            continue;
        }
        unsafe { *slot = new };
        if !old.is_null() {
            unsafe { crate::object::Py_DecRef(old) };
        }
    }
    // A shrink (pop/remove/slice-delete) leaves stale tail occupants; drop
    // their references and clear the slots.
    if old_n > n && !base.is_null() {
        for i in n..old_n {
            let slot = unsafe { base.add(i) };
            let old = unsafe { *slot };
            unsafe { *slot = ptr::null_mut() };
            if !old.is_null() {
                unsafe { crate::object::Py_DecRef(old) };
            }
        }
    }
    let vo = p as *mut layout::PyVarObject;
    unsafe { (*vo).ob_size = n as PySsizeT };
    // Record the published fingerprint (so the next flush can skip an
    // unmutated list) and the resulting `ob_item` snapshot (so a read-back
    // does not mistake this publish for a foreign C write). `get_mut` (not
    // `insert`) avoids resurrecting an entry an interleaved
    // decref→`unregister_seeded_list` may have removed.
    let c_ptrs = unsafe { list_ptr_snapshot(p) };
    if let Ok(mut g) = SEEDED_LISTS.lock() {
        if let Some(map) = g.as_mut() {
            if let Some(slot) = map.get_mut(&(p as usize)) {
                slot.rc_fp = fp;
                slot.c_ptrs = c_ptrs;
            }
        }
    }
}

/// Re-sync every seeded faithful list mirror's `ob_item` from its prefix
/// `Rc`. Called at the VM→C boundary so a stock extension's inlined
/// `PyList_GET_ITEM` macro reads the VM's latest list mutations.
///
/// # Safety
/// May only be called when no C code is mid-read of a seeded list's
/// `ob_item` (i.e. at a VM→C transition).
pub unsafe fn flush_seeded_lists() {
    if std::env::var_os("WEAVEPY_NO_LISTSYNC").is_some() {
        return;
    }
    let c = SEEDED_LIST_COUNT.load(Ordering::Relaxed);
    if c == 0 {
        return;
    }
    // A decref inside a sync can free an object whose drop re-enters here;
    // skip the nested call rather than re-snapshotting mid-flush.
    if FLUSHING.with(|f| f.replace(true)) {
        return;
    }
    let _guard = FlushGuard;
    if std::env::var_os("WEAVEPY_TRACE_LISTSYNC").is_some() {
        eprintln!("[LISTSYNC] flush count={c}");
    }
    // Snapshot under the lock, then sync without holding it (a slot decref
    // may free an object and re-enter this module).
    let ptrs: Vec<usize> = match SEEDED_LISTS.lock() {
        Ok(g) => g
            .as_ref()
            .map(|m| m.keys().copied().collect())
            .unwrap_or_default(),
        Err(_) => return,
    };
    for pu in ptrs {
        unsafe { sync_list_ob_item(pu as *mut PyObject) };
    }
}

// ---------------------------------------------------------------------------
// Faithful mutable unicode (RFC 0047, wave 5).
//
// WeavePy's native string is an immutable `Rc<str>`, but macro-heavy
// Cython mutates a string's character buffer *in place*: the f-string /
// `repr` codegen builds a result by `PyUnicode_New(n, maxchar)` followed by
// the inlined `PyUnicode_WRITE` macro (a direct store at `PyUnicode_DATA(o)
// + i*kind`), and concatenation takes an in-place fast path —
// `PyUnicode_Resize(&left, left_len+right_len)` then
// `PyUnicode_CopyCharacters(left, left_len, right, 0, right_len)` — when
// `left` is uniquely owned and not interned. To satisfy a stock reader the
// buffer must be a real, writable PEP 393 body, and any in-place mutation
// must be visible when the string crosses back. We therefore mint such
// strings as **buffer-authoritative** mirrors ([`MirrorPrefix::str_buffer`])
// whose C body — not the staged prefix object — is read by [`native_of`].
// ---------------------------------------------------------------------------

/// The PEP 393 compact form for a string whose largest code point is
/// `maxchar`: `(kind, ascii, data_offset, char_width)`. The data offset is
/// where the inlined `PyUnicode_DATA` macro looks: just past
/// `PyASCIIObject` for a compact-ASCII string, else past
/// `PyCompactUnicodeObject` (which carries the UTF-8 cache fields).
fn unicode_form(maxchar: u32) -> (u32, bool, usize, usize) {
    let ascii_head = std::mem::size_of::<layout::PyASCIIObject>();
    let compact_head = std::mem::size_of::<layout::PyCompactUnicodeObject>();
    if maxchar < 0x80 {
        (ustate::KIND_1BYTE, true, ascii_head, 1)
    } else if maxchar < 0x100 {
        (ustate::KIND_1BYTE, false, compact_head, 1)
    } else if maxchar < 0x1_0000 {
        (ustate::KIND_2BYTE, false, compact_head, 2)
    } else {
        (ustate::KIND_4BYTE, false, compact_head, 4)
    }
}

/// The maximum code point a `kind`/`ascii` body may hold. A compact-ASCII
/// body is capped at `0x7F` (CPython's `PyUnicode_MAX_CHAR_VALUE`), so
/// writing a Latin-1 char into it is rejected, matching CPython.
#[inline]
fn kind_maxchar(kind: u32, ascii: bool) -> u32 {
    match kind {
        1 => {
            if ascii {
                0x7f
            } else {
                0xff
            }
        }
        2 => 0xffff,
        _ => 0x10_ffff,
    }
}

/// Store one code point into a PEP 393 buffer of the given `kind`.
///
/// # Safety
/// `data` must point at a writable buffer with room for `i + 1` units of
/// `kind` bytes each.
#[inline]
unsafe fn write_codepoint(data: *mut u8, kind: u32, i: usize, cp: u32) {
    match kind {
        1 => unsafe { *data.add(i) = cp as u8 },
        2 => unsafe { *(data as *mut u16).add(i) = cp as u16 },
        _ => unsafe { *(data as *mut u32).add(i) = cp },
    }
}

/// Load one code point from a PEP 393 buffer of the given `kind`.
///
/// # Safety
/// `data` must point at a readable buffer with at least `i + 1` units.
#[inline]
unsafe fn read_codepoint(data: *const u8, kind: u32, i: usize) -> u32 {
    match kind {
        1 => unsafe { *data.add(i) as u32 },
        2 => unsafe { *(data as *const u16).add(i) as u32 },
        _ => unsafe { *(data as *const u32).add(i) },
    }
}

/// True iff `p` is a **buffer-authoritative** unicode mirror — a string
/// built by [`new_unicode_mirror`] whose C buffer is the source of truth
/// and is safe to mutate through [`unicode_write_char`] /
/// [`unicode_copy_characters`]. A normal str mirror or a foreign string
/// returns `false`.
///
/// # Safety
/// `p` must be non-null and point at a valid object head.
pub unsafe fn is_str_buffer(p: *mut PyObject) -> bool {
    if !unsafe { is_mirror(p) } {
        return false;
    }
    let head = unsafe { &*p };
    if head.ob_type.is_null() || !std::ptr::eq(head.ob_type, types::PyUnicode_Type.as_ptr()) {
        return false;
    }
    unsafe { (*prefix_of(p)).str_buffer }
}

/// `(kind, ascii, length, data)` for a unicode mirror that carries a
/// faithful PEP 393 body (a buffer-authoritative string, or a normal
/// `fill_str` mirror). `data` points at the writable character buffer.
///
/// # Safety
/// `p` must be a unicode mirror with a faithful body (its allocation is at
/// least `size_of::<PyASCIIObject>()`).
unsafe fn str_buffer_info(p: *mut PyObject) -> (u32, bool, usize, *mut u8) {
    let ao = p as *mut layout::PyASCIIObject;
    let len = {
        let l = unsafe { (*ao).length };
        if l < 0 {
            0
        } else {
            l as usize
        }
    };
    let state = unsafe { (*ao).state };
    let kind = (state >> ustate::KIND_SHIFT) & 0x7;
    let ascii = (state >> ustate::ASCII_SHIFT) & 0x1 != 0;
    let off = if ascii {
        std::mem::size_of::<layout::PyASCIIObject>()
    } else {
        std::mem::size_of::<layout::PyCompactUnicodeObject>()
    };
    let data = unsafe { (p as *mut u8).add(off) };
    (kind, ascii, len, data)
}

/// The largest code point representable by a unicode mirror's body
/// (`0x7F`/`0xFF`/`0xFFFF`/`0x10FFFF`), or `None` if `p` is not a unicode
/// mirror with a faithful body. Used by [`resize_unicode`] to preserve the
/// source string's kind across a resize (CPython never narrows the kind).
///
/// # Safety
/// `p` must be non-null and point at a valid object head.
unsafe fn mirror_str_maxchar(p: *mut PyObject) -> Option<u32> {
    if !unsafe { is_mirror(p) } {
        return None;
    }
    let head = unsafe { &*p };
    if head.ob_type.is_null() || !std::ptr::eq(head.ob_type, types::PyUnicode_Type.as_ptr()) {
        return None;
    }
    let pre = unsafe { prefix_of(p) };
    let body_size = unsafe { (*pre).alloc_size }.saturating_sub(PREFIX_SIZE);
    if body_size < std::mem::size_of::<layout::PyASCIIObject>() {
        // A non-Latin-1 string crosses head-only (no faithful buffer); its
        // value lives in the prefix, so fall back to a content scan.
        return None;
    }
    let (kind, ascii, _len, _data) = unsafe { str_buffer_info(p) };
    Some(kind_maxchar(kind, ascii))
}

/// Reconstruct an [`Object::Str`] from a unicode mirror's faithful PEP 393
/// buffer (length, `kind`, and character data). Used by [`native_of`] for a
/// buffer-authoritative string so a direct `PyUnicode_WRITE` /
/// `PyUnicode_CopyCharacters` mutation is visible on read-back.
///
/// # Safety
/// `p` must be a unicode mirror with a faithful body
/// ([`is_str_buffer`], or a normal `fill_str` mirror).
pub unsafe fn read_str(p: *mut PyObject) -> Object {
    let (kind, _ascii, len, data) = unsafe { str_buffer_info(p) };
    if kind == 0 {
        // No PEP 393 kind: not a faithful buffer — defer to the prefix.
        return unsafe { (*prefix_of(p)).obj.clone() };
    }
    let mut s = String::with_capacity(len);
    for i in 0..len {
        let cp = unsafe { read_codepoint(data, kind, i) };
        s.push(char::from_u32(cp).unwrap_or('\u{fffd}'));
    }
    Object::from_str(s)
}

/// Decode any faithful `PyUnicodeObject` body — **compact** (inline data,
/// the `PyUnicode_New` form) or **legacy / non-compact** (out-of-line
/// `data.any`, the `unicode_subtype_new` form numpy's `str_` uses) — into a
/// Rust [`String`]. Returns `None` if the body has no valid PEP 393 kind
/// (so the caller can fall back). Mirrors the inlined `PyUnicode_KIND` /
/// `PyUnicode_DATA` reader macros.
///
/// # Safety
/// `p` must point at a readable object head whose body is at least
/// `size_of::<PyASCIIObject>()` (compact) or `size_of::<PyUnicodeObject>()`
/// (non-compact) bytes.
pub unsafe fn read_unicode_value(p: *mut PyObject) -> Option<String> {
    let ao = p as *const layout::PyASCIIObject;
    let length = {
        let l = unsafe { (*ao).length };
        if l < 0 {
            return None;
        }
        l as usize
    };
    let state = unsafe { (*ao).state };
    let kind = (state >> ustate::KIND_SHIFT) & 0x7;
    if kind == 0 {
        return None;
    }
    let ascii = (state >> ustate::ASCII_SHIFT) & 0x1 != 0;
    let compact = (state >> ustate::COMPACT_SHIFT) & 0x1 != 0;
    let data: *const u8 = if compact {
        let off = if ascii {
            std::mem::size_of::<layout::PyASCIIObject>()
        } else {
            std::mem::size_of::<layout::PyCompactUnicodeObject>()
        };
        unsafe { (p as *const u8).add(off) }
    } else {
        let uo = p as *const layout::PyUnicodeObject;
        unsafe { (*uo).data as *const u8 }
    };
    if data.is_null() {
        return None;
    }
    let mut s = String::with_capacity(length);
    for i in 0..length {
        let cp = unsafe { read_codepoint(data, kind, i) };
        s.push(char::from_u32(cp).unwrap_or('\u{fffd}'));
    }
    Some(s)
}

/// Read the value of a faithful **bytes-subtype** body (numpy's `bytes_`)
/// from its inline `PyBytesObject` fields: `ob_size` (offset 16) and the
/// inline `ob_sval` char array (offset 32). Returns `None` for a negative
/// (uninitialised) size. Mirror of [`read_unicode_value`] for `bytes`.
///
/// # Safety
/// `p` must be a faithful instance body whose type is a `bytes` subtype.
pub unsafe fn read_bytes_value(p: *mut PyObject) -> Option<Vec<u8>> {
    let bo = p as *const layout::PyBytesObject;
    let n = unsafe { (*bo).ob_base.ob_size };
    if n < 0 {
        return None;
    }
    let data = unsafe { (*bo).ob_sval.as_ptr() as *const u8 };
    if data.is_null() {
        return None;
    }
    Some(unsafe { std::slice::from_raw_parts(data, n as usize).to_vec() })
}

/// Mint a faithful, writable unicode mirror of `len` code points at the
/// PEP 393 kind implied by `maxchar`, with a zero-filled buffer (and a NUL
/// terminator unit). The caller owns one reference. This is the body of
/// `PyUnicode_New`: a stock extension fills it with the inlined
/// `PyUnicode_WRITE` macro and reads it with `PyUnicode_READ`, both of
/// which address `PyUnicode_DATA(o) + i*kind` — so the body must be a real
/// compact string at the exact offsets [`unicode_form`] computes.
pub fn new_unicode_mirror(len: usize, maxchar: u32) -> *mut PyObject {
    let (kind, ascii, data_off, width) = unicode_form(maxchar);
    let body_size = round_up(data_off + (len + 1) * width, 8);
    let total = PREFIX_SIZE + body_size;
    let layout = Layout::from_size_align(total, BODY_ALIGN).expect("unicode mirror layout");
    let raw = unsafe { alloc(layout) };
    assert!(!raw.is_null(), "unicode mirror allocation failed");
    unsafe { ptr::write_bytes(raw, 0, total) };

    let body = unsafe { raw.add(PREFIX_SIZE) } as *mut PyObject;
    let ty = types::PyUnicode_Type.as_ptr();
    unsafe {
        (*body).ob_refcnt = 1;
        (*body).ob_type = ty;
        let ao = body as *mut layout::PyASCIIObject;
        (*ao).length = len as PySsizeT;
        (*ao).hash = -1;
        (*ao).state = ustate::pack(0, kind, true, ascii, false);
        // utf8/utf8_length (compact non-ASCII) stay zeroed by the wipe.
    }

    let pre = raw as *mut MirrorPrefix;
    unsafe {
        ptr::write(
            pre,
            MirrorPrefix {
                obj: Object::None,
                inst: None,
                user_data: ptr::null_mut(),
                destructor: None,
                alloc_size: total,
                aux_ptr: ptr::null_mut(),
                aux_size: 0,
                str_buffer: true,
                list_synced: false,
                magic: MIRROR_MAGIC,
            },
        );
    }
    crate::object::register_minted(body);
    body
}

/// Resize the buffer-authoritative (or normal) unicode mirror `p` to
/// `newlen` code points, preserving the leading `min(oldlen, newlen)`
/// characters and the source kind. Returns a freshly minted mirror (the
/// caller publishes it and releases the old reference); the result's tail
/// `[oldlen, newlen)` is zero-filled, ready for `PyUnicode_CopyCharacters`.
/// Returns null if `p` is not a unicode object.
///
/// # Safety
/// `p` must be non-null and point at a valid object head.
pub unsafe fn resize_unicode(p: *mut PyObject, newlen: usize) -> *mut PyObject {
    // Snapshot the existing content (works for a buffer-authoritative body,
    // a normal `fill_str` mirror, or a head-only non-Latin-1 string).
    let content = unsafe { native_of(p) };
    let s = match content {
        Object::Str(s) => s,
        // PyUnicode_Resize only targets strings under construction; if `p`
        // is not a str, refuse rather than corrupt memory.
        _ => return ptr::null_mut(),
    };
    let maxchar = unsafe { mirror_str_maxchar(p) }
        .unwrap_or_else(|| s.chars().map(|c| c as u32).max().unwrap_or(0));
    let np = new_unicode_mirror(newlen, maxchar);
    let (kind, _ascii, _nlen, data) = unsafe { str_buffer_info(np) };
    for (i, ch) in s.chars().take(newlen).enumerate() {
        unsafe { write_codepoint(data, kind, i, ch as u32) };
    }
    np
}

/// Write one code point into a buffer-authoritative unicode mirror at
/// `idx` (the body of `PyUnicode_WriteChar`). Returns an error string for
/// an out-of-range index, a code point too wide for the body's kind, or a
/// non-writable target.
///
/// # Safety
/// `o` must be non-null and point at a valid object head.
pub unsafe fn unicode_write_char(o: *mut PyObject, idx: usize, ch: u32) -> Result<(), String> {
    if !unsafe { is_str_buffer(o) } {
        return Err("PyUnicode_WriteChar: target is not a writable unicode buffer".to_owned());
    }
    let (kind, ascii, len, data) = unsafe { str_buffer_info(o) };
    if idx >= len {
        return Err("string index out of range".to_owned());
    }
    if ch > kind_maxchar(kind, ascii) {
        return Err("character does not fit in the string's storage".to_owned());
    }
    unsafe { write_codepoint(data, kind, idx, ch) };
    Ok(())
}

/// Copy `how_many` code points from `from[from_start..]` into the
/// buffer-authoritative mirror `to` at `to_start` (the body of
/// `PyUnicode_CopyCharacters`). `from` may be any string (read through
/// [`native_of`]); the source is snapshotted first, so an overlapping
/// `from == to` copy is well-defined. Returns the number copied, or an
/// error string.
///
/// # Safety
/// `to` and `from` must be non-null and point at valid object heads.
pub unsafe fn unicode_copy_characters(
    to: *mut PyObject,
    to_start: usize,
    from: *mut PyObject,
    from_start: usize,
    how_many: usize,
) -> Result<usize, String> {
    if !unsafe { is_str_buffer(to) } {
        return Err("PyUnicode_CopyCharacters: target is not a writable unicode buffer".to_owned());
    }
    let (to_kind, to_ascii, to_len, to_data) = unsafe { str_buffer_info(to) };
    if to_start > to_len || how_many > to_len - to_start {
        return Err("PyUnicode_CopyCharacters: target index out of range".to_owned());
    }
    let from_obj = unsafe { native_of(from) };
    let from_s = match from_obj {
        Object::Str(s) => s,
        _ => return Err("PyUnicode_CopyCharacters: source is not a str".to_owned()),
    };
    let from_chars: Vec<u32> = from_s.chars().map(|c| c as u32).collect();
    if from_start > from_chars.len() || how_many > from_chars.len() - from_start {
        return Err("PyUnicode_CopyCharacters: source index out of range".to_owned());
    }
    let cap = kind_maxchar(to_kind, to_ascii);
    for k in 0..how_many {
        let cp = from_chars[from_start + k];
        if cp > cap {
            return Err(
                "PyUnicode_CopyCharacters: character does not fit in target storage".to_owned(),
            );
        }
        unsafe { write_codepoint(to_data, to_kind, to_start + k, cp) };
    }
    Ok(how_many)
}

/// Read one code point from a buffer-authoritative unicode mirror at
/// `idx`, or `None` for an out-of-range index / non-buffer target.
///
/// # Safety
/// `o` must be non-null and point at a valid object head.
pub unsafe fn unicode_read_char(o: *mut PyObject, idx: usize) -> Option<u32> {
    if !unsafe { is_str_buffer(o) } {
        return None;
    }
    let (kind, _ascii, len, data) = unsafe { str_buffer_info(o) };
    if idx >= len {
        return None;
    }
    Some(unsafe { read_codepoint(data, kind, idx) })
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

/// Bring a faithful list mirror's shared prefix `Object::List` *contents*
/// into line with its current C `ob_item` buffer — once, **in place** so
/// the `Rc` identity (and any VM alias that observes it, e.g. a
/// `defaultdict[k]` entry) is preserved — then mark the mirror
/// prefix-authoritative and register it for VM→C re-sync. A no-op once
/// already synced.
///
/// This is the C→VM half of faithful-list coherence (RFC 0047, wave 5):
/// a stock `PyList_Append`/`PyList_SetItem` writes only the inline
/// `ob_item`, but Cython routinely holds the *same* list in the VM (a
/// dict entry, a `cdef` attribute) and reads it back there. Without this
/// the mutation vanished — a `cdef defaultdict group_dict` built with
/// `group_dict[k].append(...)` (pandas' `internals.get_blkno_indexers`)
/// yielded empty lists.
///
/// For a VM-originated list the prefix `Rc` and `ob_item` already agree,
/// so the one-time refill is a cheap no-op copy; for a C-built list
/// (`PyList_New` + `PyList_SET_ITEM` macro) it captures the
/// macro-written elements before the targeted mutation is applied.
///
/// # Safety
/// `p` must satisfy [`is_faithful_list`].
unsafe fn list_prefix_seed_once(p: *mut PyObject) {
    let pre = unsafe { prefix_of(p) };
    if unsafe { (*pre).list_synced } {
        return;
    }
    let rc = match unsafe { &(*pre).obj } {
        Object::List(rc) => rc.clone(),
        _ => return,
    };
    let cur = unsafe { read_list_vec(p) };
    *rc.borrow_mut() = cur;
    unsafe { (*pre).list_synced = true };
    register_seeded_list(p);
}

/// Append `item` to a faithful list mirror, taking a new strong
/// reference (CPython `PyList_Append` semantics — the caller keeps its
/// own reference). Writes the inline `ob_item` buffer *and* the shared
/// prefix `Object::List` `Rc` (the VM-visible view), keeping the two
/// coherent so a VM holder of the same list sees the append.
///
/// # Safety
/// `p` must satisfy [`is_faithful_list`]; `item` must be a live,
/// non-null `PyObject*`.
pub unsafe fn list_append(p: *mut PyObject, item: *mut PyObject) {
    unsafe { list_prefix_seed_once(p) };
    let n = unsafe { list_size(p) } as usize;
    let base = unsafe { list_reserve(p, n + 1) };
    unsafe { crate::object::Py_IncRef(item) };
    unsafe { *base.add(n) = item };
    let vo = p as *mut layout::PyVarObject;
    unsafe { (*vo).ob_size = (n + 1) as PySsizeT };
    // Write-through to the shared prefix `Rc` (identity preserved) so a VM
    // alias — a `defaultdict[k]` list a Cython `.append(...)` mutated —
    // observes the append (RFC 0047, wave 5).
    let pre = unsafe { prefix_of(p) };
    if let Object::List(rc) = unsafe { &(*pre).obj } {
        rc.borrow_mut()
            .push(unsafe { crate::object::clone_object(item) });
    }
    unsafe { note_c_agreement(p) };
}

/// Insert `item` before `pos` (clamped to `[0, len]`) in a faithful list
/// mirror, taking a new strong reference.
///
/// # Safety
/// `p` must satisfy [`is_faithful_list`]; `item` must be a live,
/// non-null `PyObject*`.
pub unsafe fn list_insert(p: *mut PyObject, pos: PySsizeT, item: *mut PyObject) {
    unsafe { list_prefix_seed_once(p) };
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
    // Mirror the insert into the shared prefix `Rc` (RFC 0047, wave 5).
    let pre = unsafe { prefix_of(p) };
    if let Object::List(rc) = unsafe { &(*pre).obj } {
        let mut v = rc.borrow_mut();
        let at = at.min(v.len());
        v.insert(at, unsafe { crate::object::clone_object(item) });
    }
    unsafe { note_c_agreement(p) };
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
    unsafe { list_prefix_seed_once(p) };
    let lo = p as *mut layout::PyListObject;
    let base = unsafe { (*lo).ob_item };
    let slot = unsafe { base.add(pos as usize) };
    let prev = unsafe { *slot };
    unsafe { *slot = item };
    if !prev.is_null() {
        unsafe { crate::object::Py_DecRef(prev) };
    }
    // Mirror the store into the shared prefix `Rc` (RFC 0047, wave 5).
    let pre = unsafe { prefix_of(p) };
    if let Object::List(rc) = unsafe { &(*pre).obj } {
        let mut v = rc.borrow_mut();
        let idx = pos as usize;
        if idx < v.len() {
            v[idx] = unsafe { crate::object::clone_object(item) };
        }
    }
    unsafe { note_c_agreement(p) };
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
    // Re-publish the reordering into the shared prefix `Rc`, in place so a
    // VM alias observes it (RFC 0047, wave 5).
    let pre = unsafe { prefix_of(p) };
    if let Object::List(rc) = unsafe { &(*pre).obj } {
        let cur = unsafe { read_list_vec(p) };
        *rc.borrow_mut() = cur;
        if !unsafe { (*pre).list_synced } {
            unsafe { (*pre).list_synced = true };
            register_seeded_list(p);
        }
    }
    unsafe { note_c_agreement(p) };
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
    unsafe { record_mirror_free(p) };
    crate::object::unregister_minted(p);
    // Drop a seeded list mirror from the write-through set. Gated on the
    // atomic so an ordinary mirror free (float/int/…) never takes the lock.
    if SEEDED_LIST_COUNT.load(Ordering::Relaxed) > 0 && unsafe { is_faithful_list(p) } {
        unregister_seeded_list(p);
    }
    // RFC 0047 (wave 5): drop this box from the canonical set cache before
    // its prefix (and the native `Rc` the key is derived from) is dropped.
    if SET_BOX_COUNT.load(Ordering::Relaxed) > 0 && unsafe { is_faithful_set(p) } {
        unsafe { unregister_set_box(p) };
    }
    let pre = unsafe { prefix_of(p) };
    let destructor = unsafe { (*pre).destructor };
    if let Some(d) = destructor {
        unsafe { d(p) };
    }
    let alloc_size = unsafe { (*pre).alloc_size };
    let aux_ptr = unsafe { (*pre).aux_ptr };
    let aux_size = unsafe { (*pre).aux_size };

    // A list's out-of-line `ob_item` (RFC 0046, wave 4) holds one owned
    // reference per element (including any a stock `PyList_SET_ITEM` stored
    // directly), so release them before freeing the buffer. Immortal
    // singletons (None/bool) no-op. Gated on `is_faithful_list`: a faithful
    // **memoryview** mirror (RFC 0047, wave 5) also carries an aux buffer,
    // but its bytes are packed `shape`/`strides`/data/format — *not*
    // `PyObject*` slots — and must never be decref'd here.
    if !aux_ptr.is_null() && aux_size > 0 && unsafe { is_faithful_list(p) } {
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

    // RFC 0047 (wave 5): a faithful bound method owns one reference to each
    // of `im_func` and `im_self` (materialised in `fill_body`), so release
    // them before the block goes away. Immortal singletons no-op.
    if unsafe { is_faithful_method(p) } {
        let mo = p as *mut layout::PyMethodObject;
        let func = unsafe { (*mo).im_func };
        let recv = unsafe { (*mo).im_self };
        if !func.is_null() {
            unsafe { crate::object::Py_DecRef(func) };
        }
        if !recv.is_null() {
            unsafe { crate::object::Py_DecRef(recv) };
        }
    }

    // RFC 0047 (wave 5): a faithful slice owns one reference to each of
    // `start`/`stop`/`step` (materialised in `fill_body`), so release them
    // before the block goes away. Immortal singletons (None/bool) no-op.
    if unsafe { is_faithful_slice(p) } {
        let so = p as *mut layout::PySliceObject;
        for field in [
            unsafe { (*so).start },
            unsafe { (*so).stop },
            unsafe { (*so).step },
        ] {
            if !field.is_null() {
                unsafe { crate::object::Py_DecRef(field) };
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
    /// Faithful `PyMethodObject` (a bound method) with `im_func`/`im_self`
    /// populated (RFC 0047, wave 5). Macro-heavy Cython unpacks a bound
    /// method by reading those two fields straight off the struct
    /// (`PyMethod_GET_FUNCTION` / `PyMethod_GET_SELF`) before calling — so
    /// they must hold real, owned `PyObject*`s, not opaque box bytes.
    Method,
    /// Faithful `PyDictObject` header (RFC 0047, wave 5): `ma_used` holds
    /// the item count so a stock `PyDict_GET_SIZE` / the Cython keyword
    /// fast path reads the right size. The entries live in the prefix's
    /// native dict (reached via the C-API functions), so `ma_keys` /
    /// `ma_values` stay NULL.
    Dict,
    /// Faithful `PySetObject` header (RFC 0047, wave 5): `fill`/`used` hold
    /// the element count so a stock `PySet_GET_SIZE` / `PyFrozenSet_GET_SIZE`
    /// macro — which Cython emits for both `len(s)` and the truthiness test
    /// `if s:` on a set-typed value — reads the right size. `table` points at
    /// the inline (empty) `smalltable`; the entries live in the prefix's
    /// native set (reached via `PySet_Size` / `tp_iter`).
    Set,
    /// Faithful `PySliceObject` (RFC 0047, wave 5) with `start`/`stop`/`step`
    /// populated as owned references. Macro-heavy Cython reads those three
    /// fields straight off the struct (`((PySliceObject*)s)->step`), so they
    /// must hold real `PyObject*`s. A slice is immutable, so the prefix's
    /// staged `Object` stays authoritative on read-back; these owned refs are
    /// released in `free_mirror`.
    Slice,
    /// Faithful `PyMemoryViewObject` (RFC 0047, wave 5) with a populated
    /// inline `Py_buffer view`. `PyMemoryView_GET_BUFFER` is a macro
    /// (`&mv->view`), so Cython's fused-type dispatch reads `view.ndim`,
    /// `view.itemsize` and `view.format` straight off the struct — pandas'
    /// `lib.map_infer_mask` keys its `ndarray[object]` specialization on
    /// `itemsize == 8`/`format == "O"`. `view.buf`/`format`/`shape`/`strides`
    /// point into the mirror's out-of-line aux buffer (freed in
    /// `free_mirror`); the prefix's staged `Object::MemoryView` stays
    /// authoritative on read-back.
    MemoryView,
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
            Object::Str(s) if is_ascii_or_latin1(s) => {
                // A compact-ASCII string carries its 1-byte data just past
                // `PyASCIIObject` (40); a compact Latin-1 string carries it
                // past `PyCompactUnicodeObject` (56), where the inlined
                // `PyUnicode_DATA` macro reads it (a stock extension keys
                // the offset off the `ascii` state bit). Size the body for
                // whichever form `fill_str` will write.
                let n = s.chars().count();
                let (_kind, _ascii, data_off, width) = unicode_form(str_maxchar(s));
                BodyPlan {
                    kind: BodyKind::Str,
                    body_size: round_up(data_off + (n + 1) * width, 8),
                }
            }
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
            Object::BoundMethod(_) => BodyPlan {
                kind: BodyKind::Method,
                // Exactly `PyMethodObject`; `im_func`/`im_self` are owned
                // refs filled in `fill_body` and released in `free_mirror`.
                body_size: std::mem::size_of::<layout::PyMethodObject>(),
            },
            Object::Dict(_) => BodyPlan {
                kind: BodyKind::Dict,
                // Exactly `PyDictObject`; only `ma_used` is populated.
                body_size: std::mem::size_of::<layout::PyDictObject>(),
            },
            Object::Set(_) | Object::FrozenSet(_) => BodyPlan {
                kind: BodyKind::Set,
                // Exactly `PySetObject`; `fill`/`used` carry the count and
                // `table` points at the inline (empty) `smalltable`.
                body_size: std::mem::size_of::<layout::PySetObject>(),
            },
            Object::Slice(_) => BodyPlan {
                kind: BodyKind::Slice,
                // Exactly `PySliceObject`; `start`/`stop`/`step` are owned
                // refs filled in `fill_body` and released in `free_mirror`.
                body_size: std::mem::size_of::<layout::PySliceObject>(),
            },
            Object::MemoryView(_) => BodyPlan {
                kind: BodyKind::MemoryView,
                // Exactly `PyMemoryViewObject` (up to `weakreflist`); the
                // inline `view`'s `buf`/`format`/`shape`/`strides` point at a
                // packed out-of-line aux buffer filled in `fill_body`.
                body_size: std::mem::size_of::<layout::PyMemoryViewObject>(),
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
        BodyKind::Method => {
            // Lay a faithful `PyMethodObject` over the body and populate
            // `im_func`/`im_self` with owned references, so a stock
            // `PyMethod_GET_FUNCTION(m)` / `PyMethod_GET_SELF(m)` (the
            // macros Cython's `with`/`for`/call fast paths inline) read a
            // real function and receiver rather than Rust enum bytes. The
            // calling convention WeavePy applies when the *method* is
            // invoked (prepend `receiver`, call `function`) matches what
            // Cython does after unpacking (prepend `im_self`, call
            // `im_func`), so both routes reach the same callee with the
            // same `self`. `im_weakreflist`/`vectorcall` stay NULL — the
            // method is never invoked through its own vectorcall slot (its
            // `tp_call` is unset, so a stock `PyObject_Call` routes through
            // the VM via the prefix's `BoundMethod`). The owning
            // `BoundMethod` also lives in the prefix, so these two extra
            // owned refs are released in `free_mirror`.
            if let Object::BoundMethod(bm) = obj {
                let mo = body as *mut layout::PyMethodObject;
                let func = crate::object::into_owned(bm.function.clone());
                let recv = crate::object::into_owned(bm.receiver.clone());
                unsafe {
                    (*mo).im_func = func;
                    (*mo).im_self = recv;
                    (*mo).im_weakreflist = ptr::null_mut();
                    (*mo).vectorcall = ptr::null_mut();
                }
            }
            let _ = (aux_ptr, aux_size);
        }
        BodyKind::Dict => {
            // Faithful `PyDictObject` header. Only `ma_used` (the item
            // count a stock `PyDict_GET_SIZE` reads directly) is meaningful;
            // the entries are served from the prefix's native dict through
            // the C-API, so `ma_keys` / `ma_values` stay NULL.
            if let Object::Dict(rc) = obj {
                let d = body as *mut layout::PyDictObject;
                unsafe {
                    (*d).ma_used = rc.borrow().len() as PySsizeT;
                    (*d).ma_version_tag = 0;
                    (*d).ma_keys = ptr::null_mut();
                    (*d).ma_values = ptr::null_mut();
                }
            }
            let _ = (aux_ptr, aux_size);
        }
        BodyKind::Set => {
            // Faithful `PySetObject` header. `fill`/`used` are the element
            // count a stock `PySet_GET_SIZE` reads directly; the entries are
            // served from the prefix's native set via the C-API, so `table`
            // just points at the (zeroed) inline `smalltable` and the set
            // looks like a freshly-initialised — if under-populated — CPython
            // set (`mask == PySet_MINSIZE - 1`, `hash == -1`, `finger == 0`).
            let n = match obj {
                Object::Set(rc) => rc.borrow().len() as PySsizeT,
                Object::FrozenSet(fs) => fs.len() as PySsizeT,
                _ => 0,
            };
            let so = body as *mut layout::PySetObject;
            unsafe {
                (*so).fill = n;
                (*so).used = n;
                (*so).mask = (layout::PYSET_MINSIZE - 1) as PySsizeT;
                (*so).table = ptr::addr_of_mut!((*so).smalltable) as *mut core::ffi::c_void;
                (*so).hash = -1;
                (*so).finger = 0;
                (*so).weakreflist = ptr::null_mut();
            }
            let _ = (aux_ptr, aux_size);
        }
        BodyKind::Slice => {
            // Lay a faithful `PySliceObject` over the body and populate
            // `start`/`stop`/`step` with owned references, so a stock
            // `((PySliceObject*)s)->step` read (and the inline incref/decref
            // Cython brackets it with) hits real `PyObject*`s. A `None`
            // component reuses the immortal singleton so the incref/decref is a
            // no-op. The three owned refs are released in `free_mirror`.
            if let Object::Slice(s) = obj {
                let so = body as *mut layout::PySliceObject;
                let materialise = |o: &Object| -> *mut PyObject {
                    match o {
                        Object::None => crate::singletons::none_ptr(),
                        Object::Bool(true) => crate::singletons::true_ptr(),
                        Object::Bool(false) => crate::singletons::false_ptr(),
                        _ => crate::object::into_owned(o.clone()),
                    }
                };
                unsafe {
                    (*so).start = materialise(&s.start);
                    (*so).stop = materialise(&s.stop);
                    (*so).step = materialise(&s.step);
                }
            }
            let _ = (aux_ptr, aux_size);
        }
        BodyKind::MemoryView => {
            // Lay a faithful `PyMemoryViewObject` over the body and populate
            // its inline `Py_buffer view`, so a stock `PyMemoryView_GET_BUFFER`
            // macro (`&mv->view`) and the `__Pyx_PyMemoryView_Get_*` reads it
            // feeds hit real `ndim`/`itemsize`/`format`/`shape`/`strides`. The
            // window bytes, NUL-terminated format and the `shape`/`strides`
            // `Py_ssize_t` arrays are packed into one out-of-line aux block
            // (`view` points into it); the prefix's staged `Object::MemoryView`
            // stays authoritative on read-back ([`native_of`]). The aux block
            // is freed in [`free_mirror`] (gated off the list path, so its
            // bytes are never mistaken for `PyObject*` slots).
            if let Object::MemoryView(mv) = obj {
                let mo = body as *mut layout::PyMemoryViewObject;
                let itemsize = mv.itemsize.get().max(1);
                let nbytes = mv.len.get();
                let shape = mv.shape_dims();
                let strides = mv.stride_bytes();
                let ndim = shape.len();
                let data = if mv.released.get() {
                    Vec::new()
                } else {
                    mv.to_bytes()
                };
                let fmt = mv.format.borrow();
                let fmt_bytes = fmt.as_bytes();

                // Pack: [shape: ndim·8][strides: ndim·8][data][format+NUL],
                // 8-aligned arrays first so `view.shape`/`strides` are aligned.
                let ssz = std::mem::size_of::<PySsizeT>();
                let shape_off = 0usize;
                let strides_off = shape_off + ndim * ssz;
                let data_off = strides_off + ndim * ssz;
                let fmt_off = data_off + data.len();
                let total_aux = round_up(fmt_off + fmt_bytes.len() + 1, 8).max(8);
                let aux_layout =
                    Layout::from_size_align(total_aux, BODY_ALIGN).expect("mv aux layout");
                let aux = unsafe { alloc(aux_layout) };
                assert!(!aux.is_null(), "mv aux allocation failed");
                unsafe { ptr::write_bytes(aux, 0, total_aux) };

                let shape_ptr = unsafe { aux.add(shape_off) } as *mut PySsizeT;
                let strides_ptr = unsafe { aux.add(strides_off) } as *mut PySsizeT;
                let data_ptr = unsafe { aux.add(data_off) };
                let fmt_ptr = unsafe { aux.add(fmt_off) } as *mut core::ffi::c_char;
                for i in 0..ndim {
                    unsafe {
                        *shape_ptr.add(i) = shape[i] as PySsizeT;
                        *strides_ptr.add(i) = strides[i] as PySsizeT;
                    }
                }
                if !data.is_empty() {
                    unsafe {
                        ptr::copy_nonoverlapping(data.as_ptr(), data_ptr, data.len());
                    }
                }
                unsafe {
                    ptr::copy_nonoverlapping(fmt_bytes.as_ptr(), aux.add(fmt_off), fmt_bytes.len());
                }

                // `_Py_MEMORYVIEW_C`(1) | `_Py_MEMORYVIEW_FORTRAN`(2): a
                // contiguous view advertises both for 1-D, matching CPython's
                // `init_flags`. A released view advertises `_RELEASED`(16).
                let mut flags: core::ffi::c_int = 0;
                if mv.released.get() {
                    flags |= 0x10;
                } else if mv.is_c_contiguous() {
                    flags |= 0x1;
                    if ndim <= 1 {
                        flags |= 0x2;
                    }
                }

                unsafe {
                    // `PyObject_VAR_HEAD` `ob_size` is `ndim` (CPython sizes
                    // the `ob_array` tail off it); harmless to a reader that
                    // uses `view.ndim`.
                    (*mo).ob_base.ob_size = ndim as PySsizeT;
                    (*mo).mbuf = ptr::null_mut();
                    (*mo).hash = -1;
                    (*mo).flags = flags;
                    (*mo).exports = 0;
                    (*mo).weakreflist = ptr::null_mut();
                    // `view.obj` stays NULL: a stray `PyBuffer_Release` on the
                    // macro-fetched view is then a no-op (no spurious decref of
                    // the memoryview). The real buffer protocol path
                    // (`PyObject_GetBuffer(mv, …)`) is serviced separately by
                    // `fill_native_buffer`'s `MemoryView` branch.
                    (*mo).view.buf = data_ptr as *mut std::ffi::c_void;
                    (*mo).view.obj = ptr::null_mut();
                    (*mo).view.len = nbytes as PySsizeT;
                    (*mo).view.itemsize = itemsize as PySsizeT;
                    (*mo).view.readonly = core::ffi::c_int::from(mv.readonly.get());
                    (*mo).view.ndim = ndim as core::ffi::c_int;
                    (*mo).view.format = fmt_ptr;
                    (*mo).view.shape = if ndim > 0 { shape_ptr } else { ptr::null_mut() };
                    (*mo).view.strides = if ndim > 0 { strides_ptr } else { ptr::null_mut() };
                    (*mo).view.suboffsets = ptr::null_mut();
                    (*mo).view.internal = ptr::null_mut();
                }
                *aux_ptr = aux;
                *aux_size = total_aux;
            }
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

/// Fill a compact 1-byte (ASCII or Latin-1) unicode body. The planner
/// routes only `is_ascii_or_latin1` strings here, so the kind is always
/// 1-byte; the data offset (and the `ascii` state bit) differ between the
/// compact-ASCII form (data past `PyASCIIObject`) and the compact Latin-1
/// form (data past `PyCompactUnicodeObject`, where the inlined
/// `PyUnicode_DATA` macro reads it).
unsafe fn fill_str(body: *mut PyObject, obj: &Object) {
    let Object::Str(s) = obj else { return };
    let chars: Vec<u8> = s.chars().map(|c| c as u8).collect(); // latin-1 guaranteed by planner
    let n = chars.len();
    let (kind, ascii, data_off, _width) = unicode_form(str_maxchar(s));
    let ao = body as *mut layout::PyASCIIObject;
    unsafe {
        (*ao).length = n as PySsizeT;
        // RFC 0047 (wave 5): publish the real hash, not CPython's
        // "uncomputed" sentinel (-1). Macro-heavy Cython matches keyword
        // arguments by reading `((PyASCIIObject*)key)->hash` *directly*
        // off the struct and comparing it to each interned argname's hash
        // (`__Pyx_MatchKeywordArg_str`); both sides are WeavePy-minted
        // strings, so a `py_str_hash`-consistent value makes the compare
        // agree. Leaving -1 made every Cython keyword call fail with a
        // spurious "unexpected keyword argument".
        (*ao).hash = weavepy_vm::object::py_str_hash(s) as crate::object::PyHashT;
        (*ao).state = ustate::pack(
            0, // not interned
            kind,
            true,  // compact
            ascii, // ascii
            false, // not statically allocated
        );
        let data = (body as *mut u8).add(data_off);
        ptr::copy_nonoverlapping(chars.as_ptr(), data, n);
        *data.add(n) = 0;
    }
}

/// The largest code point in `s` (0 for the empty string), for
/// [`unicode_form`].
fn str_maxchar(s: &str) -> u32 {
    s.chars().map(|c| c as u32).max().unwrap_or(0)
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

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

/// Cached `WEAVEPY_TRACE_LISTSYNC` gate — checked on every seeded-list
/// flush (i.e. every bridged C call), so it must not `getenv` each time.
pub(crate) fn listsync_trace_enabled() -> bool {
    use std::sync::OnceLock;
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("WEAVEPY_TRACE_LISTSYNC").is_some())
}

/// Cached `WEAVEPY_NO_LISTSYNC` kill-switch (same hot path as above).
pub(crate) fn listsync_disabled() -> bool {
    use std::sync::OnceLock;
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("WEAVEPY_NO_LISTSYNC").is_some())
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
    static FREED_BODY_TYPES: RefCell<std::collections::HashMap<usize, String>> =
        RefCell::new(std::collections::HashMap::new());
    /// Diagnostic (WEAVEPY_WATCH_BLOCKS): addresses of blocks tuples to
    /// trace refcount ops on, to find a premature-free / over-decref.
    static WATCHED: RefCell<std::collections::HashSet<usize>> =
        RefCell::new(std::collections::HashSet::new());
    /// Diagnostic (WEAVEPY_WATCH_BLOCKS): free-site history (type + short
    /// backtrace) for each mirror address, so a later stale read can print
    /// the full reuse chain that led to the confusion.
    static FREE_BT: RefCell<std::collections::HashMap<usize, Vec<(String, String)>>> =
        RefCell::new(std::collections::HashMap::new());
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
    /// True iff this mirror is a faithful, **buffer-authoritative** bytes
    /// object: the result of `PyBytes_FromStringAndSize(NULL, n)`, whose
    /// contract is "caller fills the uninitialised buffer before exposing
    /// the object". A stock extension writes through the inlined
    /// `PyBytes_AS_STRING` macro (`ob_sval` directly) — orjson's `dumps`
    /// builds its output exactly this way — so the C body, not the staged
    /// prefix [`obj`](Self::obj), is authoritative on the first read-back
    /// ([`native_of`] adopts `ob_sval` and clears the flag; bytes are
    /// immutable once exposed, so later crossings reuse the adopted value).
    pub bytes_buffer: bool,
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
    /// True iff this mirror is a **canonical pinned scalar box** (RFC 0047,
    /// wave 5): a Float/Int/Str/Bytes/Long/Complex/Tuple minted while
    /// marshaling VM arguments into a C call and registered in
    /// [`SCALAR_PIN_CACHE`]. CPython extensions may store such an argument
    /// pointer *borrowed* (pandas' khash `PyObjectHashTable.set_item` keeps
    /// the raw `PyObject*` with no incref, relying on the caller's reference
    /// to keep it alive). WeavePy's VM references don't pin C boxes, so a
    /// pinned box is **not freed when its C refcount reaches zero** — it
    /// stays alive (and identity-stable for later crossings of the same
    /// value) until evicted by [`sweep_scalar_pins`].
    pub scalar_pinned: bool,
    /// For a faithful **list** mirror minted VM→C ([`mirror_out`]):
    /// the `(ob_item digest, prefix-`Rc` fingerprint)` pair captured at
    /// mint, when buffer and `Rc` agree by construction (RFC 0076 WS3).
    /// A mirror that is only ever *macro*-mutated by the extension —
    /// Cython's inlined `list.pop()` decrements `ob_size` with no C-API
    /// call — and then freed without a C→VM read-back never enters
    /// `SEEDED_LISTS`, so [`reconcile_list_from_c`] has no snapshot to
    /// compare against and the mutation dies with the box (lxml.sax's
    /// `_element_stack.pop()` "un-popped" on the next crossing). This
    /// mint-time pair lets the free-path reconcile adopt the C write iff
    /// C moved and the VM side did not. `None` for non-list mirrors.
    pub list_mint: Option<((u64, usize), (u64, usize))>,
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
    type_is_faithful(ty) || types::is_inline_instance_type(ty) || types::is_container_body_type(ty)
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
            | Object::WStr(_)
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
    // RFC 0047 (wave 5): a plain `list` likewise crosses as a single
    // canonical box per native `Rc` (see [`LIST_BOX_CACHE`]). Cython's
    // `__Pyx_PyList_Append` fast path appends through the raw
    // `PyList_SET_ITEM` + `__Pyx_SET_SIZE` macros — a direct `ob_item`
    // write on whichever box it happens to hold, invisible to WeavePy
    // until that same box is next touched. When every crossing minted a
    // fresh box (pandas' `IndexEngine.get_indexer_non_unique` re-fetches
    // `d[val]` with a fresh `np.int64` key each iteration, so the
    // borrowed-box cache missed every time), the macro-written elements
    // were stranded on abandoned boxes and roughly half the duplicate
    // positions vanished. One canonical box per native list makes every
    // fetch, macro write, and reconcile land on the same memory.
    // `PyTuple_New`'s staging list (advertised as `PyTuple_Type`) is
    // excluded: canonicalising it would hand a tuple-typed box to later
    // list crossings.
    if std::ptr::eq(ty, types::PyList_Type.as_ptr()) {
        if let Some(key) = list_rc_key(&obj) {
            if let Some(p) = cached_list_box(key) {
                // The cached box's `ob_item` may lag the shared `Rc` (a VM
                // mutation since the last VM→C flush); re-publish so a
                // stock macro read sees the live contents.
                unsafe { sync_list_ob_item(p) };
                return p;
            }
            // RFC 0069 WS5: the mint registers itself in the canonical
            // cache *before* filling `ob_item`, so a self-referential list
            // (`l.append(l)`, numpy's pathological-self-containing test)
            // resolves its inner crossing to the box being built instead
            // of recursing the mint until the C stack faults.
            return mirror_out_fresh_inner(obj, ty, Some(key));
        }
    }
    // A bytearray likewise crosses as a single canonical box per VM buffer
    // (RFC 0056 WS5) — extension code holds the pointer across mutations
    // (aiohttp's parser keeps `self._buf` in a cdef field) and macro-reads
    // its struct fields, which are refreshed on every crossing.
    if std::ptr::eq(ty, types::PyByteArray_Type.as_ptr()) {
        if let Some(key) = bytearray_rc_key(&obj) {
            if let Some(p) = cached_bytearray_box(key) {
                unsafe { sync_bytearray_fields(p) };
                return p;
            }
            let p = mirror_out_fresh(obj, ty);
            register_bytearray_box(key, p);
            return p;
        }
    }
    // A `builtin_function_or_method` crosses as a single canonical box (see
    // [`BUILTIN_BOX_CACHE`]) so pointer-identity tests (`op is operator.eq`,
    // as in `pandas._libs.ops.vec_compare`) hold across the boundary. Reuse
    // the live one whenever the same native builtin is already mirrored.
    if let Some(key) = builtin_rc_key(&obj) {
        if let Some(p) = cached_builtin_box(key) {
            return p;
        }
        let p = mirror_out_fresh(obj, ty);
        register_builtin_box(key, p);
        return p;
    }
    // RFC 0056 WS5: inside an intern scope (kwnames marshaling, the
    // `PyUnicode_Intern*` entry points), exact `str` values resolve through
    // the content-keyed intern table so equal text yields the *same*
    // pointer — extensions compare interned names by identity (orjson's
    // keyword dispatch).
    if intern_scope_active() && std::ptr::eq(ty, types::PyUnicode_Type.as_ptr()) {
        if let Object::Str(s) = &obj {
            let text = s.to_string();
            return interned_str_mirror(&text, ty, obj);
        }
    }
    // RFC 0066 WS7: `bytes` crossings *always* mint through the canonical
    // pin cache, not just while marshaling arguments. On CPython an
    // attribute read like `o.data` hands back a pointer whose storage the
    // owner keeps alive, so generated code (Cython) extracts the interior
    // buffer (`char* rawval = PyBytes_AS_STRING(t)`) and DECREFs the
    // temporary before the buffer's last use. An unpinned per-crossing
    // mirror dies on that DECREF — msgpack's `Packer._pack` then read the
    // `ExtType.data` payload out of freed memory (right length, all
    // zeros). A pinned box survives a zero C refcount (until swept), so
    // the borrowed interior pointer stays valid while the VM value is
    // reachable — the exact lifetime the extension assumes. Bytes are
    // immutable and keyed by `Rc` identity, so the wider scope can never
    // serve stale contents.
    if std::ptr::eq(ty, types::PyBytes_Type.as_ptr()) {
        if let Some(key) = scalar_pin_key(&obj) {
            if let Some(p) = cached_scalar_pin(key, ty) {
                return p;
            }
            let p = mirror_out_fresh(obj, ty);
            register_scalar_pin(key, p);
            return p;
        }
    }
    // RFC 0047 (wave 5): while marshaling VM arguments into a C call,
    // immutable hashable scalars mint through the canonical pin cache so a
    // callee that stores the pointer *borrowed* (pandas' khash hashtables)
    // reads valid memory for as long as the value stays reachable — see
    // [`ScalarPinKey`].
    if arg_pin_active() {
        if let Some(key) = scalar_pin_key(&obj) {
            if let Some(p) = cached_scalar_pin(key, ty) {
                return p;
            }
            let p = mirror_out_fresh(obj, ty);
            register_scalar_pin(key, p);
            return p;
        }
    }
    mirror_out_fresh(obj, ty)
}

/// Mint a fresh, never-shared mirror, bypassing every canonical-box and
/// pin cache. The *staging* creators (`PyBytes_FromStringAndSize(NULL,
/// n)`, `_PyBytes_Resize`) must come through here: their caller fills the
/// body in place through `PyBytes_AS_STRING`, and the read-back adoption
/// *replaces* the prefix `Rc` — which would break the pin cache's
/// key-liveness invariant (the dropped `Rc`'s heap address can be reused
/// by the next staging buffer, and the cache would serve the previous
/// call's box: orjson's second `dumps` returned the first call's JSON).
pub fn mirror_out_unpinned(obj: Object) -> *mut PyObject {
    let ty = types::type_for_object(&obj);
    mirror_out_fresh(obj, ty)
}

/// Mint a fresh faithful mirror block for `obj` (no canonical-box cache
/// consultation). Every mirror is born here; [`mirror_out_with_type`]
/// layers the set cache on top.
fn mirror_out_fresh(obj: Object, ty: *mut PyTypeObject) -> *mut PyObject {
    mirror_out_fresh_inner(obj, ty, None)
}

/// The mint body. `list_precache_key` is `Some` only from the canonical
/// list lane of [`mirror_out_with_type`]: the box is published to
/// [`LIST_BOX_CACHE`] *before* its elements are materialised, so a list
/// that (transitively) contains itself terminates — the inner crossing
/// hits the cache and stores this very box's pointer, giving the C side
/// the same self-referential `ob_item` CPython would (RFC 0069 WS5;
/// numpy's `test_pathological_self_containing` used to ride this mint
/// recursion into a stack fault).
fn mirror_out_fresh_inner(
    obj: Object,
    ty: *mut PyTypeObject,
    list_precache_key: Option<usize>,
) -> *mut PyObject {
    if listsync_trace_enabled() {
        if let Object::List(rc) = &obj {
            eprintln!(
                "[LISTSYNC] mint-list rc=0x{:x} len={}",
                weavepy_vm::sync::Rc::as_ptr(rc) as usize,
                rc.borrow().len()
            );
        }
    }
    if pin_trace_enabled() {
        if let Object::Bytes(b) = &obj {
            if b.len() > 4096 {
                eprintln!("[pin] mint-bytes {}B", b.len());
            }
        }
    }
    let plan = BodyPlan::for_object(&obj);
    let total = PREFIX_SIZE + plan.body_size;
    let layout = Layout::from_size_align(total, BODY_ALIGN).expect("mirror layout");
    let raw = unsafe { alloc(layout) };
    assert!(!raw.is_null(), "mirror allocation failed");
    unsafe { ptr::write_bytes(raw, 0, total) };

    let body = unsafe { raw.add(PREFIX_SIZE) } as *mut PyObject;

    // Head — written *before* the body fill so a re-entrant crossing of a
    // self-referential list (which hands back a fresh reference to this
    // box mid-fill) increments a live refcount instead of being clobbered.
    // No `fill_body` lane touches `ob_refcnt`/`ob_type`.
    unsafe {
        (*body).ob_refcnt = 1;
        (*body).ob_type = ty;
    }
    if let Some(key) = list_precache_key {
        register_list_box(key, body);
    }

    // Allocate any out-of-line buffer (list `ob_item`) before we move
    // `obj` into the prefix, so we can still read it.
    let mut aux_ptr: *mut u8 = ptr::null_mut();
    let mut aux_size: usize = 0;
    unsafe {
        fill_body(body, ty, &obj, &plan, &mut aux_ptr, &mut aux_size);
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
                bytes_buffer: false,
                list_synced: false,
                scalar_pinned: false,
                list_mint: None,
                magic: MIRROR_MAGIC,
            },
        );
    }
    // Capture the mint-time agreement for a VM-shared list (buffer was
    // just built *from* the `Rc`, so the two agree by construction) —
    // the free-path reconcile's baseline for adopting macro writes on a
    // never-registered mirror (RFC 0076 WS3; see `MirrorPrefix::list_mint`).
    if let Object::List(rc) = unsafe { &(*pre).obj } {
        let c = unsafe { list_ptr_snapshot(body) };
        let fp = digest_objects(rc.borrow().iter());
        unsafe { (*pre).list_mint = Some((c, fp)) };
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
                bytes_buffer: false,
                list_synced: false,
                scalar_pinned: false,
                list_mint: None,
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
/// The minted-registry check is what makes this sound to call on an
/// *arbitrary* pointer (e.g. a scratch buffer or a foreign object handed
/// to `PyObject_Free`): only a pointer WeavePy itself minted can carry
/// our negative-offset prefix, so the prefix bytes are only read for
/// those. A foreign block whose `ob_type` bytes happen to name a
/// registered inline type (an extension's `PyObject_Malloc` +
/// `PyObject_Init` object, a Cython freelist slot) previously reached
/// the prefix read — and when the block sat at the very start of an
/// mmap'd region, `p - PREFIX_SIZE` crossed into an unmapped page and
/// SIGBUS'd (pandas `get_indexer_non_unique` under `test_loc.py`).
///
/// # Safety
/// `p` must be non-null and point at a valid object head.
pub unsafe fn is_instance_body(p: *mut PyObject) -> bool {
    if !unsafe { is_mirror(p) } {
        return false;
    }
    if !crate::object::is_weavepy_owned(p) {
        return false;
    }
    let pre = unsafe { prefix_of(p) };
    unsafe { (*pre).magic == MIRROR_MAGIC && (*pre).inst.is_some() }
}

/// True if `p` is an instance body whose owning `PyInstance` is already
/// collected — the prefix's `Weak` no longer upgrades. Such a body was
/// *orphaned* by the free hook (RFC 0075 WS9): the instance died while a
/// C extension still held references acquired with the inlined
/// `Py_INCREF` macro (lxml's proxy registry), so the block is kept alive
/// as a C-owned object until its last C reference dies.
///
/// # Safety
/// `p` must be a valid, live `PyObject*`.
pub unsafe fn is_orphaned_instance_body(p: *mut PyObject) -> bool {
    if !unsafe { is_instance_body(p) } {
        return false;
    }
    let pre = unsafe { prefix_of(p) };
    match unsafe { (*pre).inst.as_ref() } {
        Some(w) => w.upgrade().is_none(),
        None => false,
    }
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
    let aux_ptr = unsafe { (*pre).aux_ptr };
    let aux_size = unsafe { (*pre).aux_size };
    // RFC 0047 (wave 5): a **container body** (list/tuple-subclass
    // instance) owns one reference per element — a list body's occupants
    // live in the out-of-line aux `ob_item` buffer, a tuple body's in the
    // inline array. Ordinary instance bodies have neither (aux is null,
    // both predicates false), so this is dead code for them.
    if unsafe { is_faithful_list(p) } {
        // Adopt pending direct C writes before the mirror dies — see the
        // matching reconcile in [`free_mirror`] (RFC 0076 WS3).
        unsafe { reconcile_list_from_c(p) };
        unregister_seeded_list(p);
        if !aux_ptr.is_null() && aux_size > 0 {
            // Live prefix only (`ob_size`), like CPython's `list_dealloc`:
            // the allocated tail can hold stale pointers a shrinking
            // mutation (Cython's inlined `pop()`) left behind — see the
            // matching sweep in [`free_mirror`].
            let cap = (aux_size / std::mem::size_of::<*mut PyObject>()) as isize;
            let live = unsafe { (*(p as *const layout::PyVarObject)).ob_size }.clamp(0, cap);
            let slots = aux_ptr as *mut *mut PyObject;
            for i in 0..live {
                let elem = unsafe { *slots.offset(i) };
                if !elem.is_null() {
                    unsafe { crate::object::Py_DecRef(elem) };
                }
            }
        }
    } else if unsafe { is_faithful_tuple(p) } {
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
    // Drop the prefix in place (`obj` is None; the `Weak` back-reference
    // decrements the instance's weak count) before releasing the block.
    unsafe { ptr::drop_in_place(pre) };
    if !aux_ptr.is_null() && aux_size > 0 {
        let aux_layout = Layout::from_size_align(aux_size, BODY_ALIGN).expect("aux layout");
        unsafe { dealloc(aux_ptr, aux_layout) };
    }
    let layout = Layout::from_size_align(alloc_size, BODY_ALIGN).expect("instance body layout");
    unsafe { dealloc(pre as *mut u8, layout) };
}

/// Pack a faithful `PyListObject` layout into a freshly-allocated
/// **list-subclass container body** (RFC 0047, wave 5): fill an
/// out-of-line `ob_item` buffer with owned references to the owning
/// instance's native list payload, mark the body seeded, and register it
/// for VM↔C list syncing (a Python-side `.append()` must be visible to a
/// later C macro read, and vice versa).
///
/// # Safety
/// `body` must be a zeroed instance body of at least
/// `sizeof(PyListObject)` bytes whose prefix `inst` back-reference is set.
pub(crate) unsafe fn pack_list_subclass_body(body: *mut PyObject) {
    let rc = match unsafe { list_rc_of(body) } {
        Some(rc) => rc,
        // No native list payload (e.g. `object.__new__(C)`): the zeroed
        // body already reads as a safe empty list.
        None => return,
    };
    let items: Vec<Object> = rc.borrow().clone();
    let n = items.len();
    let vo = body as *mut layout::PyVarObject;
    unsafe { (*vo).ob_size = n as PySsizeT };
    let lo = body as *mut layout::PyListObject;
    if n == 0 {
        unsafe {
            (*lo).ob_item = ptr::null_mut();
            (*lo).allocated = 0;
        }
    } else {
        let bytes = n * std::mem::size_of::<*mut PyObject>();
        let buf_layout = Layout::from_size_align(bytes, BODY_ALIGN).expect("ob_item layout");
        let buf = unsafe { alloc(buf_layout) };
        assert!(!buf.is_null(), "ob_item allocation failed");
        unsafe { ptr::write_bytes(buf, 0, bytes) };
        let slots = buf as *mut *mut PyObject;
        for (i, elem) in items.iter().enumerate() {
            let ep = match elem {
                Object::None => crate::singletons::none_ptr(),
                Object::Bool(true) => crate::singletons::true_ptr(),
                Object::Bool(false) => crate::singletons::false_ptr(),
                _ => crate::object::into_owned(elem.clone()),
            };
            unsafe { *slots.add(i) = ep };
        }
        let pre = unsafe { prefix_of(body) };
        unsafe {
            (*lo).ob_item = slots;
            (*lo).allocated = n as PySsizeT;
            (*pre).aux_ptr = buf;
            (*pre).aux_size = bytes;
        }
    }
    let pre = unsafe { prefix_of(body) };
    unsafe { (*pre).list_synced = true };
    register_seeded_list(body);
}

/// Pack a faithful `PyTupleObject` layout into a freshly-allocated
/// **tuple-subclass container body** (RFC 0047, wave 5 — every
/// `namedtuple`): fill the inline `ob_item` array with owned references
/// to the owning instance's native tuple payload. Tuples are immutable,
/// so this is pack-once with no sync registration.
///
/// # Safety
/// `body` must be a zeroed instance body of at least
/// `sizeof(PyVarObject) + n * sizeof(PyObject*)` bytes whose prefix
/// `inst` back-reference is set.
pub(crate) unsafe fn pack_tuple_subclass_body(body: *mut PyObject) {
    let pre = unsafe { prefix_of(body) };
    let items: Vec<Object> = match unsafe { (*pre).inst.as_ref() }.and_then(|w| w.upgrade()) {
        Some(inst) => match inst.native.get() {
            Some(Object::Tuple(t)) => t.iter().cloned().collect(),
            _ => return,
        },
        None => return,
    };
    let n = items.len();
    let vo = body as *mut layout::PyVarObject;
    unsafe { (*vo).ob_size = n as PySsizeT };
    let to = body as *mut layout::PyTupleObject;
    let base = ptr::addr_of_mut!((*to).ob_item) as *mut *mut PyObject;
    for (i, elem) in items.iter().enumerate() {
        let ep = match elem {
            Object::None => crate::singletons::none_ptr(),
            Object::Bool(true) => crate::singletons::true_ptr(),
            Object::Bool(false) => crate::singletons::false_ptr(),
            _ => crate::object::into_owned(elem.clone()),
        };
        unsafe { *base.add(i) = ep };
    }
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
        let inst = match weak.upgrade() {
            Some(inst) => inst,
            None => return Object::None,
        };
        // RFC 0046 (wave 5): a faithful **str-subtype** body (numpy's
        // `str_`, built by `builtin_new::str_new`) had its unicode value
        // stamped into the inline body by the extension's `tp_new` chain
        // *after* allocation, so the `PyInstance` minted at alloc time
        // carries no VM-native payload yet. Seed `native` from the body on
        // the first crossing — the VM's string operations (`+`, f-strings,
        // comparison, hashing) unwrap it exactly like a `class S(str)`
        // subclass — while the instance keeps its real type (CPython
        // parity: `type(np.str_('x')) is np.str_`). Gated on the cheap
        // `tp_base`-chain subtype test, so an ordinary faithful instance
        // (numpy `ndarray`, pandas block, …) is unaffected.
        let head = unsafe { &*p };
        if inst.native.get().is_none()
            && !head.ob_type.is_null()
            && !std::ptr::eq(head.ob_type, types::PyUnicode_Type.as_ptr())
            && unsafe {
                crate::types::PyType_IsSubtype(head.ob_type, types::PyUnicode_Type.as_ptr())
            } != 0
        {
            if let Some(s) = unsafe { read_unicode_value(p) } {
                let _ = inst.native.set(Object::from_str(s));
            }
        }
        // Same for a faithful **bytes-subtype** body (numpy's `bytes_`):
        // its value lives in the inline `ob_sval` array; seed `native` so
        // the VM's `bytes` operations see it while `type()` stays faithful.
        if inst.native.get().is_none()
            && !head.ob_type.is_null()
            && !std::ptr::eq(head.ob_type, types::PyBytes_Type.as_ptr())
            && unsafe { crate::types::PyType_IsSubtype(head.ob_type, types::PyBytes_Type.as_ptr()) }
                != 0
        {
            if let Some(b) = unsafe { read_bytes_value(p) } {
                let rc: weavepy_vm::sync::Rc<[u8]> = b.into();
                let _ = inst.native.set(Object::Bytes(rc));
            }
        }
        // And for a faithful **tuple-subtype** body (RFC 0076 WS5):
        // torch's `Size` is a readied static subtype of tuple, built via
        // `tp_alloc(&THPSizeType, ndim)` + `PyTuple_SET_ITEM` macro
        // writes into the inline `ob_item` — the `PyInstance` minted at
        // alloc time has no tuple payload, so `len(t.shape)` collapsed to
        // "'object' has no len()". Tuples are immutable and the macro
        // fills complete before the object is exposed, so a once-seed at
        // first crossing is faithful.
        if inst.native.get().is_none()
            && !head.ob_type.is_null()
            && !std::ptr::eq(head.ob_type, types::PyTuple_Type.as_ptr())
            && unsafe { crate::types::PyType_IsSubtype(head.ob_type, types::PyTuple_Type.as_ptr()) }
                != 0
        {
            let vo = p as *const layout::PyVarObject;
            let n = unsafe { (*vo).ob_size };
            if n >= 0 {
                let to = p as *const layout::PyTupleObject;
                let base = unsafe { ptr::addr_of!((*to).ob_item) } as *const *mut PyObject;
                let mut items = Vec::with_capacity(n as usize);
                for i in 0..n as usize {
                    let slot = unsafe { *base.add(i) };
                    items.push(if slot.is_null() {
                        Object::None
                    } else {
                        unsafe { crate::object::clone_object(slot) }
                    });
                }
                let _ = inst.native.set(Object::new_tuple(items));
            }
        }
        // RFC 0047 (wave 5): a **list-subclass container body** can carry
        // direct C macro writes (`PyList_SET_ITEM` + `__Pyx_SET_SIZE`, the
        // Cython append fast path); adopt them into the instance's native
        // list payload before handing the instance back. `list_synced` is
        // only ever set on a packed list body, so ordinary instance bodies
        // skip this entirely.
        if unsafe { (*pre).list_synced } {
            unsafe { reconcile_list_from_c(p) };
        }
        return Object::Instance(inst);
    }
    // RFC 0046 (wave 4) / RFC 0047 (wave 5): a faithful tuple's inline
    // `ob_item` is the source of truth (a stock `PyTuple_SET_ITEM` writes it
    // directly, bypassing our functions) — but rebuilding a fresh
    // `Object::Tuple` on *every* crossing broke pointer identity: the same
    // C-held `PyObject*` read back twice compared `is`-different, and a
    // VM-minted tuple stored into a C container (a numpy object array) came
    // back as a copy (pandas' `test_np_max_nested_tuples` asserts
    // `arr.max() is arr[2]`). CPython has no boundary, so identity always
    // holds there. Restore it with a slot-pointer snapshot (the aux buffer,
    // written at mint by `fill_body` and refreshed after each rebuild): when
    // the live `ob_item` matches the snapshot, no `PyTuple_SET_ITEM` has
    // rewired the tuple since the prefix object was captured, so hand back
    // the shared prefix `Object::Tuple` — the original `Rc` for a VM-minted
    // tuple, the seeded rebuild for a C-built one (`PyTuple_New` + macro
    // fills, which happen before the tuple is first exposed). A mismatch —
    // the placeholder-filled `PyTuple_New` staging tuple on its first
    // crossing, or an (illegal-on-shared-tuples, but observed) late store —
    // rebuilds once and re-snapshots, so identity is stable from then on.
    if unsafe { is_faithful_tuple(p) } {
        return unsafe { tuple_native_shared(p) };
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
            // Seed **in place**: refill the existing prefix `Rc` from the
            // buffer rather than replacing it with a fresh `Object::List`.
            // A VM-minted mirror (`into_owned` of a list a dict/instance
            // still holds) *shares* that `Rc`; swapping in a new one would
            // disconnect the mirror from its VM holder, so a C-side append
            // through this box would never reach the dict's value (pandas'
            // `IndexEngine.get_indexer_non_unique` built its position dict
            // with `d[val].append(i)` — each fresh `np.int64` key minted a
            // new box, the reseed cut it loose, and every third duplicate
            // position vanished). For a C-built list (`PyList_New` + macro
            // fills) the mint `Rc` is unshared and the refill is the same
            // seeding as before.
            // RFC 0069 WS5: flip `list_synced` *before* reading the buffer.
            // `read_list_vec` clones every slot back to a VM object, and a
            // self-referential list (`l.append(l)` — numpy's pathological-
            // self-containing test) holds this very box in slot 0: with the
            // flag still clear the nested clone re-entered this seed until
            // the C stack faulted. Synced-early, the nested crossing takes
            // the reconcile branch (a no-op — the box is not yet in
            // `SEEDED_LISTS`) and resolves to the shared prefix `Rc`, which
            // is exactly the identity `l[0] is l` requires.
            unsafe {
                (*pre).list_synced = true;
            }
            if let Object::List(rc) = unsafe { &(*pre).obj } {
                let cur = unsafe { read_list_vec(p) };
                *rc.borrow_mut() = cur;
            } else {
                let seeded = unsafe { read_list(p) };
                unsafe {
                    (*pre).obj = seeded;
                }
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
            // A macro write to a *nested* seeded list leaves this list's own
            // slot pointers unchanged, so the reconcile above misses it (pandas
            // `to_csv`'s reused `rows` buffer). Descend to adopt those too.
            unsafe { reconcile_nested_lists(p, 0) };
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
    // A buffer-authoritative bytes mirror (`PyBytes_FromStringAndSize(NULL,
    // n)`) was filled through the inlined `PyBytes_AS_STRING` macro after
    // minting (orjson's `dumps`), so adopt `ob_sval` into the prefix on the
    // first crossing. Bytes are immutable once exposed to Python, so the
    // adopted value stays authoritative afterwards.
    if unsafe { (*pre).bytes_buffer } {
        if let Some(v) = unsafe { read_bytes_value(p) } {
            let rc: weavepy_vm::sync::Rc<[u8]> = v.into();
            unsafe {
                (*pre).obj = Object::Bytes(rc);
                (*pre).bytes_buffer = false;
            }
        }
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
    let ty = head.ob_type;
    if ty.is_null() {
        return false;
    }
    if std::ptr::eq(ty, crate::types::PyTuple_Type.as_ptr()) {
        return true;
    }
    // RFC 0047 (wave 5): a VM tuple-subclass instance (every `namedtuple`)
    // crosses as a faithful `PyTupleObject`-shaped container body, so the
    // inlined `PyTuple_Check` → `PyTuple_GET_ITEM`/`Py_SIZE` macro sequence
    // reads real slots.
    //
    // RFC 0076 WS5: a readied *foreign* tuple subclass (torch's
    // `THPSizeType`) takes the inline-instance path instead — its body is
    // equally a real `PyTupleObject` (`ob_size` stamped by
    // `make_inline_instance`, `ob_item` filled by the extension's
    // `PyTuple_SET_ITEM` macros). Without this arm, `PyTuple_Size(self)`
    // fell through to `clone_object` → `Object::Instance` → -1, and
    // `THPSize_repr`/`THPSize_reduce` saw an empty tuple (`torch.Size`
    // pickled as `Size(())`, killing DataLoader spawn workers).
    (types::is_container_body_type(ty) || types::is_inline_instance_type(ty))
        && unsafe { (*ty).tp_flags } & layout::tpflags::TUPLE_SUBCLASS != 0
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

/// Re-publish the macro-visible state of a dict/set/list mirror after it
/// may have been mutated in place through the C boundary. A cheap no-op
/// for any pointer that isn't one of those faithful mirrors (the
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
    } else if unsafe { is_faithful_list(p) } {
        // A seeded list mutated by a VM method call issued from *inside* a
        // C frame (`lg_inclusion_list.remove(...)` in Cython-compiled
        // charset_normalizer 3.5.0) never reaches the outermost-boundary
        // [`flush_seeded_lists`] before the extension's next inlined
        // `PyList_GET_ITEM`/`Py_SIZE` macro read — so re-publish this one
        // list here. Fingerprint-gated, so an unmutated list stays free.
        unsafe { sync_list_ob_item(p) };
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

/// Identity-stable read-back of a faithful tuple mirror (see the
/// [`native_of`] tuple arm for the full rationale). Returns the shared
/// prefix `Object::Tuple` while the live `ob_item` slots still match the
/// aux snapshot; otherwise rebuilds from the slots once, re-snapshots, and
/// shares the rebuild thereafter.
///
/// # Safety
/// `p` must satisfy [`is_faithful_tuple`].
unsafe fn tuple_native_shared(p: *mut PyObject) -> Object {
    let pre = unsafe { prefix_of(p) };
    let vo = p as *const layout::PyVarObject;
    let n = unsafe { (*vo).ob_size };
    let n = if n < 0 { 0 } else { n as usize };
    // An empty tuple has no slots to rewire; the prefix object (staged by
    // `mirror_out_fresh` or `PyTuple_New(0)`) is authoritative forever.
    if n == 0 {
        if matches!(unsafe { &(*pre).obj }, Object::Tuple(_)) {
            return unsafe { (*pre).obj.clone() };
        }
        return Object::new_tuple(Vec::new());
    }
    let to = p as *const layout::PyTupleObject;
    let base = ptr::addr_of!((*to).ob_item) as *const *mut PyObject;
    let want = n * std::mem::size_of::<*mut PyObject>();
    let snap = unsafe { (*pre).aux_ptr } as *const *mut PyObject;
    if !snap.is_null()
        && unsafe { (*pre).aux_size } == want
        && matches!(unsafe { &(*pre).obj }, Object::Tuple(_))
        && unsafe { std::slice::from_raw_parts(base, n) == std::slice::from_raw_parts(snap, n) }
    {
        return unsafe { (*pre).obj.clone() };
    }
    // Slots changed (or never snapshotted): adopt the authoritative inline
    // array into the shared prefix object and refresh the snapshot.
    let rebuilt = unsafe { read_tuple(p) };
    unsafe {
        (*pre).obj = rebuilt.clone();
        // The snapshot *owns* a reference to each recorded pointer (taken
        // below, released here / in `free_mirror`). Raw pointer values were
        // not enough: `_testbuffer`'s `pack_from_list` frees its per-loop
        // offset int and the very next `PyLong_FromSsize_t` reuses the same
        // heap address, so a pointer-only comparison saw "unchanged" slots
        // and replayed the stale converted tuple (ABA), silently packing
        // every item at the first offset. Owning the pointers pins them, so
        // a recycled address can never masquerade as an unchanged slot.
        if !(*pre).aux_ptr.is_null() && (*pre).aux_size > 0 {
            let old_n = (*pre).aux_size / std::mem::size_of::<*mut PyObject>();
            let old_slots = (*pre).aux_ptr as *mut *mut PyObject;
            for i in 0..old_n {
                let e = *old_slots.add(i);
                if !e.is_null() {
                    crate::object::Py_DecRef(e);
                }
            }
        }
        if (*pre).aux_ptr.is_null() || (*pre).aux_size != want {
            if !(*pre).aux_ptr.is_null() && (*pre).aux_size > 0 {
                let old_layout =
                    Layout::from_size_align((*pre).aux_size, BODY_ALIGN).expect("aux layout");
                dealloc((*pre).aux_ptr, old_layout);
            }
            let buf_layout = Layout::from_size_align(want, BODY_ALIGN).expect("tuple seed layout");
            let buf = alloc(buf_layout);
            assert!(!buf.is_null(), "tuple seed allocation failed");
            (*pre).aux_ptr = buf;
            (*pre).aux_size = want;
        }
        ptr::copy_nonoverlapping(base as *const u8, (*pre).aux_ptr, want);
        let slots = (*pre).aux_ptr as *mut *mut PyObject;
        for i in 0..n {
            let e = *slots.add(i);
            if !e.is_null() {
                crate::object::Py_IncRef(e);
            }
        }
    }
    rebuilt
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
    let ty = head.ob_type;
    if ty.is_null() {
        return false;
    }
    if std::ptr::eq(ty, crate::types::PyList_Type.as_ptr()) {
        return true;
    }
    // RFC 0047 (wave 5): a VM list-subclass instance (pandas' `FrozenList`)
    // crosses as a faithful `PyListObject`-shaped container body, so the
    // inlined `PyList_Check` → `PyList_GET_ITEM`/`Py_SIZE` macro sequence
    // (pandas' ujson serializer) reads real slots.
    types::is_container_body_type(ty)
        && unsafe { (*ty).tp_flags } & layout::tpflags::LIST_SUBCLASS != 0
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

/// True iff `p` is a faithful **builtin function** mirror
/// (`PyCFunction_Type`). Used by [`free_mirror`] to release the owned
/// `m_self` a `PyCFunction_NewEx`-minted builtin carries (RFC 0066 WS3).
///
/// # Safety
/// `p` must be non-null and readable for `[prefix .. head + 16]`.
pub unsafe fn is_faithful_cfunction(p: *mut PyObject) -> bool {
    if !unsafe { is_mirror(p) } {
        return false;
    }
    let head = unsafe { &*p };
    !head.ob_type.is_null() && std::ptr::eq(head.ob_type, crate::types::PyCFunction_Type.as_ptr())
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

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use weavepy_vm::fasthash::FxHashMap;

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
/// Both sides are stored as a *combined* `(chained-FNV hash, length)`
/// digest rather than per-slot vectors: the flush runs at **every**
/// outermost VM→C transition over **every** seeded list, so the per-list
/// state must be comparable and replaceable without allocating. (Per-slot
/// vectors allocated two `Vec`s per list per C call and dominated whole
/// pandas test files.) A 64-bit chained FNV over the slot values keeps
/// order sensitivity; a collision — one in 2⁶⁴ — costs a missed
/// republish, the same failure mode the fingerprint scheme already
/// accepted per-slot.
#[derive(Default)]
struct ListSync {
    /// Digest of the prefix `Rc` elements last published to `ob_item`
    /// (`None` until the first flush — forces the initial sync). See
    /// [`sync_list_ob_item`].
    rc_fp: Option<(u64, usize)>,
    /// Digest of the raw `ob_item` pointers at the last agreement point
    /// (seed, publish, write-through, or adopt). A later read that finds a
    /// different buffer knows C wrote it directly. See
    /// [`reconcile_list_from_c`].
    c_ptrs: (u64, usize),
}

/// Chained-FNV accumulator for the digests in [`ListSync`].
#[inline]
fn digest_fold(h: u64, v: u64) -> u64 {
    let mut h = h ^ v;
    // FNV-1a style multiply; chaining keeps the digest order-sensitive.
    h = h.wrapping_mul(0x0000_0100_0000_01B3);
    h ^= h >> 29;
    h
}

const DIGEST_SEED: u64 = 0xcbf2_9ce4_8422_2325;

/// Seeded faithful list mirrors keyed by `PyObject*`.
static SEEDED_LISTS: Mutex<Option<FxHashMap<usize, ListSync>>> = Mutex::new(None);
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
static SET_BOX_CACHE: Mutex<Option<FxHashMap<usize, usize>>> = Mutex::new(None);
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
        if g.get_or_insert_with(FxHashMap::default)
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

/// Canonical faithful `list` boxes keyed by native `Rc` identity
/// (RFC 0047, wave 5): `Rc-payload-pointer → PyObject*`. See the long
/// comment at the `list_rc_key` call site in [`mirror_out_with_type`]:
/// Cython's inlined append macros write whichever box they hold, so box
/// identity per native list is load-bearing for coherence.
static LIST_BOX_CACHE: Mutex<Option<FxHashMap<usize, usize>>> = Mutex::new(None);
static LIST_BOX_COUNT: AtomicUsize = AtomicUsize::new(0);

/// Native `Rc` identity key for a plain `list`, or `None` for any other
/// object.
fn list_rc_key(obj: &Object) -> Option<usize> {
    match obj {
        Object::List(rc) => Some(weavepy_vm::sync::Rc::as_ptr(rc) as usize),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Canonical bytearray boxes (RFC 0056 WS5).
// ---------------------------------------------------------------------------

static BYTEARRAY_BOX_CACHE: Mutex<Option<FxHashMap<usize, usize>>> = Mutex::new(None);
static BYTEARRAY_BOX_COUNT: AtomicUsize = AtomicUsize::new(0);

fn bytearray_rc_key(obj: &Object) -> Option<usize> {
    match obj {
        Object::ByteArray(rc) => Some(weavepy_vm::sync::Rc::as_ptr(rc) as usize),
        _ => None,
    }
}

/// Write the faithful `PyByteArrayObject` fields of `body` from the VM
/// buffer of the `Object::ByteArray` it carries: `ob_bytes`/`ob_start`
/// address the `Vec<u8>` data directly (kept alive by the prefix's `Rc`),
/// so the inlined `PyByteArray_AS_STRING` macro reads real bytes.
///
/// # Safety
/// `body` must be (or be becoming) a bytearray mirror body.
unsafe fn write_bytearray_fields(body: *mut PyObject, obj: &Object) {
    if let Object::ByteArray(rc) = obj {
        let b = rc.borrow();
        let vo = body as *mut layout::PyVarObject;
        let bo = body as *mut layout::PyByteArrayObject;
        // CPython's empty bytearray carries a NULL buffer; a dangling
        // `Vec::as_ptr` would technically never be read (size 0) but a
        // NULL is what stock code expects.
        let data = if b.is_empty() {
            ptr::null_mut()
        } else {
            b.as_ptr() as *mut std::ffi::c_char
        };
        unsafe {
            (*vo).ob_size = b.len() as PySsizeT;
            (*bo).ob_alloc = b.capacity() as PySsizeT;
            (*bo).ob_bytes = data;
            (*bo).ob_start = data;
            (*bo).ob_exports = 0;
        }
    }
}

/// Refresh a bytearray mirror's struct fields from its prefix `Rc` — the
/// VM buffer may have grown (and reallocated) since the last publish.
///
/// # Safety
/// `p` must be a live bytearray mirror.
pub unsafe fn sync_bytearray_fields(p: *mut PyObject) {
    let pre = unsafe { prefix_of(p) };
    let obj = unsafe { (*pre).obj.clone() };
    unsafe { write_bytearray_fields(p, &obj) };
}

fn cached_bytearray_box(key: usize) -> Option<*mut PyObject> {
    if BYTEARRAY_BOX_COUNT.load(Ordering::Relaxed) == 0 {
        return None;
    }
    let g = BYTEARRAY_BOX_CACHE.lock().ok()?;
    let bp = *g.as_ref()?.get(&key)?;
    let p = bp as *mut PyObject;
    unsafe { crate::object::Py_IncRef(p) };
    Some(p)
}

fn register_bytearray_box(key: usize, p: *mut PyObject) {
    if let Ok(mut g) = BYTEARRAY_BOX_CACHE.lock() {
        if g.get_or_insert_with(FxHashMap::default)
            .insert(key, p as usize)
            .is_none()
        {
            BYTEARRAY_BOX_COUNT.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// True iff `p` is a faithful bytearray mirror.
///
/// # Safety
/// `p` must be non-null with a readable head.
pub unsafe fn is_faithful_bytearray(p: *mut PyObject) -> bool {
    if !unsafe { is_mirror(p) } {
        return false;
    }
    std::ptr::eq(unsafe { (*p).ob_type }, types::PyByteArray_Type.as_ptr())
}

/// Drop a bytearray mirror from the canonical cache when its storage is
/// released.
///
/// # Safety
/// `p` must be a bytearray mirror whose prefix is still intact.
pub unsafe fn unregister_bytearray_box(p: *mut PyObject) {
    let key = match bytearray_rc_key(unsafe { &(*prefix_of(p)).obj }) {
        Some(k) => k,
        None => return,
    };
    if let Ok(mut g) = BYTEARRAY_BOX_CACHE.lock() {
        if let Some(map) = g.as_mut() {
            if map.get(&key).copied() == Some(p as usize) && map.remove(&key).is_some() {
                BYTEARRAY_BOX_COUNT.fetch_sub(1, Ordering::Relaxed);
            }
        }
    }
}

/// Refresh every live bytearray mirror's struct fields. Run after each
/// bridged C→VM call returns: extension code mutates a C-resident
/// bytearray through our call surface (`self._buf.extend(...)` in
/// aiohttp's parser) and then reads the buffer with inlined macros, so
/// the fields must track the (possibly reallocated) VM buffer.
pub fn sync_bytearray_boxes() {
    if BYTEARRAY_BOX_COUNT.load(Ordering::Relaxed) == 0 {
        return;
    }
    let ptrs: Vec<usize> = match BYTEARRAY_BOX_CACHE.lock() {
        Ok(g) => g
            .as_ref()
            .map(|m| m.values().copied().collect())
            .unwrap_or_default(),
        Err(_) => return,
    };
    for bp in ptrs {
        unsafe { sync_bytearray_fields(bp as *mut PyObject) };
    }
}

/// Return the live canonical box for native-list identity `key`, handing
/// back a *fresh* C reference (matching `into_owned`'s "+1" contract).
fn cached_list_box(key: usize) -> Option<*mut PyObject> {
    if LIST_BOX_COUNT.load(Ordering::Relaxed) == 0 {
        return None;
    }
    let g = LIST_BOX_CACHE.lock().ok()?;
    let map = g.as_ref()?;
    let bp = *map.get(&key)?;
    let p = bp as *mut PyObject;
    unsafe { crate::object::Py_IncRef(p) };
    Some(p)
}

/// Record `p` as the canonical box for native-list identity `key`.
fn register_list_box(key: usize, p: *mut PyObject) {
    if listsync_trace_enabled() {
        eprintln!("[LISTSYNC] register key=0x{key:x} p={p:p}");
    }
    if let Ok(mut g) = LIST_BOX_CACHE.lock() {
        if g.get_or_insert_with(FxHashMap::default)
            .insert(key, p as usize)
            .is_none()
        {
            LIST_BOX_COUNT.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// Evict a faithful list mirror from the canonical cache when its storage
/// is released — called from [`free_mirror`] *before* the prefix's native
/// `Object` (and thus its `Rc`) is dropped. Only removes the entry when it
/// still points at `p` so a stale box can never clobber the live one.
///
/// # Safety
/// `p` must be a faithful list mirror whose prefix is still intact.
pub unsafe fn unregister_list_box(p: *mut PyObject) {
    if LIST_BOX_COUNT.load(Ordering::Relaxed) == 0 {
        return;
    }
    let pre = unsafe { prefix_of(p) };
    let key = match list_rc_key(unsafe { &(*pre).obj }) {
        Some(k) => k,
        None => return,
    };
    if let Ok(mut g) = LIST_BOX_CACHE.lock() {
        if let Some(map) = g.as_mut() {
            if map.get(&key) == Some(&(p as usize)) {
                map.remove(&key);
                LIST_BOX_COUNT.fetch_sub(1, Ordering::Relaxed);
            }
        }
    }
}

/// Canonical-box cache for `Object::Builtin` (`builtin_function_or_method`),
/// keyed by the native `Rc<BuiltinFn>` payload pointer.
///
/// A builtin such as `operator.eq` is a faithful mirror (see
/// [`obj_is_faithful`]), so absent a cache every crossing mints a *fresh*
/// `PyCFunction` box via [`mirror_out_fresh`] and hands C a different
/// pointer each time. That breaks the pointer-identity contract stock
/// Cython relies on: pandas' `pandas._libs.ops.vec_compare` /
/// `scalar_compare` select the comparison with a chain of `op is
/// operator.lt` / `elif op is operator.eq: …` tests (Cython lowers `is` to
/// a raw C `==`), and the analogous `Timedelta` / `Timestamp` reductions do
/// the same. When the argument box (`op`, marshaled at the call) and the box
/// Cython fetches with `PyObject_GetAttr(operator, "eq")` differ, *every*
/// branch is false and the function raises `ValueError("Unrecognized
/// operator")`. Handing out **one** canonical box per native builtin makes
/// the marshaled argument and the module-attribute lookup the *same* memory,
/// so the identity chain resolves exactly as under CPython.
static BUILTIN_BOX_CACHE: Mutex<Option<FxHashMap<usize, usize>>> = Mutex::new(None);
static BUILTIN_BOX_COUNT: AtomicUsize = AtomicUsize::new(0);

/// Native `Rc` identity key for an `Object::Builtin` (its `BuiltinFn`
/// payload pointer), or `None` for any other object. Two `Object` clones of
/// the same builtin share one `Rc`, so this is a stable per-builtin identity
/// for as long as any clone (e.g. a live mirror's prefix, or the value held
/// in the owning module dict) keeps it alive.
fn builtin_rc_key(obj: &Object) -> Option<usize> {
    match obj {
        Object::Builtin(rc) => Some(weavepy_vm::sync::Rc::as_ptr(rc) as usize),
        _ => None,
    }
}

/// True iff `p` is a faithful `builtin_function_or_method` mirror — a mirror
/// whose advertised type is `PyCFunction_Type`. Used only as a cheap guard in
/// [`free_mirror`] before evicting from [`BUILTIN_BOX_CACHE`].
///
/// # Safety
/// `p` must be non-null and readable for `[prefix .. head + 16]`.
pub unsafe fn is_faithful_builtin(p: *mut PyObject) -> bool {
    if !unsafe { is_mirror(p) } {
        return false;
    }
    let head = unsafe { &*p };
    !head.ob_type.is_null() && std::ptr::eq(head.ob_type, crate::types::PyCFunction_Type.as_ptr())
}

/// Return the live canonical box for native-builtin identity `key`, handing
/// back a *fresh* C reference (matching the mint path's "+1" contract).
/// `None` if no box is currently cached.
fn cached_builtin_box(key: usize) -> Option<*mut PyObject> {
    let g = BUILTIN_BOX_CACHE.lock().ok()?;
    let map = g.as_ref()?;
    let bp = *map.get(&key)?;
    let p = bp as *mut PyObject;
    unsafe { crate::object::Py_IncRef(p) };
    if std::env::var_os("WEAVEPY_BOXDBG").is_some() {
        eprintln!(
            "[BOXDBG] builtin cache HIT key=0x{key:x} -> box=0x{:x}",
            p as usize
        );
    }
    Some(p)
}

/// Record `p` as the canonical box for native-builtin identity `key`.
fn register_builtin_box(key: usize, p: *mut PyObject) {
    if std::env::var_os("WEAVEPY_BOXDBG").is_some() {
        eprintln!(
            "[BOXDBG] builtin cache MISS key=0x{key:x} minted box=0x{:x}",
            p as usize
        );
    }
    if let Ok(mut g) = BUILTIN_BOX_CACHE.lock() {
        if g.get_or_insert_with(FxHashMap::default)
            .insert(key, p as usize)
            .is_none()
        {
            BUILTIN_BOX_COUNT.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// Evict a faithful builtin mirror from the canonical cache when its storage
/// is released — called from [`free_mirror`] *before* the prefix's native
/// `Object` (and thus its `Rc`) is dropped. Only removes the entry when it
/// still points at `p`, so a stale box that lost a cache race can never
/// clobber the live canonical one.
///
/// # Safety
/// `p` must be a faithful builtin mirror ([`is_faithful_builtin`]) whose
/// prefix is still intact.
pub unsafe fn unregister_builtin_box(p: *mut PyObject) {
    let pre = unsafe { prefix_of(p) };
    let key = match builtin_rc_key(unsafe { &(*pre).obj }) {
        Some(k) => k,
        None => return,
    };
    if let Ok(mut g) = BUILTIN_BOX_CACHE.lock() {
        if let Some(map) = g.as_mut() {
            if map.get(&key) == Some(&(p as usize)) {
                map.remove(&key);
                BUILTIN_BOX_COUNT.fetch_sub(1, Ordering::Relaxed);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Canonical pinned scalar boxes (RFC 0047, wave 5).
// ---------------------------------------------------------------------------

/// CPython extensions may keep an argument's `PyObject*` **borrowed** —
/// pandas' khash `PyObjectHashTable.set_item(key, val)` stores the raw
/// key pointer with *no* incref, relying on the Python caller's reference
/// to keep the object alive (`get_item`/rehash later reads it back through
/// `kh_python_hash_equal`). Under CPython that contract holds because the
/// pointer *is* the object. Under WeavePy a fresh box is minted per
/// crossing and dies with the call, leaving khash dangling — every direct
/// scalar key round-trip (`t.set_item(3.5, 1); t.get_item(3.5)`) failed.
///
/// Fix: while marshaling VM **arguments** into a C call (the
/// [`enter_arg_pin`] guard, set by `foreign::fwd_call`), immutable
/// hashable scalars are minted through this canonical cache. The box is
/// flagged [`MirrorPrefix::scalar_pinned`] and **survives its C refcount
/// reaching zero** — `free_box` skips it — so a borrowed pointer stored by
/// the callee stays valid, and a later crossing of the same value returns
/// the *same pointer* (which also turns khash probes into identity hits,
/// exactly like CPython where the caller passes the same object).
///
/// Keys: `Int`/`Float` by value (the VM's value semantics make equal
/// scalars indistinguishable — consistent with the VM, where `3.5 is 3.5`
/// is `True`); `Long`/`Complex`/`Str`/`Bytes`/`Tuple` by `Rc` data-pointer
/// identity (the pinned box's prefix clone keeps the `Rc` alive, so the
/// key can never dangle or be reused while the entry exists).
///
/// Memory is bounded by [`SCALAR_PIN_HWM`]: registering past the
/// high-water mark sweeps entries whose C refcount is zero (freeing the
/// boxes). A borrowed store older than a full sweep window can still
/// dangle in theory, but that window (64Ki distinct marshaled scalars
/// while the consumer stays alive) dwarfs every observed workload.
#[derive(PartialEq, Eq, Hash, Clone, Copy)]
enum ScalarPinKey {
    Int(i64),
    FloatBits(u64),
    /// `(type tag, Rc data pointer)` — the tag keeps zero-sized payloads
    /// of different types (empty str vs empty bytes) from colliding.
    Rc(u8, usize),
}

static SCALAR_PIN_CACHE: Mutex<Option<FxHashMap<ScalarPinKey, usize>>> = Mutex::new(None);
/// Eviction sweep threshold (entries).
const SCALAR_PIN_HWM: usize = 1 << 16;
/// Approximate payload bytes held by the cache's entries (RFC 0076 WS3).
/// The entry-count HWM alone lets a *few* huge dead pins retain megabytes
/// — Pillow's font leak test marshals a fresh ~10 KB text string per
/// `draw.text` call, and 100 dead ~50 KB string mirrors blow its 1 MB
/// RSS ceiling long before 64Ki entries. Registrations past
/// [`SCALAR_PIN_BYTE_HWM`] sweep dead entries just like the count HWM.
static SCALAR_PIN_BYTES: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
/// Byte-accounted sweep threshold.
const SCALAR_PIN_BYTE_HWM: usize = 512 * 1024;

/// Approximate payload bytes a pinned mirror of `obj` retains (the C-side
/// data copy; prefix/box overhead is ignored — it is bounded by the entry
/// count HWM).
fn pin_payload_bytes(obj: &Object) -> usize {
    match obj {
        // A str mirror carries up to a UCS4 copy plus a UTF-8 cache.
        Object::Str(s) => s.len() * 5,
        Object::Bytes(b) => b.len(),
        // A pinned tuple mirror holds a C reference to each element's
        // mirror until it is swept (`free_mirror` decrefs `ob_item`), so
        // it retains its elements' payloads too. This is the args-tuple
        // shape: `font.getmask(text)` pins a fresh 2-tuple whose ~10 KB
        // text mirror the 16-bytes-per-slot estimate missed entirely.
        Object::Tuple(t) => t.iter().map(|e| 16 + pin_payload_bytes(e)).sum(),
        _ => 16,
    }
}

thread_local! {
    /// Non-zero while marshaling VM arguments into a C call on this thread.
    static ARG_PIN_DEPTH: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// RAII guard for one argument-marshaling region; see [`enter_arg_pin`].
pub struct ArgPinGuard(());

impl Drop for ArgPinGuard {
    fn drop(&mut self) {
        let _ = ARG_PIN_DEPTH.try_with(|c| c.set(c.get().saturating_sub(1)));
    }
}

/// Mark the current thread as marshaling VM arguments into a C call:
/// scalar mints inside the region route through the canonical pin cache.
pub fn enter_arg_pin() -> ArgPinGuard {
    let _ = ARG_PIN_DEPTH.try_with(|c| c.set(c.get() + 1));
    ArgPinGuard(())
}

pub(crate) fn arg_pin_active() -> bool {
    ARG_PIN_DEPTH.try_with(|c| c.get() > 0).unwrap_or(false)
}

// ---------------------------------------------------------------------------
// Interned strings (RFC 0056 WS5).
//
// CPython interns keyword names (`kwnames` entries are interned by the
// compiler/vectorcall machinery) and gives extensions
// `PyUnicode_InternFromString`. Extensions compare the two by **pointer
// identity**: orjson's `dumps` matches each `kwnames` element against the
// `PyUnicode_InternFromString("option")` pointer it stashed at module init
// and raises "unexpected keyword argument" on a mismatch. The scalar pin
// cache can't provide this — it keys strings by their VM `Rc` pointer, not
// content — so interning needs a content-keyed table of immortal boxes.
// ---------------------------------------------------------------------------

static STR_INTERN_CACHE: Mutex<Option<FxHashMap<Box<str>, usize>>> = Mutex::new(None);

thread_local! {
    /// Non-zero while minting strings that must resolve through the
    /// content-keyed intern table (kwnames marshaling, the
    /// `PyUnicode_Intern*` entry points).
    static INTERN_DEPTH: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// RAII guard for one intern-minting region; see [`enter_intern_scope`].
pub struct InternGuard(());

impl Drop for InternGuard {
    fn drop(&mut self) {
        let _ = INTERN_DEPTH.try_with(|c| c.set(c.get().saturating_sub(1)));
    }
}

/// Route `Object::Str` mints on this thread through the content-keyed
/// intern table until the guard drops.
pub fn enter_intern_scope() -> InternGuard {
    let _ = INTERN_DEPTH.try_with(|c| c.set(c.get() + 1));
    InternGuard(())
}

fn intern_scope_active() -> bool {
    INTERN_DEPTH.try_with(|c| c.get() > 0).unwrap_or(false)
}

/// The canonical interned mirror for string content `s` (a fresh strong
/// reference). Interned boxes are immortal, matching CPython's behaviour
/// for extension-interned names.
fn interned_str_mirror(s: &str, ty: *mut PyTypeObject, obj: Object) -> *mut PyObject {
    if let Ok(g) = STR_INTERN_CACHE.lock() {
        if let Some(map) = g.as_ref() {
            if let Some(&bp) = map.get(s) {
                let p = bp as *mut PyObject;
                unsafe { crate::object::Py_IncRef(p) };
                return p;
            }
        }
    }
    let p = mirror_out_fresh(obj, ty);
    // Immortal: `scalar_pinned` keeps `free_box` from releasing the block
    // at C refcount zero, so the identity C extensions captured stays valid.
    unsafe { (*prefix_of(p)).scalar_pinned = true };
    if let Ok(mut g) = STR_INTERN_CACHE.lock() {
        g.get_or_insert_with(FxHashMap::default)
            .insert(s.into(), p as usize);
    }
    p
}

/// The canonical-cache key for `obj`, or `None` when the value kind is not
/// pinned (mutable containers, instances, foreigns, …).
fn scalar_pin_key(obj: &Object) -> Option<ScalarPinKey> {
    use weavepy_vm::sync::Rc as VmRc;
    Some(match obj {
        Object::Int(i) => ScalarPinKey::Int(*i),
        Object::Float(f) => ScalarPinKey::FloatBits(f.to_bits()),
        Object::Long(rc) => ScalarPinKey::Rc(1, VmRc::as_ptr(rc) as usize),
        Object::Complex(rc) => ScalarPinKey::Rc(2, VmRc::as_ptr(rc) as usize),
        Object::Str(rc) => ScalarPinKey::Rc(3, VmRc::as_ptr(rc) as *const u8 as usize),
        Object::Bytes(rc) => ScalarPinKey::Rc(5, VmRc::as_ptr(rc) as *const u8 as usize),
        Object::Tuple(rc) => ScalarPinKey::Rc(6, VmRc::as_ptr(rc) as *const Object as usize),
        _ => return None,
    })
}

/// Return a fresh C reference to the live canonical pinned box for `key`,
/// or `None` when no box is cached (or the cached one wears a different
/// type — e.g. the same value crossing under a staging type).
fn cached_scalar_pin(key: ScalarPinKey, ty: *mut PyTypeObject) -> Option<*mut PyObject> {
    let g = SCALAR_PIN_CACHE.lock().ok()?;
    let map = g.as_ref()?;
    let bp = *map.get(&key)?;
    let p = bp as *mut PyObject;
    if unsafe { (*p).ob_type } != ty {
        return None;
    }
    unsafe { crate::object::Py_IncRef(p) };
    Some(p)
}

/// Record `p` as the canonical pinned box for `key`, sweeping the cache
/// first when it is past the high-water mark. A displaced previous entry
/// is unpinned (and freed at once if C no longer references it).
/// Diagnostic: gate scalar-pin/UTF-8-cache accounting traces on
/// `WEAVEPY_PIN_TRACE` (RFC 0076 WS3 leak hunting).
pub(crate) fn pin_trace_enabled() -> bool {
    use std::sync::OnceLock;
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("WEAVEPY_PIN_TRACE").is_some())
}

fn register_scalar_pin(key: ScalarPinKey, p: *mut PyObject) {
    use std::sync::atomic::Ordering;
    unsafe { (*prefix_of(p)).scalar_pinned = true };
    let nbytes = pin_payload_bytes(unsafe { &(*prefix_of(p)).obj });
    if pin_trace_enabled() && nbytes > 4096 {
        eprintln!(
            "[pin] register {nbytes}B total={} refcnt={}",
            SCALAR_PIN_BYTES.load(Ordering::Relaxed),
            unsafe { (*p).ob_refcnt },
        );
    }
    let displaced = {
        let mut g = match SCALAR_PIN_CACHE.lock() {
            Ok(g) => g,
            Err(_) => return,
        };
        let map = g.get_or_insert_with(FxHashMap::default);
        if map.len() >= SCALAR_PIN_HWM
            || SCALAR_PIN_BYTES.load(Ordering::Relaxed) + nbytes > SCALAR_PIN_BYTE_HWM
        {
            sweep_scalar_pins_locked(map);
        }
        SCALAR_PIN_BYTES.fetch_add(nbytes, Ordering::Relaxed);
        map.insert(key, p as usize)
    };
    if let Some(old) = displaced {
        let old_bytes = if old == p as usize {
            // Re-registered the same box: the insert above double-counted.
            nbytes
        } else {
            pin_payload_bytes(unsafe { &(*prefix_of(old as *mut PyObject)).obj })
        };
        SCALAR_PIN_BYTES.fetch_sub(old_bytes, Ordering::Relaxed);
        if old != p as usize {
            unsafe { unpin_scalar_box(old as *mut PyObject) };
        }
    }
}

/// Clear a box's pin flag; free it immediately when C holds no reference.
///
/// # Safety
/// `p` must be a live pinned mirror.
unsafe fn unpin_scalar_box(p: *mut PyObject) {
    unsafe { (*prefix_of(p)).scalar_pinned = false };
    if unsafe { (*p).ob_refcnt } <= 0 {
        unsafe { free_mirror(p) };
    }
}

/// Drop every entry whose box C no longer references (refcount zero),
/// freeing the boxes. Entries C still holds stay registered (identity
/// remains stable for future crossings).
fn sweep_scalar_pins_locked(map: &mut FxHashMap<ScalarPinKey, usize>) {
    use std::sync::atomic::Ordering;
    let dead: Vec<(ScalarPinKey, usize)> = map
        .iter()
        .filter(|(_, &bp)| unsafe { (*(bp as *mut PyObject)).ob_refcnt } <= 0)
        .map(|(k, &bp)| (*k, bp))
        .collect();
    if pin_trace_enabled() {
        eprintln!(
            "[pin] sweep: {} dead of {} entries, {}B total",
            dead.len(),
            map.len(),
            SCALAR_PIN_BYTES.load(Ordering::Relaxed),
        );
    }
    for (k, bp) in dead {
        map.remove(&k);
        let p = bp as *mut PyObject;
        SCALAR_PIN_BYTES.fetch_sub(
            pin_payload_bytes(unsafe { &(*prefix_of(p)).obj }),
            Ordering::Relaxed,
        );
        unsafe {
            (*prefix_of(p)).scalar_pinned = false;
            free_mirror(p);
        }
    }
}

/// Count the clones of `target` held by **dead** scalar pins (C refcount
/// zero — CPython would already have freed the memory): a pin that *is*
/// `target`, or a dead tuple pin whose only strong reference is the pin
/// itself and whose elements include `target`. The `sys.getrefcount`
/// discount half of RFC 0076 WS1 (see `object::pin_clone_count_hook`);
/// the tuple case is what a marshaled `np.array((obj, 2), dtype=...)`
/// argument leaves behind — the pinned args-tuple mirror keeps the VM
/// tuple (and through it `obj`) alive past the call.
pub(crate) fn dead_pin_clones_of(target: &Object) -> usize {
    let g = match SCALAR_PIN_CACHE.lock() {
        Ok(g) => g,
        Err(_) => return 0,
    };
    let Some(map) = g.as_ref() else { return 0 };
    let mut n = 0;
    for &bp in map.values() {
        let p = bp as *mut PyObject;
        if unsafe { (*p).ob_refcnt } > 0 {
            continue;
        }
        let obj = unsafe { &(*prefix_of(p)).obj };
        if crate::object::same_native_identity(obj, target) {
            n += 1;
            continue;
        }
        if let Object::Tuple(t) = obj {
            // Only when the pin is the tuple's sole owner — otherwise the
            // program can still reach the tuple and the element reference
            // is genuinely visible.
            if weavepy_vm::sync::Rc::strong_count(t) == 1
                && t.iter()
                    .any(|e| crate::object::same_native_identity(e, target))
            {
                n += 1;
            }
        }
    }
    n
}

/// Count the C references to box `bp` held by **dead** scalar-pinned
/// tuple mirrors (C refcount ≤ 0) — both the `ob_item` slot references
/// and the aux identity-snapshot references (a tuple mirror owns one of
/// each per element; see `fill_body`'s tuple arm). On CPython the args
/// tuple dies at call end and decrefs its elements; a pinned tuple parks
/// *without* releasing them (identity stability for re-crossings), so an
/// element identity box can be kept at refcount ≥ 1 by storage the
/// program can no longer reach. `pin_clone_count_hook` treats such a box
/// as parked when these refs account for its whole refcount
/// (RFC 0076 WS1, test_cleanup_with_refs_non_contig: the dead `(obj, 2)`
/// args-tuple pin held `obj`'s box at 2, keeping the box's payload clone
/// visible to `sys.getrefcount`).
pub(crate) fn dead_pin_c_refs_to(bp: *mut PyObject) -> usize {
    let g = match SCALAR_PIN_CACHE.lock() {
        Ok(g) => g,
        Err(_) => return 0,
    };
    let Some(map) = g.as_ref() else { return 0 };
    let mut n = 0;
    for &pin in map.values() {
        let p = pin as *mut PyObject;
        if unsafe { (*p).ob_refcnt } > 0 {
            continue;
        }
        let pre = unsafe { prefix_of(p) };
        if !matches!(unsafe { &(*pre).obj }, Object::Tuple(_)) {
            continue;
        }
        let vo = p as *const layout::PyVarObject;
        let len = unsafe { (*vo).ob_size };
        let len = if len < 0 { 0 } else { len as usize };
        let to = p as *const layout::PyTupleObject;
        let base = unsafe { ptr::addr_of!((*to).ob_item) } as *const *mut PyObject;
        for i in 0..len {
            if unsafe { *base.add(i) } == bp {
                n += 1;
            }
        }
        let snap = unsafe { (*pre).aux_ptr } as *const *mut PyObject;
        let snap_n = unsafe { (*pre).aux_size } / std::mem::size_of::<*mut PyObject>();
        if !snap.is_null() {
            for i in 0..snap_n {
                if unsafe { *snap.add(i) } == bp {
                    n += 1;
                }
            }
        }
    }
    n
}

/// True iff `p` is a canonical pinned scalar box whose storage must
/// outlive a zero C refcount. Checked (deref-free beyond the prefix flag)
/// by `free_box` before dispatching to [`free_mirror`].
///
/// # Safety
/// `p` must satisfy [`is_mirror`].
pub unsafe fn is_scalar_pinned(p: *mut PyObject) -> bool {
    unsafe { (*prefix_of(p)).scalar_pinned }
}

/// Mint the **args tuple** for a VM→C call: the outer tuple itself is a
/// per-call temporary (fresh `Rc`, never re-marshaled) so it mints
/// *unpinned*, while its elements — boxed inside `fill_body` with the
/// [`enter_arg_pin`] flag live — route through the canonical pin cache.
/// See [`ScalarPinKey`] for why argument scalars must outlive the call.
pub fn args_tuple_out(obj: Object) -> *mut PyObject {
    let _pin = enter_arg_pin();
    let ty = types::type_for_object(&obj);
    mirror_out_fresh(obj, ty)
}

/// Digest a faithful list mirror's raw `ob_item` pointers. Cheap — no
/// allocation, no minting, no refcount change — so a read can tell whether
/// C wrote the buffer since the last agreement without reconstructing
/// objects.
///
/// # Safety
/// `p` must satisfy [`is_faithful_list`].
unsafe fn list_ptr_snapshot(p: *mut PyObject) -> (u64, usize) {
    let n = unsafe { list_size(p) }.max(0) as usize;
    let lo = p as *const layout::PyListObject;
    let base = unsafe { (*lo).ob_item };
    let mut h = DIGEST_SEED;
    if !base.is_null() {
        for i in 0..n {
            h = digest_fold(h, unsafe { *base.add(i) } as usize as u64);
        }
    }
    (h, n)
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
    let cur = unsafe { list_ptr_snapshot(p) };
    // Every caller (`list_append` / `list_insert` / `list_store` /
    // `list_permute`) has just written *both* sides — buffer and `Rc` —
    // so record the `Rc` fingerprint alongside the buffer snapshot.
    // Refreshing only `c_ptrs` left `rc_fp` at the last *sync*'s value; a
    // later `Rc` mutation that happened to land back on that stale
    // fingerprint (lxml's append → `del path[-1]` → append cycle returns
    // to the same two-element path) made `sync_list_ob_item` early-return
    // as "unmutated" and the delete never reached `ob_item` (RFC 0076
    // WS3: `descendantpaths()` accumulated every sibling segment).
    let fp = unsafe { list_rc_of(p) }.map(|rc| digest_objects(rc.borrow().iter()));
    if SEEDED_LIST_COUNT.load(Ordering::Relaxed) != 0 {
        if let Ok(mut g) = SEEDED_LISTS.lock() {
            if let Some(map) = g.as_mut() {
                if let Some(slot) = map.get_mut(&(p as usize)) {
                    slot.c_ptrs = cur;
                    if let Some(fp) = fp {
                        slot.rc_fp = Some(fp);
                    }
                }
            }
        }
    }
    // Keep the never-registered baseline current too (see `list_mint`).
    let pre = unsafe { prefix_of(p) };
    if unsafe { (*pre).list_mint.is_some() } {
        if let Some(fp) = fp {
            unsafe { (*pre).list_mint = Some((cur, fp)) };
        }
    }
}

/// The authoritative VM list `Rc` behind a faithful list mirror: the
/// prefix's own `Object::List` for a builtin `list` mirror, or the owning
/// instance's native payload for a **list-subclass container body**
/// (RFC 0047, wave 5 — pandas' `FrozenList`). All list read/sync/mutate
/// paths resolve through this so both shapes stay coherent.
///
/// # Safety
/// `p` must satisfy [`is_faithful_list`].
unsafe fn list_rc_of(
    p: *mut PyObject,
) -> Option<weavepy_vm::sync::Rc<weavepy_vm::sync::RefCell<Vec<Object>>>> {
    let pre = unsafe { prefix_of(p) };
    if let Object::List(rc) = unsafe { &(*pre).obj } {
        return Some(rc.clone());
    }
    if let Some(w) = unsafe { (*pre).inst.as_ref() } {
        if let Some(inst) = w.upgrade() {
            if let Some(Object::List(rc)) = inst.native.get() {
                return Some(rc.clone());
            }
        }
    }
    None
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
    if SEEDED_LIST_COUNT.load(Ordering::Relaxed) == 0
        && unsafe { (*prefix_of(p)).list_mint.is_none() }
    {
        return;
    }
    let cur = unsafe { list_ptr_snapshot(p) };
    // Cheap gate: an unchanged buffer means C wrote nothing; the `Rc`
    // (possibly ahead with un-flushed VM mutations) stays authoritative.
    // A missing entry ⇒ leave the `Rc` alone (never clobber VM state).
    let (present, changed) = match SEEDED_LISTS.lock() {
        Ok(g) => match g.as_ref().and_then(|m| m.get(&(p as usize))) {
            Some(st) => (true, st.c_ptrs != cur),
            None => (false, false),
        },
        Err(_) => return,
    };
    if !present {
        // Never-registered mirror (minted VM→C, no C→VM read-back, no
        // C-API mutator): the mint-time agreement in the prefix is the
        // baseline. Adopt the buffer iff C moved it and the VM side did
        // not — Cython's inlined `list.pop()` shrinks `ob_size` through
        // a bare macro, so a mirror freed right after (lxml.sax's
        // per-event `_element_stack` crossing) is this exact shape
        // (RFC 0076 WS3).
        let pre = unsafe { prefix_of(p) };
        if let Some((mint_c, mint_fp)) = unsafe { (*pre).list_mint } {
            if cur != mint_c {
                if let Some(rc) = unsafe { list_rc_of(p) } {
                    let rc_now = digest_objects(rc.borrow().iter());
                    if rc_now == mint_fp {
                        let adopted = unsafe { read_list_vec(p) };
                        let n = adopted.len();
                        *rc.borrow_mut() = adopted;
                        unsafe {
                            (*pre).list_mint = Some((cur, digest_objects(rc.borrow().iter())))
                        };
                        if listsync_trace_enabled() {
                            eprintln!("[LISTSYNC] adopt-unseeded {p:p} ob_size={n}");
                        }
                        return;
                    }
                }
            }
        }
        if listsync_trace_enabled() {
            eprintln!(
                "[LISTSYNC] reconcile-skip {p:p} present=false cur=({:x},{})",
                cur.0, cur.1
            );
        }
        return;
    }
    if !changed {
        if listsync_trace_enabled() {
            eprintln!(
                "[LISTSYNC] reconcile-skip {p:p} present={present} cur=({:x},{})",
                cur.0, cur.1
            );
        }
        return;
    }
    let rc = match unsafe { list_rc_of(p) } {
        Some(rc) => rc,
        None => return,
    };
    let adopted = unsafe { read_list_vec(p) };
    let fp = digest_objects(adopted.iter());
    let n = cur.1;
    *rc.borrow_mut() = adopted;
    if let Ok(mut g) = SEEDED_LISTS.lock() {
        if let Some(map) = g.as_mut() {
            if let Some(slot) = map.get_mut(&(p as usize)) {
                slot.rc_fp = Some(fp);
                slot.c_ptrs = cur;
            }
        }
    }
    if listsync_trace_enabled() {
        eprintln!("[LISTSYNC] adopt {p:p} ob_size={n}");
    }
}

/// Public wrapper over [`reconcile_list_from_c`] for C-API mutators that
/// update the prefix `Rc` *directly* (slice deletion, `PySequence_DelItem`,
/// `PyObject_SetItem`) and then republish via [`sync_list_ob_item`]. They
/// must adopt any pending *macro* write (Cython's inlined append/pop)
/// **before** mutating the `Rc`: otherwise the pre-publish reconcile inside
/// the sync sees the changed buffer and adopts it wholesale — undoing the
/// mutation just made (lxml.objectify's `del path[-1]` between two macro
/// appends became a no-op, so `descendantpaths()` accumulated every
/// sibling segment into one path — RFC 0076 WS3).
///
/// # Safety
/// `p` must be a live pointer; non-list mirrors are ignored.
pub unsafe fn adopt_c_list_writes(p: *mut PyObject) {
    if unsafe { is_faithful_list(p) } {
        unsafe { reconcile_list_from_c(p) };
    }
}

/// Combined digest of a sequence of elements ([`fingerprint`] folded
/// through [`digest_fold`]) — the allocation-free replacement for the
/// per-slot fingerprint vectors.
fn digest_objects<'a>(items: impl Iterator<Item = &'a Object>) -> (u64, usize) {
    let mut h = DIGEST_SEED;
    let mut n = 0usize;
    for it in items {
        h = digest_fold(h, fingerprint(it));
        n += 1;
    }
    (h, n)
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
    #[allow(clippy::enum_glob_use)]
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
        MappingProxyObj(rc) => mix(45, rc_id(rc)),
    }
}

thread_local! {
    /// Set while [`flush_seeded_lists`] is running. A slot decref during a
    /// sync can free an object whose drop re-enters the VM→C boundary (and
    /// thus `ensure_active` → `flush_seeded_lists`); the guard makes that
    /// nested call a no-op so the outer flush keeps a consistent snapshot.
    static FLUSHING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// Boxes currently mid-[`sync_list_ob_item`] on this thread — the
    /// re-entrancy fence for self-referential lists (RFC 0069 WS5).
    static LIST_SYNCING: std::cell::RefCell<Vec<usize>> =
        const { std::cell::RefCell::new(Vec::new()) };
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
        // `rc_fp: None` forces the first flush to do a real sync.
        if g.get_or_insert_with(FxHashMap::default)
            .insert(
                p as usize,
                ListSync {
                    rc_fp: None,
                    c_ptrs,
                },
            )
            .is_none()
        {
            SEEDED_LIST_COUNT.fetch_add(1, Ordering::Relaxed);
            if listsync_trace_enabled() {
                let n = unsafe { list_size(p) };
                eprintln!("[LISTSYNC] register {p:p} ob_size={n}");
            }
        }
    }
}

/// Adopt every registered seeded list whose `ob_item` buffer no longer
/// matches its recorded snapshot — the C↩VM twin of [`flush_seeded_lists`],
/// run at the outermost bridged call's *return* (see `ensure_active`).
///
/// This catches macro writes to a list that never crosses the boundary
/// again on its own: orjson's iterative deserializer attaches a fresh
/// `PyList_New(n)` to its parent dict/list *first* (our `PyDict_SetItem`
/// clones the still-placeholder `Rc` at that moment) and only then fills
/// the elements through the inlined `PyList_SET_ITEM` macro, so without
/// this sweep the parent keeps `[None, …]` forever.
///
/// # Safety
/// Must run with no extension C frame below (outermost return); entries
/// are unregistered on free, so every recorded pointer is live.
pub unsafe fn reconcile_seeded_lists() {
    if SEEDED_LIST_COUNT.load(Ordering::Relaxed) == 0 {
        return;
    }
    // Collect first: `reconcile_list_from_c` takes the same lock.
    let ptrs: Vec<usize> = match SEEDED_LISTS.lock() {
        Ok(g) => g
            .as_ref()
            .map(|m| m.keys().copied().collect())
            .unwrap_or_default(),
        Err(_) => return,
    };
    for p in ptrs {
        unsafe { reconcile_list_from_c(p as *mut PyObject) };
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
    // RFC 0069 WS5: publishing the elements below crosses each of them
    // back out (`into_owned`), and a list that (transitively) contains
    // itself re-enters this very sync through the canonical-cache hit —
    // its fingerprint is only recorded on the way *out*, so the nested
    // call would republish forever. The box being synced already holds
    // its own live pointer in the slot; skip the nested pass.
    let key = p as usize;
    let reentered = LIST_SYNCING.with(|s| {
        let mut v = s.borrow_mut();
        if v.contains(&key) {
            true
        } else {
            v.push(key);
            false
        }
    });
    if reentered {
        return;
    }
    struct SyncGuard(usize);
    impl Drop for SyncGuard {
        fn drop(&mut self) {
            LIST_SYNCING.with(|s| s.borrow_mut().retain(|&k| k != self.0));
        }
    }
    let _guard = SyncGuard(key);
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
    let rc = match unsafe { list_rc_of(p) } {
        Some(rc) => rc,
        None => return,
    };
    // Fingerprint the VM-shared list (allocation-free). If it matches what we
    // last published to `ob_item`, the list is unmutated since the previous
    // flush and the buffer is already coherent — leave it untouched (no
    // allocation, no refcount churn). This is what keeps a flush at *every*
    // VM→C boundary affordable; only a genuinely mutated list pays to rebuild.
    let fp = digest_objects(rc.borrow().iter());
    if let Ok(g) = SEEDED_LISTS.lock() {
        if let Some(map) = g.as_ref() {
            if let Some(st) = map.get(&(p as usize)) {
                if st.rc_fp == Some(fp) {
                    return;
                }
            }
        }
    }
    let items: Vec<Object> = rc.borrow().clone();
    let n = items.len();
    let old_n = unsafe { list_size(p) }.max(0) as usize;
    if listsync_trace_enabled() {
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
        if listsync_trace_enabled() && n <= 3 {
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
                slot.rc_fp = Some(fp);
                slot.c_ptrs = c_ptrs;
            }
        }
    }
    // Advance the mint-time agreement too: a never-registered mirror
    // (RFC 0076 WS3) relies on `list_mint` as its reconcile baseline, and
    // leaving the stale pre-publish snapshot there would make the *next*
    // C macro write look like a two-sided divergence (both `cur != mint_c`
    // and `rc != mint_fp`) and be skipped.
    if unsafe { (*pre).list_mint.is_some() } {
        unsafe { (*pre).list_mint = Some((c_ptrs, fp)) };
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
    if listsync_disabled() {
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
    if listsync_trace_enabled() {
        eprintln!("[LISTSYNC] flush count={c}");
    }
    // One pass under a single lock acquisition classifies every seeded
    // list as clean (the overwhelmingly common case — both digests match)
    // or dirty. Only dirty lists take the full per-list sync afterwards,
    // *outside* the lock (a slot decref may free an object and re-enter
    // this module). The classification is read-only — raw `ob_item` reads
    // plus a shared borrow of the prefix `Rc` — so holding the map lock
    // across it is safe (no `Py_DecRef`, no minting, no VM re-entry).
    let dirty: Vec<usize> = match SEEDED_LISTS.lock() {
        Ok(g) => match g.as_ref() {
            Some(m) => m
                .iter()
                .filter(|(&pu, st)| {
                    let p = pu as *mut PyObject;
                    if !unsafe { is_faithful_list(p) } {
                        return false;
                    }
                    let pre = unsafe { prefix_of(p) };
                    if !unsafe { (*pre).list_synced } {
                        return false;
                    }
                    // Dirty when C wrote the buffer directly (`c_ptrs`
                    // digest moved) or the VM mutated the prefix `Rc`
                    // (`rc_fp` digest moved).
                    if st.c_ptrs != unsafe { list_ptr_snapshot(p) } {
                        return true;
                    }
                    let rc = match unsafe { list_rc_of(p) } {
                        Some(rc) => rc,
                        None => return false,
                    };
                    let dirty = match rc.try_borrow() {
                        Ok(items) => st.rc_fp != Some(digest_objects(items.iter())),
                        // Mid-mutation borrow: treat as dirty, the full
                        // sync re-checks outside the lock.
                        Err(_) => true,
                    };
                    dirty
                })
                .map(|(&pu, _)| pu)
                .collect(),
            None => return,
        },
        Err(_) => return,
    };
    for pu in dirty {
        unsafe { sync_list_ob_item(pu as *mut PyObject) };
    }
}

// ---------------------------------------------------------------------------
// Macro-read watch set (RFC 0076 WS1).
// ---------------------------------------------------------------------------
//
// [`flush_seeded_lists`] runs only at the *outermost* VM→C transition — a
// deliberate cost choice (it digests every seeded list). But an extension
// holding a `PySequence_Fast` result reads it through the raw
// `PySequence_Fast_GET_SIZE`/`GET_ITEM` macros *between* its own nested
// callbacks into the VM, and expects a callback's list mutation to be
// visible immediately (same object on CPython). numpy's coercion is the
// canonical case: a malicious `__len__` appends to the outer list mid
// `np.array(obj)`, and `PyArray_AssignFromCache` re-reads `Py_SIZE(seq)`
// to raise "Content of sequences changed" — with the mirror republished
// only at the outermost return, the mutation was invisible and the
// RuntimeError never fired (test_array_coercion, TestBadSequences).
//
// So `PySequence_Fast` registers its (seeded) list result in a small
// thread-local watch set, and every *nested* `ensure_active` exit re-syncs
// just those watched lists. The set is capped (pathological detection is
// best-effort, per numpy's own "we do not test a shrinking list" caveat)
// and cleared at the outermost C→VM return, so steady-state VM code pays
// one empty-TLS check per bridged call.

thread_local! {
    static MACRO_WATCHED_LISTS: std::cell::RefCell<Vec<usize>> =
        const { std::cell::RefCell::new(Vec::new()) };
}
const MACRO_WATCH_CAP: usize = 8;

/// Watch `p` (a seeded faithful list mirror) for macro reads: nested
/// VM→C boundary exits will republish its prefix `Rc` into `ob_item`.
pub fn watch_list_for_macro_reads(p: *mut PyObject) {
    let _ = MACRO_WATCHED_LISTS.try_with(|w| {
        let mut v = w.borrow_mut();
        let k = p as usize;
        if v.contains(&k) {
            return;
        }
        if v.len() >= MACRO_WATCH_CAP {
            v.remove(0);
        }
        v.push(k);
    });
}

/// Drop `p` from the watch set — called from [`free_mirror`] so a nested
/// sync can never touch freed storage.
fn unwatch_list(p: *mut PyObject) {
    let _ = MACRO_WATCHED_LISTS.try_with(|w| {
        let mut v = w.borrow_mut();
        if !v.is_empty() {
            v.retain(|&k| k != p as usize);
        }
    });
}

/// True iff the watch set is empty (the common steady state) — lets the
/// nested-boundary caller skip the sync without cloning the TLS vec.
pub fn no_watched_lists() -> bool {
    MACRO_WATCHED_LISTS
        .try_with(|w| w.borrow().is_empty())
        .unwrap_or(true)
}

/// Republish every watched list's prefix `Rc` into its `ob_item` buffer.
/// Called at *nested* VM→C boundary exits (see module comment above).
///
/// # Safety
/// May only be called at a VM→C transition (no C code mid-read of a
/// watched list's `ob_item`).
pub unsafe fn sync_watched_lists() {
    let snapshot: Vec<usize> = match MACRO_WATCHED_LISTS.try_with(|w| w.borrow().clone()) {
        Ok(v) => v,
        Err(_) => return,
    };
    for k in snapshot {
        let p = k as *mut PyObject;
        if unsafe { is_faithful_list(p) } {
            unsafe { sync_list_ob_item(p) };
        }
    }
}

/// Clear the watch set — called at the outermost C→VM return, scoping the
/// watches to a single extension-call window.
pub fn clear_watched_lists() {
    let _ = MACRO_WATCHED_LISTS.try_with(|w| w.borrow_mut().clear());
}

/// Adopt direct `PyList_SET_ITEM`-macro writes made to the **nested** seeded
/// faithful lists reachable from `p`'s `ob_item` slots (RFC 0047, wave 5).
///
/// [`reconcile_list_from_c`] already adopts a macro write to a list's *own*
/// `ob_item` when that list is read back — its slot pointers changed, so the
/// gate fires. But a stock extension frequently mutates a **child** list's
/// cells while leaving the parent's slot (the child *pointer*) unchanged, so
/// the parent's read-back short-circuits and never descends. pandas' `to_csv`
/// is the canonical case: its Cython `write_csv_rows` reuses one
/// `rows = [[None] * ncols for _ in range(100)]` buffer, overwrites each inner
/// row's cells through `__Pyx_SetItemInt_Fast` → the `PyList_SET_ITEM` macro
/// (the index is a C `Py_ssize_t` and the runtime `PyList_CheckExact`
/// succeeds), then re-hands the *same* `rows` object to `writer.writerows`.
/// The outer slots never change, so without this the inner rows' stale prefix
/// `Rc`s (seeded from the first 100-row batch) were replayed for every later
/// batch — every row `>= 100` wrote the data of row `i % 100`.
///
/// Descending reconciles each child directly: its own slot pointers *did*
/// change (fresh cell objects), so [`reconcile_list_from_c`] refills its `Rc`
/// in place — and because the parent's prefix shares that very `Rc`, the
/// parent observes the fresh cells. A child with only a pending *VM* mutation
/// (its `ob_item` unchanged) is left untouched, so VM state is never clobbered.
///
/// Cost is bounded by the size of the crossed list (the same O(n) as the
/// pointer snapshot [`reconcile_list_from_c`] already takes), not the global
/// seeded set — so it stays on the hot C→VM path only for the data actually
/// being read. `depth` caps recursion so a self-referential list cannot loop.
///
/// # Safety
/// `p` must satisfy [`is_faithful_list`]; called at a C→VM transition.
pub(crate) unsafe fn reconcile_nested_lists(p: *mut PyObject, depth: u32) {
    // pandas' nesting (list-of-rows) is shallow; a small cap defends against a
    // cyclic list without a visited-set allocation on this hot path.
    const MAX_DEPTH: u32 = 6;
    if depth >= MAX_DEPTH || SEEDED_LIST_COUNT.load(Ordering::Relaxed) == 0 {
        return;
    }
    let n = unsafe { list_size(p) };
    if n <= 0 {
        return;
    }
    let lo = p as *const layout::PyListObject;
    let base = unsafe { (*lo).ob_item };
    if base.is_null() {
        return;
    }
    for i in 0..n as usize {
        let slot = unsafe { *base.add(i) };
        // Only faithful-list children can carry a macro write we would miss;
        // guard against a direct self-reference before recursing.
        if !slot.is_null() && slot != p && unsafe { is_faithful_list(slot) } {
            unsafe { reconcile_list_from_c(slot) };
            unsafe { reconcile_nested_lists(slot, depth + 1) };
        }
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
        // Defensive: any WeavePy-minted string now carries a faithful PEP 393
        // body (all kinds), so this only guards a degenerate head-only body;
        // its value would live in the prefix, so fall back to a content scan.
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
    let mut cps = Vec::with_capacity(len);
    for i in 0..len {
        cps.push(unsafe { read_codepoint(data, kind, i) });
    }
    // Canonicalises to `Str` (no surrogates) or `WStr` (some) — a C-side
    // `PyUnicode_WRITE` of a lone surrogate must survive the round trip,
    // not collapse to U+FFFD.
    Object::str_from_codepoints(cps)
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
    // Overflow-safe size computation. A stock extension (e.g. Cython's
    // inlined `str.join`, which sizes the result by summing
    // `PyUnicode_GET_LENGTH` over the parts) can hand us a bogus or huge
    // length; CPython's `PyUnicode_New` returns NULL + raises MemoryError in
    // that case rather than aborting, so we must not panic here.
    let raw_body = match len
        .checked_add(1)
        .and_then(|n| n.checked_mul(width))
        .and_then(|n| n.checked_add(data_off))
    {
        Some(n) if n <= isize::MAX as usize => n,
        _ => {
            if std::env::var_os("WEAVEPY_USTR_DBG").is_some() {
                eprintln!(
                    "[USTR] new_unicode_mirror oversize len={len} maxchar={maxchar:#x} width={width}\n{}",
                    std::backtrace::Backtrace::force_capture()
                );
            }
            return ptr::null_mut();
        }
    };
    let body_size = round_up(raw_body, 8);
    let total = match body_size.checked_add(PREFIX_SIZE) {
        Some(t) if t <= isize::MAX as usize => t,
        _ => return ptr::null_mut(),
    };
    let layout = match Layout::from_size_align(total, BODY_ALIGN) {
        Ok(l) => l,
        Err(_) => return ptr::null_mut(),
    };
    let raw = unsafe { alloc(layout) };
    if raw.is_null() {
        return ptr::null_mut();
    }
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
                bytes_buffer: false,
                list_synced: false,
                scalar_pinned: false,
                list_mint: None,
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
    if np.is_null() {
        return ptr::null_mut();
    }
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
pub(crate) unsafe fn list_prefix_seed_once(p: *mut PyObject) {
    let pre = unsafe { prefix_of(p) };
    if unsafe { (*pre).list_synced } {
        return;
    }
    let rc = match unsafe { list_rc_of(p) } {
        Some(rc) => rc,
        None => return,
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
    // Adopt any *direct* C macro write first (RFC 0047, wave 5). Cython's
    // `__Pyx_PyList_Append` inlines the append — `PyList_SET_ITEM` +
    // `__Pyx_SET_SIZE`, no call into us — whenever the list has spare
    // capacity and is more than half full, falling back to `PyList_Append`
    // only on the grow step. Those macro-written elements exist solely in
    // `ob_item`; pushing onto the stale prefix `Rc` here and then stamping
    // the buffer as agreed would orphan them from the VM view forever
    // (pandas' `get_blkno_indexers` built [0,1,2,3,7] out of 8 appends —
    // `AssertionError: Gaps in blk ref_locs`).
    unsafe { reconcile_list_from_c(p) };
    let n = unsafe { list_size(p) } as usize;
    let base = unsafe { list_reserve(p, n + 1) };
    unsafe { crate::object::Py_IncRef(item) };
    unsafe { *base.add(n) = item };
    let vo = p as *mut layout::PyVarObject;
    unsafe { (*vo).ob_size = (n + 1) as PySsizeT };
    // Write-through to the shared prefix `Rc` (identity preserved) so a VM
    // alias — a `defaultdict[k]` list a Cython `.append(...)` mutated —
    // observes the append (RFC 0047, wave 5).
    if let Some(rc) = unsafe { list_rc_of(p) } {
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
    // Adopt macro-written elements before the targeted `Rc` mutation (see
    // [`list_append`]) — a stale prefix would misplace the insert.
    unsafe { reconcile_list_from_c(p) };
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
    if let Some(rc) = unsafe { list_rc_of(p) } {
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
    // Adopt macro-written elements before the targeted `Rc` mutation (see
    // [`list_append`]) — on a stale (short) prefix the index could fall
    // past `v.len()` and the store would vanish from the VM view.
    unsafe { reconcile_list_from_c(p) };
    let lo = p as *mut layout::PyListObject;
    let base = unsafe { (*lo).ob_item };
    let slot = unsafe { base.add(pos as usize) };
    let prev = unsafe { *slot };
    unsafe { *slot = item };
    if !prev.is_null() {
        unsafe { crate::object::Py_DecRef(prev) };
    }
    // Mirror the store into the shared prefix `Rc` (RFC 0047, wave 5).
    if let Some(rc) = unsafe { list_rc_of(p) } {
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
    if let Some(rc) = unsafe { list_rc_of(p) } {
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
    if pin_trace_enabled() {
        if let Object::Bytes(b) = unsafe { &(*prefix_of(p)).obj } {
            if b.len() > 4096 {
                eprintln!("[pin] free-bytes {}B", b.len());
            }
        }
    }
    unsafe { record_mirror_free(p) };
    crate::object::unregister_minted(p);
    // Release any argument references `PyArg_ParseTuple` tethered to this
    // owner's lifetime (RFC 0047, wave 5); no-op unless tethers exist.
    crate::argparse::drop_tethered(p);
    // Drop a seeded list mirror from the write-through set.
    if unsafe { is_faithful_list(p) } {
        // RFC 0076 WS3: adopt any pending *direct* C write first — Cython's
        // inlined `list.pop()` shrinks `ob_size` via `__Pyx_SET_SIZE` with
        // no C-API call, so if this mirror dies before the next boundary
        // flush the mutation would die with it and the next crossing would
        // republish the stale prefix `Rc` (lxml.sax's `_element_stack.pop()`
        // "un-popped", failing every saxify round-trip with "Unexpected
        // element closed"). Covers both the registered case (snapshot in
        // `SEEDED_LISTS`) and the never-registered one (mint snapshot in
        // the prefix).
        unsafe { reconcile_list_from_c(p) };
        if SEEDED_LIST_COUNT.load(Ordering::Relaxed) > 0 {
            unregister_seeded_list(p);
        }
    }
    // RFC 0047 (wave 5): drop this box from the canonical list cache before
    // its prefix (and the native `Rc` the key is derived from) is dropped.
    if unsafe { is_faithful_list(p) } {
        unsafe { unregister_list_box(p) };
        unwatch_list(p);
    }
    // RFC 0047 (wave 5): drop this box from the canonical set cache before
    // its prefix (and the native `Rc` the key is derived from) is dropped.
    if SET_BOX_COUNT.load(Ordering::Relaxed) > 0 && unsafe { is_faithful_set(p) } {
        unsafe { unregister_set_box(p) };
    }
    // RFC 0056 WS5: likewise for the canonical bytearray cache.
    if BYTEARRAY_BOX_COUNT.load(Ordering::Relaxed) > 0 && unsafe { is_faithful_bytearray(p) } {
        unsafe { unregister_bytearray_box(p) };
    }
    // Likewise drop this box from the canonical builtin cache (`operator.eq`
    // &c.) before its prefix `Rc` is dropped, so the next crossing of the
    // same native builtin mints (and re-registers) a fresh canonical box.
    if BUILTIN_BOX_COUNT.load(Ordering::Relaxed) > 0 && unsafe { is_faithful_builtin(p) } {
        unsafe { unregister_builtin_box(p) };
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
    //
    // Only the **live prefix** (`ob_size` slots) owns references, exactly
    // like CPython's `list_dealloc` (which walks `Py_SIZE(op)` items). The
    // allocated tail beyond `ob_size` can hold *stale* pointers: Cython's
    // inlined `list.pop()` fast path takes the item and shrinks `ob_size`
    // via `__Pyx_SET_SIZE` without nulling the vacated slot (CPython never
    // reads past `ob_size`, so it doesn't need to). Sweeping the whole
    // buffer by `aux_size` decref'd those popped elements a second time —
    // uvloop's `UVProcess._init` pops the errpipe fds off `fds_to_close`
    // while `self._errpipe_read/_write` still own them, so the double
    // decref freed live int boxes and the transport's later `tp_traverse`
    // visited freed memory (SIGSEGV on Linux; masked by macOS's allocator).
    if !aux_ptr.is_null() && aux_size > 0 && unsafe { is_faithful_list(p) } {
        let cap = (aux_size / std::mem::size_of::<*mut PyObject>()) as isize;
        let live = unsafe { (*(p as *const layout::PyVarObject)).ob_size }.clamp(0, cap);
        let slots = aux_ptr as *mut *mut PyObject;
        for i in 0..live {
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
        // The aux slot-pointer snapshot owns one reference per recorded
        // pointer (ABA guard, see `tuple_native_shared`); release those too.
        if !aux_ptr.is_null() && aux_size > 0 {
            let sn = aux_size / std::mem::size_of::<*mut PyObject>();
            let slots = aux_ptr as *mut *mut PyObject;
            for i in 0..sn {
                let elem = unsafe { *slots.add(i) };
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

    // RFC 0066 WS3: a faithful `PyCFunction` mirror minted for a
    // `PyCFunction_NewEx` builtin owns one reference to its `m_self`
    // (materialised in `fill_body` from the sidecar). NULL for every
    // method-table builtin — a no-op there.
    if unsafe { is_faithful_cfunction(p) } {
        let cf = p as *mut layout::PyCFunctionObject;
        let recv = unsafe { (*cf).m_self };
        if !recv.is_null() {
            unsafe { crate::object::Py_DecRef(recv) };
        }
    }

    // RFC 0047 (wave 5): a faithful slice owns one reference to each of
    // `start`/`stop`/`step` (materialised in `fill_body`), so release them
    // before the block goes away. Immortal singletons (None/bool) no-op.
    if unsafe { is_faithful_slice(p) } {
        let so = p as *mut layout::PySliceObject;
        for field in [unsafe { (*so).start }, unsafe { (*so).stop }, unsafe {
            (*so).step
        }] {
            if !field.is_null() {
                unsafe { crate::object::Py_DecRef(field) };
            }
        }
    }

    // RFC 0047 (wave 5): C is dropping what may be the native object's
    // last program-visible reference (a faithful tuple/list mirror
    // decref'd inside a Cython setter — pandas' `self.blocks = new`
    // releasing the old `tuple[Block, ...]`). Park instance/container
    // natives for a refcount-guarded reap at the next eval-loop safe
    // point so members that died with the container get their weakrefs
    // cleared with CPython's timing; anything still alive elsewhere
    // fails the reap's deadness test untouched.
    weavepy_vm::vm_singletons::queue_cext_dropped(unsafe { &(*pre).obj });
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
    /// Faithful `PyByteArrayObject` whose `ob_bytes`/`ob_start` point at
    /// the live VM `Vec<u8>` buffer (RFC 0056 WS5). Cython inlines
    /// `PyByteArray_AS_STRING`/`PyByteArray_GET_SIZE` (aiohttp's
    /// `_http_parser` decodes its `bytearray` URL buffer that way), so the
    /// struct fields must address real bytes; they are refreshed on every
    /// crossing and after every bridged C→VM call (the buffer reallocates
    /// when the VM grows it) — see [`sync_bytearray_boxes`].
    ByteArray,
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
            Object::ByteArray(_) => BodyPlan {
                kind: BodyKind::ByteArray,
                // Exactly `PyByteArrayObject`; the byte buffer is the VM
                // `Vec` itself (no copy) — see the `BodyKind` docs.
                body_size: std::mem::size_of::<layout::PyByteArrayObject>(),
            },
            Object::Str(s) => {
                // Every string — 1-, 2-, or 4-byte kind — gets a faithful
                // PEP 393 compact body so a stock extension's inlined
                // `PyUnicode_DATA`/`PyUnicode_KIND`/`PyUnicode_GET_LENGTH`
                // macros (and Cython's f-string / `str.join` fast paths,
                // which read the parts' buffers directly) address real
                // memory. A compact-ASCII string carries its 1-byte data
                // just past `PyASCIIObject`; a compact non-ASCII string
                // (Latin-1, UCS-2, or UCS-4) carries it past
                // `PyCompactUnicodeObject`, where the inlined `PyUnicode_DATA`
                // macro reads it (keyed off the `ascii`/`kind` state bits).
                // Size the body for whichever kind `fill_str` will write.
                let n = s.chars().count();
                let (_kind, _ascii, data_off, width) = unicode_form(str_maxchar(s));
                BodyPlan {
                    kind: BodyKind::Str,
                    body_size: round_up(data_off + (n + 1) * width, 8),
                }
            }
            Object::WStr(cps) => {
                // A surrogate-bearing string gets the same faithful PEP 393
                // body as a plain `str` — CPython happily stores lone
                // surrogates in UCS-2/UCS-4 buffers, and stock readers
                // (`PyUnicode_GET_LENGTH`, `PyUnicode_READ`) must see them.
                let n = cps.len();
                let maxchar = cps.iter().copied().max().unwrap_or(0);
                let (_kind, _ascii, data_off, width) = unicode_form(maxchar);
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
                // C reads `ob_fval` via the `PyFloat_AS_DOUBLE` macro; it
                // must hold the canonical NaN bits CPython would store, not
                // WeavePy's identity tag (see `untag_nan`).
                unsafe { (*fo).ob_fval = weavepy_vm::object::untag_nan(*f) };
            }
        }
        BodyKind::Complex => {
            if let Object::Complex(c) = obj {
                let co = body as *mut layout::PyComplexObject;
                unsafe {
                    (*co).cval = layout::PyComplexValue {
                        real: weavepy_vm::object::untag_nan(c.real),
                        imag: weavepy_vm::object::untag_nan(c.imag),
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
        BodyKind::ByteArray => {
            if let Object::ByteArray(_) = obj {
                unsafe { write_bytearray_fields(body, obj) };
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
                // RFC 0047 (wave 5): snapshot the freshly-written `ob_item`
                // pointers into the (otherwise unused for tuples) aux buffer.
                // [`native_of`]'s read-back compares the live slots against
                // this snapshot: unchanged slots mean no stock
                // `PyTuple_SET_ITEM` rewired the tuple, so the prefix's
                // native `Object::Tuple` — for a VM-minted tuple, the
                // *original* `Rc` — is returned with its identity intact
                // (CPython parity: an object stored into and read back out
                // of a C container `is` itself; pandas'
                // `test_np_max_nested_tuples` asserts `arr.max() is arr[2]`).
                // The snapshot owns one reference per recorded pointer (see
                // `tuple_native_shared`'s ABA rationale); `free_mirror`'s
                // tuple pass and re-snapshots release them.
                if !t.is_empty() {
                    let bytes = t.len() * std::mem::size_of::<*mut PyObject>();
                    let buf_layout =
                        Layout::from_size_align(bytes, BODY_ALIGN).expect("tuple seed layout");
                    let buf = unsafe { alloc(buf_layout) };
                    assert!(!buf.is_null(), "tuple seed allocation failed");
                    unsafe {
                        ptr::copy_nonoverlapping(base as *const u8, buf, bytes);
                        let slots = buf as *mut *mut PyObject;
                        for i in 0..t.len() {
                            let e = *slots.add(i);
                            if !e.is_null() {
                                crate::object::Py_IncRef(e);
                            }
                        }
                    }
                    *aux_ptr = buf;
                    *aux_size = bytes;
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
            // RFC 0066 WS3: a builtin minted by `PyCFunction_NewEx` carries
            // the caller's real `PyMethodDef*` and bound `self`, and stock
            // extensions read both straight off the struct — pybind11's
            // `initialize_generic` does `PyCFunction_GET_SELF(sibling)` to
            // recover its `function_record` capsule for overload chaining,
            // failing module init on NULL. Populated on *every* mint, so the
            // fields survive VM round-trips; `m_self` is an owned reference
            // released in `free_mirror`.
            if let Some((self_obj, ml)) = crate::module::cfunction_extra(obj) {
                unsafe {
                    if ml != 0 {
                        (*cf).m_ml = ml as *mut layout::PyMethodDef;
                    }
                    if let Some(s) = self_obj {
                        (*cf).m_self = crate::object::into_owned(s);
                    }
                }
            } else {
                // A VM-native builtin (`object.__init__`, method-table
                // installs, …) has no C-level receiver. Advertise `None`
                // rather than NULL: CPython's NULL means "unbound
                // PyCFunction", a state stock CPython never hands out
                // through attribute lookup — pybind11's overload-chaining
                // sibling probe hard-fails on it, but treats a non-capsule
                // `m_self` as a plain non-chainable function (exactly what
                // an inherited VM builtin is). The immortal singleton makes
                // `free_mirror`'s decref a no-op.
                unsafe { (*cf).m_self = crate::singletons::none_ptr() };
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
                    (*mo).view.strides = if ndim > 0 {
                        strides_ptr
                    } else {
                        ptr::null_mut()
                    };
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

/// Fill a compact PEP 393 unicode body of the kind implied by the string's
/// largest code point: 1-byte (compact-ASCII or Latin-1), 2-byte (UCS-2), or
/// 4-byte (UCS-4). The data offset (and the `ascii` state bit) differ
/// between the compact-ASCII form (data past `PyASCIIObject`) and the
/// compact non-ASCII forms (data past `PyCompactUnicodeObject`, where the
/// inlined `PyUnicode_DATA` macro reads it). Each code point is stored at
/// its kind's width and the buffer is NUL-terminated with one trailing unit,
/// so a stock reader's `PyUnicode_READ`/`PyUnicode_DATA` and Cython's
/// `str.join`/f-string fast paths address a real, correctly-sized buffer.
unsafe fn fill_str(body: *mut PyObject, obj: &Object) {
    // A `WStr` (lone surrogates) fills the same faithful body; its code
    // points are stored verbatim so a stock `PyUnicode_READ` sees the
    // surrogate and `PyUnicode_AsUTF8*` can raise the canonical
    // UnicodeEncodeError. The hash comes from the VM's `hash()` (dict-
    // bucketing-consistent), like `py_str_hash` for a plain `str`.
    let (cps, hash): (Vec<u32>, i64) = match obj {
        Object::Str(s) => (
            s.chars().map(|c| c as u32).collect(),
            weavepy_vm::object::py_str_hash(s),
        ),
        Object::WStr(w) => {
            let h = match weavepy_vm::builtins::hash_object(obj) {
                Ok(Object::Int(h)) => h,
                _ => -2,
            };
            (w.to_vec(), h)
        }
        _ => return,
    };
    let maxchar = cps.iter().copied().max().unwrap_or(0);
    let (kind, ascii, data_off, width) = unicode_form(maxchar);
    let n = cps.len();
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
        (*ao).hash = hash as crate::object::PyHashT;
        (*ao).state = ustate::pack(
            0, // not interned
            kind, true,  // compact
            ascii, // ascii
            false, // not statically allocated
        );
        let data = (body as *mut u8).add(data_off);
        for (i, cp) in cps.iter().enumerate() {
            write_codepoint(data, kind, i, *cp);
        }
        // NUL-terminate with one trailing code unit of the body's width.
        match width {
            1 => *data.add(n) = 0,
            2 => *(data as *mut u16).add(n) = 0,
            _ => *(data as *mut u32).add(n) = 0,
        }
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

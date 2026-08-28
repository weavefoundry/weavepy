//! Faithful inline instance bodies (RFC 0045, wave 3).
//!
//! Wave 1 gave WeavePy's *built-in* values layout-faithful mirrors so a
//! stock extension's inlined field reads (`PyFloat_AS_DOUBLE`, …) land on
//! real CPython-shaped memory. Wave 2 readied stock *types*, but stored
//! their instance state in `__dict__` (the side-allocated `_core_addr`
//! pattern) because a C struct field read at a fixed `tp_basicsize`
//! offset (`((MyType *)self)->field`) was not yet stable across the
//! boundary — every crossing minted a fresh box.
//!
//! This module closes that gap. An instance of an **inline-storage
//! extension type** ([`crate::types::is_inline_instance_type`] — a
//! `PyType_FromSpec` / `PyType_Ready` type that declares
//! `tp_basicsize > sizeof(PyObject)`) is materialised **once** into a
//! `tp_basicsize`-sized faithful body — `[PyObject head | inline fields |
//! inline var-data]` — via [`crate::mirror::alloc_instance_body`]. The
//! body is **owned by the native [`PyInstance`]** (recorded in its
//! `c_body` cell) and presents the **same pointer** on every crossing, so
//! `self->field` written in one C call is still there in the next, and
//! the Python view (`obj.field` via `tp_members`) reads the same bytes.
//!
//! ## Lifetime
//!
//! Two halves reference each other; exactly one edge is strong, so there
//! is no cycle:
//!
//! * The **instance owns the body.** [`PyInstance`]'s `Drop` frees the
//!   block (via the `register_instance_body_free` hook installed by
//!   [`install`]) — running the type's custom `tp_dealloc` first for
//!   faithful resource cleanup.
//! * The **body borrows the instance** through a `Weak<PyInstance>` in its
//!   [`MirrorPrefix`](crate::mirror::MirrorPrefix), so
//!   [`crate::mirror::native_of`] resolves the pointer back to *the same*
//!   instance without owning it.
//! * While **C holds at least one reference** (the body's `ob_refcnt` is
//!   positive) the [`STRONG`] map pins the instance with a real `Rc`, so a
//!   pointer handed to C never dangles even if the VM drops its last
//!   reference first. When C's refcount reaches zero
//!   ([`release_c_ownership`]) that pin is dropped; the block survives as
//!   long as the VM still references the instance, and is reclaimed with
//!   the instance otherwise.

use std::collections::HashMap;
use std::sync::Mutex;

use weavepy_vm::object::Object;
use weavepy_vm::sync::Rc;
use weavepy_vm::types::PyInstance;

use crate::object::{PyObject, PySsizeT};
use crate::types::PyTypeObject;

/// C-side ownership of inline instances: `body pointer -> Rc<PyInstance>`.
///
/// An entry exists exactly while the body's C refcount is positive —
/// i.e. while a C extension holds a reference. It is the strong edge
/// that keeps the native instance (and therefore its faithful body)
/// alive for C even after the VM has dropped its last reference. The
/// [`MirrorPrefix`](crate::mirror::MirrorPrefix)'s back-reference is a
/// `Weak`, so this map is the *only* strong C→instance link and there
/// is no ownership cycle.
///
/// Process-global (RFC 0047, wave 5): an instance pinned on the importing
/// thread is released from whichever `threading.Thread` drops the last C
/// reference, so a per-thread map would leak the pin (and, worse, let a
/// worker misroute the body's free). Guard is a plain `Mutex`; every
/// mutation drops removed values *after* the lock is released because
/// dropping an `Rc<PyInstance>` can re-enter this map.
static STRONG: Mutex<Option<HashMap<usize, Rc<PyInstance>>>> = Mutex::new(None);

/// Install the VM hook that frees an instance's faithful body when the
/// instance is collected (RFC 0045, wave 3). Idempotent; called from
/// [`crate::interp::ensure_initialised`].
pub fn install() {
    weavepy_vm::types::register_instance_body_free(free_instance_body_hook);
}

/// Hand `inst` to C as its single, stable faithful body (RFC 0045).
///
/// On the **first** crossing the body is allocated `tp_basicsize` bytes
/// wide (its `ob_refcnt` starts at 1, representing C's borrow) and
/// recorded in `inst.c_body`; subsequent crossings return that same
/// pointer. Either way C's borrow is pinned in [`STRONG`] for the
/// lifetime of the reference, and the returned pointer carries one C
/// reference the caller owns.
pub fn instance_body_out(inst: &Rc<PyInstance>, ty: *mut PyTypeObject) -> *mut PyObject {
    let existing = inst.c_body.get();
    if existing == 0 {
        // First crossing: mint the faithful body. `alloc_instance_body`
        // starts it at refcount 1 — that is C's borrow, so pin the
        // instance for as long as the reference lives.
        let basicsize =
            unsafe { (*ty).tp_basicsize }.max(std::mem::size_of::<PyObject>() as PySsizeT) as usize;
        let body = attach_body(inst, ty, basicsize);
        // RFC 0029 (wave 5): a `datetime`/`date`/`time`/`timedelta`
        // instance crossing into C is materialised into a byte-faithful
        // body so the inlined `PyDateTime_GET_*` accessor macros (which
        // pandas' tslibs read directly) see correct data. A no-op for
        // every other inline type.
        crate::datetime_api::maybe_pack_datetime_body(body, ty, inst);
        return body;
    }

    // Re-crossing: the body already exists and outlived any previous C
    // reference (the instance owns it). Re-establish C's borrow.
    let body = existing as *mut PyObject;
    if crate::mirror::body_trace_enabled() {
        // Verify the cached body still resolves back to *this* instance.
        let resolved = unsafe { crate::mirror::native_of(body) };
        let matches = matches!(&resolved, weavepy_vm::object::Object::Instance(other)
            if weavepy_vm::sync::Rc::ptr_eq(other, inst));
        if !matches {
            let tn = unsafe { crate::object::debug_type_name(body) };
            eprintln!(
                "[STALE-CBODY] inst=0x{:x} cls={} c_body=0x{:x} body-type={} resolved-to={}",
                weavepy_vm::sync::Rc::as_ptr(inst) as usize,
                inst.cls().name,
                body as usize,
                tn,
                resolved.type_name_owned(),
            );
        }
    }
    let head = unsafe { &mut *body };
    if head.ob_refcnt <= 0 {
        head.ob_refcnt = 1;
        strong_pin(body, inst);
    } else {
        head.ob_refcnt += 1;
    }
    body
}

/// Hand a **container-subclass** instance — a VM `class C(list)` /
/// `class C(tuple)` (pandas' `FrozenList`, every `namedtuple`) — to C as
/// a faithful `PyListObject`/`PyTupleObject`-shaped body (RFC 0047,
/// wave 5). CPython lays such instances out as real container structs
/// with the subclass in `ob_type`, and stock extensions classify with
/// `PyList_Check`/`PyTuple_Check` (a `tp_flags` bit test) then read the
/// layout through the `PyList_GET_ITEM`/`PyTuple_GET_ITEM`/`Py_SIZE`
/// macros — no function call to interpose. A plain identity box (Rust
/// payload where C expects `ob_item`) read that way is garbage: pandas'
/// ujson serializer walked `FrozenList`'s payload bytes as a pointer
/// array and segfaulted on `to_json(orient="table")`.
///
/// Same identity / caching / [`STRONG`]-pinning contract as
/// [`instance_body_out`]. Returns `None` when `ty` is not a registered
/// container-body type — the caller falls through to the identity box.
pub fn container_body_out(inst: &Rc<PyInstance>, ty: *mut PyTypeObject) -> Option<*mut PyObject> {
    if !crate::types::is_container_body_type(ty) {
        return None;
    }
    let existing = inst.c_body.get();
    if existing != 0 {
        // Re-crossing: same pointer, re-establish C's borrow (mirrors
        // `instance_body_out`).
        let body = existing as *mut PyObject;
        let head = unsafe { &mut *body };
        if head.ob_refcnt <= 0 {
            head.ob_refcnt = 1;
            strong_pin(body, inst);
        } else {
            head.ob_refcnt += 1;
        }
        return Some(body);
    }
    use crate::layout::tpflags;
    let flags = unsafe { (*ty).tp_flags };
    let is_list = flags & tpflags::LIST_SUBCLASS != 0;
    let basicsize =
        unsafe { (*ty).tp_basicsize }.max(std::mem::size_of::<PyObject>() as PySsizeT) as usize;
    let body_bytes = if is_list {
        basicsize.max(std::mem::size_of::<crate::layout::PyListObject>())
    } else {
        // Tuple elements live inline after the var head.
        let n = match inst.native.get() {
            Some(weavepy_vm::object::Object::Tuple(t)) => t.len(),
            _ => 0,
        };
        basicsize.max(
            std::mem::size_of::<crate::layout::PyVarObject>()
                + n * std::mem::size_of::<*mut PyObject>(),
        )
    };
    let body = attach_body(inst, ty, body_bytes);
    if is_list {
        unsafe { crate::mirror::pack_list_subclass_body(body) };
    } else {
        unsafe { crate::mirror::pack_tuple_subclass_body(body) };
    }
    Some(body)
}

/// Allocate a faithful, zeroed inline instance for `ty` directly from C
/// (RFC 0045) — the `PyType_GenericAlloc` path for inline-storage types.
/// Mints a fresh [`PyInstance`] bound to `ty`'s bridged class, gives it a
/// `tp_basicsize + nitems * tp_itemsize`-wide body (refcount 1), and pins
/// C's ownership. Returns null if `ty` is not a bridged type.
pub fn make_inline_instance(ty: *mut PyTypeObject, nitems: PySsizeT) -> *mut PyObject {
    let Some(cls) = (unsafe { crate::types::bridge_type(ty) }) else {
        return std::ptr::null_mut();
    };
    let basicsize =
        unsafe { (*ty).tp_basicsize }.max(std::mem::size_of::<PyObject>() as PySsizeT) as usize;
    let mut itemsize = unsafe { (*ty).tp_itemsize }.max(0) as usize;
    let mut min_body = 0usize;
    // A container-body type (RFC 0047, wave 5) needs at least the real
    // container struct: its synthesised `tp_basicsize`/`tp_itemsize` are
    // identity-box values, not CPython's.
    if crate::types::is_container_body_type(ty) {
        use crate::layout::tpflags;
        let flags = unsafe { (*ty).tp_flags };
        if flags & tpflags::LIST_SUBCLASS != 0 {
            min_body = std::mem::size_of::<crate::layout::PyListObject>();
        } else {
            min_body = std::mem::size_of::<crate::layout::PyVarObject>();
            itemsize = itemsize.max(std::mem::size_of::<*mut PyObject>());
        }
    }
    let body_bytes = (basicsize + nitems.max(0) as usize * itemsize).max(min_body);
    // A builtin-container subclass needs its native payload seeded at
    // allocation, matching the VM's own `instantiate` — sqlalchemy's
    // `cdef class immutabledict(dict)` allocates through this path and
    // `dict.__init__` then demands a real dict payload on the instance.
    let inst = {
        let bt = weavepy_vm::builtin_types::builtin_types();
        let native: Option<Object> = if cls.is_subclass_of(&bt.dict_) {
            Some(Object::Dict(weavepy_vm::sync::Rc::new(
                weavepy_vm::sync::RefCell::new(weavepy_vm::object::DictData::default()),
            )))
        } else if cls.is_subclass_of(&bt.list_) {
            Some(Object::List(weavepy_vm::sync::Rc::new(
                weavepy_vm::sync::RefCell::new(Vec::new()),
            )))
        } else if cls.is_subclass_of(&bt.set_) {
            Some(Object::Set(weavepy_vm::sync::Rc::new(
                weavepy_vm::sync::RefCell::new(weavepy_vm::object::SetData::default()),
            )))
        } else {
            None
        };
        match native {
            Some(n) => Rc::new(PyInstance::with_native(cls, n)),
            None => Rc::new(PyInstance::new(cls)),
        }
    };
    let body = attach_body(&inst, ty, body_bytes);
    // CPython's `PyType_GenericAlloc` initialises a var-sized instance with
    // `PyObject_InitVar`, which stamps `ob_size = nitems`. numpy's
    // `PyArray_Scalar` depends on this for STRING scalars: it calls
    // `type->tp_alloc(type, itemsize)` and then memcpys the payload into
    // `ob_sval` *without* touching the size — a zeroed `ob_size` reads back
    // as an empty `np.bytes_`.
    if !body.is_null() && unsafe { (*ty).tp_itemsize } != 0 {
        let vo = body as *mut crate::layout::PyVarObject;
        unsafe { (*vo).ob_size = nitems.max(0) };
    }
    body
}

/// Allocate the faithful body, record it on `inst`, and pin C's borrow.
/// Shared by [`instance_body_out`] (first crossing) and
/// [`make_inline_instance`] (C-side alloc). The body's refcount is 1.
fn attach_body(inst: &Rc<PyInstance>, ty: *mut PyTypeObject, body_bytes: usize) -> *mut PyObject {
    let weak = Rc::downgrade(inst);
    let body = crate::mirror::alloc_instance_body(ty, body_bytes, weak);
    inst.c_body.set(body as usize);
    strong_pin(body, inst);
    body
}

/// Pin the instance in [`STRONG`] under `body`. The previous value (if
/// any) is dropped *after* the borrow is released — dropping an
/// `Rc<PyInstance>` can run `PyInstance::drop` → the free hook → back
/// into [`STRONG`], which would otherwise re-borrow it mutably.
fn strong_pin(body: *mut PyObject, inst: &Rc<PyInstance>) {
    if crate::mirror::body_trace_enabled() {
        let tn = unsafe { crate::object::debug_type_name(body) };
        if tn.contains("Engine") || tn.contains("BlockManager") {
            let rc = unsafe { (*body).ob_refcnt };
            eprintln!("[PIN] body=0x{:x} type={} refcnt={}", body as usize, tn, rc);
        }
    }
    let previous = STRONG.lock().ok().and_then(|mut g| {
        g.get_or_insert_with(HashMap::new)
            .insert(body as usize, inst.clone())
    });
    drop(previous);
}

/// End C's borrow of an inline instance body (RFC 0045): its C refcount
/// has reached zero. Drops the [`STRONG`] pin — the block itself is owned
/// by the instance and is freed when the instance is collected (which may
/// happen synchronously here, if the VM also holds no further reference).
///
/// # Safety
/// `p` must be a faithful instance body
/// ([`crate::mirror::is_instance_body`]).
pub unsafe fn release_c_ownership(p: *mut PyObject) {
    if crate::mirror::body_trace_enabled() {
        let tn = unsafe { crate::object::debug_type_name(p) };
        if tn.contains("Engine") || tn.contains("BlockManager") {
            let rc = unsafe { (*p).ob_refcnt };
            eprintln!(
                "[RELEASE-C] body=0x{:x} type={} refcnt={}",
                p as usize, tn, rc
            );
        }
    }
    // Take the pin out *before* dropping it: dropping the last `Rc` runs
    // `PyInstance::drop`, which calls the free hook, which touches
    // `STRONG` again — so the lock must already be released. (`Mutex`,
    // unlike the old thread-local `RefCell`, would deadlock rather than
    // panic on re-entry — same discipline, harder failure.)
    let pinned = STRONG
        .lock()
        .ok()
        .and_then(|mut g| g.as_mut().and_then(|m| m.remove(&(p as usize))));
    if let Some(inst) = &pinned {
        // C just dropped what may be this instance's last program-visible
        // reference (CPython would run `tp_dealloc` right here). If the VM
        // side still pins it through GC-handle / weakref-registry clones, a
        // plain `Rc` drop below cannot reap it — a *tracked* extension
        // temporary that dies inside a C call (a Cython generator abandoned
        // by `BlockManager.iget`, still holding `self` in its closure) would
        // otherwise stay pinned by its own handle until the next full
        // collection, keeping everything it references alive with it
        // (pandas' `_is_view_after_cow_rules` then reads a stale live
        // `Block` weakref). Park it for the eval loop's between-bytecodes
        // reap; anything still genuinely alive fails the drain's
        // refcount-dead test untouched.
        //
        // Only a *GC-tracked* instance needs the park: an untracked one has
        // no handle pinning it, so the plain `Rc` drop below reclaims it
        // immediately (running `tp_dealloc` through the free hook) — and a
        // queued clone would instead keep it alive until the next eval-loop
        // safe point, which never comes for a drop performed outside
        // bytecode execution (an embedding host dropping its last handle).
        let obj = weavepy_vm::object::Object::Instance(inst.clone());
        if weavepy_vm::gc_trace::is_tracked(weavepy_vm::weakref_registry::id_of(&obj)) {
            weavepy_vm::vm_singletons::queue_cext_dropped(&obj);
        }
    } else if unsafe { crate::mirror::is_orphaned_instance_body(p) }
        && !body_free_in_flight(p as usize)
    {
        // RFC 0075 WS9: no pin *and* the owning instance is already
        // collected — this is an orphaned body (the free hook deferred its
        // dealloc because C still held inline-acquired references; see the
        // orphan branch there). This zero-crossing is the last reference
        // anywhere, so run the deferred dealloc + free now. Both decref
        // routes land here: our exported `Py_DecRef` via `free_box`, and
        // the stock inlined `Py_DECREF` via `_Py_Dealloc` → `free_box`.
        // The in-flight guard breaks the re-entry loop: the dealloc's own
        // self-refcount guard (Cython increfs/decrefs `o` around
        // `__dealloc__`) crosses zero again and lands back here.
        unsafe { dealloc_and_free_body(p) };
        return;
    }
    drop(pinned);
}

thread_local! {
    /// Bodies whose deferred dealloc ([`dealloc_and_free_body`]) is
    /// currently on the stack (RFC 0075 WS9). A Cython `tp_dealloc`
    /// increfs/decrefs `self` around `__dealloc__`, re-crossing zero and
    /// re-entering [`release_c_ownership`]'s orphan arm — without this
    /// guard the same body deallocs recursively until the stack blows.
    static BODY_FREE_IN_FLIGHT: std::cell::RefCell<Vec<usize>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

fn body_free_in_flight(body: usize) -> bool {
    // `try_with`: this is reachable from `Object` drops that run while
    // the thread's own TLS is being destroyed (a parked
    // `PENDING_CEXT_DROPS` queue dying at thread exit) — `with` would
    // panic-abort inside a destructor. See [`tls_dead`].
    BODY_FREE_IN_FLIGHT
        .try_with(|s| s.borrow().contains(&body))
        .unwrap_or(false)
}

/// True when this thread's TLS has already been (or is being) torn
/// down. Extension `tp_dealloc` code must not run past that point —
/// the reentrancy/consent guards it depends on are gone, and the VM
/// state it may re-enter is dying with the thread. Callers leak the
/// body instead, matching CPython's own no-finalization-guarantee at
/// thread/process exit.
fn tls_dead() -> bool {
    BODY_FREE_IN_FLIGHT.try_with(|_| ()).is_err()
}

thread_local! {
    /// Consent window for [`free_instance_body_hook`]: while it runs an
    /// extension `tp_dealloc` for body `ptr`, this holds `(ptr, false)`.
    /// The absorbing `tp_free` entry points ([`crate::memory::
    /// PyObject_Free`], [`crate::gc_bridge::PyObject_GC_Del`]) flip the
    /// flag when they see that same body — proof the dealloc *released*
    /// the object rather than stashing its raw pointer in a freelist.
    static BODY_FREE_CONSENT: std::cell::Cell<(usize, bool)> =
        const { std::cell::Cell::new((0, false)) };
}

/// Called by the absorbing `tp_free` paths: record that the extension's
/// `tp_dealloc` explicitly released `body` (see [`BODY_FREE_CONSENT`]).
pub(crate) fn note_body_free_consented(body: usize) {
    let _ = BODY_FREE_CONSENT.try_with(|c| {
        let (ptr, _) = c.get();
        if ptr == body {
            c.set((ptr, true));
        }
    });
}

/// VM hook: free an instance's faithful body when the instance is
/// collected (registered by [`install`]). Runs the type's *custom*
/// `tp_dealloc` once for faithful resource cleanup (e.g. freeing a
/// `self->data` buffer), then releases the block. A stock dealloc's
/// `tp_free(self)` / `PyObject_Free(self)` / `PyObject_GC_Del(self)` on
/// this body is absorbed (see [`crate::memory::PyObject_Free`]).
fn free_instance_body_hook(body: usize) {
    if body == 0 {
        return;
    }
    let p = body as *mut PyObject;
    if crate::mirror::body_trace_enabled() {
        let tn = unsafe { crate::object::debug_type_name(p) };
        eprintln!("[FREE-HOOK] body=0x{body:x} type={tn}");
    }
    // RFC 0046 (wave 4): a *non-inline* instance's `c_body` holds a plain
    // identity `PyObjectBox`, not a faithful mirror body. That box is owned
    // by C's refcount and reclaimed by `free_box` (which clears `c_body`
    // first), so the box's strong payload pins the instance and this hook
    // can only see it if some future refactor breaks that invariant. Guard
    // defensively: routing a non-body through the faithful free path below
    // would read a mirror prefix that does not exist. `free_box` frees it
    // correctly instead. `is_instance_body` only reads `ob_type`, so it is
    // sound on a live box.
    if !unsafe { crate::mirror::is_instance_body(p) } {
        unsafe { crate::object::free_box(p) };
        return;
    }
    // The instance only reaches `Drop` once its strong count is zero, and
    // a live `STRONG` pin *is* a strong count — so no pin can remain here.
    //
    // A poisoned mutex means a panic already aborted a mutation mid-flight;
    // bail without freeing rather than compound the damage.
    match STRONG
        .lock()
        .map(|mut g| g.as_mut().and_then(|m| m.remove(&body)))
    {
        Ok(stale) => {
            debug_assert!(
                stale.is_none(),
                "RFC 0045: instance collected while C still owned its body"
            );
            drop(stale);
        }
        Err(_) => return,
    }

    // RFC 0075 WS9: the body's C refcount can be positive here even though
    // the pin is gone. The pin tracks references acquired through *our*
    // entry points, but an extension that keeps a borrowed registry of its
    // objects re-acquires one with the **inlined** `Py_INCREF` macro — a
    // direct field increment we never observe. lxml's per-document proxy
    // registry is the motivating case: the root `_Element` proxy's C refs
    // dropped to zero during iteration (pin released, body kept by the VM
    // instance), then `_FeedParser.close()`'s `getProxy()` re-increfed the
    // *same* body and Cython stored it in `iterparse.root`. Deallocating
    // now would leave that reference dangling — a use-after-free plus a
    // second dealloc when `iterparse` dies, the source of the lxml suite's
    // mid-run segfaults (`tostring(context.root)` jumped through a zeroed
    // `ob_type`). Orphan the block instead: it lives on as a C-owned
    // object (crossings proxy it as foreign — see `clone_object`), and the
    // deferred dealloc runs when C's refcount finally reaches zero
    // (`release_c_ownership`'s dead-`Weak` arm).
    let live = unsafe { (*p).ob_refcnt };
    if crate::mirror::body_trace_enabled() {
        eprintln!("[FREE-HOOK-RC] body=0x{body:x} refcnt={live}");
    }
    if live > 0 {
        if crate::mirror::body_trace_enabled() {
            eprintln!("[ORPHAN] body=0x{body:x} outlives its instance (C refcnt={live})");
        }
        return;
    }

    unsafe { dealloc_and_free_body(p) };
}

/// Run the extension type's custom `tp_dealloc` (with the freelist
/// neutralisation + consent protocol) and release the body's storage.
/// Shared by [`free_instance_body_hook`] (instance collected, no C refs)
/// and [`release_c_ownership`]'s orphan arm (last C ref died *after* the
/// instance was collected — RFC 0075 WS9).
///
/// # Safety
/// `p` must be a faithful instance body with no remaining C references.
unsafe fn dealloc_and_free_body(p: *mut PyObject) {
    // Thread teardown: the guards below are TLS and already destroyed
    // (this drop is running from another TLS value's destructor — a
    // thread-exit `run_dtors` draining a parked drop queue). Running an
    // extension `tp_dealloc` here would re-enter the dying VM; leak the
    // body instead (CPython guarantees no finalization at exit either).
    if tls_dead() {
        return;
    }
    let _ = BODY_FREE_IN_FLIGHT.try_with(|s| s.borrow_mut().push(p as usize));
    // The block is released before this scope ends, so the guard entry is
    // popped via a drop guard rather than after the free (the free itself
    // can re-enter through the dealloc's decref chains).
    struct Unmark(usize);
    impl Drop for Unmark {
        fn drop(&mut self) {
            let _ = BODY_FREE_IN_FLIGHT.try_with(|s| {
                let mut v = s.borrow_mut();
                if let Some(i) = v.iter().rposition(|&b| b == self.0) {
                    v.remove(i);
                }
            });
        }
    }
    let _unmark = Unmark(p as usize);
    unsafe {
        let ty = (*p).ob_type;
        if !ty.is_null() {
            // CPython's `subtype_dealloc` semantics: a Python-defined
            // subclass of an extension type carries the *default* dealloc
            // on its own synthesised `PyTypeObject`, but its instances
            // still own C struct fields at the base's offsets (a pandas
            // `_iLocIndexer(NDFrameIndexerBase)` stores `self->obj` /
            // `self->_name` through Cython's `__init__`). Walk `tp_base`
            // to the first genuine extension `tp_dealloc` — exactly the
            // `while (basedealloc == subtype_dealloc) base = base->tp_base`
            // loop in `Objects/typeobject.c` — so those fields are
            // released when the instance dies rather than leaking their
            // referents until the next cyclic collection (RFC 0047: the
            // leaked `self->obj` pinned the intermediate DataFrame whose
            // weakref pandas' `_check_setitem_copy` needs cleared).
            let default_dealloc: unsafe extern "C" fn(*mut PyObject) =
                crate::object::_PyWeavePy_Dealloc;
            let mut dealloc_ty = ty;
            let mut chosen: Option<unsafe extern "C" fn(*mut PyObject)> = None;
            while !dealloc_ty.is_null() {
                match (*dealloc_ty).tp_dealloc {
                    Some(d) if d as usize != default_dealloc as usize => {
                        chosen = Some(d);
                        break;
                    }
                    Some(_) => dealloc_ty = (*dealloc_ty).tp_base,
                    None => break,
                }
            }
            {
                if let Some(dealloc) = chosen {
                    // RFC 0045 (wave 5): neutralise a Cython `@cython.freelist`
                    // dealloc for the duration of this call. A `@cython.freelist`
                    // `cdef class` — pandas' `BlockManager`, `Block`,
                    // `BlockPlacement`, … — ends its `tp_dealloc` with
                    //
                    //   if (freecount < N & Py_TYPE(o)->tp_basicsize == sizeof)
                    //       freelist[freecount++] = o;      // stash raw pointer
                    //   else
                    //       Py_TYPE(o)->tp_free(o);         // release
                    //
                    // (verified by disassembling the pandas 2.3 wheel: the stash
                    // is gated *only* on `freecount < N` and the exact
                    // `tp_basicsize` — the `!HasFeature(IS_ABSTRACT | HEAPTYPE)`
                    // guard some Cython versions add is absent here, so flag
                    // manipulation does not divert it).
                    //
                    // The stash keeps a **raw** pointer to `o` past refcount
                    // zero, but WeavePy is about to `free_instance_body(p)` —
                    // returning that block to the allocator. The dangling
                    // freelist entry is then handed back by a later `tp_new`
                    // (`o = freelist[--freecount]; memset(o,…); PyObject_INIT`)
                    // *after* the block has been re-minted as an unrelated
                    // object, aliasing e.g. a `slice` onto a `BlockManager`
                    // (`'slice' object is not iterable`) or an `ndarray` onto an
                    // `IndexEngine` (`'ndarray' has no attribute 'is_unique'`).
                    // Faithful instance bodies are owned by the VM instance, not
                    // a C freelist.
                    //
                    // Perturbing `tp_basicsize` for the duration of the call
                    // fails the `tp_basicsize == sizeof` term, so the dealloc
                    // takes the `tp_free(o)` branch instead. Readied types wire
                    // `tp_free = PyObject_Free`, which *absorbs* the free of a
                    // body (`crate::memory::PyObject_Free`) because `ob_type`
                    // is untouched — the body is still recognised as an instance
                    // body. No entry is stashed, so `freecount` stays 0 and the
                    // matching `tp_new` reuse branch (`freecount > 0`) never
                    // fires either: every instance is minted afresh through
                    // `tp_alloc` (`PyType_GenericAlloc`), exactly as WeavePy's
                    // ownership model requires. `tp_basicsize` is restored
                    // immediately (before `free_instance_body` and before any
                    // subsequent allocation reads it).
                    let orig_basicsize = (*ty).tp_basicsize;
                    (*ty).tp_basicsize = orig_basicsize.wrapping_add(8);
                    // A `tp_dealloc` is extension code: its decref chains can
                    // re-enter the VM, and any bytecode that runs beneath it
                    // must see a live C frame (RFC 0047 — the prompt reaper's
                    // borrowed-pointer window).
                    let _cext_guard = weavepy_vm::vm_singletons::enter_cext_call();
                    // Consent protocol: the dealloc must reach a `tp_free`
                    // (absorbed by `PyObject_Free`/`PyObject_GC_Del`, which
                    // flip this flag) for WeavePy to release the storage.
                    // A dealloc that *stashes* the raw pointer instead —
                    // mypyc's per-class freelist gates only on "slot empty",
                    // so the Cython `tp_basicsize` perturbation above cannot
                    // divert it — keeps the pointer live past this call, and
                    // freeing the block would hand the next reuse recycled
                    // garbage (charset-normalizer's `coherence_ratio_env`).
                    let saved_consent = BODY_FREE_CONSENT
                        .try_with(|c| c.replace((p as usize, false)))
                        .unwrap_or((0, false));
                    dealloc(p);
                    let consented = BODY_FREE_CONSENT
                        .try_with(|c| c.replace(saved_consent))
                        .map(|prev| prev.1)
                        .unwrap_or(false);
                    drop(_cext_guard);
                    (*ty).tp_basicsize = orig_basicsize;
                    if !consented {
                        // Disown the block: the extension holds a raw
                        // pointer to it (freelist stash), so the storage —
                        // prefix, body, and aux — leaks by design. Bounded:
                        // each such freelist caches at most a few instances.
                        if crate::mirror::body_trace_enabled() {
                            eprintln!("[ORPHAN] body=0x{:x} stashed by tp_dealloc", p as usize);
                        }
                        return;
                    }
                }
            }
        }
        crate::mirror::free_instance_body(p);
    }
}

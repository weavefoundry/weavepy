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

use std::cell::RefCell;
use std::collections::HashMap;

use weavepy_vm::sync::Rc;
use weavepy_vm::types::PyInstance;

use crate::object::{PyObject, PySsizeT};
use crate::types::PyTypeObject;

thread_local! {
    /// C-side ownership of inline instances: `body pointer -> Rc<PyInstance>`.
    ///
    /// An entry exists exactly while the body's C refcount is positive —
    /// i.e. while a C extension holds a reference. It is the strong edge
    /// that keeps the native instance (and therefore its faithful body)
    /// alive for C even after the VM has dropped its last reference. The
    /// [`MirrorPrefix`](crate::mirror::MirrorPrefix)'s back-reference is a
    /// `Weak`, so this map is the *only* strong C→instance link and there
    /// is no ownership cycle.
    static STRONG: RefCell<HashMap<usize, Rc<PyInstance>>> =
        RefCell::new(HashMap::new());
}

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
        return body;
    }

    // Re-crossing: the body already exists and outlived any previous C
    // reference (the instance owns it). Re-establish C's borrow.
    let body = existing as *mut PyObject;
    let head = unsafe { &mut *body };
    if head.ob_refcnt <= 0 {
        head.ob_refcnt = 1;
        strong_pin(body, inst);
    } else {
        head.ob_refcnt += 1;
    }
    body
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
    let itemsize = unsafe { (*ty).tp_itemsize }.max(0) as usize;
    let body_bytes = basicsize + nitems.max(0) as usize * itemsize;
    let inst = Rc::new(PyInstance::new(cls));
    attach_body(&inst, ty, body_bytes)
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
    let previous = STRONG.with(|m| m.borrow_mut().insert(body as usize, inst.clone()));
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
    // Take the pin out *before* dropping it: dropping the last `Rc` runs
    // `PyInstance::drop`, which calls the free hook, which touches
    // `STRONG` again — so the borrow must already be released.
    //
    // `try_with`, not `with`: at thread/process teardown the `STRONG`
    // thread-local may itself be mid-destruction, and a plain `.with`
    // there panics (`AccessError`) — which, in a `Drop`, aborts the
    // process (RFC 0046, wave 4). If the map is gone the pins are gone
    // too; there is nothing to remove.
    let pinned = STRONG
        .try_with(|m| m.borrow_mut().remove(&(p as usize)))
        .ok()
        .flatten();
    drop(pinned);
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
    // `try_with`, not `with`: at thread/process teardown this hook fires
    // *from within* the `STRONG` map's own destructor (dropping its
    // pinned `Rc<PyInstance>`s runs `PyInstance::drop` → here). The map is
    // then mid-destruction, so a plain `.with` panics with `AccessError`
    // — and panicking in a TLS destructor aborts the process (the exit
    // 133 / "thread local panicked on drop" abort, RFC 0046 wave 4). When
    // the TLS is gone the process is exiting; the OS reclaims the block,
    // so bail without freeing rather than touch more (possibly destroyed)
    // capi thread-locals (`unregister_minted`, the mirror registry).
    match STRONG.try_with(|m| m.borrow_mut().remove(&body)) {
        Ok(stale) => {
            debug_assert!(
                stale.is_none(),
                "RFC 0045: instance collected while C still owned its body"
            );
            drop(stale);
        }
        Err(_) => return,
    }

    unsafe {
        let ty = (*p).ob_type;
        if !ty.is_null() {
            if let Some(dealloc) = (*ty).tp_dealloc {
                // Skip our own default dealloc (it would recurse into
                // `free_box`); run only a genuine extension `tp_dealloc`
                // for faithful resource cleanup.
                let default_dealloc: unsafe extern "C" fn(*mut PyObject) =
                    crate::object::_PyWeavePy_Dealloc;
                if dealloc as usize != default_dealloc as usize {
                    dealloc(p);
                }
            }
        }
        crate::mirror::free_instance_body(p);
    }
}

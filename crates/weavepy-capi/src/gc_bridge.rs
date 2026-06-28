//! Cycle-collector bridge for C extension types (RFC 0044, WS4).
//!
//! WeavePy's tracing collector already walks an instance's `__dict__`
//! and `__slots__`, so a stock GC type that parks its children there is
//! *already* collectable. The gap this module closes is the other kind
//! of edge: a `Py_TPFLAGS_HAVE_GC` type that stores child references in
//! **C-managed memory** — a side table, a malloc'd struct, anything the
//! VM cannot see. CPython surfaces those edges through the type's
//! `tp_traverse` (enumerate children) and `tp_clear` (drop them to break
//! a cycle) slots.
//!
//! We register one [`traverse`](weavepy_vm::gc_trace::register_traverse)
//! and one [`clear`](weavepy_vm::gc_trace::register_clear) callback with
//! the VM. They fire for any `Object::Instance` whose class is bridged
//! from a C `PyTypeObject` (static, `PyType_FromSpec`, or readied via
//! `PyType_Ready`) that carries the corresponding slot, marshal the
//! instance back into a borrowed `PyObject *`, and invoke the C slot:
//!
//!   * `tp_traverse(self, visit, arg)` — we hand it a [`gc_visit`]
//!     trampoline that turns each reported child `PyObject *` back into
//!     an [`Object`] and forwards it to the collector's visitor.
//!   * `tp_clear(self)` — called during the collector's clear phase so
//!     the extension drops the references it owns.
//!
//! The slot pointers live at fixed offsets (184 / 192) inside the
//! faithful 416-byte `PyTypeObject` prefix, so reading them is sound for
//! every bridged type pointer regardless of how the type was created.

use std::os::raw::{c_int, c_void};

use weavepy_vm::object::Object;

use crate::object::{PyObject, PySsizeT};
use crate::types::PyTypeObject;

/// `int (*visitproc)(PyObject *object, void *arg)`.
type VisitProc = unsafe extern "C" fn(*mut PyObject, *mut c_void) -> c_int;
/// `int (*traverseproc)(PyObject *self, visitproc visit, void *arg)`.
type TraverseProc = unsafe extern "C" fn(*mut PyObject, VisitProc, *mut c_void) -> c_int;
/// `int (*inquiry)(PyObject *self)` — the shape of `tp_clear`.
type InquiryProc = unsafe extern "C" fn(*mut PyObject) -> c_int;

/// The live `PyTypeObject *` backing `obj`, if `obj` is an instance of a
/// C-bridged type. (`None` for built-in scalars and pure-Python
/// classes, which never appear as `Object::Instance` over a C type.)
fn instance_type_ptr(obj: &Object) -> Option<*mut PyTypeObject> {
    match obj {
        Object::Instance(i) => crate::types::type_ptr_for_class(&i.cls()),
        _ => None,
    }
}

/// Trampoline handed to `tp_traverse`. `arg` is a thin pointer to the
/// collector's `&mut dyn FnMut(&Object)` visitor (see [`traverse`]).
unsafe extern "C" fn gc_visit(child: *mut PyObject, arg: *mut c_void) -> c_int {
    if child.is_null() || arg.is_null() {
        return 0;
    }
    // `arg` points at the `&mut dyn FnMut(&Object)` parked on
    // `traverse`'s stack; reborrow it and forward the child.
    let visit = unsafe { &mut *(arg as *mut &mut dyn FnMut(&Object)) };
    let obj = unsafe { crate::object::clone_object(child) };
    (*visit)(&obj);
    0
}

/// VM traverse callback: invoke the C `tp_traverse` so children held in
/// C-managed memory are reported to the collector.
fn traverse(obj: &Object, visit: &mut dyn FnMut(&Object)) {
    let Some(ty) = instance_type_ptr(obj) else {
        return;
    };
    let slot = unsafe { (*ty).tp_traverse };
    if slot.is_null() {
        return;
    }
    let func: TraverseProc = unsafe { std::mem::transmute::<*mut c_void, TraverseProc>(slot) };

    // Borrow the instance into C as its declared type. `into_owned_with_type`
    // shares the same underlying `Object`, so an attribute read inside
    // `tp_traverse` (e.g. to locate this node's side-table slot) sees the
    // genuine instance dict.
    let self_box = crate::object::into_owned_with_type(obj.clone(), ty);

    // Park the (fat) visitor behind a thin pointer we can smuggle through
    // the C `void *arg`.
    let mut visit_ref: &mut dyn FnMut(&Object) = visit;
    let arg = &mut visit_ref as *mut &mut dyn FnMut(&Object) as *mut c_void;

    crate::interp::ensure_active(|| unsafe {
        func(self_box, gc_visit, arg);
    });
    unsafe { crate::object::Py_DecRef(self_box) };
}

/// VM clear callback: invoke the C `tp_clear` so the extension drops the
/// child references it owns, breaking cycles routed through C memory.
fn clear(obj: &Object) {
    let Some(ty) = instance_type_ptr(obj) else {
        return;
    };
    let slot = unsafe { (*ty).tp_clear };
    if slot.is_null() {
        return;
    }
    let func: InquiryProc = unsafe { std::mem::transmute::<*mut c_void, InquiryProc>(slot) };
    let self_box = crate::object::into_owned_with_type(obj.clone(), ty);
    crate::interp::ensure_active(|| unsafe {
        func(self_box);
    });
    unsafe { crate::object::Py_DecRef(self_box) };
}

/// True when `obj` is an instance whose C type carries a `tp_traverse`.
fn has_traverse(obj: &Object) -> bool {
    instance_type_ptr(obj)
        .map(|ty| !unsafe { (*ty).tp_traverse }.is_null())
        .unwrap_or(false)
}

/// True when `obj` is an instance whose C type carries a `tp_clear`.
fn has_clear(obj: &Object) -> bool {
    instance_type_ptr(obj)
        .map(|ty| !unsafe { (*ty).tp_clear }.is_null())
        .unwrap_or(false)
}

/// Register the traverse/clear bridges with the VM collector. Called
/// once from [`crate::interp::ensure_initialised`], before any extension
/// code (hence any C GC type) can run.
pub fn install() {
    weavepy_vm::gc_trace::register_traverse(has_traverse, traverse);
    weavepy_vm::gc_trace::register_clear(has_clear, clear);
}

// ====================================================================
// GC allocation + tracking C-API (RFC 0044, WS4)
//
// A `Py_TPFLAGS_HAVE_GC` extension type allocates its instances through
// `PyObject_GC_New` / `_PyObject_GC_New` (never the plain object
// allocator), then enrols them with the collector via
// `PyObject_GC_Track` once their fields are initialised, and finally
// frees them with `PyObject_GC_Del` from `tp_dealloc`. We back the whole
// family by the existing `PyType_GenericAlloc` storage model and the VM
// collector's `track` / `untrack` registry.
// ====================================================================

/// `_PyObject_GC_New(tp)` — allocate a GC-tracked-capable instance.
///
/// Allocation only: like CPython, the caller is responsible for the
/// follow-up `PyObject_GC_Track`. The storage and header are identical
/// to [`PyType_GenericAlloc`](crate::genericalloc::PyType_GenericAlloc),
/// so a readied type's instance is seeded with a real `Object::Instance`
/// payload.
///
/// # Safety
/// `tp` must be a valid `PyTypeObject *` (typically a static GC type
/// finalised through `PyType_Ready`).
#[no_mangle]
pub unsafe extern "C" fn _PyObject_GC_New(tp: *mut PyTypeObject) -> *mut PyObject {
    unsafe { crate::genericalloc::PyType_GenericAlloc(tp, 0) }
}

/// `_PyObject_GC_NewVar(tp, nitems)` — variable-size GC allocation.
///
/// # Safety
/// Same contract as [`_PyObject_GC_New`].
#[no_mangle]
pub unsafe extern "C" fn _PyObject_GC_NewVar(
    tp: *mut PyTypeObject,
    nitems: PySsizeT,
) -> *mut PyObject {
    unsafe { crate::genericalloc::PyType_GenericAlloc(tp, nitems) }
}

/// `PyObject_GC_Track(op)` — start tracking `op` with the cycle
/// collector. Enrolling the bridged `Object::Instance` makes a cycle
/// routed through this object collectable, including edges that only its
/// `tp_traverse` can surface.
///
/// # Safety
/// `op` must be a live object pointer previously produced by the GC
/// allocator (or any WeavePy box / mirror).
#[no_mangle]
pub unsafe extern "C" fn PyObject_GC_Track(op: *mut c_void) {
    if op.is_null() {
        return;
    }
    let obj = unsafe { crate::object::clone_object(op as *mut PyObject) };
    if matches!(obj, Object::Instance(_)) {
        weavepy_vm::gc_trace::track(obj);
    }
}

/// `PyObject_GC_UnTrack(op)` — stop tracking `op` (the inverse of
/// [`PyObject_GC_Track`]). Idempotent and safe to call on an object that
/// was never tracked, matching CPython's contract for `tp_dealloc`.
///
/// # Safety
/// Same contract as [`PyObject_GC_Track`].
#[no_mangle]
pub unsafe extern "C" fn PyObject_GC_UnTrack(op: *mut c_void) {
    if op.is_null() {
        return;
    }
    let obj = unsafe { crate::object::clone_object(op as *mut PyObject) };
    weavepy_vm::gc_trace::untrack(&obj);
}

/// `PyObject_GC_IsTracked(op)` — 1 if `op` is currently tracked by the
/// cycle collector, else 0 (CPython 3.9+ public API).
///
/// # Safety
/// Same contract as [`PyObject_GC_Track`].
#[no_mangle]
pub unsafe extern "C" fn PyObject_GC_IsTracked(op: *mut c_void) -> c_int {
    if op.is_null() {
        return 0;
    }
    let obj = unsafe { crate::object::clone_object(op as *mut PyObject) };
    let id = weavepy_vm::weakref_registry::id_of(&obj);
    c_int::from(weavepy_vm::gc_trace::is_tracked(id))
}

/// `PyObject_GC_Del(op)` — free a GC object's storage (a GC type's
/// `tp_free`). Untracks defensively, then releases the box/mirror.
///
/// # Safety
/// `op` must be a live object whose refcount has reached zero (exactly
/// how `tp_dealloc` invokes `tp_free`).
#[no_mangle]
pub unsafe extern "C" fn PyObject_GC_Del(op: *mut c_void) {
    if op.is_null() {
        return;
    }
    let p = op as *mut PyObject;
    // RFC 0045 (wave 3): a faithful inline instance body is owned by its
    // native instance. A stock GC-type `tp_dealloc` that ends with
    // `PyObject_GC_Del(self)` is absorbed — the block is reclaimed when
    // the instance is collected, not here.
    if unsafe { crate::mirror::is_instance_body(p) } {
        return;
    }
    let obj = unsafe { crate::object::clone_object(p) };
    weavepy_vm::gc_trace::untrack(&obj);
    drop(obj);
    // Release the storage through the canonical box/mirror free path.
    unsafe { crate::object::_PyWeavePy_Dealloc(p) };
}

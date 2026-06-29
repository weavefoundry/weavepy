//! RFC 0047 (wave 5): faithful `inherit_slots`.
//!
//! CPython's `PyType_Ready` finishes by running `inherit_slots`
//! (`Objects/typeobject.c`): every `tp_*` function slot and method-suite
//! entry the subtype leaves NULL is copied down from its base, so that an
//! inlined `Py_TYPE(self)->tp_repr(self)` on a *subclass* resolves to the
//! function the base defined. Waves 1-4 did **not** do this: a readied
//! subtype carried only the slots it spelled out itself, and inherited
//! behaviour was reached *only* through the bridged type's MRO (the
//! synthesised dunder shims). That is correct for Python-level dispatch
//! (`sub + other` finds `Base.__add__` via the MRO) but wrong for the
//! dominant Cython idiom: Cython-generated code reads
//! `Py_TYPE(obj)->tp_as_number->nb_add` (and friends) **directly off the
//! C struct**, with no MRO walk. On a subclass whose `tp_as_number` was
//! NULL that is a NULL-deref. RFC 0046 §2.7 shipped a per-call `tp_base`
//! walk as a stop-gap for the repr/str path and named the real fix as
//! wave-5 work; this module is that fix.
//!
//! ## What this bakes in, and where
//!
//! At `PyType_Ready` time, immediately after the bridged type is built
//! (so the type dict still carries only the subtype's *own* dunders —
//! the inherited ones are reached through the MRO, exactly as CPython),
//! [`inherit_slots`] copies, from the **immediate base only**:
//!
//! 1. **The decoded [`SlotTable`].** Every slot id the subtype left NULL
//!    is filled from the base's table, so the direct-table-read dispatch
//!    paths (the buffer protocol, vectorcall, `tp_descr_get`/`set`, the GC
//!    bridge) and the `has_*_protocol` queries see the inherited slot.
//! 2. **The faithful `PyTypeObject` struct.** Every NULL direct function
//!    slot and every NULL/partial method suite (`tp_as_number`, …) is
//!    filled, so an extension's inlined `Py_TYPE(self)->tp_*` read lands
//!    on the inherited function.
//!
//! Copying only from the *immediate* base is sufficient and complete:
//! `PyType_Ready` readies a type's base before the type itself
//! (`bridge_or_ready(tp_base)` during harvest), and each base was itself
//! run through `inherit_slots`, so the immediate base's table and struct
//! are already **fully flattened**. One level of copy therefore carries
//! the whole ancestor chain — the same invariant CPython relies on.

use core::ffi::c_int;
use core::ffi::c_void;
use core::mem::size_of;

use crate::layout;
use crate::slottable::{self, SlotTable};
use crate::types::PyTypeObject;

/// Bake every slot the base provides but the subtype `t` leaves NULL into
/// the subtype's decoded `table` and its faithful struct — the wave-5
/// equivalent of CPython's `inherit_slots`.
///
/// `base` is the subtype's `tp_base` (already readied + flattened). When
/// either pointer is null, or the base is a WeavePy-native built-in (whose
/// behaviour the VM provides through the MRO rather than C slots), this is
/// a no-op.
///
/// # Safety
/// `t` must be a freshly-harvested, writable faithful `PyTypeObject`;
/// `base` must be null or a valid (readied or built-in) `PyTypeObject`.
pub unsafe fn inherit_slots(t: *mut PyTypeObject, table: &mut SlotTable, base: *mut PyTypeObject) {
    if t.is_null() || base.is_null() {
        return;
    }

    // (A) Complete the decoded slot table from the base's flattened one.
    if let Some(base_table) = unsafe { slottable::slot_table_for(base) } {
        for id in 1..(slottable::SLOT_TABLE_SIZE as c_int) {
            if table.get(id).is_null() {
                let inherited = base_table.get(id);
                if !inherited.is_null() {
                    table.install(id, inherited.as_void());
                }
            }
        }
    }

    // (B) Bake the inherited direct slots + method suites into the struct.
    unsafe { inherit_struct(t, base) };
}

/// Copy a single raw function slot when the destination is NULL.
fn copy_void(dst: &mut *mut c_void, src: *mut c_void) {
    if dst.is_null() && !src.is_null() {
        *dst = src;
    }
}

/// Fill the subtype's NULL direct slots + method-suite pointers from the
/// (already-flattened) base's faithful struct.
unsafe fn inherit_struct(t: *mut PyTypeObject, base: *mut PyTypeObject) {
    let sub = unsafe { &mut *t };
    let b = unsafe { &*base };

    // `tp_dealloc` is an `Option<destructor>`, not a raw pointer. Inherit
    // it so a subclass instance is finalised through the base's destructor
    // (e.g. a base that frees a `malloc`'d buffer). The `PyType_Ready`
    // default (`_PyWeavePy_Dealloc`) only applies if it is *still* NULL.
    if sub.tp_dealloc.is_none() {
        sub.tp_dealloc = b.tp_dealloc;
    }

    copy_void(&mut sub.tp_repr, b.tp_repr);
    copy_void(&mut sub.tp_str, b.tp_str);
    copy_void(&mut sub.tp_hash, b.tp_hash);
    copy_void(&mut sub.tp_call, b.tp_call);
    copy_void(&mut sub.tp_getattr, b.tp_getattr);
    copy_void(&mut sub.tp_setattr, b.tp_setattr);
    copy_void(&mut sub.tp_getattro, b.tp_getattro);
    copy_void(&mut sub.tp_setattro, b.tp_setattro);
    copy_void(&mut sub.tp_iter, b.tp_iter);
    copy_void(&mut sub.tp_iternext, b.tp_iternext);
    copy_void(&mut sub.tp_richcompare, b.tp_richcompare);
    copy_void(&mut sub.tp_descr_get, b.tp_descr_get);
    copy_void(&mut sub.tp_descr_set, b.tp_descr_set);
    copy_void(&mut sub.tp_init, b.tp_init);
    copy_void(&mut sub.tp_new, b.tp_new);
    copy_void(&mut sub.tp_alloc, b.tp_alloc);
    copy_void(&mut sub.tp_free, b.tp_free);
    copy_void(&mut sub.tp_is_gc, b.tp_is_gc);
    copy_void(&mut sub.tp_del, b.tp_del);
    copy_void(&mut sub.tp_finalize, b.tp_finalize);
    copy_void(&mut sub.tp_traverse, b.tp_traverse);
    copy_void(&mut sub.tp_clear, b.tp_clear);
    copy_void(&mut sub.tp_vectorcall, b.tp_vectorcall);

    // The instance-layout offsets are inherited when the subtype adds no
    // storage of its own (the common pure-behaviour subclass).
    if sub.tp_dictoffset == 0 {
        sub.tp_dictoffset = b.tp_dictoffset;
    }
    if sub.tp_weaklistoffset == 0 {
        sub.tp_weaklistoffset = b.tp_weaklistoffset;
    }
    if sub.tp_vectorcall_offset == 0 {
        sub.tp_vectorcall_offset = b.tp_vectorcall_offset;
    }

    unsafe {
        inherit_suite(
            &mut sub.tp_as_number,
            b.tp_as_number,
            size_of::<layout::PyNumberMethods>(),
        );
        inherit_suite(
            &mut sub.tp_as_sequence,
            b.tp_as_sequence,
            size_of::<layout::PySequenceMethods>(),
        );
        inherit_suite(
            &mut sub.tp_as_mapping,
            b.tp_as_mapping,
            size_of::<layout::PyMappingMethods>(),
        );
        inherit_suite(
            &mut sub.tp_as_async,
            b.tp_as_async,
            size_of::<layout::PyAsyncMethods>(),
        );
        inherit_suite(
            &mut sub.tp_as_buffer,
            b.tp_as_buffer,
            size_of::<layout::PyBufferProcs>(),
        );
    }
}

/// Inherit one method suite.
///
/// * Subtype has no suite → **share** the base's pointer. The base's
///   suite is already flattened and the entries are read-only function
///   pointers, so sharing is sound and matches the inlined-read effect of
///   `inherit_slots` (`Py_TYPE(self)->tp_as_number->nb_add` now resolves).
/// * Subtype has its own (possibly partial) suite → fill its NULL
///   word-slots from the base's, in place — matching CPython's per-slot
///   `COPYSLOT`. Every field of a method suite is pointer-width, so a
///   word-by-word merge covers them all (including the reserved holes,
///   which are copied harmlessly).
unsafe fn inherit_suite(sub: &mut *mut c_void, base: *mut c_void, size: usize) {
    if base.is_null() {
        return;
    }
    if sub.is_null() {
        *sub = base;
        return;
    }
    let words = size / size_of::<*mut c_void>();
    let sp = (*sub) as *mut *mut c_void;
    let bp = base as *const *mut c_void;
    for i in 0..words {
        unsafe {
            let s = sp.add(i);
            if (*s).is_null() {
                let inherited = *bp.add(i);
                if !inherited.is_null() {
                    *s = inherited;
                }
            }
        }
    }
}

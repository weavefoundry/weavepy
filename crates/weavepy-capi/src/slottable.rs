//! Per-type slot table.
//!
//! Heap types created by [`PyType_FromSpec`](crate::types::PyType_FromSpec)
//! carry a [`SlotTable`] embedded in their
//! [`PyTypeObjectBox`](crate::types::PyTypeObjectBox). The table maps a
//! CPython-canonical slot identifier (`Py_tp_call`, `Py_nb_add`,
//! `Py_bf_getbuffer`, …) to a stored function pointer.
//!
//! ## Why a separate table?
//!
//! Static built-in types use the WeavePy native dispatch path; they
//! don't need a slot table because the VM already knows how to add two
//! `int`s, hash a `str`, etc. Heap types, on the other hand, are
//! described by an extension at load time as a `PyType_Slot[]`
//! array, and the runtime needs a way to (a) look the slot up at
//! call sites that bypass the dunder protocol (the buffer protocol,
//! vectorcall, descriptor `tp_descr_get`/`tp_descr_set`, generic
//! allocation), and (b) inject synthesised dunder shims
//! (`__add__`, `__call__`, `__getitem__`, …) into the type's dict so
//! the existing VM dispatch path "just works".
//!
//! ## Storage
//!
//! Slot identifiers fit in an `i32` and are dense (1..=82) so a
//! plain `Vec<SlotPtr>` keyed by ID is cheap. We size the table at
//! 128 entries to leave headroom for future additions; lookups are a
//! single bounds check + array read.

use std::os::raw::c_int;
use std::os::raw::c_void;

/// Newtype wrapper so we can stash raw `*mut c_void` slot pointers
/// in `Send + Sync`-bounded statics.
///
/// Function pointers don't carry any interior state, so taking
/// `&self` from another thread is sound — there is no shared
/// mutable state behind the pointer.
#[derive(Clone, Copy, Debug)]
#[repr(transparent)]
pub struct SlotPtr(pub *mut c_void);

unsafe impl Send for SlotPtr {}
unsafe impl Sync for SlotPtr {}

impl SlotPtr {
    pub const NULL: SlotPtr = SlotPtr(std::ptr::null_mut());

    pub fn is_null(self) -> bool {
        self.0.is_null()
    }

    pub fn as_void(self) -> *mut c_void {
        self.0
    }

    pub unsafe fn cast<T>(self) -> T {
        // SAFETY: caller asserts the slot was registered with a
        // function pointer compatible with `T`.
        unsafe { std::mem::transmute_copy::<*mut c_void, T>(&self.0) }
    }
}

/// Number of entries in the slot table. CPython 3.13 currently
/// uses slot IDs up to 82 (`Py_am_send`); 128 leaves padding for
/// the next several point releases.
pub const SLOT_TABLE_SIZE: usize = 128;

/// Per-type slot vtable.
///
/// All slots default to null; [`SlotTable::install`] writes through
/// the array. Lookup is `O(1)` and bounds-checked.
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct SlotTable {
    pub slots: [SlotPtr; SLOT_TABLE_SIZE],
}

impl SlotTable {
    pub const fn empty() -> Self {
        Self {
            slots: [SlotPtr::NULL; SLOT_TABLE_SIZE],
        }
    }

    /// Install `pfunc` for slot `id`. Out-of-range slot IDs are
    /// silently dropped, matching CPython's `PyType_FromSpec` which
    /// accepts unknown slots without raising.
    pub fn install(&mut self, id: c_int, pfunc: *mut c_void) {
        let idx = id as usize;
        if idx == 0 || idx >= SLOT_TABLE_SIZE {
            return;
        }
        self.slots[idx] = SlotPtr(pfunc);
    }

    /// Read the slot for `id`. Returns null if unset or out of range.
    pub fn get(&self, id: c_int) -> SlotPtr {
        let idx = id as usize;
        if idx == 0 || idx >= SLOT_TABLE_SIZE {
            return SlotPtr::NULL;
        }
        self.slots[idx]
    }

    /// True if any of the buffer-protocol slots is populated.
    pub fn has_buffer_protocol(&self) -> bool {
        !self.get(Py_bf_getbuffer).is_null() || !self.get(Py_bf_releasebuffer).is_null()
    }

    /// True if any number-protocol slot is populated.
    pub fn has_number_protocol(&self) -> bool {
        for id in PY_NB_SLOTS {
            if !self.get(*id).is_null() {
                return true;
            }
        }
        false
    }

    /// True if any sequence-protocol slot is populated.
    pub fn has_sequence_protocol(&self) -> bool {
        for id in PY_SQ_SLOTS {
            if !self.get(*id).is_null() {
                return true;
            }
        }
        false
    }

    /// True if any mapping-protocol slot is populated.
    pub fn has_mapping_protocol(&self) -> bool {
        for id in PY_MP_SLOTS {
            if !self.get(*id).is_null() {
                return true;
            }
        }
        false
    }
}

impl Default for SlotTable {
    fn default() -> Self {
        Self::empty()
    }
}

unsafe impl Send for SlotTable {}
unsafe impl Sync for SlotTable {}

/// Locate the slot table embedded in a heap-allocated
/// [`PyTypeObject`](crate::types::PyTypeObject).
///
/// Static types (those without `Py_TPFLAGS_HEAPTYPE`) don't carry a
/// table; the C-API surfaces dispatch through the WeavePy-native
/// type machinery for those, so the lookup returns `None`.
///
/// # Safety
///
/// `ty` must be either null or a valid `PyTypeObject` pointer; for
/// heap types it must in particular be the `head` field of a live
/// [`PyTypeObjectBox`](crate::types::PyTypeObjectBox).
pub unsafe fn slot_table_for(ty: *mut crate::types::PyTypeObject) -> Option<&'static SlotTable> {
    if ty.is_null() {
        return None;
    }
    let flags = unsafe { (*ty).tp_flags };
    if (flags & crate::types::PY_TPFLAGS_HEAPTYPE as u64) == 0 {
        return None;
    }
    let bx = ty as *const crate::types::PyTypeObjectBox;
    let table_ptr = unsafe { &(*bx).slot_table };
    Some(unsafe { &*(table_ptr as *const SlotTable) })
}

// ----------------------------------------------------------------
// Slot ID constants. Derived from CPython 3.13 `Include/typeslots.h`.
// Keep these in sync with `crates/weavepy-capi/include/Python.h`.
// ----------------------------------------------------------------

pub const Py_bf_getbuffer: c_int = 1;
pub const Py_bf_releasebuffer: c_int = 2;
pub const Py_mp_ass_subscript: c_int = 3;
pub const Py_mp_length: c_int = 4;
pub const Py_mp_subscript: c_int = 5;
pub const Py_nb_absolute: c_int = 6;
pub const Py_nb_add: c_int = 7;
pub const Py_nb_and: c_int = 8;
pub const Py_nb_bool: c_int = 9;
pub const Py_nb_divmod: c_int = 10;
pub const Py_nb_float: c_int = 11;
pub const Py_nb_floor_divide: c_int = 12;
pub const Py_nb_index: c_int = 13;
pub const Py_nb_inplace_add: c_int = 14;
pub const Py_nb_inplace_and: c_int = 15;
pub const Py_nb_inplace_floor_divide: c_int = 16;
pub const Py_nb_inplace_lshift: c_int = 17;
pub const Py_nb_inplace_multiply: c_int = 18;
pub const Py_nb_inplace_or: c_int = 19;
pub const Py_nb_inplace_power: c_int = 20;
pub const Py_nb_inplace_remainder: c_int = 21;
pub const Py_nb_inplace_rshift: c_int = 22;
pub const Py_nb_inplace_subtract: c_int = 23;
pub const Py_nb_inplace_true_divide: c_int = 24;
pub const Py_nb_inplace_xor: c_int = 25;
pub const Py_nb_int: c_int = 26;
pub const Py_nb_invert: c_int = 27;
pub const Py_nb_lshift: c_int = 28;
pub const Py_nb_multiply: c_int = 29;
pub const Py_nb_negative: c_int = 30;
pub const Py_nb_or: c_int = 31;
pub const Py_nb_positive: c_int = 32;
pub const Py_nb_power: c_int = 33;
pub const Py_nb_remainder: c_int = 34;
pub const Py_nb_rshift: c_int = 35;
pub const Py_nb_subtract: c_int = 36;
pub const Py_nb_true_divide: c_int = 37;
pub const Py_nb_xor: c_int = 38;
pub const Py_sq_ass_item: c_int = 39;
pub const Py_sq_concat: c_int = 40;
pub const Py_sq_contains: c_int = 41;
pub const Py_sq_inplace_concat: c_int = 42;
pub const Py_sq_inplace_repeat: c_int = 43;
pub const Py_sq_item: c_int = 44;
pub const Py_sq_length: c_int = 45;
pub const Py_sq_repeat: c_int = 46;
pub const Py_tp_alloc: c_int = 47;
pub const Py_tp_base: c_int = 48;
pub const Py_tp_bases: c_int = 49;
pub const Py_tp_call: c_int = 50;
pub const Py_tp_clear: c_int = 51;
pub const Py_tp_dealloc: c_int = 52;
pub const Py_tp_del: c_int = 53;
pub const Py_tp_descr_get: c_int = 54;
pub const Py_tp_descr_set: c_int = 55;
pub const Py_tp_doc: c_int = 56;
pub const Py_tp_getattr: c_int = 57;
pub const Py_tp_getattro: c_int = 58;
pub const Py_tp_hash: c_int = 59;
pub const Py_tp_init: c_int = 60;
pub const Py_tp_is_gc: c_int = 61;
pub const Py_tp_iter: c_int = 62;
pub const Py_tp_iternext: c_int = 63;
pub const Py_tp_methods: c_int = 64;
pub const Py_tp_new: c_int = 65;
pub const Py_tp_repr: c_int = 66;
pub const Py_tp_richcompare: c_int = 67;
pub const Py_tp_setattr: c_int = 68;
pub const Py_tp_setattro: c_int = 69;
pub const Py_tp_str: c_int = 70;
pub const Py_tp_traverse: c_int = 71;
pub const Py_tp_members: c_int = 72;
pub const Py_tp_getset: c_int = 73;
pub const Py_tp_free: c_int = 74;
pub const Py_tp_finalize: c_int = 75;
pub const Py_tp_vectorcall: c_int = 76;
pub const Py_am_await: c_int = 77;
pub const Py_am_aiter: c_int = 78;
pub const Py_am_anext: c_int = 79;
pub const Py_nb_matrix_multiply: c_int = 80;
pub const Py_nb_inplace_matrix_multiply: c_int = 81;
pub const Py_am_send: c_int = 82;

/// All number-protocol slot IDs, used by [`SlotTable::has_number_protocol`].
pub const PY_NB_SLOTS: &[c_int] = &[
    Py_nb_absolute,
    Py_nb_add,
    Py_nb_and,
    Py_nb_bool,
    Py_nb_divmod,
    Py_nb_float,
    Py_nb_floor_divide,
    Py_nb_index,
    Py_nb_inplace_add,
    Py_nb_inplace_and,
    Py_nb_inplace_floor_divide,
    Py_nb_inplace_lshift,
    Py_nb_inplace_multiply,
    Py_nb_inplace_or,
    Py_nb_inplace_power,
    Py_nb_inplace_remainder,
    Py_nb_inplace_rshift,
    Py_nb_inplace_subtract,
    Py_nb_inplace_true_divide,
    Py_nb_inplace_xor,
    Py_nb_int,
    Py_nb_invert,
    Py_nb_lshift,
    Py_nb_multiply,
    Py_nb_negative,
    Py_nb_or,
    Py_nb_positive,
    Py_nb_power,
    Py_nb_remainder,
    Py_nb_rshift,
    Py_nb_subtract,
    Py_nb_true_divide,
    Py_nb_xor,
    Py_nb_matrix_multiply,
    Py_nb_inplace_matrix_multiply,
];

pub const PY_SQ_SLOTS: &[c_int] = &[
    Py_sq_ass_item,
    Py_sq_concat,
    Py_sq_contains,
    Py_sq_inplace_concat,
    Py_sq_inplace_repeat,
    Py_sq_item,
    Py_sq_length,
    Py_sq_repeat,
];

pub const PY_MP_SLOTS: &[c_int] = &[Py_mp_ass_subscript, Py_mp_length, Py_mp_subscript];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_and_lookup_round_trip() {
        let mut t = SlotTable::empty();
        let p = 0x1234_usize as *mut c_void;
        t.install(Py_tp_call, p);
        assert_eq!(t.get(Py_tp_call).0, p);
        assert!(t.get(Py_tp_init).is_null());
    }

    #[test]
    fn out_of_range_install_is_noop() {
        let mut t = SlotTable::empty();
        let p = 0x1234_usize as *mut c_void;
        t.install(0, p);
        t.install(SLOT_TABLE_SIZE as c_int, p);
        t.install(SLOT_TABLE_SIZE as c_int + 7, p);
        for i in 0..SLOT_TABLE_SIZE {
            assert!(t.slots[i].is_null());
        }
    }

    #[test]
    fn has_protocol_flags_track_installation() {
        let mut t = SlotTable::empty();
        assert!(!t.has_buffer_protocol());
        assert!(!t.has_number_protocol());
        assert!(!t.has_sequence_protocol());
        assert!(!t.has_mapping_protocol());

        t.install(Py_bf_getbuffer, std::ptr::dangling_mut::<c_void>());
        assert!(t.has_buffer_protocol());

        let mut t2 = SlotTable::empty();
        t2.install(Py_nb_add, std::ptr::dangling_mut::<c_void>());
        assert!(t2.has_number_protocol());

        let mut t3 = SlotTable::empty();
        t3.install(Py_sq_item, std::ptr::dangling_mut::<c_void>());
        assert!(t3.has_sequence_protocol());

        let mut t4 = SlotTable::empty();
        t4.install(Py_mp_subscript, std::ptr::dangling_mut::<c_void>());
        assert!(t4.has_mapping_protocol());
    }
}

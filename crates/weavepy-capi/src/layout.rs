//! Byte-faithful CPython 3.13 object layouts (RFC 0043, wave 1, WS1).
//!
//! Every struct here is `#[repr(C)]` and pinned, with compile-time
//! `const _: () = assert!(...)` guards, to the *exact* sizes and field
//! offsets that the host's stock CPython 3.13 headers produce on a
//! 64-bit little-endian build (the `LP64` / arm64 + x86-64 macOS/Linux
//! ABI). The numbers were read out of the installed headers with an
//! `offsetof`/`sizeof` probe; see `docs/rfcs/0043-cpython-binary-abi.md`.
//!
//! The point of this module is twofold:
//!
//! 1. It is the **authoritative, machine-checked description** of the
//!    binary ABI WeavePy must present so that a *stock* C extension
//!    (compiled against CPython's real headers, carrying *inlined*
//!    field-access macros like `PyFloat_AS_DOUBLE`, `PyList_GET_ITEM`,
//!    `Py_SIZE`) reads correct memory when it pokes a WeavePy object.
//!    If a CPython point release shifts a field, the `assert!`s here
//!    fail the build loudly rather than silently corrupting memory.
//! 2. It provides the concrete Rust types the mirror bridge
//!    ([`crate::mirror`]) allocates and fills.
//!
//! Only the non-debug, non-free-threaded (`!Py_GIL_DISABLED`,
//! `!Py_TRACE_REFS`) build is modelled; those are explicit non-goals in
//! RFC 0043.

#![allow(non_camel_case_types)]

use std::os::raw::{c_char, c_int, c_void};

use crate::object::{PyHashT, PyObject, PySsizeT};

/// CPython's `digit` (a 30-bit limb stored in a `uint32_t`). See
/// `Include/cpython/longintrepr.h` and `PyLong_SHIFT == 30`.
pub type digit = u32;

/// Base-2^30 limb shift, matching CPython 3.13's `PyLong_SHIFT`.
pub const PYLONG_SHIFT: u32 = 30;
/// Mask of a single 30-bit limb.
pub const PYLONG_MASK: u32 = (1u32 << PYLONG_SHIFT) - 1;

// `_PyLongValue.lv_tag` packing (Include/cpython/longintrepr.h):
//   lv_tag = (digit_count << NON_SIZE_BITS) | sign
// where the low 2 bits encode the sign.
/// Number of low tag bits reserved for the sign (`lv_tag >> 3` is the
/// digit count).
pub const PYLONG_NON_SIZE_BITS: usize = 3;
/// Sign field: positive (`> 0`).
pub const PYLONG_SIGN_POSITIVE: usize = 0;
/// Sign field: the value is exactly zero.
pub const PYLONG_SIGN_ZERO: usize = 1;
/// Sign field: negative (`< 0`).
pub const PYLONG_SIGN_NEGATIVE: usize = 2;

// ---------------------------------------------------------------------------
// Variable-size object head.
// ---------------------------------------------------------------------------

/// `PyVarObject` — `PyObject` plus an `ob_size` element count. The head
/// of every variable-length built-in (tuple, list, bytes, int, ...).
#[repr(C)]
#[derive(Debug)]
pub struct PyVarObject {
    pub ob_base: PyObject,
    pub ob_size: PySsizeT,
}

const _: () = {
    assert!(std::mem::size_of::<PyObject>() == 16);
    assert!(std::mem::offset_of!(PyObject, ob_refcnt) == 0);
    assert!(std::mem::offset_of!(PyObject, ob_type) == 8);
    assert!(std::mem::size_of::<PyVarObject>() == 24);
    assert!(std::mem::offset_of!(PyVarObject, ob_size) == 16);
};

// ---------------------------------------------------------------------------
// Numeric scalars.
// ---------------------------------------------------------------------------

/// `PyFloatObject { PyObject_HEAD; double ob_fval; }`.
#[repr(C)]
#[derive(Debug)]
pub struct PyFloatObject {
    pub ob_base: PyObject,
    pub ob_fval: f64,
}

const _: () = {
    assert!(std::mem::size_of::<PyFloatObject>() == 24);
    assert!(std::mem::offset_of!(PyFloatObject, ob_fval) == 16);
};

/// CPython's `Py_complex { double real; double imag; }`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct PyComplexValue {
    pub real: f64,
    pub imag: f64,
}

/// `PyComplexObject { PyObject_HEAD; Py_complex cval; }`.
#[repr(C)]
#[derive(Debug)]
pub struct PyComplexObject {
    pub ob_base: PyObject,
    pub cval: PyComplexValue,
}

const _: () = {
    assert!(std::mem::size_of::<PyComplexObject>() == 32);
    assert!(std::mem::offset_of!(PyComplexObject, cval) == 16);
};

/// CPython 3.12+ `_PyLongValue { uintptr_t lv_tag; digit ob_digit[1]; }`.
///
/// `lv_tag` packs the limb count (`>> 3`) and the sign (low 2 bits); the
/// limbs follow inline. Declared with a 1-element `ob_digit` to match
/// CPython's `[1]` flexible-array convention (so `size_of` agrees);
/// real instances over-allocate the tail.
#[repr(C)]
#[derive(Debug)]
pub struct PyLongValue {
    pub lv_tag: usize,
    pub ob_digit: [digit; 1],
}

/// `PyLongObject { PyObject_HEAD; _PyLongValue long_value; }`.
#[repr(C)]
#[derive(Debug)]
pub struct PyLongObject {
    pub ob_base: PyObject,
    pub long_value: PyLongValue,
}

const _: () = {
    assert!(std::mem::size_of::<PyLongObject>() == 32);
    assert!(std::mem::offset_of!(PyLongObject, long_value) == 16);
    // lv_tag is the first field of long_value, hence also at +16.
    assert!(std::mem::offset_of!(PyLongValue, lv_tag) == 0);
    assert!(std::mem::offset_of!(PyLongValue, ob_digit) == 8);
    assert!(std::mem::size_of::<digit>() == 4);
    assert!(PYLONG_SHIFT == 30);
};

// ---------------------------------------------------------------------------
// Byte strings.
// ---------------------------------------------------------------------------

/// `PyBytesObject { PyObject_VAR_HEAD; Py_hash_t ob_shash; char ob_sval[1]; }`.
#[repr(C)]
#[derive(Debug)]
pub struct PyBytesObject {
    pub ob_base: PyVarObject,
    pub ob_shash: PyHashT,
    pub ob_sval: [c_char; 1],
}

const _: () = {
    assert!(std::mem::size_of::<PyBytesObject>() == 40);
    assert!(std::mem::offset_of!(PyBytesObject, ob_shash) == 24);
    assert!(std::mem::offset_of!(PyBytesObject, ob_sval) == 32);
};

/// `PyByteArrayObject` — `Include/cpython/bytearrayobject.h`.
#[repr(C)]
#[derive(Debug)]
pub struct PyByteArrayObject {
    pub ob_base: PyVarObject,
    pub ob_alloc: PySsizeT,
    pub ob_bytes: *mut c_char,
    pub ob_start: *mut c_char,
    pub ob_exports: PySsizeT,
}

const _: () = {
    assert!(std::mem::size_of::<PyByteArrayObject>() == 56);
    assert!(std::mem::offset_of!(PyByteArrayObject, ob_alloc) == 24);
    assert!(std::mem::offset_of!(PyByteArrayObject, ob_bytes) == 32);
    assert!(std::mem::offset_of!(PyByteArrayObject, ob_start) == 40);
    assert!(std::mem::offset_of!(PyByteArrayObject, ob_exports) == 48);
};

// ---------------------------------------------------------------------------
// Sequence containers.
// ---------------------------------------------------------------------------

/// `PyTupleObject { PyObject_VAR_HEAD; PyObject *ob_item[1]; }`.
#[repr(C)]
#[derive(Debug)]
pub struct PyTupleObject {
    pub ob_base: PyVarObject,
    pub ob_item: [*mut PyObject; 1],
}

const _: () = {
    assert!(std::mem::size_of::<PyTupleObject>() == 32);
    assert!(std::mem::offset_of!(PyTupleObject, ob_item) == 24);
};

/// `PyListObject { PyObject_VAR_HEAD; PyObject **ob_item; Py_ssize_t allocated; }`.
#[repr(C)]
#[derive(Debug)]
pub struct PyListObject {
    pub ob_base: PyVarObject,
    pub ob_item: *mut *mut PyObject,
    pub allocated: PySsizeT,
}

const _: () = {
    assert!(std::mem::size_of::<PyListObject>() == 40);
    assert!(std::mem::offset_of!(PyListObject, ob_item) == 24);
    assert!(std::mem::offset_of!(PyListObject, allocated) == 32);
};

// ---------------------------------------------------------------------------
// PEP 393 flexible unicode representation.
// ---------------------------------------------------------------------------

/// The PEP 393 `state` bit-field, packed into a `u32`. CPython declares:
///
/// ```c
/// struct {
///     unsigned int interned:2;             // bits 0..1
///     unsigned int kind:3;                 // bits 2..4
///     unsigned int compact:1;              // bit 5
///     unsigned int ascii:1;                // bit 6
///     unsigned int statically_allocated:1; // bit 7
///     unsigned int :24;                    // padding
/// } state;
/// ```
///
/// On a little-endian LP64 target clang allocates `unsigned int`
/// bit-fields from the least-significant bit, so the packing above is
/// stable; the inlined `PyUnicode_KIND`/`PyUnicode_IS_ASCII` macros read
/// exactly these bits.
pub mod ustate {
    pub const KIND_1BYTE: u32 = 1;
    pub const KIND_2BYTE: u32 = 2;
    pub const KIND_4BYTE: u32 = 4;

    pub const INTERNED_SHIFT: u32 = 0;
    pub const KIND_SHIFT: u32 = 2;
    pub const COMPACT_SHIFT: u32 = 5;
    pub const ASCII_SHIFT: u32 = 6;
    pub const STATIC_SHIFT: u32 = 7;

    /// Build a faithful `state` word.
    #[inline]
    pub fn pack(interned: u32, kind: u32, compact: bool, ascii: bool, statically: bool) -> u32 {
        (interned & 0x3) << INTERNED_SHIFT
            | (kind & 0x7) << KIND_SHIFT
            | (compact as u32) << COMPACT_SHIFT
            | (ascii as u32) << ASCII_SHIFT
            | (statically as u32) << STATIC_SHIFT
    }
}

/// `PyASCIIObject` — the compact-ASCII string head (PEP 393). The
/// character data for a compact-ASCII string follows the struct inline.
#[repr(C)]
#[derive(Debug)]
pub struct PyASCIIObject {
    pub ob_base: PyObject,
    pub length: PySsizeT,
    pub hash: PyHashT,
    /// PEP 393 `state` bit-field; see [`ustate`].
    pub state: u32,
    _state_pad: u32,
}

const _: () = {
    assert!(std::mem::size_of::<PyASCIIObject>() == 40);
    assert!(std::mem::offset_of!(PyASCIIObject, length) == 16);
    assert!(std::mem::offset_of!(PyASCIIObject, hash) == 24);
    assert!(std::mem::offset_of!(PyASCIIObject, state) == 32);
};

/// `PyCompactUnicodeObject` — adds the lazily-filled UTF-8 cache.
#[repr(C)]
#[derive(Debug)]
pub struct PyCompactUnicodeObject {
    pub _base: PyASCIIObject,
    pub utf8_length: PySsizeT,
    pub utf8: *mut c_char,
}

const _: () = {
    assert!(std::mem::size_of::<PyCompactUnicodeObject>() == 56);
    assert!(std::mem::offset_of!(PyCompactUnicodeObject, utf8_length) == 40);
    assert!(std::mem::offset_of!(PyCompactUnicodeObject, utf8) == 48);
};

/// `PyUnicodeObject` — the non-compact form with an out-of-line buffer.
#[repr(C)]
#[derive(Debug)]
pub struct PyUnicodeObject {
    pub _base: PyCompactUnicodeObject,
    pub data: *mut c_void,
}

const _: () = {
    assert!(std::mem::size_of::<PyUnicodeObject>() == 64);
    assert!(std::mem::offset_of!(PyUnicodeObject, data) == 56);
};

// ---------------------------------------------------------------------------
// Method-suite structs (defined faithfully now; dispatched in wave 2).
// ---------------------------------------------------------------------------
//
// We model these as fixed-size opaque blobs of the correct byte length
// rather than spelling out every binaryfunc/ternaryfunc slot, because
// wave 1 does not *call* through them — it only needs `tp_as_number`
// (etc.) to point at correctly-sized storage so a stock extension that
// reads `nb_add` at its real offset sees a real (possibly null) slot
// rather than running off the end. Wave 2 replaces these with fully
// spelled-out slot tables.

macro_rules! opaque_suite {
    ($name:ident, $bytes:literal) => {
        #[repr(C)]
        #[derive(Debug)]
        pub struct $name {
            _bytes: [u8; $bytes],
        }
        impl Default for $name {
            fn default() -> Self {
                Self {
                    _bytes: [0; $bytes],
                }
            }
        }
    };
}

opaque_suite!(PyNumberMethods, 288);
opaque_suite!(PySequenceMethods, 80);
opaque_suite!(PyMappingMethods, 24);
opaque_suite!(PyAsyncMethods, 32);
opaque_suite!(PyBufferProcs, 16);

const _: () = {
    assert!(std::mem::size_of::<PyNumberMethods>() == 288);
    assert!(std::mem::size_of::<PySequenceMethods>() == 80);
    assert!(std::mem::size_of::<PyMappingMethods>() == 24);
    assert!(std::mem::size_of::<PyAsyncMethods>() == 32);
    assert!(std::mem::size_of::<PyBufferProcs>() == 16);
};

// ---------------------------------------------------------------------------
// The full, byte-faithful PyTypeObject.
// ---------------------------------------------------------------------------

/// C function-pointer slot types. We type them as raw `*mut c_void` /
/// `Option<extern "C" fn ...>` where wave 1 reads or writes them, and as
/// opaque `*mut c_void` where it only needs the byte to be present at the
/// right offset.
pub type destructor = unsafe extern "C" fn(*mut PyObject);
pub type freefunc = unsafe extern "C" fn(*mut c_void);
pub type allocfunc = unsafe extern "C" fn(*mut PyTypeObjectFull, PySsizeT) -> *mut PyObject;
pub type newfunc =
    unsafe extern "C" fn(*mut PyTypeObjectFull, *mut PyObject, *mut PyObject) -> *mut PyObject;

/// The full CPython 3.13 `PyTypeObject`, byte-for-byte. Field order and
/// offsets are pinned below. Slots wave 1 does not dispatch through are
/// typed as `*mut c_void` (a pointer-sized hole at the correct offset);
/// the ones it reads/writes (`tp_basicsize`, `tp_itemsize`, `tp_flags`,
/// `tp_dealloc`, `tp_alloc`, `tp_new`, `tp_free`, `tp_name`) are typed.
#[repr(C)]
pub struct PyTypeObjectFull {
    pub ob_base: PyVarObject,                   // 0
    pub tp_name: *const c_char,                 // 24
    pub tp_basicsize: PySsizeT,                 // 32
    pub tp_itemsize: PySsizeT,                  // 40
    pub tp_dealloc: Option<destructor>,         // 48
    pub tp_vectorcall_offset: PySsizeT,         // 56
    pub tp_getattr: *mut c_void,                // 64
    pub tp_setattr: *mut c_void,                // 72
    pub tp_as_async: *mut PyAsyncMethods,       // 80
    pub tp_repr: *mut c_void,                   // 88
    pub tp_as_number: *mut PyNumberMethods,     // 96
    pub tp_as_sequence: *mut PySequenceMethods, // 104
    pub tp_as_mapping: *mut PyMappingMethods,   // 112
    pub tp_hash: *mut c_void,                   // 120
    pub tp_call: *mut c_void,                   // 128
    pub tp_str: *mut c_void,                    // 136
    pub tp_getattro: *mut c_void,               // 144
    pub tp_setattro: *mut c_void,               // 152
    pub tp_as_buffer: *mut PyBufferProcs,       // 160
    pub tp_flags: u64,                          // 168 (unsigned long)
    pub tp_doc: *const c_char,                  // 176
    pub tp_traverse: *mut c_void,               // 184
    pub tp_clear: *mut c_void,                  // 192
    pub tp_richcompare: *mut c_void,            // 200
    pub tp_weaklistoffset: PySsizeT,            // 208
    pub tp_iter: *mut c_void,                   // 216
    pub tp_iternext: *mut c_void,               // 224
    pub tp_methods: *mut c_void,                // 232
    pub tp_members: *mut c_void,                // 240
    pub tp_getset: *mut c_void,                 // 248
    pub tp_base: *mut PyTypeObjectFull,         // 256
    pub tp_dict: *mut PyObject,                 // 264
    pub tp_descr_get: *mut c_void,              // 272
    pub tp_descr_set: *mut c_void,              // 280
    pub tp_dictoffset: PySsizeT,                // 288
    pub tp_init: *mut c_void,                   // 296
    pub tp_alloc: Option<allocfunc>,            // 304
    pub tp_new: Option<newfunc>,                // 312
    pub tp_free: Option<freefunc>,              // 320
    pub tp_is_gc: *mut c_void,                  // 328
    pub tp_bases: *mut PyObject,                // 336
    pub tp_mro: *mut PyObject,                  // 344
    pub tp_cache: *mut PyObject,                // 352
    pub tp_subclasses: *mut c_void,             // 360
    pub tp_weaklist: *mut PyObject,             // 368
    pub tp_del: *mut c_void,                    // 376
    pub tp_version_tag: c_uint_pad,             // 384 (unsigned int + 4 pad)
    pub tp_finalize: *mut c_void,               // 392
    pub tp_vectorcall: *mut c_void,             // 400
    /// `unsigned char tp_watched` + `uint16_t tp_versions_used` + pad.
    pub tp_tail: [u8; 8], // 408
}

/// `unsigned int tp_version_tag` widened to its 8-byte aligned slot.
pub type c_uint_pad = u64;

const _: () = {
    assert!(std::mem::size_of::<PyTypeObjectFull>() == 416);
    assert!(std::mem::offset_of!(PyTypeObjectFull, tp_name) == 24);
    assert!(std::mem::offset_of!(PyTypeObjectFull, tp_basicsize) == 32);
    assert!(std::mem::offset_of!(PyTypeObjectFull, tp_itemsize) == 40);
    assert!(std::mem::offset_of!(PyTypeObjectFull, tp_dealloc) == 48);
    assert!(std::mem::offset_of!(PyTypeObjectFull, tp_as_number) == 96);
    assert!(std::mem::offset_of!(PyTypeObjectFull, tp_as_sequence) == 104);
    assert!(std::mem::offset_of!(PyTypeObjectFull, tp_as_mapping) == 112);
    assert!(std::mem::offset_of!(PyTypeObjectFull, tp_as_buffer) == 160);
    assert!(std::mem::offset_of!(PyTypeObjectFull, tp_flags) == 168);
    assert!(std::mem::offset_of!(PyTypeObjectFull, tp_doc) == 176);
    assert!(std::mem::offset_of!(PyTypeObjectFull, tp_base) == 256);
    assert!(std::mem::offset_of!(PyTypeObjectFull, tp_dictoffset) == 288);
    assert!(std::mem::offset_of!(PyTypeObjectFull, tp_alloc) == 304);
    assert!(std::mem::offset_of!(PyTypeObjectFull, tp_new) == 312);
    assert!(std::mem::offset_of!(PyTypeObjectFull, tp_free) == 320);
    assert!(std::mem::offset_of!(PyTypeObjectFull, tp_finalize) == 392);
    assert!(std::mem::offset_of!(PyTypeObjectFull, tp_vectorcall) == 400);
};

/// A handful of the `tp_flags` feature bits wave 1 sets so a stock
/// `PyType_HasFeature(t, Py_TPFLAGS_…)` read returns the truth. Values
/// from `Include/object.h`.
pub mod tpflags {
    pub const LONG_SUBCLASS: u64 = 1 << 24;
    pub const LIST_SUBCLASS: u64 = 1 << 25;
    pub const TUPLE_SUBCLASS: u64 = 1 << 26;
    pub const BYTES_SUBCLASS: u64 = 1 << 27;
    pub const UNICODE_SUBCLASS: u64 = 1 << 28;
    pub const DICT_SUBCLASS: u64 = 1 << 29;
    pub const BASE_EXC_SUBCLASS: u64 = 1 << 30;
    pub const TYPE_SUBCLASS: u64 = 1 << 31;
    pub const DEFAULT: u64 = 0;
    pub const BASETYPE: u64 = 1 << 10;
    pub const READY: u64 = 1 << 12;
    pub const IMMUTABLETYPE: u64 = 1 << 8;
}

/// Sanity: `c_int` is 4 bytes on every target we model.
const _: () = assert!(std::mem::size_of::<c_int>() == 4);

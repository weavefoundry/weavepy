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
// Built-in function objects.
// ---------------------------------------------------------------------------
//
// `builtin_function_or_method` (`PyCFunctionObject`) and the
// `PyMethodDef` it points at. RFC 0046 (wave 4): numpy's `add_docstring`
// reaches *through* a function object — `((PyCFunctionObject *)f)->m_ml->ml_doc`
// — to install (and dedupe) docstrings, a direct struct walk the host
// cannot interpose. A WeavePy `Object::Builtin` therefore crosses into C
// as a faithful `PyCFunctionObject` whose `m_ml` points at a real,
// writable `PyMethodDef` (carried inline, just past the object) so the
// read of `ml_doc` and the subsequent `ml_doc = docstr` write both land
// on valid memory.

/// `PyMethodDef { const char *ml_name; PyCFunction ml_meth; int ml_flags;
/// const char *ml_doc; }` — `Include/methodobject.h`.
#[repr(C)]
#[derive(Debug)]
pub struct PyMethodDef {
    pub ml_name: *const c_char, // 0
    pub ml_meth: *mut c_void,   // 8  (PyCFunction)
    pub ml_flags: c_int,        // 16 (+4 pad)
    _flags_pad: u32,
    pub ml_doc: *const c_char, // 24
}

const _: () = {
    assert!(std::mem::size_of::<PyMethodDef>() == 32);
    assert!(std::mem::offset_of!(PyMethodDef, ml_name) == 0);
    assert!(std::mem::offset_of!(PyMethodDef, ml_meth) == 8);
    assert!(std::mem::offset_of!(PyMethodDef, ml_flags) == 16);
    assert!(std::mem::offset_of!(PyMethodDef, ml_doc) == 24);
};

/// `PyCFunctionObject` — `Include/cpython/methodobject.h`.
#[repr(C)]
#[derive(Debug)]
pub struct PyCFunctionObject {
    pub ob_base: PyObject,            // 0
    pub m_ml: *mut PyMethodDef,       // 16
    pub m_self: *mut PyObject,        // 24
    pub m_module: *mut PyObject,      // 32
    pub m_weakreflist: *mut PyObject, // 40
    pub vectorcall: *mut c_void,      // 48 (vectorcallfunc)
}

const _: () = {
    assert!(std::mem::size_of::<PyCFunctionObject>() == 56);
    assert!(std::mem::offset_of!(PyCFunctionObject, m_ml) == 16);
    assert!(std::mem::offset_of!(PyCFunctionObject, m_self) == 24);
    assert!(std::mem::offset_of!(PyCFunctionObject, m_module) == 32);
    assert!(std::mem::offset_of!(PyCFunctionObject, m_weakreflist) == 40);
    assert!(std::mem::offset_of!(PyCFunctionObject, vectorcall) == 48);
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
// Method-suite structs (RFC 0044, wave 2, WS1).
// ---------------------------------------------------------------------------
//
// Spelled out field-by-field, byte-faithful to CPython 3.13. Wave 2
// *dispatches* through these: a stock extension defines a type with
// `tp_as_number = &my_number`, and `PyType_Ready` reads `nb_add` at
// offset 0, `nb_multiply` at 16, … to populate the `SlotTable`.
//
// Every slot is typed `*mut c_void` (the ABI is pointer-width and the
// harvest stores the raw pointer in the `SlotTable`, casting to the
// concrete `unsafe extern "C" fn` only at the call site). This matches
// how `crate::types::PyTypeObject` types its `tp_*` slots. The
// canonical C signature for each slot is given in the doc comment. The
// reserved holes CPython keeps (`nb_reserved`, `was_sq_slice`,
// `was_sq_ass_slice`) are named so the offset asserts cover the whole
// struct.

/// `PyNumberMethods` — the numeric protocol suite (`tp_as_number`).
#[repr(C)]
#[derive(Debug)]
pub struct PyNumberMethods {
    pub nb_add: *mut c_void,       // 0   binaryfunc
    pub nb_subtract: *mut c_void,  // 8   binaryfunc
    pub nb_multiply: *mut c_void,  // 16  binaryfunc
    pub nb_remainder: *mut c_void, // 24  binaryfunc
    pub nb_divmod: *mut c_void,    // 32  binaryfunc
    pub nb_power: *mut c_void,     // 40  ternaryfunc
    pub nb_negative: *mut c_void,  // 48  unaryfunc
    pub nb_positive: *mut c_void,  // 56  unaryfunc
    pub nb_absolute: *mut c_void,  // 64  unaryfunc
    pub nb_bool: *mut c_void,      // 72  inquiry
    pub nb_invert: *mut c_void,    // 80  unaryfunc
    pub nb_lshift: *mut c_void,    // 88  binaryfunc
    pub nb_rshift: *mut c_void,    // 96  binaryfunc
    pub nb_and: *mut c_void,       // 104 binaryfunc
    pub nb_xor: *mut c_void,       // 112 binaryfunc
    pub nb_or: *mut c_void,        // 120 binaryfunc
    pub nb_int: *mut c_void,       // 128 unaryfunc
    /// Reserved (was `nb_long`); always null.
    pub nb_reserved: *mut c_void, // 136
    pub nb_float: *mut c_void,     // 144 unaryfunc
    pub nb_inplace_add: *mut c_void, // 152 binaryfunc
    pub nb_inplace_subtract: *mut c_void, // 160 binaryfunc
    pub nb_inplace_multiply: *mut c_void, // 168 binaryfunc
    pub nb_inplace_remainder: *mut c_void, // 176 binaryfunc
    pub nb_inplace_power: *mut c_void, // 184 ternaryfunc
    pub nb_inplace_lshift: *mut c_void, // 192 binaryfunc
    pub nb_inplace_rshift: *mut c_void, // 200 binaryfunc
    pub nb_inplace_and: *mut c_void, // 208 binaryfunc
    pub nb_inplace_xor: *mut c_void, // 216 binaryfunc
    pub nb_inplace_or: *mut c_void, // 224 binaryfunc
    pub nb_floor_divide: *mut c_void, // 232 binaryfunc
    pub nb_true_divide: *mut c_void, // 240 binaryfunc
    pub nb_inplace_floor_divide: *mut c_void, // 248 binaryfunc
    pub nb_inplace_true_divide: *mut c_void, // 256 binaryfunc
    pub nb_index: *mut c_void,     // 264 unaryfunc
    pub nb_matrix_multiply: *mut c_void, // 272 binaryfunc
    pub nb_inplace_matrix_multiply: *mut c_void, // 280 binaryfunc
}

/// `PySequenceMethods` — the sequence protocol suite (`tp_as_sequence`).
#[repr(C)]
#[derive(Debug)]
pub struct PySequenceMethods {
    pub sq_length: *mut c_void, // 0  lenfunc
    pub sq_concat: *mut c_void, // 8  binaryfunc
    pub sq_repeat: *mut c_void, // 16 ssizeargfunc
    pub sq_item: *mut c_void,   // 24 ssizeargfunc
    /// Reserved (was `sq_slice`); always null.
    pub was_sq_slice: *mut c_void, // 32
    pub sq_ass_item: *mut c_void, // 40 ssizeobjargproc
    /// Reserved (was `sq_ass_slice`); always null.
    pub was_sq_ass_slice: *mut c_void, // 48
    pub sq_contains: *mut c_void, // 56 objobjproc
    pub sq_inplace_concat: *mut c_void, // 64 binaryfunc
    pub sq_inplace_repeat: *mut c_void, // 72 ssizeargfunc
}

/// `PyMappingMethods` — the mapping protocol suite (`tp_as_mapping`).
#[repr(C)]
#[derive(Debug)]
pub struct PyMappingMethods {
    pub mp_length: *mut c_void,        // 0  lenfunc
    pub mp_subscript: *mut c_void,     // 8  binaryfunc
    pub mp_ass_subscript: *mut c_void, // 16 objobjargproc
}

/// `PyAsyncMethods` — the async protocol suite (`tp_as_async`).
#[repr(C)]
#[derive(Debug)]
pub struct PyAsyncMethods {
    pub am_await: *mut c_void, // 0  unaryfunc
    pub am_aiter: *mut c_void, // 8  unaryfunc
    pub am_anext: *mut c_void, // 16 unaryfunc
    pub am_send: *mut c_void,  // 24 sendfunc
}

/// `PyBufferProcs` — the PEP 3118 buffer suite (`tp_as_buffer`).
#[repr(C)]
#[derive(Debug)]
pub struct PyBufferProcs {
    pub bf_getbuffer: *mut c_void,     // 0 getbufferproc
    pub bf_releasebuffer: *mut c_void, // 8 releasebufferproc
}

const _: () = {
    assert!(std::mem::size_of::<PyNumberMethods>() == 288);
    assert!(std::mem::offset_of!(PyNumberMethods, nb_add) == 0);
    assert!(std::mem::offset_of!(PyNumberMethods, nb_multiply) == 16);
    assert!(std::mem::offset_of!(PyNumberMethods, nb_power) == 40);
    assert!(std::mem::offset_of!(PyNumberMethods, nb_bool) == 72);
    assert!(std::mem::offset_of!(PyNumberMethods, nb_int) == 128);
    assert!(std::mem::offset_of!(PyNumberMethods, nb_reserved) == 136);
    assert!(std::mem::offset_of!(PyNumberMethods, nb_float) == 144);
    assert!(std::mem::offset_of!(PyNumberMethods, nb_inplace_add) == 152);
    assert!(std::mem::offset_of!(PyNumberMethods, nb_floor_divide) == 232);
    assert!(std::mem::offset_of!(PyNumberMethods, nb_true_divide) == 240);
    assert!(std::mem::offset_of!(PyNumberMethods, nb_index) == 264);
    assert!(std::mem::offset_of!(PyNumberMethods, nb_matrix_multiply) == 272);
    assert!(std::mem::offset_of!(PyNumberMethods, nb_inplace_matrix_multiply) == 280);

    assert!(std::mem::size_of::<PySequenceMethods>() == 80);
    assert!(std::mem::offset_of!(PySequenceMethods, sq_length) == 0);
    assert!(std::mem::offset_of!(PySequenceMethods, sq_item) == 24);
    assert!(std::mem::offset_of!(PySequenceMethods, sq_ass_item) == 40);
    assert!(std::mem::offset_of!(PySequenceMethods, sq_contains) == 56);
    assert!(std::mem::offset_of!(PySequenceMethods, sq_inplace_repeat) == 72);

    assert!(std::mem::size_of::<PyMappingMethods>() == 24);
    assert!(std::mem::offset_of!(PyMappingMethods, mp_length) == 0);
    assert!(std::mem::offset_of!(PyMappingMethods, mp_subscript) == 8);
    assert!(std::mem::offset_of!(PyMappingMethods, mp_ass_subscript) == 16);

    assert!(std::mem::size_of::<PyAsyncMethods>() == 32);
    assert!(std::mem::offset_of!(PyAsyncMethods, am_await) == 0);
    assert!(std::mem::offset_of!(PyAsyncMethods, am_aiter) == 8);
    assert!(std::mem::offset_of!(PyAsyncMethods, am_anext) == 16);
    assert!(std::mem::offset_of!(PyAsyncMethods, am_send) == 24);

    assert!(std::mem::size_of::<PyBufferProcs>() == 16);
    assert!(std::mem::offset_of!(PyBufferProcs, bf_getbuffer) == 0);
    assert!(std::mem::offset_of!(PyBufferProcs, bf_releasebuffer) == 8);
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

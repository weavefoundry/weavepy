//! RFC 0077 WS12 — the CPython 3.14 C-API delta.
//!
//! Everything `Doc/whatsnew/3.14.rst` adds to the public (and
//! limited) API that the 3.13-era waves did not already export: the
//! public `PyUnicodeWriter`, the fixed-width `PyLong_*Int32/64`
//! family and the sign predicates, the `PyLong_Export`/`PyLongWriter`
//! import-export layer (PEP 757), `Py_HashBuffer`, `PyBytes_Join`,
//! `PyImport_ImportModuleAttr`, `PyType_GetBaseByToken`
//! (`Py_tp_token`), `PyType_Freeze`, `PyUnicode_Equal`, the exported
//! `PyUnicode_KIND`/`PyUnicode_DATA` accessor functions,
//! `Py_fopen`/`Py_fclose`, `PyConfig_Set`, the `PyUnstable_*`
//! refcount hints, the `PyTime_*` clocks, `PySys_AuditTuple`, the
//! remaining watcher registrars, and a handful of 3.12/3.13 audit
//! items (`PyCode_GetCellvars`/`GetFreevars`, `PyWeakref_IsDead`,
//! `PyUnstable_AtExit`, tracing no-ops) the survey found missing.
//!
//! Each entry is rooted in [`crate::force_link_table`]; the variadic
//! `PyUnicodeWriter_Format` and `PySys_Audit` live in `varargs.c` and
//! call the `*V`/`*Tuple` forms here.

#![allow(clippy::missing_safety_doc)]

use std::ffi::CStr;
use std::os::raw::{c_char, c_int, c_void};
use std::ptr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use weavepy_vm::shared_value::SharedSlice;

use num_bigint::{BigInt, Sign};
use num_traits::{ToPrimitive, Zero};
use weavepy_vm::builtin_types::builtin_types;
use weavepy_vm::object::Object;
use weavepy_vm::sync::Rc;

use crate::object::{clone_object, clone_object_value, into_owned, PyObject, PySsizeT};
use crate::types::PyTypeObject;

fn set_system_error(msg: impl Into<String>) {
    crate::errors::set_pending(
        Some(builtin_types().system_error.clone()),
        Object::from_str(msg.into()),
    );
}

// ---------------------------------------------------------------------------
// PyUnicodeWriter — the public incremental string builder (3.14)
// ---------------------------------------------------------------------------

/// Opaque to C; a growable code-point buffer. Errors leave the buffer
/// untouched so a caller can recover (test_capi `test_recover_*`).
pub struct PyUnicodeWriter {
    cps: Vec<u32>,
}

fn writer_mut<'a>(w: *mut PyUnicodeWriter) -> Option<&'a mut PyUnicodeWriter> {
    if w.is_null() {
        set_system_error("PyUnicodeWriter: NULL writer");
        return None;
    }
    // SAFETY: created by `PyUnicodeWriter_Create` and not yet finished.
    Some(unsafe { &mut *w })
}

/// `PyUnicodeWriter_Create(length)` — `length` is a capacity hint.
#[no_mangle]
pub unsafe extern "C" fn PyUnicodeWriter_Create(length: PySsizeT) -> *mut PyUnicodeWriter {
    if length < 0 {
        crate::errors::set_value_error("length must be positive");
        return ptr::null_mut();
    }
    let cap = (length as usize).min(1 << 20);
    Box::into_raw(Box::new(PyUnicodeWriter {
        cps: Vec::with_capacity(cap),
    }))
}

/// `PyUnicodeWriter_Discard(writer)` — drop without producing a string.
#[no_mangle]
pub unsafe extern "C" fn PyUnicodeWriter_Discard(writer: *mut PyUnicodeWriter) {
    if !writer.is_null() {
        drop(unsafe { Box::from_raw(writer) });
    }
}

/// `PyUnicodeWriter_Finish(writer)` — the built `str`; the writer is
/// consumed either way.
#[no_mangle]
pub unsafe extern "C" fn PyUnicodeWriter_Finish(writer: *mut PyUnicodeWriter) -> *mut PyObject {
    if writer.is_null() {
        set_system_error("PyUnicodeWriter_Finish: NULL writer");
        return ptr::null_mut();
    }
    let w = unsafe { Box::from_raw(writer) };
    into_owned(Object::str_from_codepoints(w.cps))
}

/// `PyUnicodeWriter_WriteChar(writer, ch)`.
#[no_mangle]
pub unsafe extern "C" fn PyUnicodeWriter_WriteChar(writer: *mut PyUnicodeWriter, ch: u32) -> c_int {
    let Some(w) = writer_mut(writer) else {
        return -1;
    };
    if ch > 0x10_FFFF {
        crate::errors::set_value_error("character must be in range(0x110000)");
        return -1;
    }
    w.cps.push(ch);
    0
}

unsafe fn c_slice<'a>(s: *const c_char, size: PySsizeT) -> &'a [u8] {
    if s.is_null() {
        return &[];
    }
    let len = if size < 0 {
        unsafe { CStr::from_ptr(s) }.to_bytes().len()
    } else {
        size as usize
    };
    unsafe { std::slice::from_raw_parts(s as *const u8, len) }
}

/// Append every code point of `obj` (a `str`) to `w`.
unsafe fn write_str_object(w: &mut PyUnicodeWriter, obj: *mut PyObject) -> c_int {
    match unsafe { clone_object_value(obj) }.str_codepoints() {
        Some(cps) => {
            w.cps.extend(cps);
            0
        }
        None => {
            crate::errors::set_type_error("PyUnicodeWriter: expected str");
            -1
        }
    }
}

/// `PyUnicodeWriter_WriteUTF8(writer, str, size)` — `size < 0` means
/// `strlen`; invalid UTF-8 is a `UnicodeDecodeError` (strict).
#[no_mangle]
pub unsafe extern "C" fn PyUnicodeWriter_WriteUTF8(
    writer: *mut PyUnicodeWriter,
    s: *const c_char,
    size: PySsizeT,
) -> c_int {
    let Some(w) = writer_mut(writer) else {
        return -1;
    };
    let bytes = unsafe { c_slice(s, size) };
    let decoded = unsafe {
        crate::strings::PyUnicode_DecodeUTF8(
            bytes.as_ptr() as *const c_char,
            bytes.len() as PySsizeT,
            ptr::null(),
        )
    };
    if decoded.is_null() {
        return -1;
    }
    let rc = unsafe { write_str_object(w, decoded) };
    unsafe { crate::object::Py_DecRef(decoded) };
    rc
}

/// `PyUnicodeWriter_WriteASCII(writer, str, size)` — the caller
/// guarantees ASCII; bytes are taken as code points.
#[no_mangle]
pub unsafe extern "C" fn PyUnicodeWriter_WriteASCII(
    writer: *mut PyUnicodeWriter,
    s: *const c_char,
    size: PySsizeT,
) -> c_int {
    let Some(w) = writer_mut(writer) else {
        return -1;
    };
    let bytes = unsafe { c_slice(s, size) };
    w.cps.extend(bytes.iter().map(|&b| b as u32));
    0
}

/// `PyUnicodeWriter_WriteWideChar(writer, str, size)` — `wchar_t` is
/// 32-bit on every non-Windows target WeavePy builds; `size < 0` means
/// `wcslen`.
#[no_mangle]
#[allow(clippy::unnecessary_cast)]
pub unsafe extern "C" fn PyUnicodeWriter_WriteWideChar(
    writer: *mut PyUnicodeWriter,
    s: *const libc::wchar_t,
    size: PySsizeT,
) -> c_int {
    let Some(w) = writer_mut(writer) else {
        return -1;
    };
    if s.is_null() {
        return 0;
    }
    let len = if size < 0 {
        unsafe { libc::wcslen(s) }
    } else {
        size as usize
    };
    let units = unsafe { std::slice::from_raw_parts(s, len) };
    let mut out = Vec::with_capacity(len);
    if std::mem::size_of::<libc::wchar_t>() == 2 {
        // UTF-16 surrogate pairs (Windows).
        let mut i = 0;
        while i < units.len() {
            let u = units[i] as u32 & 0xFFFF;
            if (0xD800..0xDC00).contains(&u) && i + 1 < units.len() {
                let lo = units[i + 1] as u32 & 0xFFFF;
                if (0xDC00..0xE000).contains(&lo) {
                    out.push(0x10000 + ((u - 0xD800) << 10) + (lo - 0xDC00));
                    i += 2;
                    continue;
                }
            }
            out.push(u);
            i += 1;
        }
    } else {
        for &u in units {
            let cp = u as u32;
            if cp > 0x10_FFFF {
                crate::errors::set_value_error("character is not in range [U+0000; U+10ffff]");
                return -1;
            }
            out.push(cp);
        }
    }
    w.cps.extend(out);
    0
}

/// `PyUnicodeWriter_WriteUCS4(writer, str, size)` — `size` must be
/// non-negative (there is no terminator convention for UCS-4).
#[no_mangle]
pub unsafe extern "C" fn PyUnicodeWriter_WriteUCS4(
    writer: *mut PyUnicodeWriter,
    s: *const u32,
    size: PySsizeT,
) -> c_int {
    let Some(w) = writer_mut(writer) else {
        return -1;
    };
    if size < 0 {
        crate::errors::set_value_error("size must be positive");
        return -1;
    }
    if size == 0 || s.is_null() {
        return 0;
    }
    let units = unsafe { std::slice::from_raw_parts(s, size as usize) };
    for &cp in units {
        if cp > 0x10_FFFF {
            crate::errors::set_value_error("character must be in range(0x110000)");
            return -1;
        }
    }
    w.cps.extend_from_slice(units);
    0
}

/// `PyUnicodeWriter_WriteStr(writer, obj)` — `str(obj)`.
#[no_mangle]
pub unsafe extern "C" fn PyUnicodeWriter_WriteStr(
    writer: *mut PyUnicodeWriter,
    obj: *mut PyObject,
) -> c_int {
    let Some(w) = writer_mut(writer) else {
        return -1;
    };
    if obj.is_null() {
        set_system_error("PyUnicodeWriter_WriteStr: NULL object");
        return -1;
    }
    let s = unsafe { crate::abstract_::PyObject_Str(obj) };
    if s.is_null() {
        return -1;
    }
    let rc = unsafe { write_str_object(w, s) };
    unsafe { crate::object::Py_DecRef(s) };
    rc
}

/// `PyUnicodeWriter_WriteRepr(writer, obj)` — `repr(obj)`; a NULL
/// object writes `<NULL>` like `%R` in `PyUnicode_FromFormat`.
#[no_mangle]
pub unsafe extern "C" fn PyUnicodeWriter_WriteRepr(
    writer: *mut PyUnicodeWriter,
    obj: *mut PyObject,
) -> c_int {
    let Some(w) = writer_mut(writer) else {
        return -1;
    };
    if obj.is_null() {
        w.cps.extend("<NULL>".chars().map(|c| c as u32));
        return 0;
    }
    let s = unsafe { crate::abstract_::PyObject_Repr(obj) };
    if s.is_null() {
        return -1;
    }
    let rc = unsafe { write_str_object(w, s) };
    unsafe { crate::object::Py_DecRef(s) };
    rc
}

/// `PyUnicodeWriter_WriteSubstring(writer, str, start, end)`.
#[no_mangle]
pub unsafe extern "C" fn PyUnicodeWriter_WriteSubstring(
    writer: *mut PyUnicodeWriter,
    s: *mut PyObject,
    start: PySsizeT,
    end: PySsizeT,
) -> c_int {
    let Some(w) = writer_mut(writer) else {
        return -1;
    };
    if s.is_null() {
        set_system_error("PyUnicodeWriter_WriteSubstring: NULL str");
        return -1;
    }
    let Some(cps) = (unsafe { clone_object_value(s) }).str_codepoints() else {
        crate::errors::set_type_error("PyUnicodeWriter_WriteSubstring: expected str");
        return -1;
    };
    let n = cps.len() as PySsizeT;
    if start < 0 || start > n {
        crate::errors::set_value_error("invalid start argument");
        return -1;
    }
    if end < start || end > n {
        crate::errors::set_value_error("invalid end argument");
        return -1;
    }
    w.cps.extend_from_slice(&cps[start as usize..end as usize]);
    0
}

/// `PyUnicodeWriter_DecodeUTF8Stateful(writer, string, length, errors,
/// consumed)` — with `consumed` a truncated trailing sequence is left
/// unconsumed instead of being an error; without it the whole input
/// is decoded under `errors`.
#[no_mangle]
pub unsafe extern "C" fn PyUnicodeWriter_DecodeUTF8Stateful(
    writer: *mut PyUnicodeWriter,
    string: *const c_char,
    length: PySsizeT,
    errors: *const c_char,
    consumed: *mut PySsizeT,
) -> c_int {
    let Some(w) = writer_mut(writer) else {
        return -1;
    };
    let bytes = unsafe { c_slice(string, length) };
    let decoded = unsafe {
        crate::abi313::PyUnicode_DecodeUTF8Stateful(
            bytes.as_ptr() as *const c_char,
            bytes.len() as PySsizeT,
            errors,
            consumed,
        )
    };
    if decoded.is_null() {
        if !consumed.is_null() {
            unsafe { *consumed = 0 };
        }
        return -1;
    }
    let rc = unsafe { write_str_object(w, decoded) };
    unsafe { crate::object::Py_DecRef(decoded) };
    rc
}

// ---------------------------------------------------------------------------
// PyLong: fixed-width conversions and sign predicates (3.14)
// ---------------------------------------------------------------------------

/// The exact integer value of `obj` (Int/Long/Bool, or an int-backed
/// instance), or `None` without setting an error.
unsafe fn exact_bigint(obj: *mut PyObject) -> Option<BigInt> {
    match unsafe { clone_object_value(obj) } {
        Object::Int(i) => Some(BigInt::from(i)),
        Object::Long(b) => Some((*b).clone()),
        Object::Bool(b) => Some(BigInt::from(b as i64)),
        _ => None,
    }
}

/// `_PyNumber_Index` semantics: an exact int passes through, anything
/// with `__index__` is converted, otherwise a TypeError is pending.
unsafe fn index_bigint(obj: *mut PyObject) -> Option<BigInt> {
    if obj.is_null() {
        unsafe { crate::errors::PyErr_BadInternalCall() };
        return None;
    }
    if let Some(b) = unsafe { exact_bigint(obj) } {
        return Some(b);
    }
    let idx = unsafe { crate::abstract_::PyNumber_Index(obj) };
    if idx.is_null() {
        return None;
    }
    let out = unsafe { exact_bigint(idx) };
    unsafe { crate::object::Py_DecRef(idx) };
    if out.is_none() {
        crate::errors::set_type_error("__index__ returned non-int");
    }
    out
}

fn int_object(v: BigInt) -> *mut PyObject {
    match v.to_i64() {
        Some(small) => into_owned(Object::Int(small)),
        None => into_owned(Object::Long(Rc::new(v))),
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyLong_FromInt32(value: i32) -> *mut PyObject {
    into_owned(Object::Int(value as i64))
}

#[no_mangle]
pub unsafe extern "C" fn PyLong_FromUInt32(value: u32) -> *mut PyObject {
    into_owned(Object::Int(value as i64))
}

#[no_mangle]
pub unsafe extern "C" fn PyLong_FromInt64(value: i64) -> *mut PyObject {
    into_owned(Object::Int(value))
}

#[no_mangle]
pub unsafe extern "C" fn PyLong_FromUInt64(value: u64) -> *mut PyObject {
    int_object(BigInt::from(value))
}

unsafe fn as_signed_fixed(obj: *mut PyObject, bits: u32, what: &str) -> Option<i64> {
    let big = unsafe { index_bigint(obj) }?;
    let lo = -(BigInt::from(1) << (bits - 1));
    let hi = (BigInt::from(1) << (bits - 1)) - 1;
    if big < lo || big > hi {
        crate::errors::set_overflow_error(format!("Python int too large to convert to C {what}"));
        return None;
    }
    big.to_i64()
}

unsafe fn as_unsigned_fixed(obj: *mut PyObject, bits: u32, what: &str) -> Option<u64> {
    let big = unsafe { index_bigint(obj) }?;
    if big.sign() == Sign::Minus {
        crate::errors::set_value_error("can't convert negative int to unsigned");
        return None;
    }
    if big > (BigInt::from(1) << bits) - 1 {
        crate::errors::set_overflow_error(format!("Python int too large to convert to C {what}"));
        return None;
    }
    big.to_u64()
}

/// `PyLong_AsInt32(obj, &value)` — 0 on success, -1 with an error set.
#[no_mangle]
pub unsafe extern "C" fn PyLong_AsInt32(obj: *mut PyObject, value: *mut i32) -> c_int {
    match unsafe { as_signed_fixed(obj, 32, "int32_t") } {
        Some(v) => {
            if !value.is_null() {
                unsafe { *value = v as i32 };
            }
            0
        }
        None => -1,
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyLong_AsInt64(obj: *mut PyObject, value: *mut i64) -> c_int {
    match unsafe { as_signed_fixed(obj, 64, "int64_t") } {
        Some(v) => {
            if !value.is_null() {
                unsafe { *value = v };
            }
            0
        }
        None => -1,
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyLong_AsUInt32(obj: *mut PyObject, value: *mut u32) -> c_int {
    match unsafe { as_unsigned_fixed(obj, 32, "uint32_t") } {
        Some(v) => {
            if !value.is_null() {
                unsafe { *value = v as u32 };
            }
            0
        }
        None => -1,
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyLong_AsUInt64(obj: *mut PyObject, value: *mut u64) -> c_int {
    match unsafe { as_unsigned_fixed(obj, 64, "uint64_t") } {
        Some(v) => {
            if !value.is_null() {
                unsafe { *value = v };
            }
            0
        }
        None => -1,
    }
}

/// The sign of an exact int (`PyLong_Check` gate, no `__index__`), or
/// `None` with a TypeError pending.
unsafe fn exact_sign(obj: *mut PyObject, what: &str) -> Option<Sign> {
    if obj.is_null() {
        unsafe { crate::errors::PyErr_BadInternalCall() };
        return None;
    }
    match unsafe { exact_bigint(obj) } {
        Some(b) => Some(b.sign()),
        None => {
            crate::errors::set_type_error(format!("expected int, got {what}"));
            None
        }
    }
}

unsafe fn type_name_of(obj: *mut PyObject) -> String {
    weavepy_vm::builtins::class_of(&unsafe { clone_object(obj) })
        .name
        .clone()
}

/// `PyLong_GetSign(obj, &sign)` — 0 on success with `sign` in {-1,0,1}.
#[no_mangle]
pub unsafe extern "C" fn PyLong_GetSign(obj: *mut PyObject, sign: *mut c_int) -> c_int {
    let name = if obj.is_null() {
        String::new()
    } else {
        unsafe { type_name_of(obj) }
    };
    match unsafe { exact_sign(obj, &name) } {
        Some(s) => {
            if !sign.is_null() {
                unsafe {
                    *sign = match s {
                        Sign::Minus => -1,
                        Sign::NoSign => 0,
                        Sign::Plus => 1,
                    }
                };
            }
            0
        }
        None => -1,
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyLong_IsPositive(obj: *mut PyObject) -> c_int {
    let name = if obj.is_null() {
        String::new()
    } else {
        unsafe { type_name_of(obj) }
    };
    match unsafe { exact_sign(obj, &name) } {
        Some(Sign::Plus) => 1,
        Some(_) => 0,
        None => -1,
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyLong_IsNegative(obj: *mut PyObject) -> c_int {
    let name = if obj.is_null() {
        String::new()
    } else {
        unsafe { type_name_of(obj) }
    };
    match unsafe { exact_sign(obj, &name) } {
        Some(Sign::Minus) => 1,
        Some(_) => 0,
        None => -1,
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyLong_IsZero(obj: *mut PyObject) -> c_int {
    let name = if obj.is_null() {
        String::new()
    } else {
        unsafe { type_name_of(obj) }
    };
    match unsafe { exact_sign(obj, &name) } {
        Some(Sign::NoSign) => 1,
        Some(_) => 0,
        None => -1,
    }
}

/// `PyUnstable_Long_IsCompact(op)` — a single-digit (30-bit) magnitude.
#[no_mangle]
pub unsafe extern "C" fn PyUnstable_Long_IsCompact(op: *mut PyObject) -> c_int {
    match unsafe { exact_bigint(op) }.and_then(|b| b.to_i64()) {
        Some(v) if v.unsigned_abs() < (1 << PYLONG_BITS_PER_DIGIT) => 1,
        _ => 0,
    }
}

/// `PyUnstable_Long_CompactValue(op)` — only meaningful after
/// `IsCompact` returned 1.
#[no_mangle]
pub unsafe extern "C" fn PyUnstable_Long_CompactValue(op: *mut PyObject) -> PySsizeT {
    unsafe { exact_bigint(op) }
        .and_then(|b| b.to_i64())
        .unwrap_or(0) as PySsizeT
}

/// `PyLong_AsPid(obj)` — `pid_t` is a 32-bit `int` on every supported
/// platform (`pyport.h` defines it as `int` on Windows, where libc has
/// no `pid_t`); `__index__` accepted, range-checked as `int`.
#[no_mangle]
pub unsafe extern "C" fn PyLong_AsPid(obj: *mut PyObject) -> c_int {
    match unsafe { as_signed_fixed(obj, 32, "int") } {
        Some(v) => v as c_int,
        None => -1,
    }
}

// ---------------------------------------------------------------------------
// PEP 757: PyLong_Export / PyLongWriter
// ---------------------------------------------------------------------------

/// `sys.int_info.bits_per_digit`; the VM reports CPython's 30.
const PYLONG_BITS_PER_DIGIT: u32 = 30;

/// Byte-layout twin of `PyLongLayout`.
#[repr(C)]
pub struct PyLongLayout {
    pub bits_per_digit: u8,
    pub digit_size: u8,
    pub digits_order: i8,
    pub digit_endianness: i8,
}

static NATIVE_LAYOUT: PyLongLayout = PyLongLayout {
    bits_per_digit: PYLONG_BITS_PER_DIGIT as u8,
    digit_size: 4,
    digits_order: -1,
    digit_endianness: if cfg!(target_endian = "little") {
        -1
    } else {
        1
    },
};

/// `PyLong_GetNativeLayout()` — the layout `PyLong_Export` produces.
#[no_mangle]
pub unsafe extern "C" fn PyLong_GetNativeLayout() -> *const PyLongLayout {
    &NATIVE_LAYOUT
}

/// Byte-layout twin of `PyLongExport`.
#[repr(C)]
pub struct PyLongExport {
    pub value: i64,
    pub negative: u8,
    pub ndigits: PySsizeT,
    pub digits: *const c_void,
    /// A `Box<[u32]>` handed to `PyLong_FreeExport` (0 when `digits`
    /// is unused).
    pub _reserved: usize,
}

fn to_digits(mag: &BigInt) -> Vec<u32> {
    let mask = BigInt::from((1u64 << PYLONG_BITS_PER_DIGIT) - 1);
    let mut cur = mag.clone();
    let mut out = Vec::new();
    while !cur.is_zero() {
        out.push((&cur & &mask).to_u32().unwrap_or(0));
        cur >>= PYLONG_BITS_PER_DIGIT;
    }
    if out.is_empty() {
        out.push(0);
    }
    out
}

/// `PyLong_Export(obj, export)` — a value that fits `int64_t` is
/// returned in `value` (digits NULL); otherwise the 30-bit digit array
/// is exposed until `PyLong_FreeExport`.
#[no_mangle]
pub unsafe extern "C" fn PyLong_Export(obj: *mut PyObject, export: *mut PyLongExport) -> c_int {
    if export.is_null() {
        unsafe { crate::errors::PyErr_BadInternalCall() };
        return -1;
    }
    if obj.is_null() {
        unsafe { crate::errors::PyErr_BadInternalCall() };
        return -1;
    }
    let Some(big) = (unsafe { exact_bigint(obj) }) else {
        crate::errors::set_type_error(format!("expect int, got {}", unsafe { type_name_of(obj) }));
        return -1;
    };
    let ex = unsafe { &mut *export };
    if let Some(v) = big.to_i64() {
        ex.value = v;
        ex.negative = 0;
        ex.ndigits = 0;
        ex.digits = ptr::null();
        ex._reserved = 0;
        return 0;
    }
    let digits = to_digits(&BigInt::from(big.magnitude().clone())).into_boxed_slice();
    ex.value = 0;
    ex.negative = u8::from(big.sign() == Sign::Minus);
    ex.ndigits = digits.len() as PySsizeT;
    // The length rides in `ndigits`; `_reserved` keeps the data pointer
    // for `PyLong_FreeExport`.
    let data = Box::into_raw(digits) as *mut u32;
    ex.digits = data as *const c_void;
    ex._reserved = data as usize;
    0
}

/// `PyLong_FreeExport(export)` — release a digit array from `Export`.
#[no_mangle]
pub unsafe extern "C" fn PyLong_FreeExport(export: *mut PyLongExport) {
    if export.is_null() {
        return;
    }
    let ex = unsafe { &mut *export };
    if ex._reserved != 0 && ex.ndigits > 0 {
        let slice = ptr::slice_from_raw_parts_mut(ex._reserved as *mut u32, ex.ndigits as usize);
        drop(unsafe { Box::from_raw(slice) });
    }
    ex._reserved = 0;
    ex.digits = ptr::null();
    ex.ndigits = 0;
}

/// Opaque to C: the digit buffer being filled by the caller.
pub struct PyLongWriter {
    negative: bool,
    digits: Vec<u32>,
}

/// `PyLongWriter_Create(negative, ndigits, &digits)` — `ndigits` must be
/// positive; the caller fills `digits` (least-significant first) and
/// calls `Finish` or `Discard`.
#[no_mangle]
pub unsafe extern "C" fn PyLongWriter_Create(
    negative: c_int,
    ndigits: PySsizeT,
    digits: *mut *mut c_void,
) -> *mut PyLongWriter {
    if ndigits <= 0 {
        crate::errors::set_value_error("ndigits must be positive");
        return ptr::null_mut();
    }
    if digits.is_null() {
        unsafe { crate::errors::PyErr_BadInternalCall() };
        return ptr::null_mut();
    }
    let mut w = Box::new(PyLongWriter {
        negative: negative != 0,
        digits: vec![0u32; ndigits as usize],
    });
    unsafe { *digits = w.digits.as_mut_ptr() as *mut c_void };
    Box::into_raw(w)
}

/// `PyLongWriter_Finish(writer)` — the normalised int (small values hit
/// the VM's interned scalars), consuming the writer.
#[no_mangle]
pub unsafe extern "C" fn PyLongWriter_Finish(writer: *mut PyLongWriter) -> *mut PyObject {
    if writer.is_null() {
        set_system_error("PyLongWriter_Finish: NULL writer");
        return ptr::null_mut();
    }
    let w = unsafe { Box::from_raw(writer) };
    let mut value = BigInt::zero();
    for &d in w.digits.iter().rev() {
        value = (value << PYLONG_BITS_PER_DIGIT) + BigInt::from(d);
    }
    if w.negative {
        value = -value;
    }
    int_object(value)
}

#[no_mangle]
pub unsafe extern "C" fn PyLongWriter_Discard(writer: *mut PyLongWriter) {
    if !writer.is_null() {
        drop(unsafe { Box::from_raw(writer) });
    }
}

// ---------------------------------------------------------------------------
// Hashing, bytes, imports, strings
// ---------------------------------------------------------------------------

/// `Py_HashBuffer(ptr, len)` — the hash `bytes(ptr[:len])` would have,
/// so C extensions can hash buffer contents consistently with `bytes`.
#[no_mangle]
pub unsafe extern "C" fn Py_HashBuffer(p: *const c_void, len: PySsizeT) -> PySsizeT {
    let data: &[u8] = if p.is_null() || len <= 0 {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(p as *const u8, len as usize) }
    };
    let b = into_owned(Object::Bytes(SharedSlice::from(data)));
    let h = unsafe { crate::abstract_::PyObject_Hash(b) };
    unsafe { crate::object::Py_DecRef(b) };
    h as PySsizeT
}

/// `PyBytes_Join(sep, iterable)` — `sep.join(iterable)`; `sep` must be
/// bytes.
#[no_mangle]
pub unsafe extern "C" fn PyBytes_Join(
    sep: *mut PyObject,
    iterable: *mut PyObject,
) -> *mut PyObject {
    if sep.is_null() {
        unsafe { crate::errors::PyErr_BadInternalCall() };
        return ptr::null_mut();
    }
    if !matches!(unsafe { clone_object_value(sep) }, Object::Bytes(_)) {
        crate::errors::set_type_error(format!("sep: expected bytes, got {}", unsafe {
            type_name_of(sep)
        }));
        return ptr::null_mut();
    }
    if iterable.is_null() {
        unsafe { crate::errors::PyErr_BadInternalCall() };
        return ptr::null_mut();
    }
    let join = unsafe { crate::abstract_::PyObject_GetAttrString(sep, c"join".as_ptr()) };
    if join.is_null() {
        return ptr::null_mut();
    }
    let args = unsafe { crate::wave4::pack_tuple(&[iterable]) };
    if args.is_null() {
        unsafe { crate::object::Py_DecRef(join) };
        return ptr::null_mut();
    }
    let out = unsafe { crate::abstract_::PyObject_CallObject(join, args) };
    unsafe {
        crate::object::Py_DecRef(join);
        crate::object::Py_DecRef(args);
    }
    out
}

/// `PyImport_ImportModuleAttr(mod_name, attr_name)` — import the module
/// and return a new reference to its attribute.
#[no_mangle]
pub unsafe extern "C" fn PyImport_ImportModuleAttr(
    mod_name: *mut PyObject,
    attr_name: *mut PyObject,
) -> *mut PyObject {
    if mod_name.is_null() || attr_name.is_null() {
        unsafe { crate::errors::PyErr_BadInternalCall() };
        return ptr::null_mut();
    }
    if !unsafe { clone_object_value(mod_name) }.is_str() {
        crate::errors::set_type_error("module name must be a string");
        return ptr::null_mut();
    }
    if !unsafe { clone_object_value(attr_name) }.is_str() {
        crate::errors::set_type_error("attribute name must be a string");
        return ptr::null_mut();
    }
    let module = unsafe { crate::wave4::PyImport_Import(mod_name) };
    if module.is_null() {
        return ptr::null_mut();
    }
    let attr = unsafe { crate::abstract_::PyObject_GetAttr(module, attr_name) };
    unsafe { crate::object::Py_DecRef(module) };
    attr
}

/// `PyImport_ImportModuleAttrString(mod_name, attr_name)` — the UTF-8
/// `char*` spelling.
#[no_mangle]
pub unsafe extern "C" fn PyImport_ImportModuleAttrString(
    mod_name: *const c_char,
    attr_name: *const c_char,
) -> *mut PyObject {
    if mod_name.is_null() || attr_name.is_null() {
        unsafe { crate::errors::PyErr_BadInternalCall() };
        return ptr::null_mut();
    }
    let m = unsafe { crate::strings::PyUnicode_FromString(mod_name) };
    if m.is_null() {
        return ptr::null_mut();
    }
    let a = unsafe { crate::strings::PyUnicode_FromString(attr_name) };
    if a.is_null() {
        unsafe { crate::object::Py_DecRef(m) };
        return ptr::null_mut();
    }
    let out = unsafe { PyImport_ImportModuleAttr(m, a) };
    unsafe {
        crate::object::Py_DecRef(m);
        crate::object::Py_DecRef(a);
    }
    out
}

/// `PyUnicode_Equal(a, b)` — 1/0 for two `str` (subclasses included),
/// -1 with a TypeError otherwise.
#[no_mangle]
pub unsafe extern "C" fn PyUnicode_Equal(a: *mut PyObject, b: *mut PyObject) -> c_int {
    if a.is_null() || b.is_null() {
        unsafe { crate::errors::PyErr_BadInternalCall() };
        return -1;
    }
    let (Some(x), Some(y)) = (
        unsafe { clone_object_value(a) }.str_codepoints(),
        unsafe { clone_object_value(b) }.str_codepoints(),
    ) else {
        let bad = if unsafe { clone_object_value(a) }.is_str() {
            b
        } else {
            a
        };
        crate::errors::set_type_error(format!("first argument must be str, not {}", unsafe {
            type_name_of(bad)
        }));
        return -1;
    };
    c_int::from(x == y)
}

/// `PyUnicode_KIND(op)` as an exported function.
///
/// 3.14 exports the PEP 393 accessors as real functions next to the
/// header macros (`PyAPI_FUNC(int) PyUnicode_KIND` /
/// `PyAPI_FUNC(void*) PyUnicode_DATA` in `cpython/unicodeobject.h`) so
/// non-C consumers can call them; PyO3 0.26+ binds both as `extern`
/// symbols, and a manylinux wheel linked `-z now` (pydantic-core's
/// `_pydantic_core.so`) fails at `dlopen` with `undefined symbol:
/// PyUnicode_DATA` when they're missing. Every `str` that crosses into C
/// is minted with a faithful PEP 393 body (`mirror::fill_str`), so this
/// is the macro's read of the `state` word, no VM round trip.
#[no_mangle]
pub unsafe extern "C" fn PyUnicode_KIND(op: *mut PyObject) -> c_int {
    let ao = op as *const crate::layout::PyASCIIObject;
    let state = unsafe { (*ao).state };
    ((state >> crate::layout::ustate::KIND_SHIFT) & 0x7) as c_int
}

/// `PyUnicode_DATA(op)` as an exported function; see [`PyUnicode_KIND`].
/// Compact bodies carry their data just past `PyASCIIObject` (ASCII) or
/// `PyCompactUnicodeObject` (Latin-1/UCS-2/UCS-4); a non-compact body
/// (the `unicode_subtype_new` form) points at it through `data.any`.
#[no_mangle]
pub unsafe extern "C" fn PyUnicode_DATA(op: *mut PyObject) -> *mut c_void {
    use crate::layout::{ustate, PyASCIIObject, PyCompactUnicodeObject, PyUnicodeObject};
    let ao = op as *const PyASCIIObject;
    let state = unsafe { (*ao).state };
    let compact = (state >> ustate::COMPACT_SHIFT) & 0x1 != 0;
    if compact {
        let ascii = (state >> ustate::ASCII_SHIFT) & 0x1 != 0;
        let off = if ascii {
            std::mem::size_of::<PyASCIIObject>()
        } else {
            std::mem::size_of::<PyCompactUnicodeObject>()
        };
        unsafe { (op as *mut u8).add(off) as *mut c_void }
    } else {
        unsafe { (*(op as *const PyUnicodeObject)).data }
    }
}

// ---------------------------------------------------------------------------
// Types: tokens, freezing, names
// ---------------------------------------------------------------------------

/// The `Py_tp_token` of heap type `ty` (NULL when unset).
unsafe fn type_token(ty: *mut PyTypeObject) -> *mut c_void {
    if ty.is_null() {
        return ptr::null_mut();
    }
    match unsafe { crate::slottable::slot_table_for(ty) } {
        Some(table) => table.get(crate::slottable::Py_tp_token).as_void(),
        None => ptr::null_mut(),
    }
}

/// `PyType_GetBaseByToken(type, token, &result)` — walk the MRO for a
/// heap type whose `Py_tp_token` is `token`: 1 and a new reference in
/// `result` when found, 0/NULL otherwise, -1 on a bad argument.
#[no_mangle]
pub unsafe extern "C" fn PyType_GetBaseByToken(
    ty: *mut PyTypeObject,
    token: *mut c_void,
    result: *mut *mut PyTypeObject,
) -> c_int {
    if !result.is_null() {
        unsafe { *result = ptr::null_mut() };
    }
    if token.is_null() {
        set_system_error("PyType_GetBaseByToken called with token=NULL");
        return -1;
    }
    if ty.is_null() {
        set_system_error("PyType_GetBaseByToken called with type=NULL");
        return -1;
    }
    let Object::Type(t) = (unsafe { clone_object(ty as *mut PyObject) }) else {
        crate::errors::set_type_error(format!("expected a type, got a '{}' object", unsafe {
            type_name_of(ty as *mut PyObject)
        }));
        return -1;
    };
    if unsafe { type_token(ty) } == token {
        if !result.is_null() {
            unsafe {
                crate::object::Py_IncRef(ty as *mut PyObject);
                *result = ty;
            }
        }
        return 1;
    }
    let mro = t.mro.borrow().clone();
    for base in mro.iter() {
        let base_ptr = crate::types::install_user_type(base);
        if unsafe { type_token(base_ptr) } == token {
            if !result.is_null() {
                unsafe {
                    crate::object::Py_IncRef(base_ptr as *mut PyObject);
                    *result = base_ptr;
                }
            }
            return 1;
        }
    }
    0
}

/// `PyType_Freeze(type)` — set `Py_TPFLAGS_IMMUTABLETYPE`; every base
/// must already be immutable.
#[no_mangle]
pub unsafe extern "C" fn PyType_Freeze(ty: *mut PyTypeObject) -> c_int {
    if ty.is_null() {
        unsafe { crate::errors::PyErr_BadInternalCall() };
        return -1;
    }
    let Object::Type(t) = (unsafe { clone_object(ty as *mut PyObject) }) else {
        crate::errors::set_type_error("PyType_Freeze: not a type");
        return -1;
    };
    const IMMUTABLETYPE: i64 = 1 << 8;
    for base in t.mro.borrow().iter().skip(1) {
        if base.flags_bits() & IMMUTABLETYPE == 0 {
            crate::errors::set_type_error("Base type is not immutable");
            return -1;
        }
    }
    t.immutable.set(true);
    t.frozen_heap_type.set(true);
    0
}

/// `PyType_GetFullyQualifiedName(type)` — `module.qualname`, or just
/// `qualname` for `builtins`/`__main__`.
#[no_mangle]
pub unsafe extern "C" fn PyType_GetFullyQualifiedName(ty: *mut PyTypeObject) -> *mut PyObject {
    if ty.is_null() {
        unsafe { crate::errors::PyErr_BadInternalCall() };
        return ptr::null_mut();
    }
    let Object::Type(t) = (unsafe { clone_object(ty as *mut PyObject) }) else {
        crate::errors::set_type_error("PyType_GetFullyQualifiedName: not a type");
        return ptr::null_mut();
    };
    let qual = unsafe {
        crate::abstract_::PyObject_GetAttrString(ty as *mut PyObject, c"__qualname__".as_ptr())
    };
    let qualname = if qual.is_null() {
        crate::errors::clear_thread_local();
        t.name.clone()
    } else {
        let s = unsafe { clone_object_value(qual) }
            .str_codepoints()
            .map(|cps| {
                cps.into_iter()
                    .map(|c| char::from_u32(c).unwrap_or('\u{FFFD}'))
                    .collect::<String>()
            })
            .unwrap_or_else(|| t.name.clone());
        unsafe { crate::object::Py_DecRef(qual) };
        s
    };
    let module = unsafe { crate::abi313::PyType_GetModuleName(ty as *mut PyObject) };
    let modname = if module.is_null() {
        crate::errors::clear_thread_local();
        "builtins".to_owned()
    } else {
        let s = match unsafe { clone_object_value(module) } {
            Object::Str(s) => s.to_string(),
            _ => "builtins".to_owned(),
        };
        unsafe { crate::object::Py_DecRef(module) };
        s
    };
    let full = if modname == "builtins" || modname == "__main__" {
        qualname
    } else {
        format!("{modname}.{qualname}")
    };
    into_owned(Object::from_str(full))
}

// ---------------------------------------------------------------------------
// Files
// ---------------------------------------------------------------------------

/// `Py_fopen(path, mode)` — `fopen` on `os.fspath(path)` encoded with
/// the filesystem encoding; `OSError` on failure.
#[no_mangle]
pub unsafe extern "C" fn Py_fopen(path: *mut PyObject, mode: *const c_char) -> *mut libc::FILE {
    if path.is_null() || mode.is_null() {
        unsafe { crate::errors::PyErr_BadInternalCall() };
        return ptr::null_mut();
    }
    let fs = unsafe { crate::wave4::PyOS_FSPath(path) };
    if fs.is_null() {
        return ptr::null_mut();
    }
    let encoded = match unsafe { clone_object_value(fs) } {
        Object::Bytes(b) => b.to_vec(),
        _ => {
            let enc = unsafe { crate::strings::PyUnicode_EncodeFSDefault(fs) };
            if enc.is_null() {
                unsafe { crate::object::Py_DecRef(fs) };
                return ptr::null_mut();
            }
            let v = match unsafe { clone_object_value(enc) } {
                Object::Bytes(b) => b.to_vec(),
                _ => Vec::new(),
            };
            unsafe { crate::object::Py_DecRef(enc) };
            v
        }
    };
    unsafe { crate::object::Py_DecRef(fs) };
    let Ok(cpath) = std::ffi::CString::new(encoded) else {
        crate::errors::set_value_error("embedded null byte");
        return ptr::null_mut();
    };
    let f = unsafe { libc::fopen(cpath.as_ptr(), mode) };
    if f.is_null() {
        unsafe {
            crate::wave4::PyErr_SetFromErrno(crate::errors::PyExc_OSError);
        }
    }
    f
}

/// `Py_fclose(fp)`.
#[no_mangle]
pub unsafe extern "C" fn Py_fclose(fp: *mut libc::FILE) -> c_int {
    if fp.is_null() {
        return -1;
    }
    unsafe { libc::fclose(fp) }
}

/// `Py_UniversalNewlineFgets(buf, n, stream, fobj)` — `fgets` with
/// universal-newline translation: `\r\n` and a lone `\r` both become
/// `\n`. Returns `buf`, or NULL at EOF with nothing read. `fobj` is
/// unused (CPython asserts it is NULL).
#[no_mangle]
pub unsafe extern "C" fn Py_UniversalNewlineFgets(
    buf: *mut c_char,
    n: c_int,
    stream: *mut libc::FILE,
    _fobj: *mut PyObject,
) -> *mut c_char {
    if buf.is_null() || stream.is_null() || n <= 1 {
        return ptr::null_mut();
    }
    let mut written = 0usize;
    let cap = (n - 1) as usize;
    let mut skip_lf = false;
    while written < cap {
        let c = unsafe { libc::fgetc(stream) };
        if c == libc::EOF {
            break;
        }
        if skip_lf {
            skip_lf = false;
            if c == c_int::from(b'\n') {
                continue;
            }
        }
        if c == c_int::from(b'\r') {
            unsafe { *buf.add(written) = b'\n' as c_char };
            written += 1;
            skip_lf = true;
            break;
        }
        unsafe { *buf.add(written) = c as c_char };
        written += 1;
        if c == c_int::from(b'\n') {
            break;
        }
    }
    if skip_lf {
        // Swallow the `\n` of a `\r\n` pair that straddles the read.
        let c = unsafe { libc::fgetc(stream) };
        if c != libc::EOF && c != c_int::from(b'\n') {
            unsafe { libc::ungetc(c, stream) };
        }
    }
    unsafe { *buf.add(written) = 0 };
    if written == 0 {
        return ptr::null_mut();
    }
    buf
}

// ---------------------------------------------------------------------------
// PyConfig_Set (PEP 741 write side)
// ---------------------------------------------------------------------------

/// `PyConfig_Set(name, value)` — the runtime write side of PEP 741.
/// The handful of options CPython allows to change after
/// initialisation are the `sys.flags`-mirrored ints and `cpu_count`;
/// they are routed through the VM's `sys` module so `PyConfig_Get`
/// (which reads `sys` live) observes the change.
#[no_mangle]
pub unsafe extern "C" fn PyConfig_Set(name: *const c_char, value: *mut PyObject) -> c_int {
    if name.is_null() {
        crate::errors::set_value_error("PyConfig_Set: NULL option name");
        return -1;
    }
    let opt = unsafe { CStr::from_ptr(name) }
        .to_string_lossy()
        .into_owned();
    if value.is_null() {
        unsafe { crate::errors::PyErr_BadInternalCall() };
        return -1;
    }
    let cname = std::ffi::CString::new(opt.clone()).unwrap_or_default();
    let names = unsafe { crate::pep741::PyConfig_Names() };
    if names.is_null() {
        return -1;
    }
    let is_opt = |n: &Object| matches!(n, Object::Str(s) if **s == *opt);
    let known = match unsafe { clone_object_value(names) } {
        Object::Tuple(items) => items.iter().any(is_opt),
        Object::List(items) => items.borrow().iter().any(is_opt),
        _ => false,
    };
    unsafe { crate::object::Py_DecRef(names) };
    if !known {
        crate::errors::set_value_error(format!("unknown option name \"{opt}\""));
        return -1;
    }
    // Only the documented mutable options are writable at runtime.
    const WRITABLE: &[&str] = &["argv", "cpu_count", "int_max_str_digits"];
    const SYS_ATTR: &[(&str, &str)] = &[
        ("argv", "argv"),
        ("int_max_str_digits", "int_max_str_digits"),
    ];
    if !WRITABLE.contains(&opt.as_str()) {
        crate::errors::set_value_error(format!("cannot set option \"{opt}\""));
        return -1;
    }
    let _ = cname;
    match opt.as_str() {
        "cpu_count" => {
            if !matches!(
                unsafe { clone_object_value(value) },
                Object::Int(_) | Object::Bool(_)
            ) {
                crate::errors::set_type_error("\"cpu_count\" option must be an int");
                return -1;
            }
            let r = unsafe {
                crate::wave4::call_module_attr(
                    c"_testinternalcapi".as_ptr(),
                    c"set_cpu_count_override".as_ptr(),
                    &[value],
                )
            };
            if r.is_null() {
                // The fixture module may be absent in a stripped build;
                // treat the override as accepted but inert.
                crate::errors::clear_thread_local();
                return 0;
            }
            unsafe { crate::object::Py_DecRef(r) };
            0
        }
        _ => {
            let attr = SYS_ATTR
                .iter()
                .find(|(o, _)| *o == opt.as_str())
                .map(|(_, a)| *a)
                .unwrap_or(opt.as_str());
            let sys = unsafe { crate::module::PyImport_ImportModule(c"sys".as_ptr()) };
            if sys.is_null() {
                return -1;
            }
            let cattr = std::ffi::CString::new(attr).unwrap_or_default();
            let rc = if attr == "int_max_str_digits" {
                let setter = unsafe {
                    crate::abstract_::PyObject_GetAttrString(
                        sys,
                        c"set_int_max_str_digits".as_ptr(),
                    )
                };
                if setter.is_null() {
                    unsafe { crate::object::Py_DecRef(sys) };
                    return -1;
                }
                let args = unsafe { crate::wave4::pack_tuple(&[value]) };
                let r = unsafe { crate::abstract_::PyObject_CallObject(setter, args) };
                unsafe {
                    crate::object::Py_DecRef(setter);
                    crate::object::Py_DecRef(args);
                }
                if r.is_null() {
                    -1
                } else {
                    unsafe { crate::object::Py_DecRef(r) };
                    0
                }
            } else {
                unsafe { crate::abstract_::PyObject_SetAttrString(sys, cattr.as_ptr(), value) }
            };
            unsafe { crate::object::Py_DecRef(sys) };
            rc
        }
    }
}

// ---------------------------------------------------------------------------
// PyUnstable_* refcount hints
// ---------------------------------------------------------------------------

/// `PyUnstable_Object_IsUniqueReferencedTemporary(op)` — 1 when the
/// reference the caller holds is the only one.
#[no_mangle]
pub unsafe extern "C" fn PyUnstable_Object_IsUniqueReferencedTemporary(op: *mut PyObject) -> c_int {
    if op.is_null() {
        return 0;
    }
    let rc = unsafe { (*op).ob_refcnt };
    c_int::from(rc == 1)
}

/// `PyUnstable_Object_IsUniquelyReferenced(op)` — 1 when `op` has exactly
/// one strong reference (with a GIL that is a plain refcount test).
#[no_mangle]
pub unsafe extern "C" fn PyUnstable_Object_IsUniquelyReferenced(op: *mut PyObject) -> c_int {
    if op.is_null() {
        return 0;
    }
    c_int::from(unsafe { (*op).ob_refcnt } == 1)
}

/// `PyUnstable_Object_EnableDeferredRefcount(op)` — a free-threading
/// hint; WeavePy has no deferred-refcount mode, so 0 ("not enabled").
#[no_mangle]
pub unsafe extern "C" fn PyUnstable_Object_EnableDeferredRefcount(_op: *mut PyObject) -> c_int {
    0
}

/// `PyUnstable_IsImmortal(op)`.
#[no_mangle]
pub unsafe extern "C" fn PyUnstable_IsImmortal(op: *mut PyObject) -> c_int {
    if op.is_null() {
        return 0;
    }
    c_int::from(crate::object::is_immortal_refcnt(unsafe {
        (*op).ob_refcnt
    }))
}

/// `PyUnstable_TryIncRef(op)` — with a GIL the incref always succeeds.
#[no_mangle]
pub unsafe extern "C" fn PyUnstable_TryIncRef(op: *mut PyObject) -> c_int {
    if op.is_null() {
        return 0;
    }
    unsafe { crate::object::Py_IncRef(op) };
    1
}

/// `PyUnstable_EnableTryIncRef(op)` — nothing to enable here.
#[no_mangle]
pub unsafe extern "C" fn PyUnstable_EnableTryIncRef(_op: *mut PyObject) {}

/// `PyUnstable_Object_ClearWeakRefsNoCallbacks(op)` — drop every
/// weakref to `op` without running callbacks (a `tp_dealloc` helper
/// for types that manage their own weaklist).
#[no_mangle]
pub unsafe extern "C" fn PyUnstable_Object_ClearWeakRefsNoCallbacks(op: *mut PyObject) {
    if op.is_null() {
        return;
    }
    let obj = unsafe { clone_object(op) };
    let id = weavepy_vm::weakref_registry::id_of(&obj);
    // Clearing detaches the slots; the returned callbacks are dropped
    // unrun, which is the whole point of the NoCallbacks variant.
    drop(weavepy_vm::weakref_registry::notify_clear(id));
}

/// `PyWeakref_IsDead(ref)` — 1 dead, 0 alive, -1 with TypeError for a
/// non-weakref (or SystemError for NULL).
#[no_mangle]
pub unsafe extern "C" fn PyWeakref_IsDead(reference: *mut PyObject) -> c_int {
    if reference.is_null() {
        unsafe { crate::errors::PyErr_BadInternalCall() };
        return -1;
    }
    let wrapper = unsafe { clone_object(reference) };
    match weavepy_vm::stdlib::weakref_real::c_referent(&wrapper) {
        Some(Some(_)) => 0,
        Some(None) => 1,
        None => {
            crate::errors::set_type_error("expected a weakref");
            -1
        }
    }
}

// ---------------------------------------------------------------------------
// Clocks
// ---------------------------------------------------------------------------

fn wall_clock_ns() -> Option<i64> {
    let d = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?;
    i64::try_from(d.as_nanos()).ok()
}

/// `PyTime_Time(&t)` — wall clock in nanoseconds since the epoch.
#[no_mangle]
pub unsafe extern "C" fn PyTime_Time(result: *mut i64) -> c_int {
    if result.is_null() {
        return -1;
    }
    match wall_clock_ns() {
        Some(ns) => {
            unsafe { *result = ns };
            0
        }
        None => {
            crate::errors::set_overflow_error("timestamp too large to convert to C PyTime_t");
            -1
        }
    }
}

/// `PyTime_TimeRaw(&t)` — the signal-safe twin (never sets an error).
#[no_mangle]
pub unsafe extern "C" fn PyTime_TimeRaw(result: *mut i64) -> c_int {
    if result.is_null() {
        return -1;
    }
    match wall_clock_ns() {
        Some(ns) => {
            unsafe { *result = ns };
            0
        }
        None => -1,
    }
}

/// `PyTime_PerfCounter(&t)` — the highest-resolution monotonic clock;
/// shares the monotonic anchor.
#[no_mangle]
pub unsafe extern "C" fn PyTime_PerfCounter(result: *mut i64) -> c_int {
    unsafe { crate::modsupport_ext::PyTime_Monotonic(result) }
}

#[no_mangle]
pub unsafe extern "C" fn PyTime_PerfCounterRaw(result: *mut i64) -> c_int {
    unsafe { crate::modsupport_ext::PyTime_Monotonic(result) }
}

#[no_mangle]
pub unsafe extern "C" fn PyTime_MonotonicRaw(result: *mut i64) -> c_int {
    unsafe { crate::modsupport_ext::PyTime_Monotonic(result) }
}

// ---------------------------------------------------------------------------
// Audit hooks
// ---------------------------------------------------------------------------

/// `PySys_AuditTuple(event, args)` — `sys.audit(event, *args)`; a NULL
/// or non-tuple `args` audits with no arguments.
#[no_mangle]
pub unsafe extern "C" fn PySys_AuditTuple(event: *const c_char, args: *mut PyObject) -> c_int {
    if event.is_null() {
        unsafe { crate::errors::PyErr_BadInternalCall() };
        return -1;
    }
    let ev = unsafe { crate::strings::PyUnicode_FromString(event) };
    if ev.is_null() {
        return -1;
    }
    let mut argv: Vec<*mut PyObject> = vec![ev];
    let mut owned: Vec<*mut PyObject> = Vec::new();
    if !args.is_null() {
        if let Object::Tuple(items) = unsafe { clone_object_value(args) } {
            for item in items.iter() {
                let p = into_owned(item.clone());
                owned.push(p);
                argv.push(p);
            }
        }
    }
    let r = unsafe { crate::wave4::call_module_attr(c"sys".as_ptr(), c"audit".as_ptr(), &argv) };
    unsafe {
        crate::object::Py_DecRef(ev);
        for p in owned {
            crate::object::Py_DecRef(p);
        }
    }
    if r.is_null() {
        return -1;
    }
    unsafe { crate::object::Py_DecRef(r) };
    0
}

// ---------------------------------------------------------------------------
// Codecs
// ---------------------------------------------------------------------------

/// `PyCodec_NameReplaceErrors(exc)` — `codecs.namereplace_errors(exc)`.
#[no_mangle]
pub unsafe extern "C" fn PyCodec_NameReplaceErrors(exc: *mut PyObject) -> *mut PyObject {
    if exc.is_null() {
        unsafe { crate::errors::PyErr_BadInternalCall() };
        return ptr::null_mut();
    }
    unsafe {
        crate::wave4::call_module_attr(c"codecs".as_ptr(), c"namereplace_errors".as_ptr(), &[exc])
    }
}

// ---------------------------------------------------------------------------
// Code objects (3.11 audit items)
// ---------------------------------------------------------------------------

unsafe fn code_names(
    code: *mut PyObject,
    pick: fn(&weavepy_compiler::CodeObject) -> Vec<String>,
) -> *mut PyObject {
    let Object::Code(c) = (unsafe { clone_object(code) }) else {
        crate::errors::set_type_error("expected a code object");
        return ptr::null_mut();
    };
    let items: Vec<Object> = pick(&c).into_iter().map(Object::from_str).collect();
    into_owned(Object::new_tuple(items))
}

/// `PyCode_GetCellvars(code)` — new reference to `co_cellvars`.
#[no_mangle]
pub unsafe extern "C" fn PyCode_GetCellvars(code: *mut PyObject) -> *mut PyObject {
    unsafe { code_names(code, |c| c.cellvars.clone()) }
}

/// `PyCode_GetFreevars(code)` — new reference to `co_freevars`.
#[no_mangle]
pub unsafe extern "C" fn PyCode_GetFreevars(code: *mut PyObject) -> *mut PyObject {
    unsafe { code_names(code, |c| c.freevars.clone()) }
}

// ---------------------------------------------------------------------------
// Tracing / ref tracing / atexit (honest no-ops and registries)
// ---------------------------------------------------------------------------

/// `PyEval_SetTraceAllThreads(func, arg)` — like the profile twin, the
/// VM's tracing is Python-level (`sys.settrace`); C tracers are not
/// invoked.
#[no_mangle]
pub extern "C" fn PyEval_SetTraceAllThreads(_func: *mut c_void, _arg: *mut PyObject) {}

/// `PyThreadState_EnterTracing`/`LeaveTracing` — suppress C tracer
/// re-entry; nothing to suppress here.
#[no_mangle]
pub extern "C" fn PyThreadState_EnterTracing(_tstate: *mut crate::lifecycle::PyThreadState) {}

#[no_mangle]
pub extern "C" fn PyThreadState_LeaveTracing(_tstate: *mut crate::lifecycle::PyThreadState) {}

static REF_TRACER: AtomicUsize = AtomicUsize::new(0);
static REF_TRACER_DATA: AtomicUsize = AtomicUsize::new(0);

/// `PyRefTracer_SetTracer(tracer, data)` — stored (and readable back
/// through `GetTracer`); object creation/destruction events are not
/// reported because the VM allocates outside the C heap.
#[no_mangle]
pub unsafe extern "C" fn PyRefTracer_SetTracer(tracer: *mut c_void, data: *mut c_void) -> c_int {
    REF_TRACER.store(tracer as usize, Ordering::SeqCst);
    REF_TRACER_DATA.store(data as usize, Ordering::SeqCst);
    0
}

#[no_mangle]
pub unsafe extern "C" fn PyRefTracer_GetTracer(data: *mut *mut c_void) -> *mut c_void {
    if !data.is_null() {
        unsafe { *data = REF_TRACER_DATA.load(Ordering::SeqCst) as *mut c_void };
    }
    REF_TRACER.load(Ordering::SeqCst) as *mut c_void
}

type AtExitDataFn = unsafe extern "C" fn(*mut c_void);

struct AtExitEntry(AtExitDataFn, usize);
// SAFETY: a fn pointer and an opaque address behind a mutex.
unsafe impl Send for AtExitEntry {}

static UNSTABLE_ATEXIT: Mutex<Vec<AtExitEntry>> = Mutex::new(Vec::new());

/// `PyUnstable_AtExit(interp, func, data)` — per-interpreter atexit
/// with a data pointer; run LIFO by [`run_unstable_atexit`] from the
/// finalizer.
#[no_mangle]
pub unsafe extern "C" fn PyUnstable_AtExit(
    _interp: *mut c_void,
    func: Option<AtExitDataFn>,
    data: *mut c_void,
) -> c_int {
    let Some(func) = func else { return -1 };
    UNSTABLE_ATEXIT
        .lock()
        .unwrap()
        .push(AtExitEntry(func, data as usize));
    0
}

/// Drain the `PyUnstable_AtExit` table (called from `Py_FinalizeEx`).
pub fn run_unstable_atexit() {
    let entries = std::mem::take(&mut *UNSTABLE_ATEXIT.lock().unwrap());
    for AtExitEntry(f, data) in entries.into_iter().rev() {
        unsafe { f(data as *mut c_void) };
    }
}

// ---------------------------------------------------------------------------
// Watchers (code / function / type / context)
// ---------------------------------------------------------------------------

/// One 8-slot registry per watcher family. Registration hands out IDs
/// and validates them; the VM has no C-visible mutation hooks for these
/// objects, so the callbacks never fire (the same honest degradation
/// as `PyDict_AddWatcher`).
struct WatcherSlots(Mutex<[usize; 8]>);

impl WatcherSlots {
    const fn new() -> Self {
        WatcherSlots(Mutex::new([0; 8]))
    }

    fn add(&self, callback: *mut c_void, what: &str) -> c_int {
        if callback.is_null() {
            crate::errors::set_value_error(format!("{what} watcher callback must not be NULL"));
            return -1;
        }
        let mut slots = self.0.lock().unwrap();
        for (i, slot) in slots.iter_mut().enumerate() {
            if *slot == 0 {
                *slot = callback as usize;
                return i as c_int;
            }
        }
        crate::errors::set_runtime_error(format!("no more {what} watcher IDs available"));
        -1
    }

    fn clear(&self, id: c_int, what: &str) -> c_int {
        let mut slots = self.0.lock().unwrap();
        if !(0..8).contains(&id) || slots[id as usize] == 0 {
            crate::errors::set_value_error(format!("Invalid {what} watcher ID {id}"));
            return -1;
        }
        slots[id as usize] = 0;
        0
    }
}

static CODE_WATCHERS: WatcherSlots = WatcherSlots::new();
static FUNC_WATCHERS: WatcherSlots = WatcherSlots::new();
static TYPE_WATCHERS: WatcherSlots = WatcherSlots::new();
static CONTEXT_WATCHERS: WatcherSlots = WatcherSlots::new();

#[no_mangle]
pub extern "C" fn PyCode_AddWatcher(callback: *mut c_void) -> c_int {
    CODE_WATCHERS.add(callback, "code")
}

#[no_mangle]
pub extern "C" fn PyCode_ClearWatcher(id: c_int) -> c_int {
    CODE_WATCHERS.clear(id, "code")
}

#[no_mangle]
pub extern "C" fn PyFunction_AddWatcher(callback: *mut c_void) -> c_int {
    FUNC_WATCHERS.add(callback, "function")
}

#[no_mangle]
pub extern "C" fn PyFunction_ClearWatcher(id: c_int) -> c_int {
    FUNC_WATCHERS.clear(id, "function")
}

#[no_mangle]
pub extern "C" fn PyType_AddWatcher(callback: *mut c_void) -> c_int {
    TYPE_WATCHERS.add(callback, "type")
}

#[no_mangle]
pub extern "C" fn PyType_ClearWatcher(id: c_int) -> c_int {
    TYPE_WATCHERS.clear(id, "type")
}

/// `PyType_Watch(id, type)` / `PyType_Unwatch(id, type)` — validate the
/// ID; the mark itself has no observable effect without C callbacks.
#[no_mangle]
pub extern "C" fn PyType_Watch(id: c_int, ty: *mut PyObject) -> c_int {
    if ty.is_null() {
        unsafe { crate::errors::PyErr_BadInternalCall() };
        return -1;
    }
    let valid = (0..8).contains(&id) && TYPE_WATCHERS.0.lock().unwrap()[id as usize] != 0;
    if !valid {
        crate::errors::set_value_error(format!("Invalid type watcher ID {id}"));
        return -1;
    }
    0
}

#[no_mangle]
pub extern "C" fn PyType_Unwatch(id: c_int, ty: *mut PyObject) -> c_int {
    PyType_Watch(id, ty)
}

#[no_mangle]
pub extern "C" fn PyContext_AddWatcher(callback: *mut c_void) -> c_int {
    CONTEXT_WATCHERS.add(callback, "context")
}

#[no_mangle]
pub extern "C" fn PyContext_ClearWatcher(id: c_int) -> c_int {
    CONTEXT_WATCHERS.clear(id, "context")
}

// ---------------------------------------------------------------------------
// Stable-ABI function forms of the object-header macros
// ---------------------------------------------------------------------------

/// `Py_TYPE(ob)` as a function (the limited API links it instead of
/// reading `ob_type` inline).
#[no_mangle]
pub unsafe extern "C" fn Py_TYPE(ob: *mut PyObject) -> *mut PyTypeObject {
    if ob.is_null() {
        return ptr::null_mut();
    }
    unsafe { (*ob).ob_type }
}

/// `Py_SET_TYPE(ob, type)` as a function.
#[no_mangle]
pub unsafe extern "C" fn Py_SET_TYPE(ob: *mut PyObject, ty: *mut PyTypeObject) {
    if !ob.is_null() {
        unsafe { (*ob).ob_type = ty };
    }
}

/// `PyObject_GenericHash(obj)` — the default `tp_hash`: identity.
#[no_mangle]
pub unsafe extern "C" fn PyObject_GenericHash(obj: *mut PyObject) -> PySsizeT {
    unsafe { crate::genericalloc::Py_HashPointer(obj as *const c_void) }
}

// ---------------------------------------------------------------------------
// Critical sections (3.13): with a GIL these are empty scopes
// ---------------------------------------------------------------------------

/// Byte-layout twin of `PyCriticalSection` (the contents are private in
/// CPython too; nothing reads them here).
#[repr(C)]
pub struct PyCriticalSection {
    pub _cs_prev: usize,
    pub _cs_mutex: *mut c_void,
}

#[repr(C)]
pub struct PyCriticalSection2 {
    pub _cs_base: PyCriticalSection,
    pub _cs_mutex2: *mut c_void,
}

#[no_mangle]
pub extern "C" fn PyCriticalSection_Begin(_c: *mut PyCriticalSection, _op: *mut PyObject) {}

#[no_mangle]
pub extern "C" fn PyCriticalSection_BeginMutex(_c: *mut PyCriticalSection, _m: *mut c_void) {}

#[no_mangle]
pub extern "C" fn PyCriticalSection_End(_c: *mut PyCriticalSection) {}

#[no_mangle]
pub extern "C" fn PyCriticalSection2_Begin(
    _c: *mut PyCriticalSection2,
    _a: *mut PyObject,
    _b: *mut PyObject,
) {
}

#[no_mangle]
pub extern "C" fn PyCriticalSection2_BeginMutex(
    _c: *mut PyCriticalSection2,
    _m1: *mut c_void,
    _m2: *mut c_void,
) {
}

#[no_mangle]
pub extern "C" fn PyCriticalSection2_End(_c: *mut PyCriticalSection2) {}

// ---------------------------------------------------------------------------
// PyTuple_FromArray (private; Cython 3.1 binds it)
// ---------------------------------------------------------------------------

/// `_PyTuple_FromArray(items, n)` — a new tuple holding new references
/// to `items[0..n]`.
#[no_mangle]
pub unsafe extern "C" fn _PyTuple_FromArray(
    items: *const *mut PyObject,
    n: PySsizeT,
) -> *mut PyObject {
    if n < 0 || (items.is_null() && n > 0) {
        unsafe { crate::errors::PyErr_BadInternalCall() };
        return ptr::null_mut();
    }
    let slice = if n == 0 {
        &[][..]
    } else {
        unsafe { std::slice::from_raw_parts(items, n as usize) }
    };
    let mut out = Vec::with_capacity(slice.len());
    for &p in slice {
        if p.is_null() {
            set_system_error("_PyTuple_FromArray: NULL item");
            return ptr::null_mut();
        }
        out.push(unsafe { clone_object(p) });
    }
    into_owned(Object::new_tuple(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digits_round_trip() {
        let v = BigInt::from(1u64) << 100u32;
        let d = to_digits(&v);
        let mut back = BigInt::zero();
        for &x in d.iter().rev() {
            back = (back << PYLONG_BITS_PER_DIGIT) + BigInt::from(x);
        }
        assert_eq!(back, v);
        assert_eq!(d.len(), 4);
    }

    #[test]
    fn layout_matches_sys_int_info() {
        assert_eq!(NATIVE_LAYOUT.bits_per_digit, 30);
        assert_eq!(NATIVE_LAYOUT.digit_size, 4);
        assert_eq!(NATIVE_LAYOUT.digits_order, -1);
    }
}

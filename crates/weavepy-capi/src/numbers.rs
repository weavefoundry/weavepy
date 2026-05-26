//! `PyLong_*`, `PyFloat_*`, `PyBool_*`, `PyComplex_*`.

use std::ffi::CStr;
use std::os::raw::{c_char, c_int};
use std::ptr;
use weavepy_vm::sync::Rc;

use num_bigint::BigInt;
use num_traits::ToPrimitive;
use weavepy_vm::object::{Object, PyComplex};

use crate::object::PyObject;

// ---------- PyLong (Python `int`) ----------

#[no_mangle]
pub unsafe extern "C" fn PyLong_FromLong(v: i64) -> *mut PyObject {
    crate::object::into_owned(Object::Int(v))
}

#[no_mangle]
pub unsafe extern "C" fn PyLong_FromUnsignedLong(v: u64) -> *mut PyObject {
    if v <= i64::MAX as u64 {
        crate::object::into_owned(Object::Int(v as i64))
    } else {
        crate::object::into_owned(Object::Long(Rc::new(BigInt::from(v))))
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyLong_FromLongLong(v: i64) -> *mut PyObject {
    crate::object::into_owned(Object::Int(v))
}

#[no_mangle]
pub unsafe extern "C" fn PyLong_FromUnsignedLongLong(v: u64) -> *mut PyObject {
    unsafe { PyLong_FromUnsignedLong(v) }
}

#[no_mangle]
pub unsafe extern "C" fn PyLong_FromSsize_t(v: isize) -> *mut PyObject {
    crate::object::into_owned(Object::Int(v as i64))
}

#[no_mangle]
pub unsafe extern "C" fn PyLong_FromSize_t(v: usize) -> *mut PyObject {
    if v <= i64::MAX as usize {
        crate::object::into_owned(Object::Int(v as i64))
    } else {
        crate::object::into_owned(Object::Long(Rc::new(BigInt::from(v as u64))))
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyLong_FromDouble(v: f64) -> *mut PyObject {
    if v.is_nan() || v.is_infinite() {
        crate::errors::set_overflow_error("cannot convert float infinity/NaN to int");
        return ptr::null_mut();
    }
    crate::object::into_owned(Object::Int(v.trunc() as i64))
}

#[no_mangle]
pub unsafe extern "C" fn PyLong_FromString(
    s: *const c_char,
    end: *mut *mut c_char,
    base: c_int,
) -> *mut PyObject {
    if s.is_null() {
        crate::errors::set_value_error("PyLong_FromString: NULL pointer");
        return ptr::null_mut();
    }
    let s_bytes = unsafe { CStr::from_ptr(s) }.to_bytes();
    let s_str = std::str::from_utf8(s_bytes).unwrap_or("");
    let trimmed = s_str.trim();
    let radix = if base == 0 { 10 } else { base as u32 };
    match BigInt::parse_bytes(trimmed.as_bytes(), radix) {
        Some(big) => {
            if !end.is_null() {
                unsafe {
                    *end = s.add(s_bytes.len()).cast_mut();
                }
            }
            if let Some(small) = big.to_i64() {
                crate::object::into_owned(Object::Int(small))
            } else {
                crate::object::into_owned(Object::Long(Rc::new(big)))
            }
        }
        None => {
            crate::errors::set_value_error(format!(
                "invalid literal for int() with base {}: {}",
                radix, trimmed
            ));
            ptr::null_mut()
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyLong_AsLong(o: *mut PyObject) -> i64 {
    if o.is_null() {
        crate::errors::set_type_error("PyLong_AsLong: NULL");
        return -1;
    }
    match unsafe { crate::object::clone_object(o) } {
        Object::Int(i) => i,
        Object::Bool(b) => i64::from(b),
        Object::Long(big) => match big.to_i64() {
            Some(v) => v,
            None => {
                crate::errors::set_overflow_error("Python int too large to convert to C long");
                -1
            }
        },
        Object::Float(f) => f.trunc() as i64,
        _ => {
            crate::errors::set_type_error("an integer is required");
            -1
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyLong_AsLongLong(o: *mut PyObject) -> i64 {
    unsafe { PyLong_AsLong(o) }
}

#[no_mangle]
pub unsafe extern "C" fn PyLong_AsUnsignedLong(o: *mut PyObject) -> u64 {
    let v = unsafe { PyLong_AsLong(o) };
    if v < 0 {
        if crate::errors::pending().is_none() {
            crate::errors::set_overflow_error("can't convert negative value to unsigned int");
        }
        return u64::MAX;
    }
    v as u64
}

#[no_mangle]
pub unsafe extern "C" fn PyLong_AsUnsignedLongLong(o: *mut PyObject) -> u64 {
    unsafe { PyLong_AsUnsignedLong(o) }
}

#[no_mangle]
pub unsafe extern "C" fn PyLong_AsSsize_t(o: *mut PyObject) -> isize {
    unsafe { PyLong_AsLong(o) as isize }
}

#[no_mangle]
pub unsafe extern "C" fn PyLong_AsDouble(o: *mut PyObject) -> f64 {
    if o.is_null() {
        crate::errors::set_type_error("PyLong_AsDouble: NULL");
        return -1.0;
    }
    match unsafe { crate::object::clone_object(o) } {
        Object::Int(i) => i as f64,
        Object::Long(big) => big.to_f64().unwrap_or(f64::INFINITY),
        Object::Bool(b) => f64::from(b as i32),
        Object::Float(f) => f,
        _ => {
            crate::errors::set_type_error("an integer is required");
            -1.0
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyLong_Check(o: *mut PyObject) -> c_int {
    if o.is_null() {
        return 0;
    }
    matches!(
        unsafe { crate::object::clone_object(o) },
        Object::Int(_) | Object::Long(_) | Object::Bool(_)
    )
    .into()
}

/// Convert an int to a C `long` with overflow detection
/// (CPython 3.0+).
///
/// Returns the long value on success; on a value that overflows
/// the C `long` range, returns `-1` and writes `1` (positive
/// overflow) or `-1` (negative overflow) through `overflow`.
/// On a type mismatch returns `-1` and sets a `TypeError`.
#[no_mangle]
pub unsafe extern "C" fn PyLong_AsLongAndOverflow(o: *mut PyObject, overflow: *mut c_int) -> i64 {
    if !overflow.is_null() {
        unsafe { *overflow = 0 };
    }
    if o.is_null() {
        crate::errors::set_type_error("PyLong_AsLongAndOverflow: NULL");
        return -1;
    }
    match unsafe { crate::object::clone_object(o) } {
        Object::Int(i) => i,
        Object::Bool(b) => i64::from(b),
        Object::Long(big) => match big.to_i64() {
            Some(v) => v,
            None => {
                if !overflow.is_null() {
                    let sign = match big.sign() {
                        num_bigint::Sign::Minus => -1,
                        _ => 1,
                    };
                    unsafe { *overflow = sign };
                }
                -1
            }
        },
        Object::Float(f) => f.trunc() as i64,
        _ => {
            crate::errors::set_type_error("an integer is required");
            -1
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyLong_AsLongLongAndOverflow(
    o: *mut PyObject,
    overflow: *mut c_int,
) -> i64 {
    unsafe { PyLong_AsLongAndOverflow(o, overflow) }
}

/// `PyLong_AsByteArray` — write the int's two's-complement
/// representation into a byte buffer.
#[no_mangle]
pub unsafe extern "C" fn _PyLong_AsByteArray(
    o: *mut PyObject,
    bytes: *mut u8,
    n: usize,
    little_endian: c_int,
    is_signed: c_int,
) -> c_int {
    if o.is_null() || bytes.is_null() {
        crate::errors::set_type_error("_PyLong_AsByteArray: NULL");
        return -1;
    }
    let big = match unsafe { crate::object::clone_object(o) } {
        Object::Int(i) => BigInt::from(i),
        Object::Long(b) => (*b).clone(),
        Object::Bool(b) => BigInt::from(b as i64),
        _ => {
            crate::errors::set_type_error("an integer is required");
            return -1;
        }
    };
    let mut buf: Vec<u8> = if is_signed != 0 {
        big.to_signed_bytes_le()
    } else {
        big.to_bytes_le().1
    };
    // Sign-extend or zero-extend to fit `n` bytes.
    let target = n;
    if buf.len() > target {
        crate::errors::set_overflow_error("int too big to convert");
        return -1;
    }
    let pad_byte = if is_signed != 0 && buf.last().copied().unwrap_or(0) & 0x80 != 0 {
        0xff
    } else {
        0x00
    };
    while buf.len() < target {
        buf.push(pad_byte);
    }
    if little_endian == 0 {
        buf.reverse();
    }
    unsafe { std::ptr::copy_nonoverlapping(buf.as_ptr(), bytes, target) };
    0
}

/// `PyLong_FromByteArray` — build a long from a byte buffer.
#[no_mangle]
pub unsafe extern "C" fn _PyLong_FromByteArray(
    bytes: *const u8,
    n: usize,
    little_endian: c_int,
    is_signed: c_int,
) -> *mut PyObject {
    if bytes.is_null() {
        crate::errors::set_type_error("_PyLong_FromByteArray: NULL");
        return ptr::null_mut();
    }
    let mut slice = unsafe { std::slice::from_raw_parts(bytes, n) }.to_vec();
    if little_endian == 0 {
        slice.reverse();
    }
    let big = if is_signed != 0 {
        BigInt::from_signed_bytes_le(&slice)
    } else {
        BigInt::from_bytes_le(num_bigint::Sign::Plus, &slice)
    };
    match big.to_i64() {
        Some(small) => crate::object::into_owned(Object::Int(small)),
        None => crate::object::into_owned(Object::Long(Rc::new(big))),
    }
}

/// Convert an `int` to a `void *`. CPython treats this as a
/// signed roundtrip through `Py_ssize_t`; we mirror that.
#[no_mangle]
pub unsafe extern "C" fn PyLong_AsVoidPtr(o: *mut PyObject) -> *mut std::ffi::c_void {
    let v = unsafe { PyLong_AsLongLong(o) };
    v as usize as *mut std::ffi::c_void
}

/// Build a new `int` whose value is the integer representation
/// of the pointer.
#[no_mangle]
pub unsafe extern "C" fn PyLong_FromVoidPtr(p: *const std::ffi::c_void) -> *mut PyObject {
    crate::object::into_owned(Object::Int(p as usize as i64))
}

/// `PyLong_GetInfo` — opaque "structseq" describing the int
/// implementation. CPython returns a struct with `bits_per_digit`
/// and `sizeof_digit`; we approximate with a 2-element tuple
/// since user code generally just reads attributes off it.
#[no_mangle]
pub unsafe extern "C" fn PyLong_GetInfo() -> *mut PyObject {
    crate::object::into_owned(Object::new_tuple(vec![Object::Int(30), Object::Int(4)]))
}

// ---------- PyFloat ----------

#[no_mangle]
pub unsafe extern "C" fn PyFloat_FromDouble(v: f64) -> *mut PyObject {
    crate::object::into_owned(Object::Float(v))
}

#[no_mangle]
pub unsafe extern "C" fn PyFloat_AsDouble(o: *mut PyObject) -> f64 {
    if o.is_null() {
        crate::errors::set_type_error("PyFloat_AsDouble: NULL");
        return -1.0;
    }
    match unsafe { crate::object::clone_object(o) } {
        Object::Float(f) => f,
        Object::Int(i) => i as f64,
        Object::Long(big) => big.to_f64().unwrap_or(f64::INFINITY),
        Object::Bool(b) => f64::from(b as i32),
        _ => {
            crate::errors::set_type_error("a float is required");
            -1.0
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyFloat_Check(o: *mut PyObject) -> c_int {
    if o.is_null() {
        return 0;
    }
    matches!(unsafe { crate::object::clone_object(o) }, Object::Float(_)).into()
}

#[no_mangle]
pub unsafe extern "C" fn PyFloat_GetMax() -> f64 {
    f64::MAX
}

#[no_mangle]
pub unsafe extern "C" fn PyFloat_GetMin() -> f64 {
    f64::MIN_POSITIVE
}

/// `PyFloat_GetInfo()` — returns a structseq-shaped info bundle.
/// User code expects attribute access (`.max`, `.min`, `.epsilon`,
/// `.dig`, …) so we publish it as a small tuple keyed by index.
#[no_mangle]
pub unsafe extern "C" fn PyFloat_GetInfo() -> *mut PyObject {
    crate::object::into_owned(Object::new_tuple(vec![
        Object::Float(f64::MAX),
        Object::Int(1024),
        Object::Int(308),
        Object::Float(f64::MIN_POSITIVE),
        Object::Int(-1021),
        Object::Int(-307),
        Object::Int(15),
        Object::Int(53),
        Object::Float(f64::EPSILON),
        Object::Int(2),
        Object::Int(1),
    ]))
}

/// `_PyFloat_Pack4` — pack a double into 4 IEEE-754 bytes.
/// `little_endian == 0` selects big-endian on the wire.
#[no_mangle]
pub unsafe extern "C" fn _PyFloat_Pack4(x: f64, p: *mut u8, little_endian: c_int) -> c_int {
    if p.is_null() {
        return -1;
    }
    let bytes = (x as f32).to_bits();
    let raw = if little_endian != 0 {
        bytes.to_le_bytes()
    } else {
        bytes.to_be_bytes()
    };
    unsafe { std::ptr::copy_nonoverlapping(raw.as_ptr(), p, 4) };
    0
}

#[no_mangle]
pub unsafe extern "C" fn _PyFloat_Pack8(x: f64, p: *mut u8, little_endian: c_int) -> c_int {
    if p.is_null() {
        return -1;
    }
    let bytes = x.to_bits();
    let raw = if little_endian != 0 {
        bytes.to_le_bytes()
    } else {
        bytes.to_be_bytes()
    };
    unsafe { std::ptr::copy_nonoverlapping(raw.as_ptr(), p, 8) };
    0
}

#[no_mangle]
pub unsafe extern "C" fn _PyFloat_Unpack4(p: *const u8, little_endian: c_int) -> f64 {
    if p.is_null() {
        return f64::NAN;
    }
    let mut buf = [0u8; 4];
    unsafe { std::ptr::copy_nonoverlapping(p, buf.as_mut_ptr(), 4) };
    let bits = if little_endian != 0 {
        u32::from_le_bytes(buf)
    } else {
        u32::from_be_bytes(buf)
    };
    f32::from_bits(bits) as f64
}

#[no_mangle]
pub unsafe extern "C" fn _PyFloat_Unpack8(p: *const u8, little_endian: c_int) -> f64 {
    if p.is_null() {
        return f64::NAN;
    }
    let mut buf = [0u8; 8];
    unsafe { std::ptr::copy_nonoverlapping(p, buf.as_mut_ptr(), 8) };
    let bits = if little_endian != 0 {
        u64::from_le_bytes(buf)
    } else {
        u64::from_be_bytes(buf)
    };
    f64::from_bits(bits)
}

// ---------- PyBool ----------

#[no_mangle]
pub unsafe extern "C" fn PyBool_FromLong(v: i64) -> *mut PyObject {
    if v != 0 {
        unsafe { crate::object::Py_IncRef(crate::singletons::true_ptr()) };
        crate::singletons::true_ptr()
    } else {
        unsafe { crate::object::Py_IncRef(crate::singletons::false_ptr()) };
        crate::singletons::false_ptr()
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyBool_Check(o: *mut PyObject) -> c_int {
    if o.is_null() {
        return 0;
    }
    matches!(unsafe { crate::object::clone_object(o) }, Object::Bool(_)).into()
}

// ---------- PyComplex ----------

#[no_mangle]
pub unsafe extern "C" fn PyComplex_FromDoubles(real: f64, imag: f64) -> *mut PyObject {
    crate::object::into_owned(Object::Complex(Rc::new(PyComplex { real, imag })))
}

#[no_mangle]
pub unsafe extern "C" fn PyComplex_RealAsDouble(o: *mut PyObject) -> f64 {
    if o.is_null() {
        return -1.0;
    }
    match unsafe { crate::object::clone_object(o) } {
        Object::Complex(c) => c.real,
        Object::Float(f) => f,
        Object::Int(i) => i as f64,
        Object::Long(big) => big.to_f64().unwrap_or(f64::INFINITY),
        _ => {
            crate::errors::set_type_error("a complex is required");
            -1.0
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyComplex_ImagAsDouble(o: *mut PyObject) -> f64 {
    if o.is_null() {
        return -1.0;
    }
    match unsafe { crate::object::clone_object(o) } {
        Object::Complex(c) => c.imag,
        _ => 0.0,
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyComplex_Check(o: *mut PyObject) -> c_int {
    if o.is_null() {
        return 0;
    }
    matches!(
        unsafe { crate::object::clone_object(o) },
        Object::Complex(_)
    )
    .into()
}

//! `PyLong_*`, `PyFloat_*`, `PyBool_*`, `PyComplex_*`.

use std::ffi::CStr;
use std::os::raw::{c_char, c_int};
use std::ptr;
use std::rc::Rc;

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

//! `PyUnicode_*` (str), `PyBytes_*`, `PyByteArray_*`.
//!
//! Strings are UTF-8 throughout. CPython's "raw `wchar_t` /
//! `PEP 393` compact representation" is hidden behind these helpers
//! — for the common path (ASCII / UTF-8) we expose the underlying
//! buffer directly via [`PyUnicode_AsUTF8`] without copying.

use std::cell::RefCell;
use std::ffi::CStr;
use std::os::raw::{c_char, c_int};
use std::ptr;
use std::rc::Rc;

use weavepy_vm::object::Object;

use crate::object::{PyObject, PySsizeT};

// ----------------------------------------------------------------
// PyUnicode (str).
// ----------------------------------------------------------------

#[no_mangle]
pub unsafe extern "C" fn PyUnicode_FromString(s: *const c_char) -> *mut PyObject {
    if s.is_null() {
        return ptr::null_mut();
    }
    let bytes = unsafe { CStr::from_ptr(s) }.to_bytes();
    let str_val = std::str::from_utf8(bytes).unwrap_or("");
    crate::object::into_owned(Object::from_str(str_val))
}

#[no_mangle]
pub unsafe extern "C" fn PyUnicode_FromStringAndSize(
    s: *const c_char,
    n: PySsizeT,
) -> *mut PyObject {
    if s.is_null() && n != 0 {
        return ptr::null_mut();
    }
    let len = n.max(0) as usize;
    let slice = if s.is_null() {
        b""
    } else {
        unsafe { std::slice::from_raw_parts(s as *const u8, len) }
    };
    let str_val = std::str::from_utf8(slice).unwrap_or("");
    crate::object::into_owned(Object::from_str(str_val))
}

// Cache of `(rc str, leaked bytes)` so that `PyUnicode_AsUTF8`
// returns a stable pointer for the lifetime of the string. CPython
// caches the UTF-8 representation on the str object itself; we
// approximate by leaking a `\0`-terminated copy on first call.
thread_local! {
    static UTF8_CACHE: RefCell<Vec<Rc<[u8]>>> = const { RefCell::new(Vec::new()) };
}

fn cache_cstr(s: &str) -> *const c_char {
    let mut bytes: Vec<u8> = s.as_bytes().to_vec();
    bytes.push(0);
    let rc: Rc<[u8]> = bytes.into();
    let p = rc.as_ptr() as *const c_char;
    UTF8_CACHE.with(|c| c.borrow_mut().push(rc));
    p
}

#[no_mangle]
pub unsafe extern "C" fn PyUnicode_AsUTF8(o: *mut PyObject) -> *const c_char {
    if o.is_null() {
        return ptr::null();
    }
    match unsafe { crate::object::clone_object(o) } {
        Object::Str(s) => cache_cstr(&s),
        _ => {
            crate::errors::set_type_error("expected str");
            ptr::null()
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyUnicode_AsUTF8AndSize(
    o: *mut PyObject,
    size: *mut PySsizeT,
) -> *const c_char {
    let p = unsafe { PyUnicode_AsUTF8(o) };
    if !size.is_null() && !p.is_null() {
        unsafe {
            *size = libc_strlen(p) as PySsizeT;
        }
    }
    p
}

fn libc_strlen(p: *const c_char) -> usize {
    if p.is_null() {
        return 0;
    }
    let mut n = 0;
    while unsafe { *p.add(n) } != 0 {
        n += 1;
    }
    n
}

#[no_mangle]
pub unsafe extern "C" fn PyUnicode_GetLength(o: *mut PyObject) -> PySsizeT {
    if o.is_null() {
        return -1;
    }
    match unsafe { crate::object::clone_object(o) } {
        Object::Str(s) => s.chars().count() as PySsizeT,
        _ => {
            crate::errors::set_type_error("expected str");
            -1
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyUnicode_Concat(a: *mut PyObject, b: *mut PyObject) -> *mut PyObject {
    if a.is_null() || b.is_null() {
        return ptr::null_mut();
    }
    let (sa, sb) = match (unsafe { crate::object::clone_object(a) }, unsafe {
        crate::object::clone_object(b)
    }) {
        (Object::Str(sa), Object::Str(sb)) => (sa, sb),
        _ => {
            crate::errors::set_type_error("PyUnicode_Concat: expected str");
            return ptr::null_mut();
        }
    };
    let mut combined = String::with_capacity(sa.len() + sb.len());
    combined.push_str(&sa);
    combined.push_str(&sb);
    crate::object::into_owned(Object::from_str(combined))
}

#[no_mangle]
pub unsafe extern "C" fn PyUnicode_Check(o: *mut PyObject) -> c_int {
    if o.is_null() {
        return 0;
    }
    matches!(unsafe { crate::object::clone_object(o) }, Object::Str(_)).into()
}

#[no_mangle]
pub unsafe extern "C" fn PyUnicode_CompareWithASCIIString(
    o: *mut PyObject,
    s: *const c_char,
) -> c_int {
    if o.is_null() || s.is_null() {
        return -1;
    }
    let cmp = unsafe { CStr::from_ptr(s) }.to_bytes();
    match unsafe { crate::object::clone_object(o) } {
        Object::Str(rs) => match rs.as_bytes().cmp(cmp) {
            std::cmp::Ordering::Less => -1,
            std::cmp::Ordering::Equal => 0,
            std::cmp::Ordering::Greater => 1,
        },
        _ => -1,
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyUnicode_AsEncodedString(
    o: *mut PyObject,
    _enc: *const c_char,
    _errors: *const c_char,
) -> *mut PyObject {
    // We treat all encodings as UTF-8 for the foundation; a future
    // RFC will add the codecs registry pass-through.
    unsafe { PyUnicode_AsUTF8String(o) }
}

#[no_mangle]
pub unsafe extern "C" fn PyUnicode_AsUTF8String(o: *mut PyObject) -> *mut PyObject {
    if o.is_null() {
        return ptr::null_mut();
    }
    match unsafe { crate::object::clone_object(o) } {
        Object::Str(s) => {
            let bytes: Rc<[u8]> = s.as_bytes().into();
            crate::object::into_owned(Object::Bytes(bytes))
        }
        _ => {
            crate::errors::set_type_error("expected str");
            ptr::null_mut()
        }
    }
}

// ----------------------------------------------------------------
// PyBytes.
// ----------------------------------------------------------------

#[no_mangle]
pub unsafe extern "C" fn PyBytes_FromString(s: *const c_char) -> *mut PyObject {
    if s.is_null() {
        return ptr::null_mut();
    }
    let bytes = unsafe { CStr::from_ptr(s) }.to_bytes();
    let rc: Rc<[u8]> = bytes.into();
    crate::object::into_owned(Object::Bytes(rc))
}

#[no_mangle]
pub unsafe extern "C" fn PyBytes_FromStringAndSize(s: *const c_char, n: PySsizeT) -> *mut PyObject {
    let len = n.max(0) as usize;
    let slice = if s.is_null() {
        vec![0u8; len]
    } else {
        unsafe { std::slice::from_raw_parts(s as *const u8, len).to_vec() }
    };
    let rc: Rc<[u8]> = slice.into();
    crate::object::into_owned(Object::Bytes(rc))
}

#[no_mangle]
pub unsafe extern "C" fn PyBytes_AsString(o: *mut PyObject) -> *mut c_char {
    if o.is_null() {
        return ptr::null_mut();
    }
    match unsafe { crate::object::clone_object(o) } {
        Object::Bytes(b) => {
            let mut owned = b.to_vec();
            owned.push(0);
            let rc: Rc<[u8]> = owned.into();
            let p = rc.as_ptr() as *mut c_char;
            UTF8_CACHE.with(|c| c.borrow_mut().push(rc));
            p
        }
        _ => {
            crate::errors::set_type_error("expected bytes");
            ptr::null_mut()
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyBytes_AsStringAndSize(
    o: *mut PyObject,
    buffer: *mut *mut c_char,
    length: *mut PySsizeT,
) -> c_int {
    if o.is_null() || buffer.is_null() {
        return -1;
    }
    match unsafe { crate::object::clone_object(o) } {
        Object::Bytes(b) => {
            let p = unsafe { PyBytes_AsString(o) };
            unsafe {
                *buffer = p;
                if !length.is_null() {
                    *length = b.len() as PySsizeT;
                }
            }
            0
        }
        _ => {
            crate::errors::set_type_error("expected bytes");
            -1
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyBytes_Size(o: *mut PyObject) -> PySsizeT {
    if o.is_null() {
        return -1;
    }
    match unsafe { crate::object::clone_object(o) } {
        Object::Bytes(b) => b.len() as PySsizeT,
        _ => -1,
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyBytes_Check(o: *mut PyObject) -> c_int {
    if o.is_null() {
        return 0;
    }
    matches!(unsafe { crate::object::clone_object(o) }, Object::Bytes(_)).into()
}

// ----------------------------------------------------------------
// PyByteArray.
// ----------------------------------------------------------------

#[no_mangle]
pub unsafe extern "C" fn PyByteArray_FromStringAndSize(
    s: *const c_char,
    n: PySsizeT,
) -> *mut PyObject {
    let len = n.max(0) as usize;
    let v = if s.is_null() {
        vec![0u8; len]
    } else {
        unsafe { std::slice::from_raw_parts(s as *const u8, len).to_vec() }
    };
    let inner = Rc::new(std::cell::RefCell::new(v));
    crate::object::into_owned(Object::ByteArray(inner))
}

#[no_mangle]
pub unsafe extern "C" fn PyByteArray_AsString(o: *mut PyObject) -> *mut c_char {
    if o.is_null() {
        return ptr::null_mut();
    }
    match unsafe { crate::object::clone_object(o) } {
        Object::ByteArray(b) => {
            let mut owned = b.borrow().clone();
            owned.push(0);
            let rc: Rc<[u8]> = owned.into();
            let p = rc.as_ptr() as *mut c_char;
            UTF8_CACHE.with(|c| c.borrow_mut().push(rc));
            p
        }
        _ => ptr::null_mut(),
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyByteArray_Size(o: *mut PyObject) -> PySsizeT {
    if o.is_null() {
        return -1;
    }
    match unsafe { crate::object::clone_object(o) } {
        Object::ByteArray(b) => b.borrow().len() as PySsizeT,
        _ => -1,
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyByteArray_Check(o: *mut PyObject) -> c_int {
    if o.is_null() {
        return 0;
    }
    matches!(
        unsafe { crate::object::clone_object(o) },
        Object::ByteArray(_)
    )
    .into()
}

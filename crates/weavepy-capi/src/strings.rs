//! `PyUnicode_*` (str), `PyBytes_*`, `PyByteArray_*`.
//!
//! Strings are UTF-8 throughout. CPython's "raw `wchar_t` /
//! `PEP 393` compact representation" is hidden behind these helpers
//! — for the common path (ASCII / UTF-8) we expose the underlying
//! buffer directly via [`PyUnicode_AsUTF8`] without copying.

use std::ffi::CStr;
use std::os::raw::{c_char, c_int};
use std::ptr;
use weavepy_vm::sync::Rc;
use weavepy_vm::sync::RefCell;

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
        Object::Str(s) => {
            let n = s.chars().count() as PySsizeT;
            if std::env::var_os("WEAVEPY_TRACE_UCS4").is_some() {
                eprintln!("[UCS4] GetLength({o:p}) = {n} value={s:?}");
            }
            n
        }
        other => {
            if std::env::var_os("WEAVEPY_TRACE_UCS4").is_some() {
                eprintln!("[UCS4] GetLength({o:p}) NOT-STR kind={}", other.type_name());
            }
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
    let inner = Rc::new(weavepy_vm::sync::RefCell::new(v));
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

// ----------------------------------------------------------------
// RFC 0029 — additional `PyUnicode_*` surface.
// ----------------------------------------------------------------

/// `PyUnicode_FromOrdinal(ord)` — build a single-character str
/// from a Unicode code point.
#[no_mangle]
pub unsafe extern "C" fn PyUnicode_FromOrdinal(ord: c_int) -> *mut PyObject {
    let cp = u32::try_from(ord).ok().and_then(char::from_u32);
    match cp {
        Some(c) => {
            let mut buf = [0u8; 4];
            let s = c.encode_utf8(&mut buf);
            crate::object::into_owned(Object::from_str(s.to_owned()))
        }
        None => {
            crate::errors::set_value_error("ordinal out of range for chr()");
            ptr::null_mut()
        }
    }
}

/// `PyUnicode_Decode(s, size, encoding, errors)` — build a str
/// from a raw byte buffer. We treat all encodings as UTF-8 for
/// now; codecs registry support is a future RFC.
#[no_mangle]
pub unsafe extern "C" fn PyUnicode_Decode(
    s: *const c_char,
    size: PySsizeT,
    _encoding: *const c_char,
    _errors: *const c_char,
) -> *mut PyObject {
    unsafe { PyUnicode_FromStringAndSize(s, size) }
}

/// `PyUnicode_DecodeUTF8(s, size, errors)`.
#[no_mangle]
pub unsafe extern "C" fn PyUnicode_DecodeUTF8(
    s: *const c_char,
    size: PySsizeT,
    _errors: *const c_char,
) -> *mut PyObject {
    unsafe { PyUnicode_FromStringAndSize(s, size) }
}

/// `PyUnicode_DecodeASCII(s, size, errors)`.
#[no_mangle]
pub unsafe extern "C" fn PyUnicode_DecodeASCII(
    s: *const c_char,
    size: PySsizeT,
    _errors: *const c_char,
) -> *mut PyObject {
    unsafe { PyUnicode_FromStringAndSize(s, size) }
}

/// `PyUnicode_DecodeLatin1(s, size, errors)` — Latin-1 source
/// bytes map 1:1 to U+0000..U+00FF.
#[no_mangle]
pub unsafe extern "C" fn PyUnicode_DecodeLatin1(
    s: *const c_char,
    size: PySsizeT,
    _errors: *const c_char,
) -> *mut PyObject {
    if s.is_null() && size != 0 {
        return ptr::null_mut();
    }
    let len = size.max(0) as usize;
    let slice = if s.is_null() {
        b""
    } else {
        unsafe { std::slice::from_raw_parts(s as *const u8, len) }
    };
    let mut out = String::with_capacity(len);
    for &b in slice {
        out.push(b as char);
    }
    crate::object::into_owned(Object::from_str(out))
}

/// `PyUnicode_FromEncodedObject(obj, encoding, errors)` — accept
/// a bytes object or buffer-protocol exporter and decode it.
#[no_mangle]
pub unsafe extern "C" fn PyUnicode_FromEncodedObject(
    obj: *mut PyObject,
    encoding: *const c_char,
    errors: *const c_char,
) -> *mut PyObject {
    if obj.is_null() {
        return ptr::null_mut();
    }
    match unsafe { crate::object::clone_object(obj) } {
        Object::Str(_) => unsafe {
            crate::object::Py_IncRef(obj);
            obj
        },
        Object::Bytes(b) => {
            let s = b.as_ptr() as *const c_char;
            unsafe { PyUnicode_Decode(s, b.len() as PySsizeT, encoding, errors) }
        }
        Object::ByteArray(b) => {
            let buf = b.borrow();
            let s = buf.as_ptr() as *const c_char;
            unsafe { PyUnicode_Decode(s, buf.len() as PySsizeT, encoding, errors) }
        }
        _ => {
            // CPython falls back to the PEP 3118 buffer protocol for any
            // bytes-like object that isn't an exact `bytes`/`bytearray`
            // (memoryview, array.array, mmap, and — crucially for numpy —
            // its `bytes_`/`str_` scalars and 0-d buffer exporters that
            // datetime64 array formatting hands to us). Mirror that instead
            // of rejecting everything non-native.
            let mut view = crate::buffer::Py_buffer::zeroed();
            // PyBUF_SIMPLE == 0
            let rc = unsafe { crate::buffer::PyObject_GetBuffer(obj, &mut view, 0) };
            if rc != 0 {
                // Replace the pending BufferError with the TypeError CPython
                // raises here (clearer for "needs a bytes-like object").
                crate::errors::set_type_error(
                    "decoding to str: need a bytes-like object",
                );
                return ptr::null_mut();
            }
            let data = view.buf as *const c_char;
            let len = view.len;
            let out = if data.is_null() || len == 0 {
                unsafe {
                    PyUnicode_Decode(b"".as_ptr() as *const c_char, 0, encoding, errors)
                }
            } else {
                unsafe { PyUnicode_Decode(data, len, encoding, errors) }
            };
            unsafe { crate::buffer::PyBuffer_Release(&mut view) };
            out
        }
    }
}

/// `PyUnicode_Substring(o, start, end)` — slice by code-point
/// offset (not byte offset).
#[no_mangle]
pub unsafe extern "C" fn PyUnicode_Substring(
    o: *mut PyObject,
    start: PySsizeT,
    end: PySsizeT,
) -> *mut PyObject {
    if o.is_null() {
        return ptr::null_mut();
    }
    match unsafe { crate::object::clone_object(o) } {
        Object::Str(s) => {
            let start = start.max(0) as usize;
            let end = end.max(0) as usize;
            let total = s.chars().count();
            let end = end.min(total);
            let start = start.min(end);
            let collected: String = s.chars().skip(start).take(end - start).collect();
            crate::object::into_owned(Object::from_str(collected))
        }
        _ => {
            crate::errors::set_type_error("PyUnicode_Substring: expected str");
            ptr::null_mut()
        }
    }
}

/// `PyUnicode_ReadChar(o, idx)` — read one code point.
#[no_mangle]
pub unsafe extern "C" fn PyUnicode_ReadChar(o: *mut PyObject, idx: PySsizeT) -> u32 {
    if o.is_null() {
        return u32::MAX;
    }
    // Fast path: a buffer-authoritative string reads straight from its
    // PEP 393 buffer (no per-call string rebuild).
    if idx >= 0 {
        if let Some(cp) = unsafe { crate::mirror::unicode_read_char(o, idx as usize) } {
            return cp;
        }
    }
    match unsafe { crate::object::clone_object(o) } {
        Object::Str(s) => {
            let i = idx.max(0) as usize;
            match s.chars().nth(i) {
                Some(c) => c as u32,
                None => {
                    crate::errors::set_value_error("string index out of range");
                    u32::MAX
                }
            }
        }
        _ => {
            crate::errors::set_type_error("expected str");
            u32::MAX
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyUnicode_Compare(a: *mut PyObject, b: *mut PyObject) -> c_int {
    if a.is_null() || b.is_null() {
        return -1;
    }
    match (unsafe { crate::object::clone_object(a) }, unsafe {
        crate::object::clone_object(b)
    }) {
        (Object::Str(sa), Object::Str(sb)) => match sa.cmp(&sb) {
            std::cmp::Ordering::Less => -1,
            std::cmp::Ordering::Equal => 0,
            std::cmp::Ordering::Greater => 1,
        },
        _ => {
            crate::errors::set_type_error("PyUnicode_Compare: expected str");
            -1
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyUnicode_RichCompare(
    a: *mut PyObject,
    b: *mut PyObject,
    op: c_int,
) -> *mut PyObject {
    let cmp = unsafe { PyUnicode_Compare(a, b) };
    if cmp == -1 && crate::errors::pending().is_some() {
        return ptr::null_mut();
    }
    let result = match op {
        0 => cmp < 0,  // Py_LT
        1 => cmp <= 0, // Py_LE
        2 => cmp == 0, // Py_EQ
        3 => cmp != 0, // Py_NE
        4 => cmp > 0,  // Py_GT
        5 => cmp >= 0, // Py_GE
        _ => false,
    };
    if result {
        unsafe { crate::object::Py_IncRef(crate::singletons::true_ptr()) };
        crate::singletons::true_ptr()
    } else {
        unsafe { crate::object::Py_IncRef(crate::singletons::false_ptr()) };
        crate::singletons::false_ptr()
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyUnicode_EqualToUTF8(o: *mut PyObject, s: *const c_char) -> c_int {
    if o.is_null() || s.is_null() {
        return 0;
    }
    let want = unsafe { CStr::from_ptr(s) }.to_string_lossy().into_owned();
    match unsafe { crate::object::clone_object(o) } {
        Object::Str(rs) => i32::from(&*rs == want.as_str()),
        _ => 0,
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyUnicode_EqualToUTF8AndSize(
    o: *mut PyObject,
    s: *const c_char,
    n: PySsizeT,
) -> c_int {
    if o.is_null() || s.is_null() {
        return 0;
    }
    let len = n.max(0) as usize;
    let want = unsafe { std::slice::from_raw_parts(s as *const u8, len) };
    match unsafe { crate::object::clone_object(o) } {
        Object::Str(rs) => i32::from(rs.as_bytes() == want),
        _ => 0,
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyUnicode_InternFromString(s: *const c_char) -> *mut PyObject {
    unsafe { PyUnicode_FromString(s) }
}

#[no_mangle]
pub unsafe extern "C" fn PyUnicode_InternInPlace(_p: *mut *mut PyObject) {
    // No-op: WeavePy doesn't have a separate interned-string
    // table. Strings are already content-addressed via Rc, which
    // gives us the same sharing semantics for compile-time
    // literals.
}

/// `PyUnicode_New(size, maxchar)` — mint a fresh, writable, zero-filled
/// unicode string of `size` code points at the PEP 393 kind implied by
/// `maxchar` (RFC 0047, wave 5). The result is a **buffer-authoritative**
/// mirror: a stock extension fills it with the inlined `PyUnicode_WRITE`
/// macro (a direct store at `PyUnicode_DATA(o) + i*kind`) and the bytes are
/// reconstructed when the string crosses back to the VM. This is the exact
/// idiom Cython's f-string / integer-format codegen uses.
#[no_mangle]
pub unsafe extern "C" fn PyUnicode_New(size: PySsizeT, maxchar: u32) -> *mut PyObject {
    let n = if size < 0 { 0 } else { size as usize };
    crate::mirror::new_unicode_mirror(n, maxchar)
}

/// `PyUnicode_WriteChar(o, idx, ch)` — store one code point into a writable
/// string built by `PyUnicode_New`. Returns 0 on success, -1 (with an
/// exception set) for an out-of-range index, a too-wide code point, or a
/// non-writable target.
#[no_mangle]
pub unsafe extern "C" fn PyUnicode_WriteChar(o: *mut PyObject, idx: PySsizeT, ch: u32) -> c_int {
    if o.is_null() || idx < 0 {
        crate::errors::set_value_error("PyUnicode_WriteChar: invalid argument");
        return -1;
    }
    match unsafe { crate::mirror::unicode_write_char(o, idx as usize, ch) } {
        Ok(()) => 0,
        Err(msg) => {
            crate::errors::set_value_error(msg);
            -1
        }
    }
}

/// `PyUnicode_CopyCharacters(to, to_start, from, from_start, how_many)` —
/// copy `how_many` code points from `from` into the writable string `to`
/// (RFC 0047, wave 5). Returns the number copied, or -1 with an exception
/// set. Cython's in-place concatenation fast path calls this straight after
/// `PyUnicode_Resize` to append the right operand.
#[no_mangle]
pub unsafe extern "C" fn PyUnicode_CopyCharacters(
    to: *mut PyObject,
    to_start: PySsizeT,
    from: *mut PyObject,
    from_start: PySsizeT,
    how_many: PySsizeT,
) -> PySsizeT {
    if to.is_null() || from.is_null() {
        crate::errors::set_type_error("PyUnicode_CopyCharacters: expected str");
        return -1;
    }
    let ts = to_start.max(0) as usize;
    let fs = from_start.max(0) as usize;
    let hm = how_many.max(0) as usize;
    match unsafe { crate::mirror::unicode_copy_characters(to, ts, from, fs, hm) } {
        Ok(n) => n as PySsizeT,
        Err(msg) => {
            crate::errors::set_value_error(msg);
            -1
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyUnicode_Contains(
    haystack: *mut PyObject,
    needle: *mut PyObject,
) -> c_int {
    if haystack.is_null() || needle.is_null() {
        return -1;
    }
    match (unsafe { crate::object::clone_object(haystack) }, unsafe {
        crate::object::clone_object(needle)
    }) {
        (Object::Str(h), Object::Str(n)) => i32::from(h.contains(&*n)),
        _ => {
            crate::errors::set_type_error("PyUnicode_Contains: expected str");
            -1
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyUnicode_IsIdentifier(o: *mut PyObject) -> c_int {
    if o.is_null() {
        return 0;
    }
    match unsafe { crate::object::clone_object(o) } {
        Object::Str(s) => {
            if s.is_empty() {
                return 0;
            }
            let mut chars = s.chars();
            let first = chars.next().unwrap();
            if !first.is_alphabetic() && first != '_' {
                return 0;
            }
            for c in chars {
                if !c.is_alphanumeric() && c != '_' {
                    return 0;
                }
            }
            1
        }
        _ => 0,
    }
}

/// `PyUnicode_Find(haystack, needle, start, end, direction)` —
/// return the index of `needle` in `haystack`, or -1 if missing,
/// or -2 on error.
#[no_mangle]
pub unsafe extern "C" fn PyUnicode_Find(
    haystack: *mut PyObject,
    needle: *mut PyObject,
    start: PySsizeT,
    end: PySsizeT,
    direction: c_int,
) -> PySsizeT {
    if haystack.is_null() || needle.is_null() {
        return -2;
    }
    let (h, n) = match (unsafe { crate::object::clone_object(haystack) }, unsafe {
        crate::object::clone_object(needle)
    }) {
        (Object::Str(h), Object::Str(n)) => (h.to_string(), n.to_string()),
        _ => {
            crate::errors::set_type_error("PyUnicode_Find: expected str");
            return -2;
        }
    };
    let start = start.max(0) as usize;
    let end = end.max(0) as usize;
    let slice: String = h
        .chars()
        .skip(start)
        .take(end.saturating_sub(start))
        .collect();
    let idx = if direction >= 0 {
        slice.find(&n)
    } else {
        slice.rfind(&n)
    };
    match idx {
        Some(byte_off) => {
            // Convert byte offset back to char offset.
            let char_off = slice[..byte_off].chars().count();
            (start + char_off) as PySsizeT
        }
        None => -1,
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyUnicode_FindChar(
    haystack: *mut PyObject,
    ch: u32,
    start: PySsizeT,
    end: PySsizeT,
    direction: c_int,
) -> PySsizeT {
    let needle = match char::from_u32(ch) {
        Some(c) => c.to_string(),
        None => return -1,
    };
    let needle_o = crate::object::into_owned(Object::from_str(needle));
    let r = unsafe { PyUnicode_Find(haystack, needle_o, start, end, direction) };
    unsafe { crate::object::Py_DecRef(needle_o) };
    r
}

#[no_mangle]
pub unsafe extern "C" fn PyUnicode_Tailmatch(
    o: *mut PyObject,
    substr: *mut PyObject,
    start: PySsizeT,
    end: PySsizeT,
    direction: c_int,
) -> c_int {
    if o.is_null() || substr.is_null() {
        return -1;
    }
    let (o_s, sub_s) = match (unsafe { crate::object::clone_object(o) }, unsafe {
        crate::object::clone_object(substr)
    }) {
        (Object::Str(o_s), Object::Str(s_s)) => (o_s.to_string(), s_s.to_string()),
        _ => return -1,
    };
    let chars: Vec<char> = o_s.chars().collect();
    let start = start.max(0) as usize;
    let end = (end.max(0) as usize).min(chars.len());
    if start > end {
        return 0;
    }
    let window: String = chars[start..end].iter().collect();
    if direction >= 0 {
        i32::from(window.ends_with(&sub_s))
    } else {
        i32::from(window.starts_with(&sub_s))
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyUnicode_Split(
    s: *mut PyObject,
    sep: *mut PyObject,
    max_split: PySsizeT,
) -> *mut PyObject {
    if s.is_null() {
        return ptr::null_mut();
    }
    let s_str = match unsafe { crate::object::clone_object(s) } {
        Object::Str(s) => s.to_string(),
        _ => {
            crate::errors::set_type_error("PyUnicode_Split: expected str");
            return ptr::null_mut();
        }
    };
    let sep_str = if sep.is_null() {
        None
    } else {
        match unsafe { crate::object::clone_object(sep) } {
            Object::Str(s) => Some(s.to_string()),
            Object::None => None,
            _ => {
                crate::errors::set_type_error("PyUnicode_Split: separator must be str or None");
                return ptr::null_mut();
            }
        }
    };
    let parts: Vec<Object> = match sep_str {
        Some(sep) => {
            let max = if max_split < 0 {
                usize::MAX
            } else {
                (max_split as usize) + 1
            };
            s_str
                .splitn(max, sep.as_str())
                .map(|p| Object::from_str(p))
                .collect()
        }
        None => s_str
            .split_whitespace()
            .map(|p| Object::from_str(p))
            .collect(),
    };
    crate::object::into_owned(Object::new_list(parts))
}

#[no_mangle]
pub unsafe extern "C" fn PyUnicode_Splitlines(s: *mut PyObject, keepends: c_int) -> *mut PyObject {
    if s.is_null() {
        return ptr::null_mut();
    }
    let s_str = match unsafe { crate::object::clone_object(s) } {
        Object::Str(s) => s.to_string(),
        _ => {
            crate::errors::set_type_error("expected str");
            return ptr::null_mut();
        }
    };
    let mut lines: Vec<Object> = Vec::new();
    let mut current = String::new();
    for ch in s_str.chars() {
        current.push(ch);
        if ch == '\n' || ch == '\r' {
            if keepends == 0 {
                current.pop();
            }
            lines.push(Object::from_str(current.clone()));
            current.clear();
        }
    }
    if !current.is_empty() {
        lines.push(Object::from_str(current));
    }
    crate::object::into_owned(Object::new_list(lines))
}

#[no_mangle]
pub unsafe extern "C" fn PyUnicode_Join(
    separator: *mut PyObject,
    seq: *mut PyObject,
) -> *mut PyObject {
    if separator.is_null() || seq.is_null() {
        return ptr::null_mut();
    }
    let sep_str = match unsafe { crate::object::clone_object(separator) } {
        Object::Str(s) => s.to_string(),
        _ => {
            crate::errors::set_type_error("separator must be str");
            return ptr::null_mut();
        }
    };

    // CPython's `PyUnicode_Join` runs `seq` through `PySequence_Fast`, so any
    // iterable (list, tuple, generator, set, dict-keys, list-subclass, …) is
    // accepted — not just the two builtin sequence types. Collect the elements
    // into a native `Vec<Object>`: use a direct borrow for the common
    // list/tuple fast paths and fall back to the iterator protocol otherwise.
    let elems: Vec<Object> = match unsafe { crate::object::clone_object(seq) } {
        Object::List(rc) => rc.borrow().iter().cloned().collect(),
        Object::Tuple(items) => items.iter().cloned().collect(),
        _ => {
            let it = unsafe { crate::abstract_::PyObject_GetIter(seq) };
            if it.is_null() {
                // Preserve CPython's message shape for non-iterables; the
                // iterator protocol already set a TypeError, but be explicit.
                if crate::errors::pending().is_none() {
                    crate::errors::set_type_error("can only join an iterable");
                }
                return ptr::null_mut();
            }
            let mut out: Vec<Object> = Vec::new();
            loop {
                let item = unsafe { crate::abstract_::PyIter_Next(it) };
                if item.is_null() {
                    break;
                }
                out.push(unsafe { crate::object::clone_object(item) });
                unsafe { crate::object::Py_DecRef(item) };
            }
            unsafe { crate::object::Py_DecRef(it) };
            if crate::errors::pending().is_some() {
                return ptr::null_mut();
            }
            out
        }
    };

    // Every element must be a `str`; mirror CPython's
    // "sequence item N: expected str instance, T found" TypeError otherwise.
    let mut items: Vec<String> = Vec::with_capacity(elems.len());
    for (i, o) in elems.iter().enumerate() {
        match o {
            Object::Str(s) => items.push(s.to_string()),
            other => {
                crate::errors::set_type_error(&format!(
                    "sequence item {}: expected str instance, {} found",
                    i,
                    other.type_name()
                ));
                return ptr::null_mut();
            }
        }
    }
    crate::object::into_owned(Object::from_str(items.join(&sep_str)))
}

#[no_mangle]
pub unsafe extern "C" fn PyUnicode_Replace(
    s: *mut PyObject,
    needle: *mut PyObject,
    replacement: *mut PyObject,
    max_count: PySsizeT,
) -> *mut PyObject {
    if s.is_null() || needle.is_null() || replacement.is_null() {
        return ptr::null_mut();
    }
    let (s_str, n_str, r_str) = match (
        unsafe { crate::object::clone_object(s) },
        unsafe { crate::object::clone_object(needle) },
        unsafe { crate::object::clone_object(replacement) },
    ) {
        (Object::Str(a), Object::Str(b), Object::Str(c)) => {
            (a.to_string(), b.to_string(), c.to_string())
        }
        _ => {
            crate::errors::set_type_error("PyUnicode_Replace: expected str");
            return ptr::null_mut();
        }
    };
    let count = if max_count < 0 {
        usize::MAX
    } else {
        max_count as usize
    };
    crate::object::into_owned(Object::from_str(s_str.replacen(&n_str, &r_str, count)))
}

#[no_mangle]
pub unsafe extern "C" fn PyUnicode_Fill(
    _o: *mut PyObject,
    _start: PySsizeT,
    _length: PySsizeT,
    _ch: u32,
) -> PySsizeT {
    -1
}

#[no_mangle]
pub unsafe extern "C" fn PyUnicode_FromKindAndData(
    kind: c_int,
    buffer: *const std::ffi::c_void,
    size: PySsizeT,
) -> *mut PyObject {
    // PEP 393: `data` is an array of `size` code units whose width is
    // given by `kind` (1/2/4 bytes per code point). Each code unit is a
    // raw code point — for the 1-byte kind that means Latin-1, NOT
    // UTF-8. numpy's UNICODE_getitem reads array elements back via
    // `PyUnicode_FromKindAndData(PyUnicode_4BYTE_KIND, ucs4, len)`, so
    // honoring `kind` is required to avoid truncating multi-char strings.
    let len = size.max(0) as usize;
    if buffer.is_null() || len == 0 {
        return crate::object::into_owned(Object::from_str(String::new()));
    }
    let mut s = String::with_capacity(len);
    match kind {
        2 => {
            let p = buffer as *const u16;
            for i in 0..len {
                let cp = unsafe { *p.add(i) } as u32;
                s.push(char::from_u32(cp).unwrap_or('\u{FFFD}'));
            }
        }
        4 => {
            let p = buffer as *const u32;
            for i in 0..len {
                let cp = unsafe { *p.add(i) };
                s.push(char::from_u32(cp).unwrap_or('\u{FFFD}'));
            }
        }
        // kind == 1 (Latin-1); also the fallback for the deprecated
        // wchar/0 kind. Each byte is a code point in 0..=255.
        _ => {
            let p = buffer as *const u8;
            for i in 0..len {
                let cp = unsafe { *p.add(i) } as u32;
                s.push(char::from_u32(cp).unwrap_or('\u{FFFD}'));
            }
        }
    }
    crate::object::into_owned(Object::from_str(s))
}

/// `PyUnicode_DecodeFSDefault` / `PyUnicode_EncodeFSDefault` —
/// pass-through to UTF-8 on every platform we support.
#[no_mangle]
pub unsafe extern "C" fn PyUnicode_DecodeFSDefault(s: *const c_char) -> *mut PyObject {
    unsafe { PyUnicode_FromString(s) }
}

#[no_mangle]
pub unsafe extern "C" fn PyUnicode_DecodeFSDefaultAndSize(
    s: *const c_char,
    n: PySsizeT,
) -> *mut PyObject {
    unsafe { PyUnicode_FromStringAndSize(s, n) }
}

#[no_mangle]
pub unsafe extern "C" fn PyUnicode_EncodeFSDefault(o: *mut PyObject) -> *mut PyObject {
    unsafe { PyUnicode_AsUTF8String(o) }
}

/// Codec aliases — we treat every encoding as UTF-8 for now;
/// the codecs registry is a future RFC.
#[no_mangle]
pub unsafe extern "C" fn PyUnicode_AsASCIIString(o: *mut PyObject) -> *mut PyObject {
    unsafe { PyUnicode_AsUTF8String(o) }
}

#[no_mangle]
pub unsafe extern "C" fn PyUnicode_AsLatin1String(o: *mut PyObject) -> *mut PyObject {
    if o.is_null() {
        return ptr::null_mut();
    }
    match unsafe { crate::object::clone_object(o) } {
        Object::Str(s) => {
            let bytes: Vec<u8> = s
                .chars()
                .map(|c| if (c as u32) < 256 { c as u8 } else { b'?' })
                .collect();
            let rc: Rc<[u8]> = bytes.into();
            crate::object::into_owned(Object::Bytes(rc))
        }
        _ => {
            crate::errors::set_type_error("expected str");
            ptr::null_mut()
        }
    }
}

// ----------------------------------------------------------------
// RFC 0029 — additional `PyBytes_*` surface.
// ----------------------------------------------------------------

#[no_mangle]
pub unsafe extern "C" fn PyBytes_FromObject(o: *mut PyObject) -> *mut PyObject {
    if o.is_null() {
        return ptr::null_mut();
    }
    match unsafe { crate::object::clone_object(o) } {
        Object::Bytes(_) => unsafe {
            crate::object::Py_IncRef(o);
            o
        },
        Object::ByteArray(b) => {
            let snapshot = b.borrow().clone();
            let rc: Rc<[u8]> = snapshot.into();
            crate::object::into_owned(Object::Bytes(rc))
        }
        Object::Str(s) => {
            let bytes: Rc<[u8]> = s.as_bytes().into();
            crate::object::into_owned(Object::Bytes(bytes))
        }
        Object::List(rc) => {
            let inner: Vec<u8> = rc
                .borrow()
                .iter()
                .map(|o| match o {
                    Object::Int(i) => *i as u8,
                    _ => 0,
                })
                .collect();
            let arr: Rc<[u8]> = inner.into();
            crate::object::into_owned(Object::Bytes(arr))
        }
        Object::Tuple(items) => {
            let inner: Vec<u8> = items
                .iter()
                .map(|o| match o {
                    Object::Int(i) => *i as u8,
                    _ => 0,
                })
                .collect();
            let arr: Rc<[u8]> = inner.into();
            crate::object::into_owned(Object::Bytes(arr))
        }
        _ => {
            crate::errors::set_type_error("cannot convert to bytes");
            ptr::null_mut()
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyBytes_Concat(p: *mut *mut PyObject, w: *mut PyObject) {
    if p.is_null() || w.is_null() {
        return;
    }
    let left = unsafe { *p };
    if left.is_null() {
        return;
    }
    match (unsafe { crate::object::clone_object(left) }, unsafe {
        crate::object::clone_object(w)
    }) {
        (Object::Bytes(a), Object::Bytes(b)) => {
            let mut out = a.to_vec();
            out.extend_from_slice(&b);
            let rc: Rc<[u8]> = out.into();
            let new_p = crate::object::into_owned(Object::Bytes(rc));
            unsafe {
                crate::object::Py_DecRef(left);
                *p = new_p;
            }
        }
        _ => {}
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyBytes_ConcatAndDel(p: *mut *mut PyObject, w: *mut PyObject) {
    unsafe { PyBytes_Concat(p, w) };
    unsafe { crate::object::Py_DecRef(w) };
}

#[no_mangle]
pub unsafe extern "C" fn PyBytes_FromFormat(
    fmt: *const c_char,
    arg0: *const c_char,
) -> *mut PyObject {
    // Minimal: %s replacement. Real CPython supports the printf
    // family; that's a future enhancement.
    if fmt.is_null() {
        return ptr::null_mut();
    }
    let fmt_s = unsafe { CStr::from_ptr(fmt) }
        .to_string_lossy()
        .into_owned();
    let arg_s = if arg0.is_null() {
        String::new()
    } else {
        unsafe { CStr::from_ptr(arg0) }
            .to_string_lossy()
            .into_owned()
    };
    let out = fmt_s.replacen("%s", &arg_s, 1);
    let rc: Rc<[u8]> = out.into_bytes().into();
    crate::object::into_owned(Object::Bytes(rc))
}

// ----------------------------------------------------------------
// RFC 0029 — additional `PyByteArray_*` surface.
// ----------------------------------------------------------------

#[no_mangle]
pub unsafe extern "C" fn PyByteArray_Resize(o: *mut PyObject, size: PySsizeT) -> c_int {
    if o.is_null() || size < 0 {
        return -1;
    }
    match unsafe { crate::object::clone_object(o) } {
        Object::ByteArray(b) => {
            let mut v = b.borrow_mut();
            v.resize(size as usize, 0);
            0
        }
        _ => {
            crate::errors::set_type_error("expected bytearray");
            -1
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyByteArray_Concat(a: *mut PyObject, b: *mut PyObject) -> *mut PyObject {
    if a.is_null() || b.is_null() {
        return ptr::null_mut();
    }
    let mut out = match unsafe { crate::object::clone_object(a) } {
        Object::ByteArray(rc) => rc.borrow().clone(),
        Object::Bytes(rc) => rc.to_vec(),
        _ => {
            crate::errors::set_type_error("PyByteArray_Concat: expected bytes-like");
            return ptr::null_mut();
        }
    };
    match unsafe { crate::object::clone_object(b) } {
        Object::ByteArray(rc) => out.extend_from_slice(&rc.borrow()),
        Object::Bytes(rc) => out.extend_from_slice(&rc),
        _ => {}
    }
    let inner = Rc::new(weavepy_vm::sync::RefCell::new(out));
    crate::object::into_owned(Object::ByteArray(inner))
}

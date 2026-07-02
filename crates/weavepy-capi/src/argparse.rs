//! Argument parsing — the non-variadic core that the C shim
//! ([`varargs.c`](../varargs.c)) calls into.
//!
//! `PyArg_ParseTuple` and friends are variadic; receiving varargs
//! in stable Rust is non-trivial, so we split the work:
//!
//! 1. The C shim parses the format string with a `va_list`, peels
//!    off each format unit, and calls the corresponding non-variadic
//!    Rust helper (`_WeavePy_Arg_Long`, `_WeavePy_Arg_Double`, etc.)
//!    with the destination pointer.
//! 2. The Rust helpers extract the value from the corresponding
//!    `PyObject *` slot and write it through the destination
//!    pointer, returning 0 on success or a negative number to
//!    signal a parse failure.

use std::os::raw::{c_char, c_int};
use std::ptr;

use num_traits::ToPrimitive;
use weavepy_vm::object::Object;

use crate::object::PyObject;

/// Inspect a tuple `args` at index `i`. Returns null if out of
/// bounds (callers report a TypeError).
fn item_at(args: *mut PyObject, i: i32) -> *mut PyObject {
    if args.is_null() {
        return ptr::null_mut();
    }
    let obj = unsafe { crate::object::clone_object(args) };
    let item = match obj {
        Object::Tuple(items) => items.get(i as usize).cloned(),
        Object::List(rc) => rc.borrow().get(i as usize).cloned(),
        _ => None,
    };
    item.map_or(ptr::null_mut(), crate::object::into_owned)
}

/// Length of a tuple/list. -1 on type error.
fn args_len(args: *mut PyObject) -> i32 {
    if args.is_null() {
        return 0;
    }
    match unsafe { crate::object::clone_object(args) } {
        Object::Tuple(items) => items.len() as i32,
        Object::List(rc) => rc.borrow().len() as i32,
        _ => -1,
    }
}

#[no_mangle]
pub unsafe extern "C" fn _WeavePy_Arg_Length(args: *mut PyObject) -> c_int {
    args_len(args)
}

#[no_mangle]
pub unsafe extern "C" fn _WeavePy_Arg_Item(args: *mut PyObject, i: c_int) -> *mut PyObject {
    item_at(args, i)
}

/// Read a long-ish int from `arg` and write it through `dest`.
/// Returns 0 on success, -1 on type error.
#[no_mangle]
pub unsafe extern "C" fn _WeavePy_Arg_Long(arg: *mut PyObject, dest: *mut i64) -> c_int {
    if arg.is_null() || dest.is_null() {
        return -1;
    }
    match unsafe { crate::object::clone_object(arg) } {
        Object::Int(i) => {
            unsafe { *dest = i };
            0
        }
        Object::Bool(b) => {
            unsafe { *dest = i64::from(b) };
            0
        }
        Object::Long(big) => match big.to_i64() {
            Some(v) => {
                unsafe { *dest = v };
                0
            }
            None => {
                crate::errors::set_overflow_error("Python int too large for C long");
                -1
            }
        },
        // CPython's integer format codes ('i'/'l'/'n'/'L'...) run the arg
        // through `PyLong_As*`, which coerces via `__index__` — so a numpy
        // integer scalar passed positionally is accepted. Mirror that.
        _ => match unsafe { crate::numbers::index_to_builtin_int(arg) } {
            Some(Object::Int(i)) => {
                unsafe { *dest = i };
                0
            }
            Some(Object::Bool(b)) => {
                unsafe { *dest = i64::from(b) };
                0
            }
            Some(Object::Long(big)) => match big.to_i64() {
                Some(v) => {
                    unsafe { *dest = v };
                    0
                }
                None => {
                    crate::errors::set_overflow_error("Python int too large for C long");
                    -1
                }
            },
            _ => -1,
        },
    }
}

#[no_mangle]
pub unsafe extern "C" fn _WeavePy_Arg_Int(arg: *mut PyObject, dest: *mut c_int) -> c_int {
    let mut v: i64 = 0;
    if unsafe { _WeavePy_Arg_Long(arg, &raw mut v) } != 0 {
        return -1;
    }
    if !(c_int::MIN as i64..=c_int::MAX as i64).contains(&v) {
        crate::errors::set_overflow_error("int out of range");
        return -1;
    }
    unsafe { *dest = v as c_int };
    0
}

#[no_mangle]
pub unsafe extern "C" fn _WeavePy_Arg_Double(arg: *mut PyObject, dest: *mut f64) -> c_int {
    if arg.is_null() || dest.is_null() {
        return -1;
    }
    match unsafe { crate::object::clone_object(arg) } {
        Object::Float(f) => {
            unsafe { *dest = f };
            0
        }
        Object::Int(i) => {
            unsafe { *dest = i as f64 };
            0
        }
        Object::Long(big) => {
            unsafe { *dest = big.to_f64().unwrap_or(f64::INFINITY) };
            0
        }
        Object::Bool(b) => {
            unsafe { *dest = f64::from(b as i32) };
            0
        }
        _ => {
            crate::errors::set_type_error("a float is required");
            -1
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn _WeavePy_Arg_String(
    arg: *mut PyObject,
    dest: *mut *const c_char,
) -> c_int {
    if arg.is_null() || dest.is_null() {
        return -1;
    }
    let p = unsafe { crate::strings::PyUnicode_AsUTF8(arg) };
    if p.is_null() {
        return -1;
    }
    unsafe { *dest = p };
    0
}

#[no_mangle]
pub unsafe extern "C" fn _WeavePy_Arg_StringAndSize(
    arg: *mut PyObject,
    dest: *mut *const c_char,
    dest_len: *mut isize,
) -> c_int {
    let mut sz: isize = 0;
    let p = unsafe { crate::strings::PyUnicode_AsUTF8AndSize(arg, &raw mut sz) };
    if p.is_null() {
        return -1;
    }
    unsafe {
        *dest = p;
        *dest_len = sz;
    }
    0
}

#[no_mangle]
pub unsafe extern "C" fn _WeavePy_Arg_Object(
    arg: *mut PyObject,
    dest: *mut *mut PyObject,
) -> c_int {
    if arg.is_null() || dest.is_null() {
        return -1;
    }
    unsafe {
        crate::object::Py_IncRef(arg);
        *dest = arg;
    }
    0
}

#[no_mangle]
pub unsafe extern "C" fn _WeavePy_Arg_Bool(arg: *mut PyObject, dest: *mut c_int) -> c_int {
    if arg.is_null() || dest.is_null() {
        return -1;
    }
    let v = unsafe { crate::abstract_::PyObject_IsTrue(arg) };
    if v < 0 {
        return -1;
    }
    unsafe { *dest = v };
    0
}

/// Lookup `kwargs[key]` (returning a new reference) or NULL when
/// either `kwargs` is NULL or the key is absent. Used by the
/// kw-aware C shim to bind named arguments.
#[no_mangle]
pub unsafe extern "C" fn _WeavePy_Kwargs_Pop(
    kwargs: *mut PyObject,
    key: *const c_char,
) -> *mut PyObject {
    if kwargs.is_null() || key.is_null() {
        return ptr::null_mut();
    }
    let key_obj = unsafe { std::ffi::CStr::from_ptr(key) }
        .to_string_lossy()
        .into_owned();
    let result = match unsafe { crate::object::clone_object(kwargs) } {
        Object::Dict(d) => {
            let k = weavepy_vm::object::DictKey(Object::from_str(key_obj));
            d.borrow().get(&k).cloned()
        }
        _ => None,
    };
    result.map_or(ptr::null_mut(), crate::object::into_owned)
}

/// Count how many kwargs are still present (used for error
/// reporting when extra keywords arrive).
#[no_mangle]
pub unsafe extern "C" fn _WeavePy_Kwargs_Len(kwargs: *mut PyObject) -> c_int {
    if kwargs.is_null() {
        return 0;
    }
    match unsafe { crate::object::clone_object(kwargs) } {
        Object::Dict(d) => d.borrow().len() as c_int,
        _ => 0,
    }
}

/// Iterate `kwargs` and return the i'th key as a borrowed C string.
/// Returns NULL when the index is out of range or when the key isn't
/// a string. Used by the C shim to detect "unexpected keyword
/// argument".
#[no_mangle]
pub unsafe extern "C" fn _WeavePy_Kwargs_KeyAt(kwargs: *mut PyObject, i: c_int) -> *const c_char {
    if kwargs.is_null() {
        return ptr::null();
    }
    match unsafe { crate::object::clone_object(kwargs) } {
        Object::Dict(d) => {
            let borrowed = d.borrow();
            let entry = borrowed.iter().nth(i as usize);
            match entry {
                Some((k, _)) => match &k.0 {
                    Object::Str(s) => {
                        let mut bytes = s.as_bytes().to_vec();
                        bytes.push(0);
                        Box::leak(bytes.into_boxed_slice()).as_ptr() as *const c_char
                    }
                    _ => ptr::null(),
                },
                None => ptr::null(),
            }
        }
        _ => ptr::null(),
    }
}

#[no_mangle]
pub unsafe extern "C" fn _WeavePy_Arg_Buffer(
    arg: *mut PyObject,
    buffer: *mut *mut c_char,
    length: *mut isize,
) -> c_int {
    if arg.is_null() {
        return -1;
    }
    match unsafe { crate::object::clone_object(arg) } {
        Object::Bytes(b) => {
            let mut bytes: Vec<u8> = b.to_vec();
            bytes.push(0);
            let leaked: Box<[u8]> = bytes.into_boxed_slice();
            let ptr_ = leaked.as_ptr() as *mut c_char;
            let len = leaked.len() - 1;
            std::mem::forget(leaked);
            unsafe {
                *buffer = ptr_;
                if !length.is_null() {
                    *length = len as isize;
                }
            }
            0
        }
        _ => {
            crate::errors::set_type_error("a bytes-like object is required");
            -1
        }
    }
}

// `PyArg_VaParse` is provided by the C shim; the Rust side
// declares the symbol so other Rust code can refer to it without a
// `#[link]` cycle.
extern "C" {
    pub fn PyArg_ParseTuple(args: *mut PyObject, fmt: *const c_char, ...) -> c_int;
    pub fn PyArg_ParseTupleAndKeywords(
        args: *mut PyObject,
        kwargs: *mut PyObject,
        fmt: *const c_char,
        kwlist: *mut *mut c_char,
        ...
    ) -> c_int;
    pub fn PyArg_VaParse(
        args: *mut PyObject,
        fmt: *const c_char,
        va: *mut std::ffi::c_void,
    ) -> c_int;
    pub fn PyArg_VaParseTupleAndKeywords(
        args: *mut PyObject,
        kwargs: *mut PyObject,
        fmt: *const c_char,
        kwlist: *mut *mut c_char,
        va: *mut std::ffi::c_void,
    ) -> c_int;
    pub fn PyArg_Parse(args: *mut PyObject, fmt: *const c_char, ...) -> c_int;
    pub fn PyArg_UnpackTuple(
        args: *mut PyObject,
        name: *const c_char,
        min_count: isize,
        max_count: isize,
        ...
    ) -> c_int;
    pub fn Py_BuildValue(fmt: *const c_char, ...) -> *mut PyObject;
    pub fn Py_VaBuildValue(fmt: *const c_char, va: *mut std::ffi::c_void) -> *mut PyObject;
    pub fn PyUnicode_FromFormat(fmt: *const c_char, ...) -> *mut PyObject;
    pub fn PyUnicode_FromFormatV(fmt: *const c_char, va: *mut std::ffi::c_void) -> *mut PyObject;
    pub fn PyErr_Format(ty: *mut PyObject, fmt: *const c_char, ...) -> *mut PyObject;
    pub fn PyErr_FormatV(
        ty: *mut PyObject,
        fmt: *const c_char,
        va: *mut std::ffi::c_void,
    ) -> *mut PyObject;
    pub fn PyObject_CallFunction(callable: *mut PyObject, fmt: *const c_char, ...)
        -> *mut PyObject;
    pub fn PyObject_CallMethod(
        target: *mut PyObject,
        name: *const c_char,
        fmt: *const c_char,
        ...
    ) -> *mut PyObject;
    pub fn PyObject_CallMethodObjArgs(
        target: *mut PyObject,
        name: *mut PyObject,
        ...
    ) -> *mut PyObject;
    pub fn PyObject_CallFunctionObjArgs(callable: *mut PyObject, ...) -> *mut PyObject;
    pub fn PyTuple_Pack(n: isize, ...) -> *mut PyObject;
}

// ---------- helpers used by the shim's Py_BuildValue ----------

#[no_mangle]
pub unsafe extern "C" fn _WeavePy_Build_None() -> *mut PyObject {
    unsafe { crate::object::Py_IncRef(crate::singletons::none_ptr()) };
    crate::singletons::none_ptr()
}

#[no_mangle]
pub unsafe extern "C" fn _WeavePy_Build_FromI64(v: i64) -> *mut PyObject {
    crate::object::into_owned(Object::Int(v))
}

#[no_mangle]
pub unsafe extern "C" fn _WeavePy_Build_FromU64(v: u64) -> *mut PyObject {
    if v <= i64::MAX as u64 {
        crate::object::into_owned(Object::Int(v as i64))
    } else {
        crate::object::into_owned(Object::Long(weavepy_vm::sync::Rc::new(
            num_bigint::BigInt::from(v),
        )))
    }
}

#[no_mangle]
pub unsafe extern "C" fn _WeavePy_Build_FromDouble(v: f64) -> *mut PyObject {
    crate::object::into_owned(Object::Float(v))
}

#[no_mangle]
pub unsafe extern "C" fn _WeavePy_Build_FromString(s: *const c_char) -> *mut PyObject {
    if s.is_null() {
        unsafe { crate::object::Py_IncRef(crate::singletons::none_ptr()) };
        return crate::singletons::none_ptr();
    }
    unsafe { crate::strings::PyUnicode_FromString(s) }
}

#[no_mangle]
pub unsafe extern "C" fn _WeavePy_Build_FromStringAndSize(
    s: *const c_char,
    n: isize,
) -> *mut PyObject {
    unsafe { crate::strings::PyUnicode_FromStringAndSize(s, n) }
}

#[no_mangle]
pub unsafe extern "C" fn _WeavePy_Build_FromBytesAndSize(
    s: *const c_char,
    n: isize,
) -> *mut PyObject {
    unsafe { crate::strings::PyBytes_FromStringAndSize(s, n) }
}

#[no_mangle]
pub unsafe extern "C" fn _WeavePy_Build_TupleFromArray(
    n: isize,
    items: *const *mut PyObject,
) -> *mut PyObject {
    if n < 0 {
        return ptr::null_mut();
    }
    let mut out = Vec::with_capacity(n as usize);
    for i in 0..n {
        let p = unsafe { *items.add(i as usize) };
        let obj = if p.is_null() {
            Object::None
        } else {
            unsafe { crate::object::clone_object(p) }
        };
        if !p.is_null() {
            // The shim hands us references it has already
            // incremented; we balance them out here so the
            // resulting tuple ends with the canonical refcount.
            unsafe { crate::object::Py_DecRef(p) };
        }
        out.push(obj);
    }
    crate::object::into_owned(Object::new_tuple(out))
}

#[no_mangle]
pub unsafe extern "C" fn _WeavePy_Build_ListFromArray(
    n: isize,
    items: *const *mut PyObject,
) -> *mut PyObject {
    if n < 0 {
        return ptr::null_mut();
    }
    let mut out = Vec::with_capacity(n as usize);
    for i in 0..n {
        let p = unsafe { *items.add(i as usize) };
        let obj = if p.is_null() {
            Object::None
        } else {
            unsafe { crate::object::clone_object(p) }
        };
        if !p.is_null() {
            unsafe { crate::object::Py_DecRef(p) };
        }
        out.push(obj);
    }
    crate::object::into_owned(Object::new_list(out))
}

#[no_mangle]
pub unsafe extern "C" fn _WeavePy_Build_DictFromArrays(
    n: isize,
    keys: *const *mut PyObject,
    values: *const *mut PyObject,
) -> *mut PyObject {
    if n < 0 {
        return ptr::null_mut();
    }
    let dict = crate::object::into_owned(Object::new_dict());
    for i in 0..n {
        let k = unsafe { *keys.add(i as usize) };
        let v = unsafe { *values.add(i as usize) };
        unsafe {
            crate::containers::PyDict_SetItem(dict, k, v);
            crate::object::Py_DecRef(k);
            crate::object::Py_DecRef(v);
        }
    }
    dict
}

#[no_mangle]
pub unsafe extern "C" fn _WeavePy_Format_Set(ty: *mut PyObject, msg: *const c_char, len: isize) {
    let s = if msg.is_null() {
        String::new()
    } else {
        let bytes = unsafe { std::slice::from_raw_parts(msg as *const u8, len.max(0) as usize) };
        String::from_utf8_lossy(bytes).into_owned()
    };
    let cls = if ty.is_null() {
        Some(
            weavepy_vm::builtin_types::builtin_types()
                .runtime_error
                .clone(),
        )
    } else {
        match unsafe { crate::object::clone_object(ty) } {
            Object::Type(t) => Some(t),
            Object::Instance(inst) => Some(inst.cls()),
            _ => None,
        }
    };
    crate::errors::set_pending(cls, Object::from_str(s));
}

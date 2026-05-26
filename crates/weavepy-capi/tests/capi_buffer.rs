//! Unit tests for the C-API buffer protocol surface (RFC 0028) that
//! don't require a separate C extension to be built. We exercise:
//!
//! - `PyObject_GetBuffer` / `PyBuffer_Release` on `bytes`,
//!   `bytearray`, `memoryview` round-trip producers.
//! - `PyBuffer_FillInfo` / `PyBuffer_IsContiguous` /
//!   `PyBuffer_FillContiguousStrides` shape arithmetic.
//! - `PyBuffer_SizeFromFormat` for canonical `struct`-style formats.
//! - `PyMemoryView_FromMemory` / `PyMemoryView_FromObject` /
//!   `PyMemoryView_FromBuffer` constructors.
//! - `PyVectorcall_NARGS` / `PyObject_Vectorcall` fall-back path.

use std::os::raw::c_char;

use weavepy_capi::buffer::{
    PyBuffer_FillContiguousStrides, PyBuffer_FillInfo, PyBuffer_IsContiguous, PyBuffer_Release,
    PyBuffer_SizeFromFormat, PyObject_CheckBuffer, PyObject_GetBuffer, Py_buffer,
};
use weavepy_capi::memoryview::{
    PyMemoryView_Check, PyMemoryView_FromMemory, PyMemoryView_FromObject,
};
use weavepy_capi::object::{clone_object, into_owned, PyObject, PySsizeT, Py_DecRef};
use weavepy_capi::strings::{PyByteArray_FromStringAndSize, PyBytes_FromStringAndSize};
use weavepy_capi::vectorcall::{PyVectorcall_NARGS, PY_VECTORCALL_ARGUMENTS_OFFSET};
use weavepy_vm::object::Object;

fn init() {
    weavepy_capi::force_link();
}

#[test]
fn vectorcall_nargs_strips_offset_bit() {
    assert_eq!(unsafe { PyVectorcall_NARGS(3) }, 3);
    assert_eq!(
        unsafe { PyVectorcall_NARGS(3 | PY_VECTORCALL_ARGUMENTS_OFFSET) },
        3
    );
}

#[test]
fn buffer_size_from_format_resolves_known_codes() {
    init();
    let sized = |s: &str| {
        let cs = std::ffi::CString::new(s).unwrap();
        unsafe { PyBuffer_SizeFromFormat(cs.as_ptr()) }
    };
    assert_eq!(sized("B"), 1);
    assert_eq!(sized("h"), 2);
    assert_eq!(sized("d"), 8);
    assert_eq!(sized("3i"), 12);
}

#[test]
fn fill_contiguous_strides_round_trip() {
    init();
    let shape = [3 as PySsizeT, 4, 5];
    let mut strides = [0 as PySsizeT; 3];
    unsafe {
        PyBuffer_FillContiguousStrides(
            3,
            shape.as_ptr().cast_mut(),
            strides.as_mut_ptr(),
            8,
            b'C' as c_char,
        );
    }
    assert_eq!(strides[2], 8);
    assert_eq!(strides[1], 8 * 5);
    assert_eq!(strides[0], 8 * 5 * 4);
}

#[test]
fn buffer_check_works_for_native_exporters() {
    init();
    let data = b"hello world\0";
    let bytes = unsafe { PyBytes_FromStringAndSize(data.as_ptr().cast::<c_char>(), 11) };
    assert!(!bytes.is_null());
    assert_eq!(unsafe { PyObject_CheckBuffer(bytes) }, 1);
    unsafe { Py_DecRef(bytes) };

    let ba = unsafe { PyByteArray_FromStringAndSize(b"raw".as_ptr().cast::<c_char>(), 3) };
    assert!(!ba.is_null());
    assert_eq!(unsafe { PyObject_CheckBuffer(ba) }, 1);
    unsafe { Py_DecRef(ba) };
}

#[test]
fn get_buffer_round_trips_bytes() {
    init();
    let data = b"abcdef";
    let bytes = unsafe { PyBytes_FromStringAndSize(data.as_ptr().cast::<c_char>(), 6) };
    let mut view = Py_buffer::zeroed();
    let rc = unsafe { PyObject_GetBuffer(bytes, &raw mut view, 0) };
    assert_eq!(rc, 0);
    assert_eq!(view.len, 6);
    assert_eq!(view.itemsize, 1);
    assert_eq!(view.readonly, 1);
    let slice = unsafe { std::slice::from_raw_parts(view.buf as *const u8, view.len as usize) };
    assert_eq!(slice, b"abcdef");
    unsafe { PyBuffer_Release(&raw mut view) };
    unsafe { Py_DecRef(bytes) };
}

#[test]
fn fill_info_populates_view() {
    init();
    let mut buf = vec![0_u8; 16];
    let mut view = Py_buffer::zeroed();
    let rc = unsafe {
        PyBuffer_FillInfo(
            &raw mut view,
            std::ptr::null_mut(),
            buf.as_mut_ptr().cast::<std::ffi::c_void>(),
            16,
            0,
            0x0008, // PYBUF_ND
        )
    };
    assert_eq!(rc, 0);
    assert_eq!(view.len, 16);
    assert_eq!(view.ndim, 1);
    assert!(!view.shape.is_null());
    let s = unsafe { std::slice::from_raw_parts(view.shape, 1) };
    assert_eq!(s[0], 16);
    unsafe { PyBuffer_Release(&raw mut view) };
}

#[test]
fn is_contiguous_flat_buffer() {
    init();
    let shape: [PySsizeT; 1] = [16];
    let strides: [PySsizeT; 1] = [1];
    let view = Py_buffer {
        buf: std::ptr::null_mut(),
        obj: std::ptr::null_mut(),
        len: 16,
        itemsize: 1,
        readonly: 1,
        ndim: 1,
        format: std::ptr::null_mut(),
        shape: shape.as_ptr().cast_mut(),
        strides: strides.as_ptr().cast_mut(),
        suboffsets: std::ptr::null_mut(),
        internal: std::ptr::null_mut(),
    };
    assert_eq!(
        unsafe { PyBuffer_IsContiguous(&raw const view, b'C' as c_char) },
        1
    );
}

#[test]
fn memoryview_check_recognises_constructed_views() {
    init();
    let mut data = vec![1_u8, 2, 3, 4];
    let mv = unsafe {
        PyMemoryView_FromMemory(
            data.as_mut_ptr().cast::<c_char>(),
            4,
            0x100, /* PyBUF_READ */
        )
    };
    assert!(!mv.is_null());
    assert_eq!(unsafe { PyMemoryView_Check(mv) }, 1);
    let obj = unsafe { clone_object(mv) };
    matches!(obj, Object::MemoryView(_));
    unsafe { Py_DecRef(mv) };
}

#[test]
fn memoryview_from_object_wraps_bytes() {
    init();
    let bytes = unsafe { PyBytes_FromStringAndSize(b"hello".as_ptr().cast::<c_char>(), 5) };
    let mv = unsafe { PyMemoryView_FromObject(bytes) };
    assert!(!mv.is_null());
    assert_eq!(unsafe { PyMemoryView_Check(mv) }, 1);
    unsafe { Py_DecRef(mv) };
    unsafe { Py_DecRef(bytes) };
}

#[test]
fn vectorcall_falls_back_for_plain_builtin() {
    init();
    // We test the fallback path: an Object::Builtin doesn't carry a
    // tp_vectorcall slot, so PyObject_Vectorcall should route through
    // PyObject_Call.
    let identity = Object::Builtin(weavepy_vm::sync::Rc::new(
        weavepy_vm::object::BuiltinFn::new("identity", |args| {
            args.first()
                .cloned()
                .ok_or_else(|| weavepy_vm::error::type_error("identity expects 1 arg"))
        }),
    ));
    let p_callable = into_owned(identity);
    let arg = into_owned(Object::Int(42));
    let args_arr: [*mut PyObject; 1] = [arg];
    let result = unsafe {
        weavepy_capi::vectorcall::PyObject_Vectorcall(
            p_callable,
            args_arr.as_ptr(),
            1,
            std::ptr::null_mut(),
        )
    };
    assert!(!result.is_null());
    let out = unsafe { clone_object(result) };
    assert!(matches!(out, Object::Int(42)));
    unsafe { Py_DecRef(result) };
    unsafe { Py_DecRef(arg) };
    unsafe { Py_DecRef(p_callable) };
}

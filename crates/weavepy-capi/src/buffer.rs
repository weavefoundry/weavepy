//! Tiny `Py_buffer` surface — enough that extensions calling
//! `PyObject_GetBuffer` against bytes/bytearray succeed.
//!
//! Numerical extensions like NumPy will need a much richer buffer
//! protocol (multi-dim shapes, strides, format codes); RFC 0023
//! tracks that follow-up.

use std::os::raw::{c_char, c_int};
use std::ptr;

use weavepy_vm::object::Object;

use crate::object::{PyObject, PySsizeT};

#[repr(C)]
pub struct Py_buffer {
    pub buf: *mut std::ffi::c_void,
    pub obj: *mut PyObject,
    pub len: PySsizeT,
    pub itemsize: PySsizeT,
    pub readonly: c_int,
    pub ndim: c_int,
    pub format: *mut c_char,
    pub shape: *mut PySsizeT,
    pub strides: *mut PySsizeT,
    pub suboffsets: *mut PySsizeT,
    pub internal: *mut std::ffi::c_void,
}

#[no_mangle]
pub unsafe extern "C" fn PyObject_GetBuffer(
    exporter: *mut PyObject,
    view: *mut Py_buffer,
    _flags: c_int,
) -> c_int {
    if view.is_null() || exporter.is_null() {
        crate::errors::set_type_error("PyObject_GetBuffer: NULL argument");
        return -1;
    }
    let obj = unsafe { crate::object::clone_object(exporter) };
    let (buf, len, readonly) = match obj {
        Object::Bytes(b) => {
            // Leak a fresh copy so the buffer pointer stays valid
            // across the call (the caller must Py_DECREF the view's
            // `obj` to release).
            let copy: Vec<u8> = b.to_vec();
            let len = copy.len();
            let boxed: Box<[u8]> = copy.into_boxed_slice();
            let ptr = Box::into_raw(boxed);
            (ptr as *mut std::ffi::c_void, len as PySsizeT, 1)
        }
        Object::ByteArray(rc) => {
            let mut copy = rc.borrow().clone();
            let len = copy.len();
            let p = copy.as_mut_ptr();
            std::mem::forget(copy);
            (p as *mut std::ffi::c_void, len as PySsizeT, 0)
        }
        _ => {
            crate::errors::set_type_error("buffer protocol not supported");
            return -1;
        }
    };
    unsafe {
        (*view).buf = buf;
        (*view).obj = exporter;
        (*view).len = len;
        (*view).itemsize = 1;
        (*view).readonly = readonly;
        (*view).ndim = 1;
        (*view).format = b"B\0".as_ptr() as *mut c_char;
        (*view).shape = ptr::null_mut();
        (*view).strides = ptr::null_mut();
        (*view).suboffsets = ptr::null_mut();
        (*view).internal = ptr::null_mut();
        crate::object::Py_IncRef(exporter);
    }
    0
}

#[no_mangle]
pub unsafe extern "C" fn PyBuffer_Release(view: *mut Py_buffer) {
    if view.is_null() {
        return;
    }
    let v = unsafe { &mut *view };
    if !v.obj.is_null() {
        unsafe { crate::object::Py_DecRef(v.obj) };
    }
    if !v.buf.is_null() {
        // We don't own the buffer when ndim==0 (in-place borrow);
        // for the foundation we always allocated a copy in
        // GetBuffer, so freeing here is safe. Use the same allocator
        // used in GetBuffer (Box<[u8]>::into_raw).
        let layout = std::alloc::Layout::from_size_align(v.len.max(1) as usize, 1).expect("layout");
        unsafe { std::alloc::dealloc(v.buf as *mut u8, layout) };
    }
    *v = Py_buffer {
        buf: ptr::null_mut(),
        obj: ptr::null_mut(),
        len: 0,
        itemsize: 0,
        readonly: 0,
        ndim: 0,
        format: ptr::null_mut(),
        shape: ptr::null_mut(),
        strides: ptr::null_mut(),
        suboffsets: ptr::null_mut(),
        internal: ptr::null_mut(),
    };
}

#[no_mangle]
pub unsafe extern "C" fn PyObject_CheckBuffer(o: *mut PyObject) -> c_int {
    if o.is_null() {
        return 0;
    }
    matches!(
        unsafe { crate::object::clone_object(o) },
        Object::Bytes(_) | Object::ByteArray(_)
    )
    .into()
}

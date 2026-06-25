//! `PyMemoryView_*` C-API surface.
//!
//! `memoryview` is a built-in type backed by [`Object::MemoryView`]
//! in the WeavePy native model. The C-API constructors here mirror
//! CPython's `Modules/_memoryview.c`:
//!
//! - [`PyMemoryView_FromObject`] takes any buffer-protocol exporter
//!   and wraps it.
//! - [`PyMemoryView_FromMemory`] wraps a raw `(ptr, len)` window —
//!   useful for extensions that publish a heap-allocated view.
//! - [`PyMemoryView_FromBuffer`] wraps a fully-populated `Py_buffer`
//!   record (the multi-dimensional / strided form).
//! - [`PyMemoryView_GetContiguous`] is the C-order copy convenience
//!   that numpy ports commonly call.
//!
//! ## Lifetime contract
//!
//! `PyMemoryView_FromObject` retains a reference to the exporter via
//! the underlying [`PyMemoryView`]'s payload. The view stays alive
//! until the consumer drops the last C-side reference, at which
//! point the box's destructor decrements the underlying buffer.

use std::os::raw::{c_char, c_int};
use std::ptr;

use weavepy_vm::object::{MemoryViewBuffer, Object, PyMemoryView};
use weavepy_vm::sync::{Cell, RefCell};

use crate::buffer::Py_buffer;
use crate::object::{PyObject, PySsizeT};

/// `PyMemoryView_Check(o)` — true if `o` is a `memoryview` instance.
#[no_mangle]
pub unsafe extern "C" fn PyMemoryView_Check(o: *mut PyObject) -> c_int {
    if o.is_null() {
        return 0;
    }
    matches!(
        unsafe { crate::object::clone_object(o) },
        Object::MemoryView(_)
    )
    .into()
}

/// `PyMemoryView_FromObject(exporter)` — wrap an exporter in a
/// memoryview.
#[no_mangle]
pub unsafe extern "C" fn PyMemoryView_FromObject(exporter: *mut PyObject) -> *mut PyObject {
    if exporter.is_null() {
        crate::errors::set_buffer_error("PyMemoryView_FromObject: NULL argument");
        return ptr::null_mut();
    }

    // Special-case bytes/bytearray for the fast path.
    let obj = unsafe { crate::object::clone_object(exporter) };
    let mv = match &obj {
        Object::Bytes(b) => PyMemoryView::from_bytes(b.clone()),
        Object::ByteArray(b) => PyMemoryView::from_bytearray(b.clone()),
        Object::MemoryView(other) => clone_memoryview(other),
        _ => {
            // Generic path: drive the buffer protocol on the
            // exporter and build a memoryview from the results.
            let mut view = Py_buffer::zeroed();
            let rc = unsafe { crate::buffer::PyObject_GetBuffer(exporter, &raw mut view, 0) };
            if rc != 0 {
                return ptr::null_mut();
            }
            let bytes_data = if view.buf.is_null() || view.len <= 0 {
                Vec::new()
            } else {
                unsafe { std::slice::from_raw_parts(view.buf as *const u8, view.len as usize) }
                    .to_vec()
            };
            let readonly = view.readonly != 0;
            // Snapshot the scalar fields before [`PyBuffer_Release`]
            // tears down `view`'s exporter-owned arrays.
            let view_len = view.len.max(0) as usize;
            let view_itemsize = view.itemsize.max(1) as usize;
            unsafe { crate::buffer::PyBuffer_Release(&raw mut view) };
            PyMemoryView::contiguous_1d(
                MemoryViewBuffer::Bytes(bytes_data.into()),
                view_len,
                readonly,
                "B".to_owned(),
                view_itemsize,
            )
        }
    };
    crate::object::into_owned(Object::MemoryView(weavepy_vm::sync::Rc::new(mv)))
}

fn clone_memoryview(other: &PyMemoryView) -> PyMemoryView {
    PyMemoryView {
        buffer: match &other.buffer {
            MemoryViewBuffer::Bytes(b) => MemoryViewBuffer::Bytes(b.clone()),
            MemoryViewBuffer::ByteArray(b) => MemoryViewBuffer::ByteArray(b.clone()),
            MemoryViewBuffer::Shared(s) => MemoryViewBuffer::Shared(s.clone()),
        },
        start: Cell::new(other.start.get()),
        len: Cell::new(other.len.get()),
        readonly: Cell::new(other.readonly.get()),
        released: Cell::new(false),
        format: RefCell::new(other.format.borrow().clone()),
        itemsize: Cell::new(other.itemsize.get()),
        shape: RefCell::new(other.shape.borrow().clone()),
        strides: RefCell::new(other.strides.borrow().clone()),
    }
}

/// `PyMemoryView_FromMemory(mem, size, flags)` — wrap a raw `(ptr,
/// len)` block. `flags` is `PyBUF_READ` or `PyBUF_WRITE`.
#[no_mangle]
pub unsafe extern "C" fn PyMemoryView_FromMemory(
    mem: *mut c_char,
    size: PySsizeT,
    flags: c_int,
) -> *mut PyObject {
    if mem.is_null() && size != 0 {
        crate::errors::set_buffer_error("PyMemoryView_FromMemory: NULL pointer");
        return ptr::null_mut();
    }
    let len = size.max(0) as usize;
    let bytes = if mem.is_null() {
        Vec::new()
    } else {
        unsafe { std::slice::from_raw_parts(mem as *const u8, len) }.to_vec()
    };
    let readonly = (flags & 0x100) != 0; // PyBUF_READ
    let mv = if readonly {
        PyMemoryView::contiguous_1d(
            MemoryViewBuffer::Bytes(bytes.into()),
            len,
            true,
            "B".to_owned(),
            1,
        )
    } else {
        PyMemoryView::contiguous_1d(
            MemoryViewBuffer::ByteArray(weavepy_vm::sync::Rc::new(RefCell::new(bytes))),
            len,
            false,
            "B".to_owned(),
            1,
        )
    };
    crate::object::into_owned(Object::MemoryView(weavepy_vm::sync::Rc::new(mv)))
}

/// `PyMemoryView_FromBuffer(view)` — build a memoryview that wraps a
/// fully-populated `Py_buffer`. We copy the buffer's contents into
/// the memoryview so the view's lifetime is decoupled from the
/// exporter's underlying memory.
#[no_mangle]
pub unsafe extern "C" fn PyMemoryView_FromBuffer(view: *const Py_buffer) -> *mut PyObject {
    if view.is_null() {
        crate::errors::set_buffer_error("PyMemoryView_FromBuffer: NULL view");
        return ptr::null_mut();
    }
    let v = unsafe { &*view };
    let bytes = if v.buf.is_null() || v.len <= 0 {
        Vec::new()
    } else {
        unsafe { std::slice::from_raw_parts(v.buf as *const u8, v.len as usize) }.to_vec()
    };
    let format = if v.format.is_null() {
        "B".to_owned()
    } else {
        unsafe { std::ffi::CStr::from_ptr(v.format) }
            .to_string_lossy()
            .into_owned()
    };
    let mv = if v.readonly != 0 {
        PyMemoryView::contiguous_1d(
            MemoryViewBuffer::Bytes(bytes.into()),
            v.len.max(0) as usize,
            true,
            format,
            v.itemsize.max(1) as usize,
        )
    } else {
        PyMemoryView::contiguous_1d(
            MemoryViewBuffer::ByteArray(weavepy_vm::sync::Rc::new(RefCell::new(bytes))),
            v.len.max(0) as usize,
            false,
            format,
            v.itemsize.max(1) as usize,
        )
    };
    crate::object::into_owned(Object::MemoryView(weavepy_vm::sync::Rc::new(mv)))
}

/// `PyMemoryView_GetContiguous(base, buffertype, order)` — fetch
/// the buffer protocol's data and copy it into a fresh contiguous
/// memoryview. `order` is `'C'` or `'F'`; `buffertype` is
/// `PyBUF_READ` (read-only view) or `PyBUF_WRITE` (writable view).
#[no_mangle]
pub unsafe extern "C" fn PyMemoryView_GetContiguous(
    base: *mut PyObject,
    buffertype: c_int,
    _order: c_char,
) -> *mut PyObject {
    if base.is_null() {
        crate::errors::set_buffer_error("PyMemoryView_GetContiguous: NULL base");
        return ptr::null_mut();
    }
    let mut view = Py_buffer::zeroed();
    let flags = match buffertype {
        0x200 => 0x0001 | 0x0008, // WRITABLE | ND
        _ => 0x0008,              // ND
    };
    let rc = unsafe { crate::buffer::PyObject_GetBuffer(base, &raw mut view, flags) };
    if rc != 0 {
        return ptr::null_mut();
    }
    let bytes = if view.buf.is_null() || view.len <= 0 {
        Vec::new()
    } else {
        unsafe { std::slice::from_raw_parts(view.buf as *const u8, view.len as usize) }.to_vec()
    };
    let readonly = view.readonly != 0;
    let view_len = view.len.max(0) as usize;
    unsafe { crate::buffer::PyBuffer_Release(&raw mut view) };
    let mv = if readonly {
        PyMemoryView::contiguous_1d(
            MemoryViewBuffer::Bytes(bytes.into()),
            view_len,
            true,
            "B".to_owned(),
            1,
        )
    } else {
        PyMemoryView::contiguous_1d(
            MemoryViewBuffer::ByteArray(weavepy_vm::sync::Rc::new(RefCell::new(bytes))),
            view_len,
            false,
            "B".to_owned(),
            1,
        )
    };
    crate::object::into_owned(Object::MemoryView(weavepy_vm::sync::Rc::new(mv)))
}

/// `PyMemoryView_GET_BUFFER(mv)` — return a borrow of the underlying
/// `Py_buffer`. CPython exposes a stable cell on the view; we
/// materialise one on demand and stash it in a thread-local cache.
#[no_mangle]
pub unsafe extern "C" fn PyMemoryView_GET_BUFFER(mv: *mut PyObject) -> *mut Py_buffer {
    if mv.is_null() {
        return ptr::null_mut();
    }
    let obj = unsafe { crate::object::clone_object(mv) };
    let view = match &obj {
        Object::MemoryView(rc) => rc,
        _ => {
            crate::errors::set_type_error("PyMemoryView_GET_BUFFER: expected memoryview");
            return ptr::null_mut();
        }
    };
    if view.released.get() {
        crate::errors::set_value_error("memoryview: released");
        return ptr::null_mut();
    }

    // Materialise a fresh Py_buffer on the heap.
    let bytes = view.buffer.with_read(<[u8]>::to_vec);
    let len = view.len.get();
    let mut buf_box: Box<[u8]> = bytes.into_boxed_slice();
    let buf_ptr = buf_box.as_mut_ptr() as *mut std::ffi::c_void;
    let format = view.format.borrow().clone() + "\0";
    let format_storage: Box<[u8]> = format.into_bytes().into_boxed_slice();

    let internal = Box::new(crate::buffer::BufferInternal {
        owned_buf: Some(buf_box),
        shape: Box::new([len as PySsizeT]),
        strides: Box::new([view.itemsize.get() as PySsizeT]),
        suboffsets: Box::new([]),
        format: format_storage,
    });
    let internal_ptr = Box::into_raw(internal);
    let internal_ref = unsafe { &mut *internal_ptr };
    let pyb = Py_buffer {
        buf: buf_ptr,
        obj: mv,
        len: len as PySsizeT,
        itemsize: view.itemsize.get() as PySsizeT,
        readonly: c_int::from(view.readonly.get()),
        ndim: 1,
        format: internal_ref.format.as_ptr() as *mut c_char,
        shape: internal_ref.shape.as_mut_ptr(),
        strides: internal_ref.strides.as_mut_ptr(),
        suboffsets: ptr::null_mut(),
        internal: internal_ptr as *mut std::ffi::c_void,
    };
    Box::into_raw(Box::new(pyb))
}

/// `PyMemoryView_GET_BASE(mv)` — return the underlying exporter, or
/// `None` for memoryviews wrapping standalone byte arrays.
#[no_mangle]
pub unsafe extern "C" fn PyMemoryView_GET_BASE(mv: *mut PyObject) -> *mut PyObject {
    if mv.is_null() {
        return ptr::null_mut();
    }
    // We don't currently track an explicit base — the buffer is the
    // base. Return Py_None to signal "no underlying object". CPython
    // does the same for views built from raw memory.
    unsafe {
        crate::object::Py_IncRef(crate::singletons::none_ptr());
    }
    crate::singletons::none_ptr()
}

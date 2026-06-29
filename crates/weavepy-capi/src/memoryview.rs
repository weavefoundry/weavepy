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
            // Generic path: drive the buffer protocol on the exporter and
            // build a faithful memoryview from the results. Request the
            // full read-only buffer (format + ndim + strides) exactly like
            // CPython's `PyMemoryView_FromObject`, so an exporter such as
            // numpy reports its real `format`/`itemsize` — `'O'`/8 for an
            // `dtype=object` array, `'l'`/8 for `int64`, etc. A bare
            // `PyBUF_SIMPLE` (flags=0) request loses the format string and
            // collapses every view to bytes, which breaks Cython
            // fused-type dispatch: `map_fused_type` resolves `ndarray[object]`
            // only when `memoryview(arr)` reports `itemsize == sizeof(void*)`
            // and a parseable `'O'` format (pandas' `lib.map_infer_mask`).
            const PYBUF_FULL_RO: c_int = 0x011C; // INDIRECT | STRIDES | ND | FORMAT
            let mut view = Py_buffer::zeroed();
            let rc =
                unsafe { crate::buffer::PyObject_GetBuffer(exporter, &raw mut view, PYBUF_FULL_RO) };
            if rc != 0 {
                return ptr::null_mut();
            }
            let readonly = view.readonly != 0;
            // Snapshot the scalar fields before [`PyBuffer_Release`]
            // tears down `view`'s exporter-owned arrays.
            let view_len = view.len.max(0) as usize;
            let view_itemsize = view.itemsize.max(1) as usize;
            let format = if view.format.is_null() {
                "B".to_owned()
            } else {
                unsafe { core::ffi::CStr::from_ptr(view.format) }
                    .to_string_lossy()
                    .into_owned()
            };

            // Capture the exporter's multi-dimensional geometry before
            // `PyBuffer_Release` frees the exporter-owned shape/stride
            // arrays. CPython's memoryview references the exporter's memory
            // and keeps its `ndim`/`shape`/`strides`; WeavePy copies the
            // bytes, so we snapshot the geometry and re-materialise the data
            // in C order to stay self-consistent. Collapsing every view to
            // 1-D (the old behaviour) breaks Cython typed-memoryview
            // dispatch, which validates `buf.ndim` — e.g. pandas'
            // `group_last(int64_t[:, ::1] out, ndarray[int64_t, ndim=2]
            // values, ...)` rejects a flattened `values` with "Buffer has
            // wrong number of dimensions (expected 2, got 1)".
            let ndim = view.ndim.max(0) as usize;
            let shape: Vec<usize> = if ndim >= 1 && !view.shape.is_null() {
                (0..ndim)
                    .map(|i| unsafe { *view.shape.add(i) }.max(0) as usize)
                    .collect()
            } else {
                Vec::new()
            };
            let strides: Vec<isize> = if ndim >= 1 && !view.strides.is_null() {
                (0..ndim)
                    .map(|i| unsafe { *view.strides.add(i) } as isize)
                    .collect()
            } else {
                Vec::new()
            };

            // Materialise the bytes in C-contiguous order. A C-contiguous
            // source (numpy's usual case, and any array whose only gaps come
            // from size-1 axes) is a straight `view.len`-byte copy; a
            // strided/transposed/reversed source is gathered element by
            // element following `strides`, matching `memoryview.tobytes()`.
            let bytes_data = if view.buf.is_null() || view_len == 0 {
                Vec::new()
            } else if shape.len() >= 2 && !is_c_contiguous_dims(&shape, &strides, view_itemsize) {
                gather_c_order(view.buf as *const u8, &shape, &strides, view_itemsize)
            } else {
                unsafe { std::slice::from_raw_parts(view.buf as *const u8, view_len) }.to_vec()
            };

            unsafe { crate::buffer::PyBuffer_Release(&raw mut view) };
            if std::env::var_os("WEAVEPY_TRACE_BUF").is_some() {
                eprintln!(
                    "[WEAVEPY_TRACE_BUF]   FromObject built mv format={format:?} itemsize={view_itemsize} ndim={ndim} len={view_len}"
                );
            }
            let built = PyMemoryView::contiguous_1d(
                MemoryViewBuffer::Bytes(bytes_data.into()),
                view_len,
                readonly,
                format,
                view_itemsize,
            );
            // Store the ≥2-D shape so `ndim`/`shape` survive the copy; leave
            // `strides` empty so the VM derives C-contiguous strides that
            // match the C-order bytes we just materialised. (A 1-D view's
            // derived `[len / itemsize]` shape already matches, so there is
            // nothing extra to record.)
            if shape.len() >= 2 {
                *built.shape.borrow_mut() = shape;
            }
            built
        }
    };
    crate::object::into_owned(Object::MemoryView(weavepy_vm::sync::Rc::new(mv)))
}

/// C-contiguity test that mirrors numpy's: axes of length 0 or 1 impose
/// no layout constraint, so an `(n, 1)` view (a transposed row, common in
/// pandas' `_call_cython_op`) still counts as contiguous and takes the
/// fast linear-copy path.
fn is_c_contiguous_dims(shape: &[usize], strides: &[isize], itemsize: usize) -> bool {
    if strides.is_empty() {
        return true;
    }
    let mut expected = itemsize as isize;
    for i in (0..shape.len()).rev() {
        if shape[i] > 1 && strides[i] != expected {
            return false;
        }
        expected *= shape[i] as isize;
    }
    true
}

/// Gather a strided buffer into a fresh C-order byte vector, walking the
/// multi-index last-axis-fastest exactly like `memoryview.tobytes()`.
/// Handles negative strides (`arr[::-1]`), where `buf` points at the first
/// logical element and offsets run backwards through the allocation.
fn gather_c_order(buf: *const u8, shape: &[usize], strides: &[isize], itemsize: usize) -> Vec<u8> {
    let total: usize = shape.iter().product();
    let mut out = Vec::with_capacity(total * itemsize);
    if total == 0 {
        return out;
    }
    let ndim = shape.len();
    let mut index = vec![0usize; ndim];
    for _ in 0..total {
        let mut off: isize = 0;
        for d in 0..ndim {
            off += index[d] as isize * strides[d];
        }
        let src = unsafe { buf.offset(off) };
        out.extend_from_slice(unsafe { std::slice::from_raw_parts(src, itemsize) });
        for d in (0..ndim).rev() {
            index[d] += 1;
            if index[d] < shape[d] {
                break;
            }
            index[d] = 0;
        }
    }
    out
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
    let itemsize = view.itemsize.get().max(1);

    // Element shape/stride: honour an explicit shape, else derive a 1-D
    // `[len / itemsize]` C-contiguous layout so `shape`/`itemsize`/`len`
    // stay self-consistent (`shape[0] == len / itemsize`, not `len`).
    let stored_shape = view.shape.borrow();
    let (shape_box, strides_box): (Box<[PySsizeT]>, Box<[PySsizeT]>) = if stored_shape.is_empty() {
        let n = if itemsize > 0 { len / itemsize } else { 0 };
        (
            Box::new([n as PySsizeT]),
            Box::new([itemsize as PySsizeT]),
        )
    } else {
        let shape: Vec<PySsizeT> = stored_shape.iter().map(|&s| s as PySsizeT).collect();
        let stored_strides = view.strides.borrow();
        let strides: Vec<PySsizeT> = if stored_strides.is_empty() {
            let mut st = vec![0 as PySsizeT; shape.len()];
            let mut acc = itemsize as PySsizeT;
            for i in (0..shape.len()).rev() {
                st[i] = acc;
                acc *= shape[i];
            }
            st
        } else {
            stored_strides.iter().map(|&s| s as PySsizeT).collect()
        };
        (shape.into_boxed_slice(), strides.into_boxed_slice())
    };
    let ndim = shape_box.len() as c_int;

    let internal = Box::new(crate::buffer::BufferInternal {
        owned_buf: Some(buf_box),
        shape: shape_box,
        strides: strides_box,
        suboffsets: Box::new([]),
        format: format_storage,
    });
    let internal_ptr = Box::into_raw(internal);
    let internal_ref = unsafe { &mut *internal_ptr };
    if std::env::var_os("WEAVEPY_TRACE_BUF").is_some() {
        eprintln!(
            "[WEAVEPY_TRACE_BUF]   GET_BUFFER mv format={:?} itemsize={itemsize} ndim={ndim} len={len}",
            view.format.borrow()
        );
    }
    let pyb = Py_buffer {
        buf: buf_ptr,
        obj: mv,
        len: len as PySsizeT,
        itemsize: itemsize as PySsizeT,
        readonly: c_int::from(view.readonly.get()),
        ndim,
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

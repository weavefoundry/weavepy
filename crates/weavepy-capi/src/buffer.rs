//! Full PEP 3118 buffer protocol surface.
//!
//! The buffer protocol is the C-level lingua franca that lets data
//! producers (`bytes`, `bytearray`, `array.array`, the new `_ndarray`
//! fixture, third-party numpy) hand a typed pointer to data
//! consumers (anything that accepts a `bytes-like` argument plus
//! NumPy itself). RFC 0028 lifts WeavePy from the byte-array minimum
//! to the multi-dimensional, struct-typed surface CPython 3.13
//! exposes through `Py_LIMITED_API`.
//!
//! ## Lifetime contract
//!
//! - The exporter populates `Py_buffer` with pointers to memory it
//!   owns. Multi-dimensional buffers carry separate `shape` /
//!   `strides` / `suboffsets` arrays whose lifetime is tied to the
//!   exporter call.
//! - Consumers must call [`PyBuffer_Release`] when they're done.
//!   The release path consults the `internal` pointer (which the
//!   exporter populated with a [`BufferInternal`] block) and frees
//!   the temporary allocations the exporter handed out.
//! - Refcount discipline: `PyObject_GetBuffer` increments the
//!   exporter's refcount; `PyBuffer_Release` drops it. CPython
//!   leaves this contract to the exporter; we centralise it here so
//!   the byte-array native exporter and the user-defined extension
//!   path both get it right.
//!
//! ## Dispatch
//!
//! [`PyObject_GetBuffer`] consults the exporter's type for a
//! [`Py_bf_getbuffer`](crate::slottable::Py_bf_getbuffer) slot. If
//! present, the slot owns the buffer-fill responsibilities and
//! [`PyBuffer_Release`] forwards to its
//! [`Py_bf_releasebuffer`](crate::slottable::Py_bf_releasebuffer)
//! counterpart (when defined). Otherwise we fall back to a native
//! exporter that handles the bytes-like built-ins.

use std::os::raw::{c_char, c_int};
use std::ptr;

use weavepy_vm::object::{MemoryViewBuffer, Object};

use crate::buffer_format::{format_string_for, ByteOrder, FormatKind};
use crate::object::{PyObject, PySsizeT};
use crate::slottable::{slot_table_for, Py_bf_getbuffer, Py_bf_releasebuffer};

/// Layout of `Py_buffer` in `Python.h`. Field order matches CPython
/// 3.13 exactly.
#[repr(C)]
#[derive(Debug)]
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

impl Py_buffer {
    /// Initialise an all-null buffer view; used by extension code
    /// that wants to ensure later releases don't double-free.
    pub fn zeroed() -> Self {
        Self {
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
        }
    }
}

/// Per-view bookkeeping the WeavePy native exporter stashes in
/// `Py_buffer::internal`. [`PyBuffer_Release`] reads it back via
/// [`Box::from_raw`] when the view is released.
///
/// User-defined exporters supply their own internal state; the
/// shape of `internal` is opaque to consumers and the extension
/// code is responsible for matching alloc/free.
#[derive(Debug)]
pub(crate) struct BufferInternal {
    /// Heap-allocated copy of the source data. We make a defensive
    /// copy so the consumer can hold the buffer across
    /// allocator events without invalidation.
    pub owned_buf: Option<Box<[u8]>>,
    pub shape: Box<[PySsizeT]>,
    pub strides: Box<[PySsizeT]>,
    pub suboffsets: Box<[PySsizeT]>,
    pub format: Box<[u8]>,
}

// ----------------------------------------------------------------
// PyObject_GetBuffer / PyBuffer_Release
// ----------------------------------------------------------------

/// Major flag bit values mirrored from `Python.h`.
const PYBUF_WRITABLE: c_int = 0x0001;
const PYBUF_FORMAT: c_int = 0x0004;
const PYBUF_ND: c_int = 0x0008;
const PYBUF_STRIDES: c_int = 0x0010 | PYBUF_ND;
const PYBUF_C_CONTIGUOUS: c_int = 0x0020 | PYBUF_STRIDES;
const PYBUF_F_CONTIGUOUS: c_int = 0x0040 | PYBUF_STRIDES;
const PYBUF_ANY_CONTIGUOUS: c_int = 0x0080 | PYBUF_STRIDES;
const PYBUF_INDIRECT: c_int = 0x0100 | PYBUF_STRIDES;

/// `PyObject_GetBuffer(exporter, view, flags)` — entry point for
/// consumers. Returns 0 on success, -1 on error (with a pending
/// exception installed).
#[no_mangle]
pub unsafe extern "C" fn PyObject_GetBuffer(
    exporter: *mut PyObject,
    view: *mut Py_buffer,
    flags: c_int,
) -> c_int {
    if view.is_null() || exporter.is_null() {
        crate::errors::set_buffer_error("PyObject_GetBuffer: NULL argument");
        return -1;
    }
    unsafe { *view = Py_buffer::zeroed() };

    // 1) Heap-type slot dispatch.
    let head = unsafe { &*exporter };
    if let Some(slot_table) = unsafe { slot_table_for(head.ob_type) } {
        let slot = slot_table.get(Py_bf_getbuffer);
        if !slot.is_null() {
            let getbuf: unsafe extern "C" fn(*mut PyObject, *mut Py_buffer, c_int) -> c_int =
                unsafe { slot.cast() };
            return unsafe { getbuf(exporter, view, flags) };
        }
    }

    // 2) Native fallback for built-in bytes-like types.
    let obj = unsafe { crate::object::clone_object(exporter) };
    fill_native_buffer(exporter, &obj, view, flags)
}

/// `PyBuffer_Release(view)` — release the resources backing `view`.
///
/// CPython's contract: a release for a `Py_buffer` whose `obj` slot
/// is null is a no-op; releases for views obtained from a heap-type
/// exporter forward to the type's `bf_releasebuffer` slot if any.
#[no_mangle]
pub unsafe extern "C" fn PyBuffer_Release(view: *mut Py_buffer) {
    if view.is_null() {
        return;
    }
    let v = unsafe { &mut *view };
    let exporter = v.obj;
    if exporter.is_null() {
        return;
    }

    // 1) Heap-type slot dispatch.
    let head = unsafe { &*exporter };
    if let Some(slot_table) = unsafe { slot_table_for(head.ob_type) } {
        let slot = slot_table.get(Py_bf_releasebuffer);
        if !slot.is_null() {
            let release: unsafe extern "C" fn(*mut PyObject, *mut Py_buffer) =
                unsafe { slot.cast() };
            unsafe { release(exporter, view) };
            // Drop the exporter ref the loader installed during get.
            unsafe { crate::object::Py_DecRef(exporter) };
            *v = Py_buffer::zeroed();
            return;
        }
    }

    // 2) Native release path. We allocated a `BufferInternal` on
    // the heap during `fill_native_buffer`; reclaim it now.
    if !v.internal.is_null() {
        let _ = unsafe { Box::from_raw(v.internal as *mut BufferInternal) };
    }
    if !exporter.is_null() {
        unsafe { crate::object::Py_DecRef(exporter) };
    }
    *v = Py_buffer::zeroed();
}

/// `PyObject_CheckBuffer(o)` — true if `o` exports the buffer
/// protocol. Both heap-type slots and built-in bytes-likes count.
#[no_mangle]
pub unsafe extern "C" fn PyObject_CheckBuffer(o: *mut PyObject) -> c_int {
    if o.is_null() {
        return 0;
    }
    let head = unsafe { &*o };
    if let Some(slot_table) = unsafe { slot_table_for(head.ob_type) } {
        if !slot_table.get(Py_bf_getbuffer).is_null() {
            return 1;
        }
    }
    matches!(
        unsafe { crate::object::clone_object(o) },
        Object::Bytes(_) | Object::ByteArray(_) | Object::MemoryView(_)
    )
    .into()
}

// ----------------------------------------------------------------
// Native fallback — handles bytes / bytearray / memoryview.
// ----------------------------------------------------------------

fn fill_native_buffer(
    exporter: *mut PyObject,
    obj: &Object,
    view: *mut Py_buffer,
    flags: c_int,
) -> c_int {
    let (data, len, readonly) = match obj {
        Object::Bytes(b) => (b.to_vec(), b.len(), 1),
        Object::ByteArray(rc) => {
            let data = rc.borrow().clone();
            let len = data.len();
            (data, len, 0)
        }
        Object::MemoryView(mv) => {
            if mv.released.get() {
                crate::errors::set_value_error("memoryview: released");
                return -1;
            }
            let bytes = match &mv.buffer {
                MemoryViewBuffer::Bytes(b) => b.to_vec(),
                MemoryViewBuffer::ByteArray(b) => b.borrow().clone(),
            };
            let len = mv.len.get();
            let start = mv.start.get();
            let slice = bytes[start..start + len].to_vec();
            (slice, len, c_int::from(mv.readonly.get()))
        }
        _ => {
            crate::errors::set_buffer_error(format!(
                "a bytes-like object is required, not '{}'",
                type_name(obj)
            ));
            return -1;
        }
    };

    if (flags & PYBUF_WRITABLE) != 0 && readonly != 0 {
        crate::errors::set_buffer_error("Object is not writable");
        return -1;
    }

    let mut owned: Box<[u8]> = data.into_boxed_slice();
    let buf_ptr = owned.as_mut_ptr() as *mut std::ffi::c_void;

    let format_bytes = format_string_for(FormatKind::UInt8, ByteOrder::Native);
    let format_storage: Box<[u8]> = format_bytes.into_boxed_slice();

    let want_shape = (flags & PYBUF_ND) == PYBUF_ND;
    let want_strides = (flags & 0x0010) != 0;
    let shape_box: Box<[PySsizeT]> = if want_shape {
        Box::new([len as PySsizeT])
    } else {
        Box::new([])
    };
    let strides_box: Box<[PySsizeT]> = if want_strides {
        Box::new([1])
    } else {
        Box::new([])
    };
    let suboffsets_box: Box<[PySsizeT]> = Box::new([]);

    // Heap up the internal block — the release path relies on it.
    let internal = Box::new(BufferInternal {
        owned_buf: Some(owned),
        shape: shape_box,
        strides: strides_box,
        suboffsets: suboffsets_box,
        format: format_storage,
    });
    let internal_ptr = Box::into_raw(internal);
    let internal_ref = unsafe { &mut *internal_ptr };

    unsafe {
        (*view).buf = buf_ptr;
        (*view).obj = exporter;
        (*view).len = len as PySsizeT;
        (*view).itemsize = 1;
        (*view).readonly = readonly;
        (*view).ndim = if want_shape { 1 } else { 0 };
        (*view).format = if (flags & PYBUF_FORMAT) != 0 {
            internal_ref.format.as_ptr() as *mut c_char
        } else {
            ptr::null_mut()
        };
        (*view).shape = if want_shape && !internal_ref.shape.is_empty() {
            internal_ref.shape.as_mut_ptr()
        } else {
            ptr::null_mut()
        };
        (*view).strides = if want_strides && !internal_ref.strides.is_empty() {
            internal_ref.strides.as_mut_ptr()
        } else {
            ptr::null_mut()
        };
        (*view).suboffsets = ptr::null_mut();
        (*view).internal = internal_ptr as *mut std::ffi::c_void;
        crate::object::Py_IncRef(exporter);
    }
    0
}

fn type_name(o: &Object) -> &'static str {
    use Object as O;
    match o {
        O::None => "NoneType",
        O::Bool(_) => "bool",
        O::Int(_) | O::Long(_) => "int",
        O::Float(_) => "float",
        O::Complex(_) => "complex",
        O::Str(_) => "str",
        O::Bytes(_) => "bytes",
        O::ByteArray(_) => "bytearray",
        O::Tuple(_) => "tuple",
        O::List(_) => "list",
        O::Dict(_) => "dict",
        O::Set(_) => "set",
        O::FrozenSet(_) => "frozenset",
        O::Range(_) => "range",
        O::MemoryView(_) => "memoryview",
        _ => "object",
    }
}

// ----------------------------------------------------------------
// PyBuffer_FillInfo / PyBuffer_FromContiguous / friends.
// ----------------------------------------------------------------

/// `PyBuffer_FillInfo(view, exporter, buf, len, readonly, flags)` —
/// helper invoked by user `bf_getbuffer` implementations to populate
/// a 1-D contiguous view. Mirrors CPython's helper exactly.
#[no_mangle]
pub unsafe extern "C" fn PyBuffer_FillInfo(
    view: *mut Py_buffer,
    exporter: *mut PyObject,
    buf: *mut std::ffi::c_void,
    len: PySsizeT,
    readonly: c_int,
    flags: c_int,
) -> c_int {
    if view.is_null() {
        crate::errors::set_buffer_error("PyBuffer_FillInfo: NULL view");
        return -1;
    }
    if (flags & PYBUF_WRITABLE) != 0 && readonly != 0 {
        crate::errors::set_buffer_error("Object is not writable");
        return -1;
    }
    let format_bytes = format_string_for(FormatKind::UInt8, ByteOrder::Native);
    let format_storage: Box<[u8]> = format_bytes.into_boxed_slice();
    let want_shape = (flags & PYBUF_ND) == PYBUF_ND;
    let want_strides = (flags & 0x0010) != 0;
    let shape_box: Box<[PySsizeT]> = if want_shape {
        Box::new([len])
    } else {
        Box::new([])
    };
    let strides_box: Box<[PySsizeT]> = if want_strides {
        Box::new([1])
    } else {
        Box::new([])
    };

    let internal = Box::new(BufferInternal {
        owned_buf: None,
        shape: shape_box,
        strides: strides_box,
        suboffsets: Box::new([]),
        format: format_storage,
    });
    let internal_ptr = Box::into_raw(internal);
    let internal_ref = unsafe { &mut *internal_ptr };

    unsafe {
        (*view).buf = buf;
        (*view).obj = exporter;
        (*view).len = len;
        (*view).itemsize = 1;
        (*view).readonly = readonly;
        (*view).ndim = if want_shape { 1 } else { 0 };
        (*view).format = if (flags & PYBUF_FORMAT) != 0 {
            internal_ref.format.as_ptr() as *mut c_char
        } else {
            ptr::null_mut()
        };
        (*view).shape = if want_shape && !internal_ref.shape.is_empty() {
            internal_ref.shape.as_mut_ptr()
        } else {
            ptr::null_mut()
        };
        (*view).strides = if want_strides && !internal_ref.strides.is_empty() {
            internal_ref.strides.as_mut_ptr()
        } else {
            ptr::null_mut()
        };
        (*view).suboffsets = ptr::null_mut();
        (*view).internal = internal_ptr as *mut std::ffi::c_void;
        if !exporter.is_null() {
            crate::object::Py_IncRef(exporter);
        }
    }
    0
}

/// `PyBuffer_IsContiguous(view, order)` — true if the view describes
/// memory laid out contiguously according to `order`:
/// - `'C'`: row-major
/// - `'F'`: column-major
/// - `'A'`: either
///
/// Returns 1 (true) or 0 (false). NULL `view` is a 0 (CPython does
/// the same — sentinel value).
#[no_mangle]
pub unsafe extern "C" fn PyBuffer_IsContiguous(view: *const Py_buffer, order: c_char) -> c_int {
    if view.is_null() {
        return 0;
    }
    let v = unsafe { &*view };
    if v.ndim == 0 {
        return 1;
    }
    if v.shape.is_null() {
        return 0;
    }
    let ndim = v.ndim as isize;
    let shape = unsafe { std::slice::from_raw_parts(v.shape, ndim as usize) };
    let strides_slice = if v.strides.is_null() {
        None
    } else {
        Some(unsafe { std::slice::from_raw_parts(v.strides, ndim as usize) })
    };
    let order = order as u8;
    let order = if order == b'A' {
        // Try both.
        return c_int::from(
            check_contiguous(shape, strides_slice, v.itemsize, true)
                || check_contiguous(shape, strides_slice, v.itemsize, false),
        );
    } else {
        order
    };
    c_int::from(check_contiguous(
        shape,
        strides_slice,
        v.itemsize,
        order == b'C',
    ))
}

fn check_contiguous(
    shape: &[PySsizeT],
    strides: Option<&[PySsizeT]>,
    itemsize: PySsizeT,
    c_order: bool,
) -> bool {
    let strides = match strides {
        Some(s) => s,
        None => {
            // No strides → treat as C-contiguous.
            return c_order;
        }
    };
    let mut sd = itemsize;
    if c_order {
        for i in (0..shape.len()).rev() {
            if shape[i] > 1 && strides[i] != sd {
                return false;
            }
            sd *= shape[i];
        }
    } else {
        for i in 0..shape.len() {
            if shape[i] > 1 && strides[i] != sd {
                return false;
            }
            sd *= shape[i];
        }
    }
    true
}

/// `PyBuffer_ToContiguous(buf, src, len, order)` — copy `src`'s
/// (possibly strided) memory into a flat contiguous block at `buf`.
///
/// `order` selects the iteration order (`'C'` or `'F'`).
#[no_mangle]
pub unsafe extern "C" fn PyBuffer_ToContiguous(
    buf: *mut std::ffi::c_void,
    src: *const Py_buffer,
    len: PySsizeT,
    order: c_char,
) -> c_int {
    if buf.is_null() || src.is_null() {
        return -1;
    }
    let v = unsafe { &*src };
    if v.len > len {
        return -1;
    }
    if v.ndim == 0 || v.shape.is_null() || v.strides.is_null() {
        unsafe { ptr::copy_nonoverlapping(v.buf as *const u8, buf as *mut u8, v.len as usize) };
        return 0;
    }
    walk_strided(v, buf as *mut u8, order as u8 == b'F')
}

/// `PyBuffer_FromContiguous(view, buf, len, order)` — inverse of
/// `PyBuffer_ToContiguous`: copy a flat contiguous block at `buf`
/// into a (possibly strided) destination view.
#[no_mangle]
pub unsafe extern "C" fn PyBuffer_FromContiguous(
    view: *const Py_buffer,
    buf: *mut std::ffi::c_void,
    len: PySsizeT,
    order: c_char,
) -> c_int {
    if buf.is_null() || view.is_null() {
        return -1;
    }
    let v = unsafe { &*view };
    if v.len < len {
        return -1;
    }
    if v.ndim == 0 || v.shape.is_null() || v.strides.is_null() {
        unsafe { ptr::copy_nonoverlapping(buf as *const u8, v.buf as *mut u8, len as usize) };
        return 0;
    }
    walk_strided_into(v, buf as *const u8, order as u8 == b'F')
}

fn walk_strided(v: &Py_buffer, dst: *mut u8, fortran: bool) -> c_int {
    let ndim = v.ndim as usize;
    let shape = unsafe { std::slice::from_raw_parts(v.shape, ndim) };
    let strides = unsafe { std::slice::from_raw_parts(v.strides, ndim) };
    let itemsize = v.itemsize as usize;
    let total: usize = shape.iter().map(|s| *s as usize).product();
    let mut indices = vec![0_isize; ndim];
    for n in 0..total {
        // Compute element offset in the source.
        let mut offset: isize = 0;
        for d in 0..ndim {
            offset += indices[d] * strides[d] as isize;
        }
        unsafe {
            ptr::copy_nonoverlapping(
                (v.buf as *const u8).offset(offset),
                dst.add(n * itemsize),
                itemsize,
            );
        }
        // Increment indices.
        if fortran {
            for d in 0..ndim {
                indices[d] += 1;
                if indices[d] < shape[d] as isize {
                    break;
                }
                indices[d] = 0;
            }
        } else {
            for d in (0..ndim).rev() {
                indices[d] += 1;
                if indices[d] < shape[d] as isize {
                    break;
                }
                indices[d] = 0;
            }
        }
    }
    0
}

fn walk_strided_into(v: &Py_buffer, src: *const u8, fortran: bool) -> c_int {
    let ndim = v.ndim as usize;
    let shape = unsafe { std::slice::from_raw_parts(v.shape, ndim) };
    let strides = unsafe { std::slice::from_raw_parts(v.strides, ndim) };
    let itemsize = v.itemsize as usize;
    let total: usize = shape.iter().map(|s| *s as usize).product();
    let mut indices = vec![0_isize; ndim];
    for n in 0..total {
        let mut offset: isize = 0;
        for d in 0..ndim {
            offset += indices[d] * strides[d] as isize;
        }
        unsafe {
            ptr::copy_nonoverlapping(
                src.add(n * itemsize),
                (v.buf as *mut u8).offset(offset),
                itemsize,
            );
        }
        if fortran {
            for d in 0..ndim {
                indices[d] += 1;
                if indices[d] < shape[d] as isize {
                    break;
                }
                indices[d] = 0;
            }
        } else {
            for d in (0..ndim).rev() {
                indices[d] += 1;
                if indices[d] < shape[d] as isize {
                    break;
                }
                indices[d] = 0;
            }
        }
    }
    0
}

/// `PyBuffer_GetPointer(view, indices)` — compute `view.buf + Σ
/// indices[i]*strides[i]`, dereferencing through `suboffsets[i]` if
/// non-negative (PEP 3118 indirect buffers).
#[no_mangle]
pub unsafe extern "C" fn PyBuffer_GetPointer(
    view: *const Py_buffer,
    indices: *const PySsizeT,
) -> *mut std::ffi::c_void {
    if view.is_null() {
        return ptr::null_mut();
    }
    let v = unsafe { &*view };
    let ndim = v.ndim as usize;
    if ndim == 0 {
        return v.buf;
    }
    if indices.is_null() {
        return v.buf;
    }
    let idxs = unsafe { std::slice::from_raw_parts(indices, ndim) };
    let strides = if v.strides.is_null() {
        None
    } else {
        Some(unsafe { std::slice::from_raw_parts(v.strides, ndim) })
    };
    let suboffsets = if v.suboffsets.is_null() {
        None
    } else {
        Some(unsafe { std::slice::from_raw_parts(v.suboffsets, ndim) })
    };

    let mut p = v.buf as *mut u8;
    for d in 0..ndim {
        let i = idxs[d];
        let stride = strides.map_or(v.itemsize, |s| s[d]);
        unsafe {
            p = p.offset(i as isize * stride as isize);
        }
        if let Some(so) = suboffsets {
            if so[d] >= 0 {
                unsafe {
                    let p_pp = p as *mut *mut u8;
                    p = (*p_pp).offset(so[d] as isize);
                }
            }
        }
    }
    p as *mut std::ffi::c_void
}

/// `PyBuffer_FillContiguousStrides(ndim, shape, strides, itemsize, order)` —
/// populate a stride array describing the C- or Fortran-contiguous
/// layout of `shape * itemsize` bytes.
#[no_mangle]
pub unsafe extern "C" fn PyBuffer_FillContiguousStrides(
    ndim: c_int,
    shape: *mut PySsizeT,
    strides: *mut PySsizeT,
    itemsize: PySsizeT,
    order: c_char,
) {
    if ndim <= 0 || shape.is_null() || strides.is_null() {
        return;
    }
    let n = ndim as usize;
    let shape_slice = unsafe { std::slice::from_raw_parts(shape, n) };
    let strides_slice = unsafe { std::slice::from_raw_parts_mut(strides, n) };
    if order as u8 == b'F' {
        let mut sd: PySsizeT = itemsize;
        for d in 0..n {
            strides_slice[d] = sd;
            sd *= shape_slice[d];
        }
    } else {
        let mut sd: PySsizeT = itemsize;
        for d in (0..n).rev() {
            strides_slice[d] = sd;
            sd *= shape_slice[d];
        }
    }
}

/// `PyBuffer_SizeFromFormat(format)` — see [`buffer_format::size_from_format`].
#[no_mangle]
pub unsafe extern "C" fn PyBuffer_SizeFromFormat(format: *const c_char) -> PySsizeT {
    unsafe { crate::buffer_format::size_from_format(format) }
}

/// `PyBuffer_HasFlag(view, flag)` — convenience macro CPython
/// extensions sometimes call. Expands to a presence test on the
/// flags carried by `view`'s exporter; we approximate by checking
/// the populated fields against the flag.
#[no_mangle]
pub unsafe extern "C" fn PyBuffer_HasFlag(view: *const Py_buffer, flag: c_int) -> c_int {
    if view.is_null() {
        return 0;
    }
    let v = unsafe { &*view };
    let mut effective: c_int = 0;
    if !v.shape.is_null() {
        effective |= PYBUF_ND;
    }
    if !v.strides.is_null() {
        effective |= PYBUF_STRIDES;
    }
    if !v.suboffsets.is_null() {
        effective |= PYBUF_INDIRECT;
    }
    if !v.format.is_null() {
        effective |= PYBUF_FORMAT;
    }
    if v.readonly == 0 {
        effective |= PYBUF_WRITABLE;
    }
    if (effective & flag) == flag {
        1
    } else {
        0
    }
}

// ----------------------------------------------------------------
// Tests
// ----------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fill_contiguous_strides_c_order() {
        let shape = [3 as PySsizeT, 4, 5];
        let mut strides = [0 as PySsizeT; 3];
        unsafe {
            PyBuffer_FillContiguousStrides(
                3,
                shape.as_ptr() as *mut PySsizeT,
                strides.as_mut_ptr(),
                8,
                b'C' as c_char,
            );
        }
        // Innermost dimension carries itemsize.
        assert_eq!(strides[2], 8);
        assert_eq!(strides[1], 8 * 5);
        assert_eq!(strides[0], 8 * 5 * 4);
    }

    #[test]
    fn fill_contiguous_strides_f_order() {
        let shape = [3 as PySsizeT, 4, 5];
        let mut strides = [0 as PySsizeT; 3];
        unsafe {
            PyBuffer_FillContiguousStrides(
                3,
                shape.as_ptr() as *mut PySsizeT,
                strides.as_mut_ptr(),
                8,
                b'F' as c_char,
            );
        }
        assert_eq!(strides[0], 8);
        assert_eq!(strides[1], 8 * 3);
        assert_eq!(strides[2], 8 * 3 * 4);
    }

    #[test]
    fn check_contiguous_recognises_c_order() {
        let shape = [3 as PySsizeT, 4];
        let strides = [4 * 4 as PySsizeT, 4];
        assert!(check_contiguous(&shape, Some(&strides), 4, true));
        assert!(!check_contiguous(&shape, Some(&strides), 4, false));
    }

    #[test]
    fn check_contiguous_recognises_f_order() {
        let shape = [3 as PySsizeT, 4];
        let strides = [4 as PySsizeT, 3 * 4];
        assert!(!check_contiguous(&shape, Some(&strides), 4, true));
        assert!(check_contiguous(&shape, Some(&strides), 4, false));
    }
}

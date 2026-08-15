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

use weavepy_vm::object::Object;

use crate::buffer_format::{format_string_for, ByteOrder, FormatKind};
use crate::object::{PyObject, PySsizeT};
use crate::slottable::{slot_table_for, Py_bf_getbuffer, Py_bf_releasebuffer};

/// Layout of `Py_buffer` in `Python.h`. Field order matches CPython
/// 3.13 exactly.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
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
    /// Heap-allocated copy of the source data. Only used by
    /// [`PyBuffer_FillInfo`] callers that hand us a raw pointer with no
    /// owning object; the native bytes-like exporter uses `keepalive`
    /// (zero-copy) instead.
    pub owned_buf: Option<Box<[u8]>>,
    /// Keep-alive for the exporter's *own* backing store. The native
    /// exporter points `Py_buffer::buf` directly at the exporter's data
    /// (no copy) and stashes a clone of the backing `Rc` here so the
    /// window stays valid for the view's lifetime.
    ///
    /// This is load-bearing for zero-copy consumers such as numpy's
    /// `PyArray_FromBuffer` (`np.frombuffer`, and hence protocol-5
    /// `_frombuffer` unpickling): numpy records `view.buf` as the array's
    /// data pointer, pins the exporter as the array's `base`, and then
    /// *releases the view*. CPython keeps working because a `bytes`
    /// exporter's `view.buf` aliases `PyBytes_AS_STRING` — memory owned by
    /// the (still-pinned) object, not the view. A defensive copy freed by
    /// `PyBuffer_Release` would dangle the instant numpy released the view,
    /// so the array would read freed memory (observed as corrupted
    /// datetime/period/interval data on protocol-5 round-trips).
    pub keepalive: Option<Object>,
    /// Export pin for a VM memoryview exporter — bumps the view's `exports`
    /// count for the C buffer's lifetime, dropped on `PyBuffer_Release`.
    pub export_pin: Option<ExportPin>,
    pub shape: Box<[PySsizeT]>,
    pub strides: Box<[PySsizeT]>,
    pub suboffsets: Box<[PySsizeT]>,
    pub format: Box<[u8]>,
}

/// A `PyObject_GetBuffer` over a VM `memoryview` counts as an export of
/// that view (CPython `memory_getbuf` bumps `self->exports`), so
/// `m.release()` raises BufferError while a C consumer (a
/// `_testbuffer.ndarray` re-exporter, say) still holds the buffer.
pub(crate) struct ExportPin(weavepy_vm::sync::Rc<weavepy_vm::object::PyMemoryView>);

impl ExportPin {
    pub(crate) fn new(mv: &weavepy_vm::sync::Rc<weavepy_vm::object::PyMemoryView>) -> Self {
        mv.exports.set(mv.exports.get() + 1);
        ExportPin(mv.clone())
    }
}

impl Drop for ExportPin {
    fn drop(&mut self) {
        self.0.exports.set(self.0.exports.get().saturating_sub(1));
    }
}

impl std::fmt::Debug for ExportPin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "<export pin>")
    }
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
    let trace = std::env::var_os("WEAVEPY_TRACE_BUF").is_some();
    if trace {
        let tn = unsafe {
            let ty = (*exporter).ob_type as *mut crate::layout::PyTypeObjectFull;
            if ty.is_null() {
                "<null>".to_owned()
            } else {
                let np = (*ty).tp_name;
                if np.is_null() {
                    "<noname>".to_owned()
                } else {
                    core::ffi::CStr::from_ptr(np).to_string_lossy().into_owned()
                }
            }
        };
        let has_st = unsafe { slot_table_for((*exporter).ob_type) }
            .map(|t| !t.get(Py_bf_getbuffer).is_null())
            .unwrap_or(false);
        let has_fb = !unsafe { foreign_bf_getbuffer((*exporter).ob_type) }.is_null();
        eprintln!(
            "[WEAVEPY_TRACE_BUF] GetBuffer exporter type={tn} flags={flags:#x} slot_table_bf={has_st} foreign_bf={has_fb}"
        );
    }

    // 1) Heap-type slot dispatch (WeavePy-managed slot table — types built
    //    through the dunder shim / `PyType_FromSpec`).
    let head = unsafe { &*exporter };
    if let Some(slot_table) = unsafe { slot_table_for(head.ob_type) } {
        let slot = slot_table.get(Py_bf_getbuffer);
        if !slot.is_null() {
            let getbuf: unsafe extern "C" fn(*mut PyObject, *mut Py_buffer, c_int) -> c_int =
                unsafe { slot.cast() };
            let rc = unsafe { getbuf(exporter, view, flags) };
            if trace {
                let fmt = unsafe {
                    let f = (*view).format;
                    if f.is_null() {
                        "<null>".to_owned()
                    } else {
                        core::ffi::CStr::from_ptr(f).to_string_lossy().into_owned()
                    }
                };
                eprintln!(
                    "[WEAVEPY_TRACE_BUF]   slot getbuf exp={exporter:p} rc={rc} format={fmt:?} itemsize={} ndim={} len={} buf={:p}",
                    unsafe { (*view).itemsize },
                    unsafe { (*view).ndim },
                    unsafe { (*view).len },
                    unsafe { (*view).buf },
                );
            }
            return rc;
        }
    }

    // 2) Foreign-type C-struct dispatch: a real extension type (numpy's
    //    `ndarray`, a Cython `cdef class` with `__getbuffer__`) stores its
    //    exporter in `tp_as_buffer->bf_getbuffer`. This is the path numpy's
    //    `numpy.random` Cython modules take when they acquire a typed
    //    `np.ndarray[uint32]` view (`SeedSequence.mix_entropy`). The slot
    //    owns filling `view` (incl. `view->obj`/refcount), exactly as
    //    CPython's `PyObject_GetBuffer` delegates.
    let slot = unsafe { foreign_bf_getbuffer(head.ob_type) };
    if !slot.is_null() {
        let getbuf: unsafe extern "C" fn(*mut PyObject, *mut Py_buffer, c_int) -> c_int =
            unsafe { std::mem::transmute(slot) };
        let rc = unsafe { getbuf(exporter, view, flags) };
        if trace {
            let fmt = unsafe {
                let f = (*view).format;
                if f.is_null() {
                    "<null>".to_owned()
                } else {
                    core::ffi::CStr::from_ptr(f).to_string_lossy().into_owned()
                }
            };
            let pend = crate::errors::pending().is_some();
            eprintln!(
                "[WEAVEPY_TRACE_BUF]   foreign getbuf rc={rc} format={fmt:?} itemsize={} ndim={} len={} pending_err={pend}",
                unsafe { (*view).itemsize },
                unsafe { (*view).ndim },
                unsafe { (*view).len },
            );
        }
        return rc;
    }

    // 3) Native fallback for built-in bytes-like types.
    let obj = unsafe { crate::object::clone_object(exporter) };
    let rc = fill_native_buffer(exporter, &obj, view, flags);
    if trace {
        let fmt = unsafe {
            let f = (*view).format;
            if f.is_null() {
                "<null>".to_owned()
            } else {
                core::ffi::CStr::from_ptr(f).to_string_lossy().into_owned()
            }
        };
        eprintln!(
            "[WEAVEPY_TRACE_BUF]   native getbuf rc={rc} format={fmt:?} itemsize={} ndim={} len={}",
            unsafe { (*view).itemsize },
            unsafe { (*view).ndim },
            unsafe { (*view).len },
        );
    }
    rc
}

/// Read a foreign type's `tp_as_buffer->bf_getbuffer` slot (or NULL).
///
/// # Safety
/// `ty` must be a live `PyObject*`-typed type pointer or NULL.
unsafe fn foreign_bf_getbuffer(ty: *mut crate::types::PyTypeObject) -> *mut std::ffi::c_void {
    // Walk `tp_base` — CPython's `type_new` inherits `tp_as_buffer` into
    // subclasses, but a VM-level Python subclass of a foreign exporter
    // (e.g. `np.ma.MaskedArray(ndarray)`) gets its C mirror built without
    // that copy, so resolve the slot the way `inherit_slots` would.
    let mut tyf = ty as *mut crate::layout::PyTypeObjectFull;
    while !tyf.is_null() {
        let procs = unsafe { (*tyf).tp_as_buffer };
        if !procs.is_null() {
            let slot = unsafe { (*procs).bf_getbuffer };
            if !slot.is_null() {
                return slot;
            }
        }
        tyf = unsafe { (*tyf).tp_base } as *mut crate::layout::PyTypeObjectFull;
    }
    ptr::null_mut()
}

/// Read a foreign type's `tp_as_buffer->bf_releasebuffer` slot (or NULL).
///
/// # Safety
/// `ty` must be a live `PyObject*`-typed type pointer or NULL.
unsafe fn foreign_bf_releasebuffer(ty: *mut crate::types::PyTypeObject) -> *mut std::ffi::c_void {
    // Same `tp_base` walk as `foreign_bf_getbuffer` — get/release must
    // resolve against the same ancestor's `tp_as_buffer`.
    let mut tyf = ty as *mut crate::layout::PyTypeObjectFull;
    while !tyf.is_null() {
        let procs = unsafe { (*tyf).tp_as_buffer };
        if !procs.is_null() {
            let slot = unsafe { (*procs).bf_releasebuffer };
            if !slot.is_null() {
                return slot;
            }
        }
        tyf = unsafe { (*tyf).tp_base } as *mut crate::layout::PyTypeObjectFull;
    }
    ptr::null_mut()
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
    if std::env::var_os("WEAVEPY_TRACE_BUF").is_some() {
        eprintln!(
            "[WEAVEPY_TRACE_BUF] Release exp={exporter:p} ndim={} buf={:p}",
            v.ndim, v.buf,
        );
    }

    // 1) Heap-type slot dispatch (WeavePy-managed slot table).
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
        // A WeavePy slot-table exporter with no release slot: still drop the
        // get-time ref below via the native path's DecRef.
        if !slot_table.get(Py_bf_getbuffer).is_null() {
            unsafe { crate::object::Py_DecRef(exporter) };
            *v = Py_buffer::zeroed();
            return;
        }
    }

    // 2) Foreign-type C-struct dispatch: mirror CPython's `PyBuffer_Release`
    //    — call `bf_releasebuffer` (if any), then drop the reference the
    //    exporter took in `bf_getbuffer`.
    let rel = unsafe { foreign_bf_releasebuffer(head.ob_type) };
    let getb = unsafe { foreign_bf_getbuffer(head.ob_type) };
    if !rel.is_null() || !getb.is_null() {
        if !rel.is_null() {
            let release: unsafe extern "C" fn(*mut PyObject, *mut Py_buffer) =
                unsafe { std::mem::transmute(rel) };
            unsafe { release(exporter, view) };
        }
        unsafe { crate::object::Py_DecRef(exporter) };
        *v = Py_buffer::zeroed();
        return;
    }

    // 3) Native release path. We allocated a `BufferInternal` on
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
    if !unsafe { foreign_bf_getbuffer(head.ob_type) }.is_null() {
        return 1;
    }
    let obj = unsafe { crate::object::clone_object(o) };
    (matches!(
        obj,
        Object::Bytes(_) | Object::ByteArray(_) | Object::MemoryView(_)
    ) || weavepy_vm::builtins::has_buffer_dunder(&obj))
    .into()
}

// ----------------------------------------------------------------
// Native fallback — handles bytes / bytearray / memoryview.
// ----------------------------------------------------------------

/// A bytes-like exporter's buffer, fully described: raw window bytes,
/// element size, PEP 3118 `format` (not yet NUL-terminated) and the
/// per-dimension `shape` (elements) / `strides` (bytes). Most
/// bytes-likes are a flat 1-D `'B'`/itemsize-1 region, but a
/// `memoryview` re-export carries its own (possibly typed) format and
/// itemsize so that a consumer probing it — e.g. Cython fused-type
/// dispatch over `ndarray[object]` — observes the faithful `'O'`/8
/// layout instead of a byte collapse.
struct NativeExport {
    /// Pointer into the exporter's *own* backing store (no copy). Valid
    /// as long as `keepalive` — and, after release, the pinned exporter —
    /// is alive.
    ptr: *mut u8,
    len: usize,
    itemsize: usize,
    format: Vec<u8>,
    shape: Vec<PySsizeT>,
    strides: Vec<PySsizeT>,
    /// PEP 3118 suboffsets (empty = no indirection). Only a memoryview
    /// over a suboffsets-carrying C exporter re-exports these; the
    /// consumer must have requested `PyBUF_INDIRECT`.
    suboffsets: Vec<PySsizeT>,
    readonly: c_int,
    /// Clone of the exporter (or its inner backing) that keeps `ptr` valid.
    keepalive: Object,
}

/// Raw pointer to the first byte of a memoryview's backing region. All
/// three backings keep the region at a stable address for the `Rc`'s
/// lifetime (`Bytes`/`ByteArray` heap buffers never move without a
/// resize; `Shared` mmap/shared regions never move at all).
fn memoryview_backing_ptr(buf: &weavepy_vm::object::MemoryViewBuffer) -> *mut u8 {
    use weavepy_vm::object::MemoryViewBuffer as B;
    match buf {
        B::Bytes(b) => b.as_ptr() as *mut u8,
        B::ByteArray(rc) => rc.borrow().as_ptr() as *mut u8,
        B::Shared(s) => s.data_ptr(),
    }
}

fn describe_native_export(obj: &Object) -> Result<NativeExport, ()> {
    let export = match obj {
        Object::Bytes(b) => NativeExport {
            ptr: b.as_ptr() as *mut u8,
            len: b.len(),
            itemsize: 1,
            format: b"B".to_vec(),
            shape: vec![b.len() as PySsizeT],
            strides: vec![1],
            suboffsets: Vec::new(),
            readonly: 1,
            keepalive: obj.clone(),
        },
        Object::ByteArray(rc) => {
            let (ptr, len) = {
                let borrowed = rc.borrow();
                (borrowed.as_ptr() as *mut u8, borrowed.len())
            };
            NativeExport {
                ptr,
                len,
                itemsize: 1,
                format: b"B".to_vec(),
                shape: vec![len as PySsizeT],
                strides: vec![1],
                suboffsets: Vec::new(),
                readonly: 0,
                keepalive: obj.clone(),
            }
        }
        Object::MemoryView(mv) => {
            if mv.released.get() {
                crate::errors::set_value_error("memoryview: released");
                return Err(());
            }
            let len = mv.len.get();
            let start = mv.start.get();
            let base_ptr = memoryview_backing_ptr(&mv.buffer);
            let ptr = unsafe { base_ptr.add(start) };
            let itemsize = mv.itemsize.get().max(1);
            let format = mv.format.borrow().clone().into_bytes();
            // Element shape/stride: honour an explicit shape, else derive a
            // 1-D `[len / itemsize]` C-contiguous layout. This is what keeps
            // a typed view's `itemsize`/`format` self-consistent with the
            // dimensions a consumer reads back.
            let stored_shape = mv.shape.borrow();
            let (shape, strides) = if mv.zero_dim.get() {
                // 0-dim scalar view: ndim stays 0 (empty shape/strides), so
                // a re-export through C reads back `ndim == 0` like CPython.
                (Vec::new(), Vec::new())
            } else if stored_shape.is_empty() {
                let n = len.checked_div(itemsize).unwrap_or(0);
                (vec![n as PySsizeT], vec![itemsize as PySsizeT])
            } else {
                let shape: Vec<PySsizeT> = stored_shape.iter().map(|&s| s as PySsizeT).collect();
                let stored_strides = mv.strides.borrow();
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
                (shape, strides)
            };
            NativeExport {
                ptr,
                len,
                itemsize,
                format,
                shape,
                strides,
                suboffsets: mv
                    .suboffsets
                    .borrow()
                    .iter()
                    .map(|&s| s as PySsizeT)
                    .collect(),
                readonly: c_int::from(mv.readonly.get()),
                keepalive: obj.clone(),
            }
        }
        other => {
            // A PEP 688 exporter (`array.array`, a user class with
            // `__buffer__`) crossing into C: take its view and export that —
            // the memoryview pins the storage, so the export stays valid
            // (test_buffer builds `_testbuffer.ndarray(array.array(...))`).
            if let Some(mv) = weavepy_vm::builtins::buffer_exported_view(other) {
                return describe_native_export(&Object::MemoryView(mv));
            }
            // CPython's `PyObject_GetBuffer` raises *TypeError* for a
            // non-exporter (abstract.c), which is what e.g.
            // `urllib.parse.parse_qsl(object())` asserts on.
            crate::errors::set_type_error(format!(
                "a bytes-like object is required, not '{}'",
                obj.type_name_owned()
            ));
            return Err(());
        }
    };
    Ok(export)
}

fn fill_native_buffer(
    exporter: *mut PyObject,
    obj: &Object,
    view: *mut Py_buffer,
    flags: c_int,
) -> c_int {
    let export = match describe_native_export(obj) {
        Ok(e) => e,
        Err(()) => return -1,
    };

    if (flags & PYBUF_WRITABLE) != 0 && export.readonly != 0 {
        crate::errors::set_buffer_error("Object is not writable");
        return -1;
    }
    // A suboffsets-carrying view can only be exported to a consumer that
    // accepts indirection (CPython memoryobject.c `memory_getbuf`).
    let has_suboffsets = export.suboffsets.iter().any(|&s| s >= 0);
    if has_suboffsets && (flags & PYBUF_INDIRECT) != PYBUF_INDIRECT {
        crate::errors::set_buffer_error("underlying buffer requires suboffsets");
        return -1;
    }
    if let Object::MemoryView(mv) = obj {
        // CPython `memory_getbuf`: PyBUF_SIMPLE|PyBUF_FORMAT (and
        // WRITABLE|FORMAT) make no sense — a raw request must drop the
        // format (test_ndarray_getbuf drives every flag combination).
        if (flags & PYBUF_FORMAT) != 0 && (flags & PYBUF_ND) != PYBUF_ND {
            crate::errors::set_buffer_error(
                "memoryview: cannot cast to unsigned bytes if the format flag is present",
            );
            return -1;
        }
        // Without a strides request the exporter must be C-contiguous
        // (a raw block hands out `buf..buf+len` linearly).
        if (flags & 0x0010) == 0 && !mv.is_c_contiguous() {
            crate::errors::set_buffer_error("memoryview: underlying buffer is not C-contiguous");
            return -1;
        }
        // Explicit contiguity requests (CPython `PyBuffer_IsContiguous`
        // gating in `memory_getbuf`).
        if (flags & 0x0020) != 0 && !mv.is_c_contiguous() {
            crate::errors::set_buffer_error("memoryview: underlying buffer is not C-contiguous");
            return -1;
        }
        if (flags & 0x0040) != 0 && !mv.is_f_contiguous() {
            crate::errors::set_buffer_error(
                "memoryview: underlying buffer is not Fortran contiguous",
            );
            return -1;
        }
        if (flags & 0x0080) != 0 && !mv.is_c_contiguous() && !mv.is_f_contiguous() {
            crate::errors::set_buffer_error("memoryview: underlying buffer is not contiguous");
            return -1;
        }
    }

    let len = export.len;
    // Zero-copy: `view.buf` aliases the exporter's own storage. The
    // `keepalive` clone stashed in `BufferInternal` keeps that storage
    // resident for the view's lifetime; the pinned exporter keeps it
    // resident afterwards (see `BufferInternal::keepalive`).
    let buf_ptr = export.ptr as *mut std::ffi::c_void;

    // NUL-terminate the format for `Py_buffer::format`.
    let mut format_vec = export.format;
    if format_vec.is_empty() {
        format_vec.push(b'B');
    }
    format_vec.push(0);
    let format_storage: Box<[u8]> = format_vec.into_boxed_slice();

    let want_shape = (flags & PYBUF_ND) == PYBUF_ND;
    let want_strides = (flags & 0x0010) != 0;
    let want_format = (flags & PYBUF_FORMAT) != 0;
    let ndim = export.shape.len();
    let shape_box: Box<[PySsizeT]> = if want_shape {
        export.shape.into_boxed_slice()
    } else {
        Box::new([])
    };
    let strides_box: Box<[PySsizeT]> = if want_strides {
        export.strides.into_boxed_slice()
    } else {
        Box::new([])
    };
    let suboffsets_box: Box<[PySsizeT]> = if has_suboffsets {
        export.suboffsets.into_boxed_slice()
    } else {
        Box::new([])
    };

    // Heap up the internal block — the release path relies on it.
    let internal = Box::new(BufferInternal {
        owned_buf: None,
        keepalive: Some(export.keepalive),
        export_pin: match obj {
            Object::MemoryView(mv) => Some(ExportPin::new(mv)),
            _ => None,
        },
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
        (*view).itemsize = export.itemsize as PySsizeT;
        (*view).readonly = export.readonly;
        // Without PyBUF_ND the export is a flat byte block: CPython
        // (`PyBuffer_FillInfo`, `memory_getbuf`) reports `ndim = 1` with
        // NULL shape (test_ndarray_getbuf checks `nd.ndim == 1` for
        // SIMPLE/WRITABLE requests).
        (*view).ndim = if want_shape { ndim as c_int } else { 1 };
        (*view).format = if want_format {
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
        (*view).suboffsets = if internal_ref.suboffsets.is_empty() {
            ptr::null_mut()
        } else {
            internal_ref.suboffsets.as_mut_ptr()
        };
        (*view).internal = internal_ptr as *mut std::ffi::c_void;
        crate::object::Py_IncRef(exporter);
    }
    0
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
        keepalive: None,
        export_pin: None,
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
        // CPython `PyBuffer_FillInfo` always reports `ndim = 1` (shape stays
        // NULL for simple requests).
        (*view).ndim = 1;
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
    // CPython: any suboffsets array (even all-negative) is non-contiguous.
    if !v.suboffsets.is_null() {
        return 0;
    }
    match order as u8 {
        b'C' => c_int::from(unsafe { view_is_c_contiguous(v) }),
        b'F' => c_int::from(unsafe { view_is_f_contiguous(v) }),
        b'A' => c_int::from(unsafe { view_is_c_contiguous(v) || view_is_f_contiguous(v) }),
        _ => 0,
    }
}

/// CPython 3.13 `_IsCContiguous` (abstract.c): a zero-length buffer or
/// `strides == NULL` is C-contiguous by definition; axes of length 0/1
/// impose no stride constraint (`dim > 1` gate).
unsafe fn view_is_c_contiguous(v: &Py_buffer) -> bool {
    if v.len == 0 {
        return true;
    }
    if v.strides.is_null() || v.shape.is_null() || v.ndim <= 0 {
        return true;
    }
    let n = v.ndim as usize;
    let shape = unsafe { std::slice::from_raw_parts(v.shape, n) };
    let strides = unsafe { std::slice::from_raw_parts(v.strides, n) };
    let mut sd = v.itemsize;
    for i in (0..n).rev() {
        let dim = shape[i];
        if dim > 1 && strides[i] != sd {
            return false;
        }
        sd *= dim;
    }
    true
}

/// CPython 3.13 `_IsFortranContiguous` (abstract.c): with
/// `strides == NULL` (C-contiguous packing) the view is
/// Fortran-contiguous only when it is effectively 1-D; strided views
/// skip axes of length 0/1 (`dim > 1` gate).
unsafe fn view_is_f_contiguous(v: &Py_buffer) -> bool {
    if v.len == 0 {
        return true;
    }
    if v.strides.is_null() {
        if v.ndim <= 1 {
            return true;
        }
        if v.shape.is_null() {
            return true;
        }
        // Effectively 1-D: at most one axis longer than 1.
        let n = v.ndim as usize;
        let shape = unsafe { std::slice::from_raw_parts(v.shape, n) };
        return shape.iter().filter(|&&d| d > 1).count() <= 1;
    }
    if v.shape.is_null() || v.ndim <= 0 {
        return true;
    }
    let n = v.ndim as usize;
    let shape = unsafe { std::slice::from_raw_parts(v.shape, n) };
    let strides = unsafe { std::slice::from_raw_parts(v.strides, n) };
    let mut sd = v.itemsize;
    for i in 0..n {
        let dim = shape[i];
        if dim > 1 && strides[i] != sd {
            return false;
        }
        sd *= dim;
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
    // CPython `PyBuffer_ToContiguous`: a buffer that is already contiguous
    // in the requested order is copied *verbatim* — crucially, order 'A' on
    // an F-contiguous buffer keeps the Fortran layout (test_buffer's
    // `verify` reconstructs Fortran ndarrays from exactly these bytes).
    if unsafe { PyBuffer_IsContiguous(src, order) } != 0 {
        unsafe { ptr::copy_nonoverlapping(v.buf as *const u8, buf as *mut u8, v.len as usize) };
        return 0;
    }
    if v.ndim == 0 || v.shape.is_null() {
        unsafe { ptr::copy_nonoverlapping(v.buf as *const u8, buf as *mut u8, v.len as usize) };
        return 0;
    }
    // A non-contiguous 'A' request gathers in C order (CPython: `order = 'C'`).
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
    // Mirror of `PyBuffer_ToContiguous`: an already-contiguous destination
    // takes the flat bytes verbatim in its own layout.
    if unsafe { PyBuffer_IsContiguous(view, order) } != 0 {
        unsafe { ptr::copy_nonoverlapping(buf as *const u8, v.buf as *mut u8, len as usize) };
        return 0;
    }
    if v.ndim == 0 || v.shape.is_null() {
        unsafe { ptr::copy_nonoverlapping(buf as *const u8, v.buf as *mut u8, len as usize) };
        return 0;
    }
    walk_strided_into(v, buf as *const u8, order as u8 == b'F')
}

/// Element pointer at `indices` per PEP 3118: add `index * stride` per
/// dimension, dereferencing through `suboffsets[d]` when non-negative
/// (PIL-style indirect buffers) — CPython's `PyBuffer_GetPointer`.
fn strided_element_ptr(v: &Py_buffer, indices: &[isize]) -> *mut u8 {
    let ndim = indices.len();
    // NULL strides means C-contiguous layout (PEP 3118) — synthesize the
    // stride table so an ND-only export still walks correctly
    // (test_py_buffer_to_contiguous requests 'F' from a PyBUF_ND view).
    let synthesized: Vec<PySsizeT>;
    let strides: &[PySsizeT] = if v.strides.is_null() {
        let shape = unsafe { std::slice::from_raw_parts(v.shape, ndim) };
        let mut s = vec![0; ndim];
        let mut acc = v.itemsize;
        for d in (0..ndim).rev() {
            s[d] = acc;
            acc *= shape[d].max(1);
        }
        synthesized = s;
        &synthesized
    } else {
        unsafe { std::slice::from_raw_parts(v.strides, ndim) }
    };
    let suboffsets = if v.suboffsets.is_null() {
        None
    } else {
        Some(unsafe { std::slice::from_raw_parts(v.suboffsets, ndim) })
    };
    let mut p = v.buf as *mut u8;
    for d in 0..ndim {
        p = unsafe { p.offset(indices[d] * strides[d] as isize) };
        if let Some(so) = suboffsets {
            if so[d] >= 0 {
                p = unsafe { (*(p as *mut *mut u8)).offset(so[d] as isize) };
            }
        }
    }
    p
}

fn walk_strided(v: &Py_buffer, dst: *mut u8, fortran: bool) -> c_int {
    let ndim = v.ndim as usize;
    let shape = unsafe { std::slice::from_raw_parts(v.shape, ndim) };
    let itemsize = v.itemsize as usize;
    let total: usize = shape.iter().map(|s| *s as usize).product();
    let mut indices = vec![0_isize; ndim];
    for n in 0..total {
        unsafe {
            ptr::copy_nonoverlapping(
                strided_element_ptr(v, &indices) as *const u8,
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
    let itemsize = v.itemsize as usize;
    let total: usize = shape.iter().map(|s| *s as usize).product();
    let mut indices = vec![0_isize; ndim];
    for n in 0..total {
        unsafe {
            ptr::copy_nonoverlapping(
                src.add(n * itemsize),
                strided_element_ptr(v, &indices),
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

    fn contiguity_view(
        shape: &mut [PySsizeT],
        strides: &mut [PySsizeT],
        itemsize: PySsizeT,
    ) -> Py_buffer {
        let mut v: Py_buffer = unsafe { std::mem::zeroed() };
        v.ndim = shape.len() as c_int;
        v.itemsize = itemsize;
        v.len = shape.iter().product::<PySsizeT>() * itemsize;
        v.shape = shape.as_mut_ptr();
        v.strides = strides.as_mut_ptr();
        v
    }

    #[test]
    fn check_contiguous_recognises_c_order() {
        let mut shape = [3 as PySsizeT, 4];
        let mut strides = [4 * 4 as PySsizeT, 4];
        let v = contiguity_view(&mut shape, &mut strides, 4);
        unsafe {
            assert!(view_is_c_contiguous(&v));
            assert!(!view_is_f_contiguous(&v));
        }
    }

    #[test]
    fn check_contiguous_recognises_f_order() {
        let mut shape = [3 as PySsizeT, 4];
        let mut strides = [4 as PySsizeT, 3 * 4];
        let v = contiguity_view(&mut shape, &mut strides, 4);
        unsafe {
            assert!(!view_is_c_contiguous(&v));
            assert!(view_is_f_contiguous(&v));
        }
    }
}

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

use weavepy_vm::object::{MemoryViewBuffer, Object, PyMemoryView, SharedMemBuffer};
use weavepy_vm::sync::{Cell, RefCell};

use crate::buffer::Py_buffer;
use crate::object::{PyObject, PySsizeT};

/// A held C `Py_buffer` export presented as a [`SharedMemBuffer`] region,
/// so a WeavePy `memoryview` over a C exporter (numpy's `ndarray`,
/// `_testbuffer.ndarray`, a Cython typed buffer) *aliases* the exporter's
/// memory instead of copying it — writes through the view land in the C
/// buffer, and `PyBuffer_Release` runs when the last view drops (RFC 0066
/// WS1).
///
/// The addressable window is `[ptr, ptr + len)`. For a strided exporter
/// with negative strides `ptr` sits at the *lowest* addressed byte (the
/// view's `start` then points back at the first logical element); for a
/// suboffsets (PIL-style) exporter the window only spans the root block —
/// element access chases the indirection pointers instead of the linear
/// window.
struct CBufferRegion {
    /// The held buffer (keeps a reference on the exporter). Released via
    /// `PyBuffer_Release` on drop, or eagerly when the last view over the
    /// region calls `memoryview.release()` (CPython `_PyManagedBuffer`
    /// releases the exporter as soon as its export count hits zero, which
    /// is what lets `_testbuffer.ndarray.pop()` proceed after `m.release()`).
    view: std::cell::Cell<Py_buffer>,
    released: std::cell::Cell<bool>,
    /// Count of views over this region that have been explicitly
    /// `release()`d. A released view still holds its `Rc` (the `buffer`
    /// field is immutable), so the strong count alone can't tell live
    /// views from released ones.
    released_views: std::cell::Cell<usize>,
    ptr: *mut u8,
    len: usize,
    readonly: bool,
}

impl CBufferRegion {
    fn release_now(&self) {
        if !self.released.replace(true) {
            let mut v = self.view.get();
            unsafe { crate::buffer::PyBuffer_Release(&raw mut v) };
            self.view.set(v);
        }
    }
}

// SAFETY: the region models genuinely aliased exporter memory, exactly
// like the mmap/shared-memory `SharedMemBuffer` impls. Cross-thread
// access is serialised by the GIL, as in CPython.
unsafe impl Send for CBufferRegion {}
unsafe impl Sync for CBufferRegion {}

impl std::fmt::Debug for CBufferRegion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "<C buffer region {:p} len={} readonly={}>",
            self.ptr, self.len, self.readonly
        )
    }
}

impl SharedMemBuffer for CBufferRegion {
    fn byte_len(&self) -> usize {
        self.len
    }
    fn data_ptr(&self) -> *mut u8 {
        self.ptr
    }
    fn is_readonly(&self) -> bool {
        self.readonly
    }
    fn on_view_release(&self, holders: usize) {
        let released = self.released_views.get() + 1;
        self.released_views.set(released);
        // Every remaining holder is a released view: nothing can access the
        // region any more, release the exporter's buffer eagerly.
        if released >= holders {
            self.release_now();
        }
    }
}

impl Drop for CBufferRegion {
    fn drop(&mut self) {
        self.release_now();
    }
}

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
            // CPython checks `PyObject_CheckBuffer` up front and raises
            // TypeError with the "memoryview: " prefix (memoryobject.c);
            // `test_urlparse` relies on the TypeError class.
            if unsafe { crate::buffer::PyObject_CheckBuffer(exporter) } == 0 {
                crate::errors::set_type_error(format!(
                    "memoryview: a bytes-like object is required, not '{}'",
                    obj.type_name_owned()
                ));
                return ptr::null_mut();
            }
            const PYBUF_FULL_RO: c_int = 0x011C; // INDIRECT | STRIDES | ND | FORMAT
            let mut view = Py_buffer::zeroed();
            let rc = unsafe {
                crate::buffer::PyObject_GetBuffer(exporter, &raw mut view, PYBUF_FULL_RO)
            };
            if rc != 0 {
                return ptr::null_mut();
            }
            // CPython `PyBUF_MAX_NDIM` (test_memoryview_construction builds
            // a 128-dim `_testbuffer.ndarray`).
            if view.ndim > 64 {
                unsafe { crate::buffer::PyBuffer_Release(&raw mut view) };
                crate::errors::set_value_error(
                    "memoryview: number of dimensions must not exceed 64",
                );
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

            // Capture the exporter's multi-dimensional geometry. CPython's
            // memoryview references the exporter's memory and keeps its
            // `ndim`/`shape`/`strides`/`suboffsets`; WeavePy holds the
            // `Py_buffer` open inside a [`CBufferRegion`] so the view
            // *aliases* the exporter's memory the same way — a
            // `struct.pack_into(...)` through the view mutates the C
            // buffer, and a PIL-style (suboffsets) exporter round-trips.
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
            } else if !shape.is_empty() {
                weavepy_vm::object::c_contiguous_strides(&shape, view_itemsize)
            } else {
                Vec::new()
            };
            let suboffsets: Vec<isize> = if ndim >= 1 && !view.suboffsets.is_null() {
                (0..ndim)
                    .map(|i| unsafe { *view.suboffsets.add(i) } as isize)
                    .collect()
            } else {
                Vec::new()
            };
            let has_sub = suboffsets.iter().any(|&s| s >= 0);
            let total_elems: usize = shape.iter().product();

            // Compute the addressable window. Negative strides address
            // bytes *below* `view.buf`, so the region starts at the lowest
            // addressed byte and `start` points back at the first logical
            // element. An indirect (suboffsets) exporter's window only
            // spans the root block; element access chases the pointers.
            let (region_ptr, region_len, start) = if view.buf.is_null() {
                (ptr::null_mut::<u8>(), 0usize, 0usize)
            } else if has_sub {
                // Indirect exporter: the *root block* is the pointer table
                // spanned by dims up to the first indirect one; negative
                // strides address entries below `buf`, so anchor the region
                // at the lowest entry and point `start` back at `buf` —
                // otherwise slicing (`start + i*stride`) would go negative
                // and get clamped into silent rebasing (test_buffer's
                // ND_PIL negative-stride slices crashed on the bad chase).
                let f = suboffsets.iter().position(|&s| s >= 0).unwrap_or(0);
                let mut min_off = 0isize;
                let mut max_off = 0isize;
                for d in 0..=f.min(shape.len().saturating_sub(1)) {
                    let extent = (shape[d].saturating_sub(1)) as isize * strides[d];
                    min_off += extent.min(0);
                    max_off += extent.max(0);
                }
                let unit = std::mem::size_of::<*mut u8>();
                let span = (max_off - min_off) as usize + unit;
                (
                    unsafe { (view.buf as *mut u8).offset(min_off) },
                    span,
                    (-min_off) as usize,
                )
            } else if ndim == 0 || shape.is_empty() {
                (view.buf as *mut u8, view_len, 0usize)
            } else if total_elems == 0 {
                (view.buf as *mut u8, 0usize, 0usize)
            } else {
                let mut min_off = 0isize;
                let mut max_off = 0isize;
                for (d, &dim) in shape.iter().enumerate() {
                    let extent = (dim.saturating_sub(1)) as isize * strides[d];
                    min_off += extent.min(0);
                    max_off += extent.max(0);
                }
                let span = (max_off - min_off) as usize + view_itemsize;
                (
                    unsafe { (view.buf as *mut u8).offset(min_off) },
                    span,
                    (-min_off) as usize,
                )
            };
            if std::env::var_os("WEAVEPY_TRACE_BUF").is_some() {
                eprintln!(
                    "[WEAVEPY_TRACE_BUF]   FromObject aliased mv format={format:?} itemsize={view_itemsize} ndim={ndim} len={view_len} region_len={region_len} start={start} suboffsets={has_sub}"
                );
            }
            let region = CBufferRegion {
                view: std::cell::Cell::new(view),
                released: std::cell::Cell::new(false),
                released_views: std::cell::Cell::new(0),
                ptr: region_ptr,
                len: region_len,
                readonly,
            };
            let zero_dim = ndim == 0;
            // `m.obj` is exactly `view.obj`: an ND_REDIRECT-style exporter
            // sets it to the *root* exporter (test_memoryview_redirect
            // asserts `memoryview(z).obj is x` across a redirect chain) and
            // a legacy getbufferproc leaves it NULL, which surfaces as None
            // (test_memoryview_from_static_exporter's `legacy_mode`).
            let owner = if view.obj.is_null() {
                None
            } else if view.obj != exporter {
                Some(unsafe { crate::object::clone_object(view.obj) })
            } else {
                Some(obj.clone())
            };
            PyMemoryView {
                buffer: MemoryViewBuffer::Shared(weavepy_vm::sync::Rc::new(region)),
                start: Cell::new(start),
                len: Cell::new(view_len),
                readonly: Cell::new(readonly),
                released: Cell::new(false),
                format: RefCell::new(format),
                itemsize: Cell::new(view_itemsize),
                shape: RefCell::new(shape),
                strides: RefCell::new(strides),
                suboffsets: RefCell::new(suboffsets),
                exporter: RefCell::new(owner),
                zero_dim: Cell::new(zero_dim),
                hash: Cell::new(-1),
                exports: Cell::new(0),
                release_inner: RefCell::new(None),
                restricted: Cell::new(false),
            }
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
        suboffsets: RefCell::new(other.suboffsets.borrow().clone()),
        exporter: RefCell::new(other.exporter.borrow().clone()),
        zero_dim: Cell::new(other.zero_dim.get()),
        hash: Cell::new(-1),
        exports: Cell::new(0),
        release_inner: RefCell::new(None),
        restricted: Cell::new(false),
    }
}

/// `PyMemoryView_FromMemory(mem, size, flags)` — wrap a raw `(ptr,
/// len)` block. `flags` is `PyBUF_READ` or `PyBUF_WRITE`. Like CPython,
/// the view *aliases* the memory (the caller guarantees its lifetime):
/// extensions routinely create the view first and fill the block
/// afterwards (e.g. `_testbuffer`'s `unpack_single` scratch), or write
/// through it (`pack_single`), so a snapshot copy breaks both.
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
    let readonly = (flags & 0x100) != 0; // PyBUF_READ
    let region = CBufferRegion {
        view: std::cell::Cell::new(Py_buffer::zeroed()), // obj == NULL → release is a no-op
        released: std::cell::Cell::new(false),
        released_views: std::cell::Cell::new(0),
        ptr: mem as *mut u8,
        len,
        readonly,
    };
    let mv = PyMemoryView::contiguous_1d(
        MemoryViewBuffer::Shared(weavepy_vm::sync::Rc::new(region)),
        len,
        readonly,
        "B".to_owned(),
        1,
    );
    crate::object::into_owned(Object::MemoryView(weavepy_vm::sync::Rc::new(mv)))
}

/// `PyMemoryView_FromBuffer(view)` — build a memoryview that wraps a
/// fully-populated `Py_buffer`. Like CPython, the view *aliases* the
/// buffer's memory (a snapshot of the `Py_buffer` record is held and
/// `PyBuffer_Release`d when the view drops — a no-op for the common
/// `view->obj == NULL` caller-owned case). `_testbuffer`'s
/// `ndarray.__init__` relies on write-through: it packs its initializer
/// via `struct.pack_into` on exactly this view.
#[no_mangle]
pub unsafe extern "C" fn PyMemoryView_FromBuffer(view: *const Py_buffer) -> *mut PyObject {
    if view.is_null() {
        crate::errors::set_buffer_error("PyMemoryView_FromBuffer: NULL view");
        return ptr::null_mut();
    }
    let v = unsafe { &*view };
    // CPython `PyBUF_MAX_NDIM`: memoryviews cap at 64 dimensions
    // (test_ndarray_exceptions builds a 128-dim `_testbuffer.ndarray` and
    // expects `memoryview_from_buffer()` to refuse it).
    if v.ndim > 64 {
        crate::errors::set_value_error("memoryview: number of dimensions must not exceed 64");
        return ptr::null_mut();
    }
    let format = if v.format.is_null() {
        "B".to_owned()
    } else {
        unsafe { std::ffi::CStr::from_ptr(v.format) }
            .to_string_lossy()
            .into_owned()
    };
    let len = v.len.max(0) as usize;
    let itemsize = v.itemsize.max(1) as usize;
    let readonly = v.readonly != 0;
    // Geometry snapshot (the caller's shape/strides arrays are only
    // guaranteed alive now).
    let ndim = v.ndim.max(0) as usize;
    let shape: Vec<usize> = if ndim >= 1 && !v.shape.is_null() {
        (0..ndim)
            .map(|i| unsafe { *v.shape.add(i) }.max(0) as usize)
            .collect()
    } else {
        Vec::new()
    };
    let strides: Vec<isize> = if ndim >= 1 && !v.strides.is_null() {
        (0..ndim)
            .map(|i| unsafe { *v.strides.add(i) } as isize)
            .collect()
    } else if !shape.is_empty() {
        weavepy_vm::object::c_contiguous_strides(&shape, itemsize)
    } else {
        Vec::new()
    };
    let suboffsets: Vec<isize> = if ndim >= 1 && !v.suboffsets.is_null() {
        (0..ndim)
            .map(|i| unsafe { *v.suboffsets.add(i) } as isize)
            .collect()
    } else {
        Vec::new()
    };
    // CPython `PyMemoryView_FromBuffer` treats `view->obj` as either NULL
    // or a *borrowed* reference and stores `master.obj = NULL` — the
    // resulting view must never `PyBuffer_Release` through the exporter
    // (`_testbuffer.memoryview_from_buffer` passes a static Py_buffer whose
    // `obj` still points at the ndarray; releasing it corrupted the export
    // count and crashed the next iteration).
    let mut owned_view = *v;
    owned_view.obj = ptr::null_mut();
    let region = CBufferRegion {
        view: std::cell::Cell::new(owned_view),
        released: std::cell::Cell::new(false),
        released_views: std::cell::Cell::new(0),
        ptr: v.buf as *mut u8,
        len,
        readonly,
    };
    let mv = PyMemoryView {
        buffer: MemoryViewBuffer::Shared(weavepy_vm::sync::Rc::new(region)),
        start: Cell::new(0),
        len: Cell::new(len),
        readonly: Cell::new(readonly),
        released: Cell::new(false),
        format: RefCell::new(format),
        itemsize: Cell::new(itemsize),
        shape: RefCell::new(shape),
        strides: RefCell::new(strides),
        suboffsets: RefCell::new(suboffsets),
        exporter: RefCell::new(None),
        zero_dim: Cell::new(v.ndim == 0),
        hash: Cell::new(-1),
        exports: Cell::new(0),
        release_inner: RefCell::new(None),
        restricted: Cell::new(false),
    };
    crate::object::into_owned(Object::MemoryView(weavepy_vm::sync::Rc::new(mv)))
}

/// `PyMemoryView_GetContiguous(base, buffertype, order)` — a memoryview
/// over `base` that is contiguous in `order` (`'C'`, `'F'` or `'A'`).
/// Mirrors CPython: an already-contiguous exporter is *aliased* (writes
/// propagate when `buffertype` is `PyBUF_WRITE`); a non-contiguous one is
/// gathered into a fresh read-only copy that keeps the source's
/// `shape`/`format`, and a writable request on it is a `BufferError`.
#[no_mangle]
pub unsafe extern "C" fn PyMemoryView_GetContiguous(
    base: *mut PyObject,
    buffertype: c_int,
    order: c_char,
) -> *mut PyObject {
    if base.is_null() {
        crate::errors::set_buffer_error("PyMemoryView_GetContiguous: NULL base");
        return ptr::null_mut();
    }
    let raw = unsafe { PyMemoryView_FromObject(base) };
    if raw.is_null() {
        return ptr::null_mut();
    }
    let Object::MemoryView(mv) = (unsafe { crate::object::clone_object(raw) }) else {
        return raw;
    };
    let want_write = buffertype == 0x200; // PyBUF_WRITE
    let contiguous = match order as u8 {
        b'F' => mv.is_f_contiguous(),
        b'A' => mv.is_c_contiguous() || mv.is_f_contiguous(),
        _ => mv.is_c_contiguous(),
    };
    if contiguous {
        if want_write && mv.readonly.get() {
            unsafe { crate::object::Py_DecRef(raw) };
            crate::errors::set_buffer_error("underlying buffer is not writable");
            return ptr::null_mut();
        }
        return raw;
    }
    if want_write {
        unsafe { crate::object::Py_DecRef(raw) };
        crate::errors::set_buffer_error(
            "writable contiguous buffer requested for a non-contiguous object",
        );
        return ptr::null_mut();
    }
    // Gather a fresh copy in the requested element order, keeping the
    // source geometry (CPython re-derives the strides for `order` over
    // the copied bytes; an empty stored `strides` derives C order).
    let shape = mv.shape_dims();
    let itemsize = mv.itemsize.get().max(1);
    let total: usize = shape.iter().product();
    let mut out = Vec::with_capacity(total * itemsize);
    let ndim = shape.len();
    let mut index = vec![0usize; ndim];
    let fortran = order as u8 == b'F';
    for _ in 0..total {
        mv.read_element(&index, |b| out.extend_from_slice(b));
        if fortran {
            for d in 0..ndim {
                index[d] += 1;
                if index[d] < shape[d] {
                    break;
                }
                index[d] = 0;
            }
        } else {
            for d in (0..ndim).rev() {
                index[d] += 1;
                if index[d] < shape[d] {
                    break;
                }
                index[d] = 0;
            }
        }
    }
    let copy = PyMemoryView::contiguous_1d(
        MemoryViewBuffer::Bytes(out.into()),
        total * itemsize,
        true,
        mv.format.borrow().clone(),
        itemsize,
    );
    if ndim >= 1 {
        *copy.shape.borrow_mut() = shape.clone();
        if fortran {
            // First axis fastest: stride grows along the axes.
            let mut strides = vec![0isize; ndim];
            let mut acc = itemsize as isize;
            for d in 0..ndim {
                strides[d] = acc;
                acc *= shape[d] as isize;
            }
            *copy.strides.borrow_mut() = strides;
        }
    }
    unsafe { crate::object::Py_DecRef(raw) };
    crate::object::into_owned(Object::MemoryView(weavepy_vm::sync::Rc::new(copy)))
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

    // Materialise a fresh Py_buffer on the heap. A `Shared` backing (an
    // aliased C exporter region, mmap, shared memory) hands out the live
    // pointer so C-side writes land in the real buffer; owned backings
    // are copied as before.
    let len = view.len.get();
    let (buf_ptr, owned_buf): (*mut std::ffi::c_void, Option<Box<[u8]>>) = match &view.buffer {
        MemoryViewBuffer::Shared(s) => (
            unsafe { s.data_ptr().add(view.start.get()) } as *mut std::ffi::c_void,
            None,
        ),
        _ => {
            let bytes = view.buffer.with_read(<[u8]>::to_vec);
            let mut buf_box: Box<[u8]> = bytes.into_boxed_slice();
            let p = buf_box.as_mut_ptr() as *mut std::ffi::c_void;
            (p, Some(buf_box))
        }
    };
    let format = view.format.borrow().clone() + "\0";
    let format_storage: Box<[u8]> = format.into_bytes().into_boxed_slice();
    let itemsize = view.itemsize.get().max(1);

    // Element shape/stride: honour an explicit shape, else derive a 1-D
    // `[len / itemsize]` C-contiguous layout so `shape`/`itemsize`/`len`
    // stay self-consistent (`shape[0] == len / itemsize`, not `len`).
    let stored_shape = view.shape.borrow();
    let (shape_box, strides_box): (Box<[PySsizeT]>, Box<[PySsizeT]>) = if stored_shape.is_empty() {
        let n = len.checked_div(itemsize).unwrap_or(0);
        (Box::new([n as PySsizeT]), Box::new([itemsize as PySsizeT]))
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

    let suboffsets_box: Box<[PySsizeT]> = view
        .suboffsets
        .borrow()
        .iter()
        .map(|&s| s as PySsizeT)
        .collect();
    let internal = Box::new(crate::buffer::BufferInternal {
        owned_buf,
        keepalive: None,
        // GET_BUFFER is a *borrow* (CPython's macro reads the view's own
        // Py_buffer cell) — no PyBuffer_Release is owed, so no export pin.
        export_pin: None,
        shape: shape_box,
        strides: strides_box,
        suboffsets: suboffsets_box,
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
        suboffsets: if internal_ref.suboffsets.is_empty() {
            ptr::null_mut()
        } else {
            internal_ref.suboffsets.as_mut_ptr()
        },
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

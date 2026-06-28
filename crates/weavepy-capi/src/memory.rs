//! `PyMem_*` and `PyObject_Malloc/Free`.
//!
//! Extensions allocate scratch buffers via these helpers; we route
//! through the system allocator. CPython has a per-allocator
//! abstraction (raw, mem, object); WeavePy uses the same allocator
//! for all three for now.

use std::alloc::{alloc, alloc_zeroed, dealloc, realloc, Layout};
use std::os::raw::c_int;
use std::ptr;

const ALIGN: usize = if std::mem::align_of::<usize>() > 8 {
    std::mem::align_of::<usize>()
} else {
    8
};

#[repr(C)]
struct AllocHeader {
    size: usize,
}

unsafe fn header_layout() -> Layout {
    Layout::from_size_align(std::mem::size_of::<AllocHeader>(), ALIGN).unwrap()
}

fn alloc_with_header(size: usize, zero: bool) -> *mut u8 {
    let header_layout = unsafe { header_layout() };
    let total = header_layout.size() + size;
    let layout = Layout::from_size_align(total, ALIGN).unwrap();
    let p = if zero {
        unsafe { alloc_zeroed(layout) }
    } else {
        unsafe { alloc(layout) }
    };
    if p.is_null() {
        return ptr::null_mut();
    }
    unsafe {
        (p as *mut AllocHeader).write(AllocHeader { size });
        p.add(header_layout.size())
    }
}

unsafe fn free_with_header(p: *mut u8) {
    if p.is_null() {
        return;
    }
    let header_layout = unsafe { header_layout() };
    let header_ptr = unsafe { p.sub(header_layout.size()) };
    let header = unsafe { (header_ptr as *mut AllocHeader).read() };
    let total = header_layout.size() + header.size;
    let layout = Layout::from_size_align(total, ALIGN).unwrap();
    unsafe { dealloc(header_ptr, layout) };
}

unsafe fn realloc_with_header(p: *mut u8, new_size: usize) -> *mut u8 {
    if p.is_null() {
        return alloc_with_header(new_size, false);
    }
    let header_layout = unsafe { header_layout() };
    let header_ptr = unsafe { p.sub(header_layout.size()) };
    let header = unsafe { (header_ptr as *mut AllocHeader).read() };
    let old_total = header_layout.size() + header.size;
    let new_total = header_layout.size() + new_size;
    let old_layout = Layout::from_size_align(old_total, ALIGN).unwrap();
    let np = unsafe { realloc(header_ptr, old_layout, new_total) };
    if np.is_null() {
        return ptr::null_mut();
    }
    unsafe {
        (np as *mut AllocHeader).write(AllocHeader { size: new_size });
        np.add(header_layout.size())
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyMem_Malloc(n: usize) -> *mut std::ffi::c_void {
    alloc_with_header(n, false) as *mut std::ffi::c_void
}

#[no_mangle]
pub unsafe extern "C" fn PyMem_Calloc(nelem: usize, elsize: usize) -> *mut std::ffi::c_void {
    let total = nelem.saturating_mul(elsize);
    alloc_with_header(total, true) as *mut std::ffi::c_void
}

#[no_mangle]
pub unsafe extern "C" fn PyMem_Realloc(
    p: *mut std::ffi::c_void,
    n: usize,
) -> *mut std::ffi::c_void {
    unsafe { realloc_with_header(p as *mut u8, n) as *mut std::ffi::c_void }
}

#[no_mangle]
pub unsafe extern "C" fn PyMem_Free(p: *mut std::ffi::c_void) {
    unsafe { free_with_header(p as *mut u8) };
}

#[no_mangle]
pub unsafe extern "C" fn PyMem_RawMalloc(n: usize) -> *mut std::ffi::c_void {
    unsafe { PyMem_Malloc(n) }
}

#[no_mangle]
pub unsafe extern "C" fn PyMem_RawCalloc(nelem: usize, elsize: usize) -> *mut std::ffi::c_void {
    unsafe { PyMem_Calloc(nelem, elsize) }
}

#[no_mangle]
pub unsafe extern "C" fn PyMem_RawRealloc(
    p: *mut std::ffi::c_void,
    n: usize,
) -> *mut std::ffi::c_void {
    unsafe { PyMem_Realloc(p, n) }
}

#[no_mangle]
pub unsafe extern "C" fn PyMem_RawFree(p: *mut std::ffi::c_void) {
    unsafe { PyMem_Free(p) };
}

#[no_mangle]
pub unsafe extern "C" fn PyObject_Malloc(n: usize) -> *mut std::ffi::c_void {
    unsafe { PyMem_Malloc(n) }
}

#[no_mangle]
pub unsafe extern "C" fn PyObject_Calloc(nelem: usize, elsize: usize) -> *mut std::ffi::c_void {
    unsafe { PyMem_Calloc(nelem, elsize) }
}

#[no_mangle]
pub unsafe extern "C" fn PyObject_Realloc(
    p: *mut std::ffi::c_void,
    n: usize,
) -> *mut std::ffi::c_void {
    unsafe { PyMem_Realloc(p, n) }
}

#[no_mangle]
pub unsafe extern "C" fn PyObject_Free(p: *mut std::ffi::c_void) {
    // RFC 0045 (wave 3): a faithful inline *instance body* is owned by its
    // native instance, not by C's allocator. A stock `tp_dealloc` that
    // ends with `tp_free(self)` / `PyObject_Free(self)` must be absorbed —
    // the block is reclaimed when the owning instance is collected, and
    // freeing it here (it has no `PyMem` allocation header) would corrupt
    // the heap. The check is strict (mirror magic + `Weak` back-ref), so
    // it never mistakes a genuine `PyObject_Free` scratch buffer for one.
    if !p.is_null() && unsafe { crate::mirror::is_instance_body(p as *mut crate::object::PyObject) }
    {
        return;
    }
    unsafe { PyMem_Free(p) };
}

/// `Py_AtExit` — accept and silently drop. Real cleanup happens
/// when the host binary exits.
#[no_mangle]
pub unsafe extern "C" fn Py_AtExit(_func: Option<unsafe extern "C" fn()>) -> c_int {
    0
}

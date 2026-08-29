//! `PyMem_*` and `PyObject_Malloc/Free`.
//!
//! Extensions allocate scratch buffers via these helpers; we route
//! straight through the **system allocator** (`malloc`/`free`), exactly
//! like CPython built with `PYTHONMALLOC=malloc` (and matching release
//! pymalloc's observable contract, whose `address_in_range` check makes
//! `PyObject_Free` accept any malloc-family pointer).
//!
//! Faithfulness matters more than it looks: wheels *mix* allocators
//! across module boundaries. pandas' Cython code `Py_DECREF`s objects
//! whose `tp_dealloc` funnels into `PyObject_Free`, numpy hands
//! `PyDataMem_NEW` (plain `malloc`) buffers to code that releases them
//! with `PyMem_Free`, and khash headers pair `PyMem_Malloc` with plain
//! `free`. A private header-prefixed allocator (WeavePy's previous
//! scheme) corrupts the heap on every such crossing — reading a garbage
//! "size" at `p - 8` and, when `p` happens to sit at the start of an
//! mmap'd region, faulting outright (SIGBUS in pandas'
//! `get_indexer_non_unique` under `test_loc.py`). The system allocator
//! needs no size bookkeeping, so any pointer from any malloc-family
//! source is freeable everywhere.

extern "C" {
    fn malloc(size: usize) -> *mut std::ffi::c_void;
    fn calloc(nelem: usize, elsize: usize) -> *mut std::ffi::c_void;
    fn realloc(p: *mut std::ffi::c_void, size: usize) -> *mut std::ffi::c_void;
    fn free(p: *mut std::ffi::c_void);
}

#[no_mangle]
pub unsafe extern "C" fn PyMem_Malloc(n: usize) -> *mut std::ffi::c_void {
    // CPython guarantees a unique, freeable pointer for n == 0.
    unsafe { malloc(n.max(1)) }
}

#[no_mangle]
pub unsafe extern "C" fn PyMem_Calloc(nelem: usize, elsize: usize) -> *mut std::ffi::c_void {
    // calloc(0, x) may return NULL on some platforms; normalise like CPython.
    let (n, e) = if nelem == 0 || elsize == 0 {
        (1, 1)
    } else {
        (nelem, elsize)
    };
    unsafe { calloc(n, e) }
}

#[no_mangle]
pub unsafe extern "C" fn PyMem_Realloc(
    p: *mut std::ffi::c_void,
    n: usize,
) -> *mut std::ffi::c_void {
    unsafe { realloc(p, n.max(1)) }
}

#[no_mangle]
pub unsafe extern "C" fn PyMem_Free(p: *mut std::ffi::c_void) {
    if p.is_null() {
        return;
    }
    unsafe { free(p) };
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
    // freeing it here (it was minted through Rust's allocator with a
    // negative-offset prefix) would corrupt the heap. The check is strict
    // (mirror magic + `Weak` back-ref), so it never mistakes a genuine
    // `PyObject_Free` scratch buffer for one.
    if !p.is_null() && unsafe { crate::mirror::is_instance_body(p as *mut crate::object::PyObject) }
    {
        crate::instance::note_body_free_consented(p as usize);
        return;
    }
    // A WeavePy-minted *object* (box or mirror) must never reach the raw
    // system `free` — boxes are `Box`-allocated with a Rust layout and
    // mirrors carry the negative-offset prefix. A stock `tp_dealloc`
    // chain (`_Py_Dealloc` → extension dealloc → `tp_free` == this
    // function) can land one here; route it through the owning release
    // path instead.
    if !p.is_null() && crate::object::is_weavepy_owned(p as *mut crate::object::PyObject) {
        unsafe { crate::object::free_owned_storage(p as *mut crate::object::PyObject) };
        return;
    }
    unsafe { PyMem_Free(p) };
}

// `Py_AtExit` lives in `crate::embed` since RFC 0075: registered
// callbacks run LIFO inside the real `Py_FinalizeEx`.

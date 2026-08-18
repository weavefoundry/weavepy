//! RFC 0047 (wave 5): a byte-faithful `PyThreadState` backing store.
//!
//! Genuine Cython output compiled against stock CPython 3.13 sets
//! `CYTHON_FAST_THREAD_STATE 1`, which makes its error machinery
//! (`__Pyx_ErrFetchInState` / `__Pyx_ErrRestoreInState` /
//! `__Pyx_PyErr_Occurred`) read and write **`tstate->current_exception`
//! directly** at the field's fixed struct offset, bypassing the
//! `PyErr_*` call surface entirely. It also reads `tstate->interp`
//! (`__Pyx_check_single_interpreter`).
//!
//! WeavePy previously handed out a one-byte sentinel from
//! `PyThreadState_Get`, which works only as long as nothing dereferences
//! it. To run real Cython we expose a thread-local store laid out like
//! CPython's `PyThreadState` — at minimum a readable `interp` slot and a
//! readable/writable `current_exception` slot at the correct offsets —
//! and we make [`crate::errors`] treat that `current_exception` slot as
//! the single source of truth for the pending exception. That unification
//! is what lets an exception raised by a WeavePy C-API call be *seen* by
//! Cython's inlined `current_exception` read, and an exception stashed by
//! Cython be seen by WeavePy.
//!
//! The store is intentionally over-sized and zeroed: every field Cython
//! might touch lands inside it, and a zeroed `interp`/`current_exception`
//! is the correct "no interpreter id / no error" initial state.

#![allow(clippy::missing_safety_doc)]

use core::cell::UnsafeCell;
use core::ffi::{c_int, c_void};
use std::ptr;

use crate::lifecycle::PyThreadState;
use crate::object::PyObject;

// CPython 3.13 `struct _ts` field offsets (machine-checked against stock
// `cpython/pystate.h`; see the layout walk in the wave-5 work log).
const OFF_INTERP: usize = 16; // PyInterpreterState *interp
const OFF_C_RECURSION_REMAINING: usize = 52; // int c_recursion_remaining
const OFF_CURRENT_EXCEPTION: usize = 112; // PyObject *current_exception
const OFF_EXC_INFO: usize = 120; // _PyErr_StackItem *exc_info
const OFF_GILSTATE_COUNTER: usize = 136; // int gilstate_counter
const OFF_DELETE_LATER: usize = 168; // PyObject *delete_later

/// Initial `c_recursion_remaining`. mypyc's `Py_TRASHCAN_BEGIN` expansion
/// reads/writes the field *directly* and deposits the object (deferring its
/// dealloc) whenever the remaining budget is ≤ 50 — a zeroed field would
/// push *every* dealloc through the trashcan and never drain it. CPython
/// 3.13 initialises it to `Py_C_RECURSION_LIMIT`; the precise figure only
/// bounds native dealloc recursion.
const C_RECURSION_BUDGET: i32 = 4000;

/// Generously sized backing body. The real 3.13 `PyThreadState` is well
/// under this; the slack guarantees any in-struct field write Cython emits
/// stays in-bounds.
const TS_BYTES: usize = 1024;

/// `_PyErr_StackItem { PyObject *exc_value; struct _err_stackitem *previous_item; }`.
/// `tstate->exc_info` must be non-NULL (CPython guarantees it), so we point
/// it at this per-thread item. WeavePy does not model the handled-exception
/// stack, so it stays empty.
#[repr(C)]
struct StackItem {
    exc_value: *mut PyObject,
    previous_item: *mut c_void,
}

#[repr(C, align(16))]
struct TStateStore {
    body: [u8; TS_BYTES],
    exc_info: StackItem,
    initialized: bool,
}

thread_local! {
    static TSTATE: UnsafeCell<TStateStore> = const {
        UnsafeCell::new(TStateStore {
            body: [0u8; TS_BYTES],
            exc_info: StackItem {
                exc_value: ptr::null_mut(),
                previous_item: ptr::null_mut(),
            },
            initialized: false,
        })
    };
}

/// Return the current thread's faithful `PyThreadState` body, wiring the
/// `exc_info` self-pointer on first touch. The returned pointer is stable
/// for the life of the thread.
fn store_ptr() -> *mut TStateStore {
    TSTATE.with(|cell| {
        let store = cell.get();
        unsafe {
            if !(*store).initialized {
                (*store).initialized = true;
                // exc_info (offset 120) points at the embedded StackItem.
                let exc_info_ptr = ptr::addr_of_mut!((*store).exc_info) as *mut c_void;
                let body = (*store).body.as_mut_ptr();
                ptr::write_unaligned(body.add(OFF_EXC_INFO) as *mut *mut c_void, exc_info_ptr);
                ptr::write_unaligned(
                    body.add(OFF_C_RECURSION_REMAINING) as *mut i32,
                    C_RECURSION_BUDGET,
                );
                // RFC 0066 WS3: `tstate->interp` must be the same live
                // handle `PyInterpreterState_Get`/`_Main` return. pybind11's
                // `get_python_state_dict` reads it *directly*
                // (`PyThreadState_GetUnchecked()->interp`) to reach
                // `PyInterpreterState_GetDict`; a zeroed slot made internals
                // bootstrap fail and crash scipy's `_highspy._core` init.
                ptr::write_unaligned(
                    body.add(OFF_INTERP) as *mut *mut c_void,
                    crate::abi313::PyInterpreterState_Get(),
                );
                // RFC 0066 WS3: a thread bound to its state starts at
                // `gilstate_counter == 1` (CPython's
                // `_PyGILState_NoteThreadState`). pybind11's
                // `gil_scoped_acquire` inc/decs the field *directly*, and
                // treats a decrement that reaches 0 as "this guard created
                // the thread state and must delete it" — from a zeroed
                // slot, the very first guard pair aborted with
                // "scoped_acquire::dec_ref(): internal error!".
                ptr::write_unaligned(body.add(OFF_GILSTATE_COUNTER) as *mut c_int, 1);
            }
        }
        store
    })
}

/// `*mut PyThreadState` for the current thread (the body pointer).
pub fn current_threadstate() -> *mut PyThreadState {
    let store = store_ptr();
    unsafe { (*store).body.as_mut_ptr() as *mut PyThreadState }
}

/// Pointer to this thread's `current_exception` field — the canonical
/// pending-exception cell shared with Cython's inlined access.
pub fn current_exception_slot() -> *mut *mut PyObject {
    let store = store_ptr();
    unsafe { (*store).body.as_mut_ptr().add(OFF_CURRENT_EXCEPTION) as *mut *mut PyObject }
}

/// Pointer to this thread's handled-exception (`exc_info->exc_value`) slot,
/// holding NULL or one owned reference. `PyErr_GetExcInfo`/`SetExcInfo`
/// (mypyc's try/finally save-restore) read and write it.
pub fn exc_info_value_slot() -> *mut *mut PyObject {
    let store = store_ptr();
    unsafe { ptr::addr_of_mut!((*store).exc_info.exc_value) }
}

/// Pointer to this thread's `delete_later` field. mypyc's `Py_TRASHCAN_END`
/// expansion reads the field directly to decide whether to call
/// `_PyTrash_thread_destroy_chain`, so the deposit/destroy pair in
/// [`crate::mypyc_tail`] must keep it accurate.
pub fn delete_later_slot() -> *mut *mut PyObject {
    let store = store_ptr();
    unsafe { (*store).body.as_mut_ptr().add(OFF_DELETE_LATER) as *mut *mut PyObject }
}

// ---------------------------------------------------------------------------
// FFI
// ---------------------------------------------------------------------------

/// `PyThreadState_GetUnchecked()` — the non-asserting current-thread-state
/// accessor (3.13). Cython's `__Pyx_PyThreadState_Current` resolves to this.
#[no_mangle]
pub unsafe extern "C" fn PyThreadState_GetUnchecked() -> *mut PyThreadState {
    crate::interp::ensure_initialised();
    current_threadstate()
}

/// `PyThreadState_New(interp)` — hand back the calling thread's faithful
/// per-thread body (RFC 0066 WS3). CPython mints a fresh tstate bound to
/// the interpreter; WeavePy's per-thread store *is* that state, and the
/// callers (pybind11's `gil_scoped_acquire` bootstrap for threads it
/// created itself) only pair it with `PyThreadState_DeleteCurrent`.
#[no_mangle]
pub unsafe extern "C" fn PyThreadState_New(_interp: *mut c_void) -> *mut PyThreadState {
    crate::interp::ensure_initialised();
    current_threadstate()
}

/// `PyThreadState_DeleteCurrent()` — the per-thread body is a
/// thread-local reclaimed with the OS thread; explicit deletion is a
/// sound no-op.
#[no_mangle]
pub unsafe extern "C" fn PyThreadState_DeleteCurrent() {}

// ---------------------------------------------------------------------------
// PyThread_tss_* — CPython's TSS (thread-specific storage) API
// (RFC 0066 WS3). pybind11's `internals` constructor creates a TSS key
// through `PYBIND11_TLS_KEY_CREATE` (= `PyThread_tss_create`) and every
// `gil_scoped_acquire` reads it back. The extension links with
// `-undefined dynamic_lookup`, so a missing symbol binds to NULL
// *silently* and the first call jumps to address zero — the exact crash
// scipy's `_highspy._core` init hit. Faithful shape: CPython's
// `Py_tss_t` is `{ int _is_initialized; pthread_key_t _key; }` on POSIX
// (`thread_pthread.h`) and `{ int _is_initialized; DWORD _key; }` on
// Windows (`thread_nt.h`, keys from `TlsAlloc`).
// ---------------------------------------------------------------------------

/// The native key half of CPython's `Py_tss_t`.
#[cfg(windows)]
type NativeTssKey = u32; // DWORD from TlsAlloc
#[cfg(not(windows))]
type NativeTssKey = libc::pthread_key_t;

/// CPython's `Py_tss_t` (platform layout, see module comment).
#[repr(C)]
pub struct PyTssT {
    is_initialized: c_int,
    key: NativeTssKey,
}

#[cfg(windows)]
mod win_tls {
    pub const TLS_OUT_OF_INDEXES: u32 = 0xFFFF_FFFF;
    #[link(name = "kernel32")]
    extern "system" {
        pub fn TlsAlloc() -> u32;
        pub fn TlsFree(dw_tls_index: u32) -> i32;
        pub fn TlsSetValue(dw_tls_index: u32, lp_tls_value: *mut core::ffi::c_void) -> i32;
        pub fn TlsGetValue(dw_tls_index: u32) -> *mut core::ffi::c_void;
    }
}

fn native_tss_create(key: &mut NativeTssKey) -> bool {
    #[cfg(windows)]
    {
        let k = unsafe { win_tls::TlsAlloc() };
        if k == win_tls::TLS_OUT_OF_INDEXES {
            return false;
        }
        *key = k;
        true
    }
    #[cfg(not(windows))]
    {
        unsafe { libc::pthread_key_create(key, None) == 0 }
    }
}

fn native_tss_delete(key: NativeTssKey) {
    #[cfg(windows)]
    unsafe {
        win_tls::TlsFree(key);
    }
    #[cfg(not(windows))]
    unsafe {
        libc::pthread_key_delete(key);
    }
}

fn native_tss_set(key: NativeTssKey, value: *mut c_void) -> bool {
    #[cfg(windows)]
    {
        unsafe { win_tls::TlsSetValue(key, value) != 0 }
    }
    #[cfg(not(windows))]
    {
        unsafe { libc::pthread_setspecific(key, value) == 0 }
    }
}

fn native_tss_get(key: NativeTssKey) -> *mut c_void {
    #[cfg(windows)]
    {
        unsafe { win_tls::TlsGetValue(key) }
    }
    #[cfg(not(windows))]
    {
        unsafe { libc::pthread_getspecific(key) as *mut c_void }
    }
}

/// `PyThread_tss_create(key)` — 0 on success. Idempotent on an
/// already-created key, per CPython.
#[no_mangle]
pub unsafe extern "C" fn PyThread_tss_create(key: *mut PyTssT) -> c_int {
    if key.is_null() {
        return -1;
    }
    unsafe {
        if (*key).is_initialized != 0 {
            return 0;
        }
        if !native_tss_create(&mut (*key).key) {
            return -1;
        }
        (*key).is_initialized = 1;
    }
    0
}

/// `PyThread_tss_delete(key)` — no-op on a never-created key, per CPython.
#[no_mangle]
pub unsafe extern "C" fn PyThread_tss_delete(key: *mut PyTssT) {
    if key.is_null() {
        return;
    }
    unsafe {
        if (*key).is_initialized != 0 {
            native_tss_delete((*key).key);
            (*key).is_initialized = 0;
            (*key).key = 0;
        }
    }
}

/// `PyThread_tss_set(key, value)` — 0 on success.
#[no_mangle]
pub unsafe extern "C" fn PyThread_tss_set(key: *mut PyTssT, value: *mut c_void) -> c_int {
    if key.is_null() || unsafe { (*key).is_initialized } == 0 {
        return -1;
    }
    if !native_tss_set(unsafe { (*key).key }, value) {
        return -1;
    }
    0
}

/// `PyThread_tss_get(key)` — NULL when unset or the key was never created.
#[no_mangle]
pub unsafe extern "C" fn PyThread_tss_get(key: *mut PyTssT) -> *mut c_void {
    if key.is_null() || unsafe { (*key).is_initialized } == 0 {
        return ptr::null_mut();
    }
    native_tss_get(unsafe { (*key).key })
}

/// `PyThread_tss_is_created(key)`.
#[no_mangle]
pub unsafe extern "C" fn PyThread_tss_is_created(key: *mut PyTssT) -> c_int {
    if key.is_null() {
        return 0;
    }
    (unsafe { (*key).is_initialized } != 0) as c_int
}

/// `PyThread_tss_alloc()` — a zeroed (not-created) key, freed with
/// [`PyThread_tss_free`].
#[no_mangle]
pub unsafe extern "C" fn PyThread_tss_alloc() -> *mut PyTssT {
    unsafe { libc::calloc(1, std::mem::size_of::<PyTssT>()) as *mut PyTssT }
}

/// `PyThread_tss_free(key)` — delete then release.
#[no_mangle]
pub unsafe extern "C" fn PyThread_tss_free(key: *mut PyTssT) {
    if key.is_null() {
        return;
    }
    unsafe {
        PyThread_tss_delete(key);
        libc::free(key as *mut c_void);
    }
}

/// `PyThread_get_thread_native_id()` — the OS-assigned id of the calling
/// thread (bpo-36084). Exported so `ctypes.pythonapi` finds it, mirroring
/// CPython builds where `PY_HAVE_THREAD_NATIVE_ID` is defined.
#[no_mangle]
pub unsafe extern "C" fn PyThread_get_thread_native_id() -> libc::c_ulong {
    #[cfg(target_os = "macos")]
    {
        let mut tid: u64 = 0;
        unsafe {
            libc::pthread_threadid_np(0, &raw mut tid);
        }
        tid as libc::c_ulong
    }
    #[cfg(target_os = "linux")]
    {
        (unsafe { libc::syscall(libc::SYS_gettid) }) as libc::c_ulong
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        0
    }
}

/// `Py_FrozenMain(argc, argv)` — CPython's entry point for frozen
/// binaries (bpo-44133 requires the symbol to be exported). WeavePy
/// hosts no frozen `__main__` table, which is exactly the state of a
/// CPython binary with an empty `PyImport_FrozenModules`: report the
/// import failure and exit non-zero.
#[no_mangle]
pub unsafe extern "C" fn Py_FrozenMain(_argc: c_int, _argv: *mut *mut libc::c_char) -> c_int {
    let msg = b"Unable to import __main__: no frozen modules are registered\n";
    unsafe {
        libc::write(2, msg.as_ptr().cast::<c_void>(), msg.len());
    }
    1
}

/// `PyInterpreterState_GetID(interp)` — WeavePy is single-interpreter, so
/// the id is always 0. The argument (which Cython derives from
/// `tstate->interp`, currently a zeroed/NULL slot) is intentionally ignored.
#[no_mangle]
pub unsafe extern "C" fn PyInterpreterState_GetID(_interp: *mut c_void) -> i64 {
    0
}

/// `PyGC_Enable()` / `PyGC_Disable()` — return the *previous* enabled flag.
/// WeavePy's collector isn't toggled through this C entry; report "was
/// enabled" (1) so Cython's save/restore bookkeeping is internally
/// consistent, and otherwise no-op.
#[no_mangle]
pub unsafe extern "C" fn PyGC_Enable() -> c_int {
    1
}

#[no_mangle]
pub unsafe extern "C" fn PyGC_Disable() -> c_int {
    1
}

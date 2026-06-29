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
const OFF_CURRENT_EXCEPTION: usize = 112; // PyObject *current_exception
const OFF_EXC_INFO: usize = 120; // _PyErr_StackItem *exc_info

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

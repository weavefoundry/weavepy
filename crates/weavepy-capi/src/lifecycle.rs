//! Lifecycle: `Py_Initialize`, version helpers, GIL stubs.
//!
//! Most of these are no-ops in the WeavePy world: the host binary
//! is the interpreter, so by the time an extension's `PyInit_*`
//! function runs everything is already up. But the symbols still
//! have to exist for legacy extensions that call them defensively.

use std::os::raw::{c_char, c_int};

static VERSION: &str = "3.13.0 (WeavePy)\0";
static COMPILER: &str = "[WeavePy/Rust]\0";
static COPYRIGHT: &str = "Copyright (c) 2026 Weave Foundry. PSF licensed.\0";
static PLATFORM: &str = if cfg!(target_os = "macos") {
    "darwin\0"
} else if cfg!(target_os = "linux") {
    "linux\0"
} else if cfg!(target_os = "windows") {
    "win32\0"
} else {
    "unknown\0"
};
static BUILD_INFO: &str = "default, weavepy\0";

#[no_mangle]
pub unsafe extern "C" fn Py_Initialize() {
    crate::interp::ensure_initialised();
}

#[no_mangle]
pub unsafe extern "C" fn Py_InitializeEx(_init_sigs: c_int) {
    crate::interp::ensure_initialised();
}

#[no_mangle]
pub unsafe extern "C" fn Py_FinalizeEx() -> c_int {
    0
}

#[no_mangle]
pub unsafe extern "C" fn Py_Finalize() {}

#[no_mangle]
pub unsafe extern "C" fn Py_IsInitialized() -> c_int {
    1
}

#[no_mangle]
pub unsafe extern "C" fn Py_GetVersion() -> *const c_char {
    VERSION.as_ptr() as *const c_char
}

#[no_mangle]
pub unsafe extern "C" fn Py_GetCompiler() -> *const c_char {
    COMPILER.as_ptr() as *const c_char
}

#[no_mangle]
pub unsafe extern "C" fn Py_GetCopyright() -> *const c_char {
    COPYRIGHT.as_ptr() as *const c_char
}

#[no_mangle]
pub unsafe extern "C" fn Py_GetPlatform() -> *const c_char {
    PLATFORM.as_ptr() as *const c_char
}

#[no_mangle]
pub unsafe extern "C" fn Py_GetBuildInfo() -> *const c_char {
    BUILD_INFO.as_ptr() as *const c_char
}

// ----------------------------------------------------------------
// GIL — wired to `weavepy_vm::gil::global_gil()` (RFC 0024).
// ----------------------------------------------------------------
//
// We replicate the CPython contract:
//
// - `PyGILState_Ensure()` acquires the GIL if the calling thread
//   doesn't already hold it. Returns an opaque token that
//   `PyGILState_Release` consumes. `1` means "this call acquired
//   the GIL"; `0` means "the calling thread already held it" and
//   `Release` is a no-op.
// - `PyGILState_Release(state)` undoes whatever `Ensure` did.
// - `PyGILState_Check()` returns 1 if the calling thread holds
//   the GIL, 0 otherwise.
// - `PyEval_SaveThread()` releases the GIL and stashes a thread
//   state for `PyEval_RestoreThread()` to re-acquire later. Used
//   to wrap blocking C-side I/O.
//
// The per-thread "do I currently hold the GIL?" flag and the
// stashed `GilGuard` live in `THREAD_GIL` below.

use weavepy_vm::gil::{current_thread_holds_gil, global_gil, pop_gil_guard, push_gil_guard};

fn currently_holding() -> bool {
    current_thread_holds_gil()
}

#[no_mangle]
pub unsafe extern "C" fn PyGILState_Ensure() -> c_int {
    if currently_holding() {
        // Already hold it (reentrant); release will be a no-op.
        return 0;
    }
    push_gil_guard(global_gil().acquire());
    1
}

#[no_mangle]
pub unsafe extern "C" fn PyGILState_Release(state: c_int) {
    if state != 1 {
        return;
    }
    let _ = pop_gil_guard();
}

#[no_mangle]
pub unsafe extern "C" fn PyGILState_Check() -> c_int {
    if currently_holding() {
        1
    } else {
        0
    }
}

#[repr(C)]
pub struct PyThreadState {
    _opaque: [u8; 0],
}

#[no_mangle]
pub unsafe extern "C" fn PyThreadState_Get() -> *mut PyThreadState {
    // We don't yet have per-thread state objects; return a
    // sentinel. The interpreter doesn't dereference this.
    static mut DUMMY: u8 = 0;
    &raw mut DUMMY as *mut PyThreadState
}

#[no_mangle]
pub unsafe extern "C" fn PyThreadState_Swap(new_state: *mut PyThreadState) -> *mut PyThreadState {
    // RFC 0025: real per-thread state management goes through
    // the `crate::gil::push_gil_guard` stack. Swap is a no-op
    // here — the calling thread keeps its current GIL guard on
    // the stack — and we return the same sentinel that
    // `PyThreadState_Get` would return, satisfying CPython's
    // "the previous tstate" contract for the common case where
    // callers only check non-null.
    let _ = new_state;
    PyThreadState_Get()
}

#[no_mangle]
pub unsafe extern "C" fn PyEval_SaveThread() -> *mut PyThreadState {
    // Drop the GIL. Returns a token that
    // `PyEval_RestoreThread` consumes to re-acquire. We
    // represent the token as the count of guards we just
    // popped, encoded into the pointer. Anything non-null
    // restores; null means "we weren't holding the GIL".
    let popped = pop_gil_guard();
    if popped.is_some() {
        // Encode "1 guard popped" as a dangling sentinel; the
        // matching `RestoreThread` call only checks for non-null.
        std::ptr::dangling_mut::<PyThreadState>()
    } else {
        std::ptr::null_mut()
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyEval_RestoreThread(t: *mut PyThreadState) {
    if t.is_null() {
        return;
    }
    push_gil_guard(global_gil().acquire());
}

#[no_mangle]
pub unsafe extern "C" fn PyEval_AcquireThread(_state: *mut PyThreadState) {
    // RFC 0025: equivalent to `PyEval_RestoreThread` in our
    // current single-interpreter model. The C-API distinguishes
    // these for sub-interpreter symmetry; we route both through
    // the same global GIL.
    if !currently_holding() {
        push_gil_guard(global_gil().acquire());
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyEval_ReleaseThread(_state: *mut PyThreadState) {
    // Paired with `PyEval_AcquireThread`.
    let _ = pop_gil_guard();
}

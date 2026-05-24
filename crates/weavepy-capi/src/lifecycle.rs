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
// GIL stubs (single-threaded today).
// ----------------------------------------------------------------

#[no_mangle]
pub unsafe extern "C" fn PyGILState_Ensure() -> c_int {
    0
}

#[no_mangle]
pub unsafe extern "C" fn PyGILState_Release(_state: c_int) {}

#[no_mangle]
pub unsafe extern "C" fn PyGILState_Check() -> c_int {
    1
}

#[repr(C)]
pub struct PyThreadState {
    _opaque: [u8; 0],
}

#[no_mangle]
pub unsafe extern "C" fn PyThreadState_Get() -> *mut PyThreadState {
    static mut DUMMY: u8 = 0;
    &raw mut DUMMY as *mut PyThreadState
}

#[no_mangle]
pub unsafe extern "C" fn PyEval_SaveThread() -> *mut PyThreadState {
    unsafe { PyThreadState_Get() }
}

#[no_mangle]
pub unsafe extern "C" fn PyEval_RestoreThread(_t: *mut PyThreadState) {}

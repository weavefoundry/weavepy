//! RFC 0075 WS1–WS3 — the embedding lifecycle, driven exactly the way
//! a C embedder drives it: through the `extern "C"` entry points, with
//! no WeavePy host in the process.
//!
//! Everything lives in one `#[test]` because the lifecycle is
//! process-global state: parallel test threads doing init/fini would
//! race each other in ways no real embedder does.

#![allow(non_snake_case)]

use std::ffi::CString;
use std::os::raw::c_int;

use weavepy_capi::initconfig::{
    PyConfig, PyConfig_Clear, PyConfig_InitPythonConfig, PyConfig_SetBytesArgv, PyStatus_Exception,
    Py_InitializeFromConfig,
};
use weavepy_capi::lifecycle::{Py_Finalize, Py_FinalizeEx, Py_Initialize, Py_IsInitialized};
use weavepy_capi::pythonrun::{PyRun_SimpleString, PyRun_String, Py_eval_input};

static ATEXIT_FIRED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

unsafe extern "C" fn note_atexit() {
    ATEXIT_FIRED.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
}

fn run_simple(src: &str) -> c_int {
    let c = CString::new(src).unwrap();
    unsafe { PyRun_SimpleString(c.as_ptr()) }
}

/// Evaluate an expression in `__main__` and return its `repr`-ish
/// int value via `PyLong_AsLong`, or `None` when evaluation failed.
fn eval_int(expr: &str) -> Option<i64> {
    let main = CString::new("__main__").unwrap();
    let module = unsafe { weavepy_capi::module::PyImport_AddModule(main.as_ptr()) };
    assert!(!module.is_null(), "PyImport_AddModule(__main__) failed");
    let dict = unsafe { weavepy_capi::module::PyModule_GetDict(module) };
    assert!(!dict.is_null(), "PyModule_GetDict failed");
    let c = CString::new(expr).unwrap();
    let result = unsafe { PyRun_String(c.as_ptr(), Py_eval_input, dict, dict) };
    if result.is_null() {
        // Clear the pending error the way an embedder would.
        unsafe { weavepy_capi::errors::PyErr_Clear() };
        return None;
    }
    let value = unsafe { weavepy_capi::numbers::PyLong_AsLong(result) };
    unsafe { weavepy_capi::object::Py_DecRef(result) };
    Some(value)
}

#[test]
fn embedding_lifecycle_end_to_end() {
    // --- Round 1: plain Py_Initialize ------------------------------
    unsafe { Py_Initialize() };
    assert_eq!(unsafe { Py_IsInitialized() }, 1, "initialized after init");

    assert_eq!(run_simple("x = 20 + 22"), 0, "simple exec succeeds");
    assert_eq!(eval_int("x"), Some(42), "state visible across PyRun calls");

    // A failing PyRun_SimpleString reports -1 (and prints a traceback).
    assert_eq!(run_simple("raise ValueError('embedding')"), -1);

    // Py_AtExit registration (post-init is CPython's documented shape).
    assert_eq!(
        unsafe { weavepy_capi::embed::Py_AtExit(Some(note_atexit)) },
        0
    );

    assert_eq!(unsafe { Py_FinalizeEx() }, 0, "finalize succeeds");
    assert_eq!(unsafe { Py_IsInitialized() }, 0, "uninitialized after fini");
    assert_eq!(
        ATEXIT_FIRED.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "Py_AtExit callback ran during finalize"
    );

    // --- Round 2: re-init is a *fresh* interpreter -----------------
    unsafe { Py_Initialize() };
    assert_eq!(unsafe { Py_IsInitialized() }, 1);
    assert_eq!(
        eval_int("x"),
        None,
        "previous round's __main__ binding must not leak into a fresh init"
    );
    assert_eq!(run_simple("import sys; y = len(sys.path)"), 0);
    assert!(eval_int("y").is_some());
    unsafe { Py_Finalize() };
    assert_eq!(unsafe { Py_IsInitialized() }, 0);

    // --- Round 3: Py_InitializeFromConfig with argv ----------------
    unsafe {
        let mut config: PyConfig = std::mem::zeroed();
        PyConfig_InitPythonConfig(&raw mut config);
        let args = [
            CString::new("embedder").unwrap(),
            CString::new("first-arg").unwrap(),
        ];
        let argv: Vec<*mut std::os::raw::c_char> =
            args.iter().map(|a| a.as_ptr().cast_mut()).collect();
        let st = PyConfig_SetBytesArgv(&raw mut config, argv.len() as isize, argv.as_ptr());
        assert_eq!(PyStatus_Exception(st), 0, "SetBytesArgv ok");
        // parse_argv consumes argv[0]; no -c/-m/script means plain
        // interpreter argv.
        let st = Py_InitializeFromConfig(&raw const config);
        assert_eq!(PyStatus_Exception(st), 0, "InitializeFromConfig ok");
        PyConfig_Clear(&raw mut config);
    }
    assert_eq!(unsafe { Py_IsInitialized() }, 1);
    assert_eq!(run_simple("import sys; n = len(sys.argv)"), 0);
    assert!(eval_int("n").is_some(), "sys.argv populated from config");
    assert_eq!(unsafe { Py_FinalizeEx() }, 0);
}

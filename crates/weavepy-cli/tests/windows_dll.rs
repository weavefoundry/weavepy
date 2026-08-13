//! The `python313.dll` contract, end to end (RFC 0064 WS5).
//!
//! On Windows `weavepy.exe` is a thin shim over `python313.dll` — the
//! runtime, and the import target every `.pyd`'s PE header names. This
//! battery is the smoke half of the POSIX `force_link_completeness`
//! contract, adapted to the PE world:
//!
//! 1. the DLL exists next to the exe and loads;
//! 2. `GetProcAddress` resolves the embedding entry points and a
//!    curated sample spanning the export families — including the
//!    `varargs.c` symbols that need explicit `/EXPORT`s (a regression
//!    here means the MSVC export plumbing in `weavepy-pylib/build.rs`
//!    broke);
//! 3. the shim runs Python *through* the DLL (`sys.dllhandle` is the
//!    real HMODULE);
//! 4. `os.add_dll_directory` round-trips (WS2);
//! 5. a broken `.pyd` raises CPython's exact
//!    `ImportError: DLL load failed while importing …` shape.
//!
//! CI builds `-p weavepy-pylib` alongside the workspace, so the DLL is
//! always present there; a missing DLL fails loudly with the build
//! command rather than skipping.
#![cfg(windows)]

use std::path::{Path, PathBuf};
use std::process::Command;

fn exe_path() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_weavepy"))
}

/// `python313.dll` sits next to the exe (both land in
/// `target/<profile>/`).
fn dll_path() -> PathBuf {
    let dll = exe_path()
        .parent()
        .expect("exe path has a parent")
        .join("python313.dll");
    assert!(
        dll.is_file(),
        "python313.dll not found at {} — build it with \
         `cargo build -p weavepy-pylib` (same profile as this test)",
        dll.display()
    );
    dll
}

fn load_dll(path: &Path) -> *mut core::ffi::c_void {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::System::LibraryLoader::LoadLibraryExW;
    let wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let handle = unsafe { LoadLibraryExW(wide.as_ptr(), std::ptr::null_mut(), 0) };
    assert!(
        !handle.is_null(),
        "LoadLibraryExW({}) failed with Win32 error {}",
        path.display(),
        unsafe { windows_sys::Win32::Foundation::GetLastError() }
    );
    handle
}

/// The export families, sampled: numbers, strings, containers,
/// modules, errors, abstract, types, capsules, GIL/lifecycle,
/// singletons (a `#[no_mangle] static`), the embedding entry points,
/// and the `varargs.c` set that rides `/EXPORT` linker args.
const SYMBOL_SAMPLE: &[&str] = &[
    // embedding entry points (weavepy-pylib itself)
    "weavepy_main",
    "Py_Main",
    "Py_BytesMain",
    // lifecycle / GIL
    "Py_Initialize",
    "Py_IsInitialized",
    "PyGILState_Ensure",
    "PyGILState_Release",
    "PyEval_SaveThread",
    "PyEval_RestoreThread",
    // numbers
    "PyLong_FromLong",
    "PyLong_AsLong",
    "PyFloat_FromDouble",
    // strings / bytes
    "PyUnicode_FromString",
    "PyBytes_FromStringAndSize",
    // containers
    "PyTuple_New",
    "PyList_New",
    "PyList_Append",
    "PyDict_New",
    "PyDict_SetItemString",
    // modules / import
    "PyModule_Create2",
    "PyModule_GetDict",
    "PyImport_ImportModule",
    // errors
    "PyErr_SetString",
    "PyErr_Occurred",
    "PyErr_Clear",
    // abstract / types / capsules
    "PyObject_GetAttrString",
    "PyObject_CallObject",
    "PyType_FromSpec",
    "PyType_Ready",
    "PyCapsule_New",
    "PyCapsule_GetPointer",
    // a #[no_mangle] static (data export, not a function)
    "_Py_NoneStruct",
    // varargs.c — native-archive symbols needing explicit /EXPORT
    "PyArg_ParseTuple",
    "PyArg_ParseTupleAndKeywords",
    "Py_BuildValue",
    "PyErr_Format",
    "PyObject_CallMethod",
    "PyUnicode_FromFormat",
];

#[test]
fn dll_loads_and_exports_resolve() {
    use windows_sys::Win32::System::LibraryLoader::GetProcAddress;
    let handle = load_dll(&dll_path());
    let mut missing = Vec::new();
    for name in SYMBOL_SAMPLE {
        let cname = std::ffi::CString::new(*name).unwrap();
        let addr = unsafe { GetProcAddress(handle, cname.as_ptr().cast()) };
        if addr.is_none() {
            missing.push(*name);
        }
    }
    assert!(
        missing.is_empty(),
        "python313.dll is missing exports: {missing:?} — if these are \
         varargs.c symbols, check the /EXPORT list in weavepy-pylib/build.rs"
    );
}

/// Run the shim exe with `-c code` and return `(success, stdout, stderr)`.
fn run_c(code: &str) -> (bool, String, String) {
    let out = Command::new(exe_path())
        .arg("-c")
        .arg(code)
        .output()
        .expect("failed to spawn weavepy.exe");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn shim_runs_python_through_the_dll() {
    // The DLL loaded into the process is what sys.dllhandle reports.
    let (ok, stdout, stderr) =
        run_c("import sys; assert sys.dllhandle != 0, sys.dllhandle; print('ok')");
    assert!(
        ok,
        "sys.dllhandle probe failed\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert_eq!(stdout.trim(), "ok");
}

#[test]
fn add_dll_directory_round_trips() {
    let code = r#"
import os, tempfile
d = tempfile.mkdtemp()
h = os.add_dll_directory(d)
r = repr(h)
assert r.startswith("<AddedDllDirectory(") and r.endswith(")>"), r
h.close()
assert repr(h) == "<AddedDllDirectory()>", repr(h)
with os.add_dll_directory(d) as ctx:
    assert repr(ctx).startswith("<AddedDllDirectory(")
print("ok")
"#;
    let (ok, stdout, stderr) = run_c(code);
    assert!(
        ok,
        "add_dll_directory probe failed\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert_eq!(stdout.trim(), "ok");
}

#[test]
fn broken_pyd_raises_cpython_shaped_import_error() {
    let code = r#"
import os, sys, tempfile
d = tempfile.mkdtemp()
with open(os.path.join(d, "_weave_bogus.pyd"), "wb") as f:
    f.write(b"MZ this is not a real DLL")
sys.path.insert(0, d)
try:
    import _weave_bogus
except ImportError as e:
    msg = str(e)
    assert msg.startswith("DLL load failed while importing _weave_bogus:"), msg
    print("ok")
else:
    raise SystemExit("expected ImportError")
"#;
    let (ok, stdout, stderr) = run_c(code);
    assert!(
        ok,
        "ImportError shape probe failed\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert_eq!(stdout.trim(), "ok");
}

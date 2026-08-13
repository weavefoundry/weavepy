//! Build helper for the `python313` cdylib (RFC 0064 WS1).
//!
//! rustc derives a cdylib's export table from the reachable
//! `#[no_mangle]` surface of the whole crate graph, which covers the
//! ~682 Rust-defined C-API symbols in `weavepy-capi` and this crate's
//! own entry points. The one blind spot is `weavepy-capi/src/varargs.c`:
//! its variadic helpers (`PyArg_ParseTuple`, `Py_BuildValue`, …) are
//! compiled by `cc` into a native static archive, and native-archive
//! symbols are *not* part of rustc's export list. On MSVC each one
//! needs an explicit `/EXPORT` (plus `/INCLUDE` so the archive member
//! is pulled even if the Rust side's force-link table were ever
//! reorganised away).
//!
//! The list below is the complete set of public definitions in
//! `varargs.c`; `src/lib.rs` carries a unit test that re-derives the
//! set from the C source and fails if the two drift.

use std::env;

/// Public symbols defined in `weavepy-capi/src/varargs.c` that a
/// `.pyd` may import and that rustc's cdylib export machinery cannot
/// see. Keep in sync with the C file (enforced by the unit test in
/// `src/lib.rs`).
pub const VARARGS_C_EXPORTS: &[&str] = &[
    "PyArg_Parse",
    "PyArg_ParseTuple",
    "PyArg_ParseTupleAndKeywords",
    "PyArg_UnpackTuple",
    "PyArg_VaParse",
    "PyArg_VaParseTupleAndKeywords",
    "PyBytes_FromFormat",
    "PyBytes_FromFormatV",
    "PyErr_Format",
    "PyErr_FormatUnraisable",
    "PyErr_FormatV",
    "PyErr_WarnFormat",
    "PyOS_snprintf",
    "PyObject_CallFunction",
    "PyObject_CallFunctionObjArgs",
    "PyObject_CallMethod",
    "PyObject_CallMethodObjArgs",
    "PyTuple_Pack",
    "PyUnicode_FromFormat",
    "PyUnicode_FromFormatV",
    "Py_BuildValue",
    "Py_VaBuildValue",
    "_PyErr_FormatFromCause",
];

fn main() {
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let target_env = env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    if target_os == "windows" && target_env == "msvc" {
        for sym in VARARGS_C_EXPORTS {
            // `/INCLUDE` forces the symbol (and so its archive
            // member) into the link; `/EXPORT` adds it to the DLL's
            // export table alongside the rustc-derived set.
            println!("cargo:rustc-link-arg-cdylib=/INCLUDE:{sym}");
            println!("cargo:rustc-link-arg-cdylib=/EXPORT:{sym}");
        }
    }
}

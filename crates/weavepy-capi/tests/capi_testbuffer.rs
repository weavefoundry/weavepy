//! Integration test: CPython's own `Modules/_testbuffer.c` (vendored
//! verbatim, RFC 0066 WS1) loads and its `ndarray` — the PEP 3118 spec
//! exporter — round-trips through WeavePy's buffer C-API and
//! memoryview, including the multi-dimensional and PIL-style
//! (suboffsets / `PyBUF_INDIRECT`) shapes that back `test_buffer`.
//!
//! `crates/weavepy-capi/build.rs` compiles the fixture against the
//! vendored stock CPython 3.13 headers and exports
//! `WEAVEPY_CAPI_TESTBUFFER_EXTENSION`. Skipped (passes) when unset.

use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard};

use weavepy::{run_source_with_options, InterpreterFlags, RunOptions};

/// One extension load at a time — the fixture keeps C-global state
/// (`Struct`, `calcsize`, the format cache), so concurrent interpreters
/// importing it race (same pattern as `capi_numpylike.rs`).
fn serialize() -> MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn extension_path() -> Option<PathBuf> {
    option_env!("WEAVEPY_CAPI_TESTBUFFER_EXTENSION").map(PathBuf::from)
}

/// Render `s` as a Python single-quoted literal.
fn py_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\'' => out.push_str("\\'"),
            c => out.push(c),
        }
    }
    out.push('\'');
    out
}

/// Stage the built `.so` under a temp dir as `_testbuffer.so` and run
/// `driver` with that dir prepended to `sys.path`.
fn run_with_testbuffer(driver_body: &str) {
    let Some(ext) = extension_path() else {
        eprintln!("WEAVEPY_CAPI_TESTBUFFER_EXTENSION not set — skipping");
        return;
    };
    if !ext.is_file() {
        eprintln!("extension path missing: {} — skipping", ext.display());
        return;
    }
    let _guard = serialize();
    let tmp = tempfile::tempdir().expect("mktemp");
    let staged = tmp.path().join("_testbuffer.so");
    std::fs::copy(&ext, &staged).expect("staging extension");
    let p_dir = py_quote(&tmp.path().display().to_string());
    let driver = format!("import sys\nsys.path.insert(0, {p_dir})\n{driver_body}");
    let opts = RunOptions::new("<testbuffer-test>").with_flags(InterpreterFlags::default());
    if let Err(err) = run_source_with_options(&driver, &opts) {
        let formatted = err.format(&driver, "<testbuffer-test>");
        panic!("_testbuffer driver failed:\n{formatted}");
    }
}

#[test]
fn testbuffer_skipped_when_extension_missing() {
    if extension_path().is_none() {
        eprintln!("WEAVEPY_CAPI_TESTBUFFER_EXTENSION not set — skipping _testbuffer proof");
    }
}

/// The module imports and exposes the surface `test_buffer` star-imports.
#[test]
fn testbuffer_imports() {
    run_with_testbuffer(
        "
import _testbuffer
from _testbuffer import ndarray, staticarray
from _testbuffer import ND_WRITABLE, ND_FORTRAN, ND_PIL, ND_SCALAR, ND_GETBUF_FAIL
from _testbuffer import PyBUF_SIMPLE, PyBUF_WRITABLE, PyBUF_FULL, PyBUF_FULL_RO, PyBUF_INDIRECT
assert _testbuffer.ND_MAX_NDIM == 128, _testbuffer.ND_MAX_NDIM
",
    );
}

/// 1-D construct + memoryview round-trip + writability.
#[test]
fn testbuffer_one_dimensional() {
    run_with_testbuffer(
        "
from _testbuffer import ndarray, ND_WRITABLE
nd = ndarray([1, 2, 3, 4, 5], shape=[5], format='i', flags=ND_WRITABLE)
m = memoryview(nd)
assert m.format == 'i', m.format
assert m.itemsize == 4, m.itemsize
assert m.ndim == 1, m.ndim
assert m.shape == (5,), m.shape
assert m.strides == (4,), m.strides
assert m.tolist() == [1, 2, 3, 4, 5], m.tolist()
m[0] = 42
assert m[0] == 42, m[0]
assert nd[0] == 42, nd[0]
",
    );
}

/// Multi-dimensional C- and Fortran-order arrays.
#[test]
fn testbuffer_multi_dimensional() {
    run_with_testbuffer(
        "
from _testbuffer import ndarray, ND_FORTRAN
nd = ndarray(list(range(6)), shape=[2, 3], format='B')
m = memoryview(nd)
assert m.ndim == 2, m.ndim
assert m.shape == (2, 3), m.shape
assert m.strides == (3, 1), m.strides
assert m.c_contiguous, 'expected C-contiguous'
assert not m.f_contiguous, 'not F-contiguous'
assert m[0, 0] == 0 and m[1, 2] == 5, (m[0, 0], m[1, 2])
assert m.tolist() == [[0, 1, 2], [3, 4, 5]], m.tolist()
assert m.tobytes() == bytes(range(6)), m.tobytes()

f = ndarray(list(range(6)), shape=[2, 3], format='B', flags=ND_FORTRAN)
fm = memoryview(f)
assert fm.strides == (1, 2), fm.strides
assert fm.f_contiguous, 'expected F-contiguous'
assert not fm.c_contiguous, 'not C-contiguous'
assert fm[1, 2] == 5, fm[1, 2]
assert fm.tolist() == [[0, 2, 4], [1, 3, 5]], fm.tolist()
",
    );
}

/// PIL-style arrays: suboffsets / PyBUF_INDIRECT consumers.
#[test]
fn testbuffer_pil_suboffsets() {
    run_with_testbuffer(
        "
from _testbuffer import ndarray, ND_PIL
items = list(range(12))
nd = ndarray(items, shape=[2, 2, 3], format='B', flags=ND_PIL)
m = memoryview(nd)
assert m.ndim == 3, m.ndim
assert m.shape == (2, 2, 3), m.shape
assert any(s >= 0 for s in m.suboffsets), m.suboffsets
assert m[0, 0, 0] == 0, m[0, 0, 0]
assert m[1, 1, 2] == 11, m[1, 1, 2]
assert m.tolist() == [[[0, 1, 2], [3, 4, 5]], [[6, 7, 8], [9, 10, 11]]], m.tolist()
assert m.tobytes() == bytes(range(12)), m.tobytes()
assert not m.c_contiguous and not m.f_contiguous
",
    );
}

/// `staticarray` exports a static Py_buffer; memoryview reads it.
#[test]
fn testbuffer_staticarray() {
    run_with_testbuffer(
        "
from _testbuffer import staticarray
sa = staticarray()
m = memoryview(sa)
assert m.tolist() == list(range(12)), m.tolist()
assert m.readonly, 'staticarray is read-only'
",
    );
}

/// The module-level helpers test_buffer leans on.
#[test]
fn testbuffer_module_helpers() {
    run_with_testbuffer(
        "
from _testbuffer import ndarray, get_contiguous, py_buffer_to_contiguous, is_contiguous
from _testbuffer import PyBUF_READ, PyBUF_ND, PyBUF_FULL_RO, ND_PIL
nd = ndarray(list(range(6)), shape=[2, 3], format='B')
assert is_contiguous(nd, 'C'), 'ndarray C-contiguous'
c = get_contiguous(nd, PyBUF_READ, 'C')
assert c.tolist() == [[0, 1, 2], [3, 4, 5]], c.tolist()
b = py_buffer_to_contiguous(nd, 'C', PyBUF_ND)
assert b == bytes(range(6)), b

pil = ndarray(list(range(6)), shape=[2, 3], format='B', flags=ND_PIL)
assert not is_contiguous(pil, 'C'), 'PIL array is not contiguous'
b2 = py_buffer_to_contiguous(pil, 'C', PyBUF_FULL_RO)
assert b2 == bytes(range(6)), b2
",
    );
}

//! End-to-end integration tests.
//!
//! Each `.py` file under `tests/fixtures/run/` is parsed, compiled,
//! and run through the WeavePy interpreter, with `print` redirected
//! to a buffer. The expected output is the matching `.out` file
//! beside it. A mismatch in either output content or the existence
//! of either file fails the test.
//!
//! This is the smallest amount of plumbing that lets us add new
//! fixtures by dropping `.py` / `.out` pairs into the directory.

use std::cell::RefCell;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use weavepy::{compiler, parser, vm};

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("run")
}

fn list_fixtures() -> Vec<PathBuf> {
    let dir = fixtures_dir();
    let mut out = Vec::new();
    if !dir.is_dir() {
        return out;
    }
    for entry in fs::read_dir(&dir).expect("read fixtures dir") {
        let entry = entry.expect("entry");
        let p = entry.path();
        if p.extension().and_then(|s| s.to_str()) == Some("py") {
            out.push(p);
        }
    }
    out.sort();
    out
}

/// Read a text file and collapse Windows-style `\r\n` line endings to
/// `\n`. `actions/checkout` rewrites text fixtures to CRLF on Windows
/// runners, which would otherwise break byte-exact comparisons.
fn read_text_normalized(path: &Path) -> std::io::Result<String> {
    fs::read_to_string(path).map(|s| s.replace("\r\n", "\n"))
}

fn run_fixture(py_path: &Path) {
    let source = read_text_normalized(py_path).expect("read source");
    let out_path = py_path.with_extension("out");
    let expected = read_text_normalized(&out_path)
        .unwrap_or_else(|_| panic!("missing expected-output file {}", out_path.display()));

    let module = parser::parse_module(&source).unwrap_or_else(|e| {
        panic!("parse {}: {e}", py_path.display());
    });
    let code = compiler::compile_module(&module).unwrap_or_else(|e| {
        panic!("compile {}: {e}", py_path.display());
    });

    let mut interp = vm::Interpreter::new();
    let buf: Rc<RefCell<Vec<u8>>> = Rc::new(RefCell::new(Vec::new()));
    let sink: vm::Stdout = buf.clone() as Rc<RefCell<dyn Write>>;
    interp.set_stdout(sink);
    interp.run_module(&code).unwrap_or_else(|e| {
        panic!("run {}: {e}", py_path.display());
    });

    let actual_bytes = buf.borrow().clone();
    let actual = String::from_utf8(actual_bytes)
        .expect("utf-8 output")
        .replace("\r\n", "\n");
    if actual != expected {
        panic!(
            "fixture {} mismatch\nexpected:\n{}\nactual:\n{}",
            py_path.display(),
            expected,
            actual
        );
    }
}

#[test]
fn all_run_fixtures_match() {
    let fixtures = list_fixtures();
    assert!(
        !fixtures.is_empty(),
        "expected at least one .py fixture in {}",
        fixtures_dir().display()
    );
    for f in fixtures {
        run_fixture(&f);
    }
}

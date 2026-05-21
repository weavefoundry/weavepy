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
        if p.extension().and_then(|s| s.to_str()) != Some("py") {
            continue;
        }
        // Files starting with `_` are import-only helpers and not
        // run as standalone fixtures (e.g. `_geom.py` is imported by
        // `19_import_user_module.py`). Mirrors Python's "leading
        // underscore means non-public" convention.
        let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or_default();
        if stem.starts_with('_') {
            continue;
        }
        out.push(p);
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

/// Some fixtures depend on platform-specific surface (e.g. the
/// `select`/`selectors`/`asyncio` stack only works on Unix today
/// because there's no IOCP backend). Honour a `# weavepy-skip: <os>`
/// directive at the top of the fixture so the test can co-exist
/// in the repo without breaking Windows CI.
fn should_skip(source: &str) -> bool {
    let target = if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(target_os = "linux") {
        "linux"
    } else {
        return false;
    };
    source.lines().take(20).any(|line| {
        let l = line.trim_start_matches('#').trim();
        if let Some(rest) = l.strip_prefix("weavepy-skip:") {
            rest.split(',')
                .map(str::trim)
                .any(|name| name.eq_ignore_ascii_case(target))
        } else {
            false
        }
    })
}

fn run_fixture(py_path: &Path) {
    let source = read_text_normalized(py_path).expect("read source");
    if should_skip(&source) {
        eprintln!("skipping fixture {} on this platform", py_path.display());
        return;
    }
    let out_path = py_path.with_extension("out");
    let expected = read_text_normalized(&out_path)
        .unwrap_or_else(|_| panic!("missing expected-output file {}", out_path.display()));

    let filename = py_path.display().to_string();
    let module = parser::parse_module(&source).unwrap_or_else(|e| {
        panic!("parse {}: {e}", py_path.display());
    });
    let code =
        compiler::compile_module_with_source(&module, &source, &filename).unwrap_or_else(|e| {
            panic!("compile {}: {e}", py_path.display());
        });

    let mut interp = vm::Interpreter::new();
    let buf: Rc<RefCell<Vec<u8>>> = Rc::new(RefCell::new(Vec::new()));
    let sink: vm::Stdout = buf.clone() as Rc<RefCell<dyn Write>>;
    interp.set_stdout(sink);
    // Sibling files in the fixtures directory must be importable so
    // multi-file tests (e.g. `import _helper`) work.
    if let Some(dir) = py_path.parent() {
        interp.prepend_path(dir);
    }
    interp.set_argv([filename.clone()]);
    interp
        .run_module_as(&code, "__main__", Some(&filename))
        .unwrap_or_else(|e| {
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

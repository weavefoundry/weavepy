//! Integration coverage for `weavepy -m test` (RFC 0034 §6).
//!
//! These tests drive the real `weavepy` binary through the
//! `test.__main__` → `test.libregrtest.main` plumbing against the
//! bundled self-host fixtures in `tests/regrtest/`. They guarantee the
//! `-m test` entry point — argument parsing, discovery, per-module
//! classification, the CPython-shaped summary, and the propagated exit
//! code — never silently rots, without needing a CPython checkout.

use std::path::PathBuf;
use std::process::Command;

/// Absolute path to the bundled `tests/regrtest/` fixture directory.
fn bundled_testdir() -> PathBuf {
    // CARGO_MANIFEST_DIR is `<workspace>/crates/weavepy-cli`.
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/regrtest")
        .canonicalize()
        .expect("bundled tests/regrtest directory should exist");
    assert!(
        dir.join("test_unittest_machinery.py").is_file(),
        "expected bundled fixtures under {}",
        dir.display()
    );
    dir
}

/// Run `weavepy -m test <args...>` against the bundled fixtures and
/// return `(success, stdout, stderr)`.
fn run_m_test(extra: &[&str]) -> (bool, String, String) {
    let testdir = bundled_testdir();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_weavepy"));
    cmd.arg("-m")
        .arg("test")
        .arg("--testdir")
        .arg(&testdir)
        .args(extra);
    let out = cmd.output().expect("failed to spawn weavepy -m test");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn m_test_single_bundled_fixture_passes() {
    let (ok, stdout, stderr) = run_m_test(&["--single", "test_unittest_machinery"]);
    assert!(
        ok,
        "`weavepy -m test --single test_unittest_machinery` should exit 0\n\
         --- stdout ---\n{stdout}\n--- stderr ---\n{stderr}"
    );
    assert!(
        stdout.contains("Result: SUCCESS"),
        "expected a CPython-shaped SUCCESS summary, got:\n{stdout}"
    );
    assert!(
        stdout.contains("passed: 1"),
        "expected exactly one passing module, got:\n{stdout}"
    );
}

#[test]
fn m_test_runs_multiple_named_modules() {
    let (ok, stdout, stderr) = run_m_test(&["test_unittest_machinery", "test_doctest_machinery"]);
    assert!(
        ok,
        "`weavepy -m test <two modules>` should exit 0\n\
         --- stdout ---\n{stdout}\n--- stderr ---\n{stderr}"
    );
    assert!(
        stdout.contains("Result: SUCCESS"),
        "expected a SUCCESS summary, got:\n{stdout}"
    );
    assert!(
        stdout.contains("passed: 2"),
        "expected two passing modules, got:\n{stdout}"
    );
}

/// A module that fails its assertions must make `-m test` exit non-zero
/// with a CPython-shaped FAILURE summary — this is the signal CI gates
/// on, so it must be wired through faithfully.
#[test]
fn m_test_reports_failure_exit_code() {
    let testdir = bundled_testdir();
    let tmp = std::env::temp_dir().join(format!("weavepy_mtest_fail_{}", std::process::id()));
    std::fs::create_dir_all(&tmp).expect("create temp testdir");
    let failing = tmp.join("test_intentional_fail.py");
    std::fs::write(
        &failing,
        "import unittest\n\
         class T(unittest.TestCase):\n\
         \x20   def test_boom(self):\n\
         \x20       self.assertEqual(1, 2)\n\
         if __name__ == '__main__':\n\
         \x20   unittest.main()\n",
    )
    .expect("write failing fixture");

    // Point --testdir at the temp dir holding only the failing module.
    let out = Command::new(env!("CARGO_BIN_EXE_weavepy"))
        .arg("-m")
        .arg("test")
        .arg("--testdir")
        .arg(&tmp)
        .arg("test_intentional_fail")
        .output()
        .expect("failed to spawn weavepy -m test");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    let _ = std::fs::remove_dir_all(&tmp);
    let _ = testdir; // bundled dir presence already asserted above.

    assert!(
        !out.status.success(),
        "a failing test module must yield a non-zero exit\n\
         --- stdout ---\n{stdout}\n--- stderr ---\n{stderr}"
    );
    assert!(
        stdout.contains("Result: FAILURE") || stdout.contains("failed:"),
        "expected a FAILURE summary, got:\n{stdout}"
    );
}

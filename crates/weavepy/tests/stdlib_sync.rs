//! RFC 0077 (WS8): the bundled-stdlib drift gate.
//!
//! `tools/stdlib_sync.py --check` re-derives which bundled files are
//! byte-identical to the vendored CPython `Lib/` and fails if any name
//! recorded in `tools/data/stdlib_verbatim.txt` no longer is. A file
//! that needs a WeavePy patch must be moved out of the recorded set
//! deliberately (re-run the tool with `--write`), so a stray edit to a
//! verbatim module is a test failure rather than archaeology at the
//! next re-vendor.

use std::path::PathBuf;
use std::process::Command;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repo root")
}

#[test]
fn bundled_stdlib_verbatim_set_has_not_drifted() {
    let root = repo_root();
    let tree = root.join("vendor/cpython/Lib");
    if !tree.join("os.py").exists() {
        eprintln!(
            "skipping: vendored CPython Lib/ not present at {}",
            tree.display()
        );
        return;
    }
    let Ok(out) = Command::new("python3")
        .arg(root.join("tools/stdlib_sync.py"))
        .arg("--check")
        .arg("--from")
        .arg(&tree)
        .current_dir(&root)
        .output()
    else {
        eprintln!("skipping: python3 not available");
        return;
    };
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "tools/stdlib_sync.py --check failed:\n{stdout}\n{stderr}"
    );
}

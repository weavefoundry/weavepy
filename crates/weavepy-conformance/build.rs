//! Build helper: compile CPython's `_testbuffer.c` fixture so the
//! regrtest harness can put it on `sys.path` (RFC 0066 WS1).
//!
//! `test_buffer` star-imports `_testbuffer` — CPython's PEP 3118 spec
//! exporter (`ndarray` with the full `PyBUF_*` flag matrix including
//! suboffsets, `staticarray`). The fixture lives in
//! `tests/capi_ext/_testbuffer.c` (vendored verbatim) and is compiled
//! against the *stock* CPython 3.13 headers vendored under
//! `crates/weavepy-capi/include/cpython313/`, exactly like
//! `weavepy-capi/build.rs` builds its binary-ABI proof fixtures. The
//! resulting dylib path is exported as
//! `WEAVEPY_REGRTEST_TESTBUFFER_EXTENSION`; the regrtest bootstrap
//! stages it into a shim directory appended to `sys.path`.
//!
//! Skipped quietly when the headers or a C compiler are unavailable —
//! `test_buffer` then simply keeps skipping its `TestBufferProtocol`
//! class, as before.

use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The stock CPython 3.13 include directories (staged `pyconfig.h` +
/// the vendored tree), mirroring `weavepy-capi/build.rs`'s
/// `stock_python_include`.
fn stock_python_include(capi_dir: &Path, out_dir: &Path) -> Option<Vec<PathBuf>> {
    let tree = capi_dir.join("include").join("cpython313");
    if !tree.join("Python.h").is_file() {
        return None;
    }
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let pyconfig = match target_os.as_str() {
        "macos" => "pyconfig-macos.h",
        "linux" | "freebsd" | "android" => "pyconfig-linux.h",
        _ => return None,
    };
    let pyconfig_src = capi_dir.join("include").join("pyconfig").join(pyconfig);
    println!("cargo:rerun-if-changed={}", pyconfig_src.display());
    let staged = out_dir.join("stock-include");
    std::fs::create_dir_all(&staged).ok()?;
    std::fs::copy(&pyconfig_src, staged.join("pyconfig.h")).ok()?;
    Some(vec![staged, tree])
}

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let workspace_root = manifest_dir
        .parent()
        .and_then(|p| p.parent())
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| manifest_dir.clone());
    let capi_dir = workspace_root.join("crates").join("weavepy-capi");
    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));

    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    // In-process regrtest runs load the extension into *this* binary, so
    // its dynamic symbol table must export the C-API surface (same flag
    // the weavepy CLI and the capi test binaries get).
    if target_os == "linux" || target_os == "freebsd" || target_os == "android" {
        println!("cargo:rustc-link-arg-bins=-Wl,--export-dynamic");
    }

    let src = workspace_root.join("tests/capi_ext/_testbuffer.c");
    if !src.is_file() {
        return;
    }
    println!("cargo:rerun-if-changed={}", src.display());
    let Some(include_dirs) = stock_python_include(&capi_dir, &out_dir) else {
        println!(
            "cargo:warning=stock CPython 3.13 headers not found; \
             regrtest runs without the _testbuffer fixture"
        );
        return;
    };

    let suffix = if target_os == "windows" { "dll" } else { "so" };
    let cc = env::var("CC").unwrap_or_else(|_| "cc".to_owned());
    let dylib = out_dir.join(format!("_testbuffer.{suffix}"));
    let mut cmd = Command::new(&cc);
    cmd.arg("-shared")
        .arg("-fPIC")
        .arg("-fvisibility=default")
        .arg("-O0")
        .arg("-Wno-error");
    for dir in &include_dirs {
        cmd.arg(format!("-I{}", dir.display()));
    }
    cmd.arg(&src).arg("-o").arg(&dylib);
    if target_os == "macos" {
        cmd.arg("-undefined").arg("dynamic_lookup");
    }
    match cmd.output() {
        Ok(out) if out.status.success() => {
            println!(
                "cargo:rustc-env=WEAVEPY_REGRTEST_TESTBUFFER_EXTENSION={}",
                dylib.display()
            );
        }
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            println!("cargo:warning=_testbuffer cc failed: {stderr}");
        }
        Err(err) => {
            println!("cargo:warning=could not run cc for _testbuffer: {err}");
        }
    }
}

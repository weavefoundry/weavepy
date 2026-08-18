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

    let Some(include_dirs) = stock_python_include(&capi_dir, &out_dir) else {
        println!(
            "cargo:warning=stock CPython 3.13 headers not found; \
             regrtest runs without the compiled C-API fixtures"
        );
        return;
    };
    let internal_include = capi_dir.join("include").join("cpython313").join("internal");

    let suffix = if target_os == "windows" { "dll" } else { "so" };
    let cc = env::var("CC").unwrap_or_else(|_| "cc".to_owned());

    // (source file, needs Py_BUILD_CORE_MODULE + the internal headers).
    // `_testsinglephase` / `_testmultiphase` (RFC 0068 WS4) are CPython's
    // extension-loader fixtures — test_importlib's extension suite scans
    // `sys.path` for the actual files.
    let fixtures: [(&str, bool); 3] = [
        ("_testbuffer", false),
        ("_testsinglephase", true),
        ("_testmultiphase", true),
    ];
    let mut built: Vec<String> = Vec::new();
    for (name, core_module) in fixtures {
        let src = workspace_root.join(format!("tests/capi_ext/{name}.c"));
        if !src.is_file() {
            continue;
        }
        println!("cargo:rerun-if-changed={}", src.display());
        let dylib = out_dir.join(format!("{name}.{suffix}"));
        let mut cmd = Command::new(&cc);
        cmd.arg("-shared")
            .arg("-fPIC")
            .arg("-fvisibility=default")
            .arg("-O0")
            .arg("-Wno-error");
        for dir in &include_dirs {
            cmd.arg(format!("-I{}", dir.display()));
        }
        if core_module {
            cmd.arg("-DPy_BUILD_CORE_MODULE")
                .arg(format!("-I{}", internal_include.display()))
                .arg(format!(
                    "-I{}",
                    workspace_root.join("tests/capi_ext").display()
                ));
        }
        cmd.arg(&src).arg("-o").arg(&dylib);
        if target_os == "macos" {
            cmd.arg("-undefined").arg("dynamic_lookup");
        }
        match cmd.output() {
            Ok(out) if out.status.success() => {
                built.push(dylib.display().to_string());
            }
            Ok(out) => {
                let stderr = String::from_utf8_lossy(&out.stderr);
                println!("cargo:warning={name} cc failed: {stderr}");
            }
            Err(err) => {
                println!("cargo:warning=could not run cc for {name}: {err}");
            }
        }
    }
    if !built.is_empty() {
        // Kept under the historical name; regrtest stages every listed
        // fixture into the shared shim directory.
        println!(
            "cargo:rustc-env=WEAVEPY_REGRTEST_TESTBUFFER_EXTENSION={}",
            built.join(";")
        );
    }
}

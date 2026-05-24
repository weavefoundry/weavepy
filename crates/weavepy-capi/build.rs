//! Build helper: compile the variadic C shim and the test extension
//! used by the integration tests.
//!
//! - `src/varargs.c` provides the variadic helpers (`PyArg_ParseTuple`,
//!   `Py_BuildValue`, `PyErr_Format`, `PyObject_CallFunction`, …)
//!   that can't be expressed in stable Rust.
//! - `tests/capi_ext/_smalltest.c` is a tiny extension module that
//!   the integration tests dlopen at runtime to verify the loader
//!   end-to-end.
//!
//! Both are compiled with `-fPIC -fvisibility=default` so the
//! resulting object can be linked into a shared library.

use std::env;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let workspace_root = manifest_dir
        .parent()
        .and_then(|p| p.parent())
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| manifest_dir.clone());

    // ----------------------------------------------------------------
    // 1) Compile the variadic shim into a static archive that gets
    //    linked into every consumer of the crate.
    // ----------------------------------------------------------------
    println!("cargo:rerun-if-changed=src/varargs.c");
    println!("cargo:rerun-if-changed=include/Python.h");
    let mut build = cc::Build::new();
    build
        .file("src/varargs.c")
        .include("include")
        .flag_if_supported("-fPIC")
        .flag_if_supported("-fvisibility=default")
        .flag_if_supported("-Wno-unused-parameter")
        .flag_if_supported("-Wno-unused-but-set-variable")
        .flag_if_supported("-Wno-unused-variable")
        .flag_if_supported("-Wno-unused-function")
        .flag_if_supported("-Wno-format-truncation");
    build.compile("weavepy_capi_varargs");

    // ----------------------------------------------------------------
    // 2) Build the integration-test extension module to a dylib
    //    in `target/<profile>/capi_ext`. The harness in
    //    `tests/capi_loader.rs` dlopens it and verifies the bridge.
    //
    //    We only build when the tests source exists; downstream
    //    consumers building only the library don't pay the cost.
    // ----------------------------------------------------------------
    let test_src = workspace_root.join("tests/capi_ext/_smalltest.c");
    if test_src.is_file() {
        println!("cargo:rerun-if-changed={}", test_src.display());
        let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR")).join("capi_ext");
        let _ = std::fs::create_dir_all(&out_dir);

        let cc = env::var("CC").unwrap_or_else(|_| "cc".to_owned());
        let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
        let suffix = match target_os.as_str() {
            "windows" => "dll",
            _ => "so",
        };
        let dylib = out_dir.join(format!("_smalltest.{suffix}"));
        let mut cmd = Command::new(&cc);
        cmd.arg("-shared")
            .arg("-fPIC")
            .arg("-fvisibility=default")
            .arg("-O0")
            .arg("-Wno-error")
            .arg(format!("-I{}", manifest_dir.join("include").display()))
            .arg(test_src.clone())
            .arg("-o")
            .arg(&dylib);
        if target_os == "macos" {
            cmd.arg("-undefined").arg("dynamic_lookup");
        }
        match cmd.output() {
            Ok(out) => {
                if !out.status.success() {
                    let stderr = String::from_utf8_lossy(&out.stderr);
                    println!("cargo:warning=test extension cc failed: {stderr}");
                } else {
                    println!(
                        "cargo:rustc-env=WEAVEPY_CAPI_TEST_EXTENSION={}",
                        dylib.display()
                    );
                }
            }
            Err(err) => {
                println!("cargo:warning=could not run cc for test extension: {err}");
            }
        }
    }

    // Re-export the include directory so dependent crates can see
    // `Python.h` via `DEP_WEAVEPY_CAPI_INCLUDE`.
    println!("cargo:include={}", manifest_dir.join("include").display());
}

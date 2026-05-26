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
use std::path::{Path, PathBuf};
use std::process::Command;

/// Bundle of the per-extension parameters threaded through
/// [`build_extension`]. Splitting these out keeps clippy's
/// `too_many_arguments` lint happy while still keeping the
/// build-script flat (no globals).
struct ExtensionBuild<'a> {
    cc: &'a str,
    manifest_dir: &'a Path,
    out_dir: &'a Path,
    target_os: &'a str,
    suffix: &'a str,
    src: &'a Path,
    name: &'a str,
    env_var: &'a str,
}

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
    // 2) Build the integration-test extension modules to dylibs in
    //    `target/<profile>/capi_ext`. The harness in
    //    `tests/capi_loader.rs` dlopens `_smalltest`; the buffer /
    //    vectorcall regression tests in `tests/capi_ndarray.rs`
    //    dlopen `_ndarray`.
    //
    //    We only build when each tests source exists; downstream
    //    consumers building only the library don't pay the cost.
    // ----------------------------------------------------------------
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let suffix = match target_os.as_str() {
        "windows" => "dll",
        _ => "so",
    };
    let cc = env::var("CC").unwrap_or_else(|_| "cc".to_owned());
    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR")).join("capi_ext");
    let _ = std::fs::create_dir_all(&out_dir);

    fn build_extension(opts: ExtensionBuild<'_>) {
        let ExtensionBuild {
            cc,
            manifest_dir,
            out_dir,
            target_os,
            suffix,
            src,
            name,
            env_var,
        } = opts;
        if !src.is_file() {
            return;
        }
        println!("cargo:rerun-if-changed={}", src.display());
        let dylib = out_dir.join(format!("{name}.{suffix}"));
        let mut cmd = Command::new(cc);
        cmd.arg("-shared")
            .arg("-fPIC")
            .arg("-fvisibility=default")
            .arg("-O0")
            .arg("-Wno-error")
            .arg(format!("-I{}", manifest_dir.join("include").display()))
            .arg(src)
            .arg("-o")
            .arg(&dylib);
        if target_os == "macos" {
            cmd.arg("-undefined").arg("dynamic_lookup");
        }
        match cmd.output() {
            Ok(out) => {
                if !out.status.success() {
                    let stderr = String::from_utf8_lossy(&out.stderr);
                    println!("cargo:warning={name} cc failed: {stderr}");
                } else {
                    println!("cargo:rustc-env={env_var}={}", dylib.display());
                }
            }
            Err(err) => {
                println!("cargo:warning=could not run cc for {name}: {err}");
            }
        }
    }

    let smalltest_src = workspace_root.join("tests/capi_ext/_smalltest.c");
    build_extension(ExtensionBuild {
        cc: &cc,
        manifest_dir: &manifest_dir,
        out_dir: &out_dir,
        target_os: &target_os,
        suffix,
        src: &smalltest_src,
        name: "_smalltest",
        env_var: "WEAVEPY_CAPI_TEST_EXTENSION",
    });
    let ndarray_src = workspace_root.join("tests/capi_ext/_ndarray.c");
    build_extension(ExtensionBuild {
        cc: &cc,
        manifest_dir: &manifest_dir,
        out_dir: &out_dir,
        target_os: &target_os,
        suffix,
        src: &ndarray_src,
        name: "_ndarray",
        env_var: "WEAVEPY_CAPI_NDARRAY_EXTENSION",
    });
    let numpylike_src = workspace_root.join("tests/capi_ext/_numpylike.c");
    build_extension(ExtensionBuild {
        cc: &cc,
        manifest_dir: &manifest_dir,
        out_dir: &out_dir,
        target_os: &target_os,
        suffix,
        src: &numpylike_src,
        name: "_numpylike",
        env_var: "WEAVEPY_CAPI_NUMPYLIKE_EXTENSION",
    });

    // Re-export the include directory so dependent crates can see
    // `Python.h` via `DEP_WEAVEPY_CAPI_INCLUDE`.
    println!("cargo:include={}", manifest_dir.join("include").display());

    // On Linux (and other ELF targets that aren't macOS or Windows),
    // dlopen'd extension modules resolve symbols like
    // `PyExc_RuntimeError` and `PyLong_FromLong` against the host
    // executable's *dynamic* symbol table. Without `--export-dynamic`,
    // `ld` only exposes the subset that the binary's own dependencies
    // already asked for — which strips out essentially the entire
    // C-API surface and produces
    // `ImportError: undefined symbol: PyExc_RuntimeError` at load
    // time. This is the same flag CPython itself ships with
    // (`./configure --enable-shared` adds `-Wl,--export-dynamic`).
    // No-op on macOS (two-level namespaces) and unrecognised by
    // `link.exe` on Windows, hence the target-family gate.
    //
    // `weavepy-capi` is a library crate with no bin / example /
    // benchmark targets (Cargo 1.95+ rejects
    // `rustc-link-arg-bins`/`-benches`/`-examples` from a build
    // script that doesn't produce those target kinds), so we emit
    // the flag only for the crate's own integration tests — that's
    // what reaches the `capi_wheel_endtoend` and `capi_loader` test
    // binaries on CI. The production `weavepy` CLI gets the same
    // flag through `crates/weavepy-cli/build.rs`.
    if target_os == "linux" || target_os == "freebsd" || target_os == "android" {
        println!("cargo:rustc-link-arg-tests=-Wl,--export-dynamic");
    }
}

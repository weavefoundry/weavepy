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
    include_dirs: &'a [PathBuf],
    out_dir: &'a Path,
    target_os: &'a str,
    suffix: &'a str,
    src: &'a Path,
    name: &'a str,
    env_var: &'a str,
}

/// The stock CPython 3.13 include directories the binary-ABI proof
/// fixtures compile against.
///
/// RFC 0062 WS2 vendored the stock `Include/` tree into
/// `include/cpython313/` (plus per-OS generated `pyconfig.h`
/// variants), so the default is fully hermetic: stage the right
/// `pyconfig.h` under `OUT_DIR` and compile against
/// `[staged, tree]`. `WEAVEPY_STOCK_PYTHON` still overrides with a
/// host interpreter's headers for cross-validation against a real
/// CPython install.
fn stock_python_include(manifest_dir: &Path, out_dir: &Path) -> Option<Vec<PathBuf>> {
    println!("cargo:rerun-if-env-changed=WEAVEPY_STOCK_PYTHON");
    if let Ok(interp) = env::var("WEAVEPY_STOCK_PYTHON") {
        let out = Command::new(&interp)
            .arg("-c")
            .arg("import sysconfig; print(sysconfig.get_path('include'))")
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        let inc = String::from_utf8(out.stdout).ok()?.trim().to_owned();
        if !inc.is_empty() && Path::new(&inc).join("Python.h").is_file() {
            return Some(vec![PathBuf::from(inc)]);
        }
        return None;
    }
    let tree = manifest_dir
        .join("include")
        .join(weavepy_version::HEADER_TREE);
    if !tree.join("Python.h").is_file() {
        return None;
    }
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let pyconfig = match target_os.as_str() {
        "macos" => "pyconfig-macos.h",
        "linux" | "freebsd" | "android" => "pyconfig-linux.h",
        _ => return None,
    };
    let pyconfig_src = manifest_dir.join("include").join("pyconfig").join(pyconfig);
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
            include_dirs,
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
            .arg("-Wno-error");
        for dir in include_dirs {
            cmd.arg(format!("-I{}", dir.display()));
        }
        cmd.arg(src).arg("-o").arg(&dylib);
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

    let weavepy_inc = vec![manifest_dir.join("include")];
    let smalltest_src = workspace_root.join("tests/capi_ext/_smalltest.c");
    build_extension(ExtensionBuild {
        cc: &cc,
        include_dirs: &weavepy_inc,
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
        include_dirs: &weavepy_inc,
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
        include_dirs: &weavepy_inc,
        out_dir: &out_dir,
        target_os: &target_os,
        suffix,
        src: &numpylike_src,
        name: "_numpylike",
        env_var: "WEAVEPY_CAPI_NUMPYLIKE_EXTENSION",
    });

    // ----------------------------------------------------------------
    // 2b) RFC 0043 binary-ABI hermetic proofs: compile the proof
    //     fixtures against the host's *stock* CPython 3.13 headers
    //     (full, non-limited API → real inlined macros and the genuine
    //     416-byte `PyTypeObject`), NOT WeavePy's `include/Python.h`.
    //
    //       * `_stockabi.c`  — wave 1: faithful object mirrors, inlined
    //         head/field macros, refcount poke, `tp_dealloc`.
    //       * `_stocktype.c` — wave 2 (RFC 0044): classic static
    //         `PyTypeObject` + `PyType_Ready`, method suites, richcompare,
    //         call/iter/descriptor protocols, and a `Py_TPFLAGS_HAVE_GC`
    //         type with `tp_traverse`/`tp_clear`.
    //       * `_stockarray.c` — wave 3 (RFC 0045): inline `tp_basicsize`
    //         instance storage (`PyArrayObject` shape), real `tp_members`
    //         at fixed offsets, the `__array_interface__`/`__array_struct__`
    //         interchange protocols, and the `import_array()` array-C-API
    //         capsule pattern.
    //
    //     Skipped (with a note) when CPython 3.13 dev headers aren't
    //     present, so a bare CI host still builds and the stock proofs
    //     self-skip.
    // ----------------------------------------------------------------
    match stock_python_include(&manifest_dir, out_dir.parent().unwrap_or(&out_dir)) {
        Some(stock_inc) => {
            println!(
                "cargo:rustc-env=WEAVEPY_STOCK_PYTHON_INCLUDE={}",
                stock_inc
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect::<Vec<_>>()
                    .join(":")
            );
            build_extension(ExtensionBuild {
                cc: &cc,
                include_dirs: &stock_inc,
                out_dir: &out_dir,
                target_os: &target_os,
                suffix,
                src: &workspace_root.join("tests/capi_ext/_stockabi.c"),
                name: "_stockabi",
                env_var: "WEAVEPY_CAPI_STOCKABI_EXTENSION",
            });
            build_extension(ExtensionBuild {
                cc: &cc,
                include_dirs: &stock_inc,
                out_dir: &out_dir,
                target_os: &target_os,
                suffix,
                src: &workspace_root.join("tests/capi_ext/_stocktype.c"),
                name: "_stocktype",
                env_var: "WEAVEPY_CAPI_STOCKTYPE_EXTENSION",
            });
            build_extension(ExtensionBuild {
                cc: &cc,
                include_dirs: &stock_inc,
                out_dir: &out_dir,
                target_os: &target_os,
                suffix,
                src: &workspace_root.join("tests/capi_ext/_stockarray.c"),
                name: "_stockarray",
                env_var: "WEAVEPY_CAPI_STOCKARRAY_EXTENSION",
            });
            // `_stockcython.c` — wave 5 (RFC 0047): a Cython-shaped
            // extension that subclasses an extension-defined base and
            // reads inherited slots directly off `Py_TYPE(self)` (the
            // `inherit_slots` proof), plus the Cython C-API runtime tail.
            build_extension(ExtensionBuild {
                cc: &cc,
                include_dirs: &stock_inc,
                out_dir: &out_dir,
                target_os: &target_os,
                suffix,
                src: &workspace_root.join("tests/capi_ext/_stockcython.c"),
                name: "_stockcython",
                env_var: "WEAVEPY_CAPI_STOCKCYTHON_EXTENSION",
            });
            // `_stockdatetime.c` — wave 5 (RFC 0029): a datetime consumer
            // compiled against the real `datetime.h`, exercising
            // `PyDateTime_IMPORT`, the inlined `PyDateTime_GET_*` accessor
            // macros, the capsule constructors, and the `tp_basicsize`
            // size-check — the exact ABI surface pandas' `tslibs` uses.
            build_extension(ExtensionBuild {
                cc: &cc,
                include_dirs: &stock_inc,
                out_dir: &out_dir,
                target_os: &target_os,
                suffix,
                src: &workspace_root.join("tests/capi_ext/_stockdatetime.c"),
                name: "_stockdatetime",
                env_var: "WEAVEPY_CAPI_STOCKDATETIME_EXTENSION",
            });
            // `_abi3check.c` — RFC 0056 WS5: the limited-API (abi3) proof.
            // The source `#define`s `Py_LIMITED_API 0x030D0000` before
            // including the stock `Python.h`, so it binds only exported
            // functions (no inlined macros) — the exact surface a PyO3
            // `abi3-py313` wheel uses (multiphase init, PyType_FromSpec,
            // PyObject_Vectorcall, PyGILState_*, PyInterpreterState_Get).
            build_extension(ExtensionBuild {
                cc: &cc,
                include_dirs: &stock_inc,
                out_dir: &out_dir,
                target_os: &target_os,
                suffix,
                src: &workspace_root.join("tests/capi_ext/_abi3check.c"),
                name: "_abi3check",
                env_var: "WEAVEPY_CAPI_ABI3CHECK_EXTENSION",
            });
            // `_greenletconsumer.c` — RFC 0072 WS1: a gevent-shaped
            // consumer of the `greenlet._C_API` capsule, compiled
            // against the stock headers plus the vendored upstream
            // `greenlet/greenlet.h`. Exercises `PyGreenlet_Import`,
            // the 12-slot table, `sizeof(PyGreenlet)` type checks, a
            // static C subclass with a field at offset
            // `sizeof(PyGreenlet)`, and switching from inside a C
            // frame.
            {
                let mut greenlet_inc = stock_inc.clone();
                greenlet_inc.push(workspace_root.join("tests/capi_ext"));
                println!(
                    "cargo:rerun-if-changed={}",
                    workspace_root
                        .join("tests/capi_ext/greenlet/greenlet.h")
                        .display()
                );
                build_extension(ExtensionBuild {
                    cc: &cc,
                    include_dirs: &greenlet_inc,
                    out_dir: &out_dir,
                    target_os: &target_os,
                    suffix,
                    src: &workspace_root.join("tests/capi_ext/_greenletconsumer.c"),
                    name: "_greenletconsumer",
                    env_var: "WEAVEPY_CAPI_GREENLETCONSUMER_EXTENSION",
                });
            }
            // `_testbuffer.c` — RFC 0066 WS1: CPython's own
            // `Modules/_testbuffer.c`, verbatim — the PEP 3118 spec
            // exporter (`ndarray` with the full PyBUF flag matrix incl.
            // suboffsets, `staticarray`) that `test_buffer` and
            // pickletester's out-of-band legs import. Compiled against
            // the stock headers so its inlined macros are CPython's.
            build_extension(ExtensionBuild {
                cc: &cc,
                include_dirs: &stock_inc,
                out_dir: &out_dir,
                target_os: &target_os,
                suffix,
                src: &workspace_root.join("tests/capi_ext/_testbuffer.c"),
                name: "_testbuffer",
                env_var: "WEAVEPY_CAPI_TESTBUFFER_EXTENSION",
            });
        }
        None => {
            println!(
                "cargo:warning=stock CPython 3.13 headers not found; \
                 skipping the _stockabi/_stocktype/_stockarray/_stockcython \
                 binary-ABI proof fixtures"
            );
        }
    }

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

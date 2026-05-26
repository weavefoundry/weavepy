//! Build helper for the `weavepy` CLI binary.
//!
//! On Linux (and other ELF targets), dlopen'd extension modules
//! such as `_numpylike.so` expect to resolve C-API symbols
//! (`PyExc_RuntimeError`, `PyLong_FromLong`, …) against the host
//! binary's dynamic symbol table. Stock Rust binaries don't export
//! their symbols by default, which would cause every binary
//! extension to fail at import time with
//! `ImportError: undefined symbol: PyExc_RuntimeError`.
//!
//! `-Wl,--export-dynamic` is what CPython itself ships with
//! (`./configure --enable-shared` adds it for the same reason).
//! It's a no-op on macOS (two-level namespaces) and unrecognised
//! by `link.exe` on Windows, hence the target-family gate.

use std::env;

fn main() {
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os == "linux" || target_os == "freebsd" || target_os == "android" {
        println!("cargo:rustc-link-arg-bins=-Wl,--export-dynamic");
    }
}

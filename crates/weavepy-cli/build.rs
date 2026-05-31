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
//!
//! On Windows we additionally enlarge the binary's *main-thread* stack
//! reserve. WeavePy's evaluator is a recursive tree-walker, so Python
//! call depth maps onto native (Rust) stack depth. Windows reserves only
//! 1 MiB for the main thread by default — far below the 8 MiB Linux and
//! macOS give — so deep workloads such as `weavepy -m test` overflow the
//! stack before `sys.setrecursionlimit` can guard them. Reserving 64 MiB
//! (committed lazily, so it costs only address space) makes the depth
//! limit governed by the recursion limit uniformly across platforms.
//! A build-script link arg is used rather than `.cargo/config.toml`
//! `rustflags` because the latter is silently dropped when CI sets the
//! `RUSTFLAGS` environment variable.

use std::env;

const WINDOWS_STACK_BYTES: u64 = 64 * 1024 * 1024;

fn main() {
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os == "linux" || target_os == "freebsd" || target_os == "android" {
        println!("cargo:rustc-link-arg-bins=-Wl,--export-dynamic");
    }
    if target_os == "windows" {
        let target_env = env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
        if target_env == "gnu" {
            // GNU ld (mingw): `--stack <reserve>`.
            println!("cargo:rustc-link-arg-bins=-Wl,--stack,{WINDOWS_STACK_BYTES}");
        } else {
            // MSVC link.exe (the default on the GitHub `windows-latest`
            // runner): `/STACK:reserve`.
            println!("cargo:rustc-link-arg-bins=/STACK:{WINDOWS_STACK_BYTES}");
        }
    }
}

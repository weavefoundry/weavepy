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
//!
//! The `weavepy` binary is linked with `weavepy.order` (macOS
//! `-order_file`; on x86-64 Linux, whose default linker is lld,
//! `--symbol-ordering-file`): the functions start-up and common workloads
//! run, start-up's in first-use order (`scripts/gen-order-file.sh`), so a
//! process maps a few contiguous megabytes of code instead of touching
//! pages spread over the whole text segment. Linux maps 64 KiB around
//! each page a fault touches, so there the order takes start-up's peak
//! RSS from 22 to 14 MiB. The names embed crate hashes that change with
//! the toolchain, the profile and the dependencies, so the host targets
//! link through `scripts/link-ordered.sh` (see `.cargo/config.toml`),
//! which remaps the file onto the link's own symbols; names it can't
//! place are dropped, so a stale file costs only the layout benefit.
//! `WEAVEPY_NO_ORDER_FILE` opts out.

use std::env;
use std::path::Path;

const WINDOWS_STACK_BYTES: u64 = 64 * 1024 * 1024;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=WEAVEPY_NO_ORDER_FILE");
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os == "linux" || target_os == "freebsd" || target_os == "android" {
        println!("cargo:rustc-link-arg-bins=-Wl,--export-dynamic");
    }
    let order =
        Path::new(&env::var("CARGO_MANIFEST_DIR").unwrap_or_default()).join("weavepy.order");
    println!("cargo:rerun-if-changed={}", order.display());
    if order.exists() && env::var_os("WEAVEPY_NO_ORDER_FILE").is_none() {
        let target_arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
        let target_env = env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
        if target_os == "macos" {
            println!(
                "cargo:rustc-link-arg-bin=weavepy=-Wl,-order_file,{}",
                order.display()
            );
        } else if target_os == "linux" && target_arch == "x86_64" && target_env == "gnu" {
            // (`link-ordered.py` drops both for a link that isn't through
            // lld: GNU ld has no such option.)
            println!(
                "cargo:rustc-link-arg-bin=weavepy=-Wl,--symbol-ordering-file={}",
                order.display()
            );
            println!("cargo:rustc-link-arg-bin=weavepy=-Wl,--no-warn-symbol-ordering");
        }
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

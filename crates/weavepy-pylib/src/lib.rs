//! The WeavePy runtime as a CPython-ABI shared library (RFC 0064 WS1).
//!
//! On Windows this crate builds `python313.dll` — the module name
//! every CPython-3.13 extension's PE import table references. The
//! whole interpreter lives here: the `weavepy.exe` shim
//! (`weavepy-cli/src/main.rs`) loads this DLL and calls
//! [`weavepy_main`], and a `.pyd` loaded later binds its
//! `python313.dll` imports to this already-loaded module, so there is
//! exactly one runtime in the process.
//!
//! The C-API itself needs no code in this crate: the ~682
//! `#[no_mangle]` symbols defined in `weavepy-capi` (linked
//! transitively through `weavepy-cli` → `weavepy`) are exported by
//! rustc's cdylib machinery, and the variadic C helpers from
//! `varargs.c` are exported via `/EXPORT` link args emitted by this
//! crate's `build.rs`. What lives here are the *entry points*:
//! [`weavepy_main`] for the shim, and the CPython embedding twins
//! [`Py_Main`] / [`Py_BytesMain`] that stock `pylifecycle.h` declares.
//!
//! On POSIX the same crate builds `libpython313.{so,dylib}`; it is
//! compiled everywhere (keeping the export surface honest on every
//! `cargo test --workspace`) but only the Windows artifact ships —
//! the POSIX distribution keeps its fully-static binary (RFC 0064
//! Non-goals).

use std::ffi::{c_char, c_int, CStr};

/// Run the WeavePy CLI against the process's real argv and
/// environment, returning the exit code. The `weavepy.exe` shim's
/// whole job is `GetProcAddress(dll, "weavepy_main")` + call.
#[no_mangle]
pub extern "C" fn weavepy_main() -> c_int {
    weavepy_cli::cli_main()
}

/// CPython's wide-argv embedding entry point.
///
/// Decodes `argv` (UTF-16 on Windows, UTF-32 elsewhere — `wchar_t`'s
/// platform width) and runs the CLI with it. Ill-formed sequences
/// decode lossily (U+FFFD), matching how WeavePy's own Windows argv
/// path treats non-Unicode argv today.
///
/// # Safety
///
/// `argv` must point to `argc` valid NUL-terminated `wchar_t`
/// strings, per the CPython contract.
#[no_mangle]
pub unsafe extern "C" fn Py_Main(argc: c_int, argv: *mut *mut libc::wchar_t) -> c_int {
    let mut args: Vec<String> = Vec::new();
    if !argv.is_null() {
        for i in 0..usize::try_from(argc.max(0)).unwrap_or(0) {
            // SAFETY: caller guarantees `argc` valid entries.
            let arg = unsafe { *argv.add(i) };
            if arg.is_null() {
                break;
            }
            // SAFETY: caller guarantees NUL termination.
            args.push(unsafe { decode_wide_arg(arg) });
        }
    }
    weavepy_cli::cli_main_with_args(args)
}

/// CPython's byte-argv embedding entry point (PEP 587's
/// `Py_BytesMain`). Bytes are decoded as UTF-8; undecodable bytes
/// decode lossily (U+FFFD) — the embedding twin of the CLI's
/// Windows argv posture. (The POSIX CLI's PEP 383 surrogateescape
/// bridge applies to *process* argv; embedders passing non-UTF-8
/// argv through this entry point get the lossy decode, documented
/// here rather than silently diverging per platform.)
///
/// # Safety
///
/// `argv` must point to `argc` valid NUL-terminated C strings, per
/// the CPython contract.
#[no_mangle]
pub unsafe extern "C" fn Py_BytesMain(argc: c_int, argv: *mut *mut c_char) -> c_int {
    let mut args: Vec<String> = Vec::new();
    if !argv.is_null() {
        for i in 0..usize::try_from(argc.max(0)).unwrap_or(0) {
            // SAFETY: caller guarantees `argc` valid entries.
            let arg = unsafe { *argv.add(i) };
            if arg.is_null() {
                break;
            }
            // SAFETY: caller guarantees NUL termination.
            let bytes = unsafe { CStr::from_ptr(arg) }.to_bytes();
            args.push(String::from_utf8_lossy(bytes).into_owned());
        }
    }
    weavepy_cli::cli_main_with_args(args)
}

/// Decode one NUL-terminated `wchar_t` string: UTF-16 where
/// `wchar_t` is 2 bytes (Windows), UTF-32 where it is 4 (POSIX);
/// lossy on ill-formed input.
///
/// # Safety
///
/// `ptr` must point to a valid NUL-terminated `wchar_t` string.
// `wchar_t` is u16 on Windows and i32 on POSIX; `as u32` is the one
// portable bridge (a negative POSIX unit wraps and `from_u32` rejects
// it as REPLACEMENT_CHARACTER, which is the lossy contract anyway).
#[allow(clippy::cast_lossless, clippy::cast_sign_loss)]
unsafe fn decode_wide_arg(ptr: *const libc::wchar_t) -> String {
    let mut len = 0usize;
    // SAFETY: caller guarantees NUL termination.
    while unsafe { *ptr.add(len) } != 0 {
        len += 1;
    }
    // SAFETY: `len` counted valid elements above.
    let units = unsafe { std::slice::from_raw_parts(ptr, len) };
    if std::mem::size_of::<libc::wchar_t>() == 2 {
        let units16: Vec<u16> = units.iter().map(|&u| u as u16).collect();
        String::from_utf16_lossy(&units16)
    } else {
        units
            .iter()
            .map(|&u| char::from_u32(u as u32).unwrap_or(char::REPLACEMENT_CHARACTER))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    /// The `/EXPORT` list in `build.rs` must exactly match the public
    /// definitions in `weavepy-capi/src/varargs.c` — a drifted list
    /// means a `.pyd` importing a variadic helper gets an unresolved
    /// import on Windows.
    #[test]
    fn varargs_export_list_matches_c_source() {
        let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let c_src = std::fs::read_to_string(manifest.join("../weavepy-capi/src/varargs.c"))
            .expect("varargs.c readable");
        let build_rs =
            std::fs::read_to_string(manifest.join("build.rs")).expect("build.rs readable");

        // Public definitions: `<ret-type> Name(` at column 0. The
        // crash-handler helper is internal (the driver references it
        // at DLL link time; no extension imports it).
        let mut defined: Vec<String> = Vec::new();
        for line in c_src.lines() {
            let Some(rest) = line
                .strip_prefix("PyObject *")
                .or_else(|| line.strip_prefix("int "))
                .or_else(|| line.strip_prefix("void "))
            else {
                continue;
            };
            let Some(paren) = rest.find('(') else {
                continue;
            };
            let name = rest[..paren].trim();
            if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                continue;
            }
            if name == "weavepy_install_crash_handler" {
                continue;
            }
            defined.push(name.to_owned());
        }
        defined.sort();
        defined.dedup();
        assert!(
            defined.len() >= 20,
            "suspiciously few public definitions parsed from varargs.c: {defined:?}"
        );
        for name in &defined {
            assert!(
                build_rs.contains(&format!("\"{name}\"")),
                "varargs.c defines `{name}` but build.rs does not export it"
            );
        }
        // And nothing exported that the C file doesn't define.
        for line in build_rs.lines() {
            let line = line.trim();
            let Some(name) = line.strip_prefix('"').and_then(|l| l.strip_suffix("\",")) else {
                continue;
            };
            assert!(
                defined.iter().any(|d| d == name),
                "build.rs exports `{name}` but varargs.c does not define it"
            );
        }
    }
}

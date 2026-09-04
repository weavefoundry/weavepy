//! The CPython version WeavePy presents itself as, spelled once.
//!
//! RFC 0077 (WS12). Before this crate the identity was written out by
//! hand in a dozen places (`sys.rs`, `stdlib_tree.rs`, `pycache.rs`,
//! `sysconfig_native.rs`, the extension loader's suffix table, the
//! C-API's `Py_Version`/`Py_GetVersion`, the CLI's DLL name, the dist
//! tool's artifact names, and both header-embedding `build.rs`), so a
//! minor-version switch was a grep. Every one of those now reads from
//! here; the switch to 3.15 is the three numbers plus the derived
//! literals below, and the `derived_literals_agree` test keeps the
//! literals honest against the numbers.
//!
//! This crate has no dependencies so `build.rs` scripts can use it too.

/// `sys.version_info.major`.
pub const MAJOR: u32 = 3;
/// `sys.version_info.minor`.
pub const MINOR: u32 = 13;
/// `sys.version_info.micro`. Tracks the vendored `Lib/` the regrtest
/// baseline was measured against.
pub const MICRO: u32 = 0;

/// `"3.13"`: `sys.winver`, `python3.13-config`, `libpython3.13`, the
/// `python-3.13.pc` files, `include/python3.13/`.
pub const SHORT: &str = "3.13";
/// `"313"`: `py_version_nodot`, `python313.dll`, `libpython313.dylib`.
pub const NODOT: &str = "313";
/// `"3.13.0"`: `platform.python_version()`, `Py_GetVersion()`.
pub const FULL: &str = "3.13.0";

/// `PY_VERSION_HEX` for a final release (`serial 0`, level `f`).
pub const HEX: u32 = (MAJOR << 24) | (MINOR << 16) | (MICRO << 8) | 0xF0;

/// The bundled stdlib's directory under `lib/` (deliberately not
/// `python3.13`, so a WeavePy tree and a CPython install never shadow
/// each other).
pub const LIB_DIR_NAME: &str = "weavepy3.13";
/// `sys.implementation.cache_tag` prefix; `pycache.rs` appends the
/// bytecode-format generation.
pub const CACHE_TAG_PREFIX: &str = "weavepy-313";
/// `sysconfig.get_config_var("SOABI")` prefix and the C-extension
/// filename tag (`foo.cpython-313-darwin.so`).
pub const SOABI_PREFIX: &str = "cpython-313";
/// The wheel/ABI tag (`cp313`) and Windows extension tag (`.cp313-win_amd64.pyd`).
pub const CP_TAG: &str = "cp313";
/// The C-API library stem: `python313.dll`, `libpython313.dylib`.
pub const PYLIB_STEM: &str = "python313";
/// The vendored `Include/` tree under `crates/weavepy-capi/include/`.
pub const HEADER_TREE: &str = "cpython313";

/// Concatenate string parts into a fixed byte buffer at compile time.
/// The building block for [`vconcat!`]; not meant to be called directly.
pub const fn concat_into<const N: usize, const M: usize>(parts: [&str; M]) -> [u8; N] {
    let mut out = [0u8; N];
    let mut at = 0;
    let mut p = 0;
    while p < M {
        let bytes = parts[p].as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            out[at] = bytes[i];
            at += 1;
            i += 1;
        }
        p += 1;
    }
    assert!(at == N, "concat_into: length mismatch");
    out
}

/// Build a `&'static str` from string constants at compile time, so a
/// platform suffix like `".cpython-313-darwin.so"` can be spelled as
/// `vconcat!(".", weavepy_version::SOABI_PREFIX, "-darwin.so")` and
/// follow [`NODOT`] without a runtime allocation.
#[macro_export]
macro_rules! vconcat {
    ($($s:expr),+ $(,)?) => {{
        const LEN: usize = 0 $(+ $s.len())+;
        const BUF: [u8; LEN] = $crate::concat_into::<LEN, { [$($s),+].len() }>([$($s),+]);
        // SAFETY: every part is a `&str`, and concatenating valid UTF-8
        // strings yields valid UTF-8.
        unsafe { ::std::str::from_utf8_unchecked(&BUF) }
    }};
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vconcat_builds_static_strs() {
        const S: &str = vconcat!(".", SOABI_PREFIX, "-darwin.so");
        assert_eq!(S, ".cpython-313-darwin.so");
        assert_eq!(vconcat!(PYLIB_STEM, ".dll"), "python313.dll");
    }

    #[test]
    fn derived_literals_agree() {
        assert_eq!(SHORT, format!("{MAJOR}.{MINOR}"));
        assert_eq!(NODOT, format!("{MAJOR}{MINOR}"));
        assert_eq!(FULL, format!("{MAJOR}.{MINOR}.{MICRO}"));
        assert_eq!(LIB_DIR_NAME, format!("weavepy{SHORT}"));
        assert_eq!(CACHE_TAG_PREFIX, format!("weavepy-{NODOT}"));
        assert_eq!(SOABI_PREFIX, format!("cpython-{NODOT}"));
        assert_eq!(CP_TAG, format!("cp{NODOT}"));
        assert_eq!(PYLIB_STEM, format!("python{NODOT}"));
        assert_eq!(HEADER_TREE, format!("cpython{NODOT}"));
        assert_eq!(HEX, 0x030d_00f0);
    }
}

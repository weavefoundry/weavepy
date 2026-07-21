//! RFC 0053 WS1 — the materialized stdlib tree.
//!
//! WeavePy's pure-Python stdlib ships *inside* the binary
//! (`include_str!` frozen sources; see `stdlib::frozen_sources`). That
//! is great for startup (RFC 0021's frozen-code cache) and terrible
//! for identity: a module whose `__file__` is `<frozen argparse>`
//! breaks every consumer that treats `__file__` as a path —
//! `open(module.__file__)`, `inspect.getsource`, `linecache`,
//! doctest, coverage tools, and `test.support`'s directory math.
//!
//! This module guarantees an on-disk mirror of the embedded stdlib
//! exists and hands out real paths for frozen modules. The embedded
//! sources remain the *execution* source of truth — nothing is ever
//! imported *from* the tree by the fast path — the tree is a
//! byte-identical projection used as the module's filesystem
//! identity. Skew is structurally impossible: the tree lives under a
//! directory keyed by a hash of every embedded source (plus the crate
//! version), so a rebuilt binary materializes a fresh tree instead of
//! mislabeling an old one.
//!
//! Resolution order (getpath-shaped):
//!
//! 1. `WEAVEPY_NO_STDLIB_TREE` disables the tree entirely (modules
//!    keep their `<frozen name>` pseudo-filenames — the pre-RFC-0053
//!    behavior, and the graceful degradation mode for read-only
//!    filesystems).
//! 2. `WEAVEPYHOME` (or `PYTHONHOME`): `{home}/lib/weavepy3.13` is
//!    accepted if the `os.py` landmark exists — an installed layout.
//! 3. Landmark search relative to the executable: any ancestor `d`
//!    of the binary with `d/lib/weavepy3.13/os.py`.
//! 4. Fallback: a per-build cache prefix under the user cache
//!    directory, extracted on demand (idempotent, concurrency-safe:
//!    write to a temp dir, `rename` into place, `COMPLETE` marker).

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Implementation-specific lib directory name. Deliberately
/// `weavepy3.13`, not `python3.13`, so a WeavePy tree and a CPython
/// install can never shadow each other (same trade as the
/// `weavepy-313` bytecode cache tag from RFC 0033).
pub const LIB_DIR_NAME: &str = "weavepy3.13";

const COMPLETE_MARKER: &str = ".weavepy-complete";

/// Non-`.py` files materialized into the stdlib tree (RFC 0055 WS2):
/// the verbatim `venv` activation scripts, found by
/// `venv.EnvBuilder.setup_scripts` relative to `venv.__file__`.
/// Paths are relative to the stdlib dir, `/`-separated.
const DATA_FILES: &[(&str, &str)] = &[
    (
        "venv/scripts/common/activate",
        include_str!("stdlib/python/venv/scripts/common/activate"),
    ),
    (
        "venv/scripts/common/Activate.ps1",
        include_str!("stdlib/python/venv/scripts/common/Activate.ps1"),
    ),
    (
        "venv/scripts/common/activate.fish",
        include_str!("stdlib/python/venv/scripts/common/activate.fish"),
    ),
    (
        "venv/scripts/posix/activate.csh",
        include_str!("stdlib/python/venv/scripts/posix/activate.csh"),
    ),
];

/// The bundled pip wheel's filename. The version must agree with
/// `ensurepip._PIP_VERSION` and the frozen pip facade's
/// `pip.__version__` — `ensurepip` derives the resource name from its
/// `_PIP_VERSION` and refuses to uninstall a mismatched install.
pub const PIP_WHEEL_NAME: &str = "pip-24.0.0+weavepy-py3-none-any.whl";

/// The materialized stdlib directory (`…/lib/weavepy3.13`) for this
/// process, or `None` when disabled or unavailable. Resolved once;
/// the warm path after first call is a pointer read.
pub fn stdlib_dir() -> Option<&'static Path> {
    static DIR: OnceLock<Option<PathBuf>> = OnceLock::new();
    DIR.get_or_init(resolve).as_deref()
}

/// The installation prefix implied by the stdlib dir
/// (`{prefix}/lib/weavepy3.13`).
pub fn prefix() -> Option<&'static Path> {
    stdlib_dir().and_then(|d| d.parent()).and_then(Path::parent)
}

/// The on-disk path a frozen module's `__file__`/`co_filename` should
/// carry, or `None` when the tree is unavailable. The path is
/// guaranteed to exist and hold exactly the embedded source.
pub fn module_path(name: &str, is_package: bool) -> Option<PathBuf> {
    let dir = stdlib_dir()?;
    Some(dir.join(rel_path(name, is_package)))
}

/// Whether `path` points inside the materialized tree. Used by the
/// import machinery to keep its "a frozen package also present on
/// `sys.path` reports the disk copy" rule scoped to *foreign* disk
/// copies (a vendored CPython `Lib/`), not our own mirror.
pub fn contains(path: &Path) -> bool {
    match stdlib_dir() {
        Some(dir) => path.starts_with(dir),
        None => false,
    }
}

/// Module name → path relative to the stdlib dir. Dotted names become
/// directories; packages land on their `__init__.py`.
fn rel_path(name: &str, is_package: bool) -> PathBuf {
    let mut p: PathBuf = name.split('.').collect();
    if is_package {
        p.push("__init__.py");
    } else {
        p.set_extension("py");
    }
    p
}

/// Layout version of the materialized tree itself (directory shape,
/// aliases, markers — anything `materialize` writes that is not an
/// embedded source). Bump when the shape changes so existing caches
/// keyed on unchanged sources are not mistaken for the new layout.
const TREE_FORMAT: &str = "tree-format-3";

/// FNV-1a over every frozen module's name and source, mixed with the
/// crate version. Any change to any embedded byte lands in a new
/// cache directory.
fn build_id() -> u64 {
    const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut h = FNV_OFFSET;
    let mut eat = |bytes: &[u8]| {
        for &b in bytes {
            h ^= u64::from(b);
            h = h.wrapping_mul(FNV_PRIME);
        }
    };
    eat(env!("CARGO_PKG_VERSION").as_bytes());
    eat(TREE_FORMAT.as_bytes());
    for src in crate::stdlib::frozen_sources() {
        eat(src.name.as_bytes());
        eat(&[u8::from(src.is_package)]);
        eat(src.source.as_bytes());
    }
    for (path, contents) in DATA_FILES {
        eat(path.as_bytes());
        eat(contents.as_bytes());
    }
    h
}

/// Build the bundled pip wheel (RFC 0055 WS2) from the frozen pip
/// facade source. A wheel is a zip archive; entries are stored
/// uncompressed with a fixed 1980-01-01 DOS timestamp so the bytes
/// are deterministic for a given build.
fn pip_wheel_bytes() -> Vec<u8> {
    let pip_source = crate::stdlib::frozen_sources()
        .iter()
        .find(|s| s.name == "_minipip")
        .map_or("", |s| s.source);
    let version = PIP_WHEEL_NAME
        .trim_start_matches("pip-")
        .split("-py3-none-any.whl")
        .next()
        .unwrap_or("0");
    let dist_info = format!("pip-{version}.dist-info");
    let metadata = format!(
        "Metadata-Version: 2.1\n\
         Name: pip\n\
         Version: {version}\n\
         Summary: WeavePy's bundled pip-compatible installer (the frozen pip facade, RFC 0030).\n\
         License: MIT OR Apache-2.0\n\
         Requires-Python: >=3.8\n"
    );
    let wheel_meta = "Wheel-Version: 1.0\n\
         Generator: weavepy\n\
         Root-Is-Purelib: true\n\
         Tag: py3-none-any\n";
    let entry_points = "[console_scripts]\n\
         pip = pip:main\n\
         pip3 = pip:main\n";

    let mut entries: Vec<(String, Vec<u8>)> = vec![
        ("pip.py".to_owned(), pip_source.as_bytes().to_vec()),
        (format!("{dist_info}/METADATA"), metadata.into_bytes()),
        (format!("{dist_info}/WHEEL"), wheel_meta.as_bytes().to_vec()),
        (
            format!("{dist_info}/entry_points.txt"),
            entry_points.as_bytes().to_vec(),
        ),
    ];
    // RECORD: `path,sha256=<urlsafe-b64-nopad>,<size>` per PEP 376,
    // with the RECORD row itself left hashless.
    let mut record = String::new();
    for (name, data) in &entries {
        use base64::Engine as _;
        use sha2::Digest as _;
        let digest = sha2::Sha256::digest(data);
        let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest);
        record.push_str(&format!("{name},sha256={b64},{}\n", data.len()));
    }
    record.push_str(&format!("{dist_info}/RECORD,,\n"));
    entries.push((format!("{dist_info}/RECORD"), record.into_bytes()));

    // Minimal stored-entry zip (PKZIP appnote 4.4.x): local headers,
    // central directory, end-of-central-directory.
    let mut out: Vec<u8> = Vec::new();
    let mut central: Vec<u8> = Vec::new();
    let mut count: u16 = 0;
    for (name, data) in &entries {
        let crc = crc32fast::hash(data);
        let offset = u32::try_from(out.len()).unwrap_or(u32::MAX);
        let name_bytes = name.as_bytes();
        let size = u32::try_from(data.len()).unwrap_or(u32::MAX);
        // Local file header.
        out.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
        out.extend_from_slice(&20u16.to_le_bytes()); // version needed
        out.extend_from_slice(&0u16.to_le_bytes()); // flags
        out.extend_from_slice(&0u16.to_le_bytes()); // method: stored
        out.extend_from_slice(&0u16.to_le_bytes()); // mod time
        out.extend_from_slice(&0x21u16.to_le_bytes()); // mod date: 1980-01-01
        out.extend_from_slice(&crc.to_le_bytes());
        out.extend_from_slice(&size.to_le_bytes()); // csize
        out.extend_from_slice(&size.to_le_bytes()); // usize
        out.extend_from_slice(&u16::try_from(name_bytes.len()).unwrap_or(0).to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes()); // extra len
        out.extend_from_slice(name_bytes);
        out.extend_from_slice(data);
        // Central directory entry.
        central.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
        central.extend_from_slice(&20u16.to_le_bytes()); // version made by
        central.extend_from_slice(&20u16.to_le_bytes()); // version needed
        central.extend_from_slice(&0u16.to_le_bytes()); // flags
        central.extend_from_slice(&0u16.to_le_bytes()); // method
        central.extend_from_slice(&0u16.to_le_bytes()); // mod time
        central.extend_from_slice(&0x21u16.to_le_bytes()); // mod date
        central.extend_from_slice(&crc.to_le_bytes());
        central.extend_from_slice(&size.to_le_bytes());
        central.extend_from_slice(&size.to_le_bytes());
        central.extend_from_slice(&u16::try_from(name_bytes.len()).unwrap_or(0).to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes()); // extra len
        central.extend_from_slice(&0u16.to_le_bytes()); // comment len
        central.extend_from_slice(&0u16.to_le_bytes()); // disk number
        central.extend_from_slice(&0u16.to_le_bytes()); // internal attrs
        central.extend_from_slice(&0u32.to_le_bytes()); // external attrs
        central.extend_from_slice(&offset.to_le_bytes());
        central.extend_from_slice(name_bytes);
        count += 1;
    }
    let cd_offset = u32::try_from(out.len()).unwrap_or(u32::MAX);
    let cd_size = u32::try_from(central.len()).unwrap_or(u32::MAX);
    out.extend_from_slice(&central);
    // End of central directory.
    out.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes()); // this disk
    out.extend_from_slice(&0u16.to_le_bytes()); // cd start disk
    out.extend_from_slice(&count.to_le_bytes());
    out.extend_from_slice(&count.to_le_bytes());
    out.extend_from_slice(&cd_size.to_le_bytes());
    out.extend_from_slice(&cd_offset.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes()); // comment len
    out
}

fn resolve() -> Option<PathBuf> {
    if std::env::var_os("WEAVEPY_NO_STDLIB_TREE").is_some() {
        return None;
    }
    // Installed layouts: an explicit home, then the executable's
    // ancestors. getpath uses `os.py` as its landmark; WeavePy's `os`
    // is Rust-native (never on disk), so the landmark is the tree's
    // own completion marker.
    for var in ["WEAVEPYHOME", "PYTHONHOME"] {
        if let Some(home) = std::env::var_os(var) {
            if home.is_empty() {
                continue;
            }
            let candidate = PathBuf::from(home).join("lib").join(LIB_DIR_NAME);
            if candidate.join(COMPLETE_MARKER).is_file() {
                return Some(candidate);
            }
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        let mut dir = exe.parent();
        while let Some(d) = dir {
            let candidate = d.join("lib").join(LIB_DIR_NAME);
            if candidate.join(COMPLETE_MARKER).is_file() {
                return Some(candidate);
            }
            dir = d.parent();
        }
    }
    // Cache fallback: materialize under the user cache directory.
    let root = cache_root()?;
    let prefix = root.join(format!("{:016x}", build_id()));
    let lib = prefix.join("lib").join(LIB_DIR_NAME);
    if lib.join(COMPLETE_MARKER).is_file() {
        return Some(lib);
    }
    materialize(&prefix).then_some(lib)
}

fn cache_root() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("WEAVEPY_STDLIB_CACHE") {
        if !dir.is_empty() {
            return Some(PathBuf::from(dir));
        }
    }
    #[cfg(target_os = "macos")]
    {
        let home = std::env::var_os("HOME")?;
        Some(
            PathBuf::from(home)
                .join("Library")
                .join("Caches")
                .join("weavepy"),
        )
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        if let Some(xdg) = std::env::var_os("XDG_CACHE_HOME") {
            if !xdg.is_empty() {
                return Some(PathBuf::from(xdg).join("weavepy"));
            }
        }
        let home = std::env::var_os("HOME")?;
        Some(PathBuf::from(home).join(".cache").join("weavepy"))
    }
    #[cfg(windows)]
    {
        let base = std::env::var_os("LOCALAPPDATA")?;
        Some(PathBuf::from(base).join("weavepy"))
    }
}

/// Extract the embedded stdlib into `{prefix}/lib/weavepy3.13`.
/// Concurrency-safe against sibling WeavePy processes (the regrtest
/// harness spawns many at once on a cold cache): each writer builds a
/// private `{prefix}.tmp-{pid}` tree and renames it into place; the
/// loser of the race removes its temp copy and uses the winner's.
fn materialize(prefix: &Path) -> bool {
    let Some(parent) = prefix.parent() else {
        return false;
    };
    let tmp_prefix = parent.join(format!(
        ".tmp-{}-{}",
        prefix
            .file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned()),
        std::process::id()
    ));
    let tmp_lib = tmp_prefix.join("lib").join(LIB_DIR_NAME);
    let write_tree = || -> std::io::Result<()> {
        std::fs::create_dir_all(&tmp_lib)?;
        for src in crate::stdlib::frozen_sources() {
            let path = tmp_lib.join(rel_path(src.name, src.is_package));
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            std::fs::write(&path, src.source)?;
        }
        // `sysconfig`'s posix_prefix scheme derives `stdlib`/`purelib`
        // as `{prefix}/lib/python3.13[/site-packages]`. Make those
        // paths real inside our private prefix: a `python3.13` symlink
        // onto the tree (POSIX only) and an empty `site-packages`.
        // Nothing outside this hash-keyed prefix ever resolves the
        // alias, so it cannot shadow a CPython install.
        std::fs::create_dir_all(tmp_lib.join("site-packages"))?;
        #[cfg(unix)]
        {
            let alias = tmp_prefix
                .join("lib")
                .join(format!("python{}", &LIB_DIR_NAME["weavepy".len()..]));
            let _ = std::os::unix::fs::symlink(LIB_DIR_NAME, alias);
        }
        // RFC 0055 WS1 — the installation artifacts `sysconfig`
        // points at must exist for the surface to be truthful:
        // `get_makefile_filename()` → `{stdlib}/config-3.13-{multiarch}/
        // Makefile` (also `srcdir`), and `get_config_h_filename()` →
        // `{prefix}/include/python3.13/pyconfig.h`. Both carry the
        // same variables the frozen `_weave_sysconfigdata` reports, in
        // CPython's on-disk formats (`_parse_makefile`/`parse_config_h`
        // can read them back).
        {
            let version_short = &LIB_DIR_NAME["weavepy".len()..];
            let multiarch = crate::stdlib::sysconfig_native::MULTIARCH;
            let config_dir_name = if multiarch.is_empty() {
                format!("config-{version_short}")
            } else {
                format!("config-{version_short}-{multiarch}")
            };
            let config_dir = tmp_lib.join(config_dir_name);
            std::fs::create_dir_all(&config_dir)?;
            std::fs::write(
                config_dir.join("Makefile"),
                format!(
                    "# Generated by WeavePy (RFC 0055); mirrors _sysconfigdata.\n\
                     VERSION=\t{version_short}\n\
                     ABIFLAGS=\t\n\
                     SOABI=\t{soabi}\n\
                     EXT_SUFFIX=\t{ext_suffix}\n\
                     MULTIARCH=\t{multiarch}\n\
                     LIBRARY=\tlibpython{version_short}.a\n\
                     LDLIBRARY=\tlibpython{version_short}.a\n\
                     Py_DEBUG=\t0\n\
                     Py_GIL_DISABLED=\t0\n",
                    soabi = crate::stdlib::sysconfig_native::SOABI,
                    ext_suffix = crate::stdlib::sysconfig_native::EXT_SUFFIX,
                ),
            )?;
            let include_dir = tmp_prefix
                .join("include")
                .join(format!("python{version_short}"));
            std::fs::create_dir_all(&include_dir)?;
            std::fs::write(
                include_dir.join("pyconfig.h"),
                "/* Generated by WeavePy (RFC 0055); mirrors _sysconfigdata. */\n\
                 #define PY_VERSION_HEX 0x030d00f0\n\
                 #define SIZEOF_VOID_P 8\n\
                 #define WITH_DOC_STRINGS 1\n\
                 /* #undef Py_DEBUG */\n\
                 /* #undef Py_GIL_DISABLED */\n\
                 /* #undef Py_TRACE_REFS */\n",
            )?;
        }
        // RFC 0055 WS2 — data files (venv activation scripts) and the
        // bundled pip wheel `importlib.resources.files('ensurepip')`
        // resolves against.
        for (rel, contents) in DATA_FILES {
            let path: PathBuf = tmp_lib.join(rel.split('/').collect::<PathBuf>());
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            std::fs::write(&path, contents)?;
        }
        {
            let bundled = tmp_lib.join("ensurepip").join("_bundled");
            std::fs::create_dir_all(&bundled)?;
            std::fs::write(bundled.join(PIP_WHEEL_NAME), pip_wheel_bytes())?;
        }
        std::fs::write(
            tmp_lib.join(COMPLETE_MARKER),
            format!("{:016x}\n", build_id()),
        )?;
        Ok(())
    };
    let ok = write_tree().is_ok();
    if !ok {
        let _ = std::fs::remove_dir_all(&tmp_prefix);
        return false;
    }
    match std::fs::rename(&tmp_prefix, prefix) {
        Ok(()) => true,
        Err(_) => {
            // Lost the race (or the destination already exists from a
            // previous partial run). Use the winner's tree if it is
            // complete; otherwise give up gracefully.
            let _ = std::fs::remove_dir_all(&tmp_prefix);
            prefix
                .join("lib")
                .join(LIB_DIR_NAME)
                .join(COMPLETE_MARKER)
                .is_file()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rel_path_shapes() {
        assert_eq!(rel_path("argparse", false), PathBuf::from("argparse.py"));
        assert_eq!(
            rel_path("test.support", true),
            PathBuf::from("test/support/__init__.py")
        );
        assert_eq!(
            rel_path("email.mime.text", false),
            PathBuf::from("email/mime/text.py")
        );
    }

    #[test]
    fn build_id_is_stable_within_process() {
        assert_eq!(build_id(), build_id());
    }
}

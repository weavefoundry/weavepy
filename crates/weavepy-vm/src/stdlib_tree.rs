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
const TREE_FORMAT: &str = "tree-format-2";

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
    h
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

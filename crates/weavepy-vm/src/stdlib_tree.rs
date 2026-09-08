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
//! 2. `WEAVEPYHOME` (or `PYTHONHOME`): `{home}/lib/weavepy3.14` is
//!    accepted if the `os.py` landmark exists — an installed layout.
//! 3. Landmark search relative to the executable: any ancestor `d`
//!    of the binary with `d/lib/weavepy3.14/os.py`.
//! 4. Fallback: a per-build cache prefix under the user cache
//!    directory, extracted on demand (idempotent, concurrency-safe:
//!    write to a temp dir, `rename` into place, `COMPLETE` marker).

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Implementation-specific lib directory name. Deliberately
/// `weavepy3.14`, not `python3.14`, so a WeavePy tree and a CPython
/// install can never shadow each other (same trade as the
/// `weavepy-314` bytecode cache tag from RFC 0033).
pub const LIB_DIR_NAME: &str = weavepy_version::LIB_DIR_NAME;

const COMPLETE_MARKER: &str = ".weavepy-complete";

use crate::stdlib::tree_manifest::DATA_FILES;

/// The bundled pip wheel's filename. The version must agree with
/// `ensurepip._PIP_VERSION` and the frozen pip facade's
/// `pip.__version__` — `ensurepip` derives the resource name from its
/// `_PIP_VERSION` and refuses to uninstall a mismatched install.
pub const PIP_WHEEL_NAME: &str = "pip-24.0.0+weavepy-py3-none-any.whl";

/// The materialized stdlib directory (`…/lib/weavepy3.14`) for this
/// process, or `None` when disabled or unavailable. Resolved once;
/// the warm path after first call is a pointer read.
pub fn stdlib_dir() -> Option<&'static Path> {
    static DIR: OnceLock<Option<PathBuf>> = OnceLock::new();
    DIR.get_or_init(resolve).as_deref()
}

/// The installation prefix implied by the stdlib dir
/// (`{prefix}/lib/weavepy3.14`).
pub fn prefix() -> Option<&'static Path> {
    stdlib_dir().and_then(|d| d.parent()).and_then(Path::parent)
}

/// Whether `prefix` carries an installed WeavePy home — a complete
/// `{prefix}/lib/weavepy3.14` tree. RFC 0075 (embedding): CPython's
/// getpath falls back to computing the prefix from the *shared
/// library's* location when the program itself is a foreign embedder
/// binary; `Py_InitializeFromConfig` uses this predicate to run the
/// same probe over the `libpython3.14` on-disk path (dladdr).
pub fn is_home_prefix(prefix: &Path) -> bool {
    prefix
        .join("lib")
        .join(LIB_DIR_NAME)
        .join(COMPLETE_MARKER)
        .is_file()
}

/// The on-disk path a frozen module's `__file__`/`co_filename` should
/// carry, or `None` when the tree is unavailable. The path is
/// guaranteed to exist and hold exactly the embedded source.
pub fn module_path(name: &str, is_package: bool) -> Option<PathBuf> {
    // RFC 0057 WS3 — the frozen *test* modules keep their `<frozen …>`
    // pseudo-filename even though the tree carries their sources. In
    // CPython they import with `origin='frozen'` and
    // `loader is FrozenImporter` (`test_frozen` asserts the loader
    // identity); a real path here would re-label them as
    // SourceFileLoader modules via `_weave_spec`'s taxonomy.
    if crate::import::is_test_frozen_name(name) {
        return None;
    }
    let dir = stdlib_dir()?;
    Some(dir.join(rel_path(name, is_package)))
}

/// The tree projection of a frozen test module's source, bypassing the
/// `module_path` pseudo-filename exception. This is the disk copy the
/// loader falls back to when `_imp._override_frozen_modules_for_tests`
/// disables the frozen import (CPython finds the same files in its
/// on-`sys.path` stdlib directory).
pub fn test_frozen_disk_path(name: &str, is_package: bool) -> Option<PathBuf> {
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

/// Computed from the embedded sources at build time. Reading the constant
/// avoids hashing and paging in the whole stdlib on every process startup.
fn build_id() -> u64 {
    include!(concat!(env!("OUT_DIR"), "/stdlib_build_id.rs"))
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

/// RFC 0075 (embedding): `PyConfig.program_name` override for
/// [`program_exe`]. An embedder's process argv[0] names the *host*
/// binary; CPython's getpath computes `program_full_path` from the
/// configured program name instead (bare names PATH-searched, then
/// cwd-joined). Set by `Py_InitializeFromConfig` before the owned
/// interpreter is constructed.
static PROGRAM_NAME_OVERRIDE: std::sync::Mutex<Option<PathBuf>> = std::sync::Mutex::new(None);

pub fn set_program_name_override(name: Option<PathBuf>) {
    *PROGRAM_NAME_OVERRIDE.lock().unwrap() = name;
}

/// The interpreter's program path, symlink identity preserved.
///
/// CPython's getpath computes `program_full_path` from argv[0] (made
/// absolute, PATH-searched when bare) and only then resolves symlinks
/// as a *fallback* — the unresolved path is what venv detection keys
/// on. `std::env::current_exe()` is wrong for that job on Linux: it
/// reads `/proc/self/exe`, which the kernel pre-resolves, so a venv's
/// `bin/python -> …/artifact/bin/weavepy` symlink loses its identity
/// and `pyvenv.cfg` is never found (macOS returns the exec'd path and
/// dodges this). Falls back to `current_exe()` when argv[0] is absent
/// or doesn't name a real file (misleading custom argv0).
pub fn program_exe() -> Option<PathBuf> {
    if let Some(name) = PROGRAM_NAME_OVERRIDE.lock().unwrap().clone() {
        // getpath's program_full_path: relative-with-separator paths
        // are cwd-anchored; bare names are PATH-searched, and an
        // unresolved bare name still lands as `{cwd}/{name}`
        // (test_embed's test_pre_initialization_api expects exactly
        // that for the never-on-PATH name "spam").
        if name.components().count() > 1 {
            return std::path::absolute(&name).ok();
        }
        let found = std::env::var_os("PATH").and_then(|path| {
            std::env::split_paths(&path)
                .filter(|d| !d.as_os_str().is_empty())
                .map(|d| d.join(&name))
                .find(|p| p.is_file())
        });
        return Some(found.unwrap_or_else(|| {
            std::env::current_dir()
                .map(|d| d.join(&name))
                .unwrap_or(name)
        }));
    }
    let argv0 = std::env::args_os().next().map(PathBuf::from);
    if let Some(argv0) = argv0 {
        let candidate = if argv0.components().count() > 1 {
            // Carries a separator: shell-style, relative to the cwd.
            std::path::absolute(&argv0).ok()
        } else {
            // Bare name: PATH search, like a shell (and getpath).
            std::env::var_os("PATH").and_then(|path| {
                std::env::split_paths(&path)
                    .filter(|d| !d.as_os_str().is_empty())
                    .map(|d| d.join(&argv0))
                    .find(|p| p.is_file())
            })
        };
        if let Some(p) = candidate {
            if p.is_file() {
                return Some(p);
            }
        }
    }
    std::env::current_exe().ok()
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
            // CPython accepts `PYTHONHOME=<prefix>[:<exec_prefix>]`
            // (`;` on Windows). The stdlib tree lives under the first
            // component; the exec_prefix half only matters for
            // platform-specific lib-dynload layouts WeavePy doesn't
            // have (RFC 0062 WS5).
            let sep = if cfg!(windows) { ';' } else { ':' };
            let home = home.to_string_lossy().into_owned();
            let prefix = home.split_once(sep).map_or(home.as_str(), |(p, _)| p);
            if prefix.is_empty() {
                continue;
            }
            let candidate = PathBuf::from(prefix).join("lib").join(LIB_DIR_NAME);
            if candidate.join(COMPLETE_MARKER).is_file() {
                return Some(candidate);
            }
        }
    }
    if let Some(exe) = program_exe() {
        if let Some(found) = landmark_walk(&exe) {
            return Some(found);
        }
        // Venv chaining (RFC 0062 WS5): a venv interpreter is a
        // symlink/copy whose own ancestors carry no stdlib. CPython's
        // getpath reads pyvenv.cfg's `home` key (the base
        // interpreter's bin directory) and resumes the landmark search
        // from there; without this, a venv made from a relocatable
        // artifact silently fell back to the materialize cache and
        // `sys.base_prefix` pointed outside the installation.
        if let Some(home) = venv_home_dir(&exe) {
            let mut dir = Some(home.as_path());
            while let Some(d) = dir {
                let candidate = d.join("lib").join(LIB_DIR_NAME);
                if candidate.join(COMPLETE_MARKER).is_file() {
                    return Some(candidate);
                }
                dir = d.parent();
            }
        }
        // Symlink chasing (RFC 0062 WS5): a bare symlink from outside
        // the installation (`~/bin/python3 -> …/artifact/bin/weavepy`)
        // has no pyvenv.cfg and its raw ancestors carry no landmark.
        // getpath resolves symlinks on the program path before the
        // prefix search; mirror that with a canonicalized retry so the
        // artifact self-locates instead of materializing a decoy cache
        // (whose foreign prefix also let the host Python's
        // site-packages leak into `sys.path`).
        if let Ok(real) = std::fs::canonicalize(&exe) {
            if real != exe {
                if let Some(found) = landmark_walk(&real) {
                    return Some(found);
                }
            }
        }
    }
    // Cache fallback: materialize under the user cache directory.
    let root = cache_root()?;
    let prefix = root.join(format!("{:016x}", build_id()));
    let lib = prefix.join("lib").join(LIB_DIR_NAME);
    if lib.join(COMPLETE_MARKER).is_file() {
        // The cache tree is keyed on the build, not the binary's location,
        // so another copy of this same build (a probe under /tmp, a
        // renamed artifact) may have written the exe-derived metadata
        // (`BINDIR`, `base_interpreter`). Re-key it to *this* executable
        // when it disagrees (`test_build_details.test_base_interpreter`).
        #[cfg(unix)]
        refresh_install_metadata(&prefix, &lib);
        return Some(lib);
    }
    materialize(&prefix).then_some(lib)
}

/// Rewrite the two exe-dependent metadata files under `lib` when
/// `build-details.json` names a different interpreter than the running one.
#[cfg(unix)]
fn refresh_install_metadata(prefix: &Path, lib: &Path) {
    let Some(exe) = program_exe() else { return };
    let current = std::fs::read_to_string(lib.join("build-details.json")).unwrap_or_default();
    let recorded = serde_json::from_str::<serde_json::Value>(&current)
        .ok()
        .and_then(|v| {
            v.get("base_interpreter")
                .and_then(|b| b.as_str().map(str::to_owned))
        });
    if recorded.as_deref() == Some(exe.to_string_lossy().as_ref()) {
        return;
    }
    let _ = write_install_json(lib, prefix);
}

/// Walk `exe`'s ancestors for the installed-layout landmark
/// (`{d}/lib/weavepy3.14/.weavepy-complete`).
///
/// The walk starts at the exe's own directory, which covers both
/// artifact shapes without a special case: the POSIX layout
/// (`{prefix}/bin/weavepy`) finds the landmark one level up, and the
/// RFC 0063 WS6 NT layout — `python.exe` at the prefix root, no
/// `bin\` — finds `{prefix}/lib/weavepy3.14` on the very first probe.
fn landmark_walk(exe: &Path) -> Option<PathBuf> {
    let mut dir = exe.parent();
    while let Some(d) = dir {
        let candidate = d.join("lib").join(LIB_DIR_NAME);
        if candidate.join(COMPLETE_MARKER).is_file() {
            return Some(candidate);
        }
        dir = d.parent();
    }
    None
}

/// The `home` value from the pyvenv.cfg governing `exe`, when `exe`
/// lives inside a virtual environment (getpath's venv detection:
/// pyvenv.cfg sits next to the binary or one directory up).
fn venv_home_dir(exe: &Path) -> Option<PathBuf> {
    let exe_dir = exe.parent()?;
    let cfg = [
        exe_dir.join("pyvenv.cfg"),
        exe_dir.parent()?.join("pyvenv.cfg"),
    ]
    .into_iter()
    .find(|p| p.is_file())?;
    let contents = std::fs::read_to_string(cfg).ok()?;
    contents.lines().find_map(|line| {
        let (key, value) = line.split_once('=')?;
        let value = value.trim();
        (key.trim().eq_ignore_ascii_case("home") && !value.is_empty()).then(|| PathBuf::from(value))
    })
}

fn cache_root() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("WEAVEPY_STDLIB_CACHE") {
        if !dir.is_empty() {
            return Some(PathBuf::from(dir));
        }
    }
    #[cfg(target_os = "macos")]
    {
        if let Some(home) = std::env::var_os("HOME") {
            return Some(
                PathBuf::from(home)
                    .join("Library")
                    .join("Caches")
                    .join("weavepy"),
            );
        }
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        if let Some(xdg) = std::env::var_os("XDG_CACHE_HOME") {
            if !xdg.is_empty() {
                return Some(PathBuf::from(xdg).join("weavepy"));
            }
        }
        if let Some(home) = std::env::var_os("HOME") {
            return Some(PathBuf::from(home).join(".cache").join("weavepy"));
        }
    }
    #[cfg(windows)]
    {
        if let Some(base) = std::env::var_os("LOCALAPPDATA") {
            return Some(PathBuf::from(base).join("weavepy"));
        }
    }
    // No home directory in the environment (e.g. a test that scrubs
    // `HOME` — test_pdb's `run_pdb_script(remove_home=True)` re-execs
    // `sys.executable`): fall back to a per-uid tmp cache so the stdlib
    // still materializes and `-m NAME` resolution keeps working.
    #[cfg(unix)]
    let per_user = format!("weavepy-{}", unsafe { libc::getuid() });
    #[cfg(not(unix))]
    let per_user = "weavepy".to_owned();
    Some(std::env::temp_dir().join(per_user))
}

/// Extract the embedded stdlib into `{prefix}/lib/weavepy3.14`.
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
        // as `{prefix}/lib/python3.14[/site-packages]`. Make those
        // paths real inside our private prefix: a `python3.14` symlink
        // onto the tree (POSIX only) and an empty `site-packages`.
        // Nothing outside this hash-keyed prefix ever resolves the
        // alias, so it cannot shadow a CPython install. On Windows the
        // symlink is skipped outright — the `nt` scheme names
        // `lib/weavepy3.14` directly (RFC 0063 WS6), so there is no
        // alias to make real (and NTFS symlinks need privileges).
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
        // `get_makefile_filename()` → `{stdlib}/config-3.14-{multiarch}/
        // Makefile` (also `srcdir`), and `get_config_h_filename()` →
        // `{prefix}/include/python3.14/pyconfig.h`. Both carry the
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
                    "# Generated by WeavePy (RFC 0055/0062); mirrors _sysconfigdata.\n\
                     VERSION=\t{version_short}\n\
                     ABIFLAGS=\t\n\
                     SOABI=\t{soabi}\n\
                     EXT_SUFFIX=\t{ext_suffix}\n\
                     MULTIARCH=\t{multiarch}\n\
                     LIBRARY=\tlibpython{version_short}.a\n\
                     LDLIBRARY=\tlibpython{version_short}.a\n\
                     CC=\t{cc}\n\
                     CXX=\t{cxx}\n\
                     CFLAGS=\t{cflags}\n\
                     CCSHARED=\t{ccshared}\n\
                     LDSHARED=\t{ldshared}\n\
                     BLDSHARED=\t{ldshared}\n\
                     LDCXXSHARED=\t{ldcxxshared}\n\
                     OPT=\t{opt}\n\
                     AR=\tar\n\
                     ARFLAGS=\trcs\n\
                     Py_DEBUG=\t0\n\
                     Py_GIL_DISABLED=\t0\n",
                    soabi = crate::stdlib::sysconfig_native::SOABI,
                    ext_suffix = crate::stdlib::sysconfig_native::EXT_SUFFIX,
                    cc = crate::stdlib::sysconfig_native::CC,
                    cxx = crate::stdlib::sysconfig_native::CXX,
                    cflags = crate::stdlib::sysconfig_native::CFLAGS,
                    ccshared = crate::stdlib::sysconfig_native::CCSHARED,
                    ldshared = crate::stdlib::sysconfig_native::LDSHARED,
                    ldcxxshared = crate::stdlib::sysconfig_native::LDCXXSHARED,
                    opt = crate::stdlib::sysconfig_native::OPT,
                ),
            )?;
            // RFC 0062 WS2 — the installable header surface. A real
            // CPython install ships the full `Include/` tree plus the
            // generated `pyconfig.h` under `{prefix}/include/python3.14/`
            // on POSIX and directly under `{prefix}\Include` on Windows
            // (RFC 0063 WS6 — sysconfig's `nt` scheme and therefore
            // `INCLUDEPY` resolve there, with no versioned subdir).
            // That directory is what setuptools hands to the compiler
            // for an sdist's C extensions. Write the embedded stock
            // tree (vendored, PSF-licensed) and the per-OS `pyconfig.h`.
            let include_dir = if cfg!(windows) {
                tmp_prefix.join("Include")
            } else {
                tmp_prefix
                    .join("include")
                    .join(format!("python{version_short}"))
            };
            for (rel, contents) in crate::cpython_headers::CPYTHON_HEADERS {
                let path: PathBuf = include_dir.join(rel.split('/').collect::<PathBuf>());
                if let Some(dir) = path.parent() {
                    std::fs::create_dir_all(dir)?;
                }
                std::fs::write(&path, contents)?;
            }
            std::fs::write(
                include_dir.join("pyconfig.h"),
                crate::cpython_headers::PYCONFIG_H.unwrap_or(
                    // Platforms without a real generated pyconfig
                    // (not macOS/Linux/Windows — those all embed one,
                    // Windows since RFC 0064 WS3) keep the pre-0062
                    // stub.
                    "/* Generated by WeavePy (RFC 0055); mirrors _sysconfigdata. */\n\
                     #define PY_VERSION_HEX 0x030d00f0\n\
                     #define SIZEOF_VOID_P 8\n\
                     #define WITH_DOC_STRINGS 1\n\
                     /* #undef Py_DEBUG */\n\
                     /* #undef Py_GIL_DISABLED */\n\
                     /* #undef Py_TRACE_REFS */\n",
                ),
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
        // CPython 3.14's installed metadata (POSIX): the
        // `_sysconfig_vars__*.json` snapshot of `sysconfig.get_config_vars()`
        // and PEP 739's `build-details.json`, both keyed on the *final*
        // prefix the tree is renamed into.
        #[cfg(unix)]
        write_install_metadata(&tmp_prefix, &tmp_lib, prefix)?;
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

/// `os.uname()` fields `(sysname, release, machine)`.
#[cfg(unix)]
fn uname_fields() -> (String, String, String) {
    fn field(raw: &[libc::c_char]) -> String {
        let bytes: Vec<u8> = raw
            .iter()
            .take_while(|&&c| c != 0)
            .map(|&c| c as u8)
            .collect();
        String::from_utf8_lossy(&bytes).into_owned()
    }
    // SAFETY: `uname` fills the zeroed struct; the fields are read back
    // as NUL-terminated C strings.
    let mut u: libc::utsname = unsafe { std::mem::zeroed() };
    if unsafe { libc::uname(&raw mut u) } != 0 {
        return (String::new(), String::new(), String::new());
    }
    (field(&u.sysname), field(&u.release), field(&u.machine))
}

/// `sysconfig.get_platform()` for this host, computed the way the
/// verbatim `sysconfig`/`_osx_support` do so `build-details.json`'s
/// `platform` agrees with the running interpreter
/// (`test_build_details.test_platform`).
#[cfg(unix)]
fn sysconfig_platform() -> String {
    let (osname, release, machine) = uname_fields();
    let osname = osname.to_lowercase().replace('/', "");
    let machine = machine.replace(' ', "_").replace('/', "-");
    if osname.starts_with("linux") {
        return format!("{osname}-{machine}");
    }
    if osname == "darwin" {
        // `_osx_support._get_system_version()`: the first two components
        // of `ProductUserVisibleVersion` from SystemVersion.plist.
        let plist = std::fs::read_to_string("/System/Library/CoreServices/SystemVersion.plist")
            .unwrap_or_default();
        let macver = plist
            .split("<key>ProductUserVisibleVersion</key>")
            .nth(1)
            .and_then(|rest| rest.split("<string>").nth(1))
            .and_then(|rest| rest.split("</string>").next())
            .map(|v| v.trim().split('.').take(2).collect::<Vec<_>>().join("."))
            .filter(|v| !v.is_empty());
        return match macver {
            Some(ver) => format!("macosx-{ver}-{machine}"),
            None => format!("{osname}-{release}-{machine}"),
        };
    }
    format!("{osname}-{release}-{machine}")
}

/// Write the 3.14 install-time metadata into the tree being built:
///
/// * `{stdlib}/_sysconfig_vars_{abiflags}_{platform}_{multiarch}.json`
///   — gh-127178's JSON twin of `_sysconfigdata`, holding exactly what
///   `sysconfig.get_config_vars()` reports for this prefix (the frozen
///   `_weave_sysconfigdata` plus the keys `sysconfig._init_config_vars`
///   layers on; `test_sysconfig.test_sysconfigdata_json` diffs the two).
/// * `{stdlib}/build-details.json` — PEP 739 (`test_build_details`).
/// * `{prefix}/lib/pkgconfig/python-3.14.pc` — the relocatable pc file
///   `build-details.json`'s `c_api.pkgconfig_path` points at (the dist
///   tool rewrites it and adds the `python3.pc` aliases).
#[cfg(unix)]
fn write_install_metadata(
    tmp_prefix: &Path,
    tmp_lib: &Path,
    final_prefix: &Path,
) -> std::io::Result<()> {
    write_install_json(tmp_lib, final_prefix)?;
    let version_short = weavepy_version::SHORT;
    let pc_dir = tmp_prefix.join("lib").join("pkgconfig");
    std::fs::create_dir_all(&pc_dir)?;
    std::fs::write(
        pc_dir.join(format!("python-{version_short}.pc")),
        format!(
            "# WeavePy (RFC 0075 WS5) — relocatable via ${{pcfiledir}}.\n\
             prefix=${{pcfiledir}}/../..\n\
             exec_prefix=${{prefix}}\n\
             libdir=${{prefix}}/lib\n\
             includedir=${{prefix}}/include\n\
             \n\
             Name: Python\n\
             Description: Embed WeavePy (CPython {version_short}-compatible) into an application\n\
             Requires:\n\
             Version: {version_short}\n\
             Libs.private: -lm\n\
             Libs: \n\
             Cflags: -I${{includedir}}/python{version_short}\n"
        ),
    )
}

/// Atomically place `contents` at `path` (write-then-rename), so a
/// concurrent reader never sees a truncated JSON file.
#[cfg(unix)]
fn write_atomic(path: &Path, contents: &str) -> std::io::Result<()> {
    let tmp = path.with_extension(format!("tmp-{}", std::process::id()));
    std::fs::write(&tmp, contents)?;
    std::fs::rename(&tmp, path)
}

/// The two exe-dependent JSON files in the stdlib dir:
/// `_sysconfig_vars__*.json` and PEP 739's `build-details.json`, keyed on
/// `final_prefix` and the running executable.
#[cfg(unix)]
fn write_install_json(tmp_lib: &Path, final_prefix: &Path) -> std::io::Result<()> {
    use serde_json::{json, Map, Value};

    let version_short = weavepy_version::SHORT;
    let multiarch = crate::stdlib::sysconfig_native::MULTIARCH;
    let prefix = final_prefix.to_string_lossy().into_owned();
    let join = |parts: &[&str]| -> String {
        let mut p = final_prefix.to_path_buf();
        for part in parts {
            p.push(part);
        }
        p.to_string_lossy().into_owned()
    };
    let python_lib = format!("python{version_short}");
    let libdest = join(&["lib", &python_lib]);
    let includepy = join(&["include", &python_lib]);
    let config_dir_name = if multiarch.is_empty() {
        format!("config-{version_short}")
    } else {
        format!("config-{version_short}-{multiarch}")
    };
    // `srcdir` is `os.path.realpath(dirname(get_makefile_filename()))`;
    // the final prefix doesn't exist yet, so canonicalize its parent.
    let srcdir = final_prefix
        .parent()
        .and_then(|p| std::fs::canonicalize(p).ok())
        .and_then(|real| final_prefix.file_name().map(|n| real.join(n)))
        .unwrap_or_else(|| final_prefix.to_path_buf())
        .join("lib")
        .join(LIB_DIR_NAME)
        .join(&config_dir_name)
        .to_string_lossy()
        .into_owned();
    let exe = program_exe();
    let bindir = exe
        .as_deref()
        .and_then(Path::parent)
        .map_or_else(|| join(&["bin"]), |d| d.to_string_lossy().into_owned());
    let sys_platform = if cfg!(target_os = "macos") {
        "darwin"
    } else if cfg!(target_os = "linux") {
        "linux"
    } else if cfg!(target_os = "freebsd") {
        "freebsd"
    } else {
        "unknown"
    };
    let (ccshared, ldshared, ldcxxshared) = (
        crate::stdlib::sysconfig_native::CCSHARED,
        crate::stdlib::sysconfig_native::LDSHARED,
        crate::stdlib::sysconfig_native::LDCXXSHARED,
    );

    // Mirrors `_weave_sysconfigdata.build_time_vars` (keep in sync) …
    let mut vars: Map<String, Value> = Map::new();
    let mut put = |k: &str, v: Value| {
        vars.insert(k.to_owned(), v);
    };
    put("ABIFLAGS", json!(""));
    put("AR", json!("ar"));
    put("ARFLAGS", json!("rcs"));
    put("BINDIR", json!(bindir));
    put("BINLIBDEST", json!(libdest));
    put("CC", json!(crate::stdlib::sysconfig_native::CC));
    put("CCSHARED", json!(ccshared));
    put("CFLAGS", json!(crate::stdlib::sysconfig_native::CFLAGS));
    put("CONFINCLUDEPY", json!(includepy));
    put("CXX", json!(crate::stdlib::sysconfig_native::CXX));
    put("EXE", json!(""));
    put(
        "EXT_SUFFIX",
        json!(crate::stdlib::sysconfig_native::EXT_SUFFIX),
    );
    put("HOST_GNU_TYPE", json!(""));
    put("INCLUDEPY", json!(includepy));
    put("LDCXXSHARED", json!(ldcxxshared));
    put("LDFLAGS", json!(""));
    put("LDLIBRARY", json!(format!("libpython{version_short}.a")));
    put("LIBPYTHON", json!(""));
    put("LDSHARED", json!(ldshared));
    put("BLDSHARED", json!(ldshared));
    put("LDVERSION", json!(version_short));
    put("OPT", json!(crate::stdlib::sysconfig_native::OPT));
    put("LIBDEST", json!(libdest));
    put("LIBDIR", json!(join(&["lib"])));
    put("LIBRARY", json!(format!("libpython{version_short}.a")));
    put("MULTIARCH", json!(multiarch));
    put("Py_DEBUG", json!(0));
    put("Py_ENABLE_SHARED", json!(0));
    put("Py_GIL_DISABLED", json!(0));
    put("SHLIB_SUFFIX", json!(".so"));
    put("SIZEOF_VOID_P", json!(8));
    put("SOABI", json!(crate::stdlib::sysconfig_native::SOABI));
    put("srcdir", json!(srcdir));
    put(
        "TZPATH",
        json!("/usr/share/zoneinfo:/usr/lib/zoneinfo:/usr/share/lib/zoneinfo:/etc/zoneinfo"),
    );
    put("VERSION", json!(version_short));
    put("WITH_DOC_STRINGS", json!(1));
    put("WITH_PYMALLOC", json!(0));
    put("exec_prefix", json!(prefix));
    put("platlibdir", json!("lib"));
    put("prefix", json!(prefix));
    if cfg!(any(
        target_os = "macos",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "netbsd"
    )) {
        put("HAVE_GETENTROPY", json!(1));
    } else if cfg!(target_os = "linux") {
        put("HAVE_GETRANDOM_SYSCALL", json!(1));
        put("HAVE_GETRANDOM", json!(1));
        put("HAVE_GETENTROPY", json!(1));
    }
    // … plus what `sysconfig._init_config_vars` adds (`userbase` is
    // environment-dependent and excluded by the test on both sides).
    put("py_version", json!(weavepy_version::FULL));
    put("py_version_short", json!(version_short));
    put("py_version_nodot", json!(weavepy_version::NODOT));
    put("installed_base", json!(prefix));
    put("base", json!(prefix));
    put("installed_platbase", json!(prefix));
    put("platbase", json!(prefix));
    put("projectbase", json!(bindir));
    put("implementation", json!("Python"));
    put("implementation_lower", json!("python"));
    put("stdlib_impl_lower", json!("weavepy"));
    put("abiflags", json!(""));
    put("py_version_nodot_plat", json!(""));
    put("abi_thread", json!(""));
    let json_name = format!("_sysconfig_vars__{sys_platform}_{multiarch}.json");
    write_atomic(
        &tmp_lib.join(json_name),
        &serde_json::to_string_pretty(&Value::Object(vars)).unwrap_or_default(),
    )?;

    // PEP 739 build details.
    let version_info = json!({
        "major": weavepy_version::MAJOR,
        "minor": weavepy_version::MINOR,
        "micro": weavepy_version::MICRO,
        "releaselevel": "final",
        "serial": 0,
    });
    let mut implementation = json!({
        "name": "weavepy",
        "cache_tag": crate::pycache::CACHE_TAG,
        "version": version_info.clone(),
        "hexversion": weavepy_version::HEX,
        "supports_isolated_interpreters": true,
    });
    if !multiarch.is_empty() {
        implementation["_multiarch"] = json!(multiarch);
    }
    let mut details = json!({
        "schema_version": "1.0",
        "base_prefix": prefix,
        "platform": sysconfig_platform(),
        "language": {
            "version": version_short,
            "version_info": version_info,
        },
        "implementation": implementation,
        "abi": {
            "flags": [],
            "extension_suffix": crate::stdlib::sysconfig_native::EXT_SUFFIX,
            "stable_abi_suffix": ".abi3.so",
        },
        "suffixes": {
            "source": [".py"],
            "bytecode": [".pyc"],
            "extensions": [crate::stdlib::sysconfig_native::EXT_SUFFIX, ".abi3.so", ".so"],
        },
        "c_api": {
            "headers": includepy,
            "pkgconfig_path": join(&["lib", "pkgconfig"]),
        },
    });
    if let Some(exe) = exe {
        details["base_interpreter"] = json!(exe.to_string_lossy());
    }
    write_atomic(
        &tmp_lib.join("build-details.json"),
        &serde_json::to_string_pretty(&details).unwrap_or_default(),
    )
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
    fn build_id_matches_embedded_manifest() {
        assert_eq!(
            build_id(),
            crate::stdlib::tree_manifest::fingerprint(
                env!("CARGO_PKG_VERSION"),
                crate::stdlib::frozen_sources(),
            ),
        );
    }
}

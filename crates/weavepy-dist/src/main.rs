//! `weavepy-dist`: the relocatable artifact builder + self-check (RFC 0062 WS1).
//!
//! `build` assembles the POSIX install layout the RFC 0053 landmark walk
//! already resolves, named `weavepy-{version}+g{git}-{target-triple}`:
//!
//! ```text
//! weavepy-0.0.0+gabc1234-aarch64-apple-darwin/
//! ├── bin/
//! │   ├── weavepy                  # the release binary
//! │   ├── python3.13 -> weavepy    # POSIX symlinks
//! │   ├── python3    -> weavepy
//! │   └── python     -> weavepy
//! ├── lib/
//! │   ├── weavepy3.13/             # full stdlib tree + .weavepy-complete
//! │   │   └── site-packages/       #   marker + config-3.13*/Makefile
//! │   └── python3.13 -> weavepy3.13
//! ├── include/
//! │   └── python3.13/              # RFC 0062 WS2 header set (Python.h,
//! │       └── ...                  #   pyconfig.h, cpython/, internal/)
//! ├── README.md
//! └── LICENSE-{APACHE,MIT}
//! ```
//!
//! On Windows the artifact takes CPython's NT shape instead (RFC 0063
//! WS6): `weavepy.exe` plus `python.exe`/`python3.exe`/`python3.13.exe`
//! sit at the *prefix root* as real file copies — no `bin/`, no symlinks
//! anywhere in the artifact — and the default format is `zip` (written
//! by bsdtar's `tar -a`). The exes are thin shims over `python313.dll`,
//! which also sits at the prefix root (RFC 0064 WS1), with its MSVC
//! import library at `{prefix}\libs\python313.lib` (WS3 — setuptools'
//! `library_dirs` convention). Headers live at `{prefix}\Include`
//! (CPython's NT shape, where sysconfig's `nt` scheme points); `lib/`
//! is unchanged and the RFC 0053 landmark walk finds
//! `{prefix}/lib/weavepy3.13` from the exe's own directory, so nothing
//! else moves.
//!
//! Rather than reimplementing the stdlib writer, `build` runs the packaged
//! binary itself with `WEAVEPY_STDLIB_CACHE` pointed at a fresh directory,
//! so `stdlib_tree::materialize()` writes the exact tree the binary's
//! embedded sources expect — one writer, one layout, no drift — then moves
//! that tree into the staging root and adds the `bin/` shims (the
//! root-level exe copies on Windows).
//!
//! `check` is the falsifiability half: it extracts the artifact (or builds
//! one) into a scratch prefix and runs a smoke matrix through
//! `bin/python3` (`{prefix}\python3.exe` on Windows) — the shim,
//! deliberately — under a scrubbed environment
//! (no `WEAVEPYHOME`/`PYTHONHOME`/`PYTHONPATH`/`VIRTUAL_ENV`/`WEAVEPY_*`,
//! and `WEAVEPY_STDLIB_CACHE` pointed at an empty decoy directory so a
//! materialize fallback shows up as a check failure instead of silently
//! rescuing a broken artifact). The legs:
//!
//! 1. `version`  — `python3 -V` exits 0 and reports 3.13.
//! 2. `identity` — `sys.prefix`/`base_prefix`/`executable`/`_stdlib_dir`
//!    all resolve inside the artifact; `sysconfig`'s include dir exists
//!    and carries `Python.h` + `pyconfig.h`, and matches `INCLUDEPY`.
//! 3. `stdlib`   — spot-checks crossing native/frozen boundaries:
//!    sqlite3, ssl, zlib, decimal, json, hashlib.
//! 4. `venv`     — `python3 -m venv` then the venv python chains back to
//!    the artifact prefix (`venv/bin/python`; `venv\Scripts\python.exe`
//!    on Windows).
//! 5. `pip`      — offline `pip install` in the venv (needs `--wheels`).
//! 6. `cext`     — compile + import a minimal C extension against the
//!    shipped headers: unix via the `sysconfig` compiler vars (needs cc),
//!    Windows via MSVC `cl /LD` against `libs\python313.lib` (RFC 0064
//!    WS3; SKIP when no toolchain is installed).
//! 7. `decoy-cache` — the decoy stdlib cache stayed empty, proving every
//!    leg ran off the artifact tree itself.

use std::ffi::OsString;
use std::fmt;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand, ValueEnum};

#[derive(Debug, Parser)]
#[command(
    name = "weavepy-dist",
    bin_name = "weavepy-dist",
    about = "Build and self-check relocatable WeavePy distribution artifacts (RFC 0062 WS1).",
    version
)]
struct Cli {
    /// Path to the workspace root. Defaults to the workspace this binary
    /// was compiled from.
    #[arg(long, value_name = "DIR")]
    workspace: Option<PathBuf>,

    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Debug, Subcommand)]
enum Cmd {
    /// Assemble a relocatable artifact from a release `weavepy` binary.
    Build {
        /// Path to the `weavepy` binary to package. Defaults to
        /// `<workspace>/target/release/weavepy`.
        #[arg(long, value_name = "BIN")]
        weavepy: Option<PathBuf>,

        /// Output directory. Defaults to `<workspace>/target/dist`.
        #[arg(long, value_name = "DIR")]
        out: Option<PathBuf>,

        /// Artifact format.
        #[arg(long, value_enum, default_value_t = Format::host_default())]
        format: Format,
    },

    /// Boot an artifact on a clean scratch prefix and run the smoke matrix.
    Check {
        /// Archive (tar.gz or zip) or directory to check. When omitted,
        /// a fresh `dir` artifact is built into the scratch area first.
        #[arg(long, value_name = "PATH")]
        artifact: Option<PathBuf>,

        /// Binary used when `--artifact` is omitted and a build is needed.
        #[arg(long, value_name = "BIN")]
        weavepy: Option<PathBuf>,

        /// Offline wheel cache (`tools/ecosystem_fetch.py` output). When
        /// given, the pip leg installs `six` from it inside the venv.
        #[arg(long, value_name = "DIR")]
        wheels: Option<PathBuf>,

        /// Keep the scratch prefix around for post-mortem.
        #[arg(long, action = clap::ArgAction::SetTrue)]
        keep_scratch: bool,
    },
}

/// Artifact output format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Format {
    /// A gzip tarball created with the system `tar` (preserves symlinks
    /// and modes).
    TarGz,
    /// A zip archive, also created with the system `tar`: `-a` makes
    /// bsdtar pick the format from the `.zip` extension. GNU tar has no
    /// zip writer, but zip is only the default where bsdtar *is* the
    /// system tar (Windows 10+ and the GitHub runners ship it; macOS
    /// too) — on GNU/Linux, `tar.gz` remains the supported archive.
    Zip,
    /// A plain directory tree.
    Dir,
}

impl Format {
    /// The host's conventional archive format: zip on Windows (the NT
    /// artifact has no symlinks to preserve and zip is what Windows
    /// users unpack natively — RFC 0063 WS6), gzip tarball elsewhere.
    /// The builder always packages the host target, so `cfg!(windows)`
    /// is the right switch.
    const fn host_default() -> Self {
        if cfg!(windows) {
            Format::Zip
        } else {
            Format::TarGz
        }
    }
}

fn main() -> ExitCode {
    match real_main() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("weavepy-dist: {err:#}");
            ExitCode::from(1)
        }
    }
}

fn real_main() -> Result<()> {
    let cli = Cli::parse();
    let workspace = resolve_workspace(cli.workspace.as_deref())?;
    match cli.cmd {
        Cmd::Build {
            weavepy,
            out,
            format,
        } => {
            let weavepy = resolve_weavepy(&workspace, weavepy.as_deref())?;
            let out = out.unwrap_or_else(|| workspace.join("target").join("dist"));
            let artifact = build_artifact(&workspace, &weavepy, &out, format)?;
            println!("{}", artifact.display());
            Ok(())
        }
        Cmd::Check {
            artifact,
            weavepy,
            wheels,
            keep_scratch,
        } => cmd_check(&workspace, artifact, weavepy, wheels, keep_scratch),
    }
}

// ---------------------------------------------------------------------------
// Workspace / binary discovery
// ---------------------------------------------------------------------------

/// Locate the workspace root: an explicit `--workspace`, else the workspace
/// this binary was compiled from (the grandparent of this crate's
/// `CARGO_MANIFEST_DIR`).
fn resolve_workspace(explicit: Option<&Path>) -> Result<PathBuf> {
    if let Some(p) = explicit {
        return p
            .canonicalize()
            .map(strip_verbatim)
            .with_context(|| format!("--workspace path does not exist: {}", p.display()));
    }
    let compiled_from = Path::new(env!("CARGO_MANIFEST_DIR"));
    let root = compiled_from
        .ancestors()
        .nth(2)
        .filter(|d| d.join("Cargo.toml").is_file())
        .context(
            "could not locate the workspace root from the compiled-in manifest dir; \
             pass --workspace explicitly",
        )?;
    Ok(root.to_path_buf())
}

/// Resolve the `weavepy` binary to package, defaulting to the release build.
fn resolve_weavepy(workspace: &Path, explicit: Option<&Path>) -> Result<PathBuf> {
    let path = match explicit {
        Some(p) => p.to_path_buf(),
        None => workspace
            .join("target")
            .join("release")
            .join(exe_name("weavepy")),
    };
    if !path.is_file() {
        bail!(
            "weavepy binary not found at {} — build it with `cargo build --release -p weavepy-cli{}` \
             or pass --weavepy",
            path.display(),
            if cfg!(windows) { " -p weavepy-pylib" } else { "" },
        );
    }
    Ok(path)
}

/// The runtime DLL and its MSVC import library, which must sit next
/// to the exe being packaged (cargo writes all three into
/// `target/<profile>/` — RFC 0064 WS1/WS3). The exe is a thin shim
/// over the DLL, so a Windows artifact without it would not even
/// start; the import library is what `pip install` of a C sdist
/// links (`{prefix}\libs\python313.lib`, setuptools' convention).
#[cfg(windows)]
fn resolve_windows_runtime(weavepy: &Path) -> Result<(PathBuf, PathBuf)> {
    let dir = weavepy
        .parent()
        .context("weavepy binary path has no parent directory")?;
    let dll = dir.join("python313.dll");
    // rustc names the cdylib's import library `python313.dll.lib`.
    let implib = dir.join("python313.dll.lib");
    if !dll.is_file() || !implib.is_file() {
        bail!(
            "python313.dll / python313.dll.lib not found next to {} — the Windows exe is a \
             shim over the runtime DLL (RFC 0064); build both with \
             `cargo build --release -p weavepy-cli -p weavepy-pylib`",
            weavepy.display()
        );
    }
    Ok((dll, implib))
}

fn exe_name(base: &str) -> String {
    if cfg!(windows) {
        format!("{base}.exe")
    } else {
        base.to_owned()
    }
}

/// `Path::canonicalize` on Windows returns `\\?\`-prefixed verbatim
/// paths. NT's verbatim syntax turns off `/`-as-separator, which breaks
/// CPython-shaped consumers: sysconfig's install schemes join with `/`
/// (`{base}/Lib/site-packages`), so `python -m venv` under a `\\?\`
/// prefix fails — on stock CPython too. Real installs never see `\\?\`
/// paths; strip the prefix so the interpreter under check is handed the
/// path shape users actually produce. No-op on non-Windows and for
/// paths (UNC shares, device paths) that have no plain spelling.
fn strip_verbatim(p: PathBuf) -> PathBuf {
    if !cfg!(windows) {
        return p;
    }
    let Some(s) = p.to_str() else { return p };
    let Some(rest) = s.strip_prefix(r"\\?\") else {
        return p;
    };
    // `\\?\C:\...` → `C:\...`; leave `\\?\UNC\...` and friends alone.
    if rest.len() >= 3 && rest.as_bytes()[1] == b':' && rest.as_bytes()[2] == b'\\' {
        return PathBuf::from(rest);
    }
    p
}

// ---------------------------------------------------------------------------
// build
// ---------------------------------------------------------------------------

/// Assemble the artifact. Returns the archive path (`Format::TarGz`,
/// `Format::Zip`) or the staging directory (`Format::Dir`).
fn build_artifact(workspace: &Path, weavepy: &Path, out: &Path, format: Format) -> Result<PathBuf> {
    let name = artifact_name(workspace);
    std::fs::create_dir_all(out).with_context(|| format!("failed to create {}", out.display()))?;
    let staging = out.join(&name);
    if staging.exists() {
        std::fs::remove_dir_all(&staging)
            .with_context(|| format!("failed to remove stale {}", staging.display()))?;
    }
    std::fs::create_dir_all(&staging)
        .with_context(|| format!("failed to create {}", staging.display()))?;

    // Have the packaged binary write its own stdlib tree into a fresh
    // cache dir — this guarantees the tree matches the binary's embedded
    // sources, with no skew against the repo checkout.
    let cache = out.join(format!(".materialize-{}", std::process::id()));
    if cache.exists() {
        std::fs::remove_dir_all(&cache)
            .with_context(|| format!("failed to remove stale {}", cache.display()))?;
    }
    std::fs::create_dir_all(&cache)
        .with_context(|| format!("failed to create {}", cache.display()))?;
    let cache = cache
        .canonicalize()
        .map(strip_verbatim)
        .with_context(|| format!("failed to canonicalize {}", cache.display()))?;

    let env = scrubbed_env(&cache);
    let output = run_captured(
        weavepy,
        &["-c", "import sys; print(sys.prefix)"],
        &env,
        None,
    )
    .with_context(|| format!("failed to run {}", weavepy.display()))?;
    if !output.status.success() {
        bail!(
            "{} exited with {} while materializing its stdlib tree:\n{}\n{}",
            weavepy.display(),
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
    }
    let printed = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    let prefix = PathBuf::from(&printed)
        .canonicalize()
        .map(strip_verbatim)
        .with_context(|| format!("binary printed a non-existent sys.prefix: {printed:?}"))?;
    if !prefix.starts_with(&cache) {
        bail!(
            "the binary resolved sys.prefix = {} instead of materializing into the fresh \
             cache {} — it found an installed layout (WEAVEPYHOME/landmark walk) despite the \
             scrubbed environment; refusing to package a tree that may not match the binary",
            prefix.display(),
            cache.display(),
        );
    }

    // Move everything the materializer wrote (lib/, include/, ...) into
    // the staging root.
    move_entries(&prefix, &staging)?;
    std::fs::remove_dir_all(&cache)
        .with_context(|| format!("failed to clean up {}", cache.display()))?;

    if cfg!(windows) {
        // RFC 0063 WS6 — the NT artifact is CPython-shaped at the root:
        // `python.exe` and friends sit directly in the prefix (no
        // `bin/`), all as real file copies — NTFS symlinks need
        // privileges, so nothing in the artifact may depend on them.
        // The landmark walk starts at the exe's own directory, so
        // `{prefix}/lib/weavepy3.13` is found on the first probe.
        for name in ["weavepy", "python", "python3", "python3.13"] {
            let dest = staging.join(exe_name(name));
            std::fs::copy(weavepy, &dest).with_context(|| {
                format!("failed to copy {} to {}", weavepy.display(), dest.display())
            })?;
        }
        // RFC 0064 WS3 — the binary ABI. `python313.dll` sits beside
        // the exes at the prefix root (the shim's first probe and
        // where a `.pyd`'s PE import resolves from), and the MSVC
        // import library ships as `libs\python313.lib` — the exact
        // path setuptools' `library_dirs` convention
        // (`{sys.base_exec_prefix}\libs`) and pyconfig.h's autolink
        // pragma expect.
        #[cfg(windows)]
        {
            let (dll, implib) = resolve_windows_runtime(weavepy)?;
            let dll_dest = staging.join("python313.dll");
            std::fs::copy(&dll, &dll_dest).with_context(|| {
                format!("failed to copy {} to {}", dll.display(), dll_dest.display())
            })?;
            let libs_dir = staging.join("libs");
            std::fs::create_dir_all(&libs_dir)
                .with_context(|| format!("failed to create {}", libs_dir.display()))?;
            let implib_dest = libs_dir.join("python313.lib");
            std::fs::copy(&implib, &implib_dest).with_context(|| {
                format!(
                    "failed to copy {} to {}",
                    implib.display(),
                    implib_dest.display()
                )
            })?;
        }
    } else {
        // bin/weavepy + the PEP 394-ish shim names.
        let bin_dir = staging.join("bin");
        std::fs::create_dir_all(&bin_dir)
            .with_context(|| format!("failed to create {}", bin_dir.display()))?;
        let dest = bin_dir.join(exe_name("weavepy"));
        std::fs::copy(weavepy, &dest).with_context(|| {
            format!("failed to copy {} to {}", weavepy.display(), dest.display())
        })?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o755))
                .with_context(|| format!("failed to chmod {}", dest.display()))?;
        }
        for shim in ["python3", "python", "python3.13"] {
            #[cfg(unix)]
            {
                std::os::unix::fs::symlink("weavepy", bin_dir.join(shim))
                    .with_context(|| format!("failed to symlink bin/{shim}"))?;
            }
            // Unreachable on Windows (the cfg!(windows) arm above owns
            // that layout); kept for hypothetical non-unix, non-Windows
            // hosts so the build still produces runnable shims.
            #[cfg(not(unix))]
            {
                let shim_dest = bin_dir.join(exe_name(shim));
                std::fs::copy(&dest, &shim_dest)
                    .with_context(|| format!("failed to copy bin shim {}", shim_dest.display()))?;
            }
        }
    }

    // Artifact-level docs + licenses.
    std::fs::write(staging.join("README.md"), artifact_readme(&name))
        .context("failed to write artifact README.md")?;
    for license in ["LICENSE-APACHE", "LICENSE-MIT"] {
        let src = workspace.join(license);
        std::fs::copy(&src, staging.join(license))
            .with_context(|| format!("failed to copy {}", src.display()))?;
    }

    match format {
        Format::Dir => Ok(staging),
        Format::TarGz => {
            let tarball = out.join(format!("{name}.tar.gz"));
            // The system tar preserves symlinks and modes; the Rust
            // stdlib has no tar writer and we keep this crate dep-light.
            create_archive(&tarball, &["-czf"], out, &name)?;
            Ok(tarball)
        }
        Format::Zip => {
            let archive = out.join(format!("{name}.zip"));
            // `tar -a -cf x.zip …` — bsdtar (the system tar on Windows
            // 10+, the GitHub runners, and macOS) autodetects the zip
            // output format from the extension. Same dep-light story
            // as TarGz: one external tool, already on PATH.
            create_archive(&archive, &["-a", "-cf"], out, &name)?;
            Ok(archive)
        }
    }
}

/// Create `archive` by invoking the system `tar` with `flags` (e.g.
/// `-czf` or `-a -cf`), archiving `root` relative to `dir`, replacing
/// any stale file first.
fn create_archive(archive: &Path, flags: &[&str], dir: &Path, root: &str) -> Result<()> {
    if archive.exists() {
        std::fs::remove_file(archive)
            .with_context(|| format!("failed to remove stale {}", archive.display()))?;
    }
    let status = std::process::Command::new("tar")
        .args(flags)
        .arg(archive)
        .arg("-C")
        .arg(dir)
        .arg(root)
        .status()
        .context("failed to spawn `tar` — is it on PATH?")?;
    if !status.success() {
        bail!(
            "`tar {} {}` exited with {status}",
            flags.join(" "),
            archive.display()
        );
    }
    Ok(())
}

/// `weavepy-{version}+g{git_short}-{target_triple}`.
fn artifact_name(workspace: &Path) -> String {
    let version = env!("CARGO_PKG_VERSION");
    let git = git_short_hash(workspace).unwrap_or_else(|| "unknown".to_owned());
    format!("weavepy-{version}+g{git}-{}", target_triple())
}

fn git_short_hash(workspace: &Path) -> Option<String> {
    let output = std::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .current_dir(workspace)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let hash = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    (!hash.is_empty()).then_some(hash)
}

/// The conventional target triple for the *host*, from `std::env::consts`.
/// (The artifact packages a host-built binary, so host == target.)
fn target_triple() -> String {
    let arch = std::env::consts::ARCH;
    match std::env::consts::OS {
        "macos" => format!("{arch}-apple-darwin"),
        "linux" => format!("{arch}-unknown-linux-gnu"),
        "windows" => format!("{arch}-pc-windows-msvc"),
        other => format!("{arch}-unknown-{other}"),
    }
}

fn artifact_readme(name: &str) -> String {
    // The builder always packages the host target, so the README
    // describes the layout this artifact actually has — the POSIX
    // `bin/` shims, or the RFC 0063 WS6 exe-at-root NT shape — and
    // mentions the sibling layout only in passing.
    if cfg!(windows) {
        format!(
            "# {name}\n\
             \n\
             A relocatable build of WeavePy, a Python 3.13-compatible interpreter.\n\
             \n\
             ## Usage\n\
             \n\
             Extract this directory anywhere and run the interpreter directly:\n\
             \n\
             ```bat\n\
             .\\python3.exe\n\
             ```\n\
             \n\
             The exes are thin shims over `python313.dll` at the artifact root —\n\
             the runtime itself, and what C extensions link against (the CPython\n\
             Windows convention; POSIX artifacts use `bin/` symlinks instead).\n\
             The layout is self-locating — no environment variables are required.\n\
             \n\
             ## Packaging\n\
             \n\
             Virtual environments and pip work out of the box (a pip wheel is\n\
             bundled):\n\
             \n\
             ```bat\n\
             .\\python3.exe -m venv .venv\n\
             .venv\\Scripts\\python.exe -m pip install <package>\n\
             ```\n\
             \n\
             Building C extensions from source needs MSVC (Visual Studio Build\n\
             Tools): the CPython 3.13 header set ships under `Include\\` and the\n\
             import library under `libs\\python313.lib`, the paths setuptools\n\
             uses by convention.\n\
             \n\
             ## License\n\
             \n\
             MIT OR Apache-2.0 — see `LICENSE-MIT` and `LICENSE-APACHE`.\n"
        )
    } else {
        format!(
            "# {name}\n\
             \n\
             A relocatable build of WeavePy, a Python 3.13-compatible interpreter.\n\
             \n\
             ## Usage\n\
             \n\
             Extract this directory anywhere and run the interpreter directly:\n\
             \n\
             ```sh\n\
             ./bin/python3\n\
             ```\n\
             \n\
             Optionally put it on your PATH:\n\
             \n\
             ```sh\n\
             export PATH=\"$PWD/bin:$PATH\"\n\
             ```\n\
             \n\
             `bin/weavepy` is the real binary; `python`, `python3`, and\n\
             `python3.13` are symlinks to it (Windows artifacts instead place\n\
             `python.exe` and friends at the archive root). The layout is\n\
             self-locating — no environment variables are required.\n\
             \n\
             ## Packaging\n\
             \n\
             Virtual environments and pip work out of the box (a pip wheel is\n\
             bundled):\n\
             \n\
             ```sh\n\
             ./bin/python3 -m venv .venv\n\
             .venv/bin/python -m pip install <package>\n\
             ```\n\
             \n\
             The CPython 3.13 C header set ships under `include/python3.13`\n\
             (what `sysconfig.get_paths()[\"include\"]` reports), so building\n\
             C extensions from source — `pip install --no-binary` of sdists —\n\
             works with a C compiler on PATH.\n\
             \n\
             ## License\n\
             \n\
             MIT OR Apache-2.0 — see `LICENSE-MIT` and `LICENSE-APACHE`.\n"
        )
    }
}

/// Move each entry of `from` into `to`, preferring `fs::rename` and falling
/// back to a symlink-preserving recursive copy when the rename fails
/// (e.g. across filesystems).
fn move_entries(from: &Path, to: &Path) -> Result<()> {
    for entry in
        std::fs::read_dir(from).with_context(|| format!("failed to read {}", from.display()))?
    {
        let entry = entry.with_context(|| format!("failed to read {}", from.display()))?;
        let src = entry.path();
        let dst = to.join(entry.file_name());
        if std::fs::rename(&src, &dst).is_err() {
            copy_recursive(&src, &dst).with_context(|| {
                format!("failed to copy {} to {}", src.display(), dst.display())
            })?;
        }
    }
    Ok(())
}

/// Recursive copy preserving symlinks (unix) and file modes.
fn copy_recursive(src: &Path, dst: &Path) -> Result<()> {
    let meta = std::fs::symlink_metadata(src)
        .with_context(|| format!("failed to stat {}", src.display()))?;
    let ftype = meta.file_type();
    if ftype.is_symlink() {
        #[cfg(unix)]
        {
            let target = std::fs::read_link(src)
                .with_context(|| format!("failed to readlink {}", src.display()))?;
            std::os::unix::fs::symlink(&target, dst)
                .with_context(|| format!("failed to symlink {}", dst.display()))?;
        }
        #[cfg(not(unix))]
        {
            std::fs::copy(src, dst).with_context(|| format!("failed to copy {}", src.display()))?;
        }
    } else if ftype.is_dir() {
        std::fs::create_dir_all(dst)
            .with_context(|| format!("failed to create {}", dst.display()))?;
        for entry in
            std::fs::read_dir(src).with_context(|| format!("failed to read {}", src.display()))?
        {
            let entry = entry.with_context(|| format!("failed to read {}", src.display()))?;
            copy_recursive(&entry.path(), &dst.join(entry.file_name()))?;
        }
    } else {
        std::fs::copy(src, dst).with_context(|| format!("failed to copy {}", src.display()))?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// check
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LegStatus {
    Pass,
    Fail,
    Skip,
}

impl fmt::Display for LegStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LegStatus::Pass => write!(f, "PASS"),
            LegStatus::Fail => write!(f, "FAIL"),
            LegStatus::Skip => write!(f, "SKIP"),
        }
    }
}

#[derive(Debug)]
struct Leg {
    name: &'static str,
    status: LegStatus,
    /// One-line summary for the table; full failure output for FAIL legs.
    detail: String,
}

fn cmd_check(
    workspace: &Path,
    artifact: Option<PathBuf>,
    weavepy: Option<PathBuf>,
    wheels: Option<PathBuf>,
    keep_scratch: bool,
) -> Result<()> {
    let scratch = std::env::temp_dir().join(format!("weavepy-dist-check-{}", std::process::id()));
    if scratch.exists() {
        std::fs::remove_dir_all(&scratch)
            .with_context(|| format!("failed to remove stale {}", scratch.display()))?;
    }
    std::fs::create_dir_all(&scratch)
        .with_context(|| format!("failed to create {}", scratch.display()))?;
    let scratch = scratch
        .canonicalize()
        .map(strip_verbatim)
        .with_context(|| format!("failed to canonicalize {}", scratch.display()))?;

    let result = run_check(workspace, artifact, weavepy, wheels, &scratch);

    if keep_scratch {
        eprintln!("scratch kept at {}", scratch.display());
    } else {
        // Venv trees can carry read-only entries; ignore cleanup failures.
        let _ = std::fs::remove_dir_all(&scratch);
    }
    result
}

fn run_check(
    workspace: &Path,
    artifact: Option<PathBuf>,
    weavepy: Option<PathBuf>,
    wheels: Option<PathBuf>,
    scratch: &Path,
) -> Result<()> {
    // Resolve (or build) the artifact prefix to check.
    let prefix = match artifact {
        Some(path) => {
            if path.is_dir() {
                path.canonicalize()
                    .map(strip_verbatim)
                    .with_context(|| format!("failed to canonicalize {}", path.display()))?
            } else if path.is_file() {
                extract_archive(&path, scratch)?
            } else {
                bail!("--artifact path does not exist: {}", path.display());
            }
        }
        None => {
            let weavepy = resolve_weavepy(workspace, weavepy.as_deref())?;
            eprintln!("no --artifact given; building a fresh dir artifact into the scratch area");
            build_artifact(workspace, &weavepy, &scratch.join("build"), Format::Dir)?
        }
    };
    let prefix = prefix
        .canonicalize()
        .map(strip_verbatim)
        .with_context(|| format!("failed to canonicalize {}", prefix.display()))?;
    eprintln!("checking artifact prefix {}", prefix.display());

    // The interpreter's place mirrors the layout `build_artifact` wrote
    // for this host: `{prefix}/python3.exe` at the prefix root on
    // Windows (RFC 0063 WS6 — no `bin/`), `{prefix}/bin/python3`
    // elsewhere.
    let python3 = if cfg!(windows) {
        prefix.join(exe_name("python3"))
    } else {
        prefix.join("bin").join(exe_name("python3"))
    };
    if !python3.exists() {
        bail!(
            "artifact has no {} — not a WeavePy prefix?",
            python3.display()
        );
    }

    // Scrubbed environment: the artifact must self-locate; a decoy stdlib
    // cache makes any materialize fallback visible as a failure.
    let decoy = scratch.join("decoy-cache");
    std::fs::create_dir_all(&decoy)
        .with_context(|| format!("failed to create {}", decoy.display()))?;
    let env = scrubbed_env(&decoy);

    let mut legs: Vec<Leg> = Vec::new();

    // Leg 1: version.
    legs.push(leg_version(&python3, &env));

    // Leg 2: identity.
    legs.push(leg_python_script(
        "identity",
        &python3,
        IDENTITY_SCRIPT,
        &env,
        &[("WEAVEPY_DIST_EXPECT_PREFIX", prefix.as_os_str().to_owned())],
    ));

    // Leg 3: stdlib spot-checks.
    legs.push(leg_python_script(
        "stdlib",
        &python3,
        STDLIB_SCRIPT,
        &env,
        &[],
    ));

    // Leg 4: venv. The venv's interpreter follows the platform scheme
    // (`sysconfig`'s `venv` vs `nt_venv`): `bin/python` on POSIX,
    // `Scripts\python.exe` on Windows.
    let venv_dir = scratch.join("venv");
    let venv_python = if cfg!(windows) {
        venv_dir.join("Scripts").join(exe_name("python"))
    } else {
        venv_dir.join("bin").join(exe_name("python"))
    };
    let venv_leg = leg_venv(&python3, &venv_dir, &venv_python, &prefix, &env);
    let venv_ok = venv_leg.status == LegStatus::Pass;
    legs.push(venv_leg);

    // Leg 5: pip (needs a wheel cache and a working venv).
    legs.push(match (&wheels, venv_ok) {
        (None, _) => Leg {
            name: "pip",
            status: LegStatus::Skip,
            detail: "no --wheels dir given".to_owned(),
        },
        (Some(_), false) => Leg {
            name: "pip",
            status: LegStatus::Skip,
            detail: "venv leg failed".to_owned(),
        },
        (Some(wheels), true) => leg_pip(&venv_python, wheels, &env),
    });

    // Leg 6: C-extension build (needs a C toolchain: `cc` on unix,
    // MSVC on Windows — the Windows script discovers MSVC itself via
    // `cl` on PATH or vswhere/vcvars64 and exits 2 when there is
    // none, RFC 0064 WS3).
    legs.push(if cfg!(unix) && which("cc").is_none() {
        Leg {
            name: "cext",
            status: LegStatus::Skip,
            detail: "no `cc` on PATH".to_owned(),
        }
    } else {
        leg_cext(&python3, scratch, &env)
    });

    // Leg 7: the decoy cache must still be empty — anything in it means
    // the binary fell back to materializing a stdlib tree, i.e. the
    // artifact layout failed to self-locate.
    legs.push(leg_decoy(&decoy));

    print_report(&legs);

    let failed = legs.iter().filter(|l| l.status == LegStatus::Fail).count();
    if failed > 0 {
        bail!("{failed} check leg(s) failed");
    }
    println!("all check legs passed");
    Ok(())
}

/// Extract an archive into `<scratch>/extract` and return the single
/// top-level directory inside it. Plain `-xf` handles both artifact
/// formats: tar sniffs gzip from the file contents, and bsdtar (the
/// system tar everywhere zip artifacts exist — see `Format::Zip`)
/// sniffs zip the same way.
fn extract_archive(archive: &Path, scratch: &Path) -> Result<PathBuf> {
    let extract = scratch.join("extract");
    std::fs::create_dir_all(&extract)
        .with_context(|| format!("failed to create {}", extract.display()))?;
    let status = std::process::Command::new("tar")
        .arg("-xf")
        .arg(archive)
        .arg("-C")
        .arg(&extract)
        .status()
        .context("failed to spawn `tar` — is it on PATH?")?;
    if !status.success() {
        bail!("`tar -xf {}` exited with {status}", archive.display());
    }
    let mut dirs: Vec<PathBuf> = Vec::new();
    for entry in std::fs::read_dir(&extract)
        .with_context(|| format!("failed to read {}", extract.display()))?
    {
        let entry = entry.with_context(|| format!("failed to read {}", extract.display()))?;
        if entry.path().is_dir() {
            dirs.push(entry.path());
        }
    }
    match dirs.as_slice() {
        [single] => Ok(single.clone()),
        other => bail!(
            "expected exactly one top-level directory in {}, found {}",
            archive.display(),
            other.len()
        ),
    }
}

fn leg_version(python3: &Path, env: &[(OsString, OsString)]) -> Leg {
    match run_captured(python3, &["-V"], env, None) {
        Err(err) => Leg {
            name: "version",
            status: LegStatus::Fail,
            detail: format!("failed to spawn {}: {err:#}", python3.display()),
        },
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout).trim().to_owned();
            let stderr = String::from_utf8_lossy(&out.stderr).trim().to_owned();
            if out.status.success() && (stdout.contains("3.13") || stderr.contains("3.13")) {
                Leg {
                    name: "version",
                    status: LegStatus::Pass,
                    detail: stdout,
                }
            } else {
                Leg {
                    name: "version",
                    status: LegStatus::Fail,
                    detail: format!(
                        "`python3 -V` exited {} without reporting 3.13\nstdout: {stdout}\nstderr: {stderr}",
                        out.status
                    ),
                }
            }
        }
    }
}

/// Run a `-c` script leg; PASS iff exit 0.
fn leg_python_script(
    name: &'static str,
    python3: &Path,
    script: &str,
    env: &[(OsString, OsString)],
    extra_env: &[(&str, OsString)],
) -> Leg {
    let mut env = env.to_vec();
    for (k, v) in extra_env {
        env.push((OsString::from(k), v.clone()));
    }
    grade_output(name, run_captured(python3, &["-c", script], &env, None))
}

fn leg_venv(
    python3: &Path,
    venv_dir: &Path,
    venv_python: &Path,
    prefix: &Path,
    env: &[(OsString, OsString)],
) -> Leg {
    let venv_arg = venv_dir.display().to_string();
    // Import-trace the creation run (`PYTHONVERBOSE=1`): a native crash —
    // Windows fast-fail is *silent* (no traceback, empty stderr) — leaves
    // the trace's last `import ...` line as the only pointer to where
    // startup died. Verbose output is discarded on success.
    let mut create_env = env.to_vec();
    create_env.push((OsString::from("PYTHONVERBOSE"), OsString::from("1")));
    let created = run_captured(python3, &["-m", "venv", &venv_arg], &create_env, None);
    match created {
        Err(err) => {
            return Leg {
                name: "venv",
                status: LegStatus::Fail,
                detail: format!("failed to spawn venv creation: {err:#}"),
            }
        }
        Ok(out) if !out.status.success() => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            // The import trace can run to thousands of lines; only the
            // tail (the crash neighbourhood) is diagnostic.
            let tail_start = stderr
                .lines()
                .rev()
                .take(60)
                .last()
                .map(|l| l.as_ptr() as usize - stderr.as_ptr() as usize)
                .unwrap_or(0);
            return Leg {
                name: "venv",
                status: LegStatus::Fail,
                detail: format!(
                    "`python3 -m venv` exited {}\n{}\n{}{}",
                    out.status,
                    String::from_utf8_lossy(&out.stdout),
                    &stderr[tail_start..],
                    venv_failure_diagnostics(venv_python, env),
                ),
            };
        }
        Ok(_) => {}
    }
    let mut env = env.to_vec();
    env.push((
        OsString::from("WEAVEPY_DIST_EXPECT_PREFIX"),
        prefix.as_os_str().to_owned(),
    ));
    env.push((
        OsString::from("WEAVEPY_DIST_EXPECT_VENV"),
        venv_dir.as_os_str().to_owned(),
    ));
    grade_output(
        "venv",
        run_captured(venv_python, &["-c", VENV_SCRIPT], &env, None),
    )
}

/// `python -m venv` bootstraps pip through `subprocess.check_output`,
/// so the ensurepip child's traceback is captured and discarded — the
/// `CalledProcessError` venv prints carries only the exit status. When
/// creation fails and the venv interpreter was already copied in, re-run
/// the two commands the half-built venv can still answer and append
/// their output, so a CI-only failure is diagnosable from the leg
/// detail alone.
fn venv_failure_diagnostics(venv_python: &Path, env: &[(OsString, OsString)]) -> String {
    if !venv_python.is_file() {
        return format!(
            "\n(no diagnostics: {} does not exist — venv creation failed before the interpreter copy)",
            venv_python.display()
        );
    }
    const IDENTITY: &str = "import sys, sysconfig\n\
         print('executable:', sys.executable)\n\
         print('prefix:', sys.prefix)\n\
         print('base_prefix:', sys.base_prefix)\n\
         print('path:', sys.path)\n\
         print('purelib:', sysconfig.get_paths()['purelib'])";
    let probes: [(&str, &[&str]); 2] = [
        ("identity", &["-c", IDENTITY]),
        (
            "ensurepip",
            &["-m", "ensurepip", "--upgrade", "--default-pip"],
        ),
    ];
    let mut detail = String::new();
    for (label, args) in probes {
        detail.push_str(&format!("\n--- diagnostic: venv python {label} ---\n"));
        match run_captured(venv_python, args, env, None) {
            Err(err) => detail.push_str(&format!("failed to spawn: {err:#}\n")),
            Ok(out) => detail.push_str(&format!(
                "exit: {}\nstdout:\n{}\nstderr:\n{}\n",
                out.status,
                String::from_utf8_lossy(&out.stdout).trim_end(),
                String::from_utf8_lossy(&out.stderr).trim_end(),
            )),
        }
    }
    detail
}

fn leg_pip(venv_python: &Path, wheels: &Path, env: &[(OsString, OsString)]) -> Leg {
    let wheels_arg = wheels.display().to_string();
    let install = run_captured(
        venv_python,
        &[
            "-m",
            "pip",
            "install",
            "--no-index",
            "--find-links",
            &wheels_arg,
            "six",
        ],
        env,
        None,
    );
    match install {
        Err(err) => {
            return Leg {
                name: "pip",
                status: LegStatus::Fail,
                detail: format!("failed to spawn pip: {err:#}"),
            }
        }
        Ok(out) if !out.status.success() => {
            return Leg {
                name: "pip",
                status: LegStatus::Fail,
                detail: format!(
                    "`pip install six` exited {}\n{}\n{}",
                    out.status,
                    String::from_utf8_lossy(&out.stdout),
                    String::from_utf8_lossy(&out.stderr),
                ),
            }
        }
        Ok(_) => {}
    }
    grade_output(
        "pip",
        run_captured(
            venv_python,
            &["-c", "import six; print(six.__version__)"],
            env,
            None,
        ),
    )
}

fn leg_cext(python3: &Path, scratch: &Path, env: &[(OsString, OsString)]) -> Leg {
    let cext_dir = scratch.join("cext");
    if let Err(err) = std::fs::create_dir_all(&cext_dir) {
        return Leg {
            name: "cext",
            status: LegStatus::Fail,
            detail: format!("failed to create {}: {err}", cext_dir.display()),
        };
    }
    let script = if cfg!(windows) {
        CEXT_SCRIPT_NT
    } else {
        CEXT_SCRIPT
    };
    let script_path = scratch.join("cext_build_check.py");
    if let Err(err) = std::fs::write(&script_path, script) {
        return Leg {
            name: "cext",
            status: LegStatus::Fail,
            detail: format!("failed to write {}: {err}", script_path.display()),
        };
    }
    let mut env = env.to_vec();
    env.push((
        OsString::from("WEAVEPY_DIST_CEXT_DIR"),
        cext_dir.as_os_str().to_owned(),
    ));
    let script_arg = script_path.display().to_string();
    let result = run_captured(python3, &[&script_arg], &env, None);
    // Exit 2 is the Windows script's "no MSVC toolchain" sentinel —
    // a machine without Visual Studio skips the leg rather than
    // failing the whole check.
    if let Ok(out) = &result {
        if out.status.code() == Some(2) {
            let stdout = String::from_utf8_lossy(&out.stdout).trim().to_owned();
            return Leg {
                name: "cext",
                status: LegStatus::Skip,
                detail: stdout
                    .lines()
                    .last()
                    .unwrap_or("no MSVC toolchain")
                    .to_owned(),
            };
        }
    }
    grade_output("cext", result)
}

fn leg_decoy(decoy: &Path) -> Leg {
    match std::fs::read_dir(decoy) {
        Err(err) => Leg {
            name: "decoy-cache",
            status: LegStatus::Fail,
            detail: format!("failed to read {}: {err}", decoy.display()),
        },
        Ok(entries) => {
            let names: Vec<String> = entries
                .filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().into_owned()))
                .collect();
            if names.is_empty() {
                Leg {
                    name: "decoy-cache",
                    status: LegStatus::Pass,
                    detail: "empty — no materialize fallback".to_owned(),
                }
            } else {
                Leg {
                    name: "decoy-cache",
                    status: LegStatus::Fail,
                    detail: format!(
                        "the binary materialized a stdlib cache ({}) — the artifact layout \
                         failed to self-locate and the checks above silently ran off the \
                         fallback tree",
                        names.join(", ")
                    ),
                }
            }
        }
    }
}

/// Fold a captured subprocess result into PASS (exit 0) or FAIL.
fn grade_output(name: &'static str, result: Result<std::process::Output>) -> Leg {
    match result {
        Err(err) => Leg {
            name,
            status: LegStatus::Fail,
            detail: format!("failed to spawn: {err:#}"),
        },
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout).trim().to_owned();
            let stderr = String::from_utf8_lossy(&out.stderr).trim().to_owned();
            if out.status.success() {
                Leg {
                    name,
                    status: LegStatus::Pass,
                    detail: stdout.lines().last().unwrap_or("").to_owned(),
                }
            } else {
                Leg {
                    name,
                    status: LegStatus::Fail,
                    detail: format!(
                        "exited {}\nstdout:\n{stdout}\nstderr:\n{stderr}",
                        out.status
                    ),
                }
            }
        }
    }
}

fn print_report(legs: &[Leg]) {
    println!();
    println!("| leg         | status | detail");
    println!("|-------------|--------|-------");
    for leg in legs {
        let summary = leg.detail.lines().next().unwrap_or("");
        println!("| {:<11} | {:<6} | {summary}", leg.name, leg.status);
    }
    println!();
    for leg in legs {
        if leg.status == LegStatus::Fail {
            println!("--- {} ---", leg.name);
            println!("{}", leg.detail);
            println!();
        }
    }
}

// ---------------------------------------------------------------------------
// Subprocess plumbing
// ---------------------------------------------------------------------------

/// The scrubbed environment every packaged-binary subprocess runs under:
/// the current env minus everything that could leak the repo checkout or
/// a host Python (`WEAVEPYHOME`, `PYTHONHOME`, `PYTHONPATH`,
/// `PYTHONSTARTUP`, `VIRTUAL_ENV`, `WEAVEPY_*`), plus
/// `WEAVEPY_STDLIB_CACHE` pointed at `cache`.
fn scrubbed_env(cache: &Path) -> Vec<(OsString, OsString)> {
    let mut env: Vec<(OsString, OsString)> = std::env::vars_os()
        .filter(|(key, _)| {
            let key = key.to_string_lossy();
            !matches!(
                key.as_ref(),
                "WEAVEPYHOME" | "PYTHONHOME" | "PYTHONPATH" | "PYTHONSTARTUP" | "VIRTUAL_ENV"
            ) && !key.starts_with("WEAVEPY_")
        })
        .collect();
    env.push((
        OsString::from("WEAVEPY_STDLIB_CACHE"),
        cache.as_os_str().to_owned(),
    ));
    env
}

fn run_captured(
    program: &Path,
    args: &[&str],
    env: &[(OsString, OsString)],
    cwd: Option<&Path>,
) -> Result<std::process::Output> {
    let mut cmd = std::process::Command::new(program);
    cmd.args(args).env_clear().envs(env.iter().cloned());
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    cmd.output()
        .with_context(|| format!("failed to run {}", program.display()))
}

/// Minimal `which`: search `PATH` for an executable file named `prog`.
fn which(prog: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(prog);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Python-side check scripts
// ---------------------------------------------------------------------------

/// Identity leg: every `sys`/`sysconfig` self-location surface must point
/// inside the artifact prefix (passed via `WEAVEPY_DIST_EXPECT_PREFIX`;
/// both sides compared through `os.path.realpath`).
const IDENTITY_SCRIPT: &str = r#"
import os, sys, sysconfig
expect = os.path.realpath(os.environ["WEAVEPY_DIST_EXPECT_PREFIX"])
prefix = os.path.realpath(sys.prefix)
assert prefix == expect, f"sys.prefix={sys.prefix!r} != {expect!r}"
base = os.path.realpath(sys.base_prefix)
assert base == expect, f"sys.base_prefix={sys.base_prefix!r} != {expect!r}"
exe_dir = os.path.realpath(os.path.dirname(sys.executable))
# NT artifacts put the exe at the prefix root (RFC 0063 WS6); POSIX
# artifacts keep it under bin/.
want_exe_dir = expect if os.name == "nt" else os.path.join(expect, "bin")
assert exe_dir == want_exe_dir, (
    f"sys.executable={sys.executable!r} not in {want_exe_dir!r}"
)
stdlib = os.path.realpath(sys._stdlib_dir)
want_stdlib = os.path.join(expect, "lib", "weavepy3.13")
assert stdlib == want_stdlib, f"sys._stdlib_dir={sys._stdlib_dir!r} != {want_stdlib!r}"
inc = sysconfig.get_paths()["include"]
assert os.path.isdir(inc), f"sysconfig include dir missing: {inc!r}"
for name in ("Python.h", "pyconfig.h"):
    assert os.path.isfile(os.path.join(inc, name)), f"missing {name} in {inc!r}"
includepy = sysconfig.get_config_var("INCLUDEPY")
assert os.path.realpath(includepy) == os.path.realpath(inc), (
    f"INCLUDEPY={includepy!r} != include path {inc!r}"
)
print("identity ok:", prefix)
"#;

/// Stdlib spot-checks crossing native/frozen boundaries.
const STDLIB_SCRIPT: &str = r#"
import sqlite3, ssl, zlib, decimal, json, hashlib
con = sqlite3.connect(":memory:")
con.execute("CREATE TABLE t (x INTEGER, y TEXT)")
con.execute("INSERT INTO t VALUES (?, ?)", (42, "weave"))
assert con.execute("SELECT x, y FROM t").fetchone() == (42, "weave")
con.close()
ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_CLIENT)
try:
    ctx.load_default_certs()
except Exception:
    pass  # may fail offline; creating the context is the check
data = b"weavepy" * 1000
assert zlib.decompress(zlib.compress(data)) == data
assert decimal.Decimal("1.1") + decimal.Decimal("2.2") == decimal.Decimal("3.3")
assert json.loads(json.dumps({"a": [1, 2, 3]})) == {"a": [1, 2, 3]}
digest = hashlib.sha256(b"weavepy").hexdigest()
assert len(digest) == 64
print("stdlib ok")
"#;

/// Venv leg: the venv python must report the venv as `sys.prefix` and
/// chain `sys.base_prefix` back to the artifact.
const VENV_SCRIPT: &str = r#"
import os, sys, sysconfig
venv = os.path.realpath(os.environ["WEAVEPY_DIST_EXPECT_VENV"])
base = os.path.realpath(os.environ["WEAVEPY_DIST_EXPECT_PREFIX"])
prefix = os.path.realpath(sys.prefix)
assert prefix == venv, f"sys.prefix={sys.prefix!r} != venv {venv!r}"
got_base = os.path.realpath(sys.base_prefix)
assert got_base == base, f"sys.base_prefix={sys.base_prefix!r} != artifact {base!r}"
paths = sysconfig.get_paths()
assert paths["purelib"], "sysconfig.get_paths() gave no purelib"
print("venv ok:", prefix)
"#;

/// C-build leg: compile a minimal extension with the `sysconfig` compiler
/// vars against the shipped headers, import it, and call it. Mirrors
/// `tests/regrtest/test_cext_build.py` (setuptools-free: compile, link,
/// import — exactly what ccompiler does under the hood).
const CEXT_SCRIPT: &str = r#"
import os, shlex, subprocess, sys, sysconfig

scratch = os.environ["WEAVEPY_DIST_CEXT_DIR"]
cc = sysconfig.get_config_var("CC") or "cc"
cflags = sysconfig.get_config_var("CFLAGS") or ""
ccshared = sysconfig.get_config_var("CCSHARED") or ""
ldshared = sysconfig.get_config_var("LDSHARED")
includepy = sysconfig.get_config_var("INCLUDEPY")
ext_suffix = sysconfig.get_config_var("EXT_SUFFIX") or ".so"
assert ldshared, "LDSHARED unset"
assert includepy and os.path.isfile(os.path.join(includepy, "Python.h")), (
    f"no Python.h under INCLUDEPY={includepy!r}"
)

SOURCE = r'''
#define PY_SSIZE_T_CLEAN
#include <Python.h>

static PyObject *
add(PyObject *self, PyObject *args)
{
    Py_ssize_t a, b;
    if (!PyArg_ParseTuple(args, "nn", &a, &b))
        return NULL;
    return PyLong_FromSsize_t(a + b);
}

static PyMethodDef methods[] = {
    {"add", add, METH_VARARGS, "add two ints"},
    {NULL, NULL, 0, NULL}
};

static struct PyModuleDef module = {
    PyModuleDef_HEAD_INIT, "_weavepy_dist_cext", NULL, -1, methods
};

PyMODINIT_FUNC
PyInit__weavepy_dist_cext(void)
{
    return PyModule_Create(&module);
}
'''

src = os.path.join(scratch, "_weavepy_dist_cext.c")
with open(src, "w") as f:
    f.write(SOURCE)


def run(cmd):
    proc = subprocess.run(cmd, capture_output=True, text=True)
    assert proc.returncode == 0, "%r failed:\n%s\n%s" % (cmd, proc.stdout, proc.stderr)


obj = os.path.join(scratch, "_weavepy_dist_cext.o")
run(
    shlex.split(cc)
    + shlex.split(cflags)
    + shlex.split(ccshared)
    + ["-I", includepy, "-c", src, "-o", obj]
)
mod_path = os.path.join(scratch, "_weavepy_dist_cext" + ext_suffix)
run(shlex.split(ldshared) + [obj, "-o", mod_path])

sys.path.insert(0, scratch)
import _weavepy_dist_cext

assert _weavepy_dist_cext.add(20, 22) == 42
print("cext ok:", mod_path)
"#;

/// The Windows twin of [`CEXT_SCRIPT`] (RFC 0064 WS3): same inline
/// module, but built with MSVC `cl /LD` against the shipped
/// `Include\` headers and linked against `libs\python313.lib` (found
/// via the pyconfig.h autolink pragma + `/LIBPATH`, exactly what
/// setuptools does). MSVC is discovered the way setuptools' msvc
/// module does it — `cl` already on PATH, else vswhere →
/// `vcvars64.bat` env capture. Exit 2 = no toolchain (the leg SKIPs).
const CEXT_SCRIPT_NT: &str = r#"
import os, subprocess, sys, sysconfig

scratch = os.environ["WEAVEPY_DIST_CEXT_DIR"]
includepy = sysconfig.get_config_var("INCLUDEPY")
ext_suffix = sysconfig.get_config_var("EXT_SUFFIX") or ".pyd"
libs = os.path.join(sys.base_exec_prefix, "libs")
assert includepy and os.path.isfile(os.path.join(includepy, "Python.h")), (
    f"no Python.h under INCLUDEPY={includepy!r}"
)
assert os.path.isfile(os.path.join(libs, "python313.lib")), (
    f"no python313.lib under {libs!r}"
)


def msvc_env():
    """Env with cl.exe reachable, or None if no MSVC install exists."""
    from shutil import which
    if which("cl"):
        return dict(os.environ)
    vswhere = os.path.join(
        os.environ.get("ProgramFiles(x86)", r"C:\Program Files (x86)"),
        "Microsoft Visual Studio", "Installer", "vswhere.exe",
    )
    if not os.path.isfile(vswhere):
        return None
    proc = subprocess.run(
        [vswhere, "-latest", "-products", "*",
         "-requires", "Microsoft.VisualStudio.Component.VC.Tools.x86.x64",
         "-property", "installationPath"],
        capture_output=True, text=True,
    )
    lines = [l.strip() for l in proc.stdout.splitlines() if l.strip()]
    if proc.returncode != 0 or not lines:
        return None
    vcvars = os.path.join(lines[0], "VC", "Auxiliary", "Build", "vcvars64.bat")
    if not os.path.isfile(vcvars):
        return None
    # Capture the env vcvars64 sets up (setuptools' _get_vc_env trick).
    probe = subprocess.run(
        ["cmd", "/S", "/C", f'"{vcvars}" >NUL 2>&1 && set'],
        capture_output=True, text=True,
    )
    if probe.returncode != 0:
        return None
    env = {}
    for line in probe.stdout.splitlines():
        key, sep, value = line.partition("=")
        if sep:
            env[key] = value
    return env if env else None


env = msvc_env()
if env is None:
    print("no MSVC toolchain (cl not on PATH, vswhere found no VC tools)")
    sys.exit(2)

SOURCE = r'''
#define PY_SSIZE_T_CLEAN
#include <Python.h>

static PyObject *
add(PyObject *self, PyObject *args)
{
    Py_ssize_t a, b;
    if (!PyArg_ParseTuple(args, "nn", &a, &b))
        return NULL;
    return PyLong_FromSsize_t(a + b);
}

static PyMethodDef methods[] = {
    {"add", add, METH_VARARGS, "add two ints"},
    {NULL, NULL, 0, NULL}
};

static struct PyModuleDef module = {
    PyModuleDef_HEAD_INIT, "_weavepy_dist_cext", NULL, -1, methods
};

PyMODINIT_FUNC
PyInit__weavepy_dist_cext(void)
{
    return PyModule_Create(&module);
}
'''

src = os.path.join(scratch, "_weavepy_dist_cext.c")
with open(src, "w") as f:
    f.write(SOURCE)

mod_path = os.path.join(scratch, "_weavepy_dist_cext" + ext_suffix)
cmd = [
    "cl", "/nologo", "/LD", "/O2", "/W3",
    "/I", includepy, src,
    "/link", "/LIBPATH:" + libs, "/OUT:" + mod_path,
]
proc = subprocess.run(cmd, capture_output=True, text=True, cwd=scratch, env=env)
assert proc.returncode == 0, "%r failed:\n%s\n%s" % (cmd, proc.stdout, proc.stderr)

sys.path.insert(0, scratch)
import _weavepy_dist_cext

assert _weavepy_dist_cext.add(20, 22) == 42
print("cext ok:", mod_path)
"#;

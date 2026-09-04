//! The `weavepy` binary.
//!
//! On POSIX this is the fully-static interpreter it has always been:
//! `main` calls straight into the driver library ([`weavepy_cli::cli_main`])
//! and the C-API symbols stay dlopen-visible in the executable itself
//! (`--export-dynamic` on ELF via `build.rs`, Mach-O default exports
//! on macOS).
//!
//! On Windows (RFC 0064 WS1) the binary is a *thin shim* over
//! `python313.dll`, mirroring CPython's own NT split (`python.exe` →
//! `Py_Main` in the core DLL): extension modules' PE import tables
//! name `python313.dll`, so the interpreter must live in a DLL of
//! that name for `.pyd` imports to resolve in-process. The shim
//! locates the DLL (its own directory first; then the `pyvenv.cfg`
//! `home=` chain, because venvs copy the exe but not the DLL; then
//! the default loader search), loads it, and calls the exported
//! `weavepy_main`. It deliberately references nothing else, so the
//! exe stays shim-sized and every byte of runtime state lives in the
//! DLL image.

#[cfg(not(windows))]
fn main() {
    std::process::exit(weavepy_cli::cli_main());
}

#[cfg(windows)]
fn main() {
    std::process::exit(shim::run());
}

#[cfg(windows)]
mod shim {
    use std::ffi::c_void;
    use std::os::windows::ffi::OsStrExt;
    use std::path::{Path, PathBuf};

    use windows_sys::Win32::Foundation::HMODULE;
    use windows_sys::Win32::System::LibraryLoader::{
        GetProcAddress, LoadLibraryExW, LoadLibraryW, LOAD_WITH_ALTERED_SEARCH_PATH,
    };

    /// The runtime DLL the shim binds — the CPython-compatible ABI
    /// name that `.pyd` import tables reference.
    const DLL_NAME: &str = weavepy_version::vconcat!(weavepy_version::PYLIB_STEM, ".dll");

    /// Exit code when the runtime DLL cannot be found or bound —
    /// well clear of Python's 1/2/120 conventions so scripts can
    /// tell "the program failed" from "the installation is broken".
    const EXIT_NO_RUNTIME: i32 = 103;

    pub(crate) fn run() -> i32 {
        let mut probed: Vec<PathBuf> = Vec::new();
        let dll = match locate_and_load(&mut probed) {
            Some(dll) => dll,
            None => {
                eprintln!(
                    "weavepy: {DLL_NAME} not found (probed: {}) — the exe and DLL ship \
                     together; reinstall or point PATH at a complete WeavePy distribution",
                    probed
                        .iter()
                        .map(|p| p.display().to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                );
                return EXIT_NO_RUNTIME;
            }
        };
        // SAFETY: `weavepy_main` is exported by `weavepy-pylib` with
        // exactly this signature; a DLL that lacks it is not ours.
        let entry = unsafe { GetProcAddress(dll, c"weavepy_main".as_ptr().cast()) };
        let Some(entry) = entry else {
            eprintln!(
                "weavepy: {DLL_NAME} does not export weavepy_main — version-skewed or \
                 foreign python313.dll on the search path?"
            );
            return EXIT_NO_RUNTIME;
        };
        let entry: unsafe extern "C" fn() -> i32 = unsafe { std::mem::transmute(entry) };
        unsafe { entry() }
    }

    /// Probe order (RFC 0064 WS1): the exe's own directory (the
    /// distribution layout — DLL beside the exes at the prefix
    /// root, and cargo's `target/<profile>/` during development);
    /// the `pyvenv.cfg` `home=` directory (venvs copy the exe, not
    /// the DLL); finally the loader's default search.
    fn locate_and_load(probed: &mut Vec<PathBuf>) -> Option<HMODULE> {
        let exe_dir = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(Path::to_path_buf));
        if let Some(dir) = &exe_dir {
            let candidate = dir.join(DLL_NAME);
            if let Some(dll) = load_at(&candidate) {
                return Some(dll);
            }
            probed.push(candidate);
            if let Some(home) = pyvenv_home(dir) {
                let candidate = home.join(DLL_NAME);
                if let Some(dll) = load_at(&candidate) {
                    return Some(dll);
                }
                probed.push(candidate);
            }
        }
        probed.push(PathBuf::from(DLL_NAME));
        let wide = to_wide(std::ffi::OsStr::new(DLL_NAME));
        // SAFETY: `wide` is a NUL-terminated UTF-16 string.
        let dll = unsafe { LoadLibraryW(wide.as_ptr()) };
        (!dll.is_null()).then_some(dll)
    }

    /// `LoadLibraryExW` with an absolute path; `None` when the file
    /// is absent or refuses to load.
    fn load_at(path: &Path) -> Option<HMODULE> {
        if !path.is_file() {
            return None;
        }
        let wide = to_wide(path.as_os_str());
        // SAFETY: `wide` is a NUL-terminated UTF-16 path;
        // LOAD_WITH_ALTERED_SEARCH_PATH resolves the DLL's own
        // (static) imports relative to its location, matching how
        // CPython's shim binds its core DLL.
        let dll = unsafe {
            LoadLibraryExW(
                wide.as_ptr(),
                std::ptr::null_mut::<c_void>(),
                LOAD_WITH_ALTERED_SEARCH_PATH,
            )
        };
        (!dll.is_null()).then_some(dll)
    }

    /// The `home` key of `{venv}/pyvenv.cfg` when the exe sits in a
    /// venv's `Scripts\` directory — the base prefix, where the real
    /// DLL lives. Whitespace-tolerant like CPython's getpath.
    fn pyvenv_home(exe_dir: &Path) -> Option<PathBuf> {
        let cfg = exe_dir.parent()?.join("pyvenv.cfg");
        let text = std::fs::read_to_string(cfg).ok()?;
        for line in text.lines() {
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            if key.trim().eq_ignore_ascii_case("home") {
                let value = value.trim();
                if !value.is_empty() {
                    return Some(PathBuf::from(value));
                }
            }
        }
        None
    }

    fn to_wide(s: &std::ffi::OsStr) -> Vec<u16> {
        s.encode_wide().chain(std::iter::once(0)).collect()
    }
}

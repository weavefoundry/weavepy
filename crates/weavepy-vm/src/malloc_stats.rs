//! `PYTHONMALLOCSTATS` (RFC 0077 WS7).
//!
//! CPython prints `_PyObject_DebugMallocStats` to stderr when the
//! interpreter finalizes with `PYTHONMALLOCSTATS` set. WeavePy has no
//! pymalloc (objects live on the Rust/system heap), so the report keeps
//! the CPython header shape with the allocator's real parameters (no
//! small-block classes) and adds what the process can measure about
//! itself: the peak resident set size.

use std::sync::atomic::{AtomicBool, Ordering};

static INSTALLED: AtomicBool = AtomicBool::new(false);

/// Arrange for the report to print on process exit (through the C
/// `exit` path, which is how the CLI leaves after `SystemExit` too).
pub fn install_exit_report() {
    if INSTALLED.swap(true, Ordering::Relaxed) {
        return;
    }
    #[cfg(unix)]
    {
        extern "C" fn at_exit() {
            report();
        }
        // SAFETY: `at_exit` is a plain `extern "C" fn()` with no
        // captured state, which is exactly what `atexit` expects.
        unsafe {
            libc::atexit(at_exit);
        }
    }
}

/// Peak resident set size in bytes, when the platform reports it.
fn peak_rss_bytes() -> Option<u64> {
    #[cfg(unix)]
    {
        let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
        // SAFETY: `usage` is a valid, writable `rusage`.
        let rc = unsafe { libc::getrusage(libc::RUSAGE_SELF, &raw mut usage) };
        if rc != 0 {
            return None;
        }
        // macOS reports bytes, Linux/BSD kilobytes.
        let raw = usage.ru_maxrss as u64;
        return Some(if cfg!(target_os = "macos") {
            raw
        } else {
            raw * 1024
        });
    }
    #[cfg(not(unix))]
    {
        None
    }
}

/// Write the report to stderr.
pub fn report() {
    use std::io::Write as _;
    let mut out = std::io::stderr().lock();
    let _ = writeln!(out, "Small block threshold = 0, in 0 size classes.");
    let _ = writeln!(out, "Medium block threshold = 0");
    let _ = writeln!(out, "Large object max size = 0");
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "WeavePy allocates objects on the system heap (no pymalloc arenas)."
    );
    if let Some(rss) = peak_rss_bytes() {
        let _ = writeln!(out, "    Peak resident set size: {rss} bytes");
    }
}

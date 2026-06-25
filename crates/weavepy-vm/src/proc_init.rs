//! Process-start descriptor hygiene (CPython-faithful standard fds).
//!
//! Rust's standard runtime runs `sanitize_standard_fds()` *before* `main`: any
//! of file descriptors 0/1/2 that are **closed** at process start get re-opened
//! onto `/dev/null`. That is a sensible hardening default for ordinary
//! programs, but it diverges from CPython, which leaves an inherited-closed
//! standard descriptor closed. A parent that deliberately closes a child's
//! stdin — e.g.
//! `os.posix_spawn(..., file_actions=[(os.POSIX_SPAWN_CLOSE, 0)])`
//! (`test_posix.test_close_file`) — expects the child's `os.fstat(0)` to raise
//! `EBADF`; with the std sanitizer the child would instead see a live
//! `/dev/null`.
//!
//! To stay faithful we snapshot the descriptor table *before* the std runtime
//! runs — via a C constructor placed in the platform init-array, which the
//! dynamic loader invokes ahead of Rust's `lang_start` — and then, once we are
//! safely inside `main`, re-close any standard descriptor that started closed.
//! This undoes only the sanitizer's `/dev/null` placeholders; a descriptor the
//! parent actually handed us was *open* at snapshot time and is left untouched.

#[cfg(unix)]
use std::sync::atomic::{AtomicU8, Ordering};

/// Bitmask of standard fds (bit `n` ⇒ fd `n`) that were already closed when the
/// process image started, captured before Rust's `sanitize_standard_fds`.
#[cfg(unix)]
static INITIALLY_CLOSED_STD_FDS: AtomicU8 = AtomicU8::new(0);

/// Pre-`main` snapshot of which standard descriptors are closed. Runs from the
/// platform constructor array, *before* the Rust runtime's fd sanitizer.
#[cfg(unix)]
extern "C" fn snapshot_std_fds() {
    let mut mask = 0u8;
    for fd in 0..3 {
        // `F_GETFD` is the cheapest "is this fd valid?" probe and is
        // async-signal-safe. A closed descriptor fails with `EBADF`.
        if unsafe { libc::fcntl(fd, libc::F_GETFD) } == -1 {
            mask |= 1 << fd;
        }
    }
    INITIALLY_CLOSED_STD_FDS.store(mask, Ordering::SeqCst);
}

// Register `snapshot_std_fds` in the platform constructor array so it runs
// before `main` (hence before the std runtime opens `/dev/null` on closed
// standard fds). ELF targets use `.init_array`; Mach-O (Apple) uses
// `__DATA,__mod_init_func`.
#[cfg(all(unix, not(target_vendor = "apple")))]
#[used]
#[link_section = ".init_array"]
static SNAPSHOT_CTOR: extern "C" fn() = snapshot_std_fds;

#[cfg(all(unix, target_vendor = "apple"))]
#[used]
#[link_section = "__DATA,__mod_init_func"]
static SNAPSHOT_CTOR: extern "C" fn() = snapshot_std_fds;

/// Re-close any standard descriptor (0/1/2) that was closed at process start but
/// has since been re-opened onto `/dev/null` by the Rust runtime's
/// `sanitize_standard_fds`. Call once, as early as possible in `main`.
///
/// Faithful to CPython, which leaves an inherited-closed standard descriptor
/// closed (`test_posix.test_close_file`). Descriptors the parent left open are
/// never touched (their snapshot bit is clear).
#[cfg(unix)]
pub fn restore_initial_std_fds() {
    let mask = INITIALLY_CLOSED_STD_FDS.load(Ordering::SeqCst);
    if mask == 0 {
        return;
    }
    for fd in 0..3 {
        if mask & (1 << fd) == 0 {
            continue;
        }
        // Only close if the descriptor is *currently* open — i.e. the sanitizer
        // replaced it. Ignore the result; a redundant close is merely `EBADF`.
        if unsafe { libc::fcntl(fd, libc::F_GETFD) } != -1 {
            unsafe {
                libc::close(fd);
            }
        }
    }
}

#[cfg(not(unix))]
pub fn restore_initial_std_fds() {}

//! `-X importtime` / `PYTHONPROFILEIMPORTTIME` (RFC 0077 WS7).
//!
//! CPython's `import.c` times every *fresh* module load and prints one
//! line per module to stderr as it finishes, innermost first, with the
//! name indented by its nesting depth:
//!
//! ```text
//! import time: self [us] | cumulative | imported package
//! import time:       152 |        152 |   _io
//! import time:        61 |        213 | io
//! ```
//!
//! `self` is the module's own body time (cumulative minus the children
//! it imported); `cumulative` is wall time for the whole load. Cached
//! (`sys.modules`) hits are not reported, matching CPython. The
//! accounting lives on a thread-local stack so nested imports on the
//! same thread attribute correctly; imports on other threads print
//! their own lines.

use std::cell::RefCell;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Instant;

/// `0` off, `1` on. Set once at startup from the CLI (`-X importtime`)
/// or the environment (`PYTHONPROFILEIMPORTTIME`), read on every fresh
/// load with one relaxed byte load.
static ENABLED: AtomicU8 = AtomicU8::new(0);

/// Whether the header line has been printed yet (process-wide).
static HEADER_DONE: AtomicU8 = AtomicU8::new(0);

thread_local! {
    /// One entry per in-flight fresh load on this thread:
    /// `(start, microseconds spent in nested loads)`.
    static STACK: RefCell<Vec<(Instant, u64)>> = const { RefCell::new(Vec::new()) };
}

/// Turn import timing on for the process.
pub fn set_enabled(on: bool) {
    ENABLED.store(u8::from(on), Ordering::Relaxed);
}

/// Whether import timing is on.
#[inline]
pub fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed) != 0
}

/// Begin timing a fresh load. Must be paired with [`finish`].
pub fn begin() {
    STACK.with(|s| s.borrow_mut().push((Instant::now(), 0)));
}

/// Finish timing the innermost fresh load and print its line. Called on
/// both the success and the error path so the stack stays balanced.
pub fn finish(name: &str) {
    let Some((start, children_us)) = STACK.with(|s| s.borrow_mut().pop()) else {
        return;
    };
    let cumulative = start.elapsed().as_micros() as u64;
    let self_us = cumulative.saturating_sub(children_us);
    let depth = STACK.with(|s| {
        let mut stack = s.borrow_mut();
        if let Some(parent) = stack.last_mut() {
            parent.1 += cumulative;
        }
        stack.len()
    });
    if HEADER_DONE.swap(1, Ordering::Relaxed) == 0 {
        eprintln!("import time: self [us] | cumulative | imported package");
    }
    // CPython: `"import time: %9ld | %10ld | %*s%s\n"` with the name
    // indented by one space per nesting level.
    eprintln!(
        "import time: {self_us:>9} | {cumulative:>10} | {:width$}{name}",
        "",
        width = depth
    );
}

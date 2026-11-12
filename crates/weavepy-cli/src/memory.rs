//! What the process does about its own memory as it runs.
//!
//! The runtime ticks where a process may have grown: when a cycle
//! collection comes due, every few thousand calls, returns and loop
//! iterations, and when the JIT's worker runs out of work. At a tick, at
//! most once a millisecond, the process looks at its resident set and:
//!
//! - releases the code pages it no longer runs (see [`crate::text_trim`]);
//! - keeps freed heap resident longer once it's large (see
//!   [`crate::alloc::note_resident`]).

use std::sync::atomic::{AtomicU64, Ordering};

/// When [`tick`] next looks, in microseconds since [`started`], and how
/// long it then waits before looking again: a program allocating flat out
/// ticks thousands of times a second, and asking the kernel for the
/// resident set costs about a microsecond.
static NEXT_LOOK: AtomicU64 = AtomicU64::new(0);
const LOOK_EVERY_MICROS: u64 = 1000;
/// When [`install`] ran.
fn started() -> std::time::Instant {
    static STARTED: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    *STARTED.get_or_init(std::time::Instant::now)
}

/// Start watching the process's memory at the runtime's ticks.
pub(crate) fn install() {
    crate::text_trim::install();
    started();
    weavepy_vm::set_memory_tick_hook(tick);
}

/// One of the runtime's ticks (see the module docs).
pub(crate) extern "C" fn tick() {
    let now = started().elapsed().as_micros() as u64;
    if now < NEXT_LOOK.load(Ordering::Relaxed) {
        return;
    }
    NEXT_LOOK.store(now + LOOK_EVERY_MICROS, Ordering::Relaxed);
    if let Some(resident) = crate::text_trim::resident() {
        crate::alloc::note_resident(resident.all);
        crate::text_trim::grown(resident, now, weavepy_vm::import_events());
    }
}

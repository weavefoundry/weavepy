//! Python-level recursion guard — RFC 0037 (WS1).
//!
//! WeavePy's evaluator is a recursive tree-walker: every Python call
//! activation (`run_until_yield_or_return`) maps onto a native (Rust)
//! stack frame. Without a guard, unbounded Python recursion overflows
//! the native stack and `abort()`s the process (the failure mode RFC
//! 0036 hit on `test_exceptions`).
//!
//! CPython instead raises `RecursionError` once Python call depth
//! crosses `sys.setrecursionlimit` (default 1000). The `weavepy-cli`
//! build reserves enough main-thread stack (8 MiB on Linux/macOS, an
//! explicit 64 MiB reserve on Windows) that the *limit* is reached well
//! before the native stack, so enforcing the limit here is what makes
//! deep recursion fail cleanly and uniformly across platforms.
//!
//! This module owns the process-wide limit (CPython's limit is global —
//! `setrecursionlimit` affects every thread) and a per-thread depth
//! counter, plus a small RAII [`Guard`] the dispatch loop holds so the
//! depth is restored on *every* exit path (return, yield, exception).
//!
//! ## Why we raise on *every* over-limit call
//!
//! CPython raises `RecursionError` on every activation attempted past the
//! limit; its "recursion headroom" is a count of how many times the error
//! machinery may itself recurse before a *fatal* abort — it is **not** a
//! block of extra frames a program may freely use. An earlier design here
//! tolerated 50 free frames once the limit was first exceeded, re-arming
//! the allowance whenever depth dipped back under the ceiling. That turns
//! a function which recurses in *both* its body and its `except` handler
//! (`test_exceptions.test_recursion_in_except_handler`) into an
//! exponential blowup: each partial unwind frees a frame the handler
//! immediately re-consumes, so the stack never actually drains. Raising at
//! the limit every time makes such teardown linear, exactly like CPython.

use std::cell::Cell;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Default `sys.getrecursionlimit()`.
pub const DEFAULT_RECURSION_LIMIT: usize = 1000;

/// Process-wide recursion limit.
static RECURSION_LIMIT: AtomicUsize = AtomicUsize::new(DEFAULT_RECURSION_LIMIT);

thread_local! {
    /// Live Python call depth on *this* thread.
    static DEPTH: Cell<usize> = const { Cell::new(0) };
}

/// Current process-wide recursion limit (`sys.getrecursionlimit()`).
pub fn recursion_limit() -> usize {
    RECURSION_LIMIT.load(Ordering::Relaxed)
}

/// Live Python call depth on the calling thread.
pub fn current_depth() -> usize {
    DEPTH.with(|d| d.get())
}

/// Set a new process-wide limit. Returns `Err(current_depth)` if the
/// requested limit isn't strictly above the calling thread's current
/// depth — CPython raises `RecursionError` in that case so a program
/// can't lower the limit out from under its own live stack.
pub fn set_limit(new_limit: usize) -> Result<(), usize> {
    let depth = current_depth();
    if new_limit <= depth {
        return Err(depth);
    }
    RECURSION_LIMIT.store(new_limit, Ordering::Relaxed);
    Ok(())
}

/// Result of attempting to enter one more activation.
#[derive(Debug)]
pub enum Enter {
    /// Proceed; the caller must hold the [`Guard`] for the activation.
    Ok(Guard),
    /// Limit exceeded — the caller should raise `RecursionError`.
    Overflow,
}

/// RAII handle that restores the per-thread depth on drop. Created only
/// when [`enter`] permits the activation, so the increment and the
/// decrement are always balanced.
#[derive(Debug)]
pub struct Guard {
    _private: (),
}

impl Drop for Guard {
    fn drop(&mut self) {
        DEPTH.with(|d| d.set(d.get().saturating_sub(1)));
    }
}

/// Enter one Python activation. On [`Enter::Ok`] the returned [`Guard`]
/// must stay alive until the activation finishes; dropping it restores
/// the depth.
///
/// Returns [`Enter::Overflow`] — and rolls the (un-run) activation back —
/// whenever the new depth would exceed the limit, on *every* such call.
/// See the module docs for why there is no extra-frame headroom.
pub fn enter() -> Enter {
    let limit = recursion_limit();
    let depth = DEPTH.with(|d| {
        let n = d.get() + 1;
        d.set(n);
        n
    });
    if depth > limit {
        DEPTH.with(|d| d.set(d.get().saturating_sub(1)));
        return Enter::Overflow;
    }
    Enter::Ok(Guard { _private: () })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn depth_balances_across_guards() {
        assert_eq!(current_depth(), 0);
        {
            let _g = match enter() {
                Enter::Ok(g) => g,
                Enter::Overflow => panic!("unexpected overflow"),
            };
            assert_eq!(current_depth(), 1);
            {
                let _g2 = match enter() {
                    Enter::Ok(g) => g,
                    Enter::Overflow => panic!("unexpected overflow"),
                };
                assert_eq!(current_depth(), 2);
            }
            assert_eq!(current_depth(), 1);
        }
        assert_eq!(current_depth(), 0);
    }

    #[test]
    fn over_limit_raises_every_time_without_inflating_depth() {
        // Use a tiny limit on this thread's view via the global atomic.
        // (Tests run single-threaded per #[test] body.)
        let saved = recursion_limit();
        RECURSION_LIMIT.store(4, Ordering::Relaxed);

        let mut guards = Vec::new();
        for _ in 0..4 {
            match enter() {
                Enter::Ok(g) => guards.push(g),
                Enter::Overflow => panic!("should fit under the limit"),
            }
        }
        assert_eq!(current_depth(), 4);
        // Every breach past the limit overflows and leaves depth pinned at
        // the limit — no free "headroom" frames a recursing handler could
        // re-consume. This is what keeps `recurse_in_body_and_except`
        // teardown linear instead of exponential.
        for _ in 0..1000 {
            assert!(matches!(enter(), Enter::Overflow));
            assert_eq!(current_depth(), 4);
        }

        drop(guards);
        assert_eq!(current_depth(), 0);
        RECURSION_LIMIT.store(saved, Ordering::Relaxed);
    }
}

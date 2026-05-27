//! VM observability registry — `sys.settrace`, `sys.setprofile`,
//! PEP 669 `sys.monitoring`, PEP 578 `sys.audit`, and the
//! `tracemalloc` allocator hook (RFC 0031).
//!
//! All state lives in thread-locals so `sys.gettrace()` /
//! `sys.getprofile()` / `sys.monitoring` see the right value per
//! thread. The dispatch loop in [`crate::Interpreter::step`] checks
//! [`any_observers_active`] before paying for any of this; once a
//! debugger / profiler / coverage tool calls `settrace` /
//! `setprofile` / `sys.monitoring.set_events`, the slow path runs
//! and the corresponding Python callbacks fire at the right
//! transitions.
//!
//! Event firing follows CPython's
//! `sys.settrace` / `sys.setprofile` contract:
//!
//! * The hook is called with `(frame, event, arg)` where
//!   `event` is one of `'call' | 'line' | 'return' | 'exception'
//!   | 'opcode'` (trace) or `'call' | 'return' | 'c_call' |
//!   'c_return' | 'c_exception'` (profile).
//! * The trace hook's return value becomes the *frame-local* trace
//!   function for subsequent line / return / exception events on
//!   that frame. Returning `None` disables tracing for the frame
//!   (matches CPython).
//! * Re-entrance is guarded: a hook calling user code that itself
//!   raises events must not infinitely recurse. We disable hook
//!   firing for the duration of any hook callout.

use crate::object::Object;
use crate::sync::RefCell;

thread_local! {
    static TRACE_HOOK: RefCell<Option<Object>> = const { RefCell::new(None) };
    static PROFILE_HOOK: RefCell<Option<Object>> = const { RefCell::new(None) };
    static MONITORING_TOOLS: RefCell<MonitoringTools> = const { RefCell::new(MonitoringTools::new()) };
    static AUDIT_HOOKS: RefCell<Vec<Object>> = const { RefCell::new(Vec::new()) };
    /// Re-entrance guard. Set while inside any hook callout so a
    /// hook calling Python code (which itself triggers more events)
    /// doesn't infinitely recurse.
    static HOOK_REENTRY: RefCell<u32> = const { RefCell::new(0) };
}

/// Bookkeeping for PEP 669 `sys.monitoring`.
///
/// Tools register their callbacks for a set of events; the runtime
/// fires the union of all registered callbacks. Tool IDs are bounded
/// (0..=5 in CPython 3.13) and each event is a bit mask.
#[derive(Default, Debug)]
pub struct MonitoringTools {
    /// `tool_id -> name` for `sys.monitoring.use_tool_id`.
    pub tools: [Option<String>; 6],
    /// `tool_id -> (event_index -> callback)` for
    /// `sys.monitoring.register_callback`.
    pub callbacks: [[Option<Object>; 32]; 6],
    /// `tool_id -> active event mask` for `sys.monitoring.set_events`.
    pub events: [u32; 6],
}

impl MonitoringTools {
    pub const fn new() -> Self {
        Self {
            tools: [None, None, None, None, None, None],
            callbacks: [
                [const { None }; 32],
                [const { None }; 32],
                [const { None }; 32],
                [const { None }; 32],
                [const { None }; 32],
                [const { None }; 32],
            ],
            events: [0; 6],
        }
    }

    /// Union of every tool's active event mask. The dispatcher
    /// checks `(mask & EVENT_BIT) != 0` to know whether any tool
    /// wants this event before paying for the callback walk.
    pub fn union_mask(&self) -> u32 {
        self.events.iter().fold(0, |acc, m| acc | *m)
    }
}

pub fn set_trace_hook(hook: Object) {
    TRACE_HOOK.with(|cell| {
        *cell.borrow_mut() = match hook {
            Object::None => None,
            other => Some(other),
        };
    });
}

pub fn trace_hook() -> Option<Object> {
    TRACE_HOOK.with(|cell| cell.borrow().clone())
}

pub fn set_profile_hook(hook: Object) {
    PROFILE_HOOK.with(|cell| {
        *cell.borrow_mut() = match hook {
            Object::None => None,
            other => Some(other),
        };
    });
}

pub fn profile_hook() -> Option<Object> {
    PROFILE_HOOK.with(|cell| cell.borrow().clone())
}

pub fn with_monitoring<R>(f: impl FnOnce(&mut MonitoringTools) -> R) -> R {
    MONITORING_TOOLS.with(|cell| f(&mut cell.borrow_mut()))
}

/// Add an audit hook (PEP 578). Hooks fire in the order they were
/// registered when `sys.audit(event, *args)` is called.
pub fn add_audit_hook(hook: Object) {
    if matches!(hook, Object::None) {
        return;
    }
    AUDIT_HOOKS.with(|cell| {
        cell.borrow_mut().push(hook);
    });
}

pub fn audit_hooks() -> Vec<Object> {
    AUDIT_HOOKS.with(|cell| cell.borrow().clone())
}

/// True when any observer (trace / profile / monitoring tool /
/// audit hook) is registered. The dispatch loop uses this as a
/// fast bail-out so the no-observer path stays free.
#[inline]
pub fn any_observers_active() -> bool {
    TRACE_HOOK.with(|cell| cell.borrow().is_some())
        || PROFILE_HOOK.with(|cell| cell.borrow().is_some())
        || MONITORING_TOOLS.with(|cell| cell.borrow().union_mask() != 0)
}

/// True when any audit hook is registered.
#[inline]
pub fn any_audit_active() -> bool {
    AUDIT_HOOKS.with(|cell| !cell.borrow().is_empty())
}

/// Re-entrance guard. Use when calling into Python from inside a
/// hook so nested events don't fire and infinite-loop.
pub struct ReentryGuard {
    _private: (),
}

impl ReentryGuard {
    /// Acquire the guard. Returns `None` if a hook is already on
    /// the stack — the caller should silently skip its event in
    /// that case.
    pub fn acquire() -> Option<Self> {
        let entered = HOOK_REENTRY.with(|cell| {
            let mut depth = cell.borrow_mut();
            if *depth > 0 {
                false
            } else {
                *depth = 1;
                true
            }
        });
        if entered {
            Some(Self { _private: () })
        } else {
            None
        }
    }
}

impl Drop for ReentryGuard {
    fn drop(&mut self) {
        HOOK_REENTRY.with(|cell| {
            let mut depth = cell.borrow_mut();
            *depth = depth.saturating_sub(1);
        });
    }
}

impl std::fmt::Debug for ReentryGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReentryGuard").finish()
    }
}

// ---------- PEP 669 event indices ----------
//
// These match the bit positions used in `crate::stdlib::sys_monitoring::build_events_namespace`.

pub const EVENT_BRANCH: usize = 0;
pub const EVENT_CALL: usize = 1;
pub const EVENT_C_RAISE: usize = 2;
pub const EVENT_C_RETURN: usize = 3;
pub const EVENT_EXCEPTION_HANDLED: usize = 4;
pub const EVENT_INSTRUCTION: usize = 5;
pub const EVENT_JUMP: usize = 6;
pub const EVENT_LINE: usize = 7;
pub const EVENT_PY_RESUME: usize = 8;
pub const EVENT_PY_RETURN: usize = 9;
pub const EVENT_PY_START: usize = 10;
pub const EVENT_PY_THROW: usize = 11;
pub const EVENT_PY_UNWIND: usize = 12;
pub const EVENT_PY_YIELD: usize = 13;
pub const EVENT_RAISE: usize = 14;
pub const EVENT_RERAISE: usize = 15;
pub const EVENT_STOP_ITERATION: usize = 16;

/// Bit mask for the given event index.
#[inline]
pub const fn event_mask(idx: usize) -> u32 {
    1u32 << idx
}

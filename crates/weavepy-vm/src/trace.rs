//! Trace and profile hook registry (RFC 0030).
//!
//! Holds the active ``sys.settrace`` / ``sys.setprofile`` callbacks
//! and the PEP 669 (`sys.monitoring`) event registrations on a
//! thread-local basis, so ``sys.gettrace`` / ``sys.getprofile``
//! observe the right value per thread.
//!
//! Wiring the line-level event firing into the VM hot path is
//! deferred to RFC 0031 — the dispatcher would need to check the
//! hook on every opcode, which has a measurable cost on tight
//! arithmetic loops. The current shape is sufficient for the most
//! common consumers (``coverage.py``, ``trace.py``, ``pdb`` set-up)
//! to install themselves without crashing; line events fire only
//! at function entry / return / exception.

use crate::object::Object;
use crate::sync::RefCell;

thread_local! {
    static TRACE_HOOK: RefCell<Option<Object>> = const { RefCell::new(None) };
    static PROFILE_HOOK: RefCell<Option<Object>> = const { RefCell::new(None) };
    static MONITORING_TOOLS: RefCell<MonitoringTools> = const { RefCell::new(MonitoringTools::new()) };
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
    /// `tool_id -> (event -> callback)` for `sys.monitoring.register_callback`.
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

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

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;

use crate::object::Object;
use crate::sync::RefCell;

/// Process-wide count of registered observers (per-thread trace/profile
/// hooks, the all-threads fallbacks, and monitoring tools with a
/// non-empty event mask). Zero ⇒ definitely no observer anywhere, which
/// lets [`any_observers_active`] — called every bytecode step — bail on
/// a single relaxed load instead of three thread-local borrows. Every
/// registration path routes through the setters below, which keep the
/// count in sync on None↔Some transitions.
static OBSERVER_COUNT: AtomicUsize = AtomicUsize::new(0);

/// Adjust [`OBSERVER_COUNT`] for a slot transitioning between empty and
/// occupied.
fn observer_transition(was: bool, is: bool) {
    match (was, is) {
        (false, true) => {
            OBSERVER_COUNT.fetch_add(1, Ordering::AcqRel);
        }
        (true, false) => {
            OBSERVER_COUNT.fetch_sub(1, Ordering::AcqRel);
        }
        _ => {}
    }
}

thread_local! {
    static TRACE_HOOK: RefCell<Option<Object>> = const { RefCell::new(None) };
    static PROFILE_HOOK: RefCell<Option<Object>> = const { RefCell::new(None) };
    static MONITORING_TOOLS: RefCell<MonitoringTools> = RefCell::new(MonitoringTools::new());
    /// Re-entrance guard. Set while inside any hook callout so a
    /// hook calling Python code (which itself triggers more events)
    /// doesn't infinitely recurse.
    static HOOK_REENTRY: RefCell<u32> = const { RefCell::new(0) };
    /// CPython's `PyThreadState_EnterTracing` depth: while > 0, trace
    /// and profile callbacks are suppressed. Raised around audit-hook
    /// invocations (PEP 578: hooks run untraced unless the hook object
    /// carries a truthy `__cantrace__` — test_audit test_cantrace).
    static TRACING_SUPPRESS: RefCell<u32> = const { RefCell::new(0) };
}

// PEP 578 audit hooks are *per-interpreter* in CPython, not per-thread:
// a hook added on the main thread observes `sys.audit` calls made from
// worker threads (`test_audit.test_threading` fires `test.test_func`
// from inside `_thread.start_new_thread`). Object access is GIL-serial.
static AUDIT_HOOKS_GLOBAL: Mutex<Vec<Object>> = Mutex::new(Vec::new());
static AUDIT_SET: AtomicBool = AtomicBool::new(false);

/// RAII for the CPython `EnterTracing`/`LeaveTracing` pair.
#[derive(Debug)]
pub struct TracingSuppressGuard {
    _private: (),
}

impl TracingSuppressGuard {
    pub fn enter() -> Self {
        TRACING_SUPPRESS.with(|c| *c.borrow_mut() += 1);
        Self { _private: () }
    }
}

impl Drop for TracingSuppressGuard {
    fn drop(&mut self) {
        TRACING_SUPPRESS.with(|c| {
            let mut d = c.borrow_mut();
            *d = d.saturating_sub(1);
        });
    }
}

/// True while an audit hook (without `__cantrace__`) is running on
/// this thread: trace/profile events must not fire.
#[inline]
pub fn tracing_suppressed() -> bool {
    TRACING_SUPPRESS.with(|c| *c.borrow() > 0)
}

/// Temporarily lift the suppression for a `__cantrace__` hook
/// (CPython's `LeaveTracing` around the vectorcall). The hook-reentry
/// depth is lifted too: trace/profile callouts share it with audit
/// dispatch, and a `__cantrace__` hook explicitly opts into being
/// traced while it runs.
#[derive(Debug)]
pub struct TracingAllowGuard {
    suppress_restore: u32,
    reentry_restore: u32,
}

impl TracingAllowGuard {
    pub fn enter() -> Self {
        let suppress_restore = TRACING_SUPPRESS.with(|c| {
            let mut d = c.borrow_mut();
            let saved = *d;
            *d = 0;
            saved
        });
        let reentry_restore = HOOK_REENTRY.with(|c| {
            let mut d = c.borrow_mut();
            let saved = *d;
            *d = 0;
            saved
        });
        Self {
            suppress_restore,
            reentry_restore,
        }
    }
}

impl Drop for TracingAllowGuard {
    fn drop(&mut self) {
        let suppress = self.suppress_restore;
        let reentry = self.reentry_restore;
        TRACING_SUPPRESS.with(|c| *c.borrow_mut() = suppress);
        HOOK_REENTRY.with(|c| *c.borrow_mut() = reentry);
    }
}

// `threading.settrace_all_threads` / `setprofile_all_threads` (via
// `sys._settraceallthreads` / `sys._setprofileallthreads`) install a
// hook on *every* thread, including ones already running. WeavePy can't
// write another thread's thread-local, so we keep a process-global
// fallback: a thread with no explicit per-thread hook observes the
// all-threads hook instead. The `*_SET` gates keep the common
// (no-global-hook) path a single relaxed bool load.
static ALL_TRACE_HOOK: Mutex<Option<Object>> = Mutex::new(None);
static ALL_PROFILE_HOOK: Mutex<Option<Object>> = Mutex::new(None);
static ALL_TRACE_SET: AtomicBool = AtomicBool::new(false);
static ALL_PROFILE_SET: AtomicBool = AtomicBool::new(false);

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
    /// `code id -> per-tool local event masks` for
    /// `sys.monitoring.set_local_events` (PEP 669 local events).
    pub local_events: std::collections::HashMap<u64, [u32; 6]>,
    /// Locations a callback returned `sys.monitoring.DISABLE` for:
    /// `(code id, instruction offset, tool, event index)`. Cleared by
    /// `sys.monitoring.restart_events`.
    pub disabled: std::collections::HashSet<(u64, u32, u8, u8)>,
}

impl MonitoringTools {
    pub fn new() -> Self {
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
            local_events: std::collections::HashMap::new(),
            disabled: std::collections::HashSet::new(),
        }
    }

    /// Union of every tool's active event mask — global and local.
    /// The dispatcher checks `(mask & EVENT_BIT) != 0` to know
    /// whether any tool wants this event before paying for the
    /// callback walk.
    pub fn union_mask(&self) -> u32 {
        let global = self.events.iter().fold(0, |acc, m| acc | *m);
        self.local_events
            .values()
            .flatten()
            .fold(global, |acc, m| acc | *m)
    }

    /// Per-tool effective mask for a code object: global events plus
    /// that code's local events.
    pub fn effective_events(&self, tool: usize, code_id: u64) -> u32 {
        self.events[tool]
            | self
                .local_events
                .get(&code_id)
                .map_or(0, |per_tool| per_tool[tool])
    }
}

/// Which observer slot a hook invocation belongs to. Used so that
/// when a hook raises on a non-`exception` event we can disable the
/// *right* hook (CPython turns off the offending trace/profile
/// function and re-raises).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum HookKind {
    Trace,
    Profile,
}

pub fn set_trace_hook(hook: Object) {
    TRACE_HOOK.with(|cell| {
        let mut slot = cell.borrow_mut();
        let was = slot.is_some();
        *slot = match hook {
            Object::None => None,
            other => Some(other),
        };
        observer_transition(was, slot.is_some());
    });
}

pub fn trace_hook() -> Option<Object> {
    // While an audit hook runs (CPython `EnterTracing`), events are
    // suppressed — dispatch sees no hook. `sys.gettrace` uses
    // [`trace_hook_raw`] so introspection still works.
    if tracing_suppressed() {
        return None;
    }
    trace_hook_raw()
}

pub fn trace_hook_raw() -> Option<Object> {
    if let Some(h) = TRACE_HOOK.with(|cell| cell.borrow().clone()) {
        return Some(h);
    }
    if ALL_TRACE_SET.load(Ordering::Acquire) {
        return ALL_TRACE_HOOK.lock().unwrap().clone();
    }
    None
}

/// `threading.settrace_all_threads` — install `hook` on every thread.
/// Sets the calling thread's own hook *and* the process-global
/// fallback, so threads already running (which we can't reach through
/// their thread-locals) observe it via [`trace_hook`].
pub fn set_trace_all_threads(hook: Object) {
    let opt = match hook {
        Object::None => None,
        other => Some(other),
    };
    let was = ALL_TRACE_SET.swap(opt.is_some(), Ordering::AcqRel);
    observer_transition(was, opt.is_some());
    *ALL_TRACE_HOOK.lock().unwrap() = opt.clone();
    TRACE_HOOK.with(|cell| {
        let mut slot = cell.borrow_mut();
        let was = slot.is_some();
        observer_transition(was, opt.is_some());
        *slot = opt;
    });
}

pub fn set_profile_hook(hook: Object) {
    PROFILE_HOOK.with(|cell| {
        let mut slot = cell.borrow_mut();
        let was = slot.is_some();
        *slot = match hook {
            Object::None => None,
            other => Some(other),
        };
        observer_transition(was, slot.is_some());
    });
}

pub fn profile_hook() -> Option<Object> {
    if tracing_suppressed() {
        return None;
    }
    profile_hook_raw()
}

pub fn profile_hook_raw() -> Option<Object> {
    if let Some(h) = PROFILE_HOOK.with(|cell| cell.borrow().clone()) {
        return Some(h);
    }
    if ALL_PROFILE_SET.load(Ordering::Acquire) {
        return ALL_PROFILE_HOOK.lock().unwrap().clone();
    }
    None
}

/// `threading.setprofile_all_threads` — see [`set_trace_all_threads`].
pub fn set_profile_all_threads(hook: Object) {
    let opt = match hook {
        Object::None => None,
        other => Some(other),
    };
    let was = ALL_PROFILE_SET.swap(opt.is_some(), Ordering::AcqRel);
    observer_transition(was, opt.is_some());
    *ALL_PROFILE_HOOK.lock().unwrap() = opt.clone();
    PROFILE_HOOK.with(|cell| {
        let mut slot = cell.borrow_mut();
        let was = slot.is_some();
        observer_transition(was, opt.is_some());
        *slot = opt;
    });
}

pub fn with_monitoring<R>(f: impl FnOnce(&mut MonitoringTools) -> R) -> R {
    MONITORING_TOOLS.with(|cell| {
        let mut tools = cell.borrow_mut();
        let was = tools.union_mask() != 0;
        let r = f(&mut tools);
        observer_transition(was, tools.union_mask() != 0);
        r
    })
}

/// Add an audit hook (PEP 578). Hooks fire in the order they were
/// registered when `sys.audit(event, *args)` is called.
pub fn add_audit_hook(hook: Object) {
    if matches!(hook, Object::None) {
        return;
    }
    AUDIT_HOOKS_GLOBAL.lock().unwrap().push(hook);
    AUDIT_SET.store(true, Ordering::Release);
}

pub fn audit_hooks() -> Vec<Object> {
    AUDIT_HOOKS_GLOBAL.lock().unwrap().clone()
}

/// True when any observer (trace / profile / monitoring tool /
/// audit hook) is registered. The dispatch loop uses this as a
/// fast bail-out so the no-observer path stays free.
///
/// The common no-observer case is a single relaxed load of
/// [`OBSERVER_COUNT`]; the per-thread re-check below only runs once
/// some thread has registered something. (The count is a process-wide
/// over-approximation — a hook on thread A makes thread B take the slow
/// re-check — but observers are vanishingly rare outside debuggers.)
#[inline]
pub fn any_observers_active() -> bool {
    if OBSERVER_COUNT.load(Ordering::Relaxed) == 0 {
        return false;
    }
    TRACE_HOOK.with(|cell| cell.borrow().is_some())
        || PROFILE_HOOK.with(|cell| cell.borrow().is_some())
        || ALL_TRACE_SET.load(Ordering::Acquire)
        || ALL_PROFILE_SET.load(Ordering::Acquire)
        || MONITORING_TOOLS.with(|cell| cell.borrow().union_mask() != 0)
}

/// True when any audit hook is registered.
#[inline]
pub fn any_audit_active() -> bool {
    AUDIT_SET.load(Ordering::Acquire)
}

// ---------------------------------------------------------------------------
// RFC 0060 — `_testinternalcapi.set_eval_frame_record` (CPython's
// `_PyInterpreterState_SetEvalFrameFunc` probe, test_optimizer /
// test_capi). While installed, every frame evaluation appends its code
// object to the caller's list; `set_eval_frame_default` uninstalls it.
// The gate is one relaxed load on frame entry, zero when unused.
// ---------------------------------------------------------------------------

static EVAL_FRAME_RECORD_ACTIVE: AtomicBool = AtomicBool::new(false);
static EVAL_FRAME_RECORD: Mutex<Option<Object>> = Mutex::new(None);

pub fn set_eval_frame_record(list: Object) {
    *EVAL_FRAME_RECORD.lock().unwrap() = Some(list);
    EVAL_FRAME_RECORD_ACTIVE.store(true, Ordering::Release);
}

pub fn set_eval_frame_default() {
    EVAL_FRAME_RECORD_ACTIVE.store(false, Ordering::Release);
    *EVAL_FRAME_RECORD.lock().unwrap() = None;
}

#[inline]
pub fn eval_frame_record_active() -> bool {
    EVAL_FRAME_RECORD_ACTIVE.load(Ordering::Relaxed)
}

/// Append one executed frame's code object to the recording list.
pub fn record_eval_frame(code: Object) {
    if let Some(Object::List(l)) = &*EVAL_FRAME_RECORD.lock().unwrap() {
        l.borrow_mut().push(code);
    }
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

/// Events that may be set per-code-object (`set_local_events`) and
/// whose callbacks may return `sys.monitoring.DISABLE`. Everything
/// else (the exception family and the C-call results) is global-only
/// and non-disableable, per PEP 669.
pub const LOCAL_EVENTS_MASK: u32 = event_mask(EVENT_PY_START)
    | event_mask(EVENT_PY_RESUME)
    | event_mask(EVENT_PY_RETURN)
    | event_mask(EVENT_PY_YIELD)
    | event_mask(EVENT_CALL)
    | event_mask(EVENT_LINE)
    | event_mask(EVENT_INSTRUCTION)
    | event_mask(EVENT_JUMP)
    | event_mask(EVENT_BRANCH)
    | event_mask(EVENT_STOP_ITERATION);

/// Every settable event bit. `C_RETURN`/`C_RAISE` are in the word but
/// `set_events` rejects them (they fire whenever `CALL` is set).
pub const ALL_EVENTS_MASK: u32 = (1u32 << 17) - 1;

/// Human-readable PEP 669 event name (for `DISABLE` error messages).
pub fn monitoring_event_name(event_idx: usize) -> &'static str {
    match event_idx {
        EVENT_BRANCH => "BRANCH",
        EVENT_CALL => "CALL",
        EVENT_C_RAISE => "C_RAISE",
        EVENT_C_RETURN => "C_RETURN",
        EVENT_EXCEPTION_HANDLED => "EXCEPTION_HANDLED",
        EVENT_INSTRUCTION => "INSTRUCTION",
        EVENT_JUMP => "JUMP",
        EVENT_LINE => "LINE",
        EVENT_PY_RESUME => "PY_RESUME",
        EVENT_PY_RETURN => "PY_RETURN",
        EVENT_PY_START => "PY_START",
        EVENT_PY_THROW => "PY_THROW",
        EVENT_PY_UNWIND => "PY_UNWIND",
        EVENT_PY_YIELD => "PY_YIELD",
        EVENT_RAISE => "RAISE",
        EVENT_RERAISE => "RERAISE",
        EVENT_STOP_ITERATION => "STOP_ITERATION",
        _ => "?",
    }
}

/// Union of every tool's active monitoring mask (global + local).
pub fn monitoring_union_mask() -> u32 {
    MONITORING_TOOLS.with(|cell| cell.borrow().union_mask())
}

// The `sys.monitoring.DISABLE` / `MISSING` sentinels. Identity
// objects: the dispatcher compares callback return values against
// `DISABLE` by pointer, so the module namespace and the dispatcher
// must share the very same objects. Process-global (like the audit
// hooks) so every interpreter/thread observes one canonical pair;
// object access is GIL-serial.
static MON_SENTINELS: Mutex<Option<(Object, Object)>> = Mutex::new(None);

/// `(DISABLE, MISSING)`.
pub fn monitoring_sentinels() -> (Object, Object) {
    let mut guard = MON_SENTINELS.lock().unwrap();
    if guard.is_none() {
        // Tag each sentinel so the two never compare `==` to each
        // other (SimpleNamespace equality is dict-based) and reprs
        // are self-describing.
        let mk = |tag: &'static str| {
            let mut d = crate::object::DictData::default();
            d.insert(
                crate::object::DictKey(Object::from_static("_name")),
                Object::from_static(tag),
            );
            Object::SimpleNamespace(crate::sync::Rc::new(RefCell::new(d)))
        };
        *guard = Some((mk("DISABLE"), mk("MISSING")));
    }
    guard.clone().expect("just initialized")
}

//! RFC 0047 (wave 5): the PEP 669 `sys.monitoring` C-API.
//!
//! Cython compiles every function — including a module's top-level
//! `__pyx_pymod_exec_*` body — with profiling/line-tracing hooks when the
//! wheel is built with `linetrace=True` / `-DCYTHON_TRACE(_NOGIL)=1`. This
//! is *not* an exotic configuration: `frozenlist`, `aiohttp`'s `_helpers`,
//! and many other published wheels enable it in their default source build
//! (it is inert at runtime unless a tracer is installed).
//!
//! On CPython 3.13 those hooks lower onto the `sys.monitoring` C-API
//! (`CYTHON_USE_SYS_MONITORING`): `Cython/Utility/Profile.c` emits, at every
//! function entry,
//!
//! ```c
//! memset(state_array, 0, sizeof(state_array));
//! if (!tstate->tracing) {
//!     code = __Pyx_createFrameCodeObject(...);          // PyCode_NewEmpty
//!     ret  = __Pyx__TraceStartFunc(state_array, code, ...);
//! }
//! // __Pyx__TraceStartFunc:
//! //   PyMonitoring_EnterScope(state_array, &version, event_types, n);
//! //   PyMonitoring_FirePyStartEvent(&state_array[PY_START], code, offset);
//! ```
//!
//! `PyMonitoring_FirePyStartEvent` (and the other `Fire*` helpers) are
//! `static inline` in `cpython/monitoring.h` and short-circuit on
//! `state->active`, so once the state array reports "inactive" they never
//! reach the out-of-line `_PyMonitoring_Fire*Event` body. **But
//! `PyMonitoring_EnterScope` / `PyMonitoring_ExitScope` are real exported
//! functions** that Cython calls unconditionally. Without them the macOS
//! dynamic loader (`-undefined dynamic_lookup`) lets the `.so` load and then
//! binds the first `PyMonitoring_EnterScope` call to address 0 → a NULL-call
//! segfault inside `__Pyx__TraceStartFunc`, before the first line of the
//! extension's own code runs.
//!
//! WeavePy does not implement bytecode instrumentation, so **no monitoring
//! event is ever active**. The faithful behaviour for such an
//! un-instrumented interpreter is exactly what CPython does when nothing is
//! being monitored: `EnterScope` reports every requested event as inactive
//! (and records the current monitoring version so a repeat call fast-paths),
//! `ExitScope` is a no-op, and the out-of-line `Fire*` bodies — reached only
//! when a state is active, which never happens — return 0 ("no error, not
//! handled"). With the state array left inactive, Cython's inline
//! `Fire*Event` shims return 0 and the whole trace path collapses to a
//! couple of branches, just as it does on stock CPython with no tracer set.

#![allow(clippy::missing_safety_doc)]

use core::ffi::c_int;

use crate::object::PyObject;

/// `PyMonitoringState` — `cpython/monitoring.h`. A two-byte per-event cell
/// the interpreter keeps in sync with the active tool set; `active` gates the
/// inline `PyMonitoring_Fire*Event` fast path.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct PyMonitoringState {
    pub active: u8,
    pub opaque: u8,
}

/// The monitoring version WeavePy reports. Monitoring is never enabled, so
/// the active configuration never changes and a single stable version is
/// faithful: a caller that cached this value short-circuits its next
/// `EnterScope`, and one that didn't gets the (already inactive) states
/// recomputed.
const WEAVEPY_MONITORING_VERSION: u64 = 0;

/// `PyMonitoring_EnterScope(state_array, version, event_types, length)` —
/// synchronise a function's local monitoring-state array with the
/// interpreter's active tool set on scope entry.
///
/// WeavePy monitors nothing, so every requested event is inactive. We clear
/// the `length` cells the caller asked about and stamp the stable monitoring
/// version into `*version`. Returns 0 (success); CPython only returns -1 on
/// an allocation failure that cannot occur here.
#[no_mangle]
pub unsafe extern "C" fn PyMonitoring_EnterScope(
    state_array: *mut PyMonitoringState,
    version: *mut u64,
    _event_types: *const u8,
    length: isize,
) -> c_int {
    if !state_array.is_null() && length > 0 {
        for i in 0..length {
            let cell = unsafe { &mut *state_array.offset(i) };
            cell.active = 0;
            cell.opaque = 0;
        }
    }
    if !version.is_null() {
        unsafe { *version = WEAVEPY_MONITORING_VERSION };
    }
    0
}

/// `PyMonitoring_ExitScope()` — leave the current monitoring scope. With no
/// active monitoring there is no per-scope state to unwind.
#[no_mangle]
pub unsafe extern "C" fn PyMonitoring_ExitScope() -> c_int {
    0
}

// ---------------------------------------------------------------------------
// Out-of-line `_PyMonitoring_Fire*Event` bodies.
//
// Reached only through the inline `PyMonitoring_Fire*Event` shims in
// `cpython/monitoring.h`, each of which returns early unless its
// `PyMonitoringState::active` byte is set. Because `EnterScope` always
// leaves every state inactive under WeavePy, these are never actually
// invoked — but Cython's generated `.so` references their symbols, so they
// must resolve to a real address. Each is the correct "nothing happened"
// answer: 0 (no error; the event was not handled).
// ---------------------------------------------------------------------------

macro_rules! fire_event_noop {
    ($($name:ident),* $(,)?) => {
        $(
            #[no_mangle]
            pub unsafe extern "C" fn $name(
                _state: *mut PyMonitoringState,
                _codelike: *mut PyObject,
                _offset: i32,
            ) -> c_int {
                0
            }
        )*
    };
}

// Events whose ABI is (state, codelike, offset).
fire_event_noop!(
    _PyMonitoring_FirePyStartEvent,
    _PyMonitoring_FirePyResumeEvent,
    _PyMonitoring_FirePyThrowEvent,
    _PyMonitoring_FireRaiseEvent,
    _PyMonitoring_FireReraiseEvent,
    _PyMonitoring_FireExceptionHandledEvent,
    _PyMonitoring_FireCRaiseEvent,
    _PyMonitoring_FirePyUnwindEvent,
);

/// `(state, codelike, offset, retval)` — Python return.
#[no_mangle]
pub unsafe extern "C" fn _PyMonitoring_FirePyReturnEvent(
    _state: *mut PyMonitoringState,
    _codelike: *mut PyObject,
    _offset: i32,
    _retval: *mut PyObject,
) -> c_int {
    0
}

/// `(state, codelike, offset, retval)` — generator yield.
#[no_mangle]
pub unsafe extern "C" fn _PyMonitoring_FirePyYieldEvent(
    _state: *mut PyMonitoringState,
    _codelike: *mut PyObject,
    _offset: i32,
    _retval: *mut PyObject,
) -> c_int {
    0
}

/// `(state, codelike, offset, retval)` — C function return.
#[no_mangle]
pub unsafe extern "C" fn _PyMonitoring_FireCReturnEvent(
    _state: *mut PyMonitoringState,
    _codelike: *mut PyObject,
    _offset: i32,
    _retval: *mut PyObject,
) -> c_int {
    0
}

/// `(state, codelike, offset, callable, arg0)` — call event.
#[no_mangle]
pub unsafe extern "C" fn _PyMonitoring_FireCallEvent(
    _state: *mut PyMonitoringState,
    _codelike: *mut PyObject,
    _offset: i32,
    _callable: *mut PyObject,
    _arg0: *mut PyObject,
) -> c_int {
    0
}

/// `(state, codelike, offset, lineno)` — line event.
#[no_mangle]
pub unsafe extern "C" fn _PyMonitoring_FireLineEvent(
    _state: *mut PyMonitoringState,
    _codelike: *mut PyObject,
    _offset: i32,
    _lineno: c_int,
) -> c_int {
    0
}

/// `(state, codelike, offset, target_offset)` — jump event.
#[no_mangle]
pub unsafe extern "C" fn _PyMonitoring_FireJumpEvent(
    _state: *mut PyMonitoringState,
    _codelike: *mut PyObject,
    _offset: i32,
    _target_offset: *mut PyObject,
) -> c_int {
    0
}

/// `(state, codelike, offset, target_offset)` — branch event.
#[no_mangle]
pub unsafe extern "C" fn _PyMonitoring_FireBranchEvent(
    _state: *mut PyMonitoringState,
    _codelike: *mut PyObject,
    _offset: i32,
    _target_offset: *mut PyObject,
) -> c_int {
    0
}

/// `(state, codelike, offset, value)` — `StopIteration`.
#[no_mangle]
pub unsafe extern "C" fn _PyMonitoring_FireStopIterationEvent(
    _state: *mut PyMonitoringState,
    _codelike: *mut PyObject,
    _offset: i32,
    _value: *mut PyObject,
) -> c_int {
    0
}

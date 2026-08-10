//! RFC 0060 WS5c — native `_testcapi` monitoring fixtures: the PEP 669
//! C-API surface (`PyMonitoring_EnterScope` / `PyMonitoring_Fire*Event`)
//! exercised by `test_monitoring.TestCApiEventGeneration`.
//!
//! These must be *native* builtins: the tests count real interpreter
//! events alongside the synthetically fired ones, so the fire
//! primitives cannot execute Python bytecode of their own (a pure
//! Python shim would drown the counters in its own CALL/LINE/PY_START
//! events).
//!
//! The `CodeLike` object itself is a plain Python class (defined in
//! the `_testcapi` shim) whose `_active` attribute is a list of
//! per-state tool bitmasks — the analogue of CPython's
//! `PyMonitoringState.active`. `monitoring_enter_scope` populates it
//! from the current global subscriptions; each fire consults and
//! (on `DISABLE`) updates it in place.

use crate::error::{type_error, value_error, RuntimeError};
use crate::object::{BuiltinFn, DictData, DictKey, Object, StrKey};
use crate::sync::{Rc, RefCell};

/// Extract the `_active` state list from a CodeLike instance.
fn codelike_active(cl: &Object) -> Result<Rc<RefCell<Vec<Object>>>, RuntimeError> {
    if let Object::Instance(inst) = cl {
        if let Some(Object::List(l)) = inst.dict.borrow().get(&StrKey("_active")) {
            return Ok(l.clone());
        }
    }
    Err(type_error(format!(
        "expected a code-like, got {}",
        cl.type_name()
    )))
}

fn int_arg(args: &[Object], pos: usize, func: &str) -> Result<i64, RuntimeError> {
    match args.get(pos) {
        Some(Object::Int(i)) => Ok(*i),
        Some(other) => Err(type_error(format!(
            "{func}: argument {pos} must be int, not {}",
            other.type_name()
        ))),
        None => Err(type_error(format!("{func}: missing argument {pos}"))),
    }
}

/// `monitoring_enter_scope(codelike, event1[, event2])` —
/// `PyMonitoring_EnterScope`: snapshot, per state slot, the bitmask of
/// tools currently subscribed (globally) to that slot's event.
fn enter_scope(args: &[Object]) -> Result<Object, RuntimeError> {
    let cl = args
        .first()
        .ok_or_else(|| type_error("monitoring_enter_scope: missing code-like"))?;
    let active = codelike_active(cl)?;
    let mut events: Vec<usize> = Vec::new();
    for pos in 1..args.len() {
        events.push(int_arg(args, pos, "monitoring_enter_scope")? as usize);
    }
    crate::trace::with_monitoring(|m| {
        let mut a = active.borrow_mut();
        for (i, ev) in events.iter().enumerate() {
            if i >= a.len() || *ev >= 32 {
                continue;
            }
            let mut mask: i64 = 0;
            for tool in 0..6 {
                if m.events[tool] & (1u32 << *ev) != 0 {
                    mask |= 1 << tool;
                }
            }
            a[i] = Object::Int(mask);
        }
    });
    Ok(Object::None)
}

fn exit_scope(_args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(Object::None)
}

/// Argument/`exc` shape of a fire primitive.
enum FireKind {
    /// `(codelike, offset)` -> callback `(codelike, offset)`.
    Plain,
    /// `(codelike, offset, value)` -> callback `(codelike, offset, value)`.
    Value,
    /// `(codelike, offset, callable, arg0)` -> 4-arg callback.
    Call,
    /// `(codelike, offset, lineno)` -> callback `(codelike, lineno)`.
    Line,
    /// `(codelike, offset, exc)`; `exc=None` is a ValueError
    /// ("no exception set").
    Exception,
    /// `(codelike, offset, value)` -> callback gets `StopIteration(value)`.
    StopIteration,
}

/// The C-API fire core. Walks the state slot's tool mask
/// (highest tool first, like CPython's `most_significant_bit` loop),
/// invoking each tool's callback for `event_idx`. `DISABLE` clears the
/// tool's bit in the state (instrumented events only). Returns the
/// state's remaining mask, matching `RETURN_INT(state->active)`.
fn fire(event_idx: usize, kind: FireKind, args: &[Object]) -> Result<Object, RuntimeError> {
    let cl = args
        .first()
        .ok_or_else(|| type_error("fire_event: missing code-like"))?;
    let active = codelike_active(cl)?;
    let offset = int_arg(args, 1, "fire_event")?;
    let slot = offset as usize;

    // Build the callback argument vector.
    let offset_obj = Object::Int(offset);
    let cb_args: Vec<Object> = match &kind {
        FireKind::Plain => vec![cl.clone(), offset_obj],
        FireKind::Value | FireKind::Call => {
            let mut v = vec![cl.clone(), offset_obj];
            v.extend(args[2..].iter().cloned());
            v
        }
        FireKind::Line => vec![cl.clone(), args.get(2).cloned().unwrap_or(Object::None)],
        FireKind::Exception => {
            let exc = args.get(2).cloned().unwrap_or(Object::None);
            if matches!(exc, Object::None) {
                return Err(value_error(format!(
                    "Firing event {event_idx} with no exception set"
                )));
            }
            vec![cl.clone(), offset_obj, exc]
        }
        FireKind::StopIteration => {
            // `PyMonitoring_FireStopIterationEvent` wraps the value in
            // a StopIteration exception before dispatch.
            let value = args.get(2).cloned().unwrap_or(Object::None);
            let exc_obj = match crate::error::stop_iteration_with(value) {
                RuntimeError::PyException(pe) => pe.instance.clone(),
                _ => Object::None,
            };
            vec![cl.clone(), offset_obj, exc_obj]
        }
    };

    let mask = match active.borrow().get(slot) {
        Some(Object::Int(m)) => *m,
        _ => 0,
    };
    if mask == 0 {
        return Ok(Object::Int(0));
    }
    let Some(ptr) = crate::vm_singletons::current_interpreter_ptr() else {
        return Ok(Object::Int(mask));
    };
    // SAFETY: published by an enclosing VM frame live on this thread;
    // the GIL serializes access.
    let interp = unsafe { &mut *ptr };
    let outer = interp.builtins_dict();
    let (disable, _missing) = crate::trace::monitoring_sentinels();
    let Some(_guard) = crate::trace::ReentryGuard::acquire() else {
        return Ok(Object::Int(mask));
    };
    for tool in (0..6usize).rev() {
        if mask & (1 << tool) == 0 {
            continue;
        }
        let cb = crate::trace::with_monitoring(|m| m.callbacks[tool][event_idx].clone());
        let Some(cb) = cb else { continue };
        let r = interp.call_object_with_globals(&cb, &cb_args, &[], &outer)?;
        if r.is_same(&disable) {
            if crate::trace::event_mask(event_idx) & crate::trace::LOCAL_EVENTS_MASK == 0 {
                // Uninstrumented events cannot be disabled; CPython
                // also drops the callback to break the loop.
                crate::trace::with_monitoring(|m| {
                    m.callbacks[tool][event_idx] = None;
                });
                return Err(value_error(format!(
                    "Cannot disable {} events. Callback removed.",
                    crate::trace::monitoring_event_name(event_idx)
                )));
            }
            let mut a = active.borrow_mut();
            if let Some(Object::Int(m)) = a.get(slot).cloned() {
                a[slot] = Object::Int(m & !(1i64 << tool));
            }
        }
    }
    let remaining = match active.borrow().get(slot) {
        Some(Object::Int(m)) => *m,
        _ => 0,
    };
    Ok(Object::Int(remaining))
}

/// Install the fixture functions into a module dict.
pub fn install(d: &mut DictData) {
    fn b(
        d: &mut DictData,
        name: &'static str,
        body: impl Fn(&[Object]) -> Result<Object, RuntimeError> + Send + Sync + 'static,
    ) {
        d.insert(
            DictKey(Object::from_static(name)),
            Object::Builtin(Rc::new(BuiltinFn {
                name,
                binds_instance: false,
                call: Box::new(body),
                call_kw: None,
            })),
        );
    }
    use crate::trace as t;
    b(d, "monitoring_enter_scope", enter_scope);
    b(d, "monitoring_exit_scope", exit_scope);
    b(d, "fire_event_py_start", |a| {
        fire(t::EVENT_PY_START, FireKind::Plain, a)
    });
    b(d, "fire_event_py_resume", |a| {
        fire(t::EVENT_PY_RESUME, FireKind::Plain, a)
    });
    b(d, "fire_event_py_return", |a| {
        fire(t::EVENT_PY_RETURN, FireKind::Value, a)
    });
    b(d, "fire_event_c_return", |a| {
        fire(t::EVENT_C_RETURN, FireKind::Value, a)
    });
    b(d, "fire_event_py_yield", |a| {
        fire(t::EVENT_PY_YIELD, FireKind::Value, a)
    });
    b(d, "fire_event_call", |a| {
        fire(t::EVENT_CALL, FireKind::Call, a)
    });
    b(d, "fire_event_line", |a| {
        fire(t::EVENT_LINE, FireKind::Line, a)
    });
    b(d, "fire_event_jump", |a| {
        fire(t::EVENT_JUMP, FireKind::Value, a)
    });
    b(d, "fire_event_branch", |a| {
        fire(t::EVENT_BRANCH, FireKind::Value, a)
    });
    b(d, "fire_event_py_throw", |a| {
        fire(t::EVENT_PY_THROW, FireKind::Exception, a)
    });
    b(d, "fire_event_raise", |a| {
        fire(t::EVENT_RAISE, FireKind::Exception, a)
    });
    b(d, "fire_event_c_raise", |a| {
        fire(t::EVENT_C_RAISE, FireKind::Exception, a)
    });
    b(d, "fire_event_reraise", |a| {
        fire(t::EVENT_RERAISE, FireKind::Exception, a)
    });
    b(d, "fire_event_exception_handled", |a| {
        fire(t::EVENT_EXCEPTION_HANDLED, FireKind::Exception, a)
    });
    b(d, "fire_event_py_unwind", |a| {
        fire(t::EVENT_PY_UNWIND, FireKind::Exception, a)
    });
    b(d, "fire_event_stop_iteration", |a| {
        fire(t::EVENT_STOP_ITERATION, FireKind::StopIteration, a)
    });
}

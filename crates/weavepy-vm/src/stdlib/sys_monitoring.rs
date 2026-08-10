//! PEP 669 — `sys.monitoring` (RFC 0060 WS5c).
//!
//! The user-facing API surface: tool registration, global and local
//! event masks, callback registration, the `DISABLE`/`MISSING`
//! sentinels, and `restart_events`. Event *firing* lives in the
//! interpreter dispatch loop (`Interpreter::fire_monitoring_event`
//! and the CALL-family instrumentation in `dispatch_call`).
//!
//! The persistent state lives in [`crate::trace::MonitoringTools`]
//! so it's thread-local and shared with `sys.gettrace` /
//! `sys.getprofile`.

use crate::error::{type_error, value_error, RuntimeError};
use crate::object::{BuiltinFn, DictData, DictKey, Object, PyModule};
use crate::sync::{Rc, RefCell};
use crate::trace::with_monitoring;

/// `(name, bit)` for every PEP 669 event, in bit order. Shared by the
/// `events` namespace and `_all_events()`.
const EVENT_NAMES: &[(&str, usize)] = &[
    ("BRANCH", crate::trace::EVENT_BRANCH),
    ("CALL", crate::trace::EVENT_CALL),
    ("C_RAISE", crate::trace::EVENT_C_RAISE),
    ("C_RETURN", crate::trace::EVENT_C_RETURN),
    ("EXCEPTION_HANDLED", crate::trace::EVENT_EXCEPTION_HANDLED),
    ("INSTRUCTION", crate::trace::EVENT_INSTRUCTION),
    ("JUMP", crate::trace::EVENT_JUMP),
    ("LINE", crate::trace::EVENT_LINE),
    ("PY_RESUME", crate::trace::EVENT_PY_RESUME),
    ("PY_RETURN", crate::trace::EVENT_PY_RETURN),
    ("PY_START", crate::trace::EVENT_PY_START),
    ("PY_THROW", crate::trace::EVENT_PY_THROW),
    ("PY_UNWIND", crate::trace::EVENT_PY_UNWIND),
    ("PY_YIELD", crate::trace::EVENT_PY_YIELD),
    ("RAISE", crate::trace::EVENT_RAISE),
    ("RERAISE", crate::trace::EVENT_RERAISE),
    ("STOP_ITERATION", crate::trace::EVENT_STOP_ITERATION),
];

pub fn build() -> Object {
    let dict = Rc::new(RefCell::new(DictData::default()));
    {
        let mut d = dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("__name__")),
            Object::from_static("sys.monitoring"),
        );
        // Tool ID constants — CPython 3.13 enumerates exactly six.
        d.insert(DictKey(Object::from_static("DEBUGGER_ID")), Object::Int(0));
        d.insert(DictKey(Object::from_static("COVERAGE_ID")), Object::Int(1));
        d.insert(DictKey(Object::from_static("PROFILER_ID")), Object::Int(2));
        d.insert(DictKey(Object::from_static("OPTIMIZER_ID")), Object::Int(5));

        // Sentinels — the same identity objects the dispatcher compares
        // against (a callback returning `DISABLE` is a pointer check).
        let (disable, missing) = crate::trace::monitoring_sentinels();
        d.insert(DictKey(Object::from_static("DISABLE")), disable);
        d.insert(DictKey(Object::from_static("MISSING")), missing);

        // Tool ID + event registration.
        d.insert(
            DictKey(Object::from_static("use_tool_id")),
            builtin("use_tool_id", mon_use_tool_id),
        );
        d.insert(
            DictKey(Object::from_static("free_tool_id")),
            builtin("free_tool_id", mon_free_tool_id),
        );
        d.insert(
            DictKey(Object::from_static("clear_tool_id")),
            builtin("clear_tool_id", mon_clear_tool_id),
        );
        d.insert(
            DictKey(Object::from_static("get_tool")),
            builtin("get_tool", mon_get_tool),
        );
        d.insert(
            DictKey(Object::from_static("set_events")),
            builtin("set_events", mon_set_events),
        );
        d.insert(
            DictKey(Object::from_static("get_events")),
            builtin("get_events", mon_get_events),
        );
        d.insert(
            DictKey(Object::from_static("set_local_events")),
            builtin("set_local_events", mon_set_local_events),
        );
        d.insert(
            DictKey(Object::from_static("get_local_events")),
            builtin("get_local_events", mon_get_local_events),
        );
        d.insert(
            DictKey(Object::from_static("register_callback")),
            builtin("register_callback", mon_register_callback),
        );
        d.insert(
            DictKey(Object::from_static("restart_events")),
            builtin("restart_events", mon_restart_events),
        );
        d.insert(
            DictKey(Object::from_static("_all_events")),
            builtin("_all_events", mon_all_events),
        );

        // `sys.monitoring.events` namespace — one bit per event kind.
        d.insert(
            DictKey(Object::from_static("events")),
            build_events_namespace(),
        );
    }
    Object::Module(Rc::new(PyModule {
        name: "sys.monitoring".to_owned(),
        filename: None,
        dict,
    }))
}

fn builtin(name: &'static str, body: fn(&[Object]) -> Result<Object, RuntimeError>) -> Object {
    Object::Builtin(Rc::new(BuiltinFn {
        name,
        binds_instance: false,
        call: Box::new(body),
        call_kw: None,
    }))
}

fn build_events_namespace() -> Object {
    let mut ns = DictData::default();
    ns.insert(DictKey(Object::from_static("NO_EVENTS")), Object::Int(0));
    for &(name, idx) in EVENT_NAMES {
        ns.insert(
            DictKey(Object::from_static(name)),
            Object::Int(i64::from(crate::trace::event_mask(idx))),
        );
    }
    Object::SimpleNamespace(Rc::new(RefCell::new(ns)))
}

fn pop_tool_id(args: &[Object], func: &str) -> Result<usize, RuntimeError> {
    match args.first() {
        Some(Object::Int(i)) => {
            if *i < 0 || *i >= 6 {
                Err(value_error(format!(
                    "{func}: invalid tool {i} (must be between 0 and 5)"
                )))
            } else {
                Ok(*i as usize)
            }
        }
        Some(other) => Err(type_error(format!(
            "{func}: tool id must be int, not '{}'",
            other.type_name()
        ))),
        None => Err(type_error(format!("{func}: missing tool id"))),
    }
}

fn code_id_arg(args: &[Object], pos: usize, func: &str) -> Result<u64, RuntimeError> {
    match args.get(pos) {
        Some(Object::Code(c)) => Ok(Rc::as_ptr(c) as u64),
        Some(other) => Err(type_error(format!(
            "{func}: code must be a code object, not '{}'",
            other.type_name()
        ))),
        None => Err(type_error(format!("{func}: missing code argument"))),
    }
}

fn mask_arg(args: &[Object], pos: usize, func: &str) -> Result<u32, RuntimeError> {
    match args.get(pos) {
        Some(Object::Int(i)) => {
            if *i < 0 || *i > i64::from(crate::trace::ALL_EVENTS_MASK) {
                Err(value_error(format!("invalid event set 0x{i:x}")))
            } else {
                Ok(*i as u32)
            }
        }
        Some(other) => Err(type_error(format!(
            "{func}: event set must be int, not '{}'",
            other.type_name()
        ))),
        None => Err(type_error(format!("{func}: missing event set"))),
    }
}

fn mon_use_tool_id(args: &[Object]) -> Result<Object, RuntimeError> {
    let id = pop_tool_id(args, "use_tool_id")?;
    let name = match args.get(1) {
        Some(Object::Str(s)) => s.to_string(),
        Some(other) => {
            return Err(type_error(format!(
                "use_tool_id: name must be str, not '{}'",
                other.type_name()
            )))
        }
        None => return Err(type_error("use_tool_id: name required")),
    };
    with_monitoring(|m| {
        if m.tools[id].is_some() {
            return Err(value_error(format!("tool {id} is already in use")));
        }
        m.tools[id] = Some(name);
        Ok(Object::None)
    })
}

fn mon_free_tool_id(args: &[Object]) -> Result<Object, RuntimeError> {
    let id = pop_tool_id(args, "free_tool_id")?;
    with_monitoring(|m| {
        m.tools[id] = None;
        m.events[id] = 0;
        m.callbacks[id] = std::array::from_fn(|_| None);
        for per_tool in m.local_events.values_mut() {
            per_tool[id] = 0;
        }
        m.local_events
            .retain(|_, per_tool| per_tool.iter().any(|v| *v != 0));
        m.disabled.retain(|(_, _, tool, _)| *tool as usize != id);
        Ok(Object::None)
    })
}

fn mon_clear_tool_id(args: &[Object]) -> Result<Object, RuntimeError> {
    let id = pop_tool_id(args, "clear_tool_id")?;
    with_monitoring(|m| {
        m.events[id] = 0;
        m.callbacks[id] = std::array::from_fn(|_| None);
        for per_tool in m.local_events.values_mut() {
            per_tool[id] = 0;
        }
        m.disabled.retain(|(_, _, tool, _)| *tool as usize != id);
        Ok(Object::None)
    })
}

fn mon_get_tool(args: &[Object]) -> Result<Object, RuntimeError> {
    let id = pop_tool_id(args, "get_tool")?;
    with_monitoring(|m| match &m.tools[id] {
        Some(name) => Ok(Object::from_str(name.clone())),
        None => Ok(Object::None),
    })
}

fn mon_set_events(args: &[Object]) -> Result<Object, RuntimeError> {
    let id = pop_tool_id(args, "set_events")?;
    let mask = mask_arg(args, 1, "set_events")?;
    // C_RETURN / C_RAISE may only be set together with CALL (they
    // fire as CALL's completion events).
    let c_bits = crate::trace::event_mask(crate::trace::EVENT_C_RETURN)
        | crate::trace::event_mask(crate::trace::EVENT_C_RAISE);
    if mask & c_bits != 0 && mask & crate::trace::event_mask(crate::trace::EVENT_CALL) == 0 {
        return Err(value_error(
            "cannot set C_RETURN or C_RAISE events independently",
        ));
    }
    with_monitoring(|m| {
        if m.tools[id].is_none() {
            return Err(value_error(format!("tool {id} is not in use")));
        }
        m.events[id] = mask;
        Ok(Object::None)
    })
}

fn mon_get_events(args: &[Object]) -> Result<Object, RuntimeError> {
    let id = pop_tool_id(args, "get_events")?;
    with_monitoring(|m| Ok(Object::Int(i64::from(m.events[id]))))
}

fn mon_set_local_events(args: &[Object]) -> Result<Object, RuntimeError> {
    let id = pop_tool_id(args, "set_local_events")?;
    let code_id = code_id_arg(args, 1, "set_local_events")?;
    let mut mask = mask_arg(args, 2, "set_local_events")?;
    // C_RETURN/C_RAISE ride along with CALL: accepted when CALL is in
    // the set, then stripped before range validation (CPython
    // monitoring_set_local_events_impl).
    let c_ret = crate::trace::event_mask(crate::trace::EVENT_C_RETURN)
        | crate::trace::event_mask(crate::trace::EVENT_C_RAISE);
    if mask & c_ret != 0 {
        if mask & crate::trace::event_mask(crate::trace::EVENT_CALL) == 0 {
            return Err(value_error(
                "cannot set C_RETURN or C_RAISE events independently",
            ));
        }
        mask &= !c_ret;
    }
    if mask & !crate::trace::LOCAL_EVENTS_MASK != 0 {
        return Err(value_error(format!("invalid local event set 0x{mask:x}")));
    }
    with_monitoring(|m| {
        if m.tools[id].is_none() {
            return Err(value_error(format!("tool {id} is not in use")));
        }
        let per_tool = m.local_events.entry(code_id).or_insert([0; 6]);
        per_tool[id] = mask;
        if mask == 0 {
            let empty = m
                .local_events
                .get(&code_id)
                .is_some_and(|p| p.iter().all(|v| *v == 0));
            if empty {
                m.local_events.remove(&code_id);
            }
        }
        Ok(Object::None)
    })
}

fn mon_get_local_events(args: &[Object]) -> Result<Object, RuntimeError> {
    let id = pop_tool_id(args, "get_local_events")?;
    let code_id = code_id_arg(args, 1, "get_local_events")?;
    with_monitoring(|m| {
        let mask = m.local_events.get(&code_id).map_or(0, |p| p[id]);
        Ok(Object::Int(i64::from(mask)))
    })
}

fn mon_register_callback(args: &[Object]) -> Result<Object, RuntimeError> {
    let id = pop_tool_id(args, "register_callback")?;
    let event = match args.get(1) {
        Some(Object::Int(i)) => *i,
        _ => return Err(type_error("register_callback: event must be int")),
    };
    let callback = args.get(2).cloned().unwrap_or(Object::None);
    // PEP 578: audits the callback object (test_audit
    // test_sys_monitoring_register_callback expects `(None,)`).
    crate::stdlib::sys::audit_event(
        "sys.monitoring.register_callback",
        std::slice::from_ref(&callback),
    )?;
    if event <= 0
        || (event as u64).count_ones() != 1
        || event > i64::from(crate::trace::ALL_EVENTS_MASK)
    {
        return Err(value_error(
            "The callback can only be set for one event at a time",
        ));
    }
    let event_index = (event as u64).trailing_zeros() as usize;
    with_monitoring(|m| {
        let prior = m.callbacks[id][event_index].clone().unwrap_or(Object::None);
        m.callbacks[id][event_index] = match callback {
            Object::None => None,
            other => Some(other),
        };
        Ok(prior)
    })
}

fn mon_restart_events(_args: &[Object]) -> Result<Object, RuntimeError> {
    // Re-arm every location a callback disabled by returning
    // `sys.monitoring.DISABLE`.
    with_monitoring(|m| {
        m.disabled.clear();
        Ok(Object::None)
    })
}

/// `sys.monitoring._all_events()` — `{event_name: bitmask_of_tools}`
/// for every event with at least one subscribed tool (global masks
/// only, matching CPython's instrumentation summary).
fn mon_all_events(_args: &[Object]) -> Result<Object, RuntimeError> {
    let mut out = DictData::default();
    with_monitoring(|m| {
        for &(name, idx) in EVENT_NAMES {
            let bit = crate::trace::event_mask(idx);
            let mut tools_mask: i64 = 0;
            for tool in 0..6 {
                if m.events[tool] & bit != 0 {
                    tools_mask |= 1 << tool;
                }
            }
            if tools_mask != 0 {
                out.insert(DictKey(Object::from_static(name)), Object::Int(tools_mask));
            }
        }
    });
    Ok(Object::Dict(Rc::new(RefCell::new(out))))
}

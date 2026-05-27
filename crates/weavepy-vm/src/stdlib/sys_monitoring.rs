//! PEP 669 — `sys.monitoring` skeleton.
//!
//! Implements the user-facing API surface so debuggers, coverage tools,
//! and profilers can register tool IDs and event callbacks without
//! crashing. Event firing inside the interpreter dispatch loop is
//! gated behind RFC 0031; for now `sys.monitoring.events.*` constants,
//! `use_tool_id`, `set_events`, `register_callback`, `get_events`,
//! `get_tool`, and the helper constants (`DISABLE`, `MISSING`) all
//! behave correctly and are observable through `sys.monitoring`'s
//! introspection.
//!
//! The persistent state lives in [`crate::trace::MonitoringTools`]
//! so it's thread-local and shared with `sys.gettrace` /
//! `sys.getprofile`.

use crate::error::{type_error, value_error, RuntimeError};
use crate::object::{BuiltinFn, DictData, DictKey, Object, PyModule};
use crate::sync::{Rc, RefCell};
use crate::trace::with_monitoring;

pub fn build() -> Object {
    let dict = Rc::new(RefCell::new(DictData::new()));
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

        // Sentinels.
        d.insert(
            DictKey(Object::from_static("DISABLE")),
            Object::from_static("DISABLE"),
        );
        d.insert(
            DictKey(Object::from_static("MISSING")),
            Object::from_static("MISSING"),
        );

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
            builtin("restart_events", |_| Ok(Object::None)),
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
        call: Box::new(body),
        call_kw: None,
    }))
}

fn build_events_namespace() -> Object {
    let names: &[(&str, u32)] = &[
        ("NO_EVENTS", 0),
        ("BRANCH", 1 << 0),
        ("CALL", 1 << 1),
        ("C_RAISE", 1 << 2),
        ("C_RETURN", 1 << 3),
        ("EXCEPTION_HANDLED", 1 << 4),
        ("INSTRUCTION", 1 << 5),
        ("JUMP", 1 << 6),
        ("LINE", 1 << 7),
        ("PY_RESUME", 1 << 8),
        ("PY_RETURN", 1 << 9),
        ("PY_START", 1 << 10),
        ("PY_THROW", 1 << 11),
        ("PY_UNWIND", 1 << 12),
        ("PY_YIELD", 1 << 13),
        ("RAISE", 1 << 14),
        ("RERAISE", 1 << 15),
        ("STOP_ITERATION", 1 << 16),
    ];
    let mut ns = DictData::new();
    for (name, value) in names {
        ns.insert(
            DictKey(Object::from_str((*name).to_string())),
            Object::Int(i64::from(*value)),
        );
    }
    Object::SimpleNamespace(Rc::new(RefCell::new(ns)))
}

fn pop_tool_id(args: &[Object], func: &str) -> Result<usize, RuntimeError> {
    match args.first() {
        Some(Object::Int(i)) => {
            if *i < 0 || *i >= 6 {
                Err(value_error(format!(
                    "{func}: tool id must be in 0..6, got {i}"
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
            return Err(value_error(format!("tool id {id} is already in use")));
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
    let mask = match args.get(1) {
        Some(Object::Int(i)) => *i as u32,
        _ => return Err(type_error("set_events: mask must be int")),
    };
    with_monitoring(|m| {
        m.events[id] = mask;
        Ok(Object::None)
    })
}

fn mon_get_events(args: &[Object]) -> Result<Object, RuntimeError> {
    let id = pop_tool_id(args, "get_events")?;
    with_monitoring(|m| Ok(Object::Int(i64::from(m.events[id]))))
}

fn mon_set_local_events(args: &[Object]) -> Result<Object, RuntimeError> {
    // Local events apply to a specific code object; without per-code
    // storage we lump them in with global events.
    let _code = args.first();
    mon_set_events(&args[1..])
}

fn mon_get_local_events(args: &[Object]) -> Result<Object, RuntimeError> {
    let _code = args.first();
    mon_get_events(&args[1..])
}

fn mon_register_callback(args: &[Object]) -> Result<Object, RuntimeError> {
    let id = pop_tool_id(args, "register_callback")?;
    let event = match args.get(1) {
        Some(Object::Int(i)) => *i as u32,
        _ => return Err(type_error("register_callback: event must be int")),
    };
    let callback = args.get(2).cloned().unwrap_or(Object::None);
    let event_index = event.trailing_zeros() as usize;
    if event_index >= 32 {
        return Err(value_error("register_callback: event mask invalid"));
    }
    with_monitoring(|m| {
        let prior = m.callbacks[id][event_index].clone().unwrap_or(Object::None);
        m.callbacks[id][event_index] = match callback {
            Object::None => None,
            other => Some(other),
        };
        Ok(prior)
    })
}

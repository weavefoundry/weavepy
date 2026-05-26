//! The `_contextvars` module — RFC 0023.
//!
//! Provides `ContextVar`, `Context`, `Token`, `copy_context`. The
//! Python wrapper `contextvars` re-exports these. Implementation
//! uses a per-thread stack of "currently active" Context objects;
//! ContextVar reads return the most recent binding visible through
//! the stack.

use crate::sync::Rc;
use crate::sync::RefCell;

use crate::error::{type_error, RuntimeError};
use crate::import::ModuleCache;
use crate::object::{BuiltinFn, DictData, DictKey, Object, PyModule};
use crate::types::{PyInstance, TypeFlags, TypeObject};

thread_local! {
    static CONTEXT_STACK: RefCell<Vec<Rc<RefCell<DictData>>>> =
        RefCell::new(vec![Rc::new(RefCell::new(DictData::new()))]);
}

pub fn build(_cache: &ModuleCache) -> Rc<PyModule> {
    let dict = Rc::new(RefCell::new(DictData::new()));
    {
        let mut d = dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("__name__")),
            Object::from_static("_contextvars"),
        );
        d.insert(
            DictKey(Object::from_static("ContextVar")),
            Object::Type(contextvar_type()),
        );
        d.insert(
            DictKey(Object::from_static("Context")),
            Object::Type(context_type()),
        );
        d.insert(
            DictKey(Object::from_static("Token")),
            Object::Type(token_type()),
        );
        d.insert(
            DictKey(Object::from_static("copy_context")),
            builtin("copy_context", cv_copy_context),
        );
    }
    Rc::new(PyModule {
        name: "_contextvars".to_owned(),
        filename: None,
        dict,
    })
}

fn builtin(name: &'static str, body: fn(&[Object]) -> Result<Object, RuntimeError>) -> Object {
    Object::Builtin(Rc::new(BuiltinFn {
        name,
        call: Box::new(body),
        call_kw: None,
    }))
}

fn current_context() -> Rc<RefCell<DictData>> {
    CONTEXT_STACK.with(|s| {
        s.borrow()
            .last()
            .cloned()
            .unwrap_or_else(|| Rc::new(RefCell::new(DictData::new())))
    })
}

fn contextvar_type() -> Rc<TypeObject> {
    use crate::builtin_types::builtin_types;
    let bt = builtin_types();
    let mut td = DictData::new();
    for (name, fn_) in [
        (
            "__init__",
            cv_init as fn(&[Object]) -> Result<Object, RuntimeError>,
        ),
        ("get", cv_get),
        ("set", cv_set),
        ("reset", cv_reset),
    ] {
        td.insert(
            DictKey(Object::from_static(name)),
            Object::Builtin(Rc::new(BuiltinFn {
                name,
                call: Box::new(fn_),
                call_kw: None,
            })),
        );
    }
    TypeObject::new_with_flags(
        "ContextVar",
        vec![bt.object_.clone()],
        td,
        TypeFlags {
            is_exception: false,
            is_builtin: true,
        },
    )
    .expect("ContextVar")
}

fn context_type() -> Rc<TypeObject> {
    use crate::builtin_types::builtin_types;
    let bt = builtin_types();
    let mut td = DictData::new();
    for (name, fn_) in [
        (
            "__init__",
            ctx_init as fn(&[Object]) -> Result<Object, RuntimeError>,
        ),
        ("run", ctx_run),
        ("copy", ctx_copy),
        ("get", ctx_get),
        ("keys", ctx_keys),
        ("values", ctx_values),
        ("items", ctx_items),
        ("__contains__", ctx_contains),
        ("__len__", ctx_len),
        ("__iter__", ctx_iter),
    ] {
        td.insert(
            DictKey(Object::from_static(name)),
            Object::Builtin(Rc::new(BuiltinFn {
                name,
                call: Box::new(fn_),
                call_kw: None,
            })),
        );
    }
    TypeObject::new_with_flags(
        "Context",
        vec![bt.object_.clone()],
        td,
        TypeFlags {
            is_exception: false,
            is_builtin: true,
        },
    )
    .expect("Context")
}

fn token_type() -> Rc<TypeObject> {
    use crate::builtin_types::builtin_types;
    let bt = builtin_types();
    TypeObject::new_with_flags(
        "Token",
        vec![bt.object_.clone()],
        DictData::new(),
        TypeFlags {
            is_exception: false,
            is_builtin: true,
        },
    )
    .expect("Token")
}

fn cv_init(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = match args.first() {
        Some(Object::Instance(i)) => i.clone(),
        _ => return Err(type_error("ContextVar.__init__ missing self")),
    };
    let name = args.get(1).cloned().unwrap_or(Object::from_static(""));
    let default = args.get(2).cloned().unwrap_or(Object::None);
    inst.dict
        .borrow_mut()
        .insert(DictKey(Object::from_static("name")), name);
    inst.dict
        .borrow_mut()
        .insert(DictKey(Object::from_static("_default")), default);
    Ok(Object::None)
}

fn cv_get(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = match args.first() {
        Some(Object::Instance(i)) => i.clone(),
        _ => return Err(type_error("ContextVar.get missing self")),
    };
    let key = DictKey(Object::Instance(inst.clone()));
    let ctx = current_context();
    if let Some(v) = ctx.borrow().get(&key).cloned() {
        return Ok(v);
    }
    if let Some(default) = args.get(1).cloned() {
        return Ok(default);
    }
    let d = inst
        .dict
        .borrow()
        .get(&DictKey(Object::from_static("_default")))
        .cloned()
        .unwrap_or(Object::None);
    if matches!(d, Object::None)
        && !inst
            .dict
            .borrow()
            .contains_key(&DictKey(Object::from_static("_default")))
    {
        return Err(crate::error::runtime_error(
            "<ContextVar>: no value".to_owned(),
        ));
    }
    Ok(d)
}

fn cv_set(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = match args.first() {
        Some(Object::Instance(i)) => i.clone(),
        _ => return Err(type_error("ContextVar.set missing self")),
    };
    let value = args.get(1).cloned().unwrap_or(Object::None);
    let ctx = current_context();
    let key = DictKey(Object::Instance(inst.clone()));
    let prev = ctx.borrow().get(&key).cloned();
    ctx.borrow_mut().insert(key, value);
    // Return a token: a SimpleNamespace recording (var, prev).
    let mut d = DictData::new();
    d.insert(
        DictKey(Object::from_static("var")),
        Object::Instance(inst.clone()),
    );
    d.insert(
        DictKey(Object::from_static("_prev")),
        prev.unwrap_or(Object::None),
    );
    Ok(Object::SimpleNamespace(Rc::new(RefCell::new(d))))
}

fn cv_reset(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = match args.first() {
        Some(Object::Instance(i)) => i.clone(),
        _ => return Err(type_error("ContextVar.reset missing self")),
    };
    let token = match args.get(1) {
        Some(Object::SimpleNamespace(d)) => d.clone(),
        _ => return Err(type_error("reset(): missing Token")),
    };
    let prev = token
        .borrow()
        .get(&DictKey(Object::from_static("_prev")))
        .cloned()
        .unwrap_or(Object::None);
    let ctx = current_context();
    let key = DictKey(Object::Instance(inst));
    if matches!(prev, Object::None) {
        ctx.borrow_mut().shift_remove(&key);
    } else {
        ctx.borrow_mut().insert(key, prev);
    }
    Ok(Object::None)
}

fn ctx_init(_args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(Object::None)
}

fn ctx_run(args: &[Object]) -> Result<Object, RuntimeError> {
    // The Python wrapper handles execution under a swapped context.
    // We can't drive arbitrary call-with-state from here, so return
    // the callable unchanged for the wrapper to invoke. The wrapper
    // sets the context manually around the call.
    Ok(args.get(1).cloned().unwrap_or(Object::None))
}

fn ctx_copy(_args: &[Object]) -> Result<Object, RuntimeError> {
    let cur = current_context();
    let cloned: DictData = cur.borrow().clone();
    let new_inst = PyInstance::new(context_type());
    new_inst.dict.borrow_mut().insert(
        DictKey(Object::from_static("_data")),
        Object::Dict(Rc::new(RefCell::new(cloned))),
    );
    Ok(Object::Instance(Rc::new(new_inst)))
}

fn ctx_get(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = match args.first() {
        Some(Object::Instance(i)) => i.clone(),
        _ => return Err(type_error("Context.get missing self")),
    };
    let key = args
        .get(1)
        .cloned()
        .ok_or_else(|| type_error("Context.get missing key"))?;
    let data = inst
        .dict
        .borrow()
        .get(&DictKey(Object::from_static("_data")))
        .cloned();
    if let Some(Object::Dict(d)) = data {
        if let Some(v) = d.borrow().get(&DictKey(key)).cloned() {
            return Ok(v);
        }
    }
    Ok(args.get(2).cloned().unwrap_or(Object::None))
}

fn ctx_keys(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = match args.first() {
        Some(Object::Instance(i)) => i.clone(),
        _ => return Err(type_error("Context.keys missing self")),
    };
    if let Some(Object::Dict(d)) = inst
        .dict
        .borrow()
        .get(&DictKey(Object::from_static("_data")))
        .cloned()
    {
        let keys: Vec<Object> = d.borrow().keys().map(|k| k.0.clone()).collect();
        return Ok(Object::new_list(keys));
    }
    Ok(Object::new_list(vec![]))
}

fn ctx_values(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = match args.first() {
        Some(Object::Instance(i)) => i.clone(),
        _ => return Err(type_error("Context.values missing self")),
    };
    if let Some(Object::Dict(d)) = inst
        .dict
        .borrow()
        .get(&DictKey(Object::from_static("_data")))
        .cloned()
    {
        let vs: Vec<Object> = d.borrow().values().cloned().collect();
        return Ok(Object::new_list(vs));
    }
    Ok(Object::new_list(vec![]))
}

fn ctx_items(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = match args.first() {
        Some(Object::Instance(i)) => i.clone(),
        _ => return Err(type_error("Context.items missing self")),
    };
    if let Some(Object::Dict(d)) = inst
        .dict
        .borrow()
        .get(&DictKey(Object::from_static("_data")))
        .cloned()
    {
        let items: Vec<Object> = d
            .borrow()
            .iter()
            .map(|(k, v)| Object::new_tuple(vec![k.0.clone(), v.clone()]))
            .collect();
        return Ok(Object::new_list(items));
    }
    Ok(Object::new_list(vec![]))
}

fn ctx_contains(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = match args.first() {
        Some(Object::Instance(i)) => i.clone(),
        _ => return Err(type_error("Context.__contains__ missing self")),
    };
    let key = args.get(1).cloned().unwrap_or(Object::None);
    if let Some(Object::Dict(d)) = inst
        .dict
        .borrow()
        .get(&DictKey(Object::from_static("_data")))
        .cloned()
    {
        return Ok(Object::Bool(d.borrow().contains_key(&DictKey(key))));
    }
    Ok(Object::Bool(false))
}

fn ctx_len(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = match args.first() {
        Some(Object::Instance(i)) => i.clone(),
        _ => return Err(type_error("Context.__len__ missing self")),
    };
    if let Some(Object::Dict(d)) = inst
        .dict
        .borrow()
        .get(&DictKey(Object::from_static("_data")))
        .cloned()
    {
        return Ok(Object::Int(d.borrow().len() as i64));
    }
    Ok(Object::Int(0))
}

fn ctx_iter(args: &[Object]) -> Result<Object, RuntimeError> {
    ctx_keys(args).and_then(|keys| {
        keys.make_iter()
            .map(|it| Object::Iter(Rc::new(RefCell::new(it))))
    })
}

fn cv_copy_context(_args: &[Object]) -> Result<Object, RuntimeError> {
    ctx_copy(&[])
}

/// Push a fresh copy of the current context onto the stack. Used by
/// `Context.run` from Python.
pub fn push_context(data: Rc<RefCell<DictData>>) {
    CONTEXT_STACK.with(|s| s.borrow_mut().push(data));
}

/// Pop the top context. No-op if only the root is left.
pub fn pop_context() {
    CONTEXT_STACK.with(|s| {
        let mut g = s.borrow_mut();
        if g.len() > 1 {
            g.pop();
        }
    });
}

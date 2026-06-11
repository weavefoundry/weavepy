//! The `_functools` built-in module — Rust core for `functools`.
//!
//! CPython implements `functools.partial` in C, so calling a partial
//! pushes no Python frame and leaves no traceback entry. The frozen
//! Python `partial` class delegates `__call__` here to match that:
//! `test_traceback` asserts that a `partial(exec, …)` call site shows
//! only the caller's frame.

use crate::sync::Rc;
use crate::sync::RefCell;

use crate::error::{type_error, RuntimeError};
use crate::import::ModuleCache;
use crate::object::{BuiltinFn, DictData, DictKey, Object, PyModule};

pub fn build(_cache: &ModuleCache) -> Rc<PyModule> {
    let dict = Rc::new(RefCell::new(DictData::new()));
    {
        let mut d = dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("__name__")),
            Object::from_static("_functools"),
        );
        d.insert(
            DictKey(Object::from_static("__doc__")),
            Object::from_static("Tools that operate on functions — native core."),
        );
        d.insert(
            DictKey(Object::from_static("_partial_call")),
            Object::Builtin(Rc::new(BuiltinFn {
                name: "__call__",
                call: Box::new(|args| partial_call(args, &[])),
                call_kw: Some(Box::new(partial_call)),
            })),
        );
    }
    Rc::new(PyModule {
        name: "_functools".to_owned(),
        filename: None,
        dict,
    })
}

/// `partial.__call__(self, /, *args, **keywords)` without a Python
/// frame: merge stored args/keywords with the call's and tail-call
/// `self.func` through the interpreter.
fn partial_call(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let Some(ptr) = crate::vm_singletons::current_interpreter_ptr() else {
        return Err(type_error("partial.__call__ requires a running interpreter"));
    };
    // SAFETY: published by the enclosing VM frame on this thread.
    let interp = unsafe { &mut *ptr };
    let slf = args
        .first()
        .ok_or_else(|| type_error("descriptor '__call__' of 'functools.partial' object needs an argument"))?;
    let func = interp.load_attr_public(slf, "func")?;
    let stored_args = interp.load_attr_public(slf, "args")?;
    let stored_kw = interp.load_attr_public(slf, "keywords")?;

    let mut call_args: Vec<Object> = match &stored_args {
        Object::Tuple(xs) => xs.to_vec(),
        _ => return Err(type_error("partial 'args' must be a tuple")),
    };
    call_args.extend_from_slice(&args[1..]);

    let mut call_kwargs: Vec<(String, Object)> = Vec::new();
    if let Object::Dict(d) = &stored_kw {
        for (k, v) in d.borrow().iter() {
            if let Object::Str(s) = &k.0 {
                call_kwargs.push((s.to_string(), v.clone()));
            }
        }
    }
    // Call-site keywords override stored ones (`{**self.keywords, **keywords}`).
    for (k, v) in kwargs {
        if let Some(slot) = call_kwargs.iter_mut().find(|(name, _)| name == k) {
            slot.1 = v.clone();
        } else {
            call_kwargs.push((k.clone(), v.clone()));
        }
    }

    let globals = interp.builtins_dict();
    interp.call(&func, &call_args, &call_kwargs, &globals)
}

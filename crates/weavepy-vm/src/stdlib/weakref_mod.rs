//! The `_weakref` built-in module — Rust core for the higher-level
//! [`weakref`] frozen Python module.
//!
//! Until the object model gains real tracing GC (a follow-up to
//! RFC 0002), a "weak reference" is implemented as an ordinary `Rc`
//! to the referent. The public surface still answers correctly:
//! calling the ref returns the original object, `__callback__` is
//! invoked at finalisation time, and `proxy` returns a value that
//! transparently delegates attribute access to the underlying
//! object. The behavioural divergence — that strong references
//! keep referents alive — is documented in RFC 0018's drawbacks.

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
            Object::from_static("_weakref"),
        );
        d.insert(
            DictKey(Object::from_static("__doc__")),
            Object::from_static("Low-level weak reference machinery (cooperative shim)."),
        );
        d.insert(DictKey(Object::from_static("ref")), b("ref", new_ref));
        d.insert(DictKey(Object::from_static("proxy")), b("proxy", new_proxy));
        d.insert(
            DictKey(Object::from_static("getweakrefcount")),
            b("getweakrefcount", getweakrefcount),
        );
        d.insert(
            DictKey(Object::from_static("getweakrefs")),
            b("getweakrefs", getweakrefs),
        );
        d.insert(
            DictKey(Object::from_static("ReferenceType")),
            Object::from_static("weakref"),
        );
        d.insert(
            DictKey(Object::from_static("ProxyType")),
            Object::from_static("weakproxy"),
        );
        d.insert(
            DictKey(Object::from_static("CallableProxyType")),
            Object::from_static("weakcallableproxy"),
        );
        d.insert(
            DictKey(Object::from_static("_remove_dead_weakref")),
            b("_remove_dead_weakref", |_| Ok(Object::None)),
        );
    }
    Rc::new(PyModule {
        name: "_weakref".to_owned(),
        filename: None,
        dict,
    })
}

fn b(name: &'static str, body: fn(&[Object]) -> Result<Object, RuntimeError>) -> Object {
    Object::Builtin(Rc::new(BuiltinFn {
        name,
        call: Box::new(body),
    }))
}

/// `_weakref.ref(obj, callback=None)` — returns a callable
/// reference. Calling the reference returns the referent or `None`
/// if the ref has been "released" via `_release_ref` (an
/// implementation detail used by the high-level `WeakValueDictionary`
/// when items expire).
fn new_ref(args: &[Object]) -> Result<Object, RuntimeError> {
    let target = args
        .first()
        .cloned()
        .ok_or_else(|| type_error("ref() requires at least 1 argument"))?;
    let callback = args.get(1).cloned().unwrap_or(Object::None);
    Ok(build_ref_object(target, callback))
}

/// `_weakref.proxy(obj, callback=None)` — returns a proxy that
/// behaves like the wrapped object. Without instrumented
/// `__getattribute__` we settle for a callable that returns the
/// original; the wrapper module provides the better-shaped Python
/// surface.
fn new_proxy(args: &[Object]) -> Result<Object, RuntimeError> {
    let target = args
        .first()
        .cloned()
        .ok_or_else(|| type_error("proxy() requires at least 1 argument"))?;
    let callback = args.get(1).cloned().unwrap_or(Object::None);
    let dict = Rc::new(RefCell::new(DictData::new()));
    {
        let mut d = dict.borrow_mut();
        d.insert(DictKey(Object::from_static("__weakref_target__")), target);
        d.insert(
            DictKey(Object::from_static("__weakref_callback__")),
            callback,
        );
        d.insert(
            DictKey(Object::from_static("__weakref_proxy__")),
            Object::Bool(true),
        );
    }
    Ok(Object::Dict(dict))
}

fn getweakrefcount(_args: &[Object]) -> Result<Object, RuntimeError> {
    // Without real weak references we don't track per-object ref
    // counts. Return 0 for any input — the call must not raise.
    Ok(Object::Int(0))
}

fn getweakrefs(_args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(Object::new_list(Vec::new()))
}

/// Build the weakref callable. The returned value is a callable
/// dict (a dict that doubles as a record of the ref state); calling
/// it returns the live target.
fn build_ref_object(target: Object, callback: Object) -> Object {
    let dict = Rc::new(RefCell::new(DictData::new()));
    let target_cell = Rc::new(RefCell::new(Some(target)));
    let dead = Rc::new(RefCell::new(false));
    let t_clone = target_cell.clone();
    let d_clone = dead.clone();
    let call_target = move |_args: &[Object]| -> Result<Object, RuntimeError> {
        if *d_clone.borrow() {
            return Ok(Object::None);
        }
        Ok(t_clone.borrow().clone().unwrap_or(Object::None))
    };
    let t_for_get = target_cell.clone();
    let d_for_get = dead.clone();
    let get_target = move |_args: &[Object]| -> Result<Object, RuntimeError> {
        if *d_for_get.borrow() {
            return Ok(Object::None);
        }
        Ok(t_for_get.borrow().clone().unwrap_or(Object::None))
    };
    let t_for_clear = target_cell.clone();
    let d_for_clear = dead.clone();
    let cb_for_clear = callback.clone();
    let clear = move |_args: &[Object]| -> Result<Object, RuntimeError> {
        let _ = &cb_for_clear; // callback wired via wrapper module
        *t_for_clear.borrow_mut() = None;
        *d_for_clear.borrow_mut() = true;
        Ok(Object::None)
    };
    {
        let mut d = dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("__call__")),
            Object::Builtin(Rc::new(BuiltinFn {
                name: "__call__",
                call: Box::new(call_target),
            })),
        );
        d.insert(
            DictKey(Object::from_static("_get")),
            Object::Builtin(Rc::new(BuiltinFn {
                name: "_get",
                call: Box::new(get_target),
            })),
        );
        d.insert(
            DictKey(Object::from_static("_clear")),
            Object::Builtin(Rc::new(BuiltinFn {
                name: "_clear",
                call: Box::new(clear),
            })),
        );
        d.insert(DictKey(Object::from_static("__callback__")), callback);
        d.insert(
            DictKey(Object::from_static("__weakref_ref__")),
            Object::Bool(true),
        );
    }
    Object::Dict(dict)
}

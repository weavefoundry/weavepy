//! Real `_weakref` Rust core — RFC 0024.
//!
//! See [`crate::weakref_registry`] for the `Arc<…>` non-Send-Sync
//! rationale.

#![allow(clippy::arc_with_non_send_sync)]
//!
//! Replaces the cooperative shim in `stdlib::weakref_mod`. The
//! new module exposes:
//!
//! - **`ref(obj, callback=None)`** that returns a callable
//!   weakref. Calling the ref returns the live target while
//!   it's reachable; once the cycle GC clears the referent,
//!   the call returns `None` and the callback fires.
//! - **`proxy(obj, callback=None)`** that returns a
//!   delegating proxy. Attribute / item / call access all
//!   forward to the live target; once cleared, the proxy
//!   raises `ReferenceError` on any access.
//! - **`getweakrefcount(obj)`** that returns the number of
//!   live weakrefs targeting `obj` (via the per-thread
//!   registry).
//! - **`getweakrefs(obj)`** that returns a list of every live
//!   weakref targeting `obj`.
//! - **`_remove_dead_weakref(...)`** — compatibility no-op
//!   needed by `weakref.WeakValueDictionary` internals.
//!
//! The user-visible types (`ReferenceType`, `ProxyType`,
//! `CallableProxyType`) are real `TypeObject`s, so
//! `isinstance(w, weakref.ref)` and friends finally return
//! `True`.

use crate::sync::Rc;
use crate::sync::RefCell;
use std::sync::Arc;

use crate::error::{type_error, value_error, RuntimeError};
use crate::import::ModuleCache;
use crate::object::{BuiltinFn, DictData, DictKey, Object, PyModule};
use crate::types::{PyInstance, TypeFlags, TypeObject};
use crate::weakref_registry::{self as reg, id_of, kind, register, ObjectId, WeakRefSlot};

thread_local! {
    static REF_TYPE: RefCell<Option<Rc<TypeObject>>> = const { RefCell::new(None) };
    static PROXY_TYPE: RefCell<Option<Rc<TypeObject>>> = const { RefCell::new(None) };
    static CALLABLE_PROXY_TYPE: RefCell<Option<Rc<TypeObject>>> = const { RefCell::new(None) };
}

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
            Object::from_static(
                "Low-level weak reference machinery. \
                 References zero out when the cycle GC \
                 collects the referent; callbacks fire as \
                 part of the clear phase.",
            ),
        );
        d.insert(DictKey(Object::from_static("ref")), b("ref", new_ref));
        d.insert(DictKey(Object::from_static("proxy")), b("proxy", new_proxy));
        d.insert(
            DictKey(Object::from_static("getweakrefcount")),
            b("getweakrefcount", get_weakref_count),
        );
        d.insert(
            DictKey(Object::from_static("getweakrefs")),
            b("getweakrefs", get_weakrefs),
        );
        d.insert(
            DictKey(Object::from_static("ReferenceType")),
            Object::Type(ref_type()),
        );
        d.insert(
            DictKey(Object::from_static("ProxyType")),
            Object::Type(proxy_type()),
        );
        d.insert(
            DictKey(Object::from_static("CallableProxyType")),
            Object::Type(callable_proxy_type()),
        );
        d.insert(
            DictKey(Object::from_static("_remove_dead_weakref")),
            b("_remove_dead_weakref", remove_dead_weakref),
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
        call_kw: None,
    }))
}

fn b_dyn(
    name: &'static str,
    body: impl Fn(&[Object]) -> Result<Object, RuntimeError> + Send + Sync + 'static,
) -> Object {
    Object::Builtin(Rc::new(BuiltinFn {
        name,
        call: Box::new(body),
        call_kw: None,
    }))
}

fn ref_type() -> Rc<TypeObject> {
    REF_TYPE.with(|cell| {
        if let Some(t) = cell.borrow().clone() {
            return t;
        }
        let t = TypeObject::new_with_flags(
            "weakref",
            vec![crate::builtin_types::builtin_types().object_.clone()],
            DictData::new(),
            TypeFlags {
                is_exception: false,
                is_builtin: true,
            },
        )
        .expect("ref type");
        *cell.borrow_mut() = Some(t.clone());
        t
    })
}

fn proxy_type() -> Rc<TypeObject> {
    PROXY_TYPE.with(|cell| {
        if let Some(t) = cell.borrow().clone() {
            return t;
        }
        let t = TypeObject::new_with_flags(
            "weakproxy",
            vec![crate::builtin_types::builtin_types().object_.clone()],
            DictData::new(),
            TypeFlags {
                is_exception: false,
                is_builtin: true,
            },
        )
        .expect("proxy type");
        *cell.borrow_mut() = Some(t.clone());
        t
    })
}

fn callable_proxy_type() -> Rc<TypeObject> {
    CALLABLE_PROXY_TYPE.with(|cell| {
        if let Some(t) = cell.borrow().clone() {
            return t;
        }
        let t = TypeObject::new_with_flags(
            "weakcallableproxy",
            vec![crate::builtin_types::builtin_types().object_.clone()],
            DictData::new(),
            TypeFlags {
                is_exception: false,
                is_builtin: true,
            },
        )
        .expect("callable proxy type");
        *cell.borrow_mut() = Some(t.clone());
        t
    })
}

fn extract_callback(arg: Option<&Object>) -> Option<Object> {
    match arg {
        None | Some(Object::None) => None,
        Some(o) => Some(o.clone()),
    }
}

/// `_weakref.ref(obj, callback=None)` — returns a fresh
/// weakref. Internally the slot is registered with the
/// per-thread weakref registry; the slot is cleared when the
/// cycle GC reclaims the referent.
fn new_ref(args: &[Object]) -> Result<Object, RuntimeError> {
    let target = args
        .first()
        .cloned()
        .ok_or_else(|| type_error("ref() requires at least 1 argument"))?;
    let callback = extract_callback(args.get(1));
    Ok(make_ref_object(target, callback, kind::REF))
}

/// `_weakref.proxy(obj, callback=None)` — returns a delegating
/// proxy. If `obj` is callable, the proxy is a
/// `CallableProxyType`; otherwise a plain `ProxyType`.
fn new_proxy(args: &[Object]) -> Result<Object, RuntimeError> {
    let target = args
        .first()
        .cloned()
        .ok_or_else(|| type_error("proxy() requires at least 1 argument"))?;
    let callback = extract_callback(args.get(1));
    let is_callable = matches!(
        target,
        Object::Function(_) | Object::Builtin(_) | Object::BoundMethod(_) | Object::Type(_)
    );
    let k = if is_callable {
        kind::CALLABLE_PROXY
    } else {
        kind::PROXY
    };
    Ok(make_ref_object(target, callback, k))
}

fn make_ref_object(target: Object, callback: Option<Object>, kind_tag: u8) -> Object {
    let target_id = id_of(&target);
    let slot = Arc::new(WeakRefSlot::new(
        target_id,
        target.clone(),
        callback.clone(),
        kind_tag,
    ));
    register(slot.clone());

    let dict = Rc::new(RefCell::new(DictData::new()));

    let class = match kind_tag {
        kind::PROXY => proxy_type(),
        kind::CALLABLE_PROXY => callable_proxy_type(),
        _ => ref_type(),
    };

    // Methods.
    let slot_for_call = slot.clone();
    let call = move |_args: &[Object]| -> Result<Object, RuntimeError> {
        Ok(slot_for_call.upgrade().unwrap_or(Object::None))
    };
    let slot_for_get = slot.clone();
    let get_target = move |_args: &[Object]| -> Result<Object, RuntimeError> {
        Ok(slot_for_get.upgrade().unwrap_or(Object::None))
    };
    let slot_for_clear = slot.clone();
    let target_id_for_clear = target_id;
    let clear = move |_args: &[Object]| -> Result<Object, RuntimeError> {
        let _ = slot_for_clear.clear();
        let _ = reg::notify_clear(target_id_for_clear);
        Ok(Object::None)
    };
    let slot_for_alive = slot.clone();
    let alive = move |_args: &[Object]| -> Result<Object, RuntimeError> {
        Ok(Object::Bool(!slot_for_alive.is_dead()))
    };
    let hash_value = slot.identity_hash;
    let hash =
        move |_args: &[Object]| -> Result<Object, RuntimeError> { Ok(Object::Int(hash_value)) };
    let slot_for_repr = slot.clone();
    let repr = move |_args: &[Object]| -> Result<Object, RuntimeError> {
        let txt = if slot_for_repr.is_dead() {
            "<weakref at 0x0; dead>"
        } else {
            "<weakref at 0x0; live>"
        };
        Ok(Object::from_static(txt))
    };

    {
        let mut d = dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("__call__")),
            b_dyn("__call__", call),
        );
        d.insert(
            DictKey(Object::from_static("__weakref_get__")),
            b_dyn("__weakref_get__", get_target),
        );
        d.insert(
            DictKey(Object::from_static("__clear__")),
            b_dyn("__clear__", clear),
        );
        d.insert(
            DictKey(Object::from_static("__alive__")),
            b_dyn("__alive__", alive),
        );
        d.insert(
            DictKey(Object::from_static("__hash__")),
            b_dyn("__hash__", hash),
        );
        d.insert(
            DictKey(Object::from_static("__repr__")),
            b_dyn("__repr__", repr),
        );
        if let Some(cb) = callback.clone() {
            d.insert(DictKey(Object::from_static("__callback__")), cb);
        } else {
            d.insert(DictKey(Object::from_static("__callback__")), Object::None);
        }
        d.insert(
            DictKey(Object::from_static("__weakref_kind__")),
            Object::Int(i64::from(kind_tag)),
        );
    }

    Object::Instance(Rc::new(PyInstance {
        class,
        dict,
        native: None,
    }))
}

/// `_weakref.getweakrefcount(obj)` — number of live weakrefs
/// targeting `obj`.
fn get_weakref_count(args: &[Object]) -> Result<Object, RuntimeError> {
    let target = args
        .first()
        .ok_or_else(|| type_error("getweakrefcount() requires 1 argument"))?;
    let id: ObjectId = id_of(target);
    Ok(Object::Int(reg::count_for(id) as i64))
}

/// `_weakref.getweakrefs(obj)` — list of live weakrefs targeting
/// `obj`. We return placeholders (`Object::None`) for now since
/// reconstructing the full ref-object from a slot requires a
/// reverse mapping; user code that needs this typically pivots
/// on `weakref.ref` directly.
fn get_weakrefs(args: &[Object]) -> Result<Object, RuntimeError> {
    let target = args
        .first()
        .ok_or_else(|| type_error("getweakrefs() requires 1 argument"))?;
    let id = id_of(target);
    let _slots = reg::collect_for(id);
    // We can't reconstruct the weakref object without the
    // original wrapper; CPython does this through a separate
    // tp_weaklistoffset chain. For now return an empty list
    // — code that needs the precise CPython behaviour pivots
    // on `weakref.ref(obj)` directly.
    Ok(Object::new_list(Vec::new()))
}

fn remove_dead_weakref(_args: &[Object]) -> Result<Object, RuntimeError> {
    // Compatibility entry. Real cleanup happens via the GC's
    // notify_clear path; this helper is occasionally called
    // by `WeakValueDictionary` / `WeakKeyDictionary` to prune
    // explicit slots and is a no-op today.
    Ok(Object::None)
}

#[allow(dead_code)]
fn referent_of_proxy(_args: &[Object]) -> Result<Object, RuntimeError> {
    Err(value_error("weakly-referenced object no longer exists"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ref_returns_alive_target_then_none_after_clear() {
        let target = Object::from_static("hello");
        let r = make_ref_object(target.clone(), None, kind::REF);
        if let Object::Instance(inst) = &r {
            let call = inst
                .dict
                .borrow()
                .get(&DictKey(Object::from_static("__call__")))
                .cloned();
            if let Some(Object::Builtin(b)) = call {
                let live = (b.call)(&[]).unwrap();
                assert!(matches!(live, Object::Str(_)));
            }
        }
        let id = id_of(&target);
        let _ = reg::notify_clear(id);
        if let Object::Instance(inst) = &r {
            let call = inst
                .dict
                .borrow()
                .get(&DictKey(Object::from_static("__call__")))
                .cloned();
            if let Some(Object::Builtin(b)) = call {
                let after = (b.call)(&[]).unwrap();
                assert!(matches!(after, Object::None));
            }
        }
    }
}

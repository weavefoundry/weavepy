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
    let dict = Rc::new(RefCell::new(DictData::default()));
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
        // `ref` IS the ReferenceType type object, exactly as in CPython
        // (`_weakref.ref is _weakref.ReferenceType`); instantiation routes
        // through `construct_ref` via the VM's builtin-type special-case.
        d.insert(
            DictKey(Object::from_static("ref")),
            Object::Type(ref_type()),
        );
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
        binds_instance: false,
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
        binds_instance: false,
        call: Box::new(body),
        call_kw: None,
    }))
}

/// Type-dict method variant of [`b`]: looked up through the class MRO,
/// so it must bind the receiver like CPython's method descriptors.
fn m(name: &'static str, body: fn(&[Object]) -> Result<Object, RuntimeError>) -> Object {
    Object::Builtin(Rc::new(BuiltinFn {
        name,
        binds_instance: true,
        call: Box::new(body),
        call_kw: None,
    }))
}

/// Type-level `__call__` for `weakref`/proxy instances.
///
/// CPython looks up special methods (here `__call__`) on the *type*,
/// not the instance, so `weakref.ref(obj)()` must resolve `__call__`
/// via the class MRO. Each ref instance stores its per-target deref
/// closure under `__weakref_get__` in its own dict; this shared
/// type-level method bridges to it so `r()` returns the live target
/// (or `None` once the referent is collected).
fn ref_type_call(args: &[Object]) -> Result<Object, RuntimeError> {
    let me = args
        .first()
        .ok_or_else(|| type_error("__call__() missing self"))?;
    if let Object::Instance(inst) = me {
        let getter = inst
            .dict
            .borrow()
            .get(&DictKey(Object::from_static("__weakref_get__")))
            .cloned();
        if let Some(Object::Builtin(b)) = getter {
            return (b.call)(&[]);
        }
    }
    Err(type_error("__call__() requires a weakref instance"))
}

/// C-API bridge (`PyWeakref_NewRef`): mint a plain weakref to `target`,
/// exactly like `_weakref.ref(target, callback)`.
pub fn c_new_ref(target: Object, callback: Option<Object>) -> Result<Object, RuntimeError> {
    if !supports_weakref(&target) {
        return Err(type_error(format!(
            "cannot create weak reference to '{}' object",
            target.type_name_owned()
        )));
    }
    Ok(make_ref_object(target, callback, kind::REF))
}

/// C-API bridge (`PyWeakref_GetRef`): referent of a weakref wrapper.
/// `Some(Some(target))` while live, `Some(None)` once dead, `None` when
/// `obj` isn't a weakref wrapper at all.
pub fn c_referent(obj: &Object) -> Option<Option<Object>> {
    wrapper_referent(obj)
}

/// Referent of a ref/proxy wrapper through its per-instance deref
/// closure. `Some(Some(target))` while live, `Some(None)` once dead,
/// `None` when `obj` isn't a weakref wrapper at all.
fn wrapper_referent(obj: &Object) -> Option<Option<Object>> {
    let Object::Instance(inst) = obj else {
        return None;
    };
    let getter = inst
        .dict
        .borrow()
        .get(&DictKey(Object::from_static("__weakref_get__")))
        .cloned();
    match getter {
        Some(Object::Builtin(b)) => {
            let t = (b.call)(&[]).ok()?;
            Some(if matches!(t, Object::None) {
                None
            } else {
                Some(t)
            })
        }
        _ => None,
    }
}

/// Native `weakref.__hash__` for the `DictKey` hot path: a `ref`'s hash
/// is its referent's hash, computed once and memoised on the ref
/// (CPython's `weakref_hash`, which caches `wr_hash`). Computing it here
/// — directly, via the same `py_hash_value`/identity reduction the
/// `hash()` builtin uses — avoids a reentrant `__hash__` *dispatch* per
/// hash-table probe, which is otherwise catastrophic for large
/// `WeakKeyDictionary`/`WeakValueDictionary` workloads keyed by `ref`s
/// (`test_weakref` `test_threaded_weak_key_dict_copy`'s 70k entries).
/// Returns `None` when `obj` isn't a `ref`, or its referent is already
/// gone and no hash was ever cached (the caller then falls back to the
/// ref's identity bucket — a dead, never-hashed ref matches nothing).
pub(crate) fn weakref_native_hash(obj: &Object) -> Option<i64> {
    let Object::Instance(inst) = obj else {
        return None;
    };
    if !Rc::ptr_eq(&inst.cls(), &ref_type()) {
        return None;
    }
    if let Some(h) = inst.hash_cache.get() {
        return Some(h);
    }
    let target = wrapper_referent(obj).flatten()?;
    let h = crate::object::py_hash_value(&target)
        .unwrap_or_else(|| crate::object::identity_hash(&target));
    inst.hash_cache.set(Some(h));
    Some(h)
}

/// Type-level `weakref.__eq__` — CPython's `weakref_richcompare`:
/// while both referents are alive compare them with `==`; once either
/// side is dead, fall back to identity of the *refs* themselves. A
/// non-weakref operand declines with `NotImplemented`.
fn ref_type_eq(args: &[Object]) -> Result<Object, RuntimeError> {
    let me = args
        .first()
        .ok_or_else(|| type_error("__eq__() missing self"))?;
    let other = args.get(1).cloned().unwrap_or(Object::None);
    let (Some(a), Some(b)) = (wrapper_referent(me), wrapper_referent(&other)) else {
        return Ok(crate::vm_singletons::not_implemented());
    };
    let result = match (a, b) {
        (Some(ta), Some(tb)) => {
            if let Some(ptr) = crate::vm_singletons::current_interpreter_ptr() {
                // SAFETY: published by an enclosing VM frame on this thread.
                let interp = unsafe { &mut *ptr };
                interp.reentrant_py_eq(&ta, &tb).unwrap_or(false)
            } else {
                ta.is_same(&tb)
            }
        }
        _ => me.is_same(&other),
    };
    Ok(Object::Bool(result))
}

/// Type-level `weakref.__hash__` — hash of the referent, cached on
/// first use; once the referent is gone an uncached hash raises
/// `TypeError` exactly as CPython's `weakref_hash` does.
fn ref_type_hash(args: &[Object]) -> Result<Object, RuntimeError> {
    let me = args
        .first()
        .ok_or_else(|| type_error("__hash__() missing self"))?;
    let Object::Instance(inst) = me else {
        return Err(type_error(
            "descriptor '__hash__' requires a 'weakref' object",
        ));
    };
    // Consult the *same* memo the `DictKey` fast path uses
    // (`weakref_native_hash`, which caches the referent hash in
    // `inst.hash_cache`). This is what keeps a ref hashable across OS
    // threads: `ref_type()` is thread-local, so a ref minted on one thread
    // fails the native path's `Rc::ptr_eq(cls, ref_type())` identity check
    // on another and falls through to this method. Without sharing the
    // `Cell` here it would re-hash a now-dead referent and spuriously raise
    // "weak object has gone away" — exactly `WeakSet._remove`'s
    // `data.discard(ref)` firing on the executor / manager thread during
    // `ProcessPoolExecutor`/`Manager` teardown.
    if let Some(h) = inst.hash_cache.get() {
        return Ok(Object::Int(h));
    }
    let target = wrapper_referent(me)
        .flatten()
        .ok_or_else(|| type_error("weak object has gone away"))?;
    let h = if let Some(ptr) = crate::vm_singletons::current_interpreter_ptr() {
        // SAFETY: published by an enclosing VM frame on this thread.
        let interp = unsafe { &mut *ptr };
        let globals = interp.builtins_dict();
        interp.do_hash_call(&target, &globals)?
    } else {
        crate::builtins::hash_object(&target)?
    };
    // Memoise in the native `Cell` (shared with `weakref_native_hash`) so
    // every later probe — on any thread, via either hash path — agrees on
    // the bucket and a dead ref stays discardable.
    if let Some(hv) = h.as_i64() {
        inst.hash_cache.set(Some(hv));
    }
    Ok(h)
}

/// CPython's `%T` formatter (`PyType_GetFullyQualifiedName`):
/// `module.qualname`, with a `builtins`/`__main__` prefix omitted.
fn fq_type_name(target: &Object) -> String {
    if let Object::Instance(i) = target {
        let cls = i.cls();
        let qual = cls
            .qualname
            .borrow()
            .clone()
            .unwrap_or_else(|| cls.name.clone());
        let module = match cls
            .dict
            .borrow()
            .get(&DictKey(Object::from_static("__module__")))
        {
            Some(Object::Str(s)) => s.to_string(),
            _ => String::new(),
        };
        if module.is_empty() || module == "builtins" || module == "__main__" {
            qual
        } else {
            format!("{module}.{qual}")
        }
    } else {
        target.type_name_owned()
    }
}

/// The referent's `__name__` for `weakref.__repr__`'s optional
/// `(name)` suffix. CPython performs a *type-restricted* lookup
/// (`_PyObject_LookupSpecial`), so an instance `__getattr__` is never
/// consulted and can't blow up the repr (gh-99184: a dict subclass
/// whose `__getattr__` raises `KeyError` for `__name__`).
fn referent_display_name(target: &Object) -> Option<String> {
    match target {
        Object::Type(t) => Some(t.name.clone()),
        Object::Function(f) => Some(f.name.clone()),
        Object::Module(m) => Some(m.name.clone()),
        Object::Instance(i) => match i.cls().lookup("__name__")? {
            Object::Str(s) => Some(s.to_string()),
            Object::Property(p) => {
                let fget = p.fget.borrow().clone();
                let ptr = crate::vm_singletons::current_interpreter_ptr()?;
                // SAFETY: published by an enclosing VM frame on this thread.
                let interp = unsafe { &mut *ptr };
                match interp.call_object(fget, &[target.clone()], &[]).ok()? {
                    Object::Str(s) => Some(s.to_string()),
                    _ => None,
                }
            }
            _ => None,
        },
        _ => None,
    }
}

/// Type-level `weakref.__repr__` — CPython's `weakref_repr`:
/// `<weakref at 0x…; to 'T' at 0x… (name)>` while alive,
/// `<weakref at 0x…; dead>` afterwards.
fn ref_type_repr(args: &[Object]) -> Result<Object, RuntimeError> {
    let me = args
        .first()
        .ok_or_else(|| type_error("__repr__() missing self"))?;
    let self_addr = id_of(me);
    let txt = match wrapper_referent(me) {
        Some(Some(target)) => {
            let tn = fq_type_name(&target);
            let taddr = id_of(&target);
            match referent_display_name(&target) {
                Some(n) => {
                    format!("<weakref at 0x{self_addr:x}; to '{tn}' at 0x{taddr:x} ({n})>")
                }
                None => format!("<weakref at 0x{self_addr:x}; to '{tn}' at 0x{taddr:x}>"),
            }
        }
        _ => format!("<weakref at 0x{self_addr:x}; dead>"),
    };
    Ok(Object::from_str(txt))
}

/// Getter behind the read-only `__callback__` property: the live
/// callback before the referent dies, `None` once it has fired (the
/// clear path nulls the backing dict entry). The property (a data
/// descriptor with no setter) is what makes
/// `ref.__callback__ = …` raise `AttributeError`
/// (test_set_callback_attribute).
fn ref_callback_get(args: &[Object]) -> Result<Object, RuntimeError> {
    if let Some(Object::Instance(inst)) = args.first() {
        if let Some(v) = inst
            .dict
            .borrow()
            .get(&DictKey(Object::from_static("__callback__")))
        {
            return Ok(v.clone());
        }
    }
    Ok(Object::None)
}

fn ref_type() -> Rc<TypeObject> {
    REF_TYPE.with(|cell| {
        if let Some(t) = cell.borrow().clone() {
            return t;
        }
        let mut type_dict = DictData::default();
        type_dict.insert(
            DictKey(Object::from_static("__call__")),
            m("__call__", ref_type_call),
        );
        type_dict.insert(
            DictKey(Object::from_static("__eq__")),
            m("__eq__", ref_type_eq),
        );
        type_dict.insert(
            DictKey(Object::from_static("__hash__")),
            m("__hash__", ref_type_hash),
        );
        type_dict.insert(
            DictKey(Object::from_static("__repr__")),
            m("__repr__", ref_type_repr),
        );
        // Read-only data descriptor: shadows the per-instance dict entry
        // (which backs it) and rejects assignment
        // (test_set_callback_attribute).
        type_dict.insert(
            DictKey(Object::from_static("__callback__")),
            Object::Property(Rc::new(crate::object::PyProperty::new(
                m("__callback__", ref_callback_get),
                Object::None,
                Object::None,
                Object::None,
            ))),
        );
        type_dict.insert(
            DictKey(Object::from_static("__module__")),
            Object::from_static("weakref"),
        );
        // Real `__new__`/`__init__` entries so *subclasses* construct
        // through the weakref machinery (CPython's `weakref___new__` /
        // `weakref___init__`): `class WeakMethod(ref)` and test_weakref's
        // `MyRef` call `ref.__new__(cls, ob, callback)` and expect an
        // instance of `cls` wired to a live slot. The base type's own
        // call path stays on the VM's `construct_ref` special-case.
        type_dict.insert(
            DictKey(Object::from_static("__new__")),
            Object::StaticMethod(crate::object::MethodWrapper::new(Object::Builtin(Rc::new(
                BuiltinFn {
                    name: "weakref.__new__",
                    binds_instance: false,
                    call: Box::new(|args| ref_subclass_new(args, &[])),
                    call_kw: Some(Box::new(ref_subclass_new)),
                },
            )))),
        );
        type_dict.insert(
            DictKey(Object::from_static("__init__")),
            Object::Builtin(Rc::new(BuiltinFn {
                name: "__init__",
                binds_instance: true,
                call: Box::new(|args| ref_init(args, &[])),
                call_kw: Some(Box::new(ref_init)),
            })),
        );
        // CPython 3.13's `tp_name` is `"weakref.ReferenceType"`, so
        // `weakref.ref.__name__ == 'ReferenceType'` and
        // `__module__ == 'weakref'` (test_weakref's ModuleTestCase).
        let t = TypeObject::new_with_flags(
            "ReferenceType",
            vec![crate::builtin_types::builtin_types().object_.clone()],
            type_dict,
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

/// If `obj` is a weakproxy (either flavour), dereference it: CPython's
/// proxy forwards `tp_getattro` wholesale, so attribute reads such as
/// `proxy.__class__` must report the *referent*, never the proxy type.
/// `Some(Err(..))` is a dead referent (ReferenceError).
pub fn proxy_referent(obj: &Object) -> Option<Result<Object, RuntimeError>> {
    if let Object::Instance(inst) = obj {
        let cls = inst.cls();
        if Rc::ptr_eq(&cls, &proxy_type()) || Rc::ptr_eq(&cls, &callable_proxy_type()) {
            return Some(proxy_target(obj));
        }
    }
    None
}

/// Dereference a proxy instance, raising `ReferenceError` once the
/// referent has been collected — CPython's `proxy_checkref`.
fn proxy_target(me: &Object) -> Result<Object, RuntimeError> {
    if let Object::Instance(inst) = me {
        let getter = inst
            .dict
            .borrow()
            .get(&DictKey(Object::from_static("__weakref_get__")))
            .cloned();
        if let Some(Object::Builtin(b)) = getter {
            let t = (b.call)(&[])?;
            if !matches!(t, Object::None) {
                return Ok(t);
            }
            let bt = crate::builtin_types::builtin_types();
            let inst = crate::builtin_types::make_exception_with_class(
                bt.reference_error.clone(),
                "weakly-referenced object no longer exists",
            );
            return Err(RuntimeError::PyException(crate::error::PyException::new(
                inst,
            )));
        }
    }
    Err(type_error("expected a weak proxy"))
}

/// Forward an operation to the referent by calling the named builtin
/// (`iter`, `next`, `len`, …) on it through the live interpreter.
fn proxy_forward_via_builtin(
    builtin: &'static str,
    target: &Object,
) -> Result<Object, RuntimeError> {
    let ptr = crate::vm_singletons::current_interpreter_ptr()
        .ok_or_else(|| type_error("no running interpreter"))?;
    // SAFETY: published by an enclosing VM frame on this thread.
    let interp = unsafe { &mut *ptr };
    let globals = interp.builtins_dict();
    let f = globals
        .borrow()
        .get(&DictKey(Object::from_static(builtin)))
        .cloned()
        .ok_or_else(|| type_error(format!("builtin {builtin} unavailable")))?;
    interp.call_object_with_globals(&f, std::slice::from_ref(target), &[], &globals)
}

/// The shared forwarding dunders for both proxy flavours.
fn install_proxy_forwarding(td: &mut DictData) {
    fn fwd_getattr(args: &[Object]) -> Result<Object, RuntimeError> {
        let target = proxy_target(args.first().ok_or_else(|| type_error("missing self"))?)?;
        let name = match args.get(1) {
            Some(Object::Str(s)) => s.to_string(),
            _ => return Err(type_error("attribute name must be string")),
        };
        let ptr = crate::vm_singletons::current_interpreter_ptr()
            .ok_or_else(|| type_error("no running interpreter"))?;
        // SAFETY: published by an enclosing VM frame on this thread.
        let interp = unsafe { &mut *ptr };
        interp.load_attr_public(&target, &name)
    }
    fn fwd_iter(args: &[Object]) -> Result<Object, RuntimeError> {
        let target = proxy_target(args.first().ok_or_else(|| type_error("missing self"))?)?;
        proxy_forward_via_builtin("iter", &target)
    }
    fn fwd_next(args: &[Object]) -> Result<Object, RuntimeError> {
        let target = proxy_target(args.first().ok_or_else(|| type_error("missing self"))?)?;
        // CPython's `proxy_iternext` checks `PyIter_Check` on the referent
        // first and raises its own message (test_proxy_bad_next).
        let is_iterator = match &target {
            Object::Iter(_) | Object::Generator(_) => true,
            Object::Instance(inst) => inst.cls().lookup("__next__").is_some(),
            _ => false,
        };
        if !is_iterator {
            return Err(type_error("Weakref proxy referenced a non-iterator"));
        }
        proxy_forward_via_builtin("next", &target)
    }
    fn fwd_len(args: &[Object]) -> Result<Object, RuntimeError> {
        let target = proxy_target(args.first().ok_or_else(|| type_error("missing self"))?)?;
        proxy_forward_via_builtin("len", &target)
    }
    fn fwd_str(args: &[Object]) -> Result<Object, RuntimeError> {
        let target = proxy_target(args.first().ok_or_else(|| type_error("missing self"))?)?;
        proxy_forward_via_builtin("str", &target)
    }
    fn fwd_setattr(args: &[Object]) -> Result<Object, RuntimeError> {
        let target = proxy_target(args.first().ok_or_else(|| type_error("missing self"))?)?;
        let name = match args.get(1) {
            Some(Object::Str(s)) => s.to_string(),
            _ => return Err(type_error("attribute name must be string")),
        };
        let value = args
            .get(2)
            .cloned()
            .ok_or_else(|| type_error("__setattr__ expected 2 arguments"))?;
        let ptr = crate::vm_singletons::current_interpreter_ptr()
            .ok_or_else(|| type_error("no running interpreter"))?;
        // SAFETY: published by an enclosing VM frame on this thread.
        let interp = unsafe { &mut *ptr };
        interp.store_attr_public(&target, &name, value)?;
        Ok(Object::None)
    }
    fn fwd_delattr(args: &[Object]) -> Result<Object, RuntimeError> {
        let target = proxy_target(args.first().ok_or_else(|| type_error("missing self"))?)?;
        let name = match args.get(1) {
            Some(Object::Str(s)) => s.to_string(),
            _ => return Err(type_error("attribute name must be string")),
        };
        let ptr = crate::vm_singletons::current_interpreter_ptr()
            .ok_or_else(|| type_error("no running interpreter"))?;
        // SAFETY: published by an enclosing VM frame on this thread.
        let interp = unsafe { &mut *ptr };
        interp.delete_attr_public(&target, &name)?;
        Ok(Object::None)
    }
    fn fwd_dir(args: &[Object]) -> Result<Object, RuntimeError> {
        let target = proxy_target(args.first().ok_or_else(|| type_error("missing self"))?)?;
        proxy_forward_via_builtin("dir", &target)
    }
    fn fwd_reversed(args: &[Object]) -> Result<Object, RuntimeError> {
        let target = proxy_target(args.first().ok_or_else(|| type_error("missing self"))?)?;
        proxy_forward_via_builtin("reversed", &target)
    }
    // CPython's `proxy_bool` runs the full `PyObject_IsTrue` protocol on
    // the referent (`__bool__`, then `__len__`, then default-true), so
    // forward through the `bool` builtin rather than a bare dunder.
    fn fwd_bool(args: &[Object]) -> Result<Object, RuntimeError> {
        let target = proxy_target(args.first().ok_or_else(|| type_error("missing self"))?)?;
        proxy_forward_via_builtin("bool", &target)
    }
    // CPython's `proxy_contains` is `PySequence_Contains(referent, v)` —
    // the *full* membership protocol, including the fall-back to
    // `__iter__` when the referent has no `__contains__`
    // (test_proxy_iter's `"blech" in p` where the referent only
    // defines `__iter__`).
    fn fwd_contains(args: &[Object]) -> Result<Object, RuntimeError> {
        let target = proxy_target(args.first().ok_or_else(|| type_error("missing self"))?)?;
        let item = args
            .get(1)
            .cloned()
            .ok_or_else(|| type_error("__contains__ expected 1 argument"))?;
        let ptr = crate::vm_singletons::current_interpreter_ptr()
            .ok_or_else(|| type_error("no running interpreter"))?;
        // SAFETY: published by an enclosing VM frame on this thread.
        let interp = unsafe { &mut *ptr };
        Ok(Object::Bool(interp.py_contains(&target, &item)?))
    }
    for (name, f) in [
        (
            "__getattr__",
            fwd_getattr as fn(&[Object]) -> Result<Object, RuntimeError>,
        ),
        ("__setattr__", fwd_setattr),
        ("__delattr__", fwd_delattr),
        ("__iter__", fwd_iter),
        ("__next__", fwd_next),
        ("__len__", fwd_len),
        ("__str__", fwd_str),
        ("__dir__", fwd_dir),
        ("__reversed__", fwd_reversed),
        ("__bool__", fwd_bool),
        ("__contains__", fwd_contains),
    ] {
        td.insert(DictKey(Object::from_static(name)), m(name, f));
    }
    // Proxies are unhashable in CPython (`tp_hash = PyObject_HashNotImplemented`):
    // `hash(proxy(o))` raises TypeError (test_proxy_hash). `__hash__ = None`
    // in the type dict is the Python-level spelling of that slot.
    td.insert(DictKey(Object::from_static("__hash__")), Object::None);

    // CPython's proxy fills in the *entire* number/sequence/mapping slot
    // tables with unwrapping forwarders (`WRAP_BINARY(proxy_add,
    // PyNumber_Add)` etc.), so `p + 1.0`, `p // 5`, `p @ m`, `p[1] = x`,
    // `del p[0]`, `operator.index(p)` … all operate on the referent
    // (test_proxy_div/matmul/index/deletion, test_newstyle_number_ops).
    // A binary forwarder that finds no such dunder on the referent
    // declines with `NotImplemented` so the interpreter's reflected /
    // fallback protocol proceeds exactly as if the referent itself were
    // the operand.
    for name in [
        "__add__",
        "__radd__",
        "__iadd__",
        "__sub__",
        "__rsub__",
        "__isub__",
        "__mul__",
        "__rmul__",
        "__imul__",
        "__matmul__",
        "__rmatmul__",
        "__imatmul__",
        "__truediv__",
        "__rtruediv__",
        "__itruediv__",
        "__floordiv__",
        "__rfloordiv__",
        "__ifloordiv__",
        "__mod__",
        "__rmod__",
        "__imod__",
        "__divmod__",
        "__rdivmod__",
        "__pow__",
        "__rpow__",
        "__ipow__",
        "__lshift__",
        "__rlshift__",
        "__ilshift__",
        "__rshift__",
        "__rrshift__",
        "__irshift__",
        "__and__",
        "__rand__",
        "__iand__",
        "__xor__",
        "__rxor__",
        "__ixor__",
        "__or__",
        "__ror__",
        "__ior__",
        "__eq__",
        "__ne__",
        "__lt__",
        "__le__",
        "__gt__",
        "__ge__",
    ] {
        td.insert(
            DictKey(Object::from_static(name)),
            make_proxy_forwarder(name, true),
        );
    }
    // Unary / conversion / container dunders: forwarded the same way but
    // errors propagate (there is no reflected protocol to fall back to).
    for name in [
        "__neg__",
        "__pos__",
        "__abs__",
        "__invert__",
        "__int__",
        "__float__",
        "__index__",
        "__complex__",
        "__bytes__",
        "__getitem__",
        "__setitem__",
        "__delitem__",
    ] {
        td.insert(
            DictKey(Object::from_static(name)),
            make_proxy_forwarder(name, false),
        );
    }
}

/// A type-dict method that dereferences the proxy receiver and re-invokes
/// the named dunder on the referent, unwrapping any proxy among the
/// remaining operands (CPython's `proxy_add`/`proxy_getitem`/… wrappers).
/// With `decline_missing`, a referent without the dunder yields
/// `NotImplemented` instead of an error so binary-operator dispatch can
/// continue with the reflected operand.
fn make_proxy_forwarder(name: &'static str, decline_missing: bool) -> Object {
    let body = move |args: &[Object]| -> Result<Object, RuntimeError> {
        let target = proxy_target(args.first().ok_or_else(|| type_error("missing self"))?)?;
        let ptr = crate::vm_singletons::current_interpreter_ptr()
            .ok_or_else(|| type_error("no running interpreter"))?;
        // SAFETY: published by an enclosing VM frame on this thread.
        let interp = unsafe { &mut *ptr };
        let func = match interp.load_attr_public(&target, name) {
            Ok(f) => f,
            Err(e) => {
                if decline_missing {
                    return Ok(crate::vm_singletons::not_implemented());
                }
                return Err(e);
            }
        };
        let rest: Vec<Object> = args[1..]
            .iter()
            .map(|a| match proxy_referent(a) {
                Some(Ok(t)) => t,
                _ => a.clone(),
            })
            .collect();
        interp.call_object(func, &rest, &[])
    };
    Object::Builtin(Rc::new(BuiltinFn {
        name,
        binds_instance: true,
        call: Box::new(body),
        call_kw: None,
    }))
}

/// `__call__` for `CallableProxyType`: dereference and call the referent
/// with the original positional and keyword arguments
/// (test_callable_proxy's `ref1('twinkies!')` / `ref1(x='Splat.')`).
fn install_callable_proxy_call(td: &mut DictData) {
    fn call_fwd(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
        let target = proxy_target(args.first().ok_or_else(|| type_error("missing self"))?)?;
        let ptr = crate::vm_singletons::current_interpreter_ptr()
            .ok_or_else(|| type_error("no running interpreter"))?;
        // SAFETY: published by an enclosing VM frame on this thread.
        let interp = unsafe { &mut *ptr };
        let rest: Vec<Object> = args[1..]
            .iter()
            .map(|a| match proxy_referent(a) {
                Some(Ok(t)) => t,
                _ => a.clone(),
            })
            .collect();
        interp.call_object(target, &rest, kwargs)
    }
    td.insert(
        DictKey(Object::from_static("__call__")),
        Object::Builtin(Rc::new(BuiltinFn {
            name: "__call__",
            binds_instance: true,
            call: Box::new(|args| call_fwd(args, &[])),
            call_kw: Some(Box::new(call_fwd)),
        })),
    );
}

fn proxy_type() -> Rc<TypeObject> {
    PROXY_TYPE.with(|cell| {
        if let Some(t) = cell.borrow().clone() {
            return t;
        }
        let mut td = DictData::default();
        install_proxy_forwarding(&mut td);
        td.insert(
            DictKey(Object::from_static("__module__")),
            Object::from_static("weakref"),
        );
        let t = TypeObject::new_with_flags(
            "ProxyType",
            vec![crate::builtin_types::builtin_types().object_.clone()],
            td,
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
        let mut td = DictData::default();
        install_proxy_forwarding(&mut td);
        install_callable_proxy_call(&mut td);
        td.insert(
            DictKey(Object::from_static("__module__")),
            Object::from_static("weakref"),
        );
        let t = TypeObject::new_with_flags(
            "CallableProxyType",
            vec![crate::builtin_types::builtin_types().object_.clone()],
            td,
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
    if !supports_weakref(&target) {
        return Err(type_error(format!(
            "cannot create weak reference to '{}' object",
            target.type_name_owned()
        )));
    }
    let callback = extract_callback(args.get(1));
    if callback.is_none() {
        // Reuse the cached callback-less basic ref (CPython's
        // `weakref.ref(o) is weakref.ref(o)` — test_ref_reuse).
        if let Some(cached) = find_cached_wrapper(id_of(&target), kind::REF) {
            return Ok(cached);
        }
    }
    Ok(make_ref_object(target, callback, kind::REF))
}

/// `weakref.__new__(cls, ob, callback=None)` — the subclass allocation
/// path (CPython's `weakref___new__`): validate the target, then mint a
/// fully-wired ref whose class is `cls`, so `WeakMethod`/user subclasses
/// get live slots plus their own MRO.
fn ref_subclass_new(args: &[Object], _kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    // CPython's `weakref___new__` unpacks positionals only
    // (`PyArg_UnpackTuple`) and silently ignores keywords — a subclass
    // like test_weakref's `MyRef(o, value=24)` passes its kwargs on to
    // its own `__init__`; the base `__init__` rejects them instead.
    let cls = match args.first() {
        Some(Object::Type(t)) => t.clone(),
        _ => return Err(type_error("weakref.__new__(): not a type")),
    };
    if args.len() < 2 {
        return Err(type_error("__new__ expected at least 1 argument, got 0"));
    }
    if args.len() > 3 {
        return Err(type_error(format!(
            "__new__ expected at most 2 arguments, got {}",
            args.len() - 1
        )));
    }
    let target = args[1].clone();
    if !supports_weakref(&target) {
        return Err(type_error(format!(
            "cannot create weak reference to '{}' object",
            target.type_name_owned()
        )));
    }
    let callback = extract_callback(args.get(2));
    Ok(make_ref_object_with_class(
        target,
        callback,
        kind::REF,
        Some(cls),
    ))
}

/// `weakref.__init__(self, ob, callback=None)` — accepts the
/// constructor arguments and does nothing (allocation already wired the
/// slot), exactly like CPython's `weakref___init__`. Present so a
/// subclass `__init__` can chain `super().__init__(ob, callback)`
/// without hitting `object.__init__`'s arity error.
fn ref_init(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    if !kwargs.is_empty() {
        return Err(type_error("ref() takes no keyword arguments"));
    }
    // (self, ob[, callback])
    if args.len() < 2 || args.len() > 3 {
        return Err(type_error(format!(
            "__init__ expected at most 2 arguments, got {}",
            args.len().saturating_sub(1)
        )));
    }
    Ok(Object::None)
}

/// Entry point for `weakref.ref(target, callback=None)` when invoked by
/// calling the `ReferenceType` type object (the only spelling CPython
/// has). Wired from the VM's `instantiate` builtin-type dispatch.
pub(crate) fn construct_ref(
    args: &[Object],
    kwargs: &[(String, Object)],
) -> Result<Object, RuntimeError> {
    if !kwargs.is_empty() {
        return Err(type_error("ref() takes no keyword arguments"));
    }
    if args.len() > 2 {
        return Err(type_error(format!(
            "__new__ expected at most 2 arguments, got {}",
            args.len()
        )));
    }
    new_ref(args)
}

/// The live cached wrapper for `(target, kind)` when one exists —
/// CPython reuses a referent's callback-less basic ref and proxy
/// (`get_basic_refs` + the `new == NULL` reuse branch), so
/// `weakref.ref(o) is weakref.ref(o)` and `proxy(o) is proxy(o)` hold
/// (test_ref_reuse / test_proxy_reuse). Only exact native-type wrappers
/// are shared; subclass instances never are.
fn find_cached_wrapper(target_id: ObjectId, kind_tag: u8) -> Option<Object> {
    let base = match kind_tag {
        kind::PROXY => proxy_type(),
        kind::CALLABLE_PROXY => callable_proxy_type(),
        _ => ref_type(),
    };
    for slot in reg::collect_for(target_id) {
        if slot.kind == kind_tag && !slot.is_dead() && !slot.has_callback {
            if let Some(inst) = slot.py_ref.borrow().as_ref().and_then(|w| w.upgrade()) {
                if Rc::ptr_eq(&inst.cls(), &base) {
                    return Some(Object::Instance(inst));
                }
            }
        }
    }
    None
}

/// `_weakref.proxy(obj, callback=None)` — returns a delegating
/// proxy. If `obj` is callable, the proxy is a
/// `CallableProxyType`; otherwise a plain `ProxyType`.
fn new_proxy(args: &[Object]) -> Result<Object, RuntimeError> {
    let target = args
        .first()
        .cloned()
        .ok_or_else(|| type_error("proxy() requires at least 1 argument"))?;
    if !supports_weakref(&target) {
        return Err(type_error(format!(
            "cannot create weak reference to '{}' object",
            target.type_name_owned()
        )));
    }
    let callback = extract_callback(args.get(1));
    // CPython's `PyCallable_Check` (tp_call): instances count when their
    // class MRO exposes `__call__` (test_callable_proxy wraps a plain
    // class with a `__call__` method).
    let is_callable = match &target {
        Object::Function(_)
        | Object::Builtin(_)
        | Object::BoundMethod(_)
        | Object::Type(_)
        | Object::StaticMethod(_) => true,
        Object::Instance(inst) => inst.cls().lookup("__call__").is_some(),
        _ => false,
    };
    let k = if is_callable {
        kind::CALLABLE_PROXY
    } else {
        kind::PROXY
    };
    if callback.is_none() {
        if let Some(cached) = find_cached_wrapper(id_of(&target), k) {
            return Ok(cached);
        }
    }
    Ok(make_ref_object(target, callback, k))
}

fn make_ref_object(target: Object, callback: Option<Object>, kind_tag: u8) -> Object {
    make_ref_object_with_class(target, callback, kind_tag, None)
}

/// [`make_ref_object`] with an explicit instance class — the
/// `ref.__new__(cls, ...)` path, where `cls` is a user subclass
/// (`weakref.WeakMethod`, test_weakref's `MyRef`). `None` selects the
/// kind's own native type.
fn make_ref_object_with_class(
    target: Object,
    callback: Option<Object>,
    kind_tag: u8,
    class_override: Option<Rc<TypeObject>>,
) -> Object {
    let target_id = id_of(&target);
    let slot = Arc::new(WeakRefSlot::new(
        target_id,
        target.clone(),
        callback.is_some(),
        kind_tag,
    ));
    register(slot.clone());

    // RFC 0040 (GC arc): a weakref *with a callback* (`weakref.ref(obj, cb)`,
    // `weakref.finalize`, `multiprocessing.util.Finalize`) must fire that
    // callback the instant the referent's last strong reference drops, not at
    // the next cyclic collection. Enroll the (tracked) referent in the cycle
    // GC's prompt-finalization index so a refcount-death between bytecodes
    // fires it — matching CPython's `tp_dealloc` weakref clear.
    //
    // Callback-*less* refs must clear promptly too (leak tests observe
    // `wr()` going `None` right after the owning scope exits — test_ssl's
    // SSLContext checks), but NOT via this index: enrolling every weakref
    // target would leave `has_any_finalizable()` permanently true from the
    // interpreter-boot weakrefs (`abc`/`typing` registries), turning the
    // eval loop's drop safe point into a full index scan on every
    // reference-dropping opcode (a 3-4x interpreter-wide slowdown, RFC 0054
    // WS5 re-measure). They are served by the opcode-level prompt-reap
    // cascade (`reap_dead_subgraph` clears weakrefs) plus the suspect
    // re-probe for deaths inside Rust transients.
    if callback.is_some() {
        crate::gc_trace::note_weakref_finalizable(target_id);
    }

    let dict = Rc::new(RefCell::new(DictData::default()));

    let class = class_override.unwrap_or_else(|| match kind_tag {
        kind::PROXY => proxy_type(),
        kind::CALLABLE_PROXY => callable_proxy_type(),
        _ => ref_type(),
    });

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
        reg::queue_callbacks(reg::notify_clear(target_id_for_clear));
        Ok(Object::None)
    };
    let slot_for_alive = slot.clone();
    let alive = move |_args: &[Object]| -> Result<Object, RuntimeError> {
        Ok(Object::Bool(!slot_for_alive.is_dead()))
    };
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

    let inst = Rc::new(PyInstance {
        class: crate::sync::RefCell::new(class),
        dict,
        native: std::sync::OnceLock::new(),
        inline_values: crate::sync::Cell::new(true),
        slots: crate::sync::RefCell::new(None),
        hash_cache: crate::sync::Cell::new(None),
        finalize_ran: crate::sync::Cell::new(false),
        c_body: crate::types::CBody::default(),
    });
    // Back-pointer so `obj.__weakref__` / `getweakrefs(obj)` can return
    // this same wrapper object.
    *slot.py_ref.borrow_mut() = Some(Rc::downgrade(&inst));
    let wrapper = Object::Instance(inst);
    // CPython GC-tracks a weakref *with a callback* (`gc_track` in
    // `weakref___init__`): the wrapper's strong `wr_callback` edge (our
    // `__callback__` dict entry) must be visible to the cycle collector
    // or a cycle routed through the callback — `c.wr = ref(d, c.cb)`
    // with `c ↔ d` — is never collected (test_callbacks_on_callback,
    // test_callback_in_cycle_resurrection). Callback-less wrappers stay
    // untracked, as in CPython.
    //
    // We narrow CPython's rule for throughput: a callback that is a
    // builtin or a closure-free plain function can only route a cycle
    // through its module globals, which stay alive until interpreter
    // shutdown anyway — while `WeakValueDictionary`/`WeakSet` mint one
    // closure-free `_remove` callback ref per entry, so tracking those
    // would put the whole population (70k in test_weakref's threaded
    // copy tests) on the collector's candidate list. Instances (a
    // `weakref.finalize` object is its own callback), bound methods and
    // closures — the shapes that can actually close a user-visible
    // cycle — are tracked.
    let callback_can_cycle = match &callback {
        None | Some(Object::Builtin(_)) => false,
        Some(Object::Function(f)) => !f.closure.is_empty(),
        Some(_) => true,
    };
    if callback_can_cycle {
        crate::gc_trace::track(wrapper.clone());
    }
    wrapper
}

/// Can a weak reference be created to `target`? Mirrors CPython's
/// `tp_weaklistoffset != 0` check for the cases we model: instances of
/// pure-`__slots__` classes are only weakref-able when `__weakref__`
/// appears in the slots of some class on the MRO (or a dict-bearing
/// user class contributes its implicit weakref support). Everything
/// else in our heap remains permissively weakref-able.
pub(crate) fn supports_weakref(target: &Object) -> bool {
    let inst = match target {
        Object::Instance(inst) => inst,
        // Built-ins that carry a `tp_weaklistoffset` in CPython and so can
        // be the target of a `weakref.ref`: `set`, `bytearray`, functions,
        // bound methods, classes, modules, generators/coroutines/
        // async-generators, file objects and `types.SimpleNamespace`.
        Object::Set(_)
        | Object::ByteArray(_)
        | Object::Function(_)
        | Object::BoundMethod(_)
        | Object::Type(_)
        | Object::Module(_)
        | Object::Generator(_)
        | Object::Coroutine(_)
        | Object::AsyncGenerator(_)
        | Object::File(_)
        // `memoryview` grew `tp_weaklistoffset` support in CPython
        // (test_memoryio.test_getbuffer_gc_collect takes a `weakref.ref`
        // to a `BytesIO.getbuffer()` view).
        | Object::MemoryView(_)
        // Code objects carry weakref support in CPython
        // (test_code.CodeWeakRefTest).
        | Object::Code(_)
        | Object::SimpleNamespace(_) => return true,
        // Everything else — numbers, `str`/`bytes`, `tuple`/`list`/`dict`/
        // `frozenset`/`range`, slices, the descriptor and internal frame/
        // code/iterator types — has no weak-reference support, exactly like
        // CPython, so `weakref.ref([])` / `ref({})` / `ref(1)` raise
        // `TypeError` (`test_weakset` passes `[[]]` to assert this).
        _ => return false,
    };
    // CPython: a heap class contributes weakref support unless it
    // declares `__slots__` without listing `"__weakref__"`. Any single
    // slots-free (or weakref-slotted) class on the MRO is enough —
    // `__slots__ = ["__dict__"]` grants a dict but *not* weakrefs.
    let cls = inst.cls();
    // CPython's `module` type carries `tp_weaklistoffset`, so every module
    // object is weakly referenceable — including `types.ModuleType(...)`
    // doubles and the fresh extension-module re-imports
    // `import_fresh_module` hands back. `test_struct`'s reference-cycle
    // test takes a `weakref.ref` to a freshly imported `_struct`.
    if cls.is_subclass_of(&crate::builtin_types::builtin_types().module_) {
        return true;
    }
    // The native `_thread` synchronisation primitives and `mmap.mmap`
    // carry a `tp_weaklistoffset` in CPython (`lock_tests` takes weakrefs
    // to locks; `test_mmap.test_weakref` to mappings). They're builtin
    // types with an all-builtin MRO, so the loop below would otherwise
    // reject them.
    if cls
        .mro
        .borrow()
        .iter()
        .any(|t| matches!(t.name.as_str(), "lock" | "RLock" | "_ThreadHandle" | "mmap"))
    {
        return true;
    }
    // CPython's `_io._IOBase` carries a `tp_weaklistoffset`, so every io
    // object — `FileIO`/`Buffered*`/`TextIOWrapper` and any user subclass —
    // is weakly referenceable (`test_io` gc/`test_weakref_clearing`). The
    // whole io tower is rooted at one of these (immutable, all-builtin) ABCs,
    // so checking the MRO names covers both the native types and subclasses.
    if cls.mro.borrow().iter().any(|t| {
        matches!(
            t.name.as_str(),
            "IOBase" | "RawIOBase" | "BufferedIOBase" | "TextIOBase"
        )
    }) {
        return true;
    }
    let mro = cls.mro.borrow().clone();
    for ty in mro {
        if ty.flags.is_builtin {
            continue;
        }
        if !ty.declares_slots.get() {
            return true;
        }
        if ty.slot_names.borrow().iter().any(|s| s == "__weakref__") {
            return true;
        }
    }
    false
}

/// The first live user-visible weakref object (kind `REF`, no callback
/// preferred) targeting `obj`, if any — CPython's "basic ref" served by
/// the `__weakref__` getset.
pub(crate) fn basic_ref_for(obj: &Object) -> Option<Object> {
    let id = id_of(obj);
    let slots = reg::collect_for(id);
    for slot in slots {
        if slot.kind != kind::REF || slot.is_dead() {
            continue;
        }
        if let Some(w) = slot.py_ref.borrow().as_ref() {
            if let Some(inst) = w.upgrade() {
                return Some(Object::Instance(inst));
            }
        }
    }
    None
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
    // CPython keeps the *basic* refs (exact `ReferenceType`, no callback —
    // the shared/cached ones) at the head of the referent's weakref list;
    // subclass instances and callback-carrying refs follow
    // (test_subclass_refs_dont_replace_standard_refs asserts
    // `getweakrefs(o)[0]` is the plain `weakref.ref(o)`).
    let base = ref_type();
    let mut basics = Vec::new();
    let mut rest = Vec::new();
    for slot in reg::collect_for(id) {
        if slot.is_dead() {
            continue;
        }
        if let Some(w) = slot.py_ref.borrow().as_ref() {
            if let Some(inst) = w.upgrade() {
                let is_basic =
                    slot.kind == kind::REF && !slot.has_callback && Rc::ptr_eq(&inst.cls(), &base);
                if is_basic {
                    basics.push(Object::Instance(inst));
                } else {
                    rest.push(Object::Instance(inst));
                }
            }
        }
    }
    basics.extend(rest);
    Ok(Object::new_list(basics))
}

/// `_weakref._remove_dead_weakref(dct, key)` — CPython's atomic
/// dead-weakref pruner (`Modules/_weakref.c`). Removes `dct[key]` *only*
/// if the value currently stored there is a weakref whose referent has
/// been cleared. `WeakValueDictionary` relies on this from its removal
/// callback: a key may have been rebound to a fresh, live weakref
/// between the old value's death and the callback firing, and that live
/// entry must survive (`test_weakref` threaded-consistency).
fn remove_dead_weakref(args: &[Object]) -> Result<Object, RuntimeError> {
    let (Some(dict_obj), Some(key)) = (args.first(), args.get(1)) else {
        return Err(type_error(
            "_remove_dead_weakref expected 2 arguments, got fewer",
        ));
    };
    let Object::Dict(d) = dict_obj else {
        return Err(type_error("_remove_dead_weakref expected a dictionary"));
    };
    let dk = DictKey(key.clone());
    // Snapshot the current value, dropping the shared borrow before any
    // mutation so the conditional remove can take the exclusive borrow.
    let value = crate::object::key_cmp_scope(|| d.borrow().get(&dk).cloned())?;
    let Some(value) = value else {
        return Ok(Object::None);
    };
    // `Some(None)` == a weakref wrapper whose referent is gone. Anything
    // else (a live ref, or a non-weakref value) is left in place — this
    // is CPython's `is_dead_weakref` predicate.
    if matches!(wrapper_referent(&value), Some(None)) {
        crate::object::key_cmp_scope(|| {
            d.borrow_mut().shift_remove(&dk);
        })?;
    }
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

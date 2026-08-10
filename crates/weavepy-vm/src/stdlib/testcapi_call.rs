//! RFC 0060 — native `_testcapi` vectorcall fixtures (CPython
//! `Modules/_testcapi/vectorcall.c`), exercised by `test_call.TestPEP590`.
//!
//! Four fixture types plus a heap-type factory:
//!
//! - `MethodDescriptorBase` — supports vectorcall, has
//!   `Py_TPFLAGS_METHOD_DESCRIPTOR`; calling an instance returns `True`.
//! - `MethodDescriptorDerived` — subclass, inherits both.
//! - `MethodDescriptorNopGet` — implements `tp_call` (no vectorcall, no
//!   method-descriptor flag); its call returns the argument tuple
//!   *itself* (the `f(*args) is args` identity assertion — CPython's
//!   `tp_call` receives the call-site tuple unchanged).
//! - `MethodDescriptor2` — own `tp_call` returning `False`, keeps the
//!   vectorcall flag.
//! - `make_vectorcall_class([base])` — a *mutable* heap type whose
//!   `tp_call` returns `"tp_call"` until `instance.set_vectorcall(tp)`
//!   flips the instance to its vectorcall path (`"vectorcall"`).
//!
//! The `Py_TPFLAGS_HAVE_VECTORCALL` / `Py_TPFLAGS_METHOD_DESCRIPTOR`
//! bits these tests read off `type.__flags__` are answered from the
//! side registries below (see [`type_has_vectorcall`]): CPython clears
//! an inherited vectorcall flag when `__call__` is (re)assigned on a
//! mutable type, which maps exactly onto "the type's own `__call__` is
//! no longer the one the fixture installed".

use std::sync::atomic::{AtomicBool, Ordering};

use crate::builtin_types::builtin_types;
use crate::error::{type_error, RuntimeError};
use crate::object::{BuiltinFn, DictData, DictKey, Object};
use crate::sync::Rc;
use crate::types::{PyInstance, TypeFlags, TypeObject};

/// Fast gate for [`crate::TypeObject::flags_bits`]: true once any fixture
/// type exists in this process, so ordinary programs never pay for the
/// registry lookups.
static FIXTURES_ACTIVE: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Copy)]
struct FixtureFlags {
    vectorcall: bool,
    method_descriptor: bool,
    /// The `f(*args) is args` type (`MethodDescriptorNopGet`).
    nopget: bool,
}

#[allow(clippy::type_complexity)]
static REGISTRY: std::sync::LazyLock<
    parking_lot::Mutex<std::collections::HashMap<usize, FixtureFlags>>,
> = std::sync::LazyLock::new(|| parking_lot::Mutex::new(std::collections::HashMap::new()));

/// `make_vectorcall_class` types: type ptr → address of the *original*
/// native `__call__` object installed at creation. While the type's own
/// `__call__` is still that object the type "has vectorcall"; assigning
/// a new `__call__` (CPython `type_modified` clearing the flag) breaks
/// the identity and the flag reads false — for the type and every
/// subclass that inherits its `__call__`.
#[allow(clippy::type_complexity)]
static HEAP_VECTORCALL: std::sync::LazyLock<
    parking_lot::Mutex<std::collections::HashMap<usize, usize>>,
> = std::sync::LazyLock::new(|| parking_lot::Mutex::new(std::collections::HashMap::new()));

fn object_addr(o: &Object) -> usize {
    match o {
        Object::Builtin(b) => Rc::as_ptr(b) as usize,
        _ => 0,
    }
}

fn register(ty: &Rc<TypeObject>, flags: FixtureFlags) {
    REGISTRY.lock().insert(Rc::as_ptr(ty) as usize, flags);
    FIXTURES_ACTIVE.store(true, Ordering::Release);
}

#[inline]
pub fn fixtures_active() -> bool {
    FIXTURES_ACTIVE.load(Ordering::Relaxed)
}

/// `Py_TPFLAGS_METHOD_DESCRIPTOR` contribution for a fixture type
/// (never inherited by heap subclasses, matching CPython).
pub fn type_is_method_descriptor(ty: &TypeObject) -> bool {
    let ptr = std::ptr::from_ref::<TypeObject>(ty) as usize;
    REGISTRY
        .lock()
        .get(&ptr)
        .is_some_and(|f| f.method_descriptor)
}

/// `Py_TPFLAGS_HAVE_VECTORCALL` for a type, resolved through its MRO:
/// the first type that either is a native fixture or defines `__call__`
/// decides — a fixture answers its static flag; a heap type answers
/// "is my `__call__` still the original vectorcall-backed one?".
pub fn type_has_vectorcall(ty: &TypeObject) -> bool {
    let call_key = DictKey(Object::from_static("__call__"));
    for t in ty.mro.borrow().iter() {
        let ptr = Rc::as_ptr(t) as usize;
        if let Some(f) = REGISTRY.lock().get(&ptr) {
            return f.vectorcall;
        }
        if let Some(call) = t.dict.borrow().get(&call_key) {
            return HEAP_VECTORCALL
                .lock()
                .get(&ptr)
                .is_some_and(|orig| object_addr(call) == *orig);
        }
    }
    false
}

/// True when `obj` is an instance of `MethodDescriptorNopGet` (or a
/// subclass that inherits its `tp_call`): `CALL_FUNCTION_EX` then hands
/// back the call-site tuple itself, as CPython's `tp_call` would.
pub fn instance_call_returns_args_tuple(inst: &PyInstance) -> bool {
    let call_key = DictKey(Object::from_static("__call__"));
    for t in inst.cls().mro.borrow().iter() {
        let ptr = Rc::as_ptr(t) as usize;
        if let Some(f) = REGISTRY.lock().get(&ptr) {
            return f.nopget;
        }
        if t.dict.borrow().contains_key(&call_key) {
            return false;
        }
    }
    false
}

fn builtin(name: &'static str, f: fn(&[Object]) -> Result<Object, RuntimeError>) -> Object {
    Object::Builtin(Rc::new(BuiltinFn {
        name,
        binds_instance: true,
        call: Box::new(f),
        call_kw: None,
    }))
}

fn base_call(_args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(Object::Bool(true))
}

fn desc2_call(_args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(Object::Bool(false))
}

/// `MethodDescriptorNopGet.tp_call` on the plain (non-splat) call path:
/// a fresh tuple of the positional arguments after `self`. The
/// identity-preserving splat path lives in `CALL_FUNCTION_EX`.
fn nopget_call(args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(Object::new_tuple(args[1..].to_vec()))
}

const VECTORCALL_SET_KEY: &str = "_weave_vectorcall_set";

/// `tp_call` of a `make_vectorcall_class` type: `"vectorcall"` once the
/// instance's vectorcall member is set, `"tp_call"` before.
fn vc_class_call(args: &[Object]) -> Result<Object, RuntimeError> {
    let Some(Object::Instance(inst)) = args.first() else {
        return Err(type_error("expected instance"));
    };
    let set = inst
        .dict
        .borrow()
        .contains_key(&DictKey(Object::from_static(VECTORCALL_SET_KEY)));
    Ok(Object::from_static(if set {
        "vectorcall"
    } else {
        "tp_call"
    }))
}

/// `instance.set_vectorcall(type)` — install the recording vectorcall
/// function on the instance (the type argument only locates the member
/// in CPython's fixture; the effect is per-instance either way).
fn vc_set_vectorcall(args: &[Object]) -> Result<Object, RuntimeError> {
    let Some(Object::Instance(inst)) = args.first() else {
        return Err(type_error("expected instance"));
    };
    inst.dict.borrow_mut().insert(
        DictKey(Object::from_static(VECTORCALL_SET_KEY)),
        Object::Bool(true),
    );
    Ok(Object::None)
}

/// Build the four `MethodDescriptor*` fixture types. Returns
/// `(name, type)` pairs for the module dict.
pub fn method_descriptor_types() -> Vec<(&'static str, Rc<TypeObject>)> {
    let bt = builtin_types();
    let flags = TypeFlags {
        is_exception: false,
        is_builtin: true,
    };

    let mut base_dict = DictData::default();
    base_dict.insert(
        DictKey(Object::from_static("__call__")),
        builtin("__call__", base_call),
    );
    let base = TypeObject::new_with_flags(
        "MethodDescriptorBase",
        vec![bt.object_.clone()],
        base_dict,
        flags,
    )
    .expect("MethodDescriptorBase");
    register(
        &base,
        FixtureFlags {
            vectorcall: true,
            method_descriptor: true,
            nopget: false,
        },
    );

    let derived = TypeObject::new_with_flags(
        "MethodDescriptorDerived",
        vec![base.clone()],
        DictData::default(),
        flags,
    )
    .expect("MethodDescriptorDerived");
    register(
        &derived,
        FixtureFlags {
            vectorcall: true,
            method_descriptor: true,
            nopget: false,
        },
    );

    let mut nopget_dict = DictData::default();
    nopget_dict.insert(
        DictKey(Object::from_static("__call__")),
        builtin("__call__", nopget_call),
    );
    let nopget = TypeObject::new_with_flags(
        "MethodDescriptorNopGet",
        vec![base.clone()],
        nopget_dict,
        flags,
    )
    .expect("MethodDescriptorNopGet");
    register(
        &nopget,
        FixtureFlags {
            vectorcall: false,
            method_descriptor: false,
            nopget: true,
        },
    );

    let mut d2_dict = DictData::default();
    d2_dict.insert(
        DictKey(Object::from_static("__call__")),
        builtin("__call__", desc2_call),
    );
    let d2 = TypeObject::new_with_flags("MethodDescriptor2", vec![base.clone()], d2_dict, flags)
        .expect("MethodDescriptor2");
    register(
        &d2,
        FixtureFlags {
            vectorcall: true,
            method_descriptor: false,
            nopget: false,
        },
    );

    vec![
        ("MethodDescriptorBase", base),
        ("MethodDescriptorDerived", derived),
        ("MethodDescriptorNopGet", nopget),
        ("MethodDescriptor2", d2),
    ]
}

/// `make_vectorcall_class([base])` — a fresh *mutable* heap type with the
/// recording `tp_call`/vectorcall pair and a `set_vectorcall` method.
pub fn make_vectorcall_class(args: &[Object]) -> Result<Object, RuntimeError> {
    let base = match args.first() {
        None | Some(Object::None) => builtin_types().object_.clone(),
        Some(Object::Type(t)) => t.clone(),
        Some(other) => {
            return Err(type_error(format!(
                "expected a type, got {}",
                other.type_name()
            )))
        }
    };
    let call = builtin("__call__", vc_class_call);
    let call_addr = object_addr(&call);
    let mut dict = DictData::default();
    dict.insert(DictKey(Object::from_static("__call__")), call);
    dict.insert(
        DictKey(Object::from_static("set_vectorcall")),
        builtin("set_vectorcall", vc_set_vectorcall),
    );
    let ty = TypeObject::new_user("VectorCallClass", vec![base], dict)?;
    HEAP_VECTORCALL
        .lock()
        .insert(Rc::as_ptr(&ty) as usize, call_addr);
    FIXTURES_ACTIVE.store(true, Ordering::Release);
    Ok(Object::Type(ty))
}

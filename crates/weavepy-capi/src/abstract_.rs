//! `PyObject_*`, `PyNumber_*`, `PySequence_*`, `PyMapping_*` —
//! the "abstract object" protocol.
//!
//! These functions translate to native operations on
//! [`weavepy_vm::object::Object`]. Calls that need an active
//! interpreter (e.g. attribute access through user-defined
//! `__getattr__`, function invocation) reach into
//! [`crate::interp`].

use std::ffi::CStr;
use std::os::raw::{c_char, c_int};
use std::ptr;
use weavepy_vm::sync::Rc;

use weavepy_vm::error::RuntimeError;
use weavepy_vm::object::{DictKey, Object};

use crate::object::{PyHashT, PyObject, PySsizeT};

// ----------------------------------------------------------------
// PyObject_* helpers.
// ----------------------------------------------------------------

#[no_mangle]
pub unsafe extern "C" fn PyObject_Repr(o: *mut PyObject) -> *mut PyObject {
    if o.is_null() {
        return ptr::null_mut();
    }
    let obj = unsafe { crate::object::clone_object(o) };
    let s = repr_for(&obj);
    crate::object::into_owned(Object::from_str(s))
}

#[no_mangle]
pub unsafe extern "C" fn PyObject_Str(o: *mut PyObject) -> *mut PyObject {
    if o.is_null() {
        return ptr::null_mut();
    }
    let obj = unsafe { crate::object::clone_object(o) };
    let s = str_for(&obj);
    crate::object::into_owned(Object::from_str(s))
}

#[no_mangle]
pub unsafe extern "C" fn PyObject_ASCII(o: *mut PyObject) -> *mut PyObject {
    unsafe { PyObject_Repr(o) }
}

fn repr_for(o: &Object) -> String {
    use Object as O;
    match o {
        O::None => "None".to_owned(),
        O::Bool(b) => if *b { "True" } else { "False" }.to_owned(),
        O::Int(i) => i.to_string(),
        O::Long(big) => big.to_string(),
        O::Float(f) => crate::numbers_format::format_float(*f),
        O::Str(s) => format!("'{}'", s.replace('\\', "\\\\").replace('\'', "\\'")),
        O::Bytes(b) => format!("b'{}'", String::from_utf8_lossy(b)),
        O::Tuple(items) => {
            let inner: Vec<String> = items.iter().map(repr_for).collect();
            if items.len() == 1 {
                format!("({},)", inner[0])
            } else {
                format!("({})", inner.join(", "))
            }
        }
        O::List(rc) => {
            let inner: Vec<String> = rc.borrow().iter().map(repr_for).collect();
            format!("[{}]", inner.join(", "))
        }
        O::Dict(rc) => {
            let inner: Vec<String> = rc
                .borrow()
                .iter()
                .map(|(k, v)| format!("{}: {}", repr_for(&k.0), repr_for(v)))
                .collect();
            format!("{{{}}}", inner.join(", "))
        }
        O::Type(t) => format!("<class '{}'>", t.name),
        O::Module(m) => format!("<module '{}'>", m.name),
        _ => format!("{o:?}"),
    }
}

fn str_for(o: &Object) -> String {
    if let Object::Str(s) = o {
        return s.to_string();
    }
    repr_for(o)
}

#[no_mangle]
pub unsafe extern "C" fn PyObject_GetAttr(o: *mut PyObject, attr: *mut PyObject) -> *mut PyObject {
    if o.is_null() || attr.is_null() {
        return ptr::null_mut();
    }
    let key = match unsafe { crate::object::clone_object(attr) } {
        Object::Str(s) => s.to_string(),
        _ => {
            crate::errors::set_type_error("attribute name must be string");
            return ptr::null_mut();
        }
    };
    do_getattr(o, &key)
}

#[no_mangle]
pub unsafe extern "C" fn PyObject_GetAttrString(
    o: *mut PyObject,
    attr: *const c_char,
) -> *mut PyObject {
    if o.is_null() || attr.is_null() {
        return ptr::null_mut();
    }
    let key = unsafe { CStr::from_ptr(attr) }
        .to_string_lossy()
        .into_owned();
    do_getattr(o, &key)
}

fn do_getattr(o: *mut PyObject, key: &str) -> *mut PyObject {
    let obj = unsafe { crate::object::clone_object(o) };
    match attr_lookup(&obj, key) {
        Some(v) => crate::object::into_owned(v),
        None => {
            crate::errors::set_pending(
                Some(
                    weavepy_vm::builtin_types::builtin_types()
                        .attribute_error
                        .clone(),
                ),
                Object::from_str(format!(
                    "'{}' object has no attribute '{}'",
                    type_name(&obj),
                    key
                )),
            );
            ptr::null_mut()
        }
    }
}

fn attr_lookup(o: &Object, key: &str) -> Option<Object> {
    match o {
        Object::Module(m) => {
            let kk = DictKey(Object::from_str(key));
            m.dict.borrow().get(&kk).cloned()
        }
        Object::Dict(rc) => {
            let kk = DictKey(Object::from_str(key));
            rc.borrow().get(&kk).cloned()
        }
        Object::Type(t) => t.lookup(key),
        Object::Instance(inst) => {
            let kk = DictKey(Object::from_str(key));
            if let Some(v) = inst.dict.borrow().get(&kk).cloned() {
                return Some(v);
            }
            inst.class.lookup(key)
        }
        _ => None,
    }
}

fn type_name(o: &Object) -> &'static str {
    use Object as O;
    match o {
        O::None => "NoneType",
        O::Bool(_) => "bool",
        O::Int(_) | O::Long(_) => "int",
        O::Float(_) => "float",
        O::Complex(_) => "complex",
        O::Str(_) => "str",
        O::Bytes(_) => "bytes",
        O::ByteArray(_) => "bytearray",
        O::Tuple(_) => "tuple",
        O::List(_) => "list",
        O::Dict(_) => "dict",
        O::Set(_) => "set",
        O::FrozenSet(_) => "frozenset",
        O::Range(_) => "range",
        O::Module(_) => "module",
        O::Type(_) => "type",
        O::Function(_) | O::Builtin(_) => "function",
        O::BoundMethod(_) => "method",
        O::Generator(_) => "generator",
        O::Coroutine(_) => "coroutine",
        O::Slice(_) => "slice",
        _ => "object",
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyObject_SetAttr(
    o: *mut PyObject,
    attr: *mut PyObject,
    value: *mut PyObject,
) -> c_int {
    if o.is_null() || attr.is_null() {
        return -1;
    }
    let key = match unsafe { crate::object::clone_object(attr) } {
        Object::Str(s) => s.to_string(),
        _ => return -1,
    };
    do_setattr(o, &key, value)
}

#[no_mangle]
pub unsafe extern "C" fn PyObject_SetAttrString(
    o: *mut PyObject,
    attr: *const c_char,
    value: *mut PyObject,
) -> c_int {
    if o.is_null() || attr.is_null() {
        return -1;
    }
    let key = unsafe { CStr::from_ptr(attr) }
        .to_string_lossy()
        .into_owned();
    do_setattr(o, &key, value)
}

fn do_setattr(o: *mut PyObject, key: &str, value: *mut PyObject) -> c_int {
    let obj = unsafe { crate::object::clone_object(o) };
    match obj {
        Object::Module(m) => {
            let v = if value.is_null() {
                m.dict
                    .borrow_mut()
                    .shift_remove(&DictKey(Object::from_str(key)));
                return 0;
            } else {
                unsafe { crate::object::clone_object(value) }
            };
            m.dict
                .borrow_mut()
                .insert(DictKey(Object::from_str(key)), v);
            0
        }
        Object::Dict(rc) => {
            if value.is_null() {
                rc.borrow_mut()
                    .shift_remove(&DictKey(Object::from_str(key)));
            } else {
                let v = unsafe { crate::object::clone_object(value) };
                rc.borrow_mut().insert(DictKey(Object::from_str(key)), v);
            }
            0
        }
        Object::Instance(inst) => {
            if value.is_null() {
                inst.dict
                    .borrow_mut()
                    .shift_remove(&DictKey(Object::from_str(key)));
            } else {
                let v = unsafe { crate::object::clone_object(value) };
                inst.dict
                    .borrow_mut()
                    .insert(DictKey(Object::from_str(key)), v);
            }
            0
        }
        _ => {
            crate::errors::set_type_error("object has no settable attributes");
            -1
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyObject_HasAttr(o: *mut PyObject, attr: *mut PyObject) -> c_int {
    let p = unsafe { PyObject_GetAttr(o, attr) };
    if p.is_null() {
        crate::errors::clear_thread_local();
        0
    } else {
        unsafe { crate::object::Py_DecRef(p) };
        1
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyObject_HasAttrString(o: *mut PyObject, attr: *const c_char) -> c_int {
    let p = unsafe { PyObject_GetAttrString(o, attr) };
    if p.is_null() {
        crate::errors::clear_thread_local();
        0
    } else {
        unsafe { crate::object::Py_DecRef(p) };
        1
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyObject_DelAttrString(o: *mut PyObject, attr: *const c_char) -> c_int {
    unsafe { PyObject_SetAttrString(o, attr, ptr::null_mut()) }
}

#[no_mangle]
pub unsafe extern "C" fn PyObject_Call(
    callable: *mut PyObject,
    args: *mut PyObject,
    kwargs: *mut PyObject,
) -> *mut PyObject {
    if callable.is_null() {
        crate::errors::set_type_error("PyObject_Call: callable is NULL");
        return ptr::null_mut();
    }
    let target = unsafe { crate::object::clone_object(callable) };
    let arg_vec = if args.is_null() {
        Vec::new()
    } else {
        match unsafe { crate::object::clone_object(args) } {
            Object::Tuple(items) => items.iter().cloned().collect(),
            Object::List(rc) => rc.borrow().clone(),
            other => vec![other],
        }
    };
    let kwarg_pairs = if kwargs.is_null() {
        Vec::new()
    } else {
        match unsafe { crate::object::clone_object(kwargs) } {
            Object::Dict(rc) => rc
                .borrow()
                .iter()
                .map(|(k, v)| (key_string(&k.0), v.clone()))
                .collect::<Vec<_>>(),
            _ => Vec::new(),
        }
    };

    invoke_callable(target, arg_vec, kwarg_pairs)
}

#[no_mangle]
pub unsafe extern "C" fn PyObject_CallObject(
    callable: *mut PyObject,
    args: *mut PyObject,
) -> *mut PyObject {
    unsafe { PyObject_Call(callable, args, ptr::null_mut()) }
}

#[no_mangle]
pub unsafe extern "C" fn PyObject_CallNoArgs(callable: *mut PyObject) -> *mut PyObject {
    unsafe { PyObject_Call(callable, ptr::null_mut(), ptr::null_mut()) }
}

#[no_mangle]
pub unsafe extern "C" fn PyObject_CallOneArg(
    callable: *mut PyObject,
    arg: *mut PyObject,
) -> *mut PyObject {
    if callable.is_null() {
        return ptr::null_mut();
    }
    let target = unsafe { crate::object::clone_object(callable) };
    let arg_obj = if arg.is_null() {
        Object::None
    } else {
        unsafe { crate::object::clone_object(arg) }
    };
    invoke_callable(target, vec![arg_obj], Vec::new())
}

fn key_string(o: &Object) -> String {
    match o {
        Object::Str(s) => s.to_string(),
        _ => format!("{o:?}"),
    }
}

fn invoke_callable(
    target: Object,
    args: Vec<Object>,
    kwargs: Vec<(String, Object)>,
) -> *mut PyObject {
    let result: Result<Object, RuntimeError> = match target {
        Object::Builtin(bf) => (bf.call)(&args),
        Object::Type(_) | Object::Function(_) | Object::BoundMethod(_) => {
            // For non-Builtin callables we need the VM to do the
            // dispatch (locals, frame setup, etc.).
            let r = crate::interp::with_interp_mut(|interp| {
                interp.call_object(target.clone(), &args, &kwargs)
            });
            match r {
                Some(r) => r,
                None => Err(weavepy_vm::error::runtime_error(
                    "PyObject_Call: no active interpreter",
                )),
            }
        }
        Object::None => Err(weavepy_vm::error::type_error(
            "PyObject_Call: NoneType is not callable",
        )),
        other => {
            // Best-effort: maybe `__call__` is defined.
            if let Some(call) = attr_lookup(&other, "__call__") {
                invoke_callable_inner(call, args, kwargs)
            } else {
                Err(weavepy_vm::error::type_error(format!(
                    "'{}' object is not callable",
                    type_name(&other)
                )))
            }
        }
    };
    match result {
        Ok(v) => crate::object::into_owned(v),
        Err(err) => {
            install_runtime_error(err);
            ptr::null_mut()
        }
    }
}

fn invoke_callable_inner(
    target: Object,
    args: Vec<Object>,
    kwargs: Vec<(String, Object)>,
) -> Result<Object, RuntimeError> {
    match target {
        Object::Builtin(bf) => (bf.call)(&args),
        _ => {
            let r = crate::interp::with_interp_mut(|interp| {
                interp.call_object(target.clone(), &args, &kwargs)
            });
            r.unwrap_or_else(|| Err(weavepy_vm::error::runtime_error("no active interpreter")))
        }
    }
}

fn install_runtime_error(err: RuntimeError) {
    match err {
        RuntimeError::PyException(pe) => {
            let cls = match &pe.instance {
                Object::Instance(inst) => Some(inst.class.clone()),
                _ => None,
            };
            crate::errors::set_pending(cls, Object::from_str(pe.message()));
        }
        RuntimeError::Internal(msg) => {
            crate::errors::set_runtime_error(msg);
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyObject_IsTrue(o: *mut PyObject) -> c_int {
    if o.is_null() {
        return -1;
    }
    let obj = unsafe { crate::object::clone_object(o) };
    truthy(&obj).into()
}

#[no_mangle]
pub unsafe extern "C" fn PyObject_Not(o: *mut PyObject) -> c_int {
    let r = unsafe { PyObject_IsTrue(o) };
    if r < 0 {
        -1
    } else {
        c_int::from(r == 0)
    }
}

fn truthy(o: &Object) -> bool {
    use Object as O;
    match o {
        O::None => false,
        O::Bool(b) => *b,
        O::Int(i) => *i != 0,
        O::Long(b) => !(**b == num_bigint::BigInt::from(0)),
        O::Float(f) => *f != 0.0,
        O::Str(s) => !s.is_empty(),
        O::Bytes(b) => !b.is_empty(),
        O::Tuple(items) => !items.is_empty(),
        O::List(rc) => !rc.borrow().is_empty(),
        O::Dict(rc) => !rc.borrow().is_empty(),
        O::Set(rc) => !rc.borrow().is_empty(),
        _ => true,
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyObject_RichCompare(
    a: *mut PyObject,
    b: *mut PyObject,
    op: c_int,
) -> *mut PyObject {
    let r = unsafe { PyObject_RichCompareBool(a, b, op) };
    if r < 0 {
        return ptr::null_mut();
    }
    if r != 0 {
        unsafe { crate::object::Py_IncRef(crate::singletons::true_ptr()) };
        crate::singletons::true_ptr()
    } else {
        unsafe { crate::object::Py_IncRef(crate::singletons::false_ptr()) };
        crate::singletons::false_ptr()
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyObject_RichCompareBool(
    a: *mut PyObject,
    b: *mut PyObject,
    op: c_int,
) -> c_int {
    if a.is_null() || b.is_null() {
        return -1;
    }
    let oa = unsafe { crate::object::clone_object(a) };
    let ob = unsafe { crate::object::clone_object(b) };
    let cmp = compare_objects(&oa, &ob);
    match (cmp, op) {
        (Some(o), 0) => i32::from(o == std::cmp::Ordering::Less),
        (Some(o), 1) => i32::from(o != std::cmp::Ordering::Greater),
        (Some(o), 2) => i32::from(o == std::cmp::Ordering::Equal),
        (Some(o), 3) => i32::from(o != std::cmp::Ordering::Equal),
        (Some(o), 4) => i32::from(o == std::cmp::Ordering::Greater),
        (Some(o), 5) => i32::from(o != std::cmp::Ordering::Less),
        // Equality without ordering: 2/3 do object equality.
        (None, 2) => i32::from(oa.eq_value(&ob)),
        (None, 3) => i32::from(!oa.eq_value(&ob)),
        _ => -1,
    }
}

fn compare_objects(a: &Object, b: &Object) -> Option<std::cmp::Ordering> {
    use Object as O;
    match (a, b) {
        (O::Int(x), O::Int(y)) => Some(x.cmp(y)),
        (O::Float(x), O::Float(y)) => x.partial_cmp(y),
        (O::Str(x), O::Str(y)) => Some(x.as_ref().cmp(y.as_ref())),
        (O::Bytes(x), O::Bytes(y)) => Some(x.cmp(y)),
        (O::Bool(x), O::Bool(y)) => Some(x.cmp(y)),
        (O::Long(x), O::Long(y)) => Some(x.cmp(y)),
        (O::Int(x), O::Float(y)) | (O::Float(y), O::Int(x)) => (*x as f64).partial_cmp(y),
        _ => None,
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyObject_Hash(o: *mut PyObject) -> PyHashT {
    if o.is_null() {
        return -1;
    }
    use std::hash::{Hash, Hasher};
    let obj = unsafe { crate::object::clone_object(o) };
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    DictKey(obj).hash(&mut hasher);
    let h = hasher.finish() as PyHashT;
    if h == -1 {
        -2
    } else {
        h
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyObject_Type(o: *mut PyObject) -> *mut PyObject {
    if o.is_null() {
        return ptr::null_mut();
    }
    let head = unsafe { &*o };
    let ty = head.ob_type;
    if ty.is_null() {
        return ptr::null_mut();
    }
    unsafe { crate::object::Py_IncRef(ty as *mut PyObject) };
    ty as *mut PyObject
}

#[no_mangle]
pub unsafe extern "C" fn PyObject_IsInstance(o: *mut PyObject, cls: *mut PyObject) -> c_int {
    if o.is_null() || cls.is_null() {
        return 0;
    }
    let ob = unsafe { crate::object::clone_object(o) };
    let class = match unsafe { crate::object::clone_object(cls) } {
        Object::Type(t) => t,
        _ => return 0,
    };
    let actual = match &ob {
        Object::Instance(inst) => Some(inst.class.clone()),
        Object::Type(_) => Some(weavepy_vm::builtin_types::builtin_types().type_.clone()),
        _ => weavepy_vm::builtin_types::builtin_types()
            .by_name(type_name(&ob))
            .clone(),
    };
    actual.map_or(0, |a| i32::from(a.is_subclass_of(&class)))
}

#[no_mangle]
pub unsafe extern "C" fn PyObject_IsSubclass(o: *mut PyObject, cls: *mut PyObject) -> c_int {
    if o.is_null() || cls.is_null() {
        return 0;
    }
    let oa = match unsafe { crate::object::clone_object(o) } {
        Object::Type(t) => t,
        _ => return 0,
    };
    let oc = match unsafe { crate::object::clone_object(cls) } {
        Object::Type(t) => t,
        _ => return 0,
    };
    i32::from(oa.is_subclass_of(&oc))
}

#[no_mangle]
pub unsafe extern "C" fn PyObject_Length(o: *mut PyObject) -> PySsizeT {
    if o.is_null() {
        return -1;
    }
    let obj = unsafe { crate::object::clone_object(o) };
    sequence_len(&obj).unwrap_or(-1)
}

#[no_mangle]
pub unsafe extern "C" fn PyObject_Size(o: *mut PyObject) -> PySsizeT {
    unsafe { PyObject_Length(o) }
}

fn sequence_len(o: &Object) -> Option<PySsizeT> {
    use Object as O;
    Some(match o {
        O::Str(s) => s.chars().count() as PySsizeT,
        O::Bytes(b) => b.len() as PySsizeT,
        O::Tuple(items) => items.len() as PySsizeT,
        O::List(rc) => rc.borrow().len() as PySsizeT,
        O::Dict(rc) => rc.borrow().len() as PySsizeT,
        O::Set(rc) => rc.borrow().len() as PySsizeT,
        O::FrozenSet(s) => s.len() as PySsizeT,
        _ => return None,
    })
}

#[no_mangle]
pub unsafe extern "C" fn PyObject_GetItem(o: *mut PyObject, key: *mut PyObject) -> *mut PyObject {
    if o.is_null() || key.is_null() {
        return ptr::null_mut();
    }
    let obj = unsafe { crate::object::clone_object(o) };
    let k = unsafe { crate::object::clone_object(key) };
    match get_item(&obj, &k) {
        Ok(v) => crate::object::into_owned(v),
        Err(err) => {
            install_runtime_error(err);
            ptr::null_mut()
        }
    }
}

fn get_item(o: &Object, k: &Object) -> Result<Object, RuntimeError> {
    use Object as O;
    match o {
        O::Dict(rc) => rc
            .borrow()
            .get(&DictKey(k.clone()))
            .cloned()
            .ok_or_else(|| weavepy_vm::error::key_error(format!("{k:?}"))),
        O::List(rc) => match k {
            O::Int(i) => rc
                .borrow()
                .get(*i as usize)
                .cloned()
                .ok_or_else(|| weavepy_vm::error::index_error("list index out of range")),
            _ => Err(weavepy_vm::error::type_error(
                "list indices must be integers",
            )),
        },
        O::Tuple(items) => match k {
            O::Int(i) => items
                .get(*i as usize)
                .cloned()
                .ok_or_else(|| weavepy_vm::error::index_error("tuple index out of range")),
            _ => Err(weavepy_vm::error::type_error(
                "tuple indices must be integers",
            )),
        },
        O::Str(s) => match k {
            O::Int(i) => {
                let idx = *i as usize;
                s.chars()
                    .nth(idx)
                    .map(|c| Object::from_str(c.to_string()))
                    .ok_or_else(|| weavepy_vm::error::index_error("string index out of range"))
            }
            _ => Err(weavepy_vm::error::type_error(
                "string indices must be integers",
            )),
        },
        _ => Err(weavepy_vm::error::type_error("object is not subscriptable")),
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyObject_SetItem(
    o: *mut PyObject,
    key: *mut PyObject,
    v: *mut PyObject,
) -> c_int {
    if o.is_null() || key.is_null() {
        return -1;
    }
    let obj = unsafe { crate::object::clone_object(o) };
    let k = unsafe { crate::object::clone_object(key) };
    let val = if v.is_null() {
        return unsafe { PyObject_DelItem(o, key) };
    } else {
        unsafe { crate::object::clone_object(v) }
    };
    match obj {
        Object::Dict(rc) => {
            rc.borrow_mut().insert(DictKey(k), val);
            0
        }
        Object::List(rc) => match k {
            Object::Int(i) => {
                let idx = i as usize;
                let mut g = rc.borrow_mut();
                if idx < g.len() {
                    g[idx] = val;
                    0
                } else {
                    crate::errors::set_value_error("list assignment index out of range");
                    -1
                }
            }
            _ => {
                crate::errors::set_type_error("list indices must be integers");
                -1
            }
        },
        _ => {
            crate::errors::set_type_error("object does not support item assignment");
            -1
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyObject_DelItem(o: *mut PyObject, key: *mut PyObject) -> c_int {
    if o.is_null() || key.is_null() {
        return -1;
    }
    let obj = unsafe { crate::object::clone_object(o) };
    let k = unsafe { crate::object::clone_object(key) };
    match obj {
        Object::Dict(rc) => {
            if rc.borrow_mut().shift_remove(&DictKey(k)).is_some() {
                0
            } else {
                crate::errors::set_value_error("KeyError");
                -1
            }
        }
        _ => -1,
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyObject_Dir(o: *mut PyObject) -> *mut PyObject {
    if o.is_null() {
        return ptr::null_mut();
    }
    let obj = unsafe { crate::object::clone_object(o) };
    let mut keys: Vec<String> = match &obj {
        Object::Module(m) => m.dict.borrow().keys().map(|k| key_string(&k.0)).collect(),
        Object::Dict(rc) => rc.borrow().keys().map(|k| key_string(&k.0)).collect(),
        Object::Type(t) => t.dict.borrow().keys().map(|k| key_string(&k.0)).collect(),
        Object::Instance(inst) => inst
            .dict
            .borrow()
            .keys()
            .map(|k| key_string(&k.0))
            .collect(),
        _ => Vec::new(),
    };
    keys.sort();
    crate::object::into_owned(Object::new_list(
        keys.into_iter().map(Object::from_str).collect(),
    ))
}

#[no_mangle]
pub unsafe extern "C" fn PyObject_GetIter(o: *mut PyObject) -> *mut PyObject {
    if o.is_null() {
        return ptr::null_mut();
    }
    let obj = unsafe { crate::object::clone_object(o) };
    let r = crate::interp::with_interp_mut(|interp| interp.iter_object(obj));
    match r {
        Some(Ok(it)) => crate::object::into_owned(it),
        Some(Err(err)) => {
            install_runtime_error(err);
            ptr::null_mut()
        }
        None => {
            crate::errors::set_runtime_error("PyObject_GetIter: no active interpreter");
            ptr::null_mut()
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyIter_Next(it: *mut PyObject) -> *mut PyObject {
    if it.is_null() {
        return ptr::null_mut();
    }
    let obj = unsafe { crate::object::clone_object(it) };
    let r = crate::interp::with_interp_mut(|interp| interp.iter_next_object(obj));
    match r {
        Some(Ok(Some(v))) => crate::object::into_owned(v),
        Some(Ok(None)) => ptr::null_mut(),
        Some(Err(err)) => {
            install_runtime_error(err);
            ptr::null_mut()
        }
        None => ptr::null_mut(),
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyIter_NextItem(it: *mut PyObject, finished: *mut c_int) -> *mut PyObject {
    if !finished.is_null() {
        unsafe {
            *finished = 0;
        }
    }
    let r = unsafe { PyIter_Next(it) };
    if r.is_null() && !finished.is_null() {
        if crate::errors::pending().is_none() {
            unsafe {
                *finished = 1;
            }
        }
    }
    r
}

// ----------------------------------------------------------------
// PyNumber_*
// ----------------------------------------------------------------

fn binop(a: *mut PyObject, b: *mut PyObject, op: BinOp) -> *mut PyObject {
    if a.is_null() || b.is_null() {
        return ptr::null_mut();
    }
    let oa = unsafe { crate::object::clone_object(a) };
    let ob = unsafe { crate::object::clone_object(b) };
    match apply_binop(&oa, &ob, op) {
        Some(v) => crate::object::into_owned(v),
        None => {
            crate::errors::set_type_error(format!("unsupported operand type for {op:?}"));
            ptr::null_mut()
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum BinOp {
    Add,
    Sub,
    Mul,
    TrueDiv,
    FloorDiv,
    Rem,
    Pow,
}

fn apply_binop(a: &Object, b: &Object, op: BinOp) -> Option<Object> {
    use Object as O;
    match (a, b) {
        (O::Int(x), O::Int(y)) => Some(match op {
            BinOp::Add => O::Int(x.wrapping_add(*y)),
            BinOp::Sub => O::Int(x.wrapping_sub(*y)),
            BinOp::Mul => O::Int(x.wrapping_mul(*y)),
            BinOp::TrueDiv => {
                if *y == 0 {
                    return None;
                }
                O::Float(*x as f64 / *y as f64)
            }
            BinOp::FloorDiv => {
                if *y == 0 {
                    return None;
                }
                O::Int(x.div_euclid(*y))
            }
            BinOp::Rem => {
                if *y == 0 {
                    return None;
                }
                O::Int(x.rem_euclid(*y))
            }
            BinOp::Pow => O::Int((*x).pow((*y).max(0) as u32)),
        }),
        (O::Float(x), O::Float(y)) => Some(match op {
            BinOp::Add => O::Float(x + y),
            BinOp::Sub => O::Float(x - y),
            BinOp::Mul => O::Float(x * y),
            BinOp::TrueDiv | BinOp::FloorDiv => O::Float(x / y),
            BinOp::Rem => O::Float(x.rem_euclid(*y)),
            BinOp::Pow => O::Float(x.powf(*y)),
        }),
        (O::Float(x), O::Int(y)) => apply_binop(&O::Float(*x), &O::Float(*y as f64), op),
        (O::Int(x), O::Float(y)) => apply_binop(&O::Float(*x as f64), &O::Float(*y), op),
        (O::Str(x), O::Str(y)) if matches!(op, BinOp::Add) => {
            let mut s = String::with_capacity(x.len() + y.len());
            s.push_str(x);
            s.push_str(y);
            Some(O::from_str(s))
        }
        _ => None,
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyNumber_Add(a: *mut PyObject, b: *mut PyObject) -> *mut PyObject {
    binop(a, b, BinOp::Add)
}

#[no_mangle]
pub unsafe extern "C" fn PyNumber_Subtract(a: *mut PyObject, b: *mut PyObject) -> *mut PyObject {
    binop(a, b, BinOp::Sub)
}

#[no_mangle]
pub unsafe extern "C" fn PyNumber_Multiply(a: *mut PyObject, b: *mut PyObject) -> *mut PyObject {
    binop(a, b, BinOp::Mul)
}

#[no_mangle]
pub unsafe extern "C" fn PyNumber_TrueDivide(a: *mut PyObject, b: *mut PyObject) -> *mut PyObject {
    binop(a, b, BinOp::TrueDiv)
}

#[no_mangle]
pub unsafe extern "C" fn PyNumber_FloorDivide(a: *mut PyObject, b: *mut PyObject) -> *mut PyObject {
    binop(a, b, BinOp::FloorDiv)
}

#[no_mangle]
pub unsafe extern "C" fn PyNumber_Remainder(a: *mut PyObject, b: *mut PyObject) -> *mut PyObject {
    binop(a, b, BinOp::Rem)
}

#[no_mangle]
pub unsafe extern "C" fn PyNumber_Power(
    a: *mut PyObject,
    b: *mut PyObject,
    _mod_: *mut PyObject,
) -> *mut PyObject {
    binop(a, b, BinOp::Pow)
}

#[no_mangle]
pub unsafe extern "C" fn PyNumber_Negative(o: *mut PyObject) -> *mut PyObject {
    if o.is_null() {
        return ptr::null_mut();
    }
    match unsafe { crate::object::clone_object(o) } {
        Object::Int(i) => crate::object::into_owned(Object::Int(-i)),
        Object::Float(f) => crate::object::into_owned(Object::Float(-f)),
        Object::Long(b) => crate::object::into_owned(Object::Long(Rc::new((*b).clone() * -1))),
        _ => ptr::null_mut(),
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyNumber_Positive(o: *mut PyObject) -> *mut PyObject {
    if o.is_null() {
        return ptr::null_mut();
    }
    let obj = unsafe { crate::object::clone_object(o) };
    crate::object::into_owned(obj)
}

#[no_mangle]
pub unsafe extern "C" fn PyNumber_Absolute(o: *mut PyObject) -> *mut PyObject {
    if o.is_null() {
        return ptr::null_mut();
    }
    match unsafe { crate::object::clone_object(o) } {
        Object::Int(i) => crate::object::into_owned(Object::Int(i.abs())),
        Object::Float(f) => crate::object::into_owned(Object::Float(f.abs())),
        Object::Long(b) => {
            let abs = if b.sign() == num_bigint::Sign::Minus {
                (*b).clone() * -1
            } else {
                (*b).clone()
            };
            crate::object::into_owned(Object::Long(Rc::new(abs)))
        }
        _ => ptr::null_mut(),
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyNumber_Long(o: *mut PyObject) -> *mut PyObject {
    if o.is_null() {
        return ptr::null_mut();
    }
    match unsafe { crate::object::clone_object(o) } {
        Object::Int(i) => crate::object::into_owned(Object::Int(i)),
        Object::Bool(b) => crate::object::into_owned(Object::Int(i64::from(b))),
        Object::Float(f) => crate::object::into_owned(Object::Int(f.trunc() as i64)),
        Object::Long(big) => crate::object::into_owned(Object::Long(big)),
        Object::Str(s) => match s.parse::<i64>() {
            Ok(v) => crate::object::into_owned(Object::Int(v)),
            Err(_) => {
                crate::errors::set_value_error("invalid literal for int()");
                ptr::null_mut()
            }
        },
        _ => {
            crate::errors::set_type_error("cannot convert to int");
            ptr::null_mut()
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyNumber_Float(o: *mut PyObject) -> *mut PyObject {
    if o.is_null() {
        return ptr::null_mut();
    }
    let v = unsafe { crate::numbers::PyFloat_AsDouble(o) };
    if v == -1.0 && crate::errors::pending().is_some() {
        return ptr::null_mut();
    }
    crate::object::into_owned(Object::Float(v))
}

#[no_mangle]
pub unsafe extern "C" fn PyNumber_Check(o: *mut PyObject) -> c_int {
    if o.is_null() {
        return 0;
    }
    matches!(
        unsafe { crate::object::clone_object(o) },
        Object::Int(_) | Object::Long(_) | Object::Float(_) | Object::Bool(_) | Object::Complex(_)
    )
    .into()
}

// ----------------------------------------------------------------
// PySequence_*
// ----------------------------------------------------------------

#[no_mangle]
pub unsafe extern "C" fn PySequence_Check(o: *mut PyObject) -> c_int {
    if o.is_null() {
        return 0;
    }
    matches!(
        unsafe { crate::object::clone_object(o) },
        Object::List(_) | Object::Tuple(_) | Object::Str(_) | Object::Bytes(_)
    )
    .into()
}

#[no_mangle]
pub unsafe extern "C" fn PySequence_Length(o: *mut PyObject) -> PySsizeT {
    unsafe { PyObject_Length(o) }
}

#[no_mangle]
pub unsafe extern "C" fn PySequence_Size(o: *mut PyObject) -> PySsizeT {
    unsafe { PyObject_Length(o) }
}

#[no_mangle]
pub unsafe extern "C" fn PySequence_GetItem(o: *mut PyObject, i: PySsizeT) -> *mut PyObject {
    if o.is_null() {
        return ptr::null_mut();
    }
    let key = crate::object::into_owned(Object::Int(i as i64));
    let result = unsafe { PyObject_GetItem(o, key) };
    unsafe { crate::object::Py_DecRef(key) };
    result
}

#[no_mangle]
pub unsafe extern "C" fn PySequence_SetItem(
    o: *mut PyObject,
    i: PySsizeT,
    v: *mut PyObject,
) -> c_int {
    if o.is_null() {
        return -1;
    }
    let key = crate::object::into_owned(Object::Int(i as i64));
    let result = unsafe { PyObject_SetItem(o, key, v) };
    unsafe { crate::object::Py_DecRef(key) };
    result
}

#[no_mangle]
pub unsafe extern "C" fn PySequence_Contains(o: *mut PyObject, v: *mut PyObject) -> c_int {
    if o.is_null() || v.is_null() {
        return -1;
    }
    let obj = unsafe { crate::object::clone_object(o) };
    let needle = unsafe { crate::object::clone_object(v) };
    match obj {
        Object::List(rc) => i32::from(rc.borrow().iter().any(|x| x.eq_value(&needle))),
        Object::Tuple(items) => i32::from(items.iter().any(|x| x.eq_value(&needle))),
        Object::Str(s) => match needle {
            Object::Str(n) => i32::from(s.contains(n.as_ref())),
            _ => 0,
        },
        Object::Set(rc) => i32::from(rc.borrow().contains(&DictKey(needle))),
        Object::FrozenSet(s) => i32::from(s.contains(&DictKey(needle))),
        _ => -1,
    }
}

#[no_mangle]
pub unsafe extern "C" fn PySequence_List(o: *mut PyObject) -> *mut PyObject {
    if o.is_null() {
        return ptr::null_mut();
    }
    let obj = unsafe { crate::object::clone_object(o) };
    match obj {
        Object::List(rc) => crate::object::into_owned(Object::new_list(rc.borrow().clone())),
        Object::Tuple(items) => {
            crate::object::into_owned(Object::new_list(items.iter().cloned().collect()))
        }
        _ => crate::object::into_owned(Object::new_list(Vec::new())),
    }
}

#[no_mangle]
pub unsafe extern "C" fn PySequence_Tuple(o: *mut PyObject) -> *mut PyObject {
    if o.is_null() {
        return ptr::null_mut();
    }
    let obj = unsafe { crate::object::clone_object(o) };
    match obj {
        Object::List(rc) => crate::object::into_owned(Object::new_tuple(rc.borrow().clone())),
        Object::Tuple(items) => crate::object::into_owned(Object::Tuple(items)),
        _ => crate::object::into_owned(Object::new_tuple(Vec::new())),
    }
}

// ----------------------------------------------------------------
// PyMapping_*
// ----------------------------------------------------------------

#[no_mangle]
pub unsafe extern "C" fn PyMapping_Check(o: *mut PyObject) -> c_int {
    if o.is_null() {
        return 0;
    }
    matches!(unsafe { crate::object::clone_object(o) }, Object::Dict(_)).into()
}

#[no_mangle]
pub unsafe extern "C" fn PyMapping_Length(o: *mut PyObject) -> PySsizeT {
    unsafe { PyObject_Length(o) }
}

#[no_mangle]
pub unsafe extern "C" fn PyMapping_Size(o: *mut PyObject) -> PySsizeT {
    unsafe { PyObject_Length(o) }
}

#[no_mangle]
pub unsafe extern "C" fn PyMapping_GetItemString(
    o: *mut PyObject,
    key: *const c_char,
) -> *mut PyObject {
    if o.is_null() || key.is_null() {
        return ptr::null_mut();
    }
    let k = crate::object::into_owned(Object::from_str(
        unsafe { CStr::from_ptr(key) }
            .to_string_lossy()
            .into_owned(),
    ));
    let result = unsafe { PyObject_GetItem(o, k) };
    unsafe { crate::object::Py_DecRef(k) };
    result
}

#[no_mangle]
pub unsafe extern "C" fn PyMapping_HasKey(o: *mut PyObject, key: *mut PyObject) -> c_int {
    if o.is_null() || key.is_null() {
        return 0;
    }
    let p = unsafe { PyObject_GetItem(o, key) };
    if p.is_null() {
        crate::errors::clear_thread_local();
        0
    } else {
        unsafe { crate::object::Py_DecRef(p) };
        1
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyMapping_HasKeyString(o: *mut PyObject, key: *const c_char) -> c_int {
    if o.is_null() || key.is_null() {
        return 0;
    }
    let k = crate::object::into_owned(Object::from_str(
        unsafe { CStr::from_ptr(key) }
            .to_string_lossy()
            .into_owned(),
    ));
    let result = unsafe { PyMapping_HasKey(o, k) };
    unsafe { crate::object::Py_DecRef(k) };
    result
}

#[no_mangle]
pub unsafe extern "C" fn PyMapping_SetItemString(
    o: *mut PyObject,
    key: *const c_char,
    v: *mut PyObject,
) -> c_int {
    if o.is_null() || key.is_null() {
        return -1;
    }
    let k = crate::object::into_owned(Object::from_str(
        unsafe { CStr::from_ptr(key) }
            .to_string_lossy()
            .into_owned(),
    ));
    let result = unsafe { PyObject_SetItem(o, k, v) };
    unsafe { crate::object::Py_DecRef(k) };
    result
}

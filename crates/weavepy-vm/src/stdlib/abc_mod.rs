//! The `_abc` accelerator module — RFC 0023.
//!
//! Backs `abc.ABCMeta` with the registry of virtual subclasses and
//! the abstractmethod cache. Surface mirrors CPython's `_abc`:
//! `get_cache_token`, `_abc_init`, `_abc_register`, `_abc_instancecheck`,
//! `_abc_subclasscheck`, `_get_dump`, `_reset_registry`,
//! `_reset_caches`.

use crate::sync::Rc;
use crate::sync::RefCell;

use crate::error::RuntimeError;
use crate::import::ModuleCache;
use crate::object::{BuiltinFn, DictData, DictKey, Object, PyModule};

thread_local! {
    static CACHE_TOKEN: RefCell<u64> = const { RefCell::new(1) };
}

pub fn build(_cache: &ModuleCache) -> Rc<PyModule> {
    let dict = Rc::new(RefCell::new(DictData::new()));
    {
        let mut d = dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("__name__")),
            Object::from_static("_abc"),
        );
        for (name, fn_) in [
            (
                "get_cache_token",
                abc_get_cache_token as fn(&[Object]) -> Result<Object, RuntimeError>,
            ),
            ("_abc_init", abc_init),
            ("_abc_register", abc_register),
            ("_abc_instancecheck", abc_instancecheck),
            ("_abc_subclasscheck", abc_subclasscheck),
            ("_get_dump", abc_get_dump),
            ("_reset_registry", abc_reset_registry),
            ("_reset_caches", abc_reset_caches),
        ] {
            d.insert(
                DictKey(Object::from_static(name)),
                Object::Builtin(Rc::new(BuiltinFn {
                    name,
                    binds_instance: false,
                    call: Box::new(fn_),
                    call_kw: None,
                })),
            );
        }
    }
    Rc::new(PyModule {
        name: "_abc".to_owned(),
        filename: None,
        dict,
    })
}

fn bump_cache() {
    CACHE_TOKEN.with(|c| {
        let mut g = c.borrow_mut();
        *g = g.wrapping_add(1);
    });
}

fn abc_get_cache_token(_args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(Object::Int(CACHE_TOKEN.with(|c| *c.borrow() as i64)))
}

fn abc_init(args: &[Object]) -> Result<Object, RuntimeError> {
    // _abc_init(cls) — initialise the registry / cache on the class.
    if let Some(Object::Type(cls)) = args.first() {
        // CPython's `_abc_init` consumes `__abc_tpflags__`: it validates
        // that a class doesn't claim both `Py_TPFLAGS_SEQUENCE` and
        // `Py_TPFLAGS_MAPPING`, folds the collection bits into
        // `tp_flags` (we keep them under a private dict key that
        // `flags_bits` reads), and deletes the public attribute.
        const COLLECTION_FLAGS: i64 = (1 << 5) | (1 << 6);
        let tpflags = cls
            .dict
            .borrow()
            .get(&DictKey(Object::from_static("__abc_tpflags__")))
            .cloned();
        if let Some(flags) = tpflags {
            if let Some(val) = flags.as_i64() {
                if (val & COLLECTION_FLAGS) == COLLECTION_FLAGS {
                    return Err(crate::error::type_error(
                        "__abc_tpflags__ cannot be both Py_TPFLAGS_SEQUENCE and Py_TPFLAGS_MAPPING",
                    ));
                }
                cls.dict.borrow_mut().insert(
                    DictKey(Object::from_static("_abc_collection_flags")),
                    Object::Int(val & COLLECTION_FLAGS),
                );
            }
            cls.dict
                .borrow_mut()
                .shift_remove(&DictKey(Object::from_static("__abc_tpflags__")));
        }
        let mut td = cls.dict.borrow_mut();
        td.insert(
            DictKey(Object::from_static("_abc_registry")),
            Object::new_set(),
        );
        td.insert(
            DictKey(Object::from_static("_abc_cache")),
            Object::new_set(),
        );
        td.insert(
            DictKey(Object::from_static("_abc_negative_cache")),
            Object::new_set(),
        );
        td.insert(
            DictKey(Object::from_static("_abc_negative_cache_version")),
            Object::Int(CACHE_TOKEN.with(|c| *c.borrow() as i64)),
        );
    }
    Ok(Object::None)
}

fn abc_register(args: &[Object]) -> Result<Object, RuntimeError> {
    let cls = args.first().cloned().unwrap_or(Object::None);
    let sub = args.get(1).cloned().unwrap_or(Object::None);
    if let Object::Type(t) = &cls {
        if let Some(Object::Set(reg)) = t
            .dict
            .borrow()
            .get(&DictKey(Object::from_static("_abc_registry")))
            .cloned()
        {
            reg.borrow_mut().insert(DictKey(sub.clone()));
        }
    }
    bump_cache();
    Ok(sub)
}

fn abc_instancecheck(args: &[Object]) -> Result<Object, RuntimeError> {
    // Delegates to issubclass(type(obj), cls) — the Python wrapper
    // dispatches the full protocol.
    let cls = args.first().cloned().unwrap_or(Object::None);
    let inst = args.get(1).cloned().unwrap_or(Object::None);
    if let (Object::Type(t), Object::Instance(i)) = (&cls, &inst) {
        if i.cls().is_subclass_of(t) {
            return Ok(Object::Bool(true));
        }
        if let Some(Object::Set(reg)) = t
            .dict
            .borrow()
            .get(&DictKey(Object::from_static("_abc_registry")))
            .cloned()
        {
            for entry in reg.borrow().iter() {
                if let Object::Type(et) = &entry.0 {
                    if i.cls().is_subclass_of(et) {
                        return Ok(Object::Bool(true));
                    }
                }
            }
        }
    }
    Ok(Object::Bool(false))
}

fn abc_subclasscheck(args: &[Object]) -> Result<Object, RuntimeError> {
    let cls = args.first().cloned().unwrap_or(Object::None);
    let sub = args.get(1).cloned().unwrap_or(Object::None);
    if let (Object::Type(t), Object::Type(st)) = (&cls, &sub) {
        if st.is_subclass_of(t) {
            return Ok(Object::Bool(true));
        }
        if let Some(Object::Set(reg)) = t
            .dict
            .borrow()
            .get(&DictKey(Object::from_static("_abc_registry")))
            .cloned()
        {
            for entry in reg.borrow().iter() {
                if let Object::Type(et) = &entry.0 {
                    if st.is_subclass_of(et) {
                        return Ok(Object::Bool(true));
                    }
                }
            }
        }
    }
    Ok(Object::Bool(false))
}

fn abc_get_dump(args: &[Object]) -> Result<Object, RuntimeError> {
    let cls = args.first().cloned().unwrap_or(Object::None);
    if let Object::Type(t) = cls {
        let reg = t
            .dict
            .borrow()
            .get(&DictKey(Object::from_static("_abc_registry")))
            .cloned()
            .unwrap_or(Object::new_set());
        return Ok(Object::new_tuple(vec![
            reg,
            Object::new_set(),
            Object::new_set(),
            Object::Int(0),
        ]));
    }
    Ok(Object::new_tuple(vec![
        Object::new_set(),
        Object::new_set(),
        Object::new_set(),
        Object::Int(0),
    ]))
}

fn abc_reset_registry(args: &[Object]) -> Result<Object, RuntimeError> {
    let _ = args;
    bump_cache();
    Ok(Object::None)
}

fn abc_reset_caches(args: &[Object]) -> Result<Object, RuntimeError> {
    let _ = args;
    bump_cache();
    Ok(Object::None)
}

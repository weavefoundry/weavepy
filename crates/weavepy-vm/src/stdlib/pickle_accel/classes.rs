//! Type-metadata checks for instances that use the default object reduction.
//!
//! Nothing here invokes Python code. Every lookup is a native probe of a
//! class dictionary, a module dictionary, or `sys.modules`; any shape that
//! could require a Python callback reports `None` so that the caller returns
//! to the full pickler or unpickler.

use std::hash::BuildHasher;

use crate::builtin_types::builtin_types;
use crate::object::{py_str_hash, DictData, Object, StrKey};
use crate::sync::{Rc, RefCell};
use crate::types::TypeObject;

/// Hooks that replace the default reduction, plus the attribute protocol
/// overrides through which the Python pickler could reach user code.
pub(super) const ENCODE_HOOKS: &[&str] = &[
    "__reduce_ex__",
    "__reduce__",
    "__getstate__",
    "__getnewargs_ex__",
    "__getnewargs__",
    "__new__",
    "__getattribute__",
    "__getattr__",
    "__class__",
];

/// Hooks that `NEWOBJ` and `BUILD` would call in the Python unpickler.
/// A finalizer is included so that an abandoned decode cannot run one.
pub(super) const DECODE_HOOKS: &[&str] = &[
    "__new__",
    "__setstate__",
    "__getattribute__",
    "__getattr__",
    "__setattr__",
    "__del__",
    "__class__",
];

const METACLASS_HOOKS: &[&str] = &[
    "__getattribute__",
    "__getattr__",
    "__setattr__",
    "__delattr__",
    "__hash__",
    "__eq__",
    "__get__",
    "__set__",
    "__delete__",
];

/// Process-wide state under which native class handling is never attempted:
/// class dictionaries with keys whose comparison can run Python code, audit
/// hooks that observe `pickle.find_class`, and dictionary or type watchers.
pub(super) fn ambient_state_is_plain() -> bool {
    !crate::object::exotic_str_keys_possible()
        && !crate::trace::any_audit_active()
        && !crate::capi_watchers::types_active()
        && !crate::capi_watchers::dicts_active()
}

/// Probe a string key without ever comparing against a non-string key.
///
/// The outer `None` reports a stored key that isn't an exact `str` in the
/// probe sequence. Comparing with it could call a Python `__eq__`.
pub(super) fn pure_get(dict: &DictData, name: &str) -> Option<Option<Object>> {
    use indexmap::map::raw_entry_v1::RawEntryApiV1;
    let hash = crate::fasthash::FxBuildHasher.hash_one(py_str_hash(name));
    let mut exotic = false;
    let found = dict
        .raw_entry_v1()
        .from_hash(hash, |key| match &key.0 {
            Object::Str(stored) => stored.as_ref() == name,
            Object::WStr(_) => false,
            _ => {
                exotic = true;
                false
            }
        })
        .map(|(_, value)| value.clone());
    if exotic {
        None
    } else {
        Some(found)
    }
}

fn is_heap_class(class: &TypeObject) -> bool {
    !class.flags.is_builtin
        && !class.flags.is_exception
        && class.c_ext_ptr.get() == 0
        && class.c_tp_name.get().is_none()
}

/// Accept a class whose MRO holds only ordinary heap classes and `object`,
/// none of which defines one of `hooks` or replaces the `__dict__` getset.
/// Returns whether instances have a `__dict__`.
pub(super) fn plain_layout(class: &Rc<TypeObject>, hooks: &[&str]) -> Option<bool> {
    let types = builtin_types();
    let mro = class.mro.borrow();
    let (object, heap) = mro.split_last()?;
    if !Rc::ptr_eq(object, &types.object_) || !heap.first().is_some_and(|c| Rc::ptr_eq(c, class)) {
        return None;
    }
    for entry in heap {
        if !is_heap_class(entry) {
            return None;
        }
        let dict = entry.dict.borrow();
        if hooks.iter().any(|name| dict.contains_key(&StrKey(name))) {
            return None;
        }
        match dict.get(&StrKey("__dict__")) {
            None => {}
            Some(Object::SlotDescriptor(slot)) if slot.name == "__dict__" => {}
            Some(_) => return None,
        }
    }
    Some(!class.forbids_dict)
}

/// Accept `type` itself, or a heap metaclass that cannot intercept attribute
/// access, hashing, or comparison of its classes. Every value in such a
/// metaclass must be a kind that is never a data descriptor.
pub(super) fn benign_metaclass(class: &TypeObject) -> bool {
    let types = builtin_types();
    let Some(meta) = class.metaclass.borrow().clone() else {
        return true;
    };
    if Rc::ptr_eq(&meta, &types.type_) {
        return true;
    }
    if meta
        .metaclass
        .borrow()
        .as_ref()
        .is_some_and(|outer| !Rc::ptr_eq(outer, &types.type_))
    {
        return false;
    }
    let mro = meta.mro.borrow();
    mro.iter().all(|entry| {
        if Rc::ptr_eq(entry, &types.type_) || Rc::ptr_eq(entry, &types.object_) {
            return true;
        }
        is_heap_class(entry)
            && entry.dict.borrow().iter().all(|(key, value)| {
                matches!(&key.0, Object::Str(name) if !METACLASS_HOOKS.contains(&name.as_ref()))
                    && matches!(
                        value,
                        Object::Function(_)
                            | Object::Builtin(_)
                            | Object::StaticMethod(_)
                            | Object::ClassMethod(_)
                            | Object::Str(_)
                            | Object::None
                            | Object::Bool(_)
                            | Object::Int(_)
                            | Object::Float(_)
                            | Object::Tuple(_)
                            | Object::FrozenSet(_)
                            | Object::Dict(_)
                    )
            })
    })
}

/// `getattr(sys.modules[module], ...)` along a dotted `qualname`, restricted
/// to steps that are plain dictionary hits. Module-level `__getattr__`,
/// module subclasses, descriptors, and absent modules report `None`.
pub(super) fn resolve_global(
    modules: &RefCell<DictData>,
    module: &str,
    qualname: &str,
) -> Option<Rc<TypeObject>> {
    let Object::Module(module) = pure_get(&modules.borrow(), module)?? else {
        return None;
    };
    if crate::object::module_class(&module).is_some() {
        return None;
    }
    let mut parts = qualname.split('.');
    let first = parts.next()?;
    let special = |part: &str| part.is_empty() || (part.starts_with("__") && part.ends_with("__"));
    if special(first) {
        return None;
    }
    let Object::Type(mut current) = pure_get(&module.dict.borrow(), first)?? else {
        return None;
    };
    for part in parts {
        // `<locals>` is not an identifier and is never a class attribute.
        if special(part) || !is_heap_class(&current) || !benign_metaclass(&current) {
            return None;
        }
        let Object::Type(next) = current.lookup(part)? else {
            return None;
        };
        current = next;
    }
    Some(current)
}

/// Whether `name` resolves to the member descriptor of a `__slots__` entry,
/// so that `getattr` and `setattr` reach the slot storage directly.
pub(super) fn is_member_slot(class: &TypeObject, name: &str) -> bool {
    !matches!(name, "__dict__" | "__weakref__")
        && matches!(
            class.lookup(name),
            Some(Object::SlotDescriptor(slot))
                if slot.name == name && slot.default.is_none() && !slot.readonly
        )
}

/// The names `copyreg._slotnames` computes, as the string objects it would
/// store. Unusual `__slots__` declarations report `None`.
pub(super) fn slot_names(class: &TypeObject) -> Option<Vec<Object>> {
    let mut names = Vec::new();
    for entry in class.mro.borrow().iter() {
        let dict = entry.dict.borrow();
        let Some(slots) = dict.get(&StrKey("__slots__")) else {
            continue;
        };
        // A class-level `__name__` entry could differ from the name that
        // mangled the member descriptors.
        if dict.contains_key(&StrKey("__name__")) {
            return None;
        }
        let items = match slots {
            Object::Str(_) => vec![slots.clone()],
            Object::Tuple(items) => items.to_vec(),
            Object::List(items) => items.borrow().clone(),
            _ => return None,
        };
        for item in items {
            let Object::Str(name) = &item else {
                return None;
            };
            if matches!(name.as_ref(), "__dict__" | "__weakref__") {
                return None;
            }
            let stripped = entry.name.trim_start_matches('_');
            if name.starts_with("__") && !name.ends_with("__") && !stripped.is_empty() {
                names.push(Object::from_str(format!("_{stripped}{name}")));
            } else {
                names.push(item);
            }
        }
    }
    names
        .iter()
        .all(|name| matches!(name, Object::Str(name) if is_member_slot(class, name)))
        .then_some(names)
}

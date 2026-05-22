//! WeavePy singleton values exposed in `builtins` — `NotImplemented`
//! and `Ellipsis`. CPython hands out the *same* object for every
//! reference: `a is NotImplemented` is an identity test, not a
//! comparison. We mirror that by building both once at process start
//! and serving the same `Rc` for the lifetime of the interpreter.
//!
//! Both values are modelled as bare `object()` instances backed by a
//! per-singleton anonymous type. This is enough for the comparison
//! sentinel use case (`return NotImplemented` from `__lt__` etc.) and
//! for the indexing protocol value bound to the `...` literal. We
//! don't yet wire either into the type system as `types.EllipsisType`
//! / `types.NotImplementedType`; nothing in the stdlib reaches for
//! those directly.

use std::cell::RefCell;
use std::rc::Rc;

use crate::object::{DictData, Object};
use crate::types::{PyInstance, TypeFlags, TypeObject};

thread_local! {
    static NOT_IMPLEMENTED: RefCell<Option<Object>> = const { RefCell::new(None) };
    static ELLIPSIS: RefCell<Option<Object>> = const { RefCell::new(None) };
}

fn make_singleton(name: &'static str) -> Object {
    let cls = TypeObject::new_with_flags(
        name,
        vec![],
        DictData::new(),
        TypeFlags {
            is_exception: false,
            is_builtin: true,
        },
    )
    .expect("singleton MRO");
    let instance = PyInstance::new(cls);
    Object::Instance(Rc::new(instance))
}

/// Return the unique `NotImplemented` instance, allocating it on
/// first access. Subsequent calls hand back the same `Rc`-shared
/// object so `x is NotImplemented` works.
pub fn not_implemented() -> Object {
    NOT_IMPLEMENTED.with(|slot| {
        let mut s = slot.borrow_mut();
        if let Some(v) = s.as_ref() {
            return v.clone();
        }
        let v = make_singleton("NotImplementedType");
        *s = Some(v.clone());
        v
    })
}

/// Same idea for `Ellipsis` (the value of `...`).
pub fn ellipsis() -> Object {
    ELLIPSIS.with(|slot| {
        let mut s = slot.borrow_mut();
        if let Some(v) = s.as_ref() {
            return v.clone();
        }
        let v = make_singleton("ellipsis");
        *s = Some(v.clone());
        v
    })
}

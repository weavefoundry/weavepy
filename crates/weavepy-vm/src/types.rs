//! Runtime type objects.
//!
//! Every Python value at runtime has a `type` — for built-in values
//! that mapping is computed from the [`Object`] enum tag; for
//! user-defined classes it lives directly on the instance.
//!
//! `TypeObject` itself is a Python object — `type(x)` returns one —
//! so the `Object::Type` variant carries an `Rc<TypeObject>`. The MRO
//! is C3 linearised at class-creation time and cached on the type.

use std::cell::RefCell;
use std::rc::Rc;

use crate::error::{type_error, RuntimeError};
use crate::object::{DictData, DictKey, Object};

/// A Python class.
///
/// The dict stores methods and class attributes — the same dict you
/// see as `cls.__dict__`. The MRO is precomputed at construction
/// time so attribute lookups are linear in the depth of inheritance.
pub struct TypeObject {
    pub name: String,
    pub bases: Vec<Rc<TypeObject>>,
    pub mro: RefCell<Vec<Rc<TypeObject>>>,
    pub dict: Rc<RefCell<DictData>>,
    pub flags: TypeFlags,
    /// The class's *class* — i.e. its metaclass. Defaults to `type`
    /// (set by the constructor builders). User-defined classes pick
    /// up a custom metaclass either via the `metaclass=` keyword or
    /// by inheriting the highest-priority metaclass of their bases.
    /// Wrapped in a `RefCell` so the [`crate::builtin_types`] startup
    /// path can self-reference (`type.__class__ is type`) by patching
    /// the slot after construction.
    pub metaclass: RefCell<Option<Rc<TypeObject>>>,
    /// Explicit `__slots__` declarations, in declaration order.
    /// Empty when the class does not use slots. Used at class
    /// creation to install [`crate::object::SlotDescriptor`]s, and at
    /// attribute-set time to enforce slot-only access on classes
    /// whose entire MRO declares slots.
    pub slot_names: RefCell<Vec<String>>,
    /// `True` for slot-using classes whose MRO carries `__slots__`
    /// every step of the way (so the instance has no implicit
    /// `__dict__`). Set when the user neither omits `__slots__` from
    /// any base nor lists `"__dict__"` in slots.
    pub forbids_dict: bool,
}

impl std::fmt::Debug for TypeObject {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "<class '{}'>", self.name)
    }
}

#[derive(Default, Clone, Copy, Debug)]
pub struct TypeFlags {
    /// `True` for types whose MRO contains `BaseException`.
    pub is_exception: bool,
    /// `True` for the small set of types created by the interpreter
    /// itself at startup (vs user-defined `class` statements).
    pub is_builtin: bool,
}

impl TypeObject {
    /// Construct a built-in type that inherits from `bases`. The MRO
    /// is computed via C3 linearisation.
    pub fn new_builtin(name: &str, bases: Vec<Rc<TypeObject>>) -> Result<Rc<Self>, RuntimeError> {
        Self::new_with_flags(
            name,
            bases,
            DictData::new(),
            TypeFlags {
                is_exception: false,
                is_builtin: true,
            },
        )
    }

    /// Construct a built-in exception type. Convenience wrapper.
    pub fn new_exception(name: &str, base: Rc<TypeObject>) -> Result<Rc<Self>, RuntimeError> {
        Self::new_with_flags(
            name,
            vec![base],
            DictData::new(),
            TypeFlags {
                is_exception: true,
                is_builtin: true,
            },
        )
    }

    /// Construct a user-defined class from a class statement.
    pub fn new_user(
        name: &str,
        bases: Vec<Rc<TypeObject>>,
        dict: DictData,
    ) -> Result<Rc<Self>, RuntimeError> {
        let is_exception = bases.iter().any(|b| b.flags.is_exception);
        Self::new_with_flags(
            name,
            bases,
            dict,
            TypeFlags {
                is_exception,
                is_builtin: false,
            },
        )
    }

    fn new_with_flags(
        name: &str,
        bases: Vec<Rc<TypeObject>>,
        dict: DictData,
        flags: TypeFlags,
    ) -> Result<Rc<Self>, RuntimeError> {
        let ty = Rc::new(TypeObject {
            name: name.to_owned(),
            bases: bases.clone(),
            mro: RefCell::new(Vec::new()),
            dict: Rc::new(RefCell::new(dict)),
            flags,
            metaclass: RefCell::new(None),
            slot_names: RefCell::new(Vec::new()),
            forbids_dict: false,
        });
        let mro = compute_c3(&ty, &bases, name)?;
        *ty.mro.borrow_mut() = mro;
        Ok(ty)
    }

    /// Internal: install a metaclass on this type. Used at startup
    /// to wire `type.__class__ is type` for the built-in `type`
    /// itself, and by [`crate::Vm::build_class`] when honouring the
    /// `metaclass=` keyword.
    pub fn set_metaclass(&self, meta: Rc<TypeObject>) {
        *self.metaclass.borrow_mut() = Some(meta);
    }

    /// The metaclass slot, falling back to `type` for any type that
    /// hasn't had one installed yet.
    pub fn metaclass_or_type(&self) -> Rc<TypeObject> {
        if let Some(m) = self.metaclass.borrow().as_ref() {
            return m.clone();
        }
        crate::builtin_types::builtin_types().type_.clone()
    }

    /// `True` when `self` is a subclass of `other` (including itself).
    pub fn is_subclass_of(&self, other: &TypeObject) -> bool {
        let other_ptr = std::ptr::from_ref::<TypeObject>(other);
        self.mro
            .borrow()
            .iter()
            .any(|t| std::ptr::eq(Rc::as_ptr(t), other_ptr))
    }

    /// Look up `name` in this type's MRO.
    pub fn lookup(&self, name: &str) -> Option<Object> {
        let key = DictKey(Object::from_str(name));
        for ty in self.mro.borrow().iter() {
            if let Some(v) = ty.dict.borrow().get(&key).cloned() {
                return Some(v);
            }
        }
        None
    }

    pub fn class_name(&self) -> &str {
        &self.name
    }
}

fn compute_c3(
    self_ty: &Rc<TypeObject>,
    bases: &[Rc<TypeObject>],
    name: &str,
) -> Result<Vec<Rc<TypeObject>>, RuntimeError> {
    let mut lists: Vec<Vec<Rc<TypeObject>>> =
        bases.iter().map(|b| b.mro.borrow().clone()).collect();
    lists.push(bases.to_vec());
    let mut merged: Vec<Rc<TypeObject>> = vec![self_ty.clone()];
    loop {
        lists.retain(|l| !l.is_empty());
        if lists.is_empty() {
            break;
        }
        let mut chosen: Option<Rc<TypeObject>> = None;
        for list in &lists {
            let head = &list[0];
            let head_in_other_tails = lists
                .iter()
                .any(|other| other.iter().skip(1).any(|t| Rc::ptr_eq(t, head)));
            if !head_in_other_tails {
                chosen = Some(head.clone());
                break;
            }
        }
        let Some(c) = chosen else {
            return Err(type_error(format!(
                "Cannot create a consistent method resolution order (MRO) for bases of '{name}'"
            )));
        };
        merged.push(c.clone());
        for list in &mut lists {
            if let Some(h) = list.first() {
                if Rc::ptr_eq(h, &c) {
                    list.remove(0);
                }
            }
        }
    }
    Ok(merged)
}

/// An instance of a user-defined class.
///
/// `dict` mirrors CPython's `__dict__` — attribute writes land here
/// directly without descriptor checks (the slice doesn't have data
/// descriptors yet; see RFC 0010).
#[derive(Debug, Clone)]
pub struct PyInstance {
    pub class: Rc<TypeObject>,
    pub dict: Rc<RefCell<DictData>>,
}

impl PyInstance {
    pub fn new(class: Rc<TypeObject>) -> Self {
        Self {
            class,
            dict: Rc::new(RefCell::new(DictData::new())),
        }
    }
}

//! Runtime type objects.
//!
//! Every Python value at runtime has a `type` — for built-in values
//! that mapping is computed from the [`Object`] enum tag; for
//! user-defined classes it lives directly on the instance.
//!
//! `TypeObject` itself is a Python object — `type(x)` returns one —
//! so the `Object::Type` variant carries an `Rc<TypeObject>`. The MRO
//! is C3 linearised at class-creation time and cached on the type.

use crate::sync::Cell;
use crate::sync::Rc;
use crate::sync::RefCell;
use crate::sync::Weak;

use crate::error::{type_error, RuntimeError};
use crate::object::{DictData, DictKey, Object};

/// A Python class.
///
/// The dict stores methods and class attributes — the same dict you
/// see as `cls.__dict__`. The MRO is precomputed at construction
/// time so attribute lookups are linear in the depth of inheritance.
pub struct TypeObject {
    pub name: String,
    /// PEP 3155 `__qualname__`. CPython's `type_new` *pops* the
    /// compiler-stored `__qualname__` out of the class namespace into
    /// `tp_qualname` (it is not visible in `cls.__dict__`); mirrored
    /// here. `None` falls back to `name` (dynamic `type(...)` classes).
    pub qualname: RefCell<Option<String>>,
    /// Direct bases. Mutable because CPython supports `cls.__bases__ = …`
    /// assignment (with layout/MRO validation and subclass re-resolution).
    pub bases: RefCell<Vec<Rc<TypeObject>>>,
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
    /// `True` when the class body *declared* `__slots__` (even an empty
    /// one). Distinguishes `__slots__ = []` (no `__weakref__` support
    /// contributed) from a plain class (which contributes both
    /// `__dict__` and `__weakref__`), mirroring CPython's tp_weaklistoffset
    /// computation.
    pub declares_slots: Cell<bool>,
    /// `True` for slot-using classes whose MRO carries `__slots__`
    /// every step of the way (so the instance has no implicit
    /// `__dict__`). Set when the user neither omits `__slots__` from
    /// any base nor lists `"__dict__"` in slots.
    pub forbids_dict: bool,
    /// Direct subclasses of this type, tracked as *weak* references so
    /// the parent→child edge doesn't form an uncollectable `Rc` cycle
    /// with the strong child→parent `bases` edge. Mirrors CPython's
    /// `tp_subclasses`; surfaced by `type.__subclasses__()` and used by
    /// the ABC virtual-subclass machinery.
    pub subclasses: RefCell<Vec<Weak<TypeObject>>>,
    /// Cached classification of this type's `__getattribute__` slot, so the
    /// hot attribute path can skip an MRO walk: `0` = not yet computed,
    /// `1` = default (`object.__getattribute__`), `2` = a user override.
    /// Invalidated (reset to `0`) for the type and its subclasses whenever
    /// `__getattribute__` is assigned to / deleted from a type's dict.
    pub getattribute_kind: Cell<u8>,
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

    /// Is this built-in type one whose instances "own" a distinct memory
    /// layout (CPython: `tp_basicsize`/`tp_itemsize` extended past the
    /// base)? Determines the `solid_base` used for multiple-inheritance
    /// layout-conflict checks. Plain exceptions all share
    /// `BaseException`'s layout; the listed ones add fields.
    fn owns_layout(&self) -> bool {
        if !self.flags.is_builtin || self.name == "object" {
            return false;
        }
        if self.flags.is_exception {
            return matches!(
                self.name.as_str(),
                "BaseException"
                    | "OSError"
                    | "SyntaxError"
                    | "SystemExit"
                    | "StopIteration"
                    | "ImportError"
                    | "NameError"
                    | "AttributeError"
                    | "UnicodeDecodeError"
                    | "UnicodeEncodeError"
                    | "UnicodeTranslateError"
                    | "BaseExceptionGroup"
            );
        }
        true
    }

    /// CPython's `solid_base`: the most-derived class on the MRO whose
    /// instance layout this type shares. `None` means plain `object`.
    pub fn solid_base(&self) -> Option<Rc<TypeObject>> {
        self.mro.borrow().iter().find(|t| t.owns_layout()).cloned()
    }

    /// Recompute this type's C3 linearisation from its *current* bases
    /// (used by `type.mro()` and `__bases__` assignment).
    pub fn recompute_c3(ty: &Rc<TypeObject>) -> Result<Vec<Rc<TypeObject>>, RuntimeError> {
        let bases = ty.bases.borrow().clone();
        compute_c3(ty, &bases, &ty.name)
    }

    /// CPython `type_new` base validation (`best_base`): every base must
    /// be subclassable, and the solid bases of all bases must form a
    /// single inheritance chain (no instance lay-out conflict).
    pub fn validate_bases(name: &str, bases: &[Rc<TypeObject>]) -> Result<(), RuntimeError> {
        let _ = name;
        for b in bases {
            if b.flags.is_builtin
                && matches!(
                    b.name.as_str(),
                    "bool"
                        | "NoneType"
                        | "NotImplementedType"
                        | "ellipsis"
                        | "range"
                        | "slice"
                        | "memoryview"
                        | "function"
                        | "builtin_function_or_method"
                        | "method"
                        | "generator"
                        | "coroutine"
                        | "async_generator"
                        | "frame"
                        | "traceback"
                        | "code"
                        | "cell"
                        | "mappingproxy"
                        | "weakproxy"
                        | "weakcallableproxy"
                        | "member_descriptor"
                        | "method_descriptor"
                        | "getset_descriptor"
                        | "wrapper_descriptor"
                        | "method-wrapper"
                )
            {
                return Err(type_error(format!(
                    "type '{}' is not an acceptable base type",
                    b.name
                )));
            }
        }
        let mut winner: Option<Rc<TypeObject>> = None;
        for b in bases {
            let Some(sb) = b.solid_base() else { continue };
            match &winner {
                None => winner = Some(sb),
                Some(w) => {
                    if w.is_subclass_of(&sb) {
                        // current winner already extends sb — keep it
                    } else if sb.is_subclass_of(w) {
                        winner = Some(sb);
                    } else {
                        return Err(type_error(
                            "multiple bases have instance lay-out conflict",
                        ));
                    }
                }
            }
        }
        Ok(())
    }

    /// Construct a user-defined class from a class statement.
    pub fn new_user(
        name: &str,
        bases: Vec<Rc<TypeObject>>,
        mut dict: DictData,
    ) -> Result<Rc<Self>, RuntimeError> {
        Self::validate_bases(name, &bases)?;
        let is_exception = bases.iter().any(|b| b.flags.is_exception);
        // CPython `type_new`: a class that defines `__eq__` without
        // defining `__hash__` is unhashable (`__hash__` is set to None
        // in the new class's dict).
        if dict.contains_key(&DictKey(Object::from_static("__eq__")))
            && !dict.contains_key(&DictKey(Object::from_static("__hash__")))
        {
            dict.insert(DictKey(Object::from_static("__hash__")), Object::None);
        }
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

    pub fn new_with_flags(
        name: &str,
        bases: Vec<Rc<TypeObject>>,
        mut dict: DictData,
        flags: TypeFlags,
    ) -> Result<Rc<Self>, RuntimeError> {
        // CPython `type_new`: `__qualname__` is removed from the class
        // namespace and stored on the type itself.
        let qualname = match dict.shift_remove(&DictKey(Object::from_static("__qualname__"))) {
            Some(Object::Str(s)) => Some(s.to_string()),
            Some(other) => {
                return Err(type_error(format!(
                    "type __qualname__ must be a str, not {}",
                    other.type_name()
                )))
            }
            None => None,
        };
        let ty = Rc::new(TypeObject {
            name: name.to_owned(),
            qualname: RefCell::new(qualname),
            bases: RefCell::new(bases.clone()),
            mro: RefCell::new(Vec::new()),
            dict: Rc::new(RefCell::new(dict)),
            flags,
            metaclass: RefCell::new(None),
            slot_names: RefCell::new(Vec::new()),
            declares_slots: Cell::new(false),
            forbids_dict: false,
            subclasses: RefCell::new(Vec::new()),
            getattribute_kind: Cell::new(0),
        });
        let mro = compute_c3(&ty, &bases, name)?;
        *ty.mro.borrow_mut() = mro;
        // Register the new class as a (weak) direct subclass of each of
        // its bases so `base.__subclasses__()` reports it.
        for base in &bases {
            base.subclasses.borrow_mut().push(Rc::downgrade(&ty));
        }
        // RFC 0024: user classes join the cycle collector. Every class
        // is born in a self-cycle (its own `mro` holds an `Rc` to
        // itself), so without tracking, `del SomeClass` could never
        // free it — and weakrefs to it (or to methods in its dict)
        // would never clear. Built-ins are immortal; skip them.
        if !ty.flags.is_builtin {
            crate::gc_trace::track(Object::Type(ty.clone()));
        }
        Ok(ty)
    }

    /// Does this type have a CPython "managed `__dict__`" — i.e. do its
    /// instances carry an attribute dict? True for user-defined classes
    /// whose MRO doesn't declare slots-without-dict the whole way down.
    pub fn has_managed_dict(&self) -> bool {
        !self.flags.is_builtin && !self.forbids_dict
    }

    /// Does this type inherit from a *variable-sized* built-in
    /// (`tp_itemsize != 0` in CPython: `int`, `tuple`, `str`, `bytes`,
    /// `type`)? Such types get a managed dict but no inline values.
    pub fn has_var_sized_base(&self) -> bool {
        self.mro.borrow().iter().any(|t| {
            t.flags.is_builtin
                && matches!(t.name.as_str(), "int" | "tuple" | "str" | "bytes" | "type")
        })
    }

    /// The first built-in class in the MRO other than `object` — the
    /// moral equivalent of CPython's `solid_base`, which determines
    /// instance memory layout for `__class__` assignment checks.
    /// `None` for plain `object`-rooted classes.
    pub fn solid_base_name(&self) -> Option<String> {
        self.mro
            .borrow()
            .iter()
            .find(|t| t.flags.is_builtin && t.name != "object")
            .map(|t| t.name.clone())
    }

    /// CPython `type.__flags__` (`tp_flags`), computed from this type's
    /// observable properties. Covers the documented/queried bits:
    /// inline-values + managed-dict (`test_class`), heap/base/ready/gc,
    /// abstractness, and the `*_SUBCLASS` fast-classification bits.
    pub fn flags_bits(&self) -> i64 {
        const INLINE_VALUES: i64 = 1 << 2;
        const MANAGED_WEAKREF: i64 = 1 << 3;
        const MANAGED_DICT: i64 = 1 << 4;
        const IMMUTABLETYPE: i64 = 1 << 8;
        const HEAPTYPE: i64 = 1 << 9;
        const BASETYPE: i64 = 1 << 10;
        const READY: i64 = 1 << 12;
        const HAVE_GC: i64 = 1 << 14;
        const IS_ABSTRACT: i64 = 1 << 20;
        const LONG_SUBCLASS: i64 = 1 << 24;
        const LIST_SUBCLASS: i64 = 1 << 25;
        const TUPLE_SUBCLASS: i64 = 1 << 26;
        const BYTES_SUBCLASS: i64 = 1 << 27;
        const UNICODE_SUBCLASS: i64 = 1 << 28;
        const DICT_SUBCLASS: i64 = 1 << 29;
        const BASE_EXC_SUBCLASS: i64 = 1 << 30;
        const TYPE_SUBCLASS: i64 = 1 << 31;

        let mut bits = READY;
        if self.flags.is_builtin {
            bits |= IMMUTABLETYPE;
            // Built-ins that refuse subclassing.
            let is_final = matches!(
                self.name.as_str(),
                "bool"
                    | "NoneType"
                    | "NotImplementedType"
                    | "ellipsis"
                    | "range"
                    | "slice"
                    | "memoryview"
                    | "generator"
                    | "coroutine"
                    | "async_generator"
                    | "function"
                    | "builtin_function_or_method"
                    | "method_wrapper"
                    | "mappingproxy"
            );
            if !is_final {
                bits |= BASETYPE;
            }
            if matches!(
                self.name.as_str(),
                "list" | "dict" | "set" | "frozenset" | "tuple" | "type"
            ) || self.flags.is_exception
            {
                bits |= HAVE_GC;
            }
        } else {
            bits |= HEAPTYPE | BASETYPE | HAVE_GC | MANAGED_WEAKREF;
            if self.has_managed_dict() {
                bits |= MANAGED_DICT;
                if !self.has_var_sized_base() {
                    bits |= INLINE_VALUES;
                }
            }
        }
        match self
            .dict
            .borrow()
            .get(&DictKey(Object::from_static("__abstractmethods__")))
        {
            Some(Object::Set(s)) if !s.borrow().is_empty() => bits |= IS_ABSTRACT,
            Some(Object::FrozenSet(s)) if !s.is_empty() => bits |= IS_ABSTRACT,
            _ => {}
        }
        const SEQUENCE: i64 = 1 << 5;
        const MAPPING: i64 = 1 << 6;
        for t in self.mro.borrow().iter() {
            if t.flags.is_builtin {
                match t.name.as_str() {
                    "int" => bits |= LONG_SUBCLASS,
                    "list" => bits |= LIST_SUBCLASS | SEQUENCE,
                    "tuple" => bits |= TUPLE_SUBCLASS | SEQUENCE,
                    "bytes" => bits |= BYTES_SUBCLASS,
                    "str" => bits |= UNICODE_SUBCLASS,
                    "dict" => bits |= DICT_SUBCLASS | MAPPING,
                    "range" | "memoryview" | "bytearray" => bits |= SEQUENCE,
                    "mappingproxy" => bits |= MAPPING,
                    "type" => bits |= TYPE_SUBCLASS,
                    _ => {}
                }
            }
            // ABCs that declared `__abc_tpflags__` (Sequence / Mapping):
            // `_abc_init` stowed the collection bits here, and CPython
            // propagates them to subclasses through tp_flags inheritance —
            // the MRO walk reproduces that.
            if let Some(v) = t
                .dict
                .borrow()
                .get(&DictKey(Object::from_static("_abc_collection_flags")))
                .and_then(Object::as_i64)
            {
                bits |= v & (SEQUENCE | MAPPING);
            }
        }
        if self.flags.is_exception {
            bits |= BASE_EXC_SUBCLASS;
        }
        bits
    }

    /// Reset the cached `__getattribute__` classification for this type and
    /// every (transitive) subclass. Called when `__getattribute__` is
    /// assigned to or deleted from a type's dict, since that can change the
    /// resolved slot for the type *and* anything inheriting from it. Class
    /// hierarchies are acyclic, so the recursion terminates.
    pub fn invalidate_getattribute_cache(&self) {
        self.getattribute_kind.set(0);
        for sub in self.subclasses() {
            sub.invalidate_getattribute_cache();
        }
    }

    /// Live direct subclasses, in registration order. Dead weak refs
    /// (subclasses that have been dropped) are pruned as a side effect.
    pub fn subclasses(&self) -> Vec<Rc<TypeObject>> {
        let mut subs = self.subclasses.borrow_mut();
        subs.retain(|w| w.strong_count() > 0);
        subs.iter().filter_map(Weak::upgrade).collect()
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
        // Snapshot the MRO before walking it (CPython `_PyType_Lookup`
        // holds a strong reference for the same reason): a dict probe
        // can re-enter Python (`__eq__` on a non-string class-dict key)
        // and reassign `__bases__` mid-lookup. The in-flight lookup
        // must keep resolving against the *old* linearisation.
        let mro: Vec<Rc<TypeObject>> = self.mro.borrow().clone();
        for ty in mro.iter() {
            if let Some(v) = ty.dict.borrow().get(&key).cloned() {
                return Some(v);
            }
        }
        None
    }

    /// Like [`Self::lookup`], but also report the MRO entry that owns
    /// the attribute. Lets callers distinguish a dunder *supplied by a
    /// user class* from one inherited off a built-in (e.g. `object`'s
    /// identity `__hash__`).
    pub fn lookup_with_owner(&self, name: &str) -> Option<(Object, Rc<TypeObject>)> {
        let key = DictKey(Object::from_str(name));
        // Snapshot for reentrancy — see `lookup`.
        let mro: Vec<Rc<TypeObject>> = self.mro.borrow().clone();
        for ty in mro.iter() {
            if let Some(v) = ty.dict.borrow().get(&key).cloned() {
                return Some((v, ty.clone()));
            }
        }
        None
    }

    pub fn class_name(&self) -> &str {
        &self.name
    }

    /// CPython `type_repr` name: `__module__.__qualname__`, with the
    /// module prefix omitted for `builtins` (so `<class 'int'>` but
    /// `<class 'collections.abc.Iterable'>` / `<class '__main__.Foo'>`).
    pub fn qualified_display_name(&self) -> String {
        let dict = self.dict.borrow();
        // Only honour *string* entries — some built-in types carry a
        // `__qualname__`/`__module__` *property descriptor* (for their
        // instances) in the dict, which must not leak into the class
        // repr (`type(gen)` printing `<class '<property object>'>`).
        let as_str = |name: &'static str| match dict.get(&DictKey(Object::from_static(name))) {
            Some(Object::Str(s)) => Some(s.as_ref().to_owned()),
            _ => None,
        };
        let module = as_str("__module__");
        let qual = as_str("__qualname__")
            .or_else(|| self.qualname.borrow().clone())
            .unwrap_or_else(|| self.name.clone());
        match module.as_deref() {
            None | Some("builtins") | Some("") => qual,
            Some(m) => format!("{m}.{qual}"),
        }
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
    /// The instance's type. Interior-mutable because Python permits
    /// `obj.__class__ = OtherClass` for layout-compatible heap types;
    /// read through [`PyInstance::cls`].
    pub class: RefCell<Rc<TypeObject>>,
    pub dict: Rc<RefCell<DictData>>,
    /// For instances of a subclass of an immutable built-in
    /// (`int`, `str`, `float`, `bytes`, `tuple`, …) this holds the
    /// underlying primitive value the instance *is* — the moral
    /// equivalent of CPython storing the C-level value in the object
    /// struct. `None` for ordinary objects. Set once at construction
    /// (the wrapped builtins are themselves immutable) and unwrapped
    /// by the numeric / comparison / hashing / conversion fast paths
    /// so e.g. `class C(int)` instances behave like real ints.
    pub native: Option<Object>,
    /// Mirrors CPython 3.13's "inline values" state observable through
    /// `_testinternalcapi.has_inline_values`: starts `true` and is
    /// permanently cleared when the instance's `__dict__` is deleted or
    /// replaced wholesale (`del obj.__dict__` / `obj.__dict__ = d`).
    /// The capacity-overflow half of the state (too many attributes)
    /// is computed at query time from the dict size.
    pub inline_values: Cell<bool>,
    /// `__slots__` storage. CPython lays slot values out as C struct
    /// members *outside* the instance `__dict__`; we mirror that
    /// separation with a side table so `vars(obj)` never exposes slot
    /// values and `object.__getstate__` can report them separately.
    /// `None` until the first slot write (most instances have none).
    pub slots: RefCell<Option<DictData>>,
}

impl PyInstance {
    pub fn new(class: Rc<TypeObject>) -> Self {
        Self {
            class: RefCell::new(class),
            dict: Rc::new(RefCell::new(DictData::new())),
            native: None,
            inline_values: Cell::new(true),
            slots: RefCell::new(None),
        }
    }

    /// Build an instance that wraps a primitive `native` value
    /// (subclass of `int`/`str`/…).
    pub fn with_native(class: Rc<TypeObject>, native: Object) -> Self {
        Self {
            class: RefCell::new(class),
            dict: Rc::new(RefCell::new(DictData::new())),
            native: Some(native),
            inline_values: Cell::new(true),
            slots: RefCell::new(None),
        }
    }

    /// The instance's current class (honours `__class__` assignment).
    #[inline]
    pub fn cls(&self) -> Rc<TypeObject> {
        self.class.borrow().clone()
    }

    /// Re-point the instance at a new class (`obj.__class__ = C`).
    pub fn set_cls(&self, class: Rc<TypeObject>) {
        *self.class.borrow_mut() = class;
    }

    /// Read slot `name` from the side table (a `__slots__` member).
    pub fn slot_get(&self, name: &str) -> Option<Object> {
        self.slots
            .borrow()
            .as_ref()
            .and_then(|s| s.get(&DictKey(Object::from_str(name))).cloned())
    }

    /// Write slot `name` into the side table.
    pub fn slot_set(&self, name: &str, value: Object) {
        self.slots
            .borrow_mut()
            .get_or_insert_with(DictData::new)
            .insert(DictKey(Object::from_str(name)), value);
    }

    /// Delete slot `name` from the side table; `false` when unset.
    pub fn slot_del(&self, name: &str) -> bool {
        self.slots
            .borrow_mut()
            .as_mut()
            .map(|s| s.shift_remove(&DictKey(Object::from_str(name))).is_some())
            .unwrap_or(false)
    }

    /// Snapshot of the populated slot values (for `__getstate__`,
    /// `copy`, and GC tracing).
    pub fn slots_snapshot(&self) -> Vec<(String, Object)> {
        self.slots
            .borrow()
            .as_ref()
            .map(|s| {
                s.iter()
                    .filter_map(|(k, v)| match &k.0 {
                        Object::Str(name) => Some((name.to_string(), v.clone())),
                        _ => None,
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
}

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
    /// Cached classification of this type's `__setattr__` slot, same
    /// protocol as [`Self::getattribute_kind`]: `0` = not yet computed,
    /// `1` = default (`object.__setattr__`), `2` = a user override.
    /// Consulted on every instance attribute store — the default routes
    /// straight to the generic setter instead of a full builtin call
    /// dispatch. Invalidated alongside `getattribute_kind` (same walk)
    /// and on `__setattr__` assignment / deletion / MRO changes.
    pub setattr_kind: Cell<u8>,
    /// Attribute-resolution version, WeavePy's analogue of CPython's
    /// `tp_version_tag`: bumped (for the type and every transitive
    /// subclass) whenever the class dict or MRO changes in a way that can
    /// alter attribute resolution — a class attribute set/delete or a
    /// `__bases__` reshaping. LOAD_ATTR/STORE_ATTR inline caches embed
    /// the value observed at specialisation time and deopt on mismatch,
    /// so installing e.g. a `property` over a name that instances carry
    /// in `__dict__` is seen by already-specialised call sites.
    pub attr_version: Cell<u32>,
    /// Cached "do instances of this type carry a `__del__` finalizer
    /// anywhere in their MRO?" answer, so [`crate::object::PyInstance`]'s
    /// `Drop` safety net can skip an MRO walk on the hot per-instance drop
    /// path: `0` = not yet computed, `1` = no finalizer, `2` = has one.
    /// Invalidated (reset to `0`) for the type and its subclasses whenever
    /// `__del__` is assigned to / deleted from a type's dict or the MRO is
    /// recomputed (`__bases__` assignment).
    pub has_del: Cell<u8>,
    /// Memoised instantiation plan (`type(…)` call protocol resolution:
    /// `__new__`/`__init__`/native-payload classification), stamped with
    /// the [`Self::attr_version`] observed when it was built. Rebuilt
    /// lazily whenever the version moved on. See
    /// `Interpreter::instance_plan`.
    pub instance_plan: RefCell<Option<(u32, Rc<InstancePlan>)>>,
    /// The C `tp_name` of the extension type this class bridges, when it
    /// differs from the bare [`Self::name`] (`"numpy.ndarray"` vs
    /// `"ndarray"`). CPython's `tp_name`-based error text (the
    /// "unsupported operand type(s)" family) prints this full dotted
    /// string while `__name__` stays bare; set by the C-API bridge when a
    /// stock extension type is readied. The `&'static` is a leaked copy —
    /// bridged types are immortal.
    pub c_tp_name: Cell<Option<&'static str>>,
    /// The bridged C type carries a real `tp_as_sequence->sq_item` (RFC
    /// 0047, wave 5). Set by the C-API bridge when an extension type is
    /// readied/spec-built. `PyObject_GetIter`'s legacy fallback keys on
    /// exactly this (`PySequence_Check`): a C type with `sq_item` but no
    /// `tp_iter` is iterable through `PySeqIter` (numpy's
    /// `_array_converter`), while a mapping whose `__getitem__` shim comes
    /// from `mp_subscript` (numpy's parametric dtypes) is not. The VM's
    /// `make_iter` consults this to replicate that distinction, which the
    /// synthesised `__getitem__` dunder alone cannot express.
    pub c_sq_item: Cell<bool>,
}

/// Per-class resolution of the `type.__call__` protocol, cached on the
/// class ([`TypeObject::instance_plan`]) and invalidated by
/// [`TypeObject::attr_version`]. Everything here is a pure function of
/// the class dict / MRO — the *per-call* work (allocating the instance,
/// running `__init__`) stays in `Interpreter::instantiate`.
#[derive(Debug)]
pub struct InstancePlan {
    /// `Some(message)` when the class still carries unimplemented
    /// abstract methods: instantiation fails with exactly this TypeError.
    pub abstract_error: Option<String>,
    /// The resolved user `__new__` (already unwrapped from its
    /// static/classmethod wrapper, pre-bound for the classmethod form).
    /// `None` when the default allocator (`object.__new__`) applies.
    pub user_new: Option<Object>,
    /// The MRO resolution of `__new__` was `object.__new__` itself.
    pub is_object_new: bool,
    /// Native payload classification for built-in value subclasses
    /// (`class C(int)`, `class D(dict)`, …) and descriptor wrappers.
    pub native: NativeKind,
    /// The raw `__init__` MRO hit (may be a `Property` or any object; the
    /// call-time match handles the shapes).
    pub init_fn: Option<Object>,
    /// That `__init__` is `object.__init__` (so the default-arity rules
    /// apply and the call can be skipped when `__new__` is overridden).
    pub init_from_object: bool,
    /// Instances descend from `BaseException`: seed `.args` at allocation.
    pub seeds_exception_args: bool,
    /// The MRO beyond the class itself is just `object` — the strict
    /// "takes no arguments" arity check applies when no `__init__` exists.
    pub only_object_init: bool,
}

/// How a fresh instance's `native` payload is provisioned (see
/// [`InstancePlan::native`]).
#[derive(Debug, Clone)]
pub enum NativeKind {
    /// Plain Python instance — no native payload.
    Plain,
    /// Subclass of `property` — wrap a fresh property built from args.
    Property,
    /// Subclass of `classmethod`.
    ClassMethod,
    /// Subclass of `staticmethod`.
    StaticMethod,
    /// Subclass of a built-in value/container type: seed from the base's
    /// constructor (`mutable` bases with a user `__init__` get an empty
    /// payload instead — the `__init__` owns the filling).
    Value { base: Rc<TypeObject>, mutable: bool },
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
            DictData::default(),
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
            DictData::default(),
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
                        | "lock"
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
                        | "ProxyType"
                        | "CallableProxyType"
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
            // A user class whose MRO is still unset is mid-creation (its
            // custom metaclass `mro()` is running); CPython refuses to
            // extend such an incomplete type (`best_base` error path).
            if !b.flags.is_builtin && b.mro.borrow().is_empty() {
                return Err(type_error(format!(
                    "Cannot extend an incomplete type '{}'",
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
                        return Err(type_error("multiple bases have instance lay-out conflict"));
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
        // CPython `type_new`: a *string* `__qualname__` in the class
        // namespace is removed and stored on the type itself.
        //
        // A getset/member *descriptor* named `__qualname__` is different:
        // it describes the type's *instances* (Cython's
        // `cython_function_or_method`, the generator/coroutine getsets, a
        // `__slots__ = ["__qualname__"]` member) and must stay in the dict
        // — the type's own qualname then falls back to its name, exactly as
        // CPython does (those descriptors arrive via `tp_getset`/
        // `tp_members`, never the `type_new` namespace). The C-API spec/
        // ready bridge merges harvested descriptors into this same dict, so
        // we recognise that case here rather than choking on it.
        let qualname = match dict.get(&DictKey(Object::from_static("__qualname__"))) {
            Some(Object::Str(_)) => {
                match dict.shift_remove(&DictKey(Object::from_static("__qualname__"))) {
                    Some(Object::Str(s)) => Some(s.to_string()),
                    _ => None,
                }
            }
            Some(v) if is_instance_descriptor(v) => None,
            Some(other) => {
                return Err(type_error(format!(
                    "type __qualname__ must be a str, not {}",
                    other.type_name()
                )))
            }
            None => None,
        };
        // Class dicts are the domain of `TypeObject::lookup`'s
        // allocation-free fast pass; note any non-`str` namespace key so
        // the Python-`__eq__` fallback probe knows it can be needed.
        for k in dict.keys() {
            crate::object::note_class_dict_key(k);
        }
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
            setattr_kind: Cell::new(0),
            attr_version: Cell::new(0),
            has_del: Cell::new(0),
            instance_plan: RefCell::new(None),
            c_tp_name: Cell::new(None),
            c_sq_item: Cell::new(false),
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

    /// Does this type install its own `__dict__` slot (CPython's
    /// `tp_dictoffset` added at this level)? Used by the `__class__`
    /// assignment layout check (`same_slots_added`).
    pub fn adds_own_dict(&self) -> bool {
        self.dict
            .borrow()
            .contains_key(&DictKey(Object::from_static("__dict__")))
    }

    /// Does this type install its own `__weakref__` slot (CPython's
    /// `tp_weaklistoffset` added at this level)?
    pub fn adds_own_weakref(&self) -> bool {
        self.dict
            .borrow()
            .contains_key(&DictKey(Object::from_static("__weakref__")))
    }

    /// This type's own `__slots__` member names, sorted, excluding the
    /// pseudo-slots `__dict__`/`__weakref__` (which are accounted for
    /// separately, exactly like CPython's `ht_slots`).
    pub fn member_slots_sorted(&self) -> Vec<String> {
        let mut v: Vec<String> = self
            .slot_names
            .borrow()
            .iter()
            .filter(|s| s.as_str() != "__dict__" && s.as_str() != "__weakref__")
            .cloned()
            .collect();
        v.sort();
        v
    }

    /// Does this type change the instance C-struct relative to its base
    /// (CPython: `!compatible_with_tp_base`)? A built-in value type owns
    /// its layout; a heap type changes it by adding member slots, a
    /// `__dict__`, or a `__weakref__`.
    pub fn changes_layout(&self) -> bool {
        if self.flags.is_builtin {
            return self.name != "object";
        }
        !self.member_slots_sorted().is_empty() || self.adds_own_dict() || self.adds_own_weakref()
    }

    /// CPython `best_base`: the base contributing the instance layout —
    /// the one whose solid base is the most derived. Ties resolve to the
    /// first base (matching `type_new`'s left-to-right scan).
    pub fn best_base(self: &Rc<Self>) -> Option<Rc<TypeObject>> {
        let bases = self.bases.borrow();
        let mut best: Option<Rc<TypeObject>> = None;
        for b in bases.iter() {
            if b.name == "object" && bases.len() > 1 {
                // `object` only wins when it is the sole base.
                if best.is_none() {
                    best = Some(b.clone());
                }
                continue;
            }
            match &best {
                None => best = Some(b.clone()),
                Some(cur) => {
                    if b.is_subclass_of(cur) {
                        best = Some(b.clone());
                    }
                }
            }
        }
        best
    }

    /// CPython `compatible_for_assignment`'s `newbase`/`oldbase` walk:
    /// climb the `best_base` chain past every level that doesn't change
    /// the struct, returning the most-derived type that *does*.
    pub fn layout_struct_base(self: &Rc<Self>) -> Rc<TypeObject> {
        let mut cur = self.clone();
        while !cur.changes_layout() {
            match cur.best_base() {
                Some(b) => cur = b,
                None => break,
            }
        }
        cur
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
            // gh-89653: the `_io` classes were converted to *immutable heap
            // types* (`PyType_FromModuleAndSpec`) in CPython 3.11+, so they
            // report `HEAPTYPE` in addition to `IMMUTABLETYPE`. This is
            // observable: `copyreg._reduce_ex` walks `__mro__` until a
            // non-heap base, so without `HEAPTYPE` an `_io` subclass with
            // `__getstate__`/`__setstate__` cannot pickle at protocols 0/1
            // (`test_io.test_pickling_subclass`).
            if matches!(
                self.name.as_str(),
                "FileIO"
                    | "BytesIO"
                    | "StringIO"
                    | "BufferedReader"
                    | "BufferedWriter"
                    | "BufferedRandom"
                    | "BufferedRWPair"
                    | "TextIOWrapper"
                    | "IncrementalNewlineDecoder"
                    | "IOBase"
                    | "RawIOBase"
                    | "BufferedIOBase"
                    | "TextIOBase"
            ) {
                bits |= HEAPTYPE;
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
        for t in self.mro.borrow().iter() {
            if t.flags.is_builtin {
                match t.name.as_str() {
                    "int" => bits |= LONG_SUBCLASS,
                    "list" => bits |= LIST_SUBCLASS,
                    "tuple" => bits |= TUPLE_SUBCLASS,
                    "bytes" => bits |= BYTES_SUBCLASS,
                    "str" => bits |= UNICODE_SUBCLASS,
                    "dict" => bits |= DICT_SUBCLASS,
                    "type" => bits |= TYPE_SUBCLASS,
                    _ => {}
                }
            }
        }
        bits |= self.collection_flags();
        if self.flags.is_exception {
            bits |= BASE_EXC_SUBCLASS;
        }
        bits
    }

    /// PEP 634 collection flag for this type: `Py_TPFLAGS_SEQUENCE`
    /// (1 << 5) or `Py_TPFLAGS_MAPPING` (1 << 6), driving `MATCH_SEQUENCE`
    /// / `MATCH_MAPPING`. CPython inherits these bits from the dominant
    /// base, so the *first* flag-bearing entry along the MRO wins —
    /// `class M1(UserDict, Sequence)` is MAPPING only, and
    /// `class Both(Sequence, Mapping)` is SEQUENCE only
    /// (test_patma.TestInheritance). Sources of the flag, per MRO entry:
    /// the flag-carrying C builtins (list/tuple/range/memoryview are
    /// sequences; dict/mappingproxy are mappings; str/bytes/bytearray are
    /// deliberately excluded by the PEP), and `_abc_collection_flags` —
    /// where ABCMeta stows a class's `__abc_tpflags__` declaration and
    /// where `ABC.register()` stamps virtual registrations.
    pub fn collection_flags(&self) -> i64 {
        const SEQUENCE: i64 = 1 << 5;
        const MAPPING: i64 = 1 << 6;
        let mro: Vec<Rc<TypeObject>> = self.mro.borrow().clone();
        for t in mro.iter() {
            if t.flags.is_builtin {
                match t.name.as_str() {
                    "list" | "tuple" | "range" | "memoryview" => return SEQUENCE,
                    "dict" | "mappingproxy" => return MAPPING,
                    _ => {}
                }
            }
            if let Some(v) = t
                .dict
                .borrow()
                .get(&DictKey(Object::from_static("_abc_collection_flags")))
                .and_then(Object::as_i64)
            {
                if v & (SEQUENCE | MAPPING) != 0 {
                    return v & (SEQUENCE | MAPPING);
                }
            }
        }
        0
    }

    /// Reset the cached `__getattribute__` / `__setattr__` classifications
    /// for this type and every (transitive) subclass. Called when either
    /// dunder is assigned to or deleted from a type's dict, since that can
    /// change the resolved slot for the type *and* anything inheriting from
    /// it. Class hierarchies are acyclic, so the recursion terminates.
    pub fn invalidate_getattribute_cache(&self) {
        // Iterative walk with a visited set: reentrant `__bases__`
        // assignment can leave a *cycle* through the subclass registry
        // (gh-92112), which naive recursion would loop on forever.
        let mut visited: Vec<*const TypeObject> = vec![std::ptr::from_ref::<TypeObject>(self)];
        self.getattribute_kind.set(0);
        self.setattr_kind.set(0);
        let mut queue: Vec<Rc<TypeObject>> = self.subclasses();
        while let Some(t) = queue.pop() {
            let ptr = Rc::as_ptr(&t);
            if visited.contains(&ptr) {
                continue;
            }
            visited.push(ptr);
            t.getattribute_kind.set(0);
            t.setattr_kind.set(0);
            queue.extend(t.subclasses());
        }
    }

    /// Advance [`Self::attr_version`] for this type and every transitive
    /// subclass. Call after any class-dict mutation or MRO reshaping that
    /// can change what an attribute name resolves to; specialised
    /// LOAD_ATTR/STORE_ATTR sites guard on the version and deopt.
    pub fn bump_attr_version(&self) {
        let mut visited: Vec<*const TypeObject> = vec![std::ptr::from_ref::<TypeObject>(self)];
        self.attr_version
            .set(self.attr_version.get().wrapping_add(1));
        let mut queue: Vec<Rc<TypeObject>> = self.subclasses();
        while let Some(t) = queue.pop() {
            let ptr = Rc::as_ptr(&t);
            if visited.contains(&ptr) {
                continue;
            }
            visited.push(ptr);
            t.attr_version.set(t.attr_version.get().wrapping_add(1));
            queue.extend(t.subclasses());
        }
    }

    /// Classify this type's resolved `__setattr__`: `true` when it is the
    /// stock `object.__setattr__` default (so an instance attribute store
    /// may run the generic setter directly), `false` when any class in the
    /// MRO overrides it. Cached in [`Self::setattr_kind`].
    pub fn setattr_is_default(&self) -> bool {
        match self.setattr_kind.get() {
            1 => return true,
            2 => return false,
            _ => {}
        }
        let default = match self.lookup_with_owner("__setattr__") {
            // The stock default: `object`'s own builtin. Owner identity —
            // not value shape — so a user class *shadowing* the name with
            // a copied builtin still dispatches through the full path.
            Some((Object::Builtin(_), owner)) => {
                Rc::ptr_eq(&owner, &crate::builtin_types::builtin_types().object_)
            }
            None => true,
            Some(_) => false,
        };
        self.setattr_kind.set(if default { 1 } else { 2 });
        default
    }

    /// Do instances of this type carry a `__del__` finalizer anywhere in
    /// their MRO? Cached (see [`TypeObject::has_del`]) so the per-instance
    /// `Drop` safety net pays an MRO walk at most once per type, then a
    /// single `Cell` read. The result only changes when `__del__` is
    /// assigned to / deleted from a class in the MRO, or the MRO itself is
    /// reshaped — both of which reset the cache via
    /// [`Self::invalidate_finalizer_cache`].
    ///
    /// **Drop-safe.** This is called from [`crate::object::PyInstance`]'s
    /// `Drop`, which can fire at *any* moment — including while this very
    /// type's `dict`/`mro` is mutably borrowed (a class attribute that is an
    /// instance of its own class, evicted mid-`__setattr__`). It therefore
    /// probes with `try_borrow` and never re-enters Python (the `__del__`
    /// key is an interned `str`, so dict lookup is pure), falling back to a
    /// conservative `true` on any borrow conflict: a spurious resurrection
    /// is harmless because `Vm::invoke_finalizer` simply no-ops when the
    /// instance turns out to have no `__del__`.
    pub fn instances_need_finalize(&self) -> bool {
        match self.has_del.get() {
            1 => return false,
            2 => return true,
            _ => {}
        }
        let Ok(mro) = self.mro.try_borrow() else {
            return true;
        };
        let key = DictKey(Object::from_static("__del__"));
        for ty in mro.iter() {
            match ty.dict.try_borrow() {
                Ok(d) => {
                    if d.get(&key).is_some() {
                        self.has_del.set(2);
                        return true;
                    }
                }
                // The type is mid-mutation; don't risk a panic, and don't
                // poison the cache — assume finalizable for this one drop.
                Err(_) => return true,
            }
        }
        self.has_del.set(1);
        false
    }

    /// Reset the cached `__del__` classification for this type and every
    /// (transitive) subclass. Called when `__del__` is assigned to / deleted
    /// from a type's dict, or when the MRO is recomputed — either can change
    /// which `__del__` (if any) an instance resolves. Mirrors
    /// [`Self::invalidate_getattribute_cache`]'s acyclic-with-visited walk.
    pub fn invalidate_finalizer_cache(&self) {
        let mut visited: Vec<*const TypeObject> = vec![std::ptr::from_ref::<TypeObject>(self)];
        self.has_del.set(0);
        let mut queue: Vec<Rc<TypeObject>> = self.subclasses();
        while let Some(t) = queue.pop() {
            let ptr = Rc::as_ptr(&t);
            if visited.contains(&ptr) {
                continue;
            }
            visited.push(ptr);
            t.has_del.set(0);
            queue.extend(t.subclasses());
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
        // Fast pass: allocation-free `StrKey` probes, walking the MRO
        // under its borrow. Holding the borrow across the probes is
        // safe while every class-dict key in the process is a plain
        // `str` — the comparisons are then pure native code and can
        // never re-enter Python to reassign `__bases__` mid-walk.
        if !crate::object::exotic_str_keys_possible() {
            let key = crate::object::StrKey(name);
            let mro = self.mro.borrow();
            for ty in mro.iter() {
                if let Some(v) = ty.dict.borrow().get(&key).cloned() {
                    // Introspection-only entries (RFC 0056 WS4) are
                    // invisible to dispatch — keep walking as if absent.
                    // Only in *builtin* dicts, where the docs surface pass
                    // installed them: a user class that aliases one
                    // (`__str__ = object.__str__`) means it, per CPython.
                    if ty.flags.is_builtin && crate::descr_registry::is_surface_only(&v) {
                        continue;
                    }
                    return Some(v);
                }
            }
            return None;
        }
        let key = DictKey(Object::from_str(name));
        // Snapshot the MRO before walking it (CPython `_PyType_Lookup`
        // holds a strong reference for the same reason): a dict probe
        // can re-enter Python (`__eq__` on a non-string class-dict key)
        // and reassign `__bases__` mid-lookup. The in-flight lookup
        // must keep resolving against the *old* linearisation.
        let mro: Vec<Rc<TypeObject>> = self.mro.borrow().clone();
        for ty in mro.iter() {
            if let Some(v) = ty.dict.borrow().get(&key).cloned() {
                if ty.flags.is_builtin && crate::descr_registry::is_surface_only(&v) {
                    continue;
                }
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
        // Fast pass — see `lookup` for the gate rationale.
        if !crate::object::exotic_str_keys_possible() {
            let key = crate::object::StrKey(name);
            let mro = self.mro.borrow();
            for ty in mro.iter() {
                if let Some(v) = ty.dict.borrow().get(&key).cloned() {
                    // Builtin-dict-only skip — see `lookup`.
                    if ty.flags.is_builtin && crate::descr_registry::is_surface_only(&v) {
                        continue;
                    }
                    return Some((v, ty.clone()));
                }
            }
            return None;
        }
        let key = DictKey(Object::from_str(name));
        // Snapshot for reentrancy — see `lookup`.
        let mro: Vec<Rc<TypeObject>> = self.mro.borrow().clone();
        for ty in mro.iter() {
            if let Some(v) = ty.dict.borrow().get(&key).cloned() {
                if ty.flags.is_builtin && crate::descr_registry::is_surface_only(&v) {
                    continue;
                }
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

    /// The name CPython's `tp_name`-based error text prints: the full
    /// dotted C `tp_name` for a bridged/static-emulating type
    /// (`'numpy.ndarray'`, `'re.Pattern'`), the bare `__name__` otherwise.
    pub fn error_tp_name(&self) -> String {
        match self.c_tp_name.get() {
            Some(full) => full.to_owned(),
            None => self.name.clone(),
        }
    }
}

/// Is `obj` a descriptor that describes a type's *instances* (rather than
/// being a plain class attribute)? Used by [`TypeObject::new_with_flags`]
/// to tell a getset/member `__qualname__` (which must stay in the dict)
/// from a class-body `__qualname__` string/value.
fn is_instance_descriptor(obj: &Object) -> bool {
    match obj {
        Object::Property(_) | Object::SlotDescriptor(_) => true,
        Object::Instance(inst) => {
            inst.cls().lookup("__get__").is_some()
                || inst.cls().lookup("__set__").is_some()
                || inst.cls().lookup("__delete__").is_some()
        }
        _ => false,
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
            // CPython `set_mro_error`: list each unmerged list's *head*
            // by `__name__`, in first-appearance order, deduplicated.
            let _ = name;
            let mut seen: Vec<String> = Vec::new();
            for list in &lists {
                if let Some(t) = list.first() {
                    if !seen.iter().any(|n| n == &t.name) {
                        seen.push(t.name.clone());
                    }
                }
            }
            return Err(type_error(format!(
                "Cannot create a consistent method resolution order (MRO) for bases {}",
                seen.join(", ")
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

/// The stable, layout-faithful C "inline body" a C-extension instance
/// owns once it has crossed into an extension that reads its fields at
/// fixed offsets (RFC 0045, wave 3). Holds the body pointer as a
/// `usize` (`0` = no body). `Send + Sync` because the underlying
/// [`Cell`] is.
///
/// **Excluded from the structural clone of [`PyInstance`].** A cloned
/// instance is a *distinct* object that owns no body — most importantly
/// the wave-2 finalizer-resurrection net (`PyInstance::drop`) shallow-
/// copies a dying instance, and duplicating the raw body pointer there
/// would double-free it. `Clone` therefore yields the empty state; the
/// freshly-cloned instance lazily mints its own body if it ever crosses
/// into C again.
#[derive(Debug, Default)]
pub struct CBody(Cell<usize>);

impl CBody {
    /// The body pointer (`0` when the instance has no faithful body).
    #[inline]
    pub fn get(&self) -> usize {
        self.0.get()
    }
    /// Record the body pointer this instance now owns.
    #[inline]
    pub fn set(&self, p: usize) {
        self.0.set(p);
    }
}

impl Clone for CBody {
    fn clone(&self) -> Self {
        CBody(Cell::new(0))
    }
}

/// Process-global hook that frees a C-extension instance's faithful
/// inline body. Registered once by `weavepy-capi` at interpreter init
/// (the same additive-hook pattern wave 2 used for
/// `register_traverse`/`register_clear`); inert in a pure-VM build, so a
/// run with no C extension loaded is byte-for-byte unchanged.
static INSTANCE_BODY_FREE: std::sync::OnceLock<fn(usize)> = std::sync::OnceLock::new();

/// Register the faithful-instance-body free hook (RFC 0045, wave 3).
/// Idempotent — a second registration is ignored.
pub fn register_instance_body_free(f: fn(usize)) {
    let _ = INSTANCE_BODY_FREE.set(f);
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
    /// struct. Unset for ordinary objects. Set once — normally at
    /// construction (the wrapped builtins are themselves immutable), or
    /// on the first C-boundary crossing for a faithful C-built subtype
    /// body (numpy's `str_`/`bytes_`, whose value is stamped into the
    /// inline body by the extension's `tp_new` chain after allocation).
    /// Unwrapped by the numeric / comparison / hashing / conversion
    /// fast paths so e.g. `class C(int)` instances behave like real ints.
    pub native: std::sync::OnceLock<Object>,
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
    /// Memoised Python `__hash__` result, populated lazily *only* for
    /// instances that wrap an **immutable** builtin value (an `int`/`str`/
    /// `tuple`/… subclass). Such an instance's value can't change, so its
    /// `__hash__` is genuinely constant and may be cached — which is what
    /// lets CPython's hash-table reuse (`set(dict)`, `dict.fromkeys(set)`,
    /// …) observe exactly one `__hash__` call per element
    /// (`test_set`/`test_dict` `test_do_not_rehash_dict_keys`). A plain
    /// `object` subclass with a side-effecting or conditionally-raising
    /// `__hash__` (test_dict's `BadHash`) wraps no native value and is
    /// never cached here, so it is re-invoked on every probe like CPython.
    pub hash_cache: Cell<Option<i64>>,
    /// One-shot "has `__del__` already run for this instance?" guard,
    /// mirroring [`crate::object::PyGenerator::finalize_ran`] and CPython's
    /// `_PyGC_FINALIZED` bit. Set by `Vm::invoke_finalizer` the moment the
    /// finalizer is dispatched; read by [`PyInstance`]'s `Drop` to decide
    /// whether the dying instance still needs its `__del__` resurrected onto
    /// the pending-finalizer queue. Without this, an acyclic finalizable
    /// instance whose last `Arc` is dropped on a code path that *didn't*
    /// route through the prompt-reap cascade (e.g. a cross-thread handoff
    /// where a transient clone briefly inflated the refcount so the reap
    /// bailed) is freed silently, skipping `__del__` (RFC 0040:
    /// `test_multiprocessing_*` `test_release_task_refs` leaked one
    /// `CountedObject` per race).
    pub finalize_ran: Cell<bool>,
    /// The stable C "inline body" this instance owns once it has crossed
    /// into a C extension that reads its fields at fixed `tp_basicsize`
    /// offsets (RFC 0045, wave 3). `0` for the overwhelmingly common case
    /// (pure-Python instances, and C instances that store state in
    /// `__dict__`). Freed exactly once, in [`PyInstance`]'s `Drop`, via
    /// the [`register_instance_body_free`] hook.
    pub c_body: CBody,
}

impl PyInstance {
    pub fn new(class: Rc<TypeObject>) -> Self {
        Self {
            class: RefCell::new(class),
            dict: Rc::new(RefCell::new(DictData::default())),
            native: std::sync::OnceLock::new(),
            inline_values: Cell::new(true),
            slots: RefCell::new(None),
            hash_cache: Cell::new(None),
            finalize_ran: Cell::new(false),
            c_body: CBody::default(),
        }
    }

    /// Build an instance that wraps a primitive `native` value
    /// (subclass of `int`/`str`/…).
    pub fn with_native(class: Rc<TypeObject>, native: Object) -> Self {
        Self {
            class: RefCell::new(class),
            dict: Rc::new(RefCell::new(DictData::default())),
            native: std::sync::OnceLock::from(native),
            inline_values: Cell::new(true),
            slots: RefCell::new(None),
            hash_cache: Cell::new(None),
            finalize_ran: Cell::new(false),
            c_body: CBody::default(),
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
    /// Keys in the side table are always plain `Str` (only `slot_set`
    /// writes it), so the allocation-free probe is authoritative.
    pub fn slot_get(&self, name: &str) -> Option<Object> {
        self.slots
            .borrow()
            .as_ref()
            .and_then(|s| s.get(&crate::object::StrKey(name)).cloned())
    }

    /// Write slot `name` into the side table.
    pub fn slot_set(&self, name: &str, value: Object) {
        let mut guard = self.slots.borrow_mut();
        let table = guard.get_or_insert_with(DictData::default);
        // Overwrite in place without re-allocating the key when the
        // slot already exists (every write after the first).
        if let Some(v) = table.get_mut(&crate::object::StrKey(name)) {
            *v = value;
        } else {
            table.insert(DictKey(Object::from_str(name)), value);
        }
    }

    /// Delete slot `name` from the side table; `false` when unset.
    pub fn slot_del(&self, name: &str) -> bool {
        self.slots
            .borrow_mut()
            .as_mut()
            .map(|s| s.shift_remove(&crate::object::StrKey(name)).is_some())
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

impl Drop for PyInstance {
    /// Last-resort finalizer safety net, mirroring
    /// [`crate::object::PyGenerator`]'s `Drop`. WeavePy normally runs an
    /// instance's `__del__` through the prompt-reap cascade the instant its
    /// last program reference is dropped (matching CPython's refcount
    /// timing). But that cascade is driven from specific eval-loop sites and
    /// is gated on a refcount-dead test; when the final `Arc` is released
    /// somewhere else — most often a cross-thread object handoff where a
    /// transient clone on another thread briefly inflated the strong count so
    /// the reap conservatively bailed — the instance would otherwise be freed
    /// by a plain `Arc` drop with its `__del__` silently skipped (the cycle
    /// collector never revisits acyclic objects). Catch that here: resurrect
    /// a shallow copy that shares the dying instance's `__dict__`/slots/native
    /// value onto the VM's pending-finalizer queue so `__del__` still runs.
    fn drop(&mut self) {
        if std::env::var_os("WEAVEPY_REAP_TRACE").is_some() {
            let name = self.cls().name.clone();
            if name.contains("Block") || name.contains("DataFrame") {
                eprintln!("[INST-DROP] {name} body={:#x}", self.c_body.get());
            }
        }
        // RFC 0045 (wave 3): release the faithful C inline body this
        // instance owns, if it ever crossed into a C extension that reads
        // its fields at fixed `tp_basicsize` offsets. Runs before the
        // finalizer net (and its early returns) so the body is freed
        // exactly once regardless of `__del__` state. Inert (one `Cell`
        // read) for every instance that never grew a body — i.e. all
        // pure-Python instances and all dict-backed C instances.
        let body = self.c_body.get();
        if body != 0 {
            self.c_body.set(0);
            if let Some(free) = INSTANCE_BODY_FREE.get() {
                free(body);
            }
        }
        // Already finalized (cascade/GC/this net's resurrected copy): the
        // common case for finalizable instances and the *only* path for the
        // overwhelmingly common finalizer-free instance — a single `Cell`
        // read keeps the hot drop path cheap.
        if self.finalize_ran.get() {
            return;
        }
        // No `__del__` anywhere in the MRO ⇒ nothing to do. Cached on the
        // type, so this is one more `Cell` read after the first instance.
        if !self.cls().instances_need_finalize() {
            return;
        }
        // CPython runs `tp_finalize` once: claim it so the resurrected copy
        // (and any re-drop after the finalizer completes) can't loop.
        self.finalize_ran.set(true);
        // Can't run Python from `Drop`; resurrect onto the pending queue. The
        // copy shares `dict`/`slots`/`native` (cloning the `Arc`s/contents),
        // so `__del__` observes the same attributes; `try_*` tolerates TLS
        // teardown and re-entrant borrows by dropping the request.
        let resurrected = Object::Instance(Rc::new(PyInstance {
            class: RefCell::new(self.cls()),
            dict: self.dict.clone(),
            native: match self.native.get() {
                Some(v) => std::sync::OnceLock::from(v.clone()),
                None => std::sync::OnceLock::new(),
            },
            inline_values: Cell::new(self.inline_values.get()),
            slots: RefCell::new(self.slots.borrow().clone()),
            hash_cache: Cell::new(self.hash_cache.get()),
            finalize_ran: Cell::new(true),
            // The resurrected copy is a distinct object that owns no C
            // body (the dying `self` already freed its own above).
            c_body: CBody::default(),
        }));
        crate::vm_singletons::try_push_pending_finalizer(resurrected);
    }
}

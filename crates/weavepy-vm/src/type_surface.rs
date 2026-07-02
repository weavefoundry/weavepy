//! Materialized method/dunder surface for the built-in types.
//!
//! CPython stores every method and slot wrapper of a built-in type in
//! that type's `tp_dict`; `vars(list)`, `'__hash__' in bytearray.__dict__`
//! and `_collections_abc._check_methods` all introspect those dicts
//! directly. WeavePy historically synthesized built-in methods *on
//! demand* (variant match tables in `builtins.rs`), which kept the type
//! dicts almost empty and made structural ABC checks
//! (`Hashable`/`Callable`/`Reversible`/`Buffer`, …) misreport.
//!
//! This module fills the built-in type dicts at interpreter start with
//! real entries that delegate to the existing method machinery:
//!
//! - regular methods (`list.append`, `dict.keys`, …) reuse the
//!   [`crate::builtins::lookup_method`] tables, wrapped in a shim that
//!   unwraps a built-in-subclass receiver to its native payload (the
//!   binding CPython's descriptors do via the C `self` slot);
//! - protocol dunders missing from those tables (`set.__sub__`,
//!   `dict.__or__`, `list.__reversed__`, `__class_getitem__`,
//!   `__buffer__`, …) are implemented here with CPython's exact
//!   semantics (strict operand types, `NotImplemented` declines);
//! - unhashable container types get the literal `__hash__ = None`
//!   marker CPython stores (what `Hashable`'s subclass hook keys on).
//!
//! Entries are only inserted when absent so the specialized dunders
//! installed by `builtin_types.rs` (`__new__`, `__init__`, exception
//! `__str__`, …) keep priority.

use crate::sync::Rc;
use crate::sync::RefCell;

use crate::builtin_types::BuiltinTypes;
use crate::error::{type_error, value_error, RuntimeError};
use crate::object::{
    BuiltinFn, DictData, DictKey, MethodWrapper, Object, PyIterator, PyMemoryView,
};
use crate::types::TypeObject;

/// Entry point: called once per thread when [`BuiltinTypes`] is built.
pub fn install(bt: &BuiltinTypes) {
    install_callables(bt);
    install_hash_markers(bt);
    install_container_protocols(bt);
    install_set_operators(bt);
    install_dict_operators(bt);
    install_class_getitem(bt);
    install_buffer_protocol(bt);
    install_method_tables(bt);
    install_object_compare(bt);
    install_value_richcmp(bt);
    install_numeric_getsets(bt);
    install_value_reprs(bt);
    install_numeric_dunders(bt);
    install_immutable_getnewargs(bt);
    register_descriptor_kinds(bt);
}

/// Tag every `Object::Builtin` sitting in a built-in type's own dict as a
/// `method_descriptor` (regular method) or `wrapper_descriptor` (C slot
/// dunder), recording its owning class so `type(str.lower)`,
/// `str.lower.__qualname__` and `int.__add__.__objclass__` match CPython
/// (test_descr test_qualname). Runs last, after every install pass has
/// populated the dicts. Properties are tagged at creation in
/// `install_numeric_getsets`.
fn register_descriptor_kinds(bt: &BuiltinTypes) {
    use crate::descr_registry::{is_slot_wrapper_name, register, DescrKind};
    for (_, value) in bt.as_globals() {
        let Object::Type(ty) = value else { continue };
        if !ty.flags.is_builtin {
            continue;
        }
        // Snapshot the (name, value) pairs to avoid holding the dict borrow
        // across `register` (which borrows the descr table, not this dict).
        let entries: Vec<(String, Object)> = ty
            .dict
            .borrow()
            .iter()
            .filter_map(|(k, v)| match (&k.0, v) {
                // Instance-binding builtins are `method_descriptor`s. The
                // class/static-method descriptors (`fromkeys`/`maketrans`/
                // `fromhex`) are now non-binding (see `unwrap_shim`) but still
                // need their `__qualname__`/`__objclass__` recorded, so admit
                // them explicitly by name.
                (Object::Str(s), Object::Builtin(b))
                    if b.binds_instance || is_no_receiver_descr(s) =>
                {
                    Some((s.to_string(), v.clone()))
                }
                _ => None,
            })
            .collect();
        for (name, value) in entries {
            let kind = if is_slot_wrapper_name(&name) {
                DescrKind::Wrapper
            } else {
                DescrKind::Method
            };
            register(&value, kind, ty.clone(), &name, None);
        }
    }
}

/// Materialize `__getnewargs__` in the immutable sequence types' dicts
/// (`'__getnewargs__' in tuple.__dict__` is True in CPython). Without a
/// real type-dict entry, `copyreg._reduce_newobj`'s type-only
/// `_lookup_special` can't see WeavePy's instance-synthesized hook, so
/// `copy.copy`/`pickle` of a `tuple`/`str`/`bytes` *subclass* rebuilds an
/// empty instance (regression caught by `test_copy.test_copy_tuple_subclass`).
/// The numeric value types already get theirs via `install_numeric_dunders`.
fn install_immutable_getnewargs(bt: &BuiltinTypes) {
    for ty in [&bt.tuple_, &bt.str_, &bt.bytes_] {
        insert_if_absent(
            ty,
            "__getnewargs__",
            crate::builtins::immutable_getnewargs_method(),
        );
    }
}

/// Materialize the numeric operator slots in the value types' dicts
/// (`'__add__' in vars(int)` is True in CPython; test_descr's
/// OperatorsTest walks `t.__dict__` looking for them). The synthesized
/// builtins are the same ones `lookup_method` resolves on instances.
fn install_numeric_dunders(bt: &BuiltinTypes) {
    const NAMES: &[&str] = &[
        "__add__",
        "__radd__",
        "__sub__",
        "__rsub__",
        "__mul__",
        "__rmul__",
        "__truediv__",
        "__rtruediv__",
        "__floordiv__",
        "__rfloordiv__",
        "__mod__",
        "__rmod__",
        "__pow__",
        "__rpow__",
        "__divmod__",
        "__rdivmod__",
        "__lshift__",
        "__rlshift__",
        "__rshift__",
        "__rrshift__",
        "__and__",
        "__rand__",
        "__or__",
        "__ror__",
        "__xor__",
        "__rxor__",
        "__neg__",
        "__pos__",
        "__invert__",
        "__abs__",
        "__bool__",
        "__eq__",
        "__ne__",
        "__lt__",
        "__le__",
        "__gt__",
        "__ge__",
        "__hash__",
        "__format__",
        "__getnewargs__",
    ];
    for (ty, rep) in [
        (&bt.int_, Object::Int(0)),
        (&bt.float_, Object::Float(0.0)),
        (&bt.complex_, Object::new_complex(0.0, 0.0)),
    ] {
        for name in NAMES {
            if let Some(b) = crate::builtins::numeric_dunder(&rep, name) {
                insert_if_absent(ty, name, Object::Builtin(Rc::new(b)));
            }
        }
    }
}

/// Materialize `tp_repr` for the value types (`'__repr__' in vars(int)`
/// is True in CPython). The slot renders the *native payload* without
/// re-dispatching `type(self).__repr__` — `int.__repr__(IntFlagMember)`
/// must yield the plain digits.
fn install_value_reprs(bt: &BuiltinTypes) {
    fn install(ty: &Rc<TypeObject>) {
        let key = DictKey(Object::from_static("__repr__"));
        let mut d = ty.dict.borrow_mut();
        if !d.contains_key(&key) {
            d.insert(
                key,
                Object::Builtin(Rc::new(BuiltinFn {
                    name: "__repr__",
                    binds_instance: true,
                    call: Box::new(crate::builtins::value_slot_repr),
                    call_kw: None,
                })),
            );
        }
    }
    for ty in [
        &bt.int_,
        &bt.bool_,
        &bt.float_,
        &bt.complex_,
        &bt.str_,
        &bt.bytes_,
        &bt.bytearray_,
        &bt.tuple_,
    ] {
        install(ty);
    }
}

// ---------------------------------------------------------------------------
// Numeric getset descriptors
// ---------------------------------------------------------------------------

/// CPython stores `int.numerator`/`denominator`/`real`/`imag` (and the
/// float pair) as getset descriptors in the type dict. Materialize them
/// as properties so `int.__dict__['numerator']` exists and is a
/// descriptor — `enum.EnumType._add_member_` keys its shadowed-attribute
/// redirect on exactly this.
fn install_numeric_getsets(bt: &BuiltinTypes) {
    fn int_value(o: &Object) -> Result<Object, RuntimeError> {
        match o {
            Object::Bool(b) => Ok(Object::Int(i64::from(*b))),
            Object::Int(_) | Object::Long(_) => Ok(o.clone()),
            Object::Instance(_) => match o.native_value() {
                Some(n) => int_value(&n),
                None => Err(type_error("descriptor requires an 'int' object")),
            },
            _ => Err(type_error("descriptor requires an 'int' object")),
        }
    }
    fn float_value(o: &Object) -> Result<Object, RuntimeError> {
        match o {
            Object::Float(_) => Ok(o.clone()),
            Object::Instance(_) => match o.native_value() {
                Some(n) => float_value(&n),
                None => Err(type_error("descriptor requires a 'float' object")),
            },
            _ => Err(type_error("descriptor requires a 'float' object")),
        }
    }
    fn getset(
        ty: &Rc<TypeObject>,
        name: &'static str,
        kind: crate::descr_registry::DescrKind,
        doc: Option<&'static str>,
        f: fn(&[Object]) -> Result<Object, RuntimeError>,
    ) {
        let fget = Object::Builtin(Rc::new(BuiltinFn {
            name,
            binds_instance: true,
            call: Box::new(f),
            call_kw: None,
        }));
        let prop = Object::Property(Rc::new(crate::object::PyProperty::new(
            fget,
            Object::None,
            Object::None,
            Object::None,
        )));
        let key = DictKey(Object::from_static(name));
        let mut d = ty.dict.borrow_mut();
        if !d.contains_key(&key) {
            // Tag the property so `type(float.real)` reports the right
            // descriptor type and `float.real.__qualname__`/`__doc__`/
            // `__objclass__` resolve (test_descr test_qualname/test_descrdoc).
            crate::descr_registry::register(&prop, kind, ty.clone(), name, doc);
            d.insert(key, prop);
        }
    }
    use crate::descr_registry::DescrKind::{GetSet, Member};
    // CPython models `int`/`float` real/imag/numerator/denominator as
    // `tp_getset` (getset_descriptor) but `complex` real/imag as
    // `tp_members` (member_descriptor).
    getset(&bt.int_, "numerator", GetSet, None, |args| {
        int_value(args.first().unwrap_or(&Object::None))
    });
    getset(&bt.int_, "denominator", GetSet, None, |args| {
        int_value(args.first().unwrap_or(&Object::None)).map(|_| Object::Int(1))
    });
    getset(&bt.int_, "real", GetSet, None, |args| {
        int_value(args.first().unwrap_or(&Object::None))
    });
    getset(&bt.int_, "imag", GetSet, None, |args| {
        int_value(args.first().unwrap_or(&Object::None)).map(|_| Object::Int(0))
    });
    getset(&bt.float_, "real", GetSet, None, |args| {
        float_value(args.first().unwrap_or(&Object::None))
    });
    getset(&bt.float_, "imag", GetSet, None, |args| {
        float_value(args.first().unwrap_or(&Object::None)).map(|_| Object::Float(0.0))
    });
    fn complex_value(o: &Object) -> Result<(f64, f64), RuntimeError> {
        match o {
            Object::Complex(c) => Ok((c.real, c.imag)),
            Object::Instance(_) => match o.native_value() {
                Some(n) => complex_value(&n),
                None => Err(type_error("descriptor requires a 'complex' object")),
            },
            _ => Err(type_error("descriptor requires a 'complex' object")),
        }
    }
    getset(
        &bt.complex_,
        "real",
        Member,
        Some("the real part of a complex number"),
        |args| complex_value(args.first().unwrap_or(&Object::None)).map(|(r, _)| Object::Float(r)),
    );
    getset(
        &bt.complex_,
        "imag",
        Member,
        Some("the imaginary part of a complex number"),
        |args| complex_value(args.first().unwrap_or(&Object::None)).map(|(_, i)| Object::Float(i)),
    );
}

// ---------------------------------------------------------------------------
// Value-type rich comparisons
// ---------------------------------------------------------------------------

/// Materialize `tp_richcompare` for the value types that define one in
/// CPython (`'__lt__' in vars(int)` is True there). Each slot is
/// type-strict: a foreign right operand *declines* with
/// `NotImplemented` so the reflected dunder gets its turn — e.g.
/// `(3).__eq__(3.0)` is `NotImplemented` and `3 == 3.0` resolves via
/// `float.__eq__`.
fn install_value_richcmp(bt: &BuiltinTypes) {
    use crate::CompareKind;

    fn fam_int(o: &Object) -> bool {
        matches!(o, Object::Int(_) | Object::Long(_) | Object::Bool(_))
    }
    fn fam_float(o: &Object) -> bool {
        matches!(
            o,
            Object::Int(_) | Object::Long(_) | Object::Bool(_) | Object::Float(_)
        )
    }
    fn fam_complex(o: &Object) -> bool {
        matches!(
            o,
            Object::Complex(_)
                | Object::Int(_)
                | Object::Long(_)
                | Object::Bool(_)
                | Object::Float(_)
        )
    }
    fn fam_str(o: &Object) -> bool {
        matches!(o, Object::Str(_))
    }
    fn fam_bytes(o: &Object) -> bool {
        matches!(o, Object::Bytes(_) | Object::ByteArray(_))
    }
    fn fam_tuple(o: &Object) -> bool {
        matches!(o, Object::Tuple(_))
    }
    fn fam_list(o: &Object) -> bool {
        matches!(o, Object::List(_))
    }
    fn fam_none(_: &Object) -> bool {
        false
    }

    fn richcmp_builtin(
        name: &'static str,
        op: CompareKind,
        family: fn(&Object) -> bool,
        owner: &Rc<TypeObject>,
    ) -> Object {
        let owner = crate::sync::Rc::downgrade(owner);
        Object::Builtin(Rc::new(BuiltinFn {
            name,
            binds_instance: true,
            call: Box::new(move |args: &[Object]| {
                let (a, b) = match args {
                    [a, b] => (as_native(a), as_native(b)),
                    _ => {
                        return Err(type_error(format!(
                            "{name} expected 2 arguments, got {}",
                            args.len()
                        )))
                    }
                };
                // Wrong-class receiver: CPython slot wrappers *raise*
                // (bpo-37619: `class A(int): __eq__ = str.__eq__`), they
                // don't decline with NotImplemented. The other operand
                // failing the family check declines below.
                if let (Some(owner), Some(first)) = (owner.upgrade(), args.first()) {
                    let cls = crate::builtins::class_of(first);
                    if !cls.is_subclass_of(&owner) {
                        return Err(type_error(format!(
                            "descriptor '{}' requires a '{}' object but received a '{}'",
                            name, owner.name, cls.name
                        )));
                    }
                }
                if !family(&a) || !family(&b) {
                    return Ok(crate::vm_singletons::not_implemented());
                }
                // Containers recurse *through the interpreter* so element
                // `__eq__`/`__lt__` is honoured — `list_richcompare` in
                // CPython calls `PyObject_RichCompare` per item. The
                // native fallback only runs with no ambient interpreter
                // (early startup) or for scalar families.
                if let Some(ptr) = crate::vm_singletons::current_interpreter_ptr() {
                    // SAFETY: published by an enclosing VM frame on this thread.
                    let interp = unsafe { &mut *ptr };
                    let globals = interp.builtins_dict();
                    match op {
                        CompareKind::Eq | CompareKind::NotEq => {
                            if let Some(rv) = interp.deep_equal_collection(&a, &b, &globals)? {
                                let rv = if matches!(op, CompareKind::Eq) {
                                    rv
                                } else {
                                    !rv
                                };
                                return Ok(Object::Bool(rv));
                            }
                        }
                        _ => {
                            if let Some(rv) = interp.deep_order_collection(&a, &b, op, &globals)? {
                                return Ok(Object::Bool(rv));
                            }
                        }
                    }
                }
                match op {
                    CompareKind::Eq => Ok(Object::Bool(a.eq_value(&b))),
                    CompareKind::NotEq => Ok(Object::Bool(!a.eq_value(&b))),
                    _ => match crate::compare_op(&a, &b, op) {
                        Ok(v) => Ok(Object::Bool(v)),
                        // An unordered pair declines rather than raising;
                        // the dispatcher produces the final TypeError.
                        Err(_) => Ok(crate::vm_singletons::not_implemented()),
                    },
                }
            }),
            call_kw: None,
        }))
    }

    const ORDERED: &[(&str, CompareKind)] = &[
        ("__lt__", CompareKind::Lt),
        ("__le__", CompareKind::LtE),
        ("__gt__", CompareKind::Gt),
        ("__ge__", CompareKind::GtE),
        ("__eq__", CompareKind::Eq),
        ("__ne__", CompareKind::NotEq),
    ];
    let totally_ordered: &[(&Rc<TypeObject>, fn(&Object) -> bool)] = &[
        (&bt.int_, fam_int),
        (&bt.float_, fam_float),
        (&bt.str_, fam_str),
        (&bt.bytes_, fam_bytes),
        (&bt.bytearray_, fam_bytes),
        (&bt.tuple_, fam_tuple),
        (&bt.list_, fam_list),
    ];
    for (ty, fam) in totally_ordered {
        for (name, op) in ORDERED {
            insert_if_absent(ty, name, richcmp_builtin(name, *op, *fam, ty));
        }
    }
    // complex: equality across the numeric tower, ordering always
    // declines (CPython's `complex_richcompare` returns NotImplemented
    // for Py_LT/…, but the slots still exist in `vars(complex)`).
    for (name, op) in ORDERED {
        let fam = if matches!(op, CompareKind::Eq | CompareKind::NotEq) {
            fam_complex
        } else {
            fam_none
        };
        insert_if_absent(
            &bt.complex_,
            name,
            richcmp_builtin(name, *op, fam, &bt.complex_),
        );
    }
    // dict / range: equality only.
    fn fam_dict(o: &Object) -> bool {
        matches!(o, Object::Dict(_))
    }
    fn fam_range(o: &Object) -> bool {
        matches!(o, Object::Range(_))
    }
    for (name, op) in &[("__eq__", CompareKind::Eq), ("__ne__", CompareKind::NotEq)] {
        insert_if_absent(
            &bt.dict_,
            name,
            richcmp_builtin(name, *op, fam_dict, &bt.dict_),
        );
        insert_if_absent(
            &bt.range_,
            name,
            richcmp_builtin(name, *op, fam_range, &bt.range_),
        );
    }
}

// ---------------------------------------------------------------------------
// object's rich comparisons
// ---------------------------------------------------------------------------

/// `object` owns the six rich-comparison dunders in CPython
/// (`'__lt__' in vars(object)` is True and `type.__gt__ is
/// object.__gt__` — `functools.total_ordering`'s root detection does
/// exactly that identity test). Materialize them once so every MRO
/// lookup returns the *same* object.
fn install_object_compare(bt: &BuiltinTypes) {
    fn not_implemented(_args: &[Object]) -> Result<Object, RuntimeError> {
        Ok(crate::vm_singletons::not_implemented())
    }
    fn obj_eq(args: &[Object]) -> Result<Object, RuntimeError> {
        // Default `__eq__` is identity; non-identical operands *decline*
        // (NotImplemented) so the reflected dunder gets its turn.
        match args {
            [a, b] if a.is_same(b) => Ok(Object::Bool(true)),
            _ => Ok(crate::vm_singletons::not_implemented()),
        }
    }
    fn obj_ne(args: &[Object]) -> Result<Object, RuntimeError> {
        match args {
            [a, b] if a.is_same(b) => Ok(Object::Bool(false)),
            _ => Ok(crate::vm_singletons::not_implemented()),
        }
    }
    insert_if_absent(&bt.object_, "__eq__", builtin("__eq__", obj_eq));
    insert_if_absent(&bt.object_, "__ne__", builtin("__ne__", obj_ne));
    for name in ["__lt__", "__le__", "__gt__", "__ge__"] {
        // One shared entry per name; the closure ignores its arguments.
        let f = match name {
            "__lt__" => builtin("__lt__", not_implemented),
            "__le__" => builtin("__le__", not_implemented),
            "__gt__" => builtin("__gt__", not_implemented),
            _ => builtin("__ge__", not_implemented),
        };
        insert_if_absent(&bt.object_, name, f);
    }
}

/// True for the named-method-table entries that are CPython class/static
/// methods rather than instance methods (`dict.fromkeys`, `str.maketrans`,
/// `bytes.fromhex`/`bytearray.fromhex`). Their bodies scan arguments from
/// slot 0 and take no instance receiver, so the type-surface shim must not
/// prepend `self` and must not rebind when read through an instance.
fn is_no_receiver_descr(name: &str) -> bool {
    matches!(name, "maketrans" | "fromkeys" | "fromhex")
}

/// Insert `name` into `ty`'s dict unless already present.
fn insert_if_absent(ty: &Rc<TypeObject>, name: &str, value: Object) {
    let key = DictKey(Object::from_str(name));
    let mut dict = ty.dict.borrow_mut();
    if !dict.contains_key(&key) {
        dict.insert(key, value);
    }
}

/// Replace an `Instance` receiver/operand with its wrapped native
/// payload (`class C(dict)` instances act as their payload for the
/// base type's methods, like CPython's C-level `self`).
fn as_native(o: &Object) -> Object {
    if let Object::Instance(inst) = o {
        if let Some(native) = &inst.native {
            return native.clone();
        }
    }
    o.clone()
}

/// Wrap a `lookup_method`-table builtin so an `Instance` receiver
/// (built-in subclass) is unwrapped to its native payload before the
/// underlying implementation runs.
fn unwrap_shim(inner: Rc<BuiltinFn>, owner: &Rc<TypeObject>) -> Object {
    let inner_pos = inner.clone();
    let has_kw = inner.call_kw.is_some();
    // Static/class-method-like entries take no instance receiver;
    // skip the descriptor receiver check for them.
    let no_receiver = is_no_receiver_descr(inner.name);
    let owner_pos = crate::sync::Rc::downgrade(owner);
    let owner_kw = owner_pos.clone();
    // CPython method descriptors validate the receiver
    // (`descrobject.c::descr_check`): `list.sort(thing)` for a non-list
    // raises TypeError instead of silently running (bpo-37619 /
    // gh-92063).
    fn check_receiver(
        name: &str,
        owner: &crate::sync::Weak<TypeObject>,
        first: &Object,
    ) -> Result<(), RuntimeError> {
        if let Some(owner) = owner.upgrade() {
            let cls = crate::builtins::class_of(first);
            if !cls.is_subclass_of(&owner) {
                return Err(type_error(format!(
                    "descriptor '{}' for '{}' objects doesn't apply to a '{}' object",
                    name, owner.name, cls.name
                )));
            }
        }
        Ok(())
    }
    let mut shim = BuiltinFn {
        name: inner.name,
        // `maketrans`/`fromkeys`/`fromhex` are CPython class/static methods:
        // they take no instance receiver (the body scans args from slot 0), so
        // reading one off an *instance* (`class C: ctor = dict.fromkeys;
        // C().ctor(it)`) must NOT prepend `self` — otherwise `self` is fed in
        // as the iterable (bpo-46615's `TestMethodsMutating_Set_Dict`). A
        // non-binding builtin is returned unchanged by
        // `maybe_bind`/`descriptor_get`, matching CPython where `dict.fromkeys`
        // is already class-bound and not an instance descriptor.
        binds_instance: inner.binds_instance && !no_receiver,
        call: Box::new(move |args| {
            if let Some(first) = args.first() {
                if !no_receiver {
                    check_receiver(inner_pos.name, &owner_pos, first)?;
                }
                let unwrapped = as_native(first);
                if !unwrapped.is_same(first) {
                    let mut v = args.to_vec();
                    v[0] = unwrapped;
                    return (inner_pos.call)(&v);
                }
            }
            (inner_pos.call)(args)
        }),
        call_kw: None,
    };
    if has_kw {
        let inner_kw = inner.clone();
        shim.call_kw = Some(Box::new(move |args, kwargs| {
            let kw = inner_kw
                .call_kw
                .as_ref()
                .expect("call_kw checked at shim construction");
            if let Some(first) = args.first() {
                if !no_receiver {
                    check_receiver(inner_kw.name, &owner_kw, first)?;
                }
                let unwrapped = as_native(first);
                if !unwrapped.is_same(first) {
                    let mut v = args.to_vec();
                    v[0] = unwrapped;
                    return kw(&v, kwargs);
                }
            }
            kw(args, kwargs)
        }));
    }
    Object::Builtin(Rc::new(shim))
}

fn builtin(name: &'static str, f: fn(&[Object]) -> Result<Object, RuntimeError>) -> Object {
    Object::Builtin(Rc::new(BuiltinFn {
        name,
        binds_instance: true,
        call: Box::new(f),
        call_kw: None,
    }))
}

// ---------------------------------------------------------------------------
// callables: `__call__` (what `collections.abc.Callable`'s hook checks)
// ---------------------------------------------------------------------------

fn install_callables(bt: &BuiltinTypes) {
    for ty in [
        &bt.function_,
        &bt.builtin_function_,
        &bt.method_,
        &bt.method_wrapper_,
        &bt.type_,
        // The callable descriptor types (CPython's `method_descriptor` /
        // `wrapper_descriptor` carry `tp_call`); `getset_descriptor` /
        // `member_descriptor` are data-only and stay non-callable.
        &bt.method_descriptor_,
        &bt.wrapper_descriptor_,
    ] {
        if let Some(w) = crate::builtins::builtin_type_dunder(&ty.name, "__call__") {
            insert_if_absent(ty, "__call__", w);
        }
    }
}

// ---------------------------------------------------------------------------
// hashability markers
// ---------------------------------------------------------------------------

fn obj_hash_builtin(args: &[Object]) -> Result<Object, RuntimeError> {
    let o = args
        .first()
        .ok_or_else(|| type_error("__hash__() takes exactly one argument (0 given)"))?;
    crate::builtins::hash_object(&as_native(o))
}

fn install_hash_markers(bt: &BuiltinTypes) {
    // CPython stores the literal `None` in the type dict of every
    // unhashable built-in; `_check_methods` (Hashable's hook) and
    // user-visible `bytearray.__hash__ is None` both rely on it. It also
    // makes subclass instances correctly unhashable through the MRO.
    for ty in [&bt.list_, &bt.dict_, &bt.set_, &bt.bytearray_] {
        insert_if_absent(ty, "__hash__", Object::None);
    }
    // Hashable value types expose a real `__hash__` slot.
    for ty in [
        &bt.str_,
        &bt.bytes_,
        &bt.int_,
        &bt.bool_,
        &bt.float_,
        &bt.complex_,
        &bt.tuple_,
        &bt.frozenset_,
        &bt.range_,
        &bt.slice_,
    ] {
        insert_if_absent(ty, "__hash__", builtin("__hash__", obj_hash_builtin));
    }
}

// ---------------------------------------------------------------------------
// container protocol dunders
// ---------------------------------------------------------------------------

fn obj_iter_builtin(args: &[Object]) -> Result<Object, RuntimeError> {
    let recv = as_native(
        args.first()
            .ok_or_else(|| type_error("__iter__() missing self"))?,
    );
    Ok(Object::Iter(Rc::new(RefCell::new(recv.make_iter()?))))
}

fn obj_len_builtin(args: &[Object]) -> Result<Object, RuntimeError> {
    let recv = as_native(
        args.first()
            .ok_or_else(|| type_error("__len__() missing self"))?,
    );
    Ok(Object::Int(recv.len()? as i64))
}

fn obj_contains_builtin(args: &[Object]) -> Result<Object, RuntimeError> {
    let recv = as_native(
        args.first()
            .ok_or_else(|| type_error("__contains__() missing self"))?,
    );
    let item = args
        .get(1)
        .ok_or_else(|| type_error("__contains__() takes exactly one argument (0 given)"))?;
    Ok(Object::Bool(recv.contains(item)?))
}

fn list_reversed_builtin(args: &[Object]) -> Result<Object, RuntimeError> {
    let recv = as_native(
        args.first()
            .ok_or_else(|| type_error("__reversed__() missing self"))?,
    );
    let Object::List(items) = &recv else {
        return Err(type_error(
            "descriptor '__reversed__' requires a 'list' object",
        ));
    };
    let reversed: Vec<Object> = items.borrow().iter().rev().cloned().collect();
    Ok(Object::Iter(Rc::new(RefCell::new(PyIterator::Tuple {
        items: Rc::from(reversed.as_slice()),
        index: 0,
    }))))
}

fn dict_reversed_builtin(args: &[Object]) -> Result<Object, RuntimeError> {
    let recv = as_native(
        args.first()
            .ok_or_else(|| type_error("__reversed__() missing self"))?,
    );
    let Object::Dict(d) = &recv else {
        return Err(type_error(
            "descriptor '__reversed__' requires a 'dict' object",
        ));
    };
    let keys: Vec<Object> = d.borrow().keys().rev().map(|k| k.0.clone()).collect();
    Ok(Object::Iter(Rc::new(RefCell::new(PyIterator::Tuple {
        items: Rc::from(keys.as_slice()),
        index: 0,
    }))))
}

/// `dict_keys.__reversed__` / `dict_values` / `dict_items` — CPython's
/// dict views are reversible (their C types carry `__reversed__`, which
/// is also what `collections.abc.Reversible`'s subclasshook probes for).
fn dict_view_reversed_builtin(args: &[Object]) -> Result<Object, RuntimeError> {
    let recv = args
        .first()
        .ok_or_else(|| type_error("__reversed__() missing self"))?;
    let Object::DictView(v) = recv else {
        return Err(type_error(
            "descriptor '__reversed__' requires a dict view object",
        ));
    };
    let d = v.dict.borrow();
    let items: Vec<Object> = match v.kind {
        crate::object::DictViewKind::Keys => d.keys().rev().map(|k| k.0.clone()).collect(),
        crate::object::DictViewKind::Values => d.values().rev().cloned().collect(),
        crate::object::DictViewKind::Items => d
            .iter()
            .rev()
            .map(|(k, val)| Object::new_tuple(vec![k.0.clone(), val.clone()]))
            .collect(),
    };
    Ok(Object::Iter(Rc::new(RefCell::new(PyIterator::Tuple {
        items: Rc::from(items.as_slice()),
        index: 0,
    }))))
}

fn iter_next_builtin(args: &[Object]) -> Result<Object, RuntimeError> {
    let recv = args
        .first()
        .ok_or_else(|| type_error("__next__() missing self"))?;
    let Object::Iter(it) = recv else {
        return Err(type_error("descriptor '__next__' requires an iterator"));
    };
    match it.borrow_mut().next_value() {
        Some(v) => Ok(v),
        None => Err(RuntimeError::PyException(
            crate::error::PyException::from_builtin("StopIteration", ""),
        )),
    }
}

fn iter_self_builtin(args: &[Object]) -> Result<Object, RuntimeError> {
    args.first()
        .cloned()
        .ok_or_else(|| type_error("__iter__() missing self"))
}

fn install_container_protocols(bt: &BuiltinTypes) {
    let iterable: [&Rc<TypeObject>; 9] = [
        &bt.list_,
        &bt.tuple_,
        &bt.str_,
        &bt.dict_,
        &bt.set_,
        &bt.frozenset_,
        &bt.bytes_,
        &bt.bytearray_,
        &bt.range_,
    ];
    for ty in iterable {
        insert_if_absent(ty, "__iter__", builtin("__iter__", obj_iter_builtin));
        insert_if_absent(ty, "__len__", builtin("__len__", obj_len_builtin));
        insert_if_absent(
            ty,
            "__contains__",
            builtin("__contains__", obj_contains_builtin),
        );
    }
    for ty in [&bt.dict_keys_, &bt.dict_values_, &bt.dict_items_] {
        insert_if_absent(ty, "__iter__", builtin("__iter__", obj_iter_builtin));
        insert_if_absent(ty, "__len__", builtin("__len__", obj_len_builtin));
        insert_if_absent(
            ty,
            "__contains__",
            builtin("__contains__", obj_contains_builtin),
        );
        insert_if_absent(
            ty,
            "__reversed__",
            builtin("__reversed__", dict_view_reversed_builtin),
        );
    }
    insert_if_absent(
        &bt.list_,
        "__reversed__",
        builtin("__reversed__", list_reversed_builtin),
    );
    insert_if_absent(
        &bt.dict_,
        "__reversed__",
        builtin("__reversed__", dict_reversed_builtin),
    );
    insert_if_absent(
        &bt.iterator_,
        "__iter__",
        builtin("__iter__", iter_self_builtin),
    );
    insert_if_absent(
        &bt.iterator_,
        "__next__",
        builtin("__next__", iter_next_builtin),
    );
}

// ---------------------------------------------------------------------------
// set operators — strict operand types, `NotImplemented` declines
// ---------------------------------------------------------------------------

/// Both operands as set payloads, or `None` to signal a decline.
fn two_sets(args: &[Object]) -> Option<(Object, Object)> {
    let a = as_native(args.first()?);
    let b = as_native(args.get(1)?);
    let is_set = |o: &Object| matches!(o, Object::Set(_) | Object::FrozenSet(_));
    if is_set(&a) && is_set(&b) {
        Some((a, b))
    } else {
        None
    }
}

fn set_items(o: &Object) -> Vec<DictKey> {
    match o {
        Object::Set(s) => s.borrow().iter().cloned().collect(),
        Object::FrozenSet(s) => s.iter().cloned().collect(),
        _ => Vec::new(),
    }
}

/// Build the result with the *left* operand's storage kind (CPython:
/// `set | frozenset` → `set`, `frozenset | set` → `frozenset`).
fn set_like(model: &Object, items: Vec<DictKey>) -> Object {
    match model {
        Object::FrozenSet(_) => Object::new_frozenset_from(items.into_iter().map(|k| k.0)),
        _ => {
            let mut out = indexmap::IndexSet::new();
            for k in items {
                out.insert(k);
            }
            Object::Set(Rc::new(RefCell::new(out)))
        }
    }
}

/// Owned membership snapshot of a set operand.
///
/// Set operators must run element `__hash__`/`__eq__` (which, per the
/// bpo-46615 regression tests, can re-enter and `clear()` the operands)
/// *without* holding a borrow on any live `Object::Set` cell. We snapshot
/// the elements out under a short-lived borrow (`set_items`) and rebuild a
/// detached `IndexSet`, so a re-entrant mutation touches a cell we are no
/// longer borrowing instead of panicking with `BorrowMutError`.
fn snapshot_set(o: &Object) -> indexmap::IndexSet<DictKey> {
    let mut out = indexmap::IndexSet::new();
    for k in set_items(o) {
        out.insert(k);
    }
    out
}

macro_rules! set_binop {
    ($fname:ident, $f:expr) => {
        fn $fname(args: &[Object]) -> Result<Object, RuntimeError> {
            match two_sets(args) {
                Some((a, b)) => {
                    let op: fn(&Object, &Object) -> Vec<DictKey> = $f;
                    Ok(set_like(&a, op(&a, &b)))
                }
                None => Ok(crate::vm_singletons::not_implemented()),
            }
        }
    };
}

set_binop!(set_sub_builtin, |a, b| {
    let bs = snapshot_set(b);
    set_items(a)
        .into_iter()
        .filter(|k| !bs.contains(k))
        .collect()
});
set_binop!(set_and_builtin, |a, b| {
    let bs = snapshot_set(b);
    set_items(a)
        .into_iter()
        .filter(|k| bs.contains(k))
        .collect()
});
set_binop!(set_or_builtin, |a, b| {
    let as_ = snapshot_set(a);
    let mut items: Vec<DictKey> = as_.iter().cloned().collect();
    for k in set_items(b) {
        if !as_.contains(&k) {
            items.push(k);
        }
    }
    items
});
set_binop!(set_xor_builtin, |a, b| {
    let as_ = snapshot_set(a);
    let bs = snapshot_set(b);
    let mut items: Vec<DictKey> = as_.iter().filter(|k| !bs.contains(*k)).cloned().collect();
    for k in bs.iter() {
        if !as_.contains(k) {
            items.push(k.clone());
        }
    }
    items
});

// Reflected forms: `__rsub__(self, other)` computes `other - self` with
// the result kind following `other` (the left operand of the original
// expression).
set_binop!(set_rsub_builtin, |a, b| {
    let as_ = snapshot_set(a);
    set_items(b)
        .into_iter()
        .filter(|k| !as_.contains(k))
        .collect()
});

fn set_rsub_outer(args: &[Object]) -> Result<Object, RuntimeError> {
    match two_sets(args) {
        Some((_, b)) => {
            let r = set_rsub_builtin(args)?;
            // Re-key the result on the *other* operand's storage kind.
            match r {
                Object::Set(_) | Object::FrozenSet(_) => {
                    let items = set_items(&r);
                    Ok(set_like(&b, items))
                }
                other => Ok(other),
            }
        }
        None => Ok(crate::vm_singletons::not_implemented()),
    }
}

fn set_rand_outer(args: &[Object]) -> Result<Object, RuntimeError> {
    match two_sets(args) {
        Some((a, b)) => {
            let as_ = snapshot_set(&a);
            let items = set_items(&b)
                .into_iter()
                .filter(|k| as_.contains(k))
                .collect();
            Ok(set_like(&b, items))
        }
        None => Ok(crate::vm_singletons::not_implemented()),
    }
}

fn set_ror_outer(args: &[Object]) -> Result<Object, RuntimeError> {
    match two_sets(args) {
        Some((a, b)) => {
            let bs = snapshot_set(&b);
            let mut items: Vec<DictKey> = bs.iter().cloned().collect();
            for k in set_items(&a) {
                if !bs.contains(&k) {
                    items.push(k);
                }
            }
            Ok(set_like(&b, items))
        }
        None => Ok(crate::vm_singletons::not_implemented()),
    }
}

fn set_rxor_outer(args: &[Object]) -> Result<Object, RuntimeError> {
    match two_sets(args) {
        Some((a, b)) => {
            let as_ = snapshot_set(&a);
            let bs = snapshot_set(&b);
            let mut items: Vec<DictKey> =
                bs.iter().filter(|k| !as_.contains(*k)).cloned().collect();
            for k in as_.iter() {
                if !bs.contains(k) {
                    items.push(k.clone());
                }
            }
            Ok(set_like(&b, items))
        }
        None => Ok(crate::vm_singletons::not_implemented()),
    }
}

fn set_subset(a: &Object, b: &Object) -> bool {
    let bs = snapshot_set(b);
    set_items(a).iter().all(|k| bs.contains(k))
}

macro_rules! set_cmp {
    ($fname:ident, $f:expr) => {
        fn $fname(args: &[Object]) -> Result<Object, RuntimeError> {
            match two_sets(args) {
                Some((a, b)) => {
                    let op: fn(&Object, &Object) -> bool = $f;
                    Ok(Object::Bool(op(&a, &b)))
                }
                None => Ok(crate::vm_singletons::not_implemented()),
            }
        }
    };
}

fn set_len_of(o: &Object) -> usize {
    match o {
        Object::Set(s) => s.borrow().len(),
        Object::FrozenSet(s) => s.len(),
        _ => 0,
    }
}

set_cmp!(set_le_builtin, |a, b| set_subset(a, b));
set_cmp!(set_lt_builtin, |a, b| {
    set_len_of(a) < set_len_of(b) && set_subset(a, b)
});
set_cmp!(set_ge_builtin, |a, b| set_subset(b, a));
set_cmp!(set_gt_builtin, |a, b| {
    set_len_of(a) > set_len_of(b) && set_subset(b, a)
});
set_cmp!(set_eq_builtin, |a, b| {
    set_len_of(a) == set_len_of(b) && set_subset(a, b)
});
set_cmp!(set_ne_builtin, |a, b| {
    !(set_len_of(a) == set_len_of(b) && set_subset(a, b))
});

/// In-place ops (`__isub__`, …) — mutate a real `set` receiver in place
/// and return it; decline for frozenset/non-set operands so the VM
/// falls back to the binary form (CPython only defines these on `set`).
macro_rules! set_iop {
    ($fname:ident, $compute:expr) => {
        fn $fname(args: &[Object]) -> Result<Object, RuntimeError> {
            let recv = as_native(
                args.first()
                    .ok_or_else(|| type_error("set operation missing self"))?,
            );
            let other = as_native(
                args.get(1)
                    .ok_or_else(|| type_error("set operation missing operand"))?,
            );
            let Object::Set(target) = &recv else {
                return Ok(crate::vm_singletons::not_implemented());
            };
            if !matches!(other, Object::Set(_) | Object::FrozenSet(_)) {
                return Ok(crate::vm_singletons::not_implemented());
            }
            // Compute the new contents from a *detached* snapshot of the
            // receiver, then publish in a borrow that runs no Python. The
            // element `__hash__`/`__eq__` invoked while computing may
            // re-enter and `clear()` the receiver (bpo-46615); because we
            // hold no borrow on `target` during that window, the re-entrant
            // mutation hits an un-borrowed cell instead of panicking.
            let compute: fn(indexmap::IndexSet<DictKey>, &Object) -> indexmap::IndexSet<DictKey> =
                $compute;
            let snapshot = target.borrow().clone();
            let result = compute(snapshot, &other);
            *target.borrow_mut() = result;
            // Return the original receiver (subclass instance included)
            // so `s -= t` preserves identity.
            Ok(args[0].clone())
        }
    };
}

set_iop!(set_isub_builtin, |mut t, o| {
    let os = snapshot_set(o);
    t.retain(|k| !os.contains(k));
    t
});
set_iop!(set_iand_builtin, |t, o| {
    let os = snapshot_set(o);
    let mut out = indexmap::IndexSet::new();
    for k in t {
        if os.contains(&k) {
            out.insert(k);
        }
    }
    out
});
set_iop!(set_ior_builtin, |mut t, o| {
    for k in set_items(o) {
        t.insert(k);
    }
    t
});
set_iop!(set_ixor_builtin, |mut t, o| {
    for k in set_items(o) {
        if t.contains(&k) {
            t.shift_remove(&k);
        } else {
            t.insert(k);
        }
    }
    t
});

fn install_set_operators(bt: &BuiltinTypes) {
    for ty in [&bt.set_, &bt.frozenset_] {
        insert_if_absent(ty, "__sub__", builtin("__sub__", set_sub_builtin));
        insert_if_absent(ty, "__rsub__", builtin("__rsub__", set_rsub_outer));
        insert_if_absent(ty, "__and__", builtin("__and__", set_and_builtin));
        insert_if_absent(ty, "__rand__", builtin("__rand__", set_rand_outer));
        insert_if_absent(ty, "__or__", builtin("__or__", set_or_builtin));
        insert_if_absent(ty, "__ror__", builtin("__ror__", set_ror_outer));
        insert_if_absent(ty, "__xor__", builtin("__xor__", set_xor_builtin));
        insert_if_absent(ty, "__rxor__", builtin("__rxor__", set_rxor_outer));
        insert_if_absent(ty, "__le__", builtin("__le__", set_le_builtin));
        insert_if_absent(ty, "__lt__", builtin("__lt__", set_lt_builtin));
        insert_if_absent(ty, "__ge__", builtin("__ge__", set_ge_builtin));
        insert_if_absent(ty, "__gt__", builtin("__gt__", set_gt_builtin));
        insert_if_absent(ty, "__eq__", builtin("__eq__", set_eq_builtin));
        insert_if_absent(ty, "__ne__", builtin("__ne__", set_ne_builtin));
    }
    insert_if_absent(&bt.set_, "__isub__", builtin("__isub__", set_isub_builtin));
    insert_if_absent(&bt.set_, "__iand__", builtin("__iand__", set_iand_builtin));
    insert_if_absent(&bt.set_, "__ior__", builtin("__ior__", set_ior_builtin));
    insert_if_absent(&bt.set_, "__ixor__", builtin("__ixor__", set_ixor_builtin));
}

// ---------------------------------------------------------------------------
// dict operators (PEP 584 + equality)
// ---------------------------------------------------------------------------

fn dict_pairs(o: &Object) -> Option<Rc<RefCell<DictData>>> {
    match o {
        Object::Dict(d) => Some(d.clone()),
        _ => None,
    }
}

fn dict_or_builtin(args: &[Object]) -> Result<Object, RuntimeError> {
    let (a, b) = (
        as_native(args.first().unwrap_or(&Object::None)),
        as_native(args.get(1).unwrap_or(&Object::None)),
    );
    match (dict_pairs(&a), dict_pairs(&b)) {
        (Some(da), Some(db)) => {
            let mut merged = da.borrow().clone();
            for (k, v) in db.borrow().iter() {
                merged.insert(k.clone(), v.clone());
            }
            Ok(Object::Dict(Rc::new(RefCell::new(merged))))
        }
        _ => Ok(crate::vm_singletons::not_implemented()),
    }
}

fn dict_ror_builtin(args: &[Object]) -> Result<Object, RuntimeError> {
    let (a, b) = (
        as_native(args.first().unwrap_or(&Object::None)),
        as_native(args.get(1).unwrap_or(&Object::None)),
    );
    match (dict_pairs(&a), dict_pairs(&b)) {
        (Some(da), Some(db)) => {
            let mut merged = db.borrow().clone();
            for (k, v) in da.borrow().iter() {
                merged.insert(k.clone(), v.clone());
            }
            Ok(Object::Dict(Rc::new(RefCell::new(merged))))
        }
        _ => Ok(crate::vm_singletons::not_implemented()),
    }
}

fn dict_eq_builtin(args: &[Object]) -> Result<Object, RuntimeError> {
    let (a, b) = (
        as_native(args.first().unwrap_or(&Object::None)),
        as_native(args.get(1).unwrap_or(&Object::None)),
    );
    match (&a, &b) {
        (Object::Dict(_), Object::Dict(_)) => Ok(Object::Bool(a.eq_value(&b))),
        _ => Ok(crate::vm_singletons::not_implemented()),
    }
}

fn dict_ne_builtin(args: &[Object]) -> Result<Object, RuntimeError> {
    match dict_eq_builtin(args)? {
        Object::Bool(v) => Ok(Object::Bool(!v)),
        other => Ok(other),
    }
}

fn install_dict_operators(bt: &BuiltinTypes) {
    insert_if_absent(&bt.dict_, "__or__", builtin("__or__", dict_or_builtin));
    insert_if_absent(&bt.dict_, "__ror__", builtin("__ror__", dict_ror_builtin));
    insert_if_absent(&bt.dict_, "__eq__", builtin("__eq__", dict_eq_builtin));
    insert_if_absent(&bt.dict_, "__ne__", builtin("__ne__", dict_ne_builtin));
}

// ---------------------------------------------------------------------------
// PEP 585 `__class_getitem__`
// ---------------------------------------------------------------------------

fn class_getitem_builtin(args: &[Object]) -> Result<Object, RuntimeError> {
    let origin = args
        .first()
        .cloned()
        .ok_or_else(|| type_error("__class_getitem__() missing cls"))?;
    let params = args.get(1).cloned().unwrap_or(Object::None);
    Ok(crate::make_generic_alias_public(origin, params))
}

fn install_class_getitem(bt: &BuiltinTypes) {
    for ty in [
        &bt.list_,
        &bt.tuple_,
        &bt.dict_,
        &bt.set_,
        &bt.frozenset_,
        &bt.type_,
        // PEP 654 groups expose `__class_getitem__` in CPython (C
        // `Py_GenericAlias`), e.g. `BaseExceptionGroup[T]` in hypothesis.
        &bt.base_exception_group,
        &bt.exception_group,
    ] {
        insert_if_absent(
            ty,
            "__class_getitem__",
            Object::ClassMethod(MethodWrapper::new(builtin(
                "__class_getitem__",
                class_getitem_builtin,
            ))),
        );
    }
}

// ---------------------------------------------------------------------------
// PEP 688 `__buffer__`
// ---------------------------------------------------------------------------

fn buffer_builtin(args: &[Object]) -> Result<Object, RuntimeError> {
    let recv = as_native(
        args.first()
            .ok_or_else(|| type_error("__buffer__() missing self"))?,
    );
    match &recv {
        Object::Bytes(b) => Ok(Object::MemoryView(Rc::new(PyMemoryView::from_bytes(
            b.clone(),
        )))),
        Object::ByteArray(b) => Ok(Object::MemoryView(Rc::new(PyMemoryView::from_bytearray(
            b.clone(),
        )))),
        Object::MemoryView(_) => Ok(recv.clone()),
        other => Err(value_error(format!(
            "__buffer__ not supported for '{}'",
            other.type_name()
        ))),
    }
}

fn install_buffer_protocol(bt: &BuiltinTypes) {
    for ty in [&bt.bytes_, &bt.bytearray_, &bt.memoryview_] {
        insert_if_absent(ty, "__buffer__", builtin("__buffer__", buffer_builtin));
    }
}

// ---------------------------------------------------------------------------
// regular method tables (reusing `lookup_method` via a representative)
// ---------------------------------------------------------------------------

fn install_named_methods(ty: &Rc<TypeObject>, type_name: &str, names: &[&str]) {
    for name in names {
        if let Some(Object::Builtin(inner)) = crate::builtins::unbound_method(type_name, name) {
            insert_if_absent(ty, name, unwrap_shim(inner, ty));
        }
    }
}

fn install_method_tables(bt: &BuiltinTypes) {
    install_named_methods(
        &bt.str_,
        "str",
        &[
            "upper",
            "lower",
            "title",
            "capitalize",
            "casefold",
            "swapcase",
            "strip",
            "lstrip",
            "rstrip",
            "split",
            "rsplit",
            "splitlines",
            "join",
            "startswith",
            "endswith",
            "replace",
            "find",
            "rfind",
            "index",
            "rindex",
            "count",
            "partition",
            "rpartition",
            "isdigit",
            "isalpha",
            "isalnum",
            "isspace",
            "isupper",
            "islower",
            "isascii",
            "isnumeric",
            "isdecimal",
            "isidentifier",
            "isprintable",
            "istitle",
            "zfill",
            "ljust",
            "rjust",
            "center",
            "expandtabs",
            "encode",
            "removeprefix",
            "removesuffix",
            "translate",
            "maketrans",
            "__getitem__",
            "__add__",
            "__mul__",
            "__rmul__",
            "__mod__",
            "__len__",
            "__contains__",
        ],
    );
    install_named_methods(
        &bt.list_,
        "list",
        &[
            "append",
            "pop",
            "extend",
            "insert",
            "remove",
            "index",
            "count",
            "sort",
            "reverse",
            "clear",
            "copy",
            "__getitem__",
            "__setitem__",
            "__delitem__",
            "__add__",
            "__mul__",
            "__rmul__",
            "__iadd__",
            "__imul__",
            "__len__",
            "__contains__",
        ],
    );
    install_named_methods(
        &bt.dict_,
        "dict",
        &[
            "get",
            "keys",
            "values",
            "items",
            "pop",
            "update",
            "clear",
            "setdefault",
            "copy",
            "fromkeys",
            "popitem",
            "__getitem__",
            "__setitem__",
            "__delitem__",
        ],
    );
    install_named_methods(
        &bt.tuple_,
        "tuple",
        &[
            "count",
            "index",
            "__getitem__",
            "__add__",
            "__mul__",
            "__rmul__",
            "__len__",
            "__contains__",
        ],
    );
    install_named_methods(
        &bt.set_,
        "set",
        &[
            "add",
            "discard",
            "remove",
            "pop",
            "clear",
            "copy",
            "update",
            "union",
            "intersection",
            "difference",
            "symmetric_difference",
            "issubset",
            "issuperset",
            "isdisjoint",
            "intersection_update",
            "difference_update",
            "symmetric_difference_update",
        ],
    );
    install_named_methods(
        &bt.frozenset_,
        "frozenset",
        &[
            "copy",
            "union",
            "intersection",
            "difference",
            "symmetric_difference",
            "issubset",
            "issuperset",
            "isdisjoint",
        ],
    );
    for (ty, name) in [(&bt.bytes_, "bytes"), (&bt.bytearray_, "bytearray")] {
        install_named_methods(
            ty,
            name,
            &[
                "decode",
                "hex",
                "fromhex",
                "startswith",
                "endswith",
                "find",
                "rfind",
                "index",
                "rindex",
                "count",
                "lower",
                "upper",
                "strip",
                "lstrip",
                "rstrip",
                "split",
                "rsplit",
                "splitlines",
                "join",
                "replace",
                "translate",
                "maketrans",
                "partition",
                "rpartition",
                "removeprefix",
                "removesuffix",
                "expandtabs",
                "center",
                "ljust",
                "rjust",
                "zfill",
                "capitalize",
                "title",
                "swapcase",
                "isalnum",
                "isalpha",
                "isdigit",
                "isspace",
                "islower",
                "isupper",
                "istitle",
                "isascii",
                "__getitem__",
                "__add__",
                "__mul__",
                "__rmul__",
                "__len__",
                "__contains__",
            ],
        );
    }
    install_named_methods(
        &bt.bytearray_,
        "bytearray",
        &["append", "extend", "clear", "pop", "reverse", "insert"],
    );
}

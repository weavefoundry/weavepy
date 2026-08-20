//! The registry of built-in types.
//!
//! Built-in types (`object`, `type`, `int`, `str`, …) and the entire
//! `BaseException` hierarchy live as singleton `Rc<TypeObject>`s
//! created once at interpreter startup and cached per-thread.
//!
//! User-facing names map to these via the `as_dict()` snapshot,
//! which the builtins module installs into module globals at import
//! time. Internally the VM reaches for individual types — e.g.
//! `BuiltinTypes::with(|bt| bt.type_error.clone())` — to construct
//! exception instances.

use crate::sync::Rc;
use crate::sync::RefCell;

use crate::error::RuntimeError;
use crate::object::{DictData, DictKey, MethodWrapper, Object};
use crate::types::TypeObject;

/// All built-in classes, kept in one place so calls like
/// `BuiltinTypes::type_error()` are constant-time.
#[allow(missing_debug_implementations)]
pub struct BuiltinTypes {
    pub object_: Rc<TypeObject>,
    pub type_: Rc<TypeObject>,
    pub property_: Rc<TypeObject>,
    pub staticmethod_: Rc<TypeObject>,
    pub classmethod_: Rc<TypeObject>,

    pub int_: Rc<TypeObject>,
    pub float_: Rc<TypeObject>,
    pub bool_: Rc<TypeObject>,
    pub complex_: Rc<TypeObject>,
    pub str_: Rc<TypeObject>,
    pub bytes_: Rc<TypeObject>,
    pub bytearray_: Rc<TypeObject>,
    pub tuple_: Rc<TypeObject>,
    pub list_: Rc<TypeObject>,
    pub dict_: Rc<TypeObject>,
    pub set_: Rc<TypeObject>,
    pub frozenset_: Rc<TypeObject>,
    pub range_: Rc<TypeObject>,
    pub slice_: Rc<TypeObject>,
    pub memoryview_: Rc<TypeObject>,
    pub mappingproxy_: Rc<TypeObject>,
    pub dict_keys_: Rc<TypeObject>,
    pub dict_values_: Rc<TypeObject>,
    pub dict_items_: Rc<TypeObject>,
    pub iterator_: Rc<TypeObject>,
    /// `enumerate` — a real type in CPython (`type(enumerate([])) is
    /// enumerate`, subclassable, PEP 585 generic).
    pub enumerate_: Rc<TypeObject>,
    /// `reversed` — likewise a real type in CPython.
    pub reversed_: Rc<TypeObject>,
    pub none_type: Rc<TypeObject>,
    pub ellipsis_: Rc<TypeObject>,
    pub not_implemented_type_: Rc<TypeObject>,
    pub simple_namespace_: Rc<TypeObject>,
    /// `types.GenericAlias` — the type of PEP 585 aliases (`list[int]`).
    pub generic_alias_: Rc<TypeObject>,
    /// `types.UnionType` — the type of PEP 604 unions (`int | str`).
    pub union_type_: Rc<TypeObject>,
    pub function_: Rc<TypeObject>,
    pub method_: Rc<TypeObject>,
    /// `builtin_function_or_method` — the type of Rust-implemented
    /// callables (`type(len)`), distinct from `function` as in CPython.
    pub builtin_function_: Rc<TypeObject>,
    /// `method-wrapper` — the type of a slot wrapper bound to an
    /// instance (`type(object().__str__)`).
    pub method_wrapper_: Rc<TypeObject>,
    /// `member_descriptor` — the type of `__slots__` storage descriptors
    /// (`types.MemberDescriptorType`).
    pub member_descriptor_: Rc<TypeObject>,
    /// `method_descriptor` — an unbound built-in method reached through a
    /// type (`type(str.lower)`, `types.MethodDescriptorType`).
    pub method_descriptor_: Rc<TypeObject>,
    /// `wrapper_descriptor` — an unbound slot wrapper reached through a
    /// type (`type(int.__add__)`, `types.WrapperDescriptorType`).
    pub wrapper_descriptor_: Rc<TypeObject>,
    /// `getset_descriptor` — a computed attribute descriptor reached
    /// through a type (`type(float.real)`, `types.GetSetDescriptorType`).
    pub getset_descriptor_: Rc<TypeObject>,
    /// `classmethod_descriptor` — a *C-level* classmethod reached through
    /// a type dict (`type(dict.__dict__['fromkeys'])`,
    /// `types.ClassMethodDescriptorType`). Distinct from user
    /// `classmethod` objects, which inspect treats as user-defined
    /// callables (test_inspect test_signature_on_class [classmethod]).
    pub classmethod_descriptor_: Rc<TypeObject>,
    /// `super` — the type of `super(...)` proxies (`type(super(C, x))`).
    /// Real (subclassable) so `class mysuper(super)` works.
    pub super_: Rc<TypeObject>,
    pub generator_: Rc<TypeObject>,
    pub coroutine_: Rc<TypeObject>,
    pub async_generator_: Rc<TypeObject>,
    /// The awaitables returned by `agen.asend(...)` / `agen.__anext__()`
    /// and `agen.athrow(...)` / `agen.aclose()` (CPython's
    /// `async_generator_asend` / `async_generator_athrow`). Giving them
    /// real types lets `_collections_abc` register them as `Coroutine`s,
    /// so `asyncio.iscoroutine(agen.aclose())` is true and
    /// `loop.create_task(agen.aclose())` works (PEP 525 finalization,
    /// `shutdown_asyncgens`).
    pub async_generator_asend_: Rc<TypeObject>,
    pub async_generator_athrow_: Rc<TypeObject>,
    /// `types.FrameType` / `types.TracebackType`.
    pub frame_: Rc<TypeObject>,
    pub code_: Rc<TypeObject>,
    pub traceback_: Rc<TypeObject>,
    /// `types.CellType` — real closure cells (RFC 0056 WS4):
    /// constructible (`CellType()` / `CellType(v)`), with writable
    /// `cell_contents` and contents-based rich comparison, so
    /// `mock.patch` on closures and `@deprecated` retained-reference
    /// checks work.
    pub cell_: Rc<TypeObject>,

    pub module_: Rc<TypeObject>,

    pub base_exception: Rc<TypeObject>,
    pub exception: Rc<TypeObject>,
    pub arithmetic_error: Rc<TypeObject>,
    pub assertion_error: Rc<TypeObject>,
    pub attribute_error: Rc<TypeObject>,
    pub import_error: Rc<TypeObject>,
    pub module_not_found_error: Rc<TypeObject>,
    pub index_error: Rc<TypeObject>,
    pub key_error: Rc<TypeObject>,
    pub lookup_error: Rc<TypeObject>,
    pub name_error: Rc<TypeObject>,
    pub not_implemented_error: Rc<TypeObject>,
    pub os_error: Rc<TypeObject>,
    pub overflow_error: Rc<TypeObject>,
    pub floating_point_error: Rc<TypeObject>,
    pub runtime_error: Rc<TypeObject>,
    pub stop_iteration: Rc<TypeObject>,
    pub stop_async_iteration: Rc<TypeObject>,
    pub syntax_error: Rc<TypeObject>,
    pub indentation_error: Rc<TypeObject>,
    pub tab_error: Rc<TypeObject>,
    pub timeout_error: Rc<TypeObject>,
    pub type_error: Rc<TypeObject>,
    pub unbound_local_error: Rc<TypeObject>,
    pub value_error: Rc<TypeObject>,
    pub unicode_error: Rc<TypeObject>,
    pub unicode_encode_error: Rc<TypeObject>,
    pub unicode_decode_error: Rc<TypeObject>,
    pub unicode_translate_error: Rc<TypeObject>,
    pub zero_division_error: Rc<TypeObject>,
    pub generator_exit: Rc<TypeObject>,
    pub keyboard_interrupt: Rc<TypeObject>,
    pub system_exit: Rc<TypeObject>,
    pub recursion_error: Rc<TypeObject>,
    /// 3.13 (gh-114570): raised when an operation is attempted during
    /// interpreter finalization (e.g. `_thread.start_new_thread` after
    /// shutdown began).
    pub python_finalization_error: Rc<TypeObject>,
    /// 3.13: `SyntaxError` subclass the `codeop`/REPL machinery raises
    /// for source that is syntactically incomplete rather than wrong.
    pub incomplete_input_error: Rc<TypeObject>,

    // RFC 0017 — OSError sub-hierarchy used by the new socket /
    // subprocess / filesystem surface. Mirrors CPython's PEP 3151
    // "exception hierarchy refactor."
    pub blocking_io_error: Rc<TypeObject>,
    pub broken_pipe_error: Rc<TypeObject>,
    pub child_process_error: Rc<TypeObject>,
    pub connection_error: Rc<TypeObject>,
    pub connection_aborted_error: Rc<TypeObject>,
    pub connection_refused_error: Rc<TypeObject>,
    pub connection_reset_error: Rc<TypeObject>,
    pub file_exists_error: Rc<TypeObject>,
    pub file_not_found_error: Rc<TypeObject>,
    pub interrupted_error: Rc<TypeObject>,
    pub is_a_directory_error: Rc<TypeObject>,
    pub not_a_directory_error: Rc<TypeObject>,
    pub permission_error: Rc<TypeObject>,
    pub process_lookup_error: Rc<TypeObject>,

    pub eof_error: Rc<TypeObject>,
    pub buffer_error: Rc<TypeObject>,
    /// Raised on access through a dead weak proxy.
    pub reference_error: Rc<TypeObject>,
    pub memory_error: Rc<TypeObject>,
    pub system_error: Rc<TypeObject>,
    /// PEP 654 / RFC 0018 — exception group hierarchy.
    pub base_exception_group: Rc<TypeObject>,
    pub exception_group: Rc<TypeObject>,

    // RFC 0018 — `warnings` module hierarchy.
    pub warning: Rc<TypeObject>,
    pub user_warning: Rc<TypeObject>,
    pub deprecation_warning: Rc<TypeObject>,
    pub pending_deprecation_warning: Rc<TypeObject>,
    pub syntax_warning: Rc<TypeObject>,
    pub runtime_warning: Rc<TypeObject>,
    pub future_warning: Rc<TypeObject>,
    pub import_warning: Rc<TypeObject>,
    pub unicode_warning: Rc<TypeObject>,
    pub bytes_warning: Rc<TypeObject>,
    pub resource_warning: Rc<TypeObject>,
    pub encoding_warning: Rc<TypeObject>,
}

impl BuiltinTypes {
    /// Construct all built-in types. Single-inheritance only here —
    /// C3 cannot fail, so `expect` is appropriate and we don't risk
    /// recursing through [`crate::error::type_error`] before the
    /// registry exists.
    fn build() -> Self {
        let mk = |name: &str, bases: Vec<Rc<TypeObject>>| -> Rc<TypeObject> {
            TypeObject::new_builtin(name, bases).expect("built-in type must linearise")
        };
        let exc = |name: &str, base: Rc<TypeObject>| -> Rc<TypeObject> {
            TypeObject::new_exception(name, base).expect("built-in exception must linearise")
        };
        let object_ = mk("object", vec![]);
        // `object()` instances carry no `__dict__` (tp_dictoffset 0 in
        // CPython): attribute writes on a plain object raise
        // AttributeError, and weak references to one are refused.
        {
            // SAFETY: this is the only reference; nothing observes the
            // flag before the registry is published.
            let raw = Rc::as_ptr(&object_).cast_mut();
            unsafe { (*raw).forbids_dict = true };
        }
        let type_ = mk("type", vec![object_.clone()]);
        let property_ = mk("property", vec![object_.clone()]);
        let staticmethod_ = mk("staticmethod", vec![object_.clone()]);
        let classmethod_ = mk("classmethod", vec![object_.clone()]);
        // `staticmethod.__init__`/`classmethod.__init__` set `__func__`
        // (CPython's `sm_init`/`cm_init`); `__new__` leaves it `None`, so
        // a subclass overriding `__init__` without chaining keeps it
        // `None` (test_descr test_classmethod_new / test_staticmethod_new).
        install_descriptor_init(&staticmethod_, true);
        install_descriptor_init(&classmethod_, false);
        // Self-reference: `type.__class__ is type`. Every other
        // built-in's metaclass is `type` by default, installed in
        // bulk after the rest of the registry exists.
        type_.set_metaclass(type_.clone());
        object_.set_metaclass(type_.clone());
        install_object_dunders(&object_);
        install_type_dunders(&type_);

        let int_ = mk("int", vec![object_.clone()]);
        let float_ = mk("float", vec![object_.clone()]);
        let bool_ = mk("bool", vec![int_.clone()]);
        let complex_ = mk("complex", vec![object_.clone()]);
        let str_ = mk("str", vec![object_.clone()]);
        let bytes_ = mk("bytes", vec![object_.clone()]);
        let bytearray_ = mk("bytearray", vec![object_.clone()]);
        let tuple_ = mk("tuple", vec![object_.clone()]);
        let list_ = mk("list", vec![object_.clone()]);
        let dict_ = mk("dict", vec![object_.clone()]);
        let set_ = mk("set", vec![object_.clone()]);
        let frozenset_ = mk("frozenset", vec![object_.clone()]);
        let range_ = mk("range", vec![object_.clone()]);
        let slice_ = mk("slice", vec![object_.clone()]);
        let memoryview_ = mk("memoryview", vec![object_.clone()]);
        let mappingproxy_ = mk("mappingproxy", vec![object_.clone()]);
        // `types.MappingProxyType(mapping)` is a live read-only *view*:
        // wrapping a dict shares the underlying storage (CPython's
        // `mappingproxy_new`; enum's `__members__` builds one per access).
        {
            use crate::object::BuiltinFn;
            fn proxy_new(args: &[Object]) -> Result<Object, RuntimeError> {
                // args[0] is the class object. CPython's
                // `mappingproxy_check_mapping`: any `PyMapping_Check`
                // object except list/tuple qualifies — a dict subclass,
                // a ChainMap, any class with `__getitem__`.
                match args.get(1) {
                    Some(Object::Dict(d)) => Ok(Object::MappingProxy(d.clone())),
                    Some(p @ Object::MappingProxy(_)) | Some(p @ Object::MappingProxyObj(_)) => {
                        Ok(Object::MappingProxyObj(Rc::new(p.clone())))
                    }
                    Some(inst @ Object::Instance(i)) => {
                        // A list/tuple subclass is excluded like the base
                        // type (CPython uses PyList_Check / PyTuple_Check,
                        // which include subclasses).
                        let seq_backed = matches!(
                            i.native.get(),
                            Some(Object::List(_)) | Some(Object::Tuple(_))
                        );
                        let has_getitem = i.cls().lookup("__getitem__").is_some();
                        if !seq_backed && has_getitem {
                            Ok(Object::MappingProxyObj(Rc::new(inst.clone())))
                        } else {
                            Err(crate::error::type_error(format!(
                                "mappingproxy() argument must be a mapping, not {}",
                                inst.type_name()
                            )))
                        }
                    }
                    Some(other) => Err(crate::error::type_error(format!(
                        "mappingproxy() argument must be a mapping, not {}",
                        other.type_name()
                    ))),
                    None => Err(crate::error::type_error(
                        "mappingproxy() missing required argument 'mapping' (pos 1)",
                    )),
                }
            }
            mappingproxy_.dict.borrow_mut().insert(
                DictKey(Object::from_static("__new__")),
                Object::Builtin(Rc::new(BuiltinFn {
                    name: "__new__",
                    binds_instance: false,
                    call: Box::new(proxy_new),
                    call_kw: None,
                })),
            );
            crate::builtins::install_mappingproxy_methods(&mappingproxy_);
        }
        let dict_keys_ = mk("dict_keys", vec![object_.clone()]);
        let dict_values_ = mk("dict_values", vec![object_.clone()]);
        let dict_items_ = mk("dict_items", vec![object_.clone()]);
        // The view types have no `tp_new` in CPython — calling
        // `type({}.keys())(...)` raises "cannot create ... instances"
        // (test_dictviews.test_constructors_not_callable). Same for the
        // pickle/copy path: views are not reducible.
        for (ty, tname) in [
            (&dict_keys_, "dict_keys"),
            (&dict_values_, "dict_values"),
            (&dict_items_, "dict_items"),
        ] {
            use crate::object::BuiltinFn;
            ty.dict.borrow_mut().insert(
                DictKey(Object::from_static("__new__")),
                Object::Builtin(Rc::new(BuiltinFn {
                    name: "__new__",
                    binds_instance: false,
                    call: Box::new(move |_args| {
                        Err(crate::error::type_error(format!(
                            "cannot create '{tname}' instances"
                        )))
                    }),
                    call_kw: None,
                })),
            );
            ty.dict.borrow_mut().insert(
                DictKey(Object::from_static("__reduce__")),
                Object::Builtin(Rc::new(BuiltinFn {
                    name: "__reduce__",
                    binds_instance: true,
                    call: Box::new(move |_args| {
                        Err(crate::error::type_error(format!(
                            "cannot pickle '{tname}' object"
                        )))
                    }),
                    call_kw: None,
                })),
            );
        }
        let iterator_ = mk("iterator", vec![object_.clone()]);
        // The concrete iterator types have no `tp_new` either —
        // `type(iter('abc'))()` is a TypeError
        // (test_str.test_iterators_invocation).
        {
            use crate::object::BuiltinFn;
            iterator_.dict.borrow_mut().insert(
                DictKey(Object::from_static("__new__")),
                Object::Builtin(Rc::new(BuiltinFn {
                    name: "__new__",
                    binds_instance: false,
                    call: Box::new(|_args| {
                        Err(crate::error::type_error(
                            "cannot create 'iterator' instances",
                        ))
                    }),
                    call_kw: None,
                })),
            );
        }
        let enumerate_ = mk("enumerate", vec![object_.clone()]);
        let reversed_ = mk("reversed", vec![object_.clone()]);
        let none_type = mk("NoneType", vec![object_.clone()]);
        let ellipsis_ = mk("ellipsis", vec![object_.clone()]);
        let not_implemented_type_ = mk("NotImplementedType", vec![object_.clone()]);
        // CPython pickles the singletons by *global name*: `ellipsis`
        // defines `__reduce__` returning "Ellipsis" (and NotImplemented
        // likewise) — `pickle.dumps(Tuple[int, ...])` needs it.
        for (ty, global_name) in [
            (&ellipsis_, "Ellipsis"),
            (&not_implemented_type_, "NotImplemented"),
        ] {
            use crate::object::BuiltinFn;
            ty.dict.borrow_mut().insert(
                DictKey(Object::from_static("__reduce__")),
                Object::Builtin(Rc::new(BuiltinFn {
                    name: "__reduce__",
                    binds_instance: true,
                    call: Box::new(move |_args| Ok(Object::from_static(global_name))),
                    call_kw: None,
                })),
            );
        }
        let simple_namespace_ = mk("SimpleNamespace", vec![object_.clone()]);
        // CPython's `SimpleNamespace(**kwargs)` constructor: attributes
        // straight from the keywords (plus an optional mapping/iterable
        // positional in 3.13). `type(sys.implementation)(**kwargs)` is a
        // live pattern (test_import.SubinterpImportTests builds interp
        // configs that way).
        {
            use crate::object::BuiltinFn;
            let build_ns =
                |args: &[Object], kwargs: &[(String, Object)]| -> Result<Object, RuntimeError> {
                    // args[0] is the class object.
                    //
                    // CPython splits construction: `namespace_new` ignores
                    // the arguments entirely and `namespace_init` consumes
                    // them. A subclass overriding only `__init__` (e.g.
                    // test_capi's `PendingTask(payload)`) therefore never
                    // routes its constructor args through the mapping
                    // merge — its own `__init__` gets them. Mirror that:
                    // when the (user) class carries a Python `__init__`,
                    // allocate an empty namespace and stand aside.
                    let init_overridden = match args.first() {
                        Some(Object::Type(cls)) if !cls.flags.is_builtin => {
                            matches!(cls.lookup("__init__"), Some(Object::Function(_)))
                        }
                        _ => false,
                    };
                    if init_overridden {
                        let dict = Rc::new(RefCell::new(crate::object::DictData::default()));
                        if let Some(Object::Type(cls)) = args.first() {
                            let mut pi = crate::types::PyInstance::with_native(
                                cls.clone(),
                                Object::SimpleNamespace(dict.clone()),
                            );
                            pi.dict = dict;
                            let inst = Object::Instance(Rc::new(pi));
                            crate::gc_trace::track(inst.clone());
                            return Ok(inst);
                        }
                    }
                    if args.len() > 2 {
                        return Err(crate::error::type_error(
                            "SimpleNamespace() takes at most 1 positional argument",
                        ));
                    }
                    let dict = Rc::new(RefCell::new(crate::object::DictData::default()));
                    if let Some(mapping) = args.get(1) {
                        match mapping {
                            Object::Dict(m) => {
                                let entries: Vec<_> = m
                                    .borrow()
                                    .iter()
                                    .map(|(k, v)| (k.clone(), v.clone()))
                                    .collect();
                                let mut d = dict.borrow_mut();
                                for (k, v) in entries {
                                    d.insert(k, v);
                                }
                            }
                            other => {
                                // CPython's `namespace_init` runs `PyDict_Update`
                                // on the positional: any mapping (`keys()` +
                                // `__getitem__`) or iterable of key/value pairs
                                // qualifies (test_types test_constructor feeds a
                                // list of lists and a UserDict).
                                let interp = crate::builtins::reentrant_interp()?;
                                let g = interp.builtins_dict();
                                let tmp = Object::Dict(dict.clone());
                                interp.dict_merge_from(&tmp, other, &g)?;
                            }
                        }
                    }
                    {
                        let mut d = dict.borrow_mut();
                        for (k, v) in kwargs {
                            d.insert(DictKey(Object::from_str(k.clone())), v.clone());
                        }
                    }
                    // CPython's `namespace_init` re-checks the merged dict:
                    // every attribute name must be a string
                    // (`SimpleNamespace({1: 2})` → TypeError, test_constructor).
                    for (k, _) in dict.borrow().iter() {
                        if !matches!(k.0, Object::Str(_) | Object::WStr(_)) {
                            return Err(crate::error::type_error(format!(
                                "attribute name must be string, not '{}'",
                                k.0.type_name()
                            )));
                        }
                    }
                    let ns = Object::SimpleNamespace(dict.clone());
                    // Subclass constructor: the instance's `__dict__` *is* the
                    // namespace dict (CPython allocates `ns_dict` in the
                    // object struct; `vars(spam)` and plain setattr see the
                    // same storage — test_subclass / test_replace_subclass).
                    if let Some(Object::Type(cls)) = args.first() {
                        if !cls.flags.is_builtin {
                            let mut pi = crate::types::PyInstance::with_native(cls.clone(), ns);
                            pi.dict = dict;
                            let inst = Object::Instance(Rc::new(pi));
                            crate::gc_trace::track(inst.clone());
                            return Ok(inst);
                        }
                    }
                    Ok(ns)
                };
            let mut ns_dict = simple_namespace_.dict.borrow_mut();
            ns_dict.insert(
                DictKey(Object::from_static("__new__")),
                Object::Builtin(Rc::new(BuiltinFn {
                    name: "__new__",
                    binds_instance: false,
                    call: Box::new(move |args| build_ns(args, &[])),
                    call_kw: Some(Box::new(build_ns)),
                })),
            );
            // CPython tp_name is "types.SimpleNamespace"; pickle's
            // save-by-reference needs the module to resolve.
            ns_dict.insert(
                DictKey(Object::from_static("__module__")),
                Object::from_static("types"),
            );
            // `__reduce__` (all pickle protocols route here) and
            // `__replace__` (copy.replace, 3.13) — installed on the type
            // so subclass instances inherit them through the MRO and
            // `getattr(cls, '__replace__')` (copy.replace's probe) hits.
            ns_dict.insert(
                DictKey(Object::from_static("__reduce__")),
                Object::Builtin(Rc::new(BuiltinFn {
                    name: "__reduce__",
                    binds_instance: true,
                    call: Box::new(crate::builtins::namespace_reduce),
                    call_kw: None,
                })),
            );
            ns_dict.insert(
                DictKey(Object::from_static("__reduce_ex__")),
                Object::Builtin(Rc::new(BuiltinFn {
                    name: "__reduce_ex__",
                    binds_instance: true,
                    call: Box::new(crate::builtins::namespace_reduce),
                    call_kw: None,
                })),
            );
            ns_dict.insert(
                DictKey(Object::from_static("__replace__")),
                Object::Builtin(Rc::new(BuiltinFn {
                    name: "__replace__",
                    binds_instance: true,
                    call: Box::new(|args| crate::builtins::namespace_replace(args, &[])),
                    call_kw: Some(Box::new(crate::builtins::namespace_replace)),
                })),
            );
        }
        // PEP 585 / PEP 604 runtime types. The *instances* are
        // namespace-shaped (`Object::SimpleNamespace` carrying
        // `__origin__` / `__args__`), but their reported class must be
        // `types.GenericAlias` / `types.UnionType` as in CPython —
        // `functools` does `GenericAlias = type(list[int])` and then both
        // `isinstance(typ, GenericAlias)` and
        // `__class_getitem__ = classmethod(GenericAlias)`.
        let generic_alias_ = mk("GenericAlias", vec![object_.clone()]);
        let union_type_ = mk("UnionType", vec![object_.clone()]);
        for ty in [&generic_alias_, &union_type_] {
            // Not in `as_globals` (they live in `types`, not `builtins`),
            // so the bulk metaclass pass below won't reach them.
            ty.set_metaclass(type_.clone());
            let mut d = ty.dict.borrow_mut();
            d.insert(
                crate::object::DictKey(Object::from_static("__module__")),
                Object::from_static("types"),
            );
        }
        // (Their C tp_doc strings live in `builtin_type_doc` — the
        // `type.__doc__` getset path; test_pydoc test_union_type.)
        // CPython's `ga_new`: `GenericAlias(origin, args)` — also reached
        // through subclasses (`class SubClass(GenericAlias)`) and
        // `super().__new__(cls, ...)`, and rejects keyword arguments
        // (test_genericalias test_subclassing_types_genericalias).
        {
            use crate::object::BuiltinFn;
            let ga_ty = generic_alias_.clone();
            generic_alias_.dict.borrow_mut().insert(
                DictKey(Object::from_static("__new__")),
                Object::Builtin(Rc::new(BuiltinFn {
                    name: "__new__",
                    binds_instance: false,
                    call: Box::new(move |args| {
                        // args[0] is the class (SubClass or GenericAlias).
                        if args.len() != 3 {
                            return Err(crate::error::type_error(format!(
                                "GenericAlias expected 2 arguments, got {}",
                                args.len().saturating_sub(1)
                            )));
                        }
                        let alias =
                            crate::make_generic_alias_public(args[1].clone(), args[2].clone());
                        // CPython `ga_new` allocates through `cls`, so a
                        // subclass instance reports the subclass as its type
                        // (test_dataclasses test_is_dataclass_genericalias).
                        // Stamp the subclass; `class_of` honours it.
                        if let (Object::Type(cls), Object::SimpleNamespace(d)) = (&args[0], &alias)
                        {
                            if !Rc::ptr_eq(cls, &ga_ty) {
                                d.borrow_mut().insert(
                                    DictKey(Object::from_static("__class__")),
                                    args[0].clone(),
                                );
                            }
                        }
                        Ok(alias)
                    }),
                    call_kw: None,
                })),
            );
        }
        // CPython's `ga_reduce`: aliases pickle as `GenericAlias(origin,
        // args)` (test_type_aliases pickles `Alias[T]` round-trip).
        {
            use crate::object::BuiltinFn;
            let ga_ty = generic_alias_.clone();
            generic_alias_.dict.borrow_mut().insert(
                DictKey(Object::from_static("__reduce__")),
                Object::Builtin(Rc::new(BuiltinFn {
                    name: "__reduce__",
                    binds_instance: true,
                    call: Box::new(move |args| {
                        let d = match args.first() {
                            Some(Object::SimpleNamespace(d)) => d.borrow(),
                            _ => {
                                return Err(crate::error::type_error(
                                    "__reduce__ requires a GenericAlias".to_string(),
                                ))
                            }
                        };
                        let get = |name: &'static str| {
                            d.get(&DictKey(Object::from_static(name)))
                                .cloned()
                                .unwrap_or(Object::None)
                        };
                        Ok(Object::new_tuple(vec![
                            Object::Type(ga_ty.clone()),
                            Object::new_tuple(vec![get("__origin__"), get("__args__")]),
                        ]))
                    }),
                    call_kw: None,
                })),
            );
            // CPython `ga_mro_entries`: a PEP 585 alias used as a base
            // resolves to its origin. typing's `_GenericAlias.
            // __mro_entries__` also *calls* this on later bases to decide
            // whether Generic is already contributed (`class MySeq(List[T],
            // BaseSeq[T])` must not inject a second Generic).
            generic_alias_.dict.borrow_mut().insert(
                DictKey(Object::from_static("__mro_entries__")),
                Object::Builtin(Rc::new(BuiltinFn {
                    name: "__mro_entries__",
                    binds_instance: true,
                    call: Box::new(|args| {
                        let origin = match args.first() {
                            Some(Object::SimpleNamespace(d)) => d
                                .borrow()
                                .get(&DictKey(Object::from_static("__origin__")))
                                .cloned()
                                .unwrap_or(Object::None),
                            _ => {
                                return Err(crate::error::type_error(
                                    "__mro_entries__ requires a GenericAlias".to_string(),
                                ))
                            }
                        };
                        Ok(Object::new_tuple(vec![origin]))
                    }),
                    call_kw: None,
                })),
            );
        }
        let function_ = mk("function", vec![object_.clone()]);
        install_function_methods(&function_);
        // `types.MethodType` — the bound-method type. Distinct from
        // `function` so `type(obj.meth)` is `method` (as in CPython) and
        // `types.MethodType(func, obj)` can construct a bound method.
        let method_ = mk("method", vec![object_.clone()]);
        // `types.BuiltinFunctionType` — Rust-implemented callables.
        // CPython keeps this distinct from `function` (`type(len) is not
        // type(lambda: 0)`); `inspect`/`pydoc` classification relies on
        // the distinction.
        let builtin_function_ = mk("builtin_function_or_method", vec![object_.clone()]);
        // `types.MethodWrapperType` — a slot-wrapper dunder bound to an
        // instance (`object().__str__`).
        let method_wrapper_ = mk("method-wrapper", vec![object_.clone()]);
        // `types.MemberDescriptorType` — `__slots__` storage descriptors
        // (`type(A.x)` for `class A: __slots__ = ('x',)`). `dataclasses`
        // uses an isinstance check against this to recognize slot-shadowed
        // defaults.
        let member_descriptor_ = mk("member_descriptor", vec![object_.clone()]);
        install_member_descriptor_methods(&member_descriptor_);
        // The other three CPython descriptor types, distinguished by name so
        // `type(str.lower).__name__ == 'method_descriptor'` etc. hold
        // (test_qualname). Their instances are tagged via `descr_registry`.
        let method_descriptor_ = mk("method_descriptor", vec![object_.clone()]);
        let wrapper_descriptor_ = mk("wrapper_descriptor", vec![object_.clone()]);
        let getset_descriptor_ = mk("getset_descriptor", vec![object_.clone()]);
        let classmethod_descriptor_ = mk("classmethod_descriptor", vec![object_.clone()]);
        // `super` is a real, subclassable type (`class mysuper(super)`,
        // test_supers). Its instances are ordinary `PyInstance`s carrying
        // `__thisclass__`/`__self__`/`__self_class__`; attribute access is
        // special-cased in `load_attr_instance_default`.
        let super_ = mk("super", vec![object_.clone()]);
        install_super_methods(&super_);
        let generator_ = mk("generator", vec![object_.clone()]);
        let coroutine_ = mk("coroutine", vec![object_.clone()]);
        let async_generator_ = mk("async_generator", vec![object_.clone()]);
        // The single-shot awaitables behind `asend`/`athrow`/`aclose`.
        // CPython names them `async_generator_asend` / `_athrow`; `aclose`
        // reuses the `_athrow` type.
        let async_generator_asend_ = mk("async_generator_asend", vec![object_.clone()]);
        let async_generator_athrow_ = mk("async_generator_athrow", vec![object_.clone()]);
        install_gen_name_getsets(&generator_, "generator");
        install_gen_name_getsets(&coroutine_, "coroutine");
        install_gen_name_getsets(&async_generator_, "async generator");
        let frame_ = mk("frame", vec![object_.clone()]);
        install_frame_getsets(&frame_);
        let code_ = mk("code", vec![object_.clone()]);
        // `copy.replace(code, …)` resolves `type(code).__replace__` and
        // calls it unbound with the code object first (RFC 0060 /
        // test_code.test_replace's copy.replace legs).
        code_.dict.borrow_mut().insert(
            DictKey(Object::from_static("__replace__")),
            crate::builtins::code_dunder_replace_object(),
        );
        let traceback_ = mk("traceback", vec![object_.clone()]);
        let cell_ = mk("cell", vec![object_.clone()]);
        let module_ = mk("module", vec![object_.clone()]);
        install_module_init(&module_);
        install_module_methods(&module_);

        let base_exception = exc("BaseException", object_.clone());
        let exception = exc("Exception", base_exception.clone());

        // Hang `__str__` / `__repr__` off `BaseException` so that
        // `str(ValueError("msg"))` / `print(exc)` produce the
        // CPython-familiar message rather than the generic
        // "<X object at 0x...>" instance repr.
        install_exception_str_repr(&base_exception);
        // CPython's `BaseException` getsets default to None/False/() —
        // an instance that was never raised still answers
        // `e.__traceback__` etc. Instance dicts shadow these when the
        // raise machinery (or user code) sets real values.
        {
            let mut d = base_exception.dict.borrow_mut();
            for key in ["__traceback__", "__context__", "__cause__"] {
                d.insert(
                    crate::object::DictKey(Object::from_static(key)),
                    exc_slot(key, "BaseException", Object::None),
                );
            }
            d.insert(
                crate::object::DictKey(Object::from_static("__suppress_context__")),
                exc_slot("__suppress_context__", "BaseException", Object::Bool(false)),
            );
            d.insert(
                crate::object::DictKey(Object::from_static("args")),
                exc_slot("args", "BaseException", Object::new_tuple(Vec::new())),
            );
        }

        let arithmetic_error = exc("ArithmeticError", exception.clone());
        let assertion_error = exc("AssertionError", exception.clone());
        let attribute_error = exc("AttributeError", exception.clone());
        let import_error = exc("ImportError", exception.clone());
        let module_not_found_error = exc("ModuleNotFoundError", import_error.clone());
        let lookup_error = exc("LookupError", exception.clone());
        let index_error = exc("IndexError", lookup_error.clone());
        let key_error = exc("KeyError", lookup_error.clone());
        let name_error = exc("NameError", exception.clone());
        let unbound_local_error = exc("UnboundLocalError", name_error.clone());
        // Structured-field defaults mirroring CPython's getset members:
        // raise sites / keyword constructors override per instance, and
        // unenriched instances read `None` (`AttributeError("m").name`).
        fn install_field_defaults(ty: &Rc<TypeObject>, fields: &[&'static str]) {
            let mut d = ty.dict.borrow_mut();
            for f in fields {
                d.insert(
                    crate::object::DictKey(Object::from_static(f)),
                    exc_slot(f, &ty.name, Object::None),
                );
            }
        }
        install_field_defaults(&attribute_error, &["name", "obj"]);
        install_field_defaults(&name_error, &["name"]);
        install_field_defaults(&import_error, &["msg", "name", "path", "name_from"]);
        install_import_error_init(&import_error);
        let os_error = exc("OSError", exception.clone());
        install_os_error_init(&os_error);
        let runtime_error = exc("RuntimeError", exception.clone());
        let not_implemented_error = exc("NotImplementedError", runtime_error.clone());
        let recursion_error = exc("RecursionError", runtime_error.clone());
        let python_finalization_error = exc("PythonFinalizationError", runtime_error.clone());
        let overflow_error = exc("OverflowError", arithmetic_error.clone());
        let floating_point_error = exc("FloatingPointError", arithmetic_error.clone());
        let zero_division_error = exc("ZeroDivisionError", arithmetic_error.clone());
        let stop_iteration = exc("StopIteration", exception.clone());
        // PEP 525: `StopAsyncIteration` is a sibling of `StopIteration`
        // in CPython's hierarchy, not a subclass.
        let stop_async_iteration = exc("StopAsyncIteration", exception.clone());
        let syntax_error = exc("SyntaxError", exception.clone());
        // CPython's `SyntaxError.__init__` unpacks the
        // `(filename, lineno, offset, text[, end_lineno, end_offset])`
        // detail tuple into attributes, and its `__str__` appends
        // `" (<basename>, line N)"`. Install both so the type behaves as a
        // drop-in whether constructed from Python or raised from Rust.
        install_syntax_error_dunders(&syntax_error);
        let incomplete_input_error = exc("_IncompleteInputError", syntax_error.clone());
        let indentation_error = exc("IndentationError", syntax_error.clone());
        let tab_error = exc("TabError", indentation_error.clone());
        // `TimeoutError` lands here so `asyncio.wait_for` raises a
        // public, importable type rather than a synthetic shim.
        let timeout_error = exc("TimeoutError", os_error.clone());
        let type_error = exc("TypeError", exception.clone());
        let value_error = exc("ValueError", exception.clone());
        // Unicode error hierarchy: `UnicodeError` derives from
        // `ValueError`, and the three concrete codecs errors derive from
        // it. CPython gives the concrete three extra attributes
        // (`encoding`/`object`/`start`/`end`/`reason`) populated by their
        // `__init__`; install those so `str(UnicodeDecodeError(...))` and
        // attribute access match.
        let unicode_error = exc("UnicodeError", value_error.clone());
        let unicode_encode_error = exc("UnicodeEncodeError", unicode_error.clone());
        let unicode_decode_error = exc("UnicodeDecodeError", unicode_error.clone());
        let unicode_translate_error = exc("UnicodeTranslateError", unicode_error.clone());
        install_unicode_error_dunders(&unicode_encode_error, UnicodeErrorKind::Encode);
        install_unicode_error_dunders(&unicode_decode_error, UnicodeErrorKind::Decode);
        install_unicode_error_dunders(&unicode_translate_error, UnicodeErrorKind::Translate);
        let generator_exit = exc("GeneratorExit", base_exception.clone());
        let keyboard_interrupt = exc("KeyboardInterrupt", base_exception.clone());
        let system_exit = exc("SystemExit", base_exception.clone());

        // PEP 3151 OSError hierarchy. ConnectionError is itself a
        // subclass of OSError; the concrete connection types hang
        // off it. BrokenPipeError's MRO in CPython is
        // [BrokenPipeError, ConnectionError, OSError, ...]; we
        // realise it via single-inheritance through ConnectionError
        // for the same observable lookup walk.
        let blocking_io_error = exc("BlockingIOError", os_error.clone());
        let connection_error = exc("ConnectionError", os_error.clone());
        let broken_pipe_error = exc("BrokenPipeError", connection_error.clone());
        let child_process_error = exc("ChildProcessError", os_error.clone());
        let connection_aborted_error = exc("ConnectionAbortedError", connection_error.clone());
        let connection_refused_error = exc("ConnectionRefusedError", connection_error.clone());
        let connection_reset_error = exc("ConnectionResetError", connection_error.clone());
        let file_exists_error = exc("FileExistsError", os_error.clone());
        let file_not_found_error = exc("FileNotFoundError", os_error.clone());
        let interrupted_error = exc("InterruptedError", os_error.clone());
        let is_a_directory_error = exc("IsADirectoryError", os_error.clone());
        let not_a_directory_error = exc("NotADirectoryError", os_error.clone());
        let permission_error = exc("PermissionError", os_error.clone());
        let process_lookup_error = exc("ProcessLookupError", os_error.clone());

        let eof_error = exc("EOFError", exception.clone());
        let buffer_error = exc("BufferError", exception.clone());
        let reference_error = exc("ReferenceError", exception.clone());
        let memory_error = exc("MemoryError", exception.clone());
        let system_error = exc("SystemError", exception.clone());

        // RFC 0018 — Warning hierarchy.
        let warning = exc("Warning", exception.clone());
        let user_warning = exc("UserWarning", warning.clone());
        let deprecation_warning = exc("DeprecationWarning", warning.clone());
        let pending_deprecation_warning = exc("PendingDeprecationWarning", warning.clone());
        let syntax_warning = exc("SyntaxWarning", warning.clone());
        let runtime_warning = exc("RuntimeWarning", warning.clone());
        let future_warning = exc("FutureWarning", warning.clone());
        let import_warning = exc("ImportWarning", warning.clone());
        let unicode_warning = exc("UnicodeWarning", warning.clone());
        let bytes_warning = exc("BytesWarning", warning.clone());
        let resource_warning = exc("ResourceWarning", warning.clone());
        let encoding_warning = exc("EncodingWarning", warning.clone());

        // PEP 654: BaseExceptionGroup derives from BaseException;
        // ExceptionGroup is a sibling subclass that also derives
        // from Exception so it's caught by `except Exception:`. We
        // model the dual inheritance via the C3 linearisation —
        // ExceptionGroup's bases are (BaseExceptionGroup, Exception)
        // and the resulting MRO is
        //   [ExceptionGroup, BaseExceptionGroup, Exception,
        //    BaseException, object]
        // which matches CPython.
        let base_exception_group = exc("BaseExceptionGroup", base_exception.clone());
        let exception_group = TypeObject::new_with_flags(
            "ExceptionGroup",
            vec![base_exception_group.clone(), exception.clone()],
            DictData::default(),
            crate::types::TypeFlags {
                is_exception: true,
                is_builtin: true,
            },
        )
        .expect("ExceptionGroup MRO");
        install_exception_group_init(&base_exception_group);
        // Exception pseudo-slots (RFC 0057): CPython keeps these in C
        // struct members/getsets outside the instance `__dict__`, so
        // `vars(e)` never shows them. Reads of an unset slot answer the
        // descriptor default; writes land in the slot side table.
        install_field_defaults(&stop_iteration, &["value"]);
        install_field_defaults(&system_exit, &["code"]);
        install_field_defaults(
            &syntax_error,
            &[
                "msg",
                "filename",
                "lineno",
                "offset",
                "text",
                "end_lineno",
                "end_offset",
                "print_file_and_line",
            ],
        );
        install_field_defaults(
            &unicode_error,
            &["encoding", "object", "start", "end", "reason"],
        );
        // CPython declares `message`/`exceptions` as `Py_READONLY`
        // members of the C struct: Python-level assignment raises
        // `AttributeError("readonly attribute")`.
        {
            let mut d = base_exception_group.dict.borrow_mut();
            for f in ["message", "exceptions"] {
                d.insert(
                    crate::object::DictKey(Object::from_static(f)),
                    exc_slot_readonly(f, "BaseExceptionGroup", Object::None),
                );
            }
        }

        let bt = BuiltinTypes {
            object_: object_.clone(),
            type_: type_.clone(),
            property_: property_.clone(),
            staticmethod_: staticmethod_.clone(),
            classmethod_: classmethod_.clone(),
            int_,
            float_,
            bool_,
            complex_,
            str_,
            bytes_,
            bytearray_,
            tuple_,
            list_,
            dict_,
            set_,
            frozenset_,
            range_,
            slice_,
            memoryview_,
            mappingproxy_,
            dict_keys_,
            dict_values_,
            dict_items_,
            iterator_,
            enumerate_,
            reversed_,
            none_type,
            ellipsis_,
            not_implemented_type_,
            simple_namespace_,
            generic_alias_,
            union_type_,
            function_,
            method_,
            builtin_function_,
            method_wrapper_,
            member_descriptor_,
            method_descriptor_,
            wrapper_descriptor_,
            classmethod_descriptor_,
            getset_descriptor_,
            super_,
            generator_,
            coroutine_,
            async_generator_,
            async_generator_asend_,
            async_generator_athrow_,
            frame_,
            code_,
            traceback_,
            cell_,
            module_,
            base_exception,
            exception,
            arithmetic_error,
            assertion_error,
            attribute_error,
            import_error,
            module_not_found_error,
            index_error,
            key_error,
            lookup_error,
            name_error,
            not_implemented_error,
            os_error,
            overflow_error,
            floating_point_error,
            runtime_error,
            stop_iteration,
            stop_async_iteration,
            syntax_error,
            indentation_error,
            tab_error,
            timeout_error,
            type_error,
            unbound_local_error,
            value_error,
            unicode_error,
            unicode_encode_error,
            unicode_decode_error,
            unicode_translate_error,
            zero_division_error,
            generator_exit,
            keyboard_interrupt,
            system_exit,
            recursion_error,
            python_finalization_error,
            incomplete_input_error,
            blocking_io_error,
            broken_pipe_error,
            child_process_error,
            connection_error,
            connection_aborted_error,
            connection_refused_error,
            connection_reset_error,
            file_exists_error,
            file_not_found_error,
            interrupted_error,
            is_a_directory_error,
            not_a_directory_error,
            permission_error,
            process_lookup_error,
            eof_error,
            buffer_error,
            reference_error,
            memory_error,
            system_error,
            base_exception_group,
            exception_group,
            warning,
            user_warning,
            deprecation_warning,
            pending_deprecation_warning,
            syntax_warning,
            runtime_warning,
            future_warning,
            import_warning,
            unicode_warning,
            bytes_warning,
            resource_warning,
            encoding_warning,
        };
        // Every other built-in type's metaclass is `type`.
        for (_, value) in bt.as_globals() {
            if let Object::Type(t) = value {
                if t.metaclass.borrow().is_none() {
                    t.set_metaclass(type_.clone());
                }
            }
        }
        // RFC 0019 — install numeric/bytes class methods.
        install_numeric_class_methods(&bt);
        // Install `__new__` in each value/container type's own dict (CPython
        // keeps a distinct `tp_new` per type). Needed so `'__new__' in
        // int.__dict__` is True — `enum._find_data_type_` uses exactly this to
        // recognise `int`/`str`/… as the mix-in data type.
        install_value_type_new(&bt);
        // RFC 0037 — materialize the full method/dunder surface into the
        // type dicts (CPython's `tp_dict` parity: `vars(list)`,
        // `bytearray.__hash__ is None`, `_check_methods`-style ABC hooks).
        crate::type_surface::install(&bt);
        // Native callables are descriptors: `func.__get__(obj)` already
        // binds at the instance level. Mirror the *type*-level slot so
        // `getattr(type(descr), '__get__')` resolves too — the form
        // `inspect._descriptor_get` uses to bind a native
        // `__call__`/`__init__` before reading its signature. Without it
        // an Argument-Clinic `$self`/`$module` marker is not stripped and
        // the signature keeps a spurious leading parameter (test_operator,
        // test_functools). Installed after `type_surface` so it lands on
        // the final, fully-populated descriptor type dicts.
        install_builtin_descriptor_get(&bt.method_descriptor_);
        install_builtin_descriptor_get(&bt.wrapper_descriptor_);
        install_classmethod_descriptor_get(&bt.classmethod_descriptor_);
        // NOT on `builtin_function_or_method`: CPython's PyCFunction type
        // has no `tp_descr_get` — a bound builtin stored as a class attr
        // (`__call__ = ''.join`) is used as-is, never re-bound (test_inspect
        // test_signature_on_callable_objects [BuiltinMethodType]).
        bt
    }

    /// Public copies of each built-in type as `Object::Type` values,
    /// suitable for installing into module globals.
    pub fn as_globals(&self) -> Vec<(String, Object)> {
        macro_rules! pair {
            ($field:ident, $name:literal) => {
                ($name.to_owned(), Object::Type(self.$field.clone()))
            };
        }
        vec![
            pair!(object_, "object"),
            pair!(type_, "type"),
            pair!(property_, "property"),
            pair!(staticmethod_, "staticmethod"),
            pair!(classmethod_, "classmethod"),
            pair!(int_, "int"),
            pair!(float_, "float"),
            pair!(bool_, "bool"),
            pair!(complex_, "complex"),
            pair!(str_, "str"),
            pair!(bytes_, "bytes"),
            pair!(bytearray_, "bytearray"),
            pair!(tuple_, "tuple"),
            pair!(list_, "list"),
            pair!(dict_, "dict"),
            pair!(set_, "set"),
            pair!(frozenset_, "frozenset"),
            pair!(range_, "range"),
            pair!(slice_, "slice"),
            pair!(memoryview_, "memoryview"),
            pair!(enumerate_, "enumerate"),
            pair!(reversed_, "reversed"),
            // `super` is a real type (`super(C, obj)`, `class mysuper(super)`).
            // The `Interpreter::default` seed overrides the function-flavoured
            // `super` entry with this type; construction routes through
            // `instantiate`'s `"super"` case.
            pair!(super_, "super"),
            pair!(base_exception, "BaseException"),
            pair!(exception, "Exception"),
            pair!(arithmetic_error, "ArithmeticError"),
            pair!(assertion_error, "AssertionError"),
            pair!(attribute_error, "AttributeError"),
            pair!(import_error, "ImportError"),
            pair!(module_not_found_error, "ModuleNotFoundError"),
            pair!(index_error, "IndexError"),
            pair!(key_error, "KeyError"),
            pair!(lookup_error, "LookupError"),
            pair!(name_error, "NameError"),
            pair!(not_implemented_error, "NotImplementedError"),
            pair!(os_error, "OSError"),
            // `IOError` and `EnvironmentError` are the same object as `OSError`
            // in Python 3 (`IOError is OSError`), kept as builtin aliases.
            pair!(os_error, "IOError"),
            pair!(os_error, "EnvironmentError"),
            pair!(overflow_error, "OverflowError"),
            pair!(floating_point_error, "FloatingPointError"),
            pair!(runtime_error, "RuntimeError"),
            pair!(stop_iteration, "StopIteration"),
            pair!(stop_async_iteration, "StopAsyncIteration"),
            pair!(syntax_error, "SyntaxError"),
            pair!(indentation_error, "IndentationError"),
            pair!(tab_error, "TabError"),
            pair!(timeout_error, "TimeoutError"),
            pair!(type_error, "TypeError"),
            pair!(unbound_local_error, "UnboundLocalError"),
            pair!(value_error, "ValueError"),
            pair!(unicode_error, "UnicodeError"),
            pair!(unicode_encode_error, "UnicodeEncodeError"),
            pair!(unicode_decode_error, "UnicodeDecodeError"),
            pair!(unicode_translate_error, "UnicodeTranslateError"),
            pair!(zero_division_error, "ZeroDivisionError"),
            pair!(generator_exit, "GeneratorExit"),
            pair!(keyboard_interrupt, "KeyboardInterrupt"),
            pair!(system_exit, "SystemExit"),
            pair!(recursion_error, "RecursionError"),
            pair!(python_finalization_error, "PythonFinalizationError"),
            pair!(incomplete_input_error, "_IncompleteInputError"),
            pair!(blocking_io_error, "BlockingIOError"),
            pair!(broken_pipe_error, "BrokenPipeError"),
            pair!(child_process_error, "ChildProcessError"),
            pair!(connection_error, "ConnectionError"),
            pair!(connection_aborted_error, "ConnectionAbortedError"),
            pair!(connection_refused_error, "ConnectionRefusedError"),
            pair!(connection_reset_error, "ConnectionResetError"),
            pair!(file_exists_error, "FileExistsError"),
            pair!(file_not_found_error, "FileNotFoundError"),
            pair!(interrupted_error, "InterruptedError"),
            pair!(is_a_directory_error, "IsADirectoryError"),
            pair!(not_a_directory_error, "NotADirectoryError"),
            pair!(permission_error, "PermissionError"),
            pair!(process_lookup_error, "ProcessLookupError"),
            pair!(eof_error, "EOFError"),
            pair!(buffer_error, "BufferError"),
            pair!(reference_error, "ReferenceError"),
            pair!(memory_error, "MemoryError"),
            pair!(system_error, "SystemError"),
            pair!(base_exception_group, "BaseExceptionGroup"),
            pair!(exception_group, "ExceptionGroup"),
            pair!(warning, "Warning"),
            pair!(user_warning, "UserWarning"),
            pair!(deprecation_warning, "DeprecationWarning"),
            pair!(pending_deprecation_warning, "PendingDeprecationWarning"),
            pair!(syntax_warning, "SyntaxWarning"),
            pair!(runtime_warning, "RuntimeWarning"),
            pair!(future_warning, "FutureWarning"),
            pair!(import_warning, "ImportWarning"),
            pair!(unicode_warning, "UnicodeWarning"),
            pair!(bytes_warning, "BytesWarning"),
            pair!(resource_warning, "ResourceWarning"),
            pair!(encoding_warning, "EncodingWarning"),
        ]
    }

    /// Find a built-in type by its bare name. Used by error helpers
    /// in cold paths where keeping the field name in code would
    /// double the boilerplate.
    pub fn by_name(&self, name: &str) -> Option<Rc<TypeObject>> {
        match name {
            "object" => Some(self.object_.clone()),
            "type" => Some(self.type_.clone()),
            "int" => Some(self.int_.clone()),
            "float" => Some(self.float_.clone()),
            "bool" => Some(self.bool_.clone()),
            "complex" => Some(self.complex_.clone()),
            "str" => Some(self.str_.clone()),
            "bytes" => Some(self.bytes_.clone()),
            "bytearray" => Some(self.bytearray_.clone()),
            "tuple" => Some(self.tuple_.clone()),
            "list" => Some(self.list_.clone()),
            "dict" => Some(self.dict_.clone()),
            "set" => Some(self.set_.clone()),
            "frozenset" => Some(self.frozenset_.clone()),
            "range" => Some(self.range_.clone()),
            "slice" => Some(self.slice_.clone()),
            "memoryview" => Some(self.memoryview_.clone()),
            "enumerate" => Some(self.enumerate_.clone()),
            "reversed" => Some(self.reversed_.clone()),
            "mappingproxy" => Some(self.mappingproxy_.clone()),
            "dict_keys" => Some(self.dict_keys_.clone()),
            "dict_values" => Some(self.dict_values_.clone()),
            "dict_items" => Some(self.dict_items_.clone()),
            "frame" => Some(self.frame_.clone()),
            "code" => Some(self.code_.clone()),
            "traceback" => Some(self.traceback_.clone()),
            "BaseException" => Some(self.base_exception.clone()),
            "Exception" => Some(self.exception.clone()),
            "ArithmeticError" => Some(self.arithmetic_error.clone()),
            "AssertionError" => Some(self.assertion_error.clone()),
            "AttributeError" => Some(self.attribute_error.clone()),
            "ImportError" => Some(self.import_error.clone()),
            "ModuleNotFoundError" => Some(self.module_not_found_error.clone()),
            "IndexError" => Some(self.index_error.clone()),
            "KeyError" => Some(self.key_error.clone()),
            "LookupError" => Some(self.lookup_error.clone()),
            "NameError" => Some(self.name_error.clone()),
            "NotImplementedError" => Some(self.not_implemented_error.clone()),
            "OSError" | "IOError" | "EnvironmentError" => Some(self.os_error.clone()),
            "OverflowError" => Some(self.overflow_error.clone()),
            "FloatingPointError" => Some(self.floating_point_error.clone()),
            "RuntimeError" => Some(self.runtime_error.clone()),
            "StopIteration" => Some(self.stop_iteration.clone()),
            "StopAsyncIteration" => Some(self.stop_async_iteration.clone()),
            "SyntaxError" => Some(self.syntax_error.clone()),
            "IndentationError" => Some(self.indentation_error.clone()),
            "TabError" => Some(self.tab_error.clone()),
            "TimeoutError" => Some(self.timeout_error.clone()),
            "TypeError" => Some(self.type_error.clone()),
            "UnboundLocalError" => Some(self.unbound_local_error.clone()),
            "ValueError" => Some(self.value_error.clone()),
            "UnicodeError" => Some(self.unicode_error.clone()),
            "UnicodeEncodeError" => Some(self.unicode_encode_error.clone()),
            "UnicodeDecodeError" => Some(self.unicode_decode_error.clone()),
            "UnicodeTranslateError" => Some(self.unicode_translate_error.clone()),
            "ZeroDivisionError" => Some(self.zero_division_error.clone()),
            "GeneratorExit" => Some(self.generator_exit.clone()),
            "KeyboardInterrupt" => Some(self.keyboard_interrupt.clone()),
            "SystemExit" => Some(self.system_exit.clone()),
            "RecursionError" => Some(self.recursion_error.clone()),
            "PythonFinalizationError" => Some(self.python_finalization_error.clone()),
            "_IncompleteInputError" => Some(self.incomplete_input_error.clone()),
            "BlockingIOError" => Some(self.blocking_io_error.clone()),
            "BrokenPipeError" => Some(self.broken_pipe_error.clone()),
            "ChildProcessError" => Some(self.child_process_error.clone()),
            "ConnectionError" => Some(self.connection_error.clone()),
            "ConnectionAbortedError" => Some(self.connection_aborted_error.clone()),
            "ConnectionRefusedError" => Some(self.connection_refused_error.clone()),
            "ConnectionResetError" => Some(self.connection_reset_error.clone()),
            "FileExistsError" => Some(self.file_exists_error.clone()),
            "FileNotFoundError" => Some(self.file_not_found_error.clone()),
            "InterruptedError" => Some(self.interrupted_error.clone()),
            "IsADirectoryError" => Some(self.is_a_directory_error.clone()),
            "NotADirectoryError" => Some(self.not_a_directory_error.clone()),
            "PermissionError" => Some(self.permission_error.clone()),
            "ProcessLookupError" => Some(self.process_lookup_error.clone()),
            "EOFError" => Some(self.eof_error.clone()),
            "BufferError" => Some(self.buffer_error.clone()),
            "ReferenceError" => Some(self.reference_error.clone()),
            "MemoryError" => Some(self.memory_error.clone()),
            "SystemError" => Some(self.system_error.clone()),
            "BaseExceptionGroup" => Some(self.base_exception_group.clone()),
            "ExceptionGroup" => Some(self.exception_group.clone()),
            "Warning" => Some(self.warning.clone()),
            "UserWarning" => Some(self.user_warning.clone()),
            "DeprecationWarning" => Some(self.deprecation_warning.clone()),
            "PendingDeprecationWarning" => Some(self.pending_deprecation_warning.clone()),
            "SyntaxWarning" => Some(self.syntax_warning.clone()),
            "RuntimeWarning" => Some(self.runtime_warning.clone()),
            "FutureWarning" => Some(self.future_warning.clone()),
            "ImportWarning" => Some(self.import_warning.clone()),
            "UnicodeWarning" => Some(self.unicode_warning.clone()),
            "BytesWarning" => Some(self.bytes_warning.clone()),
            "ResourceWarning" => Some(self.resource_warning.clone()),
            "EncodingWarning" => Some(self.encoding_warning.clone()),
            _ => None,
        }
    }
}

thread_local! {
    static BUILTIN_TYPES: RefCell<Option<Rc<BuiltinTypes>>> = const { RefCell::new(None) };
    static PROPERTY_CLASS: RefCell<Option<Rc<TypeObject>>> = const { RefCell::new(None) };
}

/// Drop this thread's lazily-built type registry (and the derived
/// `property` class) *now*, while the caller still holds the GIL. A worker
/// thread's TLS destructors would otherwise release this whole per-thread
/// type graph after the GIL has been dropped — those unsynchronised `Rc`
/// decrements race a peer thread's in-flight GC mark phase, which reads
/// `Rc::strong_count` snapshots and intermittently mis-classifies the
/// peer's *live* objects as garbage (test_threading.test_foreign_thread).
pub fn clear_thread_type_registry() {
    let _ = PROPERTY_CLASS.try_with(|slot| slot.borrow_mut().take());
    let _ = BUILTIN_TYPES.try_with(|slot| slot.borrow_mut().take());
}

/// Per-thread accessor. The registry is constructed lazily on first
/// access. Panics if construction fails — that means the C3 invariant
/// is broken on the built-in hierarchy itself.
pub fn property_class() -> Rc<TypeObject> {
    PROPERTY_CLASS.with(|slot| {
        if let Some(c) = slot.borrow().as_ref() {
            return c.clone();
        }
        let bt = builtin_types();
        let cls = TypeObject::new_user("property", vec![bt.object_.clone()], DictData::default())
            .expect("property type");
        *slot.borrow_mut() = Some(cls.clone());
        cls
    })
}

pub fn builtin_types() -> Rc<BuiltinTypes> {
    let (bt, fresh) = BUILTIN_TYPES.with(|cell| {
        if cell.borrow().is_none() {
            let bt = Rc::new(BuiltinTypes::build());
            *cell.borrow_mut() = Some(bt.clone());
            (bt, true)
        } else {
            (cell.borrow().as_ref().unwrap().clone(), false)
        }
    });
    if fresh {
        // Deferred surface pass (RFC 0056 WS4): synthesizing descriptor-
        // type members re-enters `builtin_types()`, which must resolve to
        // the just-published cell rather than recursively rebuild.
        crate::type_surface::install_docs_table_surface(&bt);
    }
    bt
}

/// Resolve `__objclass__` for a built-in method/slot-wrapper object by
/// locating the built-in type whose dict holds this exact descriptor
/// (CPython stores the owner in the descriptor itself; we recover it
/// by identity search over the materialized type dicts).
pub fn builtin_fn_objclass(b: &Rc<crate::object::BuiltinFn>) -> Option<Rc<TypeObject>> {
    let bt = builtin_types();
    let candidates: &[&Rc<TypeObject>] = &[
        &bt.object_,
        &bt.type_,
        &bt.int_,
        &bt.float_,
        &bt.bool_,
        &bt.complex_,
        &bt.str_,
        &bt.bytes_,
        &bt.bytearray_,
        &bt.tuple_,
        &bt.list_,
        &bt.dict_,
        &bt.set_,
        &bt.frozenset_,
        &bt.range_,
        &bt.slice_,
        &bt.memoryview_,
        &bt.mappingproxy_,
        &bt.dict_keys_,
        &bt.dict_values_,
        &bt.dict_items_,
        &bt.iterator_,
        &bt.none_type,
        &bt.function_,
        &bt.method_,
        &bt.builtin_function_,
        &bt.method_wrapper_,
        &bt.member_descriptor_,
        &bt.generator_,
        &bt.coroutine_,
        &bt.module_,
        &bt.property_,
        &bt.staticmethod_,
        &bt.classmethod_,
        &bt.base_exception,
    ];
    let needle = Rc::as_ptr(b);
    for ty in candidates {
        for (_, v) in ty.dict.borrow().iter() {
            if let Object::Builtin(other) = v {
                if Rc::as_ptr(other) == needle {
                    return Some((*ty).clone());
                }
            }
        }
    }
    None
}

/// RFC 0025: adopt an existing registry on this thread. Worker threads
/// forked from the interpreter seed must see the *same* `type`,
/// `object`, … `TypeObject`s as the seed thread — class statements
/// compare metaclasses by pointer, so a worker that lazily built its
/// own registry would hit "metaclass conflict" on any class whose
/// bases came from the seed thread (e.g. importing a frozen module
/// inside a `threading.Thread`).
pub fn install_shared(bt: Rc<BuiltinTypes>) {
    BUILTIN_TYPES.with(|cell| {
        let mut slot = cell.borrow_mut();
        if slot.is_none() {
            *slot = Some(bt);
        }
    });
}

/// A [`crate::object::SlotDescriptor`] for an exception pseudo-slot:
/// reads of an unset slot answer `default` (mirroring CPython's
/// getset/member defaults) and writes land in the instance's slot side
/// table — never the `__dict__`, so `vars(e)` stays clean.
fn exc_slot(name: &str, class_name: &str, default: Object) -> Object {
    Object::SlotDescriptor(Rc::new(crate::object::SlotDescriptor {
        name: name.to_owned(),
        class_name: class_name.to_owned(),
        default: Some(default),
        readonly: false,
        doc: None,
        objclass: crate::sync::RefCell::new(None),
    }))
}

/// A read-only exception pseudo-slot (CPython `Py_READONLY` member):
/// `BaseExceptionGroup.message` / `.exceptions` reject Python-level
/// assignment and deletion with `AttributeError("readonly attribute")`.
fn exc_slot_readonly(name: &str, class_name: &str, default: Object) -> Object {
    Object::SlotDescriptor(Rc::new(crate::object::SlotDescriptor {
        name: name.to_owned(),
        class_name: class_name.to_owned(),
        default: Some(default),
        readonly: true,
        doc: None,
        objclass: crate::sync::RefCell::new(None),
    }))
}

/// Read an exception pseudo-slot: the slot side table first, then the
/// instance `__dict__` (a user subclass may have stored a plain
/// same-named attribute before the descriptor existed — e.g. state
/// applied by an old pickle, or `self.args = …` in a shadowing
/// `__init__` that ran before the class descriptor was reachable).
pub(crate) fn exc_attr(inst: &crate::types::PyInstance, name: &str) -> Option<Object> {
    inst.slot_get(name).or_else(|| {
        inst.dict
            .borrow()
            .get(&crate::object::StrKey(name))
            .cloned()
    })
}

/// Construct an exception instance of `class_name` with `message` as
/// `args[0]`. Used by Rust-side error helpers.
pub fn make_exception(class_name: &str, message: impl Into<String>) -> Object {
    let bt = builtin_types();
    let class = bt
        .by_name(class_name)
        .unwrap_or_else(|| bt.exception.clone());
    make_exception_with_class(class, message)
}

/// Build a built-in exception instance whose single `args[0]` element is the
/// *object* `arg`, not a stringified message — `KeyError(key)` where
/// `e.args[0] is key`. CPython's `KeyError.__str__` renders `repr(args[0])`,
/// which our `exc_str` already reproduces; we set `message` to that repr so
/// the Rust Display/traceback path matches too.
pub fn make_exception_with_object(class_name: &str, arg: Object) -> Object {
    let exc = make_exception(class_name, "");
    if let Object::Instance(inst) = &exc {
        inst.slot_set("args", Object::new_tuple(vec![arg.clone()]));
        inst.slot_set("message", Object::from_str(arg.repr()));
    }
    exc
}

/// Build a faithful `UnicodeEncodeError` instance carrying the 5-tuple
/// `(encoding, object, start, end, reason)` its custom `__init__`/`__str__`
/// expect (see [`install_unicode_error_dunders`]). The strict-mode codec
/// uses this so `str.encode()` of an unencodable character raises a real
/// `UnicodeEncodeError` (a `ValueError` subclass) — matching CPython —
/// rather than the bare `ValueError` we used to surface
/// (test_struct.test_Struct_reinitialization, test_exceptions unicode-error
/// cases).
pub fn make_unicode_encode_error(
    encoding: &str,
    object: &str,
    start: usize,
    end: usize,
    reason: &str,
) -> Object {
    make_unicode_encode_error_obj(encoding, Object::from_str(object), start, end, reason)
}

/// [`make_unicode_encode_error`] with the failing text as an `Object`, so a
/// surrogate-carrying [`Object::WStr`] survives into the exception's
/// `.object` attribute (a Rust `&str` cannot hold a lone surrogate) and the
/// rendered message names the real offending code point (`'\udac0'`, not the
/// `'\ufffd'` a lossy conversion would show).
pub fn make_unicode_encode_error_obj(
    encoding: &str,
    object: Object,
    start: usize,
    end: usize,
    reason: &str,
) -> Object {
    use crate::types::PyInstance;
    let bt = builtin_types();
    let class = bt
        .by_name("UnicodeEncodeError")
        .unwrap_or_else(|| bt.value_error.clone());
    let inst = PyInstance::new(class);
    let enc = Object::from_str(encoding);
    let obj = object;
    let start_o = Object::Int(start as i64);
    let end_o = Object::Int(end as i64);
    let reason_o = Object::from_str(reason);
    inst.slot_set(
        "args",
        Object::new_tuple(vec![
            enc.clone(),
            obj.clone(),
            start_o.clone(),
            end_o.clone(),
            reason_o.clone(),
        ]),
    );
    inst.slot_set("encoding", enc);
    inst.slot_set("object", obj);
    inst.slot_set("start", start_o);
    inst.slot_set("end", end_o);
    inst.slot_set("reason", reason_o);
    Object::Instance(Rc::new(inst))
}

/// `UnicodeDecodeError` instance with the canonical `(encoding, object,
/// start, end, reason)` payload — `object` is the *bytes* input, per
/// CPython (`PyUnicodeDecodeError_Create`).
pub fn make_unicode_decode_error(
    encoding: &str,
    object: &[u8],
    start: usize,
    end: usize,
    reason: &str,
) -> Object {
    use crate::types::PyInstance;
    let bt = builtin_types();
    let class = bt
        .by_name("UnicodeDecodeError")
        .unwrap_or_else(|| bt.value_error.clone());
    let inst = PyInstance::new(class);
    let enc = Object::from_str(encoding);
    let obj = Object::new_bytes(object.to_vec());
    let start_o = Object::Int(start as i64);
    let end_o = Object::Int(end as i64);
    let reason_o = Object::from_str(reason);
    inst.slot_set(
        "args",
        Object::new_tuple(vec![
            enc.clone(),
            obj.clone(),
            start_o.clone(),
            end_o.clone(),
            reason_o.clone(),
        ]),
    );
    inst.slot_set("encoding", enc);
    inst.slot_set("object", obj);
    inst.slot_set("start", start_o);
    inst.slot_set("end", end_o);
    inst.slot_set("reason", reason_o);
    Object::Instance(Rc::new(inst))
}

/// Extract the elements of a *concrete* iterable (one that doesn't need
/// the interpreter to drive). Used by `object.__new__` to seed the
/// native payload of an immutable-container subclass from a
/// `__getnewargs__`-supplied value. Returns `None` for anything that
/// would require VM iteration (generators, user iterators), which
/// `object.__new__` can't run.
fn concrete_elements(obj: &Object) -> Option<Vec<Object>> {
    match obj {
        Object::List(items) => Some(items.borrow().clone()),
        Object::Tuple(items) => Some(items.to_vec()),
        Object::Set(s) => Some(s.borrow().iter().map(|k| k.0.clone()).collect()),
        Object::FrozenSet(s) => Some(s.iter().map(|k| k.0.clone()).collect()),
        Object::Str(s) => Some(s.chars().map(|c| Object::from_str(c.to_string())).collect()),
        Object::Bytes(b) => Some(b.iter().map(|&x| Object::Int(i64::from(x))).collect()),
        Object::ByteArray(b) => Some(
            b.borrow()
                .iter()
                .map(|&x| Object::Int(i64::from(x)))
                .collect(),
        ),
        // A subclass instance wrapping a concrete native container.
        Object::Instance(inst) => inst.native.get().and_then(concrete_elements),
        _ => None,
    }
}

/// Drain any other iterable (map/filter/generator/range/…) through the
/// running interpreter — the general-protocol fallback for the seeding
/// conversions below (CPython's `PySequence_Tuple` reach).
fn elements_via_interp(obj: &Object) -> Option<Vec<Object>> {
    let ptr = crate::vm_singletons::current_interpreter_ptr()?;
    // SAFETY: published by an enclosing VM frame still live on this
    // thread; the GIL keeps the access exclusive.
    let interp = unsafe { &mut *ptr };
    let globals = interp.builtins_dict();
    interp.collect_iterable(obj, &globals).ok()
}

/// `concrete_elements` plus the interpreter-driven fallback.
fn any_elements(obj: &Object) -> Option<Vec<Object>> {
    concrete_elements(obj).or_else(|| elements_via_interp(obj))
}

/// Build the native payload `object.__new__(cls, value?)` should stash
/// on an instance of a value/container built-in subclass, or `None` for
/// an ordinary `object` subclass. Mutable containers (`list`/`dict`/
/// `set`/`bytearray`) start empty regardless of `value` — they're filled
/// afterwards by `__init__`/`__setstate__`/the copy reconstruction loop;
/// immutable ones (`int`/`float`/`complex`/`str`/`bytes`/`tuple`/
/// `frozenset`) capture `value` here because they can't be mutated later.
fn native_seed_for_new(cls: &Rc<TypeObject>, value: Option<&Object>) -> Option<Object> {
    if cls.flags.is_builtin {
        return None;
    }
    let bt = builtin_types();
    let is_strict = |base: &Rc<TypeObject>| cls.is_subclass_of(base) && !Rc::ptr_eq(cls, base);
    if is_strict(&bt.int_) {
        return Some(match value {
            None => Object::Int(0),
            Some(o @ (Object::Int(_) | Object::Long(_))) => o.clone(),
            Some(Object::Bool(b)) => Object::Int(i64::from(*b)),
            Some(o) => o
                .native_value()
                .unwrap_or_else(|| Object::Int(o.as_i64().unwrap_or(0))),
        });
    }
    if is_strict(&bt.float_) {
        let f = value.and_then(Object::as_f64).unwrap_or(0.0);
        return Some(Object::Float(f));
    }
    if is_strict(&bt.complex_) {
        return Some(match value {
            Some(c @ Object::Complex(_)) => c.clone(),
            // `complex.__new__(Sub, x)` coerces `x` to a complex (CPython
            // `complex_new`), so a `float`/`int` seed becomes `(x+0j)` and a
            // complex-subclass seed unwraps to its native complex — never a
            // raw non-complex payload (test_complexes).
            Some(o) => o
                .native_value()
                .filter(|n| matches!(n, Object::Complex(_)))
                .or_else(|| o.as_complex().map(|(r, i)| Object::new_complex(r, i)))
                .unwrap_or_else(|| Object::new_complex(0.0, 0.0)),
            None => Object::new_complex(0.0, 0.0),
        });
    }
    if is_strict(&bt.str_) {
        return Some(match value {
            Some(s @ Object::Str(_)) => s.clone(),
            _ => Object::from_static(""),
        });
    }
    if is_strict(&bt.bytearray_) {
        let bytes = value
            .and_then(any_elements)
            .map(|els| {
                els.iter()
                    .filter_map(|o| o.as_i64())
                    .map(|i| i as u8)
                    .collect()
            })
            .unwrap_or_default();
        return Some(Object::ByteArray(Rc::new(RefCell::new(bytes))));
    }
    if is_strict(&bt.bytes_) {
        let bytes: Vec<u8> = value
            .and_then(any_elements)
            .map(|els| {
                els.iter()
                    .filter_map(|o| o.as_i64())
                    .map(|i| i as u8)
                    .collect()
            })
            .unwrap_or_default();
        return Some(Object::Bytes(Rc::from(bytes.as_slice())));
    }
    if is_strict(&bt.tuple_) {
        let els = value.and_then(any_elements).unwrap_or_default();
        return Some(Object::new_tuple(els));
    }
    if is_strict(&bt.frozenset_) {
        let els = value.and_then(any_elements).unwrap_or_default();
        return Some(Object::new_frozenset_from(els));
    }
    if is_strict(&bt.list_) {
        return Some(Object::new_list(Vec::new()));
    }
    if is_strict(&bt.set_) {
        return Some(Object::new_set_from(Vec::<Object>::new()));
    }
    if is_strict(&bt.dict_) {
        return Some(Object::Dict(Rc::new(RefCell::new(DictData::default()))));
    }
    None
}

/// `object.__new__(cls, *args, **kwargs)` — the default allocator, shared by
/// `object.__new__` and the value-type `__new__`s (`int.__new__`, …) installed
/// by [`install_value_type_new`]. `args[0]` is `cls`; for a subclass of a
/// value/container built-in the native payload is captured so the inherited
/// protocols keep firing through the subclass.
pub(crate) fn object_new(args: &[Object]) -> Result<Object, RuntimeError> {
    use crate::types::PyInstance;
    let cls = match args.first() {
        Some(Object::Type(t)) => t.clone(),
        _ => {
            return Err(crate::error::type_error(
                "object.__new__(): first arg must be a class".to_owned(),
            ))
        }
    };
    // Exception classes: `BaseException.__new__` allocates and seeds
    // `.args` but never runs `__init__` (CPython `BaseException_new`).
    // `UnicodeDecodeError.__new__(UnicodeDecodeError)` must succeed
    // with zero constructor arguments.
    if cls.mro.borrow().iter().any(|t| t.name == "BaseException") {
        let new_args = if args.len() > 1 { &args[1..] } else { &[][..] };
        if let Some(ptr) = crate::vm_singletons::current_interpreter_ptr() {
            // SAFETY: published by an enclosing VM frame still live on
            // this thread; the GIL keeps the access exclusive.
            let interp = unsafe { &mut *ptr };
            return Ok(interp.build_exception_instance(cls, new_args));
        }
        let inst = Rc::new(PyInstance::new(cls));
        inst.dict.borrow_mut().insert(
            DictKey(Object::from_static("args")),
            Object::new_tuple(new_args.to_vec()),
        );
        let obj = Object::Instance(inst);
        crate::gc_trace::track(obj.clone());
        return Ok(obj);
    }
    // `tuple.__new__(tuple, it)` / `int.__new__(int, x)` … on the *built-in
    // class itself* must produce the native value, not a PyInstance shell
    // (CPython's per-type `tp_new`). Subclasses keep falling through to the
    // payload-seeding path below.
    // `module.__new__(module)` allocates an *uninitialized* module —
    // empty dict, no `__name__` — exactly CPython's `module_new` (the
    // name/doc seeding lives in `module.__init__` only).
    if cls.is_subclass_of(&builtin_types().module_) {
        let inst = Object::Instance(Rc::new(PyInstance::new(cls)));
        crate::gc_trace::track(inst.clone());
        return Ok(inst);
    }
    // A `type` subclass reaching the generic allocator is a wrong-arity
    // `type.__new__` call — the three-argument class-building form is
    // intercepted upstream (the 4-arg `__new__` route / `Meta(name,
    // bases, ns)`), so e.g. `type(typing.Any)()` must raise like
    // CPython's `type_new` ("takes exactly 3 arguments (0 given)").
    if cls.is_subclass_of(&builtin_types().type_) && args.len() != 4 {
        return Err(crate::error::type_error(format!(
            "type.__new__() takes exactly 3 arguments ({} given)",
            args.len() - 1
        )));
    }
    if cls.flags.is_builtin && !Rc::ptr_eq(&cls, &builtin_types().object_) {
        if let Some(ptr) = crate::vm_singletons::current_interpreter_ptr() {
            // SAFETY: published by an enclosing VM frame still live on this
            // thread; the GIL keeps the access exclusive.
            let interp = unsafe { &mut *ptr };
            return interp.type_call_default(&cls, &args[1..], &[]);
        }
    }
    // CPython: a subclass of `types.GenericAlias` (collections.abc's
    // `_CallableGenericAlias`, typing's private aliases, …) inherits
    // `GenericAlias.tp_new`, so a delegating `super().__new__(cls, origin,
    // args)` builds a parameterised alias rather than reaching the strict
    // `object.__new__`. WeavePy keys alias construction on the *built-in*
    // type's name in `type_call_default`, which a user subclass bypasses, so
    // honour the inherited constructor here: `(origin, params)` becomes a
    // duck-typed generic alias. (numpy's `_array_like` builds
    // `collections.abc.Callable[..., Any]` exactly this way during import.)
    {
        let bt = builtin_types();
        if args.len() == 3
            && !Rc::ptr_eq(&cls, &bt.generic_alias_)
            && cls.is_subclass_of(&bt.generic_alias_)
        {
            let alias = crate::make_generic_alias_public(args[1].clone(), args[2].clone());
            // CPython `ga_new` allocates through `cls`, so the instance
            // reports the subclass as its type (test_dataclasses
            // test_is_dataclass_genericalias). `class_of` honours the stamp.
            if let Object::SimpleNamespace(d) = &alias {
                d.borrow_mut().insert(
                    DictKey(Object::from_static("__class__")),
                    Object::Type(cls.clone()),
                );
            }
            return Ok(alias);
        }
    }
    // CPython `object_new` arity policy (bpo-31506): excess arguments
    // are an error unless exactly one of `__new__`/`__init__` is
    // overridden (the overriding side owns the signature).
    if args.len() > 1 && !cls.flags.is_builtin && native_seed_for_new(&cls, None).is_none() {
        if overrides_dunder_new(&cls) {
            return Err(crate::error::type_error(
                "object.__new__() takes exactly one argument (the type to instantiate)".to_owned(),
            ));
        }
        if !overrides_dunder_init(&cls) {
            return Err(crate::error::type_error(format!(
                "{}() takes no arguments",
                cls.name
            )));
        }
    }
    // `str.__new__(cls, value[, encoding[, errors]])` on a subclass
    // converts exactly like `str(value, …)` (CPython `unicode_new` calls
    // `unicode_new_impl` then re-wraps in the subclass) — a non-str seed
    // (`str.__new__(IntSeeded, 1)` from a mixed-in enum's
    // `_new_member_`) must yield `'1'`, and bad `encoding`/`errors`
    // arguments must raise str()'s own TypeError.
    {
        let bt = builtin_types();
        if cls.is_subclass_of(&bt.str_) && !Rc::ptr_eq(&cls, &bt.str_) && args.len() > 1 {
            let needs_convert = args.len() > 2 || !matches!(args[1], Object::Str(_));
            if needs_convert {
                if let Some(ptr) = crate::vm_singletons::current_interpreter_ptr() {
                    // SAFETY: published by an enclosing VM frame still live
                    // on this thread; the GIL keeps the access exclusive.
                    let interp = unsafe { &mut *ptr };
                    let s = interp.type_call_default(&bt.str_, &args[1..], &[])?;
                    let inst = Object::Instance(Rc::new(PyInstance::with_native(cls.clone(), s)));
                    crate::gc_trace::track(inst.clone());
                    return Ok(inst);
                }
            }
        }
    }
    // `int.__new__(cls, value[, base])` on a subclass converts exactly like
    // `int(value[, base])` (CPython `long_new` builds the int, then
    // `long_subtype_new` re-wraps it): pickletester's ComplexNewObj seeds
    // from `('FACE', 16)` via `__getnewargs__`, and a str/bytes/float seed
    // must coerce through the real constructor rather than default to 0.
    {
        let bt = builtin_types();
        if cls.is_subclass_of(&bt.int_) && !Rc::ptr_eq(&cls, &bt.int_) && args.len() > 1 {
            let needs_convert = args.len() > 2
                || matches!(
                    args[1],
                    Object::Str(_) | Object::Bytes(_) | Object::ByteArray(_) | Object::Float(_)
                );
            if needs_convert {
                if let Some(ptr) = crate::vm_singletons::current_interpreter_ptr() {
                    // SAFETY: published by an enclosing VM frame still live
                    // on this thread; the GIL keeps the access exclusive.
                    let interp = unsafe { &mut *ptr };
                    let v = interp.type_call_default(&bt.int_, &args[1..], &[])?;
                    let inst = Object::Instance(Rc::new(PyInstance::with_native(cls.clone(), v)));
                    crate::gc_trace::track(inst.clone());
                    return Ok(inst);
                }
            }
        }
    }
    // When `cls` derives from a value/container built-in (`int`, `float`,
    // `str`, `tuple`, `list`, `dict`, …) capture the native payload the
    // instance wraps so the inherited protocols keep firing through the
    // subclass. `super().__new__(cls, value)` passes the seed value as the
    // second positional argument (how `copyreg.__newobj__` reconstructs
    // immutable subclasses); mutable containers start empty and are filled by
    // `__init__` / `__setstate__` / the `_reconstruct` append-and-update loop.
    if let Some(native) = native_seed_for_new(&cls, args.get(1)) {
        let inst = Object::Instance(Rc::new(PyInstance::with_native(cls, native)));
        crate::gc_trace::track(inst.clone());
        return Ok(inst);
    }
    // RFC 0024: explicit `object.__new__(cls)` / `super().__new__(cls)`
    // allocations join the cycle collector exactly like instances born
    // through the default `instantiate` path — otherwise they're
    // invisible to `gc.collect()` and their weakrefs never clear.
    let inst = Object::Instance(Rc::new(PyInstance::new(cls)));
    crate::gc_trace::track(inst.clone());
    Ok(inst)
}

/// Does `cls` inherit `__new__` from somewhere other than `object`?
/// The value built-ins (`int`, `str`, …) install their own `__new__`
/// (CPython `int_new` etc.), which counts as an override for the
/// `object_new`/`object_init` arity policy even though WeavePy routes
/// it through the same default allocator.
pub(crate) fn overrides_dunder_new(cls: &Rc<TypeObject>) -> bool {
    for ty in cls.mro.borrow().iter() {
        if ty
            .dict
            .borrow()
            .contains_key(&DictKey(Object::from_static("__new__")))
        {
            return ty.name != "object";
        }
    }
    false
}

/// Does `cls` (or a non-`object` base) define a *user* `__init__`?
pub(crate) fn overrides_dunder_init(cls: &Rc<TypeObject>) -> bool {
    for ty in cls.mro.borrow().iter() {
        if let Some(init) = ty
            .dict
            .borrow()
            .get(&DictKey(Object::from_static("__init__")))
        {
            // Surface-only default-`__init__` mirrors (RFC 0056 WS4)
            // are not overrides.
            if crate::descr_registry::is_surface_only(init) {
                continue;
            }
            return ty.name != "object";
        }
    }
    false
}

/// A fresh `Object::Builtin("__new__")` wrapping [`object_new`]. Each
/// call returns a *distinct* object so `int.__new__ is object.__new__`
/// is `False` (matching CPython) while the instantiation path still treats it
/// as the default allocator (it keys on the builtin's `"__new__"` name).
/// Stored *raw*, not `StaticMethod`-wrapped: CPython's type dicts hold
/// the bare `PyCFunction` (`vars(object)['__new__']` is
/// `<built-in method __new__ …>`, and pydoc's allmethods diffs it
/// against `getattr(cls, '__new__')` by equality — test_pydoc
/// test_allmethods). `binds_instance: false` keeps instance reads
/// unbound, which is the staticmethod-like half CPython gets from
/// `PyCFunction` simply not being a descriptor.
fn make_default_new() -> Object {
    use crate::object::BuiltinFn;
    let obj = Object::Builtin(Rc::new(BuiltinFn {
        name: "__new__",
        binds_instance: false,
        call: Box::new(object_new),
        // CPython's `object.__new__(cls, *args, **kwargs)` ignores excess
        // arguments when `cls` overrides `__init__` but not `__new__`
        // (`tp_new_wrapper` → `excess_args` rules): `Future.__new__(Future,
        // loop=loop)` allocates uninitialized (RFC 0054 WS6,
        // test_futures.test_uninitialized). Without an overridden
        // `__init__` the excess is an error, as on CPython.
        call_kw: Some(Box::new(|args, kwargs| {
            if kwargs.is_empty() {
                return object_new(args);
            }
            let overrides_init = match args.first() {
                Some(Object::Type(cls)) => cls
                    .mro
                    .borrow()
                    .iter()
                    .take_while(|t| t.name != "object")
                    .any(|t| {
                        t.dict
                            .borrow()
                            .get(&DictKey(Object::from_static("__init__")))
                            .is_some_and(|init| !crate::descr_registry::is_surface_only(init))
                    }),
                _ => false,
            };
            if !overrides_init {
                return Err(crate::error::type_error(
                    "object.__new__() takes exactly one argument (the type to instantiate)",
                ));
            }
            object_new(&args[..args.len().min(1)])
        })),
    }));
    crate::descr_registry::mark_default_new(&obj);
    // CPython `object.__new__`'s clinic string — `inspect.signature(
    // C.__new__, follow_wrapped=False)` on a plain class parses it to
    // `(*args, **kwargs)` (test_inspect test_signature_on_class_with_
    // wrapped_init [descriptor]).
    crate::descr_registry::register_text_signature(&obj, "($type, *args, **kwargs)");
    obj
}

/// `module.__init__(self, name, doc=None)` — CPython's `module_init`.
/// `types.ModuleType("m")` (runpy, importlib, test doubles) reaches this;
/// it must accept the name/doc arguments rather than fall back to the
/// strict `object.__init__`.
/// Install the `__name__` / `__qualname__` getset descriptors on the
/// generator-family types (CPython's `gen_getsetlist` /
/// `coro_getsetlist` / `async_gen_getsetlist`). Tests read their
/// docstrings out of the type dict (`test_corotype_1`); reads on the
/// type itself still report the type's own name via the metaclass
/// precedence in `load_attr_type`.
fn install_gen_name_getsets(ty: &Rc<TypeObject>, kind: &'static str) {
    use crate::object::{BuiltinFn, PyProperty};
    fn gen_of(
        args: &[Object],
    ) -> Result<&crate::sync::Rc<crate::object::PyGenerator>, RuntimeError> {
        match args.first() {
            Some(Object::Generator(g) | Object::Coroutine(g) | Object::AsyncGenerator(g)) => Ok(g),
            _ => Err(crate::error::type_error(
                "descriptor requires a generator-family object",
            )),
        }
    }
    fn get_name(args: &[Object]) -> Result<Object, RuntimeError> {
        Ok(gen_of(args)?.name.borrow().clone())
    }
    fn get_qualname(args: &[Object]) -> Result<Object, RuntimeError> {
        Ok(gen_of(args)?.qualname.borrow().clone())
    }
    let docs = [
        (
            "__name__",
            get_name as fn(&[Object]) -> Result<Object, RuntimeError>,
            format!("name of the {kind}"),
        ),
        (
            "__qualname__",
            get_qualname,
            format!("qualified name of the {kind}"),
        ),
    ];
    for (attr, f, doc) in docs {
        ty.dict.borrow_mut().insert(
            DictKey(Object::from_static(attr)),
            Object::Property(Rc::new(PyProperty::new(
                Object::Builtin(Rc::new(BuiltinFn {
                    name: attr,
                    binds_instance: true,
                    call: Box::new(f),
                    call_kw: None,
                })),
                Object::None,
                Object::None,
                Object::from_str(doc),
            ))),
        );
    }
}

/// Explicit-protocol methods on `member_descriptor` (`__slots__` storage
/// descriptors): `A.x.__set__(obj, v)` / `.__get__(obj)` / `.__delete__(obj)`
/// — CPython's `member_get`/`member_set`/`member_delete`, including the
/// receiver type check that rejects virtual (ABC-registered) instances.
/// Expose the descriptor protocol on the `function` *type* dict so
/// `type(func).__get__` resolves (not just the per-instance
/// `func.__get__` fast path). CPython functions are non-data
/// descriptors; `inspect._descriptor_get` reaches for
/// `getattr(type(descriptor), '__get__')` to bind a class's `__init__`
/// to the class before reading its signature (which is how the leading
/// `self` is dropped from a constructor signature). Without the
/// type-level slot that lookup misses and every class signature keeps a
/// spurious `self`.
/// CPython's frame type exposes `f_locals` as a `tp_getset` entry —
/// `type(frame).f_locals` is a `getset_descriptor`
/// (`inspect.isgetsetdescriptor` in test_inspect
/// test_excluding_predicates). Instance reads are served by the native
/// `Object::Frame` attribute fast path; the descriptor's own getter
/// routes back through the current interpreter for parity.
fn install_frame_getsets(frame_: &Rc<TypeObject>) {
    use crate::object::{BuiltinFn, PyProperty};
    fn get_f_locals(args: &[Object]) -> Result<Object, RuntimeError> {
        if let Some(Object::Frame(f)) = args.first() {
            if let Some(ptr) = crate::vm_singletons::current_interpreter_ptr() {
                // SAFETY: builtin getters only run on the interpreter
                // thread that owns the pointer, under the GIL.
                let vm = unsafe { &mut *ptr };
                return vm.frame_locals_view(f.clone());
            }
        }
        Err(crate::error::type_error(
            "descriptor 'f_locals' for 'frame' objects doesn't apply",
        ))
    }
    let prop = Object::Property(Rc::new(PyProperty::new(
        Object::Builtin(Rc::new(BuiltinFn {
            name: "f_locals",
            binds_instance: true,
            call: Box::new(get_f_locals),
            call_kw: None,
        })),
        Object::None,
        Object::None,
        Object::None,
    )));
    crate::descr_registry::register(
        &prop,
        crate::descr_registry::DescrKind::GetSet,
        frame_.clone(),
        "f_locals",
        None,
    );
    frame_
        .dict
        .borrow_mut()
        .insert(DictKey(Object::from_static("f_locals")), prop);
}

fn install_function_methods(function_: &Rc<TypeObject>) {
    let get = crate::builtins::function_get_builtin();
    crate::descr_registry::register(
        &get,
        crate::descr_registry::DescrKind::Method,
        function_.clone(),
        "__get__",
        None,
    );
    function_
        .dict
        .borrow_mut()
        .insert(DictKey(Object::from_static("__get__")), get);
    // CPython's function type also exposes its attributes as *type-dict
    // descriptors*: `__code__` is a `tp_getset` entry (getset_descriptor)
    // and `__globals__` a `tp_members` one (member_descriptor). Class-level
    // access must resolve — `Lib/types.py` literally derives the
    // descriptor types from them (`GetSetDescriptorType =
    // type(FunctionType.__code__)`), and `importlib.reload(types)`
    // re-executes that file verbatim (test_api's ReloadTests). Instance
    // reads never reach these: the native `Object::Function` attribute
    // fast path serves `f.__code__` first.
    fn get_code(args: &[Object]) -> Result<Object, RuntimeError> {
        match args.first() {
            Some(Object::Function(f)) => Ok(Object::Code(f.code())),
            _ => Err(crate::error::type_error(
                "descriptor '__code__' for 'function' objects doesn't apply",
            )),
        }
    }
    fn get_globals(args: &[Object]) -> Result<Object, RuntimeError> {
        match args.first() {
            Some(Object::Function(f)) => Ok(Object::Dict(f.globals.clone())),
            _ => Err(crate::error::type_error(
                "descriptor '__globals__' for 'function' objects doesn't apply",
            )),
        }
    }
    use crate::object::{BuiltinFn, PyProperty};
    for (attr, f, kind) in [
        (
            "__code__",
            get_code as fn(&[Object]) -> Result<Object, RuntimeError>,
            crate::descr_registry::DescrKind::GetSet,
        ),
        (
            "__globals__",
            get_globals,
            crate::descr_registry::DescrKind::Member,
        ),
    ] {
        let prop = Object::Property(Rc::new(PyProperty::new(
            Object::Builtin(Rc::new(BuiltinFn {
                name: attr,
                binds_instance: true,
                call: Box::new(f),
                call_kw: None,
            })),
            Object::None,
            Object::None,
            Object::None,
        )));
        crate::descr_registry::register(&prop, kind, function_.clone(), attr, None);
        function_
            .dict
            .borrow_mut()
            .insert(DictKey(Object::from_static(attr)), prop);
    }
}

/// Expose `__get__` on a native-callable *type* dict (see
/// `install_function_methods`; this is the `builtin_function_or_method`
/// / descriptor analogue, backed by `builtin_descriptor_get`).
fn install_builtin_descriptor_get(ty: &Rc<TypeObject>) {
    use crate::object::BuiltinFn;
    let get = Object::Builtin(Rc::new(BuiltinFn {
        name: "__get__",
        binds_instance: true,
        call: Box::new(crate::builtins::builtin_descriptor_get),
        call_kw: None,
    }));
    crate::descr_registry::register(
        &get,
        crate::descr_registry::DescrKind::Method,
        ty.clone(),
        "__get__",
        None,
    );
    ty.dict
        .borrow_mut()
        .insert(DictKey(Object::from_static("__get__")), get);
}

/// `classmethod_descriptor.__get__(obj, owner)` — CPython's
/// `classmethod_get`: binds the wrapped C function to the *owner class*
/// (or `type(obj)` when only an instance is given), never to the
/// instance itself (`dict.__dict__['fromkeys'].__get__(None, dict)` is
/// the bound `dict.fromkeys`).
fn install_classmethod_descriptor_get(ty: &Rc<TypeObject>) {
    use crate::object::BuiltinFn;
    fn cmd_get(args: &[Object]) -> Result<Object, RuntimeError> {
        let descr = args
            .first()
            .cloned()
            .ok_or_else(|| crate::error::type_error("__get__() missing descriptor"))?;
        let inner = match &descr {
            Object::ClassMethod(w) => w.func(),
            other => other.clone(),
        };
        let obj = args.get(1).cloned().unwrap_or(Object::None);
        let owner = args.get(2).cloned().unwrap_or(Object::None);
        let target = match (&obj, &owner) {
            (Object::None, Object::None) => {
                return Err(crate::error::type_error("__get__(None, None) is invalid"))
            }
            (_, o @ Object::Type(_)) => o.clone(),
            _ => Object::Type(crate::builtins::class_of(&obj)),
        };
        Ok(Object::BoundMethod(Rc::new(
            crate::object::BoundMethod::new(target, inner),
        )))
    }
    let get = Object::Builtin(Rc::new(BuiltinFn {
        name: "__get__",
        binds_instance: true,
        call: Box::new(cmd_get),
        call_kw: None,
    }));
    crate::descr_registry::register(
        &get,
        crate::descr_registry::DescrKind::Method,
        ty.clone(),
        "__get__",
        None,
    );
    ty.dict
        .borrow_mut()
        .insert(DictKey(Object::from_static("__get__")), get);
}

fn install_member_descriptor_methods(member_: &Rc<TypeObject>) {
    use crate::object::BuiltinFn;
    fn slot_and_receiver<'a>(
        args: &'a [Object],
        op: &str,
    ) -> Result<(&'a Rc<crate::object::SlotDescriptor>, &'a Object), RuntimeError> {
        let slot = match args.first() {
            Some(Object::SlotDescriptor(s)) => s,
            _ => {
                return Err(crate::error::type_error(format!(
                    "descriptor '{op}' requires a 'member_descriptor' object"
                )))
            }
        };
        let obj = args.get(1).ok_or_else(|| {
            crate::error::type_error(format!(
                "descriptor '{}' of object needs an argument",
                slot.name
            ))
        })?;
        Ok((slot, obj))
    }
    /// CPython `descr_check`: the receiver must be a *real* instance of
    /// the declaring class (virtual/ABC registration doesn't count).
    fn check_receiver(
        slot: &crate::object::SlotDescriptor,
        obj: &Object,
    ) -> Result<crate::sync::Rc<crate::types::PyInstance>, RuntimeError> {
        if let Object::Instance(inst) = obj {
            let owns =
                inst.cls().mro.borrow().iter().any(|t| {
                    t.name == slot.class_name && t.slot_names.borrow().contains(&slot.name)
                });
            if owns {
                return Ok(inst.clone());
            }
        }
        Err(crate::error::type_error(format!(
            "descriptor '{}' for '{}' objects doesn't apply to a '{}' object",
            slot.name,
            slot.class_name,
            obj.type_name()
        )))
    }
    fn member_get(args: &[Object]) -> Result<Object, RuntimeError> {
        let (slot, obj) = slot_and_receiver(args, "__get__")?;
        if matches!(obj, Object::None) {
            return Ok(args[0].clone());
        }
        let inst = check_receiver(slot, obj)?;
        inst.slot_get(&slot.name).ok_or_else(|| {
            crate::error::attribute_error(format!(
                "'{}' object has no attribute '{}'",
                inst.cls().qualified_display_name(),
                slot.name
            ))
        })
    }
    fn member_set(args: &[Object]) -> Result<Object, RuntimeError> {
        let (slot, obj) = slot_and_receiver(args, "__set__")?;
        let inst = check_receiver(slot, obj)?;
        let value = args
            .get(2)
            .cloned()
            .ok_or_else(|| crate::error::type_error("__set__ expected 2 arguments"))?;
        inst.slot_set(&slot.name, value);
        Ok(Object::None)
    }
    fn member_delete(args: &[Object]) -> Result<Object, RuntimeError> {
        let (slot, obj) = slot_and_receiver(args, "__delete__")?;
        let inst = check_receiver(slot, obj)?;
        if !inst.slot_del(&slot.name) {
            return Err(crate::error::attribute_error(format!(
                "'{}' object has no attribute '{}'",
                inst.cls().name,
                slot.name
            )));
        }
        Ok(Object::None)
    }
    let mut td = member_.dict.borrow_mut();
    for (name, f) in [
        (
            "__get__",
            member_get as fn(&[Object]) -> Result<Object, RuntimeError>,
        ),
        ("__set__", member_set),
        ("__delete__", member_delete),
    ] {
        td.insert(
            DictKey(Object::from_static(name)),
            Object::Builtin(Rc::new(BuiltinFn {
                name,
                binds_instance: true,
                call: Box::new(f),
                call_kw: None,
            })),
        );
    }
}

/// Install `__init__` on `staticmethod`/`classmethod` (CPython's
/// `sm_init`/`cm_init`): it sets `__func__`, which `__new__` left as
/// `None`. Keeping the assignment in `__init__` is what makes a
/// subclass that overrides `__init__` without chaining observe
/// `__func__ is None`.
fn install_descriptor_init(ty: &Rc<TypeObject>, is_static: bool) {
    use crate::object::BuiltinFn;
    let call: fn(&[Object]) -> Result<Object, RuntimeError> = if is_static {
        crate::builtins::staticmethod_init
    } else {
        crate::builtins::classmethod_init
    };
    ty.dict.borrow_mut().insert(
        DictKey(Object::from_static("__init__")),
        Object::Builtin(Rc::new(BuiltinFn {
            name: "__init__",
            binds_instance: true,
            call: Box::new(call),
            call_kw: None,
        })),
    );
}

/// Install `super`'s own methods: `__init__` (so `class mysuper(super)`
/// can chain `super().__init__(type, obj)`), `__get__` (rebind an unbound
/// `super(C)` to `super(C, obj)`), and `__repr__`. The proxy's MRO walk
/// itself lives in `load_attr_instance_default`.
fn install_super_methods(super_: &Rc<TypeObject>) {
    use crate::object::BuiltinFn;
    fn super_repr(args: &[Object]) -> Result<Object, RuntimeError> {
        let Some(Object::Instance(i)) = args.first() else {
            return Err(crate::error::type_error("super.__repr__ requires a super"));
        };
        let d = i.dict.borrow();
        let this = match d.get(&DictKey(Object::from_static("__thisclass__"))) {
            Some(Object::Type(t)) => t.name.clone(),
            _ => "?".to_owned(),
        };
        let obj_type = d.get(&DictKey(Object::from_static("__self_class__")));
        let s = match obj_type {
            Some(Object::Type(t)) => format!("<super: <class '{}'>, <{} object>>", this, t.name),
            _ => format!("<super: <class '{this}'>, NULL>"),
        };
        Ok(Object::from_str(&s))
    }
    let mut td = super_.dict.borrow_mut();
    td.insert(
        DictKey(Object::from_static("__init__")),
        Object::Builtin(Rc::new(BuiltinFn {
            name: "__init__",
            binds_instance: true,
            call: Box::new(crate::builtins::super_init_impl),
            call_kw: None,
        })),
    );
    td.insert(
        DictKey(Object::from_static("__get__")),
        Object::Builtin(Rc::new(BuiltinFn {
            name: "__get__",
            binds_instance: true,
            call: Box::new(crate::builtins::super_descr_get_impl),
            call_kw: None,
        })),
    );
    td.insert(
        DictKey(Object::from_static("__repr__")),
        Object::Builtin(Rc::new(BuiltinFn {
            name: "__repr__",
            binds_instance: true,
            call: Box::new(super_repr),
            call_kw: None,
        })),
    );
}

fn install_module_init(module_: &Rc<TypeObject>) {
    use crate::object::BuiltinFn;
    fn module_init(args: &[Object]) -> Result<Object, RuntimeError> {
        let inst = match args.first() {
            Some(Object::Instance(i)) => i.clone(),
            _ => {
                return Err(crate::error::type_error(
                    "module.__init__() requires a module instance".to_owned(),
                ))
            }
        };
        if args.len() > 3 {
            return Err(crate::error::type_error(format!(
                "module.__init__() takes at most 2 arguments ({} given)",
                args.len() - 1
            )));
        }
        let name = match args.get(1) {
            Some(Object::Str(s)) => Object::Str(s.clone()),
            Some(_) => {
                return Err(crate::error::type_error(
                    "module.__init__() argument 1 must be str".to_owned(),
                ))
            }
            None => {
                return Err(crate::error::type_error(
                    "module.__init__() missing required argument: 'name' (pos 1)".to_owned(),
                ))
            }
        };
        let doc = args.get(2).cloned().unwrap_or(Object::None);
        let mut dict = inst.dict.borrow_mut();
        dict.insert(DictKey(Object::from_static("__name__")), name);
        dict.insert(DictKey(Object::from_static("__doc__")), doc);
        dict.insert(DictKey(Object::from_static("__package__")), Object::None);
        dict.insert(DictKey(Object::from_static("__loader__")), Object::None);
        dict.insert(DictKey(Object::from_static("__spec__")), Object::None);
        Ok(Object::None)
    }
    module_.dict.borrow_mut().insert(
        DictKey(Object::from_static("__init__")),
        Object::Builtin(Rc::new(BuiltinFn {
            name: "__init__",
            binds_instance: true,
            call: Box::new(module_init),
            call_kw: None,
        })),
    );
}

/// CPython `moduleobject.c` surface on the module *type*. Imported
/// modules are `Object::Module` and take the native fast paths in
/// `lib.rs` (`load_attr`'s Module arm, the `Object::Module` repr arm);
/// modules built *from Python* — `types.ModuleType('foo')` and module
/// subclasses — are plain `Object::Instance`s of this class and reach
/// the same behavior through the generic protocol instead
/// (test_module's repr/getattr/annotations matrix).
fn install_module_methods(module_: &Rc<TypeObject>) {
    use crate::object::{BuiltinFn, PyProperty};

    /// The namespace dict of either module representation.
    fn dict_of(o: &Object) -> Result<Rc<RefCell<DictData>>, RuntimeError> {
        match o {
            Object::Instance(i) => Ok(i.dict.clone()),
            Object::Module(m) => Ok(m.dict.clone()),
            _ => Err(crate::error::type_error(
                "descriptor requires a 'module' object".to_owned(),
            )),
        }
    }

    // `module.__repr__` — CPython's `module_repr` delegates wholesale to
    // `importlib._bootstrap._module_repr`; so do we. Without a running
    // interpreter, fall back to the anonymous shape CPython shows before
    // importlib is initialized.
    fn module_repr(args: &[Object]) -> Result<Object, RuntimeError> {
        let this = args.first().ok_or_else(|| {
            crate::error::type_error("__repr__ requires a module object".to_owned())
        })?;
        if let Some(ptr) = crate::vm_singletons::current_interpreter_ptr() {
            // SAFETY: published by an enclosing VM frame still live on
            // this thread; the GIL keeps access exclusive.
            let interp = unsafe { &mut *ptr };
            let repr = (|| {
                // The frozen name: `importlib._bootstrap` is only a
                // sys.modules alias minted by `importlib/__init__` and may
                // not exist yet.
                let m = interp.import_path_internal("_frozen_importlib")?;
                let f = interp.load_attr_public(&m, "_module_repr")?;
                interp.call_object(f, &[this.clone()], &[])
            })();
            if let Ok(r) = repr {
                return Ok(r);
            }
        }
        Ok(Object::from_str(format!(
            "<module object at 0x{:x}>",
            crate::builtins::object_identity(this)
        )))
    }

    // `module.__getattr__` — the miss half of CPython's
    // `module_getattro`: PEP 562 dict-level `__getattr__` dispatch, then
    // the exact error wording ("module 'foo' has no attribute 'x'";
    // nameless uninitialized modules drop the quoted name —
    // test_module.test_uninitialized_missing_getattr).
    fn module_getattr_miss(args: &[Object]) -> Result<Object, RuntimeError> {
        let (this, name) = match args {
            [this, Object::Str(s)] => (this, s.to_string()),
            _ => {
                return Err(crate::error::type_error(
                    "module.__getattr__ requires (module, name)".to_owned(),
                ))
            }
        };
        let dict = dict_of(this)?;
        if name != "__getattr__" {
            let hook = dict
                .borrow()
                .get(&crate::object::StrKey("__getattr__"))
                .cloned();
            if let Some(hook) = hook {
                if let Some(ptr) = crate::vm_singletons::current_interpreter_ptr() {
                    // SAFETY: as in `module_repr` above.
                    let interp = unsafe { &mut *ptr };
                    return interp.call_object(hook, &[Object::from_str(&name)], &[]);
                }
            }
        }
        let mod_name = dict
            .borrow()
            .get(&crate::object::StrKey("__name__"))
            .cloned();
        Err(match mod_name {
            Some(Object::Str(s)) => {
                crate::error::attribute_error(format!("module '{}' has no attribute '{}'", s, name))
            }
            _ => crate::error::attribute_error(format!("module has no attribute '{}'", name)),
        })
    }

    // `module.__annotations__` — CPython `module_get_annotations`:
    // reading through the descriptor lazily creates-and-caches an empty
    // dict (test_module.test_lazy_create_annotations); set/delete write
    // through to the namespace dict.
    fn module_annotations_get(args: &[Object]) -> Result<Object, RuntimeError> {
        let this = args.first().ok_or_else(|| {
            crate::error::type_error("descriptor requires a 'module' object".to_owned())
        })?;
        let dict = dict_of(this)?;
        if let Some(v) = dict.borrow().get(&crate::object::StrKey("__annotations__")) {
            return Ok(v.clone());
        }
        let fresh = Object::new_dict();
        dict.borrow_mut().insert(
            DictKey(Object::from_static("__annotations__")),
            fresh.clone(),
        );
        Ok(fresh)
    }
    fn module_annotations_set(args: &[Object]) -> Result<Object, RuntimeError> {
        let (this, value) = match args {
            [this, value] => (this, value.clone()),
            _ => {
                return Err(crate::error::type_error(
                    "__annotations__ setter requires (module, value)".to_owned(),
                ))
            }
        };
        dict_of(this)?
            .borrow_mut()
            .insert(DictKey(Object::from_static("__annotations__")), value);
        Ok(Object::None)
    }
    fn module_annotations_del(args: &[Object]) -> Result<Object, RuntimeError> {
        let this = args.first().ok_or_else(|| {
            crate::error::type_error("descriptor requires a 'module' object".to_owned())
        })?;
        let removed = dict_of(this)?
            .borrow_mut()
            .shift_remove(&DictKey(Object::from_static("__annotations__")));
        if removed.is_none() {
            return Err(crate::error::attribute_error("__annotations__".to_owned()));
        }
        Ok(Object::None)
    }

    fn builtin(name: &'static str, f: fn(&[Object]) -> Result<Object, RuntimeError>) -> Object {
        Object::Builtin(Rc::new(BuiltinFn {
            name,
            binds_instance: true,
            call: Box::new(f),
            call_kw: None,
        }))
    }

    let mut d = module_.dict.borrow_mut();
    // CPython's `module_doc` — `test_module.test_uninitialized` reads it
    // through an uninitialized instance (empty namespace dict, so the
    // lookup falls back to the type).
    d.insert(
        DictKey(Object::from_static("__doc__")),
        Object::from_static(
            "Create a module object.\n\nThe name must be a string; \
             the optional doc argument can have any type.",
        ),
    );
    d.insert(
        DictKey(Object::from_static("__repr__")),
        builtin("__repr__", module_repr),
    );
    d.insert(
        DictKey(Object::from_static("__getattr__")),
        builtin("__getattr__", module_getattr_miss),
    );
    d.insert(
        DictKey(Object::from_static("__annotations__")),
        Object::Property(Rc::new(PyProperty::new(
            builtin("__annotations__", module_annotations_get),
            builtin("__annotations__", module_annotations_set),
            builtin("__annotations__", module_annotations_del),
            Object::None,
        ))),
    );
}

/// Install `object.__new__`, `object.__init__`, `object.__setattr__`
/// and `object.__delattr__` on the root class. These are the implicit
/// base methods every user class inherits.
fn install_object_dunders(object_: &Rc<TypeObject>) {
    use crate::object::BuiltinFn;
    fn object_init(args: &[Object]) -> Result<Object, RuntimeError> {
        // Unbound use (`object.__init__()`) still needs the instance —
        // CPython's method descriptor rejects the empty call.
        if args.is_empty() {
            return Err(crate::error::type_error(
                "descriptor '__init__' of 'object' object needs an argument".to_owned(),
            ));
        }
        // CPython `object_init` arity policy (bpo-31506): excess
        // arguments are an error unless `__new__` is overridden while
        // `__init__` is not (then `__new__` owns the signature and the
        // default `__init__` stays lenient).
        if args.len() > 1 {
            if let Some(Object::Instance(inst)) = args.first() {
                let cls = &inst.cls();
                // A native payload means a built-in base's constructor
                // (`int_new`, `property_init`, …) owns the signature —
                // CPython's tp_new/tp_init for those types aren't
                // `object_new`/`object_init`, so the strict arity
                // policy doesn't apply.
                if inst.native.get().is_none() {
                    if overrides_dunder_init(cls) {
                        // An overriding `__init__` delegated here
                        // (`super().__init__(*args)`) — blame object.__init__.
                        return Err(crate::error::type_error(
                            "object.__init__() takes exactly one argument (the instance to initialize)"
                                .to_owned(),
                        ));
                    }
                    if !overrides_dunder_new(cls) {
                        return Err(crate::error::type_error(format!(
                            "{}.__init__() takes exactly one argument (the instance to initialize)",
                            cls.name
                        )));
                    }
                }
            }
        }
        // No-op; honours `super().__init__()` chains.
        Ok(Object::None)
    }
    fn object_setattr(args: &[Object]) -> Result<Object, RuntimeError> {
        // `object.__setattr__(self, name, value)` — CPython's
        // `PyObject_GenericSetAttr`: descriptors, `__slots__` and
        // `__class__` handling, but *no* user-`__setattr__` dispatch
        // (this is the default that overrides chain up to).
        if args.len() != 3 {
            return Err(crate::error::type_error(
                "object.__setattr__() takes 3 arguments".to_owned(),
            ));
        }
        let name = match &args[1] {
            Object::Str(s) => s.to_string(),
            _ => return Err(crate::error::type_error("attribute name must be str")),
        };
        match &args[0] {
            Object::Instance(inst) => {
                if let Some(ptr) = crate::vm_singletons::current_interpreter_ptr() {
                    // SAFETY: published by an enclosing VM frame still
                    // live on this thread; the GIL keeps access exclusive.
                    let interp = unsafe { &mut *ptr };
                    interp.generic_setattr_instance(inst, &args[0], &name, args[2].clone())?;
                } else if matches!(inst.cls().lookup(&name), Some(Object::SlotDescriptor(_))) {
                    // No VM frame on this thread (e.g. a bare C-API embed):
                    // we can't run a Python property setter, but a
                    // `__slots__` member is pure-native and must still land
                    // in the slot side table — never the instance `__dict__`,
                    // where the slot descriptor would not find it.
                    inst.slot_set(&name, args[2].clone());
                } else {
                    inst.dict
                        .borrow_mut()
                        .insert(DictKey(Object::from_str(name)), args[2].clone());
                }
                Ok(Object::None)
            }
            // CPython's "Carlo Verre hack" guard (`hackcheck`): applying the
            // base `object.__setattr__` to a *type* would bypass the type's
            // own `type_setattro`. Metaclass overrides reach the default via
            // `super().__setattr__(…)`, which resolves to `type.__setattr__`
            // (not here), so any type arriving at `object.__setattr__` is an
            // illegal bypass (test_carloverre_multi_inherit_invalid). The
            // message names the metatype, as CPython does.
            Object::Type(_) => Err(crate::error::type_error(format!(
                "can't apply this __setattr__ to {} object",
                crate::builtins::class_of(&args[0]).name
            ))),
            other => Err(crate::error::type_error(format!(
                "object.__setattr__() requires an instance, got '{}'",
                other.type_name()
            ))),
        }
    }
    fn object_delattr(args: &[Object]) -> Result<Object, RuntimeError> {
        if args.len() != 2 {
            return Err(crate::error::type_error(
                "object.__delattr__() takes 2 arguments".to_owned(),
            ));
        }
        let name = match &args[1] {
            Object::Str(s) => s.to_string(),
            _ => return Err(crate::error::type_error("attribute name must be str")),
        };
        match &args[0] {
            Object::Instance(inst) => {
                if let Some(ptr) = crate::vm_singletons::current_interpreter_ptr() {
                    // SAFETY: published by an enclosing VM frame still
                    // live on this thread; the GIL keeps access exclusive.
                    let interp = unsafe { &mut *ptr };
                    interp.generic_delattr_instance(inst, &args[0], &name)?;
                    return Ok(Object::None);
                }
                // No VM frame: a `__slots__` member lives in the side table,
                // so try it before the instance `__dict__` (mirrors the
                // slot-aware `object.__setattr__` fallback above).
                let removed = inst.slot_del(&name)
                    || inst
                        .dict
                        .borrow_mut()
                        .shift_remove(&DictKey(Object::from_str(&name)))
                        .is_some();
                if !removed {
                    return Err(crate::error::attribute_error(format!(
                        "'{}' object has no attribute '{}'",
                        inst.cls().name,
                        name
                    )));
                }
                Ok(Object::None)
            }
            // Carlo Verre hack guard (see `object_setattr`): the base
            // `object.__delattr__` can't be applied to a type — that bypasses
            // `type.__delattr__`. Metaclass overrides chain through
            // `super().__delattr__(…)` (→ `type.__delattr__`) instead.
            Object::Type(_) => Err(crate::error::type_error(format!(
                "can't apply this __delattr__ to {} object",
                crate::builtins::class_of(&args[0]).name
            ))),
            other => Err(crate::error::type_error(format!(
                "object.__delattr__() requires an instance, got '{}'",
                other.type_name()
            ))),
        }
    }
    fn object_hash(args: &[Object]) -> Result<Object, RuntimeError> {
        // CPython's `object.__hash__` is a wrapper around the *default*
        // identity-hash slot: calling it explicitly never re-dispatches a
        // subclass override and never unwraps a value subclass. mock relies
        // on this — `MagicMock`'s `__hash__` return value is computed as
        // `object.__hash__(self)`, and re-dispatching would invoke the
        // `MagicProxy` child mock and record a phantom `call()`
        // (testmagicmethods.test_magic_mock_does_not_reset_magic_returns).
        // `hash(x)` (which does honour overrides) stays on
        // `builtins::hash_object`.
        let obj = args.first().ok_or_else(|| {
            crate::error::type_error("object.__hash__() takes exactly 1 argument")
        })?;
        Ok(Object::Int(crate::object::identity_hash(obj)))
    }
    let mut dict = object_.dict.borrow_mut();
    dict.insert(
        DictKey(Object::from_static("__hash__")),
        Object::Builtin(Rc::new(BuiltinFn {
            name: "__hash__",
            binds_instance: true,
            call: Box::new(object_hash),
            call_kw: None,
        })),
    );
    let object_new_obj = make_default_new();
    register_new_metadata(&object_new_obj, object_);
    dict.insert(DictKey(Object::from_static("__new__")), object_new_obj);
    dict.insert(
        DictKey(Object::from_static("__init__")),
        Object::Builtin(Rc::new(BuiltinFn {
            name: "__init__",
            binds_instance: true,
            call: Box::new(object_init),
            call_kw: None,
        })),
    );
    dict.insert(
        DictKey(Object::from_static("__setattr__")),
        Object::Builtin(Rc::new(BuiltinFn {
            name: "__setattr__",
            binds_instance: true,
            call: Box::new(object_setattr),
            call_kw: None,
        })),
    );
    dict.insert(
        DictKey(Object::from_static("__delattr__")),
        Object::Builtin(Rc::new(BuiltinFn {
            name: "__delattr__",
            binds_instance: true,
            call: Box::new(object_delattr),
            call_kw: None,
        })),
    );
    // `object.__class__` is a getset *in the type dict* (CPython
    // `object_getsets`), not just an attribute-path special case.
    // Static introspection reads it there: `inspect.getattr_static`,
    // `classify_class_attrs` (which must report `__class__` as defined
    // by `object`, not by whichever class it walks first — test_enum's
    // TestStdLib), and pydoc's descriptor sweep. The attribute fast
    // paths in `load_attr`/`setattr` still short-circuit `__class__`
    // before descriptor dispatch, so these functions only run when the
    // descriptor is invoked explicitly.
    fn object_class_get(args: &[Object]) -> Result<Object, RuntimeError> {
        let obj = args.first().ok_or_else(|| {
            crate::error::type_error("descriptor '__class__' of 'object' needs an argument")
        })?;
        // A weakproxy lies about its class: CPython's proxy `tp_getattro`
        // forwards the whole read to the referent (and a dead proxy
        // raises ReferenceError). The proxy is an `Object::Instance`, so
        // this data descriptor is reached through the normal MRO walk
        // and must forward too (test_itertools.test_tee).
        if let Some(target) = crate::stdlib::weakref_real::proxy_referent(obj) {
            return Ok(Object::Type(crate::builtins::class_of(&target?)));
        }
        Ok(Object::Type(crate::builtins::class_of(obj)))
    }
    fn object_class_set(args: &[Object]) -> Result<Object, RuntimeError> {
        let (Some(obj), Some(value)) = (args.first(), args.get(1)) else {
            return Err(crate::error::type_error(
                "descriptor '__class__' requires (instance, value)",
            ));
        };
        // Route through the interpreter's setattr, which owns the
        // layout-compatibility rules for `__class__` assignment.
        if let Some(ptr) = crate::vm_singletons::current_interpreter_ptr() {
            // SAFETY: published by an enclosing VM frame still live on
            // this thread; the GIL keeps access exclusive.
            let interp = unsafe { &mut *ptr };
            interp.store_attr_public(obj, "__class__", value.clone())?;
            return Ok(Object::None);
        }
        Err(crate::error::type_error(
            "__class__ assignment only supported inside a running interpreter",
        ))
    }
    {
        let getset = Object::Property(Rc::new(crate::object::PyProperty::new(
            Object::Builtin(Rc::new(BuiltinFn {
                name: "__class__",
                binds_instance: true,
                call: Box::new(object_class_get),
                call_kw: None,
            })),
            Object::Builtin(Rc::new(BuiltinFn {
                name: "__class__",
                binds_instance: true,
                call: Box::new(object_class_set),
                call_kw: None,
            })),
            Object::Builtin(Rc::new(BuiltinFn {
                name: "__class__",
                binds_instance: true,
                call: Box::new(object_class_set),
                call_kw: None,
            })),
            Object::None,
        )));
        crate::descr_registry::register(
            &getset,
            crate::descr_registry::DescrKind::GetSet,
            object_.clone(),
            "__class__",
            None,
        );
        dict.insert(DictKey(Object::from_static("__class__")), getset);
    }
    // `object.__init_subclass__(cls)` and `object.__subclasshook__`
    // are no-ops by default; defining them here lets every subclass
    // chain through `super().__init_subclass__()` without raising.
    fn object_no_op(_args: &[Object]) -> Result<Object, RuntimeError> {
        Ok(Object::None)
    }
    dict.insert(
        DictKey(Object::from_static("__init_subclass__")),
        Object::ClassMethod(MethodWrapper::new(Object::Builtin(Rc::new(BuiltinFn {
            name: "__init_subclass__",
            binds_instance: true,
            call: Box::new(object_no_op),
            call_kw: None,
        })))),
    );
    // `object.__subclasshook__(cls, subclass)` returns `NotImplemented`
    // by default (CPython), telling `issubclass`/ABCMeta to fall back to
    // the normal MRO/registry check. ABCs override it to implement
    // structural ("duck typing") subclass tests.
    fn object_subclasshook(_args: &[Object]) -> Result<Object, RuntimeError> {
        Ok(crate::vm_singletons::not_implemented())
    }
    dict.insert(
        DictKey(Object::from_static("__subclasshook__")),
        Object::ClassMethod(MethodWrapper::new(Object::Builtin(Rc::new(BuiltinFn {
            name: "__subclasshook__",
            binds_instance: true,
            call: Box::new(object_subclasshook),
            call_kw: None,
        })))),
    );
    // `object.__reduce_ex__(self, protocol)` / `object.__reduce__(self)`
    // need interpreter access (to import `copyreg` and call the receiver's
    // `__getstate__`/`__getnewargs__` hooks), so they are registered under
    // sentinel names that `Interpreter::call` intercepts (see the
    // `.object_reduce_ex` / `.object_reduce` arms there). Plain
    // `BuiltinFn::call` is a `fn(&[Object])` and can't reach the VM.
    fn object_reduce_ex_sentinel(_args: &[Object]) -> Result<Object, RuntimeError> {
        Err(crate::error::runtime_error(
            "object.__reduce_ex__ must be dispatched via Interpreter::call",
        ))
    }
    fn object_reduce_sentinel(_args: &[Object]) -> Result<Object, RuntimeError> {
        Err(crate::error::runtime_error(
            "object.__reduce__ must be dispatched via Interpreter::call",
        ))
    }
    dict.insert(
        DictKey(Object::from_static("__reduce_ex__")),
        Object::Builtin(Rc::new(BuiltinFn {
            name: ".object_reduce_ex",
            binds_instance: true,
            call: Box::new(object_reduce_ex_sentinel),
            call_kw: None,
        })),
    );
    dict.insert(
        DictKey(Object::from_static("__reduce__")),
        Object::Builtin(Rc::new(BuiltinFn {
            name: ".object_reduce",
            binds_instance: true,
            call: Box::new(object_reduce_sentinel),
            call_kw: None,
        })),
    );
    // `object.__getattribute__(self, name)` — the default attribute
    // lookup (data descriptor → instance dict → class attr → AttributeError).
    // Needs VM access to run the descriptor protocol and walk the MRO, so it
    // is wired through a sentinel name that `Interpreter::call` intercepts
    // (both bound `x.__getattribute__(name)` and unbound
    // `object.__getattribute__(x, name)` forms). Exposing it here lets a
    // user-defined `__getattribute__` delegate to `object.__getattribute__`
    // (the canonical CPython idiom), and lets `load_attr` distinguish a real
    // override from this default without a special is-defined-on-object flag.
    fn object_getattribute_sentinel(_args: &[Object]) -> Result<Object, RuntimeError> {
        Err(crate::error::runtime_error(
            "object.__getattribute__ must be dispatched via Interpreter::call",
        ))
    }
    dict.insert(
        DictKey(Object::from_static("__getattribute__")),
        Object::Builtin(Rc::new(BuiltinFn {
            name: ".object_getattribute",
            binds_instance: true,
            call: Box::new(object_getattribute_sentinel),
            call_kw: None,
        })),
    );
}

/// Install `type.__new__` and `type.__init__` so user metaclasses
/// can do `super().__new__(mcs, name, bases, ns)` to allocate a
/// fresh class. The implementations are picked up by [`Vm::call`]
/// via the `metaclass.__new__` lookup at class-build time.
pub fn install_type_dunders(type_: &Rc<TypeObject>) {
    use crate::object::BuiltinFn;
    fn type_new_sentinel(_args: &[Object]) -> Result<Object, RuntimeError> {
        // Reaching this path means `type.__new__` was invoked
        // outside the VM's class-build context. The interpreter
        // intercepts the real path before we ever get called.
        Err(crate::error::runtime_error(
            "type.__new__ must be called through the VM class-build path",
        ))
    }
    fn type_init(_args: &[Object]) -> Result<Object, RuntimeError> {
        // The corresponding init is a no-op; user metaclasses can
        // still override it.
        Ok(Object::None)
    }
    // `type.__setattr__(cls, name, value)` — CPython `type_setattro`. This is
    // the default a metaclass override chains to via `super().__setattr__`,
    // and (unlike `object.__setattr__`) it is permitted to mutate a class.
    fn type_setattr(args: &[Object]) -> Result<Object, RuntimeError> {
        if args.len() != 3 {
            return Err(crate::error::type_error(
                "type.__setattr__() takes exactly 3 arguments".to_owned(),
            ));
        }
        let Object::Type(ty) = &args[0] else {
            return Err(crate::error::type_error(format!(
                "descriptor '__setattr__' requires a 'type' object but received a '{}'",
                args[0].type_name()
            )));
        };
        let name = match &args[1] {
            Object::Str(s) => s.to_string(),
            _ => return Err(crate::error::type_error("attribute name must be string")),
        };
        let ptr = crate::vm_singletons::current_interpreter_ptr().ok_or_else(|| {
            crate::error::runtime_error("type.__setattr__ requires an active interpreter")
        })?;
        // SAFETY: published by an enclosing VM frame live on this thread.
        let interp = unsafe { &mut *ptr };
        interp.set_type_attr_direct(ty, &name, args[2].clone())?;
        Ok(Object::None)
    }
    fn type_delattr(args: &[Object]) -> Result<Object, RuntimeError> {
        if args.len() != 2 {
            return Err(crate::error::type_error(
                "type.__delattr__() takes exactly 2 arguments".to_owned(),
            ));
        }
        let Object::Type(ty) = &args[0] else {
            return Err(crate::error::type_error(format!(
                "descriptor '__delattr__' requires a 'type' object but received a '{}'",
                args[0].type_name()
            )));
        };
        let name = match &args[1] {
            Object::Str(s) => s.to_string(),
            _ => return Err(crate::error::type_error("attribute name must be string")),
        };
        let ptr = crate::vm_singletons::current_interpreter_ptr().ok_or_else(|| {
            crate::error::runtime_error("type.__delattr__ requires an active interpreter")
        })?;
        // SAFETY: published by an enclosing VM frame live on this thread.
        let interp = unsafe { &mut *ptr };
        interp.del_type_attr_direct(ty, &name)?;
        Ok(Object::None)
    }
    // `type.__doc__` / `__qualname__` / `__name__` are getset *data
    // descriptors* on the metatype (CPython `type_getsets`), not plain dict
    // strings. Modelling them as real descriptors lets
    // `type(C).__dict__['__doc__'].__set__/__delete__` (test_descr
    // test_set_doc) and `type.__dict__['__qualname__'].__set__` (test_qualname)
    // behave like CPython, while normal `C.__doc__`/`C.__name__` reads stay on
    // their existing fast paths (`load_attr_type` resolves name/qualname from
    // the type's own fields before the metaclass descriptor is consulted).
    fn type_doc_get(args: &[Object]) -> Result<Object, RuntimeError> {
        let Some(Object::Type(ty)) = args.first() else {
            return Err(crate::error::type_error(
                "descriptor '__doc__' for 'type' objects doesn't apply to other objects",
            ));
        };
        // Built-in types expose their curated `tp_doc`; heap classes carry
        // an own-dict `__doc__` (set to the body docstring or `None` at
        // class creation), never inheriting a base's docstring.
        if ty.flags.is_builtin {
            return Ok(crate::builtin_type_doc(&ty.name)
                .map(Object::from_static)
                .unwrap_or(Object::None));
        }
        let entry = ty
            .dict
            .borrow()
            .get(&DictKey(Object::from_static("__doc__")))
            .cloned();
        match entry {
            None => Ok(Object::None),
            // A plain docstring (or `None`) is returned verbatim; the rare
            // descriptor-valued `__doc__` (`__doc__ = SomeDescr()`) has the
            // descriptor protocol applied, matching CPython's `type_get_doc`
            // (test_descr test_doc_descriptor).
            Some(v @ (Object::Str(_) | Object::None)) => Ok(v),
            Some(v) => {
                let ptr = crate::vm_singletons::current_interpreter_ptr().ok_or_else(|| {
                    crate::error::runtime_error(
                        "type.__doc__ getter requires an active interpreter",
                    )
                })?;
                // SAFETY: published by an enclosing VM frame live on this thread.
                let interp = unsafe { &mut *ptr };
                interp.descriptor_get(&v, &Object::None, &args[0])
            }
        }
    }
    fn type_doc_set(args: &[Object]) -> Result<Object, RuntimeError> {
        let Some(Object::Type(ty)) = args.first() else {
            return Err(crate::error::type_error(
                "descriptor '__doc__' for 'type' objects doesn't apply to other objects",
            ));
        };
        if ty.flags.is_builtin {
            return Err(crate::error::type_error(format!(
                "cannot set '__doc__' attribute of immutable type '{}'",
                ty.name
            )));
        }
        let value = args.get(1).cloned().unwrap_or(Object::None);
        ty.dict
            .borrow_mut()
            .insert(DictKey(Object::from_static("__doc__")), value);
        Ok(Object::None)
    }
    fn type_doc_del(args: &[Object]) -> Result<Object, RuntimeError> {
        // CPython's `check_set_special_type_attr` reports the *immutable*
        // wording even for heap classes on deletion (there is no deleter),
        // so `del`/`__delete__` always raises here.
        let name = match args.first() {
            Some(Object::Type(ty)) => ty.name.clone(),
            _ => "?".to_owned(),
        };
        Err(crate::error::type_error(format!(
            "cannot delete '__doc__' attribute of immutable type '{name}'"
        )))
    }
    fn type_qualname_get(args: &[Object]) -> Result<Object, RuntimeError> {
        let Some(Object::Type(ty)) = args.first() else {
            return Err(crate::error::type_error(
                "descriptor '__qualname__' for 'type' objects doesn't apply to other objects",
            ));
        };
        if let Some(q) = ty.qualname.borrow().as_ref() {
            return Ok(Object::interned_str(q));
        }
        Ok(Object::interned_str(&ty.name))
    }
    fn type_qualname_set(args: &[Object]) -> Result<Object, RuntimeError> {
        let Some(Object::Type(ty)) = args.first() else {
            return Err(crate::error::type_error(
                "descriptor '__qualname__' for 'type' objects doesn't apply to other objects",
            ));
        };
        let value = args.get(1).cloned().unwrap_or(Object::None);
        let ptr = crate::vm_singletons::current_interpreter_ptr().ok_or_else(|| {
            crate::error::runtime_error("type.__qualname__ setter requires an active interpreter")
        })?;
        // SAFETY: published by an enclosing VM frame live on this thread.
        // `set_type_attr_direct` rejects immutable types (test_qualname:
        // `type.__dict__['__qualname__'].__set__(str, 'Oink')` → TypeError)
        // and validates the value is a string.
        let interp = unsafe { &mut *ptr };
        interp.set_type_attr_direct(ty, "__qualname__", value)?;
        Ok(Object::None)
    }
    fn type_qualname_del(args: &[Object]) -> Result<Object, RuntimeError> {
        let name = match args.first() {
            Some(Object::Type(ty)) => ty.name.clone(),
            _ => "?".to_owned(),
        };
        Err(crate::error::type_error(format!(
            "can't delete {name}.__qualname__"
        )))
    }
    fn type_name_get(args: &[Object]) -> Result<Object, RuntimeError> {
        let Some(Object::Type(ty)) = args.first() else {
            return Err(crate::error::type_error(
                "descriptor '__name__' for 'type' objects doesn't apply to other objects",
            ));
        };
        // Honour an own-dict string override (a reassigned `__name__`),
        // otherwise the type's own name — mirroring `load_attr_type`.
        if let Some(v @ Object::Str(_)) = ty
            .dict
            .borrow()
            .get(&DictKey(Object::from_static("__name__")))
            .cloned()
        {
            return Ok(v);
        }
        Ok(Object::interned_str(&ty.name))
    }
    fn type_name_set(args: &[Object]) -> Result<Object, RuntimeError> {
        let Some(Object::Type(ty)) = args.first() else {
            return Err(crate::error::type_error(
                "descriptor '__name__' for 'type' objects doesn't apply to other objects",
            ));
        };
        let value = args.get(1).cloned().unwrap_or(Object::None);
        let ptr = crate::vm_singletons::current_interpreter_ptr().ok_or_else(|| {
            crate::error::runtime_error("type.__name__ setter requires an active interpreter")
        })?;
        // SAFETY: published by an enclosing VM frame live on this thread.
        let interp = unsafe { &mut *ptr };
        interp.set_type_attr_direct(ty, "__name__", value)?;
        Ok(Object::None)
    }
    fn type_name_del(args: &[Object]) -> Result<Object, RuntimeError> {
        let name = match args.first() {
            Some(Object::Type(ty)) => ty.name.clone(),
            _ => "?".to_owned(),
        };
        Err(crate::error::type_error(format!(
            "can't delete {name}.__name__"
        )))
    }
    // `type.__mro__` / `type.__dict__` are read-only getsets on the
    // metatype. Exposing them *in the type dict* (not just on the
    // attribute fast path) matters because `inspect.py` (verbatim,
    // RFC 0053) resolves them statically: `_static_getmro =
    // type.__dict__['__mro__'].__get__`, and likewise for
    // `__dict__` — the whole point being to bypass any metaclass
    // `__getattribute__`.
    fn type_mro_get(args: &[Object]) -> Result<Object, RuntimeError> {
        let Some(Object::Type(ty)) = args.first() else {
            return Err(crate::error::type_error(
                "descriptor '__mro__' for 'type' objects doesn't apply to other objects",
            ));
        };
        if ty.mro.borrow().is_empty() && !ty.flags.is_builtin {
            return Ok(Object::None);
        }
        Ok(Object::new_tuple(
            ty.mro
                .borrow()
                .iter()
                .map(|b| Object::Type(b.clone()))
                .collect(),
        ))
    }
    fn type_mro_set(_args: &[Object]) -> Result<Object, RuntimeError> {
        Err(crate::error::attribute_error("readonly attribute"))
    }
    fn type_bases_get(args: &[Object]) -> Result<Object, RuntimeError> {
        let Some(Object::Type(ty)) = args.first() else {
            return Err(crate::error::type_error(
                "descriptor '__bases__' for 'type' objects doesn't apply to other objects",
            ));
        };
        // gh-132176: `type()` called with a tuple-*subclass* bases object
        // keeps that object in `tp_bases` unchanged, so
        // `type(typ.__bases__)` reports the subclass.
        if let Some(orig) = ty
            .dict
            .borrow()
            .get(&DictKey(Object::from_static("__weavepy_bases_obj__")))
        {
            return Ok(orig.clone());
        }
        Ok(Object::new_tuple(
            ty.bases
                .borrow()
                .iter()
                .map(|b| Object::Type(b.clone()))
                .collect(),
        ))
    }
    fn type_bases_set(args: &[Object]) -> Result<Object, RuntimeError> {
        let Some(Object::Type(ty)) = args.first() else {
            return Err(crate::error::type_error(
                "descriptor '__bases__' for 'type' objects doesn't apply to other objects",
            ));
        };
        let value = args.get(1).cloned().unwrap_or(Object::None);
        let ptr = crate::vm_singletons::current_interpreter_ptr().ok_or_else(|| {
            crate::error::runtime_error("type.__bases__ setter requires an active interpreter")
        })?;
        // SAFETY: published by an enclosing VM frame live on this thread.
        let interp = unsafe { &mut *ptr };
        // Routes through `set_type_attr_direct` so the PEP 578
        // `object.__setattr__` audit and the full `type_set_bases`
        // MRO recomputation both run — `type.__dict__['__bases__']
        // .__set__(C, …)` must be indistinguishable from
        // `C.__bases__ = …` (test_audit test_monkeypatch).
        interp.set_type_attr_direct(ty, "__bases__", value)?;
        Ok(Object::None)
    }
    fn type_bases_del(args: &[Object]) -> Result<Object, RuntimeError> {
        let name = match args.first() {
            Some(Object::Type(ty)) => ty.name.clone(),
            _ => "?".to_owned(),
        };
        Err(crate::error::type_error(format!(
            "can't delete {name}.__bases__"
        )))
    }
    fn type_dunder_dict_get(args: &[Object]) -> Result<Object, RuntimeError> {
        let Some(Object::Type(ty)) = args.first() else {
            return Err(crate::error::type_error(
                "descriptor '__dict__' for 'type' objects doesn't apply to other objects",
            ));
        };
        Ok(Object::MappingProxy(ty.dict.clone()))
    }
    fn type_dunder_dict_set(_args: &[Object]) -> Result<Object, RuntimeError> {
        Err(crate::error::attribute_error("readonly attribute"))
    }
    type GetSetFn = fn(&[Object]) -> Result<Object, RuntimeError>;
    fn mk_getset(name: &'static str, get: GetSetFn, set: GetSetFn, del: GetSetFn) -> Object {
        Object::Property(Rc::new(crate::object::PyProperty::new(
            Object::Builtin(Rc::new(BuiltinFn {
                name,
                binds_instance: true,
                call: Box::new(get),
                call_kw: None,
            })),
            Object::Builtin(Rc::new(BuiltinFn {
                name,
                binds_instance: true,
                call: Box::new(set),
                call_kw: None,
            })),
            Object::Builtin(Rc::new(BuiltinFn {
                name,
                binds_instance: true,
                call: Box::new(del),
                call_kw: None,
            })),
            Object::None,
        )))
    }
    for (name, getset) in [
        (
            "__doc__",
            mk_getset("__doc__", type_doc_get, type_doc_set, type_doc_del),
        ),
        (
            "__qualname__",
            mk_getset(
                "__qualname__",
                type_qualname_get,
                type_qualname_set,
                type_qualname_del,
            ),
        ),
        (
            "__name__",
            mk_getset("__name__", type_name_get, type_name_set, type_name_del),
        ),
        (
            "__mro__",
            mk_getset("__mro__", type_mro_get, type_mro_set, type_mro_set),
        ),
        (
            "__bases__",
            mk_getset("__bases__", type_bases_get, type_bases_set, type_bases_del),
        ),
        (
            "__dict__",
            mk_getset(
                "__dict__",
                type_dunder_dict_get,
                type_dunder_dict_set,
                type_dunder_dict_set,
            ),
        ),
    ] {
        crate::descr_registry::register(
            &getset,
            crate::descr_registry::DescrKind::GetSet,
            type_.clone(),
            name,
            None,
        );
        type_
            .dict
            .borrow_mut()
            .insert(DictKey(Object::from_static(name)), getset);
    }
    let mut dict = type_.dict.borrow_mut();
    dict.insert(
        DictKey(Object::from_static("__setattr__")),
        Object::Builtin(Rc::new(BuiltinFn {
            name: "__setattr__",
            binds_instance: true,
            call: Box::new(type_setattr),
            call_kw: None,
        })),
    );
    dict.insert(
        DictKey(Object::from_static("__delattr__")),
        Object::Builtin(Rc::new(BuiltinFn {
            name: "__delattr__",
            binds_instance: true,
            call: Box::new(type_delattr),
            call_kw: None,
        })),
    );
    dict.insert(
        DictKey(Object::from_static("__new__")),
        Object::StaticMethod(MethodWrapper::new(Object::Builtin(Rc::new(BuiltinFn {
            name: "__new__",
            binds_instance: true,
            call: Box::new(type_new_sentinel),
            call_kw: None,
        })))),
    );
    dict.insert(
        DictKey(Object::from_static("__init__")),
        Object::Builtin(Rc::new(BuiltinFn {
            name: "__init__",
            binds_instance: true,
            call: Box::new(type_init),
            call_kw: None,
        })),
    );
}

fn install_import_error_init(import_error: &Rc<TypeObject>) {
    use crate::object::BuiltinFn;
    // `ImportError.__init__(self, *args, name=None, path=None,
    // name_from=None)` — CPython `ImportError_init`: every named field
    // resets on each call (gh test_reset_attributes), `msg` is the sole
    // positional when there is exactly one.
    fn import_error_init_impl(
        args: &[Object],
        kwargs: &[(String, Object)],
    ) -> Result<Object, RuntimeError> {
        let inst = args
            .first()
            .ok_or_else(|| crate::error::type_error("expected exception instance".to_owned()))?;
        if let Object::Instance(inst_rc) = inst {
            let rest = if args.len() > 1 { &args[1..] } else { &[][..] };
            let mut name = Object::None;
            let mut path = Object::None;
            let mut name_from = Object::None;
            for (k, v) in kwargs {
                match k.as_str() {
                    "name" => name = v.clone(),
                    "path" => path = v.clone(),
                    "name_from" => name_from = v.clone(),
                    other => {
                        return Err(crate::error::type_error(format!(
                            "ImportError() got an unexpected keyword argument '{other}'"
                        )))
                    }
                }
            }
            inst_rc.slot_set("args", Object::new_tuple(rest.to_vec()));
            inst_rc.slot_set(
                "msg",
                if rest.len() == 1 {
                    rest[0].clone()
                } else {
                    Object::None
                },
            );
            inst_rc.slot_set("name", name);
            inst_rc.slot_set("path", path);
            inst_rc.slot_set("name_from", name_from);
        }
        Ok(Object::None)
    }
    let mut dict = import_error.dict.borrow_mut();
    dict.insert(
        DictKey(Object::from_static("__init__")),
        Object::Builtin(Rc::new(BuiltinFn {
            name: "__init__",
            binds_instance: true,
            call: Box::new(|args| import_error_init_impl(args, &[])),
            call_kw: Some(Box::new(|args, kwargs| {
                import_error_init_impl(args, kwargs)
            })),
        })),
    );
}

fn install_os_error_init(os_error: &Rc<TypeObject>) {
    use crate::object::BuiltinFn;
    fn oserror_init(args: &[Object]) -> Result<Object, RuntimeError> {
        // OSError(errno, strerror, [filename, [winerror, filename2]])
        // — populate named attributes from the positional args.
        let inst = args
            .first()
            .ok_or_else(|| crate::error::type_error("expected exception instance".to_owned()))?;
        if let Object::Instance(inst_rc) = inst {
            // CPython's `oserror_use_init` (issue12555): when a subclass
            // overrides `__new__` but *not* `__init__`, everything was
            // already done in the overridden `__new__`'s chain and
            // `OSError.__init__` must leave `args` alone
            // (test_exception_hierarchy.test_new_overridden — the extra
            // `baz` argument is dropped, not folded into `.args`).
            {
                let cls = inst_rc.cls();
                // A *user* `__new__` is a Python function or a
                // staticmethod wrapping one — the default allocator is
                // also StaticMethod-wrapped but wraps the builtin named
                // `__new__` (same discrimination as `instance_plan`).
                // Matching it here made every plain OSError subclass
                // skip errno/strerror parsing (test_ssl's SSLError
                // BIO loop branches on `e.errno`).
                let user_new = match cls.lookup("__new__") {
                    Some(Object::Function(_)) => true,
                    Some(Object::StaticMethod(inner)) => {
                        !matches!(&inner.func(), Object::Builtin(b) if b.name == "__new__")
                    }
                    _ => false,
                };
                let user_init = matches!(cls.lookup("__init__"), Some(Object::Function(_)));
                if user_new && !user_init {
                    return Ok(Object::None);
                }
            }
            let rest = if args.len() > 1 { &args[1..] } else { &[][..] };
            // CPython `oserror_init` special case: a `BlockingIOError` (and
            // subclasses) built with *exactly three* positional args whose
            // third is a *number* treats it as `characters_written` rather
            // than `filename` and leaves `filename` unset
            // (`test_io.test_write_non_blocking` relies on
            // `BlockingIOError(EAGAIN, msg, written).characters_written`;
            // a non-numeric third arg parses as a plain OSError —
            // test_exception_hierarchy.test_blockingioerror).
            let is_blocking = inst_rc
                .cls()
                .is_subclass_of(&builtin_types().blocking_io_error);
            if is_blocking && rest.len() == 3 && matches!(rest[2], Object::Int(_) | Object::Long(_))
            {
                inst_rc.slot_set("args", Object::new_tuple(rest.to_vec()));
                inst_rc.slot_set("errno", rest[0].clone());
                inst_rc.slot_set("strerror", rest[1].clone());
                inst_rc.slot_set("characters_written", rest[2].clone());
                return Ok(Object::None);
            }
            // CPython `oserror_init`: the named fields populate only
            // for the 2..5-positional forms, and `.args` keeps just
            // `(errno, strerror)` in those forms; otherwise the full
            // tuple is stored and the fields stay None.
            let populated = (2..=5).contains(&rest.len());
            let args_tuple = if populated {
                Object::new_tuple(rest[..2].to_vec())
            } else {
                Object::new_tuple(rest.to_vec())
            };
            inst_rc.slot_set("args", args_tuple);
            let pick = |i: usize| {
                if populated {
                    rest.get(i).cloned().unwrap_or(Object::None)
                } else {
                    Object::None
                }
            };
            // Only set fields that have real values. CPython keeps these in
            // C slots, so a subclass's *class attribute* (`class Err(OSError):
            // errno = EINVAL`) is what attribute lookup finds when the slot
            // was never populated — writing `None` into the instance dict
            // here would shadow it (asyncio's add_signal_handler tests build
            // exactly such subclasses). The type-level `None` defaults below
            // cover the genuinely-unset case.
            for (i, name) in ["errno", "strerror", "filename", "winerror", "filename2"]
                .into_iter()
                .enumerate()
            {
                // The 4th positional (winerror) is accepted but *ignored*
                // on posix, where the member doesn't exist at all.
                if cfg!(not(windows)) && name == "winerror" {
                    continue;
                }
                let v = pick(i);
                if !matches!(v, Object::None) {
                    inst_rc.slot_set(name, v);
                }
            }
        }
        Ok(Object::None)
    }
    // CPython's `OSError_str` (`Objects/exceptions.c`): prefer the
    // `[Errno N] strerror[: filename[ -> filename2]]` shape when the
    // named fields are populated (the 2..5-arg form), else fall back to
    // `BaseException.__str__`. The named slots default to `None`, which
    // we treat as "unset".
    fn oserror_str(args: &[Object]) -> Result<Object, RuntimeError> {
        let Some(Object::Instance(inst)) = args.first() else {
            return Ok(Object::from_static(""));
        };
        let get = |name: &'static str| exc_attr(inst, name);
        let set = |o: &Option<Object>| matches!(o, Some(v) if !matches!(v, Object::None));
        let errno = get("errno");
        let strerror = get("strerror");
        let filename = get("filename");
        let filename2 = get("filename2");
        let errno_s = errno.as_ref().map(Object::to_str).unwrap_or_default();
        let strerror_s = strerror.as_ref().map(Object::to_str).unwrap_or_default();
        if set(&filename) {
            let f1 = filename.as_ref().map(Object::repr).unwrap_or_default();
            if set(&filename2) {
                let f2 = filename2.as_ref().map(Object::repr).unwrap_or_default();
                return Ok(Object::from_str(format!(
                    "[Errno {errno_s}] {strerror_s}: {f1} -> {f2}"
                )));
            }
            return Ok(Object::from_str(format!(
                "[Errno {errno_s}] {strerror_s}: {f1}"
            )));
        }
        if set(&errno) && set(&strerror) {
            return Ok(Object::from_str(format!("[Errno {errno_s}] {strerror_s}")));
        }
        // BaseException.__str__: "" / str(arg) / repr(args).
        match get("args") {
            Some(Object::Tuple(items)) => Ok(match items.as_ref() {
                [] => Object::from_static(""),
                [single] => Object::from_str(single.to_str()),
                _ => Object::from_str(format!(
                    "({})",
                    items
                        .iter()
                        .map(Object::repr)
                        .collect::<Vec<_>>()
                        .join(", ")
                )),
            }),
            _ => Ok(Object::from_static("")),
        }
    }
    let mut dict = os_error.dict.borrow_mut();
    dict.insert(
        DictKey(Object::from_static("__init__")),
        Object::Builtin(Rc::new(BuiltinFn {
            name: "__init__",
            binds_instance: true,
            call: Box::new(oserror_init),
            call_kw: None,
        })),
    );
    dict.insert(
        DictKey(Object::from_static("__str__")),
        Object::Builtin(Rc::new(BuiltinFn {
            name: "__str__",
            binds_instance: true,
            call: Box::new(oserror_str),
            call_kw: None,
        })),
    );
    // CPython exposes `errno`/`strerror`/`filename`/`filename2`/`winerror`
    // as getset descriptors on the `OSError` type that default to `None`.
    // Subclasses that override `__init__` *without* chaining to
    // `OSError.__init__` (e.g. `urllib.error.URLError`, which only sets
    // `args`/`reason`) still expect `inst.filename` to resolve — so provide
    // the defaults at the type level, where instance-dict entries shadow
    // them once a real value is assigned.
    // `winerror` is Windows-only in CPython (`#ifdef MS_WINDOWS` member):
    // `dir(OSError)` on posix must not show it
    // (test_exception_hierarchy.test_windows_error).
    #[cfg(windows)]
    const OSERROR_FIELDS: [&str; 5] = ["errno", "strerror", "filename", "filename2", "winerror"];
    #[cfg(not(windows))]
    const OSERROR_FIELDS: [&str; 4] = ["errno", "strerror", "filename", "filename2"];
    for name in OSERROR_FIELDS {
        dict.insert(
            DictKey(Object::from_static(name)),
            exc_slot(name, "OSError", Object::None),
        );
    }
    // `characters_written` raises AttributeError while unset (CPython's
    // getset has no default), so its descriptor carries no fallback.
    dict.insert(
        DictKey(Object::from_static("characters_written")),
        Object::SlotDescriptor(Rc::new(crate::object::SlotDescriptor {
            name: "characters_written".to_owned(),
            class_name: "OSError".to_owned(),
            default: None,
            readonly: false,
            doc: None,
            objclass: crate::sync::RefCell::new(None),
        })),
    );
}

/// Which of the three concrete unicode errors we're installing dunders
/// for. They share storage (`object`/`start`/`end`/`reason`, plus
/// `encoding` for the codec variants) but differ in constructor arity
/// and the `__str__` message shape.
#[derive(Clone, Copy, PartialEq, Eq)]
enum UnicodeErrorKind {
    Encode,
    Decode,
    Translate,
}

/// Install `__init__` / `__str__` for `UnicodeEncodeError`,
/// `UnicodeDecodeError`, and `UnicodeTranslateError`, mirroring CPython's
/// `Objects/exceptions.c` (`UnicodeEncodeError_init`, `…_str`, etc.).
///
/// Constructors:
///   * encode/decode: `(encoding, object, start, end, reason)`
///   * translate:     `(object, start, end, reason)`
///
/// `__str__` reproduces the exact CPython wording, including the
/// single-element `'\\xXX'` / `'\\uXXXX'` / `'\\UXXXXXXXX'` escape for a
/// one-position slice and the `position M-N` form for a range.
fn install_unicode_error_dunders(ty: &Rc<TypeObject>, kind: UnicodeErrorKind) {
    use crate::object::BuiltinFn;

    let init = move |args: &[Object]| -> Result<Object, RuntimeError> {
        let Some(Object::Instance(inst_rc)) = args.first() else {
            return Ok(Object::None);
        };
        let rest = if args.len() > 1 { &args[1..] } else { &[][..] };
        let want = if kind == UnicodeErrorKind::Translate {
            4
        } else {
            5
        };
        if rest.len() != want {
            return Err(crate::error::type_error(format!(
                "function takes exactly {} arguments ({} given)",
                want,
                rest.len()
            )));
        }
        // CPython parses with `PyArg_ParseTuple("UUnnU" / "UOnnU" / "UnnU")`:
        // wrong-typed arguments raise TypeError at construction.
        let is_str = |o: &Object| {
            matches!(o, Object::Str(_) | Object::WStr(_))
                || matches!(o, Object::Instance(i)
                    if i.cls().mro.borrow().iter().any(|t| t.name == "str"))
        };
        let is_index = |o: &Object| matches!(o, Object::Int(_) | Object::Bool(_));
        let is_buffer = |o: &Object| {
            matches!(
                o,
                Object::Bytes(_) | Object::ByteArray(_) | Object::MemoryView(_)
            )
        };
        let check = |ok: bool, pos: usize, expect: &str, got: &Object| {
            if ok {
                Ok(())
            } else {
                Err(crate::error::type_error(format!(
                    "argument {} must be {expect}, not {}",
                    pos + 1,
                    got.type_name_owned()
                )))
            }
        };
        match kind {
            UnicodeErrorKind::Encode => {
                check(is_str(&rest[0]), 0, "str", &rest[0])?;
                check(is_str(&rest[1]), 1, "str", &rest[1])?;
                check(is_index(&rest[2]), 2, "int", &rest[2])?;
                check(is_index(&rest[3]), 3, "int", &rest[3])?;
                check(is_str(&rest[4]), 4, "str", &rest[4])?;
            }
            UnicodeErrorKind::Decode => {
                check(is_str(&rest[0]), 0, "str", &rest[0])?;
                check(is_buffer(&rest[1]), 1, "a bytes-like object", &rest[1])?;
                check(is_index(&rest[2]), 2, "int", &rest[2])?;
                check(is_index(&rest[3]), 3, "int", &rest[3])?;
                check(is_str(&rest[4]), 4, "str", &rest[4])?;
            }
            UnicodeErrorKind::Translate => {
                check(is_str(&rest[0]), 0, "str", &rest[0])?;
                check(is_index(&rest[1]), 1, "int", &rest[1])?;
                check(is_index(&rest[2]), 2, "int", &rest[2])?;
                check(is_str(&rest[3]), 3, "str", &rest[3])?;
            }
        }
        inst_rc.slot_set("args", Object::new_tuple(rest.to_vec()));
        let mut i = 0;
        if kind != UnicodeErrorKind::Translate {
            inst_rc.slot_set("encoding", rest[i].clone());
            i += 1;
        }
        // Decode errors normalize a bytes-like payload to `bytes`
        // (CPython `UnicodeDecodeError_init` via PyObject_GetBuffer).
        let object = match (&kind, &rest[i]) {
            (UnicodeErrorKind::Decode, Object::ByteArray(b)) => {
                let bytes: Vec<u8> = b.borrow().clone();
                Object::new_bytes(bytes)
            }
            _ => rest[i].clone(),
        };
        inst_rc.slot_set("object", object);
        inst_rc.slot_set("start", rest[i + 1].clone());
        inst_rc.slot_set("end", rest[i + 2].clone());
        inst_rc.slot_set("reason", rest[i + 3].clone());
        Ok(Object::None)
    };

    let str_fn = move |args: &[Object]| -> Result<Object, RuntimeError> {
        let Some(Object::Instance(inst_rc)) = args.first() else {
            return Ok(Object::from_static(""));
        };
        let get = |name: &'static str| exc_attr(inst_rc, name);
        let as_i = |o: &Object| -> i64 {
            match o {
                Object::Int(n) => *n,
                Object::Bool(b) => i64::from(*b),
                _ => 0,
            }
        };
        // `encoding` / `reason` render via str() whatever their type —
        // attributes are reassignable after construction (issue 7309).
        let encoding = get("encoding").map(|o| o.to_str()).unwrap_or_default();
        let reason = get("reason").map(|o| o.to_str()).unwrap_or_default();
        let start = get("start").as_ref().map(as_i).unwrap_or(0);
        let end = get("end").as_ref().map(as_i).unwrap_or(0);

        // CPython: a half-built instance (`__new__` without `__init__`)
        // falls back to `BaseException.__str__` — "" for empty args,
        // str(arg) for one, repr(args) otherwise.
        if get("object").is_none() || get("reason").is_none() {
            let args = get("args");
            return Ok(match args {
                Some(Object::Tuple(t)) => match t.as_ref() {
                    [] => Object::from_static(""),
                    [single] => Object::from_str(single.to_str()),
                    _ => Object::from_str(Object::Tuple(t.clone()).repr()),
                },
                _ => Object::from_static(""),
            });
        }
        let obj = get("object").unwrap_or(Object::None);

        // Escape a single offending scalar exactly as CPython does.
        let escape = |c: u32| -> String {
            if c < 0x100 {
                format!("\\x{c:02x}")
            } else if c < 0x10000 {
                format!("\\u{c:04x}")
            } else {
                format!("\\U{c:08x}")
            }
        };

        let msg = match kind {
            UnicodeErrorKind::Encode => {
                let s: Vec<u32> = match &obj {
                    Object::Str(s) => s.chars().map(|c| c as u32).collect(),
                    // A WStr carries the lone surrogate the encode
                    // rejected; the message must show it verbatim
                    // (CPython: "can't encode character '\udac0'").
                    Object::WStr(cps) => cps.to_vec(),
                    _ => Vec::new(),
                };
                if start >= 0 && (start as usize) < s.len() && end == start + 1 {
                    let c = s[start as usize];
                    format!(
                        "'{encoding}' codec can't encode character '{}' in position {start}: {reason}",
                        escape(c)
                    )
                } else {
                    format!(
                        "'{encoding}' codec can't encode characters in position {start}-{}: {reason}",
                        end - 1
                    )
                }
            }
            UnicodeErrorKind::Decode => {
                let b: &[u8] = match &obj {
                    Object::Bytes(b) => b,
                    _ => &[],
                };
                if start >= 0 && (start as usize) < b.len() && end == start + 1 {
                    format!(
                        "'{encoding}' codec can't decode byte 0x{:02x} in position {start}: {reason}",
                        b[start as usize]
                    )
                } else {
                    format!(
                        "'{encoding}' codec can't decode bytes in position {start}-{}: {reason}",
                        end - 1
                    )
                }
            }
            UnicodeErrorKind::Translate => {
                let s: Vec<char> = match &obj {
                    Object::Str(s) => s.chars().collect(),
                    _ => Vec::new(),
                };
                if start >= 0 && (start as usize) < s.len() && end == start + 1 {
                    let c = s[start as usize] as u32;
                    format!(
                        "can't translate character '{}' in position {start}: {reason}",
                        escape(c)
                    )
                } else {
                    format!(
                        "can't translate characters in position {start}-{}: {reason}",
                        end - 1
                    )
                }
            }
        };
        Ok(Object::from_str(msg))
    };

    let mut dict = ty.dict.borrow_mut();
    dict.insert(
        DictKey(Object::from_static("__init__")),
        Object::Builtin(Rc::new(BuiltinFn {
            name: "__init__",
            binds_instance: true,
            call: Box::new(init),
            call_kw: None,
        })),
    );
    dict.insert(
        DictKey(Object::from_static("__str__")),
        Object::Builtin(Rc::new(BuiltinFn {
            name: "__str__",
            binds_instance: true,
            call: Box::new(str_fn),
            call_kw: None,
        })),
    );
}

/// CPython's `SyntaxError.__init__` / `__str__`.
///
/// `__init__(self, *args)` stores `args` like `BaseException`, then — when
/// called as `SyntaxError(msg, (filename, lineno, offset, text[, end_lineno,
/// end_offset]))` — unpacks the detail sequence into named attributes.
/// `__str__` reproduces CPython's `SyntaxError_str`: bare `msg` unless a
/// filename and/or line are present, in which case it appends
/// `" (<basename>, line N)"` / `" (<basename>)"` / `" (line N)"`.
fn install_syntax_error_dunders(syntax_error: &Rc<TypeObject>) {
    use crate::object::BuiltinFn;

    fn set(dict: &mut crate::object::DictData, name: &'static str, value: Object) {
        dict.insert(DictKey(Object::from_static(name)), value);
    }

    fn syntaxerror_init(args: &[Object]) -> Result<Object, RuntimeError> {
        let inst = args
            .first()
            .ok_or_else(|| crate::error::type_error("expected exception instance".to_owned()))?;
        let Object::Instance(inst_rc) = inst else {
            return Ok(Object::None);
        };
        let rest = if args.len() > 1 { &args[1..] } else { &[][..] };
        inst_rc.slot_set("args", Object::new_tuple(rest.to_vec()));
        // Defaults — CPython always defines these slots.
        for name in [
            "msg",
            "filename",
            "lineno",
            "offset",
            "text",
            "end_lineno",
            "end_offset",
        ] {
            inst_rc.slot_set(name, Object::None);
        }
        if let Some(msg) = rest.first() {
            inst_rc.slot_set("msg", msg.clone());
        }
        // `SyntaxError(msg, detail)` — `detail` is a `(filename, lineno,
        // offset, text[, end_lineno, end_offset])` sequence. CPython runs
        // it through `PySequence_Tuple` and requires exactly 4 or 6
        // items (5 gets a dedicated message).
        if rest.len() == 2 {
            let items: Vec<Object> = match &rest[1] {
                Object::Tuple(items) => items.to_vec(),
                Object::List(items) => items.borrow().clone(),
                // Any other sequence goes through `tuple()` like
                // CPython's `PySequence_Tuple` — including strings
                // (`SyntaxError('error', 'abcd')` unpacks to 4 chars).
                other => {
                    let mut it = other.make_iter().map_err(|_| {
                        crate::error::type_error(format!(
                            "'{}' object is not iterable",
                            other.type_name()
                        ))
                    })?;
                    let mut out = Vec::new();
                    while let Some(v) = it.next_value() {
                        out.push(v);
                    }
                    out
                }
            };
            if items.len() < 4 {
                return Err(crate::error::type_error(format!(
                    "function takes at least 4 arguments ({} given)",
                    items.len()
                )));
            }
            if items.len() > 6 {
                return Err(crate::error::type_error(format!(
                    "function takes at most 6 arguments ({} given)",
                    items.len()
                )));
            }
            if items.len() == 5 {
                return Err(crate::error::type_error(
                    "end_offset must be provided when end_lineno is provided".to_owned(),
                ));
            }
            let pick = |i: usize| items.get(i).cloned().unwrap_or(Object::None);
            inst_rc.slot_set("filename", pick(0));
            inst_rc.slot_set("lineno", pick(1));
            inst_rc.slot_set("offset", pick(2));
            inst_rc.slot_set("text", pick(3));
            if items.len() == 6 {
                inst_rc.slot_set("end_lineno", pick(4));
                inst_rc.slot_set("end_offset", pick(5));
            }
        }
        Ok(Object::None)
    }

    fn syntaxerror_str(args: &[Object]) -> Result<Object, RuntimeError> {
        let inst = args
            .first()
            .ok_or_else(|| crate::error::type_error("expected exception instance".to_owned()))?;
        let Object::Instance(inst_rc) = inst else {
            return Ok(Object::from_static(""));
        };
        let get = |name: &'static str| exc_attr(inst_rc, name).unwrap_or(Object::None);
        let msg = get("msg");
        let filename = get("filename");
        let lineno = get("lineno");
        // CPython renders the message via `str(self.msg)` — for instance
        // messages (e.g. `ParseError(ExpatError(...))` in ElementTree) that
        // means the instance's own `__str__`, not its repr.
        let msg_str = match &msg {
            Object::None => "None".to_owned(),
            inst @ Object::Instance(_) => {
                match crate::vm_singletons::current_interpreter_ptr() {
                    // SAFETY: published by an enclosing VM frame on this thread.
                    Some(ptr) => unsafe { &mut *ptr }.str_object(inst)?,
                    None => inst.to_str(),
                }
            }
            other => other.to_str(),
        };
        let have_filename = matches!(filename, Object::Str(_));
        let lineno_val = match &lineno {
            Object::Int(n) => Some(*n),
            Object::Bool(b) => Some(i64::from(*b)),
            _ => None,
        };
        let result = match (have_filename, lineno_val) {
            (true, Some(n)) => {
                format!("{msg_str} ({}, line {n})", syntax_basename(&filename))
            }
            (true, None) => format!("{msg_str} ({})", syntax_basename(&filename)),
            (false, Some(n)) => format!("{msg_str} (line {n})"),
            (false, None) => msg_str,
        };
        Ok(Object::from_str(result))
    }

    let mut dict = syntax_error.dict.borrow_mut();
    set(
        &mut dict,
        "__init__",
        Object::Builtin(Rc::new(BuiltinFn {
            name: "__init__",
            binds_instance: true,
            call: Box::new(syntaxerror_init),
            call_kw: None,
        })),
    );
    set(
        &mut dict,
        "__str__",
        Object::Builtin(Rc::new(BuiltinFn {
            name: "__str__",
            binds_instance: true,
            call: Box::new(syntaxerror_str),
            call_kw: None,
        })),
    );
}

/// Last path component of a `SyntaxError.filename`, mirroring CPython's
/// `my_basename` (split on `/` — and `\\` on the same footing so Windows
/// paths render the same). Non-string filenames yield an empty string.
fn syntax_basename(filename: &Object) -> String {
    let Object::Str(s) = filename else {
        return String::new();
    };
    let s = s.as_ref();
    let cut = s.rfind(['/', '\\']).map_or(0, |i| i + 1);
    s[cut..].to_owned()
}

/// CPython's `BaseException.__init__(self, *args)` — stores `args`
/// on the instance so every subclass — built-in or user-defined
/// — exposes `e.args` automatically. Module-scope so the docs surface
/// pass (RFC 0056 WS4) can mint per-exception-type mirrors of it.
fn exc_init(args: &[Object]) -> Result<Object, RuntimeError> {
    {
        let inst = args
            .first()
            .ok_or_else(|| crate::error::type_error("expected exception instance".to_owned()))?;
        if let Object::Instance(inst_rc) = inst {
            let rest = if args.len() > 1 {
                args[1..].to_vec()
            } else {
                Vec::new()
            };
            // PEP 380: `StopIteration.value` mirrors args[0] for the
            // built-in class and any user subclass (CPython stores it
            // in `StopIteration.__init__`).
            if is_subclass_by_name(&inst_rc.cls(), "StopIteration") {
                inst_rc.slot_set("value", rest.first().cloned().unwrap_or(Object::None));
            }
            inst_rc.slot_set("args", Object::new_tuple(rest));
        }
        Ok(Object::None)
    }
}

/// CPython's `BaseException.__str__` — `str(args[0])` for a single
/// argument, tuple repr for several. Module-scope for the docs surface
/// pass (see [`exc_init`]).
fn exc_str(args: &[Object]) -> Result<Object, RuntimeError> {
    {
        let inst = args
            .first()
            .ok_or_else(|| crate::error::type_error("expected exception instance".to_owned()))?;
        if let Object::Instance(inst_rc) = inst {
            // CPython's ``KeyError.__str__`` overrides the default to
            // render the key via ``repr`` — so ``str(KeyError('x'))``
            // is ``"'x'"`` not ``'x'``. We special-case KeyError here
            // because the runtime constructs them from Rust and we
            // can't easily install a per-subclass ``__str__``.
            let is_key_error = is_subclass_by_name(&inst_rc.cls(), "KeyError");
            if let Some(Object::Tuple(items)) = exc_attr(inst_rc, "args") {
                return Ok(match items.as_ref() {
                    [] => Object::from_static(""),
                    [single] => {
                        if is_key_error {
                            Object::from_str(single.repr())
                        } else if matches!(single, Object::Str(_) | Object::WStr(_)) {
                            // `str(args[0])` of a str IS args[0] — round-
                            // tripping through a Rust String would mangle
                            // lone surrogates (test_getargs's surrogate
                            // keyword message).
                            single.clone()
                        } else if matches!(single, Object::Instance(_) | Object::Foreign(_)) {
                            // A nested exception, other instance, or a
                            // *foreign* extension object (e.g. a numpy
                            // scalar) needs its own `__str__`/`tp_str`
                            // dispatched: CPython's `BaseException.__str__`
                            // is `str(args[0])`, not `repr`. Without this a
                            // `OutOfBoundsTimedelta(np.timedelta64(...))`
                            // stringified to its repr
                            // (`np.timedelta64(...,'h')`) instead of
                            // `"... hours"`.
                            Object::from_str(
                                crate::builtins::str_reentrant(single)
                                    .unwrap_or_else(|| single.to_str()),
                            )
                        } else {
                            Object::from_str(single.to_str())
                        }
                    }
                    _ => Object::from_str(format!(
                        "({})",
                        items
                            .iter()
                            .map(|x| x.repr())
                            .collect::<Vec<_>>()
                            .join(", ")
                    )),
                });
            }
        }
        Ok(Object::from_static(""))
    }
}

/// CPython's `BaseException.__repr__` — `ClsName(arg_reprs…)`.
/// Module-scope for the docs surface pass (see [`exc_init`]).
fn exc_repr(args: &[Object]) -> Result<Object, RuntimeError> {
    {
        let inst = args
            .first()
            .ok_or_else(|| crate::error::type_error("expected exception instance".to_owned()))?;
        if let Object::Instance(inst_rc) = inst {
            let cls = inst_rc.cls().name.clone();
            let args_repr = if let Some(Object::Tuple(items)) = exc_attr(inst_rc, "args") {
                items
                    .iter()
                    .map(|x| x.repr())
                    .collect::<Vec<_>>()
                    .join(", ")
            } else {
                String::new()
            };
            return Ok(Object::from_str(format!("{cls}({args_repr})")));
        }
        Ok(Object::from_static(""))
    }
}

fn install_exception_str_repr(base_exception: &Rc<TypeObject>) {
    use crate::object::BuiltinFn;
    // PEP 678: ``e.add_note("...")`` appends a string note to
    // ``__notes__``. The list is created on the first call and
    // travels with the instance through ``raise`` (we store it on
    // the instance ``__dict__``).
    fn exc_add_note(args: &[Object]) -> Result<Object, RuntimeError> {
        let inst = args
            .first()
            .ok_or_else(|| crate::error::type_error("expected exception instance".to_owned()))?;
        let note = args.get(1).ok_or_else(|| {
            crate::error::type_error("add_note() expects one argument".to_owned())
        })?;
        if !matches!(note, Object::Str(_)) {
            return Err(crate::error::type_error(format!(
                "note must be a str, not '{}'",
                note.type_name_owned()
            )));
        }
        if let Object::Instance(inst_rc) = inst {
            let key = DictKey(Object::from_static("__notes__"));
            let mut dict = inst_rc.dict.borrow_mut();
            match dict.get(&key) {
                // Append in place so `e.__notes__` keeps its identity.
                Some(Object::List(l)) => l.borrow_mut().push(note.clone()),
                Some(other) => {
                    let msg = format!(
                        "Cannot add note: __notes__ is not a list, it is '{}' instead",
                        other.type_name_owned()
                    );
                    return Err(crate::error::type_error(msg));
                }
                None => {
                    dict.insert(
                        key,
                        Object::List(Rc::new(crate::sync::GilCell::new(vec![note.clone()]))),
                    );
                }
            }
        }
        Ok(Object::None)
    }
    // `e.with_traceback(tb)` sets `__traceback__` and returns `self`, so
    // `raise e.with_traceback(tb)` and chained-exception helpers work.
    fn exc_with_traceback(args: &[Object]) -> Result<Object, RuntimeError> {
        let inst = args
            .first()
            .ok_or_else(|| crate::error::type_error("expected exception instance".to_owned()))?;
        let tb = args.get(1).cloned().unwrap_or(Object::None);
        if let Object::Instance(inst_rc) = inst {
            inst_rc.slot_set("__traceback__", tb);
        }
        Ok(inst.clone())
    }
    // `BaseException.__setstate__(state)` — pickle protocol support:
    // apply each dict entry as an attribute (CPython
    // `BaseException_setstate`); `None` is a no-op; anything else is
    // a TypeError.
    fn exc_setstate(args: &[Object]) -> Result<Object, RuntimeError> {
        let inst = args
            .first()
            .ok_or_else(|| crate::error::type_error("expected exception instance".to_owned()))?;
        let state = args.get(1).cloned().unwrap_or(Object::None);
        if matches!(state, Object::None) {
            return Ok(Object::None);
        }
        let Object::Dict(d) = &state else {
            return Err(crate::error::type_error(
                "state is not a dictionary".to_owned(),
            ));
        };
        if let Object::Instance(inst_rc) = inst {
            let entries: Vec<(Object, Object)> = d
                .borrow()
                .iter()
                .map(|(k, v)| (k.0.clone(), v.clone()))
                .collect();
            let cls = inst_rc.cls();
            for (k, v) in entries {
                // CPython routes each entry through `PyObject_SetAttr`,
                // which accepts any `str` *subclass* as the name
                // (test_baseexception.test_setstate_refcount_no_crash).
                let is_str = match &k {
                    Object::Str(_) | Object::WStr(_) => true,
                    Object::Instance(i) => {
                        matches!(i.native.get(), Some(Object::Str(_) | Object::WStr(_)))
                    }
                    _ => false,
                };
                if !is_str {
                    return Err(crate::error::type_error(format!(
                        "attribute name must be string, not '{}'",
                        k.type_name_owned()
                    )));
                }
                // Normalize a subclass key to its plain-str value so the
                // regular attribute lookup (keyed on `Object::Str`) finds it.
                let key = match &k {
                    Object::Str(_) | Object::WStr(_) => k,
                    other => Object::from_str(other.to_str()),
                };
                // Route through the same storage a setattr would use:
                // names the class exposes as slot descriptors land in
                // the slot side table, everything else in `__dict__`.
                let name = key.to_str();
                if matches!(cls.lookup(&name), Some(Object::SlotDescriptor(_))) {
                    inst_rc.slot_set(&name, v);
                } else {
                    inst_rc.dict.borrow_mut().insert(DictKey(key), v);
                }
            }
        }
        Ok(Object::None)
    }
    // `BaseException.__reduce__` — `(cls, self.args)` plus the instance
    // dict (minus the runtime exception metadata we model as dict
    // entries but CPython keeps in C slots) when it is non-empty.
    // OSError appends `filename`/`filename2` to the reconstruction args
    // (CPython `OSError_reduce`).
    fn exc_reduce(args: &[Object]) -> Result<Object, RuntimeError> {
        let inst_obj = args
            .first()
            .ok_or_else(|| crate::error::type_error("expected exception instance".to_owned()))?;
        let Object::Instance(inst) = inst_obj else {
            return Err(crate::error::type_error(
                "__reduce__ requires an exception instance".to_owned(),
            ));
        };
        let cls = inst.cls();
        let get = |name: &'static str| exc_attr(inst, name);
        let mut ctor_args: Vec<Object> = match get("args") {
            Some(Object::Tuple(t)) => t.to_vec(),
            _ => Vec::new(),
        };
        if is_subclass_by_name(&cls, "OSError") && ctor_args.len() == 2 {
            let filename = get("filename").filter(|v| !matches!(v, Object::None));
            let filename2 = get("filename2").filter(|v| !matches!(v, Object::None));
            if let Some(f) = filename {
                ctor_args.push(f);
                if let Some(f2) = filename2 {
                    ctor_args.push(Object::None);
                    ctor_args.push(f2);
                }
            }
        }
        // Exception state CPython keeps out of `__dict__` (C slots /
        // interpreter metadata); everything else round-trips.
        const SKIP: &[&str] = &[
            "args",
            "__traceback__",
            "__context__",
            "__cause__",
            "__suppress_context__",
        ];
        // `message` is WeavePy's internal mirror of `str(args[0])`; while
        // it matches, it's the auto-derived value and stays out of the
        // state (CPython has no such attribute at all). Once user code
        // diverges it (configparser's `ParsingError.append` does
        // `self.message += …` — test_configparser's pickling cases), it
        // must round-trip like any other instance attribute.
        let dict = inst.dict.borrow();
        let message_is_derived = match (get("message"), ctor_args.first()) {
            (Some(m), Some(a)) => {
                m.is_same(a) || matches!((&m, a), (Object::Str(x), Object::Str(y)) if x == y)
            }
            (Some(_), None) => false,
            (None, _) => true,
        };
        // GH-103352: AttributeError deliberately drops `obj` from its
        // pickled state (it may be huge or unpicklable).
        let skip_obj = is_subclass_by_name(&cls, "AttributeError");
        let mut state = crate::object::DictData::default();
        for (k, v) in dict.iter() {
            if let Object::Str(s) = &k.0 {
                if SKIP.contains(&s.as_ref())
                    || (skip_obj && s.as_ref() == "obj")
                    || (message_is_derived && s.as_ref() == "message")
                {
                    continue;
                }
            }
            state.insert(k.clone(), v.clone());
        }
        drop(dict);
        // CPython `ImportError_getstate`: the `name`/`path`/`name_from`
        // slots ride in the pickle state when populated, so
        // `pickle.loads(pickle.dumps(ImportError('m', name='n')))`
        // keeps its `.name` (test_exceptions ImportErrorTests
        // test_copy_pickle).
        if is_subclass_by_name(&cls, "ImportError") {
            for key in ["name", "path", "name_from"] {
                if let Some(v) = get(key).filter(|v| !matches!(v, Object::None)) {
                    state.insert(DictKey(Object::from_static(key)), v);
                }
            }
        }
        // CPython `AttributeError_getstate` (gh-103352): `name` rides in
        // the state; `obj` is deliberately dropped (often unpicklable).
        if skip_obj {
            if let Some(v) = get("name").filter(|v| !matches!(v, Object::None)) {
                state.insert(DictKey(Object::from_static("name")), v);
            }
        }
        let cls_obj = Object::Type(cls);
        let args_obj = Object::new_tuple(ctor_args);
        Ok(if state.is_empty() {
            Object::new_tuple(vec![cls_obj, args_obj])
        } else {
            Object::new_tuple(vec![
                cls_obj,
                args_obj,
                Object::Dict(Rc::new(RefCell::new(state))),
            ])
        })
    }
    let mut dict = base_exception.dict.borrow_mut();
    dict.insert(
        DictKey(Object::from_static("__init__")),
        Object::Builtin(Rc::new(BuiltinFn {
            name: "__init__",
            binds_instance: true,
            call: Box::new(exc_init),
            call_kw: None,
        })),
    );
    dict.insert(
        DictKey(Object::from_static("__setstate__")),
        Object::Builtin(Rc::new(BuiltinFn {
            name: "__setstate__",
            binds_instance: true,
            call: Box::new(exc_setstate),
            call_kw: None,
        })),
    );
    dict.insert(
        DictKey(Object::from_static("__reduce__")),
        Object::Builtin(Rc::new(BuiltinFn {
            name: "__reduce__",
            binds_instance: true,
            call: Box::new(exc_reduce),
            call_kw: None,
        })),
    );
    dict.insert(
        DictKey(Object::from_static("__str__")),
        Object::Builtin(Rc::new(BuiltinFn {
            name: "__str__",
            binds_instance: true,
            call: Box::new(exc_str),
            call_kw: None,
        })),
    );
    dict.insert(
        DictKey(Object::from_static("__repr__")),
        Object::Builtin(Rc::new(BuiltinFn {
            name: "__repr__",
            binds_instance: true,
            call: Box::new(exc_repr),
            call_kw: None,
        })),
    );
    dict.insert(
        DictKey(Object::from_static("add_note")),
        Object::Builtin(Rc::new(BuiltinFn {
            name: "add_note",
            binds_instance: true,
            call: Box::new(exc_add_note),
            call_kw: None,
        })),
    );
    dict.insert(
        DictKey(Object::from_static("with_traceback")),
        Object::Builtin(Rc::new(BuiltinFn {
            name: "with_traceback",
            binds_instance: true,
            call: Box::new(exc_with_traceback),
            call_kw: None,
        })),
    );
}

pub fn make_exception_with_class(class: Rc<TypeObject>, message: impl Into<String>) -> Object {
    use crate::types::PyInstance;
    let is_syntax = is_subclass_by_name(&class, "SyntaxError");
    let is_stop_iteration = is_subclass_by_name(&class, "StopIteration");
    let is_import = is_subclass_by_name(&class, "ImportError");
    let inst = PyInstance::new(class);
    let msg = Object::from_str(message);
    // A messageless raise (`StopIteration()`, `GeneratorExit()`, …)
    // has *empty* args in CPython, not `("",)`.
    let args = if msg.to_str().is_empty() {
        Object::new_tuple(Vec::new())
    } else {
        Object::new_tuple(vec![msg.clone()])
    };
    // PEP 380: `StopIteration.value` is always present (CPython sets it
    // in `StopIteration.__init__`, defaulting to None). A Rust-raised
    // bare `StopIteration` must answer `.value` too — asyncio's
    // `Task.__step` reads `exc.value` on every coroutine return, and a
    // missing attribute leaves the task wedged (gh: shutdown_asyncgens).
    if is_stop_iteration {
        let value = if msg.to_str().is_empty() {
            Object::None
        } else {
            msg.clone()
        };
        inst.slot_set("value", value);
    }
    inst.slot_set("args", args);
    inst.slot_set("message", msg.clone());
    // The BaseException pseudo-slots (`__context__`/`__cause__`/
    // `__suppress_context__`/`__traceback__`) and OSError's named fields
    // (`errno`/`strerror`/…) are *not* seeded per-instance: their class
    // slot descriptors answer the CPython getset defaults while unset,
    // and a subclass's own class attribute (`class Err(OSError): errno =
    // EINVAL`, raised bare via `raise Err`) stays visible.
    if is_import {
        // ImportError/ModuleNotFoundError expose `msg` (the message
        // string). CPython always defines the slot; a Rust-raised
        // ImportError must answer `.msg` so consumers (e.g. numpy's
        // `_core/__init__` error handling, which reads `exc.msg`) don't
        // AttributeError. `name`/`path`/`name_from` are intentionally
        // *not* pre-set here so the import machinery's
        // `set_exception_attr` (which skips already-present slots) can
        // still populate the real module name.
        inst.slot_set("msg", msg.clone());
    }
    if is_syntax {
        // SyntaxError gets `msg` from `args[0]`; the location payload
        // reads `None` off the class descriptors until
        // `error::syntax_error_located` fills real values.
        inst.slot_set("msg", msg);
    }
    Object::Instance(Rc::new(inst))
}

/// PEP 654 — `BaseExceptionGroup.__init__(self, msg, exceptions)`
/// + the `message`, `exceptions`, `split`, `subgroup`, `derive`
///   methods. `ExceptionGroup` inherits the same `__init__` through
///   the MRO.
#[allow(clippy::doc_lazy_continuation)]
fn install_exception_group_init(base: &Rc<TypeObject>) {
    use crate::object::BuiltinFn;
    fn eg_init(args: &[Object]) -> Result<Object, RuntimeError> {
        // CPython `BaseExceptionGroup_init` → `BaseException_init`:
        // `__init__` only (re)binds `args` to the positional arguments;
        // `message`/`exceptions` were normalized by `__new__`. Subclass
        // `__init__`s with extra parameters (`EG(msg, excs, code)`)
        // therefore work — `args` simply keeps all three.
        let inst = args
            .first()
            .ok_or_else(|| crate::error::type_error("expected exception instance"))?;
        if let Object::Instance(inst_rc) = inst {
            inst_rc.slot_set("args", Object::new_tuple(args[1..].to_vec()));
        }
        Ok(Object::None)
    }
    fn eg_str(args: &[Object]) -> Result<Object, RuntimeError> {
        let inst = args
            .first()
            .ok_or_else(|| crate::error::type_error("expected exception instance"))?;
        if let Object::Instance(inst_rc) = inst {
            let message = exc_attr(inst_rc, "message").unwrap_or(Object::from_static(""));
            let n = exc_attr(inst_rc, "exceptions")
                .and_then(|e| match e {
                    Object::Tuple(t) => Some(t.len()),
                    _ => None,
                })
                .unwrap_or(0);
            return Ok(Object::from_str(format!(
                "{} ({} sub-exception{})",
                message.to_str(),
                n,
                if n > 1 { "s" } else { "" }
            )));
        }
        Ok(Object::from_static(""))
    }
    fn eg_repr(args: &[Object]) -> Result<Object, RuntimeError> {
        // CPython `BaseExceptionGroup_repr`: renders from the frozen
        // `exceptions` tuple (or the repr string saved at construction
        // for custom sequences), *not* from the possibly-mutated
        // `args[1]` — but keeps `args[1]`'s list/tuple brackets.
        let Some(Object::Instance(inst)) = args.first() else {
            return Err(crate::error::type_error("expected exception instance"));
        };
        let name = inst.cls().name.clone();
        let msg = exc_attr(inst, "message").unwrap_or(Object::from_static(""));
        let interp = eg_interp()?;
        let excs_str = if let Some(Object::Str(s)) = inst.slot_get("__excs_str__") {
            s.to_string()
        } else {
            let excs: Vec<Object> = match exc_attr(inst, "exceptions") {
                Some(Object::Tuple(t)) => t.to_vec(),
                _ => Vec::new(),
            };
            let args_second_is_list = matches!(
                exc_attr(inst, "args"),
                Some(Object::Tuple(t)) if t.len() == 2 && matches!(t[1], Object::List(_))
            );
            if args_second_is_list {
                interp.repr_object(&Object::new_list(excs))?
            } else {
                interp.repr_object(&Object::new_tuple(excs))?
            }
        };
        let msg_repr = interp.repr_object(&msg)?;
        Ok(Object::from_str(format!("{name}({msg_repr}, {excs_str})")))
    }
    fn eg_derive(args: &[Object]) -> Result<Object, RuntimeError> {
        // Default `derive(self, excs)` — CPython's calls the *plain*
        // `BaseExceptionGroup(self.message, excs)` constructor (not
        // `type(self)`), which `__new__`'s PEP 654 magic lowers to
        // `ExceptionGroup` when every leaf is an `Exception`.
        // Subclasses that want to survive `split`/`subgroup` must
        // override `derive`.
        let inst = args
            .first()
            .ok_or_else(|| crate::error::type_error("expected exception instance"))?;
        let excs = args
            .get(1)
            .cloned()
            .unwrap_or(Object::new_tuple(Vec::new()));
        let Object::Instance(inst_rc) = inst else {
            return Ok(Object::None);
        };
        let msg = exc_attr(inst_rc, "message").unwrap_or(Object::from_static(""));
        eg_new(&[
            Object::Type(builtin_types().base_exception_group.clone()),
            msg,
            excs,
        ])
    }
    fn eg_split(args: &[Object]) -> Result<Object, RuntimeError> {
        let (m, r) = eg_split_impl(args, true)?;
        Ok(Object::new_tuple(vec![m, r]))
    }
    fn eg_subgroup(args: &[Object]) -> Result<Object, RuntimeError> {
        let (m, _) = eg_split_impl(args, false)?;
        Ok(m)
    }
    fn eg_new(args: &[Object]) -> Result<Object, RuntimeError> {
        // `BaseExceptionGroup.__new__(cls, message, exceptions)` —
        // reached from user subclasses' `super().__new__(...)` and
        // from the generic instantiation path for EG subclasses.
        let Some(Object::Type(cls)) = args.first() else {
            return Err(crate::error::type_error(
                "BaseExceptionGroup.__new__ requires a class argument",
            ));
        };
        exception_group_new(cls, &args[1..])
    }
    let mut dict = base.dict.borrow_mut();
    dict.insert(
        DictKey(Object::from_static("__init__")),
        Object::Builtin(Rc::new(BuiltinFn {
            name: "__init__",
            binds_instance: true,
            call: Box::new(eg_init),
            call_kw: None,
        })),
    );
    dict.insert(
        DictKey(Object::from_static("__str__")),
        Object::Builtin(Rc::new(BuiltinFn {
            name: "__str__",
            binds_instance: true,
            call: Box::new(eg_str),
            call_kw: None,
        })),
    );
    dict.insert(
        DictKey(Object::from_static("__repr__")),
        Object::Builtin(Rc::new(BuiltinFn {
            name: "__repr__",
            binds_instance: true,
            call: Box::new(eg_repr),
            call_kw: None,
        })),
    );
    dict.insert(
        DictKey(Object::from_static("derive")),
        Object::Builtin(Rc::new(BuiltinFn {
            name: "derive",
            binds_instance: true,
            call: Box::new(eg_derive),
            call_kw: None,
        })),
    );
    dict.insert(
        DictKey(Object::from_static("split")),
        Object::Builtin(Rc::new(BuiltinFn {
            name: "split",
            binds_instance: true,
            call: Box::new(eg_split),
            call_kw: None,
        })),
    );
    dict.insert(
        DictKey(Object::from_static("subgroup")),
        Object::Builtin(Rc::new(BuiltinFn {
            name: "subgroup",
            binds_instance: true,
            call: Box::new(eg_subgroup),
            call_kw: None,
        })),
    );
    // A *plain* Builtin (not StaticMethod-wrapped like the default
    // allocator) so the instantiation path treats it as a real user
    // `__new__` and EG subclasses' `super().__new__(cls, msg, excs)`
    // reaches PEP 654 construction instead of `object.__new__`.
    dict.insert(
        DictKey(Object::from_static("__new__")),
        Object::Builtin(Rc::new(BuiltinFn {
            name: "__new__",
            binds_instance: true,
            call: Box::new(eg_new),
            call_kw: None,
        })),
    );
}

/// PEP 654 class-selection rule for a derived/constructed group: a
/// plain `BaseExceptionGroup` whose leaves are all `Exception`s
/// materialises as `ExceptionGroup`.
fn exception_group_class_for(items: &[Object]) -> Rc<TypeObject> {
    let bt = builtin_types();
    let all_exceptions = items.iter().all(|e| instance_is_subclass(e, &bt.exception));
    if all_exceptions {
        bt.exception_group.clone()
    } else {
        bt.base_exception_group.clone()
    }
}

/// CPython `BaseExceptionGroup_new`'s class-selection block: a plain
/// `BaseExceptionGroup` of all-`Exception` leaves lowers to
/// `ExceptionGroup`; nesting a bare `BaseException` inside
/// `ExceptionGroup` — or any user subclass that derives from
/// `Exception` — is a `TypeError`.
fn resolve_eg_class(
    cls: Rc<TypeObject>,
    nested_base_exceptions: bool,
) -> Result<Rc<TypeObject>, RuntimeError> {
    let bt = builtin_types();
    if Rc::ptr_eq(&cls, &bt.exception_group) {
        if nested_base_exceptions {
            return Err(crate::error::type_error(
                "Cannot nest BaseExceptions in an ExceptionGroup",
            ));
        }
    } else if Rc::ptr_eq(&cls, &bt.base_exception_group) {
        if !nested_base_exceptions {
            return Ok(bt.exception_group.clone());
        }
    } else if nested_base_exceptions && cls.is_subclass_of(&bt.exception) {
        return Err(crate::error::type_error(format!(
            "Cannot nest BaseExceptions in '{}'",
            cls.name
        )));
    }
    Ok(cls)
}

/// CPython `BaseExceptionGroup_new`, step for step: parse
/// `(message: str, exceptions: sequence)`, freeze a repr of custom
/// sequences (for `__repr__` accuracy after mutation), convert to a
/// tuple, validate the items, apply the PEP 654 class-selection rules,
/// and populate `args`/`message`/`exceptions`.
///
/// `ctor_args` are the constructor arguments *without* the class.
pub(crate) fn exception_group_new(
    cls: &Rc<TypeObject>,
    ctor_args: &[Object],
) -> Result<Object, RuntimeError> {
    let bt = builtin_types();
    if ctor_args.len() != 2 {
        return Err(crate::error::type_error(format!(
            "BaseExceptionGroup.__new__() takes exactly 2 arguments ({} given)",
            ctor_args.len()
        )));
    }
    let msg = ctor_args[0].clone();
    if !matches!(msg, Object::Str(_)) && !instance_is_subclass(&msg, &bt.str_) {
        return Err(crate::error::type_error(format!(
            "BaseExceptionGroup.__new__() argument 1 must be str, not {}",
            msg.type_name()
        )));
    }
    let excs = ctor_args[1].clone();
    // `PySequence_Check`: lists, tuples, and instances whose class
    // exposes `__getitem__` (sets/dicts/None are not sequences).
    let is_sequence = match &excs {
        Object::Tuple(_) | Object::List(_) | Object::Str(_) | Object::Bytes(_) => true,
        Object::ByteArray(_) => true,
        Object::Instance(i) => {
            i.cls().lookup("__getitem__").is_some()
                && !i.cls().is_subclass_of(&bt.dict_)
                && !i.cls().is_subclass_of(&bt.set_)
                && !i.cls().is_subclass_of(&bt.frozenset_)
        }
        _ => false,
    };
    if !is_sequence {
        return Err(crate::error::type_error(
            "second argument (exceptions) must be a sequence",
        ));
    }
    // Freeze a repr of custom (non-list/tuple) sequences now, so
    // `repr(eg)` stays accurate after the caller mutates them.
    let excs_str = if matches!(excs, Object::List(_) | Object::Tuple(_)) {
        None
    } else {
        Some(Object::from_str(eg_interp()?.repr_object(&excs)?))
    };
    let items: Vec<Object> = match &excs {
        Object::Tuple(t) => t.to_vec(),
        Object::List(l) => l.borrow().clone(),
        _ => {
            let interp = eg_interp()?;
            let globals = interp.builtins_dict();
            interp.collect_iterable(&excs, &globals)?
        }
    };
    if items.is_empty() {
        return Err(crate::error::value_error(
            "second argument (exceptions) must be a non-empty sequence".to_owned(),
        ));
    }
    let mut nested_base_exceptions = false;
    for (i, item) in items.iter().enumerate() {
        if !instance_is_subclass(item, &bt.base_exception) {
            return Err(crate::error::value_error(format!(
                "Item {i} of second argument (exceptions) is not an exception"
            )));
        }
        if !instance_is_subclass(item, &bt.exception) {
            nested_base_exceptions = true;
        }
    }
    let cls = resolve_eg_class(cls.clone(), nested_base_exceptions)?;
    let inst = make_exception_with_class(cls, "");
    if let Object::Instance(inst_rc) = &inst {
        // `args` keeps the *original* second argument (mutations show
        // through `eg.args`); `.exceptions` is the frozen tuple copy.
        inst_rc.slot_set("args", Object::new_tuple(vec![msg.clone(), excs]));
        inst_rc.slot_set("message", msg);
        inst_rc.slot_set("exceptions", Object::new_tuple(items));
        if let Some(s) = excs_str {
            inst_rc.slot_set("__excs_str__", s);
        }
    }
    // An exception group always anchors other exception instances, so
    // it can participate in reference cycles — GC-track it like
    // `build_exception_instance` does for enriched exceptions.
    crate::gc_trace::track(inst.clone());
    Ok(inst)
}

/// Fetch the interpreter published by the enclosing VM frame —
/// exception-group construction and split need re-entry for `repr`,
/// sequence iteration, predicate calls, and truthiness.
fn eg_interp() -> Result<&'static mut crate::Interpreter, RuntimeError> {
    let ptr = crate::vm_singletons::current_interpreter_ptr()
        .ok_or_else(|| crate::error::runtime_error("no running interpreter"))?;
    // SAFETY: the pointer was published by an enclosing VM frame still
    // live on this thread; the GIL keeps the access exclusive.
    Ok(unsafe { &mut *ptr })
}

/// CPython `get_matcher_type`: a callable that is not a class is a
/// predicate matcher; an exception class or tuple of exception classes
/// matches by type; anything else is a `TypeError`. Returns `true`
/// for predicate matchers.
fn eg_matcher_is_predicate(pred: &Object) -> Result<bool, RuntimeError> {
    let bt = builtin_types();
    let is_exc_type =
        |o: &Object| matches!(o, Object::Type(t) if t.is_subclass_of(&bt.base_exception));
    let ok = match pred {
        Object::Function(_)
        | Object::Builtin(_)
        | Object::BoundMethod(_)
        | Object::StaticMethod(_) => return Ok(true),
        Object::Instance(i) if i.cls().lookup("__call__").is_some() => return Ok(true),
        Object::Type(_) => is_exc_type(pred),
        Object::Tuple(items) => items.iter().all(is_exc_type),
        _ => false,
    };
    if ok {
        return Ok(false);
    }
    Err(crate::error::type_error(
        "expected an exception type, a tuple of exception types, or a callable (other than a class)",
    ))
}

/// `BaseExceptionGroup.split(matcher)` / `.subgroup(matcher)` — the
/// method entry points: validate the matcher, then run the recursive
/// split. `construct_rest` is `false` for `subgroup`, which never
/// builds (or `derive`s) the non-matching parts.
fn eg_split_impl(args: &[Object], construct_rest: bool) -> Result<(Object, Object), RuntimeError> {
    let inst = args
        .first()
        .ok_or_else(|| crate::error::type_error("expected exception instance"))?;
    let pred = args
        .get(1)
        .cloned()
        .ok_or_else(|| crate::error::type_error("split requires a matcher argument"))?;
    let by_predicate = eg_matcher_is_predicate(&pred)?;
    let matcher = |exc: &Object| -> Result<bool, RuntimeError> {
        if by_predicate {
            let r = crate::builtins::reentrant_call(&pred, std::slice::from_ref(exc))?;
            let interp = eg_interp()?;
            let globals = interp.builtins_dict();
            interp.obj_truthy(&r, &globals)
        } else {
            Ok(exception_matches_type(exc, &pred))
        }
    };
    split_eg_recursive(inst, &matcher, construct_rest, 1)
}

/// `True` if `class` overrides `split` below the builtin
/// `BaseExceptionGroup` implementation — the VM's `CheckEGMatch` must
/// then dispatch the override (gh-128049) instead of the native split.
pub fn overrides_eg_split(class: &Rc<TypeObject>) -> bool {
    overrides_eg_method(class, "split")
}

fn overrides_eg_method(class: &Rc<TypeObject>, name: &'static str) -> bool {
    let bt = builtin_types();
    for t in class.mro.borrow().iter() {
        if Rc::ptr_eq(t, &bt.base_exception_group) {
            return false;
        }
        if t.dict
            .borrow()
            .contains_key(&DictKey(Object::from_static(name)))
        {
            return true;
        }
    }
    false
}

/// Split an exception group instance against a type predicate. Used
/// by the VM's `CheckEGMatch` opcode and exposed via
/// `BaseExceptionGroup.split(typ)`.
///
/// Returns `(matched, rest)` where:
/// - `matched` is `None` if no contained exception matches, otherwise
///   a new exception group containing the matches.
/// - `rest` is `None` if every contained exception matches, otherwise
///   a new group with the non-matching ones.
///
/// New groups are produced via `derive` semantics: the *default*
/// derive returns a plain group (auto-lowered per PEP 654); a
/// user-overridden `derive` is dispatched through the interpreter.
/// `__cause__`, `__context__`, `__traceback__` and `__notes__` are
/// copied onto the derived parts, mirroring CPython's split.
pub fn split_exception_group(
    group: &Object,
    type_pred: &Object,
) -> Result<(Object, Object), RuntimeError> {
    split_exception_group_by(group, &|exc| Ok(exception_matches_type(exc, type_pred)))
}

/// Predicate-based core of [`split_exception_group`]. Also used for
/// CPython's `exception_group_projection` (leaf-identity matching) in
/// the `except*` re-raise machinery.
pub fn split_exception_group_by(
    group: &Object,
    leaf_matches: &dyn Fn(&Object) -> Result<bool, RuntimeError>,
) -> Result<(Object, Object), RuntimeError> {
    split_eg_recursive(group, leaf_matches, true, 1)
}

/// CPython `exceptiongroup_split_recursive`: a matching exception
/// (leaf *or whole group*) passes through by identity; a non-matching
/// leaf lands in `rest`; a non-matching group recurses and rebuilds
/// the matching/non-matching parts via [`eg_subset`]. Depth is guarded
/// like `_Py_EnterRecursiveCall` (`RecursionError` past the C limit).
fn split_eg_recursive(
    exc: &Object,
    matches_pred: &dyn Fn(&Object) -> Result<bool, RuntimeError>,
    construct_rest: bool,
    depth: usize,
) -> Result<(Object, Object), RuntimeError> {
    if depth > crate::recursion::C_RECURSION_LIMIT {
        return Err(crate::error::recursion_error(
            "maximum recursion depth exceeded in exceptiongroup split",
        ));
    }
    if matches_pred(exc)? {
        // Full match — passes through by identity.
        return Ok((exc.clone(), Object::None));
    }
    let group_inst = match exc {
        Object::Instance(i) if is_subclass_by_name(&i.cls(), "BaseExceptionGroup") => i.clone(),
        _ => {
            // Leaf exception, no match.
            let rest = if construct_rest {
                exc.clone()
            } else {
                Object::None
            };
            return Ok((Object::None, rest));
        }
    };
    let excs: Vec<Object> = match exc_attr(&group_inst, "exceptions") {
        Some(Object::Tuple(t)) => t.to_vec(),
        _ => Vec::new(),
    };
    let mut matched = Vec::new();
    let mut rest = Vec::new();
    for e in excs {
        let (m, r) = split_eg_recursive(&e, matches_pred, construct_rest, depth + 1)?;
        if !matches!(m, Object::None) {
            matched.push(m);
        }
        if !matches!(r, Object::None) {
            rest.push(r);
        }
    }
    let match_part = eg_subset(exc, &group_inst, matched)?;
    let rest_part = if construct_rest {
        eg_subset(exc, &group_inst, rest)?
    } else {
        Object::None
    };
    Ok((match_part, rest_part))
}

/// CPython `exceptiongroup_subset`: wrap a sub-sequence of `orig`'s
/// exceptions in a new group with `orig`'s metadata. Dispatches
/// `orig.derive(excs)` (always — the default `derive` reconstructs via
/// the `BaseExceptionGroup` constructor), validates the result, then
/// copies `__traceback__`/`__context__`/`__cause__` and shallow-copies
/// a sequence-valued `__notes__` so each part gets its own list.
fn eg_subset(
    orig: &Object,
    orig_inst: &Rc<crate::types::PyInstance>,
    items: Vec<Object>,
) -> Result<Object, RuntimeError> {
    if items.is_empty() {
        return Ok(Object::None);
    }
    let excs_list = Object::new_list(items);
    let derive = orig_inst
        .cls()
        .lookup("derive")
        .ok_or_else(|| crate::error::type_error("exception group lost its derive"))?;
    let derived = crate::builtins::reentrant_call(&derive, &[orig.clone(), excs_list])?;
    if !instance_is_subclass(&derived, &builtin_types().base_exception_group) {
        return Err(crate::error::type_error(
            "derive must return an instance of BaseExceptionGroup",
        ));
    }
    if let Object::Instance(dst) = &derived {
        for key in ["__traceback__", "__context__", "__cause__"] {
            if let Some(v) = orig_inst.slot_get(key) {
                dst.slot_set(key, v);
            }
        }
        // `__notes__` is a real instance attribute (PEP 678). A
        // sequence is shallow-copied so the parts have independent
        // lists; a non-sequence is silently skipped (split is not the
        // place to report that user error — CPython does the same).
        let notes = orig_inst
            .dict
            .borrow()
            .get(&crate::object::StrKey("__notes__"))
            .cloned();
        if let Some(notes) = notes {
            let copied = match &notes {
                Object::List(l) => Some(Object::new_list(l.borrow().clone())),
                Object::Tuple(t) => Some(Object::new_list(t.to_vec())),
                _ => None,
            };
            if let Some(c) = copied {
                dst.dict
                    .borrow_mut()
                    .insert(DictKey(Object::from_static("__notes__")), c);
            }
        }
    }
    Ok(derived)
}

/// Wrap a naked (non-group) exception caught by an `except*` clause in
/// an implicit `ExceptionGroup("", (exc,))` — CPython's
/// `exception_group_match` does this inside `CHECK_EG_MATCH`. The
/// caller attaches the current frame's traceback entry (gh-128799).
pub fn make_naked_eg_wrapper(exc: &Object) -> Object {
    let items = vec![exc.clone()];
    let cls = exception_group_class_for(&items);
    let items_t = Object::new_tuple(items);
    let wrapper = make_exception_with_class(cls, "");
    if let Object::Instance(inst) = &wrapper {
        inst.slot_set(
            "args",
            Object::new_tuple(vec![Object::from_static(""), items_t.clone()]),
        );
        inst.slot_set("message", Object::from_static(""));
        inst.slot_set("exceptions", items_t);
    }
    wrapper
}

/// CPython's `is_same_exception_metadata`: two exceptions are "the
/// same raise" when their `__notes__`, `__traceback__`, `__cause__`
/// and `__context__` are identical *objects*. Used by
/// `prep_reraise_star` to tell re-raised parts of the original group
/// from newly raised exceptions.
fn is_same_exception_metadata(a: &Object, b: &Object) -> bool {
    let (Object::Instance(ia), Object::Instance(ib)) = (a, b) else {
        return false;
    };
    for key in ["__notes__", "__traceback__", "__cause__", "__context__"] {
        let va = exc_attr(ia, key);
        let vb = exc_attr(ib, key);
        let same = match (&va, &vb) {
            (Some(Object::None) | None, Some(Object::None) | None) => true,
            (Some(x), Some(y)) => x.is_same(y),
            _ => false,
        };
        if !same {
            return false;
        }
    }
    true
}

/// Collect the identities (`Rc` pointers) of an exception tree's leaf
/// exceptions, recursing through nested groups.
fn collect_eg_leaf_ids(exc: &Object, ids: &mut std::collections::HashSet<usize>) {
    let is_group = matches!(
        exc,
        Object::Instance(i) if is_subclass_by_name(&i.cls(), "BaseExceptionGroup")
    );
    if is_group {
        if let Object::Instance(inst) = exc {
            let excs = exc_attr(inst, "exceptions");
            if let Some(Object::Tuple(t)) = excs {
                for e in t.iter() {
                    collect_eg_leaf_ids(e, ids);
                }
                return;
            }
        }
    }
    if let Object::Instance(inst) = exc {
        ids.insert(Rc::as_ptr(inst) as usize);
    }
}

/// CPython's `exception_group_projection`: the subgroup of `orig`
/// containing exactly the leaves that appear (by identity) under any
/// exception in `keep`. Preserves `orig`'s nesting structure and
/// metadata on the derived groups. Returns `None` when nothing is kept.
fn exception_group_projection(orig: &Object, keep: &[Object]) -> Result<Object, RuntimeError> {
    let mut ids = std::collections::HashSet::new();
    for e in keep {
        collect_eg_leaf_ids(e, &mut ids);
    }
    let (matched, _rest) = split_exception_group_by(orig, &|exc| {
        // CPython's `EXCEPTION_GROUP_MATCH_INSTANCE_IDS` never matches
        // a *group* — only leaves are compared by identity.
        let is_group = matches!(
            exc,
            Object::Instance(i) if is_subclass_by_name(&i.cls(), "BaseExceptionGroup")
        );
        Ok(match exc {
            Object::Instance(i) if !is_group => ids.contains(&(Rc::as_ptr(i) as usize)),
            _ => false,
        })
    })?;
    Ok(matched)
}

/// CPython's `_PyExc_PrepReraiseStar` intrinsic: combine the exceptions
/// raised/re-raised by `except*` handler bodies (`excs`, with the
/// unmatched remainder — possibly `None` — as its last element) into
/// the single exception to propagate, or `None` when fully handled.
pub fn prep_reraise_star(orig: &Object, excs: &[Object]) -> Result<Object, RuntimeError> {
    if excs.is_empty() {
        return Ok(Object::None);
    }
    let bt = builtin_types();
    let orig_is_group = matches!(
        orig,
        Object::Instance(i) if i.cls().is_subclass_of(&bt.base_exception_group)
    );
    if !orig_is_group {
        // A naked exception was caught and wrapped; at most one
        // `except*` clause ran, so there is at most one exception to
        // raise (plus the always-appended `None` remainder).
        return Ok(excs
            .iter()
            .find(|e| !matches!(e, Object::None))
            .cloned()
            .unwrap_or(Object::None));
    }
    let mut raised: Vec<Object> = Vec::new();
    let mut reraised: Vec<Object> = Vec::new();
    for e in excs {
        if matches!(e, Object::None) {
            continue;
        }
        if is_same_exception_metadata(e, orig) {
            reraised.push(e.clone());
        } else {
            raised.push(e.clone());
        }
    }
    let reraised_eg = if reraised.is_empty() {
        Object::None
    } else {
        exception_group_projection(orig, &reraised)?
    };
    if raised.is_empty() {
        return Ok(reraised_eg);
    }
    if !matches!(reraised_eg, Object::None) {
        raised.push(reraised_eg);
    }
    if raised.len() == 1 {
        return Ok(raised.pop().expect("len checked"));
    }
    // Multiple exceptions — combine them as siblings in a fresh group
    // with an empty message (no metadata is copied; the re-raise builds
    // the traceback from the `except*` frame outward).
    let cls = exception_group_class_for(&raised);
    let items_t = Object::new_tuple(raised);
    let combined = make_exception_with_class(cls, "");
    if let Object::Instance(inst) = &combined {
        inst.slot_set(
            "args",
            Object::new_tuple(vec![Object::from_static(""), items_t.clone()]),
        );
        inst.slot_set("message", Object::from_static(""));
        inst.slot_set("exceptions", items_t);
    }
    Ok(combined)
}

fn exception_matches_type(exc: &Object, type_pred: &Object) -> bool {
    match type_pred {
        Object::Type(t) => instance_is_subclass(exc, t),
        Object::Tuple(items) => items
            .iter()
            .any(|x| matches!(x, Object::Type(t) if instance_is_subclass(exc, t))),
        _ => false,
    }
}

fn is_subclass_by_name(class: &Rc<TypeObject>, ancestor: &str) -> bool {
    for t in class.mro.borrow().iter() {
        if t.name == ancestor {
            return true;
        }
    }
    false
}

/// Extract the "message" of an exception instance — used by the
/// error formatter.
pub fn exception_message(obj: &Object) -> Option<String> {
    match obj {
        Object::Instance(inst) => {
            if let Some(Object::Str(s)) = exc_attr(inst, "message") {
                return Some(s.to_string());
            }
            if let Some(Object::Tuple(items)) = exc_attr(inst, "args") {
                if let Some(first) = items.first() {
                    return Some(first.to_str());
                }
            }
            None
        }
        _ => None,
    }
}

/// `True` when `obj` is an instance whose class derives from `cls`.
pub fn instance_is_subclass(obj: &Object, cls: &TypeObject) -> bool {
    match obj {
        Object::Instance(inst) => inst.cls().is_subclass_of(cls),
        _ => false,
    }
}

/// Install a distinct `__new__` in each value/container built-in's own dict.
///
/// CPython exposes a per-type `tp_new` in `tp_dict`, so `'__new__' in
/// int.__dict__` is True and `int.__new__ is not object.__new__`. WeavePy's
/// instantiation path keys the "default allocator" check on the builtin's
/// `"__new__"` name (not its type), so these all route through the same
/// native-seeding allocator — only their *identity* differs, which is what
/// `enum`'s `_find_data_type_` / `_find_new_` inspect.
fn install_value_type_new(bt: &BuiltinTypes) {
    for ty in [
        &bt.int_,
        &bt.float_,
        &bt.bool_,
        &bt.complex_,
        &bt.str_,
        &bt.bytes_,
        &bt.bytearray_,
        &bt.tuple_,
        &bt.list_,
        &bt.dict_,
        &bt.set_,
        &bt.frozenset_,
    ] {
        // `int.__new__` carries its owner so the `tp_new_wrapper` safety
        // check can reject `int.__new__(bool, 0)` (bool's tp_new differs
        // from int's in CPython) while `bool.__new__(bool, 0)` still works.
        let wrapper = if Rc::ptr_eq(ty, &bt.int_) {
            make_owned_new("int")
        } else {
            make_default_new()
        };
        register_new_metadata(&wrapper, ty);
        ty.dict
            .borrow_mut()
            .insert(DictKey(Object::from_static("__new__")), wrapper);
    }
    install_mutable_container_init(bt);
}

/// Descriptor metadata for a per-type default allocator:
/// `complex.__new__.__qualname__ == 'complex.__new__'` and
/// `__module__ is None`, as on CPython's tp_new wrappers — pickling a
/// `partial(cls.__new__, cls, …)` (protocol 2/3 `__getnewargs_ex__`
/// route) resolves the callable through `builtins.<type>.__new__` by
/// qualname (pickletester test_complex_newobj_ex).
fn register_new_metadata(wrapper: &Object, ty: &Rc<TypeObject>) {
    crate::descr_registry::register(
        wrapper,
        crate::descr_registry::DescrKind::StaticBuiltin,
        ty.clone(),
        "__new__",
        None,
    );
    crate::descr_registry::set_builtin_module(wrapper, Object::None);
}

/// Like [`make_default_new`], but the wrapper knows which built-in type it
/// was installed on, mirroring CPython's `tp_new_wrapper` "staticbase"
/// check: calling `A.__new__(B, …)` when `B` overrides `A`'s `tp_new`
/// raises "A.__new__(B) is not safe, use B.__new__()". Only the
/// `int`/`bool` pair matters among WeavePy's built-ins (bool is the one
/// built-in subclass with its own constructor semantics).
fn make_owned_new(owner: &'static str) -> Object {
    use crate::object::BuiltinFn;
    fn reject_bool(owner: &str, args: &[Object]) -> Result<(), RuntimeError> {
        if owner == "int" {
            if let Some(Object::Type(cls)) = args.first() {
                if cls.flags.is_builtin && cls.name == "bool" {
                    return Err(crate::error::type_error(
                        "int.__new__(bool) is not safe, use bool.__new__()".to_owned(),
                    ));
                }
            }
        }
        Ok(())
    }
    let owner2 = owner;
    // Raw builtin, like `make_default_new` — CPython type dicts hold the
    // bare `PyCFunction`, never a staticmethod wrapper.
    let obj = Object::Builtin(Rc::new(BuiltinFn {
        name: "__new__",
        binds_instance: false,
        call: Box::new(move |args| {
            reject_bool(owner, args)?;
            object_new(args)
        }),
        // `int.__new__(cls, x, base=…)` — the argument clinic exposes
        // `base` by keyword (pickletester's ComplexNewObjEx round-trips
        // through NEWOBJ_EX exactly this way).
        call_kw: Some(Box::new(move |args, kwargs| {
            reject_bool(owner2, args)?;
            if kwargs.is_empty() {
                return object_new(args);
            }
            if let Some(res) = int_new_kw(args, kwargs) {
                return res;
            }
            Err(crate::error::type_error(
                "__new__() takes no keyword arguments".to_owned(),
            ))
        })),
    }));
    crate::descr_registry::mark_default_new(&obj);
    // CPython `object.__new__`'s clinic string — `inspect.signature(
    // C.__new__, follow_wrapped=False)` on a plain class parses it to
    // `(*args, **kwargs)` (test_inspect test_signature_on_class_with_
    // wrapped_init [descriptor]).
    crate::descr_registry::register_text_signature(&obj, "($type, *args, **kwargs)");
    obj
}

/// `int.__new__(cls, …, base=…)`: forward the keyword form to the real int
/// constructor, then re-wrap in `cls` when it's a strict subclass (CPython
/// `long_new` → `long_subtype_new`). Returns `None` when `cls` isn't an
/// int-family user type (the caller falls back to its arity policy).
fn int_new_kw(
    args: &[Object],
    kwargs: &[(String, Object)],
) -> Option<Result<Object, RuntimeError>> {
    use crate::types::PyInstance;
    let Some(Object::Type(cls)) = args.first() else {
        return None;
    };
    let bt = builtin_types();
    if !cls.is_subclass_of(&bt.int_) {
        return None;
    }
    if cls.flags.is_builtin && !Rc::ptr_eq(cls, &bt.int_) {
        // bool (and any other builtin int-family type) rejects kwargs.
        return None;
    }
    let ptr = crate::vm_singletons::current_interpreter_ptr()?;
    // SAFETY: published by an enclosing VM frame still live on this
    // thread; the GIL keeps the access exclusive.
    let interp = unsafe { &mut *ptr };
    let v = match interp.type_call_default(&bt.int_, &args[1..], kwargs) {
        Ok(v) => v,
        Err(e) => return Some(Err(e)),
    };
    if Rc::ptr_eq(cls, &bt.int_) {
        return Some(Ok(v));
    }
    let inst = Object::Instance(Rc::new(PyInstance::with_native(cls.clone(), v)));
    crate::gc_trace::track(inst.clone());
    Some(Ok(inst))
}

/// The mutable containers own a real `tp_init` in CPython: `dict.__init__`
/// merges a mapping/iterable + kwargs, `list.__init__` clears and extends,
/// `set.__init__` clears and unions. `super().__init__(src)` from a
/// subclass must reach these (not the strict `object.__init__`).
fn install_mutable_container_init(bt: &BuiltinTypes) {
    use crate::object::BuiltinFn;

    fn self_payload(args: &[Object]) -> Result<Object, RuntimeError> {
        match args.first() {
            Some(o @ (Object::Dict(_) | Object::List(_) | Object::Set(_))) => Ok(o.clone()),
            Some(Object::Instance(inst)) => match inst.native.get() {
                Some(n @ (Object::Dict(_) | Object::List(_) | Object::Set(_))) => Ok(n.clone()),
                _ => Err(crate::error::type_error(
                    "descriptor '__init__' requires a container instance".to_owned(),
                )),
            },
            _ => Err(crate::error::type_error(
                "descriptor '__init__' requires a container instance".to_owned(),
            )),
        }
    }

    fn reenter() -> Result<&'static mut crate::Interpreter, RuntimeError> {
        let ptr = crate::vm_singletons::current_interpreter_ptr()
            .ok_or_else(|| crate::error::runtime_error("no running interpreter"))?;
        // SAFETY: published by an enclosing VM frame still live on this
        // thread; the GIL keeps the access exclusive.
        Ok(unsafe { &mut *ptr })
    }

    fn dict_pairs_from_iterable(
        interp: &mut crate::Interpreter,
        src: &Object,
        globals: &Rc<RefCell<DictData>>,
    ) -> Result<Vec<(DictKey, Object)>, RuntimeError> {
        let items = interp.collect_iterable(src, globals)?;
        let mut out = Vec::with_capacity(items.len());
        for (i, pair) in items.into_iter().enumerate() {
            let kv = interp.collect_iterable(&pair, globals)?;
            if kv.len() != 2 {
                return Err(crate::error::type_error(format!(
                    "dictionary update sequence element #{i} has length {}; 2 is required",
                    kv.len()
                )));
            }
            out.push((DictKey(kv[0].clone()), kv[1].clone()));
        }
        Ok(out)
    }

    fn dict_init_kw(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
        let payload = self_payload(args)?;
        let Object::Dict(target) = &payload else {
            return Err(crate::error::type_error(
                "descriptor '__init__' requires a 'dict' object".to_owned(),
            ));
        };
        if args.len() > 2 {
            return Err(crate::error::type_error(format!(
                "dict expected at most 1 argument, got {}",
                args.len() - 1
            )));
        }
        if let Some(src) = args.get(1) {
            let interp = reenter()?;
            let globals = interp.builtins_dict();
            let merged: Vec<(DictKey, Object)> =
                if let Some(Object::Dict(d)) = interp.try_dict_from_mapping(src, &globals)? {
                    let view = d.borrow();
                    view.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
                } else {
                    dict_pairs_from_iterable(interp, src, &globals)?
                };
            let mut t = target.borrow_mut();
            for (k, v) in merged {
                t.insert(k, v);
            }
        }
        let mut t = target.borrow_mut();
        for (k, v) in kwargs {
            t.insert(DictKey(Object::from_str(k.clone())), v.clone());
        }
        Ok(Object::None)
    }

    fn list_init(args: &[Object]) -> Result<Object, RuntimeError> {
        let payload = self_payload(args)?;
        let Object::List(target) = &payload else {
            return Err(crate::error::type_error(
                "descriptor '__init__' requires a 'list' object".to_owned(),
            ));
        };
        if args.len() > 2 {
            return Err(crate::error::type_error(format!(
                "list expected at most 1 argument, got {}",
                args.len() - 1
            )));
        }
        let items = match args.get(1) {
            Some(src) => {
                let interp = reenter()?;
                let globals = interp.builtins_dict();
                interp.collect_iterable(src, &globals)?
            }
            None => Vec::new(),
        };
        let mut t = target.borrow_mut();
        t.clear();
        t.extend(items);
        Ok(Object::None)
    }

    fn set_init(args: &[Object]) -> Result<Object, RuntimeError> {
        let payload = self_payload(args)?;
        let Object::Set(target) = &payload else {
            return Err(crate::error::type_error(
                "descriptor '__init__' requires a 'set' object".to_owned(),
            ));
        };
        if args.len() > 2 {
            return Err(crate::error::type_error(format!(
                "set expected at most 1 argument, got {}",
                args.len() - 1
            )));
        }
        let items = match args.get(1) {
            Some(src) => {
                let interp = reenter()?;
                let globals = interp.builtins_dict();
                interp.collect_iterable(src, &globals)?
            }
            None => Vec::new(),
        };
        // Enforce hashability as each element is admitted, exactly like the
        // free-function `set(...)` constructor (`set_insert_key` →
        // `ensure_hashable`). Building the keyed list *before* mutating the
        // target means an unhashable element (`MySet([[]])`) raises
        // `TypeError` without leaving the set half-filled.
        let mut keys = Vec::with_capacity(items.len());
        for item in items {
            keys.push(crate::builtins::set_insert_key(&item)?);
        }
        let mut t = target.borrow_mut();
        t.clear();
        crate::object::key_cmp_scope(|| {
            for k in keys {
                t.insert(k);
            }
        })?;
        Ok(Object::None)
    }

    fn dict_init(args: &[Object]) -> Result<Object, RuntimeError> {
        dict_init_kw(args, &[])
    }

    bt.dict_.dict.borrow_mut().insert(
        DictKey(Object::from_static("__init__")),
        Object::Builtin(Rc::new(BuiltinFn {
            name: "__init__",
            binds_instance: true,
            call: Box::new(dict_init),
            call_kw: Some(Box::new(dict_init_kw)),
        })),
    );
    bt.list_.dict.borrow_mut().insert(
        DictKey(Object::from_static("__init__")),
        Object::Builtin(Rc::new(BuiltinFn {
            name: "__init__",
            binds_instance: true,
            call: Box::new(list_init),
            call_kw: None,
        })),
    );
    bt.set_.dict.borrow_mut().insert(
        DictKey(Object::from_static("__init__")),
        Object::Builtin(Rc::new(BuiltinFn {
            name: "__init__",
            binds_instance: true,
            call: Box::new(set_init),
            call_kw: None,
        })),
    );
    // bytearray owns a real `tp_init` too: it (re)seeds the buffer from
    // `source`/`encoding`/`errors` keywords — `bytearray(source=b'abc')`
    // and subclass `__init__` chains both rely on it.
    bt.bytearray_.dict.borrow_mut().insert(
        DictKey(Object::from_static("__init__")),
        Object::Builtin(Rc::new(BuiltinFn {
            name: "__init__",
            binds_instance: true,
            call: Box::new(crate::builtins::bytearray_init),
            call_kw: Some(Box::new(crate::builtins::bytearray_init_kw)),
        })),
    );
}

/// RFC 0019 — install class methods on the numeric / bytes types.
/// Adds `int.from_bytes`, `bytes.fromhex`, `bytearray.fromhex`,
/// and `float.fromhex` as classmethod-shaped builtins so that
/// `int.from_bytes(b'\\x00\\xff', 'big')` resolves through the
/// type's MRO rather than the instance method dispatch.
fn install_numeric_class_methods(bt: &BuiltinTypes) {
    use crate::object::BuiltinFn;
    fn install(
        ty: &Rc<TypeObject>,
        name: &'static str,
        f: fn(&[Object]) -> Result<Object, RuntimeError>,
    ) {
        let builtin = Object::Builtin(Rc::new(BuiltinFn {
            name,
            binds_instance: true,
            call: Box::new(f),
            call_kw: None,
        }));
        // Wrap as `classmethod` so descriptor binding skips the
        // instance and routes through the class.
        let cm = Object::ClassMethod(MethodWrapper::new(builtin));
        ty.dict
            .borrow_mut()
            .insert(DictKey(Object::from_static(name)), cm);
    }
    // Same, but for a classmethod that accepts keyword arguments
    // (`int.from_bytes(b, byteorder='big', *, signed=False)`).
    fn install_kw(
        ty: &Rc<TypeObject>,
        name: &'static str,
        f: fn(&[Object], &[(String, Object)]) -> Result<Object, RuntimeError>,
    ) {
        let builtin = Object::Builtin(Rc::new(BuiltinFn {
            name,
            binds_instance: true,
            call: Box::new(move |args| f(args, &[])),
            call_kw: Some(Box::new(f)),
        }));
        let cm = Object::ClassMethod(MethodWrapper::new(builtin));
        ty.dict
            .borrow_mut()
            .insert(DictKey(Object::from_static(name)), cm);
    }

    install_kw(
        &bt.int_,
        "from_bytes",
        crate::builtins::b_int_from_bytes_cls,
    );
    install(&bt.bytes_, "fromhex", crate::builtins::b_bytes_fromhex_cls);
    install(
        &bt.bytearray_,
        "fromhex",
        crate::builtins::b_bytearray_fromhex_cls,
    );
    install(&bt.float_, "fromhex", crate::builtins::b_float_fromhex_cls);
    // `float.__getformat__` — CPython keeps this (undocumented) classmethod
    // for `test.support` (`HAVE_IEEE_754`) and struct/float tests. Rust f64
    // is IEEE 754 binary64 on every supported target, so the answer is
    // constant per endianness.
    install(
        &bt.float_,
        "__getformat__",
        crate::builtins::b_float_getformat_cls,
    );

    // Expose `__hash__` on the hashable value built-ins so it sits in their
    // type dict. Without this, a mixin like `class F(float, H)` would resolve
    // `H.__hash__` (the first `__hash__` found in the MRO) instead of
    // `float.__hash__`; CPython resolves `float.__hash__` because `float`
    // precedes `H`. The method itself defers to the canonical `hash()`
    // (which unwraps the native payload), so `object.__hash__(x) == hash(x)`.
    fn install_hash(ty: &Rc<TypeObject>) {
        fn value_hash(args: &[Object]) -> Result<Object, RuntimeError> {
            let obj = args.first().unwrap_or(&Object::None);
            // `int.__hash__(self)` / `float.__hash__(self)` / … must hash the
            // *underlying value* directly, exactly like CPython's
            // `long_hash`/`float_hash` type slot. It must NOT re-dispatch
            // through a subclass's Python `__hash__`, otherwise the common
            // idiom `class H(int): def __hash__(self): return int.__hash__(self)`
            // recurses (HashCountingInt in test_set) until the recursion limit.
            // Unwrap an int/str/… subclass instance to the primitive it wraps
            // so the hash is computed on the value, bypassing the override.
            let target = match obj {
                Object::Instance(inst) => inst.native.get().unwrap_or(obj),
                other => other,
            };
            crate::builtins::hash_object(target)
        }
        ty.dict.borrow_mut().insert(
            DictKey(Object::from_static("__hash__")),
            Object::Builtin(Rc::new(BuiltinFn {
                name: "__hash__",
                binds_instance: true,
                call: Box::new(value_hash),
                call_kw: None,
            })),
        );
    }
    for ty in [
        &bt.int_,
        &bt.float_,
        &bt.complex_,
        &bt.str_,
        &bt.bytes_,
        &bt.tuple_,
        &bt.frozenset_,
    ] {
        install_hash(ty);
    }

    // Expose the inherited numeric coercion dunders so a subclass that does
    // *not* override them (`class C(int)` with only `__index__`) still
    // resolves the base type's value-returning `__int__`/`__index__`/
    // `__float__` through the MRO — matching CPython, where `int(C())` uses
    // the wrapped value rather than the overriding hook.
    fn install_method(
        ty: &Rc<TypeObject>,
        name: &'static str,
        f: fn(&[Object]) -> Result<Object, RuntimeError>,
    ) {
        ty.dict.borrow_mut().insert(
            DictKey(Object::from_static(name)),
            Object::Builtin(Rc::new(BuiltinFn {
                name,
                binds_instance: true,
                call: Box::new(f),
                call_kw: None,
            })),
        );
    }
    fn self_as_int(args: &[Object]) -> Result<Object, RuntimeError> {
        let o = args
            .first()
            .ok_or_else(|| crate::error::type_error("__int__ requires an argument"))?;
        let native = o.native_value();
        match native.as_ref().unwrap_or(o) {
            Object::Int(i) => Ok(Object::Int(*i)),
            Object::Long(b) => Ok(Object::Long(b.clone())),
            Object::Bool(b) => Ok(Object::Int(i64::from(*b))),
            other => Err(crate::error::type_error(format!(
                "descriptor '__int__' requires a 'int' object but received a '{}'",
                other.type_name()
            ))),
        }
    }
    fn self_as_float(args: &[Object]) -> Result<Object, RuntimeError> {
        let o = args
            .first()
            .ok_or_else(|| crate::error::type_error("__float__ requires an argument"))?;
        let native = o.native_value();
        match native.as_ref().unwrap_or(o) {
            Object::Float(f) => Ok(Object::Float(*f)),
            other => Err(crate::error::type_error(format!(
                "descriptor '__float__' requires a 'float' object but received a '{}'",
                other.type_name()
            ))),
        }
    }
    // CPython's `float.__int__` truncates toward zero, raising on non-finite
    // values (`ValueError` for NaN, `OverflowError` for ±inf). Faithful via a
    // bigint so magnitudes past the i64 range convert exactly.
    fn float_as_int(args: &[Object]) -> Result<Object, RuntimeError> {
        let o = args
            .first()
            .ok_or_else(|| crate::error::type_error("__int__ requires an argument"))?;
        let native = o.native_value();
        match native.as_ref().unwrap_or(o) {
            Object::Float(f) => {
                if f.is_nan() {
                    return Err(crate::error::value_error(
                        "cannot convert float NaN to integer",
                    ));
                }
                if f.is_infinite() {
                    return Err(crate::error::overflow_error(
                        "cannot convert float infinity to integer",
                    ));
                }
                Ok(Object::int_from_bigint(
                    crate::object::bigint_from_f64_trunc(f.trunc()),
                ))
            }
            other => Err(crate::error::type_error(format!(
                "descriptor '__int__' requires a 'float' object but received a '{}'",
                other.type_name()
            ))),
        }
    }
    install_method(&bt.int_, "__int__", self_as_int);
    install_method(&bt.int_, "__index__", self_as_int);
    install_method(&bt.float_, "__float__", self_as_float);
    install_method(&bt.float_, "__int__", float_as_int);
}

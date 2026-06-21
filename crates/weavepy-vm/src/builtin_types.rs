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
        let dict_keys_ = mk("dict_keys", vec![object_.clone()]);
        let dict_values_ = mk("dict_values", vec![object_.clone()]);
        let dict_items_ = mk("dict_items", vec![object_.clone()]);
        let iterator_ = mk("iterator", vec![object_.clone()]);
        let none_type = mk("NoneType", vec![object_.clone()]);
        let ellipsis_ = mk("ellipsis", vec![object_.clone()]);
        let not_implemented_type_ = mk("NotImplementedType", vec![object_.clone()]);
        let simple_namespace_ = mk("SimpleNamespace", vec![object_.clone()]);
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
        let function_ = mk("function", vec![object_.clone()]);
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
        let code_ = mk("code", vec![object_.clone()]);
        let traceback_ = mk("traceback", vec![object_.clone()]);
        let module_ = mk("module", vec![object_.clone()]);
        install_module_init(&module_);

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
                    Object::None,
                );
            }
            d.insert(
                crate::object::DictKey(Object::from_static("__suppress_context__")),
                Object::Bool(false),
            );
            d.insert(
                crate::object::DictKey(Object::from_static("args")),
                Object::new_tuple(Vec::new()),
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
                d.insert(crate::object::DictKey(Object::from_static(f)), Object::None);
            }
        }
        install_field_defaults(&attribute_error, &["name", "obj"]);
        install_field_defaults(&name_error, &["name"]);
        install_field_defaults(&import_error, &["name", "path", "name_from"]);
        install_import_error_init(&import_error);
        let os_error = exc("OSError", exception.clone());
        install_os_error_init(&os_error);
        let runtime_error = exc("RuntimeError", exception.clone());
        let not_implemented_error = exc("NotImplementedError", runtime_error.clone());
        let recursion_error = exc("RecursionError", runtime_error.clone());
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
            DictData::new(),
            crate::types::TypeFlags {
                is_exception: true,
                is_builtin: true,
            },
        )
        .expect("ExceptionGroup MRO");
        install_exception_group_init(&base_exception_group);

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
            "OSError" => Some(self.os_error.clone()),
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
}

/// Per-thread accessor. The registry is constructed lazily on first
/// access. Panics if construction fails — that means the C3 invariant
/// is broken on the built-in hierarchy itself.
pub fn property_class() -> Rc<TypeObject> {
    thread_local! {
        static PROPERTY_CLASS: RefCell<Option<Rc<TypeObject>>> = const { RefCell::new(None) };
    }
    PROPERTY_CLASS.with(|slot| {
        if let Some(c) = slot.borrow().as_ref() {
            return c.clone();
        }
        let bt = builtin_types();
        let cls = TypeObject::new_user("property", vec![bt.object_.clone()], DictData::new())
            .expect("property type");
        *slot.borrow_mut() = Some(cls.clone());
        cls
    })
}

pub fn builtin_types() -> Rc<BuiltinTypes> {
    BUILTIN_TYPES.with(|cell| {
        if cell.borrow().is_none() {
            *cell.borrow_mut() = Some(Rc::new(BuiltinTypes::build()));
        }
        cell.borrow().as_ref().unwrap().clone()
    })
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
        let mut dict = inst.dict.borrow_mut();
        dict.insert(
            DictKey(Object::from_static("args")),
            Object::new_tuple(vec![arg.clone()]),
        );
        dict.insert(
            DictKey(Object::from_static("message")),
            Object::from_str(arg.repr()),
        );
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
    use crate::types::PyInstance;
    let bt = builtin_types();
    let class = bt
        .by_name("UnicodeEncodeError")
        .unwrap_or_else(|| bt.value_error.clone());
    let inst = PyInstance::new(class);
    let enc = Object::from_str(encoding);
    let obj = Object::from_str(object);
    let start_o = Object::Int(start as i64);
    let end_o = Object::Int(end as i64);
    let reason_o = Object::from_str(reason);
    {
        let mut dict = inst.dict.borrow_mut();
        dict.insert(
            DictKey(Object::from_static("args")),
            Object::new_tuple(vec![
                enc.clone(),
                obj.clone(),
                start_o.clone(),
                end_o.clone(),
                reason_o.clone(),
            ]),
        );
        dict.insert(DictKey(Object::from_static("encoding")), enc);
        dict.insert(DictKey(Object::from_static("object")), obj);
        dict.insert(DictKey(Object::from_static("start")), start_o);
        dict.insert(DictKey(Object::from_static("end")), end_o);
        dict.insert(DictKey(Object::from_static("reason")), reason_o);
        dict.insert(DictKey(Object::from_static("__context__")), Object::None);
        dict.insert(DictKey(Object::from_static("__cause__")), Object::None);
        dict.insert(
            DictKey(Object::from_static("__suppress_context__")),
            Object::Bool(false),
        );
        dict.insert(DictKey(Object::from_static("__traceback__")), Object::None);
    }
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
    {
        let mut dict = inst.dict.borrow_mut();
        dict.insert(
            DictKey(Object::from_static("args")),
            Object::new_tuple(vec![
                enc.clone(),
                obj.clone(),
                start_o.clone(),
                end_o.clone(),
                reason_o.clone(),
            ]),
        );
        dict.insert(DictKey(Object::from_static("encoding")), enc);
        dict.insert(DictKey(Object::from_static("object")), obj);
        dict.insert(DictKey(Object::from_static("start")), start_o);
        dict.insert(DictKey(Object::from_static("end")), end_o);
        dict.insert(DictKey(Object::from_static("reason")), reason_o);
        dict.insert(DictKey(Object::from_static("__context__")), Object::None);
        dict.insert(DictKey(Object::from_static("__cause__")), Object::None);
        dict.insert(
            DictKey(Object::from_static("__suppress_context__")),
            Object::Bool(false),
        );
        dict.insert(DictKey(Object::from_static("__traceback__")), Object::None);
    }
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
        Object::Instance(inst) => inst.native.as_ref().and_then(concrete_elements),
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
        return Some(Object::Dict(Rc::new(RefCell::new(DictData::new()))));
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
    if cls.flags.is_builtin && !Rc::ptr_eq(&cls, &builtin_types().object_) {
        if let Some(ptr) = crate::vm_singletons::current_interpreter_ptr() {
            // SAFETY: published by an enclosing VM frame still live on this
            // thread; the GIL keeps the access exclusive.
            let interp = unsafe { &mut *ptr };
            return interp.type_call_default(&cls, &args[1..], &[]);
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
        if ty
            .dict
            .borrow()
            .contains_key(&DictKey(Object::from_static("__init__")))
        {
            return ty.name != "object";
        }
    }
    false
}

/// A fresh `Object::StaticMethod(Builtin "__new__")` wrapping [`object_new`].
/// Each call returns a *distinct* object so `int.__new__ is object.__new__`
/// is `False` (matching CPython) while the instantiation path still treats it
/// as the default allocator (it keys on the builtin's `"__new__"` name).
fn make_default_new() -> Object {
    use crate::object::BuiltinFn;
    Object::StaticMethod(MethodWrapper::new(Object::Builtin(Rc::new(BuiltinFn {
        name: "__new__",
        binds_instance: true,
        call: Box::new(object_new),
        call_kw: None,
    }))))
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
        Ok(Object::from_str(gen_of(args)?.name.borrow().clone()))
    }
    fn get_qualname(args: &[Object]) -> Result<Object, RuntimeError> {
        Ok(Object::from_str(gen_of(args)?.qualname.borrow().clone()))
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

/// Install `object.__new__`, `object.__init__`, `object.__setattr__`
/// and `object.__delattr__` on the root class. These are the implicit
/// base methods every user class inherits.
fn install_object_dunders(object_: &Rc<TypeObject>) {
    use crate::object::BuiltinFn;
    fn object_init(args: &[Object]) -> Result<Object, RuntimeError> {
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
                if inst.native.is_none() {
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
                let removed = inst
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
        // Default `object.__hash__`: the same canonical hash the `hash()`
        // builtin falls back to when no custom `__hash__` is defined, so
        // `object.__hash__(x) == hash(x)` for any object using the default.
        let obj = args.first().ok_or_else(|| {
            crate::error::type_error("object.__hash__() takes exactly 1 argument")
        })?;
        crate::builtins::hash_object(obj)
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
    dict.insert(DictKey(Object::from_static("__new__")), make_default_new());
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
            let mut dict = inst_rc.dict.borrow_mut();
            dict.insert(
                DictKey(Object::from_static("args")),
                Object::new_tuple(rest.to_vec()),
            );
            dict.insert(
                DictKey(Object::from_static("msg")),
                if rest.len() == 1 {
                    rest[0].clone()
                } else {
                    Object::None
                },
            );
            dict.insert(DictKey(Object::from_static("name")), name);
            dict.insert(DictKey(Object::from_static("path")), path);
            dict.insert(DictKey(Object::from_static("name_from")), name_from);
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
            let rest = if args.len() > 1 { &args[1..] } else { &[][..] };
            let mut dict = inst_rc.dict.borrow_mut();
            // CPython `oserror_init` special case: a `BlockingIOError` (and
            // subclasses) built with *exactly three* positional args treats
            // the third as `characters_written` rather than `filename`, keeps
            // the full 3-tuple as `.args`, and leaves `filename` unset. With
            // any other arity it parses as a plain `OSError`
            // (`test_io.test_write_non_blocking` relies on
            // `BlockingIOError(EAGAIN, msg, written).characters_written`).
            let is_blocking = inst_rc
                .cls()
                .is_subclass_of(&builtin_types().blocking_io_error);
            if is_blocking && rest.len() == 3 {
                dict.insert(
                    DictKey(Object::from_static("args")),
                    Object::new_tuple(rest.to_vec()),
                );
                dict.insert(DictKey(Object::from_static("errno")), rest[0].clone());
                dict.insert(DictKey(Object::from_static("strerror")), rest[1].clone());
                dict.insert(
                    DictKey(Object::from_static("characters_written")),
                    rest[2].clone(),
                );
                dict.insert(DictKey(Object::from_static("filename")), Object::None);
                dict.insert(DictKey(Object::from_static("winerror")), Object::None);
                dict.insert(DictKey(Object::from_static("filename2")), Object::None);
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
            dict.insert(DictKey(Object::from_static("args")), args_tuple);
            let pick = |i: usize| {
                if populated {
                    rest.get(i).cloned().unwrap_or(Object::None)
                } else {
                    Object::None
                }
            };
            dict.insert(DictKey(Object::from_static("errno")), pick(0));
            dict.insert(DictKey(Object::from_static("strerror")), pick(1));
            dict.insert(DictKey(Object::from_static("filename")), pick(2));
            dict.insert(DictKey(Object::from_static("winerror")), pick(3));
            dict.insert(DictKey(Object::from_static("filename2")), pick(4));
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
        let dict = inst.dict.borrow();
        let get = |name: &'static str| dict.get(&DictKey(Object::from_static(name))).cloned();
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
        match dict.get(&DictKey(Object::from_static("args"))) {
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

    fn set(dict: &mut crate::object::DictData, name: &'static str, value: Object) {
        dict.insert(DictKey(Object::from_static(name)), value);
    }

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
        let mut dict = inst_rc.dict.borrow_mut();
        set(&mut dict, "args", Object::new_tuple(rest.to_vec()));
        let mut i = 0;
        if kind != UnicodeErrorKind::Translate {
            set(&mut dict, "encoding", rest[i].clone());
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
        set(&mut dict, "object", object);
        set(&mut dict, "start", rest[i + 1].clone());
        set(&mut dict, "end", rest[i + 2].clone());
        set(&mut dict, "reason", rest[i + 3].clone());
        Ok(Object::None)
    };

    let str_fn = move |args: &[Object]| -> Result<Object, RuntimeError> {
        let Some(Object::Instance(inst_rc)) = args.first() else {
            return Ok(Object::from_static(""));
        };
        let dict = inst_rc.dict.borrow();
        let get = |name: &'static str| dict.get(&DictKey(Object::from_static(name))).cloned();
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
                let s: Vec<char> = match &obj {
                    Object::Str(s) => s.chars().collect(),
                    _ => Vec::new(),
                };
                if start >= 0 && (start as usize) < s.len() && end == start + 1 {
                    let c = s[start as usize] as u32;
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
        let mut dict = inst_rc.dict.borrow_mut();
        set(&mut dict, "args", Object::new_tuple(rest.to_vec()));
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
            set(&mut dict, name, Object::None);
        }
        if let Some(msg) = rest.first() {
            set(&mut dict, "msg", msg.clone());
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
            set(&mut dict, "filename", pick(0));
            set(&mut dict, "lineno", pick(1));
            set(&mut dict, "offset", pick(2));
            set(&mut dict, "text", pick(3));
            if items.len() == 6 {
                set(&mut dict, "end_lineno", pick(4));
                set(&mut dict, "end_offset", pick(5));
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
        let dict = inst_rc.dict.borrow();
        let get = |name: &'static str| {
            dict.get(&DictKey(Object::from_static(name)))
                .cloned()
                .unwrap_or(Object::None)
        };
        let msg = get("msg");
        // CPython renders the message via `str(self.msg)`.
        let msg_str = match &msg {
            Object::None => "None".to_owned(),
            other => other.to_str(),
        };
        let filename = get("filename");
        let lineno = get("lineno");
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

fn install_exception_str_repr(base_exception: &Rc<TypeObject>) {
    use crate::object::BuiltinFn;
    fn exc_init(args: &[Object]) -> Result<Object, RuntimeError> {
        // CPython's BaseException.__init__(self, *args) stores `args`
        // on the instance so every subclass — built-in or user-defined
        // — exposes `e.args` automatically.
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
                inst_rc.dict.borrow_mut().insert(
                    DictKey(Object::from_static("value")),
                    rest.first().cloned().unwrap_or(Object::None),
                );
            }
            inst_rc.dict.borrow_mut().insert(
                DictKey(Object::from_static("args")),
                Object::new_tuple(rest),
            );
        }
        Ok(Object::None)
    }
    fn exc_str(args: &[Object]) -> Result<Object, RuntimeError> {
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
            let dict = inst_rc.dict.borrow();
            if let Some(Object::Tuple(items)) = dict.get(&DictKey(Object::from_static("args"))) {
                return Ok(match items.as_ref() {
                    [] => Object::from_static(""),
                    [single] => {
                        if is_key_error {
                            Object::from_str(single.repr())
                        } else if matches!(single, Object::Instance(_)) {
                            // A nested exception (or other instance) needs
                            // its own __str__ dispatched: CPython's
                            // BaseException.__str__ is `str(args[0])`.
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
    fn exc_repr(args: &[Object]) -> Result<Object, RuntimeError> {
        let inst = args
            .first()
            .ok_or_else(|| crate::error::type_error("expected exception instance".to_owned()))?;
        if let Object::Instance(inst_rc) = inst {
            let cls = inst_rc.cls().name.clone();
            let dict = inst_rc.dict.borrow();
            let args_repr = if let Some(Object::Tuple(items)) =
                dict.get(&DictKey(Object::from_static("args")))
            {
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
            inst_rc
                .dict
                .borrow_mut()
                .insert(DictKey(Object::from_static("__traceback__")), tb);
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
            let mut dict = inst_rc.dict.borrow_mut();
            for (k, v) in entries {
                if !matches!(k, Object::Str(_)) {
                    return Err(crate::error::type_error(format!(
                        "attribute name must be string, not '{}'",
                        k.type_name_owned()
                    )));
                }
                dict.insert(DictKey(k), v);
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
        let dict = inst.dict.borrow();
        let get = |name: &'static str| dict.get(&DictKey(Object::from_static(name))).cloned();
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
            "message",
            "__traceback__",
            "__context__",
            "__cause__",
            "__suppress_context__",
        ];
        // GH-103352: AttributeError deliberately drops `obj` from its
        // pickled state (it may be huge or unpicklable).
        let skip_obj = is_subclass_by_name(&cls, "AttributeError");
        let mut state = crate::object::DictData::new();
        for (k, v) in dict.iter() {
            if let Object::Str(s) = &k.0 {
                if SKIP.contains(&s.as_ref()) || (skip_obj && s.as_ref() == "obj") {
                    continue;
                }
            }
            state.insert(k.clone(), v.clone());
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
    let is_os = is_subclass_by_name(&class, "OSError");
    let is_syntax = is_subclass_by_name(&class, "SyntaxError");
    let is_stop_iteration = is_subclass_by_name(&class, "StopIteration");
    let inst = PyInstance::new(class);
    let msg = Object::from_str(message);
    // A messageless raise (`StopIteration()`, `GeneratorExit()`, …)
    // has *empty* args in CPython, not `("",)`.
    let args = if msg.to_str().is_empty() {
        Object::new_tuple(Vec::new())
    } else {
        Object::new_tuple(vec![msg.clone()])
    };
    {
        let mut dict = inst.dict.borrow_mut();
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
            dict.insert(DictKey(Object::from_static("value")), value);
        }
        dict.insert(DictKey(Object::from_static("args")), args);
        dict.insert(DictKey(Object::from_static("message")), msg.clone());
        // Always-present `BaseException` slots (see `build_exception_instance`):
        // default None/None/False/None so attribute access and context-chain
        // walks never `AttributeError`.
        dict.insert(DictKey(Object::from_static("__context__")), Object::None);
        dict.insert(DictKey(Object::from_static("__cause__")), Object::None);
        dict.insert(
            DictKey(Object::from_static("__suppress_context__")),
            Object::Bool(false),
        );
        dict.insert(DictKey(Object::from_static("__traceback__")), Object::None);
        if is_os {
            // OSError attributes — populated to None when we raise
            // from Rust so callers can still ask `exc.errno` without
            // an AttributeError. Real values land here through the
            // `OSError(errno, strerror, ...)` __init__ in Python.
            for name in ["errno", "strerror", "filename", "winerror", "filename2"] {
                dict.insert(DictKey(Object::from_static(name)), Object::None);
            }
        }
        if is_syntax {
            // SyntaxError exposes `msg` plus a location payload. CPython
            // always defines these slots (default `None`); a Rust-raised
            // bare SyntaxError gets `msg` from `args[0]` and `None`
            // elsewhere so `e.lineno` / `e.offset` never `AttributeError`.
            // `error::syntax_error_located` overwrites them with real
            // values when a byte offset is available.
            dict.insert(DictKey(Object::from_static("msg")), msg);
            for name in [
                "filename",
                "lineno",
                "offset",
                "text",
                "end_lineno",
                "end_offset",
            ] {
                dict.insert(DictKey(Object::from_static(name)), Object::None);
            }
        }
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
        // args = (self, msg, exceptions[, ...])
        let inst = args
            .first()
            .ok_or_else(|| crate::error::type_error("expected exception instance"))?;
        let msg = args.get(1).cloned().unwrap_or(Object::from_static(""));
        let excs = args
            .get(2)
            .cloned()
            .unwrap_or(Object::new_tuple(Vec::new()));
        // `exceptions` must be a sequence of BaseException instances;
        // CPython raises ValueError on empty. We're lenient here —
        // the caller may construct empty groups for split/subgroup.
        let excs_tuple = match &excs {
            Object::Tuple(items) => items.clone(),
            Object::List(items) => Rc::from(items.borrow().clone().into_boxed_slice()),
            other => {
                return Err(crate::error::type_error(format!(
                    "second argument (exceptions) must be a sequence, not '{}'",
                    other.type_name()
                )))
            }
        };
        if let Object::Instance(inst_rc) = inst {
            let mut dict = inst_rc.dict.borrow_mut();
            // `args` keeps the *original* second argument (a list stays
            // a list — `repr(eg)` shows it); only the `.exceptions`
            // accessor is normalized to a tuple, like CPython.
            dict.insert(
                DictKey(Object::from_static("args")),
                Object::new_tuple(vec![msg.clone(), excs]),
            );
            dict.insert(DictKey(Object::from_static("message")), msg);
            dict.insert(
                DictKey(Object::from_static("exceptions")),
                Object::Tuple(excs_tuple),
            );
        }
        Ok(Object::None)
    }
    fn eg_str(args: &[Object]) -> Result<Object, RuntimeError> {
        let inst = args
            .first()
            .ok_or_else(|| crate::error::type_error("expected exception instance"))?;
        if let Object::Instance(inst_rc) = inst {
            let dict = inst_rc.dict.borrow();
            let message = dict
                .get(&DictKey(Object::from_static("message")))
                .cloned()
                .unwrap_or(Object::from_static(""));
            let n = dict
                .get(&DictKey(Object::from_static("exceptions")))
                .and_then(|e| match e {
                    Object::Tuple(t) => Some(t.len()),
                    _ => None,
                })
                .unwrap_or(0);
            return Ok(Object::from_str(format!(
                "{} ({} sub-exception{})",
                message.to_str(),
                n,
                if n == 1 { "" } else { "s" }
            )));
        }
        Ok(Object::from_static(""))
    }
    fn eg_derive(args: &[Object]) -> Result<Object, RuntimeError> {
        // Default `derive(self, excs)` — CPython's returns a *plain*
        // `BaseExceptionGroup(self.message, excs)` (not `type(self)`),
        // which `__new__`'s PEP 654 magic lowers to `ExceptionGroup`
        // when every leaf is an `Exception`. Subclasses that want to
        // survive `split`/`subgroup` must override `derive`.
        let inst = args
            .first()
            .ok_or_else(|| crate::error::type_error("expected exception instance"))?;
        let excs = args
            .get(1)
            .cloned()
            .unwrap_or(Object::new_tuple(Vec::new()));
        if let Object::Instance(inst_rc) = inst {
            let dict = inst_rc.dict.borrow();
            let msg = dict
                .get(&DictKey(Object::from_static("message")))
                .cloned()
                .unwrap_or(Object::from_static(""));
            drop(dict);
            let excs_tuple: Rc<[Object]> = match excs {
                Object::Tuple(t) => t,
                Object::List(l) => Rc::from(l.borrow().clone().into_boxed_slice()),
                _ => Rc::from(Vec::<Object>::new().into_boxed_slice()),
            };
            let cls = exception_group_class_for(&excs_tuple);
            let new_inst = make_exception_with_class(cls, "");
            if let Object::Instance(ni) = &new_inst {
                let mut d = ni.dict.borrow_mut();
                d.insert(
                    DictKey(Object::from_static("args")),
                    Object::new_tuple(vec![msg.clone(), Object::Tuple(excs_tuple.clone())]),
                );
                d.insert(DictKey(Object::from_static("message")), msg);
                d.insert(
                    DictKey(Object::from_static("exceptions")),
                    Object::Tuple(excs_tuple),
                );
            }
            return Ok(new_inst);
        }
        Ok(Object::None)
    }
    fn eg_split(args: &[Object]) -> Result<Object, RuntimeError> {
        let inst = args
            .first()
            .ok_or_else(|| crate::error::type_error("expected exception instance"))?;
        let pred = args
            .get(1)
            .cloned()
            .ok_or_else(|| crate::error::type_error("split requires a type argument"))?;
        let (m, r) = split_exception_group(inst, &pred)?;
        Ok(Object::new_tuple(vec![m, r]))
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
        let ctor_args = &args[1..];
        let excs = ctor_args
            .get(1)
            .cloned()
            .ok_or_else(|| crate::error::type_error("expected 2 arguments, got 1"))?;
        let items: Vec<Object> = match &excs {
            Object::Tuple(t) => t.to_vec(),
            Object::List(l) => l.borrow().clone(),
            _ => {
                return Err(crate::error::type_error(
                    "second argument (exceptions) must be a sequence",
                ))
            }
        };
        if items.is_empty() {
            return Err(crate::error::value_error(
                "second argument (exceptions) must be a non-empty sequence".to_owned(),
            ));
        }
        for (i, item) in items.iter().enumerate() {
            if !instance_is_subclass(item, &builtin_types().base_exception) {
                return Err(crate::error::value_error(format!(
                    "Item {i} of second argument (exceptions) is not an exception"
                )));
            }
        }
        let cls = resolve_exception_group_class(cls.clone(), ctor_args)?;
        let msg = ctor_args
            .first()
            .cloned()
            .unwrap_or(Object::from_static(""));
        let inst = make_exception_with_class(cls, "");
        if let Object::Instance(inst_rc) = &inst {
            let mut dict = inst_rc.dict.borrow_mut();
            dict.insert(
                DictKey(Object::from_static("args")),
                Object::new_tuple(vec![msg.clone(), excs]),
            );
            dict.insert(DictKey(Object::from_static("message")), msg);
            dict.insert(
                DictKey(Object::from_static("exceptions")),
                Object::new_tuple(items),
            );
        }
        Ok(inst)
    }
    fn eg_subgroup(args: &[Object]) -> Result<Object, RuntimeError> {
        let inst = args
            .first()
            .ok_or_else(|| crate::error::type_error("expected exception instance"))?;
        let pred = args
            .get(1)
            .cloned()
            .ok_or_else(|| crate::error::type_error("subgroup requires a type argument"))?;
        let (m, _) = split_exception_group(inst, &pred)?;
        Ok(m)
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

/// Enforce PEP 654's construction rules when instantiating exception
/// classes: lower a plain `BaseExceptionGroup` to `ExceptionGroup`
/// when every contained exception is an `Exception`, and refuse to
/// nest a bare `BaseException` inside an `ExceptionGroup` (subclass).
pub fn resolve_exception_group_class(
    cls: Rc<TypeObject>,
    args: &[Object],
) -> Result<Rc<TypeObject>, RuntimeError> {
    let bt = builtin_types();
    if !cls.is_subclass_of(&bt.base_exception_group) {
        return Ok(cls);
    }
    let items: Vec<Object> = match args.get(1) {
        Some(Object::Tuple(t)) => t.to_vec(),
        Some(Object::List(l)) => l.borrow().clone(),
        _ => return Ok(cls),
    };
    let all_exceptions = items.iter().all(|e| instance_is_subclass(e, &bt.exception));
    if Rc::ptr_eq(&cls, &bt.base_exception_group) {
        if all_exceptions {
            return Ok(bt.exception_group.clone());
        }
        return Ok(cls);
    }
    if cls.is_subclass_of(&bt.exception_group) && !all_exceptions {
        return Err(crate::error::type_error(
            "Cannot nest BaseExceptions in an ExceptionGroup",
        ));
    }
    Ok(cls)
}

/// `True` if `class` overrides `derive` somewhere below the builtin
/// `BaseExceptionGroup` implementation in its MRO.
fn overrides_eg_derive(class: &Rc<TypeObject>) -> bool {
    overrides_eg_method(class, "derive")
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
    split_exception_group_by(group, &|exc| exception_matches_type(exc, type_pred))
}

/// Predicate-based core of [`split_exception_group`]. Also used for
/// CPython's `exception_group_projection` (leaf-identity matching) in
/// the `except*` re-raise machinery.
pub fn split_exception_group_by(
    group: &Object,
    leaf_matches: &dyn Fn(&Object) -> bool,
) -> Result<(Object, Object), RuntimeError> {
    let (cls, message, excs) = match group {
        Object::Instance(inst) => {
            let dict = inst.dict.borrow();
            let msg = dict
                .get(&DictKey(Object::from_static("message")))
                .cloned()
                .unwrap_or(Object::from_static(""));
            let excs = match dict.get(&DictKey(Object::from_static("exceptions"))) {
                Some(Object::Tuple(t)) => t.to_vec(),
                _ => Vec::new(),
            };
            (inst.cls(), msg, excs)
        }
        _ => {
            return Err(crate::error::type_error(
                "split argument must be an exception group",
            ))
        }
    };
    let mut matched = Vec::new();
    let mut rest = Vec::new();
    for exc in excs {
        // For nested groups, recurse.
        let is_group = match &exc {
            Object::Instance(i) => is_subclass_by_name(&i.cls(), "BaseExceptionGroup"),
            _ => false,
        };
        if is_group && !leaf_matches(&exc) {
            let (m, r) = split_exception_group_by(&exc, leaf_matches)?;
            if !matches!(m, Object::None) {
                matched.push(m);
            }
            if !matches!(r, Object::None) {
                rest.push(r);
            }
        } else if leaf_matches(&exc) {
            matched.push(exc);
        } else {
            rest.push(exc);
        }
    }
    let derive_override = overrides_eg_derive(&cls);
    let mk = |items: Vec<Object>| -> Result<Object, RuntimeError> {
        if items.is_empty() {
            return Ok(Object::None);
        }
        let items_t = Object::new_tuple(items.clone());
        let new_inst = if derive_override {
            // Dispatch the subclass's own `derive(self, excs)`.
            let derive = cls
                .lookup("derive")
                .ok_or_else(|| crate::error::type_error("exception group lost its derive"))?;
            crate::builtins::reentrant_call(&derive, &[group.clone(), items_t.clone()])?
        } else {
            let new_cls = exception_group_class_for(&items);
            let ni = make_exception_with_class(new_cls, "");
            if let Object::Instance(inst_rc) = &ni {
                let mut d = inst_rc.dict.borrow_mut();
                d.insert(
                    DictKey(Object::from_static("args")),
                    Object::new_tuple(vec![message.clone(), items_t.clone()]),
                );
                d.insert(DictKey(Object::from_static("message")), message.clone());
                d.insert(DictKey(Object::from_static("exceptions")), items_t.clone());
            }
            ni
        };
        // CPython copies the chaining/traceback metadata from the
        // original group onto each derived part.
        if let (Object::Instance(src), Object::Instance(dst)) = (group, &new_inst) {
            let src_d = src.dict.borrow();
            let mut dst_d = dst.dict.borrow_mut();
            for key in ["__cause__", "__context__", "__traceback__", "__notes__"] {
                if let Some(v) = src_d.get(&DictKey(Object::from_static(key))) {
                    dst_d.insert(DictKey(Object::from_static(key)), v.clone());
                }
            }
        }
        Ok(new_inst)
    };
    Ok((mk(matched)?, mk(rest)?))
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
        let mut d = inst.dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("args")),
            Object::new_tuple(vec![Object::from_static(""), items_t.clone()]),
        );
        d.insert(
            DictKey(Object::from_static("message")),
            Object::from_static(""),
        );
        d.insert(DictKey(Object::from_static("exceptions")), items_t);
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
    let da = ia.dict.borrow();
    let db = ib.dict.borrow();
    for key in ["__notes__", "__traceback__", "__cause__", "__context__"] {
        let va = da.get(&DictKey(Object::from_static(key)));
        let vb = db.get(&DictKey(Object::from_static(key)));
        let same = match (va, vb) {
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
            let excs = inst
                .dict
                .borrow()
                .get(&DictKey(Object::from_static("exceptions")))
                .cloned();
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
    let (matched, _rest) = split_exception_group_by(orig, &|exc| match exc {
        Object::Instance(i) => ids.contains(&(Rc::as_ptr(i) as usize)),
        _ => false,
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
        let mut d = inst.dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("args")),
            Object::new_tuple(vec![Object::from_static(""), items_t.clone()]),
        );
        d.insert(
            DictKey(Object::from_static("message")),
            Object::from_static(""),
        );
        d.insert(DictKey(Object::from_static("exceptions")), items_t);
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
            let dict: crate::sync::Ref<'_, DictData> = inst.dict.borrow();
            if let Some(Object::Str(s)) = dict.get(&DictKey(Object::from_static("message"))) {
                return Some(s.to_string());
            }
            if let Some(Object::Tuple(items)) = dict.get(&DictKey(Object::from_static("args"))) {
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
        ty.dict
            .borrow_mut()
            .insert(DictKey(Object::from_static("__new__")), make_default_new());
    }
    install_mutable_container_init(bt);
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
            Some(Object::Instance(inst)) => match &inst.native {
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

    install(
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
                Object::Instance(inst) => inst.native.as_ref().unwrap_or(obj),
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
    install_method(&bt.int_, "__int__", self_as_int);
    install_method(&bt.int_, "__index__", self_as_int);
    install_method(&bt.float_, "__float__", self_as_float);
}

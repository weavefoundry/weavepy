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
use crate::object::{DictData, DictKey, Object};
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
    pub function_: Rc<TypeObject>,
    pub method_: Rc<TypeObject>,
    /// `builtin_function_or_method` — the type of Rust-implemented
    /// callables (`type(len)`), distinct from `function` as in CPython.
    pub builtin_function_: Rc<TypeObject>,
    /// `method-wrapper` — the type of a slot wrapper bound to an
    /// instance (`type(object().__str__)`).
    pub method_wrapper_: Rc<TypeObject>,
    pub generator_: Rc<TypeObject>,
    pub coroutine_: Rc<TypeObject>,
    pub async_generator_: Rc<TypeObject>,

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
    pub runtime_error: Rc<TypeObject>,
    pub stop_iteration: Rc<TypeObject>,
    pub stop_async_iteration: Rc<TypeObject>,
    pub syntax_error: Rc<TypeObject>,
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
    pub memory_error: Rc<TypeObject>,
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
        let type_ = mk("type", vec![object_.clone()]);
        let property_ = mk("property", vec![object_.clone()]);
        let staticmethod_ = mk("staticmethod", vec![object_.clone()]);
        let classmethod_ = mk("classmethod", vec![object_.clone()]);
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
        let generator_ = mk("generator", vec![object_.clone()]);
        let coroutine_ = mk("coroutine", vec![object_.clone()]);
        let async_generator_ = mk("async_generator", vec![object_.clone()]);
        let module_ = mk("module", vec![object_.clone()]);

        let base_exception = exc("BaseException", object_.clone());
        let exception = exc("Exception", base_exception.clone());

        // Hang `__str__` / `__repr__` off `BaseException` so that
        // `str(ValueError("msg"))` / `print(exc)` produce the
        // CPython-familiar message rather than the generic
        // "<X object at 0x...>" instance repr.
        install_exception_str_repr(&base_exception);

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
        let os_error = exc("OSError", exception.clone());
        install_os_error_init(&os_error);
        let runtime_error = exc("RuntimeError", exception.clone());
        let not_implemented_error = exc("NotImplementedError", runtime_error.clone());
        let recursion_error = exc("RecursionError", runtime_error.clone());
        let overflow_error = exc("OverflowError", arithmetic_error.clone());
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
        let memory_error = exc("MemoryError", exception.clone());

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
            function_,
            method_,
            builtin_function_,
            method_wrapper_,
            generator_,
            coroutine_,
            async_generator_,
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
            runtime_error,
            stop_iteration,
            stop_async_iteration,
            syntax_error,
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
            memory_error,
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
            pair!(runtime_error, "RuntimeError"),
            pair!(stop_iteration, "StopIteration"),
            pair!(stop_async_iteration, "StopAsyncIteration"),
            pair!(syntax_error, "SyntaxError"),
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
            pair!(memory_error, "MemoryError"),
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
            "RuntimeError" => Some(self.runtime_error.clone()),
            "StopIteration" => Some(self.stop_iteration.clone()),
            "StopAsyncIteration" => Some(self.stop_async_iteration.clone()),
            "SyntaxError" => Some(self.syntax_error.clone()),
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
            "MemoryError" => Some(self.memory_error.clone()),
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

/// Construct an exception instance of `class_name` with `message` as
/// `args[0]`. Used by Rust-side error helpers.
pub fn make_exception(class_name: &str, message: impl Into<String>) -> Object {
    let bt = builtin_types();
    let class = bt
        .by_name(class_name)
        .unwrap_or_else(|| bt.exception.clone());
    make_exception_with_class(class, message)
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
            Some(o) => o.native_value().unwrap_or_else(|| Object::Int(o.as_i64().unwrap_or(0))),
        });
    }
    if is_strict(&bt.float_) {
        let f = value.and_then(Object::as_f64).unwrap_or(0.0);
        return Some(Object::Float(f));
    }
    if is_strict(&bt.complex_) {
        return Some(match value {
            Some(c @ Object::Complex(_)) => c.clone(),
            Some(o) => o.native_value().filter(|n| matches!(n, Object::Complex(_))).unwrap_or(o.clone()),
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
            .and_then(concrete_elements)
            .map(|els| els.iter().filter_map(|o| o.as_i64()).map(|i| i as u8).collect())
            .unwrap_or_default();
        return Some(Object::ByteArray(Rc::new(RefCell::new(bytes))));
    }
    if is_strict(&bt.bytes_) {
        let bytes: Vec<u8> = value
            .and_then(concrete_elements)
            .map(|els| els.iter().filter_map(|o| o.as_i64()).map(|i| i as u8).collect())
            .unwrap_or_default();
        return Some(Object::Bytes(Rc::from(bytes.as_slice())));
    }
    if is_strict(&bt.tuple_) {
        let els = value.and_then(concrete_elements).unwrap_or_default();
        return Some(Object::new_tuple(els));
    }
    if is_strict(&bt.frozenset_) {
        let els = value.and_then(concrete_elements).unwrap_or_default();
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
    // When `cls` derives from a value/container built-in (`int`, `float`,
    // `str`, `tuple`, `list`, `dict`, …) capture the native payload the
    // instance wraps so the inherited protocols keep firing through the
    // subclass. `super().__new__(cls, value)` passes the seed value as the
    // second positional argument (how `copyreg.__newobj__` reconstructs
    // immutable subclasses); mutable containers start empty and are filled by
    // `__init__` / `__setstate__` / the `_reconstruct` append-and-update loop.
    if let Some(native) = native_seed_for_new(&cls, args.get(1)) {
        return Ok(Object::Instance(Rc::new(PyInstance::with_native(cls, native))));
    }
    Ok(Object::Instance(Rc::new(PyInstance::new(cls))))
}

/// A fresh `Object::StaticMethod(Builtin "__new__")` wrapping [`object_new`].
/// Each call returns a *distinct* object so `int.__new__ is object.__new__`
/// is `False` (matching CPython) while the instantiation path still treats it
/// as the default allocator (it keys on the builtin's `"__new__"` name).
fn make_default_new() -> Object {
    use crate::object::BuiltinFn;
    Object::StaticMethod(Rc::new(Object::Builtin(Rc::new(BuiltinFn {
        name: "__new__",
        call: Box::new(object_new),
        call_kw: None,
    }))))
}

/// Install `object.__new__`, `object.__init__`, `object.__setattr__`
/// and `object.__delattr__` on the root class. These are the implicit
/// base methods every user class inherits.
fn install_object_dunders(object_: &Rc<TypeObject>) {
    use crate::object::BuiltinFn;
    fn object_init(_args: &[Object]) -> Result<Object, RuntimeError> {
        // No-op; honours `super().__init__()` chains.
        Ok(Object::None)
    }
    fn object_setattr(args: &[Object]) -> Result<Object, RuntimeError> {
        // `object.__setattr__(self, name, value)` — write directly
        // to the instance dict, bypassing any user `__setattr__`
        // override on the receiver's class (used by dataclasses'
        // frozen __init__ to populate fields).
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
                inst.dict
                    .borrow_mut()
                    .insert(DictKey(Object::from_str(name)), args[2].clone());
                Ok(Object::None)
            }
            // `type.__setattr__` semantics for a class receiver — reached
            // via `super().__setattr__(…)` inside a metaclass override
            // (e.g. `EnumType.__setattr__` chaining to the default).
            Object::Type(ty) => {
                if ty.flags.is_builtin {
                    return Err(crate::error::type_error(format!(
                        "cannot set '{name}' attribute of immutable type '{}'",
                        ty.name
                    )));
                }
                ty.dict
                    .borrow_mut()
                    .insert(DictKey(Object::from_str(&name)), args[2].clone());
                if name == "__getattribute__" {
                    ty.invalidate_getattribute_cache();
                }
                Ok(Object::None)
            }
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
                let removed = inst
                    .dict
                    .borrow_mut()
                    .shift_remove(&DictKey(Object::from_str(&name)))
                    .is_some();
                if !removed {
                    return Err(crate::error::attribute_error(format!(
                        "'{}' object has no attribute '{}'",
                        inst.class.name, name
                    )));
                }
                Ok(Object::None)
            }
            // `type.__delattr__` semantics for a class receiver (chained
            // via `super().__delattr__(…)` in a metaclass override).
            Object::Type(ty) => {
                if ty.flags.is_builtin {
                    return Err(crate::error::type_error(format!(
                        "cannot delete '{name}' attribute of immutable type '{}'",
                        ty.name
                    )));
                }
                let removed = ty
                    .dict
                    .borrow_mut()
                    .shift_remove(&DictKey(Object::from_str(&name)))
                    .is_some();
                if !removed {
                    return Err(crate::error::attribute_error(format!(
                        "type object '{}' has no attribute '{}'",
                        ty.name, name
                    )));
                }
                Ok(Object::None)
            }
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
        let obj = args
            .first()
            .ok_or_else(|| crate::error::type_error("object.__hash__() takes exactly 1 argument"))?;
        crate::builtins::hash_object(obj)
    }
    let mut dict = object_.dict.borrow_mut();
    dict.insert(
        DictKey(Object::from_static("__hash__")),
        Object::Builtin(Rc::new(BuiltinFn {
            name: "__hash__",
            call: Box::new(object_hash),
            call_kw: None,
        })),
    );
    dict.insert(
        DictKey(Object::from_static("__new__")),
        make_default_new(),
    );
    dict.insert(
        DictKey(Object::from_static("__init__")),
        Object::Builtin(Rc::new(BuiltinFn {
            name: "__init__",
            call: Box::new(object_init),
            call_kw: None,
        })),
    );
    dict.insert(
        DictKey(Object::from_static("__setattr__")),
        Object::Builtin(Rc::new(BuiltinFn {
            name: "__setattr__",
            call: Box::new(object_setattr),
            call_kw: None,
        })),
    );
    dict.insert(
        DictKey(Object::from_static("__delattr__")),
        Object::Builtin(Rc::new(BuiltinFn {
            name: "__delattr__",
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
        Object::ClassMethod(Rc::new(Object::Builtin(Rc::new(BuiltinFn {
            name: "__init_subclass__",
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
        Object::ClassMethod(Rc::new(Object::Builtin(Rc::new(BuiltinFn {
            name: "__subclasshook__",
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
            call: Box::new(object_reduce_ex_sentinel),
            call_kw: None,
        })),
    );
    dict.insert(
        DictKey(Object::from_static("__reduce__")),
        Object::Builtin(Rc::new(BuiltinFn {
            name: ".object_reduce",
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
    let mut dict = type_.dict.borrow_mut();
    dict.insert(
        DictKey(Object::from_static("__new__")),
        Object::StaticMethod(Rc::new(Object::Builtin(Rc::new(BuiltinFn {
            name: "__new__",
            call: Box::new(type_new_sentinel),
            call_kw: None,
        })))),
    );
    dict.insert(
        DictKey(Object::from_static("__init__")),
        Object::Builtin(Rc::new(BuiltinFn {
            name: "__init__",
            call: Box::new(type_init),
            call_kw: None,
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
            dict.insert(
                DictKey(Object::from_static("args")),
                Object::new_tuple(rest.to_vec()),
            );
            let pick = |i: usize| rest.get(i).cloned().unwrap_or(Object::None);
            dict.insert(DictKey(Object::from_static("errno")), pick(0));
            dict.insert(DictKey(Object::from_static("strerror")), pick(1));
            dict.insert(DictKey(Object::from_static("filename")), pick(2));
            dict.insert(DictKey(Object::from_static("winerror")), pick(3));
            dict.insert(DictKey(Object::from_static("filename2")), pick(4));
        }
        Ok(Object::None)
    }
    let mut dict = os_error.dict.borrow_mut();
    dict.insert(
        DictKey(Object::from_static("__init__")),
        Object::Builtin(Rc::new(BuiltinFn {
            name: "__init__",
            call: Box::new(oserror_init),
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
        set(&mut dict, "object", rest[i].clone());
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
        let encoding = match get("encoding") {
            Some(Object::Str(s)) => s.to_string(),
            _ => String::new(),
        };
        let reason = match get("reason") {
            Some(Object::Str(s)) => s.to_string(),
            _ => String::new(),
        };
        let start = get("start").as_ref().map(as_i).unwrap_or(0);
        let end = get("end").as_ref().map(as_i).unwrap_or(0);
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
            call: Box::new(init),
            call_kw: None,
        })),
    );
    dict.insert(
        DictKey(Object::from_static("__str__")),
        Object::Builtin(Rc::new(BuiltinFn {
            name: "__str__",
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
        // `SyntaxError(msg, detail)` — `detail` is a 2-to-6 element
        // sequence `(filename, lineno, offset, text[, end_lineno,
        // end_offset])`.
        if rest.len() == 2 {
            let info: Option<&[Object]> = match &rest[1] {
                Object::Tuple(items) => Some(items.as_ref()),
                Object::List(items) => {
                    // Borrow can't outlive the match arm; handle inline.
                    let v = items.borrow();
                    let pick = |i: usize| v.get(i).cloned().unwrap_or(Object::None);
                    set(&mut dict, "filename", pick(0));
                    set(&mut dict, "lineno", pick(1));
                    set(&mut dict, "offset", pick(2));
                    set(&mut dict, "text", pick(3));
                    if v.len() > 4 {
                        set(&mut dict, "end_lineno", pick(4));
                        set(&mut dict, "end_offset", pick(5));
                    }
                    None
                }
                // Non-sequence second arg: CPython leaves the location
                // attributes at their `None` defaults.
                _ => None,
            };
            if let Some(items) = info {
                let pick = |i: usize| items.get(i).cloned().unwrap_or(Object::None);
                set(&mut dict, "filename", pick(0));
                set(&mut dict, "lineno", pick(1));
                set(&mut dict, "offset", pick(2));
                set(&mut dict, "text", pick(3));
                if items.len() > 4 {
                    set(&mut dict, "end_lineno", pick(4));
                    set(&mut dict, "end_offset", pick(5));
                }
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
            call: Box::new(syntaxerror_init),
            call_kw: None,
        })),
    );
    set(
        &mut dict,
        "__str__",
        Object::Builtin(Rc::new(BuiltinFn {
            name: "__str__",
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
            let is_key_error = is_subclass_by_name(&inst_rc.class, "KeyError");
            let dict = inst_rc.dict.borrow();
            if let Some(Object::Tuple(items)) = dict.get(&DictKey(Object::from_static("args"))) {
                return Ok(match items.as_ref() {
                    [] => Object::from_static(""),
                    [single] => {
                        if is_key_error {
                            Object::from_str(single.repr())
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
            let cls = inst_rc.class.name.clone();
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
            return Err(crate::error::type_error("note must be a str".to_owned()));
        }
        if let Object::Instance(inst_rc) = inst {
            let key = DictKey(Object::from_static("__notes__"));
            let mut dict = inst_rc.dict.borrow_mut();
            let mut notes = match dict.get(&key) {
                Some(Object::List(l)) => l.borrow().clone(),
                _ => Vec::new(),
            };
            notes.push(note.clone());
            dict.insert(key, Object::List(Rc::new(crate::sync::GilCell::new(notes))));
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
    let mut dict = base_exception.dict.borrow_mut();
    dict.insert(
        DictKey(Object::from_static("__init__")),
        Object::Builtin(Rc::new(BuiltinFn {
            name: "__init__",
            call: Box::new(exc_init),
            call_kw: None,
        })),
    );
    dict.insert(
        DictKey(Object::from_static("__str__")),
        Object::Builtin(Rc::new(BuiltinFn {
            name: "__str__",
            call: Box::new(exc_str),
            call_kw: None,
        })),
    );
    dict.insert(
        DictKey(Object::from_static("__repr__")),
        Object::Builtin(Rc::new(BuiltinFn {
            name: "__repr__",
            call: Box::new(exc_repr),
            call_kw: None,
        })),
    );
    dict.insert(
        DictKey(Object::from_static("add_note")),
        Object::Builtin(Rc::new(BuiltinFn {
            name: "add_note",
            call: Box::new(exc_add_note),
            call_kw: None,
        })),
    );
    dict.insert(
        DictKey(Object::from_static("with_traceback")),
        Object::Builtin(Rc::new(BuiltinFn {
            name: "with_traceback",
            call: Box::new(exc_with_traceback),
            call_kw: None,
        })),
    );
}

pub fn make_exception_with_class(class: Rc<TypeObject>, message: impl Into<String>) -> Object {
    use crate::types::PyInstance;
    let is_os = is_subclass_by_name(&class, "OSError");
    let is_syntax = is_subclass_by_name(&class, "SyntaxError");
    let inst = PyInstance::new(class);
    let msg = Object::from_str(message);
    let args = Object::new_tuple(vec![msg.clone()]);
    {
        let mut dict = inst.dict.borrow_mut();
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
            for name in ["filename", "lineno", "offset", "text", "end_lineno", "end_offset"] {
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
        let excs_tuple = match excs {
            Object::Tuple(items) => items,
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
            dict.insert(
                DictKey(Object::from_static("args")),
                Object::new_tuple(vec![msg.clone(), Object::Tuple(excs_tuple.clone())]),
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
        // derive(self, excs) -> new EG of the same class
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
            let cls = inst_rc.class.clone();
            let new_inst = make_exception_with_class(cls, "");
            if let Object::Instance(ni) = &new_inst {
                let excs_tuple = match excs {
                    Object::Tuple(t) => t,
                    Object::List(l) => Rc::from(l.borrow().clone().into_boxed_slice()),
                    _ => Rc::from(Vec::<Object>::new().into_boxed_slice()),
                };
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
            call: Box::new(eg_init),
            call_kw: None,
        })),
    );
    dict.insert(
        DictKey(Object::from_static("__str__")),
        Object::Builtin(Rc::new(BuiltinFn {
            name: "__str__",
            call: Box::new(eg_str),
            call_kw: None,
        })),
    );
    dict.insert(
        DictKey(Object::from_static("derive")),
        Object::Builtin(Rc::new(BuiltinFn {
            name: "derive",
            call: Box::new(eg_derive),
            call_kw: None,
        })),
    );
    dict.insert(
        DictKey(Object::from_static("split")),
        Object::Builtin(Rc::new(BuiltinFn {
            name: "split",
            call: Box::new(eg_split),
            call_kw: None,
        })),
    );
    dict.insert(
        DictKey(Object::from_static("subgroup")),
        Object::Builtin(Rc::new(BuiltinFn {
            name: "subgroup",
            call: Box::new(eg_subgroup),
            call_kw: None,
        })),
    );
}

/// Split an exception group instance against a type predicate. Used
/// by the VM's `CheckEGMatch` opcode and exposed via
/// `BaseExceptionGroup.split(typ)`.
///
/// Returns `(matched, rest)` where:
/// - `matched` is `None` if no contained exception matches, otherwise
///   a new exception group of the same class containing the matches.
/// - `rest` is `None` if every contained exception matches, otherwise
///   a new group with the non-matching ones.
pub fn split_exception_group(
    group: &Object,
    type_pred: &Object,
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
            (inst.class.clone(), msg, excs)
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
            Object::Instance(i) => is_subclass_by_name(&i.class, "BaseExceptionGroup"),
            _ => false,
        };
        if is_group {
            let (m, r) = split_exception_group(&exc, type_pred)?;
            if !matches!(m, Object::None) {
                matched.push(m);
            }
            if !matches!(r, Object::None) {
                rest.push(r);
            }
        } else if exception_matches_type(&exc, type_pred) {
            matched.push(exc);
        } else {
            rest.push(exc);
        }
    }
    let mk = |items: Vec<Object>| -> Object {
        if items.is_empty() {
            return Object::None;
        }
        let new_inst = make_exception_with_class(cls.clone(), "");
        if let Object::Instance(ni) = &new_inst {
            let mut d = ni.dict.borrow_mut();
            let items_t = Object::new_tuple(items);
            d.insert(
                DictKey(Object::from_static("args")),
                Object::new_tuple(vec![message.clone(), items_t.clone()]),
            );
            d.insert(DictKey(Object::from_static("message")), message.clone());
            d.insert(DictKey(Object::from_static("exceptions")), items_t);
        }
        new_inst
    };
    Ok((mk(matched), mk(rest)))
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
        Object::Instance(inst) => inst.class.is_subclass_of(cls),
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
            call: Box::new(f),
            call_kw: None,
        }));
        // Wrap as `classmethod` so descriptor binding skips the
        // instance and routes through the class.
        let cm = Object::ClassMethod(Rc::new(builtin));
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
            crate::builtins::hash_object(args.first().unwrap_or(&Object::None))
        }
        ty.dict.borrow_mut().insert(
            DictKey(Object::from_static("__hash__")),
            Object::Builtin(Rc::new(BuiltinFn {
                name: "__hash__",
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

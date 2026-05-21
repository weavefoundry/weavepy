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

use std::cell::RefCell;
use std::rc::Rc;

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
    pub str_: Rc<TypeObject>,
    pub bytes_: Rc<TypeObject>,
    pub bytearray_: Rc<TypeObject>,
    pub tuple_: Rc<TypeObject>,
    pub list_: Rc<TypeObject>,
    pub dict_: Rc<TypeObject>,
    pub set_: Rc<TypeObject>,
    pub frozenset_: Rc<TypeObject>,
    pub range_: Rc<TypeObject>,
    pub none_type: Rc<TypeObject>,
    pub function_: Rc<TypeObject>,
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
    pub zero_division_error: Rc<TypeObject>,
    pub generator_exit: Rc<TypeObject>,
    pub keyboard_interrupt: Rc<TypeObject>,
    pub system_exit: Rc<TypeObject>,
    pub recursion_error: Rc<TypeObject>,
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
        let str_ = mk("str", vec![object_.clone()]);
        let bytes_ = mk("bytes", vec![object_.clone()]);
        let bytearray_ = mk("bytearray", vec![object_.clone()]);
        let tuple_ = mk("tuple", vec![object_.clone()]);
        let list_ = mk("list", vec![object_.clone()]);
        let dict_ = mk("dict", vec![object_.clone()]);
        let set_ = mk("set", vec![object_.clone()]);
        let frozenset_ = mk("frozenset", vec![object_.clone()]);
        let range_ = mk("range", vec![object_.clone()]);
        let none_type = mk("NoneType", vec![object_.clone()]);
        let function_ = mk("function", vec![object_.clone()]);
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
        // `TimeoutError` lands here so `asyncio.wait_for` raises a
        // public, importable type rather than a synthetic shim.
        let timeout_error = exc("TimeoutError", os_error.clone());
        let type_error = exc("TypeError", exception.clone());
        let value_error = exc("ValueError", exception.clone());
        let generator_exit = exc("GeneratorExit", base_exception.clone());
        let keyboard_interrupt = exc("KeyboardInterrupt", base_exception.clone());
        let system_exit = exc("SystemExit", base_exception.clone());

        let bt = BuiltinTypes {
            object_: object_.clone(),
            type_: type_.clone(),
            property_: property_.clone(),
            staticmethod_: staticmethod_.clone(),
            classmethod_: classmethod_.clone(),
            int_,
            float_,
            bool_,
            str_,
            bytes_,
            bytearray_,
            tuple_,
            list_,
            dict_,
            set_,
            frozenset_,
            range_,
            none_type,
            function_,
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
            zero_division_error,
            generator_exit,
            keyboard_interrupt,
            system_exit,
            recursion_error,
        };
        // Every other built-in type's metaclass is `type`.
        for (_, value) in bt.as_globals() {
            if let Object::Type(t) = value {
                if t.metaclass.borrow().is_none() {
                    t.set_metaclass(type_.clone());
                }
            }
        }
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
            pair!(str_, "str"),
            pair!(bytes_, "bytes"),
            pair!(bytearray_, "bytearray"),
            pair!(tuple_, "tuple"),
            pair!(list_, "list"),
            pair!(dict_, "dict"),
            pair!(set_, "set"),
            pair!(frozenset_, "frozenset"),
            pair!(range_, "range"),
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
            pair!(zero_division_error, "ZeroDivisionError"),
            pair!(generator_exit, "GeneratorExit"),
            pair!(keyboard_interrupt, "KeyboardInterrupt"),
            pair!(system_exit, "SystemExit"),
            pair!(recursion_error, "RecursionError"),
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
            "str" => Some(self.str_.clone()),
            "bytes" => Some(self.bytes_.clone()),
            "bytearray" => Some(self.bytearray_.clone()),
            "tuple" => Some(self.tuple_.clone()),
            "list" => Some(self.list_.clone()),
            "dict" => Some(self.dict_.clone()),
            "set" => Some(self.set_.clone()),
            "frozenset" => Some(self.frozenset_.clone()),
            "range" => Some(self.range_.clone()),
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
            "ZeroDivisionError" => Some(self.zero_division_error.clone()),
            "GeneratorExit" => Some(self.generator_exit.clone()),
            "KeyboardInterrupt" => Some(self.keyboard_interrupt.clone()),
            "SystemExit" => Some(self.system_exit.clone()),
            "RecursionError" => Some(self.recursion_error.clone()),
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

/// Install `object.__new__`, `object.__init__`, `object.__setattr__`
/// and `object.__delattr__` on the root class. These are the implicit
/// base methods every user class inherits.
fn install_object_dunders(object_: &Rc<TypeObject>) {
    use crate::object::BuiltinFn;
    use crate::types::PyInstance;
    fn object_new(args: &[Object]) -> Result<Object, RuntimeError> {
        // `object.__new__(cls, *args, **kwargs)` — args[0] is `cls`.
        let cls = match args.first() {
            Some(Object::Type(t)) => t.clone(),
            _ => {
                return Err(crate::error::type_error(
                    "object.__new__(): first arg must be a class".to_owned(),
                ))
            }
        };
        Ok(Object::Instance(Rc::new(PyInstance::new(cls))))
    }
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
        let inst = match &args[0] {
            Object::Instance(i) => i.clone(),
            other => {
                return Err(crate::error::type_error(format!(
                    "object.__setattr__() requires an instance, got '{}'",
                    other.type_name()
                )))
            }
        };
        let name = match &args[1] {
            Object::Str(s) => s.to_string(),
            _ => return Err(crate::error::type_error("attribute name must be str")),
        };
        inst.dict
            .borrow_mut()
            .insert(DictKey(Object::from_str(name)), args[2].clone());
        Ok(Object::None)
    }
    fn object_delattr(args: &[Object]) -> Result<Object, RuntimeError> {
        if args.len() != 2 {
            return Err(crate::error::type_error(
                "object.__delattr__() takes 2 arguments".to_owned(),
            ));
        }
        let inst = match &args[0] {
            Object::Instance(i) => i.clone(),
            other => {
                return Err(crate::error::type_error(format!(
                    "object.__delattr__() requires an instance, got '{}'",
                    other.type_name()
                )))
            }
        };
        let name = match &args[1] {
            Object::Str(s) => s.to_string(),
            _ => return Err(crate::error::type_error("attribute name must be str")),
        };
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
    let mut dict = object_.dict.borrow_mut();
    dict.insert(
        DictKey(Object::from_static("__new__")),
        Object::StaticMethod(Rc::new(Object::Builtin(Rc::new(BuiltinFn {
            name: "__new__",
            call: Box::new(object_new),
        })))),
    );
    dict.insert(
        DictKey(Object::from_static("__init__")),
        Object::Builtin(Rc::new(BuiltinFn {
            name: "__init__",
            call: Box::new(object_init),
        })),
    );
    dict.insert(
        DictKey(Object::from_static("__setattr__")),
        Object::Builtin(Rc::new(BuiltinFn {
            name: "__setattr__",
            call: Box::new(object_setattr),
        })),
    );
    dict.insert(
        DictKey(Object::from_static("__delattr__")),
        Object::Builtin(Rc::new(BuiltinFn {
            name: "__delattr__",
            call: Box::new(object_delattr),
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
        })))),
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
        })))),
    );
    dict.insert(
        DictKey(Object::from_static("__init__")),
        Object::Builtin(Rc::new(BuiltinFn {
            name: "__init__",
            call: Box::new(type_init),
        })),
    );
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
            let dict = inst_rc.dict.borrow();
            if let Some(Object::Tuple(items)) = dict.get(&DictKey(Object::from_static("args"))) {
                return Ok(match items.as_ref() {
                    [] => Object::from_static(""),
                    [single] => Object::from_str(single.to_str()),
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
    let mut dict = base_exception.dict.borrow_mut();
    dict.insert(
        DictKey(Object::from_static("__init__")),
        Object::Builtin(Rc::new(BuiltinFn {
            name: "__init__",
            call: Box::new(exc_init),
        })),
    );
    dict.insert(
        DictKey(Object::from_static("__str__")),
        Object::Builtin(Rc::new(BuiltinFn {
            name: "__str__",
            call: Box::new(exc_str),
        })),
    );
    dict.insert(
        DictKey(Object::from_static("__repr__")),
        Object::Builtin(Rc::new(BuiltinFn {
            name: "__repr__",
            call: Box::new(exc_repr),
        })),
    );
}

pub fn make_exception_with_class(class: Rc<TypeObject>, message: impl Into<String>) -> Object {
    use crate::types::PyInstance;
    let inst = PyInstance::new(class);
    let msg = Object::from_str(message);
    let args = Object::new_tuple(vec![msg.clone()]);
    inst.dict
        .borrow_mut()
        .insert(DictKey(Object::from_static("args")), args);
    inst.dict
        .borrow_mut()
        .insert(DictKey(Object::from_static("message")), msg);
    Object::Instance(Rc::new(inst))
}

/// Extract the "message" of an exception instance — used by the
/// error formatter.
pub fn exception_message(obj: &Object) -> Option<String> {
    match obj {
        Object::Instance(inst) => {
            let dict: std::cell::Ref<'_, DictData> = inst.dict.borrow();
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

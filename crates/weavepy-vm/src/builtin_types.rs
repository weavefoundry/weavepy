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

use crate::object::{DictData, DictKey, Object};
use crate::types::TypeObject;

/// All built-in classes, kept in one place so calls like
/// `BuiltinTypes::type_error()` are constant-time.
#[allow(missing_debug_implementations)]
pub struct BuiltinTypes {
    pub object_: Rc<TypeObject>,
    pub type_: Rc<TypeObject>,

    pub int_: Rc<TypeObject>,
    pub float_: Rc<TypeObject>,
    pub bool_: Rc<TypeObject>,
    pub str_: Rc<TypeObject>,
    pub bytes_: Rc<TypeObject>,
    pub tuple_: Rc<TypeObject>,
    pub list_: Rc<TypeObject>,
    pub dict_: Rc<TypeObject>,
    pub set_: Rc<TypeObject>,
    pub range_: Rc<TypeObject>,
    pub none_type: Rc<TypeObject>,
    pub function_: Rc<TypeObject>,

    pub base_exception: Rc<TypeObject>,
    pub exception: Rc<TypeObject>,
    pub arithmetic_error: Rc<TypeObject>,
    pub assertion_error: Rc<TypeObject>,
    pub attribute_error: Rc<TypeObject>,
    pub index_error: Rc<TypeObject>,
    pub key_error: Rc<TypeObject>,
    pub lookup_error: Rc<TypeObject>,
    pub name_error: Rc<TypeObject>,
    pub not_implemented_error: Rc<TypeObject>,
    pub overflow_error: Rc<TypeObject>,
    pub runtime_error: Rc<TypeObject>,
    pub stop_iteration: Rc<TypeObject>,
    pub syntax_error: Rc<TypeObject>,
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

        let int_ = mk("int", vec![object_.clone()]);
        let float_ = mk("float", vec![object_.clone()]);
        let bool_ = mk("bool", vec![int_.clone()]);
        let str_ = mk("str", vec![object_.clone()]);
        let bytes_ = mk("bytes", vec![object_.clone()]);
        let tuple_ = mk("tuple", vec![object_.clone()]);
        let list_ = mk("list", vec![object_.clone()]);
        let dict_ = mk("dict", vec![object_.clone()]);
        let set_ = mk("set", vec![object_.clone()]);
        let range_ = mk("range", vec![object_.clone()]);
        let none_type = mk("NoneType", vec![object_.clone()]);
        let function_ = mk("function", vec![object_.clone()]);

        let base_exception = exc("BaseException", object_.clone());
        let exception = exc("Exception", base_exception.clone());

        let arithmetic_error = exc("ArithmeticError", exception.clone());
        let assertion_error = exc("AssertionError", exception.clone());
        let attribute_error = exc("AttributeError", exception.clone());
        let lookup_error = exc("LookupError", exception.clone());
        let index_error = exc("IndexError", lookup_error.clone());
        let key_error = exc("KeyError", lookup_error.clone());
        let name_error = exc("NameError", exception.clone());
        let unbound_local_error = exc("UnboundLocalError", name_error.clone());
        let runtime_error = exc("RuntimeError", exception.clone());
        let not_implemented_error = exc("NotImplementedError", runtime_error.clone());
        let recursion_error = exc("RecursionError", runtime_error.clone());
        let overflow_error = exc("OverflowError", arithmetic_error.clone());
        let zero_division_error = exc("ZeroDivisionError", arithmetic_error.clone());
        let stop_iteration = exc("StopIteration", exception.clone());
        let syntax_error = exc("SyntaxError", exception.clone());
        let type_error = exc("TypeError", exception.clone());
        let value_error = exc("ValueError", exception.clone());
        let generator_exit = exc("GeneratorExit", base_exception.clone());
        let keyboard_interrupt = exc("KeyboardInterrupt", base_exception.clone());
        let system_exit = exc("SystemExit", base_exception.clone());

        BuiltinTypes {
            object_,
            type_,
            int_,
            float_,
            bool_,
            str_,
            bytes_,
            tuple_,
            list_,
            dict_,
            set_,
            range_,
            none_type,
            function_,
            base_exception,
            exception,
            arithmetic_error,
            assertion_error,
            attribute_error,
            index_error,
            key_error,
            lookup_error,
            name_error,
            not_implemented_error,
            overflow_error,
            runtime_error,
            stop_iteration,
            syntax_error,
            type_error,
            unbound_local_error,
            value_error,
            zero_division_error,
            generator_exit,
            keyboard_interrupt,
            system_exit,
            recursion_error,
        }
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
            pair!(int_, "int"),
            pair!(float_, "float"),
            pair!(bool_, "bool"),
            pair!(str_, "str"),
            pair!(bytes_, "bytes"),
            pair!(tuple_, "tuple"),
            pair!(list_, "list"),
            pair!(dict_, "dict"),
            pair!(set_, "set"),
            pair!(range_, "range"),
            pair!(base_exception, "BaseException"),
            pair!(exception, "Exception"),
            pair!(arithmetic_error, "ArithmeticError"),
            pair!(assertion_error, "AssertionError"),
            pair!(attribute_error, "AttributeError"),
            pair!(index_error, "IndexError"),
            pair!(key_error, "KeyError"),
            pair!(lookup_error, "LookupError"),
            pair!(name_error, "NameError"),
            pair!(not_implemented_error, "NotImplementedError"),
            pair!(overflow_error, "OverflowError"),
            pair!(runtime_error, "RuntimeError"),
            pair!(stop_iteration, "StopIteration"),
            pair!(syntax_error, "SyntaxError"),
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
            "tuple" => Some(self.tuple_.clone()),
            "list" => Some(self.list_.clone()),
            "dict" => Some(self.dict_.clone()),
            "set" => Some(self.set_.clone()),
            "range" => Some(self.range_.clone()),
            "BaseException" => Some(self.base_exception.clone()),
            "Exception" => Some(self.exception.clone()),
            "ArithmeticError" => Some(self.arithmetic_error.clone()),
            "AssertionError" => Some(self.assertion_error.clone()),
            "AttributeError" => Some(self.attribute_error.clone()),
            "IndexError" => Some(self.index_error.clone()),
            "KeyError" => Some(self.key_error.clone()),
            "LookupError" => Some(self.lookup_error.clone()),
            "NameError" => Some(self.name_error.clone()),
            "NotImplementedError" => Some(self.not_implemented_error.clone()),
            "OverflowError" => Some(self.overflow_error.clone()),
            "RuntimeError" => Some(self.runtime_error.clone()),
            "StopIteration" => Some(self.stop_iteration.clone()),
            "SyntaxError" => Some(self.syntax_error.clone()),
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

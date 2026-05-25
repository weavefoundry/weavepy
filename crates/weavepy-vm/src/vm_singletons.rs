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

/// CPython's `help`/`copyright`/`license`/`credits` builtins are
/// `_Printer` instances: `repr(copyright)` returns the body, but
/// `copyright()` also prints it. We model them as
/// `builtin_function_or_method` callables that print + return None.
pub fn interactive_printer(name: &'static str, body: &'static str) -> Object {
    use crate::object::BuiltinFn;
    let body_for_repr = body.to_owned();
    let body_for_call = body.to_owned();
    let f = BuiltinFn {
        name,
        call: Box::new(move |_args: &[Object]| {
            // We can't reach the interpreter's stdout from a static
            // builtin; route through Rust's stdout for the
            // interactive case. Tests/REPL go through `print`, which
            // uses the configured sink.
            println!("{}", body_for_call);
            Ok(Object::None)
        }),
    };
    let printer = Object::Builtin(Rc::new(f));
    // Store the message as a side-channel for the VM to surface via
    // repr if it ever cares; for now repr falls back to the
    // builtin's default "<built-in function ...>".
    let _ = body_for_repr;
    printer
}

/// `quit` and `exit` — interactive sentinels that raise `SystemExit`.
pub fn quitter(name: &'static str) -> Object {
    use crate::object::BuiltinFn;
    let f = BuiltinFn {
        name,
        call: Box::new(|args: &[Object]| {
            let code = args.first().cloned().unwrap_or(Object::None);
            let bt = crate::builtin_types::builtin_types();
            let inst = crate::builtin_types::make_exception_with_class(
                bt.system_exit.clone(),
                code.to_str(),
            );
            if let Object::Instance(inst_rc) = &inst {
                inst_rc.dict.borrow_mut().insert(
                    crate::object::DictKey(Object::from_static("code")),
                    code.clone(),
                );
                inst_rc.dict.borrow_mut().insert(
                    crate::object::DictKey(Object::from_static("args")),
                    Object::new_tuple(vec![code]),
                );
            }
            Err(crate::error::RuntimeError::PyException(
                crate::error::PyException::new(inst),
            ))
        }),
    };
    Object::Builtin(Rc::new(f))
}

//! The `atexit` module — RFC 0023.
//!
//! Registers callables to run on interpreter shutdown. We keep the
//! list in a thread-local; the CLI driver calls `run_handlers` at
//! exit time after the main module returns.

use crate::sync::Rc;
use crate::sync::RefCell;

use crate::error::{type_error, RuntimeError};
use crate::import::ModuleCache;
use crate::object::{BuiltinFn, DictData, DictKey, Object, PyModule};

thread_local! {
    static HANDLERS: RefCell<Vec<(Object, Vec<Object>, Vec<(String, Object)>)>> =
        const { RefCell::new(Vec::new()) };
}

pub fn build(_cache: &ModuleCache) -> Rc<PyModule> {
    let dict = Rc::new(RefCell::new(DictData::new()));
    {
        let mut d = dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("__name__")),
            Object::from_static("atexit"),
        );
        d.insert(
            DictKey(Object::from_static("register")),
            builtin("register", a_register),
        );
        d.insert(
            DictKey(Object::from_static("unregister")),
            builtin("unregister", a_unregister),
        );
        d.insert(
            DictKey(Object::from_static("_run_exitfuncs")),
            builtin("_run_exitfuncs", a_run_exitfuncs),
        );
        d.insert(
            DictKey(Object::from_static("_clear")),
            builtin("_clear", a_clear),
        );
        d.insert(
            DictKey(Object::from_static("_ncallbacks")),
            builtin("_ncallbacks", a_ncallbacks),
        );
    }
    Rc::new(PyModule {
        name: "atexit".to_owned(),
        filename: None,
        dict,
    })
}

fn builtin(name: &'static str, body: fn(&[Object]) -> Result<Object, RuntimeError>) -> Object {
    Object::Builtin(Rc::new(BuiltinFn {
        name,
        call: Box::new(body),
    }))
}

fn a_register(args: &[Object]) -> Result<Object, RuntimeError> {
    let func = args
        .first()
        .cloned()
        .ok_or_else(|| type_error("atexit.register() requires a callable"))?;
    let positional = args.get(1..).map(|s| s.to_vec()).unwrap_or_default();
    HANDLERS.with(|h| h.borrow_mut().push((func.clone(), positional, Vec::new())));
    Ok(func)
}

fn a_unregister(args: &[Object]) -> Result<Object, RuntimeError> {
    let func = args
        .first()
        .ok_or_else(|| type_error("atexit.unregister() requires a callable"))?;
    HANDLERS.with(|h| {
        h.borrow_mut().retain(|(f, _, _)| !f.is_same(func));
    });
    Ok(Object::None)
}

fn a_run_exitfuncs(_args: &[Object]) -> Result<Object, RuntimeError> {
    // We can't actually run the callables from this static fn because
    // we don't have a Vm handle. The CLI driver harvests the list via
    // `take_handlers` below and runs them with the interpreter state.
    Ok(Object::None)
}

fn a_clear(_args: &[Object]) -> Result<Object, RuntimeError> {
    HANDLERS.with(|h| h.borrow_mut().clear());
    Ok(Object::None)
}

fn a_ncallbacks(_args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(Object::Int(HANDLERS.with(|h| h.borrow().len() as i64)))
}

/// Drain the registered handlers in LIFO order. Called by the CLI
/// shutdown sequence. The caller invokes each `(func, args, kwargs)`
/// triple in turn.
pub fn take_handlers() -> Vec<(Object, Vec<Object>, Vec<(String, Object)>)> {
    HANDLERS.with(|h| {
        let mut v = h.borrow_mut();
        let drained: Vec<_> = v.drain(..).collect();
        drained.into_iter().rev().collect()
    })
}

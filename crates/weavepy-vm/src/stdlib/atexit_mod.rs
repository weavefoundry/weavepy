//! The `atexit` module — RFC 0023.
//!
//! Registers callables to run on interpreter shutdown. We keep the
//! list in a thread-local. There are two drains, sharing the same
//! `take_handlers` storage so a handler never runs twice:
//!   * the CLI driver runs whatever remains at real interpreter exit
//!     (after `__main__` returns), and
//!   * `_run_exitfuncs()` runs them on demand — `test_atexit` and
//!     `multiprocessing.popen_fork`'s forked child both call it
//!     explicitly before `os._exit`.

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
        binds_instance: false,
        call: Box::new(body),
        call_kw: None,
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
    // CPython's `atexit._run_exitfuncs()` (`Modules/atexitmodule.c`
    // `atexit_callfuncs`): invoke every registered callback in LIFO order,
    // *clearing* the registry as it goes, and report any callback error
    // through `sys.unraisablehook` rather than propagating it (so one bad
    // handler can't abort the rest).
    //
    // This is reachable two ways and both depend on it actually running the
    // callables — until now it was a silent no-op:
    //   * `test_atexit` calls `atexit._run_exitfuncs()` directly;
    //   * `multiprocessing.popen_fork`'s forked child calls it in a
    //     `finally` immediately before `os._exit(code)`. That's what runs
    //     the `Queue` feeder's `Finalize` (send-sentinel + join-thread), so
    //     the daemon feeder flushes its buffer to the pipe before the child
    //     dies. Without it the child `os._exit`s mid-flush and the parent's
    //     `Queue.get()` sees nothing.
    // The normal full-shutdown path still drains any *remaining* handlers
    // via `take_handlers` in the CLI driver; `take_handlers` here makes the
    // two paths share one drain so handlers never run twice.
    let handlers = take_handlers();
    if handlers.is_empty() {
        return Ok(Object::None);
    }
    let ptr = crate::vm_singletons::current_interpreter_ptr().ok_or_else(|| {
        crate::error::runtime_error("atexit._run_exitfuncs(): no running interpreter")
    })?;
    // SAFETY: the pointer was published by an enclosing VM frame still live
    // on this thread (we were called through VM dispatch); the GIL keeps the
    // access exclusive.
    let interp = unsafe { &mut *ptr };
    for (func, args, kwargs) in handlers {
        if let Err(err) = interp.call_object(func.clone(), &args, &kwargs) {
            let is_exit = matches!(&err,
                RuntimeError::PyException(exc) if exc.system_exit_code().is_some());
            if !is_exit {
                let context_repr = func.repr();
                interp.write_unraisable_msg(&err, &func, &context_repr, None);
            }
        }
    }
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

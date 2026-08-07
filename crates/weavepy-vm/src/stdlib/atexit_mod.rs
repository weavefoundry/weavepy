//! The `atexit` module — RFC 0023, event-exactness per RFC 0057 WS6.
//!
//! Mirrors CPython 3.13's `Modules/atexitmodule.c`: the registry is a
//! slot array in *registration order* where `unregister`/`_run_exitfuncs`
//! null out slots rather than compacting, so an `__eq__` or callback
//! that re-enters `unregister`/`_clear` mid-iteration (gh-112127,
//! bpo-46025) sees a stable indexing scheme, exactly like the C
//! `state->callbacks` array.

use crate::sync::Rc;
use crate::sync::RefCell;

use crate::error::{type_error, RuntimeError};
use crate::import::ModuleCache;
use crate::object::{BuiltinFn, DictData, DictKey, Object, PyModule};
use weavepy_compiler::CompareKind;

type Callback = (Object, Vec<Object>, Vec<(String, Object)>);

thread_local! {
    /// Registration-order slots; `None` = deleted (CPython's NULL holes).
    static HANDLERS: RefCell<Vec<Option<Callback>>> = const { RefCell::new(Vec::new()) };
}

pub fn build(_cache: &ModuleCache) -> Rc<PyModule> {
    let dict = Rc::new(RefCell::new(DictData::default()));
    {
        let mut d = dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("__name__")),
            Object::from_static("atexit"),
        );
        // `register` takes `func, *args, **kwargs` (atexit_register).
        d.insert(
            DictKey(Object::from_static("register")),
            Object::Builtin(Rc::new(BuiltinFn {
                name: "register",
                binds_instance: false,
                call: Box::new(|args| a_register(args, &[])),
                call_kw: Some(Box::new(a_register)),
            })),
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

fn current_interp(what: &str) -> Result<&'static mut crate::Interpreter, RuntimeError> {
    let ptr = crate::vm_singletons::current_interpreter_ptr()
        .ok_or_else(|| crate::error::runtime_error(format!("{what}: no running interpreter")))?;
    // SAFETY: the pointer was published by an enclosing VM frame still live
    // on this thread (we were called through VM dispatch); the GIL keeps the
    // access exclusive.
    Ok(unsafe { &mut *ptr })
}

fn a_register(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let func = args
        .first()
        .cloned()
        .ok_or_else(|| type_error("register() takes at least 1 argument (0 given)"))?;
    let positional = args.get(1..).map(|s| s.to_vec()).unwrap_or_default();
    HANDLERS.with(|h| {
        h.borrow_mut()
            .push(Some((func.clone(), positional, kwargs.to_vec())));
    });
    Ok(func)
}

fn a_unregister(args: &[Object]) -> Result<Object, RuntimeError> {
    let func = args
        .first()
        .cloned()
        .ok_or_else(|| type_error("unregister() takes exactly one argument (0 given)"))?;
    let interp = current_interp("atexit.unregister()")?;
    // CPython `atexit_unregister`: walk slots 0..ncallbacks comparing each
    // live entry with `PyObject_RichCompareBool(cb->func, func, Py_EQ)` and
    // null every match. The `__eq__` call may re-enter `unregister`/`_clear`
    // (gh-112127), so re-read length and slot state on every step and never
    // hold the registry borrow across the comparison.
    let mut i = 0usize;
    loop {
        let stored = HANDLERS.with(|h| {
            let v = h.borrow();
            if i >= v.len() {
                None
            } else {
                Some(v[i].as_ref().map(|(f, _, _)| f.clone()))
            }
        });
        let stored = match stored {
            None => break, // past the end
            Some(None) => {
                i += 1;
                continue; // deleted slot
            }
            Some(Some(f)) => f,
        };
        // `PyObject_RichCompareBool` identity shortcut for Py_EQ, then the
        // full forward/reflected `__eq__` protocol.
        let eq = if stored.is_same(&func) {
            true
        } else {
            interp
                .rich_compare_public(&stored, &func, CompareKind::Eq)?
                .is_truthy()
        };
        if eq {
            HANDLERS.with(|h| {
                let mut v = h.borrow_mut();
                if i < v.len() {
                    v[i] = None;
                }
            });
        }
        i += 1;
    }
    Ok(Object::None)
}

fn a_run_exitfuncs(_args: &[Object]) -> Result<Object, RuntimeError> {
    // CPython's `atexit._run_exitfuncs()` (`Modules/atexitmodule.c`
    // `atexit_callfuncs`). Reachable two ways:
    //   * `test_atexit` calls `atexit._run_exitfuncs()` directly;
    //   * `multiprocessing.popen_fork`'s forked child calls it in a
    //     `finally` immediately before `os._exit(code)` to flush the
    //     `Queue` feeder before the child dies.
    let interp = current_interp("atexit._run_exitfuncs()")?;
    run_exit_handlers(interp);
    Ok(Object::None)
}

fn a_clear(_args: &[Object]) -> Result<Object, RuntimeError> {
    HANDLERS.with(|h| h.borrow_mut().clear());
    Ok(Object::None)
}

fn a_ncallbacks(_args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(Object::Int(HANDLERS.with(|h| {
        h.borrow().iter().filter(|s| s.is_some()).count() as i64
    })))
}

/// CPython `atexit_callfuncs`: walk the slots from the highest index at
/// entry down to 0 (LIFO), re-reading each slot at its turn so a callback
/// that `unregister`s a not-yet-run entry suppresses it, and a callback
/// that unregisters *itself* still runs to completion (bpo-46025: the C
/// code takes `Py_NewRef(cb->func)` before the call). A failing callback
/// is reported through `sys.unraisablehook` with `object=None` and
/// `err_msg="Exception ignored in atexit callback {func!r}"`
/// (`PyErr_FormatUnraisable`); 3.13 does *not* special-case `SystemExit`.
/// Afterwards the whole registry is cleared (`atexit_cleanup`), including
/// entries registered by the callbacks themselves.
///
/// Called both by `atexit._run_exitfuncs()` and by the interpreter's
/// shutdown sequence (`_PyAtExit_Call`); the shared registry means a
/// handler never runs twice.
pub fn run_exit_handlers(interp: &mut crate::Interpreter) {
    let start = HANDLERS.with(|h| h.borrow().len());
    for i in (0..start).rev() {
        let cb = HANDLERS.with(|h| {
            let v = h.borrow();
            if i < v.len() {
                v[i].clone()
            } else {
                None
            }
        });
        let Some((func, args, kwargs)) = cb else {
            continue;
        };
        if let Err(err) = interp.call_object(func.clone(), &args, &kwargs) {
            let func_repr = interp.repr_object(&func).unwrap_or_else(|_| func.repr());
            let err_msg = format!("Exception ignored in atexit callback {func_repr}");
            interp.write_unraisable_msg(&err, &Object::None, &func_repr, Some(&err_msg));
        }
    }
    HANDLERS.with(|h| h.borrow_mut().clear());
}

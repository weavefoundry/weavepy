//! The `_itertools` built-in module — native cores for `itertools`.
//!
//! CPython implements itertools in C: its adapters are plain iterator
//! objects whose stepping pushes no Python frame. The frozen Python
//! `itertools` module prefers these natives and falls back to its
//! generator implementations for the rest. Frame-neutral stepping is
//! load-bearing for `traceback.walk_stack`, which hardcodes how many
//! `f_back` hops separate it from its caller — a Python-level `islice`
//! in `StackSummary.extract` would skew the chain.

use crate::sync::Rc;
use crate::sync::RefCell;

use crate::error::{type_error, value_error, RuntimeError};
use crate::import::ModuleCache;
use crate::object::{
    BuiltinFn, DictData, DictKey, LazyIterKind, Object, PyLazyIter, PyModule,
};

pub fn build(_cache: &ModuleCache) -> Rc<PyModule> {
    let dict = Rc::new(RefCell::new(DictData::new()));
    {
        let mut d = dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("__name__")),
            Object::from_static("_itertools"),
        );
        d.insert(
            DictKey(Object::from_static("__doc__")),
            Object::from_static("Functional tools for creating and using iterators — native core."),
        );
        d.insert(
            DictKey(Object::from_static("islice")),
            Object::Builtin(Rc::new(BuiltinFn {
                name: "islice",
                binds_instance: false,
                call: Box::new(islice),
                call_kw: None,
            })),
        );
    }
    Rc::new(PyModule {
        name: "_itertools".to_owned(),
        filename: None,
        dict,
    })
}

/// One `islice` index argument: `None` or an int in `0..=isize::MAX`.
fn islice_index(arg: &Object, what: &str) -> Result<Option<u64>, RuntimeError> {
    match arg {
        Object::None => Ok(None),
        Object::Int(i) if *i >= 0 => Ok(Some(*i as u64)),
        Object::Int(_) | Object::Long(_) => Err(value_error(format!(
            "{what} for islice() must be None or an integer: 0 <= x <= sys.maxsize."
        ))),
        _ => Err(value_error(format!(
            "{what} for islice() must be None or an integer: 0 <= x <= sys.maxsize."
        ))),
    }
}

/// `islice(iterable, stop)` / `islice(iterable, start, stop[, step])`.
fn islice(args: &[Object]) -> Result<Object, RuntimeError> {
    let Some(ptr) = crate::vm_singletons::current_interpreter_ptr() else {
        return Err(type_error("islice() requires a running interpreter"));
    };
    // SAFETY: published by the enclosing VM frame on this thread.
    let interp = unsafe { &mut *ptr };

    let (iterable, rest) = match args {
        [it, rest @ ..] if (1..=3).contains(&rest.len()) => (it, rest),
        _ => {
            return Err(type_error(format!(
                "islice expected 2 to 4 arguments, got {}",
                args.len()
            )))
        }
    };

    let (start, stop, step) = match rest {
        [stop] => (0, islice_index(stop, "Stop argument")?, 1),
        [start, stop, step @ ..] => {
            let start = islice_index(start, "Indices")?.unwrap_or(0);
            let stop = islice_index(stop, "Indices")?;
            let step = match step {
                [] | [Object::None] => 1,
                [Object::Int(i)] if *i >= 1 => *i as u64,
                [_] => {
                    return Err(value_error(
                        "Step for islice() must be a positive integer or None.",
                    ))
                }
                _ => unreachable!("rest.len() <= 3"),
            };
            (start, stop, step)
        }
        [] => unreachable!("rest.len() >= 1"),
    };

    let source = interp.iter_object(iterable.clone())?;
    Ok(Object::LazyIter(Rc::new(PyLazyIter {
        state: RefCell::new(LazyIterKind::Islice {
            source,
            next_idx: start,
            pos: 0,
            stop,
            step,
            done: false,
        }),
    })))
}

//! The `_warnings` module — RFC 0023.
//!
//! Backs the frozen `warnings.py` with a global filter list and a
//! `warn`/`warn_explicit` entry point. The actual rendering is done
//! in Python; this module owns the mutable filter state.

use crate::sync::Rc;
use crate::sync::RefCell;

use crate::error::RuntimeError;
use crate::import::ModuleCache;
use crate::object::{BuiltinFn, DictData, DictKey, Object, PyModule};

thread_local! {
    static FILTERS: RefCell<Vec<Object>> = const { RefCell::new(Vec::new()) };
    static MUTATED: RefCell<u64> = const { RefCell::new(0) };
}

pub fn build(_cache: &ModuleCache) -> Rc<PyModule> {
    let dict = Rc::new(RefCell::new(DictData::new()));
    {
        let mut d = dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("__name__")),
            Object::from_static("_warnings"),
        );
        d.insert(
            DictKey(Object::from_static("warn")),
            builtin("warn", w_warn),
        );
        d.insert(
            DictKey(Object::from_static("warn_explicit")),
            builtin("warn_explicit", w_warn_explicit),
        );
        d.insert(
            DictKey(Object::from_static("filters_mutated")),
            builtin("filters_mutated", w_filters_mutated),
        );
        d.insert(
            DictKey(Object::from_static("_filters_action")),
            builtin("_filters_action", w_filters_action),
        );
    }
    Rc::new(PyModule {
        name: "_warnings".to_owned(),
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

fn w_warn(args: &[Object]) -> Result<Object, RuntimeError> {
    // The Python wrapper handles the heavy lifting; here we just
    // print to stderr in the default-action case. Real filter logic
    // lives in warnings.py.
    let msg = args
        .first()
        .map(|o| o.to_str())
        .unwrap_or_else(|| "warning".to_owned());
    let cls = args
        .get(1)
        .map(|o| o.type_name_owned())
        .unwrap_or_else(|| "UserWarning".to_owned());
    eprintln!("{cls}: {msg}");
    Ok(Object::None)
}

fn w_warn_explicit(args: &[Object]) -> Result<Object, RuntimeError> {
    let msg = args.first().map(|o| o.to_str()).unwrap_or_default();
    eprintln!("warning: {msg}");
    let _ = args;
    Ok(Object::None)
}

fn w_filters_mutated(_args: &[Object]) -> Result<Object, RuntimeError> {
    MUTATED.with(|m| {
        let mut g = m.borrow_mut();
        *g = g.wrapping_add(1);
    });
    Ok(Object::None)
}

fn w_filters_action(args: &[Object]) -> Result<Object, RuntimeError> {
    // Inspector hook for warnings.py: return the current filter list.
    let _ = args;
    FILTERS.with(|f| Ok(Object::new_list(f.borrow().clone())))
}

pub fn warn_message(msg: &str) {
    eprintln!("UserWarning: {msg}");
}

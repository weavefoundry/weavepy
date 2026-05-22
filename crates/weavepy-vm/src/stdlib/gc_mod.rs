//! The `gc` module — exposes the knobs Python programs reach for
//! without actually implementing tracing GC.
//!
//! The interpreter is reference-counted, so reference cycles are
//! leaked. The module is shaped so user code can call `gc.collect`,
//! tune thresholds, and disable / enable collection without raising.
//! Real cycle collection is a follow-up to RFC 0002.

use std::cell::RefCell;
use std::rc::Rc;

use crate::error::RuntimeError;
use crate::import::ModuleCache;
use crate::object::{BuiltinFn, DictData, DictKey, Object, PyModule};

thread_local! {
    static GC_ENABLED: RefCell<bool> = const { RefCell::new(true) };
    static GC_DEBUG: RefCell<i64> = const { RefCell::new(0) };
    static GC_THRESHOLD: RefCell<(i64, i64, i64)> = const { RefCell::new((700, 10, 10)) };
}

pub fn build(_cache: &ModuleCache) -> Rc<PyModule> {
    let dict = Rc::new(RefCell::new(DictData::new()));
    {
        let mut d = dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("__name__")),
            Object::from_static("gc"),
        );
        d.insert(
            DictKey(Object::from_static("__doc__")),
            Object::from_static(
                "Cycle-collector knobs. The interpreter uses reference counting so \
                 collection is effectively manual.",
            ),
        );
        d.insert(
            DictKey(Object::from_static("DEBUG_STATS")),
            Object::Int(0x1),
        );
        d.insert(
            DictKey(Object::from_static("DEBUG_COLLECTABLE")),
            Object::Int(0x2),
        );
        d.insert(
            DictKey(Object::from_static("DEBUG_UNCOLLECTABLE")),
            Object::Int(0x4),
        );
        d.insert(
            DictKey(Object::from_static("DEBUG_SAVEALL")),
            Object::Int(0x20),
        );
        d.insert(
            DictKey(Object::from_static("DEBUG_LEAK")),
            Object::Int(0x26),
        );
        d.insert(
            DictKey(Object::from_static("garbage")),
            Object::new_list(Vec::new()),
        );
        d.insert(
            DictKey(Object::from_static("callbacks")),
            Object::new_list(Vec::new()),
        );
        d.insert(
            DictKey(Object::from_static("collect")),
            b("collect", collect),
        );
        d.insert(
            DictKey(Object::from_static("get_count")),
            b("get_count", get_count),
        );
        d.insert(
            DictKey(Object::from_static("get_threshold")),
            b("get_threshold", get_threshold),
        );
        d.insert(
            DictKey(Object::from_static("set_threshold")),
            b("set_threshold", set_threshold),
        );
        d.insert(
            DictKey(Object::from_static("disable")),
            b("disable", disable),
        );
        d.insert(DictKey(Object::from_static("enable")), b("enable", enable));
        d.insert(
            DictKey(Object::from_static("isenabled")),
            b("isenabled", isenabled),
        );
        d.insert(
            DictKey(Object::from_static("get_objects")),
            b("get_objects", get_objects),
        );
        d.insert(
            DictKey(Object::from_static("get_referrers")),
            b("get_referrers", get_referrers),
        );
        d.insert(
            DictKey(Object::from_static("get_referents")),
            b("get_referents", get_referents),
        );
        d.insert(
            DictKey(Object::from_static("get_debug")),
            b("get_debug", get_debug),
        );
        d.insert(
            DictKey(Object::from_static("set_debug")),
            b("set_debug", set_debug),
        );
        d.insert(
            DictKey(Object::from_static("is_tracked")),
            b("is_tracked", is_tracked),
        );
        d.insert(
            DictKey(Object::from_static("is_finalized")),
            b("is_finalized", is_finalized),
        );
        d.insert(DictKey(Object::from_static("freeze")), b("freeze", noop));
        d.insert(
            DictKey(Object::from_static("unfreeze")),
            b("unfreeze", noop),
        );
        d.insert(
            DictKey(Object::from_static("get_freeze_count")),
            b("get_freeze_count", zero),
        );
        d.insert(
            DictKey(Object::from_static("get_stats")),
            b("get_stats", get_stats),
        );
    }
    Rc::new(PyModule {
        name: "gc".to_owned(),
        filename: None,
        dict,
    })
}

fn b(name: &'static str, body: fn(&[Object]) -> Result<Object, RuntimeError>) -> Object {
    Object::Builtin(Rc::new(BuiltinFn {
        name,
        call: Box::new(body),
    }))
}

fn collect(_args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(Object::Int(0))
}

fn get_count(_args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(Object::new_tuple(vec![
        Object::Int(0),
        Object::Int(0),
        Object::Int(0),
    ]))
}

fn get_threshold(_args: &[Object]) -> Result<Object, RuntimeError> {
    GC_THRESHOLD.with(|t| {
        let (a, b, c) = *t.borrow();
        Ok(Object::new_tuple(vec![
            Object::Int(a),
            Object::Int(b),
            Object::Int(c),
        ]))
    })
}

fn set_threshold(args: &[Object]) -> Result<Object, RuntimeError> {
    let mut vals = [700i64, 10, 10];
    for (slot, v) in vals.iter_mut().zip(args.iter()) {
        if let Object::Int(n) = v {
            *slot = *n;
        }
    }
    GC_THRESHOLD.with(|t| {
        *t.borrow_mut() = (vals[0], vals[1], vals[2]);
    });
    Ok(Object::None)
}

fn disable(_args: &[Object]) -> Result<Object, RuntimeError> {
    GC_ENABLED.with(|e| *e.borrow_mut() = false);
    Ok(Object::None)
}

fn enable(_args: &[Object]) -> Result<Object, RuntimeError> {
    GC_ENABLED.with(|e| *e.borrow_mut() = true);
    Ok(Object::None)
}

fn isenabled(_args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(GC_ENABLED.with(|e| Object::Bool(*e.borrow())))
}

fn get_objects(_args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(Object::new_list(Vec::new()))
}

fn get_referrers(_args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(Object::new_list(Vec::new()))
}

fn get_referents(_args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(Object::new_list(Vec::new()))
}

fn get_debug(_args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(GC_DEBUG.with(|d| Object::Int(*d.borrow())))
}

fn set_debug(args: &[Object]) -> Result<Object, RuntimeError> {
    if let Some(Object::Int(n)) = args.first() {
        GC_DEBUG.with(|d| *d.borrow_mut() = *n);
    }
    Ok(Object::None)
}

fn is_tracked(_args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(Object::Bool(false))
}

fn is_finalized(_args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(Object::Bool(false))
}

fn noop(_args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(Object::None)
}

fn zero(_args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(Object::Int(0))
}

fn get_stats(_args: &[Object]) -> Result<Object, RuntimeError> {
    let mut stat = DictData::new();
    stat.insert(DictKey(Object::from_static("collections")), Object::Int(0));
    stat.insert(DictKey(Object::from_static("collected")), Object::Int(0));
    stat.insert(
        DictKey(Object::from_static("uncollectable")),
        Object::Int(0),
    );
    Ok(Object::new_list(vec![Object::Dict(Rc::new(RefCell::new(
        stat,
    )))]))
}

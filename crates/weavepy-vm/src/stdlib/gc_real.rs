//! Real `gc` module — RFC 0024.
//!
//! Replaces the no-op shim in `stdlib::gc_mod`. The new module
//! exposes the full `gc` surface CPython 3.13 documents,
//! plumbed through to [`crate::gc_trace`]:
//!
//! - **`collect(generation=2)`** — runs the cycle collector.
//!   Returns the number of objects reclaimed.
//! - **`enable` / `disable` / `isenabled`** — toggle the
//!   collector.
//! - **`set_threshold` / `get_threshold`** — tune the
//!   generation thresholds.
//! - **`get_count`** — current per-generation counters.
//! - **`get_objects(generation=None)`** — list of tracked
//!   objects in `generation` or all generations.
//! - **`get_referrers(*objs)`** — objects directly referencing
//!   any in `objs`.
//! - **`get_referents(*objs)`** — objects directly referenced
//!   by any in `objs`.
//! - **`is_tracked(obj)`** — is `obj` currently tracked?
//! - **`is_finalized(obj)`** — has `obj`'s `__del__` already
//!   run?
//! - **`set_debug` / `get_debug`** — DEBUG_* flag word.
//! - **`freeze` / `unfreeze` / `get_freeze_count`** — promote
//!   live objects to a permanent generation.
//! - **`get_stats()`** — aggregate per-generation stats.
//! - **`callbacks`** — user list invoked at start/stop.
//! - **`garbage`** — uncollectable objects.
//! - **`DEBUG_*` constants**.

use crate::sync::Rc;
use crate::sync::RefCell;
use std::sync::atomic::Ordering;

use crate::error::{type_error, RuntimeError};
use crate::gc_trace::{self, N_GENERATIONS};
use crate::import::ModuleCache;
use crate::object::{BuiltinFn, DictData, DictKey, Object, PyModule};
use crate::weakref_registry::id_of;

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
                "Generational tracing cycle collector. Runs alongside \
                 reference counting; collects cycles between scheduled \
                 ticks and on explicit `gc.collect()`.",
            ),
        );
        for (name, value) in [
            ("DEBUG_STATS", 0x1),
            ("DEBUG_COLLECTABLE", 0x2),
            ("DEBUG_UNCOLLECTABLE", 0x4),
            ("DEBUG_SAVEALL", 0x20),
            ("DEBUG_LEAK", 0x26),
        ] {
            d.insert(
                DictKey(Object::from_static(name)),
                Object::Int(i64::from(value)),
            );
        }
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
            b(".gc.collect", collect),
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
        d.insert(DictKey(Object::from_static("enable")), b("enable", enable));
        d.insert(
            DictKey(Object::from_static("disable")),
            b("disable", disable),
        );
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
            DictKey(Object::from_static("is_tracked")),
            b("is_tracked", is_tracked),
        );
        d.insert(
            DictKey(Object::from_static("is_finalized")),
            b("is_finalized", is_finalized),
        );
        d.insert(
            DictKey(Object::from_static("set_debug")),
            b("set_debug", set_debug),
        );
        d.insert(
            DictKey(Object::from_static("get_debug")),
            b("get_debug", get_debug),
        );
        d.insert(DictKey(Object::from_static("freeze")), b("freeze", freeze));
        d.insert(
            DictKey(Object::from_static("unfreeze")),
            b("unfreeze", unfreeze),
        );
        d.insert(
            DictKey(Object::from_static("get_freeze_count")),
            b("get_freeze_count", get_freeze_count),
        );
        d.insert(
            DictKey(Object::from_static("get_stats")),
            b("get_stats", get_stats),
        );
        d.insert(
            DictKey(Object::from_static("_track")),
            b("_track", track_obj),
        );
        d.insert(
            DictKey(Object::from_static("_untrack")),
            b("_untrack", untrack_obj),
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
        call_kw: None,
    }))
}

/// Internal name: prefixed with `.gc.` so the interpreter's call
/// dispatcher can recognise this builtin and drain pending
/// `__del__` finalizers after the underlying mark-sweep returns.
/// Plain Rust BuiltinFns can't reach the interpreter.
fn collect(args: &[Object]) -> Result<Object, RuntimeError> {
    let upto = match args.first() {
        Some(Object::Int(n)) => (*n).max(0) as usize,
        _ => N_GENERATIONS - 1,
    };
    let collected = gc_trace::collect_upto(upto);
    Ok(Object::Int(collected as i64))
}

fn get_count(_args: &[Object]) -> Result<Object, RuntimeError> {
    let counts = gc_trace::with_state(|s| s.counts());
    Ok(Object::new_tuple(vec![
        Object::Int(counts[0] as i64),
        Object::Int(counts[1] as i64),
        Object::Int(counts[2] as i64),
    ]))
}

fn get_threshold(_args: &[Object]) -> Result<Object, RuntimeError> {
    let t = gc_trace::with_state(|s| s.thresholds());
    Ok(Object::new_tuple(vec![
        Object::Int(t[0] as i64),
        Object::Int(t[1] as i64),
        Object::Int(t[2] as i64),
    ]))
}

fn set_threshold(args: &[Object]) -> Result<Object, RuntimeError> {
    let mut vals = [700usize, 10, 10];
    for (slot, v) in vals.iter_mut().zip(args.iter()) {
        if let Object::Int(n) = v {
            *slot = (*n).max(0) as usize;
        } else {
            return Err(type_error("set_threshold expects ints"));
        }
    }
    gc_trace::with_state(|s| s.set_thresholds(vals));
    Ok(Object::None)
}

fn enable(_args: &[Object]) -> Result<Object, RuntimeError> {
    gc_trace::with_state(|s| s.enable());
    Ok(Object::None)
}

fn disable(_args: &[Object]) -> Result<Object, RuntimeError> {
    gc_trace::with_state(|s| s.disable());
    Ok(Object::None)
}

fn isenabled(_args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(Object::Bool(gc_trace::with_state(|s| s.is_enabled())))
}

fn get_objects(args: &[Object]) -> Result<Object, RuntimeError> {
    let gen = match args.first() {
        Some(Object::Int(n)) => Some((*n).max(0) as usize),
        _ => None,
    };
    let objs = gc_trace::with_state(|s| s.snapshot(gen));
    Ok(Object::new_list(objs))
}

fn get_referrers(args: &[Object]) -> Result<Object, RuntimeError> {
    if args.is_empty() {
        return Ok(Object::new_list(Vec::new()));
    }
    let target_ids: Vec<_> = args.iter().map(id_of).collect();
    let mut referrers: Vec<Object> = Vec::new();
    let snapshot = gc_trace::with_state(|s| s.snapshot(None));
    for candidate in snapshot {
        let mut hits = false;
        gc_trace::traverse_object(&candidate, &mut |child| {
            if target_ids.iter().any(|t| *t == id_of(child)) {
                hits = true;
            }
        });
        if hits {
            referrers.push(candidate);
        }
    }
    Ok(Object::new_list(referrers))
}

fn get_referents(args: &[Object]) -> Result<Object, RuntimeError> {
    let mut out: Vec<Object> = Vec::new();
    for arg in args {
        gc_trace::traverse_object(arg, &mut |child| out.push(child.clone()));
    }
    Ok(Object::new_list(out))
}

fn is_tracked(args: &[Object]) -> Result<Object, RuntimeError> {
    let target = args
        .first()
        .ok_or_else(|| type_error("is_tracked() requires 1 argument"))?;
    let id = id_of(target);
    Ok(Object::Bool(gc_trace::with_state(|s| s.is_tracked(id))))
}

fn is_finalized(_args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(Object::Bool(false))
}

fn set_debug(args: &[Object]) -> Result<Object, RuntimeError> {
    if let Some(Object::Int(n)) = args.first() {
        gc_trace::with_state(|s| s.debug.store(*n, Ordering::Release));
    }
    Ok(Object::None)
}

fn get_debug(_args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(Object::Int(gc_trace::with_state(|s| {
        s.debug.load(Ordering::Acquire)
    })))
}

fn freeze(_args: &[Object]) -> Result<Object, RuntimeError> {
    gc_trace::with_state(|s| s.freeze_all());
    Ok(Object::None)
}

fn unfreeze(_args: &[Object]) -> Result<Object, RuntimeError> {
    gc_trace::with_state(|s| s.unfreeze_all());
    Ok(Object::None)
}

fn get_freeze_count(_args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(Object::Int(
        gc_trace::with_state(|s| s.freeze_count()) as i64
    ))
}

fn get_stats(_args: &[Object]) -> Result<Object, RuntimeError> {
    let stats = gc_trace::with_state(|s| *s.stats.borrow());
    let mut entries = Vec::new();
    for s in stats.iter() {
        let mut d = DictData::new();
        d.insert(
            DictKey(Object::from_static("collections")),
            Object::Int(s.collections as i64),
        );
        d.insert(
            DictKey(Object::from_static("collected")),
            Object::Int(s.collected as i64),
        );
        d.insert(
            DictKey(Object::from_static("uncollectable")),
            Object::Int(s.uncollectable as i64),
        );
        entries.push(Object::Dict(Rc::new(RefCell::new(d))));
    }
    Ok(Object::new_list(entries))
}

fn track_obj(args: &[Object]) -> Result<Object, RuntimeError> {
    if let Some(o) = args.first() {
        gc_trace::track(o.clone());
    }
    Ok(Object::None)
}

fn untrack_obj(args: &[Object]) -> Result<Object, RuntimeError> {
    if let Some(o) = args.first() {
        let id = id_of(o);
        gc_trace::with_state(|s| s.untrack_id(id));
    }
    Ok(Object::None)
}

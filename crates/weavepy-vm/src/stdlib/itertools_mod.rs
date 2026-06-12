//! The `_itertools` built-in module — native cores for `itertools`.
//!
//! CPython implements itertools in C: its adapters are plain iterator
//! objects whose stepping pushes no Python frame. The frozen Python
//! `itertools` module prefers these natives and falls back to its
//! generator implementations for the rest. Frame-neutral stepping is
//! load-bearing for `traceback.walk_stack`, which hardcodes how many
//! `f_back` hops separate it from its caller — a Python-level `islice`
//! in `StackSummary.extract` would skew the chain.

use std::collections::HashMap;
use std::sync::Mutex;

use crate::sync::Rc;
use crate::sync::RefCell;
use crate::sync::Weak;

use crate::error::{type_error, value_error, RuntimeError};
use crate::import::ModuleCache;
use crate::object::{
    BuiltinFn, DictData, DictKey, LazyIterKind, Object, PyLazyIter, PyModule, TeeShared,
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
        macro_rules! reg {
            ($name:literal, $f:ident) => {
                d.insert(
                    DictKey(Object::from_static($name)),
                    Object::Builtin(Rc::new(BuiltinFn {
                        name: $name,
                        binds_instance: false,
                        call: Box::new($f),
                        call_kw: None,
                    })),
                );
            };
        }
        reg!("islice", islice);
        reg!("islice_core", islice_core);
        reg!("islice_set_cnt", islice_set_cnt);
        reg!("repeat_core", repeat_core);
        reg!("tee_core", tee_core);
        reg!("lazy_state", lazy_state);
        reg!("count_core", count_core);
        reg!("cycle_core", cycle_core);
        reg!("chain_core", chain_core);
        reg!("compress_core", compress_core);
        reg!("dropwhile_core", dropwhile_core);
        reg!("takewhile_core", takewhile_core);
        reg!("filterfalse_core", filterfalse_core);
        reg!("starmap_core", starmap_core);
        reg!("pairwise_core", pairwise_core);
        reg!("zip_longest_core", zip_longest_core);
        reg!("accumulate_core", accumulate_core);
        reg!("product_core", product_core);
        reg!("permutations_core", permutations_core);
        reg!("combinations_core", combinations_core);
        reg!("cwr_core", cwr_core);
        reg!("batched_core", batched_core);
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

/// Non-negative machine int from a builtin-call argument.
fn nonneg_int(arg: &Object, what: &str) -> Result<u64, RuntimeError> {
    match arg {
        Object::Int(i) if *i >= 0 => Ok(*i as u64),
        _ => Err(type_error(format!("{what} must be a non-negative int"))),
    }
}

/// `islice_core(iterator, start, stop_or_None, step)` — the frozen
/// `itertools.islice` class pre-validates the arguments and passes an
/// already-`iter()`ed source.
fn islice_core(args: &[Object]) -> Result<Object, RuntimeError> {
    let [source, start, stop, step] = args else {
        return Err(type_error("islice_core expected 4 arguments"));
    };
    let start = nonneg_int(start, "start")?;
    let stop = match stop {
        Object::None => None,
        other => Some(nonneg_int(other, "stop")?),
    };
    let step = nonneg_int(step, "step")?.max(1);
    Ok(Object::LazyIter(Rc::new(PyLazyIter {
        state: RefCell::new(LazyIterKind::Islice {
            source: source.clone(),
            next_idx: start,
            pos: 0,
            stop,
            step,
            done: false,
        }),
    })))
}

/// `islice_set_cnt(core, cnt)` — `islice.__setstate__` restores the
/// consumed-element counter (CPython's `lz->cnt`).
fn islice_set_cnt(args: &[Object]) -> Result<Object, RuntimeError> {
    let [core, cnt] = args else {
        return Err(type_error("islice_set_cnt expected 2 arguments"));
    };
    let cnt = nonneg_int(cnt, "cnt")?;
    let Object::LazyIter(l) = core else {
        return Err(type_error("islice_set_cnt: not an islice core"));
    };
    match &mut *l.state.borrow_mut() {
        LazyIterKind::Islice { pos, .. } => {
            *pos = cnt;
            Ok(Object::None)
        }
        _ => Err(type_error("islice_set_cnt: not an islice core")),
    }
}

/// `repeat_core(object, times_or_None)`.
fn repeat_core(args: &[Object]) -> Result<Object, RuntimeError> {
    let [obj, times] = args else {
        return Err(type_error("repeat_core expected 2 arguments"));
    };
    let times = match times {
        Object::None => None,
        Object::Int(i) => Some((*i).max(0)),
        _ => return Err(type_error("repeat_core: times must be an int or None")),
    };
    Ok(Object::LazyIter(Rc::new(PyLazyIter {
        state: RefCell::new(LazyIterKind::Repeat {
            obj: obj.clone(),
            times,
        }),
    })))
}

/// Live native [`TeeShared`]s keyed by the address of the Python
/// `_tee_dataobject` instance they mirror. Branches created from the
/// same data object (tee siblings, copies, co-unpickled branches) must
/// share one buffer/busy-flag; a weak registry provides that identity
/// without creating an Rc cycle through the data object. An entry can
/// only be live while some branch (which also owns the data object)
/// is alive, so a dead data object's address can never collide with a
/// live entry.
static TEE_REGISTRY: Mutex<Option<HashMap<usize, Weak<RefCell<TeeShared>>>>> = Mutex::new(None);

/// `tee_core(data, index)` — one branch over a `_tee_dataobject`.
fn tee_core(args: &[Object]) -> Result<Object, RuntimeError> {
    let [data, index] = args else {
        return Err(type_error("tee_core expected 2 arguments"));
    };
    let index = nonneg_int(index, "index")? as usize;
    let Object::Instance(inst) = data else {
        return Err(type_error("tee_core: data must be a _tee_dataobject"));
    };
    let key = Rc::as_ptr(inst) as usize;
    let mut guard = TEE_REGISTRY.lock().expect("tee registry poisoned");
    let registry = guard.get_or_insert_with(HashMap::new);
    registry.retain(|_, w| w.strong_count() > 0);
    let shared = match registry.get(&key).and_then(Weak::upgrade) {
        Some(shared) => shared,
        None => {
            let d = inst.dict.borrow();
            let source = match d.get(&DictKey(Object::from_static("source"))) {
                Some(Object::None) | None => None,
                Some(src) => Some(src.clone()),
            };
            let buffer = match d.get(&DictKey(Object::from_static("buffer"))) {
                Some(Object::List(items)) => items.clone(),
                _ => return Err(type_error("tee_core: data.buffer must be a list")),
            };
            drop(d);
            let shared = Rc::new(RefCell::new(TeeShared {
                source,
                buffer,
                busy: false,
            }));
            registry.insert(key, Rc::downgrade(&shared));
            shared
        }
    };
    drop(guard);
    Ok(Object::LazyIter(Rc::new(PyLazyIter {
        state: RefCell::new(LazyIterKind::TeeBranch {
            shared,
            data: data.clone(),
            index,
        }),
    })))
}

fn make_lazy(kind: LazyIterKind) -> Object {
    Object::LazyIter(Rc::new(PyLazyIter {
        state: RefCell::new(kind),
    }))
}

fn opt_obj(o: &Object) -> Option<Object> {
    match o {
        Object::None => None,
        other => Some(other.clone()),
    }
}

fn as_bool(o: &Object) -> bool {
    o.is_truthy()
}

/// Materialised pool argument: a tuple (the Python wrappers always
/// pass tuples).
fn pool_of(o: &Object, what: &str) -> Result<Rc<[Object]>, RuntimeError> {
    match o {
        Object::Tuple(items) => Ok(items.clone()),
        _ => Err(type_error(format!("{what} must be a tuple"))),
    }
}

fn usize_vec(o: &Object, what: &str) -> Result<Vec<usize>, RuntimeError> {
    let items: Vec<Object> = match o {
        Object::Tuple(items) => items.to_vec(),
        Object::List(items) => items.borrow().clone(),
        _ => return Err(type_error(format!("{what} must be a tuple or list"))),
    };
    items
        .iter()
        .map(|x| match x {
            Object::Int(i) if *i >= 0 => Ok(*i as usize),
            _ => Err(type_error(format!("{what} must contain non-negative ints"))),
        })
        .collect()
}

/// `count_core(current, step)`.
fn count_core(args: &[Object]) -> Result<Object, RuntimeError> {
    let [current, step] = args else {
        return Err(type_error("count_core expected 2 arguments"));
    };
    Ok(make_lazy(LazyIterKind::Count {
        current: current.clone(),
        step: step.clone(),
    }))
}

/// `cycle_core(source_or_None, saved_list, index, firstpass)` — the
/// saved list's storage is shared with the caller's list object.
fn cycle_core(args: &[Object]) -> Result<Object, RuntimeError> {
    let [source, saved, index, firstpass] = args else {
        return Err(type_error("cycle_core expected 4 arguments"));
    };
    let Object::List(saved) = saved else {
        return Err(type_error("cycle_core: saved must be a list"));
    };
    Ok(make_lazy(LazyIterKind::Cycle {
        source: opt_obj(source),
        saved: saved.clone(),
        index: nonneg_int(index, "index")? as usize,
        firstpass: as_bool(firstpass),
    }))
}

/// `chain_core(source_or_None, active_or_None)`.
fn chain_core(args: &[Object]) -> Result<Object, RuntimeError> {
    let [source, active] = args else {
        return Err(type_error("chain_core expected 2 arguments"));
    };
    Ok(make_lazy(LazyIterKind::Chain {
        source: opt_obj(source),
        active: opt_obj(active),
    }))
}

/// `compress_core(data, selectors)`.
fn compress_core(args: &[Object]) -> Result<Object, RuntimeError> {
    let [data, selectors] = args else {
        return Err(type_error("compress_core expected 2 arguments"));
    };
    Ok(make_lazy(LazyIterKind::Compress {
        data: data.clone(),
        selectors: selectors.clone(),
    }))
}

/// `dropwhile_core(func, source, started)`.
fn dropwhile_core(args: &[Object]) -> Result<Object, RuntimeError> {
    let [func, source, started] = args else {
        return Err(type_error("dropwhile_core expected 3 arguments"));
    };
    Ok(make_lazy(LazyIterKind::DropWhile {
        func: func.clone(),
        source: source.clone(),
        started: as_bool(started),
    }))
}

/// `takewhile_core(func, source, stopped)`.
fn takewhile_core(args: &[Object]) -> Result<Object, RuntimeError> {
    let [func, source, stopped] = args else {
        return Err(type_error("takewhile_core expected 3 arguments"));
    };
    Ok(make_lazy(LazyIterKind::TakeWhile {
        func: func.clone(),
        source: source.clone(),
        stopped: as_bool(stopped),
    }))
}

/// `filterfalse_core(func_or_None, source)`.
fn filterfalse_core(args: &[Object]) -> Result<Object, RuntimeError> {
    let [func, source] = args else {
        return Err(type_error("filterfalse_core expected 2 arguments"));
    };
    Ok(make_lazy(LazyIterKind::FilterFalse {
        func: func.clone(),
        source: source.clone(),
    }))
}

/// `starmap_core(func, source)`.
fn starmap_core(args: &[Object]) -> Result<Object, RuntimeError> {
    let [func, source] = args else {
        return Err(type_error("starmap_core expected 2 arguments"));
    };
    Ok(make_lazy(LazyIterKind::StarMap {
        func: func.clone(),
        source: source.clone(),
    }))
}

/// `pairwise_core(source)`.
fn pairwise_core(args: &[Object]) -> Result<Object, RuntimeError> {
    let [source] = args else {
        return Err(type_error("pairwise_core expected 1 argument"));
    };
    Ok(make_lazy(LazyIterKind::Pairwise {
        source: opt_obj(source),
        old: None,
    }))
}

/// `zip_longest_core(fillvalue, iters_tuple)` — slots that are Python
/// `None` are already-exhausted iterators.
fn zip_longest_core(args: &[Object]) -> Result<Object, RuntimeError> {
    let [fillvalue, iters] = args else {
        return Err(type_error("zip_longest_core expected 2 arguments"));
    };
    let slots: Vec<Option<Object>> = match iters {
        Object::Tuple(items) => items.iter().map(opt_obj).collect(),
        Object::List(items) => items.borrow().iter().map(opt_obj).collect(),
        _ => return Err(type_error("zip_longest_core: iters must be a sequence")),
    };
    let numactive = slots.iter().filter(|s| s.is_some()).count();
    Ok(make_lazy(LazyIterKind::ZipLongest {
        iters: slots,
        fillvalue: fillvalue.clone(),
        numactive,
    }))
}

/// `accumulate_core(source, func_or_None, has_total, total, initial_or_None)`.
fn accumulate_core(args: &[Object]) -> Result<Object, RuntimeError> {
    let [source, func, has_total, total, initial] = args else {
        return Err(type_error("accumulate_core expected 5 arguments"));
    };
    Ok(make_lazy(LazyIterKind::Accumulate {
        source: source.clone(),
        func: opt_obj(func),
        total: if as_bool(has_total) {
            Some(total.clone())
        } else {
            None
        },
        initial: opt_obj(initial),
    }))
}

/// `product_core(pools_tuple, indices_or_None, started, stopped)`.
fn product_core(args: &[Object]) -> Result<Object, RuntimeError> {
    let [pools, indices, started, stopped] = args else {
        return Err(type_error("product_core expected 4 arguments"));
    };
    let pools: Vec<Rc<[Object]>> = match pools {
        Object::Tuple(items) => items
            .iter()
            .map(|p| pool_of(p, "pool"))
            .collect::<Result<_, _>>()?,
        _ => return Err(type_error("product_core: pools must be a tuple")),
    };
    let indices = match indices {
        Object::None => vec![0; pools.len()],
        other => usize_vec(other, "indices")?,
    };
    Ok(make_lazy(LazyIterKind::Product {
        pools,
        indices,
        started: as_bool(started),
        stopped: as_bool(stopped),
    }))
}

/// `permutations_core(pool, r, indices, cycles, started, stopped)`.
fn permutations_core(args: &[Object]) -> Result<Object, RuntimeError> {
    let [pool, r, indices, cycles, started, stopped] = args else {
        return Err(type_error("permutations_core expected 6 arguments"));
    };
    let pool = pool_of(pool, "pool")?;
    let r = nonneg_int(r, "r")? as usize;
    let indices = match indices {
        Object::None => (0..pool.len()).collect(),
        other => usize_vec(other, "indices")?,
    };
    let cycles = match cycles {
        Object::None => (pool.len().saturating_sub(r) + 1..=pool.len())
            .rev()
            .collect(),
        other => usize_vec(other, "cycles")?,
    };
    Ok(make_lazy(LazyIterKind::Permutations {
        pool,
        r,
        indices,
        cycles,
        started: as_bool(started),
        stopped: as_bool(stopped),
    }))
}

/// `combinations_core(pool, r, indices_or_None, started, stopped)`.
fn combinations_core(args: &[Object]) -> Result<Object, RuntimeError> {
    let [pool, r, indices, started, stopped] = args else {
        return Err(type_error("combinations_core expected 5 arguments"));
    };
    let pool = pool_of(pool, "pool")?;
    let r = nonneg_int(r, "r")? as usize;
    let indices = match indices {
        Object::None => (0..r).collect(),
        other => usize_vec(other, "indices")?,
    };
    Ok(make_lazy(LazyIterKind::Combinations {
        pool,
        r,
        indices,
        started: as_bool(started),
        stopped: as_bool(stopped),
    }))
}

/// `cwr_core(pool, r, indices_or_None, started, stopped)`.
fn cwr_core(args: &[Object]) -> Result<Object, RuntimeError> {
    let [pool, r, indices, started, stopped] = args else {
        return Err(type_error("cwr_core expected 5 arguments"));
    };
    let pool = pool_of(pool, "pool")?;
    let r = nonneg_int(r, "r")? as usize;
    let indices = match indices {
        Object::None => vec![0; r],
        other => usize_vec(other, "indices")?,
    };
    Ok(make_lazy(LazyIterKind::Cwr {
        pool,
        r,
        indices,
        started: as_bool(started),
        stopped: as_bool(stopped),
    }))
}

/// `batched_core(source_or_None, n, strict)`.
fn batched_core(args: &[Object]) -> Result<Object, RuntimeError> {
    let [source, n, strict] = args else {
        return Err(type_error("batched_core expected 3 arguments"));
    };
    Ok(make_lazy(LazyIterKind::Batched {
        source: opt_obj(source),
        n: nonneg_int(n, "n")?.max(1) as usize,
        strict: as_bool(strict),
    }))
}

/// `lazy_state(core)` — expose a native core's state to the frozen
/// Python wrappers (for `__reduce__` / `__repr__` / `__length_hint__`).
fn lazy_state(args: &[Object]) -> Result<Object, RuntimeError> {
    let [core] = args else {
        return Err(type_error("lazy_state expected 1 argument"));
    };
    let Object::LazyIter(l) = core else {
        return Err(type_error("lazy_state: not a native itertools core"));
    };
    let items: Vec<Object> = match &*l.state.borrow() {
        LazyIterKind::Islice {
            source,
            next_idx,
            pos,
            stop,
            step,
            done,
        } => vec![
            if *done { Object::None } else { source.clone() },
            Object::Int(*next_idx as i64),
            Object::Int(*pos as i64),
            stop.map_or(Object::None, |s| Object::Int(s as i64)),
            Object::Int(*step as i64),
            Object::Bool(*done),
        ],
        LazyIterKind::Repeat { obj, times } => vec![
            obj.clone(),
            times.map_or(Object::None, Object::Int),
        ],
        LazyIterKind::TeeBranch { data, index, .. } => {
            vec![data.clone(), Object::Int(*index as i64)]
        }
        LazyIterKind::Count { current, step } => vec![current.clone(), step.clone()],
        LazyIterKind::Cycle {
            source,
            saved,
            index,
            firstpass,
        } => vec![
            source.clone().unwrap_or(Object::None),
            Object::List(saved.clone()),
            Object::Int(*index as i64),
            Object::Bool(*firstpass),
        ],
        LazyIterKind::Chain { source, active } => vec![
            source.clone().unwrap_or(Object::None),
            active.clone().unwrap_or(Object::None),
        ],
        LazyIterKind::Compress { data, selectors } => {
            vec![data.clone(), selectors.clone()]
        }
        LazyIterKind::DropWhile {
            func,
            source,
            started,
        } => vec![func.clone(), source.clone(), Object::Bool(*started)],
        LazyIterKind::TakeWhile {
            func,
            source,
            stopped,
        } => vec![func.clone(), source.clone(), Object::Bool(*stopped)],
        LazyIterKind::FilterFalse { func, source } => vec![func.clone(), source.clone()],
        LazyIterKind::StarMap { func, source } => vec![func.clone(), source.clone()],
        LazyIterKind::Pairwise { source, old } => vec![
            source.clone().unwrap_or(Object::None),
            old.clone().unwrap_or(Object::None),
        ],
        LazyIterKind::ZipLongest {
            iters,
            fillvalue,
            numactive,
        } => {
            let slots: Vec<Object> = iters
                .iter()
                .map(|s| s.clone().unwrap_or(Object::None))
                .collect();
            vec![
                fillvalue.clone(),
                Object::Int(*numactive as i64),
                Object::Tuple(slots.into()),
            ]
        }
        LazyIterKind::Accumulate {
            source,
            func,
            total,
            initial,
        } => vec![
            source.clone(),
            func.clone().unwrap_or(Object::None),
            Object::Bool(total.is_some()),
            total.clone().unwrap_or(Object::None),
            initial.clone().unwrap_or(Object::None),
        ],
        LazyIterKind::Product {
            pools,
            indices,
            started,
            stopped,
        } => {
            let pools: Vec<Object> = pools
                .iter()
                .map(|p| Object::Tuple(p.clone()))
                .collect();
            let ix: Vec<Object> = indices.iter().map(|&i| Object::Int(i as i64)).collect();
            vec![
                Object::Tuple(pools.into()),
                Object::Tuple(ix.into()),
                Object::Bool(*started),
                Object::Bool(*stopped),
            ]
        }
        LazyIterKind::Permutations {
            pool,
            r,
            indices,
            cycles,
            started,
            stopped,
        } => {
            let ix: Vec<Object> = indices.iter().map(|&i| Object::Int(i as i64)).collect();
            let cy: Vec<Object> = cycles.iter().map(|&i| Object::Int(i as i64)).collect();
            vec![
                Object::Tuple(pool.clone()),
                Object::Int(*r as i64),
                Object::Tuple(ix.into()),
                Object::Tuple(cy.into()),
                Object::Bool(*started),
                Object::Bool(*stopped),
            ]
        }
        LazyIterKind::Combinations {
            pool,
            r,
            indices,
            started,
            stopped,
        }
        | LazyIterKind::Cwr {
            pool,
            r,
            indices,
            started,
            stopped,
        } => {
            let ix: Vec<Object> = indices.iter().map(|&i| Object::Int(i as i64)).collect();
            vec![
                Object::Tuple(pool.clone()),
                Object::Int(*r as i64),
                Object::Tuple(ix.into()),
                Object::Bool(*started),
                Object::Bool(*stopped),
            ]
        }
        LazyIterKind::Batched { source, n, strict } => vec![
            source.clone().unwrap_or(Object::None),
            Object::Int(*n as i64),
            Object::Bool(*strict),
        ],
    };
    Ok(Object::Tuple(items.into()))
}

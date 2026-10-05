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
    let dict = Rc::new(RefCell::new(DictData::default()));
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
        reg!("chain_from_iterable", chain_from_iterable);
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
        reg!(
            "_register_classmethod_descriptor",
            register_classmethod_descriptor
        );
        reg!("_register_types", register_types);
        reg!("groupby_core", groupby_core);
        reg!("grouper_core", grouper_core);
        reg!("groupby_setstate", groupby_setstate);
    }
    Rc::new(PyModule {
        name: "_itertools".to_owned(),
        filename: None,
        dict,
    })
}

/// `_register_classmethod_descriptor(cls, name)` — post-splice hook for
/// the frozen `itertools.py` (the `_operator._register_call_descriptors`
/// pattern): after `chain.from_iterable = classmethod(_n.chain_from_iterable)`
/// it tags the wrapped native as a descriptor *owned by* `cls`, which is
/// what `type(chain.from_iterable)` keys on to present the bound
/// attribute as `builtin_function_or_method` (a C `METH_CLASS` slot binds
/// to one; a Python-level `classmethod(len).__get__` binds to `method`)
/// and what gives it `__qualname__ == 'chain.from_iterable'`.
/// torch._dynamo's `substitute_in_graph(itertools.chain.from_iterable)`
/// gates its polyfill on exactly that type. Before RFC 0077 the native
/// passed the check only by accident: the descriptor side table was never
/// purged, so a fresh `BuiltinFn` at a recycled address inherited a dead
/// builtin's registration. The table is now purged on drop
/// (`descr_registry::forget`), and this hook records the fact honestly.
fn register_classmethod_descriptor(args: &[Object]) -> Result<Object, RuntimeError> {
    let [cls_obj, name_obj] = args else {
        return Err(type_error(
            "_register_classmethod_descriptor expected 2 arguments (cls, name)",
        ));
    };
    let Object::Type(cls) = cls_obj else {
        return Err(type_error(
            "_register_classmethod_descriptor: first argument must be a class",
        ));
    };
    let Object::Str(name) = name_obj else {
        return Err(type_error(
            "_register_classmethod_descriptor: second argument must be a str",
        ));
    };
    let entry = cls.dict.borrow().get(&DictKey(name_obj.clone())).cloned();
    if let Some(Object::ClassMethod(w)) = entry {
        let func = w.func();
        if matches!(func, Object::Builtin(_)) {
            crate::descr_registry::register(
                &func,
                crate::descr_registry::DescrKind::Method,
                cls.clone(),
                name.as_ref(),
                None,
            );
        }
    }
    Ok(Object::None)
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
    Ok(make_lazy(LazyIterKind::Islice {
        source,
        next_idx: start,
        pos: 0,
        stop,
        step,
        done: false,
    }))
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
    Ok(make_lazy(LazyIterKind::Islice {
        source: source.clone(),
        next_idx: start,
        pos: 0,
        stop,
        step,
        done: false,
    }))
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
    Ok(make_lazy(LazyIterKind::Repeat {
        obj: obj.clone(),
        times,
    }))
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
            let d = inst.dict_cell().borrow();
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
    Ok(make_lazy(LazyIterKind::TeeBranch {
        shared,
        data: data.clone(),
        index,
    }))
}

fn make_lazy(kind: LazyIterKind) -> Object {
    Object::LazyIter(Rc::new(PyLazyIter::new(kind)))
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
fn pool_of(o: &Object, what: &str) -> Result<crate::object::SharedTuple, RuntimeError> {
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

/// `chain_from_iterable(cls, iterable)` — the native body behind
/// `chain.from_iterable` (RFC 0076 WS5). The frozen `itertools.py`
/// splices it in as `classmethod(_n.chain_from_iterable)`, so the bound
/// attribute presents as `builtin_function_or_method` exactly like
/// CPython's METH_CLASS original — torch._dynamo's `substitute_in_graph`
/// gates its polyfill registration on that type surface (a Python
/// classmethod bound as `method` was rejected). The body mirrors the
/// Python spelling: `object.__new__(cls)` + `_chain_core(iter(x), None)`.
fn chain_from_iterable(args: &[Object]) -> Result<Object, RuntimeError> {
    let [cls_obj, iterable] = args else {
        return Err(type_error("from_iterable expected 2 arguments"));
    };
    let Object::Type(cls) = cls_obj else {
        return Err(type_error("from_iterable expects a chain subclass"));
    };
    let Some(ptr) = crate::vm_singletons::current_interpreter_ptr() else {
        return Err(type_error(
            "chain.from_iterable requires a running interpreter",
        ));
    };
    // SAFETY: published by the enclosing VM frame on this thread.
    let interp = unsafe { &mut *ptr };
    let globals = interp.builtins_dict();
    let it = interp.make_iter(iterable, &globals)?;
    let core = chain_core(&[it, Object::None])?;
    if cls.lazy_ctor.get() == K_CHAIN {
        return Ok(with_class(core, cls));
    }
    let inst = Rc::new(crate::types::PyInstance::new(cls.clone()));
    inst.dict_cell()
        .borrow_mut()
        .insert(DictKey(Object::from_static("_core")), core);
    Ok(Object::Instance(inst))
}

// ---------------------------------------------------------------------------
// Native construction. The frozen `itertools.py` registers its exact
// classes (`_register_types`); calling one builds the adapter here, with
// no Python `__new__` frame, and the adapter itself is the instance: it
// reports the class as its type, and the class's Python methods reach
// its state through `self._core` (which an exact adapter answers with
// itself). Any shape the native path doesn't take exactly (keywords it
// doesn't know, arguments that would raise, a subclass) decides so before
// any side effect and runs the class's own `__new__`.
// ---------------------------------------------------------------------------

pub(crate) const K_CHAIN: u8 = 1;
const K_COUNT: u8 = 2;
const K_REPEAT: u8 = 3;
const K_CYCLE: u8 = 4;
const K_ACCUMULATE: u8 = 5;
const K_COMPRESS: u8 = 6;
const K_DROPWHILE: u8 = 7;
const K_TAKEWHILE: u8 = 8;
const K_FILTERFALSE: u8 = 9;
const K_STARMAP: u8 = 10;
const K_ISLICE: u8 = 11;
const K_PAIRWISE: u8 = 12;
const K_ZIP_LONGEST: u8 = 13;
const K_PRODUCT: u8 = 14;
const K_PERMUTATIONS: u8 = 15;
const K_COMBINATIONS: u8 = 16;
const K_CWR: u8 = 17;
const K_BATCHED: u8 = 18;
const K_GROUPBY: u8 = 19;

const KINDS: [(&str, u8); 19] = [
    ("groupby", K_GROUPBY),
    ("chain", K_CHAIN),
    ("count", K_COUNT),
    ("repeat", K_REPEAT),
    ("cycle", K_CYCLE),
    ("accumulate", K_ACCUMULATE),
    ("compress", K_COMPRESS),
    ("dropwhile", K_DROPWHILE),
    ("takewhile", K_TAKEWHILE),
    ("filterfalse", K_FILTERFALSE),
    ("starmap", K_STARMAP),
    ("islice", K_ISLICE),
    ("pairwise", K_PAIRWISE),
    ("zip_longest", K_ZIP_LONGEST),
    ("product", K_PRODUCT),
    ("permutations", K_PERMUTATIONS),
    ("combinations", K_COMBINATIONS),
    ("combinations_with_replacement", K_CWR),
    ("batched", K_BATCHED),
];

/// `_register_types(namespace)`: mark the module's exact classes.
fn register_types(args: &[Object]) -> Result<Object, RuntimeError> {
    let [Object::Dict(ns)] = args else {
        return Err(type_error("_register_types expected a namespace dict"));
    };
    let ns = ns.borrow();
    for (name, kind) in KINDS {
        if let Some(Object::Type(cls)) = ns.get(&DictKey(Object::from_static(name))) {
            cls.lazy_ctor.set(kind);
        }
    }
    if let (Some(Object::Type(gb)), Some(Object::Type(gr))) = (
        ns.get(&DictKey(Object::from_static("groupby"))),
        ns.get(&DictKey(Object::from_static("_grouper"))),
    ) {
        GROUPER_CLASSES.with(|g| {
            let mut g = g.borrow_mut();
            g.retain(|(a, _)| a.strong_count() > 0);
            g.push((Rc::downgrade(gb), Rc::downgrade(gr)));
        });
    }
    Ok(Object::None)
}

type WeakType = crate::sync::Weak<crate::types::TypeObject>;

thread_local! {
    /// Each registered `groupby` class with the `_grouper` class its
    /// groups report.
    static GROUPER_CLASSES: std::cell::RefCell<Vec<(WeakType, WeakType)>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// The `_grouper` class registered with `groupby` class `cls`.
fn grouper_class_for(cls: &crate::types::TypeObject) -> Option<Rc<crate::types::TypeObject>> {
    GROUPER_CLASSES.with(|g| {
        g.borrow()
            .iter()
            .find(|(gb, _)| std::ptr::eq(gb.as_ptr(), cls))
            .and_then(|(_, gr)| gr.upgrade())
    })
}

/// `groupby_core(iterator, key_or_None, grouper_class)`.
fn groupby_core(args: &[Object]) -> Result<Object, RuntimeError> {
    let [source, keyfunc, grouper_cls] = args else {
        return Err(type_error("groupby_core expected 3 arguments"));
    };
    Ok(make_lazy(LazyIterKind::GroupBy {
        source: source.clone(),
        keyfunc: keyfunc.clone(),
        tgtkey: None,
        currkey: None,
        currvalue: None,
        currgrouper: None,
        grouper_cls: match grouper_cls {
            Object::Type(t) => Some(t.clone()),
            _ => None,
        },
    }))
}

/// `grouper_core(groupby_core, tgtkey)`: a group of `groupby_core`,
/// installed as its current one (CPython's `_grouper_create`).
fn grouper_core(args: &[Object]) -> Result<Object, RuntimeError> {
    let [Object::LazyIter(parent), tgtkey] = args else {
        return Err(type_error("incorrect usage of internal _grouper"));
    };
    if !matches!(&*parent.state.borrow(), LazyIterKind::GroupBy { .. }) {
        return Err(type_error("incorrect usage of internal _grouper"));
    }
    let grouper = Rc::new(PyLazyIter::new(LazyIterKind::Grouper {
        parent: parent.clone(),
        tgtkey: tgtkey.clone(),
    }));
    if let LazyIterKind::GroupBy { currgrouper, .. } = &mut *parent.state.borrow_mut() {
        *currgrouper = Some(Rc::downgrade(&grouper));
    }
    Ok(Object::LazyIter(grouper))
}

/// `groupby_setstate(core, currkey, currvalue, tgtkey)`.
fn groupby_setstate(args: &[Object]) -> Result<Object, RuntimeError> {
    let [Object::LazyIter(l), key, value, tgt] = args else {
        return Err(type_error("groupby_setstate expected 4 arguments"));
    };
    let old = match &mut *l.state.borrow_mut() {
        LazyIterKind::GroupBy {
            currkey,
            currvalue,
            tgtkey,
            ..
        } => (
            currkey.replace(key.clone()),
            currvalue.replace(value.clone()),
            tgtkey.replace(tgt.clone()),
        ),
        _ => return Err(type_error("groupby_setstate: not a groupby core")),
    };
    drop(old);
    Ok(Object::None)
}

/// `core`, a freshly built adapter, as an instance of `cls` (a
/// `groupby` also learns the class its groups report).
fn with_class(core: Object, cls: &Rc<crate::types::TypeObject>) -> Object {
    match core {
        Object::LazyIter(mut l) => {
            if let Some(slot) = Rc::get_mut(&mut l) {
                slot.cls = Some(cls.clone());
                if let LazyIterKind::GroupBy { grouper_cls, .. } = slot.state.get_mut() {
                    *grouper_cls = grouper_class_for(cls);
                }
            }
            Object::LazyIter(l)
        }
        other => other,
    }
}

/// An `int` argument as CPython's index conversion would take it for
/// the shapes the native path serves (`bool` included).
fn small_int(o: &Object) -> Option<i64> {
    match o {
        Object::Int(i) => Some(*i),
        Object::Bool(b) => Some(i64::from(*b)),
        _ => None,
    }
}

/// `islice` index: `None`, or an int in `0..=sys.maxsize`.
fn islice_arg(o: &Object) -> Option<Object> {
    match o {
        Object::None => Some(Object::None),
        _ => small_int(o).filter(|i| *i >= 0).map(Object::Int),
    }
}

/// Unwrap a `Some(Ok(v))`, returning `None` (decline) or the error.
macro_rules! take {
    ($e:expr) => {
        match $e {
            None => return None,
            Some(Err(e)) => return Some(Err(e)),
            Some(Ok(v)) => v,
        }
    };
}

/// Build the adapter for `kind` from a call's arguments: `None` to run
/// the class's own `__new__`, nothing having been done. `iter_of` is
/// `iter(x)` and `pool_of` is `tuple(x)`; either may decline (the core
/// loop's variants, which run no code), and only after every argument
/// that could make the call decline has been checked.
pub(crate) fn native_new(
    kind: u8,
    args: &[Object],
    kwargs: &[(String, Object)],
    iter_of: &mut dyn FnMut(&Object) -> Option<Result<Object, RuntimeError>>,
    pool_of: &mut dyn FnMut(&Object) -> Option<Result<Object, RuntimeError>>,
) -> Option<Result<Object, RuntimeError>> {
    let n = args.len();
    // The keywords a kind understands; any other declines.
    let mut fillvalue = Object::None;
    let mut repeat = 1i64;
    let mut initial = Object::None;
    let mut key = Object::None;
    match kind {
        K_GROUPBY => {
            for (k, v) in kwargs {
                if k != "key" {
                    return None;
                }
                key = v.clone();
            }
        }
        K_ACCUMULATE => {
            for (k, v) in kwargs {
                if k != "initial" {
                    return None;
                }
                initial = v.clone();
            }
        }
        K_ZIP_LONGEST => {
            for (k, v) in kwargs {
                if k != "fillvalue" {
                    return None;
                }
                fillvalue = v.clone();
            }
        }
        K_PRODUCT => {
            for (k, v) in kwargs {
                if k != "repeat" {
                    return None;
                }
                repeat = small_int(v).filter(|r| *r >= 0)?;
            }
        }
        _ if !kwargs.is_empty() => return None,
        _ => {}
    }
    let core = match kind {
        K_CHAIN => {
            let source = Object::Iter(Rc::new(RefCell::new(
                Object::new_tuple(args.to_vec()).make_iter().ok()?,
            )));
            chain_core(&[source, Object::None])
        }
        K_COUNT => {
            let num = |o: &Object| {
                matches!(
                    o,
                    Object::Int(_) | Object::Bool(_) | Object::Long(_) | Object::Float(_)
                )
            };
            if n > 2 || !args.iter().all(num) {
                return None;
            }
            let start = args.first().cloned().unwrap_or(Object::Int(0));
            let step = args.get(1).cloned().unwrap_or(Object::Int(1));
            count_core(&[start, step])
        }
        K_REPEAT => {
            let times = match args {
                [_] => Object::None,
                [_, Object::None] => Object::None,
                [_, t] => Object::Int(small_int(t)?.max(0)),
                _ => return None,
            };
            repeat_core(&[args[0].clone(), times])
        }
        K_CYCLE => {
            let [it] = args else {
                return None;
            };
            let source = take!(iter_of(it));
            Ok(make_lazy(LazyIterKind::Cycle {
                source: Some(source),
                saved: Rc::new(RefCell::new(Vec::new())),
                index: 0,
                firstpass: false,
            }))
        }
        K_ACCUMULATE => {
            if !(1..=2).contains(&n) {
                return None;
            }
            let func = args.get(1).cloned().unwrap_or(Object::None);
            let source = take!(iter_of(&args[0]));
            accumulate_core(&[source, func, Object::Bool(false), Object::None, initial])
        }
        K_COMPRESS => {
            let [data, selectors] = args else {
                return None;
            };
            let data = take!(iter_of(data));
            let selectors = take!(iter_of(selectors));
            compress_core(&[data, selectors])
        }
        K_DROPWHILE | K_TAKEWHILE | K_FILTERFALSE | K_STARMAP => {
            let [func, it] = args else {
                return None;
            };
            let source = take!(iter_of(it));
            match kind {
                K_DROPWHILE => dropwhile_core(&[func.clone(), source, Object::Bool(false)]),
                K_TAKEWHILE => takewhile_core(&[func.clone(), source, Object::Bool(false)]),
                K_FILTERFALSE => filterfalse_core(&[func.clone(), source]),
                _ => starmap_core(&[func.clone(), source]),
            }
        }
        K_ISLICE => {
            let (start, stop, step) = match args {
                [_, stop] => (Object::Int(0), islice_arg(stop)?, Object::Int(1)),
                [_, start, stop, rest @ ..] if rest.len() <= 1 => {
                    let start = match islice_arg(start)? {
                        Object::None => Object::Int(0),
                        s => s,
                    };
                    let step = match rest {
                        [] | [Object::None] => Object::Int(1),
                        [s] => Object::Int(small_int(s).filter(|s| *s >= 1)?),
                        _ => return None,
                    };
                    (start, islice_arg(stop)?, step)
                }
                _ => return None,
            };
            let source = take!(iter_of(&args[0]));
            islice_core(&[source, start, stop, step])
        }
        K_PAIRWISE => {
            let [it] = args else {
                return None;
            };
            let source = take!(iter_of(it));
            pairwise_core(&[source])
        }
        K_ZIP_LONGEST => {
            let mut iters = Vec::with_capacity(n);
            for a in args {
                iters.push(take!(iter_of(a)));
            }
            zip_longest_core(&[fillvalue, Object::new_tuple(iters)])
        }
        K_PRODUCT => {
            let mut pools = Vec::with_capacity(n);
            for a in args {
                pools.push(take!(pool_of(a)));
            }
            let repeat = usize::try_from(repeat).ok()?;
            let mut all = Vec::with_capacity(pools.len().saturating_mul(repeat));
            for _ in 0..repeat {
                all.extend(pools.iter().cloned());
            }
            product_core(&[
                Object::new_tuple(all),
                Object::None,
                Object::Bool(false),
                Object::Bool(false),
            ])
        }
        K_PERMUTATIONS => {
            let r = match args {
                [_] | [_, Object::None] => None,
                [_, r] => Some(small_int(r).filter(|r| *r >= 0)?),
                _ => return None,
            };
            let pool = take!(pool_of(&args[0]));
            let len = match &pool {
                Object::Tuple(t) => t.len() as i64,
                _ => return None,
            };
            let r = r.unwrap_or(len);
            permutations_core(&[
                pool,
                Object::Int(r),
                Object::None,
                Object::None,
                Object::Bool(false),
                Object::Bool(r > len),
            ])
        }
        K_COMBINATIONS | K_CWR => {
            let [it, r] = args else {
                return None;
            };
            let r = small_int(r).filter(|r| *r >= 0)?;
            let pool = take!(pool_of(it));
            let len = match &pool {
                Object::Tuple(t) => t.len() as i64,
                _ => return None,
            };
            let stopped = if kind == K_CWR {
                len == 0 && r > 0
            } else {
                r > len
            };
            let build = if kind == K_CWR {
                cwr_core
            } else {
                combinations_core
            };
            build(&[
                pool,
                Object::Int(r),
                Object::None,
                Object::Bool(false),
                Object::Bool(stopped),
            ])
        }
        K_GROUPBY => {
            let key = match args {
                [_] => key,
                [_, k] if kwargs.is_empty() => k.clone(),
                _ => return None,
            };
            let source = take!(iter_of(&args[0]));
            groupby_core(&[source, key, Object::None])
        }
        K_BATCHED => {
            let [it, size] = args else {
                return None;
            };
            let size = small_int(size).filter(|s| *s >= 1)?;
            let source = take!(iter_of(it));
            batched_core(&[source, Object::Int(size), Object::Bool(false)])
        }
        _ => return None,
    };
    Some(core)
}

/// `iter(x)` for the core loop: an iterator is its own, a native
/// container builds one; anything that might run code declines.
fn pure_iter(o: &Object) -> Option<Result<Object, RuntimeError>> {
    match o {
        Object::Iter(_) | Object::Generator(_) | Object::LazyIter(_) => Some(Ok(o.clone())),
        Object::List(_)
        | Object::Tuple(_)
        | Object::Str(_)
        | Object::Range(_)
        | Object::Dict(_)
        | Object::Set(_)
        | Object::FrozenSet(_)
        | Object::Bytes(_) => Some(Ok(Object::Iter(Rc::new(RefCell::new(o.make_iter().ok()?))))),
        _ => None,
    }
}

/// `tuple(x)` for the core loop, over a tuple or a list.
fn pure_pool(o: &Object) -> Option<Result<Object, RuntimeError>> {
    match o {
        Object::Tuple(_) => Some(Ok(o.clone())),
        Object::List(l) => Some(Ok(Object::new_tuple(l.try_borrow().ok()?.clone()))),
        _ => None,
    }
}

/// `cls(*args)` for a registered exact class, built without running any
/// code (the core loop's `CALL`): `None` for every other shape.
pub(crate) fn native_new_pure(
    cls: &Rc<crate::types::TypeObject>,
    args: &[Object],
) -> Option<Object> {
    let core = native_new(
        cls.lazy_ctor.get(),
        args,
        &[],
        &mut pure_iter,
        &mut pure_pool,
    )?
    .ok()?;
    Some(with_class(core, cls))
}

/// `cls(*args, **kwargs)` for a registered exact class: `None` to run
/// the class's own `__new__`.
pub(crate) fn native_new_interp(
    interp: &mut crate::Interpreter,
    cls: &Rc<crate::types::TypeObject>,
    args: &[Object],
    kwargs: &[(String, Object)],
) -> Option<Result<Object, RuntimeError>> {
    let globals = interp.builtins_dict();
    let cell = RefCell::new(interp);
    let mut iter_of = |o: &Object| Some(cell.borrow_mut().make_iter(o, &globals));
    let mut pool_of = |o: &Object| {
        Some(
            cell.borrow_mut()
                .collect_iterable(o, &globals)
                .map(|v| match o {
                    Object::Tuple(_) => o.clone(),
                    _ => Object::new_tuple(v),
                }),
        )
    };
    let core = native_new(
        cls.lazy_ctor.get(),
        args,
        kwargs,
        &mut iter_of,
        &mut pool_of,
    )?;
    Some(core.map(|c| with_class(c, cls)))
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
    let pools: Vec<crate::object::SharedTuple> = match pools {
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
        LazyIterKind::Repeat { obj, times } => {
            vec![obj.clone(), times.map_or(Object::None, Object::Int)]
        }
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
                Object::new_tuple(slots),
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
            let pools: Vec<Object> = pools.iter().map(|p| Object::Tuple(p.clone())).collect();
            let ix: Vec<Object> = indices.iter().map(|&i| Object::Int(i as i64)).collect();
            vec![
                Object::new_tuple(pools),
                Object::new_tuple(ix),
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
                Object::new_tuple(ix),
                Object::new_tuple(cy),
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
                Object::new_tuple(ix),
                Object::Bool(*started),
                Object::Bool(*stopped),
            ]
        }
        LazyIterKind::Batched { source, n, strict } => vec![
            source.clone().unwrap_or(Object::None),
            Object::Int(*n as i64),
            Object::Bool(*strict),
        ],
        LazyIterKind::GroupBy {
            source,
            keyfunc,
            tgtkey,
            currkey,
            currvalue,
            ..
        } => {
            // A cleared (NULL) field is an empty tuple, a set one a 1-tuple.
            let field = |o: &Option<Object>| match o {
                Some(v) => Object::new_tuple_array([v.clone()]),
                None => Object::new_tuple_array([]),
            };
            vec![
                source.clone(),
                keyfunc.clone(),
                field(tgtkey),
                field(currkey),
                field(currvalue),
            ]
        }
        LazyIterKind::Grouper { parent, tgtkey } => {
            let current = matches!(
                &*parent.state.borrow(),
                LazyIterKind::GroupBy { currgrouper: Some(w), .. }
                    if std::ptr::eq(w.as_ptr(), Rc::as_ptr(l))
            );
            vec![
                Object::LazyIter(parent.clone()),
                tgtkey.clone(),
                Object::Bool(current),
            ]
        }
        LazyIterKind::Map { .. }
        | LazyIterKind::Filter { .. }
        | LazyIterKind::Zip { .. }
        | LazyIterKind::Enumerate { .. } => {
            return Err(type_error("lazy_state: not a native itertools core"))
        }
    };
    Ok(Object::new_tuple(items))
}

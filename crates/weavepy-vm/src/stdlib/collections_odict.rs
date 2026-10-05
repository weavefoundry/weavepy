//! Native `OrderedDict` operations for `_collections`.
//!
//! An `OrderedDict` instance is a dict subclass: its dict payload holds
//! the mapping, so `od[key]`, `len(od)`, `key in od` and the inherited
//! `dict` methods are the dict's own. The order lives beside it, the way
//! CPython's C `odict` keeps a linked list of nodes next to its dict: a
//! second, keys-only dict in a hidden slot (see [`order_of`]). The order
//! dict is insertion-ordered like every WeavePy dict, so appending a key,
//! moving one (`IndexMap::move_index`), and removing one are its own
//! operations. Values are always read from the payload by key, so a
//! payload changed behind the order's back (`dict.__delitem__(od, k)`)
//! surfaces as a `KeyError` on the next ordered read, as it does in
//! CPython.
//!
//! Iteration over the order is a native dict-key iterator over the order
//! dict, flagged as an `OrderedDict` one ([`crate::object::PyIterator::DictKeys`]'s
//! `odict`): a structural change of the order (an insertion, a deletion,
//! `move_to_end`) between steps raises `OrderedDict mutated during
//! iteration`, as CPython's `od_state` check does, while replacing a value
//! does not. The values and items iterators walk the order the same way
//! and read each value from the payload.

use crate::error::{key_error, key_error_object, runtime_error, type_error, RuntimeError};
use crate::object::{DictData, DictKey, DictViewKind, Object, PyIterator};
use crate::sync::{Rc, RefCell};
use crate::types::{PyInstance, TypeObject};

/// The hidden slot holding an instance's order dict. Its name isn't an
/// identifier, so no Python code can reach it.
pub(crate) const ORDER: &str = "<order>";

/// [`crate::types::TypeObject::collections_kind`] of the `OrderedDict`
/// class itself (subclasses are recognized through their MRO).
pub(crate) const KIND_ODICT: u8 = 2;

/// Whether `cls` is `OrderedDict` or a subclass of it.
fn is_odict_class(cls: &TypeObject) -> bool {
    cls.collections_kind.get() == KIND_ODICT
        || cls
            .mro
            .borrow()
            .iter()
            .any(|t| t.collections_kind.get() == KIND_ODICT)
}

/// The order dict of `inst`, created (empty) on first use: an instance
/// whose `__init__` never ran, or one built by `__new__` alone, starts
/// with no order, like CPython's freshly allocated `odict`.
pub(crate) fn order_of(inst: &PyInstance) -> Rc<RefCell<DictData>> {
    if let Some(d) = peek_order(inst) {
        return d;
    }
    let d = Rc::new(RefCell::new(DictData::default()));
    let order = Object::Dict(d.clone());
    // The collector must see the order's keys (`A.od[A] = None` is a
    // cycle through it — test_reference_loop).
    crate::gc_trace::track(&order);
    inst.slot_set(ORDER, order);
    d
}

/// The order dict of `inst`, if it has one.
#[inline]
fn peek_order(inst: &PyInstance) -> Option<Rc<RefCell<DictData>>> {
    let slots = inst.slots.try_borrow().ok()?;
    match slots.get_hinted(0, ORDER).or_else(|| slots.get(ORDER)) {
        Some(Object::Dict(d)) => Some(d.clone()),
        _ => None,
    }
}

/// The `OrderedDict` receiver of a method and its dict payload, or the
/// TypeError CPython's method descriptor raises for anything else.
fn receiver(
    args: &[Object],
    method: &str,
) -> Result<(Rc<PyInstance>, Rc<RefCell<DictData>>), RuntimeError> {
    let Some(recv) = args.first() else {
        return Err(type_error(format!(
            "descriptor '{method}' of 'collections.OrderedDict' object needs an argument"
        )));
    };
    if let Object::Instance(inst) = recv {
        if is_odict_class(inst.cls_raw()) {
            if let Some(Object::Dict(d)) = inst.native.get() {
                return Ok((inst.clone(), d.clone()));
            }
        }
    }
    Err(type_error(format!(
        "descriptor '{method}' for 'collections.OrderedDict' objects doesn't apply to a '{}' object",
        crate::builtins::class_of(recv).name
    )))
}

/// Bind a method's arguments (after the receiver) to `names`, Argument
/// Clinic style: positional first, then keywords; the first `required`
/// are mandatory.
fn bind(
    fname: &str,
    args: &[Object],
    kwargs: &[(String, Object)],
    names: &[&str],
    required: usize,
) -> Result<Vec<Option<Object>>, RuntimeError> {
    if args.len() > names.len() {
        return Err(type_error(format!(
            "{fname}() takes at most {} argument{} ({} given)",
            names.len(),
            if names.len() == 1 { "" } else { "s" },
            args.len()
        )));
    }
    let mut out: Vec<Option<Object>> = names.iter().map(|_| None).collect();
    for (slot, a) in out.iter_mut().zip(args) {
        *slot = Some(a.clone());
    }
    for (k, v) in kwargs {
        let Some(i) = names.iter().position(|n| n == k) else {
            return Err(type_error(format!(
                "{fname}() got an unexpected keyword argument '{k}'"
            )));
        };
        if out[i].is_some() {
            return Err(type_error(format!(
                "argument for {fname}() given by name ('{k}') and position ({})",
                i + 1
            )));
        }
        out[i] = Some(v.clone());
    }
    for (i, name) in names.iter().enumerate().take(required) {
        if out[i].is_none() {
            return Err(type_error(format!(
                "{fname}() missing required argument '{name}' (pos {})",
                i + 1
            )));
        }
    }
    Ok(out)
}

/// The truth of a `bool`-converted argument (`last=`).
fn truth(v: &Object) -> Result<bool, RuntimeError> {
    match v {
        Object::Bool(b) => Ok(*b),
        Object::Int(i) => Ok(*i != 0),
        Object::None => Ok(false),
        other => crate::builtins::reentrant_interp()?.op_truth(other),
    }
}

/// The position of `key` in the order dict (honouring keys whose
/// `__eq__`/`__hash__` run Python).
fn index_of(d: &Rc<RefCell<DictData>>, key: &Object) -> Result<Option<usize>, RuntimeError> {
    if let Some(probe) = crate::object::LeafProbe::new(key) {
        if let Ok(m) = d.try_borrow() {
            match m.get_index_of(&probe) {
                Some(i) => return Ok(Some(i)),
                None if probe.miss_is_exact() => return Ok(None),
                None => {}
            }
        }
    }
    crate::object::dict_index_of(d, key)
}

/// A fresh iterator over the order of `od` (`main` is its dict payload):
/// its keys, or its values or items read from the payload, reversed or
/// not, holding `od` alive.
pub(crate) fn order_iter(
    od: &Object,
    main: Rc<RefCell<DictData>>,
    order: Rc<RefCell<DictData>>,
    kind: DictViewKind,
    reverse: bool,
) -> Object {
    let len = order.borrow().len();
    // The order's state is captured now, as CPython's iterator snapshots
    // `od_state` at creation.
    let watch = Some(crate::object::DictWatch::new(&order));
    let it = Object::Iter(Rc::new(RefCell::new(PyIterator::DictKeys {
        kind,
        index: if reverse { len } else { 0 },
        dict: Some(order),
        len,
        watch,
        reverse,
        owner: Some(od.clone()),
        odict: Some(main),
    })));
    crate::gc_trace::track(&it);
    it
}

/// `OrderedDict.__setitem__(self, key, value)`.
fn od_setitem(args: &[Object]) -> Result<Object, RuntimeError> {
    let (inst, main) = receiver(args, "__setitem__")?;
    let [_, key, value] = args else {
        return Err(type_error(format!(
            "expected 2 arguments, got {}",
            args.len().saturating_sub(1)
        )));
    };
    crate::builtins::ensure_hashable(key)?;
    let order = order_of(&inst);
    let old = crate::builtins::dict_insert(&main, key.clone(), value.clone())?;
    // A new key gets a node; an existing one keeps its place (re-storing
    // `None` in the order dict moves nothing).
    let fresh = old.is_none() || !order_contains(&order, key)?;
    if fresh {
        crate::builtins::dict_insert(&order, key.clone(), Object::None)?;
    }
    drop(old);
    Ok(Object::None)
}

fn order_contains(order: &Rc<RefCell<DictData>>, key: &Object) -> Result<bool, RuntimeError> {
    Ok(index_of(order, key)?.is_some())
}

/// `od[key] = value`'s leaf half (see `leaf_builtins::Fast`): a `str` or
/// `int` key on an instance whose order already exists, settled by exact
/// native equality in both dicts. Both are probed before either changes.
fn od_setitem_leaf(args: &[Object]) -> Option<Result<Object, RuntimeError>> {
    let [Object::Instance(inst), key, value] = args else {
        return None;
    };
    if crate::capi_watchers::dicts_active() {
        return None;
    }
    let Some(Object::Dict(main)) = inst.native.get() else {
        return None;
    };
    let order = peek_order(inst)?;
    let probe = crate::object::LeafProbe::new(key)?;
    let mut m = main.try_borrow_mut().ok()?;
    let mut o = order.try_borrow_mut().ok()?;
    let in_main = m.get_index_of(&probe);
    let in_order = o.get_index_of(&probe).is_some();
    if (in_main.is_none() || !in_order) && !probe.miss_is_exact() {
        return None;
    }
    let old = match in_main {
        Some(i) => {
            let (_, slot) = m.get_index_mut(i)?;
            Some(std::mem::replace(slot, value.clone()))
        }
        None => {
            m.insert(DictKey(key.clone()), value.clone());
            None
        }
    };
    if !in_order {
        o.insert(DictKey(key.clone()), Object::None);
    }
    drop((m, o));
    let changed = old.as_ref().is_none_or(|o| !o.is_same(value));
    if in_main.is_none() {
        crate::object::dict_watch_bump(&main);
    }
    if changed {
        crate::object::dict_mutation_event(&main);
    }
    if !in_order {
        crate::object::dict_watch_bump(&order);
    }
    drop(old);
    Some(Ok(Object::None))
}

/// `OrderedDict.__delitem__(self, key)`: the node goes first, then the
/// payload entry (a `KeyError` if the payload lacks the key).
fn od_delitem(args: &[Object]) -> Result<Object, RuntimeError> {
    let (inst, main) = receiver(args, "__delitem__")?;
    let [_, key] = args else {
        return Err(type_error(format!(
            "expected 1 argument, got {}",
            args.len().saturating_sub(1)
        )));
    };
    crate::builtins::ensure_hashable(key)?;
    if let Some(order) = peek_order(&inst) {
        crate::builtins::dict_remove(&order, key)?;
    }
    match crate::builtins::dict_remove(&main, key)? {
        Some(entry) => {
            drop(entry);
            Ok(Object::None)
        }
        None => Err(key_error_object(key.clone())),
    }
}

/// `OrderedDict.__iter__(self)`.
fn od_iter(args: &[Object]) -> Result<Object, RuntimeError> {
    let (inst, main) = receiver(args, "__iter__")?;
    if args.len() != 1 {
        return Err(type_error("__iter__() takes no arguments"));
    }
    Ok(order_iter(
        &args[0],
        main,
        order_of(&inst),
        DictViewKind::Keys,
        false,
    ))
}

/// `OrderedDict.__reversed__(self)`.
fn od_reversed(args: &[Object]) -> Result<Object, RuntimeError> {
    let (inst, main) = receiver(args, "__reversed__")?;
    if args.len() != 1 {
        return Err(type_error("__reversed__() takes no arguments"));
    }
    Ok(order_iter(
        &args[0],
        main,
        order_of(&inst),
        DictViewKind::Keys,
        true,
    ))
}

/// `OrderedDict.move_to_end(self, key, last=True)`.
fn od_move_to_end(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let (inst, _) = receiver(args, "move_to_end")?;
    let bound = bind("move_to_end", &args[1..], kwargs, &["key", "last"], 1)?;
    let key = bound[0].clone().expect("required");
    let last = match &bound[1] {
        Some(v) => truth(v)?,
        None => true,
    };
    let order = order_of(&inst);
    let (len, at_end) = {
        let o = order.borrow();
        let len = o.len();
        let end = if last { len.wrapping_sub(1) } else { 0 };
        (
            len,
            o.get_index(end).is_some_and(|(k, _)| k.0.is_same(&key)),
        )
    };
    if len == 0 {
        return Err(key_error_object(key));
    }
    if at_end {
        return Ok(Object::None);
    }
    crate::builtins::ensure_hashable(&key)?;
    let Some(i) = index_of(&order, &key)? else {
        return Err(key_error_object(key));
    };
    let mut o = order.borrow_mut();
    // The comparison may have run Python that resized the order.
    let n = o.len();
    if i < n {
        let end = if last { n - 1 } else { 0 };
        if i != end {
            o.move_index(i, end);
            drop(o);
            crate::object::dict_watch_bump(&order);
        }
    }
    Ok(Object::None)
}

/// Remove the node at `index` of the order and the payload's entry for
/// its key: `(key, value)`.
fn pop_node(
    main: &Rc<RefCell<DictData>>,
    order: &Rc<RefCell<DictData>>,
    index: usize,
) -> Result<(Object, Object), RuntimeError> {
    let (key, _) = order
        .borrow_mut()
        .shift_remove_index(index)
        .ok_or_else(|| runtime_error("OrderedDict mutated during iteration"))?;
    crate::object::dict_watch_bump(order);
    let key = key.0;
    match crate::builtins::dict_remove(main, &key)? {
        Some((_, value)) => Ok((key, value)),
        None => Err(key_error_object(key)),
    }
}

/// `OrderedDict.popitem(self, last=True)`.
fn od_popitem(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let (inst, main) = receiver(args, "popitem")?;
    let bound = bind("popitem", &args[1..], kwargs, &["last"], 0)?;
    let last = match &bound[0] {
        Some(v) => truth(v)?,
        None => true,
    };
    let order = order_of(&inst);
    let len = order.borrow().len();
    if len == 0 {
        return Err(key_error("dictionary is empty"));
    }
    let (key, value) = pop_node(&main, &order, if last { len - 1 } else { 0 })?;
    Ok(Object::new_tuple_array([key, value]))
}

/// `OrderedDict.pop(self, key, default=<unset>)`.
fn od_pop(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let (inst, main) = receiver(args, "pop")?;
    let bound = bind("pop", &args[1..], kwargs, &["key", "default"], 1)?;
    let key = bound[0].clone().expect("required");
    let default = bound[1].clone();
    crate::builtins::ensure_hashable(&key)?;
    // Only a key with a node is popped (CPython's `_odict_popkey_hash`):
    // the node first, then the payload's entry.
    let removed = match peek_order(&inst) {
        Some(order) => crate::builtins::dict_remove(&order, &key)?.is_some(),
        None => false,
    };
    if removed {
        if let Some((_, value)) = crate::builtins::dict_remove(&main, &key)? {
            return Ok(value);
        }
    }
    default.ok_or_else(|| key_error_object(key))
}

/// `OrderedDict.clear(self)`.
fn od_clear(args: &[Object]) -> Result<Object, RuntimeError> {
    let (inst, main) = receiver(args, "clear")?;
    if args.len() != 1 {
        return Err(type_error(format!(
            "OrderedDict.clear() takes no arguments ({} given)",
            args.len() - 1
        )));
    }
    crate::builtins::dict_clear(&[Object::Dict(main)])?;
    if let Some(order) = peek_order(&inst) {
        crate::builtins::dict_clear(&[Object::Dict(order.clone())])?;
        // An emptied order still invalidates its iterators.
        crate::object::dict_watch_bump(&order);
    }
    Ok(Object::None)
}

/// `setdefault(self, key, default)` for an exact `OrderedDict` (a subclass
/// dispatches through its own `__contains__`/`__getitem__`/`__setitem__`
/// in Python).
fn od_setdefault(args: &[Object]) -> Result<Object, RuntimeError> {
    let (_, main) = receiver(args, "setdefault")?;
    let [_, key, default] = args else {
        return Err(type_error("setdefault() expects a key and a default"));
    };
    crate::builtins::ensure_dict_key(key)?;
    if let Some(v) = crate::builtins::dict_lookup(&main, key)? {
        return Ok(v);
    }
    od_setitem(args)?;
    Ok(default.clone())
}

/// `keys_equal(a, b)`: CPython's `_odict_keys_equal` for two
/// `OrderedDict`s whose dict comparison already succeeded: the orders
/// hold equal keys in the same sequence. A key comparison that changes
/// either order raises `RuntimeError` (gh-119004).
fn od_keys_equal(args: &[Object]) -> Result<Object, RuntimeError> {
    let (a, _) = receiver(args, "__eq__")?;
    let (b, _) = receiver(args.get(1..).unwrap_or_default(), "__eq__")?;
    let (oa, ob) = (order_of(&a), order_of(&b));
    let wa = crate::object::DictWatch::new(&oa);
    let wb = crate::object::DictWatch::new(&ob);
    let (na, nb) = (oa.borrow().len(), ob.borrow().len());
    let mut i = 0;
    loop {
        let ka = oa.borrow().get_index(i).map(|(k, _)| k.0.clone());
        let kb = ob.borrow().get_index(i).map(|(k, _)| k.0.clone());
        let (ka, kb) = match (ka, kb) {
            (None, None) => return Ok(Object::Bool(true)),
            (Some(ka), Some(kb)) => (ka, kb),
            _ => return Ok(Object::Bool(false)),
        };
        let eq = ka.is_same(&kb)
            || crate::builtins::reentrant_interp()?.reentrant_py_eq_bool(&ka, &kb)?;
        if wa.changed() || wb.changed() || oa.borrow().len() != na || ob.borrow().len() != nb {
            return Err(runtime_error("OrderedDict mutated during iteration"));
        }
        if !eq {
            return Ok(Object::Bool(false));
        }
        i += 1;
    }
}

/// `update_fast(self, other)`: `self.update(other)` done natively when it
/// can be, before anything changes: `other` an exact `dict`, an exact
/// `OrderedDict` (in its order), or a `list`/`tuple` of exact two-item
/// tuples and lists. The caller vouches that `self`'s class keeps the
/// native `__setitem__`. `False` (nothing done) for every other shape.
fn od_update_fast(args: &[Object]) -> Result<Object, RuntimeError> {
    let [od, other] = args else {
        return Err(type_error(
            "update_fast() expects an OrderedDict and a source",
        ));
    };
    receiver(args, "update")?;
    let pairs: Vec<(Object, Object)> = match other {
        Object::Dict(d) => d
            .borrow()
            .iter()
            .map(|(k, v)| (k.0.clone(), v.clone()))
            .collect(),
        Object::Instance(inst) if inst.cls_raw().collections_kind.get() == KIND_ODICT => {
            let Some(Object::Dict(main)) = inst.native.get() else {
                return Ok(Object::Bool(false));
            };
            let order = order_of(inst);
            let keys: Vec<Object> = order.borrow().keys().map(|k| k.0.clone()).collect();
            let mut pairs = Vec::with_capacity(keys.len());
            for k in keys {
                // Only plain keys: a lookup must not run Python here.
                let Some(probe) = crate::object::LeafProbe::new(&k) else {
                    return Ok(Object::Bool(false));
                };
                let v = main.borrow().get(&probe).cloned();
                match v {
                    Some(v) => pairs.push((k, v)),
                    None => return Ok(Object::Bool(false)),
                }
            }
            pairs
        }
        Object::List(_) | Object::Tuple(_) => {
            let items: Vec<Object> = match other {
                Object::List(l) => l.borrow().clone(),
                Object::Tuple(t) => t.iter().cloned().collect(),
                _ => unreachable!(),
            };
            let mut pairs = Vec::with_capacity(items.len());
            for item in &items {
                match item {
                    Object::Tuple(t) if t.len() == 2 => pairs.push((t[0].clone(), t[1].clone())),
                    Object::List(l) if l.borrow().len() == 2 => {
                        let l = l.borrow();
                        pairs.push((l[0].clone(), l[1].clone()));
                    }
                    _ => return Ok(Object::Bool(false)),
                }
            }
            pairs
        }
        _ => return Ok(Object::Bool(false)),
    };
    for (k, v) in pairs {
        od_setitem(&[od.clone(), k, v])?;
    }
    Ok(Object::Bool(true))
}

// ---------------------------------------------------------------------
// Leaf halves (see `leaf_builtins::Fast`): the common shapes with a `str`
// or `int` key settled by exact native equality, so no Python runs.
// Anything else (another key kind, a keyword argument, a key only a
// Python `__eq__` could match) declines before changing anything.

/// The payload and order of an `OrderedDict` receiver that has an order.
#[inline]
fn leaf_parts(recv: &Object) -> Option<(Rc<RefCell<DictData>>, Rc<RefCell<DictData>>)> {
    let Object::Instance(inst) = recv else {
        return None;
    };
    if !is_odict_class(inst.cls_raw()) || crate::capi_watchers::dicts_active() {
        return None;
    }
    let Some(Object::Dict(main)) = inst.native.get() else {
        return None;
    };
    Some((main.clone(), peek_order(inst)?))
}

/// `od.move_to_end(key[, last])`.
fn od_move_to_end_leaf(args: &[Object]) -> Option<Result<Object, RuntimeError>> {
    let (key, last) = match args {
        [_, key] => (key, true),
        [_, key, Object::Bool(last)] => (key, *last),
        _ => return None,
    };
    let (_, order) = leaf_parts(&args[0])?;
    let probe = crate::object::LeafProbe::new(key)?;
    let mut o = order.try_borrow_mut().ok()?;
    let Some(i) = o.get_index_of(&probe) else {
        return probe
            .miss_is_exact()
            .then(|| Err(key_error_object(key.clone())));
    };
    let end = if last { o.len() - 1 } else { 0 };
    if i != end {
        o.move_index(i, end);
        drop(o);
        crate::object::dict_watch_bump(&order);
    }
    Some(Ok(Object::None))
}

/// Remove `key` (at `index` of the order) from both dicts: its value, or
/// `None` (nothing changed) when the payload lacks it.
fn leaf_unlink(
    main: &Rc<RefCell<DictData>>,
    order: &Rc<RefCell<DictData>>,
    index: usize,
    probe: &crate::object::LeafProbe<'_>,
) -> Option<Object> {
    let mut m = main.try_borrow_mut().ok()?;
    let mut o = order.try_borrow_mut().ok()?;
    let at = m.get_index_of(probe)?;
    let (k, v) = m.shift_remove_index(at)?;
    let node = o.shift_remove_index(index);
    drop((m, o));
    crate::object::dict_watch_bump(main);
    crate::object::dict_mutation_event(main);
    crate::object::dict_watch_bump(order);
    drop((k, node));
    Some(v)
}

/// `od.popitem([last])`.
fn od_popitem_leaf(args: &[Object]) -> Option<Result<Object, RuntimeError>> {
    let last = match args {
        [_] => true,
        [_, Object::Bool(last)] => *last,
        _ => return None,
    };
    let (main, order) = leaf_parts(&args[0])?;
    let (index, key) = {
        let o = order.try_borrow().ok()?;
        if o.is_empty() {
            return Some(Err(key_error("dictionary is empty")));
        }
        let index = if last { o.len() - 1 } else { 0 };
        (index, o.get_index(index)?.0 .0.clone())
    };
    let probe = crate::object::LeafProbe::new(&key)?;
    let value = leaf_unlink(&main, &order, index, &probe)?;
    Some(Ok(Object::new_tuple_array([key, value])))
}

/// `od.pop(key[, default])`.
fn od_pop_leaf(args: &[Object]) -> Option<Result<Object, RuntimeError>> {
    let (key, default) = match args {
        [_, key] => (key, None),
        [_, key, default] => (key, Some(default)),
        _ => return None,
    };
    let (main, order) = leaf_parts(&args[0])?;
    let probe = crate::object::LeafProbe::new(key)?;
    let index = order.try_borrow().ok()?.get_index_of(&probe);
    let Some(index) = index else {
        if !probe.miss_is_exact() {
            return None;
        }
        return Some(
            default
                .cloned()
                .ok_or_else(|| key_error_object(key.clone())),
        );
    };
    leaf_unlink(&main, &order, index, &probe).map(Ok)
}

/// The view classes `keys()`, `values()` and `items()` hand out (CPython's
/// `odict_keys`, `odict_values` and `odict_items`), kept on the
/// `OrderedDict` class by [`install_odict`].
struct ViewTypes([crate::sync::Weak<TypeObject>; 3]);

/// `install_odict(cls, keys, values, items)`: mark the `OrderedDict` class
/// (exactly) and record its view classes.
fn install_odict(args: &[Object]) -> Result<Object, RuntimeError> {
    let [Object::Type(cls), Object::Type(k), Object::Type(v), Object::Type(i)] = args else {
        return Err(type_error(
            "install_odict() expects the class and its view classes",
        ));
    };
    cls.collections_kind.set(KIND_ODICT);
    let views = Rc::new(ViewTypes([
        Rc::downgrade(k),
        Rc::downgrade(v),
        Rc::downgrade(i),
    ]));
    let _ = cls
        .native_ext
        .set(crate::rc_unsize!(views => dyn std::any::Any + Send + Sync));
    Ok(Object::None)
}

/// A new view of `od` of the given kind (0 keys, 1 values, 2 items): an
/// instance of the view class with its `_mapping` slot set, as
/// `MappingView.__init__` leaves it.
fn make_view(args: &[Object], kind: usize, method: &str) -> Result<Object, RuntimeError> {
    let (inst, _) = receiver(args, method)?;
    if args.len() != 1 {
        return Err(type_error(format!(
            "{method}() takes no arguments ({} given)",
            args.len() - 1
        )));
    }
    let cls = inst
        .cls_raw()
        .mro
        .borrow()
        .iter()
        .find(|t| t.collections_kind.get() == KIND_ODICT)
        .and_then(|t| t.native_ext.get()?.downcast_ref::<ViewTypes>()?.0[kind].upgrade())
        .ok_or_else(|| type_error("OrderedDict views are not installed"))?;
    let view = PyInstance::new_deferred(cls);
    view.slot_set("_mapping", args[0].clone());
    Ok(Object::Instance(view))
}

fn od_keys(args: &[Object]) -> Result<Object, RuntimeError> {
    make_view(args, 0, "keys")
}

fn od_values(args: &[Object]) -> Result<Object, RuntimeError> {
    make_view(args, 1, "values")
}

fn od_items(args: &[Object]) -> Result<Object, RuntimeError> {
    make_view(args, 2, "items")
}

/// A view's `__iter__`/`__reversed__`: the order iterator of the view's
/// `OrderedDict` (its `_mapping`).
fn view_iter(args: &[Object], kind: DictViewKind, reverse: bool) -> Result<Object, RuntimeError> {
    let [Object::Instance(view)] = args else {
        return Err(type_error("__iter__() takes no arguments"));
    };
    let Some(od) = view.slot_get("_mapping") else {
        return Err(crate::error::attribute_error("_mapping"));
    };
    let (inst, main) = receiver(std::slice::from_ref(&od), "__iter__")?;
    Ok(order_iter(&od, main, order_of(&inst), kind, reverse))
}

fn keys_iter(args: &[Object]) -> Result<Object, RuntimeError> {
    view_iter(args, DictViewKind::Keys, false)
}

fn keys_reversed(args: &[Object]) -> Result<Object, RuntimeError> {
    view_iter(args, DictViewKind::Keys, true)
}

fn values_iter(args: &[Object]) -> Result<Object, RuntimeError> {
    view_iter(args, DictViewKind::Values, false)
}

fn values_reversed(args: &[Object]) -> Result<Object, RuntimeError> {
    view_iter(args, DictViewKind::Values, true)
}

fn items_iter(args: &[Object]) -> Result<Object, RuntimeError> {
    view_iter(args, DictViewKind::Items, false)
}

fn items_reversed(args: &[Object]) -> Result<Object, RuntimeError> {
    view_iter(args, DictViewKind::Items, true)
}

/// The module's `OrderedDict` callables: `(export, name, body, keyword
/// body)`.
pub(crate) type Kw = fn(&[Object], &[(String, Object)]) -> Result<Object, RuntimeError>;

/// A builtin's positional body.
pub(crate) type Body = fn(&[Object]) -> Result<Object, RuntimeError>;

pub(crate) fn exports() -> Vec<(&'static str, &'static str, Body, Option<Kw>)> {
    vec![
        ("od_setitem", "__setitem__", od_setitem, None),
        ("od_delitem", "__delitem__", od_delitem, None),
        ("od_iter", "__iter__", od_iter, None),
        ("od_reversed", "__reversed__", od_reversed, None),
        ("od_keys", "keys", od_keys, None),
        ("od_values", "values", od_values, None),
        ("od_items", "items", od_items, None),
        ("odv_keys_iter", "__iter__", keys_iter, None),
        ("odv_keys_reversed", "__reversed__", keys_reversed, None),
        ("odv_values_iter", "__iter__", values_iter, None),
        ("odv_values_reversed", "__reversed__", values_reversed, None),
        ("odv_items_iter", "__iter__", items_iter, None),
        ("odv_items_reversed", "__reversed__", items_reversed, None),
        (
            "od_move_to_end",
            "move_to_end",
            (|a| od_move_to_end(a, &[])) as Body,
            Some(od_move_to_end as Kw),
        ),
        (
            "od_popitem",
            "popitem",
            (|a| od_popitem(a, &[])) as Body,
            Some(od_popitem as Kw),
        ),
        (
            "od_pop",
            "pop",
            (|a| od_pop(a, &[])) as Body,
            Some(od_pop as Kw),
        ),
        ("od_clear", "clear", od_clear, None),
        ("od_setdefault", "setdefault", od_setdefault, None),
        ("od_keys_equal", "keys_equal", od_keys_equal, None),
        ("od_update_fast", "update_fast", od_update_fast, None),
        ("install_odict", "install_odict", install_odict, None),
    ]
}

/// The exports whose whole body runs no Python code (receiver checks,
/// iterator and view construction).
pub(crate) const LEAVES: [&str; 11] = [
    "od_iter",
    "od_reversed",
    "od_keys",
    "od_values",
    "od_items",
    "odv_keys_iter",
    "odv_keys_reversed",
    "odv_values_iter",
    "odv_values_reversed",
    "odv_items_iter",
    "odv_items_reversed",
];

/// The exports with leaf halves.
pub(crate) fn leaf_halves() -> [(&'static str, crate::leaf_builtins::Fast); 4] {
    [
        ("od_setitem", od_setitem_leaf),
        ("od_move_to_end", od_move_to_end_leaf),
        ("od_popitem", od_popitem_leaf),
        ("od_pop", od_pop_leaf),
    ]
}

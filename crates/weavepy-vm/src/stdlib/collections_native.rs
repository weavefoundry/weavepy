//! `_weave_collections`: native sequence operations for the Python
//! `_collections.deque` stand-in.
//!
//! CPython documents `deque.append`/`appendleft`/`pop`/`popleft` as
//! thread-safe (the C deque mutates under the GIL, and the
//! free-threaded build wraps each method in a per-object critical
//! section). `queue.SimpleQueue`, `asyncio`'s `call_soon_threadsafe`
//! (an `append` from a foreign thread against the loop's `popleft`),
//! and `ThreadPoolExecutor`'s work queue all lean on that guarantee.
//! A pure-Python method body is not atomic in WeavePy: the GIL is handed
//! off at eval-breaker checkpoints inside the method, and under
//! `-X gil=0` two threads run the body concurrently, so a racing pair
//! of `popleft` calls could both read the same slot and lose a head
//! increment (the item was delivered twice and another never at all:
//! `test_rfc0076_gil0`'s `ThreadPoolExecutor.map` hung on the missing
//! future).
//!
//! The end operations run the whole operation while holding the
//! backing list's `GilCell` borrow. A builtin call has no eval-breaker
//! checkpoint, so the operation is atomic under the GIL; under `gil=0`
//! the cell's reentrant mutex serialises concurrent callers. They are
//! adopted by `_collections.deque` the same way `_queue.SimpleQueue`
//! adopts `_weave_queue.simplequeue_put`: as class attributes with
//! `binds_instance`, so `d.append` is a `builtin_function_or_method`
//! like the C accelerator's.
//!
//! The layout mirrors the Python class: `_data` is the backing list,
//! `_head` the count of consumed slots at its front, `_maxlen` the
//! bound (or `None`), `_state` the mutation counter live iterators
//! compare against.

use crate::sync::Rc;
use crate::sync::RefCell;

use crate::error::{index_error, runtime_error, stop_iteration, type_error, RuntimeError};
use crate::import::ModuleCache;
use crate::object::{BuiltinFn, DictData, DictKey, Object, PyModule};
use crate::types::PyInstance;

/// The receiver must be a deque instance (any class whose `_data` slot
/// is the backing list). CPython's method descriptor rejects a foreign
/// receiver with this exact wording (gh-92063).
/// A deque's state, read once per operation: the backing list, and the
/// `_head` / `_maxlen` / `_state` slots through one borrow of the slot
/// store (each was a separate borrow and name scan before).
struct DequeState<'a> {
    slots: crate::sync::RefMut<'a, crate::types::SlotStorage>,
    data: Rc<RefCell<Vec<Object>>>,
}

// The slot positions `deque.__init__` assigns in order (hints only; the
// name is always verified).
const SLOT_DATA: usize = 0;
const SLOT_HEAD: usize = 1;
const SLOT_MAXLEN: usize = 2;
const SLOT_STATE: usize = 3;

impl DequeState<'_> {
    fn head(&self) -> usize {
        match self.slots.get_hinted(SLOT_HEAD, "_head") {
            Some(Object::Int(h)) if *h >= 0 => *h as usize,
            _ => 0,
        }
    }

    fn set_head(&mut self, h: usize) {
        match self.slots.get_hinted_mut(SLOT_HEAD, "_head") {
            Some(slot) => *slot = Object::Int(h as i64),
            None => self
                .slots
                .insert("_head", Object::Int(h as i64))
                .map_or((), drop),
        }
    }

    fn maxlen(&self) -> Option<usize> {
        match self.slots.get_hinted(SLOT_MAXLEN, "_maxlen") {
            Some(Object::Int(m)) if *m >= 0 => Some(*m as usize),
            _ => None,
        }
    }

    fn bump_state(&mut self) {
        match self.slots.get_hinted_mut(SLOT_STATE, "_state") {
            Some(Object::Int(s)) => *s = s.wrapping_add(1),
            Some(slot) => *slot = Object::Int(1),
            None => self.slots.insert("_state", Object::Int(1)).map_or((), drop),
        }
    }
}

fn receiver<'a>(args: &'a [Object], method: &str) -> Result<DequeState<'a>, RuntimeError> {
    let recv = args
        .first()
        .ok_or_else(|| type_error(format!("unbound method deque.{method}() needs an argument")))?;
    if let Object::Instance(inst) = recv {
        let slots = inst.slots.borrow_mut();
        if let Some(Object::List(data)) = slots.get_hinted(SLOT_DATA, "_data") {
            let data = data.clone();
            return Ok(DequeState { slots, data });
        }
    }
    Err(type_error(format!(
        "descriptor '{method}' for 'collections.deque' objects doesn't apply to a '{}' object",
        crate::builtins::class_of(recv).name
    )))
}

/// Read-only helper for the iterator paths that still address the
/// instance directly.
fn head(inst: &PyInstance) -> usize {
    match inst.slot_get("_head") {
        Some(Object::Int(h)) if h >= 0 => h as usize,
        _ => 0,
    }
}

fn popleft_locked(st: &mut DequeState<'_>, d: &mut Vec<Object>) -> Result<Object, RuntimeError> {
    let mut h = st.head();
    if h >= d.len() {
        return Err(index_error("pop from an empty deque"));
    }
    st.bump_state();
    let x = std::mem::replace(&mut d[h], Object::None);
    h += 1;
    if h >= d.len() {
        d.clear();
        h = 0;
    } else if h >= 32 && h * 2 >= d.len() {
        d.drain(..h);
        h = 0;
    }
    st.set_head(h);
    Ok(x)
}

fn deque_append(args: &[Object]) -> Result<Object, RuntimeError> {
    let mut st = receiver(args, "append")?;
    let x = match args {
        [_, x] => x.clone(),
        [_] => {
            return Err(type_error(
                "deque.append() takes exactly one argument (0 given)",
            ))
        }
        _ => {
            return Err(type_error(format!(
                "deque.append() takes exactly one argument ({} given)",
                args.len() - 1
            )))
        }
    };
    let data = st.data.clone();
    let trimmed;
    {
        let mut d = data.borrow_mut();
        st.bump_state();
        d.push(x);
        trimmed = match st.maxlen() {
            Some(m) if d.len() - st.head() > m => Some(popleft_locked(&mut st, &mut d)?),
            _ => None,
        };
    }
    drop(st);
    if trimmed.is_some() {
        // A bounded deque released its oldest item (leaf contract).
        crate::gc_trace::mark_maybe_dead();
    }
    drop(trimmed);
    Ok(Object::None)
}

fn deque_appendleft(args: &[Object]) -> Result<Object, RuntimeError> {
    let mut st = receiver(args, "appendleft")?;
    let x = match args {
        [_, x] => x.clone(),
        _ => {
            return Err(type_error(format!(
                "deque.appendleft() takes exactly one argument ({} given)",
                args.len().saturating_sub(1)
            )))
        }
    };
    let data = st.data.clone();
    let trimmed;
    {
        let mut d = data.borrow_mut();
        st.bump_state();
        let mut h = st.head();
        if h == 0 {
            h = std::cmp::max(8, d.len() / 2);
            d.splice(0..0, std::iter::repeat_n(Object::None, h));
        }
        h -= 1;
        d[h] = x;
        st.set_head(h);
        trimmed = match st.maxlen() {
            Some(m) if d.len() - h > m => d.pop(),
            _ => None,
        };
    }
    drop(st);
    if trimmed.is_some() {
        crate::gc_trace::mark_maybe_dead();
    }
    drop(trimmed);
    Ok(Object::None)
}

fn deque_pop(args: &[Object]) -> Result<Object, RuntimeError> {
    let mut st = receiver(args, "pop")?;
    if args.len() != 1 {
        return Err(type_error(format!(
            "deque.pop() takes no arguments ({} given)",
            args.len() - 1
        )));
    }
    let data = st.data.clone();
    let mut d = data.borrow_mut();
    let h = st.head();
    if d.len() <= h {
        return Err(index_error("pop from an empty deque"));
    }
    st.bump_state();
    let x = d.pop().expect("len checked");
    if d.len() == h && h != 0 {
        d.clear();
        st.set_head(0);
    }
    Ok(x)
}

fn deque_popleft(args: &[Object]) -> Result<Object, RuntimeError> {
    let mut st = receiver(args, "popleft")?;
    if args.len() != 1 {
        return Err(type_error(format!(
            "deque.popleft() takes no arguments ({} given)",
            args.len() - 1
        )));
    }
    let data = st.data.clone();
    let mut d = data.borrow_mut();
    popleft_locked(&mut st, &mut d)
}

fn deque_len(args: &[Object]) -> Result<Object, RuntimeError> {
    let st = receiver(args, "__len__")?;
    if args.len() != 1 {
        return Err(type_error("deque.__len__() takes no arguments"));
    }
    let d = st.data.borrow();
    Ok(Object::Int(d.len().saturating_sub(st.head()) as i64))
}

fn deque_bool(args: &[Object]) -> Result<Object, RuntimeError> {
    let st = receiver(args, "__bool__")?;
    if args.len() != 1 {
        return Err(type_error("deque.__bool__() takes no arguments"));
    }
    let d = st.data.borrow();
    Ok(Object::Bool(d.len() > st.head()))
}

fn deque_getitem(args: &[Object]) -> Result<Object, RuntimeError> {
    let [_, index] = args else {
        receiver(args, "__getitem__")?;
        return Err(type_error("deque.__getitem__() takes one argument"));
    };
    let mut index = crate::builtins::coerce_index_object(index)?
        .as_i64()
        .ok_or_else(|| index_error("deque index out of range"))?;
    let st = receiver(args, "__getitem__")?;
    let d = st.data.borrow();
    let h = st.head();
    let n = d.len().saturating_sub(h) as i64;
    if index < 0 {
        index += n;
    }
    if index < 0 || index >= n {
        return Err(index_error("deque index out of range"));
    }
    Ok(d[h + index as usize].clone())
}

fn deque_rotate(args: &[Object]) -> Result<Object, RuntimeError> {
    let n = match args {
        [_] => 1,
        [_, n] => crate::builtins::coerce_index_i64(n)?,
        _ => {
            receiver(args, "rotate")?;
            return Err(type_error("deque.rotate() takes at most one argument"));
        }
    };
    let mut st = receiver(args, "rotate")?;
    let data = st.data.clone();
    let mut d = data.borrow_mut();
    let mut h = st.head().min(d.len());
    let len = d.len() - h;
    if len <= 1 {
        return Ok(Object::None);
    }
    st.bump_state();
    let right = n.rem_euclid(len as i64) as usize;
    if right == 0 {
        return Ok(Object::None);
    }
    if right <= len / 2 {
        if h < right {
            let slack = right.max(len / 2).max(8);
            d.splice(0..0, std::iter::repeat_n(Object::None, slack));
            h += slack;
        }
        let end = d.len();
        for i in 0..right {
            d.swap(h - right + i, end - right + i);
        }
        d.truncate(end - right);
        st.set_head(h - right);
    } else {
        let left = len - right;
        d.reserve(left);
        for i in 0..left {
            let value = std::mem::replace(&mut d[h + i], Object::None);
            d.push(value);
        }
        h += left;
        if h >= 32 && h * 2 >= d.len() {
            d.drain(..h);
            h = 0;
        }
        st.set_head(h);
    }
    Ok(Object::None)
}

fn deque_iterator_index(args: &[Object]) -> Result<Object, RuntimeError> {
    receiver(args, "__iter__")?;
    let [_, index] = args else {
        return Err(type_error("deque iterator requires a deque and an index"));
    };
    let index = crate::builtins::coerce_index_i64(index)?.max(0) as usize;
    let st = receiver(args, "__iter__")?;
    let d = st.data.borrow();
    Ok(Object::Int(
        index.min(d.len().saturating_sub(st.head())) as i64
    ))
}

fn deque_iterator_next(args: &[Object]) -> Result<Object, RuntimeError> {
    deque_next(args, false)
}

fn deque_reverse_iterator_next(args: &[Object]) -> Result<Object, RuntimeError> {
    deque_next(args, true)
}

fn deque_next(args: &[Object], reverse: bool) -> Result<Object, RuntimeError> {
    let [Object::Instance(iterator)] = args else {
        return Err(type_error("deque iterator __next__ requires one iterator"));
    };
    let deque = match iterator.slot_get("_deq") {
        Some(Object::Instance(deque)) => deque,
        Some(Object::None) => return Err(stop_iteration()),
        _ => return Err(type_error("deque iterator expected")),
    };
    let Some(Object::List(data)) = deque.slot_get("_data") else {
        return Err(type_error("deque expected"));
    };
    // Serialize the state check, cursor advance, and item read with deque
    // end operations, including simultaneous next() calls under gil=0.
    // The local deque reference outlives the guard, so clearing _deq can't
    // finalize its items while the backing list is borrowed.
    let d = data.borrow();
    if deque.slot_get("_state").and_then(|s| s.as_i64())
        != iterator.slot_get("_deq_state").and_then(|s| s.as_i64())
    {
        iterator.slot_set("_deq", Object::None);
        return Err(runtime_error("deque mutated during iteration"));
    }
    let h = head(&deque).min(d.len());
    let index = iterator
        .slot_get("_index")
        .and_then(|i| i.as_i64())
        .ok_or_else(|| type_error("invalid deque iterator index"))?;
    if index < 0 || index as usize >= d.len() - h {
        iterator.slot_set("_deq", Object::None);
        return Err(stop_iteration());
    }
    iterator.slot_set("_index", Object::Int(index + 1));
    let slot = if reverse {
        d.len() - 1 - index as usize
    } else {
        h + index as usize
    };
    Ok(d[slot].clone())
}

pub fn build(_cache: &ModuleCache) -> Rc<PyModule> {
    let dict = Rc::new(RefCell::new(DictData::default()));
    {
        let mut d = dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("__name__")),
            Object::from_static("_weave_collections"),
        );
        let mut reg = |export: &'static str,
                       name: &'static str,
                       f: fn(&[Object]) -> Result<Object, RuntimeError>| {
            d.insert(
                DictKey(Object::from_static(export)),
                Object::Builtin(Rc::new(BuiltinFn {
                    name,
                    binds_instance: true,
                    call: Box::new(f),
                    call_kw: None,
                })),
            );
        };
        reg("append", "append", deque_append);
        reg("appendleft", "appendleft", deque_appendleft);
        reg("pop", "pop", deque_pop);
        reg("popleft", "popleft", deque_popleft);
        reg("__len__", "__len__", deque_len);
        reg("__bool__", "__bool__", deque_bool);
        reg("__getitem__", "__getitem__", deque_getitem);
        reg("rotate", "rotate", deque_rotate);
        reg("iterator_index", "iterator_index", deque_iterator_index);
        reg("iterator_next", "__next__", deque_iterator_next);
        reg(
            "reverse_iterator_next",
            "__next__",
            deque_reverse_iterator_next,
        );
        // The end operations, length, truth, indexing, and rotation run no
        // Python code for any argument (they only ever touch the deque's
        // own slots and list): the leaf burst may call them directly.
        for name in [
            "append",
            "appendleft",
            "pop",
            "popleft",
            "__len__",
            "__bool__",
            "__getitem__",
            "rotate",
            "iterator_next",
            "reverse_iterator_next",
        ] {
            if let Some(Object::Builtin(b)) = d.get(&DictKey(Object::from_static(name))) {
                crate::leaf_builtins::register(b);
            }
        }
    }
    Rc::new(PyModule {
        name: "_weave_collections".to_owned(),
        filename: None,
        dict,
    })
}

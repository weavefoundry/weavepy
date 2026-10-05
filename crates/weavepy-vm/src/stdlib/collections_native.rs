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

/// The slot names, interned: a slot key stored through the attribute
/// machinery shares the interned storage, so the hinted lookups settle on
/// one pointer compare instead of comparing the bytes.
struct SlotNames {
    data: crate::shared_value::SharedStr,
    head: crate::shared_value::SharedStr,
    maxlen: crate::shared_value::SharedStr,
    state: crate::shared_value::SharedStr,
    deq: crate::shared_value::SharedStr,
    index: crate::shared_value::SharedStr,
    deq_state: crate::shared_value::SharedStr,
}

/// The first interpreter thread's interned names (a thread interning
/// its own copies only misses the pointer compare, never the lookup).
static NAMES: std::sync::OnceLock<SlotNames> = std::sync::OnceLock::new();

/// The interned slot names.
#[inline]
fn names() -> &'static SlotNames {
    NAMES.get_or_init(|| {
        let intern = |n: &str| match crate::stdlib::sys::intern_name(n) {
            Object::Str(s) => s,
            _ => unreachable!("names intern as strings"),
        };
        SlotNames {
            data: intern("_data"),
            head: intern("_head"),
            maxlen: intern("_maxlen"),
            state: intern("_state"),
            deq: intern("_deq"),
            index: intern("_index"),
            deq_state: intern("_deq_state"),
        }
    })
}

// The slot positions `deque.__init__` assigns in order (hints only; the
// name is always verified).
const SLOT_DATA: usize = 0;
const SLOT_HEAD: usize = 1;
const SLOT_MAXLEN: usize = 2;
const SLOT_STATE: usize = 3;

fn head_of(slots: &crate::types::SlotStorage) -> usize {
    match slots.get_hinted(SLOT_HEAD, &names().head) {
        Some(Object::Int(h)) if *h >= 0 => *h as usize,
        _ => 0,
    }
}

fn set_head_of(slots: &mut crate::types::SlotStorage, h: usize) {
    match slots.get_hinted_mut(SLOT_HEAD, &names().head) {
        Some(slot) => *slot = Object::Int(h as i64),
        None => slots
            .insert("_head", Object::Int(h as i64))
            .map_or((), drop),
    }
}

fn maxlen_of(slots: &crate::types::SlotStorage) -> Option<usize> {
    match slots.get_hinted(SLOT_MAXLEN, &names().maxlen) {
        Some(Object::Int(m)) if *m >= 0 => Some(*m as usize),
        _ => None,
    }
}

fn bump_state_of(slots: &mut crate::types::SlotStorage) {
    match slots.get_hinted_mut(SLOT_STATE, &names().state) {
        Some(Object::Int(s)) => *s = s.wrapping_add(1),
        Some(slot) => *slot = Object::Int(1),
        None => slots.insert("_state", Object::Int(1)).map_or((), drop),
    }
}

impl DequeState<'_> {
    fn head(&self) -> usize {
        head_of(&self.slots)
    }

    fn set_head(&mut self, h: usize) {
        set_head_of(&mut self.slots, h);
    }

    fn maxlen(&self) -> Option<usize> {
        maxlen_of(&self.slots)
    }

    fn bump_state(&mut self) {
        bump_state_of(&mut self.slots);
    }
}

/// A deque's slot storage and backing list, borrowed without guards
/// when nothing holds either (the end operations run no Python code
/// while they use them; the free-threaded build's shared cells always
/// take the guarded path). `None` sends the operation to that path.
// The `&mut` out of `&[Object]` is the point: both cells are checked
// unguarded (`peek_mut` returns `None` if any guard is live) and neither
// reference outlives the native call, which runs no Python.
#[allow(clippy::mut_from_ref)]
#[inline(always)]
fn fast_parts(args: &[Object]) -> Option<Fast<'_>> {
    let Object::Instance(inst) = args.first()? else {
        return None;
    };
    // SAFETY: see above — no guard is live on either cell (`peek_mut`
    // checks), and neither reference outlives the native call.
    let slots = unsafe { inst.slots.peek_mut() }?;
    let n = names();
    let [data, head, maxlen, state] =
        slots.leading_interned_mut([&n.data, &n.head, &n.maxlen, &n.state])?;
    let Object::List(data) = data else {
        return None;
    };
    // The list lives in its own allocation, held by the `_data` slot,
    // which the operations never rebind.
    let data: *const RefCell<Vec<Object>> = Rc::as_ptr(data);
    // SAFETY: as above.
    let d = unsafe { (*data).peek_mut() }?;
    Some(Fast {
        d,
        head,
        maxlen,
        state,
    })
}

/// A deque's backing list and its `_head`, `_maxlen` and `_state` slots,
/// borrowed unguarded (see [`fast_parts`]) and found in one pass over
/// the slot store.
struct Fast<'a> {
    d: &'a mut Vec<Object>,
    head: &'a mut Object,
    maxlen: &'a Object,
    state: &'a mut Object,
}

impl Fast<'_> {
    #[inline]
    fn head(&self) -> usize {
        match *self.head {
            Object::Int(h) if h >= 0 => h as usize,
            _ => 0,
        }
    }

    #[inline]
    fn set_head(&mut self, h: usize) {
        match &mut *self.head {
            Object::Int(slot) => *slot = h as i64,
            slot => *slot = Object::Int(h as i64),
        }
    }

    #[inline]
    fn maxlen(&self) -> Option<usize> {
        match *self.maxlen {
            Object::Int(m) if m >= 0 => Some(m as usize),
            _ => None,
        }
    }

    #[inline]
    fn bump_state(&mut self) {
        match &mut *self.state {
            Object::Int(s) => *s = s.wrapping_add(1),
            slot => *slot = Object::Int(1),
        }
    }

    /// [`popleft_locked`] over the unguarded parts; `None` (nothing
    /// touched) on an empty deque.
    #[inline]
    fn popleft(&mut self) -> Option<Object> {
        let mut h = self.head();
        let d = &mut *self.d;
        if h >= d.len() {
            return None;
        }
        let x = std::mem::replace(&mut d[h], Object::None);
        h += 1;
        if h >= d.len() {
            d.clear();
            h = 0;
        } else if h >= 32 && h * 2 >= d.len() {
            d.drain(..h);
            h = 0;
        }
        self.bump_state();
        self.set_head(h);
        Some(x)
    }
}

/// The deque operations a compiled caller may run directly (see
/// [`fast_op`]); `0` names none. The JIT's method registry carries one
/// per builtin (`leaf_builtins::register_jit_method`).
pub(crate) const OP_APPEND: u8 = 1;
pub(crate) const OP_APPENDLEFT: u8 = 2;
pub(crate) const OP_POP: u8 = 3;
pub(crate) const OP_POPLEFT: u8 = 4;
pub(crate) const OP_LEN: u8 = 5;
pub(crate) const OP_BOOL: u8 = 6;
pub(crate) const OP_GETITEM: u8 = 7;
/// A forward (`_deque_iterator`) or reverse iterator step; the receiver
/// is the iterator.
pub(crate) const OP_NEXT: u8 = 8;
pub(crate) const OP_RNEXT: u8 = 9;

/// One deque operation's common case on `recv` (and its one argument),
/// over the unguarded views [`fast_parts`] takes: the whole operation,
/// or `None` with nothing touched when it needs the full body (a guarded
/// cell, a foreign receiver, an empty deque's `IndexError`, an index out
/// of range or not an exact `int`). The builtins' fast paths and the
/// JIT's direct calls share it; it runs no Python code. Inlined, so a
/// builtin's constant `op` folds the dispatch away.
#[inline(always)]
pub(crate) fn fast_op(op: u8, recv: &Object, arg: Option<&Object>) -> Option<Object> {
    if op >= OP_NEXT {
        let Object::Instance(iterator) = recv else {
            return None;
        };
        return deque_next_fast(iterator, op == OP_RNEXT);
    }
    let mut f = fast_parts(std::slice::from_ref(recv))?;
    match op {
        OP_APPEND => {
            let x = arg?;
            f.bump_state();
            f.d.push(x.clone());
            let trimmed = match f.maxlen() {
                Some(m) if f.d.len() - f.head() > m => f.popleft(),
                _ => None,
            };
            drop(trimmed);
            Some(Object::None)
        }
        OP_APPENDLEFT => {
            let x = arg?;
            f.bump_state();
            let mut h = f.head().min(f.d.len());
            if h == 0 {
                h = std::cmp::max(8, f.d.len() / 2);
                f.d.splice(0..0, std::iter::repeat_n(Object::None, h));
            }
            h -= 1;
            f.d[h] = x.clone();
            f.set_head(h);
            let trimmed = match f.maxlen() {
                Some(m) if f.d.len() - h > m => f.d.pop(),
                _ => None,
            };
            drop(trimmed);
            Some(Object::None)
        }
        OP_POP => {
            let h = f.head();
            if f.d.len() <= h {
                return None;
            }
            f.bump_state();
            let x = f.d.pop().expect("len checked");
            // An emptied deque keeps a short free prefix for the next
            // `appendleft` instead of splicing a new one.
            if f.d.len() == h && h > 32 {
                f.d.clear();
                f.set_head(0);
            }
            Some(x)
        }
        OP_POPLEFT => f.popleft(),
        OP_LEN => Some(Object::Int(f.d.len().saturating_sub(f.head()) as i64)),
        OP_BOOL => Some(Object::Bool(f.d.len() > f.head())),
        OP_GETITEM => {
            let Some(&Object::Int(i)) = arg else {
                return None;
            };
            let h = f.head();
            let n = f.d.len().saturating_sub(h) as i64;
            let index = if i < 0 { i + n } else { i };
            (0..n)
                .contains(&index)
                .then(|| f.d[h + index as usize].clone())
        }
        _ => None,
    }
}

fn receiver<'a>(args: &'a [Object], method: &str) -> Result<DequeState<'a>, RuntimeError> {
    let recv = args
        .first()
        .ok_or_else(|| type_error(format!("unbound method deque.{method}() needs an argument")))?;
    if let Object::Instance(inst) = recv {
        let slots = inst.slots.borrow_mut();
        if let Some(Object::List(data)) = slots.get_hinted(SLOT_DATA, &names().data) {
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
    if let [recv, x] = args {
        if let Some(v) = fast_op(OP_APPEND, recv, Some(x)) {
            return Ok(v);
        }
    }
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
    drop(trimmed);
    Ok(Object::None)
}

fn deque_appendleft(args: &[Object]) -> Result<Object, RuntimeError> {
    if let [recv, x] = args {
        if let Some(v) = fast_op(OP_APPENDLEFT, recv, Some(x)) {
            return Ok(v);
        }
    }
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
    drop(trimmed);
    Ok(Object::None)
}

fn deque_pop(args: &[Object]) -> Result<Object, RuntimeError> {
    if let [recv] = args {
        if let Some(v) = fast_op(OP_POP, recv, None) {
            return Ok(v);
        }
    }
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
    if let [recv] = args {
        if let Some(v) = fast_op(OP_POPLEFT, recv, None) {
            return Ok(v);
        }
    }
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
    if let [recv] = args {
        if let Some(v) = fast_op(OP_LEN, recv, None) {
            return Ok(v);
        }
    }
    let st = receiver(args, "__len__")?;
    if args.len() != 1 {
        return Err(type_error("deque.__len__() takes no arguments"));
    }
    let d = st.data.borrow();
    Ok(Object::Int(d.len().saturating_sub(st.head()) as i64))
}

fn deque_bool(args: &[Object]) -> Result<Object, RuntimeError> {
    if let [recv] = args {
        if let Some(v) = fast_op(OP_BOOL, recv, None) {
            return Ok(v);
        }
    }
    let st = receiver(args, "__bool__")?;
    if args.len() != 1 {
        return Err(type_error("deque.__bool__() takes no arguments"));
    }
    let d = st.data.borrow();
    Ok(Object::Bool(d.len() > st.head()))
}

fn deque_getitem(args: &[Object]) -> Result<Object, RuntimeError> {
    if let [recv, index] = args {
        if let Some(v) = fast_op(OP_GETITEM, recv, Some(index)) {
            return Ok(v);
        }
    }
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

/// `iter(d)` / `reversed(d)`: a fresh `_deque_iterator` (or reverse
/// iterator) over `d` at index 0, snapshotting the mutation counter,
/// exactly what the iterator classes' `__init__` builds, but with no
/// Python code running. The classes come from the deque class's
/// `_iter_types` pair (set by `_collections.py`), so a subclass and each
/// copy of the module use their own.
fn deque_make_iter(args: &[Object], reverse: bool) -> Result<Object, RuntimeError> {
    let method = if reverse { "__reversed__" } else { "__iter__" };
    let state = {
        let st = receiver(args, method)?;
        st.slots
            .get_hinted(SLOT_STATE, &names().state)
            .cloned()
            .unwrap_or(Object::Int(0))
    };
    let [recv @ Object::Instance(inst)] = args else {
        return Err(type_error(format!(
            "deque.{method}() takes no arguments ({} given)",
            args.len().saturating_sub(1)
        )));
    };
    let it_cls = match inst.cls().lookup("_iter_types") {
        Some(Object::Tuple(types)) => match types.get(usize::from(reverse)) {
            Some(Object::Type(cls)) => cls.clone(),
            _ => return Err(type_error("deque iterator types are not set")),
        },
        _ => return Err(type_error("deque iterator types are not set")),
    };
    let it = crate::types::PyInstance::new_deferred(it_cls);
    // The iterator classes' slot order (see `deque_next_fast`).
    it.slot_set("_deq", recv.clone());
    it.slot_set("_index", Object::Int(0));
    it.slot_set("_deq_state", state);
    Ok(Object::Instance(it))
}

fn deque_iter(args: &[Object]) -> Result<Object, RuntimeError> {
    deque_make_iter(args, false)
}

fn deque_reversed(args: &[Object]) -> Result<Object, RuntimeError> {
    deque_make_iter(args, true)
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

// The iterator classes' slot positions as their `__init__` assigns them
// (hints, like the deque's own).
const IT_DEQ: usize = 0;
const IT_INDEX: usize = 1;
const IT_STATE: usize = 2;

/// [`deque_next`]'s common case over unguarded views (see
/// [`fast_parts`]): a live iterator over an unmutated deque with an item
/// left. `None` (nothing touched) leaves every other case to the full
/// body.
#[inline(always)]
fn deque_next_fast(iterator: &crate::types::PyInstance, reverse: bool) -> Option<Object> {
    // SAFETY: no guard is live on the cells (`peek`/`peek_mut` check), no
    // Python runs before the last use, and the iterator and its deque are
    // distinct objects.
    let its = unsafe { iterator.slots.peek_mut() }?;
    let n = names();
    let [deque, index, it_state] = its.leading_interned_mut([&n.deq, &n.index, &n.deq_state])?;
    let Object::Instance(deque) = deque else {
        return None;
    };
    // The deque lives while the iterator's slot holds it (unchanged here).
    let deque: *const crate::types::PyInstance = Rc::as_ptr(deque);
    let Object::Int(i) = index else {
        return None;
    };
    // SAFETY: as above.
    let ds = unsafe { (*deque).slots.peek() }?;
    let [data, head, _, state] = ds.leading_interned([&n.data, &n.head, &n.maxlen, &n.state])?;
    let Object::List(data) = data else {
        return None;
    };
    // The counters are plain ints (any other value takes the full body).
    match (state, &*it_state) {
        (Object::Int(a), Object::Int(b)) if a == b => {}
        _ => return None,
    }
    let h = match *head {
        Object::Int(h) if h >= 0 => h as usize,
        _ => 0,
    };
    // SAFETY: as above.
    let d = unsafe { data.peek() }?;
    let h = h.min(d.len());
    let index = *i;
    if index < 0 || index as usize >= d.len() - h {
        return None;
    }
    let slot = if reverse {
        d.len() - 1 - index as usize
    } else {
        h + index as usize
    };
    let v = d[slot].clone();
    *i = index + 1;
    Some(v)
}

fn deque_next(args: &[Object], reverse: bool) -> Result<Object, RuntimeError> {
    let [Object::Instance(iterator)] = args else {
        return Err(type_error("deque iterator __next__ requires one iterator"));
    };
    if let Some(v) = fast_op(if reverse { OP_RNEXT } else { OP_NEXT }, &args[0], None) {
        return Ok(v);
    }
    let (deque, index, it_state) = {
        let s = iterator.slots.borrow();
        let deque = match s.get_hinted(IT_DEQ, "_deq") {
            Some(Object::Instance(deque)) => deque.clone(),
            Some(Object::None) => return Err(stop_iteration()),
            _ => return Err(type_error("deque iterator expected")),
        };
        (
            deque,
            s.get_hinted(IT_INDEX, &names().index)
                .and_then(Object::as_i64),
            s.get_hinted(IT_STATE, &names().deq_state)
                .and_then(Object::as_i64),
        )
    };
    let (data, h, state) = {
        let s = deque.slots.borrow();
        let Some(Object::List(data)) = s.get_hinted(SLOT_DATA, "_data") else {
            return Err(type_error("deque expected"));
        };
        (
            data.clone(),
            head_of(&s),
            s.get_hinted(SLOT_STATE, "_state").and_then(Object::as_i64),
        )
    };
    // Serialize the state check, cursor advance, and item read with deque
    // end operations, including simultaneous next() calls under gil=0.
    // The local deque reference outlives the guard, so clearing _deq can't
    // finalize its items while the backing list is borrowed.
    let d = data.borrow();
    if state != it_state {
        iterator.slot_set("_deq", Object::None);
        return Err(runtime_error("deque mutated during iteration"));
    }
    let h = h.min(d.len());
    let index = index.ok_or_else(|| type_error("invalid deque iterator index"))?;
    if index < 0 || index as usize >= d.len() - h {
        iterator.slot_set("_deq", Object::None);
        return Err(stop_iteration());
    }
    let advanced = match iterator
        .slots
        .borrow_mut()
        .get_hinted_mut(IT_INDEX, "_index")
    {
        Some(slot) => {
            *slot = Object::Int(index + 1);
            true
        }
        None => false,
    };
    if !advanced {
        iterator.slot_set("_index", Object::Int(index + 1));
    }
    let slot = if reverse {
        d.len() - 1 - index as usize
    } else {
        h + index as usize
    };
    Ok(d[slot].clone())
}

// Only exact integer indices keep these calls within the leaf contract.
// __index__ on another object must run after publishing the caller's frame.
fn deque_getitem_leaf(args: &[Object]) -> Option<Result<Object, RuntimeError>> {
    match args {
        [_, Object::Int(_) | Object::Long(_) | Object::Bool(_)] => Some(deque_getitem(args)),
        _ => None,
    }
}

fn deque_rotate_leaf(args: &[Object]) -> Option<Result<Object, RuntimeError>> {
    match args {
        [_] | [_, Object::Int(_) | Object::Long(_) | Object::Bool(_)] => Some(deque_rotate(args)),
        _ => None,
    }
}

// ---------------------------------------------------------------------
// `_tuplegetter`: the named tuple field descriptor.

/// [`crate::types::TypeObject::collections_kind`] of the `_tuplegetter` class:
/// the attribute paths read a named tuple field through one of its
/// instances as a native tuple index (see [`tuplegetter_read`]).
pub(crate) const DESCR_TUPLEGETTER: u8 = 1;

/// The descriptor's field index lives in a slot whose name isn't an
/// identifier: no Python code can reach (or rebind) it, so the index is
/// fixed for the descriptor's life, as in CPython's C struct, and the
/// attribute caches may remember it.
const TG_INDEX: &str = "<index>";

/// The field index of `desc` when it is an instance of a `_tuplegetter`
/// class.
#[inline]
pub(crate) fn tuplegetter_index(desc: &crate::types::PyInstance) -> Option<usize> {
    if desc.cls_raw().collections_kind.get() != DESCR_TUPLEGETTER {
        return None;
    }
    let slots = desc.slots.try_borrow().ok()?;
    match slots.get_hinted(0, TG_INDEX)? {
        Object::Int(i) => usize::try_from(*i).ok(),
        _ => None,
    }
}

/// The field `desc` (a `_tuplegetter`) reads from the tuple-subclass
/// instance `recv`, when it is in range (anything else is the Python
/// `__get__`'s to raise).
#[inline]
pub(crate) fn tuplegetter_read(
    desc: &crate::types::PyInstance,
    recv: &crate::types::PyInstance,
) -> Option<Object> {
    let index = tuplegetter_index(desc)?;
    match recv.native.get()? {
        Object::Tuple(t) => t.get(index).cloned(),
        _ => None,
    }
}

/// `install_tuplegetter(cls)`: mark the `_tuplegetter` class (exactly; a
/// subclass is never marked) for the native attribute paths.
fn install_tuplegetter(args: &[Object]) -> Result<Object, RuntimeError> {
    let [Object::Type(cls)] = args else {
        return Err(type_error("install_tuplegetter() expects a class"));
    };
    cls.collections_kind.set(DESCR_TUPLEGETTER);
    Ok(Object::None)
}

/// `tuplegetter_init(desc, index)`: record the field index, once.
fn tuplegetter_init(args: &[Object]) -> Result<Object, RuntimeError> {
    let [Object::Instance(desc), index] = args else {
        return Err(type_error(
            "tuplegetter_init() expects a descriptor and an index",
        ));
    };
    let index = crate::builtins::coerce_index_i64(index)?;
    if desc.slot_get(TG_INDEX).is_some() {
        return Err(type_error("_tuplegetter index is already set"));
    }
    desc.slot_set(TG_INDEX, Object::Int(index));
    Ok(Object::None)
}

fn tg_index(desc: &Object) -> Result<i64, RuntimeError> {
    match desc {
        Object::Instance(d) => match d.slot_get(TG_INDEX) {
            Some(Object::Int(i)) => Ok(i),
            _ => Err(type_error("uninitialized _tuplegetter")),
        },
        _ => Err(type_error("_tuplegetter expected")),
    }
}

/// `tuplegetter_index(desc)`: the field index (for `__reduce__`).
fn tuplegetter_index_builtin(args: &[Object]) -> Result<Object, RuntimeError> {
    let [desc] = args else {
        return Err(type_error("tuplegetter_index() expects a descriptor"));
    };
    tg_index(desc).map(Object::Int)
}

/// `_tuplegetter.__get__(self, obj, type=None)`, as CPython's
/// `tuplegetter_descr_get`: the descriptor itself through the class, the
/// field through a tuple, and an error otherwise.
fn tuplegetter_get(args: &[Object]) -> Result<Object, RuntimeError> {
    let (desc, obj) = match args {
        [desc, obj] | [desc, obj, _] => (desc, obj),
        _ => return Err(type_error("__get__ expected 1 or 2 arguments")),
    };
    let index = tg_index(desc)?;
    let tuple = match obj {
        Object::Tuple(t) => Some(t),
        Object::Instance(i) => match i.native.get() {
            Some(Object::Tuple(t)) => Some(t),
            _ => None,
        },
        _ => None,
    };
    let Some(tuple) = tuple else {
        if matches!(obj, Object::None) {
            return Ok(desc.clone());
        }
        return Err(type_error(format!(
            "descriptor for index '{index}' for tuple subclasses doesn't apply to '{}' object",
            crate::builtins::class_of(obj).name
        )));
    };
    usize::try_from(index)
        .ok()
        .and_then(|i| tuple.get(i).cloned())
        .ok_or_else(|| index_error("tuple index out of range"))
}

// ---------------------------------------------------------------------
// `namedtuple` construction.

/// A registered generated `__new__`: the function (weakly), the code
/// object it was built with (held, so its address names it for the
/// registry's life), and the field count.
type NtNew = (
    crate::sync::Weak<crate::object::PyFunction>,
    Rc<weavepy_compiler::CodeObject>,
    usize,
);

/// The generated `__new__` functions of named tuple classes (see
/// [`namedtuple_new_shape`]).
static NT_NEWS: parking_lot::Mutex<Vec<NtNew>> = parking_lot::Mutex::new(Vec::new());

/// `namedtuple_register(new, nfields)`: `namedtuple()` vouches that `new`
/// is its generated `lambda _cls, f1, …, fn: _tuple_new(_cls, (f1, …, fn))`
/// (with `_tuple_new` bound to `tuple.__new__` in a namespace nothing else
/// holds).
fn namedtuple_register(args: &[Object]) -> Result<Object, RuntimeError> {
    let [Object::Function(f), Object::Int(n)] = args else {
        return Err(type_error(
            "namedtuple_register() expects a function and a count",
        ));
    };
    let n = usize::try_from(*n).map_err(|_| type_error("negative field count"))?;
    let code = f.code();
    let mut reg = NT_NEWS.lock();
    reg.retain(|(w, _, _)| w.strong_count() > 0);
    reg.push((Rc::downgrade(f), code, n));
    Ok(Object::None)
}

/// When `f` is a registered named tuple `__new__`: the address of the code
/// object it was registered with and its field count. A class whose
/// `__new__` is `f` builds `cls(*fields)` as the tuple directly while `f`
/// still runs that code (see `Interpreter::instantiate_named_tuple`).
pub(crate) fn namedtuple_new_shape(f: &Rc<crate::object::PyFunction>) -> Option<(usize, usize)> {
    let reg = NT_NEWS.lock();
    reg.iter()
        .find(|(w, _, _)| std::ptr::eq(w.as_ptr(), Rc::as_ptr(f)) && w.strong_count() > 0)
        .map(|(_, code, n)| (Rc::as_ptr(code) as usize, *n))
}

// ---------------------------------------------------------------------
// `defaultdict`.

/// The `default_factory` member's docstring (CPython's `defdict_members`).
const DD_FACTORY_DOC: &str = "Factory for default value called by __missing__().";

/// `install_defaultdict(cls)`: turn the class's `default_factory` slot
/// into CPython's member descriptor: unset (never assigned, or deleted)
/// reads as `None`. The `__slots__` declaration that made the slot is an
/// implementation detail the C type doesn't have, so it leaves the class
/// dict (the layout it set up, no instance `__dict__` and no weak
/// references, stays, as in CPython).
fn install_defaultdict(args: &[Object]) -> Result<Object, RuntimeError> {
    let [Object::Type(cls)] = args else {
        return Err(type_error("install_defaultdict() expects a class"));
    };
    let desc = Object::SlotDescriptor(Rc::new(crate::object::SlotDescriptor {
        name: "default_factory".to_owned(),
        class_name: cls.name.clone(),
        default: Some(Object::None),
        readonly: false,
        doc: Some(DD_FACTORY_DOC),
        objclass: RefCell::new(Some(Rc::downgrade(cls))),
    }));
    {
        let mut d = cls.dict.borrow_mut();
        d.insert(DictKey(Object::from_static("default_factory")), desc);
        d.shift_remove(&DictKey(Object::from_static("__slots__")));
    }
    cls.bump_attr_version();
    Ok(Object::None)
}

/// `defaultdict.__init__(self, default_factory=None, /, *args, **kwds)`,
/// as CPython's `defdict_init`: the factory (callable or `None`) is set
/// first, then the rest initializes the dict.
fn dd_init(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let Some(Object::Instance(inst)) = args.first() else {
        return Err(type_error(
            "descriptor '__init__' of 'collections.defaultdict' object needs an argument",
        ));
    };
    let factory = args.get(1).cloned().unwrap_or(Object::None);
    if !matches!(factory, Object::None) && !crate::builtins::object_is_callable(&factory) {
        return Err(type_error("first argument must be callable or None"));
    }
    inst.slot_set("default_factory", factory);
    if args.len() > 2 || !kwargs.is_empty() {
        let init = crate::builtin_types::builtin_types()
            .dict_
            .dict
            .borrow()
            .get(&crate::object::StrKey("__init__"))
            .cloned()
            .ok_or_else(|| type_error("dict.__init__ is missing"))?;
        let mut rest = Vec::with_capacity(args.len() - 1);
        rest.push(args[0].clone());
        rest.extend_from_slice(&args[2..]);
        crate::builtins::reentrant_interp()?.call_object(init, &rest, kwargs)?;
    }
    Ok(Object::None)
}

/// A new value from one of the common factories, built natively: the
/// value `int()`, `list()`, `dict()`, `set()`, `float()`, `str()` or
/// `tuple()` returns.
fn builtin_factory_value(factory: &Object) -> Option<Object> {
    let Object::Type(t) = factory else {
        return None;
    };
    let bt = crate::builtin_types::builtin_types();
    Some(if Rc::ptr_eq(t, &bt.int_) {
        Object::Int(0)
    } else if Rc::ptr_eq(t, &bt.list_) {
        Object::new_list(Vec::new())
    } else if Rc::ptr_eq(t, &bt.dict_) {
        Object::new_dict()
    } else if Rc::ptr_eq(t, &bt.set_) {
        Object::new_set()
    } else if Rc::ptr_eq(t, &bt.float_) {
        Object::Float(0.0)
    } else if Rc::ptr_eq(t, &bt.str_) {
        Object::from_static("")
    } else if Rc::ptr_eq(t, &bt.tuple_) {
        Object::new_tuple(Vec::new())
    } else {
        return None;
    })
}

/// `defaultdict.__missing__(self, key)`, as CPython's `defdict_missing`:
/// `KeyError(key)` without a factory; otherwise the factory's value,
/// stored with `dict.setdefault` (the first value stored wins when the
/// factory itself fills the key — gh-91618).
fn dd_missing(args: &[Object]) -> Result<Object, RuntimeError> {
    let [recv, key] = args else {
        return Err(type_error(format!(
            "__missing__() takes exactly one argument ({} given)",
            args.len().saturating_sub(1)
        )));
    };
    let Object::Instance(inst) = recv else {
        return Err(type_error(format!(
            "descriptor '__missing__' for 'collections.defaultdict' objects doesn't apply to a '{}' object",
            crate::builtins::class_of(recv).name
        )));
    };
    let factory = inst.slot_get("default_factory").unwrap_or(Object::None);
    if matches!(factory, Object::None) {
        return Err(crate::error::key_error_object(key.clone()));
    }
    let value = match builtin_factory_value(&factory) {
        Some(v) => v,
        None => crate::builtins::reentrant_interp()?.call_object(factory, &[], &[])?,
    };
    if matches!(value, Object::List(_) | Object::Dict(_) | Object::Set(_)) {
        crate::gc_trace::track(&value);
    }
    crate::builtins::dict_setdefault(&[recv.clone(), key.clone(), value])
}

// ---------------------------------------------------------------------
// `_count_elements`.

/// The dict a `Counter`-style tally may update directly: an exact dict,
/// or a dict subclass whose class keeps `dict.get` and `dict.__setitem__`
/// (CPython's `_count_elements` fast-path test).
fn tally_dict(mapping: &Object) -> Option<Rc<RefCell<DictData>>> {
    match mapping {
        Object::Dict(d) => Some(d.clone()),
        Object::Instance(inst) => {
            let Some(Object::Dict(d)) = inst.native.get() else {
                return None;
            };
            let cls = inst.cls_raw();
            let bt = crate::builtin_types::builtin_types();
            let same = |name: &str| {
                let base = bt
                    .dict_
                    .dict
                    .borrow()
                    .get(&crate::object::StrKey(name))
                    .cloned();
                match (cls.lookup(name), base) {
                    (Some(Object::Builtin(a)), Some(Object::Builtin(b))) => Rc::ptr_eq(&a, &b),
                    _ => false,
                }
            };
            (same("get") && same("__setitem__")).then(|| d.clone())
        }
        _ => None,
    }
}

/// Count one element into `d` (`d[key] = d.get(key, 0) + 1`).
fn tally_one(
    interp: &mut crate::Interpreter,
    d: &Rc<RefCell<DictData>>,
    key: &Object,
) -> Result<(), RuntimeError> {
    // A plain key and an `int` count: in place.
    if let Some(probe) = crate::object::LeafProbe::new(key) {
        if !crate::capi_watchers::dicts_active() {
            if let Ok(mut m) = d.try_borrow_mut() {
                match m.get_mut(&probe) {
                    Some(Object::Int(n)) if *n < i64::MAX => {
                        *n += 1;
                        drop(m);
                        crate::object::dict_mutation_event(d);
                        return Ok(());
                    }
                    Some(_) => {}
                    None if probe.miss_is_exact() => {
                        m.insert(DictKey(key.clone()), Object::Int(1));
                        drop(m);
                        crate::object::dict_watch_bump(d);
                        crate::object::dict_mutation_event(d);
                        return Ok(());
                    }
                    None => {}
                }
            }
        }
    }
    crate::builtins::ensure_hashable(key)?;
    let new = match crate::builtins::dict_lookup(d, key)? {
        None => Object::Int(1),
        Some(old) => {
            interp.binary_op_public(&old, &Object::Int(1), weavepy_compiler::BinOpKind::Add)?
        }
    };
    crate::builtins::dict_insert(d, key.clone(), new)?;
    Ok(())
}

/// `count_elements(mapping, iterable)`: the tally `_count_elements` runs
/// when `mapping` is a dict it may update directly (see [`tally_dict`]):
/// `True` when done, `False` (nothing consumed) for the Python loop over
/// `mapping.get` and `mapping[key] = …`.
fn count_elements(args: &[Object]) -> Result<Object, RuntimeError> {
    let [mapping, iterable] = args else {
        return Err(type_error(
            "count_elements() expects a mapping and an iterable",
        ));
    };
    let Some(d) = tally_dict(mapping) else {
        return Ok(Object::Bool(false));
    };
    let interp = crate::builtins::reentrant_interp()?;
    match iterable {
        Object::Str(s) => {
            for c in s.chars() {
                tally_one(interp, &d, &Object::from_char(c))?;
            }
        }
        Object::List(items) => {
            // Snapshot-free: the list may change under a key's `__eq__`.
            let mut i = 0;
            loop {
                let item = items.borrow().get(i).cloned();
                let Some(item) = item else { break };
                tally_one(interp, &d, &item)?;
                i += 1;
            }
        }
        Object::Tuple(items) => {
            for item in items.iter() {
                tally_one(interp, &d, item)?;
            }
        }
        _ => {
            let globals = interp.builtins_dict();
            let it = interp.make_iter(iterable, &globals)?;
            while let Some(item) = interp.iter_next(&it, &globals)? {
                tally_one(interp, &d, &item)?;
            }
        }
    }
    Ok(Object::Bool(true))
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
        reg("__iter__", "__iter__", deque_iter);
        reg("__reversed__", "__reversed__", deque_reversed);
        reg("iterator_index", "iterator_index", deque_iterator_index);
        reg(
            "install_tuplegetter",
            "install_tuplegetter",
            install_tuplegetter,
        );
        reg("tuplegetter_init", "tuplegetter_init", tuplegetter_init);
        reg(
            "tuplegetter_index",
            "tuplegetter_index",
            tuplegetter_index_builtin,
        );
        reg("tuplegetter_get", "__get__", tuplegetter_get);
        reg(
            "install_defaultdict",
            "install_defaultdict",
            install_defaultdict,
        );
        reg("dd_missing", "__missing__", dd_missing);
        reg("count_elements", "count_elements", count_elements);
        reg(
            "namedtuple_register",
            "namedtuple_register",
            namedtuple_register,
        );
        reg("iterator_next", "__next__", deque_iterator_next);
        reg(
            "reverse_iterator_next",
            "__next__",
            deque_reverse_iterator_next,
        );
        d.insert(
            DictKey(Object::from_static("dd_init")),
            Object::Builtin(Rc::new(BuiltinFn {
                name: "__init__",
                binds_instance: true,
                call: Box::new(|a: &[Object]| dd_init(a, &[])),
                call_kw: Some(Box::new(dd_init)),
            })),
        );
        // The `OrderedDict` methods (see `collections_odict`).
        for (export, name, body, kw) in super::collections_odict::exports() {
            d.insert(
                DictKey(Object::from_static(export)),
                Object::Builtin(Rc::new(BuiltinFn {
                    name,
                    binds_instance: true,
                    call: Box::new(body),
                    call_kw: kw.map(|kw| {
                        Box::new(kw)
                            as Box<
                                dyn Fn(
                                        &[Object],
                                        &[(String, Object)],
                                    )
                                        -> Result<Object, RuntimeError>
                                    + Send
                                    + Sync,
                            >
                    }),
                })),
            );
        }
        for name in super::collections_odict::LEAVES {
            if let Some(Object::Builtin(b)) = d.get(&DictKey(Object::from_static(name))) {
                crate::leaf_builtins::register(b);
            }
        }
        for (name, fast) in super::collections_odict::leaf_halves() {
            if let Some(Object::Builtin(b)) = d.get(&DictKey(Object::from_static(name))) {
                crate::leaf_builtins::register_fast(b, fast);
            }
        }
        // The end operations, length, truth, iterator construction, and
        // iterator steps run no Python code. Indexing and rotation require
        // an argument guard because index coercion can invoke Python's
        // __index__ protocol.
        for name in [
            "append",
            "appendleft",
            "pop",
            "popleft",
            "__len__",
            "__bool__",
            "__iter__",
            "__reversed__",
            "iterator_next",
            "reverse_iterator_next",
            "tuplegetter_get",
        ] {
            if let Some(Object::Builtin(b)) = d.get(&DictKey(Object::from_static(name))) {
                crate::leaf_builtins::register(b);
            }
        }
        for (name, fast) in [
            (
                "__getitem__",
                deque_getitem_leaf as crate::leaf_builtins::Fast,
            ),
            ("rotate", deque_rotate_leaf as crate::leaf_builtins::Fast),
        ] {
            if let Some(Object::Builtin(b)) = d.get(&DictKey(Object::from_static(name))) {
                crate::leaf_builtins::register_fast(b, fast);
            }
        }
        // The deque's own methods ride the JIT's native method lane (the
        // receiver checks above reject a foreign receiver exactly).
        for (name, op) in [
            ("append", OP_APPEND),
            ("appendleft", OP_APPENDLEFT),
            ("pop", OP_POP),
            ("popleft", OP_POPLEFT),
            ("__len__", OP_LEN),
            ("__bool__", OP_BOOL),
            ("__getitem__", OP_GETITEM),
            ("rotate", 0),
            ("iterator_next", OP_NEXT),
            ("reverse_iterator_next", OP_RNEXT),
        ] {
            if let Some(Object::Builtin(b)) = d.get(&DictKey(Object::from_static(name))) {
                crate::leaf_builtins::register_jit_method(b, op);
            }
        }
    }
    Rc::new(PyModule {
        name: "_weave_collections".to_owned(),
        filename: None,
        dict,
    })
}

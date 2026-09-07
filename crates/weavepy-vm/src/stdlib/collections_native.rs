//! `_weave_collections` — native end operations for the Python
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
//! These four builtins run the whole operation while holding the
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

use crate::error::{index_error, type_error, RuntimeError};
use crate::import::ModuleCache;
use crate::object::{BuiltinFn, DictData, DictKey, Object, PyModule};
use crate::types::PyInstance;

/// The receiver must be a deque instance (any class whose `_data` slot
/// is the backing list). CPython's method descriptor rejects a foreign
/// receiver with this exact wording (gh-92063).
fn receiver<'a>(
    args: &'a [Object],
    method: &str,
) -> Result<(&'a Rc<PyInstance>, Rc<RefCell<Vec<Object>>>), RuntimeError> {
    let recv = args
        .first()
        .ok_or_else(|| type_error(format!("unbound method deque.{method}() needs an argument")))?;
    if let Object::Instance(inst) = recv {
        if let Some(Object::List(data)) = inst.slot_get("_data") {
            return Ok((inst, data));
        }
    }
    Err(type_error(format!(
        "descriptor '{method}' for 'collections.deque' objects doesn't apply to a '{}' object",
        crate::builtins::class_of(recv).name
    )))
}

fn head(inst: &PyInstance) -> usize {
    match inst.slot_get("_head") {
        Some(Object::Int(h)) if h >= 0 => h as usize,
        _ => 0,
    }
}

fn set_head(inst: &PyInstance, h: usize) {
    inst.slot_set("_head", Object::Int(h as i64));
}

fn maxlen(inst: &PyInstance) -> Option<usize> {
    match inst.slot_get("_maxlen") {
        Some(Object::Int(m)) if m >= 0 => Some(m as usize),
        _ => None,
    }
}

fn bump_state(inst: &PyInstance) {
    let next = match inst.slot_get("_state") {
        Some(Object::Int(s)) => s.wrapping_add(1),
        _ => 1,
    };
    inst.slot_set("_state", Object::Int(next));
}

/// Shared `popleft` body: the caller holds the list borrow.
fn popleft_locked(inst: &PyInstance, d: &mut Vec<Object>) -> Result<Object, RuntimeError> {
    let mut h = head(inst);
    if h >= d.len() {
        return Err(index_error("pop from an empty deque"));
    }
    bump_state(inst);
    let x = std::mem::replace(&mut d[h], Object::None);
    h += 1;
    if h >= d.len() {
        d.clear();
        h = 0;
    } else if h >= 32 && h * 2 >= d.len() {
        d.drain(..h);
        h = 0;
    }
    set_head(inst, h);
    Ok(x)
}

/// `deque.append(x)`: push on the right, then trim the left when the
/// `maxlen` bound is exceeded.
fn deque_append(args: &[Object]) -> Result<Object, RuntimeError> {
    let (inst, data) = receiver(args, "append")?;
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
    // Dropped after the guard so a trimmed item's `__del__` never runs
    // under the list borrow.
    let trimmed;
    {
        let mut d = data.borrow_mut();
        bump_state(inst);
        d.push(x);
        trimmed = match maxlen(inst) {
            Some(m) if d.len() - head(inst) > m => Some(popleft_locked(inst, &mut d)?),
            _ => None,
        };
    }
    drop(trimmed);
    Ok(Object::None)
}

/// `deque.appendleft(x)`: fill the consumed prefix from the right,
/// opening a block of slack proportional to the size when there is
/// none (amortised O(1) like CPython's block deque), then trim the
/// right under `maxlen`.
fn deque_appendleft(args: &[Object]) -> Result<Object, RuntimeError> {
    let (inst, data) = receiver(args, "appendleft")?;
    let x = match args {
        [_, x] => x.clone(),
        _ => {
            return Err(type_error(format!(
                "deque.appendleft() takes exactly one argument ({} given)",
                args.len().saturating_sub(1)
            )))
        }
    };
    let trimmed;
    {
        let mut d = data.borrow_mut();
        bump_state(inst);
        let mut h = head(inst);
        if h == 0 {
            h = std::cmp::max(8, d.len() / 2);
            d.splice(0..0, std::iter::repeat_n(Object::None, h));
        }
        h -= 1;
        d[h] = x;
        set_head(inst, h);
        trimmed = match maxlen(inst) {
            Some(m) if d.len() - h > m => d.pop(),
            _ => None,
        };
    }
    drop(trimmed);
    Ok(Object::None)
}

/// `deque.pop()`: remove and return the rightmost item.
fn deque_pop(args: &[Object]) -> Result<Object, RuntimeError> {
    let (inst, data) = receiver(args, "pop")?;
    if args.len() != 1 {
        return Err(type_error(format!(
            "deque.pop() takes no arguments ({} given)",
            args.len() - 1
        )));
    }
    let mut d = data.borrow_mut();
    let h = head(inst);
    if d.len() <= h {
        return Err(index_error("pop from an empty deque"));
    }
    bump_state(inst);
    let x = d.pop().expect("len checked");
    if d.len() == h && h != 0 {
        d.clear();
        set_head(inst, 0);
    }
    Ok(x)
}

/// `deque.popleft()`: remove and return the leftmost item.
fn deque_popleft(args: &[Object]) -> Result<Object, RuntimeError> {
    let (inst, data) = receiver(args, "popleft")?;
    if args.len() != 1 {
        return Err(type_error(format!(
            "deque.popleft() takes no arguments ({} given)",
            args.len() - 1
        )));
    }
    let mut d = data.borrow_mut();
    popleft_locked(inst, &mut d)
}

pub fn build(_cache: &ModuleCache) -> Rc<PyModule> {
    let dict = Rc::new(RefCell::new(DictData::default()));
    {
        let mut d = dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("__name__")),
            Object::from_static("_weave_collections"),
        );
        let mut reg = |name: &'static str, f: fn(&[Object]) -> Result<Object, RuntimeError>| {
            d.insert(
                DictKey(Object::from_static(name)),
                Object::Builtin(Rc::new(BuiltinFn {
                    name,
                    binds_instance: true,
                    call: Box::new(f),
                    call_kw: None,
                })),
            );
        };
        reg("append", deque_append);
        reg("appendleft", deque_appendleft);
        reg("pop", deque_pop);
        reg("popleft", deque_popleft);
    }
    Rc::new(PyModule {
        name: "_weave_collections".to_owned(),
        filename: None,
        dict,
    })
}

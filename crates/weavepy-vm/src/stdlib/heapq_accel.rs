//! The `_heapq` accelerator module — a faithful port of CPython's
//! `Modules/_heapqmodule.c`.
//!
//! WeavePy ships a verbatim pure-Python `heapq` (`stdlib/python/heapq.py`)
//! that ends with `from _heapq import *` (and the three `_*_max` helpers),
//! exactly like CPython's `Lib/heapq.py`. `test_heapq` builds a `C`/`Py`
//! pair via `import_fresh_module('heapq', fresh=['_heapq'])` /
//! `blocked=['_heapq']` and runs the whole stress/randomised suite against
//! both, asserting `fn.__module__` is `'_heapq'` for the C variant. So the
//! accelerator must exist *and* match CPython's algorithm bit-for-bit
//! (including the "list changed size during iteration" guard and the
//! tie-goes-right child selection).
//!
//! All comparisons route through the interpreter's rich-comparison machinery
//! (`op_compare(.., Py_LT)` == `PyObject_RichCompareBool(a, b, Py_LT)`), so a
//! heap of objects with a custom `__lt__` behaves identically to CPython.

use crate::sync::Rc;
use crate::sync::RefCell;

use crate::error::{index_error, runtime_error, type_error, RuntimeError};
use crate::import::ModuleCache;
use crate::object::{BuiltinFn, DictData, DictKey, Object, PyModule};
use weavepy_compiler::CompareKind;

pub fn build(_cache: &ModuleCache) -> Rc<PyModule> {
    let dict = Rc::new(RefCell::new(DictData::new()));
    {
        let mut d = dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("__name__")),
            Object::from_static("_heapq"),
        );
        d.insert(
            DictKey(Object::from_static("__doc__")),
            Object::from_static("Heap queue algorithm (a.k.a. priority queue)."),
        );
        let mut all: Vec<Object> = Vec::new();
        macro_rules! reg {
            ($name:literal, $f:expr) => {{
                let f = Object::Builtin(Rc::new(BuiltinFn {
                    name: $name,
                    binds_instance: false,
                    call: Box::new($f),
                    call_kw: None,
                }));
                crate::descr_registry::register_module(&f, "_heapq");
                d.insert(DictKey(Object::from_static($name)), f);
                // `from _heapq import *` only exports the public names.
                if !$name.starts_with('_') {
                    all.push(Object::from_static($name));
                }
            }};
        }
        reg!("heappush", heappush);
        reg!("heappop", heappop);
        reg!("heapify", heapify);
        reg!("heapreplace", heapreplace);
        reg!("heappushpop", heappushpop);
        reg!("_heapify_max", heapify_max);
        reg!("_heapreplace_max", heapreplace_max);
        reg!("_heappop_max", heappop_max);
        d.insert(
            DictKey(Object::from_static("__all__")),
            Object::new_list(all),
        );
    }
    Rc::new(PyModule {
        name: "_heapq".to_owned(),
        filename: None,
        dict,
    })
}

/// Borrow the active interpreter published on this thread by the dispatch
/// loop. Always present while a builtin runs.
fn with_interp<F, R>(f: F) -> Result<R, RuntimeError>
where
    F: FnOnce(&mut crate::Interpreter) -> Result<R, RuntimeError>,
{
    let ptr = crate::vm_singletons::current_interpreter_ptr()
        .ok_or_else(|| type_error("_heapq: no active interpreter"))?;
    // SAFETY: published by the enclosing VM frame on this thread.
    let interp = unsafe { &mut *ptr };
    f(interp)
}

/// The first argument must be a `list` (CPython requires `PyList_Check`).
fn heap_arg(args: &[Object]) -> Result<Rc<RefCell<Vec<Object>>>, RuntimeError> {
    match args.first() {
        Some(Object::List(l)) => Ok(l.clone()),
        _ => Err(type_error("heap argument must be a list")),
    }
}

/// `siftdown` (CPython's confusingly named "move toward the root"): the item
/// at `pos` bubbles up past larger parents until it fits. `max_heap` flips the
/// comparison direction so the `_*_max` helpers build a max-heap.
fn siftdown(
    interp: &mut crate::Interpreter,
    heap: &Rc<RefCell<Vec<Object>>>,
    startpos: usize,
    mut pos: usize,
    max_heap: bool,
) -> Result<(), RuntimeError> {
    let size = heap.borrow().len();
    if pos >= size {
        return Err(index_error("index out of range"));
    }
    while pos > startpos {
        let parentpos = (pos - 1) >> 1;
        let (cur, parent) = {
            let h = heap.borrow();
            (h[pos].clone(), h[parentpos].clone())
        };
        // min-heap: `cur < parent`; max-heap: `parent < cur`.
        let cmp = if max_heap {
            interp.op_compare(&parent, &cur, CompareKind::Lt)?
        } else {
            interp.op_compare(&cur, &parent, CompareKind::Lt)?
        };
        if heap.borrow().len() != size {
            return Err(runtime_error("list changed size during iteration"));
        }
        if !cmp {
            break;
        }
        heap.borrow_mut().swap(pos, parentpos);
        pos = parentpos;
    }
    Ok(())
}

/// `siftup` (CPython's "move toward the leaves"): bubble the smaller (or, for a
/// max-heap, larger) child up until reaching a leaf, then `siftdown` to settle.
fn siftup(
    interp: &mut crate::Interpreter,
    heap: &Rc<RefCell<Vec<Object>>>,
    mut pos: usize,
    max_heap: bool,
) -> Result<(), RuntimeError> {
    let endpos = heap.borrow().len();
    let startpos = pos;
    if pos >= endpos {
        return Err(index_error("index out of range"));
    }
    let limit = endpos >> 1;
    while pos < limit {
        let mut childpos = 2 * pos + 1;
        if childpos + 1 < endpos {
            let (left, right) = {
                let h = heap.borrow();
                (h[childpos].clone(), h[childpos + 1].clone())
            };
            // Pick the smaller child (min) / larger child (max); ties go right,
            // matching CPython's `childpos += (cmp ^ 1)`.
            let cmp = if max_heap {
                interp.op_compare(&right, &left, CompareKind::Lt)?
            } else {
                interp.op_compare(&left, &right, CompareKind::Lt)?
            };
            childpos += usize::from(!cmp);
            if heap.borrow().len() != endpos {
                return Err(runtime_error("list changed size during iteration"));
            }
        }
        heap.borrow_mut().swap(pos, childpos);
        pos = childpos;
    }
    siftdown(interp, heap, startpos, pos, max_heap)
}

fn heappush_impl(args: &[Object], max_heap: bool) -> Result<Object, RuntimeError> {
    let heap = heap_arg(args)?;
    let item = args
        .get(1)
        .cloned()
        .ok_or_else(|| type_error("heappush expected 2 arguments"))?;
    with_interp(|interp| {
        let pos = {
            let mut h = heap.borrow_mut();
            h.push(item);
            h.len() - 1
        };
        siftdown(interp, &heap, 0, pos, max_heap)?;
        Ok(Object::None)
    })
}

fn heappush(args: &[Object]) -> Result<Object, RuntimeError> {
    heappush_impl(args, false)
}

fn heappop_impl(args: &[Object], max_heap: bool) -> Result<Object, RuntimeError> {
    let heap = heap_arg(args)?;
    with_interp(|interp| {
        let lastelt = {
            let mut h = heap.borrow_mut();
            match h.pop() {
                Some(x) => x,
                None => return Err(index_error("index out of range")),
            }
        };
        if heap.borrow().is_empty() {
            return Ok(lastelt);
        }
        let returnitem = {
            let mut h = heap.borrow_mut();
            std::mem::replace(&mut h[0], lastelt)
        };
        siftup(interp, &heap, 0, max_heap)?;
        Ok(returnitem)
    })
}

fn heappop(args: &[Object]) -> Result<Object, RuntimeError> {
    heappop_impl(args, false)
}

fn heapreplace_impl(args: &[Object], max_heap: bool) -> Result<Object, RuntimeError> {
    let heap = heap_arg(args)?;
    let item = args
        .get(1)
        .cloned()
        .ok_or_else(|| type_error("heapreplace expected 2 arguments"))?;
    with_interp(|interp| {
        let returnitem = {
            let mut h = heap.borrow_mut();
            if h.is_empty() {
                return Err(index_error("index out of range"));
            }
            std::mem::replace(&mut h[0], item)
        };
        siftup(interp, &heap, 0, max_heap)?;
        Ok(returnitem)
    })
}

fn heapreplace(args: &[Object]) -> Result<Object, RuntimeError> {
    heapreplace_impl(args, false)
}

fn heappushpop(args: &[Object]) -> Result<Object, RuntimeError> {
    let heap = heap_arg(args)?;
    let item = args
        .get(1)
        .cloned()
        .ok_or_else(|| type_error("heappushpop expected 2 arguments"))?;
    with_interp(|interp| {
        let top = {
            let h = heap.borrow();
            h.first().cloned()
        };
        let Some(top) = top else {
            return Ok(item);
        };
        // Only replace when `heap[0] < item` (min-heap), else `item` is the
        // smallest and is returned untouched.
        if !interp.op_compare(&top, &item, CompareKind::Lt)? {
            return Ok(item);
        }
        // bpo-39421: the comparison above may have run arbitrary Python
        // (`__lt__`) that mutated — even cleared — the heap. CPython
        // re-checks the size here and raises rather than indexing a
        // now-empty list.
        let returnitem = {
            let mut h = heap.borrow_mut();
            if h.is_empty() {
                return Err(index_error("list index out of range"));
            }
            std::mem::replace(&mut h[0], item)
        };
        siftup(interp, &heap, 0, false)?;
        Ok(returnitem)
    })
}

fn heapify_impl(args: &[Object], max_heap: bool) -> Result<Object, RuntimeError> {
    let heap = heap_arg(args)?;
    with_interp(|interp| {
        let n = heap.borrow().len();
        // Transform bottom-up: the largest index with a child is n//2 - 1.
        for i in (0..n / 2).rev() {
            siftup(interp, &heap, i, max_heap)?;
        }
        Ok(Object::None)
    })
}

fn heapify(args: &[Object]) -> Result<Object, RuntimeError> {
    heapify_impl(args, false)
}

fn heapify_max(args: &[Object]) -> Result<Object, RuntimeError> {
    heapify_impl(args, true)
}

fn heapreplace_max(args: &[Object]) -> Result<Object, RuntimeError> {
    heapreplace_impl(args, true)
}

fn heappop_max(args: &[Object]) -> Result<Object, RuntimeError> {
    heappop_impl(args, true)
}

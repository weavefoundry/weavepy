//! `_weave_queue` — native backing for the `_queue` accelerator shim.
//!
//! CPython's `_queue.SimpleQueue.put` is a C method descriptor
//! (`method_descriptor` on the type, `builtin_function_or_method` once
//! bound — test_types.test_method_descriptor_crash exercises exactly
//! that through `put.__get__(instance)`). The Python shim in
//! `python/_queue.py` adopts this builtin as its `put`, keeping the
//! blocking `get` logic in Python.
//!
//! The queue follows `_queuemodule.c`'s wake-gate design (see the shim's
//! module docstring): `put` appends and, when a getter holds the gate,
//! releases it. It never blocks and never acquires a lock, so a signal
//! handler may call it while the same thread is anywhere inside `get`.
//! `get`'s last step is native too: it pops and passes the gate on to the
//! next parked getter in one step. `get` itself is a builtin that takes an
//! item that is already waiting (the common case, and all a thread pool's
//! busy workers do) and leaves an empty queue, or a timeout, to the shim's
//! `_get_slow`.

use crate::sync::Rc;
use crate::sync::RefCell;

use crate::error::RuntimeError;
use crate::import::ModuleCache;
use crate::object::{BuiltinFn, DictData, DictKey, Object, PyModule};
use crate::stdlib::collections_native as deque;

/// The queue's own attribute `name`: the instance's entry read in place,
/// or, for a subclass that answers some other way, the full lookup's.
fn field(
    interp: &mut crate::Interpreter,
    recv: &Object,
    name: &str,
) -> Result<Object, RuntimeError> {
    if let Object::Instance(inst) = recv {
        if let Some(v) = inst.attr_get_str(name) {
            return Ok(v);
        }
    }
    interp.load_attr_public(recv, name)
}

/// `SimpleQueue.put(self, item, block=True, timeout=None)` — append to
/// the deque, then open the wake gate if a `get` is parked on it
/// (`simplequeue_put_locked`: `if (self->locked) { release; locked = 0 }`).
/// Never blocks; `block`/`timeout` are accepted and ignored (CPython
/// signature compatibility).
fn simplequeue_put(args: &[Object]) -> Result<Object, RuntimeError> {
    let recv = args.first().ok_or_else(|| {
        crate::error::type_error("put() missing the SimpleQueue instance argument")
    })?;
    let item = args
        .get(1)
        .ok_or_else(|| crate::error::type_error("put() missing required argument: 'item'"))?;
    let interp = crate::builtins::reentrant_interp()?;
    let dq = field(interp, recv, "_queue")?;
    // (A deque's append in place; anything else through its method.)
    if deque::fast_op(deque::OP_APPEND, &dq, Some(item)).is_none() {
        let g = interp.builtins_dict();
        let append = interp.load_attr_public(&dq, "append")?;
        interp.call(&append, std::slice::from_ref(item), &[], &g)?;
    }
    open_gate(interp, recv)?;
    Ok(Object::None)
}

/// The oldest item of the queue's deque: `IndexError` when it's empty.
fn pop_item(interp: &mut crate::Interpreter, recv: &Object) -> Result<Object, RuntimeError> {
    let dq = field(interp, recv, "_queue")?;
    if let Some(item) = deque::fast_op(deque::OP_POPLEFT, &dq, None) {
        return Ok(item);
    }
    let g = interp.builtins_dict();
    let popleft = interp.load_attr_public(&dq, "popleft")?;
    interp.call(&popleft, &[], &[], &g)
}

/// `_weave_queue.simplequeue_pop(queue)` — `simplequeue_get`'s last step:
/// pop the oldest item, then open the gate if this queue's getters hold it.
/// That's what wakes the next parked getter when a burst of puts found the
/// gate already open, since only the first of them released it. `IndexError`
/// when another getter won the item.
fn simplequeue_pop(args: &[Object]) -> Result<Object, RuntimeError> {
    let recv = args.first().ok_or_else(|| {
        crate::error::type_error("simplequeue_pop() missing the SimpleQueue instance argument")
    })?;
    let interp = crate::builtins::reentrant_interp()?;
    let item = pop_item(interp, recv)?;
    open_gate(interp, recv)?;
    Ok(item)
}

/// `SimpleQueue.get(self, block=True, timeout=None)` — the item that is
/// already waiting, popped as [`simplequeue_pop`] pops it, when the call
/// gives no timeout (whose validation, like everything about an empty
/// queue, is the shim's `_get_slow`: waiting on the gate, `Empty`).
fn simplequeue_get(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let recv = args.first().ok_or_else(|| {
        crate::error::type_error("get() missing the SimpleQueue instance argument")
    })?;
    let interp = crate::builtins::reentrant_interp()?;
    // (Only `get()`, `get(block)` and `get(block=...)` are taken here.)
    let plain = args.len() <= 2 && kwargs.iter().all(|(k, _)| k == "block");
    if plain {
        let dq = field(interp, recv, "_queue")?;
        if let Some(item) = deque::fast_op(deque::OP_POPLEFT, &dq, None) {
            open_gate(interp, recv)?;
            return Ok(item);
        }
    }
    let g = interp.builtins_dict();
    let slow = interp.load_attr_public(recv, "_get_slow")?;
    interp.call(&slow, &args[1..], kwargs, &g)
}

/// `SimpleQueue.get_nowait(self)` — `get(block=False)`.
fn simplequeue_get_nowait(args: &[Object]) -> Result<Object, RuntimeError> {
    let [recv] = args else {
        return Err(crate::error::type_error("get_nowait() takes no arguments"));
    };
    simplequeue_get(&[recv.clone(), Object::Bool(false)], &[])
}

/// `SimpleQueue.put_nowait(self, item)` — `put(item, block=False)`, which
/// never blocks either way.
fn simplequeue_put_nowait(args: &[Object]) -> Result<Object, RuntimeError> {
    let [_, _] = args else {
        return Err(crate::error::type_error(
            "put_nowait() takes exactly one argument",
        ));
    };
    simplequeue_put(args)
}

/// Release the wake gate when a getter holds it (`if (self->locked) {
/// release; locked = 0 }`). The flag's test, clear and release are one
/// step, as CPython's critical section makes them without the GIL: two
/// threads that both saw it set would release the gate twice (`release
/// unlocked lock`). Nothing inside runs Python code; the caller's deque
/// operation, which may collect, stays outside.
fn open_gate(interp: &mut crate::Interpreter, recv: &Object) -> Result<(), RuntimeError> {
    // (No getter parked, the common case: the flag alone says so.)
    if !field(interp, recv, "_locked")?.is_truthy() {
        return Ok(());
    }
    static GATE: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _held = GATE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if interp.load_attr_public(recv, "_locked")?.is_truthy() {
        // Clear the flag before releasing: the woken getter sets it again
        // itself.
        interp.store_attr_public(recv, "_locked", Object::Bool(false))?;
        let g = interp.builtins_dict();
        let lock = interp.load_attr_public(recv, "_lock")?;
        let release = interp.load_attr_public(&lock, "release")?;
        interp.call(&release, &[], &[], &g)?;
    }
    Ok(())
}

pub fn build(_cache: &ModuleCache) -> Rc<PyModule> {
    let dict = Rc::new(RefCell::new(DictData::default()));
    {
        let mut d = dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("__name__")),
            Object::from_static("_weave_queue"),
        );
        d.insert(
            DictKey(Object::from_static("simplequeue_put")),
            Object::Builtin(Rc::new(BuiltinFn {
                name: "put",
                binds_instance: true,
                call: Box::new(simplequeue_put),
                // `put(item, block=True, timeout=None)` — the keywords are
                // documented no-ops, so they are accepted and dropped.
                call_kw: Some(Box::new(|args, _kwargs| simplequeue_put(args))),
            })),
        );
        d.insert(
            DictKey(Object::from_static("simplequeue_get")),
            Object::Builtin(Rc::new(BuiltinFn {
                name: "get",
                binds_instance: true,
                call: Box::new(|args| simplequeue_get(args, &[])),
                call_kw: Some(Box::new(simplequeue_get)),
            })),
        );
        for (key, name, body) in [
            (
                "simplequeue_get_nowait",
                "get_nowait",
                simplequeue_get_nowait as fn(&[Object]) -> Result<Object, RuntimeError>,
            ),
            (
                "simplequeue_put_nowait",
                "put_nowait",
                simplequeue_put_nowait,
            ),
        ] {
            d.insert(
                DictKey(Object::from_static(key)),
                Object::Builtin(Rc::new(BuiltinFn {
                    name,
                    binds_instance: true,
                    call: Box::new(body),
                    call_kw: None,
                })),
            );
        }
        d.insert(
            DictKey(Object::from_static("simplequeue_pop")),
            Object::Builtin(Rc::new(BuiltinFn {
                name: "simplequeue_pop",
                binds_instance: false,
                call: Box::new(simplequeue_pop),
                call_kw: None,
            })),
        );
    }
    Rc::new(PyModule {
        name: "_weave_queue".to_owned(),
        filename: None,
        dict,
    })
}

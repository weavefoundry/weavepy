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
//! next parked getter in one step.

use crate::sync::Rc;
use crate::sync::RefCell;

use crate::error::RuntimeError;
use crate::import::ModuleCache;
use crate::object::{BuiltinFn, DictData, DictKey, Object, PyModule};

/// `SimpleQueue.put(self, item, block=True, timeout=None)` — append to
/// the deque, then open the wake gate if a `get` is parked on it
/// (`simplequeue_put_locked`: `if (self->locked) { release; locked = 0 }`).
/// Never blocks; `block`/`timeout` are accepted and ignored (CPython
/// signature compatibility).
fn simplequeue_put(args: &[Object]) -> Result<Object, RuntimeError> {
    let recv = args.first().cloned().ok_or_else(|| {
        crate::error::type_error("put() missing the SimpleQueue instance argument")
    })?;
    let item = args
        .get(1)
        .cloned()
        .ok_or_else(|| crate::error::type_error("put() missing required argument: 'item'"))?;
    let interp = crate::builtins::reentrant_interp()?;
    let g = interp.builtins_dict();
    let dq = interp.load_attr_public(&recv, "_queue")?;
    let append = interp.load_attr_public(&dq, "append")?;
    interp.call(&append, &[item], &[], &g)?;
    open_gate(interp, &recv, &g)?;
    Ok(Object::None)
}

/// `_weave_queue.simplequeue_pop(queue)` — `simplequeue_get`'s last step:
/// pop the oldest item, then open the gate if this queue's getters hold it.
/// That's what wakes the next parked getter when a burst of puts found the
/// gate already open, since only the first of them released it. `IndexError`
/// when another getter won the item.
fn simplequeue_pop(args: &[Object]) -> Result<Object, RuntimeError> {
    let recv = args.first().cloned().ok_or_else(|| {
        crate::error::type_error("simplequeue_pop() missing the SimpleQueue instance argument")
    })?;
    let interp = crate::builtins::reentrant_interp()?;
    let g = interp.builtins_dict();
    let dq = interp.load_attr_public(&recv, "_queue")?;
    let popleft = interp.load_attr_public(&dq, "popleft")?;
    let item = interp.call(&popleft, &[], &[], &g)?;
    open_gate(interp, &recv, &g)?;
    Ok(item)
}

/// Release the wake gate when a getter holds it (`if (self->locked) {
/// release; locked = 0 }`). The flag's test, clear and release are one
/// step, as CPython's critical section makes them without the GIL: two
/// threads that both saw it set would release the gate twice (`release
/// unlocked lock`). Nothing inside runs Python code; the caller's deque
/// operation, which may collect, stays outside.
fn open_gate(
    interp: &mut crate::Interpreter,
    recv: &Object,
    g: &Rc<RefCell<DictData>>,
) -> Result<(), RuntimeError> {
    static GATE: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _held = GATE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if interp.load_attr_public(recv, "_locked")?.is_truthy() {
        // Clear the flag before releasing: the woken getter sets it again
        // itself.
        interp.store_attr_public(recv, "_locked", Object::Bool(false))?;
        let lock = interp.load_attr_public(recv, "_lock")?;
        let release = interp.load_attr_public(&lock, "release")?;
        interp.call(&release, &[], &[], g)?;
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

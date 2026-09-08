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
    if interp.load_attr_public(&recv, "_locked")?.is_truthy() {
        // Clear the flag before releasing: a second putter that runs in
        // between must not release the gate twice (`release unlocked
        // lock`), and the woken getter sets it again itself.
        interp.store_attr_public(&recv, "_locked", Object::Bool(false))?;
        let lock = interp.load_attr_public(&recv, "_lock")?;
        let release = interp.load_attr_public(&lock, "release")?;
        interp.call(&release, &[], &[], &g)?;
    }
    Ok(Object::None)
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
    }
    Rc::new(PyModule {
        name: "_weave_queue".to_owned(),
        filename: None,
        dict,
    })
}

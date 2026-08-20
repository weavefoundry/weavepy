//! `_weave_queue` — native backing for the `_queue` accelerator shim.
//!
//! CPython's `_queue.SimpleQueue.put` is a C method descriptor
//! (`method_descriptor` on the type, `builtin_function_or_method` once
//! bound — test_types.test_method_descriptor_crash exercises exactly
//! that through `put.__get__(instance)`). The Python shim in
//! `python/_queue.py` adopts this builtin as its `put`, keeping the
//! blocking `get` logic in Python where it can use
//! `threading.Semaphore`.

use crate::sync::Rc;
use crate::sync::RefCell;

use crate::error::RuntimeError;
use crate::import::ModuleCache;
use crate::object::{BuiltinFn, DictData, DictKey, Object, PyModule};

/// `SimpleQueue.put(self, item, block=True, timeout=None)` — append to
/// the deque, then release the counting semaphore. Never blocks;
/// `block`/`timeout` are accepted and ignored (CPython signature
/// compatibility).
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
    let sem = interp.load_attr_public(&recv, "_count")?;
    let release = interp.load_attr_public(&sem, "release")?;
    interp.call(&release, &[], &[], &g)?;
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

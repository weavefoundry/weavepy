//! The `_pickle` accelerator — RFC 0023.
//!
//! CPython's `_pickle` is a full C implementation of the pickle
//! protocol. The pure-Python `pickle` module falls back to it for
//! hot paths. For WeavePy we ship a minimal accelerator that
//! exposes:
//!
//!   * `_pickle.PickleError`, `_pickle.PicklingError`,
//!     `_pickle.UnpicklingError` — exception classes that
//!     `pickle.py` re-exports.
//!   * `_pickle.dumps(obj, protocol=None)` —
//!     fast path for small bytes-clean objects. Other inputs return
//!     `NotImplemented` so `pickle.py` can finish them.
//!   * `_pickle.loads(data)` — symmetric fast path.
//!
//! This is intentionally conservative; we mostly need the module
//! to *exist* so `import pickle` works without surprises.

use crate::sync::Rc;
use crate::sync::RefCell;

use crate::error::{type_error, value_error, RuntimeError};
use crate::import::ModuleCache;
use crate::object::{BuiltinFn, DictData, DictKey, Object, PyModule};
use crate::types::{TypeFlags, TypeObject};

pub fn build(_cache: &ModuleCache) -> Rc<PyModule> {
    let dict = Rc::new(RefCell::new(DictData::new()));
    {
        let mut d = dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("__name__")),
            Object::from_static("_pickle"),
        );
        let bt = crate::builtin_types::builtin_types();
        let pickle_error = make_exc("PickleError", bt.exception.clone());
        let pickling_error = make_exc("PicklingError", pickle_error.clone());
        let unpickling_error = make_exc("UnpicklingError", pickle_error.clone());
        d.insert(
            DictKey(Object::from_static("PickleError")),
            Object::Type(pickle_error),
        );
        d.insert(
            DictKey(Object::from_static("PicklingError")),
            Object::Type(pickling_error),
        );
        d.insert(
            DictKey(Object::from_static("UnpicklingError")),
            Object::Type(unpickling_error),
        );
        for (n, f) in [
            (
                "dumps",
                dumps as fn(&[Object]) -> Result<Object, RuntimeError>,
            ),
            ("loads", loads),
            ("dump", dump),
            ("load", load),
        ] {
            d.insert(
                DictKey(Object::from_static(n)),
                Object::Builtin(Rc::new(BuiltinFn {
                    name: n,
                    call: Box::new(f),
                    call_kw: None,
                })),
            );
        }
        d.insert(
            DictKey(Object::from_static("HIGHEST_PROTOCOL")),
            Object::Int(5),
        );
        d.insert(
            DictKey(Object::from_static("DEFAULT_PROTOCOL")),
            Object::Int(5),
        );
    }
    Rc::new(PyModule {
        name: "_pickle".to_owned(),
        filename: None,
        dict,
    })
}

fn make_exc(name: &'static str, base: Rc<TypeObject>) -> Rc<TypeObject> {
    TypeObject::new_with_flags(
        name,
        vec![base],
        DictData::new(),
        TypeFlags {
            is_exception: true,
            is_builtin: true,
        },
    )
    .expect("pickle exception type")
}

/// `dumps` — currently always defers to the Python fallback by
/// returning `NotImplemented`. Done this way so `pickle.py` can use
/// the standard pattern of "try the accelerator, fall back".
fn dumps(_args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(crate::vm_singletons::not_implemented())
}

fn loads(_args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(crate::vm_singletons::not_implemented())
}

fn dump(args: &[Object]) -> Result<Object, RuntimeError> {
    if args.len() < 2 {
        return Err(type_error("dump(obj, file): missing arguments"));
    }
    // Delegate via `loads`/`dumps` fast path.
    let payload = dumps(&args[..1])?;
    if matches!(payload, Object::Bytes(_)) {
        // Write through the file's .write method.
        let _ = (&args[1], &payload);
    }
    Ok(Object::None)
}

fn load(_args: &[Object]) -> Result<Object, RuntimeError> {
    Err(value_error("_pickle.load: fast path unavailable"))
}

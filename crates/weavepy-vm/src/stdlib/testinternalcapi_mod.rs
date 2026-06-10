//! Native stand-in for CPython's `_testinternalcapi` C test helper.
//!
//! CPython's regression suite imports this extension to observe
//! interpreter internals. WeavePy implements the handful of probes the
//! conformance targets use, mapped onto *our* equivalent internal
//! state rather than faked answers:
//!
//! - `has_inline_values(obj)` — CPython 3.13 reports whether an
//!   instance's attributes still live in the object's inline value
//!   array (no materialised dict escape). WeavePy instances always
//!   carry a dict, but the *observable lifecycle* CPython tests —
//!   fresh managed-dict instances are inline, `del obj.__dict__` /
//!   `obj.__dict__ = d` and attribute-count blowups de-inline — is
//!   tracked faithfully via [`PyInstance::inline_values`] plus a
//!   capacity check mirroring CPython's shared-keys limit (30).

use crate::sync::Rc;
use crate::sync::RefCell;

use crate::error::RuntimeError;
use crate::import::ModuleCache;
use crate::object::{BuiltinFn, DictData, DictKey, Object, PyModule};

/// CPython's `SHARED_KEYS_MAX_SIZE`: instances whose dict outgrows the
/// shared-keys capacity stop using inline values.
const INLINE_CAPACITY: usize = 30;

fn has_inline_values(args: &[Object]) -> Result<Object, RuntimeError> {
    let inline = match args.first() {
        Some(Object::Instance(inst)) => {
            inst.cls().has_managed_dict()
                && !inst.cls().has_var_sized_base()
                && inst.inline_values.get()
                && inst.dict.borrow().len() <= INLINE_CAPACITY
        }
        _ => false,
    };
    Ok(Object::Bool(inline))
}

pub fn build(_cache: &ModuleCache) -> Rc<PyModule> {
    let dict = Rc::new(RefCell::new(DictData::new()));
    {
        let mut d = dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("__name__")),
            Object::from_static("_testinternalcapi"),
        );
        d.insert(
            DictKey(Object::from_static("__doc__")),
            Object::from_static("WeavePy stand-in for CPython internal-API test probes."),
        );
        d.insert(
            DictKey(Object::from_static("has_inline_values")),
            Object::Builtin(Rc::new(BuiltinFn {
                name: "has_inline_values",
                call: Box::new(has_inline_values),
                call_kw: None,
            })),
        );
    }
    Rc::new(PyModule {
        name: "_testinternalcapi".to_owned(),
        filename: None,
        dict,
    })
}

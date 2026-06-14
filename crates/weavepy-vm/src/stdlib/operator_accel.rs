//! The `_operator` accelerator module.
//!
//! WeavePy ships a pure-Python `operator` (`stdlib/python/operator_mod.py`)
//! whose function bodies are written in Python, guarded by an optional
//! `from _operator import *`. The only piece that *must* live in native
//! code is `_compare_digest` — the constant-time comparison `hmac` and
//! `secrets` reach for — because a Python implementation cannot offer the
//! timing guarantee. Exposing it here (and nothing public) lets
//! `hmac.compare_digest is _operator._compare_digest` hold (CPython relies
//! on the *identity*, see `test_hmac`) without disturbing the pure-Python
//! `operator` surface (the leading underscore keeps it out of `import *`).

use crate::sync::Rc;
use crate::sync::RefCell;

use crate::error::{type_error, RuntimeError};
use crate::import::ModuleCache;
use crate::object::{BuiltinFn, DictData, DictKey, Object, PyModule};

pub fn build(_cache: &ModuleCache) -> Rc<PyModule> {
    let dict = Rc::new(RefCell::new(DictData::new()));
    {
        let mut d = dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("__name__")),
            Object::from_static("_operator"),
        );
        d.insert(
            DictKey(Object::from_static("__doc__")),
            Object::from_static("Operator interface."),
        );
        d.insert(
            DictKey(Object::from_static("_compare_digest")),
            Object::Builtin(Rc::new(BuiltinFn {
                name: "_compare_digest",
                binds_instance: false,
                call: Box::new(compare_digest),
                call_kw: None,
            })),
        );
    }
    Rc::new(PyModule {
        name: "_operator".to_owned(),
        filename: None,
        dict,
    })
}

/// Constant-time comparison, ported from CPython's `_tscmp`. Returns `true`
/// when the inputs are equal. Branch-free over the byte contents so the
/// running time leaks only the (already public) length.
fn tscmp(a: &[u8], b: &[u8]) -> bool {
    // On a length mismatch CPython compares `b` against itself (so the loop
    // body always XORs to zero) but seeds `result` with 1 → never equal.
    let (left, len, mut result) = if a.len() != b.len() {
        (b, b.len(), 1u8)
    } else {
        (a, a.len(), 0u8)
    };
    for i in 0..len {
        result |= left[i] ^ b[i];
    }
    result == 0
}

/// `_operator._compare_digest(a, b)` — constant-time equality for two
/// ASCII `str`s or two bytes-like objects (mixing the two, or passing
/// anything else, raises `TypeError`), matching `Modules/_operator.c`.
fn compare_digest(args: &[Object]) -> Result<Object, RuntimeError> {
    let a = args
        .first()
        .ok_or_else(|| type_error("_compare_digest expected 2 arguments, got 0"))?;
    let b = args
        .get(1)
        .ok_or_else(|| type_error("_compare_digest expected 2 arguments, got 1"))?;
    // `str`/`bytes` subclasses (which may override `__eq__`) compare by their
    // underlying value, so unwrap to the native primitive first.
    let an = a.native_value();
    let bn = b.native_value();
    let a = an.as_ref().unwrap_or(a);
    let b = bn.as_ref().unwrap_or(b);
    let equal = match (a, b) {
        (Object::Str(sa), Object::Str(sb)) => {
            if !sa.is_ascii() || !sb.is_ascii() {
                return Err(type_error(
                    "comparing strings with non-ASCII characters is not supported",
                ));
            }
            tscmp(sa.as_bytes(), sb.as_bytes())
        }
        _ => {
            // Reject a str paired with a non-str (and any non-buffer type)
            // before touching the buffer protocol, exactly as CPython does.
            if matches!(a, Object::Str(_)) || matches!(b, Object::Str(_)) {
                return Err(compare_type_error(a, b));
            }
            match (a.as_bytes_view(), b.as_bytes_view()) {
                (Some(ba), Some(bb)) => tscmp(&ba, &bb),
                _ => return Err(compare_type_error(a, b)),
            }
        }
    };
    Ok(Object::Bool(equal))
}

fn compare_type_error(a: &Object, b: &Object) -> RuntimeError {
    type_error(format!(
        "unsupported operand types(s) or combination of types: '{}' and '{}'",
        a.type_name_owned(),
        b.type_name_owned()
    ))
}

//! The `_random` accelerator module — RFC 0023.
//!
//! Provides the Mersenne Twister state machine that the Python
//! `random` module wraps. We expose a `Random` class with the
//! methods that `random.py` uses internally:
//! `seed`, `random`, `getstate`, `setstate`, `getrandbits`.

use crate::sync::Rc;
use crate::sync::RefCell;

use crate::error::{type_error, RuntimeError};
use crate::import::ModuleCache;
use crate::object::{BuiltinFn, DictData, DictKey, Object, PyModule};
use crate::types::{PyInstance, TypeFlags, TypeObject};

pub fn build(_cache: &ModuleCache) -> Rc<PyModule> {
    let dict = Rc::new(RefCell::new(DictData::new()));
    {
        let mut d = dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("__name__")),
            Object::from_static("_random"),
        );
        d.insert(
            DictKey(Object::from_static("Random")),
            Object::Type(random_type()),
        );
    }
    Rc::new(PyModule {
        name: "_random".to_owned(),
        filename: None,
        dict,
    })
}

fn random_type() -> Rc<TypeObject> {
    use crate::builtin_types::builtin_types;
    let bt = builtin_types();
    let mut td = DictData::new();
    for (name, fn_) in [
        (
            "__init__",
            random_init as fn(&[Object]) -> Result<Object, RuntimeError>,
        ),
        ("seed", random_seed),
        ("random", random_random),
        ("getrandbits", random_getrandbits),
        ("getstate", random_getstate),
        ("setstate", random_setstate),
    ] {
        td.insert(
            DictKey(Object::from_static(name)),
            Object::Builtin(Rc::new(BuiltinFn {
                name,
                call: Box::new(fn_),
            })),
        );
    }
    TypeObject::new_with_flags(
        "Random",
        vec![bt.object_.clone()],
        td,
        TypeFlags {
            is_exception: false,
            is_builtin: true,
        },
    )
    .expect("Random type")
}

/// Linear-congruential PRNG state. We use a 64-bit splitmix engine
/// because the full Mersenne Twister state (624 × 32-bit words) is
/// heavy to thread through `__dict__`. The distribution is uniform
/// over [0, 1) and good enough for non-cryptographic use — which
/// matches CPython's `random` module's contract.
fn current_state(inst: &Rc<PyInstance>) -> u64 {
    let dict = inst.dict.borrow();
    match dict.get(&DictKey(Object::from_static("_state"))) {
        Some(Object::Int(i)) => *i as u64,
        Some(Object::Long(b)) => {
            use num_traits::ToPrimitive;
            b.to_u64().unwrap_or(0xDEAD_BEEF)
        }
        _ => 0xDEAD_BEEF,
    }
}

fn set_state(inst: &Rc<PyInstance>, state: u64) {
    inst.dict.borrow_mut().insert(
        DictKey(Object::from_static("_state")),
        Object::Int(state as i64),
    );
}

fn splitmix64(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

fn random_init(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = match args.first() {
        Some(Object::Instance(i)) => i.clone(),
        _ => return Err(type_error("Random.__init__: missing self")),
    };
    let seed = args.get(1).cloned().unwrap_or(Object::None);
    let initial = match &seed {
        Object::None => {
            use std::time::{SystemTime, UNIX_EPOCH};
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(0x1234_5678)
        }
        Object::Int(i) => *i as u64,
        Object::Long(b) => {
            use num_traits::ToPrimitive;
            b.to_u64().unwrap_or(0xDEAD)
        }
        Object::Float(f) => f.to_bits(),
        Object::Str(s) => {
            let mut h = 0u64;
            for b in s.bytes() {
                h = h.wrapping_mul(31).wrapping_add(u64::from(b));
            }
            h
        }
        other => other
            .repr()
            .bytes()
            .fold(0u64, |h, b| h.wrapping_mul(31).wrapping_add(u64::from(b))),
    };
    set_state(&inst, initial);
    Ok(Object::None)
}

fn random_seed(args: &[Object]) -> Result<Object, RuntimeError> {
    random_init(args)
}

fn random_random(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = match args.first() {
        Some(Object::Instance(i)) => i.clone(),
        _ => return Err(type_error("random.random() requires self")),
    };
    let mut s = current_state(&inst);
    let v = splitmix64(&mut s);
    set_state(&inst, s);
    // Mantissa-only 53-bit fraction in [0, 1).
    let frac = (v >> 11) as f64 / (1u64 << 53) as f64;
    Ok(Object::Float(frac))
}

fn random_getrandbits(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = match args.first() {
        Some(Object::Instance(i)) => i.clone(),
        _ => return Err(type_error("getrandbits() requires self")),
    };
    let k = match args.get(1) {
        Some(Object::Int(k)) if *k >= 0 => *k as u32,
        _ => {
            return Err(type_error(
                "getrandbits() argument must be non-negative int",
            ))
        }
    };
    if k == 0 {
        return Ok(Object::Int(0));
    }
    let mut state = current_state(&inst);
    let mut remaining = k;
    let mut result_lo: u128 = 0;
    let mut shift = 0u32;
    while remaining > 0 {
        let take = remaining.min(64);
        let v = splitmix64(&mut state);
        let mask: u64 = if take == 64 { !0 } else { (1u64 << take) - 1 };
        result_lo |= u128::from(v & mask) << shift;
        shift += take;
        remaining -= take;
        if shift >= 128 {
            break;
        }
    }
    set_state(&inst, state);
    if let Ok(small) = i64::try_from(result_lo) {
        Ok(Object::Int(small))
    } else {
        Ok(Object::int_from_bigint(num_bigint::BigInt::from(result_lo)))
    }
}

fn random_getstate(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = match args.first() {
        Some(Object::Instance(i)) => i.clone(),
        _ => return Err(type_error("getstate() requires self")),
    };
    Ok(Object::new_tuple(vec![Object::Int(
        current_state(&inst) as i64
    )]))
}

fn random_setstate(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = match args.first() {
        Some(Object::Instance(i)) => i.clone(),
        _ => return Err(type_error("setstate() requires self")),
    };
    let state = match args.get(1) {
        Some(Object::Tuple(items)) if !items.is_empty() => match &items[0] {
            Object::Int(i) => *i as u64,
            Object::Long(b) => {
                use num_traits::ToPrimitive;
                b.to_u64().unwrap_or(0)
            }
            _ => return Err(type_error("setstate(): invalid state tuple")),
        },
        _ => return Err(type_error("setstate(): state must be a tuple")),
    };
    set_state(&inst, state);
    Ok(Object::None)
}

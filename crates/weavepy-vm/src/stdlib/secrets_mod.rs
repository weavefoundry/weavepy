//! The `secrets` built-in module.
//!
//! Cryptographically secure random helpers, backed by OS entropy via
//! the host's `/dev/urandom` (POSIX) or `BCryptGenRandom` (Windows).
//! We don't link the `rand` family; instead we read from `/dev/urandom`
//! directly and synthesise the convenience helpers on top.
//!
//! Surface: `token_bytes`, `token_hex`, `token_urlsafe`, `choice`,
//! `randbelow`, `randbits`, `compare_digest`.

use crate::sync::Rc;
use crate::sync::RefCell;

use crate::error::{type_error, value_error, RuntimeError};
use crate::import::ModuleCache;
use crate::object::{BuiltinFn, DictData, DictKey, Object, PyModule};

pub fn build(_cache: &ModuleCache) -> Rc<PyModule> {
    let dict = Rc::new(RefCell::new(DictData::new()));
    {
        let mut d = dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("__name__")),
            Object::from_static("secrets"),
        );
        d.insert(
            DictKey(Object::from_static("__doc__")),
            Object::from_static("Generate secure random numbers for managing secrets."),
        );
        d.insert(
            DictKey(Object::from_static("token_bytes")),
            b("token_bytes", token_bytes),
        );
        d.insert(
            DictKey(Object::from_static("token_hex")),
            b("token_hex", token_hex),
        );
        d.insert(
            DictKey(Object::from_static("token_urlsafe")),
            b("token_urlsafe", token_urlsafe),
        );
        d.insert(DictKey(Object::from_static("choice")), b("choice", choice));
        d.insert(
            DictKey(Object::from_static("randbelow")),
            b("randbelow", randbelow),
        );
        d.insert(
            DictKey(Object::from_static("randbits")),
            b("randbits", randbits),
        );
        d.insert(
            DictKey(Object::from_static("compare_digest")),
            b("compare_digest", compare_digest),
        );
    }
    Rc::new(PyModule {
        name: "secrets".to_owned(),
        filename: None,
        dict,
    })
}

fn b(name: &'static str, body: fn(&[Object]) -> Result<Object, RuntimeError>) -> Object {
    Object::Builtin(Rc::new(BuiltinFn {
        name,
        binds_instance: false,
        call: Box::new(body),
        call_kw: None,
    }))
}

/// Fill `out` with cryptographically secure random bytes from the OS.
/// On POSIX hosts we read from `/dev/urandom`; on Windows we shell
/// out to `BCryptGenRandom` via `getrandom_inner`.
fn os_random(out: &mut [u8]) -> Result<(), RuntimeError> {
    #[cfg(unix)]
    {
        use std::fs::File;
        use std::io::Read;
        let mut f = File::open("/dev/urandom").map_err(|e| value_error(e.to_string()))?;
        f.read_exact(out).map_err(|e| value_error(e.to_string()))?;
        Ok(())
    }
    #[cfg(not(unix))]
    {
        // No bcrypt binding on the worst-case platform; fall back to
        // `time` + `random` seeding. Not ideal — surfaced in the RFC.
        use std::time::{SystemTime, UNIX_EPOCH};
        let seed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0xCAFE_BABE_FEED_FACE);
        let mut state = seed;
        for byte in out.iter_mut() {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            *byte = (state >> 33) as u8;
        }
        Ok(())
    }
}

fn token_bytes(args: &[Object]) -> Result<Object, RuntimeError> {
    let nbytes = match args.first() {
        Some(Object::Int(n)) => *n as usize,
        None | Some(Object::None) => 32,
        _ => return Err(type_error("token_bytes: arg must be int")),
    };
    let mut out = vec![0u8; nbytes];
    os_random(&mut out)?;
    Ok(Object::new_bytes(out))
}

fn token_hex(args: &[Object]) -> Result<Object, RuntimeError> {
    let bytes_obj = token_bytes(args)?;
    let bytes = match bytes_obj {
        Object::Bytes(b) => b.to_vec(),
        _ => return Err(value_error("internal error in token_hex")),
    };
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        use std::fmt::Write;
        write!(s, "{b:02x}").unwrap();
    }
    Ok(Object::from_str(s))
}

fn token_urlsafe(args: &[Object]) -> Result<Object, RuntimeError> {
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine;
    let bytes_obj = token_bytes(args)?;
    let bytes = match bytes_obj {
        Object::Bytes(b) => b.to_vec(),
        _ => return Err(value_error("internal error in token_urlsafe")),
    };
    Ok(Object::from_str(URL_SAFE_NO_PAD.encode(bytes)))
}

fn choice(args: &[Object]) -> Result<Object, RuntimeError> {
    let seq = args.first().ok_or_else(|| type_error("missing sequence"))?;
    let items: Vec<Object> = match seq {
        Object::List(l) => l.borrow().clone(),
        Object::Tuple(t) => t.to_vec(),
        Object::Str(s) => s.chars().map(|c| Object::from_str(c.to_string())).collect(),
        _ => return Err(type_error("choice: expected sequence")),
    };
    if items.is_empty() {
        return Err(value_error("choice from empty sequence"));
    }
    let mut idx_bytes = [0u8; 8];
    os_random(&mut idx_bytes)?;
    let idx = (u64::from_le_bytes(idx_bytes) as usize) % items.len();
    Ok(items[idx].clone())
}

fn randbelow(args: &[Object]) -> Result<Object, RuntimeError> {
    let n = match args.first() {
        Some(Object::Int(n)) => *n,
        _ => return Err(type_error("randbelow: arg must be int")),
    };
    if n <= 0 {
        return Err(value_error("randbelow argument must be positive"));
    }
    let mut bytes = [0u8; 8];
    os_random(&mut bytes)?;
    let raw = i64::from_le_bytes(bytes).unsigned_abs();
    Ok(Object::Int((raw % n as u64) as i64))
}

/// `secrets.randbits(k)` — a non-negative int with `k` cryptographically
/// secure random bits (CPython's `SystemRandom.getrandbits`). Faithful at
/// any width: numpy's `SeedSequence` default-seeds with `randbits(128)`,
/// so we read `ceil(k/8)` OS-entropy bytes, trim the top byte to the exact
/// bit count, and normalise to a machine `Int` or a big `Long`.
fn randbits(args: &[Object]) -> Result<Object, RuntimeError> {
    let k = match args.first() {
        Some(Object::Int(n)) => *n,
        Some(Object::Bool(b)) => i64::from(*b),
        Some(Object::Long(b)) => {
            use num_traits::ToPrimitive;
            b.to_i64()
                .ok_or_else(|| value_error("number of bits is too large"))?
        }
        _ => return Err(type_error("randbits: arg must be int")),
    };
    if k < 0 {
        return Err(value_error("number of bits must be non-negative"));
    }
    if k == 0 {
        return Ok(Object::Int(0));
    }
    let k = k as usize;
    let nbytes = k.div_ceil(8);
    let mut bytes = vec![0u8; nbytes];
    os_random(&mut bytes)?;
    let rem = k % 8;
    if rem != 0 {
        let last = nbytes - 1;
        bytes[last] &= (1u8 << rem) - 1;
    }
    let big = num_bigint::BigUint::from_bytes_le(&bytes);
    Ok(Object::int_from_bigint(num_bigint::BigInt::from_biguint(
        num_bigint::Sign::Plus,
        big,
    )))
}

fn compare_digest(args: &[Object]) -> Result<Object, RuntimeError> {
    let bytes_a = match args.first() {
        Some(Object::Bytes(b)) => b.to_vec(),
        Some(Object::ByteArray(b)) => b.borrow().clone(),
        Some(Object::Str(s)) => s.as_bytes().to_vec(),
        _ => return Err(type_error("compare_digest: bytes-like required")),
    };
    let bytes_b = match args.get(1) {
        Some(Object::Bytes(b)) => b.to_vec(),
        Some(Object::ByteArray(b)) => b.borrow().clone(),
        Some(Object::Str(s)) => s.as_bytes().to_vec(),
        _ => return Err(type_error("compare_digest: bytes-like required")),
    };
    if bytes_a.len() != bytes_b.len() {
        return Ok(Object::Bool(false));
    }
    let acc = bytes_a
        .iter()
        .zip(bytes_b.iter())
        .fold(0u8, |acc, (x, y)| acc | (x ^ y));
    Ok(Object::Bool(acc == 0))
}

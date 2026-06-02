//! The `random` built-in module.
//!
//! Implements the most-used functions on top of a small splitmix64
//! generator. The interface matches CPython, but the underlying
//! algorithm differs — `random.seed(0); random.random()` therefore
//! produces different bits than CPython. The contract we make is
//! *statistical*: outputs are uniformly distributed.

use crate::sync::Rc;
use crate::sync::RefCell;

use crate::error::{type_error, value_error, RuntimeError};
use crate::import::ModuleCache;
use crate::object::{BuiltinFn, DictData, DictKey, Object, PyModule};

/// Splitmix64 — small, fast, good enough for everyday Python code.
struct Rng {
    state: u64,
}

impl Rng {
    fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn next_double(&mut self) -> f64 {
        let bits = self.next_u64() >> 11;
        (bits as f64) / ((1u64 << 53) as f64)
    }
}

thread_local! {
    static RNG: RefCell<Rng> = RefCell::new(Rng::new(default_seed()));
}

fn default_seed() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0xDEAD_BEEF_DEAD_BEEF)
        .max(1)
}

pub fn build(_cache: &ModuleCache) -> Rc<PyModule> {
    let dict = Rc::new(RefCell::new(DictData::new()));
    {
        let mut d = dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("__name__")),
            Object::from_static("random"),
        );
        d.insert(
            DictKey(Object::from_static("__doc__")),
            Object::from_static("Pseudo-random number generators."),
        );
        d.insert(DictKey(Object::from_static("seed")), b("seed", random_seed));
        d.insert(
            DictKey(Object::from_static("random")),
            b("random", random_random),
        );
        d.insert(
            DictKey(Object::from_static("uniform")),
            b("uniform", random_uniform),
        );
        d.insert(
            DictKey(Object::from_static("randint")),
            b("randint", random_randint),
        );
        d.insert(
            DictKey(Object::from_static("randrange")),
            b("randrange", random_randrange),
        );
        d.insert(
            DictKey(Object::from_static("choice")),
            b("choice", random_choice),
        );
        d.insert(
            DictKey(Object::from_static("choices")),
            b("choices", random_choices),
        );
        d.insert(
            DictKey(Object::from_static("shuffle")),
            b("shuffle", random_shuffle),
        );
        d.insert(
            DictKey(Object::from_static("sample")),
            b("sample", random_sample),
        );
        d.insert(
            DictKey(Object::from_static("gauss")),
            b("gauss", random_gauss),
        );
        d.insert(
            DictKey(Object::from_static("getrandbits")),
            b("getrandbits", random_getrandbits),
        );
    }
    Rc::new(PyModule {
        name: "random".to_owned(),
        filename: None,
        dict,
    })
}

fn b(name: &'static str, body: fn(&[Object]) -> Result<Object, RuntimeError>) -> Object {
    Object::Builtin(Rc::new(BuiltinFn {
        name,
        call: Box::new(body),
        call_kw: None,
    }))
}

fn random_seed(args: &[Object]) -> Result<Object, RuntimeError> {
    let seed = match args.first() {
        Some(Object::Int(n)) => *n as u64,
        Some(Object::None) | None => default_seed(),
        _ => return Err(type_error("seed must be int or None")),
    };
    RNG.with(|r| {
        *r.borrow_mut() = Rng::new(seed.max(1));
    });
    Ok(Object::None)
}

fn random_random(_args: &[Object]) -> Result<Object, RuntimeError> {
    let v = RNG.with(|r| r.borrow_mut().next_double());
    Ok(Object::Float(v))
}

/// Module-level `random.getrandbits(k)` — a non-negative int with `k`
/// random bits (`0 <= result < 2**k`), drawn from the module RNG.
fn random_getrandbits(args: &[Object]) -> Result<Object, RuntimeError> {
    use num_bigint::{BigInt, Sign};
    let k = match args.first() {
        Some(Object::Bool(b)) => u64::from(*b),
        Some(Object::Int(n)) if *n >= 0 => *n as u64,
        Some(Object::Int(_)) => {
            return Err(value_error("number of bits must be non-negative"))
        }
        _ => return Err(type_error("getrandbits() requires an integer argument")),
    };
    if k == 0 {
        return Ok(Object::Int(0));
    }
    let nbytes = ((k + 7) / 8) as usize;
    let excess = (nbytes as u64) * 8 - k;
    let mut buf = vec![0u8; nbytes];
    RNG.with(|r| {
        let mut rng = r.borrow_mut();
        let mut i = 0;
        while i < nbytes {
            let w = rng.next_u64().to_le_bytes();
            let take = (nbytes - i).min(8);
            buf[i..i + take].copy_from_slice(&w[..take]);
            i += take;
        }
    });
    if excess > 0 {
        buf[nbytes - 1] &= 0xFFu8 >> excess;
    }
    Ok(Object::int_from_bigint(BigInt::from_bytes_le(Sign::Plus, &buf)))
}

fn random_uniform(args: &[Object]) -> Result<Object, RuntimeError> {
    let a = to_f64(args.first())?;
    let b = to_f64(args.get(1))?;
    let r = RNG.with(|r| r.borrow_mut().next_double());
    Ok(Object::Float(a + (b - a) * r))
}

fn random_randint(args: &[Object]) -> Result<Object, RuntimeError> {
    let a = to_i64(args.first())?;
    let b = to_i64(args.get(1))?;
    if a > b {
        return Err(value_error("randint: a must be <= b"));
    }
    let span = (b - a + 1) as u64;
    let raw = RNG.with(|r| r.borrow_mut().next_u64());
    Ok(Object::Int(a + (raw % span) as i64))
}

/// Coerce a `randrange` bound to a `BigInt`, accepting any integer
/// (incl. arbitrary-precision) — CPython's `randrange` has no upper
/// bound on the magnitude of its arguments.
fn to_bigint(arg: Option<&Object>) -> Result<num_bigint::BigInt, RuntimeError> {
    use num_bigint::BigInt;
    match arg {
        Some(Object::Int(i)) => Ok(BigInt::from(*i)),
        Some(Object::Bool(b)) => Ok(BigInt::from(i64::from(*b))),
        Some(Object::Long(b)) => Ok((**b).clone()),
        _ => Err(type_error("expected int")),
    }
}

/// Uniform random `BigInt` in `[0, n)` via rejection sampling on a
/// bit-masked candidate (`n` must be positive). Mirrors the shape of
/// CPython's `Random._randbelow` without depending on i64 width.
fn rand_below_bigint(n: &num_bigint::BigInt) -> num_bigint::BigInt {
    use num_bigint::{BigInt, Sign};
    let bits = n.bits();
    if bits == 0 {
        return BigInt::from(0);
    }
    let nbytes = ((bits + 7) / 8) as usize;
    let excess = (nbytes as u64) * 8 - bits;
    loop {
        let mut buf = vec![0u8; nbytes];
        RNG.with(|r| {
            let mut rng = r.borrow_mut();
            let mut i = 0;
            while i < nbytes {
                let w = rng.next_u64().to_le_bytes();
                let take = (nbytes - i).min(8);
                buf[i..i + take].copy_from_slice(&w[..take]);
                i += take;
            }
        });
        if excess > 0 {
            buf[nbytes - 1] &= 0xFFu8 >> excess;
        }
        let cand = BigInt::from_bytes_le(Sign::Plus, &buf);
        if &cand < n {
            return cand;
        }
    }
}

fn random_randrange(args: &[Object]) -> Result<Object, RuntimeError> {
    use num_bigint::BigInt;
    use num_integer::Integer;
    let zero = BigInt::from(0);
    match args.len() {
        1 => {
            let stop = to_bigint(args.first())?;
            if stop <= zero {
                return Err(value_error("empty range for randrange()"));
            }
            Ok(Object::int_from_bigint(rand_below_bigint(&stop)))
        }
        2 => {
            let start = to_bigint(args.first())?;
            let stop = to_bigint(args.get(1))?;
            let width = &stop - &start;
            if width <= zero {
                return Err(value_error("empty range for randrange()"));
            }
            Ok(Object::int_from_bigint(start + rand_below_bigint(&width)))
        }
        3 => {
            let start = to_bigint(args.first())?;
            let stop = to_bigint(args.get(1))?;
            let step = to_bigint(args.get(2))?;
            if step == zero {
                return Err(value_error("zero step for randrange()"));
            }
            let width = &stop - &start;
            let one = BigInt::from(1);
            // Count of reachable values: ceil(width/step), via floor div
            // on the CPython-adjusted numerator (matches `range` length).
            let n = if step > zero {
                (&width + &step - &one).div_floor(&step)
            } else {
                (&width + &step + &one).div_floor(&step)
            };
            if n <= zero {
                return Err(value_error("empty range for randrange()"));
            }
            Ok(Object::int_from_bigint(start + step * rand_below_bigint(&n)))
        }
        _ => Err(type_error("randrange expects 1-3 args")),
    }
}

fn random_choice(args: &[Object]) -> Result<Object, RuntimeError> {
    let seq = args
        .first()
        .ok_or_else(|| type_error("choice expects a sequence"))?;
    let items = sequence_items(seq)?;
    if items.is_empty() {
        return Err(value_error("choice from empty sequence"));
    }
    let raw = RNG.with(|r| r.borrow_mut().next_u64());
    let idx = (raw as usize) % items.len();
    Ok(items[idx].clone())
}

fn random_choices(args: &[Object]) -> Result<Object, RuntimeError> {
    let seq = args
        .first()
        .ok_or_else(|| type_error("choices expects a sequence"))?;
    let items = sequence_items(seq)?;
    if items.is_empty() {
        return Err(value_error("choices from empty sequence"));
    }
    let k = match args.get(1) {
        Some(Object::Int(n)) => *n as usize,
        None => 1,
        _ => return Err(type_error("k must be int")),
    };
    let mut out = Vec::with_capacity(k);
    for _ in 0..k {
        let raw = RNG.with(|r| r.borrow_mut().next_u64());
        out.push(items[(raw as usize) % items.len()].clone());
    }
    Ok(Object::new_list(out))
}

fn random_shuffle(args: &[Object]) -> Result<Object, RuntimeError> {
    let list = match args.first() {
        Some(Object::List(l)) => l.clone(),
        _ => return Err(type_error("shuffle expects a list")),
    };
    let mut data = list.borrow_mut();
    let n = data.len();
    if n > 1 {
        for i in (1..n).rev() {
            let raw = RNG.with(|r| r.borrow_mut().next_u64());
            let j = (raw as usize) % (i + 1);
            data.swap(i, j);
        }
    }
    Ok(Object::None)
}

fn random_sample(args: &[Object]) -> Result<Object, RuntimeError> {
    let seq = args
        .first()
        .ok_or_else(|| type_error("sample expects a sequence"))?;
    let k = match args.get(1) {
        Some(Object::Int(n)) => *n as usize,
        _ => return Err(type_error("sample k must be int")),
    };
    let mut items = sequence_items(seq)?;
    if k > items.len() {
        return Err(value_error("sample larger than population"));
    }
    let mut out = Vec::with_capacity(k);
    for _ in 0..k {
        let raw = RNG.with(|r| r.borrow_mut().next_u64());
        let idx = (raw as usize) % items.len();
        out.push(items.swap_remove(idx));
    }
    Ok(Object::new_list(out))
}

fn random_gauss(args: &[Object]) -> Result<Object, RuntimeError> {
    let mu = to_f64(args.first())?;
    let sigma = to_f64(args.get(1))?;
    let (u1, u2) = RNG.with(|r| {
        let mut r = r.borrow_mut();
        (r.next_double().max(f64::MIN_POSITIVE), r.next_double())
    });
    let mag = sigma * (-2.0 * u1.ln()).sqrt();
    let z = mag * (2.0 * std::f64::consts::PI * u2).cos();
    Ok(Object::Float(mu + z))
}

fn to_i64(arg: Option<&Object>) -> Result<i64, RuntimeError> {
    match arg {
        Some(Object::Int(i)) => Ok(*i),
        Some(Object::Bool(b)) => Ok(i64::from(*b)),
        _ => Err(type_error("expected int")),
    }
}

fn to_f64(arg: Option<&Object>) -> Result<f64, RuntimeError> {
    match arg {
        Some(Object::Int(i)) => Ok(*i as f64),
        Some(Object::Float(f)) => Ok(*f),
        Some(Object::Bool(b)) => Ok(i64::from(*b) as f64),
        _ => Err(type_error("expected number")),
    }
}

fn sequence_items(obj: &Object) -> Result<Vec<Object>, RuntimeError> {
    match obj {
        Object::List(l) => Ok(l.borrow().clone()),
        Object::Tuple(t) => Ok(t.to_vec()),
        Object::Str(s) => Ok(s.chars().map(|c| Object::from_str(c.to_string())).collect()),
        Object::Range(r) => {
            let mut out = Vec::new();
            let mut i = r.start;
            if r.step > 0 {
                while i < r.stop {
                    out.push(Object::Int(i));
                    i += r.step;
                }
            } else if r.step < 0 {
                while i > r.stop {
                    out.push(Object::Int(i));
                    i += r.step;
                }
            }
            Ok(out)
        }
        _ => Err(type_error("expected a sequence")),
    }
}

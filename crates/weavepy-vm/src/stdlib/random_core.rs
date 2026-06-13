//! The `_random` accelerator module — RFC 0023 / RFC 0037.
//!
//! A faithful port of CPython's `_randommodule.c`: the genuine
//! MT19937 Mersenne Twister, seeded with `init_by_array` exactly like
//! CPython, so `random.Random(42)` produces bit-identical streams.
//! The frozen pure-Python `random.py` (verbatim CPython) wraps this
//! class with the user-facing distribution API.

use crate::sync::Rc;
use crate::sync::RefCell;

use crate::error::{type_error, value_error, RuntimeError};
use crate::import::ModuleCache;
use crate::object::{BuiltinFn, DictData, DictKey, Object, PyModule};
use crate::types::{PyInstance, TypeFlags, TypeObject};

const N: usize = 624;
const M: usize = 397;
const MATRIX_A: u32 = 0x9908_b0df;
const UPPER_MASK: u32 = 0x8000_0000;
const LOWER_MASK: u32 = 0x7fff_ffff;

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
        ("randbytes", random_randbytes),
        ("getstate", random_getstate),
        ("setstate", random_setstate),
    ] {
        td.insert(
            DictKey(Object::from_static(name)),
            Object::Builtin(Rc::new(BuiltinFn {
                name,
                binds_instance: true,
                call: Box::new(fn_),
                call_kw: None,
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

// ===================================================================
// MT19937 core (identical to CPython's `_randommodule.c`)
// ===================================================================

struct Mt {
    key: [u32; N],
    pos: usize,
}

impl Mt {
    /// `init_genrand` — seed the state vector from a single u32.
    fn init_genrand(s: u32) -> Self {
        let mut key = [0u32; N];
        key[0] = s;
        for i in 1..N {
            key[i] = (1_812_433_253u32)
                .wrapping_mul(key[i - 1] ^ (key[i - 1] >> 30))
                .wrapping_add(i as u32);
        }
        Mt { key, pos: N }
    }

    /// `init_by_array` — seed from an arbitrary-length u32 key.
    fn init_by_array(init_key: &[u32]) -> Self {
        let mut mt = Self::init_genrand(19_650_218);
        let key_length = init_key.len();
        let mut i: usize = 1;
        let mut j: usize = 0;
        let mut k = N.max(key_length);
        while k > 0 {
            mt.key[i] = (mt.key[i]
                ^ (mt.key[i - 1] ^ (mt.key[i - 1] >> 30)).wrapping_mul(1_664_525))
            .wrapping_add(init_key[j])
            .wrapping_add(j as u32);
            i += 1;
            j += 1;
            if i >= N {
                mt.key[0] = mt.key[N - 1];
                i = 1;
            }
            if j >= key_length {
                j = 0;
            }
            k -= 1;
        }
        k = N - 1;
        while k > 0 {
            mt.key[i] = (mt.key[i]
                ^ (mt.key[i - 1] ^ (mt.key[i - 1] >> 30)).wrapping_mul(1_566_083_941))
            .wrapping_sub(i as u32);
            i += 1;
            if i >= N {
                mt.key[0] = mt.key[N - 1];
                i = 1;
            }
            k -= 1;
        }
        mt.key[0] = 0x8000_0000;
        mt
    }

    /// `genrand_uint32` — the raw 32-bit output stream.
    fn genrand_u32(&mut self) -> u32 {
        if self.pos >= N {
            // Regenerate the whole block.
            for kk in 0..(N - M) {
                let y = (self.key[kk] & UPPER_MASK) | (self.key[kk + 1] & LOWER_MASK);
                self.key[kk] = self.key[kk + M] ^ (y >> 1) ^ if y & 1 != 0 { MATRIX_A } else { 0 };
            }
            for kk in (N - M)..(N - 1) {
                let y = (self.key[kk] & UPPER_MASK) | (self.key[kk + 1] & LOWER_MASK);
                self.key[kk] =
                    self.key[kk + M - N] ^ (y >> 1) ^ if y & 1 != 0 { MATRIX_A } else { 0 };
            }
            let y = (self.key[N - 1] & UPPER_MASK) | (self.key[0] & LOWER_MASK);
            self.key[N - 1] = self.key[M - 1] ^ (y >> 1) ^ if y & 1 != 0 { MATRIX_A } else { 0 };
            self.pos = 0;
        }
        let mut y = self.key[self.pos];
        self.pos += 1;
        y ^= y >> 11;
        y ^= (y << 7) & 0x9d2c_5680;
        y ^= (y << 15) & 0xefc6_0000;
        y ^ (y >> 18)
    }
}

// ===================================================================
// Instance-state plumbing. The 624-word state lives in a bytearray in
// the instance dict (so Python-level subclasses share it), the cursor
// in an int.
// ===================================================================

const STATE_KEY: &str = "_mt_state";
const POS_KEY: &str = "_mt_pos";

fn self_instance(args: &[Object], what: &str) -> Result<Rc<PyInstance>, RuntimeError> {
    match args.first() {
        Some(Object::Instance(i)) => Ok(i.clone()),
        _ => Err(type_error(format!("{what} requires a _random.Random self"))),
    }
}

fn load_mt(inst: &Rc<PyInstance>) -> Result<Mt, RuntimeError> {
    let dict = inst.dict.borrow();
    let bytes = match dict.get(&DictKey(Object::from_static(STATE_KEY))) {
        Some(Object::ByteArray(b)) => b.clone(),
        _ => {
            drop(dict);
            // Unseeded use (e.g. subclass skipping __init__): seed from
            // system entropy, as CPython does at allocation time.
            let mt = seed_from_entropy();
            store_mt(inst, &mt);
            return Ok(mt);
        }
    };
    let pos = match dict.get(&DictKey(Object::from_static(POS_KEY))) {
        Some(Object::Int(i)) => *i as usize,
        _ => N,
    };
    let buf = bytes.borrow();
    let mut key = [0u32; N];
    for (i, chunk) in buf.chunks_exact(4).enumerate().take(N) {
        key[i] = u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
    }
    Ok(Mt { key, pos })
}

fn store_mt(inst: &Rc<PyInstance>, mt: &Mt) {
    let mut buf = Vec::with_capacity(N * 4);
    for w in &mt.key {
        buf.extend_from_slice(&w.to_le_bytes());
    }
    let mut dict = inst.dict.borrow_mut();
    dict.insert(
        DictKey(Object::from_static(STATE_KEY)),
        Object::ByteArray(Rc::new(RefCell::new(buf))),
    );
    dict.insert(
        DictKey(Object::from_static(POS_KEY)),
        Object::Int(mt.pos as i64),
    );
}

/// Mutate-in-place fast path: run `f` against the deserialized state,
/// then persist the (changed) words back into the bytearray buffer.
fn with_mt<R>(inst: &Rc<PyInstance>, f: impl FnOnce(&mut Mt) -> R) -> Result<R, RuntimeError> {
    let mut mt = load_mt(inst)?;
    let r = f(&mut mt);
    store_mt(inst, &mt);
    Ok(r)
}

fn seed_from_entropy() -> Mt {
    // CPython pulls 624 words from the OS urandom pool (falling back
    // to time+pid). getrandom/urandom equivalents without new deps:
    // hash system time and a process-unique counter through splitmix64.
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0xDEAD_BEEF);
    let pid = u64::from(std::process::id());
    let mut s = nanos ^ (pid << 32) ^ 0x9E37_79B9_7F4A_7C15;
    let mut words = [0u32; N];
    for w in &mut words {
        s = s.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = s;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        *w = (z ^ (z >> 31)) as u32;
    }
    Mt::init_by_array(&words)
}

/// CPython `random_seed`: None → entropy; int → `init_by_array` over
/// the absolute value's 32-bit little-endian digits; floats and other
/// hashables are reduced like CPython reduces them (via hash) by the
/// pure-Python layer before they get here.
fn seed_from_object(arg: &Object) -> Result<Mt, RuntimeError> {
    use num_bigint::BigInt;
    use num_traits::Signed;
    let n: BigInt = match arg {
        Object::None => return Ok(seed_from_entropy()),
        Object::Int(i) => BigInt::from(*i),
        Object::Long(b) => (**b).clone(),
        Object::Bool(b) => BigInt::from(i64::from(*b)),
        Object::Float(f) => BigInt::from(f.to_bits()),
        Object::Str(s) => {
            // Defensive: random.py normally converts str seeds to int
            // first (sha512). Fall back to a stable byte fold.
            BigInt::from_bytes_le(num_bigint::Sign::Plus, s.as_bytes())
        }
        other => {
            return Err(type_error(format!(
                "cannot seed from '{}'",
                other.type_name()
            )))
        }
    };
    let n = n.abs();
    let (_, bytes) = n.to_bytes_le();
    let mut words: Vec<u32> = bytes
        .chunks(4)
        .map(|c| {
            let mut b = [0u8; 4];
            b[..c.len()].copy_from_slice(c);
            u32::from_le_bytes(b)
        })
        .collect();
    if words.is_empty() {
        words.push(0);
    }
    Ok(Mt::init_by_array(&words))
}

// ===================================================================
// Methods
// ===================================================================

fn random_init(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = self_instance(args, "Random.__init__")?;
    let seed = args.get(1).cloned().unwrap_or(Object::None);
    let mt = seed_from_object(&seed)?;
    store_mt(&inst, &mt);
    Ok(Object::None)
}

fn random_seed(args: &[Object]) -> Result<Object, RuntimeError> {
    random_init(args)
}

/// `genrand_res53`: 53-bit resolution double in [0, 1).
fn random_random(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = self_instance(args, "random()")?;
    let v = with_mt(&inst, |mt| {
        let a = mt.genrand_u32() >> 5;
        let b = mt.genrand_u32() >> 6;
        (f64::from(a) * 67_108_864.0 + f64::from(b)) * (1.0 / 9_007_199_254_740_992.0)
    })?;
    Ok(Object::Float(v))
}

/// `getrandbits(k)`: k random bits as a non-negative int, assembled
/// from 32-bit words little-endian with the *last* word truncated —
/// CPython's exact layout, which `random.py`'s `_randbelow` depends on.
fn random_getrandbits(args: &[Object]) -> Result<Object, RuntimeError> {
    use num_bigint::{BigUint, Sign};
    let inst = self_instance(args, "getrandbits()")?;
    let k = match args.get(1) {
        Some(Object::Bool(b)) => i64::from(*b),
        Some(Object::Int(i)) => *i,
        Some(Object::Long(b)) => {
            use num_traits::ToPrimitive;
            b.to_i64()
                .ok_or_else(|| value_error("number of bits is too large"))?
        }
        _ => return Err(type_error("getrandbits() requires an integer argument")),
    };
    if k < 0 {
        return Err(value_error("number of bits must be non-negative"));
    }
    if k == 0 {
        return Ok(Object::Int(0));
    }
    let k = k as u64;
    if k <= 32 {
        let v = with_mt(&inst, |mt| mt.genrand_u32())? >> (32 - k as u32);
        return Ok(Object::Int(i64::from(v)));
    }
    let words = ((k - 1) / 32 + 1) as usize;
    let digits = with_mt(&inst, |mt| {
        let mut out = Vec::with_capacity(words);
        let mut remaining = k;
        for _ in 0..words {
            let mut r = mt.genrand_u32();
            if remaining < 32 {
                r >>= 32 - remaining as u32;
            }
            out.push(r);
            remaining = remaining.saturating_sub(32);
        }
        out
    })?;
    let mut bytes = Vec::with_capacity(words * 4);
    for d in &digits {
        bytes.extend_from_slice(&d.to_le_bytes());
    }
    let big = BigUint::from_bytes_le(&bytes);
    Ok(Object::int_from_bigint(num_bigint::BigInt::from_biguint(
        Sign::Plus,
        big,
    )))
}

/// `randbytes(n)` — CPython implements this on the C class.
fn random_randbytes(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = self_instance(args, "randbytes()")?;
    let n = match args.get(1) {
        Some(Object::Int(i)) if *i >= 0 => *i as usize,
        Some(Object::Int(_)) => return Err(value_error("negative argument not allowed")),
        _ => return Err(type_error("randbytes() requires a non-negative int")),
    };
    let out = with_mt(&inst, |mt| {
        let mut buf = Vec::with_capacity(n);
        while buf.len() < n {
            let w = mt.genrand_u32().to_le_bytes();
            let take = (n - buf.len()).min(4);
            buf.extend_from_slice(&w[..take]);
        }
        buf
    })?;
    Ok(Object::new_bytes(out))
}

/// `getstate()` → 625-tuple: the 624 state words plus the cursor.
fn random_getstate(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = self_instance(args, "getstate()")?;
    let mt = load_mt(&inst)?;
    let mut items: Vec<Object> = mt.key.iter().map(|w| Object::Int(i64::from(*w))).collect();
    items.push(Object::Int(mt.pos as i64));
    Ok(Object::new_tuple(items))
}

fn random_setstate(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = self_instance(args, "setstate()")?;
    let items = match args.get(1) {
        Some(Object::Tuple(t)) => t.clone(),
        _ => return Err(type_error("state vector must be a tuple")),
    };
    if items.len() != N + 1 {
        return Err(value_error(format!(
            "state vector is the wrong size; expected {}, got {}",
            N + 1,
            items.len()
        )));
    }
    let mut key = [0u32; N];
    for (i, slot) in key.iter_mut().enumerate() {
        *slot = match &items[i] {
            Object::Int(v) => *v as u32,
            Object::Long(b) => {
                use num_traits::ToPrimitive;
                b.to_u64().unwrap_or(0) as u32
            }
            _ => return Err(type_error("state vector items must be ints")),
        };
    }
    let pos = match &items[N] {
        Object::Int(v) if (0..=N as i64).contains(v) => *v as usize,
        Object::Int(_) => return Err(value_error("invalid state")),
        _ => return Err(type_error("state vector items must be ints")),
    };
    store_mt(&inst, &Mt { key, pos });
    Ok(Object::None)
}

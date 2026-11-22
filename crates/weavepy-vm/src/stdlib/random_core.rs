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
    let dict = Rc::new(RefCell::new(DictData::default()));
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
        for (name, f) in [
            (
                "_int_methods",
                int_methods as fn(&[Object]) -> Result<Object, RuntimeError>,
            ),
            ("_seal_int_methods", seal_int_methods),
        ] {
            d.insert(
                DictKey(Object::from_static(name)),
                Object::Builtin(Rc::new(BuiltinFn {
                    name,
                    binds_instance: false,
                    call: Box::new(f),
                    call_kw: None,
                })),
            );
        }
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
    let mut td = DictData::default();
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
        let b = Rc::new(BuiltinFn {
            name,
            binds_instance: true,
            call: Box::new(fn_),
            call_kw: None,
        });
        // `random()` runs no Python code, so the dispatch loop and
        // compiled callers call it in place; `getrandbits` does for a
        // plain `int` count (an index-protocol argument runs Python).
        match name {
            "random" => {
                crate::leaf_builtins::register(&b);
                crate::leaf_builtins::register_jit_method(&b, OP_RANDOM);
            }
            "getrandbits" => crate::leaf_builtins::register_fast(&b, getrandbits_leaf),
            _ => {}
        }
        td.insert(DictKey(Object::from_static(name)), Object::Builtin(b));
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

    /// Regenerate the whole block of state words (`genrand_uint32`'s
    /// refill).
    fn regenerate(&mut self) {
        for kk in 0..(N - M) {
            let y = (self.key[kk] & UPPER_MASK) | (self.key[kk + 1] & LOWER_MASK);
            self.key[kk] = self.key[kk + M] ^ (y >> 1) ^ if y & 1 != 0 { MATRIX_A } else { 0 };
        }
        for kk in (N - M)..(N - 1) {
            let y = (self.key[kk] & UPPER_MASK) | (self.key[kk + 1] & LOWER_MASK);
            self.key[kk] = self.key[kk + M - N] ^ (y >> 1) ^ if y & 1 != 0 { MATRIX_A } else { 0 };
        }
        let y = (self.key[N - 1] & UPPER_MASK) | (self.key[0] & LOWER_MASK);
        self.key[N - 1] = self.key[M - 1] ^ (y >> 1) ^ if y & 1 != 0 { MATRIX_A } else { 0 };
        self.pos = 0;
    }
}

/// `genrand_uint32`'s output tempering.
#[inline]
fn temper(mut y: u32) -> u32 {
    y ^= y >> 11;
    y ^= (y << 7) & 0x9d2c_5680;
    y ^= (y << 15) & 0xefc6_0000;
    y ^ (y >> 18)
}

/// The persisted state, borrowed in place: the 624 key words, then the
/// cursor, each little-endian (see [`STATE_LEN`]).
struct MtBytes<'a>(&'a mut [u8]);

impl MtBytes<'_> {
    #[inline]
    fn word(&self, i: usize) -> u32 {
        let mut w = [0u8; 4];
        w.copy_from_slice(&self.0[4 * i..4 * i + 4]);
        u32::from_le_bytes(w)
    }

    #[inline]
    fn set_word(&mut self, i: usize, v: u32) {
        self.0[4 * i..4 * i + 4].copy_from_slice(&v.to_le_bytes());
    }

    fn to_mt(&self) -> Mt {
        let mut key = [0u32; N];
        for (i, k) in key.iter_mut().enumerate() {
            *k = self.word(i);
        }
        Mt {
            key,
            pos: (self.word(N) as usize).min(N),
        }
    }

    fn write(&mut self, mt: &Mt) {
        for (i, k) in mt.key.iter().enumerate() {
            self.set_word(i, *k);
        }
        self.set_word(N, mt.pos as u32);
    }

    /// `genrand_uint32` — the raw 32-bit output stream.
    fn genrand_u32(&mut self) -> u32 {
        let mut pos = self.word(N) as usize;
        if pos >= N {
            let mut mt = self.to_mt();
            mt.regenerate();
            self.write(&mt);
            pos = 0;
        }
        let y = self.word(pos);
        self.set_word(N, pos as u32 + 1);
        temper(y)
    }
}

// ===================================================================
// Instance-state plumbing. The state lives in a bytearray in a slot of
// the instance's own that no descriptor names (so Python-level
// subclasses share it, and the instance's `__dict__` holds only what
// Python stored there, as CPython's does): the 624 key words, then the
// cursor. The generator runs on it in place.
// ===================================================================

const STATE_KEY: &str = "_mt_state";
/// The state bytearray's length: the key words and the cursor.
const STATE_LEN: usize = (N + 1) * 4;

fn self_instance(args: &[Object], what: &str) -> Result<Rc<PyInstance>, RuntimeError> {
    match args.first() {
        Some(Object::Instance(i)) => Ok(i.clone()),
        _ => Err(type_error(format!("{what} requires a _random.Random self"))),
    }
}

/// The instance's state buffer, seeding it from system entropy when it
/// is missing (a subclass skipping `__init__`), as CPython does at
/// allocation time.
fn state_buffer(inst: &Rc<PyInstance>) -> Rc<RefCell<Vec<u8>>> {
    let found = match inst.slots.borrow().get(STATE_KEY) {
        Some(Object::ByteArray(b)) if b.borrow().len() == STATE_LEN => Some(b.clone()),
        _ => None,
    };
    found.unwrap_or_else(|| store_mt(inst, &seed_from_entropy()))
}

fn load_mt(inst: &Rc<PyInstance>) -> Mt {
    let buf = state_buffer(inst);
    let mut bytes = buf.borrow_mut();
    MtBytes(&mut bytes).to_mt()
}

fn store_mt(inst: &Rc<PyInstance>, mt: &Mt) -> Rc<RefCell<Vec<u8>>> {
    let mut bytes = vec![0u8; STATE_LEN];
    MtBytes(&mut bytes).write(mt);
    let buf = Rc::new(RefCell::new(bytes));
    inst.slot_set(STATE_KEY, Object::ByteArray(buf.clone()));
    buf
}

/// Run `f` against the instance's state, in place.
fn with_mt<R>(inst: &Rc<PyInstance>, f: impl FnOnce(&mut MtBytes<'_>) -> R) -> R {
    let buf = state_buffer(inst);
    let mut bytes = buf.borrow_mut();
    f(&mut MtBytes(&mut bytes))
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
/// The JIT's direct operation for `random()` (a native method lane op,
/// distinct from `collections_native`'s).
pub(crate) const OP_RANDOM: u8 = 32;

/// `random()` on an instance receiver, as the JIT's native method lane
/// calls it directly (`None` declines: the full call raises exactly).
pub(crate) fn random_fast(recv: &Object) -> Option<Object> {
    let Object::Instance(inst) = recv else {
        return None;
    };
    Some(Object::Float(random_from(inst)))
}

fn random_from(inst: &Rc<PyInstance>) -> f64 {
    with_mt(inst, |mt| {
        let a = mt.genrand_u32() >> 5;
        let b = mt.genrand_u32() >> 6;
        (f64::from(a) * 67_108_864.0 + f64::from(b)) * (1.0 / 9_007_199_254_740_992.0)
    })
}

fn random_random(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = self_instance(args, "random()")?;
    Ok(Object::Float(random_from(&inst)))
}

/// `getrandbits(k)`: k random bits as a non-negative int, assembled
/// from 32-bit words little-endian with the *last* word truncated —
/// CPython's exact layout, which `random.py`'s `_randbelow` depends on.
fn random_getrandbits(args: &[Object]) -> Result<Object, RuntimeError> {
    use num_bigint::{BigUint, Sign};
    let inst = self_instance(args, "getrandbits()")?;
    // METH_O in CPython: exactly one argument
    // (test_random.test_getrandbits passes two and expects TypeError).
    if args.len() > 2 {
        return Err(type_error(format!(
            "getrandbits() takes exactly one argument ({} given)",
            args.len() - 1
        )));
    }
    // 3.14 (gh-129889): `k` is a C `uint64_t` (`PyLong_AsUnsignedLongLong`
    // semantics): a negative int of any size is ValueError "Cannot convert
    // negative int", one past 2**64 is OverflowError, and a count too large
    // to allocate is MemoryError (test_random.test_getrandbits).
    let negative = || value_error("Cannot convert negative int");
    let k: u64 = match args.get(1) {
        Some(Object::Bool(b)) => u64::from(*b),
        Some(Object::Int(i)) => u64::try_from(*i).map_err(|_| negative())?,
        Some(Object::Long(b)) => {
            use num_traits::ToPrimitive;
            if b.sign() == Sign::Minus {
                return Err(negative());
            }
            b.to_u64().ok_or_else(|| {
                crate::error::overflow_error("Python int too large for C uint64_t")
            })?
        }
        // Anything indexable — `getrandbits(MyIndex(100))` runs the user
        // __index__ (test_random.test_getrandbits).
        Some(other @ Object::Instance(_)) => {
            u64::try_from(crate::builtins::coerce_index_i64(other)?).map_err(|_| negative())?
        }
        _ => return Err(type_error("getrandbits() requires an integer argument")),
    };
    if k == 0 {
        return Ok(Object::Int(0));
    }
    if k <= 32 {
        let v = with_mt(&inst, |mt| mt.genrand_u32()) >> (32 - k as u32);
        return Ok(Object::Int(i64::from(v)));
    }
    if (k - 1) / 32 + 1 > (isize::MAX as u64) / 4 {
        return Err(crate::error::memory_error(""));
    }
    let words = ((k - 1) / 32 + 1) as usize;
    let mut buf: Vec<u32> = Vec::new();
    if buf.try_reserve_exact(words).is_err() {
        return Err(crate::error::memory_error(""));
    }
    let digits = with_mt(&inst, |mt| {
        let mut out = buf;
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
    });
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

/// The `random.Random` class whose integer methods [`int_methods`]
/// replaced, and its attribute version then.
static PY_RANDOM_CLASS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
static PY_RANDOM_VERSION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// `_int_methods(Random)`: native `randint`, `randrange`, `choice` and
/// `shuffle` for
/// `random.py`'s `Random` class, as `(name, method)` pairs for the module
/// to assign. Each serves an instance of the class itself, unchanged since,
/// whose `_randbelow` and `getrandbits` are the class's, with plain int
/// arguments, by `_randbelow_with_getrandbits`'s own draws (the same
/// numbers from the same state); anything else calls the Python method it
/// replaces.
fn int_methods(args: &[Object]) -> Result<Object, RuntimeError> {
    let [Object::Type(cls)] = args else {
        return Err(type_error("_int_methods() expects the Random class"));
    };
    let mut out = Vec::new();
    for (name, fast) in [
        ("randint", randint_fast as crate::stdlib::struct_mod::Fast),
        ("randrange", randrange_fast),
        ("choice", choice_fast),
        ("shuffle", shuffle_fast),
    ] {
        let orig = cls
            .dict
            .borrow()
            .get(&crate::object::StrKey(name))
            .cloned()
            .ok_or_else(|| type_error(format!("_int_methods(): no {name} to replace")))?;
        out.push(Object::new_tuple(vec![
            Object::from_static(name),
            crate::stdlib::struct_mod::fast_method(cls, name, fast, orig, false),
        ]));
    }
    Ok(Object::new_list(out))
}

/// `_seal_int_methods(Random)`, once the module has assigned what
/// [`int_methods`] made: the class and its version as they now stand,
/// which the methods' native halves require.
fn seal_int_methods(args: &[Object]) -> Result<Object, RuntimeError> {
    let [Object::Type(cls)] = args else {
        return Err(type_error("_seal_int_methods() expects the Random class"));
    };
    PY_RANDOM_CLASS.store(
        Rc::as_ptr(cls) as usize,
        std::sync::atomic::Ordering::Relaxed,
    );
    PY_RANDOM_VERSION.store(cls.attr_version.get(), std::sync::atomic::Ordering::Relaxed);
    Ok(Object::None)
}

/// The instance at `recv` when it is a plain `random.Random` (see
/// [`int_methods`]) and no trace or profile hook is watching.
fn plain_random(recv: &Object) -> Option<&Rc<PyInstance>> {
    let Object::Instance(inst) = recv else {
        return None;
    };
    let cls = inst.cls_raw();
    // (A trace or profile hook sees the Python methods' frames, as a
    // debugger stepping into `randint` expects.)
    if crate::trace::any_observers_active()
        || std::ptr::from_ref(cls) as usize
            != PY_RANDOM_CLASS.load(std::sync::atomic::Ordering::Relaxed)
        || cls.attr_version.get() != PY_RANDOM_VERSION.load(std::sync::atomic::Ordering::Relaxed)
        || inst.attr_get_str("_randbelow").is_some()
        || inst.attr_get_str("getrandbits").is_some()
    {
        return None;
    }
    Some(inst)
}

/// `_randbelow_with_getrandbits(n)` for `0 < n < 2**63`: draws of
/// `n.bit_length()` bits, as `getrandbits` assembles them, until one is
/// below `n`.
fn randbelow(inst: &Rc<PyInstance>, n: u64) -> u64 {
    let k = 64 - n.leading_zeros();
    with_mt(inst, |mt| loop {
        let r = if k <= 32 {
            u64::from(mt.genrand_u32() >> (32 - k))
        } else {
            let lo = u64::from(mt.genrand_u32());
            let hi = u64::from(mt.genrand_u32() >> (64 - k));
            lo | (hi << 32)
        };
        if r < n {
            break r;
        }
    })
}

/// A plain int argument's value (`_index` of an `int` or `bool`).
fn plain_int(o: &Object) -> Option<i64> {
    match *o {
        Object::Int(i) => Some(i),
        Object::Bool(b) => Some(i64::from(b)),
        _ => None,
    }
}

/// `randint(a, b)` of a plain `Random` (see [`int_methods`]).
fn randint_fast(args: &[Object]) -> Option<Result<Object, RuntimeError>> {
    let [recv, a, b] = args else {
        return None;
    };
    let inst = plain_random(recv)?;
    let (a, b) = (plain_int(a)?, plain_int(b)?);
    let width = u64::try_from(b.checked_sub(a)?.checked_add(1)?).ok()?;
    if width == 0 {
        return None;
    }
    Some(Ok(Object::Int(a + randbelow(inst, width) as i64)))
}

/// `randrange(stop)`, `randrange(start, stop)` and a step of one, of a
/// plain `Random` (see [`int_methods`]).
fn randrange_fast(args: &[Object]) -> Option<Result<Object, RuntimeError>> {
    let (recv, start, stop) = match args {
        [recv, stop] => (recv, 0, plain_int(stop)?),
        [recv, start, stop] => (recv, plain_int(start)?, plain_int(stop)?),
        [recv, start, stop, Object::Int(1)] => (recv, plain_int(start)?, plain_int(stop)?),
        _ => return None,
    };
    let inst = plain_random(recv)?;
    let width = u64::try_from(stop.checked_sub(start)?).ok()?;
    if width == 0 {
        return None;
    }
    Some(Ok(Object::Int(start + randbelow(inst, width) as i64)))
}

/// `choice(seq)` of a non-empty list or tuple, or an ASCII string, of a
/// plain `Random` (see [`int_methods`]).
fn choice_fast(args: &[Object]) -> Option<Result<Object, RuntimeError>> {
    let [recv, seq] = args else {
        return None;
    };
    let inst = plain_random(recv)?;
    let n = match seq {
        Object::List(l) => l.try_borrow().ok()?.len(),
        Object::Tuple(t) => t.len(),
        Object::Str(s) if crate::shared_value::SharedStr::is_ascii(s) => s.len(),
        _ => return None,
    };
    if n == 0 {
        return None;
    }
    let i = randbelow(inst, n as u64) as usize;
    Some(Ok(match seq {
        Object::List(l) => l.try_borrow().ok()?.get(i)?.clone(),
        Object::Tuple(t) => t.get(i)?.clone(),
        Object::Str(s) => Object::from_str(&s[i..=i]),
        _ => return None,
    }))
}

/// `shuffle(x)` of a list, of a plain `Random` (see [`int_methods`]):
/// `random.py`'s Fisher-Yates walk from the end, each swap partner drawn as
/// `_randbelow(i + 1)` draws it. (Swapping items runs no code.)
fn shuffle_fast(args: &[Object]) -> Option<Result<Object, RuntimeError>> {
    let [recv, Object::List(l)] = args else {
        return None;
    };
    let inst = plain_random(recv)?;
    let mut xs = l.try_borrow_mut().ok()?;
    for i in (1..xs.len()).rev() {
        let j = randbelow(inst, i as u64 + 1) as usize;
        xs.swap(i, j);
    }
    Some(Ok(Object::None))
}

/// `getrandbits`'s leaf half: an exact `int` or `bool` count never runs
/// Python code.
fn getrandbits_leaf(args: &[Object]) -> Option<Result<Object, RuntimeError>> {
    match args {
        [Object::Instance(_), Object::Int(_) | Object::Bool(_)] => Some(random_getrandbits(args)),
        _ => None,
    }
}

/// `randbytes(n)` — CPython implements this on the C class.
fn random_randbytes(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = self_instance(args, "randbytes()")?;
    let n = match args.get(1) {
        Some(Object::Int(i)) if *i >= 0 => *i as usize,
        Some(Object::Int(_)) => return Err(value_error("negative argument not allowed")),
        // Clinic 'n' conversion: an int beyond ssize_t is an
        // OverflowError (randbytes(1 << 1000), test_random).
        Some(Object::Long(_)) => {
            return Err(crate::error::overflow_error(
                "Python int too large to convert to C ssize_t",
            ))
        }
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
    });
    Ok(Object::new_bytes(out))
}

/// `getstate()` → 625-tuple: the 624 state words plus the cursor.
fn random_getstate(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = self_instance(args, "getstate()")?;
    let mt = load_mt(&inst);
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

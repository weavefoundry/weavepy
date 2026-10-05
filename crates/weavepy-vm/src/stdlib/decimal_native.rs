//! Native fast paths for the `decimal` types.
//!
//! WeavePy's `decimal` is the Python `_decimal` module (a fork of
//! `_pydecimal` carrying the C accelerator's surface). CPython runs the
//! same operations in C through libmpdec; interpreted, one `a + b` is
//! dozens of Python calls. This module gives the hot operations native
//! bodies that work directly on `Decimal`'s `__slots__` storage
//! (`_sign`, `_int`, `_exp`, `_is_special`) and on the current context's
//! attributes: construction, arithmetic, comparisons, hashing, rounding
//! (`quantize`, `to_integral*`, `round`), string conversion and
//! formatting, the predicates, and the `Context` methods built on them.
//!
//! [`install`] (called at the end of `_decimal`'s module body) marks the
//! exact `Decimal` class with [`KIND_DECIMAL`], puts the native callables
//! in the `Decimal` and `Context` class dicts, and keeps the Python
//! methods they replaced. A native body serves only finite operands of
//! the exact classes (and `int`s where Python converts them), under an
//! exact `Context` whose flag and trap mappings it can read; it computes
//! the result with the same algorithms as the Python code, then either
//! records the conditions it raised in `context.flags` or, when one of
//! them is trapped (or the operation needs a special value, a huge
//! coefficient, or anything else unusual), calls the replaced Python
//! method instead. Nothing is recorded before that decision, so the
//! Python method sees the state it would have seen, and every exception
//! and message stays the Python code's.
//!
//! Coefficients are `u128`s while they fit and big integers beyond, and
//! a natively built value stores its slots in a layout shared by every
//! such instance (see `SlotStorage::from_layout`).

use std::cmp::Ordering;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering::Relaxed};

use num_bigint::{BigInt, BigUint, Sign};
use num_integer::Integer;
use num_traits::{ToPrimitive, Zero};

use crate::error::{type_error, RuntimeError};
use crate::import::ModuleCache;
use crate::object::{BuiltinFn, DictData, DictKey, Object, PyModule};
use crate::shared_value::{SharedSlice, SharedStr};
use crate::stdlib::datetime_native::PackedSlots;
use crate::sync::{Rc, RefCell, Weak};
use crate::types::{PyInstance, SlotStorage, TypeObject};

/// The `native_kind` of the exact `Decimal` class (the `datetime` kinds
/// are below 8).
pub(crate) const KIND_DECIMAL: u8 = 16;

/// The most digits a native coefficient (or a zero padding) may reach;
/// past it the Python code runs.
const MAX_DIGITS: u64 = 4000;

/// `MAX_EMAX` and `MIN_ETINY` of the 64-bit build: the context-free
/// constructor's exact range.
const MAX_EMAX: i128 = 999_999_999_999_999_999;
const MIN_ETINY: i128 = -1_999_999_999_999_999_997;

const POW10: [u128; 39] = {
    let mut t = [1u128; 39];
    let mut i = 1;
    while i < 39 {
        t[i] = t[i - 1] * 10;
        i += 1;
    }
    t
};

// ---------------------------------------------------------------------
// Coefficients.

/// A non-negative coefficient: a `u128` while it fits, else a big
/// integer (never one that fits in a `u128`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Coef {
    S(u128),
    B(BigUint),
}

/// Write `v`'s decimal digits at the end of `buf`; the start index.
fn format_u128(v: u128, buf: &mut [u8; 40]) -> usize {
    // `u128` division is a library call; the low 19 digits go through
    // `u64` arithmetic.
    let mut i = buf.len();
    let (mut hi, mut lo) = if v > u128::from(u64::MAX) {
        let p = POW10[19];
        (v / p, (v % p) as u64)
    } else {
        (0, v as u64)
    };
    let pad = hi != 0;
    let start = i;
    loop {
        i -= 1;
        buf[i] = b'0' + (lo % 10) as u8;
        lo /= 10;
        if lo == 0 && (!pad || start - i == 19) {
            break;
        }
    }
    while pad && start - i < 19 {
        i -= 1;
        buf[i] = b'0';
    }
    while hi != 0 {
        i -= 1;
        buf[i] = b'0' + (hi % 10) as u8;
        hi /= 10;
    }
    i
}

#[inline]
fn digits_u128(v: u128) -> u64 {
    if let Ok(w) = u64::try_from(v) {
        if w == 0 {
            return 1;
        }
        let bits = 64 - w.leading_zeros() as usize;
        let t = (bits * 1233) >> 12;
        return if w >= POW10[t] as u64 {
            t as u64 + 1
        } else {
            t as u64
        };
    }
    let bits = 128 - v.leading_zeros() as usize;
    let t = (bits * 1233) >> 12;
    if v >= POW10[t] {
        t as u64 + 1
    } else {
        t as u64
    }
}

#[cold]
#[inline(never)]
fn digits_big(b: &BigUint) -> u64 {
    let bits = b.bits();
    let t = (bits * 1233) >> 12;
    if *b >= pow10_big(t) {
        t + 1
    } else {
        t
    }
}

/// `divmod(v, 10**k)` for `k < 39`, through `u64` arithmetic when `v`
/// fits (`u128` division is a library call).
#[inline]
fn divrem_pow10_u128(v: u128, k: u64) -> (u128, u128) {
    if let Ok(w) = u64::try_from(v) {
        if k < 20 {
            let p = POW10[k as usize] as u64;
            return (u128::from(w / p), u128::from(w % p));
        }
        return (0, v);
    }
    let p = POW10[k as usize];
    (v / p, v % p)
}

fn pow10_big(k: u64) -> BigUint {
    num_traits::pow(BigUint::from(10u32), k as usize)
}

impl Coef {
    const ZERO: Coef = Coef::S(0);

    fn from_big(b: BigUint) -> Coef {
        match b.to_u128() {
            Some(v) => Coef::S(v),
            None => Coef::B(b),
        }
    }

    fn to_big(&self) -> BigUint {
        match self {
            Coef::S(v) => BigUint::from(*v),
            Coef::B(b) => b.clone(),
        }
    }

    #[inline]
    fn is_zero(&self) -> bool {
        matches!(self, Coef::S(0))
    }

    #[inline]
    fn is_one(&self) -> bool {
        matches!(self, Coef::S(1))
    }

    /// The number of decimal digits (`1` for zero).
    #[inline]
    fn ndigits(&self) -> u64 {
        match self {
            Coef::S(v) => digits_u128(*v),
            Coef::B(b) => digits_big(b),
        }
    }

    /// `self * 10**k`, or `None` past [`MAX_DIGITS`].
    fn mul_pow10(&self, k: u64) -> Option<Coef> {
        if k == 0 || self.is_zero() {
            return Some(self.clone());
        }
        if let Coef::S(v) = self {
            if k < 39 {
                if let Some(r) = v.checked_mul(POW10[k as usize]) {
                    return Some(Coef::S(r));
                }
            }
        }
        if self.ndigits().saturating_add(k) > MAX_DIGITS {
            return None;
        }
        Some(Coef::from_big(self.to_big() * pow10_big(k)))
    }

    /// `divmod(self, 10**k)`.
    fn divrem_pow10(&self, k: u64) -> (Coef, Coef) {
        if k == 0 {
            return (self.clone(), Coef::ZERO);
        }
        match self {
            Coef::S(v) => {
                if k < 39 {
                    let (q, r) = divrem_pow10_u128(*v, k);
                    (Coef::S(q), Coef::S(r))
                } else {
                    (Coef::ZERO, self.clone())
                }
            }
            Coef::B(b) => {
                if k > self.ndigits() {
                    return (Coef::ZERO, self.clone());
                }
                let (q, r) = b.div_rem(&pow10_big(k));
                (Coef::from_big(q), Coef::from_big(r))
            }
        }
    }

    fn add(&self, o: &Coef) -> Coef {
        if let (Coef::S(a), Coef::S(b)) = (self, o) {
            if let Some(r) = a.checked_add(*b) {
                return Coef::S(r);
            }
        }
        Coef::from_big(self.to_big() + o.to_big())
    }

    /// `self - o` for `self >= o`.
    fn sub(&self, o: &Coef) -> Coef {
        if let (Coef::S(a), Coef::S(b)) = (self, o) {
            return Coef::S(a - b);
        }
        Coef::from_big(self.to_big() - o.to_big())
    }

    fn mul(&self, o: &Coef) -> Option<Coef> {
        if let (Coef::S(a), Coef::S(b)) = (self, o) {
            if let (Ok(x), Ok(y)) = (u64::try_from(*a), u64::try_from(*b)) {
                return Some(Coef::S(u128::from(x) * u128::from(y)));
            }
            if let Some(r) = a.checked_mul(*b) {
                return Some(Coef::S(r));
            }
        }
        if self.ndigits() + o.ndigits() > MAX_DIGITS {
            return None;
        }
        Some(Coef::from_big(self.to_big() * o.to_big()))
    }

    /// `divmod(self, o)` for `o != 0`.
    fn divrem(&self, o: &Coef) -> (Coef, Coef) {
        if let (Coef::S(a), Coef::S(b)) = (self, o) {
            if let (Ok(x), Ok(y)) = (u64::try_from(*a), u64::try_from(*b)) {
                return (Coef::S(u128::from(x / y)), Coef::S(u128::from(x % y)));
            }
            return (Coef::S(a / b), Coef::S(a % b));
        }
        let (q, r) = self.to_big().div_rem(&o.to_big());
        (Coef::from_big(q), Coef::from_big(r))
    }

    fn cmp(&self, o: &Coef) -> Ordering {
        match (self, o) {
            (Coef::S(a), Coef::S(b)) => a.cmp(b),
            (Coef::S(_), Coef::B(_)) => Ordering::Less,
            (Coef::B(_), Coef::S(_)) => Ordering::Greater,
            (Coef::B(a), Coef::B(b)) => a.cmp(b),
        }
    }

    /// `self % m` for a small `m`.
    #[inline]
    fn rem_small(&self, m: u64) -> u64 {
        match self {
            Coef::S(v) => match u64::try_from(*v) {
                Ok(w) => w % m,
                Err(_) => (v % u128::from(m)) as u64,
            },
            Coef::B(b) => (b % m).to_u64().unwrap_or(0),
        }
    }

    fn incr(&self) -> Coef {
        self.add(&Coef::S(1))
    }

    /// `f` of the decimal digits (formatted on the stack while they fit).
    #[inline]
    fn with_digits<R>(&self, f: impl FnOnce(&str) -> R) -> R {
        match self {
            Coef::S(v) => {
                let mut buf = [0u8; 40];
                let i = format_u128(*v, &mut buf);
                // SAFETY: ASCII digits.
                f(unsafe { std::str::from_utf8_unchecked(&buf[i..]) })
            }
            Coef::B(b) => f(&b.to_str_radix(10)),
        }
    }

    fn to_digits(&self) -> String {
        self.with_digits(str::to_owned)
    }

    /// The value of the ASCII digits `d` (non-empty, at most
    /// [`MAX_DIGITS`] significant digits), or `None`.
    fn parse(d: &[u8]) -> Option<Coef> {
        let start = d.iter().position(|&c| c != b'0').unwrap_or(d.len());
        let d = &d[start..];
        if d.len() <= 38 {
            let mut v: u128 = 0;
            for &c in d {
                if !c.is_ascii_digit() {
                    return None;
                }
                v = v * 10 + u128::from(c - b'0');
            }
            return Some(Coef::S(v));
        }
        if d.len() as u64 > MAX_DIGITS || !d.iter().all(u8::is_ascii_digit) {
            return None;
        }
        BigUint::parse_bytes(d, 10).map(Coef::from_big)
    }

    fn to_object(&self) -> Object {
        match self {
            Coef::S(v) => match i64::try_from(*v) {
                Ok(i) => Object::Int(i),
                Err(_) => Object::int_from_bigint(BigInt::from(*v)),
            },
            Coef::B(b) => Object::int_from_bigint(BigInt::from_biguint(Sign::Plus, b.clone())),
        }
    }
}

// ---------------------------------------------------------------------
// Rounding.

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Round {
    Down,
    HalfUp,
    HalfEven,
    Ceiling,
    Floor,
    Up,
    HalfDown,
    R05Up,
}

const ROUND_NAMES: [(&str, Round); 8] = [
    ("ROUND_DOWN", Round::Down),
    ("ROUND_HALF_UP", Round::HalfUp),
    ("ROUND_HALF_EVEN", Round::HalfEven),
    ("ROUND_CEILING", Round::Ceiling),
    ("ROUND_FLOOR", Round::Floor),
    ("ROUND_UP", Round::Up),
    ("ROUND_HALF_DOWN", Round::HalfDown),
    ("ROUND_05UP", Round::R05Up),
];

fn round_of(o: &Object) -> Option<Round> {
    let Object::Str(s) = o else { return None };
    let s: &str = s.as_ref();
    ROUND_NAMES.iter().find(|(n, _)| *n == s).map(|(_, r)| *r)
}

impl State {
    /// [`round_of`], settled by identity for the module's own rounding
    /// constants (which a context's `rounding` normally is).
    #[inline]
    fn round(&self, o: &Object) -> Option<Round> {
        if let Object::Str(s) = o {
            for (r, (_, mode)) in self.roundings.iter().zip(ROUND_NAMES.iter()) {
                if SharedStr::ptr_eq(s, r) {
                    return Some(*mode);
                }
            }
        }
        round_of(o)
    }
}

/// How the discarded digits compare with half a unit of the last kept
/// digit.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Rem {
    Zero,
    Below,
    Half,
    Above,
}

/// `divmod(c, 10**drop)` (`drop >= 1`), the remainder classified.
#[inline]
fn split(c: &Coef, drop: u64) -> (Coef, Rem) {
    if let Coef::S(v) = *c {
        if drop >= 39 {
            // `v < 2**128 < 5 * 10**38`: below half a unit.
            return (Coef::ZERO, if v == 0 { Rem::Zero } else { Rem::Below });
        }
        let (q, r) = divrem_pow10_u128(v, drop);
        let rem = if r == 0 {
            Rem::Zero
        } else {
            match r.cmp(&(5 * POW10[drop as usize - 1])) {
                Ordering::Less => Rem::Below,
                Ordering::Equal => Rem::Half,
                Ordering::Greater => Rem::Above,
            }
        };
        return (Coef::S(q), rem);
    }
    let (q, r) = c.divrem_pow10(drop);
    let rem = if r.is_zero() {
        Rem::Zero
    } else {
        let half = match Coef::S(5).mul_pow10(drop - 1) {
            Some(h) => h,
            None => return (q, Rem::Below),
        };
        match r.cmp(&half) {
            Ordering::Less => Rem::Below,
            Ordering::Equal => Rem::Half,
            Ordering::Greater => Rem::Above,
        }
    };
    (q, rem)
}

/// The `_round_*` functions' verdict: `1` round away from zero, `0` the
/// discarded digits were zero, `-1` truncate an inexact value. `q` is
/// the kept part (its last digit decides the parity cases; `0` stands
/// for an empty kept part, as the Python functions treat it).
fn round_dir(mode: Round, sign: u8, q: &Coef, rem: Rem) -> i8 {
    let down = if rem == Rem::Zero { 0 } else { -1 };
    let half_up = match rem {
        Rem::Half | Rem::Above => 1,
        Rem::Zero => 0,
        Rem::Below => -1,
    };
    match mode {
        Round::Down => down,
        Round::Up => -down,
        Round::HalfUp => half_up,
        Round::HalfDown => {
            if rem == Rem::Half {
                -1
            } else {
                half_up
            }
        }
        Round::HalfEven => {
            if rem == Rem::Half && q.rem_small(2) == 0 {
                -1
            } else {
                half_up
            }
        }
        Round::Ceiling => {
            if sign == 1 {
                down
            } else {
                -down
            }
        }
        Round::Floor => {
            if sign == 0 {
                down
            } else {
                -down
            }
        }
        Round::R05Up => {
            let last = q.rem_small(10);
            if last != 0 && last != 5 {
                down
            } else {
                -down
            }
        }
    }
}

// ---------------------------------------------------------------------
// Finite values and the context parameters.

/// A finite decimal: `(-1)**sign * coef * 10**exp`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Fin {
    sign: u8,
    coef: Coef,
    exp: i64,
}

impl Fin {
    #[inline]
    fn new(sign: u8, coef: Coef, exp: i128) -> Option<Fin> {
        Some(Fin {
            sign,
            coef,
            exp: i64::try_from(exp).ok()?,
        })
    }

    #[inline]
    fn is_zero(&self) -> bool {
        self.coef.is_zero()
    }

    fn adjusted(&self) -> i128 {
        self.exp as i128 + self.coef.ndigits() as i128 - 1
    }

    fn negated(&self) -> Fin {
        Fin {
            sign: self.sign ^ 1,
            coef: self.coef.clone(),
            exp: self.exp,
        }
    }

    fn abs(&self) -> Fin {
        Fin {
            sign: 0,
            coef: self.coef.clone(),
            exp: self.exp,
        }
    }
}

/// The numeric parameters of a context.
#[derive(Clone, Copy, Debug)]
struct CtxP {
    prec: i128,
    emin: i128,
    emax: i128,
    round: Round,
    clamp: bool,
}

impl CtxP {
    fn etiny(&self) -> i128 {
        self.emin - self.prec + 1
    }

    fn etop(&self) -> i128 {
        self.emax - self.prec + 1
    }
}

// The signals, as bits in `_signals` order.
const S_CLAMPED: u16 = 1 << 0;
const S_DIVZERO: u16 = 1 << 1;
const S_INEXACT: u16 = 1 << 2;
const S_OVERFLOW: u16 = 1 << 3;
const S_ROUNDED: u16 = 1 << 4;
const S_UNDERFLOW: u16 = 1 << 5;
const S_INVALID: u16 = 1 << 6;
const S_SUBNORMAL: u16 = 1 << 7;
const S_FLOATOP: u16 = 1 << 8;
const N_SIGNALS: usize = 9;

// Silence "never read" for the signals only Python raises.
const _: u16 = S_DIVZERO | S_OVERFLOW | S_INVALID;

// ---------------------------------------------------------------------
// The algorithms (the Python methods', step for step). `None` means the
// native code does not serve the case (an overflow, a special result,
// a coefficient past `MAX_DIGITS`).

/// `Decimal._fix`.
fn fix(d: Fin, c: &CtxP, sig: &mut u16) -> Option<Fin> {
    let (prec, etiny, etop) = (c.prec, c.etiny(), c.etop());
    let exp = d.exp as i128;
    if d.coef.is_zero() {
        let exp_max = if c.clamp { etop } else { c.emax };
        let new_exp = exp.max(etiny).min(exp_max);
        if new_exp != exp {
            *sig |= S_CLAMPED;
            return Fin::new(d.sign, Coef::ZERO, new_exp);
        }
        return Some(d);
    }
    let len = d.coef.ndigits() as i128;
    let mut exp_min = len + exp - prec;
    if exp_min > etop {
        return None;
    }
    let subnormal = exp_min < etiny;
    if subnormal {
        exp_min = etiny;
    }
    if exp < exp_min {
        let digits = len + exp - exp_min;
        let (q, rem) = if digits < 0 {
            (Coef::ZERO, Rem::Below)
        } else {
            split(&d.coef, (len - digits) as u64)
        };
        let changed = round_dir(c.round, d.sign, &q, rem);
        let mut coeff = q;
        if changed > 0 {
            coeff = coeff.incr();
            if coeff.ndigits() as i128 > prec {
                coeff = coeff.divrem_pow10(1).0;
                exp_min += 1;
            }
        }
        if exp_min > etop {
            return None;
        }
        let ans = Fin::new(d.sign, coeff, exp_min)?;
        if changed != 0 && subnormal {
            *sig |= S_UNDERFLOW;
        }
        if subnormal {
            *sig |= S_SUBNORMAL;
        }
        if changed != 0 {
            *sig |= S_INEXACT;
        }
        *sig |= S_ROUNDED;
        if ans.is_zero() {
            *sig |= S_CLAMPED;
        }
        return Some(ans);
    }
    if subnormal {
        *sig |= S_SUBNORMAL;
    }
    if c.clamp && exp > etop {
        *sig |= S_CLAMPED;
        let coef = d.coef.mul_pow10((exp - etop) as u64)?;
        return Fin::new(d.sign, coef, etop);
    }
    Some(d)
}

/// `Decimal._rescale` (quiet).
#[inline]
fn rescale(d: &Fin, exp: i128, mode: Round) -> Option<Fin> {
    Some(rescale_inexact(d, exp, mode)?.0)
}

/// [`rescale`], and whether the value changed (digits that were not all
/// zeros were discarded).
fn rescale_inexact(d: &Fin, exp: i128, mode: Round) -> Option<(Fin, bool)> {
    if d.is_zero() {
        return Some((Fin::new(d.sign, Coef::ZERO, exp)?, false));
    }
    let dexp = d.exp as i128;
    if dexp >= exp {
        let k = u64::try_from(dexp - exp).ok()?;
        return Some((Fin::new(d.sign, d.coef.mul_pow10(k)?, exp)?, false));
    }
    let len = d.coef.ndigits() as i128;
    let digits = len + dexp - exp;
    let (q, rem) = if digits < 0 {
        (Coef::ZERO, Rem::Below)
    } else {
        split(&d.coef, (len - digits) as u64)
    };
    let changed = round_dir(mode, d.sign, &q, rem);
    let coeff = if changed == 1 { q.incr() } else { q };
    Some((Fin::new(d.sign, coeff, exp)?, rem != Rem::Zero))
}

/// `Decimal._cmp` for finite values.
#[inline]
fn cmp(a: &Fin, b: &Fin) -> Option<Ordering> {
    // Nonzero word-sized coefficients of one sign: the adjusted exponents,
    // then (equal, so the exponents are within 19 of each other) the
    // aligned coefficients.
    if let (Coef::S(x), Coef::S(y)) = (&a.coef, &b.coef) {
        if let (Ok(x), Ok(y)) = (u64::try_from(*x), u64::try_from(*y)) {
            if x != 0 && y != 0 && a.sign == b.sign {
                let aa = a.exp as i128 + digits_u128(u128::from(x)) as i128;
                let ba = b.exp as i128 + digits_u128(u128::from(y)) as i128;
                let mag = if aa != ba {
                    aa.cmp(&ba)
                } else {
                    let d = a.exp as i128 - b.exp as i128;
                    let (px, py) = if d >= 0 {
                        (POW10[d as usize] as u64, 1)
                    } else {
                        (1, POW10[(-d) as usize] as u64)
                    };
                    (u128::from(x) * u128::from(px)).cmp(&(u128::from(y) * u128::from(py)))
                };
                return Some(if a.sign == 1 { mag.reverse() } else { mag });
            }
        }
    }
    cmp_general(a, b)
}

/// [`cmp`] for every finite pair.
fn cmp_general(a: &Fin, b: &Fin) -> Option<Ordering> {
    let neg = |s: u8| {
        if s == 1 {
            Ordering::Less
        } else {
            Ordering::Greater
        }
    };
    if a.is_zero() {
        if b.is_zero() {
            return Some(Ordering::Equal);
        }
        return Some(neg(b.sign).reverse());
    }
    if b.is_zero() {
        return Some(neg(a.sign));
    }
    if b.sign < a.sign {
        return Some(Ordering::Less);
    }
    if a.sign < b.sign {
        return Some(Ordering::Greater);
    }
    let (aa, ba) = (a.adjusted(), b.adjusted());
    let mag = if aa == ba {
        let (ae, be) = (a.exp as i128, b.exp as i128);
        let ap = a.coef.mul_pow10(u64::try_from((ae - be).max(0)).ok()?)?;
        let bp = b.coef.mul_pow10(u64::try_from((be - ae).max(0)).ok()?)?;
        ap.cmp(&bp)
    } else {
        aa.cmp(&ba)
    };
    Some(if a.sign == 1 { mag.reverse() } else { mag })
}

/// `_WorkRep`: the operands `__add__` aligns.
struct Work {
    sign: u8,
    int: Coef,
    exp: i128,
}

/// `_normalize(op1, op2, prec)`.
fn normalize(op1: &mut Work, op2: &mut Work, prec: i128) -> Option<()> {
    let (tmp, other) = if op1.exp < op2.exp {
        (op2, op1)
    } else {
        (op1, op2)
    };
    let tmp_len = tmp.int.ndigits() as i128;
    let other_len = other.int.ndigits() as i128;
    let exp = tmp.exp + (-1i128).min(tmp_len - prec - 2);
    if other_len + other.exp - 1 < exp {
        other.int = Coef::S(1);
        other.exp = exp;
    }
    tmp.int = tmp
        .int
        .mul_pow10(u64::try_from(tmp.exp - other.exp).ok()?)?;
    tmp.exp = other.exp;
    Some(())
}

/// `Decimal.__add__` for finite operands.
fn add(a: &Fin, b: &Fin, c: &CtxP, sig: &mut u16) -> Option<Fin> {
    let exp = (a.exp.min(b.exp)) as i128;
    let negativezero = c.round == Round::Floor && a.sign != b.sign;
    if a.is_zero() && b.is_zero() {
        let sign = if negativezero { 1 } else { a.sign.min(b.sign) };
        return fix(Fin::new(sign, Coef::ZERO, exp)?, c, sig);
    }
    if a.is_zero() {
        let e = exp.max(b.exp as i128 - c.prec - 1);
        return fix(rescale(b, e, c.round)?, c, sig);
    }
    if b.is_zero() {
        let e = exp.max(a.exp as i128 - c.prec - 1);
        return fix(rescale(a, e, c.round)?, c, sig);
    }
    // Word-sized coefficients a few digits apart: the exact sum, which
    // rounds as `_normalize`'s shortened operand does (that only drops
    // digits below the rounding position).
    if let (Coef::S(x), Coef::S(y)) = (&a.coef, &b.coef) {
        let (da, db) = (a.exp as i128 - exp, b.exp as i128 - exp);
        if *x <= u128::from(u64::MAX) && *y <= u128::from(u64::MAX) && da < 20 && db < 20 {
            // Both factors are below 2**64: one widening multiply each.
            let (x, y) = (
                u128::from(*x as u64) * u128::from(POW10[da as usize] as u64),
                u128::from(*y as u64) * u128::from(POW10[db as usize] as u64),
            );
            let (sign, int) = if a.sign == b.sign {
                match x.checked_add(y) {
                    Some(s) => (a.sign, s),
                    None => return add_general(a, b, exp, negativezero, c, sig),
                }
            } else {
                match x.cmp(&y) {
                    Ordering::Equal => (u8::from(negativezero), 0),
                    Ordering::Greater => (a.sign, x - y),
                    Ordering::Less => (b.sign, y - x),
                }
            };
            return fix(Fin::new(sign, Coef::S(int), exp)?, c, sig);
        }
    }
    add_general(a, b, exp, negativezero, c, sig)
}

/// [`add`] of nonzero operands through `_normalize`.
fn add_general(
    a: &Fin,
    b: &Fin,
    exp: i128,
    negativezero: bool,
    c: &CtxP,
    sig: &mut u16,
) -> Option<Fin> {
    let mut op1 = Work {
        sign: a.sign,
        int: a.coef.clone(),
        exp: a.exp as i128,
    };
    let mut op2 = Work {
        sign: b.sign,
        int: b.coef.clone(),
        exp: b.exp as i128,
    };
    normalize(&mut op1, &mut op2, c.prec)?;
    let (sign, int) = if op1.sign != op2.sign {
        match op1.int.cmp(&op2.int) {
            Ordering::Equal => {
                let s = u8::from(negativezero);
                return fix(Fin::new(s, Coef::ZERO, exp)?, c, sig);
            }
            Ordering::Less => (op2.sign, op2.int.sub(&op1.int)),
            Ordering::Greater => (op1.sign, op1.int.sub(&op2.int)),
        }
    } else {
        (op1.sign, op1.int.add(&op2.int))
    };
    fix(Fin::new(sign, int, op1.exp)?, c, sig)
}

/// `Decimal.__mul__` for finite operands.
fn mul(a: &Fin, b: &Fin, c: &CtxP, sig: &mut u16) -> Option<Fin> {
    let sign = a.sign ^ b.sign;
    let exp = a.exp as i128 + b.exp as i128;
    if a.is_zero() || b.is_zero() {
        return fix(Fin::new(sign, Coef::ZERO, exp)?, c, sig);
    }
    let coef = if a.coef.is_one() {
        b.coef.clone()
    } else if b.coef.is_one() {
        a.coef.clone()
    } else {
        a.coef.mul(&b.coef)?
    };
    fix(Fin::new(sign, coef, exp)?, c, sig)
}

/// `Decimal.__truediv__` for finite operands and a nonzero divisor.
fn div(a: &Fin, b: &Fin, c: &CtxP, sig: &mut u16) -> Option<Fin> {
    if b.is_zero() {
        return None;
    }
    let sign = a.sign ^ b.sign;
    let (coeff, exp) = if a.is_zero() {
        (Coef::ZERO, a.exp as i128 - b.exp as i128)
    } else {
        let shift = b.coef.ndigits() as i128 - a.coef.ndigits() as i128 + c.prec + 1;
        let mut exp = a.exp as i128 - b.exp as i128 - shift;
        let (mut coeff, rem) = if shift >= 0 {
            a.coef
                .mul_pow10(u64::try_from(shift).ok()?)?
                .divrem(&b.coef)
        } else {
            a.coef
                .divrem(&b.coef.mul_pow10(u64::try_from(-shift).ok()?)?)
        };
        if !rem.is_zero() {
            if coeff.rem_small(5) == 0 {
                coeff = coeff.incr();
            }
        } else {
            let ideal = a.exp as i128 - b.exp as i128;
            while exp < ideal && coeff.rem_small(10) == 0 {
                coeff = coeff.divrem_pow10(1).0;
                exp += 1;
            }
        }
        (coeff, exp)
    };
    fix(Fin::new(sign, coeff, exp)?, c, sig)
}

/// `Decimal._divide`: `(self // other, self % other)` unrounded, for a
/// finite dividend and a finite nonzero divisor; `None` for the
/// `DivisionImpossible` case.
fn divide(a: &Fin, b: &Fin, c: &CtxP) -> Option<(Fin, Fin)> {
    if b.is_zero() {
        return None;
    }
    let sign = a.sign ^ b.sign;
    let ideal_exp = a.exp.min(b.exp) as i128;
    let expdiff = a.adjusted() - b.adjusted();
    if a.is_zero() || expdiff <= -2 {
        return Some((
            Fin::new(sign, Coef::ZERO, 0)?,
            rescale(a, ideal_exp, c.round)?,
        ));
    }
    if expdiff <= c.prec {
        let (mut i1, mut i2) = (a.coef.clone(), b.coef.clone());
        let (e1, e2) = (a.exp as i128, b.exp as i128);
        if e1 >= e2 {
            i1 = i1.mul_pow10(u64::try_from(e1 - e2).ok()?)?;
        } else {
            i2 = i2.mul_pow10(u64::try_from(e2 - e1).ok()?)?;
        }
        let (q, r) = i1.divrem(&i2);
        if (q.ndigits() as i128) <= c.prec || q.is_zero() {
            return Some((Fin::new(sign, q, 0)?, Fin::new(a.sign, r, ideal_exp)?));
        }
    }
    None
}

/// `Decimal.quantize` for finite operands with a valid rounding.
fn quantize(a: &Fin, e: i64, mode: Round, c: &CtxP, sig: &mut u16) -> Option<Fin> {
    let e = e as i128;
    if !(c.etiny() <= e && e <= c.emax) {
        return None;
    }
    if a.is_zero() {
        return fix(Fin::new(a.sign, Coef::ZERO, e)?, c, sig);
    }
    let adj = a.adjusted();
    if adj > c.emax || adj - e + 1 > c.prec {
        return None;
    }
    let (ans, inexact) = rescale_inexact(a, e, mode)?;
    let ans_adj = ans.adjusted();
    if ans_adj > c.emax || ans.coef.ndigits() as i128 > c.prec {
        return None;
    }
    if !ans.is_zero() && ans_adj < c.emin {
        *sig |= S_SUBNORMAL;
    }
    if ans.exp > a.exp {
        // `ans != self`: digits that were not all zeros went.
        if inexact {
            *sig |= S_INEXACT;
        }
        *sig |= S_ROUNDED;
    }
    fix(ans, c, sig)
}

/// `Decimal.__hash__` for a finite value.
fn hash(d: &Fin) -> i64 {
    const M: u64 = (1 << 61) - 1;
    // The inverse of 10 modulo M (`pow(10, M - 2, M)`).
    const INV10: u64 = 2_075_258_708_292_324_556;
    #[inline]
    fn reduce(x: u128) -> u64 {
        // x mod (2**61 - 1): two folds leave less than 2 * M.
        let m = u128::from(M);
        let x = (x & m) + (x >> 61);
        let x = ((x & m) + (x >> 61)) as u64;
        if x >= M {
            x - M
        } else {
            x
        }
    }
    #[inline]
    fn mulmod(a: u64, b: u64) -> u64 {
        reduce(u128::from(a) * u128::from(b))
    }
    fn powmod(mut b: u64, mut e: u64) -> u64 {
        let mut r = 1u64;
        while e > 0 {
            if e & 1 == 1 {
                r = mulmod(r, b);
            }
            b = mulmod(b, b);
            e >>= 1;
        }
        r
    }
    let exp_hash = if d.exp >= 0 {
        powmod(10, d.exp as u64)
    } else {
        powmod(INV10, d.exp.unsigned_abs())
    };
    let c = match &d.coef {
        Coef::S(v) => reduce(*v),
        Coef::B(b) => (b % BigUint::from(M)).to_u64().unwrap_or(0),
    };
    let h = mulmod(c, exp_hash) as i64;
    let ans = if d.sign == 1 && !d.is_zero() { -h } else { h };
    if ans == -1 {
        -2
    } else {
        ans
    }
}

/// `Decimal.__str__` for a finite value.
fn to_sci(d: &Fin, eng: bool, capitals: bool) -> String {
    let digits = d.coef.to_digits();
    let len = digits.len() as i128;
    let exp = d.exp as i128;
    let leftdigits = exp + len;
    let dotplace = if exp <= 0 && leftdigits > -6 {
        leftdigits
    } else if !eng {
        1
    } else if d.coef.is_zero() {
        (leftdigits + 1).rem_euclid(3) - 1
    } else {
        (leftdigits - 1).rem_euclid(3) + 1
    };
    let mut out = String::with_capacity(digits.len() + 8);
    if d.sign == 1 {
        out.push('-');
    }
    if dotplace <= 0 {
        out.push_str("0.");
        for _ in 0..-dotplace {
            out.push('0');
        }
        out.push_str(&digits);
    } else if dotplace >= len {
        out.push_str(&digits);
        for _ in 0..dotplace - len {
            out.push('0');
        }
    } else {
        out.push_str(&digits[..dotplace as usize]);
        out.push('.');
        out.push_str(&digits[dotplace as usize..]);
    }
    if leftdigits != dotplace {
        out.push(if capitals { 'E' } else { 'e' });
        let e = leftdigits - dotplace;
        if e >= 0 {
            out.push('+');
        }
        out.push_str(&e.to_string());
    }
    out
}

/// The exact value of a finite `float` (`Decimal.from_float`).
fn from_f64(f: f64) -> Option<Fin> {
    if !f.is_finite() {
        return None;
    }
    let sign = u8::from(f.is_sign_negative());
    if f == 0.0 {
        return Some(Fin {
            sign,
            coef: Coef::ZERO,
            exp: 0,
        });
    }
    let bits = f.to_bits();
    let bexp = ((bits >> 52) & 0x7ff) as i64;
    let frac = bits & ((1 << 52) - 1);
    let (mut mant, mut e) = if bexp == 0 {
        (frac, -1074)
    } else {
        (frac | (1 << 52), bexp - 1075)
    };
    if e >= 0 {
        let coef = Coef::from_big(BigUint::from(mant) << (e as usize));
        return Some(Fin { sign, coef, exp: 0 });
    }
    let tz = (mant.trailing_zeros() as i64).min(-e);
    mant >>= tz;
    e += tz;
    let k = (-e) as u32;
    let coef = if k <= 55 {
        // 5**55 < 2**128 / 2**53.
        match 5u128
            .checked_pow(k)
            .and_then(|p| p.checked_mul(u128::from(mant)))
        {
            Some(v) => Coef::S(v),
            None => Coef::from_big(
                BigUint::from(mant) * num_traits::pow(BigUint::from(5u32), k as usize),
            ),
        }
    } else {
        Coef::from_big(BigUint::from(mant) * num_traits::pow(BigUint::from(5u32), k as usize))
    };
    Some(Fin {
        sign,
        coef,
        exp: -i64::from(k),
    })
}

/// A numeric string's finite value, as the constructor's regular
/// expression reads it, for an ASCII string with no underscores (after
/// `strip()` when `strip`); `None` for anything else (specials, other
/// digits, malformed text, huge exponents).
fn parse_str(s: &str, strip: bool) -> Option<Fin> {
    let mut b = s.as_bytes();
    if !b.is_ascii() || b.contains(&b'_') {
        return None;
    }
    if strip {
        // `str.strip()`'s ASCII whitespace.
        let ws = |c: &u8| matches!(c, b' ' | b'\t'..=b'\r' | 0x1c..=0x1f);
        while let Some((c, rest)) = b.split_first() {
            if !ws(c) {
                break;
            }
            b = rest;
        }
        while let Some((c, rest)) = b.split_last() {
            if !ws(c) {
                break;
            }
            b = rest;
        }
    }
    let mut i = 0;
    let mut sign = 0u8;
    match b.first() {
        Some(b'-') => {
            sign = 1;
            i = 1;
        }
        Some(b'+') => i = 1,
        _ => {}
    }
    let int_start = i;
    while i < b.len() && b[i].is_ascii_digit() {
        i += 1;
    }
    let int_part = &b[int_start..i];
    let mut frac_part: &[u8] = &[];
    if i < b.len() && b[i] == b'.' {
        i += 1;
        let fs = i;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        frac_part = &b[fs..i];
    }
    if int_part.is_empty() && frac_part.is_empty() {
        return None;
    }
    let mut exp: i128 = 0;
    if i < b.len() && (b[i] == b'e' || b[i] == b'E') {
        i += 1;
        let mut eneg = false;
        match b.get(i) {
            Some(b'-') => {
                eneg = true;
                i += 1;
            }
            Some(b'+') => i += 1,
            _ => {}
        }
        let es = i;
        while i < b.len() && b[i].is_ascii_digit() {
            if exp > 1_000_000_000_000_000_000_000 {
                return None;
            }
            exp = exp * 10 + i128::from(b[i] - b'0');
            i += 1;
        }
        if i == es {
            return None;
        }
        if eneg {
            exp = -exp;
        }
    }
    if i != b.len() {
        return None;
    }
    let coef = if frac_part.is_empty() {
        Coef::parse(int_part)?
    } else if int_part.is_empty() {
        Coef::parse(frac_part)?
    } else {
        let mut all = Vec::with_capacity(int_part.len() + frac_part.len());
        all.extend_from_slice(int_part);
        all.extend_from_slice(frac_part);
        Coef::parse(&all)?
    };
    Fin::new(sign, coef, exp - frac_part.len() as i128)
}

// ---------------------------------------------------------------------
// The per-class-set state.

/// Interned slot and attribute names.
struct Names {
    sign: SharedStr,
    int: SharedStr,
    exp: SharedStr,
    special: SharedStr,
    prec: SharedStr,
    rounding: SharedStr,
    emin: SharedStr,
    emax: SharedStr,
    capitals: SharedStr,
    clamp: SharedStr,
    ignored: SharedStr,
    traps: SharedStr,
    flags: SharedStr,
    owner: SharedStr,
    data: SharedStr,
}

fn interned(s: &str) -> SharedStr {
    match crate::stdlib::sys::intern_name(s) {
        Object::Str(s) => s,
        _ => SharedStr::from(s),
    }
}

impl Names {
    fn new() -> Self {
        Self {
            sign: interned("_sign"),
            int: interned("_int"),
            exp: interned("_exp"),
            special: interned("_is_special"),
            prec: interned("prec"),
            rounding: interned("rounding"),
            emin: interned("Emin"),
            emax: interned("Emax"),
            capitals: interned("capitals"),
            clamp: interned("clamp"),
            ignored: interned("_ignored_flags"),
            traps: interned("traps"),
            flags: interned("flags"),
            owner: interned("_owner"),
            data: interned("_data"),
        }
    }
}

/// One `_decimal` module's classes (held weakly: the natives live in
/// their dicts) and the Python methods the natives replaced.
struct State {
    decimal: Weak<TypeObject>,
    context: Weak<TypeObject>,
    signal_dict: Weak<TypeObject>,
    decimal_tuple: Weak<TypeObject>,
    /// The signal classes, in `_signals` order.
    signals: Vec<Object>,
    /// `_decimal._current_context_var`.
    var: Object,
    /// `contextvars._STATES` (thread ident → current `contextvars.Context`).
    states: Object,
    /// The rounding constants, in [`ROUND_NAMES`] order.
    roundings: Vec<SharedStr>,
    names: Names,
    /// The slot layout natively built values share: `_dec_from_triple`'s
    /// assignment order.
    layout: SharedSlice<DictKey>,
    /// `(class, name)` → the replaced Python implementation.
    orig: HashMap<(u8, &'static str), Object>,
    /// The `attr_version` of `Decimal` after [`install`] (the class is
    /// immutable; a change turns the dispatch-loop shortcuts off).
    sealed: u64,
    /// `(signal dict, context)` pairs whose `_owner` reference was seen to
    /// name that context (see [`State::owned_by`]).
    owners: RefCell<Vec<(Weak<PyInstance>, Weak<PyInstance>)>>,
    /// Dead packed instances kept for reuse (see [`recycle`]).
    pool: Pool,
    /// The last context whose mappings [`State::signal_dicts`] validated,
    /// with its `traps` and `flags` (a `SignalDict`'s owner never
    /// changes, and its mapping is fixed at creation).
    checked: RefCell<Option<(Weak<PyInstance>, Weak<PyInstance>, Weak<PyInstance>)>>,
    /// Where the current thread's entry sat in `_STATES`, and the
    /// context variable's in a `contextvars.Context`'s mapping, last time
    /// (hints; see [`State::current`]).
    states_hint: AtomicUsize,
    data_hint: AtomicUsize,
    /// The `Context` class's shared attribute names, once proved to start
    /// with `Context.__init__`'s (see [`State::ctx_fields`]).
    ctx_keys: AtomicUsize,
    /// Advanced after every `Context` attribute store (the native
    /// `_context_changed`, which `Context.__setattr__` calls).
    ctx_epoch: AtomicU64,
    /// The last context read (see [`State::read_ctx`]).
    ctx_cache: RefCell<Option<CtxCache>>,
    /// The native `Decimal` methods (by address) → their [`SPECS`] index,
    /// for keyword calls from the dispatch loop (see [`method_kw`]).
    methods: HashMap<usize, usize>,
}

const POOL_CAP: usize = 64;

/// Dead natively packed `Decimal`s, kept with their class and storage
/// for the next value (CPython's `_decimal` keeps a freelist too). A
/// pooled instance keeps its class, so the pool (in the class's
/// [`State`]) and the class keep each other alive, as CPython's static
/// types live for the whole process.
#[derive(Default)]
struct Pool(std::cell::UnsafeCell<Vec<Rc<PyInstance>>>);

// SAFETY: the pool is touched only while `datetime_native::pool_ok()`:
// then every thread that reaches VM objects holds the GIL, which
// serializes the accesses (each is a push or a pop that runs no other
// code).
unsafe impl Sync for Pool {}
unsafe impl Send for Pool {}

impl Pool {
    #[inline(always)]
    fn pop(&self) -> Option<Rc<PyInstance>> {
        if !crate::stdlib::datetime_native::pool_ok() {
            return None;
        }
        // SAFETY: see the `Sync` impl.
        unsafe { (*self.0.get()).pop() }
    }

    /// Keep `inst`, or hand it back when the pool is full.
    #[inline(always)]
    fn push(&self, inst: Rc<PyInstance>) -> Result<(), Rc<PyInstance>> {
        // SAFETY: see the `Sync` impl.
        let v = unsafe { &mut *self.0.get() };
        if v.len() >= POOL_CAP {
            return Err(inst);
        }
        v.push(inst);
        Ok(())
    }
}

/// Retire a dying packed `Decimal` into its class's pool. `Err` hands back
/// an instance that must be freed normally (dropped as an `Arc`, not
/// through the finalizing `Rc` drop that may have called here).
pub(crate) fn recycle(mut inst: Rc<PyInstance>) -> Result<(), Rc<PyInstance>> {
    // While the refcount bias holds, this thread owns every count.
    let unique = if crate::rc::refcounts_biased() {
        Rc::strong_count(&inst) == 1 && Rc::weak_count(&inst) == 0
    } else {
        Rc::get_mut(&mut inst).is_some()
    };
    if !unique {
        return Err(inst);
    }
    // SAFETY: `inst` is the only reference (checked above).
    let m = unsafe { &mut *Rc::as_ptr(&inst).cast_mut() };
    let cls = m.class.get_mut();
    if cls.native_kind.get() != KIND_DECIMAL || cls.instances_need_finalize() {
        return Err(inst);
    }
    let Some(st) = state_of_cls(cls) else {
        return Err(inst);
    };
    if !std::ptr::eq(&**cls, st.decimal.as_ptr())
        || m.dict.published().is_some()
        || !m.dict.split_mut().is_empty()
        || m.native.get().is_some()
        || m.c_body.get() != 0
        || m.finalize_ran.get()
    {
        return Err(inst);
    }
    match m.slots.get_mut().as_packed_mut() {
        Some(p) if p.decimal().is_some() => p.clear_decimal(),
        _ => return Err(inst),
    }
    m.hash_cache = crate::sync::CachedHash::new(None);
    // SAFETY: `st` lives in the class `inst` holds.
    let st: &State = unsafe { &*std::ptr::from_ref(st) };
    st.pool.push(inst)
}

/// A `str` of `v`'s decimal digits (a packed `Decimal`'s `_int`).
pub(crate) fn digits_object(v: u128) -> Object {
    Object::Str(Coef::S(v).with_digits(SharedStr::from_ascii))
}

const OWNER_CACHE: usize = 16;

const C_DECIMAL: u8 = 0;
const C_CONTEXT: u8 = 1;

/// The [`State`] a class marked by [`install`] carries in its
/// `native_ext` slot.
#[inline]
#[allow(clippy::cast_ptr_alignment)]
fn state_of_cls(cls: &TypeObject) -> Option<&State> {
    // Only `datetime_native` (whose classes all carry a `datetime` kind)
    // and [`install`] (for `Decimal`, kind [`KIND_DECIMAL`], and
    // `Context`, kind 0) fill `native_ext`.
    if !matches!(cls.native_kind.get(), 0 | KIND_DECIMAL) {
        return None;
    }
    let ext = cls.native_ext.get()?;
    debug_assert!(ext.downcast_ref::<State>().is_some());
    // SAFETY: as above, the payload is a `State` (checked in debug
    // builds), so the pointer is a `State`'s, aligned for it; the cast
    // drops the vtable.
    Some(unsafe { &*Rc::as_ptr(ext).cast::<State>() })
}

#[inline]
fn state_of(o: &Object) -> Option<&State> {
    match o {
        Object::Instance(i) => state_of_cls(i.cls_raw()),
        _ => None,
    }
}

/// The instance's slot storage for a read (no Python code may run while
/// `f` reads).
#[inline(always)]
fn with_slots<R>(i: &PyInstance, f: impl FnOnce(&SlotStorage) -> Option<R>) -> Option<R> {
    // SAFETY: the view is dropped when `f` returns, and `f` runs no code.
    if let Some(s) = unsafe { i.slots.peek() } {
        return f(s);
    }
    let s = i.slots.try_borrow().ok()?;
    f(&s)
}

impl State {
    #[inline]
    fn is_decimal(&self, i: &PyInstance) -> bool {
        std::ptr::eq(i.cls_raw(), self.decimal.as_ptr())
    }

    /// The four slot values of an exact `Decimal` (`sign, int, exp,
    /// special`), cloned.
    fn slots_of(&self, i: &PyInstance) -> Option<[Object; 4]> {
        with_slots(i, |s| {
            if let Some(v) = s.values_for_layout(&self.layout) {
                return Some([v[0].clone(), v[1].clone(), v[2].clone(), v[3].clone()]);
            }
            let n = &self.names;
            Some([
                s.get_hinted(0, &n.sign)?.clone(),
                s.get_hinted(1, &n.int)?.clone(),
                s.get_hinted(2, &n.exp)?.clone(),
                s.get_hinted(3, &n.special)?.clone(),
            ])
        })
    }

    /// The finite value of an exact `Decimal`, or `None` (a special, a
    /// subclass, unusual slot values).
    #[inline]
    fn fin(&self, o: &Object) -> Option<Fin> {
        let Object::Instance(i) = o else { return None };
        if !self.is_decimal(i) {
            return None;
        }
        self.fin_of(i)
    }

    /// [`Self::fin`] of an instance known to be an exact `Decimal`.
    #[inline]
    fn fin_of(&self, i: &PyInstance) -> Option<Fin> {
        with_slots(i, |s| {
            if let Some((sign, coef, exp)) = s.as_packed().and_then(PackedSlots::decimal) {
                return Some(Fin {
                    sign,
                    coef: Coef::S(coef),
                    exp,
                });
            }
            self.fin_slots(s)
        })
    }

    /// [`Self::fin`] of slot values stored as objects.
    fn fin_slots(&self, s: &SlotStorage) -> Option<Fin> {
        {
            let (sign, int, exp, special) = match s.values_for_layout(&self.layout) {
                Some(v) => (&v[0], &v[1], &v[2], &v[3]),
                None => {
                    let n = &self.names;
                    (
                        s.get_hinted(0, &n.sign)?,
                        s.get_hinted(1, &n.int)?,
                        s.get_hinted(2, &n.exp)?,
                        s.get_hinted(3, &n.special)?,
                    )
                }
            };
            if !matches!(special, Object::Bool(false)) {
                return None;
            }
            // (A `bool` sign, which a tuple can give, stays visible in
            // `as_tuple()`: the Python code serves it.)
            let sign = match sign {
                Object::Int(v @ 0..=1) => *v as u8,
                _ => return None,
            };
            let Object::Int(exp) = *exp else { return None };
            let Object::Str(int) = int else { return None };
            let digits = int.as_bytes();
            if digits.is_empty() {
                return None;
            }
            Some(Fin {
                sign,
                coef: Coef::parse(digits)?,
                exp,
            })
        }
    }

    /// An operand `_convert_other` accepts natively: an exact finite
    /// `Decimal`, or an `int`.
    fn operand(&self, o: &Object) -> Option<Fin> {
        match o {
            Object::Instance(_) => self.fin(o),
            _ => int_fin(o),
        }
    }

    /// A new exact `Decimal` holding `d`: packed (from the pool when it
    /// has one) while the coefficient fits in a word.
    #[inline]
    fn make(&self, d: &Fin) -> Option<Object> {
        if let Coef::S(c) = d.coef {
            if let Some(r) = self.make_packed(d.sign, c, d.exp) {
                return Some(r);
            }
        }
        self.make_fixed(d)
    }

    /// [`Self::make`] of a value that packs (see `PackedSlots`), else
    /// `None`.
    #[inline]
    fn make_packed(&self, sign: u8, coef: u128, exp: i64) -> Option<Object> {
        let parts = PackedSlots::decimal_parts(sign, coef, exp)?;
        if let Some(inst) = self.pool.pop() {
            // SAFETY: pooled instances are unique (see `recycle`): nothing
            // else can observe the storage while it changes.
            let m = unsafe { &mut *Rc::as_ptr(&inst).cast_mut() };
            if let Some(p) = m.slots.get_mut().as_packed_mut() {
                p.set_decimal(parts);
                return Some(Object::Instance(inst));
            }
            drop(Rc::into_arc(inst));
        }
        let cls = self.decimal.upgrade()?;
        let mut i = PyInstance::new(cls);
        *i.slots.get_mut() = SlotStorage::from_packed(PackedSlots::new_decimal(parts));
        Some(Object::Instance(Rc::new(i)))
    }

    /// [`Self::make`] for a coefficient past a word: the slot objects.
    fn make_fixed(&self, d: &Fin) -> Option<Object> {
        let cls = self.decimal.upgrade()?;
        let digits = Object::Str(d.coef.with_digits(SharedStr::from_ascii));
        let mut i = PyInstance::new(cls);
        *i.slots.get_mut() = SlotStorage::from_layout(
            self.layout.clone(),
            vec![
                Object::Int(i64::from(d.sign)),
                digits,
                Object::Int(d.exp),
                Object::Bool(false),
            ],
        );
        Some(Object::Instance(Rc::new(i)))
    }

    /// A copy of the exact `Decimal` `i` (specials included) within the
    /// context-free constructor's exact range, or `None`.
    fn copy_of(&self, i: &PyInstance) -> Option<Object> {
        let packed = with_slots(i, |s| s.as_packed().and_then(PackedSlots::decimal));
        if let Some((sign, coef, exp)) = packed {
            if exp < MIN_ETINY as i64 || exp as i128 + digits_u128(coef) as i128 - 1 > MAX_EMAX {
                return None;
            }
            return self.make_packed(sign, coef, exp);
        }
        let v = self.slots_of(i)?;
        match (&v[0], &v[1], &v[2], &v[3]) {
            (_, _, _, Object::Bool(true)) => {}
            (Object::Int(0..=1), Object::Str(int), Object::Int(exp), Object::Bool(false)) => {
                let exp = *exp as i128;
                if exp + int.len() as i128 - 1 > MAX_EMAX || exp < MIN_ETINY {
                    return None;
                }
            }
            _ => return None,
        }
        self.make_raw(v)
    }

    /// A new exact `Decimal` with the slot values `v` (a copy).
    fn make_raw(&self, v: [Object; 4]) -> Option<Object> {
        let cls = self.decimal.upgrade()?;
        let mut i = PyInstance::new(cls);
        *i.slots.get_mut() = SlotStorage::from_layout(self.layout.clone(), v.to_vec());
        Some(Object::Instance(Rc::new(i)))
    }

    fn signal(&self, bit: usize) -> &Object {
        &self.signals[bit]
    }

    /// Whether `d` (an exact `SignalDict`) names `ctx` as its live owner
    /// (`SignalDict._check_valid`).
    fn owned_by(&self, d: &Rc<PyInstance>, ctx: &Rc<PyInstance>) -> bool {
        // SAFETY: a read that runs no code.
        if let Some(cache) = unsafe { self.owners.peek() } {
            // A live weak reference keeps its allocation, so an address
            // match names the same object.
            if cache.iter().any(|(dw, cw)| {
                std::ptr::eq(dw.as_ptr(), Rc::as_ptr(d))
                    && std::ptr::eq(cw.as_ptr(), Rc::as_ptr(ctx))
            }) {
                return true;
            }
        }
        self.owned_by_slow(d, ctx)
    }

    #[cold]
    #[inline(never)]
    fn owned_by_slow(&self, d: &Rc<PyInstance>, ctx: &Rc<PyInstance>) -> bool {
        let Some(owner) = d.attr_get_str(&self.names.owner) else {
            return false;
        };
        match crate::stdlib::weakref_real::c_referent(&owner) {
            Some(Some(Object::Instance(t))) if Rc::ptr_eq(&t, ctx) => {}
            _ => return false,
        }
        if let Ok(mut cache) = self.owners.try_borrow_mut() {
            cache.retain(|(dw, cw)| dw.strong_count() > 0 && cw.strong_count() > 0);
            if cache.len() >= OWNER_CACHE {
                cache.pop();
            }
            cache.insert(0, (Rc::downgrade(d), Rc::downgrade(ctx)));
        }
        true
    }

    /// The decimal context `getcontext()` would return, when it exists
    /// (an exact `Context`): `contextvars._STATES[get_ident()]._data[var]`.
    fn current(&self) -> Option<Rc<PyInstance>> {
        let Object::Dict(states) = &self.states else {
            return None;
        };
        let ident = crate::vm_singletons::current_worker_thread_id() as i64;
        // SAFETY: reads that run no code (the keys are an int and a plain
        // instance, which hash and compare natively); the context is
        // cloned before anything else runs.
        unsafe {
            let states = states.peek()?;
            // Each table's last position of the key is a hint, checked
            // against the key itself before the value is used.
            let hint = self.states_hint.load(Relaxed);
            let cv = match states.get_index(hint) {
                Some((DictKey(Object::Int(k)), v)) if *k == ident => v,
                _ => {
                    let i = states.get_index_of(&DictKey(Object::Int(ident)))?;
                    self.states_hint.store(i, Relaxed);
                    states.get_index(i)?.1
                }
            };
            let Object::Instance(cv) = cv else {
                return None;
            };
            let data = match cv.slots.peek()?.get_hinted(0, &self.names.data)? {
                Object::Dict(d) => d.peek()?,
                _ => return None,
            };
            let Object::Instance(var) = &self.var else {
                return None;
            };
            let hint = self.data_hint.load(Relaxed);
            let ctx = match data.get_index(hint) {
                Some((DictKey(Object::Instance(k)), v)) if Rc::ptr_eq(k, var) => v,
                _ => {
                    // A borrowed key: no reference count changes hands.
                    let key = std::mem::ManuallyDrop::new(DictKey(std::ptr::read(&self.var)));
                    let i = data.get_index_of(&*key)?;
                    self.data_hint.store(i, Relaxed);
                    data.get_index(i)?.1
                }
            };
            match ctx {
                Object::Instance(c) if std::ptr::eq(c.cls_raw(), self.context.as_ptr()) => {
                    Some(c.clone())
                }
                _ => None,
            }
        }
    }

    /// The context argument `context` resolves to (`_resolve_context`):
    /// the current one for `None` (or an omitted argument), an exact
    /// `Context`, or `None` for anything the Python code must handle.
    #[inline]
    fn resolve(&self, context: Option<&Object>) -> Option<Ctx> {
        let inst = match context {
            None | Some(Object::None) => self.current()?,
            Some(Object::Instance(c)) if std::ptr::eq(c.cls_raw(), self.context.as_ptr()) => {
                c.clone()
            }
            _ => return None,
        };
        self.read_ctx(inst)
    }

    /// A context's attributes, checked as `Context.__setattr__` leaves
    /// them.
    fn read_ctx(&self, inst: Rc<PyInstance>) -> Option<Ctx> {
        // SAFETY: a read that runs no code. The cached pointers name
        // objects the context's attributes hold, unchanged while the
        // epoch stands (`Context.__setattr__` advances it after every
        // store), and the context is alive (`inst`).
        if let Some(c) = unsafe { self.ctx_cache.peek() }.and_then(Option::as_ref) {
            if std::ptr::eq(c.ctx.as_ptr(), Rc::as_ptr(&inst))
                && c.epoch == self.ctx_epoch.load(Relaxed)
            {
                let ignored = unsafe { &*(c.ignored as *const RefCell<Vec<Object>>) };
                if !unsafe { ignored.peek() }?.is_empty() {
                    return None;
                }
                return Some(Ctx {
                    p: c.p,
                    capitals: c.capitals,
                    flags: c.flags as *const RefCell<DictData>,
                    traps: c.traps as *const RefCell<DictData>,
                    _inst: inst,
                });
            }
        }
        self.read_ctx_fields(inst)
    }

    #[inline(never)]
    fn read_ctx_fields(&self, inst: Rc<PyInstance>) -> Option<Ctx> {
        let n = &self.names;
        // SAFETY: the values are read in place, and nothing runs until the
        // operation that reads the context is done with them; the flag and
        // trap mappings stay owned by the context `Ctx` keeps alive.
        unsafe {
            if let Some(v) = self.ctx_fields(&inst) {
                let ctx = self.ctx_from(&v[..9], inst.clone())?;
                if let (Object::List(l), Ok(mut cache)) = (&v[6], self.ctx_cache.try_borrow_mut()) {
                    *cache = Some(CtxCache {
                        ctx: Rc::downgrade(&inst),
                        epoch: self.ctx_epoch.load(Relaxed),
                        p: ctx.p,
                        capitals: ctx.capitals,
                        traps: ctx.traps as usize,
                        flags: ctx.flags as usize,
                        ignored: Rc::as_ptr(l) as usize,
                    });
                }
                return Some(ctx);
            }
            let attr = |idx: usize, name: &SharedStr| -> Option<&Object> {
                if let Some((DictKey(Object::Str(k)), v)) = inst.attr_peek_index(idx) {
                    if SharedStr::ptr_eq(k, name) {
                        return Some(v);
                    }
                }
                None
            };
            let int = |o: &Object| match *o {
                Object::Int(v) => Some(v as i128),
                _ => None,
            };
            let (
                Some(prec),
                Some(rounding),
                Some(emin),
                Some(emax),
                Some(capitals),
                Some(clamp),
                Some(ignored),
                Some(traps),
                Some(flags),
            ) = (
                attr(0, &n.prec),
                attr(1, &n.rounding),
                attr(2, &n.emin),
                attr(3, &n.emax),
                attr(4, &n.capitals),
                attr(5, &n.clamp),
                attr(6, &n.ignored),
                attr(7, &n.traps),
                attr(8, &n.flags),
            )
            else {
                return self.read_ctx_slow(inst);
            };
            let p = CtxP {
                prec: int(prec)?,
                emin: int(emin)?,
                emax: int(emax)?,
                round: self.round(rounding)?,
                clamp: match int(clamp)? {
                    0 => false,
                    1 => true,
                    _ => return None,
                },
            };
            let capitals = int(capitals)? != 0;
            match ignored {
                Object::List(l) if l.peek()?.is_empty() => {}
                _ => return None,
            }
            let traps = self.signal_dict(traps, &inst)?;
            let flags = self.signal_dict(flags, &inst)?;
            if p.prec < 1 {
                return None;
            }
            Some(Ctx {
                p,
                capitals,
                flags,
                traps,
                _inst: inst,
            })
        }
    }

    /// The first nine attribute values of a context whose attributes are
    /// still split over its class's shared names, when those names are
    /// `Context.__init__`'s (proved once per class: the names at a
    /// position never change).
    ///
    /// # Safety
    ///
    /// As [`crate::sync::GilCell::peek`].
    #[inline]
    unsafe fn ctx_fields<'a>(&self, inst: &'a PyInstance) -> Option<&'a [Object]> {
        let keys: *const crate::inst_dict::SharedKeys = inst.cls_raw().shared_keys.get()?;
        if self.ctx_keys.load(Relaxed) != keys as usize && !self.prove_ctx_keys(keys) {
            return None;
        }
        // SAFETY: forwarded contract.
        let split = unsafe { inst.dict.split_peek() }?;
        split.get_over(keys, 8)?;
        Some(split.values())
    }

    #[cold]
    #[inline(never)]
    fn prove_ctx_keys(&self, keys: *const crate::inst_dict::SharedKeys) -> bool {
        let n = &self.names;
        let want = [
            &n.prec,
            &n.rounding,
            &n.emin,
            &n.emax,
            &n.capitals,
            &n.clamp,
            &n.ignored,
            &n.traps,
            &n.flags,
        ];
        // SAFETY: the class's shared names live as long as the class.
        let keys_ref = unsafe { &*keys };
        let ok = want.iter().enumerate().all(|(i, name)| {
            matches!(keys_ref.get(i), Some(DictKey(Object::Str(k)))
                if SharedStr::ptr_eq(k, name) || k.as_ref() as &str == name.as_ref() as &str)
        });
        if ok {
            self.ctx_keys.store(keys as usize, Relaxed);
        }
        ok
    }

    /// A [`Ctx`] from `Context.__init__`'s nine attribute values, in its
    /// order.
    ///
    /// # Safety
    ///
    /// `v` lives in `inst`, and nothing runs while the result is in use.
    unsafe fn ctx_from(&self, v: &[Object], inst: Rc<PyInstance>) -> Option<Ctx> {
        let int = |o: &Object| match *o {
            Object::Int(v) => Some(v as i128),
            _ => None,
        };
        let p = CtxP {
            prec: int(&v[0])?,
            emin: int(&v[2])?,
            emax: int(&v[3])?,
            round: self.round(&v[1])?,
            clamp: match int(&v[5])? {
                0 => false,
                1 => true,
                _ => return None,
            },
        };
        let capitals = int(&v[4])? != 0;
        match &v[6] {
            // SAFETY: forwarded contract.
            Object::List(l) if unsafe { l.peek() }?.is_empty() => {}
            _ => return None,
        }
        let (traps, flags) = self.signal_dicts(&v[7], &v[8], &inst)?;
        if p.prec < 1 {
            return None;
        }
        Some(Ctx {
            p,
            capitals,
            flags,
            traps,
            _inst: inst,
        })
    }

    /// [`Self::read_ctx`] for a context whose attributes were stored in
    /// another order: through owned copies.
    #[cold]
    #[inline(never)]
    fn read_ctx_slow(&self, inst: Rc<PyInstance>) -> Option<Ctx> {
        let n = &self.names;
        let attr = |name: &SharedStr| inst.attr_get_str(name);
        let int = |o: Object| match o {
            Object::Int(v) => Some(v as i128),
            _ => None,
        };
        let p = CtxP {
            prec: int(attr(&n.prec)?)?,
            emin: int(attr(&n.emin)?)?,
            emax: int(attr(&n.emax)?)?,
            round: round_of(&attr(&n.rounding)?)?,
            clamp: match int(attr(&n.clamp)?)? {
                0 => false,
                1 => true,
                _ => return None,
            },
        };
        let capitals = int(attr(&n.capitals)?)? != 0;
        match attr(&n.ignored)? {
            Object::List(l) if l.try_borrow().ok()?.is_empty() => {}
            _ => return None,
        }
        // The context's own references keep both mappings alive.
        let (traps, flags) = (attr(&n.traps)?, attr(&n.flags)?);
        let traps = self.signal_dict(&traps, &inst)?;
        let flags = self.signal_dict(&flags, &inst)?;
        if p.prec < 1 {
            return None;
        }
        Some(Ctx {
            p,
            capitals,
            flags,
            traps,
            _inst: inst,
        })
    }

    /// The mappings behind a context's `traps` and `flags` (see
    /// [`Self::signal_dict`]), settled by identity for the context seen
    /// last.
    #[inline]
    fn signal_dicts(
        &self,
        traps: &Object,
        flags: &Object,
        ctx: &Rc<PyInstance>,
    ) -> Option<(*const RefCell<DictData>, *const RefCell<DictData>)> {
        let (Object::Instance(t), Object::Instance(f)) = (traps, flags) else {
            return None;
        };
        // SAFETY: a read that runs no code. A live weak reference keeps
        // its allocation, so address matches name the same objects.
        let hit = unsafe { self.checked.peek() }
            .and_then(Option::as_ref)
            .is_some_and(|(cw, tw, fw)| {
                std::ptr::eq(cw.as_ptr(), Rc::as_ptr(ctx))
                    && std::ptr::eq(tw.as_ptr(), Rc::as_ptr(t))
                    && std::ptr::eq(fw.as_ptr(), Rc::as_ptr(f))
            });
        if hit {
            let map = |d: &PyInstance| match d.native.get() {
                Some(Object::Dict(m)) => Some(Rc::as_ptr(m)),
                _ => None,
            };
            return Some((map(t)?, map(f)?));
        }
        let pair = (self.signal_dict(traps, ctx)?, self.signal_dict(flags, ctx)?);
        if let Ok(mut c) = self.checked.try_borrow_mut() {
            *c = Some((Rc::downgrade(ctx), Rc::downgrade(t), Rc::downgrade(f)));
        }
        Some(pair)
    }

    /// The mapping behind a context's `flags` or `traps` (an exact,
    /// valid `SignalDict`).
    fn signal_dict(&self, o: &Object, ctx: &Rc<PyInstance>) -> Option<*const RefCell<DictData>> {
        let Object::Instance(d) = o else { return None };
        if !std::ptr::eq(d.cls_raw(), self.signal_dict.as_ptr()) {
            return None;
        }
        let Some(Object::Dict(map)) = d.native.get() else {
            return None;
        };
        // SAFETY: a read that runs no code.
        if unsafe { map.peek() }?.is_empty() || !self.owned_by(d, ctx) {
            return None;
        }
        Some(Rc::as_ptr(map))
    }

    /// Record the conditions `sig` in the context's flags, as
    /// `_raise_error` does when none of them is trapped; `None` (nothing
    /// recorded) when one is, for the Python code to raise.
    fn commit(&self, ctx: &Ctx, sig: u16) -> Option<()> {
        if sig == 0 {
            return Some(());
        }
        // SAFETY: `Ctx` keeps the context (and so its mappings) alive, and
        // nothing here runs code; the keys are the signal classes, which
        // hash and compare natively.
        unsafe {
            let traps = (*ctx.traps).peek()?;
            for bit in 0..N_SIGNALS {
                if sig & (1 << bit) != 0 {
                    match self.signal_entry(traps, bit)? {
                        Object::Bool(false) | Object::Int(0) => {}
                        _ => return None,
                    }
                }
            }
            let flags = (*ctx.flags).peek_mut()?;
            for bit in 0..N_SIGNALS {
                if sig & (1 << bit) != 0 {
                    let i = self.signal_index(flags, bit)?;
                    if let Some((_, v)) = flags.map_mut_value_store().get_index_mut(i) {
                        *v = Object::Bool(true);
                    }
                }
            }
        }
        Some(())
    }

    /// The position of signal `bit` in a flag or trap mapping (usually
    /// its `_signals` position).
    #[inline]
    fn signal_index(&self, map: &DictData, bit: usize) -> Option<usize> {
        let Object::Type(want) = self.signal(bit) else {
            return None;
        };
        match map.get_index(bit) {
            Some((DictKey(Object::Type(k)), _)) if Rc::ptr_eq(k, want) => Some(bit),
            _ => {
                // SAFETY: a borrowed key: no reference count changes hands.
                let key = std::mem::ManuallyDrop::new(DictKey(unsafe {
                    std::ptr::read(self.signal(bit))
                }));
                map.get_index_of(&*key)
            }
        }
    }

    #[inline]
    fn signal_entry<'a>(&self, map: &'a DictData, bit: usize) -> Option<&'a Object> {
        Some(map.get_index(self.signal_index(map, bit)?)?.1)
    }

    /// `ctx`'s flags recorded for `sig` and the value `d` as a new
    /// `Decimal`.
    #[inline]
    fn finish(&self, ctx: &Ctx, sig: u16, d: &Fin) -> Option<Object> {
        self.commit(ctx, sig)?;
        self.make(d)
    }
}

/// The last context [`State::read_ctx`] read through its fields, with
/// what it found (the pointers as addresses; see `read_ctx`).
struct CtxCache {
    ctx: Weak<PyInstance>,
    epoch: u64,
    p: CtxP,
    capitals: bool,
    traps: usize,
    flags: usize,
    ignored: usize,
}

/// A context read for one operation. The mappings are borrowed from the
/// context, which the value keeps alive; no Python code may run while
/// one is in use.
struct Ctx {
    p: CtxP,
    capitals: bool,
    flags: *const RefCell<DictData>,
    traps: *const RefCell<DictData>,
    _inst: Rc<PyInstance>,
}

/// An `int` (or `bool`) as a decimal, as `Decimal(int)` builds it.
fn int_fin(o: &Object) -> Option<Fin> {
    match o {
        Object::Int(v) => Some(Fin {
            sign: u8::from(*v < 0),
            coef: Coef::S(u128::from(v.unsigned_abs())),
            exp: 0,
        }),
        Object::Bool(b) => Some(Fin {
            sign: 0,
            coef: Coef::S(u128::from(*b)),
            exp: 0,
        }),
        Object::Long(b) => {
            let (s, mag) = (b.sign(), b.magnitude());
            if mag.bits() > MAX_DIGITS * 3 {
                return None;
            }
            Some(Fin {
                sign: u8::from(s == Sign::Minus),
                coef: Coef::from_big(mag.clone()),
                exp: 0,
            })
        }
        _ => None,
    }
}

/// `args[i]` when given (an omitted keyword parameter is `Unbound`).
#[inline]
fn arg(a: &[Object], i: usize) -> Option<&Object> {
    match a.get(i) {
        None | Some(Object::Unbound) => None,
        Some(o) => Some(o),
    }
}

/// The optional `context` argument at `i` (`None` when omitted).
#[inline]
fn ctx_arg(a: &[Object], i: usize) -> Option<&Object> {
    arg(a, i)
}

/// An optional rounding argument: `Some(None)` when omitted or `None`,
/// `Some(Some(mode))` for a valid mode, `None` for anything else.
fn rounding_arg(a: &[Object], i: usize) -> Option<Option<Round>> {
    match arg(a, i) {
        None | Some(Object::None) => Some(None),
        Some(o) => Some(Some(round_of(o)?)),
    }
}

type Fast = fn(&[Object]) -> Option<Result<Object, RuntimeError>>;

#[inline]
fn ok(o: Object) -> Option<Result<Object, RuntimeError>> {
    Some(Ok(o))
}

// ---------------------------------------------------------------------
// The pure fast halves. `None` means "not this shape": the caller runs
// the replaced Python method (or, inline, the full dispatch path).

/// The receiver's state and finite value, for a method of `Decimal`.
#[inline]
fn recv(a: &[Object], max: usize) -> Option<(&State, Fin)> {
    if a.len() > max {
        return None;
    }
    let s = a.first()?;
    let st = state_of(s)?;
    Some((st, st.fin(s)?))
}

/// A binary arithmetic operation: `f(x, y)` for `x OP y`, with `x`
/// and `y` in the order given (`reflected` swaps the receiver and the
/// argument), the context at `a[2]`.
#[inline]
fn binary(
    a: &[Object],
    reflected: bool,
    f: impl FnOnce(&Fin, &Fin, &CtxP, &mut u16) -> Option<Fin>,
) -> Option<Result<Object, RuntimeError>> {
    let (st, x) = recv(a, 3)?;
    let y = st.operand(a.get(1)?)?;
    let ctx = st.resolve(ctx_arg(a, 2))?;
    let mut sig = 0;
    let r = if reflected {
        f(&y, &x, &ctx.p, &mut sig)?
    } else {
        f(&x, &y, &ctx.p, &mut sig)?
    };
    ok(st.finish(&ctx, sig, &r)?)
}

fn d_add(a: &[Object]) -> Option<Result<Object, RuntimeError>> {
    binary(a, false, add)
}

fn d_sub(a: &[Object]) -> Option<Result<Object, RuntimeError>> {
    binary(a, false, |x, y, c, s| add(x, &y.negated(), c, s))
}

fn d_rsub(a: &[Object]) -> Option<Result<Object, RuntimeError>> {
    binary(a, true, |x, y, c, s| add(x, &y.negated(), c, s))
}

fn d_mul(a: &[Object]) -> Option<Result<Object, RuntimeError>> {
    binary(a, false, mul)
}

fn d_truediv(a: &[Object]) -> Option<Result<Object, RuntimeError>> {
    binary(a, false, div)
}

fn d_rtruediv(a: &[Object]) -> Option<Result<Object, RuntimeError>> {
    binary(a, true, div)
}

/// The value of an integral power exponent: an `int`, or an integral
/// finite `Decimal`, between 1 and a bound past which the Python code
/// runs.
fn int_exponent(y: &Fin) -> Option<u32> {
    if y.sign == 1 || y.is_zero() {
        return None;
    }
    let v = if y.exp >= 0 {
        y.coef
            .mul_pow10(u64::try_from(y.exp).ok().filter(|e| *e < 8)?)?
    } else {
        let (q, r) = y.coef.divrem_pow10(y.exp.unsigned_abs());
        if !r.is_zero() {
            return None;
        }
        q
    };
    match v {
        Coef::S(n @ 1..=100_000) => Some(n as u32),
        _ => None,
    }
}

/// `Decimal.__pow__(other)` without a modulus, for a finite base and a
/// positive integral exponent (`y` is its value, `ideal` the base's
/// exponent): the exact power when it fits in `prec + 1` digits, at the
/// exponent `_power_exact` picks, else the exact value, which `_fix`
/// rounds correctly as the Python code's `_dpower` result does. `None`
/// for zero bases, a base equal to one, and results that leave the
/// normal range.
fn power(x: &Fin, n: u32, c: &CtxP, sig: &mut u16) -> Option<Fin> {
    if x.is_zero() {
        return None;
    }
    let result_sign = if n & 1 == 1 { x.sign } else { 0 };
    // `self == 1` has its own exponent rule.
    if cmp(
        &x.abs(),
        &Fin {
            sign: 0,
            coef: Coef::S(1),
            exp: 0,
        },
    )? == Ordering::Equal
    {
        return None;
    }
    // Strip the coefficient's trailing zeros.
    let (mut xc, mut xe) = (x.coef.clone(), x.exp as i128);
    loop {
        let (q, r) = xc.divrem_pow10(1);
        if !r.is_zero() {
            break;
        }
        xc = q;
        xe += 1;
    }
    let m = i128::from(n);
    let p = c.prec + 1;
    let ideal = x.exp as i128 * m;
    let exp = xe * m;
    let ans = if xc.is_one() {
        let zeros = (exp - ideal).min(p - 1);
        Fin::new(
            result_sign,
            Coef::S(1).mul_pow10(u64::try_from(zeros).ok()?)?,
            exp - zeros,
        )?
    } else {
        // `xc ** n` stays below `MAX_DIGITS` digits, or the Python code runs.
        if (xc.ndigits() as u128).saturating_mul(u128::from(n)) > u128::from(MAX_DIGITS) + 1 {
            return None;
        }
        let mut pw = Coef::S(1);
        let mut base = xc;
        let mut e = n;
        while e > 0 {
            if e & 1 == 1 {
                pw = pw.mul(&base)?;
            }
            e >>= 1;
            if e > 0 {
                base = base.mul(&base)?;
            }
        }
        let len = pw.ndigits() as i128;
        if len <= p {
            let zeros = (exp - ideal).min(p - len);
            Fin::new(
                result_sign,
                pw.mul_pow10(u64::try_from(zeros).ok()?)?,
                exp - zeros,
            )?
        } else {
            Fin::new(result_sign, pw, exp)?
        }
    };
    let mut s = 0;
    let r = fix(ans, c, &mut s)?;
    // Subnormal, underflowing and clamped results follow the Python code's
    // own overflow and underflow estimates.
    if s & (S_SUBNORMAL | S_UNDERFLOW | S_CLAMPED) != 0 {
        return None;
    }
    *sig |= s;
    Some(r)
}

/// `__pow__(other, modulo=None, context=None)` / `__rpow__`.
fn pow_impl(a: &[Object], reflected: bool) -> Option<Result<Object, RuntimeError>> {
    if arg(a, 2).is_some_and(|m| !matches!(m, Object::None)) {
        return None;
    }
    let (st, x) = recv(a, 4)?;
    let y = st.operand(a.get(1)?)?;
    let ctx = st.resolve(ctx_arg(a, 3))?;
    let (base, exp) = if reflected { (y, x) } else { (x, y) };
    let n = int_exponent(&exp)?;
    let mut sig = 0;
    let r = power(&base, n, &ctx.p, &mut sig)?;
    ok(st.finish(&ctx, sig, &r)?)
}

fn d_pow(a: &[Object]) -> Option<Result<Object, RuntimeError>> {
    pow_impl(a, false)
}

fn d_rpow(a: &[Object]) -> Option<Result<Object, RuntimeError>> {
    pow_impl(a, true)
}

fn pow_op(x: &Fin, y: &Fin, c: &CtxP, sig: &mut u16) -> Option<Fin> {
    power(x, int_exponent(y)?, c, sig)
}

fn floordiv(x: &Fin, y: &Fin, c: &CtxP, _: &mut u16) -> Option<Fin> {
    Some(divide(x, y, c)?.0)
}

fn rem(x: &Fin, y: &Fin, c: &CtxP, sig: &mut u16) -> Option<Fin> {
    fix(divide(x, y, c)?.1, c, sig)
}

fn d_floordiv(a: &[Object]) -> Option<Result<Object, RuntimeError>> {
    binary(a, false, floordiv)
}

fn d_rfloordiv(a: &[Object]) -> Option<Result<Object, RuntimeError>> {
    binary(a, true, floordiv)
}

fn d_mod(a: &[Object]) -> Option<Result<Object, RuntimeError>> {
    binary(a, false, rem)
}

fn d_rmod(a: &[Object]) -> Option<Result<Object, RuntimeError>> {
    binary(a, true, rem)
}

fn divmod_impl(a: &[Object], reflected: bool) -> Option<Result<Object, RuntimeError>> {
    let (st, x) = recv(a, 3)?;
    let y = st.operand(a.get(1)?)?;
    let ctx = st.resolve(ctx_arg(a, 2))?;
    let (x, y) = if reflected { (y, x) } else { (x, y) };
    let (q, r) = divide(&x, &y, &ctx.p)?;
    let mut sig = 0;
    let r = fix(r, &ctx.p, &mut sig)?;
    let (q, r) = (st.make(&q)?, st.make(&r)?);
    st.commit(&ctx, sig)?;
    ok(Object::new_tuple_array([q, r]))
}

fn d_divmod(a: &[Object]) -> Option<Result<Object, RuntimeError>> {
    divmod_impl(a, false)
}

fn d_rdivmod(a: &[Object]) -> Option<Result<Object, RuntimeError>> {
    divmod_impl(a, true)
}

/// `__neg__` / `__pos__` (with `negate`): the zero-sign rule, then `_fix`.
fn unary(a: &[Object], negate: bool) -> Option<Result<Object, RuntimeError>> {
    let (st, x) = recv(a, 2)?;
    let ctx = st.resolve(ctx_arg(a, 1))?;
    let ans = if x.is_zero() && ctx.p.round != Round::Floor {
        x.abs()
    } else if negate {
        x.negated()
    } else {
        x
    };
    let mut sig = 0;
    let r = fix(ans, &ctx.p, &mut sig)?;
    ok(st.finish(&ctx, sig, &r)?)
}

fn d_neg(a: &[Object]) -> Option<Result<Object, RuntimeError>> {
    unary(a, true)
}

fn d_pos(a: &[Object]) -> Option<Result<Object, RuntimeError>> {
    unary(a, false)
}

/// `__abs__(round=True, context=None)`.
fn d_abs(a: &[Object]) -> Option<Result<Object, RuntimeError>> {
    match arg(a, 1) {
        None | Some(Object::Bool(true)) => {}
        _ => return None,
    }
    let (_, x) = recv(a, 3)?;
    let rest = [a[0].clone(), arg(a, 2).cloned().unwrap_or(Object::None)];
    unary(&rest, x.sign == 1)
}

/// The comparison of the receiver with an exact `Decimal` or an `int`.
fn compare_with(a: &[Object]) -> Option<Ordering> {
    let (st, x) = recv(a, 3)?;
    let y = st.operand(a.get(1)?)?;
    if arg(a, 2).is_some_and(|c| !matches!(c, Object::None)) {
        return None;
    }
    cmp(&x, &y)
}

fn d_eq(a: &[Object]) -> Option<Result<Object, RuntimeError>> {
    ok(Object::Bool(compare_with(a)? == Ordering::Equal))
}

fn d_lt(a: &[Object]) -> Option<Result<Object, RuntimeError>> {
    ok(Object::Bool(compare_with(a)?.is_lt()))
}

fn d_le(a: &[Object]) -> Option<Result<Object, RuntimeError>> {
    ok(Object::Bool(compare_with(a)?.is_le()))
}

fn d_gt(a: &[Object]) -> Option<Result<Object, RuntimeError>> {
    ok(Object::Bool(compare_with(a)?.is_gt()))
}

fn d_ge(a: &[Object]) -> Option<Result<Object, RuntimeError>> {
    ok(Object::Bool(compare_with(a)?.is_ge()))
}

fn d_hash(a: &[Object]) -> Option<Result<Object, RuntimeError>> {
    let (_, x) = recv(a, 1)?;
    ok(Object::Int(hash(&x)))
}

fn d_bool(a: &[Object]) -> Option<Result<Object, RuntimeError>> {
    let (_, x) = recv(a, 1)?;
    ok(Object::Bool(!x.is_zero()))
}

/// `__str__(eng=False, context=None)`.
fn d_str(a: &[Object]) -> Option<Result<Object, RuntimeError>> {
    let eng = match arg(a, 1) {
        None | Some(Object::Bool(false)) => false,
        Some(Object::Bool(true)) => true,
        _ => return None,
    };
    let (st, x) = recv(a, 3)?;
    let s = sci_string(st, &x, eng, ctx_arg(a, 2))?;
    ok(Object::Str(SharedStr::from_ascii(&s)))
}

/// [`to_sci`], reading `capitals` from the context only when an exponent
/// is shown (as the Python code does).
fn sci_string(st: &State, x: &Fin, eng: bool, context: Option<&Object>) -> Option<String> {
    let lower = to_sci(x, eng, false);
    if !lower.contains('e') {
        if let Some(c) = context {
            if !matches!(c, Object::None) {
                st.resolve(Some(c))?;
            }
        }
        return Some(lower);
    }
    let ctx = st.resolve(context)?;
    Some(if ctx.capitals {
        lower.replace('e', "E")
    } else {
        lower
    })
}

fn d_repr(a: &[Object]) -> Option<Result<Object, RuntimeError>> {
    let (st, x) = recv(a, 1)?;
    let s = sci_string(st, &x, false, None)?;
    ok(Object::Str(SharedStr::from_ascii(&format!(
        "Decimal('{s}')"
    ))))
}

fn d_to_eng_string(a: &[Object]) -> Option<Result<Object, RuntimeError>> {
    let (st, x) = recv(a, 2)?;
    let c = ctx_arg(a, 1);
    // `_resolve_context` runs first even when no exponent is shown.
    st.resolve(c)?;
    let s = sci_string(st, &x, true, c)?;
    ok(Object::Str(SharedStr::from_ascii(&s)))
}

fn d_float(a: &[Object]) -> Option<Result<Object, RuntimeError>> {
    let (_, x) = recv(a, 1)?;
    let s = to_sci(&x, false, false);
    ok(Object::Float(s.parse::<f64>().ok()?))
}

/// The integer part of a finite value (`__int__`), or `None` past
/// [`MAX_DIGITS`].
fn to_int(x: &Fin) -> Option<Object> {
    let mag = if x.exp >= 0 {
        x.coef.mul_pow10(x.exp as u64)?
    } else {
        x.coef.divrem_pow10(x.exp.unsigned_abs()).0
    };
    let o = mag.to_object();
    if x.sign == 1 && !mag.is_zero() {
        return Some(match o {
            Object::Int(v) => Object::Int(-v),
            Object::Long(b) => Object::int_from_bigint(-(*b).clone()),
            _ => return None,
        });
    }
    Some(o)
}

fn d_int(a: &[Object]) -> Option<Result<Object, RuntimeError>> {
    let (_, x) = recv(a, 1)?;
    ok(to_int(&x)?)
}

/// `int(self._rescale(0, mode))`: `__round__()`, `__floor__`, `__ceil__`.
fn int_rounded(a: &[Object], mode: Round) -> Option<Result<Object, RuntimeError>> {
    let (_, x) = recv(a, 2)?;
    ok(to_int(&rescale(&x, 0, mode)?)?)
}

fn d_floor(a: &[Object]) -> Option<Result<Object, RuntimeError>> {
    if a.len() != 1 {
        return None;
    }
    int_rounded(a, Round::Floor)
}

fn d_ceil(a: &[Object]) -> Option<Result<Object, RuntimeError>> {
    if a.len() != 1 {
        return None;
    }
    int_rounded(a, Round::Ceiling)
}

/// `__round__(n=None)`.
fn d_round(a: &[Object]) -> Option<Result<Object, RuntimeError>> {
    match arg(a, 1) {
        None | Some(Object::None) => int_rounded(a, Round::HalfEven),
        Some(Object::Int(n)) => {
            let (st, x) = recv(a, 2)?;
            let ctx = st.resolve(None)?;
            let mut sig = 0;
            let e = n.checked_neg()?;
            let r = quantize(&x, e, ctx.p.round, &ctx.p, &mut sig)?;
            ok(st.finish(&ctx, sig, &r)?)
        }
        _ => None,
    }
}

/// `quantize(exp, rounding=None, context=None)`.
fn d_quantize(a: &[Object]) -> Option<Result<Object, RuntimeError>> {
    let (st, x) = recv(a, 4)?;
    let e = st.operand(a.get(1)?)?;
    let mode = rounding_arg(a, 2)?;
    let ctx = st.resolve(ctx_arg(a, 3))?;
    let mut sig = 0;
    let r = quantize(&x, e.exp, mode.unwrap_or(ctx.p.round), &ctx.p, &mut sig)?;
    ok(st.finish(&ctx, sig, &r)?)
}

/// `to_integral_value(rounding=None, context=None)`.
fn d_to_integral_value(a: &[Object]) -> Option<Result<Object, RuntimeError>> {
    let (st, x) = recv(a, 3)?;
    let mode = rounding_arg(a, 1)?;
    let ctx = st.resolve(ctx_arg(a, 2))?;
    if x.exp >= 0 {
        return ok(st.make(&x)?);
    }
    ok(st.make(&rescale(&x, 0, mode.unwrap_or(ctx.p.round))?)?)
}

/// `to_integral_exact(rounding=None, context=None)`.
fn d_to_integral_exact(a: &[Object]) -> Option<Result<Object, RuntimeError>> {
    let (st, x) = recv(a, 3)?;
    let mode = rounding_arg(a, 1)?;
    let ctx = st.resolve(ctx_arg(a, 2))?;
    if x.exp >= 0 {
        return ok(st.make(&x)?);
    }
    if x.is_zero() {
        return ok(st.make(&Fin {
            sign: x.sign,
            coef: Coef::ZERO,
            exp: 0,
        })?);
    }
    let (ans, inexact) = rescale_inexact(&x, 0, mode.unwrap_or(ctx.p.round))?;
    let mut sig = S_ROUNDED;
    if inexact {
        sig |= S_INEXACT;
    }
    ok(st.finish(&ctx, sig, &ans)?)
}

fn d_adjusted(a: &[Object]) -> Option<Result<Object, RuntimeError>> {
    let (_, x) = recv(a, 1)?;
    ok(Object::Int(i64::try_from(x.adjusted()).ok()?))
}

fn d_copy_abs(a: &[Object]) -> Option<Result<Object, RuntimeError>> {
    let (st, x) = recv(a, 1)?;
    ok(st.make(&x.abs())?)
}

fn d_copy_negate(a: &[Object]) -> Option<Result<Object, RuntimeError>> {
    let (st, x) = recv(a, 1)?;
    ok(st.make(&x.negated())?)
}

/// `copy_sign(other, context=None)`.
fn d_copy_sign(a: &[Object]) -> Option<Result<Object, RuntimeError>> {
    let (st, x) = recv(a, 3)?;
    let y = st.operand(a.get(1)?)?;
    if arg(a, 2).is_some_and(|c| !matches!(c, Object::None)) {
        return None;
    }
    ok(st.make(&Fin {
        sign: y.sign,
        coef: x.coef,
        exp: x.exp,
    })?)
}

/// `compare(other, context=None)`.
fn d_compare(a: &[Object]) -> Option<Result<Object, RuntimeError>> {
    let (st, x) = recv(a, 3)?;
    let y = st.operand(a.get(1)?)?;
    if arg(a, 2).is_some_and(|c| !matches!(c, Object::None)) {
        st.resolve(arg(a, 2))?;
    }
    let v = match cmp(&x, &y)? {
        Ordering::Less => -1,
        Ordering::Equal => 0,
        Ordering::Greater => 1,
    };
    ok(st.make(&int_fin(&Object::Int(v))?)?)
}

/// The predicates that hold for every finite value or none.
fn d_is_finite(a: &[Object]) -> Option<Result<Object, RuntimeError>> {
    recv(a, 1)?;
    ok(Object::Bool(true))
}

fn d_is_not(a: &[Object]) -> Option<Result<Object, RuntimeError>> {
    recv(a, 1)?;
    ok(Object::Bool(false))
}

fn d_is_signed(a: &[Object]) -> Option<Result<Object, RuntimeError>> {
    let (_, x) = recv(a, 1)?;
    ok(Object::Bool(x.sign == 1))
}

fn d_is_zero(a: &[Object]) -> Option<Result<Object, RuntimeError>> {
    let (_, x) = recv(a, 1)?;
    ok(Object::Bool(x.is_zero()))
}

/// `is_normal(context=None)` / `is_subnormal(context=None)`.
fn normal_impl(a: &[Object], want_normal: bool) -> Option<Result<Object, RuntimeError>> {
    let (st, x) = recv(a, 2)?;
    let ctx = st.resolve(ctx_arg(a, 1))?;
    if x.is_zero() {
        return ok(Object::Bool(false));
    }
    let normal = ctx.p.emin <= x.adjusted();
    ok(Object::Bool(normal == want_normal))
}

fn d_is_normal(a: &[Object]) -> Option<Result<Object, RuntimeError>> {
    normal_impl(a, true)
}

fn d_is_subnormal(a: &[Object]) -> Option<Result<Object, RuntimeError>> {
    normal_impl(a, false)
}

/// `as_tuple()`: `DecimalTuple(sign, digits, exp)`.
fn d_as_tuple(a: &[Object]) -> Option<Result<Object, RuntimeError>> {
    let (st, x) = recv(a, 1)?;
    let cls = st.decimal_tuple.upgrade()?;
    let digits: Vec<Object> = x
        .coef
        .to_digits()
        .bytes()
        .map(|c| Object::Int(i64::from(c - b'0')))
        .collect();
    let t = Object::new_tuple_array([
        Object::Int(i64::from(x.sign)),
        Object::new_tuple(digits),
        Object::Int(x.exp),
    ]);
    ok(Object::Instance(Rc::new(PyInstance::with_native(cls, t))))
}

/// `same_quantum(other, context=None)`.
fn d_same_quantum(a: &[Object]) -> Option<Result<Object, RuntimeError>> {
    let (st, x) = recv(a, 3)?;
    let y = st.operand(a.get(1)?)?;
    if arg(a, 2).is_some_and(|c| !matches!(c, Object::None)) {
        return None;
    }
    ok(Object::Bool(x.exp == y.exp))
}

/// `normalize(context=None)`.
fn d_normalize(a: &[Object]) -> Option<Result<Object, RuntimeError>> {
    let (st, x) = recv(a, 2)?;
    let ctx = st.resolve(ctx_arg(a, 1))?;
    let mut sig = 0;
    let dup = fix(x, &ctx.p, &mut sig)?;
    let r = if dup.is_zero() {
        Fin {
            sign: dup.sign,
            coef: Coef::ZERO,
            exp: 0,
        }
    } else {
        let exp_max = if ctx.p.clamp {
            ctx.p.etop()
        } else {
            ctx.p.emax
        };
        let mut coef = dup.coef;
        let mut exp = dup.exp as i128;
        while exp < exp_max && coef.rem_small(10) == 0 {
            coef = coef.divrem_pow10(1).0;
            exp += 1;
        }
        Fin::new(dup.sign, coef, exp)?
    };
    ok(st.finish(&ctx, sig, &r)?)
}

/// `max`/`min(other, context=None)` for finite operands.
fn minmax(a: &[Object], want_max: bool) -> Option<Result<Object, RuntimeError>> {
    let (st, x) = recv(a, 3)?;
    let y = st.operand(a.get(1)?)?;
    let ctx = st.resolve(ctx_arg(a, 2))?;
    let mut c = cmp(&x, &y)?;
    if c == Ordering::Equal {
        // `compare_total` for numerically equal finite values: the sign,
        // then the exponent (reversed for negative values).
        c = if x.sign != y.sign {
            if x.sign == 1 {
                Ordering::Less
            } else {
                Ordering::Greater
            }
        } else {
            let e = x.exp.cmp(&y.exp);
            if x.sign == 1 {
                e.reverse()
            } else {
                e
            }
        };
    }
    let pick_y = if want_max {
        c == Ordering::Less
    } else {
        c != Ordering::Less
    };
    let ans = if pick_y { y } else { x };
    let mut sig = 0;
    let r = fix(ans, &ctx.p, &mut sig)?;
    ok(st.finish(&ctx, sig, &r)?)
}

fn d_max(a: &[Object]) -> Option<Result<Object, RuntimeError>> {
    minmax(a, true)
}

fn d_min(a: &[Object]) -> Option<Result<Object, RuntimeError>> {
    minmax(a, false)
}

/// `as_integer_ratio()`.
fn d_as_integer_ratio(a: &[Object]) -> Option<Result<Object, RuntimeError>> {
    let (_, x) = recv(a, 1)?;
    if x.is_zero() {
        return ok(Object::new_tuple_array([Object::Int(0), Object::Int(1)]));
    }
    let mut n = x.coef.to_big();
    let d = if x.exp >= 0 {
        n *= pow10_big(u64::try_from(x.exp).ok().filter(|e| *e <= MAX_DIGITS)?);
        BigUint::from(1u32)
    } else {
        let k = x.exp.unsigned_abs();
        if k > MAX_DIGITS {
            return None;
        }
        let mut d5 = k;
        let five = BigUint::from(5u32);
        while d5 > 0 {
            let (q, r) = n.div_rem(&five);
            if !r.is_zero() {
                break;
            }
            n = q;
            d5 -= 1;
        }
        let d2 = k;
        let shift2 = n.trailing_zeros().unwrap_or(0).min(d2);
        n >>= shift2 as usize;
        (num_traits::pow(five, d5 as usize)) << ((d2 - shift2) as usize)
    };
    let mut n = BigInt::from_biguint(Sign::Plus, n);
    if x.sign == 1 {
        n = -n;
    }
    ok(Object::new_tuple_array([
        Object::int_from_bigint(n),
        Object::int_from_bigint(BigInt::from_biguint(Sign::Plus, d)),
    ]))
}

/// `Decimal.from_float(f)` (a class method: `a[0]` is the class).
fn d_from_float(a: &[Object]) -> Option<Result<Object, RuntimeError>> {
    let Object::Type(cls) = a.first()? else {
        return None;
    };
    let st = state_of_cls(cls)?;
    if !std::ptr::eq(&**cls, st.decimal.as_ptr()) || a.len() != 2 {
        return None;
    }
    let d = match &a[1] {
        Object::Float(f) => from_f64(*f)?,
        o => int_fin(o)?,
    };
    ok(st.make(&d)?)
}

/// `Decimal(value="0", context=None)` for the exact class: `None` for
/// every shape the Python constructor must serve (and diagnose).
pub(crate) fn construct(
    cls: &TypeObject,
    args: &[Object],
    kwargs: &[(String, Object)],
) -> Option<Result<Object, RuntimeError>> {
    let st = state_of_cls(cls)?;
    if !std::ptr::eq(cls, st.decimal.as_ptr()) {
        return None;
    }
    let mut value = args.first();
    let mut context = args.get(1);
    if args.len() > 2 {
        return None;
    }
    for (k, v) in kwargs {
        match k.as_str() {
            "value" if value.is_none() => value = Some(v),
            "context" if context.is_none() => context = Some(v),
            _ => return None,
        }
    }
    construct_value(st, value, context)
}

/// [`construct`] with the keyword names as the call's names tuple.
pub(crate) fn construct_names(
    cls: &TypeObject,
    args: &[Object],
    names: &[Object],
    values: &[Object],
) -> Option<Result<Object, RuntimeError>> {
    let st = state_of_cls(cls)?;
    if !std::ptr::eq(cls, st.decimal.as_ptr()) || names.len() != values.len() || args.len() > 2 {
        return None;
    }
    let mut value = args.first();
    let mut context = args.get(1);
    for (k, v) in names.iter().zip(values) {
        let Object::Str(k) = k else { return None };
        match k.as_ref() as &str {
            "value" if value.is_none() => value = Some(v),
            "context" if context.is_none() => context = Some(v),
            _ => return None,
        }
    }
    construct_value(st, value, context)
}

fn construct_value(
    st: &State,
    value: Option<&Object>,
    context: Option<&Object>,
) -> Option<Result<Object, RuntimeError>> {
    // The context argument must be `None` or a `Context`.
    let context = match context {
        None | Some(Object::None) => None,
        Some(c @ Object::Instance(i)) if std::ptr::eq(i.cls_raw(), st.context.as_ptr()) => Some(c),
        _ => return None,
    };
    let d = match value {
        None => Fin {
            sign: 0,
            coef: Coef::ZERO,
            exp: 0,
        },
        Some(Object::Str(s)) => parse_str(s.as_ref(), true)?,
        Some(Object::Float(f)) => {
            // `_raise_error(FloatOperation)` on the resolved context.
            let d = from_f64(*f)?;
            let ctx = st.resolve(context)?;
            let r = st.make(&d)?;
            st.commit(&ctx, S_FLOATOP)?;
            return ok(r);
        }
        Some(Object::Instance(i)) if st.is_decimal(i) => return ok(st.copy_of(i)?),
        Some(o) => int_fin(o)?,
    };
    // The context-free constructor's exact range.
    let exp = d.exp as i128;
    if exp + d.coef.ndigits() as i128 - 1 > MAX_EMAX || exp < MIN_ETINY {
        return None;
    }
    ok(st.make(&d)?)
}

// ---------------------------------------------------------------------
// `Context` methods. `a[0]` is the context.

/// The receiver context (an exact `Context`), read.
fn ctx_recv(a: &[Object], min: usize, max: usize) -> Option<(&State, Ctx)> {
    if a.len() < min || a.len() > max {
        return None;
    }
    let c = a.first()?;
    let st = state_of(c)?;
    let Object::Instance(i) = c else { return None };
    if !std::ptr::eq(i.cls_raw(), st.context.as_ptr()) {
        return None;
    }
    Some((st, st.read_ctx(i.clone())?))
}

fn ctx_binary(
    a: &[Object],
    f: impl FnOnce(&Fin, &Fin, &CtxP, &mut u16) -> Option<Fin>,
) -> Option<Result<Object, RuntimeError>> {
    let (st, ctx) = ctx_recv(a, 3, 3)?;
    let x = st.operand(a.get(1)?)?;
    let y = st.operand(a.get(2)?)?;
    let mut sig = 0;
    let r = f(&x, &y, &ctx.p, &mut sig)?;
    ok(st.finish(&ctx, sig, &r)?)
}

fn c_add(a: &[Object]) -> Option<Result<Object, RuntimeError>> {
    ctx_binary(a, add)
}

fn c_subtract(a: &[Object]) -> Option<Result<Object, RuntimeError>> {
    ctx_binary(a, |x, y, c, s| add(x, &y.negated(), c, s))
}

fn c_multiply(a: &[Object]) -> Option<Result<Object, RuntimeError>> {
    ctx_binary(a, mul)
}

fn c_divide(a: &[Object]) -> Option<Result<Object, RuntimeError>> {
    ctx_binary(a, div)
}

fn c_divide_int(a: &[Object]) -> Option<Result<Object, RuntimeError>> {
    ctx_binary(a, floordiv)
}

fn c_remainder(a: &[Object]) -> Option<Result<Object, RuntimeError>> {
    ctx_binary(a, rem)
}

fn c_divmod(a: &[Object]) -> Option<Result<Object, RuntimeError>> {
    let (st, ctx) = ctx_recv(a, 3, 3)?;
    let x = st.operand(a.get(1)?)?;
    let y = st.operand(a.get(2)?)?;
    let (q, r) = divide(&x, &y, &ctx.p)?;
    let mut sig = 0;
    let r = fix(r, &ctx.p, &mut sig)?;
    let (q, r) = (st.make(&q)?, st.make(&r)?);
    st.commit(&ctx, sig)?;
    ok(Object::new_tuple_array([q, r]))
}

fn c_quantize(a: &[Object]) -> Option<Result<Object, RuntimeError>> {
    let (st, ctx) = ctx_recv(a, 3, 3)?;
    let x = st.operand(a.get(1)?)?;
    let e = st.operand(a.get(2)?)?;
    let mut sig = 0;
    let r = quantize(&x, e.exp, ctx.p.round, &ctx.p, &mut sig)?;
    ok(st.finish(&ctx, sig, &r)?)
}

/// `plus` / `minus` / `abs` of a context.
fn ctx_unary(a: &[Object], op: u8) -> Option<Result<Object, RuntimeError>> {
    let (st, ctx) = ctx_recv(a, 2, 2)?;
    let x = st.operand(a.get(1)?)?;
    let negate = match op {
        0 => false,
        1 => true,
        _ => x.sign == 1,
    };
    let ans = if x.is_zero() && ctx.p.round != Round::Floor {
        x.abs()
    } else if negate {
        x.negated()
    } else {
        x
    };
    let mut sig = 0;
    let r = fix(ans, &ctx.p, &mut sig)?;
    ok(st.finish(&ctx, sig, &r)?)
}

fn c_plus(a: &[Object]) -> Option<Result<Object, RuntimeError>> {
    ctx_unary(a, 0)
}

fn c_minus(a: &[Object]) -> Option<Result<Object, RuntimeError>> {
    ctx_unary(a, 1)
}

fn c_abs(a: &[Object]) -> Option<Result<Object, RuntimeError>> {
    ctx_unary(a, 2)
}

fn c_compare(a: &[Object]) -> Option<Result<Object, RuntimeError>> {
    let (st, _) = ctx_recv(a, 3, 3)?;
    let x = st.operand(a.get(1)?)?;
    let y = st.operand(a.get(2)?)?;
    let v = match cmp(&x, &y)? {
        Ordering::Less => -1,
        Ordering::Equal => 0,
        Ordering::Greater => 1,
    };
    ok(st.make(&int_fin(&Object::Int(v))?)?)
}

fn c_to_integral_value(a: &[Object]) -> Option<Result<Object, RuntimeError>> {
    let (st, ctx) = ctx_recv(a, 2, 2)?;
    let x = st.operand(a.get(1)?)?;
    if x.exp >= 0 {
        return ok(st.make(&x)?);
    }
    ok(st.make(&rescale(&x, 0, ctx.p.round)?)?)
}

fn c_to_integral_exact(a: &[Object]) -> Option<Result<Object, RuntimeError>> {
    let (st, ctx) = ctx_recv(a, 2, 2)?;
    let x = st.operand(a.get(1)?)?;
    if x.exp >= 0 {
        return ok(st.make(&x)?);
    }
    if x.is_zero() {
        return ok(st.make(&Fin {
            sign: x.sign,
            coef: Coef::ZERO,
            exp: 0,
        })?);
    }
    let (ans, inexact) = rescale_inexact(&x, 0, ctx.p.round)?;
    let mut sig = S_ROUNDED;
    if inexact {
        sig |= S_INEXACT;
    }
    ok(st.finish(&ctx, sig, &ans)?)
}

fn c_to_sci_string(a: &[Object]) -> Option<Result<Object, RuntimeError>> {
    let (st, ctx) = ctx_recv(a, 2, 2)?;
    let x = st.operand(a.get(1)?)?;
    let s = to_sci(&x, false, ctx.capitals);
    ok(Object::Str(SharedStr::from_ascii(&s)))
}

/// `create_decimal(num='0')`: the conversion, then `_fix` under the
/// context.
fn c_create_decimal(a: &[Object]) -> Option<Result<Object, RuntimeError>> {
    let (st, ctx) = ctx_recv(a, 1, 2)?;
    let mut sig = 0;
    let d = match arg(a, 1) {
        None => Fin {
            sign: 0,
            coef: Coef::ZERO,
            exp: 0,
        },
        Some(Object::Str(s)) => parse_str(s.as_ref(), false)?,
        Some(Object::Float(f)) => {
            sig |= S_FLOATOP;
            from_f64(*f)?
        }
        Some(o @ Object::Instance(_)) => st.fin(o)?,
        Some(o) => int_fin(o)?,
    };
    let r = fix(d, &ctx.p, &mut sig)?;
    ok(st.finish(&ctx, sig, &r)?)
}

/// `create_decimal_from_float(f)`.
fn c_create_decimal_from_float(a: &[Object]) -> Option<Result<Object, RuntimeError>> {
    let (st, ctx) = ctx_recv(a, 2, 2)?;
    let d = match a.get(1)? {
        Object::Float(f) => from_f64(*f)?,
        o => int_fin(o)?,
    };
    let mut sig = 0;
    let r = fix(d, &ctx.p, &mut sig)?;
    ok(st.finish(&ctx, sig, &r)?)
}

// ---------------------------------------------------------------------
// `__format__` (PEP 3101), for every type but the locale-dependent `n`.

/// A parsed format specifier (`_parse_format_specifier`'s dictionary).
struct FormatSpec {
    fill: char,
    align: u8,
    sign: u8,
    no_neg_0: bool,
    alt: bool,
    zeropad: bool,
    minimumwidth: usize,
    thousands_sep: &'static str,
    precision: Option<usize>,
    frac_separators: &'static str,
    ty: Option<u8>,
}

/// `_parse_format_specifier` for a specifier the native code formats;
/// `None` for the `n` type and every specifier the Python code rejects.
fn parse_spec(spec: &str) -> Option<FormatSpec> {
    let mut chars = spec.char_indices().peekable();
    let is_align = |c: char| matches!(c, '<' | '>' | '=' | '^');
    let mut fill = None;
    let mut align = None;
    let mut rest = spec;
    // `(?:(?P<fill>.)?(?P<align>[<>=^]))?`
    if let Some((_, c0)) = chars.next() {
        let after0 = &spec[c0.len_utf8()..];
        match after0.chars().next() {
            Some(c1) if is_align(c1) => {
                fill = Some(c0);
                align = Some(c1 as u8);
                rest = &after0[1..];
            }
            _ if is_align(c0) => {
                align = Some(c0 as u8);
                rest = after0;
            }
            _ => {}
        }
    }
    let b = rest.as_bytes();
    let mut i = 0;
    let mut sign = b'-';
    if i < b.len() && matches!(b[i], b'-' | b'+' | b' ') {
        sign = b[i];
        i += 1;
    }
    let no_neg_0 = i < b.len() && b[i] == b'z';
    if no_neg_0 {
        i += 1;
    }
    let alt = i < b.len() && b[i] == b'#';
    if alt {
        i += 1;
    }
    let zeropad = i < b.len() && b[i] == b'0';
    if zeropad {
        i += 1;
    }
    let ws = i;
    while i < b.len() && b[i].is_ascii_digit() {
        i += 1;
    }
    let minimumwidth = if i > ws {
        if i - ws > 15 {
            return None;
        }
        rest[ws..i].parse::<usize>().ok()?
    } else {
        0
    };
    let sep = |c: u8| match c {
        b',' => Some(","),
        b'_' => Some("_"),
        _ => None,
    };
    let mut thousands_sep = "";
    if let Some(s) = b.get(i).copied().and_then(sep) {
        thousands_sep = s;
        i += 1;
    }
    let mut precision = None;
    let mut frac_separators = "";
    if i < b.len() && b[i] == b'.' {
        // `\.(?=[\d,_])`
        match b.get(i + 1) {
            Some(c) if c.is_ascii_digit() || *c == b',' || *c == b'_' => {}
            _ => return None,
        }
        i += 1;
        let ps = i;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        if i > ps {
            if i - ps > 15 {
                return None;
            }
            precision = Some(rest[ps..i].parse::<usize>().ok()?);
        }
        if let Some(s) = b.get(i).copied().and_then(sep) {
            frac_separators = s;
            i += 1;
        }
    }
    let mut ty = None;
    if i < b.len() && matches!(b[i], b'e' | b'E' | b'f' | b'F' | b'g' | b'G' | b'%') {
        ty = Some(b[i]);
        i += 1;
    }
    if i != b.len() {
        return None;
    }
    if zeropad && (fill.is_some() || align.is_some()) {
        return None;
    }
    if precision == Some(0) && matches!(ty, None | Some(b'g' | b'G')) {
        precision = Some(1);
    }
    Some(FormatSpec {
        fill: fill.unwrap_or(' '),
        align: align.unwrap_or(b'>'),
        sign,
        no_neg_0,
        alt,
        zeropad,
        minimumwidth,
        thousands_sep,
        precision,
        frac_separators,
        ty,
    })
}

/// `Decimal._round(places, rounding)` for a finite value (`places >= 1`).
fn round_places(d: &Fin, places: i128, mode: Round) -> Option<Fin> {
    if d.is_zero() {
        return Some(d.clone());
    }
    let ans = rescale(d, d.adjusted() + 1 - places, mode)?;
    if ans.adjusted() != d.adjusted() {
        return rescale(&ans, ans.adjusted() + 1 - places, mode);
    }
    Some(ans)
}

/// `_insert_thousands_sep(digits, spec, min_width)` with the `[3, 0]`
/// grouping.
fn insert_thousands_sep(digits: &str, sep: &str, min_width: isize) -> String {
    let mut digits = digits;
    let mut min_width = min_width;
    let mut groups: Vec<String> = Vec::new();
    loop {
        let l = (digits.len() as isize).max(min_width).max(1).min(3) as usize;
        let take = l.min(digits.len());
        let mut g = "0".repeat(l - take);
        g.push_str(&digits[digits.len() - take..]);
        groups.push(g);
        digits = &digits[..digits.len() - take];
        min_width -= l as isize;
        if digits.is_empty() && min_width <= 0 {
            break;
        }
        min_width -= sep.chars().count() as isize;
    }
    groups.reverse();
    groups.join(sep)
}

/// `Decimal.__format__(specifier)` for a finite value under the context
/// `ctx`, or `None`.
fn format_fin(x: &Fin, spec: &str, ctx: &Ctx) -> Option<String> {
    let sp = parse_spec(spec)?;
    let ty = sp.ty.unwrap_or(if ctx.capitals { b'G' } else { b'g' });
    let mut d = x.clone();
    if ty == b'%' {
        d.exp = d.exp.checked_add(2)?;
    }
    let rounding = ctx.p.round;
    if let Some(p) = sp.precision {
        let p = p as i128;
        match ty {
            b'e' | b'E' => d = round_places(&d, p + 1, rounding)?,
            b'f' | b'F' | b'%' => d = rescale(&d, -p, rounding)?,
            _ => {
                if d.coef.ndigits() as i128 > p {
                    d = round_places(&d, p, rounding)?;
                }
            }
        }
    }
    if d.is_zero() && d.exp > 0 && matches!(ty, b'f' | b'F' | b'%') {
        d = rescale(&d, 0, rounding)?;
    }
    let negative = if d.is_zero() && sp.no_neg_0 {
        false
    } else {
        d.sign == 1
    };
    let digits = d.coef.to_digits();
    let len = digits.len() as i128;
    let leftdigits = d.exp as i128 + len;
    let dotplace = match ty {
        b'e' | b'E' => {
            if d.is_zero() && sp.precision.is_some() {
                1 - sp.precision? as i128
            } else {
                1
            }
        }
        b'f' | b'F' | b'%' => leftdigits,
        _ => {
            if d.exp <= 0 && leftdigits > -6 {
                leftdigits
            } else {
                1
            }
        }
    };
    // Keep the zero padding a formatter would write bounded.
    if dotplace.unsigned_abs() > MAX_DIGITS as u128 * 4 {
        return None;
    }
    let (intpart, mut fracpart) = if dotplace < 0 {
        let mut f = "0".repeat((-dotplace) as usize);
        f.push_str(&digits);
        ("0".to_owned(), f)
    } else if dotplace > len {
        let mut i = digits.clone();
        i.push_str(&"0".repeat((dotplace - len) as usize));
        (i, String::new())
    } else {
        let (a, b) = digits.split_at(dotplace as usize);
        (
            if a.is_empty() {
                "0".to_owned()
            } else {
                a.to_owned()
            },
            b.to_owned(),
        )
    };
    let exp = leftdigits - dotplace;
    // `_format_number`.
    let sign = if negative {
        "-"
    } else {
        match sp.sign {
            b'+' => "+",
            b' ' => " ",
            _ => "",
        }
    };
    if !fracpart.is_empty() && !sp.frac_separators.is_empty() {
        let groups: Vec<&str> = fracpart
            .as_bytes()
            .chunks(3)
            // SAFETY: ASCII digits.
            .map(|c| unsafe { std::str::from_utf8_unchecked(c) })
            .collect();
        fracpart = groups.join(sp.frac_separators);
    }
    if !fracpart.is_empty() || sp.alt {
        fracpart.insert(0, '.');
    }
    if exp != 0 || matches!(ty, b'e' | b'E') {
        let echar = if matches!(ty, b'E' | b'G') { 'E' } else { 'e' };
        fracpart.push(echar);
        if exp >= 0 {
            fracpart.push('+');
        }
        fracpart.push_str(&exp.to_string());
    }
    if ty == b'%' {
        fracpart.push('%');
    }
    let min_width = if sp.zeropad {
        sp.minimumwidth as isize - fracpart.len() as isize - sign.len() as isize
    } else {
        0
    };
    let intpart = insert_thousands_sep(&intpart, sp.thousands_sep, min_width);
    // `_format_align`.
    let body_len = intpart.len() + fracpart.len();
    let pad_len = sp.minimumwidth.saturating_sub(sign.len() + body_len);
    let mut out = String::with_capacity(sp.minimumwidth.max(body_len + 1) + 4);
    let pad = |out: &mut String, n: usize| {
        for _ in 0..n {
            out.push(sp.fill);
        }
    };
    match sp.align {
        b'<' => {
            out.push_str(sign);
            out.push_str(&intpart);
            out.push_str(&fracpart);
            pad(&mut out, pad_len);
        }
        b'=' => {
            out.push_str(sign);
            pad(&mut out, pad_len);
            out.push_str(&intpart);
            out.push_str(&fracpart);
        }
        b'^' => {
            let half = pad_len / 2;
            pad(&mut out, half);
            out.push_str(sign);
            out.push_str(&intpart);
            out.push_str(&fracpart);
            pad(&mut out, pad_len - half);
        }
        _ => {
            pad(&mut out, pad_len);
            out.push_str(sign);
            out.push_str(&intpart);
            out.push_str(&fracpart);
        }
    }
    Some(out)
}

/// `__format__(specifier, override=None)`.
fn d_format(a: &[Object]) -> Option<Result<Object, RuntimeError>> {
    if arg(a, 2).is_some_and(|o| !matches!(o, Object::None)) {
        return None;
    }
    let Object::Str(spec) = a.get(1)? else {
        return None;
    };
    let (st, x) = recv(a, 3)?;
    // `__format__` reads `getcontext()` first, whatever the specifier.
    let ctx = st.resolve(None)?;
    let s = format_fin(&x, spec.as_ref(), &ctx)?;
    ok(Object::Str(SharedStr::from(s.as_str())))
}

// ---------------------------------------------------------------------
// The interpreter's `str()`, `repr()`, `format()` and `hash()` of an
// exact `Decimal`, while its class is as [`install`] left it.

/// Whether `i` is an instance of a `Decimal` class [`install`] marked
/// (a cheap pre-check for the interpreter's hooks).
#[inline]
pub(crate) fn is_decimal_instance(i: &PyInstance) -> bool {
    // SAFETY: a read of the class pointer that runs no code.
    unsafe { i.class.peek() }.is_some_and(|c| c.native_kind.get() == KIND_DECIMAL)
}

/// The state of an exact, sealed `Decimal` instance.
#[inline]
fn sealed_state(i: &PyInstance) -> Option<&State> {
    // SAFETY: as above; the class outlives the instance's use here.
    let cls: &TypeObject = unsafe { i.class.peek() }?;
    let st = state_of_cls(cls)?;
    (std::ptr::eq(cls, st.decimal.as_ptr()) && sealed(st, cls)).then_some(st)
}

/// `str(d)`.
pub(crate) fn leaf_str(i: &PyInstance) -> Option<Object> {
    let st = sealed_state(i)?;
    let x = st.fin_of(i)?;
    let s = sci_string(st, &x, false, None)?;
    Some(Object::Str(SharedStr::from_ascii(&s)))
}

/// `repr(d)`.
pub(crate) fn leaf_repr(i: &PyInstance) -> Option<Object> {
    let st = sealed_state(i)?;
    let x = st.fin_of(i)?;
    let s = sci_string(st, &x, false, None)?;
    Some(Object::Str(SharedStr::from_ascii(&format!(
        "Decimal('{s}')"
    ))))
}

/// `format(d, spec)`.
pub(crate) fn leaf_format(i: &PyInstance, spec: &str) -> Option<String> {
    let st = sealed_state(i)?;
    let x = st.fin_of(i)?;
    let ctx = st.resolve(None)?;
    format_fin(&x, spec, &ctx)
}

/// `hash(d)` (pure: dictionary and set probes call it).
pub(crate) fn leaf_hash(i: &PyInstance) -> Option<i64> {
    let st = sealed_state(i)?;
    Some(hash(&st.fin_of(i)?))
}

// ---------------------------------------------------------------------
// The dispatch loop's shortcuts.

#[inline]
fn sealed(st: &State, cls: &TypeObject) -> bool {
    cls.attr_version.get() == st.sealed
}

/// `a OP b` for an exact `Decimal` left operand, or `None` (full path).
pub(crate) fn leaf_binop(
    op: weavepy_compiler::BinOpKind,
    a: &Object,
    b: &Object,
) -> Option<Result<Object, RuntimeError>> {
    use weavepy_compiler::BinOpKind as B;
    let Object::Instance(i) = a else { return None };
    let st = state_of_cls(i.cls_raw())?;
    if !sealed(st, i.cls_raw()) {
        return None;
    }
    // A right operand of another class (a subclass's reflected method
    // could win) takes the full path.
    if let Object::Instance(j) = b {
        if !st.is_decimal(j) {
            return None;
        }
    }
    let f: fn(&Fin, &Fin, &CtxP, &mut u16) -> Option<Fin> = match op {
        B::Add => add,
        B::Sub => |x, y, c, s| add(x, &y.negated(), c, s),
        B::Mult => mul,
        B::Div => div,
        B::FloorDiv => floordiv,
        B::Mod => rem,
        B::Pow => pow_op,
        _ => return None,
    };
    let x = st.fin(a)?;
    let y = st.operand(b)?;
    let ctx = st.resolve(None)?;
    let mut sig = 0;
    let r = f(&x, &y, &ctx.p, &mut sig)?;
    Some(Ok(st.finish(&ctx, sig, &r)?))
}

/// A keyword call of a native method of an exact `Decimal`, from the
/// dispatch loop's `CALL_KW` operands (the method, the receiver in the
/// self slot, `argc` positional arguments, the keyword values, and the
/// names tuple): the result, or `None` (nothing touched) for the full
/// handler.
pub(crate) fn method_kw(ops: &[Object], argc: usize) -> Option<Object> {
    let (Object::Builtin(b), recv @ Object::Instance(i)) = (ops.first()?, ops.get(1)?) else {
        return None;
    };
    let st = sealed_state(i)?;
    let spec = &SPECS[*st.methods.get(&(Rc::as_ptr(b) as usize))?];
    let Object::Tuple(names) = ops.last()? else {
        return None;
    };
    let n = spec.params.len() + 1;
    let args = ops.get(2..2 + argc)?;
    let kwv = ops.get(2 + argc..ops.len() - 1)?;
    if argc >= n || n > 6 || kwv.len() != names.len() {
        return None;
    }
    let mut v: [Object; 6] = std::array::from_fn(|_| Object::Unbound);
    v[0] = recv.clone();
    for (k, o) in args.iter().enumerate() {
        v[k + 1] = o.clone();
    }
    for (name, val) in names.iter().zip(kwv) {
        let Object::Str(name) = name else {
            return None;
        };
        let ix = spec
            .params
            .iter()
            .position(|p| *p == name.as_ref() as &str)?
            + 1;
        if !matches!(v[ix], Object::Unbound) {
            return None;
        }
        v[ix] = val.clone();
    }
    (spec.fast)(&v[..n])?.ok()
}

/// `a OP b` comparison for exact `Decimal` operands, or `None`.
pub(crate) fn leaf_compare(
    op: weavepy_compiler::CompareKind,
    a: &Object,
    b: &Object,
) -> Option<Result<Object, RuntimeError>> {
    use weavepy_compiler::CompareKind as C;
    let (Object::Instance(i), Object::Instance(j)) = (a, b) else {
        return None;
    };
    let st = state_of_cls(i.cls_raw())?;
    if !st.is_decimal(i) || !st.is_decimal(j) || !sealed(st, i.cls_raw()) {
        return None;
    }
    let o = cmp(&st.fin_of(i)?, &st.fin_of(j)?)?;
    Some(Ok(Object::Bool(match op {
        C::Eq => o.is_eq(),
        C::NotEq => !o.is_eq(),
        C::Lt => o.is_lt(),
        C::LtE => o.is_le(),
        C::Gt => o.is_gt(),
        C::GtE => o.is_ge(),
    })))
}

// ---------------------------------------------------------------------
// Installation.

fn with_interp<R>(
    f: impl FnOnce(&mut crate::Interpreter) -> Result<R, RuntimeError>,
) -> Result<R, RuntimeError> {
    let ptr = crate::vm_singletons::current_interpreter_ptr()
        .ok_or_else(|| type_error("decimal: no active interpreter"))?;
    // SAFETY: published by the enclosing VM frame on this thread.
    f(unsafe { &mut *ptr })
}

/// One native method: the class it lives on, its name, its parameters
/// after `self` (for keyword calls), its pure fast half, and its
/// `__text_signature__`.
struct Spec {
    class: u8,
    name: &'static str,
    params: &'static [&'static str],
    fast: Fast,
    sig: &'static str,
    classmethod: bool,
}

macro_rules! spec {
    ($class:expr, $name:literal, [$($p:literal),*], $fast:expr, $sig:literal) => {
        Spec {
            class: $class,
            name: $name,
            params: &[$($p),*],
            fast: $fast,
            sig: $sig,
            classmethod: false,
        }
    };
}

const D: u8 = C_DECIMAL;
const CX: u8 = C_CONTEXT;

const SPECS: &[Spec] = &[
    spec!(
        D,
        "__add__",
        ["other", "context"],
        d_add,
        "($self, /, other, context=None)"
    ),
    spec!(
        D,
        "__radd__",
        ["other", "context"],
        d_add,
        "($self, /, other, context=None)"
    ),
    spec!(
        D,
        "__sub__",
        ["other", "context"],
        d_sub,
        "($self, /, other, context=None)"
    ),
    spec!(
        D,
        "__rsub__",
        ["other", "context"],
        d_rsub,
        "($self, /, other, context=None)"
    ),
    spec!(
        D,
        "__mul__",
        ["other", "context"],
        d_mul,
        "($self, /, other, context=None)"
    ),
    spec!(
        D,
        "__rmul__",
        ["other", "context"],
        d_mul,
        "($self, /, other, context=None)"
    ),
    spec!(
        D,
        "__truediv__",
        ["other", "context"],
        d_truediv,
        "($self, /, other, context=None)"
    ),
    spec!(
        D,
        "__rtruediv__",
        ["other", "context"],
        d_rtruediv,
        "($self, /, other, context=None)"
    ),
    spec!(
        D,
        "__floordiv__",
        ["other", "context"],
        d_floordiv,
        "($self, /, other, context=None)"
    ),
    spec!(
        D,
        "__rfloordiv__",
        ["other", "context"],
        d_rfloordiv,
        "($self, /, other, context=None)"
    ),
    spec!(
        D,
        "__mod__",
        ["other", "context"],
        d_mod,
        "($self, /, other, context=None)"
    ),
    spec!(
        D,
        "__rmod__",
        ["other", "context"],
        d_rmod,
        "($self, /, other, context=None)"
    ),
    spec!(
        D,
        "__pow__",
        ["other", "modulo", "context"],
        d_pow,
        "($self, /, other, modulo=None, context=None)"
    ),
    spec!(
        D,
        "__rpow__",
        ["other", "modulo", "context"],
        d_rpow,
        "($self, /, other, modulo=None, context=None)"
    ),
    spec!(
        D,
        "__divmod__",
        ["other", "context"],
        d_divmod,
        "($self, /, other, context=None)"
    ),
    spec!(
        D,
        "__rdivmod__",
        ["other", "context"],
        d_rdivmod,
        "($self, /, other, context=None)"
    ),
    spec!(D, "__neg__", ["context"], d_neg, "($self, /, context=None)"),
    spec!(D, "__pos__", ["context"], d_pos, "($self, /, context=None)"),
    spec!(
        D,
        "__abs__",
        ["round", "context"],
        d_abs,
        "($self, /, round=True, context=None)"
    ),
    spec!(
        D,
        "__eq__",
        ["other", "context"],
        d_eq,
        "($self, /, other, context=None)"
    ),
    spec!(
        D,
        "__lt__",
        ["other", "context"],
        d_lt,
        "($self, /, other, context=None)"
    ),
    spec!(
        D,
        "__le__",
        ["other", "context"],
        d_le,
        "($self, /, other, context=None)"
    ),
    spec!(
        D,
        "__gt__",
        ["other", "context"],
        d_gt,
        "($self, /, other, context=None)"
    ),
    spec!(
        D,
        "__ge__",
        ["other", "context"],
        d_ge,
        "($self, /, other, context=None)"
    ),
    spec!(D, "__hash__", [], d_hash, "($self, /)"),
    spec!(D, "__bool__", [], d_bool, "($self, /)"),
    spec!(
        D,
        "__str__",
        ["eng", "context"],
        d_str,
        "($self, /, eng=False, context=None)"
    ),
    spec!(D, "__repr__", [], d_repr, "($self, /)"),
    spec!(
        D,
        "__format__",
        ["specifier", "override"],
        d_format,
        "($self, specifier, override=None, /)"
    ),
    spec!(D, "__float__", [], d_float, "($self, /)"),
    spec!(D, "__int__", [], d_int, "($self, /)"),
    spec!(D, "__trunc__", [], d_int, "($self, /)"),
    spec!(D, "__floor__", [], d_floor, "($self, /)"),
    spec!(D, "__ceil__", [], d_ceil, "($self, /)"),
    spec!(D, "__round__", ["n"], d_round, "($self, /, n=None)"),
    spec!(
        D,
        "quantize",
        ["exp", "rounding", "context"],
        d_quantize,
        "($self, /, exp, rounding=None, context=None)"
    ),
    spec!(
        D,
        "to_integral",
        ["rounding", "context"],
        d_to_integral_value,
        "($self, /, rounding=None, context=None)"
    ),
    spec!(
        D,
        "to_integral_value",
        ["rounding", "context"],
        d_to_integral_value,
        "($self, /, rounding=None, context=None)"
    ),
    spec!(
        D,
        "to_integral_exact",
        ["rounding", "context"],
        d_to_integral_exact,
        "($self, /, rounding=None, context=None)"
    ),
    spec!(
        D,
        "to_eng_string",
        ["context"],
        d_to_eng_string,
        "($self, /, context=None)"
    ),
    spec!(D, "adjusted", [], d_adjusted, "($self, /)"),
    spec!(D, "copy_abs", [], d_copy_abs, "($self, /)"),
    spec!(D, "copy_negate", [], d_copy_negate, "($self, /)"),
    spec!(
        D,
        "copy_sign",
        ["other", "context"],
        d_copy_sign,
        "($self, /, other, context=None)"
    ),
    spec!(
        D,
        "compare",
        ["other", "context"],
        d_compare,
        "($self, /, other, context=None)"
    ),
    spec!(D, "is_finite", [], d_is_finite, "($self, /)"),
    spec!(D, "is_infinite", [], d_is_not, "($self, /)"),
    spec!(D, "is_nan", [], d_is_not, "($self, /)"),
    spec!(D, "is_qnan", [], d_is_not, "($self, /)"),
    spec!(D, "is_snan", [], d_is_not, "($self, /)"),
    spec!(D, "is_signed", [], d_is_signed, "($self, /)"),
    spec!(D, "is_zero", [], d_is_zero, "($self, /)"),
    spec!(
        D,
        "is_normal",
        ["context"],
        d_is_normal,
        "($self, /, context=None)"
    ),
    spec!(
        D,
        "is_subnormal",
        ["context"],
        d_is_subnormal,
        "($self, /, context=None)"
    ),
    spec!(D, "as_tuple", [], d_as_tuple, "($self, /)"),
    spec!(D, "as_integer_ratio", [], d_as_integer_ratio, "($self, /)"),
    spec!(
        D,
        "same_quantum",
        ["other", "context"],
        d_same_quantum,
        "($self, /, other, context=None)"
    ),
    spec!(
        D,
        "normalize",
        ["context"],
        d_normalize,
        "($self, /, context=None)"
    ),
    spec!(
        D,
        "max",
        ["other", "context"],
        d_max,
        "($self, /, other, context=None)"
    ),
    spec!(
        D,
        "min",
        ["other", "context"],
        d_min,
        "($self, /, other, context=None)"
    ),
    Spec {
        class: D,
        name: "from_float",
        params: &[],
        fast: d_from_float,
        sig: "($type, f, /)",
        classmethod: true,
    },
    spec!(CX, "add", [], c_add, "($self, a, b, /)"),
    spec!(CX, "subtract", [], c_subtract, "($self, a, b, /)"),
    spec!(CX, "multiply", [], c_multiply, "($self, a, b, /)"),
    spec!(CX, "divide", [], c_divide, "($self, a, b, /)"),
    spec!(CX, "divide_int", [], c_divide_int, "($self, a, b, /)"),
    spec!(CX, "remainder", [], c_remainder, "($self, a, b, /)"),
    spec!(CX, "divmod", [], c_divmod, "($self, a, b, /)"),
    spec!(CX, "quantize", [], c_quantize, "($self, a, b, /)"),
    spec!(CX, "plus", [], c_plus, "($self, a, /)"),
    spec!(CX, "minus", [], c_minus, "($self, a, /)"),
    spec!(CX, "abs", [], c_abs, "($self, a, /)"),
    spec!(CX, "compare", [], c_compare, "($self, a, b, /)"),
    spec!(CX, "to_integral", [], c_to_integral_value, "($self, a, /)"),
    spec!(
        CX,
        "to_integral_value",
        [],
        c_to_integral_value,
        "($self, a, /)"
    ),
    spec!(
        CX,
        "to_integral_exact",
        [],
        c_to_integral_exact,
        "($self, a, /)"
    ),
    spec!(CX, "to_sci_string", [], c_to_sci_string, "($self, a, /)"),
    spec!(
        CX,
        "create_decimal",
        [],
        c_create_decimal,
        "($self, num='0', /)"
    ),
    spec!(
        CX,
        "create_decimal_from_float",
        [],
        c_create_decimal_from_float,
        "($self, f, /)"
    ),
];

/// A keyword call of a native method: the positional argument list
/// (receiver first, `Unbound` for an omitted parameter), or `None` for
/// the replaced Python method (unknown or repeated names).
fn kw_positional(params: &[&str], a: &[Object], kw: &[(String, Object)]) -> Option<Vec<Object>> {
    let (recv, pos) = a.split_first()?;
    if pos.len() > params.len() {
        return None;
    }
    let mut out = Vec::with_capacity(params.len() + 1);
    out.push(recv.clone());
    out.extend(pos.iter().cloned());
    out.resize(params.len() + 1, Object::Unbound);
    for (name, v) in kw {
        let ix = params.iter().position(|p| p == name)? + 1;
        if !matches!(out[ix], Object::Unbound) {
            return None;
        }
        out[ix] = v.clone();
    }
    Some(out)
}

/// `install(Decimal, Context, SignalDict, DecimalTuple, signals,
/// context_var, states, rounding_modes, getcontext)`: put the native
/// methods on the exact classes (see the module docs), and return the
/// native `getcontext` (or `None` when the classes were already
/// installed).
pub(crate) fn install(args: &[Object]) -> Result<Object, RuntimeError> {
    let [Object::Type(dec), Object::Type(ctx), Object::Type(sd), Object::Type(dt), Object::List(signals), var, states @ Object::Dict(_), Object::Tuple(roundings), getcontext] =
        args
    else {
        return Err(type_error("install() expects the decimal classes"));
    };
    let roundings: Vec<SharedStr> = roundings
        .iter()
        .filter_map(|o| match o {
            Object::Str(s) => Some(s.clone()),
            _ => None,
        })
        .collect();
    if roundings.len() != ROUND_NAMES.len()
        || roundings
            .iter()
            .zip(ROUND_NAMES.iter())
            .any(|(s, (n, _))| s.as_ref() as &str != *n)
    {
        return Err(type_error("install(): expected the rounding modes"));
    }
    if dec.native_ext.get().is_some() || ctx.native_ext.get().is_some() {
        return Ok(Object::None);
    }
    let signals: Vec<Object> = signals.borrow().clone();
    if signals.len() != N_SIGNALS || !signals.iter().all(|s| matches!(s, Object::Type(_))) {
        return Err(type_error("install(): expected the nine signals"));
    }
    let classes: [(u8, &Rc<TypeObject>); 2] = [(C_DECIMAL, dec), (C_CONTEXT, ctx)];
    let mut orig = HashMap::new();
    for spec in SPECS {
        let cls = classes[spec.class as usize].1;
        let own = cls
            .dict
            .borrow()
            .get(&crate::object::StrKey(spec.name))
            .cloned();
        let Some(v) = own.or_else(|| cls.lookup(spec.name)) else {
            return Err(type_error(format!(
                "install(): {} has no {}",
                cls.name, spec.name
            )));
        };
        let v = match (&v, spec.classmethod) {
            (Object::ClassMethod(w), true) => w.func(),
            (_, true) => return Err(type_error("install(): expected a classmethod")),
            _ => v,
        };
        orig.insert((spec.class, spec.name), v);
    }
    let names = Names::new();
    let layout = crate::stdlib::datetime_native::decimal_layout().clone();
    let mut state = State {
        decimal: Rc::downgrade(dec),
        context: Rc::downgrade(ctx),
        signal_dict: Rc::downgrade(sd),
        decimal_tuple: Rc::downgrade(dt),
        signals,
        var: var.clone(),
        states: states.clone(),
        roundings,
        names,
        layout,
        orig,
        sealed: 0,
        owners: RefCell::new(Vec::new()),
        pool: Pool::default(),
        checked: RefCell::new(None),
        states_hint: AtomicUsize::new(0),
        data_hint: AtomicUsize::new(0),
        ctx_keys: AtomicUsize::new(0),
        ctx_epoch: AtomicU64::new(1),
        ctx_cache: RefCell::new(None),
        methods: HashMap::new(),
    };
    // The natives go in first, then the class version they leave is the
    // sealed one.
    let mut natives: Vec<(u8, &'static str, Object)> = Vec::new();
    let pending: Rc<std::sync::OnceLock<Rc<State>>> = Rc::new(std::sync::OnceLock::new());
    for (ix, spec) in SPECS.iter().enumerate() {
        let fast = spec.fast;
        let key = (spec.class, spec.name);
        let params = spec.params;
        let st = pending.clone();
        let st_kw = pending.clone();
        let b = Rc::new(BuiltinFn {
            name: spec.name,
            binds_instance: !spec.classmethod,
            call: Box::new(move |a: &[Object]| match fast(a) {
                Some(r) => r,
                None => {
                    let f = st
                        .get()
                        .and_then(|s| s.orig.get(&key).cloned())
                        .unwrap_or(Object::None);
                    with_interp(|i| i.call_object(f, a, &[]))
                }
            }),
            call_kw: Some(Box::new(move |a: &[Object], kw: &[(String, Object)]| {
                if kw.is_empty() {
                    if let Some(r) = fast(a) {
                        return r;
                    }
                } else if let Some(r) = kw_positional(params, a, kw).and_then(|a| fast(&a)) {
                    return r;
                }
                let f = st_kw
                    .get()
                    .and_then(|s| s.orig.get(&key).cloned())
                    .unwrap_or(Object::None);
                with_interp(|i| i.call_object(f, a, kw))
            })),
        });
        crate::leaf_builtins::register_fast(&b, fast);
        if spec.class == C_DECIMAL && !spec.classmethod {
            state.methods.insert(Rc::as_ptr(&b) as usize, ix);
        }
        let value = Object::Builtin(b.clone());
        crate::descr_registry::register_text_signature(&value, spec.sig);
        let value = if spec.classmethod {
            Object::ClassMethod(crate::object::MethodWrapper::new(value))
        } else {
            value
        };
        natives.push((spec.class, spec.name, value));
    }
    for (class, name, value) in natives {
        let cls = classes[class as usize].1;
        cls.dict
            .borrow_mut()
            .insert(DictKey(Object::Str(interned(name))), value);
    }
    for (_, cls) in &classes {
        cls.bump_attr_version();
    }
    state.sealed = dec.attr_version.get();
    let state = Rc::new(state);
    let _ = pending.set(state.clone());
    for (_, cls) in &classes {
        let _ = cls
            .native_ext
            .set(crate::rc_unsize!(state.clone() => dyn std::any::Any + Send + Sync));
    }
    dec.native_kind.set(KIND_DECIMAL);
    // `getcontext()`: the current context when it exists, else the Python
    // function (which creates one).
    let (st_call, orig_call) = (state.clone(), getcontext.clone());
    let (st_kw, orig_kw) = (state.clone(), getcontext.clone());
    let native_getcontext = Object::Builtin(Rc::new(BuiltinFn {
        name: "getcontext",
        binds_instance: false,
        call: Box::new(move |a: &[Object]| {
            if a.is_empty() {
                if let Some(c) = st_call.current() {
                    return Ok(Object::Instance(c));
                }
            }
            with_interp(|i| i.call_object(orig_call.clone(), a, &[]))
        }),
        call_kw: Some(Box::new(move |a: &[Object], kw: &[(String, Object)]| {
            if a.is_empty() && kw.is_empty() {
                if let Some(c) = st_kw.current() {
                    return Ok(Object::Instance(c));
                }
            }
            with_interp(|i| i.call_object(orig_kw.clone(), a, kw))
        })),
    }));
    crate::descr_registry::register_module(&native_getcontext, "decimal");
    crate::descr_registry::register_text_signature(&native_getcontext, "()");
    // `_context_changed()`: `Context.__setattr__`'s note that a context
    // attribute changed (see `State::read_ctx`).
    let st_changed = state.clone();
    let changed = Rc::new(BuiltinFn {
        name: "_context_changed",
        binds_instance: false,
        call: Box::new(move |_: &[Object]| {
            st_changed.ctx_epoch.fetch_add(1, Relaxed);
            Ok(Object::None)
        }),
        call_kw: None,
    });
    crate::leaf_builtins::register(&changed);
    Ok(Object::new_tuple_array([
        native_getcontext,
        Object::Builtin(changed),
    ]))
}

/// `_weave_decimal`: the installer.
pub(crate) fn build(_cache: &ModuleCache) -> Rc<PyModule> {
    let mut dict = DictData::default();
    dict.insert(
        DictKey(Object::from_static("__name__")),
        Object::from_static("_weave_decimal"),
    );
    let function = Object::Builtin(Rc::new(BuiltinFn {
        name: "install",
        binds_instance: false,
        call: Box::new(install),
        call_kw: None,
    }));
    crate::descr_registry::register_module(&function, "_weave_decimal");
    dict.insert(DictKey(Object::from_static("install")), function);
    Rc::new(PyModule {
        name: "_weave_decimal".to_owned(),
        filename: None,
        dict: Rc::new(RefCell::new(dict)),
    })
}

/// Whether a dying exact `Decimal` may simply be dropped: its slots hold
/// only ints, strings and bools.
pub(crate) fn plain_drop_ok(_i: &PyInstance) -> bool {
    true
}

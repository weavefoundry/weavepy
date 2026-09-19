//! Native fast paths for the `datetime` types.
//!
//! WeavePy's `datetime` classes are the pure-Python `_pydatetime` ones
//! (which also serve as `_datetime`). CPython runs their operations in C;
//! interpreted, one `datetime + timedelta` is a dozen Python calls. This
//! module gives the hot operations — arithmetic, comparisons, the field
//! getters, `weekday`/`toordinal`, `isoformat`, `strftime` with numeric
//! directives, `fromisoformat` for the common shapes, `date.replace` and
//! `datetime.date()` — native bodies that work directly on the classes'
//! `__slots__` storage.
//!
//! [`install`] (called at the end of `_pydatetime`'s module body) marks the
//! *exact* classes with a [`TypeObject::native_kind`], puts the native
//! callables in their class dicts, and records the resulting class
//! versions. A native body serves only exact-class operands whose fields
//! hold plain ints and whose `tzinfo` is `None` or an exact `timezone`;
//! anything else — subclasses, custom `tzinfo`s, unusual arguments, and
//! every error whose message the Python code owns — calls the original
//! Python method it replaced, so observable behavior is unchanged. The
//! pure fast halves are registered as leaf entries, so the dispatch loop
//! runs them inline; the loop's operator and field shortcuts
//! ([`leaf_binop`], [`leaf_compare`], [`leaf_field`]) apply only while a
//! class is in the state [`install`] left it in.
//!
//! Exact-class instances are never cycle-collector tracked: CPython's C
//! datetime types are not GC types either (a cycle through `tzinfo` is
//! uncollectable there too), and their fields are ints, `None` and the
//! `tzinfo` reference.

use std::collections::HashMap;
use std::fmt::Write as _;

use crate::error::{overflow_error, type_error, RuntimeError};
use crate::object::{BuiltinFn, DictKey, Object};
use crate::shared_value::SharedStr;
use crate::sync::{Rc, RefCell, Weak};
use crate::types::{PyInstance, SlotStorage, TypeObject};

pub(crate) const KIND_TIMEDELTA: u8 = 1;
pub(crate) const KIND_DATE: u8 = 2;
pub(crate) const KIND_TIME: u8 = 3;
pub(crate) const KIND_DATETIME: u8 = 4;
pub(crate) const KIND_TIMEZONE: u8 = 5;

const MAX_ORDINAL: i64 = 3_652_059;
const MAX_DAYS: i128 = 999_999_999;
const US_PER_DAY: i128 = 86_400_000_000;

// Slot positions as the classes' `__new__` assigns them. They are hints:
// a lookup falls back to the name for an instance built another way
// (unpickling, `copy`).
const TD_DAYS: usize = 0;
const TD_SECONDS: usize = 1;
const TD_US: usize = 2;
const D_YEAR: usize = 0;
const D_MONTH: usize = 1;
const D_DAY: usize = 2;
const DT_HOUR: usize = 3;
const DT_MINUTE: usize = 4;
const DT_SECOND: usize = 5;
const DT_US: usize = 6;
const DT_TZINFO: usize = 7;
const DT_FOLD: usize = 9;
const TZ_OFFSET: usize = 0;

/// Interned slot and field names.
struct Names {
    days: SharedStr,
    seconds: SharedStr,
    microseconds: SharedStr,
    hashcode: SharedStr,
    pub_days: SharedStr,
    pub_seconds: SharedStr,
    pub_microseconds: SharedStr,
    year: SharedStr,
    month: SharedStr,
    day: SharedStr,
    hour: SharedStr,
    minute: SharedStr,
    second: SharedStr,
    microsecond: SharedStr,
    tzinfo: SharedStr,
    fold: SharedStr,
    offset: SharedStr,
    name: SharedStr,
    // Public field names served by the property getters.
    f_year: SharedStr,
    f_month: SharedStr,
    f_day: SharedStr,
    f_hour: SharedStr,
    f_minute: SharedStr,
    f_second: SharedStr,
    f_microsecond: SharedStr,
    f_tzinfo: SharedStr,
    f_fold: SharedStr,
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
            days: interned("_days"),
            seconds: interned("_seconds"),
            microseconds: interned("_microseconds"),
            hashcode: interned("_hashcode"),
            pub_days: interned("days"),
            pub_seconds: interned("seconds"),
            pub_microseconds: interned("microseconds"),
            year: interned("_year"),
            month: interned("_month"),
            day: interned("_day"),
            hour: interned("_hour"),
            minute: interned("_minute"),
            second: interned("_second"),
            microsecond: interned("_microsecond"),
            tzinfo: interned("_tzinfo"),
            fold: interned("_fold"),
            offset: interned("_offset"),
            name: interned("_name"),
            f_year: interned("year"),
            f_month: interned("month"),
            f_day: interned("day"),
            f_hour: interned("hour"),
            f_minute: interned("minute"),
            f_second: interned("second"),
            f_microsecond: interned("microsecond"),
            f_tzinfo: interned("tzinfo"),
            f_fold: interned("fold"),
        }
    }
}

/// One interpreter's `_pydatetime` classes (held weakly: the natives live
/// in their dicts) and the Python methods the natives replaced.
struct State {
    timedelta: Weak<TypeObject>,
    date: Weak<TypeObject>,
    datetime: Weak<TypeObject>,
    timezone: Weak<TypeObject>,
    utc: Weak<PyInstance>,
    names: Names,
    /// The slot layouts natively built instances share (see
    /// `SlotStorage::from_layout`), in the order `new_td` / `new_date` /
    /// `new_dt` fill them.
    td_layout: Rc<[DictKey]>,
    date_layout: Rc<[DictKey]>,
    dt_layout: Rc<[DictKey]>,
    /// `(kind, name)` → the replaced Python implementation.
    orig: HashMap<(u8, &'static str), Object>,
    /// The `attr_version` at which each exact class was last verified to
    /// resolve the shortcut names (see [`verified`]) to what [`install`]
    /// left there; `0` = never.
    sealed: [std::sync::atomic::AtomicU64; 6],
    /// `(kind, name)` → what the class resolved the name to after
    /// [`install`]: the natives, and the field properties.
    expect: std::sync::OnceLock<Vec<(u8, &'static str, Object)>>,
}

/// The dunders and fields the dispatch-loop shortcuts serve without a
/// lookup, per class kind.
const SHORTCUT_NAMES: &[(u8, &str)] = &[
    (KIND_TIMEDELTA, "__add__"),
    (KIND_TIMEDELTA, "__sub__"),
    (KIND_TIMEDELTA, "__mul__"),
    (KIND_TIMEDELTA, "__eq__"),
    (KIND_TIMEDELTA, "__ne__"),
    (KIND_TIMEDELTA, "__lt__"),
    (KIND_TIMEDELTA, "__le__"),
    (KIND_TIMEDELTA, "__gt__"),
    (KIND_TIMEDELTA, "__ge__"),
    (KIND_DATE, "__add__"),
    (KIND_DATE, "__sub__"),
    (KIND_DATE, "__eq__"),
    (KIND_DATE, "__ne__"),
    (KIND_DATE, "__lt__"),
    (KIND_DATE, "__le__"),
    (KIND_DATE, "__gt__"),
    (KIND_DATE, "__ge__"),
    (KIND_DATE, "year"),
    (KIND_DATE, "month"),
    (KIND_DATE, "day"),
    (KIND_DATETIME, "__add__"),
    (KIND_DATETIME, "__sub__"),
    (KIND_DATETIME, "__eq__"),
    (KIND_DATETIME, "__ne__"),
    (KIND_DATETIME, "__lt__"),
    (KIND_DATETIME, "__le__"),
    (KIND_DATETIME, "__gt__"),
    (KIND_DATETIME, "__ge__"),
    (KIND_DATETIME, "year"),
    (KIND_DATETIME, "month"),
    (KIND_DATETIME, "day"),
    (KIND_DATETIME, "hour"),
    (KIND_DATETIME, "minute"),
    (KIND_DATETIME, "second"),
    (KIND_DATETIME, "microsecond"),
    (KIND_DATETIME, "tzinfo"),
    (KIND_DATETIME, "fold"),
];

/// Whether `cls` (of `kind`) still resolves every shortcut name to what
/// [`install`] left there. Checked once per class version: a version
/// change re-verifies (other class-dict edits keep the shortcuts), a
/// replaced dunder or property turns them off for that version.
#[inline]
fn verified(st: &State, cls: &TypeObject, kind: u8) -> bool {
    use std::sync::atomic::Ordering::Relaxed;
    let ver = cls.attr_version.get();
    if st.sealed[kind as usize].load(Relaxed) == ver {
        return true;
    }
    verify_slow(st, cls, kind, ver)
}

#[cold]
#[inline(never)]
fn verify_slow(st: &State, cls: &TypeObject, kind: u8, ver: u64) -> bool {
    let Some(expect) = st.expect.get() else {
        return false;
    };
    let ok = expect
        .iter()
        .filter(|(k, _, _)| *k == kind)
        .all(|(_, name, want)| cls.lookup(name).is_some_and(|got| got.is_same(want)));
    if ok {
        st.sealed[kind as usize].store(ver, std::sync::atomic::Ordering::Relaxed);
    }
    ok
}

// SAFETY-free: `State` is reached from an exact class through its
// `native_ext` slot (see `state_of`).
fn state_of_cls(cls: &TypeObject) -> Option<&State> {
    cls.native_ext.get()?.downcast_ref::<State>()
}

#[inline]
fn kind_of(o: &Object) -> u8 {
    match o {
        Object::Instance(i) => i.cls_raw().native_kind.get(),
        _ => 0,
    }
}

#[inline]
fn inst(o: &Object, kind: u8) -> Option<&Rc<PyInstance>> {
    match o {
        Object::Instance(i) if i.cls_raw().native_kind.get() == kind => Some(i),
        _ => None,
    }
}

#[inline]
fn state_of(o: &Object) -> Option<&State> {
    match o {
        Object::Instance(i) => state_of_cls(i.cls_raw()),
        _ => None,
    }
}

#[inline]
fn as_int(o: &Object) -> Option<i64> {
    match o {
        Object::Int(v) => Some(*v),
        Object::Bool(b) => Some(i64::from(*b)),
        _ => None,
    }
}

fn td_fields(i: &PyInstance, n: &Names) -> Option<(i64, i64, i64)> {
    let s = i.slots.try_borrow().ok()?;
    Some((
        as_int(s.get_hinted(TD_DAYS, &n.days)?)?,
        as_int(s.get_hinted(TD_SECONDS, &n.seconds)?)?,
        as_int(s.get_hinted(TD_US, &n.microseconds)?)?,
    ))
}

fn td_us(f: (i64, i64, i64)) -> i128 {
    (i128::from(f.0) * 86_400 + i128::from(f.1)) * 1_000_000 + i128::from(f.2)
}

fn date_fields(i: &PyInstance, n: &Names) -> Option<(i64, i64, i64)> {
    let s = i.slots.try_borrow().ok()?;
    Some((
        as_int(s.get_hinted(D_YEAR, &n.year)?)?,
        as_int(s.get_hinted(D_MONTH, &n.month)?)?,
        as_int(s.get_hinted(D_DAY, &n.day)?)?,
    ))
}

struct Dt {
    y: i64,
    m: i64,
    d: i64,
    hh: i64,
    mm: i64,
    ss: i64,
    us: i64,
    tz: Object,
    fold: i64,
}

fn dt_fields(i: &PyInstance, n: &Names) -> Option<Dt> {
    let s = i.slots.try_borrow().ok()?;
    Some(Dt {
        y: as_int(s.get_hinted(D_YEAR, &n.year)?)?,
        m: as_int(s.get_hinted(D_MONTH, &n.month)?)?,
        d: as_int(s.get_hinted(D_DAY, &n.day)?)?,
        hh: as_int(s.get_hinted(DT_HOUR, &n.hour)?)?,
        mm: as_int(s.get_hinted(DT_MINUTE, &n.minute)?)?,
        ss: as_int(s.get_hinted(DT_SECOND, &n.second)?)?,
        us: as_int(s.get_hinted(DT_US, &n.microsecond)?)?,
        tz: s.get_hinted(DT_TZINFO, &n.tzinfo)?.clone(),
        fold: as_int(s.get_hinted(DT_FOLD, &n.fold)?)?,
    })
}

/// A `tzinfo` the natives understand: none, or an exact `timezone`
/// (its fixed offset in microseconds). `None` for anything else.
enum Tz {
    Naive,
    Fixed(i128),
}

fn tz_of(tz: &Object, n: &Names) -> Option<Tz> {
    match tz {
        Object::None => Some(Tz::Naive),
        o => {
            let i = inst(o, KIND_TIMEZONE)?;
            let off = {
                let s = i.slots.try_borrow().ok()?;
                s.get_hinted(TZ_OFFSET, &n.offset)?.clone()
            };
            let td = inst(&off, KIND_TIMEDELTA)?;
            Some(Tz::Fixed(td_us(td_fields(td, n)?)))
        }
    }
}

// ---------------------------------------------------------------------
// Calendar arithmetic (proleptic Gregorian, ordinal 1 = 0001-01-01).

const DAYS_IN_MONTH: [i64; 12] = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
const DAYS_BEFORE_MONTH: [i64; 12] = [0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334];

fn is_leap(y: i64) -> bool {
    y % 4 == 0 && (y % 100 != 0 || y % 400 == 0)
}

fn days_in_month(y: i64, m: i64) -> i64 {
    DAYS_IN_MONTH[(m - 1) as usize] + i64::from(m == 2 && is_leap(y))
}

fn ymd2ord(y: i64, m: i64, d: i64) -> i64 {
    let y1 = y - 1;
    y1 * 365 + y1 / 4 - y1 / 100
        + y1 / 400
        + DAYS_BEFORE_MONTH[(m - 1) as usize]
        + i64::from(m > 2 && is_leap(y))
        + d
}

fn ord2ymd(n: i64) -> (i64, i64, i64) {
    let n = n - 1;
    let n400 = n / 146_097;
    let n = n % 146_097;
    let n100 = n / 36_524;
    let n = n % 36_524;
    let n4 = n / 1461;
    let n = n % 1461;
    let n1 = n / 365;
    let mut rem = n % 365;
    let mut year = n400 * 400 + n100 * 100 + n4 * 4 + n1 + 1;
    if n1 == 4 || n100 == 4 {
        year -= 1;
        return (year, 12, 31);
    }
    let leap = is_leap(year);
    let mut month = 1;
    for dim in DAYS_IN_MONTH {
        let dim = dim + i64::from(month == 2 && leap);
        if rem < dim {
            break;
        }
        rem -= dim;
        month += 1;
    }
    (year, month, rem + 1)
}

fn valid_date(y: i64, m: i64, d: i64) -> bool {
    (1..=9999).contains(&y) && (1..=12).contains(&m) && d >= 1 && d <= days_in_month(y, m)
}

// ---------------------------------------------------------------------
// Construction.

fn entry(k: &SharedStr, v: Object) -> (DictKey, Object) {
    (DictKey(Object::Str(k.clone())), v)
}

fn instance(cls: Rc<TypeObject>, entries: Vec<(DictKey, Object)>) -> Object {
    let mut i = PyInstance::new(cls);
    i.slots = RefCell::new(SlotStorage::from_entries(entries));
    Object::Instance(Rc::new(i))
}

/// [`instance`] over one of the shared layouts (see `State`).
fn instance_fixed(cls: Rc<TypeObject>, layout: &Rc<[DictKey]>, values: Vec<Object>) -> Object {
    let mut i = PyInstance::new(cls);
    i.slots = RefCell::new(SlotStorage::from_layout(layout.clone(), values));
    Object::Instance(Rc::new(i))
}

/// A normalized exact `timedelta` from unnormalized components.
fn new_td(st: &State, d: i128, s: i128, us: i128) -> Option<Result<Object, RuntimeError>> {
    let total = d * US_PER_DAY + s * 1_000_000 + us;
    let days = total.div_euclid(US_PER_DAY);
    let rest = total.rem_euclid(US_PER_DAY);
    if days.abs() > MAX_DAYS {
        return Some(Err(overflow_error(format!(
            "days={days}; must have magnitude <= 999999999"
        ))));
    }
    let (days, secs, us) = (
        days as i64,
        (rest / 1_000_000) as i64,
        (rest % 1_000_000) as i64,
    );
    let cls = st.timedelta.upgrade()?;
    Some(Ok(instance_fixed(
        cls,
        &st.td_layout,
        vec![
            Object::Int(days),
            Object::Int(secs),
            Object::Int(us),
            Object::Int(-1),
            Object::Int(days),
            Object::Int(secs),
            Object::Int(us),
        ],
    )))
}

fn new_date(st: &State, y: i64, m: i64, d: i64) -> Option<Object> {
    let cls = st.date.upgrade()?;
    Some(instance_fixed(
        cls,
        &st.date_layout,
        vec![Object::Int(y), Object::Int(m), Object::Int(d), Object::Int(-1)],
    ))
}

#[allow(clippy::too_many_arguments)]
fn new_dt(st: &State, f: &Dt) -> Option<Object> {
    let cls = st.datetime.upgrade()?;
    Some(instance_fixed(
        cls,
        &st.dt_layout,
        vec![
            Object::Int(f.y),
            Object::Int(f.m),
            Object::Int(f.d),
            Object::Int(f.hh),
            Object::Int(f.mm),
            Object::Int(f.ss),
            Object::Int(f.us),
            f.tz.clone(),
            Object::Int(-1),
            Object::Int(f.fold),
        ],
    ))
}

/// `datetime` fields shifted by `delta_us` (fold cleared, `tzinfo`
/// kept), or `None` past the representable range.
fn dt_shift(f: &Dt, delta_us: i128) -> Option<Dt> {
    let base = (i128::from(ymd2ord(f.y, f.m, f.d)) * 86_400
        + i128::from(f.hh * 3600 + f.mm * 60 + f.ss))
        * 1_000_000
        + i128::from(f.us);
    let total = base + delta_us;
    let ord = total.div_euclid(US_PER_DAY);
    if !(1..=i128::from(MAX_ORDINAL)).contains(&ord) {
        return None;
    }
    let rest = total.rem_euclid(US_PER_DAY);
    let (y, m, d) = ord2ymd(ord as i64);
    let secs = (rest / 1_000_000) as i64;
    Some(Dt {
        y,
        m,
        d,
        hh: secs / 3600,
        mm: secs % 3600 / 60,
        ss: secs % 60,
        us: (rest % 1_000_000) as i64,
        tz: f.tz.clone(),
        fold: 0,
    })
}

/// Microseconds since 0001-01-01T00:00, naive.
fn dt_us(f: &Dt) -> i128 {
    (i128::from(ymd2ord(f.y, f.m, f.d)) * 86_400 + i128::from(f.hh * 3600 + f.mm * 60 + f.ss))
        * 1_000_000
        + i128::from(f.us)
}

// ---------------------------------------------------------------------
// The pure fast halves. `None` means "not this shape": the caller runs
// the replaced Python method (or, inline, the full dispatch path).

type Fast = fn(&[Object]) -> Option<Result<Object, RuntimeError>>;

fn td_add(a: &[Object]) -> Option<Result<Object, RuntimeError>> {
    let [x, y] = a else { return None };
    let st = state_of(x)?;
    let p = td_fields(inst(x, KIND_TIMEDELTA)?, &st.names)?;
    let q = td_fields(inst(y, KIND_TIMEDELTA)?, &st.names)?;
    new_td(
        st,
        i128::from(p.0) + i128::from(q.0),
        i128::from(p.1) + i128::from(q.1),
        i128::from(p.2) + i128::from(q.2),
    )
}

fn td_sub(a: &[Object]) -> Option<Result<Object, RuntimeError>> {
    let [x, y] = a else { return None };
    let st = state_of(x)?;
    let p = td_fields(inst(x, KIND_TIMEDELTA)?, &st.names)?;
    let q = td_fields(inst(y, KIND_TIMEDELTA)?, &st.names)?;
    new_td(
        st,
        i128::from(p.0) - i128::from(q.0),
        i128::from(p.1) - i128::from(q.1),
        i128::from(p.2) - i128::from(q.2),
    )
}

fn td_neg(a: &[Object]) -> Option<Result<Object, RuntimeError>> {
    let [x] = a else { return None };
    let st = state_of(x)?;
    let p = td_fields(inst(x, KIND_TIMEDELTA)?, &st.names)?;
    new_td(st, -i128::from(p.0), -i128::from(p.1), -i128::from(p.2))
}

fn td_mul(a: &[Object]) -> Option<Result<Object, RuntimeError>> {
    let [x, k] = a else { return None };
    let k = i128::from(as_int(k)?);
    let st = state_of(x)?;
    let p = td_fields(inst(x, KIND_TIMEDELTA)?, &st.names)?;
    new_td(
        st,
        i128::from(p.0) * k,
        i128::from(p.1) * k,
        i128::from(p.2) * k,
    )
}

fn td_bool(a: &[Object]) -> Option<Result<Object, RuntimeError>> {
    let [x] = a else { return None };
    let st = state_of(x)?;
    let p = td_fields(inst(x, KIND_TIMEDELTA)?, &st.names)?;
    Some(Ok(Object::Bool(p != (0, 0, 0))))
}

fn td_cmp(a: &[Object]) -> Option<std::cmp::Ordering> {
    let [x, y] = a else { return None };
    let st = state_of(x)?;
    let p = td_fields(inst(x, KIND_TIMEDELTA)?, &st.names)?;
    let q = td_fields(inst(y, KIND_TIMEDELTA)?, &st.names)?;
    Some(p.cmp(&q))
}

macro_rules! cmp_fast {
    ($name:ident, $cmp:ident, $pred:expr) => {
        fn $name(a: &[Object]) -> Option<Result<Object, RuntimeError>> {
            let o = $cmp(a)?;
            let pred: fn(std::cmp::Ordering) -> bool = $pred;
            Some(Ok(Object::Bool(pred(o))))
        }
    };
}

cmp_fast!(td_eq, td_cmp, |o| o.is_eq());
cmp_fast!(td_lt, td_cmp, |o| o.is_lt());
cmp_fast!(td_le, td_cmp, |o| o.is_le());
cmp_fast!(td_gt, td_cmp, |o| o.is_gt());
cmp_fast!(td_ge, td_cmp, |o| o.is_ge());

fn date_toordinal(a: &[Object]) -> Option<Result<Object, RuntimeError>> {
    let [x] = a else { return None };
    let st = state_of(x)?;
    let i = match kind_of(x) {
        KIND_DATE | KIND_DATETIME => match x {
            Object::Instance(i) => i,
            _ => return None,
        },
        _ => return None,
    };
    let (y, m, d) = date_fields(i, &st.names)?;
    if !valid_date(y, m, d) {
        return None;
    }
    Some(Ok(Object::Int(ymd2ord(y, m, d))))
}

fn date_weekday(a: &[Object]) -> Option<Result<Object, RuntimeError>> {
    let Ok(Object::Int(o)) = date_toordinal(a)? else {
        return None;
    };
    Some(Ok(Object::Int((o + 6) % 7)))
}

fn date_isoweekday(a: &[Object]) -> Option<Result<Object, RuntimeError>> {
    let Ok(Object::Int(o)) = date_toordinal(a)? else {
        return None;
    };
    Some(Ok(Object::Int(match o % 7 {
        0 => 7,
        r => r,
    })))
}

/// `date + timedelta` / `date - timedelta` (only whole days count).
fn date_shift(a: &[Object], sign: i64) -> Option<Result<Object, RuntimeError>> {
    let [x, y] = a else { return None };
    let st = state_of(x)?;
    let (yy, m, d) = date_fields(inst(x, KIND_DATE)?, &st.names)?;
    let (days, _, _) = td_fields(inst(y, KIND_TIMEDELTA)?, &st.names)?;
    if !valid_date(yy, m, d) {
        return None;
    }
    let o = ymd2ord(yy, m, d) + sign * days;
    if !(1..=MAX_ORDINAL).contains(&o) {
        return Some(Err(overflow_error("date value out of range")));
    }
    let (ny, nm, nd) = ord2ymd(o);
    Some(Ok(new_date(st, ny, nm, nd)?))
}

fn date_add(a: &[Object]) -> Option<Result<Object, RuntimeError>> {
    date_shift(a, 1)
}

fn date_sub(a: &[Object]) -> Option<Result<Object, RuntimeError>> {
    let [x, y] = a else { return None };
    if kind_of(y) == KIND_TIMEDELTA {
        return date_shift(a, -1);
    }
    let st = state_of(x)?;
    let (y1, m1, d1) = date_fields(inst(x, KIND_DATE)?, &st.names)?;
    let (y2, m2, d2) = date_fields(inst(y, KIND_DATE)?, &st.names)?;
    if !valid_date(y1, m1, d1) || !valid_date(y2, m2, d2) {
        return None;
    }
    new_td(
        st,
        i128::from(ymd2ord(y1, m1, d1) - ymd2ord(y2, m2, d2)),
        0,
        0,
    )
}

fn date_cmp(a: &[Object]) -> Option<std::cmp::Ordering> {
    let [x, y] = a else { return None };
    let st = state_of(x)?;
    let p = date_fields(inst(x, KIND_DATE)?, &st.names)?;
    let q = date_fields(inst(y, KIND_DATE)?, &st.names)?;
    Some(p.cmp(&q))
}

cmp_fast!(date_eq, date_cmp, |o| o.is_eq());
cmp_fast!(date_lt, date_cmp, |o| o.is_lt());
cmp_fast!(date_le, date_cmp, |o| o.is_le());
cmp_fast!(date_gt, date_cmp, |o| o.is_gt());
cmp_fast!(date_ge, date_cmp, |o| o.is_ge());

fn date_isoformat(a: &[Object]) -> Option<Result<Object, RuntimeError>> {
    let [x] = a else { return None };
    let st = state_of(x)?;
    let (y, m, d) = date_fields(inst(x, KIND_DATE)?, &st.names)?;
    if !valid_date(y, m, d) {
        return None;
    }
    Some(Ok(Object::from_str(format!("{y:04}-{m:02}-{d:02}"))))
}

/// `date.replace(year=None, month=None, day=None)`, positional form.
fn date_replace(a: &[Object]) -> Option<Result<Object, RuntimeError>> {
    let (x, rest) = a.split_first()?;
    let st = state_of(x)?;
    let (mut y, mut m, mut d) = date_fields(inst(x, KIND_DATE)?, &st.names)?;
    if rest.len() > 3 {
        return None;
    }
    for (i, v) in rest.iter().enumerate() {
        if matches!(v, Object::None) {
            continue;
        }
        let v = as_int(v)?;
        match i {
            0 => y = v,
            1 => m = v,
            _ => d = v,
        }
    }
    if !valid_date(y, m, d) {
        return None;
    }
    Some(Ok(new_date(st, y, m, d)?))
}

fn dt_add(a: &[Object]) -> Option<Result<Object, RuntimeError>> {
    let [x, y] = a else { return None };
    let st = state_of(x)?;
    let f = dt_fields(inst(x, KIND_DATETIME)?, &st.names)?;
    let delta = td_us(td_fields(inst(y, KIND_TIMEDELTA)?, &st.names)?);
    if !valid_date(f.y, f.m, f.d) {
        return None;
    }
    match dt_shift(&f, delta) {
        Some(g) => Some(Ok(new_dt(st, &g)?)),
        None => Some(Err(overflow_error("date value out of range"))),
    }
}

fn dt_sub(a: &[Object]) -> Option<Result<Object, RuntimeError>> {
    let [x, y] = a else { return None };
    let st = state_of(x)?;
    let f = dt_fields(inst(x, KIND_DATETIME)?, &st.names)?;
    if !valid_date(f.y, f.m, f.d) {
        return None;
    }
    match kind_of(y) {
        KIND_TIMEDELTA => {
            let delta = td_us(td_fields(inst(y, KIND_TIMEDELTA)?, &st.names)?);
            match dt_shift(&f, -delta) {
                Some(g) => Some(Ok(new_dt(st, &g)?)),
                None => Some(Err(overflow_error("date value out of range"))),
            }
        }
        KIND_DATETIME => {
            let g = dt_fields(inst(y, KIND_DATETIME)?, &st.names)?;
            if !valid_date(g.y, g.m, g.d) {
                return None;
            }
            let mut diff = dt_us(&f) - dt_us(&g);
            if !f.tz.is_same(&g.tz) {
                match (tz_of(&f.tz, &st.names)?, tz_of(&g.tz, &st.names)?) {
                    (Tz::Naive, Tz::Naive) => {}
                    (Tz::Fixed(a), Tz::Fixed(b)) => diff += b - a,
                    // The mixed case raises in the Python code.
                    _ => return None,
                }
            }
            new_td(st, 0, 0, diff)
        }
        _ => None,
    }
}

/// `datetime` ordering: `(ordering, comparable)`; `comparable` is false
/// for a naive/aware pair (equality says unequal, ordering raises).
fn dt_cmp_raw(a: &[Object]) -> Option<(std::cmp::Ordering, bool)> {
    let [x, y] = a else { return None };
    let st = state_of(x)?;
    let f = dt_fields(inst(x, KIND_DATETIME)?, &st.names)?;
    let g = dt_fields(inst(y, KIND_DATETIME)?, &st.names)?;
    if !valid_date(f.y, f.m, f.d) || !valid_date(g.y, g.m, g.d) {
        return None;
    }
    if f.tz.is_same(&g.tz) {
        return Some((dt_us(&f).cmp(&dt_us(&g)), true));
    }
    match (tz_of(&f.tz, &st.names)?, tz_of(&g.tz, &st.names)?) {
        (Tz::Naive, Tz::Naive) => Some((dt_us(&f).cmp(&dt_us(&g)), true)),
        (Tz::Fixed(a), Tz::Fixed(b)) => Some(((dt_us(&f) - a).cmp(&(dt_us(&g) - b)), true)),
        _ => Some((std::cmp::Ordering::Equal, false)),
    }
}

fn dt_eq(a: &[Object]) -> Option<Result<Object, RuntimeError>> {
    let (o, comparable) = dt_cmp_raw(a)?;
    Some(Ok(Object::Bool(comparable && o.is_eq())))
}

macro_rules! dt_order {
    ($name:ident, $pred:expr) => {
        fn $name(a: &[Object]) -> Option<Result<Object, RuntimeError>> {
            let (o, comparable) = dt_cmp_raw(a)?;
            if !comparable {
                // The Python code raises (with its own message).
                return None;
            }
            let pred: fn(std::cmp::Ordering) -> bool = $pred;
            Some(Ok(Object::Bool(pred(o))))
        }
    };
}

dt_order!(dt_lt, |o| o.is_lt());
dt_order!(dt_le, |o| o.is_le());
dt_order!(dt_gt, |o| o.is_gt());
dt_order!(dt_ge, |o| o.is_ge());

fn dt_date(a: &[Object]) -> Option<Result<Object, RuntimeError>> {
    let [x] = a else { return None };
    let st = state_of(x)?;
    let (y, m, d) = date_fields(inst(x, KIND_DATETIME)?, &st.names)?;
    if !valid_date(y, m, d) {
        return None;
    }
    Some(Ok(new_date(st, y, m, d)?))
}

/// `_format_offset(off, sep)` for a fixed offset in microseconds.
fn format_offset(out: &mut String, off: i128, sep: &str) {
    let (sign, mut v) = if off < 0 { ('-', -off) } else { ('+', off) };
    let hh = v / 3_600_000_000;
    v %= 3_600_000_000;
    let mm = v / 60_000_000;
    v %= 60_000_000;
    let _ = write!(out, "{sign}{hh:02}{sep}{mm:02}");
    if v != 0 {
        let _ = write!(out, "{sep}{:02}", v / 1_000_000);
        if v % 1_000_000 != 0 {
            let _ = write!(out, ".{:06}", v % 1_000_000);
        }
    }
}

/// `datetime.isoformat(sep='T', timespec='auto')`, positional form.
fn dt_isoformat(a: &[Object]) -> Option<Result<Object, RuntimeError>> {
    let (x, rest) = a.split_first()?;
    let sep = match rest.first() {
        None => 'T',
        Some(Object::Str(s)) => {
            let mut cs = s.chars();
            let c = cs.next()?;
            if cs.next().is_some() {
                return None;
            }
            c
        }
        Some(_) => return None,
    };
    let spec = match rest.get(1) {
        None => "auto",
        Some(Object::Str(s)) => match &**s {
            "auto" => "auto",
            "hours" => "hours",
            "minutes" => "minutes",
            "seconds" => "seconds",
            "milliseconds" => "milliseconds",
            "microseconds" => "microseconds",
            _ => return None,
        },
        Some(_) => return None,
    };
    if rest.len() > 2 {
        return None;
    }
    let st = state_of(x)?;
    let f = dt_fields(inst(x, KIND_DATETIME)?, &st.names)?;
    if !valid_date(f.y, f.m, f.d) {
        return None;
    }
    let tz = tz_of(&f.tz, &st.names)?;
    let mut out = String::with_capacity(32);
    let _ = write!(out, "{:04}-{:02}-{:02}{sep}", f.y, f.m, f.d);
    match spec {
        "hours" => {
            let _ = write!(out, "{:02}", f.hh);
        }
        "minutes" => {
            let _ = write!(out, "{:02}:{:02}", f.hh, f.mm);
        }
        "milliseconds" => {
            let _ = write!(
                out,
                "{:02}:{:02}:{:02}.{:03}",
                f.hh,
                f.mm,
                f.ss,
                f.us / 1000
            );
        }
        "microseconds" => {
            let _ = write!(out, "{:02}:{:02}:{:02}.{:06}", f.hh, f.mm, f.ss, f.us);
        }
        "seconds" => {
            let _ = write!(out, "{:02}:{:02}:{:02}", f.hh, f.mm, f.ss);
        }
        _ => {
            if f.us != 0 {
                let _ = write!(out, "{:02}:{:02}:{:02}.{:06}", f.hh, f.mm, f.ss, f.us);
            } else {
                let _ = write!(out, "{:02}:{:02}:{:02}", f.hh, f.mm, f.ss);
            }
        }
    }
    if let Tz::Fixed(off) = tz {
        format_offset(&mut out, off, ":");
    }
    Some(Ok(Object::from_str(out)))
}

/// `strftime` for the numeric directives (no locale involvement); any
/// other directive declines.
fn strftime_numeric(
    y: i64,
    m: i64,
    d: i64,
    time: Option<(i64, i64, i64, i64)>,
    tz: Option<&Tz>,
    fmt: &str,
) -> Option<String> {
    let (hh, mm, ss, us) = time.unwrap_or((0, 0, 0, 0));
    let mut out = String::with_capacity(fmt.len() + 16);
    let mut chars = fmt.chars();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        match chars.next()? {
            'Y' => {
                let _ = write!(out, "{y:04}");
            }
            'm' => {
                let _ = write!(out, "{m:02}");
            }
            'd' => {
                let _ = write!(out, "{d:02}");
            }
            'H' => {
                let _ = write!(out, "{hh:02}");
            }
            'M' => {
                let _ = write!(out, "{mm:02}");
            }
            'S' => {
                let _ = write!(out, "{ss:02}");
            }
            'f' => {
                let _ = write!(out, "{us:06}");
            }
            'y' => {
                let _ = write!(out, "{:02}", y % 100);
            }
            'j' => {
                let doy = DAYS_BEFORE_MONTH[(m - 1) as usize] + i64::from(m > 2 && is_leap(y)) + d;
                let _ = write!(out, "{doy:03}");
            }
            'I' => {
                let h12 = match hh % 12 {
                    0 => 12,
                    h => h,
                };
                let _ = write!(out, "{h12:02}");
            }
            'F' => {
                let _ = write!(out, "{y:04}-{m:02}-{d:02}");
            }
            'T' => {
                let _ = write!(out, "{hh:02}:{mm:02}:{ss:02}");
            }
            '%' => out.push('%'),
            'z' => match tz? {
                Tz::Naive => {}
                Tz::Fixed(off) => format_offset(&mut out, *off, ""),
            },
            ':' => {
                if chars.next()? != 'z' {
                    return None;
                }
                match tz? {
                    Tz::Naive => {}
                    Tz::Fixed(off) => format_offset(&mut out, *off, ":"),
                }
            }
            _ => return None,
        }
    }
    Some(out)
}

fn dt_strftime(a: &[Object]) -> Option<Result<Object, RuntimeError>> {
    let [x, Object::Str(fmt)] = a else {
        return None;
    };
    let st = state_of(x)?;
    let f = dt_fields(inst(x, KIND_DATETIME)?, &st.names)?;
    if !valid_date(f.y, f.m, f.d) {
        return None;
    }
    let tz = tz_of(&f.tz, &st.names)?;
    let s = strftime_numeric(
        f.y,
        f.m,
        f.d,
        Some((f.hh, f.mm, f.ss, f.us)),
        Some(&tz),
        fmt,
    )?;
    Some(Ok(Object::from_str(s)))
}

fn date_strftime(a: &[Object]) -> Option<Result<Object, RuntimeError>> {
    let [x, Object::Str(fmt)] = a else {
        return None;
    };
    let st = state_of(x)?;
    let (y, m, d) = date_fields(inst(x, KIND_DATE)?, &st.names)?;
    if !valid_date(y, m, d) {
        return None;
    }
    // A date has no time or offset: `%z` formats as empty, like CPython.
    let s = strftime_numeric(y, m, d, None, Some(&Tz::Naive), fmt)?;
    Some(Ok(Object::from_str(s)))
}

fn digits(b: &[u8]) -> Option<i64> {
    if b.is_empty() || !b.iter().all(u8::is_ascii_digit) {
        return None;
    }
    Some(b.iter().fold(0, |n, c| n * 10 + i64::from(c - b'0')))
}

/// `datetime.fromisoformat` for `YYYY-MM-DD[?HH[:MM[:SS[.fff[fff]]]]]`
/// with an optional `Z` or `±HH:MM[:SS[.ffffff]]` offset. Any other
/// spelling (week dates, compact forms, hour 24, …) declines.
fn dt_fromisoformat(a: &[Object]) -> Option<Result<Object, RuntimeError>> {
    let [Object::Type(cls), Object::Str(s)] = a else {
        return None;
    };
    if cls.native_kind.get() != KIND_DATETIME {
        return None;
    }
    let st = state_of_cls(cls)?;
    let b = s.as_bytes();
    if b.len() < 10 || !s.is_ascii() || b[4] != b'-' || b[7] != b'-' {
        return None;
    }
    let (y, m, d) = (digits(&b[0..4])?, digits(&b[5..7])?, digits(&b[8..10])?);
    if !valid_date(y, m, d) {
        return None;
    }
    let mut f = Dt {
        y,
        m,
        d,
        hh: 0,
        mm: 0,
        ss: 0,
        us: 0,
        tz: Object::None,
        fold: 0,
    };
    if b.len() > 10 {
        let t = &b[11..];
        if t.is_empty() {
            return None;
        }
        // Split off the offset.
        let tz_at = t.iter().position(|c| matches!(c, b'+' | b'-' | b'Z'));
        let (time, off) = match tz_at {
            Some(i) => (&t[..i], Some(&t[i..])),
            None => (t, None),
        };
        let (hh, mm, ss, us) = parse_hms(time)?;
        if hh > 23 || mm > 59 || ss > 59 {
            return None;
        }
        (f.hh, f.mm, f.ss, f.us) = (hh, mm, ss, us);
        if let Some(off) = off {
            f.tz = if off == b"Z" {
                Object::Instance(st.utc.upgrade()?)
            } else if off[0] == b'Z' {
                // Text after `Z` is malformed; the Python code raises.
                return None;
            } else {
                let sign: i128 = if off[0] == b'-' { -1 } else { 1 };
                let body = &off[1..];
                // `±HH:MM[:SS[.ffffff]]` only (the colon forms).
                if body.len() < 5 || body[2] != b':' {
                    return None;
                }
                let (oh, om, os, ous) = parse_hms(body)?;
                if om > 59 || os > 59 {
                    return None;
                }
                let us = sign
                    * ((i128::from(oh) * 3600 + i128::from(om) * 60 + i128::from(os)) * 1_000_000
                        + i128::from(ous));
                if us == 0 {
                    Object::Instance(st.utc.upgrade()?)
                } else if us.abs() >= US_PER_DAY {
                    return None;
                } else {
                    new_timezone(st, us)?
                }
            };
        }
    }
    Some(Ok(new_dt(st, &f)?))
}

/// `HH[:MM[:SS[.f{1,6}]]]` (colon-separated only).
fn parse_hms(t: &[u8]) -> Option<(i64, i64, i64, i64)> {
    let hh = digits(t.get(0..2)?)?;
    let mut rest = &t[2..];
    let mut mm = 0;
    let mut ss = 0;
    let mut us = 0;
    if !rest.is_empty() {
        if rest[0] != b':' {
            return None;
        }
        mm = digits(rest.get(1..3)?)?;
        rest = &rest[3..];
        if !rest.is_empty() {
            if rest[0] != b':' {
                return None;
            }
            ss = digits(rest.get(1..3)?)?;
            rest = &rest[3..];
            if !rest.is_empty() {
                if !matches!(rest[0], b'.' | b',') {
                    return None;
                }
                let frac = &rest[1..];
                if frac.is_empty() || !frac.iter().all(u8::is_ascii_digit) {
                    return None;
                }
                let take = frac.len().min(6);
                let mut v = digits(&frac[..take])?;
                for _ in take..6 {
                    v *= 10;
                }
                us = v;
            }
        }
    }
    Some((hh, mm, ss, us))
}

/// An exact `timezone` with the fixed offset `us` (non-zero, in range)
/// and no name.
fn new_timezone(st: &State, us: i128) -> Option<Object> {
    let off = new_td(st, 0, 0, us)?.ok()?;
    let cls = st.timezone.upgrade()?;
    let n = &st.names;
    Some(instance(
        cls,
        vec![entry(&n.offset, off), entry(&n.name, Object::None)],
    ))
}

/// Whether a dying natively served instance may simply be dropped: it is
/// never tracked and has no finalizer; what it references (ints, `None`,
/// strings, natively served `timedelta`s and `timezone`s) cannot start a
/// finalization either. A `datetime`/`time` holding any other `tzinfo`
/// keeps the careful path, which frees that object promptly.
pub(crate) fn plain_drop_ok(i: &PyInstance) -> bool {
    match i.cls_raw().native_kind.get() {
        KIND_DATETIME | KIND_TIME => {
            let Ok(s) = i.slots.try_borrow() else {
                return false;
            };
            let idx = if i.cls_raw().native_kind.get() == KIND_DATETIME {
                DT_TZINFO
            } else {
                4
            };
            match s.get_hinted(idx, "_tzinfo") {
                Some(Object::None) => true,
                Some(o @ Object::Instance(_)) => kind_of(o) == KIND_TIMEZONE,
                _ => false,
            }
        }
        KIND_TIMEDELTA | KIND_DATE | KIND_TIMEZONE => true,
        _ => false,
    }
}

// ---------------------------------------------------------------------
// The dispatch loop's shortcuts.

/// `a OP b` for a natively served left operand, or `None` (full path).
pub(crate) fn leaf_binop(
    op: weavepy_compiler::BinOpKind,
    a: &Object,
    b: &Object,
) -> Option<Result<Object, RuntimeError>> {
    use weavepy_compiler::BinOpKind as B;
    let Object::Instance(i) = a else { return None };
    let cls = i.cls_raw();
    let kind = cls.native_kind.get();
    if kind == 0 {
        return None;
    }
    let st = state_of_cls(cls)?;
    if !verified(st, cls, kind) {
        return None;
    }
    // The right operand's reflected method wins first only when its type
    // is a proper subclass of the left's; every natively served pair has
    // exact types, so that never applies.
    if let Object::Instance(j) = b {
        let k = j.cls_raw().native_kind.get();
        if k == 0 || !verified(st, j.cls_raw(), k) {
            return None;
        }
    }
    let args = [a.clone(), b.clone()];
    match (kind, op) {
        (KIND_TIMEDELTA, B::Add) => td_add(&args),
        (KIND_TIMEDELTA, B::Sub) => td_sub(&args),
        (KIND_TIMEDELTA, B::Mult) => td_mul(&args),
        (KIND_DATE, B::Add) => date_add(&args),
        (KIND_DATE, B::Sub) => date_sub(&args),
        (KIND_DATETIME, B::Add) => dt_add(&args),
        (KIND_DATETIME, B::Sub) => dt_sub(&args),
        _ => None,
    }
}

/// `a OP b` comparison for natively served operands, or `None`.
pub(crate) fn leaf_compare(
    op: weavepy_compiler::CompareKind,
    a: &Object,
    b: &Object,
) -> Option<Result<Object, RuntimeError>> {
    use weavepy_compiler::CompareKind as C;
    let (Object::Instance(i), Object::Instance(j)) = (a, b) else {
        return None;
    };
    let (ci, cj) = (i.cls_raw(), j.cls_raw());
    let kind = ci.native_kind.get();
    if kind == 0 || cj.native_kind.get() != kind {
        return None;
    }
    let st = state_of_cls(ci)?;
    if !verified(st, ci, kind) || !verified(st, cj, kind) {
        return None;
    }
    let args = [a.clone(), b.clone()];
    let r = match kind {
        KIND_TIMEDELTA => td_cmp(&args).map(|o| (o, true)),
        KIND_DATE => date_cmp(&args).map(|o| (o, true)),
        KIND_DATETIME => dt_cmp_raw(&args),
        _ => None,
    }?;
    let (o, comparable) = r;
    Some(Ok(Object::Bool(match op {
        C::Eq => comparable && o.is_eq(),
        C::NotEq => !(comparable && o.is_eq()),
        _ if !comparable => return None,
        C::Lt => o.is_lt(),
        C::LtE => o.is_le(),
        C::Gt => o.is_gt(),
        C::GtE => o.is_ge(),
    })))
}

/// A public field (`dt.hour`, `d.year`, …) of a natively served instance
/// whose class still has its original property.
pub(crate) fn leaf_field(i: &PyInstance, name: &SharedStr) -> Option<Object> {
    let cls = i.cls_raw();
    let kind = cls.native_kind.get();
    if kind != KIND_DATE && kind != KIND_DATETIME {
        return None;
    }
    let st = state_of_cls(cls)?;
    if !verified(st, cls, kind) {
        return None;
    }
    let n = &st.names;
    let (idx, slot) = if SharedStr::ptr_eq(name, &n.f_year) {
        (D_YEAR, &n.year)
    } else if SharedStr::ptr_eq(name, &n.f_month) {
        (D_MONTH, &n.month)
    } else if SharedStr::ptr_eq(name, &n.f_day) {
        (D_DAY, &n.day)
    } else if kind != KIND_DATETIME {
        return None;
    } else if SharedStr::ptr_eq(name, &n.f_hour) {
        (DT_HOUR, &n.hour)
    } else if SharedStr::ptr_eq(name, &n.f_minute) {
        (DT_MINUTE, &n.minute)
    } else if SharedStr::ptr_eq(name, &n.f_second) {
        (DT_SECOND, &n.second)
    } else if SharedStr::ptr_eq(name, &n.f_microsecond) {
        (DT_US, &n.microsecond)
    } else if SharedStr::ptr_eq(name, &n.f_tzinfo) {
        (DT_TZINFO, &n.tzinfo)
    } else if SharedStr::ptr_eq(name, &n.f_fold) {
        (DT_FOLD, &n.fold)
    } else {
        return None;
    };
    let s = i.slots.try_borrow().ok()?;
    Some(s.get_hinted(idx, slot)?.clone())
}

// ---------------------------------------------------------------------
// Installation.

fn with_interp<R>(
    f: impl FnOnce(&mut crate::Interpreter) -> Result<R, RuntimeError>,
) -> Result<R, RuntimeError> {
    let ptr = crate::vm_singletons::current_interpreter_ptr()
        .ok_or_else(|| type_error("datetime: no active interpreter"))?;
    // SAFETY: published by the enclosing VM frame on this thread.
    f(unsafe { &mut *ptr })
}

/// One native method: its name, the class kind it lives on, and the
/// pure fast half.
struct Spec {
    kind: u8,
    name: &'static str,
    fast: Fast,
    /// Stored as a `classmethod` (the fast half then gets the class).
    classmethod: bool,
}

const SPECS: &[Spec] = &[
    Spec {
        kind: KIND_TIMEDELTA,
        name: "__add__",
        fast: td_add,
        classmethod: false,
    },
    Spec {
        kind: KIND_TIMEDELTA,
        name: "__radd__",
        fast: td_add,
        classmethod: false,
    },
    Spec {
        kind: KIND_TIMEDELTA,
        name: "__sub__",
        fast: td_sub,
        classmethod: false,
    },
    Spec {
        kind: KIND_TIMEDELTA,
        name: "__neg__",
        fast: td_neg,
        classmethod: false,
    },
    Spec {
        kind: KIND_TIMEDELTA,
        name: "__mul__",
        fast: td_mul,
        classmethod: false,
    },
    Spec {
        kind: KIND_TIMEDELTA,
        name: "__rmul__",
        fast: td_mul,
        classmethod: false,
    },
    Spec {
        kind: KIND_TIMEDELTA,
        name: "__bool__",
        fast: td_bool,
        classmethod: false,
    },
    Spec {
        kind: KIND_TIMEDELTA,
        name: "__eq__",
        fast: td_eq,
        classmethod: false,
    },
    Spec {
        kind: KIND_TIMEDELTA,
        name: "__lt__",
        fast: td_lt,
        classmethod: false,
    },
    Spec {
        kind: KIND_TIMEDELTA,
        name: "__le__",
        fast: td_le,
        classmethod: false,
    },
    Spec {
        kind: KIND_TIMEDELTA,
        name: "__gt__",
        fast: td_gt,
        classmethod: false,
    },
    Spec {
        kind: KIND_TIMEDELTA,
        name: "__ge__",
        fast: td_ge,
        classmethod: false,
    },
    Spec {
        kind: KIND_DATE,
        name: "toordinal",
        fast: date_toordinal,
        classmethod: false,
    },
    Spec {
        kind: KIND_DATE,
        name: "weekday",
        fast: date_weekday,
        classmethod: false,
    },
    Spec {
        kind: KIND_DATE,
        name: "isoweekday",
        fast: date_isoweekday,
        classmethod: false,
    },
    Spec {
        kind: KIND_DATE,
        name: "__add__",
        fast: date_add,
        classmethod: false,
    },
    Spec {
        kind: KIND_DATE,
        name: "__radd__",
        fast: date_add,
        classmethod: false,
    },
    Spec {
        kind: KIND_DATE,
        name: "__sub__",
        fast: date_sub,
        classmethod: false,
    },
    Spec {
        kind: KIND_DATE,
        name: "__eq__",
        fast: date_eq,
        classmethod: false,
    },
    Spec {
        kind: KIND_DATE,
        name: "__lt__",
        fast: date_lt,
        classmethod: false,
    },
    Spec {
        kind: KIND_DATE,
        name: "__le__",
        fast: date_le,
        classmethod: false,
    },
    Spec {
        kind: KIND_DATE,
        name: "__gt__",
        fast: date_gt,
        classmethod: false,
    },
    Spec {
        kind: KIND_DATE,
        name: "__ge__",
        fast: date_ge,
        classmethod: false,
    },
    Spec {
        kind: KIND_DATE,
        name: "isoformat",
        fast: date_isoformat,
        classmethod: false,
    },
    Spec {
        kind: KIND_DATE,
        name: "strftime",
        fast: date_strftime,
        classmethod: false,
    },
    Spec {
        kind: KIND_DATE,
        name: "replace",
        fast: date_replace,
        classmethod: false,
    },
    Spec {
        kind: KIND_DATETIME,
        name: "__add__",
        fast: dt_add,
        classmethod: false,
    },
    Spec {
        kind: KIND_DATETIME,
        name: "__radd__",
        fast: dt_add,
        classmethod: false,
    },
    Spec {
        kind: KIND_DATETIME,
        name: "__sub__",
        fast: dt_sub,
        classmethod: false,
    },
    Spec {
        kind: KIND_DATETIME,
        name: "__eq__",
        fast: dt_eq,
        classmethod: false,
    },
    Spec {
        kind: KIND_DATETIME,
        name: "__lt__",
        fast: dt_lt,
        classmethod: false,
    },
    Spec {
        kind: KIND_DATETIME,
        name: "__le__",
        fast: dt_le,
        classmethod: false,
    },
    Spec {
        kind: KIND_DATETIME,
        name: "__gt__",
        fast: dt_gt,
        classmethod: false,
    },
    Spec {
        kind: KIND_DATETIME,
        name: "__ge__",
        fast: dt_ge,
        classmethod: false,
    },
    Spec {
        kind: KIND_DATETIME,
        name: "isoformat",
        fast: dt_isoformat,
        classmethod: false,
    },
    Spec {
        kind: KIND_DATETIME,
        name: "strftime",
        fast: dt_strftime,
        classmethod: false,
    },
    Spec {
        kind: KIND_DATETIME,
        name: "date",
        fast: dt_date,
        classmethod: false,
    },
    Spec {
        kind: KIND_DATETIME,
        name: "fromisoformat",
        fast: dt_fromisoformat,
        classmethod: true,
    },
];

/// `install(timedelta, date, time, datetime, timezone)`: put the native
/// methods on the exact classes (see the module docs). Idempotent per
/// class set; returns `None`.
pub(crate) fn install(args: &[Object]) -> Result<Object, RuntimeError> {
    let [Object::Type(td), Object::Type(date), Object::Type(time), Object::Type(dt), Object::Type(tz)] =
        args
    else {
        return Err(type_error("install() expects the five datetime classes"));
    };
    if dt.native_ext.get().is_some() {
        return Ok(Object::None);
    }
    let classes: [(u8, &Rc<TypeObject>); 5] = [
        (KIND_TIMEDELTA, td),
        (KIND_DATE, date),
        (KIND_TIME, time),
        (KIND_DATETIME, dt),
        (KIND_TIMEZONE, tz),
    ];
    let utc = match tz.dict.borrow().get(&crate::object::StrKey("utc")) {
        Some(Object::Instance(i)) => Rc::downgrade(i),
        _ => return Err(type_error("install(): timezone.utc missing")),
    };
    // The replaced Python implementations, read from each class's own
    // dict before anything changes.
    let mut orig = HashMap::new();
    for spec in SPECS {
        let cls = classes
            .iter()
            .find(|(k, _)| *k == spec.kind)
            .map(|(_, c)| *c)
            .expect("spec kinds are the five classes");
        // The class's own definition, else the inherited one (the native
        // then overrides it on this class only).
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
        orig.insert((spec.kind, spec.name), v);
    }
    let names = Names::new();
    let layout = |keys: &[&SharedStr]| -> Rc<[DictKey]> {
        keys.iter()
            .map(|k| DictKey(Object::Str((*k).clone())))
            .collect::<Vec<_>>()
            .into()
    };
    let td_layout = layout(&[
        &names.days,
        &names.seconds,
        &names.microseconds,
        &names.hashcode,
        &names.pub_days,
        &names.pub_seconds,
        &names.pub_microseconds,
    ]);
    let date_layout = layout(&[&names.year, &names.month, &names.day, &names.hashcode]);
    let dt_layout = layout(&[
        &names.year,
        &names.month,
        &names.day,
        &names.hour,
        &names.minute,
        &names.second,
        &names.microsecond,
        &names.tzinfo,
        &names.hashcode,
        &names.fold,
    ]);
    let state = Rc::new(State {
        timedelta: Rc::downgrade(td),
        date: Rc::downgrade(date),
        datetime: Rc::downgrade(dt),
        timezone: Rc::downgrade(tz),
        utc,
        names,
        td_layout,
        date_layout,
        dt_layout,
        orig,
        sealed: Default::default(),
        expect: std::sync::OnceLock::new(),
    });
    // Each class carries the state (a class in `native_ext` resolves to
    // it from any of its instances).
    for (kind, cls) in &classes {
        let _ = cls
            .native_ext
            .set(state.clone() as Rc<dyn std::any::Any + Send + Sync>);
        cls.native_kind.set(*kind);
    }
    for spec in SPECS {
        let cls = classes
            .iter()
            .find(|(k, _)| *k == spec.kind)
            .map(|(_, c)| *c)
            .expect("spec kinds are the five classes");
        let fast = spec.fast;
        let key = (spec.kind, spec.name);
        let st = state.clone();
        let st_kw = state.clone();
        let b = Rc::new(BuiltinFn {
            name: spec.name,
            binds_instance: !spec.classmethod,
            call: Box::new(move |a: &[Object]| match fast(a) {
                Some(r) => r,
                None => {
                    let f = st.orig.get(&key).cloned().unwrap_or(Object::None);
                    with_interp(|i| i.call_object(f, a, &[]))
                }
            }),
            call_kw: Some(Box::new(move |a: &[Object], kw: &[(String, Object)]| {
                if kw.is_empty() {
                    if let Some(r) = fast(a) {
                        return r;
                    }
                }
                let f = st_kw.orig.get(&key).cloned().unwrap_or(Object::None);
                with_interp(|i| i.call_object(f, a, kw))
            })),
        });
        crate::leaf_builtins::register_fast(&b, fast);
        let value = if spec.classmethod {
            Object::ClassMethod(crate::object::MethodWrapper::new(Object::Builtin(b)))
        } else {
            Object::Builtin(b)
        };
        cls.dict
            .borrow_mut()
            .insert(DictKey(Object::Str(interned(spec.name))), value);
    }
    for (_, cls) in &classes {
        cls.bump_attr_version();
    }
    // What each shortcut name resolves to now (see `verified`).
    let expect: Vec<(u8, &'static str, Object)> = SHORTCUT_NAMES
        .iter()
        .filter_map(|(kind, name)| {
            let cls = classes.iter().find(|(k, _)| k == kind)?.1;
            Some((*kind, *name, cls.lookup(name)?))
        })
        .collect();
    let _ = state.expect.set(expect);
    for (_, cls) in &classes {
        let names: Vec<Object> = cls
            .dict
            .borrow()
            .iter()
            .filter_map(|(_, v)| match v {
                Object::Instance(i) if i.cls_raw().native_kind.get() != 0 => {
                    Some(Object::Instance(i.clone()))
                }
                _ => None,
            })
            .collect();
        for v in names {
            crate::gc_trace::untrack(&v);
            if let Object::Instance(i) = &v {
                let off = i
                    .slots
                    .try_borrow()
                    .ok()
                    .and_then(|s| s.get_hinted(TZ_OFFSET, "_offset").cloned());
                if let Some(off @ Object::Instance(_)) = off {
                    crate::gc_trace::untrack(&off);
                }
            }
        }
    }
    Ok(Object::None)
}

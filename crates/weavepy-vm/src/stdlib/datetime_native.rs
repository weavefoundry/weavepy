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
//! The natives build exact `timedelta`, `date` and `datetime` instances
//! with packed storage ([`PackedSlots`]): the fields as two machine words
//! instead of a vector of objects, as CPython's C structs hold them. The
//! exact classes' constructors (keywords included), arithmetic, and
//! `fromisoformat` produce such values, and a dead one returns to its
//! class set's [`Pool`], so a value's life costs no allocation. Python
//! code reading a slot (`self._year`) sees field objects built on first
//! use; writing one turns the storage back into a plain field vector.
//!
//! Exact-class instances are never cycle-collector tracked: CPython's C
//! datetime types are not GC types either (a cycle through `tzinfo` is
//! uncollectable there too), and their fields are ints, `None` and the
//! `tzinfo` reference.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::error::{overflow_error, type_error, RuntimeError};
use crate::object::{BuiltinFn, DictKey, Object};
use crate::shared_value::{SharedSlice, SharedStr};
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

// ---------------------------------------------------------------------
// Packed storage.

/// A natively built `timedelta`, `date` or `datetime`, packed into two
/// words: the instance's slot storage (see `SlotData::Packed`) instead
/// of a vector of field objects. CPython's C types store these as C
/// struct members; packing gets WeavePy the same allocation-free
/// representation, and the natives read a field with a shift.
///
/// - `datetime` and `date`: `word` holds the fields in chronological
///   significance (see [`pack_dt`]), so two naive values compare as
///   `word >> 1`; `aux` owns the `tzinfo` (an `Rc<PyInstance>` turned
///   into a raw pointer), or is `0` for `None`.
/// - `timedelta`: `word` is the days (as `i64`), `aux` the seconds and
///   microseconds (`seconds << 20 | microseconds`).
///
/// Code that reads a slot by reference (a slot descriptor serving
/// `self._year` in a Python method, `copy`, the collector) gets a copy
/// of the field objects built on first use and kept until the storage
/// dies or is written; every write first converts the storage to a
/// field vector (`SlotStorage::unpack`).
pub(crate) struct PackedSlots {
    word: u64,
    aux: usize,
    /// The kind (low three bits), and the lazily built field objects (a
    /// thin pointer to `layout().len()` objects; `0` until built).
    meta: AtomicUsize,
}

const META_KIND: usize = 7;

// Bit positions in a `datetime`/`date` word.
const W_FOLD: u32 = 0;
const W_US: u32 = 1;
const W_SS: u32 = 21;
const W_MM: u32 = 27;
const W_HH: u32 = 33;
const W_DAY: u32 = 38;
const W_MONTH: u32 = 43;
const W_YEAR: u32 = 47;

/// A validated `datetime`'s fields as a packed word.
#[inline]
fn pack_dt(f: &Dt) -> u64 {
    ((f.y as u64) << W_YEAR)
        | ((f.m as u64) << W_MONTH)
        | ((f.d as u64) << W_DAY)
        | ((f.hh as u64) << W_HH)
        | ((f.mm as u64) << W_MM)
        | ((f.ss as u64) << W_SS)
        | ((f.us as u64) << W_US)
        | ((f.fold as u64) << W_FOLD)
}

#[inline]
fn bits(word: u64, shift: u32, width: u32) -> i64 {
    ((word >> shift) & ((1 << width) - 1)) as i64
}

/// `(year, month, day)` of a packed `date`/`datetime` word.
#[inline]
fn word_ymd(w: u64) -> (i64, i64, i64) {
    (bits(w, W_YEAR, 14), bits(w, W_MONTH, 4), bits(w, W_DAY, 5))
}

/// `(hour, minute, second, microsecond, fold)` of a packed word.
#[inline]
fn word_time(w: u64) -> (i64, i64, i64, i64, i64) {
    (
        bits(w, W_HH, 5),
        bits(w, W_MM, 6),
        bits(w, W_SS, 6),
        bits(w, W_US, 20),
        bits(w, W_FOLD, 1),
    )
}

/// The slot layouts packed values describe themselves with, shared by
/// every interpreter: `[timedelta, date, datetime]`, in the order the
/// classes' `__new__` assigns the slots.
fn layouts() -> &'static [SharedSlice<DictKey>; 3] {
    static LAYOUTS: std::sync::OnceLock<[SharedSlice<DictKey>; 3]> = std::sync::OnceLock::new();
    LAYOUTS.get_or_init(|| {
        let layout = |names: &[&str]| -> SharedSlice<DictKey> {
            names
                .iter()
                .map(|n| DictKey(Object::Str(interned(n))))
                .collect::<Vec<_>>()
                .into()
        };
        [
            layout(&[
                "_days",
                "_seconds",
                "_microseconds",
                "_hashcode",
                "days",
                "seconds",
                "microseconds",
            ]),
            layout(&["_year", "_month", "_day", "_hashcode"]),
            layout(&[
                "_year",
                "_month",
                "_day",
                "_hour",
                "_minute",
                "_second",
                "_microsecond",
                "_tzinfo",
                "_hashcode",
                "_fold",
            ]),
        ]
    })
}

#[inline]
fn layout_of(kind: u8) -> &'static SharedSlice<DictKey> {
    let l = layouts();
    match kind {
        KIND_TIMEDELTA => &l[0],
        KIND_DATE => &l[1],
        _ => &l[2],
    }
}

impl PackedSlots {
    #[inline]
    fn new(kind: u8, word: u64, aux: usize) -> Self {
        Self {
            word,
            aux,
            meta: AtomicUsize::new(kind as usize),
        }
    }

    #[inline]
    pub(crate) fn kind(&self) -> u8 {
        (self.meta.load(Ordering::Relaxed) & META_KIND) as u8
    }

    #[inline]
    fn has_tz(&self) -> bool {
        self.kind() == KIND_DATETIME
    }

    /// The owned `tzinfo` pointer (`null` for `None` and other kinds).
    #[inline]
    fn tz_ptr(&self) -> *const PyInstance {
        if self.has_tz() {
            self.aux as *const PyInstance
        } else {
            std::ptr::null()
        }
    }

    /// A new reference to the `tzinfo`.
    #[inline]
    fn tz(&self) -> Object {
        let p = self.tz_ptr();
        if p.is_null() {
            return Object::None;
        }
        // SAFETY: `aux` owns one strong reference; this adds another.
        unsafe {
            Rc::increment_strong_count(p);
            Object::Instance(Rc::from_raw(p))
        }
    }

    pub(crate) fn layout(&self) -> &'static SharedSlice<DictKey> {
        layout_of(self.kind())
    }

    /// The field objects (built on first use; see the type docs).
    pub(crate) fn values(&self) -> &[Object] {
        let len = self.layout().len();
        let m = self.meta.load(Ordering::Acquire);
        let ptr = if m & !META_KIND != 0 {
            (m & !META_KIND) as *const Object
        } else {
            self.materialize()
        };
        // SAFETY: a built copy holds exactly `len` objects and lives until
        // the storage is dropped, cleared or unpacked (all `&mut self`).
        unsafe { std::slice::from_raw_parts(ptr, len) }
    }

    #[cold]
    #[inline(never)]
    fn materialize(&self) -> *const Object {
        let vals = self.build_values().into_boxed_slice();
        let len = vals.len();
        let ptr = Box::into_raw(vals).cast::<Object>();
        let kind = self.kind() as usize;
        match self.meta.compare_exchange(
            kind,
            ptr as usize | kind,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => ptr,
            Err(cur) => {
                // Another reader built a copy first.
                // SAFETY: `ptr` came from `Box::into_raw` just above.
                drop(unsafe { Box::from_raw(std::ptr::slice_from_raw_parts_mut(ptr, len)) });
                (cur & !META_KIND) as *const Object
            }
        }
    }

    fn build_values(&self) -> Vec<Object> {
        let w = self.word;
        match self.kind() {
            KIND_TIMEDELTA => {
                let (d, s, us) = self.td();
                vec![
                    Object::Int(d),
                    Object::Int(s),
                    Object::Int(us),
                    Object::Int(-1),
                    Object::Int(d),
                    Object::Int(s),
                    Object::Int(us),
                ]
            }
            KIND_DATE => {
                let (y, m, d) = word_ymd(w);
                vec![
                    Object::Int(y),
                    Object::Int(m),
                    Object::Int(d),
                    Object::Int(-1),
                ]
            }
            _ => {
                let (y, m, d) = word_ymd(w);
                let (hh, mm, ss, us, fold) = word_time(w);
                vec![
                    Object::Int(y),
                    Object::Int(m),
                    Object::Int(d),
                    Object::Int(hh),
                    Object::Int(mm),
                    Object::Int(ss),
                    Object::Int(us),
                    self.tz(),
                    Object::Int(-1),
                    Object::Int(fold),
                ]
            }
        }
    }

    /// `(days, seconds, microseconds)` of a packed `timedelta`.
    #[inline]
    fn td(&self) -> (i64, i64, i64) {
        (
            self.word as i64,
            (self.aux >> 20) as i64,
            (self.aux & 0xF_FFFF) as i64,
        )
    }

    /// Take the field objects, releasing the packed form.
    pub(crate) fn into_values(self) -> Box<[Object]> {
        let this = std::mem::ManuallyDrop::new(self);
        let len = this.layout().len();
        let m = this.meta.load(Ordering::Acquire);
        let vals = if m & !META_KIND != 0 {
            // SAFETY: a built copy of `len` objects (see `materialize`),
            // owned by the storage being consumed.
            unsafe {
                Box::from_raw(std::ptr::slice_from_raw_parts_mut(
                    (m & !META_KIND) as *mut Object,
                    len,
                ))
            }
        } else {
            this.build_values().into_boxed_slice()
        };
        this.release_aux();
        vals
    }

    /// Release the owned `tzinfo` reference (the caller forgets `aux`).
    #[inline]
    fn release_aux(&self) {
        let p = self.tz_ptr();
        if !p.is_null() {
            // SAFETY: `aux` owns one strong reference.
            drop(unsafe { Rc::from_raw(p) });
        }
    }

    /// Release the built copy, if any.
    #[inline]
    fn free_values(&mut self) {
        let m = *self.meta.get_mut();
        if m & !META_KIND != 0 {
            let len = self.layout().len();
            *self.meta.get_mut() = m & META_KIND;
            // SAFETY: see `into_values`.
            drop(unsafe {
                Box::from_raw(std::ptr::slice_from_raw_parts_mut(
                    (m & !META_KIND) as *mut Object,
                    len,
                ))
            });
        }
    }

    /// Release everything the value owns, keeping the kind (a pooled
    /// instance's storage; see [`recycle`]).
    fn clear(&mut self) {
        self.release_aux();
        self.aux = 0;
        self.free_values();
    }
}

impl Drop for PackedSlots {
    fn drop(&mut self) {
        self.release_aux();
        self.free_values();
    }
}

impl Clone for PackedSlots {
    fn clone(&self) -> Self {
        let p = self.tz_ptr();
        if !p.is_null() {
            // SAFETY: `aux` keeps the allocation alive; the clone owns the
            // added reference.
            unsafe { Rc::increment_strong_count(p) };
        }
        Self::new(self.kind(), self.word, self.aux)
    }
}

impl std::fmt::Debug for PackedSlots {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PackedSlots")
            .field("kind", &self.kind())
            .field("word", &self.word)
            .field("aux", &self.aux)
            .finish_non_exhaustive()
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

/// Interned slot and field names.
struct Names {
    days: SharedStr,
    seconds: SharedStr,
    microseconds: SharedStr,
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
    f_days: SharedStr,
    f_seconds: SharedStr,
    f_microseconds: SharedStr,
    f_fromisoformat: SharedStr,
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
            f_days: interned("days"),
            f_seconds: interned("seconds"),
            f_microseconds: interned("microseconds"),
            f_fromisoformat: interned("fromisoformat"),
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
    td_layout: SharedSlice<DictKey>,
    date_layout: SharedSlice<DictKey>,
    dt_layout: SharedSlice<DictKey>,
    /// `(kind, name)` → the replaced Python implementation.
    orig: HashMap<(u8, &'static str), Object>,
    /// The `attr_version` at which each exact class was last verified to
    /// resolve the shortcut names (see [`verified`]) to what [`install`]
    /// left there; `0` = never.
    sealed: [std::sync::atomic::AtomicU64; 6],
    /// `(kind, name)` → what the class resolved the name to after
    /// [`install`]: the natives, and the field properties.
    expect: std::sync::OnceLock<Vec<(u8, &'static str, Object)>>,
    /// Dead instances kept for reuse (see [`Pool`]).
    pool: Pool,
    /// The native `datetime.fromisoformat` (the function its
    /// `classmethod` wraps).
    fromiso: std::sync::OnceLock<Object>,
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
    (KIND_TIMEDELTA, "__new__"),
    (KIND_TIMEDELTA, "__init__"),
    (KIND_TIMEDELTA, "days"),
    (KIND_TIMEDELTA, "seconds"),
    (KIND_TIMEDELTA, "microseconds"),
    (KIND_DATE, "__add__"),
    (KIND_DATE, "__sub__"),
    (KIND_DATE, "__eq__"),
    (KIND_DATE, "__ne__"),
    (KIND_DATE, "__lt__"),
    (KIND_DATE, "__le__"),
    (KIND_DATE, "__gt__"),
    (KIND_DATE, "__ge__"),
    (KIND_DATE, "__new__"),
    (KIND_DATE, "__init__"),
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
    (KIND_DATETIME, "__new__"),
    (KIND_DATETIME, "__init__"),
    (KIND_DATETIME, "year"),
    (KIND_DATETIME, "month"),
    (KIND_DATETIME, "day"),
    (KIND_DATETIME, "hour"),
    (KIND_DATETIME, "minute"),
    (KIND_DATETIME, "second"),
    (KIND_DATETIME, "microsecond"),
    (KIND_DATETIME, "tzinfo"),
    (KIND_DATETIME, "fold"),
    (KIND_DATETIME, "fromisoformat"),
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

/// The [`State`] a class marked by [`install`] carries in its
/// `native_ext` slot.
#[inline]
#[allow(clippy::cast_ptr_alignment)]
fn state_of_cls(cls: &TypeObject) -> Option<&State> {
    let ext = cls.native_ext.get()?;
    debug_assert!(ext.downcast_ref::<State>().is_some());
    // SAFETY: only `install` fills `native_ext`, always with a `State`
    // (checked above in debug builds), so the payload pointer is a
    // `State`'s, aligned for it; the cast drops the vtable.
    Some(unsafe { &*Rc::as_ptr(ext).cast::<State>() })
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

// The field readers take a natively built instance's values straight
// from the shared layout; any other storage is read by name.

#[allow(clippy::index_refutable_slice)]
#[inline]
fn td_fields(i: &PyInstance, st: &State) -> Option<(i64, i64, i64)> {
    with_slots(i, |s| {
        if let Some(p) = s.as_packed() {
            return (p.kind() == KIND_TIMEDELTA).then(|| p.td());
        }
        td_fields_slow(s, st)
    })
}

#[allow(clippy::index_refutable_slice)]
#[inline(never)]
fn td_fields_slow(s: &SlotStorage, st: &State) -> Option<(i64, i64, i64)> {
    let n = &st.names;
    if let Some(v) = s.values_for_layout(&st.td_layout) {
        return Some((
            as_int(&v[TD_DAYS])?,
            as_int(&v[TD_SECONDS])?,
            as_int(&v[TD_US])?,
        ));
    }
    Some((
        as_int(s.get_hinted(TD_DAYS, &n.days)?)?,
        as_int(s.get_hinted(TD_SECONDS, &n.seconds)?)?,
        as_int(s.get_hinted(TD_US, &n.microseconds)?)?,
    ))
}

fn td_us(f: (i64, i64, i64)) -> i128 {
    (i128::from(f.0) * 86_400 + i128::from(f.1)) * 1_000_000 + i128::from(f.2)
}

#[inline]
fn date_fields(i: &PyInstance, st: &State) -> Option<(i64, i64, i64)> {
    with_slots(i, |s| {
        if let Some(p) = s.as_packed() {
            return (p.kind() != KIND_TIMEDELTA).then(|| word_ymd(p.word));
        }
        date_fields_slow(s, st)
    })
}

#[allow(clippy::index_refutable_slice)]
#[inline(never)]
fn date_fields_slow(s: &SlotStorage, st: &State) -> Option<(i64, i64, i64)> {
    let n = &st.names;
    if let Some(v) = s
        .values_for_layout(&st.date_layout)
        .or_else(|| s.values_for_layout(&st.dt_layout))
    {
        return Some((
            as_int(&v[D_YEAR])?,
            as_int(&v[D_MONTH])?,
            as_int(&v[D_DAY])?,
        ));
    }
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

impl Dt {
    /// The fields of a packed `datetime` word, with `tz`.
    #[inline]
    fn from_word(w: u64, tz: Object) -> Self {
        let (y, m, d) = word_ymd(w);
        let (hh, mm, ss, us, fold) = word_time(w);
        Dt {
            y,
            m,
            d,
            hh,
            mm,
            ss,
            us,
            tz,
            fold,
        }
    }
}

#[inline]
fn dt_fields(i: &PyInstance, st: &State) -> Option<Dt> {
    with_slots(i, |s| {
        if let Some(p) = s.as_packed() {
            return (p.kind() == KIND_DATETIME).then(|| Dt::from_word(p.word, p.tz()));
        }
        dt_fields_slow(s, st)
    })
}

/// A packed `datetime`'s word and borrowed `tzinfo` pointer (`null` for
/// `None`): the comparison and difference paths, which keep no field.
#[inline]
fn dt_packed(i: &PyInstance) -> Option<(u64, *const PyInstance)> {
    with_slots(i, |s| {
        let p = s.as_packed()?;
        (p.kind() == KIND_DATETIME).then(|| (p.word, p.tz_ptr()))
    })
}

#[inline(never)]
fn dt_fields_slow(s: &SlotStorage, st: &State) -> Option<Dt> {
    let n = &st.names;
    if let Some(v) = s.values_for_layout(&st.dt_layout) {
        return Some(Dt {
            y: as_int(&v[D_YEAR])?,
            m: as_int(&v[D_MONTH])?,
            d: as_int(&v[D_DAY])?,
            hh: as_int(&v[DT_HOUR])?,
            mm: as_int(&v[DT_MINUTE])?,
            ss: as_int(&v[DT_SECOND])?,
            us: as_int(&v[DT_US])?,
            tz: v[DT_TZINFO].clone(),
            fold: as_int(&v[DT_FOLD])?,
        });
    }
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

/// [`tz_of`] for a packed `tzinfo` pointer (`null` for `None`).
#[inline]
fn tz_of_ptr(p: *const PyInstance, st: &State) -> Option<Tz> {
    if p.is_null() {
        return Some(Tz::Naive);
    }
    if std::ptr::eq(p, st.utc.as_ptr()) {
        return Some(Tz::Fixed(0));
    }
    // SAFETY: a borrowed view of the owner's reference, never dropped.
    let o = std::mem::ManuallyDrop::new(Object::Instance(unsafe { Rc::from_raw(p) }));
    tz_of(&o, st)
}

fn tz_of(tz: &Object, st: &State) -> Option<Tz> {
    let n = &st.names;
    match tz {
        Object::None => Some(Tz::Naive),
        Object::Instance(i) if std::ptr::eq(Rc::as_ptr(i), st.utc.as_ptr()) => Some(Tz::Fixed(0)),
        o => {
            let i = inst(o, KIND_TIMEZONE)?;
            let off = {
                let s = i.slots.try_borrow().ok()?;
                s.get_hinted(TZ_OFFSET, &n.offset)?.clone()
            };
            let td = inst(&off, KIND_TIMEDELTA)?;
            Some(Tz::Fixed(td_us(td_fields(td, st)?)))
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
fn instance_fixed(
    cls: Rc<TypeObject>,
    layout: &SharedSlice<DictKey>,
    values: Vec<Object>,
) -> Object {
    let mut i = PyInstance::new(cls);
    i.slots = RefCell::new(SlotStorage::from_layout(layout.clone(), values));
    Object::Instance(Rc::new(i))
}

/// `timedelta(...)`, `date(...)` and `datetime(...)` of the exact classes
/// with plain `int` fields (by position or keyword) and, for `datetime`, a
/// naive or fixed-offset zone: the instance the replaced `__new__` builds,
/// built natively. `None` for every other shape, which the Python
/// constructor serves (and diagnoses).
pub(crate) fn construct(
    cls: &Rc<TypeObject>,
    args: &[Object],
    kwargs: &[(String, Object)],
) -> Option<Result<Object, RuntimeError>> {
    construct_with(cls, args, kwargs.len(), |i| {
        (kwargs[i].0.as_str(), &kwargs[i].1)
    })
}

/// [`construct`] with the keyword names as the call's names tuple
/// (`names[i]` names `values[i]`).
pub(crate) fn construct_names(
    cls: &TypeObject,
    args: &[Object],
    names: &[Object],
    values: &[Object],
) -> Option<Result<Object, RuntimeError>> {
    if names.len() != values.len() {
        return None;
    }
    construct_with(cls, args, names.len(), |i| match &names[i] {
        Object::Str(s) => (s.as_ref(), &values[i]),
        _ => ("", &values[i]),
    })
}

/// The constructors' parameter names, by kind, in positional order.
const TD_PARAMS: [&str; 7] = [
    "days",
    "seconds",
    "microseconds",
    "milliseconds",
    "minutes",
    "hours",
    "weeks",
];
const DATE_PARAMS: [&str; 3] = ["year", "month", "day"];
const DT_PARAMS: [&str; 9] = [
    "year",
    "month",
    "day",
    "hour",
    "minute",
    "second",
    "microsecond",
    "tzinfo",
    "fold",
];

fn construct_with<'a>(
    cls: &TypeObject,
    args: &'a [Object],
    nkw: usize,
    kw: impl Fn(usize) -> (&'a str, &'a Object),
) -> Option<Result<Object, RuntimeError>> {
    let st = state_of_cls(cls)?;
    let kind = cls.native_kind.get();
    let (exact, params): (*const TypeObject, &[&str]) = match kind {
        KIND_TIMEDELTA => (st.timedelta.as_ptr(), &TD_PARAMS),
        KIND_DATE => (st.date.as_ptr(), &DATE_PARAMS),
        // `fold` is keyword-only.
        KIND_DATETIME => (st.datetime.as_ptr(), &DT_PARAMS),
        _ => return None,
    };
    if !std::ptr::eq(cls, exact) || !verified(st, cls, kind) {
        return None;
    }
    let npos = if kind == KIND_DATETIME {
        8
    } else {
        params.len()
    };
    if args.len() > npos {
        return None;
    }
    // Each parameter's argument, by position then keyword.
    let mut slots: [Option<&'a Object>; 9] = [None; 9];
    for (slot, a) in slots.iter_mut().zip(args) {
        *slot = Some(a);
    }
    for i in 0..nkw {
        let (name, v) = kw(i);
        let ix = params.iter().position(|p| *p == name)?;
        if slots[ix].is_some() {
            // A duplicate: the Python constructor raises.
            return None;
        }
        slots[ix] = Some(v);
    }
    let int = |ix: usize, default: Option<i64>| -> Option<i64> {
        match slots[ix] {
            Some(Object::Int(v)) => Some(*v),
            None => default,
            _ => None,
        }
    };
    match kind {
        KIND_TIMEDELTA => {
            let mut v = [0i128; 7];
            for (ix, slot) in v.iter_mut().enumerate() {
                *slot = i128::from(int(ix, Some(0))?);
            }
            let [days, seconds, us, ms, minutes, hours, weeks] = v;
            new_td(
                st,
                days + weeks * 7,
                seconds + minutes * 60 + hours * 3600,
                us + ms * 1000,
            )
        }
        KIND_DATE => {
            let (y, m, d) = (int(0, None)?, int(1, None)?, int(2, None)?);
            valid_date(y, m, d)
                .then(|| new_date(st, y, m, d))
                .flatten()
                .map(Ok)
        }
        _ => {
            let f = Dt {
                y: int(0, None)?,
                m: int(1, None)?,
                d: int(2, None)?,
                hh: int(3, Some(0))?,
                mm: int(4, Some(0))?,
                ss: int(5, Some(0))?,
                us: int(6, Some(0))?,
                tz: slots[7].cloned().unwrap_or(Object::None),
                fold: int(8, Some(0))?,
            };
            let ok = valid_date(f.y, f.m, f.d)
                && (0..24).contains(&f.hh)
                && (0..60).contains(&f.mm)
                && (0..60).contains(&f.ss)
                && (0..1_000_000).contains(&f.us)
                && (0..=1).contains(&f.fold);
            if !ok {
                return None;
            }
            tz_of(&f.tz, st)?;
            new_dt(st, f).map(Ok)
        }
    }
}

/// A normalized exact `timedelta` from unnormalized components.
fn new_td(st: &State, d: i128, s: i128, us: i128) -> Option<Result<Object, RuntimeError>> {
    let total = d * US_PER_DAY + s * 1_000_000 + us;
    // 64-bit division when it fits (128-bit division is a library call).
    let (days, rest) = match i64::try_from(total) {
        Ok(t) => {
            let per_day = US_PER_DAY as i64;
            (
                i128::from(t.div_euclid(per_day)),
                i128::from(t.rem_euclid(per_day)),
            )
        }
        Err(_) => (total.div_euclid(US_PER_DAY), total.rem_euclid(US_PER_DAY)),
    };
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
    let aux = ((secs as usize) << 20) | us as usize;
    Some(Ok(new_packed(
        st,
        &st.timedelta,
        KIND_TIMEDELTA,
        days as u64,
        aux,
    )?))
}

fn new_date(st: &State, y: i64, m: i64, d: i64) -> Option<Object> {
    if !valid_date(y, m, d) {
        let cls = st.date.upgrade()?;
        return Some(instance_fixed(
            cls,
            &st.date_layout,
            vec![
                Object::Int(y),
                Object::Int(m),
                Object::Int(d),
                Object::Int(-1),
            ],
        ));
    }
    let word = ((y as u64) << W_YEAR) | ((m as u64) << W_MONTH) | ((d as u64) << W_DAY);
    new_packed(st, &st.date, KIND_DATE, word, 0)
}

fn new_dt(st: &State, f: Dt) -> Option<Object> {
    let ok = valid_date(f.y, f.m, f.d)
        && (0..24).contains(&f.hh)
        && (0..60).contains(&f.mm)
        && (0..60).contains(&f.ss)
        && (0..1_000_000).contains(&f.us)
        && (0..=1).contains(&f.fold);
    if ok {
        let word = pack_dt(&f);
        match f.tz {
            Object::None => return new_packed(st, &st.datetime, KIND_DATETIME, word, 0),
            Object::Instance(tz) => {
                let aux = Rc::into_raw(tz) as usize;
                let r = new_packed(st, &st.datetime, KIND_DATETIME, word, aux);
                if r.is_none() {
                    // SAFETY: not adopted; release the reference taken above.
                    drop(unsafe { Rc::from_raw(aux as *const PyInstance) });
                }
                return r;
            }
            _ => {}
        }
    }
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
            f.tz,
            Object::Int(-1),
            Object::Int(f.fold),
        ],
    ))
}

// ---------------------------------------------------------------------
// The instance pool: CPython's C types keep per-type freelists; a dead
// natively served instance keeps its allocation, class and packed
// storage here for the next value of its kind.

const POOL_CAP: usize = 64;

/// One class set's pooled instances, per kind (`timedelta`, `date`,
/// `datetime`). A pooled instance keeps its class, so the pool (in the
/// classes' shared [`State`]) and the classes keep each other alive, as
/// CPython's static types live for the whole process.
#[derive(Default)]
struct Pool(std::cell::UnsafeCell<[Vec<Rc<PyInstance>>; 3]>);

// SAFETY: the pool is touched only while `pool_ok()`: then every thread
// that reaches VM objects holds the GIL, which serializes the accesses
// (each is a push or a pop that runs no other code).
unsafe impl Sync for Pool {}
unsafe impl Send for Pool {}

/// Whether the pools may be used: under the GIL with no thread reaching
/// objects outside it (see `sync::cells_unguarded`). Debug builds (the
/// unit-test binary runs interpreters on concurrent threads with no GIL
/// between them) never pool.
#[inline]
fn pool_ok() -> bool {
    #[cfg(debug_assertions)]
    {
        false
    }
    #[cfg(not(debug_assertions))]
    {
        !crate::sync::cells_unguarded()
    }
}

impl Pool {
    #[inline(always)]
    fn with<R>(&self, kind: u8, f: impl FnOnce(&mut Vec<Rc<PyInstance>>) -> R) -> Option<R> {
        if !pool_ok() {
            return None;
        }
        let ix = match kind {
            KIND_TIMEDELTA => 0,
            KIND_DATE => 1,
            KIND_DATETIME => 2,
            _ => return None,
        };
        // SAFETY: see the `Sync` impl; `f` only pushes or pops.
        Some(f(unsafe { &mut (*self.0.get())[ix] }))
    }
}

/// An instance of `cls` (the exact class of `kind`) holding the packed
/// value `(word, aux)`; `aux` is adopted on success.
#[inline]
fn new_packed(
    st: &State,
    cls: &Weak<TypeObject>,
    kind: u8,
    word: u64,
    aux: usize,
) -> Option<Object> {
    if let Some(inst) = st.pool.with(kind, Vec::pop).flatten() {
        // SAFETY: pooled instances are unique (see `recycle`): nothing
        // else can observe the class or storage while they change.
        let m = unsafe { &mut *Rc::as_ptr(&inst).cast_mut() };
        if std::ptr::eq(Rc::as_ptr(m.class.get_mut()), cls.as_ptr()) {
            if let Some(p) = m.slots.get_mut().as_packed_mut() {
                p.word = word;
                p.aux = aux;
                return Some(Object::Instance(inst));
            }
        }
        // Not this class's (a `__class__` assignment cannot reach a pooled
        // instance, so this does not happen): free it outright.
        drop(Rc::into_arc(inst));
    }
    let cls = cls.upgrade()?;
    let mut i = PyInstance::new(cls);
    *i.slots.get_mut() = SlotStorage::from_packed(PackedSlots::new(kind, word, aux));
    Some(Object::Instance(Rc::new(i)))
}

/// Whether `i`'s class keeps a pool (see [`recycle`]).
#[inline]
pub(crate) fn pooled_kind(i: &PyInstance) -> bool {
    // SAFETY: a read between two instructions (see `GilCell::peek`).
    unsafe { i.class.peek() }.is_some_and(|c| {
        matches!(
            c.native_kind.get(),
            KIND_TIMEDELTA | KIND_DATE | KIND_DATETIME
        )
    })
}

/// Retire a dying natively served instance into its class set's pool
/// (see above). `Err` hands back an instance that must be freed normally
/// (dropped as an `Arc`, not through the finalizing `Rc` drop that may
/// have called here).
pub(crate) fn recycle(mut inst: Rc<PyInstance>) -> Result<(), Rc<PyInstance>> {
    if !pool_ok() {
        return Err(inst);
    }
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
    let kind = cls.native_kind.get();
    if !matches!(kind, KIND_TIMEDELTA | KIND_DATE | KIND_DATETIME) || cls.instances_need_finalize()
    {
        return Err(inst);
    }
    let Some(st) = state_of_cls(cls) else {
        return Err(inst);
    };
    if m.dict.published().is_some()
        || !m.dict.split_mut().is_empty()
        || m.native.get().is_some()
        || m.c_body.get() != 0
        || m.finalize_ran.get()
    {
        return Err(inst);
    }
    match m.slots.get_mut().as_packed_mut() {
        Some(p) if p.kind() == kind => {
            // Releasing the `tzinfo` runs no code (a dying one is queued
            // for finalization, not finalized here).
            p.clear();
        }
        _ => return Err(inst),
    }
    m.hash_cache = crate::sync::CachedHash::new(None);
    // SAFETY: `st` lives in the class `inst` holds.
    let st: &State = unsafe { &*std::ptr::from_ref(st) };
    let mut slot = Some(inst);
    st.pool.with(kind, |v| {
        if v.len() < POOL_CAP {
            v.extend(slot.take());
        }
    });
    match slot {
        None => Ok(()),
        Some(inst) => Err(inst),
    }
}

/// `datetime` fields shifted by `delta_us` (fold cleared, `tzinfo`
/// kept), or `None` past the representable range.
fn dt_shift(f: Dt, delta_us: i128) -> Option<Dt> {
    // Every representable datetime is under 2^59 microseconds, so a
    // delta that fits no `i64` lands out of range (the Python path
    // raises); the rest is 64-bit arithmetic over in-range fields.
    let in_range = (1..=9999).contains(&f.y)
        && valid_date(f.y, f.m, f.d)
        && (0..24).contains(&f.hh)
        && (0..60).contains(&f.mm)
        && (0..60).contains(&f.ss)
        && (0..1_000_000).contains(&f.us);
    if !in_range {
        return None;
    }
    let base =
        (ymd2ord(f.y, f.m, f.d) * 86_400 + f.hh * 3600 + f.mm * 60 + f.ss) * 1_000_000 + f.us;
    let total = base.checked_add(i64::try_from(delta_us).ok()?)?;
    let per_day = US_PER_DAY as i64;
    let ord = total.div_euclid(per_day);
    if !(1..=MAX_ORDINAL).contains(&ord) {
        return None;
    }
    let rest = total.rem_euclid(per_day);
    let (y, m, d) = ord2ymd(ord);
    let secs = (rest / 1_000_000) as i64;
    Some(Dt {
        y,
        m,
        d,
        hh: secs / 3600,
        mm: secs % 3600 / 60,
        ss: secs % 60,
        us: (rest % 1_000_000) as i64,
        tz: f.tz,
        fold: 0,
    })
}

/// Microseconds since 0001-01-01T00:00 of a packed (so valid) word.
#[inline]
fn word_us(w: u64) -> i64 {
    let (y, m, d) = word_ymd(w);
    let (hh, mm, ss, us, _) = word_time(w);
    (ymd2ord(y, m, d) * 86_400 + hh * 3600 + mm * 60 + ss) * 1_000_000 + us
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
    let p = td_fields(inst(x, KIND_TIMEDELTA)?, st)?;
    let q = td_fields(inst(y, KIND_TIMEDELTA)?, st)?;
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
    let p = td_fields(inst(x, KIND_TIMEDELTA)?, st)?;
    let q = td_fields(inst(y, KIND_TIMEDELTA)?, st)?;
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
    let p = td_fields(inst(x, KIND_TIMEDELTA)?, st)?;
    new_td(st, -i128::from(p.0), -i128::from(p.1), -i128::from(p.2))
}

fn td_mul(a: &[Object]) -> Option<Result<Object, RuntimeError>> {
    let [x, k] = a else { return None };
    let k = i128::from(as_int(k)?);
    let st = state_of(x)?;
    let p = td_fields(inst(x, KIND_TIMEDELTA)?, st)?;
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
    let p = td_fields(inst(x, KIND_TIMEDELTA)?, st)?;
    Some(Ok(Object::Bool(p != (0, 0, 0))))
}

/// `hash((days, seconds, microseconds))`: the replaced `__hash__`'s
/// value for a `timedelta` (`hash(self._getstate())`).
fn td_tuple_hash(d: i64, s: i64, us: i64) -> Option<i64> {
    let h = |v: i64| crate::object::numeric_hash(&Object::Int(v));
    Some(crate::object::combine_tuple_hash(&[h(d)?, h(s)?, h(us)?]))
}

fn td_hash(a: &[Object]) -> Option<Result<Object, RuntimeError>> {
    let [x] = a else { return None };
    let st = state_of(x)?;
    let (d, s, us) = td_fields(inst(x, KIND_TIMEDELTA)?, st)?;
    Some(Ok(Object::Int(td_tuple_hash(d, s, us)?)))
}

fn date_hash(a: &[Object]) -> Option<Result<Object, RuntimeError>> {
    let [x] = a else { return None };
    let st = state_of(x)?;
    let (y, m, d) = date_fields(inst(x, KIND_DATE)?, st)?;
    if !valid_date(y, m, d) {
        return None;
    }
    // `hash(self._getstate())`: a one-tuple of the state bytes.
    let state = [(y / 256) as u8, (y % 256) as u8, m as u8, d as u8];
    let h = crate::object::py_bytes_hash(&state);
    Some(Ok(Object::Int(crate::object::combine_tuple_hash(&[h]))))
}

/// The replaced `datetime.__hash__` for a naive or fixed-offset value:
/// the state bytes' hash when naive, else the hash of the UTC value as a
/// `timedelta` since 0001-01-01 (`fold` never changes a fixed offset).
fn dt_hash(a: &[Object]) -> Option<Result<Object, RuntimeError>> {
    let [x] = a else { return None };
    let st = state_of(x)?;
    let f = dt_fields(inst(x, KIND_DATETIME)?, st)?;
    if !valid_date(f.y, f.m, f.d) {
        return None;
    }
    let h = match tz_of(&f.tz, st)? {
        Tz::Naive => {
            let state = [
                (f.y / 256) as u8,
                (f.y % 256) as u8,
                f.m as u8,
                f.d as u8,
                f.hh as u8,
                f.mm as u8,
                f.ss as u8,
                (f.us >> 16) as u8,
                (f.us >> 8 & 0xff) as u8,
                (f.us & 0xff) as u8,
            ];
            crate::object::py_bytes_hash(&state)
        }
        Tz::Fixed(off) => {
            let total = dt_us(&f) - off;
            let days = total.div_euclid(US_PER_DAY);
            let rest = total.rem_euclid(US_PER_DAY);
            td_tuple_hash(
                i64::try_from(days).ok()?,
                (rest / 1_000_000) as i64,
                (rest % 1_000_000) as i64,
            )?
        }
    };
    Some(Ok(Object::Int(h)))
}

fn td_cmp(a: &[Object]) -> Option<std::cmp::Ordering> {
    let [x, y] = a else { return None };
    let st = state_of(x)?;
    let p = td_fields(inst(x, KIND_TIMEDELTA)?, st)?;
    let q = td_fields(inst(y, KIND_TIMEDELTA)?, st)?;
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
    let (y, m, d) = date_fields(i, st)?;
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
    let (yy, m, d) = date_fields(inst(x, KIND_DATE)?, st)?;
    let (days, _, _) = td_fields(inst(y, KIND_TIMEDELTA)?, st)?;
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
    let (y1, m1, d1) = date_fields(inst(x, KIND_DATE)?, st)?;
    let (y2, m2, d2) = date_fields(inst(y, KIND_DATE)?, st)?;
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
    let p = date_fields(inst(x, KIND_DATE)?, st)?;
    let q = date_fields(inst(y, KIND_DATE)?, st)?;
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
    let (y, m, d) = date_fields(inst(x, KIND_DATE)?, st)?;
    if !valid_date(y, m, d) {
        return None;
    }
    let mut out = Vec::with_capacity(10);
    push_ymd(&mut out, y, m, d);
    Some(Ok(text_object(&out)))
}

/// Append `v` in decimal, zero-padded to `width` digits:
/// `format!("{v:0width$}")` without the formatting machinery.
#[inline]
fn push_num(out: &mut Vec<u8>, v: i64, width: usize) {
    const LIMIT: [i64; 7] = [1, 10, 100, 1_000, 10_000, 100_000, 1_000_000];
    if width < LIMIT.len() && (0..LIMIT[width]).contains(&v) {
        // Exactly `width` digits.
        let start = out.len();
        out.resize(start + width, b'0');
        let mut n = v as u32;
        for b in out[start..].iter_mut().rev() {
            *b = b'0' + (n % 10) as u8;
            n /= 10;
        }
        return;
    }
    let mut buf = [b'0'; 20];
    let mut n = v.unsigned_abs();
    let mut i = buf.len();
    loop {
        i -= 1;
        buf[i] = b'0' + (n % 10) as u8;
        n /= 10;
        if n == 0 {
            break;
        }
    }
    let start = i.min(buf.len() - width.min(buf.len()));
    if v < 0 {
        out.push(b'-');
    }
    out.extend_from_slice(&buf[start..]);
}

/// `YYYY-MM-DD`.
#[inline]
fn push_ymd(out: &mut Vec<u8>, y: i64, m: i64, d: i64) {
    push_num(out, y, 4);
    out.push(b'-');
    push_num(out, m, 2);
    out.push(b'-');
    push_num(out, d, 2);
}

/// `HH:MM:SS`.
#[inline]
fn push_hms(out: &mut Vec<u8>, hh: i64, mm: i64, ss: i64) {
    push_num(out, hh, 2);
    out.push(b':');
    push_num(out, mm, 2);
    out.push(b':');
    push_num(out, ss, 2);
}

/// A `str` of `text` (UTF-8: ASCII fields around the caller's text).
#[inline]
fn text_object(text: &[u8]) -> Object {
    // SAFETY: the formatters write ASCII and copy whole UTF-8 sequences.
    let text = unsafe { std::str::from_utf8_unchecked(text) };
    if text.is_ascii() {
        Object::Str(SharedStr::from_ascii(text))
    } else {
        Object::Str(SharedStr::from(text))
    }
}

/// `date.replace(year=None, month=None, day=None)`, positional form.
fn date_replace(a: &[Object]) -> Option<Result<Object, RuntimeError>> {
    let (x, rest) = a.split_first()?;
    let st = state_of(x)?;
    let (mut y, mut m, mut d) = date_fields(inst(x, KIND_DATE)?, st)?;
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

/// A packed `datetime` shifted by `delta_us` within its day: the same
/// date, new time fields, fold cleared, the `tzinfo` shared. `None` when
/// `xi` is not packed or the result leaves the day.
#[inline]
fn dt_shift_in_day(st: &State, xi: &PyInstance, delta_us: i128) -> Option<Object> {
    let (w, tz) = dt_packed(xi)?;
    if delta_us.unsigned_abs() >= US_PER_DAY as u128 {
        return None;
    }
    let (hh, mm, ss, us, _) = word_time(w);
    let n = ((hh * 3600 + mm * 60 + ss) * 1_000_000 + us) + delta_us as i64;
    if !(0..US_PER_DAY as i64).contains(&n) {
        return None;
    }
    let (secs, us) = (n / 1_000_000, n % 1_000_000);
    let word = (w & !((1u64 << W_DAY) - 1))
        | (((secs / 3600) as u64) << W_HH)
        | (((secs % 3600 / 60) as u64) << W_MM)
        | (((secs % 60) as u64) << W_SS)
        | ((us as u64) << W_US);
    if !tz.is_null() {
        // SAFETY: `xi` holds a reference; the new value owns this one.
        unsafe { Rc::increment_strong_count(tz) };
    }
    let r = new_packed(st, &st.datetime, KIND_DATETIME, word, tz as usize);
    if r.is_none() && !tz.is_null() {
        // SAFETY: not adopted; release the reference taken above.
        drop(unsafe { Rc::from_raw(tz) });
    }
    r
}

fn dt_add(a: &[Object]) -> Option<Result<Object, RuntimeError>> {
    let [x, y] = a else { return None };
    let st = state_of(x)?;
    let xi = inst(x, KIND_DATETIME)?;
    let delta = td_us(td_fields(inst(y, KIND_TIMEDELTA)?, st)?);
    if let Some(r) = dt_shift_in_day(st, xi, delta) {
        return Some(Ok(r));
    }
    let f = dt_fields(xi, st)?;
    if !valid_date(f.y, f.m, f.d) {
        return None;
    }
    match dt_shift(f, delta) {
        Some(g) => Some(Ok(new_dt(st, g)?)),
        None => Some(Err(overflow_error("date value out of range"))),
    }
}

fn dt_sub(a: &[Object]) -> Option<Result<Object, RuntimeError>> {
    let [x, y] = a else { return None };
    let st = state_of(x)?;
    let xi = inst(x, KIND_DATETIME)?;
    match kind_of(y) {
        KIND_TIMEDELTA => {
            let delta = td_us(td_fields(inst(y, KIND_TIMEDELTA)?, st)?);
            if let Some(r) = dt_shift_in_day(st, xi, -delta) {
                return Some(Ok(r));
            }
            let f = dt_fields(xi, st)?;
            if !valid_date(f.y, f.m, f.d) {
                return None;
            }
            match dt_shift(f, -delta) {
                Some(g) => Some(Ok(new_dt(st, g)?)),
                None => Some(Err(overflow_error("date value out of range"))),
            }
        }
        KIND_DATETIME => {
            let j = inst(y, KIND_DATETIME)?;
            if let (Some((wf, tf)), Some((wg, tg))) = (dt_packed(xi), dt_packed(j)) {
                let mut diff = i128::from(word_us(wf) - word_us(wg));
                if !std::ptr::eq(tf, tg) {
                    match (tz_of_ptr(tf, st)?, tz_of_ptr(tg, st)?) {
                        (Tz::Naive, Tz::Naive) => {}
                        (Tz::Fixed(a), Tz::Fixed(b)) => diff += b - a,
                        _ => return None,
                    }
                }
                return new_td(st, 0, 0, diff);
            }
            let f = dt_fields(xi, st)?;
            if !valid_date(f.y, f.m, f.d) {
                return None;
            }
            let g = dt_fields(j, st)?;
            if !valid_date(g.y, g.m, g.d) {
                return None;
            }
            let mut diff = dt_us(&f) - dt_us(&g);
            if !f.tz.is_same(&g.tz) {
                match (tz_of(&f.tz, st)?, tz_of(&g.tz, st)?) {
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
    let (xi, yi) = (inst(x, KIND_DATETIME)?, inst(y, KIND_DATETIME)?);
    if let (Some((wf, tf)), Some((wg, tg))) = (dt_packed(xi), dt_packed(yi)) {
        // The fields sit in significance order above the fold bit.
        if std::ptr::eq(tf, tg) {
            return Some(((wf >> 1).cmp(&(wg >> 1)), true));
        }
        return match (tz_of_ptr(tf, st)?, tz_of_ptr(tg, st)?) {
            (Tz::Naive, Tz::Naive) => Some(((wf >> 1).cmp(&(wg >> 1)), true)),
            (Tz::Fixed(a), Tz::Fixed(b)) => Some((
                (i128::from(word_us(wf)) - a).cmp(&(i128::from(word_us(wg)) - b)),
                true,
            )),
            _ => Some((std::cmp::Ordering::Equal, false)),
        };
    }
    let f = dt_fields(xi, st)?;
    let g = dt_fields(yi, st)?;
    if !valid_date(f.y, f.m, f.d) || !valid_date(g.y, g.m, g.d) {
        return None;
    }
    if f.tz.is_same(&g.tz) {
        return Some((dt_us(&f).cmp(&dt_us(&g)), true));
    }
    match (tz_of(&f.tz, st)?, tz_of(&g.tz, st)?) {
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
    let (y, m, d) = date_fields(inst(x, KIND_DATETIME)?, st)?;
    if !valid_date(y, m, d) {
        return None;
    }
    Some(Ok(new_date(st, y, m, d)?))
}

/// `_format_offset(off, sep)` for a fixed offset in microseconds.
fn format_offset(out: &mut Vec<u8>, off: i128, sep: &[u8]) {
    let (sign, mut v) = if off < 0 { (b'-', -off) } else { (b'+', off) };
    let hh = (v / 3_600_000_000) as i64;
    v %= 3_600_000_000;
    let mm = (v / 60_000_000) as i64;
    v %= 60_000_000;
    out.push(sign);
    push_num(out, hh, 2);
    out.extend_from_slice(sep);
    push_num(out, mm, 2);
    if v != 0 {
        out.extend_from_slice(sep);
        push_num(out, (v / 1_000_000) as i64, 2);
        if v % 1_000_000 != 0 {
            out.push(b'.');
            push_num(out, (v % 1_000_000) as i64, 6);
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
    let f = dt_fields(inst(x, KIND_DATETIME)?, st)?;
    if !valid_date(f.y, f.m, f.d) {
        return None;
    }
    let tz = tz_of(&f.tz, st)?;
    let mut out = Vec::with_capacity(40);
    push_ymd(&mut out, f.y, f.m, f.d);
    out.extend_from_slice(sep.encode_utf8(&mut [0; 4]).as_bytes());
    match spec {
        "hours" => push_num(&mut out, f.hh, 2),
        "minutes" => {
            push_num(&mut out, f.hh, 2);
            out.push(b':');
            push_num(&mut out, f.mm, 2);
        }
        "milliseconds" => {
            push_hms(&mut out, f.hh, f.mm, f.ss);
            out.push(b'.');
            push_num(&mut out, f.us / 1000, 3);
        }
        "microseconds" => {
            push_hms(&mut out, f.hh, f.mm, f.ss);
            out.push(b'.');
            push_num(&mut out, f.us, 6);
        }
        "seconds" => push_hms(&mut out, f.hh, f.mm, f.ss),
        _ => {
            push_hms(&mut out, f.hh, f.mm, f.ss);
            if f.us != 0 {
                out.push(b'.');
                push_num(&mut out, f.us, 6);
            }
        }
    }
    if let Tz::Fixed(off) = tz {
        format_offset(&mut out, off, b":");
    }
    Some(Ok(text_object(&out)))
}

/// `strftime` for the numeric directives (no locale involvement); any
/// other directive declines. The result is UTF-8: literal text is copied
/// byte for byte, and a directive is ASCII.
fn strftime_numeric(
    y: i64,
    m: i64,
    d: i64,
    time: Option<(i64, i64, i64, i64)>,
    tz: Option<&Tz>,
    fmt: &str,
) -> Option<Vec<u8>> {
    let (hh, mm, ss, us) = time.unwrap_or((0, 0, 0, 0));
    let fmt = fmt.as_bytes();
    let mut out = Vec::with_capacity(fmt.len() + 16);
    let mut i = 0;
    while i < fmt.len() {
        let c = fmt[i];
        i += 1;
        if c != b'%' {
            out.push(c);
            continue;
        }
        let k = *fmt.get(i)?;
        i += 1;
        match k {
            b'Y' => push_num(&mut out, y, 4),
            b'm' => push_num(&mut out, m, 2),
            b'd' => push_num(&mut out, d, 2),
            b'H' => push_num(&mut out, hh, 2),
            b'M' => push_num(&mut out, mm, 2),
            b'S' => push_num(&mut out, ss, 2),
            b'f' => push_num(&mut out, us, 6),
            b'y' => push_num(&mut out, y % 100, 2),
            b'j' => {
                let doy = DAYS_BEFORE_MONTH[(m - 1) as usize] + i64::from(m > 2 && is_leap(y)) + d;
                push_num(&mut out, doy, 3);
            }
            b'I' => {
                let h12 = match hh % 12 {
                    0 => 12,
                    h => h,
                };
                push_num(&mut out, h12, 2);
            }
            b'F' => push_ymd(&mut out, y, m, d),
            b'T' => push_hms(&mut out, hh, mm, ss),
            b'%' => out.push(b'%'),
            b'z' => match tz? {
                Tz::Naive => {}
                Tz::Fixed(off) => format_offset(&mut out, *off, b""),
            },
            b':' => {
                if *fmt.get(i)? != b'z' {
                    return None;
                }
                i += 1;
                match tz? {
                    Tz::Naive => {}
                    Tz::Fixed(off) => format_offset(&mut out, *off, b":"),
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
    let f = dt_fields(inst(x, KIND_DATETIME)?, st)?;
    if !valid_date(f.y, f.m, f.d) {
        return None;
    }
    let tz = tz_of(&f.tz, st)?;
    let s = strftime_numeric(
        f.y,
        f.m,
        f.d,
        Some((f.hh, f.mm, f.ss, f.us)),
        Some(&tz),
        fmt,
    )?;
    Some(Ok(text_object(&s)))
}

fn date_strftime(a: &[Object]) -> Option<Result<Object, RuntimeError>> {
    let [x, Object::Str(fmt)] = a else {
        return None;
    };
    let st = state_of(x)?;
    let (y, m, d) = date_fields(inst(x, KIND_DATE)?, st)?;
    if !valid_date(y, m, d) {
        return None;
    }
    // A date has no time or offset: `%z` formats as empty, like CPython.
    let s = strftime_numeric(y, m, d, None, Some(&Tz::Naive), fmt)?;
    Some(Ok(text_object(&s)))
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
    Some(Ok(new_dt(st, f)?))
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
        kind @ (KIND_DATETIME | KIND_TIME) => with_slots(i, |s| {
            if let Some(p) = s.as_packed() {
                let tz = p.tz_ptr();
                // SAFETY: `tz` is owned by the packed value being read.
                return Some(
                    tz.is_null() || unsafe { &*tz }.cls_raw().native_kind.get() == KIND_TIMEZONE,
                );
            }
            let idx = if kind == KIND_DATETIME { DT_TZINFO } else { 4 };
            Some(match s.get_hinted(idx, "_tzinfo") {
                Some(Object::None) => true,
                Some(o @ Object::Instance(_)) => kind_of(o) == KIND_TIMEZONE,
                _ => false,
            })
        })
        .unwrap_or(false),
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
    // SAFETY: a shallow copy the fast halves only read; never dropped.
    let args = std::mem::ManuallyDrop::new(unsafe { [std::ptr::read(a), std::ptr::read(b)] });
    match (kind, op) {
        (KIND_TIMEDELTA, B::Add) => td_add(&args[..]),
        (KIND_TIMEDELTA, B::Sub) => td_sub(&args[..]),
        (KIND_TIMEDELTA, B::Mult) => td_mul(&args[..]),
        (KIND_DATE, B::Add) => date_add(&args[..]),
        (KIND_DATE, B::Sub) => date_sub(&args[..]),
        (KIND_DATETIME, B::Add) => dt_add(&args[..]),
        (KIND_DATETIME, B::Sub) => dt_sub(&args[..]),
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
    // SAFETY: a shallow copy the fast halves only read; never dropped.
    let args = std::mem::ManuallyDrop::new(unsafe { [std::ptr::read(a), std::ptr::read(b)] });
    let r = match kind {
        KIND_TIMEDELTA => td_cmp(&args[..]).map(|o| (o, true)),
        KIND_DATE => date_cmp(&args[..]).map(|o| (o, true)),
        KIND_DATETIME => dt_cmp_raw(&args[..]),
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

/// The function under the native class method `name` of the exact class
/// `cls` while the class still has it (`datetime.fromisoformat`), for
/// the dispatch loop to bind to `cls` without the descriptor protocol.
pub(crate) fn type_classmethod(cls: &TypeObject, name: &SharedStr) -> Option<Object> {
    let kind = cls.native_kind.get();
    if kind != KIND_DATETIME {
        return None;
    }
    let st = state_of_cls(cls)?;
    if !SharedStr::ptr_eq(name, &st.names.f_fromisoformat)
        || !std::ptr::eq(cls, st.datetime.as_ptr())
        || !verified(st, cls, kind)
    {
        return None;
    }
    st.fromiso.get().cloned()
}

/// The fast half of `function` bound to `receiver` when that is a native
/// class method of its exact class (see [`type_classmethod`]); the
/// caller passes the receiver first.
pub(crate) fn bound_fast(function: &Object, receiver: &Object) -> Option<Fast> {
    let (Object::Builtin(f), Object::Type(cls)) = (function, receiver) else {
        return None;
    };
    if cls.native_kind.get() != KIND_DATETIME {
        return None;
    }
    match state_of_cls(cls)?.fromiso.get()? {
        Object::Builtin(g) if Rc::ptr_eq(f, g) => Some(dt_fromisoformat),
        _ => None,
    }
}

/// A public field (`dt.hour`, `d.year`, …) of a natively served instance
/// whose class still has its original property.
pub(crate) fn leaf_field(i: &PyInstance, name: &SharedStr) -> Option<Object> {
    let cls = i.cls_raw();
    let kind = cls.native_kind.get();
    if kind != KIND_DATE && kind != KIND_DATETIME && kind != KIND_TIMEDELTA {
        return None;
    }
    let st = state_of_cls(cls)?;
    if !verified(st, cls, kind) {
        return None;
    }
    let n = &st.names;
    if kind == KIND_TIMEDELTA {
        let ix = if SharedStr::ptr_eq(name, &n.f_days) {
            0
        } else if SharedStr::ptr_eq(name, &n.f_seconds) {
            1
        } else if SharedStr::ptr_eq(name, &n.f_microseconds) {
            2
        } else {
            return None;
        };
        // The public slots themselves (a Python-built value's are written
        // apart from the private ones).
        return with_slots(i, |s| match s.as_packed() {
            Some(p) if p.kind() == KIND_TIMEDELTA => {
                let (d, s, us) = p.td();
                Some(Object::Int([d, s, us][ix]))
            }
            Some(_) => None,
            None => Some(s.get_hinted(4 + ix, name)?.clone()),
        });
    }
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
    with_slots(i, |s| {
        if let Some(p) = s.as_packed() {
            let w = p.word;
            return Some(match idx {
                D_YEAR => Object::Int(bits(w, W_YEAR, 14)),
                D_MONTH => Object::Int(bits(w, W_MONTH, 4)),
                D_DAY => Object::Int(bits(w, W_DAY, 5)),
                DT_HOUR => Object::Int(bits(w, W_HH, 5)),
                DT_MINUTE => Object::Int(bits(w, W_MM, 6)),
                DT_SECOND => Object::Int(bits(w, W_SS, 6)),
                DT_US => Object::Int(bits(w, W_US, 20)),
                DT_FOLD => Object::Int(bits(w, W_FOLD, 1)),
                _ => p.tz(),
            });
        }
        Some(s.get_hinted(idx, slot)?.clone())
    })
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
/// A keyword call of a native method whose fast half takes its optional
/// parameters by position, `None` standing for an omitted one: the
/// positional argument list (receiver first), or `None` for the replaced
/// Python method (unknown or repeated names, other methods).
fn kw_positional(
    key: (u8, &'static str),
    a: &[Object],
    kw: &[(String, Object)],
) -> Option<Vec<Object>> {
    let params: &[&str] = match key {
        (KIND_DATE, "replace") => &["year", "month", "day"],
        _ => return None,
    };
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
    for o in &mut out {
        if matches!(o, Object::Unbound) {
            *o = Object::None;
        }
    }
    Some(out)
}

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
        name: "__hash__",
        fast: td_hash,
        classmethod: false,
    },
    Spec {
        kind: KIND_DATE,
        name: "__hash__",
        fast: date_hash,
        classmethod: false,
    },
    Spec {
        kind: KIND_DATETIME,
        name: "__hash__",
        fast: dt_hash,
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
    let [td_layout, date_layout, dt_layout] = layouts().clone();
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
        pool: Pool::default(),
        fromiso: std::sync::OnceLock::new(),
    });
    // Each class carries the state (a class in `native_ext` resolves to
    // it from any of its instances).
    for (kind, cls) in &classes {
        let _ = cls
            .native_ext
            .set(crate::rc_unsize!(state.clone() => dyn std::any::Any + Send + Sync));
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
                } else if let Some(r) = kw_positional(key, a, kw).and_then(|a| fast(&a)) {
                    return r;
                }
                let f = st_kw.orig.get(&key).cloned().unwrap_or(Object::None);
                with_interp(|i| i.call_object(f, a, kw))
            })),
        });
        crate::leaf_builtins::register_fast(&b, fast);
        let value = if spec.classmethod {
            if spec.name == "fromisoformat" {
                let _ = state.fromiso.set(Object::Builtin(b.clone()));
            }
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

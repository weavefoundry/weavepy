//! The `_datetime` built-in module — the low-level helper backing
//! the user-facing `datetime` frozen Python module.
//!
//! We export a flat collection of pure functions that produce
//! component tuples and epoch arithmetic. The frozen `datetime`
//! module on top builds the canonical `date` / `time` / `datetime` /
//! `timedelta` / `timezone` classes around these primitives.
//!
//! All component tuples are 8-element:
//!   `(year, month, day, hour, minute, second, microsecond,
//!     utc_offset_seconds)`
//!
//! Where `utc_offset_seconds` is `None` for naive times. CPython's
//! `_datetime` is C-only; we follow the same split.

use crate::sync::Rc;
use crate::sync::RefCell;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::error::{type_error, value_error, RuntimeError};
use crate::import::ModuleCache;
use crate::object::{BuiltinFn, DictData, DictKey, Object, PyModule};

pub fn build(_cache: &ModuleCache) -> Rc<PyModule> {
    let dict = Rc::new(RefCell::new(DictData::new()));
    {
        let mut d = dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("__name__")),
            Object::from_static("_datetime"),
        );
        d.insert(
            DictKey(Object::from_static("__doc__")),
            Object::from_static("Internal helper for the `datetime` module."),
        );
        d.insert(DictKey(Object::from_static("MINYEAR")), Object::Int(1));
        d.insert(DictKey(Object::from_static("MAXYEAR")), Object::Int(9999));
        d.insert(
            DictKey(Object::from_static("now_components")),
            b("now_components", now_components),
        );
        d.insert(
            DictKey(Object::from_static("utc_components")),
            b("utc_components", utc_components),
        );
        d.insert(
            DictKey(Object::from_static("monotonic_ns")),
            b("monotonic_ns", monotonic_ns),
        );
        d.insert(
            DictKey(Object::from_static("from_timestamp")),
            b("from_timestamp", from_timestamp),
        );
        d.insert(
            DictKey(Object::from_static("epoch_from_components")),
            b("epoch_from_components", epoch_from_components),
        );
        d.insert(
            DictKey(Object::from_static("days_in_month")),
            b("days_in_month", days_in_month_py),
        );
        d.insert(
            DictKey(Object::from_static("is_leap_year")),
            b("is_leap_year", is_leap_year_py),
        );
        d.insert(
            DictKey(Object::from_static("days_to_ordinal")),
            b("days_to_ordinal", days_to_ordinal_py),
        );
        d.insert(
            DictKey(Object::from_static("ordinal_to_components")),
            b("ordinal_to_components", ordinal_to_components_py),
        );
        d.insert(
            DictKey(Object::from_static("weekday")),
            b("weekday", weekday_py),
        );
        d.insert(
            DictKey(Object::from_static("iso_calendar")),
            b("iso_calendar", iso_calendar_py),
        );
        d.insert(
            DictKey(Object::from_static("local_utc_offset")),
            b("local_utc_offset", local_utc_offset),
        );
    }
    Rc::new(PyModule {
        name: "_datetime".to_owned(),
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

// ---------- public callables ----------

fn now_components(_args: &[Object]) -> Result<Object, RuntimeError> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| value_error("system time before epoch"))?;
    let secs = now.as_secs() as i64;
    let micros = i64::from(now.subsec_micros());
    // Convert UTC components first, then add local offset.
    let offset_seconds = local_offset_secs(secs);
    let local = secs + offset_seconds;
    let (year, month, day, hour, minute, second) = utc_to_components(local);
    Ok(Object::new_tuple(vec![
        Object::Int(year),
        Object::Int(month),
        Object::Int(day),
        Object::Int(hour),
        Object::Int(minute),
        Object::Int(second),
        Object::Int(micros),
        Object::Int(offset_seconds),
    ]))
}

fn utc_components(_args: &[Object]) -> Result<Object, RuntimeError> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| value_error("system time before epoch"))?;
    let secs = now.as_secs() as i64;
    let micros = i64::from(now.subsec_micros());
    let (year, month, day, hour, minute, second) = utc_to_components(secs);
    Ok(Object::new_tuple(vec![
        Object::Int(year),
        Object::Int(month),
        Object::Int(day),
        Object::Int(hour),
        Object::Int(minute),
        Object::Int(second),
        Object::Int(micros),
        Object::Int(0),
    ]))
}

fn monotonic_ns(_args: &[Object]) -> Result<Object, RuntimeError> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| value_error("system time before epoch"))?;
    Ok(Object::Int(now.as_nanos() as i64))
}

fn from_timestamp(args: &[Object]) -> Result<Object, RuntimeError> {
    let ts = match args.first() {
        Some(Object::Float(f)) => *f,
        Some(Object::Int(i)) => *i as f64,
        _ => return Err(type_error("timestamp must be a number")),
    };
    let utc = matches!(args.get(1), Some(Object::Bool(true)));
    let secs = ts.floor() as i64;
    let micros = ((ts.fract().abs()) * 1_000_000.0) as i64;
    let offset = if utc { 0 } else { local_offset_secs(secs) };
    let local_secs = secs + offset;
    let (year, month, day, hour, minute, second) = utc_to_components(local_secs);
    Ok(Object::new_tuple(vec![
        Object::Int(year),
        Object::Int(month),
        Object::Int(day),
        Object::Int(hour),
        Object::Int(minute),
        Object::Int(second),
        Object::Int(micros),
        Object::Int(offset),
    ]))
}

fn epoch_from_components(args: &[Object]) -> Result<Object, RuntimeError> {
    let y = arg_int(args, 0)?;
    let mo = arg_int(args, 1)?;
    let dd = arg_int(args, 2)?;
    let hh = arg_int_or(args, 3, 0)?;
    let mm = arg_int_or(args, 4, 0)?;
    let ss = arg_int_or(args, 5, 0)?;
    let us = arg_int_or(args, 6, 0)?;
    let off = arg_int_or(args, 7, 0)?;
    let utc_secs = components_to_utc(y, mo, dd, hh, mm, ss) - off;
    let total = utc_secs as f64 + (us as f64) / 1_000_000.0;
    Ok(Object::Float(total))
}

fn days_in_month_py(args: &[Object]) -> Result<Object, RuntimeError> {
    let y = arg_int(args, 0)?;
    let m = arg_int(args, 1)?;
    Ok(Object::Int(i64::from(days_in_month(y, m))))
}

fn is_leap_year_py(args: &[Object]) -> Result<Object, RuntimeError> {
    let y = arg_int(args, 0)?;
    Ok(Object::Bool(is_leap_year(y)))
}

fn days_to_ordinal_py(args: &[Object]) -> Result<Object, RuntimeError> {
    let y = arg_int(args, 0)?;
    let mo = arg_int(args, 1)?;
    let dd = arg_int(args, 2)?;
    Ok(Object::Int(ymd_to_ordinal(y, mo, dd)))
}

fn ordinal_to_components_py(args: &[Object]) -> Result<Object, RuntimeError> {
    let ordinal = arg_int(args, 0)?;
    let (y, m, d) = ordinal_to_ymd(ordinal);
    Ok(Object::new_tuple(vec![
        Object::Int(y),
        Object::Int(i64::from(m)),
        Object::Int(i64::from(d)),
    ]))
}

fn weekday_py(args: &[Object]) -> Result<Object, RuntimeError> {
    let y = arg_int(args, 0)?;
    let m = arg_int(args, 1)?;
    let d = arg_int(args, 2)?;
    let ordinal = ymd_to_ordinal(y, m, d);
    // Ordinal 1 (Jan 1, year 1) was a Monday (CPython convention).
    let wd = ((ordinal - 1).rem_euclid(7)) as i64;
    Ok(Object::Int(wd))
}

fn iso_calendar_py(args: &[Object]) -> Result<Object, RuntimeError> {
    let y = arg_int(args, 0)?;
    let m = arg_int(args, 1)?;
    let d = arg_int(args, 2)?;
    let (iy, iw, iwd) = iso_calendar(y, m, d);
    Ok(Object::new_tuple(vec![
        Object::Int(iy),
        Object::Int(iw),
        Object::Int(iwd),
    ]))
}

fn local_utc_offset(_args: &[Object]) -> Result<Object, RuntimeError> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| value_error("system time before epoch"))?;
    Ok(Object::Int(local_offset_secs(now.as_secs() as i64)))
}

fn arg_int(args: &[Object], idx: usize) -> Result<i64, RuntimeError> {
    match args.get(idx) {
        Some(Object::Int(i)) => Ok(*i),
        Some(Object::Bool(b)) => Ok(i64::from(*b)),
        _ => Err(type_error("expected int")),
    }
}

fn arg_int_or(args: &[Object], idx: usize, default: i64) -> Result<i64, RuntimeError> {
    match args.get(idx) {
        None | Some(Object::None) => Ok(default),
        Some(Object::Int(i)) => Ok(*i),
        Some(Object::Bool(b)) => Ok(i64::from(*b)),
        _ => Err(type_error("expected int")),
    }
}

// ---------- Gregorian calendar helpers ----------

/// `True` if the proleptic Gregorian year `y` is a leap year.
pub(crate) fn is_leap_year(y: i64) -> bool {
    (y % 4 == 0 && y % 100 != 0) || (y % 400 == 0)
}

pub(crate) fn days_in_month(y: i64, m: i64) -> u32 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if is_leap_year(y) {
                29
            } else {
                28
            }
        }
        _ => 0,
    }
}

const DAYS_BEFORE_MONTH_NONLEAP: [i64; 13] =
    [0, 0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334];
const DAYS_BEFORE_MONTH_LEAP: [i64; 13] =
    [0, 0, 31, 60, 91, 121, 152, 182, 213, 244, 274, 305, 335];

fn days_before_year(y: i64) -> i64 {
    let yy = y - 1;
    yy * 365 + yy / 4 - yy / 100 + yy / 400
}

/// Convert (year, month, day) to a Gregorian ordinal where Jan 1
/// year 1 = 1.
fn ymd_to_ordinal(y: i64, m: i64, d: i64) -> i64 {
    let before = if is_leap_year(y) {
        DAYS_BEFORE_MONTH_LEAP[m as usize]
    } else {
        DAYS_BEFORE_MONTH_NONLEAP[m as usize]
    };
    days_before_year(y) + before + d
}

/// Convert ordinal back to (year, month, day).
fn ordinal_to_ymd(n: i64) -> (i64, u32, u32) {
    let n0 = n - 1;
    let n400 = n0 / 146_097;
    let n0 = n0.rem_euclid(146_097);
    let n100 = n0 / 36_524;
    let n100 = if n100 == 4 { 3 } else { n100 };
    let n0 = n0 - n100 * 36_524;
    let n4 = n0 / 1_461;
    let n0 = n0.rem_euclid(1_461);
    let n1 = n0 / 365;
    let n1 = if n1 == 4 { 3 } else { n1 };
    let n0 = n0 - n1 * 365;
    let year = n400 * 400 + n100 * 100 + n4 * 4 + n1 + 1;
    let leap = n1 == 3 && (n4 != 24 || n100 == 3);
    let table = if leap {
        &DAYS_BEFORE_MONTH_LEAP
    } else {
        &DAYS_BEFORE_MONTH_NONLEAP
    };
    // n0 is 0-based day-of-year.
    let n0 = n0 + 1;
    let mut month = 1usize;
    while month < 12 && n0 > table[month + 1] {
        month += 1;
    }
    let day = n0 - table[month];
    (year, month as u32, day as u32)
}

fn iso_calendar(y: i64, m: i64, d: i64) -> (i64, i64, i64) {
    // CPython: ISO year, week, weekday (1=Mon..7=Sun).
    let ordinal = ymd_to_ordinal(y, m, d);
    let weekday = ((ordinal - 1).rem_euclid(7)) + 1;
    let week1_start = iso_week1_start(y);
    let mut iso_year = y;
    let mut delta = ordinal - week1_start;
    if delta < 0 {
        iso_year -= 1;
        let prev_week1 = iso_week1_start(iso_year);
        delta = ordinal - prev_week1;
    } else if delta >= 52 * 7 {
        let next_week1 = iso_week1_start(y + 1);
        if ordinal >= next_week1 {
            iso_year = y + 1;
            delta = ordinal - next_week1;
        }
    }
    let week = delta / 7 + 1;
    (iso_year, week, weekday)
}

fn iso_week1_start(y: i64) -> i64 {
    // Week 1 contains the first Thursday of the year. Start of
    // week 1 = Monday of that week.
    let jan1 = ymd_to_ordinal(y, 1, 1);
    let dow = ((jan1 - 1).rem_euclid(7)) + 1;
    let offset = if dow <= 4 { 1 - dow } else { 8 - dow };
    jan1 + offset
}

/// Unix seconds → UTC components.
fn utc_to_components(mut secs: i64) -> (i64, i64, i64, i64, i64, i64) {
    let mut day = secs.div_euclid(86_400);
    secs = secs.rem_euclid(86_400);
    let hour = secs / 3600;
    secs -= hour * 3600;
    let minute = secs / 60;
    let second = secs - minute * 60;
    // Unix epoch = ordinal 719_163 (1970-01-01).
    day += 719_163;
    let (y, m, d) = ordinal_to_ymd(day);
    (y, i64::from(m), i64::from(d), hour, minute, second)
}

fn components_to_utc(y: i64, m: i64, d: i64, hh: i64, mm: i64, ss: i64) -> i64 {
    let ordinal = ymd_to_ordinal(y, m, d);
    let day_unix = ordinal - 719_163;
    day_unix * 86_400 + hh * 3600 + mm * 60 + ss
}

/// Get the local UTC offset in seconds for a given Unix timestamp.
fn local_offset_secs(unix_secs: i64) -> i64 {
    use chrono::{Local, TimeZone, Utc};
    let utc = match Utc.timestamp_opt(unix_secs, 0).single() {
        Some(t) => t,
        None => return 0,
    };
    let local = utc.with_timezone(&Local);
    i64::from(local.offset().local_minus_utc())
}

//! The `time` built-in module.
//!
//! Surface area matches the CPython subset that everyday Python code
//! actually reaches for: `time()`, `monotonic()`, `perf_counter()`,
//! `sleep()`, `strftime`, `localtime`, `gmtime`, `time_ns()`.
//!
//! Calendar formatting is delegated to the `chrono` crate.

use std::cell::RefCell;
use std::rc::Rc;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use chrono::{DateTime, Datelike, Local, TimeZone, Timelike, Utc};

use crate::error::{type_error, RuntimeError};
use crate::import::ModuleCache;
use crate::object::{BuiltinFn, DictData, DictKey, Object, PyModule};

thread_local! {
    static EPOCH: Instant = Instant::now();
}

pub fn build(_cache: &ModuleCache) -> Rc<PyModule> {
    let dict = Rc::new(RefCell::new(DictData::new()));
    {
        let mut d = dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("__name__")),
            Object::from_static("time"),
        );
        d.insert(
            DictKey(Object::from_static("__doc__")),
            Object::from_static("Time access and conversions."),
        );
        d.insert(DictKey(Object::from_static("time")), b("time", time_time));
        d.insert(
            DictKey(Object::from_static("time_ns")),
            b("time_ns", time_ns),
        );
        d.insert(
            DictKey(Object::from_static("monotonic")),
            b("monotonic", time_monotonic),
        );
        d.insert(
            DictKey(Object::from_static("perf_counter")),
            b("perf_counter", time_monotonic),
        );
        d.insert(
            DictKey(Object::from_static("sleep")),
            b("sleep", time_sleep),
        );
        d.insert(
            DictKey(Object::from_static("strftime")),
            b("strftime", time_strftime),
        );
        d.insert(
            DictKey(Object::from_static("localtime")),
            b("localtime", time_localtime),
        );
        d.insert(
            DictKey(Object::from_static("gmtime")),
            b("gmtime", time_gmtime),
        );
        d.insert(
            DictKey(Object::from_static("mktime")),
            b("mktime", time_mktime),
        );
    }
    Rc::new(PyModule {
        name: "time".to_owned(),
        filename: None,
        dict,
    })
}

fn b(name: &'static str, body: fn(&[Object]) -> Result<Object, RuntimeError>) -> Object {
    Object::Builtin(Rc::new(BuiltinFn {
        name,
        call: Box::new(body),
    }))
}

fn time_time(_args: &[Object]) -> Result<Object, RuntimeError> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    Ok(Object::Float(now.as_secs_f64()))
}

fn time_ns(_args: &[Object]) -> Result<Object, RuntimeError> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    Ok(Object::Int(now.as_nanos() as i64))
}

fn time_monotonic(_args: &[Object]) -> Result<Object, RuntimeError> {
    let elapsed = EPOCH.with(|e| e.elapsed());
    Ok(Object::Float(elapsed.as_secs_f64()))
}

fn time_sleep(args: &[Object]) -> Result<Object, RuntimeError> {
    let secs = match args.first() {
        Some(Object::Int(i)) => *i as f64,
        Some(Object::Float(f)) => *f,
        _ => return Err(type_error("sleep expects a number")),
    };
    if secs > 0.0 {
        thread::sleep(Duration::from_secs_f64(secs));
    }
    Ok(Object::None)
}

fn tuple_to_dt(args: Option<&Object>) -> Result<DateTime<Local>, RuntimeError> {
    let tup = match args {
        Some(Object::Tuple(t)) => t,
        _ => return Err(type_error("expected struct_time tuple")),
    };
    let extract = |i: usize| -> Result<i32, RuntimeError> {
        match tup.get(i) {
            Some(Object::Int(v)) => Ok(*v as i32),
            _ => Err(type_error("invalid struct_time")),
        }
    };
    let dt = Local
        .with_ymd_and_hms(
            extract(0)?,
            extract(1)? as u32,
            extract(2)? as u32,
            extract(3)? as u32,
            extract(4)? as u32,
            extract(5)? as u32,
        )
        .single()
        .ok_or_else(|| type_error("invalid local time"))?;
    Ok(dt)
}

fn time_strftime(args: &[Object]) -> Result<Object, RuntimeError> {
    let fmt = match args.first() {
        Some(Object::Str(s)) => s.to_string(),
        _ => return Err(type_error("strftime expects format string")),
    };
    let dt = if args.len() >= 2 {
        tuple_to_dt(args.get(1))?
    } else {
        Local::now()
    };
    Ok(Object::from_str(dt.format(&fmt).to_string()))
}

fn struct_time_from_local(dt: DateTime<Local>) -> Object {
    Object::new_tuple(vec![
        Object::Int(i64::from(dt.year())),
        Object::Int(i64::from(dt.month())),
        Object::Int(i64::from(dt.day())),
        Object::Int(i64::from(dt.hour())),
        Object::Int(i64::from(dt.minute())),
        Object::Int(i64::from(dt.second())),
        Object::Int(i64::from(dt.weekday().num_days_from_monday())),
        Object::Int(i64::from(dt.ordinal())),
        Object::Int(-1),
    ])
}

fn struct_time_from_utc(dt: DateTime<Utc>) -> Object {
    Object::new_tuple(vec![
        Object::Int(i64::from(dt.year())),
        Object::Int(i64::from(dt.month())),
        Object::Int(i64::from(dt.day())),
        Object::Int(i64::from(dt.hour())),
        Object::Int(i64::from(dt.minute())),
        Object::Int(i64::from(dt.second())),
        Object::Int(i64::from(dt.weekday().num_days_from_monday())),
        Object::Int(i64::from(dt.ordinal())),
        Object::Int(0),
    ])
}

fn time_localtime(args: &[Object]) -> Result<Object, RuntimeError> {
    let dt = match args.first() {
        Some(Object::Int(i)) => {
            let secs = *i;
            Local
                .timestamp_opt(secs, 0)
                .single()
                .ok_or_else(|| type_error("invalid timestamp"))?
        }
        Some(Object::Float(f)) => {
            let secs = *f as i64;
            Local
                .timestamp_opt(secs, 0)
                .single()
                .ok_or_else(|| type_error("invalid timestamp"))?
        }
        None | Some(Object::None) => Local::now(),
        _ => return Err(type_error("localtime expects a number")),
    };
    Ok(struct_time_from_local(dt))
}

fn time_gmtime(args: &[Object]) -> Result<Object, RuntimeError> {
    let dt = match args.first() {
        Some(Object::Int(i)) => Utc
            .timestamp_opt(*i, 0)
            .single()
            .ok_or_else(|| type_error("invalid timestamp"))?,
        Some(Object::Float(f)) => Utc
            .timestamp_opt(*f as i64, 0)
            .single()
            .ok_or_else(|| type_error("invalid timestamp"))?,
        None | Some(Object::None) => Utc::now(),
        _ => return Err(type_error("gmtime expects a number")),
    };
    Ok(struct_time_from_utc(dt))
}

fn time_mktime(args: &[Object]) -> Result<Object, RuntimeError> {
    let dt = tuple_to_dt(args.first())?;
    Ok(Object::Float(dt.timestamp() as f64))
}

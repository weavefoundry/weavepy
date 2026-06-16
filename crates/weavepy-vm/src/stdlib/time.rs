//! The `time` built-in module.
//!
//! Surface area matches the CPython subset that everyday Python code
//! actually reaches for: `time()`, `monotonic()`, `perf_counter()`,
//! `sleep()`, `strftime`, `localtime`, `gmtime`, `time_ns()`.
//!
//! Calendar formatting is delegated to the `chrono` crate.

use crate::sync::Rc;
use crate::sync::RefCell;
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
            DictKey(Object::from_static("get_clock_info")),
            b("get_clock_info", time_get_clock_info),
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
        d.insert(
            DictKey(Object::from_static("struct_time")),
            Object::Type(struct_time_type()),
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
        binds_instance: false,
        call: Box::new(body),
        call_kw: None,
    }))
}

/// CPython's `time.struct_time` visible fields (index order). The hidden
/// `tm_zone`/`tm_gmtoff` extras are set by name when available.
const STRUCT_TIME_FIELDS: [&str; 9] = [
    "tm_year", "tm_mon", "tm_mday", "tm_hour", "tm_min", "tm_sec", "tm_wday", "tm_yday", "tm_isdst",
];

/// `time.struct_time` — a CPython struct sequence (named `tm_*` attributes *and*
/// 9-element tuple indexing). Returned by `localtime`/`gmtime`; `zipfile`,
/// `tarfile`, `email`, `http.cookiejar`, … read `.tm_year` etc. off it, so a
/// bare tuple (the old shape) broke them with `'tuple' object has no attribute
/// 'tm_year'`.
fn struct_time_type() -> Rc<crate::types::TypeObject> {
    crate::stdlib::os::struct_seq_type("struct_time", &STRUCT_TIME_FIELDS)
}

fn make_struct_time(values: Vec<Object>) -> Object {
    crate::stdlib::os::struct_seq_instance(struct_time_type(), &STRUCT_TIME_FIELDS, values)
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

/// `time.get_clock_info(name)` — a namespace with `implementation`,
/// `monotonic`, `adjustable`, and `resolution`. asyncio reads
/// `get_clock_info('monotonic').resolution` when building a loop.
fn time_get_clock_info(args: &[Object]) -> Result<Object, RuntimeError> {
    let name = match args.first() {
        Some(Object::Str(s)) => s.to_string(),
        _ => return Err(type_error("get_clock_info() argument must be a str")),
    };
    let (implementation, monotonic, adjustable) = match name.as_str() {
        "monotonic" | "perf_counter" => ("mach_absolute_time()", true, false),
        "time" => ("clock_gettime(CLOCK_REALTIME)", false, true),
        "process_time" => ("clock_gettime(CLOCK_PROCESS_CPUTIME_ID)", true, false),
        "thread_time" => ("clock_gettime(CLOCK_THREAD_CPUTIME_ID)", true, false),
        other => {
            return Err(crate::error::value_error(format!("unknown clock: {other}")))
        }
    };
    thread_local! {
        static CLOCK_INFO_TYPE: RefCell<Option<Rc<crate::types::TypeObject>>> =
            const { RefCell::new(None) };
    }
    let cls = CLOCK_INFO_TYPE.with(|slot| {
        if let Some(c) = slot.borrow().as_ref() {
            return c.clone();
        }
        let bt = crate::builtin_types::builtin_types();
        let cls = crate::types::TypeObject::new_user(
            "clock_info",
            vec![bt.object_.clone()],
            DictData::new(),
        )
        .expect("clock_info class must linearise");
        *slot.borrow_mut() = Some(cls.clone());
        cls
    });
    let inst = Rc::new(crate::types::PyInstance::new(cls));
    {
        let mut d = inst.dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("implementation")),
            Object::from_static(implementation),
        );
        d.insert(
            DictKey(Object::from_static("monotonic")),
            Object::Bool(monotonic),
        );
        d.insert(
            DictKey(Object::from_static("adjustable")),
            Object::Bool(adjustable),
        );
        // 1 ns — the resolution of the underlying nanosecond clocks.
        d.insert(
            DictKey(Object::from_static("resolution")),
            Object::Float(1e-9),
        );
    }
    Ok(Object::Instance(inst))
}

fn time_monotonic(_args: &[Object]) -> Result<Object, RuntimeError> {
    let elapsed = EPOCH.with(|e| e.elapsed());
    Ok(Object::Float(elapsed.as_secs_f64()))
}

fn time_sleep(args: &[Object]) -> Result<Object, RuntimeError> {
    let secs = match args.first() {
        Some(Object::Int(i)) => *i as f64,
        Some(Object::Float(f)) => *f,
        Some(Object::Bool(b)) => f64::from(*b),
        _ => return Err(type_error("sleep expects a number")),
    };
    if secs.is_nan() || secs < 0.0 {
        // CPython raises ValueError for a negative sleep.
        return Err(crate::error::value_error(
            "sleep length must be non-negative",
        ));
    }
    if secs > 0.0 {
        // CPython's `time.sleep` releases the GIL for the duration of
        // the sleep so other threads run (RFC 0039). Holding it would
        // serialize the whole interpreter behind one sleeping thread —
        // e.g. a `threading.Barrier` peer that `time.sleep`s would stall
        // every other peer's timed `wait()`.
        crate::gil::allow_threads_then(|| {
            thread::sleep(Duration::from_secs_f64(secs));
        });
    }
    Ok(Object::None)
}

fn tuple_to_dt(args: Option<&Object>) -> Result<DateTime<Local>, RuntimeError> {
    // Accept both a bare 9-tuple/list and a real `struct_time` instance (which
    // stores the calendar fields under their `tm_*` names but is no longer a
    // `Tuple`). For the instance, read the visible fields positionally.
    let get = |i: usize| -> Option<Object> {
        match args {
            Some(Object::Tuple(t)) => t.get(i).cloned(),
            Some(Object::List(items)) => items.borrow().get(i).cloned(),
            Some(Object::Instance(inst)) => inst
                .dict
                .borrow()
                .get(&DictKey(Object::from_static(STRUCT_TIME_FIELDS[i])))
                .cloned(),
            _ => None,
        }
    };
    if !matches!(
        args,
        Some(Object::Tuple(_) | Object::List(_) | Object::Instance(_))
    ) {
        return Err(type_error("expected struct_time tuple"));
    }
    let extract = |i: usize| -> Result<i32, RuntimeError> {
        match get(i) {
            Some(Object::Int(v)) => Ok(v as i32),
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
    make_struct_time(vec![
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
    make_struct_time(vec![
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

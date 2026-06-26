//! The `time` built-in module.
//!
//! Surface area matches the CPython subset that everyday Python code
//! actually reaches for: `time()`, `monotonic()`, `perf_counter()`,
//! `sleep()`, `strftime`, `localtime`, `gmtime`, `time_ns()`.
//!
//! Calendar formatting is delegated to the `chrono` crate.

use crate::sync::Rc;
use crate::sync::RefCell;
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
            DictKey(Object::from_static("ctime")),
            b("ctime", time_ctime),
        );
        d.insert(
            DictKey(Object::from_static("asctime")),
            b("asctime", time_asctime),
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
            DictKey(Object::from_static("strptime")),
            b("strptime", time_strptime),
        );
        d.insert(
            DictKey(Object::from_static("struct_time")),
            Object::Type(struct_time_type()),
        );
        // Module-level timezone constants, computed from the local zone the
        // way CPython's `init_timezone` derives them from the C library after
        // `tzset()`. `_strptime` reads all four, and `email`/`http.cookiejar`
        // read `time.timezone`/`time.tzname`.
        let (timezone, altzone, daylight, std_name, dst_name) = compute_timezone();
        d.insert(
            DictKey(Object::from_static("timezone")),
            Object::Int(timezone),
        );
        d.insert(
            DictKey(Object::from_static("altzone")),
            Object::Int(altzone),
        );
        d.insert(
            DictKey(Object::from_static("daylight")),
            Object::Int(daylight),
        );
        d.insert(
            DictKey(Object::from_static("tzname")),
            Object::new_tuple(vec![Object::from_str(std_name), Object::from_str(dst_name)]),
        );
        // `_strptime._strptime_time` slices its result to this many items
        // before building a `struct_time`. Our `struct_time` exposes the 9
        // visible `tm_*` fields (the hidden `tm_zone`/`tm_gmtoff` are set by
        // name, not positionally), so 9 is the faithful count.
        d.insert(
            DictKey(Object::from_static("_STRUCT_TM_ITEMS")),
            Object::Int(9),
        );
    }
    Rc::new(PyModule {
        name: "time".to_owned(),
        filename: None,
        dict,
    })
}

/// Derive `(timezone, altzone, daylight, tzname[0], tzname[1])` from the
/// host's local zone, matching CPython's `init_timezone`:
/// `timezone`/`altzone` are seconds **west** of UTC for standard/DST time,
/// `daylight` is nonzero when the zone observes DST, and `tzname` is the
/// `(std, dst)` abbreviation pair. We sample January and July to find the
/// standard (smaller east offset) and DST (larger) sides.
fn compute_timezone() -> (i64, i64, i64, String, String) {
    use chrono::{Datelike, Offset};
    let year = Local::now().year();
    let sample = |month: u32| -> Option<(i64, String)> {
        let dt = Local.with_ymd_and_hms(year, month, 1, 12, 0, 0).single()?;
        let east = i64::from(dt.offset().fix().local_minus_utc());
        Some((east, dt.format("%Z").to_string()))
    };
    let Some((jan_east, jan_name)) = sample(1) else {
        return (0, 0, 0, "UTC".to_owned(), "UTC".to_owned());
    };
    let (jul_east, jul_name) = sample(7).unwrap_or((jan_east, jan_name.clone()));
    // Standard time is the side with the *smaller* east offset (clocks not
    // moved forward); DST is the larger.
    let (std_east, std_name, dst_east, dst_name) = if jan_east <= jul_east {
        (jan_east, jan_name, jul_east, jul_name)
    } else {
        (jul_east, jul_name, jan_east, jan_name)
    };
    let daylight = i64::from(jan_east != jul_east);
    (-std_east, -dst_east, daylight, std_name, dst_name)
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
    crate::stdlib::os::struct_seq_type("struct_time", "time", &STRUCT_TIME_FIELDS)
}

fn make_struct_time(values: Vec<Object>) -> Object {
    crate::stdlib::os::struct_seq_instance(struct_time_type(), &STRUCT_TIME_FIELDS, values)
}

/// `time.strptime(string[, format])` — parse a time string to a
/// `struct_time`. CPython's `timemodule.c` delegates to the pure-Python
/// `_strptime._strptime_time`; we do the same so the full locale-aware
/// directive set (`%a %b %Y %H:%M:%S …`) and error messages match.
fn time_strptime(args: &[Object]) -> Result<Object, RuntimeError> {
    if args.is_empty() || args.len() > 2 {
        return Err(type_error(format!(
            "strptime() takes 1 or 2 arguments ({} given)",
            args.len()
        )));
    }
    let ptr = crate::vm_singletons::current_interpreter_ptr().ok_or_else(|| {
        crate::error::runtime_error("time.strptime requires a running interpreter")
    })?;
    // SAFETY: the per-thread interpreter pointer is published by the
    // bytecode dispatch loop, the same bridge the `_thread`/C-API
    // callbacks use; we re-enter synchronously to import + call `_strptime`.
    let interp = unsafe { &mut *ptr };
    let module = interp.import_path("_strptime")?;
    let Object::Module(m) = &module else {
        return Err(crate::error::runtime_error("_strptime is not a module"));
    };
    let func = m
        .dict
        .borrow()
        .get(&DictKey(Object::from_static("_strptime_time")))
        .cloned()
        .ok_or_else(|| crate::error::runtime_error("_strptime._strptime_time missing"))?;
    interp.call_object(func, args, &[])
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
        other => return Err(crate::error::value_error(format!("unknown clock: {other}"))),
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
        //
        // It is also a signal-delivery point: a SIGINT (or any handled
        // signal) arriving mid-sleep must break the wait and run the Python
        // handler, so `time.sleep(30)` raises `KeyboardInterrupt` promptly
        // (test_subprocess.test_send_signal). On POSIX we loop over
        // `nanosleep`, which returns `EINTR` with the unslept remainder when
        // a signal interrupts it; we re-acquire the GIL, service pending
        // handlers (which may raise), then resume for the remainder.
        #[cfg(unix)]
        {
            let mut remaining = Duration::from_secs_f64(secs);
            loop {
                let leftover = crate::gil::allow_threads_then(|| {
                    let req = libc::timespec {
                        tv_sec: remaining.as_secs() as libc::time_t,
                        tv_nsec: libc::c_long::from(remaining.subsec_nanos() as i32),
                    };
                    let mut rem = libc::timespec {
                        tv_sec: 0,
                        tv_nsec: 0,
                    };
                    let rc = unsafe { libc::nanosleep(&raw const req, &raw mut rem) };
                    if rc == 0 {
                        None
                    } else if std::io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
                        Some(Duration::new(
                            rem.tv_sec.max(0) as u64,
                            rem.tv_nsec.clamp(0, 999_999_999) as u32,
                        ))
                    } else {
                        // Any other error: stop sleeping (CPython would raise,
                        // but nanosleep only fails with EINTR/EINVAL here).
                        Some(Duration::ZERO)
                    }
                });
                match leftover {
                    None => break,
                    Some(rem) => {
                        // GIL re-acquired: run any handler the signal tripped.
                        if crate::stdlib::signal_mod::signals_pending() {
                            if let Some(ptr) = crate::vm_singletons::current_interpreter_ptr() {
                                unsafe { (*ptr).run_pending_signals_public()? };
                            }
                        }
                        if rem.is_zero() {
                            break;
                        }
                        remaining = rem;
                    }
                }
            }
        }
        #[cfg(not(unix))]
        {
            crate::gil::allow_threads_then(|| {
                std::thread::sleep(Duration::from_secs_f64(secs));
            });
        }
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
    // A format string may carry lone surrogates: `_pydatetime._wrap_strftime`
    // splices the object's `%Z`/`%z`/`%f` values in *before* calling us, so a
    // surrogate tzname (`datetimetester.test_zones`) or a surrogate literal
    // (`t.strftime('%y\ud800%m')`) arrives as an `Object::WStr`. Bridge the
    // code points into the PUA window so `chrono`'s UTF-8 formatter copies
    // them through as opaque literals, then map them back — exactly how the
    // `%`/`str.format` engines preserve surrogates.
    let cps = match args.first() {
        Some(o @ (Object::Str(_) | Object::WStr(_))) => o.str_codepoints().unwrap_or_default(),
        Some(other) => {
            return Err(type_error(format!(
                "strftime() argument 1 must be str, not {}",
                other.type_name()
            )))
        }
        None => return Err(type_error("strftime expects format string")),
    };
    let fmt = crate::builtins::bridge_encode_cps(&cps);
    let dt = if args.len() >= 2 {
        tuple_to_dt(args.get(1))?
    } else {
        Local::now()
    };
    // `chrono`'s `DelayedFormat` reports an unsupported/invalid directive (e.g.
    // the glibc extension `%4Y`) by returning `Err` from its `Display` impl;
    // calling `.to_string()` on that panics. Render through `write!` so we can
    // surface a Python-level `ValueError` instead of aborting the interpreter
    // (CPython's `time.strftime` likewise raises on a bad format string).
    use std::fmt::Write as _;
    let mut rendered = String::new();
    match write!(rendered, "{}", dt.format(&fmt)) {
        Ok(()) => Ok(crate::builtins::bridge_to_object(&rendered)),
        Err(_) => Err(crate::error::value_error("Invalid format string")),
    }
}

/// `time.asctime([t])` / `time.ctime([secs])` shared formatter — CPython's
/// `asctime`/`ctime` both render `"%a %b %e %H:%M:%S %Y"` (the libc
/// `asctime` layout: day-of-month *space*-padded to width 2), which is what
/// `_pydatetime.date.ctime()` reproduces with its `"%s %s %2d …"` format.
fn format_ctime_local(dt: DateTime<Local>) -> Object {
    Object::from_str(dt.format("%a %b %e %H:%M:%S %Y").to_string())
}

fn time_asctime(args: &[Object]) -> Result<Object, RuntimeError> {
    let dt = if args.first().is_some_and(|o| !matches!(o, Object::None)) {
        tuple_to_dt(args.first())?
    } else {
        Local::now()
    };
    Ok(format_ctime_local(dt))
}

fn time_ctime(args: &[Object]) -> Result<Object, RuntimeError> {
    let dt = match args.first() {
        None | Some(Object::None) => Local::now(),
        Some(Object::Int(i)) => local_from_timestamp(*i)?,
        Some(Object::Float(f)) => local_from_timestamp(float_to_timestamp(*f)?)?,
        Some(other) => {
            return Err(type_error(format!(
                "ctime() argument must be a number, not '{}'",
                other.type_name()
            )))
        }
    };
    Ok(format_ctime_local(dt))
}

/// Convert a float timestamp to whole seconds, raising CPython's
/// `OverflowError` for the non-finite / out-of-`time_t`-range values that
/// `datetimetester.test_insane_fromtimestamp` feeds in (`±1e200`).
fn float_to_timestamp(f: f64) -> Result<i64, RuntimeError> {
    if !f.is_finite() || f < i64::MIN as f64 || f >= i64::MAX as f64 {
        return Err(crate::error::overflow_error(
            "timestamp out of range for platform time_t",
        ));
    }
    Ok(f as i64)
}

fn local_from_timestamp(secs: i64) -> Result<DateTime<Local>, RuntimeError> {
    Local
        .timestamp_opt(secs, 0)
        .single()
        .ok_or_else(|| crate::error::overflow_error("timestamp out of range for platform time_t"))
}

fn utc_from_timestamp(secs: i64) -> Result<DateTime<Utc>, RuntimeError> {
    Utc.timestamp_opt(secs, 0)
        .single()
        .ok_or_else(|| crate::error::overflow_error("timestamp out of range for platform time_t"))
}

/// Attach the two hidden `struct_time` extras (`tm_gmtoff`, `tm_zone`) by
/// name. CPython's `struct_time` carries them as named-but-unindexed members;
/// `_pydatetime._local_timezone` reads `localtm.tm_gmtoff`/`.tm_zone` straight
/// off the `localtime()` result (`test_subclass_alternate_constructors_*`).
fn with_tz_extras(obj: Object, gmtoff: i64, zone: &str) -> Object {
    if let Object::Instance(inst) = &obj {
        let mut d = inst.dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("tm_gmtoff")),
            Object::Int(gmtoff),
        );
        d.insert(
            DictKey(Object::from_static("tm_zone")),
            Object::from_str(zone.to_owned()),
        );
    }
    obj
}

fn struct_time_from_local(dt: DateTime<Local>) -> Object {
    use chrono::Offset;
    let gmtoff = i64::from(dt.offset().fix().local_minus_utc());
    let zone = dt.format("%Z").to_string();
    let base = make_struct_time(vec![
        Object::Int(i64::from(dt.year())),
        Object::Int(i64::from(dt.month())),
        Object::Int(i64::from(dt.day())),
        Object::Int(i64::from(dt.hour())),
        Object::Int(i64::from(dt.minute())),
        Object::Int(i64::from(dt.second())),
        Object::Int(i64::from(dt.weekday().num_days_from_monday())),
        Object::Int(i64::from(dt.ordinal())),
        Object::Int(-1),
    ]);
    with_tz_extras(base, gmtoff, &zone)
}

fn struct_time_from_utc(dt: DateTime<Utc>) -> Object {
    let base = make_struct_time(vec![
        Object::Int(i64::from(dt.year())),
        Object::Int(i64::from(dt.month())),
        Object::Int(i64::from(dt.day())),
        Object::Int(i64::from(dt.hour())),
        Object::Int(i64::from(dt.minute())),
        Object::Int(i64::from(dt.second())),
        Object::Int(i64::from(dt.weekday().num_days_from_monday())),
        Object::Int(i64::from(dt.ordinal())),
        Object::Int(0),
    ]);
    with_tz_extras(base, 0, "UTC")
}

fn time_localtime(args: &[Object]) -> Result<Object, RuntimeError> {
    // An out-of-range or non-finite seconds value is an `OverflowError`, not a
    // `TypeError` — `datetime.fromtimestamp(1e200)` relies on this
    // (`datetimetester.test_insane_fromtimestamp`).
    let dt = match args.first() {
        Some(Object::Int(i)) => local_from_timestamp(*i)?,
        Some(Object::Float(f)) => local_from_timestamp(float_to_timestamp(*f)?)?,
        None | Some(Object::None) => Local::now(),
        _ => return Err(type_error("localtime expects a number")),
    };
    Ok(struct_time_from_local(dt))
}

fn time_gmtime(args: &[Object]) -> Result<Object, RuntimeError> {
    let dt = match args.first() {
        Some(Object::Int(i)) => utc_from_timestamp(*i)?,
        Some(Object::Float(f)) => utc_from_timestamp(float_to_timestamp(*f)?)?,
        None | Some(Object::None) => Utc::now(),
        _ => return Err(type_error("gmtime expects a number")),
    };
    Ok(struct_time_from_utc(dt))
}

fn time_mktime(args: &[Object]) -> Result<Object, RuntimeError> {
    let dt = tuple_to_dt(args.first())?;
    Ok(Object::Float(dt.timestamp() as f64))
}

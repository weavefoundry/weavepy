//! Datetime C-API surface (RFC 0029).
//!
//! CPython exposes the datetime constructors and type checks
//! through a capsule registered as `datetime.datetime_CAPI`.
//! Extension modules read the capsule once at init time, store
//! the `PyDateTime_CAPI` struct pointer, and use it as a
//! vtable. We mirror the layout exactly so user-written C code
//! (compiled against CPython's `datetime.h`) sees the same
//! shape.
//!
//! ## Layout
//!
//! The `PyDateTime_CAPI` struct begins with eight type slots,
//! followed by twelve function pointers, and a recent CPython
//! addition for the timezone module. The order is part of the
//! ABI: shifting fields would silently break every numpy /
//! pandas / pendulum / arrow extension on the planet.
//!
//! ## Lifetime
//!
//! The struct is allocated `'static`; the capsule we publish
//! holds a raw pointer into the static. Extensions that import
//! the capsule keep the pointer for the life of the process,
//! which is fine because the struct is immutable.

use std::ffi::CString;
use std::os::raw::c_int;
use std::ptr;

use weavepy_vm::object::Object;

use crate::object::PyObject;
use crate::types::PyTypeObject;

/// Layout of `PyDateTime_CAPI` (from `Include/datetime.h`).
///
/// Field order matches CPython 3.13 exactly. Adding fields in
/// the middle would break binary compatibility — new entries
/// must be appended to the end (mirroring CPython's evolution).
#[repr(C)]
pub struct PyDateTimeCAPI {
    pub DateType: *mut PyTypeObject,
    pub DateTimeType: *mut PyTypeObject,
    pub TimeType: *mut PyTypeObject,
    pub DeltaType: *mut PyTypeObject,
    pub TZInfoType: *mut PyTypeObject,
    // Singleton: a `tzinfo` representing UTC. CPython publishes
    // this as the *only* easily-importable UTC. We synthesize a
    // sentinel object.
    pub TimeZone_UTC: *mut PyObject,

    // Constructors.
    pub Date_FromDate: unsafe extern "C" fn(
        year: c_int,
        month: c_int,
        day: c_int,
        cls: *mut PyTypeObject,
    ) -> *mut PyObject,
    pub DateTime_FromDateAndTime: unsafe extern "C" fn(
        year: c_int,
        month: c_int,
        day: c_int,
        hour: c_int,
        minute: c_int,
        second: c_int,
        usec: c_int,
        tzinfo: *mut PyObject,
        cls: *mut PyTypeObject,
    ) -> *mut PyObject,
    pub Time_FromTime: unsafe extern "C" fn(
        hour: c_int,
        minute: c_int,
        second: c_int,
        usec: c_int,
        tzinfo: *mut PyObject,
        cls: *mut PyTypeObject,
    ) -> *mut PyObject,
    pub Delta_FromDelta: unsafe extern "C" fn(
        days: c_int,
        seconds: c_int,
        microseconds: c_int,
        normalize: c_int,
        cls: *mut PyTypeObject,
    ) -> *mut PyObject,
    pub TimeZone_FromTimeZone:
        unsafe extern "C" fn(offset: *mut PyObject, name: *mut PyObject) -> *mut PyObject,

    // Convenience: from-timestamp constructors.
    pub DateTime_FromTimestamp: unsafe extern "C" fn(
        cls: *mut PyObject,
        args: *mut PyObject,
        kwargs: *mut PyObject,
    ) -> *mut PyObject,
    pub Date_FromTimestamp:
        unsafe extern "C" fn(cls: *mut PyObject, args: *mut PyObject) -> *mut PyObject,

    // 3.13 additions for full-precision constructors.
    pub DateTime_FromDateAndTimeAndFold: unsafe extern "C" fn(
        year: c_int,
        month: c_int,
        day: c_int,
        hour: c_int,
        minute: c_int,
        second: c_int,
        usec: c_int,
        tzinfo: *mut PyObject,
        fold: c_int,
        cls: *mut PyTypeObject,
    ) -> *mut PyObject,
    pub Time_FromTimeAndFold: unsafe extern "C" fn(
        hour: c_int,
        minute: c_int,
        second: c_int,
        usec: c_int,
        tzinfo: *mut PyObject,
        fold: c_int,
        cls: *mut PyTypeObject,
    ) -> *mut PyObject,
}

// SAFETY: every field is a raw pointer to a `'static` resource
// (a `PyTypeObject` static or a top-level extern "C" fn). The
// struct itself is immutable; no thread can observe a torn
// write.
unsafe impl Sync for PyDateTimeCAPI {}

// ---------------------------------------------------------------------
// Implementations.
// ---------------------------------------------------------------------

unsafe extern "C" fn date_from_date(
    year: c_int,
    month: c_int,
    day: c_int,
    _cls: *mut PyTypeObject,
) -> *mut PyObject {
    construct_date(year, month, day)
}

unsafe extern "C" fn datetime_from_date_and_time(
    year: c_int,
    month: c_int,
    day: c_int,
    hour: c_int,
    minute: c_int,
    second: c_int,
    usec: c_int,
    tzinfo: *mut PyObject,
    _cls: *mut PyTypeObject,
) -> *mut PyObject {
    construct_datetime(year, month, day, hour, minute, second, usec, tzinfo, 0)
}

unsafe extern "C" fn datetime_from_date_and_time_and_fold(
    year: c_int,
    month: c_int,
    day: c_int,
    hour: c_int,
    minute: c_int,
    second: c_int,
    usec: c_int,
    tzinfo: *mut PyObject,
    fold: c_int,
    _cls: *mut PyTypeObject,
) -> *mut PyObject {
    construct_datetime(year, month, day, hour, minute, second, usec, tzinfo, fold)
}

unsafe extern "C" fn time_from_time(
    hour: c_int,
    minute: c_int,
    second: c_int,
    usec: c_int,
    tzinfo: *mut PyObject,
    _cls: *mut PyTypeObject,
) -> *mut PyObject {
    construct_time(hour, minute, second, usec, tzinfo, 0)
}

unsafe extern "C" fn time_from_time_and_fold(
    hour: c_int,
    minute: c_int,
    second: c_int,
    usec: c_int,
    tzinfo: *mut PyObject,
    fold: c_int,
    _cls: *mut PyTypeObject,
) -> *mut PyObject {
    construct_time(hour, minute, second, usec, tzinfo, fold)
}

unsafe extern "C" fn delta_from_delta(
    days: c_int,
    seconds: c_int,
    microseconds: c_int,
    _normalize: c_int,
    _cls: *mut PyTypeObject,
) -> *mut PyObject {
    construct_timedelta(days, seconds, microseconds)
}

unsafe extern "C" fn timezone_from_timezone(
    offset: *mut PyObject,
    name: *mut PyObject,
) -> *mut PyObject {
    construct_timezone(offset, name)
}

unsafe extern "C" fn datetime_from_timestamp(
    _cls: *mut PyObject,
    args: *mut PyObject,
    _kwargs: *mut PyObject,
) -> *mut PyObject {
    // args is a (timestamp,) or (timestamp, tz). The result is
    // produced by calling the `datetime` module's
    // `datetime.fromtimestamp` Python builtin.
    match call_datetime_attr("datetime", "fromtimestamp", args) {
        Some(p) => p,
        None => ptr::null_mut(),
    }
}

unsafe extern "C" fn date_from_timestamp(
    _cls: *mut PyObject,
    args: *mut PyObject,
) -> *mut PyObject {
    match call_datetime_attr("date", "fromtimestamp", args) {
        Some(p) => p,
        None => ptr::null_mut(),
    }
}

fn construct_date(year: c_int, month: c_int, day: c_int) -> *mut PyObject {
    invoke_class(
        "date",
        vec![
            Object::Int(year as i64),
            Object::Int(month as i64),
            Object::Int(day as i64),
        ],
    )
}

fn construct_datetime(
    year: c_int,
    month: c_int,
    day: c_int,
    hour: c_int,
    minute: c_int,
    second: c_int,
    usec: c_int,
    tzinfo: *mut PyObject,
    fold: c_int,
) -> *mut PyObject {
    let mut args: Vec<Object> = vec![
        Object::Int(year as i64),
        Object::Int(month as i64),
        Object::Int(day as i64),
        Object::Int(hour as i64),
        Object::Int(minute as i64),
        Object::Int(second as i64),
        Object::Int(usec as i64),
    ];
    if !tzinfo.is_null() {
        args.push(unsafe { crate::object::clone_object(tzinfo) });
    }
    // `fold` is keyword-only in CPython; for the foundation we
    // ignore it.
    let _ = fold;
    invoke_class("datetime", args)
}

fn construct_time(
    hour: c_int,
    minute: c_int,
    second: c_int,
    usec: c_int,
    tzinfo: *mut PyObject,
    _fold: c_int,
) -> *mut PyObject {
    let mut args: Vec<Object> = vec![
        Object::Int(hour as i64),
        Object::Int(minute as i64),
        Object::Int(second as i64),
        Object::Int(usec as i64),
    ];
    if !tzinfo.is_null() {
        args.push(unsafe { crate::object::clone_object(tzinfo) });
    }
    invoke_class("time", args)
}

fn construct_timedelta(days: c_int, seconds: c_int, microseconds: c_int) -> *mut PyObject {
    invoke_class(
        "timedelta",
        vec![
            Object::Int(days as i64),
            Object::Int(seconds as i64),
            Object::Int(microseconds as i64),
        ],
    )
}

fn construct_timezone(offset: *mut PyObject, name: *mut PyObject) -> *mut PyObject {
    let mut args: Vec<Object> = Vec::new();
    if !offset.is_null() {
        args.push(unsafe { crate::object::clone_object(offset) });
    }
    if !name.is_null() {
        args.push(unsafe { crate::object::clone_object(name) });
    }
    invoke_class("timezone", args)
}

/// Look up the class on the running `datetime` module and
/// invoke it with `args`. Caller gets a fresh owned reference;
/// on lookup failure returns NULL and sets an `ImportError` so
/// the C-side can propagate.
fn invoke_class(class_name: &str, args: Vec<Object>) -> *mut PyObject {
    let class_obj = match lookup_datetime_class(class_name) {
        Some(c) => c,
        None => {
            crate::errors::set_pending(
                Some(
                    weavepy_vm::builtin_types::builtin_types()
                        .runtime_error
                        .clone(),
                ),
                Object::from_str(format!("datetime.{class_name} is not available")),
            );
            return ptr::null_mut();
        }
    };
    let res =
        crate::interp::with_interp_mut(|interp| interp.call_object(class_obj.clone(), &args, &[]));
    match res {
        Some(Ok(v)) => crate::object::into_owned(v),
        Some(Err(e)) => {
            crate::errors::set_pending_from_runtime(e);
            ptr::null_mut()
        }
        None => {
            crate::errors::set_pending(
                Some(
                    weavepy_vm::builtin_types::builtin_types()
                        .runtime_error
                        .clone(),
                ),
                Object::from_static("no active interpreter"),
            );
            ptr::null_mut()
        }
    }
}

fn lookup_datetime_class(class_name: &str) -> Option<Object> {
    crate::interp::with_interp_mut(
        |interp| -> Result<Option<Object>, weavepy_vm::error::RuntimeError> {
            let module = interp.import_path("datetime")?;
            match module {
                Object::Module(m) => {
                    let key = weavepy_vm::object::DictKey(Object::from_str(class_name));
                    Ok(m.dict.borrow().get(&key).cloned())
                }
                _ => Ok(None),
            }
        },
    )
    .and_then(|r| r.ok().flatten())
}

fn call_datetime_attr(
    class_name: &str,
    method: &str,
    args_tuple: *mut PyObject,
) -> Option<*mut PyObject> {
    let class_obj = lookup_datetime_class(class_name)?;
    let mut args_vec = Vec::new();
    if !args_tuple.is_null() {
        if let Object::Tuple(items) = unsafe { crate::object::clone_object(args_tuple) } {
            args_vec = items.iter().cloned().collect();
        }
    }
    // Look up method on class.
    let method_o = match &class_obj {
        Object::Type(t) => t.lookup(method)?,
        _ => return None,
    };
    let res = crate::interp::with_interp_mut(|interp| interp.call_object(method_o, &args_vec, &[]));
    match res {
        Some(Ok(v)) => Some(crate::object::into_owned(v)),
        _ => None,
    }
}

// ---------------------------------------------------------------------
// The static API table + the capsule import path.
// ---------------------------------------------------------------------

/// The single static `PyDateTime_CAPI` instance. Extensions
/// capture a pointer to this through the capsule and use it for
/// the lifetime of the process.
#[no_mangle]
pub static mut PyDateTimeAPI: *mut PyDateTimeCAPI = std::ptr::null_mut();

#[no_mangle]
pub static PyDateTimeAPI_Instance: PyDateTimeCAPI = PyDateTimeCAPI {
    DateType: ptr::null_mut(),
    DateTimeType: ptr::null_mut(),
    TimeType: ptr::null_mut(),
    DeltaType: ptr::null_mut(),
    TZInfoType: ptr::null_mut(),
    TimeZone_UTC: ptr::null_mut(),
    Date_FromDate: date_from_date,
    DateTime_FromDateAndTime: datetime_from_date_and_time,
    Time_FromTime: time_from_time,
    Delta_FromDelta: delta_from_delta,
    TimeZone_FromTimeZone: timezone_from_timezone,
    DateTime_FromTimestamp: datetime_from_timestamp,
    Date_FromTimestamp: date_from_timestamp,
    DateTime_FromDateAndTimeAndFold: datetime_from_date_and_time_and_fold,
    Time_FromTimeAndFold: time_from_time_and_fold,
};

/// Address-of-table — what the capsule wraps. Stored in a
/// `static` so the pointer is stable across the program.
fn capi_table_ptr() -> *mut std::ffi::c_void {
    &PyDateTimeAPI_Instance as *const _ as *mut std::ffi::c_void
}

/// Address-of-table cleanup — kept private; the capsule
/// machinery publishes the table via
/// [`crate::capsule::try_install_well_known_capsule`].
#[doc(hidden)]
pub fn capi_table_void_ptr() -> *mut std::ffi::c_void {
    capi_table_ptr()
}

// ---------------------------------------------------------------------
// Public C-API symbols for type checking and direct construction.
// ---------------------------------------------------------------------

/// `PyDate_FromDate(year, month, day)` — direct construction.
#[no_mangle]
pub unsafe extern "C" fn PyDate_FromDate(year: c_int, month: c_int, day: c_int) -> *mut PyObject {
    construct_date(year, month, day)
}

#[no_mangle]
pub unsafe extern "C" fn PyDateTime_FromDateAndTime(
    year: c_int,
    month: c_int,
    day: c_int,
    hour: c_int,
    minute: c_int,
    second: c_int,
    usec: c_int,
) -> *mut PyObject {
    construct_datetime(
        year,
        month,
        day,
        hour,
        minute,
        second,
        usec,
        ptr::null_mut(),
        0,
    )
}

#[no_mangle]
pub unsafe extern "C" fn PyTime_FromTime(
    hour: c_int,
    minute: c_int,
    second: c_int,
    usec: c_int,
) -> *mut PyObject {
    construct_time(hour, minute, second, usec, ptr::null_mut(), 0)
}

#[no_mangle]
pub unsafe extern "C" fn PyDelta_FromDSU(
    days: c_int,
    seconds: c_int,
    microseconds: c_int,
) -> *mut PyObject {
    construct_timedelta(days, seconds, microseconds)
}

#[no_mangle]
pub unsafe extern "C" fn PyTimeZone_FromOffset(offset: *mut PyObject) -> *mut PyObject {
    construct_timezone(offset, ptr::null_mut())
}

#[no_mangle]
pub unsafe extern "C" fn PyTimeZone_FromOffsetAndName(
    offset: *mut PyObject,
    name: *mut PyObject,
) -> *mut PyObject {
    construct_timezone(offset, name)
}

/// Get year/month/day from a date object.
#[no_mangle]
pub unsafe extern "C" fn PyDateTime_GET_YEAR(o: *mut PyObject) -> c_int {
    get_int_attr(o, "year")
}

#[no_mangle]
pub unsafe extern "C" fn PyDateTime_GET_MONTH(o: *mut PyObject) -> c_int {
    get_int_attr(o, "month")
}

#[no_mangle]
pub unsafe extern "C" fn PyDateTime_GET_DAY(o: *mut PyObject) -> c_int {
    get_int_attr(o, "day")
}

#[no_mangle]
pub unsafe extern "C" fn PyDateTime_DATE_GET_HOUR(o: *mut PyObject) -> c_int {
    get_int_attr(o, "hour")
}

#[no_mangle]
pub unsafe extern "C" fn PyDateTime_DATE_GET_MINUTE(o: *mut PyObject) -> c_int {
    get_int_attr(o, "minute")
}

#[no_mangle]
pub unsafe extern "C" fn PyDateTime_DATE_GET_SECOND(o: *mut PyObject) -> c_int {
    get_int_attr(o, "second")
}

#[no_mangle]
pub unsafe extern "C" fn PyDateTime_DATE_GET_MICROSECOND(o: *mut PyObject) -> c_int {
    get_int_attr(o, "microsecond")
}

#[no_mangle]
pub unsafe extern "C" fn PyDateTime_TIME_GET_HOUR(o: *mut PyObject) -> c_int {
    get_int_attr(o, "hour")
}

#[no_mangle]
pub unsafe extern "C" fn PyDateTime_TIME_GET_MINUTE(o: *mut PyObject) -> c_int {
    get_int_attr(o, "minute")
}

#[no_mangle]
pub unsafe extern "C" fn PyDateTime_TIME_GET_SECOND(o: *mut PyObject) -> c_int {
    get_int_attr(o, "second")
}

#[no_mangle]
pub unsafe extern "C" fn PyDateTime_TIME_GET_MICROSECOND(o: *mut PyObject) -> c_int {
    get_int_attr(o, "microsecond")
}

#[no_mangle]
pub unsafe extern "C" fn PyDateTime_DELTA_GET_DAYS(o: *mut PyObject) -> c_int {
    get_int_attr(o, "days")
}

#[no_mangle]
pub unsafe extern "C" fn PyDateTime_DELTA_GET_SECONDS(o: *mut PyObject) -> c_int {
    get_int_attr(o, "seconds")
}

#[no_mangle]
pub unsafe extern "C" fn PyDateTime_DELTA_GET_MICROSECONDS(o: *mut PyObject) -> c_int {
    get_int_attr(o, "microseconds")
}

fn get_int_attr(o: *mut PyObject, attr: &str) -> c_int {
    if o.is_null() {
        return -1;
    }
    let name = CString::new(attr).unwrap();
    let p = unsafe { crate::abstract_::PyObject_GetAttrString(o, name.as_ptr()) };
    if p.is_null() {
        return -1;
    }
    let v = unsafe { crate::numbers::PyLong_AsLong(p) };
    unsafe { crate::object::Py_DecRef(p) };
    v as c_int
}

// Type-check macros. CPython exposes these as C `static inline`
// helpers; we use function-shaped versions so dlopen'd extensions
// (which can't see the macros) get the same effect.
#[no_mangle]
pub unsafe extern "C" fn PyDate_Check(o: *mut PyObject) -> c_int {
    is_class_named(o, "date")
}

#[no_mangle]
pub unsafe extern "C" fn PyDate_CheckExact(o: *mut PyObject) -> c_int {
    is_class_named_exact(o, "date")
}

#[no_mangle]
pub unsafe extern "C" fn PyDateTime_Check(o: *mut PyObject) -> c_int {
    is_class_named(o, "datetime")
}

#[no_mangle]
pub unsafe extern "C" fn PyDateTime_CheckExact(o: *mut PyObject) -> c_int {
    is_class_named_exact(o, "datetime")
}

#[no_mangle]
pub unsafe extern "C" fn PyTime_Check(o: *mut PyObject) -> c_int {
    is_class_named(o, "time")
}

#[no_mangle]
pub unsafe extern "C" fn PyTime_CheckExact(o: *mut PyObject) -> c_int {
    is_class_named_exact(o, "time")
}

#[no_mangle]
pub unsafe extern "C" fn PyDelta_Check(o: *mut PyObject) -> c_int {
    is_class_named(o, "timedelta")
}

#[no_mangle]
pub unsafe extern "C" fn PyDelta_CheckExact(o: *mut PyObject) -> c_int {
    is_class_named_exact(o, "timedelta")
}

#[no_mangle]
pub unsafe extern "C" fn PyTZInfo_Check(o: *mut PyObject) -> c_int {
    is_class_named(o, "tzinfo")
}

#[no_mangle]
pub unsafe extern "C" fn PyTZInfo_CheckExact(o: *mut PyObject) -> c_int {
    is_class_named_exact(o, "tzinfo")
}

fn is_class_named(o: *mut PyObject, name: &str) -> c_int {
    if o.is_null() {
        return 0;
    }
    match unsafe { crate::object::clone_object(o) } {
        Object::Instance(inst) => {
            for cls in inst.cls().mro.borrow().iter() {
                if cls.name == name {
                    return 1;
                }
            }
            0
        }
        _ => 0,
    }
}

fn is_class_named_exact(o: *mut PyObject, name: &str) -> c_int {
    if o.is_null() {
        return 0;
    }
    match unsafe { crate::object::clone_object(o) } {
        Object::Instance(inst) => {
            if inst.cls().name == name {
                1
            } else {
                0
            }
        }
        _ => 0,
    }
}

/// Force-linker keep-alive for the static.
pub fn touch() -> *const PyDateTimeCAPI {
    &PyDateTimeAPI_Instance as *const _
}

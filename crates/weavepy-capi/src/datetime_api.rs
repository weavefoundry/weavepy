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
use std::os::raw::{c_char, c_int, c_void};
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;

use weavepy_vm::object::{DictData, DictKey, Object};
use weavepy_vm::sync::{Rc, RefCell};
use weavepy_vm::types::{PyInstance, TypeObject};

use crate::layout::tpflags;
use crate::object::{PyObject, PySsizeT};
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
    invoke_class_kw(class_name, &args, &[])
}

/// As [`invoke_class`], but forwarding keyword arguments — needed for
/// `timedelta(weeks=…, hours=…)` normalisation in [`delta_tp_new`].
fn invoke_class_kw(class_name: &str, args: &[Object], kwargs: &[(String, Object)]) -> *mut PyObject {
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
        crate::interp::with_interp_mut(|interp| interp.call_object(class_obj.clone(), args, kwargs));
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

/// Address-of-table — what the capsule wraps.
///
/// Once [`ensure_datetime_bridge`] has run we publish the **dynamic**
/// table (its `DateType`/`DateTimeType`/… slots point at the faithful
/// heap types and `TimeZone_UTC` is filled), so a Cython `cimport
/// datetime` sees real, size-correct, type-checkable type objects.
/// Before that (or if the `datetime` module can't be located) we fall
/// back to the static table, whose type slots are NULL — enough for the
/// function-pointer constructors but not the `PyDateTimeAPI->DateType`
/// macros.
#[doc(hidden)]
pub fn capi_table_void_ptr() -> *mut std::ffi::c_void {
    let dynamic = CAPI_TABLE.load(Ordering::Acquire);
    if dynamic != 0 {
        return dynamic as *mut std::ffi::c_void;
    }
    &PyDateTimeAPI_Instance as *const _ as *mut std::ffi::c_void
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

// ---------------------------------------------------------------------
// Faithful datetime C-ABI types (RFC 0029, wave 5).
//
// CPython's `_datetime` C module defines genuine `PyTypeObject`s
// (`PyDateTime_DateType`, …) whose instances carry the field bytes
// **inline** at fixed offsets, and `Lib/datetime.py` re-exports them.
// Macro-heavy Cython (`cimport datetime`) reads those bytes directly —
// `((PyDateTime_Date*)o)->data[0]` — and validates the type with a
// `tp_basicsize` size-check (`Expected 48 from C header`).
//
// WeavePy's `datetime` module is the pure-Python `_pydatetime`
// fallback, so its classes are `Object::Type` with no C layout. We
// close the gap by minting faithful C `PyTypeObject`s **bridged** to
// those live classes: the bridge gives `PyDate_Check` / subclassing
// (`Timestamp ← datetime`) the correct MRO for free (WeavePy's
// `PyType_IsSubtype` resolves through the bridge), the faithful
// `tp_basicsize` satisfies the size-check, and registering them as
// inline-instance types means a datetime instance crossing into C is
// materialised into a byte-faithful body that the inlined accessor
// macros read correctly.
// ---------------------------------------------------------------------

// CPython 3.13 `tp_basicsize` for each datetime type on LP64
// (`PyObject_HEAD` = 16, `Py_hash_t` = 8). Derived from `datetime.h`:
//   date     = HEAD(16) + hashcode(8) + hastzinfo(1) + data[4]      -> 29 -> 32
//   datetime = …(25)    + data[10]     + fold(1) + pad + tzinfo(8)  -> 48
//   time     = …(25)    + data[6]      + fold(1) + tzinfo(8)        -> 40
//   delta    = HEAD(16) + hashcode(8)  + days/seconds/us (3 * i32)  -> 36 -> 40
//   tzinfo   = HEAD(16)                                             -> 16
//   timezone = HEAD(16) + offset(8) + name(8)                       -> 32
const SIZE_DATE: PySsizeT = 32;
const SIZE_DATETIME: PySsizeT = 48;
const SIZE_TIME: PySsizeT = 40;
const SIZE_DELTA: PySsizeT = 40;
const SIZE_TZINFO: PySsizeT = 16;
const SIZE_TIMEZONE: PySsizeT = 32;

// Instance-body field offsets (relative to the `PyObject` head), shared
// by date/datetime/time per the `_PyTZINFO_HEAD` macro.
const OFF_HASHCODE: usize = 16; // Py_hash_t
const OFF_HASTZINFO: usize = 24; // char
const OFF_DATA: usize = 25; // unsigned char data[]
const OFF_DT_FOLD: usize = 35; // datetime: after data[10]
const OFF_DT_TZINFO: usize = 40; // datetime: 8-aligned after fold
const OFF_TIME_FOLD: usize = 31; // time: after data[6]
const OFF_TIME_TZINFO: usize = 32; // time: 8-aligned after fold
const OFF_DELTA_DAYS: usize = 24; // int
const OFF_DELTA_SECONDS: usize = 28; // int
const OFF_DELTA_US: usize = 32; // int

/// `true` once the six faithful type shells + the dynamic capsule table
/// are minted. They are **interpreter-independent** (layout, flags, and
/// the `tp_base` chain only — none of which depend on a VM class), so
/// this is a genuine process-global one-shot, mirroring CPython, whose
/// `_datetime` C types are themselves process-global statics.
static DT_READY: AtomicBool = AtomicBool::new(false);
/// Init lock: at most one thread mints the faithful type shells.
static DT_INIT_LOCK: Mutex<bool> = Mutex::new(false);
/// The leaked dynamic [`PyDateTimeCAPI`] (type slots filled), as `usize`.
static CAPI_TABLE: AtomicUsize = AtomicUsize::new(0);

// The minted faithful `PyTypeObject *` shells (as `usize`, lock-free for
// the hot instance-packing path). One per `datetime` class name; their
// layout is fixed, so a single global shell serves every interpreter.
macro_rules! dt_slot {
    ($ptr:ident) => {
        static $ptr: AtomicUsize = AtomicUsize::new(0);
    };
}
dt_slot!(PTR_DATE);
dt_slot!(PTR_DATETIME);
dt_slot!(PTR_TIME);
dt_slot!(PTR_DELTA);
dt_slot!(PTR_TZINFO);
dt_slot!(PTR_TIMEZONE);

/// Identity map: a *live VM datetime class* (`Rc::as_ptr`, in any
/// interpreter) → the global faithful type shell it resolves to.
///
/// Populated lazily as each class is first validated, so resolution is
/// correct across **multiple interpreters in one process** (the test
/// harness creates a fresh `Interpreter` per case): every interpreter's
/// `datetime.date` is recorded against the same global shell, rather
/// than the registry being frozen to whichever interpreter happened to
/// import `datetime` first. A user class that merely *shares* the name
/// is rejected up front by [`class_is_datetime`], so it never lands
/// here. Keyed/valued by `usize` (a raw `Rc`/type pointer) to stay
/// `Send`; the `Rc` whose address is the key keeps the class alive for
/// as long as any datetime instance of it can reach C.
fn dt_identity() -> &'static Mutex<std::collections::HashMap<usize, usize>> {
    static MAP: std::sync::OnceLock<Mutex<std::collections::HashMap<usize, usize>>> =
        std::sync::OnceLock::new();
    MAP.get_or_init(|| Mutex::new(std::collections::HashMap::new()))
}

thread_local! {
    /// Per-thread guard: the six shells have been registered as
    /// inline-instance types *on this thread* (the
    /// [`crate::types::INLINE_TYPES`] set is thread-local, so each thread
    /// that crosses datetime instances must opt them in). Set once, the
    /// first time a datetime class resolves on the thread.
    static INLINE_DONE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Cheap gate: is `name` one of the six `datetime` module class names?
fn is_datetime_class_name(name: &str) -> bool {
    matches!(
        name,
        "date" | "datetime" | "time" | "timedelta" | "tzinfo" | "timezone"
    )
}

/// Is `t` genuinely a `datetime`-module class (not a user class that
/// merely shares one of the six names)? Decided from the class's own
/// `__module__` — `_pydatetime` (where WeavePy's pure-Python `datetime`
/// classes are defined and re-exported from) — read straight off the
/// `TypeObject`'s dict. Deliberately **interpreter-free**: this runs on
/// the argument-marshalling hot path (inside [`crate::object::into_owned`]),
/// where re-entering the VM (`with_interp_mut`) to consult the live
/// module would alias the `&mut Interpreter` the caller already holds.
fn class_is_datetime(t: &Rc<TypeObject>) -> bool {
    let key = DictKey(Object::from_static("__module__"));
    matches!(
        t.dict.borrow().get(&key),
        Some(Object::Str(s)) if matches!(&**s, "_pydatetime" | "datetime")
    )
}

/// The global faithful shell for a `datetime` class name (post-mint).
fn ptr_for_name(name: &str) -> Option<*mut PyTypeObject> {
    let p = match name {
        "date" => PTR_DATE.load(Ordering::Relaxed),
        "datetime" => PTR_DATETIME.load(Ordering::Relaxed),
        "time" => PTR_TIME.load(Ordering::Relaxed),
        "timedelta" => PTR_DELTA.load(Ordering::Relaxed),
        "tzinfo" => PTR_TZINFO.load(Ordering::Relaxed),
        "timezone" => PTR_TIMEZONE.load(Ordering::Relaxed),
        _ => 0,
    };
    (p != 0).then_some(p as *mut PyTypeObject)
}

/// Register the six shells as inline-instance types on the current
/// thread (idempotent; [`crate::types::maybe_register_inline_type`]
/// skips `tzinfo`, whose `tp_basicsize` is just the object head).
fn register_inline_on_thread() {
    INLINE_DONE.with(|c| {
        if c.get() {
            return;
        }
        for slot in [
            &PTR_DATE,
            &PTR_DATETIME,
            &PTR_TIME,
            &PTR_DELTA,
            &PTR_TZINFO,
            &PTR_TIMEZONE,
        ] {
            let p = slot.load(Ordering::Relaxed);
            if p != 0 {
                crate::types::maybe_register_inline_type(p as *mut PyTypeObject);
            }
        }
        c.set(true);
    });
}

/// The faithful C type for a `datetime`-module class, minting the global
/// shells on first use. Returns `None` for every other class, *including*
/// a user class that merely shares one of the six names ([`class_is_datetime`]
/// decides by `__module__`, not by name). Called by
/// [`crate::types::find_type_ptr`] so both the class-object crossing (the
/// `__Pyx_ImportType` size-check) and the instance crossing (`ob_type`)
/// resolve to the size-correct type.
///
/// Robust across interpreters: the resolved shell is recorded against
/// *this* class's identity ([`dt_identity`]), so a second interpreter's
/// `datetime.date` resolves correctly too — the registry is never frozen
/// to the first interpreter that imported `datetime`.
pub fn faithful_type_for_class(t: &Rc<TypeObject>) -> Option<*mut PyTypeObject> {
    if !is_datetime_class_name(&t.name) {
        return None;
    }
    let key = Rc::as_ptr(t) as usize;
    // Fast path: this exact class has been validated before.
    let cached = dt_identity().lock().ok().and_then(|m| m.get(&key).copied());
    if let Some(p) = cached {
        register_inline_on_thread();
        return Some(p as *mut PyTypeObject);
    }
    // Validate genuinely-datetime (interpreter-free) before minting.
    if !class_is_datetime(t) {
        return None;
    }
    ensure_dt_types();
    let p = ptr_for_name(&t.name)?;
    // Record the identity and, once, point the shell's bridge at a live
    // class (best-effort: it backs the rarely-taken C-side `tp_alloc`
    // path; the instance- and class-crossing paths never need it). Both
    // under the one lock so concurrent first-crossers don't race the
    // `bridge` write.
    if let Ok(mut map) = dt_identity().lock() {
        map.insert(key, p as usize);
        unsafe {
            if (*p).bridge.is_null() {
                (*p).bridge = Box::into_raw(Box::new(t.clone()));
            }
        }
    }
    register_inline_on_thread();
    Some(p)
}

/// Mint a faithful heap `PyTypeObject` *shell* for a `datetime` class.
///
/// Modelled on [`crate::types::install_user_type`]: immortal refcount,
/// `ob_type = type`, the CPython `tp_name`, the **faithful
/// `tp_basicsize`**, `DEFAULT | BASETYPE | READY` flags, and a `tp_base`
/// chain. The `bridge` is left null — it is filled lazily, per
/// interpreter, by [`faithful_type_for_class`], because the shell's
/// layout (all this function sets) is interpreter-independent. Registered
/// in the heap-type registry (so `bridge_type` / `find_type_ptr` see it)
/// and — when it declares storage past the object head — as an
/// inline-instance type on the minting thread.
fn make_dt_type(name: &str, basicsize: PySsizeT, base: *mut PyTypeObject) -> *mut PyTypeObject {
    let cname = CString::new(name).unwrap_or_else(|_| CString::new("datetime.object").unwrap());
    let mut ty = PyTypeObject::new_zeroed();
    ty.head.ob_type = crate::types::PyType_Type.as_ptr();
    ty.tp_name = cname.into_raw() as *const c_char;
    ty.tp_basicsize = basicsize;
    ty.tp_itemsize = 0;
    ty.tp_dealloc = Some(crate::object::_PyWeavePy_Dealloc);
    ty.tp_flags = tpflags::DEFAULT | tpflags::BASETYPE | tpflags::READY;
    ty.tp_base = base;
    ty.bridge = ptr::null_mut();
    let p = Box::into_raw(Box::new(ty));
    crate::types::register_heap_type(p);
    // Inline-instance registration is gated on `tp_basicsize >
    // sizeof(PyObject)` inside `maybe_register_inline_type`; tzinfo (16)
    // is therefore skipped (it has no inline data and pandas never reads
    // its bytes via a macro), which is exactly what we want.
    crate::types::maybe_register_inline_type(p);
    p
}

/// Public alias kept for the capsule-import call site
/// ([`crate::capsule`]): mint the faithful datetime types + dynamic
/// capsule table. See [`ensure_dt_types`].
pub fn ensure_datetime_bridge() {
    ensure_dt_types();
}

/// Idempotently mint the six faithful datetime type shells and publish
/// the dynamic capsule table. **Interpreter-independent**: it sets only
/// layout (`tp_basicsize`), flags, the `tp_base` chain, and the
/// constructor function pointers — none of which depend on a VM class —
/// so a single global set of types serves every interpreter (matching
/// CPython, whose `_datetime` C types are process-global statics). The
/// per-interpreter `bridge` is wired lazily by [`faithful_type_for_class`].
///
/// Safe to call from any C-triggered path (capsule import,
/// `PyObject_GetAttrString(datetime, "datetime")`, an instance
/// crossing): it never re-enters the bytecode loop.
fn ensure_dt_types() {
    if DT_READY.load(Ordering::Acquire) {
        return;
    }
    let mut done = match DT_INIT_LOCK.lock() {
        Ok(g) => g,
        Err(_) => return,
    };
    if *done {
        return;
    }

    let obj = crate::types::PyBaseObject_Type.as_ptr();
    // `tp_base` chain mirrors CPython: object → date → datetime,
    // object → time, object → timedelta, object → tzinfo → timezone.
    let date = make_dt_type("datetime.date", SIZE_DATE, obj);
    let datetime = make_dt_type("datetime.datetime", SIZE_DATETIME, date);
    let time = make_dt_type("datetime.time", SIZE_TIME, obj);
    let delta = make_dt_type("datetime.timedelta", SIZE_DELTA, obj);
    let tzinfo = make_dt_type("datetime.tzinfo", SIZE_TZINFO, obj);
    let timezone = make_dt_type("datetime.timezone", SIZE_TIMEZONE, tzinfo);

    // RFC 0029 (wave 5): give each shell a faithful `tp_new`. A Cython
    // cdef class subclassing one of them (`_NaT ← datetime`, pandas'
    // `_Timedelta ← timedelta`, `Timestamp ← datetime`) inherits the
    // base's `tp_new` and *calls it directly* (`(*t->tp_base->tp_new)(t,
    // a, k)`); a NULL slot is a jump through address 0. The shell builds
    // the subclass instance, packs the byte-faithful body the inlined
    // `PyDateTime_GET_*` macros read, and seeds the VM slots so the
    // Python view agrees.
    unsafe {
        (*date).tp_new = date_tp_new as *mut c_void;
        (*datetime).tp_new = datetime_tp_new as *mut c_void;
        (*time).tp_new = time_tp_new as *mut c_void;
        (*delta).tp_new = delta_tp_new as *mut c_void;
        (*tzinfo).tp_new = tzinfo_tp_new as *mut c_void;
        (*timezone).tp_new = timezone_tp_new as *mut c_void;
    }
    if std::env::var_os("WEAVEPY_TRACE_NEW").is_some() {
        eprintln!(
            "[DT] shells minted date={date:p}(tp_new={:p}) datetime={datetime:p}(tp_new={:p})",
            unsafe { (*date).tp_new },
            unsafe { (*datetime).tp_new },
        );
    }

    for (slot, ptr) in [
        (&PTR_DATE, date),
        (&PTR_DATETIME, datetime),
        (&PTR_TIME, time),
        (&PTR_DELTA, delta),
        (&PTR_TZINFO, tzinfo),
        (&PTR_TIMEZONE, timezone),
    ] {
        slot.store(ptr as usize, Ordering::Relaxed);
    }

    // Publish the dynamic capsule table with the faithful type slots.
    let table = Box::new(PyDateTimeCAPI {
        DateType: date,
        DateTimeType: datetime,
        TimeType: time,
        DeltaType: delta,
        TZInfoType: tzinfo,
        TimeZone_UTC: ptr::null_mut(), // filled best-effort by `fill_utc_singleton`
        Date_FromDate: date_from_date,
        DateTime_FromDateAndTime: datetime_from_date_and_time,
        Time_FromTime: time_from_time,
        Delta_FromDelta: delta_from_delta,
        TimeZone_FromTimeZone: timezone_from_timezone,
        DateTime_FromTimestamp: datetime_from_timestamp,
        Date_FromTimestamp: date_from_timestamp,
        DateTime_FromDateAndTimeAndFold: datetime_from_date_and_time_and_fold,
        Time_FromTimeAndFold: time_from_time_and_fold,
    });
    let table_ptr = Box::into_raw(table);
    CAPI_TABLE.store(table_ptr as usize, Ordering::Release);
    unsafe { PyDateTimeAPI = table_ptr };

    DT_READY.store(true, Ordering::Release);
    *done = true;
}

/// Best-effort: fill the capsule's `TimeZone_UTC` singleton from the
/// `datetime` module's exported `UTC`. Called after
/// [`ensure_datetime_bridge`] from the capsule-import path (a safe,
/// C-triggered context for crossing the UTC instance into C). A failure
/// leaves the slot NULL — only tz-aware extension paths need it, and a
/// basic DataFrame never touches it.
pub fn fill_utc_singleton() {
    let table = CAPI_TABLE.load(Ordering::Acquire);
    if table == 0 {
        return;
    }
    let table = table as *mut PyDateTimeCAPI;
    if !unsafe { (*table).TimeZone_UTC }.is_null() {
        return;
    }
    let utc = crate::interp::with_interp_mut(|interp| {
        match interp.module_cache().get("datetime")? {
            Object::Module(m) => m.dict.borrow().get(&DictKey(Object::from_static("UTC"))).cloned(),
            _ => None,
        }
    })
    .flatten();
    if let Some(utc) = utc {
        let p = crate::object::into_owned(utc);
        if !p.is_null() {
            unsafe { (*table).TimeZone_UTC = p };
        }
    }
}

/// Pack a VM datetime instance into its faithful inline C body (RFC
/// 0029). Called once, on an instance's first crossing into C
/// ([`crate::instance::instance_body_out`]); datetime objects are
/// immutable, so the byte image never goes stale. A no-op for every
/// non-datetime inline type (numpy arrays, extension instances).
///
/// Reads field values straight from the instance's `__slots__` side
/// table ([`PyInstance::slot_get`]) — **not** the bytecode loop — so it
/// is safe to invoke while the VM is mid-marshal of a call's arguments.
pub fn maybe_pack_datetime_body(body: *mut PyObject, ty: *mut PyTypeObject, inst: &Rc<PyInstance>) {
    if body.is_null() || !DT_READY.load(Ordering::Acquire) {
        return;
    }
    let t = ty as usize;
    if t == PTR_DATE.load(Ordering::Relaxed) {
        unsafe { pack_date(body, inst) };
    } else if t == PTR_DATETIME.load(Ordering::Relaxed) {
        unsafe { pack_datetime(body, inst) };
    } else if t == PTR_TIME.load(Ordering::Relaxed) {
        unsafe { pack_time(body, inst) };
    } else if t == PTR_DELTA.load(Ordering::Relaxed) {
        unsafe { pack_delta(body, inst) };
    }
}

// ---------------------------------------------------------------------
// Faithful `tp_new` for the datetime shells (RFC 0029, wave 5).
//
// CPython's `_datetime` defines a `datetime_new` / `date_new` / … as the
// `tp_new` of each type. A Cython cdef class subclassing one of them
// inherits that slot and *invokes the base's `tp_new` pointer directly*
// from its own generated `__pyx_tp_new` — so the slot must be non-NULL
// and must return a fully-formed instance of the *subclass* `type_`.
//
// Two shapes are served:
//
//   * Constructing the **base** type itself (`datetime(2020, 1, 1)` from
//     C) routes through the pure-Python VM constructor ([`invoke_class`]),
//     which validates ranges and applies the real datetime semantics; the
//     instance packs its faithful body lazily on its first crossing.
//   * Constructing a **subclass** (`type_ != shell`, e.g. pandas' `_NaT`,
//     `Timestamp`, `_Timedelta`) allocates `type_`'s faithful inline body
//     via `tp_alloc`, packs the byte image at the declared offsets, and
//     seeds the VM `__slots__` so a later Python-level `.year` agrees.
//     The subclass's own cdef fields live *past* the base `tp_basicsize`,
//     so packing the base bytes never disturbs them.
// ---------------------------------------------------------------------

/// Decode the `(args, kwds)` a `tp_new` receives into positional
/// [`Object`]s plus the keyword dict (if any).
fn new_args(
    args: *mut PyObject,
    kwds: *mut PyObject,
) -> (Vec<Object>, Option<Rc<RefCell<DictData>>>) {
    let pos = if args.is_null() {
        Vec::new()
    } else {
        match unsafe { crate::object::clone_object(args) } {
            Object::Tuple(items) => items.iter().cloned().collect(),
            Object::None => Vec::new(),
            other => vec![other],
        }
    };
    let kw = if kwds.is_null() {
        None
    } else if let Object::Dict(d) = unsafe { crate::object::clone_object(kwds) } {
        Some(d)
    } else {
        None
    };
    (pos, kw)
}

fn obj_to_i64(o: &Object) -> Option<i64> {
    match o {
        Object::Int(i) => Some(*i),
        Object::Bool(b) => Some(i64::from(*b)),
        _ => None,
    }
}

/// Resolve a datetime constructor argument from positional slot `idx` or
/// keyword `name`, falling back to `dflt`.
fn arg_i64(
    pos: &[Object],
    kw: &Option<Rc<RefCell<DictData>>>,
    idx: usize,
    name: &str,
    dflt: i64,
) -> i64 {
    if let Some(v) = pos.get(idx).and_then(obj_to_i64) {
        return v;
    }
    if let Some(d) = kw {
        if let Some(o) = d.borrow().get(&DictKey(Object::from_str(name))).cloned() {
            if let Some(v) = obj_to_i64(&o) {
                return v;
            }
        }
    }
    dflt
}

/// Resolve a `tzinfo` argument (`None` for naive — slot absent or
/// `Py_None`).
fn arg_tzinfo(pos: &[Object], kw: &Option<Rc<RefCell<DictData>>>, idx: usize) -> Option<Object> {
    let raw = pos.get(idx).cloned().or_else(|| {
        kw.as_ref().and_then(|d| {
            d.borrow()
                .get(&DictKey(Object::from_static("tzinfo")))
                .cloned()
        })
    });
    match raw {
        Some(Object::None) | None => None,
        other => other,
    }
}

/// The native [`PyInstance`] backing a freshly-allocated inline body, so
/// the constructor can seed its VM `__slots__`. `None` for the (rare)
/// non-inline subclass whose body is a plain identity box.
fn body_instance(body: *mut PyObject) -> Option<Rc<PyInstance>> {
    match unsafe { crate::object::clone_object(body) } {
        Object::Instance(inst) => Some(inst),
        _ => None,
    }
}

unsafe extern "C" fn date_tp_new(
    type_: *mut PyTypeObject,
    args: *mut PyObject,
    kwds: *mut PyObject,
) -> *mut PyObject {
    let (pos, kw) = new_args(args, kwds);
    let y = arg_i64(&pos, &kw, 0, "year", 1);
    let mo = arg_i64(&pos, &kw, 1, "month", 1);
    let d = arg_i64(&pos, &kw, 2, "day", 1);
    if type_ as usize == PTR_DATE.load(Ordering::Relaxed) {
        return construct_date(y as c_int, mo as c_int, d as c_int);
    }
    let body = unsafe { crate::genericalloc::PyType_GenericAlloc(type_, 0) };
    if body.is_null() {
        return ptr::null_mut();
    }
    unsafe {
        wi64(body, OFF_HASHCODE, -1);
        wbyte(body, OFF_HASTZINFO, 0);
        pack_ymd(body, y, mo, d);
    }
    if let Some(inst) = body_instance(body) {
        inst.slot_set("_year", Object::Int(y));
        inst.slot_set("_month", Object::Int(mo));
        inst.slot_set("_day", Object::Int(d));
        // The pure-Python `date.__new__` seeds `_hashcode = -1` (its
        // `__hash__` reads this slot lazily). A Cython subclass that reaches
        // this base `tp_new` never runs that Python `__new__`, so seed it
        // here too — otherwise `hash(subclass_instance)` raises
        // `AttributeError: '_hashcode'`.
        inst.slot_set("_hashcode", Object::Int(-1));
    }
    body
}

unsafe extern "C" fn datetime_tp_new(
    type_: *mut PyTypeObject,
    args: *mut PyObject,
    kwds: *mut PyObject,
) -> *mut PyObject {
    let (pos, kw) = new_args(args, kwds);
    let y = arg_i64(&pos, &kw, 0, "year", 1);
    let mo = arg_i64(&pos, &kw, 1, "month", 1);
    let d = arg_i64(&pos, &kw, 2, "day", 1);
    let hh = arg_i64(&pos, &kw, 3, "hour", 0);
    let mi = arg_i64(&pos, &kw, 4, "minute", 0);
    let ss = arg_i64(&pos, &kw, 5, "second", 0);
    let us = arg_i64(&pos, &kw, 6, "microsecond", 0);
    let fold = arg_i64(&pos, &kw, 8, "fold", 0);
    let tz = arg_tzinfo(&pos, &kw, 7);
    if std::env::var_os("WEAVEPY_TRACE_NEW").is_some() {
        eprintln!(
            "[DT] datetime_tp_new type={:p} name={} y={y} mo={mo} d={d}",
            type_,
            crate::types::ctor_trace_name(type_),
        );
    }
    if type_ as usize == PTR_DATETIME.load(Ordering::Relaxed) {
        let mut a = vec![
            Object::Int(y),
            Object::Int(mo),
            Object::Int(d),
            Object::Int(hh),
            Object::Int(mi),
            Object::Int(ss),
            Object::Int(us),
        ];
        if let Some(o) = &tz {
            a.push(o.clone());
        }
        return invoke_class("datetime", a);
    }
    let body = unsafe { crate::genericalloc::PyType_GenericAlloc(type_, 0) };
    if body.is_null() {
        return ptr::null_mut();
    }
    unsafe {
        wi64(body, OFF_HASHCODE, -1);
        pack_ymd(body, y, mo, d);
        wbyte(body, OFF_DATA + 4, (hh & 0xff) as u8);
        wbyte(body, OFF_DATA + 5, (mi & 0xff) as u8);
        wbyte(body, OFF_DATA + 6, (ss & 0xff) as u8);
        wbyte(body, OFF_DATA + 7, ((us >> 16) & 0xff) as u8);
        wbyte(body, OFF_DATA + 8, ((us >> 8) & 0xff) as u8);
        wbyte(body, OFF_DATA + 9, (us & 0xff) as u8);
        wbyte(body, OFF_DT_FOLD, (fold & 0xff) as u8);
        match &tz {
            Some(o) => {
                let p = crate::object::into_owned(o.clone());
                wbyte(body, OFF_HASTZINFO, 1);
                wptr(body, OFF_DT_TZINFO, p);
            }
            None => {
                wbyte(body, OFF_HASTZINFO, 0);
                wptr(body, OFF_DT_TZINFO, crate::object::into_owned(Object::None));
            }
        }
    }
    if let Some(inst) = body_instance(body) {
        inst.slot_set("_year", Object::Int(y));
        inst.slot_set("_month", Object::Int(mo));
        inst.slot_set("_day", Object::Int(d));
        inst.slot_set("_hour", Object::Int(hh));
        inst.slot_set("_minute", Object::Int(mi));
        inst.slot_set("_second", Object::Int(ss));
        inst.slot_set("_microsecond", Object::Int(us));
        inst.slot_set("_fold", Object::Int(fold));
        inst.slot_set("_tzinfo", tz.unwrap_or(Object::None));
        // See the `date`/`timedelta` note: seed the lazy hash cache slot so a
        // Cython subclass reaching this base `tp_new` can still be hashed.
        inst.slot_set("_hashcode", Object::Int(-1));
    }
    body
}

unsafe extern "C" fn time_tp_new(
    type_: *mut PyTypeObject,
    args: *mut PyObject,
    kwds: *mut PyObject,
) -> *mut PyObject {
    let (pos, kw) = new_args(args, kwds);
    let hh = arg_i64(&pos, &kw, 0, "hour", 0);
    let mi = arg_i64(&pos, &kw, 1, "minute", 0);
    let ss = arg_i64(&pos, &kw, 2, "second", 0);
    let us = arg_i64(&pos, &kw, 3, "microsecond", 0);
    let fold = arg_i64(&pos, &kw, 5, "fold", 0);
    let tz = arg_tzinfo(&pos, &kw, 4);
    if type_ as usize == PTR_TIME.load(Ordering::Relaxed) {
        let tzp = match &tz {
            Some(o) => crate::object::into_owned(o.clone()),
            None => ptr::null_mut(),
        };
        let r = construct_time(
            hh as c_int,
            mi as c_int,
            ss as c_int,
            us as c_int,
            tzp,
            fold as c_int,
        );
        if !tzp.is_null() {
            unsafe { crate::object::Py_DecRef(tzp) };
        }
        return r;
    }
    let body = unsafe { crate::genericalloc::PyType_GenericAlloc(type_, 0) };
    if body.is_null() {
        return ptr::null_mut();
    }
    unsafe {
        wi64(body, OFF_HASHCODE, -1);
        wbyte(body, OFF_DATA, (hh & 0xff) as u8);
        wbyte(body, OFF_DATA + 1, (mi & 0xff) as u8);
        wbyte(body, OFF_DATA + 2, (ss & 0xff) as u8);
        wbyte(body, OFF_DATA + 3, ((us >> 16) & 0xff) as u8);
        wbyte(body, OFF_DATA + 4, ((us >> 8) & 0xff) as u8);
        wbyte(body, OFF_DATA + 5, (us & 0xff) as u8);
        wbyte(body, OFF_TIME_FOLD, (fold & 0xff) as u8);
        match &tz {
            Some(o) => {
                let p = crate::object::into_owned(o.clone());
                wbyte(body, OFF_HASTZINFO, 1);
                wptr(body, OFF_TIME_TZINFO, p);
            }
            None => {
                wbyte(body, OFF_HASTZINFO, 0);
                wptr(body, OFF_TIME_TZINFO, crate::object::into_owned(Object::None));
            }
        }
    }
    if let Some(inst) = body_instance(body) {
        inst.slot_set("_hour", Object::Int(hh));
        inst.slot_set("_minute", Object::Int(mi));
        inst.slot_set("_second", Object::Int(ss));
        inst.slot_set("_microsecond", Object::Int(us));
        inst.slot_set("_fold", Object::Int(fold));
        inst.slot_set("_tzinfo", tz.unwrap_or(Object::None));
        // See the `date`/`timedelta` note: seed the lazy hash cache slot so a
        // Cython subclass reaching this base `tp_new` can still be hashed.
        inst.slot_set("_hashcode", Object::Int(-1));
    }
    body
}

unsafe extern "C" fn delta_tp_new(
    type_: *mut PyTypeObject,
    args: *mut PyObject,
    kwds: *mut PyObject,
) -> *mut PyObject {
    // `timedelta(...)` normalisation (weeks/hours/minutes/milliseconds →
    // days/seconds/microseconds, with carries) is intricate; reuse the
    // VM constructor for it. For the base type that *is* the result; for
    // a subclass we normalise via a throwaway base `timedelta`, read the
    // canonical three fields back, then pack them into the subclass body.
    let (pos, kw) = new_args(args, kwds);
    let mut kwa: Vec<(String, Object)> = Vec::new();
    if let Some(d) = &kw {
        for (k, v) in d.borrow().iter() {
            if let Object::Str(s) = &k.0 {
                kwa.push((s.to_string(), v.clone()));
            }
        }
    }
    if type_ as usize == PTR_DELTA.load(Ordering::Relaxed) {
        return invoke_class_kw("timedelta", &pos, &kwa);
    }
    // Normalise through the base constructor.
    let tmp = invoke_class_kw("timedelta", &pos, &kwa);
    if tmp.is_null() {
        return ptr::null_mut();
    }
    let days = get_int_attr(tmp, "days");
    let secs = get_int_attr(tmp, "seconds");
    let us = get_int_attr(tmp, "microseconds");
    unsafe { crate::object::Py_DecRef(tmp) };
    let body = unsafe { crate::genericalloc::PyType_GenericAlloc(type_, 0) };
    if body.is_null() {
        return ptr::null_mut();
    }
    unsafe {
        wi64(body, OFF_HASHCODE, -1);
        wi32(body, OFF_DELTA_DAYS, days);
        wi32(body, OFF_DELTA_SECONDS, secs);
        wi32(body, OFF_DELTA_US, us);
    }
    if let Some(inst) = body_instance(body) {
        inst.slot_set("_days", Object::Int(days as i64));
        inst.slot_set("_seconds", Object::Int(secs as i64));
        inst.slot_set("_microseconds", Object::Int(us as i64));
        // The pure-Python `timedelta.__new__` seeds `_hashcode = -1`; its
        // `__hash__` (which pandas' `_Timedelta.__hash__` defers to for ns/us
        // resolutions) reads this slot lazily. Without it,
        // `hash(pd.Timedelta(...))` raised `AttributeError: '_hashcode'`
        // (and SIGSEGV'd deep in Cython during pytest collection).
        inst.slot_set("_hashcode", Object::Int(-1));
    }
    body
}

/// `tzinfo` carries no inline data; a subclass that reaches the base
/// `tp_new` just needs a non-NULL slot returning a fresh allocation.
unsafe extern "C" fn tzinfo_tp_new(
    type_: *mut PyTypeObject,
    _args: *mut PyObject,
    _kwds: *mut PyObject,
) -> *mut PyObject {
    if type_ as usize == PTR_TZINFO.load(Ordering::Relaxed) {
        return invoke_class("tzinfo", Vec::new());
    }
    unsafe { crate::genericalloc::PyType_GenericAlloc(type_, 0) }
}

unsafe extern "C" fn timezone_tp_new(
    type_: *mut PyTypeObject,
    args: *mut PyObject,
    kwds: *mut PyObject,
) -> *mut PyObject {
    let (pos, _kw) = new_args(args, kwds);
    if type_ as usize == PTR_TIMEZONE.load(Ordering::Relaxed) {
        return invoke_class("timezone", pos);
    }
    unsafe { crate::genericalloc::PyType_GenericAlloc(type_, 0) }
}

// --- raw body writers (offsets are head-relative; head is 0..16) ---

#[inline]
unsafe fn wbyte(body: *mut PyObject, off: usize, v: u8) {
    unsafe { *(body as *mut u8).add(off) = v };
}
#[inline]
unsafe fn wi32(body: *mut PyObject, off: usize, v: i32) {
    unsafe { ((body as *mut u8).add(off) as *mut i32).write_unaligned(v) };
}
#[inline]
unsafe fn wi64(body: *mut PyObject, off: usize, v: i64) {
    unsafe { ((body as *mut u8).add(off) as *mut i64).write_unaligned(v) };
}
#[inline]
unsafe fn wptr(body: *mut PyObject, off: usize, v: *mut PyObject) {
    unsafe { ((body as *mut u8).add(off) as *mut *mut PyObject).write_unaligned(v) };
}

/// Read an integer `__slots__`/`__dict__` field, defaulting to 0.
fn field_i64(inst: &Rc<PyInstance>, name: &str) -> i64 {
    let v = inst.slot_get(name).or_else(|| {
        inst.dict
            .borrow()
            .get(&DictKey(Object::from_str(name)))
            .cloned()
    });
    match v {
        Some(Object::Int(i)) => i,
        Some(Object::Bool(b)) => i64::from(b),
        _ => 0,
    }
}

/// Read the `_tzinfo` field: `Some(obj)` when tz-aware, `None` when
/// naive (the slot is absent or `None`).
fn field_tzinfo(inst: &Rc<PyInstance>) -> Option<Object> {
    match inst.slot_get("_tzinfo") {
        Some(Object::None) | None => None,
        other => other,
    }
}

/// Write `data[]` year/month/day (big-endian year) at `OFF_DATA`.
unsafe fn pack_ymd(body: *mut PyObject, y: i64, mo: i64, d: i64) {
    unsafe {
        wbyte(body, OFF_DATA, ((y >> 8) & 0xff) as u8);
        wbyte(body, OFF_DATA + 1, (y & 0xff) as u8);
        wbyte(body, OFF_DATA + 2, (mo & 0xff) as u8);
        wbyte(body, OFF_DATA + 3, (d & 0xff) as u8);
    }
}

unsafe fn pack_date(body: *mut PyObject, inst: &Rc<PyInstance>) {
    let (y, mo, d) = (
        field_i64(inst, "_year"),
        field_i64(inst, "_month"),
        field_i64(inst, "_day"),
    );
    unsafe {
        wi64(body, OFF_HASHCODE, -1);
        wbyte(body, OFF_HASTZINFO, 0);
        pack_ymd(body, y, mo, d);
    }
}

unsafe fn pack_datetime(body: *mut PyObject, inst: &Rc<PyInstance>) {
    let y = field_i64(inst, "_year");
    let mo = field_i64(inst, "_month");
    let d = field_i64(inst, "_day");
    let hh = field_i64(inst, "_hour");
    let mi = field_i64(inst, "_minute");
    let ss = field_i64(inst, "_second");
    let us = field_i64(inst, "_microsecond");
    let fold = field_i64(inst, "_fold");
    unsafe {
        wi64(body, OFF_HASHCODE, -1);
        pack_ymd(body, y, mo, d);
        wbyte(body, OFF_DATA + 4, (hh & 0xff) as u8);
        wbyte(body, OFF_DATA + 5, (mi & 0xff) as u8);
        wbyte(body, OFF_DATA + 6, (ss & 0xff) as u8);
        wbyte(body, OFF_DATA + 7, ((us >> 16) & 0xff) as u8);
        wbyte(body, OFF_DATA + 8, ((us >> 8) & 0xff) as u8);
        wbyte(body, OFF_DATA + 9, (us & 0xff) as u8);
        wbyte(body, OFF_DT_FOLD, (fold & 0xff) as u8);
        pack_tzinfo(body, inst, OFF_HASTZINFO, OFF_DT_TZINFO);
    }
}

unsafe fn pack_time(body: *mut PyObject, inst: &Rc<PyInstance>) {
    let hh = field_i64(inst, "_hour");
    let mi = field_i64(inst, "_minute");
    let ss = field_i64(inst, "_second");
    let us = field_i64(inst, "_microsecond");
    let fold = field_i64(inst, "_fold");
    unsafe {
        wi64(body, OFF_HASHCODE, -1);
        wbyte(body, OFF_DATA, (hh & 0xff) as u8);
        wbyte(body, OFF_DATA + 1, (mi & 0xff) as u8);
        wbyte(body, OFF_DATA + 2, (ss & 0xff) as u8);
        wbyte(body, OFF_DATA + 3, ((us >> 16) & 0xff) as u8);
        wbyte(body, OFF_DATA + 4, ((us >> 8) & 0xff) as u8);
        wbyte(body, OFF_DATA + 5, (us & 0xff) as u8);
        wbyte(body, OFF_TIME_FOLD, (fold & 0xff) as u8);
        pack_tzinfo(body, inst, OFF_HASTZINFO, OFF_TIME_TZINFO);
    }
}

/// Shared tz packing: set `hastzinfo` + the `tzinfo` pointer. Naive
/// objects store `hastzinfo = 0` and `Py_None` (the
/// `PyDateTime_DATE_GET_TZINFO` macro short-circuits to `Py_None`
/// without reading the field); aware objects retain one owned reference
/// to the crossed tzinfo for the body's lifetime.
unsafe fn pack_tzinfo(body: *mut PyObject, inst: &Rc<PyInstance>, off_flag: usize, off_ptr: usize) {
    match field_tzinfo(inst) {
        Some(tz) => {
            let p = crate::object::into_owned(tz);
            unsafe {
                wbyte(body, off_flag, 1);
                wptr(body, off_ptr, p);
            }
        }
        None => unsafe {
            wbyte(body, off_flag, 0);
            wptr(body, off_ptr, crate::object::into_owned(Object::None));
        },
    }
}

unsafe fn pack_delta(body: *mut PyObject, inst: &Rc<PyInstance>) {
    let days = field_i64(inst, "_days");
    let secs = field_i64(inst, "_seconds");
    let us = field_i64(inst, "_microseconds");
    unsafe {
        wi64(body, OFF_HASHCODE, -1);
        wi32(body, OFF_DELTA_DAYS, days as i32);
        wi32(body, OFF_DELTA_SECONDS, secs as i32);
        wi32(body, OFF_DELTA_US, us as i32);
    }
}

/// Force-linker keep-alive for the static.
pub fn touch() -> *const PyDateTimeCAPI {
    &PyDateTimeAPI_Instance as *const _
}

#![allow(
    clippy::cast_lossless,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::borrow_as_ptr,
    clippy::struct_field_names
)]

//! The `resource` built-in module.
//!
//! Resource usage queries and limits — the POSIX subset of CPython's
//! `resource` module. `getrusage()` is exposed as a NamedTuple-shaped
//! object (we model it as a tuple plus a type marker for ``isinstance``
//! checks); `getrlimit()` / `setrlimit()` use raw libc.

use crate::sync::Rc;
use crate::sync::RefCell;

use crate::error::{os_error, type_error, RuntimeError};
use crate::import::ModuleCache;
use crate::object::{BuiltinFn, DictData, DictKey, Object, PyModule};

pub fn build(_cache: &ModuleCache) -> Rc<PyModule> {
    let dict = Rc::new(RefCell::new(DictData::new()));
    {
        let mut d = dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("__name__")),
            Object::from_static("resource"),
        );
        d.insert(
            DictKey(Object::from_static("__doc__")),
            Object::from_static("Resource usage information and limits."),
        );

        // Sentinel for "no limit" (RLIM_INFINITY).
        d.insert(
            DictKey(Object::from_static("RLIM_INFINITY")),
            Object::Int(rlim_infinity()),
        );

        // RLIMIT_* constants.
        for (name, value) in RLIMIT_CONSTANTS {
            d.insert(DictKey(Object::from_static(name)), Object::Int(*value));
        }
        // RUSAGE_* constants.
        for (name, value) in RUSAGE_CONSTANTS {
            d.insert(DictKey(Object::from_static(name)), Object::Int(*value));
        }

        d.insert(
            DictKey(Object::from_static("getrlimit")),
            builtin("getrlimit", resource_getrlimit),
        );
        d.insert(
            DictKey(Object::from_static("setrlimit")),
            builtin("setrlimit", resource_setrlimit),
        );
        d.insert(
            DictKey(Object::from_static("getrusage")),
            builtin("getrusage", resource_getrusage),
        );
        d.insert(
            DictKey(Object::from_static("getpagesize")),
            builtin("getpagesize", resource_getpagesize),
        );
    }
    Rc::new(PyModule {
        name: "resource".to_owned(),
        filename: None,
        dict,
    })
}

fn builtin(name: &'static str, body: fn(&[Object]) -> Result<Object, RuntimeError>) -> Object {
    Object::Builtin(Rc::new(BuiltinFn {
        name,
        call: Box::new(body),
        call_kw: None,
    }))
}

fn resource_getrlimit(args: &[Object]) -> Result<Object, RuntimeError> {
    let which = match args.first() {
        Some(Object::Int(n)) => *n as i32,
        _ => return Err(type_error("getrlimit() requires an int resource id")),
    };
    let mut rlim = RawRlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    let ret = unsafe { libc_getrlimit(which, &mut rlim) };
    if ret != 0 {
        return Err(os_error(format!(
            "getrlimit({which}) failed: errno={}",
            last_os_error_code()
        )));
    }
    Ok(Object::Tuple(Rc::from(
        vec![
            Object::Int(rlim.rlim_cur as i64),
            Object::Int(rlim.rlim_max as i64),
        ]
        .into_boxed_slice(),
    )))
}

fn resource_setrlimit(args: &[Object]) -> Result<Object, RuntimeError> {
    let which = match args.first() {
        Some(Object::Int(n)) => *n as i32,
        _ => return Err(type_error("setrlimit() requires an int resource id")),
    };
    let (soft, hard) = match args.get(1) {
        Some(Object::Tuple(t)) if t.len() == 2 => {
            let s = match &t[0] {
                Object::Int(n) => *n,
                _ => return Err(type_error("setrlimit() limits must be ints")),
            };
            let h = match &t[1] {
                Object::Int(n) => *n,
                _ => return Err(type_error("setrlimit() limits must be ints")),
            };
            (s, h)
        }
        _ => return Err(type_error("setrlimit() expects a 2-tuple of limits")),
    };
    let rlim = RawRlimit {
        rlim_cur: soft as u64,
        rlim_max: hard as u64,
    };
    let ret = unsafe { libc_setrlimit(which, &rlim) };
    if ret != 0 {
        return Err(os_error(format!(
            "setrlimit({which}) failed: errno={}",
            last_os_error_code()
        )));
    }
    Ok(Object::None)
}

fn resource_getrusage(args: &[Object]) -> Result<Object, RuntimeError> {
    let who = match args.first() {
        Some(Object::Int(n)) => *n as i32,
        Some(Object::None) | None => 0,
        _ => return Err(type_error("getrusage() requires an int")),
    };
    let mut ru = RawRusage::default();
    let ret = unsafe { libc_getrusage(who, &mut ru) };
    if ret != 0 {
        return Err(os_error(format!(
            "getrusage({who}) failed: errno={}",
            last_os_error_code()
        )));
    }
    let utime = ru.ru_utime_sec as f64 + (ru.ru_utime_usec as f64) / 1.0e6;
    let stime = ru.ru_stime_sec as f64 + (ru.ru_stime_usec as f64) / 1.0e6;
    Ok(Object::Tuple(Rc::from(
        vec![
            Object::Float(utime),
            Object::Float(stime),
            Object::Int(ru.ru_maxrss as i64),
            Object::Int(ru.ru_ixrss as i64),
            Object::Int(ru.ru_idrss as i64),
            Object::Int(ru.ru_isrss as i64),
            Object::Int(ru.ru_minflt as i64),
            Object::Int(ru.ru_majflt as i64),
            Object::Int(ru.ru_nswap as i64),
            Object::Int(ru.ru_inblock as i64),
            Object::Int(ru.ru_oublock as i64),
            Object::Int(ru.ru_msgsnd as i64),
            Object::Int(ru.ru_msgrcv as i64),
            Object::Int(ru.ru_nsignals as i64),
            Object::Int(ru.ru_nvcsw as i64),
            Object::Int(ru.ru_nivcsw as i64),
        ]
        .into_boxed_slice(),
    )))
}

fn resource_getpagesize(_args: &[Object]) -> Result<Object, RuntimeError> {
    let pg = unsafe { libc_getpagesize() };
    Ok(Object::Int(pg as i64))
}

fn last_os_error_code() -> i32 {
    std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
}

// ---------------------------------------------------------------------
// Raw C bindings. We declare them inline to keep dependencies light.
// ---------------------------------------------------------------------

#[repr(C)]
#[allow(non_camel_case_types)]
struct RawRlimit {
    rlim_cur: u64,
    rlim_max: u64,
}

#[repr(C)]
#[derive(Default)]
#[allow(non_camel_case_types)]
struct RawRusage {
    ru_utime_sec: i64,
    ru_utime_usec: i64,
    ru_stime_sec: i64,
    ru_stime_usec: i64,
    ru_maxrss: i64,
    ru_ixrss: i64,
    ru_idrss: i64,
    ru_isrss: i64,
    ru_minflt: i64,
    ru_majflt: i64,
    ru_nswap: i64,
    ru_inblock: i64,
    ru_oublock: i64,
    ru_msgsnd: i64,
    ru_msgrcv: i64,
    ru_nsignals: i64,
    ru_nvcsw: i64,
    ru_nivcsw: i64,
}

#[cfg(unix)]
unsafe fn libc_getrlimit(which: i32, rlim: *mut RawRlimit) -> i32 {
    unsafe {
        extern "C" {
            fn getrlimit(which: i32, rlim: *mut RawRlimit) -> i32;
        }
        getrlimit(which, rlim)
    }
}

#[cfg(not(unix))]
unsafe fn libc_getrlimit(_which: i32, _rlim: *mut RawRlimit) -> i32 {
    -1
}

#[cfg(unix)]
unsafe fn libc_setrlimit(which: i32, rlim: *const RawRlimit) -> i32 {
    unsafe {
        extern "C" {
            fn setrlimit(which: i32, rlim: *const RawRlimit) -> i32;
        }
        setrlimit(which, rlim)
    }
}

#[cfg(not(unix))]
unsafe fn libc_setrlimit(_which: i32, _rlim: *const RawRlimit) -> i32 {
    -1
}

#[cfg(unix)]
unsafe fn libc_getrusage(who: i32, ru: *mut RawRusage) -> i32 {
    unsafe {
        extern "C" {
            fn getrusage(who: i32, ru: *mut RawRusage) -> i32;
        }
        getrusage(who, ru)
    }
}

#[cfg(not(unix))]
unsafe fn libc_getrusage(_who: i32, _ru: *mut RawRusage) -> i32 {
    -1
}

#[cfg(unix)]
unsafe fn libc_getpagesize() -> i32 {
    unsafe {
        extern "C" {
            fn getpagesize() -> i32;
        }
        getpagesize()
    }
}

#[cfg(not(unix))]
unsafe fn libc_getpagesize() -> i32 {
    4096
}

#[cfg(target_pointer_width = "64")]
fn rlim_infinity() -> i64 {
    i64::MAX
}

#[cfg(not(target_pointer_width = "64"))]
fn rlim_infinity() -> i64 {
    i32::MAX as i64
}

#[cfg(target_os = "macos")]
const RLIMIT_CONSTANTS: &[(&str, i64)] = &[
    ("RLIMIT_CPU", 0),
    ("RLIMIT_FSIZE", 1),
    ("RLIMIT_DATA", 2),
    ("RLIMIT_STACK", 3),
    ("RLIMIT_CORE", 4),
    ("RLIMIT_RSS", 5),
    ("RLIMIT_MEMLOCK", 6),
    ("RLIMIT_NPROC", 7),
    ("RLIMIT_NOFILE", 8),
    ("RLIMIT_AS", 5),
];

#[cfg(not(target_os = "macos"))]
const RLIMIT_CONSTANTS: &[(&str, i64)] = &[
    ("RLIMIT_CPU", 0),
    ("RLIMIT_FSIZE", 1),
    ("RLIMIT_DATA", 2),
    ("RLIMIT_STACK", 3),
    ("RLIMIT_CORE", 4),
    ("RLIMIT_RSS", 5),
    ("RLIMIT_NPROC", 6),
    ("RLIMIT_NOFILE", 7),
    ("RLIMIT_MEMLOCK", 8),
    ("RLIMIT_AS", 9),
    ("RLIMIT_LOCKS", 10),
    ("RLIMIT_SIGPENDING", 11),
    ("RLIMIT_MSGQUEUE", 12),
    ("RLIMIT_NICE", 13),
    ("RLIMIT_RTPRIO", 14),
    ("RLIMIT_RTTIME", 15),
];

const RUSAGE_CONSTANTS: &[(&str, i64)] = &[
    ("RUSAGE_SELF", 0),
    ("RUSAGE_CHILDREN", -1),
    #[cfg(not(target_os = "macos"))]
    ("RUSAGE_THREAD", 1),
];

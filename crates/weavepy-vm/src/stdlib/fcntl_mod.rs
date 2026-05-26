#![allow(
    clippy::cast_lossless,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::borrow_as_ptr,
    clippy::unreadable_literal
)]

//! The `fcntl` built-in module.
//!
//! Mirrors CPython's `fcntl` module on Unix: file descriptor control
//! constants (`F_GETFL`, `F_SETFL`, `O_NONBLOCK`, …), the `fcntl()` /
//! `ioctl()` wrappers, and `flock()` advisory locks. The constants are
//! drawn from `libc` on the host; on platforms where `libc` isn't
//! available we fall back to the values that Linux and macOS agree on.
//!
//! WeavePy uses `nix::fcntl` for the actual `fcntl()` syscall and
//! `nix::sys::file`/`nix::libc` for the rest. Calls that take an integer
//! argument (e.g. `F_GETFL` / `F_SETFL`) are wired through; the
//! `struct flock` variants used by `F_GETLK`/`F_SETLK` are documented
//! as unsupported until we have a story for the binary struct (RFC
//! 0026 follow-up).

use crate::sync::Rc;
use crate::sync::RefCell;

use crate::error::{os_error, type_error, value_error, RuntimeError};
use crate::import::ModuleCache;
use crate::object::{BuiltinFn, DictData, DictKey, Object, PyModule};

pub fn build(_cache: &ModuleCache) -> Rc<PyModule> {
    let dict = Rc::new(RefCell::new(DictData::new()));
    {
        let mut d = dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("__name__")),
            Object::from_static("fcntl"),
        );
        d.insert(
            DictKey(Object::from_static("__doc__")),
            Object::from_static("File control and I/O control on file descriptors."),
        );

        // fcntl(2) commands.
        for (name, value) in FCNTL_COMMANDS {
            d.insert(DictKey(Object::from_static(name)), Object::Int(*value));
        }
        // open(2) flags exposed via fcntl in CPython.
        for (name, value) in OPEN_FLAGS {
            d.insert(DictKey(Object::from_static(name)), Object::Int(*value));
        }
        // FD_* flags.
        for (name, value) in FD_FLAGS {
            d.insert(DictKey(Object::from_static(name)), Object::Int(*value));
        }
        // flock(2) constants.
        for (name, value) in FLOCK_FLAGS {
            d.insert(DictKey(Object::from_static(name)), Object::Int(*value));
        }
        // lockf(3) constants.
        for (name, value) in LOCKF_FLAGS {
            d.insert(DictKey(Object::from_static(name)), Object::Int(*value));
        }

        // Function bindings.
        d.insert(
            DictKey(Object::from_static("fcntl")),
            builtin("fcntl", fcntl_fcntl),
        );
        d.insert(
            DictKey(Object::from_static("ioctl")),
            builtin("ioctl", fcntl_ioctl),
        );
        d.insert(
            DictKey(Object::from_static("flock")),
            builtin("flock", fcntl_flock),
        );
        d.insert(
            DictKey(Object::from_static("lockf")),
            builtin("lockf", fcntl_lockf),
        );
    }
    Rc::new(PyModule {
        name: "fcntl".to_owned(),
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

// ---------------------------------------------------------------------
// fcntl(2) wrapper.
//
// Signature mirrors CPython: `fcntl.fcntl(fd, op, arg=0)`. The third
// argument may be an int or a buffer; we accept ints today and reject
// buffers (the binary-flock case lives behind `struct flock`).
// ---------------------------------------------------------------------
fn fcntl_fcntl(args: &[Object]) -> Result<Object, RuntimeError> {
    let fd = extract_fd(args.first())?;
    let op = extract_int(args.get(1), "op")?;
    let arg = match args.get(2) {
        Some(Object::Int(n)) => *n as i32,
        Some(Object::None) | None => 0,
        Some(other) => {
            return Err(type_error(format!(
                "fcntl() arg must be an int, got '{}'",
                other.type_name()
            )))
        }
    };
    let ret = unsafe { libc_fcntl(fd, op as i32, arg) };
    if ret < 0 {
        return Err(os_error(format!(
            "fcntl({fd}, {op}, {arg}) failed: errno={}",
            last_os_error_code()
        )));
    }
    Ok(Object::Int(ret as i64))
}

fn fcntl_ioctl(args: &[Object]) -> Result<Object, RuntimeError> {
    let fd = extract_fd(args.first())?;
    let request = extract_int(args.get(1), "request")?;
    let arg = match args.get(2) {
        Some(Object::Int(n)) => *n,
        Some(Object::None) | None => 0,
        Some(other) => {
            return Err(type_error(format!(
                "ioctl() arg must be an int, got '{}'",
                other.type_name()
            )))
        }
    };
    let ret = unsafe { libc_ioctl(fd, request as u64, arg) };
    if ret < 0 {
        return Err(os_error(format!(
            "ioctl({fd}, {request}, {arg}) failed: errno={}",
            last_os_error_code()
        )));
    }
    Ok(Object::Int(ret as i64))
}

fn fcntl_flock(args: &[Object]) -> Result<Object, RuntimeError> {
    let fd = extract_fd(args.first())?;
    let op = extract_int(args.get(1), "operation")?;
    let ret = unsafe { libc_flock(fd, op as i32) };
    if ret < 0 {
        return Err(os_error(format!(
            "flock({fd}, {op}) failed: errno={}",
            last_os_error_code()
        )));
    }
    Ok(Object::None)
}

fn fcntl_lockf(args: &[Object]) -> Result<Object, RuntimeError> {
    let fd = extract_fd(args.first())?;
    let cmd = extract_int(args.get(1), "cmd")?;
    let length = match args.get(2) {
        Some(Object::Int(n)) => *n,
        Some(Object::None) | None => 0,
        Some(other) => {
            return Err(type_error(format!(
                "lockf() length must be an int, got '{}'",
                other.type_name()
            )))
        }
    };
    let ret = unsafe { libc_lockf(fd, cmd as i32, length as i64) };
    if ret < 0 {
        return Err(os_error(format!(
            "lockf({fd}, {cmd}, {length}) failed: errno={}",
            last_os_error_code()
        )));
    }
    Ok(Object::None)
}

fn extract_fd(arg: Option<&Object>) -> Result<i32, RuntimeError> {
    match arg {
        Some(Object::Int(n)) => Ok(*n as i32),
        Some(obj) => Err(type_error(format!(
            "fd must be an int, got '{}'",
            obj.type_name()
        ))),
        None => Err(type_error("missing fd")),
    }
}

fn extract_int(arg: Option<&Object>, name: &str) -> Result<i64, RuntimeError> {
    match arg {
        Some(Object::Int(n)) => Ok(*n),
        Some(other) => Err(type_error(format!(
            "{name} must be an int, got '{}'",
            other.type_name()
        ))),
        None => Err(value_error(format!("missing {name}"))),
    }
}

fn last_os_error_code() -> i32 {
    std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
}

// ---------------------------------------------------------------------
// Bindings via raw libc. We rely on the host's libc being available
// (the only platforms WeavePy ships on are macOS / Linux / Windows;
// Windows callers that import fcntl get a stub module). The
// declarations are duplicated here rather than pulling in the `libc`
// crate so this module stays dependency-light.
// ---------------------------------------------------------------------
#[cfg(unix)]
unsafe fn libc_fcntl(fd: i32, op: i32, arg: i32) -> i32 {
    unsafe {
        extern "C" {
            fn fcntl(fd: i32, op: i32, ...) -> i32;
        }
        fcntl(fd, op, arg)
    }
}

#[cfg(not(unix))]
unsafe fn libc_fcntl(_fd: i32, _op: i32, _arg: i32) -> i32 {
    -1
}

#[cfg(unix)]
unsafe fn libc_ioctl(fd: i32, request: u64, arg: i64) -> i32 {
    unsafe {
        extern "C" {
            fn ioctl(fd: i32, request: u64, ...) -> i32;
        }
        ioctl(fd, request, arg)
    }
}

#[cfg(not(unix))]
unsafe fn libc_ioctl(_fd: i32, _request: u64, _arg: i64) -> i32 {
    -1
}

#[cfg(unix)]
unsafe fn libc_flock(fd: i32, op: i32) -> i32 {
    unsafe {
        extern "C" {
            fn flock(fd: i32, op: i32) -> i32;
        }
        flock(fd, op)
    }
}

#[cfg(not(unix))]
unsafe fn libc_flock(_fd: i32, _op: i32) -> i32 {
    -1
}

#[cfg(unix)]
unsafe fn libc_lockf(fd: i32, cmd: i32, length: i64) -> i32 {
    unsafe {
        extern "C" {
            fn lockf(fd: i32, cmd: i32, length: i64) -> i32;
        }
        lockf(fd, cmd, length)
    }
}

#[cfg(not(unix))]
unsafe fn libc_lockf(_fd: i32, _cmd: i32, _length: i64) -> i32 {
    -1
}

// ---------------------------------------------------------------------
// Constants. These are the values shared by macOS / Linux for the
// commands and flags that get exercised by `multiprocessing`,
// `subprocess`, `socket`, and friends. The host-specific values match
// Linux+macOS for the common ones; pull from libc when feasible.
// ---------------------------------------------------------------------

#[cfg(target_os = "macos")]
const FCNTL_COMMANDS: &[(&str, i64)] = &[
    ("F_DUPFD", 0),
    ("F_GETFD", 1),
    ("F_SETFD", 2),
    ("F_GETFL", 3),
    ("F_SETFL", 4),
    ("F_GETOWN", 5),
    ("F_SETOWN", 6),
    ("F_GETLK", 7),
    ("F_SETLK", 8),
    ("F_SETLKW", 9),
    ("F_DUPFD_CLOEXEC", 67),
    ("F_NOCACHE", 48),
    ("F_FULLFSYNC", 51),
];

#[cfg(not(target_os = "macos"))]
const FCNTL_COMMANDS: &[(&str, i64)] = &[
    ("F_DUPFD", 0),
    ("F_GETFD", 1),
    ("F_SETFD", 2),
    ("F_GETFL", 3),
    ("F_SETFL", 4),
    ("F_GETLK", 5),
    ("F_SETLK", 6),
    ("F_SETLKW", 7),
    ("F_SETOWN", 8),
    ("F_GETOWN", 9),
    ("F_DUPFD_CLOEXEC", 1030),
    ("F_NOCACHE", 0),
    ("F_FULLFSYNC", 0),
];

const OPEN_FLAGS: &[(&str, i64)] = &[
    ("O_RDONLY", 0),
    ("O_WRONLY", 1),
    ("O_RDWR", 2),
    #[cfg(target_os = "macos")]
    ("O_NONBLOCK", 0x0004),
    #[cfg(not(target_os = "macos"))]
    ("O_NONBLOCK", 0o4000),
    #[cfg(target_os = "macos")]
    ("O_APPEND", 0x0008),
    #[cfg(not(target_os = "macos"))]
    ("O_APPEND", 0o2000),
    #[cfg(target_os = "macos")]
    ("O_CREAT", 0x0200),
    #[cfg(not(target_os = "macos"))]
    ("O_CREAT", 0o100),
    #[cfg(target_os = "macos")]
    ("O_TRUNC", 0x0400),
    #[cfg(not(target_os = "macos"))]
    ("O_TRUNC", 0o1000),
    #[cfg(target_os = "macos")]
    ("O_EXCL", 0x0800),
    #[cfg(not(target_os = "macos"))]
    ("O_EXCL", 0o200),
    #[cfg(target_os = "macos")]
    ("O_CLOEXEC", 0x1000000),
    #[cfg(not(target_os = "macos"))]
    ("O_CLOEXEC", 0o2000000),
];

const FD_FLAGS: &[(&str, i64)] = &[("FD_CLOEXEC", 1)];

#[cfg(target_os = "macos")]
const FLOCK_FLAGS: &[(&str, i64)] = &[
    ("LOCK_SH", 1),
    ("LOCK_EX", 2),
    ("LOCK_NB", 4),
    ("LOCK_UN", 8),
];

#[cfg(not(target_os = "macos"))]
const FLOCK_FLAGS: &[(&str, i64)] = &[
    ("LOCK_SH", 1),
    ("LOCK_EX", 2),
    ("LOCK_NB", 4),
    ("LOCK_UN", 8),
];

const LOCKF_FLAGS: &[(&str, i64)] = &[("F_LOCK", 1), ("F_TLOCK", 2), ("F_ULOCK", 0), ("F_TEST", 3)];

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

use crate::error::{io_error_to_py, overflow_error, type_error, value_error, RuntimeError};
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
        // struct flock lock types (F_RDLCK / F_WRLCK / F_UNLCK).
        for (name, value) in LOCK_TYPES {
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
        binds_instance: false,
        call: Box::new(body),
        call_kw: None,
    }))
}

// ---------------------------------------------------------------------
// fcntl(2) wrapper.
//
// Signature mirrors CPython: `fcntl.fcntl(fd, op, arg=0)`. The `fd` may
// be an int or any object with a `fileno()` method (file, socket, …).
// The third argument may be an int or a bytes/bytearray buffer: CPython
// copies the buffer into a fixed 1024-byte scratch area, runs the
// syscall against it, and returns the (possibly mutated) prefix as
// `bytes` — exactly what `F_SETLKW`/`F_GETPATH` rely on.
// ---------------------------------------------------------------------
fn fcntl_fcntl(args: &[Object]) -> Result<Object, RuntimeError> {
    let fd = coerce_fd(args.first())?;
    let op = extract_int(args.get(1), "op")? as i32;
    match args.get(2) {
        Some(Object::Bytes(b)) => fcntl_with_buffer(fd, op, &b[..]),
        Some(Object::ByteArray(b)) => {
            let data = b.borrow().clone();
            fcntl_with_buffer(fd, op, &data)
        }
        Some(Object::Int(n)) => fcntl_with_int(fd, op, *n),
        Some(Object::Bool(b)) => fcntl_with_int(fd, op, i64::from(*b)),
        Some(Object::None) | None => fcntl_with_int(fd, op, 0),
        Some(other) => Err(type_error(format!(
            "fcntl() argument 3 must be an integer or bytes, not '{}'",
            other.type_name()
        ))),
    }
}

fn fcntl_with_int(fd: i32, op: i32, arg: i64) -> Result<Object, RuntimeError> {
    let ret = unsafe { libc_fcntl(fd, op, arg as std::os::raw::c_long) };
    if ret < 0 {
        return Err(last_os_err());
    }
    Ok(Object::Int(i64::from(ret)))
}

fn fcntl_with_buffer(fd: i32, op: i32, data: &[u8]) -> Result<Object, RuntimeError> {
    const BUFSZ: usize = 1024;
    if data.len() > BUFSZ {
        return Err(value_error("fcntl bytes arg too long"));
    }
    let mut buf = [0u8; BUFSZ];
    buf[..data.len()].copy_from_slice(data);
    let ret = unsafe { libc_fcntl_ptr(fd, op, buf.as_mut_ptr()) };
    if ret < 0 {
        return Err(last_os_err());
    }
    Ok(Object::Bytes(Rc::from(&buf[..data.len()])))
}

fn fcntl_ioctl(args: &[Object]) -> Result<Object, RuntimeError> {
    let fd = coerce_fd(args.first())?;
    let request = extract_int(args.get(1), "request")?;
    // The mutable-buffer form of `ioctl` mirrors `fcntl`: copy the buffer
    // into scratch, run the syscall, return the mutated prefix.
    match args.get(2) {
        Some(Object::Bytes(b)) => ioctl_with_buffer(fd, request as u64, &b[..]),
        Some(Object::ByteArray(b)) => {
            let data = b.borrow().clone();
            ioctl_with_buffer(fd, request as u64, &data)
        }
        Some(Object::Int(n)) => ioctl_with_int(fd, request as u64, *n),
        Some(Object::Bool(b)) => ioctl_with_int(fd, request as u64, i64::from(*b)),
        Some(Object::None) | None => ioctl_with_int(fd, request as u64, 0),
        Some(other) => Err(type_error(format!(
            "ioctl() argument 3 must be an integer or bytes, not '{}'",
            other.type_name()
        ))),
    }
}

fn ioctl_with_int(fd: i32, request: u64, arg: i64) -> Result<Object, RuntimeError> {
    let ret = unsafe { libc_ioctl(fd, request, arg) };
    if ret < 0 {
        return Err(last_os_err());
    }
    Ok(Object::Int(i64::from(ret)))
}

fn ioctl_with_buffer(fd: i32, request: u64, data: &[u8]) -> Result<Object, RuntimeError> {
    const BUFSZ: usize = 1024;
    if data.len() > BUFSZ {
        return Err(value_error("ioctl bytes arg too long"));
    }
    let mut buf = [0u8; BUFSZ];
    buf[..data.len()].copy_from_slice(data);
    let ret = unsafe { libc_ioctl_ptr(fd, request, buf.as_mut_ptr()) };
    if ret < 0 {
        return Err(last_os_err());
    }
    Ok(Object::Bytes(Rc::from(&buf[..data.len()])))
}

fn fcntl_flock(args: &[Object]) -> Result<Object, RuntimeError> {
    let fd = coerce_fd(args.first())?;
    let op = extract_int(args.get(1), "operation")?;
    let ret = unsafe { libc_flock(fd, op as i32) };
    if ret < 0 {
        return Err(last_os_err());
    }
    Ok(Object::None)
}

// CPython's `fcntl.lockf(fd, cmd, len=0, start=0, whence=0)` does *not*
// call the C `lockf(3)`; it translates the `flock`-style `LOCK_*` flags
// into a `struct flock` and runs `fcntl(F_SETLK | F_SETLKW)`. We mirror
// that exactly so `LOCK_EX|LOCK_NB` doesn't reach the kernel as a bogus
// `lockf` command (which returns EINVAL).
#[cfg(unix)]
fn fcntl_lockf(args: &[Object]) -> Result<Object, RuntimeError> {
    const LOCK_SH: i64 = 1;
    const LOCK_EX: i64 = 2;
    const LOCK_NB: i64 = 4;
    const LOCK_UN: i64 = 8;

    let fd = coerce_fd(args.first())?;
    let code = extract_int(args.get(1), "cmd")?;
    let length = optional_off(args.get(2), "len")?;
    let start = optional_off(args.get(3), "start")?;
    let whence = match args.get(4) {
        Some(Object::Int(n)) => *n,
        Some(Object::Bool(b)) => i64::from(*b),
        Some(Object::None) | None => 0,
        Some(other) => {
            return Err(type_error(format!(
                "lockf() whence must be an int, got '{}'",
                other.type_name()
            )))
        }
    };

    // SAFETY: zero-initialising a POD C struct is well-defined.
    let mut l: libc::flock = unsafe { std::mem::zeroed() };
    l.l_type = if code == LOCK_UN {
        libc::F_UNLCK as _
    } else if code & LOCK_SH != 0 {
        libc::F_RDLCK as _
    } else if code & LOCK_EX != 0 {
        libc::F_WRLCK as _
    } else {
        return Err(value_error(
            "unrecognized lock argument: pass one of LOCK_SH, LOCK_EX or LOCK_UN",
        ));
    };
    l.l_start = start as _;
    l.l_len = length as _;
    l.l_whence = whence as _;

    let cmd = if code & LOCK_NB != 0 {
        libc::F_SETLK
    } else {
        libc::F_SETLKW
    };
    let ret = unsafe { libc::fcntl(fd, cmd, std::ptr::from_mut(&mut l)) };
    if ret < 0 {
        return Err(last_os_err());
    }
    Ok(Object::None)
}

#[cfg(not(unix))]
fn fcntl_lockf(_args: &[Object]) -> Result<Object, RuntimeError> {
    Err(crate::error::os_error(
        "lockf is not supported on this platform",
    ))
}

fn optional_off(arg: Option<&Object>, name: &str) -> Result<i64, RuntimeError> {
    match arg {
        Some(Object::Int(n)) => Ok(*n),
        Some(Object::Bool(b)) => Ok(i64::from(*b)),
        Some(Object::None) | None => Ok(0),
        Some(other) => Err(type_error(format!(
            "lockf() {name} must be an int, got '{}'",
            other.type_name()
        ))),
    }
}

/// CPython's `_PyObject_AsFileDescriptor`: accept an `int` directly, or
/// any object exposing a `fileno()` method that returns one. A negative
/// descriptor is a `ValueError`; an out-of-`int`-range one an
/// `OverflowError`; anything else a `TypeError`.
fn coerce_fd(arg: Option<&Object>) -> Result<i32, RuntimeError> {
    let obj = arg.ok_or_else(|| type_error("function missing required argument 'fd'"))?;
    let raw: i64 = match obj {
        Object::Int(n) => *n,
        Object::Bool(b) => i64::from(*b),
        Object::File(f) => f
            .fileno()
            .ok_or_else(|| value_error("I/O operation on closed file"))?,
        other => {
            let ptr = crate::vm_singletons::current_interpreter_ptr()
                .ok_or_else(|| type_error("argument must be an int, or have a fileno() method."))?;
            // SAFETY: published by the enclosing VM frame on this thread.
            let interp = unsafe { &mut *ptr };
            let meth = interp
                .load_attr_public(other, "fileno")
                .map_err(|_| type_error("argument must be an int, or have a fileno() method."))?;
            match interp.call_object(meth, &[], &[])? {
                Object::Int(n) => n,
                Object::Bool(b) => i64::from(b),
                _ => return Err(type_error("fileno() returned a non-integer")),
            }
        }
    };
    if raw > i64::from(i32::MAX) || raw < i64::from(i32::MIN) {
        return Err(overflow_error("Python int too large to convert to C int"));
    }
    if raw < 0 {
        return Err(value_error(format!(
            "file descriptor cannot be a negative integer ({raw})"
        )));
    }
    Ok(raw as i32)
}

fn extract_int(arg: Option<&Object>, name: &str) -> Result<i64, RuntimeError> {
    match arg {
        Some(Object::Int(n)) => Ok(*n),
        Some(Object::Bool(b)) => Ok(i64::from(*b)),
        Some(other) => Err(type_error(format!(
            "{name} must be an int, got '{}'",
            other.type_name()
        ))),
        None => Err(value_error(format!("missing {name}"))),
    }
}

fn last_os_err() -> RuntimeError {
    io_error_to_py(&std::io::Error::last_os_error())
}

// ---------------------------------------------------------------------
// Bindings via raw libc. We rely on the host's libc being available
// (the only platforms WeavePy ships on are macOS / Linux / Windows;
// Windows callers that import fcntl get a stub module). The
// declarations are duplicated here rather than pulling in the `libc`
// crate so this module stays dependency-light.
// ---------------------------------------------------------------------
#[cfg(unix)]
unsafe fn libc_fcntl(fd: i32, op: i32, arg: std::os::raw::c_long) -> i32 {
    unsafe {
        extern "C" {
            fn fcntl(fd: i32, op: i32, ...) -> i32;
        }
        fcntl(fd, op, arg)
    }
}

#[cfg(unix)]
unsafe fn libc_fcntl_ptr(fd: i32, op: i32, arg: *mut u8) -> i32 {
    unsafe {
        extern "C" {
            fn fcntl(fd: i32, op: i32, ...) -> i32;
        }
        fcntl(fd, op, arg)
    }
}

#[cfg(not(unix))]
unsafe fn libc_fcntl(_fd: i32, _op: i32, _arg: std::os::raw::c_long) -> i32 {
    -1
}

#[cfg(not(unix))]
unsafe fn libc_fcntl_ptr(_fd: i32, _op: i32, _arg: *mut u8) -> i32 {
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

#[cfg(unix)]
unsafe fn libc_ioctl_ptr(fd: i32, request: u64, arg: *mut u8) -> i32 {
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

#[cfg(not(unix))]
unsafe fn libc_ioctl_ptr(_fd: i32, _request: u64, _arg: *mut u8) -> i32 {
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
    ("F_GETPATH", 50),
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

// `struct flock` lock types used by F_GETLK/F_SETLK[W] (packed by callers
// via `struct`). Values differ between macOS (BSD) and Linux.
#[cfg(target_os = "macos")]
const LOCK_TYPES: &[(&str, i64)] = &[("F_RDLCK", 1), ("F_UNLCK", 2), ("F_WRLCK", 3)];

#[cfg(not(target_os = "macos"))]
const LOCK_TYPES: &[(&str, i64)] = &[("F_RDLCK", 0), ("F_WRLCK", 1), ("F_UNLCK", 2)];


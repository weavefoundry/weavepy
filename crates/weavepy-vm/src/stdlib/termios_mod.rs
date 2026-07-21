#![allow(
    clippy::cast_lossless,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::unnecessary_cast
)]

//! The `termios` built-in module (RFC 0055 WS6).
//!
//! Real POSIX terminal control over `libc` — CPython's `Modules/termios.c`
//! shape: `tcgetattr` returns the 7-item attribute list whose `cc` field is
//! a list of 1-byte `bytes` (with `VMIN`/`VTIME` as ints when `ICANON` is
//! off), `tcsetattr` accepts the same shape back, and every failing syscall
//! raises `termios.error(errno, strerror)` — a distinct exception type,
//! *not* an `OSError` subclass, exactly like upstream.

use std::sync::OnceLock;

use crate::sync::Rc;
use crate::sync::RefCell;

use crate::builtin_types::{builtin_types, make_exception_with_class};
use crate::error::{overflow_error, type_error, value_error, PyException, RuntimeError};
use crate::import::ModuleCache;
use crate::object::{BuiltinFn, DictData, DictKey, Object, PyModule};
use crate::types::TypeObject;

pub fn build(_cache: &ModuleCache) -> Rc<PyModule> {
    let dict = Rc::new(RefCell::new(DictData::default()));
    {
        let mut d = dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("__name__")),
            Object::from_static("termios"),
        );
        d.insert(
            DictKey(Object::from_static("__doc__")),
            Object::from_static(
                "This module provides an interface to the Posix calls for tty I/O control.",
            ),
        );
        for (name, value) in constants() {
            d.insert(DictKey(Object::from_str(name)), Object::Int(value));
        }
        d.insert(
            DictKey(Object::from_static("error")),
            Object::Type(error_class()),
        );
        for (name, body) in [
            ("tcgetattr", termios_tcgetattr as fn(&[Object]) -> _),
            ("tcsetattr", termios_tcsetattr),
            ("tcsendbreak", termios_tcsendbreak),
            ("tcdrain", termios_tcdrain),
            ("tcflush", termios_tcflush),
            ("tcflow", termios_tcflow),
            ("tcgetwinsize", termios_tcgetwinsize),
            ("tcsetwinsize", termios_tcsetwinsize),
        ] {
            d.insert(
                DictKey(Object::from_str(name)),
                Object::Builtin(Rc::new(BuiltinFn {
                    name: Box::leak(name.to_owned().into_boxed_str()),
                    binds_instance: false,
                    call: Box::new(body),
                    call_kw: None,
                })),
            );
        }
    }
    Rc::new(PyModule {
        name: "termios".to_owned(),
        filename: None,
        dict,
    })
}

/// `termios.error` — process-global singleton so `except termios.error`
/// keeps one stable identity across imports.
fn error_class() -> Rc<TypeObject> {
    static CLS: OnceLock<Rc<TypeObject>> = OnceLock::new();
    CLS.get_or_init(|| {
        let parent = builtin_types().exception.clone();
        let cls = TypeObject::new_exception("error", parent).expect("termios.error must build");
        cls.dict.borrow_mut().insert(
            DictKey(Object::from_static("__module__")),
            Object::from_static("termios"),
        );
        cls
    })
    .clone()
}

/// `termios.error(errno, strerror)` from the current `errno`.
fn last_termios_error() -> RuntimeError {
    let err = std::io::Error::last_os_error();
    let errno = err.raw_os_error().unwrap_or(0);
    let strerror = err.to_string();
    // Strip io::Error's " (os error N)" suffix; CPython carries the bare
    // strerror text.
    let strerror = strerror
        .split(" (os error")
        .next()
        .unwrap_or(&strerror)
        .to_owned();
    let inst = make_exception_with_class(error_class(), strerror.clone());
    if let Object::Instance(i) = &inst {
        i.dict.borrow_mut().insert(
            DictKey(Object::from_static("args")),
            Object::new_tuple(vec![
                Object::Int(i64::from(errno)),
                Object::from_str(strerror),
            ]),
        );
    }
    RuntimeError::PyException(PyException::new(inst))
}

/// CPython's `_PyObject_AsFileDescriptor`: int, or anything with a
/// `fileno()`. Negative → `ValueError`; > C int → `OverflowError`
/// (`test_tcgetattr_errors` passes `2**1000`); other types → `TypeError`.
fn coerce_fd(arg: Option<&Object>) -> Result<i32, RuntimeError> {
    let obj = arg.ok_or_else(|| type_error("function missing required argument 'fd'"))?;
    let raw: i64 = match obj {
        Object::Int(n) => *n,
        Object::Bool(b) => i64::from(*b),
        long @ Object::Long(_) => long
            .as_i64()
            .ok_or_else(|| overflow_error("Python int too large to convert to C int"))?,
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

fn extract_c_int(obj: &Object, name: &str) -> Result<i32, RuntimeError> {
    match obj {
        Object::Int(n) => i32::try_from(*n)
            .map_err(|_| overflow_error("Python int too large to convert to C int")),
        Object::Bool(b) => Ok(i32::from(*b)),
        Object::Long(_) => Err(overflow_error("Python int too large to convert to C int")),
        other => Err(type_error(format!(
            "{name} must be an int, not {}",
            other.type_name()
        ))),
    }
}

// ---------------------------------------------------------------------
// The syscalls (unix only; the module is not registered elsewhere).
// ---------------------------------------------------------------------

#[cfg(unix)]
fn termios_tcgetattr(args: &[Object]) -> Result<Object, RuntimeError> {
    let fd = coerce_fd(args.first())?;
    let mut mode: libc::termios = unsafe { std::mem::zeroed() };
    if unsafe { libc::tcgetattr(fd, &raw mut mode) } != 0 {
        return Err(last_termios_error());
    }
    let ispeed = unsafe { libc::cfgetispeed(&raw const mode) };
    let ospeed = unsafe { libc::cfgetospeed(&raw const mode) };
    // `cc` entries are 1-byte `bytes`, except VMIN/VTIME which surface as
    // ints when ICANON is off (they hold counts/timeouts then, not
    // characters) — CPython's exact rule.
    let icanon_off = mode.c_lflag & (libc::ICANON as libc::tcflag_t) == 0;
    let mut cc: Vec<Object> = Vec::with_capacity(libc::NCCS);
    for (i, &ch) in mode.c_cc.iter().enumerate() {
        if icanon_off && (i == libc::VMIN as usize || i == libc::VTIME as usize) {
            cc.push(Object::Int(i64::from(ch)));
        } else {
            cc.push(Object::Bytes(Rc::from(&[ch as u8][..])));
        }
    }
    let items = vec![
        Object::Int(mode.c_iflag as i64),
        Object::Int(mode.c_oflag as i64),
        Object::Int(mode.c_cflag as i64),
        Object::Int(mode.c_lflag as i64),
        Object::Int(ispeed as i64),
        Object::Int(ospeed as i64),
        Object::List(Rc::new(RefCell::new(cc))),
    ];
    Ok(Object::List(Rc::new(RefCell::new(items))))
}

#[cfg(unix)]
fn termios_tcsetattr(args: &[Object]) -> Result<Object, RuntimeError> {
    let fd = coerce_fd(args.first())?;
    let when = extract_c_int(
        args.get(1)
            .ok_or_else(|| type_error("tcsetattr() missing 'when'"))?,
        "when",
    )?;
    let attrs = match args.get(2) {
        Some(Object::List(l)) => l.borrow().clone(),
        Some(other) => {
            return Err(type_error(format!(
                "tcsetattr, arg 3: must be 7 element list, not {}",
                other.type_name()
            )))
        }
        None => return Err(type_error("tcsetattr() missing 'attributes'")),
    };
    if attrs.len() != 7 {
        return Err(type_error("tcsetattr, arg 3: must be 7 element list"));
    }
    // Start from the fd's current state so fields we don't model (input
    // baud pairs on Linux, etc.) survive the round-trip.
    let mut mode: libc::termios = unsafe { std::mem::zeroed() };
    if unsafe { libc::tcgetattr(fd, &raw mut mode) } != 0 {
        return Err(last_termios_error());
    }
    let flag = |i: usize| -> Result<libc::tcflag_t, RuntimeError> {
        match &attrs[i] {
            Object::Int(n) => Ok(*n as libc::tcflag_t),
            Object::Bool(b) => Ok(libc::tcflag_t::from(*b)),
            Object::Long(_) => Err(overflow_error("Python int too large to convert to C long")),
            other => Err(type_error(format!(
                "tcsetattr: an integer is required (got type {})",
                other.type_name()
            ))),
        }
    };
    mode.c_iflag = flag(0)?;
    mode.c_oflag = flag(1)?;
    mode.c_cflag = flag(2)?;
    mode.c_lflag = flag(3)?;
    let ispeed = flag(4)? as libc::speed_t;
    let ospeed = flag(5)? as libc::speed_t;
    let cc = match &attrs[6] {
        Object::List(l) => l.borrow().clone(),
        _ => {
            return Err(type_error(
                "tcsetattr: attributes[6] must be 20 element list",
            ))
        }
    };
    if cc.len() != libc::NCCS {
        return Err(type_error(format!(
            "tcsetattr: attributes[6] must be {} element list",
            libc::NCCS
        )));
    }
    for (i, item) in cc.iter().enumerate() {
        match item {
            Object::Bytes(b) if b.len() == 1 => mode.c_cc[i] = b[0] as libc::cc_t,
            Object::Int(n) => {
                mode.c_cc[i] = libc::cc_t::try_from(*n)
                    .map_err(|_| overflow_error("Python int too large to convert to C char"))?;
            }
            Object::Bool(b) => mode.c_cc[i] = libc::cc_t::from(*b),
            Object::Long(_) => {
                return Err(overflow_error("Python int too large to convert to C char"))
            }
            _ => {
                return Err(type_error(
                    "tcsetattr: elements of attributes must be characters or integers",
                ))
            }
        }
    }
    unsafe {
        libc::cfsetispeed(&raw mut mode, ispeed);
        libc::cfsetospeed(&raw mut mode, ospeed);
        if libc::tcsetattr(fd, when, &raw const mode) != 0 {
            return Err(last_termios_error());
        }
    }
    Ok(Object::None)
}

#[cfg(unix)]
fn termios_tcsendbreak(args: &[Object]) -> Result<Object, RuntimeError> {
    let fd = coerce_fd(args.first())?;
    let duration = extract_c_int(
        args.get(1)
            .ok_or_else(|| type_error("tcsendbreak() missing 'duration'"))?,
        "duration",
    )?;
    if unsafe { libc::tcsendbreak(fd, duration) } != 0 {
        return Err(last_termios_error());
    }
    Ok(Object::None)
}

#[cfg(unix)]
fn termios_tcdrain(args: &[Object]) -> Result<Object, RuntimeError> {
    let fd = coerce_fd(args.first())?;
    if unsafe { libc::tcdrain(fd) } != 0 {
        return Err(last_termios_error());
    }
    Ok(Object::None)
}

#[cfg(unix)]
fn termios_tcflush(args: &[Object]) -> Result<Object, RuntimeError> {
    let fd = coerce_fd(args.first())?;
    let queue = extract_c_int(
        args.get(1)
            .ok_or_else(|| type_error("tcflush() missing 'queue'"))?,
        "queue",
    )?;
    if unsafe { libc::tcflush(fd, queue) } != 0 {
        return Err(last_termios_error());
    }
    Ok(Object::None)
}

#[cfg(unix)]
fn termios_tcflow(args: &[Object]) -> Result<Object, RuntimeError> {
    let fd = coerce_fd(args.first())?;
    let action = extract_c_int(
        args.get(1)
            .ok_or_else(|| type_error("tcflow() missing 'action'"))?,
        "action",
    )?;
    if unsafe { libc::tcflow(fd, action) } != 0 {
        return Err(last_termios_error());
    }
    Ok(Object::None)
}

#[cfg(unix)]
fn termios_tcgetwinsize(args: &[Object]) -> Result<Object, RuntimeError> {
    let fd = coerce_fd(args.first())?;
    let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
    if unsafe { libc::ioctl(fd, libc::TIOCGWINSZ, &mut ws) } != 0 {
        return Err(last_termios_error());
    }
    Ok(Object::new_tuple(vec![
        Object::Int(i64::from(ws.ws_row)),
        Object::Int(i64::from(ws.ws_col)),
    ]))
}

#[cfg(unix)]
fn termios_tcsetwinsize(args: &[Object]) -> Result<Object, RuntimeError> {
    let fd = coerce_fd(args.first())?;
    let pair: Vec<Object> = match args.get(1) {
        Some(Object::Tuple(t)) => t.to_vec(),
        Some(Object::List(l)) => l.borrow().clone(),
        _ => {
            return Err(type_error(
                "tcsetwinsize, arg 2: must be a two-item sequence",
            ))
        }
    };
    if pair.len() != 2 {
        return Err(type_error(
            "tcsetwinsize, arg 2: must be a two-item sequence",
        ));
    }
    let row = extract_c_int(&pair[0], "winsize row")?;
    let col = extract_c_int(&pair[1], "winsize col")?;
    if row < 0 || col < 0 {
        return Err(value_error("winsize value(s) out of range"));
    }
    let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
    if unsafe { libc::ioctl(fd, libc::TIOCGWINSZ, &mut ws) } != 0 {
        return Err(last_termios_error());
    }
    ws.ws_row = row as u16;
    ws.ws_col = col as u16;
    if unsafe { libc::ioctl(fd, libc::TIOCSWINSZ, &ws) } != 0 {
        return Err(last_termios_error());
    }
    Ok(Object::None)
}

#[cfg(not(unix))]
mod stubs {
    use super::*;
    fn unsupported() -> RuntimeError {
        crate::error::os_error("termios is not supported on this platform")
    }
    pub fn termios_tcgetattr(_: &[Object]) -> Result<Object, RuntimeError> {
        Err(unsupported())
    }
    pub fn termios_tcsetattr(_: &[Object]) -> Result<Object, RuntimeError> {
        Err(unsupported())
    }
    pub fn termios_tcsendbreak(_: &[Object]) -> Result<Object, RuntimeError> {
        Err(unsupported())
    }
    pub fn termios_tcdrain(_: &[Object]) -> Result<Object, RuntimeError> {
        Err(unsupported())
    }
    pub fn termios_tcflush(_: &[Object]) -> Result<Object, RuntimeError> {
        Err(unsupported())
    }
    pub fn termios_tcflow(_: &[Object]) -> Result<Object, RuntimeError> {
        Err(unsupported())
    }
    pub fn termios_tcgetwinsize(_: &[Object]) -> Result<Object, RuntimeError> {
        Err(unsupported())
    }
    pub fn termios_tcsetwinsize(_: &[Object]) -> Result<Object, RuntimeError> {
        Err(unsupported())
    }
}
#[cfg(not(unix))]
use stubs::*;

// ---------------------------------------------------------------------
// Constants — CPython exports every termios.h name it can see; we
// export the set the stdlib (`tty`, `pty`, `getpass`, `curses` glue)
// and the CPython tests reach for, valued from libc for this target.
// ---------------------------------------------------------------------

#[cfg(unix)]
#[allow(clippy::vec_init_then_push)]
fn constants() -> Vec<(&'static str, i64)> {
    macro_rules! c {
        ($v:expr, $($name:ident),+ $(,)?) => {
            $( $v.push((stringify!($name), libc::$name as i64)); )+
        };
    }
    let mut v: Vec<(&'static str, i64)> = Vec::new();
    // c_iflag
    c!(
        v, IGNBRK, BRKINT, IGNPAR, PARMRK, INPCK, ISTRIP, INLCR, IGNCR, ICRNL, IXON, IXOFF, IXANY,
        IMAXBEL
    );
    // c_oflag
    c!(v, OPOST, ONLCR, OCRNL, ONOCR, ONLRET, OFILL, OFDEL);
    #[cfg(any(target_os = "macos", target_os = "freebsd"))]
    c!(v, OXTABS, ONOEOT);
    #[cfg(target_os = "linux")]
    c!(v, OLCUC, NLDLY, CRDLY, TABDLY, BSDLY, VTDLY, FFDLY);
    // c_cflag
    c!(v, CSIZE, CS5, CS6, CS7, CS8, CSTOPB, CREAD, PARENB, PARODD, HUPCL, CLOCAL, CRTSCTS);
    // c_lflag
    c!(
        v, ECHOKE, ECHOE, ECHOK, ECHO, ECHONL, ECHOPRT, ECHOCTL, ISIG, ICANON, IEXTEN, EXTPROC,
        TOSTOP, FLUSHO, PENDIN, NOFLSH
    );
    #[cfg(target_os = "macos")]
    c!(v, NOKERNINFO);
    // c_cc indices
    c!(
        v, VEOF, VEOL, VEOL2, VERASE, VWERASE, VKILL, VREPRINT, VINTR, VQUIT, VSUSP, VSTART, VSTOP,
        VLNEXT, VDISCARD, VMIN, VTIME
    );
    #[cfg(target_os = "macos")]
    c!(v, VDSUSP);
    v.push(("NCCS", libc::NCCS as i64));
    // tcsetattr `when`
    c!(v, TCSANOW, TCSADRAIN, TCSAFLUSH);
    #[cfg(target_os = "macos")]
    v.push(("TCSASOFT", 0x10));
    // tcflush queues / tcflow actions
    c!(v, TCIFLUSH, TCOFLUSH, TCIOFLUSH, TCOOFF, TCOON, TCIOFF, TCION);
    // speeds
    c!(
        v, B0, B50, B75, B110, B134, B150, B200, B300, B600, B1200, B1800, B2400, B4800, B9600,
        B19200, B38400, B57600, B115200, B230400
    );
    // ioctls the tests/tools poke (`test_ioctl` uses TIOCGPGRP + winsize)
    c!(
        v, TIOCGWINSZ, TIOCSWINSZ, TIOCGPGRP, TIOCSPGRP, TIOCSCTTY, TIOCEXCL, TIOCNXCL, TIOCOUTQ,
        FIONREAD, FIONBIO, FIOCLEX, FIONCLEX
    );
    #[cfg(target_os = "linux")]
    c!(v, TCGETS, TCSETS, TCSETSW, TCSETSF, TCFLSH, TCXONC, TCSBRK, TIOCMGET, TIOCMSET);
    v
}

#[cfg(not(unix))]
fn constants() -> Vec<(&'static str, i64)> {
    Vec::new()
}

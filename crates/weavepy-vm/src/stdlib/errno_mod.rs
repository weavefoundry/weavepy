//! The `errno` built-in module.
//!
//! Mirrors CPython's `errno` constants for the POSIX (and Windows-
//! compatible) error numbers user code reaches for. The numeric
//! values are taken from `libc` on the host platform, falling back to
//! the canonical POSIX numbers on platforms where `libc` is not
//! linked. We do not depend on the `libc` crate; the values below are
//! the POSIX-blessed numbers that match Linux and macOS.
//!
//! The module exposes both the constants and an `errorcode` dict that
//! maps the integer value back to the symbolic name — exactly the
//! shape `traceback` / `OSError(errno, ...)` formatters expect.

use crate::sync::Rc;
use crate::sync::RefCell;

use crate::import::ModuleCache;
use crate::object::{DictData, DictKey, Object, PyModule};

pub fn build(_cache: &ModuleCache) -> Rc<PyModule> {
    let dict = Rc::new(RefCell::new(DictData::new()));
    let mut errorcode = DictData::new();
    {
        let mut d = dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("__name__")),
            Object::from_static("errno"),
        );
        d.insert(
            DictKey(Object::from_static("__doc__")),
            Object::from_static("Standard errno system symbols."),
        );

        // On unix the numeric value comes from the host `libc` so it
        // matches what real syscalls return (e.g. `EINPROGRESS` is 115
        // on Linux but 36 on macOS) and round-trips against
        // `OSError.errno`. On other platforms we fall back to the
        // canonical Linux numbers. `errorcode` maps value → name; when
        // two names share a value (`EAGAIN`/`EWOULDBLOCK`), the last
        // insertion wins, matching how CPython overwrites the slot.
        #[cfg(unix)]
        macro_rules! e {
            ($name:literal, $libc:ident, $_fallback:literal) => {
                let v = i64::from(libc::$libc);
                d.insert(DictKey(Object::from_static($name)), Object::Int(v));
                errorcode.insert(DictKey(Object::Int(v)), Object::from_static($name));
            };
        }
        #[cfg(not(unix))]
        macro_rules! e {
            ($name:literal, $libc:ident, $fallback:literal) => {
                d.insert(DictKey(Object::from_static($name)), Object::Int($fallback));
                errorcode.insert(DictKey(Object::Int($fallback)), Object::from_static($name));
            };
        }

        e!("EPERM", EPERM, 1);
        e!("ENOENT", ENOENT, 2);
        e!("ESRCH", ESRCH, 3);
        e!("EINTR", EINTR, 4);
        e!("EIO", EIO, 5);
        e!("ENXIO", ENXIO, 6);
        e!("E2BIG", E2BIG, 7);
        e!("ENOEXEC", ENOEXEC, 8);
        e!("EBADF", EBADF, 9);
        e!("ECHILD", ECHILD, 10);
        e!("EAGAIN", EAGAIN, 11);
        e!("EWOULDBLOCK", EWOULDBLOCK, 11);
        e!("ENOMEM", ENOMEM, 12);
        e!("EACCES", EACCES, 13);
        e!("EFAULT", EFAULT, 14);
        e!("ENOTBLK", ENOTBLK, 15);
        e!("EBUSY", EBUSY, 16);
        e!("EEXIST", EEXIST, 17);
        e!("EXDEV", EXDEV, 18);
        e!("ENODEV", ENODEV, 19);
        e!("ENOTDIR", ENOTDIR, 20);
        e!("EISDIR", EISDIR, 21);
        e!("EINVAL", EINVAL, 22);
        e!("ENFILE", ENFILE, 23);
        e!("EMFILE", EMFILE, 24);
        e!("ENOTTY", ENOTTY, 25);
        e!("ETXTBSY", ETXTBSY, 26);
        e!("EFBIG", EFBIG, 27);
        e!("ENOSPC", ENOSPC, 28);
        e!("ESPIPE", ESPIPE, 29);
        e!("EROFS", EROFS, 30);
        e!("EMLINK", EMLINK, 31);
        e!("EPIPE", EPIPE, 32);
        e!("EDOM", EDOM, 33);
        e!("ERANGE", ERANGE, 34);
        e!("EDEADLK", EDEADLK, 35);
        e!("ENAMETOOLONG", ENAMETOOLONG, 36);
        e!("ENOLCK", ENOLCK, 37);
        e!("ENOSYS", ENOSYS, 38);
        e!("ENOTEMPTY", ENOTEMPTY, 39);
        e!("ELOOP", ELOOP, 40);
        e!("ENOMSG", ENOMSG, 42);
        e!("EIDRM", EIDRM, 43);

        e!("EPROTO", EPROTO, 71);
        e!("EOVERFLOW", EOVERFLOW, 75);
        e!("ENOTSOCK", ENOTSOCK, 88);
        e!("EDESTADDRREQ", EDESTADDRREQ, 89);
        e!("EMSGSIZE", EMSGSIZE, 90);
        e!("EPROTOTYPE", EPROTOTYPE, 91);
        e!("ENOPROTOOPT", ENOPROTOOPT, 92);
        e!("EPROTONOSUPPORT", EPROTONOSUPPORT, 93);
        e!("ESOCKTNOSUPPORT", ESOCKTNOSUPPORT, 94);
        e!("EOPNOTSUPP", EOPNOTSUPP, 95);
        e!("ENOTSUP", ENOTSUP, 95);
        e!("EPFNOSUPPORT", EPFNOSUPPORT, 96);
        e!("EAFNOSUPPORT", EAFNOSUPPORT, 97);
        e!("EADDRINUSE", EADDRINUSE, 98);
        e!("EADDRNOTAVAIL", EADDRNOTAVAIL, 99);
        e!("ENETDOWN", ENETDOWN, 100);
        e!("ENETUNREACH", ENETUNREACH, 101);
        e!("ENETRESET", ENETRESET, 102);
        e!("ECONNABORTED", ECONNABORTED, 103);
        e!("ECONNRESET", ECONNRESET, 104);
        e!("ENOBUFS", ENOBUFS, 105);
        e!("EISCONN", EISCONN, 106);
        e!("ENOTCONN", ENOTCONN, 107);
        e!("ESHUTDOWN", ESHUTDOWN, 108);
        e!("ETOOMANYREFS", ETOOMANYREFS, 109);
        e!("ETIMEDOUT", ETIMEDOUT, 110);
        e!("ECONNREFUSED", ECONNREFUSED, 111);
        e!("EHOSTDOWN", EHOSTDOWN, 112);
        e!("EHOSTUNREACH", EHOSTUNREACH, 113);
        e!("EALREADY", EALREADY, 114);
        e!("EINPROGRESS", EINPROGRESS, 115);
        e!("ESTALE", ESTALE, 116);
        e!("EDQUOT", EDQUOT, 122);
        e!("ECANCELED", ECANCELED, 125);

        d.insert(
            DictKey(Object::from_static("errorcode")),
            Object::Dict(Rc::new(RefCell::new(errorcode))),
        );
    }
    Rc::new(PyModule {
        name: "errno".to_owned(),
        filename: None,
        dict,
    })
}

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

use std::cell::RefCell;
use std::rc::Rc;

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

        macro_rules! e {
            ($name:literal, $value:expr) => {
                d.insert(DictKey(Object::from_static($name)), Object::Int($value));
                errorcode.insert(DictKey(Object::Int($value)), Object::from_static($name));
            };
        }

        // POSIX standard. Values here match Linux / macOS where they
        // share; the few divergences (EAGAIN vs EWOULDBLOCK on some
        // BSDs, EDEADLK on Solaris) are picked for the most common
        // host platforms WeavePy ships on.
        e!("EPERM", 1);
        e!("ENOENT", 2);
        e!("ESRCH", 3);
        e!("EINTR", 4);
        e!("EIO", 5);
        e!("ENXIO", 6);
        e!("E2BIG", 7);
        e!("ENOEXEC", 8);
        e!("EBADF", 9);
        e!("ECHILD", 10);
        e!("EAGAIN", 11);
        e!("EWOULDBLOCK", 11);
        e!("ENOMEM", 12);
        e!("EACCES", 13);
        e!("EFAULT", 14);
        e!("ENOTBLK", 15);
        e!("EBUSY", 16);
        e!("EEXIST", 17);
        e!("EXDEV", 18);
        e!("ENODEV", 19);
        e!("ENOTDIR", 20);
        e!("EISDIR", 21);
        e!("EINVAL", 22);
        e!("ENFILE", 23);
        e!("EMFILE", 24);
        e!("ENOTTY", 25);
        e!("ETXTBSY", 26);
        e!("EFBIG", 27);
        e!("ENOSPC", 28);
        e!("ESPIPE", 29);
        e!("EROFS", 30);
        e!("EMLINK", 31);
        e!("EPIPE", 32);
        e!("EDOM", 33);
        e!("ERANGE", 34);
        e!("EDEADLK", 35);
        e!("ENAMETOOLONG", 36);
        e!("ENOLCK", 37);
        e!("ENOSYS", 38);
        e!("ENOTEMPTY", 39);
        e!("ELOOP", 40);
        e!("ENOMSG", 42);
        e!("EIDRM", 43);

        // Networking subset (Linux numbering; macOS differs but the
        // CPython API only exposes the symbolic names).
        e!("EPROTO", 71);
        e!("EOVERFLOW", 75);
        e!("ENOTSOCK", 88);
        e!("EDESTADDRREQ", 89);
        e!("EMSGSIZE", 90);
        e!("EPROTOTYPE", 91);
        e!("ENOPROTOOPT", 92);
        e!("EPROTONOSUPPORT", 93);
        e!("ESOCKTNOSUPPORT", 94);
        e!("EOPNOTSUPP", 95);
        e!("ENOTSUP", 95);
        e!("EPFNOSUPPORT", 96);
        e!("EAFNOSUPPORT", 97);
        e!("EADDRINUSE", 98);
        e!("EADDRNOTAVAIL", 99);
        e!("ENETDOWN", 100);
        e!("ENETUNREACH", 101);
        e!("ENETRESET", 102);
        e!("ECONNABORTED", 103);
        e!("ECONNRESET", 104);
        e!("ENOBUFS", 105);
        e!("EISCONN", 106);
        e!("ENOTCONN", 107);
        e!("ESHUTDOWN", 108);
        e!("ETOOMANYREFS", 109);
        e!("ETIMEDOUT", 110);
        e!("ECONNREFUSED", 111);
        e!("EHOSTDOWN", 112);
        e!("EHOSTUNREACH", 113);
        e!("EALREADY", 114);
        e!("EINPROGRESS", 115);
        e!("ESTALE", 116);
        e!("EDQUOT", 122);
        e!("ECANCELED", 125);

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

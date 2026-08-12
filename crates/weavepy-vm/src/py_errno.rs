//! CPython-truthful errno constants, cross-platform (RFC 0063).
//!
//! On POSIX these are the host libc values. On Windows, CPython
//! deliberately does *not* use the CRT's POSIX-flavoured socket errno
//! values (`ECONNREFUSED == 107`, …): `Modules/errnomodule.c` and
//! `Objects/exceptions.c` both prefer the Winsock `WSAE*` codes
//! (`errno.ECONNREFUSED == 10061`), and `winerror_to_errno`
//! (`PC/errmap.h`) passes the 10000–11000 Winsock range through
//! untranslated. Every place the VM dispatches an OS error to a PEP
//! 3151 `OSError` subclass must therefore compare against *these*
//! constants, not `libc::*` — on Windows the libc crate exposes the
//! CRT values, which are the wrong ones.
//!
//! File-domain errnos (`ENOENT`, `EACCES`, …) keep the CRT values on
//! Windows; those match CPython.

#[cfg(unix)]
mod imp {
    pub use libc::{
        E2BIG, EACCES, EAGAIN, EALREADY, EBADF, ECHILD, ECONNABORTED, ECONNREFUSED, ECONNRESET,
        EEXIST, EINPROGRESS, EINTR, EINVAL, EISDIR, EMFILE, ENOENT, ENOEXEC, ENOMEM, ENOSPC,
        ENOTDIR, ENOTEMPTY, EPIPE, ETIMEDOUT, EWOULDBLOCK, EXDEV,
    };
}

#[cfg(windows)]
mod imp {
    // CRT-domain values (match CPython-on-Windows's errno module).
    pub const E2BIG: i32 = 7;
    pub const EACCES: i32 = 13;
    pub const EBADF: i32 = 9;
    pub const ECHILD: i32 = 10;
    pub const EEXIST: i32 = 17;
    pub const EINTR: i32 = 4;
    pub const EINVAL: i32 = 22;
    pub const EISDIR: i32 = 21;
    pub const EMFILE: i32 = 24;
    pub const ENOENT: i32 = 2;
    pub const ENOEXEC: i32 = 8;
    pub const ENOMEM: i32 = 12;
    pub const ENOSPC: i32 = 28;
    pub const ENOTDIR: i32 = 20;
    pub const ENOTEMPTY: i32 = 41;
    pub const EPIPE: i32 = 32;
    pub const EXDEV: i32 = 18;
    // Winsock-domain values: CPython's errno module publishes the
    // `WSAE*` codes under the POSIX names on Windows.
    pub const EAGAIN: i32 = 11; // CRT EAGAIN (no WSA equivalent is published under this name)
    pub const EWOULDBLOCK: i32 = 10035; // WSAEWOULDBLOCK
    pub const EALREADY: i32 = 10037; // WSAEALREADY
    pub const EINPROGRESS: i32 = 10036; // WSAEINPROGRESS
    pub const ECONNABORTED: i32 = 10053; // WSAECONNABORTED
    pub const ECONNREFUSED: i32 = 10061; // WSAECONNREFUSED
    pub const ECONNRESET: i32 = 10054; // WSAECONNRESET
    pub const ETIMEDOUT: i32 = 10060; // WSAETIMEDOUT
}

pub use imp::*;

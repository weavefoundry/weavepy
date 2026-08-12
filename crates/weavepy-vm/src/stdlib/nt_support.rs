//! Shared NT plumbing for the Windows-native runtime core (RFC 0063).
//!
//! This module is the single home for three things every Windows
//! surface consumes:
//!
//! 1. **The CRT fd layer.** WeavePy adopts CPython's fd model on
//!    Windows: everything Python-visible is a CRT file descriptor
//!    (`_open_osfhandle`/`_get_osfhandle` at the io/mmap boundaries),
//!    never a raw `HANDLE`. The UCRT imports live here as one audited
//!    `extern` block, plus the handle↔fd registry that keeps
//!    `std::fs::File`-backed streams and their minted fds from double
//!    closing (a `HANDLE` has exactly one owner; once a CRT fd adopts
//!    it, the fd is that owner).
//! 2. **The error bridge.** `winerror_to_errno` transcribes CPython's
//!    generated `PC/errmap.h`, `format_message` is the
//!    `FormatMessageW` strerror source, and `crt_error_to_py` builds
//!    an `OSError` from the CRT's `errno` domain (which
//!    `std::io::Error` cannot represent on Windows — its
//!    `raw_os_error` is always the Win32 domain).
//! 3. **Wide-string helpers** for the `W`-suffixed Win32 surface.
//!
//! Everything here is `#[cfg(windows)]` (gated at the `mod`
//! declaration).

use std::collections::HashMap;
use std::ffi::c_void;
use std::fs::File;
use std::io;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::io::{AsRawHandle, FromRawHandle, IntoRawHandle, RawHandle};
use std::sync::Mutex;

use crate::error::RuntimeError;

// ---------------------------------------------------------------------------
// UCRT imports (the CRT fd layer + conio). One block, one audit point.
// ---------------------------------------------------------------------------

pub(crate) mod crt {
    #![allow(non_camel_case_types)]
    use std::ffi::c_void;

    pub(crate) type intptr_t = isize;

    unsafe extern "C" {
        pub(crate) fn _open_osfhandle(osfhandle: intptr_t, flags: i32) -> i32;
        pub(crate) fn _get_osfhandle(fd: i32) -> intptr_t;
        pub(crate) fn _close(fd: i32) -> i32;
        pub(crate) fn _read(fd: i32, buf: *mut c_void, count: u32) -> i32;
        pub(crate) fn _write(fd: i32, buf: *const c_void, count: u32) -> i32;
        pub(crate) fn _commit(fd: i32) -> i32;
        pub(crate) fn _dup(fd: i32) -> i32;
        pub(crate) fn _dup2(fd1: i32, fd2: i32) -> i32;
        pub(crate) fn _lseeki64(fd: i32, offset: i64, origin: i32) -> i64;
        pub(crate) fn _chsize_s(fd: i32, size: i64) -> i32;
        pub(crate) fn _isatty(fd: i32) -> i32;
        pub(crate) fn _setmode(fd: i32, mode: i32) -> i32;
        pub(crate) fn _pipe(pfds: *mut i32, psize: u32, textmode: i32) -> i32;
        pub(crate) fn _locking(fd: i32, mode: i32, nbytes: i32) -> i32;
        pub(crate) fn _wsopen_s(
            pfh: *mut i32,
            filename: *const u16,
            oflag: i32,
            shflag: i32,
            pmode: i32,
        ) -> i32;
        pub(crate) fn _errno() -> *mut i32;
        pub(crate) fn strerror(errnum: i32) -> *const i8;
        pub(crate) fn raise(sig: i32) -> i32;
        pub(crate) fn _heapmin() -> i32;
        // conio (msvcrt's console family).
        pub(crate) fn _kbhit() -> i32;
        pub(crate) fn _getch() -> i32;
        pub(crate) fn _getche() -> i32;
        pub(crate) fn _getwch() -> u16;
        pub(crate) fn _getwche() -> u16;
        pub(crate) fn _putch(c: i32) -> i32;
        pub(crate) fn _putwch(c: u16) -> u16;
        pub(crate) fn _ungetch(c: i32) -> i32;
        pub(crate) fn _ungetwch(c: u16) -> u16;
    }

    // CRT `_open`/`_sopen` flag bits (`fcntl.h`). These are *not* the
    // POSIX values; `os.O_*` on Windows must publish exactly these.
    pub(crate) const O_RDONLY: i32 = 0x0000;
    pub(crate) const O_WRONLY: i32 = 0x0001;
    pub(crate) const O_RDWR: i32 = 0x0002;
    pub(crate) const O_APPEND: i32 = 0x0008;
    pub(crate) const O_CREAT: i32 = 0x0100;
    pub(crate) const O_TRUNC: i32 = 0x0200;
    pub(crate) const O_EXCL: i32 = 0x0400;
    pub(crate) const O_TEXT: i32 = 0x4000;
    pub(crate) const O_BINARY: i32 = 0x8000;
    pub(crate) const O_WTEXT: i32 = 0x10000;
    pub(crate) const O_U16TEXT: i32 = 0x20000;
    pub(crate) const O_U8TEXT: i32 = 0x40000;
    pub(crate) const O_NOINHERIT: i32 = 0x0080;
    pub(crate) const O_TEMPORARY: i32 = 0x0040;
    pub(crate) const O_RANDOM: i32 = 0x0010;
    pub(crate) const O_SEQUENTIAL: i32 = 0x0020;
    pub(crate) const O_SHORT_LIVED: i32 = 0x1000;
    // `_sopen_s` share flags (`share.h`). CPython opens `_SH_DENYNO`.
    pub(crate) const SH_DENYNO: i32 = 0x40;
    // `_locking` modes (`sys/locking.h`).
    pub(crate) const LK_UNLCK: i32 = 0;
    pub(crate) const LK_LOCK: i32 = 1;
    pub(crate) const LK_NBLCK: i32 = 2;
    pub(crate) const LK_RLCK: i32 = 3;
    pub(crate) const LK_NBRLCK: i32 = 4;
}

/// The CRT's current `errno` value (the *CRT* domain, distinct from
/// `GetLastError()`'s Win32 domain).
pub(crate) fn crt_errno() -> i32 {
    unsafe { *crt::_errno() }
}

/// CRT `strerror(errno)` — the text CPython shows for CRT-domain
/// failures (`os.read` on a bad fd, …).
pub(crate) fn crt_strerror(errnum: i32) -> String {
    let ptr = unsafe { crt::strerror(errnum) };
    if ptr.is_null() {
        return format!("Unknown error {errnum}");
    }
    unsafe { std::ffi::CStr::from_ptr(ptr) }
        .to_string_lossy()
        .into_owned()
}

// ---------------------------------------------------------------------------
// Wide-string helpers.
// ---------------------------------------------------------------------------

/// NUL-terminated UTF-16 for the `W` Win32 surface.
pub(crate) fn wide(s: &str) -> Vec<u16> {
    std::ffi::OsStr::new(s)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

/// Decode a UTF-16 buffer (no terminator) into a `String`, replacing
/// unpaired surrogates (identity is not load-bearing for error text).
pub(crate) fn from_wide(buf: &[u16]) -> String {
    std::ffi::OsString::from_wide(buf)
        .to_string_lossy()
        .into_owned()
}

/// Decode a NUL-terminated UTF-16 buffer.
pub(crate) fn from_wide_nul(buf: &[u16]) -> String {
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    from_wide(&buf[..len])
}

// ---------------------------------------------------------------------------
// The error bridge.
// ---------------------------------------------------------------------------

/// Transcription of CPython's generated `PC/errmap.h`
/// (`winerror_to_errno`): map a Win32 error to the approximate CRT
/// errno CPython publishes on `OSError.errno`. The Winsock range
/// (10000–11000) passes through untranslated — `errno.WSAE*` values
/// double as errno values on Windows.
pub(crate) fn winerror_to_errno(winerror: i32) -> i32 {
    use crate::py_errno as e;
    match winerror {
        // ERROR_FILE_NOT_FOUND / PATH_NOT_FOUND / INVALID_DRIVE /
        // NO_MORE_FILES / BAD_NETPATH / BAD_NET_NAME / BAD_PATHNAME /
        // FILENAME_EXCED_RANGE
        2 | 3 | 15 | 18 | 53 | 67 | 161 | 206 => e::ENOENT,
        // ERROR_TOO_MANY_OPEN_FILES
        4 => e::EMFILE,
        // ERROR_ACCESS_DENIED / CURRENT_DIRECTORY / WRITE_PROTECT /
        // BAD_UNIT / NOT_READY / BAD_COMMAND / CRC / BAD_LENGTH /
        // SEEK / NOT_DOS_DISK / SECTOR_NOT_FOUND / OUT_OF_PAPER /
        // WRITE_FAULT / READ_FAULT / GEN_FAILURE / SHARING_VIOLATION /
        // LOCK_VIOLATION / WRONG_DISK / SHARING_BUFFER_EXCEEDED /
        // DRIVE_LOCKED / SEEK_ON_DEVICE / NOT_LOCKED / LOCK_FAILED
        5 | 16 | 19..=34 | 36 | 108 | 132 | 158 | 167 => e::EACCES,
        // ERROR_INVALID_HANDLE / INVALID_TARGET_HANDLE /
        // DIRECT_ACCESS_HANDLE
        6 | 114 | 130 => e::EBADF,
        // ERROR_ARENA_TRASHED / NOT_ENOUGH_MEMORY / INVALID_BLOCK /
        // NOT_ENOUGH_QUOTA
        7 | 8 | 9 | 1816 => e::ENOMEM,
        // ERROR_BAD_ENVIRONMENT
        10 => e::E2BIG,
        // ERROR_BAD_FORMAT + the exe-image family
        11 | 182 | 188..=202 => e::ENOEXEC,
        // ERROR_NOT_SAME_DEVICE
        17 => e::EXDEV,
        // ERROR_FILE_EXISTS / ALREADY_EXISTS
        80 | 183 => e::EEXIST,
        // ERROR_NO_PROC_SLOTS / MAX_THRDS_REACHED / NESTING_NOT_ALLOWED
        89 | 164 | 215 => e::EAGAIN,
        // ERROR_BROKEN_PIPE / NO_DATA
        109 | 232 => e::EPIPE,
        // ERROR_DISK_FULL
        112 => e::ENOSPC,
        // ERROR_INVALID_PARAMETER / NEGATIVE_SEEK
        87 | 131 => e::EINVAL,
        // ERROR_WAIT_NO_CHILDREN / CHILD_NOT_COMPLETE
        128 | 129 => e::ECHILD,
        // ERROR_DIR_NOT_EMPTY
        145 => e::ENOTEMPTY,
        // ERROR_DIRECTORY ("The directory name is invalid")
        267 => e::ENOTDIR,
        // ERROR_OPERATION_ABORTED (CancelIoEx / alertable-wait cancel)
        995 => e::EINTR,
        // ERROR_CONNECTION_ABORTED / CONNECTION_REFUSED map into the
        // Winsock-domain values CPython publishes under the POSIX names.
        1236 => e::ECONNABORTED,
        1225 => e::ECONNREFUSED,
        // ERROR_SEM_TIMEOUT
        121 => e::ETIMEDOUT,
        // Winsock's own range passes through.
        10000..=11000 => winerror,
        _ => e::EINVAL,
    }
}

/// `FormatMessageW` for a Win32 (or Winsock) error code, with
/// CPython's trims: trailing CR/LF/dot whitespace removed. Falls back
/// to the CPython shape for unknown codes.
pub(crate) fn format_message(winerror: i32) -> String {
    use windows_sys::Win32::System::Diagnostics::Debug::{
        FormatMessageW, FORMAT_MESSAGE_FROM_SYSTEM, FORMAT_MESSAGE_IGNORE_INSERTS,
    };
    let mut buf = [0u16; 2048];
    let len = unsafe {
        FormatMessageW(
            FORMAT_MESSAGE_FROM_SYSTEM | FORMAT_MESSAGE_IGNORE_INSERTS,
            std::ptr::null(),
            winerror as u32,
            0,
            buf.as_mut_ptr(),
            buf.len() as u32,
            std::ptr::null(),
        )
    };
    if len == 0 {
        return format!("Windows Error 0x{winerror:X}");
    }
    let mut s = from_wide(&buf[..len as usize]);
    while s.ends_with(['\r', '\n', ' ']) {
        s.pop();
    }
    s
}

/// Build the CPython-shaped `OSError` for a Win32 error code:
/// `.winerror` carries the original code, `.errno` the errmap
/// translation, `.strerror` the `FormatMessageW` text, and the PEP
/// 3151 subclass is chosen from the mapped errno.
pub(crate) fn win32_error_to_py(winerror: i32, filename: Option<&str>) -> RuntimeError {
    crate::error::os_error_from_parts(
        winerror_to_errno(winerror),
        format_message(winerror),
        filename,
        None,
        Some(i64::from(winerror)),
    )
}

/// `win32_error_to_py` from the calling thread's `GetLastError()`.
pub(crate) fn last_win32_error_to_py(filename: Option<&str>) -> RuntimeError {
    let code = unsafe { windows_sys::Win32::Foundation::GetLastError() } as i32;
    win32_error_to_py(code, filename)
}

/// Build the CPython-shaped `OSError` for a CRT (`errno`-domain)
/// failure — `os.read` on a stale fd, `_setmode` on a non-fd, ….
/// No `.winerror` is set (CPython's CRT paths don't either).
pub(crate) fn crt_error_to_py(errnum: i32, filename: Option<&str>) -> RuntimeError {
    crate::error::os_error_from_parts(errnum, crt_strerror(errnum), filename, None, None)
}

/// `crt_error_to_py` from the CRT's current `errno`.
pub(crate) fn last_crt_error_to_py(filename: Option<&str>) -> RuntimeError {
    crt_error_to_py(crt_errno(), filename)
}

// ---------------------------------------------------------------------------
// The CRT fd registry: handle↔fd single-ownership bookkeeping.
// ---------------------------------------------------------------------------

/// Handle→fd map for `std::fs::File`-backed streams that have minted
/// a CRT fd via `fileno()`. Once an fd adopts a handle, the fd is the
/// sole owner: the `File` must be defused (`into_raw_handle`) before
/// the stream releases OS resources, and the release goes through
/// `_close(fd)` (which closes the handle). Keyed by raw handle value.
static CRT_FD_REGISTRY: Mutex<Option<HashMap<usize, i32>>> = Mutex::new(None);

fn with_registry<R>(f: impl FnOnce(&mut HashMap<usize, i32>) -> R) -> R {
    let mut guard = CRT_FD_REGISTRY.lock().unwrap_or_else(|e| e.into_inner());
    f(guard.get_or_insert_with(HashMap::new))
}

/// The CRT fd for a `Disk`-backed stream, minting one on first use.
/// The minted fd *adopts* the `File`'s handle (no duplication) — the
/// registry records the adoption so the close path releases exactly
/// once, through the fd.
pub(crate) fn fileno_for_disk_file(f: &File) -> io::Result<i32> {
    let raw = f.as_raw_handle() as usize;
    with_registry(|reg| {
        if let Some(&fd) = reg.get(&raw) {
            return Ok(fd);
        }
        // O_NOINHERIT mirrors PEP 446: descriptors Python mints are
        // non-inheritable. The handle's own inheritance flag is
        // separate and unchanged.
        let fd = unsafe { crt::_open_osfhandle(raw as crt::intptr_t, crt::O_NOINHERIT) };
        if fd < 0 {
            return Err(io::Error::other(
                "could not allocate a CRT file descriptor for this handle",
            ));
        }
        reg.insert(raw, fd);
        Ok(fd)
    })
}

/// Register an fd that already owns `handle` (a stream constructed
/// *from* a CRT fd — `io.open(fd)`, `os.fdopen`).
pub(crate) fn register_fd_for_handle(handle: RawHandle, fd: i32) {
    with_registry(|reg| reg.insert(handle as usize, fd));
}

/// Take (and forget) the fd adopted for `handle`, if any. The caller
/// is about to release the stream and must route the close through
/// `_close(fd)` when this returns `Some`.
pub(crate) fn take_fd_for_handle(handle: RawHandle) -> Option<i32> {
    with_registry(|reg| reg.remove(&(handle as usize)))
}

/// Forget an fd closed out from under us (`os.close(f.fileno())`):
/// drop any registry entry naming it so the stream's own close
/// doesn't re-close a recycled fd.
pub(crate) fn forget_fd(fd: i32) {
    with_registry(|reg| reg.retain(|_, v| *v != fd));
}

/// Release a `Disk` backend on Windows: defuse the `File`'s checked
/// drop, then close through the adopted fd when one exists (the fd
/// owns the handle) or `CloseHandle` directly otherwise. A stale
/// handle/fd reports the error like Unix's swallowed `EBADF` — the
/// caller decides whether to surface it.
pub(crate) fn close_disk_file(f: File) -> io::Result<()> {
    let raw = f.into_raw_handle();
    if let Some(fd) = take_fd_for_handle(raw) {
        let rc = unsafe { crt::_close(fd) };
        if rc < 0 {
            // A stale fd (closed out from under us) is the EBADF story;
            // ERROR_INVALID_HANDLE maps to exactly that via the errmap.
            return Err(io::Error::from_raw_os_error(6));
        }
        return Ok(());
    }
    let ok = unsafe { windows_sys::Win32::Foundation::CloseHandle(raw.cast::<c_void>()) };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// A `std::fs::File` *view* over a CRT fd, for metadata/`std::io`
/// convenience on streams the fd owns. The view must never be
/// dropped as an owner — wrap-and-forget via `ManuallyDrop`.
pub(crate) fn file_view_from_fd(fd: i32) -> io::Result<std::mem::ManuallyDrop<File>> {
    let handle = unsafe { crt::_get_osfhandle(fd) };
    if handle == -1 || handle == -2 {
        // -1: not an open fd; -2: fd not associated with a stream.
        return Err(io::Error::from_raw_os_error(6)); // ERROR_INVALID_HANDLE
    }
    Ok(std::mem::ManuallyDrop::new(unsafe {
        File::from_raw_handle(handle as RawHandle)
    }))
}

/// An owning `std::fs::File` constructed from a CRT fd, with the fd
/// recorded in the registry so the eventual `close_disk_file` routes
/// the release back through `_close(fd)`.
pub(crate) fn owning_file_from_fd(fd: i32) -> io::Result<File> {
    let handle = unsafe { crt::_get_osfhandle(fd) };
    if handle == -1 || handle == -2 {
        return Err(io::Error::from_raw_os_error(6));
    }
    let file = unsafe { File::from_raw_handle(handle as RawHandle) };
    register_fd_for_handle(file.as_raw_handle(), fd);
    Ok(file)
}

// ---------------------------------------------------------------------------
// Small shared Win32 conveniences.
// ---------------------------------------------------------------------------

/// `GetFileType` classification for a CRT fd (pipe/char/disk), used
/// by `os.fstat`'s `st_mode` shaping and `_winapi.GetFileType`.
pub(crate) fn file_type_of_fd(fd: i32) -> Option<u32> {
    let handle = unsafe { crt::_get_osfhandle(fd) };
    if handle == -1 || handle == -2 {
        return None;
    }
    Some(unsafe { windows_sys::Win32::Storage::FileSystem::GetFileType(handle as *mut c_void) })
}

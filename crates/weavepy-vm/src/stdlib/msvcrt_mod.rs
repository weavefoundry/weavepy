//! The `msvcrt` built-in module (RFC 0063 WS2).
//!
//! Transcribes CPython's `PC/msvcrtmodule.c`: the CRT fd↔HANDLE bridge
//! (`get_osfhandle`/`open_osfhandle`), text/binary `setmode`, region
//! `locking` (+ the `LK_*` modes), the conio console family (`kbhit`,
//! `getch`/`getwch`/`getche`/`getwche`, `putch`/`putwch`,
//! `ungetch`/`ungetwch`), `heapmin`, and the Win32 error-mode pair
//! (`SetErrorMode`/`GetErrorMode` + `SEM_*`).
//!
//! Error domains follow CPython exactly: the CRT functions raise the
//! errno-shaped `OSError` (`PyErr_SetFromErrno` ↔
//! [`nt_support::last_crt_error_to_py`]); the error-mode functions live
//! in the Win32 domain and cannot fail. All CRT externs come from
//! [`nt_support::crt`] — the single audited UCRT import block.

use crate::sync::Rc;
use crate::sync::RefCell;

use crate::error::{type_error, RuntimeError};
use crate::import::ModuleCache;
use crate::object::{DictData, DictKey, Object, PyModule};
use crate::stdlib::nt_support::{self, crt};
use crate::stdlib::os::builtin;

pub fn build(_cache: &ModuleCache) -> Rc<PyModule> {
    let dict = Rc::new(RefCell::new(DictData::default()));
    {
        let mut d = dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("__name__")),
            Object::from_static("msvcrt"),
        );
        d.insert(
            DictKey(Object::from_static("__doc__")),
            Object::from_static("Functions from the msvcrt library on Windows platforms."),
        );

        for (name, body) in [
            ("heapmin", msvcrt_heapmin as fn(&[Object]) -> _),
            ("locking", msvcrt_locking),
            ("setmode", msvcrt_setmode),
            ("open_osfhandle", msvcrt_open_osfhandle),
            ("get_osfhandle", msvcrt_get_osfhandle),
            ("kbhit", msvcrt_kbhit),
            ("getch", msvcrt_getch),
            ("getwch", msvcrt_getwch),
            ("getche", msvcrt_getche),
            ("getwche", msvcrt_getwche),
            ("putch", msvcrt_putch),
            ("putwch", msvcrt_putwch),
            ("ungetch", msvcrt_ungetch),
            ("ungetwch", msvcrt_ungetwch),
            ("SetErrorMode", msvcrt_set_error_mode),
            ("GetErrorMode", msvcrt_get_error_mode),
            ("CrtSetReportMode", msvcrt_crt_set_report_mode),
            ("CrtSetReportFile", msvcrt_crt_set_report_file),
        ] {
            d.insert(DictKey(Object::from_static(name)), builtin(name, body));
        }

        // `_locking` modes (`sys/locking.h`) — the values already live in
        // the audited CRT block.
        for (name, val) in [
            ("LK_LOCK", crt::LK_LOCK),
            ("LK_NBLCK", crt::LK_NBLCK),
            ("LK_NBRLCK", crt::LK_NBRLCK),
            ("LK_RLCK", crt::LK_RLCK),
            ("LK_UNLCK", crt::LK_UNLCK),
        ] {
            d.insert(
                DictKey(Object::from_static(name)),
                Object::Int(i64::from(val)),
            );
        }

        // `SetErrorMode` flags (winbase.h).
        for (name, val) in [
            ("SEM_FAILCRITICALERRORS", 0x0001_i64),
            ("SEM_NOGPFAULTERRORBOX", 0x0002),
            ("SEM_NOALIGNMENTFAULTEXCEPT", 0x0004),
            ("SEM_NOOPENFILEERRORBOX", 0x8000),
        ] {
            d.insert(DictKey(Object::from_static(name)), Object::Int(val));
        }

        // CRT debug-report streams (crtdbg.h). CPython publishes the
        // values unconditionally even though the report *functions* only
        // exist in debug CRTs.
        d.insert(DictKey(Object::from_static("_CRT_WARN")), Object::Int(0));
        d.insert(DictKey(Object::from_static("_CRT_ERROR")), Object::Int(1));
        d.insert(DictKey(Object::from_static("_CRT_ASSERT")), Object::Int(2));

        // CPython bakes in the _VC_CRT_*_VERSION macros of the compiling
        // toolchain; WeavePy links the UCRT whose stable binding version
        // is the VS2015+ "14.0" ABI, so publish that.
        d.insert(
            DictKey(Object::from_static("CRT_ASSEMBLY_VERSION")),
            Object::from_static("14.0.0.0"),
        );
    }
    Rc::new(PyModule {
        name: "msvcrt".to_owned(),
        filename: None,
        dict,
    })
}

// ---------------------------------------------------------------------------
// Argument helpers.
// ---------------------------------------------------------------------------

fn int_arg(args: &[Object], idx: usize, func: &str) -> Result<i32, RuntimeError> {
    args.get(idx)
        .and_then(Object::as_i64)
        .map(|v| v as i32)
        .ok_or_else(|| type_error(format!("{func}: argument {} must be an int", idx + 1)))
}

/// The clinic `char` converter: a `bytes`/`bytearray` of length 1.
fn byte_char_arg(args: &[Object], idx: usize, func: &str) -> Result<u8, RuntimeError> {
    match args.get(idx) {
        Some(Object::Bytes(b)) if b.len() == 1 => Ok(b[0]),
        Some(Object::ByteArray(b)) if b.borrow().len() == 1 => Ok(b.borrow()[0]),
        _ => Err(type_error(format!(
            "{func}() argument must be a byte string of length 1"
        ))),
    }
}

/// The clinic `int(accept={str})` converter: a one-character `str`,
/// converted to its ordinal. Code points above the BMP truncate to
/// `wchar_t` exactly like CPython's `_putwch(int)` call does.
fn wchar_arg(args: &[Object], idx: usize, func: &str) -> Result<u16, RuntimeError> {
    match args.get(idx) {
        Some(Object::Str(s)) => {
            let mut it = s.chars();
            match (it.next(), it.next()) {
                (Some(c), None) => Ok(c as u16),
                _ => Err(type_error(format!(
                    "{func}() argument must be a str of length 1"
                ))),
            }
        }
        // A lone surrogate (WeavePy's WStr arc) is a valid one-char str.
        Some(Object::WStr(cps)) if cps.len() == 1 => Ok(cps[0] as u16),
        _ => Err(type_error(format!(
            "{func}() argument must be a str of length 1"
        ))),
    }
}

/// A console wide char as a Python `str`. `_getwch` can hand back one
/// half of a surrogate pair; `str_from_codepoints` keeps it as a lone
/// surrogate exactly like CPython's UCS-2-native `str` would.
fn wchar_to_str(wc: u16) -> Object {
    Object::str_from_codepoints(vec![u32::from(wc)])
}

// ---------------------------------------------------------------------------
// The fd↔HANDLE bridge + file-region functions.
// ---------------------------------------------------------------------------

fn msvcrt_get_osfhandle(args: &[Object]) -> Result<Object, RuntimeError> {
    let fd = int_arg(args, 0, "get_osfhandle")?;
    let handle = unsafe { crt::_get_osfhandle(fd) };
    if handle == -1 {
        return Err(nt_support::last_crt_error_to_py(None));
    }
    // PyLong_FromVoidPtr: the handle surfaces unsigned, like _winapi's.
    Ok(super::winapi_mod::handle_to_object(handle as usize))
}

fn msvcrt_open_osfhandle(args: &[Object]) -> Result<Object, RuntimeError> {
    let handle = super::winapi_mod::handle_arg(args, 0, "open_osfhandle")?;
    let flags = int_arg(args, 1, "open_osfhandle")?;
    let fd = unsafe { crt::_open_osfhandle(handle as crt::intptr_t, flags) };
    if fd == -1 {
        return Err(nt_support::last_crt_error_to_py(None));
    }
    Ok(Object::Int(i64::from(fd)))
}

fn msvcrt_setmode(args: &[Object]) -> Result<Object, RuntimeError> {
    let fd = int_arg(args, 0, "setmode")?;
    let mode = int_arg(args, 1, "setmode")?;
    let old = unsafe { crt::_setmode(fd, mode) };
    if old == -1 {
        return Err(nt_support::last_crt_error_to_py(None));
    }
    Ok(Object::Int(i64::from(old)))
}

fn msvcrt_locking(args: &[Object]) -> Result<Object, RuntimeError> {
    let fd = int_arg(args, 0, "locking")?;
    let mode = int_arg(args, 1, "locking")?;
    let nbytes = int_arg(args, 2, "locking")?;
    // `_locking(LK_LOCK)` retries once a second for ten seconds — a
    // blocking region, so drop the GIL like CPython does. Capture errno
    // inside the region: re-acquiring the GIL may run CRT calls that
    // clobber it.
    let (rc, errnum) = crate::gil::allow_threads_then(|| {
        let rc = unsafe { crt::_locking(fd, mode, nbytes) };
        (rc, if rc != 0 { nt_support::crt_errno() } else { 0 })
    });
    if rc != 0 {
        return Err(nt_support::crt_error_to_py(errnum, None));
    }
    Ok(Object::None)
}

fn msvcrt_heapmin(_args: &[Object]) -> Result<Object, RuntimeError> {
    if unsafe { crt::_heapmin() } != 0 {
        return Err(nt_support::last_crt_error_to_py(None));
    }
    Ok(Object::None)
}

// ---------------------------------------------------------------------------
// The conio console family.
// ---------------------------------------------------------------------------

fn msvcrt_kbhit(_args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(Object::Bool(unsafe { crt::_kbhit() } != 0))
}

fn msvcrt_getch(_args: &[Object]) -> Result<Object, RuntimeError> {
    // Blocks until a key is pressed — release the GIL (CPython does).
    let ch = crate::gil::allow_threads_then(|| unsafe { crt::_getch() });
    Ok(Object::new_bytes(vec![ch as u8]))
}

fn msvcrt_getche(_args: &[Object]) -> Result<Object, RuntimeError> {
    let ch = crate::gil::allow_threads_then(|| unsafe { crt::_getche() });
    Ok(Object::new_bytes(vec![ch as u8]))
}

fn msvcrt_getwch(_args: &[Object]) -> Result<Object, RuntimeError> {
    let wc = crate::gil::allow_threads_then(|| unsafe { crt::_getwch() });
    Ok(wchar_to_str(wc))
}

fn msvcrt_getwche(_args: &[Object]) -> Result<Object, RuntimeError> {
    let wc = crate::gil::allow_threads_then(|| unsafe { crt::_getwche() });
    Ok(wchar_to_str(wc))
}

fn msvcrt_putch(args: &[Object]) -> Result<Object, RuntimeError> {
    let ch = byte_char_arg(args, 0, "putch")?;
    unsafe { crt::_putch(i32::from(ch)) };
    Ok(Object::None)
}

fn msvcrt_putwch(args: &[Object]) -> Result<Object, RuntimeError> {
    let wc = wchar_arg(args, 0, "putwch")?;
    unsafe { crt::_putwch(wc) };
    Ok(Object::None)
}

fn msvcrt_ungetch(args: &[Object]) -> Result<Object, RuntimeError> {
    let ch = byte_char_arg(args, 0, "ungetch")?;
    // EOF (-1) signals the pushback slot is already occupied.
    if unsafe { crt::_ungetch(i32::from(ch)) } == -1 {
        return Err(nt_support::last_crt_error_to_py(None));
    }
    Ok(Object::None)
}

fn msvcrt_ungetwch(args: &[Object]) -> Result<Object, RuntimeError> {
    let wc = wchar_arg(args, 0, "ungetwch")?;
    // WEOF (0xFFFF) is the wide-char twin of EOF.
    if unsafe { crt::_ungetwch(wc) } == 0xFFFF {
        return Err(nt_support::last_crt_error_to_py(None));
    }
    Ok(Object::None)
}

// ---------------------------------------------------------------------------
// Error modes and CRT debug-report stubs.
// ---------------------------------------------------------------------------

fn msvcrt_set_error_mode(args: &[Object]) -> Result<Object, RuntimeError> {
    let mode = int_arg(args, 0, "SetErrorMode")? as u32;
    let old = unsafe { windows_sys::Win32::System::Diagnostics::Debug::SetErrorMode(mode) };
    Ok(Object::Int(i64::from(old)))
}

fn msvcrt_get_error_mode(_args: &[Object]) -> Result<Object, RuntimeError> {
    let mode = unsafe { windows_sys::Win32::System::Diagnostics::Debug::GetErrorMode() };
    Ok(Object::Int(i64::from(mode)))
}

/// `_CrtSetReportMode` exists only in the *debug* CRT (CPython compiles
/// the binding under `#ifdef _DEBUG`); WeavePy links the release UCRT,
/// so the call is accepted and reports "was 0" — enough for the
/// test-support code that toggles assertion popups around subprocesses.
fn msvcrt_crt_set_report_mode(args: &[Object]) -> Result<Object, RuntimeError> {
    let _type = int_arg(args, 0, "CrtSetReportMode")?;
    let _mode = int_arg(args, 1, "CrtSetReportMode")?;
    Ok(Object::Int(0))
}

/// Debug-CRT-only twin of [`msvcrt_crt_set_report_mode`].
fn msvcrt_crt_set_report_file(args: &[Object]) -> Result<Object, RuntimeError> {
    let _type = int_arg(args, 0, "CrtSetReportFile")?;
    Ok(Object::Int(0))
}

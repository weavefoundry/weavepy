//! `_WindowsConsoleIO` and the console byte bridge (RFC 0064 WS4).
//!
//! CPython's `Modules/_io/winconsoleio.c` exists because the Windows
//! console is not a byte stream: bytes written with `WriteFile` are
//! interpreted in the console *codepage* (usually not UTF-8), so
//! anything outside it mojibakes. The fix is to talk to the console in
//! UTF-16 — `ReadConsoleW`/`WriteConsoleW` — and present UTF-8 at the
//! Python-visible edge.
//!
//! Two consumers share the bridge here:
//!
//! 1. **`_io._WindowsConsoleIO`** — the raw-io type itself (reachable
//!    the CPython way, `_io._WindowsConsoleIO(fd_or_path, mode)`),
//!    registered by `io_full::build` on Windows only.
//! 2. **The native `PyFile` stdio monolith** — WeavePy's std streams
//!    are one native object, not CPython's three-layer stack (a
//!    documented RFC 0050/0053 divergence). `object.rs` routes
//!    `Stdin`/`Stdout`/`Stderr` backends through
//!    [`stdin_console_read`]/[`console_write`] when the fd is a real
//!    console, so interactive I/O round-trips the full Unicode range
//!    regardless of codepage — CPython-faithful *behavior* through
//!    WeavePy-shaped plumbing. Redirected/piped fds (everything CI
//!    sees) fail the `GetConsoleMode` probe and keep the RFC 0063
//!    byte paths untouched.

use std::ffi::c_void;
use std::sync::Mutex;

use crate::error::{type_error, value_error, RuntimeError};
use crate::object::{BuiltinFn, DictData, DictKey, Object, StrKey};
use crate::stdlib::nt_support::{crt, last_win32_error_to_py, win32_error_to_py};
use crate::sync::Rc;
use crate::sync::RefCell;
use crate::types::{PyInstance, TypeFlags, TypeObject};

use windows_sys::Win32::Foundation::{
    GetLastError, ERROR_OPERATION_ABORTED, GENERIC_READ, GENERIC_WRITE, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::System::Console::{
    GetConsoleMode, GetNumberOfConsoleInputEvents, ReadConsoleW, WriteConsoleW,
};

// ---------------------------------------------------------------------------
// Console detection.
// ---------------------------------------------------------------------------

/// The OS handle behind a CRT fd when that fd is a *real* console
/// (`GetConsoleMode` succeeds), `None` for anything redirected — the
/// probe CPython's `_PyIO_get_console_type` builds on. Checked per
/// operation, not cached: `os.dup2` can repoint a std fd at a file
/// mid-process and the byte path must follow it.
pub(crate) fn console_handle(fd: i32) -> Option<isize> {
    let handle = unsafe { crt::_get_osfhandle(fd) };
    if handle == -1 || handle == -2 {
        return None;
    }
    let mut mode = 0u32;
    (unsafe { GetConsoleMode(handle as *mut c_void, &raw mut mode) } != 0).then_some(handle)
}

/// Classify a console handle as input (`'r'`) or screen (`'w'`) —
/// CPython's trick: only input handles answer
/// `GetNumberOfConsoleInputEvents`.
fn console_kind(handle: isize) -> Option<char> {
    let mut mode = 0u32;
    if unsafe { GetConsoleMode(handle as *mut c_void, &raw mut mode) } == 0 {
        return None;
    }
    let mut events = 0u32;
    if unsafe { GetNumberOfConsoleInputEvents(handle as *mut c_void, &raw mut events) } != 0 {
        Some('r')
    } else {
        Some('w')
    }
}

/// The console type a *path* names (`_PyIO_get_console_type`):
/// `CONIN$` is input, `CONOUT$` is screen, `CON` is either (resolved
/// from the opened handle). Accepts the `\\.\` device prefix.
fn path_console_kind(path: &str) -> Option<char> {
    let leaf = path
        .strip_prefix("\\\\.\\")
        .or_else(|| path.strip_prefix("//./"))
        .unwrap_or(path);
    if leaf.eq_ignore_ascii_case("CONIN$") {
        Some('r')
    } else if leaf.eq_ignore_ascii_case("CONOUT$") {
        Some('w')
    } else if leaf.eq_ignore_ascii_case("CON") {
        Some('x')
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// The UTF-16 bridge: write.
// ---------------------------------------------------------------------------

/// CPython's per-call ceiling (winconsoleio.c `BUFMAX` rationale):
/// `WriteConsoleW` over ~32766 wchars can fail with not-enough-memory.
const WCHAR_CHUNK: usize = 32766;

/// Bytes of an incomplete UTF-8 sequence dangling at the end of `data`
/// (0 when the tail is complete). CPython's `_find_last_utf8_boundary`:
/// a `BufferedWriter` chunk may split a character, and the split tail
/// must stay unconsumed for the next write rather than mojibake.
fn trailing_incomplete_utf8(data: &[u8]) -> usize {
    let n = data.len();
    for back in 1..=n.min(3) {
        let b = data[n - back];
        if b & 0xC0 == 0x80 {
            continue; // continuation byte — keep scanning for the lead
        }
        let need = if b >= 0xF0 {
            4
        } else if b >= 0xE0 {
            3
        } else if b >= 0xC0 {
            2
        } else {
            1
        };
        return if need > back { back } else { 0 };
    }
    0
}

/// Write `data` to a console handle via `WriteConsoleW`, returning the
/// count of *bytes consumed*. Whole characters only: a trailing split
/// UTF-8 sequence is left for the caller's next write (unless the
/// buffer holds nothing else — then it is written with U+FFFD, exactly
/// what `MultiByteToWideChar` without `MB_ERR_INVALID_CHARS` does with
/// invalid bytes mid-buffer too).
pub(crate) fn write_console(handle: isize, data: &[u8]) -> Result<usize, RuntimeError> {
    if data.is_empty() {
        return Ok(0);
    }
    let mut consume = data.len() - trailing_incomplete_utf8(data);
    if consume == 0 {
        consume = data.len();
    }
    let text = String::from_utf8_lossy(&data[..consume]);
    let wide: Vec<u16> = text.encode_utf16().collect();
    let mut off = 0usize;
    while off < wide.len() {
        let chunk = (wide.len() - off).min(WCHAR_CHUNK);
        let mut written = 0u32;
        let ok = unsafe {
            WriteConsoleW(
                handle as *mut c_void,
                wide[off..].as_ptr().cast(),
                chunk as u32,
                &raw mut written,
                std::ptr::null(),
            )
        };
        if ok == 0 {
            return Err(win32_error_to_py(unsafe { GetLastError() } as i32, None));
        }
        if written == 0 {
            break;
        }
        off += written as usize;
    }
    Ok(consume)
}

/// Route a `PyFile` `Stdout`/`Stderr` write through the console bridge
/// when the fd is a real console; `None` keeps the ordinary sink path.
pub(crate) fn console_write(fd: i32, data: &[u8]) -> Option<Result<usize, RuntimeError>> {
    console_handle(fd).map(|handle| write_console(handle, data))
}

// ---------------------------------------------------------------------------
// The UTF-16 bridge: read.
// ---------------------------------------------------------------------------

/// wchars per `ReadConsoleW` request for unbounded reads.
const READ_WCHARS: usize = 8192;

/// Run any tripped Python signal handler on the main thread (the
/// `os`/`socket` blocking-call pattern): a `ReadConsoleW` aborted by
/// Ctrl-C surfaces the handler's `KeyboardInterrupt` here.
fn service_pending_signals() -> Result<(), RuntimeError> {
    if !crate::gil::is_main_thread() || !crate::stdlib::signal_mod::signals_pending() {
        return Ok(());
    }
    if let Some(ptr) = crate::vm_singletons::current_interpreter_ptr() {
        // SAFETY: published by the active builtin call on this (main)
        // thread; the interpreter outlives this synchronous call.
        let interp = unsafe { &mut *ptr };
        interp.run_pending_signals_public()?;
    }
    Ok(())
}

/// One `ReadConsoleW` request, transcoded to UTF-8. Empty means EOF
/// (Ctrl-Z at the start of the read, winconsoleio.c) or a Ctrl-C whose
/// handler chose not to raise. A Ctrl-C aborting the read runs the
/// Python handler after CPython's 100ms grace sleep — the default
/// SIGINT handler raises `KeyboardInterrupt` out of here.
fn read_chunk(handle: isize, nwchars: usize) -> Result<Vec<u8>, RuntimeError> {
    let mut wbuf = vec![0u16; nwchars.clamp(1, READ_WCHARS)];
    let mut read = 0u32;
    let ok = unsafe {
        ReadConsoleW(
            handle as *mut c_void,
            wbuf.as_mut_ptr().cast(),
            wbuf.len() as u32,
            &raw mut read,
            std::ptr::null(),
        )
    };
    if ok == 0 {
        let err = unsafe { GetLastError() };
        if err == ERROR_OPERATION_ABORTED {
            std::thread::sleep(std::time::Duration::from_millis(100));
            service_pending_signals()?;
            return Ok(Vec::new());
        }
        return Err(win32_error_to_py(err as i32, None));
    }
    let wchars = &wbuf[..read as usize];
    if wchars.first() == Some(&0x1a) {
        return Ok(Vec::new()); // Ctrl-Z: EOF
    }
    Ok(String::from_utf16_lossy(wchars).into_bytes())
}

/// Read console bytes with a caller-owned carry buffer (`pending`
/// holds UTF-8 spill: one wchar can decode to more bytes than the
/// caller asked for). `None` reads to EOF (Ctrl-Z); `Some(n)` returns
/// up to `n` bytes, stopping at a completed line — the console cooks
/// input per line, and blocking past Enter would hang `read(n)` on an
/// interactive prompt.
pub(crate) fn read_console(
    handle: isize,
    pending: &mut Vec<u8>,
    n: Option<usize>,
) -> Result<Vec<u8>, RuntimeError> {
    match n {
        Some(n) => {
            while pending.len() < n {
                let want = n - pending.len();
                let chunk = read_chunk(handle, want)?;
                if chunk.is_empty() {
                    break;
                }
                pending.extend_from_slice(&chunk);
                if pending.ends_with(b"\n") {
                    break;
                }
            }
            let take = n.min(pending.len());
            Ok(pending.drain(..take).collect())
        }
        None => {
            loop {
                let chunk = read_chunk(handle, READ_WCHARS)?;
                if chunk.is_empty() {
                    break;
                }
                pending.extend_from_slice(&chunk);
            }
            Ok(std::mem::take(pending))
        }
    }
}

/// Carry buffer for the `PyFile` `Stdin` bridge (fd 0 is a singleton;
/// the spill must survive across `read(1)` probes from
/// `readline_unbounded`).
static STDIN_PENDING: Mutex<Vec<u8>> = Mutex::new(Vec::new());

/// Route a `PyFile` `Stdin` read through the console bridge when fd 0
/// is a real console; `None` keeps the ordinary byte path.
pub(crate) fn stdin_console_read(n: Option<usize>) -> Option<Result<Vec<u8>, RuntimeError>> {
    let handle = console_handle(0)?;
    let mut pending = STDIN_PENDING.lock().unwrap_or_else(|e| e.into_inner());
    Some(read_console(handle, &mut pending, n))
}

// ---------------------------------------------------------------------------
// The `_io._WindowsConsoleIO` type.
// ---------------------------------------------------------------------------

fn wcio_self(args: &[Object]) -> Result<Rc<PyInstance>, RuntimeError> {
    match args.first() {
        Some(Object::Instance(i)) => Ok(i.clone()),
        _ => Err(type_error(
            "unbound method _WindowsConsoleIO requires a _WindowsConsoleIO instance",
        )),
    }
}

fn wcio_get(inst: &PyInstance, name: &str) -> Option<Object> {
    inst.dict.borrow().get(&StrKey(name)).cloned()
}

fn wcio_set(inst: &PyInstance, name: &'static str, value: Object) {
    inst.dict
        .borrow_mut()
        .insert(DictKey(Object::from_static(name)), value);
}

fn wcio_fd(inst: &PyInstance) -> i64 {
    wcio_get(inst, "_fd").and_then(|o| o.as_i64()).unwrap_or(-1)
}

fn wcio_flag(inst: &PyInstance, name: &str) -> bool {
    matches!(wcio_get(inst, name), Some(Object::Bool(true)))
}

fn wcio_check_open(inst: &PyInstance) -> Result<i32, RuntimeError> {
    let fd = wcio_fd(inst);
    if fd < 0 {
        return Err(value_error("I/O operation on closed file."));
    }
    Ok(fd as i32)
}

fn wcio_console_handle_checked(inst: &PyInstance) -> Result<isize, RuntimeError> {
    let fd = wcio_check_open(inst)?;
    console_handle(fd).ok_or_else(|| value_error("Cannot open non-console file"))
}

/// `__init__(file, mode='r', closefd=True, opener=None)` — CPython's
/// `_io__WindowsConsoleIO___init___impl`.
fn wcio_init(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let inst = wcio_self(args)?;
    let positional = &args[1..];
    let kw = |name: &str| {
        kwargs
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.clone())
    };
    let file = positional
        .first()
        .cloned()
        .or_else(|| kw("file"))
        .ok_or_else(|| {
            type_error("_WindowsConsoleIO() missing required argument 'file' (pos 1)")
        })?;
    let mode = match positional.get(1).cloned().or_else(|| kw("mode")) {
        Some(Object::Str(s)) => s.to_string(),
        None => "r".to_owned(),
        Some(other) => {
            return Err(type_error(format!(
                "argument 2 must be str, not {}",
                other.type_name()
            )))
        }
    };
    let closefd = match positional.get(2).cloned().or_else(|| kw("closefd")) {
        Some(v) => v.is_truthy(),
        None => true,
    };
    let opener = positional.get(3).cloned().or_else(|| kw("opener"));

    // Mode chars: 'b' is a no-op, exactly one of 'r'/'w' picks the
    // direction, anything else is CPython's ValueError.
    let mut readable = false;
    let mut writable = false;
    for c in mode.chars() {
        match c {
            'b' => {}
            'r' => readable = true,
            'w' | 'a' | 'x' => writable = true,
            _ => return Err(value_error(format!("invalid mode: {mode}"))),
        }
    }
    if readable == writable {
        return Err(value_error("Console buffer must be readable or writable"));
    }
    let wanted = if readable { 'r' } else { 'w' };

    crate::stdlib::sys::audit_event(
        "open",
        &[
            file.clone(),
            Object::from_str(mode.clone()),
            Object::Int(i64::from(closefd)),
        ],
    )?;

    let (fd, kind) = match &file {
        Object::Int(fd) => {
            let fd = *fd as i32;
            let handle = unsafe { crt::_get_osfhandle(fd) };
            if handle == -1 || handle == -2 {
                return Err(crate::stdlib::nt_support::crt_error_to_py(
                    crate::py_errno::EBADF,
                    None,
                ));
            }
            let kind =
                console_kind(handle).ok_or_else(|| value_error("Cannot open non-console file"))?;
            (fd, kind)
        }
        Object::Str(path) => {
            if !closefd {
                return Err(value_error("Cannot use closefd=False with file name"));
            }
            let path = path.to_string();
            let named_kind = path_console_kind(&path);
            let fd = match &opener {
                Some(op) if !matches!(op, Object::None) => {
                    // CPython honors a custom opener even here; it must
                    // return a console fd (validated below).
                    let ptr = crate::vm_singletons::current_interpreter_ptr()
                        .ok_or_else(|| value_error("no running interpreter for opener call"))?;
                    // SAFETY: published by the enclosing VM frame on this thread.
                    let interp = unsafe { &mut *ptr };
                    let flags = if readable {
                        crt::O_RDONLY | crt::O_BINARY
                    } else {
                        crt::O_WRONLY | crt::O_BINARY
                    };
                    let result = interp.call_object(
                        op.clone(),
                        &[file.clone(), Object::Int(i64::from(flags))],
                        &[],
                    )?;
                    match result.as_i64() {
                        Some(fd) if fd >= 0 => fd as i32,
                        Some(_) => {
                            return Err(value_error("opener returned a negative file descriptor"))
                        }
                        None => return Err(type_error("expected integer from opener")),
                    }
                }
                _ => {
                    use windows_sys::Win32::Storage::FileSystem::{
                        CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
                    };
                    let wpath = crate::stdlib::nt_support::wide(&path);
                    // CPython opens read+write first (a console screen
                    // buffer wants both for mode probing), falling back
                    // to the mode's own access.
                    let mut handle = unsafe {
                        CreateFileW(
                            wpath.as_ptr(),
                            GENERIC_READ | GENERIC_WRITE,
                            FILE_SHARE_READ | FILE_SHARE_WRITE,
                            std::ptr::null(),
                            OPEN_EXISTING,
                            0,
                            std::ptr::null_mut(),
                        )
                    };
                    if handle == INVALID_HANDLE_VALUE {
                        let access = if readable {
                            GENERIC_READ
                        } else {
                            GENERIC_WRITE
                        };
                        handle = unsafe {
                            CreateFileW(
                                wpath.as_ptr(),
                                access,
                                FILE_SHARE_READ | FILE_SHARE_WRITE,
                                std::ptr::null(),
                                OPEN_EXISTING,
                                0,
                                std::ptr::null_mut(),
                            )
                        };
                    }
                    if handle == INVALID_HANDLE_VALUE {
                        return Err(last_win32_error_to_py(Some(&path)));
                    }
                    let fd =
                        unsafe { crt::_open_osfhandle(handle as crt::intptr_t, crt::O_BINARY) };
                    if fd < 0 {
                        unsafe {
                            windows_sys::Win32::Foundation::CloseHandle(handle);
                        }
                        return Err(crate::stdlib::nt_support::last_crt_error_to_py(Some(&path)));
                    }
                    fd
                }
            };
            let handle = unsafe { crt::_get_osfhandle(fd) };
            let kind = named_kind
                .filter(|k| *k != 'x')
                .or_else(|| console_kind(handle));
            let Some(kind) = kind else {
                if closefd {
                    unsafe {
                        crt::_close(fd);
                    }
                }
                return Err(value_error("Cannot open non-console file"));
            };
            (fd, kind)
        }
        other => {
            return Err(type_error(format!(
                "expected int or str, not {}",
                other.type_name(),
            )))
        }
    };

    // Direction mismatch is CPython's exact pair of messages.
    if kind == 'r' && wanted == 'w' {
        return Err(value_error("Cannot open console input buffer for writing"));
    }
    if kind == 'w' && wanted == 'r' {
        return Err(value_error("Cannot open console output buffer for reading"));
    }

    wcio_set(&inst, "_fd", Object::Int(i64::from(fd)));
    wcio_set(&inst, "_readable", Object::Bool(readable));
    wcio_set(&inst, "_writable", Object::Bool(writable));
    wcio_set(&inst, "_closefd", Object::Bool(closefd));
    wcio_set(&inst, "name", file);
    wcio_set(&inst, "_pending", Object::new_bytes(Vec::new()));
    Ok(Object::None)
}

fn wcio_close(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = wcio_self(args)?;
    let fd = wcio_fd(inst.as_ref());
    if fd >= 0 {
        if wcio_flag(&inst, "_closefd") {
            // CPython's internal_close ignores the CRT result too.
            unsafe {
                crt::_close(fd as i32);
            }
        }
        wcio_set(&inst, "_fd", Object::Int(-1));
    }
    Ok(Object::None)
}

fn wcio_pending(inst: &PyInstance) -> Vec<u8> {
    wcio_get(inst, "_pending")
        .and_then(|o| o.as_bytes_view())
        .unwrap_or_default()
}

fn wcio_read_impl(inst: &Rc<PyInstance>, n: Option<usize>) -> Result<Object, RuntimeError> {
    if !wcio_flag(inst, "_readable") {
        return Err(crate::stdlib::io::unsupported_op(
            "File not open for reading",
        ));
    }
    let handle = wcio_console_handle_checked(inst)?;
    let mut pending = wcio_pending(inst);
    let result = read_console(handle, &mut pending, n);
    wcio_set(inst, "_pending", Object::new_bytes(pending));
    Ok(Object::new_bytes(result?))
}

fn wcio_read(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = wcio_self(args)?;
    let n = match args.get(1) {
        None | Some(Object::None) => None,
        Some(v) => match v.as_i64() {
            Some(n) if n < 0 => None,
            Some(n) => Some(n as usize),
            None => return Err(type_error("argument should be integer or None")),
        },
    };
    wcio_read_impl(&inst, n)
}

fn wcio_readall(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = wcio_self(args)?;
    wcio_read_impl(&inst, None)
}

fn wcio_readinto(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = wcio_self(args)?;
    let (dst, start, cap) = crate::stdlib::io::readinto_writable_buffer(args.get(1))?;
    let data = wcio_read_impl(&inst, Some(cap))?;
    let bytes = data.as_bytes_view().expect("read returns bytes");
    let n = bytes.len().min(cap);
    dst.borrow_mut()[start..start + n].copy_from_slice(&bytes[..n]);
    Ok(Object::Int(n as i64))
}

fn wcio_write(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = wcio_self(args)?;
    if !wcio_flag(&inst, "_writable") {
        return Err(crate::stdlib::io::unsupported_op(
            "File not open for writing",
        ));
    }
    let handle = wcio_console_handle_checked(&inst)?;
    let data = args.get(1).and_then(|o| o.as_bytes_view()).ok_or_else(|| {
        type_error(format!(
            "a bytes-like object is required, not '{}'",
            args.get(1).map_or("NoneType", |o| o.type_name())
        ))
    })?;
    let n = write_console(handle, &data)?;
    Ok(Object::Int(n as i64))
}

/// Build the `_io._WindowsConsoleIO` type (memoised — one identity per
/// process, like the rest of the `_io` family). Bases on the shared
/// `RawIOBase` so the IOBase mixins (`__enter__`, `readline`, …) come
/// along, exactly as CPython's type inherits them.
pub(crate) fn windows_console_io_type() -> Rc<TypeObject> {
    use crate::object::MethodWrapper;
    thread_local! {
        static CLS: RefCell<Option<Rc<TypeObject>>> = const { RefCell::new(None) };
    }
    CLS.with(|slot| {
        if let Some(c) = slot.borrow().as_ref() {
            return c.clone();
        }
        let raw_base = crate::stdlib::io::build_iobase_family().raw.clone();
        let mut dict = DictData::default();
        let mut method = |n: &'static str, body: fn(&[Object]) -> Result<Object, RuntimeError>| {
            dict.insert(
                DictKey(Object::from_static(n)),
                Object::Builtin(Rc::new(BuiltinFn {
                    name: n,
                    binds_instance: true,
                    call: Box::new(body),
                    call_kw: None,
                })),
            );
        };
        method("read", wcio_read);
        method("readall", wcio_readall);
        method("readinto", wcio_readinto);
        method("write", wcio_write);
        method("close", wcio_close);
        method("fileno", |args| {
            let inst = wcio_self(args)?;
            Ok(Object::Int(i64::from(wcio_check_open(&inst)?)))
        });
        method("isatty", |args| {
            let inst = wcio_self(args)?;
            wcio_check_open(&inst)?;
            Ok(Object::Bool(true))
        });
        method("readable", |args| {
            let inst = wcio_self(args)?;
            wcio_check_open(&inst)?;
            Ok(Object::Bool(wcio_flag(&inst, "_readable")))
        });
        method("writable", |args| {
            let inst = wcio_self(args)?;
            wcio_check_open(&inst)?;
            Ok(Object::Bool(wcio_flag(&inst, "_writable")))
        });
        method("seekable", |args| {
            let inst = wcio_self(args)?;
            wcio_check_open(&inst)?;
            Ok(Object::Bool(false))
        });
        method("flush", |args| {
            let inst = wcio_self(args)?;
            wcio_check_open(&inst)?;
            Ok(Object::None)
        });
        method("__repr__", |args| {
            let inst = wcio_self(args)?;
            let mode = if wcio_flag(&inst, "_readable") {
                "rb"
            } else {
                "wb"
            };
            Ok(Object::from_str(format!(
                "<_io._WindowsConsoleIO mode='{mode}' closefd={}>",
                if wcio_flag(&inst, "_closefd") {
                    "True"
                } else {
                    "False"
                }
            )))
        });
        dict.insert(
            DictKey(Object::from_static("__new__")),
            Object::StaticMethod(MethodWrapper::new(Object::Builtin(Rc::new(BuiltinFn {
                name: "__new__",
                binds_instance: false,
                call: Box::new(wcio_new),
                call_kw: Some(Box::new(|a, _kw| wcio_new(a))),
            })))),
        );
        dict.insert(
            DictKey(Object::from_static("__init__")),
            Object::Builtin(Rc::new(BuiltinFn {
                name: "__init__",
                binds_instance: true,
                call: Box::new(|a| wcio_init(a, &[])),
                call_kw: Some(Box::new(wcio_init)),
            })),
        );
        let ty = TypeObject::new_with_flags(
            "_WindowsConsoleIO",
            vec![raw_base],
            dict,
            TypeFlags {
                is_exception: false,
                is_builtin: true,
            },
        )
        .expect("_WindowsConsoleIO type");
        // `closed` and `mode` are getset descriptors on the C type.
        let getset = |name: &'static str,
                      body: fn(&[Object]) -> Result<Object, RuntimeError>,
                      doc: &'static str| {
            let prop = Object::Property(Rc::new(crate::object::PyProperty::new(
                Object::Builtin(Rc::new(BuiltinFn {
                    name,
                    binds_instance: true,
                    call: Box::new(body),
                    call_kw: None,
                })),
                Object::None,
                Object::None,
                Object::from_static(doc),
            )));
            crate::descr_registry::register(
                &prop,
                crate::descr_registry::DescrKind::GetSet,
                ty.clone(),
                name,
                None,
            );
            ty.dict
                .borrow_mut()
                .insert(DictKey(Object::from_static(name)), prop);
        };
        getset(
            "closed",
            |args| {
                let inst = wcio_self(args)?;
                Ok(Object::Bool(wcio_fd(&inst) < 0))
            },
            "True if the file is closed",
        );
        getset(
            "mode",
            |args| {
                let inst = wcio_self(args)?;
                Ok(Object::from_static(if wcio_flag(&inst, "_readable") {
                    "rb"
                } else {
                    "wb"
                }))
            },
            "String giving the file mode",
        );
        crate::stdlib::io::set_type_module(&ty, "_io");
        *slot.borrow_mut() = Some(ty.clone());
        ty
    })
}

fn wcio_new(args: &[Object]) -> Result<Object, RuntimeError> {
    let cls = match args.first() {
        Some(Object::Type(t)) => t.clone(),
        _ => {
            return Err(type_error(
                "_WindowsConsoleIO.__new__(X): X is not a type object",
            ))
        }
    };
    let inst = Object::Instance(Rc::new(PyInstance::new(cls)));
    crate::gc_trace::track(inst.clone());
    Ok(inst)
}

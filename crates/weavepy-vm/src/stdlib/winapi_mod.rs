//! The `_winapi` built-in module (RFC 0063 WS2).
//!
//! The private Win32 surface `subprocess`, `multiprocessing`, and
//! `shutil` consume, transcribed from CPython 3.13's
//! `Modules/_winapi.c`. Names, argument order, and return *shapes*
//! match CPython exactly so the frozen Windows stdlib drives this
//! module unchanged: handles are plain Python ints (CPython's `_winapi`
//! only exposes the `Overlapped` helper type — `subprocess.py` supplies
//! its own `Handle(int)` subclass), `CreatePipe` returns
//! `(read, write)`, `CreateProcess` returns `(hp, ht, pid, tid)`, and
//! the overlapped I/O functions return `(Overlapped, err)`.
//!
//! Error handling follows CPython: a failed Win32 call raises the
//! `winerror`-truthful `OSError`
//! ([`nt_support::last_win32_error_to_py`], which fills
//! `.winerror`/`.errno`/`.strerror`). Every blocking wait
//! (`WaitFor*`, `ConnectNamedPipe`, sync `ReadFile`/`WriteFile`)
//! releases the GIL through [`crate::gil::allow_threads_then`], exactly
//! as CPython wraps them in `Py_BEGIN_ALLOW_THREADS`.
//!
//! Handles are represented unsigned (CPython's `HANDLE_TO_PYNUM` is
//! `PyLong_FromVoidPtr`), so `INVALID_HANDLE_VALUE` is the unsigned
//! `(uintptr_t)-1`, not `-1`.

use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Mutex;

use num_traits::ToPrimitive;

use crate::error::{type_error, value_error, RuntimeError};
use crate::import::ModuleCache;
use crate::object::{BuiltinFn, DictData, DictKey, Object, PyModule};
use crate::stdlib::nt_support::{self, wide};
use crate::sync::Rc;
use crate::sync::RefCell;

use windows_sys::Win32::Foundation as fnd;
use windows_sys::Win32::Globalization as glob;
use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
use windows_sys::Win32::Storage::FileSystem as fs;
use windows_sys::Win32::System::Console as con;
use windows_sys::Win32::System::LibraryLoader as libl;
use windows_sys::Win32::System::Memory as mem;
use windows_sys::Win32::System::Pipes as pipes;
use windows_sys::Win32::System::Threading as thr;
use windows_sys::Win32::System::IO as wio;

use fnd::HANDLE;

// Win32 error codes the control flow branches on (winerror.h). The
// module also *publishes* these plus many more (see `constants`).
const ERROR_SUCCESS: u32 = 0;
const ERROR_BROKEN_PIPE: u32 = 109;
const ERROR_MORE_DATA: u32 = 234;
const ERROR_IO_INCOMPLETE: u32 = 996;
const ERROR_IO_PENDING: u32 = 997;
const ERROR_OPERATION_ABORTED: u32 = 995;
const ERROR_NOT_FOUND: u32 = 1168;

const INFINITE: u32 = 0xFFFF_FFFF;

pub fn build(_cache: &ModuleCache) -> Rc<PyModule> {
    let dict = Rc::new(RefCell::new(DictData::default()));
    {
        let mut d = dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("__name__")),
            Object::from_static("_winapi"),
        );
        d.insert(DictKey(Object::from_static("__doc__")), Object::None);

        // Every `_winapi` function is registered keyword-capable: CPython
        // clinic-generates keyword support for the whole surface, and the
        // frozen stdlib calls several with keywords (`overlapped=True`,
        // `milliseconds=...`).
        let mut reg =
            |name: &'static str,
             body: fn(&[Object], &[(String, Object)]) -> Result<Object, RuntimeError>| {
                d.insert(
                    DictKey(Object::from_static(name)),
                    crate::stdlib::os::builtin_kw(name, body),
                );
            };
        reg("CloseHandle", win_close_handle);
        reg("GetLastError", win_get_last_error);
        reg("GetACP", win_get_acp);
        reg("GetVersion", win_get_version);
        reg("GetCurrentProcess", win_get_current_process);
        reg("GetExitCodeProcess", win_get_exit_code_process);
        reg("GetFileType", win_get_file_type);
        reg("GetModuleFileName", win_get_module_file_name);
        reg("GetStdHandle", win_get_std_handle);
        reg("ExitProcess", win_exit_process);
        reg("TerminateProcess", win_terminate_process);
        reg("OpenProcess", win_open_process);
        reg("DuplicateHandle", win_duplicate_handle);
        reg("WaitForSingleObject", win_wait_for_single_object);
        reg("WaitForMultipleObjects", win_wait_for_multiple_objects);
        reg("CreateEventW", win_create_event);
        reg("OpenEventW", win_open_event);
        reg("SetEvent", win_set_event);
        reg("ResetEvent", win_reset_event);
        reg("CreateMutexW", win_create_mutex);
        reg("OpenMutexW", win_open_mutex);
        reg("ReleaseMutex", win_release_mutex);
        reg("CreatePipe", win_create_pipe);
        reg("CreateNamedPipe", win_create_named_pipe);
        reg("ConnectNamedPipe", win_connect_named_pipe);
        reg("WaitNamedPipe", win_wait_named_pipe);
        reg("PeekNamedPipe", win_peek_named_pipe);
        reg("SetNamedPipeHandleState", win_set_named_pipe_handle_state);
        reg("CreateFile", win_create_file);
        reg("ReadFile", win_read_file);
        reg("WriteFile", win_write_file);
        reg("CreateFileMapping", win_create_file_mapping);
        reg("OpenFileMapping", win_open_file_mapping);
        reg("MapViewOfFile", win_map_view_of_file);
        reg("UnmapViewOfFile", win_unmap_view_of_file);
        reg("VirtualQuerySize", win_virtual_query_size);
        reg("CreateProcess", win_create_process);
        reg("CreateJunction", win_create_junction);
        reg("NeedCurrentDirectoryForExePath", win_need_cwd_for_exe_path);
        reg("CopyFile2", win_copy_file2);
        reg("LCMapStringEx", win_lcmapstring_ex);

        for (name, val) in constants() {
            d.insert(DictKey(Object::from_static(name)), Object::Int(val));
        }
        // INVALID_HANDLE_VALUE is `(uintptr_t)-1` — an unsigned int too
        // large for `i64`; publish it through the unsigned handle path.
        d.insert(
            DictKey(Object::from_static("INVALID_HANDLE_VALUE")),
            handle_to_object(usize::MAX),
        );
        // `LOCALE_NAME_*` for LCMapStringEx: invariant is the empty
        // string, user-default is `None`, system-default the magic name.
        d.insert(
            DictKey(Object::from_static("LOCALE_NAME_INVARIANT")),
            Object::from_static(""),
        );
        d.insert(
            DictKey(Object::from_static("LOCALE_NAME_SYSTEM_DEFAULT")),
            Object::from_static("!x-sys-default-locale"),
        );
        d.insert(
            DictKey(Object::from_static("LOCALE_NAME_USER_DEFAULT")),
            Object::None,
        );
    }
    Rc::new(PyModule {
        name: "_winapi".to_owned(),
        filename: None,
        dict,
    })
}

// ---------------------------------------------------------------------------
// Handle / argument marshalling.
// ---------------------------------------------------------------------------

/// A HANDLE (or any pointer-sized value) as a Python int. CPython's
/// `HANDLE_TO_PYNUM` is `PyLong_FromVoidPtr`, i.e. *unsigned*, so a
/// handle with the high bit set does not surface negative.
pub(crate) fn handle_to_object(v: usize) -> Object {
    Object::int_from_i128(v as i128)
}

/// The unsigned pointer bit-pattern of an integer argument. Accepts the
/// arbitrary-precision arc so `INVALID_HANDLE_VALUE` (round-tripped as a
/// large `Long`) parses back to the same bits.
fn obj_to_usize(o: &Object) -> Option<usize> {
    let bits: u64 = match o {
        Object::Bool(b) => u64::from(*b),
        Object::Int(i) => *i as u64,
        Object::Long(b) => b.to_u64().or_else(|| b.to_i64().map(|v| v as u64))?,
        _ => return None,
    };
    Some(bits as usize)
}

/// Parse positional argument `idx` as a HANDLE.
pub(crate) fn handle_arg(args: &[Object], idx: usize, func: &str) -> Result<HANDLE, RuntimeError> {
    args.get(idx)
        .and_then(obj_to_usize)
        .map(|v| v as HANDLE)
        .ok_or_else(|| type_error(format!("{func}: argument {} must be a handle", idx + 1)))
}

fn int_arg(o: Option<&Object>, func: &str, which: &str) -> Result<i64, RuntimeError> {
    o.and_then(Object::as_i64)
        .ok_or_else(|| type_error(format!("{func}: {which} must be an int")))
}

/// Fetch an argument by position, falling back to a keyword of the same
/// clinic name. CPython accepts both forms for every parameter.
fn pick<'a>(
    args: &'a [Object],
    kw: &'a [(String, Object)],
    pos: usize,
    name: &str,
) -> Option<&'a Object> {
    args.get(pos)
        .or_else(|| kw.iter().find(|(k, _)| k == name).map(|(_, v)| v))
}

/// A `str`/`WStr` argument decoded to a Rust `String` (surrogate code
/// points pass through lossily — the wide re-encode is exact for the
/// BMP-only paths these functions take).
fn str_arg(o: Option<&Object>, func: &str, which: &str) -> Result<String, RuntimeError> {
    match o {
        Some(Object::Str(s)) => Ok(s.to_string()),
        Some(Object::WStr(cps)) => Ok(String::from_utf16_lossy(
            &cps.iter().map(|&c| c as u16).collect::<Vec<_>>(),
        )),
        _ => Err(type_error(format!("{func}: {which} must be a str"))),
    }
}

/// `None`/absent → `NULL` path for an optional wide-string argument.
fn opt_str_arg(
    o: Option<&Object>,
    func: &str,
    which: &str,
) -> Result<Option<String>, RuntimeError> {
    match o {
        None | Some(Object::None) => Ok(None),
        _ => str_arg(o, func, which).map(Some),
    }
}

/// A `SECURITY_ATTRIBUTES*` from an argument. CPython's `subprocess`/
/// `multiprocessing` always pass `None` here (they arrange inheritance
/// through handle-inheritance flags, not a descriptor), so `None` → NULL
/// covers the live callers; an integer is honoured as a raw pointer for
/// parity with CPython's converter.
fn sec_attr_ptr(o: Option<&Object>) -> *const SECURITY_ATTRIBUTES {
    match o {
        Some(o) if !matches!(o, Object::None) => {
            obj_to_usize(o).map_or(std::ptr::null(), |v| v as *const SECURITY_ATTRIBUTES)
        }
        _ => std::ptr::null(),
    }
}

fn extract_handle_seq(o: &Object) -> Option<Vec<HANDLE>> {
    let items: Vec<Object> = match o {
        Object::Tuple(t) => t.to_vec(),
        Object::List(l) => l.borrow().clone(),
        _ => return None,
    };
    items
        .iter()
        .map(obj_to_usize)
        .map(|v| v.map(|h| h as HANDLE))
        .collect()
}

// ---------------------------------------------------------------------------
// Misc / info functions.
// ---------------------------------------------------------------------------

fn win_close_handle(args: &[Object], _kw: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let h = handle_arg(args, 0, "CloseHandle")?;
    if unsafe { fnd::CloseHandle(h) } == 0 {
        return Err(nt_support::last_win32_error_to_py(None));
    }
    Ok(Object::None)
}

fn win_get_last_error(_args: &[Object], _kw: &[(String, Object)]) -> Result<Object, RuntimeError> {
    Ok(Object::Int(i64::from(unsafe { fnd::GetLastError() })))
}

fn win_get_acp(_args: &[Object], _kw: &[(String, Object)]) -> Result<Object, RuntimeError> {
    Ok(Object::Int(i64::from(unsafe { glob::GetACP() })))
}

fn win_get_version(_args: &[Object], _kw: &[(String, Object)]) -> Result<Object, RuntimeError> {
    // GetVersion is deprecated but still what CPython's `_winapi.GetVersion`
    // (and `sys.getwindowsversion`'s fast path) returns.
    Ok(Object::Int(i64::from(unsafe {
        windows_sys::Win32::System::SystemInformation::GetVersion()
    })))
}

fn win_get_current_process(
    _args: &[Object],
    _kw: &[(String, Object)],
) -> Result<Object, RuntimeError> {
    Ok(handle_to_object(
        unsafe { thr::GetCurrentProcess() } as usize
    ))
}

fn win_get_exit_code_process(
    args: &[Object],
    _kw: &[(String, Object)],
) -> Result<Object, RuntimeError> {
    let h = handle_arg(args, 0, "GetExitCodeProcess")?;
    let mut code: u32 = 0;
    if unsafe { thr::GetExitCodeProcess(h, &raw mut code) } == 0 {
        return Err(nt_support::last_win32_error_to_py(None));
    }
    Ok(Object::Int(i64::from(code)))
}

fn win_get_file_type(args: &[Object], _kw: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let h = handle_arg(args, 0, "GetFileType")?;
    // GetFileType returns FILE_TYPE_UNKNOWN both for a genuine unknown
    // type and for failure; CPython disambiguates via GetLastError.
    let ty = unsafe { fs::GetFileType(h) };
    if ty == 0 {
        let err = unsafe { fnd::GetLastError() };
        if err != ERROR_SUCCESS {
            return Err(nt_support::last_win32_error_to_py(None));
        }
    }
    Ok(Object::Int(i64::from(ty)))
}

fn win_get_module_file_name(
    args: &[Object],
    _kw: &[(String, Object)],
) -> Result<Object, RuntimeError> {
    let module = handle_arg(args, 0, "GetModuleFileName")?;
    let mut buf = vec![0u16; 260];
    loop {
        let n = unsafe {
            libl::GetModuleFileNameW(module as fnd::HMODULE, buf.as_mut_ptr(), buf.len() as u32)
        };
        if n == 0 {
            return Err(nt_support::last_win32_error_to_py(None));
        }
        // A return equal to the buffer size means truncation (the string
        // was at least that long); grow and retry.
        if (n as usize) < buf.len() {
            return Ok(Object::from_str(nt_support::from_wide(&buf[..n as usize])));
        }
        buf.resize(buf.len() * 2, 0);
    }
}

fn win_get_std_handle(args: &[Object], _kw: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let n = int_arg(args.first(), "GetStdHandle", "std_handle")? as u32;
    let h = unsafe { con::GetStdHandle(n) };
    if h == fnd::INVALID_HANDLE_VALUE {
        return Err(nt_support::last_win32_error_to_py(None));
    }
    Ok(handle_to_object(h as usize))
}

fn win_exit_process(args: &[Object], _kw: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let code = int_arg(args.first(), "ExitProcess", "exit_code")? as u32;
    // Diverges (`-> !`); coerces to the Result return type.
    unsafe { thr::ExitProcess(code) }
}

fn win_terminate_process(
    args: &[Object],
    _kw: &[(String, Object)],
) -> Result<Object, RuntimeError> {
    let h = handle_arg(args, 0, "TerminateProcess")?;
    let code = int_arg(args.get(1), "TerminateProcess", "exit_code")? as u32;
    if unsafe { thr::TerminateProcess(h, code) } == 0 {
        return Err(nt_support::last_win32_error_to_py(None));
    }
    Ok(Object::None)
}

fn win_open_process(args: &[Object], _kw: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let access = int_arg(args.first(), "OpenProcess", "desired_access")? as u32;
    let inherit = args.get(1).is_some_and(Object::is_truthy);
    let pid = int_arg(args.get(2), "OpenProcess", "process_id")? as u32;
    let h = unsafe { thr::OpenProcess(access, i32::from(inherit), pid) };
    if h.is_null() {
        return Err(nt_support::last_win32_error_to_py(None));
    }
    Ok(handle_to_object(h as usize))
}

fn win_duplicate_handle(args: &[Object], kw: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let src_proc = handle_arg(args, 0, "DuplicateHandle")?;
    let src = handle_arg(args, 1, "DuplicateHandle")?;
    let tgt_proc = handle_arg(args, 2, "DuplicateHandle")?;
    let access = int_arg(
        pick(args, kw, 3, "desired_access"),
        "DuplicateHandle",
        "desired_access",
    )? as u32;
    let inherit = pick(args, kw, 4, "inherit_handle").is_some_and(Object::is_truthy);
    let options = pick(args, kw, 5, "options")
        .and_then(Object::as_i64)
        .unwrap_or(0) as u32;
    let mut target: HANDLE = std::ptr::null_mut();
    let ok = unsafe {
        fnd::DuplicateHandle(
            src_proc,
            src,
            tgt_proc,
            &raw mut target,
            access,
            i32::from(inherit),
            options,
        )
    };
    if ok == 0 {
        return Err(nt_support::last_win32_error_to_py(None));
    }
    Ok(handle_to_object(target as usize))
}

// ---------------------------------------------------------------------------
// Synchronization objects.
// ---------------------------------------------------------------------------

fn wait_gil(handle: HANDLE, ms: u32) -> u32 {
    // A zero timeout is a pure poll — no point paying the GIL round-trip.
    if ms == 0 {
        unsafe { thr::WaitForSingleObject(handle, 0) }
    } else {
        crate::gil::allow_threads_then(|| unsafe { thr::WaitForSingleObject(handle, ms) })
    }
}

fn win_wait_for_single_object(
    args: &[Object],
    kw: &[(String, Object)],
) -> Result<Object, RuntimeError> {
    let h = handle_arg(args, 0, "WaitForSingleObject")?;
    let ms = int_arg(
        pick(args, kw, 1, "milliseconds"),
        "WaitForSingleObject",
        "milliseconds",
    )? as u32;
    let res = wait_gil(h, ms);
    if res == fnd::WAIT_FAILED {
        return Err(nt_support::last_win32_error_to_py(None));
    }
    Ok(Object::Int(i64::from(res)))
}

fn win_wait_for_multiple_objects(
    args: &[Object],
    kw: &[(String, Object)],
) -> Result<Object, RuntimeError> {
    let handles = args.first().and_then(extract_handle_seq).ok_or_else(|| {
        type_error("WaitForMultipleObjects: handle_seq must be a sequence of handles")
    })?;
    // MAXIMUM_WAIT_OBJECTS (winnt.h) — the kernel's hard cap on a single
    // WaitForMultipleObjects call.
    if handles.len() > 64 {
        return Err(value_error("need at most 64 handles"));
    }
    let wait_all = pick(args, kw, 1, "wait_flag").is_some_and(Object::is_truthy);
    let ms = pick(args, kw, 2, "milliseconds")
        .and_then(Object::as_i64)
        .map_or(INFINITE, |v| v as u32);
    // GIL-released for any non-zero timeout (CPython always releases it
    // here; we keep the zero-timeout poll on-thread).
    let res = if ms == 0 {
        unsafe {
            thr::WaitForMultipleObjects(
                handles.len() as u32,
                handles.as_ptr(),
                i32::from(wait_all),
                0,
            )
        }
    } else {
        crate::gil::allow_threads_then(|| unsafe {
            thr::WaitForMultipleObjects(
                handles.len() as u32,
                handles.as_ptr(),
                i32::from(wait_all),
                ms,
            )
        })
    };
    if res == fnd::WAIT_FAILED {
        return Err(nt_support::last_win32_error_to_py(None));
    }
    Ok(Object::Int(i64::from(res)))
}

fn win_create_event(args: &[Object], kw: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let sec = sec_attr_ptr(pick(args, kw, 0, "security_attributes"));
    let manual_reset = pick(args, kw, 1, "manual_reset").is_some_and(Object::is_truthy);
    let initial_state = pick(args, kw, 2, "initial_state").is_some_and(Object::is_truthy);
    let name = opt_str_arg(pick(args, kw, 3, "name"), "CreateEventW", "name")?;
    let name_w = name.as_deref().map(wide);
    let h = unsafe {
        thr::CreateEventW(
            sec,
            i32::from(manual_reset),
            i32::from(initial_state),
            name_w.as_ref().map_or(std::ptr::null(), |v| v.as_ptr()),
        )
    };
    if h.is_null() {
        return Err(nt_support::last_win32_error_to_py(None));
    }
    Ok(handle_to_object(h as usize))
}

fn win_open_event(args: &[Object], kw: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let access = int_arg(
        pick(args, kw, 0, "desired_access"),
        "OpenEventW",
        "desired_access",
    )? as u32;
    let inherit = pick(args, kw, 1, "inherit_handle").is_some_and(Object::is_truthy);
    let name = str_arg(pick(args, kw, 2, "name"), "OpenEventW", "name")?;
    let name_w = wide(&name);
    let h = unsafe { thr::OpenEventW(access, i32::from(inherit), name_w.as_ptr()) };
    if h.is_null() {
        return Err(nt_support::last_win32_error_to_py(None));
    }
    Ok(handle_to_object(h as usize))
}

fn win_set_event(args: &[Object], _kw: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let h = handle_arg(args, 0, "SetEvent")?;
    if unsafe { thr::SetEvent(h) } == 0 {
        return Err(nt_support::last_win32_error_to_py(None));
    }
    Ok(Object::None)
}

fn win_reset_event(args: &[Object], _kw: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let h = handle_arg(args, 0, "ResetEvent")?;
    if unsafe { thr::ResetEvent(h) } == 0 {
        return Err(nt_support::last_win32_error_to_py(None));
    }
    Ok(Object::None)
}

fn win_create_mutex(args: &[Object], kw: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let sec = sec_attr_ptr(pick(args, kw, 0, "security_attributes"));
    let initial_owner = pick(args, kw, 1, "initial_owner").is_some_and(Object::is_truthy);
    let name = opt_str_arg(pick(args, kw, 2, "name"), "CreateMutexW", "name")?;
    let name_w = name.as_deref().map(wide);
    let h = unsafe {
        thr::CreateMutexW(
            sec,
            i32::from(initial_owner),
            name_w.as_ref().map_or(std::ptr::null(), |v| v.as_ptr()),
        )
    };
    if h.is_null() {
        return Err(nt_support::last_win32_error_to_py(None));
    }
    Ok(handle_to_object(h as usize))
}

fn win_open_mutex(args: &[Object], kw: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let access = int_arg(
        pick(args, kw, 0, "desired_access"),
        "OpenMutexW",
        "desired_access",
    )? as u32;
    let inherit = pick(args, kw, 1, "inherit_handle").is_some_and(Object::is_truthy);
    let name = str_arg(pick(args, kw, 2, "name"), "OpenMutexW", "name")?;
    let name_w = wide(&name);
    let h = unsafe { thr::OpenMutexW(access, i32::from(inherit), name_w.as_ptr()) };
    if h.is_null() {
        return Err(nt_support::last_win32_error_to_py(None));
    }
    Ok(handle_to_object(h as usize))
}

fn win_release_mutex(args: &[Object], _kw: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let h = handle_arg(args, 0, "ReleaseMutex")?;
    if unsafe { thr::ReleaseMutex(h) } == 0 {
        return Err(nt_support::last_win32_error_to_py(None));
    }
    Ok(Object::None)
}

// ---------------------------------------------------------------------------
// Pipes and files.
// ---------------------------------------------------------------------------

fn win_create_pipe(args: &[Object], kw: &[(String, Object)]) -> Result<Object, RuntimeError> {
    // `pipe_attrs` is accepted for signature parity and ignored, exactly
    // as CPython's `_winapi.CreatePipe` passes `NULL`.
    let _pipe_attrs = pick(args, kw, 0, "pipe_attrs");
    let size = pick(args, kw, 1, "size")
        .and_then(Object::as_i64)
        .unwrap_or(0) as u32;
    let mut read: HANDLE = std::ptr::null_mut();
    let mut write: HANDLE = std::ptr::null_mut();
    let ok = crate::gil::allow_threads_then(|| unsafe {
        pipes::CreatePipe(&raw mut read, &raw mut write, std::ptr::null(), size)
    });
    if ok == 0 {
        return Err(nt_support::last_win32_error_to_py(None));
    }
    Ok(Object::new_tuple(vec![
        handle_to_object(read as usize),
        handle_to_object(write as usize),
    ]))
}

fn win_create_named_pipe(args: &[Object], kw: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let name = str_arg(pick(args, kw, 0, "name"), "CreateNamedPipe", "name")?;
    let open_mode = int_arg(
        pick(args, kw, 1, "open_mode"),
        "CreateNamedPipe",
        "open_mode",
    )? as u32;
    let pipe_mode = int_arg(
        pick(args, kw, 2, "pipe_mode"),
        "CreateNamedPipe",
        "pipe_mode",
    )? as u32;
    let max_instances = int_arg(
        pick(args, kw, 3, "max_instances"),
        "CreateNamedPipe",
        "max_instances",
    )? as u32;
    let out_size = int_arg(
        pick(args, kw, 4, "out_buffer_size"),
        "CreateNamedPipe",
        "out_buffer_size",
    )? as u32;
    let in_size = int_arg(
        pick(args, kw, 5, "in_buffer_size"),
        "CreateNamedPipe",
        "in_buffer_size",
    )? as u32;
    let default_timeout = int_arg(
        pick(args, kw, 6, "default_timeout"),
        "CreateNamedPipe",
        "default_timeout",
    )? as u32;
    let sec = sec_attr_ptr(pick(args, kw, 7, "security_attributes"));
    let name_w = wide(&name);
    let h = unsafe {
        pipes::CreateNamedPipeW(
            name_w.as_ptr(),
            open_mode,
            pipe_mode,
            max_instances,
            out_size,
            in_size,
            default_timeout,
            sec,
        )
    };
    if h == fnd::INVALID_HANDLE_VALUE {
        return Err(nt_support::last_win32_error_to_py(None));
    }
    Ok(handle_to_object(h as usize))
}

fn win_connect_named_pipe(
    args: &[Object],
    kw: &[(String, Object)],
) -> Result<Object, RuntimeError> {
    let h = handle_arg(args, 0, "ConnectNamedPipe")?;
    let overlapped = pick(args, kw, 1, "overlapped").is_some_and(Object::is_truthy);
    if overlapped {
        let ov = OverlappedObject::new(h, false, None);
        let ovp = ov.overlapped_ptr();
        let ret = unsafe { pipes::ConnectNamedPipe(h, ovp) };
        let err = if ret != 0 {
            ERROR_SUCCESS
        } else {
            unsafe { fnd::GetLastError() }
        };
        match err {
            ERROR_IO_PENDING => ov.set_pending(true),
            // A client that connected between CreateNamedPipe and here
            // reports PIPE_CONNECTED; CPython treats that as done.
            535 /* ERROR_PIPE_CONNECTED */ | ERROR_SUCCESS => ov.set_pending(false),
            _ => {
                ov.discard();
                return Err(nt_support::win32_error_to_py(err as i32, None));
            }
        }
        return Ok(ov.into_object());
    }
    let ok = crate::gil::allow_threads_then(|| unsafe {
        pipes::ConnectNamedPipe(h, std::ptr::null_mut())
    });
    // ERROR_PIPE_CONNECTED is success for a synchronous connect too.
    if ok == 0 {
        let err = unsafe { fnd::GetLastError() };
        if err != 535 {
            return Err(nt_support::win32_error_to_py(err as i32, None));
        }
    }
    Ok(Object::None)
}

fn win_wait_named_pipe(args: &[Object], kw: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let name = str_arg(pick(args, kw, 0, "name"), "WaitNamedPipe", "name")?;
    let timeout = int_arg(pick(args, kw, 1, "timeout"), "WaitNamedPipe", "timeout")? as u32;
    let name_w = wide(&name);
    let ok = crate::gil::allow_threads_then(|| unsafe {
        pipes::WaitNamedPipeW(name_w.as_ptr(), timeout)
    });
    if ok == 0 {
        return Err(nt_support::last_win32_error_to_py(None));
    }
    Ok(Object::None)
}

fn win_peek_named_pipe(args: &[Object], kw: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let h = handle_arg(args, 0, "PeekNamedPipe")?;
    let size = pick(args, kw, 1, "size")
        .and_then(Object::as_i64)
        .unwrap_or(0);
    if size < 0 {
        return Err(value_error("negative size"));
    }
    let mut read: u32 = 0;
    let mut avail: u32 = 0;
    let mut left: u32 = 0;
    if size == 0 {
        // Query-only form: (bytes_available, bytes_left_this_message).
        let ok = unsafe {
            pipes::PeekNamedPipe(
                h,
                std::ptr::null_mut(),
                0,
                std::ptr::null_mut(),
                &raw mut avail,
                &raw mut left,
            )
        };
        if ok == 0 {
            return Err(nt_support::last_win32_error_to_py(None));
        }
        return Ok(Object::new_tuple(vec![
            Object::Int(i64::from(avail)),
            Object::Int(i64::from(left)),
        ]));
    }
    let mut buf = vec![0u8; size as usize];
    let ok = unsafe {
        pipes::PeekNamedPipe(
            h,
            buf.as_mut_ptr().cast::<c_void>(),
            size as u32,
            &raw mut read,
            &raw mut avail,
            &raw mut left,
        )
    };
    // CPython tolerates ERROR_MORE_DATA (the peek buffer was smaller than
    // the message) and returns the partial read.
    if ok == 0 {
        let err = unsafe { fnd::GetLastError() };
        if err != ERROR_MORE_DATA {
            return Err(nt_support::win32_error_to_py(err as i32, None));
        }
    }
    buf.truncate(read as usize);
    Ok(Object::new_tuple(vec![
        Object::new_bytes(buf),
        Object::Int(i64::from(avail)),
        Object::Int(i64::from(left)),
    ]))
}

fn win_set_named_pipe_handle_state(
    args: &[Object],
    kw: &[(String, Object)],
) -> Result<Object, RuntimeError> {
    let h = handle_arg(args, 0, "SetNamedPipeHandleState")?;
    // Each of mode / max_collection_count / collect_data_timeout is
    // independently `None`-able (pass NULL to leave it unchanged).
    let mode = pick(args, kw, 1, "mode")
        .and_then(Object::as_i64)
        .map(|v| v as u32);
    let max_collect = pick(args, kw, 2, "max_collection_count")
        .and_then(Object::as_i64)
        .map(|v| v as u32);
    let timeout = pick(args, kw, 3, "collect_data_timeout")
        .and_then(Object::as_i64)
        .map(|v| v as u32);
    let ok = unsafe {
        pipes::SetNamedPipeHandleState(
            h,
            mode.as_ref()
                .map_or(std::ptr::null(), std::ptr::from_ref::<u32>),
            max_collect
                .as_ref()
                .map_or(std::ptr::null(), std::ptr::from_ref::<u32>),
            timeout
                .as_ref()
                .map_or(std::ptr::null(), std::ptr::from_ref::<u32>),
        )
    };
    if ok == 0 {
        return Err(nt_support::last_win32_error_to_py(None));
    }
    Ok(Object::None)
}

fn win_create_file(args: &[Object], kw: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let name = str_arg(pick(args, kw, 0, "file_name"), "CreateFile", "file_name")?;
    let access = int_arg(
        pick(args, kw, 1, "desired_access"),
        "CreateFile",
        "desired_access",
    )? as u32;
    let share = int_arg(pick(args, kw, 2, "share_mode"), "CreateFile", "share_mode")? as u32;
    let sec = sec_attr_ptr(pick(args, kw, 3, "security_attributes"));
    let disp = int_arg(
        pick(args, kw, 4, "creation_disposition"),
        "CreateFile",
        "creation_disposition",
    )? as u32;
    let flags = int_arg(
        pick(args, kw, 5, "flags_and_attributes"),
        "CreateFile",
        "flags_and_attributes",
    )? as u32;
    let template = pick(args, kw, 6, "template_file")
        .and_then(obj_to_usize)
        .unwrap_or(0) as HANDLE;
    let name_w = wide(&name);
    let h = unsafe { fs::CreateFileW(name_w.as_ptr(), access, share, sec, disp, flags, template) };
    if h == fnd::INVALID_HANDLE_VALUE {
        return Err(nt_support::last_win32_error_to_py(Some(&name)));
    }
    Ok(handle_to_object(h as usize))
}

fn win_read_file(args: &[Object], kw: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let h = handle_arg(args, 0, "ReadFile")?;
    let size = int_arg(pick(args, kw, 1, "size"), "ReadFile", "size")?;
    if size < 0 {
        return Err(value_error("negative size"));
    }
    let overlapped = pick(args, kw, 2, "overlapped").is_some_and(Object::is_truthy);
    let size = size as usize;

    if overlapped {
        let ov = OverlappedObject::new(h, false, Some(vec![0u8; size]));
        let ovp = ov.overlapped_ptr();
        let (bufptr, buflen) = ov.buffer_ptr_len();
        let mut nread: u32 = 0;
        let ret = unsafe { fs::ReadFile(h, bufptr, buflen as u32, &raw mut nread, ovp) };
        let err = if ret != 0 {
            ERROR_SUCCESS
        } else {
            unsafe { fnd::GetLastError() }
        };
        match err {
            ERROR_BROKEN_PIPE => ov.set_pending(false),
            ERROR_SUCCESS | ERROR_MORE_DATA | ERROR_IO_PENDING => ov.set_pending(true),
            _ => {
                ov.discard();
                return Err(nt_support::win32_error_to_py(err as i32, None));
            }
        }
        return Ok(Object::new_tuple(vec![
            ov.into_object(),
            Object::Int(i64::from(err)),
        ]));
    }

    let mut buf = vec![0u8; size];
    let mut nread: u32 = 0;
    let bufptr = buf.as_mut_ptr();
    let ret = crate::gil::allow_threads_then(|| unsafe {
        fs::ReadFile(h, bufptr, size as u32, &raw mut nread, std::ptr::null_mut())
    });
    let err = if ret != 0 {
        ERROR_SUCCESS
    } else {
        unsafe { fnd::GetLastError() }
    };
    match err {
        ERROR_BROKEN_PIPE => Ok(Object::new_tuple(vec![
            Object::new_bytes(Vec::new()),
            Object::Int(i64::from(err)),
        ])),
        ERROR_SUCCESS | ERROR_MORE_DATA => {
            buf.truncate(nread as usize);
            Ok(Object::new_tuple(vec![
                Object::new_bytes(buf),
                Object::Int(i64::from(err)),
            ]))
        }
        _ => Err(nt_support::win32_error_to_py(err as i32, None)),
    }
}

fn win_write_file(args: &[Object], kw: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let h = handle_arg(args, 0, "WriteFile")?;
    let data = pick(args, kw, 1, "buffer")
        .and_then(Object::as_bytes_view)
        .ok_or_else(|| type_error("WriteFile: buffer must be a bytes-like object"))?;
    let overlapped = pick(args, kw, 2, "overlapped").is_some_and(Object::is_truthy);

    if overlapped {
        // The buffer must outlive the async write; the Overlapped owns it.
        let ov = OverlappedObject::new(h, true, Some(data));
        let ovp = ov.overlapped_ptr();
        let (bufptr, buflen) = ov.buffer_ptr_len();
        let mut written: u32 = 0;
        let ret =
            unsafe { fs::WriteFile(h, bufptr.cast_const(), buflen as u32, &raw mut written, ovp) };
        let err = if ret != 0 {
            ERROR_SUCCESS
        } else {
            unsafe { fnd::GetLastError() }
        };
        match err {
            ERROR_SUCCESS | ERROR_IO_PENDING => ov.set_pending(true),
            _ => {
                ov.discard();
                return Err(nt_support::win32_error_to_py(err as i32, None));
            }
        }
        return Ok(Object::new_tuple(vec![
            ov.into_object(),
            Object::Int(i64::from(err)),
        ]));
    }

    let mut written: u32 = 0;
    let dptr = data.as_ptr();
    let dlen = data.len();
    let ret = crate::gil::allow_threads_then(|| unsafe {
        fs::WriteFile(h, dptr, dlen as u32, &raw mut written, std::ptr::null_mut())
    });
    if ret == 0 {
        return Err(nt_support::last_win32_error_to_py(None));
    }
    Ok(Object::new_tuple(vec![
        Object::Int(i64::from(written)),
        Object::Int(0),
    ]))
}

// ---------------------------------------------------------------------------
// File mappings (the shared_memory NT backend).
// ---------------------------------------------------------------------------

fn win_create_file_mapping(
    args: &[Object],
    kw: &[(String, Object)],
) -> Result<Object, RuntimeError> {
    let file = handle_arg(args, 0, "CreateFileMapping")?;
    let sec = sec_attr_ptr(pick(args, kw, 1, "security_attributes"));
    let protect = int_arg(pick(args, kw, 2, "protect"), "CreateFileMapping", "protect")? as u32;
    let max_high = int_arg(
        pick(args, kw, 3, "maximum_size_high"),
        "CreateFileMapping",
        "maximum_size_high",
    )? as u32;
    let max_low = int_arg(
        pick(args, kw, 4, "maximum_size_low"),
        "CreateFileMapping",
        "maximum_size_low",
    )? as u32;
    let name = opt_str_arg(pick(args, kw, 5, "name"), "CreateFileMapping", "name")?;
    let name_w = name.as_deref().map(wide);
    let h = unsafe {
        mem::CreateFileMappingW(
            file,
            sec,
            protect,
            max_high,
            max_low,
            name_w.as_ref().map_or(std::ptr::null(), |v| v.as_ptr()),
        )
    };
    if h.is_null() {
        return Err(nt_support::last_win32_error_to_py(None));
    }
    Ok(handle_to_object(h as usize))
}

fn win_open_file_mapping(args: &[Object], kw: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let access = int_arg(
        pick(args, kw, 0, "desired_access"),
        "OpenFileMapping",
        "desired_access",
    )? as u32;
    let inherit = pick(args, kw, 1, "inherit_handle").is_some_and(Object::is_truthy);
    let name = str_arg(pick(args, kw, 2, "name"), "OpenFileMapping", "name")?;
    let name_w = wide(&name);
    let h = unsafe { mem::OpenFileMappingW(access, i32::from(inherit), name_w.as_ptr()) };
    if h.is_null() {
        return Err(nt_support::last_win32_error_to_py(None));
    }
    Ok(handle_to_object(h as usize))
}

fn win_map_view_of_file(args: &[Object], kw: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let file = handle_arg(args, 0, "MapViewOfFile")?;
    let access = int_arg(
        pick(args, kw, 1, "desired_access"),
        "MapViewOfFile",
        "desired_access",
    )? as u32;
    let off_high = int_arg(
        pick(args, kw, 2, "file_offset_high"),
        "MapViewOfFile",
        "file_offset_high",
    )? as u32;
    let off_low = int_arg(
        pick(args, kw, 3, "file_offset_low"),
        "MapViewOfFile",
        "file_offset_low",
    )? as u32;
    let count = pick(args, kw, 4, "number_bytes")
        .and_then(Object::as_i64)
        .unwrap_or(0) as usize;
    let view = unsafe { mem::MapViewOfFile(file, access, off_high, off_low, count) };
    if view.Value.is_null() {
        return Err(nt_support::last_win32_error_to_py(None));
    }
    Ok(handle_to_object(view.Value as usize))
}

fn win_unmap_view_of_file(
    args: &[Object],
    _kw: &[(String, Object)],
) -> Result<Object, RuntimeError> {
    let addr = handle_arg(args, 0, "UnmapViewOfFile")?;
    let view = mem::MEMORY_MAPPED_VIEW_ADDRESS { Value: addr.cast() };
    if unsafe { mem::UnmapViewOfFile(view) } == 0 {
        return Err(nt_support::last_win32_error_to_py(None));
    }
    Ok(Object::None)
}

fn win_virtual_query_size(
    args: &[Object],
    _kw: &[(String, Object)],
) -> Result<Object, RuntimeError> {
    let addr = args
        .first()
        .and_then(obj_to_usize)
        .ok_or_else(|| type_error("VirtualQuerySize: address must be an int"))?;
    let mut info: mem::MEMORY_BASIC_INFORMATION = unsafe { std::mem::zeroed() };
    let written = unsafe {
        mem::VirtualQuery(
            addr as *const c_void,
            &raw mut info,
            std::mem::size_of::<mem::MEMORY_BASIC_INFORMATION>(),
        )
    };
    if written == 0 {
        return Err(nt_support::last_win32_error_to_py(None));
    }
    Ok(Object::Int(info.RegionSize as i64))
}

// ---------------------------------------------------------------------------
// Process creation.
// ---------------------------------------------------------------------------

/// Read one attribute off a Python object (the `STARTUPINFO` instance
/// `subprocess` passes), returning `None` when the attribute is absent
/// or `None`-valued.
fn get_attr_opt(obj: &Object, name: &str) -> Option<Object> {
    let ptr = crate::vm_singletons::current_interpreter_ptr()?;
    // SAFETY: the GIL is held throughout a builtin body, so the
    // interpreter pointer is exclusively ours (same contract as
    // `codecs_engine::get_attr`).
    let interp = unsafe { &mut *ptr };
    match interp.load_attr_public(obj, name) {
        Ok(Object::None) | Err(_) => None,
        Ok(v) => Some(v),
    }
}

/// The double-NUL UTF-16 environment block CPython's
/// `getenvironment()` builds: `KEY=VALUE` entries sorted case-insensitively
/// by key, NUL-separated, with a trailing empty entry.
fn build_environment_block(env: &Object) -> Result<Vec<u16>, RuntimeError> {
    let dict = match env {
        Object::Dict(d) => d,
        _ => return Err(type_error("environment must be a mapping or None")),
    };
    let mut entries: Vec<(String, String)> = Vec::new();
    for (k, v) in dict.borrow().iter() {
        let key = match &k.0 {
            Object::Str(s) => s.to_string(),
            _ => return Err(value_error("environment keys must be strings")),
        };
        let val = match v {
            Object::Str(s) => s.to_string(),
            _ => return Err(value_error("environment values must be strings")),
        };
        // CPython rejects an '=' inside a key (it would corrupt the block),
        // except the leading-'=' "drive current directory" entries.
        if key[1..].contains('=') {
            return Err(value_error("illegal environment variable name"));
        }
        entries.push((key, val));
    }
    entries.sort_by_key(|a| a.0.to_uppercase());
    let mut block: Vec<u16> = Vec::new();
    for (k, v) in entries {
        block.extend(format!("{k}={v}").encode_utf16());
        block.push(0);
    }
    // An empty mapping still needs the block's own terminating NUL so the
    // result is never a bare pointer to nothing.
    block.push(0);
    Ok(block)
}

fn win_create_process(args: &[Object], kw: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let app_name = opt_str_arg(
        pick(args, kw, 0, "application_name"),
        "CreateProcess",
        "application_name",
    )?;
    let cmd_line = opt_str_arg(
        pick(args, kw, 1, "command_line"),
        "CreateProcess",
        "command_line",
    )?;
    let proc_attrs = sec_attr_ptr(pick(args, kw, 2, "proc_attrs"));
    let thread_attrs = sec_attr_ptr(pick(args, kw, 3, "thread_attrs"));
    let inherit = pick(args, kw, 4, "inherit_handles").is_some_and(Object::is_truthy);
    let flags = int_arg(
        pick(args, kw, 5, "creation_flags"),
        "CreateProcess",
        "creation_flags",
    )? as u32;
    let env = pick(args, kw, 6, "env_mapping");
    let cwd = opt_str_arg(
        pick(args, kw, 7, "current_directory"),
        "CreateProcess",
        "current_directory",
    )?;
    let startup_info = pick(args, kw, 8, "startup_info");

    let app_w = app_name.as_deref().map(wide);
    // CreateProcessW may write into the command-line buffer, so it must be
    // a writable, NUL-terminated copy.
    let mut cmd_w = cmd_line.as_deref().map(wide);
    let cwd_w = cwd.as_deref().map(wide);

    // The environment block, when supplied, is CREATE_UNICODE_ENVIRONMENT.
    let env_block = match env {
        None | Some(Object::None) => None,
        Some(e) => Some(build_environment_block(e)?),
    };
    let creation_flags = flags | if env_block.is_some() { 0x0000_0400 } else { 0 }; // CREATE_UNICODE_ENVIRONMENT

    let mut si: thr::STARTUPINFOW = unsafe { std::mem::zeroed() };
    si.cb = std::mem::size_of::<thr::STARTUPINFOW>() as u32;
    if let Some(sinfo) = startup_info {
        if let Some(v) = get_attr_opt(sinfo, "dwFlags")
            .as_ref()
            .and_then(Object::as_i64)
        {
            si.dwFlags = v as u32;
        }
        if let Some(v) = get_attr_opt(sinfo, "wShowWindow")
            .as_ref()
            .and_then(Object::as_i64)
        {
            si.wShowWindow = v as u16;
        }
        if let Some(h) = get_attr_opt(sinfo, "hStdInput")
            .as_ref()
            .and_then(obj_to_usize)
        {
            si.hStdInput = h as HANDLE;
        }
        if let Some(h) = get_attr_opt(sinfo, "hStdOutput")
            .as_ref()
            .and_then(obj_to_usize)
        {
            si.hStdOutput = h as HANDLE;
        }
        if let Some(h) = get_attr_opt(sinfo, "hStdError")
            .as_ref()
            .and_then(obj_to_usize)
        {
            si.hStdError = h as HANDLE;
        }
        // `lpAttributeList={"handle_list": [...]}` restricts inheritance to
        // the listed handles via a STARTUPINFOEX proc-thread attribute in
        // CPython. WeavePy takes the pre-3.7 equivalent for now: mark each
        // listed handle inheritable and rely on `bInheritHandles`. This
        // inherits the same handles; it does not *restrict* inheritance to
        // only them (the attribute-list isolation is deferred — it needs
        // InitializeProcThreadAttributeList plumbing).
        if let Some(attr) = get_attr_opt(sinfo, "lpAttributeList") {
            if let Object::Dict(d) = &attr {
                let hl = d
                    .borrow()
                    .get(&DictKey(Object::from_static("handle_list")))
                    .cloned();
                if let Some(list) = hl.as_ref().and_then(extract_handle_seq) {
                    for h in list {
                        unsafe {
                            fnd::SetHandleInformation(h, 1 /* HANDLE_FLAG_INHERIT */, 1);
                        }
                    }
                }
            }
        }
    }

    let mut pi: thr::PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
    let ok = unsafe {
        thr::CreateProcessW(
            app_w.as_ref().map_or(std::ptr::null(), |v| v.as_ptr()),
            cmd_w
                .as_mut()
                .map_or(std::ptr::null_mut(), |v| v.as_mut_ptr()),
            proc_attrs,
            thread_attrs,
            i32::from(inherit),
            creation_flags,
            env_block
                .as_ref()
                .map_or(std::ptr::null(), |v| v.as_ptr().cast::<c_void>()),
            cwd_w.as_ref().map_or(std::ptr::null(), |v| v.as_ptr()),
            &raw const si,
            &raw mut pi,
        )
    };
    if ok == 0 {
        return Err(nt_support::last_win32_error_to_py(None));
    }
    Ok(Object::new_tuple(vec![
        handle_to_object(pi.hProcess as usize),
        handle_to_object(pi.hThread as usize),
        Object::Int(i64::from(pi.dwProcessId)),
        Object::Int(i64::from(pi.dwThreadId)),
    ]))
}

// ---------------------------------------------------------------------------
// Reparse points, exe-path policy, file copy, locale mapping.
// ---------------------------------------------------------------------------

const IO_REPARSE_TAG_MOUNT_POINT: u32 = 0xA000_0003;
const FSCTL_SET_REPARSE_POINT: u32 = 0x0009_00A4;
const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;

fn win_create_junction(args: &[Object], kw: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let src = str_arg(pick(args, kw, 0, "src_path"), "CreateJunction", "src_path")?;
    let dst = str_arg(pick(args, kw, 1, "dst_path"), "CreateJunction", "dst_path")?;

    // The reparse target must be an absolute NT path (`\??\` prefix), like
    // CPython's `_winapi_CreateJunction_impl`.
    let substitute: Vec<u16> = wide(&format!("\\??\\{src}"));
    let subst_wo_nul = &substitute[..substitute.len() - 1];

    // Create the empty directory that becomes the junction, then open it
    // with backup semantics so the reparse write is permitted.
    let dst_w = wide(&dst);
    if unsafe { fs::CreateDirectoryW(dst_w.as_ptr(), std::ptr::null()) } == 0 {
        return Err(nt_support::last_win32_error_to_py(Some(&dst)));
    }
    let junction = unsafe {
        fs::CreateFileW(
            dst_w.as_ptr(),
            fnd::GENERIC_WRITE,
            0,
            std::ptr::null(),
            fs::OPEN_EXISTING,
            FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS,
            std::ptr::null_mut(),
        )
    };
    if junction == fnd::INVALID_HANDLE_VALUE {
        return Err(nt_support::last_win32_error_to_py(Some(&dst)));
    }

    // REPARSE_DATA_BUFFER (mount-point form). The path buffer holds the
    // substitute name (NUL-terminated) followed by an empty print name.
    let subst_bytes = subst_wo_nul.len() * 2;
    let path_buffer_len = subst_bytes + 2 /* subst NUL */ + 2 /* empty print name NUL */;
    let reparse_data_length = 8 /* the four WORD offset/length fields */ + path_buffer_len;
    let mut buf: Vec<u8> = Vec::with_capacity(8 + reparse_data_length);
    buf.extend_from_slice(&IO_REPARSE_TAG_MOUNT_POINT.to_le_bytes());
    buf.extend_from_slice(&(reparse_data_length as u16).to_le_bytes());
    buf.extend_from_slice(&0u16.to_le_bytes()); // Reserved
    buf.extend_from_slice(&0u16.to_le_bytes()); // SubstituteNameOffset
    buf.extend_from_slice(&(subst_bytes as u16).to_le_bytes()); // SubstituteNameLength
    buf.extend_from_slice(&((subst_bytes + 2) as u16).to_le_bytes()); // PrintNameOffset
    buf.extend_from_slice(&0u16.to_le_bytes()); // PrintNameLength (empty)
    for &wc in subst_wo_nul {
        buf.extend_from_slice(&wc.to_le_bytes());
    }
    buf.extend_from_slice(&0u16.to_le_bytes()); // substitute NUL
    buf.extend_from_slice(&0u16.to_le_bytes()); // empty print name NUL

    let mut returned: u32 = 0;
    let ok = unsafe {
        wio::DeviceIoControl(
            junction,
            FSCTL_SET_REPARSE_POINT,
            buf.as_ptr().cast::<c_void>(),
            buf.len() as u32,
            std::ptr::null_mut(),
            0,
            &raw mut returned,
            std::ptr::null_mut(),
        )
    };
    let err = if ok != 0 {
        None
    } else {
        Some(nt_support::last_win32_error_to_py(Some(&dst)))
    };
    unsafe {
        fnd::CloseHandle(junction);
    }
    if let Some(e) = err {
        return Err(e);
    }
    Ok(Object::None)
}

fn win_need_cwd_for_exe_path(
    args: &[Object],
    kw: &[(String, Object)],
) -> Result<Object, RuntimeError> {
    let exe = str_arg(
        pick(args, kw, 0, "exe_name"),
        "NeedCurrentDirectoryForExePath",
        "exe_name",
    )?;
    let exe_w = wide(&exe);
    let need = unsafe {
        windows_sys::Win32::System::Environment::NeedCurrentDirectoryForExePathW(exe_w.as_ptr())
    };
    Ok(Object::Bool(need != 0))
}

fn win_copy_file2(args: &[Object], kw: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let src = str_arg(
        pick(args, kw, 0, "existing_file_name"),
        "CopyFile2",
        "existing_file_name",
    )?;
    let dst = str_arg(
        pick(args, kw, 1, "new_file_name"),
        "CopyFile2",
        "new_file_name",
    )?;
    // `flags` here is the `COPYFILE2_EXTENDED_PARAMETERS.dwCopyFlags` set;
    // `progress_routine` is accepted for signature parity and unused (the
    // callback bridge is deferred). We implement over CopyFileExW, whose
    // dwCopyFlags space is the same COPY_FILE_* bits shutil passes.
    let flags = pick(args, kw, 2, "flags")
        .and_then(Object::as_i64)
        .unwrap_or(0) as u32;
    let _progress = pick(args, kw, 3, "progress_routine");
    let src_w = wide(&src);
    let dst_w = wide(&dst);
    let ok = crate::gil::allow_threads_then(|| unsafe {
        fs::CopyFileExW(
            src_w.as_ptr(),
            dst_w.as_ptr(),
            None,
            std::ptr::null(),
            std::ptr::null_mut(),
            flags,
        )
    });
    if ok == 0 {
        return Err(nt_support::last_win32_error_to_py(Some(&src)));
    }
    // CPython's `_winapi.CopyFile2` returns S_OK (0).
    Ok(Object::Int(0))
}

fn win_lcmapstring_ex(args: &[Object], kw: &[(String, Object)]) -> Result<Object, RuntimeError> {
    // locale=None → user-default (NULL); the empty string is the invariant
    // locale, which LCMapStringEx accepts directly.
    let locale = opt_str_arg(
        pick(args, kw, 0, "locale_name"),
        "LCMapStringEx",
        "locale_name",
    )?;
    let flags = int_arg(pick(args, kw, 1, "map_flags"), "LCMapStringEx", "map_flags")? as u32;
    let src = str_arg(pick(args, kw, 2, "src"), "LCMapStringEx", "src")?;
    let locale_w = locale.as_deref().map(wide);
    let locale_ptr = locale_w.as_ref().map_or(std::ptr::null(), |v| v.as_ptr());
    let src_w: Vec<u16> = src.encode_utf16().collect();
    let src_len = src_w.len() as i32;

    let needed = unsafe {
        glob::LCMapStringEx(
            locale_ptr,
            flags,
            src_w.as_ptr(),
            src_len,
            std::ptr::null_mut(),
            0,
            std::ptr::null(),
            std::ptr::null(),
            0,
        )
    };
    if needed <= 0 {
        return Err(nt_support::last_win32_error_to_py(None));
    }
    let mut out = vec![0u16; needed as usize];
    let written = unsafe {
        glob::LCMapStringEx(
            locale_ptr,
            flags,
            src_w.as_ptr(),
            src_len,
            out.as_mut_ptr(),
            needed,
            std::ptr::null(),
            std::ptr::null(),
            0,
        )
    };
    if written <= 0 {
        return Err(nt_support::last_win32_error_to_py(None));
    }
    Ok(Object::from_str(nt_support::from_wide(
        &out[..written as usize],
    )))
}

// ---------------------------------------------------------------------------
// The `Overlapped` helper type.
// ---------------------------------------------------------------------------
//
// CPython's `_winapi.Overlapped` pins its OVERLAPPED, its owned event,
// and (for reads/writes) its buffer until the async op completes or is
// cancelled — the VM must never free memory the kernel still owns. We
// keep that state in a process-global registry keyed by an opaque id
// carried on the instance, mirroring `select.poll`'s handle scheme. All
// fields are integers/`Vec`, so the registry is `Send` even though a raw
// `HANDLE` is not; pointers are reconstituted at use.

struct OverlappedState {
    /// Owned `*mut OVERLAPPED` (`Box::into_raw`); freed on drop.
    ov: usize,
    /// Owned manual-reset event; `CloseHandle`d on drop.
    event: usize,
    /// The file/pipe handle the op runs on (borrowed, not owned).
    handle: usize,
    /// The pinned I/O buffer: the read target (resized to the transferred
    /// count on completion) or the write source (kept alive).
    buffer: Option<Vec<u8>>,
    is_write: bool,
    pending: bool,
    completed: bool,
}

fn overlapped_registry() -> &'static Mutex<HashMap<i64, OverlappedState>> {
    static R: std::sync::OnceLock<Mutex<HashMap<i64, OverlappedState>>> =
        std::sync::OnceLock::new();
    R.get_or_init(|| Mutex::new(HashMap::new()))
}

static NEXT_OVERLAPPED_ID: AtomicI64 = AtomicI64::new(1);

thread_local! {
    static OVERLAPPED_CLASS: RefCell<Option<Rc<crate::types::TypeObject>>> =
        const { RefCell::new(None) };
}

/// A live handle to a registry entry, used only during construction of
/// an op before it is turned into (or discarded instead of) an instance.
struct OverlappedObject {
    id: i64,
    event: usize,
}

impl OverlappedObject {
    fn new(handle: HANDLE, is_write: bool, buffer: Option<Vec<u8>>) -> Self {
        // Manual-reset, non-signaled, unnamed — CPython's overlapped event.
        let event = unsafe { thr::CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) };
        let ov = Box::new(wio::OVERLAPPED {
            hEvent: event,
            ..unsafe { std::mem::zeroed() }
        });
        let ovp = Box::into_raw(ov);
        let id = NEXT_OVERLAPPED_ID.fetch_add(1, Ordering::Relaxed);
        overlapped_registry().lock().unwrap().insert(
            id,
            OverlappedState {
                ov: ovp as usize,
                event: event as usize,
                handle: handle as usize,
                buffer,
                is_write,
                pending: false,
                completed: false,
            },
        );
        OverlappedObject {
            id,
            event: event as usize,
        }
    }

    fn overlapped_ptr(&self) -> *mut wio::OVERLAPPED {
        overlapped_registry().lock().unwrap()[&self.id].ov as *mut wio::OVERLAPPED
    }

    /// The pinned buffer's `(ptr, len)` — the heap allocation is stable
    /// across registry rehashes, so the kernel-visible pointer stays valid.
    fn buffer_ptr_len(&self) -> (*mut u8, usize) {
        let mut reg = overlapped_registry().lock().unwrap();
        let st = reg.get_mut(&self.id).unwrap();
        let b = st.buffer.as_mut().expect("overlapped op has a buffer");
        (b.as_mut_ptr(), b.len())
    }

    fn set_pending(&self, pending: bool) {
        let mut reg = overlapped_registry().lock().unwrap();
        let st = reg.get_mut(&self.id).unwrap();
        st.pending = pending;
        st.completed = !pending;
    }

    /// The op could not be issued; free the entry (and its OVERLAPPED +
    /// event) without ever handing out an instance.
    fn discard(self) {
        if let Some(st) = overlapped_registry().lock().unwrap().remove(&self.id) {
            free_overlapped_state(st);
        }
    }

    fn into_object(self) -> Object {
        let inst = Rc::new(crate::types::PyInstance::new(overlapped_type()));
        {
            let mut d = inst.dict.borrow_mut();
            d.insert(DictKey(Object::from_static("_id")), Object::Int(self.id));
            // `.event` is a plain attribute here (CPython exposes it as a
            // read-only getset; the consumers only read it).
            d.insert(
                DictKey(Object::from_static("event")),
                handle_to_object(self.event),
            );
        }
        Object::Instance(inst)
    }
}

/// Free an OVERLAPPED box + owned event. The caller must have already
/// ensured the kernel is done with them (op not pending, or drained).
fn free_overlapped_state(st: OverlappedState) {
    unsafe {
        drop(Box::from_raw(st.ov as *mut wio::OVERLAPPED));
        fnd::CloseHandle(st.event as HANDLE);
    }
}

fn overlapped_method(
    name: &'static str,
    body: fn(&[Object]) -> Result<Object, RuntimeError>,
) -> Object {
    Object::Builtin(Rc::new(BuiltinFn {
        name,
        binds_instance: true,
        call: Box::new(body),
        call_kw: None,
    }))
}

fn overlapped_type() -> Rc<crate::types::TypeObject> {
    OVERLAPPED_CLASS.with(|slot| {
        if let Some(c) = slot.borrow().as_ref() {
            return c.clone();
        }
        let bt = crate::builtin_types::builtin_types();
        let mut dict = DictData::default();
        for (name, m) in [
            (
                "GetOverlappedResult",
                overlapped_method("GetOverlappedResult", overlapped_get_result),
            ),
            (
                "getbuffer",
                overlapped_method("getbuffer", overlapped_getbuffer),
            ),
            ("cancel", overlapped_method("cancel", overlapped_cancel)),
            ("__del__", overlapped_method("__del__", overlapped_del)),
        ] {
            dict.insert(DictKey(Object::from_static(name)), m);
        }
        dict.insert(
            DictKey(Object::from_static("__module__")),
            Object::from_static("_winapi"),
        );
        let cls = crate::types::TypeObject::new_user("Overlapped", vec![bt.object_.clone()], dict)
            .expect("Overlapped class must linearise");
        *slot.borrow_mut() = Some(cls.clone());
        cls
    })
}

fn overlapped_id(args: &[Object]) -> Result<i64, RuntimeError> {
    match args.first() {
        Some(Object::Instance(i)) => {
            match i.dict.borrow().get(&DictKey(Object::from_static("_id"))) {
                Some(Object::Int(id)) => Ok(*id),
                _ => Err(value_error("Overlapped object is closed")),
            }
        }
        _ => Err(type_error("descriptor requires an 'Overlapped' object")),
    }
}

fn overlapped_get_result(args: &[Object]) -> Result<Object, RuntimeError> {
    let id = overlapped_id(args)?;
    let wait = args.get(1).is_some_and(Object::is_truthy);
    let (handle, ovp) = {
        let reg = overlapped_registry().lock().unwrap();
        let st = reg
            .get(&id)
            .ok_or_else(|| value_error("Overlapped object is closed"))?;
        (st.handle as HANDLE, st.ov as *const wio::OVERLAPPED)
    };
    let mut transferred: u32 = 0;
    let res = if wait {
        crate::gil::allow_threads_then(|| unsafe {
            wio::GetOverlappedResult(handle, ovp, &raw mut transferred, 1)
        })
    } else {
        unsafe { wio::GetOverlappedResult(handle, ovp, &raw mut transferred, 0) }
    };
    let err = if res != 0 {
        ERROR_SUCCESS
    } else {
        unsafe { fnd::GetLastError() }
    };
    match err {
        ERROR_SUCCESS | ERROR_MORE_DATA | ERROR_OPERATION_ABORTED => {
            let mut reg = overlapped_registry().lock().unwrap();
            if let Some(st) = reg.get_mut(&id) {
                st.completed = true;
                st.pending = false;
                // For a completed read, the buffer shrinks to the count the
                // kernel actually delivered (CPython `_PyBytes_Resize`).
                if !st.is_write {
                    if let Some(b) = st.buffer.as_mut() {
                        b.truncate(transferred as usize);
                    }
                }
            }
        }
        ERROR_IO_INCOMPLETE => {}
        _ => {
            if let Some(st) = overlapped_registry().lock().unwrap().get_mut(&id) {
                st.pending = false;
            }
            return Err(nt_support::win32_error_to_py(err as i32, None));
        }
    }
    Ok(Object::new_tuple(vec![
        Object::Int(i64::from(transferred)),
        Object::Int(i64::from(err)),
    ]))
}

fn overlapped_getbuffer(args: &[Object]) -> Result<Object, RuntimeError> {
    let id = overlapped_id(args)?;
    let reg = overlapped_registry().lock().unwrap();
    let st = reg
        .get(&id)
        .ok_or_else(|| value_error("Overlapped object is closed"))?;
    // Only meaningful after a completed read; None otherwise (CPython).
    match (&st.buffer, st.is_write, st.completed) {
        (Some(b), false, true) => Ok(Object::new_bytes(b.clone())),
        _ => Ok(Object::None),
    }
}

fn overlapped_cancel(args: &[Object]) -> Result<Object, RuntimeError> {
    let id = overlapped_id(args)?;
    let mut reg = overlapped_registry().lock().unwrap();
    if let Some(st) = reg.get_mut(&id) {
        if st.pending && !st.completed {
            // ERROR_NOT_FOUND means the op already finished — not an error.
            let ok =
                unsafe { wio::CancelIoEx(st.handle as HANDLE, st.ov as *const wio::OVERLAPPED) };
            if ok == 0 {
                let err = unsafe { fnd::GetLastError() };
                if err != ERROR_NOT_FOUND {
                    return Err(nt_support::win32_error_to_py(err as i32, None));
                }
            }
        }
    }
    Ok(Object::None)
}

fn overlapped_del(args: &[Object]) -> Result<Object, RuntimeError> {
    let id = match overlapped_id(args) {
        Ok(id) => id,
        Err(_) => return Ok(Object::None),
    };
    let st = overlapped_registry().lock().unwrap().remove(&id);
    if let Some(st) = st {
        // A still-pending op owns a buffer the kernel may write to; cancel
        // and drain to completion before freeing, so we never release
        // kernel-owned memory (CPython's dealloc does the same wait).
        if st.pending && !st.completed {
            unsafe {
                wio::CancelIoEx(st.handle as HANDLE, st.ov as *const wio::OVERLAPPED);
                thr::WaitForSingleObject(st.event as HANDLE, INFINITE);
            }
        }
        free_overlapped_state(st);
    }
    Ok(Object::None)
}

// ---------------------------------------------------------------------------
// Constant table (winbase.h / winnt.h / handleapi.h values). Published as
// plain module ints — CPython's `_winapi` exposes exactly these.
// ---------------------------------------------------------------------------

fn constants() -> Vec<(&'static str, i64)> {
    vec![
        // Process priority classes.
        ("ABOVE_NORMAL_PRIORITY_CLASS", 0x0000_8000),
        ("BELOW_NORMAL_PRIORITY_CLASS", 0x0000_4000),
        ("HIGH_PRIORITY_CLASS", 0x0000_0080),
        ("IDLE_PRIORITY_CLASS", 0x0000_0040),
        ("NORMAL_PRIORITY_CLASS", 0x0000_0020),
        ("REALTIME_PRIORITY_CLASS", 0x0000_0100),
        // Creation flags.
        ("CREATE_BREAKAWAY_FROM_JOB", 0x0100_0000),
        ("CREATE_DEFAULT_ERROR_MODE", 0x0400_0000),
        ("CREATE_NO_WINDOW", 0x0800_0000),
        ("CREATE_NEW_CONSOLE", 0x0000_0010),
        ("CREATE_NEW_PROCESS_GROUP", 0x0000_0200),
        ("CREATE_UNICODE_ENVIRONMENT", 0x0000_0400),
        ("DETACHED_PROCESS", 0x0000_0008),
        ("STARTF_USESHOWWINDOW", 0x0000_0001),
        ("STARTF_USESTDHANDLES", 0x0000_0100),
        ("STARTF_FORCEONFEEDBACK", 0x0000_0040),
        ("STARTF_FORCEOFFFEEDBACK", 0x0000_0080),
        // Handle duplication.
        ("DUPLICATE_CLOSE_SOURCE", 0x0000_0001),
        ("DUPLICATE_SAME_ACCESS", 0x0000_0002),
        // Win32 error codes.
        ("ERROR_ALREADY_EXISTS", 183),
        ("ERROR_BROKEN_PIPE", 109),
        ("ERROR_IO_PENDING", 997),
        ("ERROR_MORE_DATA", 234),
        ("ERROR_NETNAME_DELETED", 64),
        ("ERROR_NO_DATA", 232),
        ("ERROR_NO_SYSTEM_RESOURCES", 1450),
        ("ERROR_OPERATION_ABORTED", 995),
        ("ERROR_PIPE_BUSY", 231),
        ("ERROR_PIPE_CONNECTED", 535),
        ("ERROR_SEM_TIMEOUT", 121),
        // File flags and access.
        ("FILE_FLAG_FIRST_PIPE_INSTANCE", 0x0008_0000),
        ("FILE_FLAG_OVERLAPPED", 0x4000_0000),
        ("FILE_GENERIC_READ", 0x0012_0089),
        ("FILE_GENERIC_WRITE", 0x0012_0116),
        ("FILE_MAP_ALL_ACCESS", 0x000F_001F),
        ("FILE_MAP_COPY", 0x0000_0001),
        ("FILE_MAP_EXECUTE", 0x0000_0020),
        ("FILE_MAP_READ", 0x0000_0004),
        ("FILE_MAP_WRITE", 0x0000_0002),
        ("FILE_TYPE_CHAR", 0x0002),
        ("FILE_TYPE_DISK", 0x0001),
        ("FILE_TYPE_PIPE", 0x0003),
        ("FILE_TYPE_REMOTE", 0x8000),
        ("FILE_TYPE_UNKNOWN", 0x0000),
        ("GENERIC_READ", 0x8000_0000),
        ("GENERIC_WRITE", 0x4000_0000),
        ("INFINITE", 0xFFFF_FFFF),
        // Memory / section flags.
        ("MEM_COMMIT", 0x0000_1000),
        ("MEM_FREE", 0x0001_0000),
        ("MEM_IMAGE", 0x0100_0000),
        ("MEM_MAPPED", 0x0004_0000),
        ("MEM_PRIVATE", 0x0002_0000),
        ("MEM_RESERVE", 0x0000_2000),
        ("NMPWAIT_WAIT_FOREVER", 0xFFFF_FFFF),
        ("NULL", 0),
        ("OPEN_EXISTING", 3),
        ("PAGE_NOACCESS", 0x01),
        ("PAGE_READONLY", 0x02),
        ("PAGE_READWRITE", 0x04),
        ("PAGE_WRITECOPY", 0x08),
        ("PAGE_EXECUTE", 0x10),
        ("PAGE_EXECUTE_READ", 0x20),
        ("PAGE_EXECUTE_READWRITE", 0x40),
        ("PAGE_EXECUTE_WRITECOPY", 0x80),
        ("PAGE_GUARD", 0x100),
        ("PAGE_NOCACHE", 0x200),
        ("PAGE_WRITECOMBINE", 0x400),
        // Named-pipe modes.
        ("PIPE_ACCESS_DUPLEX", 0x0000_0003),
        ("PIPE_ACCESS_INBOUND", 0x0000_0001),
        ("PIPE_ACCESS_OUTBOUND", 0x0000_0002),
        ("PIPE_READMODE_BYTE", 0x0000_0000),
        ("PIPE_READMODE_MESSAGE", 0x0000_0002),
        ("PIPE_TYPE_BYTE", 0x0000_0000),
        ("PIPE_TYPE_MESSAGE", 0x0000_0004),
        ("PIPE_UNLIMITED_INSTANCES", 255),
        ("PIPE_WAIT", 0x0000_0000),
        ("PIPE_NOWAIT", 0x0000_0001),
        // Process access rights.
        ("PROCESS_ALL_ACCESS", 0x001F_FFFF),
        ("PROCESS_DUP_HANDLE", 0x0040),
        // Section attributes.
        ("SEC_COMMIT", 0x0800_0000),
        ("SEC_IMAGE", 0x0100_0000),
        ("SEC_IMAGE_NO_EXECUTE", 0x1100_0000),
        ("SEC_LARGE_PAGES", 0x8000_0000),
        ("SEC_NOCACHE", 0x1000_0000),
        ("SEC_RESERVE", 0x0400_0000),
        ("SEC_WRITECOMBINE", 0x4000_0000),
        // Standard-handle selectors (unsigned DWORDs).
        ("STD_ERROR_HANDLE", 0xFFFF_FFF4),
        ("STD_INPUT_HANDLE", 0xFFFF_FFF6),
        ("STD_OUTPUT_HANDLE", 0xFFFF_FFF5),
        ("STILL_ACTIVE", 259),
        ("SW_HIDE", 0),
        ("SYNCHRONIZE", 0x0010_0000),
        ("WAIT_ABANDONED_0", 128),
        ("WAIT_OBJECT_0", 0),
        ("WAIT_TIMEOUT", 258),
        ("WAIT_FAILED", 0xFFFF_FFFF),
        // LCMapStringEx flags.
        ("LCMAP_FULLWIDTH", 0x0080_0000),
        ("LCMAP_HALFWIDTH", 0x0040_0000),
        ("LCMAP_HIRAGANA", 0x0010_0000),
        ("LCMAP_KATAKANA", 0x0020_0000),
        ("LCMAP_LINGUISTIC_CASING", 0x0100_0000),
        ("LCMAP_LOWERCASE", 0x0000_0100),
        ("LCMAP_SIMPLIFIED_CHINESE", 0x0200_0000),
        ("LCMAP_TITLECASE", 0x0000_0300),
        ("LCMAP_TRADITIONAL_CHINESE", 0x0400_0000),
        ("LCMAP_UPPERCASE", 0x0000_0200),
        ("LOCALE_NAME_MAX_LENGTH", 85),
        // COPY_FILE_* flags shutil's fast-copy path passes to CopyFile2.
        ("COPY_FILE_ALLOW_DECRYPTED_DESTINATION", 0x0000_0008),
        ("COPY_FILE_COPY_SYMLINK", 0x0000_0800),
        ("COPY_FILE_DIRECTORY", 0x0000_0080),
        ("COPY_FILE_FAIL_IF_EXISTS", 0x0000_0001),
        ("COPY_FILE_NO_BUFFERING", 0x0000_1000),
        ("COPY_FILE_NO_OFFLOAD", 0x0004_0000),
        ("COPY_FILE_OPEN_SOURCE_FOR_WRITE", 0x0000_0004),
        ("COPY_FILE_REQUEST_COMPRESSED_TRAFFIC", 0x1000_0000),
        ("COPY_FILE_REQUEST_SECURITY_PRIVILEGES", 0x0000_2000),
        ("COPY_FILE_RESTARTABLE", 0x0000_0002),
        ("COPY_FILE_RESUME_FROM_PAUSE", 0x0000_4000),
    ]
}

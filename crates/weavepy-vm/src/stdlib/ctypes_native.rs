//! `_ctypes_native` — the low-level primitive layer behind WeavePy's
//! frozen `_ctypes` reimplementation (which in turn backs the verbatim
//! CPython `ctypes` package).
//!
//! CPython's `_ctypes` is a *core-built* C extension (it links against
//! `_PyRuntime` and other private interpreter internals), so the host
//! `_ctypes.cpython-313-*.so` cannot be `dlopen`'d into WeavePy the way a
//! stable-ABI wheel (numpy/pandas) can. We therefore reimplement the
//! `_ctypes` contract natively. The split mirrors CPython's own
//! `Lib/ctypes` (Python) over `_ctypes` (C):
//!
//! * **This module** owns the genuinely-native pieces: the platform C type
//!   sizes/alignments, raw memory peek/poke, `dlopen`/`dlsym`, the libc
//!   `memmove`/`memset`/`string_at` block helpers, the ctypes private
//!   errno, and (RFC: wave 5 FFI) the libffi call/closure bridge.
//! * The frozen `python/_ctypes.py` builds the `_SimpleCData`/`Structure`/
//!   `Union`/`Array`/`_Pointer`/`CFuncPtr` type system + metaclasses on top
//!   of these primitives, exposing exactly the names `ctypes/__init__.py`
//!   imports.
//!
//! Memory model: a ctypes object's storage is a Python `bytearray` (owned,
//! GC'd, address-stable while its length is fixed — ctypes objects never
//! resize except via `resize()`); views (struct fields, array elements,
//! `from_buffer`) share that `bytearray` at an offset. External memory
//! (`from_address`, pointer deref, FFI return pointers) is addressed by a
//! raw integer through [`read_mem`]/[`write_mem`]. `addressof_buffer`
//! returns the `bytearray`'s stable data pointer so the two worlds unify
//! on a single `void *`.

use std::cell::Cell;
use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int, c_void};

use crate::error::{type_error, value_error, RuntimeError};
use crate::import::ModuleCache;
use crate::object::{BuiltinFn, DictData, DictKey, Object, PyModule};
use crate::sync::Rc;
use crate::sync::RefCell;

mod ffi;

// ----------------------------------------------------------------
// Argument helpers
// ----------------------------------------------------------------

fn arg(args: &[Object], i: usize) -> Result<&Object, RuntimeError> {
    args.get(i)
        .ok_or_else(|| type_error(format!("_ctypes: missing argument {i}")))
}

fn arg_usize(args: &[Object], i: usize) -> Result<usize, RuntimeError> {
    arg(args, i)?
        .as_usize()
        .ok_or_else(|| type_error(format!("_ctypes: argument {i} must be a non-negative int")))
}

fn arg_i64(args: &[Object], i: usize) -> Result<i64, RuntimeError> {
    arg(args, i)?
        .as_i64()
        .ok_or_else(|| type_error(format!("_ctypes: argument {i} must be an int")))
}

fn arg_str(args: &[Object], i: usize) -> Result<String, RuntimeError> {
    match arg(args, i)? {
        Object::Str(s) => Ok(s.to_string()),
        other => Err(type_error(format!(
            "_ctypes: argument {i} must be str (got '{}')",
            other.type_name()
        ))),
    }
}

/// Build a Python int from a (possibly > i64::MAX) machine address.
fn addr_obj(v: usize) -> Object {
    Object::int_from_i128(v as i128)
}

// ----------------------------------------------------------------
// Platform C type sizes / alignments
// ----------------------------------------------------------------

/// `(size, align)` for a ctypes `_type_` format code, using the real
/// platform C ABI (so a `Structure` laid out here matches what a loaded
/// extension's C struct expects). Returns `None` for an unknown code.
fn code_info(code: char) -> Option<(usize, usize)> {
    use std::mem::{align_of, size_of};
    let p = (size_of::<*const c_void>(), align_of::<*const c_void>());
    Some(match code {
        // signed/unsigned char, bool, char
        'c' | 'b' | 'B' | '?' => (1, 1),
        // short (`'v'` is HRESULT-era VARIANT_BOOL: a 16-bit short)
        'h' | 'H' | 'v' => (size_of::<libc::c_short>(), align_of::<libc::c_short>()),
        // int
        'i' | 'I' => (size_of::<libc::c_int>(), align_of::<libc::c_int>()),
        // long
        'l' | 'L' => (size_of::<libc::c_long>(), align_of::<libc::c_long>()),
        // long long
        'q' | 'Q' => (
            size_of::<libc::c_longlong>(),
            align_of::<libc::c_longlong>(),
        ),
        // float / double
        'f' => (size_of::<f32>(), align_of::<f32>()),
        'd' => (size_of::<f64>(), align_of::<f64>()),
        // long double — platform dependent. Apple silicon and 32-bit ARM
        // use 64-bit long double (== double); x86 uses the 80-bit extended
        // type stored in 12 (i386) / 16 (x86-64) bytes.
        'g' => long_double_info(),
        // pointers: void*, char*, wchar_t*, py_object (PyObject*)
        'P' | 'z' | 'Z' | 'O' => p,
        // wchar_t: 4 bytes on POSIX, 2 on Windows.
        'u' => wchar_info(),
        _ => return None,
    })
}

#[cfg(windows)]
fn long_double_info() -> (usize, usize) {
    // MSVC defines `long double` == `double` on every architecture, and
    // that is the ABI of the system DLLs and of CPython on Windows
    // (`sizeof(c_longdouble) == 8` there). mingw-gcc's 80-bit long double
    // is a different, non-system ABI we deliberately don't model.
    (8, 8)
}
#[cfg(all(not(windows), target_arch = "x86_64"))]
fn long_double_info() -> (usize, usize) {
    (16, 16)
}
#[cfg(all(not(windows), target_arch = "x86"))]
fn long_double_info() -> (usize, usize) {
    (12, 4)
}
#[cfg(all(not(windows), not(any(target_arch = "x86_64", target_arch = "x86"))))]
fn long_double_info() -> (usize, usize) {
    // aarch64 (incl. Apple silicon), arm, etc.: long double == double.
    (8, 8)
}

#[cfg(windows)]
fn wchar_info() -> (usize, usize) {
    (2, 2)
}
#[cfg(not(windows))]
fn wchar_info() -> (usize, usize) {
    (4, 4)
}

fn b_sizeof_code(args: &[Object]) -> Result<Object, RuntimeError> {
    let code = arg_str(args, 0)?;
    let c = code
        .chars()
        .next()
        .ok_or_else(|| value_error("empty type code"))?;
    let (size, _) = code_info(c).ok_or_else(|| value_error(format!("unknown type code {c:?}")))?;
    Ok(Object::Int(size as i64))
}

fn b_alignment_code(args: &[Object]) -> Result<Object, RuntimeError> {
    let code = arg_str(args, 0)?;
    let c = code
        .chars()
        .next()
        .ok_or_else(|| value_error("empty type code"))?;
    let (_, align) = code_info(c).ok_or_else(|| value_error(format!("unknown type code {c:?}")))?;
    Ok(Object::Int(align as i64))
}

// ----------------------------------------------------------------
// Raw memory
// ----------------------------------------------------------------

/// Stable data pointer of a `bytearray`'s backing buffer. The buffer does
/// not move while its length is fixed, so the returned address is valid as
/// long as the `bytearray` is alive and unresized.
fn b_addressof_buffer(args: &[Object]) -> Result<Object, RuntimeError> {
    match arg(args, 0)? {
        Object::ByteArray(rc) => {
            let ptr = rc.borrow().as_ptr() as usize;
            Ok(addr_obj(ptr))
        }
        Object::Bytes(b) => Ok(addr_obj(b.as_ptr() as usize)),
        // A zero-copy `from_buffer(memoryview)` keeps the view itself as the
        // ctypes object's backing store (see frozen `_ctypes._writable_buffer`),
        // so `ctypes.addressof` must resolve through it. `mmap`/`shared_memory`
        // regions have a stable pointer for their lifetime; bytearray-backed
        // views hold a live export that pins the buffer against resizing.
        Object::MemoryView(mv) => {
            use crate::object::MemoryViewBuffer;
            if mv.released.get() {
                return Err(value_error(
                    "addressof_buffer: operation forbidden on released memoryview object",
                ));
            }
            let base = match &mv.buffer {
                MemoryViewBuffer::Bytes(b) => b.as_ptr() as usize,
                MemoryViewBuffer::ByteArray(rc) => rc.borrow().as_ptr() as usize,
                MemoryViewBuffer::Shared(buf) => buf.data_ptr() as usize,
            };
            Ok(addr_obj(base + mv.start.get()))
        }
        other => Err(type_error(format!(
            "addressof_buffer: expected bytearray (got '{}')",
            other.type_name()
        ))),
    }
}

/// `intern_buffer(data) -> addr` — a process-lifetime copy of `data`
/// (already NUL-terminated by the caller), deduplicated by content.
/// Backs `c_char_p`/`c_wchar_p` argument marshalling: CPython passes a
/// pointer into the bytes/str object's own buffer, which callees may
/// legally stash past the call (lxml's `adopt_external_document`
/// `strcmp`s a capsule context set by an earlier `PyCapsule_SetContext`);
/// a per-call shadow buffer dangles for that pattern.
fn b_intern_buffer(args: &[Object]) -> Result<Object, RuntimeError> {
    let data = arg(args, 0)?
        .as_bytes_view()
        .ok_or_else(|| type_error("intern_buffer: bytes-like expected"))?;
    Ok(addr_obj(ffi::interned_cstr(data)))
}

/// `read_mem(addr, n) -> bytes` — copy `n` bytes from raw memory.
fn b_read_mem(args: &[Object]) -> Result<Object, RuntimeError> {
    let addr = arg_usize(args, 0)?;
    let n = arg_usize(args, 1)?;
    if addr == 0 {
        return Err(value_error("read_mem: NULL pointer access"));
    }
    let slice = unsafe { std::slice::from_raw_parts(addr as *const u8, n) };
    Ok(Object::new_bytes(slice.to_vec()))
}

/// `write_mem(addr, data)` — copy `data` into raw memory.
fn b_write_mem(args: &[Object]) -> Result<Object, RuntimeError> {
    let addr = arg_usize(args, 0)?;
    let data = arg(args, 1)?
        .as_bytes_view()
        .ok_or_else(|| type_error("write_mem: data must be bytes-like"))?;
    if addr == 0 && !data.is_empty() {
        return Err(value_error("write_mem: NULL pointer access"));
    }
    unsafe {
        std::ptr::copy(data.as_ptr(), addr as *mut u8, data.len());
    }
    Ok(Object::None)
}

fn b_memmove(args: &[Object]) -> Result<Object, RuntimeError> {
    let dst = arg_usize(args, 0)?;
    let src = arg_usize(args, 1)?;
    let n = arg_usize(args, 2)?;
    unsafe {
        libc::memmove(dst as *mut c_void, src as *const c_void, n);
    }
    Ok(addr_obj(dst))
}

fn b_memset(args: &[Object]) -> Result<Object, RuntimeError> {
    let dst = arg_usize(args, 0)?;
    let c = arg_i64(args, 1)? as c_int;
    let n = arg_usize(args, 2)?;
    unsafe {
        libc::memset(dst as *mut c_void, c, n);
    }
    Ok(addr_obj(dst))
}

/// `string_at(addr, size=-1) -> bytes`. With `size < 0`, reads up to the
/// first NUL (C string semantics).
fn b_string_at(args: &[Object]) -> Result<Object, RuntimeError> {
    let addr = arg_usize(args, 0)?;
    let size = args.get(1).and_then(Object::as_i64).unwrap_or(-1);
    if addr == 0 {
        return Err(value_error("string_at: NULL pointer access"));
    }
    let bytes = if size < 0 {
        let c = unsafe { CStr::from_ptr(addr as *const c_char) };
        c.to_bytes().to_vec()
    } else {
        let slice = unsafe { std::slice::from_raw_parts(addr as *const u8, size as usize) };
        slice.to_vec()
    };
    Ok(Object::new_bytes(bytes))
}

/// `wstring_at(addr, size=-1) -> str`. `wchar_t` is 4 bytes on POSIX.
fn b_wstring_at(args: &[Object]) -> Result<Object, RuntimeError> {
    let addr = arg_usize(args, 0)?;
    let size = args.get(1).and_then(Object::as_i64).unwrap_or(-1);
    if addr == 0 {
        return Err(value_error("wstring_at: NULL pointer access"));
    }
    let (wsize, _) = wchar_info();
    let mut s = String::new();
    let mut p = addr;
    let mut count = 0i64;
    loop {
        if size >= 0 && count >= size {
            break;
        }
        let cp: u32 = if wsize == 4 {
            unsafe { *(p as *const u32) }
        } else {
            unsafe { u32::from(*(p as *const u16)) }
        };
        if size < 0 && cp == 0 {
            break;
        }
        if let Some(ch) = char::from_u32(cp) {
            s.push(ch);
        } else {
            s.push('\u{fffd}');
        }
        p += wsize;
        count += 1;
    }
    Ok(Object::from_str(s))
}

// ----------------------------------------------------------------
// dlopen / dlsym
// ----------------------------------------------------------------

/// `RTLD_*` mode constants exposed to the frozen `_ctypes.py`. On Windows
/// (where library loading goes through `LoadLibrary` and modes don't apply)
/// they are all zero, matching CPython's `ctypes` there.
#[cfg(unix)]
mod rtld {
    pub(super) const LOCAL: i64 = libc::RTLD_LOCAL as i64;
    pub(super) const GLOBAL: i64 = libc::RTLD_GLOBAL as i64;
    pub(super) const NOW: i64 = libc::RTLD_NOW as i64;
    pub(super) const LAZY: i64 = libc::RTLD_LAZY as i64;
}
#[cfg(not(unix))]
mod rtld {
    pub(super) const LOCAL: i64 = 0;
    pub(super) const GLOBAL: i64 = 0;
    pub(super) const NOW: i64 = 0;
    pub(super) const LAZY: i64 = 0;
}

// The NT loader is natively wide: the `W` entry points take the path
// as-is, while the `A` variants round-trip it through the ANSI code page
// and mangle anything outside it. CPython's `load_library` (callproc.c)
// is `LoadLibraryExW`-based for the same reason. Symbol *names* stay
// ANSI — `GetProcAddress` has no wide variant.
#[cfg(windows)]
extern "system" {
    fn LoadLibraryW(name: *const u16) -> *mut c_void;
    fn GetModuleHandleW(name: *const u16) -> *mut c_void;
    fn GetProcAddress(handle: *mut c_void, name: *const c_char) -> *mut c_void;
    fn FreeLibrary(handle: *mut c_void) -> i32;
    fn GetLastError() -> u32;
}

fn b_dlopen(args: &[Object]) -> Result<Object, RuntimeError> {
    // CPython's `_ctypes.c` ORs RTLD_NOW into every dlopen mode: ctypes'
    // DEFAULT_MODE is RTLD_LOCAL, which is 0 on Linux, and glibc rejects a
    // mode carrying neither RTLD_LAZY nor RTLD_NOW with EINVAL ("invalid
    // mode for dlopen()") — `import ctypes` itself dies at
    // `pythonapi = PyDLL(None)` without this. (macOS tolerates 0.)
    #[cfg(unix)]
    let mode =
        args.get(1).and_then(Object::as_i64).unwrap_or(rtld::LOCAL) as c_int | rtld::NOW as c_int;
    let handle = match arg(args, 0)? {
        Object::None => {
            #[cfg(unix)]
            unsafe {
                libc::dlopen(std::ptr::null(), mode)
            }
            #[cfg(windows)]
            unsafe {
                GetModuleHandleW(std::ptr::null())
            }
        }
        Object::Str(s) => {
            #[cfg(unix)]
            {
                let cname = CString::new(s.as_bytes())
                    .map_err(|_| value_error("dlopen: embedded NUL in name"))?;
                unsafe { libc::dlopen(cname.as_ptr(), mode) }
            }
            #[cfg(windows)]
            {
                let wname = super::nt_support::wide(s);
                unsafe { LoadLibraryW(wname.as_ptr()) }
            }
        }
        // CPython accepts a bytes path and hands it to dlopen() verbatim
        // (no decoding) — test_dlerror exercises undecodable names.
        Object::Bytes(b) => {
            #[cfg(unix)]
            {
                let cname = CString::new(b.to_vec())
                    .map_err(|_| value_error("dlopen: embedded NUL in name"))?;
                unsafe { libc::dlopen(cname.as_ptr(), mode) }
            }
            #[cfg(windows)]
            {
                // No byte-path concept exists on NT (CPython's Windows
                // `LoadLibrary` requires str); decode lossily into the
                // wide API rather than reject outright.
                let wname = super::nt_support::wide(&String::from_utf8_lossy(b));
                unsafe { LoadLibraryW(wname.as_ptr()) }
            }
        }
        other => {
            return Err(type_error(format!(
                "dlopen: name must be str or None (got '{}')",
                other.type_name()
            )))
        }
    };
    if handle.is_null() {
        let msg = last_dlerror().unwrap_or_else(|| "dlopen failed".to_owned());
        return Err(os_error(msg));
    }
    Ok(addr_obj(handle as usize))
}

fn b_dlsym(args: &[Object]) -> Result<Object, RuntimeError> {
    let handle = arg_usize(args, 0)?;
    let name = arg_str(args, 1)?;
    let cname =
        CString::new(name.as_bytes()).map_err(|_| value_error("dlsym: embedded NUL in name"))?;
    // Clear any stale error first (dlsym returning NULL is ambiguous).
    #[cfg(unix)]
    unsafe {
        libc::dlerror()
    };
    #[cfg(unix)]
    let sym = unsafe { libc::dlsym(handle as *mut c_void, cname.as_ptr()) };
    #[cfg(windows)]
    let sym = unsafe { GetProcAddress(handle as *mut c_void, cname.as_ptr()) };
    if sym.is_null() {
        // CPython's `_ctypes.dlsym` raises OSError carrying the dlerror()
        // text (callers map it: CDLL attribute -> AttributeError, in_dll ->
        // ValueError). A NULL result with no pending dlerror (GNU IFUNC
        // resolving to NULL) is still an error since gh-126554.
        let msg = last_dlerror().unwrap_or_else(|| format!("symbol {name:?} not found"));
        return Err(os_error(msg));
    }
    Ok(addr_obj(sym as usize))
}

fn b_dlclose(args: &[Object]) -> Result<Object, RuntimeError> {
    let handle = arg_usize(args, 0)?;
    #[cfg(unix)]
    let rc = unsafe { libc::dlclose(handle as *mut c_void) };
    #[cfg(windows)]
    // `FreeLibrary` returns nonzero on success; map to dlclose's 0-on-success.
    let rc = i32::from(unsafe { FreeLibrary(handle as *mut c_void) } == 0);
    Ok(Object::Int(i64::from(rc)))
}

fn b_dlerror(_args: &[Object]) -> Result<Object, RuntimeError> {
    match last_dlerror() {
        Some(s) => Ok(Object::from_str(s)),
        None => Ok(Object::None),
    }
}

#[cfg(unix)]
fn last_dlerror() -> Option<String> {
    let p = unsafe { libc::dlerror() };
    if p.is_null() {
        None
    } else {
        Some(unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned())
    }
}

#[cfg(windows)]
fn last_dlerror() -> Option<String> {
    // `FormatMessageW` text, the same strerror source the rest of the NT
    // runtime uses — CPython's ctypes shows e.g. "Could not find module
    // '...'" here, not a bare error number.
    match unsafe { GetLastError() } {
        0 => None,
        code => Some(super::nt_support::format_message(code as i32)),
    }
}

fn os_error(msg: impl Into<String>) -> RuntimeError {
    RuntimeError::PyException(crate::error::PyException::from_builtin(
        "OSError",
        msg.into(),
    ))
}

// ----------------------------------------------------------------
// ctypes private errno (per RFC: swapped around USE_ERRNO calls)
// ----------------------------------------------------------------

thread_local! {
    static CTYPES_ERRNO: Cell<i32> = const { Cell::new(0) };
}

fn b_get_errno(_args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(Object::Int(i64::from(CTYPES_ERRNO.with(|e| e.get()))))
}

fn b_set_errno(args: &[Object]) -> Result<Object, RuntimeError> {
    let new = arg_i64(args, 0)? as i32;
    let old = CTYPES_ERRNO.with(|e| e.replace(new));
    Ok(Object::Int(i64::from(old)))
}

/// Atomically read-and-replace ctypes' private per-thread errno, returning
/// the previous value. Used by the libffi bridge's `USE_ERRNO` swap
/// (see `ffi::swap_ctypes_errno`).
pub(super) fn ctypes_errno_replace(new: i32) -> i32 {
    CTYPES_ERRNO.with(|e| e.replace(new))
}

// ----------------------------------------------------------------
// ctypes private LastError (Windows; swapped around USE_LASTERROR calls)
// ----------------------------------------------------------------
//
// The exactly-parallel mechanism to the private errno above: CPython
// keeps both in one per-thread array (Modules/_ctypes/callproc.c
// `_ctypes_get_errobj` — errno in `space[0]`, LastError in `space[1]`),
// and `get_last_error`/`set_last_error` read/write the *private* copy,
// never the thread's real `GetLastError()` state.

#[cfg(windows)]
thread_local! {
    static CTYPES_LAST_ERROR: Cell<u32> = const { Cell::new(0) };
}

#[cfg(windows)]
fn b_get_last_error(_args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(Object::Int(i64::from(CTYPES_LAST_ERROR.with(|e| e.get()))))
}

#[cfg(windows)]
fn b_set_last_error(args: &[Object]) -> Result<Object, RuntimeError> {
    // CPython parses the new value as an unsigned DWORD ("I"); negative
    // Python ints arrive here two's-complement-truncated, matching that.
    let new = arg_i64(args, 0)? as u32;
    let old = CTYPES_LAST_ERROR.with(|e| e.replace(new));
    Ok(Object::Int(i64::from(old)))
}

/// Atomically read-and-replace ctypes' private per-thread LastError,
/// returning the previous value. Used by the FFI bridge's `USE_LASTERROR`
/// swap (see `ffi::swap_ctypes_last_error`).
#[cfg(windows)]
pub(super) fn ctypes_last_error_replace(new: u32) -> u32 {
    CTYPES_LAST_ERROR.with(|e| e.replace(new))
}

/// `format_error(code_or_None) -> str` — the message text for a Win32
/// error code; with `None`, the calling thread's *real* `GetLastError()`
/// (CPython's `format_error`, callproc.c — note the asymmetry with
/// `get_last_error`, which reads the ctypes-private copy).
#[cfg(windows)]
fn b_format_error(args: &[Object]) -> Result<Object, RuntimeError> {
    let code = match args.first() {
        None | Some(Object::None) => (unsafe { GetLastError() }) as i32,
        Some(o) => o
            .as_i64()
            .ok_or_else(|| type_error("format_error: code must be an int or None"))?
            as i32,
    };
    Ok(Object::from_str(super::nt_support::format_message(code)))
}

// ----------------------------------------------------------------
// PEP 3118 view configuration (_ctypes `PyCData_NewGetBuffer`)
// ----------------------------------------------------------------

/// `configure_view(mv, format, itemsize, shape_or_none)` — stamp the PEP
/// 3118 metadata computed by the frozen `_ctypes.py` onto a freshly
/// exported memoryview. `shape_or_none` is `None` for a 0-dimensional
/// (scalar) export, or a sequence of dimension extents for arrays.
fn b_configure_view(args: &[Object]) -> Result<Object, RuntimeError> {
    let Object::MemoryView(mv) = arg(args, 0)? else {
        return Err(type_error("configure_view: expected memoryview"));
    };
    let fmt = arg_str(args, 1)?;
    let itemsize = arg_usize(args, 2)?;
    *mv.format.borrow_mut() = fmt;
    mv.itemsize.set(itemsize);
    mv.strides.borrow_mut().clear();
    match arg(args, 3)? {
        Object::None => {
            mv.zero_dim.set(true);
            mv.shape.borrow_mut().clear();
        }
        Object::Tuple(dims) => {
            let mut shape = Vec::with_capacity(dims.len());
            for d in dims.iter() {
                shape.push(d.as_usize().ok_or_else(|| {
                    type_error("configure_view: shape entries must be non-negative ints")
                })?);
            }
            mv.zero_dim.set(false);
            *mv.shape.borrow_mut() = shape;
        }
        other => {
            return Err(type_error(format!(
                "configure_view: shape must be tuple or None (got '{}')",
                other.type_name()
            )))
        }
    }
    Ok(Object::None)
}

// ----------------------------------------------------------------
// Unraisable-exception reporting (callbacks.c `_PyErr_WriteUnraisableMsg`)
// ----------------------------------------------------------------

/// `unraisable(exc, err_msg)` — route an exception instance through the
/// interpreter's `sys.unraisablehook` machinery with an explicit message.
/// The frozen `_ctypes.py` calls this from the closure trampoline when a
/// user callback raises (the exception cannot propagate into C).
pub(super) fn b_unraisable(args: &[Object]) -> Result<Object, RuntimeError> {
    let exc = arg(args, 0)?.clone();
    let msg = arg_str(args, 1)?;
    if let Some(ptr) = crate::vm_singletons::current_interpreter_ptr() {
        // SAFETY: published by an enclosing VM frame still live on this
        // thread; the GIL keeps the access exclusive.
        let interp = unsafe { &mut *ptr };
        let err = RuntimeError::PyException(crate::error::PyException::new(exc));
        interp.write_unraisable_msg(&err, &Object::None, &msg, Some(&msg));
    }
    Ok(Object::None)
}

// ----------------------------------------------------------------
// macOS dyld shared cache probe (_ctypes/callproc.c)
// ----------------------------------------------------------------

/// `dyld_shared_cache_contains_path(path)` — true when `path` names a
/// dylib baked into the macOS dyld shared cache (such libraries have no
/// on-disk file, so `os.path.exists` cannot find them).
#[cfg(target_os = "macos")]
pub(super) fn b_dyld_shared_cache_contains_path(args: &[Object]) -> Result<Object, RuntimeError> {
    let path = arg_str(args, 0)?;
    // Resolved lazily via dlsym: the symbol exists on macOS 11+ only.
    type ProbeFn = unsafe extern "C" fn(*const c_char) -> bool;
    static PROBE: std::sync::OnceLock<Option<ProbeFn>> = std::sync::OnceLock::new();
    let probe = PROBE.get_or_init(|| {
        let name = CString::new("_dyld_shared_cache_contains_path").unwrap();
        let sym = unsafe { libc::dlsym(libc::RTLD_DEFAULT, name.as_ptr()) };
        if sym.is_null() {
            None
        } else {
            Some(unsafe { std::mem::transmute::<*mut c_void, ProbeFn>(sym) })
        }
    });
    match probe {
        Some(f) => {
            let c_path =
                CString::new(path).map_err(|_| value_error("embedded null byte".to_owned()))?;
            Ok(Object::Bool(unsafe { f(c_path.as_ptr()) }))
        }
        None => Err(RuntimeError::PyException(
            crate::error::PyException::from_builtin(
                "NotImplementedError",
                "_dyld_shared_cache_contains_path symbol is missing",
            ),
        )),
    }
}

// ----------------------------------------------------------------
// Registration
// ----------------------------------------------------------------

fn register(
    d: &mut DictData,
    name: &'static str,
    body: impl Fn(&[Object]) -> Result<Object, RuntimeError> + Send + Sync + 'static,
) {
    d.insert(
        DictKey(Object::from_static(name)),
        Object::Builtin(Rc::new(BuiltinFn {
            name,
            binds_instance: false,
            call: Box::new(body),
            call_kw: None,
        })),
    );
}

pub fn build(_cache: &ModuleCache) -> Rc<PyModule> {
    let dict = Rc::new(RefCell::new(DictData::default()));
    {
        let mut d = dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("__name__")),
            Object::from_static("_ctypes_native"),
        );
        // Platform constants.
        for (n, v) in [
            ("RTLD_LOCAL", rtld::LOCAL),
            ("RTLD_GLOBAL", rtld::GLOBAL),
            ("RTLD_NOW", rtld::NOW),
            ("RTLD_LAZY", rtld::LAZY),
            ("SIZEOF_TIME_T", std::mem::size_of::<libc::time_t>() as i64),
            ("SIZEOF_VOID_P", std::mem::size_of::<*const c_void>() as i64),
        ] {
            d.insert(DictKey(Object::from_static(n)), Object::Int(v));
        }
        register(&mut d, "sizeof_code", b_sizeof_code);
        register(&mut d, "alignment_code", b_alignment_code);
        register(&mut d, "addressof_buffer", b_addressof_buffer);
        register(&mut d, "intern_buffer", b_intern_buffer);
        register(&mut d, "read_mem", b_read_mem);
        register(&mut d, "write_mem", b_write_mem);
        register(&mut d, "memmove", b_memmove);
        register(&mut d, "memset", b_memset);
        register(&mut d, "string_at", b_string_at);
        register(&mut d, "wstring_at", b_wstring_at);
        register(&mut d, "dlopen", b_dlopen);
        register(&mut d, "dlsym", b_dlsym);
        register(&mut d, "dlclose", b_dlclose);
        register(&mut d, "dlerror", b_dlerror);
        register(&mut d, "get_errno", b_get_errno);
        register(&mut d, "set_errno", b_set_errno);
        // The LastError trio backing the frozen `_ctypes.py`'s nt-only
        // surface (FormatError / get_last_error / set_last_error).
        #[cfg(windows)]
        {
            register(&mut d, "get_last_error", b_get_last_error);
            register(&mut d, "set_last_error", b_set_last_error);
            register(&mut d, "format_error", b_format_error);
        }
        register(&mut d, "unraisable", b_unraisable);
        register(&mut d, "configure_view", b_configure_view);
        #[cfg(target_os = "macos")]
        register(
            &mut d,
            "dyld_shared_cache_contains_path",
            b_dyld_shared_cache_contains_path,
        );
        // FFI bridge (libffi) — defined in the `ffi` submodule. All three
        // are positional (the frozen `_ctypes.py` calls them positionally).
        register(&mut d, "call_function", ffi::b_call_function);
        register(&mut d, "create_closure", ffi::b_create_closure);
        register(&mut d, "free_closure", ffi::b_free_closure);
    }
    Rc::new(PyModule {
        name: "_ctypes_native".to_owned(),
        filename: None,
        dict,
    })
}

//! RFC 0075 WS1 — the PEP 587 initialization-configuration surface.
//!
//! `PyStatus`, `PyWideStringList`, `PyPreConfig`, and `PyConfig` are
//! ABI-visible: embedders allocate them on their own stack and poke
//! fields directly, so the `#[repr(C)]` layouts here must match the
//! vendored stock header (`include/cpython313/cpython/initconfig.h`)
//! field-for-field. A layout assertion in this module's tests keeps
//! the twin honest.
//!
//! Ownership follows CPython: every `wchar_t*` field and every
//! `PyWideStringList` in a `PyConfig` is owned by the config. The
//! setters always copy their input, and [`PyConfig_Clear`] frees every
//! pointer field unconditionally. Allocation goes through
//! `libc::malloc` (CPython uses the raw allocator domain for exactly
//! this reason: config memory must be usable before the interpreter
//! exists).

use std::os::raw::{c_char, c_int, c_ulong};
use std::ptr;

use libc::wchar_t;

// ---------------------------------------------------------------------------
// Wide-string helpers (shared with `weavepy-pylib`'s argv decoding)
// ---------------------------------------------------------------------------

/// Decode one NUL-terminated `wchar_t` string: UTF-16 where `wchar_t`
/// is 2 bytes (Windows), UTF-32 where it is 4 (POSIX); lossy on
/// ill-formed input.
///
/// # Safety
///
/// `ptr` must point to a valid NUL-terminated `wchar_t` string.
#[allow(clippy::cast_lossless, clippy::cast_sign_loss)]
pub unsafe fn decode_wide(ptr: *const wchar_t) -> String {
    let mut len = 0usize;
    while unsafe { *ptr.add(len) } != 0 {
        len += 1;
    }
    let units = unsafe { std::slice::from_raw_parts(ptr, len) };
    if std::mem::size_of::<wchar_t>() == 2 {
        let units16: Vec<u16> = units.iter().map(|&u| u as u16).collect();
        String::from_utf16_lossy(&units16)
    } else {
        units
            .iter()
            .map(|&u| char::from_u32(u as u32).unwrap_or(char::REPLACEMENT_CHARACTER))
            .collect()
    }
}

/// Decode a possibly-NULL wide string to `Option<String>`.
unsafe fn decode_wide_opt(ptr: *const wchar_t) -> Option<String> {
    if ptr.is_null() {
        None
    } else {
        Some(unsafe { decode_wide(ptr) })
    }
}

/// Allocate a NUL-terminated `wchar_t` copy of `s` with `libc::malloc`.
/// The caller (i.e. the config) owns the allocation.
#[allow(clippy::cast_possible_wrap)]
pub fn alloc_wide(s: &str) -> *mut wchar_t {
    if std::mem::size_of::<wchar_t>() == 2 {
        let units: Vec<u16> = s.encode_utf16().collect();
        let n = units.len() + 1;
        let buf = unsafe { libc::malloc(n * 2) } as *mut u16;
        if buf.is_null() {
            return ptr::null_mut();
        }
        unsafe {
            ptr::copy_nonoverlapping(units.as_ptr(), buf, units.len());
            *buf.add(units.len()) = 0;
        }
        buf as *mut wchar_t
    } else {
        let units: Vec<i32> = s.chars().map(|c| c as i32).collect();
        let n = units.len() + 1;
        let buf = unsafe { libc::malloc(n * 4) } as *mut i32;
        if buf.is_null() {
            return ptr::null_mut();
        }
        unsafe {
            ptr::copy_nonoverlapping(units.as_ptr(), buf, units.len());
            *buf.add(units.len()) = 0;
        }
        buf as *mut wchar_t
    }
}

/// Free a config-owned wide string (NULL-safe) and clear the slot.
unsafe fn clear_wide(slot: &mut *mut wchar_t) {
    if !slot.is_null() {
        unsafe { libc::free(*slot as *mut libc::c_void) };
        *slot = ptr::null_mut();
    }
}

/// Replace a config-owned wide-string slot with a copy of `value`.
unsafe fn set_wide(slot: &mut *mut wchar_t, value: &str) -> bool {
    let fresh = alloc_wide(value);
    if fresh.is_null() {
        return false;
    }
    unsafe { clear_wide(slot) };
    *slot = fresh;
    true
}

// ---------------------------------------------------------------------------
// PyStatus
// ---------------------------------------------------------------------------

pub const _PyStatus_TYPE_OK: c_int = 0;
pub const _PyStatus_TYPE_ERROR: c_int = 1;
pub const _PyStatus_TYPE_EXIT: c_int = 2;

/// CPython's by-value status struct (`initconfig.h`). Returned by
/// value from every config entry point; `err_msg` points at a static
/// (or leaked-once) C string, so no ownership protocol exists — same
/// as CPython, whose messages are string literals.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct PyStatus {
    pub _type: c_int,
    pub func: *const c_char,
    pub err_msg: *const c_char,
    pub exitcode: c_int,
}

impl PyStatus {
    pub const OK: PyStatus = PyStatus {
        _type: _PyStatus_TYPE_OK,
        func: ptr::null(),
        err_msg: ptr::null(),
        exitcode: 0,
    };

    /// An error status with a leaked C copy of `msg`. Leaking matches
    /// CPython's static-literal lifetime contract; config errors are
    /// rare and terminal, so the leak is bounded in practice.
    pub fn error(msg: &str) -> PyStatus {
        let c = std::ffi::CString::new(msg.replace('\0', "?")).unwrap();
        PyStatus {
            _type: _PyStatus_TYPE_ERROR,
            func: ptr::null(),
            err_msg: c.into_raw(),
            exitcode: 0,
        }
    }

    pub fn exit(code: c_int) -> PyStatus {
        PyStatus {
            _type: _PyStatus_TYPE_EXIT,
            func: ptr::null(),
            err_msg: ptr::null(),
            exitcode: code,
        }
    }

    pub fn is_ok(&self) -> bool {
        self._type == _PyStatus_TYPE_OK
    }
}

#[no_mangle]
pub extern "C" fn PyStatus_Ok() -> PyStatus {
    PyStatus::OK
}

#[no_mangle]
pub unsafe extern "C" fn PyStatus_Error(err_msg: *const c_char) -> PyStatus {
    PyStatus {
        _type: _PyStatus_TYPE_ERROR,
        func: ptr::null(),
        err_msg,
        exitcode: 0,
    }
}

static NO_MEMORY: &[u8] = b"memory allocation failed\0";

#[no_mangle]
pub extern "C" fn PyStatus_NoMemory() -> PyStatus {
    PyStatus {
        _type: _PyStatus_TYPE_ERROR,
        func: ptr::null(),
        err_msg: NO_MEMORY.as_ptr() as *const c_char,
        exitcode: 1,
    }
}

#[no_mangle]
pub extern "C" fn PyStatus_Exit(exitcode: c_int) -> PyStatus {
    PyStatus::exit(exitcode)
}

#[no_mangle]
pub extern "C" fn PyStatus_IsError(status: PyStatus) -> c_int {
    (status._type == _PyStatus_TYPE_ERROR) as c_int
}

#[no_mangle]
pub extern "C" fn PyStatus_IsExit(status: PyStatus) -> c_int {
    (status._type == _PyStatus_TYPE_EXIT) as c_int
}

#[no_mangle]
pub extern "C" fn PyStatus_Exception(status: PyStatus) -> c_int {
    (status._type != _PyStatus_TYPE_OK) as c_int
}

/// `Py_ExitStatusException(status)` — terminate the process the way
/// CPython does when initialization fails: `exit(exitcode)` for an
/// exit status, an `stderr` diagnostic + `exit(1)` for an error.
/// Undefined (here: a plain abort) on a non-exception status, per the
/// documented contract.
#[no_mangle]
pub unsafe extern "C" fn Py_ExitStatusException(status: PyStatus) -> ! {
    if status._type == _PyStatus_TYPE_EXIT {
        std::process::exit(status.exitcode);
    }
    if status._type == _PyStatus_TYPE_ERROR {
        let msg = if status.err_msg.is_null() {
            "initialization error".to_owned()
        } else {
            unsafe { std::ffi::CStr::from_ptr(status.err_msg) }
                .to_string_lossy()
                .into_owned()
        };
        let func = if status.func.is_null() {
            String::new()
        } else {
            format!(
                "{}: ",
                unsafe { std::ffi::CStr::from_ptr(status.func) }.to_string_lossy()
            )
        };
        eprintln!("Fatal Python error: {func}{msg}");
        std::process::exit(1);
    }
    std::process::abort();
}

// ---------------------------------------------------------------------------
// PyWideStringList
// ---------------------------------------------------------------------------

/// `{ Py_ssize_t length; wchar_t **items; }` — config-owned.
#[repr(C)]
pub struct PyWideStringList {
    pub length: isize,
    pub items: *mut *mut wchar_t,
}

impl PyWideStringList {
    pub const EMPTY: PyWideStringList = PyWideStringList {
        length: 0,
        items: ptr::null_mut(),
    };
}

/// Decode a list into owned Rust strings.
unsafe fn list_to_vec(list: &PyWideStringList) -> Vec<String> {
    let mut out = Vec::new();
    if list.items.is_null() {
        return out;
    }
    for i in 0..list.length.max(0) as usize {
        let item = unsafe { *list.items.add(i) };
        if !item.is_null() {
            out.push(unsafe { decode_wide(item) });
        }
    }
    out
}

/// Free the list storage and zero it.
unsafe fn clear_list(list: &mut PyWideStringList) {
    if !list.items.is_null() {
        for i in 0..list.length.max(0) as usize {
            let item = unsafe { *list.items.add(i) };
            if !item.is_null() {
                unsafe { libc::free(item as *mut libc::c_void) };
            }
        }
        unsafe { libc::free(list.items as *mut libc::c_void) };
    }
    list.items = ptr::null_mut();
    list.length = 0;
}

/// Rebuild a list from Rust strings (frees the previous storage).
unsafe fn set_list(list: &mut PyWideStringList, values: &[String]) -> bool {
    let items = unsafe { libc::malloc(values.len().max(1) * std::mem::size_of::<*mut wchar_t>()) }
        as *mut *mut wchar_t;
    if items.is_null() {
        return false;
    }
    for (i, v) in values.iter().enumerate() {
        let w = alloc_wide(v);
        if w.is_null() {
            for j in 0..i {
                unsafe { libc::free(*items.add(j) as *mut libc::c_void) };
            }
            unsafe { libc::free(items as *mut libc::c_void) };
            return false;
        }
        unsafe { *items.add(i) = w };
    }
    unsafe { clear_list(list) };
    list.items = items;
    list.length = values.len() as isize;
    true
}

#[no_mangle]
pub unsafe extern "C" fn PyWideStringList_Append(
    list: *mut PyWideStringList,
    item: *const wchar_t,
) -> PyStatus {
    if list.is_null() || item.is_null() {
        return PyStatus::error("PyWideStringList_Append: NULL argument");
    }
    let list = unsafe { &mut *list };
    let mut values = unsafe { list_to_vec(list) };
    values.push(unsafe { decode_wide(item) });
    if unsafe { set_list(list, &values) } {
        PyStatus::OK
    } else {
        PyStatus_NoMemory()
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyWideStringList_Insert(
    list: *mut PyWideStringList,
    index: isize,
    item: *const wchar_t,
) -> PyStatus {
    if list.is_null() || item.is_null() {
        return PyStatus::error("PyWideStringList_Insert: NULL argument");
    }
    let list = unsafe { &mut *list };
    let mut values = unsafe { list_to_vec(list) };
    // CPython clamps: negative → 0, past-end → append.
    let idx = if index < 0 {
        0
    } else {
        (index as usize).min(values.len())
    };
    values.insert(idx, unsafe { decode_wide(item) });
    if unsafe { set_list(list, &values) } {
        PyStatus::OK
    } else {
        PyStatus_NoMemory()
    }
}

// ---------------------------------------------------------------------------
// PyPreConfig
// ---------------------------------------------------------------------------

pub const _PyConfig_INIT_COMPAT: c_int = 1;
pub const _PyConfig_INIT_PYTHON: c_int = 2;
pub const _PyConfig_INIT_ISOLATED: c_int = 3;

/// Field-for-field with the vendored `initconfig.h` `PyPreConfig`.
#[repr(C)]
pub struct PyPreConfig {
    pub _config_init: c_int,
    pub parse_argv: c_int,
    pub isolated: c_int,
    pub use_environment: c_int,
    pub configure_locale: c_int,
    pub coerce_c_locale: c_int,
    pub coerce_c_locale_warn: c_int,
    #[cfg(windows)]
    pub legacy_windows_fs_encoding: c_int,
    pub utf8_mode: c_int,
    pub dev_mode: c_int,
    pub allocator: c_int,
}

#[no_mangle]
pub unsafe extern "C" fn PyPreConfig_InitPythonConfig(config: *mut PyPreConfig) {
    if config.is_null() {
        return;
    }
    let c = unsafe { &mut *config };
    *c = PyPreConfig {
        _config_init: _PyConfig_INIT_PYTHON,
        parse_argv: 1,
        isolated: 0,
        use_environment: 1,
        configure_locale: 1,
        coerce_c_locale: -1,
        coerce_c_locale_warn: -1,
        #[cfg(windows)]
        legacy_windows_fs_encoding: 0,
        utf8_mode: -1,
        dev_mode: -1,
        allocator: 0, // PYMEM_ALLOCATOR_NOT_SET
    };
}

#[no_mangle]
pub unsafe extern "C" fn PyPreConfig_InitIsolatedConfig(config: *mut PyPreConfig) {
    if config.is_null() {
        return;
    }
    let c = unsafe { &mut *config };
    *c = PyPreConfig {
        _config_init: _PyConfig_INIT_ISOLATED,
        parse_argv: 0,
        isolated: 1,
        use_environment: 0,
        configure_locale: 0,
        coerce_c_locale: 0,
        coerce_c_locale_warn: 0,
        #[cfg(windows)]
        legacy_windows_fs_encoding: 0,
        utf8_mode: 0,
        dev_mode: 0,
        allocator: 0,
    };
}

/// The recorded pre-configuration (WeavePy is UTF-8-native, so the
/// pre-config's effect is bookkeeping reflected on `sys.flags` /
/// the embed config rather than locale surgery).
#[derive(Clone, Copy, Default)]
pub struct StoredPreConfig {
    pub isolated: bool,
    pub use_environment: bool,
    pub utf8_mode: i32,
    pub dev_mode: bool,
}

static PRECONFIG: std::sync::Mutex<Option<StoredPreConfig>> = std::sync::Mutex::new(None);

pub fn stored_preconfig() -> Option<StoredPreConfig> {
    *PRECONFIG.lock().unwrap()
}

unsafe fn preinit_common(src: *const PyPreConfig) -> PyStatus {
    let stored = if src.is_null() {
        StoredPreConfig {
            isolated: false,
            use_environment: true,
            utf8_mode: -1,
            dev_mode: false,
        }
    } else {
        let c = unsafe { &*src };
        StoredPreConfig {
            isolated: c.isolated > 0,
            use_environment: c.use_environment != 0,
            utf8_mode: c.utf8_mode,
            dev_mode: c.dev_mode > 0,
        }
    };
    *PRECONFIG.lock().unwrap() = Some(stored);
    PyStatus::OK
}

#[no_mangle]
pub unsafe extern "C" fn Py_PreInitialize(src_config: *const PyPreConfig) -> PyStatus {
    unsafe { preinit_common(src_config) }
}

#[no_mangle]
pub unsafe extern "C" fn Py_PreInitializeFromArgs(
    src_config: *const PyPreConfig,
    _argc: isize,
    _argv: *mut *mut wchar_t,
) -> PyStatus {
    unsafe { preinit_common(src_config) }
}

#[no_mangle]
pub unsafe extern "C" fn Py_PreInitializeFromBytesArgs(
    src_config: *const PyPreConfig,
    _argc: isize,
    _argv: *mut *mut c_char,
) -> PyStatus {
    unsafe { preinit_common(src_config) }
}

// ---------------------------------------------------------------------------
// PyConfig
// ---------------------------------------------------------------------------

/// Field-for-field with the vendored `initconfig.h` `PyConfig` for a
/// non-debug, GIL-enabled 3.13 build (i.e. exactly the build every
/// cp313 wheel targets). `Py_GIL_DISABLED` / `Py_STATS` / `Py_DEBUG`
/// conditional fields are absent by construction; `MS_WINDOWS` fields
/// follow `cfg(windows)`.
#[repr(C)]
pub struct PyConfig {
    pub _config_init: c_int,
    pub isolated: c_int,
    pub use_environment: c_int,
    pub dev_mode: c_int,
    pub install_signal_handlers: c_int,
    pub use_hash_seed: c_int,
    pub hash_seed: c_ulong,
    pub faulthandler: c_int,
    pub tracemalloc: c_int,
    pub perf_profiling: c_int,
    pub import_time: c_int,
    pub code_debug_ranges: c_int,
    pub show_ref_count: c_int,
    pub dump_refs: c_int,
    pub dump_refs_file: *mut wchar_t,
    pub malloc_stats: c_int,
    pub filesystem_encoding: *mut wchar_t,
    pub filesystem_errors: *mut wchar_t,
    pub pycache_prefix: *mut wchar_t,
    pub parse_argv: c_int,
    pub orig_argv: PyWideStringList,
    pub argv: PyWideStringList,
    pub xoptions: PyWideStringList,
    pub warnoptions: PyWideStringList,
    pub site_import: c_int,
    pub bytes_warning: c_int,
    pub warn_default_encoding: c_int,
    pub inspect: c_int,
    pub interactive: c_int,
    pub optimization_level: c_int,
    pub parser_debug: c_int,
    pub write_bytecode: c_int,
    pub verbose: c_int,
    pub quiet: c_int,
    pub user_site_directory: c_int,
    pub configure_c_stdio: c_int,
    pub buffered_stdio: c_int,
    pub stdio_encoding: *mut wchar_t,
    pub stdio_errors: *mut wchar_t,
    #[cfg(windows)]
    pub legacy_windows_stdio: c_int,
    pub check_hash_pycs_mode: *mut wchar_t,
    pub use_frozen_modules: c_int,
    pub safe_path: c_int,
    pub int_max_str_digits: c_int,
    pub cpu_count: c_int,
    // --- Path configuration inputs ------------
    pub pathconfig_warnings: c_int,
    pub program_name: *mut wchar_t,
    pub pythonpath_env: *mut wchar_t,
    pub home: *mut wchar_t,
    pub platlibdir: *mut wchar_t,
    // --- Path configuration outputs -----------
    pub module_search_paths_set: c_int,
    pub module_search_paths: PyWideStringList,
    pub stdlib_dir: *mut wchar_t,
    pub executable: *mut wchar_t,
    pub base_executable: *mut wchar_t,
    pub prefix: *mut wchar_t,
    pub base_prefix: *mut wchar_t,
    pub exec_prefix: *mut wchar_t,
    pub base_exec_prefix: *mut wchar_t,
    // --- Py_Main() parameters ------------------
    pub skip_source_first_line: c_int,
    pub run_command: *mut wchar_t,
    pub run_module: *mut wchar_t,
    pub run_filename: *mut wchar_t,
    pub sys_path_0: *mut wchar_t,
    // --- Private fields -------------------------
    pub _install_importlib: c_int,
    pub _init_main: c_int,
    pub _is_python_build: c_int,
}

/// CPython's `config_init_defaults` values (`initconfig.c`).
unsafe fn config_defaults(c: &mut PyConfig) {
    // Zero everything first so pointer fields are NULL (the caller's
    // struct may be stack garbage — CPython memsets too). NB: count is
    // in *elements of T*: `write_bytes(c, 0, 1)` zeroes one whole
    // PyConfig. The previous `.cast::<u8>()` made T = u8 and zeroed a
    // single *byte*, leaving every pointer field as stack garbage that
    // `PyConfig_Clear` then freed — a malloc abort inside Pillow's
    // getfont (init → PyArg → Clear on every truetype load; RFC 0075
    // WS9 Pillow selftest lane).
    unsafe { ptr::write_bytes(std::ptr::from_mut(c), 0, 1) };
    c._config_init = _PyConfig_INIT_COMPAT;
    c.isolated = -1;
    c.use_environment = -1;
    c.dev_mode = -1;
    c.install_signal_handlers = 1;
    c.use_hash_seed = -1;
    c.faulthandler = -1;
    c.tracemalloc = -1;
    c.perf_profiling = -1;
    c.module_search_paths_set = 0;
    c.parse_argv = 0;
    c.site_import = -1;
    c.bytes_warning = -1;
    c.warn_default_encoding = 0;
    c.inspect = -1;
    c.interactive = -1;
    c.optimization_level = -1;
    c.parser_debug = -1;
    c.write_bytecode = -1;
    c.verbose = -1;
    c.quiet = -1;
    c.user_site_directory = -1;
    c.configure_c_stdio = 0;
    c.buffered_stdio = -1;
    c._install_importlib = 1;
    c.pathconfig_warnings = -1;
    c._init_main = 1;
    c.use_frozen_modules = -1;
    c.safe_path = 0;
    c._is_python_build = 0;
    c.int_max_str_digits = -1;
    c.cpu_count = -1;
}

#[no_mangle]
pub unsafe extern "C" fn PyConfig_InitPythonConfig(config: *mut PyConfig) {
    if config.is_null() {
        return;
    }
    let c = unsafe { &mut *config };
    unsafe { config_defaults(c) };
    c._config_init = _PyConfig_INIT_PYTHON;
    c.configure_c_stdio = 1;
    c.parse_argv = 1;
}

#[no_mangle]
pub unsafe extern "C" fn PyConfig_InitIsolatedConfig(config: *mut PyConfig) {
    if config.is_null() {
        return;
    }
    let c = unsafe { &mut *config };
    unsafe { config_defaults(c) };
    c._config_init = _PyConfig_INIT_ISOLATED;
    c.isolated = 1;
    c.use_environment = 0;
    c.user_site_directory = 0;
    c.dev_mode = 0;
    c.install_signal_handlers = 0;
    c.use_hash_seed = 0;
    c.faulthandler = 0;
    c.tracemalloc = 0;
    c.perf_profiling = 0;
    c.pathconfig_warnings = 0;
    c.safe_path = 1;
}

#[no_mangle]
pub unsafe extern "C" fn PyConfig_Clear(config: *mut PyConfig) {
    if config.is_null() {
        return;
    }
    let c = unsafe { &mut *config };
    unsafe {
        clear_wide(&mut c.dump_refs_file);
        clear_wide(&mut c.filesystem_encoding);
        clear_wide(&mut c.filesystem_errors);
        clear_wide(&mut c.pycache_prefix);
        clear_list(&mut c.orig_argv);
        clear_list(&mut c.argv);
        clear_list(&mut c.xoptions);
        clear_list(&mut c.warnoptions);
        clear_wide(&mut c.stdio_encoding);
        clear_wide(&mut c.stdio_errors);
        clear_wide(&mut c.check_hash_pycs_mode);
        clear_wide(&mut c.program_name);
        clear_wide(&mut c.pythonpath_env);
        clear_wide(&mut c.home);
        clear_wide(&mut c.platlibdir);
        clear_list(&mut c.module_search_paths);
        clear_wide(&mut c.stdlib_dir);
        clear_wide(&mut c.executable);
        clear_wide(&mut c.base_executable);
        clear_wide(&mut c.prefix);
        clear_wide(&mut c.base_prefix);
        clear_wide(&mut c.exec_prefix);
        clear_wide(&mut c.base_exec_prefix);
        clear_wide(&mut c.run_command);
        clear_wide(&mut c.run_module);
        clear_wide(&mut c.run_filename);
        clear_wide(&mut c.sys_path_0);
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyConfig_SetString(
    config: *mut PyConfig,
    config_str: *mut *mut wchar_t,
    s: *const wchar_t,
) -> PyStatus {
    let _ = config;
    if config_str.is_null() {
        return PyStatus::error("PyConfig_SetString: NULL destination");
    }
    let slot = unsafe { &mut *config_str };
    if s.is_null() {
        unsafe { clear_wide(slot) };
        return PyStatus::OK;
    }
    let value = unsafe { decode_wide(s) };
    if unsafe { set_wide(slot, &value) } {
        PyStatus::OK
    } else {
        PyStatus_NoMemory()
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyConfig_SetBytesString(
    config: *mut PyConfig,
    config_str: *mut *mut wchar_t,
    s: *const c_char,
) -> PyStatus {
    let _ = config;
    if config_str.is_null() {
        return PyStatus::error("PyConfig_SetBytesString: NULL destination");
    }
    let slot = unsafe { &mut *config_str };
    if s.is_null() {
        unsafe { clear_wide(slot) };
        return PyStatus::OK;
    }
    let bytes = unsafe { std::ffi::CStr::from_ptr(s) }.to_bytes();
    let value = String::from_utf8_lossy(bytes).into_owned();
    if unsafe { set_wide(slot, &value) } {
        PyStatus::OK
    } else {
        PyStatus_NoMemory()
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyConfig_SetArgv(
    config: *mut PyConfig,
    argc: isize,
    argv: *const *mut wchar_t,
) -> PyStatus {
    if config.is_null() {
        return PyStatus::error("PyConfig_SetArgv: NULL config");
    }
    let mut values = Vec::new();
    if !argv.is_null() {
        for i in 0..argc.max(0) as usize {
            let a = unsafe { *argv.add(i) };
            if a.is_null() {
                break;
            }
            values.push(unsafe { decode_wide(a) });
        }
    }
    let c = unsafe { &mut *config };
    if unsafe { set_list(&mut c.argv, &values) } {
        PyStatus::OK
    } else {
        PyStatus_NoMemory()
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyConfig_SetBytesArgv(
    config: *mut PyConfig,
    argc: isize,
    argv: *const *mut c_char,
) -> PyStatus {
    if config.is_null() {
        return PyStatus::error("PyConfig_SetBytesArgv: NULL config");
    }
    let mut values = Vec::new();
    if !argv.is_null() {
        for i in 0..argc.max(0) as usize {
            let a = unsafe { *argv.add(i) };
            if a.is_null() {
                break;
            }
            let bytes = unsafe { std::ffi::CStr::from_ptr(a) }.to_bytes();
            values.push(String::from_utf8_lossy(bytes).into_owned());
        }
    }
    let c = unsafe { &mut *config };
    if unsafe { set_list(&mut c.argv, &values) } {
        PyStatus::OK
    } else {
        PyStatus_NoMemory()
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyConfig_SetWideStringList(
    config: *mut PyConfig,
    list: *mut PyWideStringList,
    length: isize,
    items: *mut *mut wchar_t,
) -> PyStatus {
    let _ = config;
    if list.is_null() {
        return PyStatus::error("PyConfig_SetWideStringList: NULL list");
    }
    let mut values = Vec::new();
    if !items.is_null() {
        for i in 0..length.max(0) as usize {
            let a = unsafe { *items.add(i) };
            if !a.is_null() {
                values.push(unsafe { decode_wide(a) });
            }
        }
    }
    if unsafe { set_list(&mut *list, &values) } {
        PyStatus::OK
    } else {
        PyStatus_NoMemory()
    }
}

// ---------------------------------------------------------------------------
// PyConfig_Read — defaults, environment, and CPython-style argv parsing
// ---------------------------------------------------------------------------

/// Resolve a `-1` ("let Python decide") int against a default.
fn resolved(v: c_int, default: c_int) -> c_int {
    if v < 0 {
        default
    } else {
        v
    }
}

/// Environment lookup honouring `use_environment`.
fn env_var(c: &PyConfig, name: &str) -> Option<String> {
    if c.use_environment == 0 {
        return None;
    }
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

/// CPython's `config_parse_cmdline`: with `parse_argv == 1`, `argv` is
/// a full python command line — interpreter options are consumed into
/// config fields, `-c`/`-m`/script land in `run_*`, and `config.argv`
/// is rewritten to the `sys.argv`-to-be form (`-c` → `["-c", rest…]`,
/// `-m` → `["-m", rest…]`, script → `[script, rest…]`).
#[allow(clippy::too_many_lines)]
unsafe fn parse_cmdline(c: &mut PyConfig) -> Result<(), PyStatus> {
    let raw = unsafe { list_to_vec(&c.argv) };
    if raw.is_empty() {
        return Ok(());
    }
    let mut xoptions = unsafe { list_to_vec(&c.xoptions) };
    let mut warnoptions = unsafe { list_to_vec(&c.warnoptions) };
    let mut run_command: Option<String> = None;
    let mut run_module: Option<String> = None;
    let mut run_filename: Option<String> = None;
    let mut new_argv: Vec<String> = Vec::new();

    let mut i = 1usize;
    while i < raw.len() {
        let arg = raw[i].clone();
        if !arg.starts_with('-') || arg == "-" {
            // Script (or stdin marker): everything from here on is
            // program argv.
            if arg != "-" {
                run_filename = Some(arg.clone());
            }
            new_argv.extend(raw[i..].iter().cloned());
            break;
        }
        // `-c` and `-m` consume the rest of the command line.
        if let Some(rest) = arg.strip_prefix("-c") {
            let (cmd, next) = if rest.is_empty() {
                i += 1;
                if i >= raw.len() {
                    return Err(PyStatus::error("Argument expected for the -c option"));
                }
                (raw[i].clone(), i + 1)
            } else {
                (rest.to_owned(), i + 1)
            };
            run_command = Some(format!("{cmd}\n"));
            new_argv.push("-c".to_owned());
            new_argv.extend(raw[next..].iter().cloned());
            break;
        }
        if let Some(rest) = arg.strip_prefix("-m") {
            let (m, next) = if rest.is_empty() {
                i += 1;
                if i >= raw.len() {
                    return Err(PyStatus::error("Argument expected for the -m option"));
                }
                (raw[i].clone(), i + 1)
            } else {
                (rest.to_owned(), i + 1)
            };
            run_module = Some(m);
            new_argv.push("-m".to_owned());
            new_argv.extend(raw[next..].iter().cloned());
            break;
        }
        match arg.as_str() {
            "-i" => {
                c.inspect = 1;
                c.interactive = 1;
            }
            "-O" => c.optimization_level = resolved(c.optimization_level, 0).max(0) + 1,
            "-B" => c.write_bytecode = 0,
            "-s" => c.user_site_directory = 0,
            "-S" => c.site_import = 0,
            "-E" => c.use_environment = 0,
            "-I" => {
                c.isolated = 1;
                c.use_environment = 0;
                c.user_site_directory = 0;
                c.safe_path = 1;
            }
            "-P" => c.safe_path = 1,
            "-v" => c.verbose = resolved(c.verbose, 0) + 1,
            "-q" => c.quiet = resolved(c.quiet, 0) + 1,
            "-u" => c.buffered_stdio = 0,
            "-b" => c.bytes_warning = resolved(c.bytes_warning, 0) + 1,
            "-d" => c.parser_debug = resolved(c.parser_debug, 0) + 1,
            "-x" => c.skip_source_first_line = 1,
            "-X" => {
                i += 1;
                if i >= raw.len() {
                    return Err(PyStatus::error("Argument expected for the -X option"));
                }
                xoptions.push(raw[i].clone());
            }
            "-W" => {
                i += 1;
                if i >= raw.len() {
                    return Err(PyStatus::error("Argument expected for the -W option"));
                }
                warnoptions.push(raw[i].clone());
            }
            _ => {
                if let Some(x) = arg.strip_prefix("-X") {
                    xoptions.push(x.to_owned());
                } else if let Some(w) = arg.strip_prefix("-W") {
                    warnoptions.push(w.to_owned());
                }
                // Unknown options are ignored (the embedding twin
                // parses far fewer flags than the CLI; embedders pass
                // canonical command lines).
            }
        }
        i += 1;
    }
    if new_argv.is_empty() {
        // No command/module/script: sys.argv = [""] like `python`.
        new_argv.push(String::new());
    }
    if !unsafe { set_list(&mut c.argv, &new_argv) }
        || !unsafe { set_list(&mut c.xoptions, &xoptions) }
        || !unsafe { set_list(&mut c.warnoptions, &warnoptions) }
    {
        return Err(PyStatus_NoMemory());
    }
    unsafe {
        if let Some(cmd) = run_command {
            if !set_wide(&mut c.run_command, &cmd) {
                return Err(PyStatus_NoMemory());
            }
        }
        if let Some(m) = run_module {
            if !set_wide(&mut c.run_module, &m) {
                return Err(PyStatus_NoMemory());
            }
        }
        if let Some(f) = run_filename {
            if !set_wide(&mut c.run_filename, &f) {
                return Err(PyStatus_NoMemory());
            }
        }
    }
    Ok(())
}

#[no_mangle]
#[allow(clippy::too_many_lines)]
pub unsafe extern "C" fn PyConfig_Read(config: *mut PyConfig) -> PyStatus {
    if config.is_null() {
        return PyStatus::error("PyConfig_Read: NULL config");
    }
    let c = unsafe { &mut *config };

    // Pre-config inheritance.
    if let Some(pre) = stored_preconfig() {
        if c.isolated < 0 {
            c.isolated = pre.isolated as c_int;
        }
        if c.use_environment < 0 {
            c.use_environment = pre.use_environment as c_int;
        }
        if c.dev_mode < 0 {
            c.dev_mode = pre.dev_mode as c_int;
        }
    }
    // Isolated implies no env / no user site.
    if c.isolated > 0 {
        c.use_environment = 0;
        c.user_site_directory = 0;
    }
    c.isolated = resolved(c.isolated, 0);
    c.use_environment = resolved(c.use_environment, 1);
    c.dev_mode = resolved(c.dev_mode, 0);

    // Record orig_argv before parsing rewrites argv (CPython does the
    // same in `_PyConfig_Write`).
    if c.orig_argv.length == 0 && c.argv.length != 0 {
        let orig = unsafe { list_to_vec(&c.argv) };
        if !unsafe { set_list(&mut c.orig_argv, &orig) } {
            return PyStatus_NoMemory();
        }
    }
    if c.parse_argv == 1 {
        if let Err(status) = unsafe { parse_cmdline(c) } {
            return status;
        }
    }
    // Environment (post-cmdline, matching CPython's precedence: the
    // command line wins).
    if c.use_environment != 0 {
        if c.optimization_level < 0 {
            if let Some(v) = env_var(c, "PYTHONOPTIMIZE") {
                c.optimization_level = v.parse::<c_int>().unwrap_or(1).max(1);
            }
        }
        if c.write_bytecode < 0 && env_var(c, "PYTHONDONTWRITEBYTECODE").is_some() {
            c.write_bytecode = 0;
        }
        if c.verbose < 0 {
            if let Some(v) = env_var(c, "PYTHONVERBOSE") {
                c.verbose = v.parse::<c_int>().unwrap_or(1).max(1);
            }
        }
        if c.inspect < 0 && env_var(c, "PYTHONINSPECT").is_some() {
            c.inspect = 1;
        }
        if c.user_site_directory < 0 && env_var(c, "PYTHONNOUSERSITE").is_some() {
            c.user_site_directory = 0;
        }
        if c.pythonpath_env.is_null() {
            if let Some(v) = env_var(c, "PYTHONPATH") {
                if !unsafe { set_wide(&mut c.pythonpath_env, &v) } {
                    return PyStatus_NoMemory();
                }
            }
        }
        if c.home.is_null() {
            if let Some(v) = env_var(c, "PYTHONHOME") {
                if !unsafe { set_wide(&mut c.home, &v) } {
                    return PyStatus_NoMemory();
                }
            }
        }
    }
    // Defaults for everything still "let Python decide".
    c.faulthandler = resolved(c.faulthandler, if c.dev_mode == 1 { 1 } else { 0 });
    c.tracemalloc = resolved(c.tracemalloc, 0);
    c.perf_profiling = resolved(c.perf_profiling, 0);
    c.use_hash_seed = resolved(c.use_hash_seed, 0);
    c.site_import = resolved(c.site_import, 1);
    c.bytes_warning = resolved(c.bytes_warning, 0);
    c.inspect = resolved(c.inspect, 0);
    c.interactive = resolved(c.interactive, 0);
    c.optimization_level = resolved(c.optimization_level, 0);
    c.parser_debug = resolved(c.parser_debug, 0);
    c.write_bytecode = resolved(c.write_bytecode, 1);
    c.verbose = resolved(c.verbose, 0);
    c.quiet = resolved(c.quiet, 0);
    c.user_site_directory = resolved(c.user_site_directory, 1);
    c.buffered_stdio = resolved(c.buffered_stdio, 1);
    c.pathconfig_warnings = resolved(c.pathconfig_warnings, 1);
    c.use_frozen_modules = resolved(c.use_frozen_modules, 1);
    c.int_max_str_digits = if c.int_max_str_digits < 0 {
        4300
    } else {
        c.int_max_str_digits
    };
    if c.check_hash_pycs_mode.is_null()
        && !unsafe { set_wide(&mut c.check_hash_pycs_mode, "default") }
    {
        return PyStatus_NoMemory();
    }
    if c.filesystem_encoding.is_null() && !unsafe { set_wide(&mut c.filesystem_encoding, "utf-8") }
    {
        return PyStatus_NoMemory();
    }
    if c.filesystem_errors.is_null() {
        let errors = if cfg!(windows) {
            "surrogatepass"
        } else {
            "surrogateescape"
        };
        if !unsafe { set_wide(&mut c.filesystem_errors, errors) } {
            return PyStatus_NoMemory();
        }
    }
    // `PYTHONIOENCODING=<encoding>[:<errors>]` fills whichever stdio
    // half the embedder left unset (CPython's config_init_stdio_encoding;
    // either half may be empty: `:ignore` sets errors only).
    if c.use_environment != 0 {
        let (env_enc, env_err) = env_pythonioencoding();
        if c.stdio_encoding.is_null() {
            if let Some(enc) = env_enc {
                if !unsafe { set_wide(&mut c.stdio_encoding, &enc) } {
                    return PyStatus_NoMemory();
                }
            }
        }
        if c.stdio_errors.is_null() {
            if let Some(errs) = env_err {
                if !unsafe { set_wide(&mut c.stdio_errors, &errs) } {
                    return PyStatus_NoMemory();
                }
            }
        }
    }
    if c.stdio_encoding.is_null() && !unsafe { set_wide(&mut c.stdio_encoding, "utf-8") } {
        return PyStatus_NoMemory();
    }
    if c.stdio_errors.is_null() && !unsafe { set_wide(&mut c.stdio_errors, "strict") } {
        return PyStatus_NoMemory();
    }
    if c.platlibdir.is_null() && !unsafe { set_wide(&mut c.platlibdir, "lib") } {
        return PyStatus_NoMemory();
    }
    // Path-configuration outputs: executable + prefixes from the
    // process (the landmark walk proper runs at interpreter bootstrap;
    // these fields are the config's *report* of it).
    let exe = std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    if c.program_name.is_null() {
        let name = unsafe { list_to_vec(&c.orig_argv) }
            .first()
            .cloned()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| {
                if cfg!(windows) {
                    "python".to_owned()
                } else {
                    "python3".to_owned()
                }
            });
        if !unsafe { set_wide(&mut c.program_name, &name) } {
            return PyStatus_NoMemory();
        }
    }
    if c.executable.is_null() && !unsafe { set_wide(&mut c.executable, &exe) } {
        return PyStatus_NoMemory();
    }
    if c.base_executable.is_null() && !unsafe { set_wide(&mut c.base_executable, &exe) } {
        return PyStatus_NoMemory();
    }
    let prefix = unsafe { decode_wide_opt(c.home) }.unwrap_or_else(|| {
        std::path::Path::new(&exe)
            .parent()
            .and_then(std::path::Path::parent)
            .map(|p| p.display().to_string())
            .unwrap_or_default()
    });
    for slot in [
        &mut c.prefix,
        &mut c.base_prefix,
        &mut c.exec_prefix,
        &mut c.base_exec_prefix,
    ] {
        if slot.is_null() && !unsafe { set_wide(slot, &prefix) } {
            return PyStatus_NoMemory();
        }
    }
    if c.argv.length == 0 {
        let empty = vec![String::new()];
        if !unsafe { set_list(&mut c.argv, &empty) } {
            return PyStatus_NoMemory();
        }
    }
    c._config_init = _PyConfig_INIT_COMPAT.max(c._config_init);
    PyStatus::OK
}

// ---------------------------------------------------------------------------
// The decoded embed configuration handed to `embed::initialize`
// ---------------------------------------------------------------------------

/// The Rust-side decode of a `PyConfig` — everything
/// `Py_InitializeFromConfig` / `Py_RunMain` act on.
#[derive(Clone, Debug, Default)]
pub struct EmbedConfig {
    pub isolated: bool,
    pub use_environment: bool,
    pub site_import: bool,
    pub user_site_directory: bool,
    pub optimization_level: u8,
    pub write_bytecode: bool,
    pub verbose: u8,
    pub quiet: bool,
    pub inspect: bool,
    pub buffered_stdio: bool,
    pub safe_path: bool,
    pub install_signal_handlers: bool,
    pub bytes_warning: u8,
    pub int_max_str_digits: Option<i64>,
    pub faulthandler: bool,
    pub tracemalloc: u32,
    pub program_name: Option<String>,
    pub home: Option<String>,
    pub stdio_encoding: Option<String>,
    pub stdio_errors: Option<String>,
    pub pythonpath_env: Option<String>,
    pub pycache_prefix: Option<String>,
    pub module_search_paths: Option<Vec<String>>,
    pub argv: Vec<String>,
    pub orig_argv: Vec<String>,
    pub xoptions: Vec<String>,
    pub warnoptions: Vec<String>,
    pub run_command: Option<String>,
    pub run_module: Option<String>,
    pub run_filename: Option<String>,
    pub skip_source_first_line: bool,
}

/// The two halves of `PYTHONIOENCODING=<encoding>[:<errors>]`, each
/// `None` when absent or empty.
pub(crate) fn env_pythonioencoding() -> (Option<String>, Option<String>) {
    let Ok(v) = std::env::var("PYTHONIOENCODING") else {
        return (None, None);
    };
    if v.is_empty() {
        return (None, None);
    }
    match v.split_once(':') {
        Some((enc, errs)) => (
            (!enc.is_empty()).then(|| enc.to_owned()),
            (!errs.is_empty()).then(|| errs.to_owned()),
        ),
        None => (Some(v), None),
    }
}

/// Decode a (read or unread) `PyConfig` into the Rust form. Unread
/// configs are read first, so `Py_InitializeFromConfig(&raw_config)`
/// works exactly like CPython (init calls `PyConfig_Read` itself).
pub unsafe fn decode_config(config: *mut PyConfig) -> Result<EmbedConfig, PyStatus> {
    // Whether the stdio halves were *chosen* (embedder or environment)
    // rather than defaulted by the read: the VM's stream setup treats
    // "no explicit encoding/errors" specially (surrogateescape under
    // UTF-8 mode, CPython's init_sys_streams), so a read-defaulted
    // "utf-8"/"strict" must decode to `None`, not a forced value.
    let pre_read_enc = !unsafe { &*config }.stdio_encoding.is_null();
    let pre_read_err = !unsafe { &*config }.stdio_errors.is_null();
    // Same for program_name: only an embedder-chosen name may override
    // the process argv[0] as the `sys.executable` seed; the read's
    // "python3" fallback must not (it would PATH-resolve to a foreign
    // interpreter).
    let pre_read_prog = !unsafe { &*config }.program_name.is_null();
    let status = unsafe { PyConfig_Read(config) };
    if !status.is_ok() {
        return Err(status);
    }
    let c = unsafe { &*config };
    let (env_enc, env_err) = if c.use_environment != 0 {
        env_pythonioencoding()
    } else {
        (None, None)
    };
    let enc_chosen = pre_read_enc || env_enc.is_some();
    let err_chosen = pre_read_err || env_err.is_some();
    Ok(EmbedConfig {
        isolated: c.isolated == 1,
        use_environment: c.use_environment != 0,
        site_import: c.site_import != 0,
        user_site_directory: c.user_site_directory != 0,
        optimization_level: c.optimization_level.clamp(0, 2) as u8,
        write_bytecode: c.write_bytecode != 0,
        verbose: c.verbose.clamp(0, 255) as u8,
        quiet: c.quiet != 0,
        inspect: c.inspect != 0,
        buffered_stdio: c.buffered_stdio != 0,
        safe_path: c.safe_path == 1,
        install_signal_handlers: c.install_signal_handlers != 0,
        bytes_warning: c.bytes_warning.clamp(0, 255) as u8,
        int_max_str_digits: if c.int_max_str_digits >= 0 {
            Some(i64::from(c.int_max_str_digits))
        } else {
            None
        },
        faulthandler: c.faulthandler == 1,
        tracemalloc: c.tracemalloc.max(0) as u32,
        program_name: if pre_read_prog {
            unsafe { decode_wide_opt(c.program_name) }
        } else {
            None
        },
        home: unsafe { decode_wide_opt(c.home) },
        stdio_encoding: if enc_chosen {
            unsafe { decode_wide_opt(c.stdio_encoding) }
        } else {
            None
        },
        stdio_errors: if err_chosen {
            unsafe { decode_wide_opt(c.stdio_errors) }
        } else {
            None
        },
        pythonpath_env: unsafe { decode_wide_opt(c.pythonpath_env) },
        pycache_prefix: unsafe { decode_wide_opt(c.pycache_prefix) },
        module_search_paths: if c.module_search_paths_set == 1 {
            Some(unsafe { list_to_vec(&c.module_search_paths) })
        } else {
            None
        },
        argv: unsafe { list_to_vec(&c.argv) },
        orig_argv: unsafe { list_to_vec(&c.orig_argv) },
        xoptions: unsafe { list_to_vec(&c.xoptions) },
        warnoptions: unsafe { list_to_vec(&c.warnoptions) },
        run_command: unsafe { decode_wide_opt(c.run_command) },
        run_module: unsafe { decode_wide_opt(c.run_module) },
        run_filename: unsafe { decode_wide_opt(c.run_filename) },
        skip_source_first_line: c.skip_source_first_line == 1,
    })
}

// ---------------------------------------------------------------------------
// Py_InitializeFromConfig / Py_GetArgcArgv
// ---------------------------------------------------------------------------

#[no_mangle]
pub unsafe extern "C" fn Py_InitializeFromConfig(config: *const PyConfig) -> PyStatus {
    if config.is_null() {
        return PyStatus::error("Py_InitializeFromConfig: NULL config");
    }
    // CPython reads the caller's config into a private copy; we decode
    // into the Rust form without mutating the caller's struct when it
    // was already read. `decode_config` runs `PyConfig_Read`, which is
    // idempotent, so a cast-away-const here matches CPython's own
    // `_PyConfig_Copy` + read discipline.
    let decoded = match unsafe { decode_config(config as *mut PyConfig) } {
        Ok(d) => d,
        Err(status) => return status,
    };
    crate::embed::initialize(Some(decoded))
}

/// Storage backing `Py_GetArgcArgv`: the original (pre-parse) argv,
/// materialized as a stable (leaked-once) wide-string array.
struct OrigArgv(usize, *mut *mut wchar_t);
// SAFETY: written once under the mutex; the array itself is
// immutable after publication.
unsafe impl Send for OrigArgv {}

static ORIG_ARGV_WIDE: std::sync::Mutex<Option<OrigArgv>> = std::sync::Mutex::new(None);

pub fn record_orig_argv(args: &[String]) {
    let mut slot = ORIG_ARGV_WIDE.lock().unwrap();
    if slot.is_some() {
        return;
    }
    let items = unsafe {
        libc::malloc(args.len().max(1) * std::mem::size_of::<*mut wchar_t>()) as *mut *mut wchar_t
    };
    if items.is_null() {
        return;
    }
    for (i, a) in args.iter().enumerate() {
        unsafe { *items.add(i) = alloc_wide(a) };
    }
    *slot = Some(OrigArgv(args.len(), items));
}

#[no_mangle]
pub unsafe extern "C" fn Py_GetArgcArgv(argc: *mut c_int, argv: *mut *mut *mut wchar_t) {
    let slot = ORIG_ARGV_WIDE.lock().unwrap();
    let (n, items) = slot.as_ref().map_or((0, ptr::null_mut()), |o| (o.0, o.1));
    if !argc.is_null() {
        unsafe { *argc = n as c_int };
    }
    if !argv.is_null() {
        unsafe { *argv = items };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The struct sizes/offsets are ABI: an embedder's stack-allocated
    /// `PyConfig` is written through these offsets by both sides. The
    /// expected values are measured from the vendored stock header
    /// (`cc -I include/cpython313` + `sizeof`/`offsetof`, LP64 POSIX).
    #[test]
    fn config_struct_layout_matches_stock_header() {
        #[cfg(all(unix, target_pointer_width = "64"))]
        {
            assert_eq!(std::mem::size_of::<PyWideStringList>(), 16);
            assert_eq!(std::mem::size_of::<PyStatus>(), 32);
            assert_eq!(std::mem::size_of::<PyPreConfig>(), 40);
            assert_eq!(std::mem::size_of::<PyConfig>(), 448);
            assert_eq!(std::mem::offset_of!(PyConfig, argv), 128);
            assert_eq!(std::mem::offset_of!(PyConfig, program_name), 280);
            assert_eq!(std::mem::offset_of!(PyConfig, run_command), 400);
            assert_eq!(std::mem::offset_of!(PyConfig, _init_main), 436);
        }
    }

    /// The embedder hands `PyConfig_Init*` an *uninitialized* stack
    /// struct (CPython memsets it). A regression here is memory
    /// corruption, not a test failure: `config_defaults` once zeroed a
    /// single byte (a stray `.cast::<u8>()` made `write_bytes` count in
    /// bytes-of-u8 elements), so every pointer field kept stack garbage
    /// and `PyConfig_Clear` free()d it — a malloc abort inside Pillow's
    /// getfont, which wraps every truetype load in init → parse → clear
    /// (RFC 0075 WS9). Poison the buffer first so the test sees exactly
    /// what a dirty C stack would. The slot must be `MaybeUninit`, not a
    /// `[u8; N]` cast: a byte array is 1-aligned and the cast pointer
    /// trips Rust's misaligned-dereference check (a C embedder's struct
    /// is aligned by its declaration, so only the test needs this care).
    #[test]
    fn init_zeroes_a_poisoned_struct() {
        unsafe {
            let mut slot = std::mem::MaybeUninit::<PyConfig>::uninit();
            std::ptr::write_bytes(slot.as_mut_ptr().cast::<u8>(), 0xAA, size_of::<PyConfig>());
            let c = slot.as_mut_ptr();
            PyConfig_InitPythonConfig(c);
            assert!((*c).dump_refs_file.is_null());
            assert!((*c).filesystem_encoding.is_null());
            assert!((*c).program_name.is_null());
            assert!((*c).run_filename.is_null());
            assert!((*c).sys_path_0.is_null());
            assert_eq!((*c).hash_seed, 0);
            assert_eq!((*c).orig_argv.length, 0);
            assert!((*c).orig_argv.items.is_null());
            // Must be a no-op free-wise: every pointer is NULL.
            PyConfig_Clear(c);

            let mut slot = std::mem::MaybeUninit::<PyPreConfig>::uninit();
            std::ptr::write_bytes(
                slot.as_mut_ptr().cast::<u8>(),
                0x55,
                size_of::<PyPreConfig>(),
            );
            let p = slot.as_mut_ptr();
            PyPreConfig_InitPythonConfig(p);
            assert_eq!((*p).utf8_mode, -1);
            assert_eq!((*p).allocator, 0);
        }
    }

    #[test]
    fn wide_round_trip() {
        let w = alloc_wide("héllo 🐍");
        let back = unsafe { decode_wide(w) };
        unsafe { libc::free(w as *mut libc::c_void) };
        assert_eq!(back, "héllo 🐍");
    }

    #[test]
    fn parse_cmdline_extracts_run_command() {
        unsafe {
            let mut c: PyConfig = std::mem::zeroed();
            PyConfig_InitPythonConfig(&mut c);
            let args = ["prog", "-I", "-c", "print(1)", "tail"];
            let wides: Vec<*mut wchar_t> = args.iter().map(|a| alloc_wide(a)).collect();
            let st = PyConfig_SetArgv(&mut c, wides.len() as isize, wides.as_ptr());
            assert!(st.is_ok());
            for w in wides {
                libc::free(w as *mut libc::c_void);
            }
            let st = PyConfig_Read(&mut c);
            assert!(st.is_ok(), "read failed");
            assert_eq!(decode_wide(c.run_command), "print(1)\n");
            assert_eq!(c.isolated, 1);
            let argv = list_to_vec(&c.argv);
            assert_eq!(argv, vec!["-c".to_owned(), "tail".to_owned()]);
            let orig = list_to_vec(&c.orig_argv);
            assert_eq!(orig.len(), 5);
            PyConfig_Clear(&mut c);
        }
    }
}

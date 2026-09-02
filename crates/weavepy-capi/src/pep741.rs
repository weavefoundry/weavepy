//! RFC 0076 WS14 — the PEP 741 configuration C API.
//!
//! PEP 741 (CPython 3.14) fronts the PEP 587 structs with a
//! string-keyed, stable-ABI surface: an opaque [`PyInitConfig`]
//! created/freed by the runtime (so no struct layout is ABI), typed
//! `Set{Int,Str,StrList}` setters keyed by option *name*, and a
//! runtime read side (`PyConfig_Get` / `PyConfig_GetInt` /
//! `PyConfig_Names`) that works after initialization. Embedders
//! shipping dual 3.13/3.14 support (PyO3, pybind11) probe for these
//! symbols; landing them against the 3.13 core means those embedders
//! compile against WeavePy unchanged.
//!
//! The option names are the `PyConfig` field names (plus the
//! pre-configuration names and `"gil"`), exactly as CPython documents.
//! Everything routes into the RFC 0075 [`crate::initconfig`] core, so
//! the two APIs cannot drift: a `PyInitConfig` *is* a `PyConfig` plus
//! error state, and `Py_InitializeFromInitConfig` is
//! `Py_InitializeFromConfig` with PEP 741 error reporting.
//!
//! Per the RFC, only the *read* runtime surface lands here
//! (`PyConfig_Set` is 3.14's mutable-runtime-option story and joins
//! the version-switch wave).

use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int};
use std::ptr;

use weavepy_vm::object::Object;

use crate::embed::PyInitFn;
use crate::initconfig::{
    self, _PyStatus_TYPE_ERROR, _PyStatus_TYPE_EXIT, EmbedConfig, PyConfig,
    PyConfig_InitIsolatedConfig, PyStatus, PyWideStringList,
};
use crate::object::PyObject;

// ---------------------------------------------------------------------------
// The opaque PyInitConfig
// ---------------------------------------------------------------------------

/// Opaque to embedders (PEP 741's core ABI point: no struct layout is
/// exposed, so the config can grow fields forever).
pub struct PyInitConfig {
    config: PyConfig,
    // Pre-configuration options (PEP 741 folds PyPreConfig's knobs
    // into the same name space).
    configure_locale: i64,
    coerce_c_locale: i64,
    coerce_c_locale_warn: i64,
    utf8_mode: i64,
    allocator: i64,
    /// `SetInt("gil", 0|1)` — recorded and forwarded as the
    /// `-X gil=N` xoption the WS11 runtime mode consumes.
    gil: Option<i64>,
    /// `PyInitConfig_AddModule` registrations, applied to the inittab
    /// at initialize time.
    modules: Vec<(String, PyInitFn)>,
    err_msg: Option<CString>,
    exitcode: Option<c_int>,
}

impl PyInitConfig {
    fn set_error(&mut self, msg: &str) -> c_int {
        self.err_msg = Some(CString::new(msg.replace('\0', "?")).unwrap());
        -1
    }
}

#[no_mangle]
pub extern "C" fn PyInitConfig_Create() -> *mut PyInitConfig {
    // PEP 741: created configs start from the *isolated* defaults;
    // embedders opt back into environment/argv parsing by name.
    let mut config = unsafe { std::mem::zeroed::<PyConfig>() };
    unsafe { PyConfig_InitIsolatedConfig(&mut config) };
    Box::into_raw(Box::new(PyInitConfig {
        config,
        configure_locale: 0,
        coerce_c_locale: 0,
        coerce_c_locale_warn: 0,
        utf8_mode: 1,
        allocator: 0,
        gil: None,
        modules: Vec::new(),
        err_msg: None,
        exitcode: None,
    }))
}

#[no_mangle]
pub unsafe extern "C" fn PyInitConfig_Free(config: *mut PyInitConfig) {
    if config.is_null() {
        return;
    }
    let mut boxed = unsafe { Box::from_raw(config) };
    unsafe { initconfig::PyConfig_Clear(&mut boxed.config) };
    drop(boxed);
}

#[no_mangle]
pub unsafe extern "C" fn PyInitConfig_GetError(
    config: *mut PyInitConfig,
    err_msg: *mut *const c_char,
) -> c_int {
    if config.is_null() || err_msg.is_null() {
        return 0;
    }
    let c = unsafe { &*config };
    match &c.err_msg {
        Some(msg) => {
            unsafe { *err_msg = msg.as_ptr() };
            1
        }
        None => {
            unsafe { *err_msg = ptr::null() };
            0
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyInitConfig_GetExitCode(
    config: *mut PyInitConfig,
    exitcode: *mut c_int,
) -> c_int {
    if config.is_null() || exitcode.is_null() {
        return 0;
    }
    match unsafe { &*config }.exitcode {
        Some(code) => {
            unsafe { *exitcode = code };
            1
        }
        None => 0,
    }
}

// ---------------------------------------------------------------------------
// Option name tables
// ---------------------------------------------------------------------------

/// `c_int`-typed `PyConfig` fields addressable by name.
fn int_slot<'a>(c: &'a mut PyConfig, name: &str) -> Option<&'a mut c_int> {
    Some(match name {
        "isolated" => &mut c.isolated,
        "use_environment" => &mut c.use_environment,
        "dev_mode" => &mut c.dev_mode,
        "install_signal_handlers" => &mut c.install_signal_handlers,
        "use_hash_seed" => &mut c.use_hash_seed,
        "faulthandler" => &mut c.faulthandler,
        "tracemalloc" => &mut c.tracemalloc,
        "perf_profiling" => &mut c.perf_profiling,
        "import_time" => &mut c.import_time,
        "code_debug_ranges" => &mut c.code_debug_ranges,
        "show_ref_count" => &mut c.show_ref_count,
        "dump_refs" => &mut c.dump_refs,
        "malloc_stats" => &mut c.malloc_stats,
        "parse_argv" => &mut c.parse_argv,
        "site_import" => &mut c.site_import,
        "bytes_warning" => &mut c.bytes_warning,
        "warn_default_encoding" => &mut c.warn_default_encoding,
        "inspect" => &mut c.inspect,
        "interactive" => &mut c.interactive,
        "optimization_level" => &mut c.optimization_level,
        "parser_debug" => &mut c.parser_debug,
        "write_bytecode" => &mut c.write_bytecode,
        "verbose" => &mut c.verbose,
        "quiet" => &mut c.quiet,
        "user_site_directory" => &mut c.user_site_directory,
        "configure_c_stdio" => &mut c.configure_c_stdio,
        "buffered_stdio" => &mut c.buffered_stdio,
        "use_frozen_modules" => &mut c.use_frozen_modules,
        "safe_path" => &mut c.safe_path,
        "int_max_str_digits" => &mut c.int_max_str_digits,
        "cpu_count" => &mut c.cpu_count,
        "pathconfig_warnings" => &mut c.pathconfig_warnings,
        "skip_source_first_line" => &mut c.skip_source_first_line,
        "module_search_paths_set" => &mut c.module_search_paths_set,
        #[cfg(windows)]
        "legacy_windows_stdio" => &mut c.legacy_windows_stdio,
        _ => return None,
    })
}

/// Wide-string `PyConfig` fields addressable by name.
fn str_slot<'a>(c: &'a mut PyConfig, name: &str) -> Option<&'a mut *mut libc::wchar_t> {
    Some(match name {
        "dump_refs_file" => &mut c.dump_refs_file,
        "filesystem_encoding" => &mut c.filesystem_encoding,
        "filesystem_errors" => &mut c.filesystem_errors,
        "pycache_prefix" => &mut c.pycache_prefix,
        "stdio_encoding" => &mut c.stdio_encoding,
        "stdio_errors" => &mut c.stdio_errors,
        "check_hash_pycs_mode" => &mut c.check_hash_pycs_mode,
        "program_name" => &mut c.program_name,
        "pythonpath_env" => &mut c.pythonpath_env,
        "home" => &mut c.home,
        "platlibdir" => &mut c.platlibdir,
        "stdlib_dir" => &mut c.stdlib_dir,
        "executable" => &mut c.executable,
        "base_executable" => &mut c.base_executable,
        "prefix" => &mut c.prefix,
        "base_prefix" => &mut c.base_prefix,
        "exec_prefix" => &mut c.exec_prefix,
        "base_exec_prefix" => &mut c.base_exec_prefix,
        "run_command" => &mut c.run_command,
        "run_module" => &mut c.run_module,
        "run_filename" => &mut c.run_filename,
        "sys_path_0" => &mut c.sys_path_0,
        _ => return None,
    })
}

/// Wide-string-list `PyConfig` fields addressable by name.
fn list_slot<'a>(c: &'a mut PyConfig, name: &str) -> Option<&'a mut PyWideStringList> {
    Some(match name {
        "orig_argv" => &mut c.orig_argv,
        "argv" => &mut c.argv,
        "xoptions" => &mut c.xoptions,
        "warnoptions" => &mut c.warnoptions,
        "module_search_paths" => &mut c.module_search_paths,
        _ => return None,
    })
}

const INT_NAMES: &[&str] = &[
    "isolated",
    "use_environment",
    "dev_mode",
    "install_signal_handlers",
    "use_hash_seed",
    "hash_seed",
    "faulthandler",
    "tracemalloc",
    "perf_profiling",
    "import_time",
    "code_debug_ranges",
    "show_ref_count",
    "dump_refs",
    "malloc_stats",
    "parse_argv",
    "site_import",
    "bytes_warning",
    "warn_default_encoding",
    "inspect",
    "interactive",
    "optimization_level",
    "parser_debug",
    "write_bytecode",
    "verbose",
    "quiet",
    "user_site_directory",
    "configure_c_stdio",
    "buffered_stdio",
    "use_frozen_modules",
    "safe_path",
    "int_max_str_digits",
    "cpu_count",
    "pathconfig_warnings",
    "skip_source_first_line",
    "module_search_paths_set",
    // Pre-configuration + runtime-mode names.
    "configure_locale",
    "coerce_c_locale",
    "coerce_c_locale_warn",
    "utf8_mode",
    "allocator",
    "gil",
];

const STR_NAMES: &[&str] = &[
    "dump_refs_file",
    "filesystem_encoding",
    "filesystem_errors",
    "pycache_prefix",
    "stdio_encoding",
    "stdio_errors",
    "check_hash_pycs_mode",
    "program_name",
    "pythonpath_env",
    "home",
    "platlibdir",
    "stdlib_dir",
    "executable",
    "base_executable",
    "prefix",
    "base_prefix",
    "exec_prefix",
    "base_exec_prefix",
    "run_command",
    "run_module",
    "run_filename",
    "sys_path_0",
];

const LIST_NAMES: &[&str] = &[
    "orig_argv",
    "argv",
    "xoptions",
    "warnoptions",
    "module_search_paths",
];

fn is_known_option(name: &str) -> bool {
    INT_NAMES.contains(&name) || STR_NAMES.contains(&name) || LIST_NAMES.contains(&name)
}

unsafe fn option_name(name: *const c_char) -> Option<String> {
    if name.is_null() {
        return None;
    }
    Some(
        unsafe { CStr::from_ptr(name) }
            .to_string_lossy()
            .into_owned(),
    )
}

#[no_mangle]
pub unsafe extern "C" fn PyInitConfig_HasOption(
    _config: *mut PyInitConfig,
    name: *const c_char,
) -> c_int {
    let Some(name) = (unsafe { option_name(name) }) else {
        return 0;
    };
    is_known_option(&name) as c_int
}

// ---------------------------------------------------------------------------
// Setters
// ---------------------------------------------------------------------------

#[no_mangle]
#[allow(clippy::cast_possible_truncation)]
pub unsafe extern "C" fn PyInitConfig_SetInt(
    config: *mut PyInitConfig,
    name: *const c_char,
    value: i64,
) -> c_int {
    if config.is_null() {
        return -1;
    }
    let c = unsafe { &mut *config };
    let Some(name) = (unsafe { option_name(name) }) else {
        return c.set_error("PyInitConfig_SetInt: NULL option name");
    };
    match name.as_str() {
        "hash_seed" => {
            // CPython: setting a hash seed implies using it.
            c.config.hash_seed = value as libc::c_ulong;
            c.config.use_hash_seed = 1;
            return 0;
        }
        "configure_locale" => {
            c.configure_locale = value;
            return 0;
        }
        "coerce_c_locale" => {
            c.coerce_c_locale = value;
            return 0;
        }
        "coerce_c_locale_warn" => {
            c.coerce_c_locale_warn = value;
            return 0;
        }
        "utf8_mode" => {
            c.utf8_mode = value;
            return 0;
        }
        "allocator" => {
            c.allocator = value;
            return 0;
        }
        "gil" => {
            if value != 0 && value != 1 {
                return c.set_error(&format!("invalid \"gil\" value: {value}"));
            }
            c.gil = Some(value);
            return 0;
        }
        _ => {}
    }
    if let Some(slot) = int_slot(&mut c.config, &name) {
        if i64::from(c_int::MIN) > value || value > i64::from(c_int::MAX) {
            return c.set_error(&format!("\"{name}\" value is out of range: {value}"));
        }
        *slot = value as c_int;
        return 0;
    }
    if STR_NAMES.contains(&name.as_str()) || LIST_NAMES.contains(&name.as_str()) {
        return c.set_error(&format!("\"{name}\" is not an int option"));
    }
    c.set_error(&format!("unknown option name \"{name}\""))
}

#[no_mangle]
pub unsafe extern "C" fn PyInitConfig_SetStr(
    config: *mut PyInitConfig,
    name: *const c_char,
    value: *const c_char,
) -> c_int {
    if config.is_null() {
        return -1;
    }
    let c = unsafe { &mut *config };
    let Some(name) = (unsafe { option_name(name) }) else {
        return c.set_error("PyInitConfig_SetStr: NULL option name");
    };
    let Some(slot) = str_slot(&mut c.config, &name) else {
        if is_known_option(&name) {
            return c.set_error(&format!("\"{name}\" is not a string option"));
        }
        return c.set_error(&format!("unknown option name \"{name}\""));
    };
    if value.is_null() {
        return c.set_error(&format!("\"{name}\": NULL value"));
    }
    let decoded = unsafe { CStr::from_ptr(value) }
        .to_string_lossy()
        .into_owned();
    if unsafe { initconfig::set_wide(slot, &decoded) } {
        0
    } else {
        c.set_error("out of memory")
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyInitConfig_SetStrList(
    config: *mut PyInitConfig,
    name: *const c_char,
    length: usize,
    items: *const *const c_char,
) -> c_int {
    if config.is_null() {
        return -1;
    }
    let c = unsafe { &mut *config };
    let Some(name) = (unsafe { option_name(name) }) else {
        return c.set_error("PyInitConfig_SetStrList: NULL option name");
    };
    let Some(slot) = list_slot(&mut c.config, &name) else {
        if is_known_option(&name) {
            return c.set_error(&format!("\"{name}\" is not a string list option"));
        }
        return c.set_error(&format!("unknown option name \"{name}\""));
    };
    if length > 0 && items.is_null() {
        return c.set_error(&format!("\"{name}\": NULL items"));
    }
    let mut values = Vec::with_capacity(length);
    for i in 0..length {
        let item = unsafe { *items.add(i) };
        if item.is_null() {
            return c.set_error(&format!("\"{name}\": NULL item at index {i}"));
        }
        values.push(
            unsafe { CStr::from_ptr(item) }
                .to_string_lossy()
                .into_owned(),
        );
    }
    if !unsafe { initconfig::set_list(slot, &values) } {
        return c.set_error("out of memory");
    }
    // CPython: providing search paths by name marks them as set.
    if name == "module_search_paths" {
        c.config.module_search_paths_set = 1;
    }
    0
}

#[no_mangle]
pub unsafe extern "C" fn PyInitConfig_AddModule(
    config: *mut PyInitConfig,
    name: *const c_char,
    initfunc: Option<PyInitFn>,
) -> c_int {
    if config.is_null() {
        return -1;
    }
    let c = unsafe { &mut *config };
    let Some(name) = (unsafe { option_name(name) }) else {
        return c.set_error("PyInitConfig_AddModule: NULL module name");
    };
    let Some(initfunc) = initfunc else {
        return c.set_error("PyInitConfig_AddModule: NULL init function");
    };
    c.modules.push((name, initfunc));
    0
}

// ---------------------------------------------------------------------------
// Getters (pre-init: read back from the stored config)
// ---------------------------------------------------------------------------

#[no_mangle]
pub unsafe extern "C" fn PyInitConfig_GetInt(
    config: *mut PyInitConfig,
    name: *const c_char,
    value: *mut i64,
) -> c_int {
    if config.is_null() || value.is_null() {
        return -1;
    }
    let c = unsafe { &mut *config };
    let Some(name) = (unsafe { option_name(name) }) else {
        return c.set_error("PyInitConfig_GetInt: NULL option name");
    };
    let v: i64 = match name.as_str() {
        "hash_seed" => c.config.hash_seed as i64,
        "configure_locale" => c.configure_locale,
        "coerce_c_locale" => c.coerce_c_locale,
        "coerce_c_locale_warn" => c.coerce_c_locale_warn,
        "utf8_mode" => c.utf8_mode,
        "allocator" => c.allocator,
        "gil" => c.gil.unwrap_or(1),
        _ => match int_slot(&mut c.config, &name) {
            Some(slot) => i64::from(*slot),
            None => {
                if is_known_option(&name) {
                    return c.set_error(&format!("\"{name}\" is not an int option"));
                }
                return c.set_error(&format!("unknown option name \"{name}\""));
            }
        },
    };
    unsafe { *value = v };
    0
}

/// Allocate a `libc::malloc` C copy of `s` (freed by the embedder via
/// `PyMem_RawFree`, which is `free`).
fn alloc_cstr(s: &str) -> *mut c_char {
    let bytes = s.as_bytes();
    let buf = unsafe { libc::malloc(bytes.len() + 1) } as *mut c_char;
    if buf.is_null() {
        return ptr::null_mut();
    }
    unsafe {
        ptr::copy_nonoverlapping(bytes.as_ptr() as *const c_char, buf, bytes.len());
        *buf.add(bytes.len()) = 0;
    }
    buf
}

#[no_mangle]
pub unsafe extern "C" fn PyInitConfig_GetStr(
    config: *mut PyInitConfig,
    name: *const c_char,
    value: *mut *mut c_char,
) -> c_int {
    if config.is_null() || value.is_null() {
        return -1;
    }
    let c = unsafe { &mut *config };
    let Some(name) = (unsafe { option_name(name) }) else {
        return c.set_error("PyInitConfig_GetStr: NULL option name");
    };
    let Some(slot) = str_slot(&mut c.config, &name) else {
        if is_known_option(&name) {
            return c.set_error(&format!("\"{name}\" is not a string option"));
        }
        return c.set_error(&format!("unknown option name \"{name}\""));
    };
    match unsafe { initconfig::decode_wide_opt(*slot) } {
        // Unset optional strings read back as NULL with success, per
        // the PEP's "*value can be set to NULL" contract.
        None => {
            unsafe { *value = ptr::null_mut() };
            0
        }
        Some(s) => {
            let buf = alloc_cstr(&s);
            if buf.is_null() {
                return c.set_error("out of memory");
            }
            unsafe { *value = buf };
            0
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyInitConfig_GetStrList(
    config: *mut PyInitConfig,
    name: *const c_char,
    length: *mut usize,
    items: *mut *mut *mut c_char,
) -> c_int {
    if config.is_null() || length.is_null() || items.is_null() {
        return -1;
    }
    let c = unsafe { &mut *config };
    let Some(name) = (unsafe { option_name(name) }) else {
        return c.set_error("PyInitConfig_GetStrList: NULL option name");
    };
    let Some(slot) = list_slot(&mut c.config, &name) else {
        if is_known_option(&name) {
            return c.set_error(&format!("\"{name}\" is not a string list option"));
        }
        return c.set_error(&format!("unknown option name \"{name}\""));
    };
    let values = unsafe { initconfig::list_to_vec(slot) };
    let array = unsafe { libc::malloc(values.len().max(1) * std::mem::size_of::<*mut c_char>()) }
        as *mut *mut c_char;
    if array.is_null() {
        return c.set_error("out of memory");
    }
    for (i, v) in values.iter().enumerate() {
        let s = alloc_cstr(v);
        if s.is_null() {
            for j in 0..i {
                unsafe { libc::free(*array.add(j) as *mut libc::c_void) };
            }
            unsafe { libc::free(array as *mut libc::c_void) };
            return c.set_error("out of memory");
        }
        unsafe { *array.add(i) = s };
    }
    unsafe {
        *length = values.len();
        *items = array;
    }
    0
}

#[no_mangle]
pub unsafe extern "C" fn PyInitConfig_FreeStrList(length: usize, items: *mut *mut c_char) {
    if items.is_null() {
        return;
    }
    for i in 0..length {
        let item = unsafe { *items.add(i) };
        if !item.is_null() {
            unsafe { libc::free(item as *mut libc::c_void) };
        }
    }
    unsafe { libc::free(items as *mut libc::c_void) };
}

// ---------------------------------------------------------------------------
// Py_InitializeFromInitConfig
// ---------------------------------------------------------------------------

#[no_mangle]
pub unsafe extern "C" fn Py_InitializeFromInitConfig(config: *mut PyInitConfig) -> c_int {
    if config.is_null() {
        return -1;
    }
    let c = unsafe { &mut *config };
    c.err_msg = None;
    c.exitcode = None;
    // Inittab additions must precede initialization (the import
    // machinery consults the table during bootstrap).
    let modules = std::mem::take(&mut c.modules);
    for (name, initfunc) in modules {
        if !crate::embed::inittab_push(name.clone(), initfunc) {
            return c.set_error(&format!(
                "PyInitConfig_AddModule(\"{name}\"): interpreter already initialized"
            ));
        }
    }
    // `SetInt("gil", N)` travels as the `-X gil=N` xoption, the same
    // channel the CLI uses — one source of truth for the WS11 mode.
    if let Some(gil) = c.gil {
        let mut xoptions = unsafe { initconfig::list_to_vec(&c.config.xoptions) };
        if !xoptions.iter().any(|x| x.starts_with("gil=")) {
            xoptions.push(format!("gil={gil}"));
            if !unsafe { initconfig::set_list(&mut c.config.xoptions, &xoptions) } {
                return c.set_error("out of memory");
            }
        }
    }
    let decoded = match unsafe { initconfig::decode_config(&mut c.config) } {
        Ok(d) => d,
        Err(status) => return unsafe { store_status(c, status) },
    };
    let status = crate::embed::initialize(Some(decoded));
    if status.is_ok() {
        0
    } else {
        unsafe { store_status(c, status) }
    }
}

unsafe fn store_status(c: &mut PyInitConfig, status: PyStatus) -> c_int {
    if status._type == _PyStatus_TYPE_EXIT {
        c.exitcode = Some(status.exitcode);
        return c.set_error("Python exit");
    }
    if status._type == _PyStatus_TYPE_ERROR {
        let msg = if status.err_msg.is_null() {
            "initialization failed".to_owned()
        } else {
            unsafe { CStr::from_ptr(status.err_msg) }
                .to_string_lossy()
                .into_owned()
        };
        return c.set_error(&msg);
    }
    0
}

// ---------------------------------------------------------------------------
// The runtime read surface: PyConfig_Get / PyConfig_GetInt / PyConfig_Names
// ---------------------------------------------------------------------------

fn raise_value_error(msg: &str) {
    let c = CString::new(msg.replace('\0', "?")).unwrap();
    unsafe { crate::errors::PyErr_SetString(crate::errors::PyExc_ValueError, c.as_ptr()) };
}

fn raise_type_error(msg: &str) {
    let c = CString::new(msg.replace('\0', "?")).unwrap();
    unsafe { crate::errors::PyErr_SetString(crate::errors::PyExc_TypeError, c.as_ptr()) };
}

/// A live `sys` attribute as a *new* reference, or NULL.
fn sys_object(name: &str) -> *mut PyObject {
    let c = CString::new(name).unwrap();
    let borrowed = unsafe { crate::wave4::PySys_GetObject(c.as_ptr()) };
    if borrowed.is_null() {
        ptr::null_mut()
    } else {
        unsafe { crate::object::Py_NewRef(borrowed) }
    }
}

/// An integer attribute of `sys.flags`, `None` when unavailable.
fn sys_flag(attr: &str) -> Option<i64> {
    let c = CString::new(attr).unwrap();
    let flags = unsafe { crate::wave4::PySys_GetObject(c"flags".as_ptr()) };
    if flags.is_null() {
        return None;
    }
    let value = unsafe { crate::abstract_::PyObject_GetAttrString(flags, c.as_ptr()) };
    if value.is_null() {
        unsafe { crate::errors::PyErr_Clear() };
        return None;
    }
    let obj = unsafe { crate::object::clone_object(value) };
    unsafe { crate::object::Py_DecRef(value) };
    match obj {
        Object::Int(i) => Some(i),
        Object::Bool(b) => Some(i64::from(b)),
        _ => None,
    }
}

/// The `sys` attribute a config option reads through at runtime.
fn sys_name_for(option: &str) -> Option<&'static str> {
    Some(match option {
        "argv" => "argv",
        "orig_argv" => "orig_argv",
        "warnoptions" => "warnoptions",
        "module_search_paths" => "path",
        "xoptions" => "_xoptions",
        "executable" => "executable",
        "base_executable" => "_base_executable",
        "prefix" => "prefix",
        "base_prefix" => "base_prefix",
        "exec_prefix" => "exec_prefix",
        "base_exec_prefix" => "base_exec_prefix",
        "platlibdir" => "platlibdir",
        "pycache_prefix" => "pycache_prefix",
        "stdlib_dir" => "_stdlib_dir",
        _ => return None,
    })
}

/// `(option, sys.flags attribute, inverted)` for int options that are
/// live on `sys.flags` in host mode.
const FLAG_MAP: &[(&str, &str, bool)] = &[
    ("isolated", "isolated", false),
    ("use_environment", "ignore_environment", true),
    ("dev_mode", "dev_mode", false),
    ("optimization_level", "optimize", false),
    ("verbose", "verbose", false),
    ("quiet", "quiet", false),
    ("inspect", "inspect", false),
    ("interactive", "interactive", false),
    ("parser_debug", "debug", false),
    ("write_bytecode", "dont_write_bytecode", true),
    ("site_import", "no_site", true),
    ("user_site_directory", "no_user_site", true),
    ("bytes_warning", "bytes_warning", false),
    ("safe_path", "safe_path", false),
    ("int_max_str_digits", "int_max_str_digits", false),
];

/// Int options read from the RFC 0075 embed config when the process
/// was initialized from one.
fn snapshot_int(cfg: &EmbedConfig, option: &str) -> Option<i64> {
    Some(match option {
        "isolated" => i64::from(cfg.isolated),
        "use_environment" => i64::from(cfg.use_environment),
        "site_import" => i64::from(cfg.site_import),
        "user_site_directory" => i64::from(cfg.user_site_directory),
        "optimization_level" => i64::from(cfg.optimization_level),
        "write_bytecode" => i64::from(cfg.write_bytecode),
        "verbose" => i64::from(cfg.verbose),
        "quiet" => i64::from(cfg.quiet),
        "inspect" => i64::from(cfg.inspect),
        "buffered_stdio" => i64::from(cfg.buffered_stdio),
        "safe_path" => i64::from(cfg.safe_path),
        "install_signal_handlers" => i64::from(cfg.install_signal_handlers),
        "bytes_warning" => i64::from(cfg.bytes_warning),
        "int_max_str_digits" => cfg.int_max_str_digits?,
        "faulthandler" => i64::from(cfg.faulthandler),
        "tracemalloc" => i64::from(cfg.tracemalloc),
        "skip_source_first_line" => i64::from(cfg.skip_source_first_line),
        _ => return None,
    })
}

/// Defaults for int options with no live runtime mirror (a fresh
/// "python config" resolution; WeavePy is UTF-8-native).
fn default_int(option: &str) -> Option<i64> {
    Some(match option {
        "install_signal_handlers"
        | "configure_c_stdio"
        | "buffered_stdio"
        | "code_debug_ranges"
        | "use_frozen_modules"
        | "pathconfig_warnings"
        | "configure_locale"
        | "utf8_mode"
        | "module_search_paths_set" => 1,
        "use_hash_seed"
        | "hash_seed"
        | "faulthandler"
        | "tracemalloc"
        | "perf_profiling"
        | "import_time"
        | "show_ref_count"
        | "dump_refs"
        | "malloc_stats"
        | "parse_argv"
        | "warn_default_encoding"
        | "skip_source_first_line"
        | "coerce_c_locale"
        | "coerce_c_locale_warn"
        | "allocator"
        | "isolated"
        | "dev_mode"
        | "optimization_level"
        | "parser_debug"
        | "verbose"
        | "quiet"
        | "interactive"
        | "inspect"
        | "bytes_warning"
        | "safe_path" => 0,
        "use_environment" | "site_import" | "user_site_directory" | "write_bytecode" => 1,
        "int_max_str_digits" => 4300,
        "cpu_count" => -1,
        "gil" => i64::from(!weavepy_vm::gil::free_threading_enabled()),
        _ => return None,
    })
}

fn runtime_int(option: &str) -> Option<i64> {
    if option == "gil" {
        return Some(i64::from(!weavepy_vm::gil::free_threading_enabled()));
    }
    if let Some(cfg) = crate::embed::run_config_snapshot() {
        if let Some(v) = snapshot_int(&cfg, option) {
            return Some(v);
        }
    }
    if let Some((_, attr, invert)) = FLAG_MAP.iter().find(|(o, _, _)| *o == option) {
        if let Some(raw) = sys_flag(attr) {
            return Some(if *invert { i64::from(raw == 0) } else { raw });
        }
    }
    default_int(option)
}

/// String options with no live `sys` mirror, from the embed snapshot
/// or the read defaults.
fn runtime_str(option: &str) -> Option<Option<String>> {
    let cfg = crate::embed::run_config_snapshot();
    Some(match option {
        "run_command" => cfg.as_ref().and_then(|c| c.run_command.clone()),
        "run_module" => cfg.as_ref().and_then(|c| c.run_module.clone()),
        "run_filename" => cfg.as_ref().and_then(|c| c.run_filename.clone()),
        "program_name" => cfg.as_ref().and_then(|c| c.program_name.clone()),
        "home" => cfg.as_ref().and_then(|c| c.home.clone()),
        "pythonpath_env" => cfg.as_ref().and_then(|c| c.pythonpath_env.clone()),
        "stdio_encoding" => Some(
            cfg.as_ref()
                .and_then(|c| c.stdio_encoding.clone())
                .unwrap_or_else(|| "utf-8".to_owned()),
        ),
        "stdio_errors" => Some(
            cfg.as_ref()
                .and_then(|c| c.stdio_errors.clone())
                .unwrap_or_else(|| "strict".to_owned()),
        ),
        "filesystem_encoding" => Some("utf-8".to_owned()),
        "filesystem_errors" => Some(
            if cfg!(windows) {
                "surrogatepass"
            } else {
                "surrogateescape"
            }
            .to_owned(),
        ),
        "check_hash_pycs_mode" => Some("default".to_owned()),
        "dump_refs_file" | "sys_path_0" => None,
        _ => return None,
    })
}

#[no_mangle]
pub unsafe extern "C" fn PyConfig_Get(name: *const c_char) -> *mut PyObject {
    let Some(name) = (unsafe { option_name(name) }) else {
        raise_value_error("PyConfig_Get: NULL option name");
        return ptr::null_mut();
    };
    if !is_known_option(&name) {
        raise_value_error(&format!("unknown option name \"{name}\""));
        return ptr::null_mut();
    }
    // Live sys-backed options first (argv, paths, prefixes, …).
    if let Some(sys_name) = sys_name_for(&name) {
        let live = sys_object(sys_name);
        if !live.is_null() {
            return live;
        }
    }
    if let Some(v) = runtime_int(&name) {
        return crate::object::into_owned(Object::Int(v));
    }
    if let Some(v) = runtime_str(&name) {
        return match v {
            Some(s) => crate::object::into_owned(Object::from_str(s)),
            None => crate::object::into_owned(Object::None),
        };
    }
    // A list option whose sys mirror is unavailable (pre-init): empty.
    if LIST_NAMES.contains(&name.as_str()) {
        return crate::object::into_owned(Object::List(weavepy_vm::sync::Rc::new(
            weavepy_vm::sync::RefCell::new(Vec::new()),
        )));
    }
    raise_value_error(&format!("option \"{name}\" is not available"));
    ptr::null_mut()
}

#[no_mangle]
#[allow(clippy::cast_possible_truncation)]
pub unsafe extern "C" fn PyConfig_GetInt(name: *const c_char, value: *mut c_int) -> c_int {
    if value.is_null() {
        return -1;
    }
    let Some(name) = (unsafe { option_name(name) }) else {
        raise_value_error("PyConfig_GetInt: NULL option name");
        return -1;
    };
    if !is_known_option(&name) {
        raise_value_error(&format!("unknown option name \"{name}\""));
        return -1;
    }
    if !INT_NAMES.contains(&name.as_str()) {
        raise_type_error(&format!("option \"{name}\" is not an int"));
        return -1;
    }
    match runtime_int(&name) {
        Some(v) => {
            unsafe { *value = v as c_int };
            0
        }
        None => {
            raise_value_error(&format!("option \"{name}\" is not available"));
            -1
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyConfig_Names() -> *mut PyObject {
    let mut names: Vec<&str> = Vec::new();
    names.extend_from_slice(INT_NAMES);
    names.extend_from_slice(STR_NAMES);
    names.extend_from_slice(LIST_NAMES);
    names.sort_unstable();
    let items: Vec<Object> = names
        .into_iter()
        .map(|n| Object::from_str(n.to_owned()))
        .collect();
    let tuple = crate::object::into_owned(Object::Tuple(items.into()));
    let set = unsafe { crate::containers::PyFrozenSet_New(tuple) };
    unsafe { crate::object::Py_DecRef(tuple) };
    set
}

#[cfg(test)]
mod tests {
    use super::*;

    unsafe fn set_str(c: *mut PyInitConfig, name: &str, value: &str) -> c_int {
        let n = CString::new(name).unwrap();
        let v = CString::new(value).unwrap();
        unsafe { PyInitConfig_SetStr(c, n.as_ptr(), v.as_ptr()) }
    }

    #[test]
    fn create_set_get_round_trip() {
        unsafe {
            let c = PyInitConfig_Create();
            assert!(!c.is_null());

            // Isolated defaults.
            let mut v = -1i64;
            assert_eq!(PyInitConfig_GetInt(c, c"isolated".as_ptr(), &mut v), 0);
            assert_eq!(v, 1);

            // Int set/get.
            assert_eq!(PyInitConfig_SetInt(c, c"verbose".as_ptr(), 2), 0);
            assert_eq!(PyInitConfig_GetInt(c, c"verbose".as_ptr(), &mut v), 0);
            assert_eq!(v, 2);

            // hash_seed implies use_hash_seed.
            assert_eq!(PyInitConfig_SetInt(c, c"hash_seed".as_ptr(), 10), 0);
            assert_eq!(PyInitConfig_GetInt(c, c"use_hash_seed".as_ptr(), &mut v), 0);
            assert_eq!(v, 1);

            // Str set/get.
            assert_eq!(set_str(c, "program_name", "my_embedder"), 0);
            let mut s: *mut c_char = ptr::null_mut();
            assert_eq!(PyInitConfig_GetStr(c, c"program_name".as_ptr(), &mut s), 0);
            assert_eq!(
                CStr::from_ptr(s).to_string_lossy().into_owned(),
                "my_embedder"
            );
            libc::free(s as *mut libc::c_void);

            // Unset optional str reads back NULL with success.
            let mut s2: *mut c_char = 0xDEAD as *mut c_char;
            assert_eq!(PyInitConfig_GetStr(c, c"run_module".as_ptr(), &mut s2), 0);
            assert!(s2.is_null());

            // Str list round trip; module_search_paths marks the flag.
            let a = CString::new("/one").unwrap();
            let b = CString::new("/two").unwrap();
            let items = [a.as_ptr(), b.as_ptr()];
            assert_eq!(
                PyInitConfig_SetStrList(c, c"module_search_paths".as_ptr(), 2, items.as_ptr()),
                0
            );
            assert_eq!(
                PyInitConfig_GetInt(c, c"module_search_paths_set".as_ptr(), &mut v),
                0
            );
            assert_eq!(v, 1);
            let mut len = 0usize;
            let mut out: *mut *mut c_char = ptr::null_mut();
            assert_eq!(
                PyInitConfig_GetStrList(c, c"module_search_paths".as_ptr(), &mut len, &mut out),
                0
            );
            assert_eq!(len, 2);
            assert_eq!(CStr::from_ptr(*out).to_string_lossy(), "/one");
            assert_eq!(CStr::from_ptr(*out.add(1)).to_string_lossy(), "/two");
            PyInitConfig_FreeStrList(len, out);

            PyInitConfig_Free(c);
        }
    }

    #[test]
    fn errors_are_named_and_readable() {
        unsafe {
            let c = PyInitConfig_Create();

            // Unknown option.
            assert_eq!(PyInitConfig_SetInt(c, c"no_such_option".as_ptr(), 1), -1);
            let mut msg: *const c_char = ptr::null();
            assert_eq!(PyInitConfig_GetError(c, &mut msg), 1);
            let text = CStr::from_ptr(msg).to_string_lossy().into_owned();
            assert!(text.contains("no_such_option"), "{text}");

            // Type confusion is a distinct error.
            assert_eq!(PyInitConfig_SetInt(c, c"program_name".as_ptr(), 1), -1);
            assert_eq!(PyInitConfig_GetError(c, &mut msg), 1);
            let text = CStr::from_ptr(msg).to_string_lossy().into_owned();
            assert!(text.contains("not an int option"), "{text}");

            // Invalid gil value.
            assert_eq!(PyInitConfig_SetInt(c, c"gil".as_ptr(), 7), -1);
            // Valid gil value records and reads back.
            assert_eq!(PyInitConfig_SetInt(c, c"gil".as_ptr(), 0), 0);
            let mut v = -1i64;
            assert_eq!(PyInitConfig_GetInt(c, c"gil".as_ptr(), &mut v), 0);
            assert_eq!(v, 0);

            // HasOption.
            assert_eq!(PyInitConfig_HasOption(c, c"argv".as_ptr()), 1);
            assert_eq!(PyInitConfig_HasOption(c, c"utf8_mode".as_ptr()), 1);
            assert_eq!(PyInitConfig_HasOption(c, c"nope".as_ptr()), 0);

            PyInitConfig_Free(c);
        }
    }
}

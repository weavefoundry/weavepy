//! RFC 0075 WS2 — the `PyRun_*` execution family
//! (`Python/pythonrun.c` twins).
//!
//! Everything funnels through the VM's own `compile` / `eval` /
//! `exec` builtins, so start-token handling, `__builtins__`
//! injection, SyntaxError shaping, and globals/locals semantics are
//! exactly what Python-level `exec`/`eval` produce. The C surface
//! adds: `FILE*` plumbing (with the `closeit` contract), the
//! `handle_system_exit` process-exit discipline of the `Simple`
//! variants, and a `codeop`-backed interactive loop for
//! `PyRun_InteractiveOne/Loop`.

use std::os::raw::{c_char, c_int};

use weavepy_vm::object::{DictData, DictKey, Object, PyModule};
use weavepy_vm::sync::{Rc, RefCell};
use weavepy_vm::{Interpreter, RuntimeError};

use crate::object::PyObject;

// CPython's grammar start tokens (`Include/compile.h`).
pub const Py_single_input: c_int = 256;
pub const Py_file_input: c_int = 257;
pub const Py_eval_input: c_int = 258;
pub const Py_func_type_input: c_int = 345;

/// `errcode.h` E_EOF — `PyRun_InteractiveOne*`'s end-of-file result.
pub const E_EOF: c_int = 11;

/// `PyCompilerFlags` (`Include/cpython/compile.h`).
#[repr(C)]
pub struct PyCompilerFlags {
    pub cf_flags: c_int,
    pub cf_feature_version: c_int,
}

// ---------------------------------------------------------------------------
// Shared plumbing
// ---------------------------------------------------------------------------

/// The `__main__` module's dict, creating the module (CPython's
/// `PyImport_AddModule("__main__")`) if this is the first touch.
pub fn main_module_dict(interp: &mut Interpreter) -> Result<Rc<RefCell<DictData>>, RuntimeError> {
    let cache = interp.module_cache().clone();
    if let Some(Object::Module(m)) = cache.get("__main__") {
        return Ok(m.dict.clone());
    }
    let dict = Rc::new(RefCell::new(DictData::default()));
    dict.borrow_mut().insert(
        DictKey(Object::from_static("__name__")),
        Object::from_static("__main__"),
    );
    let module = Object::Module(Rc::new(PyModule {
        name: "__main__".to_owned(),
        filename: None,
        dict: dict.clone(),
    }));
    cache.insert("__main__", module);
    Ok(dict)
}

fn builtin_of(interp: &mut Interpreter, name: &'static str) -> Result<Object, RuntimeError> {
    let builtins = interp.builtins_dict();
    let found = builtins
        .borrow()
        .get(&DictKey(Object::from_static(name)))
        .cloned();
    found.ok_or_else(|| {
        weavepy_vm::error::runtime_error(format!("embedding: {name} builtin unavailable"))
    })
}

fn mode_for_start(start: c_int) -> Option<&'static str> {
    match start {
        Py_eval_input => Some("eval"),
        Py_file_input => Some("exec"),
        Py_single_input => Some("single"),
        _ => None,
    }
}

/// Compile `source` with the VM's `compile` builtin.
fn compile_source(
    interp: &mut Interpreter,
    source: &str,
    filename: &str,
    mode: &'static str,
    flags: c_int,
    optimize: c_int,
) -> Result<Object, RuntimeError> {
    let compile = builtin_of(interp, "compile")?;
    interp.call_object(
        compile,
        &[
            Object::from_str(source.to_owned()),
            Object::from_str(filename.to_owned()),
            Object::from_static(mode),
            Object::Int(i64::from(flags)),
            Object::Bool(false), // dont_inherit
            Object::Int(i64::from(optimize)),
        ],
        &[],
    )
}

/// Evaluate a code object against explicit globals/locals via the
/// VM's `eval` builtin (which accepts any-mode code objects, exactly
/// like CPython's `eval`).
fn eval_code_object(
    interp: &mut Interpreter,
    code: Object,
    globals: Object,
    locals: Object,
) -> Result<Object, RuntimeError> {
    let eval = builtin_of(interp, "eval")?;
    interp.call_object(eval, &[code, globals, locals], &[])
}

/// Read a whole `FILE*` (from the current position) into a String.
unsafe fn read_file_stream(fp: *mut libc::FILE) -> Option<String> {
    if fp.is_null() {
        return None;
    }
    let mut buf: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        let n = unsafe {
            libc::fread(
                chunk.as_mut_ptr().cast::<libc::c_void>(),
                1,
                chunk.len(),
                fp,
            )
        };
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    Some(String::from_utf8_lossy(&buf).into_owned())
}

/// Read one line from a `FILE*` (fgets semantics). `None` at EOF.
unsafe fn read_line(fp: *mut libc::FILE) -> Option<String> {
    let mut out = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        let p = unsafe { libc::fgets(buf.as_mut_ptr().cast::<c_char>(), buf.len() as c_int, fp) };
        if p.is_null() {
            if out.is_empty() {
                return None;
            }
            break;
        }
        let len = unsafe { libc::strlen(buf.as_ptr().cast::<c_char>()) };
        out.extend_from_slice(&buf[..len]);
        if out.last() == Some(&b'\n') {
            break;
        }
    }
    Some(String::from_utf8_lossy(&out).into_owned())
}

fn cstr_arg(p: *const c_char, what: &str) -> Result<String, ()> {
    if p.is_null() {
        crate::errors::set_runtime_error(format!("{what}: NULL string"));
        return Err(());
    }
    Ok(unsafe { std::ffi::CStr::from_ptr(p) }
        .to_string_lossy()
        .into_owned())
}

/// Report an uncaught exception through the interpreter's own
/// excepthook path; `SystemExit` follows CPython's
/// `handle_system_exit`: the *process* exits.
fn print_or_exit(interp: &mut Interpreter, err: RuntimeError) {
    if let RuntimeError::PyException(exc) = &err {
        if let Some(code) = exc.system_exit_code() {
            let status = match &code {
                Object::None => 0,
                Object::Int(i) => *i as i32,
                Object::Bool(b) => i32::from(*b),
                other => {
                    let msg = interp
                        .str_object(other)
                        .unwrap_or_else(|_| "<exit>".to_owned());
                    eprintln!("{msg}");
                    1
                }
            };
            let _ = interp.flush_streams();
            std::process::exit(status);
        }
        if interp.print_uncaught_exception(exc) {
            return;
        }
        eprintln!("{}: {}", exc.type_name(), exc.message());
        return;
    }
    eprintln!("InternalError: {err}");
}

/// Route a `Simple`-family source through the active embed
/// sub-interpreter, if `Py_NewInterpreter` made one current.
fn run_in_embed_subinterp(id: i64, source: &str) -> Option<c_int> {
    let module = crate::interp::with_interp_mut(|interp| interp.import_path("_xxsubinterpreters"))?;
    let Ok(Object::Module(m)) = module else {
        return Some(-1);
    };
    let run_string = m
        .dict
        .borrow()
        .get(&DictKey(Object::from_static("run_string")))
        .cloned()?;
    let result = crate::interp::with_interp_mut(|interp| {
        interp.call_object(
            run_string,
            &[Object::Int(id), Object::from_str(source.to_owned())],
            &[],
        )
    })?;
    Some(match result {
        Ok(_) => 0,
        Err(_) => -1,
    })
}

// ---------------------------------------------------------------------------
// PyRun_SimpleString
// ---------------------------------------------------------------------------

#[no_mangle]
pub unsafe extern "C" fn PyRun_SimpleStringFlags(
    command: *const c_char,
    _flags: *mut PyCompilerFlags,
) -> c_int {
    crate::interp::ensure_initialised();
    let Ok(source) = cstr_arg(command, "PyRun_SimpleStringFlags") else {
        return -1;
    };
    if let Some(id) = crate::embed::current_embed_subinterp() {
        if let Some(rc) = run_in_embed_subinterp(id, &source) {
            return rc;
        }
    }
    let outcome = crate::interp::with_interp_mut(|interp| {
        match crate::embed::exec_in_main(interp, &source, None) {
            Ok(_) => 0,
            Err(err) => {
                print_or_exit(interp, err);
                -1
            }
        }
    });
    outcome.unwrap_or_else(|| {
        crate::errors::set_runtime_error(
            "PyRun_SimpleString: interpreter not initialized".to_owned(),
        );
        -1
    })
}

#[no_mangle]
pub unsafe extern "C" fn PyRun_SimpleString(command: *const c_char) -> c_int {
    unsafe { PyRun_SimpleStringFlags(command, std::ptr::null_mut()) }
}

// ---------------------------------------------------------------------------
// PyRun_String / Py_CompileString / PyEval_EvalCode
// ---------------------------------------------------------------------------

unsafe fn run_string_impl(
    source: &str,
    filename: &str,
    start: c_int,
    globals: *mut PyObject,
    locals: *mut PyObject,
    flags: c_int,
) -> *mut PyObject {
    let Some(mode) = mode_for_start(start) else {
        crate::errors::set_runtime_error(format!("PyRun_String: bad start token {start}"));
        return std::ptr::null_mut();
    };
    if globals.is_null() {
        crate::errors::set_runtime_error("PyRun_String: NULL globals".to_owned());
        return std::ptr::null_mut();
    }
    let globals_obj = unsafe { crate::object::clone_object(globals) };
    if !matches!(globals_obj, Object::Dict(_)) {
        crate::errors::set_type_error("exec/eval: globals must be a dict".to_owned());
        return std::ptr::null_mut();
    }
    let locals_obj = if locals.is_null() {
        globals_obj.clone()
    } else {
        unsafe { crate::object::clone_object(locals) }
    };
    let filename = filename.to_owned();
    let source = source.to_owned();
    let result = crate::interp::with_interp_mut(move |interp| {
        let code = compile_source(interp, &source, &filename, mode, flags, -1)?;
        eval_code_object(interp, code, globals_obj, locals_obj)
    });
    match result {
        Some(Ok(obj)) => crate::object::into_owned(obj),
        Some(Err(err)) => {
            crate::errors::set_pending_from_runtime(err);
            std::ptr::null_mut()
        }
        None => {
            crate::errors::set_runtime_error("PyRun_String: no active interpreter".to_owned());
            std::ptr::null_mut()
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyRun_StringFlags(
    string: *const c_char,
    start: c_int,
    globals: *mut PyObject,
    locals: *mut PyObject,
    flags: *mut PyCompilerFlags,
) -> *mut PyObject {
    crate::interp::ensure_initialised();
    let Ok(source) = cstr_arg(string, "PyRun_StringFlags") else {
        return std::ptr::null_mut();
    };
    let cf = if flags.is_null() {
        0
    } else {
        unsafe { (*flags).cf_flags }
    };
    unsafe { run_string_impl(&source, "<string>", start, globals, locals, cf) }
}

#[no_mangle]
pub unsafe extern "C" fn PyRun_String(
    string: *const c_char,
    start: c_int,
    globals: *mut PyObject,
    locals: *mut PyObject,
) -> *mut PyObject {
    unsafe { PyRun_StringFlags(string, start, globals, locals, std::ptr::null_mut()) }
}

#[no_mangle]
pub unsafe extern "C" fn Py_CompileStringExFlags(
    string: *const c_char,
    filename: *const c_char,
    start: c_int,
    flags: *mut PyCompilerFlags,
    optimize: c_int,
) -> *mut PyObject {
    crate::interp::ensure_initialised();
    let Ok(source) = cstr_arg(string, "Py_CompileString") else {
        return std::ptr::null_mut();
    };
    let filename = if filename.is_null() {
        "<string>".to_owned()
    } else {
        unsafe { std::ffi::CStr::from_ptr(filename) }
            .to_string_lossy()
            .into_owned()
    };
    let Some(mode) = mode_for_start(start) else {
        crate::errors::set_runtime_error(format!("Py_CompileString: bad start token {start}"));
        return std::ptr::null_mut();
    };
    let cf = if flags.is_null() {
        0
    } else {
        unsafe { (*flags).cf_flags }
    };
    let result = crate::interp::with_interp_mut(|interp| {
        compile_source(interp, &source, &filename, mode, cf, optimize)
    });
    match result {
        Some(Ok(code)) => crate::object::into_owned(code),
        Some(Err(err)) => {
            crate::errors::set_pending_from_runtime(err);
            std::ptr::null_mut()
        }
        None => {
            crate::errors::set_runtime_error("Py_CompileString: no active interpreter".to_owned());
            std::ptr::null_mut()
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn Py_CompileStringFlags(
    string: *const c_char,
    filename: *const c_char,
    start: c_int,
    flags: *mut PyCompilerFlags,
) -> *mut PyObject {
    unsafe { Py_CompileStringExFlags(string, filename, start, flags, -1) }
}

#[no_mangle]
pub unsafe extern "C" fn Py_CompileString(
    string: *const c_char,
    filename: *const c_char,
    start: c_int,
) -> *mut PyObject {
    unsafe { Py_CompileStringExFlags(string, filename, start, std::ptr::null_mut(), -1) }
}

#[no_mangle]
pub unsafe extern "C" fn PyEval_EvalCode(
    code: *mut PyObject,
    globals: *mut PyObject,
    locals: *mut PyObject,
) -> *mut PyObject {
    crate::interp::ensure_initialised();
    if code.is_null() {
        crate::errors::set_runtime_error("PyEval_EvalCode: NULL code".to_owned());
        return std::ptr::null_mut();
    }
    let code_obj = unsafe { crate::object::clone_object(code) };
    let globals_obj = if globals.is_null() {
        crate::errors::set_runtime_error("PyEval_EvalCode: NULL globals".to_owned());
        return std::ptr::null_mut();
    } else {
        unsafe { crate::object::clone_object(globals) }
    };
    let locals_obj = if locals.is_null() {
        globals_obj.clone()
    } else {
        unsafe { crate::object::clone_object(locals) }
    };
    let result = crate::interp::with_interp_mut(|interp| {
        eval_code_object(interp, code_obj, globals_obj, locals_obj)
    });
    match result {
        Some(Ok(obj)) => crate::object::into_owned(obj),
        Some(Err(err)) => {
            crate::errors::set_pending_from_runtime(err);
            std::ptr::null_mut()
        }
        None => {
            crate::errors::set_runtime_error("PyEval_EvalCode: no active interpreter".to_owned());
            std::ptr::null_mut()
        }
    }
}

// ---------------------------------------------------------------------------
// File variants
// ---------------------------------------------------------------------------

#[no_mangle]
pub unsafe extern "C" fn PyRun_FileExFlags(
    fp: *mut libc::FILE,
    filename: *const c_char,
    start: c_int,
    globals: *mut PyObject,
    locals: *mut PyObject,
    closeit: c_int,
    flags: *mut PyCompilerFlags,
) -> *mut PyObject {
    crate::interp::ensure_initialised();
    let Some(source) = (unsafe { read_file_stream(fp) }) else {
        crate::errors::set_runtime_error("PyRun_File: NULL file".to_owned());
        return std::ptr::null_mut();
    };
    if closeit != 0 {
        unsafe { libc::fclose(fp) };
    }
    let filename = if filename.is_null() {
        "???".to_owned()
    } else {
        unsafe { std::ffi::CStr::from_ptr(filename) }
            .to_string_lossy()
            .into_owned()
    };
    let cf = if flags.is_null() {
        0
    } else {
        unsafe { (*flags).cf_flags }
    };
    unsafe { run_string_impl(&source, &filename, start, globals, locals, cf) }
}

#[no_mangle]
pub unsafe extern "C" fn PyRun_File(
    fp: *mut libc::FILE,
    filename: *const c_char,
    start: c_int,
    globals: *mut PyObject,
    locals: *mut PyObject,
) -> *mut PyObject {
    unsafe {
        PyRun_FileExFlags(
            fp,
            filename,
            start,
            globals,
            locals,
            0,
            std::ptr::null_mut(),
        )
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyRun_FileEx(
    fp: *mut libc::FILE,
    filename: *const c_char,
    start: c_int,
    globals: *mut PyObject,
    locals: *mut PyObject,
    closeit: c_int,
) -> *mut PyObject {
    unsafe {
        PyRun_FileExFlags(
            fp,
            filename,
            start,
            globals,
            locals,
            closeit,
            std::ptr::null_mut(),
        )
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyRun_FileFlags(
    fp: *mut libc::FILE,
    filename: *const c_char,
    start: c_int,
    globals: *mut PyObject,
    locals: *mut PyObject,
    flags: *mut PyCompilerFlags,
) -> *mut PyObject {
    unsafe { PyRun_FileExFlags(fp, filename, start, globals, locals, 0, flags) }
}

#[no_mangle]
pub unsafe extern "C" fn PyRun_SimpleFileExFlags(
    fp: *mut libc::FILE,
    filename: *const c_char,
    closeit: c_int,
    _flags: *mut PyCompilerFlags,
) -> c_int {
    crate::interp::ensure_initialised();
    let Some(source) = (unsafe { read_file_stream(fp) }) else {
        return -1;
    };
    if closeit != 0 {
        unsafe { libc::fclose(fp) };
    }
    let filename = if filename.is_null() {
        "???".to_owned()
    } else {
        unsafe { std::ffi::CStr::from_ptr(filename) }
            .to_string_lossy()
            .into_owned()
    };
    let outcome = crate::interp::with_interp_mut(|interp| {
        match crate::embed::exec_in_main(interp, &source, Some(&filename)) {
            Ok(_) => 0,
            Err(err) => {
                print_or_exit(interp, err);
                -1
            }
        }
    });
    outcome.unwrap_or(-1)
}

#[no_mangle]
pub unsafe extern "C" fn PyRun_SimpleFile(fp: *mut libc::FILE, filename: *const c_char) -> c_int {
    unsafe { PyRun_SimpleFileExFlags(fp, filename, 0, std::ptr::null_mut()) }
}

#[no_mangle]
pub unsafe extern "C" fn PyRun_SimpleFileEx(
    fp: *mut libc::FILE,
    filename: *const c_char,
    closeit: c_int,
) -> c_int {
    unsafe { PyRun_SimpleFileExFlags(fp, filename, closeit, std::ptr::null_mut()) }
}

#[no_mangle]
pub unsafe extern "C" fn PyRun_AnyFileExFlags(
    fp: *mut libc::FILE,
    filename: *const c_char,
    closeit: c_int,
    flags: *mut PyCompilerFlags,
) -> c_int {
    let interactive = !fp.is_null() && unsafe { libc::isatty(libc::fileno(fp)) } == 1;
    if interactive {
        let rc = unsafe { PyRun_InteractiveLoopFlags(fp, filename, flags) };
        if closeit != 0 {
            unsafe { libc::fclose(fp) };
        }
        rc
    } else {
        unsafe { PyRun_SimpleFileExFlags(fp, filename, closeit, flags) }
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyRun_AnyFile(fp: *mut libc::FILE, filename: *const c_char) -> c_int {
    unsafe { PyRun_AnyFileExFlags(fp, filename, 0, std::ptr::null_mut()) }
}

#[no_mangle]
pub unsafe extern "C" fn PyRun_AnyFileEx(
    fp: *mut libc::FILE,
    filename: *const c_char,
    closeit: c_int,
) -> c_int {
    unsafe { PyRun_AnyFileExFlags(fp, filename, closeit, std::ptr::null_mut()) }
}

#[no_mangle]
pub unsafe extern "C" fn PyRun_AnyFileFlags(
    fp: *mut libc::FILE,
    filename: *const c_char,
    flags: *mut PyCompilerFlags,
) -> c_int {
    unsafe { PyRun_AnyFileExFlags(fp, filename, 0, flags) }
}

// ---------------------------------------------------------------------------
// Interactive loop
// ---------------------------------------------------------------------------

/// `sys.ps1` / `sys.ps2`, installing CPython's defaults on first use.
fn prompt(interp: &mut Interpreter, which: &'static str, default: &'static str) -> String {
    let Ok(Object::Module(sys)) = interp.import_path("sys") else {
        return default.to_owned();
    };
    let existing = sys
        .dict
        .borrow()
        .get(&DictKey(Object::from_static(which)))
        .cloned();
    match existing {
        Some(v) => interp.str_object(&v).unwrap_or_else(|_| default.to_owned()),
        None => {
            sys.dict.borrow_mut().insert(
                DictKey(Object::from_static(which)),
                Object::from_static(default),
            );
            default.to_owned()
        }
    }
}

/// Is `source` a complete interactive command? Routed through the
/// VM's own `codeop.compile_command` (None → incomplete).
fn command_complete(interp: &mut Interpreter, source: &str) -> Result<bool, RuntimeError> {
    let Object::Module(codeop) = interp.import_path("codeop")? else {
        return Ok(true);
    };
    let compile_command = codeop
        .dict
        .borrow()
        .get(&DictKey(Object::from_static("compile_command")))
        .cloned();
    let Some(cc) = compile_command else {
        return Ok(true);
    };
    let r = interp.call_object(cc, &[Object::from_str(source.to_owned())], &[])?;
    Ok(!matches!(r, Object::None))
}

#[no_mangle]
pub unsafe extern "C" fn PyRun_InteractiveOneFlags(
    fp: *mut libc::FILE,
    filename: *const c_char,
    _flags: *mut PyCompilerFlags,
) -> c_int {
    crate::interp::ensure_initialised();
    let filename = if filename.is_null() {
        "<stdin>".to_owned()
    } else {
        unsafe { std::ffi::CStr::from_ptr(filename) }
            .to_string_lossy()
            .into_owned()
    };
    let outcome = crate::interp::with_interp_mut(|interp| {
        let ps1 = prompt(interp, "ps1", ">>> ");
        let ps2 = prompt(interp, "ps2", "... ");
        eprint!("{ps1}");
        let Some(mut source) = (unsafe { read_line(fp) }) else {
            return E_EOF;
        };
        // Accumulate continuation lines until codeop says the command
        // is complete (or a blank line closes a block at EOF).
        loop {
            match command_complete(interp, source.trim_end_matches('\n')) {
                Ok(true) => break,
                Ok(false) => {
                    eprint!("{ps2}");
                    match unsafe { read_line(fp) } {
                        Some(line) => {
                            if line == "\n" && source.ends_with("\n\n") {
                                break;
                            }
                            source.push_str(&line);
                        }
                        None => break,
                    }
                }
                Err(_) => break, // syntax error: let the real compile report it
            }
        }
        // `single` mode: expression results print via sys.displayhook.
        let main_dict = match main_module_dict(interp) {
            Ok(d) => d,
            Err(_) => return -1,
        };
        let run = compile_source(interp, &source, &filename, "single", 0, -1).and_then(|code| {
            eval_code_object(
                interp,
                code,
                Object::Dict(main_dict.clone()),
                Object::Dict(main_dict),
            )
        });
        match run {
            Ok(_) => 0,
            Err(err) => {
                print_or_exit(interp, err);
                -1
            }
        }
    });
    outcome.unwrap_or(-1)
}

#[no_mangle]
pub unsafe extern "C" fn PyRun_InteractiveOne(
    fp: *mut libc::FILE,
    filename: *const c_char,
) -> c_int {
    unsafe { PyRun_InteractiveOneFlags(fp, filename, std::ptr::null_mut()) }
}

#[no_mangle]
pub unsafe extern "C" fn PyRun_InteractiveLoopFlags(
    fp: *mut libc::FILE,
    filename: *const c_char,
    flags: *mut PyCompilerFlags,
) -> c_int {
    loop {
        let rc = unsafe { PyRun_InteractiveOneFlags(fp, filename, flags) };
        if rc == E_EOF {
            return 0;
        }
        // Errors print and continue, exactly like the REPL.
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyRun_InteractiveLoop(
    fp: *mut libc::FILE,
    filename: *const c_char,
) -> c_int {
    unsafe { PyRun_InteractiveLoopFlags(fp, filename, std::ptr::null_mut()) }
}

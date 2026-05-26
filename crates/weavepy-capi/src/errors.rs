//! Error machinery: `PyErr_Set*`, `PyErr_Occurred`, the `PyExc_*`
//! exception statics, and the bridge between a thread-local
//! "pending exception" cell and the [`weavepy_vm::error::RuntimeError`]
//! the VM speaks.
//!
//! ## Pending-exception model
//!
//! When a C extension function decides to fail, it (a) calls one
//! of the `PyErr_Set*` family to install a pending exception,
//! and (b) returns `NULL` (or `-1` for int-returning functions)
//! to its caller. Eventually control returns to the VM, which
//! checks the cell, converts to a `RuntimeError`, and propagates.
//!
//! The cell is per-thread (CPython does the same). It carries a
//! `(type, value, traceback)` triple where:
//!
//! - `type`  — a `PyExc_*` static or any other type object,
//! - `value` — usually a string message but may be any object,
//! - `traceback` — currently `None` (we don't synthesise tracebacks
//!    for C-side errors yet).

use std::ffi::CStr;
use std::os::raw::{c_char, c_int};
use std::ptr;
use std::sync::Mutex;
use weavepy_vm::sync::Rc;
use weavepy_vm::sync::RefCell;

use weavepy_vm::builtin_types::{builtin_types, make_exception};
use weavepy_vm::error::{PyException, RuntimeError};
use weavepy_vm::object::{DictData, DictKey, Object};
use weavepy_vm::types::TypeObject;

use crate::object::PyObject;

thread_local! {
    static PENDING: RefCell<Option<PendingError>> = const { RefCell::new(None) };
}

#[derive(Clone)]
pub struct PendingError {
    pub ty: Option<Rc<TypeObject>>,
    pub value: Object,
}

/// Clear the per-thread pending error cell. Called at the start of
/// every extension call.
pub fn clear_thread_local() {
    PENDING.with(|cell| cell.borrow_mut().take());
}

/// Install `(ty, value)` as the pending exception. Replaces any
/// previously-pending error.
pub fn set_pending(ty: Option<Rc<TypeObject>>, value: Object) {
    PENDING.with(|cell| {
        *cell.borrow_mut() = Some(PendingError { ty, value });
    });
}

/// Read the pending exception, leaving the cell intact. The
/// optional `Rc<TypeObject>` is set when the caller installed
/// one explicitly via `PyErr_SetObject` / `PyErr_SetString` (we
/// always carry a class reference for those).
pub fn pending() -> Option<PendingError> {
    PENDING.with(|cell| cell.borrow().clone())
}

/// Take the pending exception out of the cell.
pub fn take_pending() -> Option<PendingError> {
    PENDING.with(|cell| cell.borrow_mut().take())
}

/// Take the pending exception out of the cell and convert to a
/// [`RuntimeError`] suitable for returning from VM-facing trampolines.
pub fn take_pending_error_runtime() -> Option<RuntimeError> {
    take_pending().map(to_runtime_error)
}

/// Convert a [`PendingError`] to a [`RuntimeError`] suitable for
/// returning from the VM.
pub fn to_runtime_error(p: PendingError) -> RuntimeError {
    let class =
        p.ty.unwrap_or_else(|| builtin_types().runtime_error.clone());
    let inst = make_exception(&class.name, message_for(&p.value));
    RuntimeError::PyException(PyException::new(inst))
}

fn message_for(o: &Object) -> String {
    match o {
        Object::Str(s) => s.to_string(),
        Object::Instance(inst) => {
            let key = DictKey(Object::from_static("args"));
            if let Some(args) = inst.dict.borrow().get(&key).cloned() {
                if let Object::Tuple(items) = args {
                    if let Some(Object::Str(s)) = items.first().cloned() {
                        return s.to_string();
                    }
                }
            }
            format!("<{}>", inst.class.name)
        }
        Object::None => String::new(),
        _ => format!("{o:?}"),
    }
}

/// Ergonomic helper used by Rust-side bridge code that wants to
/// install a synthetic `RuntimeError`.
pub fn set_runtime_error(msg: impl Into<String>) {
    set_pending(
        Some(builtin_types().runtime_error.clone()),
        Object::from_str(msg.into()),
    );
}

/// Helper used by argument-parsing code to install a `TypeError`.
pub fn set_type_error(msg: impl Into<String>) {
    set_pending(
        Some(builtin_types().type_error.clone()),
        Object::from_str(msg.into()),
    );
}

pub fn set_value_error(msg: impl Into<String>) {
    set_pending(
        Some(builtin_types().value_error.clone()),
        Object::from_str(msg.into()),
    );
}

pub fn set_overflow_error(msg: impl Into<String>) {
    set_pending(
        Some(builtin_types().overflow_error.clone()),
        Object::from_str(msg.into()),
    );
}

/// Helper used by buffer-protocol code to install a `BufferError`.
pub fn set_buffer_error(msg: impl Into<String>) {
    set_pending(
        Some(builtin_types().buffer_error.clone()),
        Object::from_str(msg.into()),
    );
}

/// Helper used by attribute-lookup code to install an `AttributeError`.
pub fn set_attribute_error(msg: impl Into<String>) {
    set_pending(
        Some(builtin_types().attribute_error.clone()),
        Object::from_str(msg.into()),
    );
}

/// Helper used by descriptor / generic-allocator code to install a
/// `RuntimeError`.
pub fn set_index_error(msg: impl Into<String>) {
    set_pending(
        Some(builtin_types().index_error.clone()),
        Object::from_str(msg.into()),
    );
}

/// Helper used by stop-iteration paths.
pub fn set_stop_iteration() {
    set_pending(Some(builtin_types().stop_iteration.clone()), Object::None);
}

// ----------------------------------------------------------------
// Static `PyExc_*` exception pointers.
//
// CPython exposes these as `PyObject *PyExc_TypeError;` — i.e. a
// data symbol that holds a pointer to the actual type object. We
// match that ABI: each statically-allocated cell starts null and is
// filled in by `init_static_exceptions` at first use.
// ----------------------------------------------------------------

macro_rules! exc_cell {
    ($($name:ident);* $(;)?) => {
        $(
            #[no_mangle]
            pub static mut $name: *mut PyObject = ptr::null_mut();
        )*
    };
}

exc_cell! {
    PyExc_BaseException;
    PyExc_Exception;
    PyExc_ArithmeticError;
    PyExc_AssertionError;
    PyExc_AttributeError;
    PyExc_BufferError;
    PyExc_EOFError;
    PyExc_FloatingPointError;
    PyExc_GeneratorExit;
    PyExc_ImportError;
    PyExc_IndentationError;
    PyExc_IndexError;
    PyExc_KeyError;
    PyExc_KeyboardInterrupt;
    PyExc_LookupError;
    PyExc_MemoryError;
    PyExc_ModuleNotFoundError;
    PyExc_NameError;
    PyExc_NotImplementedError;
    PyExc_OSError;
    PyExc_OverflowError;
    PyExc_RecursionError;
    PyExc_ReferenceError;
    PyExc_RuntimeError;
    PyExc_StopAsyncIteration;
    PyExc_StopIteration;
    PyExc_SyntaxError;
    PyExc_SystemError;
    PyExc_SystemExit;
    PyExc_TabError;
    PyExc_TimeoutError;
    PyExc_TypeError;
    PyExc_UnboundLocalError;
    PyExc_UnicodeDecodeError;
    PyExc_UnicodeEncodeError;
    PyExc_UnicodeError;
    PyExc_UnicodeTranslateError;
    PyExc_ValueError;
    PyExc_ZeroDivisionError;
    PyExc_BlockingIOError;
    PyExc_BrokenPipeError;
    PyExc_ChildProcessError;
    PyExc_ConnectionAbortedError;
    PyExc_ConnectionError;
    PyExc_ConnectionRefusedError;
    PyExc_ConnectionResetError;
    PyExc_FileExistsError;
    PyExc_FileNotFoundError;
    PyExc_InterruptedError;
    PyExc_IsADirectoryError;
    PyExc_NotADirectoryError;
    PyExc_PermissionError;
    PyExc_ProcessLookupError;
    PyExc_Warning;
    PyExc_UserWarning;
    PyExc_DeprecationWarning;
    PyExc_PendingDeprecationWarning;
    PyExc_SyntaxWarning;
    PyExc_RuntimeWarning;
    PyExc_FutureWarning;
    PyExc_ImportWarning;
    PyExc_UnicodeWarning;
    PyExc_BytesWarning;
    PyExc_ResourceWarning;
}

pub fn init_static_exceptions() {
    static INIT_LOCK: Mutex<bool> = Mutex::new(false);
    let mut done = INIT_LOCK.lock().unwrap();
    if *done {
        return;
    }
    *done = true;
    let bt = builtin_types();
    let publish = |slot: *mut *mut PyObject, ty: Rc<TypeObject>| {
        let p = crate::object::into_owned(Object::Type(ty));
        // The type singleton is immortal; we don't need to track
        // an extra reference.
        unsafe { *slot = p };
    };
    unsafe {
        publish(&raw mut PyExc_BaseException, bt.base_exception.clone());
        publish(&raw mut PyExc_Exception, bt.exception.clone());
        publish(&raw mut PyExc_ArithmeticError, bt.arithmetic_error.clone());
        publish(&raw mut PyExc_AssertionError, bt.assertion_error.clone());
        publish(&raw mut PyExc_AttributeError, bt.attribute_error.clone());
        publish(&raw mut PyExc_BufferError, bt.buffer_error.clone());
        publish(&raw mut PyExc_EOFError, bt.eof_error.clone());
        publish(
            &raw mut PyExc_FloatingPointError,
            bt.arithmetic_error.clone(),
        );
        publish(&raw mut PyExc_GeneratorExit, bt.generator_exit.clone());
        publish(&raw mut PyExc_ImportError, bt.import_error.clone());
        publish(&raw mut PyExc_IndentationError, bt.syntax_error.clone());
        publish(&raw mut PyExc_IndexError, bt.index_error.clone());
        publish(&raw mut PyExc_KeyError, bt.key_error.clone());
        publish(
            &raw mut PyExc_KeyboardInterrupt,
            bt.keyboard_interrupt.clone(),
        );
        publish(&raw mut PyExc_LookupError, bt.lookup_error.clone());
        publish(&raw mut PyExc_MemoryError, bt.memory_error.clone());
        publish(
            &raw mut PyExc_ModuleNotFoundError,
            bt.module_not_found_error.clone(),
        );
        publish(&raw mut PyExc_NameError, bt.name_error.clone());
        publish(
            &raw mut PyExc_NotImplementedError,
            bt.not_implemented_error.clone(),
        );
        publish(&raw mut PyExc_OSError, bt.os_error.clone());
        publish(&raw mut PyExc_OverflowError, bt.overflow_error.clone());
        publish(&raw mut PyExc_RecursionError, bt.recursion_error.clone());
        publish(&raw mut PyExc_ReferenceError, bt.runtime_error.clone());
        publish(&raw mut PyExc_RuntimeError, bt.runtime_error.clone());
        publish(
            &raw mut PyExc_StopAsyncIteration,
            bt.stop_async_iteration.clone(),
        );
        publish(&raw mut PyExc_StopIteration, bt.stop_iteration.clone());
        publish(&raw mut PyExc_SyntaxError, bt.syntax_error.clone());
        publish(&raw mut PyExc_SystemError, bt.runtime_error.clone());
        publish(&raw mut PyExc_SystemExit, bt.system_exit.clone());
        publish(&raw mut PyExc_TabError, bt.syntax_error.clone());
        publish(&raw mut PyExc_TimeoutError, bt.timeout_error.clone());
        publish(&raw mut PyExc_TypeError, bt.type_error.clone());
        publish(
            &raw mut PyExc_UnboundLocalError,
            bt.unbound_local_error.clone(),
        );
        publish(&raw mut PyExc_UnicodeDecodeError, bt.value_error.clone());
        publish(&raw mut PyExc_UnicodeEncodeError, bt.value_error.clone());
        publish(&raw mut PyExc_UnicodeError, bt.value_error.clone());
        publish(&raw mut PyExc_UnicodeTranslateError, bt.value_error.clone());
        publish(&raw mut PyExc_ValueError, bt.value_error.clone());
        publish(
            &raw mut PyExc_ZeroDivisionError,
            bt.zero_division_error.clone(),
        );
        publish(&raw mut PyExc_BlockingIOError, bt.blocking_io_error.clone());
        publish(&raw mut PyExc_BrokenPipeError, bt.broken_pipe_error.clone());
        publish(
            &raw mut PyExc_ChildProcessError,
            bt.child_process_error.clone(),
        );
        publish(
            &raw mut PyExc_ConnectionAbortedError,
            bt.connection_aborted_error.clone(),
        );
        publish(&raw mut PyExc_ConnectionError, bt.connection_error.clone());
        publish(
            &raw mut PyExc_ConnectionRefusedError,
            bt.connection_refused_error.clone(),
        );
        publish(
            &raw mut PyExc_ConnectionResetError,
            bt.connection_reset_error.clone(),
        );
        publish(&raw mut PyExc_FileExistsError, bt.file_exists_error.clone());
        publish(
            &raw mut PyExc_FileNotFoundError,
            bt.file_not_found_error.clone(),
        );
        publish(
            &raw mut PyExc_InterruptedError,
            bt.interrupted_error.clone(),
        );
        publish(
            &raw mut PyExc_IsADirectoryError,
            bt.is_a_directory_error.clone(),
        );
        publish(
            &raw mut PyExc_NotADirectoryError,
            bt.not_a_directory_error.clone(),
        );
        publish(&raw mut PyExc_PermissionError, bt.permission_error.clone());
        publish(
            &raw mut PyExc_ProcessLookupError,
            bt.process_lookup_error.clone(),
        );
        publish(&raw mut PyExc_Warning, bt.warning.clone());
        publish(&raw mut PyExc_UserWarning, bt.user_warning.clone());
        publish(
            &raw mut PyExc_DeprecationWarning,
            bt.deprecation_warning.clone(),
        );
        publish(
            &raw mut PyExc_PendingDeprecationWarning,
            bt.pending_deprecation_warning.clone(),
        );
        publish(&raw mut PyExc_SyntaxWarning, bt.syntax_warning.clone());
        publish(&raw mut PyExc_RuntimeWarning, bt.runtime_warning.clone());
        publish(&raw mut PyExc_FutureWarning, bt.future_warning.clone());
        publish(&raw mut PyExc_ImportWarning, bt.import_warning.clone());
        publish(&raw mut PyExc_UnicodeWarning, bt.unicode_warning.clone());
        publish(&raw mut PyExc_BytesWarning, bt.bytes_warning.clone());
        publish(&raw mut PyExc_ResourceWarning, bt.resource_warning.clone());
    }
}

// ----------------------------------------------------------------
// FFI surface.
// ----------------------------------------------------------------

/// `PyErr_SetString(type, msg)` — install `type(msg)` as pending.
#[no_mangle]
pub unsafe extern "C" fn PyErr_SetString(ty: *mut PyObject, msg: *const c_char) {
    crate::interp::ensure_initialised();
    let cls = type_object_for(ty);
    let s = if msg.is_null() {
        String::new()
    } else {
        unsafe { CStr::from_ptr(msg) }
            .to_string_lossy()
            .into_owned()
    };
    set_pending(cls, Object::from_str(s));
}

/// `PyErr_SetObject(type, value)` — install `value` (any object)
/// as the pending value, wrapped in a fresh exception instance of
/// `type` if it isn't already an instance of it.
#[no_mangle]
pub unsafe extern "C" fn PyErr_SetObject(ty: *mut PyObject, value: *mut PyObject) {
    crate::interp::ensure_initialised();
    let cls = type_object_for(ty);
    let v = if value.is_null() {
        Object::None
    } else {
        unsafe { crate::object::clone_object(value) }
    };
    set_pending(cls, v);
}

/// `PyErr_SetNone(type)` — install `type()` as pending.
#[no_mangle]
pub unsafe extern "C" fn PyErr_SetNone(ty: *mut PyObject) {
    crate::interp::ensure_initialised();
    set_pending(type_object_for(ty), Object::None);
}

/// `PyErr_Occurred()` returns the pending exception's *type*
/// pointer (or null if none). It does **not** consume the cell.
#[no_mangle]
pub unsafe extern "C" fn PyErr_Occurred() -> *mut PyObject {
    crate::interp::ensure_initialised();
    let Some(p) = pending() else {
        return ptr::null_mut();
    };
    let Some(ty) = p.ty else {
        return ptr::null_mut();
    };
    crate::object::into_owned(Object::Type(ty))
}

/// `PyErr_Clear()` — drop the pending exception.
#[no_mangle]
pub unsafe extern "C" fn PyErr_Clear() {
    clear_thread_local();
}

/// `PyErr_Print()` / `PyErr_PrintEx(int)` — print the pending
/// exception to stderr and clear the cell.
#[no_mangle]
pub unsafe extern "C" fn PyErr_Print() {
    unsafe { PyErr_PrintEx(1) };
}

#[no_mangle]
pub unsafe extern "C" fn PyErr_PrintEx(_set_sys_last: c_int) {
    let Some(p) = take_pending() else {
        return;
    };
    let name =
        p.ty.as_ref()
            .map_or_else(|| "Exception".to_owned(), |t| t.name.clone());
    eprintln!("{name}: {}", message_for(&p.value));
}

/// `PyErr_Fetch(&type, &value, &tb)` — atomically take the
/// pending exception out of the cell.
#[no_mangle]
pub unsafe extern "C" fn PyErr_Fetch(
    ptype: *mut *mut PyObject,
    pvalue: *mut *mut PyObject,
    ptb: *mut *mut PyObject,
) {
    let p = take_pending();
    let (ty, val) = match p {
        Some(p) => (
            p.ty.map(|t| crate::object::into_owned(Object::Type(t)))
                .unwrap_or(ptr::null_mut()),
            crate::object::into_owned(p.value),
        ),
        None => (ptr::null_mut(), ptr::null_mut()),
    };
    unsafe {
        if !ptype.is_null() {
            *ptype = ty;
        }
        if !pvalue.is_null() {
            *pvalue = val;
        }
        if !ptb.is_null() {
            *ptb = ptr::null_mut();
        }
    }
}

/// `PyErr_Restore(type, value, tb)` — re-install a previously-fetched
/// exception. Owns the references; we take them.
#[no_mangle]
pub unsafe extern "C" fn PyErr_Restore(
    ty: *mut PyObject,
    value: *mut PyObject,
    _tb: *mut PyObject,
) {
    let cls = type_object_for(ty);
    let v = if value.is_null() {
        Object::None
    } else {
        unsafe { crate::object::clone_object(value) }
    };
    set_pending(cls, v);
}

/// Match a given exception against a type (or tuple of types).
#[no_mangle]
pub unsafe extern "C" fn PyErr_GivenExceptionMatches(
    given: *mut PyObject,
    exc: *mut PyObject,
) -> c_int {
    if given.is_null() || exc.is_null() {
        return 0;
    }
    let given_ty = match unsafe { crate::object::clone_object(given) } {
        Object::Type(t) => t,
        Object::Instance(inst) => inst.class.clone(),
        _ => return 0,
    };
    let exc_obj = unsafe { crate::object::clone_object(exc) };
    matches_type(&given_ty, &exc_obj).into()
}

fn matches_type(given: &Rc<TypeObject>, exc: &Object) -> bool {
    match exc {
        Object::Type(t) => given.is_subclass_of(t),
        Object::Tuple(items) => items.iter().any(|item| matches_type(given, item)),
        _ => false,
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyErr_ExceptionMatches(exc: *mut PyObject) -> c_int {
    let Some(p) = pending() else {
        return 0;
    };
    let Some(ty) = p.ty else {
        return 0;
    };
    let exc_obj = if exc.is_null() {
        return 0;
    } else {
        unsafe { crate::object::clone_object(exc) }
    };
    matches_type(&ty, &exc_obj).into()
}

#[no_mangle]
pub unsafe extern "C" fn PyErr_NormalizeException(
    _exc: *mut *mut PyObject,
    _val: *mut *mut PyObject,
    _tb: *mut *mut PyObject,
) {
    // CPython transforms `(type, args_tuple)` into `(type, instance)`.
    // We never produce that intermediate shape, so this is a no-op.
}

#[no_mangle]
pub unsafe extern "C" fn PyErr_NoMemory() -> *mut PyObject {
    set_pending(
        Some(builtin_types().memory_error.clone()),
        Object::from_static("out of memory"),
    );
    ptr::null_mut()
}

#[no_mangle]
pub unsafe extern "C" fn PyErr_BadArgument() -> c_int {
    set_pending(
        Some(builtin_types().type_error.clone()),
        Object::from_static("bad argument type for built-in operation"),
    );
    0
}

#[no_mangle]
pub unsafe extern "C" fn PyErr_BadInternalCall() {
    set_pending(
        Some(builtin_types().runtime_error.clone()),
        Object::from_static("bad argument to internal function"),
    );
}

#[no_mangle]
pub unsafe extern "C" fn PyErr_WarnEx(
    _category: *mut PyObject,
    _msg: *const c_char,
    _stacklevel: isize,
) -> c_int {
    // Accept and ignore — `warnings` integration is RFC 0023 work.
    0
}

#[no_mangle]
pub unsafe extern "C" fn PyErr_NewException(
    name: *const c_char,
    base: *mut PyObject,
    _dict: *mut PyObject,
) -> *mut PyObject {
    crate::interp::ensure_initialised();
    if name.is_null() {
        set_runtime_error("PyErr_NewException: NULL name");
        return ptr::null_mut();
    }
    let qualified = unsafe { CStr::from_ptr(name) }
        .to_string_lossy()
        .into_owned();
    let base_ty = if base.is_null() {
        builtin_types().exception.clone()
    } else {
        match unsafe { crate::object::clone_object(base) } {
            Object::Type(t) => t,
            _ => builtin_types().exception.clone(),
        }
    };
    let bare = qualified
        .rsplit('.')
        .next()
        .unwrap_or(&qualified)
        .to_owned();
    let new_ty = match TypeObject::new_user(&bare, vec![base_ty], DictData::new()) {
        Ok(t) => t,
        Err(_) => {
            set_runtime_error("PyErr_NewException: could not linearise");
            return ptr::null_mut();
        }
    };
    crate::object::into_owned(Object::Type(new_ty))
}

#[no_mangle]
pub unsafe extern "C" fn PyErr_NewExceptionWithDoc(
    name: *const c_char,
    _doc: *const c_char,
    base: *mut PyObject,
    dict: *mut PyObject,
) -> *mut PyObject {
    unsafe { PyErr_NewException(name, base, dict) }
}

fn type_object_for(p: *mut PyObject) -> Option<Rc<TypeObject>> {
    if p.is_null() {
        return None;
    }
    match unsafe { crate::object::clone_object(p) } {
        Object::Type(t) => Some(t),
        Object::Instance(inst) => Some(inst.class.clone()),
        _ => None,
    }
}

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

use weavepy_vm::builtin_types::{builtin_types, make_exception_with_class};
use weavepy_vm::error::{PyException, RuntimeError};
use weavepy_vm::object::{DictData, DictKey, Object};
use weavepy_vm::types::TypeObject;

use crate::object::PyObject;

#[derive(Clone)]
pub struct PendingError {
    pub ty: Option<Rc<TypeObject>>,
    pub value: Object,
}

// ----------------------------------------------------------------
// Pending-exception store — backed by `tstate->current_exception`.
//
// RFC 0047 (wave 5): genuine Cython output (`CYTHON_FAST_THREAD_STATE 1`)
// reads and writes `tstate->current_exception` *directly* at its struct
// offset, bypassing this call surface. To interoperate, the canonical
// pending-exception cell is exactly that field (see [`crate::pystate`]),
// not a private Rust thread-local: an error raised by a WeavePy C-API call
// is then visible to Cython's inlined read, and an exception Cython stashes
// is visible here. The slot holds either NULL or **one owned reference** to
// a normalised exception *instance* (CPython's 3.12+ single-object model);
// every mutator preserves that invariant.
// ----------------------------------------------------------------

/// Derive the exception's class from the stored value.
fn type_of_value(value: &Object) -> Option<Rc<TypeObject>> {
    match value {
        Object::Instance(inst) => Some(inst.cls()),
        Object::Type(t) => Some(t.clone()),
        _ => None,
    }
}

/// Normalise `(ty, value)` into an exception **instance** object (CPython's
/// `PyErr_SetObject` rule): an instance already satisfying `ty` (or any
/// instance when `ty` is None) *is* the exception; otherwise build
/// `ty(value)` — preserving the historical message mapping for non-instance
/// payloads via [`message_for`].
fn exception_instance(ty: Option<Rc<TypeObject>>, value: Object) -> Object {
    if let Object::Instance(inst) = &value {
        let satisfies = match &ty {
            Some(cls) => inst.cls().is_subclass_of(cls),
            None => true,
        };
        if satisfies {
            return value;
        }
    }
    let class = ty.unwrap_or_else(|| builtin_types().runtime_error.clone());
    make_exception_with_class(class, message_for(&value))
}

/// Clear the per-thread pending error cell. Called at the start of
/// every extension call.
pub fn clear_thread_local() {
    let slot = crate::pystate::current_exception_slot();
    unsafe {
        let old = *slot;
        if !old.is_null() {
            *slot = ptr::null_mut();
            crate::object::Py_DecRef(old);
        }
    }
}

/// Install `(ty, value)` as the pending exception. Replaces any
/// previously-pending error.
pub fn set_pending(ty: Option<Rc<TypeObject>>, value: Object) {
    let inst = exception_instance(ty, value);
    if std::env::var_os("WEAVEPY_TRACE_RAISE").is_some() {
        let name = match &inst {
            Object::Instance(i) => i.cls().name.to_string(),
            _ => String::new(),
        };
        if name == "RuntimeError" {
            eprintln!(
                "[WEAVEPY_TRACE_RAISE] RuntimeError msg={:?}\n{}",
                message_for(&inst),
                std::backtrace::Backtrace::force_capture()
            );
        }
    }
    // Seed a real traceback (pointing at the current Python frame) so the
    // exception matches CPython's "unwinding attaches a traceback" invariant.
    // Cython's `except X:` handler decrefs the fetched traceback *unguarded*,
    // so a NULL `__traceback__` there is a hard crash (see
    // `Interpreter::attach_c_traceback`).
    if matches!(inst, Object::Instance(_)) {
        crate::interp::with_interp_mut(|interp| interp.attach_c_traceback(&inst));
    }
    let p = crate::object::into_owned(inst);
    let slot = crate::pystate::current_exception_slot();
    unsafe {
        let old = *slot;
        *slot = p;
        if !old.is_null() {
            crate::object::Py_DecRef(old);
        }
    }
}

/// Read the pending exception, leaving the cell (and its owned reference)
/// intact.
pub fn pending() -> Option<PendingError> {
    let slot = crate::pystate::current_exception_slot();
    let p = unsafe { *slot };
    if p.is_null() {
        return None;
    }
    // Borrowing read: for a WeavePy box this is an `Rc` clone of the payload
    // (the slot keeps its C reference); for a foreign pointer `clone_object`
    // pins its own reference that the returned `Object` releases on drop.
    let value = unsafe { crate::object::clone_object(p) };
    let ty = type_of_value(&value);
    Some(PendingError { ty, value })
}

/// Take the pending exception out of the cell, transferring the slot's
/// owned reference out.
pub fn take_pending() -> Option<PendingError> {
    let slot = crate::pystate::current_exception_slot();
    let p = unsafe { *slot };
    if p.is_null() {
        return None;
    }
    unsafe { *slot = ptr::null_mut() };
    let value = unsafe { crate::object::clone_object(p) };
    let ty = type_of_value(&value);
    // Release the slot's reference now that the payload lives in `value`.
    // For a cached instance box this drops it to zero and `free_box` clears
    // the instance's `c_body`, so a later crossing re-mints cleanly.
    unsafe { crate::object::Py_DecRef(p) };
    Some(PendingError { ty, value })
}

/// Take the pending exception out of the cell and convert to a
/// [`RuntimeError`] suitable for returning from VM-facing trampolines.
pub fn take_pending_error_runtime() -> Option<RuntimeError> {
    take_pending().map(to_runtime_error)
}

/// Convert a [`PendingError`] to a [`RuntimeError`] suitable for
/// returning from the VM.
pub fn to_runtime_error(p: PendingError) -> RuntimeError {
    // Preserve the pending exception *instance* verbatim when it is a real
    // exception object. Its class carries the faithful MRO — e.g. a C
    // extension's custom `DateParseError(ValueError)` (pandas' Cython
    // date parser) — which `except (ValueError, …)` matching and
    // `isinstance` depend on. The old path rebuilt the exception from its
    // class *name* via `make_exception`, whose `by_name` lookup only knows
    // the built-ins and so collapsed every non-builtin exception to a bare
    // `Exception`, silently dropping its base classes.
    if let Object::Instance(inst) = &p.value {
        if inst.cls().is_subclass_of(&builtin_types().base_exception) {
            return RuntimeError::PyException(PyException::new(p.value.clone()));
        }
    }
    // Otherwise rebuild from the class *object* (not its name) so a custom
    // class still keeps its identity, falling back to `RuntimeError` when
    // the pending error carried no resolvable type.
    let class =
        p.ty.unwrap_or_else(|| builtin_types().runtime_error.clone());
    let inst = make_exception_with_class(class, message_for(&p.value));
    RuntimeError::PyException(PyException::new(inst))
}

pub(crate) fn message_for(o: &Object) -> String {
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
            format!("<{}>", inst.cls().name)
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

/// Bridge a [`RuntimeError`] produced by the VM into the thread-
/// local pending-exception cell. Mirrors the small `install_runtime_error`
/// helper that several individual modules used to roll
/// themselves. Centralised here so every Rust-side bridge picks
/// up the same class/value mapping.
pub fn set_pending_from_runtime(err: RuntimeError) {
    match err {
        RuntimeError::PyException(pe) => {
            let cls = match &pe.instance {
                Object::Instance(inst) => Some(inst.cls()),
                _ => None,
            };
            set_pending(cls, Object::from_str(pe.message()));
        }
        RuntimeError::Internal(msg) => {
            set_runtime_error(msg);
        }
    }
}

/// Helper used by argument-parsing code to install a `TypeError`.
pub fn set_type_error(msg: impl Into<String>) {
    let msg = msg.into();
    // Diagnostic: when `WEAVEPY_TRACE_TYPEERR` is a substring of the message,
    // dump the Rust backtrace so we can see which C-API entry a C extension
    // called. Gated + off by default; costs nothing in the common path.
    if let Some(needle) = std::env::var_os("WEAVEPY_TRACE_TYPEERR") {
        if let Some(needle) = needle.to_str() {
            if !needle.is_empty() && msg.contains(needle) {
                eprintln!(
                    "[WEAVEPY_TRACE_TYPEERR] TypeError msg={msg:?}\n{}",
                    std::backtrace::Backtrace::force_capture()
                );
            }
        }
    }
    set_pending(
        Some(builtin_types().type_error.clone()),
        Object::from_str(msg),
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

/// Helper used by the relative-import resolver to install an `ImportError`.
pub fn set_import_error(msg: impl Into<String>) {
    set_pending(
        Some(builtin_types().import_error.clone()),
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
    PyExc_IOError;
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
    // Each `PyExc_*` slot needs a pointer whose memory layout is a
    // real `PyTypeObjectBox` (i.e. it has a valid `bridge` field that
    // resolves back to the native [`TypeObject`]). The earlier
    // implementation handed out a `PyObjectBox` produced by
    // [`crate::object::into_owned`], which lacks `bridge`; reading
    // it later through `(*p as *mut PyTypeObject).bridge` produces
    // garbage and crashes on `Rc::clone`. We dispatch via
    // [`crate::types::install_user_type`] so every exception type
    // gets a proper bridged static slot (or is folded into an
    // existing one if `bt.value_error` is the same `Rc` as a
    // built-in like `bt.unicode_error`).
    let publish = |slot: *mut *mut PyObject, ty: Rc<TypeObject>| {
        let p = crate::types::install_user_type(&ty) as *mut PyObject;
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
        // IOError is an alias of OSError in Python 3; share the slot so
        // `PyExc_IOError is PyExc_OSError`.
        publish(&raw mut PyExc_IOError, bt.os_error.clone());
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
    tb: *mut PyObject,
) {
    // CPython uses a NULL *type* as the "no exception" sentinel: restoring
    // `(NULL, …)` clears the error state. Cython's `tp_dealloc` brackets
    // *every* deallocation with `PyErr_Fetch`/`PyErr_Restore`; with nothing
    // pending it restores `(NULL, NULL, NULL)`, which must clear — not
    // synthesise a bare `RuntimeError`. The old unconditional `set_pending`
    // (with `type_object_for(NULL)` defaulting to `RuntimeError`) installed a
    // spurious message-less `<RuntimeError>` on the way out of dealloc, which
    // then aborted the *next* operation that read the slot (pandas'
    // `_rebuild_blknos_and_blklocs`, whose `for … in enumerate(bp)` frees the
    // iterator inside the loop).
    if ty.is_null() {
        let slot = crate::pystate::current_exception_slot();
        let old = unsafe { *slot };
        unsafe { *slot = ptr::null_mut() };
        if !old.is_null() {
            unsafe { crate::object::Py_DecRef(old) };
        }
        if !value.is_null() {
            unsafe { crate::object::Py_DecRef(value) };
        }
        if !tb.is_null() {
            unsafe { crate::object::Py_DecRef(tb) };
        }
        return;
    }
    let cls = type_object_for(ty);
    let v = if value.is_null() {
        Object::None
    } else {
        unsafe { crate::object::clone_object(value) }
    };
    set_pending(cls, v);
}

/// `PyErr_GetRaisedException()` (3.12+) — detach and return the active
/// exception *instance* (a new reference), leaving no error set. This is
/// the modern single-object spelling Cython's `__Pyx_AddTraceback`
/// preamble uses; it transfers the slot's owned reference straight out.
#[no_mangle]
pub unsafe extern "C" fn PyErr_GetRaisedException() -> *mut PyObject {
    crate::interp::ensure_initialised();
    let slot = crate::pystate::current_exception_slot();
    let p = unsafe { *slot };
    unsafe { *slot = ptr::null_mut() };
    p
}

/// `PyErr_SetRaisedException(exc)` (3.12+) — make `exc` the active
/// exception, stealing the reference. Releases any previously-set one.
#[no_mangle]
pub unsafe extern "C" fn PyErr_SetRaisedException(exc: *mut PyObject) {
    crate::interp::ensure_initialised();
    let slot = crate::pystate::current_exception_slot();
    let old = unsafe { *slot };
    unsafe { *slot = exc };
    if !old.is_null() {
        unsafe { crate::object::Py_DecRef(old) };
    }
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
        Object::Instance(inst) => inst.cls(),
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
    category: *mut PyObject,
    msg: *const c_char,
    stacklevel: isize,
) -> c_int {
    unsafe { warn_via_warnings(category, msg, stacklevel) }
}

/// Route a C-level warning (`PyErr_WarnEx` / `PyErr_WarnFormat`) through
/// the Python `warnings` module, exactly as CPython's `do_warn` does.
///
/// This is what lets `warnings.catch_warnings(record=True)`,
/// `simplefilter(...)`, and the `"error"` filter observe warnings raised
/// by C extensions. numpy 2.x, for instance, raises a `DeprecationWarning`
/// from `np.timedelta64(<bare int>)`; pandas' test suite asserts on it via
/// `tm.assert_produces_warning`. Dropping the warning (the old stub) made
/// those assertions silently disagree with CPython.
///
/// Returns 0 normally, or -1 with a pending exception if the active filter
/// escalated the warning into one (`filterwarnings("error")`).
unsafe fn warn_via_warnings(
    category: *mut PyObject,
    msg: *const c_char,
    stacklevel: isize,
) -> c_int {
    crate::interp::ensure_initialised();
    if msg.is_null() {
        return 0;
    }
    // Never clobber a live exception: emitting a warning runs Python
    // (the `warnings` machinery) which may install its own error state.
    // CPython's callers only warn from a clean state; skipping here keeps
    // us safe without changing observable behaviour in practice.
    if pending().is_some() {
        return 0;
    }

    let message = unsafe { crate::strings::PyUnicode_FromString(msg) };
    if message.is_null() {
        return -1;
    }
    // A NULL category means `RuntimeWarning` (CPython's default).
    let cat = if category.is_null() {
        unsafe { PyExc_RuntimeWarning }
    } else {
        category
    };

    let module = unsafe {
        crate::module::PyImport_ImportModule(b"warnings\0".as_ptr() as *const c_char)
    };
    if module.is_null() {
        unsafe { crate::object::Py_DecRef(message) };
        return -1;
    }
    let warn_fn = unsafe {
        crate::abstract_::PyObject_GetAttrString(module, b"warn\0".as_ptr() as *const c_char)
    };
    unsafe { crate::object::Py_DecRef(module) };
    if warn_fn.is_null() {
        unsafe { crate::object::Py_DecRef(message) };
        return -1;
    }

    let stack = unsafe { crate::numbers::PyLong_FromLong(stacklevel as i64) };
    let args = unsafe { crate::containers::PyTuple_New(3) };
    if args.is_null() {
        unsafe {
            crate::object::Py_DecRef(message);
            crate::object::Py_DecRef(stack);
            crate::object::Py_DecRef(warn_fn);
        }
        return -1;
    }
    // `PyTuple_SetItem` steals a reference to each item. We own `message`
    // and `stack` outright; `cat` is borrowed (the caller's category, or
    // the immortal `PyExc_RuntimeWarning` static), so bump it first.
    unsafe {
        crate::object::Py_IncRef(cat);
        crate::containers::PyTuple_SetItem(args, 0, message);
        crate::containers::PyTuple_SetItem(args, 1, cat);
        crate::containers::PyTuple_SetItem(args, 2, stack);
    }

    let res = unsafe { crate::abstract_::PyObject_CallObject(warn_fn, args) };
    unsafe {
        crate::object::Py_DecRef(args);
        crate::object::Py_DecRef(warn_fn);
    }
    if res.is_null() {
        // The active filter turned the warning into an exception; leave it
        // pending and report failure, matching CPython's `PyErr_WarnEx`.
        return -1;
    }
    unsafe { crate::object::Py_DecRef(res) };
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
        Object::Instance(inst) => Some(inst.cls()),
        _ => None,
    }
}

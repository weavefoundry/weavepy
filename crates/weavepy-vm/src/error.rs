//! Runtime error type.
//!
//! Every dispatch loop failure surfaces as a [`RuntimeError`]. The
//! [`PyException`] variant now wraps a Python instance object — so
//! `except SomeClass as e:` can bind `e` to the actual instance and
//! the exception carries its own `args`, `__cause__`, traceback, etc.

use std::fmt;

use thiserror::Error;

use crate::object::Object;

/// A traceback frame captured as the exception unwinds.
#[derive(Debug, Clone)]
pub struct TracebackEntry {
    pub filename: String,
    pub funcname: String,
    pub lineno: u32,
}

/// A Python-visible exception. The wrapped [`Object`] is always an
/// `Object::Instance` whose class's MRO contains `BaseException`.
#[derive(Debug, Clone)]
pub struct PyException {
    pub instance: Object,
    pub traceback: Vec<TracebackEntry>,
    /// Implicit chaining context (`raise X` inside `except Y:` records
    /// `Y` as `__context__`). Stored separately from `instance.__dict__`
    /// so re-raises through pure Rust paths keep the link intact.
    pub context: Option<Box<PyException>>,
    /// Explicit chaining via `raise X from Y`.
    pub cause: Option<Box<PyException>>,
}

impl PyException {
    pub fn new(instance: Object) -> Self {
        Self {
            instance,
            traceback: Vec::new(),
            context: None,
            cause: None,
        }
    }

    /// Convenience constructor used by Rust-side error helpers — looks
    /// up the named built-in exception class and constructs an
    /// instance carrying `message` as `args[0]`.
    pub fn from_builtin(kind: &str, message: impl Into<String>) -> Self {
        let instance = crate::builtin_types::make_exception(kind, message);
        Self::new(instance)
    }

    /// The class name of the wrapped instance.
    pub fn type_name(&self) -> String {
        match &self.instance {
            Object::Instance(inst) => inst.class.name.clone(),
            _ => "BaseException".to_owned(),
        }
    }

    /// The exception's first arg, rendered as a string. Used by the
    /// CLI formatter and the `RuntimeError` Display impl.
    pub fn message(&self) -> String {
        crate::builtin_types::exception_message(&self.instance).unwrap_or_default()
    }

    pub fn push_traceback(&mut self, entry: TracebackEntry) {
        self.traceback.push(entry);
    }

    /// When this exception is a `SystemExit` (or a subclass), return
    /// its exit `code`: the explicit `.code` attribute, falling back to
    /// the single `args` element (`()` → `None`, `(x,)` → `x`,
    /// `(a, b, …)` → the tuple). Returns `None` for any other
    /// exception. Used by the CLI to terminate like CPython — honouring
    /// the code and suppressing the traceback — so `weavepy -m unittest`
    /// / `-m test` and bare `sys.exit()` behave as a drop-in would.
    pub fn system_exit_code(&self) -> Option<Object> {
        let Object::Instance(inst) = &self.instance else {
            return None;
        };
        if !inst
            .class
            .is_subclass_of(&crate::builtin_types::builtin_types().system_exit)
        {
            return None;
        }
        let dict = inst.dict.borrow();
        if let Some(code) = dict.get(&crate::object::DictKey(Object::from_static("code"))) {
            return Some(code.clone());
        }
        if let Some(Object::Tuple(args)) =
            dict.get(&crate::object::DictKey(Object::from_static("args")))
        {
            return Some(match args.len() {
                0 => Object::None,
                1 => args[0].clone(),
                _ => Object::Tuple(args.clone()),
            });
        }
        Some(Object::None)
    }
}

impl PartialEq for PyException {
    fn eq(&self, other: &Self) -> bool {
        // Identity for trapped equality use-cases; deep equality of
        // exception instances isn't a meaningful operation.
        self.type_name() == other.type_name() && self.message() == other.message()
    }
}

impl Eq for PyException {}

impl fmt::Display for PyException {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let msg = self.message();
        if msg.is_empty() {
            write!(f, "{}", self.type_name())
        } else {
            write!(f, "{}: {}", self.type_name(), msg)
        }
    }
}

/// Top-level VM error.
#[derive(Debug, Clone, Error)]
pub enum RuntimeError {
    #[error("{0}")]
    PyException(PyException),
    #[error("internal error: {0}")]
    Internal(String),
}

impl PartialEq for RuntimeError {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (RuntimeError::PyException(a), RuntimeError::PyException(b)) => a == b,
            (RuntimeError::Internal(a), RuntimeError::Internal(b)) => a == b,
            _ => false,
        }
    }
}

impl Eq for RuntimeError {}

pub fn type_error(message: impl Into<String>) -> RuntimeError {
    RuntimeError::PyException(PyException::from_builtin("TypeError", message))
}

pub fn value_error(message: impl Into<String>) -> RuntimeError {
    RuntimeError::PyException(PyException::from_builtin("ValueError", message))
}

pub fn name_error(message: impl Into<String>) -> RuntimeError {
    RuntimeError::PyException(PyException::from_builtin("NameError", message))
}

pub fn attribute_error(message: impl Into<String>) -> RuntimeError {
    RuntimeError::PyException(PyException::from_builtin("AttributeError", message))
}

pub fn key_error(message: impl Into<String>) -> RuntimeError {
    RuntimeError::PyException(PyException::from_builtin("KeyError", message))
}

pub fn index_error(message: impl Into<String>) -> RuntimeError {
    RuntimeError::PyException(PyException::from_builtin("IndexError", message))
}

pub fn zero_division_error(message: impl Into<String>) -> RuntimeError {
    RuntimeError::PyException(PyException::from_builtin("ZeroDivisionError", message))
}

pub fn stop_iteration() -> RuntimeError {
    RuntimeError::PyException(PyException::from_builtin("StopIteration", ""))
}

/// `StopAsyncIteration` (PEP 525) — raised by async iterators to
/// signal the end of iteration. Distinct from `StopIteration` so
/// `async for` doesn't accidentally swallow a synchronous
/// `StopIteration` that bubbled through user code.
pub fn stop_async_iteration() -> RuntimeError {
    RuntimeError::PyException(PyException::from_builtin("StopAsyncIteration", ""))
}

/// `StopIteration(value)` — used by generators to surface the value
/// of a `return` statement. The wrapped value is exposed as `.value`
/// on the exception instance.
pub fn stop_iteration_with(value: Object) -> RuntimeError {
    let pe = PyException::from_builtin("StopIteration", "");
    if let Object::Instance(ref inst) = pe.instance {
        let key = crate::object::DictKey(Object::from_static("value"));
        inst.dict.borrow_mut().insert(key, value.clone());
        let args_key = crate::object::DictKey(Object::from_static("args"));
        let args = Object::new_tuple(vec![value]);
        inst.dict.borrow_mut().insert(args_key, args);
    }
    RuntimeError::PyException(pe)
}

pub fn not_implemented_error(message: impl Into<String>) -> RuntimeError {
    RuntimeError::PyException(PyException::from_builtin("NotImplementedError", message))
}

pub fn runtime_error(message: impl Into<String>) -> RuntimeError {
    RuntimeError::PyException(PyException::from_builtin("RuntimeError", message))
}

pub fn assertion_error(message: impl Into<String>) -> RuntimeError {
    RuntimeError::PyException(PyException::from_builtin("AssertionError", message))
}

pub fn unbound_local_error(message: impl Into<String>) -> RuntimeError {
    RuntimeError::PyException(PyException::from_builtin("UnboundLocalError", message))
}

pub fn import_error(message: impl Into<String>) -> RuntimeError {
    RuntimeError::PyException(PyException::from_builtin("ImportError", message))
}

pub fn module_not_found_error(message: impl Into<String>) -> RuntimeError {
    RuntimeError::PyException(PyException::from_builtin("ModuleNotFoundError", message))
}

pub fn os_error(message: impl Into<String>) -> RuntimeError {
    RuntimeError::PyException(PyException::from_builtin("OSError", message))
}

pub fn blocking_io_error(message: impl Into<String>) -> RuntimeError {
    RuntimeError::PyException(PyException::from_builtin("BlockingIOError", message))
}

pub fn broken_pipe_error(message: impl Into<String>) -> RuntimeError {
    RuntimeError::PyException(PyException::from_builtin("BrokenPipeError", message))
}

pub fn connection_aborted_error(message: impl Into<String>) -> RuntimeError {
    RuntimeError::PyException(PyException::from_builtin("ConnectionAbortedError", message))
}

pub fn connection_refused_error(message: impl Into<String>) -> RuntimeError {
    RuntimeError::PyException(PyException::from_builtin("ConnectionRefusedError", message))
}

pub fn connection_reset_error(message: impl Into<String>) -> RuntimeError {
    RuntimeError::PyException(PyException::from_builtin("ConnectionResetError", message))
}

pub fn file_exists_error(message: impl Into<String>) -> RuntimeError {
    RuntimeError::PyException(PyException::from_builtin("FileExistsError", message))
}

pub fn file_not_found_error(message: impl Into<String>) -> RuntimeError {
    RuntimeError::PyException(PyException::from_builtin("FileNotFoundError", message))
}

pub fn interrupted_error(message: impl Into<String>) -> RuntimeError {
    RuntimeError::PyException(PyException::from_builtin("InterruptedError", message))
}

pub fn is_a_directory_error(message: impl Into<String>) -> RuntimeError {
    RuntimeError::PyException(PyException::from_builtin("IsADirectoryError", message))
}

pub fn not_a_directory_error(message: impl Into<String>) -> RuntimeError {
    RuntimeError::PyException(PyException::from_builtin("NotADirectoryError", message))
}

pub fn permission_error(message: impl Into<String>) -> RuntimeError {
    RuntimeError::PyException(PyException::from_builtin("PermissionError", message))
}

pub fn timeout_error(message: impl Into<String>) -> RuntimeError {
    RuntimeError::PyException(PyException::from_builtin("TimeoutError", message))
}

pub fn child_process_error(message: impl Into<String>) -> RuntimeError {
    RuntimeError::PyException(PyException::from_builtin("ChildProcessError", message))
}

/// Translate a `std::io::Error` into the most specific
/// CPython-style exception we can. Falls back to a generic
/// `OSError` for unrecognised kinds.
pub fn io_error_to_py(err: &std::io::Error) -> RuntimeError {
    use std::io::ErrorKind::{
        AlreadyExists, BrokenPipe, ConnectionAborted, ConnectionRefused, ConnectionReset,
        Interrupted, NotFound, PermissionDenied, TimedOut, WouldBlock,
    };
    let message = err.to_string();
    let mut runtime = match err.kind() {
        NotFound => file_not_found_error(message),
        PermissionDenied => permission_error(message),
        ConnectionRefused => connection_refused_error(message),
        ConnectionReset => connection_reset_error(message),
        ConnectionAborted => connection_aborted_error(message),
        BrokenPipe => broken_pipe_error(message),
        TimedOut => timeout_error(message),
        WouldBlock => blocking_io_error(message),
        Interrupted => interrupted_error(message),
        AlreadyExists => file_exists_error(message),
        _ => os_error(message),
    };
    if let Some(errno) = err.raw_os_error() {
        if let RuntimeError::PyException(ref mut exc) = runtime {
            if let crate::object::Object::Instance(inst) = &exc.instance {
                inst.dict.borrow_mut().insert(
                    crate::object::DictKey(crate::object::Object::from_static("errno")),
                    crate::object::Object::Int(i64::from(errno)),
                );
            }
        }
    }
    runtime
}

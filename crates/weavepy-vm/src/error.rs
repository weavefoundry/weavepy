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

pub fn runtime_error(message: impl Into<String>) -> RuntimeError {
    RuntimeError::PyException(PyException::from_builtin("RuntimeError", message))
}

pub fn assertion_error(message: impl Into<String>) -> RuntimeError {
    RuntimeError::PyException(PyException::from_builtin("AssertionError", message))
}

pub fn unbound_local_error(message: impl Into<String>) -> RuntimeError {
    RuntimeError::PyException(PyException::from_builtin("UnboundLocalError", message))
}

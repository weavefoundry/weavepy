//! Runtime error type.
//!
//! Every dispatch loop failure surfaces as a [`RuntimeError`]. The
//! [`PyException`] variant carries the Python-visible type name and
//! message — exceptions aren't routable yet (RFC 0004), but the
//! shape is in place so try / except work can wire to it.

use thiserror::Error;

/// A Python-visible exception (`TypeError`, `ValueError`, ...).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PyException {
    pub kind: &'static str,
    pub message: String,
}

impl PyException {
    pub fn new(kind: &'static str, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for PyException {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.kind, self.message)
    }
}

/// Top-level VM error.
#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum RuntimeError {
    #[error("{0}")]
    PyException(PyException),
    #[error("internal error: {0}")]
    Internal(String),
}

pub fn type_error(message: impl Into<String>) -> RuntimeError {
    RuntimeError::PyException(PyException::new("TypeError", message))
}

pub fn value_error(message: impl Into<String>) -> RuntimeError {
    RuntimeError::PyException(PyException::new("ValueError", message))
}

pub fn name_error(message: impl Into<String>) -> RuntimeError {
    RuntimeError::PyException(PyException::new("NameError", message))
}

pub fn attribute_error(message: impl Into<String>) -> RuntimeError {
    RuntimeError::PyException(PyException::new("AttributeError", message))
}

pub fn key_error(message: impl Into<String>) -> RuntimeError {
    RuntimeError::PyException(PyException::new("KeyError", message))
}

pub fn index_error(message: impl Into<String>) -> RuntimeError {
    RuntimeError::PyException(PyException::new("IndexError", message))
}

pub fn zero_division_error(message: impl Into<String>) -> RuntimeError {
    RuntimeError::PyException(PyException::new("ZeroDivisionError", message))
}

pub fn stop_iteration() -> RuntimeError {
    RuntimeError::PyException(PyException::new("StopIteration", String::new()))
}

pub fn runtime_error(message: impl Into<String>) -> RuntimeError {
    RuntimeError::PyException(PyException::new("RuntimeError", message))
}

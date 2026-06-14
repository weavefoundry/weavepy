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
    /// One-shot marker set by a bare `raise` / `RERAISE`: the *next*
    /// frame-level unwind must not add a traceback entry (CPython
    /// re-raises preserve the original traceback — the re-raise
    /// location is not recorded).
    pub suppress_tb_once: bool,
    /// True once implicit-context chaining has been decided for this
    /// exception (CPython chains exactly once, in `_PyErr_SetObject` at
    /// the raise site). Propagation through Rust boundaries must not
    /// re-chain — user code may have set `__context__ = None` since.
    pub context_settled: bool,
}

impl PyException {
    pub fn new(instance: Object) -> Self {
        Self {
            instance,
            traceback: Vec::new(),
            context: None,
            cause: None,
            suppress_tb_once: false,
            context_settled: false,
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
            Object::Instance(inst) => inst.cls().name.clone(),
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

    /// PEP 678: append a string note to the wrapped instance's
    /// `__notes__` list (created on first use). Mirrors
    /// `BaseException.add_note`, but callable from Rust-side machinery
    /// that needs to annotate an exception before re-raising it — e.g.
    /// CPython's `type.__new__` decorates a `__set_name__` failure with
    /// "Error calling __set_name__ on '…' instance '…' in '…'".
    pub fn add_note(&self, note: impl Into<String>) {
        use crate::object::DictKey;
        if let Object::Instance(inst) = &self.instance {
            let key = DictKey(Object::from_static("__notes__"));
            let mut dict = inst.dict.borrow_mut();
            let mut notes = match dict.get(&key) {
                Some(Object::List(l)) => l.borrow().clone(),
                _ => Vec::new(),
            };
            notes.push(Object::from_str(note.into()));
            dict.insert(
                key,
                Object::List(crate::sync::Rc::new(crate::sync::GilCell::new(notes))),
            );
        }
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
            .cls()
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

/// Set an attribute in a wrapped exception instance's `__dict__` unless
/// already present. Used by raise sites that enrich exceptions with
/// structured fields the way CPython's C raisers do (`AttributeError.name`
/// / `.obj`, `NameError.name`, `ImportError.name_from`, …).
pub fn set_exception_attr(err: &RuntimeError, key: &'static str, value: Object) {
    use crate::object::DictKey;
    if let RuntimeError::PyException(pe) = err {
        if let Object::Instance(inst) = &pe.instance {
            let k = DictKey(Object::from_static(key));
            let mut dict = inst.dict.borrow_mut();
            if !dict.contains_key(&k) {
                dict.insert(k, value);
            }
        }
    }
}

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

/// `AttributeError` carrying the structured `.name`/`.obj` fields
/// CPython's C raise sites populate (PEP 3134-adjacent; used by
/// suggestion machinery and asserted on by `test_exceptions`).
pub fn attribute_error_named(obj: &Object, name: &str) -> RuntimeError {
    use crate::object::DictKey;
    let err = attribute_error(format!(
        "'{}' object has no attribute '{}'",
        obj.type_name_owned(),
        name
    ));
    if let RuntimeError::PyException(pe) = &err {
        if let Object::Instance(inst) = &pe.instance {
            let mut dict = inst.dict.borrow_mut();
            dict.insert(DictKey(Object::from_static("name")), Object::from_str(name));
            dict.insert(DictKey(Object::from_static("obj")), obj.clone());
        }
    }
    err
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

/// `LookupError` — CPython raises this (not `ValueError`) for unknown
/// codec names and error handlers (`codecs.lookup`, `bytes(s, encoding=…)`).
pub fn lookup_error(message: impl Into<String>) -> RuntimeError {
    RuntimeError::PyException(PyException::from_builtin("LookupError", message))
}

/// `BufferError` — raised when a length-changing operation hits a
/// `bytearray` with live buffer exports (`memoryview`, or a search
/// method's internal export held across re-entrant argument coercion).
pub fn buffer_error(message: impl Into<String>) -> RuntimeError {
    RuntimeError::PyException(PyException::from_builtin("BufferError", message))
}

pub fn memory_error(message: impl Into<String>) -> RuntimeError {
    RuntimeError::PyException(PyException::from_builtin("MemoryError", message))
}

pub fn overflow_error(message: impl Into<String>) -> RuntimeError {
    RuntimeError::PyException(PyException::from_builtin("OverflowError", message))
}

/// `UnicodeEncodeError` carrying the canonical `(encoding, object, start,
/// end, reason)` payload. Surfaced by the strict-mode codec when a
/// character can't be encoded, so `str.encode()` failures are catchable as
/// `UnicodeEncodeError` (not just `ValueError`).
pub fn unicode_encode_error(
    encoding: &str,
    object: &str,
    start: usize,
    end: usize,
    reason: &str,
) -> RuntimeError {
    RuntimeError::PyException(PyException::new(
        crate::builtin_types::make_unicode_encode_error(encoding, object, start, end, reason),
    ))
}

/// `UnicodeDecodeError` carrying the canonical `(encoding, object, start,
/// end, reason)` payload — the strict-mode decode counterpart of
/// [`unicode_encode_error`], so `bytes.decode()` failures are catchable
/// as `UnicodeDecodeError` (not just `ValueError`).
pub fn unicode_decode_error(
    encoding: &str,
    object: &[u8],
    start: usize,
    end: usize,
    reason: &str,
) -> RuntimeError {
    RuntimeError::PyException(PyException::new(
        crate::builtin_types::make_unicode_decode_error(encoding, object, start, end, reason),
    ))
}

/// `RecursionError` — raised when the per-thread Python call depth /
/// native-recursion guard (RFC 0037 WS1) is exceeded. CPython raises
/// this from `Py_EnterRecursiveCall`, including on the C-level recursion
/// inside `do_richcompare`/`repr` of reflexive containers.
pub fn recursion_error(message: impl Into<String>) -> RuntimeError {
    RuntimeError::PyException(PyException::from_builtin("RecursionError", message))
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
        // A bare `return` (value None) raises `StopIteration()` with
        // *empty* args, so `str(e)` renders bare and `e.args` is `()` —
        // CPython's `gen_return` only packs non-None return values.
        let args_key = crate::object::DictKey(Object::from_static("args"));
        let args = if matches!(value, Object::None) {
            Object::new_tuple(Vec::new())
        } else {
            Object::new_tuple(vec![value])
        };
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

/// A bare `SyntaxError` carrying only a message — no source location.
/// Used for compiler-phase failures (e.g. `'return' outside function`)
/// that don't track a byte offset, so `str(e)` is just the message.
pub fn syntax_error(message: impl Into<String>) -> RuntimeError {
    RuntimeError::PyException(PyException::from_builtin("SyntaxError", message))
}

/// A `SyntaxError` with CPython's full location payload. Sets `.msg`,
/// `.filename`, `.lineno`, `.offset`, and `.text` on the instance and
/// shapes `args` as `(msg, (filename, lineno, offset, text))`, exactly as
/// CPython's parser does — so `str(e)` renders
/// `"<msg> (<basename>, line <lineno>)"` and the attributes are
/// inspectable (`e.lineno`, `e.offset`, …).
pub fn syntax_error_located(
    message: impl Into<String>,
    filename: Option<&str>,
    lineno: Option<u32>,
    offset: Option<u32>,
    text: Option<&str>,
) -> RuntimeError {
    syntax_error_located_as("SyntaxError", message, filename, lineno, offset, text)
}

/// [`syntax_error_located`] with an explicit exception class —
/// `IndentationError` / `TabError` share SyntaxError's location payload.
pub fn syntax_error_located_as(
    class: &'static str,
    message: impl Into<String>,
    filename: Option<&str>,
    lineno: Option<u32>,
    offset: Option<u32>,
    text: Option<&str>,
) -> RuntimeError {
    use crate::object::DictKey;
    let message = message.into();
    let pe = PyException::from_builtin(class, message.clone());
    if let Object::Instance(inst) = &pe.instance {
        let msg_obj = Object::from_str(message);
        let file_obj = filename.map_or(Object::None, Object::from_str);
        let line_obj = lineno.map_or(Object::None, |n| Object::Int(i64::from(n)));
        let off_obj = offset.map_or(Object::None, |n| Object::Int(i64::from(n)));
        let text_obj = text.map_or(Object::None, Object::from_str);
        let detail = Object::new_tuple(vec![
            file_obj.clone(),
            line_obj.clone(),
            off_obj.clone(),
            text_obj.clone(),
        ]);
        let mut dict = inst.dict.borrow_mut();
        dict.insert(DictKey(Object::from_static("msg")), msg_obj.clone());
        dict.insert(DictKey(Object::from_static("filename")), file_obj);
        dict.insert(DictKey(Object::from_static("lineno")), line_obj);
        dict.insert(DictKey(Object::from_static("offset")), off_obj);
        dict.insert(DictKey(Object::from_static("text")), text_obj);
        dict.insert(DictKey(Object::from_static("end_lineno")), Object::None);
        dict.insert(DictKey(Object::from_static("end_offset")), Object::None);
        dict.insert(
            DictKey(Object::from_static("args")),
            Object::new_tuple(vec![msg_obj, detail]),
        );
    }
    RuntimeError::PyException(pe)
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
    io_error_to_py_named(err, None)
}

/// As [`io_error_to_py`], but attaches the offending path so the exception
/// reads like CPython's: `[Errno 2] No such file or directory: 'name'`, with
/// `.errno` / `.strerror` / `.filename` populated to match.
pub fn io_error_to_py_named(err: &std::io::Error, filename: Option<&str>) -> RuntimeError {
    use std::io::ErrorKind::{
        AlreadyExists, BrokenPipe, ConnectionAborted, ConnectionRefused, ConnectionReset,
        Interrupted, NotFound, PermissionDenied, TimedOut, WouldBlock,
    };
    let errno = err.raw_os_error();
    // CPython's `strerror` is the bare OS message; Rust appends a
    // " (os error N)" decoration we strip so the text matches CPython.
    let raw = err.to_string();
    let strerror = match raw.find(" (os error ") {
        Some(i) => raw[..i].to_string(),
        None => raw,
    };
    // Mirror `OSError.__str__`: "[Errno N] strerror: 'filename'".
    let message = match (errno, filename) {
        (Some(n), Some(f)) => format!("[Errno {n}] {strerror}: '{f}'"),
        (Some(n), None) => format!("[Errno {n}] {strerror}"),
        (None, Some(f)) => format!("{strerror}: '{f}'"),
        (None, None) => strerror.clone(),
    };
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
    if let RuntimeError::PyException(ref mut exc) = runtime {
        if let crate::object::Object::Instance(inst) = &exc.instance {
            let mut dict = inst.dict.borrow_mut();
            if let Some(errno) = errno {
                dict.insert(
                    crate::object::DictKey(crate::object::Object::from_static("errno")),
                    crate::object::Object::Int(i64::from(errno)),
                );
            }
            dict.insert(
                crate::object::DictKey(crate::object::Object::from_static("strerror")),
                crate::object::Object::from_str(strerror),
            );
            if let Some(f) = filename {
                dict.insert(
                    crate::object::DictKey(crate::object::Object::from_static("filename")),
                    crate::object::Object::from_str(f.to_owned()),
                );
            }
        }
    }
    runtime
}

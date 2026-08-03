//! `_sqlite3` — a faithful native core for CPython's `sqlite3` package
//! (RFC 0056 WS1).
//!
//! This replaces the RFC 0019 dict-shaped shim with the real thing: the
//! verbatim CPython `Lib/sqlite3/` package (`__init__.py`, `dbapi2.py`,
//! `dump.py`, `__main__.py`) runs over this module exactly as it runs
//! over `Modules/_sqlite/*.c` on CPython. The surface mirrors
//! `module.c` / `connection.c` / `cursor.c` / `row.c` / `blob.c`:
//!
//! * real heap types — `Connection`, `Cursor`, `Row`, `Blob`,
//!   `PrepareProtocol` — subclassable, with the C getset/method split;
//! * the ten-class exception hierarchy (`Warning`, `Error`,
//!   `InterfaceError`, `DatabaseError`, `DataError`, `OperationalError`,
//!   `IntegrityError`, `InternalError`, `ProgrammingError`,
//!   `NotSupportedError`) with `sqlite_errorcode`/`sqlite_errorname`
//!   on raised instances;
//! * both transaction-control worlds: legacy `isolation_level`
//!   (implicit `BEGIN <mode>` before the first DML while in
//!   autocommit) and PEP-249-2022 `autocommit` (True / False /
//!   `LEGACY_TRANSACTION_CONTROL`);
//! * the adapter/converter microprotocol (`register_adapter`,
//!   `register_converter`, `adapt`, `PrepareProtocol`,
//!   `PARSE_DECLTYPES` / `PARSE_COLNAMES`);
//! * the callback surface: `create_function(deterministic=)`,
//!   `create_aggregate`, `create_window_function`, `create_collation`,
//!   `set_authorizer`, `set_progress_handler`, `set_trace_callback`,
//!   with CPython's exception policy (`enable_callback_tracebacks`);
//! * `backup`, `serialize`/`deserialize`, `blobopen`, `interrupt`,
//!   `getlimit`/`setlimit`, `complete_statement`.
//!
//! ## Storage model
//!
//! The raw `sqlite3*` / `sqlite3_stmt*` pointers live in process-global
//! registries keyed by an integer handle stored on the instance dict
//! (the `socket_mod` / `_asyncio` pattern). Pointers are stored as
//! `usize` so the state cells stay `Send + Sync`; the GIL serialises
//! access, and `check_same_thread` (default on) enforces CPython's
//! thread-affinity error besides. All SQLite calls happen with the GIL
//! held — user-defined functions and hooks re-enter the VM from inside
//! `sqlite3_step`, which is only sound because the calling thread still
//! owns it.

#![allow(unsafe_op_in_unsafe_fn)]

pub mod connection;
pub mod cursor;
pub mod hooks;
pub mod row;
pub mod stmt;

use std::collections::HashMap;

use rusqlite::ffi;

use crate::error::{type_error, PyException, RuntimeError};
use crate::import::ModuleCache;
use crate::object::{BuiltinFn, DictData, DictKey, Object, PyModule, PyProperty};
use crate::sync::Rc;
use crate::sync::RefCell;
use crate::types::{PyInstance, TypeObject};

// ---------------------------------------------------------------
// Interpreter re-entry (the `_asyncio` pattern)
// ---------------------------------------------------------------

pub(crate) type Interp = crate::Interpreter;

pub(crate) fn interp<'a>() -> Result<&'a mut Interp, RuntimeError> {
    let ptr = crate::vm_singletons::current_interpreter_ptr()
        .ok_or_else(|| RuntimeError::Internal("_sqlite3: no running interpreter".to_owned()))?;
    // SAFETY: published by an enclosing VM frame still live on this thread;
    // the GIL keeps the access exclusive.
    Ok(unsafe { &mut *ptr })
}

pub(crate) fn call(
    interp: &mut Interp,
    f: &Object,
    args: &[Object],
) -> Result<Object, RuntimeError> {
    let globals = interp.builtins_dict();
    interp.call_object_with_globals(f, args, &[], &globals)
}

pub(crate) fn call_kw(
    interp: &mut Interp,
    f: &Object,
    args: &[Object],
    kwargs: &[(String, Object)],
) -> Result<Object, RuntimeError> {
    let globals = interp.builtins_dict();
    interp.call_object_with_globals(f, args, kwargs, &globals)
}

// ---------------------------------------------------------------
// Exception hierarchy (module.c)
// ---------------------------------------------------------------

macro_rules! exc_class {
    ($fn_name:ident, $py_name:literal, $base:expr) => {
        pub(crate) fn $fn_name() -> Rc<TypeObject> {
            static CELL: std::sync::OnceLock<Rc<TypeObject>> = std::sync::OnceLock::new();
            CELL.get_or_init(|| {
                let mut dict = DictData::default();
                dict.insert(
                    DictKey(Object::from_static("__module__")),
                    Object::from_static("sqlite3"),
                );
                TypeObject::new_user($py_name, vec![$base], dict)
                    .expect("sqlite3 exception class must linearise")
            })
            .clone()
        }
    };
}

fn builtin_exc(name: &str) -> Rc<TypeObject> {
    crate::builtin_types::builtin_types()
        .by_name(name)
        .unwrap_or_else(|| panic!("builtin exception {name} must exist"))
}

exc_class!(warning_class, "Warning", builtin_exc("Exception"));
exc_class!(error_class, "Error", builtin_exc("Exception"));
exc_class!(interface_error_class, "InterfaceError", error_class());
exc_class!(database_error_class, "DatabaseError", error_class());
exc_class!(data_error_class, "DataError", database_error_class());
exc_class!(
    operational_error_class,
    "OperationalError",
    database_error_class()
);
exc_class!(
    integrity_error_class,
    "IntegrityError",
    database_error_class()
);
exc_class!(
    internal_error_class,
    "InternalError",
    database_error_class()
);
exc_class!(
    programming_error_class,
    "ProgrammingError",
    database_error_class()
);
exc_class!(
    not_supported_error_class,
    "NotSupportedError",
    database_error_class()
);

/// Raise an exception of the given sqlite3 class with a plain message.
pub(crate) fn raise(cls: Rc<TypeObject>, msg: impl Into<String>) -> RuntimeError {
    let inst = crate::builtin_types::make_exception_with_class(cls, msg);
    RuntimeError::PyException(PyException::new(inst))
}

/// Map a SQLite result code to the CPython exception class
/// (`_pysqlite_seterror` in module.c) and raise with
/// `sqlite_errorcode` / `sqlite_errorname` attached.
pub(crate) fn raise_sqlite_error(db: *mut ffi::sqlite3, errcode: i32) -> RuntimeError {
    let msg = if db.is_null() {
        // SAFETY: sqlite3_errstr returns a static string for any code.
        unsafe { cstr_to_string(ffi::sqlite3_errstr(errcode)) }
    } else {
        // SAFETY: `db` is a live connection handle owned by the caller.
        unsafe { cstr_to_string(ffi::sqlite3_errmsg(db)) }
    };
    raise_sqlite_error_msg(errcode, &msg)
}

pub(crate) fn raise_sqlite_error_msg(errcode: i32, msg: &str) -> RuntimeError {
    let primary = errcode & 0xff;
    let cls = match primary {
        ffi::SQLITE_INTERNAL | ffi::SQLITE_NOTFOUND => internal_error_class(),
        ffi::SQLITE_NOMEM => {
            return RuntimeError::PyException(PyException::from_builtin("MemoryError", ""));
        }
        ffi::SQLITE_ERROR
        | ffi::SQLITE_PERM
        | ffi::SQLITE_ABORT
        | ffi::SQLITE_BUSY
        | ffi::SQLITE_LOCKED
        | ffi::SQLITE_READONLY
        | ffi::SQLITE_INTERRUPT
        | ffi::SQLITE_IOERR
        | ffi::SQLITE_FULL
        | ffi::SQLITE_CANTOPEN
        | ffi::SQLITE_PROTOCOL
        | ffi::SQLITE_EMPTY
        | ffi::SQLITE_SCHEMA => operational_error_class(),
        ffi::SQLITE_CORRUPT => database_error_class(),
        ffi::SQLITE_TOOBIG => data_error_class(),
        ffi::SQLITE_CONSTRAINT | ffi::SQLITE_MISMATCH => integrity_error_class(),
        ffi::SQLITE_MISUSE | ffi::SQLITE_RANGE => interface_error_class(),
        _ => database_error_class(),
    };
    let inst = crate::builtin_types::make_exception_with_class(cls, msg);
    if let Object::Instance(i) = &inst {
        let mut d = i.dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("sqlite_errorcode")),
            Object::Int(i64::from(errcode)),
        );
        d.insert(
            DictKey(Object::from_static("sqlite_errorname")),
            Object::from_str(errorname(errcode)),
        );
    }
    RuntimeError::PyException(PyException::new(inst))
}

/// The symbolic name for a SQLite (extended) result code, mirroring the
/// module-level `SQLITE_*` constant CPython exposes since 3.11.
fn errorname(code: i32) -> String {
    for (name, value) in ERROR_CODES.iter().chain(EXTENDED_ERROR_CODES) {
        if *value == code {
            return (*name).to_owned();
        }
    }
    // Fall back to the primary code's name.
    for (name, value) in ERROR_CODES {
        if *value == (code & 0xff) {
            return (*name).to_owned();
        }
    }
    format!("SQLITE_UNKNOWN_{code}")
}

/// Primary + common extended result codes (module.c `add_integer_constants`).
pub(crate) const ERROR_CODES: &[(&str, i32)] = &[
    ("SQLITE_OK", ffi::SQLITE_OK),
    ("SQLITE_ERROR", ffi::SQLITE_ERROR),
    ("SQLITE_INTERNAL", ffi::SQLITE_INTERNAL),
    ("SQLITE_PERM", ffi::SQLITE_PERM),
    ("SQLITE_ABORT", ffi::SQLITE_ABORT),
    ("SQLITE_BUSY", ffi::SQLITE_BUSY),
    ("SQLITE_LOCKED", ffi::SQLITE_LOCKED),
    ("SQLITE_NOMEM", ffi::SQLITE_NOMEM),
    ("SQLITE_READONLY", ffi::SQLITE_READONLY),
    ("SQLITE_INTERRUPT", ffi::SQLITE_INTERRUPT),
    ("SQLITE_IOERR", ffi::SQLITE_IOERR),
    ("SQLITE_CORRUPT", ffi::SQLITE_CORRUPT),
    ("SQLITE_NOTFOUND", ffi::SQLITE_NOTFOUND),
    ("SQLITE_FULL", ffi::SQLITE_FULL),
    ("SQLITE_CANTOPEN", ffi::SQLITE_CANTOPEN),
    ("SQLITE_PROTOCOL", ffi::SQLITE_PROTOCOL),
    ("SQLITE_EMPTY", ffi::SQLITE_EMPTY),
    ("SQLITE_SCHEMA", ffi::SQLITE_SCHEMA),
    ("SQLITE_TOOBIG", ffi::SQLITE_TOOBIG),
    ("SQLITE_CONSTRAINT", ffi::SQLITE_CONSTRAINT),
    ("SQLITE_MISMATCH", ffi::SQLITE_MISMATCH),
    ("SQLITE_MISUSE", ffi::SQLITE_MISUSE),
    ("SQLITE_NOLFS", ffi::SQLITE_NOLFS),
    ("SQLITE_AUTH", ffi::SQLITE_AUTH),
    ("SQLITE_FORMAT", ffi::SQLITE_FORMAT),
    ("SQLITE_RANGE", ffi::SQLITE_RANGE),
    ("SQLITE_NOTADB", ffi::SQLITE_NOTADB),
    ("SQLITE_NOTICE", ffi::SQLITE_NOTICE),
    ("SQLITE_WARNING", ffi::SQLITE_WARNING),
    ("SQLITE_ROW", ffi::SQLITE_ROW),
    ("SQLITE_DONE", ffi::SQLITE_DONE),
];

/// Extended result codes (module.c `add_integer_constants`, 3.11+).
pub(crate) const EXTENDED_ERROR_CODES: &[(&str, i32)] = &[
    ("SQLITE_ABORT_ROLLBACK", ffi::SQLITE_ABORT_ROLLBACK),
    ("SQLITE_AUTH_USER", ffi::SQLITE_AUTH_USER),
    ("SQLITE_BUSY_RECOVERY", ffi::SQLITE_BUSY_RECOVERY),
    ("SQLITE_BUSY_SNAPSHOT", ffi::SQLITE_BUSY_SNAPSHOT),
    ("SQLITE_BUSY_TIMEOUT", ffi::SQLITE_BUSY_TIMEOUT),
    ("SQLITE_CANTOPEN_CONVPATH", ffi::SQLITE_CANTOPEN_CONVPATH),
    ("SQLITE_CANTOPEN_DIRTYWAL", ffi::SQLITE_CANTOPEN_DIRTYWAL),
    ("SQLITE_CANTOPEN_FULLPATH", ffi::SQLITE_CANTOPEN_FULLPATH),
    ("SQLITE_CANTOPEN_ISDIR", ffi::SQLITE_CANTOPEN_ISDIR),
    ("SQLITE_CANTOPEN_NOTEMPDIR", ffi::SQLITE_CANTOPEN_NOTEMPDIR),
    ("SQLITE_CANTOPEN_SYMLINK", ffi::SQLITE_CANTOPEN_SYMLINK),
    ("SQLITE_CONSTRAINT_CHECK", ffi::SQLITE_CONSTRAINT_CHECK),
    (
        "SQLITE_CONSTRAINT_COMMITHOOK",
        ffi::SQLITE_CONSTRAINT_COMMITHOOK,
    ),
    (
        "SQLITE_CONSTRAINT_DATATYPE",
        ffi::SQLITE_CONSTRAINT_DATATYPE,
    ),
    (
        "SQLITE_CONSTRAINT_FOREIGNKEY",
        ffi::SQLITE_CONSTRAINT_FOREIGNKEY,
    ),
    (
        "SQLITE_CONSTRAINT_FUNCTION",
        ffi::SQLITE_CONSTRAINT_FUNCTION,
    ),
    ("SQLITE_CONSTRAINT_NOTNULL", ffi::SQLITE_CONSTRAINT_NOTNULL),
    ("SQLITE_CONSTRAINT_PINNED", ffi::SQLITE_CONSTRAINT_PINNED),
    (
        "SQLITE_CONSTRAINT_PRIMARYKEY",
        ffi::SQLITE_CONSTRAINT_PRIMARYKEY,
    ),
    ("SQLITE_CONSTRAINT_ROWID", ffi::SQLITE_CONSTRAINT_ROWID),
    ("SQLITE_CONSTRAINT_TRIGGER", ffi::SQLITE_CONSTRAINT_TRIGGER),
    ("SQLITE_CONSTRAINT_UNIQUE", ffi::SQLITE_CONSTRAINT_UNIQUE),
    ("SQLITE_CONSTRAINT_VTAB", ffi::SQLITE_CONSTRAINT_VTAB),
    ("SQLITE_CORRUPT_INDEX", ffi::SQLITE_CORRUPT_INDEX),
    ("SQLITE_CORRUPT_SEQUENCE", ffi::SQLITE_CORRUPT_SEQUENCE),
    ("SQLITE_CORRUPT_VTAB", ffi::SQLITE_CORRUPT_VTAB),
    (
        "SQLITE_ERROR_MISSING_COLLSEQ",
        ffi::SQLITE_ERROR_MISSING_COLLSEQ,
    ),
    ("SQLITE_ERROR_RETRY", ffi::SQLITE_ERROR_RETRY),
    ("SQLITE_ERROR_SNAPSHOT", ffi::SQLITE_ERROR_SNAPSHOT),
    ("SQLITE_IOERR_ACCESS", ffi::SQLITE_IOERR_ACCESS),
    ("SQLITE_IOERR_AUTH", ffi::SQLITE_IOERR_AUTH),
    ("SQLITE_IOERR_BEGIN_ATOMIC", ffi::SQLITE_IOERR_BEGIN_ATOMIC),
    ("SQLITE_IOERR_BLOCKED", ffi::SQLITE_IOERR_BLOCKED),
    (
        "SQLITE_IOERR_CHECKRESERVEDLOCK",
        ffi::SQLITE_IOERR_CHECKRESERVEDLOCK,
    ),
    ("SQLITE_IOERR_CLOSE", ffi::SQLITE_IOERR_CLOSE),
    (
        "SQLITE_IOERR_COMMIT_ATOMIC",
        ffi::SQLITE_IOERR_COMMIT_ATOMIC,
    ),
    ("SQLITE_IOERR_CONVPATH", ffi::SQLITE_IOERR_CONVPATH),
    ("SQLITE_IOERR_CORRUPTFS", ffi::SQLITE_IOERR_CORRUPTFS),
    ("SQLITE_IOERR_DATA", ffi::SQLITE_IOERR_DATA),
    ("SQLITE_IOERR_DELETE", ffi::SQLITE_IOERR_DELETE),
    ("SQLITE_IOERR_DELETE_NOENT", ffi::SQLITE_IOERR_DELETE_NOENT),
    ("SQLITE_IOERR_DIR_CLOSE", ffi::SQLITE_IOERR_DIR_CLOSE),
    ("SQLITE_IOERR_DIR_FSYNC", ffi::SQLITE_IOERR_DIR_FSYNC),
    ("SQLITE_IOERR_FSTAT", ffi::SQLITE_IOERR_FSTAT),
    ("SQLITE_IOERR_FSYNC", ffi::SQLITE_IOERR_FSYNC),
    ("SQLITE_IOERR_GETTEMPPATH", ffi::SQLITE_IOERR_GETTEMPPATH),
    ("SQLITE_IOERR_LOCK", ffi::SQLITE_IOERR_LOCK),
    ("SQLITE_IOERR_MMAP", ffi::SQLITE_IOERR_MMAP),
    ("SQLITE_IOERR_NOMEM", ffi::SQLITE_IOERR_NOMEM),
    ("SQLITE_IOERR_RDLOCK", ffi::SQLITE_IOERR_RDLOCK),
    ("SQLITE_IOERR_READ", ffi::SQLITE_IOERR_READ),
    (
        "SQLITE_IOERR_ROLLBACK_ATOMIC",
        ffi::SQLITE_IOERR_ROLLBACK_ATOMIC,
    ),
    ("SQLITE_IOERR_SEEK", ffi::SQLITE_IOERR_SEEK),
    ("SQLITE_IOERR_SHMLOCK", ffi::SQLITE_IOERR_SHMLOCK),
    ("SQLITE_IOERR_SHMMAP", ffi::SQLITE_IOERR_SHMMAP),
    ("SQLITE_IOERR_SHMOPEN", ffi::SQLITE_IOERR_SHMOPEN),
    ("SQLITE_IOERR_SHMSIZE", ffi::SQLITE_IOERR_SHMSIZE),
    ("SQLITE_IOERR_SHORT_READ", ffi::SQLITE_IOERR_SHORT_READ),
    ("SQLITE_IOERR_TRUNCATE", ffi::SQLITE_IOERR_TRUNCATE),
    ("SQLITE_IOERR_UNLOCK", ffi::SQLITE_IOERR_UNLOCK),
    ("SQLITE_IOERR_VNODE", ffi::SQLITE_IOERR_VNODE),
    ("SQLITE_IOERR_WRITE", ffi::SQLITE_IOERR_WRITE),
    ("SQLITE_LOCKED_SHAREDCACHE", ffi::SQLITE_LOCKED_SHAREDCACHE),
    ("SQLITE_LOCKED_VTAB", ffi::SQLITE_LOCKED_VTAB),
    (
        "SQLITE_NOTICE_RECOVER_ROLLBACK",
        ffi::SQLITE_NOTICE_RECOVER_ROLLBACK,
    ),
    ("SQLITE_NOTICE_RECOVER_WAL", ffi::SQLITE_NOTICE_RECOVER_WAL),
    (
        "SQLITE_OK_LOAD_PERMANENTLY",
        ffi::SQLITE_OK_LOAD_PERMANENTLY,
    ),
    ("SQLITE_OK_SYMLINK", ffi::SQLITE_OK_SYMLINK),
    ("SQLITE_READONLY_CANTINIT", ffi::SQLITE_READONLY_CANTINIT),
    ("SQLITE_READONLY_CANTLOCK", ffi::SQLITE_READONLY_CANTLOCK),
    ("SQLITE_READONLY_DBMOVED", ffi::SQLITE_READONLY_DBMOVED),
    ("SQLITE_READONLY_DIRECTORY", ffi::SQLITE_READONLY_DIRECTORY),
    ("SQLITE_READONLY_RECOVERY", ffi::SQLITE_READONLY_RECOVERY),
    ("SQLITE_READONLY_ROLLBACK", ffi::SQLITE_READONLY_ROLLBACK),
    ("SQLITE_WARNING_AUTOINDEX", ffi::SQLITE_WARNING_AUTOINDEX),
];

/// Boolean `sqlite3_db_config` toggles CPython exposes (module.c).
pub(crate) const DBCONFIG_CODES: &[(&str, i32)] = &[
    (
        "SQLITE_DBCONFIG_ENABLE_FKEY",
        ffi::SQLITE_DBCONFIG_ENABLE_FKEY,
    ),
    (
        "SQLITE_DBCONFIG_ENABLE_TRIGGER",
        ffi::SQLITE_DBCONFIG_ENABLE_TRIGGER,
    ),
    (
        "SQLITE_DBCONFIG_ENABLE_FTS3_TOKENIZER",
        ffi::SQLITE_DBCONFIG_ENABLE_FTS3_TOKENIZER,
    ),
    (
        "SQLITE_DBCONFIG_ENABLE_LOAD_EXTENSION",
        ffi::SQLITE_DBCONFIG_ENABLE_LOAD_EXTENSION,
    ),
    (
        "SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE",
        ffi::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE,
    ),
    (
        "SQLITE_DBCONFIG_ENABLE_QPSG",
        ffi::SQLITE_DBCONFIG_ENABLE_QPSG,
    ),
    (
        "SQLITE_DBCONFIG_TRIGGER_EQP",
        ffi::SQLITE_DBCONFIG_TRIGGER_EQP,
    ),
    (
        "SQLITE_DBCONFIG_RESET_DATABASE",
        ffi::SQLITE_DBCONFIG_RESET_DATABASE,
    ),
    ("SQLITE_DBCONFIG_DEFENSIVE", ffi::SQLITE_DBCONFIG_DEFENSIVE),
    (
        "SQLITE_DBCONFIG_WRITABLE_SCHEMA",
        ffi::SQLITE_DBCONFIG_WRITABLE_SCHEMA,
    ),
    (
        "SQLITE_DBCONFIG_LEGACY_ALTER_TABLE",
        ffi::SQLITE_DBCONFIG_LEGACY_ALTER_TABLE,
    ),
    ("SQLITE_DBCONFIG_DQS_DML", ffi::SQLITE_DBCONFIG_DQS_DML),
    ("SQLITE_DBCONFIG_DQS_DDL", ffi::SQLITE_DBCONFIG_DQS_DDL),
    (
        "SQLITE_DBCONFIG_ENABLE_VIEW",
        ffi::SQLITE_DBCONFIG_ENABLE_VIEW,
    ),
    (
        "SQLITE_DBCONFIG_LEGACY_FILE_FORMAT",
        ffi::SQLITE_DBCONFIG_LEGACY_FILE_FORMAT,
    ),
    (
        "SQLITE_DBCONFIG_TRUSTED_SCHEMA",
        ffi::SQLITE_DBCONFIG_TRUSTED_SCHEMA,
    ),
];

pub(crate) unsafe fn cstr_to_string(p: *const std::os::raw::c_char) -> String {
    if p.is_null() {
        return String::new();
    }
    std::ffi::CStr::from_ptr(p).to_string_lossy().into_owned()
}

/// Extract UTF-8 text from a `str` argument, accepting `str` subclasses
/// (their primitive lives in `PyInstance::native`) and surfacing
/// CPython's `UnicodeEncodeError` for lone surrogates (`WStr`).
/// Returns `None` when the object is not a string at all.
pub(crate) fn as_text(obj: &Object) -> Option<Result<String, RuntimeError>> {
    match obj {
        Object::Str(s) => Some(Ok(s.to_string())),
        Object::WStr(cps) => {
            let mut out = String::with_capacity(cps.len());
            for (i, &cp) in cps.iter().enumerate() {
                match char::from_u32(cp) {
                    Some(c) => out.push(c),
                    None => {
                        return Some(Err(RuntimeError::PyException(PyException::from_builtin(
                            "UnicodeEncodeError",
                            format!(
                                "'utf-8' codec can't encode character '\\u{cp:04x}' in \
                                     position {i}: surrogates not allowed"
                            ),
                        ))))
                    }
                }
            }
            Some(Ok(out))
        }
        Object::Instance(inst) => {
            let str_cls = crate::builtin_types::builtin_types().str_.clone();
            if inst.cls().is_subclass_of(&str_cls) {
                inst.native.get().and_then(as_text)
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Buffer-protocol extraction for BLOB binding/results: bytes,
/// bytearray, and *C-contiguous* memoryviews. A non-contiguous view is
/// CPython's BufferError; other types return `None`.
pub(crate) fn buffer_bytes(obj: &Object) -> Option<Result<Vec<u8>, RuntimeError>> {
    match obj {
        Object::Bytes(b) => Some(Ok(b.to_vec())),
        Object::ByteArray(b) => Some(Ok(b.borrow().clone())),
        Object::MemoryView(mv) => Some(if mv.is_c_contiguous() {
            Ok(mv.to_bytes())
        } else {
            Err(RuntimeError::PyException(PyException::from_builtin(
                "BufferError",
                "underlying buffer is not C-contiguous",
            )))
        }),
        _ => None,
    }
}

// ---------------------------------------------------------------
// Connection state + registry
// ---------------------------------------------------------------

/// `Connection.autocommit` sentinel (`LEGACY_TRANSACTION_CONTROL`).
pub(crate) const LEGACY_TRANSACTION_CONTROL: i64 = -1;

pub(crate) struct ConnState {
    /// Raw `sqlite3*`, stored as usize; 0 after close.
    pub db: usize,
    /// Legacy transaction control: `Some("DEFERRED"/"IMMEDIATE"/"EXCLUSIVE"/"")`
    /// or `None` (autocommit). `""` means DEFERRED.
    pub isolation_level: Option<String>,
    /// `LEGACY_TRANSACTION_CONTROL` (-1), 0 (False) or 1 (True).
    pub autocommit: i64,
    pub detect_types: i64,
    pub check_same_thread: bool,
    /// `threading.get_ident()` of the creating thread.
    pub thread_ident: i64,
    pub row_factory: Object,
    pub text_factory: Object,
    /// LRU statement cache: SQL text -> raw `sqlite3_stmt*` (as usize).
    pub stmt_cache: Vec<(String, usize)>,
    pub cached_statements: usize,
    /// Strong refs to callback payloads (functions, aggregates,
    /// collations, hooks) so their boxes outlive registration.
    pub hook_refs: Vec<Object>,
}

impl ConnState {
    pub(crate) fn db_ptr(&self) -> *mut ffi::sqlite3 {
        self.db as *mut ffi::sqlite3
    }
}

pub(crate) fn conn_registry() -> &'static parking_lot::Mutex<HashMap<i64, Rc<RefCell<ConnState>>>> {
    static REG: std::sync::OnceLock<parking_lot::Mutex<HashMap<i64, Rc<RefCell<ConnState>>>>> =
        std::sync::OnceLock::new();
    REG.get_or_init(|| parking_lot::Mutex::new(HashMap::new()))
}

pub(crate) fn next_handle() -> i64 {
    static NEXT: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(1);
    NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

pub(crate) const HANDLE_KEY: &str = "_wp_sqlite3_handle";

/// Fetch the `ConnState` behind a Connection instance, raising
/// CPython's "Cannot operate on a closed database." when appropriate.
pub(crate) fn conn_state_of(obj: &Object) -> Result<Rc<RefCell<ConnState>>, RuntimeError> {
    let Object::Instance(inst) = obj else {
        return Err(type_error("argument must be a sqlite3.Connection instance"));
    };
    let handle = inst
        .dict
        .borrow()
        .get(&DictKey(Object::from_static(HANDLE_KEY)))
        .cloned();
    match handle {
        Some(Object::Int(h)) => conn_registry().lock().get(&h).cloned().ok_or_else(|| {
            raise(
                programming_error_class(),
                "Cannot operate on a closed database.",
            )
        }),
        _ => Err(raise(
            programming_error_class(),
            "Base Connection.__init__ not called.",
        )),
    }
}

/// Like [`conn_state_of`] but also enforces open + same-thread checks
/// (`pysqlite_check_connection` + `pysqlite_check_thread`).
pub(crate) fn checked_conn(obj: &Object) -> Result<Rc<RefCell<ConnState>>, RuntimeError> {
    let st = conn_state_of(obj)?;
    {
        let s = st.borrow();
        if s.db == 0 {
            return Err(raise(
                programming_error_class(),
                "Cannot operate on a closed database.",
            ));
        }
        if s.check_same_thread {
            let here = crate::vm_singletons::current_worker_thread_id() as i64;
            if here != s.thread_ident {
                return Err(raise(
                    programming_error_class(),
                    format!(
                        "SQLite objects created in a thread can only be used in that same \
                         thread. The object was created in thread id {} and this is thread \
                         id {}.",
                        s.thread_ident, here
                    ),
                ));
            }
        }
    }
    Ok(st)
}

// ---------------------------------------------------------------
// Adapter / converter registries (microprotocols.c)
// ---------------------------------------------------------------

/// The module-level `adapters` dict (a real Python dict, shared with
/// the module attribute so user introspection sees live state). Keys
/// are `(type, protocol)` tuples exactly like CPython's.
pub(crate) fn adapters_dict() -> Rc<RefCell<DictData>> {
    static CELL: std::sync::OnceLock<Rc<RefCell<DictData>>> = std::sync::OnceLock::new();
    CELL.get_or_init(|| Rc::new(RefCell::new(DictData::default())))
        .clone()
}

/// The module-level `converters` dict; keys are uppercased type names.
pub(crate) fn converters_dict() -> Rc<RefCell<DictData>> {
    static CELL: std::sync::OnceLock<Rc<RefCell<DictData>>> = std::sync::OnceLock::new();
    CELL.get_or_init(|| Rc::new(RefCell::new(DictData::default())))
        .clone()
}

/// Whether any adapter has been registered at all — CPython skips the
/// adaptation round-trip for exact builtin types until the first
/// `register_adapter` call (`pysqlite_BaseTypeAdapted` semantics,
/// collapsed to "any registration" like modern CPython).
pub(crate) fn have_adapters() -> bool {
    !adapters_dict().borrow().is_empty()
}

pub(crate) fn prepare_protocol_class() -> Rc<TypeObject> {
    static CELL: std::sync::OnceLock<Rc<TypeObject>> = std::sync::OnceLock::new();
    CELL.get_or_init(|| {
        let mut dict = DictData::default();
        dict.insert(
            DictKey(Object::from_static("__module__")),
            Object::from_static("sqlite3"),
        );
        dict.insert(
            DictKey(Object::from_static("__doc__")),
            Object::from_static("PEP 246 style object adaption protocol type."),
        );
        let bt = crate::builtin_types::builtin_types();
        TypeObject::new_user("PrepareProtocol", vec![bt.object_.clone()], dict)
            .expect("PrepareProtocol must linearise")
    })
    .clone()
}

/// `adapt(obj, proto=PrepareProtocol, alt=<sentinel>)` —
/// `pysqlite_microprotocols_adapt`.
pub(crate) fn adapt_object(
    ip: &mut Interp,
    obj: &Object,
    proto: &Object,
    alt: Option<&Object>,
) -> Result<Object, RuntimeError> {
    // 1. Registered adapter for the exact type.
    let cls = crate::builtins::class_of(obj);
    let key = DictKey(Object::new_tuple(vec![Object::Type(cls), proto.clone()]));
    let adapter = adapters_dict().borrow().get(&key).cloned();
    if let Some(adapter) = adapter {
        return call(ip, &adapter, std::slice::from_ref(obj));
    }
    // 2. Ask the protocol itself: `proto.__adapt__(obj)` (PEP 246 order
    // in `pysqlite_microprotocols_adapt`). A TypeError means "cannot
    // adapt"; anything else propagates.
    if let Ok(adapt) = ip.load_attr_public(proto, "__adapt__") {
        match call(ip, &adapt, std::slice::from_ref(obj)) {
            Ok(Object::None) => {}
            Ok(adapted) => return Ok(adapted),
            Err(e) => {
                if !is_type_error(&e) {
                    return Err(e);
                }
            }
        }
    }
    // 3. `__conform__(protocol)`.
    if let Ok(conform) = ip.load_attr_public(obj, "__conform__") {
        match call(ip, &conform, std::slice::from_ref(proto)) {
            Ok(Object::None) => {}
            Ok(adapted) => return Ok(adapted),
            Err(e) => {
                // A TypeError from __conform__ means "cannot conform";
                // anything else propagates (microprotocols.c).
                if !is_type_error(&e) {
                    return Err(e);
                }
            }
        }
    }
    // 3. Fall back.
    match alt {
        Some(a) => Ok(a.clone()),
        None => Err(raise(
            programming_error_class(),
            format!("can't adapt type '{}'", obj.type_name_owned()),
        )),
    }
}

pub(crate) fn is_type_error(e: &RuntimeError) -> bool {
    match e {
        RuntimeError::PyException(exc) => {
            if let Object::Instance(inst) = &exc.instance {
                crate::builtin_types::builtin_types()
                    .by_name("TypeError")
                    .is_some_and(|te| inst.cls().is_subclass_of(&te))
            } else {
                false
            }
        }
        _ => false,
    }
}

// ---------------------------------------------------------------
// Small builders shared by the class files
// ---------------------------------------------------------------

pub(crate) fn method(
    name: &'static str,
    body: fn(&[Object]) -> Result<Object, RuntimeError>,
) -> Object {
    Object::Builtin(Rc::new(BuiltinFn {
        name,
        binds_instance: true,
        call: Box::new(body),
        call_kw: None,
    }))
}

pub(crate) fn method_kw(
    name: &'static str,
    body: fn(&[Object], &[(String, Object)]) -> Result<Object, RuntimeError>,
) -> Object {
    Object::Builtin(Rc::new(BuiltinFn {
        name,
        binds_instance: true,
        call: Box::new(move |args| body(args, &[])),
        call_kw: Some(Box::new(body)),
    }))
}

pub(crate) fn modfn(
    name: &'static str,
    body: fn(&[Object]) -> Result<Object, RuntimeError>,
) -> Object {
    Object::Builtin(Rc::new(BuiltinFn {
        name,
        binds_instance: false,
        call: Box::new(body),
        call_kw: None,
    }))
}

pub(crate) fn modfn_kw(
    name: &'static str,
    body: fn(&[Object], &[(String, Object)]) -> Result<Object, RuntimeError>,
) -> Object {
    Object::Builtin(Rc::new(BuiltinFn {
        name,
        binds_instance: false,
        call: Box::new(move |args| body(args, &[])),
        call_kw: Some(Box::new(body)),
    }))
}

pub(crate) fn install_getset(
    cls: &Rc<TypeObject>,
    name: &'static str,
    getter: fn(&[Object]) -> Result<Object, RuntimeError>,
    setter: Option<fn(&[Object]) -> Result<Object, RuntimeError>>,
) {
    let fset = match setter {
        Some(s) => Object::Builtin(Rc::new(BuiltinFn {
            name,
            binds_instance: true,
            call: Box::new(s),
            call_kw: None,
        })),
        None => Object::None,
    };
    let prop = Object::Property(Rc::new(PyProperty::new(
        Object::Builtin(Rc::new(BuiltinFn {
            name,
            binds_instance: true,
            call: Box::new(getter),
            call_kw: None,
        })),
        fset,
        Object::None,
        Object::None,
    )));
    crate::descr_registry::register(
        &prop,
        crate::descr_registry::DescrKind::GetSet,
        cls.clone(),
        name,
        None,
    );
    cls.dict
        .borrow_mut()
        .insert(DictKey(Object::from_static(name)), prop);
}

/// `callable()` semantics for validating user-supplied hooks.
pub(crate) fn is_callable(obj: &Object) -> bool {
    match obj {
        Object::Function(_)
        | Object::Builtin(_)
        | Object::BoundMethod(_)
        | Object::Type(_)
        | Object::StaticMethod(_) => true,
        Object::Instance(inst) => inst.cls().lookup("__call__").is_some(),
        _ => false,
    }
}

pub(crate) fn self_instance(args: &[Object]) -> Result<Rc<PyInstance>, RuntimeError> {
    match args.first() {
        Some(Object::Instance(inst)) => Ok(inst.clone()),
        _ => Err(type_error("method requires an instance")),
    }
}

/// Read a keyword argument, tolerating both positions and names.
pub(crate) fn kwarg<'a>(kwargs: &'a [(String, Object)], name: &str) -> Option<&'a Object> {
    kwargs.iter().find(|(k, _)| k == name).map(|(_, v)| v)
}

// ---------------------------------------------------------------
// Module-level functions
// ---------------------------------------------------------------

fn mod_connect(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let ip = interp()?;
    if args.len() > 1 {
        // Clinic deprecation on >1 positional (gh-107948, Python 3.15).
        ip.warn_deprecation_from_builtin(
            "Passing more than 1 positional argument to sqlite3.connect() is deprecated. \
             Parameters 'timeout', 'detect_types', 'isolation_level', 'check_same_thread', \
             'factory', 'cached_statements' and 'uri' will become keyword-only parameters \
             in Python 3.15."
                .to_owned(),
        )?;
    }
    // `factory` is consumed here; everything else is forwarded.
    let mut factory = Object::Type(connection::connection_class());
    let mut fwd_kwargs: Vec<(String, Object)> = Vec::new();
    for (k, v) in kwargs {
        if k == "factory" {
            factory = v.clone();
        } else {
            fwd_kwargs.push((k.clone(), v.clone()));
        }
    }
    // `factory` may also arrive positionally (6th positional arg).
    let mut fwd_args: Vec<Object> = args.to_vec();
    if fwd_args.len() >= 6 {
        factory = fwd_args.remove(5);
    }
    call_kw(ip, &factory, &fwd_args, &fwd_kwargs)
}

fn mod_complete_statement(args: &[Object]) -> Result<Object, RuntimeError> {
    let sql = match args.first() {
        Some(Object::Str(s)) => s.to_string(),
        _ => return Err(type_error("complete_statement() argument must be str")),
    };
    let c = std::ffi::CString::new(sql).map_err(|_| {
        raise(
            programming_error_class(),
            "the query contains a null character",
        )
    })?;
    // SAFETY: `c` is a valid NUL-terminated string for the duration of the call.
    let complete = unsafe { ffi::sqlite3_complete(c.as_ptr()) };
    Ok(Object::Bool(complete != 0))
}

fn mod_register_adapter(args: &[Object]) -> Result<Object, RuntimeError> {
    let (ty, caster) = match (args.first(), args.get(1)) {
        (Some(t @ Object::Type(_)), Some(c)) => (t.clone(), c.clone()),
        _ => return Err(type_error("register_adapter(type, callable)")),
    };
    let key = DictKey(Object::new_tuple(vec![
        ty,
        Object::Type(prepare_protocol_class()),
    ]));
    adapters_dict().borrow_mut().insert(key, caster);
    Ok(Object::None)
}

fn mod_register_converter(args: &[Object]) -> Result<Object, RuntimeError> {
    let (name, converter) = match (args.first(), args.get(1)) {
        (Some(Object::Str(s)), Some(c)) => (s.to_string(), c.clone()),
        _ => return Err(type_error("register_converter(name, callable)")),
    };
    converters_dict()
        .borrow_mut()
        .insert(DictKey(Object::from_str(name.to_uppercase())), converter);
    Ok(Object::None)
}

fn mod_adapt(args: &[Object]) -> Result<Object, RuntimeError> {
    let ip = interp()?;
    let obj = args
        .first()
        .ok_or_else(|| type_error("adapt() missing required argument 'obj'"))?;
    let default_proto = Object::Type(prepare_protocol_class());
    let proto = args.get(1).cloned().unwrap_or(default_proto);
    adapt_object(ip, obj, &proto, args.get(2))
}

fn mod_enable_callback_tracebacks(args: &[Object]) -> Result<Object, RuntimeError> {
    let flag = args.first().and_then(Object::as_i64).unwrap_or(0);
    hooks::set_callback_tracebacks(flag != 0);
    Ok(Object::None)
}

// ---------------------------------------------------------------
// build()
// ---------------------------------------------------------------

pub fn build(_cache: &ModuleCache) -> Rc<PyModule> {
    let dict = Rc::new(RefCell::new(DictData::default()));
    {
        let mut d = dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("__name__")),
            Object::from_static("_sqlite3"),
        );
        d.insert(
            DictKey(Object::from_static("__doc__")),
            Object::from_static("SQLite database access module (RFC 0056 native core)."),
        );

        // Version strings. `_deprecated_version` is the pysqlite lineage
        // constant dbapi2.py re-exports through the deprecation shim.
        // SAFETY: sqlite3_libversion returns a static string.
        let libversion = unsafe { cstr_to_string(ffi::sqlite3_libversion()) };
        d.insert(
            DictKey(Object::from_static("sqlite_version")),
            Object::from_str(libversion),
        );
        d.insert(
            DictKey(Object::from_static("_deprecated_version")),
            Object::from_static("2.6.0"),
        );

        // Classes.
        d.insert(
            DictKey(Object::from_static("Connection")),
            Object::Type(connection::connection_class()),
        );
        d.insert(
            DictKey(Object::from_static("Cursor")),
            Object::Type(cursor::cursor_class()),
        );
        d.insert(
            DictKey(Object::from_static("Row")),
            Object::Type(row::row_class()),
        );
        d.insert(
            DictKey(Object::from_static("Blob")),
            Object::Type(row::blob_class()),
        );
        d.insert(
            DictKey(Object::from_static("PrepareProtocol")),
            Object::Type(prepare_protocol_class()),
        );

        // Exceptions.
        for (name, cls) in [
            ("Warning", warning_class()),
            ("Error", error_class()),
            ("InterfaceError", interface_error_class()),
            ("DatabaseError", database_error_class()),
            ("DataError", data_error_class()),
            ("OperationalError", operational_error_class()),
            ("IntegrityError", integrity_error_class()),
            ("InternalError", internal_error_class()),
            ("ProgrammingError", programming_error_class()),
            ("NotSupportedError", not_supported_error_class()),
        ] {
            d.insert(DictKey(Object::from_str(name)), Object::Type(cls));
        }

        // Module functions.
        d.insert(
            DictKey(Object::from_static("connect")),
            modfn_kw("connect", mod_connect),
        );
        d.insert(
            DictKey(Object::from_static("complete_statement")),
            modfn("complete_statement", mod_complete_statement),
        );
        d.insert(
            DictKey(Object::from_static("register_adapter")),
            modfn("register_adapter", mod_register_adapter),
        );
        d.insert(
            DictKey(Object::from_static("register_converter")),
            modfn("register_converter", mod_register_converter),
        );
        d.insert(
            DictKey(Object::from_static("adapt")),
            modfn("adapt", mod_adapt),
        );
        d.insert(
            DictKey(Object::from_static("enable_callback_tracebacks")),
            modfn("enable_callback_tracebacks", mod_enable_callback_tracebacks),
        );

        // Live registries.
        d.insert(
            DictKey(Object::from_static("adapters")),
            Object::Dict(adapters_dict()),
        );
        d.insert(
            DictKey(Object::from_static("converters")),
            Object::Dict(converters_dict()),
        );

        // Constants.
        d.insert(
            DictKey(Object::from_static("PARSE_DECLTYPES")),
            Object::Int(1),
        );
        d.insert(
            DictKey(Object::from_static("PARSE_COLNAMES")),
            Object::Int(2),
        );
        d.insert(
            DictKey(Object::from_static("LEGACY_TRANSACTION_CONTROL")),
            Object::Int(LEGACY_TRANSACTION_CONTROL),
        );
        d.insert(DictKey(Object::from_static("threadsafety")), Object::Int(3));
        d.insert(
            DictKey(Object::from_static("apilevel")),
            Object::from_static("2.0"),
        );
        d.insert(
            DictKey(Object::from_static("paramstyle")),
            Object::from_static("qmark"),
        );
        for (name, value) in ERROR_CODES
            .iter()
            .chain(EXTENDED_ERROR_CODES)
            .chain(DBCONFIG_CODES)
        {
            d.insert(
                DictKey(Object::from_str(*name)),
                Object::Int(i64::from(*value)),
            );
        }
        for (name, value) in AUTHORIZER_CODES {
            d.insert(
                DictKey(Object::from_str(*name)),
                Object::Int(i64::from(*value)),
            );
        }
        for (name, value) in LIMIT_CODES {
            d.insert(
                DictKey(Object::from_str(*name)),
                Object::Int(i64::from(*value)),
            );
        }
    }
    Rc::new(PyModule {
        name: "_sqlite3".to_owned(),
        filename: None,
        dict,
    })
}

const AUTHORIZER_CODES: &[(&str, i32)] = &[
    ("SQLITE_DENY", ffi::SQLITE_DENY),
    ("SQLITE_IGNORE", ffi::SQLITE_IGNORE),
    ("SQLITE_CREATE_INDEX", ffi::SQLITE_CREATE_INDEX),
    ("SQLITE_CREATE_TABLE", ffi::SQLITE_CREATE_TABLE),
    ("SQLITE_CREATE_TEMP_INDEX", ffi::SQLITE_CREATE_TEMP_INDEX),
    ("SQLITE_CREATE_TEMP_TABLE", ffi::SQLITE_CREATE_TEMP_TABLE),
    (
        "SQLITE_CREATE_TEMP_TRIGGER",
        ffi::SQLITE_CREATE_TEMP_TRIGGER,
    ),
    ("SQLITE_CREATE_TEMP_VIEW", ffi::SQLITE_CREATE_TEMP_VIEW),
    ("SQLITE_CREATE_TRIGGER", ffi::SQLITE_CREATE_TRIGGER),
    ("SQLITE_CREATE_VIEW", ffi::SQLITE_CREATE_VIEW),
    ("SQLITE_DELETE", ffi::SQLITE_DELETE),
    ("SQLITE_DROP_INDEX", ffi::SQLITE_DROP_INDEX),
    ("SQLITE_DROP_TABLE", ffi::SQLITE_DROP_TABLE),
    ("SQLITE_DROP_TEMP_INDEX", ffi::SQLITE_DROP_TEMP_INDEX),
    ("SQLITE_DROP_TEMP_TABLE", ffi::SQLITE_DROP_TEMP_TABLE),
    ("SQLITE_DROP_TEMP_TRIGGER", ffi::SQLITE_DROP_TEMP_TRIGGER),
    ("SQLITE_DROP_TEMP_VIEW", ffi::SQLITE_DROP_TEMP_VIEW),
    ("SQLITE_DROP_TRIGGER", ffi::SQLITE_DROP_TRIGGER),
    ("SQLITE_DROP_VIEW", ffi::SQLITE_DROP_VIEW),
    ("SQLITE_INSERT", ffi::SQLITE_INSERT),
    ("SQLITE_PRAGMA", ffi::SQLITE_PRAGMA),
    ("SQLITE_READ", ffi::SQLITE_READ),
    ("SQLITE_SELECT", ffi::SQLITE_SELECT),
    ("SQLITE_TRANSACTION", ffi::SQLITE_TRANSACTION),
    ("SQLITE_UPDATE", ffi::SQLITE_UPDATE),
    ("SQLITE_ATTACH", ffi::SQLITE_ATTACH),
    ("SQLITE_DETACH", ffi::SQLITE_DETACH),
    ("SQLITE_ALTER_TABLE", ffi::SQLITE_ALTER_TABLE),
    ("SQLITE_REINDEX", ffi::SQLITE_REINDEX),
    ("SQLITE_ANALYZE", ffi::SQLITE_ANALYZE),
    ("SQLITE_CREATE_VTABLE", ffi::SQLITE_CREATE_VTABLE),
    ("SQLITE_DROP_VTABLE", ffi::SQLITE_DROP_VTABLE),
    ("SQLITE_FUNCTION", ffi::SQLITE_FUNCTION),
    ("SQLITE_SAVEPOINT", ffi::SQLITE_SAVEPOINT),
    ("SQLITE_RECURSIVE", ffi::SQLITE_RECURSIVE),
];

const LIMIT_CODES: &[(&str, i32)] = &[
    ("SQLITE_LIMIT_LENGTH", ffi::SQLITE_LIMIT_LENGTH),
    ("SQLITE_LIMIT_SQL_LENGTH", ffi::SQLITE_LIMIT_SQL_LENGTH),
    ("SQLITE_LIMIT_COLUMN", ffi::SQLITE_LIMIT_COLUMN),
    ("SQLITE_LIMIT_EXPR_DEPTH", ffi::SQLITE_LIMIT_EXPR_DEPTH),
    (
        "SQLITE_LIMIT_COMPOUND_SELECT",
        ffi::SQLITE_LIMIT_COMPOUND_SELECT,
    ),
    ("SQLITE_LIMIT_VDBE_OP", ffi::SQLITE_LIMIT_VDBE_OP),
    ("SQLITE_LIMIT_FUNCTION_ARG", ffi::SQLITE_LIMIT_FUNCTION_ARG),
    ("SQLITE_LIMIT_ATTACHED", ffi::SQLITE_LIMIT_ATTACHED),
    (
        "SQLITE_LIMIT_LIKE_PATTERN_LENGTH",
        ffi::SQLITE_LIMIT_LIKE_PATTERN_LENGTH,
    ),
    (
        "SQLITE_LIMIT_VARIABLE_NUMBER",
        ffi::SQLITE_LIMIT_VARIABLE_NUMBER,
    ),
    (
        "SQLITE_LIMIT_TRIGGER_DEPTH",
        ffi::SQLITE_LIMIT_TRIGGER_DEPTH,
    ),
    (
        "SQLITE_LIMIT_WORKER_THREADS",
        ffi::SQLITE_LIMIT_WORKER_THREADS,
    ),
];

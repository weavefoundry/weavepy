//! The `Connection` heap type (`connection.c`).

use std::os::raw::c_int;

use rusqlite::ffi;

use super::stmt::exec_simple;
use super::{
    call, checked_conn, conn_registry, conn_state_of, cursor, hooks, install_getset, interp,
    is_callable, kwarg, method, method_kw, next_handle, operational_error_class,
    programming_error_class, raise, raise_sqlite_error, row, self_instance, ConnState, HANDLE_KEY,
    LEGACY_TRANSACTION_CONTROL,
};
use crate::error::{type_error, value_error, RuntimeError};
use crate::object::{DictData, DictKey, Object};
use crate::sync::Rc;
use crate::sync::RefCell;
use crate::types::TypeObject;

pub(crate) fn connection_class() -> Rc<TypeObject> {
    static CELL: std::sync::OnceLock<Rc<TypeObject>> = std::sync::OnceLock::new();
    CELL.get_or_init(|| {
        let bt = crate::builtin_types::builtin_types();
        let mut dict = DictData::default();
        dict.insert(
            DictKey(Object::from_static("__module__")),
            Object::from_static("sqlite3"),
        );
        dict.insert(
            DictKey(Object::from_static("__init__")),
            method_kw("__init__", conn_init),
        );
        dict.insert(
            DictKey(Object::from_static("cursor")),
            method_kw("cursor", conn_cursor),
        );
        dict.insert(
            DictKey(Object::from_static("execute")),
            method("execute", conn_execute),
        );
        dict.insert(
            DictKey(Object::from_static("executemany")),
            method("executemany", conn_executemany),
        );
        dict.insert(
            DictKey(Object::from_static("executescript")),
            method("executescript", conn_executescript),
        );
        dict.insert(
            DictKey(Object::from_static("commit")),
            method("commit", conn_commit),
        );
        dict.insert(
            DictKey(Object::from_static("rollback")),
            method("rollback", conn_rollback),
        );
        dict.insert(
            DictKey(Object::from_static("close")),
            method("close", conn_close),
        );
        dict.insert(
            DictKey(Object::from_static("create_function")),
            method_kw("create_function", hooks::conn_create_function),
        );
        dict.insert(
            DictKey(Object::from_static("create_aggregate")),
            method_kw("create_aggregate", hooks::conn_create_aggregate),
        );
        dict.insert(
            DictKey(Object::from_static("create_window_function")),
            method_kw("create_window_function", hooks::conn_create_window_function),
        );
        dict.insert(
            DictKey(Object::from_static("create_collation")),
            method("create_collation", hooks::conn_create_collation),
        );
        dict.insert(
            DictKey(Object::from_static("set_authorizer")),
            method_kw("set_authorizer", hooks::conn_set_authorizer),
        );
        dict.insert(
            DictKey(Object::from_static("set_progress_handler")),
            method_kw("set_progress_handler", hooks::conn_set_progress_handler),
        );
        dict.insert(
            DictKey(Object::from_static("set_trace_callback")),
            method_kw("set_trace_callback", hooks::conn_set_trace_callback),
        );
        dict.insert(
            DictKey(Object::from_static("interrupt")),
            method("interrupt", conn_interrupt),
        );
        dict.insert(
            DictKey(Object::from_static("backup")),
            method_kw("backup", conn_backup),
        );
        dict.insert(
            DictKey(Object::from_static("serialize")),
            method_kw("serialize", conn_serialize),
        );
        dict.insert(
            DictKey(Object::from_static("deserialize")),
            method_kw("deserialize", conn_deserialize),
        );
        dict.insert(
            DictKey(Object::from_static("blobopen")),
            method_kw("blobopen", conn_blobopen),
        );
        dict.insert(
            DictKey(Object::from_static("getlimit")),
            method("getlimit", conn_getlimit),
        );
        dict.insert(
            DictKey(Object::from_static("setlimit")),
            method("setlimit", conn_setlimit),
        );
        dict.insert(
            DictKey(Object::from_static("getconfig")),
            method("getconfig", conn_getconfig),
        );
        dict.insert(
            DictKey(Object::from_static("setconfig")),
            method("setconfig", conn_setconfig),
        );
        dict.insert(
            DictKey(Object::from_static("iterdump")),
            method_kw("iterdump", conn_iterdump),
        );
        // The dotted builtin name keys the `__text_signature__` table
        // (inspect.signature(cx) must yield "(sql, /)").
        dict.insert(
            DictKey(Object::from_static("__call__")),
            method(".sqlite3.Connection.__call__", conn_call),
        );
        dict.insert(
            DictKey(Object::from_static("__enter__")),
            method("__enter__", conn_enter),
        );
        dict.insert(
            DictKey(Object::from_static("__exit__")),
            method("__exit__", conn_exit),
        );
        dict.insert(
            DictKey(Object::from_static("__del__")),
            method("__del__", conn_del),
        );
        // DB-API 2.0 extension: the exception classes double as
        // Connection attributes (module.c stashes them on the type).
        for (name, exc) in [
            ("Warning", super::warning_class()),
            ("Error", super::error_class()),
            ("InterfaceError", super::interface_error_class()),
            ("DatabaseError", super::database_error_class()),
            ("DataError", super::data_error_class()),
            ("OperationalError", operational_error_class()),
            ("IntegrityError", super::integrity_error_class()),
            ("InternalError", super::internal_error_class()),
            ("ProgrammingError", programming_error_class()),
            ("NotSupportedError", super::not_supported_error_class()),
        ] {
            dict.insert(DictKey(Object::from_str(name)), Object::Type(exc));
        }
        let cls = TypeObject::new_user("Connection", vec![bt.object_.clone()], dict)
            .expect("Connection class must linearise");
        install_getset(
            &cls,
            "isolation_level",
            get_isolation_level,
            Some(set_isolation_level),
        );
        install_getset(&cls, "autocommit", get_autocommit, Some(set_autocommit));
        install_getset(&cls, "in_transaction", get_in_transaction, None);
        install_getset(&cls, "total_changes", get_total_changes, None);
        install_getset(&cls, "row_factory", get_row_factory, Some(set_row_factory));
        install_getset(
            &cls,
            "text_factory",
            get_text_factory,
            Some(set_text_factory),
        );
        cls
    })
    .clone()
}

// ---------------------------------------------------------------
// __init__ / open / close
// ---------------------------------------------------------------

fn database_path(ip: &mut super::Interp, obj: &Object) -> Result<Vec<u8>, RuntimeError> {
    match obj {
        Object::Str(s) => Ok(s.to_string().into_bytes()),
        Object::Bytes(b) => Ok(b.to_vec()),
        other => {
            // os.PathLike: call __fspath__ through the VM.
            let fspath = ip.load_attr_public(other, "__fspath__").map_err(|_| {
                type_error(format!(
                    "database must be a path-like object, not {}",
                    other.type_name_owned()
                ))
            })?;
            match call(ip, &fspath, &[])? {
                Object::Str(s) => Ok(s.to_string().into_bytes()),
                Object::Bytes(b) => Ok(b.to_vec()),
                _ => Err(type_error("__fspath__ must return str or bytes")),
            }
        }
    }
}

fn conn_init(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let ip = interp()?;
    let inst = self_instance(args)?;
    let pos = &args[1..];
    if pos.len() > 1 {
        ip.warn_deprecation_from_builtin(
            "Passing more than 1 positional argument to _sqlite3.Connection() is deprecated. \
             Parameters 'timeout', 'detect_types', 'isolation_level', 'check_same_thread', \
             'factory', 'cached_statements' and 'uri' will become keyword-only parameters \
             in Python 3.15."
                .to_owned(),
        )?;
    }

    // Re-init drops the previous state *first* (CPython clears
    // `self->initialized` before reopening) so a failed re-init leaves
    // the connection unusable rather than pointing at the old database.
    {
        let old = inst
            .dict
            .borrow_mut()
            .shift_remove(&DictKey(Object::from_static(HANDLE_KEY)));
        if let Some(Object::Int(h)) = old {
            if let Some(prev) = conn_registry().lock().remove(&h) {
                close_state(&prev);
            }
        }
    }

    let database = pos
        .first()
        .cloned()
        .or_else(|| kwarg(kwargs, "database").cloned())
        .ok_or_else(|| type_error("Connection() missing required argument 'database'"))?;
    let timeout = pos
        .get(1)
        .or_else(|| kwarg(kwargs, "timeout"))
        .and_then(Object::as_f64)
        .unwrap_or(5.0);
    let detect_types = pos
        .get(2)
        .or_else(|| kwarg(kwargs, "detect_types"))
        .and_then(Object::as_i64)
        .unwrap_or(0);
    let isolation_level_obj = pos
        .get(3)
        .or_else(|| kwarg(kwargs, "isolation_level"))
        .cloned()
        .unwrap_or_else(|| Object::from_static(""));
    let check_same_thread = pos
        .get(4)
        .or_else(|| kwarg(kwargs, "check_same_thread"))
        .map(|o| !matches!(o, Object::Bool(false) | Object::Int(0)))
        .unwrap_or(true);
    // pos.get(5) is `factory`, accepted and ignored (connect() consumed it).
    let cached_statements = pos
        .get(6)
        .or_else(|| kwarg(kwargs, "cached_statements"))
        .and_then(Object::as_i64)
        .unwrap_or(128)
        .max(0) as usize;
    let uri = pos
        .get(7)
        .or_else(|| kwarg(kwargs, "uri"))
        .map(|o| !matches!(o, Object::Bool(false) | Object::Int(0)))
        .unwrap_or(false);
    let autocommit = match kwarg(kwargs, "autocommit") {
        Some(o) => parse_autocommit(o)?,
        None => LEGACY_TRANSACTION_CONTROL,
    };

    let isolation_level = parse_isolation_level(&isolation_level_obj)?;

    // PEP 578: CPython's `pysqlite_connection_init` audits
    // `sqlite3.connect(database)` before opening and
    // `sqlite3.connect/handle(connection)` after — both module-level
    // `connect()` and direct `Connection()` produce the pair.
    crate::stdlib::sys::audit_event("sqlite3.connect", std::slice::from_ref(&database))?;

    let path = database_path(ip, &database)?;
    let c_path = std::ffi::CString::new(path).map_err(|_| value_error("embedded null byte"))?;
    let mut flags =
        ffi::SQLITE_OPEN_READWRITE | ffi::SQLITE_OPEN_CREATE | ffi::SQLITE_OPEN_FULLMUTEX;
    if uri {
        flags |= ffi::SQLITE_OPEN_URI;
    }
    let mut db: *mut ffi::sqlite3 = std::ptr::null_mut();
    // SAFETY: `c_path` is a valid C string; `db` is a valid out-pointer.
    let rc = unsafe { ffi::sqlite3_open_v2(c_path.as_ptr(), &raw mut db, flags, std::ptr::null()) };
    if rc != ffi::SQLITE_OK {
        let err = raise_sqlite_error(db, rc);
        if !db.is_null() {
            // SAFETY: open failed but allocated a handle; close it.
            unsafe { ffi::sqlite3_close_v2(db) };
        }
        return Err(err);
    }
    // SAFETY: live handle from the successful open above.
    unsafe {
        ffi::sqlite3_busy_timeout(db, (timeout * 1000.0) as c_int);
        // CPython enables extended result codes since 3.11.
        ffi::sqlite3_extended_result_codes(db, 1);
    }

    let state = Rc::new(RefCell::new(ConnState {
        db: db as usize,
        isolation_level,
        autocommit,
        detect_types,
        check_same_thread,
        thread_ident: crate::vm_singletons::current_worker_thread_id() as i64,
        row_factory: Object::None,
        text_factory: str_type_object(ip),
        stmt_cache: Vec::new(),
        cached_statements,
        hook_refs: Vec::new(),
    }));

    // autocommit=False begins a transaction immediately (PEP 249 mode).
    if autocommit == 0 {
        exec_simple(db, "BEGIN")?;
    }

    let handle = next_handle();
    conn_registry().lock().insert(handle, state);
    inst.dict.borrow_mut().insert(
        DictKey(Object::from_static(HANDLE_KEY)),
        Object::Int(handle),
    );
    crate::stdlib::sys::audit_event("sqlite3.connect/handle", std::slice::from_ref(&args[0]))?;
    Ok(Object::None)
}

fn str_type_object(ip: &mut super::Interp) -> Object {
    ip.builtins_dict()
        .borrow()
        .get(&DictKey(Object::from_static("str")))
        .cloned()
        .unwrap_or(Object::None)
}

/// The `isolation_level` converter (connection.c
/// `isolation_level_converter`): accepts None or any str (subclasses
/// included — CPython never calls methods like `.upper()` on it),
/// validates the value case-insensitively, and keeps the *original*
/// string (the begin statement is literally `"BEGIN " + level`).
fn parse_isolation_level(obj: &Object) -> Result<Option<String>, RuntimeError> {
    if matches!(obj, Object::None) {
        return Ok(None);
    }
    match super::as_text(obj) {
        Some(text) => {
            let val = text?;
            let upper = val.to_ascii_uppercase();
            if val.contains('\0') {
                return Err(value_error(
                    "isolation_level string must be '', 'DEFERRED', 'IMMEDIATE', or 'EXCLUSIVE'",
                ));
            }
            if upper.is_empty()
                || upper == "DEFERRED"
                || upper == "IMMEDIATE"
                || upper == "EXCLUSIVE"
            {
                Ok(Some(val))
            } else {
                Err(value_error(
                    "isolation_level string must be '', 'DEFERRED', 'IMMEDIATE', or 'EXCLUSIVE'",
                ))
            }
        }
        None => Err(type_error(format!(
            "isolation_level must be str or None, not {}",
            obj.type_name_owned()
        ))),
    }
}

/// `autocommit_converter`: exactly True, False, or the LEGACY sentinel.
fn parse_autocommit(obj: &Object) -> Result<i64, RuntimeError> {
    match obj {
        Object::Bool(true) => Ok(1),
        Object::Bool(false) => Ok(0),
        Object::Int(v) if *v == LEGACY_TRANSACTION_CONTROL => Ok(LEGACY_TRANSACTION_CONTROL),
        Object::Long(b)
            if num_traits::ToPrimitive::to_i64(b.as_ref()) == Some(LEGACY_TRANSACTION_CONTROL) =>
        {
            Ok(LEGACY_TRANSACTION_CONTROL)
        }
        _ => Err(value_error(
            "autocommit must be True, False, or sqlite3.LEGACY_TRANSACTION_CONTROL",
        )),
    }
}

fn close_state(state: &Rc<RefCell<ConnState>>) {
    let mut s = state.borrow_mut();
    if s.db == 0 {
        return;
    }
    // autocommit=False rolls back the implicit transaction on close
    // (connection.c `connection_close`); the trace callback still sees
    // the ROLLBACK statement.
    // SAFETY: live db handle throughout this block.
    if s.autocommit == 0 && unsafe { ffi::sqlite3_get_autocommit(s.db_ptr()) } == 0 {
        let _ = exec_simple(s.db_ptr(), "ROLLBACK");
    }
    for (_, ptr) in s.stmt_cache.drain(..) {
        // SAFETY: cached statements are live and owned by the cache.
        unsafe { ffi::sqlite3_finalize(ptr as *mut ffi::sqlite3_stmt) };
    }
    // SAFETY: close_v2 defers teardown until outstanding statements
    // (e.g. cursors mid-iteration) finalize; safe to call once here.
    unsafe { ffi::sqlite3_close_v2(s.db as *mut ffi::sqlite3) };
    s.db = 0;
    s.hook_refs.clear();
}

fn conn_close(args: &[Object]) -> Result<Object, RuntimeError> {
    // `close()` on an already-closed connection is a no-op; on a
    // never-initialised one it's the Base-init ProgrammingError.
    let state = conn_state_of(args.first().unwrap_or(&Object::None))?;
    {
        // Thread check still applies.
        let s = state.borrow();
        if s.db != 0 && s.check_same_thread {
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
    close_state(&state);
    Ok(Object::None)
}

fn conn_del(args: &[Object]) -> Result<Object, RuntimeError> {
    if let Some(obj) = args.first() {
        if let Ok(state) = conn_state_of(obj) {
            // CPython's connection finaliser warns about an unclosed
            // database before tearing it down.
            if state.borrow().db != 0 {
                if let Ok(ip) = interp() {
                    let _ = ip
                        .warn_resource_from_builtin(format!("unclosed database in {}", obj.repr()));
                }
            }
            close_state(&state);
            if let Object::Instance(inst) = obj {
                let handle = inst
                    .dict
                    .borrow()
                    .get(&DictKey(Object::from_static(HANDLE_KEY)))
                    .cloned();
                if let Some(Object::Int(h)) = handle {
                    conn_registry().lock().remove(&h);
                }
            }
        }
    }
    Ok(Object::None)
}

// ---------------------------------------------------------------
// Cursors + execute* conveniences
// ---------------------------------------------------------------

fn conn_cursor(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let ip = interp()?;
    let self_obj = args
        .first()
        .cloned()
        .ok_or_else(|| type_error("cursor() requires a Connection"))?;
    checked_conn(&self_obj)?;
    let factory = args
        .get(1)
        .or_else(|| kwarg(kwargs, "factory"))
        .cloned()
        .unwrap_or_else(|| Object::Type(cursor::cursor_class()));
    let cur = call(ip, &factory, &[self_obj])?;
    // CPython type-checks the factory result.
    let ok = matches!(
        &cur,
        Object::Instance(i) if i.cls().is_subclass_of(&cursor::cursor_class())
    );
    if !ok {
        return Err(type_error(format!(
            "factory must return a cursor, not {}",
            cur.type_name_owned()
        )));
    }
    Ok(cur)
}

fn conn_execute(args: &[Object]) -> Result<Object, RuntimeError> {
    let ip = interp()?;
    let self_obj = args
        .first()
        .cloned()
        .ok_or_else(|| type_error("execute() requires a Connection"))?;
    let cur = conn_cursor(std::slice::from_ref(&self_obj), &[])?;
    let execute = ip.load_attr_public(&cur, "execute")?;
    call(ip, &execute, &args[1..])?;
    Ok(cur)
}

fn conn_executemany(args: &[Object]) -> Result<Object, RuntimeError> {
    let ip = interp()?;
    let self_obj = args
        .first()
        .cloned()
        .ok_or_else(|| type_error("executemany() requires a Connection"))?;
    let cur = conn_cursor(std::slice::from_ref(&self_obj), &[])?;
    let executemany = ip.load_attr_public(&cur, "executemany")?;
    call(ip, &executemany, &args[1..])?;
    Ok(cur)
}

fn conn_executescript(args: &[Object]) -> Result<Object, RuntimeError> {
    let ip = interp()?;
    let self_obj = args
        .first()
        .cloned()
        .ok_or_else(|| type_error("executescript() requires a Connection"))?;
    let cur = conn_cursor(std::slice::from_ref(&self_obj), &[])?;
    let executescript = ip.load_attr_public(&cur, "executescript")?;
    call(ip, &executescript, &args[1..])?;
    Ok(cur)
}

// ---------------------------------------------------------------
// Transactions
// ---------------------------------------------------------------

fn conn_commit(args: &[Object]) -> Result<Object, RuntimeError> {
    let state = checked_conn(args.first().unwrap_or(&Object::None))?;
    let (db, autocommit) = {
        let s = state.borrow();
        (s.db_ptr(), s.autocommit)
    };
    if autocommit == LEGACY_TRANSACTION_CONTROL {
        // SAFETY: live db handle.
        if unsafe { ffi::sqlite3_get_autocommit(db) } == 0 {
            exec_simple(db, "COMMIT")?;
        }
    } else if autocommit == 0 {
        exec_simple(db, "COMMIT")?;
        exec_simple(db, "BEGIN")?;
    }
    Ok(Object::None)
}

fn conn_rollback(args: &[Object]) -> Result<Object, RuntimeError> {
    let state = checked_conn(args.first().unwrap_or(&Object::None))?;
    let (db, autocommit) = {
        let s = state.borrow();
        (s.db_ptr(), s.autocommit)
    };
    if autocommit == LEGACY_TRANSACTION_CONTROL {
        // SAFETY: live db handle.
        if unsafe { ffi::sqlite3_get_autocommit(db) } == 0 {
            exec_simple(db, "ROLLBACK")?;
        }
    } else if autocommit == 0 {
        exec_simple(db, "ROLLBACK")?;
        exec_simple(db, "BEGIN")?;
    }
    Ok(Object::None)
}

/// `Connection.iterdump(*, filter=None)` — delegates to the pure-Python
/// `sqlite3.dump._iterdump`, exactly like connection.c's
/// `pysqlite_connection_iterdump_impl` imports and calls it.
fn conn_iterdump(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let ip = interp()?;
    let self_obj = args.first().cloned().unwrap_or(Object::None);
    checked_conn(&self_obj)?;
    if args.len() > 1 {
        return Err(type_error("iterdump() takes no positional arguments"));
    }
    let iterdump = ip.module_attr("sqlite3.dump", "_iterdump").ok_or_else(|| {
        raise(
            operational_error_class(),
            "Failed to obtain _iterdump() reference",
        )
    })?;
    let mut kw: Vec<(String, Object)> = Vec::new();
    if let Some(f) = kwarg(kwargs, "filter") {
        kw.push(("filter".to_owned(), f.clone()));
    }
    super::call_kw(ip, &iterdump, &[self_obj], &kw)
}

/// `Connection.__call__(sql, /)` — prepares a statement (CPython returns
/// the internal `Statement` object; nothing in the public API consumes
/// it, but the closed/thread checks and prepare errors must fire).
fn conn_call(args: &[Object]) -> Result<Object, RuntimeError> {
    let self_obj = args.first().cloned().unwrap_or(Object::None);
    let state = checked_conn(&self_obj)?;
    if args.len() != 2 {
        return Err(type_error(format!(
            "Connection.__call__() takes exactly 1 argument ({} given)",
            args.len().saturating_sub(1)
        )));
    }
    let sql = super::stmt::require_sql_str(args.get(1))?;
    let db = state.borrow().db_ptr();
    let mut stmt = super::stmt::Statement::prepare(db, &sql)?;
    stmt.finalize();
    // A stand-in for the opaque Statement heap type.
    let cls = statement_class();
    Ok(Object::Instance(Rc::new(crate::types::PyInstance::new(
        cls,
    ))))
}

fn statement_class() -> Rc<TypeObject> {
    static CELL: std::sync::OnceLock<Rc<TypeObject>> = std::sync::OnceLock::new();
    CELL.get_or_init(|| {
        let bt = crate::builtin_types::builtin_types();
        let mut dict = DictData::default();
        dict.insert(
            DictKey(Object::from_static("__module__")),
            Object::from_static("sqlite3"),
        );
        // Py_TPFLAGS_DISALLOW_INSTANTIATION: both tp slots reject, so
        // `tp(...)` *and* the direct `tp.__new__(tp)` escape hatch fail.
        for slot in ["__init__", "__new__"] {
            dict.insert(
                DictKey(Object::from_static(slot)),
                super::method(slot, |_args| {
                    Err(type_error("cannot create 'sqlite3.Statement' instances"))
                }),
            );
        }
        TypeObject::new_user("Statement", vec![bt.object_.clone()], dict)
            .expect("Statement class must linearise")
    })
    .clone()
}

fn conn_enter(args: &[Object]) -> Result<Object, RuntimeError> {
    let obj = args.first().cloned().unwrap_or(Object::None);
    checked_conn(&obj)?;
    Ok(obj)
}

fn conn_exit(args: &[Object]) -> Result<Object, RuntimeError> {
    let exc_type = args.get(1).cloned().unwrap_or(Object::None);
    if matches!(exc_type, Object::None) {
        conn_commit(&args[..1])?;
    } else {
        conn_rollback(&args[..1])?;
    }
    Ok(Object::Bool(false))
}

// ---------------------------------------------------------------
// Misc methods
// ---------------------------------------------------------------

fn conn_interrupt(args: &[Object]) -> Result<Object, RuntimeError> {
    let state = conn_state_of(args.first().unwrap_or(&Object::None))?;
    let s = state.borrow();
    if s.db == 0 {
        return Err(raise(
            programming_error_class(),
            "Cannot operate on a closed database.",
        ));
    }
    // SAFETY: live db handle; interrupt is safe from any thread.
    unsafe { ffi::sqlite3_interrupt(s.db_ptr()) };
    Ok(Object::None)
}

fn conn_getlimit(args: &[Object]) -> Result<Object, RuntimeError> {
    let state = checked_conn(args.first().unwrap_or(&Object::None))?;
    let category = args
        .get(1)
        .and_then(Object::as_i64)
        .ok_or_else(|| type_error("getlimit() requires an int category"))?;
    let db = state.borrow().db_ptr();
    check_limit_category(category)?;
    // SAFETY: live db handle; -1 queries without changing.
    let v = unsafe { ffi::sqlite3_limit(db, category as c_int, -1) };
    Ok(Object::Int(i64::from(v)))
}

fn conn_setlimit(args: &[Object]) -> Result<Object, RuntimeError> {
    let state = checked_conn(args.first().unwrap_or(&Object::None))?;
    let category = args
        .get(1)
        .and_then(Object::as_i64)
        .ok_or_else(|| type_error("setlimit() requires an int category"))?;
    let limit = args
        .get(2)
        .and_then(Object::as_i64)
        .ok_or_else(|| type_error("setlimit() requires an int limit"))?;
    check_limit_category(category)?;
    let db = state.borrow().db_ptr();
    // SAFETY: live db handle.
    let prev = unsafe { ffi::sqlite3_limit(db, category as c_int, limit as c_int) };
    Ok(Object::Int(i64::from(prev)))
}

fn check_limit_category(category: i64) -> Result<(), RuntimeError> {
    if !(0..=11).contains(&category) {
        return Err(raise(
            programming_error_class(),
            format!("'category' is out of bounds ({category})"),
        ));
    }
    Ok(())
}

/// `setting_is_valid` in connection.c: only the boolean dbconfig ops
/// CPython exposes are accepted; everything else is a ValueError.
fn check_dbconfig_op(op: i64) -> Result<c_int, RuntimeError> {
    if super::DBCONFIG_CODES
        .iter()
        .any(|(_, v)| i64::from(*v) == op)
    {
        Ok(op as c_int)
    } else {
        Err(value_error(format!("unknown config 'op': {op}")))
    }
}

fn conn_getconfig(args: &[Object]) -> Result<Object, RuntimeError> {
    let state = checked_conn(args.first().unwrap_or(&Object::None))?;
    let op = args
        .get(1)
        .and_then(Object::as_i64)
        .ok_or_else(|| type_error("getconfig() requires an int op"))?;
    let op = check_dbconfig_op(op)?;
    let db = state.borrow().db_ptr();
    let mut current: c_int = 0;
    // SAFETY: live db handle; the (op, -1, &out) variadic form queries a
    // boolean dbconfig without changing it.
    let rc = unsafe { ffi::sqlite3_db_config(db, op, -1_i32, &mut current) };
    if rc != ffi::SQLITE_OK {
        return Err(raise_sqlite_error(db, rc));
    }
    Ok(Object::Bool(current != 0))
}

fn conn_setconfig(args: &[Object]) -> Result<Object, RuntimeError> {
    let state = checked_conn(args.first().unwrap_or(&Object::None))?;
    let op = args
        .get(1)
        .and_then(Object::as_i64)
        .ok_or_else(|| type_error("setconfig() requires an int op"))?;
    let op = check_dbconfig_op(op)?;
    let enable = args
        .get(2)
        .map(|o| !matches!(o, Object::Bool(false) | Object::Int(0)))
        .unwrap_or(true);
    let db = state.borrow().db_ptr();
    let mut out: c_int = 0;
    // SAFETY: live db handle; boolean dbconfig form.
    let rc = unsafe { ffi::sqlite3_db_config(db, op, c_int::from(enable), &mut out) };
    if rc != ffi::SQLITE_OK {
        return Err(raise_sqlite_error(db, rc));
    }
    Ok(Object::None)
}

// ---------------------------------------------------------------
// backup / serialize / deserialize / blobopen
// ---------------------------------------------------------------

fn conn_backup(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let ip = interp()?;
    let state = checked_conn(args.first().unwrap_or(&Object::None))?;
    if args.len() > 2 {
        return Err(type_error(format!(
            "backup() takes at most 1 positional argument ({} given)",
            args.len() - 1
        )));
    }
    let target = args
        .get(1)
        .or_else(|| kwarg(kwargs, "target"))
        .cloned()
        .ok_or_else(|| type_error("backup() missing required argument 'target'"))?;
    let target_state = checked_conn(&target)?;
    if Rc::ptr_eq(&state, &target_state) {
        return Err(value_error("target cannot be the same connection instance"));
    }
    let pages = kwarg(kwargs, "pages")
        .and_then(Object::as_i64)
        .unwrap_or(-1);
    let pages = if pages == 0 { -1 } else { pages };
    let progress = kwarg(kwargs, "progress").cloned().unwrap_or(Object::None);
    if !matches!(progress, Object::None) && !is_callable(&progress) {
        return Err(type_error("progress argument must be a callable"));
    }
    let name = match kwarg(kwargs, "name") {
        Some(Object::Str(s)) => s.to_string(),
        None => "main".to_owned(),
        Some(other) => {
            return Err(type_error(format!(
                "backup() argument 'name' must be str, not {}",
                other.type_name_owned()
            )))
        }
    };
    let sleep = kwarg(kwargs, "sleep")
        .and_then(Object::as_f64)
        .unwrap_or(0.250);

    let src_db = state.borrow().db_ptr();
    let dst_db = target_state.borrow().db_ptr();
    let c_name = std::ffi::CString::new(name).map_err(|_| value_error("embedded null byte"))?;
    let c_main = std::ffi::CString::new("main").expect("static");
    // SAFETY: both handles are live; names are valid C strings.
    let bck = unsafe { ffi::sqlite3_backup_init(dst_db, c_main.as_ptr(), src_db, c_name.as_ptr()) };
    if bck.is_null() {
        // SAFETY: error state is on the destination handle.
        let rc = unsafe { ffi::sqlite3_errcode(dst_db) };
        return Err(raise_sqlite_error(dst_db, rc));
    }
    loop {
        // SAFETY: `bck` is live until backup_finish below.
        let rc = unsafe { ffi::sqlite3_backup_step(bck, pages as c_int) };
        if !matches!(progress, Object::None) {
            // SAFETY: as above.
            let remaining = unsafe { ffi::sqlite3_backup_remaining(bck) };
            // SAFETY: as above.
            let pagecount = unsafe { ffi::sqlite3_backup_pagecount(bck) };
            let res = call(
                ip,
                &progress,
                &[
                    Object::Int(i64::from(rc)),
                    Object::Int(i64::from(remaining)),
                    Object::Int(i64::from(pagecount)),
                ],
            );
            if let Err(e) = res {
                // SAFETY: finish releases the backup object exactly once.
                unsafe { ffi::sqlite3_backup_finish(bck) };
                return Err(e);
            }
        }
        match rc {
            ffi::SQLITE_DONE => break,
            ffi::SQLITE_OK | ffi::SQLITE_BUSY | ffi::SQLITE_LOCKED => {
                if sleep > 0.0 {
                    std::thread::sleep(std::time::Duration::from_secs_f64(sleep));
                }
            }
            _ => {
                // SAFETY: as above.
                unsafe { ffi::sqlite3_backup_finish(bck) };
                return Err(raise_sqlite_error(dst_db, rc));
            }
        }
    }
    // SAFETY: as above.
    let rc = unsafe { ffi::sqlite3_backup_finish(bck) };
    if rc != ffi::SQLITE_OK {
        return Err(raise_sqlite_error(dst_db, rc));
    }
    Ok(Object::None)
}

fn conn_serialize(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let state = checked_conn(args.first().unwrap_or(&Object::None))?;
    let name = match kwarg(kwargs, "name") {
        Some(Object::Str(s)) => s.to_string(),
        _ => "main".to_owned(),
    };
    let db = state.borrow().db_ptr();
    let c_name = std::ffi::CString::new(name).map_err(|_| value_error("embedded null byte"))?;
    let mut size: ffi::sqlite3_int64 = 0;
    // SAFETY: live handle; sqlite allocates the returned buffer, which
    // we copy and free immediately.
    let ptr = unsafe { ffi::sqlite3_serialize(db, c_name.as_ptr(), &raw mut size, 0) };
    if ptr.is_null() {
        return Err(raise(
            operational_error_class(),
            "unable to serialize database",
        ));
    }
    // SAFETY: `ptr` points to `size` valid bytes per the API contract.
    let data = unsafe { std::slice::from_raw_parts(ptr, size as usize).to_vec() };
    // SAFETY: buffer was allocated by sqlite3_malloc inside serialize.
    unsafe { ffi::sqlite3_free(ptr.cast::<std::os::raw::c_void>()) };
    Ok(Object::new_bytes(data))
}

fn conn_deserialize(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let state = checked_conn(args.first().unwrap_or(&Object::None))?;
    let data = match args
        .get(1)
        .or_else(|| kwarg(kwargs, "data"))
        .and_then(super::buffer_bytes)
    {
        Some(bytes) => bytes?,
        None => {
            return Err(type_error(
                "deserialize() argument 'data' must be bytes-like",
            ))
        }
    };
    let name = match kwarg(kwargs, "name") {
        Some(Object::Str(s)) => s.to_string(),
        _ => "main".to_owned(),
    };
    let db = state.borrow().db_ptr();
    let c_name = std::ffi::CString::new(name).map_err(|_| value_error("embedded null byte"))?;
    let len = data.len() as ffi::sqlite3_int64;
    // SAFETY: we hand sqlite an owned sqlite3_malloc'd copy with
    // FREEONCLOSE|RESIZEABLE so it manages the lifetime from here.
    unsafe {
        let buf = ffi::sqlite3_malloc64(data.len() as u64).cast::<u8>();
        if buf.is_null() && !data.is_empty() {
            return Err(RuntimeError::PyException(
                crate::error::PyException::from_builtin("MemoryError", ""),
            ));
        }
        std::ptr::copy_nonoverlapping(data.as_ptr(), buf, data.len());
        let rc = ffi::sqlite3_deserialize(
            db,
            c_name.as_ptr(),
            buf,
            len,
            len,
            (ffi::SQLITE_DESERIALIZE_FREEONCLOSE | ffi::SQLITE_DESERIALIZE_RESIZEABLE) as u32,
        );
        if rc != ffi::SQLITE_OK {
            return Err(raise_sqlite_error(db, rc));
        }
    }
    Ok(Object::None)
}

fn conn_blobopen(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let ip = interp()?;
    let self_obj = args.first().cloned().unwrap_or(Object::None);
    let state = checked_conn(&self_obj)?;
    if args.len() > 4 {
        return Err(type_error(format!(
            "blobopen() takes at most 3 positional arguments ({} given)",
            args.len() - 1
        )));
    }
    let table = match args.get(1).or_else(|| kwarg(kwargs, "table")) {
        Some(Object::Str(s)) => s.to_string(),
        _ => return Err(type_error("blobopen() argument 'table' must be str")),
    };
    let column = match args.get(2).or_else(|| kwarg(kwargs, "column")) {
        Some(Object::Str(s)) => s.to_string(),
        _ => return Err(type_error("blobopen() argument 'column' must be str")),
    };
    let rowid = args
        .get(3)
        .or_else(|| kwarg(kwargs, "row"))
        .and_then(Object::as_i64)
        .ok_or_else(|| type_error("blobopen() argument 'row' must be int"))?;
    let readonly = kwarg(kwargs, "readonly")
        .map(|o| !matches!(o, Object::Bool(false) | Object::Int(0)))
        .unwrap_or(false);
    let name = match kwarg(kwargs, "name") {
        Some(Object::Str(s)) => s.to_string(),
        _ => "main".to_owned(),
    };
    row::blob_open(
        ip, &self_obj, &state, &name, &table, &column, rowid, readonly,
    )
}

// ---------------------------------------------------------------
// getsets
// ---------------------------------------------------------------

fn get_isolation_level(args: &[Object]) -> Result<Object, RuntimeError> {
    let state = conn_state_of(args.first().unwrap_or(&Object::None))?;
    let s = state.borrow();
    Ok(match &s.isolation_level {
        None => Object::None,
        Some(l) => Object::from_str(l.clone()),
    })
}

fn set_isolation_level(args: &[Object]) -> Result<Object, RuntimeError> {
    let state = conn_state_of(args.first().unwrap_or(&Object::None))?;
    let value = args.get(1).cloned().unwrap_or(Object::None);
    let level = parse_isolation_level(&value)?;
    let db = {
        let mut s = state.borrow_mut();
        s.isolation_level = level.clone();
        s.db_ptr()
    };
    // Setting isolation_level to None commits any pending transaction
    // (CPython behavior).
    if level.is_none() && !db.is_null() {
        // SAFETY: live db handle.
        if unsafe { ffi::sqlite3_get_autocommit(db) } == 0 {
            exec_simple(db, "COMMIT")?;
        }
    }
    Ok(Object::None)
}

fn get_autocommit(args: &[Object]) -> Result<Object, RuntimeError> {
    let state = conn_state_of(args.first().unwrap_or(&Object::None))?;
    let v = state.borrow().autocommit;
    Ok(match v {
        1 => Object::Bool(true),
        0 => Object::Bool(false),
        _ => Object::Int(LEGACY_TRANSACTION_CONTROL),
    })
}

fn set_autocommit(args: &[Object]) -> Result<Object, RuntimeError> {
    let state = checked_conn(args.first().unwrap_or(&Object::None))?;
    let value = args.get(1).cloned().unwrap_or(Object::None);
    let new = parse_autocommit(&value)?;
    let (db, old) = {
        let s = state.borrow();
        (s.db_ptr(), s.autocommit)
    };
    if old == new {
        return Ok(Object::None);
    }
    // Transitions per connection.c `set_autocommit`:
    // -> False: enter manual mode, beginning a transaction if needed.
    // -> True/LEGACY: commit any open transaction first.
    if new == 0 {
        // SAFETY: live db handle.
        if unsafe { ffi::sqlite3_get_autocommit(db) } != 0 {
            exec_simple(db, "BEGIN")?;
        }
    } else {
        // SAFETY: live db handle.
        if unsafe { ffi::sqlite3_get_autocommit(db) } == 0 {
            exec_simple(db, "COMMIT")?;
        }
    }
    state.borrow_mut().autocommit = new;
    Ok(Object::None)
}

fn get_in_transaction(args: &[Object]) -> Result<Object, RuntimeError> {
    let state = conn_state_of(args.first().unwrap_or(&Object::None))?;
    let s = state.borrow();
    if s.db == 0 {
        return Ok(Object::Bool(false));
    }
    // SAFETY: live db handle.
    Ok(Object::Bool(
        unsafe { ffi::sqlite3_get_autocommit(s.db_ptr()) } == 0,
    ))
}

fn get_total_changes(args: &[Object]) -> Result<Object, RuntimeError> {
    let state = checked_conn(args.first().unwrap_or(&Object::None))?;
    let db = state.borrow().db_ptr();
    // SAFETY: live db handle.
    Ok(Object::Int(unsafe { ffi::sqlite3_total_changes64(db) }))
}

fn get_row_factory(args: &[Object]) -> Result<Object, RuntimeError> {
    let state = conn_state_of(args.first().unwrap_or(&Object::None))?;
    let v = state.borrow().row_factory.clone();
    Ok(v)
}

fn set_row_factory(args: &[Object]) -> Result<Object, RuntimeError> {
    let state = conn_state_of(args.first().unwrap_or(&Object::None))?;
    state.borrow_mut().row_factory = args.get(1).cloned().unwrap_or(Object::None);
    Ok(Object::None)
}

fn get_text_factory(args: &[Object]) -> Result<Object, RuntimeError> {
    let state = conn_state_of(args.first().unwrap_or(&Object::None))?;
    let v = state.borrow().text_factory.clone();
    Ok(v)
}

fn set_text_factory(args: &[Object]) -> Result<Object, RuntimeError> {
    let state = conn_state_of(args.first().unwrap_or(&Object::None))?;
    state.borrow_mut().text_factory = args.get(1).cloned().unwrap_or(Object::None);
    Ok(Object::None)
}

//! The `Cursor` heap type (`cursor.c`).

use std::collections::HashMap;

use rusqlite::ffi;

use super::stmt::{
    bind_parameters, build_description, cache_put, cache_take, exec_simple, fetch_row,
    maybe_begin_transaction, require_sql_str, resolve_converters, Statement,
};
use super::{
    call, checked_conn, install_getset, interp, method, method_kw, next_handle,
    programming_error_class, raise, raise_sqlite_error, row, self_instance, ConnState, Interp,
    LEGACY_TRANSACTION_CONTROL,
};
use crate::error::{type_error, value_error, RuntimeError};
use crate::object::{DictData, DictKey, Object};
use crate::sync::Rc;
use crate::sync::RefCell;
use crate::types::{PyInstance, TypeObject};

pub(crate) const CURSOR_HANDLE_KEY: &str = "_wp_sqlite3_cursor";

pub(crate) struct CursorState {
    /// The owning Connection instance (kept for `.connection` and for
    /// resolving ConnState lazily).
    pub connection: Object,
    /// Live statement being iterated, if any.
    pub stmt: Option<Statement>,
    /// Whether the live statement came from the per-connection cache.
    pub from_cache: bool,
    /// True when the live statement is positioned on an unread row.
    /// Conversion (converters, text factory, row factory) happens
    /// lazily at fetch time, like CPython's `cursor_iternext` — the
    /// eager part of execute is only the first `sqlite3_step`.
    pub has_row: bool,
    pub description: Object,
    pub converters: Vec<Option<Object>>,
    pub rowcount: i64,
    pub lastrowid: Object,
    pub arraysize: i64,
    pub row_factory: Object,
    pub closed: bool,
    /// CPython's `self->locked`: set for the duration of execute/fetch so
    /// Python re-entry (converters, row factories, callbacks) touching
    /// this cursor raises "Recursive use of cursors not allowed."
    pub locked: bool,
}

/// RAII guard for [`CursorState::locked`], acquired at the public
/// execute/fetch entry points (`pysqlite_check_cursor` + `self->locked`).
struct CursorLock(Rc<RefCell<CursorState>>);

impl CursorLock {
    fn acquire(st: &Rc<RefCell<CursorState>>) -> Result<Self, RuntimeError> {
        let mut s = st.borrow_mut();
        if s.locked {
            return Err(raise(
                programming_error_class(),
                "Recursive use of cursors not allowed.",
            ));
        }
        s.locked = true;
        Ok(CursorLock(st.clone()))
    }
}

impl Drop for CursorLock {
    fn drop(&mut self) {
        self.0.borrow_mut().locked = false;
    }
}

pub(crate) fn cursor_registry(
) -> &'static parking_lot::Mutex<HashMap<i64, Rc<RefCell<CursorState>>>> {
    static REG: std::sync::OnceLock<parking_lot::Mutex<HashMap<i64, Rc<RefCell<CursorState>>>>> =
        std::sync::OnceLock::new();
    REG.get_or_init(|| parking_lot::Mutex::new(HashMap::new()))
}

pub(crate) fn cursor_class() -> Rc<TypeObject> {
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
            method("__init__", cur_init),
        );
        dict.insert(
            DictKey(Object::from_static("execute")),
            method("execute", cur_execute),
        );
        dict.insert(
            DictKey(Object::from_static("executemany")),
            method("executemany", cur_executemany),
        );
        dict.insert(
            DictKey(Object::from_static("executescript")),
            method("executescript", cur_executescript),
        );
        dict.insert(
            DictKey(Object::from_static("fetchone")),
            method("fetchone", cur_fetchone),
        );
        dict.insert(
            DictKey(Object::from_static("fetchmany")),
            method_kw("fetchmany", cur_fetchmany),
        );
        dict.insert(
            DictKey(Object::from_static("fetchall")),
            method("fetchall", cur_fetchall),
        );
        dict.insert(
            DictKey(Object::from_static("close")),
            method("close", cur_close),
        );
        dict.insert(
            DictKey(Object::from_static("setinputsizes")),
            method("setinputsizes", cur_noop),
        );
        dict.insert(
            DictKey(Object::from_static("setoutputsize")),
            method("setoutputsize", cur_noop),
        );
        dict.insert(
            DictKey(Object::from_static("__iter__")),
            method("__iter__", cur_iter),
        );
        dict.insert(
            DictKey(Object::from_static("__next__")),
            method("__next__", cur_next),
        );
        dict.insert(
            DictKey(Object::from_static("__del__")),
            method("__del__", cur_del),
        );
        let cls = TypeObject::new_user("Cursor", vec![bt.object_.clone()], dict)
            .expect("Cursor class must linearise");
        install_getset(&cls, "description", get_description, None);
        install_getset(&cls, "rowcount", get_rowcount, None);
        install_getset(&cls, "lastrowid", get_lastrowid, None);
        install_getset(&cls, "arraysize", get_arraysize, Some(set_arraysize));
        install_getset(&cls, "connection", get_connection, None);
        install_getset(&cls, "row_factory", get_row_factory, Some(set_row_factory));
        cls
    })
    .clone()
}

// ---------------------------------------------------------------
// state plumbing
// ---------------------------------------------------------------

fn state_of(obj: &Object) -> Result<Rc<RefCell<CursorState>>, RuntimeError> {
    let Object::Instance(inst) = obj else {
        return Err(type_error("expected a sqlite3.Cursor instance"));
    };
    let handle = inst
        .dict
        .borrow()
        .get(&DictKey(Object::from_static(CURSOR_HANDLE_KEY)))
        .cloned();
    match handle {
        Some(Object::Int(h)) => cursor_registry().lock().get(&h).cloned().ok_or_else(|| {
            raise(
                programming_error_class(),
                "Cannot operate on a closed cursor.",
            )
        }),
        _ => Err(raise(
            programming_error_class(),
            "Base Cursor.__init__ not called.",
        )),
    }
}

/// Open-and-usable check (`pysqlite_check_cursor`).
fn checked_cursor(
    obj: &Object,
) -> Result<(Rc<RefCell<CursorState>>, Rc<RefCell<ConnState>>), RuntimeError> {
    let st = state_of(obj)?;
    if st.borrow().closed {
        return Err(raise(
            programming_error_class(),
            "Cannot operate on a closed cursor.",
        ));
    }
    let conn_obj = st.borrow().connection.clone();
    let conn = checked_conn(&conn_obj)?;
    Ok((st, conn))
}

fn cur_init(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = self_instance(args)?;
    // Re-init while the cursor is mid-fetch (a converter calling
    // `cur.__init__(con)`) hits CPython's `self->locked` gate.
    {
        let handle = inst
            .dict
            .borrow()
            .get(&DictKey(Object::from_static(CURSOR_HANDLE_KEY)))
            .cloned();
        if let Some(Object::Int(h)) = handle {
            if let Some(prev) = cursor_registry().lock().get(&h).cloned() {
                if prev.borrow().locked {
                    return Err(raise(
                        programming_error_class(),
                        "Recursive use of cursors not allowed.",
                    ));
                }
            }
        }
    }
    let conn_obj = args
        .get(1)
        .cloned()
        .ok_or_else(|| type_error("Cursor() missing required argument 'connection'"))?;
    // A real Connection (sub)instance is required — a fake `__class__`
    // attribute doesn't count (CPython uses PyObject_TypeCheck).
    let is_conn = matches!(
        &conn_obj,
        Object::Instance(i)
            if i.cls().is_subclass_of(&super::connection::connection_class())
    );
    if !is_conn {
        return Err(type_error(format!(
            "argument 1 must be sqlite3.Connection, not {}",
            conn_obj.type_name_owned()
        )));
    }
    // The connection must be open (and same-thread).
    checked_conn(&conn_obj)?;
    // The row factory is snapshotted from the connection at cursor
    // creation time (cursor.c `pysqlite_cursor_init`).
    let row_factory = super::conn_state_of(&conn_obj)?
        .borrow()
        .row_factory
        .clone();
    let state = Rc::new(RefCell::new(CursorState {
        connection: conn_obj,
        stmt: None,
        from_cache: false,
        has_row: false,
        description: Object::None,
        converters: Vec::new(),
        rowcount: -1,
        lastrowid: Object::None,
        arraysize: 1,
        row_factory,
        closed: false,
        locked: false,
    }));
    let handle = next_handle();
    cursor_registry().lock().insert(handle, state);
    inst.dict.borrow_mut().insert(
        DictKey(Object::from_static(CURSOR_HANDLE_KEY)),
        Object::Int(handle),
    );
    Ok(Object::None)
}

// ---------------------------------------------------------------
// execute machinery
// ---------------------------------------------------------------

/// Release the cursor's live statement back to the connection cache
/// (or finalize an uncached one).
fn release_statement(st: &Rc<RefCell<CursorState>>, conn: &Rc<RefCell<ConnState>>) {
    let (stmt, from_cache) = {
        let mut s = st.borrow_mut();
        s.has_row = false;
        (s.stmt.take(), s.from_cache)
    };
    if let Some(mut stmt) = stmt {
        if from_cache {
            cache_put(conn, stmt);
        } else {
            stmt.finalize();
        }
    }
}

/// Wrap a raw value tuple through the cursor's row factory (snapshotted
/// from the connection at cursor init).
fn apply_row_factory(
    ip: &mut Interp,
    st: &Rc<RefCell<CursorState>>,
    cursor_obj: &Object,
    values: Vec<Object>,
) -> Result<Object, RuntimeError> {
    let tuple = Object::new_tuple(values);
    let factory = { st.borrow().row_factory.clone() };
    match &factory {
        Object::None => Ok(tuple),
        Object::Type(t) if Rc::ptr_eq(t, &row::row_class()) => {
            Ok(row::make_row(cursor_obj, st, tuple))
        }
        _ => call(ip, &factory, &[cursor_obj.clone(), tuple]),
    }
}

fn execute_one(
    ip: &mut Interp,
    _cursor_obj: &Object,
    st: &Rc<RefCell<CursorState>>,
    conn: &Rc<RefCell<ConnState>>,
    sql: &str,
    params: &Object,
    multiple_iteration: bool,
) -> Result<(), RuntimeError> {
    let db = conn.borrow().db_ptr();

    // Take from the statement cache or prepare fresh.
    let (stmt, from_cache) = match cache_take(conn, sql) {
        Some(s) => (s, true),
        None => (Statement::prepare(db, sql)?, false),
    };
    if stmt.ptr == 0 {
        // Comment/whitespace-only query: nothing to do.
        let mut s = st.borrow_mut();
        s.description = Object::None;
        s.rowcount = -1;
        return Ok(());
    }

    if multiple_iteration {
        // SAFETY: live statement.
        let readonly = unsafe { ffi::sqlite3_stmt_readonly(stmt.stmt()) } != 0;
        if readonly {
            let mut dead = stmt;
            if from_cache {
                cache_put(conn, dead);
            } else {
                dead.finalize();
            }
            return Err(raise(
                programming_error_class(),
                "executemany() can only execute DML statements.",
            ));
        }
    }

    maybe_begin_transaction(conn, &stmt)?;

    if let Err(e) = bind_parameters(ip, db, &stmt, params) {
        let mut dead = stmt;
        if from_cache {
            cache_put(conn, dead);
        } else {
            dead.finalize();
        }
        return Err(e);
    }

    let detect_types = conn.borrow().detect_types;
    let description = build_description(&stmt, detect_types);
    let converters = resolve_converters(&stmt, detect_types);

    // First step happens eagerly (CPython's `_pysqlite_query_execute`).
    let stepped = stmt.step(db);
    let has_row = match stepped {
        Ok(h) => h,
        Err(e) => {
            let mut dead = stmt;
            // Failed statements are not returned to the cache.
            dead.finalize();
            {
                let mut s = st.borrow_mut();
                s.description = Object::None;
                s.rowcount = -1;
            }
            return Err(e);
        }
    };

    {
        let mut s = st.borrow_mut();
        s.description = description;
        s.converters = converters;
        if stmt.is_dml {
            // SAFETY: live db handle.
            let changes = unsafe { ffi::sqlite3_changes64(db) };
            if multiple_iteration {
                s.rowcount = if s.rowcount < 0 {
                    changes
                } else {
                    s.rowcount + changes
                };
            } else {
                s.rowcount = changes;
                // lastrowid is refreshed only by a *successful* single
                // execute of a DML statement (cursor.c); executemany and
                // failed statements leave the previous value alone.
                // SAFETY: live db handle.
                let rowid = unsafe { ffi::sqlite3_last_insert_rowid(db) };
                s.lastrowid = Object::Int(rowid);
            }
        } else if !multiple_iteration {
            s.rowcount = -1;
        }
    }

    if has_row {
        // The row itself is converted lazily at fetch time (CPython's
        // `cursor_iternext`); keep the statement positioned on it.
        let mut s = st.borrow_mut();
        s.has_row = true;
        s.stmt = Some(stmt);
        s.from_cache = from_cache;
    } else {
        // Statement completed on the first step.
        let mut dead = stmt;
        if from_cache {
            cache_put(conn, dead);
        } else {
            dead.finalize();
        }
        let mut s = st.borrow_mut();
        s.has_row = false;
        s.stmt = None;
    }
    Ok(())
}

fn cur_execute(args: &[Object]) -> Result<Object, RuntimeError> {
    let ip = interp()?;
    let self_obj = args.first().cloned().unwrap_or(Object::None);
    let (st, conn) = checked_cursor(&self_obj)?;
    let sql = require_sql_str(args.get(1))?;
    let params = args.get(2).cloned().unwrap_or(Object::None);

    let _lock = CursorLock::acquire(&st)?;
    release_statement(&st, &conn);
    {
        let mut s = st.borrow_mut();
        s.rowcount = -1;
        s.description = Object::None;
    }
    execute_one(ip, &self_obj, &st, &conn, &sql, &params, false)?;
    Ok(self_obj)
}

fn cur_executemany(args: &[Object]) -> Result<Object, RuntimeError> {
    let ip = interp()?;
    let self_obj = args.first().cloned().unwrap_or(Object::None);
    let (st, conn) = checked_cursor(&self_obj)?;
    let sql = require_sql_str(args.get(1))?;
    let seq = args
        .get(2)
        .cloned()
        .ok_or_else(|| type_error("executemany() missing required argument (pos 2)"))?;

    let _lock = CursorLock::acquire(&st)?;
    release_statement(&st, &conn);
    {
        let mut s = st.borrow_mut();
        s.rowcount = -1;
        s.lastrowid = Object::None;
        s.description = Object::None;
    }

    // Materialise the parameter iterator through the VM (accepts
    // generators, iterators, sequences — CPython consumes an iterator).
    let list_fn = ip
        .builtins_dict()
        .borrow()
        .get(&DictKey(Object::from_static("list")))
        .cloned()
        .ok_or_else(|| RuntimeError::Internal("_sqlite3: no list builtin".into()))?;
    let items = match call(ip, &list_fn, std::slice::from_ref(&seq))? {
        Object::List(l) => l.borrow().clone(),
        _ => return Err(type_error("parameters are of unsupported type")),
    };
    for params in items {
        execute_one(ip, &self_obj, &st, &conn, &sql, &params, true)?;
        // Any rows produced by one iteration are discarded (CPython
        // steps row-producing statements to completion in executemany).
        loop {
            let live = { st.borrow().stmt.is_some() };
            if !live {
                break;
            }
            advance(ip, &self_obj, &st, &conn)?;
        }
    }
    Ok(self_obj)
}

fn cur_executescript(args: &[Object]) -> Result<Object, RuntimeError> {
    let self_obj = args.first().cloned().unwrap_or(Object::None);
    let (st, conn) = checked_cursor(&self_obj)?;
    let script = match args.get(1) {
        Some(other) => match super::as_text(other) {
            Some(text) => text?,
            None => {
                return Err(type_error(format!(
                    "executescript() argument must be str, not {}",
                    other.type_name_owned()
                )))
            }
        },
        None => return Err(type_error("executescript() missing required argument")),
    };
    release_statement(&st, &conn);

    let db = conn.borrow().db_ptr();
    // Implicit COMMIT of any pending transaction first (cursor.c).
    // SAFETY: live db handle.
    if unsafe { ffi::sqlite3_get_autocommit(db) } == 0
        && conn.borrow().autocommit == LEGACY_TRANSACTION_CONTROL
    {
        exec_simple(db, "COMMIT")?;
    }
    if script.contains('\0') {
        return Err(value_error("script argument must be unicode."));
    }
    super::stmt::check_sql_length(db, &script)?;
    let c = std::ffi::CString::new(script).map_err(|_| value_error("embedded null byte"))?;
    // SAFETY: live db handle; sqlite3_exec runs the whole script.
    let rc = unsafe {
        ffi::sqlite3_exec(
            db,
            c.as_ptr(),
            None,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    if rc != ffi::SQLITE_OK {
        return Err(raise_sqlite_error(db, rc));
    }
    Ok(self_obj)
}

// ---------------------------------------------------------------
// fetching
// ---------------------------------------------------------------

/// Fetch the row the statement is positioned on, then step once
/// (CPython's `cursor_iternext`).
///
/// The statement is *taken out* of the state cell around every
/// re-entry into Python (converters, row factories) so callbacks that
/// touch the cursor can't hit a RefCell borrow conflict.
fn advance(
    ip: &mut Interp,
    cursor_obj: &Object,
    st: &Rc<RefCell<CursorState>>,
    conn: &Rc<RefCell<ConnState>>,
) -> Result<Option<Object>, RuntimeError> {
    let (stmt, has_row) = {
        let mut s = st.borrow_mut();
        (s.stmt.take(), s.has_row)
    };
    let Some(stmt) = stmt else {
        return Ok(None);
    };
    if !has_row {
        st.borrow_mut().stmt = Some(stmt);
        release_statement(st, conn);
        return Ok(None);
    }
    // Convert the current row while the statement is out of the cell;
    // converters may close/reuse this cursor (the lock catches that).
    let converters = { st.borrow().converters.clone() };
    let values = match fetch_row(ip, conn, &stmt, &converters) {
        Ok(v) => v,
        Err(e) => {
            st.borrow_mut().stmt = Some(stmt);
            release_statement(st, conn);
            return Err(e);
        }
    };
    let row_obj = match apply_row_factory(ip, st, cursor_obj, values) {
        Ok(r) => r,
        Err(e) => {
            st.borrow_mut().stmt = Some(stmt);
            release_statement(st, conn);
            return Err(e);
        }
    };
    // Step to the next row (or completion).
    let db = conn.borrow().db_ptr();
    match stmt.step(db) {
        Ok(more) => {
            let mut s = st.borrow_mut();
            s.has_row = more;
            s.stmt = Some(stmt);
            if !more {
                drop(s);
                release_statement(st, conn);
            }
            Ok(Some(row_obj))
        }
        Err(e) => {
            st.borrow_mut().stmt = Some(stmt);
            release_statement(st, conn);
            Err(e)
        }
    }
}

fn cur_fetchone(args: &[Object]) -> Result<Object, RuntimeError> {
    let ip = interp()?;
    let self_obj = args.first().cloned().unwrap_or(Object::None);
    let (st, conn) = checked_cursor(&self_obj)?;
    let _lock = CursorLock::acquire(&st)?;
    match advance(ip, &self_obj, &st, &conn)? {
        Some(row) => Ok(row),
        None => Ok(Object::None),
    }
}

fn cur_fetchmany(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let ip = interp()?;
    let self_obj = args.first().cloned().unwrap_or(Object::None);
    let (st, conn) = checked_cursor(&self_obj)?;
    let size = match args.get(1).or_else(|| super::kwarg(kwargs, "size")) {
        Some(v) => parse_c_int(v, "size")?,
        None => st.borrow().arraysize,
    };
    let _lock = CursorLock::acquire(&st)?;
    let mut out = Vec::new();
    for _ in 0..size.max(0) {
        match advance(ip, &self_obj, &st, &conn)? {
            Some(row) => out.push(row),
            None => break,
        }
    }
    Ok(Object::new_list(out))
}

fn cur_fetchall(args: &[Object]) -> Result<Object, RuntimeError> {
    let ip = interp()?;
    let self_obj = args.first().cloned().unwrap_or(Object::None);
    let (st, conn) = checked_cursor(&self_obj)?;
    let _lock = CursorLock::acquire(&st)?;
    let mut out = Vec::new();
    while let Some(row) = advance(ip, &self_obj, &st, &conn)? {
        out.push(row);
    }
    Ok(Object::new_list(out))
}

fn cur_iter(args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(args.first().cloned().unwrap_or(Object::None))
}

fn cur_next(args: &[Object]) -> Result<Object, RuntimeError> {
    let ip = interp()?;
    let self_obj = args.first().cloned().unwrap_or(Object::None);
    let (st, conn) = checked_cursor(&self_obj)?;
    let _lock = CursorLock::acquire(&st)?;
    match advance(ip, &self_obj, &st, &conn)? {
        Some(row) => Ok(row),
        None => Err(RuntimeError::PyException(
            crate::error::PyException::from_builtin("StopIteration", ""),
        )),
    }
}

// ---------------------------------------------------------------
// close / __del__ / no-ops
// ---------------------------------------------------------------

fn cur_close(args: &[Object]) -> Result<Object, RuntimeError> {
    let self_obj = args.first().cloned().unwrap_or(Object::None);
    let st = state_of(&self_obj)?;
    if st.borrow().locked {
        return Err(raise(
            programming_error_class(),
            "Recursive use of cursors not allowed.",
        ));
    }
    // The connection must still be open and same-thread for close().
    let conn_obj = st.borrow().connection.clone();
    let conn = checked_conn(&conn_obj)?;
    release_statement(&st, &conn);
    st.borrow_mut().closed = true;
    Ok(Object::None)
}

fn cur_del(args: &[Object]) -> Result<Object, RuntimeError> {
    if let Some(obj) = args.first() {
        if let Ok(st) = state_of(obj) {
            // Finalize any live statement without touching the (possibly
            // closed) connection.
            let mut s = st.borrow_mut();
            if let Some(mut stmt) = s.stmt.take() {
                stmt.finalize();
            }
            s.closed = true;
            drop(s);
            if let Object::Instance(inst) = obj {
                let handle = inst
                    .dict
                    .borrow()
                    .get(&DictKey(Object::from_static(CURSOR_HANDLE_KEY)))
                    .cloned();
                if let Some(Object::Int(h)) = handle {
                    cursor_registry().lock().remove(&h);
                }
            }
        }
    }
    Ok(Object::None)
}

fn cur_noop(_args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(Object::None)
}

// ---------------------------------------------------------------
// getsets
// ---------------------------------------------------------------

fn get_description(args: &[Object]) -> Result<Object, RuntimeError> {
    let st = state_of(args.first().unwrap_or(&Object::None))?;
    let v = st.borrow().description.clone();
    Ok(v)
}

fn get_rowcount(args: &[Object]) -> Result<Object, RuntimeError> {
    let st = state_of(args.first().unwrap_or(&Object::None))?;
    let v = st.borrow().rowcount;
    Ok(Object::Int(v))
}

fn get_lastrowid(args: &[Object]) -> Result<Object, RuntimeError> {
    let st = state_of(args.first().unwrap_or(&Object::None))?;
    let v = st.borrow().lastrowid.clone();
    Ok(v)
}

fn get_arraysize(args: &[Object]) -> Result<Object, RuntimeError> {
    let st = state_of(args.first().unwrap_or(&Object::None))?;
    let v = st.borrow().arraysize;
    Ok(Object::Int(v))
}

fn set_arraysize(args: &[Object]) -> Result<Object, RuntimeError> {
    let st = state_of(args.first().unwrap_or(&Object::None))?;
    let v = parse_c_int(args.get(1).unwrap_or(&Object::None), "arraysize")?;
    st.borrow_mut().arraysize = v;
    Ok(Object::None)
}

/// A non-negative C `int` (the clinic converter shape shared by
/// `arraysize` and `fetchmany(size)`): non-ints are a TypeError,
/// negatives a ValueError, and values past INT_MAX an OverflowError.
fn parse_c_int(v: &Object, what: &str) -> Result<i64, RuntimeError> {
    let n = match v {
        Object::Bool(b) => i64::from(*b),
        Object::Int(i) => *i,
        Object::Long(b) => num_traits::ToPrimitive::to_i64(b.as_ref()).ok_or_else(|| {
            crate::error::overflow_error("Python int too large to convert to C int")
        })?,
        other => {
            return Err(type_error(format!(
                "'{}' object cannot be interpreted as an integer",
                other.type_name_owned()
            )))
        }
    };
    if n < 0 {
        return Err(value_error(format!("{what} must be positive")));
    }
    if n > i64::from(i32::MAX) {
        return Err(crate::error::overflow_error(
            "Python int too large to convert to C int",
        ));
    }
    Ok(n)
}

fn get_connection(args: &[Object]) -> Result<Object, RuntimeError> {
    let st = state_of(args.first().unwrap_or(&Object::None))?;
    let v = st.borrow().connection.clone();
    Ok(v)
}

fn get_row_factory(args: &[Object]) -> Result<Object, RuntimeError> {
    let st = state_of(args.first().unwrap_or(&Object::None))?;
    let v = st.borrow().row_factory.clone();
    Ok(v)
}

fn set_row_factory(args: &[Object]) -> Result<Object, RuntimeError> {
    let st = state_of(args.first().unwrap_or(&Object::None))?;
    st.borrow_mut().row_factory = args.get(1).cloned().unwrap_or(Object::None);
    Ok(Object::None)
}

// Keep clippy quiet about the unused PyInstance import if it ever goes.
#[allow(dead_code)]
fn _touch(_: &PyInstance) {}

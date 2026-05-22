//! `_sqlite3` — SQLite database access (RFC 0019).
//!
//! Backed by `rusqlite` (statically-linked SQLite). The frozen
//! `sqlite3.py` wrapper builds the DB-API 2.0 surface on top of this
//! thin core. We deliberately keep the surface tiny:
//!
//! * `connect(path)` returns a `Connection` value (a dict carrying
//!   built-in methods).
//! * `Connection.cursor()` returns a `Cursor` value.
//! * `Connection.executescript(sql)` runs an arbitrary SQL batch.
//! * `Connection.commit()` / `rollback()` / `close()`.
//! * `Cursor.execute(sql, params=None)` runs a single statement,
//!   storing rows for `fetch*`.
//! * `Cursor.executemany(sql, rows)` runs the same statement many
//!   times.
//! * `Cursor.fetchone() / fetchall() / fetchmany(n)` and the
//!   `description` / `rowcount` / `lastrowid` introspection getters.
//!
//! Higher-level ergonomics (row factories, isolation levels,
//! transaction-aware `__enter__`/`__exit__`) live in `sqlite3.py`.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::path::Path;
use std::rc::Rc;

use rusqlite::{params_from_iter, types::Value as SqlValue, Connection};

use crate::error::{type_error, value_error, RuntimeError};
use crate::import::ModuleCache;
use crate::object::{BuiltinFn, DictData, DictKey, Object, PyModule};

pub fn build(_cache: &ModuleCache) -> Rc<PyModule> {
    let dict = Rc::new(RefCell::new(DictData::new()));
    {
        let mut d = dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("__name__")),
            Object::from_static("_sqlite3"),
        );
        d.insert(
            DictKey(Object::from_static("__doc__")),
            Object::from_static("SQLite database access (RFC 0019 core)."),
        );
        register(&mut d, "connect", b_connect);
        d.insert(
            DictKey(Object::from_static("sqlite_version")),
            Object::from_static("3.x bundled"),
        );
        d.insert(
            DictKey(Object::from_static("sqlite_version_info")),
            Object::new_tuple(vec![Object::Int(3), Object::Int(0), Object::Int(0)]),
        );
        d.insert(
            DictKey(Object::from_static("apilevel")),
            Object::from_static("2.0"),
        );
        d.insert(DictKey(Object::from_static("threadsafety")), Object::Int(1));
        d.insert(
            DictKey(Object::from_static("paramstyle")),
            Object::from_static("qmark"),
        );
    }
    Rc::new(PyModule {
        name: "_sqlite3".to_owned(),
        filename: None,
        dict,
    })
}

fn register(
    d: &mut DictData,
    name: &'static str,
    body: impl Fn(&[Object]) -> Result<Object, RuntimeError> + 'static,
) {
    let bf = BuiltinFn {
        name,
        call: Box::new(body),
    };
    d.insert(
        DictKey(Object::from_static(name)),
        Object::Builtin(Rc::new(bf)),
    );
}

struct Conn {
    inner: RefCell<Option<Connection>>,
}

impl Conn {
    fn open(path: &str) -> Result<Rc<Self>, RuntimeError> {
        let c = if path == ":memory:" {
            Connection::open_in_memory()
        } else {
            Connection::open(Path::new(path))
        };
        let inner = c.map_err(|e| value_error(format!("could not open db: {e}")))?;
        Ok(Rc::new(Self {
            inner: RefCell::new(Some(inner)),
        }))
    }
}

fn b_connect(args: &[Object]) -> Result<Object, RuntimeError> {
    let path = match args.first() {
        Some(Object::Str(s)) => s.to_string(),
        Some(Object::Bytes(b)) => String::from_utf8_lossy(b).to_string(),
        _ => return Err(type_error("connect requires a path string")),
    };
    let conn = Conn::open(&path)?;
    Ok(make_connection_obj(conn))
}

fn make_connection_obj(conn: Rc<Conn>) -> Object {
    let dict = Rc::new(RefCell::new(DictData::new()));
    {
        let mut d = dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("__class__")),
            Object::from_static("Connection"),
        );

        let conn_for_cursor = conn.clone();
        d.insert(
            DictKey(Object::from_static("cursor")),
            builtin("cursor", move |_args: &[Object]| {
                Ok(make_cursor_obj(conn_for_cursor.clone()))
            }),
        );

        let conn_for_script = conn.clone();
        d.insert(
            DictKey(Object::from_static("executescript")),
            builtin("executescript", move |args: &[Object]| {
                let sql = match args.first() {
                    Some(Object::Str(s)) => s.to_string(),
                    _ => return Err(type_error("executescript requires SQL")),
                };
                let slot = conn_for_script.inner.borrow();
                let inner = slot
                    .as_ref()
                    .ok_or_else(|| value_error("connection closed"))?;
                inner
                    .execute_batch(&sql)
                    .map_err(|e| value_error(format!("sqlite: {e}")))?;
                Ok(Object::None)
            }),
        );

        let conn_for_commit = conn.clone();
        d.insert(
            DictKey(Object::from_static("commit")),
            builtin("commit", move |_args: &[Object]| {
                let slot = conn_for_commit.inner.borrow();
                let inner = slot
                    .as_ref()
                    .ok_or_else(|| value_error("connection closed"))?;
                let _ = inner.execute_batch("COMMIT");
                let _ = inner.execute_batch("BEGIN");
                Ok(Object::None)
            }),
        );

        let conn_for_rollback = conn.clone();
        d.insert(
            DictKey(Object::from_static("rollback")),
            builtin("rollback", move |_args: &[Object]| {
                let slot = conn_for_rollback.inner.borrow();
                let inner = slot
                    .as_ref()
                    .ok_or_else(|| value_error("connection closed"))?;
                let _ = inner.execute_batch("ROLLBACK");
                let _ = inner.execute_batch("BEGIN");
                Ok(Object::None)
            }),
        );

        let conn_for_close = conn.clone();
        d.insert(
            DictKey(Object::from_static("close")),
            builtin("close", move |_args: &[Object]| {
                let _ = conn_for_close.inner.borrow_mut().take();
                Ok(Object::None)
            }),
        );
    }
    Object::Dict(dict)
}

fn make_cursor_obj(conn: Rc<Conn>) -> Object {
    let dict = Rc::new(RefCell::new(DictData::new()));
    let rows: Rc<RefCell<VecDeque<Vec<Object>>>> = Rc::new(RefCell::new(VecDeque::new()));
    let description: Rc<RefCell<Object>> = Rc::new(RefCell::new(Object::None));
    let rowcount: Rc<Cell<i64>> = Rc::new(Cell::new(-1));
    let lastrowid: Rc<Cell<i64>> = Rc::new(Cell::new(0));
    {
        let mut d = dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("__class__")),
            Object::from_static("Cursor"),
        );

        let conn_e = conn.clone();
        let rows_e = rows.clone();
        let desc_e = description.clone();
        let rowcount_e = rowcount.clone();
        let lastrowid_e = lastrowid.clone();
        d.insert(
            DictKey(Object::from_static("execute")),
            builtin("execute", move |args: &[Object]| {
                run_execute(&conn_e, args, &rows_e, &desc_e, &rowcount_e, &lastrowid_e)?;
                Ok(Object::None)
            }),
        );

        let conn_em = conn.clone();
        let rows_em = rows.clone();
        let desc_em = description.clone();
        let rowcount_em = rowcount.clone();
        let lastrowid_em = lastrowid.clone();
        d.insert(
            DictKey(Object::from_static("executemany")),
            builtin("executemany", move |args: &[Object]| {
                run_executemany(
                    &conn_em,
                    args,
                    &rows_em,
                    &desc_em,
                    &rowcount_em,
                    &lastrowid_em,
                )?;
                Ok(Object::None)
            }),
        );

        let rows_one = rows.clone();
        d.insert(
            DictKey(Object::from_static("fetchone")),
            builtin("fetchone", move |_args: &[Object]| {
                let row = rows_one.borrow_mut().pop_front();
                Ok(match row {
                    Some(r) => Object::new_tuple(r),
                    None => Object::None,
                })
            }),
        );

        let rows_all = rows.clone();
        d.insert(
            DictKey(Object::from_static("fetchall")),
            builtin("fetchall", move |_args: &[Object]| {
                let rows: Vec<Object> = rows_all
                    .borrow_mut()
                    .drain(..)
                    .map(Object::new_tuple)
                    .collect();
                Ok(Object::new_list(rows))
            }),
        );

        let rows_many = rows.clone();
        d.insert(
            DictKey(Object::from_static("fetchmany")),
            builtin("fetchmany", move |args: &[Object]| {
                let n = match args.first() {
                    Some(Object::Int(i)) => *i as usize,
                    _ => 1,
                };
                let mut buf = rows_many.borrow_mut();
                let mut out = Vec::with_capacity(n.min(buf.len()));
                for _ in 0..n {
                    match buf.pop_front() {
                        Some(r) => out.push(Object::new_tuple(r)),
                        None => break,
                    }
                }
                Ok(Object::new_list(out))
            }),
        );

        d.insert(
            DictKey(Object::from_static("close")),
            builtin("close", move |_args: &[Object]| Ok(Object::None)),
        );

        let desc_ref = description.clone();
        d.insert(
            DictKey(Object::from_static("get_description")),
            builtin("get_description", move |_args: &[Object]| {
                Ok(desc_ref.borrow().clone())
            }),
        );
        let rc_ref = rowcount.clone();
        d.insert(
            DictKey(Object::from_static("get_rowcount")),
            builtin("get_rowcount", move |_args: &[Object]| {
                Ok(Object::Int(rc_ref.get()))
            }),
        );
        let lr_ref = lastrowid.clone();
        d.insert(
            DictKey(Object::from_static("get_lastrowid")),
            builtin("get_lastrowid", move |_args: &[Object]| {
                Ok(Object::Int(lr_ref.get()))
            }),
        );
    }
    Object::Dict(dict)
}

fn run_execute(
    conn: &Rc<Conn>,
    args: &[Object],
    rows: &Rc<RefCell<VecDeque<Vec<Object>>>>,
    description: &Rc<RefCell<Object>>,
    rowcount: &Rc<Cell<i64>>,
    lastrowid: &Rc<Cell<i64>>,
) -> Result<(), RuntimeError> {
    let sql = match args.first() {
        Some(Object::Str(s)) => s.to_string(),
        _ => return Err(type_error("execute requires SQL string")),
    };
    let params = args.get(1).cloned().unwrap_or(Object::None);
    let slot = conn.inner.borrow();
    let inner = slot
        .as_ref()
        .ok_or_else(|| value_error("connection closed"))?;
    let mut stmt = inner
        .prepare(&sql)
        .map_err(|e| value_error(format!("sqlite: {e}")))?;
    let bound = bind_params(&params)?;
    rows.borrow_mut().clear();
    *description.borrow_mut() = Object::None;
    let col_count = stmt.column_count();
    if col_count > 0 {
        let cols = column_descriptions(&stmt);
        *description.borrow_mut() = cols;
        let mut q = stmt
            .query(params_from_iter(bound.iter()))
            .map_err(|e| value_error(format!("sqlite: {e}")))?;
        while let Some(row) = q.next().map_err(|e| value_error(format!("sqlite: {e}")))? {
            let mut tup = Vec::with_capacity(col_count);
            for i in 0..col_count {
                let v: SqlValue = row
                    .get(i)
                    .map_err(|e| value_error(format!("sqlite: {e}")))?;
                tup.push(sql_to_object(v));
            }
            rows.borrow_mut().push_back(tup);
        }
        rowcount.set(rows.borrow().len() as i64);
    } else {
        let n = stmt
            .execute(params_from_iter(bound.iter()))
            .map_err(|e| value_error(format!("sqlite: {e}")))?;
        rowcount.set(n as i64);
        lastrowid.set(inner.last_insert_rowid());
    }
    Ok(())
}

fn run_executemany(
    conn: &Rc<Conn>,
    args: &[Object],
    rows: &Rc<RefCell<VecDeque<Vec<Object>>>>,
    description: &Rc<RefCell<Object>>,
    rowcount: &Rc<Cell<i64>>,
    _lastrowid: &Rc<Cell<i64>>,
) -> Result<(), RuntimeError> {
    let sql = match args.first() {
        Some(Object::Str(s)) => s.to_string(),
        _ => return Err(type_error("executemany requires SQL")),
    };
    let seq = args.get(1).cloned().unwrap_or(Object::None);
    let slot = conn.inner.borrow();
    let inner = slot
        .as_ref()
        .ok_or_else(|| value_error("connection closed"))?;
    let mut stmt = inner
        .prepare(&sql)
        .map_err(|e| value_error(format!("sqlite: {e}")))?;
    rows.borrow_mut().clear();
    *description.borrow_mut() = Object::None;
    let mut total = 0i64;
    let iter: Vec<Object> = match seq {
        Object::List(l) => l.borrow().clone(),
        Object::Tuple(t) => t.iter().cloned().collect(),
        _ => return Err(type_error("executemany requires iterable of params")),
    };
    for row in iter {
        let bound = bind_params(&row)?;
        let n = stmt
            .execute(params_from_iter(bound.iter()))
            .map_err(|e| value_error(format!("sqlite: {e}")))?;
        total += n as i64;
    }
    rowcount.set(total);
    Ok(())
}

fn bind_params(obj: &Object) -> Result<Vec<SqlValue>, RuntimeError> {
    match obj {
        Object::None => Ok(Vec::new()),
        Object::Tuple(t) => t.iter().map(object_to_sql).collect(),
        Object::List(l) => l.borrow().iter().map(object_to_sql).collect(),
        _ => Err(type_error("parameters must be tuple/list/None")),
    }
}

fn object_to_sql(o: &Object) -> Result<SqlValue, RuntimeError> {
    Ok(match o {
        Object::None => SqlValue::Null,
        Object::Bool(b) => SqlValue::Integer(i64::from(*b)),
        Object::Int(i) => SqlValue::Integer(*i),
        Object::Long(b) => match num_traits::ToPrimitive::to_i64(b.as_ref()) {
            Some(i) => SqlValue::Integer(i),
            None => SqlValue::Text(b.to_string()),
        },
        Object::Float(f) => SqlValue::Real(*f),
        Object::Str(s) => SqlValue::Text(s.to_string()),
        Object::Bytes(b) => SqlValue::Blob(b.to_vec()),
        Object::ByteArray(b) => SqlValue::Blob(b.borrow().clone()),
        _ => return Err(type_error("unsupported sqlite parameter type")),
    })
}

fn sql_to_object(v: SqlValue) -> Object {
    match v {
        SqlValue::Null => Object::None,
        SqlValue::Integer(i) => Object::Int(i),
        SqlValue::Real(r) => Object::Float(r),
        SqlValue::Text(s) => Object::from_str(s),
        SqlValue::Blob(b) => Object::new_bytes(b),
    }
}

fn column_descriptions(stmt: &rusqlite::Statement) -> Object {
    let names: Vec<&str> = stmt.column_names();
    let mut out = Vec::with_capacity(names.len());
    for name in names {
        out.push(Object::new_tuple(vec![
            Object::from_str(name.to_owned()),
            Object::None,
            Object::None,
            Object::None,
            Object::None,
            Object::None,
            Object::None,
        ]));
    }
    Object::new_tuple(out)
}

fn builtin<F>(name: &'static str, body: F) -> Object
where
    F: Fn(&[Object]) -> Result<Object, RuntimeError> + 'static,
{
    Object::Builtin(Rc::new(BuiltinFn {
        name,
        call: Box::new(body),
    }))
}

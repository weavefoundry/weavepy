//! Raw `sqlite3_stmt` handling: prepare, bind, step, column reads
//! (`statement.c` + the bind/fetch halves of `cursor.c`).

use std::os::raw::{c_char, c_int, c_void};

use rusqlite::ffi;

use super::{
    adapt_object, data_error_class, have_adapters, operational_error_class, prepare_protocol_class,
    programming_error_class, raise, raise_sqlite_error, ConnState, Interp,
};
use crate::error::{type_error, RuntimeError};
use crate::object::Object;
use crate::sync::Rc;
use crate::sync::RefCell;

/// SQLITE_TRANSIENT — tell SQLite to make its own copy of bound
/// text/blob data.
///
/// The C constant is `(sqlite3_destructor_type)-1`.
fn transient() -> ffi::sqlite3_destructor_type {
    // SAFETY: this is the documented SQLITE_TRANSIENT sentinel value.
    Some(unsafe { std::mem::transmute::<isize, unsafe extern "C" fn(*mut c_void)>(-1isize) })
}

/// A prepared statement handle. `ptr == 0` after finalize.
pub(crate) struct Statement {
    pub ptr: usize,
    /// First-keyword classification (`INSERT`/`UPDATE`/`DELETE`/`REPLACE`).
    pub is_dml: bool,
    pub sql: String,
    /// The `sqlite3*` this statement was prepared against — guards the
    /// statement cache across `Connection.__init__` re-init.
    pub db: usize,
}

impl Statement {
    pub(crate) fn stmt(&self) -> *mut ffi::sqlite3_stmt {
        self.ptr as *mut ffi::sqlite3_stmt
    }

    /// Prepare a single statement. Raises ProgrammingError if `sql`
    /// contains more than one statement (`cursor.c` check).
    pub(crate) fn prepare(db: *mut ffi::sqlite3, sql: &str) -> Result<Self, RuntimeError> {
        if sql.contains('\0') {
            return Err(raise(
                programming_error_class(),
                "the query contains a null character",
            ));
        }
        check_sql_length(db, sql)?;
        let bytes = sql.as_bytes();
        let mut stmt: *mut ffi::sqlite3_stmt = std::ptr::null_mut();
        let mut tail: *const c_char = std::ptr::null();
        // SAFETY: `db` is a live handle; `bytes` outlives the call; the
        // out-pointers are valid locals.
        let rc = unsafe {
            ffi::sqlite3_prepare_v2(
                db,
                bytes.as_ptr().cast::<c_char>(),
                bytes.len() as c_int,
                &raw mut stmt,
                &raw mut tail,
            )
        };
        if rc != ffi::SQLITE_OK {
            return Err(raise_sqlite_error(db, rc));
        }
        if stmt.is_null() {
            // Pure comment / whitespace: CPython treats this as an empty
            // query — prepare succeeds and yields no columns and no work.
            return Ok(Self {
                ptr: 0,
                is_dml: false,
                sql: sql.to_owned(),
                db: db as usize,
            });
        }
        // Reject trailing content beyond the first statement.
        // SAFETY: `tail` points into `bytes` (or its NUL) per the API.
        let consumed = unsafe { tail.offset_from(bytes.as_ptr().cast::<c_char>()) } as usize;
        let rest = &sql[consumed.min(sql.len())..];
        if !tail_is_blank(rest) {
            // SAFETY: `stmt` was just prepared and is finalized exactly once.
            unsafe { ffi::sqlite3_finalize(stmt) };
            return Err(raise(
                programming_error_class(),
                "You can only execute one statement at a time.",
            ));
        }
        let is_dml = sql_is_dml(sql);
        Ok(Self {
            ptr: stmt as usize,
            is_dml,
            sql: sql.to_owned(),
            db: db as usize,
        })
    }

    pub(crate) fn step(&self, db: *mut ffi::sqlite3) -> Result<bool, RuntimeError> {
        if self.ptr == 0 {
            return Ok(false);
        }
        // SAFETY: `self.ptr` is a live statement.
        let rc = unsafe { ffi::sqlite3_step(self.stmt()) };
        match rc {
            ffi::SQLITE_ROW => Ok(true),
            ffi::SQLITE_DONE => Ok(false),
            _ => Err(raise_sqlite_error(db, rc)),
        }
    }

    pub(crate) fn reset(&self) {
        if self.ptr != 0 {
            // SAFETY: live statement; reset is always safe post-prepare.
            unsafe {
                ffi::sqlite3_reset(self.stmt());
                ffi::sqlite3_clear_bindings(self.stmt());
            }
        }
    }

    pub(crate) fn finalize(&mut self) {
        if self.ptr != 0 {
            // SAFETY: finalized exactly once; ptr zeroed after.
            unsafe { ffi::sqlite3_finalize(self.stmt()) };
            self.ptr = 0;
        }
    }

    pub(crate) fn column_count(&self) -> i32 {
        if self.ptr == 0 {
            return 0;
        }
        // SAFETY: live statement.
        unsafe { ffi::sqlite3_column_count(self.stmt()) }
    }

    pub(crate) fn column_name(&self, i: i32) -> String {
        // SAFETY: live statement; sqlite copies the name into its own
        // storage which lives until the next call for the same column.
        unsafe { super::cstr_to_string(ffi::sqlite3_column_name(self.stmt(), i)) }
    }

    pub(crate) fn column_decltype(&self, i: i32) -> Option<String> {
        // SAFETY: live statement.
        let p = unsafe { ffi::sqlite3_column_decltype(self.stmt(), i) };
        if p.is_null() {
            None
        } else {
            // SAFETY: non-null decltype is a NUL-terminated string.
            Some(unsafe { super::cstr_to_string(p) })
        }
    }
}

/// First-keyword DML classification (`lex_first_token` in statement.c):
/// only INSERT/UPDATE/DELETE/REPLACE trigger the implicit transaction and
/// the rowcount/lastrowid refresh. Leading whitespace and comments are
/// skipped like CPython's lexer.
pub(crate) fn sql_is_dml(sql: &str) -> bool {
    let mut s = sql;
    loop {
        s = s.trim_start();
        if let Some(r) = s.strip_prefix("--") {
            match r.find('\n') {
                Some(i) => s = &r[i + 1..],
                None => return false,
            }
        } else if let Some(r) = s.strip_prefix("/*") {
            match r.find("*/") {
                Some(i) => s = &r[i + 2..],
                None => return false,
            }
        } else {
            break;
        }
    }
    let word: String = s
        .chars()
        .take_while(|c| c.is_ascii_alphabetic())
        .collect::<String>()
        .to_ascii_uppercase();
    matches!(word.as_str(), "INSERT" | "UPDATE" | "DELETE" | "REPLACE")
}

/// CPython pre-checks the SQL byte length against
/// `SQLITE_LIMIT_SQL_LENGTH` and raises its own DataError shape rather
/// than letting sqlite report "statement too long".
pub(crate) fn check_sql_length(db: *mut ffi::sqlite3, sql: &str) -> Result<(), RuntimeError> {
    // SAFETY: live db handle; -1 queries the current limit.
    let max = unsafe { ffi::sqlite3_limit(db, ffi::SQLITE_LIMIT_SQL_LENGTH, -1) };
    if sql.len() > max as usize {
        return Err(raise(data_error_class(), "query string is too large"));
    }
    Ok(())
}

fn tail_is_blank(rest: &str) -> bool {
    // CPython allows trailing whitespace, semicolons and comments after
    // the first statement (`sqlite3_complete`-adjacent lexing in
    // `pysqlite_statement_create`). A second real statement is an error.
    let mut s = rest;
    loop {
        s = s.trim_start_matches(|c: char| c.is_whitespace() || c == ';');
        if let Some(r) = s.strip_prefix("--") {
            match r.find('\n') {
                Some(i) => s = &r[i + 1..],
                None => return true,
            }
        } else if let Some(r) = s.strip_prefix("/*") {
            match r.find("*/") {
                Some(i) => s = &r[i + 2..],
                None => return true,
            }
        } else {
            return s.is_empty();
        }
    }
}

// ---------------------------------------------------------------
// Parameter binding (`_pysqlite_statement_bind_parameter[s]`)
// ---------------------------------------------------------------

/// Bind one adapted value at 1-based index `idx`.
fn bind_one(
    db: *mut ffi::sqlite3,
    stmt: *mut ffi::sqlite3_stmt,
    idx: i32,
    value: &Object,
) -> Result<(), RuntimeError> {
    let rc = match value {
        Object::None => {
            // SAFETY: live statement, in-range index checked by SQLite.
            unsafe { ffi::sqlite3_bind_null(stmt, idx) }
        }
        Object::Bool(b) => {
            // SAFETY: as above.
            unsafe { ffi::sqlite3_bind_int64(stmt, idx, i64::from(*b)) }
        }
        Object::Int(i) => {
            // SAFETY: as above.
            unsafe { ffi::sqlite3_bind_int64(stmt, idx, *i) }
        }
        Object::Long(b) => match num_traits::ToPrimitive::to_i64(b.as_ref()) {
            // SAFETY: as above.
            Some(i) => unsafe { ffi::sqlite3_bind_int64(stmt, idx, i) },
            None => {
                return Err(crate::error::overflow_error(
                    "Python int too large to convert to SQLite INTEGER",
                ))
            }
        },
        Object::Float(f) => {
            // SAFETY: as above.
            unsafe { ffi::sqlite3_bind_double(stmt, idx, *f) }
        }
        Object::Str(s) => {
            let text = s.to_string();
            // SAFETY: SQLITE_TRANSIENT makes sqlite copy the buffer
            // before returning, so the temporary is safe to drop.
            unsafe {
                ffi::sqlite3_bind_text64(
                    stmt,
                    idx,
                    text.as_ptr().cast::<c_char>(),
                    text.len() as u64,
                    transient(),
                    ffi::SQLITE_UTF8 as u8,
                )
            }
        }
        other => {
            // Lone-surrogate strings raise UnicodeEncodeError (CPython
            // funnels the bind through PyUnicode_AsUTF8AndSize); str
            // subclasses bind as TEXT.
            if let Some(text) = super::as_text(other) {
                let text = text?;
                // SAFETY: SQLITE_TRANSIENT copies the buffer.
                let rc = unsafe {
                    ffi::sqlite3_bind_text64(
                        stmt,
                        idx,
                        text.as_ptr().cast::<c_char>(),
                        text.len() as u64,
                        transient(),
                        ffi::SQLITE_UTF8 as u8,
                    )
                };
                if rc != ffi::SQLITE_OK {
                    return Err(raise_sqlite_error(db, rc));
                }
                return Ok(());
            }
            match super::buffer_bytes(other) {
                Some(bytes) => {
                    let bytes = bytes?;
                    // SAFETY: SQLITE_TRANSIENT copies the buffer.
                    unsafe {
                        ffi::sqlite3_bind_blob64(
                            stmt,
                            idx,
                            bytes.as_ptr().cast::<c_void>(),
                            bytes.len() as u64,
                            transient(),
                        )
                    }
                }
                None => {
                    return Err(raise(
                        programming_error_class(),
                        format!(
                            "Error binding parameter {}: type '{}' is not supported",
                            idx - 1,
                            other.type_name_owned()
                        ),
                    ))
                }
            }
        }
    };
    if rc != ffi::SQLITE_OK {
        return Err(raise_sqlite_error(db, rc));
    }
    Ok(())
}

fn is_directly_bindable(value: &Object) -> bool {
    matches!(
        value,
        Object::None
            | Object::Bool(_)
            | Object::Int(_)
            | Object::Long(_)
            | Object::Float(_)
            | Object::Str(_)
            | Object::WStr(_)
            | Object::Bytes(_)
            | Object::ByteArray(_)
            | Object::MemoryView(_)
    )
}

/// Adapt (when needed) then bind a single parameter.
fn adapt_and_bind(
    ip: &mut Interp,
    db: *mut ffi::sqlite3,
    stmt: *mut ffi::sqlite3_stmt,
    idx: i32,
    value: &Object,
) -> Result<(), RuntimeError> {
    if is_directly_bindable(value) && !have_adapters() {
        return bind_one(db, stmt, idx, value);
    }
    let proto = Object::Type(prepare_protocol_class());
    // Adaptation falls back to the original object; `bind_one` then
    // raises the CPython ProgrammingError shape for unsupported types.
    let adapted = adapt_object(ip, value, &proto, Some(value))?;
    bind_one(db, stmt, idx, &adapted)
}

/// Bind a full parameter set: sequence (qmark style) or mapping
/// (named style). Mirrors `_pysqlite_statement_bind_parameters`.
pub(crate) fn bind_parameters(
    ip: &mut Interp,
    db: *mut ffi::sqlite3,
    stmt: &Statement,
    params: &Object,
) -> Result<(), RuntimeError> {
    if stmt.ptr == 0 {
        return Ok(());
    }
    // SAFETY: live statement.
    let expected = unsafe { ffi::sqlite3_bind_parameter_count(stmt.stmt()) };
    match params {
        Object::None => {
            if expected != 0 {
                return Err(raise(
                    programming_error_class(),
                    format!(
                        "Incorrect number of bindings supplied. The current statement uses \
                         {expected}, and there are 0 supplied."
                    ),
                ));
            }
            Ok(())
        }
        Object::Dict(d) => {
            for idx in 1..=expected {
                let key = named_parameter(stmt, idx)?;
                let value = d
                    .borrow()
                    .get(&crate::object::DictKey(Object::from_str(key.clone())))
                    .cloned();
                match value {
                    Some(v) => adapt_and_bind(ip, db, stmt.stmt(), idx, &v)?,
                    None => {
                        return Err(raise(
                            programming_error_class(),
                            format!("You did not supply a value for binding parameter :{key}."),
                        ))
                    }
                }
            }
            Ok(())
        }
        Object::List(l) => {
            // Live per-index access (gh-64092): an adapter mutating the
            // list mid-bind surfaces as IndexError, not stale values.
            let expected_len = l.borrow().len();
            bind_sequence(ip, db, stmt, expected, expected_len, |_, i| {
                l.borrow()
                    .get(i)
                    .cloned()
                    .ok_or_else(|| crate::error::index_error("list index out of range"))
            })
        }
        Object::Tuple(t) => {
            let items: Vec<Object> = t.iter().cloned().collect();
            bind_sequence(ip, db, stmt, expected, items.len(), |_, i| {
                items
                    .get(i)
                    .cloned()
                    .ok_or_else(|| crate::error::index_error("tuple index out of range"))
            })
        }
        other => {
            let cls = crate::builtins::class_of(other);
            let dict_cls = crate::builtin_types::builtin_types().dict_.clone();
            if let Object::Instance(_) = other {
                if cls.is_subclass_of(&dict_cls) {
                    // Dict subclass: item access goes through the VM so
                    // `__missing__` and overridden `__getitem__` run
                    // (CPython's PyObject_GetItem path).
                    for idx in 1..=expected {
                        let key = named_parameter(stmt, idx)?;
                        let getitem = ip.load_attr_public(other, "__getitem__")?;
                        let v = super::call(ip, &getitem, &[Object::from_str(key.clone())])
                            .map_err(|e| {
                                if is_key_error(&e) {
                                    raise(
                                        programming_error_class(),
                                        format!(
                                            "You did not supply a value for binding \
                                                 parameter :{key}."
                                        ),
                                    )
                                } else {
                                    e
                                }
                            })?;
                        adapt_and_bind(ip, db, stmt.stmt(), idx, &v)?;
                    }
                    return Ok(());
                }
            }
            // CPython gates on PySequence_Check: anything else raises
            // ProgrammingError. Errors from `__len__` propagate
            // untouched (gh-41662).
            if cls.lookup("__getitem__").is_none() {
                return Err(raise(
                    programming_error_class(),
                    "parameters are of unsupported type",
                ));
            }
            let len_meth = ip.load_attr_public(other, "__len__").map_err(|_| {
                raise(
                    programming_error_class(),
                    "parameters are of unsupported type",
                )
            })?;
            let n = super::call(ip, &len_meth, &[])?
                .as_i64()
                .ok_or_else(|| type_error("__len__() should return an int"))?;
            let obj = other.clone();
            bind_sequence(ip, db, stmt, expected, n.max(0) as usize, move |ip, i| {
                let getitem = ip.load_attr_public(&obj, "__getitem__")?;
                super::call(ip, &getitem, &[Object::Int(i as i64)])
            })
        }
    }
}

/// The (prefix-stripped) name of parameter `idx`, or the CPython
/// "Binding N has no name" ProgrammingError for positional slots.
fn named_parameter(stmt: &Statement, idx: i32) -> Result<String, RuntimeError> {
    // SAFETY: live statement, in-range index.
    let name_ptr = unsafe { ffi::sqlite3_bind_parameter_name(stmt.stmt(), idx) };
    if name_ptr.is_null() {
        return Err(raise(
            programming_error_class(),
            format!(
                "Binding {idx} has no name, but you supplied a dictionary (which has only \
                 names)."
            ),
        ));
    }
    // SAFETY: non-null parameter name is NUL-terminated.
    let full = unsafe { super::cstr_to_string(name_ptr) };
    Ok(full
        .strip_prefix([':', '@', '$'])
        .unwrap_or(full.as_str())
        .to_owned())
}

fn is_key_error(e: &RuntimeError) -> bool {
    if let RuntimeError::PyException(exc) = e {
        if let Object::Instance(inst) = &exc.instance {
            return crate::builtin_types::builtin_types()
                .by_name("KeyError")
                .is_some_and(|cls| inst.cls().is_subclass_of(&cls));
        }
    }
    false
}

fn bind_sequence(
    ip: &mut Interp,
    db: *mut ffi::sqlite3,
    stmt: &Statement,
    expected: i32,
    supplied: usize,
    mut get: impl FnMut(&mut Interp, usize) -> Result<Object, RuntimeError>,
) -> Result<(), RuntimeError> {
    if supplied as i32 != expected {
        return Err(raise(
            programming_error_class(),
            format!(
                "Incorrect number of bindings supplied. The current statement uses {}, and \
                 there are {} supplied.",
                expected, supplied
            ),
        ));
    }
    for i in 0..supplied {
        let idx = (i + 1) as i32;
        // gh-101698: binding a *named* parameter positionally is
        // deprecated ("?N" indexed placeholders are exempt).
        // SAFETY: live statement, in-range index.
        let name_ptr = unsafe { ffi::sqlite3_bind_parameter_name(stmt.stmt(), idx) };
        if !name_ptr.is_null() {
            // SAFETY: non-null parameter name is NUL-terminated.
            let name = unsafe { super::cstr_to_string(name_ptr) };
            if !name.starts_with('?') {
                ip.warn_deprecation_from_builtin(format!(
                    "Binding {idx} ('{name}') is a named parameter. Starting with Python 3.14, \
                     named parameters must not be bound positionally; use a dict instead."
                ))?;
            }
        }
        let value = get(ip, i)?;
        adapt_and_bind(ip, db, stmt.stmt(), idx, &value)?;
    }
    Ok(())
}

// ---------------------------------------------------------------
// Row reading (`_pysqlite_fetch_one_row`)
// ---------------------------------------------------------------

pub(crate) const PARSE_DECLTYPES: i64 = 1;
pub(crate) const PARSE_COLNAMES: i64 = 2;

/// Per-column converter resolution, done once per execute.
pub(crate) fn resolve_converters(stmt: &Statement, detect_types: i64) -> Vec<Option<Object>> {
    let n = stmt.column_count();
    let mut out = Vec::with_capacity(n as usize);
    for i in 0..n {
        let mut conv: Option<Object> = None;
        if detect_types & PARSE_COLNAMES != 0 {
            let name = stmt.column_name(i);
            if let (Some(start), Some(end)) = (name.find('['), name.rfind(']')) {
                if start < end {
                    let key = name[start + 1..end].to_uppercase();
                    conv = super::converters_dict()
                        .borrow()
                        .get(&crate::object::DictKey(Object::from_str(key)))
                        .cloned();
                }
            }
        }
        if conv.is_none() && detect_types & PARSE_DECLTYPES != 0 {
            if let Some(decl) = stmt.column_decltype(i) {
                // First token of the declared type ("NUMBER(10)" -> "NUMBER").
                let token: String = decl
                    .split_whitespace()
                    .next()
                    .unwrap_or("")
                    .split('(')
                    .next()
                    .unwrap_or("")
                    .to_uppercase();
                if !token.is_empty() {
                    conv = super::converters_dict()
                        .borrow()
                        .get(&crate::object::DictKey(Object::from_str(token)))
                        .cloned();
                }
            }
        }
        out.push(conv);
    }
    out
}

/// The `description` 7-tuples. With PARSE_COLNAMES the visible name is
/// truncated at the `[type]` annotation (`_pysqlite_build_column_name`).
pub(crate) fn build_description(stmt: &Statement, detect_types: i64) -> Object {
    let n = stmt.column_count();
    if n == 0 {
        return Object::None;
    }
    let mut cols = Vec::with_capacity(n as usize);
    for i in 0..n {
        let mut name = stmt.column_name(i);
        if detect_types & PARSE_COLNAMES != 0 {
            if let Some(pos) = name.find('[') {
                name = name[..pos].trim_end().to_owned();
            }
        }
        cols.push(Object::new_tuple(vec![
            Object::from_str(name),
            Object::None,
            Object::None,
            Object::None,
            Object::None,
            Object::None,
            Object::None,
        ]));
    }
    Object::new_tuple(cols)
}

/// Read the current row, applying converters and the text factory.
pub(crate) fn fetch_row(
    ip: &mut Interp,
    conn: &Rc<RefCell<ConnState>>,
    stmt: &Statement,
    converters: &[Option<Object>],
) -> Result<Vec<Object>, RuntimeError> {
    let n = stmt.column_count();
    let mut out = Vec::with_capacity(n as usize);
    for i in 0..n {
        // Converter columns receive raw bytes (or None for NULL) —
        // regardless of the sqlite storage class.
        if let Some(conv) = converters.get(i as usize).and_then(|c| c.as_ref()) {
            // Converters receive the raw bytes; a NULL blob pointer
            // (SQL NULL *or* a zero-sized value) short-circuits to None
            // without invoking the converter (`_pysqlite_fetch_one_row`).
            match column_bytes(stmt, i) {
                None => out.push(Object::None),
                Some(bytes) => out.push(super::call(ip, conv, &[Object::new_bytes(bytes)])?),
            }
            continue;
        }
        // SAFETY: live statement positioned on a row.
        let ty = unsafe { ffi::sqlite3_column_type(stmt.stmt(), i) };
        let value = match ty {
            ffi::SQLITE_NULL => Object::None,
            ffi::SQLITE_INTEGER => {
                // SAFETY: as above.
                Object::Int(unsafe { ffi::sqlite3_column_int64(stmt.stmt(), i) })
            }
            ffi::SQLITE_FLOAT => {
                // SAFETY: as above.
                Object::Float(unsafe { ffi::sqlite3_column_double(stmt.stmt(), i) })
            }
            ffi::SQLITE_BLOB => Object::new_bytes(column_bytes(stmt, i).unwrap_or_default()),
            _ => {
                // TEXT — routed through the connection's text_factory.
                let bytes = column_bytes(stmt, i).unwrap_or_default();
                apply_text_factory(ip, conn, bytes, stmt, i)?
            }
        };
        out.push(value);
    }
    Ok(out)
}

/// Raw column bytes; `None` when sqlite hands back a NULL pointer
/// (SQL NULL or a zero-sized value).
fn column_bytes(stmt: &Statement, i: i32) -> Option<Vec<u8>> {
    // SAFETY: live statement positioned on a row; blob pointer is valid
    // for `bytes` bytes until the next sqlite call on this statement.
    unsafe {
        let p = ffi::sqlite3_column_blob(stmt.stmt(), i).cast::<u8>();
        let n = ffi::sqlite3_column_bytes(stmt.stmt(), i) as usize;
        if p.is_null() {
            None
        } else {
            Some(std::slice::from_raw_parts(p, n).to_vec())
        }
    }
}

fn apply_text_factory(
    ip: &mut Interp,
    conn: &Rc<RefCell<ConnState>>,
    bytes: Vec<u8>,
    stmt: &Statement,
    col: i32,
) -> Result<Object, RuntimeError> {
    let factory = conn.borrow().text_factory.clone();
    // Default (str): decode UTF-8, with CPython's OperationalError shape
    // on failure.
    let is_str = matches!(&factory, Object::Type(t) if t.name == "str");
    if is_str {
        return match String::from_utf8(bytes) {
            Ok(s) => Ok(Object::from_str(s)),
            Err(e) => Err(raise(
                operational_error_class(),
                format!(
                    "Could not decode to UTF-8 column '{}' with text '{}'",
                    stmt.column_name(col),
                    String::from_utf8_lossy(e.as_bytes())
                ),
            )),
        };
    }
    if matches!(&factory, Object::Type(t) if t.name == "bytes") {
        return Ok(Object::new_bytes(bytes));
    }
    if matches!(&factory, Object::Type(t) if t.name == "bytearray") {
        let b = Object::new_bytes(bytes);
        let ba_cls = ip
            .builtins_dict()
            .borrow()
            .get(&crate::object::DictKey(Object::from_static("bytearray")))
            .cloned();
        return match ba_cls {
            Some(cls) => super::call(ip, &cls, &[b]),
            None => Ok(b),
        };
    }
    super::call(ip, &factory, &[Object::new_bytes(bytes)])
}

/// Interface-level check used by cursor.execute: sql must be str.
pub(crate) fn require_sql_str(obj: Option<&Object>) -> Result<String, RuntimeError> {
    match obj {
        Some(other) => match super::as_text(other) {
            Some(text) => text,
            None => Err(type_error(format!(
                "execute() argument 1 must be str, not {}",
                other.type_name_owned()
            ))),
        },
        None => Err(type_error("execute() missing required argument (pos 1)")),
    }
}

/// Statement-cache lookup/insert (LRU by SQL text, `cached_statements`
/// capacity). Statements handed out are *removed* from the cache while
/// in use (single borrower) and returned by the cursor when finished.
pub(crate) fn cache_take(conn: &Rc<RefCell<ConnState>>, sql: &str) -> Option<Statement> {
    let mut st = conn.borrow_mut();
    if let Some(pos) = st.stmt_cache.iter().position(|(s, _)| s == sql) {
        let (sql, ptr) = st.stmt_cache.remove(pos);
        let stmt = Statement {
            ptr,
            is_dml: sql_is_dml(&sql),
            sql,
            db: st.db,
        };
        stmt.reset();
        return Some(stmt);
    }
    None
}

pub(crate) fn cache_put(conn: &Rc<RefCell<ConnState>>, mut stmt: Statement) {
    if stmt.ptr == 0 {
        return;
    }
    let mut st = conn.borrow_mut();
    // A statement prepared against a different `sqlite3*` (the cursor
    // outlived a `Connection.__init__` re-init) must not be cached.
    if st.db == 0 || st.cached_statements == 0 || st.db != stmt.db {
        drop(st);
        stmt.finalize();
        return;
    }
    stmt.reset();
    let sql = std::mem::take(&mut stmt.sql);
    st.stmt_cache.push((sql, stmt.ptr));
    stmt.ptr = 0;
    while st.stmt_cache.len() > st.cached_statements {
        let (_, ptr) = st.stmt_cache.remove(0);
        // SAFETY: evicted pointer is a live statement owned by the cache.
        unsafe { ffi::sqlite3_finalize(ptr as *mut ffi::sqlite3_stmt) };
    }
}

/// Implicit transaction begin (`begin_transaction` gate in cursor.c):
/// legacy mode + isolation_level set + currently in autocommit + the
/// statement writes.
pub(crate) fn maybe_begin_transaction(
    conn: &Rc<RefCell<ConnState>>,
    stmt: &Statement,
) -> Result<(), RuntimeError> {
    let (db, level, autocommit) = {
        let s = conn.borrow();
        (s.db_ptr(), s.isolation_level.clone(), s.autocommit)
    };
    if autocommit != super::LEGACY_TRANSACTION_CONTROL {
        return Ok(());
    }
    let Some(level) = level else { return Ok(()) };
    // Only true DML autostarts a transaction (cursor.c checks
    // `is_dml`, not readonly — DDL never autostarts, by design).
    if !stmt.is_dml {
        return Ok(());
    }
    // SAFETY: live db handle.
    if unsafe { ffi::sqlite3_get_autocommit(db) } == 0 {
        return Ok(()); // already in a transaction
    }
    // CPython's begin_statement is literally "BEGIN " + isolation_level,
    // so the default "" traces as "BEGIN " (trailing space) and
    // "DEFERRED" as "BEGIN DEFERRED".
    exec_simple(db, &format!("BEGIN {level}"))
}

/// Run a parameterless internal statement (BEGIN/COMMIT/ROLLBACK).
pub(crate) fn exec_simple(db: *mut ffi::sqlite3, sql: &str) -> Result<(), RuntimeError> {
    let c = std::ffi::CString::new(sql).map_err(|_| type_error("internal SQL contained NUL"))?;
    // SAFETY: live db handle; no callbacks; errmsg out-param unused.
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
    Ok(())
}

//! `Row` (`row.c`) and `Blob` (`blob.c`) heap types.

use std::collections::HashMap;
use std::os::raw::c_int;

use rusqlite::ffi;

use super::cursor::CursorState;
use super::{
    interp, method, next_handle, operational_error_class, programming_error_class, raise,
    raise_sqlite_error, self_instance, ConnState,
};
use crate::error::{index_error, type_error, value_error, RuntimeError};
use crate::object::{DictData, DictKey, Object};
use crate::sync::Rc;
use crate::sync::RefCell;
use crate::types::{PyInstance, TypeObject};

// ---------------------------------------------------------------
// Row
// ---------------------------------------------------------------

const ROW_DATA_KEY: &str = "_wp_row_data";
const ROW_DESC_KEY: &str = "_wp_row_desc";

pub(crate) fn row_class() -> Rc<TypeObject> {
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
            method("__init__", row_init),
        );
        dict.insert(
            DictKey(Object::from_static("keys")),
            method("keys", row_keys),
        );
        dict.insert(
            DictKey(Object::from_static("__getitem__")),
            method("__getitem__", row_getitem),
        );
        dict.insert(
            DictKey(Object::from_static("__len__")),
            method("__len__", row_len),
        );
        dict.insert(
            DictKey(Object::from_static("__iter__")),
            method("__iter__", row_iter),
        );
        dict.insert(
            DictKey(Object::from_static("__eq__")),
            method("__eq__", row_eq),
        );
        dict.insert(
            DictKey(Object::from_static("__ne__")),
            method("__ne__", row_ne),
        );
        dict.insert(
            DictKey(Object::from_static("__hash__")),
            method("__hash__", row_hash),
        );
        dict.insert(
            DictKey(Object::from_static("__repr__")),
            method("__repr__", row_repr),
        );
        TypeObject::new_user("Row", vec![bt.object_.clone()], dict)
            .expect("Row class must linearise")
    })
    .clone()
}

/// Fast-path constructor used by the cursor when `row_factory is Row`.
pub(crate) fn make_row(cursor_obj: &Object, st: &Rc<RefCell<CursorState>>, data: Object) -> Object {
    let _ = cursor_obj;
    let inst = PyInstance::new(row_class());
    let desc = st.borrow().description.clone();
    {
        let mut d = inst.dict.borrow_mut();
        d.insert(DictKey(Object::from_static(ROW_DATA_KEY)), data);
        d.insert(DictKey(Object::from_static(ROW_DESC_KEY)), desc);
    }
    Object::Instance(Rc::new(inst))
}

fn row_init(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = self_instance(args)?;
    let cursor = args
        .get(1)
        .cloned()
        .ok_or_else(|| type_error("Row() missing required argument 'cursor'"))?;
    // A real Cursor (sub)instance is required; a lying `__class__`
    // attribute is not (row.c uses PyObject_TypeCheck, gh-issue 24257).
    let is_cursor = matches!(
        &cursor,
        Object::Instance(i) if i.cls().is_subclass_of(&super::cursor::cursor_class())
    );
    if !is_cursor {
        return Err(type_error(format!(
            "instance of cursor required for cursor argument, not {}",
            cursor.type_name_owned()
        )));
    }
    let data = args
        .get(2)
        .cloned()
        .ok_or_else(|| type_error("Row() missing required argument 'data'"))?;
    if !matches!(data, Object::Tuple(_)) {
        return Err(type_error("instance data must be a tuple"));
    }
    // Description is read from the cursor at construction time.
    let ip = interp()?;
    let desc = ip
        .load_attr_public(&cursor, "description")
        .unwrap_or(Object::None);
    let mut d = inst.dict.borrow_mut();
    d.insert(DictKey(Object::from_static(ROW_DATA_KEY)), data);
    d.insert(DictKey(Object::from_static(ROW_DESC_KEY)), desc);
    Ok(Object::None)
}

fn row_parts(args: &[Object]) -> Result<(Object, Object), RuntimeError> {
    let inst = self_instance(args)?;
    let d = inst.dict.borrow();
    let data = d
        .get(&DictKey(Object::from_static(ROW_DATA_KEY)))
        .cloned()
        .ok_or_else(|| type_error("Row.__init__ not called"))?;
    let desc = d
        .get(&DictKey(Object::from_static(ROW_DESC_KEY)))
        .cloned()
        .unwrap_or(Object::None);
    Ok((data, desc))
}

fn desc_names(desc: &Object) -> Vec<String> {
    let mut out = Vec::new();
    if let Object::Tuple(cols) = desc {
        for col in cols.iter() {
            if let Object::Tuple(t) = col {
                if let Some(Object::Str(name)) = t.first() {
                    out.push(name.to_string());
                }
            }
        }
    }
    out
}

fn row_keys(args: &[Object]) -> Result<Object, RuntimeError> {
    let (_, desc) = row_parts(args)?;
    Ok(Object::new_list(
        desc_names(&desc)
            .into_iter()
            .map(Object::from_str)
            .collect(),
    ))
}

fn row_len(args: &[Object]) -> Result<Object, RuntimeError> {
    let (data, _) = row_parts(args)?;
    match data {
        Object::Tuple(t) => Ok(Object::Int(t.len() as i64)),
        _ => Err(type_error("Row data must be a tuple")),
    }
}

fn row_iter(args: &[Object]) -> Result<Object, RuntimeError> {
    let (data, _) = row_parts(args)?;
    let ip = interp()?;
    let iter_fn = ip
        .builtins_dict()
        .borrow()
        .get(&DictKey(Object::from_static("iter")))
        .cloned()
        .ok_or_else(|| RuntimeError::Internal("_sqlite3: no iter builtin".into()))?;
    super::call(ip, &iter_fn, &[data])
}

fn row_getitem(args: &[Object]) -> Result<Object, RuntimeError> {
    let (data, desc) = row_parts(args)?;
    let key = args
        .get(1)
        .ok_or_else(|| type_error("Row.__getitem__ requires a key"))?;
    let Object::Tuple(items) = &data else {
        return Err(type_error("Row data must be a tuple"));
    };
    match key {
        Object::Int(_) | Object::Long(_) | Object::Bool(_) => {
            // A too-big int can't be an index: CPython's
            // PyNumber_AsSsize_t(…, PyExc_IndexError) shape.
            let idx = key
                .as_i64()
                .ok_or_else(|| index_error("cannot fit 'int' into an index-sized integer"))?;
            let n = items.len() as i64;
            let real = if idx < 0 { idx + n } else { idx };
            if real < 0 || real >= n {
                return Err(index_error("index out of range"));
            }
            Ok(items[real as usize].clone())
        }
        Object::Str(name) => {
            let want = name.to_string();
            for (i, col) in desc_names(&desc).iter().enumerate() {
                // Case-insensitive per sqlite semantics (row.c uses a
                // custom ASCII-only case-insensitive compare).
                if col.eq_ignore_ascii_case(&want) {
                    return Ok(items[i].clone());
                }
            }
            Err(index_error(format!("No item with key {want:?}")))
        }
        Object::Slice(_) => {
            // Delegate slicing to the underlying tuple via the VM.
            let ip = interp()?;
            let getitem = ip.load_attr_public(&data, "__getitem__")?;
            super::call(ip, &getitem, std::slice::from_ref(key))
        }
        other => Err(index_error(format!(
            "Index must be int or string, not {}",
            other.type_name_owned()
        ))),
    }
}

fn row_eq(args: &[Object]) -> Result<Object, RuntimeError> {
    let (data, desc) = row_parts(args)?;
    let other = args.get(1).cloned().unwrap_or(Object::None);
    let Object::Instance(other_inst) = &other else {
        return Ok(crate::vm_singletons::not_implemented());
    };
    if !other_inst.cls().is_subclass_of(&row_class()) {
        return Ok(crate::vm_singletons::not_implemented());
    }
    let od = other_inst.dict.borrow();
    let other_data = od
        .get(&DictKey(Object::from_static(ROW_DATA_KEY)))
        .cloned()
        .unwrap_or(Object::None);
    let other_desc = od
        .get(&DictKey(Object::from_static(ROW_DESC_KEY)))
        .cloned()
        .unwrap_or(Object::None);
    drop(od);
    // Tuple equality through the VM so element-wise `__eq__` runs.
    let ip = interp()?;
    let data_eq = ip.load_attr_public(&data, "__eq__")?;
    let r1 = super::call(ip, &data_eq, &[other_data])?;
    let desc_eq = ip.load_attr_public(&desc, "__eq__")?;
    let r2 = super::call(ip, &desc_eq, &[other_desc])?;
    let truthy = |o: &Object| matches!(o, Object::Bool(true));
    Ok(Object::Bool(truthy(&r1) && truthy(&r2)))
}

fn row_ne(args: &[Object]) -> Result<Object, RuntimeError> {
    match row_eq(args)? {
        Object::Bool(b) => Ok(Object::Bool(!b)),
        other => Ok(other),
    }
}

fn row_hash(args: &[Object]) -> Result<Object, RuntimeError> {
    let (data, desc) = row_parts(args)?;
    let ip = interp()?;
    let hash_fn = ip
        .builtins_dict()
        .borrow()
        .get(&DictKey(Object::from_static("hash")))
        .cloned()
        .ok_or_else(|| RuntimeError::Internal("_sqlite3: no hash builtin".into()))?;
    let h1 = super::call(ip, &hash_fn, &[desc])?;
    let h2 = super::call(ip, &hash_fn, &[data])?;
    let (a, b) = (h1.as_i64().unwrap_or(0), h2.as_i64().unwrap_or(0));
    Ok(Object::Int(a ^ b))
}

fn row_repr(_args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(Object::from_static("<sqlite3.Row object>"))
}

// ---------------------------------------------------------------
// Blob
// ---------------------------------------------------------------

const BLOB_HANDLE_KEY: &str = "_wp_sqlite3_blob";

pub(crate) struct BlobState {
    /// Raw `sqlite3_blob*`; 0 after close.
    pub ptr: usize,
    pub offset: i64,
    /// The owning Connection: kept alive while the blob is open and
    /// checked on every operation (`pysqlite_check_blob` checks the
    /// connection before the blob handle).
    pub connection: Object,
}

fn blob_registry() -> &'static parking_lot::Mutex<HashMap<i64, Rc<RefCell<BlobState>>>> {
    static REG: std::sync::OnceLock<parking_lot::Mutex<HashMap<i64, Rc<RefCell<BlobState>>>>> =
        std::sync::OnceLock::new();
    REG.get_or_init(|| parking_lot::Mutex::new(HashMap::new()))
}

pub(crate) fn blob_class() -> Rc<TypeObject> {
    static CELL: std::sync::OnceLock<Rc<TypeObject>> = std::sync::OnceLock::new();
    CELL.get_or_init(|| {
        let bt = crate::builtin_types::builtin_types();
        let mut dict = DictData::default();
        dict.insert(
            DictKey(Object::from_static("__module__")),
            Object::from_static("sqlite3"),
        );
        // Py_TPFLAGS_DISALLOW_INSTANTIATION: block `tp(...)` and the
        // direct `tp.__new__(tp)` escape hatch alike.
        dict.insert(
            DictKey(Object::from_static("__init__")),
            method("__init__", blob_disallow_init),
        );
        dict.insert(
            DictKey(Object::from_static("__new__")),
            method("__new__", blob_disallow_init),
        );
        dict.insert(
            DictKey(Object::from_static("read")),
            method("read", blob_read),
        );
        dict.insert(
            DictKey(Object::from_static("write")),
            method("write", blob_write),
        );
        dict.insert(
            DictKey(Object::from_static("seek")),
            method("seek", blob_seek),
        );
        dict.insert(
            DictKey(Object::from_static("tell")),
            method("tell", blob_tell),
        );
        dict.insert(
            DictKey(Object::from_static("close")),
            method("close", blob_close),
        );
        dict.insert(
            DictKey(Object::from_static("__len__")),
            method("__len__", blob_len),
        );
        dict.insert(
            DictKey(Object::from_static("__getitem__")),
            method("__getitem__", blob_getitem),
        );
        dict.insert(
            DictKey(Object::from_static("__setitem__")),
            method("__setitem__", blob_setitem),
        );
        dict.insert(
            DictKey(Object::from_static("__delitem__")),
            method("__delitem__", blob_delitem),
        );
        dict.insert(
            DictKey(Object::from_static("__enter__")),
            method("__enter__", blob_enter),
        );
        dict.insert(
            DictKey(Object::from_static("__exit__")),
            method("__exit__", blob_exit),
        );
        dict.insert(
            DictKey(Object::from_static("__del__")),
            method("__del__", blob_del),
        );
        TypeObject::new_user("Blob", vec![bt.object_.clone()], dict)
            .expect("Blob class must linearise")
    })
    .clone()
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn blob_open(
    _ip: &mut super::Interp,
    connection_obj: &Object,
    conn: &Rc<RefCell<ConnState>>,
    name: &str,
    table: &str,
    column: &str,
    rowid: i64,
    readonly: bool,
) -> Result<Object, RuntimeError> {
    let db = conn.borrow().db_ptr();
    let c_name = std::ffi::CString::new(name).map_err(|_| value_error("embedded null byte"))?;
    let c_table = std::ffi::CString::new(table).map_err(|_| value_error("embedded null byte"))?;
    let c_column = std::ffi::CString::new(column).map_err(|_| value_error("embedded null byte"))?;
    let mut blob: *mut ffi::sqlite3_blob = std::ptr::null_mut();
    // SAFETY: live db handle, valid C strings, valid out-pointer.
    let rc = unsafe {
        ffi::sqlite3_blob_open(
            db,
            c_name.as_ptr(),
            c_table.as_ptr(),
            c_column.as_ptr(),
            rowid,
            c_int::from(!readonly),
            &raw mut blob,
        )
    };
    if rc != ffi::SQLITE_OK {
        return Err(raise_sqlite_error(db, rc));
    }
    let state = Rc::new(RefCell::new(BlobState {
        ptr: blob as usize,
        offset: 0,
        connection: connection_obj.clone(),
    }));
    let handle = next_handle();
    blob_registry().lock().insert(handle, state);
    let inst = PyInstance::new(blob_class());
    inst.dict.borrow_mut().insert(
        DictKey(Object::from_static(BLOB_HANDLE_KEY)),
        Object::Int(handle),
    );
    Ok(Object::Instance(Rc::new(inst)))
}

fn raw_blob_state(args: &[Object]) -> Result<Rc<RefCell<BlobState>>, RuntimeError> {
    let inst = self_instance(args)?;
    let handle = inst
        .dict
        .borrow()
        .get(&DictKey(Object::from_static(BLOB_HANDLE_KEY)))
        .cloned();
    match handle {
        Some(Object::Int(h)) => blob_registry().lock().get(&h).cloned().ok_or_else(|| {
            raise(
                programming_error_class(),
                "Cannot operate on a closed blob.",
            )
        }),
        _ => Err(type_error("expected a sqlite3.Blob instance")),
    }
}

/// `pysqlite_check_blob`: the owning connection must be open (checked
/// *before* the blob handle) and same-thread.
fn blob_state(args: &[Object]) -> Result<Rc<RefCell<BlobState>>, RuntimeError> {
    let st = raw_blob_state(args)?;
    let conn_obj = st.borrow().connection.clone();
    super::checked_conn(&conn_obj)?;
    Ok(st)
}

fn open_blob(st: &Rc<RefCell<BlobState>>) -> Result<*mut ffi::sqlite3_blob, RuntimeError> {
    let ptr = st.borrow().ptr;
    if ptr == 0 {
        return Err(raise(
            programming_error_class(),
            "Cannot operate on a closed blob.",
        ));
    }
    Ok(ptr as *mut ffi::sqlite3_blob)
}

fn blob_len(args: &[Object]) -> Result<Object, RuntimeError> {
    let st = blob_state(args)?;
    let blob = open_blob(&st)?;
    // SAFETY: live blob handle.
    Ok(Object::Int(i64::from(unsafe {
        ffi::sqlite3_blob_bytes(blob)
    })))
}

fn blob_read(args: &[Object]) -> Result<Object, RuntimeError> {
    let st = blob_state(args)?;
    let blob = open_blob(&st)?;
    // SAFETY: live blob handle.
    let total = i64::from(unsafe { ffi::sqlite3_blob_bytes(blob) });
    let offset = st.borrow().offset;
    let want = args.get(1).and_then(Object::as_i64).unwrap_or(-1);
    let n = if want < 0 {
        (total - offset).max(0)
    } else {
        want.min(total - offset).max(0)
    };
    let mut buf = vec![0u8; n as usize];
    if n > 0 {
        // SAFETY: buffer is n bytes; offset+n <= total by construction.
        let rc = unsafe {
            ffi::sqlite3_blob_read(
                blob,
                buf.as_mut_ptr().cast::<std::os::raw::c_void>(),
                n as c_int,
                offset as c_int,
            )
        };
        if rc != ffi::SQLITE_OK {
            return Err(raise_sqlite_error(std::ptr::null_mut(), rc));
        }
    }
    st.borrow_mut().offset = offset + n;
    Ok(Object::new_bytes(buf))
}

fn blob_write(args: &[Object]) -> Result<Object, RuntimeError> {
    let st = blob_state(args)?;
    let blob = open_blob(&st)?;
    let data = args
        .get(1)
        .and_then(Object::as_bytes_view)
        .ok_or_else(|| type_error("blob write argument must be bytes-like"))?;
    // SAFETY: live blob handle.
    let total = i64::from(unsafe { ffi::sqlite3_blob_bytes(blob) });
    let offset = st.borrow().offset;
    if offset + data.len() as i64 > total {
        return Err(value_error("data longer than blob length"));
    }
    // SAFETY: data buffer valid for len bytes; range checked above.
    let rc = unsafe {
        ffi::sqlite3_blob_write(
            blob,
            data.as_ptr().cast::<std::os::raw::c_void>(),
            data.len() as c_int,
            offset as c_int,
        )
    };
    if rc != ffi::SQLITE_OK {
        return Err(raise_sqlite_error(std::ptr::null_mut(), rc));
    }
    st.borrow_mut().offset = offset + data.len() as i64;
    Ok(Object::None)
}

fn blob_seek(args: &[Object]) -> Result<Object, RuntimeError> {
    let st = blob_state(args)?;
    let blob = open_blob(&st)?;
    // SAFETY: live blob handle.
    let total = i64::from(unsafe { ffi::sqlite3_blob_bytes(blob) });
    // The offset is a C int (blob.c clinic); larger Python ints
    // overflow before any range logic runs.
    let offset = match args.get(1) {
        Some(v @ (Object::Int(_) | Object::Long(_) | Object::Bool(_))) => v
            .as_i64()
            .and_then(|n| i32::try_from(n).ok())
            .ok_or_else(|| {
                crate::error::overflow_error("Python int too large to convert to C int")
            })?,
        Some(other) => {
            return Err(type_error(format!(
                "'{}' object cannot be interpreted as an integer",
                other.type_name_owned()
            )))
        }
        None => return Err(type_error("seek() requires an int offset")),
    };
    let origin = args.get(2).and_then(Object::as_i64).unwrap_or(0);
    let base = match origin {
        0 => 0,
        1 => st.borrow().offset,
        2 => total,
        _ => {
            return Err(value_error(
                "'origin' should be os.SEEK_SET, os.SEEK_CUR, or os.SEEK_END",
            ))
        }
    };
    // `base + offset` is computed in C int arithmetic with an explicit
    // overflow check ("seek offset results in overflow", blob.c).
    let new = i32::try_from(base)
        .ok()
        .and_then(|b| b.checked_add(offset))
        .ok_or_else(|| crate::error::overflow_error("seek offset results in overflow"))?;
    let new = i64::from(new);
    if new < 0 || new > total {
        return Err(value_error("offset out of blob range"));
    }
    st.borrow_mut().offset = new;
    Ok(Object::None)
}

fn blob_tell(args: &[Object]) -> Result<Object, RuntimeError> {
    let st = blob_state(args)?;
    open_blob(&st)?;
    let v = st.borrow().offset;
    Ok(Object::Int(v))
}

/// Read `n` bytes at absolute `offset` (does not move the seek offset —
/// the subscript protocol is position-independent, blob.c `subscript`).
fn blob_read_at(
    blob: *mut ffi::sqlite3_blob,
    offset: i64,
    n: usize,
) -> Result<Vec<u8>, RuntimeError> {
    let mut buf = vec![0u8; n];
    if n > 0 {
        // SAFETY: caller checked offset+n against the blob length.
        let rc = unsafe {
            ffi::sqlite3_blob_read(
                blob,
                buf.as_mut_ptr().cast::<std::os::raw::c_void>(),
                n as c_int,
                offset as c_int,
            )
        };
        if rc != ffi::SQLITE_OK {
            return Err(raise_sqlite_error(std::ptr::null_mut(), rc));
        }
    }
    Ok(buf)
}

fn blob_write_at(
    blob: *mut ffi::sqlite3_blob,
    offset: i64,
    data: &[u8],
) -> Result<(), RuntimeError> {
    if data.is_empty() {
        return Ok(());
    }
    // SAFETY: caller checked the range; buffer valid for len bytes.
    let rc = unsafe {
        ffi::sqlite3_blob_write(
            blob,
            data.as_ptr().cast::<std::os::raw::c_void>(),
            data.len() as c_int,
            offset as c_int,
        )
    };
    if rc != ffi::SQLITE_OK {
        return Err(raise_sqlite_error(std::ptr::null_mut(), rc));
    }
    Ok(())
}

/// `slice.indices(len)` through the VM: (start, stop, step) with the
/// CPython "slice indices must be integers" TypeError for bad members.
fn slice_indices(key: &Object, len: i64) -> Result<(i64, i64, i64), RuntimeError> {
    let ip = interp()?;
    let indices = ip.load_attr_public(key, "indices")?;
    // ValueError ("slice step cannot be zero") propagates untouched;
    // only a bad index *type* is rewritten to the canonical message.
    let t = super::call(ip, &indices, &[Object::Int(len)]).map_err(|e| {
        if super::is_type_error(&e) {
            type_error("slice indices must be integers or None or have an __index__ method")
        } else {
            e
        }
    })?;
    if let Object::Tuple(items) = &t {
        if let (Some(a), Some(b), Some(c)) = (
            items.first().and_then(Object::as_i64),
            items.get(1).and_then(Object::as_i64),
            items.get(2).and_then(Object::as_i64),
        ) {
            return Ok((a, b, c));
        }
    }
    Err(type_error(
        "slice indices must be integers or None or have an __index__ method",
    ))
}

fn slice_len(start: i64, stop: i64, step: i64) -> i64 {
    if step > 0 {
        ((stop - start) + (step - 1)).max(0) / step
    } else {
        ((start - stop) + (-step - 1)).max(0) / -step
    }
}

fn blob_index(key: &Object, total: i64) -> Result<i64, RuntimeError> {
    let idx = match key {
        Object::Int(_) | Object::Long(_) | Object::Bool(_) => key
            .as_i64()
            .ok_or_else(|| index_error("cannot fit 'int' into an index-sized integer"))?,
        other => {
            return Err(type_error(format!(
                "indices must be integers, not {}",
                other.type_name_owned()
            )))
        }
    };
    let real = if idx < 0 { idx + total } else { idx };
    if real < 0 || real >= total {
        return Err(index_error("Blob index out of range"));
    }
    Ok(real)
}

fn blob_getitem(args: &[Object]) -> Result<Object, RuntimeError> {
    let st = blob_state(args)?;
    let blob = open_blob(&st)?;
    let key = args
        .get(1)
        .ok_or_else(|| type_error("Blob.__getitem__ requires an index"))?;
    // SAFETY: live blob handle.
    let total = i64::from(unsafe { ffi::sqlite3_blob_bytes(blob) });
    if let Object::Slice(_) = key {
        let (start, stop, step) = slice_indices(key, total)?;
        let n = slice_len(start, stop, step);
        if n == 0 {
            return Ok(Object::new_bytes(Vec::new()));
        }
        if step == 1 {
            return Ok(Object::new_bytes(blob_read_at(blob, start, n as usize)?));
        }
        // Strided: read the covering span once, then pick.
        let (lo, hi) = if step > 0 {
            (start, stop)
        } else {
            (stop + 1, start + 1)
        };
        let span = blob_read_at(blob, lo, (hi - lo).max(0) as usize)?;
        let mut out = Vec::with_capacity(n as usize);
        let mut i = start;
        for _ in 0..n {
            out.push(span[(i - lo) as usize]);
            i += step;
        }
        return Ok(Object::new_bytes(out));
    }
    let idx = blob_index(key, total)?;
    let byte = blob_read_at(blob, idx, 1)?;
    Ok(Object::Int(i64::from(byte[0])))
}

fn blob_setitem(args: &[Object]) -> Result<Object, RuntimeError> {
    let st = blob_state(args)?;
    let blob = open_blob(&st)?;
    let key = args
        .get(1)
        .ok_or_else(|| type_error("Blob.__setitem__ requires an index"))?;
    let value = args
        .get(2)
        .ok_or_else(|| type_error("Blob.__setitem__ requires a value"))?;
    // SAFETY: live blob handle.
    let total = i64::from(unsafe { ffi::sqlite3_blob_bytes(blob) });
    if let Object::Slice(_) = key {
        let (start, stop, step) = slice_indices(key, total)?;
        let n = slice_len(start, stop, step);
        let data = match super::buffer_bytes(value) {
            Some(bytes) => bytes?,
            None => buffer_via_vm(value)?,
        };
        if data.len() as i64 != n {
            return Err(index_error("Blob slice assignment is wrong size"));
        }
        if step == 1 {
            blob_write_at(blob, start, &data)?;
        } else {
            let mut i = start;
            for byte in &data {
                blob_write_at(blob, i, std::slice::from_ref(byte))?;
                i += step;
            }
        }
        return Ok(Object::None);
    }
    let idx = blob_index(key, total)?;
    let byte = match value {
        Object::Int(_) | Object::Long(_) | Object::Bool(_) => {
            // Out-of-range *and* overflowing values are both ValueError
            // (blob.c inner_write remaps OverflowError).
            match value.as_i64() {
                Some(v @ 0..=255) => v as u8,
                _ => return Err(value_error("byte must be in range(0, 256)")),
            }
        }
        other => {
            return Err(type_error(format!(
                "'{}' object cannot be interpreted as an integer",
                other.type_name_owned()
            )))
        }
    };
    blob_write_at(blob, idx, &[byte])?;
    Ok(Object::None)
}

/// Arbitrary buffer-protocol objects (`array.array`, third-party
/// buffers) are drained through the VM: `bytes(memoryview(obj))`.
/// Anything memoryview rejects gets CPython's bytes-like TypeError.
fn buffer_via_vm(value: &Object) -> Result<Vec<u8>, RuntimeError> {
    let ip = interp()?;
    let not_bytes = || {
        type_error(format!(
            "expected a bytes-like object, not {}",
            value.type_name_owned()
        ))
    };
    fn builtin(ip: &crate::Interpreter, name: &'static str) -> Option<Object> {
        ip.builtins_dict()
            .borrow()
            .get(&DictKey(Object::from_static(name)))
            .cloned()
    }
    let mv_cls = builtin(ip, "memoryview").ok_or_else(not_bytes)?;
    let mv = super::call(ip, &mv_cls, std::slice::from_ref(value)).map_err(|_| not_bytes())?;
    if let Some(bytes) = super::buffer_bytes(&mv) {
        return bytes;
    }
    let bytes_cls = builtin(ip, "bytes").ok_or_else(not_bytes)?;
    match super::call(ip, &bytes_cls, &[mv])? {
        Object::Bytes(b) => Ok(b.to_vec()),
        _ => Err(not_bytes()),
    }
}

fn blob_delitem(_args: &[Object]) -> Result<Object, RuntimeError> {
    Err(type_error("Blob doesn't support deletion"))
}

fn blob_disallow_init(_args: &[Object]) -> Result<Object, RuntimeError> {
    Err(type_error("cannot create 'sqlite3.Blob' instances"))
}

fn blob_close(args: &[Object]) -> Result<Object, RuntimeError> {
    let st = blob_state(args)?;
    // Closing an already-closed blob is an error (blob.c close goes
    // through pysqlite_check_blob).
    open_blob(&st)?;
    let ptr = { std::mem::take(&mut st.borrow_mut().ptr) };
    if ptr != 0 {
        // SAFETY: closed exactly once (ptr zeroed above).
        unsafe { ffi::sqlite3_blob_close(ptr as *mut ffi::sqlite3_blob) };
    }
    Ok(Object::None)
}

fn blob_enter(args: &[Object]) -> Result<Object, RuntimeError> {
    let st = blob_state(args)?;
    open_blob(&st)?;
    Ok(args.first().cloned().unwrap_or(Object::None))
}

fn blob_exit(args: &[Object]) -> Result<Object, RuntimeError> {
    blob_close(&args[..1])?;
    Ok(Object::Bool(false))
}

fn blob_del(args: &[Object]) -> Result<Object, RuntimeError> {
    if let Ok(st) = raw_blob_state(args) {
        let ptr = { std::mem::take(&mut st.borrow_mut().ptr) };
        if ptr != 0 {
            // SAFETY: closed exactly once.
            unsafe { ffi::sqlite3_blob_close(ptr as *mut ffi::sqlite3_blob) };
        }
    }
    Ok(Object::None)
}

// Silence unused-import warnings if the operational error path is
// compiled out on some platform.
#[allow(dead_code)]
fn _touch_operational() -> Rc<TypeObject> {
    operational_error_class()
}

//! Faithful `tp_new` slots for WeavePy's exported built-in types
//! (RFC 0046, wave 4).
//!
//! WeavePy materialises Python `float`/`int`/`str`/… values as native VM
//! [`Object`]s, so the static `PyTypeObject`s it hands to C extensions
//! (`PyFloat_Type`, `PyUnicode_Type`, …) historically carried **no
//! `tp_new`**. That is invisible to most extensions, which build values
//! through `PyFloat_FromDouble`/`PyUnicode_FromString`/… — but a C type
//! that *subclasses* one of these builtins inherits, and may directly
//! call, the base's `tp_new`.
//!
//! NumPy is the motivating case. Its scalar types that subclass a Python
//! builtin — `numpy.float64 ← float`, `numpy.str_ ← str`,
//! `numpy.bytes_ ← bytes` — compile to a generated `<base>_arrtype_new`
//! whose fast path is literally
//!
//! ```c
//! robj = PyFloat_Type->tp_new(subtype, args, kwds);  // float.__new__
//! if (robj != NULL) return robj;
//! ```
//!
//! With a NULL slot that is a call through address `0`: `np.float64(1.0)`
//! — and NumPy's own import-time `_sanity_check()` / `_mac_os_check()` —
//! die with `SIGSEGV` at `pc = 0`.
//!
//! Each constructor here mirrors CPython's `<type>_new` /
//! `<type>_subtype_new`: for the **exact** built-in it returns a native VM
//! value; for a **subtype** it allocates a faithful inline body via the
//! subtype's `tp_alloc` (RFC 0045) and writes the payload at the
//! CPython-compatible offset, so the object handed back is byte-identical
//! to what a stock interpreter would produce.

use std::os::raw::{c_char, c_void};

use weavepy_vm::object::Object;

use crate::object::{clone_object, PyObject, PySsizeT};
use crate::types::PyTypeObject;

/// `allocfunc` — `PyObject *(*)(PyTypeObject *, Py_ssize_t)`.
type AllocFunc = unsafe extern "C" fn(*mut PyTypeObject, PySsizeT) -> *mut PyObject;

/// Borrow the single positional argument from a `tp_new` `args` tuple, or
/// `None` for a zero-argument call. The reference is **borrowed** (no
/// refcount change), matching `PyTuple_GetItem`.
unsafe fn single_arg(args: *mut PyObject) -> Option<*mut PyObject> {
    if args.is_null() {
        return None;
    }
    let n = unsafe { crate::containers::PyTuple_Size(args) };
    if n <= 0 {
        return None;
    }
    let item = unsafe { crate::containers::PyTuple_GetItem(args, 0) };
    if item.is_null() {
        None
    } else {
        Some(item)
    }
}

/// Allocate a faithful subtype instance through `ty`'s `tp_alloc`
/// (defaulting to the generic allocator), reserving `nitems` items behind
/// the header for a variable-sized type.
unsafe fn subtype_alloc(ty: *mut PyTypeObject, nitems: PySsizeT) -> *mut PyObject {
    let alloc = unsafe { (*ty).tp_alloc };
    if alloc.is_null() {
        unsafe { crate::genericalloc::PyType_GenericAlloc(ty, nitems) }
    } else {
        let f: AllocFunc = unsafe { std::mem::transmute::<*mut c_void, AllocFunc>(alloc) };
        unsafe { f(ty, nitems) }
    }
}

/// True iff `ty` is the exact exported static type `slot` (pointer
/// identity), i.e. not a subclass.
unsafe fn is_exact(ty: *mut PyTypeObject, slot: &crate::types::StaticType) -> bool {
    std::ptr::eq(
        ty as *const PyTypeObject,
        slot.as_ptr() as *const PyTypeObject,
    )
}

// ====================================================================
// float
// ====================================================================

/// `float.__new__(type, x=0.0)` — RFC 0046, wave 4.
///
/// For the exact `float` type returns a native [`Object::Float`]; for a
/// subtype (e.g. `numpy.float64`) allocates the faithful body and writes
/// the `double` at `offsetof(PyFloatObject, ob_fval) == 16`, mirroring
/// CPython's `float_subtype_new`.
pub unsafe extern "C" fn float_new(
    ty: *mut PyTypeObject,
    args: *mut PyObject,
    _kwds: *mut PyObject,
) -> *mut PyObject {
    let value = match unsafe { float_value(args) } {
        Ok(v) => v,
        Err(()) => return std::ptr::null_mut(),
    };
    if unsafe { is_exact(ty, &crate::types::PyFloat_Type) } {
        return crate::object::into_owned(Object::Float(value));
    }
    let obj = unsafe { subtype_alloc(ty, 0) };
    if obj.is_null() {
        return std::ptr::null_mut();
    }
    // `PyFloatObject.ob_fval` is at offset 16 (asserted in `layout.rs`); a
    // `float` subtype is layout-compatible and inherits that slot.
    unsafe {
        *((obj as *mut u8).add(16) as *mut f64) = value;
    }
    obj
}

/// Resolve the `double` a `float(...)` call would produce from its
/// optional single argument, mirroring CPython's `float_new_impl`
/// (numeric coercion + string parse). Returns `Err` with a pending
/// exception set on failure.
unsafe fn float_value(args: *mut PyObject) -> Result<f64, ()> {
    let Some(item) = (unsafe { single_arg(args) }) else {
        return Ok(0.0);
    };
    match unsafe { clone_object(item) } {
        Object::Float(f) => Ok(f),
        Object::Str(s) => parse_py_float(&s),
        // `int`/`bool`/`bignum`/`__float__`/`__index__` all coerce through
        // `PyFloat_AsDouble` (which sets the exception on failure).
        _ => {
            let v = unsafe { crate::numbers::PyFloat_AsDouble(item) };
            if v == -1.0 && crate::errors::pending().is_some() {
                Err(())
            } else {
                Ok(v)
            }
        }
    }
}

/// Parse a Python `float(str)` literal: surrounding whitespace, an
/// optional sign, `inf`/`infinity`/`nan` (case-insensitive), and single
/// underscores between digits are accepted. Lenient on underscore
/// placement (NumPy never emits such strings); raises `ValueError`
/// otherwise.
fn parse_py_float(s: &str) -> Result<f64, ()> {
    let trimmed = s.trim();
    let cleaned: String = trimmed.chars().filter(|&c| c != '_').collect();
    let lower = cleaned.to_ascii_lowercase();
    let parsed = match lower.as_str() {
        "inf" | "+inf" | "infinity" | "+infinity" => Some(f64::INFINITY),
        "-inf" | "-infinity" => Some(f64::NEG_INFINITY),
        "nan" | "+nan" | "-nan" => Some(f64::NAN),
        _ => cleaned.parse::<f64>().ok(),
    };
    match parsed {
        Some(v) => Ok(v),
        None => {
            crate::errors::set_value_error(format!("could not convert string to float: '{s}'"));
            Err(())
        }
    }
}

// ====================================================================
// str
// ====================================================================

/// Borrow a positional or keyword argument from a `tp_new` `(args, kwds)`
/// pair: positional index `i` first, falling back to the keyword whose
/// (NUL-terminated) name is `kw`. Borrowed (no refcount change).
unsafe fn arg_or_kw(
    args: *mut PyObject,
    kwds: *mut PyObject,
    i: PySsizeT,
    kw: &[u8],
) -> Option<*mut PyObject> {
    let nargs = if args.is_null() {
        0
    } else {
        unsafe { crate::containers::PyTuple_Size(args) }.max(0)
    };
    if i < nargs {
        let it = unsafe { crate::containers::PyTuple_GetItem(args, i) };
        if !it.is_null() {
            return Some(it);
        }
    }
    if !kwds.is_null() {
        let v = unsafe { crate::containers::PyDict_GetItemString(kwds, kw.as_ptr() as *const c_char) };
        if !v.is_null() {
            return Some(v);
        }
    }
    None
}

/// Resolve the `str` value a `str(...)` call would produce from its
/// `tp_new` `(args, kwds)`, mirroring CPython's `unicode_new` /
/// `unicode_new_impl` (`str(object='', encoding=…, errors=…)`):
///
/// * no `object`              → the empty string,
/// * `object` only           → `str(object)` (`PyObject_Str`),
/// * `object` + encoding/errors → `object.decode(encoding, errors)`.
///
/// Returns `Err` with a pending exception on failure.
unsafe fn str_value(args: *mut PyObject, kwds: *mut PyObject) -> Result<String, ()> {
    let object = unsafe { arg_or_kw(args, kwds, 0, b"object\0") };
    let encoding = unsafe { arg_or_kw(args, kwds, 1, b"encoding\0") };
    let errors = unsafe { arg_or_kw(args, kwds, 2, b"errors\0") };

    let Some(object) = object else {
        return Ok(String::new());
    };

    let result = if encoding.is_none() && errors.is_none() {
        unsafe { crate::abstract_::PyObject_Str(object) }
    } else {
        // `str(bytes-like, encoding, errors)` — decode. The codec names
        // default to CPython's (`utf-8` / `strict`) when omitted.
        let enc = encoding
            .map(|e| unsafe { crate::strings::PyUnicode_AsUTF8(e) })
            .filter(|p| !p.is_null())
            .unwrap_or(b"utf-8\0".as_ptr() as *const c_char);
        let err = errors
            .map(|e| unsafe { crate::strings::PyUnicode_AsUTF8(e) })
            .filter(|p| !p.is_null())
            .unwrap_or(b"strict\0".as_ptr() as *const c_char);
        unsafe { crate::strings::PyUnicode_FromEncodedObject(object, enc, err) }
    };
    if result.is_null() {
        return Err(());
    }
    let out = match unsafe { clone_object(result) } {
        Object::Str(t) => t.to_string(),
        _ => String::new(),
    };
    unsafe { crate::object::Py_DecRef(result) };
    Ok(out)
}

/// Write one PEP 393 code point of the given `kind` (1/2/4 bytes) into
/// `data[i]`.
///
/// # Safety
/// `data` must address a writable buffer with room for `i + 1` units.
#[inline]
unsafe fn write_codepoint(data: *mut u8, kind: u32, i: usize, cp: u32) {
    match kind {
        1 => unsafe { *data.add(i) = cp as u8 },
        2 => unsafe { *(data as *mut u16).add(i) = cp as u16 },
        _ => unsafe { *(data as *mut u32).add(i) = cp },
    }
}

/// Build a faithful `str` **subtype** instance for `value`, mirroring
/// CPython's `unicode_subtype_new`: allocate the subtype body through its
/// `tp_alloc` (the faithful inline body, RFC 0045) and populate a
/// **legacy / non-compact** `PyUnicodeObject` — the character data lives
/// in a separately allocated buffer reached through `data.any` (offset 56),
/// not inline, because a subtype's fixed `tp_basicsize` body has no room
/// for it (numpy's `PyUnicodeScalarObject` packs `obval`/`buffer_fmt`
/// right after the unicode base). A stock reader resolves the buffer via
/// `PyUnicode_DATA`'s non-compact branch, so the result is byte-identical
/// to what `numpy.str_.__new__` expects back from `PyUnicode_Type.tp_new`.
unsafe fn unicode_subtype_new(ty: *mut PyTypeObject, value: &str) -> *mut PyObject {
    let chars: Vec<u32> = value.chars().map(|c| c as u32).collect();
    let length = chars.len();
    let maxchar = chars.iter().copied().max().unwrap_or(0);
    let (kind, ascii, char_size): (u32, bool, usize) = if maxchar < 0x80 {
        (crate::layout::ustate::KIND_1BYTE, true, 1)
    } else if maxchar < 0x100 {
        (crate::layout::ustate::KIND_1BYTE, false, 1)
    } else if maxchar < 0x1_0000 {
        (crate::layout::ustate::KIND_2BYTE, false, 2)
    } else {
        (crate::layout::ustate::KIND_4BYTE, false, 4)
    };

    let obj = unsafe { subtype_alloc(ty, 0) };
    if obj.is_null() {
        return std::ptr::null_mut();
    }
    // One extra code unit for the NUL terminator CPython always keeps.
    let nbytes = (length + 1) * char_size;
    let data = unsafe { crate::memory::PyMem_Malloc(nbytes) } as *mut u8;
    if data.is_null() {
        unsafe { crate::object::Py_DecRef(obj) };
        unsafe { crate::errors::PyErr_NoMemory() };
        return std::ptr::null_mut();
    }
    unsafe {
        std::ptr::write_bytes(data, 0, nbytes);
        for (i, &cp) in chars.iter().enumerate() {
            write_codepoint(data, kind, i, cp);
        }
        // PyASCIIObject head: length / hash(-1, unhashed) / state.
        let ao = obj as *mut crate::layout::PyASCIIObject;
        (*ao).length = length as PySsizeT;
        (*ao).hash = -1;
        (*ao).state = crate::layout::ustate::pack(0, kind, false, ascii, false);
        // PyCompactUnicodeObject head: UTF-8 cache left empty (computed
        // lazily by `PyUnicode_AsUTF8`).
        let co = obj as *mut crate::layout::PyCompactUnicodeObject;
        (*co).utf8 = std::ptr::null_mut();
        (*co).utf8_length = 0;
        // PyUnicodeObject `data.any` → the out-of-line character buffer.
        let uo = obj as *mut crate::layout::PyUnicodeObject;
        (*uo).data = data as *mut c_void;
    }
    obj
}

/// `str.__new__(type, object='', encoding=…, errors=…)` — RFC 0046.
///
/// For the exact `str` type returns a native [`Object::Str`]; for a
/// subtype (e.g. `numpy.str_`) builds the faithful legacy unicode body via
/// [`unicode_subtype_new`], mirroring CPython's `unicode_new`. NumPy's
/// `unicode_arrtype_new` calls this slot directly (`PyUnicode_Type.tp_new`)
/// to let `str` do the value conversion before stamping its own scalar
/// fields, so a NULL slot SIGSEGV'd on `np.str_(...)` / `arr.astype(str)`.
pub unsafe extern "C" fn str_new(
    ty: *mut PyTypeObject,
    args: *mut PyObject,
    kwds: *mut PyObject,
) -> *mut PyObject {
    let value = match unsafe { str_value(args, kwds) } {
        Ok(v) => v,
        Err(()) => return std::ptr::null_mut(),
    };
    if unsafe { is_exact(ty, &crate::types::PyUnicode_Type) } {
        return crate::object::into_owned(Object::from_str(value));
    }
    unsafe { unicode_subtype_new(ty, &value) }
}

// ====================================================================
// bytes
// ====================================================================

/// Build the native `bytes` value a `bytes(...)` call would produce from
/// the `tp_new` `(args, kwds)`, mirroring CPython's `bytes_new` argument
/// handling (`bytes(source=b'', encoding=…, errors=…)`):
///
/// * no `source`                     → the empty byte string,
/// * `source` str + `encoding`       → `source.encode(encoding, errors)`,
/// * `source` str without `encoding` → `TypeError`,
/// * any other `source`              → `PyBytes_FromObject` (a bytes-like
///   object, or an iterable of ints).
///
/// Returns an **owned** native `bytes` `PyObject*` (for a bytes-like
/// source, a new reference to an equal value), or NULL with a pending
/// exception on failure.
unsafe fn bytes_value_obj(args: *mut PyObject, kwds: *mut PyObject) -> *mut PyObject {
    let source = unsafe { arg_or_kw(args, kwds, 0, b"source\0") };
    let encoding = unsafe { arg_or_kw(args, kwds, 1, b"encoding\0") };
    let errors = unsafe { arg_or_kw(args, kwds, 2, b"errors\0") };

    let Some(source) = source else {
        if encoding.is_some() || errors.is_some() {
            crate::errors::set_type_error("encoding or errors without a string argument");
            return std::ptr::null_mut();
        }
        return unsafe { crate::strings::PyBytes_FromStringAndSize(std::ptr::null(), 0) };
    };

    // A `str` source (including numpy's `str_`, which clones to `Object::Str`)
    // must be encoded and *requires* an encoding, matching CPython.
    if matches!(unsafe { clone_object(source) }, Object::Str(_)) {
        if encoding.is_none() {
            crate::errors::set_type_error("string argument without an encoding");
            return std::ptr::null_mut();
        }
        // The codec/errors names are accepted; the current codec layer
        // resolves them to UTF-8 (see `PyUnicode_AsEncodedString`).
        let _ = errors;
        return unsafe {
            crate::strings::PyUnicode_AsEncodedString(source, std::ptr::null(), std::ptr::null())
        };
    }
    if encoding.is_some() || errors.is_some() {
        crate::errors::set_type_error("encoding or errors without a string argument");
        return std::ptr::null_mut();
    }
    unsafe { crate::strings::PyBytes_FromObject(source) }
}

/// `bytes.__new__(type, source=b'', encoding=…, errors=…)` — RFC 0046.
///
/// For the exact `bytes` type returns the native [`Object::Bytes`] value;
/// for a subtype (e.g. `numpy.bytes_`) builds the faithful variable-length
/// `PyBytesObject` body, mirroring CPython's `bytes_subtype_new`: allocate
/// `n` items through the subtype's `tp_alloc` (the faithful inline body,
/// RFC 0045) and write `ob_size` / `ob_shash` / the inline `ob_sval` char
/// array (with its trailing NUL) at the CPython offsets, so a stock reader
/// (`PyBytes_AS_STRING` / `PyBytes_GET_SIZE`) sees a real bytes object.
///
/// NumPy's `string_arrtype_new` calls this slot directly
/// (`PyBytes_Type.tp_new`) to let `bytes` do the value conversion before
/// stamping its own scalar fields, so a NULL slot SIGSEGV'd on
/// `np.bytes_(...)` / `arr.astype("S")`.
pub unsafe extern "C" fn bytes_new(
    ty: *mut PyTypeObject,
    args: *mut PyObject,
    kwds: *mut PyObject,
) -> *mut PyObject {
    let value_obj = unsafe { bytes_value_obj(args, kwds) };
    if value_obj.is_null() {
        return std::ptr::null_mut();
    }
    if unsafe { is_exact(ty, &crate::types::PyBytes_Type) } {
        return value_obj;
    }
    // Snapshot the raw bytes, then drop the temporary native `bytes`.
    let data: Vec<u8> = match unsafe { clone_object(value_obj) } {
        Object::Bytes(b) => b.to_vec(),
        _ => Vec::new(),
    };
    unsafe { crate::object::Py_DecRef(value_obj) };

    let n = data.len() as PySsizeT;
    let obj = unsafe { subtype_alloc(ty, n) };
    if obj.is_null() {
        return std::ptr::null_mut();
    }
    // Faithful `PyBytesObject` body: `ob_size` (16), `ob_shash` (24,
    // unhashed sentinel `-1`), inline `ob_sval` (32) + trailing NUL.
    unsafe {
        let bo = obj as *mut crate::layout::PyBytesObject;
        (*bo).ob_base.ob_size = n;
        (*bo).ob_shash = -1;
        let sval = (*bo).ob_sval.as_mut_ptr() as *mut u8;
        std::ptr::copy_nonoverlapping(data.as_ptr(), sval, data.len());
        *sval.add(data.len()) = 0;
    }
    obj
}

// ====================================================================
// Installation
// ====================================================================

/// Wire the faithful `tp_new` slots onto the exported static built-ins.
/// Called from [`crate::types::init_static_types`] after the type table
/// is populated. Idempotent (writes the same pointers each time).
pub fn install_builtin_constructors() {
    unsafe {
        let fnew: unsafe extern "C" fn(
            *mut PyTypeObject,
            *mut PyObject,
            *mut PyObject,
        ) -> *mut PyObject = float_new;
        (*crate::types::PyFloat_Type.as_ptr()).tp_new = fnew as *mut c_void;
        let snew: unsafe extern "C" fn(
            *mut PyTypeObject,
            *mut PyObject,
            *mut PyObject,
        ) -> *mut PyObject = str_new;
        (*crate::types::PyUnicode_Type.as_ptr()).tp_new = snew as *mut c_void;
        let bnew: unsafe extern "C" fn(
            *mut PyTypeObject,
            *mut PyObject,
            *mut PyObject,
        ) -> *mut PyObject = bytes_new;
        (*crate::types::PyBytes_Type.as_ptr()).tp_new = bnew as *mut c_void;
    }
}

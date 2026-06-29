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

use std::os::raw::c_void;

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
    }
}

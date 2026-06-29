//! Statically-allocated singleton objects (`Py_None`, `Py_True`,
//! `Py_False`, `Py_NotImplemented`, `Py_Ellipsis`).
//!
//! Each one is exposed as a `static` symbol with the
//! CPython-canonical name (`_Py_NoneStruct`, etc.) so the macros in
//! `Python.h` (`#define Py_None (&_Py_NoneStruct)`) work
//! unchanged.
//!
//! All five carry the [`IMMORTAL_REFCNT`] sentinel: refcount
//! mutations are no-ops on them, and dereferences in C-side macros
//! (`Py_REFCNT(Py_None)` etc.) behave correctly because the field
//! layout matches `struct _object` in the header.

use std::cell::UnsafeCell;
use std::sync::atomic::{AtomicPtr, Ordering};

use crate::object::{PyObject, IMMORTAL_REFCNT};
use crate::types::PyTypeObject;

/// Wrapper that lets us declare a `static` `PyObject` without
/// triggering the `static_mut_refs` lint while still giving C code
/// a stable address.
#[repr(transparent)]
pub struct Singleton(pub UnsafeCell<PyObject>);

unsafe impl Sync for Singleton {}

impl Singleton {
    pub const fn new() -> Self {
        Self(UnsafeCell::new(PyObject {
            ob_refcnt: IMMORTAL_REFCNT,
            ob_type: std::ptr::null_mut(),
        }))
    }

    /// Return a stable raw pointer suitable for handing to C.
    pub fn as_ptr(&self) -> *mut PyObject {
        self.0.get()
    }
}

/// `Py_True`/`Py_False` are faithful `PyLongObject` singletons.
///
/// CPython's `bool` is an `int` subclass and `_Py_TrueStruct` /
/// `_Py_FalseStruct` are `struct _longobject` (a `PyLongObject`), not bare
/// `PyObject`s. RFC 0043/0047: macro-heavy Cython converts a Python bool to a
/// C integer by reading the `PyLongObject` digits and sign tag *directly* off
/// the struct (`__Pyx_PyLong_IsCompact` / `__Pyx_PyLong_CompactValue` under
/// `CYTHON_USE_PYLONG_INTERNALS`) rather than calling `PyLong_AsLong`. pandas'
/// `lib.maybe_convert_objects` stores each bool into an `ndarray[uint8]`
/// (`bools[i] = val`), which compiles to exactly that inline read; a bare
/// 16-byte `PyObject` left the `lv_tag`/`ob_digit` slots reading past the
/// allocation, so `False` decoded as a "negative value" and the store raised
/// `OverflowError: can't convert negative value to npy_uint8`. Backing the
/// singletons with a real `_PyLongValue` makes the inline decode read `0`/`1`.
#[repr(transparent)]
pub struct BoolSingleton(UnsafeCell<crate::layout::PyLongObject>);

unsafe impl Sync for BoolSingleton {}

impl BoolSingleton {
    /// `value` is `0` (False) or `1` (True).
    pub const fn new(value: crate::layout::digit) -> Self {
        // `lv_tag = (ndigits << NON_SIZE_BITS) | sign`. False is the canonical
        // zero (0 digits, SIGN_ZERO); True is one positive digit.
        let lv_tag = if value == 0 {
            crate::layout::PYLONG_SIGN_ZERO
        } else {
            (1usize << crate::layout::PYLONG_NON_SIZE_BITS) | crate::layout::PYLONG_SIGN_POSITIVE
        };
        Self(UnsafeCell::new(crate::layout::PyLongObject {
            ob_base: PyObject {
                ob_refcnt: IMMORTAL_REFCNT,
                ob_type: std::ptr::null_mut(),
            },
            long_value: crate::layout::PyLongValue {
                lv_tag,
                ob_digit: [value],
            },
        }))
    }

    /// Return a stable raw `PyObject*` (the embedded `ob_base`).
    pub fn as_ptr(&self) -> *mut PyObject {
        self.0.get() as *mut PyObject
    }
}

#[no_mangle]
pub static _Py_NoneStruct: Singleton = Singleton::new();

#[no_mangle]
pub static _Py_TrueStruct: BoolSingleton = BoolSingleton::new(1);

#[no_mangle]
pub static _Py_FalseStruct: BoolSingleton = BoolSingleton::new(0);

#[no_mangle]
pub static _Py_NotImplementedStruct: Singleton = Singleton::new();

#[no_mangle]
pub static _Py_EllipsisObject: Singleton = Singleton::new();

/// Initialise the `ob_type` slot of every singleton. Called once
/// at runtime by [`crate::interp::ensure_initialised`] so the
/// types pointed at are the real bridge types rather than null.
///
/// SAFETY: this writes through the `static`s' `UnsafeCell`. It
/// must be called before any C extension dereferences a singleton's
/// `ob_type`. Calling it more than once is harmless.
pub fn init_singleton_types(
    none_ty: *mut PyTypeObject,
    bool_ty: *mut PyTypeObject,
    not_impl_ty: *mut PyTypeObject,
    ellipsis_ty: *mut PyTypeObject,
) {
    unsafe {
        (*_Py_NoneStruct.as_ptr()).ob_type = none_ty;
        (*_Py_TrueStruct.as_ptr()).ob_type = bool_ty;
        (*_Py_FalseStruct.as_ptr()).ob_type = bool_ty;
        (*_Py_NotImplementedStruct.as_ptr()).ob_type = not_impl_ty;
        (*_Py_EllipsisObject.as_ptr()).ob_type = ellipsis_ty;
    }
}

/// Pointer cell used by call sites that need the singleton address
/// without dereferencing through the `Singleton` wrapper.
pub fn none_ptr() -> *mut PyObject {
    _Py_NoneStruct.as_ptr()
}

pub fn true_ptr() -> *mut PyObject {
    _Py_TrueStruct.as_ptr()
}

pub fn false_ptr() -> *mut PyObject {
    _Py_FalseStruct.as_ptr()
}

pub fn not_implemented_ptr() -> *mut PyObject {
    _Py_NotImplementedStruct.as_ptr()
}

pub fn ellipsis_ptr() -> *mut PyObject {
    _Py_EllipsisObject.as_ptr()
}

/// Bridge cell used by the import path to publish the running
/// interpreter to extension code. Initialised by
/// [`crate::interp::activate`] and cleared after the call returns.
pub static CURRENT_INTERPRETER: AtomicPtr<()> = AtomicPtr::new(std::ptr::null_mut());

pub fn current_interpreter() -> *mut () {
    CURRENT_INTERPRETER.load(Ordering::Relaxed)
}

pub fn set_current_interpreter(p: *mut ()) {
    CURRENT_INTERPRETER.store(p, Ordering::Relaxed);
}

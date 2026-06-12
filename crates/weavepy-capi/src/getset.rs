//! `PyGetSetDef` / `PyMemberDef` descriptor support.
//!
//! Heap types declare per-instance attributes through two struct
//! arrays:
//!
//! - [`PyGetSetDef`]: name + getter/setter function pointers + a
//!   `void *` closure carried into both. Used for computed attributes
//!   like a `shape` property that walks an internal tuple.
//! - [`PyMemberDef`]: name + offset + type code (one of the
//!   `T_*` constants in `Python.h`). Used for plain typed fields
//!   stored at a known offset in the instance's payload.
//!
//! The decoder in this module walks both arrays at `PyType_FromSpec`
//! time and emits `(name, descriptor)` pairs to inject into the
//! type's dict, modeled as builtin getters/setters over the C ABI
//! pointers.

use std::ffi::CStr;
use std::os::raw::{c_char, c_int};

use weavepy_vm::error::{type_error, RuntimeError};
use weavepy_vm::object::{BuiltinFn, Object, PyProperty};
use weavepy_vm::sync::Rc;

use crate::object::{PyObject, PySsizeT};

fn take_pending_or_default() -> RuntimeError {
    if let Some(err) = crate::errors::take_pending_error_runtime() {
        err
    } else {
        type_error("native getter raised without setting an exception")
    }
}

/// Layout matching `PyGetSetDef` in `Python.h`. Repeated here so the
/// FromSpec machinery can decode a `void *` pointer with the right
/// element size.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct PyGetSetDef {
    pub name: *const c_char,
    pub get: Option<unsafe extern "C" fn(*mut PyObject, *mut std::ffi::c_void) -> *mut PyObject>,
    pub set:
        Option<unsafe extern "C" fn(*mut PyObject, *mut PyObject, *mut std::ffi::c_void) -> c_int>,
    pub doc: *const c_char,
    pub closure: *mut std::ffi::c_void,
}

unsafe impl Send for PyGetSetDef {}
unsafe impl Sync for PyGetSetDef {}

/// Layout matching `PyMemberDef` in `Python.h`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct PyMemberDef {
    pub name: *const c_char,
    pub ty: c_int,
    pub offset: PySsizeT,
    pub flags: c_int,
    pub doc: *const c_char,
}

unsafe impl Send for PyMemberDef {}
unsafe impl Sync for PyMemberDef {}

/// Member-type codes (mirror `Python.h`).
pub mod member_types {
    use super::c_int;
    pub const T_SHORT: c_int = 0;
    pub const T_INT: c_int = 1;
    pub const T_LONG: c_int = 2;
    pub const T_FLOAT: c_int = 3;
    pub const T_DOUBLE: c_int = 4;
    pub const T_STRING: c_int = 5;
    pub const T_OBJECT: c_int = 6;
    pub const T_CHAR: c_int = 7;
    pub const T_BYTE: c_int = 8;
    pub const T_UBYTE: c_int = 9;
    pub const T_USHORT: c_int = 10;
    pub const T_UINT: c_int = 11;
    pub const T_ULONG: c_int = 12;
    pub const T_STRING_INPLACE: c_int = 13;
    pub const T_BOOL: c_int = 14;
    pub const T_OBJECT_EX: c_int = 16;
    pub const T_LONGLONG: c_int = 17;
    pub const T_ULONGLONG: c_int = 18;
    pub const T_PYSSIZET: c_int = 19;
    pub const T_NONE: c_int = 20;
}

pub const READONLY: c_int = 1;

/// Decode a null-terminated `PyGetSetDef[]` array into `(name,
/// callable)` descriptor pairs to install into the type's dict.
pub unsafe fn collect_getsets(mut defs: *mut PyGetSetDef) -> Vec<(String, Object)> {
    let mut out = Vec::new();
    if defs.is_null() {
        return out;
    }
    loop {
        let entry = unsafe { *defs };
        if entry.name.is_null() {
            break;
        }
        let name = unsafe { CStr::from_ptr(entry.name) }
            .to_string_lossy()
            .into_owned();
        let static_name: &'static str = Box::leak(name.clone().into_boxed_str());

        // Build the property's three function slots out of the C
        // getter/setter pointers. We wrap as a real `Object::Property`
        // so the VM's descriptor protocol (data-descriptor priority,
        // automatic invocation on attribute access) kicks in. Without
        // this `instance.shape` would bind as a method and the caller
        // would have to `instance.shape()` to actually get the value.
        let fget = match entry.get {
            Some(g) => make_getter(static_name, g, entry.closure as usize),
            None => Object::None,
        };
        let fset = match entry.set {
            Some(s) => make_setter(static_name, s, entry.closure as usize),
            None => Object::None,
        };
        let prop = Object::Property(Rc::new(PyProperty::new(
            fget,
            fset,
            Object::None,
            Object::None,
        )));
        out.push((name, prop));
        defs = unsafe { defs.add(1) };
    }
    out
}

fn make_getter(
    name: &'static str,
    g: unsafe extern "C" fn(*mut PyObject, *mut std::ffi::c_void) -> *mut PyObject,
    closure: usize,
) -> Object {
    let body = move |args: &[Object]| -> Result<Object, RuntimeError> {
        if args.is_empty() {
            return Err(type_error(format!(
                "getter for '{name}' expects 1 argument"
            )));
        }
        let self_p = crate::object::into_owned(args[0].clone());
        let raw = unsafe { g(self_p, closure as *mut std::ffi::c_void) };
        unsafe { crate::object::Py_DecRef(self_p) };
        if raw.is_null() {
            return Err(take_pending_or_default());
        }
        let out = unsafe { crate::object::clone_object(raw) };
        unsafe { crate::object::Py_DecRef(raw) };
        Ok(out)
    };
    Object::Builtin(Rc::new(BuiltinFn {
        name,
        binds_instance: false,
        call: Box::new(body),
        call_kw: None,
    }))
}

fn make_setter(
    name: &'static str,
    s: unsafe extern "C" fn(*mut PyObject, *mut PyObject, *mut std::ffi::c_void) -> c_int,
    closure: usize,
) -> Object {
    let body = move |args: &[Object]| -> Result<Object, RuntimeError> {
        if args.len() != 2 {
            return Err(type_error(format!(
                "setter for '{name}' expects 2 arguments (self, value)"
            )));
        }
        let self_p = crate::object::into_owned(args[0].clone());
        let val_p = crate::object::into_owned(args[1].clone());
        let r = unsafe { s(self_p, val_p, closure as *mut std::ffi::c_void) };
        unsafe {
            crate::object::Py_DecRef(self_p);
            crate::object::Py_DecRef(val_p);
        }
        if r < 0 {
            return Err(take_pending_or_default());
        }
        Ok(Object::None)
    };
    Object::Builtin(Rc::new(BuiltinFn {
        name,
        binds_instance: false,
        call: Box::new(body),
        call_kw: None,
    }))
}

/// Decode a null-terminated `PyMemberDef[]` array into descriptor
/// pairs.
///
/// Members that mark `T_OBJECT*` simply return `None` for now; we
/// don't currently project into raw extension memory because heap
/// types backed by `PyType_FromSpec` use a `PyObjectBox` whose
/// extra storage is opaque to the runtime. Numeric members
/// (`T_INT`, `T_DOUBLE`, …) likewise return None — extensions that
/// declare them are responsible for synthesising a getset pair if
/// they want runtime access.
pub unsafe fn collect_members(mut defs: *mut PyMemberDef) -> Vec<(String, Object)> {
    let mut out = Vec::new();
    if defs.is_null() {
        return out;
    }
    loop {
        let entry = unsafe { *defs };
        if entry.name.is_null() {
            break;
        }
        let name = unsafe { CStr::from_ptr(entry.name) }
            .to_string_lossy()
            .into_owned();
        let static_name: &'static str = Box::leak(name.clone().into_boxed_str());
        let _ = entry.ty;
        let _ = entry.offset;
        let f = move |args: &[Object]| -> Result<Object, RuntimeError> {
            // For now members are read-only stubs. Extensions that
            // want full support of typed members should declare a
            // getset pair instead.
            if args.is_empty() {
                return Err(type_error(format!(
                    "attribute '{}' invocation expects self",
                    static_name
                )));
            }
            Ok(Object::None)
        };
        out.push((
            name,
            Object::Builtin(Rc::new(BuiltinFn {
                name: static_name,
                binds_instance: false,
                call: Box::new(f),
                call_kw: None,
            })),
        ));
        defs = unsafe { defs.add(1) };
    }
    out
}

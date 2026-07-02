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

fn blocks_diag_enabled() -> bool {
    use std::sync::OnceLock;
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("WEAVEPY_BLOCKS_DIAG").is_some())
}

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
        let self_body = self_p as usize;
        let raw = unsafe { g(self_p, closure as *mut std::ffi::c_void) };
        unsafe { crate::object::Py_DecRef(self_p) };
        if raw.is_null() {
            return Err(take_pending_or_default());
        }
        let out = unsafe { crate::object::clone_object(raw) };
        if name == "blocks" {
            let on = out.type_name_owned();
            if blocks_diag_enabled() && on != "tuple" && on != "list" {
                let rawty = unsafe { crate::object::debug_type_name(raw) };
                eprintln!(
                    "[BLOCKS-BAD] self_body=0x{:x} raw=0x{:x} rawtype={} out={}",
                    self_body,
                    raw as usize,
                    rawty,
                    on,
                );
                if let Some(hist) = crate::mirror::lookup_free_bt(raw as usize) {
                    eprintln!("[BLOCKS-BAD] raw 0x{:x} free history ({} frees):", raw as usize, hist.len());
                    for (i, (freed_ty, bt)) in hist.iter().enumerate() {
                        eprintln!("  free #{i}: type={freed_ty} :: {bt}");
                    }
                } else {
                    eprintln!(
                        "[BLOCKS-BAD] raw 0x{:x} has no recorded mirror-free (freed via non-mirror path?)",
                        raw as usize
                    );
                }
            }
            if crate::mirror::watch_enabled() && (on == "tuple" || on == "list") {
                crate::mirror::watch_ptr(raw as usize);
            }
        }
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

/// Decode a null-terminated `PyMemberDef[]` array into descriptor pairs.
///
/// Each member projects its declared `T_*` field, at its `offset`, in the
/// instance's **faithful inline body** (RFC 0045, wave 3) to/from a Python
/// value: `obj.field` (Python) and `self->field` (C) read and write *the
/// same bytes*. Read-only members (`READONLY` in `flags`) install a getter
/// with no setter, so assignment raises `AttributeError` via the property
/// protocol. The descriptor is a real [`Object::Property`], so data-
/// descriptor priority and automatic invocation on attribute access apply.
///
/// A member access on an object that has no faithful inline body (e.g. the
/// type is dict-backed, `tp_basicsize == sizeof(PyObject)`) reads as
/// `None` and rejects writes — the offset would not name a real field.
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
        let readonly = (entry.flags & READONLY) != 0;
        out.push((
            name,
            make_member(static_name, entry.ty, entry.offset, readonly),
        ));
        defs = unsafe { defs.add(1) };
    }
    out
}

/// Build the `Object::Property` descriptor for one `tp_members` entry
/// (RFC 0045, wave 3). The getter/setter cross `self` into C — which, for
/// an inline-storage instance, yields its stable faithful body (see
/// [`crate::object::into_owned`]) — then read/write the typed field at
/// `offset` directly in that body.
fn make_member(name: &'static str, ty: c_int, offset: PySsizeT, readonly: bool) -> Object {
    let getter = {
        let g = move |args: &[Object]| -> Result<Object, RuntimeError> {
            let self_obj = args
                .first()
                .ok_or_else(|| type_error(format!("member '{name}' getter expects self")))?;
            let self_p = crate::object::into_owned(self_obj.clone());
            // A `tp_members` field lives at a fixed offset in the instance's
            // C struct. That storage exists for two kinds of object: one of
            // *our* inline-storage instances (RFC 0045) and a *foreign*
            // object the extension allocated itself (its `PyObject*` points
            // at a real C struct laid out by the declaring type — numpy's
            // `PyArray_Descr.typeobj` is read this way). For a plain
            // dict-backed instance the offset names no real field, so it
            // reads as `None`.
            let has_field = matches!(self_obj, Object::Foreign(_))
                || unsafe { crate::mirror::is_instance_body(self_p) };
            let out = if has_field {
                unsafe { read_member(self_p, ty, offset) }
            } else {
                Object::None
            };
            unsafe { crate::object::Py_DecRef(self_p) };
            Ok(out)
        };
        Object::Builtin(Rc::new(BuiltinFn {
            name,
            binds_instance: false,
            call: Box::new(g),
            call_kw: None,
        }))
    };
    let setter = if readonly {
        Object::None
    } else {
        let s = move |args: &[Object]| -> Result<Object, RuntimeError> {
            if args.len() != 2 {
                return Err(type_error(format!(
                    "member '{name}' setter expects (self, value)"
                )));
            }
            let self_p = crate::object::into_owned(args[0].clone());
            let has_field = matches!(&args[0], Object::Foreign(_))
                || unsafe { crate::mirror::is_instance_body(self_p) };
            let res = if has_field {
                unsafe { write_member(self_p, ty, offset, &args[1]) }
            } else {
                Err(type_error(format!(
                    "cannot set member '{name}' on an object without inline storage"
                )))
            };
            unsafe { crate::object::Py_DecRef(self_p) };
            res.map(|()| Object::None)
        };
        Object::Builtin(Rc::new(BuiltinFn {
            name,
            binds_instance: false,
            call: Box::new(s),
            call_kw: None,
        }))
    };
    Object::Property(Rc::new(PyProperty::new(
        getter,
        setter,
        Object::None,
        Object::None,
    )))
}

/// Wrap a C integer of any width into the narrowest faithful `Object`
/// (`Int` when it fits in `i64`, else a big `Long`).
fn int_obj(v: i128) -> Object {
    match i64::try_from(v) {
        Ok(i) => Object::Int(i),
        Err(_) => Object::Long(Rc::new(num_bigint::BigInt::from(v))),
    }
}

/// Read the `T_*` field of `body` at `offset` into a Python value
/// (RFC 0045, wave 3). `body` must be a faithful instance body, and
/// `offset` must name a field within its `tp_basicsize` block.
// Wildcard import of the local `T_*` constant module: this `match` dispatches
// over (almost) every member-type tag, so a wildcard is clearer than a
// 20-name list.
#[allow(clippy::wildcard_imports)]
unsafe fn read_member(body: *mut PyObject, ty: c_int, offset: PySsizeT) -> Object {
    use member_types::*;
    let field = unsafe { (body as *const u8).offset(offset as isize) };
    unsafe {
        match ty {
            T_BOOL => Object::Bool(std::ptr::read_unaligned(field as *const i8) != 0),
            T_BYTE => int_obj(std::ptr::read_unaligned(field as *const i8) as i128),
            T_UBYTE => int_obj(std::ptr::read_unaligned(field as *const u8) as i128),
            T_SHORT => int_obj(std::ptr::read_unaligned(field as *const i16) as i128),
            T_USHORT => int_obj(std::ptr::read_unaligned(field as *const u16) as i128),
            T_INT => int_obj(std::ptr::read_unaligned(field as *const i32) as i128),
            T_UINT => int_obj(std::ptr::read_unaligned(field as *const u32) as i128),
            T_LONG => {
                int_obj(std::ptr::read_unaligned(field as *const std::os::raw::c_long) as i128)
            }
            T_ULONG => {
                int_obj(std::ptr::read_unaligned(field as *const std::os::raw::c_ulong) as i128)
            }
            T_LONGLONG => int_obj(std::ptr::read_unaligned(field as *const i64) as i128),
            T_ULONGLONG => int_obj(std::ptr::read_unaligned(field as *const u64) as i128),
            T_PYSSIZET => int_obj(std::ptr::read_unaligned(field as *const isize) as i128),
            T_FLOAT => Object::Float(std::ptr::read_unaligned(field as *const f32) as f64),
            T_DOUBLE => Object::Float(std::ptr::read_unaligned(field as *const f64)),
            T_CHAR => {
                let c = std::ptr::read_unaligned(field as *const u8);
                Object::Str(Rc::from((c as char).to_string().as_str()))
            }
            T_STRING => {
                let p = std::ptr::read_unaligned(field as *const *const c_char);
                if p.is_null() {
                    Object::None
                } else {
                    Object::Str(Rc::from(CStr::from_ptr(p).to_string_lossy().as_ref()))
                }
            }
            T_OBJECT | T_OBJECT_EX => {
                let p = std::ptr::read_unaligned(field as *const *mut PyObject);
                if p.is_null() {
                    Object::None
                } else {
                    crate::object::clone_object(p)
                }
            }
            _ => Object::None,
        }
    }
}

/// Coerce a Python value to `i64` for an integer member assignment.
fn obj_i64(v: &Object) -> Result<i64, RuntimeError> {
    match v {
        Object::Int(i) => Ok(*i),
        Object::Bool(b) => Ok(*b as i64),
        Object::Long(big) => {
            use num_traits::ToPrimitive;
            big.to_i64()
                .ok_or_else(|| type_error("integer too large for this member"))
        }
        _ => Err(type_error("member assignment expects an integer")),
    }
}

/// Coerce a Python value to `f64` for a floating-point member assignment.
fn obj_f64(v: &Object) -> Result<f64, RuntimeError> {
    match v {
        Object::Float(f) => Ok(*f),
        Object::Int(i) => Ok(*i as f64),
        Object::Bool(b) => Ok(*b as i64 as f64),
        _ => Err(type_error("member assignment expects a real number")),
    }
}

/// Write a Python value into the `T_*` field of `body` at `offset`
/// (RFC 0045, wave 3). Mirrors [`read_member`]; numeric and object members
/// are writable, while the borrowed-pointer members (`T_STRING`, `T_CHAR`)
/// are treated as read-only (assigning one would require owning C storage).
#[allow(clippy::wildcard_imports)]
unsafe fn write_member(
    body: *mut PyObject,
    ty: c_int,
    offset: PySsizeT,
    value: &Object,
) -> Result<(), RuntimeError> {
    use member_types::*;
    let field = unsafe { (body as *mut u8).offset(offset as isize) };
    unsafe {
        match ty {
            T_BOOL => std::ptr::write_unaligned(field as *mut i8, i8::from(obj_i64(value)? != 0)),
            T_BYTE => std::ptr::write_unaligned(field as *mut i8, obj_i64(value)? as i8),
            T_UBYTE => std::ptr::write_unaligned(field as *mut u8, obj_i64(value)? as u8),
            T_SHORT => std::ptr::write_unaligned(field as *mut i16, obj_i64(value)? as i16),
            T_USHORT => std::ptr::write_unaligned(field as *mut u16, obj_i64(value)? as u16),
            T_INT => std::ptr::write_unaligned(field as *mut i32, obj_i64(value)? as i32),
            T_UINT => std::ptr::write_unaligned(field as *mut u32, obj_i64(value)? as u32),
            T_LONG => {
                std::ptr::write_unaligned(field as *mut std::os::raw::c_long, obj_i64(value)? as _)
            }
            T_ULONG => {
                std::ptr::write_unaligned(field as *mut std::os::raw::c_ulong, obj_i64(value)? as _)
            }
            T_LONGLONG => std::ptr::write_unaligned(field as *mut i64, obj_i64(value)?),
            T_ULONGLONG => std::ptr::write_unaligned(field as *mut u64, obj_i64(value)? as u64),
            T_PYSSIZET => std::ptr::write_unaligned(field as *mut isize, obj_i64(value)? as isize),
            T_FLOAT => std::ptr::write_unaligned(field as *mut f32, obj_f64(value)? as f32),
            T_DOUBLE => std::ptr::write_unaligned(field as *mut f64, obj_f64(value)?),
            T_OBJECT | T_OBJECT_EX => {
                // Own a reference for the field, release the previous one.
                let new_p = crate::object::into_owned(value.clone());
                let old = std::ptr::read_unaligned(field as *const *mut PyObject);
                std::ptr::write_unaligned(field as *mut *mut PyObject, new_p);
                if !old.is_null() {
                    crate::object::Py_DecRef(old);
                }
            }
            T_STRING | T_CHAR | T_STRING_INPLACE | T_NONE => {
                return Err(type_error("readonly attribute"));
            }
            _ => return Err(type_error("unsupported member type")),
        }
    }
    Ok(())
}

//! Vectorcall — fast calling protocol for the C-API.
//!
//! Vectorcall is the calling convention CPython 3.9+ uses to bypass
//! the cost of building an intermediate `args` tuple and a `kwargs`
//! dict for every call. Receivers register a `vectorcallfunc`
//! pointer (in the type's `tp_vectorcall` slot, or as a per-instance
//! cell projected through `tp_vectorcall_offset`) and consumers call
//! [`PyObject_Vectorcall`].
//!
//! ## Implementation strategy
//!
//! WeavePy's native dispatch is dunder-driven, so the vectorcall
//! "fast path" doesn't actually speed anything up internally — what
//! it gives us is **correctness**: extension code that emits
//! vectorcall sites needs to find a working entry point, otherwise
//! `PyObject_Vectorcall(meth, args, n, NULL)` panics.
//!
//! Our implementation:
//!
//! 1. If the callable's type carries a `tp_vectorcall` slot, route
//!    through it directly.
//! 2. Otherwise, decode the `(args, nargsf, kwnames)` triple into a
//!    `(args, kwargs)` pair and forward to [`PyObject_Call`].
//!
//! [`PY_VECTORCALL_ARGUMENTS_OFFSET`] is honoured: when the bit is
//! set, we skip the leading slot of `args`, matching CPython's
//! convention that callers may pre-reserve a slot for `self`.

use std::ptr;

use weavepy_vm::object::{DictKey, Object};

use crate::object::{PyObject, PySsizeT};

/// Top bit of `nargsf`. Indicates the caller already left a slot
/// for `self` at `args[-1]`, so we should skip the first element.
pub const PY_VECTORCALL_ARGUMENTS_OFFSET: usize = 1_usize << (usize::BITS - 1);

/// `PyVectorcall_NARGS(nargsf)` — strip the high `args-offset` bit.
#[no_mangle]
pub unsafe extern "C" fn PyVectorcall_NARGS(nargsf: usize) -> PySsizeT {
    (nargsf & !PY_VECTORCALL_ARGUMENTS_OFFSET) as PySsizeT
}

/// Vectorcall function signature: `(callable, args, nargsf, kwnames)
/// -> result`.
pub type VectorcallFunc = unsafe extern "C" fn(
    callable: *mut PyObject,
    args: *const *mut PyObject,
    nargsf: usize,
    kwnames: *mut PyObject,
) -> *mut PyObject;

/// `PyVectorcall_Function(callable)` — return the vectorcall slot
/// for `callable`'s type, or null if the callable doesn't support
/// vectorcall.
#[no_mangle]
pub unsafe extern "C" fn PyVectorcall_Function(callable: *mut PyObject) -> *mut std::ffi::c_void {
    if callable.is_null() {
        return ptr::null_mut();
    }
    let head = unsafe { &*callable };
    let Some(slot_table) = (unsafe { crate::slottable::slot_table_for(head.ob_type) }) else {
        return ptr::null_mut();
    };
    slot_table.get(crate::slottable::Py_tp_vectorcall).0
}

/// `PyObject_Vectorcall(callable, args, nargsf, kwnames)` — fast
/// invocation path.
#[no_mangle]
pub unsafe extern "C" fn PyObject_Vectorcall(
    callable: *mut PyObject,
    args: *const *mut PyObject,
    nargsf: usize,
    kwnames: *mut PyObject,
) -> *mut PyObject {
    if callable.is_null() {
        crate::errors::set_type_error("PyObject_Vectorcall: NULL callable");
        return ptr::null_mut();
    }

    // 1) Try the type's vectorcall slot.
    let slot = unsafe { PyVectorcall_Function(callable) };
    if !slot.is_null() {
        let f: VectorcallFunc = unsafe { std::mem::transmute(slot) };
        return unsafe { f(callable, args, nargsf, kwnames) };
    }

    // 2) Fallback: decode and forward to PyObject_Call.
    let (positional, kwargs) = unsafe { decode_vectorcall(args, nargsf, kwnames) };
    let arg_tuple = crate::object::into_owned(Object::new_tuple(positional));
    let kw_dict = crate::object::into_owned(Object::Dict(weavepy_vm::sync::Rc::new(
        weavepy_vm::sync::RefCell::new(kwargs),
    )));
    let result = unsafe { crate::abstract_::PyObject_Call(callable, arg_tuple, kw_dict) };
    unsafe { crate::object::Py_DecRef(arg_tuple) };
    unsafe { crate::object::Py_DecRef(kw_dict) };
    result
}

/// `PyObject_VectorcallDict(callable, args, nargsf, kwdict)` —
/// vectorcall variant where keyword arguments come pre-packed in a
/// dict instead of a tuple of names.
#[no_mangle]
pub unsafe extern "C" fn PyObject_VectorcallDict(
    callable: *mut PyObject,
    args: *const *mut PyObject,
    nargsf: usize,
    kwdict: *mut PyObject,
) -> *mut PyObject {
    if callable.is_null() {
        crate::errors::set_type_error("PyObject_VectorcallDict: NULL callable");
        return ptr::null_mut();
    }
    let nargs = (nargsf & !PY_VECTORCALL_ARGUMENTS_OFFSET) as usize;
    let positional = unsafe { collect_positional(args, nargs, nargsf) };
    let arg_tuple = crate::object::into_owned(Object::new_tuple(positional));
    let result = unsafe { crate::abstract_::PyObject_Call(callable, arg_tuple, kwdict) };
    unsafe { crate::object::Py_DecRef(arg_tuple) };
    result
}

/// `PyVectorcall_Call(callable, tuple, kw)` — slow-path entry from
/// inside extensions that want to forward a `(args, kwargs)` pair
/// through the vectorcall protocol.
#[no_mangle]
pub unsafe extern "C" fn PyVectorcall_Call(
    callable: *mut PyObject,
    tuple: *mut PyObject,
    kw: *mut PyObject,
) -> *mut PyObject {
    unsafe { crate::abstract_::PyObject_Call(callable, tuple, kw) }
}

/// `PyObject_VectorcallMethod(name, args, nargsf, kwnames)` —
/// equivalent to `getattr(args[0], name)(*args[1:], **kw)`.
#[no_mangle]
pub unsafe extern "C" fn PyObject_VectorcallMethod(
    name: *mut PyObject,
    args: *const *mut PyObject,
    nargsf: usize,
    kwnames: *mut PyObject,
) -> *mut PyObject {
    if name.is_null() || args.is_null() {
        crate::errors::set_type_error("PyObject_VectorcallMethod: NULL argument");
        return ptr::null_mut();
    }
    let nargs = (nargsf & !PY_VECTORCALL_ARGUMENTS_OFFSET) as usize;
    if nargs == 0 {
        crate::errors::set_type_error("PyObject_VectorcallMethod: empty args");
        return ptr::null_mut();
    }
    let receiver = unsafe { *args };
    let method = unsafe { crate::abstract_::PyObject_GetAttr(receiver, name) };
    if method.is_null() {
        return ptr::null_mut();
    }
    let positional = unsafe { collect_positional_after(args, nargs, nargsf) };
    let arg_tuple = crate::object::into_owned(Object::new_tuple(positional));

    let kwargs = if kwnames.is_null() {
        weavepy_vm::object::DictData::new()
    } else {
        unsafe { kwnames_to_dict(kwnames, args, nargs, nargsf) }
    };
    let kw_dict = crate::object::into_owned(Object::Dict(weavepy_vm::sync::Rc::new(
        weavepy_vm::sync::RefCell::new(kwargs),
    )));

    let result = unsafe { crate::abstract_::PyObject_Call(method, arg_tuple, kw_dict) };
    unsafe { crate::object::Py_DecRef(arg_tuple) };
    unsafe { crate::object::Py_DecRef(kw_dict) };
    unsafe { crate::object::Py_DecRef(method) };
    result
}

unsafe fn decode_vectorcall(
    args: *const *mut PyObject,
    nargsf: usize,
    kwnames: *mut PyObject,
) -> (Vec<Object>, weavepy_vm::object::DictData) {
    let nargs = (nargsf & !PY_VECTORCALL_ARGUMENTS_OFFSET) as usize;
    let positional = unsafe { collect_positional(args, nargs, nargsf) };
    let kwargs = if kwnames.is_null() {
        weavepy_vm::object::DictData::new()
    } else {
        unsafe { kwnames_to_dict(kwnames, args, nargs, nargsf) }
    };
    (positional, kwargs)
}

unsafe fn collect_positional(
    args: *const *mut PyObject,
    nargs: usize,
    nargsf: usize,
) -> Vec<Object> {
    if args.is_null() || nargs == 0 {
        return Vec::new();
    }
    let offset = if (nargsf & PY_VECTORCALL_ARGUMENTS_OFFSET) != 0 {
        1
    } else {
        0
    };
    let mut out = Vec::with_capacity(nargs);
    for i in offset..(nargs + offset) {
        let p = unsafe { *args.add(i) };
        if p.is_null() {
            out.push(Object::None);
        } else {
            out.push(unsafe { crate::object::clone_object(p) });
        }
    }
    out
}

unsafe fn collect_positional_after(
    args: *const *mut PyObject,
    nargs: usize,
    nargsf: usize,
) -> Vec<Object> {
    if args.is_null() || nargs <= 1 {
        return Vec::new();
    }
    let offset = if (nargsf & PY_VECTORCALL_ARGUMENTS_OFFSET) != 0 {
        1
    } else {
        0
    };
    let mut out = Vec::with_capacity(nargs - 1);
    for i in (offset + 1)..(nargs + offset) {
        let p = unsafe { *args.add(i) };
        if p.is_null() {
            out.push(Object::None);
        } else {
            out.push(unsafe { crate::object::clone_object(p) });
        }
    }
    out
}

unsafe fn kwnames_to_dict(
    kwnames: *mut PyObject,
    args: *const *mut PyObject,
    nargs: usize,
    nargsf: usize,
) -> weavepy_vm::object::DictData {
    let mut out = weavepy_vm::object::DictData::new();
    if kwnames.is_null() {
        return out;
    }
    let names = unsafe { crate::object::clone_object(kwnames) };
    let names_vec = match names {
        Object::Tuple(items) => items.iter().cloned().collect::<Vec<_>>(),
        _ => return out,
    };
    let offset = if (nargsf & PY_VECTORCALL_ARGUMENTS_OFFSET) != 0 {
        1
    } else {
        0
    };
    for (i, name) in names_vec.into_iter().enumerate() {
        let p = unsafe { *args.add(offset + nargs + i) };
        let value = if p.is_null() {
            Object::None
        } else {
            unsafe { crate::object::clone_object(p) }
        };
        out.insert(DictKey(name), value);
    }
    out
}

/// `PyObject_CallOneArg2(callable, arg)` — alternative entry point
/// extensions sometimes hit (CPython has it as a `_PyObject_CallOneArg`
/// internal). We forward to the single-arg path.
#[no_mangle]
pub unsafe extern "C" fn PyObject_CallOneArg2(
    callable: *mut PyObject,
    arg: *mut PyObject,
) -> *mut PyObject {
    unsafe { crate::abstract_::PyObject_CallOneArg(callable, arg) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nargs_strips_offset_bit() {
        assert_eq!(unsafe { PyVectorcall_NARGS(3) }, 3);
        assert_eq!(
            unsafe { PyVectorcall_NARGS(3 | PY_VECTORCALL_ARGUMENTS_OFFSET) },
            3
        );
    }
}

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
//! [`PY_VECTORCALL_ARGUMENTS_OFFSET`] is honoured per CPython's exact
//! contract: the bit does **not** shift `args`. `args[0]` is always the
//! first argument (the receiver, for [`PyObject_VectorcallMethod`]) and
//! the positional count `PyVectorcall_NARGS(nargsf)` counts the elements
//! at `args[0..nargs]`. The bit only tells the callee that the slot at
//! `args[-1]` is scratch space it may temporarily overwrite (CPython's
//! trick for prepending `self` without reallocating) — a guarantee we
//! never need to read, so it has no effect on our decoding.

use std::ptr;

use weavepy_vm::object::{DictKey, Object};

use crate::object::{PyObject, PySsizeT};

/// Top bit of `nargsf`. Indicates the caller left a scratch slot at
/// `args[-1]` that the callee may temporarily overwrite (e.g. to
/// prepend `self`). It does **not** shift `args`: `args[0]` is still
/// the first argument and `PyVectorcall_NARGS` still counts from there.
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

/// `Py_TPFLAGS_HAVE_VECTORCALL` — the type advertises a per-instance
/// `vectorcallfunc` projected through `tp_vectorcall_offset`.
const HAVE_VECTORCALL: u64 = 1 << 11;

/// Read the **per-instance** `vectorcallfunc` of `callable` the way
/// CPython's inlined `PyVectorcall_Function` does: require
/// `Py_TPFLAGS_HAVE_VECTORCALL` on the type, then load the function
/// pointer from `*(char *)callable + tp->tp_vectorcall_offset`.
///
/// This is the load-bearing piece for vectorcall types whose `tp_call`
/// is `PyVectorcall_Call` (the standard idiom — numpy's
/// `_ArrayFunctionDispatcher` stores `dispatcher_vectorcall` in a
/// per-instance `vectorcall` field). Routing such a call back through
/// `PyObject_Call` would re-enter `tp_call` and recurse forever.
///
/// # Safety
/// `callable` must be non-null and its `ob_type` a faithful `PyTypeObject`.
unsafe fn instance_vectorcall_func(callable: *mut PyObject) -> *mut std::ffi::c_void {
    let tp = unsafe { (*callable).ob_type } as *mut crate::types::PyTypeObject;
    if tp.is_null() {
        return ptr::null_mut();
    }
    let flags = unsafe { (*tp).tp_flags };
    if flags & HAVE_VECTORCALL == 0 {
        return ptr::null_mut();
    }
    let offset = unsafe { (*tp).tp_vectorcall_offset };
    if offset <= 0 {
        return ptr::null_mut();
    }
    let slot =
        unsafe { (callable as *const u8).offset(offset as isize) } as *const *mut std::ffi::c_void;
    unsafe { *slot }
}

/// `PyVectorcall_Function(callable)` — return the vectorcall entry point
/// for `callable`, or null if it doesn't support vectorcall. Prefers the
/// per-instance `vectorcallfunc` (the `tp_vectorcall_offset` projection),
/// falling back to a type-level `tp_vectorcall` slot.
#[no_mangle]
pub unsafe extern "C" fn PyVectorcall_Function(callable: *mut PyObject) -> *mut std::ffi::c_void {
    if callable.is_null() {
        return ptr::null_mut();
    }
    let inst = unsafe { instance_vectorcall_func(callable) };
    if !inst.is_null() {
        return inst;
    }
    let head = unsafe { &*callable };
    let Some(slot_table) = (unsafe { crate::slottable::slot_table_for(head.ob_type) }) else {
        return ptr::null_mut();
    };
    slot_table.get(crate::slottable::Py_tp_vectorcall).0
}

/// Invoke a per-instance `vectorcallfunc` with arguments supplied as a
/// CPython `(args_tuple, kwargs_dict)` pair, translating into the
/// vectorcall convention: a flat `args` array of positionals followed by
/// the keyword values, with `kwnames` a tuple of the keyword names and
/// `nargsf` the positional count.
///
/// # Safety
/// `func` must be a valid `vectorcallfunc`; `callable` non-null.
unsafe fn call_via_vectorcall(
    callable: *mut PyObject,
    func: *mut std::ffi::c_void,
    tuple: *mut PyObject,
    kw: *mut PyObject,
) -> *mut PyObject {
    let f: VectorcallFunc = unsafe { std::mem::transmute(func) };
    let nargs = if tuple.is_null() {
        0
    } else {
        unsafe { crate::containers::PyTuple_Size(tuple) }
    };
    let nargs = if nargs < 0 { 0usize } else { nargs as usize };
    let mut argvec: Vec<*mut PyObject> = Vec::with_capacity(nargs + 4);
    for i in 0..nargs {
        // Borrowed references owned by the tuple (alive across the call).
        argvec.push(unsafe { crate::containers::PyTuple_GetItem(tuple, i as PySsizeT) });
    }
    let mut kwnames: *mut PyObject = ptr::null_mut();
    let mut owned_values: Vec<*mut PyObject> = Vec::new();
    if !kw.is_null() {
        if let Object::Dict(d) = unsafe { crate::object::clone_object(kw) } {
            let items: Vec<(Object, Object)> = d
                .borrow()
                .iter()
                .map(|(k, v)| (k.0.clone(), v.clone()))
                .collect();
            if !items.is_empty() {
                let mut names = Vec::with_capacity(items.len());
                for (k, v) in items {
                    names.push(k);
                    let vp = crate::object::into_owned(v);
                    owned_values.push(vp);
                    argvec.push(vp);
                }
                kwnames = crate::object::into_owned(Object::new_tuple(names));
            }
        }
    }
    let result = unsafe { f(callable, argvec.as_ptr(), nargs, kwnames) };
    for vp in owned_values {
        unsafe { crate::object::Py_DecRef(vp) };
    }
    if !kwnames.is_null() {
        unsafe { crate::object::Py_DecRef(kwnames) };
    }
    result
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
    if std::env::var_os("WEAVEPY_TRACE_CALL").is_some() {
        let nkw = if kwnames.is_null() {
            0
        } else {
            match unsafe { crate::object::clone_object(kwnames) } {
                Object::Tuple(t) => t.len(),
                _ => 0,
            }
        };
        eprintln!(
            "[TRACE_VEC] nargs={} nkw={} slot_null={}",
            nargsf & !PY_VECTORCALL_ARGUMENTS_OFFSET,
            nkw,
            slot.is_null()
        );
    }
    if !slot.is_null() {
        let f: VectorcallFunc = unsafe { std::mem::transmute(slot) };
        return unsafe { f(callable, args, nargsf, kwnames) };
    }

    // 2) Fallback: decode and forward to PyObject_Call. A keyword-less
    //    call forwards a NULL `kwds` (CPython's convention) rather than a
    //    fresh empty dict — extensions branch on `kwds != NULL`.
    let (positional, kwargs) = unsafe { decode_vectorcall(args, nargsf, kwnames) };
    let arg_tuple = crate::object::into_owned(Object::new_tuple(positional));
    let kw_dict = if kwargs.is_empty() {
        ptr::null_mut()
    } else {
        crate::object::into_owned(Object::Dict(weavepy_vm::sync::Rc::new(
            weavepy_vm::sync::RefCell::new(kwargs),
        )))
    };
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
    if std::env::var_os("WEAVEPY_TRACE_CALL").is_some() {
        eprintln!("[TRACE_VCDICT] kwdict_null={}", kwdict.is_null());
    }
    let nargs = (nargsf & !PY_VECTORCALL_ARGUMENTS_OFFSET) as usize;
    let positional = unsafe { collect_positional(args, nargs) };
    let arg_tuple = crate::object::into_owned(Object::new_tuple(positional));
    let result = unsafe { crate::abstract_::PyObject_Call(callable, arg_tuple, kwdict) };
    unsafe { crate::object::Py_DecRef(arg_tuple) };
    result
}

/// `PyVectorcall_Call(callable, tuple, kw)` — forward a `(args, kwargs)`
/// pair through the vectorcall protocol.
///
/// CPython types that opt into vectorcall set `tp_call = PyVectorcall_Call`
/// and store the real `vectorcallfunc` per instance. We therefore read
/// that per-instance function and invoke it directly. Falling back to
/// `PyObject_Call` here (the previous behaviour) re-entered the same
/// `tp_call` slot and recursed without bound (numpy's
/// `_ArrayFunctionDispatcher`, called as `ones(...)`, overflowed the
/// stack). Only when the callable exposes no vectorcall do we forward to
/// the generic call machinery.
#[no_mangle]
pub unsafe extern "C" fn PyVectorcall_Call(
    callable: *mut PyObject,
    tuple: *mut PyObject,
    kw: *mut PyObject,
) -> *mut PyObject {
    if callable.is_null() {
        crate::errors::set_type_error("PyVectorcall_Call: NULL callable");
        return ptr::null_mut();
    }
    if std::env::var_os("WEAVEPY_TRACE_CALL").is_some() {
        eprintln!("[TRACE_PYVCALL] kw_null={}", kw.is_null());
    }
    let func = unsafe { instance_vectorcall_func(callable) };
    if !func.is_null() {
        return unsafe { call_via_vectorcall(callable, func, tuple, kw) };
    }
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
    if std::env::var_os("WEAVEPY_TRACE_CALL").is_some() {
        let mname = match unsafe { crate::object::clone_object(name) } {
            Object::Str(s) => s.to_string(),
            other => format!("{other:?}"),
        };
        let robj = unsafe { crate::object::clone_object(receiver) };
        let rdesc = match &robj {
            Object::Instance(i) => format!("Instance(class={})", i.cls().name),
            Object::Foreign(_) => "Foreign".to_string(),
            other => other.type_name().to_string(),
        };
        let mdesc = match unsafe { crate::object::clone_object(method) } {
            Object::Builtin(b) => format!("Builtin({})", b.name),
            Object::Function(_) => "Function".to_string(),
            Object::BoundMethod(bm) => format!("BoundMethod({})", bm.function.type_name()),
            other => other.type_name().to_string(),
        };
        if mname == "__init__" {
            eprintln!(
                "[TRACE_VCMETHOD] method={mname} nargs={nargs} receiver={rdesc} resolved={mdesc}"
            );
        }
    }
    let positional = unsafe { collect_positional_after(args, nargs) };
    let arg_tuple = crate::object::into_owned(Object::new_tuple(positional));

    let kw_dict = if kwnames.is_null() {
        ptr::null_mut()
    } else {
        let kwargs = unsafe { kwnames_to_dict(kwnames, args, nargs) };
        if kwargs.is_empty() {
            ptr::null_mut()
        } else {
            crate::object::into_owned(Object::Dict(weavepy_vm::sync::Rc::new(
                weavepy_vm::sync::RefCell::new(kwargs),
            )))
        }
    };

    let result = unsafe { crate::abstract_::PyObject_Call(method, arg_tuple, kw_dict) };
    unsafe { crate::object::Py_DecRef(arg_tuple) };
    unsafe { crate::object::Py_DecRef(kw_dict) };
    unsafe { crate::object::Py_DecRef(method) };
    // `receiver.method(...)` mutates the *bound* receiver (e.g.
    // `s.difference_update(other)`), which — unlike the unbound-method path —
    // is not among the forwarded positional args; refresh its macro-visible
    // size directly (RFC 0047, wave 5).
    unsafe { crate::mirror::sync_container_size(receiver) };
    result
}

unsafe fn decode_vectorcall(
    args: *const *mut PyObject,
    nargsf: usize,
    kwnames: *mut PyObject,
) -> (Vec<Object>, weavepy_vm::object::DictData) {
    let nargs = (nargsf & !PY_VECTORCALL_ARGUMENTS_OFFSET) as usize;
    let positional = unsafe { collect_positional(args, nargs) };
    let kwargs = if kwnames.is_null() {
        weavepy_vm::object::DictData::new()
    } else {
        unsafe { kwnames_to_dict(kwnames, args, nargs) }
    };
    (positional, kwargs)
}

/// Decode the `nargs` positional arguments at `args[0..nargs]`.
///
/// `PY_VECTORCALL_ARGUMENTS_OFFSET` deliberately plays no part here: it
/// concerns only the scratch slot at `args[-1]`, never the index of the
/// first real argument (see the module docs).
unsafe fn collect_positional(args: *const *mut PyObject, nargs: usize) -> Vec<Object> {
    if args.is_null() || nargs == 0 {
        return Vec::new();
    }
    let mut out = Vec::with_capacity(nargs);
    for i in 0..nargs {
        let p = unsafe { *args.add(i) };
        if p.is_null() {
            out.push(Object::None);
        } else {
            out.push(unsafe { crate::object::clone_object(p) });
        }
    }
    out
}

/// Decode the positional arguments *after* the receiver — `args[1..nargs]`
/// — for [`PyObject_VectorcallMethod`], whose `args[0]` is `self` (already
/// folded into the bound method we resolved) and whose `nargs` counts that
/// receiver.
unsafe fn collect_positional_after(args: *const *mut PyObject, nargs: usize) -> Vec<Object> {
    if args.is_null() || nargs <= 1 {
        return Vec::new();
    }
    let mut out = Vec::with_capacity(nargs - 1);
    for i in 1..nargs {
        let p = unsafe { *args.add(i) };
        if p.is_null() {
            out.push(Object::None);
        } else {
            out.push(unsafe { crate::object::clone_object(p) });
        }
    }
    out
}

/// Decode keyword arguments: their values sit at `args[nargs..nargs+nkw]`,
/// immediately after every positional, with the names in the `kwnames`
/// tuple.
unsafe fn kwnames_to_dict(
    kwnames: *mut PyObject,
    args: *const *mut PyObject,
    nargs: usize,
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
    for (i, name) in names_vec.into_iter().enumerate() {
        let p = unsafe { *args.add(nargs + i) };
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

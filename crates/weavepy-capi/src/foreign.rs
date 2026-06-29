//! Binary-ABI side of the foreign-object proxy (RFC 0046, wave 4).
//!
//! [`weavepy_vm::foreign`] defines the opaque [`Object::Foreign`] proxy
//! and a table of operation hooks; this module *implements* those hooks
//! on top of the real C-API (`PyObject_Repr`, `PyObject_Call`,
//! `PyNumber_*`, …) and installs them at interpreter start. It is the
//! counterpart of the capsule / instance-body hooks: the VM stays
//! ignorant of cpyext, and every operation on a foreign `PyObject`
//! (numpy's `ndarray`, a static `PyArray_Descr`, a builtin ufunc)
//! round-trips through here.
//!
//! Each hook mirrors the dunder-shim pattern: marshal VM [`Object`]s to
//! `*mut PyObject` with [`into_owned`], run the C call under an active
//! interpreter context ([`ensure_active`]), then convert the result
//! (and any pending exception) back with [`unwrap`].

use std::ffi::CStr;

use weavepy_compiler::{BinOpKind, CompareKind};
use weavepy_vm::error::{runtime_error, RuntimeError};
use weavepy_vm::foreign::{self, ForeignHooks};
use weavepy_vm::object::Object;
use weavepy_vm::sync::Rc;

use crate::interp::ensure_active;
use crate::object::PyObject;

/// Wrap a foreign `*mut PyObject` into an [`Object::Foreign`] proxy,
/// caching its `tp_name` and pinning a reference. Called from
/// [`crate::object::clone_object`] for any pointer WeavePy did not mint.
///
/// # Safety
/// `p` must be a live, non-null `PyObject` whose `ob_type->tp_name` is a
/// valid C string (every real type sets it).
pub unsafe fn wrap_foreign(p: *mut PyObject) -> Object {
    let type_name = unsafe { foreign_type_name(p) };
    if crate::object::freebox_trace_enabled()
        && (type_name.contains("Engine")
            || type_name.contains("Index")
            || type_name.contains("ndarray"))
    {
        eprintln!(
            "[FOREIGN-WRAP] p=0x{:x} type={} refcnt={}",
            p as usize,
            type_name,
            unsafe { (*p).ob_refcnt }
        );
    }
    Object::Foreign(foreign::wrap(p as usize, type_name))
}

/// Read `Py_TYPE(p)->tp_name` (the dotted type name) as an owned `Rc<str>`.
unsafe fn foreign_type_name(p: *mut PyObject) -> Rc<str> {
    let ty = unsafe { (*p).ob_type };
    if ty.is_null() {
        return Rc::from("object");
    }
    let np = unsafe { (*ty).tp_name };
    if np.is_null() {
        return Rc::from("object");
    }
    let s = unsafe { CStr::from_ptr(np) }.to_string_lossy();
    // numpy uses fully-qualified `tp_name`s ("numpy.ndarray"); the bare
    // tail is what Python's `type(x).__name__` reports.
    let bare = s.rsplit('.').next().unwrap_or(&s);
    Rc::from(bare)
}

/// Run `body` (a call into compiled extension/Cython code) after
/// re-publishing every seeded faithful list's `ob_item` from its prefix
/// `Rc`. This is the VM→C boundary: a Python-side `list.__setitem__` on a
/// C-resident `cdef public list` (pandas' `BlockManager.axes[0] = …`) only
/// updated the prefix `Rc`, but the extension reads the list back through
/// the inlined `PyList_GET_ITEM` macro, which consults the C `ob_item`
/// buffer. Flushing here keeps the two coherent. Gated on an atomic, so a
/// program that never crossed a list into C pays a single relaxed load.
#[inline]
fn c_call<R>(body: impl FnOnce() -> R) -> R {
    // `ensure_active` performs the seeded-list flush at the outermost VM→C
    // transition, so the foreign-hook path needs nothing extra here.
    ensure_active(body)
}

// --- result/error marshalling (mirrors dunder_shim's private helpers) ---

fn pending_or_default() -> RuntimeError {
    if let Some(p) = crate::errors::take_pending() {
        crate::errors::to_runtime_error(p)
    } else {
        runtime_error("foreign object operation failed without setting an exception")
    }
}

/// Convert an owned `*mut PyObject` result into an `Object`, consuming
/// the reference. NULL ⇒ the pending exception.
fn unwrap(raw: *mut PyObject) -> Result<Object, RuntimeError> {
    if raw.is_null() {
        return Err(pending_or_default());
    }
    let obj = unsafe { crate::object::clone_object(raw) };
    unsafe { crate::object::Py_DecRef(raw) };
    Ok(obj)
}

fn to_string(raw: *mut PyObject) -> Result<String, RuntimeError> {
    Ok(unwrap(raw)?.to_str())
}

// --- the hooks ---

fn fwd_incref(p: usize) {
    crate::object::soul_inc(p);
    unsafe { crate::object::Py_IncRef(p as *mut PyObject) };
}

fn fwd_decref(p: usize) {
    // Drop the live-soul count *before* the decref: the last soul's own
    // decref frees the box, and free_box must then see a zero count.
    crate::object::soul_dec(p);
    unsafe { crate::object::Py_DecRef(p as *mut PyObject) };
}

fn fwd_repr(p: usize) -> Result<String, RuntimeError> {
    let raw = c_call(|| unsafe { crate::abstract_::PyObject_Repr(p as *mut PyObject) });
    to_string(raw)
}

fn fwd_str(p: usize) -> Result<String, RuntimeError> {
    let raw = c_call(|| unsafe { crate::abstract_::PyObject_Str(p as *mut PyObject) });
    to_string(raw)
}

fn fwd_hash(p: usize) -> Result<i64, RuntimeError> {
    // Call the foreign type's own `tp_hash` slot, NOT `PyObject_Hash`: the
    // latter routes back through the VM (`hash_public`), and the VM routes a
    // foreign object right back here — an unbounded ping-pong that overflows
    // the stack (`hash(np.int64(0))`). `hash_via_slot` consults only the C
    // slot, so a numpy scalar hashes like the equal Python int.
    let o = p as *mut PyObject;
    match c_call(|| unsafe { crate::abstract_::hash_via_slot(o) }) {
        Some(h) => {
            if h == -1 {
                if let Some(pe) = crate::errors::take_pending() {
                    return Err(crate::errors::to_runtime_error(pe));
                }
            }
            Ok(h as i64)
        }
        // No `tp_hash` slot ⇒ an unhashable foreign type. Report failure so
        // the VM falls back to an identity hash (its prior behavior) rather
        // than mistaking a sentinel for a real hash value.
        None => Err(runtime_error("unhashable foreign type")),
    }
}

fn fwd_is_true(p: usize) -> Result<bool, RuntimeError> {
    let r = c_call(|| unsafe { crate::abstract_::PyObject_IsTrue(p as *mut PyObject) });
    if r < 0 {
        return Err(pending_or_default());
    }
    Ok(r != 0)
}

fn fwd_call(
    p: usize,
    args: &[Object],
    kwargs: &[(String, Object)],
) -> Result<Object, RuntimeError> {
    if std::env::var_os("WEAVEPY_TRACE_CALL").is_some() {
        let keys: Vec<&str> = kwargs.iter().map(|(k, _)| k.as_str()).collect();
        eprintln!(
            "[TRACE_FWDCALL] nargs={} kwargs={:?}",
            args.len(),
            keys
        );
    }
    let callable = p as *mut PyObject;
    let args_tuple = crate::object::into_owned(Object::new_tuple(args.to_vec()));
    let kw = if kwargs.is_empty() {
        std::ptr::null_mut()
    } else {
        let mut d = weavepy_vm::object::DictData::new();
        for (k, v) in kwargs {
            d.insert(
                weavepy_vm::object::DictKey(Object::from_str(k.clone())),
                v.clone(),
            );
        }
        crate::object::into_owned(Object::Dict(Rc::new(weavepy_vm::sync::RefCell::new(d))))
    };
    let raw =
        c_call(|| unsafe { crate::abstract_::PyObject_Call(callable, args_tuple, kw) });
    unsafe {
        crate::object::Py_DecRef(args_tuple);
        if !kw.is_null() {
            crate::object::Py_DecRef(kw);
        }
    }
    unwrap(raw)
}

fn fwd_getattr(p: usize, name: &str) -> Result<Object, RuntimeError> {
    if crate::object::freebox_trace_enabled() && (name == "is_unique" || name == "unique") {
        let tyname = unsafe { crate::object::debug_type_name(p as *mut PyObject) };
        let rc = unsafe { (*(p as *mut PyObject)).ob_refcnt };
        eprintln!(
            "[GETATTR] name={} p=0x{:x} type={} refcnt={}",
            name, p, tyname, rc
        );
    }
    let cname = std::ffi::CString::new(name)
        .map_err(|_| runtime_error("attribute name contains a NUL byte"))?;
    let raw = c_call(|| unsafe {
        crate::abstract_::PyObject_GetAttrString(p as *mut PyObject, cname.as_ptr())
    });
    unwrap(raw)
}

fn fwd_setattr(p: usize, name: &str, value: Option<&Object>) -> Result<(), RuntimeError> {
    let cname = std::ffi::CString::new(name)
        .map_err(|_| runtime_error("attribute name contains a NUL byte"))?;
    let val = match value {
        Some(v) => crate::object::into_owned(v.clone()),
        None => std::ptr::null_mut(),
    };
    let rc = c_call(|| unsafe {
        crate::abstract_::PyObject_SetAttrString(p as *mut PyObject, cname.as_ptr(), val)
    });
    if !val.is_null() {
        unsafe { crate::object::Py_DecRef(val) };
    }
    if rc < 0 {
        return Err(pending_or_default());
    }
    Ok(())
}

fn fwd_getitem(p: usize, key: &Object) -> Result<Object, RuntimeError> {
    let kp = crate::object::into_owned(key.clone());
    let raw =
        c_call(|| unsafe { crate::abstract_::PyObject_GetItem(p as *mut PyObject, kp) });
    unsafe { crate::object::Py_DecRef(kp) };
    unwrap(raw)
}

fn fwd_setitem(p: usize, key: &Object, value: Option<&Object>) -> Result<(), RuntimeError> {
    let kp = crate::object::into_owned(key.clone());
    let rc = match value {
        Some(v) => {
            let vp = crate::object::into_owned(v.clone());
            let rc = c_call(|| unsafe {
                crate::abstract_::PyObject_SetItem(p as *mut PyObject, kp, vp)
            });
            unsafe { crate::object::Py_DecRef(vp) };
            rc
        }
        None => {
            c_call(|| unsafe { crate::abstract_::PyObject_DelItem(p as *mut PyObject, kp) })
        }
    };
    unsafe { crate::object::Py_DecRef(kp) };
    if rc < 0 {
        return Err(pending_or_default());
    }
    Ok(())
}

fn fwd_length(p: usize) -> Result<isize, RuntimeError> {
    let n = c_call(|| unsafe { crate::abstract_::PyObject_Size(p as *mut PyObject) });
    if n < 0 {
        return Err(pending_or_default());
    }
    Ok(n)
}

fn fwd_iter(p: usize) -> Result<Object, RuntimeError> {
    let raw = c_call(|| unsafe { crate::abstract_::PyObject_GetIter(p as *mut PyObject) });
    unwrap(raw)
}

fn fwd_iternext(p: usize) -> Result<Option<Object>, RuntimeError> {
    let raw = c_call(|| unsafe { crate::abstract_::PyIter_Next(p as *mut PyObject) });
    if raw.is_null() {
        // NULL with no pending exception ⇒ normal exhaustion.
        if let Some(pe) = crate::errors::take_pending() {
            return Err(crate::errors::to_runtime_error(pe));
        }
        return Ok(None);
    }
    let obj = unsafe { crate::object::clone_object(raw) };
    unsafe { crate::object::Py_DecRef(raw) };
    Ok(Some(obj))
}

type BinFn = unsafe extern "C" fn(*mut PyObject, *mut PyObject) -> *mut PyObject;

fn fwd_binop(op: BinOpKind, a: &Object, b: &Object) -> Result<Object, RuntimeError> {
    use BinOpKind as B;
    let ap = crate::object::into_owned(a.clone());
    let bp = crate::object::into_owned(b.clone());
    let raw = c_call(|| unsafe {
        match op {
            // `**` takes a third (modulus) argument; pass None.
            B::Pow => {
                let none = crate::singletons::none_ptr();
                crate::object::Py_IncRef(none);
                let r = crate::abstract_::PyNumber_Power(ap, bp, none);
                crate::object::Py_DecRef(none);
                r
            }
            other => {
                let f: BinFn = match other {
                    B::Add => crate::abstract_::PyNumber_Add,
                    B::Sub => crate::abstract_::PyNumber_Subtract,
                    B::Mult => crate::abstract_::PyNumber_Multiply,
                    B::MatMult => crate::abstract_::PyNumber_MatrixMultiply,
                    B::Div => crate::abstract_::PyNumber_TrueDivide,
                    B::FloorDiv => crate::abstract_::PyNumber_FloorDivide,
                    B::Mod => crate::abstract_::PyNumber_Remainder,
                    B::LShift => crate::abstract_::PyNumber_Lshift,
                    B::RShift => crate::abstract_::PyNumber_Rshift,
                    B::BitOr => crate::abstract_::PyNumber_Or,
                    B::BitXor => crate::abstract_::PyNumber_Xor,
                    B::BitAnd => crate::abstract_::PyNumber_And,
                    B::Pow => unreachable!("handled above"),
                };
                f(ap, bp)
            }
        }
    });
    unsafe {
        crate::object::Py_DecRef(ap);
        crate::object::Py_DecRef(bp);
    }
    unwrap(raw)
}

fn fwd_compare(op: CompareKind, a: &Object, b: &Object) -> Result<Object, RuntimeError> {
    use CompareKind as C;
    // Mirror Python.h's Py_LT..Py_GE opcodes.
    let opid: std::os::raw::c_int = match op {
        C::Lt => 0,
        C::LtE => 1,
        C::Eq => 2,
        C::NotEq => 3,
        C::Gt => 4,
        C::GtE => 5,
    };
    let ap = crate::object::into_owned(a.clone());
    let bp = crate::object::into_owned(b.clone());
    // This is the VM→C bridge: the VM's `rich_compare_obj` already decided
    // an operand is foreign and is asking the C side whether it can compare
    // the pair. Consult ONLY the operands' C `tp_richcompare` slots
    // (`richcompare_via_slot`) — NOT the full `PyObject_RichCompare`, which
    // on a slot decline falls back to `richcompare_via_vm` and re-enters the
    // VM for the *same* pair, producing an unbounded VM↔C ping-pong that
    // overflows the native stack (seen with pandas `pivot_table`, where two
    // foreign operands both carry declining/absent C compare slots). A
    // `NotImplemented` from the C slots is returned to the VM caller, which
    // then applies the native default (identity for `==`/`!=`, `TypeError`
    // for an ordering) exactly as CPython's `do_richcompare` does.
    let raw = c_call(|| unsafe { crate::abstract_::richcompare_via_slot(ap, bp, opid) });
    unsafe {
        crate::object::Py_DecRef(ap);
        crate::object::Py_DecRef(bp);
    }
    unwrap(raw)
}

fn fwd_get_type(p: usize) -> Object {
    let ty = unsafe { (*(p as *mut PyObject)).ob_type };
    if ty.is_null() {
        return Object::None;
    }
    unsafe { crate::object::clone_object(ty as *mut PyObject) }
}

fn fwd_as_float(p: usize) -> Result<Object, RuntimeError> {
    let raw = c_call(|| unsafe { crate::abstract_::PyNumber_Float(p as *mut PyObject) });
    unwrap(raw)
}

fn fwd_as_int(p: usize) -> Result<Object, RuntimeError> {
    let raw = c_call(|| unsafe { crate::abstract_::PyNumber_Long(p as *mut PyObject) });
    unwrap(raw)
}

fn fwd_as_index(p: usize) -> Result<Object, RuntimeError> {
    let raw = c_call(|| unsafe { crate::abstract_::PyNumber_Index(p as *mut PyObject) });
    unwrap(raw)
}

/// `memoryview(foreign)` — wrap a foreign buffer exporter (numpy's
/// `ndarray`, a Cython `cdef class` with `__getbuffer__`, …) in a VM
/// memoryview. Routes through [`crate::memoryview::PyMemoryView_FromObject`]
/// which drives `PyObject_GetBuffer(PyBUF_FULL_RO)` and preserves the
/// exporter's faithful `format`/`itemsize`/`shape`/`strides`.
fn fwd_get_buffer(p: usize) -> Result<Object, RuntimeError> {
    let raw = c_call(|| unsafe {
        crate::memoryview::PyMemoryView_FromObject(p as *mut PyObject)
    });
    unwrap(raw)
}

/// `memoryview(obj)` for an arbitrary VM object — a numpy `ndarray` crosses
/// as a faithful [`Object::Instance`] (wearing its real C type), so it has no
/// foreign soul pointer. Marshal it to a `*mut PyObject` and drive
/// `PyMemoryView_FromObject`, which calls the exporter's `bf_getbuffer`. The
/// temporary cross-reference is released afterwards; the resulting memoryview
/// snapshots the buffer, so it does not depend on `p` staying alive.
fn fwd_get_buffer_obj(obj: &Object) -> Result<Object, RuntimeError> {
    let p = crate::object::into_owned(obj.clone());
    let raw = c_call(|| unsafe { crate::memoryview::PyMemoryView_FromObject(p) });
    unsafe { crate::object::Py_DecRef(p) };
    unwrap(raw)
}

/// Install the foreign-object bridge into the VM. Idempotent.
pub fn install() {
    foreign::install(ForeignHooks {
        incref: fwd_incref,
        decref: fwd_decref,
        repr: fwd_repr,
        str: fwd_str,
        hash: fwd_hash,
        is_true: fwd_is_true,
        call: fwd_call,
        getattr: fwd_getattr,
        setattr: fwd_setattr,
        getitem: fwd_getitem,
        setitem: fwd_setitem,
        length: fwd_length,
        iter: fwd_iter,
        iternext: fwd_iternext,
        binop: fwd_binop,
        compare: fwd_compare,
        get_type: fwd_get_type,
        as_float: fwd_as_float,
        as_int: fwd_as_int,
        as_index: fwd_as_index,
        get_buffer: fwd_get_buffer,
        get_buffer_obj: fwd_get_buffer_obj,
    });
}

//! `instancemethod` — CPython's C-only method wrapper (RFC 0066 WS3).
//!
//! `PyInstanceMethod_New(func)` wraps any callable so it binds like a
//! plain Python function: class access yields `func` itself, instance
//! access yields a bound method. pybind11 wraps **every** non-static
//! method of every registered class in one
//! (`cpp_function::initialize_generic` → `PYBIND11_INSTANCE_METHOD_NEW`)
//! and later reads the wrapped callable back through the
//! `PyInstanceMethod_GET_FUNCTION` *macro* — a direct read of
//! `((PyInstanceMethodObject *)op)->func` at offset 16 — so instances
//! must carry CPython's exact 24-byte layout, and the type must be the
//! address-identical `PyInstanceMethod_Type` symbol
//! (`PYBIND11_INSTANCE_METHOD_CHECK` is `Py_IS_TYPE(op, &...)`).
//! scipy ships two pybind11 extensions on the probe path
//! (`_highspy._core`, `fft._pocketfft`); the missing data symbol made
//! their dlopen fail outright.
//!
//! Instances are minted through `PyObject_Malloc` and deliberately **not**
//! registered as WeavePy-owned: they cross into the VM as
//! [`Object::Foreign`] proxies, and binding happens through the foreign
//! `tp_descr_get` hook ([`crate::foreign`]'s `descr_get`), exactly like
//! any other extension-built descriptor.

use std::os::raw::c_void;
use std::ptr;

use crate::object::PyObject;
use crate::types::PyTypeObject;
use weavepy_vm::object::Object;

/// CPython's `PyInstanceMethodObject` (Include/cpython/classobject.h).
#[repr(C)]
pub struct PyInstanceMethodObject {
    pub head: PyObject,
    pub func: *mut PyObject,
}

/// True iff `op` is exactly an `instancemethod` (CPython's
/// `PyInstanceMethod_Check` is a `Py_IS_TYPE` identity test; the type
/// does not allow subclassing).
pub unsafe fn is_instancemethod(op: *mut PyObject) -> bool {
    !op.is_null()
        && std::ptr::eq(
            unsafe { (*op).ob_type },
            crate::types::PyInstanceMethod_Type.as_ptr(),
        )
}

/// `PyInstanceMethod_New(func)` — wrap `func` (any callable) in a new
/// instancemethod. Returns a new reference.
#[no_mangle]
pub unsafe extern "C" fn PyInstanceMethod_New(func: *mut PyObject) -> *mut PyObject {
    if func.is_null() {
        crate::errors::PyErr_BadInternalCall();
        return ptr::null_mut();
    }
    crate::interp::ensure_initialised();
    let im =
        unsafe { crate::memory::PyObject_Malloc(std::mem::size_of::<PyInstanceMethodObject>()) }
            as *mut PyInstanceMethodObject;
    if im.is_null() {
        crate::errors::set_runtime_error("PyInstanceMethod_New: out of memory");
        return ptr::null_mut();
    }
    unsafe {
        (*im).head.ob_refcnt = 1;
        (*im).head.ob_type = crate::types::PyInstanceMethod_Type.as_ptr();
        crate::object::Py_IncRef(func);
        (*im).func = func;
    }
    im as *mut PyObject
}

/// `PyInstanceMethod_Function(im)` — the wrapped callable (borrowed).
#[no_mangle]
pub unsafe extern "C" fn PyInstanceMethod_Function(im: *mut PyObject) -> *mut PyObject {
    if !unsafe { is_instancemethod(im) } {
        crate::errors::PyErr_BadInternalCall();
        return ptr::null_mut();
    }
    unsafe { (*(im as *mut PyInstanceMethodObject)).func }
}

/// `tp_descr_get`: CPython's `instancemethod_get` — class access returns
/// the wrapped function itself, instance access binds it.
unsafe extern "C" fn instancemethod_descr_get(
    descr: *mut PyObject,
    obj: *mut PyObject,
    _type: *mut PyObject,
) -> *mut PyObject {
    let func = unsafe { (*(descr as *mut PyInstanceMethodObject)).func };
    if obj.is_null() {
        unsafe { crate::object::Py_IncRef(func) };
        return func;
    }
    unsafe { crate::wave4::PyMethod_New(func, obj) }
}

/// `tp_call`: CPython's `instancemethod_call` — delegate to the wrapped
/// function with the arguments unchanged.
unsafe extern "C" fn instancemethod_call(
    im: *mut PyObject,
    args: *mut PyObject,
    kwargs: *mut PyObject,
) -> *mut PyObject {
    let func = unsafe { (*(im as *mut PyInstanceMethodObject)).func };
    unsafe { crate::abstract_::PyObject_Call(func, args, kwargs) }
}

/// `tp_getattro`: CPython's `instancemethod_getattro` — `__func__` (the
/// one instancemethod-own attribute WeavePy models) answers directly,
/// and **everything else delegates to the wrapped callable**
/// (`PyObject_GetAttr(GET_FUNCTION(self), name)`). pybind11 reads
/// `cf.attr("__name__")` off the wrapper while registering every method
/// (`add_class_method` ← `cpp_function::name()`); without the delegating
/// slot the lookup missed and module init aborted.
unsafe extern "C" fn instancemethod_getattro(
    im: *mut PyObject,
    name: *mut PyObject,
) -> *mut PyObject {
    let func = unsafe { (*(im as *mut PyInstanceMethodObject)).func };
    if let Object::Str(s) = unsafe { crate::object::clone_object(name) } {
        if &*s == "__func__" {
            unsafe { crate::object::Py_IncRef(func) };
            return func;
        }
    }
    unsafe { crate::abstract_::PyObject_GetAttr(func, name) }
}

/// `tp_dealloc`: release the wrapped function and the storage minted by
/// [`PyInstanceMethod_New`].
unsafe extern "C" fn instancemethod_dealloc(im: *mut PyObject) {
    unsafe {
        let func = (*(im as *mut PyInstanceMethodObject)).func;
        if !func.is_null() {
            crate::object::Py_DecRef(func);
        }
        crate::memory::PyObject_Free(im as *mut c_void);
    }
}

/// Wire the instancemethod protocol slots onto the already-installed
/// static type. Called once from [`crate::types::init_static_types`].
pub(crate) fn init_type_slots() {
    unsafe {
        let ty: &mut PyTypeObject = &mut *crate::types::PyInstanceMethod_Type.as_ptr();
        ty.tp_basicsize = std::mem::size_of::<PyInstanceMethodObject>() as crate::object::PySsizeT;
        ty.tp_descr_get = instancemethod_descr_get as *mut c_void;
        ty.tp_call = instancemethod_call as *mut c_void;
        ty.tp_getattro = instancemethod_getattro as *mut c_void;
        ty.tp_dealloc = Some(instancemethod_dealloc);
    }
    // Keep the compiler honest about the faithful layout: PyObject_HEAD
    // (16) + func (8).
    const _: () = assert!(std::mem::size_of::<PyInstanceMethodObject>() == 24);
}

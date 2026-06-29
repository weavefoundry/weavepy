//! RFC 0047 (wave 5): code / frame / traceback object facade.
//!
//! Genuine **Cython-generated** extensions create a `__code__` object per
//! `def`/`cpdef`/`cdef` function during *module init* (`__Pyx_CreateCodeObjects`
//! → `PyUnstable_Code_NewWithPosOnlyArgs`) and then write
//! `result->_co_firsttraceable = 0` **directly into the struct**, store it
//! on the function, and `Py_DECREF` it at teardown. The traceback builder
//! (`__Pyx_AddTraceback`) additionally reaches for `PyCode_NewEmpty`,
//! `PyFrame_New`, `PyTraceBack_Here`, and the `PyCode_Type`/`PyFrame_Type`/
//! `PyTraceBack_Type` identity statics.
//!
//! WeavePy executes these functions through their C entry points, not a
//! code object, so a code object here is **metadata only**: a byte-faithful
//! CPython 3.13 `PyCodeObject` body (so the direct `_co_firsttraceable`
//! write and any field read land on real memory), refcounted correctly
//! (the object owns the `tp_*`-stored sub-objects and releases them in
//! `tp_dealloc`), and otherwise opaque to the VM (handled as a foreign
//! object — see [`crate::object::clone_object`]).
//!
//! The hermetic wave-5 `_stockcython.c` fixture hand-rolled its types and
//! never created a single code object, so this whole surface was missing
//! until a *real* Cython `.so` linked it.

#![allow(clippy::missing_safety_doc)]

use core::ffi::{c_char, c_int};
use std::alloc::{self, Layout};
use std::ptr;
use std::sync::Mutex;

use crate::lifecycle::PyThreadState;
use crate::object::{PyObject, IMMORTAL_REFCNT};
use crate::types::StaticType;

// ---------------------------------------------------------------------------
// PyCodeObject 3.13 layout (machine-checked against stock `cpython/code.h`:
// `PyObject_VAR_HEAD` is 24 bytes, then the fields below).
// ---------------------------------------------------------------------------
const OFF_CONSTS: usize = 24; // PyObject *co_consts
const OFF_NAMES: usize = 32; // PyObject *co_names
const OFF_EXCEPTIONTABLE: usize = 40; // PyObject *co_exceptiontable
const OFF_FLAGS: usize = 48; // int co_flags
const OFF_ARGCOUNT: usize = 52; // int co_argcount
const OFF_POSONLY: usize = 56; // int co_posonlyargcount
const OFF_KWONLY: usize = 60; // int co_kwonlyargcount
const OFF_STACKSIZE: usize = 64; // int co_stacksize
const OFF_FIRSTLINENO: usize = 68; // int co_firstlineno
const OFF_NLOCALS: usize = 80; // int co_nlocals
const OFF_LOCALSPLUSNAMES: usize = 96; // PyObject *co_localsplusnames
const OFF_FILENAME: usize = 112; // PyObject *co_filename
const OFF_NAME: usize = 120; // PyObject *co_name
const OFF_QUALNAME: usize = 128; // PyObject *co_qualname
const OFF_LINETABLE: usize = 136; // PyObject *co_linetable
const OFF_FIRSTTRACEABLE: usize = 184; // int _co_firsttraceable
/// Offset of the flexible `co_code_adaptive[]` member. WeavePy never
/// executes the bytecode, so we allocate a fixed body covering every named
/// field plus a small `co_code_adaptive` head; `tp_basicsize` matches
/// CPython's `sizeof(PyCodeObject)` for a one-unit body.
const CODE_BASE: usize = 200;
/// Total body we allocate per code object (all named fields fit; rounded
/// to 8). CPython would append `(ncodeunits-1)*2` more bytes for the real
/// bytecode, which we deliberately omit (never executed).
const CODE_BODY_SIZE: usize = 208;

/// The `PyObject*` fields a code object owns a strong reference to and must
/// release in `tp_dealloc`. `co_code_adaptive` holds the bytecode *inline*
/// in CPython (the `code` constructor arg is copied, not retained), so it is
/// intentionally not in this list; neither are `freevars`/`cellvars`, which
/// CPython folds into `co_localsplusnames`.
const OWNED_FIELD_OFFSETS: [usize; 8] = [
    OFF_CONSTS,
    OFF_NAMES,
    OFF_EXCEPTIONTABLE,
    OFF_LOCALSPLUSNAMES,
    OFF_FILENAME,
    OFF_NAME,
    OFF_QUALNAME,
    OFF_LINETABLE,
];

// `Py_TPFLAGS_DEFAULT` baseline (`Py_TPFLAGS_HAVE_VERSION_TAG`).
const TPFLAGS_DEFAULT: u64 = 1 << 18;

// ---------------------------------------------------------------------------
// Identity statics. Cython references `&PyCode_Type` / `&PyFrame_Type` /
// `&PyTraceBack_Type` for `Py_IS_TYPE` checks and (for code) as the
// `ob_type` of objects it creates.
// ---------------------------------------------------------------------------
#[no_mangle]
pub static PyCode_Type: StaticType = StaticType::new();
#[no_mangle]
pub static PyFrame_Type: StaticType = StaticType::new();
#[no_mangle]
pub static PyTraceBack_Type: StaticType = StaticType::new();

static INIT_LOCK: Mutex<bool> = Mutex::new(false);

/// Lazily wire the three facade type objects (idempotent). Runs before any
/// code object is created, so `ob_type`/`tp_dealloc`/`tp_basicsize` are
/// valid by the time one exists. Requires `PyType_Type` to be initialised,
/// which it always is by the time an extension's `PyInit_*` runs.
fn ensure_types() {
    let mut done = INIT_LOCK.lock().unwrap();
    if *done {
        return;
    }
    *done = true;
    let meta = crate::types::PyType_Type.as_ptr();
    unsafe {
        let code = &mut *PyCode_Type.as_ptr();
        code.head.ob_refcnt = IMMORTAL_REFCNT;
        code.head.ob_type = meta;
        code.tp_name = b"code\0".as_ptr() as *const c_char;
        code.tp_basicsize = CODE_BODY_SIZE as crate::object::PySsizeT;
        code.tp_itemsize = 2; // sizeof(_Py_CODEUNIT)
        code.tp_flags = TPFLAGS_DEFAULT;
        code.tp_dealloc = Some(code_dealloc);

        for (slot, name) in [
            (PyFrame_Type.as_ptr(), b"frame\0".as_ref()),
            (PyTraceBack_Type.as_ptr(), b"traceback\0".as_ref()),
        ] {
            let ty = &mut *slot;
            ty.head.ob_refcnt = IMMORTAL_REFCNT;
            ty.head.ob_type = meta;
            ty.tp_name = name.as_ptr() as *const c_char;
            ty.tp_flags = TPFLAGS_DEFAULT;
        }
    }
}

#[inline]
unsafe fn write_int(base: *mut u8, off: usize, v: c_int) {
    unsafe { ptr::write_unaligned(base.add(off) as *mut c_int, v) };
}

#[inline]
unsafe fn store_obj(base: *mut u8, off: usize, o: *mut PyObject) {
    if !o.is_null() {
        unsafe { crate::object::Py_IncRef(o) };
    }
    unsafe { ptr::write_unaligned(base.add(off) as *mut *mut PyObject, o) };
}

/// Allocate and zero a faithful `PyCodeObject` body with the head set
/// (`ob_refcnt = 1`, `ob_type = &PyCode_Type`). Returns the object pointer.
unsafe fn alloc_code() -> *mut PyObject {
    ensure_types();
    let layout = Layout::from_size_align(CODE_BODY_SIZE, 8).expect("code layout");
    let raw = unsafe { alloc::alloc_zeroed(layout) };
    if raw.is_null() {
        unsafe { crate::errors::PyErr_NoMemory() };
        return ptr::null_mut();
    }
    let obj = raw as *mut PyObject;
    unsafe {
        (*obj).ob_refcnt = 1;
        (*obj).ob_type = PyCode_Type.as_ptr();
    }
    obj
}

/// `tp_dealloc` for a facade code object: release the owned sub-objects and
/// free the body with the exact layout [`alloc_code`] used.
unsafe extern "C" fn code_dealloc(obj: *mut PyObject) {
    if obj.is_null() {
        return;
    }
    let base = obj as *mut u8;
    for off in OWNED_FIELD_OFFSETS {
        let field = unsafe { ptr::read_unaligned(base.add(off) as *const *mut PyObject) };
        if !field.is_null() {
            unsafe { crate::object::Py_DecRef(field) };
        }
    }
    let layout = Layout::from_size_align(CODE_BODY_SIZE, 8).expect("code layout");
    unsafe { alloc::dealloc(base, layout) };
}

/// `PyUnstable_Code_NewWithPosOnlyArgs` — the 3.13 public code-object
/// constructor Cython emits for every function in `__Pyx_CreateCodeObjects`.
/// We retain the metadata fields (names, filename, qualname, consts) and
/// leave the bytecode (`co_code_adaptive`) empty — WeavePy never runs it.
#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn PyUnstable_Code_NewWithPosOnlyArgs(
    argcount: c_int,
    posonlyargcount: c_int,
    kwonlyargcount: c_int,
    nlocals: c_int,
    stacksize: c_int,
    flags: c_int,
    _code: *mut PyObject,
    consts: *mut PyObject,
    names: *mut PyObject,
    varnames: *mut PyObject,
    _freevars: *mut PyObject,
    _cellvars: *mut PyObject,
    filename: *mut PyObject,
    name: *mut PyObject,
    qualname: *mut PyObject,
    firstlineno: c_int,
    linetable: *mut PyObject,
    exceptiontable: *mut PyObject,
) -> *mut PyObject {
    let obj = unsafe { alloc_code() };
    if obj.is_null() {
        return ptr::null_mut();
    }
    let base = obj as *mut u8;
    unsafe {
        write_int(base, OFF_ARGCOUNT, argcount);
        write_int(base, OFF_POSONLY, posonlyargcount);
        write_int(base, OFF_KWONLY, kwonlyargcount);
        write_int(base, OFF_NLOCALS, nlocals);
        write_int(base, OFF_STACKSIZE, stacksize);
        write_int(base, OFF_FLAGS, flags);
        write_int(base, OFF_FIRSTLINENO, firstlineno);
        store_obj(base, OFF_CONSTS, consts);
        store_obj(base, OFF_NAMES, names);
        store_obj(base, OFF_LOCALSPLUSNAMES, varnames);
        store_obj(base, OFF_FILENAME, filename);
        store_obj(base, OFF_NAME, name);
        store_obj(base, OFF_QUALNAME, qualname);
        store_obj(base, OFF_LINETABLE, linetable);
        store_obj(base, OFF_EXCEPTIONTABLE, exceptiontable);
    }
    obj
}

/// `PyUnstable_Code_New` — same as the pos-only variant with
/// `posonlyargcount == 0` (the 17-arg legacy spelling).
#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn PyUnstable_Code_New(
    argcount: c_int,
    kwonlyargcount: c_int,
    nlocals: c_int,
    stacksize: c_int,
    flags: c_int,
    code: *mut PyObject,
    consts: *mut PyObject,
    names: *mut PyObject,
    varnames: *mut PyObject,
    freevars: *mut PyObject,
    cellvars: *mut PyObject,
    filename: *mut PyObject,
    name: *mut PyObject,
    qualname: *mut PyObject,
    firstlineno: c_int,
    linetable: *mut PyObject,
    exceptiontable: *mut PyObject,
) -> *mut PyObject {
    unsafe {
        PyUnstable_Code_NewWithPosOnlyArgs(
            argcount,
            0,
            kwonlyargcount,
            nlocals,
            stacksize,
            flags,
            code,
            consts,
            names,
            varnames,
            freevars,
            cellvars,
            filename,
            name,
            qualname,
            firstlineno,
            linetable,
            exceptiontable,
        )
    }
}

/// `PyCode_NewEmpty(filename, funcname, firstlineno)` — the traceback
/// builder's minimal code-object constructor. Must return non-NULL or
/// Cython's `__Pyx_AddTraceback` discards the *original* pending exception.
#[no_mangle]
pub unsafe extern "C" fn PyCode_NewEmpty(
    filename: *const c_char,
    funcname: *const c_char,
    firstlineno: c_int,
) -> *mut PyObject {
    if std::env::var_os("WEAVEPY_TRACE_NULL").is_some() {
        let fname = if funcname.is_null() {
            "<null>".to_string()
        } else {
            unsafe { std::ffi::CStr::from_ptr(funcname) }
                .to_string_lossy()
                .into_owned()
        };
        let file = if filename.is_null() {
            "<null>".to_string()
        } else {
            unsafe { std::ffi::CStr::from_ptr(filename) }
                .to_string_lossy()
                .into_owned()
        };
        eprintln!("[WEAVEPY_TRACE_NULL] PyCode_NewEmpty tb-frame: {file}:{firstlineno} in {fname}");
    }
    let obj = unsafe { alloc_code() };
    if obj.is_null() {
        return ptr::null_mut();
    }
    let base = obj as *mut u8;
    unsafe {
        write_int(base, OFF_FIRSTLINENO, firstlineno);
        if !filename.is_null() {
            let f = crate::strings::PyUnicode_FromString(filename);
            // store_obj would double-incref a fresh ref; install directly.
            ptr::write_unaligned(base.add(OFF_FILENAME) as *mut *mut PyObject, f);
        }
        if !funcname.is_null() {
            let n = crate::strings::PyUnicode_FromString(funcname);
            ptr::write_unaligned(base.add(OFF_NAME) as *mut *mut PyObject, n);
        }
    }
    obj
}

// ---------------------------------------------------------------------------
// Frame / traceback. WeavePy has no C-visible frame stack; the only caller
// is Cython's `__Pyx_AddTraceback`, which on a NULL frame simply skips
// appending its synthetic traceback line and lets the *already-restored*
// original exception propagate unchanged.
// ---------------------------------------------------------------------------

/// `PyFrame_New(tstate, code, globals, locals)` — returns NULL (no error
/// set). The caller treats this as "couldn't build a traceback frame" and
/// preserves the pending exception.
#[no_mangle]
pub unsafe extern "C" fn PyFrame_New(
    _tstate: *mut PyThreadState,
    _code: *mut PyObject,
    _globals: *mut PyObject,
    _locals: *mut PyObject,
) -> *mut PyObject {
    ptr::null_mut()
}

/// `PyTraceBack_Here(frame)` — prepend a traceback entry for `frame`.
/// WeavePy keeps tracebacks on the VM side; this C-level shim is a sound
/// no-op (returns success).
#[no_mangle]
pub unsafe extern "C" fn PyTraceBack_Here(_frame: *mut PyObject) -> c_int {
    0
}

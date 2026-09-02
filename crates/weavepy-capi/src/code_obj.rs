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
        (*PyFrame_Type.as_ptr()).tp_dealloc = Some(frame_dealloc);
        (*PyFrame_Type.as_ptr()).tp_basicsize = FRAME_BODY_SIZE as crate::object::PySsizeT;
    }
}

/// `&PyTraceBack_Type`, wired. Used by `type_for_object` so a VM
/// traceback crosses into C wearing the identity static that compiled
/// `PyTraceBack_Check` macros compare against.
pub(crate) fn traceback_type_ptr() -> *mut crate::types::PyTypeObject {
    ensure_types();
    PyTraceBack_Type.as_ptr()
}

/// `&PyFrame_Type`, wired. See [`traceback_type_ptr`].
pub(crate) fn frame_type_ptr() -> *mut crate::types::PyTypeObject {
    ensure_types();
    PyFrame_Type.as_ptr()
}

/// `&PyCode_Type`, wired. See [`traceback_type_ptr`].
pub(crate) fn code_type_ptr() -> *mut crate::types::PyTypeObject {
    ensure_types();
    PyCode_Type.as_ptr()
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

/// Decode a facade code object into a genuine VM `types.CodeType`
/// instance (RFC 0066 WS3), or `None` when `p` is not one. `inspect`'s
/// function-like probe (`isinstance(f.__code__, types.CodeType)`) must
/// pass for a cyfunction's `__code__` — see
/// [`weavepy_vm::builtins::foreign_code_object`]. Called from
/// [`crate::object::clone_object`] at the C→VM crossing; the C-side
/// facade object itself is untouched (Cython keeps writing
/// `_co_firsttraceable` and `Py_DECREF`ing through its own pointer).
pub(crate) unsafe fn native_code_object(p: *mut PyObject) -> Option<weavepy_vm::object::Object> {
    use weavepy_vm::object::Object;
    let ty = unsafe { (*p).ob_type };
    if ty.is_null() || !std::ptr::eq(ty, PyCode_Type.as_ptr()) {
        return None;
    }
    let base = p as *mut u8;
    let read_int =
        |off: usize| unsafe { ptr::read_unaligned(base.add(off) as *const c_int) }.max(0) as u32;
    let read_str = |off: usize| -> String {
        let o = unsafe { ptr::read_unaligned(base.add(off) as *const *mut PyObject) };
        if o.is_null() {
            return String::new();
        }
        match unsafe { crate::object::clone_object(o) } {
            Object::Str(s) => s.to_string(),
            _ => String::new(),
        }
    };
    let varnames = {
        let o =
            unsafe { ptr::read_unaligned(base.add(OFF_LOCALSPLUSNAMES) as *const *mut PyObject) };
        if o.is_null() {
            Vec::new()
        } else {
            match unsafe { crate::object::clone_object(o) } {
                Object::Tuple(t) => t
                    .iter()
                    .map(|it| match it {
                        Object::Str(s) => s.to_string(),
                        _ => String::new(),
                    })
                    .collect(),
                _ => Vec::new(),
            }
        }
    };
    Some(weavepy_vm::builtins::foreign_code_object(
        read_str(OFF_NAME),
        read_str(OFF_QUALNAME),
        read_str(OFF_FILENAME),
        read_int(OFF_FIRSTLINENO),
        read_int(OFF_ARGCOUNT),
        read_int(OFF_POSONLY),
        read_int(OFF_KWONLY),
        read_int(OFF_FLAGS),
        varnames,
    ))
}

// ---------------------------------------------------------------------------
// VM code objects crossing INTO C (RFC 0076 WS5). A VM `Object::Code`
// used to cross as a generic `object` box, so a compiled `PyCode_Check`
// rejected it — torch._dynamo's `skip_code(code)` (module scope of
// `torch/_dynamo/decorators.py`) failed its `THPUtils` guard with
// "expected a code object" and killed the whole lazy `_dynamo` import.
// Mint a faithful facade instead — same layout the C constructors above
// produce — cached per `Rc<CodeObject>` identity, with a reverse payload
// map so the same VM code object round-trips (pointer-keyed C-side
// registries stay coherent and `co is co2` holds back in Python).
// ---------------------------------------------------------------------------

/// `Rc<CodeObject>` address → facade pointer (facades are immortal:
/// the payload map below pins the VM object, and the boxed struct is
/// never freed — code objects that cross are module-lifetime).
static VM_CODE_FACADES: Mutex<Option<std::collections::HashMap<usize, usize>>> = Mutex::new(None);
/// Facade pointer → the original VM `Object::Code`.
static VM_CODE_PAYLOAD: Mutex<
    Option<std::collections::HashMap<usize, weavepy_vm::object::Object>>,
> = Mutex::new(None);

/// Mint (or fetch) the faithful facade for a VM code object crossing
/// into C. Returns a borrowed pointer; the caller increfs per its
/// ownership contract.
pub(crate) fn facade_for_vm_code(obj: &weavepy_vm::object::Object) -> Option<*mut PyObject> {
    use weavepy_vm::object::Object;
    let Object::Code(c) = obj else { return None };
    let key = weavepy_vm::sync::Rc::as_ptr(c) as usize;
    if let Ok(g) = VM_CODE_FACADES.lock() {
        if let Some(m) = g.as_ref() {
            if let Some(&p) = m.get(&key) {
                return Some(p as *mut PyObject);
            }
        }
    }
    let p = unsafe { alloc_code() };
    if p.is_null() {
        return None;
    }
    let base = p as *mut u8;
    let mk_str =
        |s: &str| -> *mut PyObject { crate::object::into_owned(Object::from_str(s.to_owned())) };
    unsafe {
        write_int(base, OFF_ARGCOUNT, c.arg_count as c_int);
        write_int(base, OFF_POSONLY, c.posonly_count as c_int);
        write_int(base, OFF_KWONLY, c.kwonly_count as c_int);
        write_int(base, OFF_NLOCALS, c.varnames.len() as c_int);
        write_int(
            base,
            OFF_FLAGS,
            weavepy_vm::builtins::code_flags(c) as c_int,
        );
        let firstlineno = if c.name == "<module>" {
            1
        } else {
            c.linetable.iter().copied().find(|l| *l > 0).unwrap_or(1)
        };
        write_int(base, OFF_FIRSTLINENO, firstlineno as c_int);
        // Fresh owned references; installed directly (no extra incref).
        ptr::write_unaligned(
            base.add(OFF_FILENAME) as *mut *mut PyObject,
            mk_str(&c.filename),
        );
        ptr::write_unaligned(base.add(OFF_NAME) as *mut *mut PyObject, mk_str(&c.name));
        ptr::write_unaligned(
            base.add(OFF_QUALNAME) as *mut *mut PyObject,
            mk_str(&c.qualname),
        );
        let names: Vec<Object> = c
            .varnames
            .iter()
            .map(|n| Object::from_str(n.clone()))
            .collect();
        ptr::write_unaligned(
            base.add(OFF_LOCALSPLUSNAMES) as *mut *mut PyObject,
            crate::object::into_owned(Object::new_tuple(names)),
        );
    }
    if let Ok(mut g) = VM_CODE_FACADES.lock() {
        g.get_or_insert_with(std::collections::HashMap::new)
            .insert(key, p as usize);
    }
    if let Ok(mut g) = VM_CODE_PAYLOAD.lock() {
        g.get_or_insert_with(std::collections::HashMap::new)
            .insert(p as usize, obj.clone());
    }
    // Immortal: the facade is shared by every crossing of this code
    // object and registered in the payload map for the reverse trip.
    unsafe { (*p).ob_refcnt = IMMORTAL_REFCNT };
    Some(p)
}

/// The original VM `Object::Code` a facade minted by
/// [`facade_for_vm_code`] stands for, if `p` is one.
pub(crate) fn vm_code_payload(p: *mut PyObject) -> Option<weavepy_vm::object::Object> {
    VM_CODE_PAYLOAD
        .lock()
        .ok()
        .and_then(|g| g.as_ref().and_then(|m| m.get(&(p as usize)).cloned()))
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
/// Body size for a facade frame: covers CPython 3.13's `PyFrameObject`
/// through `f_extra_locals` plus slack. The only direct struct access a
/// consumer performs is mypyc's `frame_obj->f_lineno = line` write
/// (offset 40: head 16 + `f_back` 8 + `f_frame` 8 + `f_trace` 8), which
/// lands inside this zeroed block.
const FRAME_BODY_SIZE: usize = 128;

unsafe extern "C" fn frame_dealloc(obj: *mut PyObject) {
    if obj.is_null() {
        return;
    }
    let layout = Layout::from_size_align(FRAME_BODY_SIZE, 8).expect("frame layout");
    unsafe { alloc::dealloc(obj as *mut u8, layout) };
}

/// `PyFrame_New(tstate, code, globals, locals)` — mint a facade frame.
///
/// This must return a real object, not NULL: mypyc's `CPy_AddTraceback`
/// does `PyErr_Fetch` → `PyFrame_New` → `PyErr_Restore` and, if the frame
/// can't be created, takes an error path that *drops the fetched
/// exception* — every failure in a mypyc-compiled module body then
/// surfaces as "init function returned NULL" with no pending exception
/// (RFC 0055 WS5, charset_normalizer). The frame itself is metadata-only:
/// `PyTraceBack_Here` is a VM-side no-op and the caller decrefs it
/// immediately.
#[no_mangle]
pub unsafe extern "C" fn PyFrame_New(
    _tstate: *mut PyThreadState,
    _code: *mut PyObject,
    _globals: *mut PyObject,
    _locals: *mut PyObject,
) -> *mut PyObject {
    ensure_types();
    let layout = Layout::from_size_align(FRAME_BODY_SIZE, 8).expect("frame layout");
    let raw = unsafe { alloc::alloc_zeroed(layout) };
    if raw.is_null() {
        unsafe { crate::errors::PyErr_NoMemory() };
        return ptr::null_mut();
    }
    let obj = raw as *mut PyObject;
    unsafe {
        (*obj).ob_refcnt = 1;
        (*obj).ob_type = PyFrame_Type.as_ptr();
    }
    obj
}

/// `PyTraceBack_Here(frame)` — prepend a traceback entry for `frame`.
/// WeavePy keeps tracebacks on the VM side; this C-level shim is a sound
/// no-op (returns success).
#[no_mangle]
pub unsafe extern "C" fn PyTraceBack_Here(_frame: *mut PyObject) -> c_int {
    0
}

/// `PyFrame_GetCode(frame)` — a *new reference* to the frame's code
/// object, never NULL per CPython's contract. WeavePy's facade frames
/// carry no code, so mint an empty one (RFC 0066 WS3; pybind11's
/// traceback formatter walks frames through this accessor).
#[no_mangle]
pub unsafe extern "C" fn PyFrame_GetCode(_frame: *mut PyObject) -> *mut PyObject {
    unsafe {
        PyCode_NewEmpty(
            b"<weavepy>\0".as_ptr() as *const c_char,
            b"<frame>\0".as_ptr() as *const c_char,
            0,
        )
    }
}

/// `PyFrame_GetBack(frame)` — NULL means "outermost frame" (a valid,
/// non-error answer); facade frames carry no chain.
#[no_mangle]
pub unsafe extern "C" fn PyFrame_GetBack(_frame: *mut PyObject) -> *mut PyObject {
    ptr::null_mut()
}

/// `PyFrame_GetLineNumber(frame)` — facade frames have no position; 0
/// mirrors the "no line" answer callers already handle.
#[no_mangle]
pub unsafe extern "C" fn PyFrame_GetLineNumber(_frame: *mut PyObject) -> c_int {
    0
}

//! RFC 0056 WS5 — the wave-2 binary-wheel symbol burn.
//!
//! The ecosystem rows' compiled wheels (pydantic-core, orjson,
//! cryptography's cffi `_openssl`, aiohttp's multidict, msgpack's
//! Cython packer) lazy-bind a tail of CPython 3.12/3.13 entry points
//! the earlier waves never needed. macOS resolves those stubs at
//! *first call*, so a missing symbol is not a dlopen error but a
//! guaranteed SIGSEGV (`bl` to NULL) the first time the code path
//! runs — msgpack crashed packing its first float. Everything the
//! wave-2 wheel set binds and the earlier waves didn't lives here.

#![allow(clippy::missing_safety_doc)]

use std::cell::RefCell;
use std::collections::HashSet;
use std::os::raw::{c_char, c_int, c_void};
use std::ptr;
use std::sync::Mutex;

use num_bigint::BigInt;
use num_traits::ToPrimitive;
use weavepy_vm::object::Object;
use weavepy_vm::sync::Rc;

use crate::lifecycle::PyThreadState;
use crate::object::{PyObject, PySsizeT};

// ---------------------------------------------------------------------------
// Trivial aliases and process-state predicates
// ---------------------------------------------------------------------------

/// `_Py_IncRef` — the always-exported function spelling behind the
/// `Py_INCREF` macro in limited-API builds.
#[no_mangle]
pub unsafe extern "C" fn _Py_IncRef(op: *mut PyObject) {
    unsafe { crate::object::Py_IncRef(op) }
}

#[no_mangle]
pub unsafe extern "C" fn _Py_DecRef(op: *mut PyObject) {
    unsafe { crate::object::Py_DecRef(op) }
}

/// `Py_IsFinalizing()` — WeavePy tears the VM down from Rust `Drop`
/// order, never through a C-visible finalization phase.
#[no_mangle]
pub unsafe extern "C" fn Py_IsFinalizing() -> c_int {
    0
}

/// `Py_FileSystemDefaultEncoding` — legacy data export (removed from
/// the headers in 3.12 but still bound by cffi's generated glue).
#[no_mangle]
pub static mut Py_FileSystemDefaultEncoding: *const c_char = c"utf-8".as_ptr();

/// `Py_GetConstant(constant_id)` — 3.13 accessor for the immortal
/// singletons and canonical empties (PEP 737 era; pyo3 binds it).
#[no_mangle]
pub unsafe extern "C" fn Py_GetConstant(constant_id: u32) -> *mut PyObject {
    match constant_id {
        0 => crate::singletons::none_ptr(),
        1 => crate::singletons::false_ptr(),
        2 => crate::singletons::true_ptr(),
        3 => crate::singletons::ellipsis_ptr(),
        4 => crate::singletons::not_implemented_ptr(),
        5 => crate::object::into_owned(Object::Int(0)),
        6 => crate::object::into_owned(Object::Int(1)),
        7 => crate::object::into_owned(Object::from_static("")),
        8 => crate::object::into_owned(Object::Bytes(Rc::from(&b""[..]))),
        9 => crate::object::into_owned(Object::new_tuple(vec![])),
        _ => {
            crate::errors::set_runtime_error("Py_GetConstant: unknown constant id");
            ptr::null_mut()
        }
    }
}

/// `Py_GetConstantBorrowed(constant_id)` — the borrowed-reference twin.
/// Under `Py_LIMITED_API >= 0x030D0000` the headers rewrite the classic
/// `Py_None`/`Py_True`/`Py_False` spellings to calls of this function
/// (gh-115754), so every abi3-py313 wheel that so much as touches
/// `Py_RETURN_TRUE` needs it. The constants are immortal here (interned
/// scalars for 0/1, process statics for the rest), so handing out the
/// owned pointer as "borrowed" is sound.
#[no_mangle]
pub unsafe extern "C" fn Py_GetConstantBorrowed(constant_id: u32) -> *mut PyObject {
    unsafe { Py_GetConstant(constant_id) }
}

// ---------------------------------------------------------------------------
// Recursive-repr guard
// ---------------------------------------------------------------------------

thread_local! {
    /// Objects currently inside a `Py_ReprEnter` section on this thread.
    static REPR_STACK: RefCell<HashSet<usize>> = RefCell::new(HashSet::new());
}

/// `Py_ReprEnter(obj)` — 0 to proceed, 1 when `obj` is already being
/// repr'd on this thread (the caller emits `...`), <0 on error.
#[no_mangle]
pub unsafe extern "C" fn Py_ReprEnter(obj: *mut PyObject) -> c_int {
    REPR_STACK.with(|s| {
        if s.borrow_mut().insert(obj as usize) {
            0
        } else {
            1
        }
    })
}

#[no_mangle]
pub unsafe extern "C" fn Py_ReprLeave(obj: *mut PyObject) {
    REPR_STACK.with(|s| {
        s.borrow_mut().remove(&(obj as usize));
    });
}

// ---------------------------------------------------------------------------
// Exceptions
// ---------------------------------------------------------------------------

/// `PyException_GetCause(exc)` — a **new** reference to `exc.__cause__`
/// (NULL when unset), mirroring [`crate::wave5_pandas::PyException_GetTraceback`].
#[no_mangle]
pub unsafe extern "C" fn PyException_GetCause(exc: *mut PyObject) -> *mut PyObject {
    if exc.is_null() {
        return ptr::null_mut();
    }
    let cause = unsafe { crate::abstract_::PyObject_GetAttrString(exc, c"__cause__".as_ptr()) };
    if cause.is_null() {
        crate::errors::clear_thread_local();
        return ptr::null_mut();
    }
    if std::ptr::eq(cause, crate::singletons::none_ptr()) {
        unsafe { crate::object::Py_DecRef(cause) };
        return ptr::null_mut();
    }
    cause
}

/// `PyUnicodeDecodeError_Create(encoding, object, length, start, end,
/// reason)` — build the exception instance by calling the type, exactly
/// as `UnicodeDecodeError(encoding, object, start, end, reason)`.
#[no_mangle]
pub unsafe extern "C" fn PyUnicodeDecodeError_Create(
    encoding: *const c_char,
    object: *const c_char,
    length: PySsizeT,
    start: PySsizeT,
    end: PySsizeT,
    reason: *const c_char,
) -> *mut PyObject {
    let cstr = |p: *const c_char| -> String {
        if p.is_null() {
            String::new()
        } else {
            unsafe { std::ffi::CStr::from_ptr(p) }
                .to_string_lossy()
                .into_owned()
        }
    };
    let len = length.max(0) as usize;
    let data: Rc<[u8]> = if object.is_null() {
        Rc::from(&b""[..])
    } else {
        unsafe { std::slice::from_raw_parts(object as *const u8, len) }.into()
    };
    let ty = unsafe { crate::errors::PyExc_UnicodeDecodeError };
    let args = crate::object::into_owned(Object::new_tuple(vec![
        Object::from_str(cstr(encoding)),
        Object::Bytes(data),
        Object::Int(start as i64),
        Object::Int(end as i64),
        Object::from_str(cstr(reason)),
    ]));
    let exc = unsafe { crate::abstract_::PyObject_CallObject(ty, args) };
    unsafe { crate::object::Py_DecRef(args) };
    exc
}

/// `PyTraceBack_Print(tb, file)` — WeavePy renders tracebacks VM-side;
/// the C hook (cffi's embedding error report) is a best-effort no-op.
#[no_mangle]
pub unsafe extern "C" fn PyTraceBack_Print(_tb: *mut PyObject, _file: *mut PyObject) -> c_int {
    0
}

// ---------------------------------------------------------------------------
// Thread/interpreter state
// ---------------------------------------------------------------------------

/// `PyGILState_GetThisThreadState()` — the calling thread's state,
/// created on demand (WeavePy keeps one faithful body per thread).
#[no_mangle]
pub unsafe extern "C" fn PyGILState_GetThisThreadState() -> *mut PyThreadState {
    crate::pystate::current_threadstate()
}

/// The single interpreter's opaque state block. Over-sized and zeroed
/// so any in-struct read a wheel emits stays in-bounds.
static INTERP_STATE: [u8; 512] = [0u8; 512];

#[no_mangle]
pub unsafe extern "C" fn PyInterpreterState_Get() -> *mut c_void {
    INTERP_STATE.as_ptr() as *mut c_void
}

/// Process-global dict handed out (borrowed) by
/// `PyInterpreterState_GetDict`; cffi stashes its module-key cache here.
static INTERP_DICT: Mutex<usize> = Mutex::new(0);

#[no_mangle]
pub unsafe extern "C" fn PyInterpreterState_GetDict(_interp: *mut c_void) -> *mut PyObject {
    let mut slot = INTERP_DICT.lock().unwrap();
    if *slot == 0 {
        // One immortal box for the life of the process (never decref'd),
        // matching CPython's interpreter-owned dict lifetime.
        *slot = unsafe { crate::containers::PyDict_New() } as usize;
    }
    *slot as *mut PyObject
}

thread_local! {
    /// Per-thread dict for `PyThreadState_GetDict` (borrowed-ref
    /// contract; owned by the thread-local for the thread's life).
    static THREAD_DICT: RefCell<usize> = const { RefCell::new(0) };
}

#[no_mangle]
pub unsafe extern "C" fn PyThreadState_GetDict() -> *mut PyObject {
    THREAD_DICT.with(|cell| {
        let mut slot = cell.borrow_mut();
        if *slot == 0 {
            *slot = unsafe { crate::containers::PyDict_New() } as usize;
        }
        *slot as *mut PyObject
    })
}

/// `PyThreadState_Clear` / `PyThreadState_Delete` — WeavePy thread
/// states are thread-locals reclaimed with the OS thread; the explicit
/// C lifecycle is a no-op (cffi calls these when tearing down a thread
/// it bootstrapped via `PyGILState_Ensure`).
#[no_mangle]
pub unsafe extern "C" fn PyThreadState_Clear(_tstate: *mut PyThreadState) {}

#[no_mangle]
pub unsafe extern "C" fn PyThreadState_Delete(_tstate: *mut PyThreadState) {}

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// `PyType_GetDict(t)` — 3.12 accessor for a **new** reference to the
/// type's dict. WeavePy type mirrors have no C-visible `tp_dict`; hand
/// back a snapshot dict of the native type's namespace (the consumers —
/// orjson's schema cache — only read it).
#[no_mangle]
pub unsafe extern "C" fn PyType_GetDict(ty: *mut PyObject) -> *mut PyObject {
    if ty.is_null() {
        crate::errors::set_type_error("PyType_GetDict: NULL");
        return ptr::null_mut();
    }
    match unsafe { crate::object::clone_object(ty) } {
        Object::Type(t) => {
            let data = t.dict.borrow().clone();
            crate::object::into_owned(Object::Dict(Rc::new(weavepy_vm::sync::RefCell::new(data))))
        }
        _ => {
            crate::errors::set_type_error("PyType_GetDict: not a type");
            ptr::null_mut()
        }
    }
}

/// `PyType_GetModuleName(t)` — 3.13: a new str reference to the type's
/// `__module__`.
#[no_mangle]
pub unsafe extern "C" fn PyType_GetModuleName(ty: *mut PyObject) -> *mut PyObject {
    if ty.is_null() {
        crate::errors::set_type_error("PyType_GetModuleName: NULL");
        return ptr::null_mut();
    }
    let m = unsafe { crate::abstract_::PyObject_GetAttrString(ty, c"__module__".as_ptr()) };
    if m.is_null() {
        crate::errors::clear_thread_local();
        return crate::object::into_owned(Object::from_static("builtins"));
    }
    m
}

/// `(heap type ptr, module ptr)` registrations behind
/// [`PyType_GetModuleByDef`]; filled by `PyType_FromModuleAndSpec`.
pub(crate) static TYPE_MODULES: Mutex<Vec<(usize, usize)>> = Mutex::new(Vec::new());

/// Record that heap type `ty` was created for `module`
/// (`PyType_FromModuleAndSpec`). The module pointer gets one extra
/// reference so the borrowed-ref contract of `GetModuleByDef` holds.
pub(crate) fn register_type_module(ty: *mut PyObject, module: *mut PyObject) {
    if ty.is_null() || module.is_null() {
        return;
    }
    unsafe { crate::object::Py_IncRef(module) };
    TYPE_MODULES
        .lock()
        .unwrap()
        .push((ty as usize, module as usize));
}

/// `PyType_GetModuleByDef(type, def)` — the module a heap type (or one
/// of its bases) was created under. multidict's methods resolve their
/// per-module state through this. Borrowed reference. WeavePy keys the
/// registry by type identity rather than matching `def`, which is
/// equivalent for single-module extensions.
#[no_mangle]
pub unsafe extern "C" fn PyType_GetModuleByDef(
    ty: *mut PyObject,
    _def: *mut c_void,
) -> *mut PyObject {
    let reg = TYPE_MODULES.lock().unwrap();
    for (t, m) in reg.iter() {
        if *t == ty as usize {
            return *m as *mut PyObject;
        }
    }
    // Fall back to walking the MRO for a registered base.
    if let Object::Type(t) = unsafe { crate::object::clone_object(ty) } {
        let mro = t.mro.borrow().clone();
        for base in mro.iter() {
            let base_ptr = crate::types::install_user_type(base) as usize;
            for (t, m) in reg.iter() {
                if *t == base_ptr {
                    return *m as *mut PyObject;
                }
            }
        }
    }
    crate::errors::set_type_error("PyType_GetModuleByDef: no module registered for type");
    ptr::null_mut()
}

// ---------------------------------------------------------------------------
// Weakrefs
// ---------------------------------------------------------------------------

/// `PyWeakref_NewRef(referent, callback)` — a real VM weakref, exactly
/// `weakref.ref(referent, callback)`.
#[no_mangle]
pub unsafe extern "C" fn PyWeakref_NewRef(
    referent: *mut PyObject,
    callback: *mut PyObject,
) -> *mut PyObject {
    if referent.is_null() {
        crate::errors::set_type_error("PyWeakref_NewRef: NULL referent");
        return ptr::null_mut();
    }
    let target = unsafe { crate::object::clone_object(referent) };
    let cb = if callback.is_null() {
        None
    } else {
        match unsafe { crate::object::clone_object(callback) } {
            Object::None => None,
            other => Some(other),
        }
    };
    match weavepy_vm::stdlib::weakref_real::c_new_ref(target, cb) {
        Ok(r) => crate::object::into_owned(r),
        Err(e) => {
            crate::errors::set_pending_from_runtime(e);
            ptr::null_mut()
        }
    }
}

/// `PyWeakref_GetRef(ref, &out)` — 3.13: 1 and a **strong** reference
/// while the referent is alive, 0/NULL once dead, -1 on a non-weakref.
#[no_mangle]
pub unsafe extern "C" fn PyWeakref_GetRef(
    reference: *mut PyObject,
    out: *mut *mut PyObject,
) -> c_int {
    if out.is_null() {
        return -1;
    }
    unsafe { *out = ptr::null_mut() };
    if reference.is_null() {
        crate::errors::set_type_error("PyWeakref_GetRef: NULL");
        return -1;
    }
    let wrapper = unsafe { crate::object::clone_object(reference) };
    match weavepy_vm::stdlib::weakref_real::c_referent(&wrapper) {
        Some(Some(target)) => {
            unsafe { *out = crate::object::into_owned(target) };
            1
        }
        Some(None) => 0,
        None => {
            crate::errors::set_type_error("PyWeakref_GetRef: not a weakref");
            -1
        }
    }
}

// ---------------------------------------------------------------------------
// Calls
// ---------------------------------------------------------------------------

/// `_PyObject_MakeTpCall(tstate, callable, args, nargs, keywords)` —
/// the slow-path materialisation behind the header's inlined
/// `PyObject_VectorcallTstate`; route through the crate's vectorcall.
#[no_mangle]
pub unsafe extern "C" fn _PyObject_MakeTpCall(
    _tstate: *mut PyThreadState,
    callable: *mut PyObject,
    args: *const *mut PyObject,
    nargs: PySsizeT,
    keywords: *mut PyObject,
) -> *mut PyObject {
    unsafe {
        crate::vectorcall::PyObject_Vectorcall(callable, args, nargs.max(0) as usize, keywords)
    }
}

/// `_Py_CheckFunctionResult(tstate, result, callable, where)` — the
/// invariant check CPython runs after every C call: a NULL result must
/// come with a pending exception, a non-NULL result without one.
#[no_mangle]
pub unsafe extern "C" fn _Py_CheckFunctionResult(
    _tstate: *mut PyThreadState,
    result: *mut PyObject,
    _callable: *mut PyObject,
    _where: *const c_char,
) -> *mut PyObject {
    if result.is_null() && unsafe { crate::errors::PyErr_Occurred() }.is_null() {
        crate::errors::set_runtime_error("NULL result without error in _Py_CheckFunctionResult");
    }
    result
}

// ---------------------------------------------------------------------------
// PyLong native-bytes (3.13)
// ---------------------------------------------------------------------------

const NATIVEBYTES_LITTLE_ENDIAN: c_int = 1;
const NATIVEBYTES_NATIVE_ENDIAN: c_int = 3;
const NATIVEBYTES_UNSIGNED_BUFFER: c_int = 4;
const NATIVEBYTES_REJECT_NEGATIVE: c_int = 8;

fn flags_little_endian(flags: c_int) -> bool {
    if flags == -1 {
        return cfg!(target_endian = "little");
    }
    let endian = flags & NATIVEBYTES_NATIVE_ENDIAN;
    if endian == NATIVEBYTES_NATIVE_ENDIAN {
        cfg!(target_endian = "little")
    } else {
        endian == NATIVEBYTES_LITTLE_ENDIAN
    }
}

/// `PyLong_AsNativeBytes(obj, buffer, n_bytes, flags)` — 3.13
/// (pydantic-core's big-int bridge). Returns the number of bytes
/// required (which may exceed `n_bytes`), or -1 with an error set.
#[no_mangle]
pub unsafe extern "C" fn PyLong_AsNativeBytes(
    obj: *mut PyObject,
    buffer: *mut c_void,
    n_bytes: PySsizeT,
    flags: c_int,
) -> PySsizeT {
    if obj.is_null() {
        crate::errors::set_type_error("PyLong_AsNativeBytes: NULL");
        return -1;
    }
    let big = match unsafe { crate::object::clone_object(obj) } {
        Object::Int(i) => BigInt::from(i),
        Object::Long(b) => (*b).clone(),
        Object::Bool(b) => BigInt::from(b as i64),
        _ => {
            crate::errors::set_type_error("PyLong_AsNativeBytes: not an int");
            return -1;
        }
    };
    let negative = big.sign() == num_bigint::Sign::Minus;
    if negative && flags != -1 && flags & NATIVEBYTES_REJECT_NEGATIVE != 0 {
        crate::errors::set_value_error("Cannot convert negative int");
        return -1;
    }
    let unsigned_ok = flags != -1 && flags & NATIVEBYTES_UNSIGNED_BUFFER != 0 && !negative;
    let mut le = if unsigned_ok {
        // The caller treats the buffer as unsigned: no sign-slack byte.
        big.to_bytes_le().1
    } else {
        big.to_signed_bytes_le()
    };
    if le.is_empty() {
        le.push(0);
    }
    let required = le.len() as PySsizeT;
    if !buffer.is_null() && n_bytes > 0 {
        let n = n_bytes as usize;
        let pad = if negative { 0xffu8 } else { 0x00u8 };
        let mut out = le.clone();
        out.resize(n.max(out.len()), pad);
        out.truncate(n);
        if !flags_little_endian(flags) {
            out.reverse();
        }
        unsafe { ptr::copy_nonoverlapping(out.as_ptr(), buffer as *mut u8, n) };
    }
    required
}

/// `PyLong_FromNativeBytes(buffer, n_bytes, flags)` — 3.13 counterpart.
#[no_mangle]
pub unsafe extern "C" fn PyLong_FromNativeBytes(
    buffer: *const c_void,
    n_bytes: usize,
    flags: c_int,
) -> *mut PyObject {
    if buffer.is_null() {
        crate::errors::set_type_error("PyLong_FromNativeBytes: NULL");
        return ptr::null_mut();
    }
    let mut bytes = unsafe { std::slice::from_raw_parts(buffer as *const u8, n_bytes) }.to_vec();
    if !flags_little_endian(flags) {
        bytes.reverse();
    }
    let unsigned = flags != -1 && flags & NATIVEBYTES_UNSIGNED_BUFFER != 0;
    let big = if unsigned {
        BigInt::from_bytes_le(num_bigint::Sign::Plus, &bytes)
    } else {
        BigInt::from_signed_bytes_le(&bytes)
    };
    match big.to_i64() {
        Some(small) => crate::object::into_owned(Object::Int(small)),
        None => crate::object::into_owned(Object::Long(Rc::new(big))),
    }
}

// ---------------------------------------------------------------------------
// Bytes / dict internals
// ---------------------------------------------------------------------------

/// `_PyBytes_Resize(&obj, newsize)` — swap the caller's owned bytes
/// pointer for one of the requested size (contents preserved up to the
/// shorter length, growth zero-filled), CPython's create-then-copy
/// fallback path semantics.
#[no_mangle]
pub unsafe extern "C" fn _PyBytes_Resize(pv: *mut *mut PyObject, newsize: PySsizeT) -> c_int {
    if pv.is_null() || unsafe { *pv }.is_null() || newsize < 0 {
        crate::errors::set_runtime_error("_PyBytes_Resize: bad argument");
        return -1;
    }
    let old = unsafe { *pv };
    let data = match unsafe { crate::object::clone_object_value(old) } {
        Object::Bytes(b) => b,
        _ => {
            crate::errors::set_type_error("_PyBytes_Resize: not bytes");
            unsafe {
                crate::object::Py_DecRef(old);
                *pv = ptr::null_mut();
            }
            return -1;
        }
    };
    let mut v = data.to_vec();
    v.resize(newsize as usize, 0);
    // The caller keeps writing through the inlined `PyBytes_AS_STRING`
    // macro after a resize (orjson's growing output writer), so the
    // replacement must be buffer-authoritative too — a fresh, never-shared
    // mirror whose `ob_sval` is adopted on read-back.
    let fresh = crate::mirror::mirror_out(Object::Bytes(v.into()));
    if !fresh.is_null() && unsafe { crate::mirror::is_mirror(fresh) } {
        unsafe { (*crate::mirror::prefix_of(fresh)).bytes_buffer = true };
    }
    unsafe {
        crate::object::Py_DecRef(old);
        *pv = fresh;
    }
    0
}

/// `_PyDict_SetItem_KnownHash_LockHeld` — WeavePy dicts hash on insert
/// and the GIL serialises access; delegate to the plain setter.
#[no_mangle]
pub unsafe extern "C" fn _PyDict_SetItem_KnownHash_LockHeld(
    dict: *mut PyObject,
    key: *mut PyObject,
    value: *mut PyObject,
    _hash: PySsizeT,
) -> c_int {
    unsafe { crate::containers::PyDict_SetItem(dict, key, value) }
}

// ---------------------------------------------------------------------------
// PyUnicode extras
// ---------------------------------------------------------------------------

/// `PyUnicode_FromObject(o)` — exact `str` passes through (new ref);
/// a subclass is flattened to exact `str`; anything else is a TypeError.
#[no_mangle]
pub unsafe extern "C" fn PyUnicode_FromObject(o: *mut PyObject) -> *mut PyObject {
    if o.is_null() {
        crate::errors::set_type_error("PyUnicode_FromObject: NULL");
        return ptr::null_mut();
    }
    match unsafe { crate::object::clone_object(o) } {
        obj @ (Object::Str(_) | Object::WStr(_)) => crate::object::into_owned(obj),
        Object::Instance(inst) => match inst.native.get() {
            Some(native @ (Object::Str(_) | Object::WStr(_))) => {
                crate::object::into_owned(native.clone())
            }
            _ => {
                crate::errors::set_type_error("Can't convert object to str implicitly");
                ptr::null_mut()
            }
        },
        _ => {
            crate::errors::set_type_error("Can't convert object to str implicitly");
            ptr::null_mut()
        }
    }
}

/// `PyUnicode_DecodeUTF8Stateful(s, size, errors, consumed)` — like
/// `PyUnicode_DecodeUTF8`, but a truncated multi-byte sequence at the
/// *end* of the input is not an error: it's left unconsumed and
/// reported through `consumed`.
#[no_mangle]
pub unsafe extern "C" fn PyUnicode_DecodeUTF8Stateful(
    s: *const c_char,
    size: PySsizeT,
    errors: *const c_char,
    consumed: *mut PySsizeT,
) -> *mut PyObject {
    if consumed.is_null() {
        return unsafe { crate::strings::PyUnicode_DecodeUTF8(s, size, errors) };
    }
    let len = size.max(0) as usize;
    let slice = if s.is_null() {
        &b""[..]
    } else {
        unsafe { std::slice::from_raw_parts(s as *const u8, len) }
    };
    // Find how much of a trailing incomplete sequence to withhold.
    let valid_end = match std::str::from_utf8(slice) {
        Ok(_) => len,
        Err(e) if e.error_len().is_none() => e.valid_up_to(),
        // A hard (non-truncation) error mid-stream: let the strict/errors
        // machinery of the plain decoder deal with the whole input.
        Err(_) => {
            unsafe { *consumed = size };
            return unsafe { crate::strings::PyUnicode_DecodeUTF8(s, size, errors) };
        }
    };
    unsafe { *consumed = valid_end as PySsizeT };
    unsafe { crate::strings::PyUnicode_DecodeUTF8(s, valid_end as PySsizeT, errors) }
}

// ---------------------------------------------------------------------------
// _PyUnicodeWriter — the private incremental string builder
// ---------------------------------------------------------------------------
//
// multidict's C repr builder drives the private writer API and then
// *writes directly into `writer->data`* through the header's inlined
// `PyUnicode_WRITE` after `_PyUnicodeWriter_Prepare` (a macro that only
// calls `_PyUnicodeWriter_PrepareInternal`). The backing store must
// therefore be a real, caller-visible buffer with a truthful `kind`.
// WeavePy always provisions UCS-4 (kind 4, maxchar 0x10FFFF): the
// Prepare macro's `maxchar <= writer->maxchar` test then always passes,
// direct writes land as `u32` code points, and `Finish` re-encodes.

/// Byte-layout twin of CPython 3.13's `_PyUnicodeWriter`.
#[repr(C)]
pub struct UnicodeWriter {
    buffer: *mut PyObject,
    data: *mut c_void,
    kind: c_int,
    maxchar: u32,
    size: PySsizeT,
    pos: PySsizeT,
    min_length: PySsizeT,
    min_char: u32,
    overallocate: u8,
    readonly: u8,
}

#[no_mangle]
pub unsafe extern "C" fn _PyUnicodeWriter_Init(w: *mut UnicodeWriter) {
    unsafe {
        ptr::write(
            w,
            UnicodeWriter {
                buffer: ptr::null_mut(),
                data: ptr::null_mut(),
                kind: 0,
                maxchar: 127,
                size: 0,
                pos: 0,
                min_length: 0,
                min_char: 0,
                overallocate: 0,
                readonly: 0,
            },
        );
    }
}

/// Grow the UCS-4 backing store so at least `length` more code points
/// fit past `pos`. The buffer is a leaked `Vec<u32>` reallocated here;
/// `Finish`/`Dealloc` reclaim it.
#[no_mangle]
pub unsafe extern "C" fn _PyUnicodeWriter_PrepareInternal(
    w: *mut UnicodeWriter,
    length: PySsizeT,
    _maxchar: u32,
) -> c_int {
    let w = unsafe { &mut *w };
    let needed = (w.pos + length.max(0)).max(w.min_length).max(16) as usize;
    let old_cap = w.size as usize;
    if needed <= old_cap && !w.data.is_null() {
        w.maxchar = 0x0010_FFFF;
        w.kind = 4;
        return 0;
    }
    let new_cap = needed.next_power_of_two();
    // A boxed slice (length == capacity by construction) so the pointer can
    // be reclaimed exactly in `Finish`/`Dealloc`.
    let mut buf: Box<[u32]> = vec![0u32; new_cap].into_boxed_slice();
    if !w.data.is_null() {
        unsafe {
            ptr::copy_nonoverlapping(w.data as *const u32, buf.as_mut_ptr(), w.pos as usize);
        }
        // Reclaim the previous allocation.
        drop(unsafe { Box::from_raw(ptr::slice_from_raw_parts_mut(w.data as *mut u32, old_cap)) });
    }
    w.data = Box::into_raw(buf) as *mut c_void;
    w.size = new_cap as PySsizeT;
    w.kind = 4;
    w.maxchar = 0x0010_FFFF;
    0
}

#[no_mangle]
pub unsafe extern "C" fn _PyUnicodeWriter_WriteChar(w: *mut UnicodeWriter, ch: u32) -> c_int {
    if unsafe { _PyUnicodeWriter_PrepareInternal(w, 1, ch) } != 0 {
        return -1;
    }
    let w = unsafe { &mut *w };
    unsafe { *(w.data as *mut u32).add(w.pos as usize) = ch };
    w.pos += 1;
    0
}

#[no_mangle]
pub unsafe extern "C" fn _PyUnicodeWriter_WriteStr(
    w: *mut UnicodeWriter,
    s: *mut PyObject,
) -> c_int {
    if s.is_null() {
        crate::errors::set_type_error("_PyUnicodeWriter_WriteStr: NULL");
        return -1;
    }
    let text = match unsafe { crate::object::clone_object_value(s) } {
        Object::Str(t) => t.to_string(),
        Object::WStr(cps) => cps.iter().filter_map(|&c| char::from_u32(c)).collect(),
        other => other.to_str(),
    };
    let count = text.chars().count() as PySsizeT;
    if unsafe { _PyUnicodeWriter_PrepareInternal(w, count, 0x0010_FFFF) } != 0 {
        return -1;
    }
    let w = unsafe { &mut *w };
    let base = w.data as *mut u32;
    for (i, ch) in text.chars().enumerate() {
        unsafe { *base.add(w.pos as usize + i) = ch as u32 };
    }
    w.pos += count;
    0
}

#[no_mangle]
pub unsafe extern "C" fn _PyUnicodeWriter_Finish(w: *mut UnicodeWriter) -> *mut PyObject {
    let w = unsafe { &mut *w };
    let out: String = if w.data.is_null() {
        String::new()
    } else {
        let slice = unsafe { std::slice::from_raw_parts(w.data as *const u32, w.pos as usize) };
        slice
            .iter()
            .map(|&c| char::from_u32(c).unwrap_or('\u{FFFD}'))
            .collect()
    };
    unsafe { _PyUnicodeWriter_Dealloc(w) };
    crate::object::into_owned(Object::from_str(out))
}

#[no_mangle]
pub unsafe extern "C" fn _PyUnicodeWriter_Dealloc(w: *mut UnicodeWriter) {
    let w = unsafe { &mut *w };
    if !w.data.is_null() {
        let cap = w.size as usize;
        drop(unsafe { Box::from_raw(ptr::slice_from_raw_parts_mut(w.data as *mut u32, cap)) });
        w.data = ptr::null_mut();
        w.size = 0;
        w.pos = 0;
    }
}

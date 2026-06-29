//! RFC 0047 (wave 5): the CPython 3.13 C-API leaf tail that **real
//! numpy.random** and **real pandas** link but that waves 1-4 (and the
//! initial Cython tail in [`crate::wave5`]) had not yet exported.
//!
//! numpy is mostly hand-written C, but `numpy.random` is itself
//! Cython-generated (`_generator`, `_mt19937`, `bit_generator`, …), and
//! pandas is ~70% Cython. They lean on a further cluster of leaves —
//! the single-threaded lock API (`PyThread_allocate_lock` &co), a handful
//! of `PyUnicode_*` builders, the in-place number ops, list-slice
//! assignment, the static-/class-method constructors, and a few data
//! symbols (`PyWrapperDescr_Type`, `_PyByteArray_empty_string`).
//!
//! Found the wave-4 way: diff the undefined `Py*`/`_Py*` symbols of the
//! real `numpy/random/*.so` and `pandas/_libs/**/*.so` against the host's
//! dynamic symbol table. Each entry here is a thin delegator onto the
//! wave-1/2/3 surface or a sound no-op under WeavePy's single-threaded-GIL
//! object model.

#![allow(clippy::missing_safety_doc)]

use core::ffi::{c_char, c_int, c_longlong, c_void};
use std::ptr;

use weavepy_vm::object::Object;

use crate::object::{PyObject, PySsizeT};
use crate::types::PyTypeObject;

// ---------------------------------------------------------------------------
// The single-threaded lock API
// ---------------------------------------------------------------------------
//
// Cython's generated module-exec allocates locks (e.g. for the cached
// `__pyx_*` globals and, in numpy.random, the bit-generator guard).
// WeavePy runs one interpreter thread under a single GIL, so a lock is a
// non-NULL opaque handle whose acquire always succeeds and whose release
// is a no-op — exactly the value CPython's lock returns to an uncontended
// single thread. The handle is a leaked one-byte allocation so each lock
// has a distinct, non-NULL identity (Cython NULL-checks the handle).

/// CPython's `PyLockStatus`: `PY_LOCK_FAILURE = 0`, `PY_LOCK_ACQUIRED = 1`,
/// `PY_LOCK_INTR = 2`.
const PY_LOCK_ACQUIRED: c_int = 1;

#[no_mangle]
pub extern "C" fn PyThread_allocate_lock() -> *mut c_void {
    Box::into_raw(Box::new(0u8)) as *mut c_void
}

#[no_mangle]
pub unsafe extern "C" fn PyThread_free_lock(lock: *mut c_void) {
    if !lock.is_null() {
        unsafe { drop(Box::from_raw(lock as *mut u8)) };
    }
}

/// `PyThread_acquire_lock(lock, waitflag)` — 1 on success, 0 on failure.
/// Uncontended single thread: always succeeds.
#[no_mangle]
pub extern "C" fn PyThread_acquire_lock(_lock: *mut c_void, _waitflag: c_int) -> c_int {
    1
}

#[no_mangle]
pub extern "C" fn PyThread_acquire_lock_timed(
    _lock: *mut c_void,
    _microseconds: c_longlong,
    _intr_flag: c_int,
) -> c_int {
    PY_LOCK_ACQUIRED
}

#[no_mangle]
pub extern "C" fn PyThread_release_lock(_lock: *mut c_void) {}

// ---------------------------------------------------------------------------
// Thread state / module state
// ---------------------------------------------------------------------------

/// `PyThreadState_GetFrame(tstate)` — the current frame (new ref) or NULL.
/// WeavePy executes Python on the Rust stack and exposes no C-level
/// `PyFrameObject` for the running frame, so report "no frame" (CPython
/// permits NULL). Cython uses it only for traceback/line bookkeeping.
#[no_mangle]
pub extern "C" fn PyThreadState_GetFrame(_tstate: *mut c_void) -> *mut PyObject {
    ptr::null_mut()
}

// ---------------------------------------------------------------------------
// Per-module `m_size` state (PEP 3121).
//
// A Cython wheel keeps its globals in a `.so`-static, so it never reads
// `PyModule_GetState`. But a *hand-written* single-phase C extension — pandas'
// vendored ujson (`pandas/_libs/json`) — declares `m_size = sizeof(modulestate)`
// in its `PyModuleDef`, then on init does `PyModule_GetState(module)` and writes
// the cached `decimal.Decimal` type into `state->...`. Returning NULL there is a
// NULL-deref store at import. We therefore allocate the `m_size` block on
// `PyModule_Create2`/multi-phase init and hand it back here.
//
// The block is keyed by the module's *native identity* (the `Rc<PyModule>`
// inner pointer), not its C box pointer: WeavePy mints a fresh `PyObjectBox`
// for a module on every crossing, but the underlying `Rc` is shared, so the
// inner pointer is stable across init *and* later runtime calls (which receive
// the module as `self` / via `PyState_FindModule`). Blocks live for the process
// (modules sit in `sys.modules`), so the owning `Box<[u8]>` is simply retained
// in the registry — no free hook needed.
thread_local! {
    static MODULE_STATE: std::cell::RefCell<std::collections::HashMap<usize, Box<[u8]>>> =
        std::cell::RefCell::new(std::collections::HashMap::new());
    /// `PyState_FindModule` index: `PyModuleDef*` → a stable immortal C box
    /// for the module created from that def. CPython keys its
    /// `interp->modules_by_index` by `def->m_base.m_index`; the def pointer is
    /// the equivalent stable per-extension singleton and avoids mutating the
    /// caller's def. See [`register_find_module`] / [`PyState_FindModule`].
    static FIND_MODULE: std::cell::RefCell<std::collections::HashMap<usize, usize>> =
        std::cell::RefCell::new(std::collections::HashMap::new());
}

/// The native-identity key for a module's C pointer, or `None` if `module`
/// does not resolve to a WeavePy `Object::Module`.
fn module_state_key(module: *mut PyObject) -> Option<usize> {
    if module.is_null() {
        return None;
    }
    match unsafe { crate::object::clone_object(module) } {
        Object::Module(rc) => Some(weavepy_vm::sync::Rc::as_ptr(&rc) as usize),
        _ => None,
    }
}

/// Allocate (once) a zeroed `size`-byte state block for the module identified
/// by `key` and return a stable pointer to it. Idempotent: a repeat call for
/// the same module returns the existing block. `size == 0` yields NULL (CPython
/// reports no state for an `m_size == 0` module). Called from
/// `PyModule_Create2` and the multi-phase init path.
pub(crate) fn ensure_module_state(key: usize, size: usize) -> *mut c_void {
    if size == 0 {
        return ptr::null_mut();
    }
    MODULE_STATE.with(|m| {
        let mut map = m.borrow_mut();
        let buf = map
            .entry(key)
            .or_insert_with(|| vec![0u8; size].into_boxed_slice());
        buf.as_mut_ptr() as *mut c_void
    })
}

/// `PyModule_GetState(module)` — the `m_size` state buffer allocated at module
/// creation (see [`ensure_module_state`]), or NULL for a stateless module.
#[no_mangle]
pub extern "C" fn PyModule_GetState(module: *mut PyObject) -> *mut c_void {
    let Some(key) = module_state_key(module) else {
        return ptr::null_mut();
    };
    MODULE_STATE.with(|m| {
        m.borrow_mut()
            .get_mut(&key)
            .map_or(ptr::null_mut(), |buf| buf.as_mut_ptr() as *mut c_void)
    })
}

/// Register the module created from single-phase `def` so a later
/// `PyState_FindModule(def)` returns it. Mirrors CPython's
/// `_PyState_AddModule` populating `interp->modules_by_index`.
///
/// A stable, *immortal* borrowed box is minted once per def: modules live
/// for the process (they sit in `sys.modules`), which is exactly the lifetime
/// CPython's per-interpreter registry grants the stored reference, so the
/// pointer stays valid for every later lookup. The box resolves to the same
/// native `Object::Module` used at creation, hence to the same
/// [`PyModule_GetState`] block — the whole point of the round-trip: pandas'
/// vendored ujson re-fetches its own module here to read the cached
/// `Series`/`DataFrame`/`Index`/`NaT` types written into that state at import
/// (`object_is_series_type` &co). Idempotent per def.
pub(crate) fn register_find_module(def: *mut c_void, module: Object) {
    if def.is_null() {
        return;
    }
    let key = def as usize;
    if FIND_MODULE.with(|m| m.borrow().contains_key(&key)) {
        return;
    }
    let boxed = crate::object::into_owned(module);
    if boxed.is_null() {
        return;
    }
    // Never reclaimed: this is the registry's long-lived borrowed reference.
    unsafe {
        (*boxed).ob_refcnt = crate::object::IMMORTAL_REFCNT;
    }
    FIND_MODULE.with(|m| {
        m.borrow_mut().insert(key, boxed as usize);
    });
}

/// `PyState_FindModule(def)` — the module previously created from `def`
/// (borrowed ref), or NULL if none was registered (see
/// [`register_find_module`]). NULL is the correct answer for a module that
/// keeps its state in a `.so` static (`m_size < 0`, most Cython modules) and
/// is never registered.
#[no_mangle]
pub extern "C" fn PyState_FindModule(def: *mut c_void) -> *mut PyObject {
    if def.is_null() {
        return ptr::null_mut();
    }
    FIND_MODULE.with(|m| {
        m.borrow()
            .get(&(def as usize))
            .map_or(ptr::null_mut(), |&p| p as *mut PyObject)
    })
}

// ---------------------------------------------------------------------------
// List slice assignment
// ---------------------------------------------------------------------------

/// `PyList_SetSlice(list, low, high, itemlist)` — `list[low:high] = itemlist`
/// (deletion when `itemlist` is NULL). Mirrors CPython's clamping of the
/// bounds to `[0, len]`.
#[no_mangle]
pub unsafe extern "C" fn PyList_SetSlice(
    list: *mut PyObject,
    low: PySsizeT,
    high: PySsizeT,
    itemlist: *mut PyObject,
) -> c_int {
    if list.is_null() {
        crate::errors::set_type_error("PyList_SetSlice: list is NULL");
        return -1;
    }
    let rc = match unsafe { crate::object::clone_object(list) } {
        Object::List(rc) => rc,
        _ => {
            crate::errors::set_type_error("PyList_SetSlice: expected list");
            return -1;
        }
    };
    // Collect the replacement items (empty for a deletion).
    let new_items: Vec<Object> = if itemlist.is_null() {
        Vec::new()
    } else {
        match unsafe { crate::abstract_::collect_iterable(itemlist) } {
            Some(v) => v,
            None => return -1,
        }
    };
    let mut v = rc.borrow_mut();
    let len = v.len() as PySsizeT;
    let lo = low.clamp(0, len) as usize;
    let hi = high.clamp(low.max(0), len) as usize;
    v.splice(lo..hi, new_items);
    0
}

// ---------------------------------------------------------------------------
// Exception traceback accessor
// ---------------------------------------------------------------------------

/// `PyException_GetTraceback(exc)` — a **new** reference to `exc.__traceback__`
/// (or NULL when there is none). Delegates to the attribute lookup; a
/// missing/None traceback maps to NULL with no error set.
#[no_mangle]
pub unsafe extern "C" fn PyException_GetTraceback(exc: *mut PyObject) -> *mut PyObject {
    if exc.is_null() {
        return ptr::null_mut();
    }
    let tb =
        unsafe { crate::abstract_::PyObject_GetAttrString(exc, c"__traceback__".as_ptr()) };
    if tb.is_null() {
        crate::errors::clear_thread_local();
        return ptr::null_mut();
    }
    if std::ptr::eq(tb, crate::singletons::none_ptr()) {
        unsafe { crate::object::Py_DecRef(tb) };
        return ptr::null_mut();
    }
    tb
}

// ---------------------------------------------------------------------------
// static / class method constructors
// ---------------------------------------------------------------------------

/// `PyStaticMethod_New(callable)` — Python `staticmethod(callable)`.
#[no_mangle]
pub unsafe extern "C" fn PyStaticMethod_New(callable: *mut PyObject) -> *mut PyObject {
    if callable.is_null() {
        crate::errors::set_type_error("PyStaticMethod_New: callable is NULL");
        return ptr::null_mut();
    }
    let func = unsafe { crate::object::clone_object(callable) };
    crate::object::into_owned(Object::StaticMethod(weavepy_vm::object::MethodWrapper::new(
        func,
    )))
}

// ---------------------------------------------------------------------------
// Long copy
// ---------------------------------------------------------------------------

/// `_PyLong_Copy(v)` — a new reference to a copy of the int `v`. WeavePy
/// ints are immutable values, so a clone is an identical copy.
#[no_mangle]
pub unsafe extern "C" fn _PyLong_Copy(v: *mut PyObject) -> *mut PyObject {
    if v.is_null() {
        return ptr::null_mut();
    }
    match unsafe { crate::object::clone_object(v) } {
        o @ (Object::Int(_) | Object::Long(_) | Object::Bool(_)) => crate::object::into_owned(o),
        _ => {
            crate::errors::set_type_error("_PyLong_Copy: expected int");
            ptr::null_mut()
        }
    }
}

// ---------------------------------------------------------------------------
// Unicode builders / locale codecs
// ---------------------------------------------------------------------------

/// `PyUnicode_FromWideChar(w, size)` — build a `str` from a `wchar_t`
/// array (4-byte code points on macOS/Linux). `size < 0` means `w` is
/// NUL-terminated.
#[no_mangle]
pub unsafe extern "C" fn PyUnicode_FromWideChar(w: *const i32, size: PySsizeT) -> *mut PyObject {
    if w.is_null() {
        crate::errors::set_value_error("PyUnicode_FromWideChar: NULL pointer");
        return ptr::null_mut();
    }
    let n = if size < 0 {
        let mut n = 0isize;
        while unsafe { *w.offset(n) } != 0 {
            n += 1;
        }
        n as usize
    } else {
        size as usize
    };
    let mut s = String::with_capacity(n);
    for i in 0..n {
        let cp = unsafe { *w.add(i) } as u32;
        s.push(char::from_u32(cp).unwrap_or('\u{FFFD}'));
    }
    crate::object::into_owned(Object::from_str(s))
}

/// `PyUnicode_DecodeLocale(str, errors)` — decode a NUL-terminated C string
/// using the locale encoding. Modern macOS/Linux locales are UTF-8, so this
/// is the UTF-8 decode WeavePy already provides.
#[no_mangle]
pub unsafe extern "C" fn PyUnicode_DecodeLocale(
    s: *const c_char,
    _errors: *const c_char,
) -> *mut PyObject {
    unsafe { crate::strings::PyUnicode_FromString(s) }
}

/// `PyUnicode_EncodeLocale(unicode, errors)` — encode a `str` to a `bytes`
/// object using the locale encoding (UTF-8 here).
#[no_mangle]
pub unsafe extern "C" fn PyUnicode_EncodeLocale(
    unicode: *mut PyObject,
    _errors: *const c_char,
) -> *mut PyObject {
    unsafe { crate::strings::PyUnicode_AsUTF8String(unicode) }
}

/// `PyUnicode_Resize(p_unicode, length)` — resize the string `*p_unicode`
/// to `length` code points (RFC 0047, wave 5). Mints a fresh, writable
/// mirror of the new length preserving the leading characters and the
/// source kind, publishes it through `*p_unicode`, and releases the old
/// reference; the new tail is zero-filled, ready for the
/// `PyUnicode_CopyCharacters` that Cython's in-place concatenation fast
/// path performs next. Returns 0 on success, -1 (with an exception) if
/// `*p_unicode` is not a resizable unicode object.
#[no_mangle]
pub unsafe extern "C" fn PyUnicode_Resize(p_unicode: *mut *mut PyObject, length: PySsizeT) -> c_int {
    if p_unicode.is_null() {
        crate::errors::set_value_error("PyUnicode_Resize: NULL pointer");
        return -1;
    }
    let old = unsafe { *p_unicode };
    if old.is_null() {
        crate::errors::set_value_error("PyUnicode_Resize: NULL string");
        return -1;
    }
    let n = if length < 0 { 0 } else { length as usize };
    let np = unsafe { crate::mirror::resize_unicode(old, n) };
    if np.is_null() {
        crate::errors::set_value_error("PyUnicode_Resize: not a resizable unicode object");
        return -1;
    }
    // `*p_unicode` owned one reference to `old`; it now owns the resized
    // copy. Release the consumed reference (frees `old` when it was the sole
    // owner; leaves it live, copy-on-resize style, when shared).
    unsafe { crate::object::Py_DecRef(old) };
    unsafe { *p_unicode = np };
    0
}

// ---------------------------------------------------------------------------
// Fatal error
// ---------------------------------------------------------------------------

/// `_Py_FatalErrorFunc(func, msg)` — abort with a fatal-error banner
/// (CPython's `Py_FatalError` macro target). Never returns.
#[no_mangle]
pub unsafe extern "C" fn _Py_FatalErrorFunc(func: *const c_char, msg: *const c_char) {
    let f = if func.is_null() {
        "<unknown>".to_owned()
    } else {
        unsafe { core::ffi::CStr::from_ptr(func) }
            .to_string_lossy()
            .into_owned()
    };
    let m = if msg.is_null() {
        "<no message>".to_owned()
    } else {
        unsafe { core::ffi::CStr::from_ptr(msg) }
            .to_string_lossy()
            .into_owned()
    };
    eprintln!("Fatal Python error: in {f}: {m}");
    std::process::abort();
}

// ---------------------------------------------------------------------------
// Data symbols
// ---------------------------------------------------------------------------

/// `_PyByteArray_empty_string` — the shared 1-byte NUL buffer CPython hands
/// back from `PyByteArray_AS_STRING` for an empty bytearray.
#[no_mangle]
pub static _PyByteArray_empty_string: [c_char; 1] = [0];

/// `PyCMethod_New(ml, self, module, cls)` — build a builtin method object
/// from a `PyMethodDef`, binding `self` (and ignoring the defining-class
/// argument used only by `METH_METHOD` introspection). Delegates to the
/// module bridge's C-function wrapper.
#[no_mangle]
pub unsafe extern "C" fn PyCMethod_New(
    ml: *mut crate::module::PyMethodDef,
    self_: *mut PyObject,
    module: *mut PyObject,
    _cls: *mut PyTypeObject,
) -> *mut PyObject {
    unsafe { crate::module::PyCFunction_NewEx(ml, self_, module) }
}

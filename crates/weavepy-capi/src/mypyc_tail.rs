//! RFC 0055 WS5: the C-API tail that **mypyc-compiled** wheels link.
//!
//! mypyc (the compiler behind the binary wheels of `charset_normalizer`'s
//! `md`/`cd` speedups, black, and mypy itself) generates C that leans on a
//! different slice of the CPython surface than Cython does: the ASCII ctype
//! tables (`_Py_ctype_table` et al., used by its inlined `str` helpers), the
//! trashcan protocol (`Py_TRASHCAN_BEGIN`/`END` expand to *direct*
//! `tstate->c_recursion_remaining` / `tstate->delete_later` field access plus
//! calls to `_PyTrash_thread_deposit_object` / `_PyTrash_thread_destroy_chain`),
//! raw `PyLongObject` digit construction (`_PyLong_New`), and a batch of
//! plain functions (`PyUnicode_Count`, `PyDict_MergeFromSeq2`, …).
//!
//! On macOS an extension links with `-undefined dynamic_lookup`: a missing
//! *data* symbol fails at `dlopen` (clean ImportError), but a missing
//! *function* resolves lazily and the first call jumps through NULL —
//! `import charset_normalizer` was a SIGSEGV. Everything mypyc's runtime
//! (`mypyc/lib-rt/CPy.h`) references must therefore exist, even entries
//! whose faithful behaviour matters less than their existence.

#![allow(clippy::missing_safety_doc)]

use core::ffi::{c_char, c_int};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::ptr;
use std::sync::Mutex;

use weavepy_vm::builtin_types::{builtin_types, make_exception_with_class};
use weavepy_vm::object::{DictKey, Object};

use crate::layout::{
    digit, PyLongObject, PYLONG_MASK, PYLONG_NON_SIZE_BITS, PYLONG_SHIFT, PYLONG_SIGN_NEGATIVE,
    PYLONG_SIGN_POSITIVE, PYLONG_SIGN_ZERO,
};
use crate::object::{clone_object, clone_object_value, into_owned, PyObject, Py_DecRef};

fn set_system_error(msg: impl Into<String>) {
    crate::errors::set_pending(
        Some(builtin_types().system_error.clone()),
        Object::from_str(msg.into()),
    );
}

// ---------------------------------------------------------------------------
// ASCII ctype tables (Python/pyctype.c)
// ---------------------------------------------------------------------------

const PY_CTF_LOWER: u32 = 0x01;
const PY_CTF_UPPER: u32 = 0x02;
const PY_CTF_DIGIT: u32 = 0x04;
const PY_CTF_SPACE: u32 = 0x08;
const PY_CTF_XDIGIT: u32 = 0x10;

const fn build_ctype_table() -> [u32; 256] {
    let mut t = [0u32; 256];
    let mut c = 0usize;
    while c < 256 {
        let ch = c as u8;
        let mut f = 0u32;
        if ch.is_ascii_lowercase() {
            f |= PY_CTF_LOWER;
        }
        if ch.is_ascii_uppercase() {
            f |= PY_CTF_UPPER;
        }
        if ch.is_ascii_digit() {
            f |= PY_CTF_DIGIT;
        }
        if matches!(ch, b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r') {
            f |= PY_CTF_SPACE;
        }
        if ch.is_ascii_hexdigit() {
            f |= PY_CTF_XDIGIT;
        }
        t[c] = f;
        c += 1;
    }
    t
}

const fn build_tolower() -> [u8; 256] {
    let mut t = [0u8; 256];
    let mut c = 0usize;
    while c < 256 {
        t[c] = (c as u8).to_ascii_lowercase();
        c += 1;
    }
    t
}

const fn build_toupper() -> [u8; 256] {
    let mut t = [0u8; 256];
    let mut c = 0usize;
    while c < 256 {
        t[c] = (c as u8).to_ascii_uppercase();
        c += 1;
    }
    t
}

/// `const unsigned int _Py_ctype_table[256]` — ASCII classification flags
/// read by the inlined `Py_ISDIGIT`/`Py_ISSPACE`/… macros in stock headers.
#[no_mangle]
pub static _Py_ctype_table: [u32; 256] = build_ctype_table();

/// `const unsigned char _Py_ctype_tolower[256]` (`Py_TOLOWER`).
#[no_mangle]
pub static _Py_ctype_tolower: [u8; 256] = build_tolower();

/// `const unsigned char _Py_ctype_toupper[256]` (`Py_TOUPPER`).
#[no_mangle]
pub static _Py_ctype_toupper: [u8; 256] = build_toupper();

// ---------------------------------------------------------------------------
// Raw PyLongObject construction (_PyLong_New)
// ---------------------------------------------------------------------------

/// Addresses handed out by [`_PyLong_New`]. These blocks have a genuine
/// CPython `PyLongObject` layout — the caller writes limbs into `ob_digit`
/// *after* allocation — so they can be neither a WeavePy box (fixed payload)
/// nor an opaque foreign proxy (the digits must be *decoded* when the value
/// crosses into the VM). `clone_object` and `free_box` consult this set.
static RAW_LONGS: Mutex<Option<HashSet<usize>>> = Mutex::new(None);

fn register_raw_long(p: usize) {
    let mut g = RAW_LONGS.lock().unwrap();
    g.get_or_insert_with(HashSet::new).insert(p);
}

/// Whether `p` came from [`_PyLong_New`] (and is still alive).
pub fn is_raw_long(p: *mut PyObject) -> bool {
    RAW_LONGS
        .lock()
        .ok()
        .and_then(|g| g.as_ref().map(|s| s.contains(&(p as usize))))
        .unwrap_or(false)
}

/// Remove `p` from the raw-long registry; true when it was present.
/// The caller then owns freeing the block with `libc::free`.
pub fn take_raw_long(p: *mut PyObject) -> bool {
    RAW_LONGS
        .lock()
        .ok()
        .and_then(|mut g| g.as_mut().map(|s| s.remove(&(p as usize))))
        .unwrap_or(false)
}

/// Decode a raw `PyLongObject`'s sign tag + 30-bit limbs into a VM int.
pub unsafe fn decode_raw_long(p: *mut PyObject) -> Object {
    let lo = p as *const PyLongObject;
    let lv_tag = unsafe { (*lo).long_value.lv_tag };
    let sign_bits = lv_tag & 0b11;
    if sign_bits == PYLONG_SIGN_ZERO {
        return Object::Int(0);
    }
    let ndigits = lv_tag >> PYLONG_NON_SIZE_BITS;
    let digits_ptr = unsafe { ptr::addr_of!((*lo).long_value.ob_digit) as *const digit };
    let mut mag = num_bigint::BigInt::from(0u32);
    let mut i = ndigits;
    while i > 0 {
        i -= 1;
        let d = unsafe { *digits_ptr.add(i) } & PYLONG_MASK;
        mag = (mag << PYLONG_SHIFT) | num_bigint::BigInt::from(d);
    }
    if sign_bits == PYLONG_SIGN_NEGATIVE {
        mag = -mag;
    }
    Object::int_from_bigint(mag)
}

/// `_PyLong_New(ndigits)` — allocate an uninitialised int of `ndigits`
/// 30-bit limbs for the caller to fill (mypyc's int helpers write
/// `ob_digit[]` directly, then hand the object back through the API).
#[no_mangle]
pub unsafe extern "C" fn _PyLong_New(ndigits: isize) -> *mut PyObject {
    if ndigits < 0 {
        set_system_error("_PyLong_New: negative size");
        return ptr::null_mut();
    }
    let n = (ndigits as usize).max(1);
    // head(16) + lv_tag(8) + limbs, zero-initialised.
    let size = std::mem::size_of::<PyLongObject>() + (n - 1) * std::mem::size_of::<digit>();
    let block = unsafe { libc::calloc(1, size) } as *mut PyLongObject;
    if block.is_null() {
        crate::errors::set_runtime_error("_PyLong_New: out of memory");
        return ptr::null_mut();
    }
    unsafe {
        (*block).ob_base.ob_refcnt = 1;
        (*block).ob_base.ob_type = crate::types::PyLong_Type.as_ptr();
        (*block).long_value.lv_tag = if ndigits == 0 {
            PYLONG_SIGN_ZERO
        } else {
            ((ndigits as usize) << PYLONG_NON_SIZE_BITS) | PYLONG_SIGN_POSITIVE
        };
    }
    register_raw_long(block as usize);
    block as *mut PyObject
}

/// `_PyLong_NumBits(v)` — bits needed for `abs(v)`; `(size_t)-1` on error.
#[no_mangle]
pub unsafe extern "C" fn _PyLong_NumBits(v: *mut PyObject) -> usize {
    let o = if is_raw_long(v) {
        unsafe { decode_raw_long(v) }
    } else {
        unsafe { clone_object_value(v) }
    };
    match o {
        Object::Int(i) => (i.unsigned_abs().checked_ilog2().map_or(0, |b| b + 1)) as usize,
        Object::Bool(b) => usize::from(b),
        Object::Long(b) => b.bits() as usize,
        _ => {
            crate::errors::set_type_error("_PyLong_NumBits: an int is required");
            usize::MAX
        }
    }
}

// ---------------------------------------------------------------------------
// Trashcan (Py_TRASHCAN_BEGIN / Py_TRASHCAN_END)
// ---------------------------------------------------------------------------

thread_local! {
    /// Deposited-but-not-yet-destroyed objects (CPython chains them through
    /// the GC header; a plain side list is equivalent).
    static TRASH: RefCell<Vec<usize>> = const { RefCell::new(Vec::new()) };
}

/// `_PyTrash_thread_deposit_object(tstate, op)` — park `op` (refcount
/// already 0) for a later, shallower `tp_dealloc`. `tstate->delete_later`
/// must become non-NULL: `Py_TRASHCAN_END` reads the field *directly* to
/// decide whether to call [`_PyTrash_thread_destroy_chain`].
#[no_mangle]
pub unsafe extern "C" fn _PyTrash_thread_deposit_object(
    _tstate: *mut crate::lifecycle::PyThreadState,
    op: *mut PyObject,
) {
    TRASH.with(|t| t.borrow_mut().push(op as usize));
    unsafe { *crate::pystate::delete_later_slot() = op };
}

/// `_PyTrash_thread_destroy_chain(tstate)` — drain the deposit list,
/// running each object's real `tp_dealloc` now that the stack is shallow.
#[no_mangle]
pub unsafe extern "C" fn _PyTrash_thread_destroy_chain(
    _tstate: *mut crate::lifecycle::PyThreadState,
) {
    while let Some(op) = TRASH.with(|t| t.borrow_mut().pop()) {
        let op = op as *mut PyObject;
        unsafe {
            *crate::pystate::delete_later_slot() = if TRASH.with(|t| t.borrow().is_empty()) {
                ptr::null_mut()
            } else {
                op
            };
            let ty = (*op).ob_type;
            if !ty.is_null() {
                if let Some(dealloc) = (*ty).tp_dealloc {
                    dealloc(op);
                }
            }
        }
    }
    unsafe { *crate::pystate::delete_later_slot() = ptr::null_mut() };
}

// ---------------------------------------------------------------------------
// Exception-state plumbing
// ---------------------------------------------------------------------------

/// `PyErr_GetExcInfo(&ptype, &pvalue, &ptb)` — the *handled* exception
/// (`sys.exc_info()` shape), from the `tstate->exc_info` stack item that
/// [`PyErr_SetExcInfo`] maintains. mypyc saves/restores this around its
/// generated `try`/`finally` and generator resumes.
#[no_mangle]
pub unsafe extern "C" fn PyErr_GetExcInfo(
    ptype: *mut *mut PyObject,
    pvalue: *mut *mut PyObject,
    ptraceback: *mut *mut PyObject,
) {
    let value = unsafe { *crate::pystate::exc_info_value_slot() };
    if value.is_null() {
        unsafe {
            *ptype = ptr::null_mut();
            *pvalue = ptr::null_mut();
            *ptraceback = ptr::null_mut();
        }
        return;
    }
    let obj = unsafe { clone_object(value) };
    let ty = match &obj {
        Object::Instance(inst) => into_owned(Object::Type(inst.cls())),
        _ => ptr::null_mut(),
    };
    let tb = match &obj {
        Object::Instance(inst) => {
            let key = DictKey(Object::from_static("__traceback__"));
            match inst.dict.borrow().get(&key) {
                Some(t @ Object::Traceback(_)) => into_owned(t.clone()),
                _ => ptr::null_mut(),
            }
        }
        _ => ptr::null_mut(),
    };
    unsafe {
        *ptype = ty;
        crate::object::Py_IncRef(value);
        *pvalue = value;
        *ptraceback = tb;
    }
}

/// `PyErr_GetHandledException()` — the 3.11+ single-object spelling of
/// [`PyErr_GetExcInfo`]: a **new reference** to the currently handled
/// exception (the `tstate->exc_info->exc_value` slot), or NULL. Cython's
/// `__Pyx_GetException` calls it (via `PyErr_SetHandledException`'s
/// sibling) around every `except` block it compiles — gevent's
/// `TrackedRawGreenlet.__init__` (`except ValueError` around
/// `sys._getframe`) jumped to a NULL stub without this export
/// (RFC 0072 WS2).
#[no_mangle]
pub unsafe extern "C" fn PyErr_GetHandledException() -> *mut PyObject {
    let value = unsafe { *crate::pystate::exc_info_value_slot() };
    if value.is_null() {
        return ptr::null_mut();
    }
    unsafe {
        crate::object::Py_IncRef(value);
    }
    value
}

/// `PyErr_SetHandledException(exc)` — install the handled exception.
/// Does **not** steal `exc` (CPython holds a new reference); `None` and
/// NULL both clear the slot.
#[no_mangle]
pub unsafe extern "C" fn PyErr_SetHandledException(exc: *mut PyObject) {
    let slot = crate::pystate::exc_info_value_slot();
    unsafe {
        let old = *slot;
        if exc.is_null() || matches!(clone_object(exc), Object::None) {
            *slot = ptr::null_mut();
        } else {
            crate::object::Py_IncRef(exc);
            *slot = exc;
        }
        if !old.is_null() {
            Py_DecRef(old);
        }
    }
}

/// `PyErr_SetExcInfo(ty, value, tb)` — install the handled exception.
/// Steals all three references; only `value` is retained (the 3.12+
/// single-object model), the type/traceback are derivable from it.
#[no_mangle]
pub unsafe extern "C" fn PyErr_SetExcInfo(
    ty: *mut PyObject,
    value: *mut PyObject,
    traceback: *mut PyObject,
) {
    let slot = crate::pystate::exc_info_value_slot();
    unsafe {
        let old = *slot;
        // `None` means "no handled exception" (CPython normalises to NULL).
        if !value.is_null() && matches!(clone_object(value), Object::None) {
            *slot = ptr::null_mut();
            Py_DecRef(value);
        } else {
            *slot = value;
        }
        if !old.is_null() {
            Py_DecRef(old);
        }
        if !ty.is_null() {
            Py_DecRef(ty);
        }
        if !traceback.is_null() {
            Py_DecRef(traceback);
        }
    }
}

/// `PyErr_SetImportError(msg, name, path)` — an `ImportError` whose
/// `.name`/`.path` attributes are set (each may be NULL → None).
#[no_mangle]
pub unsafe extern "C" fn PyErr_SetImportError(
    msg: *mut PyObject,
    name: *mut PyObject,
    path: *mut PyObject,
) -> *mut PyObject {
    if msg.is_null() {
        crate::errors::set_type_error("PyErr_SetImportError: msg must be set");
        return ptr::null_mut();
    }
    let text = match unsafe { clone_object_value(msg) } {
        Object::Str(s) => s.to_string(),
        other => other.to_str(),
    };
    let inst = make_exception_with_class(builtin_types().import_error.clone(), text);
    if let Object::Instance(i) = &inst {
        let name_o = if name.is_null() {
            Object::None
        } else {
            unsafe { clone_object(name) }
        };
        let path_o = if path.is_null() {
            Object::None
        } else {
            unsafe { clone_object(path) }
        };
        let mut d = i.dict.borrow_mut();
        d.insert(DictKey(Object::from_static("name")), name_o);
        d.insert(DictKey(Object::from_static("path")), path_o);
    }
    crate::errors::set_pending(None, inst);
    ptr::null_mut()
}

/// `_PyErr_SetKeyError(key)` — `KeyError(key)` with the *object* as args
/// (not its string form), so `except KeyError as e: e.args[0]` round-trips.
#[no_mangle]
pub unsafe extern "C" fn _PyErr_SetKeyError(key: *mut PyObject) {
    let key_o = unsafe { clone_object(key) };
    crate::errors::set_pending(
        Some(builtin_types().key_error.clone()),
        Object::new_tuple(vec![key_o]),
    );
}

/// `_PyErr_ChainExceptions1(exc)` — if an error is pending, make `exc`
/// its `__context__`; otherwise drop `exc`. Steals the reference.
#[no_mangle]
pub unsafe extern "C" fn _PyErr_ChainExceptions1(exc: *mut PyObject) {
    if exc.is_null() {
        return;
    }
    if crate::errors::pending().is_some() {
        let ctx = unsafe { clone_object(exc) };
        if let Some(p) = crate::errors::pending() {
            if let Object::Instance(inst) = &p.value {
                inst.dict
                    .borrow_mut()
                    .insert(DictKey(Object::from_static("__context__")), ctx);
            }
        }
    }
    unsafe { Py_DecRef(exc) };
}

/// `_PyGen_FetchStopIterationValue(&pvalue)` — a pending `StopIteration`
/// is consumed and its `.value` handed out (0); any other pending error is
/// left in place (-1); no error means `*pvalue = NULL` (0).
#[no_mangle]
pub unsafe extern "C" fn _PyGen_FetchStopIterationValue(pvalue: *mut *mut PyObject) -> c_int {
    let Some(p) = crate::errors::pending() else {
        unsafe { *pvalue = ptr::null_mut() };
        return 0;
    };
    let is_stop = match &p.ty {
        Some(t) => t.is_subclass_of(&builtin_types().stop_iteration),
        None => false,
    };
    if !is_stop {
        return -1;
    }
    let taken = crate::errors::take_pending().map(|p| p.value);
    let value = match taken {
        Some(Object::Instance(inst)) => inst
            .dict
            .borrow()
            .get(&DictKey(Object::from_static("value")))
            .cloned()
            .unwrap_or(Object::None),
        _ => Object::None,
    };
    unsafe { *pvalue = into_owned(value) };
    0
}

// Two-piece plumbing for `_PyErr_FormatFromCause` (the varargs shell lives
// in `varargs.c`): detach the pending exception before formatting, then
// re-attach it as the fresh exception's `__cause__` and `__context__`.

/// Detach and return the pending exception (an owned token for
/// [`_WeavePy_ApplyCause`]); NULL when none is pending.
#[no_mangle]
pub unsafe extern "C" fn _WeavePy_FetchForCause() -> *mut PyObject {
    unsafe { crate::errors::PyErr_GetRaisedException() }
}

/// Attach `cause` (from [`_WeavePy_FetchForCause`]) to the now-pending
/// exception as both `__cause__` and `__context__`. Steals the reference.
#[no_mangle]
pub unsafe extern "C" fn _WeavePy_ApplyCause(cause: *mut PyObject) {
    if cause.is_null() {
        return;
    }
    let cause_o = unsafe { clone_object(cause) };
    if let Some(p) = crate::errors::pending() {
        if let Object::Instance(inst) = &p.value {
            let mut d = inst.dict.borrow_mut();
            d.insert(DictKey(Object::from_static("__cause__")), cause_o.clone());
            d.insert(DictKey(Object::from_static("__context__")), cause_o);
        }
    }
    unsafe { Py_DecRef(cause) };
}

// ---------------------------------------------------------------------------
// Containers
// ---------------------------------------------------------------------------

/// `PyDict_MergeFromSeq2(d, seq2, override)` — update `d` from an iterable
/// of key/value pairs; `override == 0` keeps existing keys.
#[no_mangle]
pub unsafe extern "C" fn PyDict_MergeFromSeq2(
    d: *mut PyObject,
    seq2: *mut PyObject,
    override_: c_int,
) -> c_int {
    let it = unsafe { crate::abstract_::PyObject_GetIter(seq2) };
    if it.is_null() {
        return -1;
    }
    loop {
        let item = unsafe { crate::abstract_::PyIter_Next(it) };
        if item.is_null() {
            unsafe { Py_DecRef(it) };
            return if crate::errors::pending().is_some() {
                -1
            } else {
                0
            };
        }
        let pair = unsafe { clone_object(item) };
        let kv: Option<(Object, Object)> = match &pair {
            Object::Tuple(t) if t.len() == 2 => Some((t[0].clone(), t[1].clone())),
            Object::List(l) if l.borrow().len() == 2 => {
                let b = l.borrow();
                Some((b[0].clone(), b[1].clone()))
            }
            _ => None,
        };
        unsafe { Py_DecRef(item) };
        let Some((k, v)) = kv else {
            crate::errors::set_value_error(
                "dictionary update sequence element is not a 2-sequence",
            );
            unsafe { Py_DecRef(it) };
            return -1;
        };
        if override_ == 0 {
            let kp = into_owned(k.clone());
            let has = unsafe { crate::containers::PyDict_Contains(d, kp) };
            unsafe { Py_DecRef(kp) };
            match has {
                1 => continue,
                0 => {}
                _ => {
                    unsafe { Py_DecRef(it) };
                    return -1;
                }
            }
        }
        let kp = into_owned(k);
        let vp = into_owned(v);
        let rc = unsafe { crate::containers::PyDict_SetItem(d, kp, vp) };
        unsafe {
            Py_DecRef(kp);
            Py_DecRef(vp);
        }
        if rc != 0 {
            unsafe { Py_DecRef(it) };
            return -1;
        }
    }
}

/// `PyList_Clear(list)` (3.13) — remove all items.
#[no_mangle]
pub unsafe extern "C" fn PyList_Clear(list: *mut PyObject) -> c_int {
    match unsafe { clone_object(list) } {
        Object::List(l) => {
            l.borrow_mut().clear();
            0
        }
        _ => {
            crate::errors::set_type_error("PyList_Clear: expected list");
            -1
        }
    }
}

/// `PyList_GetSlice(list, low, high)` — a new list of the clamped range.
#[no_mangle]
pub unsafe extern "C" fn PyList_GetSlice(
    list: *mut PyObject,
    low: isize,
    high: isize,
) -> *mut PyObject {
    match unsafe { clone_object(list) } {
        Object::List(l) => {
            let items = l.borrow();
            let n = items.len() as isize;
            let lo = low.clamp(0, n) as usize;
            let hi = high.clamp(0, n) as usize;
            let slice: Vec<Object> = if lo < hi {
                items[lo..hi].to_vec()
            } else {
                Vec::new()
            };
            drop(items);
            into_owned(Object::new_list(slice))
        }
        _ => {
            crate::errors::set_type_error("PyList_GetSlice: expected list");
            ptr::null_mut()
        }
    }
}

// ---------------------------------------------------------------------------
// Functions / generators / modules
// ---------------------------------------------------------------------------

/// Identity-stable owned boxes for [`PyFunction_GetAnnotations`]'s
/// *borrowed*-reference contract: one pinned box per function, live for the
/// process (functions queried through this API are module-level and
/// effectively immortal).
static ANNOTATIONS_CACHE: Mutex<Option<HashMap<usize, usize>>> = Mutex::new(None);

/// `PyFunction_GetAnnotations(func)` — the function's `__annotations__`
/// dict, or NULL without error when there are none. Borrowed reference.
#[no_mangle]
pub unsafe extern "C" fn PyFunction_GetAnnotations(func: *mut PyObject) -> *mut PyObject {
    if let Some(g) = ANNOTATIONS_CACHE.lock().ok().as_ref() {
        if let Some(m) = g.as_ref() {
            if let Some(&p) = m.get(&(func as usize)) {
                return p as *mut PyObject;
            }
        }
    }
    let p = unsafe {
        crate::abstract_::PyObject_GetAttrString(
            func,
            b"__annotations__\0".as_ptr() as *const c_char,
        )
    };
    if p.is_null() {
        crate::errors::clear_thread_local();
        return ptr::null_mut();
    }
    let mut g = ANNOTATIONS_CACHE.lock().unwrap();
    g.get_or_insert_with(HashMap::new)
        .insert(func as usize, p as usize);
    p
}

/// `PyGen_GetCode(gen)` — the generator's code object (new reference).
#[no_mangle]
pub unsafe extern "C" fn PyGen_GetCode(gen: *mut PyObject) -> *mut PyObject {
    unsafe { crate::abstract_::PyObject_GetAttrString(gen, b"gi_code\0".as_ptr() as *const c_char) }
}

/// `PyModule_GetFilenameObject(m)` — `m.__file__` as str (new reference),
/// else `SystemError`.
#[no_mangle]
pub unsafe extern "C" fn PyModule_GetFilenameObject(m: *mut PyObject) -> *mut PyObject {
    let file = match unsafe { clone_object(m) } {
        Object::Module(module) => module
            .dict
            .borrow()
            .get(&DictKey(Object::from_static("__file__")))
            .cloned(),
        _ => None,
    };
    match file {
        Some(f @ Object::Str(_)) => into_owned(f),
        _ => {
            set_system_error("module filename missing");
            ptr::null_mut()
        }
    }
}

// ---------------------------------------------------------------------------
// str / bytes operations
// ---------------------------------------------------------------------------

fn str_arg(p: *mut PyObject, who: &str) -> Option<String> {
    match unsafe { clone_object_value(p) } {
        Object::Str(s) => Some(s.to_string()),
        _ => {
            crate::errors::set_type_error(format!("{who}: expected str"));
            None
        }
    }
}

/// `PyUnicode_Append(&left, right)` — in-place concat: `*pleft` is replaced
/// with the concatenation (old reference released); NULL on error.
#[no_mangle]
pub unsafe extern "C" fn PyUnicode_Append(pleft: *mut *mut PyObject, right: *mut PyObject) {
    if pleft.is_null() {
        return;
    }
    let left = unsafe { *pleft };
    if left.is_null() || right.is_null() {
        return;
    }
    let (Some(a), Some(b)) = (
        str_arg(left, "PyUnicode_Append"),
        str_arg(right, "PyUnicode_Append"),
    ) else {
        unsafe {
            Py_DecRef(left);
            *pleft = ptr::null_mut();
        }
        return;
    };
    let joined = into_owned(Object::from_str(format!("{a}{b}")));
    unsafe {
        Py_DecRef(left);
        *pleft = joined;
    }
}

/// Clamp CPython slice-style `start`/`end` (in code points) onto `len`.
fn adjust_indices(len: isize, mut start: isize, mut end: isize) -> (usize, usize) {
    if end > len {
        end = len;
    } else if end < 0 {
        end += len;
        if end < 0 {
            end = 0;
        }
    }
    if start < 0 {
        start += len;
        if start < 0 {
            start = 0;
        }
    }
    (start as usize, end.max(0) as usize)
}

/// `PyUnicode_Count(s, sub, start, end)` — non-overlapping occurrences of
/// `sub` in `s[start:end]` (code-point indices).
#[no_mangle]
pub unsafe extern "C" fn PyUnicode_Count(
    s: *mut PyObject,
    sub: *mut PyObject,
    start: isize,
    end: isize,
) -> isize {
    let (Some(hay), Some(needle)) = (
        str_arg(s, "PyUnicode_Count"),
        str_arg(sub, "PyUnicode_Count"),
    ) else {
        return -1;
    };
    let chars: Vec<char> = hay.chars().collect();
    let (lo, hi) = adjust_indices(chars.len() as isize, start, end);
    if lo >= hi && !(needle.is_empty() && lo == hi) {
        return if needle.is_empty() && lo <= chars.len() {
            1
        } else {
            0
        };
    }
    let window: String = chars[lo.min(chars.len())..hi.min(chars.len())]
        .iter()
        .collect();
    if needle.is_empty() {
        return window.chars().count() as isize + 1;
    }
    let mut count = 0isize;
    let mut at = 0usize;
    while let Some(pos) = window[at..].find(&needle) {
        count += 1;
        at += pos + needle.len();
    }
    count
}

/// `PyUnicode_RSplit(s, sep, maxsplit)` — `str.rsplit`; a NULL `sep` splits
/// on runs of whitespace.
#[no_mangle]
pub unsafe extern "C" fn PyUnicode_RSplit(
    s: *mut PyObject,
    sep: *mut PyObject,
    maxsplit: isize,
) -> *mut PyObject {
    let Some(hay) = str_arg(s, "PyUnicode_RSplit") else {
        return ptr::null_mut();
    };
    let limit = if maxsplit < 0 {
        usize::MAX
    } else {
        maxsplit as usize
    };
    let mut parts: Vec<String> = Vec::new();
    if sep.is_null() {
        let mut rest = hay.trim_end();
        while !rest.is_empty() && parts.len() < limit {
            match rest.rfind(char::is_whitespace) {
                Some(i) => {
                    parts.push(rest[i + char_len_at(rest, i)..].to_owned());
                    rest = rest[..i].trim_end();
                }
                None => break,
            }
        }
        if !rest.is_empty() {
            parts.push(rest.to_owned());
        }
    } else {
        let Some(sep_s) = str_arg(sep, "PyUnicode_RSplit") else {
            return ptr::null_mut();
        };
        if sep_s.is_empty() {
            crate::errors::set_value_error("empty separator");
            return ptr::null_mut();
        }
        let mut rest: &str = &hay;
        while parts.len() < limit {
            match rest.rfind(&sep_s) {
                Some(i) => {
                    parts.push(rest[i + sep_s.len()..].to_owned());
                    rest = &rest[..i];
                }
                None => break,
            }
        }
        parts.push(rest.to_owned());
    }
    parts.reverse();
    let items: Vec<Object> = parts.into_iter().map(Object::from_str).collect();
    into_owned(Object::new_list(items))
}

/// Byte length of the UTF-8 char starting at byte `i` of `s`.
fn char_len_at(s: &str, i: usize) -> usize {
    s[i..].chars().next().map_or(1, char::len_utf8)
}

/// `_PyUnicode_Equal(a, b)` — exact string equality (both must be str).
#[no_mangle]
pub unsafe extern "C" fn _PyUnicode_Equal(a: *mut PyObject, b: *mut PyObject) -> c_int {
    if a == b {
        return 1;
    }
    match (unsafe { clone_object_value(a) }, unsafe {
        clone_object_value(b)
    }) {
        (Object::Str(x), Object::Str(y)) => c_int::from(x == y),
        _ => 0,
    }
}

/// `_PyBytes_Join(sep, iterable)` — `sep.join(iterable)` over bytes-likes.
#[no_mangle]
pub unsafe extern "C" fn _PyBytes_Join(sep: *mut PyObject, x: *mut PyObject) -> *mut PyObject {
    let sep_b = match unsafe { clone_object_value(sep) } {
        Object::Bytes(b) => b.to_vec(),
        _ => {
            crate::errors::set_type_error("_PyBytes_Join: sep must be bytes");
            return ptr::null_mut();
        }
    };
    let it = unsafe { crate::abstract_::PyObject_GetIter(x) };
    if it.is_null() {
        return ptr::null_mut();
    }
    let mut out: Vec<u8> = Vec::new();
    let mut first = true;
    loop {
        let item = unsafe { crate::abstract_::PyIter_Next(it) };
        if item.is_null() {
            unsafe { Py_DecRef(it) };
            if crate::errors::pending().is_some() {
                return ptr::null_mut();
            }
            return into_owned(Object::Bytes(weavepy_vm::sync::Rc::from(
                out.into_boxed_slice(),
            )));
        }
        let chunk = match unsafe { clone_object_value(item) } {
            Object::Bytes(b) => b.to_vec(),
            Object::ByteArray(b) => b.borrow().clone(),
            _ => {
                unsafe {
                    Py_DecRef(item);
                    Py_DecRef(it);
                }
                crate::errors::set_type_error("sequence item: expected a bytes-like object");
                return ptr::null_mut();
            }
        };
        unsafe { Py_DecRef(item) };
        if !first {
            out.extend_from_slice(&sep_b);
        }
        first = false;
        out.extend_from_slice(&chunk);
    }
}

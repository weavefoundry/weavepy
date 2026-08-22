//! RFC 0046 (wave 4): the CPython 3.13 C-API tail that stock numpy's
//! `_multiarray_umath` extension links but that waves 1-3 had not yet
//! exported.
//!
//! These are the leaf entry points discovered by diffing the undefined
//! `Py*`/`_Py*` symbols of a from-source-built `numpy-2.x`
//! `_multiarray_umath.cpython-313-*.so` against the host binary's
//! dynamic symbol table. Most delegate to the existing wave-1/2/3
//! surface (`crate::abstract_`, `crate::containers`, `crate::numbers`,
//! `crate::strings`); a handful that have no behavioural meaning under
//! WeavePy's single-threaded-GIL, non-tracemalloc runtime are sound
//! no-ops (matching what CPython does when the corresponding subsystem
//! is disabled).
//!
//! The variadic members of the tail (`PyOS_snprintf`, `PyErr_WarnFormat`)
//! cannot be expressed as Rust `extern "C"` definitions and live in
//! `src/varargs.c` instead.

#![allow(clippy::missing_safety_doc)]

use core::ffi::{c_char, c_double, c_int, c_long, c_uint, c_ulong, c_void};
use std::ptr;

use weavepy_vm::object::Object;

use crate::object::PyObject;

/// Return a new (owned) reference to `p`, or NULL unchanged.
unsafe fn new_ref(p: *mut PyObject) -> *mut PyObject {
    if !p.is_null() {
        unsafe { crate::object::Py_IncRef(p) };
    }
    p
}

/// Pin cache for the handful of C-API functions that return a *borrowed*
/// reference (`PySys_GetObject`, `PyEval_GetBuiltins`).
///
/// WeavePy mints a fresh `PyObject` box every time a VM value crosses the
/// boundary, so there is no persistent owner to borrow from: decref'ing
/// the freshly-minted box (as the "borrowed" contract would imply) frees
/// it and hands the caller a dangling pointer. Instead we mint once,
/// retain the reference forever (pinning it — a bounded, per-key leak),
/// and return the same stable pointer on every subsequent call. This both
/// satisfies the borrowed contract (the caller must not decref) and gives
/// the object stable identity across calls, which numpy relies on.
fn pinned_borrowed(key: String, produce: impl FnOnce() -> *mut PyObject) -> *mut PyObject {
    use std::collections::HashMap;
    use std::sync::Mutex;
    static CACHE: Mutex<Option<HashMap<String, usize>>> = Mutex::new(None);

    {
        let guard = CACHE.lock().unwrap();
        if let Some(map) = guard.as_ref() {
            if let Some(&addr) = map.get(&key) {
                return addr as *mut PyObject;
            }
        }
    }
    // Produce outside the lock (it re-enters the interpreter / capi).
    let p = produce();
    if p.is_null() {
        return ptr::null_mut();
    }
    let mut guard = CACHE.lock().unwrap();
    let map = guard.get_or_insert_with(HashMap::new);
    // Another thread may have produced concurrently; keep the first.
    if let Some(&addr) = map.get(&key) {
        unsafe { crate::object::Py_DecRef(p) };
        return addr as *mut PyObject;
    }
    map.insert(key, p as usize);
    p
}

/// A fresh owned reference to `None`.
unsafe fn new_none() -> *mut PyObject {
    unsafe { new_ref(crate::singletons::none_ptr()) }
}

// ---------------------------------------------------------------------------
// Predicates
// ---------------------------------------------------------------------------

#[no_mangle]
pub unsafe extern "C" fn PyCallable_Check(o: *mut PyObject) -> c_int {
    if o.is_null() {
        return 0;
    }
    // CPython's `PyCallable_Check` is `Py_TYPE(o)->tp_call != NULL` — a *type*
    // property, not an instance `__call__` attribute lookup. WeavePy's
    // intrinsically-callable object kinds (plain/lambda functions, builtins,
    // types, and bound/class/static methods) carry no `__call__` entry in a
    // type dict, so the old `PyObject_HasAttrString(o, "__call__")` probe
    // returned 0 for them through the C-API — even though the VM's `callable()`
    // reports true. Cython compiles `callable(x)` to this function, so pandas'
    // C parser (`parsers.pyx`: `not callable(self.usecols)` → `len(usecols)`,
    // `not callable(self.skiprows)` → `for _ in skiprows`) then treated a
    // callable `usecols`/`skiprows` as a sequence and raised
    // "object of type 'function' has no len()". Recognise the callable kinds
    // directly, matching `tp_call`.
    let obj = unsafe { crate::object::clone_object(o) };
    match &obj {
        Object::Function(_)
        | Object::Builtin(_)
        | Object::Type(_)
        | Object::BoundMethod(_)
        | Object::ClassMethod(_)
        | Object::StaticMethod(_) => return 1,
        // A pure-Python instance is callable iff its class defines `__call__`.
        Object::Instance(inst) => {
            return c_int::from(inst.cls().lookup("__call__").is_some());
        }
        // A foreign extension object is callable iff its type installs a
        // `tp_call` slot (numpy ufuncs, `functools.partial` bridged in, …).
        Object::Foreign(_) => {
            let tp = unsafe { (*o).ob_type };
            if !tp.is_null() && !unsafe { (*tp).tp_call }.is_null() {
                return 1;
            }
        }
        _ => {}
    }
    // Fallback for anything else: a `__call__` on the type still means callable.
    unsafe { crate::abstract_::PyObject_HasAttrString(o, b"__call__\0".as_ptr() as *const c_char) }
}

#[no_mangle]
pub unsafe extern "C" fn PyIndex_Check(o: *mut PyObject) -> c_int {
    if o.is_null() {
        return 0;
    }
    match unsafe { crate::object::clone_object(o) } {
        Object::Int(_) | Object::Long(_) | Object::Bool(_) => 1,
        _ => unsafe {
            crate::abstract_::PyObject_HasAttrString(o, b"__index__\0".as_ptr() as *const c_char)
        },
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyIter_Check(o: *mut PyObject) -> c_int {
    if o.is_null() {
        return 0;
    }
    unsafe { crate::abstract_::PyObject_HasAttrString(o, b"__next__\0".as_ptr() as *const c_char) }
}

#[no_mangle]
pub unsafe extern "C" fn PyObject_SelfIter(o: *mut PyObject) -> *mut PyObject {
    unsafe { new_ref(o) }
}

#[no_mangle]
pub unsafe extern "C" fn PySeqIter_New(seq: *mut PyObject) -> *mut PyObject {
    if seq.is_null() {
        crate::errors::set_value_error("PySeqIter_New: NULL sequence");
        return ptr::null_mut();
    }
    // CPython's `PySeqIter_New` builds a `seqiterobject`: a lazy iterator
    // that indexes `seq[0]`, `seq[1]`, … via `__getitem__` and stops on
    // `IndexError`. It must NOT call `seq.__iter__` — numpy's `array_iter`
    // (the ndarray `tp_iter`) returns `PySeqIter_New(self)`, so delegating
    // to `PyObject_GetIter` here would loop `__iter__` → `array_iter` →
    // `PySeqIter_New` → `__iter__` … and overflow the stack.
    let obj = unsafe { crate::object::clone_object(seq) };
    match crate::interp::with_interp_mut(|interp| interp.seq_iter_object(obj)) {
        Some(Ok(it)) => crate::object::into_owned(it),
        Some(Err(err)) => {
            crate::errors::set_pending_from_runtime(err);
            ptr::null_mut()
        }
        None => {
            crate::errors::set_runtime_error("PySeqIter_New: no active interpreter");
            ptr::null_mut()
        }
    }
}

// ---------------------------------------------------------------------------
// Sound no-ops (subsystem disabled under WeavePy)
// ---------------------------------------------------------------------------

/// No pending signals to deliver here; numpy polls this in long loops.
#[no_mangle]
pub extern "C" fn PyErr_CheckSignals() -> c_int {
    0
}

/// `PyTraceMalloc_Track(domain, ptr, size)` — record a C-allocated block
/// in the given tracemalloc domain (RFC 0047, wave 5). pandas' khash
/// allocator tracks every hashtable buffer this way (domain 472) and its
/// test suite asserts exact byte accounting through
/// `Snapshot.filter_traces((DomainFilter(True, domain),))`. Returns 0 on
/// success, -2 when tracing is disabled (CPython's contract).
#[no_mangle]
pub extern "C" fn PyTraceMalloc_Track(domain: c_uint, ptr: usize, size: usize) -> c_int {
    if weavepy_vm::stdlib::tracemalloc_real::track_domain(domain, ptr, size as u64) {
        0
    } else {
        -2
    }
}

#[no_mangle]
pub extern "C" fn PyTraceMalloc_Untrack(domain: c_uint, ptr: usize) -> c_int {
    if weavepy_vm::stdlib::tracemalloc_real::untrack_domain(domain, ptr) {
        0
    } else {
        -2
    }
}

/// `PyType_Modified(type)` — the documented contract for C code that
/// mutated `tp_dict` directly (bypassing `type.__setattr__`). The VM keeps
/// per-type caches that such surgery can invalidate: the attribute
/// resolution version (`TypeObject::attr_version`, guarding specialized
/// LOAD_ATTR/STORE_ATTR sites), the `__getattribute__`/`__setattr__`
/// classification, the `__del__` finalizer classification, and the
/// `_PyType_Lookup` borrowed-pointer memo. Flush them all for the type and
/// (transitively, where the walk is built in) its subclasses.
#[no_mangle]
pub extern "C" fn PyType_Modified(ty: *mut PyObject) {
    if ty.is_null() {
        return;
    }
    if let Object::Type(t) = unsafe { crate::object::clone_object(ty) } {
        t.bump_attr_version();
        t.invalidate_getattribute_cache();
        t.invalidate_finalizer_cache();
    }
    crate::wave5::flush_type_lookup_cache();
}

/// `PyMutex` is uncontended under WeavePy's GIL-serialised execution.
#[no_mangle]
pub extern "C" fn PyMutex_Lock(_m: *mut c_void) {}

#[no_mangle]
pub extern "C" fn PyMutex_Unlock(_m: *mut c_void) {}

/// Weakrefs are cleared by the VM when the referent is collected; the
/// explicit C hook is a no-op.
#[no_mangle]
pub extern "C" fn PyObject_ClearWeakRefs(_o: *mut PyObject) {}

/// Discard the pending exception (CPython prints it to `sys.stderr`;
/// numpy calls this only on best-effort cleanup paths).
#[no_mangle]
pub extern "C" fn PyErr_WriteUnraisable(_o: *mut PyObject) {
    crate::errors::clear_thread_local();
}

// ---------------------------------------------------------------------------
// Exception chaining
// ---------------------------------------------------------------------------

#[no_mangle]
pub unsafe extern "C" fn PyException_SetCause(ex: *mut PyObject, cause: *mut PyObject) {
    // Steals a reference to `cause`.
    unsafe {
        crate::abstract_::PyObject_SetAttrString(
            ex,
            b"__cause__\0".as_ptr() as *const c_char,
            cause,
        );
        if !cause.is_null() {
            crate::object::Py_DecRef(cause);
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyException_SetContext(ex: *mut PyObject, context: *mut PyObject) {
    // Steals a reference to `context`.
    unsafe {
        crate::abstract_::PyObject_SetAttrString(
            ex,
            b"__context__\0".as_ptr() as *const c_char,
            context,
        );
        if !context.is_null() {
            crate::object::Py_DecRef(context);
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyException_SetTraceback(ex: *mut PyObject, tb: *mut PyObject) -> c_int {
    // Borrows `tb` (does not steal).
    unsafe {
        crate::abstract_::PyObject_SetAttrString(
            ex,
            b"__traceback__\0".as_ptr() as *const c_char,
            tb,
        )
    }
}

// ---------------------------------------------------------------------------
// Dict tail (3.13 *Ref accessors + string-keyed helpers)
// ---------------------------------------------------------------------------

#[no_mangle]
pub unsafe extern "C" fn PyDict_GetItemWithError(
    d: *mut PyObject,
    key: *mut PyObject,
) -> *mut PyObject {
    unsafe { crate::containers::PyDict_GetItem(d, key) }
}

#[no_mangle]
pub unsafe extern "C" fn PyDict_GetItemRef(
    d: *mut PyObject,
    key: *mut PyObject,
    result: *mut *mut PyObject,
) -> c_int {
    let v = unsafe { crate::containers::PyDict_GetItem(d, key) };
    if v.is_null() {
        if !result.is_null() {
            unsafe { *result = ptr::null_mut() };
        }
        return 0;
    }
    if !result.is_null() {
        unsafe { *result = new_ref(v) };
    }
    1
}

#[no_mangle]
pub unsafe extern "C" fn PyDict_GetItemStringRef(
    d: *mut PyObject,
    key: *const c_char,
    result: *mut *mut PyObject,
) -> c_int {
    let v = unsafe { crate::containers::PyDict_GetItemString(d, key) };
    if v.is_null() {
        if !result.is_null() {
            unsafe { *result = ptr::null_mut() };
        }
        return 0;
    }
    if !result.is_null() {
        unsafe { *result = new_ref(v) };
    }
    1
}

#[no_mangle]
pub unsafe extern "C" fn PyDict_ContainsString(d: *mut PyObject, key: *const c_char) -> c_int {
    let v = unsafe { crate::containers::PyDict_GetItemString(d, key) };
    c_int::from(!v.is_null())
}

#[no_mangle]
pub unsafe extern "C" fn PyDict_SetDefaultRef(
    d: *mut PyObject,
    key: *mut PyObject,
    default_value: *mut PyObject,
    result: *mut *mut PyObject,
) -> c_int {
    // CPython's `PyDict_SetDefaultRef` returns 1 when the key was already
    // present (and yields its value), 0 when it was missing and
    // `default_value` was inserted, -1 on error. numpy's `PyUFunc_AddLoop`
    // treats a `1` as "loop already registered", so the polarity is load-
    // bearing — returning it inverted made every fresh ufunc loop look like
    // a duplicate ("A loop/promoter has already been registered…").
    let existing = unsafe { crate::containers::PyDict_GetItem(d, key) };
    if !existing.is_null() {
        if !result.is_null() {
            unsafe { *result = new_ref(existing) };
        }
        return 1;
    }
    if unsafe { crate::containers::PyDict_SetItem(d, key, default_value) } < 0 {
        if !result.is_null() {
            unsafe { *result = ptr::null_mut() };
        }
        return -1;
    }
    if !result.is_null() {
        unsafe { *result = new_ref(default_value) };
    }
    0
}

#[no_mangle]
pub unsafe extern "C" fn PyDictProxy_New(mapping: *mut PyObject) -> *mut PyObject {
    // A read-only view; WeavePy does not yet mint a distinct
    // mappingproxy, so we hand back the mapping itself (reads are
    // faithful; the immutability guard is a known wave-4 simplification).
    unsafe { new_ref(mapping) }
}

// ---------------------------------------------------------------------------
// Internal call helpers
// ---------------------------------------------------------------------------

/// Build a Python tuple owning new references to `items`.
unsafe fn pack_tuple(items: &[*mut PyObject]) -> *mut PyObject {
    let t = unsafe { crate::containers::PyTuple_New(items.len() as isize) };
    if t.is_null() {
        return ptr::null_mut();
    }
    for (i, &it) in items.iter().enumerate() {
        // PyTuple_SetItem steals a reference, so hand it a new one.
        unsafe { crate::containers::PyTuple_SetItem(t, i as isize, new_ref(it)) };
    }
    t
}

/// `getattr(import(modname), attr)(*args)`.
unsafe fn call_module_attr(
    modname: *const c_char,
    attr: *const c_char,
    args: &[*mut PyObject],
) -> *mut PyObject {
    let module = unsafe { crate::module::PyImport_ImportModule(modname) };
    if module.is_null() {
        return ptr::null_mut();
    }
    let func = unsafe { crate::abstract_::PyObject_GetAttrString(module, attr) };
    unsafe { crate::object::Py_DecRef(module) };
    if func.is_null() {
        return ptr::null_mut();
    }
    let argt = unsafe { pack_tuple(args) };
    if argt.is_null() {
        unsafe { crate::object::Py_DecRef(func) };
        return ptr::null_mut();
    }
    let res = unsafe { crate::abstract_::PyObject_CallObject(func, argt) };
    unsafe {
        crate::object::Py_DecRef(func);
        crate::object::Py_DecRef(argt);
    }
    res
}

// ---------------------------------------------------------------------------
// Numbers
// ---------------------------------------------------------------------------

#[no_mangle]
pub unsafe extern "C" fn PyComplex_AsCComplex(op: *mut PyObject) -> crate::layout::PyComplexValue {
    // Single conversion pass: computing real and imag with two independent
    // `PyComplex_RealAsDouble`/`PyComplex_ImagAsDouble` calls let the imag
    // call's `__complex__` probe clear the `TypeError` the real call had set
    // for an unconvertible object, so numpy's object→complex cast stored
    // garbage instead of raising (see `numbers::complex_as_ccomplex`).
    unsafe { crate::numbers::complex_as_ccomplex(op) }
}

#[no_mangle]
pub unsafe extern "C" fn PyComplex_FromCComplex(c: crate::layout::PyComplexValue) -> *mut PyObject {
    unsafe { crate::numbers::PyComplex_FromDoubles(c.real, c.imag) }
}

#[no_mangle]
pub unsafe extern "C" fn _PyLong_Sign(v: *mut PyObject) -> c_int {
    match unsafe { crate::object::clone_object(v) } {
        Object::Bool(b) => c_int::from(b),
        Object::Int(i) => match i.cmp(&0) {
            std::cmp::Ordering::Less => -1,
            std::cmp::Ordering::Equal => 0,
            std::cmp::Ordering::Greater => 1,
        },
        Object::Long(big) => match big.sign() {
            num_bigint::Sign::Minus => -1,
            num_bigint::Sign::NoSign => 0,
            num_bigint::Sign::Plus => 1,
        },
        _ => 0,
    }
}

/// Hash a `double` consistently with WeavePy's own `float` hashing (so a
/// numpy scalar and the equal Python `float` land in the same dict slot).
///
/// CPython 3.10+ (gh-43475) hashes a NaN by the *instance's* identity
/// (`PyObject_GenericHash(inst)`), so two distinct NaN-valued scalars —
/// numpy's `float64.tp_hash` calls this with the scalar itself — hash apart
/// while the same object hashes stably. Mirror that with the same
/// pointer-rotate CPython uses; a NULL `inst` (pandas' `_Pandas_HashDouble`
/// passes NULL, though it filters NaN itself) falls back to 0.
#[no_mangle]
pub unsafe extern "C" fn _Py_HashDouble(inst: *mut PyObject, v: c_double) -> isize {
    if v.is_nan() {
        if inst.is_null() {
            return 0;
        }
        let h = (inst as usize).rotate_right(4) as isize;
        return if h == -1 { -2 } else { h };
    }
    let f = unsafe { crate::numbers::PyFloat_FromDouble(v) };
    if f.is_null() {
        return 0;
    }
    let h = unsafe { crate::abstract_::PyObject_Hash(f) };
    unsafe { crate::object::Py_DecRef(f) };
    h
}

#[no_mangle]
pub unsafe extern "C" fn PyLong_FromUnicodeObject(u: *mut PyObject, base: c_int) -> *mut PyObject {
    let s = unsafe { crate::strings::PyUnicode_AsUTF8(u) };
    if s.is_null() {
        return ptr::null_mut();
    }
    unsafe { crate::numbers::PyLong_FromString(s, ptr::null_mut(), base) }
}

#[no_mangle]
pub unsafe extern "C" fn PyFloat_FromString(s: *mut PyObject) -> *mut PyObject {
    let text = match unsafe { crate::object::clone_object_value(s) } {
        Object::Str(t) => t.to_string(),
        Object::Bytes(b) => String::from_utf8_lossy(&b).into_owned(),
        _ => {
            crate::errors::set_type_error("PyFloat_FromString: argument must be string");
            return ptr::null_mut();
        }
    };
    match text.trim().parse::<f64>() {
        // `PyFloat_FromDouble` gives a NaN a fresh identity tag.
        Ok(v) => unsafe { crate::numbers::PyFloat_FromDouble(v) },
        Err(_) => {
            crate::errors::set_value_error(format!("could not convert string to float: '{text}'"));
            ptr::null_mut()
        }
    }
}

// ---------------------------------------------------------------------------
// Unicode tail
// ---------------------------------------------------------------------------

#[no_mangle]
pub unsafe extern "C" fn PyUnicode_AsUCS4(
    u: *mut PyObject,
    buffer: *mut u32,
    buflen: isize,
    copy_null: c_int,
) -> *mut u32 {
    let text = match unsafe { crate::object::clone_object_value(u) } {
        Object::Str(t) => t.to_string(),
        _ => {
            crate::errors::set_type_error("PyUnicode_AsUCS4: argument must be str");
            return ptr::null_mut();
        }
    };
    let chars: Vec<u32> = text.chars().map(|c| c as u32).collect();
    let need = chars.len() + if copy_null != 0 { 1 } else { 0 };
    if std::env::var_os("WEAVEPY_TRACE_UCS4").is_some() {
        eprintln!(
            "[UCS4] AsUCS4({u:p}, buf={buffer:p}, buflen={buflen}, copy_null={copy_null}) value={text:?} nchars={}",
            chars.len()
        );
    }
    if buflen < need as isize {
        crate::errors::set_value_error("PyUnicode_AsUCS4: buffer too small");
        return ptr::null_mut();
    }
    unsafe {
        for (i, &c) in chars.iter().enumerate() {
            *buffer.add(i) = c;
        }
        if copy_null != 0 {
            *buffer.add(chars.len()) = 0;
        }
    }
    buffer
}

#[no_mangle]
pub unsafe extern "C" fn PyUnicode_AsUCS4Copy(u: *mut PyObject) -> *mut u32 {
    let text = match unsafe { crate::object::clone_object_value(u) } {
        Object::Str(t) => t.to_string(),
        _ => {
            crate::errors::set_type_error("PyUnicode_AsUCS4Copy: argument must be str");
            return ptr::null_mut();
        }
    };
    let chars: Vec<u32> = text.chars().map(|c| c as u32).collect();
    let n = chars.len() + 1;
    let buf = unsafe { crate::memory::PyMem_Malloc(n * 4) } as *mut u32;
    if buf.is_null() {
        return ptr::null_mut();
    }
    unsafe {
        for (i, &c) in chars.iter().enumerate() {
            *buf.add(i) = c;
        }
        *buf.add(chars.len()) = 0;
    }
    buf
}

#[no_mangle]
pub unsafe extern "C" fn PyUnicode_Format(
    format: *mut PyObject,
    args: *mut PyObject,
) -> *mut PyObject {
    unsafe { crate::abstract_::PyNumber_Remainder(format, args) }
}

macro_rules! ucs4_classifier {
    ($($name:ident => $f:expr);* $(;)?) => {
        $(
            #[no_mangle]
            pub extern "C" fn $name(ch: u32) -> c_int {
                match char::from_u32(ch) {
                    Some(c) => c_int::from($f(c)),
                    None => 0,
                }
            }
        )*
    };
}

ucs4_classifier! {
    _PyUnicode_IsAlpha => |c: char| c.is_alphabetic();
    // `Numeric_Type`-faithful predicates (shared with `str.isdecimal/isdigit/isnumeric`);
    // Rust's `is_ascii_digit`/`is_numeric` diverge from CPython on non-ASCII digits,
    // superscripts, fractions, and CJK numerals.
    _PyUnicode_IsDecimalDigit => |c: char| weavepy_vm::unicode_numeric::is_decimal_char(c);
    _PyUnicode_IsDigit => |c: char| weavepy_vm::unicode_numeric::is_digit_char(c);
    _PyUnicode_IsNumeric => |c: char| weavepy_vm::unicode_numeric::is_numeric_char(c);
    _PyUnicode_IsLowercase => |c: char| c.is_lowercase();
    _PyUnicode_IsUppercase => |c: char| c.is_uppercase();
    _PyUnicode_IsTitlecase => |c: char| c.is_uppercase() && !c.is_lowercase() && c.is_alphabetic() && false;
    _PyUnicode_IsWhitespace => |c: char| c.is_whitespace();
}

/// CPython's `_Py_ascii_whitespace[128]` table: 1 at the ASCII
/// whitespace code points (`\t \n \v \f \r`, `0x1c-0x1f`, space).
#[no_mangle]
pub static _Py_ascii_whitespace: [u8; 128] = {
    let mut t = [0u8; 128];
    t[0x09] = 1;
    t[0x0a] = 1;
    t[0x0b] = 1;
    t[0x0c] = 1;
    t[0x0d] = 1;
    t[0x1c] = 1;
    t[0x1d] = 1;
    t[0x1e] = 1;
    t[0x1f] = 1;
    t[0x20] = 1;
    t
};

// ---------------------------------------------------------------------------
// OS string parsing
// ---------------------------------------------------------------------------

extern "C" {
    fn strtod(s: *const c_char, endptr: *mut *mut c_char) -> c_double;
    fn strtol(s: *const c_char, endptr: *mut *mut c_char, base: c_int) -> c_long;
    fn strtoul(s: *const c_char, endptr: *mut *mut c_char, base: c_int) -> c_ulong;
}

#[no_mangle]
pub unsafe extern "C" fn PyOS_string_to_double(
    s: *const c_char,
    endptr: *mut *mut c_char,
    _overflow_exception: *mut PyObject,
) -> c_double {
    let mut local: *mut c_char = ptr::null_mut();
    let v = unsafe { strtod(s, &mut local) };
    if endptr.is_null() {
        // No endptr means the whole string must convert.
        if !local.is_null() && unsafe { *local } != 0 {
            crate::errors::set_value_error("could not convert string to float");
            return -1.0;
        }
    } else {
        unsafe { *endptr = local };
    }
    v
}

#[no_mangle]
pub unsafe extern "C" fn PyOS_strtol(
    s: *const c_char,
    endptr: *mut *mut c_char,
    base: c_int,
) -> c_long {
    unsafe { strtol(s, endptr, base) }
}

#[no_mangle]
pub unsafe extern "C" fn PyOS_strtoul(
    s: *const c_char,
    endptr: *mut *mut c_char,
    base: c_int,
) -> c_ulong {
    unsafe { strtoul(s, endptr, base) }
}

// ---------------------------------------------------------------------------
// Object tail
// ---------------------------------------------------------------------------

#[no_mangle]
pub unsafe extern "C" fn PyObject_AsFileDescriptor(o: *mut PyObject) -> c_int {
    if let Object::Int(i) = unsafe { crate::object::clone_object(o) } {
        return i as c_int;
    }
    // Fall back to `o.fileno()`.
    let meth = unsafe {
        crate::abstract_::PyObject_GetAttrString(o, b"fileno\0".as_ptr() as *const c_char)
    };
    if meth.is_null() {
        crate::errors::set_type_error("argument must be an int, or have a fileno() method");
        return -1;
    }
    let empty = unsafe { crate::containers::PyTuple_New(0) };
    let res = unsafe { crate::abstract_::PyObject_CallObject(meth, empty) };
    unsafe {
        crate::object::Py_DecRef(meth);
        crate::object::Py_DecRef(empty);
    }
    if res.is_null() {
        return -1;
    }
    let fd = unsafe { crate::numbers::PyLong_AsLong(res) };
    unsafe { crate::object::Py_DecRef(res) };
    fd as c_int
}

#[no_mangle]
pub unsafe extern "C" fn PyObject_GetOptionalAttr(
    obj: *mut PyObject,
    name: *mut PyObject,
    result: *mut *mut PyObject,
) -> c_int {
    let v = unsafe { crate::abstract_::PyObject_GetAttr(obj, name) };
    if !v.is_null() {
        if !result.is_null() {
            unsafe { *result = v };
        } else {
            unsafe { crate::object::Py_DecRef(v) };
        }
        return 1;
    }
    if !result.is_null() {
        unsafe { *result = ptr::null_mut() };
    }
    // Suppress only `AttributeError`; anything else propagates as -1 with
    // the error pending (see `errors::optional_probe_failure`).
    crate::errors::optional_probe_failure(
        &weavepy_vm::builtin_types::builtin_types().attribute_error,
    )
}

#[no_mangle]
pub unsafe extern "C" fn PyObject_Print(o: *mut PyObject, fp: *mut c_void, flags: c_int) -> c_int {
    extern "C" {
        fn fwrite(ptr: *const c_void, size: usize, n: usize, stream: *mut c_void) -> usize;
    }
    let text = if flags & 1 != 0 {
        unsafe { crate::abstract_::PyObject_Str(o) }
    } else {
        unsafe { crate::abstract_::PyObject_Repr(o) }
    };
    if text.is_null() {
        return -1;
    }
    let mut len: isize = 0;
    let utf8 = unsafe { crate::strings::PyUnicode_AsUTF8AndSize(text, &mut len) };
    if !utf8.is_null() && !fp.is_null() {
        unsafe { fwrite(utf8 as *const c_void, 1, len as usize, fp) };
    }
    unsafe { crate::object::Py_DecRef(text) };
    0
}

#[no_mangle]
pub unsafe extern "C" fn PyMethod_New(func: *mut PyObject, self_: *mut PyObject) -> *mut PyObject {
    if func.is_null() || self_.is_null() {
        crate::errors::PyErr_BadInternalCall();
        return ptr::null_mut();
    }
    unsafe {
        call_module_attr(
            b"types\0".as_ptr() as *const c_char,
            b"MethodType\0".as_ptr() as *const c_char,
            &[func, self_],
        )
    }
}

// ---------------------------------------------------------------------------
// Import / sys / eval
// ---------------------------------------------------------------------------

#[no_mangle]
pub unsafe extern "C" fn PyImport_Import(name: *mut PyObject) -> *mut PyObject {
    let utf8 = unsafe { crate::strings::PyUnicode_AsUTF8(name) };
    if utf8.is_null() {
        return ptr::null_mut();
    }
    unsafe { crate::module::PyImport_ImportModule(utf8) }
}

#[no_mangle]
pub unsafe extern "C" fn PySys_GetObject(name: *const c_char) -> *mut PyObject {
    // The only borrowed-attr name numpy fetches at init is "flags"; pin
    // each distinct name the first time it is requested (see
    // `pinned_borrowed`). Names other than the common ones still work —
    // they just intern their own pinned slot.
    let nm = if name.is_null() {
        return ptr::null_mut();
    } else {
        unsafe { core::ffi::CStr::from_ptr(name) }
            .to_str()
            .unwrap_or("")
    };
    let name_owned = nm.to_string();
    let fetch = move || {
        let sys =
            unsafe { crate::module::PyImport_ImportModule(b"sys\0".as_ptr() as *const c_char) };
        if sys.is_null() {
            crate::errors::clear_thread_local();
            return ptr::null_mut();
        }
        let cname = match std::ffi::CString::new(name_owned) {
            Ok(c) => c,
            Err(_) => {
                unsafe { crate::object::Py_DecRef(sys) };
                return ptr::null_mut();
            }
        };
        let v = unsafe { crate::abstract_::PyObject_GetAttrString(sys, cname.as_ptr()) };
        unsafe { crate::object::Py_DecRef(sys) };
        if v.is_null() {
            crate::errors::clear_thread_local();
            return ptr::null_mut();
        }
        v
    };
    // The std streams must be fetched *live*: test code swaps
    // `sys.stdout`/`sys.stderr` (`support.captured_output`) and expects
    // `PySys_WriteStdout` & co. to hit the replacement, so a first-fetch
    // pin would write to a stale stream. Each fetched box is still pinned
    // (leaked) to honour the borrowed-reference contract; the leak is
    // bounded by the number of C-level stream fetches.
    if matches!(nm, "stdout" | "stderr" | "stdin") {
        let p = fetch();
        if !p.is_null() {
            static STREAM_PINS: std::sync::Mutex<Vec<usize>> = std::sync::Mutex::new(Vec::new());
            STREAM_PINS.lock().unwrap().push(p as usize);
        }
        return p;
    }
    pinned_borrowed(format!("sys.{nm}"), move || fetch())
}

#[no_mangle]
pub extern "C" fn PyEval_GetBuiltins() -> *mut PyObject {
    // Borrowed return — pin once (see `pinned_borrowed`).
    pinned_borrowed("eval.builtins".to_string(), || {
        let builtins = unsafe {
            crate::module::PyImport_ImportModule(b"builtins\0".as_ptr() as *const c_char)
        };
        if builtins.is_null() {
            crate::errors::clear_thread_local();
            return ptr::null_mut();
        }
        let d = unsafe {
            crate::abstract_::PyObject_GetAttrString(
                builtins,
                b"__dict__\0".as_ptr() as *const c_char,
            )
        };
        unsafe { crate::object::Py_DecRef(builtins) };
        if d.is_null() {
            crate::errors::clear_thread_local();
            return ptr::null_mut();
        }
        d
    })
}

// ---------------------------------------------------------------------------
// RFC 0060 — PEP 667 frame C-API (test_frame.TestFrameCApi). The
// "current frame" skips WeavePy's frozen ctypes machinery frames so a
// `ctypes.pythonapi` call reports the *user* caller, like CPython's
// C-implemented ctypes does naturally.
// ---------------------------------------------------------------------------

/// `PyEval_GetFrameLocals()` — an independent snapshot of the current
/// frame's locals (function scopes: fresh dict; module scope: the
/// namespace itself). New reference.
#[no_mangle]
pub extern "C" fn PyEval_GetFrameLocals() -> *mut PyObject {
    crate::interp::with_interp_mut(|vm| match vm.capi_current_py_frame() {
        Some(fr) => crate::object::into_owned(fr.locals_snapshot()),
        None => ptr::null_mut(),
    })
    .unwrap_or(ptr::null_mut())
}

/// `PyEval_GetFrameGlobals()` — the current frame's globals dict.
/// New reference.
#[no_mangle]
pub extern "C" fn PyEval_GetFrameGlobals() -> *mut PyObject {
    crate::interp::with_interp_mut(|vm| match vm.capi_current_py_frame() {
        Some(fr) => crate::object::into_owned(weavepy_vm::object::Object::Dict(fr.globals.clone())),
        None => ptr::null_mut(),
    })
    .unwrap_or(ptr::null_mut())
}

/// `PyEval_GetFrameBuiltins()` — the current frame's builtins dict.
/// New reference.
#[no_mangle]
pub extern "C" fn PyEval_GetFrameBuiltins() -> *mut PyObject {
    crate::interp::with_interp_mut(|vm| match vm.capi_current_py_frame() {
        Some(fr) => {
            crate::object::into_owned(weavepy_vm::object::Object::Dict(fr.builtins.clone()))
        }
        None => ptr::null_mut(),
    })
    .unwrap_or(ptr::null_mut())
}

/// `PyFrame_GetLocals(frame)` — PEP 667: the write-through
/// `FrameLocalsProxy` for function frames, the namespace mapping for
/// module/class frames. New reference.
#[no_mangle]
pub unsafe extern "C" fn PyFrame_GetLocals(frame: *mut PyObject) -> *mut PyObject {
    if frame.is_null() {
        return ptr::null_mut();
    }
    let obj = unsafe { crate::object::clone_object(frame) };
    let weavepy_vm::object::Object::Frame(fr) = obj else {
        return ptr::null_mut();
    };
    crate::interp::with_interp_mut(|vm| match vm.frame_locals_view(fr.clone()) {
        Ok(v) => crate::object::into_owned(v),
        Err(_) => ptr::null_mut(),
    })
    .unwrap_or(ptr::null_mut())
}

/// Opaque non-NULL handle, **identical** to [`crate::abi313::
/// PyInterpreterState_Get`]'s. WeavePy hosts a single interpreter from
/// C's point of view, and extensions compare the two by address to
/// detect sub-interpreters: numpy warns ("NumPy was imported from a
/// Python sub-interpreter"), and pybind11's `ensure_internals` flips its
/// `has_seen_non_main_interpreter` flag — sending every later
/// `get_internals()` down a per-interpreter TLS path that dereferences
/// `PyThreadState.interp` and failed with "get_internals: get_pp()
/// returned nullptr" (scipy's `_highspy._core` init). RFC 0066 WS3.
#[no_mangle]
pub extern "C" fn PyInterpreterState_Main() -> *mut c_void {
    unsafe { crate::abi313::PyInterpreterState_Get() }
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[no_mangle]
pub unsafe extern "C" fn _PyErr_BadInternalCall(_filename: *const c_char, _lineno: c_int) {
    unsafe { crate::errors::PyErr_BadInternalCall() };
}

#[no_mangle]
pub unsafe extern "C" fn PyErr_SetFromErrno(ty: *mut PyObject) -> *mut PyObject {
    let err = std::io::Error::last_os_error();
    let msg = std::ffi::CString::new(err.to_string())
        .unwrap_or_else(|_| std::ffi::CString::new("OS error").unwrap());
    unsafe { crate::errors::PyErr_SetString(ty, msg.as_ptr()) };
    ptr::null_mut()
}

// ---------------------------------------------------------------------------
// ContextVar (routed through the `contextvars` module)
// ---------------------------------------------------------------------------

#[no_mangle]
pub unsafe extern "C" fn PyContextVar_New(
    name: *const c_char,
    def: *mut PyObject,
) -> *mut PyObject {
    let name_obj = unsafe { crate::strings::PyUnicode_FromString(name) };
    if name_obj.is_null() {
        return ptr::null_mut();
    }
    let module =
        unsafe { crate::module::PyImport_ImportModule(b"contextvars\0".as_ptr() as *const c_char) };
    if module.is_null() {
        unsafe { crate::object::Py_DecRef(name_obj) };
        return ptr::null_mut();
    }
    let cls = unsafe {
        crate::abstract_::PyObject_GetAttrString(module, b"ContextVar\0".as_ptr() as *const c_char)
    };
    unsafe { crate::object::Py_DecRef(module) };
    if cls.is_null() {
        unsafe { crate::object::Py_DecRef(name_obj) };
        return ptr::null_mut();
    }
    let args = unsafe { pack_tuple(&[name_obj]) };
    unsafe { crate::object::Py_DecRef(name_obj) };
    let kwargs = if def.is_null() {
        ptr::null_mut()
    } else {
        let kw = unsafe { crate::containers::PyDict_New() };
        unsafe {
            crate::containers::PyDict_SetItemString(kw, b"default\0".as_ptr() as *const c_char, def)
        };
        kw
    };
    let var = unsafe { crate::abstract_::PyObject_Call(cls, args, kwargs) };
    unsafe {
        crate::object::Py_DecRef(cls);
        crate::object::Py_DecRef(args);
        if !kwargs.is_null() {
            crate::object::Py_DecRef(kwargs);
        }
    }
    var
}

#[no_mangle]
pub unsafe extern "C" fn PyContextVar_Get(
    var: *mut PyObject,
    default_value: *mut PyObject,
    value: *mut *mut PyObject,
) -> c_int {
    let getter = unsafe {
        crate::abstract_::PyObject_GetAttrString(var, b"get\0".as_ptr() as *const c_char)
    };
    if getter.is_null() {
        return -1;
    }
    let args = if default_value.is_null() {
        unsafe { crate::containers::PyTuple_New(0) }
    } else {
        unsafe { pack_tuple(&[default_value]) }
    };
    let res = unsafe { crate::abstract_::PyObject_CallObject(getter, args) };
    unsafe {
        crate::object::Py_DecRef(getter);
        crate::object::Py_DecRef(args);
    }
    if res.is_null() {
        // No value and no default: report "unset" without raising.
        crate::errors::clear_thread_local();
        if !value.is_null() {
            unsafe { *value = ptr::null_mut() };
        }
        return 0;
    }
    if !value.is_null() {
        unsafe { *value = res };
    } else {
        unsafe { crate::object::Py_DecRef(res) };
    }
    0
}

#[no_mangle]
pub unsafe extern "C" fn PyContextVar_Set(
    var: *mut PyObject,
    value: *mut PyObject,
) -> *mut PyObject {
    let setter = unsafe {
        crate::abstract_::PyObject_GetAttrString(var, b"set\0".as_ptr() as *const c_char)
    };
    if setter.is_null() {
        return ptr::null_mut();
    }
    let args = unsafe { pack_tuple(&[value]) };
    let token = unsafe { crate::abstract_::PyObject_CallObject(setter, args) };
    unsafe {
        crate::object::Py_DecRef(setter);
        crate::object::Py_DecRef(args);
    }
    token
}

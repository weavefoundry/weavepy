//! `PyLong_*`, `PyFloat_*`, `PyBool_*`, `PyComplex_*`.

use std::ffi::CStr;
use std::os::raw::{c_char, c_int};
use std::ptr;
use weavepy_vm::sync::Rc;

use num_bigint::BigInt;
use num_traits::ToPrimitive;
use weavepy_vm::object::{Object, PyComplex};

use crate::object::PyObject;

/// Read a `*const c_char` `tp_name` for diagnostics — best-effort, returns
/// `"?"` for a NULL chain.
unsafe fn debug_type_name(o: *mut PyObject) -> String {
    if o.is_null() {
        return "<null>".to_owned();
    }
    let ty = unsafe { (*o).ob_type };
    if ty.is_null() {
        return "<null-type>".to_owned();
    }
    let name = unsafe { (*ty).tp_name };
    if name.is_null() {
        return "<null-name>".to_owned();
    }
    unsafe { CStr::from_ptr(name) }
        .to_string_lossy()
        .into_owned()
}

/// RFC 0046 (wave 4): invoke a no-arg numeric dunder (`__float__`,
/// `__index__`, `__complex__`) on `o` and coerce the result to an
/// `Object`. Returns `None` if `o` has no such attribute (the caller then
/// tries the next protocol or raises); `Some(None)` if the call or
/// conversion failed with an exception already set.
///
/// CPython's `PyFloat_AsDouble` / `PyComplex_AsCComplex` consult the
/// number-protocol slots (`nb_float`, `nb_index`); a stock extension
/// exposes those as the matching dunder, so a `PyObject_GetAttrString`
/// reaches them through the type's `tp_getattro` (numpy scalars included).
pub(crate) unsafe fn call_number_dunder(o: *mut PyObject, name: &str) -> Option<Option<Object>> {
    let cname = match std::ffi::CString::new(name) {
        Ok(c) => c,
        Err(_) => return None,
    };
    let meth = unsafe { crate::abstract_::PyObject_GetAttrString(o, cname.as_ptr()) };
    if meth.is_null() {
        // No such attribute — clear the AttributeError and let the caller
        // fall through to the next protocol.
        let _ = crate::errors::take_pending();
        return None;
    }
    let res = unsafe { crate::abstract_::PyObject_CallNoArgs(meth) };
    unsafe { crate::object::Py_DecRef(meth) };
    if res.is_null() {
        return Some(None);
    }
    let obj = unsafe { crate::object::clone_object(res) };
    unsafe { crate::object::Py_DecRef(res) };
    Some(Some(obj))
}

/// CPython's `PyLong_As*` extractors coerce a non-`int` argument through
/// `__index__` (`_PyNumber_Index`) before failing with a `TypeError`
/// (see `Objects/longobject.c`). A numpy integer scalar (`np.int64`) is a
/// *foreign* object carrying `__index__` in its C `nb_index` slot, so
/// routing through [`crate::abstract_::PyNumber_Index`] reaches it exactly
/// as CPython does — unblocking numpy's `timedelta64(np.int64(...), unit)`
/// constructor and any Cython code feeding numpy scalars to `PyLong_As*`.
///
/// On success returns a *builtin* integer `Object` (`Int`/`Long`/`Bool`),
/// which callers convert without re-entering this fallback. Returns `None`
/// with a `TypeError` (or the `nb_index` slot's own exception) left pending
/// when `o` cannot be interpreted as an integer.
pub(crate) unsafe fn index_to_builtin_int(o: *mut PyObject) -> Option<Object> {
    let idx = unsafe { crate::abstract_::PyNumber_Index(o) };
    if idx.is_null() {
        return None;
    }
    let obj = unsafe { crate::object::clone_object(idx) };
    unsafe { crate::object::Py_DecRef(idx) };
    match obj {
        Object::Int(_) | Object::Long(_) | Object::Bool(_) => Some(obj),
        _ => {
            crate::errors::set_type_error(
                "__index__ returned non-int (the object cannot be interpreted as an integer)",
            );
            None
        }
    }
}

// ---------- PyLong (Python `int`) ----------

#[no_mangle]
pub unsafe extern "C" fn PyLong_FromLong(v: i64) -> *mut PyObject {
    crate::object::into_owned(Object::Int(v))
}

#[no_mangle]
pub unsafe extern "C" fn PyLong_FromUnsignedLong(v: u64) -> *mut PyObject {
    if v <= i64::MAX as u64 {
        crate::object::into_owned(Object::Int(v as i64))
    } else {
        crate::object::into_owned(Object::Long(Rc::new(BigInt::from(v))))
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyLong_FromLongLong(v: i64) -> *mut PyObject {
    crate::object::into_owned(Object::Int(v))
}

#[no_mangle]
pub unsafe extern "C" fn PyLong_FromUnsignedLongLong(v: u64) -> *mut PyObject {
    unsafe { PyLong_FromUnsignedLong(v) }
}

#[no_mangle]
pub unsafe extern "C" fn PyLong_FromSsize_t(v: isize) -> *mut PyObject {
    crate::object::into_owned(Object::Int(v as i64))
}

#[no_mangle]
pub unsafe extern "C" fn PyLong_FromSize_t(v: usize) -> *mut PyObject {
    if v <= i64::MAX as usize {
        crate::object::into_owned(Object::Int(v as i64))
    } else {
        crate::object::into_owned(Object::Long(Rc::new(BigInt::from(v as u64))))
    }
}

/// Convert a `f64` to a Python `int` object, faithful to CPython's
/// `PyLong_FromDouble` / `float.__int__`:
///   * NaN raises `ValueError("cannot convert float NaN to integer")`;
///   * ±infinity raises `OverflowError("cannot convert float infinity to
///     integer")`;
///   * a finite value truncates toward zero, promoting beyond the `i64`
///     range to an arbitrary-precision `Object::Long` (Python ints are
///     unbounded, so `int(1e30)` must not saturate).
///
/// A bare `v.trunc() as i64` (the previous behaviour) silently mapped NaN→0,
/// +inf→`i64::MAX`, -inf→`i64::MIN` and saturated large magnitudes — so
/// numpy's object→int cast (`arr.astype(int64)`, which reaches a `float`
/// through this `nb_int`/`PyLong_FromDouble` path) turned `[1, 2, nan]` into
/// `[1, 2, 0]` instead of raising, and pandas' `Series([1, 2, nan],
/// dtype=int)` reported the wrong error.
pub(crate) fn float_to_int_object(v: f64) -> *mut PyObject {
    if v.is_nan() {
        crate::errors::set_value_error("cannot convert float NaN to integer");
        return ptr::null_mut();
    }
    if v.is_infinite() {
        crate::errors::set_overflow_error("cannot convert float infinity to integer");
        return ptr::null_mut();
    }
    let t = v.trunc();
    // `i64::MAX as f64` rounds up to 2^63, so gate the fast path on the exact
    // power of two to avoid an out-of-range `as i64` cast.
    const TWO63: f64 = 9_223_372_036_854_775_808.0; // 2^63
    if (-TWO63..TWO63).contains(&t) {
        crate::object::into_owned(Object::Int(t as i64))
    } else {
        use num_traits::FromPrimitive;
        match BigInt::from_f64(t) {
            Some(b) => crate::object::into_owned(Object::Long(Rc::new(b))),
            None => {
                // Only NaN/inf make `from_f64` fail, and both are handled
                // above; treat any residual as overflow rather than panic.
                crate::errors::set_overflow_error("cannot convert float infinity to integer");
                ptr::null_mut()
            }
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyLong_FromDouble(v: f64) -> *mut PyObject {
    float_to_int_object(v)
}

/// True if ASCII byte `c` is a valid digit in `radix` (2..=36).
fn digit_in_radix(c: u8, radix: u32) -> bool {
    let v = match c {
        b'0'..=b'9' => u32::from(c - b'0'),
        b'a'..=b'z' => u32::from(c - b'a') + 10,
        b'A'..=b'Z' => u32::from(c - b'A') + 10,
        _ => return false,
    };
    v < radix
}

/// `PyLong_FromString(str, pend, base)` — parse an integer from a C string,
/// faithful to CPython semantics:
///   * leading/trailing ASCII whitespace is skipped;
///   * an optional leading `+`/`-` sign;
///   * `base == 0` auto-detects the base from a `0x`/`0o`/`0b` prefix
///     (else decimal); a bare leading `0` (Python 3) requires the value to
///     be all zeros;
///   * for base 2/8/16 the matching `0b`/`0o`/`0x` prefix is optional and
///     stripped;
///   * single underscores are permitted between digits (PEP 515);
///   * `*pend` (when non-NULL) is set to the first unconsumed character;
///     when `pend` is NULL any trailing non-whitespace is an error.
///
/// The previous implementation mapped `base == 0` to radix 10 and never
/// stripped a `0x`/`0o`/`0b` prefix, so a stock extension doing
/// `PyLong_FromString("0x1a2b", NULL, 0)` — pandas' `tslibs.offsets` init
/// parses a pointer address exactly this way — failed with
/// "invalid literal for int() with base 10".
#[no_mangle]
pub unsafe extern "C" fn PyLong_FromString(
    s: *const c_char,
    end: *mut *mut c_char,
    base: c_int,
) -> *mut PyObject {
    if s.is_null() {
        crate::errors::set_value_error("PyLong_FromString: NULL pointer");
        return ptr::null_mut();
    }
    if base != 0 && !(2..=36).contains(&base) {
        crate::errors::set_value_error("int() base must be >= 2 and <= 36, or 0");
        return ptr::null_mut();
    }
    let s_bytes = unsafe { CStr::from_ptr(s) }.to_bytes();
    let s_str = std::str::from_utf8(s_bytes).unwrap_or("");
    let bytes = s_str.as_bytes();
    let n = bytes.len();
    let orig = s_str;

    let mut i = 0usize;
    while i < n && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    let mut negative = false;
    if i < n && (bytes[i] == b'+' || bytes[i] == b'-') {
        negative = bytes[i] == b'-';
        i += 1;
    }

    let has_prefix = |i: usize, lo: u8, hi: u8| -> bool {
        i + 1 < n && bytes[i] == b'0' && (bytes[i + 1] == lo || bytes[i + 1] == hi)
    };

    let mut radix = base as u32;
    let mut only_zeros_required = false;
    if base == 0 {
        if has_prefix(i, b'x', b'X') {
            radix = 16;
            i += 2;
        } else if has_prefix(i, b'o', b'O') {
            radix = 8;
            i += 2;
        } else if has_prefix(i, b'b', b'B') {
            radix = 2;
            i += 2;
        } else {
            radix = 10;
            // Python 3: a bare leading zero (e.g. "0123") is invalid for
            // base 0; only "0", "00", "0_0" … (all zeros) are accepted.
            if i < n && bytes[i] == b'0' {
                only_zeros_required = true;
            }
        }
    } else if base == 16 && has_prefix(i, b'x', b'X') {
        i += 2;
    } else if base == 8 && has_prefix(i, b'o', b'O') {
        i += 2;
    } else if base == 2 && has_prefix(i, b'b', b'B') {
        i += 2;
    }

    // Collect digits, allowing a single underscore between two digits.
    let mut digits: Vec<u8> = Vec::with_capacity(n - i);
    let mut prev_was_digit = false;
    let mut consumed = i;
    while i < n {
        let c = bytes[i];
        if c == b'_' {
            // A separator is only valid immediately after a digit and
            // immediately before another digit.
            if !prev_was_digit || i + 1 >= n || !digit_in_radix(bytes[i + 1], radix) {
                break;
            }
            prev_was_digit = false;
            i += 1;
            continue;
        }
        if digit_in_radix(c, radix) {
            digits.push(c);
            prev_was_digit = true;
            i += 1;
            consumed = i;
        } else {
            break;
        }
    }

    let fail = || -> *mut PyObject {
        crate::errors::set_value_error(format!(
            "invalid literal for int() with base {}: '{}'",
            if base == 0 { 10 } else { base },
            orig.trim(),
        ));
        ptr::null_mut()
    };

    if digits.is_empty() {
        return fail();
    }
    if only_zeros_required && digits.iter().any(|&d| d != b'0') {
        return fail();
    }

    // Trailing whitespace is allowed; anything else after it is garbage.
    let mut j = consumed;
    while j < n && bytes[j].is_ascii_whitespace() {
        j += 1;
    }
    if end.is_null() {
        if j != n {
            return fail();
        }
    } else {
        unsafe {
            *end = s.add(consumed).cast_mut();
        }
    }

    match BigInt::parse_bytes(&digits, radix) {
        Some(mut big) => {
            if negative {
                big = -big;
            }
            if let Some(small) = big.to_i64() {
                crate::object::into_owned(Object::Int(small))
            } else {
                crate::object::into_owned(Object::Long(Rc::new(big)))
            }
        }
        None => fail(),
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyLong_AsLong(o: *mut PyObject) -> i64 {
    if o.is_null() {
        crate::errors::set_type_error("PyLong_AsLong: NULL");
        return -1;
    }
    match unsafe { crate::object::clone_object(o) } {
        Object::Int(i) => i,
        Object::Bool(b) => i64::from(b),
        Object::Long(big) => match big.to_i64() {
            Some(v) => v,
            None => {
                if std::env::var_os("WEAVEPY_TRACE_OVERFLOW").is_some() {
                    eprintln!(
                        "[WEAVEPY_TRACE_OVERFLOW] PyLong_AsLong overflow on value with {} bits\n{}",
                        big.bits(),
                        std::backtrace::Backtrace::force_capture()
                    );
                }
                crate::errors::set_overflow_error("Python int too large to convert to C long");
                -1
            }
        },
        Object::Float(f) => f.trunc() as i64,
        _ => match unsafe { index_to_builtin_int(o) } {
            Some(Object::Int(i)) => i,
            Some(Object::Bool(b)) => i64::from(b),
            Some(Object::Long(big)) => big.to_i64().unwrap_or_else(|| {
                crate::errors::set_overflow_error("Python int too large to convert to C long");
                -1
            }),
            _ => -1,
        },
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyLong_AsLongLong(o: *mut PyObject) -> i64 {
    unsafe { PyLong_AsLong(o) }
}

/// `PyLong_AsUnsignedLong(o)` — the full unsigned 64-bit range `[0, 2^64)`
/// on LP64/LLP64 (where `unsigned long` is 64-bit). Routing through the
/// *signed* [`PyLong_AsLong`] (as a prior version did) wrongly rejected
/// `[2^63, 2^64)` — exactly the 64-bit seed/state words numpy's
/// `numpy.random` feeds through `np.uint64(...)` during `mtrand` init.
#[no_mangle]
pub unsafe extern "C" fn PyLong_AsUnsignedLong(o: *mut PyObject) -> u64 {
    if o.is_null() {
        crate::errors::set_type_error("PyLong_AsUnsignedLong: NULL");
        return u64::MAX;
    }
    match unsafe { crate::object::clone_object(o) } {
        Object::Int(i) => {
            if i < 0 {
                crate::errors::set_overflow_error("can't convert negative value to unsigned int");
                u64::MAX
            } else {
                i as u64
            }
        }
        Object::Bool(b) => u64::from(b),
        Object::Long(big) => match big.to_u64() {
            Some(v) => v,
            None => {
                if big.sign() == num_bigint::Sign::Minus {
                    crate::errors::set_overflow_error(
                        "can't convert negative value to unsigned int",
                    );
                } else {
                    crate::errors::set_overflow_error(
                        "Python int too large to convert to C unsigned long",
                    );
                }
                u64::MAX
            }
        },
        _ => match unsafe { index_to_builtin_int(o) } {
            Some(Object::Int(i)) => {
                if i < 0 {
                    crate::errors::set_overflow_error(
                        "can't convert negative value to unsigned int",
                    );
                    u64::MAX
                } else {
                    i as u64
                }
            }
            Some(Object::Bool(b)) => u64::from(b),
            Some(Object::Long(big)) => big.to_u64().unwrap_or_else(|| {
                if big.sign() == num_bigint::Sign::Minus {
                    crate::errors::set_overflow_error(
                        "can't convert negative value to unsigned int",
                    );
                } else {
                    crate::errors::set_overflow_error(
                        "Python int too large to convert to C unsigned long",
                    );
                }
                u64::MAX
            }),
            _ => u64::MAX,
        },
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyLong_AsUnsignedLongLong(o: *mut PyObject) -> u64 {
    unsafe { PyLong_AsUnsignedLong(o) }
}

/// `PyLong_AsUnsignedLongMask` — like `AsUnsignedLong` but a negative value
/// wraps modulo 2^64 instead of raising OverflowError (CPython's "Mask"
/// family). numpy < 2.5 links this directly (`PyUFunc_AddLoop`'s hashing
/// path); a missing export left the dyld stub NULL and importing numpy
/// 2.3.x/2.4.x segfaulted at `initumath`.
#[no_mangle]
pub unsafe extern "C" fn PyLong_AsUnsignedLongMask(o: *mut PyObject) -> u64 {
    if o.is_null() {
        crate::errors::set_type_error("PyLong_AsUnsignedLongMask: NULL");
        return u64::MAX;
    }
    match unsafe { crate::object::clone_object(o) } {
        Object::Int(i) => i as u64,
        Object::Bool(b) => u64::from(b),
        Object::Long(big) => {
            // Wrap modulo 2^64: low 64 bits of the two's-complement value.
            let m = &(num_bigint::BigInt::from(1u8) << 64u32);
            let r = ((big.as_ref() % m) + m) % m;
            r.to_u64().unwrap_or(0)
        }
        _ => {
            crate::errors::set_type_error("an integer is required");
            u64::MAX
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyLong_AsUnsignedLongLongMask(o: *mut PyObject) -> u64 {
    unsafe { PyLong_AsUnsignedLongMask(o) }
}

#[no_mangle]
pub unsafe extern "C" fn PyLong_AsSsize_t(o: *mut PyObject) -> isize {
    unsafe { PyLong_AsLong(o) as isize }
}

#[no_mangle]
pub unsafe extern "C" fn PyLong_AsDouble(o: *mut PyObject) -> f64 {
    if o.is_null() {
        crate::errors::set_type_error("PyLong_AsDouble: NULL");
        return -1.0;
    }
    match unsafe { crate::object::clone_object(o) } {
        Object::Int(i) => i as f64,
        Object::Long(big) => big.to_f64().unwrap_or(f64::INFINITY),
        Object::Bool(b) => f64::from(b as i32),
        Object::Float(f) => f,
        _ => {
            crate::errors::set_type_error("an integer is required");
            -1.0
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyLong_Check(o: *mut PyObject) -> c_int {
    if o.is_null() {
        return 0;
    }
    matches!(
        unsafe { crate::object::clone_object(o) },
        Object::Int(_) | Object::Long(_) | Object::Bool(_)
    )
    .into()
}

/// Convert an int to a C `long` with overflow detection
/// (CPython 3.0+).
///
/// Returns the long value on success; on a value that overflows
/// the C `long` range, returns `-1` and writes `1` (positive
/// overflow) or `-1` (negative overflow) through `overflow`.
/// On a type mismatch returns `-1` and sets a `TypeError`.
#[no_mangle]
pub unsafe extern "C" fn PyLong_AsLongAndOverflow(o: *mut PyObject, overflow: *mut c_int) -> i64 {
    if !overflow.is_null() {
        unsafe { *overflow = 0 };
    }
    if o.is_null() {
        crate::errors::set_type_error("PyLong_AsLongAndOverflow: NULL");
        return -1;
    }
    match unsafe { crate::object::clone_object(o) } {
        Object::Int(i) => i,
        Object::Bool(b) => i64::from(b),
        Object::Long(big) => match big.to_i64() {
            Some(v) => v,
            None => {
                if !overflow.is_null() {
                    let sign = match big.sign() {
                        num_bigint::Sign::Minus => -1,
                        _ => 1,
                    };
                    unsafe { *overflow = sign };
                }
                -1
            }
        },
        Object::Float(f) => f.trunc() as i64,
        _ => match unsafe { index_to_builtin_int(o) } {
            Some(Object::Int(i)) => i,
            Some(Object::Bool(b)) => i64::from(b),
            Some(Object::Long(big)) => big.to_i64().unwrap_or_else(|| {
                if !overflow.is_null() {
                    let sign = match big.sign() {
                        num_bigint::Sign::Minus => -1,
                        _ => 1,
                    };
                    unsafe { *overflow = sign };
                }
                -1
            }),
            _ => -1,
        },
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyLong_AsLongLongAndOverflow(
    o: *mut PyObject,
    overflow: *mut c_int,
) -> i64 {
    unsafe { PyLong_AsLongAndOverflow(o, overflow) }
}

/// `PyLong_AsByteArray` — write the int's two's-complement
/// representation into a byte buffer.
#[no_mangle]
pub unsafe extern "C" fn _PyLong_AsByteArray(
    o: *mut PyObject,
    bytes: *mut u8,
    n: usize,
    little_endian: c_int,
    is_signed: c_int,
) -> c_int {
    if o.is_null() || bytes.is_null() {
        crate::errors::set_type_error("_PyLong_AsByteArray: NULL");
        return -1;
    }
    let big = match unsafe { crate::object::clone_object(o) } {
        Object::Int(i) => BigInt::from(i),
        Object::Long(b) => (*b).clone(),
        Object::Bool(b) => BigInt::from(b as i64),
        _ => match unsafe { index_to_builtin_int(o) } {
            Some(Object::Int(i)) => BigInt::from(i),
            Some(Object::Long(b)) => (*b).clone(),
            Some(Object::Bool(b)) => BigInt::from(b as i64),
            _ => return -1,
        },
    };
    let mut buf: Vec<u8> = if is_signed != 0 {
        big.to_signed_bytes_le()
    } else {
        big.to_bytes_le().1
    };
    // Sign-extend or zero-extend to fit `n` bytes.
    let target = n;
    if buf.len() > target {
        crate::errors::set_overflow_error("int too big to convert");
        return -1;
    }
    let pad_byte = if is_signed != 0 && buf.last().copied().unwrap_or(0) & 0x80 != 0 {
        0xff
    } else {
        0x00
    };
    while buf.len() < target {
        buf.push(pad_byte);
    }
    if little_endian == 0 {
        buf.reverse();
    }
    unsafe { std::ptr::copy_nonoverlapping(buf.as_ptr(), bytes, target) };
    0
}

/// `PyLong_FromByteArray` — build a long from a byte buffer.
#[no_mangle]
pub unsafe extern "C" fn _PyLong_FromByteArray(
    bytes: *const u8,
    n: usize,
    little_endian: c_int,
    is_signed: c_int,
) -> *mut PyObject {
    if bytes.is_null() {
        crate::errors::set_type_error("_PyLong_FromByteArray: NULL");
        return ptr::null_mut();
    }
    let mut slice = unsafe { std::slice::from_raw_parts(bytes, n) }.to_vec();
    if little_endian == 0 {
        slice.reverse();
    }
    let big = if is_signed != 0 {
        BigInt::from_signed_bytes_le(&slice)
    } else {
        BigInt::from_bytes_le(num_bigint::Sign::Plus, &slice)
    };
    match big.to_i64() {
        Some(small) => crate::object::into_owned(Object::Int(small)),
        None => crate::object::into_owned(Object::Long(Rc::new(big))),
    }
}

/// Convert an `int` to a `void *`. CPython treats this as a
/// signed roundtrip through `Py_ssize_t`; we mirror that.
#[no_mangle]
pub unsafe extern "C" fn PyLong_AsVoidPtr(o: *mut PyObject) -> *mut std::ffi::c_void {
    let v = unsafe { PyLong_AsLongLong(o) };
    v as usize as *mut std::ffi::c_void
}

/// Build a new `int` whose value is the integer representation
/// of the pointer.
#[no_mangle]
pub unsafe extern "C" fn PyLong_FromVoidPtr(p: *const std::ffi::c_void) -> *mut PyObject {
    crate::object::into_owned(Object::Int(p as usize as i64))
}

/// `PyLong_GetInfo` — opaque "structseq" describing the int
/// implementation. CPython returns a struct with `bits_per_digit`
/// and `sizeof_digit`; we approximate with a 2-element tuple
/// since user code generally just reads attributes off it.
#[no_mangle]
pub unsafe extern "C" fn PyLong_GetInfo() -> *mut PyObject {
    crate::object::into_owned(Object::new_tuple(vec![Object::Int(30), Object::Int(4)]))
}

// ---------- PyFloat ----------

#[no_mangle]
pub unsafe extern "C" fn PyFloat_FromDouble(v: f64) -> *mut PyObject {
    // CPython allocates a fresh object per call; a *canonical* NaN gets a
    // fresh identity tag so two separately materialised NaNs (numpy
    // `tolist()`, `.item()`) stay `is`-distinct exactly as distinct
    // allocations would. An *exotic* payload is preserved verbatim —
    // re-tagging it would destroy genuine bits an extension put there
    // (pandas' `test_first_nan_kept` round-trips 0xfff8…0001 through
    // `np.float64` and asserts the payload survives `pd.unique`).
    crate::object::into_owned(Object::Float(weavepy_vm::object::tag_unpacked_nan(v)))
}

/// Outcome of running CPython's float number-protocol on an object.
pub(crate) enum FloatProtocol {
    /// Converted to this double via `nb_float` / `__float__` (or the
    /// `nb_index` / `__index__` fallback).
    Value(f64),
    /// A protocol slot ran and *raised*; the pending error is already set and
    /// must be propagated verbatim (CPython never rewrites a slot's error).
    Raised,
    /// The object implements none of `nb_float`/`__float__`/`nb_index`/
    /// `__index__`; *no* error is set, so the caller emits its own
    /// entry-point-specific `TypeError` (the wording differs between
    /// `PyFloat_AsDouble` and `PyNumber_Float`, matching CPython).
    NoProtocol,
}

/// Run CPython's `nb_float` (→ `__float__`) then `nb_index` (→ `__index__`)
/// fallback on `o` (already cloned to `obj`). Shared by [`PyFloat_AsDouble`]
/// and [`crate::abstract_::PyNumber_Float`]; those two differ *only* in the
/// final no-protocol message and, for `PyNumber_Float`, an extra
/// `str`/`bytes` → `PyFloat_FromString` branch — exactly as in CPython.
pub(crate) unsafe fn float_number_protocol(o: *mut PyObject, obj: &Object) -> FloatProtocol {
    // A *foreign* extension scalar (numpy `float64`/`float32`) carries
    // `__float__` in its C `nb_float` slot, invisible to the getattro-based
    // `__float__` lookup below — numpy's `tp_getattro` walks only its own dict
    // and misses the dunder inherited from the mirror base (the same blind
    // spot `complex128.__complex__` hit). CPython reads `nb_float` off the
    // type directly, so try that first.
    if matches!(obj, Object::Foreign(_)) {
        let r = unsafe { crate::abstract_::foreign_nb_float(o) };
        if !r.is_null() {
            let v = unsafe { crate::object::clone_object(r) };
            unsafe { crate::object::Py_DecRef(r) };
            match v {
                Object::Float(f) => return FloatProtocol::Value(f),
                Object::Int(i) => return FloatProtocol::Value(i as f64),
                Object::Long(big) => {
                    return FloatProtocol::Value(big.to_f64().unwrap_or(f64::INFINITY))
                }
                Object::Bool(b) => return FloatProtocol::Value(f64::from(b as i32)),
                // A misbehaving `nb_float` returned a non-float; fall through
                // to the `__float__`/`__index__` protocol.
                _ => {}
            }
        } else if crate::errors::pending().is_some() {
            return FloatProtocol::Raised;
        }
    }
    // RFC 0046 (wave 4): consult `__float__` then `__index__` (CPython's
    // `nb_float` / `nb_index` fallback) so a numpy scalar or user instance
    // converts faithfully.
    for attr in ["__float__", "__index__"] {
        if let Some(result) = unsafe { call_number_dunder(o, attr) } {
            return match result {
                Some(Object::Float(f)) => FloatProtocol::Value(f),
                Some(Object::Int(i)) => FloatProtocol::Value(i as f64),
                Some(Object::Long(big)) => {
                    FloatProtocol::Value(big.to_f64().unwrap_or(f64::INFINITY))
                }
                Some(Object::Bool(b)) => FloatProtocol::Value(f64::from(b as i32)),
                Some(_) => {
                    crate::errors::set_type_error("__float__ returned non-float");
                    FloatProtocol::Raised
                }
                None => FloatProtocol::Raised,
            };
        }
    }
    FloatProtocol::NoProtocol
}

#[no_mangle]
pub unsafe extern "C" fn PyFloat_AsDouble(o: *mut PyObject) -> f64 {
    if o.is_null() {
        crate::errors::set_type_error("PyFloat_AsDouble: NULL");
        return -1.0;
    }
    match unsafe { crate::object::clone_object(o) } {
        // Strip any WeavePy NaN identity tag: C consumers (numpy array
        // stores, struct writers) must observe the canonical quiet-NaN bits
        // CPython's `ob_fval` would hold.
        Object::Float(f) => weavepy_vm::object::untag_nan(f),
        Object::Int(i) => i as f64,
        Object::Long(big) => big.to_f64().unwrap_or(f64::INFINITY),
        Object::Bool(b) => f64::from(b as i32),
        other => match unsafe { float_number_protocol(o, &other) } {
            FloatProtocol::Value(v) => weavepy_vm::object::untag_nan(v),
            FloatProtocol::Raised => -1.0,
            FloatProtocol::NoProtocol => {
                if std::env::var_os("WEAVEPY_TRACE_CONV").is_some() {
                    let owned = crate::object::is_weavepy_owned(o);
                    let variant = format!("{other:?}");
                    let short: String = variant.chars().take(80).collect();
                    eprintln!(
                        "[conv] PyFloat_AsDouble: no float protocol on {} ptr={o:p} weavepy_owned={owned} clone={short}",
                        unsafe { debug_type_name(o) },
                    );
                }
                // CPython's `PyFloat_AsDouble`: `must be real number, not X`
                // (`Py_TYPE(op)->tp_name`). This is a *different* message from
                // the `float()` builtin (`PyNumber_Float`), which pandas'
                // groupby-`corr` on an object column relies on matching.
                crate::errors::set_type_error(format!("must be real number, not {}", unsafe {
                    debug_type_name(o)
                }));
                -1.0
            }
        },
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyFloat_Check(o: *mut PyObject) -> c_int {
    if o.is_null() {
        return 0;
    }
    matches!(unsafe { crate::object::clone_object(o) }, Object::Float(_)).into()
}

#[no_mangle]
pub unsafe extern "C" fn PyFloat_GetMax() -> f64 {
    f64::MAX
}

#[no_mangle]
pub unsafe extern "C" fn PyFloat_GetMin() -> f64 {
    f64::MIN_POSITIVE
}

/// `PyFloat_GetInfo()` — returns a structseq-shaped info bundle.
/// User code expects attribute access (`.max`, `.min`, `.epsilon`,
/// `.dig`, …) so we publish it as a small tuple keyed by index.
#[no_mangle]
pub unsafe extern "C" fn PyFloat_GetInfo() -> *mut PyObject {
    crate::object::into_owned(Object::new_tuple(vec![
        Object::Float(f64::MAX),
        Object::Int(1024),
        Object::Int(308),
        Object::Float(f64::MIN_POSITIVE),
        Object::Int(-1021),
        Object::Int(-307),
        Object::Int(15),
        Object::Int(53),
        Object::Float(f64::EPSILON),
        Object::Int(2),
        Object::Int(1),
    ]))
}

/// `_PyFloat_Pack4` — pack a double into 4 IEEE-754 bytes.
/// `little_endian == 0` selects big-endian on the wire.
#[no_mangle]
pub unsafe extern "C" fn _PyFloat_Pack4(x: f64, p: *mut u8, little_endian: c_int) -> c_int {
    if p.is_null() {
        return -1;
    }
    let bytes = (x as f32).to_bits();
    let raw = if little_endian != 0 {
        bytes.to_le_bytes()
    } else {
        bytes.to_be_bytes()
    };
    unsafe { std::ptr::copy_nonoverlapping(raw.as_ptr(), p, 4) };
    0
}

#[no_mangle]
pub unsafe extern "C" fn _PyFloat_Pack8(x: f64, p: *mut u8, little_endian: c_int) -> c_int {
    if p.is_null() {
        return -1;
    }
    let bytes = x.to_bits();
    let raw = if little_endian != 0 {
        bytes.to_le_bytes()
    } else {
        bytes.to_be_bytes()
    };
    unsafe { std::ptr::copy_nonoverlapping(raw.as_ptr(), p, 8) };
    0
}

#[no_mangle]
pub unsafe extern "C" fn _PyFloat_Unpack4(p: *const u8, little_endian: c_int) -> f64 {
    if p.is_null() {
        return f64::NAN;
    }
    let mut buf = [0u8; 4];
    unsafe { std::ptr::copy_nonoverlapping(p, buf.as_mut_ptr(), 4) };
    let bits = if little_endian != 0 {
        u32::from_le_bytes(buf)
    } else {
        u32::from_be_bytes(buf)
    };
    f32::from_bits(bits) as f64
}

#[no_mangle]
pub unsafe extern "C" fn _PyFloat_Unpack8(p: *const u8, little_endian: c_int) -> f64 {
    if p.is_null() {
        return f64::NAN;
    }
    let mut buf = [0u8; 8];
    unsafe { std::ptr::copy_nonoverlapping(p, buf.as_mut_ptr(), 8) };
    let bits = if little_endian != 0 {
        u64::from_le_bytes(buf)
    } else {
        u64::from_be_bytes(buf)
    };
    f64::from_bits(bits)
}

// CPython 3.11 promoted the pack/unpack family to public API
// (`_PyFloat_Pack8` → `PyFloat_Pack8`); wheels built against 3.11+
// headers (msgpack's Cython packer) lazy-bind the public spelling, and
// an unresolved lazy stub jumps to NULL at first float pack. Both
// spellings stay exported, like CPython itself.

#[no_mangle]
pub unsafe extern "C" fn PyFloat_Pack4(x: f64, p: *mut u8, little_endian: c_int) -> c_int {
    unsafe { _PyFloat_Pack4(x, p, little_endian) }
}

#[no_mangle]
pub unsafe extern "C" fn PyFloat_Pack8(x: f64, p: *mut u8, little_endian: c_int) -> c_int {
    unsafe { _PyFloat_Pack8(x, p, little_endian) }
}

#[no_mangle]
pub unsafe extern "C" fn PyFloat_Unpack4(p: *const u8, little_endian: c_int) -> f64 {
    unsafe { _PyFloat_Unpack4(p, little_endian) }
}

#[no_mangle]
pub unsafe extern "C" fn PyFloat_Unpack8(p: *const u8, little_endian: c_int) -> f64 {
    unsafe { _PyFloat_Unpack8(p, little_endian) }
}

// ---------- PyBool ----------

#[no_mangle]
pub unsafe extern "C" fn PyBool_FromLong(v: i64) -> *mut PyObject {
    if v != 0 {
        unsafe { crate::object::Py_IncRef(crate::singletons::true_ptr()) };
        crate::singletons::true_ptr()
    } else {
        unsafe { crate::object::Py_IncRef(crate::singletons::false_ptr()) };
        crate::singletons::false_ptr()
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyBool_Check(o: *mut PyObject) -> c_int {
    if o.is_null() {
        return 0;
    }
    matches!(unsafe { crate::object::clone_object(o) }, Object::Bool(_)).into()
}

// ---------- PyComplex ----------

#[no_mangle]
pub unsafe extern "C" fn PyComplex_FromDoubles(real: f64, imag: f64) -> *mut PyObject {
    crate::object::into_owned(Object::Complex(Rc::new(PyComplex { real, imag })))
}

#[no_mangle]
pub unsafe extern "C" fn PyComplex_RealAsDouble(o: *mut PyObject) -> f64 {
    if o.is_null() {
        return -1.0;
    }
    // A foreign `complex` subtype (numpy `complex128`): direct `cval` read,
    // as CPython does for any `PyComplex_Check` object.
    if let Some(cv) = unsafe { foreign_complex_cval(o) } {
        return cv.real;
    }
    // C consumers observe canonical NaN bits — strip any identity tag.
    match unsafe { crate::object::clone_object(o) } {
        Object::Complex(c) => weavepy_vm::object::untag_nan(c.real),
        Object::Float(f) => weavepy_vm::object::untag_nan(f),
        Object::Int(i) => i as f64,
        Object::Long(big) => big.to_f64().unwrap_or(f64::INFINITY),
        _ => {
            // RFC 0046 (wave 4): CPython tries `__complex__` (real part),
            // then falls back to the float protocol (`__float__` /
            // `__index__`, via `PyFloat_AsDouble`).
            if let Some(result) = unsafe { call_number_dunder(o, "__complex__") } {
                return match result {
                    Some(Object::Complex(c)) => weavepy_vm::object::untag_nan(c.real),
                    Some(Object::Float(f)) => weavepy_vm::object::untag_nan(f),
                    Some(Object::Int(i)) => i as f64,
                    Some(Object::Long(big)) => big.to_f64().unwrap_or(f64::INFINITY),
                    Some(_) => {
                        crate::errors::set_type_error("__complex__ returned non-complex");
                        -1.0
                    }
                    None => -1.0,
                };
            }
            unsafe { PyFloat_AsDouble(o) }
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyComplex_ImagAsDouble(o: *mut PyObject) -> f64 {
    if o.is_null() {
        return -1.0;
    }
    if let Some(cv) = unsafe { foreign_complex_cval(o) } {
        return cv.imag;
    }
    match unsafe { crate::object::clone_object(o) } {
        Object::Complex(c) => weavepy_vm::object::untag_nan(c.imag),
        Object::Float(_) | Object::Int(_) | Object::Long(_) | Object::Bool(_) => 0.0,
        _ => {
            // RFC 0046 (wave 4): `__complex__` carries the imaginary part; a
            // real-only object (no `__complex__`) has imag 0.
            if let Some(result) = unsafe { call_number_dunder(o, "__complex__") } {
                return match result {
                    Some(Object::Complex(c)) => weavepy_vm::object::untag_nan(c.imag),
                    Some(_) => 0.0,
                    None => -1.0,
                };
            }
            0.0
        }
    }
}

/// CPython's `PyComplex_AsCComplex` as a *single* conversion pass. The
/// public C entry point ([`crate::wave4::PyComplex_AsCComplex`]) used to
/// compute the real and imaginary parts with two independent
/// `PyComplex_RealAsDouble` / `PyComplex_ImagAsDouble` calls; when the
/// object implements neither `__complex__` nor the float protocol (e.g. a
/// pandas `Interval`), the real call's float fallback set a `TypeError`,
/// but the imag call's own `__complex__` attribute probe then *replaced and
/// cleared* it (`call_number_dunder` drops the `AttributeError` from the
/// missing dunder). numpy's object→`complex128` cast therefore saw no
/// pending error and silently stored garbage (`-1+0j`) instead of raising —
/// so `IntervalIndex.astype(complex128)` never raised the `TypeError`
/// pandas re-wraps as "Cannot cast IntervalIndex to dtype". Doing the whole
/// conversion once leaves exactly one pending error on failure.
pub(crate) unsafe fn complex_as_ccomplex(op: *mut PyObject) -> crate::layout::PyComplexValue {
    use crate::layout::PyComplexValue;
    let real_only = |real: f64| PyComplexValue { real, imag: 0.0 };
    if op.is_null() {
        crate::errors::set_type_error("PyComplex_AsCComplex: NULL");
        return real_only(-1.0);
    }
    // CPython reads `((PyComplexObject*)op)->cval` for ANY `complex`
    // subtype (`PyComplex_Check`) before consulting `__complex__`. A
    // *foreign* complex subclass instance — numpy's `complex128` scalar —
    // carries exactly that C layout, and the direct read is the only path
    // that can't lose precision or fall into a float fallback (which
    // silently dropped the imaginary part in pandas'
    // `maybe_convert_objects`, GH itemsize tests).
    if let Some(cv) = unsafe { foreign_complex_cval(op) } {
        return cv;
    }
    // C consumers observe canonical NaN bits — strip any identity tag.
    match unsafe { crate::object::clone_object(op) } {
        Object::Complex(c) => PyComplexValue {
            real: weavepy_vm::object::untag_nan(c.real),
            imag: weavepy_vm::object::untag_nan(c.imag),
        },
        Object::Float(f) => real_only(weavepy_vm::object::untag_nan(f)),
        Object::Int(i) => real_only(i as f64),
        Object::Long(big) => real_only(big.to_f64().unwrap_or(f64::INFINITY)),
        Object::Bool(b) => real_only(f64::from(b as i32)),
        _ => {
            // `__complex__` carries both parts (CPython tries it first).
            if let Some(result) = unsafe { call_number_dunder(op, "__complex__") } {
                return match result {
                    Some(Object::Complex(c)) => PyComplexValue {
                        real: weavepy_vm::object::untag_nan(c.real),
                        imag: weavepy_vm::object::untag_nan(c.imag),
                    },
                    Some(Object::Float(f)) => real_only(weavepy_vm::object::untag_nan(f)),
                    Some(Object::Int(i)) => real_only(i as f64),
                    Some(Object::Long(big)) => real_only(big.to_f64().unwrap_or(f64::INFINITY)),
                    Some(_) => {
                        crate::errors::set_type_error("__complex__ should return a complex object");
                        real_only(-1.0)
                    }
                    // `__complex__` raised — its error is already pending.
                    None => real_only(-1.0),
                };
            }
            // No `__complex__`: a single float-protocol attempt (imag 0).
            // `PyFloat_AsDouble` leaves its own `TypeError` pending when the
            // object has no float protocol either, and this is the *only*
            // conversion attempt so nothing clears it.
            real_only(unsafe { PyFloat_AsDouble(op) })
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyComplex_Check(o: *mut PyObject) -> c_int {
    if o.is_null() {
        return 0;
    }
    if matches!(
        unsafe { crate::object::clone_object(o) },
        Object::Complex(_)
    ) {
        return 1;
    }
    // A `complex` *subtype* (numpy's `complex128` scalar reaches `complex`
    // through its MRO) — CPython's macro is `PyObject_TypeCheck(op,
    // &PyComplex_Type)`.
    let ty = unsafe { (*o).ob_type };
    unsafe { crate::types::PyType_IsSubtype(ty, crate::types::PyComplex_Type.as_ptr()) }
}

/// The inline `cval` of a *foreign* `complex`-subtype instance (numpy's
/// `complex128` scalar), read directly off its CPython-compatible
/// `PyComplexObject` layout — what CPython's `PyComplex_AsCComplex` does
/// for any `PyComplex_Check` object. `None` for WeavePy-owned boxes (their
/// payload is the source of truth) and non-complex-subtype foreigns.
pub(crate) unsafe fn foreign_complex_cval(
    op: *mut PyObject,
) -> Option<crate::layout::PyComplexValue> {
    if crate::object::is_weavepy_owned(op) {
        return None;
    }
    let ty = unsafe { (*op).ob_type };
    if unsafe { crate::types::PyType_IsSubtype(ty, crate::types::PyComplex_Type.as_ptr()) } == 0 {
        return None;
    }
    let co = op as *const crate::layout::PyComplexObject;
    Some(unsafe { (*co).cval })
}

//! FFI bridge for `_ctypes_native`.
//!
//! This is the genuinely-FFI half of ctypes: turning a resolved function
//! address + ctypes type codes into a real C ABI call (and the reverse,
//! for Python callbacks passed to C). It is implemented on top of a small,
//! self-contained native back-end ([`native`]) — a hand-written call gate
//! and a pool of closure trampolines — so it has no external C build
//! dependency (no `libffi`).
//!
//! The frozen `python/_ctypes.py` marshals every foreign-function call
//! down to two primitives:
//!
//! * `call_function(addr, rcode, codes, payloads, flags)` — invoke the C
//!   function at `addr`. `rcode` is the return type's ctypes format code
//!   (or `None` for `void`); `codes[i]`/`payloads[i]` are the format code
//!   and already-coerced Python value for argument `i`; `flags` carries
//!   the `FUNCFLAG_*` bits (only `USE_ERRNO` is honoured here).
//! * `create_closure(callable, rcode, argcodes)` — build a C-callable
//!   trampoline that, when invoked from C, marshals the C arguments back
//!   into Python, calls `callable`, and marshals the result out. Returns
//!   the trampoline's code address (what a `CFUNCTYPE(py_callable)` stores
//!   as its function pointer).
//!
//! The format codes are the standard `struct`/ctypes single-character
//! codes: `b B h H i I l L q Q` (ints), `f d g` (float/double/long
//! double), `c ?` (char/bool), `u` (wchar), and `P z Z O` (pointers:
//! `void*`, `char*`, `wchar_t*`, `PyObject*`). Aggregates and pointers
//! are always marshalled by address (`P`) on the Python side, so the
//! bridge only ever sees scalars and pointers — never a by-value struct.
//!
//! ## ABI placement
//!
//! [`native`] works purely in terms of a register-file image (up to 8
//! integer + 8 FP registers, plus overflow stack words). This module owns
//! the calling-convention decision of *which* slot each argument lands in
//! ([`assign_slots`]) and the scalar <-> register bit marshalling, keeping
//! the platform ABI knowledge in one place shared by both the call and the
//! callback direction.

use std::os::raw::c_void;

use crate::error::{type_error, value_error, PyException, RuntimeError};
use crate::object::Object;

mod native;

// ----------------------------------------------------------------
// Type-code classification
// ----------------------------------------------------------------

/// The ABI class a ctypes format code marshals to. `size` for `Int` is
/// the platform C width (so `l` is 8 on LP64, 4 on Windows), matching the
/// sizes `_ctypes_native::code_info` reports.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Cls {
    Int { size: usize, signed: bool },
    F32,
    F64,
    Ptr,
    Void,
}

fn wchar_size() -> usize {
    super::wchar_info().0
}

/// `long double` is platform-dependent. On AArch64/ARM it is identical to
/// `double` (8 bytes), so we can marshal it as `f64`. On x86 it is the
/// 80-bit extended type, which cannot round-trip through a Python float,
/// so we decline it (callers get a clear error).
fn classify_longdouble() -> Option<Cls> {
    #[cfg(any(target_arch = "aarch64", target_arch = "arm"))]
    {
        Some(Cls::F64)
    }
    #[cfg(not(any(target_arch = "aarch64", target_arch = "arm")))]
    {
        None
    }
}

fn classify(code: char) -> Option<Cls> {
    use std::mem::size_of;
    let cls = match code {
        'b' => Cls::Int { size: 1, signed: true },
        'B' | 'c' | '?' => Cls::Int { size: 1, signed: false },
        'h' => Cls::Int { size: size_of::<libc::c_short>(), signed: true },
        'H' => Cls::Int { size: size_of::<libc::c_short>(), signed: false },
        'i' => Cls::Int { size: size_of::<libc::c_int>(), signed: true },
        'I' => Cls::Int { size: size_of::<libc::c_int>(), signed: false },
        'l' => Cls::Int { size: size_of::<libc::c_long>(), signed: true },
        'L' => Cls::Int { size: size_of::<libc::c_long>(), signed: false },
        'q' => Cls::Int { size: size_of::<libc::c_longlong>(), signed: true },
        'Q' => Cls::Int { size: size_of::<libc::c_longlong>(), signed: false },
        'f' => Cls::F32,
        'd' => Cls::F64,
        'g' => return classify_longdouble(),
        'u' => Cls::Int { size: wchar_size(), signed: false },
        'P' | 'z' | 'Z' | 'O' => Cls::Ptr,
        _ => return None,
    };
    Some(cls)
}

// ----------------------------------------------------------------
// ABI slot assignment (shared by the call and callback directions)
// ----------------------------------------------------------------

/// Where an argument is passed: an integer register, an FP register, or an
/// overflow stack word. Indices are 0-based within each file.
#[derive(Clone, Copy)]
enum Slot {
    Gpr(usize),
    Fpr(usize),
    Stack(usize),
}

/// Assign every argument to an ABI slot, mirroring the platform C calling
/// convention: integer/pointer args fill the general registers then spill
/// to the stack; float/double args fill the FP registers then spill. This
/// single function is used both to *place* outgoing arguments and to
/// *recover* incoming ones in a closure, guaranteeing the two directions
/// agree.
fn assign_slots(classes: &[Cls]) -> Vec<Slot> {
    let mut ngrn = 0usize; // next general register number
    let mut nsrn = 0usize; // next SIMD/FP register number
    let mut nstk = 0usize; // next stack word
    let mut out = Vec::with_capacity(classes.len());
    for &c in classes {
        let slot = match c {
            Cls::F32 | Cls::F64 => {
                if nsrn < native::NFPR_ARG {
                    let s = Slot::Fpr(nsrn);
                    nsrn += 1;
                    s
                } else {
                    let s = Slot::Stack(nstk);
                    nstk += 1;
                    s
                }
            }
            // Int / Ptr / (Void never reaches here as an argument).
            _ => {
                if ngrn < native::NGPR_ARG {
                    let s = Slot::Gpr(ngrn);
                    ngrn += 1;
                    s
                } else {
                    let s = Slot::Stack(nstk);
                    nstk += 1;
                    s
                }
            }
        };
        out.push(slot);
    }
    out
}

// ----------------------------------------------------------------
// Scalar <-> register-bits marshalling
// ----------------------------------------------------------------

/// Sign/zero-extend a `size`-byte integer held in the low bytes of `v` to
/// a full 64-bit register image, as the C ABI requires for sub-word args.
fn widen_int(v: u64, size: usize, signed: bool) -> u64 {
    if size >= 8 {
        return v;
    }
    let bits = size * 8;
    if signed {
        let shift = 64 - bits;
        (((v << shift) as i64) >> shift) as u64
    } else {
        v & ((1u64 << bits) - 1)
    }
}

/// Reinterpret a Python value as the raw 64-bit register image of an
/// integer/char/bool argument. Negative values keep their two's-complement
/// bits. Handles big-int addresses (`Object::Long`) too.
fn payload_as_u64(o: &Object) -> Option<u64> {
    match o {
        Object::Bool(b) => Some(u64::from(*b)),
        Object::None => Some(0),
        _ => o
            .as_i64()
            .map(|i| i as u64)
            .or_else(|| o.as_usize().map(|u| u as u64)),
    }
}

/// Build a Python int from the `size`-byte integer held in the low bytes
/// of `bits`, sign-extending when `signed`.
fn int_object_from_bits(bits: u64, size: usize, signed: bool) -> Object {
    if signed {
        let shift = 64 - size * 8;
        let v = ((bits << shift) as i64) >> shift;
        Object::Int(v)
    } else {
        let v = if size >= 8 {
            bits
        } else {
            bits & ((1u64 << (size * 8)) - 1)
        };
        if v <= i64::MAX as u64 {
            Object::Int(v as i64)
        } else {
            Object::int_from_i128(v as i128)
        }
    }
}

/// Resolve a pointer-class argument to a machine address, allocating a
/// NUL-terminated temporary for `char*`/`wchar_t*` bytes/str payloads and
/// stashing it in `keep` so it outlives the call.
fn pointer_payload(
    code: char,
    payload: &Object,
    keep: &mut Vec<Vec<u8>>,
) -> Result<usize, RuntimeError> {
    match payload {
        Object::None => Ok(0),
        Object::Bytes(_) | Object::ByteArray(_) if code == 'z' => {
            let mut buf = payload.as_bytes_view().unwrap_or_default();
            buf.push(0); // C-string NUL terminator
            let ptr = buf.as_ptr() as usize;
            keep.push(buf);
            Ok(ptr)
        }
        Object::Str(s) if code == 'Z' => {
            let wsize = wchar_size();
            let mut buf: Vec<u8> = Vec::with_capacity((s.chars().count() + 1) * wsize);
            for ch in s.chars() {
                let cp = ch as u32;
                buf.extend_from_slice(&cp.to_ne_bytes()[..wsize]);
            }
            buf.extend_from_slice(&0u32.to_ne_bytes()[..wsize]);
            let ptr = buf.as_ptr() as usize;
            keep.push(buf);
            Ok(ptr)
        }
        _ => payload
            .as_usize()
            .or_else(|| payload.as_i64().map(|i| i as usize))
            .ok_or_else(|| {
                type_error(format!(
                    "call_function: cannot convert {} to a pointer argument",
                    payload.type_name()
                ))
            }),
    }
}

/// Compute the 64-bit register image for one outgoing argument.
fn arg_bits(
    cls: Cls,
    code: char,
    payload: &Object,
    keep: &mut Vec<Vec<u8>>,
) -> Result<u64, RuntimeError> {
    Ok(match cls {
        Cls::Int { size, signed } => {
            let v = payload_as_u64(payload).ok_or_else(|| {
                type_error(format!(
                    "call_function: cannot convert {} to an integer argument (code {code:?})",
                    payload.type_name()
                ))
            })?;
            widen_int(v, size, signed)
        }
        Cls::F32 => {
            let v = payload
                .as_f64()
                .ok_or_else(|| type_error("call_function: float argument expected"))?;
            u64::from((v as f32).to_bits())
        }
        Cls::F64 => {
            let v = payload
                .as_f64()
                .ok_or_else(|| type_error("call_function: float argument expected"))?;
            v.to_bits()
        }
        Cls::Ptr => pointer_payload(code, payload, keep)? as u64,
        Cls::Void => return Err(type_error("call_function: void is not a valid argument type")),
    })
}

/// Marshal the raw result registers into a Python object per the return
/// class. Integer/pointer results are read from the GPR result; float and
/// double results from the FP result (its low 32 / 64 bits).
fn marshal_ret(ret: Cls, ret_gpr: u64, ret_fpr: u64) -> Object {
    match ret {
        Cls::Void => Object::None,
        Cls::F32 => Object::Float(f64::from(f32::from_bits(ret_fpr as u32))),
        Cls::F64 => Object::Float(f64::from_bits(ret_fpr)),
        Cls::Ptr => super::addr_obj(ret_gpr as usize),
        Cls::Int { size, signed } => int_object_from_bits(ret_gpr, size, signed),
    }
}

// ----------------------------------------------------------------
// List extraction
// ----------------------------------------------------------------

fn list_items(o: Option<&Object>) -> Result<Vec<Object>, RuntimeError> {
    match o {
        None | Some(Object::None) => Ok(Vec::new()),
        Some(Object::List(rc)) => Ok(rc.borrow().clone()),
        Some(Object::Tuple(rc)) => Ok(rc.to_vec()),
        Some(other) => Err(type_error(format!(
            "call_function: expected a list (got '{}')",
            other.type_name()
        ))),
    }
}

fn list_chars(o: Option<&Object>) -> Result<Vec<char>, RuntimeError> {
    let mut out = Vec::new();
    for it in list_items(o)? {
        match it {
            Object::Str(s) => out.push(
                s.chars()
                    .next()
                    .ok_or_else(|| value_error("call_function: empty type code"))?,
            ),
            other => {
                return Err(type_error(format!(
                    "call_function: type codes must be str (got '{}')",
                    other.type_name()
                )))
            }
        }
    }
    Ok(out)
}

fn return_class(o: Option<&Object>) -> Result<Cls, RuntimeError> {
    match o {
        None | Some(Object::None) => Ok(Cls::Void),
        Some(Object::Str(s)) => {
            let c = s
                .chars()
                .next()
                .ok_or_else(|| value_error("call_function: empty return type code"))?;
            classify(c)
                .ok_or_else(|| value_error(format!("call_function: unsupported return code {c:?}")))
        }
        Some(other) => Err(type_error(format!(
            "call_function: return code must be str or None (got '{}')",
            other.type_name()
        ))),
    }
}

// ----------------------------------------------------------------
// ctypes private errno swap (FUNCFLAG_USE_ERRNO)
// ----------------------------------------------------------------

#[cfg(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "freebsd",
    target_os = "dragonfly",
    target_os = "openbsd",
    target_os = "netbsd"
))]
fn errno_location() -> *mut i32 {
    unsafe { libc::__error() }
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn errno_location() -> *mut i32 {
    unsafe { libc::__errno_location() }
}

#[cfg(not(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "freebsd",
    target_os = "dragonfly",
    target_os = "openbsd",
    target_os = "netbsd",
    target_os = "linux",
    target_os = "android"
)))]
fn errno_location() -> *mut i32 {
    // No known errno symbol for this target: fall back to a dummy cell so
    // the swap is a harmless no-op rather than UB.
    thread_local! { static DUMMY: std::cell::Cell<i32> = const { std::cell::Cell::new(0) }; }
    DUMMY.with(|c| c.as_ptr())
}

/// Swap the C library `errno` with ctypes' private per-thread errno. Called
/// symmetrically before and after the FFI call when `USE_ERRNO` is set, so
/// the real `errno` reflects the caller's saved value across the call and
/// the callee's `errno` lands back in the private slot (CPython's exact
/// `_ctypes_callproc` protocol).
fn swap_ctypes_errno() {
    let loc = errno_location();
    let real = unsafe { *loc };
    let saved = super::ctypes_errno_replace(real);
    unsafe { *loc = saved };
}

// ----------------------------------------------------------------
// call_function
// ----------------------------------------------------------------

pub(super) fn b_call_function(args: &[Object]) -> Result<Object, RuntimeError> {
    let addr = super::arg_usize(args, 0)?;
    if addr == 0 {
        return Err(value_error("call_function: attempt to call NULL function pointer"));
    }
    if !native::SUPPORTED {
        return Err(value_error(
            "call_function: native FFI is not implemented for this architecture",
        ));
    }
    let ret_cls = return_class(args.get(1))?;
    let codes = list_chars(args.get(2))?;
    let payloads = list_items(args.get(3))?;
    if codes.len() != payloads.len() {
        return Err(type_error(format!(
            "call_function: {} type code(s) but {} argument(s)",
            codes.len(),
            payloads.len()
        )));
    }
    let flags = args.get(4).and_then(Object::as_i64).unwrap_or(0);
    const FUNCFLAG_USE_ERRNO: i64 = 0x8;
    let use_errno = (flags & FUNCFLAG_USE_ERRNO) != 0;

    let n = codes.len();
    let mut classes = Vec::with_capacity(n);
    for &c in &codes {
        classes.push(
            classify(c)
                .ok_or_else(|| value_error(format!("call_function: unsupported arg code {c:?}")))?,
        );
    }
    let slots = assign_slots(&classes);

    let mut gpr = [0u64; 8];
    let mut fpr = [0u64; 8];
    let mut stack: Vec<u64> = Vec::new();
    let mut nfpr: u64 = 0;
    // Temporaries (NUL-terminated string buffers) that must stay alive for
    // the duration of the call.
    let mut keep: Vec<Vec<u8>> = Vec::new();
    for i in 0..n {
        let bits = arg_bits(classes[i], codes[i], &payloads[i], &mut keep)?;
        match slots[i] {
            Slot::Gpr(r) => gpr[r] = bits,
            Slot::Fpr(r) => {
                fpr[r] = bits;
                nfpr = nfpr.max(r as u64 + 1);
            }
            Slot::Stack(_) => stack.push(bits),
        }
    }

    let (ret_gpr, ret_fpr) = unsafe {
        if use_errno {
            swap_ctypes_errno();
        }
        let r = native::raw_call(addr, &gpr, &fpr, &stack, nfpr);
        if use_errno {
            swap_ctypes_errno();
        }
        r
    };
    // Keep the argument backing storage alive until the call has returned.
    drop(keep);
    Ok(marshal_ret(ret_cls, ret_gpr, ret_fpr))
}

// ----------------------------------------------------------------
// create_closure / free_closure (Python callable -> C function ptr)
// ----------------------------------------------------------------

/// Immutable environment bound to a closure trampoline slot. Boxed and
/// handed to [`native::alloc_trampoline`] as the slot's user-data; freed by
/// [`b_free_closure`] (or leaked for the process lifetime if the frozen
/// `_ctypes` never frees it, matching ctypes' "closure lives with the
/// CFUNCTYPE object" lifetime).
struct ClosureData {
    callable: Object,
    arg_codes: Vec<char>,
    arg_classes: Vec<Cls>,
    ret: Cls,
}

/// Read a NUL-terminated C string at `addr` into bytes.
///
/// # Safety
/// `addr` must be a valid, NUL-terminated C string pointer.
unsafe fn read_cstr(addr: usize) -> Vec<u8> {
    unsafe { std::ffi::CStr::from_ptr(addr as *const std::os::raw::c_char) }
        .to_bytes()
        .to_vec()
}

/// Read a NUL-terminated `wchar_t` string at `addr` into a `String`.
///
/// # Safety
/// `addr` must be a valid, NUL-terminated `wchar_t` string pointer.
unsafe fn read_wstr(addr: usize) -> String {
    let wsize = wchar_size();
    let mut out = String::new();
    let mut p = addr;
    loop {
        let cp: u32 = unsafe {
            if wsize == 4 {
                *(p as *const u32)
            } else {
                u32::from(*(p as *const u16))
            }
        };
        if cp == 0 {
            break;
        }
        out.push(char::from_u32(cp).unwrap_or('\u{fffd}'));
        p += wsize;
    }
    out
}

/// Marshal one incoming closure argument (already loaded into a 64-bit
/// register image) into a Python object.
///
/// # Safety
/// For pointer classes, `bits` must be a valid address of the declared
/// kind (`z`/`Z` are dereferenced as C/`wchar_t` strings).
unsafe fn bits_to_object(cls: Cls, code: char, bits: u64) -> Object {
    match cls {
        Cls::Int { size, signed } => int_object_from_bits(bits, size, signed),
        Cls::F32 => Object::Float(f64::from(f32::from_bits(bits as u32))),
        Cls::F64 => Object::Float(f64::from_bits(bits)),
        Cls::Ptr => {
            let addr = bits as usize;
            match code {
                'z' if addr != 0 => Object::new_bytes(unsafe { read_cstr(addr) }),
                'Z' if addr != 0 => Object::from_str(unsafe { read_wstr(addr) }),
                'z' | 'Z' => Object::None,
                _ => super::addr_obj(addr),
            }
        }
        Cls::Void => Object::None,
    }
}

/// Write a closure's Python return value into the result registers. Integer
/// results go to the GPR result register; float/double to the FP result
/// register (its low 32 / 64 bits).
///
/// # Safety
/// `ret_gpr`/`ret_fpr` must point to the trampoline frame's result cells.
unsafe fn write_ret(ret_gpr: *mut u64, ret_fpr: *mut u64, ret: Cls, value: &Object) {
    match ret {
        Cls::Void => {}
        Cls::Int { .. } => unsafe { *ret_gpr = payload_as_u64(value).unwrap_or(0) },
        Cls::Ptr => {
            let a = value
                .as_usize()
                .or_else(|| value.as_i64().map(|i| i as usize))
                .unwrap_or(0);
            unsafe { *ret_gpr = a as u64 };
        }
        Cls::F32 => unsafe {
            *ret_fpr = u64::from((value.as_f64().unwrap_or(0.0) as f32).to_bits())
        },
        Cls::F64 => unsafe { *ret_fpr = value.as_f64().unwrap_or(0.0).to_bits() },
    }
}

/// The Rust side of a closure trampoline: runs whenever the trampoline's
/// code pointer is invoked from C. Reconstructs the Python arguments from
/// the register-file snapshot, re-enters the interpreter published on this
/// thread (the same reentrancy hook the C-API uses), calls the Python
/// callable, and writes the marshalled result back into the result cells.
fn closure_dispatch(userdata: *mut c_void, regs: &native::ClosureRegs) {
    if userdata.is_null() {
        // Should not happen (a live trampoline always has data); leave the
        // result cells as-is.
        return;
    }
    let data: &ClosureData = unsafe { &*(userdata as *const ClosureData) };

    let slots = assign_slots(&data.arg_classes);
    let mut py_args: Vec<Object> = Vec::with_capacity(slots.len());
    for (i, (&cls, &code)) in data.arg_classes.iter().zip(data.arg_codes.iter()).enumerate() {
        let bits = unsafe {
            match slots[i] {
                Slot::Gpr(r) => regs.gpr(r),
                Slot::Fpr(r) => regs.fpr(r),
                Slot::Stack(r) => regs.stack(r),
            }
        };
        py_args.push(unsafe { bits_to_object(cls, code, bits) });
    }

    let outcome = match crate::vm_singletons::current_interpreter_ptr() {
        Some(ptr) if !ptr.is_null() => {
            let vm = unsafe { &mut *ptr };
            vm.call_object(data.callable.clone(), &py_args, &[])
        }
        _ => Err(value_error(
            "ctypes callback invoked with no active interpreter on this thread",
        )),
    };

    let value = match outcome {
        Ok(v) => v,
        Err(e) => {
            // A C caller cannot receive a Python exception; CPython prints
            // it via the unraisable hook and returns 0. We do the safe
            // thing: report (with the exception detail) and fall back to a
            // zero/default result so the C caller keeps running.
            eprintln!("Exception ignored on calling ctypes callback function: {e}");
            Object::None
        }
    };
    unsafe { write_ret(regs.ret_gpr, regs.ret_fpr, data.ret, &value) };
}

pub(super) fn b_create_closure(args: &[Object]) -> Result<Object, RuntimeError> {
    if !native::SUPPORTED {
        // The frozen `_ctypes.py` catches NotImplementedError and degrades
        // to "callable from Python only".
        return Err(RuntimeError::PyException(PyException::from_builtin(
            "NotImplementedError",
            "ctypes closures are not implemented for this architecture",
        )));
    }
    let callable = super::arg(args, 0)?.clone();
    let ret = return_class(args.get(1))?;
    let codes = list_chars(args.get(2))?;

    let mut classes = Vec::with_capacity(codes.len());
    for &c in &codes {
        classes.push(
            classify(c)
                .ok_or_else(|| value_error(format!("create_closure: unsupported arg code {c:?}")))?,
        );
    }

    let data = Box::into_raw(Box::new(ClosureData {
        callable,
        arg_codes: codes,
        arg_classes: classes,
        ret,
    }));
    match native::alloc_trampoline(data as *mut c_void) {
        Some(code) => Ok(super::addr_obj(code)),
        None => {
            // Pool exhausted: reclaim the box we just allocated.
            drop(unsafe { Box::from_raw(data) });
            Err(RuntimeError::PyException(PyException::from_builtin(
                "RuntimeError",
                "ctypes: closure trampoline pool exhausted",
            )))
        }
    }
}

pub(super) fn b_free_closure(args: &[Object]) -> Result<Object, RuntimeError> {
    // The frozen `_ctypes.py` currently never calls this (closures live for
    // the process), but honour it if it ever does: reclaim the slot and the
    // boxed `ClosureData`.
    if let Some(addr) = args.first().and_then(Object::as_usize) {
        if let Some(prev) = native::free_trampoline(addr) {
            drop(unsafe { Box::from_raw(prev as *mut ClosureData) });
        }
    }
    Ok(Object::None)
}

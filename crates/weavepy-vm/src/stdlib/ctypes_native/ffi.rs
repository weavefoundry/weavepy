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
//!   the `FUNCFLAG_*` bits (`USE_ERRNO` is honoured everywhere,
//!   `USE_LASTERROR` on Windows; the rest are calling-convention markers
//!   that need no work on the supported ABIs — Win64 stdcall == cdecl).
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

// On targets without a native back-end ([`native::SUPPORTED`] is false —
// e.g. aarch64-windows) the closure-marshalling half of this module is
// only reachable through the assembly trampolines that aren't compiled
// there, so it trips `dead_code` under `-D warnings`.
#![cfg_attr(
    not(any(
        all(unix, any(target_arch = "aarch64", target_arch = "x86_64")),
        all(windows, target_arch = "x86_64")
    )),
    allow(dead_code)
)]

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
/// `double` (8 bytes), so we can marshal it as `f64`. The same holds on
/// Windows, where MSVC (the ABI of the system DLLs and of CPython, which
/// reports `sizeof(c_longdouble) == 8` there) defines `long double` ==
/// `double` on every architecture. On unix x86 it is the 80-bit extended
/// type, which cannot round-trip through a Python float, so we decline it
/// (callers get a clear error).
fn classify_longdouble() -> Option<Cls> {
    #[cfg(any(target_arch = "aarch64", target_arch = "arm", windows))]
    {
        Some(Cls::F64)
    }
    #[cfg(not(any(target_arch = "aarch64", target_arch = "arm", windows)))]
    {
        None
    }
}

fn classify(code: char) -> Option<Cls> {
    use std::mem::size_of;
    let cls = match code {
        'b' => Cls::Int {
            size: 1,
            signed: true,
        },
        'B' | 'c' | '?' => Cls::Int {
            size: 1,
            signed: false,
        },
        'h' => Cls::Int {
            size: size_of::<libc::c_short>(),
            signed: true,
        },
        'H' => Cls::Int {
            size: size_of::<libc::c_short>(),
            signed: false,
        },
        'i' => Cls::Int {
            size: size_of::<libc::c_int>(),
            signed: true,
        },
        'I' => Cls::Int {
            size: size_of::<libc::c_int>(),
            signed: false,
        },
        'l' => Cls::Int {
            size: size_of::<libc::c_long>(),
            signed: true,
        },
        'L' => Cls::Int {
            size: size_of::<libc::c_long>(),
            signed: false,
        },
        'q' => Cls::Int {
            size: size_of::<libc::c_longlong>(),
            signed: true,
        },
        'Q' => Cls::Int {
            size: size_of::<libc::c_longlong>(),
            signed: false,
        },
        'f' => Cls::F32,
        'd' => Cls::F64,
        'g' => return classify_longdouble(),
        'u' => Cls::Int {
            size: wchar_size(),
            signed: false,
        },
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
///
/// `variadic_from` is the index of the first *anonymous* (variadic)
/// argument — everything at or past it belongs to a callee's `...` tail.
/// On Apple arm64 the AAPCS diverges from the standard convention there:
/// anonymous arguments always go on the stack (8-byte words), never in
/// registers, so calling a true variadic like `PyBytes_FromFormat` with
/// register-passed extras hands the callee garbage. Elsewhere (x86-64
/// SysV, Windows x64, Linux aarch64) variadic args use the ordinary slots.
///
/// On Windows the Microsoft x64 convention assigns slots by *position*,
/// not by class: argument `i` (i < 4) burns register slot `i` of both
/// files at once (rcx/rdx/r8/r9 for integers, xmm0..3 for floats — an FP
/// argument in position 1 lands in xmm1 and leaves rdx dead), and the
/// 5th argument onward goes to 8-byte stack slots above the 32-byte
/// shadow space (which the call gate owns, so `Slot::Stack(0)` is still
/// the first overflow word here).
fn assign_slots(classes: &[Cls], variadic_from: usize) -> Vec<Slot> {
    let apple_arm64_variadic_stack = cfg!(all(
        any(target_os = "macos", target_os = "ios"),
        target_arch = "aarch64"
    ));
    let win64_positional = cfg!(windows);
    let mut ngrn = 0usize; // next general register number (Win64: next position)
    let mut nsrn = 0usize; // next SIMD/FP register number (Win64: unused)
    let mut nstk = 0usize; // next stack word
    let mut out = Vec::with_capacity(classes.len());
    for (i, &c) in classes.iter().enumerate() {
        if apple_arm64_variadic_stack && i >= variadic_from {
            out.push(Slot::Stack(nstk));
            nstk += 1;
            continue;
        }
        let slot = if win64_positional {
            if ngrn < native::NGPR_ARG {
                let pos = ngrn;
                ngrn += 1;
                match c {
                    Cls::F32 | Cls::F64 => Slot::Fpr(pos),
                    _ => Slot::Gpr(pos),
                }
            } else {
                let s = Slot::Stack(nstk);
                nstk += 1;
                s
            }
        } else {
            match c {
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
        match i64::try_from(v) {
            Ok(i) => Object::Int(i),
            Err(_) => Object::int_from_i128(i128::from(v)),
        }
    }
}

/// Process-lifetime intern table for NUL-terminated string payloads: the
/// pointer handed to C stays valid forever, mirroring CPython's semantics
/// where the pointer lives as long as the (usually constant) bytes object.
/// Deduplicated by content, so repeated calls with the same string cost one
/// allocation total.
pub(super) fn interned_cstr(buf: Vec<u8>) -> usize {
    use std::collections::HashSet;
    use std::sync::Mutex;
    static INTERN: Mutex<Option<HashSet<&'static [u8]>>> = Mutex::new(None);
    let mut g = INTERN.lock().unwrap_or_else(|e| e.into_inner());
    let set = g.get_or_insert_with(HashSet::new);
    if let Some(existing) = set.get(buf.as_slice()) {
        return existing.as_ptr() as usize;
    }
    let leaked: &'static [u8] = Box::leak(buf.into_boxed_slice());
    set.insert(leaked);
    leaked.as_ptr() as usize
}

/// Resolve a pointer-class argument to a machine address, allocating a
/// NUL-terminated temporary for `char*`/`wchar_t*` bytes/str payloads and
/// stashing it in `keep` so it outlives the call. A `py_object` argument
/// (`'O'`) is marshalled through the capi bridge to an owned `PyObject*`,
/// recorded in `owned` for release after the call returns.
fn pointer_payload(
    code: char,
    payload: &Object,
    keep: &mut Vec<Vec<u8>>,
    owned: &mut Vec<usize>,
) -> Result<usize, RuntimeError> {
    if code == 'O' {
        // Legacy escape hatch: an explicit integer payload is already a
        // raw `PyObject*` address; anything else is the object itself.
        // (`Object::None` must marshal to `Py_None`, not NULL.)
        if let Object::Int(_) | Object::Long(_) = payload {
            if let Some(addr) = payload.as_usize() {
                return Ok(addr);
            }
        }
        let ptr = crate::foreign::object_to_owned_ptr(payload)?;
        owned.push(ptr);
        return Ok(ptr);
    }
    match payload {
        Object::None => Ok(0),
        Object::Bytes(_) if code == 'z' => {
            // CPython passes a pointer into the bytes object's *own* buffer
            // (`ob_sval`, NUL-terminated), valid for the object's lifetime —
            // and callees exploit that by stashing the pointer past the call
            // (lxml's `adopt_external_document` `strcmp`s the capsule context
            // set by an earlier `PyCapsule_SetContext(cap, b"destructor:…")`).
            // A per-call temporary dangles for that pattern, so hand out a
            // process-lifetime interned copy instead: one small allocation
            // per distinct string, matching the usual "module-level constant"
            // lifetime on the CPython side.
            let mut buf = payload.as_bytes_view().unwrap_or_default();
            buf.push(0); // C-string NUL terminator
            Ok(interned_cstr(buf))
        }
        Object::ByteArray(_) if code == 'z' => {
            // Mutable buffer: contents may differ per call, keep per-call.
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
            // Same lifetime hazard as the `'z'` arm (CPython's wchar
            // conversion is cached on the str object for its lifetime).
            Ok(interned_cstr(buf))
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
    owned: &mut Vec<usize>,
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
            // C observes raw bits — strip the WeavePy NaN identity tag.
            u64::from((crate::object::untag_nan(v) as f32).to_bits())
        }
        Cls::F64 => {
            let v = payload
                .as_f64()
                .ok_or_else(|| type_error("call_function: float argument expected"))?;
            crate::object::untag_nan(v).to_bits()
        }
        Cls::Ptr => pointer_payload(code, payload, keep, owned)? as u64,
        Cls::Void => {
            return Err(type_error(
                "call_function: void is not a valid argument type",
            ))
        }
    })
}

/// Marshal the raw result registers into a Python object per the return
/// class. Integer/pointer results are read from the GPR result; float and
/// double results from the FP result (its low 32 / 64 bits).
fn marshal_ret(ret: Cls, ret_gpr: u64, ret_fpr: u64) -> Object {
    match ret {
        Cls::Void => Object::None,
        // Fresh object per call in CPython; a canonical NaN from C gets a
        // fresh identity, an exotic payload is preserved verbatim.
        Cls::F32 => Object::Float(crate::object::tag_unpacked_nan(f64::from(f32::from_bits(
            ret_fpr as u32,
        )))),
        Cls::F64 => Object::Float(crate::object::tag_unpacked_nan(f64::from_bits(ret_fpr))),
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
// ctypes private LastError swap (FUNCFLAG_USE_LASTERROR, Windows)
// ----------------------------------------------------------------

/// Swap the thread's real Win32 `LastError` with ctypes' private per-thread
/// copy — the exactly-parallel mechanism to [`swap_ctypes_errno`] for
/// `FUNCFLAG_USE_LASTERROR`. CPython keeps both values in one per-thread
/// array (`Modules/_ctypes/callproc.c` `_ctypes_get_errobj`: errno in
/// `space[0]`, LastError in `space[1]`) and swaps each symmetrically around
/// the foreign call.
#[cfg(windows)]
fn swap_ctypes_last_error() {
    use windows_sys::Win32::Foundation::{GetLastError, SetLastError};
    let real = unsafe { GetLastError() };
    let saved = super::ctypes_last_error_replace(real);
    unsafe { SetLastError(saved) };
}

// ----------------------------------------------------------------
// call_function
// ----------------------------------------------------------------

pub(super) fn b_call_function(args: &[Object]) -> Result<Object, RuntimeError> {
    let addr = super::arg_usize(args, 0)?;
    if addr == 0 {
        return Err(value_error(
            "call_function: attempt to call NULL function pointer",
        ));
    }
    if !native::SUPPORTED {
        return Err(value_error(
            "call_function: native FFI is not implemented for this architecture",
        ));
    }
    let ret_cls = return_class(args.get(1))?;
    // `py_object` restype: the callee returns an owned `PyObject*` that
    // must be converted back to the VM object it denotes (or, for NULL,
    // into the pending C exception) rather than surfaced as an address.
    let ret_is_object = matches!(args.get(1), Some(Object::Str(s)) if s.as_ref() == "O");
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
    // FUNCFLAG_USE_LASTERROR is meaningful on Windows only (GetLastError is
    // a Win32 concept); elsewhere the bit is accepted and ignored, exactly
    // like CPython's non-MS_WIN32 build of `_call_function_pointer`.
    #[cfg(windows)]
    let use_last_error = {
        const FUNCFLAG_USE_LASTERROR: i64 = 0x10;
        (flags & FUNCFLAG_USE_LASTERROR) != 0
    };

    let n = codes.len();
    // Index of the first *variadic* argument (args past the declared
    // argtypes). Defaults to "all fixed" when the caller doesn't say —
    // like libffi's `ffi_prep_cif` vs `ffi_prep_cif_var`, this only
    // changes slot assignment on Apple arm64 (see `assign_slots`).
    let variadic_from = args
        .get(5)
        .and_then(Object::as_i64)
        .map_or(n, |v| usize::try_from(v).unwrap_or(n).min(n));
    let mut classes = Vec::with_capacity(n);
    for &c in &codes {
        classes
            .push(classify(c).ok_or_else(|| {
                value_error(format!("call_function: unsupported arg code {c:?}"))
            })?);
    }
    let slots = assign_slots(&classes, variadic_from);

    let mut gpr = [0u64; 8];
    let mut fpr = [0u64; 8];
    let mut stack: Vec<u64> = Vec::new();
    let mut nfpr: u64 = 0;
    // Temporaries (NUL-terminated string buffers) that must stay alive for
    // the duration of the call.
    let mut keep: Vec<Vec<u8>> = Vec::new();
    // Owned `PyObject*` references minted for `py_object` ('O') arguments,
    // released after the call returns.
    let mut owned: Vec<usize> = Vec::new();
    for i in 0..n {
        let bits = match arg_bits(classes[i], codes[i], &payloads[i], &mut keep, &mut owned) {
            Ok(bits) => bits,
            Err(e) => {
                for p in owned {
                    crate::foreign::release_object_ptr(p);
                }
                return Err(e);
            }
        };
        match slots[i] {
            Slot::Gpr(r) => gpr[r] = bits,
            Slot::Fpr(r) => {
                fpr[r] = bits;
                nfpr = nfpr.max(r as u64 + 1);
                // Win64 varargs rule ("Varargs" in the x64 calling
                // convention doc): an FP argument to a variadic or
                // unprototyped function must be duplicated in the
                // positionally-corresponding integer register, because the
                // callee's va_arg walks the GPR home area. We don't know
                // the callee's real prototype here, so always mirror — for
                // a prototyped callee the shadowed GPR slot is simply dead
                // (this is what libffi's win64 port does too).
                if cfg!(windows) {
                    gpr[r] = bits;
                }
            }
            Slot::Stack(_) => stack.push(bits),
        }
    }

    let (ret_gpr, ret_fpr) = unsafe {
        if use_errno {
            swap_ctypes_errno();
        }
        // The LastError swap nests *inside* the errno swap, immediately
        // around the call (callproc.c `_call_function_pointer`): no
        // intervening code may run between the callee returning and the
        // swap-out, or a stray Win32 call would clobber what it set.
        #[cfg(windows)]
        if use_last_error {
            swap_ctypes_last_error();
        }
        let r = native::raw_call(addr, &gpr, &fpr, &stack, nfpr);
        #[cfg(windows)]
        if use_last_error {
            swap_ctypes_last_error();
        }
        if use_errno {
            swap_ctypes_errno();
        }
        r
    };
    // Keep the argument backing storage alive until the call has returned.
    drop(keep);
    for p in owned {
        crate::foreign::release_object_ptr(p);
    }
    if ret_is_object {
        return crate::foreign::steal_object(ret_gpr as usize);
    }
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
        // C observes raw bits — strip the WeavePy NaN identity tag.
        Cls::F32 => unsafe {
            *ret_fpr = u64::from(
                (crate::object::untag_nan(value.as_f64().unwrap_or(0.0)) as f32).to_bits(),
            )
        },
        Cls::F64 => unsafe {
            *ret_fpr = crate::object::untag_nan(value.as_f64().unwrap_or(0.0)).to_bits()
        },
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

    // Closures (CFUNCTYPE) are never variadic: every arg is fixed.
    let slots = assign_slots(&data.arg_classes, data.arg_classes.len());
    let mut py_args: Vec<Object> = Vec::with_capacity(slots.len());
    for (i, (&cls, &code)) in data
        .arg_classes
        .iter()
        .zip(data.arg_codes.iter())
        .enumerate()
    {
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
            classify(c).ok_or_else(|| {
                value_error(format!("create_closure: unsupported arg code {c:?}"))
            })?,
        );
    }

    let data = Box::into_raw(Box::new(ClosureData {
        callable,
        arg_codes: codes,
        arg_classes: classes,
        ret,
    }));
    match native::alloc_trampoline(data.cast::<c_void>()) {
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
            drop(unsafe { Box::from_raw(prev.cast::<ClosureData>()) });
        }
    }
    Ok(Object::None)
}

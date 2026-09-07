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
//! `void*`, `char*`, `wchar_t*`, `PyObject*`). Pointers and arrays are
//! marshalled by address (`P`) on the Python side. A *by-value*
//! struct/union (a `Structure`/`Union` argtype or restype) is described
//! by an aggregate descriptor `(size, [(offset, code), ...])` in place of
//! the code string — its scalar leaves, arrays and nested aggregates
//! flattened — with the instance's raw bytes as the payload; the bridge
//! applies the platform's aggregate classification (AAPCS64 HFAs and
//! composites, System V eightbytes, Win64 size rules) to place the pieces
//! in registers, on the stack, or behind a hidden pointer, and rebuilds
//! the result bytes the same way.
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
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
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
// Aggregate (by-value struct/union) layouts
// ----------------------------------------------------------------

/// The ABI-relevant shape of a by-value aggregate: its size and the
/// scalar leaves it is made of (arrays and nested aggregates flattened),
/// each with its byte offset. The frozen `_ctypes.py` describes one as the
/// tuple `(size, [(offset, code), ...])`.
#[derive(Clone, Debug)]
struct AggLayout {
    size: usize,
    leaves: Vec<(usize, Cls)>,
}

impl AggLayout {
    fn words(&self) -> usize {
        self.size.div_ceil(8).max(1)
    }

    /// AAPCS64 Homogeneous Floating-point Aggregate: one to four members
    /// of the same floating-point type (and nothing else). Returns the
    /// element class and member count.
    fn hfa(&self) -> Option<(Cls, usize)> {
        let first = *self.leaves.first()?;
        let esize = match first.1 {
            Cls::F32 => 4,
            Cls::F64 => 8,
            _ => return None,
        };
        if self.leaves.iter().any(|&(_, c)| c != first.1) {
            return None;
        }
        // Distinct storage positions (a union of floats counts once).
        let mut offs: Vec<usize> = self.leaves.iter().map(|&(o, _)| o).collect();
        offs.sort_unstable();
        offs.dedup();
        let n = offs.len();
        if !(1..=4).contains(&n) || self.size != n * esize {
            return None;
        }
        Some((first.1, n))
    }

    /// System V x86-64 eightbyte classification: `true` for an SSE
    /// eightbyte (only floating-point leaves), `false` for INTEGER.
    fn sysv_eightbytes(&self) -> Vec<bool> {
        (0..self.words())
            .map(|i| {
                let lo = i * 8;
                let hi = lo + 8;
                let mut any = false;
                let mut all_fp = true;
                for &(off, c) in &self.leaves {
                    let len = match c {
                        Cls::Int { size, .. } => size,
                        Cls::F32 => 4,
                        Cls::F64 | Cls::Ptr => 8,
                        Cls::Void => 0,
                    };
                    if off < hi && off + len > lo {
                        any = true;
                        if !matches!(c, Cls::F32 | Cls::F64) {
                            all_fp = false;
                        }
                    }
                }
                any && all_fp
            })
            .collect()
    }
}

/// An argument's ABI type: a scalar class or a by-value aggregate.
#[derive(Clone, Debug)]
enum ArgTy {
    Scalar(Cls),
    Agg(AggLayout),
}

/// Parse an aggregate descriptor `(size, [(offset, code), ...])`.
fn parse_agg(o: &Object, what: &str) -> Result<AggLayout, RuntimeError> {
    let items = list_items(Some(o))?;
    let bad = || type_error(format!("{what}: malformed aggregate descriptor"));
    if items.len() != 2 {
        return Err(bad());
    }
    let size = items[0].as_usize().ok_or_else(bad)?;
    let mut leaves = Vec::new();
    for leaf in list_items(Some(&items[1]))? {
        let pair = list_items(Some(&leaf))?;
        if pair.len() != 2 {
            return Err(bad());
        }
        let off = pair[0].as_usize().ok_or_else(bad)?;
        let code = match &pair[1] {
            Object::Str(s) => s.chars().next().ok_or_else(bad)?,
            _ => return Err(bad()),
        };
        let cls = classify(code)
            .ok_or_else(|| value_error(format!("{what}: unsupported field code {code:?}")))?;
        leaves.push((off, cls));
    }
    Ok(AggLayout { size, leaves })
}

/// Parse one argument type: a one-character code string, or an aggregate
/// descriptor tuple.
fn parse_arg_ty(o: &Object, what: &str) -> Result<ArgTy, RuntimeError> {
    match o {
        Object::Str(s) => {
            let c = s
                .chars()
                .next()
                .ok_or_else(|| value_error(format!("{what}: empty type code")))?;
            classify(c)
                .map(ArgTy::Scalar)
                .ok_or_else(|| value_error(format!("{what}: unsupported arg code {c:?}")))
        }
        Object::Tuple(_) | Object::List(_) => Ok(ArgTy::Agg(parse_agg(o, what)?)),
        other => Err(type_error(format!(
            "{what}: type codes must be str or aggregate descriptors (got '{}')",
            other.type_name()
        ))),
    }
}

// ----------------------------------------------------------------
// ABI slot assignment (shared by the call and callback directions)
// ----------------------------------------------------------------

/// Where a scalar (or one piece of an aggregate) is passed: an integer
/// register, an FP register, or an overflow stack word. Indices are
/// 0-based within each file.
#[derive(Clone, Copy, Debug)]
enum Slot {
    Gpr(usize),
    Fpr(usize),
    Stack(usize),
}

/// Where a whole argument lands.
#[derive(Clone, Debug)]
enum Place {
    /// A scalar in one slot.
    Scalar(Slot),
    /// An aggregate split into register pieces: `(slot, byte offset within
    /// the aggregate, byte length)` per piece. AArch64 composites use
    /// 8-byte pieces in consecutive GPRs and HFAs one member per FPR;
    /// System V uses one piece per eightbyte (INTEGER -> GPR, SSE -> FPR);
    /// Win64 passes power-of-two-sized aggregates as one integer piece.
    Pieces(Vec<(Slot, usize, usize)>),
    /// An aggregate copied whole onto the stack starting at this word.
    StackCopy(usize),
    /// A pointer to a caller-owned copy of the aggregate, in this slot
    /// (AArch64 composites > 16 bytes, Win64 odd-sized aggregates).
    Indirect(Slot),
}

/// Per-platform register budget consumed as arguments are placed.
struct Budget {
    ngrn: usize, // next general register (Win64: next position)
    nsrn: usize, // next SIMD/FP register (Win64: unused)
    nstk: usize, // next stack word
}

impl Budget {
    fn gpr_or_stack(&mut self) -> Slot {
        if self.ngrn < native::NGPR_ARG {
            let s = Slot::Gpr(self.ngrn);
            self.ngrn += 1;
            s
        } else {
            let s = Slot::Stack(self.nstk);
            self.nstk += 1;
            s
        }
    }
    fn fpr_or_stack(&mut self) -> Slot {
        if self.nsrn < native::NFPR_ARG {
            let s = Slot::Fpr(self.nsrn);
            self.nsrn += 1;
            s
        } else {
            let s = Slot::Stack(self.nstk);
            self.nstk += 1;
            s
        }
    }
    fn stack_copy(&mut self, words: usize) -> Place {
        let p = Place::StackCopy(self.nstk);
        self.nstk += words;
        p
    }
}

const APPLE_ARM64: bool = cfg!(all(
    any(target_os = "macos", target_os = "ios"),
    target_arch = "aarch64"
));
const WIN64: bool = cfg!(windows);
const AARCH64: bool = cfg!(target_arch = "aarch64");

/// Assign every argument to its ABI placement, mirroring the platform C
/// calling convention. This single function is used both to *place*
/// outgoing arguments and to *recover* incoming ones in a closure,
/// guaranteeing the two directions agree.
///
/// `variadic_from` is the index of the first *anonymous* (variadic)
/// argument — everything at or past it belongs to a callee's `...` tail.
/// On Apple arm64 the AAPCS diverges from the standard convention there:
/// anonymous arguments always go on the stack (8-byte words), never in
/// registers, so calling a true variadic like `PyBytes_FromFormat` with
/// register-passed extras hands the callee garbage. Elsewhere (x86-64
/// SysV, Windows x64, Linux aarch64) variadic args use the ordinary slots.
///
/// `hidden_ret` reserves the first integer slot for the hidden pointer to
/// a MEMORY-class result (System V / Win64 large struct returns; AArch64
/// uses `x8` instead and never sets it).
///
/// Scalars: integer/pointer args fill the general registers then spill to
/// the stack; float/double args fill the FP registers then spill. On
/// Windows the Microsoft x64 convention assigns slots by *position*, not
/// by class: argument `i` (i < 4) burns register slot `i` of both files
/// at once (rcx/rdx/r8/r9 for integers, xmm0..3 for floats), and the 5th
/// argument onward goes to 8-byte stack slots above the 32-byte shadow
/// space (which the call gate owns, so `Slot::Stack(0)` is still the
/// first overflow word here).
///
/// Aggregates follow the platform rules for by-value structs (AAPCS64
/// B.3-C.15, SysV 3.2.3, Win64 "Parameter Passing").
fn assign_places(tys: &[ArgTy], variadic_from: usize, hidden_ret: bool) -> Vec<Place> {
    let mut b = Budget {
        ngrn: usize::from(hidden_ret),
        nsrn: 0,
        nstk: 0,
    };
    let mut out = Vec::with_capacity(tys.len());
    for (i, ty) in tys.iter().enumerate() {
        let variadic = i >= variadic_from;
        let place = match ty {
            ArgTy::Scalar(c) => {
                if APPLE_ARM64 && variadic {
                    let s = Slot::Stack(b.nstk);
                    b.nstk += 1;
                    Place::Scalar(s)
                } else if WIN64 {
                    if b.ngrn < native::NGPR_ARG {
                        let pos = b.ngrn;
                        b.ngrn += 1;
                        Place::Scalar(match c {
                            Cls::F32 | Cls::F64 => Slot::Fpr(pos),
                            _ => Slot::Gpr(pos),
                        })
                    } else {
                        let s = Slot::Stack(b.nstk);
                        b.nstk += 1;
                        Place::Scalar(s)
                    }
                } else {
                    Place::Scalar(match c {
                        Cls::F32 | Cls::F64 => b.fpr_or_stack(),
                        _ => b.gpr_or_stack(),
                    })
                }
            }
            ArgTy::Agg(l) => place_agg(&mut b, l, variadic),
        };
        out.push(place);
    }
    out
}

fn place_agg(b: &mut Budget, l: &AggLayout, variadic: bool) -> Place {
    let words = l.words();
    if WIN64 {
        // Win64: aggregates of 1, 2, 4 or 8 bytes travel as that many
        // bytes in an integer slot; anything else by pointer to a copy.
        let slot = b.gpr_or_stack();
        return if matches!(l.size, 1 | 2 | 4 | 8) {
            Place::Pieces(vec![(slot, 0, l.size)])
        } else {
            Place::Indirect(slot)
        };
    }
    if AARCH64 {
        if APPLE_ARM64 && variadic {
            // Apple: anonymous composites are copied onto the stack (or,
            // when larger than 16 bytes, passed by pointer on the stack).
            if l.size > 16 {
                let s = Slot::Stack(b.nstk);
                b.nstk += 1;
                return Place::Indirect(s);
            }
            return b.stack_copy(words);
        }
        if let Some((cls, n)) = l.hfa() {
            let esize = if cls == Cls::F32 { 4 } else { 8 };
            if b.nsrn + n <= native::NFPR_ARG {
                let pieces = (0..n)
                    .map(|k| {
                        let s = Slot::Fpr(b.nsrn);
                        b.nsrn += 1;
                        (s, k * esize, esize)
                    })
                    .collect();
                return Place::Pieces(pieces);
            }
            // C.3: not enough FP registers left — the whole HFA goes on
            // the stack and no further FP registers are used.
            b.nsrn = native::NFPR_ARG;
            return b.stack_copy(words);
        }
        if l.size > 16 {
            // B.4: copied to memory, pointer passed in its place.
            return Place::Indirect(b.gpr_or_stack());
        }
        if b.ngrn + words <= native::NGPR_ARG {
            let pieces = (0..words)
                .map(|k| {
                    let s = Slot::Gpr(b.ngrn);
                    b.ngrn += 1;
                    (s, k * 8, (l.size - k * 8).min(8))
                })
                .collect();
            return Place::Pieces(pieces);
        }
        // C.11: not enough integer registers — stack, and no further GPRs.
        b.ngrn = native::NGPR_ARG;
        return b.stack_copy(words);
    }
    // System V x86-64.
    if l.size > 16 {
        return b.stack_copy(words);
    }
    let classes = l.sysv_eightbytes();
    let need_fp = classes.iter().filter(|&&sse| sse).count();
    let need_int = classes.len() - need_fp;
    if b.ngrn + need_int > native::NGPR_ARG || b.nsrn + need_fp > native::NFPR_ARG {
        // 3.2.3 rule 5c: the whole argument is passed in memory.
        return b.stack_copy(words);
    }
    let pieces = classes
        .iter()
        .enumerate()
        .map(|(k, &sse)| {
            let s = if sse {
                b.fpr_or_stack()
            } else {
                b.gpr_or_stack()
            };
            (s, k * 8, (l.size - k * 8).min(8))
        })
        .collect();
    Place::Pieces(pieces)
}

/// How a by-value aggregate result comes back.
#[derive(Clone, Debug)]
enum RetPlace {
    /// Pieces from the result registers: `(slot, offset, len)`, where
    /// `Slot::Gpr(i)` is the i-th integer result register (x0/x1,
    /// rax/rdx) and `Slot::Fpr(i)` the i-th FP result register.
    Pieces(Vec<(Slot, usize, usize)>),
    /// Written by the callee into a caller-provided buffer whose address
    /// is passed hidden (x8 on AArch64; the first integer argument on
    /// x86-64 System V and Win64).
    Indirect,
}

fn ret_place(l: &AggLayout) -> RetPlace {
    let words = l.words();
    if WIN64 {
        return if matches!(l.size, 1 | 2 | 4 | 8) {
            RetPlace::Pieces(vec![(Slot::Gpr(0), 0, l.size)])
        } else {
            RetPlace::Indirect
        };
    }
    if AARCH64 {
        if let Some((cls, n)) = l.hfa() {
            let esize = if cls == Cls::F32 { 4 } else { 8 };
            return RetPlace::Pieces((0..n).map(|k| (Slot::Fpr(k), k * esize, esize)).collect());
        }
        if l.size > 16 {
            return RetPlace::Indirect;
        }
        return RetPlace::Pieces(
            (0..words)
                .map(|k| (Slot::Gpr(k), k * 8, (l.size - k * 8).min(8)))
                .collect(),
        );
    }
    if l.size > 16 {
        return RetPlace::Indirect;
    }
    let (mut ni, mut nf) = (0, 0);
    RetPlace::Pieces(
        l.sysv_eightbytes()
            .iter()
            .enumerate()
            .map(|(k, &sse)| {
                let s = if sse {
                    nf += 1;
                    Slot::Fpr(nf - 1)
                } else {
                    ni += 1;
                    Slot::Gpr(ni - 1)
                };
                (s, k * 8, (l.size - k * 8).min(8))
            })
            .collect(),
    )
}

/// Little-endian read of `len` (<= 8) bytes at `off` as a register image.
fn bytes_to_word(buf: &[u8], off: usize, len: usize) -> u64 {
    let mut w = [0u8; 8];
    let end = (off + len).min(buf.len());
    if off < end {
        w[..end - off].copy_from_slice(&buf[off..end]);
    }
    u64::from_ne_bytes(w)
}

/// Copy `buf` into 8-byte words (zero padded).
fn bytes_to_words(buf: &[u8]) -> Vec<u64> {
    (0..buf.len().div_ceil(8).max(1))
        .map(|k| bytes_to_word(buf, k * 8, 8))
        .collect()
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
        // The payload is the object itself, ints included: CPython's
        // `py_object.from_param(123)` hands the callee the int object, so
        // `pythonapi.PyOS_FSPath(123)` must reach `PyOS_FSPath` as a
        // `PyObject*` (test_capi.test_file.test_py_fopen), not as the raw
        // address 123. (`Object::None` marshals to `Py_None`, not NULL.)
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

/// A call's result type: `void`, a scalar class, or a by-value aggregate.
#[derive(Clone, Debug)]
enum RetTy {
    Scalar(Cls),
    Agg(AggLayout),
}

fn return_ty(o: Option<&Object>, what: &str) -> Result<RetTy, RuntimeError> {
    match o {
        None | Some(Object::None) => Ok(RetTy::Scalar(Cls::Void)),
        Some(Object::Str(s)) => {
            let c = s
                .chars()
                .next()
                .ok_or_else(|| value_error(format!("{what}: empty return type code")))?;
            classify(c)
                .map(RetTy::Scalar)
                .ok_or_else(|| value_error(format!("{what}: unsupported return code {c:?}")))
        }
        Some(o @ (Object::Tuple(_) | Object::List(_))) => Ok(RetTy::Agg(parse_agg(o, what)?)),
        Some(other) => Err(type_error(format!(
            "{what}: return code must be str, an aggregate descriptor or None (got '{}')",
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
    let ret_ty = return_ty(args.get(1), "call_function")?;
    // `py_object` restype: the callee returns an owned `PyObject*` that
    // must be converted back to the VM object it denotes (or, for NULL,
    // into the pending C exception) rather than surfaced as an address.
    let ret_is_object = matches!(args.get(1), Some(Object::Str(s)) if s.as_ref() == "O");
    let code_objs = list_items(args.get(2))?;
    let payloads = list_items(args.get(3))?;
    if code_objs.len() != payloads.len() {
        return Err(type_error(format!(
            "call_function: {} type code(s) but {} argument(s)",
            code_objs.len(),
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

    let n = code_objs.len();
    // Index of the first *variadic* argument (args past the declared
    // argtypes). Defaults to "all fixed" when the caller doesn't say —
    // like libffi's `ffi_prep_cif` vs `ffi_prep_cif_var`, this only
    // changes slot assignment on Apple arm64 (see `assign_places`).
    let variadic_from = args
        .get(5)
        .and_then(Object::as_i64)
        .map_or(n, |v| usize::try_from(v).unwrap_or(n).min(n));
    let mut tys = Vec::with_capacity(n);
    let mut codes = Vec::with_capacity(n);
    for o in &code_objs {
        let ty = parse_arg_ty(o, "call_function")?;
        codes.push(match o {
            Object::Str(s) => s.chars().next().unwrap_or('S'),
            _ => 'S',
        });
        tys.push(ty);
    }

    // A MEMORY-class aggregate result needs a caller-owned buffer; on
    // x86-64 its address is the hidden first integer argument.
    let ret_agg = match &ret_ty {
        RetTy::Agg(l) => Some((l.clone(), ret_place(l))),
        RetTy::Scalar(_) => None,
    };
    let mut ret_buf: Vec<u64> = Vec::new();
    let hidden_ret = matches!(ret_agg, Some((_, RetPlace::Indirect)));
    if let Some((l, RetPlace::Indirect)) = &ret_agg {
        ret_buf = vec![0u64; l.words()];
    }
    let places = assign_places(&tys, variadic_from, hidden_ret && !AARCH64);

    let mut gpr = [0u64; 8];
    let mut fpr = [0u64; 8];
    let mut stack: Vec<u64> = Vec::new();
    let mut nfpr: u64 = 0;
    // Temporaries (NUL-terminated string buffers) that must stay alive for
    // the duration of the call.
    let mut keep: Vec<Vec<u8>> = Vec::new();
    // Caller-owned copies of aggregates passed by hidden pointer.
    let mut agg_copies: Vec<Vec<u64>> = Vec::new();
    // Owned `PyObject*` references minted for `py_object` ('O') arguments,
    // released after the call returns.
    let mut owned: Vec<usize> = Vec::new();
    let mut indirect: u64 = 0;
    if hidden_ret {
        let a = ret_buf.as_mut_ptr() as u64;
        if AARCH64 {
            indirect = a;
        } else {
            gpr[0] = a;
        }
    }

    let mut set_slot = |slot: Slot, bits: u64, gpr: &mut [u64; 8], fpr: &mut [u64; 8]| match slot {
        Slot::Gpr(r) => gpr[r] = bits,
        Slot::Fpr(r) => {
            fpr[r] = bits;
            nfpr = nfpr.max(r as u64 + 1);
            // Win64 varargs rule ("Varargs" in the x64 calling convention
            // doc): an FP argument to a variadic or unprototyped function
            // must be duplicated in the positionally-corresponding
            // integer register, because the callee's va_arg walks the
            // GPR home area. We don't know the callee's real prototype
            // here, so always mirror — for a prototyped callee the
            // shadowed GPR slot is simply dead (this is what libffi's
            // win64 port does too).
            if WIN64 {
                gpr[r] = bits;
            }
        }
        Slot::Stack(w) => {
            if stack.len() <= w {
                stack.resize(w + 1, 0);
            }
            stack[w] = bits;
        }
    };

    for i in 0..n {
        let res: Result<(), RuntimeError> = match (&tys[i], &places[i]) {
            (ArgTy::Scalar(cls), Place::Scalar(slot)) => {
                arg_bits(*cls, codes[i], &payloads[i], &mut keep, &mut owned)
                    .map(|bits| set_slot(*slot, bits, &mut gpr, &mut fpr))
            }
            (ArgTy::Agg(l), place) => agg_payload_bytes(&payloads[i], l).map(|buf| match place {
                Place::Pieces(pieces) => {
                    for &(slot, off, len) in pieces {
                        set_slot(slot, bytes_to_word(&buf, off, len), &mut gpr, &mut fpr);
                    }
                }
                Place::StackCopy(w0) => {
                    for (k, word) in bytes_to_words(&buf).into_iter().enumerate() {
                        set_slot(Slot::Stack(w0 + k), word, &mut gpr, &mut fpr);
                    }
                }
                Place::Indirect(slot) => {
                    let copy = bytes_to_words(&buf);
                    let a = copy.as_ptr() as u64;
                    agg_copies.push(copy);
                    set_slot(*slot, a, &mut gpr, &mut fpr);
                }
                Place::Scalar(_) => unreachable!("aggregate placed as scalar"),
            }),
            _ => unreachable!("scalar placed as aggregate"),
        };
        if let Err(e) = res {
            for p in owned {
                crate::foreign::release_object_ptr(p);
            }
            return Err(e);
        }
    }

    let ret = unsafe {
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
        let r = native::raw_call(addr, &gpr, &fpr, &stack, nfpr, indirect);
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
    drop(agg_copies);
    for p in owned {
        crate::foreign::release_object_ptr(p);
    }
    // CPython `_call_function_pointer`: a PyDLL (`ctypes.pythonapi`) call
    // raises whatever exception the callee left pending, whatever it
    // returned. `py_object` results still go through `steal_object` so a
    // NULL with nothing pending keeps its ValueError.
    const FUNCFLAG_PYTHONAPI: i64 = 0x4;
    if (flags & FUNCFLAG_PYTHONAPI) != 0 && !(ret_is_object && ret.gpr[0] == 0) {
        if let Some(err) = crate::foreign::take_pending_error() {
            return Err(err);
        }
    }
    match ret_ty {
        RetTy::Scalar(_) if ret_is_object => crate::foreign::steal_object(ret.gpr[0] as usize),
        RetTy::Scalar(cls) => Ok(marshal_ret(cls, ret.gpr[0], ret.fpr[0])),
        RetTy::Agg(l) => {
            let mut out = vec![0u8; l.size];
            match ret_place(&l) {
                RetPlace::Indirect => {
                    let src: Vec<u8> = ret_buf.iter().flat_map(|w| w.to_ne_bytes()).collect();
                    out.copy_from_slice(&src[..l.size]);
                }
                RetPlace::Pieces(pieces) => {
                    for (slot, off, len) in pieces {
                        let word = match slot {
                            Slot::Gpr(r) => ret.gpr[r],
                            Slot::Fpr(r) => ret.fpr[r],
                            Slot::Stack(_) => 0,
                        };
                        let end = (off + len).min(l.size);
                        out[off..end].copy_from_slice(&word.to_ne_bytes()[..end - off]);
                    }
                }
            }
            Ok(Object::new_bytes(out))
        }
    }
}

/// The raw bytes of a by-value aggregate argument. The frozen `_ctypes.py`
/// hands the instance's buffer contents over as `bytes` (or a
/// `(bytes, descriptor)` pair for the variadic tail).
fn agg_payload_bytes(payload: &Object, l: &AggLayout) -> Result<Vec<u8>, RuntimeError> {
    let raw = match payload {
        Object::Tuple(_) | Object::List(_) => {
            let items = list_items(Some(payload))?;
            items.first().and_then(Object::as_bytes_view)
        }
        other => other.as_bytes_view(),
    };
    let mut buf = raw.ok_or_else(|| {
        type_error("call_function: by-value aggregate argument must carry its bytes")
    })?;
    buf.resize(l.size, 0);
    Ok(buf)
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
    arg_tys: Vec<ArgTy>,
    ret: Cls,
    /// `py_object` result: the Python return value is handed to the C
    /// caller as a new owned `PyObject*` (cfield.c `O_set`).
    ret_is_object: bool,
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

    // Closures (CFUNCTYPE) are never variadic: every arg is fixed, and a
    // callback result is always a scalar (CPython rejects aggregate
    // restypes for callbacks), so no hidden result pointer.
    let places = assign_places(&data.arg_tys, data.arg_tys.len(), false);
    let mut py_args: Vec<Object> = Vec::with_capacity(places.len());
    let read = |slot: Slot| unsafe {
        match slot {
            Slot::Gpr(r) => regs.gpr(r),
            Slot::Fpr(r) => regs.fpr(r),
            Slot::Stack(r) => regs.stack(r),
        }
    };
    for (i, (ty, &code)) in data.arg_tys.iter().zip(data.arg_codes.iter()).enumerate() {
        let cls = match ty {
            ArgTy::Scalar(c) => *c,
            ArgTy::Agg(l) => {
                // Rebuild the aggregate's bytes from wherever the caller
                // put them; the frozen `_ctypes.py` materialises the
                // instance from them.
                let mut buf = vec![0u8; l.size];
                match &places[i] {
                    Place::Pieces(pieces) => {
                        for &(slot, off, len) in pieces {
                            let end = (off + len).min(l.size);
                            buf[off..end].copy_from_slice(&read(slot).to_ne_bytes()[..end - off]);
                        }
                    }
                    Place::StackCopy(w0) => {
                        for k in 0..l.words() {
                            let end = ((k + 1) * 8).min(l.size);
                            if k * 8 < end {
                                buf[k * 8..end].copy_from_slice(
                                    &read(Slot::Stack(w0 + k)).to_ne_bytes()[..end - k * 8],
                                );
                            }
                        }
                    }
                    Place::Indirect(slot) => {
                        let p = read(*slot) as usize;
                        if p != 0 {
                            let src = unsafe { std::slice::from_raw_parts(p as *const u8, l.size) };
                            buf.copy_from_slice(src);
                        }
                    }
                    Place::Scalar(_) => {}
                }
                py_args.push(Object::new_bytes(buf));
                continue;
            }
        };
        let bits = match &places[i] {
            Place::Scalar(slot) => read(*slot),
            _ => 0,
        };
        if code == 'O' {
            // A `py_object` argument arrives as a borrowed `PyObject*`.
            match crate::foreign::borrow_object(bits as usize) {
                Ok(o) => py_args.push(o),
                Err(e) => {
                    eprintln!("Exception ignored on converting ctypes callback argument: {e}");
                    py_args.push(Object::None);
                }
            }
            continue;
        }
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
    if data.ret_is_object {
        let ptr = crate::foreign::object_to_owned_ptr(&value).unwrap_or(0);
        unsafe { *regs.ret_gpr = ptr as u64 };
        return;
    }
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
    let ret = match return_ty(args.get(1), "create_closure")? {
        RetTy::Scalar(c) => c,
        RetTy::Agg(_) => {
            return Err(type_error(
                "create_closure: aggregate result types are not supported for callbacks",
            ))
        }
    };
    let ret_is_object = matches!(args.get(1), Some(Object::Str(s)) if s.as_ref() == "O");
    let code_objs = list_items(args.get(2))?;
    let mut codes = Vec::with_capacity(code_objs.len());
    let mut tys = Vec::with_capacity(code_objs.len());
    for o in &code_objs {
        tys.push(parse_arg_ty(o, "create_closure")?);
        codes.push(match o {
            Object::Str(s) => s.chars().next().unwrap_or('S'),
            _ => 'S',
        });
    }

    let data = Box::into_raw(Box::new(ClosureData {
        callable,
        arg_codes: codes,
        arg_tys: tys,
        ret,
        ret_is_object,
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

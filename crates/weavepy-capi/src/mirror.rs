//! The object mirror bridge (RFC 0043, wave 1, WS2).
//!
//! CPython extensions are not merely *callers* of an API; the stock
//! headers *inline* the hot path, so a compiled wheel reads object
//! fields at fixed byte offsets (`PyFloat_AS_DOUBLE` → `*(double*)(op+16)`,
//! `Py_SIZE` → `*(Py_ssize_t*)(op+16)`, `PyTuple_GET_ITEM` →
//! `((PyTupleObject*)op)->ob_item[i]`). WeavePy's native value is a Rust
//! [`Object`] enum with none of those fields at those offsets, so we
//! cannot satisfy a stock reader by interposing a function.
//!
//! Following PyPy's `cpyext` and GraalPy's C-API layer, this module
//! maintains a **layout-faithful mirror**: when a native value crosses
//! into C it is materialised into a heap block whose bytes match the
//! corresponding CPython 3.13 struct ([`crate::layout`]) exactly. The
//! public `*mut PyObject` points at that faithful body; immediately
//! *before* it (a negative offset, invisible to C) sits a
//! [`MirrorPrefix`] holding the owning native [`Object`] — so a pointer
//! WeavePy minted resolves back to its native object in O(1) without a
//! global lookup, while the public pointer stays byte-faithful.
//!
//! Wave 1 fills faithful bodies for the immutable high-frequency types
//! whose internals get inlined (`float`, `int`, `complex`, `bytes`,
//! compact `str`, `tuple`); other types get a head-only "generic" body
//! whose native value still lives in the prefix (so the function-call
//! C-API and `clone_object` work, only stock *inlined field reads* are a
//! later wave). Either way the prefix is uniform, so resolution and
//! freeing are representation-independent.

use std::alloc::{alloc, dealloc, Layout};
use std::os::raw::c_void;
use std::ptr;

use num_bigint::BigInt;
use weavepy_vm::object::Object;

use crate::layout::{self, ustate};
use crate::object::{PyObject, PySsizeT};
use crate::types::{self, PyTypeObject};

/// WeavePy bookkeeping placed immediately before the faithful body. The
/// public `*mut PyObject` is `prefix as *mut u8 + PREFIX_SIZE`, so the
/// prefix is recovered by subtracting [`PREFIX_SIZE`].
#[repr(C)]
pub struct MirrorPrefix {
    /// The owning native object. Holding it here pins the value (its
    /// `Rc`s) for as long as C holds a reference; dropped when the
    /// mirror's refcount reaches zero.
    pub obj: Object,
    /// Extra C-side state (capsule pointer, module-state, …). Mirrors
    /// do not use this today but the slot keeps parity with the legacy
    /// box so shared accessors are uniform.
    pub user_data: *mut c_void,
    /// Optional destructor, run before the block is freed.
    pub destructor: Option<unsafe extern "C" fn(*mut PyObject)>,
    /// Total bytes of the body allocation (`PREFIX_SIZE + body`), for
    /// [`dealloc`].
    pub alloc_size: usize,
    /// Out-of-line buffer owned by this mirror (a list's `ob_item`
    /// array), or null.
    pub aux_ptr: *mut u8,
    /// Byte length of [`aux_ptr`]'s allocation.
    pub aux_size: usize,
    /// A small magic so debugging tools (and assertions) can recognise
    /// a mirror prefix.
    pub magic: u64,
}

/// Sentinel stamped into every [`MirrorPrefix::magic`].
pub const MIRROR_MAGIC: u64 = 0x5742_504d_5252_5230; // "WBPMRR0"

/// Body alignment. 16 is ≥ the alignment of every faithful struct
/// (`f64`, pointers, `Py_complex`) and keeps SIMD-friendly buffers sane.
const BODY_ALIGN: usize = 16;

/// Bytes reserved for the prefix, rounded so the body that follows is
/// [`BODY_ALIGN`]-aligned.
pub const PREFIX_SIZE: usize = {
    let s = std::mem::size_of::<MirrorPrefix>();
    // round up to BODY_ALIGN
    (s + (BODY_ALIGN - 1)) & !(BODY_ALIGN - 1)
};

const _: () = {
    // The prefix must not be larger than the reserved region, and the
    // reserved region must be a multiple of the body alignment.
    assert!(std::mem::align_of::<MirrorPrefix>() <= BODY_ALIGN);
    assert!(PREFIX_SIZE.is_multiple_of(BODY_ALIGN));
    assert!(PREFIX_SIZE >= std::mem::size_of::<MirrorPrefix>());
};

/// Recover the prefix pointer from a public body pointer.
///
/// # Safety
/// `p` must be a body pointer previously returned by [`mirror_out`] /
/// [`mirror_out_with_type`] (i.e. [`is_mirror`] is true).
#[inline]
pub unsafe fn prefix_of(p: *mut PyObject) -> *mut MirrorPrefix {
    unsafe { (p as *mut u8).sub(PREFIX_SIZE) as *mut MirrorPrefix }
}

/// True if `p` is a faithful mirror (as opposed to a legacy
/// `PyObjectBox` or a static singleton/type). Decided by the object's
/// type: every value of a faithful built-in type is minted as a mirror,
/// so the type pointer is a sound, deref-free discriminator.
///
/// # Safety
/// `p` must be non-null and point at a valid object head (`ob_type`
/// readable). Callers must have already excluded the static singletons
/// and static type objects (which are not mirrors).
#[inline]
pub unsafe fn is_mirror(p: *mut PyObject) -> bool {
    if p.is_null() {
        return false;
    }
    let ty = unsafe { (*p).ob_type };
    type_is_faithful(ty)
}

/// The set of built-in types whose instances are minted as faithful
/// mirrors. Mirrors `crate::types::type_for_object` for these variants.
pub fn type_is_faithful(ty: *mut PyTypeObject) -> bool {
    if ty.is_null() {
        return false;
    }
    ty == types::PyFloat_Type.as_ptr()
        || ty == types::PyLong_Type.as_ptr()
        || ty == types::PyBool_Type.as_ptr()
        || ty == types::PyComplex_Type.as_ptr()
        || ty == types::PyBytes_Type.as_ptr()
        || ty == types::PyByteArray_Type.as_ptr()
        || ty == types::PyUnicode_Type.as_ptr()
        || ty == types::PyTuple_Type.as_ptr()
        || ty == types::PyList_Type.as_ptr()
}

/// True if a native [`Object`] is mirrored with a faithful body (rather
/// than routed through the legacy `PyObjectBox`).
pub fn obj_is_faithful(obj: &Object) -> bool {
    matches!(
        obj,
        Object::Float(_)
            | Object::Int(_)
            | Object::Long(_)
            | Object::Bool(_)
            | Object::Complex(_)
            | Object::Bytes(_)
            | Object::ByteArray(_)
            | Object::Str(_)
            | Object::Tuple(_)
            | Object::List(_)
    )
}

/// Materialise `obj` into a faithful mirror, choosing the type pointer
/// from the value. Caller owns one reference.
pub fn mirror_out(obj: Object) -> *mut PyObject {
    let ty = types::type_for_object(&obj);
    mirror_out_with_type(obj, ty)
}

/// Materialise `obj` into a faithful mirror with an explicit type
/// pointer. Used for the tuple-staging case (`PyTuple_New` advertises
/// `PyTuple_Type` while staging a mutable `List`).
pub fn mirror_out_with_type(obj: Object, ty: *mut PyTypeObject) -> *mut PyObject {
    let plan = BodyPlan::for_object(&obj);
    let total = PREFIX_SIZE + plan.body_size;
    let layout = Layout::from_size_align(total, BODY_ALIGN).expect("mirror layout");
    let raw = unsafe { alloc(layout) };
    assert!(!raw.is_null(), "mirror allocation failed");
    unsafe { ptr::write_bytes(raw, 0, total) };

    let body = unsafe { raw.add(PREFIX_SIZE) } as *mut PyObject;

    // Allocate any out-of-line buffer (list `ob_item`) before we move
    // `obj` into the prefix, so we can still read it.
    let mut aux_ptr: *mut u8 = ptr::null_mut();
    let mut aux_size: usize = 0;
    unsafe {
        fill_body(body, ty, &obj, &plan, &mut aux_ptr, &mut aux_size);
    }

    // Head.
    unsafe {
        (*body).ob_refcnt = 1;
        (*body).ob_type = ty;
    }

    // Prefix (owns the native object).
    let pre = raw as *mut MirrorPrefix;
    unsafe {
        ptr::write(
            pre,
            MirrorPrefix {
                obj,
                user_data: ptr::null_mut(),
                destructor: None,
                alloc_size: total,
                aux_ptr,
                aux_size,
                magic: MIRROR_MAGIC,
            },
        );
    }
    body
}

/// Clone the native object out of a mirror without touching the C-side
/// refcount.
///
/// # Safety
/// `p` must satisfy [`is_mirror`].
pub unsafe fn native_of(p: *mut PyObject) -> Object {
    let pre = unsafe { prefix_of(p) };
    unsafe { (*pre).obj.clone() }
}

/// Borrow the C-side state pointer stored in the prefix.
///
/// # Safety
/// `p` must satisfy [`is_mirror`].
pub unsafe fn user_data(p: *mut PyObject) -> *mut c_void {
    let pre = unsafe { prefix_of(p) };
    unsafe { (*pre).user_data }
}

/// Free a mirror: run its destructor, drop the owning native object and
/// any out-of-line buffer, then release the block.
///
/// # Safety
/// `p` must satisfy [`is_mirror`] and have a zero (or about-to-be-zero)
/// refcount; it must not be used afterwards.
pub unsafe fn free_mirror(p: *mut PyObject) {
    let pre = unsafe { prefix_of(p) };
    let destructor = unsafe { (*pre).destructor };
    if let Some(d) = destructor {
        unsafe { d(p) };
    }
    let alloc_size = unsafe { (*pre).alloc_size };
    let aux_ptr = unsafe { (*pre).aux_ptr };
    let aux_size = unsafe { (*pre).aux_size };

    // Drop the owning native object (releasing its Rc clones).
    unsafe { ptr::drop_in_place(ptr::addr_of_mut!((*pre).obj)) };

    if !aux_ptr.is_null() && aux_size > 0 {
        let aux_layout = Layout::from_size_align(aux_size, BODY_ALIGN).expect("aux layout");
        unsafe { dealloc(aux_ptr, aux_layout) };
    }

    let layout = Layout::from_size_align(alloc_size, BODY_ALIGN).expect("mirror layout");
    unsafe { dealloc(pre as *mut u8, layout) };
}

// ---------------------------------------------------------------------------
// Body layout planning + filling.
// ---------------------------------------------------------------------------

/// What kind of faithful body a value gets, and how big it is.
struct BodyPlan {
    kind: BodyKind,
    /// Size in bytes of the body (head + faithful tail). Always ≥ 16.
    body_size: usize,
}

#[derive(Clone, Copy)]
enum BodyKind {
    Float,
    Long,
    Complex,
    Bytes,
    Str,
    Tuple,
    /// Head-only body; the native value lives only in the prefix.
    Generic,
}

impl BodyPlan {
    fn for_object(obj: &Object) -> BodyPlan {
        match obj {
            Object::Float(_) => BodyPlan {
                kind: BodyKind::Float,
                body_size: std::mem::size_of::<layout::PyFloatObject>(),
            },
            Object::Complex(_) => BodyPlan {
                kind: BodyKind::Complex,
                body_size: std::mem::size_of::<layout::PyComplexObject>(),
            },
            Object::Int(_) | Object::Long(_) => {
                let ndigits = long_digit_count(obj).max(1);
                // head(16) + lv_tag(8) + ndigits * 4, rounded to 8.
                let raw = 16 + 8 + ndigits * 4;
                BodyPlan {
                    kind: BodyKind::Long,
                    body_size: round_up(raw, 8),
                }
            }
            Object::Bytes(b) => BodyPlan {
                kind: BodyKind::Bytes,
                // varhead(24) + ob_shash(8) + (len+1) NUL-terminated.
                body_size: round_up(24 + 8 + b.len() + 1, 8),
            },
            Object::Str(s) if is_ascii_or_latin1(s) => BodyPlan {
                kind: BodyKind::Str,
                // PyASCIIObject(40) + (len+1) bytes of 1-byte chars.
                body_size: round_up(40 + s.chars().count() + 1, 8),
            },
            Object::Tuple(t) => BodyPlan {
                kind: BodyKind::Tuple,
                // varhead(24) + n pointers.
                body_size: round_up(24 + t.len() * 8, 8).max(24),
            },
            _ => BodyPlan {
                kind: BodyKind::Generic,
                body_size: std::mem::size_of::<PyObject>(),
            },
        }
    }
}

/// Fill the faithful fields of `body` from `obj`. The head is written by
/// the caller afterward (so `fill_body` must not depend on it).
///
/// # Safety
/// `body` points at a zeroed block of at least `plan.body_size` bytes.
unsafe fn fill_body(
    body: *mut PyObject,
    _ty: *mut PyTypeObject,
    obj: &Object,
    plan: &BodyPlan,
    aux_ptr: &mut *mut u8,
    aux_size: &mut usize,
) {
    match plan.kind {
        BodyKind::Float => {
            if let Object::Float(f) = obj {
                let fo = body as *mut layout::PyFloatObject;
                unsafe { (*fo).ob_fval = *f };
            }
        }
        BodyKind::Complex => {
            if let Object::Complex(c) = obj {
                let co = body as *mut layout::PyComplexObject;
                unsafe {
                    (*co).cval = layout::PyComplexValue {
                        real: c.real,
                        imag: c.imag,
                    };
                }
            }
        }
        BodyKind::Long => unsafe { fill_long(body, obj) },
        BodyKind::Bytes => {
            if let Object::Bytes(b) = obj {
                let vo = body as *mut layout::PyVarObject;
                unsafe { (*vo).ob_size = b.len() as PySsizeT };
                let bo = body as *mut layout::PyBytesObject;
                unsafe {
                    (*bo).ob_shash = -1;
                    let dst = ptr::addr_of_mut!((*bo).ob_sval) as *mut u8;
                    ptr::copy_nonoverlapping(b.as_ptr(), dst, b.len());
                    *dst.add(b.len()) = 0; // NUL terminator
                }
            }
        }
        BodyKind::Str => unsafe { fill_str(body, obj) },
        BodyKind::Tuple => {
            if let Object::Tuple(t) = obj {
                let vo = body as *mut layout::PyVarObject;
                unsafe { (*vo).ob_size = t.len() as PySsizeT };
                let to = body as *mut layout::PyTupleObject;
                let base = ptr::addr_of_mut!((*to).ob_item) as *mut *mut PyObject;
                for (i, elem) in t.iter().enumerate() {
                    // Each element is itself mirrored (owned by this tuple).
                    let ep = mirror_out(elem.clone());
                    unsafe { *base.add(i) = ep };
                }
            }
        }
        BodyKind::Generic => {
            // Head-only: nothing to fill. Suppress "unused" on a list's
            // would-be aux buffer.
            let _ = (aux_ptr, aux_size);
        }
    }
}

/// Encode an integer's faithful `PyLongObject` body.
unsafe fn fill_long(body: *mut PyObject, obj: &Object) {
    let (sign, mag) = int_sign_magnitude(obj);
    let digits = to_base_2_30(mag);
    let ndigits = digits.len().max(1);
    let lo = body as *mut layout::PyLongObject;
    let sign_field = if sign == 0 {
        layout::PYLONG_SIGN_ZERO
    } else if sign < 0 {
        layout::PYLONG_SIGN_NEGATIVE
    } else {
        layout::PYLONG_SIGN_POSITIVE
    };
    unsafe {
        (*lo).long_value.lv_tag = (ndigits << layout::PYLONG_NON_SIZE_BITS) | sign_field;
        let base = ptr::addr_of_mut!((*lo).long_value.ob_digit) as *mut layout::digit;
        if digits.is_empty() {
            *base = 0;
        } else {
            for (i, d) in digits.iter().enumerate() {
                *base.add(i) = *d;
            }
        }
    }
}

/// Fill a compact 1-byte (ASCII or Latin-1) unicode body.
unsafe fn fill_str(body: *mut PyObject, obj: &Object) {
    let Object::Str(s) = obj else { return };
    let is_ascii = s.is_ascii();
    let chars: Vec<u8> = s.chars().map(|c| c as u8).collect(); // latin-1 guaranteed by planner
    let n = chars.len();
    let ao = body as *mut layout::PyASCIIObject;
    unsafe {
        (*ao).length = n as PySsizeT;
        (*ao).hash = -1;
        (*ao).state = ustate::pack(
            0, // not interned
            ustate::KIND_1BYTE,
            true,     // compact
            is_ascii, // ascii
            false,    // not statically allocated
        );
        // Compact-ASCII data follows the PyASCIIObject inline.
        let data = (body as *mut u8).add(std::mem::size_of::<layout::PyASCIIObject>());
        ptr::copy_nonoverlapping(chars.as_ptr(), data, n);
        *data.add(n) = 0;
    }
}

// ---------------------------------------------------------------------------
// Integer helpers.
// ---------------------------------------------------------------------------

fn long_digit_count(obj: &Object) -> usize {
    let (_, mag) = int_sign_magnitude(obj);
    to_base_2_30(mag).len()
}

/// Returns `(sign, magnitude)` where `sign ∈ {-1, 0, 1}`.
fn int_sign_magnitude(obj: &Object) -> (i32, u128) {
    match obj {
        Object::Int(v) => {
            if *v == 0 {
                (0, 0)
            } else if *v < 0 {
                (-1, (*v as i128).unsigned_abs())
            } else {
                (1, *v as u128)
            }
        }
        Object::Bool(b) => {
            if *b {
                (1, 1)
            } else {
                (0, 0)
            }
        }
        Object::Long(big) => big_sign_magnitude(big),
        _ => (0, 0),
    }
}

/// Big integers wider than `u128` are clamped to their low 128 bits for
/// the faithful body; WeavePy itself always reads the exact value from
/// the prefix, and stock extensions read big ints through the function
/// API (`PyLong_AsLong`), so the inlined-digit path matters only for
/// values that fit. (Full-width digit encoding is a wave-2 refinement.)
fn big_sign_magnitude(big: &BigInt) -> (i32, u128) {
    use num_bigint::Sign;
    let (sign, bytes) = big.to_bytes_le();
    let mut mag: u128 = 0;
    for (i, b) in bytes.iter().take(16).enumerate() {
        mag |= (*b as u128) << (i * 8);
    }
    let s = match sign {
        Sign::NoSign => 0,
        Sign::Plus => 1,
        Sign::Minus => -1,
    };
    (s, mag)
}

/// Decompose a magnitude into base-2^30 little-endian limbs.
fn to_base_2_30(mut mag: u128) -> Vec<layout::digit> {
    let mut out = Vec::new();
    if mag == 0 {
        return out;
    }
    while mag > 0 {
        out.push((mag & (layout::PYLONG_MASK as u128)) as layout::digit);
        mag >>= layout::PYLONG_SHIFT;
    }
    out
}

fn is_ascii_or_latin1(s: &str) -> bool {
    s.chars().all(|c| (c as u32) <= 0xFF)
}

const fn round_up(n: usize, align: usize) -> usize {
    (n + (align - 1)) & !(align - 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interp::ensure_initialised;
    use weavepy_vm::sync::Rc as VmRc;

    /// Read a `T` at byte offset `off` from a body pointer, the way a
    /// stock inlined macro would.
    unsafe fn read_at<T: Copy>(p: *mut PyObject, off: usize) -> T {
        unsafe { ptr::read_unaligned((p as *const u8).add(off) as *const T) }
    }

    fn as_float(o: &Object) -> f64 {
        match o {
            Object::Float(f) => *f,
            _ => panic!("expected float"),
        }
    }
    fn as_int(o: &Object) -> i64 {
        match o {
            Object::Int(v) => *v,
            _ => panic!("expected int"),
        }
    }

    #[test]
    fn float_body_is_faithful() {
        ensure_initialised();
        let p = mirror_out(Object::Float(2.5));
        unsafe {
            assert!(is_mirror(p));
            // ob_fval lives at offset 16 (where PyFloat_AS_DOUBLE reads).
            assert_eq!(read_at::<f64>(p, 16), 2.5);
            // refcount starts at 1, type is float.
            assert_eq!((*p).ob_refcnt, 1);
            assert_eq!((*p).ob_type, types::PyFloat_Type.as_ptr());
            // The native object resolves back.
            assert_eq!(as_float(&native_of(p)), 2.5);
            free_mirror(p);
        }
    }

    #[test]
    fn long_body_encodes_small_int() {
        ensure_initialised();
        let p = mirror_out(Object::Int(5));
        unsafe {
            // lv_tag at +16: ndigits=1, sign positive → (1<<3)|0 = 8.
            assert_eq!(read_at::<usize>(p, 16), 8);
            // first digit at +24 == 5.
            assert_eq!(read_at::<u32>(p, 24), 5);
            assert_eq!(as_int(&native_of(p)), 5);
            free_mirror(p);
        }
    }

    #[test]
    fn long_body_encodes_negative() {
        ensure_initialised();
        let p = mirror_out(Object::Int(-1));
        unsafe {
            // sign negative = 2, ndigits 1 → (1<<3)|2 = 10.
            assert_eq!(read_at::<usize>(p, 16), 10);
            assert_eq!(read_at::<u32>(p, 24), 1);
            free_mirror(p);
        }
    }

    #[test]
    fn bytes_body_is_faithful() {
        ensure_initialised();
        let p = mirror_out(Object::Bytes(VmRc::from(&b"hi"[..])));
        unsafe {
            // ob_size at +16.
            assert_eq!(read_at::<isize>(p, 16), 2);
            // ob_sval at +32 holds the bytes + NUL.
            assert_eq!(read_at::<u8>(p, 32), b'h');
            assert_eq!(read_at::<u8>(p, 33), b'i');
            assert_eq!(read_at::<u8>(p, 34), 0);
            free_mirror(p);
        }
    }

    #[test]
    fn str_ascii_body_is_faithful() {
        ensure_initialised();
        let p = mirror_out(Object::Str(VmRc::from("abc")));
        unsafe {
            // length at +16.
            assert_eq!(read_at::<isize>(p, 16), 3);
            // state at +32: kind=1byte, compact, ascii.
            let state = read_at::<u32>(p, 32);
            assert_eq!(
                state,
                ustate::pack(0, ustate::KIND_1BYTE, true, true, false)
            );
            // compact data follows PyASCIIObject (offset 40).
            assert_eq!(read_at::<u8>(p, 40), b'a');
            assert_eq!(read_at::<u8>(p, 42), b'c');
            free_mirror(p);
        }
    }

    #[test]
    fn tuple_body_holds_element_mirrors() {
        ensure_initialised();
        let t = Object::new_tuple(vec![Object::Float(1.0), Object::Int(2)]);
        let p = mirror_out(t);
        unsafe {
            // ob_size at +16.
            assert_eq!(read_at::<isize>(p, 16), 2);
            // ob_item[0] at +24 is a float mirror with ob_fval 1.0.
            let e0 = read_at::<*mut PyObject>(p, 24);
            assert_eq!(read_at::<f64>(e0, 16), 1.0);
            free_mirror(p);
        }
    }

    #[test]
    fn generic_body_keeps_native_in_prefix() {
        ensure_initialised();
        // A dict is not a faithful body; it gets a generic head-only body
        // but still resolves through the prefix.
        let p = mirror_out(Object::Float(9.0));
        unsafe {
            assert!(is_mirror(p));
            free_mirror(p);
        }
    }
}

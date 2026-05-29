//! The native-call ABI: the `#[repr(C)]` [`JitFrame`] the VM fills
//! before entering compiled code and reads after it exits, plus the
//! side-exit status protocol.
//!
//! A compiled frame is a single native function with the signature
//!
//! ```text
//! extern "C" fn(frame: *mut JitFrame) -> i64   // an i64 JitStatus
//! ```
//!
//! On a [`JitStatus::Returned`] exit the function has written
//! [`JitFrame::ret_bits`] / [`JitFrame::ret_tag`]. On a
//! [`JitStatus::Deopt`] exit it has written [`JitFrame::deopt_pc`] and
//! spilled the live abstract operand stack into
//! [`JitFrame::stack_spill`] / [`JitFrame::stack_tags`] (bottom-to-top)
//! with [`JitFrame::stack_len`] entries, plus written back every
//! JIT-managed local into [`JitFrame::locals`]. The VM then rebuilds its
//! interpreter state and resumes at `deopt_pc`, bit-for-bit as though
//! the JIT had never run.

/// The status returned (as an `i64`) by a compiled frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(i64)]
pub enum JitStatus {
    /// The frame ran to a `RETURN_VALUE`. The return value is in
    /// [`JitFrame::ret_bits`] / [`JitFrame::ret_tag`].
    Returned = 0,
    /// The frame took a side exit. The VM resumes interpretation at
    /// [`JitFrame::deopt_pc`] with the spilled stack + written-back
    /// locals.
    Deopt = 1,
}

impl JitStatus {
    /// Decode the raw `i64` a compiled frame returns.
    #[inline]
    #[must_use]
    pub fn from_raw(v: i64) -> JitStatus {
        match v {
            0 => JitStatus::Returned,
            _ => JitStatus::Deopt,
        }
    }
}

/// How to interpret a `u64` slot in [`JitFrame::locals`] /
/// [`JitFrame::stack_spill`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum SlotTag {
    /// `i64` bit pattern → `Object::Int`.
    Int = 0,
    /// `f64` bit pattern (via `to_bits`) → `Object::Float`.
    Float = 1,
    /// `0`/`1` → `Object::Bool`.
    Bool = 2,
}

impl SlotTag {
    /// Decode a raw tag written by native code.
    #[inline]
    #[must_use]
    pub fn from_raw(v: u32) -> SlotTag {
        match v {
            1 => SlotTag::Float,
            2 => SlotTag::Bool,
            _ => SlotTag::Int,
        }
    }
}

/// The exchange buffer the VM passes to a compiled frame.
///
/// The VM owns the backing storage (`Vec<u64>` / `Vec<u32>`); this
/// struct holds raw pointers to it for the duration of one native call.
/// All indices the native code touches are bounded by `n_locals` /
/// `stack_cap`, which the VM sizes from the compiled frame's analysis.
#[repr(C)]
#[derive(Debug)]
pub struct JitFrame {
    /// Slot-indexed local storage, one `u64` per code-object local.
    /// Holds `i64` / `f64`-bits / `bool` per the local's stable type.
    pub locals: *mut u64,
    /// Number of valid entries in [`Self::locals`].
    pub n_locals: u32,
    /// OSR entry: the bytecode pc to begin execution at. `0` enters at
    /// the function start; a loop-header pc enters mid-frame.
    pub entry_pc: u32,

    /// `Returned`: the return value's bit pattern.
    pub ret_bits: u64,
    /// `Returned`: the return value's [`SlotTag`].
    pub ret_tag: u32,

    /// `Deopt`: the bytecode pc to resume interpretation at.
    pub deopt_pc: u32,
    /// `Deopt`: spilled abstract operand stack, bottom-to-top.
    pub stack_spill: *mut u64,
    /// `Deopt`: matching [`SlotTag`]s for [`Self::stack_spill`].
    pub stack_tags: *mut u32,
    /// `Deopt`: number of spilled stack entries.
    pub stack_len: u32,
    /// Capacity of [`Self::stack_spill`] / [`Self::stack_tags`].
    pub stack_cap: u32,
}

impl JitFrame {
    /// Reinterpret an `f64` as the `u64` stored in a slot.
    #[inline]
    #[must_use]
    pub fn f64_to_bits(v: f64) -> u64 {
        v.to_bits()
    }

    /// Reinterpret a slot's `u64` as the `f64` it encodes.
    #[inline]
    #[must_use]
    pub fn bits_to_f64(bits: u64) -> f64 {
        f64::from_bits(bits)
    }
}

//! The unboxed value model and type lattice the JIT reasons about.
//!
//! Only three concrete Python types are representable as unboxed machine
//! values: `int` (as `i64`), `float` (as `f64`), and `bool` (as a
//! one-byte `0`/`1`). Everything else is [`JitType::Unknown`], which
//! makes any region that would need it non-JITable.
//!
//! A deliberate restriction keeps deopt simple (see `analyze`): within a
//! single compiled region, each local slot and each abstract-stack
//! position has **one** stable [`JitType`]. Straight-line retyping of a
//! local (`x = 1; x = 2.0`) is rejected as non-JITable rather than
//! tracked per-pc.

/// The abstract type of an unboxed value flowing through the JIT.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum JitType {
    /// CPython `int` that fits in `i64`. Overflow deopts to the
    /// interpreter, which promotes to a bignum.
    Int,
    /// CPython `float` (`f64`).
    Float,
    /// CPython `bool`. Distinct from `Int` so the VM rebuilds the right
    /// `Object` variant on deopt; arithmetic promotes it to `Int` first.
    Bool,
    /// Anything the JIT can't represent. Its presence as an operand to a
    /// supported opcode makes the enclosing region non-JITable.
    Unknown,
}

impl JitType {
    /// `true` for the three representable types.
    #[inline]
    #[must_use]
    pub fn is_representable(self) -> bool {
        !matches!(self, JitType::Unknown)
    }

    /// `true` if this is an integral lane (`Int` or `Bool`), which share
    /// the `i64` machine representation.
    #[inline]
    #[must_use]
    pub fn is_integral(self) -> bool {
        matches!(self, JitType::Int | JitType::Bool)
    }

    /// Dataflow join at a control-flow merge. Two equal types join to
    /// themselves; everything else collapses to [`JitType::Unknown`].
    /// `Bool`/`Int` are kept distinct (they join to `Unknown`) so a slot
    /// that is sometimes a bool and sometimes an int is treated as
    /// non-uniform and the region bails — conservative but always sound.
    #[inline]
    #[must_use]
    pub fn join(self, other: JitType) -> JitType {
        if self == other {
            self
        } else {
            JitType::Unknown
        }
    }
}

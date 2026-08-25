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
    /// RFC 0061 WS5 — a *pinned* `list` of `int` elements. The machine
    /// value is an `i64` index into the embedder's per-entry pinned-
    /// object table (no pointer ever crosses the JIT boundary); element
    /// access goes through the registered `wpjit_list_get`/`_set`
    /// helpers, which re-validate shape per access and deopt on any
    /// surprise.
    ListInt,
    /// RFC 0061 WS5 — a pinned `list` of `float` elements.
    ListFloat,
    /// RFC 0071 WS4 — a pinned `list` of *object* elements (instances
    /// and `None`). Element reads ride the nullable object lane
    /// (loaded elements pin at runtime, `None` as `-1`); the helpers
    /// re-validate the element shape per access and deopt on any
    /// surprise, exactly as the scalar list lanes do.
    ListObj,
    /// RFC 0065 WS5 — a *pinned* instance receiver. Like the list pins,
    /// the machine value is an `i64` index into the embedder's
    /// per-entry pinned-object table; attribute access goes through the
    /// registered `wpjit_attr_get`/`_set` helpers, which re-validate
    /// the receiver's shape (type identity + attr-version, instance-
    /// dict hit, expected value lane) per access and deopt on any
    /// surprise.
    ///
    /// RFC 0070 WS1 — the lane is *nullable*: the machine value `-1`
    /// stands for the Python `None` singleton (there is no pin-table
    /// entry for it). `IsNone` fences test it natively; every helper
    /// treats a `-1` pin as a table miss and deopts, so the interpreter
    /// re-executes the access on the real `None` and raises exactly.
    Obj,
    /// RFC 0071 WS6 — a *pinned* exact `str` (subclasses stay
    /// `Unknown`). The machine value is a pin-table index; the read
    /// helpers (`wpjit_str_eq`, `wpjit_str_len`) re-validate the pin
    /// per access. Never nullable — `None` in a `Str` slot is a lane
    /// conflict, not a `-1`.
    Str,
    /// RFC 0071 WS6 — a *pinned* exact `bytes` (subclasses stay
    /// `Unknown`); same discipline as [`JitType::Str`]. `BytesGetItem`
    /// reads bytes as `Int`s through the registered helper.
    Bytes,
    /// RFC 0073 WS2 — a *pinned* exact `dict` (subclasses stay
    /// `Unknown`). Unlike the list lanes the key/value lanes are not
    /// part of the type: they are burned *per access site* into the
    /// dict ops (`DictGet`/`DictSet`/`DictContains`) from the
    /// embedder's shape probe, and every helper re-validates the
    /// runtime key/value lanes per access and deopts on any surprise —
    /// the same discipline as the per-site `AttrGet` lanes.
    Dict,
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

    /// `true` for a pinned-list lane (RFC 0061 WS5 / RFC 0071 WS4).
    #[inline]
    #[must_use]
    pub fn is_list(self) -> bool {
        matches!(
            self,
            JitType::ListInt | JitType::ListFloat | JitType::ListObj
        )
    }

    /// `true` for any *pinned* lane (RFC 0061/0065 WS5): the machine
    /// value is a pin-table index, meaningless outside its own
    /// activation — it cannot be marshaled as a call argument or
    /// returned across a frame boundary (RFC 0071 WS1 exempts the
    /// nullable `Obj` lane, whose marshaling resolves the pin to the
    /// real object at the boundary).
    #[inline]
    #[must_use]
    pub fn is_pinned(self) -> bool {
        matches!(
            self,
            JitType::ListInt
                | JitType::ListFloat
                | JitType::ListObj
                | JitType::Obj
                | JitType::Str
                | JitType::Bytes
                | JitType::Dict
        )
    }

    /// A pinned list's element lane, or `None` for non-list lanes.
    #[inline]
    #[must_use]
    pub fn elem_lane(self) -> Option<JitType> {
        match self {
            JitType::ListInt => Some(JitType::Int),
            JitType::ListFloat => Some(JitType::Float),
            JitType::ListObj => Some(JitType::Obj),
            _ => None,
        }
    }

    /// The pinned-list lane for an element lane (inverse of
    /// [`Self::elem_lane`]).
    #[inline]
    #[must_use]
    pub fn list_of(elem: JitType) -> Option<JitType> {
        match elem {
            JitType::Int => Some(JitType::ListInt),
            JitType::Float => Some(JitType::ListFloat),
            JitType::Obj => Some(JitType::ListObj),
            _ => None,
        }
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

//! The typed mid-IR the analyzer emits and the lowerer consumes.
//!
//! It is a *stack machine* mirroring the bytecode, but with every
//! operation resolved to a concrete [`JitType`] lane and every local
//! resolved to a slot index. Keeping a tiny IR between bytecode and
//! Cranelift means [`crate::analyze`] can be unit-tested without a
//! codegen backend and [`crate::lower`] stays a straight syntax-directed
//! translation.
//!
//! Cross-block operand-stack values are carried as Cranelift *block
//! parameters* in lowering; [`TBlock::entry_stack`] records their static
//! types so the lowerer can declare the right params. Locals become
//! Cranelift *variables*, so merges are handled by the SSA builder
//! without explicit phis.

use crate::value::JitType;

/// Index of a [`TBlock`] within a [`TFunc`].
pub type BlockId = usize;

/// Arithmetic operations the JIT lowers. `TrueDiv` (`/`) always yields a
/// `float`; `FloorDiv`/`Mod` carry Python's round-toward-negative-
/// infinity semantics on integers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ArithKind {
    Add,
    Sub,
    Mul,
    FloorDiv,
    Mod,
    TrueDiv,
    And,
    Or,
    Xor,
}

/// Comparison operators (six-way), matching `CompareKind`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CmpKind {
    Lt,
    Le,
    Eq,
    Ne,
    Gt,
    Ge,
}

/// A single stack-machine operation. Operands are implicit (the top of
/// the abstract value stack); results are pushed.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TOp {
    /// Push an `int` constant.
    PushConstInt(i64),
    /// Push a `float` constant (stored as `f64::to_bits` so `TOp` stays
    /// `Copy` + `PartialEq`).
    PushConstFloat(u64),
    /// Push a `bool` constant.
    PushConstBool(bool),
    /// Push `locals[slot]`.
    LoadLocal(u32),
    /// Pop into `locals[slot]`.
    StoreLocal(u32),
    /// `int (op) int → int`. `Add`/`Sub`/`Mul` deopt on i64 overflow;
    /// `FloorDiv`/`Mod` deopt on zero divisor or `MIN / -1`. Never
    /// carries `TrueDiv` (see [`TOp::IntTrueDiv`]).
    IntArith(ArithKind),
    /// `float (op) float → float`. Only `Add`/`Sub`/`Mul`/`TrueDiv`
    /// (float floor-div / mod are non-JITable in v1).
    FloatArith(ArithKind),
    /// `int / int → float` (Python true division). Deopts on a zero
    /// divisor (the interpreter raises `ZeroDivisionError`).
    IntTrueDiv,
    /// `int (cmp) int → bool`.
    IntCmp(CmpKind),
    /// `float (cmp) float → bool`.
    FloatCmp(CmpKind),
    /// `-int`. Deopts on `MIN` negation overflow.
    IntNeg,
    /// `-float`.
    FloatNeg,
    /// `~int`.
    IntInvert,
    /// `not x` for an integral (`int`/`bool`) operand → `bool`.
    IntNot,
    /// `not x` for a `float` operand → `bool`.
    FloatNot,
    /// Discard TOS.
    Pop,
    /// Duplicate TOS (`COPY`).
    Dup,
    /// Swap the top two stack entries (`SWAP 2`).
    Swap2,
}

/// One IR statement: a [`TOp`] tagged with its originating bytecode pc
/// so a side exit can name the exact resume point.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TStmt {
    pub pc: u32,
    pub op: TOp,
}

/// How a basic block transfers control.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TTerm {
    /// Pop TOS and return it from the frame.
    Return,
    /// Unconditional branch; the current abstract stack is passed as
    /// block args.
    Jump(BlockId),
    /// `POP_JUMP_IF_FALSE`: pop the condition; branch to `target` if
    /// falsy, else `fallthrough`.
    BranchFalse {
        target: BlockId,
        fallthrough: BlockId,
    },
    /// `POP_JUMP_IF_TRUE`: pop the condition; branch to `target` if
    /// truthy, else `fallthrough`.
    BranchTrue {
        target: BlockId,
        fallthrough: BlockId,
    },
}

/// A basic block: a static entry-stack shape, a straight-line body, and
/// a terminator.
#[derive(Clone, Debug, PartialEq)]
pub struct TBlock {
    /// Types of the operand-stack values live on entry (lowered to
    /// Cranelift block parameters), bottom-to-top.
    pub entry_stack: Vec<JitType>,
    pub stmts: Vec<TStmt>,
    pub term: TTerm,
}

/// A fully analyzed, JITable function body.
#[derive(Clone, Debug, PartialEq)]
pub struct TFunc {
    /// Number of local slots in the originating code object.
    pub n_locals: u32,
    /// Stable JIT type of each local slot, or `None` for slots the
    /// region never touches (left untouched by the JIT).
    pub local_types: Vec<Option<JitType>>,
    /// Local slots that are live-in at function entry (read before
    /// written). The VM type-guards and packs exactly these before
    /// entering native code.
    pub livein_locals: Vec<u32>,
    /// Maximum abstract operand-stack depth, for sizing the deopt spill
    /// buffer.
    pub max_stack: u32,
    pub blocks: Vec<TBlock>,
    pub entry_block: BlockId,
}

impl TOp {
    /// `true` for operations that can take a side exit (deopt) and so
    /// need their abstract stack spilled at their pc.
    #[must_use]
    pub fn can_deopt(self) -> bool {
        matches!(
            self,
            TOp::IntArith(
                ArithKind::Add
                    | ArithKind::Sub
                    | ArithKind::Mul
                    | ArithKind::FloorDiv
                    | ArithKind::Mod
            ) | TOp::IntNeg
                | TOp::IntTrueDiv
                | TOp::FloatArith(ArithKind::TrueDiv)
        )
    }
}

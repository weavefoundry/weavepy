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
    /// Convert the integral value at TOS to `float` (RFC 0058 WS4 mixed
    /// arithmetic promotion, matching the interpreter's `as f64` cast).
    /// When `guarded`, deopt unless `|v| <= 2^53` — the range where the
    /// conversion is exact — because mixed-lane *comparisons* are
    /// mathematically exact in the interpreter.
    IntToFloatTos { guarded: bool },
    /// Same conversion applied to the entry *below* TOS. A dedicated op
    /// (rather than `Swap2` + `IntToFloatTos` + `Swap2`) so a guarded
    /// deopt spills the operand stack in its original order.
    IntToFloatSecond { guarded: bool },
    /// RFC 0059 WS3 — a Python-to-Python call through the embedder's
    /// `wpjit_call_py` helper. Pops `argc` scalar arguments (the burned
    /// callee never reached the JIT stack — see
    /// [`CalleeSpanMeta`]), calls `token` (an index into the compiled
    /// frame's callee table), and pushes a `ret` result. A raised
    /// callee takes the `Raised` exit at this pc; a result outside the
    /// `ret` lane (or a caller guard invalidated by the callee's side
    /// effects) deopts *after* the call with the result spilled.
    CallPy { token: u32, argc: u8, ret: JitType },
    /// RFC 0061 WS5 — `BINARY_SUBSCR` on a pinned list: pops the `int`
    /// index and the pin reference, calls the registered
    /// `wpjit_list_get` helper (bounds + element-lane checked against
    /// the real `Object::List`), and pushes the `elem`-lane result.
    /// Any surprise (out of range, aliased shape change) deopts at
    /// this pc with both operands spilled, so the interpreter
    /// re-executes the subscript generically.
    ListGet { elem: JitType },
    /// RFC 0061 WS5 — `STORE_SUBSCR` on a pinned list: pops the index,
    /// the pin reference, and the value (staged through the frame's
    /// `ret_bits`), and calls `wpjit_list_set`. Out-of-range deopts at
    /// this pc; the interpreter re-executes the store and raises.
    ListSet,
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
    /// RFC 0058 WS4 — a recognized `FOR_ITER` over a unit-step `range`,
    /// rewritten to an i64 counted loop over two synthetic local slots.
    /// If `cur < stop`: store `cur` into `var_slot`, bump `cur`, and
    /// branch to `body`; else branch to `exit`. `cur < stop <= i64::MAX`
    /// makes the unit-step increment provably overflow-free.
    ForRange {
        cur_slot: u32,
        stop_slot: u32,
        var_slot: u32,
        body: BlockId,
        exit: BlockId,
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

/// What a `LOAD_GLOBAL` name resolved to at analysis time. Provided by
/// the embedder's resolver closure; anything other than `Opaque` is
/// burned into the compiled code, and the embedder must re-validate the
/// resolution (an identity guard) on every native entry.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ResolvedGlobal {
    /// The canonical builtin `range` — eligible as a counted-loop callee.
    RangeBuiltin,
    /// An `int` global, burned in as a constant.
    ConstInt(i64),
    /// A `float` global, burned in as a constant (stored as bits so the
    /// enum stays `Copy` + `PartialEq`).
    ConstFloat(u64),
    /// A `bool` global, burned in as a constant.
    ConstBool(bool),
    /// RFC 0059 WS3 — a plain Python function, callable natively through
    /// the `wpjit_call_py` helper. `token` indexes the embedder's callee
    /// table (parallel to resolution order); `arg_count` is the callee's
    /// positional arity; `ret` is the callee's inferred scalar return
    /// lane (`None` when the callee is the function being compiled —
    /// the analyzer resolves self-recursion through its own return-lane
    /// fixpoint).
    PyFunc {
        token: u32,
        arg_count: u32,
        is_self: bool,
        ret: Option<JitType>,
    },
    /// Anything else — not representable; the load disqualifies the frame.
    Opaque,
}

/// One entry guard the embedder must re-validate before each native
/// entry: `name` must still resolve (globals-then-builtins) to the same
/// object it resolved to at compile time.
#[derive(Clone, Debug, PartialEq)]
pub struct GlobalGuard {
    pub name: String,
    pub expect: ResolvedGlobal,
}

/// Deopt-reconstruction metadata for one erased callee (RFC 0059 WS3):
/// between the (erased) `LOAD_GLOBAL` of a Python callee and its `CALL`,
/// the *interpreter's* operand stack holds the callee object at absolute
/// depth `interp_depth` (below the argument temporaries, above any
/// enclosing range iterators). A deopt at a pc strictly inside
/// `(live_from, live_to)` must re-insert the callee-table object at that
/// index when rebuilding the stack.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CalleeSpanMeta {
    /// Callee-table index (same space as [`TOp::CallPy`]'s `token`).
    pub token: u32,
    /// The erased `LOAD_GLOBAL` pc.
    pub live_from: u32,
    /// The `CALL` pc (the call itself consumes the callee, so the span
    /// is open only for pcs strictly between the endpoints).
    pub live_to: u32,
    /// Absolute interpreter-stack index of the callee object,
    /// accounting for enclosing erased entities (range iterators and
    /// outer callee spans).
    pub interp_depth: u32,
}

/// One OSR entry point (RFC 0059 WS3b): a backward-jump target block
/// with an empty boundary stack, enterable mid-frame via
/// [`crate::JitFrame::entry_pc`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OsrEntry {
    /// The bytecode pc of the block leader (the backward jump's target).
    pub pc: u32,
    /// The [`TBlock`] to enter.
    pub block: BlockId,
}

/// Deopt-reconstruction metadata for one rewritten `range` loop: at any
/// deopt pc in `[live_from, live_to)` the *interpreter's* operand stack
/// would hold the live range iterator below the spilled temporaries, so
/// the embedder must rebuild it from the two synthetic slots.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RangeLoopMeta {
    /// Synthetic slot holding the next value to yield.
    pub cur_slot: u32,
    /// Synthetic slot holding the exclusive stop bound.
    pub stop_slot: u32,
    /// First pc (the `FOR_ITER`) at which the iterator is live.
    pub live_from: u32,
    /// The `END_FOR` pc; the iterator is dead from here on.
    pub live_to: u32,
}

/// A fully analyzed, JITable function body.
#[derive(Clone, Debug, PartialEq)]
pub struct TFunc {
    /// Number of local slots, *including* the synthetic `range`-loop
    /// slots appended after the code object's real locals.
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
    /// Entry guards for every `LOAD_GLOBAL` burned into the code
    /// (RFC 0058 WS4), deduplicated by name.
    pub global_guards: Vec<GlobalGuard>,
    /// Rewritten `range` loops, ordered outermost-first (ascending
    /// `live_from`), for deopt stack reconstruction.
    pub range_loops: Vec<RangeLoopMeta>,
    /// Erased Python callees (RFC 0059 WS3), ascending `live_from`, for
    /// deopt stack reconstruction during argument computation.
    pub callee_spans: Vec<CalleeSpanMeta>,
    /// OSR entry points (RFC 0059 WS3b): backward-jump target blocks
    /// enterable via `entry_pc`.
    pub osr_entries: Vec<OsrEntry>,
    /// Widest `CallPy` argument count, for sizing the marshal buffer.
    pub max_call_args: u32,
    /// The function's own scalar return lane, when every `return` site
    /// agrees on one representable lane (RFC 0059 WS3). This is what a
    /// *caller's* analysis burns in as `PyFunc::ret`.
    pub ret_lane: Option<JitType>,
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
                | TOp::IntToFloatTos { guarded: true }
                | TOp::IntToFloatSecond { guarded: true }
                | TOp::CallPy { .. }
                | TOp::ListGet { .. }
                | TOp::ListSet
        )
    }
}

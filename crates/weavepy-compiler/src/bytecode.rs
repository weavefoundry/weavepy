//! Bytecode instruction set for the WeavePy VM.
//!
//! Opcode names track CPython 3.13's `Lib/opcode.py` where applicable
//! so `dis` listings look familiar. Each instruction is a fixed
//! `{ op, arg }` pair — the slice favours simplicity of emission and
//! dispatch over CPython's 16-bit-packed encoding, which is RFC 0007
//! territory.
//!
//! # Compatibility level
//!
//! - **Tracks CPython** for opcode names and rough semantics.
//! - **Experimental** for the binary encoding; we explicitly do not
//!   promise wire compatibility with CPython's `.pyc` format.

/// Sub-operation tag for [`OpCode::BinaryOp`]. Mirrors CPython 3.11+'s
/// `_NB_*` enumeration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum BinOpKind {
    Add = 0,
    Sub = 1,
    Mult = 2,
    Div = 3,
    FloorDiv = 4,
    Mod = 5,
    Pow = 6,
    LShift = 7,
    RShift = 8,
    BitOr = 9,
    BitXor = 10,
    BitAnd = 11,
    MatMult = 12,
}

/// Comparison operator tag for [`OpCode::CompareOp`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum CompareKind {
    Lt = 0,
    LtE = 1,
    Eq = 2,
    NotEq = 3,
    Gt = 4,
    GtE = 5,
}

impl CompareKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Lt => "<",
            Self::LtE => "<=",
            Self::Eq => "==",
            Self::NotEq => "!=",
            Self::Gt => ">",
            Self::GtE => ">=",
        }
    }
}

impl BinOpKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Add => "+",
            Self::Sub => "-",
            Self::Mult => "*",
            Self::Div => "/",
            Self::FloorDiv => "//",
            Self::Mod => "%",
            Self::Pow => "**",
            Self::LShift => "<<",
            Self::RShift => ">>",
            Self::BitOr => "|",
            Self::BitXor => "^",
            Self::BitAnd => "&",
            Self::MatMult => "@",
        }
    }
}

/// Unary op tag for [`OpCode::UnaryOp`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum UnaryKind {
    Pos = 0,
    Neg = 1,
    Not = 2,
    Invert = 3,
}

impl UnaryKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pos => "+",
            Self::Neg => "-",
            Self::Not => "not",
            Self::Invert => "~",
        }
    }
}

/// Opcodes emitted by the WeavePy compiler. The argument's meaning
/// depends on the opcode — see comments per variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum OpCode {
    /// No-op. Used to reserve a slot during compilation.
    Nop,
    /// Marker for the start of a frame. Matches CPython's `RESUME 0`.
    Resume,

    // Constants / names / variables
    /// Push `co_consts[arg]`.
    LoadConst,
    /// Push the value bound to `co_names[arg]` from globals/builtins.
    LoadName,
    /// Push the value bound to `co_names[arg]` from globals.
    LoadGlobal,
    /// Push `co_varnames[arg]` from the current frame's locals.
    LoadFast,
    /// Pop and store TOS into `co_varnames[arg]`.
    StoreFast,
    /// Pop and store TOS into globals[co_names[arg]].
    StoreGlobal,
    /// Pop and store TOS into locals/globals[co_names[arg]].
    StoreName,
    /// Delete locals[co_varnames[arg]].
    DeleteFast,
    /// Delete globals[co_names[arg]].
    DeleteGlobal,
    /// Delete locals/globals[co_names[arg]].
    DeleteName,
    /// Push the cell at `co_freevars[arg]` contents (closure value).
    LoadDeref,
    /// Store TOS into the cell at `co_freevars[arg]`.
    StoreDeref,
    /// Push a new empty cell into the cells array for this frame
    /// (cellvar at offset `arg`).
    MakeCell,
    /// Push the cell object itself (not its content) — for building closures.
    LoadClosure,

    // Attributes / subscripts
    /// `value = stack[-1]; push value.<co_names[arg]>`
    LoadAttr,
    /// `value = stack[-1]; tos = stack[-2]; tos.<co_names[arg]> = value`
    StoreAttr,
    /// Delete attribute.
    DeleteAttr,
    /// `i = pop; obj = pop; push obj[i]`
    BinarySubscr,
    /// `i = pop; obj = pop; v = pop; obj[i] = v`
    StoreSubscr,
    /// Delete subscript.
    DeleteSubscr,

    // Arithmetic / logical
    /// Pop two values, apply [`BinOpKind`] from `arg`, push result.
    BinaryOp,
    /// Pop one value, apply [`UnaryKind`] from `arg`, push result.
    UnaryOp,
    /// Compare top two values using [`CompareKind`].
    CompareOp,
    /// `is` (or `is not` if arg == 1) — pops two, pushes bool.
    IsOp,
    /// `in` (or `not in` if arg == 1) — pops two, pushes bool.
    ContainsOp,

    // Stack management
    /// Pop and discard the top of stack.
    PopTop,
    /// Push a copy of the top of stack.
    CopyTop,
    /// Swap two top stack values.
    Swap,

    // Calls
    /// Call a callable with `arg` positional arguments.
    /// Stack layout (top-down): `arg_n, arg_(n-1), ..., arg_1, callable`.
    Call,
    /// Call with keyword arguments. `arg` = positional arg count;
    /// stack also carries kw arg names (tuple) and values.
    CallKw,
    /// Return TOS from the current frame.
    ReturnValue,

    // Control flow
    /// Pop TOS; if falsy, jump by `arg` instructions (signed).
    PopJumpIfFalse,
    /// Pop TOS; if truthy, jump by `arg` instructions (signed).
    PopJumpIfTrue,
    /// Unconditional forward jump.
    JumpForward,
    /// Unconditional backward jump (`arg` is a positive distance to subtract).
    JumpBackward,

    // Iterators
    /// Pop iterable, push its iterator.
    GetIter,
    /// Advance the iterator at TOS; on StopIteration, jump by `arg`.
    ForIter,
    /// Pop iterator (matches CPython's `END_FOR`).
    EndFor,

    // Containers
    BuildList,
    BuildTuple,
    BuildSet,
    BuildMap,
    BuildString,
    ListAppend,
    SetAdd,
    MapAdd,
    /// Unpack iterable at TOS into `arg` values, push them in
    /// reverse order (so the first element ends up at the bottom).
    UnpackSequence,

    // Functions / closures
    /// Build a function object from the code object on TOS.
    /// `arg` is a bitmask: 0x01 = defaults tuple, 0x02 = kw_defaults
    /// dict, 0x04 = annotations dict, 0x08 = closure tuple.
    MakeFunction,
    /// Slice — build a Slice(low, high, step) and push it.
    BuildSlice,

    /// Print the diagnostic representation of TOS — used by the
    /// `dis` formatter only. Never emitted; reserved.
    PrintExpr,
}

impl OpCode {
    pub fn name(self) -> &'static str {
        use OpCode::{
            BinaryOp, BinarySubscr, BuildList, BuildMap, BuildSet, BuildSlice, BuildString,
            BuildTuple, Call, CallKw, CompareOp, ContainsOp, CopyTop, DeleteAttr, DeleteFast,
            DeleteGlobal, DeleteName, DeleteSubscr, EndFor, ForIter, GetIter, IsOp, JumpBackward,
            JumpForward, ListAppend, LoadAttr, LoadClosure, LoadConst, LoadDeref, LoadFast,
            LoadGlobal, LoadName, MakeCell, MakeFunction, MapAdd, Nop, PopJumpIfFalse,
            PopJumpIfTrue, PopTop, PrintExpr, Resume, ReturnValue, SetAdd, StoreAttr, StoreDeref,
            StoreFast, StoreGlobal, StoreName, StoreSubscr, Swap, UnaryOp, UnpackSequence,
        };
        match self {
            Nop => "NOP",
            Resume => "RESUME",
            LoadConst => "LOAD_CONST",
            LoadName => "LOAD_NAME",
            LoadGlobal => "LOAD_GLOBAL",
            LoadFast => "LOAD_FAST",
            StoreFast => "STORE_FAST",
            StoreGlobal => "STORE_GLOBAL",
            StoreName => "STORE_NAME",
            DeleteFast => "DELETE_FAST",
            DeleteGlobal => "DELETE_GLOBAL",
            DeleteName => "DELETE_NAME",
            LoadDeref => "LOAD_DEREF",
            StoreDeref => "STORE_DEREF",
            MakeCell => "MAKE_CELL",
            LoadClosure => "LOAD_CLOSURE",
            LoadAttr => "LOAD_ATTR",
            StoreAttr => "STORE_ATTR",
            DeleteAttr => "DELETE_ATTR",
            BinarySubscr => "BINARY_SUBSCR",
            StoreSubscr => "STORE_SUBSCR",
            DeleteSubscr => "DELETE_SUBSCR",
            BinaryOp => "BINARY_OP",
            UnaryOp => "UNARY_OP",
            CompareOp => "COMPARE_OP",
            IsOp => "IS_OP",
            ContainsOp => "CONTAINS_OP",
            PopTop => "POP_TOP",
            CopyTop => "COPY",
            Swap => "SWAP",
            Call => "CALL",
            CallKw => "CALL_KW",
            ReturnValue => "RETURN_VALUE",
            PopJumpIfFalse => "POP_JUMP_IF_FALSE",
            PopJumpIfTrue => "POP_JUMP_IF_TRUE",
            JumpForward => "JUMP_FORWARD",
            JumpBackward => "JUMP_BACKWARD",
            GetIter => "GET_ITER",
            ForIter => "FOR_ITER",
            EndFor => "END_FOR",
            BuildList => "BUILD_LIST",
            BuildTuple => "BUILD_TUPLE",
            BuildSet => "BUILD_SET",
            BuildMap => "BUILD_MAP",
            BuildString => "BUILD_STRING",
            ListAppend => "LIST_APPEND",
            SetAdd => "SET_ADD",
            MapAdd => "MAP_ADD",
            UnpackSequence => "UNPACK_SEQUENCE",
            MakeFunction => "MAKE_FUNCTION",
            BuildSlice => "BUILD_SLICE",
            PrintExpr => "PRINT_EXPR",
        }
    }
}

/// One emitted instruction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Instruction {
    pub op: OpCode,
    pub arg: u32,
}

impl Instruction {
    #[inline]
    pub const fn new(op: OpCode, arg: u32) -> Self {
        Self { op, arg }
    }
}

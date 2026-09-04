//! CPython-3.14 bytecode wire-format codec (RFC 0033, re-pointed by
//! RFC 0077 WS9).
//!
//! WeavePy executes its own flat `Vec<Instruction>` (see [`crate::bytecode`]).
//! CPython tooling — `dis`, `marshal`, `.pyc`, and the `code` object's
//! `co_code` / `co_linetable` / `co_exceptiontable` / `co_positions()`
//! surface — expects the 16-bit `_Py_CODEUNIT` stream CPython 3.14 emits.
//!
//! This module bridges the two. It is a *presentation* codec: the VM
//! never runs the bytes produced here, so the encoding is computed on
//! demand (when Python introspects a code object or marshals it) and is
//! independent of the dispatch loop, the inline caches (RFC 0021), and
//! the JIT (RFC 0032).
//!
//! The encoder is a faithful CPython-3.14 emitter:
//!
//! - opcode numbers and the per-opcode inline-`CACHE` entry counts match
//!   CPython 3.14 (`Include/opcode_ids.h`, `_PyOpcode_Caches`),
//! - args wider than a byte are prefixed with `EXTENDED_ARG`,
//! - relative jumps are recomputed in code units across the inserted
//!   caches via a fixpoint,
//! - the 3.14 assembler-stage shapes are reproduced: `NOT_TAKEN` after
//!   every conditional jump (`normalize_jumps`), `LOAD_FAST_BORROW`
//!   from the `optimize_load_fast` liveness pass, `LOAD_LOCALS` before a
//!   class body's `LOAD_FROM_DICT_OR_DEREF`, `END_ASYNC_FOR`'s
//!   `END_SEND`-relative oparg, superinstructions, and the callable-NULL
//!   fold into `LOAD_GLOBAL`,
//! - the location table uses the PEP 626/657 forms,
//! - the exception table uses CPython's big-endian varint range format.
//!
//! The [`decode`] direction inverts [`encode`] for the canonical opcode
//! set WeavePy emits, so `marshal`/`.pyc` round-trip to an executable
//! [`CodeObject`].

use crate::bytecode::{
    BinOpKind, CompareKind, Instruction, OpCode, UnaryKind, COMPARE_OP_TO_BOOL_FLAG,
};
use crate::{CodeObject, Constant, ExcHandler};

/// CPython 3.14 opcode numbers. Sourced from `Include/opcode_ids.h` in
/// CPython v3.14.7 (every real, non-instrumented opcode; the
/// pseudo-ops `>= 256` never reach the wire).
pub mod op {
    pub const CACHE: u8 = 0;
    pub const BINARY_SLICE: u8 = 1;
    pub const BUILD_TEMPLATE: u8 = 2;
    pub const CALL_FUNCTION_EX: u8 = 4;
    pub const CHECK_EG_MATCH: u8 = 5;
    pub const CHECK_EXC_MATCH: u8 = 6;
    pub const CLEANUP_THROW: u8 = 7;
    pub const DELETE_SUBSCR: u8 = 8;
    pub const END_FOR: u8 = 9;
    pub const END_SEND: u8 = 10;
    pub const EXIT_INIT_CHECK: u8 = 11;
    pub const FORMAT_SIMPLE: u8 = 12;
    pub const FORMAT_WITH_SPEC: u8 = 13;
    pub const GET_AITER: u8 = 14;
    pub const GET_ANEXT: u8 = 15;
    pub const GET_ITER: u8 = 16;
    pub const RESERVED: u8 = 17;
    pub const GET_LEN: u8 = 18;
    pub const GET_YIELD_FROM_ITER: u8 = 19;
    pub const INTERPRETER_EXIT: u8 = 20;
    pub const LOAD_BUILD_CLASS: u8 = 21;
    /// Emitted by the encoder in front of a class body's
    /// `LOAD_FROM_DICT_OR_DEREF` (WeavePy's single `LoadClassderef`
    /// instruction is CPython's two-unit `LOAD_LOCALS; LOAD_FROM_DICT_OR_DEREF`
    /// sequence). The decoder folds the pair back.
    pub const LOAD_LOCALS: u8 = 22;
    pub const MAKE_FUNCTION: u8 = 23;
    pub const MATCH_KEYS: u8 = 24;
    pub const MATCH_MAPPING: u8 = 25;
    pub const MATCH_SEQUENCE: u8 = 26;
    pub const NOP: u8 = 27;
    /// Instrumentation anchor CPython's assembler places after every
    /// conditional jump (`normalize_jumps`). The encoder synthesizes
    /// it; the decoder folds a `POP_JUMP_IF_*; NOT_TAKEN` pair back
    /// into the jump alone (a standalone unit decodes to `NotTaken`).
    pub const NOT_TAKEN: u8 = 28;
    pub const POP_EXCEPT: u8 = 29;
    pub const POP_ITER: u8 = 30;
    pub const POP_TOP: u8 = 31;
    pub const PUSH_EXC_INFO: u8 = 32;
    pub const PUSH_NULL: u8 = 33;
    pub const RETURN_GENERATOR: u8 = 34;
    pub const RETURN_VALUE: u8 = 35;
    pub const SETUP_ANNOTATIONS: u8 = 36;
    pub const STORE_SLICE: u8 = 37;
    pub const STORE_SUBSCR: u8 = 38;
    pub const TO_BOOL: u8 = 39;
    pub const UNARY_INVERT: u8 = 40;
    pub const UNARY_NEGATIVE: u8 = 41;
    pub const UNARY_NOT: u8 = 42;
    pub const WITH_EXCEPT_START: u8 = 43;
    pub const BINARY_OP: u8 = 44;
    pub const BUILD_INTERPOLATION: u8 = 45;
    pub const BUILD_LIST: u8 = 46;
    pub const BUILD_MAP: u8 = 47;
    pub const BUILD_SET: u8 = 48;
    pub const BUILD_SLICE: u8 = 49;
    pub const BUILD_STRING: u8 = 50;
    pub const BUILD_TUPLE: u8 = 51;
    pub const CALL: u8 = 52;
    pub const CALL_INTRINSIC_1: u8 = 53;
    pub const CALL_INTRINSIC_2: u8 = 54;
    pub const CALL_KW: u8 = 55;
    pub const COMPARE_OP: u8 = 56;
    pub const CONTAINS_OP: u8 = 57;
    pub const CONVERT_VALUE: u8 = 58;
    pub const COPY: u8 = 59;
    /// Function-entry prologue unit copying the closure tuple into the
    /// frame's free-variable slots. The encoder synthesizes it (WeavePy
    /// frame setup does the copy natively); see `insert_prologue`.
    pub const COPY_FREE_VARS: u8 = 60;
    pub const DELETE_ATTR: u8 = 61;
    pub const DELETE_DEREF: u8 = 62;
    pub const DELETE_FAST: u8 = 63;
    pub const DELETE_GLOBAL: u8 = 64;
    pub const DELETE_NAME: u8 = 65;
    pub const DICT_MERGE: u8 = 66;
    pub const DICT_UPDATE: u8 = 67;
    /// Carries an oparg since 3.14: the code-unit distance back to the
    /// matching `END_SEND` (`sys.monitoring` pairs the two). WeavePy's
    /// internal arg is unused; the encoder finds the dance's `SEND`
    /// through the `__anext__` exception range this instruction
    /// handles and applies CPython's fixed `END_SEND_OFFSET`.
    pub const END_ASYNC_FOR: u8 = 68;
    pub const EXTENDED_ARG: u8 = 69;
    pub const FOR_ITER: u8 = 70;
    pub const GET_AWAITABLE: u8 = 71;
    pub const IMPORT_FROM: u8 = 72;
    pub const IMPORT_NAME: u8 = 73;
    pub const IS_OP: u8 = 74;
    pub const JUMP_BACKWARD: u8 = 75;
    pub const JUMP_BACKWARD_NO_INTERRUPT: u8 = 76;
    pub const JUMP_FORWARD: u8 = 77;
    pub const LIST_APPEND: u8 = 78;
    pub const LIST_EXTEND: u8 = 79;
    pub const LOAD_ATTR: u8 = 80;
    pub const LOAD_COMMON_CONSTANT: u8 = 81;
    pub const LOAD_CONST: u8 = 82;
    pub const LOAD_DEREF: u8 = 83;
    pub const LOAD_FAST: u8 = 84;
    pub const LOAD_FAST_AND_CLEAR: u8 = 85;
    /// Produced by the encoder's port of `flowgraph.c::optimize_load_fast`:
    /// a `LOAD_FAST` whose reference is consumed before the local can
    /// be rebound (and never stored or left on the stack across a
    /// block boundary) pushes a borrowed reference. Decodes back to a
    /// plain `LoadFast` (WeavePy's runtime has one load form).
    pub const LOAD_FAST_BORROW: u8 = 86;
    pub const LOAD_FAST_BORROW_LOAD_FAST_BORROW: u8 = 87;
    /// Emitted by the encoder's uninitialized-locals analysis
    /// (CPython's `add_checks_for_loads_of_uninitialized_variables`):
    /// a `LOAD_FAST` the compiler can't prove bound decodes back to a
    /// plain `LoadFast` (WeavePy's runtime op always checks).
    pub const LOAD_FAST_CHECK: u8 = 88;
    /// Superinstructions (CPython's `insert_superinstructions`): two
    /// adjacent fast-local ops fused into one unit, args packed as
    /// `(arg1 << 4) | arg2`.
    pub const LOAD_FAST_LOAD_FAST: u8 = 89;
    pub const LOAD_FROM_DICT_OR_DEREF: u8 = 90;
    pub const LOAD_FROM_DICT_OR_GLOBALS: u8 = 91;
    pub const LOAD_GLOBAL: u8 = 92;
    pub const LOAD_NAME: u8 = 93;
    pub const LOAD_SMALL_INT: u8 = 94;
    pub const LOAD_SPECIAL: u8 = 95;
    pub const LOAD_SUPER_ATTR: u8 = 96;
    pub const MAKE_CELL: u8 = 97;
    pub const MAP_ADD: u8 = 98;
    pub const MATCH_CLASS: u8 = 99;
    pub const POP_JUMP_IF_FALSE: u8 = 100;
    pub const POP_JUMP_IF_NONE: u8 = 101;
    pub const POP_JUMP_IF_NOT_NONE: u8 = 102;
    pub const POP_JUMP_IF_TRUE: u8 = 103;
    pub const RAISE_VARARGS: u8 = 104;
    pub const RERAISE: u8 = 105;
    pub const SEND: u8 = 106;
    pub const SET_ADD: u8 = 107;
    pub const SET_FUNCTION_ATTRIBUTE: u8 = 108;
    pub const SET_UPDATE: u8 = 109;
    pub const STORE_ATTR: u8 = 110;
    pub const STORE_DEREF: u8 = 111;
    pub const STORE_FAST: u8 = 112;
    pub const STORE_FAST_LOAD_FAST: u8 = 113;
    pub const STORE_FAST_STORE_FAST: u8 = 114;
    pub const STORE_GLOBAL: u8 = 115;
    pub const STORE_NAME: u8 = 116;
    pub const SWAP: u8 = 117;
    pub const UNPACK_EX: u8 = 118;
    pub const UNPACK_SEQUENCE: u8 = 119;
    pub const YIELD_VALUE: u8 = 120;
    pub const RESUME: u8 = 128;
}

/// CPython 3.14 `HAVE_ARGUMENT` boundary: opcodes `>=` this take an
/// inline argument. Opcodes below it ignore the (still-present) arg byte.
pub const HAVE_ARGUMENT: u8 = 43;

/// CPython's `MAGIC_NUMBER` for the 3.14 series (`importlib.util.MAGIC_NUMBER`):
/// 3627, little-endian, followed by `\r\n`.
pub const MAGIC_NUMBER: [u8; 4] = [0x2b, 0x0e, 0x0d, 0x0a];

/// `_nb_ops` index of `NB_SUBSCR`: since 3.14 `BINARY_SUBSCR` is a
/// `BINARY_OP` flavour (`a[b]` is `BINARY_OP 26`).
pub const NB_SUBSCR: u32 = 26;

/// CPython's `END_SEND_OFFSET` (assemble.c): the fixed code-unit distance
/// from a `SEND` to the `END_SEND` its exhausted edge targets in an
/// `await`/`async for` dance (`SEND` + cache, `YIELD_VALUE`, `RESUME`,
/// `JUMP_BACKWARD_NO_INTERRUPT`). `END_ASYNC_FOR`'s oparg is expressed
/// through it.
const END_SEND_OFFSET: usize = 5;

/// CALL_INTRINSIC_1 sub-op: `INTRINSIC_IMPORT_STAR`.
const INTRINSIC_IMPORT_STAR: u32 = 2;
/// CALL_INTRINSIC_1 sub-op: `INTRINSIC_STOPITERATION_ERROR` (PEP 479
/// epilogue of generator-family code objects).
const INTRINSIC_STOPITERATION_ERROR: u32 = 3;
/// CALL_INTRINSIC_1 sub-op: `INTRINSIC_ASYNC_GEN_WRAP`.
const INTRINSIC_ASYNC_GEN_WRAP: u32 = 4;
/// CALL_INTRINSIC_1 sub-op: `INTRINSIC_UNARY_POSITIVE`.
const INTRINSIC_UNARY_POSITIVE: u32 = 5;
/// CALL_INTRINSIC_1 sub-op: `INTRINSIC_LIST_TO_TUPLE`.
const INTRINSIC_LIST_TO_TUPLE: u32 = 6;
/// CALL_INTRINSIC_2 sub-op: `INTRINSIC_PREP_RERAISE_STAR`.
const INTRINSIC_PREP_RERAISE_STAR: u32 = 1;

/// Number of inline-`CACHE` code units that follow `cp_op` in CPython
/// 3.14 (`_PyOpcode_Caches`). Everything not listed has none.
#[must_use]
pub fn cache_entries(cp_op: u8) -> usize {
    match cp_op {
        op::LOAD_ATTR => 9,
        op::BINARY_OP => 5,
        op::LOAD_GLOBAL | op::STORE_ATTR => 4,
        op::CALL | op::CALL_KW | op::TO_BOOL => 3,
        op::UNPACK_SEQUENCE
        | op::COMPARE_OP
        | op::CONTAINS_OP
        | op::FOR_ITER
        | op::STORE_SUBSCR
        | op::SEND
        | op::JUMP_BACKWARD
        | op::POP_JUMP_IF_TRUE
        | op::POP_JUMP_IF_FALSE
        | op::POP_JUMP_IF_NONE
        | op::POP_JUMP_IF_NOT_NONE
        | op::LOAD_SUPER_ATTR => 1,
        _ => 0,
    }
}

/// `True` if `cp_op` is a relative jump (its arg is a code-unit delta
/// and the internal arg an instruction delta). `END_ASYNC_FOR`'s
/// `END_SEND`-relative oparg is derived separately (see `encode`).
#[must_use]
pub fn is_rel_jump(cp_op: u8) -> bool {
    matches!(
        cp_op,
        op::FOR_ITER
            | op::JUMP_BACKWARD
            | op::JUMP_BACKWARD_NO_INTERRUPT
            | op::JUMP_FORWARD
            | op::POP_JUMP_IF_FALSE
            | op::POP_JUMP_IF_TRUE
            | op::POP_JUMP_IF_NONE
            | op::POP_JUMP_IF_NOT_NONE
            | op::SEND
    )
}

/// `True` if `cp_op` jumps backwards (arg subtracted from the next pc).
#[must_use]
pub fn is_backward_jump(cp_op: u8) -> bool {
    matches!(cp_op, op::JUMP_BACKWARD | op::JUMP_BACKWARD_NO_INTERRUPT)
}

/// `True` for the four `POP_JUMP_IF_*` opcodes: the conditional jumps
/// CPython's `normalize_jumps` follows with a `NOT_TAKEN` unit.
#[must_use]
pub fn is_conditional_jump(cp_op: u8) -> bool {
    matches!(
        cp_op,
        op::POP_JUMP_IF_FALSE
            | op::POP_JUMP_IF_TRUE
            | op::POP_JUMP_IF_NONE
            | op::POP_JUMP_IF_NOT_NONE
    )
}

/// CPython's `NB_INPLACE_ADD` — the in-place variants (`+=` and
/// friends) occupy `_nb_ops[13..=25]`, offset from their plain
/// counterparts by this constant.
const NB_INPLACE_OFFSET: u32 = 13;

/// WeavePy [`BinOpKind`] → CPython `_nb_ops` index (the arg `BINARY_OP`
/// carries; `dis` renders it through `_nb_ops`).
fn binop_to_nb(kind: BinOpKind) -> u32 {
    match kind {
        BinOpKind::Add => 0,
        BinOpKind::BitAnd => 1,
        BinOpKind::FloorDiv => 2,
        BinOpKind::LShift => 3,
        BinOpKind::MatMult => 4,
        BinOpKind::Mult => 5,
        BinOpKind::Mod => 6,
        BinOpKind::BitOr => 7,
        BinOpKind::Pow => 8,
        BinOpKind::RShift => 9,
        BinOpKind::Sub => 10,
        BinOpKind::Div => 11,
        BinOpKind::BitXor => 12,
    }
}

/// Inverse of [`binop_to_nb`].
fn nb_to_binop(nb: u32) -> Option<BinOpKind> {
    Some(match nb {
        0 => BinOpKind::Add,
        1 => BinOpKind::BitAnd,
        2 => BinOpKind::FloorDiv,
        3 => BinOpKind::LShift,
        4 => BinOpKind::MatMult,
        5 => BinOpKind::Mult,
        6 => BinOpKind::Mod,
        7 => BinOpKind::BitOr,
        8 => BinOpKind::Pow,
        9 => BinOpKind::RShift,
        10 => BinOpKind::Sub,
        11 => BinOpKind::Div,
        12 => BinOpKind::BitXor,
        _ => return None,
    })
}

/// A CPython opcode + (already-transformed) argument, before code-unit
/// layout.
#[derive(Clone, Copy)]
struct MappedOp {
    cp_op: u8,
    arg: u32,
}

/// Internal deref index (cellvars then freevars) → wire `localsplus`
/// slot. CPython's layout deduplicates cells that alias a local: a
/// parameter that escapes keeps its parameter slot (kind
/// `CO_FAST_LOCAL|CO_FAST_CELL`); every other cell, then every free,
/// gets a fresh slot after the plain locals.
struct DerefSlots {
    slots: Vec<u32>,
}

impl DerefSlots {
    fn from_code(code: &CodeObject) -> Self {
        let nlocals = code.varnames.len() as u32;
        let mut slots = Vec::with_capacity(code.cellvars.len() + code.freevars.len());
        let mut next = nlocals;
        for c in &code.cellvars {
            if let Some(p) = code.varnames.iter().position(|v| v == c) {
                slots.push(p as u32);
            } else {
                slots.push(next);
                next += 1;
            }
        }
        for _ in &code.freevars {
            slots.push(next);
            next += 1;
        }
        Self { slots }
    }

    fn slot(&self, deref: u32) -> u32 {
        self.slots
            .get(deref as usize)
            .copied()
            .unwrap_or(self.slots.len() as u32 + deref)
    }
}

use crate::bytecode::wire;

/// Map one WeavePy [`Instruction`] to its CPython opcode + arg. Deref
/// opcodes index into the merged localsplus array via `slots`.
fn map_to_cpython(ins: Instruction, slots: &DerefSlots) -> MappedOp {
    use OpCode as O;
    let (cp_op, arg) = match ins.op {
        O::Nop => (op::NOP, 0),
        O::Resume => (op::RESUME, ins.arg),
        O::LoadConst => (op::LOAD_CONST, ins.arg),
        O::LoadName => (op::LOAD_NAME, ins.arg),
        // CPython packs a "push NULL" flag in bit 0; the name index is arg >> 1.
        O::LoadGlobal => (op::LOAD_GLOBAL, ins.arg << 1),
        O::LoadFast => (op::LOAD_FAST, ins.arg),
        O::LoadFastBorrow => (op::LOAD_FAST_BORROW, ins.arg),
        O::LoadClosureBorrow => (op::LOAD_FAST_BORROW, slots.slot(ins.arg)),
        O::LoadFastCheck => (op::LOAD_FAST_CHECK, ins.arg),
        O::LoadFastLoadFast => (op::LOAD_FAST_LOAD_FAST, ins.arg),
        O::LoadFastBorrowLoadFastBorrow => (op::LOAD_FAST_BORROW_LOAD_FAST_BORROW, ins.arg),
        O::StoreFastLoadFast => (op::STORE_FAST_LOAD_FAST, ins.arg),
        O::StoreFastStoreFast => (op::STORE_FAST_STORE_FAST, ins.arg),
        // The callable-flagged form: name index in the high bits, bit 0
        // set (CPython's `LOAD_GLOBAL` + `PUSH_NULL` fusion).
        O::LoadGlobalPushNull => (op::LOAD_GLOBAL, (ins.arg << 1) | 1),
        O::LoadLocals => (op::LOAD_LOCALS, 0),
        O::LoadFastAndClear => (op::LOAD_FAST_AND_CLEAR, ins.arg),
        O::StoreFast => (op::STORE_FAST, ins.arg),
        O::StoreGlobal => (op::STORE_GLOBAL, ins.arg),
        O::StoreName => (op::STORE_NAME, ins.arg),
        O::DeleteFast => (op::DELETE_FAST, ins.arg),
        O::DeleteGlobal => (op::DELETE_GLOBAL, ins.arg),
        O::DeleteName => (op::DELETE_NAME, ins.arg),
        O::LoadDeref => (op::LOAD_DEREF, slots.slot(ins.arg)),
        O::StoreDeref => (op::STORE_DEREF, slots.slot(ins.arg)),
        O::DeleteDeref => (op::DELETE_DEREF, slots.slot(ins.arg)),
        O::MakeCell => (op::MAKE_CELL, slots.slot(ins.arg)),
        O::CopyFreeVars => (op::COPY_FREE_VARS, ins.arg),
        // LOAD_CLOSURE is a pseudo-op; cells live in the fast array
        // and are loaded with LOAD_FAST.
        O::LoadClosure => (op::LOAD_FAST, slots.slot(ins.arg)),
        // bit 0 = "is method load"; the name index is arg >> 1.
        O::LoadAttr => (op::LOAD_ATTR, ins.arg << 1),
        O::LoadMethodAttr => (op::LOAD_ATTR, (ins.arg << 1) | 1),
        // Arg already carries CPython's packed form: namei << 2 |
        // method-flag (bit 0) | two-arg-super flag (bit 1).
        O::LoadSuperAttr => (op::LOAD_SUPER_ATTR, ins.arg),
        O::PushNull => (op::PUSH_NULL, 0),
        O::StoreAttr => (op::STORE_ATTR, ins.arg),
        O::DeleteAttr => (op::DELETE_ATTR, ins.arg),
        // 3.14 folded BINARY_SUBSCR into the BINARY_OP family.
        O::BinarySubscr => (op::BINARY_OP, NB_SUBSCR),
        O::BinarySlice => (op::BINARY_SLICE, 0),
        O::StoreSubscr => (op::STORE_SUBSCR, 0),
        O::StoreSlice => (op::STORE_SLICE, 0),
        O::DeleteSubscr => (op::DELETE_SUBSCR, 0),
        O::BinaryOp => {
            // Our arg carries the operator in the low byte plus an
            // augmented-assignment flag; CPython encodes in-place ops as
            // separate `_nb_ops` indexes (NB_INPLACE_*).
            let inplace = ins.arg & crate::bytecode::BINARY_OP_INPLACE_FLAG != 0;
            let nb = BinOpKind::from_arg(ins.arg & 0xFF).map_or(ins.arg, |k| {
                binop_to_nb(k) + if inplace { NB_INPLACE_OFFSET } else { 0 }
            });
            (op::BINARY_OP, nb)
        }
        O::UnaryOp => match UnaryKind::from_arg(ins.arg) {
            Some(UnaryKind::Neg) => (op::UNARY_NEGATIVE, 0),
            Some(UnaryKind::Not) => (op::UNARY_NOT, 0),
            Some(UnaryKind::Invert) => (op::UNARY_INVERT, 0),
            // No dedicated opcode for unary `+`.
            _ => (op::CALL_INTRINSIC_1, INTRINSIC_UNARY_POSITIVE),
        },
        // bits 5+ carry the comparison index; the low nibble is CPython's
        // specialization mask (COMPARISON_LESS_THAN=2 / GREATER_THAN=4 /
        // EQUALS=8 / UNORDERED=1). Bit 4 ("convert to bool") is OR'd in by
        // `encode` when the result feeds a conditional jump or `not`,
        // mirroring the COMPARE_OP+TO_BOOL fusion in CPython's optimizer.
        O::CompareOp => {
            // CPython packs `op << 5 | to_bool << 4 | result-mask`.
            let kind = ins.arg & !COMPARE_OP_TO_BOOL_FLAG;
            let to_bool = ins.arg & COMPARE_OP_TO_BOOL_FLAG;
            let mask: u32 = match kind {
                0 => 2,         // <
                1 => 2 | 8,     // <=
                2 => 8,         // ==
                3 => 1 | 2 | 4, // !=
                4 => 4,         // >
                5 => 4 | 8,     // >=
                _ => 0,
            };
            (op::COMPARE_OP, (kind << 5) | to_bool | mask)
        }
        O::IsOp => (op::IS_OP, ins.arg),
        O::ContainsOp => (op::CONTAINS_OP, ins.arg),
        O::PopTop => (op::POP_TOP, 0),
        // Legacy emit sites use arg 0 for a plain dup; CPython COPY's
        // arg is 1-based (mapping patterns emit deeper copies).
        O::CopyTop => (op::COPY, ins.arg.max(1)),
        O::Swap => (op::SWAP, ins.arg),
        O::Call => (op::CALL, ins.arg),
        // CPython's self-slot call shape: the first argument occupies
        // the wire view's self-or-null slot, excluded from the oparg.
        O::CallSelf => (op::CALL, ins.arg.saturating_sub(1)),
        // The wire CALL_KW oparg counts positional + keyword values;
        // the keyword count is folded in by `encode` (it needs the
        // kwnames tuple from the preceding LOAD_CONST).
        O::CallKw => (op::CALL_KW, ins.arg),
        // 3.14's CALL_FUNCTION_EX takes no oparg (the kwargs slot is
        // always present, NULL when the call has no `**`).
        O::CallEx => (op::CALL_FUNCTION_EX, 0),
        O::ReturnValue => (op::RETURN_VALUE, 0),
        O::PopJumpIfFalse => (op::POP_JUMP_IF_FALSE, ins.arg),
        O::PopJumpIfTrue => (op::POP_JUMP_IF_TRUE, ins.arg),
        O::PopJumpIfNone => (op::POP_JUMP_IF_NONE, ins.arg),
        O::PopJumpIfNotNone => (op::POP_JUMP_IF_NOT_NONE, ins.arg),
        O::JumpForward => (op::JUMP_FORWARD, ins.arg),
        O::JumpBackward => (op::JUMP_BACKWARD, ins.arg),
        O::GetIter => (op::GET_ITER, 0),
        O::ForIter => (op::FOR_ITER, ins.arg),
        O::EndFor => (op::END_FOR, 0),
        O::BuildList => (op::BUILD_LIST, ins.arg),
        O::BuildTuple => (op::BUILD_TUPLE, ins.arg),
        O::BuildSet => (op::BUILD_SET, ins.arg),
        O::BuildMap => (op::BUILD_MAP, ins.arg),
        O::BuildString => (op::BUILD_STRING, ins.arg),
        O::ListAppend => (op::LIST_APPEND, ins.arg),
        O::ListExtend => (op::LIST_EXTEND, ins.arg),
        O::ListToTuple => (op::CALL_INTRINSIC_1, INTRINSIC_LIST_TO_TUPLE),
        O::SetAdd => (op::SET_ADD, ins.arg),
        O::SetUpdate => (op::SET_UPDATE, ins.arg),
        O::MapAdd => (op::MAP_ADD, ins.arg),
        O::UnpackSequence => (op::UNPACK_SEQUENCE, ins.arg),
        // Our UNPACK_EX arg keeps the before-star count in the high
        // byte; CPython's keeps it in the low byte.
        O::UnpackEx => (
            op::UNPACK_EX,
            ((ins.arg >> 8) & 0xFF) | ((ins.arg & 0xFF) << 8),
        ),
        // WeavePy folds CPython's DICT_UPDATE (dict display) and
        // DICT_MERGE (call `**` splat) into one opcode: bit 0 selects
        // the merge semantics, `arg >> 1` is the target dict's stack
        // offset minus one (CPython's oparg is that offset).
        O::DictUpdate if ins.arg & 1 != 0 => (op::DICT_MERGE, (ins.arg >> 1) + 1),
        O::DictUpdate => (op::DICT_UPDATE, (ins.arg >> 1) + 1),
        O::SetupAnnotations => (op::SETUP_ANNOTATIONS, 0),
        O::MakeFunction => (op::MAKE_FUNCTION, ins.arg),
        O::SetFunctionAttribute => (op::SET_FUNCTION_ATTRIBUTE, ins.arg),
        O::BuildSlice => (op::BUILD_SLICE, ins.arg),
        O::LoadBuildClass => (op::LOAD_BUILD_CLASS, 0),
        O::LoadClassdictOrDeref => (op::LOAD_FROM_DICT_OR_DEREF, slots.slot(ins.arg)),
        O::LoadClassdictOrGlobal => (op::LOAD_FROM_DICT_OR_GLOBALS, ins.arg),
        O::RaiseVarargs => (op::RAISE_VARARGS, ins.arg),
        O::CheckExcMatch => (op::CHECK_EXC_MATCH, 0),
        O::CheckEGMatch => (op::CHECK_EG_MATCH, 0),
        O::PushExcInfo => (op::PUSH_EXC_INFO, 0),
        O::PopExcept => (op::POP_EXCEPT, 0),
        O::Reraise => (op::RERAISE, ins.arg),
        O::LoadSpecial => (op::LOAD_SPECIAL, ins.arg),
        O::LoadCommonConstant => (op::LOAD_COMMON_CONSTANT, ins.arg),
        O::WithExceptStart => (op::WITH_EXCEPT_START, 0),
        O::ImportName => (op::IMPORT_NAME, ins.arg),
        O::ImportFrom => (op::IMPORT_FROM, ins.arg),
        O::ImportStar => (op::CALL_INTRINSIC_1, INTRINSIC_IMPORT_STAR),
        O::PrepReraiseStar => (op::CALL_INTRINSIC_2, INTRINSIC_PREP_RERAISE_STAR),
        O::CleanupThrow => (op::CLEANUP_THROW, 0),
        O::StopIterationError => (op::CALL_INTRINSIC_1, INTRINSIC_STOPITERATION_ERROR),
        O::AsyncGenWrap => (op::CALL_INTRINSIC_1, INTRINSIC_ASYNC_GEN_WRAP),
        O::CallIntrinsic1 => (op::CALL_INTRINSIC_1, ins.arg),
        O::CallIntrinsic2 => (op::CALL_INTRINSIC_2, ins.arg),
        O::BuildInterpolation => (op::BUILD_INTERPOLATION, ins.arg),
        O::BuildTemplate => (op::BUILD_TEMPLATE, 0),
        O::LoadSmallInt => (op::LOAD_SMALL_INT, ins.arg),
        O::NotTaken => (op::NOT_TAKEN, 0),
        O::PopIter => (op::POP_ITER, 0),
        O::FormatValue => {
            // Neither wire form carries an oparg; the spec-on-stack bit
            // is implied by the opcode choice (and restored on decode).
            if ins.arg & 0x04 != 0 {
                (op::FORMAT_WITH_SPEC, 0)
            } else {
                (op::FORMAT_SIMPLE, 0)
            }
        }
        O::ConvertValue => (op::CONVERT_VALUE, ins.arg),
        O::ToBool => (op::TO_BOOL, 0),
        O::YieldValue => (op::YIELD_VALUE, ins.arg),
        O::GetYieldFromIter => (op::GET_YIELD_FROM_ITER, 0),
        O::ReturnGenerator => (op::RETURN_GENERATOR, 0),
        O::Send => (op::SEND, ins.arg),
        O::EndSend => (op::END_SEND, 0),
        O::GetAwaitable => (op::GET_AWAITABLE, ins.arg),
        O::GetAiter => (op::GET_AITER, 0),
        O::GetAnext => (op::GET_ANEXT, 0),
        // The wire oparg (END_SEND-relative) is resolved in `encode`'s
        // fixpoint; the internal arg carries nothing.
        O::EndAsyncFor => (op::END_ASYNC_FOR, 0),
        O::MatchSequence => (op::MATCH_SEQUENCE, 0),
        O::MatchMapping => (op::MATCH_MAPPING, 0),
        O::MatchClass => (op::MATCH_CLASS, ins.arg),
        O::MatchKeys => (op::MATCH_KEYS, 0),
        O::GetLen => (op::GET_LEN, 0),
        O::PrintExpr => (op::NOP, 0),
        // Flowgraph pseudo-ops never reach the wire: `flowgraph::flatten`
        // lowers every one of them before the stream leaves the compiler.
        O::Jump
        | O::JumpNoInterrupt
        | O::JumpIfFalse
        | O::JumpIfTrue
        | O::SetupFinally
        | O::SetupCleanup
        | O::SetupWith
        | O::PopBlock
        | O::StoreFastMaybeNull => {
            unreachable!("flowgraph pseudo-op {:?} reached the wire encoder", ins.op)
        }
    };
    MappedOp { cp_op, arg }
}

/// `(popped, pushed)` for one WeavePy instruction, as CPython's
/// `_PyOpcode_num_popped` / `_PyOpcode_num_pushed` see the equivalent
/// wire opcode. Flowgraph pseudo-ops follow `pycore_opcode_metadata.h`;
/// the block-push family reports its *jump* shape (the fallthrough
/// effect is zero, see `get_stack_effects`).
pub(crate) fn stack_shape(op: OpCode, arg: u32) -> (usize, usize) {
    use OpCode as O;
    match op {
        O::Jump | O::JumpNoInterrupt | O::PopBlock | O::Nop => (0, 0),
        O::JumpIfFalse | O::JumpIfTrue => (1, 1),
        O::SetupFinally | O::SetupWith => (0, 1),
        O::SetupCleanup => (0, 2),
        O::StoreFastMaybeNull => (1, 0),
        // WeavePy's interactive-mode print pops its operand (CPython
        // spells it `CALL_INTRINSIC_1 INTRINSIC_PRINT; POP_TOP`).
        O::PrintExpr => (1, 0),
        // Slot numbers do not affect the shape.
        _ => {
            let m = map_to_cpython(Instruction { op, arg }, &DerefSlots { slots: Vec::new() });
            cp_stack_shape(m.cp_op, m.arg)
        }
    }
}

/// Number of `EXTENDED_ARG` code units needed to express `arg`.
fn ext_count(arg: u32) -> usize {
    if arg <= 0xFF {
        0
    } else if arg <= 0xFFFF {
        1
    } else if arg <= 0x00FF_FFFF {
        2
    } else {
        3
    }
}

/// A position record, one per emitted code unit. `None` columns mean the
/// column was not tracked (WeavePy threads line numbers, not columns).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Position {
    pub lineno: i32,
    pub end_lineno: i32,
    pub col: Option<u32>,
    pub end_col: Option<u32>,
}

/// The CPython-3.14 wire view of a [`CodeObject`].
#[derive(Debug, Clone, Default)]
pub struct CpythonCode {
    /// Packed `_Py_CODEUNIT` stream (2 bytes per unit: `[opcode, arg]`).
    pub co_code: Vec<u8>,
    /// PEP 626 location table.
    pub co_linetable: Vec<u8>,
    /// CPython varint exception range table.
    pub co_exceptiontable: Vec<u8>,
    /// `varnames ++ cellvars ++ freevars`.
    pub localsplusnames: Vec<String>,
    /// `CO_FAST_*` kind byte per `localsplusnames` entry.
    pub localspluskinds: Vec<u8>,
    /// Maximum operand-stack depth (best-effort).
    pub stacksize: u32,
    /// First source line of the code object.
    pub firstlineno: u32,
    /// One [`Position`] per code unit.
    pub positions: Vec<Position>,
    /// Code-unit offset of each WeavePy instruction's *opcode* unit (i.e.
    /// past any `EXTENDED_ARG` prefix), indexed by WeavePy instruction
    /// index. Multiply by 2 for the `co_code` byte offset CPython's
    /// `f_lasti`/`tb_lasti` expose. Length equals the instruction count.
    pub inst_offsets: Vec<u32>,
}

/// Memoised [`CodeObject::to_cpython`] output. The encoding is pure —
/// it depends only on the (immutable-after-compile) code object — but
/// hot paths (`f_lasti`, `co_lines()` in trace functions) call it per
/// event, and a full re-encode of a large code object costs
/// milliseconds. Interior-mutable under the same GIL invariant as
/// [`crate::bytecode::CacheSlot`] so the fill can happen through a
/// shared `&CodeObject` (`Arc<CodeObject>` crosses thread boundaries).
#[derive(Default)]
pub struct CpCache {
    inner: std::cell::UnsafeCell<Option<std::sync::Arc<CpythonCode>>>,
}

// SAFETY: all reads/writes happen under the VM's GIL invariant — see
// `CacheSlot` in bytecode.rs for the full justification.
unsafe impl Send for CpCache {}
unsafe impl Sync for CpCache {}

impl CpCache {
    fn get_or_init(&self, init: impl FnOnce() -> CpythonCode) -> std::sync::Arc<CpythonCode> {
        // SAFETY: the GIL invariant guarantees no concurrent access.
        let slot = unsafe { &mut *self.inner.get() };
        slot.get_or_insert_with(|| std::sync::Arc::new(init()))
            .clone()
    }
}

impl std::fmt::Debug for CpCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CpCache")
    }
}

/// A clone starts cold: the copy may be mutated (e.g. `code.replace`)
/// before it is next encoded.
impl Clone for CpCache {
    fn clone(&self) -> Self {
        Self::default()
    }
}

/// The cache never affects code-object identity.
impl PartialEq for CpCache {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

pub const CO_FAST_ARG_POS: u8 = 0x02;
pub const CO_FAST_ARG_KW: u8 = 0x04;
pub const CO_FAST_ARG_VAR: u8 = 0x08;
pub const CO_FAST_HIDDEN: u8 = 0x10;
pub const CO_FAST_LOCAL: u8 = 0x20;
pub const CO_FAST_CELL: u8 = 0x40;
pub const CO_FAST_FREE: u8 = 0x80;

/// Build the merged `co_localsplusnames` / `co_localspluskinds` pair.
/// CPython's `compute_localsplus_info`: a cell that aliases a local
/// (an escaping parameter) shares the local's slot with kind
/// `CO_FAST_LOCAL|CO_FAST_CELL` rather than getting its own entry;
/// the argument slots carry their `CO_FAST_ARG_*` kind and hidden
/// comprehension locals `CO_FAST_HIDDEN`.
fn build_localsplus(code: &CodeObject) -> (Vec<String>, Vec<u8>) {
    let mut names = Vec::with_capacity(code.varnames.len() + code.cellvars.len());
    let mut kinds = Vec::with_capacity(names.capacity());
    // `argvarkinds`: pos-only, pos-or-kw, kw-only, *args, **kwargs.
    let groups: [(usize, u8); 5] = [
        (code.posonly_count as usize, CO_FAST_ARG_POS),
        (
            code.arg_count.saturating_sub(code.posonly_count) as usize,
            CO_FAST_ARG_POS | CO_FAST_ARG_KW,
        ),
        (code.kwonly_count as usize, CO_FAST_ARG_KW),
        (
            usize::from(code.has_varargs),
            CO_FAST_ARG_VAR | CO_FAST_ARG_POS,
        ),
        (
            usize::from(code.has_varkeywords),
            CO_FAST_ARG_VAR | CO_FAST_ARG_KW,
        ),
    ];
    let mut arg_kind_of = Vec::with_capacity(code.varnames.len());
    for (count, kind) in groups {
        for _ in 0..count {
            arg_kind_of.push(kind);
        }
    }
    for (i, v) in code.varnames.iter().enumerate() {
        let mut kind = CO_FAST_LOCAL | arg_kind_of.get(i).copied().unwrap_or(0);
        if code.hidden_locals.iter().any(|h| h == v) {
            kind |= CO_FAST_HIDDEN;
        }
        if code.cellvars.iter().any(|c| c == v) {
            kind |= CO_FAST_CELL;
        }
        names.push(v.clone());
        kinds.push(kind);
    }
    for c in &code.cellvars {
        if code.varnames.iter().any(|v| v == c) {
            continue;
        }
        names.push(c.clone());
        kinds.push(CO_FAST_CELL);
    }
    for f in &code.freevars {
        names.push(f.clone());
        kinds.push(CO_FAST_FREE);
    }
    (names, kinds)
}

/// Encode `code` into its CPython-3.14 wire view.
///
/// The instruction stream is already in its final shape when it gets
/// here: the flowgraph (`flowgraph::optimize`) has inserted the
/// prologue, the `NOT_TAKEN`s, the superinstructions, the borrowing
/// loads and the uninitialized-local checks, so every WeavePy
/// instruction maps to exactly one wire instruction. What remains is
/// the assembler's job (`assemble.c`): `EXTENDED_ARG` prefixes, cache
/// entries, code-unit-relative jump operands, and the three side
/// tables.
// Index-driven on purpose: the passes below read `code.instructions[i]`
// while rewriting the parallel `mapped[i]` — one offset indexes two
// arrays, which enumerate can't express without losing clarity.
#[allow(clippy::needless_range_loop)]
#[must_use]
pub fn encode(code: &CodeObject) -> CpythonCode {
    let slots = DerefSlots::from_code(code);
    let n = code.instructions.len();
    let mut mapped: Vec<MappedOp> = code
        .instructions
        .iter()
        .map(|ins| map_to_cpython(*ins, &slots))
        .collect();

    // Wire marks: borrowing/checked loads and superinstruction fusion
    // (`bytecode::wire`). A fusion head takes the superinstruction's
    // opcode and packed oparg; its tail is zero-width.
    let mark = |i: usize| code.wire_marks.get(i).copied().unwrap_or(wire::PLAIN);
    let mut zero_width = vec![false; n];
    for i in 0..n {
        let m = mark(i);
        if m & wire::FUSE_HEAD != 0 {
            let head = code.instructions[i];
            let Some(tail) = code.instructions.get(i + 1).copied() else {
                continue;
            };
            let fused = match (head.op, tail.op) {
                (OpCode::LoadGlobal, OpCode::PushNull) => {
                    Some((op::LOAD_GLOBAL, (head.arg << 1) | 1))
                }
                (OpCode::LoadFast, OpCode::LoadFast) if head.arg < 16 && tail.arg < 16 => Some((
                    if m & wire::BORROW != 0 {
                        op::LOAD_FAST_BORROW_LOAD_FAST_BORROW
                    } else {
                        op::LOAD_FAST_LOAD_FAST
                    },
                    (head.arg << 4) | tail.arg,
                )),
                (OpCode::StoreFast, OpCode::LoadFast) if head.arg < 16 && tail.arg < 16 => {
                    Some((op::STORE_FAST_LOAD_FAST, (head.arg << 4) | tail.arg))
                }
                (OpCode::StoreFast, OpCode::StoreFast) if head.arg < 16 && tail.arg < 16 => {
                    Some((op::STORE_FAST_STORE_FAST, (head.arg << 4) | tail.arg))
                }
                _ => None,
            };
            if let Some((cp_op, arg)) = fused {
                mapped[i] = MappedOp { cp_op, arg };
                zero_width[i + 1] = true;
            }
        } else if m & wire::FUSE_TAIL == 0
            && matches!(
                code.instructions[i].op,
                OpCode::LoadFast | OpCode::LoadClosure
            )
        {
            if m & wire::BORROW != 0 {
                mapped[i].cp_op = op::LOAD_FAST_BORROW;
            } else if m & wire::CHECK != 0 {
                mapped[i].cp_op = op::LOAD_FAST_CHECK;
            }
        }
    }

    // The internal stream folds both backward jumps into one opcode;
    // `no_interrupt_jumps` says which ones are `JUMP_NO_INTERRUPT`.
    for &j in &code.no_interrupt_jumps {
        if let Some(m) = mapped.get_mut(j as usize) {
            if m.cp_op == op::JUMP_BACKWARD {
                m.cp_op = op::JUMP_BACKWARD_NO_INTERRUPT;
            }
        }
    }
    // CPython's CALL_KW oparg counts positional *and* keyword values;
    // WeavePy's internal arg is the positional count alone (the kwnames
    // tuple, always the immediately preceding LOAD_CONST, carries the
    // keyword count).
    for i in 1..n {
        if code.instructions[i].op == OpCode::CallKw
            && code.instructions[i - 1].op == OpCode::LoadConst
        {
            if let Some(Constant::Tuple(names)) =
                code.constants.get(code.instructions[i - 1].arg as usize)
            {
                mapped[i].arg += names.len() as u32;
            }
        }
    }

    // END_ASYNC_FOR's oparg points back at the `END_SEND` of the
    // `__anext__` dance it closes (assemble.c: `offset - SEND.offset -
    // END_SEND_OFFSET`). The dance is the exception range this
    // instruction handles; its SEND is the first one inside.
    let eaf_send: Vec<Option<usize>> = (0..n)
        .map(|i| {
            if mapped[i].cp_op != op::END_ASYNC_FOR {
                return None;
            }
            code.exception_table
                .iter()
                .filter(|h| h.handler as usize == i)
                .find_map(|h| {
                    (h.start as usize..(h.end as usize).min(n))
                        .find(|&k| code.instructions[k].op == OpCode::Send)
                })
        })
        .collect();

    // Fixpoint: jump args depend on code-unit offsets, which depend on
    // how many EXTENDED_ARG units precede each instruction.
    //
    // The iteration can have more than one fixpoint (a backward jump
    // whose distance is 255 without its EXTENDED_ARG and 256 with it is
    // self-consistent either way), so the seed decides the result.
    // CPython's `resolve_jump_offsets` sizes every jump on its first
    // pass from the *instruction index* of the target (`i_oparg` still
    // holds the label-resolved index at that point), so a jump to the
    // 300th instruction starts out with an EXTENDED_ARG and keeps it
    // whenever the distance lands on the boundary. Seed the same way:
    // the index counts wire instructions (fusion tails are invisible).
    let cp_index: Vec<usize> = {
        let mut v = Vec::with_capacity(n + 1);
        let mut k = 0usize;
        for &zw in zero_width.iter().take(n) {
            v.push(k);
            if !zw {
                k += 1;
            }
        }
        v.push(k);
        v
    };
    let mut ext: Vec<usize> = mapped
        .iter()
        .enumerate()
        .map(|(i, m)| {
            if zero_width[i] {
                0
            } else if m.cp_op == op::END_ASYNC_FOR {
                ext_count(eaf_send[i].map_or(0, |send| cp_index[send]) as u32)
            } else if is_rel_jump(m.cp_op) {
                let target_idx = if is_backward_jump(m.cp_op) {
                    (i + 1).saturating_sub(args_target_delta(code.instructions[i]))
                } else {
                    i + 1 + args_target_delta(code.instructions[i])
                };
                ext_count(cp_index[target_idx.min(n)] as u32)
            } else {
                ext_count(m.arg)
            }
        })
        .collect();
    let mut starts = vec![0usize; n + 1];
    let mut args: Vec<u32> = mapped.iter().map(|m| m.arg).collect();

    for _ in 0..16 {
        let mut off = 0usize;
        for i in 0..n {
            starts[i] = off;
            if !zero_width[i] {
                off += ext[i] + 1 + cache_entries(mapped[i].cp_op);
            }
        }
        starts[n] = off;

        let mut changed = false;
        for i in 0..n {
            // `PUSH_EXC_INFO` has no oparg in CPython. WeavePy's
            // compiler tags its own copy with the pc past the handler
            // body, but the VM no longer reads the tag (the exception
            // table's cleanup handlers and frame-exit reconciliation
            // keep `sys.exc_info()` balanced, exactly as for CPython-
            // compiled `.pyc`s), so the wire carries 0 (RFC 0077 WS9).
            if mapped[i].cp_op == op::END_ASYNC_FOR {
                let next_unit = starts[i] + ext[i] + 1;
                let oparg = eaf_send[i].map_or(0, |send| {
                    next_unit
                        .saturating_sub(starts[send])
                        .saturating_sub(END_SEND_OFFSET)
                }) as u32;
                args[i] = oparg;
                let need = ext_count(oparg);
                if need != ext[i] {
                    ext[i] = need;
                    changed = true;
                }
                continue;
            }
            if !is_rel_jump(mapped[i].cp_op) || zero_width[i] {
                continue;
            }
            // Jumps are relative to the unit after the instruction's
            // caches.
            let next_unit = starts[i] + ext[i] + 1 + cache_entries(mapped[i].cp_op);
            // WeavePy jump arg is an instruction delta off the *next*
            // instruction (pc is pre-incremented). Resolve the absolute
            // target instruction, then re-express in code units.
            let target_idx = if is_backward_jump(mapped[i].cp_op) {
                (i + 1).saturating_sub(args_target_delta(code.instructions[i]))
            } else {
                i + 1 + args_target_delta(code.instructions[i])
            };
            let target_idx = target_idx.min(n);
            let target_unit = starts[target_idx];
            let oparg = if is_backward_jump(mapped[i].cp_op) {
                next_unit.saturating_sub(target_unit)
            } else {
                target_unit.saturating_sub(next_unit)
            } as u32;
            args[i] = oparg;
            let need = ext_count(oparg);
            if need != ext[i] {
                ext[i] = need;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }

    // Emit code units + per-unit positions.
    let mut co_code: Vec<u8> = Vec::with_capacity(starts[n] * 2);
    let mut positions: Vec<Position> = Vec::with_capacity(starts[n]);
    let mut inst_offsets: Vec<u32> = Vec::with_capacity(n);
    let firstlineno = code_firstlineno(code);
    for i in 0..n {
        if zero_width[i] {
            // A fusion tail: the head's superinstruction stands for it
            // (same `f_lasti`).
            let prev = inst_offsets.last().copied().unwrap_or(0);
            inst_offsets.push(prev);
            continue;
        }
        let pos = position_at(code, i, firstlineno);
        let arg = args[i];
        // EXTENDED_ARG units carry the high base-256 digits, MSB first.
        for k in (1..=ext[i]).rev() {
            let byte = ((arg >> (8 * k)) & 0xFF) as u8;
            co_code.push(op::EXTENDED_ARG);
            co_code.push(byte);
            positions.push(pos);
        }
        // The opcode unit lands here, past any EXTENDED_ARG prefix — this is
        // the code-unit offset CPython's `f_lasti`/`tb_lasti` point at.
        inst_offsets.push((co_code.len() / 2) as u32);
        co_code.push(mapped[i].cp_op);
        co_code.push((arg & 0xFF) as u8);
        positions.push(pos);
        for _ in 0..cache_entries(mapped[i].cp_op) {
            co_code.push(op::CACHE);
            co_code.push(0);
            positions.push(pos);
        }
    }

    let (localsplusnames, localspluskinds) = build_localsplus(code);
    CpythonCode {
        co_linetable: encode_linetable(code, &ext, &mapped, &zero_width, firstlineno),
        co_exceptiontable: encode_exception_table(code, &starts),
        co_code,
        localsplusnames,
        localspluskinds,
        stacksize: compute_stacksize(code),
        firstlineno,
        positions,
        inst_offsets,
    }
}

/// `co_firstlineno`: a module code object always reports 1 in CPython
/// regardless of where its first statement sits (leading blank lines,
/// comments — test_opcodes `test_setup_annotations_line`); anything
/// else starts at its first located instruction (the flowgraph's
/// `firstlineno`, which `resolve_line_numbers` also propagates onto
/// the entry block's unlocated prefix).
fn code_firstlineno(code: &CodeObject) -> u32 {
    if code.name == "<module>" {
        1
    } else {
        code.linetable.iter().copied().find(|&l| l > 0).unwrap_or(1)
    }
}

/// The PEP 657 position of instruction `i` as the wire tables present
/// it. WeavePy's linetable uses 0 as the NO_LOCATION sentinel; the
/// presentation layer uses -1 (CPython's convention) so that *real*
/// line 0 — the module's opening RESUME — stays representable.
fn position_at(code: &CodeObject, i: usize, firstlineno: u32) -> Position {
    if module_resume_at(code, i) {
        // CPython stamps a module's opening RESUME with the real
        // location (0, 1, 0, 0) — codegen_enter_anonymous_scope sets
        // loc.lineno = 0 for module scope (test_compile's
        // test_leading_newlines grades co_lines() starting at 0).
        return Position {
            lineno: 0,
            end_lineno: 1,
            col: Some(0),
            end_col: Some(0),
        };
    }
    let raw = code.linetable.get(i).copied().unwrap_or(firstlineno) as i32;
    let line = if raw == 0 { -1 } else { raw };
    // `col`/`end_col` are byte offsets (`-1` = unknown); `end_lineno`
    // is `0` when unknown (fall back to the start line).
    let cs = code.coltable.get(i).copied().unwrap_or_default();
    let end_lineno = if cs.end_lineno != 0 {
        cs.end_lineno as i32
    } else {
        line
    };
    Position {
        lineno: line,
        end_lineno,
        col: (cs.col >= 0).then_some(cs.col as u32),
        end_col: (cs.end_col >= 0).then_some(cs.end_col as u32),
    }
}

/// `(popped, pushed)` for a CPython 3.14 wire opcode
/// (`_PyOpcode_num_popped` / `_PyOpcode_num_pushed`, generated from
/// `bytecodes.c`). Only the opcodes `encode` can produce are listed.
fn cp_stack_shape(cp_op: u8, arg: u32) -> (usize, usize) {
    let a = arg as usize;
    match cp_op {
        op::BINARY_SLICE => (3, 1),
        op::BUILD_TEMPLATE => (2, 1),
        op::CALL_FUNCTION_EX => (4, 1),
        op::CHECK_EG_MATCH | op::CHECK_EXC_MATCH => (2, 2),
        op::CLEANUP_THROW => (3, 2),
        op::DELETE_SUBSCR => (2, 0),
        op::END_FOR => (1, 0),
        op::END_SEND => (2, 1),
        op::FORMAT_SIMPLE => (1, 1),
        op::FORMAT_WITH_SPEC => (2, 1),
        op::GET_AITER | op::GET_ITER | op::GET_YIELD_FROM_ITER | op::GET_AWAITABLE => (1, 1),
        op::GET_ANEXT | op::GET_LEN | op::MATCH_MAPPING | op::MATCH_SEQUENCE => (1, 2),
        op::LOAD_BUILD_CLASS | op::LOAD_LOCALS | op::PUSH_NULL | op::RETURN_GENERATOR => (0, 1),
        op::MAKE_FUNCTION => (1, 1),
        op::MATCH_KEYS => (2, 3),
        op::NOP | op::NOT_TAKEN | op::SETUP_ANNOTATIONS | op::RESUME | op::EXTENDED_ARG => (0, 0),
        op::POP_EXCEPT | op::POP_ITER | op::POP_TOP => (1, 0),
        op::PUSH_EXC_INFO => (1, 2),
        op::RETURN_VALUE => (1, 1),
        op::STORE_SLICE => (4, 0),
        op::STORE_SUBSCR => (3, 0),
        op::TO_BOOL | op::UNARY_INVERT | op::UNARY_NEGATIVE | op::UNARY_NOT => (1, 1),
        op::WITH_EXCEPT_START => (5, 6),
        op::BINARY_OP => (2, 1),
        op::BUILD_INTERPOLATION => (2 + (a & 1), 1),
        op::BUILD_LIST | op::BUILD_SET | op::BUILD_SLICE | op::BUILD_STRING | op::BUILD_TUPLE => {
            (a, 1)
        }
        op::BUILD_MAP => (a * 2, 1),
        op::CALL => (2 + a, 1),
        op::CALL_INTRINSIC_1 => (1, 1),
        op::CALL_INTRINSIC_2 => (2, 1),
        op::CALL_KW => (3 + a, 1),
        op::COMPARE_OP | op::CONTAINS_OP | op::IS_OP => (2, 1),
        op::CONVERT_VALUE => (1, 1),
        op::COPY => (a.max(1), a.max(1) + 1),
        op::COPY_FREE_VARS | op::MAKE_CELL => (0, 0),
        op::DELETE_ATTR => (1, 0),
        op::DELETE_DEREF | op::DELETE_FAST | op::DELETE_GLOBAL | op::DELETE_NAME => (0, 0),
        op::DICT_MERGE => (4 + a, 3 + a),
        op::DICT_UPDATE | op::LIST_APPEND | op::LIST_EXTEND | op::SET_ADD | op::SET_UPDATE => {
            (1 + a, a)
        }
        op::END_ASYNC_FOR => (2, 0),
        op::FOR_ITER => (1, 2),
        op::IMPORT_FROM => (1, 2),
        op::IMPORT_NAME => (2, 1),
        op::JUMP_BACKWARD | op::JUMP_BACKWARD_NO_INTERRUPT | op::JUMP_FORWARD => (0, 0),
        op::LOAD_ATTR => (1, 1 + (a & 1)),
        op::LOAD_COMMON_CONSTANT
        | op::LOAD_CONST
        | op::LOAD_DEREF
        | op::LOAD_FAST
        | op::LOAD_FAST_AND_CLEAR
        | op::LOAD_FAST_BORROW
        | op::LOAD_FAST_CHECK
        | op::LOAD_NAME
        | op::LOAD_SMALL_INT => (0, 1),
        op::LOAD_FAST_BORROW_LOAD_FAST_BORROW | op::LOAD_FAST_LOAD_FAST => (0, 2),
        op::LOAD_FROM_DICT_OR_DEREF | op::LOAD_FROM_DICT_OR_GLOBALS => (1, 1),
        op::LOAD_GLOBAL => (0, 1 + (a & 1)),
        op::LOAD_SPECIAL => (1, 2),
        op::LOAD_SUPER_ATTR => (3, 1 + (a & 1)),
        op::MAP_ADD => (2 + a, a),
        op::MATCH_CLASS => (3, 1),
        op::POP_JUMP_IF_FALSE
        | op::POP_JUMP_IF_NONE
        | op::POP_JUMP_IF_NOT_NONE
        | op::POP_JUMP_IF_TRUE => (1, 0),
        op::RAISE_VARARGS => (a, 0),
        op::RERAISE => (1 + a, a),
        op::SEND => (2, 2),
        op::SET_FUNCTION_ATTRIBUTE => (2, 1),
        op::STORE_ATTR => (2, 0),
        op::STORE_DEREF | op::STORE_FAST | op::STORE_GLOBAL | op::STORE_NAME => (1, 0),
        op::STORE_FAST_LOAD_FAST => (1, 1),
        op::STORE_FAST_STORE_FAST => (2, 0),
        op::SWAP => (a.max(2), a.max(2)),
        op::UNPACK_EX => (1, 1 + (a & 0xFF) + (a >> 8)),
        op::UNPACK_SEQUENCE => (1, a),
        op::YIELD_VALUE => (1, 1),
        _ => (0, 0),
    }
}

/// Whether instruction `i` is a module's opening `RESUME`, which
/// CPython locates at the synthetic (0, 1, 0, 0) span rather than
/// NO_LOCATION (compile.c sets `loc.lineno = 0` for module scope).
///
/// The flowgraph stamps that RESUME with
/// [`crate::flowgraph::MODULE_RESUME_LOCATION`], whose line the table
/// spells as its NO_LOCATION sentinel `0` and whose column span
/// `(1, 0, 0)` no NO_LOCATION instruction carries; `propagate_line_numbers`
/// may have spread it to the instructions after RESUME (an empty
/// module's `LOAD_CONST None; RETURN_VALUE`). The op-based test keeps a
/// table without column data (an AST compiled elsewhere) honest.
///
/// The opening `RESUME` isn't necessarily instruction 0: the flowgraph's
/// `insert_prefix_instructions` puts `MAKE_CELL`s ahead of it when the
/// module owns a cell (PEP 649's `__conditional_annotations__`), and the
/// top-level-await coroutine prefix (`RETURN_GENERATOR; POP_TOP`) does
/// the same. Only those prefix ops may precede it.
fn module_resume_at(code: &CodeObject, i: usize) -> bool {
    if code.name != "<module>" || code.linetable.get(i).copied().unwrap_or(1) != 0 {
        return false;
    }
    if code.coltable.get(i).copied() == Some(crate::flowgraph::MODULE_RESUME_LOCATION.col) {
        return true;
    }
    code.instructions.get(i).map(|x| x.op) == Some(OpCode::Resume)
        && code.instructions[..i].iter().all(|x| {
            matches!(
                x.op,
                OpCode::MakeCell | OpCode::CopyFreeVars | OpCode::ReturnGenerator | OpCode::PopTop
            )
        })
}

/// Read the raw instruction delta a WeavePy jump carries (its `arg`),
/// regardless of direction.
fn args_target_delta(ins: Instruction) -> usize {
    ins.arg as usize
}

// ---------- location table (PEP 626) ----------

/// Append `val` as a CPython location varint (little-endian 6-bit groups,
/// 0x40 continuation). The first byte is OR'd with `first_mask`.
fn push_loc_varint(out: &mut Vec<u8>, mut val: u32, first_mask: u8) {
    let mut first = true;
    loop {
        let mut b = (val & 0x3F) as u8;
        val >>= 6;
        if val != 0 {
            b |= 0x40;
        }
        if first {
            b |= first_mask;
            first = false;
        }
        out.push(b);
        if val == 0 {
            break;
        }
    }
}

fn push_loc_svarint(out: &mut Vec<u8>, val: i32, first_mask: u8) {
    let zig = if val < 0 {
        ((val.unsigned_abs()) << 1) | 1
    } else {
        (val as u32) << 1
    };
    push_loc_varint(out, zig, first_mask);
}

/// Encode the PEP 626/657 location table. Instructions with tracked
/// column spans use the "long" entry form (`code = 14`), preserving
/// PEP 657 fine-grained positions across the marshal round-trip
/// (traceback caret underlines from `.pyc`-loaded modules — doctest's
/// error-report tests compare them textually). Instructions without
/// columns keep the "no-column" form (`code = 13`).
#[allow(clippy::too_many_arguments)]
#[allow(clippy::too_many_arguments)]
/// PEP 626 / PEP 657 location table (`write_location_info_entry`):
/// one entry per instruction (all of its code units share the
/// location), split into chunks of at most 8 units.
fn encode_linetable(
    code: &CodeObject,
    ext: &[usize],
    mapped: &[MappedOp],
    zero_width: &[bool],
    firstlineno: u32,
) -> Vec<u8> {
    const CODE_NO_COLUMNS: u8 = 13;
    const CODE_LONG: u8 = 14;
    const CODE_NO_LOCATION: u8 = 15;
    let mut out = Vec::new();
    let mut prev_line = firstlineno as i32;
    for i in 0..code.instructions.len() {
        if zero_width[i] {
            continue;
        }
        let units = ext[i] + 1 + cache_entries(mapped[i].cp_op);
        let pos = position_at(code, i, firstlineno);
        let mut remaining = units;
        if pos.lineno < 0 {
            // NO_LOCATION: the entry form carries no line delta and
            // doesn't advance the running line.
            while remaining > 0 {
                let chunk = remaining.min(8);
                out.push(0x80 | (CODE_NO_LOCATION << 3) | ((chunk - 1) as u8));
                remaining -= chunk;
            }
            continue;
        }
        let line = pos.lineno;
        let end_line_delta = (pos.end_lineno - line).max(0) as u32;
        let mut delta = line - prev_line;
        while remaining > 0 {
            let chunk = remaining.min(8);
            match (pos.col, pos.end_col) {
                (Some(col), Some(end_col)) => {
                    out.push(0x80 | (CODE_LONG << 3) | ((chunk - 1) as u8));
                    push_loc_svarint(&mut out, delta, 0);
                    push_loc_varint(&mut out, end_line_delta, 0);
                    // Columns are stored +1 so `0` means "None" (locations.md).
                    push_loc_varint(&mut out, col + 1, 0);
                    push_loc_varint(&mut out, end_col + 1, 0);
                }
                _ => {
                    out.push(0x80 | (CODE_NO_COLUMNS << 3) | ((chunk - 1) as u8));
                    push_loc_svarint(&mut out, delta, 0);
                }
            }
            // Subsequent chunks of the same instruction repeat the line.
            delta = 0;
            remaining -= chunk;
        }
        prev_line = line;
    }
    out
}

// ---------- exception table ----------

/// Append `val` as a CPython exception-table varint (big-endian 6-bit
/// groups, 0x40 continuation). The first byte is OR'd with `first_mask`.
fn push_exc_varint(out: &mut Vec<u8>, val: u32, first_mask: u8) {
    // Collect 6-bit groups, most-significant first.
    let mut groups = [0u8; 6];
    let mut count = 0;
    let mut v = val;
    loop {
        groups[count] = (v & 0x3F) as u8;
        v >>= 6;
        count += 1;
        if v == 0 {
            break;
        }
    }
    for idx in (0..count).rev() {
        let mut b = groups[idx];
        if idx != 0 {
            b |= 0x40;
        }
        if idx == count - 1 {
            b |= first_mask;
        }
        out.push(b);
    }
}

/// Encode the exception range table the way `assemble_exception_table`
/// does: one owner per code unit, adjacent units with the same handler
/// merged into one entry. Every unit of an instruction takes the
/// instruction's entry (the flowgraph's per-instruction `i_except`,
/// already flattened into ranges by `flowgraph::flatten`).
fn encode_exception_table(code: &CodeObject, starts: &[usize]) -> Vec<u8> {
    let n = code.instructions.len();
    let total = starts.get(n).copied().unwrap_or(0);
    // (target unit, depth, lasti) per code unit.
    let mut owner: Vec<Option<(u32, u32, bool)>> = vec![None; total];
    for h in &code.exception_table {
        let target = starts.get(h.handler as usize).copied().unwrap_or(0) as u32;
        let k = (target, h.depth, h.push_lasti);
        for i in (h.start as usize)..(h.end as usize).min(n) {
            for u in starts[i]..starts[i + 1] {
                owner[u] = Some(k);
            }
        }
    }
    let mut out = Vec::new();
    let mut u = 0usize;
    while u < total {
        let Some(k) = owner[u] else {
            u += 1;
            continue;
        };
        let start = u;
        while u < total && owner[u] == Some(k) {
            u += 1;
        }
        // First byte of the entry is marked with 0x80.
        push_exc_varint(&mut out, start as u32, 0x80);
        push_exc_varint(&mut out, (u - start) as u32, 0);
        push_exc_varint(&mut out, k.0, 0);
        // depth_and_lasti = (depth << 1) | lasti.
        push_exc_varint(&mut out, (k.1 << 1) | u32::from(k.2), 0);
    }
    out
}

// ---------- stack size ----------

/// Maximum operand-stack depth via a CPython-style worklist walk over
/// the flat instruction stream (`_PyCfg_Stackdepth`). Each instruction
/// records the depth on entry; conditional jumps enqueue both edges and
/// exception handlers are seeded at their table depth plus the pushed
/// exception. Join points therefore see the *converged* depth instead
/// of the running sum a linear scan would accumulate — CPython's
/// test_compile TestExpressionStackSize/TestStackSizeStability grade
/// that `co_stacksize` is O(1) for chains like `x if x else …`.
/// Static entry-depth per instruction (the same worklist walk as
/// [`compute_stacksize`], returning the whole `startdepth` vector).
/// Used by `Compiler::finish` to resolve sentinel exception-table
/// depths for inlined comprehensions; entries the walk never reaches
/// stay -1. Handlers whose depth is still the sentinel do not seed
/// the walk (their depth is exactly what's being computed).
pub(crate) fn compute_startdepths(code: &CodeObject) -> Vec<i64> {
    use OpCode as O;
    let n = code.instructions.len();
    let mut startdepth: Vec<i64> = vec![-1; n];
    if n == 0 {
        return startdepth;
    }
    let mut worklist: Vec<usize> = Vec::new();
    let push = |i: usize, depth: i64, startdepth: &mut [i64], worklist: &mut Vec<usize>| {
        if i < n && depth > startdepth[i] {
            startdepth[i] = depth;
            worklist.push(i);
        }
    };
    push(0, 0, &mut startdepth, &mut worklist);
    for h in &code.exception_table {
        if h.depth & crate::HANDLER_DEPTH_ANCHOR_FLAG != 0 {
            continue;
        }
        // Handler entry: kept depth, plus the lasti offset when the
        // entry is flagged, plus the pushed exception (CPython 3.13's
        // on-stack discipline).
        push(
            h.handler as usize,
            i64::from(h.depth) + 1 + i64::from(h.push_lasti),
            &mut startdepth,
            &mut worklist,
        );
    }
    let budget = 64 * n + 1024;
    let mut guard = 0usize;
    // Outer fixpoint: a block reachable only through a *sentinel*
    // handler's exception edge (e.g. the END_ASYNC_FOR exit of an
    // inlined async comprehension — its loop body never falls
    // through) can't be seeded up front, because the handler's depth
    // is exactly what's being computed. Once a protected region's
    // start depth is known, its handler runs at that depth plus the
    // pushed exception; seed it and re-walk until nothing new appears.
    loop {
        while let Some(start) = worklist.pop() {
            guard += 1;
            if guard > budget {
                return startdepth;
            }
            let mut depth = startdepth[start];
            let mut i = start;
            loop {
                let ins = code.instructions[i];
                let (effect, jump) = stack_effects_at(code, i, ins);
                let from = i as u32 + 1;
                match ins.op {
                    O::JumpForward => {
                        push(
                            (from + ins.arg) as usize,
                            depth,
                            &mut startdepth,
                            &mut worklist,
                        );
                        break;
                    }
                    O::JumpBackward => {
                        push(
                            from.saturating_sub(ins.arg) as usize,
                            depth,
                            &mut startdepth,
                            &mut worklist,
                        );
                        break;
                    }
                    O::ReturnValue | O::RaiseVarargs | O::Reraise => break,
                    O::PopJumpIfFalse
                    | O::PopJumpIfTrue
                    | O::PopJumpIfNone
                    | O::PopJumpIfNotNone
                    | O::ForIter
                    | O::Send => {
                        push(
                            (from + ins.arg) as usize,
                            depth + jump,
                            &mut startdepth,
                            &mut worklist,
                        );
                    }
                    _ => {}
                }
                depth += effect;
                if depth < 0 {
                    depth = 0;
                }
                i += 1;
                if i >= n {
                    break;
                }
                if startdepth[i] >= depth {
                    break;
                }
                startdepth[i] = depth;
            }
        }
        let mut seeded = false;
        for h in &code.exception_table {
            if h.depth & crate::HANDLER_DEPTH_ANCHOR_FLAG == 0 {
                continue;
            }
            let at = if h.depth == crate::HANDLER_DEPTH_SENTINEL {
                h.start
            } else {
                h.depth & !crate::HANDLER_DEPTH_ANCHOR_FLAG
            };
            let s = startdepth.get(at as usize).copied().unwrap_or(-1);
            if s >= 0 {
                let handler = h.handler as usize;
                let entry = s + 1 + i64::from(h.push_lasti);
                if handler < n && entry > startdepth[handler] {
                    startdepth[handler] = entry;
                    worklist.push(handler);
                    seeded = true;
                }
            }
        }
        if !seeded {
            break;
        }
    }
    startdepth
}

fn compute_stacksize(code: &CodeObject) -> u32 {
    use OpCode as O;
    let n = code.instructions.len();
    if n == 0 {
        return 1;
    }
    let mut startdepth: Vec<i64> = vec![-1; n];
    let mut maxdepth: i64 = 1;
    let mut worklist: Vec<usize> = Vec::new();
    let push = |i: usize, depth: i64, startdepth: &mut [i64], worklist: &mut Vec<usize>| {
        if i < n && depth > startdepth[i] {
            startdepth[i] = depth;
            worklist.push(i);
        }
    };
    push(0, 0, &mut startdepth, &mut worklist);
    for h in &code.exception_table {
        // The VM truncates the stack to `depth` and pushes the exception.
        if h.depth & crate::HANDLER_DEPTH_ANCHOR_FLAG != 0 {
            continue;
        }
        // CPython's `calculate_stackdepth` counts the `SETUP_*`
        // pseudo-op's target depth toward `co_stacksize` even when the
        // handler's first instruction only pops.
        let entry_depth = i64::from(h.depth) + 1 + i64::from(h.push_lasti);
        maxdepth = maxdepth.max(entry_depth);
        push(
            h.handler as usize,
            entry_depth,
            &mut startdepth,
            &mut worklist,
        );
    }
    // Safety valve: the walk converges iff every cycle's modeled net
    // effect is ≤ 0. That holds for compiler-emitted streams, but
    // `encode` also sees foreign streams (`types.CodeType`, RFC 0060
    // assemble round-trips) whose shapes we don't control. Rather than
    // diverge, fall back to the conservative linear estimate.
    let budget = 64 * n + 1024;
    let mut guard = 0usize;
    while let Some(start) = worklist.pop() {
        guard += 1;
        if guard > budget {
            return linear_stacksize_estimate(code);
        }
        let mut depth = startdepth[start];
        let mut i = start;
        loop {
            let ins = code.instructions[i];
            let (effect, jump) = stack_effects_at(code, i, ins);
            let from = i as u32 + 1;
            match ins.op {
                O::JumpForward => {
                    push(
                        (from + ins.arg) as usize,
                        depth,
                        &mut startdepth,
                        &mut worklist,
                    );
                    break;
                }
                O::JumpBackward => {
                    push(
                        from.saturating_sub(ins.arg) as usize,
                        depth,
                        &mut startdepth,
                        &mut worklist,
                    );
                    break;
                }
                O::ReturnValue | O::RaiseVarargs | O::Reraise => break,
                O::PopJumpIfFalse
                | O::PopJumpIfTrue
                | O::PopJumpIfNone
                | O::PopJumpIfNotNone
                | O::ForIter
                | O::Send => {
                    let target_depth = depth + jump;
                    maxdepth = maxdepth.max(target_depth);
                    push(
                        (from + ins.arg) as usize,
                        target_depth,
                        &mut startdepth,
                        &mut worklist,
                    );
                }
                _ => {}
            }
            depth += effect;
            maxdepth = maxdepth.max(depth);
            if depth < 0 {
                depth = 0;
            }
            i += 1;
            if i >= n {
                break;
            }
            // Stop if the fallthrough has already been seen at >= depth.
            if startdepth[i] >= depth {
                break;
            }
            startdepth[i] = depth;
        }
    }
    u32::try_from(maxdepth).unwrap_or(u32::MAX)
}

/// Worst-case depth via the pre-worklist linear scan: accumulate
/// fallthrough effects, clamp at 0, take the running max. Never
/// underestimates a convergent stream's true need; used only as the
/// divergence fallback for foreign instruction streams.
fn linear_stacksize_estimate(code: &CodeObject) -> u32 {
    let mut depth: i64 = 0;
    let mut max: i64 = 1;
    for (i, ins) in code.instructions.iter().enumerate() {
        let (effect, _) = stack_effects_at(code, i, *ins);
        depth += effect;
        if depth < 0 {
            depth = 0;
        }
        max = max.max(depth);
    }
    u32::try_from(max).unwrap_or(u32::MAX)
}

/// [`stack_effects`] with stream context: `CallKw` additionally pops
/// one keyword value per entry of its kwnames tuple, which the
/// compiler always materializes as the immediately preceding
/// `LOAD_CONST` (see `compile_expr`'s keyword-call lowering). Without
/// the kwnames term a keyword call inside a loop models as a net
/// stack *gain*, and the worklist walk diverges.
fn stack_effects_at(code: &CodeObject, i: usize, ins: Instruction) -> (i64, i64) {
    let (mut effect, jump) = stack_effects(ins.op, ins.arg);
    if ins.op == OpCode::CallKw && i > 0 {
        let prev = code.instructions[i - 1];
        if prev.op == OpCode::LoadConst {
            if let Some(Constant::Tuple(names)) = code.constants.get(prev.arg as usize) {
                effect -= names.len() as i64;
            }
        }
    }
    (effect, jump)
}

/// `(fallthrough effect, jump effect)` for one instruction, matching
/// the WeavePy VM's exact pop/push behaviour. RFC 0068 WS1 adopted
/// CPython's on-stack exception discipline: `PUSH_EXC_INFO` inserts
/// the previous exception under TOS (+1) and `POP_EXCEPT` pops it
/// (-1); handlers flagged `lasti` also receive the offset as a real
/// stack slot (modeled at the seeding sites, not here). Calls carry
/// CPython's self-or-null slot, so the call family's effects match
/// the wire view.
/// Debug-only public view of [`stack_effects`] (used by the
/// `dbg_depths*` examples).
pub fn debug_stack_effects(opcode: OpCode, arg: u32) -> (i64, i64) {
    stack_effects(opcode, arg)
}

fn stack_effects(opcode: OpCode, arg: u32) -> (i64, i64) {
    use OpCode as O;
    let a = i64::from(arg);
    let e = match opcode {
        // Block-push pseudo-ops: the fallthrough effect is 0; the
        // handler-entry depth is modeled at the seeding sites.
        O::SetupFinally
        | O::SetupCleanup
        | O::SetupWith
        | O::PopBlock
        | O::Jump
        | O::JumpNoInterrupt
        | O::JumpIfFalse
        | O::JumpIfTrue
        | O::StoreFastLoadFast => 0,
        O::LoadGlobalPushNull | O::LoadFastLoadFast | O::LoadFastBorrowLoadFastBorrow => 2,
        O::StoreFastStoreFast => -2,
        O::StoreFastMaybeNull => -1,
        O::LoadConst
        | O::LoadName
        | O::LoadGlobal
        | O::LoadFast
        | O::LoadFastBorrow
        | O::LoadFastCheck
        | O::LoadFastAndClear
        | O::LoadDeref
        | O::LoadClosure
        | O::LoadClosureBorrow
        | O::LoadLocals
        | O::LoadBuildClass
        | O::LoadCommonConstant
        | O::LoadSmallInt
        // Pops the owner, pushes `method, self_or_null` (3.14's with
        // dance).
        | O::LoadSpecial
        | O::PushNull
        | O::LoadMethodAttr
        | O::CopyTop
        | O::MatchSequence
        | O::MatchMapping
        // `subject, keys -- subject, keys, values_or_none`.
        | O::MatchKeys
        | O::GetLen
        | O::GetAnext
        | O::ImportFrom
        | O::WithExceptStart
        // Every resume pushes the sent value; the prologue's POP_TOP
        // (first resume) or the dance's SEND consumes it.
        | O::ReturnGenerator
        // Inserts the previous exception under TOS (CPython 3.13).
        | O::PushExcInfo => 1,
        O::PopTop
        | O::PopIter
        | O::StoreName
        | O::StoreGlobal
        | O::StoreFast
        | O::StoreDeref
        | O::ReturnValue
        | O::PopJumpIfFalse
        | O::PopJumpIfTrue
        | O::PopJumpIfNone
        | O::PopJumpIfNotNone
        | O::PrintExpr
        | O::ImportName
        | O::DeleteAttr
        | O::BinaryOp
        | O::CompareOp
        | O::IsOp
        | O::ContainsOp
        | O::BinarySubscr
        | O::ListAppend
        | O::SetAdd
        | O::SetUpdate
        | O::ListExtend
        | O::DictUpdate
        | O::EndSend
        // Pops the attribute value from under the function, pushes the
        // function back: net -1.
        | O::SetFunctionAttribute
        // Pops the saved previous exception pushed by PUSH_EXC_INFO.
        | O::PopExcept
        // Pops [sub_iter, last_sent, exc], pushes [None, value].
        | O::CleanupThrow
        | O::PrepReraiseStar
        | O::CallIntrinsic2 => -1,
        O::StoreAttr | O::MatchClass | O::DeleteSubscr | O::MapAdd | O::EndAsyncFor => -2,
        // `start, stop[, step] -- slice`.
        O::BuildSlice => 1 - a,
        // `container, start, stop -- result`.
        O::BinarySlice => -2,
        // `value, container, start, stop --`.
        O::StoreSlice => -4,
        // Pops self, class, global_super; pushes the attribute plus a
        // null self slot when the method flag (bit 0) is set.
        O::LoadSuperAttr => (1 + (a & 1)) - 3,
        O::StoreSubscr => -3,
        // CPython's self-or-null convention: every call pops the
        // callable plus the self slot. `Call n` counts positionals
        // only (the slot holds NULL); `CallSelf n` counts the riding
        // self value among its `n`.
        O::Call => -a - 1,
        O::CallSelf => -a,
        O::CallKw => -a - 2,
        // `callable, self_or_null, args, kwargs_or_null -- result`
        // (3.14: the kwargs slot is always present).
        O::CallEx => -3,
        O::MakeFunction => -i64::from(arg.count_ones()),
        O::BuildList | O::BuildTuple | O::BuildSet | O::BuildString => 1 - a,
        O::BuildMap => 1 - 2 * a,
        O::UnpackSequence => a - 1,
        O::UnpackEx => i64::from((arg >> 8) & 0xFF) + i64::from(arg & 0xFF),
        O::FormatValue => {
            if arg & 0x04 != 0 {
                -1
            } else {
                0
            }
        }
        // Pops value + expression text (+ spec when bit 0 is set),
        // pushes the Interpolation.
        O::BuildInterpolation => {
            if arg & 1 != 0 {
                -2
            } else {
                -1
            }
        }
        // Pops the strings and interpolations tuples, pushes the Template.
        O::BuildTemplate => -1,
        O::RaiseVarargs => -a,
        // `values[oparg], exc -- values[oparg]`; terminal either way.
        O::Reraise => -1,
        O::ForIter => 1,
        // CPython's END_FOR pops the (statically modeled) next value; the
        // trailing POP_ITER then pops the iterator. The pair is dead at
        // runtime (the exhausted FOR_ITER jumps past it) but the static
        // walk passes through it, so the effects must telescope: the
        // FOR_ITER jump edge lands on END_FOR at depth+1.
        O::EndFor => -1,
        _ => 0,
    };
    let jump = match opcode {
        O::PopJumpIfFalse | O::PopJumpIfTrue | O::PopJumpIfNone | O::PopJumpIfNotNone => -1,
        // FOR_ITER's declared effect on the jump edge is `iter -- iter,
        // next` (the runtime pop happens as part of the skip-past jump).
        O::ForIter => 1,
        // SEND's exhausted branch lands on END_SEND with `[receiver,
        // value]` still intact (the VM keeps the receiver at sub-top on
        // both edges; END_SEND pops it) — the jump edge is stack-neutral
        // just like the fallthrough. Modeling it as -1 double-counted
        // the pop and under-resolved sentinel handler depths for
        // chained inlined async comprehensions (`[x async for … async
        // for …]` truncated away the inner aiter and underflowed).
        O::Send => 0,
        // Block pushes: the handler entry sees the pushed exception
        // (+ lasti for the cleanup forms).
        O::SetupFinally | O::SetupWith => 1,
        O::SetupCleanup => 2,
        _ => 0,
    };
    (e, jump)
}

// ---------- decoder ----------

/// A real (non-cache) instruction recovered from `co_code` during decode.
struct DecodedRaw {
    cp_op: u8,
    arg: u32,
    /// Code-unit offset where this instruction starts (incl. EXTENDED_ARGs).
    start_unit: usize,
    /// Total code units (EXTENDED_ARGs + op + caches).
    size: usize,
}

/// Split a `co_code` stream into real (non-cache) instructions, recording
/// each one's starting code-unit offset and total size (EXTENDED_ARGs +
/// op + caches). Shared by [`decode`] and [`decode_full`].
fn decode_raws(co_code: &[u8]) -> Vec<DecodedRaw> {
    let total_units = co_code.len() / 2;
    let mut raws: Vec<DecodedRaw> = Vec::new();
    let mut unit = 0usize;
    let mut pending_ext: u32 = 0;
    let mut ext_start: Option<usize> = None;
    while unit < total_units {
        let cp_op = co_code[unit * 2];
        let argbyte = u32::from(co_code[unit * 2 + 1]);
        if cp_op == op::EXTENDED_ARG {
            if ext_start.is_none() {
                ext_start = Some(unit);
            }
            pending_ext = (pending_ext << 8) | argbyte;
            unit += 1;
            continue;
        }
        if cp_op == op::CACHE {
            // A bare CACHE not following a real opcode: attach to previous.
            if let Some(last) = raws.last_mut() {
                last.size += 1;
            }
            unit += 1;
            continue;
        }
        let arg = (pending_ext << 8) | argbyte;
        let start = ext_start.unwrap_or(unit);
        let ncache = cache_entries(cp_op);
        raws.push(DecodedRaw {
            cp_op,
            arg,
            start_unit: start,
            size: (unit - start) + 1 + ncache,
        });
        unit += 1 + ncache;
        pending_ext = 0;
        ext_start = None;
    }
    raws
}

/// Build the code-unit-offset → raw-index map used for jump retargeting.
fn unit_index_map(raws: &[DecodedRaw]) -> std::collections::HashMap<usize, usize> {
    let mut unit_to_idx = std::collections::HashMap::new();
    for (idx, r) in raws.iter().enumerate() {
        unit_to_idx.insert(r.start_unit, idx);
    }
    unit_to_idx
}

/// Instruction count a raw expands to: superinstructions unfuse into
/// their two halves and a callable-flagged `LOAD_GLOBAL` into
/// `LoadGlobal` + `PushNull` (both marked as a fusion for the
/// re-encode); everything else is 1:1.
fn raw_expansion(cp_op: u8, arg: u32) -> usize {
    match cp_op {
        op::LOAD_FAST_LOAD_FAST
        | op::LOAD_FAST_BORROW_LOAD_FAST_BORROW
        | op::STORE_FAST_LOAD_FAST
        | op::STORE_FAST_STORE_FAST => 2,
        op::LOAD_GLOBAL if arg & 1 != 0 => 2,
        _ => 1,
    }
}

fn raw_expansions(raws: &[DecodedRaw]) -> Vec<usize> {
    raws.iter().map(|r| raw_expansion(r.cp_op, r.arg)).collect()
}

/// Per-raw index of its first WeavePy instruction (plus the total as a
/// final sentinel entry), from the per-raw expansion counts.
fn raw_first_instr(expansions: &[usize]) -> Vec<usize> {
    let mut first = Vec::with_capacity(expansions.len() + 1);
    let mut cur = 0usize;
    for &e in expansions {
        first.push(cur);
        cur += e;
    }
    first.push(cur);
    first
}

/// The decode-side inverse of [`DerefSlots`]: wire `localsplus` slot →
/// internal deref index (cellvars then freevars), plus which slots hold
/// cells/frees (their `LOAD_FAST` pushes the cell object itself —
/// WeavePy's `LoadClosure`).
#[derive(Debug)]
pub struct SlotMap {
    nlocals: u32,
    deref_of_slot: Vec<Option<u32>>,
}

impl SlotMap {
    /// Build from the wire `co_localspluskinds` bytes.
    #[must_use]
    pub fn from_kinds(kinds: &[u8]) -> Self {
        let nlocals = kinds.iter().filter(|k| *k & CO_FAST_LOCAL != 0).count() as u32;
        let mut deref_of_slot: Vec<Option<u32>> = vec![None; kinds.len()];
        let mut next = 0u32;
        for (i, &k) in kinds.iter().enumerate() {
            if k & CO_FAST_CELL != 0 {
                deref_of_slot[i] = Some(next);
                next += 1;
            }
        }
        for (i, &k) in kinds.iter().enumerate() {
            if k & CO_FAST_FREE != 0 {
                deref_of_slot[i] = Some(next);
                next += 1;
            }
        }
        Self {
            nlocals,
            deref_of_slot,
        }
    }

    /// Build from a code object's variable lists (mirrors
    /// [`DerefSlots::from_code`], inverted).
    #[must_use]
    pub fn from_code_vars(varnames: &[String], cellvars: &[String], freevars: &[String]) -> Self {
        let nlocals = varnames.len() as u32;
        let mut nslots = varnames.len() + freevars.len();
        for c in cellvars {
            if !varnames.contains(c) {
                nslots += 1;
            }
        }
        let mut deref_of_slot: Vec<Option<u32>> = vec![None; nslots];
        let mut next_slot = nlocals;
        for (i, c) in cellvars.iter().enumerate() {
            let slot = varnames.iter().position(|v| v == c).map_or_else(
                || {
                    let s = next_slot;
                    next_slot += 1;
                    s
                },
                |p| p as u32,
            );
            deref_of_slot[slot as usize] = Some(i as u32);
        }
        for j in 0..freevars.len() {
            deref_of_slot[(next_slot as usize) + j] = Some((cellvars.len() + j) as u32);
        }
        Self {
            nlocals,
            deref_of_slot,
        }
    }

    /// Internal deref index for a wire slot (falls back to the legacy
    /// `slot - nlocals` shift for out-of-range slots).
    fn deref(&self, slot: u32) -> u32 {
        self.deref_of_slot
            .get(slot as usize)
            .copied()
            .flatten()
            .unwrap_or_else(|| slot.saturating_sub(self.nlocals))
    }

    /// Does `slot` hold a cell or free variable (so `LOAD_FAST` on it
    /// pushes the cell object)?
    fn is_cellish(&self, slot: u32) -> bool {
        if let Some(d) = self.deref_of_slot.get(slot as usize) {
            d.is_some()
        } else {
            slot >= self.nlocals
        }
    }
}

/// Abstract origin of one shadow-stack slot during decode, used to
/// tell CPython's two CALL shapes apart: a NULL-style call (self slot
/// fed by PUSH_NULL / a flagged LOAD_GLOBAL / LOAD_ATTR method pair)
/// maps back to WeavePy's `Call n`; a self-slot call (decorator or
/// comprehension invocation — a real value in the slot) maps to
/// `CallSelf n+1`, whose first argument rides that slot.
#[derive(Clone, Copy, PartialEq)]
enum SlotKind {
    Null,
    Pair,
    Other,
    Unknown,
}

/// Decode the raw wire instructions into WeavePy instructions plus
/// their [`wire`] marks. Superinstructions and the callable-flagged
/// `LOAD_GLOBAL` unfuse into their two halves (marked head/tail);
/// `LOAD_FAST_BORROW`/`LOAD_FAST_CHECK` decode to marked `LoadFast`s.
/// The other reconstruction is the call shape: CPython's `CALL n` is
/// WeavePy's `Call n` when the self-or-null slot holds NULL and
/// `CallSelf n+1` when it holds a bound receiver, which a shadow stack
/// of slot kinds tracks within each basic block.
// Index-driven on purpose: the walk reads `raws[idx]` while consulting
// its neighbours and the parallel `first` table.
#[allow(clippy::needless_range_loop)]
fn decode_instructions(
    raws: &[DecodedRaw],
    slots: &SlotMap,
    constants: &[Constant],
) -> Option<(Vec<Instruction>, Vec<u8>)> {
    let unit_to_idx = unit_index_map(raws);
    let expansions = raw_expansions(raws);
    let first = raw_first_instr(&expansions);
    let total = *first.last().unwrap_or(&0);
    let instr_of_raw = |raw_idx: usize| -> usize { first.get(raw_idx).copied().unwrap_or(total) };
    // Jump-target units: shadow-stack knowledge resets there (slots
    // reached from other paths are unknown; unknown self slots decode
    // as NULL-style calls, which is what every compiler-produced
    // cross-block call shape actually is).
    let mut leaders: std::collections::HashSet<usize> = std::collections::HashSet::new();
    for r in raws {
        if is_rel_jump(r.cp_op) {
            let next_unit = r.start_unit + r.size;
            let t = if is_backward_jump(r.cp_op) {
                next_unit.saturating_sub(r.arg as usize)
            } else {
                next_unit + r.arg as usize
            };
            leaders.insert(t);
        }
    }
    let mut shadow: Vec<SlotKind> = Vec::new();
    let mut out = Vec::with_capacity(total);
    let mut marks: Vec<u8> = Vec::with_capacity(total);
    for (idx, r) in raws.iter().enumerate() {
        if leaders.contains(&r.start_unit) {
            shadow.clear();
        }
        // Fused wire forms first: they expand to two marked
        // instructions and drive the shadow stack directly.
        match r.cp_op {
            op::LOAD_GLOBAL if r.arg & 1 != 0 => {
                shadow.push(SlotKind::Other);
                shadow.push(SlotKind::Null);
                out.push(Instruction::new(OpCode::LoadGlobal, r.arg >> 1));
                marks.push(wire::FUSE_HEAD);
                out.push(Instruction::new(OpCode::PushNull, 0));
                marks.push(wire::FUSE_TAIL);
                continue;
            }
            op::LOAD_FAST_LOAD_FAST
            | op::LOAD_FAST_BORROW_LOAD_FAST_BORROW
            | op::STORE_FAST_LOAD_FAST
            | op::STORE_FAST_STORE_FAST => {
                // Args packed 4 bits each; both are always true locals,
                // never closures.
                let (a1, a2) = (r.arg >> 4, r.arg & 0x0F);
                let (op1, op2, extra) = match r.cp_op {
                    op::LOAD_FAST_LOAD_FAST => (OpCode::LoadFast, OpCode::LoadFast, wire::PLAIN),
                    op::LOAD_FAST_BORROW_LOAD_FAST_BORROW => {
                        (OpCode::LoadFast, OpCode::LoadFast, wire::BORROW)
                    }
                    op::STORE_FAST_LOAD_FAST => (OpCode::StoreFast, OpCode::LoadFast, wire::PLAIN),
                    _ => (OpCode::StoreFast, OpCode::StoreFast, wire::PLAIN),
                };
                for o in [op1, op2] {
                    if o == OpCode::LoadFast {
                        shadow.push(SlotKind::Other);
                    } else {
                        pop_slot(&mut shadow);
                    }
                }
                out.push(Instruction::new(op1, a1));
                marks.push(wire::FUSE_HEAD | extra);
                out.push(Instruction::new(op2, a2));
                marks.push(wire::FUSE_TAIL);
                continue;
            }
            _ => {}
        }
        let mark = match r.cp_op {
            // On a cell slot this decodes to `LoadClosure`; the mark
            // still records the borrow so re-encoding round-trips.
            op::LOAD_FAST_BORROW => wire::BORROW,
            op::LOAD_FAST_CHECK => wire::CHECK,
            _ => wire::PLAIN,
        };
        // Update the shadow stack (kinds only matter for the
        // call-shape decisions below; generic ops apply their net
        // effect, which keeps slot positions honest because nothing
        // but a call ever consumes a NULL/pair slot).
        let mut popped_self = SlotKind::Unknown;
        let pop = |shadow: &mut Vec<SlotKind>| shadow.pop().unwrap_or(SlotKind::Unknown);
        let kw_names_len = |out: &[Instruction]| -> u32 {
            match out.last() {
                Some(prev) if prev.op == OpCode::LoadConst => {
                    match constants.get(prev.arg as usize) {
                        Some(Constant::Tuple(names)) => names.len() as u32,
                        _ => 0,
                    }
                }
                _ => 0,
            }
        };
        let mut call_kw_internal_arg = 0u32;
        match r.cp_op {
            op::PUSH_NULL => shadow.push(SlotKind::Null),
            op::LOAD_GLOBAL => {
                shadow.push(SlotKind::Other);
                if r.arg & 1 != 0 {
                    shadow.push(SlotKind::Null);
                }
            }
            // LOAD_SPECIAL is always the method-form pair
            // (`__exit__`/`__enter__` + self-or-null), so the `CALL 0`
            // that follows is a NULL-style call.
            op::LOAD_SPECIAL => {
                pop(&mut shadow);
                shadow.push(SlotKind::Other);
                shadow.push(SlotKind::Pair);
            }
            op::LOAD_ATTR if r.arg & 1 != 0 => {
                pop(&mut shadow);
                shadow.push(SlotKind::Other);
                shadow.push(SlotKind::Pair);
            }
            op::CALL => {
                for _ in 0..r.arg {
                    pop(&mut shadow);
                }
                popped_self = pop(&mut shadow);
                pop(&mut shadow); // callable
                shadow.push(SlotKind::Other);
            }
            op::CALL_KW => {
                let nkw = kw_names_len(&out);
                call_kw_internal_arg = r.arg.saturating_sub(nkw);
                pop(&mut shadow); // kwnames tuple
                for _ in 0..r.arg {
                    pop(&mut shadow);
                }
                popped_self = pop(&mut shadow);
                pop(&mut shadow); // callable
                shadow.push(SlotKind::Other);
            }
            op::CALL_FUNCTION_EX => {
                pop(&mut shadow); // kwargs dict or NULL
                pop(&mut shadow); // args tuple
                pop(&mut shadow); // self-or-null (always NULL here)
                pop(&mut shadow); // callable
                shadow.push(SlotKind::Other);
            }
            // The 3.14 `with` prologue shuffles the `__exit__` pair
            // under the manager with SWAP 2 / SWAP 3, so slot kinds
            // have to move with the values for the exit `CALL 3` to
            // decode as the NULL-style call it is.
            op::SWAP if r.arg >= 2 => {
                let n = r.arg as usize;
                let len = shadow.len();
                if len >= n {
                    shadow.swap(len - 1, len - n);
                } else {
                    shadow.clear();
                }
            }
            op::COPY if r.arg >= 1 => {
                let n = r.arg as usize;
                let len = shadow.len();
                let k = if len >= n {
                    shadow[len - n]
                } else {
                    SlotKind::Unknown
                };
                shadow.push(k);
            }
            _ => {
                let (popped, pushed) = cp_stack_shape(r.cp_op, r.arg);
                for _ in 0..popped {
                    pop(&mut shadow);
                }
                for _ in 0..pushed {
                    shadow.push(SlotKind::Other);
                }
            }
        }
        if matches!(
            r.cp_op,
            op::JUMP_FORWARD
                | op::JUMP_BACKWARD
                | op::JUMP_BACKWARD_NO_INTERRUPT
                | op::RETURN_VALUE
                | op::RERAISE
                | op::RAISE_VARARGS
        ) {
            // Whatever follows starts a fresh block.
            shadow.clear();
        }
        marks.push(mark);
        if r.cp_op == op::CALL {
            let internal = if popped_self == SlotKind::Other {
                Instruction::new(OpCode::CallSelf, r.arg + 1)
            } else {
                Instruction::new(OpCode::Call, r.arg)
            };
            out.push(internal);
            continue;
        }
        if r.cp_op == op::CALL_KW {
            out.push(Instruction::new(OpCode::CallKw, call_kw_internal_arg));
            continue;
        }
        let op = map_from_cpython(r.cp_op, r.arg, slots)?;
        let self_idx = instr_of_raw(idx);
        let arg = if is_rel_jump(r.cp_op) {
            let next_unit = r.start_unit + r.size;
            let target_unit = if is_backward_jump(r.cp_op) {
                next_unit.saturating_sub(r.arg as usize)
            } else {
                next_unit + r.arg as usize
            };
            let target_raw = *unit_to_idx.get(&target_unit).unwrap_or(&raws.len());
            let target_idx = instr_of_raw(target_raw);
            if is_backward_jump(r.cp_op) {
                (self_idx + 1).saturating_sub(target_idx) as u32
            } else {
                target_idx.saturating_sub(self_idx + 1) as u32
            }
        } else if r.cp_op == op::PUSH_EXC_INFO && r.arg != 0 {
            // Inverse of the encoder's handler-body tag: absolute code
            // unit → instruction index (see the encode fixpoint loop).
            let raw_idx = *unit_to_idx.get(&(r.arg as usize)).unwrap_or(&raws.len());
            instr_of_raw(raw_idx) as u32
        } else {
            op.1
        };
        out.push(Instruction::new(op.0, arg));
    }
    debug_assert_eq!(out.len(), marks.len());
    Some((out, marks))
}

fn pop_slot(shadow: &mut Vec<SlotKind>) -> SlotKind {
    shadow.pop().unwrap_or(SlotKind::Unknown)
}

/// Is `cp_op` outside CPython 3.14's known opcode set (including the
/// specialized adaptive forms)? `ceval` raises `SystemError: unknown
/// opcode N` when it reaches one (test_code.test_invalid_bytecode).
/// The unknown ranges come from `_opcode_metadata.opmap` ∪
/// `_specialized_opmap`: every byte outside them is a real opcode.
#[must_use]
pub fn is_unknown_opcode(cp_op: u8) -> bool {
    matches!(cp_op, 121..=127 | 212..=233)
}

/// First unknown opcode byte in a raw `co_code` stream, if any. Even
/// offsets are always opcode bytes in the 3.14 wire format (`CACHE`
/// filler included), so a simple stride-2 scan suffices.
#[must_use]
pub fn first_unknown_opcode(co_code: &[u8]) -> Option<u8> {
    co_code
        .iter()
        .step_by(2)
        .copied()
        .find(|&o| is_unknown_opcode(o))
}

/// Decode a CPython-3.14 `co_code` stream back into WeavePy instructions.
/// Inverts [`encode`] for the canonical opcode set WeavePy emits.
/// `slots` maps wire localsplus slots back to internal deref indices.
///
/// Returns `None` if the stream contains an opcode WeavePy can't map back.
#[must_use]
pub fn decode(co_code: &[u8], slots: &SlotMap, constants: &[Constant]) -> Option<Vec<Instruction>> {
    let raws = decode_raws(co_code);
    decode_instructions(&raws, slots, constants).map(|(instructions, _)| instructions)
}

/// The reconstructed pieces of a [`CodeObject`] recovered from its
/// CPython-3.14 wire form (RFC 0033). Constants, names, arg counts, and
/// flags live outside this struct because they round-trip through
/// `marshal` directly; everything here is derived from the byte tables.
#[derive(Debug, Clone, Default)]
pub struct DecodedCode {
    pub instructions: Vec<Instruction>,
    pub linetable: Vec<u32>,
    /// PEP 657 column spans recovered from long-form location entries;
    /// same length as `instructions` (default sentinel where the table
    /// carried no columns).
    pub coltable: Vec<crate::ColSpan>,
    pub exception_table: Vec<ExcHandler>,
    pub varnames: Vec<String>,
    pub cellvars: Vec<String>,
    pub freevars: Vec<String>,
    /// Locals flagged `CO_FAST_HIDDEN` (see
    /// [`crate::CodeObject::hidden_locals`]).
    pub hidden_locals: Vec<String>,
    /// Instruction indices decoded from `JUMP_BACKWARD_NO_INTERRUPT`
    /// (see [`crate::CodeObject::no_interrupt_jumps`]); preserved so a
    /// re-encode round-trips the wire byte-for-byte.
    pub no_interrupt_jumps: Vec<u32>,
    /// One [`wire`] mark per decoded instruction (see
    /// [`crate::CodeObject::wire_marks`]); empty when all plain.
    pub wire_marks: Vec<u8>,
}

/// Invert [`encode`]: reconstruct the byte-table-derived parts of a
/// [`CodeObject`] from its wire form. Returns `None` if `co_code` holds an
/// opcode WeavePy can't map back (the caller then recompiles from source).
#[must_use]
pub fn decode_full(
    co_code: &[u8],
    co_linetable: &[u8],
    co_exceptiontable: &[u8],
    localsplusnames: &[String],
    localspluskinds: &[u8],
    firstlineno: u32,
    constants: &[Constant],
) -> Option<DecodedCode> {
    let mut varnames = Vec::new();
    let mut cellvars = Vec::new();
    let mut freevars = Vec::new();
    let mut hidden_locals = Vec::new();
    for (name, &kind) in localsplusnames.iter().zip(localspluskinds.iter()) {
        // An escaping parameter carries LOCAL|CELL on one shared slot:
        // it belongs to *both* co_varnames and co_cellvars.
        if kind & CO_FAST_LOCAL != 0 {
            varnames.push(name.clone());
            if kind & CO_FAST_HIDDEN != 0 {
                hidden_locals.push(name.clone());
            }
        }
        if kind & CO_FAST_CELL != 0 {
            cellvars.push(name.clone());
        }
        if kind & CO_FAST_FREE != 0 {
            freevars.push(name.clone());
        }
    }
    let slots = SlotMap::from_kinds(localspluskinds);
    let raws = decode_raws(co_code);
    let (instructions, mut wire_marks) = decode_instructions(&raws, &slots, constants)?;
    if wire_marks.iter().all(|&m| m == wire::PLAIN) {
        wire_marks.clear();
    }
    let (linetable, coltable) = decode_linetable(co_linetable, &raws, firstlineno);
    let exception_table = decode_exception_table(co_exceptiontable, &raws);
    // Recover the NO_INTERRUPT flag per decoded instruction index (the
    // internal stream folds both backward jumps into one opcode).
    let first = raw_first_instr(&raw_expansions(&raws));
    let no_interrupt_jumps: Vec<u32> = raws
        .iter()
        .enumerate()
        .filter(|(_, r)| r.cp_op == op::JUMP_BACKWARD_NO_INTERRUPT)
        .map(|(idx, _)| first[idx] as u32)
        .collect();
    Some(DecodedCode {
        instructions,
        linetable,
        coltable,
        exception_table,
        varnames,
        cellvars,
        freevars,
        hidden_locals,
        no_interrupt_jumps,
        wire_marks,
    })
}

// ---------- location-table decoder (inverse of `encode_linetable`) ----------

/// Read one unsigned location varint (little-endian 6-bit groups, 0x40
/// continuation). Advances `pos`.
fn read_loc_varint(table: &[u8], pos: &mut usize) -> u32 {
    let mut val = 0u32;
    let mut shift = 0u32;
    while *pos < table.len() {
        let b = table[*pos];
        *pos += 1;
        val |= u32::from(b & 0x3F) << shift;
        shift += 6;
        if b & 0x40 == 0 {
            break;
        }
    }
    val
}

/// Read one signed (zig-zag) location varint.
fn read_loc_svarint(table: &[u8], pos: &mut usize) -> i32 {
    let v = read_loc_varint(table, pos);
    if v & 1 != 0 {
        -((v >> 1) as i32)
    } else {
        (v >> 1) as i32
    }
}

/// Decode the PEP 626/657 location table into a 1-based source line (and,
/// where the entry form carries them, PEP 657 column spans) per WeavePy
/// instruction. WeavePy emits forms 13 and 14, but we tolerate the other
/// CPython forms so a table written by CPython still parses without desync.
fn decode_linetable(
    table: &[u8],
    raws: &[DecodedRaw],
    firstlineno: u32,
) -> (Vec<u32>, Vec<crate::ColSpan>) {
    let mut unit_lines: Vec<u32> = Vec::new();
    let mut unit_cols: Vec<crate::ColSpan> = Vec::new();
    let mut pos = 0usize;
    let mut line = firstlineno as i32;
    while pos < table.len() {
        let first = table[pos];
        pos += 1;
        if first & 0x80 == 0 {
            break;
        }
        let code = (first >> 3) & 0x0F;
        let length = ((first & 0x07) as usize) + 1;
        if code == 15 {
            // NONE — no location. Units decode to the 0 sentinel and
            // the running line is unchanged.
            unit_lines.extend(std::iter::repeat_n(0, length));
            unit_cols.extend(std::iter::repeat_n(crate::ColSpan::default(), length));
            continue;
        }
        let (delta, span) = match code {
            13 => (read_loc_svarint(table, &mut pos), crate::ColSpan::default()),
            14 => {
                let d = read_loc_svarint(table, &mut pos);
                let end_line_delta = read_loc_varint(table, &mut pos);
                // Columns are stored +1; `0` means "None".
                let col = read_loc_varint(table, &mut pos);
                let end_col = read_loc_varint(table, &mut pos);
                let span = crate::ColSpan {
                    end_lineno: (line + d + end_line_delta as i32).max(0) as u32,
                    col: col as i32 - 1,
                    end_col: end_col as i32 - 1,
                };
                (d, span)
            }
            10..=12 => {
                let d = i32::from(code) - 10;
                let col = read_loc_varint(table, &mut pos);
                let end_col = read_loc_varint(table, &mut pos);
                let span = crate::ColSpan {
                    end_lineno: (line + d).max(0) as u32,
                    col: col as i32,
                    end_col: end_col as i32,
                };
                (d, span)
            }
            _ => {
                // Short forms 0..=9: one extra byte packs the columns,
                // line delta 0 (locations.md "short form").
                let b = table.get(pos).copied().unwrap_or(0);
                pos += 1;
                let col = (u32::from(code) << 3) | u32::from(b >> 4);
                let span = crate::ColSpan {
                    end_lineno: line.max(0) as u32,
                    col: col as i32,
                    end_col: (col + u32::from(b & 0x0F)) as i32,
                };
                (0, span)
            }
        };
        line += delta;
        for _ in 0..length {
            unit_lines.push(line.max(0) as u32);
            unit_cols.push(span);
        }
    }
    let mut lines = Vec::with_capacity(raws.len());
    let mut cols = Vec::with_capacity(raws.len());
    for r in raws {
        let line = unit_lines.get(r.start_unit).copied().unwrap_or(firstlineno);
        let col = unit_cols.get(r.start_unit).copied().unwrap_or_default();
        // Both halves of an unfused superinstruction share its location.
        for _ in 0..raw_expansion(r.cp_op, r.arg) {
            lines.push(line);
            cols.push(col);
        }
    }
    (lines, cols)
}

// ---------- exception-table decoder (inverse of `encode_exception_table`) -----

/// Read one big-endian exception-table varint (0x40 continuation). The
/// 0x80 entry-start marker on the first byte is ignored (masked away).
fn read_exc_field(table: &[u8], pos: &mut usize) -> u32 {
    let mut val = 0u32;
    while *pos < table.len() {
        let b = table[*pos];
        *pos += 1;
        val = (val << 6) | u32::from(b & 0x3F);
        if b & 0x40 == 0 {
            break;
        }
    }
    val
}

/// Decode the exception range table back into [`ExcHandler`]s,
/// converting code-unit offsets to WeavePy instruction indices. The
/// assembler merges adjacent same-handler units into one entry, and
/// the codec is 1:1, so each wire entry is exactly one handler range.
fn decode_exception_table(table: &[u8], raws: &[DecodedRaw]) -> Vec<ExcHandler> {
    let unit_to_idx = unit_index_map(raws);
    let first = raw_first_instr(&raw_expansions(raws));
    let total = *first.last().unwrap_or(&0) as u32;
    let map_unit = |unit: usize| -> u32 {
        unit_to_idx
            .get(&unit)
            .and_then(|i| first.get(*i))
            .map_or(total, |i| *i as u32)
    };
    let mut out: Vec<ExcHandler> = Vec::new();
    let mut pos = 0usize;
    while pos < table.len() {
        let start_unit = read_exc_field(table, &mut pos) as usize;
        if pos >= table.len() {
            break;
        }
        let length = read_exc_field(table, &mut pos) as usize;
        let target_unit = read_exc_field(table, &mut pos) as usize;
        let dl = read_exc_field(table, &mut pos);
        let h = ExcHandler {
            start: map_unit(start_unit),
            end: map_unit(start_unit + length),
            handler: map_unit(target_unit),
            depth: dl >> 1,
            // Low bit of the depth/lasti word is CPython's lasti flag.
            push_lasti: dl & 1 != 0,
        };
        if h.start < h.end {
            out.push(h);
        }
    }
    out
}

/// Map a CPython opcode + arg back to a WeavePy `(OpCode, arg)`. The arg
/// is the WeavePy-domain arg for non-jumps; jump args are recomputed by
/// the caller.
fn map_from_cpython(cp_op: u8, arg: u32, slots: &SlotMap) -> Option<(OpCode, u32)> {
    use OpCode as O;
    let pair = match cp_op {
        op::NOP => (O::Nop, 0),
        op::RESUME => (O::Resume, arg),
        op::LOAD_CONST => (O::LoadConst, arg),
        op::LOAD_NAME => (O::LoadName, arg),
        op::LOAD_GLOBAL => (O::LoadGlobal, arg >> 1),
        // The borrowing and checked forms are wire marks on a plain
        // `LoadFast` (`decode_instructions` records them). On a
        // cell/free slot LOAD_FAST pushes the cell object itself
        // (closure building) — WeavePy's LoadClosure.
        op::LOAD_FAST | op::LOAD_FAST_BORROW | op::LOAD_FAST_CHECK => {
            if slots.is_cellish(arg) {
                (O::LoadClosure, slots.deref(arg))
            } else {
                (O::LoadFast, arg)
            }
        }
        op::LOAD_LOCALS => (O::LoadLocals, 0),
        op::LOAD_FAST_AND_CLEAR => (O::LoadFastAndClear, arg),
        op::STORE_FAST => (O::StoreFast, arg),
        op::STORE_GLOBAL => (O::StoreGlobal, arg),
        op::STORE_NAME => (O::StoreName, arg),
        op::DELETE_FAST => (O::DeleteFast, arg),
        op::DELETE_GLOBAL => (O::DeleteGlobal, arg),
        op::DELETE_NAME => (O::DeleteName, arg),
        op::LOAD_DEREF => (O::LoadDeref, slots.deref(arg)),
        op::STORE_DEREF => (O::StoreDeref, slots.deref(arg)),
        op::DELETE_DEREF => (O::DeleteDeref, slots.deref(arg)),
        op::MAKE_CELL => (O::MakeCell, slots.deref(arg)),
        op::COPY_FREE_VARS => (O::CopyFreeVars, arg),
        op::LOAD_ATTR if arg & 1 != 0 => (O::LoadMethodAttr, arg >> 1),
        op::LOAD_ATTR => (O::LoadAttr, arg >> 1),
        op::LOAD_SUPER_ATTR => (O::LoadSuperAttr, arg),
        op::STORE_ATTR => (O::StoreAttr, arg),
        op::DELETE_ATTR => (O::DeleteAttr, arg),
        op::BINARY_SLICE => (O::BinarySlice, 0),
        op::STORE_SLICE => (O::StoreSlice, 0),
        op::STORE_SUBSCR => (O::StoreSubscr, 0),
        op::DELETE_SUBSCR => (O::DeleteSubscr, 0),
        op::BINARY_OP if arg == NB_SUBSCR => (O::BinarySubscr, 0),
        op::BINARY_OP => {
            let (nb, flag) = if arg >= NB_INPLACE_OFFSET {
                (
                    arg - NB_INPLACE_OFFSET,
                    crate::bytecode::BINARY_OP_INPLACE_FLAG,
                )
            } else {
                (arg, 0)
            };
            (O::BinaryOp, nb_to_binop(nb)?.as_arg() | flag)
        }
        op::UNARY_NEGATIVE => (O::UnaryOp, UnaryKind::Neg.as_arg()),
        op::UNARY_NOT => (O::UnaryOp, UnaryKind::Not.as_arg()),
        op::UNARY_INVERT => (O::UnaryOp, UnaryKind::Invert.as_arg()),
        op::CALL_INTRINSIC_1 => {
            if arg == INTRINSIC_UNARY_POSITIVE {
                (O::UnaryOp, UnaryKind::Pos.as_arg())
            } else if arg == INTRINSIC_LIST_TO_TUPLE {
                (O::ListToTuple, 0)
            } else if arg == INTRINSIC_STOPITERATION_ERROR {
                (O::StopIterationError, 0)
            } else if arg == INTRINSIC_ASYNC_GEN_WRAP {
                (O::AsyncGenWrap, 0)
            } else if arg == INTRINSIC_IMPORT_STAR {
                (O::ImportStar, 0)
            } else {
                (O::CallIntrinsic1, arg)
            }
        }
        op::CALL_INTRINSIC_2 if arg == INTRINSIC_PREP_RERAISE_STAR => (O::PrepReraiseStar, 0),
        op::CALL_INTRINSIC_2 => (O::CallIntrinsic2, arg),
        op::CLEANUP_THROW => (O::CleanupThrow, 0),
        op::BUILD_INTERPOLATION => (O::BuildInterpolation, arg),
        op::BUILD_TEMPLATE => (O::BuildTemplate, 0),
        op::LOAD_COMMON_CONSTANT => (O::LoadCommonConstant, arg),
        op::LOAD_SPECIAL => (O::LoadSpecial, arg),
        op::LOAD_SMALL_INT => (O::LoadSmallInt, arg),
        op::NOT_TAKEN => (O::NotTaken, 0),
        op::POP_ITER => (O::PopIter, 0),
        op::COMPARE_OP => (
            O::CompareOp,
            CompareKind::from_arg(arg >> 5)?.as_arg() | (arg & COMPARE_OP_TO_BOOL_FLAG),
        ),
        op::IS_OP => (O::IsOp, arg),
        op::CONTAINS_OP => (O::ContainsOp, arg),
        op::POP_TOP => (O::PopTop, 0),
        op::COPY => (O::CopyTop, arg),
        op::SWAP => (O::Swap, arg),
        op::CALL => (O::Call, arg),
        op::CALL_KW => (O::CallKw, arg),
        op::CALL_FUNCTION_EX => (O::CallEx, 0),
        op::PUSH_NULL => (O::PushNull, 0),
        op::RETURN_VALUE => (O::ReturnValue, 0),
        op::POP_JUMP_IF_FALSE => (O::PopJumpIfFalse, arg),
        op::POP_JUMP_IF_TRUE => (O::PopJumpIfTrue, arg),
        op::POP_JUMP_IF_NONE => (O::PopJumpIfNone, arg),
        op::POP_JUMP_IF_NOT_NONE => (O::PopJumpIfNotNone, arg),
        op::JUMP_FORWARD => (O::JumpForward, arg),
        op::JUMP_BACKWARD | op::JUMP_BACKWARD_NO_INTERRUPT => (O::JumpBackward, arg),
        op::GET_ITER => (O::GetIter, 0),
        op::FOR_ITER => (O::ForIter, arg),
        op::END_FOR => (O::EndFor, 0),
        op::BUILD_LIST => (O::BuildList, arg),
        op::BUILD_TUPLE => (O::BuildTuple, arg),
        op::BUILD_SET => (O::BuildSet, arg),
        op::BUILD_MAP => (O::BuildMap, arg),
        op::SETUP_ANNOTATIONS => (O::SetupAnnotations, 0),
        op::BUILD_STRING => (O::BuildString, arg),
        op::LIST_APPEND => (O::ListAppend, arg),
        op::LIST_EXTEND => (O::ListExtend, arg),
        op::SET_ADD => (O::SetAdd, arg),
        op::SET_UPDATE => (O::SetUpdate, arg),
        op::MAP_ADD => (O::MapAdd, arg),
        op::UNPACK_SEQUENCE => (O::UnpackSequence, arg),
        op::UNPACK_EX => (O::UnpackEx, ((arg & 0xFF) << 8) | ((arg >> 8) & 0xFF)),
        op::DICT_UPDATE => (O::DictUpdate, arg.saturating_sub(1) << 1),
        op::DICT_MERGE => (O::DictUpdate, (arg.saturating_sub(1) << 1) | 1),
        op::MAKE_FUNCTION => (O::MakeFunction, arg),
        op::SET_FUNCTION_ATTRIBUTE => (O::SetFunctionAttribute, arg),
        op::BUILD_SLICE => (O::BuildSlice, arg),
        op::LOAD_BUILD_CLASS => (O::LoadBuildClass, 0),
        op::LOAD_FROM_DICT_OR_DEREF => (O::LoadClassdictOrDeref, slots.deref(arg)),
        op::LOAD_FROM_DICT_OR_GLOBALS => (O::LoadClassdictOrGlobal, arg),
        op::RAISE_VARARGS => (O::RaiseVarargs, arg),
        op::CHECK_EXC_MATCH => (O::CheckExcMatch, 0),
        op::CHECK_EG_MATCH => (O::CheckEGMatch, 0),
        op::PUSH_EXC_INFO => (O::PushExcInfo, 0),
        op::POP_EXCEPT => (O::PopExcept, 0),
        op::RERAISE => (O::Reraise, arg),
        op::WITH_EXCEPT_START => (O::WithExceptStart, 0),
        op::IMPORT_NAME => (O::ImportName, arg),
        op::IMPORT_FROM => (O::ImportFrom, arg),
        op::FORMAT_SIMPLE => (O::FormatValue, 0),
        op::FORMAT_WITH_SPEC => (O::FormatValue, 0x04),
        op::CONVERT_VALUE => (O::ConvertValue, arg),
        op::TO_BOOL => (O::ToBool, 0),
        op::YIELD_VALUE => (O::YieldValue, arg),
        op::GET_YIELD_FROM_ITER => (O::GetYieldFromIter, 0),
        op::RETURN_GENERATOR => (O::ReturnGenerator, 0),
        op::SEND => (O::Send, arg),
        op::END_SEND => (O::EndSend, 0),
        op::GET_AWAITABLE => (O::GetAwaitable, arg),
        op::GET_AITER => (O::GetAiter, 0),
        op::GET_ANEXT => (O::GetAnext, 0),
        // The END_SEND-relative oparg is a derived view; the encoder
        // recomputes it from the exception table.
        op::END_ASYNC_FOR => (O::EndAsyncFor, 0),
        op::MATCH_SEQUENCE => (O::MatchSequence, 0),
        op::MATCH_MAPPING => (O::MatchMapping, 0),
        op::MATCH_CLASS => (O::MatchClass, arg),
        op::MATCH_KEYS => (O::MatchKeys, 0),
        op::GET_LEN => (O::GetLen, 0),
        _ => return None,
    };
    Some(pair)
}

impl CodeObject {
    /// The CPython-3.14 wire view of this code object (RFC 0033).
    /// Encoded once per code object and memoised (the encoding is hot:
    /// trace functions hit `f_lasti` / `co_lines()` per event).
    #[must_use]
    pub fn to_cpython(&self) -> std::sync::Arc<CpythonCode> {
        self.cp_cache.get_or_init(|| encode(self))
    }

    /// Translate a WeavePy instruction index into the `co_code` byte offset
    /// CPython's `f_lasti`/`tb_lasti` expose (2 bytes/code unit, opcode past
    /// any `EXTENDED_ARG` prefix). Keeps `co_positions()` / `dis` anchoring
    /// consistent across the cache- and extended-arg-inflated encoding.
    #[must_use]
    pub fn cpython_lasti(&self, weavepy_index: u32) -> u32 {
        let cp = self.to_cpython();
        cp.inst_offsets
            .get(weavepy_index as usize)
            .map(|&unit| unit * 2)
            .unwrap_or(weavepy_index * 2)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn code_of(instrs: Vec<Instruction>) -> CodeObject {
        let mut c = CodeObject {
            linetable: vec![1u32; instrs.len()],
            instructions: instrs,
            ..CodeObject::default()
        };
        // Give a couple of locals so LOAD_FAST vs LOAD_CLOSURE disambiguates.
        c.varnames = vec!["a".to_owned(), "b".to_owned()];
        c
    }

    fn roundtrip(instrs: Vec<Instruction>) {
        let code = code_of(instrs.clone());
        let cp = encode(&code);
        // co_code is 2 bytes per code unit.
        assert_eq!(cp.co_code.len() % 2, 0);
        // positions: one per code unit.
        assert_eq!(cp.positions.len(), cp.co_code.len() / 2);
        let slots = SlotMap::from_code_vars(&code.varnames, &code.cellvars, &code.freevars);
        let back = decode(&cp.co_code, &slots, &code.constants)
            .expect("decode should map every emitted opcode");
        assert_eq!(back, code.instructions);
    }

    #[test]
    fn roundtrip_simple() {
        roundtrip(vec![
            Instruction::new(OpCode::Resume, 0),
            Instruction::new(OpCode::LoadConst, 0),
            Instruction::new(OpCode::ReturnValue, 0),
        ]);
    }

    #[test]
    fn roundtrip_arg_transforms() {
        roundtrip(vec![
            Instruction::new(OpCode::LoadGlobal, 3),
            Instruction::new(OpCode::LoadAttr, 5),
            Instruction::new(OpCode::CompareOp, CompareKind::Lt.as_arg()),
            Instruction::new(OpCode::BinaryOp, BinOpKind::Mult.as_arg()),
            Instruction::new(OpCode::UnaryOp, UnaryKind::Pos.as_arg()),
            Instruction::new(OpCode::UnaryOp, UnaryKind::Invert.as_arg()),
            Instruction::new(OpCode::ReturnValue, 0),
        ]);
    }

    #[test]
    fn roundtrip_extended_arg() {
        roundtrip(vec![
            Instruction::new(OpCode::LoadConst, 300),
            Instruction::new(OpCode::LoadConst, 70_000),
            Instruction::new(OpCode::ReturnValue, 0),
        ]);
    }

    #[test]
    fn extended_arg_units_emitted() {
        let code = code_of(vec![Instruction::new(OpCode::LoadConst, 300)]);
        let cp = encode(&code);
        // EXTENDED_ARG 1, LOAD_CONST 44 -> 2 code units, 4 bytes.
        assert_eq!(cp.co_code, vec![op::EXTENDED_ARG, 1, op::LOAD_CONST, 44]);
    }

    #[test]
    fn cache_units_inserted() {
        let code = code_of(vec![
            Instruction::new(OpCode::LoadAttr, 0),
            Instruction::new(OpCode::ReturnValue, 0),
        ]);
        let cp = encode(&code);
        // LOAD_ATTR + 9 caches + RETURN_VALUE = 11 code units.
        assert_eq!(cp.co_code.len() / 2, 11);
        // The 9 units after LOAD_ATTR are CACHE/0.
        for u in 1..10 {
            assert_eq!(cp.co_code[u * 2], op::CACHE);
        }
    }

    #[test]
    fn roundtrip_forward_jump() {
        // POP_JUMP_IF_FALSE skips the next two instructions.
        roundtrip(vec![
            Instruction::new(OpCode::LoadFast, 0),
            Instruction::new(OpCode::PopJumpIfFalse, 2),
            Instruction::new(OpCode::LoadConst, 0),
            Instruction::new(OpCode::ReturnValue, 0),
            Instruction::new(OpCode::LoadConst, 1),
            Instruction::new(OpCode::ReturnValue, 0),
        ]);
    }

    #[test]
    fn roundtrip_backward_jump_loop() {
        roundtrip(vec![
            Instruction::new(OpCode::LoadFast, 0),
            Instruction::new(OpCode::GetIter, 0),
            // ForIter: exhausted -> jump past body (+3).
            Instruction::new(OpCode::ForIter, 3),
            Instruction::new(OpCode::StoreFast, 1),
            Instruction::new(OpCode::LoadFast, 1),
            // JumpBackward to the ForIter (i+1 - 4 = 2).
            Instruction::new(OpCode::JumpBackward, 4),
            Instruction::new(OpCode::ReturnValue, 0),
        ]);
    }

    #[test]
    fn roundtrip_jump_over_caches_needs_extended_arg() {
        // Many cache-heavy instructions between a forward jump and its
        // target push the code-unit delta past 255, forcing EXTENDED_ARG
        // on the jump. The WeavePy instruction delta must still round-trip.
        let mut instrs = vec![
            Instruction::new(OpCode::LoadFast, 0),
            Instruction::new(OpCode::PopJumpIfFalse, 40),
        ];
        for _ in 0..40 {
            instrs.push(Instruction::new(OpCode::LoadAttr, 0)); // 10 units each
        }
        instrs.push(Instruction::new(OpCode::ReturnValue, 0));
        roundtrip(instrs);
    }

    /// Sum of location-entry lengths must cover every code unit.
    fn linetable_units(lt: &[u8]) -> usize {
        let mut i = 0;
        let mut total = 0;
        while i < lt.len() {
            let first = lt[i];
            i += 1;
            total += usize::from((first & 0x07) + 1);
            // Skip one signed varint (continuation bit is 0x40).
            loop {
                let cont = lt[i] & 0x40 != 0;
                i += 1;
                if !cont {
                    break;
                }
            }
        }
        total
    }

    #[test]
    fn linetable_covers_all_units() {
        let code = code_of(vec![
            Instruction::new(OpCode::Resume, 0),
            Instruction::new(OpCode::LoadAttr, 0),
            Instruction::new(OpCode::LoadConst, 300),
            Instruction::new(OpCode::ReturnValue, 0),
        ]);
        let cp = encode(&code);
        assert_eq!(linetable_units(&cp.co_linetable), cp.co_code.len() / 2);
    }

    /// Parse a big-endian exception-table varint at `*i`.
    fn exc_varint(t: &[u8], i: &mut usize) -> u32 {
        let mut b = t[*i];
        *i += 1;
        let mut val = u32::from(b & 0x3F);
        while b & 0x40 != 0 {
            b = t[*i];
            *i += 1;
            val = (val << 6) | u32::from(b & 0x3F);
        }
        val
    }

    #[test]
    fn exception_table_encodes_code_units() {
        let mut code = code_of(vec![
            Instruction::new(OpCode::Resume, 0),
            Instruction::new(OpCode::LoadAttr, 0), // 10 units (1 + 9 cache)
            Instruction::new(OpCode::LoadConst, 0),
            Instruction::new(OpCode::ReturnValue, 0),
        ]);
        code.exception_table.push(crate::ExcHandler {
            start: 1,
            end: 3,
            handler: 3,
            depth: 2,
            push_lasti: false,
        });
        let cp = encode(&code);
        let mut i = 0;
        let start = exc_varint(&cp.co_exceptiontable, &mut i);
        let length = exc_varint(&cp.co_exceptiontable, &mut i);
        let target = exc_varint(&cp.co_exceptiontable, &mut i);
        let dl = exc_varint(&cp.co_exceptiontable, &mut i);
        // Instruction 1 starts at code unit 1 (after RESUME).
        assert_eq!(start, 1);
        // Instructions 1..3 span LOAD_ATTR(10) + LOAD_CONST(1) = 11 units.
        assert_eq!(length, 11);
        // Handler at instruction 3 starts at unit 1 + 10 + 1 = 12.
        assert_eq!(target, 12);
        assert_eq!(dl >> 1, 2);
    }

    #[test]
    fn decode_full_round_trips_tables_and_locals() {
        // A code object exercising locals/cells/frees, a forward jump, an
        // exception handler, and a multi-line linetable.
        let mut code = CodeObject {
            instructions: vec![
                Instruction::new(OpCode::Resume, 0),
                Instruction::new(OpCode::LoadFast, 0),
                Instruction::new(OpCode::PopJumpIfFalse, 2),
                Instruction::new(OpCode::LoadFast, 1),
                Instruction::new(OpCode::ReturnValue, 0),
                Instruction::new(OpCode::LoadConst, 0),
                Instruction::new(OpCode::ReturnValue, 0),
            ],
            linetable: vec![1, 2, 2, 3, 3, 4, 4],
            ..CodeObject::default()
        };
        code.varnames = vec!["a".to_owned(), "b".to_owned()];
        code.cellvars = vec!["c".to_owned()];
        code.freevars = vec!["f".to_owned()];
        code.exception_table.push(ExcHandler {
            start: 1,
            end: 4,
            handler: 5,
            depth: 2,
            push_lasti: false,
        });

        let cp = encode(&code);
        let dc = decode_full(
            &cp.co_code,
            &cp.co_linetable,
            &cp.co_exceptiontable,
            &cp.localsplusnames,
            &cp.localspluskinds,
            cp.firstlineno,
            &code.constants,
        )
        .expect("decode_full should map every emitted opcode");

        assert_eq!(dc.instructions, code.instructions);
        assert_eq!(dc.varnames, code.varnames);
        assert_eq!(dc.cellvars, code.cellvars);
        assert_eq!(dc.freevars, code.freevars);
        assert_eq!(dc.linetable, code.linetable);
        assert_eq!(dc.exception_table, code.exception_table);

        // Re-encoding the decoded form must reproduce the wire bytes
        // exactly — a strong end-to-end inverse invariant.
        let mut code2 = CodeObject {
            instructions: dc.instructions,
            linetable: dc.linetable,
            ..CodeObject::default()
        };
        code2.varnames = dc.varnames;
        code2.cellvars = dc.cellvars;
        code2.freevars = dc.freevars;
        code2.exception_table = dc.exception_table;
        let cp2 = encode(&code2);
        assert_eq!(cp2.co_code, cp.co_code);
        assert_eq!(cp2.co_linetable, cp.co_linetable);
        assert_eq!(cp2.co_exceptiontable, cp.co_exceptiontable);
    }
}
#[test]
fn pyc_roundtrip_chain_repro() {
    use weavepy_parser::parse_module;
    let src = r"
class RaiseExc:
    def __init__(self, exc): self.exc = exc
    def __enter__(self): return self
    def __exit__(self, *d): raise self.exc

class RaiseExcWithContext:
    def __init__(self, outer, inner):
        self.outer = outer
        self.inner = inner
    def __enter__(self): return self
    def __exit__(self, *d):
        try:
            raise self.inner
        except:
            raise self.outer

class SuppressExc:
    def __enter__(self): return self
    def __exit__(self, *d):
        type(self).saved_details = d
        return True

def body():
    try:
        with RaiseExc(IndexError):
            with RaiseExcWithContext(KeyError, AttributeError):
                with SuppressExc():
                    with RaiseExc(ValueError):
                        1 / 0
    except IndexError as exc:
        return exc.__context__.__context__.__context__
";
    let module = parse_module(src).expect("parse");
    let code = crate::compile_module(&module).expect("compile");
    fn walk(code: &crate::CodeObject, path: String) {
        let cp = crate::cpython_code::encode(code);
        let dc = crate::cpython_code::decode_full(
            &cp.co_code,
            &cp.co_linetable,
            &cp.co_exceptiontable,
            &cp.localsplusnames,
            &cp.localspluskinds,
            cp.firstlineno,
            &code.constants,
        )
        .expect("decode");
        let norm = |ins: &crate::Instruction| -> crate::Instruction {
            // Legacy emit sites use `COPY 0` as a plain dup; the VM and
            // the wire format both read it as `COPY 1`. The VM-only
            // handler-end tag on `PUSH_EXC_INFO` isn't part of CPython's
            // encoding (its oparg is always 0 on the wire).
            if ins.op == crate::bytecode::OpCode::CopyTop && ins.arg == 0 {
                crate::Instruction::new(ins.op, 1)
            } else if ins.op == crate::bytecode::OpCode::PushExcInfo {
                crate::Instruction::new(ins.op, 0)
            } else {
                *ins
            }
        };
        for (i, (a, b)) in code
            .instructions
            .iter()
            .zip(dc.instructions.iter())
            .enumerate()
        {
            let (a, b) = (norm(a), norm(b));
            assert_eq!(a, b, "{path}: instruction {i} diverges: {a:?} vs {b:?}");
        }
        assert_eq!(
            code.instructions.len(),
            dc.instructions.len(),
            "{path}: length"
        );
        assert_eq!(
            code.exception_table, dc.exception_table,
            "{path}: exception table"
        );
        assert_eq!(code.linetable, dc.linetable, "{path}: linetable");
        for c in &code.constants {
            if let crate::Constant::Code(inner) = c {
                walk(inner, format!("{path}/{}", inner.name));
            }
        }
    }
    walk(&code, "<module>".to_owned());
}

/// `stack_effects` (WeavePy VM view, drives `co_stacksize`) and
/// `cp_stack_shape` (CPython wire view, drives the flowgraph depth
/// walk) must agree on the net fallthrough effect of every opcode the
/// decoder maps 1:1. A missing arm in either table silently skews
/// `co_stacksize` (MATCH_KEYS was absent from `stack_effects` and every
/// mapping pattern under-reported by one).
#[test]
fn stack_effects_agree_with_wire_shape() {
    let slots = SlotMap::from_code_vars(&[], &[], &[]);
    // Opcodes whose WeavePy arg is not the wire arg, or whose
    // fallthrough effect legitimately differs from the wire shape
    // (jumps take their real effect on the edge; FOR_ITER/END_FOR
    // telescope; RETURN_VALUE is `retval -- res` on the wire because
    // the result lands in the caller's frame; MAKE_FUNCTION's WeavePy
    // arg is the legacy flags word the compiler no longer emits).
    let skip: &[u8] = &[
        op::RETURN_VALUE,
        op::MAKE_FUNCTION,
        op::FOR_ITER,
        op::END_FOR,
        op::SEND,
        op::JUMP_FORWARD,
        op::JUMP_BACKWARD,
        op::JUMP_BACKWARD_NO_INTERRUPT,
        op::POP_JUMP_IF_FALSE,
        op::POP_JUMP_IF_TRUE,
        op::POP_JUMP_IF_NONE,
        op::POP_JUMP_IF_NOT_NONE,
        op::CACHE,
        op::EXTENDED_ARG,
    ];
    let mut diverging = Vec::new();
    for cp_op in 0u8..=255 {
        if skip.contains(&cp_op) {
            continue;
        }
        for arg in [0u32, 1, 2, 3, 5] {
            let Some((op, warg)) = map_from_cpython(cp_op, arg, &slots) else {
                continue;
            };
            // Round-trip filter: only opcodes whose WeavePy arg maps
            // back onto this exact wire pair are 1:1.
            let m = map_to_cpython(
                Instruction { op, arg: warg },
                &DerefSlots { slots: Vec::new() },
            );
            if (m.cp_op, m.arg) != (cp_op, arg) {
                continue;
            }
            let (popped, pushed) = cp_stack_shape(cp_op, arg);
            let wire_net = pushed as i64 - popped as i64;
            let (vm_net, _) = stack_effects(op, warg);
            if wire_net != vm_net {
                diverging.push(format!(
                    "{}({cp_op}) arg={arg}: wire {wire_net} vs vm {vm_net}",
                    op.name()
                ));
            }
        }
    }
    assert!(
        diverging.is_empty(),
        "stack effect drift:\n{}",
        diverging.join("\n")
    );
}

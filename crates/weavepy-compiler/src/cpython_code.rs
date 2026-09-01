//! CPython-3.13 bytecode wire-format codec (RFC 0033).
//!
//! WeavePy executes its own flat `Vec<Instruction>` (see [`crate::bytecode`]).
//! CPython tooling — `dis`, `marshal`, `.pyc`, and the `code` object's
//! `co_code` / `co_linetable` / `co_exceptiontable` / `co_positions()`
//! surface — expects the 16-bit `_Py_CODEUNIT` stream CPython 3.13 emits.
//!
//! This module bridges the two. It is a *presentation* codec: the VM
//! never runs the bytes produced here, so the encoding is computed on
//! demand (when Python introspects a code object or marshals it) and is
//! independent of the dispatch loop, the inline caches (RFC 0021), and
//! the JIT (RFC 0032).
//!
//! The encoder is a faithful CPython-3.13 emitter:
//!
//! - opcode numbers and the per-opcode inline-`CACHE` entry counts match
//!   CPython 3.13 (`Include/opcode_ids.h`, `_PyOpcode_Caches`),
//! - args wider than a byte are prefixed with `EXTENDED_ARG`,
//! - relative jumps are recomputed in code units across the inserted
//!   caches via a fixpoint,
//! - the location table uses the PEP 626 "no-column" form (line-accurate;
//!   full column plumbing is tracked as follow-up work),
//! - the exception table uses CPython's big-endian varint range format.
//!
//! The [`decode`] direction inverts [`encode`] for the canonical opcode
//! set WeavePy emits, so `marshal`/`.pyc` round-trip to an executable
//! [`CodeObject`].

use crate::bytecode::{BinOpKind, CompareKind, Instruction, OpCode, UnaryKind};
use crate::{CodeObject, Constant, ExcHandler};

/// CPython 3.13 opcode numbers (subset WeavePy maps onto). Sourced from
/// `Include/opcode_ids.h` in CPython v3.13.
pub mod op {
    pub const CACHE: u8 = 0;
    pub const BEFORE_ASYNC_WITH: u8 = 1;
    pub const BEFORE_WITH: u8 = 2;
    pub const BINARY_SUBSCR: u8 = 5;
    pub const CHECK_EG_MATCH: u8 = 6;
    pub const CHECK_EXC_MATCH: u8 = 7;
    pub const DELETE_SUBSCR: u8 = 9;
    pub const END_ASYNC_FOR: u8 = 10;
    pub const END_FOR: u8 = 11;
    pub const CLEANUP_THROW: u8 = 8;
    pub const END_SEND: u8 = 12;
    pub const FORMAT_SIMPLE: u8 = 14;
    pub const FORMAT_WITH_SPEC: u8 = 15;
    pub const CONVERT_VALUE: u8 = 60;
    pub const TO_BOOL: u8 = 40;
    pub const GET_AITER: u8 = 16;
    pub const GET_ANEXT: u8 = 18;
    pub const GET_ITER: u8 = 19;
    pub const GET_LEN: u8 = 20;
    pub const GET_YIELD_FROM_ITER: u8 = 21;
    pub const LOAD_BUILD_CLASS: u8 = 24;
    pub const MAKE_FUNCTION: u8 = 26;
    pub const MATCH_KEYS: u8 = 27;
    pub const MATCH_MAPPING: u8 = 28;
    pub const MATCH_SEQUENCE: u8 = 29;
    pub const NOP: u8 = 30;
    pub const POP_EXCEPT: u8 = 31;
    pub const POP_TOP: u8 = 32;
    pub const PUSH_EXC_INFO: u8 = 33;
    pub const PUSH_NULL: u8 = 34;
    pub const SET_FUNCTION_ATTRIBUTE: u8 = 106;
    /// Function-entry prologue unit copying the closure tuple into the
    /// frame's free-variable slots. The encoder synthesizes it (WeavePy
    /// frame setup does the copy natively); see `insert_prologue`.
    pub const COPY_FREE_VARS: u8 = 62;
    pub const RETURN_GENERATOR: u8 = 35;
    pub const RETURN_VALUE: u8 = 36;
    /// Emitted by the encoder's `LOAD_CONST` + `RETURN_VALUE` fusion
    /// (CPython 3.13 compiles `return <const>` to this single unit).
    pub const RETURN_CONST: u8 = 103;
    pub const STORE_SUBSCR: u8 = 39;
    pub const UNARY_INVERT: u8 = 41;
    pub const UNARY_NEGATIVE: u8 = 42;
    pub const UNARY_NOT: u8 = 43;
    pub const WITH_EXCEPT_START: u8 = 44;
    pub const BINARY_OP: u8 = 45;
    pub const BUILD_LIST: u8 = 47;
    pub const BUILD_MAP: u8 = 48;
    pub const BUILD_SET: u8 = 49;
    pub const BUILD_SLICE: u8 = 50;
    pub const BUILD_STRING: u8 = 51;
    pub const BUILD_TUPLE: u8 = 52;
    pub const CALL: u8 = 53;
    pub const CALL_FUNCTION_EX: u8 = 54;
    pub const CALL_INTRINSIC_1: u8 = 55;
    pub const CALL_INTRINSIC_2: u8 = 56;
    pub const LOAD_ASSERTION_ERROR: u8 = 23;
    pub const CALL_KW: u8 = 57;
    pub const COMPARE_OP: u8 = 58;
    pub const CONTAINS_OP: u8 = 59;
    pub const COPY: u8 = 61;
    pub const DELETE_ATTR: u8 = 63;
    pub const DELETE_DEREF: u8 = 64;
    pub const DELETE_FAST: u8 = 65;
    pub const DELETE_GLOBAL: u8 = 66;
    pub const DELETE_NAME: u8 = 67;
    pub const DICT_MERGE: u8 = 68;
    pub const DICT_UPDATE: u8 = 69;
    pub const SETUP_ANNOTATIONS: u8 = 37;
    pub const EXTENDED_ARG: u8 = 71;
    pub const FOR_ITER: u8 = 72;
    pub const GET_AWAITABLE: u8 = 73;
    pub const IMPORT_FROM: u8 = 74;
    pub const IMPORT_NAME: u8 = 75;
    pub const IS_OP: u8 = 76;
    pub const JUMP_BACKWARD: u8 = 77;
    pub const JUMP_BACKWARD_NO_INTERRUPT: u8 = 78;
    pub const JUMP_FORWARD: u8 = 79;
    pub const LIST_APPEND: u8 = 80;
    pub const LIST_EXTEND: u8 = 81;
    pub const LOAD_ATTR: u8 = 82;
    pub const LOAD_CONST: u8 = 83;
    pub const LOAD_DEREF: u8 = 84;
    pub const LOAD_FAST: u8 = 85;
    pub const LOAD_FAST_AND_CLEAR: u8 = 86;
    /// Emitted by the encoder's uninitialized-locals analysis
    /// (CPython's `add_checks_for_loads_of_uninitialized_variables`):
    /// a `LOAD_FAST` the compiler can't prove bound decodes back to a
    /// plain `LoadFast` (WeavePy's runtime op always checks).
    pub const LOAD_FAST_CHECK: u8 = 87;
    /// Superinstructions (CPython's `insert_superinstructions`): two
    /// adjacent fast-local ops fused into one unit, args packed as
    /// `(arg1 << 4) | arg2`.
    pub const LOAD_FAST_LOAD_FAST: u8 = 88;
    pub const STORE_FAST_LOAD_FAST: u8 = 111;
    pub const STORE_FAST_STORE_FAST: u8 = 112;
    pub const LOAD_FROM_DICT_OR_DEREF: u8 = 89;
    pub const LOAD_FROM_DICT_OR_GLOBALS: u8 = 90;
    /// WeavePy-private (no 3.13 equivalent): CPython lowers a class
    /// body's free-variable load to `LOAD_LOCALS` +
    /// `LOAD_FROM_DICT_OR_DEREF`, but our `LoadClassderef` is a single
    /// instruction, so it needs its own number to round-trip — it must
    /// not collide with `LoadClassdictOrDeref`'s encoding (which pops
    /// an explicit mapping; `LoadClassderef` does not).
    pub const LOAD_CLASSDEREF_WEAVEPY: u8 = 147;
    pub const LOAD_GLOBAL: u8 = 91;
    pub const LOAD_NAME: u8 = 92;
    pub const LOAD_SUPER_ATTR: u8 = 93;
    pub const MAKE_CELL: u8 = 94;
    pub const MAP_ADD: u8 = 95;
    pub const MATCH_CLASS: u8 = 96;
    pub const POP_JUMP_IF_FALSE: u8 = 97;
    pub const POP_JUMP_IF_NONE: u8 = 98;
    pub const POP_JUMP_IF_NOT_NONE: u8 = 99;
    pub const POP_JUMP_IF_TRUE: u8 = 100;
    pub const RAISE_VARARGS: u8 = 101;
    pub const RERAISE: u8 = 102;
    pub const SEND: u8 = 104;
    pub const SET_ADD: u8 = 105;
    pub const STORE_ATTR: u8 = 108;
    pub const STORE_DEREF: u8 = 109;
    pub const STORE_FAST: u8 = 110;
    pub const STORE_GLOBAL: u8 = 113;
    pub const STORE_NAME: u8 = 114;
    pub const SWAP: u8 = 115;
    pub const UNPACK_EX: u8 = 116;
    pub const UNPACK_SEQUENCE: u8 = 117;
    pub const YIELD_VALUE: u8 = 118;
    pub const RESUME: u8 = 149;
    // WeavePy extensions for PEP 750 t-strings (`-X lang=next`,
    // RFC 0076 WS15): CPython 3.13 has no such opcodes. These numbers
    // sit in 3.13's specialized/quickened range, which never appears
    // in unquickened code, so they cannot collide with anything
    // `map_from_cpython` recognizes.
    pub const BUILD_TEMPLATE: u8 = 150;
    pub const BUILD_INTERPOLATION: u8 = 151;
}

/// CPython 3.13 `HAVE_ARGUMENT` boundary: opcodes `>=` this take an
/// inline argument. Opcodes below it ignore the (still-present) arg byte.
pub const HAVE_ARGUMENT: u8 = 44;

/// CPython's `MAGIC_NUMBER` for the 3.13 series (`importlib.util.MAGIC_NUMBER`).
pub const MAGIC_NUMBER: [u8; 4] = [0xf3, 0x0d, 0x0d, 0x0a];

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
/// 3.13 (`_PyOpcode_Caches`). Everything not listed has none.
#[must_use]
pub fn cache_entries(cp_op: u8) -> usize {
    match cp_op {
        op::LOAD_GLOBAL => 4,
        op::LOAD_ATTR => 9,
        op::STORE_ATTR => 4,
        op::CALL => 3,
        op::TO_BOOL => 3,
        op::BINARY_OP
        | op::UNPACK_SEQUENCE
        | op::COMPARE_OP
        | op::CONTAINS_OP
        | op::BINARY_SUBSCR
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

/// `True` if `cp_op` is a relative jump (its arg is a code-unit delta).
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
        // 3.13 has no real LOAD_CLOSURE opcode; cells live in the fast
        // array and are loaded with LOAD_FAST.
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
        O::BinarySubscr => (op::BINARY_SUBSCR, 0),
        O::StoreSubscr => (op::STORE_SUBSCR, 0),
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
            // No dedicated opcode for unary `+` in 3.13.
            _ => (op::CALL_INTRINSIC_1, INTRINSIC_UNARY_POSITIVE),
        },
        // bits 5+ carry the comparison index; the low nibble is CPython's
        // specialization mask (COMPARISON_LESS_THAN=2 / GREATER_THAN=4 /
        // EQUALS=8 / UNORDERED=1). Bit 4 ("convert to bool") is OR'd in by
        // `encode` when the result feeds a conditional jump or `not`,
        // mirroring the COMPARE_OP+TO_BOOL fusion in CPython's optimizer.
        O::CompareOp => {
            let mask: u32 = match ins.arg {
                0 => 2,         // <
                1 => 2 | 8,     // <=
                2 => 8,         // ==
                3 => 1 | 2 | 4, // !=
                4 => 4,         // >
                5 => 4 | 8,     // >=
                _ => 0,
            };
            (op::COMPARE_OP, (ins.arg << 5) | mask)
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
        O::CallEx => (op::CALL_FUNCTION_EX, ins.arg),
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
        O::MapAdd => (op::MAP_ADD, ins.arg),
        O::UnpackSequence => (op::UNPACK_SEQUENCE, ins.arg),
        // Our UNPACK_EX arg keeps the before-star count in the high
        // byte; CPython's keeps it in the low byte.
        O::UnpackEx => (
            op::UNPACK_EX,
            ((ins.arg >> 8) & 0xFF) | ((ins.arg & 0xFF) << 8),
        ),
        // WeavePy folds CPython's DICT_UPDATE (dict display) and
        // DICT_MERGE (call `**` splat) into one opcode keyed by arg;
        // surface them as the distinct CPython opcodes, whose oparg is
        // the stack offset of the target dict (always 1 here).
        O::DictUpdate if ins.arg == 1 => (op::DICT_MERGE, 1),
        O::DictUpdate => (op::DICT_UPDATE, 1),
        O::SetupAnnotations => (op::SETUP_ANNOTATIONS, 0),
        O::MakeFunction => (op::MAKE_FUNCTION, ins.arg),
        O::SetFunctionAttribute => (op::SET_FUNCTION_ATTRIBUTE, ins.arg),
        O::BuildSlice => (op::BUILD_SLICE, ins.arg),
        O::LoadBuildClass => (op::LOAD_BUILD_CLASS, 0),
        O::LoadClassderef => (op::LOAD_CLASSDEREF_WEAVEPY, slots.slot(ins.arg)),
        O::LoadClassdictOrDeref => (op::LOAD_FROM_DICT_OR_DEREF, slots.slot(ins.arg)),
        O::LoadClassdictOrGlobal => (op::LOAD_FROM_DICT_OR_GLOBALS, ins.arg),
        O::RaiseVarargs => (op::RAISE_VARARGS, ins.arg),
        O::CheckExcMatch => (op::CHECK_EXC_MATCH, 0),
        O::CheckEGMatch => (op::CHECK_EG_MATCH, 0),
        O::PushExcInfo => (op::PUSH_EXC_INFO, 0),
        O::PopExcept => (op::POP_EXCEPT, 0),
        O::Reraise => (op::RERAISE, ins.arg),
        O::BeforeWith => (op::BEFORE_WITH, 0),
        O::WithExceptStart => (op::WITH_EXCEPT_START, 0),
        O::ImportName => (op::IMPORT_NAME, ins.arg),
        O::ImportFrom => (op::IMPORT_FROM, ins.arg),
        O::ImportStar => (op::CALL_INTRINSIC_1, INTRINSIC_IMPORT_STAR),
        O::PrepReraiseStar => (op::CALL_INTRINSIC_2, INTRINSIC_PREP_RERAISE_STAR),
        O::CleanupThrow => (op::CLEANUP_THROW, 0),
        O::StopIterationError => (op::CALL_INTRINSIC_1, INTRINSIC_STOPITERATION_ERROR),
        O::AsyncGenWrap => (op::CALL_INTRINSIC_1, INTRINSIC_ASYNC_GEN_WRAP),
        O::BuildInterpolation => (op::BUILD_INTERPOLATION, ins.arg),
        O::BuildTemplate => (op::BUILD_TEMPLATE, 0),
        O::LoadAssertionError => (op::LOAD_ASSERTION_ERROR, 0),
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
        O::EndAsyncFor => (op::END_ASYNC_FOR, 0),
        O::BeforeAsyncWith => (op::BEFORE_ASYNC_WITH, 0),
        O::MatchSequence => (op::MATCH_SEQUENCE, 0),
        O::MatchMapping => (op::MATCH_MAPPING, 0),
        O::MatchClass => (op::MATCH_CLASS, ins.arg),
        O::MatchKeys => (op::MATCH_KEYS, 0),
        O::GetLen => (op::GET_LEN, 0),
        O::PrintExpr => (op::NOP, 0),
    };
    MappedOp { cp_op, arg }
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

/// The CPython-3.13 wire view of a [`CodeObject`].
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

pub const CO_FAST_LOCAL: u8 = 0x20;
pub const CO_FAST_CELL: u8 = 0x40;
pub const CO_FAST_FREE: u8 = 0x80;

/// Build the merged `co_localsplusnames` / `co_localspluskinds` pair.
/// CPython's `compute_localsplus_info`: a cell that aliases a local
/// (an escaping parameter) shares the local's slot with kind
/// `CO_FAST_LOCAL|CO_FAST_CELL` rather than getting its own entry.
fn build_localsplus(code: &CodeObject) -> (Vec<String>, Vec<u8>) {
    let mut names = Vec::with_capacity(code.varnames.len() + code.cellvars.len());
    let mut kinds = Vec::with_capacity(names.capacity());
    for v in &code.varnames {
        let mut kind = CO_FAST_LOCAL;
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

/// CPython's `add_checks_for_loads_of_uninitialized_variables`
/// (flowgraph.c): mark every `LOAD_FAST` of a slot that may be unbound
/// on some path to it as `LOAD_FAST_CHECK`. WeavePy's runtime
/// `LoadFast` always checks, so this refines the *view* only — but
/// `dis` output and test_peepholer's TestMarkingVariablesAsUnKnown
/// grade the distinction. The dataflow is a faithful port: a 64-bit
/// "may be unsafe" mask per basic block, seeded with the non-parameter
/// locals at entry, propagated over fallthrough / jump / exception
/// edges to a fixpoint; functions with more than 64 locals get the
/// block-local `fast_scan_many_locals` treatment for the excess slots.
// Index-driven on purpose: the loops walk instruction *offsets* shared
// between `code.instructions`, `check`, and the per-block `starts`
// table — an iterator/enumerate rewrite would obscure the offset math.
#[allow(clippy::needless_range_loop)]
fn add_uninitialized_checks(code: &CodeObject, mapped: &mut [MappedOp]) {
    use OpCode as O;
    let nlocals = code.varnames.len();
    let n = code.instructions.len();
    if nlocals == 0 || n == 0 {
        return;
    }
    let nparams = (code.arg_count
        + code.kwonly_count
        + u32::from(code.has_varargs)
        + u32::from(code.has_varkeywords)) as usize;

    // Basic blocks over the flat stream: leaders are the entry, jump
    // targets, handler entries, and the fallthrough of a block ender —
    // the same boundaries CPython's cfg has.
    let mut leader = vec![false; n];
    leader[0] = true;
    for i in 0..n {
        let ins = code.instructions[i];
        let from = i as u32 + 1;
        match ins.op {
            O::JumpForward
            | O::PopJumpIfFalse
            | O::PopJumpIfTrue
            | O::PopJumpIfNone
            | O::PopJumpIfNotNone
            | O::ForIter
            | O::Send => {
                if let Some(l) = leader.get_mut((from + ins.arg) as usize) {
                    *l = true;
                }
                if let Some(l) = leader.get_mut(i + 1) {
                    *l = true;
                }
            }
            O::JumpBackward => {
                if let Some(l) = leader.get_mut(from.saturating_sub(ins.arg) as usize) {
                    *l = true;
                }
                if let Some(l) = leader.get_mut(i + 1) {
                    *l = true;
                }
            }
            O::ReturnValue | O::RaiseVarargs | O::Reraise => {
                if let Some(l) = leader.get_mut(i + 1) {
                    *l = true;
                }
            }
            _ => {}
        }
    }
    for h in &code.exception_table {
        if let Some(l) = leader.get_mut(h.handler as usize) {
            *l = true;
        }
    }
    let mut block_of = vec![0usize; n];
    let mut starts: Vec<usize> = Vec::new();
    for i in 0..n {
        if leader[i] {
            starts.push(i);
        }
        block_of[i] = starts.len() - 1;
    }
    let nb = starts.len();
    let block_end = |b: usize| if b + 1 < nb { starts[b + 1] } else { n };

    let mut check = vec![false; n];

    // `fast_scan_many_locals`: slots >= 64 are only trusted within the
    // basic block that stored them.
    if nlocals > 64 {
        let mut states = vec![0usize; nlocals - 64];
        for b in 0..nb {
            let blocknum = b + 1;
            for i in starts[b]..block_end(b) {
                let ins = code.instructions[i];
                let arg = ins.arg as usize;
                if arg < 64 {
                    continue;
                }
                match ins.op {
                    O::DeleteFast => states[arg - 64] = blocknum - 1,
                    O::StoreFast => states[arg - 64] = blocknum,
                    O::LoadFast if arg < nlocals => {
                        if states[arg - 64] != blocknum {
                            check[i] = true;
                        }
                        states[arg - 64] = blocknum;
                    }
                    _ => {}
                }
            }
        }
    }

    let track = nlocals.min(64);
    let start_mask: u64 = if nparams >= track {
        0
    } else {
        (((1u128 << track) - (1u128 << nparams)) & u128::from(u64::MAX)) as u64
    };

    fn maybe_push(
        b: usize,
        mask: u64,
        unsafe_mask: &mut [u64],
        visited: &mut [bool],
        stack: &mut Vec<usize>,
    ) {
        let both = unsafe_mask[b] | mask;
        if unsafe_mask[b] != both {
            unsafe_mask[b] = both;
            if !visited[b] {
                stack.push(b);
                visited[b] = true;
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn scan_block(
        b: usize,
        code: &CodeObject,
        starts: &[usize],
        block_of: &[usize],
        n: usize,
        nb: usize,
        unsafe_mask: &mut [u64],
        visited: &mut [bool],
        stack: &mut Vec<usize>,
        check: &mut [bool],
    ) {
        use OpCode as O;
        let end = if b + 1 < nb { starts[b + 1] } else { n };
        let mut mask = unsafe_mask[b];
        for i in starts[b]..end {
            // Exception edges: an instruction inside a protected range
            // may transfer to the handler with the mask as of here.
            for h in &code.exception_table {
                if (h.start as usize) <= i && i < (h.end as usize) && (h.handler as usize) < n {
                    maybe_push(
                        block_of[h.handler as usize],
                        mask,
                        unsafe_mask,
                        visited,
                        stack,
                    );
                }
            }
            let ins = code.instructions[i];
            if ins.arg >= 64 {
                continue;
            }
            let bit = 1u64 << ins.arg;
            match ins.op {
                O::DeleteFast => mask |= bit,
                O::StoreFast => mask &= !bit,
                O::LoadFast => {
                    if !check[i] && mask & bit != 0 {
                        check[i] = true;
                    }
                    mask &= !bit;
                }
                _ => {}
            }
        }
        let last = code.instructions[end - 1];
        let from = end as u32;
        match last.op {
            O::JumpForward => {
                let t = (from + last.arg) as usize;
                if t < n {
                    maybe_push(block_of[t], mask, unsafe_mask, visited, stack);
                }
            }
            O::JumpBackward => {
                let t = from.saturating_sub(last.arg) as usize;
                if t < n {
                    maybe_push(block_of[t], mask, unsafe_mask, visited, stack);
                }
            }
            O::PopJumpIfFalse
            | O::PopJumpIfTrue
            | O::PopJumpIfNone
            | O::PopJumpIfNotNone
            | O::ForIter
            | O::Send => {
                let t = (from + last.arg) as usize;
                if t < n {
                    maybe_push(block_of[t], mask, unsafe_mask, visited, stack);
                }
                if b + 1 < nb {
                    maybe_push(b + 1, mask, unsafe_mask, visited, stack);
                }
            }
            O::ReturnValue | O::RaiseVarargs | O::Reraise => {}
            _ => {
                if b + 1 < nb {
                    maybe_push(b + 1, mask, unsafe_mask, visited, stack);
                }
            }
        }
    }

    let mut unsafe_mask = vec![0u64; nb];
    let mut visited = vec![false; nb];
    let mut stack: Vec<usize> = Vec::new();
    maybe_push(0, start_mask, &mut unsafe_mask, &mut visited, &mut stack);
    for b in 0..nb {
        scan_block(
            b,
            code,
            &starts,
            &block_of,
            n,
            nb,
            &mut unsafe_mask,
            &mut visited,
            &mut stack,
            &mut check,
        );
    }
    while let Some(b) = stack.pop() {
        visited[b] = false;
        scan_block(
            b,
            code,
            &starts,
            &block_of,
            n,
            nb,
            &mut unsafe_mask,
            &mut visited,
            &mut stack,
            &mut check,
        );
    }

    for i in 0..n {
        if check[i] && mapped[i].cp_op == op::LOAD_FAST {
            mapped[i].cp_op = op::LOAD_FAST_CHECK;
        }
    }
}

/// Encode `code` into its CPython-3.13 wire view.
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

    // COMPARE_OP's "convert to bool" bit: CPython's optimizer fuses a
    // trailing TO_BOOL into the compare when the result feeds a branch
    // or `not`; WeavePy's stream has no TO_BOOL, so the consumer is the
    // very next instruction.
    for i in 0..n {
        if code.instructions[i].op == OpCode::CompareOp {
            let feeds_bool = matches!(
                code.instructions.get(i + 1),
                Some(next) if matches!(next.op, OpCode::PopJumpIfFalse | OpCode::PopJumpIfTrue)
                    || (next.op == OpCode::UnaryOp
                        && next.arg == crate::bytecode::UnaryKind::Not as u32)
            );
            if feeds_bool {
                mapped[i].arg |= 16;
            }
        }
    }

    // CPython's `insert_prefix_instructions`: the wire stream opens
    // with `COPY_FREE_VARS n` (if the code has free variables) and one
    // `MAKE_CELL` per cell variable in localsplus-slot order, all at
    // NO_LOCATION. WeavePy's frame setup performs both natively, so
    // the internal stream carries no prologue — synthesize it here
    // (and strip it again in decode). Streams that already start with
    // an explicit prologue (hand-built via `types.CodeType`) keep it.
    let mut prologue: Vec<MappedOp> = Vec::new();
    let has_explicit_prologue = matches!(
        code.instructions.first().map(|i| i.op),
        Some(OpCode::CopyFreeVars | OpCode::MakeCell)
    );
    if !has_explicit_prologue {
        if !code.freevars.is_empty() {
            prologue.push(MappedOp {
                cp_op: op::COPY_FREE_VARS,
                arg: code.freevars.len() as u32,
            });
        }
        let mut cell_slots: Vec<u32> = slots.slots[..code.cellvars.len()].to_vec();
        cell_slots.sort_unstable();
        for s in cell_slots {
            prologue.push(MappedOp {
                cp_op: op::MAKE_CELL,
                arg: s,
            });
        }
    }
    let prologue_units: usize = prologue.iter().map(|m| ext_count(m.arg) + 1).sum();

    // Uninitialized-locals analysis: LOAD_FAST → LOAD_FAST_CHECK where
    // the slot may be unbound (must run before superinstruction fusion,
    // as in CPython — checked loads never fuse).
    add_uninitialized_checks(code, &mut mapped);

    // CPython 3.13 compiles `return <const>` to a single RETURN_CONST
    // unit; WeavePy's internal stream keeps the LOAD_CONST +
    // RETURN_VALUE pair. Fuse on the wire so instruction offsets (and
    // `sys.monitoring` INSTRUCTION events keyed to them) line up. A
    // pair is fusable only when nothing addresses the RETURN_VALUE
    // itself (jump target, exception-range boundary, handler tag) and
    // both halves share one source location (RETURN_CONST carries a
    // single position).
    // CPython emits the await / yield-from send-dance edges as
    // JUMP_BACKWARD_NO_INTERRUPT (no eval-breaker poll between YIELD
    // and the re-SEND, nor on a CLEANUP_THROW's hop to END_SEND).
    // WeavePy lowers both with a plain JumpBackward; recover the
    // distinction structurally: a backward jump targeting a SEND is
    // the resend edge, and one straight after CLEANUP_THROW is a
    // cold-moved cleanup block's exit edge.
    for i in 0..n {
        if mapped[i].cp_op != op::JUMP_BACKWARD {
            continue;
        }
        let t = (i + 1).saturating_sub(args_target_delta(code.instructions[i]));
        if mapped.get(t).map(|m| m.cp_op) == Some(op::SEND)
            || (i > 0 && mapped[i - 1].cp_op == op::CLEANUP_THROW)
        {
            mapped[i].cp_op = op::JUMP_BACKWARD_NO_INTERRUPT;
        }
    }
    // Compiler-flagged synthetic scope-exit jumps (handler exits,
    // `with`-suppress exits, cold rejoins) — see
    // `CodeObject::no_interrupt_jumps`.
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
    let mut fused = vec![false; n];
    // Location source per emitted instruction: normally itself, but a
    // fused RETURN_CONST takes the RETURN_VALUE's location (CPython
    // stamps the whole `return <const>` statement span, not the
    // constant expression's).
    let mut loc_src: Vec<usize> = (0..n).collect();
    {
        let mut boundary = vec![false; n + 1];
        for (i, m) in mapped.iter().enumerate() {
            if is_rel_jump(m.cp_op) {
                let t = if is_backward_jump(m.cp_op) {
                    (i + 1).saturating_sub(args_target_delta(code.instructions[i]))
                } else {
                    i + 1 + args_target_delta(code.instructions[i])
                };
                boundary[t.min(n)] = true;
            }
            if m.cp_op == op::PUSH_EXC_INFO {
                boundary[(code.instructions[i].arg as usize).min(n)] = true;
            }
        }
        for h in &code.exception_table {
            boundary[(h.start as usize).min(n)] = true;
            boundary[(h.end as usize).min(n)] = true;
            boundary[(h.handler as usize).min(n)] = true;
        }
        for i in 1..n {
            if mapped[i].cp_op == op::RETURN_VALUE
                // arg 1 = codegen-origin constant return. CPython 3.13
                // emits RETURN_CONST from codegen only; an optimizer's
                // branch elimination leaving LOAD_CONST + RETURN_VALUE
                // is never re-fused (test_consts_in_conditionals).
                && code.instructions[i].arg == 1
                && mapped[i - 1].cp_op == op::LOAD_CONST
                && !boundary[i]
                && code.linetable.get(i) == code.linetable.get(i - 1)
            {
                fused[i] = true;
                mapped[i - 1].cp_op = op::RETURN_CONST;
                loc_src[i - 1] = i;
            }
        }
        // CPython folds the callable-NULL push into LOAD_GLOBAL's
        // oparg bit 0 (there is no separate PUSH_NULL unit after a
        // global callable load); every other callable load keeps the
        // explicit PUSH_NULL instruction.
        for i in 1..n {
            if mapped[i].cp_op == op::PUSH_NULL
                && mapped[i - 1].cp_op == op::LOAD_GLOBAL
                && !fused[i - 1]
                && !boundary[i]
            {
                fused[i] = true;
                mapped[i - 1].arg |= 1;
            }
        }
        // CPython's `insert_superinstructions`: adjacent fast-local
        // pairs fuse into one unit with args packed 4 bits each. Only
        // true locals participate — CPython's fusion runs before
        // LOAD_CLOSURE lowers to LOAD_FAST, so closure loads (WeavePy's
        // `LoadClosure`, mapped to LOAD_FAST above) stay unfused. Both
        // halves must sit on one line (or carry no location), like
        // `make_super_instruction`.
        for i in 1..n {
            if fused[i] || fused[i - 1] || boundary[i] {
                continue;
            }
            let sop = match (mapped[i - 1].cp_op, mapped[i].cp_op) {
                (op::LOAD_FAST, op::LOAD_FAST)
                    if code.instructions[i - 1].op == OpCode::LoadFast
                        && code.instructions[i].op == OpCode::LoadFast =>
                {
                    op::LOAD_FAST_LOAD_FAST
                }
                (op::STORE_FAST, op::LOAD_FAST) if code.instructions[i].op == OpCode::LoadFast => {
                    op::STORE_FAST_LOAD_FAST
                }
                (op::STORE_FAST, op::STORE_FAST) => op::STORE_FAST_STORE_FAST,
                _ => continue,
            };
            let (a1, a2) = (mapped[i - 1].arg, mapped[i].arg);
            if a1 >= 16 || a2 >= 16 {
                continue;
            }
            let (l1, l2) = (
                code.linetable.get(i - 1).copied().unwrap_or(0),
                code.linetable.get(i).copied().unwrap_or(0),
            );
            if l1 != 0 && l2 != 0 && l1 != l2 {
                continue;
            }
            fused[i] = true;
            mapped[i - 1].cp_op = sop;
            mapped[i - 1].arg = (a1 << 4) | a2;
        }
    }

    // Fixpoint: jump args depend on code-unit offsets, which depend on
    // how many EXTENDED_ARG units precede each instruction.
    let mut ext: Vec<usize> = mapped
        .iter()
        .enumerate()
        .map(|(i, m)| {
            if is_rel_jump(m.cp_op) || fused[i] {
                0
            } else {
                ext_count(m.arg)
            }
        })
        .collect();
    let mut starts = vec![0usize; n + 1];
    let mut args: Vec<u32> = mapped.iter().map(|m| m.arg).collect();

    for _ in 0..16 {
        // Recompute code-unit start offsets. The synthesized prologue
        // occupies the first `prologue_units` units, so every start is
        // already absolute (exception-table offsets and `f_lasti`
        // include the prologue, exactly as in CPython).
        let mut off = prologue_units;
        for i in 0..n {
            starts[i] = off;
            if !fused[i] {
                off += ext[i] + 1 + cache_entries(mapped[i].cp_op);
            }
        }
        starts[n] = off;

        let mut changed = false;
        for i in 0..n {
            // `PUSH_EXC_INFO` has no oparg in CPython, but WeavePy tags it
            // with the pc just past the handler body (the unwinder's
            // discard cue). The cache is WeavePy-only, so persist the tag
            // as an *absolute code-unit offset* — losing it (decoding to
            // the untagged 0) changes handled-exception unwinding, which
            // is observable through `__context__` chaining.
            if mapped[i].cp_op == op::PUSH_EXC_INFO {
                let tag = code.instructions[i].arg as usize;
                if tag != 0 {
                    let oparg = starts[tag.min(n)] as u32;
                    args[i] = oparg;
                    let need = ext_count(oparg);
                    if need != ext[i] {
                        ext[i] = need;
                        changed = true;
                    }
                }
                continue;
            }
            if !is_rel_jump(mapped[i].cp_op) {
                continue;
            }
            let size = ext[i] + 1 + cache_entries(mapped[i].cp_op);
            let next_unit = starts[i] + size;
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
    // The prologue first, at CPython's NO_LOCATION.
    const NO_LOCATION: Position = Position {
        lineno: -1,
        end_lineno: -1,
        col: None,
        end_col: None,
    };
    for m in &prologue {
        for k in (1..=ext_count(m.arg)).rev() {
            co_code.push(op::EXTENDED_ARG);
            co_code.push(((m.arg >> (8 * k)) & 0xFF) as u8);
            positions.push(NO_LOCATION);
        }
        co_code.push(m.cp_op);
        co_code.push((m.arg & 0xFF) as u8);
        positions.push(NO_LOCATION);
    }
    // A module code object always reports `co_firstlineno == 1` in CPython
    // regardless of where its first statement sits (leading blank lines,
    // comments — test_opcodes `test_setup_annotations_line`). Other code
    // objects start at their first instruction's line.
    let firstlineno = if code.name == "<module>" {
        1
    } else {
        code.linetable.first().copied().unwrap_or(1)
    };
    for i in 0..n {
        if fused[i] {
            // Zero-width: the RETURN_CONST unit emitted for `i - 1`
            // stands for this instruction too (same `f_lasti`).
            let prev = inst_offsets.last().copied().unwrap_or(0);
            inst_offsets.push(prev);
            continue;
        }
        let li = loc_src[i];
        // WeavePy's linetable uses 0 as the NO_LOCATION sentinel; the
        // presentation layer uses -1 (CPython's convention) so that
        // *real* line 0 — the module's opening RESUME, see below —
        // stays representable.
        let raw = code.linetable.get(li).copied().unwrap_or(firstlineno) as i32;
        let line = if raw == 0 { -1 } else { raw };
        // PEP-657 columns, when the compiler tracked them for this
        // instruction. `col`/`end_col` are byte offsets (`-1` = unknown);
        // `end_lineno` is `0` when unknown (fall back to the start line).
        let cs = code.coltable.get(li).copied().unwrap_or_default();
        let end_lineno = if cs.end_lineno != 0 {
            cs.end_lineno as i32
        } else {
            line
        };
        let pos = if module_resume_at(code, i) {
            // CPython stamps a module's opening RESUME with the real
            // location (0, 1, 0, 0) — compile.c codegen_enter_anonymous_scope
            // sets loc.lineno = 0 for module scope (test_compile's
            // test_leading_newlines grades co_lines() starting at 0).
            Position {
                lineno: 0,
                end_lineno: 1,
                col: Some(0),
                end_col: Some(0),
            }
        } else {
            Position {
                lineno: line,
                end_lineno,
                col: (cs.col >= 0).then_some(cs.col as u32),
                end_col: (cs.end_col >= 0).then_some(cs.end_col as u32),
            }
        };
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
        co_linetable: encode_linetable(
            code,
            &ext,
            &mapped,
            &fused,
            &loc_src,
            firstlineno,
            prologue_units,
        ),
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

/// Whether instruction `i` is a module's opening `RESUME`, which
/// CPython locates at the synthetic (0, 1, 0, 0) span rather than
/// NO_LOCATION (compile.c sets `loc.lineno = 0` for module scope).
fn module_resume_at(code: &CodeObject, i: usize) -> bool {
    i == 0
        && code.name == "<module>"
        && code.instructions.first().map(|x| x.op) == Some(OpCode::Resume)
        && code.linetable.first().copied().unwrap_or(1) == 0
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
fn encode_linetable(
    code: &CodeObject,
    ext: &[usize],
    mapped: &[MappedOp],
    fused: &[bool],
    loc_src: &[usize],
    firstlineno: u32,
    prologue_units: usize,
) -> Vec<u8> {
    const CODE_NO_COLUMNS: u8 = 13;
    const CODE_LONG: u8 = 14;
    const CODE_NO_LOCATION: u8 = 15;
    let mut out = Vec::new();
    let mut prev_line = firstlineno as i32;
    // The synthesized MAKE_CELL/COPY_FREE_VARS prologue is NO_LOCATION.
    let mut remaining = prologue_units;
    while remaining > 0 {
        let chunk = remaining.min(8);
        out.push(0x80 | (CODE_NO_LOCATION << 3) | ((chunk - 1) as u8));
        remaining -= chunk;
    }
    for i in 0..code.instructions.len() {
        if fused[i] {
            // Zero-width on the wire; the RETURN_CONST entry for the
            // preceding instruction covers the merged location.
            continue;
        }
        let li = loc_src[i];
        let line = code.linetable.get(li).copied().unwrap_or(firstlineno) as i32;
        let units = ext[i] + 1 + cache_entries(mapped[i].cp_op);
        // Each location entry covers 1..=8 code units; split if longer.
        let mut remaining = units;
        // A module's opening RESUME carries the real location
        // (0, 1, 0, 0) on the wire, like CPython's.
        if module_resume_at(code, i) {
            out.push(0x80 | (CODE_LONG << 3) | ((units.min(8) - 1) as u8));
            push_loc_svarint(&mut out, -prev_line, 0);
            push_loc_varint(&mut out, 1, 0); // end_line delta
            push_loc_varint(&mut out, 1, 0); // col 0, stored +1
            push_loc_varint(&mut out, 1, 0); // end_col 0, stored +1
            prev_line = 0;
            continue;
        }
        // Line 0 is WeavePy's NO_LOCATION sentinel — the entry form 15
        // carries no line delta and doesn't advance the running line.
        if line == 0 {
            while remaining > 0 {
                let chunk = remaining.min(8);
                out.push(0x80 | (CODE_NO_LOCATION << 3) | ((chunk - 1) as u8));
                remaining -= chunk;
            }
            continue;
        }
        let cs = code.coltable.get(li).copied().unwrap_or_default();
        let has_cols = cs.col >= 0 && cs.end_col >= 0;
        let end_line_delta = if cs.end_lineno != 0 {
            (cs.end_lineno as i32 - line).max(0) as u32
        } else {
            0
        };
        let mut delta = line - prev_line;
        while remaining > 0 {
            let chunk = remaining.min(8);
            if has_cols {
                out.push(0x80 | (CODE_LONG << 3) | ((chunk - 1) as u8));
                push_loc_svarint(&mut out, delta, 0);
                push_loc_varint(&mut out, end_line_delta, 0);
                // Columns are stored +1 so `0` means "None" (locations.md).
                push_loc_varint(&mut out, (cs.col + 1) as u32, 0);
                push_loc_varint(&mut out, (cs.end_col + 1) as u32, 0);
            } else {
                out.push(0x80 | (CODE_NO_COLUMNS << 3) | ((chunk - 1) as u8));
                push_loc_svarint(&mut out, delta, 0);
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

/// Encode the exception range table. Offsets are converted to code units
/// via `starts`.
fn encode_exception_table(code: &CodeObject, starts: &[usize]) -> Vec<u8> {
    let mut out = Vec::new();
    let n = code.instructions.len();
    for h in &code.exception_table {
        let start = starts.get(h.start as usize).copied().unwrap_or(0);
        let end = starts
            .get((h.end as usize).min(n))
            .copied()
            .unwrap_or(start);
        let target = starts.get(h.handler as usize).copied().unwrap_or(0);
        let length = end.saturating_sub(start);
        // First byte of the entry is marked with 0x80.
        push_exc_varint(&mut out, start as u32, 0x80);
        push_exc_varint(&mut out, length as u32, 0);
        push_exc_varint(&mut out, target as u32, 0);
        // depth_and_lasti = (depth << 1) | lasti.
        push_exc_varint(&mut out, (h.depth << 1) | u32::from(h.push_lasti), 0);
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
        push(
            h.handler as usize,
            i64::from(h.depth) + 1 + i64::from(h.push_lasti),
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
        O::LoadConst
        | O::LoadName
        | O::LoadGlobal
        | O::LoadFast
        | O::LoadFastAndClear
        | O::LoadDeref
        | O::LoadClosure
        | O::LoadClassderef
        | O::LoadBuildClass
        | O::LoadAssertionError
        | O::PushNull
        | O::LoadMethodAttr
        | O::CopyTop
        | O::MatchSequence
        | O::MatchMapping
        | O::GetLen
        | O::GetAnext
        | O::ImportFrom
        | O::BeforeWith
        | O::BeforeAsyncWith
        | O::WithExceptStart
        // Every resume pushes the sent value; the prologue's POP_TOP
        // (first resume) or the dance's SEND consumes it.
        | O::ReturnGenerator
        // Inserts the previous exception under TOS (CPython 3.13).
        | O::PushExcInfo => 1,
        O::PopTop
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
        | O::PrepReraiseStar => -1,
        O::StoreAttr | O::MatchClass | O::DeleteSubscr | O::MapAdd | O::EndAsyncFor
        | O::BuildSlice => -2,
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
        O::CallEx => -a - 2,
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
        // Pops value + expression text (+ spec when bit 2 is set),
        // pushes the Interpolation.
        O::BuildInterpolation => {
            if arg & 0x04 != 0 {
                -2
            } else {
                -1
            }
        }
        // Pops the strings and interpolations tuples, pushes the Template.
        O::BuildTemplate => -1,
        O::RaiseVarargs => -a,
        O::Reraise => {
            if arg == 0 {
                -1
            } else {
                0
            }
        }
        O::ForIter => 1,
        // CPython's END_FOR pops the (statically modeled) next value; the
        // trailing POP_TOP then pops the iterator. The pair is dead at
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

/// Instruction count a raw expands to: `RETURN_CONST` unfuses back into
/// `LOAD_CONST` + `RETURN_VALUE`, superinstructions into their two
/// halves; everything else is 1:1.
fn raw_expansion(cp_op: u8, arg: u32) -> usize {
    match cp_op {
        op::RETURN_CONST
        | op::LOAD_FAST_LOAD_FAST
        | op::STORE_FAST_LOAD_FAST
        | op::STORE_FAST_STORE_FAST => 2,
        // Callable-flagged global load unfuses into LoadGlobal + PushNull.
        op::LOAD_GLOBAL if arg & 1 != 0 => 2,
        _ => 1,
    }
}

/// Per-raw expansion counts. The leading `MAKE_CELL`/`COPY_FREE_VARS`
/// prologue expands to zero instructions: WeavePy's frame setup does
/// both natively and [`encode`] re-synthesizes the prologue, so
/// stripping it here keeps decode∘encode the identity.
fn raw_expansions(raws: &[DecodedRaw]) -> Vec<usize> {
    let mut out: Vec<usize> = raws.iter().map(|r| raw_expansion(r.cp_op, r.arg)).collect();
    for (i, r) in raws.iter().enumerate() {
        match r.cp_op {
            op::MAKE_CELL | op::COPY_FREE_VARS => out[i] = 0,
            _ => break,
        }
    }
    out
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

/// Translate decoded raws into WeavePy instructions, recomputing relative
/// jump args back into the instruction-delta domain.
fn decode_instructions(
    raws: &[DecodedRaw],
    slots: &SlotMap,
    constants: &[Constant],
) -> Option<Vec<Instruction>> {
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
    for (idx, r) in raws.iter().enumerate() {
        if expansions[idx] == 0 {
            // The MAKE_CELL/COPY_FREE_VARS prologue — stripped (the
            // encoder re-synthesizes it; frame setup does the work).
            continue;
        }
        if leaders.contains(&r.start_unit) {
            shadow.clear();
        }
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
                if r.arg & 1 != 0 {
                    pop(&mut shadow); // kwargs dict
                }
                pop(&mut shadow); // args tuple
                pop(&mut shadow); // self-or-null (always NULL here)
                pop(&mut shadow); // callable
                shadow.push(SlotKind::Other);
            }
            _ => {
                let net = map_from_cpython(r.cp_op, r.arg, slots)
                    .map(|(o, a)| stack_effects(o, a).0)
                    .unwrap_or(0)
                    + match r.cp_op {
                        op::LOAD_FAST_LOAD_FAST => 2,
                        op::STORE_FAST_LOAD_FAST => 0,
                        op::STORE_FAST_STORE_FAST => -2,
                        _ => 0,
                    };
                if net < 0 {
                    for _ in 0..(-net) {
                        pop(&mut shadow);
                    }
                } else {
                    for _ in 0..net {
                        shadow.push(SlotKind::Other);
                    }
                }
            }
        }
        if matches!(
            r.cp_op,
            op::JUMP_FORWARD
                | op::JUMP_BACKWARD
                | op::JUMP_BACKWARD_NO_INTERRUPT
                | op::RETURN_VALUE
                | op::RETURN_CONST
                | op::RERAISE
                | op::RAISE_VARARGS
        ) {
            // Whatever follows starts a fresh block.
            shadow.clear();
        }
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
        if r.cp_op == op::LOAD_GLOBAL && r.arg & 1 != 0 {
            out.push(Instruction::new(OpCode::LoadGlobal, r.arg >> 1));
            out.push(Instruction::new(OpCode::PushNull, 0));
            continue;
        }
        if r.cp_op == op::RETURN_CONST {
            out.push(Instruction::new(OpCode::LoadConst, r.arg));
            // arg 1 keeps the pair RETURN_CONST-fusable on re-encode
            // (round-trip stability).
            out.push(Instruction::new(OpCode::ReturnValue, 1));
            continue;
        }
        // Superinstructions unfuse into their two halves (args packed
        // 4 bits each; both are always true locals, never closures).
        if matches!(
            r.cp_op,
            op::LOAD_FAST_LOAD_FAST | op::STORE_FAST_LOAD_FAST | op::STORE_FAST_STORE_FAST
        ) {
            let (a1, a2) = (r.arg >> 4, r.arg & 0x0F);
            let (op1, op2) = match r.cp_op {
                op::LOAD_FAST_LOAD_FAST => (OpCode::LoadFast, OpCode::LoadFast),
                op::STORE_FAST_LOAD_FAST => (OpCode::StoreFast, OpCode::LoadFast),
                _ => (OpCode::StoreFast, OpCode::StoreFast),
            };
            out.push(Instruction::new(op1, a1));
            out.push(Instruction::new(op2, a2));
            continue;
        }
        if r.cp_op == op::PUSH_NULL {
            out.push(Instruction::new(OpCode::PushNull, 0));
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
    Some(out)
}

/// Is `cp_op` outside CPython 3.13's known opcode set (including the
/// specialized adaptive forms)? `ceval` raises `SystemError: unknown
/// opcode N` when it reaches one (test_code.test_invalid_bytecode).
/// The unknown ranges come from `_opcode_metadata.opmap` ∪
/// `_specialized_opmap`: every byte outside them is a real opcode.
#[must_use]
pub fn is_unknown_opcode(cp_op: u8) -> bool {
    matches!(cp_op, 119..=148 | 223..=235 | 255)
}

/// First unknown opcode byte in a raw `co_code` stream, if any. Even
/// offsets are always opcode bytes in the 3.13 wire format (`CACHE`
/// filler included), so a simple stride-2 scan suffices.
#[must_use]
pub fn first_unknown_opcode(co_code: &[u8]) -> Option<u8> {
    co_code
        .iter()
        .step_by(2)
        .copied()
        .find(|&o| is_unknown_opcode(o))
}

/// Decode a CPython-3.13 `co_code` stream back into WeavePy instructions.
/// Inverts [`encode`] for the canonical opcode set WeavePy emits.
/// `slots` maps wire localsplus slots back to internal deref indices.
///
/// Returns `None` if the stream contains an opcode WeavePy can't map back.
#[must_use]
pub fn decode(co_code: &[u8], slots: &SlotMap, constants: &[Constant]) -> Option<Vec<Instruction>> {
    let raws = decode_raws(co_code);
    decode_instructions(&raws, slots, constants)
}

/// The reconstructed pieces of a [`CodeObject`] recovered from its
/// CPython-3.13 wire form (RFC 0033). Constants, names, arg counts, and
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
    /// Instruction indices decoded from `JUMP_BACKWARD_NO_INTERRUPT`
    /// (see [`crate::CodeObject::no_interrupt_jumps`]); preserved so a
    /// re-encode round-trips the wire byte-for-byte.
    pub no_interrupt_jumps: Vec<u32>,
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
    for (name, &kind) in localsplusnames.iter().zip(localspluskinds.iter()) {
        // An escaping parameter carries LOCAL|CELL on one shared slot:
        // it belongs to *both* co_varnames and co_cellvars.
        if kind & CO_FAST_LOCAL != 0 {
            varnames.push(name.clone());
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
    let instructions = decode_instructions(&raws, &slots, constants)?;
    let (linetable, coltable) = decode_linetable(co_linetable, &raws, firstlineno);
    let exception_table = decode_exception_table(co_exceptiontable, &raws);
    // Recover the NO_INTERRUPT flag per decoded instruction index (the
    // internal stream folds both backward jumps into one opcode).
    let expansions = raw_expansions(&raws);
    let first = raw_first_instr(&expansions);
    let mut no_interrupt_jumps = Vec::new();
    for (idx, r) in raws.iter().enumerate() {
        if r.cp_op == op::JUMP_BACKWARD_NO_INTERRUPT && expansions[idx] > 0 {
            no_interrupt_jumps.push(first[idx] as u32);
        }
    }
    Some(DecodedCode {
        instructions,
        linetable,
        coltable,
        exception_table,
        varnames,
        cellvars,
        freevars,
        no_interrupt_jumps,
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
    let mut lines = Vec::new();
    let mut cols = Vec::new();
    let expansions = raw_expansions(raws);
    for (r, &exp) in raws.iter().zip(expansions.iter()) {
        let line = unit_lines.get(r.start_unit).copied().unwrap_or(firstlineno);
        let col = unit_cols.get(r.start_unit).copied().unwrap_or_default();
        // A RETURN_CONST raw expands to two instructions sharing the
        // fused unit's location; the stripped prologue contributes none.
        for _ in 0..exp {
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

/// Decode the exception range table back into [`ExcHandler`]s, converting
/// code-unit offsets to WeavePy instruction indices.
fn decode_exception_table(table: &[u8], raws: &[DecodedRaw]) -> Vec<ExcHandler> {
    let unit_to_idx = unit_index_map(raws);
    let first = raw_first_instr(&raw_expansions(raws));
    let total = *first.last().unwrap_or(&0) as u32;
    let map_unit = |unit: usize| -> u32 {
        unit_to_idx
            .get(&unit)
            .and_then(|i| first.get(*i))
            .map(|i| *i as u32)
            .unwrap_or(total)
    };
    let mut out = Vec::new();
    let mut pos = 0usize;
    while pos < table.len() {
        let start_unit = read_exc_field(table, &mut pos) as usize;
        if pos >= table.len() {
            break;
        }
        let length = read_exc_field(table, &mut pos) as usize;
        let target_unit = read_exc_field(table, &mut pos) as usize;
        let dl = read_exc_field(table, &mut pos);
        out.push(ExcHandler {
            start: map_unit(start_unit),
            end: map_unit(start_unit + length),
            handler: map_unit(target_unit),
            depth: dl >> 1,
            // Low bit of the depth/lasti word is CPython's lasti flag.
            push_lasti: dl & 1 != 0,
        });
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
        op::LOAD_FAST => {
            // On a cell/free slot LOAD_FAST pushes the cell object
            // itself (closure building) — WeavePy's LoadClosure.
            if slots.is_cellish(arg) {
                (O::LoadClosure, slots.deref(arg))
            } else {
                (O::LoadFast, arg)
            }
        }
        // The checked form decodes to the same runtime op (WeavePy's
        // LoadFast always checks); the distinction is view-only.
        op::LOAD_FAST_CHECK => (O::LoadFast, arg),
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
        op::BINARY_SUBSCR => (O::BinarySubscr, 0),
        op::STORE_SUBSCR => (O::StoreSubscr, 0),
        op::DELETE_SUBSCR => (O::DeleteSubscr, 0),
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
            } else {
                (O::ImportStar, 0)
            }
        }
        op::CALL_INTRINSIC_2 => (O::PrepReraiseStar, 0),
        op::CLEANUP_THROW => (O::CleanupThrow, 0),
        op::BUILD_INTERPOLATION => (O::BuildInterpolation, arg),
        op::BUILD_TEMPLATE => (O::BuildTemplate, 0),
        op::LOAD_ASSERTION_ERROR => (O::LoadAssertionError, 0),
        op::COMPARE_OP => (O::CompareOp, CompareKind::from_arg(arg >> 5)?.as_arg()),
        op::IS_OP => (O::IsOp, arg),
        op::CONTAINS_OP => (O::ContainsOp, arg),
        op::POP_TOP => (O::PopTop, 0),
        op::COPY => (O::CopyTop, arg),
        op::SWAP => (O::Swap, arg),
        op::CALL => (O::Call, arg),
        op::CALL_KW => (O::CallKw, arg),
        op::CALL_FUNCTION_EX => (O::CallEx, arg),
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
        op::MAP_ADD => (O::MapAdd, arg),
        op::UNPACK_SEQUENCE => (O::UnpackSequence, arg),
        op::UNPACK_EX => (O::UnpackEx, ((arg & 0xFF) << 8) | ((arg >> 8) & 0xFF)),
        op::DICT_UPDATE => (O::DictUpdate, 0),
        op::DICT_MERGE => (O::DictUpdate, 1),
        op::MAKE_FUNCTION => (O::MakeFunction, arg),
        op::SET_FUNCTION_ATTRIBUTE => (O::SetFunctionAttribute, arg),
        op::BUILD_SLICE => (O::BuildSlice, arg),
        op::LOAD_BUILD_CLASS => (O::LoadBuildClass, 0),
        op::LOAD_CLASSDEREF_WEAVEPY => (O::LoadClassderef, slots.deref(arg)),
        op::LOAD_FROM_DICT_OR_DEREF => (O::LoadClassdictOrDeref, slots.deref(arg)),
        op::LOAD_FROM_DICT_OR_GLOBALS => (O::LoadClassdictOrGlobal, arg),
        op::RAISE_VARARGS => (O::RaiseVarargs, arg),
        op::CHECK_EXC_MATCH => (O::CheckExcMatch, 0),
        op::CHECK_EG_MATCH => (O::CheckEGMatch, 0),
        op::PUSH_EXC_INFO => (O::PushExcInfo, 0),
        op::POP_EXCEPT => (O::PopExcept, 0),
        op::RERAISE => (O::Reraise, arg),
        op::BEFORE_WITH => (O::BeforeWith, 0),
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
        op::END_ASYNC_FOR => (O::EndAsyncFor, 0),
        op::BEFORE_ASYNC_WITH => (O::BeforeAsyncWith, 0),
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
    /// The CPython-3.13 wire view of this code object (RFC 0033).
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
            // the wire format both read it as `COPY 1`.
            if ins.op == crate::bytecode::OpCode::CopyTop && ins.arg == 0 {
                crate::Instruction::new(ins.op, 1)
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

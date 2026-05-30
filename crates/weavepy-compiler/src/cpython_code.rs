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
use crate::{CodeObject, ExcHandler};

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
    pub const END_SEND: u8 = 12;
    pub const FORMAT_SIMPLE: u8 = 14;
    pub const FORMAT_WITH_SPEC: u8 = 15;
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
    pub const RETURN_GENERATOR: u8 = 35;
    pub const RETURN_VALUE: u8 = 36;
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
    pub const CALL_KW: u8 = 57;
    pub const COMPARE_OP: u8 = 58;
    pub const CONTAINS_OP: u8 = 59;
    pub const COPY: u8 = 61;
    pub const DELETE_ATTR: u8 = 63;
    pub const DELETE_DEREF: u8 = 64;
    pub const DELETE_FAST: u8 = 65;
    pub const DELETE_GLOBAL: u8 = 66;
    pub const DELETE_NAME: u8 = 67;
    pub const DICT_UPDATE: u8 = 69;
    pub const EXTENDED_ARG: u8 = 71;
    pub const FOR_ITER: u8 = 72;
    pub const GET_AWAITABLE: u8 = 73;
    pub const IMPORT_FROM: u8 = 74;
    pub const IMPORT_NAME: u8 = 75;
    pub const IS_OP: u8 = 76;
    pub const JUMP_BACKWARD: u8 = 77;
    pub const JUMP_FORWARD: u8 = 79;
    pub const LIST_APPEND: u8 = 80;
    pub const LOAD_ATTR: u8 = 82;
    pub const LOAD_CONST: u8 = 83;
    pub const LOAD_DEREF: u8 = 84;
    pub const LOAD_FAST: u8 = 85;
    pub const LOAD_FROM_DICT_OR_DEREF: u8 = 89;
    pub const LOAD_GLOBAL: u8 = 91;
    pub const LOAD_NAME: u8 = 92;
    pub const MAKE_CELL: u8 = 94;
    pub const MAP_ADD: u8 = 95;
    pub const MATCH_CLASS: u8 = 96;
    pub const POP_JUMP_IF_FALSE: u8 = 97;
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
}

/// CPython 3.13 `HAVE_ARGUMENT` boundary: opcodes `>=` this take an
/// inline argument. Opcodes below it ignore the (still-present) arg byte.
pub const HAVE_ARGUMENT: u8 = 44;

/// CPython's `MAGIC_NUMBER` for the 3.13 series (`importlib.util.MAGIC_NUMBER`).
pub const MAGIC_NUMBER: [u8; 4] = [0xf3, 0x0d, 0x0d, 0x0a];

/// CALL_INTRINSIC_1 sub-op: `INTRINSIC_IMPORT_STAR`.
const INTRINSIC_IMPORT_STAR: u32 = 2;
/// CALL_INTRINSIC_1 sub-op: `INTRINSIC_UNARY_POSITIVE`.
const INTRINSIC_UNARY_POSITIVE: u32 = 5;

/// Number of inline-`CACHE` code units that follow `cp_op` in CPython
/// 3.13 (`_PyOpcode_Caches`). Everything not listed has none.
#[must_use]
pub fn cache_entries(cp_op: u8) -> usize {
    match cp_op {
        op::LOAD_GLOBAL => 4,
        op::LOAD_ATTR => 9,
        op::STORE_ATTR => 4,
        op::CALL => 3,
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
        | op::POP_JUMP_IF_FALSE => 1,
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
            | op::JUMP_FORWARD
            | op::POP_JUMP_IF_FALSE
            | op::POP_JUMP_IF_TRUE
            | op::SEND
    )
}

/// `True` if `cp_op` jumps backwards (arg subtracted from the next pc).
#[must_use]
pub fn is_backward_jump(cp_op: u8) -> bool {
    cp_op == op::JUMP_BACKWARD
}

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
/// layout. `nlocals` is the count of plain local variables — the offset
/// at which cell/free vars start in `co_localsplusnames`.
#[derive(Clone, Copy)]
struct MappedOp {
    cp_op: u8,
    arg: u32,
}

/// Map one WeavePy [`Instruction`] to its CPython opcode + arg. `nlocals`
/// is `varnames.len()` (deref opcodes index into the merged localsplus
/// array, so their arg is shifted by `nlocals`).
fn map_to_cpython(ins: Instruction, nlocals: u32) -> MappedOp {
    use OpCode as O;
    let (cp_op, arg) = match ins.op {
        O::Nop => (op::NOP, 0),
        O::Resume => (op::RESUME, ins.arg),
        O::LoadConst => (op::LOAD_CONST, ins.arg),
        O::LoadName => (op::LOAD_NAME, ins.arg),
        // CPython packs a "push NULL" flag in bit 0; the name index is arg >> 1.
        O::LoadGlobal => (op::LOAD_GLOBAL, ins.arg << 1),
        O::LoadFast => (op::LOAD_FAST, ins.arg),
        O::StoreFast => (op::STORE_FAST, ins.arg),
        O::StoreGlobal => (op::STORE_GLOBAL, ins.arg),
        O::StoreName => (op::STORE_NAME, ins.arg),
        O::DeleteFast => (op::DELETE_FAST, ins.arg),
        O::DeleteGlobal => (op::DELETE_GLOBAL, ins.arg),
        O::DeleteName => (op::DELETE_NAME, ins.arg),
        O::LoadDeref => (op::LOAD_DEREF, ins.arg + nlocals),
        O::StoreDeref => (op::STORE_DEREF, ins.arg + nlocals),
        O::MakeCell => (op::MAKE_CELL, ins.arg + nlocals),
        // 3.13 has no real LOAD_CLOSURE opcode; cells live in the fast
        // array and are loaded with LOAD_FAST.
        O::LoadClosure => (op::LOAD_FAST, ins.arg + nlocals),
        // bit 0 = "is method load"; the name index is arg >> 1.
        O::LoadAttr => (op::LOAD_ATTR, ins.arg << 1),
        O::StoreAttr => (op::STORE_ATTR, ins.arg),
        O::DeleteAttr => (op::DELETE_ATTR, ins.arg),
        O::BinarySubscr => (op::BINARY_SUBSCR, 0),
        O::StoreSubscr => (op::STORE_SUBSCR, 0),
        O::DeleteSubscr => (op::DELETE_SUBSCR, 0),
        O::BinaryOp => (
            op::BINARY_OP,
            BinOpKind::from_arg(ins.arg).map_or(ins.arg, binop_to_nb),
        ),
        O::UnaryOp => match UnaryKind::from_arg(ins.arg) {
            Some(UnaryKind::Neg) => (op::UNARY_NEGATIVE, 0),
            Some(UnaryKind::Not) => (op::UNARY_NOT, 0),
            Some(UnaryKind::Invert) => (op::UNARY_INVERT, 0),
            // No dedicated opcode for unary `+` in 3.13.
            _ => (op::CALL_INTRINSIC_1, INTRINSIC_UNARY_POSITIVE),
        },
        // bits 5+ carry the comparison index; bit 4 = "convert to bool".
        O::CompareOp => (op::COMPARE_OP, (ins.arg << 5) | 16),
        O::IsOp => (op::IS_OP, ins.arg),
        O::ContainsOp => (op::CONTAINS_OP, ins.arg),
        O::PopTop => (op::POP_TOP, 0),
        O::CopyTop => (op::COPY, 1),
        O::Swap => (op::SWAP, ins.arg),
        O::Call => (op::CALL, ins.arg),
        O::CallKw => (op::CALL_KW, ins.arg),
        O::CallEx => (op::CALL_FUNCTION_EX, ins.arg),
        O::ReturnValue => (op::RETURN_VALUE, 0),
        O::PopJumpIfFalse => (op::POP_JUMP_IF_FALSE, ins.arg),
        O::PopJumpIfTrue => (op::POP_JUMP_IF_TRUE, ins.arg),
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
        O::SetAdd => (op::SET_ADD, ins.arg),
        O::MapAdd => (op::MAP_ADD, ins.arg),
        O::UnpackSequence => (op::UNPACK_SEQUENCE, ins.arg),
        O::UnpackEx => (op::UNPACK_EX, ins.arg),
        O::DictUpdate => (op::DICT_UPDATE, ins.arg),
        O::MakeFunction => (op::MAKE_FUNCTION, ins.arg),
        O::BuildSlice => (op::BUILD_SLICE, ins.arg),
        O::LoadBuildClass => (op::LOAD_BUILD_CLASS, 0),
        O::LoadClassderef => (op::LOAD_FROM_DICT_OR_DEREF, ins.arg + nlocals),
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
        O::FormatValue => {
            if ins.arg & 0x04 != 0 {
                (op::FORMAT_WITH_SPEC, ins.arg)
            } else {
                (op::FORMAT_SIMPLE, ins.arg)
            }
        }
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
}

const CO_FAST_LOCAL: u8 = 0x20;
const CO_FAST_CELL: u8 = 0x40;
const CO_FAST_FREE: u8 = 0x80;

/// Build the merged `co_localsplusnames` / `co_localspluskinds` pair.
fn build_localsplus(code: &CodeObject) -> (Vec<String>, Vec<u8>) {
    let mut names = Vec::with_capacity(code.varnames.len() + code.cellvars.len());
    let mut kinds = Vec::with_capacity(names.capacity());
    for v in &code.varnames {
        names.push(v.clone());
        kinds.push(CO_FAST_LOCAL);
    }
    for c in &code.cellvars {
        names.push(c.clone());
        kinds.push(CO_FAST_CELL);
    }
    for f in &code.freevars {
        names.push(f.clone());
        kinds.push(CO_FAST_FREE);
    }
    (names, kinds)
}

/// Encode `code` into its CPython-3.13 wire view.
#[must_use]
pub fn encode(code: &CodeObject) -> CpythonCode {
    let nlocals = code.varnames.len() as u32;
    let n = code.instructions.len();
    let mapped: Vec<MappedOp> = code
        .instructions
        .iter()
        .map(|ins| map_to_cpython(*ins, nlocals))
        .collect();

    // Fixpoint: jump args depend on code-unit offsets, which depend on
    // how many EXTENDED_ARG units precede each instruction.
    let mut ext: Vec<usize> = mapped
        .iter()
        .map(|m| {
            if is_rel_jump(m.cp_op) {
                0
            } else {
                ext_count(m.arg)
            }
        })
        .collect();
    let mut starts = vec![0usize; n + 1];
    let mut args: Vec<u32> = mapped.iter().map(|m| m.arg).collect();

    for _ in 0..16 {
        // Recompute code-unit start offsets.
        let mut off = 0usize;
        for i in 0..n {
            starts[i] = off;
            off += ext[i] + 1 + cache_entries(mapped[i].cp_op);
        }
        starts[n] = off;

        let mut changed = false;
        for i in 0..n {
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
    let firstlineno = code.linetable.first().copied().unwrap_or(1);
    for i in 0..n {
        let line = code.linetable.get(i).copied().unwrap_or(firstlineno) as i32;
        let pos = Position {
            lineno: line,
            end_lineno: line,
            col: None,
            end_col: None,
        };
        let arg = args[i];
        // EXTENDED_ARG units carry the high base-256 digits, MSB first.
        for k in (1..=ext[i]).rev() {
            let byte = ((arg >> (8 * k)) & 0xFF) as u8;
            co_code.push(op::EXTENDED_ARG);
            co_code.push(byte);
            positions.push(pos);
        }
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
        co_linetable: encode_linetable(code, &ext, &mapped, firstlineno),
        co_exceptiontable: encode_exception_table(code, &starts),
        co_code,
        localsplusnames,
        localspluskinds,
        stacksize: compute_stacksize(code),
        firstlineno,
        positions,
    }
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

/// Encode the PEP 626 location table using the "no-column" entry form
/// (`code = 13`): line-accurate, columns reported as `None`.
fn encode_linetable(
    code: &CodeObject,
    ext: &[usize],
    mapped: &[MappedOp],
    firstlineno: u32,
) -> Vec<u8> {
    const CODE_NO_COLUMNS: u8 = 13;
    let mut out = Vec::new();
    let mut prev_line = firstlineno as i32;
    for i in 0..code.instructions.len() {
        let line = code.linetable.get(i).copied().unwrap_or(firstlineno) as i32;
        let units = ext[i] + 1 + cache_entries(mapped[i].cp_op);
        // Each location entry covers 1..=8 code units; split if longer.
        let mut remaining = units;
        let mut delta = line - prev_line;
        while remaining > 0 {
            let chunk = remaining.min(8);
            let first = 0x80 | (CODE_NO_COLUMNS << 3) | ((chunk - 1) as u8);
            out.push(first);
            push_loc_svarint(&mut out, delta, 0);
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
        // depth_and_lasti = (depth << 1) | lasti; WeavePy has no lasti bit.
        push_exc_varint(&mut out, h.depth << 1, 0);
    }
    out
}

// ---------- stack size (best-effort) ----------

/// Best-effort maximum operand-stack depth via a linear scan. Exactness
/// isn't required (the VM grows its stack dynamically); this only feeds
/// the informational `co_stacksize` attribute.
fn compute_stacksize(code: &CodeObject) -> u32 {
    let mut depth: i64 = 0;
    let mut max: i64 = 1;
    for ins in &code.instructions {
        depth += stack_effect(ins.op, ins.arg);
        if depth < 0 {
            depth = 0;
        }
        if depth > max {
            max = depth;
        }
    }
    u32::try_from(max).unwrap_or(u32::MAX)
}

fn stack_effect(opcode: OpCode, arg: u32) -> i64 {
    use OpCode as O;
    let a = i64::from(arg);
    match opcode {
        O::LoadConst
        | O::LoadName
        | O::LoadGlobal
        | O::LoadFast
        | O::LoadDeref
        | O::LoadClosure
        | O::LoadClassderef
        | O::LoadBuildClass => 1,
        O::PopTop
        | O::StoreName
        | O::StoreGlobal
        | O::StoreFast
        | O::StoreDeref
        | O::ReturnValue
        | O::PopJumpIfFalse
        | O::PopJumpIfTrue
        | O::ImportStar => -1,
        O::CopyTop => 1,
        O::StoreAttr => -2,
        O::StoreSubscr => -3,
        O::BinaryOp | O::CompareOp | O::IsOp | O::ContainsOp | O::BinarySubscr => -1,
        O::Call => -a,
        O::BuildList | O::BuildTuple | O::BuildSet | O::BuildString => 1 - a,
        O::BuildMap => 1 - 2 * a,
        O::UnpackSequence => a - 1,
        _ => 0,
    }
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

/// Translate decoded raws into WeavePy instructions, recomputing relative
/// jump args back into the instruction-delta domain.
fn decode_instructions(raws: &[DecodedRaw], nlocals: u32) -> Option<Vec<Instruction>> {
    let unit_to_idx = unit_index_map(raws);
    let mut out = Vec::with_capacity(raws.len());
    for (idx, r) in raws.iter().enumerate() {
        let op = map_from_cpython(r.cp_op, r.arg, nlocals)?;
        let arg = if is_rel_jump(r.cp_op) {
            let next_unit = r.start_unit + r.size;
            let target_unit = if is_backward_jump(r.cp_op) {
                next_unit.saturating_sub(r.arg as usize)
            } else {
                next_unit + r.arg as usize
            };
            let target_idx = *unit_to_idx.get(&target_unit).unwrap_or(&raws.len());
            if is_backward_jump(r.cp_op) {
                (idx + 1).saturating_sub(target_idx) as u32
            } else {
                target_idx.saturating_sub(idx + 1) as u32
            }
        } else {
            op.1
        };
        out.push(Instruction::new(op.0, arg));
    }
    Some(out)
}

/// Decode a CPython-3.13 `co_code` stream back into WeavePy instructions.
/// Inverts [`encode`] for the canonical opcode set WeavePy emits.
/// `nlocals` is `varnames.len()` (to undo the deref offset).
///
/// Returns `None` if the stream contains an opcode WeavePy can't map back.
#[must_use]
pub fn decode(co_code: &[u8], nlocals: u32) -> Option<Vec<Instruction>> {
    let raws = decode_raws(co_code);
    decode_instructions(&raws, nlocals)
}

/// The reconstructed pieces of a [`CodeObject`] recovered from its
/// CPython-3.13 wire form (RFC 0033). Constants, names, arg counts, and
/// flags live outside this struct because they round-trip through
/// `marshal` directly; everything here is derived from the byte tables.
#[derive(Debug, Clone, Default)]
pub struct DecodedCode {
    pub instructions: Vec<Instruction>,
    pub linetable: Vec<u32>,
    pub exception_table: Vec<ExcHandler>,
    pub varnames: Vec<String>,
    pub cellvars: Vec<String>,
    pub freevars: Vec<String>,
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
) -> Option<DecodedCode> {
    let mut varnames = Vec::new();
    let mut cellvars = Vec::new();
    let mut freevars = Vec::new();
    for (name, &kind) in localsplusnames.iter().zip(localspluskinds.iter()) {
        if kind & CO_FAST_FREE != 0 {
            freevars.push(name.clone());
        } else if kind & CO_FAST_CELL != 0 {
            cellvars.push(name.clone());
        } else {
            varnames.push(name.clone());
        }
    }
    let nlocals = varnames.len() as u32;
    let raws = decode_raws(co_code);
    let instructions = decode_instructions(&raws, nlocals)?;
    let linetable = decode_linetable(co_linetable, &raws, firstlineno);
    let exception_table = decode_exception_table(co_exceptiontable, &raws);
    Some(DecodedCode {
        instructions,
        linetable,
        exception_table,
        varnames,
        cellvars,
        freevars,
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

/// Decode the PEP 626 location table into a 1-based source line per
/// WeavePy instruction. WeavePy only emits the "no-column" entry form
/// (code 13), but we tolerate the other CPython forms so a table written
/// by CPython still parses without desync.
fn decode_linetable(table: &[u8], raws: &[DecodedRaw], firstlineno: u32) -> Vec<u32> {
    let mut unit_lines: Vec<u32> = Vec::new();
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
        let delta = match code {
            15 => 0,                                 // NONE — no location
            13 => read_loc_svarint(table, &mut pos), // no columns
            14 => {
                let d = read_loc_svarint(table, &mut pos);
                let _ = read_loc_varint(table, &mut pos); // end-line delta
                let _ = read_loc_varint(table, &mut pos); // col
                let _ = read_loc_varint(table, &mut pos); // end col
                d
            }
            10..=12 => {
                let d = i32::from(code) - 10;
                let _ = read_loc_varint(table, &mut pos); // col
                let _ = read_loc_varint(table, &mut pos); // end col
                d
            }
            _ => {
                // Short forms 0..=9 carry one extra column byte, line delta 0.
                pos += 1;
                0
            }
        };
        line += delta;
        for _ in 0..length {
            unit_lines.push(line.max(0) as u32);
        }
    }
    raws.iter()
        .map(|r| unit_lines.get(r.start_unit).copied().unwrap_or(firstlineno))
        .collect()
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
    let map_unit = |unit: usize| -> u32 {
        unit_to_idx
            .get(&unit)
            .map(|i| *i as u32)
            .unwrap_or(raws.len() as u32)
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
        });
    }
    out
}

/// Map a CPython opcode + arg back to a WeavePy `(OpCode, arg)`. The arg
/// is the WeavePy-domain arg for non-jumps; jump args are recomputed by
/// the caller.
fn map_from_cpython(cp_op: u8, arg: u32, nlocals: u32) -> Option<(OpCode, u32)> {
    use OpCode as O;
    let pair = match cp_op {
        op::NOP => (O::Nop, 0),
        op::RESUME => (O::Resume, arg),
        op::LOAD_CONST => (O::LoadConst, arg),
        op::LOAD_NAME => (O::LoadName, arg),
        op::LOAD_GLOBAL => (O::LoadGlobal, arg >> 1),
        op::LOAD_FAST => {
            if arg >= nlocals {
                (O::LoadClosure, arg - nlocals)
            } else {
                (O::LoadFast, arg)
            }
        }
        op::STORE_FAST => (O::StoreFast, arg),
        op::STORE_GLOBAL => (O::StoreGlobal, arg),
        op::STORE_NAME => (O::StoreName, arg),
        op::DELETE_FAST => (O::DeleteFast, arg),
        op::DELETE_GLOBAL => (O::DeleteGlobal, arg),
        op::DELETE_NAME => (O::DeleteName, arg),
        op::LOAD_DEREF => (O::LoadDeref, arg.saturating_sub(nlocals)),
        op::STORE_DEREF => (O::StoreDeref, arg.saturating_sub(nlocals)),
        op::MAKE_CELL => (O::MakeCell, arg.saturating_sub(nlocals)),
        op::LOAD_ATTR => (O::LoadAttr, arg >> 1),
        op::STORE_ATTR => (O::StoreAttr, arg),
        op::DELETE_ATTR => (O::DeleteAttr, arg),
        op::BINARY_SUBSCR => (O::BinarySubscr, 0),
        op::STORE_SUBSCR => (O::StoreSubscr, 0),
        op::DELETE_SUBSCR => (O::DeleteSubscr, 0),
        op::BINARY_OP => (O::BinaryOp, nb_to_binop(arg)?.as_arg()),
        op::UNARY_NEGATIVE => (O::UnaryOp, UnaryKind::Neg.as_arg()),
        op::UNARY_NOT => (O::UnaryOp, UnaryKind::Not.as_arg()),
        op::UNARY_INVERT => (O::UnaryOp, UnaryKind::Invert.as_arg()),
        op::CALL_INTRINSIC_1 => {
            if arg == INTRINSIC_UNARY_POSITIVE {
                (O::UnaryOp, UnaryKind::Pos.as_arg())
            } else {
                (O::ImportStar, 0)
            }
        }
        op::COMPARE_OP => (O::CompareOp, CompareKind::from_arg(arg >> 5)?.as_arg()),
        op::IS_OP => (O::IsOp, arg),
        op::CONTAINS_OP => (O::ContainsOp, arg),
        op::POP_TOP => (O::PopTop, 0),
        op::COPY => (O::CopyTop, 0),
        op::SWAP => (O::Swap, arg),
        op::CALL => (O::Call, arg),
        op::CALL_KW => (O::CallKw, arg),
        op::CALL_FUNCTION_EX => (O::CallEx, arg),
        op::RETURN_VALUE => (O::ReturnValue, 0),
        op::POP_JUMP_IF_FALSE => (O::PopJumpIfFalse, arg),
        op::POP_JUMP_IF_TRUE => (O::PopJumpIfTrue, arg),
        op::JUMP_FORWARD => (O::JumpForward, arg),
        op::JUMP_BACKWARD => (O::JumpBackward, arg),
        op::GET_ITER => (O::GetIter, 0),
        op::FOR_ITER => (O::ForIter, arg),
        op::END_FOR => (O::EndFor, 0),
        op::BUILD_LIST => (O::BuildList, arg),
        op::BUILD_TUPLE => (O::BuildTuple, arg),
        op::BUILD_SET => (O::BuildSet, arg),
        op::BUILD_MAP => (O::BuildMap, arg),
        op::BUILD_STRING => (O::BuildString, arg),
        op::LIST_APPEND => (O::ListAppend, arg),
        op::SET_ADD => (O::SetAdd, arg),
        op::MAP_ADD => (O::MapAdd, arg),
        op::UNPACK_SEQUENCE => (O::UnpackSequence, arg),
        op::UNPACK_EX => (O::UnpackEx, arg),
        op::DICT_UPDATE => (O::DictUpdate, arg),
        op::MAKE_FUNCTION => (O::MakeFunction, arg),
        op::BUILD_SLICE => (O::BuildSlice, arg),
        op::LOAD_BUILD_CLASS => (O::LoadBuildClass, 0),
        op::LOAD_FROM_DICT_OR_DEREF => (O::LoadClassderef, arg.saturating_sub(nlocals)),
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
        op::FORMAT_SIMPLE | op::FORMAT_WITH_SPEC => (O::FormatValue, arg),
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
    #[must_use]
    pub fn to_cpython(&self) -> CpythonCode {
        encode(self)
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
        let back = decode(&cp.co_code, code.varnames.len() as u32)
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
        });

        let cp = encode(&code);
        let dc = decode_full(
            &cp.co_code,
            &cp.co_linetable,
            &cp.co_exceptiontable,
            &cp.localsplusnames,
            &cp.localspluskinds,
            cp.firstlineno,
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

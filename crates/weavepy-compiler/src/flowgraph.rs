//! Control-flow-graph optimizer: a port of CPython 3.14's
//! `Python/flowgraph.c` (RFC 0077 WS9).
//!
//! The code generator emits a flat instruction stream with range-based
//! exception coverage. This module rebuilds CPython's graph of basic
//! blocks from it, runs the same optimization passes in the same
//! order (`_PyCfg_OptimizeCodeUnit` followed by
//! `_PyCfg_OptimizedCfgToInstructionSequence`), and flattens the
//! result back into the code object, so the instruction stream, the
//! location table, and the exception table come out the way CPython's
//! assembler would produce them.
//!
//! Fidelity notes:
//!
//! - Every block keeps a slot array with CPython's growth and
//!   compaction discipline (`_Py_CArray_EnsureCapacity`, `b_iused`).
//!   `basicblock_addop` never assigns `i_except`, so an instruction
//!   appended after `label_exception_targets` inherits whatever the
//!   slot held: nothing for a fresh slot, a stale copy of a displaced
//!   instruction for a block that shrank. `NOT_TAKEN` exception
//!   coverage depends on exactly this, and the model reproduces it by
//!   never clearing a freed slot.
//! - The generator-family `SETUP_CLEANUP` wrap, the `RETURN_GENERATOR`
//!   / `POP_TOP` prefix, `COPY_FREE_VARS` and `MAKE_CELL` enter and
//!   leave the graph at the same stage as in CPython, because block
//!   sizes and slot positions at each stage decide what the later
//!   passes see.
//! - Pseudo-ops (`JUMP`, `JUMP_NO_INTERRUPT`, `JUMP_IF_FALSE`,
//!   `JUMP_IF_TRUE`, `SETUP_*`, `POP_BLOCK`) exist as [`OpCode`]
//!   variants so the passes can match on them; they are gone by the
//!   time the stream is flattened.

use weavepy_parser::ast::{BinOp, Constant as AstConstant, UnaryOp};

use crate::ast_opt;
use crate::bytecode::{
    BinOpKind, Instruction, OpCode, UnaryKind, BINARY_OP_INPLACE_FLAG, COMPARE_OP_TO_BOOL_FLAG,
};
use crate::{CodeObject, ColSpan, Constant, ExcHandler, HANDLER_DEPTH_ANCHOR_FLAG};

type BlockId = usize;
type HandlerId = usize;

/// `_PY_STACK_USE_GUIDELINE`: the longest constant sequence the folds
/// collect.
const STACK_USE_GUIDELINE: usize = 30;
/// `MIN_CONST_SEQUENCE_SIZE` for `optimize_lists_and_sets`.
const MIN_CONST_SEQUENCE_SIZE: u32 = 3;
/// `MAX_COPY_SIZE`: the largest exit block `inline_small_or_no_lineno_blocks`
/// copies.
const MAX_COPY_SIZE: usize = 4;
/// `RESUME_OPARG_DEPTH1_MASK`.
const RESUME_DEPTH1_MASK: u32 = 4;
/// `DEFAULT_BLOCK_SIZE`: the first allocation of a block's slot array.
const DEFAULT_BLOCK_SIZE: usize = 16;

/// A source location (CPython `location`): `line < 0` is `NO_LOCATION`.
/// Line `0` is a real location (a module's opening `RESUME`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Loc {
    pub line: i32,
    pub col: ColSpan,
}

const NO_LOCATION: Loc = Loc {
    line: -1,
    col: ColSpan {
        end_lineno: 0,
        col: -1,
        end_col: -1,
    },
};

/// The location of a module's opening `RESUME`: `LOCATION(0, 1, 0, 0)`
/// (`codegen_enter_scope` zeroes `loc.lineno` for module scope). It
/// is a *real* location, so `propagate_line_numbers` spreads it. The
/// assembled table keeps line 0 as its NO_LOCATION sentinel and the
/// presentation layer recognises this span (`ColSpan` `(1, 0, 0)`) to
/// tell the two apart.
pub(crate) const MODULE_RESUME_LOCATION: Loc = Loc {
    line: 0,
    col: ColSpan {
        end_lineno: 1,
        col: 0,
        end_col: 0,
    },
};

/// CPython `NEXT_LOCATION` (`{-2, -2, -2, -2}`): "take the following
/// instruction's location", resolved by the assembler after every
/// flowgraph pass. Its negative line makes it "no line" to the NOP and
/// cold-block rules, yet `propagate_line_numbers` (which tests for
/// `NO_LOCATION` exactly) leaves it alone.
const NEXT_LOCATION: Loc = Loc {
    line: -2,
    col: ColSpan {
        end_lineno: 0,
        col: -2,
        end_col: -2,
    },
};

impl Loc {
    fn from_table(line: u32, col: ColSpan) -> Self {
        if line == 0 {
            NO_LOCATION
        } else if line == crate::NEXT_LOCATION_LINE {
            NEXT_LOCATION
        } else {
            Loc {
                line: line as i32,
                col,
            }
        }
    }
}

/// CPython `cfg_instr`.
#[derive(Clone, Copy, Debug)]
struct CfgInstr {
    op: OpCode,
    arg: u32,
    loc: Loc,
    /// Jump / block-push target; also the handler-region end a
    /// `PUSH_EXC_INFO` tags for the VM (not a control edge).
    target: Option<BlockId>,
    except: Option<HandlerId>,
}

/// A zero-initialised slot (`PyMem_Calloc`).
const ZERO_INSTR: CfgInstr = CfgInstr {
    op: OpCode::Nop,
    arg: 0,
    loc: NO_LOCATION,
    target: None,
    except: None,
};

/// Handler identity behind an `i_except` pointer: the handler block
/// plus what CPython keeps on that block (`b_startdepth`,
/// `b_preserve_lasti`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct HandlerInfo {
    block: BlockId,
    depth: u32,
    lasti: bool,
}

/// CPython `basicblock`.
#[derive(Debug)]
struct Block {
    /// `b_instr` / `b_ialloc`: the slot array. Never shrinks; slots
    /// past `used` keep stale contents.
    slots: Vec<CfgInstr>,
    /// `b_iused`.
    used: usize,
    /// `b_next`.
    next: Option<BlockId>,
    /// `IS_LABEL(b_label)`: the block is a jump target.
    label: bool,
    /// `b_predecessors`.
    predecessors: i32,
    /// `b_startdepth` (only carried, not computed here).
    startdepth: i32,
    visited: bool,
    except_handler: bool,
    cold: bool,
    warm: bool,
    unsafe_locals_mask: u64,
}

impl Block {
    fn new() -> Self {
        Block {
            slots: Vec::new(),
            used: 0,
            next: None,
            label: false,
            predecessors: 0,
            startdepth: i32::MIN,
            visited: false,
            except_handler: false,
            cold: false,
            warm: false,
            unsafe_locals_mask: 0,
        }
    }

    /// `basicblock_next_instr`: reserve the next slot (growing with
    /// `_Py_CArray_EnsureCapacity`'s policy; new capacity is zeroed,
    /// freed-and-reused capacity is not).
    fn next_instr(&mut self) -> usize {
        let idx = self.used + 1;
        let alloc = self.slots.len();
        if alloc == 0 {
            let mut new_alloc = DEFAULT_BLOCK_SIZE;
            if idx >= new_alloc {
                new_alloc = idx + DEFAULT_BLOCK_SIZE;
            }
            self.slots.resize(new_alloc, ZERO_INSTR);
        } else if idx >= alloc {
            let mut new_alloc = alloc << 1;
            if idx >= new_alloc {
                new_alloc = idx + DEFAULT_BLOCK_SIZE;
            }
            self.slots.resize(new_alloc, ZERO_INSTR);
        }
        let off = self.used;
        self.used += 1;
        off
    }

    /// `basicblock_addop`: opcode, arg, target and location are set;
    /// `i_except` is whatever the slot held.
    fn addop(&mut self, op: OpCode, arg: u32, loc: Loc) {
        let off = self.next_instr();
        let s = &mut self.slots[off];
        s.op = op;
        s.arg = arg;
        s.target = None;
        s.loc = loc;
    }

    /// `basicblock_add_jump`.
    fn add_jump(&mut self, op: OpCode, target: BlockId, loc: Loc) {
        debug_assert!(!self.last().is_some_and(|l| is_jump(l.op)));
        self.addop(op, 0, loc);
        let n = self.used - 1;
        self.slots[n].target = Some(target);
    }

    /// `basicblock_insert_instruction`.
    fn insert_instruction(&mut self, pos: usize, instr: CfgInstr) {
        self.next_instr();
        let mut i = self.used - 1;
        while i > pos {
            self.slots[i] = self.slots[i - 1];
            i -= 1;
        }
        self.slots[pos] = instr;
    }

    fn instrs(&self) -> &[CfgInstr] {
        &self.slots[..self.used]
    }

    fn instrs_mut(&mut self) -> &mut [CfgInstr] {
        &mut self.slots[..self.used]
    }

    fn last(&self) -> Option<&CfgInstr> {
        if self.used > 0 {
            Some(&self.slots[self.used - 1])
        } else {
            None
        }
    }

    /// `BB_NO_FALLTHROUGH`.
    fn no_fallthrough(&self) -> bool {
        self.last()
            .is_some_and(|l| is_scope_exit(l.op) || is_unconditional_jump(l.op))
    }

    fn has_fallthrough(&self) -> bool {
        !self.no_fallthrough()
    }

    /// `basicblock_exits_scope`.
    fn exits_scope(&self) -> bool {
        self.last().is_some_and(|l| is_scope_exit(l.op))
    }

    /// `basicblock_has_eval_break`.
    fn has_eval_break(&self) -> bool {
        self.instrs().iter().any(|i| has_eval_break(i.op))
    }

    /// `basicblock_has_no_lineno`.
    fn has_no_lineno(&self) -> bool {
        self.instrs().iter().all(|i| i.loc.line < 0)
    }
}

// ---------- opcode classification (pycore_opcode_utils.h) ----------

/// `OPCODE_HAS_JUMP`.
fn is_jump(op: OpCode) -> bool {
    matches!(
        op,
        OpCode::Jump
            | OpCode::JumpNoInterrupt
            | OpCode::JumpIfFalse
            | OpCode::JumpIfTrue
            | OpCode::JumpForward
            | OpCode::JumpBackward
            | OpCode::PopJumpIfFalse
            | OpCode::PopJumpIfTrue
            | OpCode::PopJumpIfNone
            | OpCode::PopJumpIfNotNone
            | OpCode::ForIter
            | OpCode::Send
            | OpCode::EndAsyncFor
    )
}

/// `IS_BLOCK_PUSH_OPCODE`.
fn is_block_push(op: OpCode) -> bool {
    matches!(
        op,
        OpCode::SetupFinally | OpCode::SetupCleanup | OpCode::SetupWith
    )
}

/// `IS_SCOPE_EXIT_OPCODE`.
fn is_scope_exit(op: OpCode) -> bool {
    matches!(
        op,
        OpCode::ReturnValue | OpCode::RaiseVarargs | OpCode::Reraise
    )
}

/// `IS_UNCONDITIONAL_JUMP_OPCODE`.
fn is_unconditional_jump(op: OpCode) -> bool {
    matches!(
        op,
        OpCode::Jump | OpCode::JumpNoInterrupt | OpCode::JumpForward | OpCode::JumpBackward
    )
}

/// `IS_CONDITIONAL_JUMP_OPCODE`.
fn is_conditional_jump(op: OpCode) -> bool {
    matches!(
        op,
        OpCode::PopJumpIfFalse
            | OpCode::PopJumpIfTrue
            | OpCode::PopJumpIfNone
            | OpCode::PopJumpIfNotNone
    )
}

/// `IS_TERMINATOR_OPCODE`.
fn is_terminator(op: OpCode) -> bool {
    is_jump(op) || is_scope_exit(op)
}

/// `OPCODE_HAS_EVAL_BREAK`.
fn has_eval_break(op: OpCode) -> bool {
    matches!(
        op,
        OpCode::Call
            | OpCode::CallSelf
            | OpCode::CallEx
            | OpCode::Jump
            | OpCode::JumpBackward
            | OpCode::Resume
    )
}

/// `loads_const`.
fn loads_const(op: OpCode) -> bool {
    matches!(op, OpCode::LoadConst | OpCode::LoadSmallInt)
}

/// `SWAPPABLE`.
fn swappable(op: OpCode) -> bool {
    matches!(
        op,
        OpCode::StoreFast | OpCode::StoreFastMaybeNull | OpCode::PopTop
    )
}

/// `STORES_TO`.
fn stores_to(i: &CfgInstr) -> i64 {
    if matches!(i.op, OpCode::StoreFast | OpCode::StoreFastMaybeNull) {
        i64::from(i.arg)
    } else {
        -1
    }
}

/// A `UnaryOp` instruction's operator.
fn unary_kind(i: &CfgInstr) -> Option<UnaryKind> {
    if i.op == OpCode::UnaryOp {
        UnaryKind::from_arg(i.arg)
    } else {
        None
    }
}

fn is_unary_not(i: &CfgInstr) -> bool {
    unary_kind(i) == Some(UnaryKind::Not)
}

// ---------- the graph ----------

/// Codegen-side facts the flat stream doesn't carry on its own.
pub(crate) struct BuildInput<'a> {
    /// Unconditional jumps CPython's codegen emits as the plain `JUMP`
    /// pseudo-op; every other one is `JUMP_NO_INTERRUPT`.
    pub plain_jumps: &'a std::collections::HashSet<u32>,
    /// Unconditional jumps codegen flagged `JUMP_NO_INTERRUPT`.
    pub no_interrupt_jumps: &'a std::collections::HashSet<u32>,
    /// `Nop`s standing in for a `SETUP_*` pseudo-op.
    pub setup_nops: &'a std::collections::HashMap<u32, OpCode>,
    /// `Nop`s standing in for a `POP_BLOCK` pseudo-op.
    pub popblock_nops: &'a std::collections::HashSet<u32>,
    /// Conditional jumps that, together with the `COPY 1; TO_BOOL`
    /// preceding them, stand for a `JUMP_IF_FALSE` / `JUMP_IF_TRUE`
    /// pseudo-op.
    pub pseudo_cond_jumps: &'a std::collections::HashSet<u32>,
    /// Conditional jumps whose `arg` is a *backward* distance.
    pub backward_conds: &'a std::collections::HashSet<u32>,
    /// The code unit carries the PEP 479 `SETUP_CLEANUP` wrap (its
    /// range is the last exception-table entry).
    pub stopiteration_wrap: bool,
    /// Number of parameters (positional, keyword-only, `*args`,
    /// `**kwargs`): the locals that are bound at entry.
    pub nparams: usize,
}

struct Cfg {
    blocks: Vec<Block>,
    entry: BlockId,
    handlers: Vec<HandlerInfo>,
    consts: Vec<Constant>,
    /// Whether the prefix stage must insert `RETURN_GENERATOR; POP_TOP`.
    generator_prefix: bool,
    /// `co_firstlineno`, for the prefix locations.
    firstlineno: i32,
}

/// Where a flat-stream jump lands, as an instruction index.
fn flat_target(ins: Instruction, i: usize, backward: bool) -> Option<usize> {
    let from = i + 1;
    match ins.op {
        OpCode::PopJumpIfFalse
        | OpCode::PopJumpIfTrue
        | OpCode::PopJumpIfNone
        | OpCode::PopJumpIfNotNone
            if backward =>
        {
            Some(from.saturating_sub(ins.arg as usize))
        }
        OpCode::JumpForward
        | OpCode::PopJumpIfFalse
        | OpCode::PopJumpIfTrue
        | OpCode::PopJumpIfNone
        | OpCode::PopJumpIfNotNone
        | OpCode::ForIter
        | OpCode::Send => Some(from + ins.arg as usize),
        OpCode::JumpBackward => Some(from.saturating_sub(ins.arg as usize)),
        _ => None,
    }
}

impl Cfg {
    fn new_block(&mut self) -> BlockId {
        self.blocks.push(Block::new());
        self.blocks.len() - 1
    }

    /// Chain order iteration (`for (b = entryblock; b; b = b->b_next)`).
    fn chain(&self) -> Vec<BlockId> {
        let mut out = Vec::with_capacity(self.blocks.len());
        let mut cur = Some(self.entry);
        while let Some(b) = cur {
            out.push(b);
            cur = self.blocks[b].next;
        }
        out
    }

    /// `next_nonempty_block`.
    fn next_nonempty(&self, mut b: Option<BlockId>) -> Option<BlockId> {
        while let Some(id) = b {
            if self.blocks[id].used > 0 {
                return Some(id);
            }
            b = self.blocks[id].next;
        }
        None
    }

    /// `copy_basicblock`.
    fn copy_block(&mut self, from: BlockId) -> BlockId {
        let copy: Vec<CfgInstr> = self.blocks[from].instrs().to_vec();
        let id = self.new_block();
        for ins in copy {
            let off = self.blocks[id].next_instr();
            self.blocks[id].slots[off] = ins;
        }
        id
    }

    /// `basicblock_append_instructions`.
    fn append_instructions(&mut self, to: BlockId, from: BlockId) {
        let copy: Vec<CfgInstr> = self.blocks[from].instrs().to_vec();
        for ins in copy {
            let off = self.blocks[to].next_instr();
            self.blocks[to].slots[off] = ins;
        }
    }

    /// `make_cfg_traversal_stack`'s side effect: clear `b_visited`.
    fn clear_visited(&mut self) {
        for b in self.chain() {
            self.blocks[b].visited = false;
        }
    }

    /// `get_const_value`.
    fn const_value(&self, i: &CfgInstr) -> Option<Constant> {
        match i.op {
            OpCode::LoadConst => self.consts.get(i.arg as usize).cloned(),
            OpCode::LoadSmallInt => Some(Constant::Int(i64::from(i.arg))),
            _ => None,
        }
    }

    /// `add_const`: find or append, returning the pool index.
    fn add_const(&mut self, c: Constant) -> u32 {
        if let Some(i) = self.consts.iter().position(|x| *x == c) {
            return i as u32;
        }
        self.consts.push(c);
        (self.consts.len() - 1) as u32
    }

    /// `maybe_instr_make_load_smallint`.
    fn maybe_make_load_smallint(i: &mut CfgInstr, c: &Constant) -> bool {
        if let Constant::Int(v) = c {
            if (0..=255).contains(v) {
                i.op = OpCode::LoadSmallInt;
                i.arg = *v as u32;
                return true;
            }
        }
        false
    }

    /// `instr_make_load_const`.
    fn make_load_const(&mut self, b: BlockId, at: usize, c: Constant) {
        if Self::maybe_make_load_smallint(&mut self.blocks[b].slots[at], &c) {
            return;
        }
        let idx = self.add_const(c);
        let i = &mut self.blocks[b].slots[at];
        i.op = OpCode::LoadConst;
        i.arg = idx;
    }
}

/// `HAS_TARGET`: the instruction's `target` is a control edge.
fn has_target(op: OpCode) -> bool {
    is_jump(op) || is_block_push(op)
}

/// `INSTR_SET_OP0(i, NOP)`.
fn set_nop(i: &mut CfgInstr) {
    i.op = OpCode::Nop;
    i.arg = 0;
}

/// `nop_out`: NOP plus `NO_LOCATION`.
fn nop_out(i: &mut CfgInstr) {
    set_nop(i);
    i.loc = NO_LOCATION;
}

// ---------- construction from the flat stream ----------

/// Build the graph from the compiler's flat stream. Mirrors what
/// `_PyCfg_FromInstructionSequence` + `translate_jump_labels_to_targets`
/// + `mark_except_handlers` + `label_exception_targets` establish:
/// blocks split at labels and after terminators, per-instruction
/// `i_except` from the innermost covering range, `SETUP_*` /
/// `POP_BLOCK` stand-ins without coverage, the generator wrap handler,
/// and the `RESUME` depth-1 flags.
fn build(co: &mut CodeObject, input: &BuildInput<'_>) -> Cfg {
    // `co_firstlineno`: a module reports 1; anything else starts at
    // its first located instruction.
    let firstlineno = if co.name == "<module>" {
        1
    } else {
        co.linetable.iter().copied().find(|&l| l > 0).unwrap_or(1) as i32
    };
    // The generator-family prefix is inserted by the flowgraph
    // (`insert_prefix_instructions`), not the code generator; take a
    // codegen-emitted one back out.
    let mut base = 0usize;
    if co.instructions.len() >= 2
        && co.instructions[0].op == OpCode::ReturnGenerator
        && co.instructions[1].op == OpCode::PopTop
    {
        base = 2;
    }
    let generator_prefix = co.is_generator || co.is_coroutine || co.is_async_generator;

    let instrs: Vec<Instruction> = co.instructions[base..].to_vec();
    let n = instrs.len();
    let shift = |x: u32| -> usize { (x as usize).saturating_sub(base) };

    // Sentinel handler depths resolve against the stream as emitted.
    let startdepths = crate::cpython_code::compute_startdepths(co);
    let table: Vec<ExcHandler> = co
        .exception_table
        .iter()
        .map(|h| {
            let mut h = *h;
            if h.depth & HANDLER_DEPTH_ANCHOR_FLAG != 0 {
                let at = if h.depth == crate::HANDLER_DEPTH_SENTINEL {
                    h.start
                } else {
                    h.depth & !HANDLER_DEPTH_ANCHOR_FLAG
                };
                let d = startdepths.get(at as usize).copied().unwrap_or(-1);
                h.depth = u32::try_from(d).unwrap_or(0);
            }
            h.start = shift(h.start) as u32;
            h.end = shift(h.end) as u32;
            h.handler = shift(h.handler) as u32;
            h
        })
        .collect();

    // The PEP 479 wrap (`codegen_wrap_in_stopiteration_handler`): its
    // range is the table's last entry; the `SETUP_CLEANUP` heading the
    // sequence is materialised below.
    let wrap = input.stopiteration_wrap;
    let wrap_entry = if wrap {
        table.len().checked_sub(1)
    } else {
        None
    };

    // Innermost-wins owner per instruction (the wrap entry is the
    // widest range, so it loses to everything).
    let mut owner: Vec<Option<usize>> = vec![None; n];
    for (idx, h) in table.iter().enumerate() {
        let span = h.end.saturating_sub(h.start);
        for k in h.start as usize..(h.end as usize).min(n) {
            let replace = match owner[k] {
                None => true,
                Some(prev) => {
                    let p = &table[prev];
                    span < p.end.saturating_sub(p.start)
                }
            };
            if replace {
                owner[k] = Some(idx);
            }
        }
    }
    // A `SETUP_*` stand-in pushes the handler whose range opens right
    // behind it.
    let setup_target = |i: usize| -> Option<usize> {
        let next = i + 1;
        table
            .iter()
            .enumerate()
            .filter(|(_, h)| h.start as usize == next)
            .min_by_key(|(_, h)| h.end)
            .map(|(idx, _)| idx)
    };

    // Block leaders: entry, jump targets, handler entries, the
    // instruction after every terminator, and the `PUSH_EXC_INFO`
    // region ends (always terminators' successors in practice). A
    // codegen `USE_LABEL` nothing jumps to (a `with`/`try` body, a
    // loop body) starts no block: `_PyCfg_FromInstructionSequence`
    // only honours labels some `HAS_TARGET` instruction names, so the
    // `SETUP_*` and its body share a block up to the first jump.
    let mut leader = vec![false; n + 1];
    let mut labeled = vec![false; n + 1];
    leader[0] = true;
    let is_backward = |i: usize| input.backward_conds.contains(&((i + base) as u32));
    for (i, ins) in instrs.iter().enumerate() {
        if let Some(t) = flat_target(*ins, i, is_backward(i)) {
            let t = t.min(n);
            leader[t] = true;
            labeled[t] = true;
        }
        if is_terminator(ins.op) {
            leader[(i + 1).min(n)] = true;
        }
        if ins.op == OpCode::PushExcInfo && ins.arg != 0 {
            leader[shift(ins.arg).min(n)] = true;
        }
    }
    for h in &table {
        let t = (h.handler as usize).min(n);
        leader[t] = true;
        labeled[t] = true;
    }
    // END_ASYNC_FOR is a jump whose target is the SEND of the dance it
    // handles: find it through the range whose handler it is.
    let eaf_target = |i: usize| -> Option<usize> {
        table
            .iter()
            .filter(|h| h.handler as usize == i)
            .find_map(|h| {
                (h.start as usize..(h.end as usize).min(n)).find(|&k| instrs[k].op == OpCode::Send)
            })
    };
    for (i, ins) in instrs.iter().enumerate() {
        if ins.op == OpCode::EndAsyncFor {
            if let Some(t) = eaf_target(i) {
                leader[t] = true;
                labeled[t] = true;
            }
        }
    }

    // Blocks in stream order; `block_of[i]` for every instruction index
    // (plus the end index, mapping to the handler block or a trailing
    // empty block).
    let mut cfg = Cfg {
        blocks: Vec::new(),
        entry: 0,
        handlers: Vec::new(),
        consts: std::mem::take(&mut co.constants),
        generator_prefix,
        firstlineno,
    };
    let mut block_of = vec![0usize; n + 1];
    let mut cur = cfg.new_block();
    cfg.entry = cur;
    for i in 0..n {
        if i > 0 && leader[i] {
            let b = cfg.new_block();
            cfg.blocks[cur].next = Some(b);
            cur = b;
        }
        cfg.blocks[cur].label |= labeled[i];
        block_of[i] = cur;
    }
    // The end-of-stream position: `AddReturnAtEnd` guarantees no jump
    // lands there, but keep a trailing empty block so the index maps.
    let tail = cfg.new_block();
    cfg.blocks[cur].next = Some(tail);
    cfg.blocks[tail].label = labeled[n];
    block_of[n] = tail;

    // Handler identities.
    let mut handler_ids: Vec<HandlerId> = Vec::with_capacity(table.len());
    for h in &table {
        let info = HandlerInfo {
            block: block_of[(h.handler as usize).min(n)],
            depth: h.depth,
            lasti: h.push_lasti,
        };
        // One identity per handler block (CPython's `i_except` is the
        // block pointer); the compiler-side depth is only a fallback
        // for a handler no `SETUP_*` stand-in reaches.
        let id = match cfg
            .handlers
            .iter()
            .position(|x| x.block == info.block && x.lasti == info.lasti)
        {
            Some(id) => id,
            None => {
                cfg.handlers.push(info);
                cfg.handlers.len() - 1
            }
        };
        handler_ids.push(id);
    }
    for hid in &cfg.handlers {
        cfg.blocks[hid.block].except_handler = true;
    }

    // Instructions. Conditional jumps flagged as `JUMP_IF_*` pseudo-ops
    // absorb the `COPY 1; TO_BOOL` in front of them (CPython inserts
    // those in `convert_pseudo_conditional_jumps`).
    let mut skip = vec![false; n];
    for i in 0..n {
        if input.pseudo_cond_jumps.contains(&((i + base) as u32))
            && i >= 2
            && instrs[i - 2].op == OpCode::CopyTop
            && instrs[i - 2].arg <= 1
            && instrs[i - 1].op == OpCode::ToBool
            && block_of[i - 2] == block_of[i]
        {
            skip[i - 2] = true;
            skip[i - 1] = true;
        }
    }
    let wrap_hid = wrap_entry.map(|e| handler_ids[e]);
    if let Some(e) = wrap_entry {
        // The wrapping `SETUP_CLEANUP` heads the entry block, before
        // RESUME; it never carries a handler.
        let entry = cfg.entry;
        let hb = cfg.handlers[handler_ids[e]].block;
        cfg.blocks[entry].add_jump(OpCode::SetupCleanup, hb, NO_LOCATION);
        cfg.blocks[entry].slots[0].except = None;
    }
    let mut last_yield_depth1 = false;
    let is_module = co.name == "<module>";
    let mut seen_resume = false;
    for i in 0..n {
        if skip[i] {
            continue;
        }
        let ins = instrs[i];
        let orig = (i + base) as u32;
        let mut loc = Loc::from_table(co.linetable[i + base], co.coltable[i + base]);
        if ins.op == OpCode::Resume && is_module && co.linetable[i + base] == 0 && !seen_resume {
            // `codegen_enter_scope` gives a module's opening RESUME the
            // *real* location (0, 1, 0, 0) — `loc.lineno = 0` — which
            // `propagate_line_numbers` then hands to a trailing
            // `LOAD_CONST None; RETURN_VALUE` (an empty module's
            // `co_lines()` is one line-0 range). The codegen's table
            // spells it as its NO_LOCATION sentinel; restore it here.
            loc = MODULE_RESUME_LOCATION;
        }
        seen_resume |= ins.op == OpCode::Resume;
        let b = block_of[i];
        let mut except = owner[i].map(|o| handler_ids[o]);
        let (op, arg, target) = match ins.op {
            OpCode::JumpForward | OpCode::JumpBackward => {
                let t = flat_target(ins, i, false).unwrap().min(n);
                let plain = input.plain_jumps.contains(&orig)
                    || (!input.no_interrupt_jumps.contains(&orig)
                        && !implied_no_interrupt(&instrs, i));
                let op = if plain {
                    OpCode::Jump
                } else {
                    OpCode::JumpNoInterrupt
                };
                (op, 0, Some(block_of[t]))
            }
            OpCode::PopJumpIfFalse | OpCode::PopJumpIfTrue
                if input.pseudo_cond_jumps.contains(&orig) && i >= 2 && skip[i - 1] =>
            {
                let t = flat_target(ins, i, is_backward(i)).unwrap().min(n);
                let op = if ins.op == OpCode::PopJumpIfFalse {
                    OpCode::JumpIfFalse
                } else {
                    OpCode::JumpIfTrue
                };
                (op, 0, Some(block_of[t]))
            }
            OpCode::PopJumpIfFalse
            | OpCode::PopJumpIfTrue
            | OpCode::PopJumpIfNone
            | OpCode::PopJumpIfNotNone
            | OpCode::ForIter
            | OpCode::Send => {
                let t = flat_target(ins, i, is_backward(i)).unwrap().min(n);
                (ins.op, 0, Some(block_of[t]))
            }
            OpCode::EndAsyncFor => {
                let t = eaf_target(i).map(|t| block_of[t]);
                (ins.op, 0, t)
            }
            OpCode::PushExcInfo => {
                let t = if ins.arg != 0 {
                    Some(block_of[shift(ins.arg).min(n)])
                } else {
                    None
                };
                (ins.op, 0, t)
            }
            OpCode::Nop if input.setup_nops.contains_key(&orig) => {
                // A located SETUP_* pseudo-op: no handler of its own,
                // a real (non-NOP) instruction until `convert_pseudo_ops`.
                // Its kind fixes the handler block's entry depth
                // (`SETUP_CLEANUP` pushes lasti as well as the
                // exception), which is where the exception table's
                // depth column comes from (`assemble_exception_table`).
                except = None;
                let t = setup_target(i).map(|e| cfg.handlers[handler_ids[e]].block);
                (input.setup_nops[&orig], 0, t)
            }
            OpCode::Nop if input.popblock_nops.contains(&orig) => {
                // POP_BLOCK: `label_exception_targets` turns it into a
                // NOP without assigning `i_except`.
                except = None;
                (OpCode::Nop, 0, None)
            }
            OpCode::SetupFinally | OpCode::SetupCleanup | OpCode::SetupWith => {
                except = None;
                let t = setup_target(i).map(|e| cfg.handlers[handler_ids[e]].block);
                (ins.op, 0, t)
            }
            OpCode::PopBlock => {
                except = None;
                (OpCode::Nop, 0, None)
            }
            OpCode::Resume => {
                let mut arg = ins.arg;
                if arg != 0 && last_yield_depth1 {
                    arg |= RESUME_DEPTH1_MASK;
                }
                (ins.op, arg, None)
            }
            _ => (ins.op, ins.arg, None),
        };
        if ins.op == OpCode::YieldValue {
            // `last_yield_except_depth == 1`: only the wrap is active.
            last_yield_depth1 = wrap && except == wrap_hid;
        }
        let blk = &mut cfg.blocks[b];
        let off = blk.next_instr();
        blk.slots[off] = CfgInstr {
            op,
            arg,
            loc,
            target,
            except,
        };
    }
    cfg
}

/// The send-dance edges CPython emits as `JUMP_NO_INTERRUPT` that the
/// code generator lowers with an unflagged `JumpBackward`: a backward
/// jump onto a `SEND`, or one straight after `CLEANUP_THROW`.
fn implied_no_interrupt(instrs: &[Instruction], i: usize) -> bool {
    let ins = instrs[i];
    if ins.op != OpCode::JumpBackward {
        return false;
    }
    let t = (i + 1).saturating_sub(ins.arg as usize);
    instrs.get(t).map(|x| x.op) == Some(OpCode::Send)
        || (i > 0 && instrs[i - 1].op == OpCode::CleanupThrow)
}

// ---------- passes: optimize_cfg ----------

impl Cfg {
    /// `remove_unreachable`.
    fn remove_unreachable(&mut self) {
        let chain = self.chain();
        for &b in &chain {
            self.blocks[b].predecessors = 0;
        }
        self.clear_visited();
        let mut stack = vec![self.entry];
        self.blocks[self.entry].predecessors = 1;
        self.blocks[self.entry].visited = true;
        // Handlers whose block push has been seen (materialised
        // `SETUP_*` targets count through `target`; the rest once).
        let mut handler_pushed = vec![false; self.handlers.len()];
        for &b in &chain {
            for k in 0..self.blocks[b].used {
                let instr = self.blocks[b].slots[k];
                if is_block_push(instr.op) {
                    if let Some(t) = instr.target {
                        for (h, info) in self.handlers.iter().enumerate() {
                            if info.block == t {
                                handler_pushed[h] = true;
                            }
                        }
                    }
                }
            }
        }
        while let Some(b) = stack.pop() {
            if let Some(next) = self.blocks[b].next {
                if self.blocks[b].has_fallthrough() {
                    if !self.blocks[next].visited {
                        stack.push(next);
                        self.blocks[next].visited = true;
                    }
                    self.blocks[next].predecessors += 1;
                }
            }
            for k in 0..self.blocks[b].used {
                let instr = self.blocks[b].slots[k];
                if is_jump(instr.op) || is_block_push(instr.op) {
                    if let Some(t) = instr.target {
                        if !self.blocks[t].visited {
                            stack.push(t);
                            self.blocks[t].visited = true;
                        }
                        self.blocks[t].predecessors += 1;
                    }
                }
                // A handler whose `SETUP_*` the code generator did not
                // materialise is still reached through the exception
                // edge; it counts as the one block push CPython would
                // have had.
                if let Some(h) = instr.except {
                    if !handler_pushed[h] {
                        handler_pushed[h] = true;
                        let t = self.handlers[h].block;
                        if !self.blocks[t].visited {
                            stack.push(t);
                            self.blocks[t].visited = true;
                        }
                        self.blocks[t].predecessors += 1;
                    }
                }
            }
        }
        for &b in &chain {
            if self.blocks[b].predecessors == 0 {
                self.blocks[b].used = 0;
                self.blocks[b].except_handler = false;
            }
        }
    }

    /// `basicblock_remove_redundant_nops`; returns the number removed.
    fn block_remove_redundant_nops(&mut self, bb: BlockId) -> usize {
        let used = self.blocks[bb].used;
        // The next block's leading location, for the block-boundary rule.
        let next_line: Option<i32> = self.next_nonempty(self.blocks[bb].next).map(|next| {
            let mut next_loc = NO_LOCATION;
            for instr in self.blocks[next].instrs() {
                if instr.op == OpCode::Nop && instr.loc.line < 0 {
                    continue;
                }
                next_loc = instr.loc;
                break;
            }
            next_loc.line
        });
        let b = &mut self.blocks[bb];
        let mut dest = 0usize;
        let mut prev_lineno: i32 = -1;
        for src in 0..used {
            let lineno = b.slots[src].loc.line;
            if b.slots[src].op == OpCode::Nop {
                if lineno < 0 {
                    continue;
                }
                if prev_lineno == lineno {
                    continue;
                }
                if src < used - 1 {
                    let next_lineno = b.slots[src + 1].loc.line;
                    if next_lineno == lineno {
                        continue;
                    }
                    if next_lineno < 0 {
                        b.slots[src + 1].loc = b.slots[src].loc;
                        continue;
                    }
                } else if let Some(nl) = next_line {
                    if lineno == nl {
                        continue;
                    }
                }
            }
            if dest != src {
                b.slots[dest] = b.slots[src];
            }
            dest += 1;
            prev_lineno = lineno;
        }
        let removed = used - dest;
        b.used = dest;
        removed
    }

    /// `remove_redundant_nops`.
    fn remove_redundant_nops(&mut self) -> usize {
        let mut changes = 0;
        for b in self.chain() {
            changes += self.block_remove_redundant_nops(b);
        }
        changes
    }

    /// `remove_redundant_nops_and_pairs`.
    fn remove_redundant_nops_and_pairs(&mut self) {
        let mut done = false;
        while !done {
            done = true;
            // `instr` is a (block, index) reference into the graph.
            let mut instr: Option<(BlockId, usize)> = None;
            for b in self.chain() {
                self.block_remove_redundant_nops(b);
                if self.blocks[b].label {
                    instr = None;
                }
                for i in 0..self.blocks[b].used {
                    let prev = instr;
                    instr = Some((b, i));
                    let (prev_opcode, prev_oparg) = match prev {
                        Some((pb, pi)) => {
                            let p = self.blocks[pb].slots[pi];
                            (Some(p.op), p.arg)
                        }
                        None => (None, 0),
                    };
                    let opcode = self.blocks[b].slots[i].op;
                    let mut is_redundant_pair = false;
                    if opcode == OpCode::PopTop {
                        if matches!(prev_opcode, Some(OpCode::LoadConst | OpCode::LoadSmallInt)) {
                            is_redundant_pair = true;
                        } else if prev_opcode == Some(OpCode::CopyTop) && prev_oparg <= 1 {
                            is_redundant_pair = true;
                        }
                    }
                    if is_redundant_pair {
                        let (pb, pi) = prev.unwrap();
                        set_nop(&mut self.blocks[pb].slots[pi]);
                        set_nop(&mut self.blocks[b].slots[i]);
                        done = false;
                    }
                }
                let ends_in_jump = self.blocks[b].last().is_some_and(|l| is_jump(l.op));
                if ends_in_jump || self.blocks[b].no_fallthrough() {
                    instr = None;
                }
            }
        }
    }

    /// `remove_redundant_jumps`.
    fn remove_redundant_jumps(&mut self) -> usize {
        let mut changes = 0;
        for b in self.chain() {
            let Some(last) = self.blocks[b].last().copied() else {
                continue;
            };
            if is_unconditional_jump(last.op) {
                let jump_target = self.next_nonempty(last.target);
                let next = self.next_nonempty(self.blocks[b].next);
                if jump_target.is_some() && jump_target == next {
                    changes += 1;
                    let n = self.blocks[b].used - 1;
                    set_nop(&mut self.blocks[b].slots[n]);
                }
            }
        }
        changes
    }

    /// `remove_redundant_nops_and_jumps`.
    fn remove_redundant_nops_and_jumps(&mut self) {
        loop {
            let removed_nops = self.remove_redundant_nops();
            let removed_jumps = self.remove_redundant_jumps();
            if removed_nops + removed_jumps == 0 {
                break;
            }
        }
    }

    /// `basicblock_inline_small_or_no_lineno_blocks`.
    fn block_inline_small_or_no_lineno(&mut self, bb: BlockId) -> bool {
        let Some(last) = self.blocks[bb].last().copied() else {
            return false;
        };
        if !is_unconditional_jump(last.op) {
            return false;
        }
        let Some(target) = last.target else {
            return false;
        };
        let small_exit_block =
            self.blocks[target].exits_scope() && self.blocks[target].used <= MAX_COPY_SIZE;
        let no_lineno_no_fallthrough =
            self.blocks[target].has_no_lineno() && !self.blocks[target].has_fallthrough();
        if small_exit_block || no_lineno_no_fallthrough {
            let removed_jump_opcode = last.op;
            let n = self.blocks[bb].used - 1;
            set_nop(&mut self.blocks[bb].slots[n]);
            self.append_instructions(bb, target);
            if no_lineno_no_fallthrough {
                let n = self.blocks[bb].used - 1;
                let l = &mut self.blocks[bb].slots[n];
                if is_unconditional_jump(l.op) && removed_jump_opcode == OpCode::Jump {
                    l.op = OpCode::Jump;
                }
            }
            self.blocks[target].predecessors -= 1;
            return true;
        }
        false
    }

    /// `inline_small_or_no_lineno_blocks`.
    fn inline_small_or_no_lineno_blocks(&mut self) {
        loop {
            let mut changes = false;
            for b in self.chain() {
                if self.block_inline_small_or_no_lineno(b) {
                    changes = true;
                }
            }
            if !changes {
                break;
            }
        }
    }

    /// `jump_thread`: retarget `inst` (the last instruction of `bb`)
    /// through `target` (the first instruction of its target block).
    /// Returns whether anything changed.
    fn jump_thread(&mut self, bb: BlockId, inst_at: usize, target: CfgInstr, op: OpCode) -> bool {
        let inst = self.blocks[bb].slots[inst_at];
        debug_assert!(is_jump(inst.op) && is_jump(target.op));
        if inst.target != target.target {
            set_nop(&mut self.blocks[bb].slots[inst_at]);
            self.blocks[bb].add_jump(op, target.target.expect("jump target"), target.loc);
            return true;
        }
        false
    }

    /// `optimize_basic_block`.
    fn optimize_basic_block(&mut self, bb: BlockId) {
        let mut i = 0usize;
        while i < self.blocks[bb].used {
            let inst = self.blocks[bb].slots[i];
            let opcode = inst.op;
            let oparg = inst.arg;
            // `target`: the first instruction of the jump target (a NOP
            // stand-in otherwise).
            let target: CfgInstr = match inst.target {
                Some(t) if is_jump(opcode) || is_block_push(opcode) => {
                    debug_assert!(self.blocks[t].used > 0);
                    self.blocks[t].slots[0]
                }
                _ => ZERO_INSTR,
            };
            let used = self.blocks[bb].used;
            let nextop = if i + 1 < used {
                Some(self.blocks[bb].slots[i + 1].op)
            } else {
                None
            };
            let next_arg = if i + 1 < used {
                self.blocks[bb].slots[i + 1].arg
            } else {
                0
            };
            let mut skip_increment = false;
            match opcode {
                OpCode::BuildTuple => {
                    if nextop == Some(OpCode::UnpackSequence) && oparg == next_arg {
                        match oparg {
                            1 => {
                                set_nop(&mut self.blocks[bb].slots[i]);
                                set_nop(&mut self.blocks[bb].slots[i + 1]);
                                i += 1;
                                continue;
                            }
                            2 | 3 => {
                                set_nop(&mut self.blocks[bb].slots[i]);
                                self.blocks[bb].slots[i + 1].op = OpCode::Swap;
                                i += 1;
                                continue;
                            }
                            _ => {}
                        }
                    }
                    self.fold_tuple_of_constants(bb, i);
                }
                OpCode::BuildList | OpCode::BuildSet => {
                    self.optimize_lists_and_sets(bb, i, nextop);
                }
                OpCode::PopJumpIfNotNone | OpCode::PopJumpIfNone => {
                    if target.op == OpCode::Jump && self.jump_thread(bb, i, target, opcode) {
                        skip_increment = true;
                    }
                }
                OpCode::PopJumpIfFalse => {
                    if target.op == OpCode::Jump
                        && self.jump_thread(bb, i, target, OpCode::PopJumpIfFalse)
                    {
                        skip_increment = true;
                    }
                }
                OpCode::PopJumpIfTrue => {
                    if target.op == OpCode::Jump
                        && self.jump_thread(bb, i, target, OpCode::PopJumpIfTrue)
                    {
                        skip_increment = true;
                    }
                }
                OpCode::JumpIfFalse => match target.op {
                    OpCode::Jump | OpCode::JumpIfFalse => {
                        if self.jump_thread(bb, i, target, OpCode::JumpIfFalse) {
                            skip_increment = true;
                        }
                        if !skip_increment {
                            i += 1;
                        }
                        continue;
                    }
                    OpCode::JumpIfTrue => {
                        let t = inst.target.unwrap();
                        self.blocks[bb].slots[i].target = self.blocks[t].next;
                        continue;
                    }
                    _ => {}
                },
                OpCode::JumpIfTrue => match target.op {
                    OpCode::Jump | OpCode::JumpIfTrue => {
                        if self.jump_thread(bb, i, target, OpCode::JumpIfTrue) {
                            skip_increment = true;
                        }
                        if !skip_increment {
                            i += 1;
                        }
                        continue;
                    }
                    OpCode::JumpIfFalse => {
                        let t = inst.target.unwrap();
                        self.blocks[bb].slots[i].target = self.blocks[t].next;
                        continue;
                    }
                    _ => {}
                },
                OpCode::Jump | OpCode::JumpNoInterrupt => match target.op {
                    OpCode::Jump => {
                        if self.jump_thread(bb, i, target, OpCode::Jump) {
                            skip_increment = true;
                        }
                        if !skip_increment {
                            i += 1;
                        }
                        continue;
                    }
                    OpCode::JumpNoInterrupt => {
                        if self.jump_thread(bb, i, target, opcode) {
                            skip_increment = true;
                        }
                        if !skip_increment {
                            i += 1;
                        }
                        continue;
                    }
                    _ => {}
                },
                OpCode::StoreFast => {
                    if nextop == Some(OpCode::StoreFast)
                        && oparg == next_arg
                        && self.blocks[bb].slots[i].loc.line
                            == self.blocks[bb].slots[i + 1].loc.line
                    {
                        self.blocks[bb].slots[i].op = OpCode::PopTop;
                        self.blocks[bb].slots[i].arg = 0;
                    }
                }
                OpCode::Swap => {
                    if oparg == 1 {
                        set_nop(&mut self.blocks[bb].slots[i]);
                    }
                }
                OpCode::LoadGlobal => {
                    if nextop == Some(OpCode::PushNull) {
                        self.blocks[bb].slots[i].op = OpCode::LoadGlobalPushNull;
                        set_nop(&mut self.blocks[bb].slots[i + 1]);
                    }
                }
                OpCode::CompareOp => {
                    if nextop == Some(OpCode::ToBool) {
                        set_nop(&mut self.blocks[bb].slots[i]);
                        let n = &mut self.blocks[bb].slots[i + 1];
                        n.op = OpCode::CompareOp;
                        n.arg = oparg | COMPARE_OP_TO_BOOL_FLAG;
                        i += 1;
                        continue;
                    }
                }
                OpCode::ContainsOp | OpCode::IsOp => {
                    if nextop == Some(OpCode::ToBool) {
                        set_nop(&mut self.blocks[bb].slots[i]);
                        let n = &mut self.blocks[bb].slots[i + 1];
                        n.op = opcode;
                        n.arg = oparg;
                        i += 1;
                        continue;
                    }
                    if i + 1 < used && is_unary_not(&self.blocks[bb].slots[i + 1]) {
                        set_nop(&mut self.blocks[bb].slots[i]);
                        let n = &mut self.blocks[bb].slots[i + 1];
                        n.op = opcode;
                        n.arg = oparg ^ 1;
                        i += 1;
                        continue;
                    }
                }
                OpCode::ToBool => {
                    if nextop == Some(OpCode::ToBool) {
                        set_nop(&mut self.blocks[bb].slots[i]);
                        i += 1;
                        continue;
                    }
                }
                OpCode::UnaryOp => {
                    let kind = UnaryKind::from_arg(oparg);
                    if kind == Some(UnaryKind::Not) {
                        if nextop == Some(OpCode::ToBool) {
                            set_nop(&mut self.blocks[bb].slots[i]);
                            let n = &mut self.blocks[bb].slots[i + 1];
                            n.op = OpCode::UnaryOp;
                            n.arg = UnaryKind::Not.as_arg();
                            i += 1;
                            continue;
                        }
                        if i + 1 < used && is_unary_not(&self.blocks[bb].slots[i + 1]) {
                            set_nop(&mut self.blocks[bb].slots[i]);
                            set_nop(&mut self.blocks[bb].slots[i + 1]);
                            i += 1;
                            continue;
                        }
                    }
                    // UNARY_NOT falls through to the constant fold, as
                    // do UNARY_INVERT / UNARY_NEGATIVE / the unary-plus
                    // intrinsic.
                    if kind.is_some() {
                        self.fold_const_unaryop(bb, i);
                    }
                }
                OpCode::ListToTuple => {
                    if nextop == Some(OpCode::GetIter) {
                        set_nop(&mut self.blocks[bb].slots[i]);
                    } else {
                        self.fold_constant_intrinsic_list_to_tuple(bb, i);
                    }
                }
                OpCode::BinaryOp | OpCode::BinarySubscr => {
                    self.fold_const_binop(bb, i);
                }
                _ => {}
            }
            if !skip_increment {
                i += 1;
            }
        }

        let mut i = 0usize;
        while i < self.blocks[bb].used {
            if self.blocks[bb].slots[i].op == OpCode::Swap {
                self.swaptimize(bb, &mut i);
                self.apply_static_swaps(bb, i);
            }
            i += 1;
        }
    }

    /// `get_const_loading_instrs`: the `size` constant loads that
    /// precede `start` (inclusive), skipping NOPs.
    fn const_loading_instrs(&self, bb: BlockId, start: usize, size: usize) -> Option<Vec<usize>> {
        let b = &self.blocks[bb];
        let mut out = vec![0usize; size];
        let mut size = size;
        let mut pos = start as i64;
        while pos >= 0 && size > 0 {
            let instr = &b.slots[pos as usize];
            if instr.op == OpCode::Nop {
                pos -= 1;
                continue;
            }
            if !loads_const(instr.op) {
                return None;
            }
            size -= 1;
            out[size] = pos as usize;
            pos -= 1;
        }
        if size == 0 {
            Some(out)
        } else {
            None
        }
    }

    /// `fold_tuple_of_constants`.
    fn fold_tuple_of_constants(&mut self, bb: BlockId, i: usize) {
        let seq_size = self.blocks[bb].slots[i].arg as usize;
        if seq_size > STACK_USE_GUIDELINE {
            return;
        }
        let Some(instrs) = self.const_loading_instrs(bb, i.wrapping_sub(1), seq_size) else {
            return;
        };
        if i == 0 && seq_size > 0 {
            return;
        }
        let mut items = Vec::with_capacity(seq_size);
        for &k in &instrs {
            let Some(c) = self.const_value(&self.blocks[bb].slots[k]) else {
                return;
            };
            items.push(c);
        }
        for &k in &instrs {
            nop_out(&mut self.blocks[bb].slots[k]);
        }
        self.make_load_const(bb, i, Constant::Tuple(items));
    }

    /// `fold_constant_intrinsic_list_to_tuple`.
    fn fold_constant_intrinsic_list_to_tuple(&mut self, bb: BlockId, i: usize) {
        let mut consts_found = 0usize;
        let mut expect_append = true;
        let mut pos = i as i64 - 1;
        while pos >= 0 {
            let p = pos as usize;
            let instr = self.blocks[bb].slots[p];
            let opcode = instr.op;
            let oparg = instr.arg;
            if opcode == OpCode::Nop {
                pos -= 1;
                continue;
            }
            if opcode == OpCode::BuildList && oparg == 0 {
                if !expect_append {
                    return;
                }
                let mut items: Vec<Constant> = Vec::with_capacity(consts_found);
                let mut newpos = i as i64 - 1;
                while newpos >= p as i64 {
                    let k = newpos as usize;
                    let ins = self.blocks[bb].slots[k];
                    if ins.op == OpCode::Nop {
                        newpos -= 1;
                        continue;
                    }
                    if loads_const(ins.op) {
                        let Some(c) = self.const_value(&ins) else {
                            return;
                        };
                        items.push(c);
                    }
                    nop_out(&mut self.blocks[bb].slots[k]);
                    newpos -= 1;
                }
                items.reverse();
                self.make_load_const(bb, i, Constant::Tuple(items));
                return;
            }
            if expect_append {
                if opcode != OpCode::ListAppend || oparg != 1 {
                    return;
                }
            } else {
                if !loads_const(opcode) {
                    return;
                }
                consts_found += 1;
            }
            expect_append = !expect_append;
            pos -= 1;
        }
    }

    /// `optimize_lists_and_sets`.
    fn optimize_lists_and_sets(&mut self, bb: BlockId, i: usize, nextop: Option<OpCode>) {
        let instr = self.blocks[bb].slots[i];
        let contains_or_iter = matches!(nextop, Some(OpCode::GetIter | OpCode::ContainsOp));
        let seq_size = instr.arg;
        if seq_size as usize > STACK_USE_GUIDELINE
            || (seq_size < MIN_CONST_SEQUENCE_SIZE && !contains_or_iter)
        {
            return;
        }
        let loads = if i == 0 && seq_size > 0 {
            None
        } else {
            self.const_loading_instrs(bb, i.wrapping_sub(1), seq_size as usize)
        };
        let Some(loads) = loads else {
            if contains_or_iter && instr.op == OpCode::BuildList {
                self.blocks[bb].slots[i].op = OpCode::BuildTuple;
            }
            return;
        };
        let mut items = Vec::with_capacity(seq_size as usize);
        for &k in &loads {
            let Some(c) = self.const_value(&self.blocks[bb].slots[k]) else {
                return;
            };
            items.push(c);
        }
        let result = if instr.op == OpCode::BuildSet {
            Constant::FrozenSet(dedup_constants(items))
        } else {
            Constant::Tuple(items)
        };
        let index = self.add_const(result);
        for &k in &loads {
            nop_out(&mut self.blocks[bb].slots[k]);
        }
        if contains_or_iter {
            let s = &mut self.blocks[bb].slots[i];
            s.op = OpCode::LoadConst;
            s.arg = index;
        } else {
            debug_assert!(i >= 2);
            let loc = instr.loc;
            let b = &mut self.blocks[bb];
            b.slots[i - 2].loc = loc;
            b.slots[i - 2].op = instr.op;
            b.slots[i - 2].arg = 0;
            b.slots[i - 1].op = OpCode::LoadConst;
            b.slots[i - 1].arg = index;
            b.slots[i].op = if instr.op == OpCode::BuildList {
                OpCode::ListExtend
            } else {
                OpCode::SetUpdate
            };
            b.slots[i].arg = 1;
        }
    }

    /// `fold_const_binop` (also covers `BinarySubscr`, 3.14's
    /// `BINARY_OP NB_SUBSCR`).
    fn fold_const_binop(&mut self, bb: BlockId, i: usize) {
        if i < 1 {
            return;
        }
        let Some(ops) = self.const_loading_instrs(bb, i - 1, 2) else {
            return;
        };
        let binop = self.blocks[bb].slots[i];
        let (Some(lhs), Some(rhs)) = (
            self.const_value(&self.blocks[bb].slots[ops[0]]),
            self.const_value(&self.blocks[bb].slots[ops[1]]),
        ) else {
            return;
        };
        let result = if binop.op == OpCode::BinarySubscr {
            eval_const_subscr(&lhs, &rhs)
        } else {
            if binop.arg & BINARY_OP_INPLACE_FLAG != 0 {
                return;
            }
            let Some(kind) = BinOpKind::from_arg(binop.arg & 0xFF) else {
                return;
            };
            eval_const_binop(&lhs, kind, &rhs)
        };
        let Some(newconst) = result else {
            return;
        };
        for &k in &ops {
            nop_out(&mut self.blocks[bb].slots[k]);
        }
        self.make_load_const(bb, i, newconst);
    }

    /// `fold_const_unaryop`.
    fn fold_const_unaryop(&mut self, bb: BlockId, i: usize) {
        if i < 1 {
            return;
        }
        let Some(ops) = self.const_loading_instrs(bb, i - 1, 1) else {
            return;
        };
        let unaryop = self.blocks[bb].slots[i];
        let Some(kind) = UnaryKind::from_arg(unaryop.arg) else {
            return;
        };
        let Some(operand) = self.const_value(&self.blocks[bb].slots[ops[0]]) else {
            return;
        };
        let Some(newconst) = eval_const_unaryop(&operand, kind) else {
            return;
        };
        nop_out(&mut self.blocks[bb].slots[ops[0]]);
        self.make_load_const(bb, i, newconst);
    }

    /// `swaptimize`.
    fn swaptimize(&mut self, bb: BlockId, ix: &mut usize) {
        let b = &mut self.blocks[bb];
        let base = *ix;
        debug_assert!(b.slots[base].op == OpCode::Swap);
        let mut depth = b.slots[base].arg as usize;
        let mut len = 0usize;
        let mut more = false;
        let limit = b.used - base;
        loop {
            len += 1;
            if len >= limit {
                break;
            }
            let op = b.slots[base + len].op;
            if op == OpCode::Swap {
                depth = depth.max(b.slots[base + len].arg as usize);
                more = true;
            } else if op != OpCode::Nop {
                break;
            }
        }
        if !more {
            return;
        }
        const VISITED: i64 = -1;
        let mut stack: Vec<i64> = (0..depth as i64).collect();
        for k in 0..len {
            if b.slots[base + k].op == OpCode::Swap {
                let oparg = b.slots[base + k].arg as usize;
                let top = stack[0];
                stack[0] = stack[oparg - 1];
                stack[oparg - 1] = top;
            }
        }
        let mut current = len as i64 - 1;
        for i in 0..depth {
            if stack[i] == VISITED || stack[i] == i as i64 {
                continue;
            }
            let mut j = i;
            loop {
                if j != 0 {
                    debug_assert!(current >= 0);
                    let s = &mut b.slots[base + current as usize];
                    s.op = OpCode::Swap;
                    s.arg = (j + 1) as u32;
                    current -= 1;
                }
                if stack[j] == VISITED {
                    debug_assert!(j == i);
                    break;
                }
                let next_j = stack[j] as usize;
                stack[j] = VISITED;
                j = next_j;
            }
        }
        while current >= 0 {
            set_nop(&mut b.slots[base + current as usize]);
            current -= 1;
        }
        *ix += len - 1;
    }

    /// `next_swappable_instruction`.
    fn next_swappable_instruction(&self, bb: BlockId, mut i: usize, lineno: i32) -> Option<usize> {
        let b = &self.blocks[bb];
        loop {
            i += 1;
            if i >= b.used {
                return None;
            }
            let instruction = &b.slots[i];
            if lineno >= 0 && instruction.loc.line != lineno {
                return None;
            }
            if instruction.op == OpCode::Nop {
                continue;
            }
            if swappable(instruction.op) {
                return Some(i);
            }
            return None;
        }
    }

    /// `apply_static_swaps`.
    fn apply_static_swaps(&mut self, bb: BlockId, i: usize) {
        let mut i = i as i64;
        while i >= 0 {
            let idx = i as usize;
            let swap = self.blocks[bb].slots[idx];
            if swap.op != OpCode::Swap {
                if swap.op == OpCode::Nop || swappable(swap.op) {
                    i -= 1;
                    continue;
                }
                return;
            }
            let Some(j) = self.next_swappable_instruction(bb, idx, -1) else {
                return;
            };
            let mut k = j;
            let lineno = self.blocks[bb].slots[j].loc.line;
            let mut count = swap.arg as i64 - 1;
            while count > 0 {
                match self.next_swappable_instruction(bb, k, lineno) {
                    Some(nk) => k = nk,
                    None => return,
                }
                count -= 1;
            }
            let store_j = stores_to(&self.blocks[bb].slots[j]);
            let store_k = stores_to(&self.blocks[bb].slots[k]);
            if store_j >= 0 || store_k >= 0 {
                if store_j == store_k {
                    return;
                }
                for idx2 in j + 1..k {
                    let store_idx = stores_to(&self.blocks[bb].slots[idx2]);
                    if store_idx >= 0 && (store_idx == store_j || store_idx == store_k) {
                        return;
                    }
                }
            }
            let b = &mut self.blocks[bb];
            set_nop(&mut b.slots[idx]);
            b.slots.swap(j, k);
            i -= 1;
        }
    }

    /// `basicblock_optimize_load_const`.
    fn block_optimize_load_const(&mut self, bb: BlockId) {
        let mut opcode: Option<OpCode> = None;
        let mut oparg: u32 = 0;
        let mut i = 0usize;
        while i < self.blocks[bb].used {
            if self.blocks[bb].slots[i].op == OpCode::LoadConst {
                if let Some(c) = self.const_value(&self.blocks[bb].slots[i]) {
                    Self::maybe_make_load_smallint(&mut self.blocks[bb].slots[i], &c);
                }
            }
            let inst = self.blocks[bb].slots[i];
            let is_copy_of_load_const =
                opcode == Some(OpCode::LoadConst) && inst.op == OpCode::CopyTop && inst.arg <= 1;
            if !is_copy_of_load_const {
                opcode = Some(inst.op);
                oparg = inst.arg;
            }
            let Some(op) = opcode else {
                i += 1;
                continue;
            };
            if !loads_const(op) {
                i += 1;
                continue;
            }
            let load = CfgInstr {
                op,
                arg: oparg,
                ..ZERO_INSTR
            };
            let used = self.blocks[bb].used;
            let nextop = if i + 1 < used {
                Some(self.blocks[bb].slots[i + 1].op)
            } else {
                None
            };
            match nextop {
                Some(
                    OpCode::PopJumpIfFalse
                    | OpCode::PopJumpIfTrue
                    | OpCode::JumpIfFalse
                    | OpCode::JumpIfTrue,
                ) => {
                    let nextop = nextop.unwrap();
                    let Some(cnt) = self.const_value(&load) else {
                        i += 1;
                        continue;
                    };
                    let is_true = const_truthy(&cnt);
                    if matches!(nextop, OpCode::PopJumpIfFalse | OpCode::PopJumpIfTrue) {
                        set_nop(&mut self.blocks[bb].slots[i]);
                    }
                    let jump_if_true = matches!(nextop, OpCode::PopJumpIfTrue | OpCode::JumpIfTrue);
                    if is_true == jump_if_true {
                        self.blocks[bb].slots[i + 1].op = OpCode::Jump;
                    } else {
                        set_nop(&mut self.blocks[bb].slots[i + 1]);
                    }
                }
                Some(OpCode::IsOp) => {
                    let Some(cnt) = self.const_value(&load) else {
                        i += 1;
                        continue;
                    };
                    if cnt != Constant::None {
                        i += 1;
                        continue;
                    }
                    if used <= i + 2 {
                        i += 1;
                        continue;
                    }
                    let is_arg = self.blocks[bb].slots[i + 1].arg;
                    let mut jump_at = i + 2;
                    if self.blocks[bb].slots[jump_at].op == OpCode::ToBool {
                        set_nop(&mut self.blocks[bb].slots[jump_at]);
                        if used <= i + 3 {
                            i += 1;
                            continue;
                        }
                        jump_at = i + 3;
                    }
                    let mut invert = is_arg != 0;
                    let jop = self.blocks[bb].slots[jump_at].op;
                    if jop == OpCode::PopJumpIfFalse {
                        invert = !invert;
                    } else if jop != OpCode::PopJumpIfTrue {
                        i += 1;
                        continue;
                    }
                    set_nop(&mut self.blocks[bb].slots[i]);
                    set_nop(&mut self.blocks[bb].slots[i + 1]);
                    self.blocks[bb].slots[jump_at].op = if invert {
                        OpCode::PopJumpIfNotNone
                    } else {
                        OpCode::PopJumpIfNone
                    };
                }
                Some(OpCode::ToBool) => {
                    let Some(cnt) = self.const_value(&load) else {
                        i += 1;
                        continue;
                    };
                    let is_true = const_truthy(&cnt);
                    let index = self.add_const(Constant::Bool(is_true));
                    set_nop(&mut self.blocks[bb].slots[i]);
                    let n = &mut self.blocks[bb].slots[i + 1];
                    n.op = OpCode::LoadConst;
                    n.arg = index;
                }
                _ => {}
            }
            i += 1;
        }
    }

    /// `optimize_load_const`.
    fn optimize_load_const(&mut self) {
        for b in self.chain() {
            self.block_optimize_load_const(b);
        }
    }

    /// `optimize_cfg`.
    fn optimize_cfg(&mut self) {
        self.inline_small_or_no_lineno_blocks();
        self.remove_unreachable();
        self.resolve_line_numbers();
        self.optimize_load_const();
        for b in self.chain() {
            self.optimize_basic_block(b);
        }
        self.remove_redundant_nops_and_pairs();
        self.remove_unreachable();
        self.remove_redundant_nops_and_jumps();
    }

    // ---------- line numbers ----------

    /// `is_exit_or_eval_check_without_lineno`.
    fn is_exit_or_eval_check_without_lineno(&self, b: BlockId) -> bool {
        let blk = &self.blocks[b];
        if blk.exits_scope() || blk.has_eval_break() {
            blk.has_no_lineno()
        } else {
            false
        }
    }

    /// `duplicate_exits_without_lineno`.
    fn duplicate_exits_without_lineno(&mut self) {
        for b in self.chain() {
            let Some(last) = self.blocks[b].last().copied() else {
                continue;
            };
            if is_jump(last.op) {
                let Some(target) = self.next_nonempty(last.target) else {
                    continue;
                };
                if self.is_exit_or_eval_check_without_lineno(target)
                    && self.blocks[target].predecessors > 1
                {
                    let new_target = self.copy_block(target);
                    self.blocks[new_target].slots[0].loc = last.loc;
                    let n = self.blocks[b].used - 1;
                    self.blocks[b].slots[n].target = Some(new_target);
                    self.blocks[target].predecessors -= 1;
                    self.blocks[new_target].predecessors = 1;
                    self.blocks[new_target].next = self.blocks[target].next;
                    self.blocks[target].next = Some(new_target);
                }
            }
        }
        for b in self.chain() {
            if self.blocks[b].has_fallthrough()
                && self.blocks[b].next.is_some()
                && self.blocks[b].used > 0
            {
                let next = self.blocks[b].next.unwrap();
                if self.is_exit_or_eval_check_without_lineno(next) {
                    let last = self.blocks[b].last().copied().unwrap();
                    if self.blocks[next].used > 0 {
                        self.blocks[next].slots[0].loc = last.loc;
                    }
                }
            }
        }
    }

    /// `propagate_line_numbers`.
    fn propagate_line_numbers(&mut self) {
        for b in self.chain() {
            if self.blocks[b].used == 0 {
                continue;
            }
            let mut prev_location = NO_LOCATION;
            for i in 0..self.blocks[b].used {
                if self.blocks[b].slots[i].loc.line == NO_LOCATION.line {
                    self.blocks[b].slots[i].loc = prev_location;
                } else {
                    prev_location = self.blocks[b].slots[i].loc;
                }
            }
            if self.blocks[b].has_fallthrough() {
                if let Some(next) = self.blocks[b].next {
                    if self.blocks[next].predecessors == 1
                        && self.blocks[next].used > 0
                        && self.blocks[next].slots[0].loc.line == NO_LOCATION.line
                    {
                        self.blocks[next].slots[0].loc = prev_location;
                    }
                }
            }
            let last = self.blocks[b].last().copied().unwrap();
            if is_jump(last.op) {
                if let Some(target) = last.target {
                    if self.blocks[target].predecessors == 1
                        && self.blocks[target].used > 0
                        && self.blocks[target].slots[0].loc.line == NO_LOCATION.line
                    {
                        self.blocks[target].slots[0].loc = prev_location;
                    }
                }
            }
        }
    }

    /// `resolve_line_numbers`.
    fn resolve_line_numbers(&mut self) {
        self.duplicate_exits_without_lineno();
        self.propagate_line_numbers();
    }

    // ---------- post-optimization passes ----------

    /// `remove_unused_consts`.
    fn remove_unused_consts(&mut self) {
        let nconsts = self.consts.len();
        if nconsts == 0 {
            return;
        }
        let mut used = vec![false; nconsts];
        // The first constant may be the docstring; keep it always.
        used[0] = true;
        for b in self.chain() {
            for instr in self.blocks[b].instrs() {
                if instr.op == OpCode::LoadConst {
                    if let Some(u) = used.get_mut(instr.arg as usize) {
                        *u = true;
                    }
                }
            }
        }
        if used.iter().all(|&u| u) {
            return;
        }
        let mut reverse = vec![u32::MAX; nconsts];
        let mut new_consts = Vec::with_capacity(nconsts);
        for (i, c) in self.consts.drain(..).enumerate() {
            if used[i] {
                reverse[i] = new_consts.len() as u32;
                new_consts.push(c);
            }
        }
        self.consts = new_consts;
        for b in self.chain() {
            for instr in self.blocks[b].instrs_mut() {
                if instr.op == OpCode::LoadConst {
                    instr.arg = reverse[instr.arg as usize];
                }
            }
        }
    }

    /// `maybe_push` (uninitialized-locals analysis).
    fn maybe_push(&mut self, b: BlockId, unsafe_mask: u64, sp: &mut Vec<BlockId>) {
        let both = self.blocks[b].unsafe_locals_mask | unsafe_mask;
        if self.blocks[b].unsafe_locals_mask != both {
            self.blocks[b].unsafe_locals_mask = both;
            if !self.blocks[b].visited {
                sp.push(b);
                self.blocks[b].visited = true;
            }
        }
    }

    /// `scan_block_for_locals`.
    fn scan_block_for_locals(&mut self, b: BlockId, sp: &mut Vec<BlockId>) {
        let mut unsafe_mask = self.blocks[b].unsafe_locals_mask;
        for i in 0..self.blocks[b].used {
            let instr = self.blocks[b].slots[i];
            if let Some(h) = instr.except {
                let hb = self.handlers[h].block;
                self.maybe_push(hb, unsafe_mask, sp);
            }
            if instr.arg >= 64 {
                continue;
            }
            let bit = 1u64 << instr.arg;
            match instr.op {
                OpCode::DeleteFast | OpCode::LoadFastAndClear | OpCode::StoreFastMaybeNull => {
                    unsafe_mask |= bit;
                }
                OpCode::StoreFast => unsafe_mask &= !bit,
                OpCode::LoadFastCheck => unsafe_mask &= !bit,
                OpCode::LoadFast => {
                    if unsafe_mask & bit != 0 {
                        self.blocks[b].slots[i].op = OpCode::LoadFastCheck;
                    }
                    unsafe_mask &= !bit;
                }
                _ => {}
            }
        }
        if let Some(next) = self.blocks[b].next {
            if self.blocks[b].has_fallthrough() {
                self.maybe_push(next, unsafe_mask, sp);
            }
        }
        let last = self.blocks[b].last().copied();
        if let Some(last) = last {
            if is_jump(last.op) {
                if let Some(t) = last.target {
                    self.maybe_push(t, unsafe_mask, sp);
                }
            }
        }
    }

    /// `fast_scan_many_locals`.
    fn fast_scan_many_locals(&mut self, nlocals: usize) {
        let mut states = vec![0i64; nlocals - 64];
        let mut blocknum: i64 = 0;
        for b in self.chain() {
            blocknum += 1;
            for i in 0..self.blocks[b].used {
                let instr = self.blocks[b].slots[i];
                let arg = instr.arg as usize;
                if arg < 64 {
                    continue;
                }
                let Some(state) = states.get_mut(arg - 64) else {
                    continue;
                };
                match instr.op {
                    OpCode::DeleteFast | OpCode::LoadFastAndClear | OpCode::StoreFastMaybeNull => {
                        *state = blocknum - 1;
                    }
                    OpCode::StoreFast => *state = blocknum,
                    OpCode::LoadFast => {
                        if *state != blocknum {
                            self.blocks[b].slots[i].op = OpCode::LoadFastCheck;
                        }
                        *state = blocknum;
                    }
                    _ => {}
                }
            }
        }
    }

    /// `add_checks_for_loads_of_uninitialized_variables`.
    fn add_checks_for_loads_of_uninitialized_variables(&mut self, nlocals: usize, nparams: usize) {
        if nlocals == 0 {
            return;
        }
        let mut nlocals = nlocals;
        if nlocals > 64 {
            self.fast_scan_many_locals(nlocals);
            nlocals = 64;
        }
        self.clear_visited();
        let mut sp: Vec<BlockId> = Vec::new();
        let mut start_mask = 0u64;
        for i in nparams..nlocals {
            start_mask |= 1u64 << i;
        }
        let entry = self.entry;
        self.maybe_push(entry, start_mask, &mut sp);
        for b in self.chain() {
            self.scan_block_for_locals(b, &mut sp);
        }
        while let Some(b) = sp.pop() {
            self.blocks[b].visited = false;
            self.scan_block_for_locals(b, &mut sp);
        }
    }

    /// `make_super_instruction`.
    fn make_super_instruction(&mut self, bb: BlockId, i: usize, super_op: OpCode) {
        let b = &mut self.blocks[bb];
        let (line1, line2) = (b.slots[i].loc.line, b.slots[i + 1].loc.line);
        if line1 >= 0 && line2 >= 0 && line1 != line2 {
            return;
        }
        if b.slots[i].arg >= 16 || b.slots[i + 1].arg >= 16 {
            return;
        }
        let packed = (b.slots[i].arg << 4) | b.slots[i + 1].arg;
        b.slots[i].op = super_op;
        b.slots[i].arg = packed;
        set_nop(&mut b.slots[i + 1]);
    }

    /// `insert_superinstructions`.
    fn insert_superinstructions(&mut self) {
        for b in self.chain() {
            let mut i = 0usize;
            while i < self.blocks[b].used {
                let op = self.blocks[b].slots[i].op;
                let nextop = if i + 1 < self.blocks[b].used {
                    Some(self.blocks[b].slots[i + 1].op)
                } else {
                    None
                };
                match op {
                    OpCode::LoadFast => {
                        if nextop == Some(OpCode::LoadFast) {
                            self.make_super_instruction(b, i, OpCode::LoadFastLoadFast);
                        }
                    }
                    OpCode::StoreFast => match nextop {
                        Some(OpCode::LoadFast) => {
                            self.make_super_instruction(b, i, OpCode::StoreFastLoadFast);
                        }
                        Some(OpCode::StoreFast) => {
                            self.make_super_instruction(b, i, OpCode::StoreFastStoreFast);
                        }
                        _ => {}
                    },
                    _ => {}
                }
                i += 1;
            }
        }
        self.remove_redundant_nops();
    }

    /// `mark_warm`.
    fn mark_warm(&mut self) {
        self.clear_visited();
        let mut stack = vec![self.entry];
        self.blocks[self.entry].visited = true;
        while let Some(b) = stack.pop() {
            self.blocks[b].warm = true;
            if let Some(next) = self.blocks[b].next {
                if self.blocks[b].has_fallthrough() && !self.blocks[next].visited {
                    stack.push(next);
                    self.blocks[next].visited = true;
                }
            }
            for i in 0..self.blocks[b].used {
                let instr = self.blocks[b].slots[i];
                if is_jump(instr.op) {
                    if let Some(t) = instr.target {
                        if !self.blocks[t].visited {
                            stack.push(t);
                            self.blocks[t].visited = true;
                        }
                    }
                }
            }
        }
    }

    /// `mark_cold`.
    fn mark_cold(&mut self) {
        self.mark_warm();
        self.clear_visited();
        let mut stack: Vec<BlockId> = Vec::new();
        for b in self.chain() {
            if self.blocks[b].except_handler {
                stack.push(b);
                self.blocks[b].visited = true;
            }
        }
        while let Some(b) = stack.pop() {
            self.blocks[b].cold = true;
            if let Some(next) = self.blocks[b].next {
                if self.blocks[b].has_fallthrough()
                    && !self.blocks[next].warm
                    && !self.blocks[next].visited
                {
                    stack.push(next);
                    self.blocks[next].visited = true;
                }
            }
            for i in 0..self.blocks[b].used {
                let instr = self.blocks[b].slots[i];
                if is_jump(instr.op) {
                    if let Some(t) = instr.target {
                        if !self.blocks[t].warm && !self.blocks[t].visited {
                            stack.push(t);
                            self.blocks[t].visited = true;
                        }
                    }
                }
            }
        }
    }

    /// `push_cold_blocks_to_end`.
    fn push_cold_blocks_to_end(&mut self) {
        if self.blocks[self.entry].next.is_none() {
            return;
        }
        self.mark_cold();
        // Cold blocks falling through into warm ones get an explicit jump.
        for b in self.chain() {
            let Some(next) = self.blocks[b].next else {
                continue;
            };
            if self.blocks[b].cold && self.blocks[b].has_fallthrough() && self.blocks[next].warm {
                let explicit_jump = self.new_block();
                self.blocks[explicit_jump].add_jump(OpCode::JumpNoInterrupt, next, NO_LOCATION);
                self.blocks[explicit_jump].cold = true;
                self.blocks[explicit_jump].next = Some(next);
                self.blocks[explicit_jump].predecessors = 1;
                self.blocks[b].next = Some(explicit_jump);
            }
        }
        // Move every cold streak to the end, in order.
        let mut cold_blocks: Option<BlockId> = None;
        let mut cold_blocks_tail: Option<BlockId> = None;
        let mut b = self.entry;
        while self.blocks[b].next.is_some() {
            while let Some(next) = self.blocks[b].next {
                if self.blocks[next].cold {
                    break;
                }
                b = next;
            }
            let Some(streak_start) = self.blocks[b].next else {
                break;
            };
            let mut b_end = streak_start;
            while let Some(next) = self.blocks[b_end].next {
                if !self.blocks[next].cold {
                    break;
                }
                b_end = next;
            }
            match cold_blocks_tail {
                None => cold_blocks = Some(streak_start),
                Some(tail) => self.blocks[tail].next = Some(streak_start),
            }
            cold_blocks_tail = Some(b_end);
            self.blocks[b].next = self.blocks[b_end].next;
            self.blocks[b_end].next = None;
        }
        self.blocks[b].next = cold_blocks;
        if cold_blocks.is_some() {
            self.remove_redundant_nops_and_jumps();
        }
    }

    /// `convert_pseudo_conditional_jumps`.
    fn convert_pseudo_conditional_jumps(&mut self) {
        for b in self.chain() {
            let mut i = 0usize;
            while i < self.blocks[b].used {
                let instr = self.blocks[b].slots[i];
                if matches!(instr.op, OpCode::JumpIfFalse | OpCode::JumpIfTrue) {
                    self.blocks[b].slots[i].op = if instr.op == OpCode::JumpIfFalse {
                        OpCode::PopJumpIfFalse
                    } else {
                        OpCode::PopJumpIfTrue
                    };
                    let copy = CfgInstr {
                        op: OpCode::CopyTop,
                        arg: 1,
                        loc: instr.loc,
                        target: None,
                        except: instr.except,
                    };
                    self.blocks[b].insert_instruction(i, copy);
                    i += 1;
                    let to_bool = CfgInstr {
                        op: OpCode::ToBool,
                        arg: 0,
                        loc: instr.loc,
                        target: None,
                        except: instr.except,
                    };
                    self.blocks[b].insert_instruction(i, to_bool);
                    i += 1;
                }
                i += 1;
            }
        }
    }

    /// `insert_prefix_instructions`: `COPY_FREE_VARS`, the `MAKE_CELL`s
    /// (in localsplus-slot order), and the generator prefix.
    fn insert_prefix_instructions(&mut self, co: &CodeObject) {
        let entry = self.entry;
        if self.generator_prefix {
            // `LOCATION(firstlineno, firstlineno, -1, -1)`.
            let loc = Loc {
                line: self.firstlineno,
                col: ColSpan {
                    end_lineno: self.firstlineno as u32,
                    col: -1,
                    end_col: -1,
                },
            };
            let make_gen = CfgInstr {
                op: OpCode::ReturnGenerator,
                arg: 0,
                loc,
                target: None,
                except: None,
            };
            self.blocks[entry].insert_instruction(0, make_gen);
            let pop_top = CfgInstr {
                op: OpCode::PopTop,
                arg: 0,
                loc,
                target: None,
                except: None,
            };
            self.blocks[entry].insert_instruction(1, pop_top);
        }
        let ncellvars = co.cellvars.len();
        if ncellvars > 0 {
            // Cells aliasing a parameter come first (they share its
            // slot), then the rest in `co_cellvars` order — CPython's
            // `fixed` offsets sorted ascending.
            let nlocals = co.varnames.len();
            let mut fixed: Vec<(usize, usize)> = (0..ncellvars)
                .map(|i| {
                    let slot = co
                        .varnames
                        .iter()
                        .position(|v| *v == co.cellvars[i])
                        .unwrap_or(nlocals + i);
                    (slot, i)
                })
                .collect();
            fixed.sort_unstable();
            for (ncellsused, (_, oldindex)) in fixed.into_iter().enumerate() {
                let make_cell = CfgInstr {
                    op: OpCode::MakeCell,
                    arg: oldindex as u32,
                    loc: NO_LOCATION,
                    target: None,
                    except: None,
                };
                self.blocks[entry].insert_instruction(ncellsused, make_cell);
            }
        }
        let nfreevars = co.freevars.len();
        if nfreevars > 0 {
            let copy_frees = CfgInstr {
                op: OpCode::CopyFreeVars,
                arg: nfreevars as u32,
                loc: NO_LOCATION,
                target: None,
                except: None,
            };
            self.blocks[entry].insert_instruction(0, copy_frees);
        }
    }

    /// `convert_pseudo_ops`.
    fn convert_pseudo_ops(&mut self) {
        for b in self.chain() {
            for instr in self.blocks[b].instrs_mut() {
                if is_block_push(instr.op) {
                    set_nop(instr);
                } else if instr.op == OpCode::StoreFastMaybeNull {
                    instr.op = OpCode::StoreFast;
                }
            }
        }
        self.remove_redundant_nops_and_jumps();
    }

    /// `normalize_jumps_in_block`.
    fn normalize_jumps_in_block(&mut self, b: BlockId) {
        let Some(last) = self.blocks[b].last().copied() else {
            return;
        };
        if !is_conditional_jump(last.op) {
            return;
        }
        let target = last.target.expect("conditional jump target");
        let is_forward = !self.blocks[target].visited;
        if is_forward {
            self.blocks[b].addop(OpCode::NotTaken, 0, last.loc);
            return;
        }
        let reversed_opcode = match last.op {
            OpCode::PopJumpIfNotNone => OpCode::PopJumpIfNone,
            OpCode::PopJumpIfNone => OpCode::PopJumpIfNotNone,
            OpCode::PopJumpIfFalse => OpCode::PopJumpIfTrue,
            OpCode::PopJumpIfTrue => OpCode::PopJumpIfFalse,
            _ => unreachable!(),
        };
        let backwards_jump = self.new_block();
        self.blocks[backwards_jump].addop(OpCode::NotTaken, 0, last.loc);
        self.blocks[backwards_jump].add_jump(OpCode::Jump, target, last.loc);
        self.blocks[backwards_jump].startdepth = self.blocks[target].startdepth;
        let n = self.blocks[b].used - 1;
        self.blocks[b].slots[n].op = reversed_opcode;
        let b_next = self.blocks[b].next;
        self.blocks[b].slots[n].target = b_next;
        self.blocks[backwards_jump].cold = self.blocks[b].cold;
        self.blocks[backwards_jump].next = b_next;
        self.blocks[b].next = Some(backwards_jump);
    }

    /// `normalize_jumps`.
    fn normalize_jumps(&mut self) {
        self.clear_visited();
        for b in self.chain() {
            self.blocks[b].visited = true;
            self.normalize_jumps_in_block(b);
        }
    }

    // ---------- stack depth + borrowed loads ----------

    /// `(popped, pushed)` of instruction `i` in block `b`
    /// (`_PyOpcode_num_popped` / `_PyOpcode_num_pushed`). `CallKw`
    /// additionally consumes one value per keyword name; the kwnames
    /// tuple is always the `LoadConst` right in front of it.
    fn shape(&self, b: BlockId, i: usize) -> (usize, usize) {
        let instr = &self.blocks[b].slots[i];
        let (mut popped, pushed) = crate::cpython_code::stack_shape(instr.op, instr.arg);
        if instr.op == OpCode::CallKw && i > 0 {
            let prev = &self.blocks[b].slots[i - 1];
            if prev.op == OpCode::LoadConst {
                if let Some(Constant::Tuple(names)) = self.consts.get(prev.arg as usize) {
                    popped += names.len();
                }
            }
        }
        (popped, pushed)
    }

    /// `get_stack_effects`: the net effect on the fallthrough
    /// (`jump == false`) or the jump edge. Block pushes only affect the
    /// stack when jumping to the handler.
    fn net_effect(&self, b: BlockId, i: usize, jump: bool) -> i32 {
        let op = self.blocks[b].slots[i].op;
        if is_block_push(op) && !jump {
            return 0;
        }
        let (popped, pushed) = self.shape(b, i);
        pushed as i32 - popped as i32
    }

    /// `stackdepth_push`.
    fn stackdepth_push(&mut self, b: BlockId, depth: i32, sp: &mut Vec<BlockId>) {
        debug_assert!(
            self.blocks[b].startdepth < 0 || self.blocks[b].startdepth == depth,
            "block {b} entered at inconsistent stack depths"
        );
        if self.blocks[b].startdepth < depth {
            self.blocks[b].startdepth = depth;
            sp.push(b);
        }
    }

    /// `calculate_stackdepth`: every reachable block's entry depth (and
    /// the maximum, `co_stacksize`), walking jump and fallthrough
    /// edges from the entry block. Exception handlers are reached
    /// through the still-present `SETUP_*` pseudo-ops' targets.
    fn calculate_stackdepth(&mut self) -> i32 {
        for b in 0..self.blocks.len() {
            self.blocks[b].startdepth = i32::MIN;
        }
        let mut maxdepth = 0i32;
        let mut sp: Vec<BlockId> = Vec::new();
        let entry = self.entry;
        self.stackdepth_push(entry, 0, &mut sp);
        while let Some(b) = sp.pop() {
            let mut depth = self.blocks[b].startdepth;
            debug_assert!(depth >= 0);
            let mut next = self.blocks[b].next;
            for i in 0..self.blocks[b].used {
                let instr = self.blocks[b].slots[i];
                let new_depth = depth + self.net_effect(b, i, false);
                maxdepth = maxdepth.max(new_depth);
                // `HAS_TARGET`: only jumps and block pushes have a
                // control edge (`PUSH_EXC_INFO` carries a VM tag, and a
                // NOP'd jump keeps its stale target).
                if let Some(target) = instr.target.filter(|_| has_target(instr.op)) {
                    if instr.op != OpCode::EndAsyncFor {
                        let target_depth = depth + self.net_effect(b, i, true);
                        maxdepth = maxdepth.max(target_depth);
                        self.stackdepth_push(target, target_depth, &mut sp);
                    }
                }
                depth = new_depth.max(0);
                if is_unconditional_jump(instr.op) || is_scope_exit(instr.op) {
                    // Remaining code is dead.
                    next = None;
                    break;
                }
            }
            if let Some(n) = next {
                self.stackdepth_push(n, depth, &mut sp);
            }
        }
        maxdepth
    }

    /// `load_fast_push_block`.
    fn load_fast_push_block(&mut self, target: BlockId, sp: &mut Vec<BlockId>) {
        if !self.blocks[target].visited {
            self.blocks[target].visited = true;
            sp.push(target);
        }
    }

    /// `optimize_load_fast`: strength-reduce `LOAD_FAST{_LOAD_FAST}`
    /// into the borrowing forms wherever the frame's own reference
    /// provably outlives the loaded one. Per basic block, a shadow
    /// stack of `(producing instruction, local)` refs is driven by each
    /// instruction's pop/push shape; a load is left alone when its ref
    /// is still on the stack while the local is killed, is stored into
    /// a local, or survives to the end of the block. Only blocks
    /// reached over jump and fallthrough edges from the entry are
    /// analysed (handler bodies keep plain loads).
    #[allow(clippy::too_many_lines)]
    fn optimize_load_fast(&mut self) {
        const NOT_LOCAL: i64 = -1;
        const DUMMY_INSTR: i64 = -1;
        const SUPPORT_KILLED: u8 = 1;
        const STORED_AS_LOCAL: u8 = 2;
        const REF_UNCONSUMED: u8 = 4;
        const CLOSURE_LOCAL_BASE: i64 = 1 << 32;
        #[derive(Clone, Copy)]
        struct Ref {
            instr: i64,
            local: i64,
        }
        const DUMMY: Ref = Ref {
            instr: DUMMY_INSTR,
            local: NOT_LOCAL,
        };
        fn kill_local(flags: &mut [u8], refs: &[Ref], local: i64) {
            for r in refs {
                if r.local == local {
                    debug_assert!(r.instr >= 0);
                    flags[r.instr as usize] |= SUPPORT_KILLED;
                }
            }
        }
        fn store_local(flags: &mut [u8], refs: &[Ref], local: i64, r: Ref) {
            kill_local(flags, refs, local);
            if r.instr != DUMMY_INSTR {
                flags[r.instr as usize] |= STORED_AS_LOCAL;
            }
        }
        fn pop(refs: &mut Vec<Ref>) -> Ref {
            refs.pop().unwrap_or(DUMMY)
        }

        self.clear_visited();
        let mut refs: Vec<Ref> = Vec::new();
        let mut flags: Vec<u8> = Vec::new();
        let mut sp: Vec<BlockId> = vec![self.entry];
        self.blocks[self.entry].startdepth = 0;
        self.blocks[self.entry].visited = true;
        while let Some(b) = sp.pop() {
            let used = self.blocks[b].used;
            flags.clear();
            flags.resize(used, 0);
            // Values on the stack at entry are opaque.
            refs.clear();
            let depth = self.blocks[b].startdepth.max(0);
            for _ in 0..depth {
                refs.push(DUMMY);
            }
            for i in 0..used {
                let instr = self.blocks[b].slots[i];
                let ii = i as i64;
                let oparg = i64::from(instr.arg);
                match instr.op {
                    OpCode::DeleteFast => kill_local(&mut flags, &refs, oparg),
                    OpCode::LoadFast => refs.push(Ref {
                        instr: ii,
                        local: oparg,
                    }),
                    // `LOAD_CLOSURE` is a `LOAD_FAST` of the cell's
                    // localsplus slot by the time CPython runs this pass;
                    // cells are only rebound through `*_DEREF`, so the
                    // reference lives in its own key space.
                    OpCode::LoadClosure => refs.push(Ref {
                        instr: ii,
                        local: CLOSURE_LOCAL_BASE + oparg,
                    }),
                    OpCode::LoadFastAndClear => {
                        kill_local(&mut flags, &refs, oparg);
                        refs.push(Ref {
                            instr: ii,
                            local: oparg,
                        });
                    }
                    OpCode::LoadFastLoadFast => {
                        refs.push(Ref {
                            instr: ii,
                            local: oparg >> 4,
                        });
                        refs.push(Ref {
                            instr: ii,
                            local: oparg & 15,
                        });
                    }
                    OpCode::StoreFast => {
                        let r = pop(&mut refs);
                        store_local(&mut flags, &refs, oparg, r);
                    }
                    OpCode::StoreFastLoadFast => {
                        let r = pop(&mut refs);
                        store_local(&mut flags, &refs, oparg >> 4, r);
                        refs.push(Ref {
                            instr: ii,
                            local: oparg & 15,
                        });
                    }
                    OpCode::StoreFastStoreFast => {
                        let r = pop(&mut refs);
                        store_local(&mut flags, &refs, oparg >> 4, r);
                        let r = pop(&mut refs);
                        store_local(&mut flags, &refs, oparg & 15, r);
                    }
                    OpCode::CopyTop => {
                        let idx = refs.len().saturating_sub(instr.arg.max(1) as usize);
                        let r = refs.get(idx).copied().unwrap_or(DUMMY);
                        refs.push(r);
                    }
                    OpCode::Swap => {
                        let len = refs.len();
                        let idx = len.saturating_sub(instr.arg.max(2) as usize);
                        if len >= 2 && idx < len {
                            refs.swap(idx, len - 1);
                        }
                    }
                    // Opcodes that consume no inputs: push their net
                    // effect. CPython 3.14's loop here reads
                    // `for (int i = 0; i < net_pushed; i++) PUSH_REF(i,
                    // NOT_LOCAL);` — the loop variable *shadows* the
                    // instruction index, so the pushed ref is attributed
                    // to instruction `j` of the block (instruction 0 for
                    // the usual single push). A later `STORE_FAST` that
                    // consumes it, or its survival to the block's end,
                    // therefore flags the block's *first* instruction: a
                    // `LOAD_FAST` heading a block that goes on to
                    // `IMPORT_FROM; STORE_FAST` stays un-borrowed
                    // (runpy.run_path's `run_name.rpartition` after
                    // `from pkgutil import get_importer`). Reproduced for
                    // byte-identical output.
                    OpCode::FormatValue if instr.arg & 0x04 == 0 => {
                        let (popped, pushed) = self.shape(b, i);
                        for j in 0..pushed.saturating_sub(popped) {
                            refs.push(Ref {
                                instr: j as i64,
                                local: NOT_LOCAL,
                            });
                        }
                    }
                    OpCode::GetAnext
                    | OpCode::GetLen
                    | OpCode::GetYieldFromIter
                    | OpCode::ImportFrom
                    | OpCode::MatchKeys
                    | OpCode::MatchMapping
                    | OpCode::MatchSequence
                    | OpCode::WithExceptStart => {
                        let (popped, pushed) = self.shape(b, i);
                        for j in 0..pushed.saturating_sub(popped) {
                            refs.push(Ref {
                                instr: j as i64,
                                local: NOT_LOCAL,
                            });
                        }
                    }
                    // Opcodes that consume some inputs and push nothing
                    // new (`DICT_MERGE`/`DICT_UPDATE` are both
                    // `DictUpdate`).
                    OpCode::DictUpdate
                    | OpCode::ListAppend
                    | OpCode::ListExtend
                    | OpCode::MapAdd
                    | OpCode::Reraise
                    | OpCode::SetAdd
                    | OpCode::SetUpdate => {
                        let (popped, pushed) = self.shape(b, i);
                        for _ in 0..popped.saturating_sub(pushed) {
                            pop(&mut refs);
                        }
                    }
                    OpCode::EndSend | OpCode::SetFunctionAttribute => {
                        let tos = pop(&mut refs);
                        pop(&mut refs);
                        refs.push(tos);
                    }
                    OpCode::CheckExcMatch => {
                        pop(&mut refs);
                        refs.push(Ref {
                            instr: ii,
                            local: NOT_LOCAL,
                        });
                    }
                    OpCode::ForIter => {
                        let target = instr.target.expect("FOR_ITER target");
                        self.load_fast_push_block(target, &mut sp);
                        refs.push(Ref {
                            instr: ii,
                            local: NOT_LOCAL,
                        });
                    }
                    OpCode::LoadAttr | OpCode::LoadMethodAttr | OpCode::LoadSuperAttr => {
                        let this = pop(&mut refs);
                        if instr.op == OpCode::LoadSuperAttr {
                            pop(&mut refs);
                            pop(&mut refs);
                        }
                        refs.push(Ref {
                            instr: ii,
                            local: NOT_LOCAL,
                        });
                        let method = match instr.op {
                            OpCode::LoadMethodAttr => true,
                            OpCode::LoadSuperAttr => instr.arg & 1 != 0,
                            _ => false,
                        };
                        if method {
                            // A method call; conservatively assume that
                            // self is pushed back onto the stack.
                            refs.push(this);
                        }
                    }
                    OpCode::LoadSpecial | OpCode::PushExcInfo => {
                        let tos = pop(&mut refs);
                        refs.push(Ref {
                            instr: ii,
                            local: NOT_LOCAL,
                        });
                        refs.push(tos);
                    }
                    OpCode::Send => {
                        let target = instr.target.expect("SEND target");
                        self.load_fast_push_block(target, &mut sp);
                        pop(&mut refs);
                        refs.push(Ref {
                            instr: ii,
                            local: NOT_LOCAL,
                        });
                    }
                    // Everything else consumes all of its inputs.
                    _ => {
                        let (popped, pushed) = self.shape(b, i);
                        if let Some(target) = instr.target.filter(|_| has_target(instr.op)) {
                            self.load_fast_push_block(target, &mut sp);
                        }
                        if !is_block_push(instr.op) {
                            for _ in 0..popped {
                                pop(&mut refs);
                            }
                            for _ in 0..pushed {
                                refs.push(Ref {
                                    instr: ii,
                                    local: NOT_LOCAL,
                                });
                            }
                        }
                    }
                }
            }
            // Push the fallthrough block.
            if let (Some(term), Some(next)) = (self.blocks[b].last().copied(), self.blocks[b].next)
            {
                if !(is_unconditional_jump(term.op) || is_scope_exit(term.op)) {
                    self.load_fast_push_block(next, &mut sp);
                }
            }
            // References still on the stack at the end of the block.
            for r in &refs {
                if r.instr != DUMMY_INSTR {
                    flags[r.instr as usize] |= REF_UNCONSUMED;
                }
            }
            for i in 0..used {
                if flags[i] != 0 {
                    continue;
                }
                let instr = &mut self.blocks[b].slots[i];
                match instr.op {
                    OpCode::LoadFast => instr.op = OpCode::LoadFastBorrow,
                    OpCode::LoadClosure => instr.op = OpCode::LoadClosureBorrow,
                    OpCode::LoadFastLoadFast => instr.op = OpCode::LoadFastBorrowLoadFastBorrow,
                    _ => {}
                }
            }
        }
    }

    // ---------- flattening ----------

    /// `_PyCfg_ToInstructionSequence` + the assembler's table
    /// construction, written straight back into `co`.
    fn flatten(self, co: &mut CodeObject) {
        use crate::bytecode::wire;
        let chain = self.chain();
        // Offsets: each block starts at the running instruction count.
        // The flowgraph-only fused opcodes lower to two runtime
        // instructions each.
        let width = |instr: &CfgInstr| -> u32 {
            match instr.op {
                OpCode::LoadGlobalPushNull
                | OpCode::LoadFastLoadFast
                | OpCode::LoadFastBorrowLoadFastBorrow
                | OpCode::StoreFastLoadFast
                | OpCode::StoreFastStoreFast => 2,
                _ => 1,
            }
        };
        let mut block_off: Vec<u32> = vec![0; self.blocks.len()];
        let mut off = 0u32;
        for &b in &chain {
            block_off[b] = off;
            off += self.blocks[b].instrs().iter().map(width).sum::<u32>();
        }
        let total = off as usize;
        let mut instructions: Vec<Instruction> = Vec::with_capacity(total);
        let mut linetable: Vec<u32> = Vec::with_capacity(total);
        let mut coltable: Vec<ColSpan> = Vec::with_capacity(total);
        let mut except_of: Vec<Option<HandlerId>> = Vec::with_capacity(total);
        let mut marks: Vec<u8> = Vec::with_capacity(total);
        let mut no_interrupt: Vec<u32> = Vec::new();
        for &b in &chain {
            for instr in self.blocks[b].instrs() {
                let at = instructions.len() as u32;
                let from = at + 1;
                let target_off = instr.target.map(|t| block_off[t]);
                let line = if instr.loc == NEXT_LOCATION {
                    crate::NEXT_LOCATION_LINE
                } else if instr.loc.line < 0 {
                    0
                } else {
                    instr.loc.line as u32
                };
                let col = if instr.loc.line < 0 {
                    ColSpan::default()
                } else {
                    instr.loc.col
                };
                // Fused forms: two runtime instructions sharing the
                // location and handler, marked head/tail for the codec.
                let fused: Option<(Instruction, Instruction, u8)> = match instr.op {
                    OpCode::LoadGlobalPushNull => Some((
                        Instruction::new(OpCode::LoadGlobal, instr.arg),
                        Instruction::new(OpCode::PushNull, 0),
                        wire::PLAIN,
                    )),
                    OpCode::LoadFastLoadFast | OpCode::LoadFastBorrowLoadFastBorrow => Some((
                        Instruction::new(OpCode::LoadFast, instr.arg >> 4),
                        Instruction::new(OpCode::LoadFast, instr.arg & 15),
                        if instr.op == OpCode::LoadFastBorrowLoadFastBorrow {
                            wire::BORROW
                        } else {
                            wire::PLAIN
                        },
                    )),
                    OpCode::StoreFastLoadFast => Some((
                        Instruction::new(OpCode::StoreFast, instr.arg >> 4),
                        Instruction::new(OpCode::LoadFast, instr.arg & 15),
                        wire::PLAIN,
                    )),
                    OpCode::StoreFastStoreFast => Some((
                        Instruction::new(OpCode::StoreFast, instr.arg >> 4),
                        Instruction::new(OpCode::StoreFast, instr.arg & 15),
                        wire::PLAIN,
                    )),
                    _ => None,
                };
                if let Some((head, tail, extra)) = fused {
                    instructions.push(head);
                    marks.push(wire::FUSE_HEAD | extra);
                    instructions.push(tail);
                    marks.push(wire::FUSE_TAIL);
                    for _ in 0..2 {
                        linetable.push(line);
                        coltable.push(col);
                        except_of.push(instr.except);
                    }
                    continue;
                }
                let (op, arg, mark) = match instr.op {
                    OpCode::LoadFastBorrow => (OpCode::LoadFast, instr.arg, wire::BORROW),
                    OpCode::LoadClosureBorrow => (OpCode::LoadClosure, instr.arg, wire::BORROW),
                    OpCode::LoadFastCheck => (OpCode::LoadFast, instr.arg, wire::CHECK),
                    other => (other, instr.arg, wire::PLAIN),
                };
                let instr = &CfgInstr { op, arg, ..*instr };
                let (op, arg) = match instr.op {
                    OpCode::Jump | OpCode::JumpNoInterrupt => {
                        let t = target_off.expect("jump target");
                        if t >= from {
                            (OpCode::JumpForward, t - from)
                        } else {
                            if instr.op == OpCode::JumpNoInterrupt {
                                no_interrupt.push(at);
                            }
                            (OpCode::JumpBackward, from - t)
                        }
                    }
                    OpCode::PopJumpIfFalse
                    | OpCode::PopJumpIfTrue
                    | OpCode::PopJumpIfNone
                    | OpCode::PopJumpIfNotNone
                    | OpCode::ForIter
                    | OpCode::Send => {
                        let t = target_off.expect("jump target");
                        debug_assert!(t >= from, "backward conditional jump after normalize_jumps");
                        (instr.op, t.saturating_sub(from))
                    }
                    OpCode::EndAsyncFor => (instr.op, 0),
                    OpCode::PushExcInfo => (instr.op, target_off.unwrap_or(0)),
                    OpCode::JumpIfFalse
                    | OpCode::JumpIfTrue
                    | OpCode::SetupFinally
                    | OpCode::SetupCleanup
                    | OpCode::SetupWith
                    | OpCode::PopBlock
                    | OpCode::StoreFastMaybeNull => {
                        unreachable!("pseudo-op {:?} survived the flowgraph", instr.op)
                    }
                    other => (other, instr.arg),
                };
                instructions.push(Instruction { op, arg });
                marks.push(mark);
                linetable.push(line);
                coltable.push(col);
                except_of.push(instr.except);
            }
        }
        // `assemble_location_info`: a `NEXT_LOCATION` takes the location
        // of the instruction after it (walked in reverse so chains
        // resolve), or `NO_LOCATION` on a terminator.
        for i in (0..total).rev() {
            if linetable[i] != crate::NEXT_LOCATION_LINE {
                continue;
            }
            if is_terminator(instructions[i].op) || i + 1 >= total {
                linetable[i] = 0;
                coltable[i] = ColSpan::default();
            } else {
                linetable[i] = linetable[i + 1];
                coltable[i] = coltable[i + 1];
            }
        }
        // Exception table: runs of one owner, in instruction order.
        let mut table: Vec<ExcHandler> = Vec::new();
        let mut i = 0usize;
        while i < total {
            let Some(h) = except_of[i] else {
                i += 1;
                continue;
            };
            let start = i;
            while i < total && except_of[i] == Some(h) {
                i += 1;
            }
            let info = self.handlers[h];
            // `assemble_exception_table`: the depth is the handler
            // block's entry depth less the exception (and lasti) the
            // unwinder pushes, as `calculate_stackdepth` established it
            // through the `SETUP_*` stand-in that targets the block.
            let startdepth = self.blocks[info.block].startdepth;
            let depth = if startdepth >= 0 {
                let d = startdepth - 1 - i32::from(info.lasti);
                u32::try_from(d).unwrap_or(0)
            } else {
                info.depth
            };
            table.push(ExcHandler {
                start: start as u32,
                end: i as u32,
                handler: block_off[info.block],
                depth,
                push_lasti: info.lasti,
            });
        }
        co.instructions = instructions;
        co.linetable = linetable;
        co.coltable = coltable;
        co.exception_table = table;
        co.constants = self.consts;
        no_interrupt.sort_unstable();
        co.no_interrupt_jumps = no_interrupt;
        co.wire_marks = if marks.iter().all(|&m| m == wire::PLAIN) {
            Vec::new()
        } else {
            marks
        };
    }
}

/// Run CPython's whole flowgraph pipeline over `co` in place.
pub(crate) fn optimize(co: &mut CodeObject, input: &BuildInput<'_>) {
    let mut cfg = build(co, input);
    let dump = std::env::var_os("WEAVEPY_CFG_DUMP").is_some();
    macro_rules! pass {
        ($name:literal, $body:expr) => {{
            $body;
            if dump {
                cfg.dump(&format!("{} (after {})", co.qualname, $name));
            }
        }};
    }
    if dump {
        cfg.dump(&format!("{} (built)", co.qualname));
    }
    // _PyCfg_OptimizeCodeUnit
    pass!("optimize_cfg", cfg.optimize_cfg());
    pass!("remove_unused_consts", cfg.remove_unused_consts());
    pass!(
        "add_checks_for_loads_of_uninitialized_variables",
        cfg.add_checks_for_loads_of_uninitialized_variables(co.varnames.len(), input.nparams)
    );
    pass!("insert_superinstructions", cfg.insert_superinstructions());
    pass!("push_cold_blocks_to_end", cfg.push_cold_blocks_to_end());
    pass!("resolve_line_numbers", cfg.resolve_line_numbers());
    // _PyCfg_OptimizedCfgToInstructionSequence
    pass!(
        "convert_pseudo_conditional_jumps",
        cfg.convert_pseudo_conditional_jumps()
    );
    pass!("calculate_stackdepth", cfg.calculate_stackdepth());
    pass!(
        "insert_prefix_instructions",
        cfg.insert_prefix_instructions(co)
    );
    pass!("convert_pseudo_ops", cfg.convert_pseudo_ops());
    pass!("normalize_jumps", cfg.normalize_jumps());
    // Can't modify the bytecode after inserting instructions that
    // produce borrowed references.
    pass!("optimize_load_fast", cfg.optimize_load_fast());
    cfg.flatten(co);
}

impl Cfg {
    /// Debug dump of the block chain (`WEAVEPY_CFG_DUMP=1`), in the
    /// spirit of CPython's `dump_basicblock`.
    fn dump(&self, title: &str) {
        eprintln!("== CFG {title}");
        for b in self.chain() {
            let blk = &self.blocks[b];
            eprintln!(
                "  B{b}: used={} preds={} depth={} cold={} handler={} next={:?}",
                blk.used, blk.predecessors, blk.startdepth, blk.cold, blk.except_handler, blk.next
            );
            for ins in blk.instrs() {
                eprintln!(
                    "      {:<28} arg={:<4} line={:<4} target={:<6} except={:?}",
                    ins.op.name(),
                    ins.arg,
                    ins.loc.line,
                    ins.target.map_or("-".to_string(), |t| format!("B{t}")),
                    ins.except
                );
            }
        }
    }
}

// ---------- constant evaluation ----------

fn to_ast_constant(c: &Constant) -> Option<AstConstant> {
    Some(match c {
        Constant::None => AstConstant::None,
        Constant::Bool(b) => AstConstant::Bool(*b),
        Constant::Int(i) => AstConstant::Int(*i),
        Constant::BigInt(b) => AstConstant::BigInt(b.to_string()),
        Constant::Float(f) => AstConstant::Float(*f),
        Constant::Complex(r, i) => AstConstant::Complex(*r, *i),
        Constant::Str(s) => AstConstant::Str(s.clone()),
        Constant::WStr(w) => AstConstant::WStr(w.clone()),
        Constant::Bytes(b) => AstConstant::Bytes(b.clone()),
        Constant::Tuple(items) => AstConstant::Tuple(
            items
                .iter()
                .map(to_ast_constant)
                .collect::<Option<Vec<_>>>()?,
        ),
        Constant::FrozenSet(items) => AstConstant::FrozenSet(
            items
                .iter()
                .map(to_ast_constant)
                .collect::<Option<Vec<_>>>()?,
        ),
        Constant::Ellipsis => AstConstant::Ellipsis,
        Constant::Code(_) | Constant::Slice(_) | Constant::Unmarshallable => return None,
    })
}

fn binop_to_ast(kind: BinOpKind) -> BinOp {
    match kind {
        BinOpKind::Add => BinOp::Add,
        BinOpKind::Sub => BinOp::Sub,
        BinOpKind::Mult => BinOp::Mult,
        BinOpKind::Div => BinOp::Div,
        BinOpKind::FloorDiv => BinOp::FloorDiv,
        BinOpKind::Mod => BinOp::Mod,
        BinOpKind::Pow => BinOp::Pow,
        BinOpKind::LShift => BinOp::LShift,
        BinOpKind::RShift => BinOp::RShift,
        BinOpKind::BitOr => BinOp::BitOr,
        BinOpKind::BitXor => BinOp::BitXor,
        BinOpKind::BitAnd => BinOp::BitAnd,
        BinOpKind::MatMult => BinOp::MatMult,
    }
}

/// `eval_const_binop` for the arithmetic operators (`NB_SUBSCR` is
/// [`eval_const_subscr`]).
fn eval_const_binop(lhs: &Constant, kind: BinOpKind, rhs: &Constant) -> Option<Constant> {
    let (l, r) = (to_ast_constant(lhs)?, to_ast_constant(rhs)?);
    ast_opt::eval_binop_const(&l, binop_to_ast(kind), &r).map(Constant::from)
}

/// `eval_const_unaryop`.
fn eval_const_unaryop(operand: &Constant, kind: UnaryKind) -> Option<Constant> {
    let c = to_ast_constant(operand)?;
    let op = match kind {
        UnaryKind::Pos => UnaryOp::UAdd,
        UnaryKind::Neg => UnaryOp::USub,
        UnaryKind::Not => UnaryOp::Not,
        UnaryKind::Invert => UnaryOp::Invert,
    };
    ast_opt::eval_unaryop_const(op, &c).map(Constant::from)
}

/// `PyObject_GetItem` on constants: sequence indexing and slicing of
/// `str` / `bytes` / `tuple`.
fn eval_const_subscr(container: &Constant, index: &Constant) -> Option<Constant> {
    match index {
        Constant::Slice(bounds) => {
            let (start, stop, step) = (&bounds.0, &bounds.1, &bounds.2);
            let step = match step {
                Constant::None => 1i64,
                _ => slice_int(step)?,
            };
            if step == 0 {
                return None;
            }
            let len = match container {
                Constant::Str(s) => s.chars().count(),
                Constant::WStr(w) => w.len(),
                Constant::Bytes(b) => b.len(),
                Constant::Tuple(t) => t.len(),
                _ => return None,
            } as i64;
            let indices = slice_indices(start, stop, step, len)?;
            Some(match container {
                Constant::Str(s) => {
                    let chars: Vec<char> = s.chars().collect();
                    Constant::Str(indices.iter().map(|&i| chars[i]).collect())
                }
                Constant::WStr(w) => {
                    let pts: Vec<u32> = indices.iter().map(|&i| w[i]).collect();
                    if pts.iter().all(|&p| char::from_u32(p).is_some()) {
                        Constant::Str(
                            pts.into_iter()
                                .map(|p| char::from_u32(p).unwrap())
                                .collect(),
                        )
                    } else {
                        Constant::WStr(pts)
                    }
                }
                Constant::Bytes(b) => Constant::Bytes(indices.iter().map(|&i| b[i]).collect()),
                Constant::Tuple(t) => {
                    Constant::Tuple(indices.iter().map(|&i| t[i].clone()).collect())
                }
                _ => unreachable!(),
            })
        }
        _ => {
            let (c, i) = (to_ast_constant(container)?, to_ast_constant(index)?);
            ast_opt::eval_subscr_const(&c, &i).map(Constant::from)
        }
    }
}

fn slice_int(c: &Constant) -> Option<i64> {
    match c {
        Constant::Bool(b) => Some(i64::from(*b)),
        Constant::Int(i) => Some(*i),
        _ => None,
    }
}

/// `PySlice_AdjustIndices` over `range(len)`.
fn slice_indices(start: &Constant, stop: &Constant, step: i64, len: i64) -> Option<Vec<usize>> {
    let clamp = |v: Option<i64>, default: i64, lower: i64, upper: i64| -> i64 {
        match v {
            None => default,
            Some(mut v) => {
                if v < 0 {
                    v += len;
                    if v < lower {
                        v = lower;
                    }
                } else if v > upper {
                    v = upper;
                }
                v
            }
        }
    };
    let start_v = match start {
        Constant::None => None,
        c => Some(slice_int(c)?),
    };
    let stop_v = match stop {
        Constant::None => None,
        c => Some(slice_int(c)?),
    };
    let (lower, upper) = if step < 0 { (-1, len - 1) } else { (0, len) };
    let start = clamp(start_v, if step < 0 { upper } else { lower }, lower, upper);
    let stop = clamp(stop_v, if step < 0 { lower } else { upper }, lower, upper);
    let mut out = Vec::new();
    let mut i = start;
    if step > 0 {
        while i < stop {
            out.push(i as usize);
            i += step;
        }
    } else {
        while i > stop {
            out.push(i as usize);
            i += step;
        }
    }
    Some(out)
}

/// `PyObject_IsTrue` on a constant.
fn const_truthy(c: &Constant) -> bool {
    match c {
        Constant::None => false,
        Constant::Bool(b) => *b,
        Constant::Int(v) => *v != 0,
        Constant::BigInt(b) => !num_traits::Zero::is_zero(b),
        Constant::Float(f) => *f != 0.0,
        Constant::Complex(r, i) => *r != 0.0 || *i != 0.0,
        Constant::Str(s) => !s.is_empty(),
        Constant::WStr(p) => !p.is_empty(),
        Constant::Bytes(b) => !b.is_empty(),
        Constant::Tuple(t) => !t.is_empty(),
        Constant::FrozenSet(s) => !s.is_empty(),
        Constant::Ellipsis | Constant::Code(_) | Constant::Slice(_) | Constant::Unmarshallable => {
            true
        }
    }
}

/// `PyFrozenSet_New` over a tuple of constants: drop duplicates by
/// Python equality.
fn dedup_constants(items: Vec<Constant>) -> Vec<Constant> {
    let ast_items: Option<Vec<AstConstant>> = items.iter().map(to_ast_constant).collect();
    match ast_items {
        Some(a) => ast_opt::dedup_py(a)
            .into_iter()
            .map(Constant::from)
            .collect(),
        None => items,
    }
}

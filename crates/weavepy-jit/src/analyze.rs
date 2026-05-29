//! JITability analysis: bytecode → [`TFunc`], or a [`JitVerdict`]
//! explaining why a code object is outside the v1 subset.
//!
//! The pipeline is:
//!
//! 1. **Block construction** — split the instruction stream into basic
//!    blocks at jump targets / after control-flow ops, resolving
//!    WeavePy's relative jumps to absolute instruction indices.
//! 2. **Reachability** — keep only blocks reachable from entry.
//! 3. **Definite assignment** — a forward must-analysis whose only job
//!    is to compute the *live-in* local set (slots read before written)
//!    that the VM type-guards before entering native code.
//! 4. **Type inference fixpoint** — abstract-interpret each block (with
//!    an empty entry stack) to assign each local slot one stable
//!    [`JitType`], bailing on any unsupported opcode, unrepresentable
//!    constant, mixed-lane arithmetic, non-uniform local, or non-empty
//!    block-boundary stack.
//! 5. **Emission** — once types converge, re-walk and emit [`TStmt`]s /
//!    [`TBlock`]s into a [`TFunc`].

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};

use weavepy_compiler::{BinOpKind, CodeObject, CompareKind, Constant, OpCode, UnaryKind};

use crate::ir::{ArithKind, BlockId, CmpKind, TBlock, TFunc, TOp, TStmt, TTerm};
use crate::value::JitType;

/// Why a code object could not be compiled by the v1 JIT. Carried back
/// to the VM so it can mark the frame `NotJitable` and stop retrying.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JitVerdict {
    /// An opcode outside the supported subset (named for diagnostics).
    UnsupportedOpcode(&'static str),
    /// A `LOAD_CONST` of a non-`int`/`float`/`bool` constant.
    UnsupportedConst,
    /// A local slot is assigned two different lanes across the region.
    NonUniformLocal(u32),
    /// An operand's type could not be resolved to a representable lane.
    TypeUnknown,
    /// The operand stack is non-empty at a basic-block boundary
    /// (short-circuit / ternary in the hot region).
    NonEmptyBoundaryStack,
    /// Arithmetic / comparison mixing `int` and `float` lanes.
    MixedArithTypes,
    /// The abstract stack underflowed (malformed or unsupported shape).
    StackUnderflow,
    /// A jump resolved outside the instruction stream.
    BadJumpTarget,
    /// Signature / kind the whole-function JIT doesn't handle
    /// (generators, `*args`, class bodies, …).
    UnsupportedSignature,
    /// Trivial / empty body — not worth compiling.
    Trivial,
    /// Type inference did not converge within the iteration budget.
    NotConverged,
}

/// A raw basic block over the original instruction indices.
#[derive(Debug, Clone)]
struct RawBlock {
    start: usize,
    end: usize,
    succs: Vec<usize>,
}

/// Maximum type-inference iterations before giving up.
const MAX_INFER_ITERS: usize = 64;

/// Analyze a code object. Returns the typed IR on success or a
/// [`JitVerdict`] describing the first disqualifying property found.
pub fn analyze(code: &CodeObject) -> Result<TFunc, JitVerdict> {
    if code.is_generator || code.is_coroutine || code.is_async_generator || code.is_class_body {
        return Err(JitVerdict::UnsupportedSignature);
    }
    if code.has_varargs || code.has_varkeywords || code.kwonly_count > 0 {
        return Err(JitVerdict::UnsupportedSignature);
    }
    let n = code.instructions.len();
    if n < 2 {
        return Err(JitVerdict::Trivial);
    }

    let raw = build_blocks(code)?;
    let reachable = reachable_blocks(&raw);
    if reachable.is_empty() {
        return Err(JitVerdict::Trivial);
    }

    let n_locals = code.varnames.len() as u32;
    let livein = compute_livein(code, &raw, &reachable, n_locals);

    // Type inference fixpoint.
    let mut local_types: Vec<Option<JitType>> = vec![None; n_locals as usize];
    let mut iters = 0;
    loop {
        let mut changed = false;
        for &bi in &reachable {
            infer_block(code, &raw[bi], &mut local_types, &mut changed)?;
        }
        if !changed {
            break;
        }
        iters += 1;
        if iters > MAX_INFER_ITERS {
            return Err(JitVerdict::NotConverged);
        }
    }

    // Compact block ids over reachable blocks (entry first is convenient
    // but not required — we record the entry id explicitly).
    let mut compact: HashMap<usize, BlockId> = HashMap::new();
    for (idx, &bi) in reachable.iter().enumerate() {
        compact.insert(bi, idx);
    }
    let entry_block = *compact
        .get(&block_index_at(&raw, 0))
        .ok_or(JitVerdict::Trivial)?;

    // Emission pass.
    let mut blocks: Vec<TBlock> = Vec::with_capacity(reachable.len());
    let mut max_stack = 0u32;
    for &bi in &reachable {
        let tb = emit_block(code, &raw[bi], &local_types, &compact, &mut max_stack)?;
        blocks.push(tb);
    }

    let mut livein_vec: Vec<u32> = livein.into_iter().collect();
    livein_vec.sort_unstable();

    Ok(TFunc {
        n_locals,
        local_types,
        livein_locals: livein_vec,
        max_stack,
        blocks,
        entry_block,
    })
}

/// Resolve a forward branch/jump target instruction index.
#[inline]
fn forward_target(i: usize, arg: u32) -> usize {
    i + 1 + arg as usize
}

/// Resolve a backward jump target instruction index.
#[inline]
fn backward_target(i: usize, arg: u32) -> Option<usize> {
    (i + 1).checked_sub(arg as usize)
}

/// Build the basic blocks, resolving relative jumps to absolute indices.
fn build_blocks(code: &CodeObject) -> Result<Vec<RawBlock>, JitVerdict> {
    let n = code.instructions.len();
    let mut leaders: BTreeSet<usize> = BTreeSet::new();
    leaders.insert(0);
    for (i, ins) in code.instructions.iter().enumerate() {
        match ins.op {
            OpCode::PopJumpIfFalse | OpCode::PopJumpIfTrue => {
                let t = forward_target(i, ins.arg);
                if t > n {
                    return Err(JitVerdict::BadJumpTarget);
                }
                leaders.insert(t);
                if i + 1 < n {
                    leaders.insert(i + 1);
                }
            }
            OpCode::JumpForward => {
                let t = forward_target(i, ins.arg);
                if t > n {
                    return Err(JitVerdict::BadJumpTarget);
                }
                leaders.insert(t);
                if i + 1 < n {
                    leaders.insert(i + 1);
                }
            }
            OpCode::JumpBackward => {
                let t = backward_target(i, ins.arg).ok_or(JitVerdict::BadJumpTarget)?;
                leaders.insert(t);
                if i + 1 < n {
                    leaders.insert(i + 1);
                }
            }
            OpCode::ReturnValue if i + 1 < n => {
                leaders.insert(i + 1);
            }
            _ => {}
        }
    }

    let leader_vec: Vec<usize> = leaders.iter().copied().collect();
    let index_of: HashMap<usize, usize> = leader_vec
        .iter()
        .enumerate()
        .map(|(idx, &pc)| (pc, idx))
        .collect();

    let mut blocks: Vec<RawBlock> = Vec::with_capacity(leader_vec.len());
    for (bi, &start) in leader_vec.iter().enumerate() {
        let end = leader_vec.get(bi + 1).copied().unwrap_or(n);
        let last = end - 1;
        let ins = code.instructions[last];
        let succs = match ins.op {
            OpCode::ReturnValue => Vec::new(),
            OpCode::JumpForward => vec![index_of[&forward_target(last, ins.arg)]],
            OpCode::JumpBackward => {
                vec![index_of[&backward_target(last, ins.arg).ok_or(JitVerdict::BadJumpTarget)?]]
            }
            OpCode::PopJumpIfFalse | OpCode::PopJumpIfTrue => {
                let t = index_of[&forward_target(last, ins.arg)];
                let f = index_of
                    .get(&(last + 1))
                    .copied()
                    .ok_or(JitVerdict::BadJumpTarget)?;
                vec![f, t]
            }
            // Falls through to the next block.
            _ => {
                let fall = index_of
                    .get(&end)
                    .copied()
                    .ok_or(JitVerdict::BadJumpTarget)?;
                vec![fall]
            }
        };
        blocks.push(RawBlock { start, end, succs });
    }
    Ok(blocks)
}

/// Index of the block whose `start == pc` (pc must be a leader).
fn block_index_at(raw: &[RawBlock], pc: usize) -> usize {
    raw.iter().position(|b| b.start == pc).unwrap_or(0)
}

/// Blocks reachable from the entry (block 0), in deterministic order.
fn reachable_blocks(raw: &[RawBlock]) -> Vec<usize> {
    let mut seen = vec![false; raw.len()];
    let mut order = Vec::new();
    let mut q = VecDeque::new();
    if !raw.is_empty() {
        q.push_back(0usize);
        seen[0] = true;
    }
    while let Some(b) = q.pop_front() {
        order.push(b);
        for &s in &raw[b].succs {
            if !seen[s] {
                seen[s] = true;
                q.push_back(s);
            }
        }
    }
    order.sort_unstable();
    order
}

/// Compute the live-in local set via a definite-assignment must-analysis.
fn compute_livein(
    code: &CodeObject,
    raw: &[RawBlock],
    reachable: &[usize],
    n_locals: u32,
) -> HashSet<u32> {
    let param_slots: HashSet<u32> = (0..code.arg_count).collect();
    let reachset: HashSet<usize> = reachable.iter().copied().collect();

    // Predecessors among reachable blocks.
    let mut preds: Vec<Vec<usize>> = vec![Vec::new(); raw.len()];
    for &b in reachable {
        for &s in &raw[b].succs {
            if reachset.contains(&s) {
                preds[s].push(b);
            }
        }
    }

    let full: HashSet<u32> = (0..n_locals).collect();
    let entry = block_index_at(raw, 0);
    let mut assigned_in: Vec<HashSet<u32>> = vec![full.clone(); raw.len()];
    if let Some(slot) = assigned_in.get_mut(entry) {
        *slot = param_slots.clone();
    }

    // Fixpoint: assigned_in[b] = ∩ assigned_out[pred].
    loop {
        let mut changed = false;
        for &b in reachable {
            let new_in = if b == entry {
                param_slots.clone()
            } else if preds[b].is_empty() {
                // Unreachable-but-listed guard; treat as empty.
                HashSet::new()
            } else {
                let mut acc: Option<HashSet<u32>> = None;
                for &p in &preds[b] {
                    let out = assigned_out(code, &raw[p], &assigned_in[p]);
                    acc = Some(match acc {
                        None => out,
                        Some(a) => a.intersection(&out).copied().collect(),
                    });
                }
                acc.unwrap_or_default()
            };
            if new_in != assigned_in[b] {
                assigned_in[b] = new_in;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }

    // Collect live-in: a load of a slot not definitely assigned yet.
    let mut livein = HashSet::new();
    for &b in reachable {
        let mut cur = assigned_in[b].clone();
        for i in raw[b].start..raw[b].end {
            let ins = code.instructions[i];
            match ins.op {
                OpCode::LoadFast if !cur.contains(&ins.arg) => {
                    livein.insert(ins.arg);
                }
                OpCode::StoreFast => {
                    cur.insert(ins.arg);
                }
                _ => {}
            }
        }
    }
    livein
}

/// `assigned_in ∪ {slots stored in this block}`.
fn assigned_out(code: &CodeObject, b: &RawBlock, assigned_in: &HashSet<u32>) -> HashSet<u32> {
    let mut out = assigned_in.clone();
    for i in b.start..b.end {
        let ins = code.instructions[i];
        if matches!(ins.op, OpCode::StoreFast) {
            out.insert(ins.arg);
        }
    }
    out
}

/// One operand-stack entry during analysis, with provenance for the
/// live-in inference (`src` is the slot of an as-yet-untyped load).
#[derive(Clone, Copy)]
struct SE {
    ty: JitType,
    src: Option<u32>,
}

impl SE {
    fn known(ty: JitType) -> SE {
        SE { ty, src: None }
    }
}

/// Map a representable [`Constant`] to its lane, or `None`.
fn const_type(c: &Constant) -> Option<JitType> {
    match c {
        Constant::Int(_) => Some(JitType::Int),
        Constant::Bool(_) => Some(JitType::Bool),
        Constant::Float(_) => Some(JitType::Float),
        _ => None,
    }
}

/// Infer/validate one block during the fixpoint. Mutates `local_types`
/// (setting `changed` when it grows) and bails on hard errors. Transient
/// `Unknown` operands are tolerated — a later iteration may resolve them.
fn infer_block(
    code: &CodeObject,
    b: &RawBlock,
    local_types: &mut [Option<JitType>],
    changed: &mut bool,
) -> Result<(), JitVerdict> {
    let mut stack: Vec<SE> = Vec::new();
    for i in b.start..(b.end - 1) {
        step_abstract(code, i, &mut stack, local_types, changed, false)?;
    }
    // Terminator stack-shape validation.
    let last = b.end - 1;
    let ins = code.instructions[last];
    match ins.op {
        OpCode::ReturnValue => {
            if stack.is_empty() {
                return Err(JitVerdict::StackUnderflow);
            }
        }
        OpCode::JumpForward | OpCode::JumpBackward => {
            if !stack.is_empty() {
                return Err(JitVerdict::NonEmptyBoundaryStack);
            }
        }
        OpCode::PopJumpIfFalse | OpCode::PopJumpIfTrue => {
            if stack.len() != 1 {
                return Err(JitVerdict::NonEmptyBoundaryStack);
            }
            let c = stack[0];
            if !c.ty.is_representable() && c.src.is_none() {
                return Err(JitVerdict::TypeUnknown);
            }
        }
        // Fall-through terminator: must leave an empty stack.
        _ => {
            step_abstract(code, last, &mut stack, local_types, changed, false)?;
            if !stack.is_empty() {
                return Err(JitVerdict::NonEmptyBoundaryStack);
            }
        }
    }
    Ok(())
}

/// Abstract-execute one non-terminator instruction, updating the type
/// stack and (via inference) `local_types`.
fn step_abstract(
    code: &CodeObject,
    i: usize,
    stack: &mut Vec<SE>,
    local_types: &mut [Option<JitType>],
    changed: &mut bool,
    strict: bool,
) -> Result<(), JitVerdict> {
    let ins = code.instructions[i];
    match ins.op {
        OpCode::Nop | OpCode::Resume => {}
        OpCode::LoadConst => {
            let c = code
                .constants
                .get(ins.arg as usize)
                .ok_or(JitVerdict::UnsupportedConst)?;
            let ty = const_type(c).ok_or(JitVerdict::UnsupportedConst)?;
            stack.push(SE::known(ty));
        }
        OpCode::LoadFast => {
            let slot = ins.arg as usize;
            match local_types.get(slot).copied().flatten() {
                Some(t) => stack.push(SE::known(t)),
                None => stack.push(SE {
                    ty: JitType::Unknown,
                    src: Some(ins.arg),
                }),
            }
        }
        OpCode::StoreFast => {
            let v = stack.pop().ok_or(JitVerdict::StackUnderflow)?;
            if v.ty.is_representable() {
                set_local(local_types, ins.arg, v.ty, changed)?;
            } else if strict {
                return Err(JitVerdict::TypeUnknown);
            }
        }
        OpCode::BinaryOp => {
            let kind = bin_kind(ins.arg)?;
            let b = stack.pop().ok_or(JitVerdict::StackUnderflow)?;
            let a = stack.pop().ok_or(JitVerdict::StackUnderflow)?;
            let (a, b) = resolve_pair(a, b, local_types, changed);
            let res = bin_result_type(kind, a.ty, b.ty, strict)?;
            stack.push(SE::known(res));
        }
        OpCode::CompareOp => {
            let _ = cmp_kind(ins.arg)?;
            let b = stack.pop().ok_or(JitVerdict::StackUnderflow)?;
            let a = stack.pop().ok_or(JitVerdict::StackUnderflow)?;
            let (a, b) = resolve_pair(a, b, local_types, changed);
            cmp_check(a.ty, b.ty, strict)?;
            stack.push(SE::known(JitType::Bool));
        }
        OpCode::UnaryOp => {
            let kind = unary_kind(ins.arg)?;
            let a = stack.pop().ok_or(JitVerdict::StackUnderflow)?;
            let res = unary_result_type(kind, a.ty, strict)?;
            stack.push(SE::known(res));
        }
        OpCode::PopTop => {
            stack.pop().ok_or(JitVerdict::StackUnderflow)?;
        }
        OpCode::CopyTop => {
            let v = *stack.last().ok_or(JitVerdict::StackUnderflow)?;
            stack.push(v);
        }
        OpCode::Swap => {
            if ins.arg != 2 {
                return Err(JitVerdict::UnsupportedOpcode("SWAP n!=2"));
            }
            let len = stack.len();
            if len < 2 {
                return Err(JitVerdict::StackUnderflow);
            }
            stack.swap(len - 1, len - 2);
        }
        other => return Err(JitVerdict::UnsupportedOpcode(other.name())),
    }
    Ok(())
}

/// If exactly one operand is an untyped live-in load and the other is a
/// concrete lane, infer the live-in's type.
fn resolve_pair(
    mut a: SE,
    mut b: SE,
    local_types: &mut [Option<JitType>],
    changed: &mut bool,
) -> (SE, SE) {
    if a.ty.is_representable() && !b.ty.is_representable() {
        if let Some(slot) = b.src {
            let _ = set_local(local_types, slot, a.ty, changed);
            b.ty = a.ty;
            b.src = None;
        }
    } else if b.ty.is_representable() && !a.ty.is_representable() {
        if let Some(slot) = a.src {
            let _ = set_local(local_types, slot, b.ty, changed);
            a.ty = b.ty;
            a.src = None;
        }
    }
    (a, b)
}

/// Assign a local's lane, enforcing single-type stability.
fn set_local(
    local_types: &mut [Option<JitType>],
    slot: u32,
    ty: JitType,
    changed: &mut bool,
) -> Result<(), JitVerdict> {
    let cell = local_types
        .get_mut(slot as usize)
        .ok_or(JitVerdict::TypeUnknown)?;
    match *cell {
        None => {
            *cell = Some(ty);
            *changed = true;
            Ok(())
        }
        Some(existing) if existing == ty => Ok(()),
        Some(_) => Err(JitVerdict::NonUniformLocal(slot)),
    }
}

/// Result lane of a binary arithmetic op, given operand lanes.
fn bin_result_type(
    kind: ArithKind,
    a: JitType,
    b: JitType,
    strict: bool,
) -> Result<JitType, JitVerdict> {
    if !a.is_representable() || !b.is_representable() {
        return if strict {
            Err(JitVerdict::TypeUnknown)
        } else {
            Ok(JitType::Unknown)
        };
    }
    let a_int = a.is_integral();
    let b_int = b.is_integral();
    if a_int && b_int {
        match kind {
            ArithKind::TrueDiv => Ok(JitType::Float),
            ArithKind::And | ArithKind::Or | ArithKind::Xor => {
                // bool∘bool stays bool in Python; we bail on that rare
                // case to keep the lane unambiguous.
                if a == JitType::Bool && b == JitType::Bool {
                    Err(JitVerdict::UnsupportedOpcode("bitwise on bool"))
                } else {
                    Ok(JitType::Int)
                }
            }
            _ => Ok(JitType::Int),
        }
    } else if a == JitType::Float && b == JitType::Float {
        match kind {
            ArithKind::Add | ArithKind::Sub | ArithKind::Mul | ArithKind::TrueDiv => {
                Ok(JitType::Float)
            }
            _ => Err(JitVerdict::UnsupportedOpcode("float floordiv/mod/bitop")),
        }
    } else {
        Err(JitVerdict::MixedArithTypes)
    }
}

/// Validate comparison operand lanes (same lane required in v1).
fn cmp_check(a: JitType, b: JitType, strict: bool) -> Result<(), JitVerdict> {
    if !a.is_representable() || !b.is_representable() {
        return if strict {
            Err(JitVerdict::TypeUnknown)
        } else {
            Ok(())
        };
    }
    if (a.is_integral() && b.is_integral()) || (a == JitType::Float && b == JitType::Float) {
        Ok(())
    } else {
        Err(JitVerdict::MixedArithTypes)
    }
}

/// Result lane of a unary op.
fn unary_result_type(kind: UnaryKind, a: JitType, strict: bool) -> Result<JitType, JitVerdict> {
    if !a.is_representable() {
        return if strict {
            Err(JitVerdict::TypeUnknown)
        } else {
            Ok(JitType::Unknown)
        };
    }
    match kind {
        UnaryKind::Not => Ok(JitType::Bool),
        UnaryKind::Neg | UnaryKind::Invert => {
            if a.is_integral() {
                Ok(JitType::Int)
            } else if matches!(kind, UnaryKind::Neg) {
                Ok(JitType::Float)
            } else {
                Err(JitVerdict::UnsupportedOpcode("~float"))
            }
        }
        UnaryKind::Pos => {
            if a == JitType::Float {
                Ok(JitType::Float)
            } else if a == JitType::Int {
                Ok(JitType::Int)
            } else {
                Err(JitVerdict::UnsupportedOpcode("+bool"))
            }
        }
    }
}

fn bin_kind(arg: u32) -> Result<ArithKind, JitVerdict> {
    let k = match arg {
        x if x == BinOpKind::Add as u32 => ArithKind::Add,
        x if x == BinOpKind::Sub as u32 => ArithKind::Sub,
        x if x == BinOpKind::Mult as u32 => ArithKind::Mul,
        x if x == BinOpKind::Div as u32 => ArithKind::TrueDiv,
        x if x == BinOpKind::FloorDiv as u32 => ArithKind::FloorDiv,
        x if x == BinOpKind::Mod as u32 => ArithKind::Mod,
        x if x == BinOpKind::BitOr as u32 => ArithKind::Or,
        x if x == BinOpKind::BitXor as u32 => ArithKind::Xor,
        x if x == BinOpKind::BitAnd as u32 => ArithKind::And,
        _ => return Err(JitVerdict::UnsupportedOpcode("BINARY_OP kind")),
    };
    Ok(k)
}

fn cmp_kind(arg: u32) -> Result<CmpKind, JitVerdict> {
    let k = match arg {
        x if x == CompareKind::Lt as u32 => CmpKind::Lt,
        x if x == CompareKind::LtE as u32 => CmpKind::Le,
        x if x == CompareKind::Eq as u32 => CmpKind::Eq,
        x if x == CompareKind::NotEq as u32 => CmpKind::Ne,
        x if x == CompareKind::Gt as u32 => CmpKind::Gt,
        x if x == CompareKind::GtE as u32 => CmpKind::Ge,
        _ => return Err(JitVerdict::UnsupportedOpcode("COMPARE_OP kind")),
    };
    Ok(k)
}

fn unary_kind(arg: u32) -> Result<UnaryKind, JitVerdict> {
    let k = match arg {
        x if x == UnaryKind::Pos as u32 => UnaryKind::Pos,
        x if x == UnaryKind::Neg as u32 => UnaryKind::Neg,
        x if x == UnaryKind::Not as u32 => UnaryKind::Not,
        x if x == UnaryKind::Invert as u32 => UnaryKind::Invert,
        _ => return Err(JitVerdict::UnsupportedOpcode("UNARY_OP kind")),
    };
    Ok(k)
}

/// Emit the typed IR for one block, with all local types now known.
fn emit_block(
    code: &CodeObject,
    b: &RawBlock,
    local_types: &[Option<JitType>],
    compact: &HashMap<usize, BlockId>,
    max_stack: &mut u32,
) -> Result<TBlock, JitVerdict> {
    let mut stack: Vec<JitType> = Vec::new();
    let mut stmts: Vec<TStmt> = Vec::new();

    for i in b.start..(b.end - 1) {
        emit_instr(code, i, local_types, &mut stack, &mut stmts, max_stack)?;
    }

    let last = b.end - 1;
    let ins = code.instructions[last];
    let term = match ins.op {
        OpCode::ReturnValue => {
            // Lowering pops the return value off its own type stack at
            // the `Return` terminator; no statement is emitted here.
            if stack.is_empty() {
                return Err(JitVerdict::StackUnderflow);
            }
            TTerm::Return
        }
        OpCode::JumpForward | OpCode::JumpBackward => {
            let t = compact[&block_succ(b, 0)];
            TTerm::Jump(t)
        }
        OpCode::PopJumpIfFalse => TTerm::BranchFalse {
            fallthrough: compact[&block_succ(b, 0)],
            target: compact[&block_succ(b, 1)],
        },
        OpCode::PopJumpIfTrue => TTerm::BranchTrue {
            fallthrough: compact[&block_succ(b, 0)],
            target: compact[&block_succ(b, 1)],
        },
        _ => {
            emit_instr(code, last, local_types, &mut stack, &mut stmts, max_stack)?;
            TTerm::Jump(compact[&block_succ(b, 0)])
        }
    };

    // Entry stack is always empty in the v1 subset.
    Ok(TBlock {
        entry_stack: Vec::new(),
        stmts,
        term,
    })
}

/// The raw successor block index at position `k`.
fn block_succ(b: &RawBlock, k: usize) -> usize {
    b.succs[k]
}

/// Emit one instruction's [`TStmt`](s), tracking the type stack so
/// result lanes match what lowering will reconstruct.
fn emit_instr(
    code: &CodeObject,
    i: usize,
    local_types: &[Option<JitType>],
    stack: &mut Vec<JitType>,
    stmts: &mut Vec<TStmt>,
    max_stack: &mut u32,
) -> Result<(), JitVerdict> {
    let ins = code.instructions[i];
    let pc = i as u32;
    let mut push =
        |op: TOp, ty: Option<JitType>, stack: &mut Vec<JitType>, stmts: &mut Vec<TStmt>| {
            stmts.push(TStmt { pc, op });
            if let Some(t) = ty {
                stack.push(t);
            }
            *max_stack = (*max_stack).max(stack.len() as u32);
        };
    match ins.op {
        OpCode::Nop | OpCode::Resume => {}
        OpCode::LoadConst => {
            let c = &code.constants[ins.arg as usize];
            let (op, ty) = match c {
                Constant::Int(v) => (TOp::PushConstInt(*v), JitType::Int),
                Constant::Bool(v) => (TOp::PushConstBool(*v), JitType::Bool),
                Constant::Float(v) => (TOp::PushConstFloat(v.to_bits()), JitType::Float),
                _ => return Err(JitVerdict::UnsupportedConst),
            };
            push(op, Some(ty), stack, stmts);
        }
        OpCode::LoadFast => {
            let ty = local_types
                .get(ins.arg as usize)
                .copied()
                .flatten()
                .ok_or(JitVerdict::TypeUnknown)?;
            push(TOp::LoadLocal(ins.arg), Some(ty), stack, stmts);
        }
        OpCode::StoreFast => {
            stack.pop().ok_or(JitVerdict::StackUnderflow)?;
            push(TOp::StoreLocal(ins.arg), None, stack, stmts);
        }
        OpCode::BinaryOp => {
            let kind = bin_kind(ins.arg)?;
            let b = stack.pop().ok_or(JitVerdict::StackUnderflow)?;
            let a = stack.pop().ok_or(JitVerdict::StackUnderflow)?;
            let (op, ty) = lower_bin(kind, a, b)?;
            push(op, Some(ty), stack, stmts);
        }
        OpCode::CompareOp => {
            let kind = cmp_kind(ins.arg)?;
            let b = stack.pop().ok_or(JitVerdict::StackUnderflow)?;
            let a = stack.pop().ok_or(JitVerdict::StackUnderflow)?;
            let op = if a.is_integral() && b.is_integral() {
                TOp::IntCmp(kind)
            } else if a == JitType::Float && b == JitType::Float {
                TOp::FloatCmp(kind)
            } else {
                return Err(JitVerdict::MixedArithTypes);
            };
            push(op, Some(JitType::Bool), stack, stmts);
        }
        OpCode::UnaryOp => {
            let kind = unary_kind(ins.arg)?;
            let a = stack.pop().ok_or(JitVerdict::StackUnderflow)?;
            match (kind, a) {
                (UnaryKind::Pos, JitType::Int | JitType::Float) => {
                    // Identity; re-push same lane, emit nothing.
                    stack.push(a);
                }
                (UnaryKind::Neg, t) if t.is_integral() => {
                    push(TOp::IntNeg, Some(JitType::Int), stack, stmts)
                }
                (UnaryKind::Neg, JitType::Float) => {
                    push(TOp::FloatNeg, Some(JitType::Float), stack, stmts);
                }
                (UnaryKind::Invert, t) if t.is_integral() => {
                    push(TOp::IntInvert, Some(JitType::Int), stack, stmts);
                }
                (UnaryKind::Not, t) if t.is_integral() => {
                    push(TOp::IntNot, Some(JitType::Bool), stack, stmts);
                }
                (UnaryKind::Not, JitType::Float) => {
                    push(TOp::FloatNot, Some(JitType::Bool), stack, stmts);
                }
                _ => return Err(JitVerdict::UnsupportedOpcode("UNARY_OP lane")),
            }
        }
        OpCode::PopTop => {
            stack.pop().ok_or(JitVerdict::StackUnderflow)?;
            push(TOp::Pop, None, stack, stmts);
        }
        OpCode::CopyTop => {
            let t = *stack.last().ok_or(JitVerdict::StackUnderflow)?;
            push(TOp::Dup, Some(t), stack, stmts);
        }
        OpCode::Swap => {
            if ins.arg != 2 {
                return Err(JitVerdict::UnsupportedOpcode("SWAP n!=2"));
            }
            let len = stack.len();
            if len < 2 {
                return Err(JitVerdict::StackUnderflow);
            }
            stack.swap(len - 1, len - 2);
            push(TOp::Swap2, None, stack, stmts);
        }
        other => return Err(JitVerdict::UnsupportedOpcode(other.name())),
    }
    Ok(())
}

/// Choose the IR op + result lane for a binary arithmetic op at emission
/// time (types are all known).
fn lower_bin(kind: ArithKind, a: JitType, b: JitType) -> Result<(TOp, JitType), JitVerdict> {
    if a.is_integral() && b.is_integral() {
        match kind {
            ArithKind::TrueDiv => Ok((TOp::IntTrueDiv, JitType::Float)),
            ArithKind::And | ArithKind::Or | ArithKind::Xor => {
                if a == JitType::Bool && b == JitType::Bool {
                    Err(JitVerdict::UnsupportedOpcode("bitwise on bool"))
                } else {
                    Ok((TOp::IntArith(kind), JitType::Int))
                }
            }
            _ => Ok((TOp::IntArith(kind), JitType::Int)),
        }
    } else if a == JitType::Float && b == JitType::Float {
        match kind {
            ArithKind::Add | ArithKind::Sub | ArithKind::Mul | ArithKind::TrueDiv => {
                Ok((TOp::FloatArith(kind), JitType::Float))
            }
            _ => Err(JitVerdict::UnsupportedOpcode("float floordiv/mod/bitop")),
        }
    } else {
        Err(JitVerdict::MixedArithTypes)
    }
}

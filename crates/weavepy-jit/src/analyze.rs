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

use crate::ir::{
    ArithKind, BlockId, CalleeSpanMeta, CmpKind, GlobalGuard, OsrEntry, RangeLoopMeta,
    ResolvedGlobal, TBlock, TFunc, TOp, TStmt, TTerm,
};
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

/// The bytecode rewrite plan computed before block construction
/// (RFC 0058 WS4): which pcs become no-ops, which `CALL`s store range
/// bounds into synthetic slots, which `FOR_ITER`s become counted-loop
/// terminators, and which `LOAD_GLOBAL`s burn in as constants.
#[derive(Default)]
struct Plan {
    /// pcs erased from the rewritten program: the `LOAD_GLOBAL range`,
    /// `GET_ITER`, `END_FOR`, and an explicit unit-step `LOAD_CONST 1`.
    nop: HashSet<usize>,
    /// `CALL` pcs → (values to pop, cur slot, stop slot). One popped
    /// value means `range(stop)` (cur seeds to 0); two means
    /// `range(start, stop)`.
    calls: HashMap<usize, (u8, u32, u32)>,
    /// `FOR_ITER` pcs → (cur slot, stop slot, loop variable slot).
    headers: HashMap<usize, (u32, u32, u32)>,
    /// The fused `STORE_FAST` pc directly after each `FOR_ITER` → its
    /// slot. The `ForRange` terminator performs this store.
    fused_store: HashMap<usize, u32>,
    /// `LOAD_GLOBAL` name index → resolution (resolved once up front so
    /// the inference fixpoint doesn't re-query the embedder).
    globals: HashMap<u32, ResolvedGlobal>,
    /// Entry guards, deduplicated by name.
    guards: Vec<GlobalGuard>,
    /// Rewritten loops, outermost-first.
    loops: Vec<RangeLoopMeta>,
    /// Synthetic slots appended after the code object's real locals.
    n_synth: u32,
}

impl Plan {
    /// `true` when the interpreter would have a live range iterator on
    /// its stack at `pc` — i.e. `pc` is inside some rewritten loop.
    fn in_loop_span(&self, pc: usize) -> bool {
        self.loops
            .iter()
            .any(|l| (l.live_from as usize) <= pc && pc < l.live_to as usize)
    }
}

/// Analyze a code object. `resolve` maps a `LOAD_GLOBAL` name to what it
/// currently resolves to (the embedder re-validates every resolution as
/// an entry guard). Returns the typed IR on success or a [`JitVerdict`]
/// describing the first disqualifying property found.
pub fn analyze(
    code: &CodeObject,
    resolve: &mut dyn FnMut(&str) -> ResolvedGlobal,
) -> Result<TFunc, JitVerdict> {
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

    let plan = plan_rewrite(code, resolve)?;

    let raw = build_blocks(code)?;
    let reachable = reachable_blocks(&raw);
    if reachable.is_empty() {
        return Err(JitVerdict::Trivial);
    }

    let n_locals = code.varnames.len() as u32 + plan.n_synth;
    let livein = compute_livein(code, &raw, &reachable, code.varnames.len() as u32);

    // Type inference fixpoint. `ret_lane` is the function's own scalar
    // return lane, fed back into self-recursive `CallPy` results
    // (RFC 0059 WS3) — fib-shaped recursion converges in two passes.
    let mut local_types: Vec<Option<JitType>> = vec![None; n_locals as usize];
    let mut ret_lane: Option<JitType> = None;
    let mut iters = 0;
    loop {
        let mut changed = false;
        for &bi in &reachable {
            infer_block(
                code,
                &raw[bi],
                &plan,
                &mut local_types,
                &mut ret_lane,
                &mut changed,
            )?;
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
    let mut out = EmitOut {
        max_stack: 0,
        callee_spans: Vec::new(),
        max_call_args: 0,
    };
    for &bi in &reachable {
        let tb = emit_block(
            code,
            &raw[bi],
            &plan,
            &local_types,
            ret_lane,
            &compact,
            &mut out,
        )?;
        blocks.push(tb);
    }

    // OSR entry points (RFC 0059 WS3b): every backward-jump target is a
    // block leader with an empty boundary stack in this subset, so it is
    // enterable mid-frame once the VM packs the locals (and decomposes
    // any live range iterators into their synthetic slots).
    let mut osr_entries: Vec<OsrEntry> = Vec::new();
    let mut osr_seen: HashSet<usize> = HashSet::new();
    for (i, ins) in code.instructions.iter().enumerate() {
        if matches!(ins.op, OpCode::JumpBackward) {
            let t = backward_target(i, ins.arg).ok_or(JitVerdict::BadJumpTarget)?;
            if osr_seen.insert(t) {
                if let Some(&bid) = compact.get(&block_index_at(&raw, t)) {
                    osr_entries.push(OsrEntry {
                        pc: t as u32,
                        block: bid,
                    });
                }
            }
        }
    }
    osr_entries.sort_unstable_by_key(|e| e.pc);

    // Parameters flow in from the caller, so every *typed* parameter
    // slot must be entry-guarded even though the definite-assignment
    // analysis treats it as already assigned. (Without this, a hot
    // kernel first called with ints and later with a float would pack
    // the float as 0 and silently compute garbage.)
    let mut livein = livein;
    for slot in 0..code.arg_count {
        if local_types.get(slot as usize).copied().flatten().is_some() {
            livein.insert(slot);
        }
    }
    let mut livein_vec: Vec<u32> = livein.into_iter().collect();
    livein_vec.sort_unstable();

    Ok(TFunc {
        n_locals,
        local_types,
        livein_locals: livein_vec,
        max_stack: out.max_stack,
        blocks,
        entry_block,
        global_guards: plan.guards,
        range_loops: plan.loops,
        callee_spans: out.callee_spans,
        osr_entries,
        max_call_args: out.max_call_args,
        ret_lane: ret_lane.filter(|t| t.is_representable()),
    })
}

/// Recognize every `FOR_ITER` as the canonical counted `range` loop and
/// build the rewrite [`Plan`]. Any `FOR_ITER` that doesn't match the
/// shape disqualifies the whole frame (there is no generic iterator
/// support in the tier-2 subset), as does any `LOAD_GLOBAL` that neither
/// feeds a recognized loop nor resolves to a burnable constant.
fn plan_rewrite(
    code: &CodeObject,
    resolve: &mut dyn FnMut(&str) -> ResolvedGlobal,
) -> Result<Plan, JitVerdict> {
    let ins = &code.instructions;
    let n = ins.len();
    let n_real = code.varnames.len() as u32;
    let mut plan = Plan::default();

    // All jump-landing pcs, to reject jumps into the middle of a
    // recognized `range(...)` prefix.
    let mut targets: HashSet<usize> = HashSet::new();
    for (i, item) in ins.iter().enumerate() {
        match item.op {
            OpCode::PopJumpIfFalse
            | OpCode::PopJumpIfTrue
            | OpCode::JumpForward
            | OpCode::ForIter => {
                targets.insert(forward_target(i, item.arg));
            }
            OpCode::JumpBackward => {
                targets.insert(backward_target(i, item.arg).ok_or(JitVerdict::BadJumpTarget)?);
            }
            _ => {}
        }
    }

    // Resolve every LOAD_GLOBAL name once.
    for item in ins.iter() {
        if matches!(item.op, OpCode::LoadGlobal) {
            if let std::collections::hash_map::Entry::Vacant(e) = plan.globals.entry(item.arg) {
                let name = code
                    .names
                    .get(item.arg as usize)
                    .ok_or(JitVerdict::UnsupportedOpcode("LOAD_GLOBAL bad name"))?;
                e.insert(resolve(name));
            }
        }
    }

    for i in 0..n {
        if !matches!(ins[i].op, OpCode::ForIter) {
            continue;
        }
        let bail = || JitVerdict::UnsupportedOpcode("FOR_ITER (non-range shape)");
        let exit = forward_target(i, ins[i].arg);
        if exit >= n || !matches!(ins[exit].op, OpCode::EndFor) {
            return Err(bail());
        }
        // Fused loop-variable store.
        if i + 1 >= n || !matches!(ins[i + 1].op, OpCode::StoreFast) {
            return Err(bail());
        }
        let var_slot = ins[i + 1].arg;
        // Walk the prefix backwards: GET_ITER, CALL k, k simple args,
        // LOAD_GLOBAL <range>.
        if i < 2
            || !matches!(ins[i - 1].op, OpCode::GetIter)
            || !matches!(ins[i - 2].op, OpCode::Call)
        {
            return Err(bail());
        }
        let k = ins[i - 2].arg as usize;
        if !(1..=3).contains(&k) || i < 3 + k {
            return Err(bail());
        }
        let args_start = i - 2 - k;
        for arg_ins in &ins[args_start..(i - 2)] {
            match arg_ins.op {
                OpCode::LoadFast => {}
                OpCode::LoadConst
                    if matches!(
                        code.constants.get(arg_ins.arg as usize),
                        Some(Constant::Int(_))
                    ) => {}
                _ => return Err(bail()),
            }
        }
        // An explicit step is only allowed as the constant 1; it is
        // erased so the call effectively becomes `range(start, stop)`.
        let mut pops = k as u8;
        if k == 3 {
            let step_pc = i - 3;
            if !matches!(ins[step_pc].op, OpCode::LoadConst)
                || !matches!(
                    code.constants.get(ins[step_pc].arg as usize),
                    Some(Constant::Int(1))
                )
            {
                return Err(bail());
            }
            plan.nop.insert(step_pc);
            pops = 2;
        }
        let callee = args_start - 1;
        if !matches!(ins[callee].op, OpCode::LoadGlobal) {
            return Err(bail());
        }
        if plan.globals.get(&ins[callee].arg) != Some(&ResolvedGlobal::RangeBuiltin) {
            return Err(bail());
        }
        // No jump may land inside the prefix or on the fused store — the
        // header itself (a JUMP_BACKWARD target) is the only allowed
        // landing point.
        if targets.iter().any(|&t| callee < t && t <= i + 1 && t != i) {
            return Err(bail());
        }
        let name = code.names[ins[callee].arg as usize].clone();
        if !plan.guards.iter().any(|g| g.name == name) {
            plan.guards.push(GlobalGuard {
                name,
                expect: ResolvedGlobal::RangeBuiltin,
            });
        }

        let cur_slot = n_real + plan.n_synth;
        let stop_slot = cur_slot + 1;
        plan.n_synth += 2;
        plan.nop.insert(callee);
        plan.nop.insert(i - 1);
        plan.nop.insert(exit);
        plan.calls.insert(i - 2, (pops, cur_slot, stop_slot));
        plan.headers.insert(i, (cur_slot, stop_slot, var_slot));
        plan.fused_store.insert(i + 1, var_slot);
        plan.loops.push(RangeLoopMeta {
            cur_slot,
            stop_slot,
            live_from: i as u32,
            live_to: exit as u32,
        });
    }

    // Burnable globals: every LOAD_GLOBAL that is not a recognized range
    // callee must resolve to a scalar constant or a callable Python
    // function (RFC 0059 WS3), and needs an identity guard either way.
    for (i, item) in ins.iter().enumerate() {
        if !matches!(item.op, OpCode::LoadGlobal) || plan.nop.contains(&i) {
            continue;
        }
        let resolved = plan.globals[&item.arg];
        match resolved {
            ResolvedGlobal::ConstInt(_)
            | ResolvedGlobal::ConstFloat(_)
            | ResolvedGlobal::ConstBool(_)
            | ResolvedGlobal::PyFunc { .. } => {
                let name = &code.names[item.arg as usize];
                if !plan.guards.iter().any(|g| g.name == *name) {
                    plan.guards.push(GlobalGuard {
                        name: name.clone(),
                        expect: resolved,
                    });
                }
            }
            _ => return Err(JitVerdict::UnsupportedOpcode("LOAD_GLOBAL")),
        }
    }

    Ok(plan)
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
            // A rewritten range loop's header: branches to the body
            // (fallthrough) or the exit (`END_FOR`) when exhausted.
            OpCode::ForIter => {
                let t = forward_target(i, ins.arg);
                if t > n {
                    return Err(JitVerdict::BadJumpTarget);
                }
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
            // succs[0] = body (fallthrough), succs[1] = exit.
            OpCode::ForIter => {
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

/// A `LOAD_GLOBAL`-resolved Python callee riding the *abstract* stack
/// between its load and its `CALL` (RFC 0059 WS3). The object itself
/// never reaches the native stack — the marker only carries what the
/// `CALL` site and the deopt metadata need.
#[derive(Clone, Copy)]
struct CalleeMark {
    token: u32,
    arg_count: u32,
    is_self: bool,
    ret: Option<JitType>,
    /// The erased `LOAD_GLOBAL` pc.
    load_pc: u32,
    /// Interpreter-stack index of the callee object at load time
    /// (emission only; 0 during inference).
    interp_depth: u32,
}

/// One operand-stack entry during analysis, with provenance for the
/// live-in inference (`src` is the slot of an as-yet-untyped load) and
/// an optional callee marker (`ty` is `Unknown` when `callee` is set —
/// the marker is consumed by `CALL`, never by a value op).
#[derive(Clone, Copy)]
struct SE {
    ty: JitType,
    src: Option<u32>,
    callee: Option<CalleeMark>,
}

impl SE {
    fn known(ty: JitType) -> SE {
        SE {
            ty,
            src: None,
            callee: None,
        }
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
    plan: &Plan,
    local_types: &mut [Option<JitType>],
    ret_lane: &mut Option<JitType>,
    changed: &mut bool,
) -> Result<(), JitVerdict> {
    let mut stack: Vec<SE> = Vec::new();
    for i in b.start..(b.end - 1) {
        step_abstract(code, i, &mut stack, plan, local_types, *ret_lane, changed)?;
    }
    // Terminator stack-shape validation.
    let last = b.end - 1;
    let ins = code.instructions[last];
    match ins.op {
        OpCode::ReturnValue => {
            let v = *stack.last().ok_or(JitVerdict::StackUnderflow)?;
            if v.callee.is_some() {
                return Err(JitVerdict::UnsupportedOpcode("CALL (callee escapes)"));
            }
            merge_ret_lane(ret_lane, v.ty, changed);
        }
        OpCode::JumpForward | OpCode::JumpBackward => {
            if !stack.is_empty() {
                return Err(JitVerdict::NonEmptyBoundaryStack);
            }
        }
        // A rewritten range header operates purely on its synthetic
        // slots; the operand stack must be empty like any other jump.
        OpCode::ForIter => {
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
            step_abstract(
                code,
                last,
                &mut stack,
                plan,
                local_types,
                *ret_lane,
                changed,
            )?;
            if !stack.is_empty() {
                return Err(JitVerdict::NonEmptyBoundaryStack);
            }
        }
    }
    Ok(())
}

/// Merge one `return` site's lane into the function-wide return lane
/// (RFC 0059 WS3). Two *concrete* conflicting lanes poison the lane to
/// `Unknown` (sticky — the function still compiles, but `CallPy` sites
/// targeting it bail). An `Unknown` return value contributes nothing:
/// either a later iteration types it, or emission bails on the
/// untypable value anyway.
fn merge_ret_lane(ret_lane: &mut Option<JitType>, ty: JitType, changed: &mut bool) {
    if !ty.is_representable() {
        return;
    }
    match *ret_lane {
        None => {
            *ret_lane = Some(ty);
            *changed = true;
        }
        Some(existing) if existing == ty || existing == JitType::Unknown => {}
        Some(_) => {
            *ret_lane = Some(JitType::Unknown);
            *changed = true;
        }
    }
}

/// Abstract-execute one non-terminator instruction, updating the type
/// stack and (via inference) `local_types`.
fn step_abstract(
    code: &CodeObject,
    i: usize,
    stack: &mut Vec<SE>,
    plan: &Plan,
    local_types: &mut [Option<JitType>],
    ret_lane: Option<JitType>,
    changed: &mut bool,
) -> Result<(), JitVerdict> {
    let ins = code.instructions[i];
    // RFC 0058 WS4 — rewritten range-loop pcs.
    if plan.nop.contains(&i) {
        return Ok(());
    }
    if let Some(&(pops, cur, stop)) = plan.calls.get(&i) {
        for _ in 0..pops {
            let v = stack.pop().ok_or(JitVerdict::StackUnderflow)?;
            if v.ty.is_representable() {
                if !v.ty.is_integral() {
                    return Err(JitVerdict::TypeUnknown);
                }
            } else if let Some(slot) = v.src {
                // A live-in feeding a range bound must be an int.
                set_local(local_types, slot, JitType::Int, changed)?;
            }
        }
        set_local(local_types, cur, JitType::Int, changed)?;
        set_local(local_types, stop, JitType::Int, changed)?;
        return Ok(());
    }
    if let Some(&var) = plan.fused_store.get(&i) {
        // Performed by the `ForRange` terminator; no stack effect here.
        set_local(local_types, var, JitType::Int, changed)?;
        return Ok(());
    }
    match ins.op {
        OpCode::Nop | OpCode::Resume => {}
        OpCode::LoadGlobal => {
            let ty = match plan.globals.get(&ins.arg) {
                Some(ResolvedGlobal::ConstInt(_)) => JitType::Int,
                Some(ResolvedGlobal::ConstFloat(_)) => JitType::Float,
                Some(ResolvedGlobal::ConstBool(_)) => JitType::Bool,
                Some(&ResolvedGlobal::PyFunc {
                    token,
                    arg_count,
                    is_self,
                    ret,
                }) => {
                    // RFC 0059 WS3: the callee rides the abstract stack
                    // as a marker until its CALL consumes it.
                    stack.push(SE {
                        ty: JitType::Unknown,
                        src: None,
                        callee: Some(CalleeMark {
                            token,
                            arg_count,
                            is_self,
                            ret,
                            load_pc: i as u32,
                            interp_depth: 0,
                        }),
                    });
                    return Ok(());
                }
                _ => return Err(JitVerdict::UnsupportedOpcode("LOAD_GLOBAL")),
            };
            stack.push(SE::known(ty));
        }
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
                    callee: None,
                }),
            }
        }
        OpCode::StoreFast => {
            let v = stack.pop().ok_or(JitVerdict::StackUnderflow)?;
            if v.callee.is_some() {
                return Err(JitVerdict::UnsupportedOpcode("CALL (callee escapes)"));
            }
            if v.ty.is_representable() {
                set_local(local_types, ins.arg, v.ty, changed)?;
            }
        }
        OpCode::BinaryOp => {
            let kind = bin_kind(ins.arg)?;
            let b = stack.pop().ok_or(JitVerdict::StackUnderflow)?;
            let a = stack.pop().ok_or(JitVerdict::StackUnderflow)?;
            if a.callee.is_some() || b.callee.is_some() {
                return Err(JitVerdict::UnsupportedOpcode("CALL (callee escapes)"));
            }
            let (a, b) = resolve_pair(a, b, local_types, changed);
            let res = bin_result_type(kind, a.ty, b.ty)?;
            stack.push(SE::known(res));
        }
        OpCode::CompareOp => {
            let _ = cmp_kind(ins.arg)?;
            let b = stack.pop().ok_or(JitVerdict::StackUnderflow)?;
            let a = stack.pop().ok_or(JitVerdict::StackUnderflow)?;
            if a.callee.is_some() || b.callee.is_some() {
                return Err(JitVerdict::UnsupportedOpcode("CALL (callee escapes)"));
            }
            let (a, b) = resolve_pair(a, b, local_types, changed);
            cmp_check(a.ty, b.ty)?;
            stack.push(SE::known(JitType::Bool));
        }
        OpCode::UnaryOp => {
            let kind = unary_kind(ins.arg)?;
            let a = stack.pop().ok_or(JitVerdict::StackUnderflow)?;
            if a.callee.is_some() {
                return Err(JitVerdict::UnsupportedOpcode("CALL (callee escapes)"));
            }
            let res = unary_result_type(kind, a.ty)?;
            stack.push(SE::known(res));
        }
        // RFC 0059 WS3 — a Python-to-Python call: the marker beneath the
        // arguments names the callee. Nested calls compose (an inner
        // call's marker sits above the outer one and is consumed first).
        OpCode::Call => {
            let argc = ins.arg as usize;
            if stack.len() < argc + 1 {
                return Err(JitVerdict::StackUnderflow);
            }
            for _ in 0..argc {
                let v = stack.pop().ok_or(JitVerdict::StackUnderflow)?;
                if v.callee.is_some() {
                    return Err(JitVerdict::UnsupportedOpcode("CALL (callee as argument)"));
                }
            }
            let f = stack.pop().ok_or(JitVerdict::StackUnderflow)?;
            let Some(mark) = f.callee else {
                return Err(JitVerdict::UnsupportedOpcode("CALL"));
            };
            // Exact positional arity only: default-filled or mis-arity
            // calls would chronically deopt, so they disqualify instead.
            if mark.arg_count as usize != argc {
                return Err(JitVerdict::UnsupportedOpcode("CALL (arity)"));
            }
            let ret = if mark.is_self { ret_lane } else { mark.ret };
            stack.push(SE::known(ret.unwrap_or(JitType::Unknown)));
        }
        OpCode::PopTop => {
            // `break` inside a rewritten range loop pops the *iterator*,
            // which the rewrite never pushed — erase the pop. (When the
            // rewritten stack is empty inside a loop span, the
            // interpreter's stack holds exactly the live iterators.)
            if stack.is_empty() && plan.in_loop_span(i) {
                return Ok(());
            }
            let v = stack.pop().ok_or(JitVerdict::StackUnderflow)?;
            if v.callee.is_some() {
                return Err(JitVerdict::UnsupportedOpcode("CALL (callee escapes)"));
            }
        }
        OpCode::CopyTop => {
            let v = *stack.last().ok_or(JitVerdict::StackUnderflow)?;
            if v.callee.is_some() {
                return Err(JitVerdict::UnsupportedOpcode("CALL (callee escapes)"));
            }
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
            // A marker's recorded interp-stack position must stay fixed
            // between load and call; reordering it disqualifies.
            if stack[len - 1].callee.is_some() || stack[len - 2].callee.is_some() {
                return Err(JitVerdict::UnsupportedOpcode("CALL (callee escapes)"));
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

/// Result lane of a binary arithmetic op, given operand lanes. An
/// `Unknown` operand yields `Unknown` — a later inference iteration may
/// resolve it, and emission bails if it never does.
fn bin_result_type(kind: ArithKind, a: JitType, b: JitType) -> Result<JitType, JitVerdict> {
    if !a.is_representable() || !b.is_representable() {
        return Ok(JitType::Unknown);
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
    } else if a == JitType::Float || b == JitType::Float {
        // Float∘float, or mixed integral/float (RFC 0058 WS4): the
        // integral operand is promoted with the same `as f64` cast the
        // interpreter applies, so only the float-lane op set is legal.
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

/// Validate comparison operand lanes. Same-lane always works; mixed
/// integral/float works via a *guarded* promotion (the interpreter
/// compares exactly, so the JIT deopts when the int exceeds ±2^53).
fn cmp_check(a: JitType, b: JitType) -> Result<(), JitVerdict> {
    if !a.is_representable() || !b.is_representable() {
        return Ok(());
    }
    if (a.is_integral() || a == JitType::Float) && (b.is_integral() || b == JitType::Float) {
        Ok(())
    } else {
        Err(JitVerdict::MixedArithTypes)
    }
}

/// Result lane of a unary op.
fn unary_result_type(kind: UnaryKind, a: JitType) -> Result<JitType, JitVerdict> {
    if !a.is_representable() {
        return Ok(JitType::Unknown);
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

/// One emission-stack entry: a native lane, or an open callee marker
/// (present on the *interpreter's* stack but never the native one).
#[derive(Clone, Copy)]
struct ESlot {
    ty: JitType,
    callee: Option<CalleeMark>,
}

impl ESlot {
    fn val(ty: JitType) -> ESlot {
        ESlot { ty, callee: None }
    }
}

/// Pop an emission-stack value that must be a plain lane (not a callee
/// marker).
fn pop_val(stack: &mut Vec<ESlot>) -> Result<JitType, JitVerdict> {
    let s = stack.pop().ok_or(JitVerdict::StackUnderflow)?;
    if s.callee.is_some() {
        return Err(JitVerdict::UnsupportedOpcode("CALL (callee escapes)"));
    }
    Ok(s.ty)
}

/// Mutable side outputs of the emission pass shared across blocks.
struct EmitOut {
    max_stack: u32,
    callee_spans: Vec<CalleeSpanMeta>,
    max_call_args: u32,
}

/// Emit the typed IR for one block, with all local types now known.
fn emit_block(
    code: &CodeObject,
    b: &RawBlock,
    plan: &Plan,
    local_types: &[Option<JitType>],
    ret_lane: Option<JitType>,
    compact: &HashMap<usize, BlockId>,
    out: &mut EmitOut,
) -> Result<TBlock, JitVerdict> {
    let mut stack: Vec<ESlot> = Vec::new();
    let mut stmts: Vec<TStmt> = Vec::new();

    for i in b.start..(b.end - 1) {
        emit_instr(
            code,
            i,
            plan,
            local_types,
            ret_lane,
            &mut stack,
            &mut stmts,
            out,
        )?;
    }

    let last = b.end - 1;
    let ins = code.instructions[last];
    let term = match ins.op {
        OpCode::ReturnValue => {
            // Lowering pops the return value off its own type stack at
            // the `Return` terminator; no statement is emitted here.
            let top = stack.last().ok_or(JitVerdict::StackUnderflow)?;
            if top.callee.is_some() {
                return Err(JitVerdict::UnsupportedOpcode("CALL (callee escapes)"));
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
        OpCode::ForIter => {
            let &(cur_slot, stop_slot, var_slot) = plan
                .headers
                .get(&last)
                .ok_or(JitVerdict::UnsupportedOpcode("FOR_ITER (unplanned)"))?;
            TTerm::ForRange {
                cur_slot,
                stop_slot,
                var_slot,
                body: compact[&block_succ(b, 0)],
                exit: compact[&block_succ(b, 1)],
            }
        }
        _ => {
            emit_instr(
                code,
                last,
                plan,
                local_types,
                ret_lane,
                &mut stack,
                &mut stmts,
                out,
            )?;
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
#[allow(clippy::too_many_arguments)]
fn emit_instr(
    code: &CodeObject,
    i: usize,
    plan: &Plan,
    local_types: &[Option<JitType>],
    ret_lane: Option<JitType>,
    stack: &mut Vec<ESlot>,
    stmts: &mut Vec<TStmt>,
    out: &mut EmitOut,
) -> Result<(), JitVerdict> {
    let ins = code.instructions[i];
    let pc = i as u32;
    let EmitOut {
        max_stack,
        callee_spans,
        max_call_args,
    } = out;
    // Note: `max_stack` counts markers too — a harmless overestimate of
    // the native spill depth (markers are never spilled).
    let mut push =
        |op: TOp, ty: Option<JitType>, stack: &mut Vec<ESlot>, stmts: &mut Vec<TStmt>| {
            stmts.push(TStmt { pc, op });
            if let Some(t) = ty {
                stack.push(ESlot::val(t));
            }
            *max_stack = (*max_stack).max(stack.len() as u32);
        };
    // RFC 0058 WS4 — rewritten range-loop pcs.
    if plan.nop.contains(&i) || plan.fused_store.contains_key(&i) {
        return Ok(());
    }
    if let Some(&(pops, cur_slot, stop_slot)) = plan.calls.get(&i) {
        // Stack is [.., start?, stop]; store the bounds into the
        // synthetic slots (one arg seeds `cur` with 0).
        stack.pop().ok_or(JitVerdict::StackUnderflow)?;
        push(TOp::StoreLocal(stop_slot), None, stack, stmts);
        if pops == 2 {
            stack.pop().ok_or(JitVerdict::StackUnderflow)?;
        } else {
            push(TOp::PushConstInt(0), Some(JitType::Int), stack, stmts);
            stack.pop();
        }
        push(TOp::StoreLocal(cur_slot), None, stack, stmts);
        return Ok(());
    }
    // `break` inside a rewritten loop: erase the phantom iterator pop.
    if matches!(ins.op, OpCode::PopTop) && stack.is_empty() && plan.in_loop_span(i) {
        return Ok(());
    }
    match ins.op {
        OpCode::Nop | OpCode::Resume => {}
        OpCode::LoadGlobal => {
            let (op, ty) = match plan.globals.get(&ins.arg) {
                Some(&ResolvedGlobal::ConstInt(v)) => (TOp::PushConstInt(v), JitType::Int),
                Some(&ResolvedGlobal::ConstFloat(bits)) => {
                    (TOp::PushConstFloat(bits), JitType::Float)
                }
                Some(&ResolvedGlobal::ConstBool(v)) => (TOp::PushConstBool(v), JitType::Bool),
                Some(&ResolvedGlobal::PyFunc {
                    token,
                    arg_count,
                    is_self,
                    ret,
                }) => {
                    // RFC 0059 WS3: the callee never reaches the native
                    // stack — push a marker and record where the
                    // *interpreter's* stack would hold the object (below
                    // any values already pushed, above the live range
                    // iterators of enclosing rewritten loops).
                    let n_iters = plan
                        .loops
                        .iter()
                        .filter(|l| (l.live_from as usize) <= i && i < l.live_to as usize)
                        .count() as u32;
                    stack.push(ESlot {
                        ty: JitType::Unknown,
                        callee: Some(CalleeMark {
                            token,
                            arg_count,
                            is_self,
                            ret,
                            load_pc: pc,
                            interp_depth: n_iters + stack.len() as u32,
                        }),
                    });
                    return Ok(());
                }
                _ => return Err(JitVerdict::UnsupportedOpcode("LOAD_GLOBAL")),
            };
            push(op, Some(ty), stack, stmts);
        }
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
            pop_val(stack)?;
            push(TOp::StoreLocal(ins.arg), None, stack, stmts);
        }
        OpCode::BinaryOp => {
            let kind = bin_kind(ins.arg)?;
            let b = pop_val(stack)?;
            let a = pop_val(stack)?;
            if (a.is_integral() && b == JitType::Float) || (a == JitType::Float && b.is_integral())
            {
                // Mixed integral/float (RFC 0058 WS4): promote the
                // integral operand exactly like the interpreter's
                // `as f64` cast, then run the float-lane op. Only the
                // float-supported op set is legal.
                if !matches!(
                    kind,
                    ArithKind::Add | ArithKind::Sub | ArithKind::Mul | ArithKind::TrueDiv
                ) {
                    return Err(JitVerdict::UnsupportedOpcode("mixed floordiv/mod/bitop"));
                }
                // Both operands are conceptually back on the stack for
                // the promotion op (they only left the *model*).
                stack.push(ESlot::val(a));
                stack.push(ESlot::val(b));
                let promote = if b == JitType::Float {
                    TOp::IntToFloatSecond { guarded: false }
                } else {
                    TOp::IntToFloatTos { guarded: false }
                };
                push(promote, None, stack, stmts);
                stack.pop();
                stack.pop();
                push(TOp::FloatArith(kind), Some(JitType::Float), stack, stmts);
            } else {
                let (op, ty) = lower_bin(kind, a, b)?;
                push(op, Some(ty), stack, stmts);
            }
        }
        OpCode::CompareOp => {
            let kind = cmp_kind(ins.arg)?;
            let b = pop_val(stack)?;
            let a = pop_val(stack)?;
            if a.is_integral() && b.is_integral() {
                push(TOp::IntCmp(kind), Some(JitType::Bool), stack, stmts);
            } else if a == JitType::Float && b == JitType::Float {
                push(TOp::FloatCmp(kind), Some(JitType::Bool), stack, stmts);
            } else if (a.is_integral() && b == JitType::Float)
                || (a == JitType::Float && b.is_integral())
            {
                // Mixed comparison is mathematically exact in the
                // interpreter, so the promotion is *guarded*: outside
                // ±2^53 (where f64 stops being exact) it deopts.
                stack.push(ESlot::val(a));
                stack.push(ESlot::val(b));
                let promote = if b == JitType::Float {
                    TOp::IntToFloatSecond { guarded: true }
                } else {
                    TOp::IntToFloatTos { guarded: true }
                };
                push(promote, None, stack, stmts);
                stack.pop();
                stack.pop();
                push(TOp::FloatCmp(kind), Some(JitType::Bool), stack, stmts);
            } else {
                return Err(JitVerdict::MixedArithTypes);
            }
        }
        OpCode::UnaryOp => {
            let kind = unary_kind(ins.arg)?;
            let a = pop_val(stack)?;
            match (kind, a) {
                (UnaryKind::Pos, JitType::Int | JitType::Float) => {
                    // Identity; re-push same lane, emit nothing.
                    stack.push(ESlot::val(a));
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
        // RFC 0059 WS3 — Python-to-Python call: pop the scalar args and
        // the callee marker, close the deopt span, and emit `CallPy`.
        OpCode::Call => {
            let argc = ins.arg as usize;
            if stack.len() < argc + 1 {
                return Err(JitVerdict::StackUnderflow);
            }
            for _ in 0..argc {
                let ty = pop_val(stack)?;
                if !ty.is_representable() {
                    return Err(JitVerdict::TypeUnknown);
                }
            }
            let f = stack.pop().ok_or(JitVerdict::StackUnderflow)?;
            let Some(mark) = f.callee else {
                return Err(JitVerdict::UnsupportedOpcode("CALL"));
            };
            if mark.arg_count as usize != argc {
                return Err(JitVerdict::UnsupportedOpcode("CALL (arity)"));
            }
            let ret = match if mark.is_self { ret_lane } else { mark.ret } {
                Some(t) if t.is_representable() => t,
                _ => return Err(JitVerdict::TypeUnknown),
            };
            callee_spans.push(CalleeSpanMeta {
                token: mark.token,
                live_from: mark.load_pc,
                live_to: pc,
                interp_depth: mark.interp_depth,
            });
            *max_call_args = (*max_call_args).max(argc as u32);
            push(
                TOp::CallPy {
                    token: mark.token,
                    argc: argc as u8,
                    ret,
                },
                Some(ret),
                stack,
                stmts,
            );
        }
        OpCode::PopTop => {
            pop_val(stack)?;
            push(TOp::Pop, None, stack, stmts);
        }
        OpCode::CopyTop => {
            let t = *stack.last().ok_or(JitVerdict::StackUnderflow)?;
            if t.callee.is_some() {
                return Err(JitVerdict::UnsupportedOpcode("CALL (callee escapes)"));
            }
            push(TOp::Dup, Some(t.ty), stack, stmts);
        }
        OpCode::Swap => {
            if ins.arg != 2 {
                return Err(JitVerdict::UnsupportedOpcode("SWAP n!=2"));
            }
            let len = stack.len();
            if len < 2 {
                return Err(JitVerdict::StackUnderflow);
            }
            if stack[len - 1].callee.is_some() || stack[len - 2].callee.is_some() {
                return Err(JitVerdict::UnsupportedOpcode("CALL (callee escapes)"));
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

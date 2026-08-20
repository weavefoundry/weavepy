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
    ArithKind, AttrSiteMeta, BlockId, CalleeSpanMeta, CmpKind, GlobalGuard, MethodSpanMeta,
    OsrEntry, RangeLoopMeta, ResolvedGlobal, TBlock, TFunc, TOp, TStmt, TTerm,
};
use crate::value::JitType;

/// The embedder's shape probes (RFC 0061/0065 WS5), bundled so the
/// inference and emission passes share one source of truth.
pub(crate) struct Probes<'a> {
    /// Element lane of a local currently holding a homogeneous `int`/
    /// `float` list (`Some(Unknown)` = an *empty* list: definitely a
    /// list, but with no lane evidence — only `append` can pin it).
    pub list: &'a mut dyn FnMut(u32) -> Option<JitType>,
    /// `(slot, name, store)` → the scalar value lane of an eligible
    /// instance-dict attribute on the local currently in `slot`
    /// (RFC 0065 WS5). Eligibility mirrors the tier-1 inline-cache
    /// predicate: no `__getattr__`/`__getattribute__`, no shadowing
    /// data descriptor, name present in the instance dict.
    pub attr: &'a mut dyn FnMut(u32, &str, bool) -> Option<JitType>,
}

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
///
/// Convenience wrapper over [`analyze_with_probe`] with no pinned-list
/// probing (RFC 0061 WS5) — subscripted locals disqualify the frame.
pub fn analyze(
    code: &CodeObject,
    resolve: &mut dyn FnMut(&str) -> ResolvedGlobal,
) -> Result<TFunc, JitVerdict> {
    analyze_with_probe(code, resolve, &mut |_| None)
}

/// [`analyze`] with a pinned-list lane probe (RFC 0061 WS5). When a
/// local slot is subscripted before any other typing evidence exists,
/// `probe_list` reports the slot's *observed* shape in the requesting
/// activation — `Some(Int)`/`Some(Float)` for a homogeneous `int`/
/// `float` list, `None` otherwise. A probed lane is only a prediction:
/// the embedder re-validates it as an entry guard on every native
/// entry, and the list helpers re-check shape per access.
pub fn analyze_with_probe(
    code: &CodeObject,
    resolve: &mut dyn FnMut(&str) -> ResolvedGlobal,
    probe_list: &mut dyn FnMut(u32) -> Option<JitType>,
) -> Result<TFunc, JitVerdict> {
    analyze_with_probes(code, resolve, probe_list, &mut |_, _, _| None)
}

/// [`analyze_with_probe`] with an attribute-site probe (RFC 0065 WS5).
/// `probe_attr(slot, name, store)` reports the observed scalar value
/// lane of an instance-dict attribute on the local currently in
/// `slot`, or `None` when the receiver shape is ineligible. Like the
/// list probe it is only a prediction: the embedder snapshots a guard
/// fingerprint per site and its access helpers re-validate per access.
pub fn analyze_with_probes(
    code: &CodeObject,
    resolve: &mut dyn FnMut(&str) -> ResolvedGlobal,
    probe_list: &mut dyn FnMut(u32) -> Option<JitType>,
    probe_attr: &mut dyn FnMut(u32, &str, bool) -> Option<JitType>,
) -> Result<TFunc, JitVerdict> {
    let mut probes = Probes {
        list: probe_list,
        attr: probe_attr,
    };
    analyze_impl(code, resolve, &mut probes)
}

fn analyze_impl(
    code: &CodeObject,
    resolve: &mut dyn FnMut(&str) -> ResolvedGlobal,
    probes: &mut Probes<'_>,
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
                probes,
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
        len_spans: Vec::new(),
        method_spans: Vec::new(),
        attr_sites: Vec::new(),
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
            probes,
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
        len_spans: out.len_spans,
        method_spans: out.method_spans,
        attr_sites: out.attr_sites,
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
        // PUSH_NULL, LOAD_GLOBAL <range>.
        if i < 2
            || !matches!(ins[i - 1].op, OpCode::GetIter)
            || !matches!(ins[i - 2].op, OpCode::Call)
        {
            return Err(bail());
        }
        let k = ins[i - 2].arg as usize;
        if !(1..=3).contains(&k) || i < 4 + k {
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
        let push_null = args_start - 1;
        if !matches!(ins[push_null].op, OpCode::PushNull) {
            return Err(bail());
        }
        let callee = args_start - 2;
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
        plan.nop.insert(push_null);
        plan.nop.insert(i - 1);
        plan.nop.insert(exit);
        // The POP_TOP paired with END_FOR (CPython 3.13 loop-exit shape)
        // is equally dead in the compiled trace.
        if exit + 1 < n && matches!(ins[exit + 1].op, OpCode::PopTop) {
            plan.nop.insert(exit + 1);
        }
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
            | ResolvedGlobal::LenBuiltin
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

/// What a callee marker stands for (RFC 0065 WS5): a burned-in Python
/// function, or the erased `len` builtin (lowered to `ListLen`, never
/// a real call).
#[derive(Clone, Copy, PartialEq, Eq)]
enum MarkKind {
    Py,
    Len,
}

/// A `LOAD_GLOBAL`-resolved Python callee riding the *abstract* stack
/// between its load and its `CALL` (RFC 0059 WS3). The object itself
/// never reaches the native stack — the marker only carries what the
/// `CALL` site and the deopt metadata need.
#[derive(Clone, Copy)]
struct CalleeMark {
    kind: MarkKind,
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
///
/// RFC 0065 WS5 adds two more flavours: `recv` marks a pinned-list
/// receiver of an erased `.append` load (the native value stays the
/// pin; the interpreter holds the bound method), and `poison` marks
/// `append`'s `None` result, which exists only on the interpreter
/// stack and may only be consumed by an immediate `POP_TOP`.
#[derive(Clone, Copy)]
struct SE {
    ty: JitType,
    src: Option<u32>,
    callee: Option<CalleeMark>,
    /// `Some(load_pc)` when this is a `.append` receiver (the
    /// `LOAD_ATTR`'s pc, for the method-span deopt metadata).
    recv: Option<u32>,
    poison: bool,
    /// RFC 0068 — a `PUSH_NULL` self-or-null marker (`Unbound` on the
    /// interpreter stack, never native), consumed by its `CALL`.
    null: bool,
}

impl SE {
    fn known(ty: JitType) -> SE {
        SE {
            ty,
            src: None,
            callee: None,
            recv: None,
            poison: false,
            null: false,
        }
    }

    /// `true` when a plain value operation may consume this entry.
    fn is_plain(&self) -> bool {
        self.callee.is_none() && self.recv.is_none() && !self.poison && !self.null
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
    probes: &mut Probes<'_>,
) -> Result<(), JitVerdict> {
    let mut stack: Vec<SE> = Vec::new();
    for i in b.start..(b.end - 1) {
        step_abstract(
            code,
            i,
            &mut stack,
            plan,
            local_types,
            *ret_lane,
            changed,
            probes,
        )?;
    }
    // Terminator stack-shape validation.
    let last = b.end - 1;
    let ins = code.instructions[last];
    match ins.op {
        OpCode::ReturnValue => {
            let v = *stack.last().ok_or(JitVerdict::StackUnderflow)?;
            if !v.is_plain() {
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
            if !c.is_plain() {
                return Err(JitVerdict::UnsupportedOpcode("CALL (callee escapes)"));
            }
            // RFC 0061/0065 WS5 — a pinned value's truth (list length,
            // instance `__bool__`) is not expressible on the pin-index
            // machine value.
            if c.ty.is_pinned() {
                return Err(JitVerdict::UnsupportedOpcode("truth test on pinned value"));
            }
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
                probes,
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
#[allow(clippy::too_many_arguments)]
fn step_abstract(
    code: &CodeObject,
    i: usize,
    stack: &mut Vec<SE>,
    plan: &Plan,
    local_types: &mut [Option<JitType>],
    ret_lane: Option<JitType>,
    changed: &mut bool,
    probes: &mut Probes<'_>,
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
                        callee: Some(CalleeMark {
                            kind: MarkKind::Py,
                            token,
                            arg_count,
                            is_self,
                            ret,
                            load_pc: i as u32,
                            interp_depth: 0,
                        }),
                        ..SE::known(JitType::Unknown)
                    });
                    return Ok(());
                }
                // RFC 0065 WS5: `len` rides the abstract stack the same
                // way; its CALL lowers to `ListLen`, never a real call.
                Some(ResolvedGlobal::LenBuiltin) => {
                    stack.push(SE {
                        callee: Some(CalleeMark {
                            kind: MarkKind::Len,
                            token: 0,
                            arg_count: 1,
                            is_self: false,
                            ret: Some(JitType::Int),
                            load_pc: i as u32,
                            interp_depth: 0,
                        }),
                        ..SE::known(JitType::Unknown)
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
            // `src` is kept even for typed loads: RFC 0065 WS5 attribute
            // sites need the slot the receiver was loaded from. All
            // other consumers only read `src` when `ty` is `Unknown`.
            let slot = ins.arg as usize;
            let ty = local_types
                .get(slot)
                .copied()
                .flatten()
                .unwrap_or(JitType::Unknown);
            stack.push(SE {
                ty,
                src: Some(ins.arg),
                ..SE::known(JitType::Unknown)
            });
        }
        OpCode::StoreFast => {
            let v = stack.pop().ok_or(JitVerdict::StackUnderflow)?;
            if !v.is_plain() {
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
            if !a.is_plain() || !b.is_plain() {
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
            if !a.is_plain() || !b.is_plain() {
                return Err(JitVerdict::UnsupportedOpcode("CALL (callee escapes)"));
            }
            let (a, b) = resolve_pair(a, b, local_types, changed);
            cmp_check(a.ty, b.ty)?;
            stack.push(SE::known(JitType::Bool));
        }
        OpCode::UnaryOp => {
            let kind = unary_kind(ins.arg)?;
            let a = stack.pop().ok_or(JitVerdict::StackUnderflow)?;
            if !a.is_plain() {
                return Err(JitVerdict::UnsupportedOpcode("CALL (callee escapes)"));
            }
            let res = unary_result_type(kind, a.ty)?;
            stack.push(SE::known(res));
        }
        // RFC 0065 WS5 — attribute load: either the erased `.append`
        // method load on a pinned list (the receiver stays on the
        // abstract stack, re-marked), or a scalar attribute read on a
        // pinned instance receiver.
        OpCode::LoadAttr => {
            let name = code
                .names
                .get(ins.arg as usize)
                .ok_or(JitVerdict::UnsupportedOpcode("LOAD_ATTR bad name"))?
                .as_str();
            let recv = stack.pop().ok_or(JitVerdict::StackUnderflow)?;
            if !recv.is_plain() {
                return Err(JitVerdict::UnsupportedOpcode("CALL (callee escapes)"));
            }
            infer_load_attr(name, recv, i, stack, local_types, changed, probes)?;
        }
        // RFC 0068 — the self-or-null slot of the CPython calling
        // convention: `Unbound` on the interpreter stack, never native.
        OpCode::PushNull => {
            stack.push(SE {
                null: true,
                ..SE::known(JitType::Unknown)
            });
        }
        // RFC 0068 — method-form attribute load: same load as
        // `LOAD_ATTR`, plus the implicit self-or-null marker on top.
        OpCode::LoadMethodAttr => {
            let name = code
                .names
                .get(ins.arg as usize)
                .ok_or(JitVerdict::UnsupportedOpcode("LOAD_ATTR bad name"))?
                .as_str();
            let recv = stack.pop().ok_or(JitVerdict::StackUnderflow)?;
            if !recv.is_plain() {
                return Err(JitVerdict::UnsupportedOpcode("CALL (callee escapes)"));
            }
            infer_load_attr(name, recv, i, stack, local_types, changed, probes)?;
            stack.push(SE {
                null: true,
                ..SE::known(JitType::Unknown)
            });
        }
        // RFC 0065 WS5 — scalar attribute write on a pinned instance
        // receiver. Stack is `[.., value, receiver]`.
        OpCode::StoreAttr => {
            let name = code
                .names
                .get(ins.arg as usize)
                .ok_or(JitVerdict::UnsupportedOpcode("STORE_ATTR bad name"))?
                .as_str();
            let recv = stack.pop().ok_or(JitVerdict::StackUnderflow)?;
            let val = stack.pop().ok_or(JitVerdict::StackUnderflow)?;
            if !recv.is_plain() || !val.is_plain() {
                return Err(JitVerdict::UnsupportedOpcode("CALL (callee escapes)"));
            }
            let Some(slot) = obj_recv_slot(&recv) else {
                if !recv.ty.is_representable() {
                    // Transient — a later iteration may type it.
                    return Ok(());
                }
                return Err(JitVerdict::UnsupportedOpcode("STORE_ATTR receiver"));
            };
            let lane = (probes.attr)(slot, name, true)
                .ok_or(JitVerdict::UnsupportedOpcode("STORE_ATTR shape"))?;
            set_local(local_types, slot, JitType::Obj, changed)?;
            if val.ty.is_representable() {
                if val.ty != lane {
                    return Err(JitVerdict::UnsupportedOpcode("STORE_ATTR (value lane)"));
                }
            } else if let Some(vs) = val.src {
                set_local(local_types, vs, lane, changed)?;
            }
        }
        // RFC 0059 WS3 — a Python-to-Python call: the marker beneath the
        // arguments names the callee. Nested calls compose (an inner
        // call's marker sits above the outer one and is consumed first).
        // RFC 0065 WS5 adds the `len(list)` and `list.append(v)` shapes.
        OpCode::Call => {
            let argc = ins.arg as usize;
            if stack.len() < argc + 2 {
                return Err(JitVerdict::StackUnderflow);
            }
            let mut args: Vec<SE> = Vec::with_capacity(argc);
            for _ in 0..argc {
                let v = stack.pop().ok_or(JitVerdict::StackUnderflow)?;
                if !v.is_plain() {
                    return Err(JitVerdict::UnsupportedOpcode("CALL (callee as argument)"));
                }
                args.push(v);
            }
            // RFC 0068 — the self-or-null slot below the arguments: only
            // the NULL form is supported (a bound self would mean a
            // method call shape the JIT doesn't model).
            let slot = stack.pop().ok_or(JitVerdict::StackUnderflow)?;
            if !slot.null {
                return Err(JitVerdict::UnsupportedOpcode("CALL (self slot)"));
            }
            let f = stack.pop().ok_or(JitVerdict::StackUnderflow)?;
            if f.recv.is_some() {
                // `x.append(v)` — the receiver rode the stack re-marked.
                if argc != 1 {
                    return Err(JitVerdict::UnsupportedOpcode("append (arity)"));
                }
                infer_append(&f, &args[0], local_types, changed)?;
                stack.push(SE {
                    poison: true,
                    ..SE::known(JitType::Unknown)
                });
                return Ok(());
            }
            let Some(mark) = f.callee else {
                return Err(JitVerdict::UnsupportedOpcode("CALL"));
            };
            // Exact positional arity only: default-filled or mis-arity
            // calls would chronically deopt, so they disqualify instead.
            if mark.arg_count as usize != argc {
                return Err(JitVerdict::UnsupportedOpcode("CALL (arity)"));
            }
            if mark.kind == MarkKind::Len {
                // `len(x)` on a pinned list → an `int`, no real call.
                let arg = &args[0];
                if arg.ty.is_list() {
                    // fine
                } else if !arg.ty.is_representable() {
                    if let Some(slot) = arg.src {
                        let elem = (probes.list)(slot)
                            .ok_or(JitVerdict::UnsupportedOpcode("len (argument shape)"))?;
                        let lty = JitType::list_of(elem)
                            .ok_or(JitVerdict::UnsupportedOpcode("len (elem lane)"))?;
                        set_local(local_types, slot, lty, changed)?;
                    }
                    // No src: transient — tolerate for this iteration.
                } else {
                    return Err(JitVerdict::UnsupportedOpcode("len (argument lane)"));
                }
                stack.push(SE::known(JitType::Int));
                return Ok(());
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
            // RFC 0065 WS5 — `append`'s `None` result exists only on
            // the interpreter stack; this pop consumes it silently.
            if v.poison {
                return Ok(());
            }
            if !v.is_plain() {
                return Err(JitVerdict::UnsupportedOpcode("CALL (callee escapes)"));
            }
        }
        OpCode::CopyTop => {
            let v = *stack.last().ok_or(JitVerdict::StackUnderflow)?;
            if !v.is_plain() {
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
            if !stack[len - 1].is_plain() || !stack[len - 2].is_plain() {
                return Err(JitVerdict::UnsupportedOpcode("CALL (callee escapes)"));
            }
            stack.swap(len - 1, len - 2);
        }
        // RFC 0061 WS5 — pinned-list element read. The container must
        // be (or probe as) a homogeneous int/float list local; the
        // index must be an `int`.
        OpCode::BinarySubscr => {
            let idx = stack.pop().ok_or(JitVerdict::StackUnderflow)?;
            let cont = stack.pop().ok_or(JitVerdict::StackUnderflow)?;
            if !idx.is_plain() || !cont.is_plain() {
                return Err(JitVerdict::UnsupportedOpcode("CALL (callee escapes)"));
            }
            check_subscr_index(&idx, local_types, changed)?;
            let elem = resolve_list_container(&cont, local_types, changed, probes)?;
            stack.push(match elem {
                Some(l) => SE::known(l),
                None => SE::known(JitType::Unknown),
            });
        }
        // RFC 0061 WS5 — pinned-list element write. The stored value's
        // lane must equal the pinned element lane exactly (a `bool`
        // into an int list, say, would change list shape).
        OpCode::StoreSubscr => {
            let idx = stack.pop().ok_or(JitVerdict::StackUnderflow)?;
            let cont = stack.pop().ok_or(JitVerdict::StackUnderflow)?;
            let val = stack.pop().ok_or(JitVerdict::StackUnderflow)?;
            if !idx.is_plain() || !cont.is_plain() || !val.is_plain() {
                return Err(JitVerdict::UnsupportedOpcode("CALL (callee escapes)"));
            }
            check_subscr_index(&idx, local_types, changed)?;
            let elem = resolve_list_container(&cont, local_types, changed, probes)?;
            if let Some(el) = elem {
                if val.ty.is_representable() {
                    if val.ty != el {
                        return Err(JitVerdict::UnsupportedOpcode("STORE_SUBSCR (value lane)"));
                    }
                } else if let Some(slot) = val.src {
                    set_local(local_types, slot, el, changed)?;
                }
            }
        }
        other => return Err(JitVerdict::UnsupportedOpcode(other.name())),
    }
    Ok(())
}

/// RFC 0061 WS5 — validate a subscript index operand: a concrete lane
/// must be exactly `Int`; an untyped live-in load is inferred as `Int`;
/// a transient `Unknown` is tolerated for a later iteration.
fn check_subscr_index(
    idx: &SE,
    local_types: &mut [Option<JitType>],
    changed: &mut bool,
) -> Result<(), JitVerdict> {
    if idx.ty.is_representable() {
        if idx.ty != JitType::Int {
            return Err(JitVerdict::UnsupportedOpcode("subscript index lane"));
        }
        Ok(())
    } else if let Some(slot) = idx.src {
        set_local(local_types, slot, JitType::Int, changed)
    } else {
        Ok(())
    }
}

/// RFC 0061 WS5 — resolve a subscript container operand to a pinned
/// element lane. An untyped local load consults the embedder's shape
/// probe and pins the slot; a concrete non-list lane disqualifies;
/// a transient `Unknown` yields `None` (tolerated during inference,
/// bailed at emission if never resolved).
fn resolve_list_container(
    cont: &SE,
    local_types: &mut [Option<JitType>],
    changed: &mut bool,
    probes: &mut Probes<'_>,
) -> Result<Option<JitType>, JitVerdict> {
    if let Some(el) = cont.ty.elem_lane() {
        return Ok(Some(el));
    }
    if cont.ty.is_representable() {
        return Err(JitVerdict::UnsupportedOpcode("subscript container lane"));
    }
    if let Some(slot) = cont.src {
        let Some(elem) = (probes.list)(slot) else {
            return Err(JitVerdict::UnsupportedOpcode("subscript container shape"));
        };
        // `Some(Unknown)` = an empty list: no lane evidence, and any
        // subscript on it would raise anyway.
        let list_ty =
            JitType::list_of(elem).ok_or(JitVerdict::UnsupportedOpcode("subscript elem lane"))?;
        set_local(local_types, slot, list_ty, changed)?;
        return Ok(Some(elem));
    }
    Ok(None)
}

/// RFC 0065 WS5 — the receiver slot of an attribute access: an
/// `Obj`-lane (or as-yet-untyped) local load. `None` when the receiver
/// has no local provenance or wears an incompatible concrete lane.
fn obj_recv_slot(recv: &SE) -> Option<u32> {
    match recv.ty {
        JitType::Obj => recv.src,
        JitType::Unknown => recv.src,
        _ => None,
    }
}

/// RFC 0065 WS5 — infer one `LOAD_ATTR`. Order of resolution:
///
/// 1. `.append` on a (known or probed) list receiver → re-mark the
///    receiver in place; the following `CALL 1` lowers to `ListAppend`.
/// 2. Any other name on an instance receiver whose probe reports an
///    eligible scalar instance-dict attribute → pin the slot as `Obj`
///    and push the value lane (lowered to `AttrGet`).
/// 3. Anything else disqualifies (a transient untyped receiver is
///    tolerated for a later iteration).
fn infer_load_attr(
    name: &str,
    recv: SE,
    i: usize,
    stack: &mut Vec<SE>,
    local_types: &mut [Option<JitType>],
    changed: &mut bool,
    probes: &mut Probes<'_>,
) -> Result<(), JitVerdict> {
    if name == "append" {
        if recv.ty.is_list() {
            stack.push(SE {
                recv: Some(i as u32),
                ..recv
            });
            return Ok(());
        }
        if !recv.ty.is_representable() {
            if let Some(slot) = recv.src {
                match (probes.list)(slot) {
                    // Empty list: definitely a list, lane pinned by the
                    // appended value at the CALL.
                    Some(JitType::Unknown) => {
                        stack.push(SE {
                            recv: Some(i as u32),
                            ..recv
                        });
                        return Ok(());
                    }
                    Some(elem) => {
                        let lty = JitType::list_of(elem)
                            .ok_or(JitVerdict::UnsupportedOpcode("append (elem lane)"))?;
                        set_local(local_types, slot, lty, changed)?;
                        stack.push(SE {
                            ty: lty,
                            recv: Some(i as u32),
                            ..recv
                        });
                        return Ok(());
                    }
                    // Not a list — fall through to the attribute path
                    // (an instance attribute literally named "append").
                    None => {}
                }
            }
        }
    }
    let Some(slot) = obj_recv_slot(&recv) else {
        if !recv.ty.is_representable() {
            // Transient — tolerate; emission bails if never resolved.
            stack.push(SE::known(JitType::Unknown));
            return Ok(());
        }
        return Err(JitVerdict::UnsupportedOpcode("LOAD_ATTR receiver"));
    };
    let Some(lane) = (probes.attr)(slot, name, false) else {
        return Err(JitVerdict::UnsupportedOpcode("LOAD_ATTR shape"));
    };
    set_local(local_types, slot, JitType::Obj, changed)?;
    stack.push(SE::known(lane));
    Ok(())
}

/// RFC 0065 WS5 — infer one `list.append(v)` call: the value lane must
/// match the pinned element lane exactly; an untyped receiver (empty
/// list) is pinned *by* the value's lane.
fn infer_append(
    recv: &SE,
    val: &SE,
    local_types: &mut [Option<JitType>],
    changed: &mut bool,
) -> Result<(), JitVerdict> {
    if let Some(elem) = recv.ty.elem_lane() {
        if val.ty.is_representable() {
            if val.ty != elem {
                return Err(JitVerdict::UnsupportedOpcode("append (value lane)"));
            }
        } else if let Some(vs) = val.src {
            set_local(local_types, vs, elem, changed)?;
        }
        return Ok(());
    }
    // Receiver lane pending (empty-list probe): pin it from the value.
    if val.ty.is_representable() {
        let lty =
            JitType::list_of(val.ty).ok_or(JitVerdict::UnsupportedOpcode("append (value lane)"))?;
        if let Some(slot) = recv.src {
            set_local(local_types, slot, lty, changed)?;
        }
    }
    // Both untyped: transient; a later iteration resolves or emission
    // bails.
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
/// RFC 0065 WS5 adds `src` (receiver-slot provenance for attribute
/// sites), `recv` (an erased `.append` receiver: native holds the pin,
/// the interpreter holds the bound method), and `poison` (`append`'s
/// `None` result: on the interpreter stack only, consumed by
/// `POP_TOP`).
#[derive(Clone, Copy)]
struct ESlot {
    ty: JitType,
    callee: Option<CalleeMark>,
    src: Option<u32>,
    recv: Option<u32>,
    poison: bool,
    /// RFC 0068 — a `PUSH_NULL` self-or-null marker (`Unbound` on the
    /// interpreter stack, never native), consumed by its `CALL`.
    null: bool,
}

impl ESlot {
    fn val(ty: JitType) -> ESlot {
        ESlot {
            ty,
            callee: None,
            src: None,
            recv: None,
            poison: false,
            null: false,
        }
    }

    fn is_plain(&self) -> bool {
        self.callee.is_none() && self.recv.is_none() && !self.poison && !self.null
    }
}

/// Pop an emission-stack value that must be a plain lane (not a callee
/// marker, an `.append` receiver, or `append`'s poison result).
fn pop_val(stack: &mut Vec<ESlot>) -> Result<JitType, JitVerdict> {
    let s = stack.pop().ok_or(JitVerdict::StackUnderflow)?;
    if !s.is_plain() {
        return Err(JitVerdict::UnsupportedOpcode("CALL (callee escapes)"));
    }
    Ok(s.ty)
}

/// Mutable side outputs of the emission pass shared across blocks.
struct EmitOut {
    max_stack: u32,
    callee_spans: Vec<CalleeSpanMeta>,
    /// RFC 0065 WS5 — erased `len` builtins (guard-snapshot object,
    /// `live_to` = pc after the `CALL`).
    len_spans: Vec<CalleeSpanMeta>,
    /// RFC 0065 WS5 — erased `.append` bound-method receivers.
    method_spans: Vec<MethodSpanMeta>,
    /// RFC 0065 WS5 — burned-in attribute-access sites, in `site`
    /// token order.
    attr_sites: Vec<AttrSiteMeta>,
    max_call_args: u32,
}

/// Emit the typed IR for one block, with all local types now known.
#[allow(clippy::too_many_arguments)]
fn emit_block(
    code: &CodeObject,
    b: &RawBlock,
    plan: &Plan,
    local_types: &[Option<JitType>],
    ret_lane: Option<JitType>,
    compact: &HashMap<usize, BlockId>,
    out: &mut EmitOut,
    probes: &mut Probes<'_>,
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
            probes,
        )?;
    }

    let last = b.end - 1;
    let ins = code.instructions[last];
    let term = match ins.op {
        OpCode::ReturnValue => {
            // Lowering pops the return value off its own type stack at
            // the `Return` terminator; no statement is emitted here.
            let top = stack.last().ok_or(JitVerdict::StackUnderflow)?;
            if !top.is_plain() {
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
                probes,
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
    probes: &mut Probes<'_>,
) -> Result<(), JitVerdict> {
    let ins = code.instructions[i];
    let pc = i as u32;
    let EmitOut {
        max_stack,
        callee_spans,
        len_spans,
        method_spans,
        attr_sites,
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
                        callee: Some(CalleeMark {
                            kind: MarkKind::Py,
                            token,
                            arg_count,
                            is_self,
                            ret,
                            load_pc: pc,
                            interp_depth: n_iters + stack.len() as u32,
                        }),
                        ..ESlot::val(JitType::Unknown)
                    });
                    return Ok(());
                }
                // RFC 0065 WS5: `len` rides the interpreter stack the
                // same way; the `CALL` lowers to `ListLen`.
                Some(ResolvedGlobal::LenBuiltin) => {
                    let n_iters = plan
                        .loops
                        .iter()
                        .filter(|l| (l.live_from as usize) <= i && i < l.live_to as usize)
                        .count() as u32;
                    stack.push(ESlot {
                        callee: Some(CalleeMark {
                            kind: MarkKind::Len,
                            token: 0,
                            arg_count: 1,
                            is_self: false,
                            ret: Some(JitType::Int),
                            load_pc: pc,
                            interp_depth: n_iters + stack.len() as u32,
                        }),
                        ..ESlot::val(JitType::Unknown)
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
            // Push manually so the slot provenance rides along (RFC
            // 0065 WS5 attribute sites re-probe by slot).
            stmts.push(TStmt {
                pc,
                op: TOp::LoadLocal(ins.arg),
            });
            stack.push(ESlot {
                src: Some(ins.arg),
                ..ESlot::val(ty)
            });
            *max_stack = (*max_stack).max(stack.len() as u32);
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
        // RFC 0065 WS5 — attribute load: erased `.append` method load
        // (re-mark the receiver in place; no native op), or a pinned-
        // instance scalar attribute read (an `AttrGet` site).
        OpCode::LoadAttr => {
            let name = code
                .names
                .get(ins.arg as usize)
                .ok_or(JitVerdict::UnsupportedOpcode("LOAD_ATTR bad name"))?
                .as_str();
            let top = stack.last().copied().ok_or(JitVerdict::StackUnderflow)?;
            if !top.is_plain() {
                return Err(JitVerdict::UnsupportedOpcode("CALL (callee escapes)"));
            }
            if name == "append" && top.ty.is_list() {
                let last = stack.len() - 1;
                stack[last].recv = Some(pc);
                return Ok(());
            }
            if top.ty != JitType::Obj {
                return Err(JitVerdict::UnsupportedOpcode("LOAD_ATTR receiver"));
            }
            let slot = top.src.ok_or(JitVerdict::UnsupportedOpcode(
                "LOAD_ATTR (receiver provenance)",
            ))?;
            let lane = (probes.attr)(slot, name, false)
                .ok_or(JitVerdict::UnsupportedOpcode("LOAD_ATTR shape"))?;
            stack.pop();
            let site = attr_sites.len() as u32;
            attr_sites.push(AttrSiteMeta {
                slot,
                name: name.to_owned(),
                lane,
                store: false,
            });
            push(TOp::AttrGet { site, out: lane }, Some(lane), stack, stmts);
        }
        // RFC 0068 — the self-or-null slot: interpreter-stack only,
        // never a native value.
        OpCode::PushNull => {
            stack.push(ESlot {
                null: true,
                ..ESlot::val(JitType::Unknown)
            });
        }
        // RFC 0068 — method-form attribute load: the erased `.append`
        // load (re-mark the receiver, no native op), with the implicit
        // self-or-null marker on top. Any other method shape falls
        // through to the `CALL` arm and bails there, matching inference.
        OpCode::LoadMethodAttr => {
            let name = code
                .names
                .get(ins.arg as usize)
                .ok_or(JitVerdict::UnsupportedOpcode("LOAD_ATTR bad name"))?
                .as_str();
            let top = stack.last().copied().ok_or(JitVerdict::StackUnderflow)?;
            if !top.is_plain() {
                return Err(JitVerdict::UnsupportedOpcode("CALL (callee escapes)"));
            }
            if name == "append" && top.ty.is_list() {
                let last = stack.len() - 1;
                stack[last].recv = Some(pc);
                stack.push(ESlot {
                    null: true,
                    ..ESlot::val(JitType::Unknown)
                });
                return Ok(());
            }
            if top.ty != JitType::Obj {
                return Err(JitVerdict::UnsupportedOpcode("LOAD_ATTR receiver"));
            }
            let slot = top.src.ok_or(JitVerdict::UnsupportedOpcode(
                "LOAD_ATTR (receiver provenance)",
            ))?;
            let lane = (probes.attr)(slot, name, false)
                .ok_or(JitVerdict::UnsupportedOpcode("LOAD_ATTR shape"))?;
            stack.pop();
            let site = attr_sites.len() as u32;
            attr_sites.push(AttrSiteMeta {
                slot,
                name: name.to_owned(),
                lane,
                store: false,
            });
            push(TOp::AttrGet { site, out: lane }, Some(lane), stack, stmts);
            stack.push(ESlot {
                null: true,
                ..ESlot::val(JitType::Unknown)
            });
        }
        // RFC 0065 WS5 — pinned-instance scalar attribute write (an
        // `AttrSet` site). Stack is `[.., value, receiver]`.
        OpCode::StoreAttr => {
            let name = code
                .names
                .get(ins.arg as usize)
                .ok_or(JitVerdict::UnsupportedOpcode("STORE_ATTR bad name"))?
                .as_str();
            let recv = stack.pop().ok_or(JitVerdict::StackUnderflow)?;
            if !recv.is_plain() || recv.ty != JitType::Obj {
                return Err(JitVerdict::UnsupportedOpcode("STORE_ATTR receiver"));
            }
            let slot = recv.src.ok_or(JitVerdict::UnsupportedOpcode(
                "STORE_ATTR (receiver provenance)",
            ))?;
            let lane = (probes.attr)(slot, name, true)
                .ok_or(JitVerdict::UnsupportedOpcode("STORE_ATTR shape"))?;
            let val = pop_val(stack)?;
            if val != lane {
                return Err(JitVerdict::UnsupportedOpcode("STORE_ATTR (value lane)"));
            }
            let site = attr_sites.len() as u32;
            attr_sites.push(AttrSiteMeta {
                slot,
                name: name.to_owned(),
                lane,
                store: true,
            });
            // Native stack order matches the interpreter: value below,
            // receiver on top. Lowering pops receiver then value.
            stack.push(recv);
            push(TOp::AttrSet { site }, None, stack, stmts);
            stack.pop();
        }
        // RFC 0059 WS3 — Python-to-Python call: pop the scalar args and
        // the callee marker, close the deopt span, and emit `CallPy`.
        // RFC 0065 WS5 adds the `len(list)` and `list.append(v)` shapes.
        OpCode::Call => {
            let argc = ins.arg as usize;
            if stack.len() < argc + 2 {
                return Err(JitVerdict::StackUnderflow);
            }
            let mut arg_tys: Vec<JitType> = Vec::with_capacity(argc);
            for _ in 0..argc {
                arg_tys.push(pop_val(stack)?);
            }
            // RFC 0068 — the self-or-null slot below the arguments.
            let slot = stack.pop().ok_or(JitVerdict::StackUnderflow)?;
            if !slot.null {
                return Err(JitVerdict::UnsupportedOpcode("CALL (self slot)"));
            }
            let f = stack.pop().ok_or(JitVerdict::StackUnderflow)?;
            // `x.append(v)` — the pin stayed on the native stack under
            // the argument; no callee object was ever native.
            if let Some(load_pc) = f.recv {
                if argc != 1 {
                    return Err(JitVerdict::UnsupportedOpcode("append (arity)"));
                }
                let elem =
                    f.ty.elem_lane()
                        .ok_or(JitVerdict::UnsupportedOpcode("append (receiver lane)"))?;
                if arg_tys[0] != elem {
                    return Err(JitVerdict::UnsupportedOpcode("append (value lane)"));
                }
                // The receiver's bottom-based native-stack index equals
                // its spill index: everything below it that carries a
                // native value (markers and null slots don't).
                let native_index = stack
                    .iter()
                    .filter(|s| s.callee.is_none() && !s.null)
                    .count() as u32;
                method_spans.push(MethodSpanMeta {
                    native_index,
                    live_from: load_pc,
                    live_to: pc + 1,
                });
                // Re-model the operands for the statement, then emit:
                // `ListAppend` pops the value and the pin natively.
                stack.push(ESlot::val(f.ty));
                stack.push(ESlot::val(elem));
                push(TOp::ListAppend, None, stack, stmts);
                stack.pop();
                stack.pop();
                // `append` returns `None` — interpreter-stack only.
                stack.push(ESlot {
                    poison: true,
                    ..ESlot::val(JitType::Unknown)
                });
                *max_stack = (*max_stack).max(stack.len() as u32);
                return Ok(());
            }
            let Some(mark) = f.callee else {
                return Err(JitVerdict::UnsupportedOpcode("CALL"));
            };
            if mark.arg_count as usize != argc {
                return Err(JitVerdict::UnsupportedOpcode("CALL (arity)"));
            }
            // `len(x)` on a pinned list — no real call, no deopt-able
            // argument window (only loads can produce a list value).
            if mark.kind == MarkKind::Len {
                if !arg_tys[0].is_list() {
                    return Err(JitVerdict::UnsupportedOpcode("len (argument lane)"));
                }
                len_spans.push(CalleeSpanMeta {
                    token: 0,
                    live_from: mark.load_pc,
                    live_to: pc + 1,
                    interp_depth: mark.interp_depth,
                });
                // Re-model the pin for the statement (ListLen pops it).
                stack.push(ESlot::val(arg_tys[0]));
                push(TOp::ListLen, None, stack, stmts);
                stack.pop();
                stack.push(ESlot::val(JitType::Int));
                *max_stack = (*max_stack).max(stack.len() as u32);
                return Ok(());
            }
            for &ty in &arg_tys {
                if !ty.is_representable() {
                    return Err(JitVerdict::TypeUnknown);
                }
                // RFC 0061/0065 WS5 — a pin index is meaningless outside
                // this activation; a pinned value cannot be marshaled as
                // a scalar call argument.
                if ty.is_pinned() {
                    return Err(JitVerdict::UnsupportedOpcode("CALL (pinned argument)"));
                }
            }
            let ret = match if mark.is_self { ret_lane } else { mark.ret } {
                Some(t) if t.is_representable() => t,
                _ => return Err(JitVerdict::TypeUnknown),
            };
            // RFC 0061/0065 WS5 — a pin index is only meaningful within
            // its own activation's pinned-object table; a callee's
            // returned value cannot cross the boundary as one.
            if ret.is_pinned() {
                return Err(JitVerdict::UnsupportedOpcode("CALL (pinned return)"));
            }
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
            // RFC 0065 WS5 — `append`'s poison result never existed on
            // the native stack; consume it silently.
            if stack.last().is_some_and(|s| s.poison) {
                stack.pop();
                return Ok(());
            }
            pop_val(stack)?;
            push(TOp::Pop, None, stack, stmts);
        }
        OpCode::CopyTop => {
            let t = *stack.last().ok_or(JitVerdict::StackUnderflow)?;
            if !t.is_plain() {
                return Err(JitVerdict::UnsupportedOpcode("CALL (callee escapes)"));
            }
            // Manual push: the copy keeps the slot provenance so an
            // augmented attribute assignment (`p.a += 1`, which dups
            // the receiver) still knows its receiver slot.
            stmts.push(TStmt { pc, op: TOp::Dup });
            stack.push(t);
            *max_stack = (*max_stack).max(stack.len() as u32);
        }
        OpCode::Swap => {
            if ins.arg != 2 {
                return Err(JitVerdict::UnsupportedOpcode("SWAP n!=2"));
            }
            let len = stack.len();
            if len < 2 {
                return Err(JitVerdict::StackUnderflow);
            }
            if !stack[len - 1].is_plain() || !stack[len - 2].is_plain() {
                return Err(JitVerdict::UnsupportedOpcode("CALL (callee escapes)"));
            }
            stack.swap(len - 1, len - 2);
            push(TOp::Swap2, None, stack, stmts);
        }
        // RFC 0061 WS5 — pinned-list element read/write. Inference
        // already pinned the container slot's lane; emission just
        // re-validates the operand lanes it sees.
        OpCode::BinarySubscr => {
            let idx = pop_val(stack)?;
            if idx != JitType::Int {
                return Err(JitVerdict::UnsupportedOpcode("subscript index lane"));
            }
            let cont = pop_val(stack)?;
            let elem = cont
                .elem_lane()
                .ok_or(JitVerdict::UnsupportedOpcode("subscript container lane"))?;
            push(TOp::ListGet { elem }, Some(elem), stack, stmts);
        }
        OpCode::StoreSubscr => {
            let idx = pop_val(stack)?;
            if idx != JitType::Int {
                return Err(JitVerdict::UnsupportedOpcode("subscript index lane"));
            }
            let cont = pop_val(stack)?;
            let elem = cont
                .elem_lane()
                .ok_or(JitVerdict::UnsupportedOpcode("subscript container lane"))?;
            let val = pop_val(stack)?;
            if val != elem {
                return Err(JitVerdict::UnsupportedOpcode("STORE_SUBSCR (value lane)"));
            }
            push(TOp::ListSet, None, stack, stmts);
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

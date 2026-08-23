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

use weavepy_compiler::{
    BinOpKind, CodeObject, CompareKind, Constant, OpCode, UnaryKind, BINARY_OP_INPLACE_FLAG,
};

use crate::ir::{
    ArithKind, AttrSiteMeta, BlockId, CalleeSpanMeta, CmpKind, GlobalGuard, IterLoopMeta,
    ListLoopMeta, MathFunc, MathGuardMeta, MethodRet, MethodSiteMeta, MethodSpanMeta, OsrEntry,
    RangeLoopMeta, ResolvedGlobal, TBlock, TFunc, TOp, TStmt, TTerm,
};
use crate::value::JitType;

/// RFC 0069 WS1 — the embedder's resolution of one `(slot, name)`
/// method probe: the class-resolved plain Python function's token in
/// the embedder's method table (which the embedder must keep parallel
/// to [`TFunc::method_sites`]), its positional arity (`self`
/// included), the arity minus trailing defaults, and its result
/// typing. The resolution is a prediction — the call helper
/// re-validates the class fingerprint and `__code__` identity per
/// call — but the *token assignment* must be stable across repeated
/// probes of the same `(slot, name)` pair (the analyzer probes during
/// both inference and emission).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MethodResolution {
    pub token: u32,
    pub arg_count: u32,
    pub min_args: u32,
    pub ret: MethodRet,
}

/// RFC 0071 WS3 — the maximum attribute-chain depth the analyzer
/// tracks as provenance (`a.b.c.d` from a root local). Deeper chains
/// disqualify the access rather than growing unbounded metadata.
const MAX_ATTR_PATH: usize = 4;

/// RFC 0071 WS3 — interned attribute-path provenance. An `Obj`-lane
/// value produced by an attribute load carries `(root local, chain of
/// names)` so a later access *through* it can be probed against the
/// live object the chain currently reaches. Segments form a parent-
/// linked interning tree; stack entries carry a segment index (they
/// must stay `Copy`).
#[derive(Debug, Default)]
pub struct PathArena {
    /// `(parent segment, root local slot, attribute name)`.
    segs: Vec<(Option<u32>, u32, String)>,
}

impl PathArena {
    /// Intern one more link on `parent` (rooted at `root`). `None`
    /// when the chain would exceed [`MAX_ATTR_PATH`].
    fn seg(&mut self, parent: Option<u32>, root: u32, name: &str) -> Option<u32> {
        if let Some(idx) = self
            .segs
            .iter()
            .position(|(p, r, n)| *p == parent && *r == root && n == name)
        {
            return Some(idx as u32);
        }
        if self.names(parent).len() >= MAX_ATTR_PATH {
            return None;
        }
        self.segs.push((parent, root, name.to_owned()));
        Some((self.segs.len() - 1) as u32)
    }

    /// The root local slot of a segment chain.
    fn root(&self, idx: u32) -> u32 {
        self.segs[idx as usize].1
    }

    /// The chain's names, root-first.
    fn names(&self, idx: Option<u32>) -> Vec<String> {
        let mut out = Vec::new();
        let mut cur = idx;
        while let Some(i) = cur {
            let (parent, _, name) = &self.segs[i as usize];
            out.push(name.clone());
            cur = *parent;
        }
        out.reverse();
        out
    }
}

/// The embedder's shape probes (RFC 0061/0065 WS5, 0069 WS1/WS2),
/// bundled so the inference and emission passes share one source of
/// truth.
#[allow(missing_debug_implementations)]
pub struct Probes<'a> {
    /// Element lane of a local currently holding a homogeneous `int`/
    /// `float` list (`Some(Unknown)` = an *empty* list: definitely a
    /// list, but with no lane evidence — only `append` can pin it).
    pub list: &'a mut dyn FnMut(u32) -> Option<JitType>,
    /// `(slot, path, name, store)` → the value lane of an eligible
    /// instance attribute on the object reached by walking `path` from
    /// the local currently in `slot` (RFC 0065 WS5; RFC 0071 WS3 adds
    /// the chain walk). Eligibility mirrors the tier-1 inline-cache
    /// predicate: no `__getattr__`/`__getattribute__`, no shadowing
    /// data descriptor, name present in the instance dict — plus,
    /// RFC 0071 WS2, the store-only new-key shape reported as
    /// `Some(Unknown)`.
    pub attr: &'a mut dyn FnMut(u32, &[String], &str, bool) -> Option<JitType>,
    /// RFC 0069 WS1 — `(slot, path, name)` → the class-resolved method
    /// on the instance reached by walking `path` from the local
    /// currently in `slot`, when the shape is eligible (plain function
    /// on the class, not shadowed by an instance attribute, receiver
    /// an instance with a pinnable class).
    pub method: &'a mut dyn FnMut(u32, &[String], &str) -> Option<MethodResolution>,
    /// RFC 0069 WS2 — `(global_name, attr)` → `true` when the global
    /// currently resolves to the canonical `math` module *and* its
    /// `attr` is the canonical intrinsic function (the embedder
    /// snapshots both as entry guards).
    pub math: &'a mut dyn FnMut(&str, &str) -> bool,
    /// RFC 0069 WS3 — the *observed* scalar lane of a parameter slot in
    /// the requesting activation (`None` = not a managed scalar, or no
    /// live activation to observe). Purely a prediction: every typed
    /// parameter slot is entry-guarded, so a call with a differently-
    /// typed argument falls back to the interpreter. The embedder
    /// supplies this only on a *retry* after an unseeded analysis
    /// failed with [`JitVerdict::TypeUnknown`], so shapes the fixpoint
    /// can type on its own never pick up extra guards or conflicts.
    pub param: &'a mut dyn FnMut(u32) -> Option<JitType>,
    /// RFC 0071 WS3 — the analysis-local attribute-path interner (the
    /// analyzer owns the entries; it lives here so both passes and the
    /// probe adapters share it without threading another parameter).
    pub paths: &'a mut PathArena,
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
    /// RFC 0071 WS4 — rewritten `GET_ITER` pcs of recognized list
    /// loops → (seq synthetic slot, index synthetic slot). The op pops
    /// the pinned list into the seq slot and zeroes the index.
    get_iter: HashMap<usize, (u32, u32)>,
    /// RFC 0071 WS4 — list-loop `FOR_ITER` pcs → (seq slot, index
    /// slot, loop variable slot).
    iter_headers: HashMap<usize, (u32, u32, u32)>,
    /// RFC 0071 WS4 — the fused `STORE_FAST` pc after each list-loop
    /// `FOR_ITER` → its slot (the `ForList` terminator performs this
    /// store; the variable's lane comes from the header's element
    /// lane, not a forced `Int`).
    fused_store_iter: HashMap<usize, u32>,
    /// RFC 0071 WS4 — rewritten list loops, ascending `live_from`.
    list_loops: Vec<ListLoopMeta>,
    /// Synthetic slots appended after the code object's real locals.
    n_synth: u32,
}

impl Plan {
    /// `true` when the interpreter would have a live iterator on its
    /// stack at `pc` — i.e. `pc` is inside some rewritten loop (range
    /// or list).
    fn in_loop_span(&self, pc: usize) -> bool {
        self.loops
            .iter()
            .any(|l| (l.live_from as usize) <= pc && pc < l.live_to as usize)
            || self
                .list_loops
                .iter()
                .any(|l| (l.live_from as usize) <= pc && pc < l.live_to as usize)
    }

    /// How many rewritten-loop iterators the *interpreter's* stack
    /// holds at `pc` (range and list loops both park one erased
    /// iterator there for their whole span).
    fn live_iters_at(&self, pc: usize) -> u32 {
        let ranges = self
            .loops
            .iter()
            .filter(|l| (l.live_from as usize) <= pc && pc < l.live_to as usize)
            .count();
        let lists = self
            .list_loops
            .iter()
            .filter(|l| (l.live_from as usize) <= pc && pc < l.live_to as usize)
            .count();
        (ranges + lists) as u32
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
    // Adapt the path-less probe shape: only direct local receivers
    // resolve (RFC 0071 WS3 chains need the full-bundle entry).
    let mut attr = |slot: u32, path: &[String], name: &str, store: bool| {
        if path.is_empty() {
            probe_attr(slot, name, store)
        } else {
            None
        }
    };
    let mut arena = PathArena::default();
    let mut probes = Probes {
        list: probe_list,
        attr: &mut attr,
        method: &mut |_, _, _| None,
        math: &mut |_, _| false,
        param: &mut |_| None,
        paths: &mut arena,
    };
    analyze_impl(code, resolve, &mut probes)
}

/// [`analyze_with_probes`] with the full probe bundle (RFC 0069 WS1/
/// WS2 adds the method-resolution and math-module probes).
pub fn analyze_frame(
    code: &CodeObject,
    resolve: &mut dyn FnMut(&str) -> ResolvedGlobal,
    probes: &mut Probes<'_>,
) -> Result<TFunc, JitVerdict> {
    analyze_impl(code, resolve, probes)
}

fn analyze_impl(
    code: &CodeObject,
    resolve: &mut dyn FnMut(&str) -> ResolvedGlobal,
    probes: &mut Probes<'_>,
) -> Result<TFunc, JitVerdict> {
    // RFC 0070 WS2 — sync generator bodies are admitted (yields become
    // `Yielded` side exits; entry is OSR-only). Coroutines and async
    // generators stay excluded this wave.
    if code.is_coroutine || code.is_async_generator || code.is_class_body {
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

    // Type inference fixpoint. `ret` tracks the function's own return
    // typing, fed back into self-recursive `CallPy` results (RFC 0059
    // WS3) — fib-shaped recursion converges in two passes. RFC 0069
    // WS2 adds per-block *entry stacks*: a block seeded `None` has not
    // been reached yet and is skipped until a predecessor seeds it
    // (the entry block starts empty); boundary stacks merge
    // elementwise, refining `Unknown` lanes and requiring marker
    // identity.
    let mut local_types: Vec<Option<JitType>> = vec![None; n_locals as usize];
    // RFC 0069 WS3 — seed parameter lanes from the requesting
    // activation's live argument values (scalar lanes only; the probe
    // is a no-op unless the embedder opted into seeding, see
    // [`Probes::param`]). A seeded slot behaves exactly like one typed
    // by an assignment: conflicting evidence still rejects, and the
    // final `TFunc` entry-guards it.
    for slot in 0..code.arg_count {
        if local_types[slot as usize].is_none() {
            if let Some(t) = (probes.param)(slot) {
                // RFC 0071 — `Str`/`Bytes` (WS6) and the nullable
                // object lane (WS1) join the seedable lanes: entry
                // packing pins exact-typed values (the guard rejects
                // subclasses), and an `Obj` seed admits parameters
                // whose only use is flowing into a call.
                if matches!(
                    t,
                    JitType::Int
                        | JitType::Float
                        | JitType::Bool
                        | JitType::Obj
                        | JitType::Str
                        | JitType::Bytes
                ) {
                    local_types[slot as usize] = Some(t);
                }
            }
        }
    }
    let mut ret = RetInfo::default();
    let mut entry_stacks: Vec<Option<Vec<SE>>> = vec![None; raw.len()];
    let entry_raw = block_index_at(&raw, 0);
    entry_stacks[entry_raw] = Some(Vec::new());
    let mut iters = 0;
    loop {
        let mut changed = false;
        for &bi in &reachable {
            let Some(entry) = entry_stacks[bi].clone() else {
                continue;
            };
            let outs = infer_block(
                code,
                &raw[bi],
                &plan,
                entry,
                &mut local_types,
                &mut ret,
                &mut changed,
                probes,
            )?;
            for (succ, stack) in outs {
                if succ == entry_raw && !stack.is_empty() {
                    return Err(JitVerdict::NonEmptyBoundaryStack);
                }
                merge_entry(&mut entry_stacks[succ], stack, &mut changed)?;
            }
        }
        if !changed {
            break;
        }
        iters += 1;
        if iters > MAX_INFER_ITERS {
            return Err(JitVerdict::NotConverged);
        }
    }
    // A reachable block never seeded means its only in-edges carry a
    // stack shape inference refused; treat as non-analyzable.
    if reachable.iter().any(|&bi| entry_stacks[bi].is_none()) {
        return Err(JitVerdict::NonEmptyBoundaryStack);
    }
    let ret_lane = ret.final_lane();
    let ret_none = ret.saw_none && !ret.saw_scalar;

    // Compact block ids over reachable blocks (entry first is convenient
    // but not required — we record the entry id explicitly).
    let mut compact: HashMap<usize, BlockId> = HashMap::new();
    for (idx, &bi) in reachable.iter().enumerate() {
        compact.insert(bi, idx);
    }
    let entry_block = *compact
        .get(&block_index_at(&raw, 0))
        .ok_or(JitVerdict::Trivial)?;

    // Emission pass. Entry ESlot stacks propagate forward along the
    // same edges inference walked: a block is emittable once any
    // predecessor has seeded its entry (all predecessors agree — the
    // fixpoint proved it), so a worklist over seeded blocks visits
    // every reachable block exactly once regardless of index order.
    let mut out = EmitOut {
        max_stack: 0,
        callee_spans: Vec::new(),
        len_spans: Vec::new(),
        method_spans: Vec::new(),
        attr_sites: Vec::new(),
        method_sites: Vec::new(),
        math_guards: Vec::new(),
        math_spans: Vec::new(),
        max_call_args: 0,
    };
    let mut emit_entries: Vec<Option<Vec<ESlot>>> = vec![None; raw.len()];
    emit_entries[entry_raw] = Some(Vec::new());
    let mut blocks_opt: Vec<Option<TBlock>> = vec![None; reachable.len()];
    let mut queue: VecDeque<usize> = reachable.iter().copied().collect();
    let mut stalled = 0usize;
    while let Some(bi) = queue.pop_front() {
        let Some(entry) = emit_entries[bi].clone() else {
            // Not seeded yet — requeue. A full lap without progress
            // means an unreachable-by-seeding block, which inference
            // would already have rejected (defensive).
            stalled += 1;
            if stalled > queue.len() {
                return Err(JitVerdict::NonEmptyBoundaryStack);
            }
            queue.push_back(bi);
            continue;
        };
        stalled = 0;
        let tb = emit_block(
            code,
            &raw[bi],
            &plan,
            &local_types,
            ret_lane,
            &compact,
            entry,
            &mut emit_entries,
            &mut out,
            probes,
        )?;
        blocks_opt[compact[&bi]] = Some(tb);
    }
    let mut blocks: Vec<TBlock> = Vec::with_capacity(reachable.len());
    for tb in blocks_opt {
        blocks.push(tb.ok_or(JitVerdict::NonEmptyBoundaryStack)?);
    }
    let method_sites: Vec<MethodSiteMeta> = {
        let mut sites = Vec::with_capacity(out.method_sites.len());
        for s in out.method_sites {
            sites.push(s.ok_or(JitVerdict::UnsupportedOpcode("method site (token gap)"))?);
        }
        sites
    };

    // OSR entry points (RFC 0059 WS3b): every backward-jump target
    // with an *empty* boundary stack is enterable mid-frame once the
    // VM packs the locals (and decomposes any live range iterators
    // into their synthetic slots). RFC 0069 WS2 — a target carrying
    // boundary values is simply not OSR-enterable.
    let mut osr_entries: Vec<OsrEntry> = Vec::new();
    let mut osr_seen: HashSet<usize> = HashSet::new();
    for (i, ins) in code.instructions.iter().enumerate() {
        if matches!(ins.op, OpCode::JumpBackward) {
            let t = backward_target(i, ins.arg).ok_or(JitVerdict::BadJumpTarget)?;
            if osr_seen.insert(t) {
                let raw_idx = block_index_at(&raw, t);
                let empty_entry = entry_stacks[raw_idx]
                    .as_ref()
                    .is_some_and(std::vec::Vec::is_empty);
                if !empty_entry {
                    continue;
                }
                if let Some(&bid) = compact.get(&raw_idx) {
                    osr_entries.push(OsrEntry {
                        pc: t as u32,
                        block: bid,
                    });
                }
            }
        }
    }
    osr_entries.sort_unstable_by_key(|e| e.pc);

    // RFC 0071 WS5 — generator resume entries: for every yield whose
    // continuation block's boundary stack is exactly `[Obj]` (the sent
    // value and nothing beneath it), the embedder may enter natively
    // at the resume pc, passing the sent value through `ret_bits`.
    // A yield buried in a wider expression stack keeps the interpreted
    // resume (the dispatch preamble cannot materialize the deeper
    // spilled values).
    let mut resume_entries: Vec<OsrEntry> = Vec::new();
    if code.is_generator {
        for (i, ins) in code.instructions.iter().enumerate() {
            if !matches!(ins.op, OpCode::YieldValue) || i + 1 >= code.instructions.len() {
                continue;
            }
            let raw_idx = block_index_at(&raw, i + 1);
            if raw[raw_idx].start != i + 1 {
                continue;
            }
            if let Some(&bid) = compact.get(&raw_idx) {
                if blocks[bid].entry_stack == [JitType::Obj] {
                    resume_entries.push(OsrEntry {
                        pc: (i + 1) as u32,
                        block: bid,
                    });
                }
            }
        }
    }
    resume_entries.sort_unstable_by_key(|e| e.pc);

    // RFC 0070 WS2 — generator profitability gate. A generator body
    // only ever enters natively at an OSR pc, and every yield pays the
    // full deopt-shaped round trip (entry guards + marshal in, spill +
    // interpreted suspension out). That round trip is only worth it
    // when real work runs natively *between* suspensions — i.e. when
    // the compiled CFG contains a cycle. Yield blocks have no
    // successors, so any cycle here is by construction yield-free: an
    // inner loop that runs to completion per resume. A body with no
    // native cycle (the classic trailing-yield loop — `while ...:
    // yield x; ...` — whose back edge is only reachable *through* the
    // interpreted resume) would execute a bounded straight-line
    // stretch per entry and lose to the interpreter, so it is ruled
    // not worth compiling. Same verdict when no OSR entry survived:
    // with fresh pc-0 entry gated off for generators, an entry-less
    // body could never run natively at all.
    // RFC 0071 WS5 — resume entries do *not* relax the cycle
    // requirement. A trailing-yield loop (`while ...: yield x`) can
    // enter natively at the yield's continuation, but each resume
    // then executes a bounded straight-line stretch (loop check,
    // next element, yield) and pays the full native entry + spill
    // round trip per element — measured as a clear net loss against
    // the interpreter's resume path (RFC 0071 measurements). Yield-
    // dense bodies stay interpreted; resume entries earn their keep
    // in bodies with a yield-free cycle, where each resume runs a
    // whole inner reduction natively.
    if code.is_generator {
        if osr_entries.is_empty() && resume_entries.is_empty() {
            return Err(JitVerdict::Trivial);
        }
        if !has_native_cycle(&blocks) {
            return Err(JitVerdict::Trivial);
        }
    }

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

    // RFC 0071 WS4 — split the plan-time GET_ITER loops by the seq
    // slot's final lane: pinned-list loops keep the (list, index)
    // reconstruction; object-lane loops rebuild the pinned iterator
    // itself.
    let list_loops: Vec<ListLoopMeta> = plan
        .list_loops
        .iter()
        .filter(|l| {
            local_types
                .get(l.seq_slot as usize)
                .copied()
                .flatten()
                .is_some_and(JitType::is_list)
        })
        .copied()
        .collect();
    let iter_loops: Vec<IterLoopMeta> = plan
        .list_loops
        .iter()
        .filter(|l| local_types.get(l.seq_slot as usize).copied().flatten() == Some(JitType::Obj))
        .map(|l| IterLoopMeta {
            iter_slot: l.seq_slot,
            live_from: l.live_from,
            live_to: l.live_to,
        })
        .collect();

    Ok(TFunc {
        n_locals,
        local_types,
        livein_locals: livein_vec,
        max_stack: out.max_stack,
        blocks,
        entry_block,
        global_guards: plan.guards,
        range_loops: plan.loops,
        list_loops,
        iter_loops,
        callee_spans: out.callee_spans,
        len_spans: out.len_spans,
        method_spans: out.method_spans,
        attr_sites: out.attr_sites,
        method_sites,
        math_guards: out.math_guards,
        math_spans: out.math_spans,
        osr_entries,
        resume_entries,
        max_call_args: out.max_call_args,
        ret_lane: ret_lane.filter(|t| t.is_representable()),
        ret_none,
    })
}

/// RFC 0069 WS1 — the function's return typing accumulated across
/// return sites: the scalar lane fixpoint (RFC 0059 WS3), plus
/// whether any site returned the `None` constant or a value.
#[derive(Default)]
struct RetInfo {
    lane: Option<JitType>,
    saw_none: bool,
    saw_scalar: bool,
}

impl RetInfo {
    /// The function-wide scalar return lane: only meaningful when no
    /// return site was the `None` constant (mixed None/scalar returns
    /// cannot be typed for callers) — except the *object* lane (RFC
    /// 0071 WS1), which is nullable: a `return None` site joins
    /// object-returning sites, crossing the call as the lane's `-1`.
    fn final_lane(&self) -> Option<JitType> {
        if self.saw_none && self.lane != Some(JitType::Obj) {
            None
        } else {
            self.lane
        }
    }
}

/// Merge one block's computed boundary stack into a successor's entry
/// (RFC 0069 WS2). A `None` destination is seeded outright; otherwise
/// the stacks must agree elementwise — identical markers, joinable
/// lanes (`Unknown` refines to a concrete lane; two distinct concrete
/// lanes disqualify), and provenance kept only when both sides agree.
fn merge_entry(
    dst: &mut Option<Vec<SE>>,
    new: Vec<SE>,
    changed: &mut bool,
) -> Result<(), JitVerdict> {
    let Some(cur) = dst else {
        *changed = true;
        *dst = Some(new);
        return Ok(());
    };
    if cur.len() != new.len() {
        return Err(JitVerdict::NonEmptyBoundaryStack);
    }
    for (c, n) in cur.iter_mut().zip(new) {
        if c.callee != n.callee
            || c.recv != n.recv
            || c.method != n.method
            || c.math_mod != n.math_mod
            || c.poison != n.poison
            || c.null != n.null
            || c.none_const != n.none_const
            || c.slice != n.slice
        {
            return Err(JitVerdict::NonEmptyBoundaryStack);
        }
        if c.ty != n.ty {
            if c.ty == JitType::Unknown {
                c.ty = n.ty;
                *changed = true;
            } else if n.ty != JitType::Unknown {
                return Err(JitVerdict::NonEmptyBoundaryStack);
            }
        }
        if c.src != n.src && c.src.is_some() {
            c.src = None;
            *changed = true;
        }
        // RFC 0071 WS3 — attribute-path provenance is kept only when
        // both sides walked the identical chain.
        if c.path != n.path && c.path.is_some() {
            c.path = None;
            *changed = true;
        }
    }
    Ok(())
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
        // Recognize the counted `range` shape first — walk the prefix
        // backwards: GET_ITER, CALL k, k simple args, PUSH_NULL,
        // LOAD_GLOBAL <range>.
        if let Some((callee, push_null, step_nop, pops)) = range_prefix(code, &plan, &targets, i) {
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
            if let Some(step_pc) = step_nop {
                plan.nop.insert(step_pc);
            }
            plan.nop.insert(callee);
            plan.nop.insert(push_null);
            plan.nop.insert(i - 1);
            plan.nop.insert(exit);
            // The POP_TOP paired with END_FOR (CPython 3.13 loop-exit
            // shape) is equally dead in the compiled trace.
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
            continue;
        }
        // RFC 0071 WS4 — the list-loop shape: `<iterable expr>,
        // GET_ITER, FOR_ITER`. The iterable stays an ordinary stack
        // value (any expression); the rewritten `GET_ITER` captures it
        // into the seq synthetic slot (so rebinding the source local
        // mid-loop cannot retarget the iteration) and zeroes the index
        // slot. Whether the captured value is actually a pinned list
        // is decided by type inference at the `GET_ITER`; a non-list
        // disqualifies there.
        if i >= 1 && matches!(ins[i - 1].op, OpCode::GetIter) {
            // No jump may land on the `GET_ITER` or the fused store —
            // the header itself is the only allowed landing point.
            if targets.iter().any(|&t| i - 1 <= t && t <= i + 1 && t != i) {
                return Err(bail());
            }
            let seq_slot = n_real + plan.n_synth;
            let idx_slot = seq_slot + 1;
            plan.n_synth += 2;
            plan.get_iter.insert(i - 1, (seq_slot, idx_slot));
            plan.nop.insert(exit);
            if exit + 1 < n && matches!(ins[exit + 1].op, OpCode::PopTop) {
                plan.nop.insert(exit + 1);
            }
            plan.iter_headers.insert(i, (seq_slot, idx_slot, var_slot));
            plan.fused_store_iter.insert(i + 1, var_slot);
            plan.list_loops.push(ListLoopMeta {
                seq_slot,
                idx_slot,
                live_from: i as u32,
                live_to: exit as u32,
            });
            continue;
        }
        return Err(bail());
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
            | ResolvedGlobal::PyFunc { .. }
            | ResolvedGlobal::MathModule => {
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

/// Recognize the counted-`range` prefix of the `FOR_ITER` at `i`
/// (RFC 0058 WS4): `LOAD_GLOBAL <range>, PUSH_NULL, k simple args,
/// CALL k, GET_ITER`, with no jump landing inside the prefix or on the
/// fused store. Returns `(callee pc, push_null pc, erased step pc,
/// bound pops)` on a match, `None` when the shape doesn't apply (the
/// caller then tries the list-loop shape).
fn range_prefix(
    code: &CodeObject,
    plan: &Plan,
    targets: &HashSet<usize>,
    i: usize,
) -> Option<(usize, usize, Option<usize>, u8)> {
    let ins = &code.instructions;
    if i < 2 || !matches!(ins[i - 1].op, OpCode::GetIter) || !matches!(ins[i - 2].op, OpCode::Call)
    {
        return None;
    }
    let k = ins[i - 2].arg as usize;
    if !(1..=3).contains(&k) || i < 4 + k {
        return None;
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
            _ => return None,
        }
    }
    // An explicit step is only allowed as the constant 1; it is erased
    // so the call effectively becomes `range(start, stop)`.
    let mut pops = k as u8;
    let mut step_nop = None;
    if k == 3 {
        let step_pc = i - 3;
        if !matches!(ins[step_pc].op, OpCode::LoadConst)
            || !matches!(
                code.constants.get(ins[step_pc].arg as usize),
                Some(Constant::Int(1))
            )
        {
            return None;
        }
        step_nop = Some(step_pc);
        pops = 2;
    }
    let push_null = args_start - 1;
    if !matches!(ins[push_null].op, OpCode::PushNull) {
        return None;
    }
    let callee = args_start - 2;
    if !matches!(ins[callee].op, OpCode::LoadGlobal) {
        return None;
    }
    if plan.globals.get(&ins[callee].arg) != Some(&ResolvedGlobal::RangeBuiltin) {
        return None;
    }
    // No jump may land inside the prefix or on the fused store — the
    // header itself (a JUMP_BACKWARD target) is the only allowed
    // landing point.
    if targets.iter().any(|&t| callee < t && t <= i + 1 && t != i) {
        return None;
    }
    Some((callee, push_null, step_nop, pops))
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

/// RFC 0069 WS1 — a cheap syntactic scan for the procedure shape:
/// `true` when every `RETURN_VALUE` in `code` returns the `None`
/// constant loaded by the immediately preceding instruction and no
/// recognized jump lands on a `RETURN_VALUE` directly (something
/// other than the `None` load could flow in). Lets the embedder
/// *predict* [`crate::ir::MethodRet::None`] for callees whose bodies
/// the analyzer cannot type. The prediction is validated per call
/// (the call helper checks the actual result), so a misprediction —
/// e.g. an exception-handler edge this scan cannot see — costs a
/// deopt, never correctness.
#[must_use]
pub fn returns_none_syntactically(code: &CodeObject) -> bool {
    let n = code.instructions.len();
    let mut saw_return = false;
    for (i, ins) in code.instructions.iter().enumerate() {
        if !matches!(ins.op, OpCode::ReturnValue) {
            continue;
        }
        saw_return = true;
        let none_load = i > 0
            && matches!(code.instructions[i - 1].op, OpCode::LoadConst)
            && matches!(
                code.constants.get(code.instructions[i - 1].arg as usize),
                Some(Constant::None)
            );
        if !none_load {
            return false;
        }
    }
    if !saw_return {
        return false;
    }
    for (i, ins) in code.instructions.iter().enumerate() {
        let target = match ins.op {
            OpCode::PopJumpIfFalse
            | OpCode::PopJumpIfTrue
            | OpCode::PopJumpIfNone
            | OpCode::PopJumpIfNotNone
            | OpCode::JumpForward
            | OpCode::ForIter => Some(forward_target(i, ins.arg)),
            OpCode::JumpBackward => backward_target(i, ins.arg),
            _ => None,
        };
        if let Some(t) = target {
            if t < n && matches!(code.instructions[t].op, OpCode::ReturnValue) {
                return false;
            }
        }
    }
    true
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
            // RFC 0070 WS2 — a yield ends its block (the suspension
            // is a side exit); the continuation is a fresh block the
            // interpreter resumes into.
            OpCode::YieldValue if i + 1 < n => {
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
            // RFC 0070 WS2 — control leaves native code at a yield.
            // RFC 0071 WS5 — the continuation block is still a
            // *dataflow* successor (the yielded value replaced by the
            // sent value on the boundary stack): it compiles so a
            // resume entry can jump straight to it. `TTerm::Yield`
            // itself keeps no CFG edge — the suspension always exits.
            OpCode::YieldValue => match index_of.get(&end) {
                Some(&fall) => vec![fall],
                None => Vec::new(),
            },
            // RFC 0070 WS2 — `RERAISE` never falls through. It closes
            // the generator epilogue's unreachable `StopIterationError`
            // trailer (past the final `RETURN_VALUE`); a *reachable*
            // block ending in it is still rejected by the abstract
            // pass (unsupported opcode), exactly as before.
            OpCode::Reraise => Vec::new(),
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

/// Whether the compiled CFG contains any cycle (RFC 0070 WS2 —
/// generator profitability). Yield terminators have no successors, so
/// a cycle is always a fully native inner loop. Iterative three-color
/// DFS over the terminator edges of every block (all compiled blocks
/// are reachable by construction).
fn has_native_cycle(blocks: &[TBlock]) -> bool {
    fn succs(term: &TTerm) -> [Option<BlockId>; 2] {
        match term {
            TTerm::Jump(b) => [Some(*b), None],
            TTerm::BranchFalse {
                target,
                fallthrough,
            }
            | TTerm::BranchTrue {
                target,
                fallthrough,
            } => [Some(*target), Some(*fallthrough)],
            TTerm::ForRange { body, exit, .. }
            | TTerm::ForList { body, exit, .. }
            | TTerm::ForIter { body, exit, .. } => [Some(*body), Some(*exit)],
            TTerm::Return | TTerm::ReturnNone | TTerm::Yield { .. } => [None, None],
        }
    }
    // 0 = unvisited, 1 = on the DFS stack, 2 = done.
    let mut color = vec![0u8; blocks.len()];
    for start in 0..blocks.len() {
        if color[start] != 0 {
            continue;
        }
        // Stack of (block, next successor slot to explore).
        let mut stack: Vec<(usize, usize)> = vec![(start, 0)];
        color[start] = 1;
        while let Some(&(b, next)) = stack.last() {
            let ss = succs(&blocks[b].term);
            if next >= ss.len() {
                color[b] = 2;
                stack.pop();
                continue;
            }
            stack.last_mut().expect("non-empty DFS stack").1 += 1;
            if let Some(s) = ss[next] {
                match color[s] {
                    1 => return true,
                    0 => {
                        color[s] = 1;
                        stack.push((s, 0));
                    }
                    _ => {}
                }
            }
        }
    }
    false
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
/// function, the erased `len` builtin (lowered to `ListLen`, never a
/// real call), or an erased `math` intrinsic (RFC 0069 WS2, lowered
/// to [`TOp::MathIntrinsic`]).
#[derive(Clone, Copy, PartialEq, Eq)]
enum MarkKind {
    Py,
    Len,
    Math(MathFunc),
}

/// A `LOAD_GLOBAL`-resolved Python callee riding the *abstract* stack
/// between its load and its `CALL` (RFC 0059 WS3). The object itself
/// never reaches the native stack — the marker only carries what the
/// `CALL` site and the deopt metadata need.
#[derive(Clone, Copy, PartialEq)]
struct CalleeMark {
    kind: MarkKind,
    token: u32,
    arg_count: u32,
    /// RFC 0069 WS3 — arity minus trailing defaults; call sites
    /// passing `min_args..=arg_count` positionals are admitted.
    min_args: u32,
    is_self: bool,
    ret: Option<JitType>,
    /// The erased `LOAD_GLOBAL` pc.
    load_pc: u32,
    /// Interpreter-stack index of the callee object at load time
    /// (emission only; 0 during inference).
    interp_depth: u32,
}

/// RFC 0069 WS1 — a resolved method riding on its *receiver's* stack
/// entry between the method-form `LOAD_ATTR` and the `CALL`: the
/// receiver's pin stays the native value while the interpreter holds
/// the bound method (plus the self-or-null marker above it).
#[derive(Clone, Copy, PartialEq)]
struct MethodMark {
    token: u32,
    /// Callee arity, `self` included.
    arg_count: u32,
    /// Arity minus trailing defaults, `self` included.
    min_args: u32,
    ret: MethodRet,
    /// The method-form `LOAD_ATTR` pc.
    load_pc: u32,
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
    /// RFC 0071 WS3 — attribute-path provenance: this `Obj`-lane value
    /// was produced by walking the interned chain from a root local
    /// (an index into the analysis's [`PathArena`]).
    path: Option<u32>,
    callee: Option<CalleeMark>,
    /// `Some(load_pc)` when this is a `.append` receiver (the
    /// `LOAD_ATTR`'s pc, for the method-span deopt metadata).
    recv: Option<u32>,
    /// RFC 0069 WS1 — a pinned receiver carrying a resolved method
    /// (the native value stays the pin; the interpreter holds the
    /// bound method + self-or-null marker).
    method: Option<MethodMark>,
    /// RFC 0069 WS2 — the `math` module (`(name_idx, load_pc)`),
    /// interpreter-stack only, consumable solely by an immediately
    /// following method-form intrinsic load.
    math_mod: Option<(u32, u32)>,
    poison: bool,
    /// RFC 0068 — a `PUSH_NULL` self-or-null marker (`Unbound` on the
    /// interpreter stack, never native), consumed by its `CALL`.
    null: bool,
    /// RFC 0069 WS1 — the `None` constant (interpreter-stack only),
    /// consumable solely by `RETURN_VALUE` (the `return None` shape).
    none_const: bool,
    /// RFC 0071 WS4 — an erased `BUILD_SLICE` result
    /// (`(has_start, has_stop)`; unit step), consumable solely by an
    /// immediately following `BINARY_SUBSCR` on a pinned list. The
    /// present bounds occupy native stack slots below this marker.
    slice: Option<(bool, bool)>,
}

impl SE {
    fn known(ty: JitType) -> SE {
        SE {
            ty,
            src: None,
            path: None,
            callee: None,
            recv: None,
            method: None,
            math_mod: None,
            poison: false,
            null: false,
            none_const: false,
            slice: None,
        }
    }

    /// `true` when a plain value operation may consume this entry.
    fn is_plain(&self) -> bool {
        self.callee.is_none()
            && self.recv.is_none()
            && self.method.is_none()
            && self.math_mod.is_none()
            && !self.poison
            && !self.null
            && !self.none_const
            && self.slice.is_none()
    }

    /// `true` when this entry may cross a basic-block boundary
    /// (RFC 0069 WS2): plain values and the marker kinds whose
    /// reconstruction metadata is position-independent. `poison`,
    /// `none_const`, the slice marker, and the math-module marker
    /// never cross (their consumers are required to be adjacent).
    fn boundary_ok(&self) -> bool {
        !self.poison && !self.none_const && self.math_mod.is_none() && self.slice.is_none()
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

/// The widest boundary stack the analyzer carries across a block edge
/// (RFC 0069 WS2). Deopt spill buffers grow with it; no real
/// predicate/ternary shape comes close to the cap.
const MAX_BOUNDARY_STACK: usize = 8;

/// Validate a stack about to cross a block boundary and hand it to
/// the caller for the successor merge.
fn boundary_stack(stack: Vec<SE>) -> Result<Vec<SE>, JitVerdict> {
    if stack.len() > MAX_BOUNDARY_STACK {
        return Err(JitVerdict::NonEmptyBoundaryStack);
    }
    if !stack.iter().all(SE::boundary_ok) {
        return Err(JitVerdict::NonEmptyBoundaryStack);
    }
    Ok(stack)
}

/// Infer/validate one block during the fixpoint, starting from its
/// merged entry stack. Mutates `local_types` (setting `changed` when
/// it grows), bails on hard errors, and returns the per-successor
/// boundary stacks. Transient `Unknown` operands are tolerated — a
/// later iteration may resolve them.
#[allow(clippy::too_many_arguments)]
fn infer_block(
    code: &CodeObject,
    b: &RawBlock,
    plan: &Plan,
    entry: Vec<SE>,
    local_types: &mut [Option<JitType>],
    ret: &mut RetInfo,
    changed: &mut bool,
    probes: &mut Probes<'_>,
) -> Result<Vec<(usize, Vec<SE>)>, JitVerdict> {
    let mut stack: Vec<SE> = entry;
    for i in b.start..(b.end - 1) {
        step_abstract(
            code,
            i,
            &mut stack,
            plan,
            local_types,
            ret.final_lane(),
            changed,
            probes,
        )?;
    }
    // Terminator stack-shape validation + successor boundary stacks.
    let last = b.end - 1;
    let ins = code.instructions[last];
    let outs = match ins.op {
        OpCode::ReturnValue => {
            let v = *stack.last().ok_or(JitVerdict::StackUnderflow)?;
            if v.none_const {
                // RFC 0069 WS1 — `return None` (incl. the implicit
                // function-tail return).
                if !ret.saw_none {
                    ret.saw_none = true;
                    *changed = true;
                }
            } else {
                if !v.is_plain() {
                    return Err(JitVerdict::UnsupportedOpcode("CALL (callee escapes)"));
                }
                if !ret.saw_scalar {
                    ret.saw_scalar = true;
                    *changed = true;
                }
                merge_ret_lane(&mut ret.lane, v.ty, changed);
            }
            Vec::new()
        }
        OpCode::JumpForward | OpCode::JumpBackward => {
            vec![(b.succs[0], boundary_stack(stack)?)]
        }
        // A rewritten range/list header operates purely on its
        // synthetic slots; the operand stack must be empty (the
        // interpreter holds only the erased iterator there).
        OpCode::ForIter => {
            if !stack.is_empty() {
                return Err(JitVerdict::NonEmptyBoundaryStack);
            }
            // RFC 0071 WS4 — a list loop's variable wears the seq
            // slot's element lane (the seq slot is typed by the
            // dominating `GET_ITER`; until the fixpoint reaches it,
            // contribute nothing). An *opaque* iterator (seq slot on
            // the object lane) yields `Int` elements in v1 — the step
            // helper re-validates each element and a lane surprise
            // deopts with the element preserved; a body that needs a
            // different lane conflicts here and stays interpreted.
            if let Some(&(seq_slot, _idx, var)) = plan.iter_headers.get(&last) {
                if let Some(lane) = local_types.get(seq_slot as usize).copied().flatten() {
                    if lane == JitType::Obj {
                        set_local(local_types, var, JitType::Int, changed)?;
                    } else {
                        let elem = lane.elem_lane().ok_or(JitVerdict::UnsupportedOpcode(
                            "FOR_ITER (unsupported iterable)",
                        ))?;
                        set_local(local_types, var, elem, changed)?;
                    }
                }
            }
            vec![(b.succs[0], Vec::new()), (b.succs[1], Vec::new())]
        }
        OpCode::PopJumpIfFalse | OpCode::PopJumpIfTrue => {
            let c = stack.pop().ok_or(JitVerdict::StackUnderflow)?;
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
            let out = boundary_stack(stack)?;
            vec![(b.succs[0], out.clone()), (b.succs[1], out)]
        }
        // RFC 0070 WS2 — a yield is an unconditional deopt-shaped
        // exit re-executing `YIELD_VALUE` in the interpreter: the
        // yielded value must exist on the *native* stack for the
        // spill (a `None` constant materializes through `PushNone`
        // at emission). RFC 0071 WS5 — the continuation block flows
        // as a dataflow successor: the yielded value replaced by the
        // sent value on the *object* lane (a resume entry provides
        // it; `next()`'s `None` rides as `-1`).
        OpCode::YieldValue => {
            if !code.is_generator {
                return Err(JitVerdict::UnsupportedOpcode(
                    "YIELD_VALUE (non-generator shape)",
                ));
            }
            let v = *stack.last().ok_or(JitVerdict::StackUnderflow)?;
            if !v.none_const {
                if !v.is_plain() {
                    return Err(JitVerdict::UnsupportedOpcode(
                        "YIELD_VALUE (marker operand)",
                    ));
                }
                if !v.ty.is_representable() && v.src.is_none() {
                    return Err(JitVerdict::TypeUnknown);
                }
            }
            match b.succs.first() {
                Some(&succ) => {
                    stack.pop();
                    stack.push(SE::known(JitType::Obj));
                    vec![(succ, boundary_stack(stack)?)]
                }
                None => Vec::new(),
            }
        }
        // Fall-through terminator: the remaining stack flows to the
        // lone successor.
        _ => {
            step_abstract(
                code,
                last,
                &mut stack,
                plan,
                local_types,
                ret.final_lane(),
                changed,
                probes,
            )?;
            vec![(b.succs[0], boundary_stack(stack)?)]
        }
    };
    Ok(outs)
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
    // RFC 0071 WS4 — performed by the `ForList` terminator; the
    // variable's lane comes from the header's element lane (typed at
    // the `FOR_ITER` inference), not here.
    if plan.fused_store_iter.contains_key(&i) {
        return Ok(());
    }
    match ins.op {
        OpCode::Nop | OpCode::Resume => {}
        // RFC 0071 WS4 — a recognized list-loop `GET_ITER`: pop the
        // iterable (it must be, or probe as, a pinned list) into the
        // seq synthetic slot and type the index slot. Anything
        // non-list disqualifies the frame here.
        OpCode::GetIter => {
            let &(seq_slot, idx_slot) = plan
                .get_iter
                .get(&i)
                .ok_or(JitVerdict::UnsupportedOpcode("GET_ITER (unplanned)"))?;
            let v = stack.pop().ok_or(JitVerdict::StackUnderflow)?;
            if !v.is_plain() {
                return Err(JitVerdict::UnsupportedOpcode("GET_ITER (marker operand)"));
            }
            // RFC 0071 WS4 — an object-lane iterable takes the
            // *opaque-iterator* path: the seq slot wears `Obj` (the
            // runtime capture verifies `iter(x) is x` and deopts
            // otherwise) and the idx slot stays unused.
            if v.ty == JitType::Obj {
                set_local(local_types, seq_slot, JitType::Obj, changed)?;
                return Ok(());
            }
            let lane = if v.ty.is_list() {
                v.ty
            } else if !v.ty.is_representable() {
                // A list shape wins when the live value probes as one;
                // a generator/iterator parameter probes onto the
                // object lane instead.
                match resolve_list_container(&v, local_types, changed, probes) {
                    Ok(Some(elem)) => JitType::list_of(elem)
                        .ok_or(JitVerdict::UnsupportedOpcode("FOR_ITER (iterable lane)"))?,
                    Ok(None) => return Err(JitVerdict::TypeUnknown),
                    Err(e) => {
                        if let Some(slot) = v.src {
                            if (probes.param)(slot) == Some(JitType::Obj) {
                                set_local(local_types, slot, JitType::Obj, changed)?;
                                set_local(local_types, seq_slot, JitType::Obj, changed)?;
                                return Ok(());
                            }
                            // The param probe is a no-op on the
                            // unseeded pass: surface `TypeUnknown` so
                            // the seeded retry gets to classify the
                            // slot (an identity-iterable argument
                            // takes the opaque path above; anything
                            // else rejects there for real).
                            return Err(JitVerdict::TypeUnknown);
                        }
                        return Err(e);
                    }
                }
            } else {
                return Err(JitVerdict::UnsupportedOpcode(
                    "FOR_ITER (unsupported iterable)",
                ));
            };
            set_local(local_types, seq_slot, lane, changed)?;
            set_local(local_types, idx_slot, JitType::Int, changed)?;
        }
        OpCode::LoadGlobal => {
            let ty = match plan.globals.get(&ins.arg) {
                Some(ResolvedGlobal::ConstInt(_)) => JitType::Int,
                Some(ResolvedGlobal::ConstFloat(_)) => JitType::Float,
                Some(ResolvedGlobal::ConstBool(_)) => JitType::Bool,
                Some(&ResolvedGlobal::PyFunc {
                    token,
                    arg_count,
                    min_args,
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
                            min_args,
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
                            min_args: 1,
                            is_self: false,
                            ret: Some(JitType::Int),
                            load_pc: i as u32,
                            interp_depth: 0,
                        }),
                        ..SE::known(JitType::Unknown)
                    });
                    return Ok(());
                }
                // RFC 0069 WS2: the math module rides the abstract
                // stack until the immediately following method-form
                // intrinsic load consumes it.
                Some(ResolvedGlobal::MathModule) => {
                    stack.push(SE {
                        math_mod: Some((ins.arg, i as u32)),
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
            // RFC 0069 WS1 — the `None` constant is admitted as a
            // marker whose only legal consumer is `RETURN_VALUE`.
            if matches!(c, Constant::None) {
                stack.push(SE {
                    none_const: true,
                    ..SE::known(JitType::Unknown)
                });
                return Ok(());
            }
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
        // RFC 0070 WS2 — the generator prologue. The interpreted
        // bootstrap executes this once (creating the generator and
        // parking the frame); the *first resume* pushes the sent
        // `None` that the following `POP_TOP` discards. Modeling it
        // as an Obj-lane `None` push keeps abstract flow (and thus
        // loop-header typing for the OSR entries) alive through the
        // prologue. Native code never actually enters here — the
        // embedder gates fresh pc-0 entries off generator code.
        OpCode::ReturnGenerator => {
            if !code.is_generator {
                return Err(JitVerdict::UnsupportedOpcode(
                    "RETURN_GENERATOR (non-generator shape)",
                ));
            }
            stack.push(SE::known(JitType::Obj));
        }
        OpCode::StoreFast => {
            let v = stack.pop().ok_or(JitVerdict::StackUnderflow)?;
            // RFC 0070 WS1 — `x = None` stores the unboxed `None` into
            // a nullable object-lane local (machine value `-1`).
            if v.none_const {
                set_local(local_types, ins.arg, JitType::Obj, changed)?;
                return Ok(());
            }
            if !v.is_plain() {
                return Err(JitVerdict::UnsupportedOpcode("CALL (callee escapes)"));
            }
            if v.ty.is_representable() {
                set_local(local_types, ins.arg, v.ty, changed)?;
            } else if let Some(src) = v.src {
                // RFC 0070 WS1 — back-propagate a known destination
                // lane to an untyped source local (`cur = head` where
                // `cur` is typed by later uses): the copy asserts both
                // slots share one lane, and a conflict rejects the
                // frame exactly like a forward mismatch.
                if let Some(dst_ty) = local_types.get(ins.arg as usize).copied().flatten() {
                    set_local(local_types, src, dst_ty, changed)?;
                }
            }
        }
        OpCode::BinaryOp => {
            let kind = bin_kind(ins.arg)?;
            let b = stack.pop().ok_or(JitVerdict::StackUnderflow)?;
            let a = stack.pop().ok_or(JitVerdict::StackUnderflow)?;
            if !a.is_plain() || !b.is_plain() {
                return Err(JitVerdict::UnsupportedOpcode("CALL (callee escapes)"));
            }
            // RFC 0071 WS4 — `list * int` repeats a pinned list on the
            // same lane (`[None] * n`, padding shapes).
            if matches!(kind, ArithKind::Mul) && a.ty.is_list() {
                check_subscr_index(&b, local_types, changed)?;
                stack.push(SE::known(a.ty));
                return Ok(());
            }
            let (a, b) = resolve_pair(a, b, local_types, changed);
            let res = bin_result_type(kind, a.ty, b.ty)?;
            stack.push(SE::known(res));
        }
        OpCode::CompareOp => {
            let kind = cmp_kind(ins.arg)?;
            let b = stack.pop().ok_or(JitVerdict::StackUnderflow)?;
            let a = stack.pop().ok_or(JitVerdict::StackUnderflow)?;
            if !a.is_plain() || !b.is_plain() {
                return Err(JitVerdict::UnsupportedOpcode("CALL (callee escapes)"));
            }
            let (a, b) = resolve_pair(a, b, local_types, changed);
            cmp_check(kind, a.ty, b.ty)?;
            stack.push(SE::known(JitType::Bool));
        }
        // RFC 0070 WS1 — `x is None` / `x is not None` on a nullable
        // object lane. Exactly one operand must be the `None` constant
        // (interpreter-stack only); the other must be (or infer as) an
        // `Obj`-lane value. Lowers to the native `IsNone` fence.
        OpCode::IsOp => {
            if ins.arg > 1 {
                return Err(JitVerdict::UnsupportedOpcode("IS_OP kind"));
            }
            let b = stack.pop().ok_or(JitVerdict::StackUnderflow)?;
            let a = stack.pop().ok_or(JitVerdict::StackUnderflow)?;
            let val = match (a.none_const, b.none_const) {
                (false, true) => a,
                (true, false) => b,
                _ => return Err(JitVerdict::UnsupportedOpcode("IS_OP shape")),
            };
            if !val.is_plain() {
                return Err(JitVerdict::UnsupportedOpcode("CALL (callee escapes)"));
            }
            match val.ty {
                JitType::Obj => {}
                JitType::Unknown => {
                    if let Some(slot) = val.src {
                        set_local(local_types, slot, JitType::Obj, changed)?;
                    }
                    // No provenance: transient — a later iteration may
                    // type it; emission bails if it never resolves.
                }
                _ => return Err(JitVerdict::UnsupportedOpcode("IS_OP operand lane")),
            }
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
        // CPython 3.13's `TO_BOOL` — the exact-bool coercion inserted
        // before a branch (or `not`) on a non-bool operand. Scalar
        // lanes only; a transient `Unknown` is tolerated for a later
        // iteration.
        OpCode::ToBool => {
            let a = stack.pop().ok_or(JitVerdict::StackUnderflow)?;
            if !a.is_plain() {
                return Err(JitVerdict::UnsupportedOpcode("CALL (callee escapes)"));
            }
            if a.ty.is_representable()
                && !matches!(a.ty, JitType::Int | JitType::Float | JitType::Bool)
            {
                return Err(JitVerdict::UnsupportedOpcode("TO_BOOL lane"));
            }
            stack.push(SE::known(JitType::Bool));
        }
        // RFC 0065 WS5 — attribute load: either the erased `.append`
        // method load on a pinned list (the receiver stays on the
        // abstract stack, re-marked), or a scalar attribute read on a
        // pinned instance receiver. RFC 0069 WS2 — a `math.<intrinsic>`
        // load consumes the module marker (the compiler pairs the
        // plain form with an explicit `PUSH_NULL`, so no implicit null
        // is pushed here).
        OpCode::LoadAttr => {
            let name = code
                .names
                .get(ins.arg as usize)
                .ok_or(JitVerdict::UnsupportedOpcode("LOAD_ATTR bad name"))?
                .as_str();
            let recv = stack.pop().ok_or(JitVerdict::StackUnderflow)?;
            if let Some((name_idx, load_pc)) = recv.math_mod {
                infer_math_load(code, name, name_idx, load_pc, stack, probes)?;
                return Ok(());
            }
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
        // RFC 0069 adds the `math.<intrinsic>` (WS2) and resolved
        // instance-method (WS1) forms.
        OpCode::LoadMethodAttr => {
            let name = code
                .names
                .get(ins.arg as usize)
                .ok_or(JitVerdict::UnsupportedOpcode("LOAD_ATTR bad name"))?
                .as_str();
            let recv = stack.pop().ok_or(JitVerdict::StackUnderflow)?;
            // RFC 0069 WS2 — the math-module marker's only legal
            // consumer: an intrinsic load, riding on as a callee mark
            // (with the method form's implicit self-or-null on top).
            if let Some((name_idx, load_pc)) = recv.math_mod {
                infer_math_load(code, name, name_idx, load_pc, stack, probes)?;
                stack.push(SE {
                    null: true,
                    ..SE::known(JitType::Unknown)
                });
                return Ok(());
            }
            if !recv.is_plain() {
                return Err(JitVerdict::UnsupportedOpcode("CALL (callee escapes)"));
            }
            // RFC 0069 WS1 — a class-resolved method on a pinned
            // instance receiver: the receiver stays on the abstract
            // stack (its pin is the native value), re-marked with the
            // resolution. `.append` keeps its dedicated list path.
            // RFC 0071 WS3 admits attribute-chain receivers.
            if name != "append" {
                if let Some((slot, path)) = obj_recv_ref(recv.ty, recv.src, recv.path, probes.paths)
                {
                    let names = probes.paths.names(path);
                    if let Some(res) = (probes.method)(slot, &names, name) {
                        if path.is_none() {
                            set_local(local_types, slot, JitType::Obj, changed)?;
                        }
                        stack.push(SE {
                            ty: JitType::Obj,
                            method: Some(MethodMark {
                                token: res.token,
                                arg_count: res.arg_count,
                                min_args: res.min_args,
                                ret: res.ret,
                                load_pc: i as u32,
                            }),
                            ..recv
                        });
                        stack.push(SE {
                            null: true,
                            ..SE::known(JitType::Unknown)
                        });
                        return Ok(());
                    }
                }
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
            if !recv.is_plain() {
                return Err(JitVerdict::UnsupportedOpcode("CALL (callee escapes)"));
            }
            // RFC 0070 WS1 — `x.attr = None` is legal on an object-lane
            // site (the unboxed `None` is the machine value `-1`).
            if !val.is_plain() && !val.none_const {
                return Err(JitVerdict::UnsupportedOpcode("CALL (callee escapes)"));
            }
            let Some((slot, path)) = obj_recv_ref(recv.ty, recv.src, recv.path, probes.paths)
            else {
                if !recv.ty.is_representable() {
                    // Transient — a later iteration may type it.
                    return Ok(());
                }
                return Err(JitVerdict::UnsupportedOpcode("STORE_ATTR receiver"));
            };
            let names = probes.paths.names(path);
            let lane = (probes.attr)(slot, &names, name, true)
                .ok_or(JitVerdict::UnsupportedOpcode("STORE_ATTR shape"))?;
            // RFC 0071 WS6 is read-only: `str`/`bytes`-lane attribute
            // *stores* stay interpreted.
            if matches!(lane, JitType::Str | JitType::Bytes) {
                return Err(JitVerdict::UnsupportedOpcode("STORE_ATTR (value lane)"));
            }
            if path.is_none() {
                set_local(local_types, slot, JitType::Obj, changed)?;
            }
            // RFC 0071 WS2 — an `Unknown` store lane is the *new-key*
            // shape (the constructor pattern): the attribute doesn't
            // exist yet, so the stored value's own lane defines the
            // site. Any marshalable lane is admissible.
            if lane == JitType::Unknown {
                if !val.none_const
                    && val.ty.is_representable()
                    && val.ty.is_pinned()
                    && val.ty != JitType::Obj
                {
                    return Err(JitVerdict::UnsupportedOpcode("STORE_ATTR (value lane)"));
                }
            } else if val.none_const {
                if lane != JitType::Obj {
                    return Err(JitVerdict::UnsupportedOpcode("STORE_ATTR (value lane)"));
                }
            } else if val.ty.is_representable() {
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
            // RFC 0069 WS1 — a resolved method call: `self` rides as
            // the receiver's pin, so the effective arity is `argc + 1`.
            if let Some(m) = f.method {
                let total = argc + 1;
                if total < m.min_args as usize || total > m.arg_count as usize {
                    return Err(JitVerdict::UnsupportedOpcode("CALL (method arity)"));
                }
                match m.ret {
                    // Procedure shape: the `None` result exists only on
                    // the interpreter stack (consumed by `POP_TOP`).
                    MethodRet::None => stack.push(SE {
                        poison: true,
                        ..SE::known(JitType::Unknown)
                    }),
                    MethodRet::Scalar(ty) => stack.push(SE::known(ty)),
                }
                return Ok(());
            }
            let Some(mark) = f.callee else {
                return Err(JitVerdict::UnsupportedOpcode("CALL"));
            };
            // RFC 0069 WS2 — a burned-in math intrinsic: one operand,
            // `float` (an integral operand promotes with the guarded
            // exact-range conversion, like mixed arithmetic).
            if let MarkKind::Math(_) = mark.kind {
                if argc != 1 {
                    return Err(JitVerdict::UnsupportedOpcode("math (arity)"));
                }
                let a = &args[0];
                if a.ty.is_representable() {
                    if !(a.ty == JitType::Float || a.ty.is_integral()) {
                        return Err(JitVerdict::UnsupportedOpcode("math (operand lane)"));
                    }
                }
                // Untyped-with-src stays transient: the operand could
                // legitimately be int or float, so nothing is pinned
                // here; emission bails if it never resolves.
                stack.push(SE::known(JitType::Float));
                return Ok(());
            }
            // Positional arity within the defaults window (RFC 0069
            // WS3): `min_args..=arg_count` positionals are admitted —
            // the call helper binds the snapshotted defaults for the
            // tail. Anything else disqualifies.
            if argc < mark.min_args as usize || argc > mark.arg_count as usize {
                return Err(JitVerdict::UnsupportedOpcode("CALL (arity)"));
            }
            if mark.kind == MarkKind::Len {
                // `len(x)` on a pinned list (or — RFC 0071 WS6 — a
                // pinned `str`/`bytes`) → an `int`, no real call.
                let arg = &args[0];
                if arg.ty.is_list() || matches!(arg.ty, JitType::Str | JitType::Bytes) {
                    // fine
                } else if !arg.ty.is_representable() {
                    if let Some(slot) = arg.src {
                        // Not a list either — fail as `TypeUnknown` so
                        // the seeded retry can type a `str`/`bytes`
                        // *parameter* (RFC 0071 WS6); a genuinely
                        // unsupported shape fails the retry too.
                        let elem = (probes.list)(slot).ok_or(JitVerdict::TypeUnknown)?;
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
            // RFC 0071 WS4 — `xs[a:b]` through an erased `BUILD_SLICE`
            // marker: the result is a fresh list on the *same* lane.
            if idx.slice.is_some() {
                if !cont.is_plain() {
                    return Err(JitVerdict::UnsupportedOpcode("CALL (callee escapes)"));
                }
                let elem = resolve_list_container(&cont, local_types, changed, probes)?
                    .ok_or(JitVerdict::TypeUnknown)?;
                let lane = JitType::list_of(elem)
                    .ok_or(JitVerdict::UnsupportedOpcode("slice container lane"))?;
                stack.push(SE::known(lane));
                return Ok(());
            }
            if !idx.is_plain() || !cont.is_plain() {
                return Err(JitVerdict::UnsupportedOpcode("CALL (callee escapes)"));
            }
            check_subscr_index(&idx, local_types, changed)?;
            // RFC 0071 WS6 — `bytes[i]` reads a byte as an `Int`.
            if cont.ty == JitType::Bytes {
                stack.push(SE::known(JitType::Int));
                return Ok(());
            }
            let elem = resolve_list_container(&cont, local_types, changed, probes)?;
            stack.push(match elem {
                Some(l) => SE::known(l),
                None => SE::known(JitType::Unknown),
            });
        }
        // RFC 0071 WS4 — `BUILD_LIST k` with uniform-lane elements (or
        // all-`None`, the `[None] * n` seed): a fresh pinned list.
        // `BUILD_LIST 0` has no lane evidence and stays interpreted.
        OpCode::BuildList => {
            let k = ins.arg as usize;
            if k == 0 || k > 16 {
                return Err(JitVerdict::UnsupportedOpcode("BUILD_LIST (shape)"));
            }
            let base = stack
                .len()
                .checked_sub(k)
                .ok_or(JitVerdict::StackUnderflow)?;
            let elems: Vec<SE> = stack.drain(base..).collect();
            if elems.iter().all(|e| e.none_const) {
                stack.push(SE::known(JitType::ListObj));
                return Ok(());
            }
            let mut lane: Option<JitType> = None;
            for e in &elems {
                if !e.is_plain() {
                    return Err(JitVerdict::UnsupportedOpcode("BUILD_LIST (marker element)"));
                }
                if !e.ty.is_representable() {
                    // Transient — a later iteration may type it.
                    stack.push(SE::known(JitType::Unknown));
                    return Ok(());
                }
                match lane {
                    None => lane = Some(e.ty),
                    Some(l) if l == e.ty => {}
                    Some(_) => {
                        return Err(JitVerdict::UnsupportedOpcode("BUILD_LIST (mixed lanes)"))
                    }
                }
            }
            let elem = lane.ok_or(JitVerdict::TypeUnknown)?;
            let list = JitType::list_of(elem)
                .ok_or(JitVerdict::UnsupportedOpcode("BUILD_LIST (element lane)"))?;
            stack.push(SE::known(list));
        }
        // RFC 0071 WS4 — `BUILD_SLICE` with a `None` step: erased into
        // a marker consumable only by the immediately following
        // `BINARY_SUBSCR` on a pinned list. Bounds are `int`s or the
        // `None` constant.
        OpCode::BuildSlice => {
            if ins.arg != 3 && ins.arg != 2 {
                return Err(JitVerdict::UnsupportedOpcode("BUILD_SLICE (shape)"));
            }
            if ins.arg == 3 {
                let step = stack.pop().ok_or(JitVerdict::StackUnderflow)?;
                if !step.none_const {
                    return Err(JitVerdict::UnsupportedOpcode("BUILD_SLICE (step)"));
                }
            }
            let stop = stack.pop().ok_or(JitVerdict::StackUnderflow)?;
            let start = stack.pop().ok_or(JitVerdict::StackUnderflow)?;
            let mut bound = |e: &SE| -> Result<bool, JitVerdict> {
                if e.none_const {
                    return Ok(false);
                }
                if !e.is_plain() {
                    return Err(JitVerdict::UnsupportedOpcode("BUILD_SLICE (bound)"));
                }
                check_subscr_index(e, local_types, changed)?;
                Ok(true)
            };
            let has_start = bound(&start)?;
            let has_stop = bound(&stop)?;
            stack.push(SE {
                slice: Some((has_start, has_stop)),
                ..SE::known(JitType::Unknown)
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

/// RFC 0065 WS5 / RFC 0071 WS3 — the receiver reference of an
/// attribute access: `(root slot, interned path)` for an `Obj`-lane
/// (or as-yet-untyped) receiver with local or attribute-chain
/// provenance. `None` when the receiver has neither, or wears an
/// incompatible concrete lane.
fn obj_recv_ref(
    ty: JitType,
    src: Option<u32>,
    path: Option<u32>,
    arena: &PathArena,
) -> Option<(u32, Option<u32>)> {
    if !matches!(ty, JitType::Obj | JitType::Unknown) {
        return None;
    }
    if let Some(p) = path {
        return Some((arena.root(p), Some(p)));
    }
    src.map(|s| (s, None))
}

/// RFC 0069 WS2 — infer a `math.<intrinsic>` load: the module marker
/// was popped by the caller; validate the attribute against
/// [`MathFunc`] and the embedder's probe, and push the callee mark
/// (the caller adds the self-or-null marker in the method form; the
/// plain form's explicit `PUSH_NULL` follows in the bytecode).
fn infer_math_load(
    code: &CodeObject,
    name: &str,
    name_idx: u32,
    load_pc: u32,
    stack: &mut Vec<SE>,
    probes: &mut Probes<'_>,
) -> Result<(), JitVerdict> {
    let global_name = code
        .names
        .get(name_idx as usize)
        .ok_or(JitVerdict::UnsupportedOpcode("LOAD_GLOBAL bad name"))?
        .as_str();
    let func = MathFunc::from_attr(name).ok_or(JitVerdict::UnsupportedOpcode("math (attr)"))?;
    if !(probes.math)(global_name, name) {
        return Err(JitVerdict::UnsupportedOpcode("math (shape)"));
    }
    stack.push(SE {
        callee: Some(CalleeMark {
            kind: MarkKind::Math(func),
            token: name_idx,
            arg_count: 1,
            min_args: 1,
            is_self: false,
            ret: Some(JitType::Float),
            load_pc,
            interp_depth: 0,
        }),
        ..SE::known(JitType::Unknown)
    });
    Ok(())
}

/// RFC 0069 WS2 — [`infer_math_load`]'s emission twin: validate the
/// pair again (the probe must agree with inference — nothing ran in
/// between), intern the guard token, and push the callee mark.
#[allow(clippy::too_many_arguments)]
fn emit_math_load(
    code: &CodeObject,
    name: &str,
    name_idx: u32,
    load_pc: u32,
    interp_depth: u32,
    stack: &mut Vec<ESlot>,
    math_guards: &mut Vec<MathGuardMeta>,
    probes: &mut Probes<'_>,
) -> Result<(), JitVerdict> {
    let global_name = code
        .names
        .get(name_idx as usize)
        .ok_or(JitVerdict::UnsupportedOpcode("LOAD_GLOBAL bad name"))?
        .as_str();
    let func = MathFunc::from_attr(name).ok_or(JitVerdict::UnsupportedOpcode("math (attr)"))?;
    if !(probes.math)(global_name, name) {
        return Err(JitVerdict::UnsupportedOpcode("math (shape)"));
    }
    let token = match math_guards
        .iter()
        .position(|g| g.name == global_name && g.attr == name)
    {
        Some(idx) => idx as u32,
        None => {
            math_guards.push(MathGuardMeta {
                name: global_name.to_owned(),
                attr: name.to_owned(),
                kind: func,
            });
            (math_guards.len() - 1) as u32
        }
    };
    stack.push(ESlot {
        callee: Some(CalleeMark {
            kind: MarkKind::Math(func),
            token,
            arg_count: 1,
            min_args: 1,
            is_self: false,
            ret: Some(JitType::Float),
            load_pc,
            interp_depth,
        }),
        ..ESlot::val(JitType::Unknown)
    });
    Ok(())
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
    let Some((slot, path)) = obj_recv_ref(recv.ty, recv.src, recv.path, probes.paths) else {
        if !recv.ty.is_representable() {
            // Transient — tolerate; emission bails if never resolved.
            stack.push(SE::known(JitType::Unknown));
            return Ok(());
        }
        return Err(JitVerdict::UnsupportedOpcode("LOAD_ATTR receiver"));
    };
    let names = probes.paths.names(path);
    let Some(lane) = (probes.attr)(slot, &names, name, false) else {
        return Err(JitVerdict::UnsupportedOpcode("LOAD_ATTR shape"));
    };
    if lane == JitType::Unknown {
        return Err(JitVerdict::UnsupportedOpcode("LOAD_ATTR shape"));
    }
    if path.is_none() {
        set_local(local_types, slot, JitType::Obj, changed)?;
    }
    // RFC 0071 WS3 — an object-lane result extends the provenance
    // chain, so a later access through it can be probed (a chain past
    // the depth cap simply carries no provenance).
    let out_path = if lane == JitType::Obj {
        probes.paths.seg(path, slot, name)
    } else {
        None
    };
    stack.push(SE {
        path: out_path,
        ..SE::known(lane)
    });
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
    } else if a == JitType::Float && b == JitType::Float {
        // Float∘float: RFC 0069 WS2 adds floor-div / mod (lowered
        // through the Python-semantics helpers with a zero-divisor
        // deopt); bit ops stay illegal on floats.
        match kind {
            ArithKind::Add
            | ArithKind::Sub
            | ArithKind::Mul
            | ArithKind::TrueDiv
            | ArithKind::FloorDiv
            | ArithKind::Mod => Ok(JitType::Float),
            _ => Err(JitVerdict::UnsupportedOpcode("float bitop")),
        }
    } else if a == JitType::Float || b == JitType::Float {
        // Mixed integral/float (RFC 0058 WS4): the integral operand is
        // promoted with the same `as f64` cast the interpreter
        // applies, so only the unguarded-promotion op set is legal.
        match kind {
            ArithKind::Add | ArithKind::Sub | ArithKind::Mul | ArithKind::TrueDiv => {
                Ok(JitType::Float)
            }
            _ => Err(JitVerdict::UnsupportedOpcode("mixed floordiv/mod/bitop")),
        }
    } else {
        Err(JitVerdict::MixedArithTypes)
    }
}

/// Validate comparison operand lanes. Same-lane always works; mixed
/// integral/float works via a *guarded* promotion (the interpreter
/// compares exactly, so the JIT deopts when the int exceeds ±2^53).
fn cmp_check(kind: CmpKind, a: JitType, b: JitType) -> Result<(), JitVerdict> {
    if !a.is_representable() || !b.is_representable() {
        return Ok(());
    }
    if (a.is_integral() || a == JitType::Float) && (b.is_integral() || b == JitType::Float) {
        return Ok(());
    }
    // RFC 0071 WS6 — `str` equality (only) rides the pinned lane.
    if a == JitType::Str && b == JitType::Str && matches!(kind, CmpKind::Eq | CmpKind::Ne) {
        return Ok(());
    }
    Err(JitVerdict::MixedArithTypes)
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
    // RFC 0069: augmented assignments (`x += y`) set BINARY_OP_INPLACE_FLAG
    // on the same operator byte. Every lane the JIT actually lowers
    // arithmetic for is an immutable scalar (int / float / bool — anything
    // else is rejected by `bin_result_type` / `lower_bin`), and immutable
    // scalars have no `__iadd__`: the interpreter's in-place dispatch falls
    // back to the plain operator. Stripping the flag is therefore exact.
    let k = match arg & !BINARY_OP_INPLACE_FLAG {
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
/// `POP_TOP`). RFC 0069 adds `method` (WS1 — a receiver pin carrying
/// its resolved method), `math_mod` (WS2 — the erased `math` module),
/// and `none_const` (WS1 — the `None` constant feeding a
/// `RETURN_VALUE`).
#[derive(Clone, Copy)]
struct ESlot {
    ty: JitType,
    callee: Option<CalleeMark>,
    src: Option<u32>,
    /// RFC 0071 WS3 — attribute-path provenance (see [`SE::path`]).
    path: Option<u32>,
    recv: Option<u32>,
    /// RFC 0069 WS1 — a pinned receiver re-marked with its resolved
    /// method between the method-form load and the `CALL`.
    method: Option<MethodMark>,
    /// RFC 0069 WS2 — the erased `math` module:
    /// `(name_idx, load_pc, interp_depth)`. Interpreter-stack only,
    /// consumed by the immediately following intrinsic load.
    math_mod: Option<(u32, u32, u32)>,
    poison: bool,
    /// RFC 0068 — a `PUSH_NULL` self-or-null marker (`Unbound` on the
    /// interpreter stack, never native), consumed by its `CALL`.
    null: bool,
    /// RFC 0069 WS1 — the `None` constant, consumable solely by
    /// `RETURN_VALUE`.
    none_const: bool,
    /// RFC 0071 WS4 — an erased `BUILD_SLICE` result (see
    /// [`SE::slice`]).
    slice: Option<(bool, bool)>,
}

impl ESlot {
    fn val(ty: JitType) -> ESlot {
        ESlot {
            ty,
            callee: None,
            src: None,
            path: None,
            recv: None,
            method: None,
            math_mod: None,
            poison: false,
            null: false,
            none_const: false,
            slice: None,
        }
    }

    fn is_plain(&self) -> bool {
        self.callee.is_none()
            && self.recv.is_none()
            && self.method.is_none()
            && self.math_mod.is_none()
            && !self.poison
            && !self.null
            && !self.none_const
            && self.slice.is_none()
    }

    /// `true` when this entry occupies a *native* stack position
    /// (plain values, plus the `.append`/method receiver pins). The
    /// other marker kinds live only on the interpreter's stack.
    fn has_native(&self) -> bool {
        self.is_plain() || self.recv.is_some() || self.method.is_some()
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
    /// RFC 0065 WS5 / RFC 0069 WS1 — erased bound-method receivers
    /// (`.append` and burned-in method sites).
    method_spans: Vec<MethodSpanMeta>,
    /// RFC 0065 WS5 — burned-in attribute-access sites, in `site`
    /// token order.
    attr_sites: Vec<AttrSiteMeta>,
    /// RFC 0069 WS1 — burned-in method-call sites, indexed by the
    /// embedder's probe token (a gap means a token the probe issued
    /// but no surviving site uses — rejected after emission).
    method_sites: Vec<Option<MethodSiteMeta>>,
    /// RFC 0069 WS2 — math-intrinsic guards, deduplicated by
    /// `(name, attr)`, in first-use order ([`TOp::MathIntrinsic`]
    /// spans index this).
    math_guards: Vec<MathGuardMeta>,
    /// RFC 0069 WS2 — erased math-intrinsic callables (`token`
    /// indexes [`Self::math_guards`], `live_to` = pc after the
    /// `CALL`).
    math_spans: Vec<CalleeSpanMeta>,
    max_call_args: u32,
}

/// Seed a successor's emission entry stack (first predecessor wins —
/// the inference fixpoint already proved every in-edge agrees on the
/// boundary shape, so later predecessors are redundant).
fn seed_entry(emit_entries: &mut [Option<Vec<ESlot>>], succ: usize, stack: &[ESlot]) {
    if emit_entries[succ].is_none() {
        emit_entries[succ] = Some(stack.to_vec());
    }
}

/// Emit the typed IR for one block, starting from its (propagated)
/// entry stack, with all local types now known. Seeds each successor's
/// entry stack in `emit_entries` (RFC 0069 WS2 — boundary values).
#[allow(clippy::too_many_arguments)]
fn emit_block(
    code: &CodeObject,
    b: &RawBlock,
    plan: &Plan,
    local_types: &[Option<JitType>],
    ret_lane: Option<JitType>,
    compact: &HashMap<usize, BlockId>,
    entry: Vec<ESlot>,
    emit_entries: &mut [Option<Vec<ESlot>>],
    out: &mut EmitOut,
    probes: &mut Probes<'_>,
) -> Result<TBlock, JitVerdict> {
    // The lowered block parameters: one per *native* entry value
    // (interpreter-only markers occupy no machine slot).
    let mut entry_stack: Vec<JitType> = Vec::with_capacity(entry.len());
    for s in &entry {
        if s.has_native() {
            if !s.ty.is_representable() {
                return Err(JitVerdict::TypeUnknown);
            }
            entry_stack.push(s.ty);
        }
    }
    let mut stack: Vec<ESlot> = entry;
    let mut stmts: Vec<TStmt> = Vec::new();
    out.max_stack = out.max_stack.max(stack.len() as u32);

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
            if top.none_const {
                // RFC 0069 WS1 — `return None` (incl. the implicit
                // tail return): nothing native to pop.
                TTerm::ReturnNone
            } else {
                if !top.is_plain() {
                    return Err(JitVerdict::UnsupportedOpcode("CALL (callee escapes)"));
                }
                TTerm::Return
            }
        }
        OpCode::JumpForward | OpCode::JumpBackward => {
            seed_entry(emit_entries, block_succ(b, 0), &stack);
            TTerm::Jump(compact[&block_succ(b, 0)])
        }
        OpCode::PopJumpIfFalse | OpCode::PopJumpIfTrue => {
            let c = stack.pop().ok_or(JitVerdict::StackUnderflow)?;
            if !c.is_plain() {
                return Err(JitVerdict::UnsupportedOpcode("CALL (callee escapes)"));
            }
            seed_entry(emit_entries, block_succ(b, 0), &stack);
            seed_entry(emit_entries, block_succ(b, 1), &stack);
            if matches!(ins.op, OpCode::PopJumpIfFalse) {
                TTerm::BranchFalse {
                    fallthrough: compact[&block_succ(b, 0)],
                    target: compact[&block_succ(b, 1)],
                }
            } else {
                TTerm::BranchTrue {
                    fallthrough: compact[&block_succ(b, 0)],
                    target: compact[&block_succ(b, 1)],
                }
            }
        }
        OpCode::ForIter => {
            if !stack.is_empty() {
                return Err(JitVerdict::NonEmptyBoundaryStack);
            }
            if let Some(&(cur_slot, stop_slot, var_slot)) = plan.headers.get(&last) {
                seed_entry(emit_entries, block_succ(b, 0), &stack);
                seed_entry(emit_entries, block_succ(b, 1), &stack);
                TTerm::ForRange {
                    cur_slot,
                    stop_slot,
                    var_slot,
                    body: compact[&block_succ(b, 0)],
                    exit: compact[&block_succ(b, 1)],
                }
            } else if let Some(&(seq_slot, idx_slot, var_slot)) = plan.iter_headers.get(&last) {
                // RFC 0071 WS4 — the list-loop header steps through
                // the registered helper; the element lane comes from
                // the seq slot's inferred list lane. An `Obj` seq slot
                // is the *opaque-iterator* loop: the step helper
                // advances the pinned iterator through the interpreter
                // core, and the element lane is the loop variable's.
                let lane = local_types
                    .get(seq_slot as usize)
                    .copied()
                    .flatten()
                    .ok_or(JitVerdict::TypeUnknown)?;
                seed_entry(emit_entries, block_succ(b, 0), &stack);
                seed_entry(emit_entries, block_succ(b, 1), &stack);
                if lane == JitType::Obj {
                    let elem = local_types
                        .get(var_slot as usize)
                        .copied()
                        .flatten()
                        .ok_or(JitVerdict::TypeUnknown)?;
                    TTerm::ForIter {
                        iter_slot: seq_slot,
                        var_slot,
                        elem,
                        pc: last as u32,
                        store_pc: (last + 1) as u32,
                        body: compact[&block_succ(b, 0)],
                        exit: compact[&block_succ(b, 1)],
                    }
                } else {
                    let elem = lane.elem_lane().ok_or(JitVerdict::UnsupportedOpcode(
                        "FOR_ITER (unsupported iterable)",
                    ))?;
                    TTerm::ForList {
                        seq_slot,
                        idx_slot,
                        var_slot,
                        elem,
                        pc: last as u32,
                        body: compact[&block_succ(b, 0)],
                        exit: compact[&block_succ(b, 1)],
                    }
                }
            } else {
                return Err(JitVerdict::UnsupportedOpcode("FOR_ITER (unplanned)"));
            }
        }
        // RFC 0070 WS2 — the yield's unconditional side exit. A
        // `None` yield (`yield`, `yield None`) materializes through
        // `PushNone` so the spilled stack top rebuilds as the real
        // singleton; anything else must already be a plain native
        // lane. RFC 0071 WS5 — the continuation block is seeded with
        // the yielded value replaced by the sent value (object lane),
        // so a resume entry can jump straight to it.
        OpCode::YieldValue => {
            if !code.is_generator {
                return Err(JitVerdict::UnsupportedOpcode(
                    "YIELD_VALUE (non-generator shape)",
                ));
            }
            let top = *stack.last().ok_or(JitVerdict::StackUnderflow)?;
            if top.none_const {
                stack.pop();
                stmts.push(TStmt {
                    pc: last as u32,
                    op: TOp::PushNone,
                });
                stack.push(ESlot::val(JitType::Obj));
                out.max_stack = out.max_stack.max(stack.len() as u32);
            } else {
                if !top.is_plain() {
                    return Err(JitVerdict::UnsupportedOpcode(
                        "YIELD_VALUE (marker operand)",
                    ));
                }
                if !top.ty.is_representable() {
                    return Err(JitVerdict::TypeUnknown);
                }
            }
            if let Some(&succ) = b.succs.first() {
                let mut cont = stack.clone();
                cont.pop();
                cont.push(ESlot::val(JitType::Obj));
                seed_entry(emit_entries, succ, &cont);
            }
            TTerm::Yield { pc: last as u32 }
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
            seed_entry(emit_entries, block_succ(b, 0), &stack);
            TTerm::Jump(compact[&block_succ(b, 0)])
        }
    };

    Ok(TBlock {
        entry_stack,
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
        method_sites,
        math_guards,
        math_spans,
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
    // RFC 0058 WS4 — rewritten range-loop pcs (RFC 0071 WS4 adds the
    // list-loop fused store, performed by the `ForList` terminator).
    if plan.nop.contains(&i)
        || plan.fused_store.contains_key(&i)
        || plan.fused_store_iter.contains_key(&i)
    {
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
        // RFC 0071 WS4 — a recognized list-loop `GET_ITER`: capture
        // the pinned list into the seq synthetic slot and zero the
        // index slot (inference already typed both).
        OpCode::GetIter => {
            let &(seq_slot, idx_slot) = plan
                .get_iter
                .get(&i)
                .ok_or(JitVerdict::UnsupportedOpcode("GET_ITER (unplanned)"))?;
            // RFC 0071 WS4 — the opaque-iterator capture: the runtime
            // helper verifies `iter(x) is x` (deopting otherwise) and
            // the pin lands in the seq slot; the idx slot is unused.
            if local_types.get(seq_slot as usize).copied().flatten() == Some(JitType::Obj) {
                stack.pop().ok_or(JitVerdict::StackUnderflow)?;
                push(
                    TOp::IterCapture {
                        iter_slot: seq_slot,
                    },
                    None,
                    stack,
                    stmts,
                );
                return Ok(());
            }
            stack.pop().ok_or(JitVerdict::StackUnderflow)?;
            push(TOp::StoreLocal(seq_slot), None, stack, stmts);
            push(TOp::PushConstInt(0), Some(JitType::Int), stack, stmts);
            stack.pop();
            push(TOp::StoreLocal(idx_slot), None, stack, stmts);
        }
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
                    min_args,
                    is_self,
                    ret,
                }) => {
                    // RFC 0059 WS3: the callee never reaches the native
                    // stack — push a marker and record where the
                    // *interpreter's* stack would hold the object (below
                    // any values already pushed, above the live
                    // iterators of enclosing rewritten loops).
                    let n_iters = plan.live_iters_at(i);
                    stack.push(ESlot {
                        callee: Some(CalleeMark {
                            kind: MarkKind::Py,
                            token,
                            arg_count,
                            min_args,
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
                    let n_iters = plan.live_iters_at(i);
                    stack.push(ESlot {
                        callee: Some(CalleeMark {
                            kind: MarkKind::Len,
                            token: 0,
                            arg_count: 1,
                            min_args: 1,
                            is_self: false,
                            ret: Some(JitType::Int),
                            load_pc: pc,
                            interp_depth: n_iters + stack.len() as u32,
                        }),
                        ..ESlot::val(JitType::Unknown)
                    });
                    return Ok(());
                }
                // RFC 0069 WS2 — the math module: interpreter-stack
                // only, consumed by the following intrinsic load.
                Some(ResolvedGlobal::MathModule) => {
                    let n_iters = plan.live_iters_at(i);
                    stack.push(ESlot {
                        math_mod: Some((ins.arg, pc, n_iters + stack.len() as u32)),
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
            // RFC 0069 WS1 — the `None` constant: interpreter-stack
            // only, consumed by `RETURN_VALUE`.
            if matches!(c, Constant::None) {
                stack.push(ESlot {
                    none_const: true,
                    ..ESlot::val(JitType::Unknown)
                });
                return Ok(());
            }
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
            let top = *stack.last().ok_or(JitVerdict::StackUnderflow)?;
            // RFC 0070 WS1 — `x = None` on an object-lane local: the
            // `None` constant never had a native slot, so materialize
            // it (machine value `-1`) and store it like any value.
            if top.none_const {
                let lt = local_types.get(ins.arg as usize).copied().flatten();
                if lt != Some(JitType::Obj) {
                    return Err(JitVerdict::UnsupportedOpcode("STORE_FAST (None lane)"));
                }
                stack.pop();
                push(TOp::PushNone, Some(JitType::Obj), stack, stmts);
                stack.pop();
                push(TOp::StoreLocal(ins.arg), None, stack, stmts);
                return Ok(());
            }
            pop_val(stack)?;
            push(TOp::StoreLocal(ins.arg), None, stack, stmts);
        }
        OpCode::BinaryOp => {
            let kind = bin_kind(ins.arg)?;
            let b = pop_val(stack)?;
            let a = pop_val(stack)?;
            // RFC 0071 WS4 — `list * int` repeats the pinned list on
            // the same lane through `wpjit_list_repeat`.
            if matches!(kind, ArithKind::Mul) && a.is_list() {
                if b != JitType::Int {
                    return Err(JitVerdict::UnsupportedOpcode("list repeat (count lane)"));
                }
                push(TOp::ListRepeat, Some(a), stack, stmts);
                return Ok(());
            }
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
            } else if a == JitType::Str && b == JitType::Str {
                // RFC 0071 WS6 — `str` equality through the registered
                // helper; ordering compares stay non-JITable.
                let negate = match kind {
                    CmpKind::Eq => false,
                    CmpKind::Ne => true,
                    _ => return Err(JitVerdict::UnsupportedOpcode("COMPARE_OP (str order)")),
                };
                push(TOp::StrEq { negate }, Some(JitType::Bool), stack, stmts);
            } else {
                return Err(JitVerdict::MixedArithTypes);
            }
        }
        // RFC 0070 WS1 — `x is None` / `x is not None` on a nullable
        // object lane. The `None` constant rides the interpreter stack
        // only (no native slot), so the native effect is pop-Obj /
        // push-Bool regardless of operand order.
        OpCode::IsOp => {
            if ins.arg > 1 {
                return Err(JitVerdict::UnsupportedOpcode("IS_OP kind"));
            }
            let b = stack.pop().ok_or(JitVerdict::StackUnderflow)?;
            let a = stack.pop().ok_or(JitVerdict::StackUnderflow)?;
            let val = match (a.none_const, b.none_const) {
                (false, true) => a,
                (true, false) => b,
                _ => return Err(JitVerdict::UnsupportedOpcode("IS_OP shape")),
            };
            if !val.is_plain() || val.ty != JitType::Obj {
                return Err(JitVerdict::UnsupportedOpcode("IS_OP operand lane"));
            }
            push(
                TOp::IsNone {
                    negate: ins.arg == 1,
                },
                Some(JitType::Bool),
                stack,
                stmts,
            );
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
        // CPython 3.13's `TO_BOOL`: `bool` is the identity;
        // `int`/`float` lower as a double negation (`x != 0`) on the
        // existing `Not` ops — two cheap native instructions, no new
        // IR.
        OpCode::ToBool => {
            let a = pop_val(stack)?;
            match a {
                JitType::Bool => stack.push(ESlot::val(JitType::Bool)),
                t if t.is_integral() => {
                    push(TOp::IntNot, Some(JitType::Bool), stack, stmts);
                    stack.pop();
                    push(TOp::IntNot, Some(JitType::Bool), stack, stmts);
                }
                JitType::Float => {
                    push(TOp::FloatNot, Some(JitType::Bool), stack, stmts);
                    stack.pop();
                    push(TOp::IntNot, Some(JitType::Bool), stack, stmts);
                }
                _ => return Err(JitVerdict::UnsupportedOpcode("TO_BOOL lane")),
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
            // RFC 0069 WS2 — plain-form `math.<intrinsic>` (the
            // explicit `PUSH_NULL` follows in the bytecode).
            if let Some((name_idx, load_pc, interp_depth)) = top.math_mod {
                stack.pop();
                emit_math_load(
                    code,
                    name,
                    name_idx,
                    load_pc,
                    interp_depth,
                    stack,
                    math_guards,
                    probes,
                )?;
                *max_stack = (*max_stack).max(stack.len() as u32);
                return Ok(());
            }
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
            let (slot, path) = obj_recv_ref(top.ty, top.src, top.path, probes.paths).ok_or(
                JitVerdict::UnsupportedOpcode("LOAD_ATTR (receiver provenance)"),
            )?;
            let names = probes.paths.names(path);
            let lane = (probes.attr)(slot, &names, name, false)
                .ok_or(JitVerdict::UnsupportedOpcode("LOAD_ATTR shape"))?;
            if lane == JitType::Unknown {
                return Err(JitVerdict::UnsupportedOpcode("LOAD_ATTR shape"));
            }
            stack.pop();
            let site = attr_sites.len() as u32;
            attr_sites.push(AttrSiteMeta {
                slot,
                path: names,
                name: name.to_owned(),
                lane,
                store: false,
                new_key: false,
            });
            push(TOp::AttrGet { site, out: lane }, Some(lane), stack, stmts);
            // RFC 0071 WS3 — extend the provenance chain on the result.
            if lane == JitType::Obj {
                if let Some(last) = stack.last_mut() {
                    last.path = probes.paths.seg(path, slot, name);
                }
            }
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
        // self-or-null marker on top. RFC 0069 adds the math-intrinsic
        // (WS2) and resolved instance-method (WS1) forms.
        OpCode::LoadMethodAttr => {
            let name = code
                .names
                .get(ins.arg as usize)
                .ok_or(JitVerdict::UnsupportedOpcode("LOAD_ATTR bad name"))?
                .as_str();
            let top = stack.last().copied().ok_or(JitVerdict::StackUnderflow)?;
            // RFC 0069 WS2 — `math.<intrinsic>`: replace the module
            // marker with a callee mark carrying the guard token (plus
            // the method form's implicit self-or-null on top).
            if let Some((name_idx, load_pc, interp_depth)) = top.math_mod {
                stack.pop();
                emit_math_load(
                    code,
                    name,
                    name_idx,
                    load_pc,
                    interp_depth,
                    stack,
                    math_guards,
                    probes,
                )?;
                stack.push(ESlot {
                    null: true,
                    ..ESlot::val(JitType::Unknown)
                });
                *max_stack = (*max_stack).max(stack.len() as u32);
                return Ok(());
            }
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
            let (slot, path) = obj_recv_ref(top.ty, top.src, top.path, probes.paths).ok_or(
                JitVerdict::UnsupportedOpcode("LOAD_ATTR (receiver provenance)"),
            )?;
            let names = probes.paths.names(path);
            // RFC 0069 WS1 — a class-resolved method: re-mark the
            // receiver in place (its pin stays the native value) and
            // record the site under the probe's token.
            if name != "append" {
                if let Some(res) = (probes.method)(slot, &names, name) {
                    let meta = MethodSiteMeta {
                        slot,
                        name: name.to_owned(),
                        arg_count: res.arg_count,
                        min_args: res.min_args,
                        ret: res.ret,
                    };
                    let idx = res.token as usize;
                    if method_sites.len() <= idx {
                        method_sites.resize(idx + 1, None);
                    }
                    method_sites[idx] = Some(meta);
                    // RFC 0070 WS1 — the burned-in resolution assumed
                    // an *instance* receiver, but the lane is nullable:
                    // fence out `None` here (deopt at this pc, receiver
                    // still on the stack) so the interpreter re-executes
                    // the load and raises the exact `AttributeError`.
                    push(TOp::GuardNotNone, None, stack, stmts);
                    let last = stack.len() - 1;
                    stack[last].method = Some(MethodMark {
                        token: res.token,
                        arg_count: res.arg_count,
                        min_args: res.min_args,
                        ret: res.ret,
                        load_pc: pc,
                    });
                    stack.push(ESlot {
                        null: true,
                        ..ESlot::val(JitType::Unknown)
                    });
                    *max_stack = (*max_stack).max(stack.len() as u32);
                    return Ok(());
                }
            }
            let lane = (probes.attr)(slot, &names, name, false)
                .ok_or(JitVerdict::UnsupportedOpcode("LOAD_ATTR shape"))?;
            if lane == JitType::Unknown {
                return Err(JitVerdict::UnsupportedOpcode("LOAD_ATTR shape"));
            }
            stack.pop();
            let site = attr_sites.len() as u32;
            attr_sites.push(AttrSiteMeta {
                slot,
                path: names,
                name: name.to_owned(),
                lane,
                store: false,
                new_key: false,
            });
            push(TOp::AttrGet { site, out: lane }, Some(lane), stack, stmts);
            // RFC 0071 WS3 — extend the provenance chain on the result.
            if lane == JitType::Obj {
                if let Some(last) = stack.last_mut() {
                    last.path = probes.paths.seg(path, slot, name);
                }
            }
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
            let (slot, path) = obj_recv_ref(recv.ty, recv.src, recv.path, probes.paths).ok_or(
                JitVerdict::UnsupportedOpcode("STORE_ATTR (receiver provenance)"),
            )?;
            let names = probes.paths.names(path);
            let probe_lane = (probes.attr)(slot, &names, name, true)
                .ok_or(JitVerdict::UnsupportedOpcode("STORE_ATTR shape"))?;
            let val = stack.pop().ok_or(JitVerdict::StackUnderflow)?;
            // RFC 0071 WS2 — the new-key shape: the value's own lane
            // defines the site (the fingerprint records the insert-or-
            // replace storage mode).
            let new_key = probe_lane == JitType::Unknown;
            let lane = if new_key {
                if val.none_const {
                    JitType::Obj
                } else {
                    val.ty
                }
            } else {
                probe_lane
            };
            if val.none_const {
                // RFC 0070 WS1 — `x.attr = None`: the `None` never had
                // a native slot, so materialize it above the receiver
                // and swap into the interpreter's `[value, receiver]`
                // order (`Swap2` is a free lowering-stack rotation).
                if lane != JitType::Obj {
                    return Err(JitVerdict::UnsupportedOpcode("STORE_ATTR (value lane)"));
                }
                stack.push(recv);
                push(TOp::PushNone, Some(JitType::Obj), stack, stmts);
                push(TOp::Swap2, None, stack, stmts);
                stack.pop();
                stack.pop();
            } else {
                if !val.is_plain() {
                    return Err(JitVerdict::UnsupportedOpcode("CALL (callee escapes)"));
                }
                if val.ty != lane {
                    return Err(JitVerdict::UnsupportedOpcode("STORE_ATTR (value lane)"));
                }
                // A storable lane: scalars and the object lane; other
                // pinned lanes have no cross-store meaning.
                if lane.is_pinned() && lane != JitType::Obj {
                    return Err(JitVerdict::UnsupportedOpcode("STORE_ATTR (value lane)"));
                }
                if !lane.is_representable() {
                    return Err(JitVerdict::TypeUnknown);
                }
            }
            let site = attr_sites.len() as u32;
            attr_sites.push(AttrSiteMeta {
                slot,
                path: names,
                name: name.to_owned(),
                lane,
                store: true,
                new_key,
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
                // native value (interpreter-only markers don't).
                let native_index = stack.iter().filter(|s| s.has_native()).count() as u32;
                method_spans.push(MethodSpanMeta {
                    native_index,
                    live_from: load_pc,
                    live_to: pc + 1,
                    token: None,
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
            // RFC 0069 WS1 — a burned-in method call: the receiver pin
            // is the callee's `self`, popped natively by `CallMethod`.
            if let Some(m) = f.method {
                let total = argc + 1;
                if total < m.min_args as usize || total > m.arg_count as usize {
                    return Err(JitVerdict::UnsupportedOpcode("CALL (method arity)"));
                }
                for &ty in &arg_tys {
                    if !ty.is_representable() {
                        return Err(JitVerdict::TypeUnknown);
                    }
                    // RFC 0071 WS1 — object-lane values marshal across
                    // the call as `ObjPin` entries (the helper resolves
                    // them against this activation's pin table). Other
                    // pinned lanes still stay put.
                    if ty.is_pinned() && ty != JitType::Obj {
                        return Err(JitVerdict::UnsupportedOpcode("CALL (pinned argument)"));
                    }
                }
                let native_index = stack.iter().filter(|s| s.has_native()).count() as u32;
                method_spans.push(MethodSpanMeta {
                    native_index,
                    live_from: m.load_pc,
                    live_to: pc + 1,
                    token: Some(m.token),
                });
                *max_call_args = (*max_call_args).max(argc as u32);
                // Re-model the receiver + args for the statement
                // (`CallMethod` pops `argc` scalars and the pin).
                stack.push(ESlot::val(f.ty));
                for &ty in arg_tys.iter().rev() {
                    stack.push(ESlot::val(ty));
                }
                push(
                    TOp::CallMethod {
                        token: m.token,
                        argc: argc as u8,
                        ret: m.ret,
                    },
                    None,
                    stack,
                    stmts,
                );
                for _ in 0..=argc {
                    stack.pop();
                }
                match m.ret {
                    // Procedure shape: `None` exists only on the
                    // interpreter stack (consumed by `POP_TOP`).
                    MethodRet::None => stack.push(ESlot {
                        poison: true,
                        ..ESlot::val(JitType::Unknown)
                    }),
                    MethodRet::Scalar(ty) => {
                        // RFC 0071 WS1 — an `Obj`-lane result arrives as
                        // a fresh pin in this activation's table.
                        if !ty.is_representable() || (ty.is_pinned() && ty != JitType::Obj) {
                            return Err(JitVerdict::TypeUnknown);
                        }
                        stack.push(ESlot::val(ty));
                    }
                }
                *max_stack = (*max_stack).max(stack.len() as u32);
                return Ok(());
            }
            let Some(mark) = f.callee else {
                return Err(JitVerdict::UnsupportedOpcode("CALL"));
            };
            // RFC 0069 WS2 — a burned-in math intrinsic: one float
            // operand (an integral operand converts exactly — i64 →
            // f64 is correctly rounded for the whole lane, matching
            // the interpreter's conversion).
            if let MarkKind::Math(func) = mark.kind {
                if argc != 1 {
                    return Err(JitVerdict::UnsupportedOpcode("math (arity)"));
                }
                if arg_tys[0].is_integral() {
                    stack.push(ESlot::val(arg_tys[0]));
                    push(TOp::IntToFloatTos { guarded: false }, None, stack, stmts);
                    stack.pop();
                } else if arg_tys[0] != JitType::Float {
                    return Err(JitVerdict::UnsupportedOpcode("math (operand lane)"));
                }
                math_spans.push(CalleeSpanMeta {
                    token: mark.token,
                    live_from: mark.load_pc,
                    live_to: pc + 1,
                    interp_depth: mark.interp_depth,
                });
                push(TOp::MathIntrinsic(func), Some(JitType::Float), stack, stmts);
                return Ok(());
            }
            // Positional arity within the defaults window (RFC 0069
            // WS3): the call helper binds the snapshotted defaults for
            // the `argc..arg_count` tail.
            if argc < mark.min_args as usize || argc > mark.arg_count as usize {
                return Err(JitVerdict::UnsupportedOpcode("CALL (arity)"));
            }
            // `len(x)` on a pinned list / `str` / `bytes` (RFC 0071
            // WS6) — no real call, no deopt-able argument window
            // (only loads can produce a pinned value).
            if mark.kind == MarkKind::Len {
                let op = if arg_tys[0].is_list() {
                    TOp::ListLen
                } else if arg_tys[0] == JitType::Str {
                    TOp::StrLen
                } else if arg_tys[0] == JitType::Bytes {
                    TOp::BytesLen
                } else {
                    return Err(JitVerdict::UnsupportedOpcode("len (argument lane)"));
                };
                len_spans.push(CalleeSpanMeta {
                    token: 0,
                    live_from: mark.load_pc,
                    live_to: pc + 1,
                    interp_depth: mark.interp_depth,
                });
                // Re-model the pin for the statement (the len op pops it).
                stack.push(ESlot::val(arg_tys[0]));
                push(op, None, stack, stmts);
                stack.pop();
                stack.push(ESlot::val(JitType::Int));
                *max_stack = (*max_stack).max(stack.len() as u32);
                return Ok(());
            }
            for &ty in &arg_tys {
                if !ty.is_representable() {
                    return Err(JitVerdict::TypeUnknown);
                }
                // RFC 0061/0065 WS5 — a raw pin index is meaningless
                // outside this activation. RFC 0071 WS1 carves out the
                // object lane: it marshals as an `ObjPin` entry the call
                // helper resolves (and re-pins on the callee side).
                if ty.is_pinned() && ty != JitType::Obj {
                    return Err(JitVerdict::UnsupportedOpcode("CALL (pinned argument)"));
                }
            }
            let ret = match if mark.is_self { ret_lane } else { mark.ret } {
                Some(t) if t.is_representable() => t,
                _ => return Err(JitVerdict::TypeUnknown),
            };
            // RFC 0061/0065 WS5 — a pin index is only meaningful within
            // its own activation's pinned-object table; RFC 0071 WS1
            // lets an object-lane result cross as a fresh caller pin.
            if ret.is_pinned() && ret != JitType::Obj {
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
        // RFC 0070 WS2 — the generator prologue's bootstrap-sent
        // `None` (see the abstract pass): a real Obj-lane push so the
        // following `POP_TOP` pops a native value. Dead code in
        // practice — native entry only happens at OSR pcs past it.
        OpCode::ReturnGenerator => {
            if !code.is_generator {
                return Err(JitVerdict::UnsupportedOpcode(
                    "RETURN_GENERATOR (non-generator shape)",
                ));
            }
            push(TOp::PushNone, Some(JitType::Obj), stack, stmts);
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
        // RFC 0071 WS4 — `BUILD_LIST k` (uniform lane or all-`None`).
        OpCode::BuildList => {
            let k = ins.arg as usize;
            if k == 0 || k > 16 {
                return Err(JitVerdict::UnsupportedOpcode("BUILD_LIST (shape)"));
            }
            let base = stack
                .len()
                .checked_sub(k)
                .ok_or(JitVerdict::StackUnderflow)?;
            let none_fill = stack[base..].iter().all(|e| e.none_const);
            let elem = if none_fill {
                JitType::Obj
            } else {
                let mut lane: Option<JitType> = None;
                for e in &stack[base..] {
                    if !e.is_plain() {
                        return Err(JitVerdict::UnsupportedOpcode("BUILD_LIST (marker element)"));
                    }
                    match lane {
                        None => lane = Some(e.ty),
                        Some(l) if l == e.ty => {}
                        Some(_) => {
                            return Err(JitVerdict::UnsupportedOpcode("BUILD_LIST (mixed lanes)"))
                        }
                    }
                }
                lane.ok_or(JitVerdict::TypeUnknown)?
            };
            let list = JitType::list_of(elem)
                .ok_or(JitVerdict::UnsupportedOpcode("BUILD_LIST (element lane)"))?;
            stack.truncate(base);
            // The elements stage through the call-arg marshal buffer,
            // which must be wide enough for them (`none_fill` stages
            // nothing).
            if !none_fill {
                *max_call_args = (*max_call_args).max(k as u32);
            }
            push(
                TOp::BuildList {
                    n: k as u32,
                    elem,
                    none_fill,
                },
                Some(list),
                stack,
                stmts,
            );
        }
        // RFC 0071 WS4 — erased `BUILD_SLICE` (unit step): the marker
        // rides the emission stack; the present bounds keep their
        // native slots below it.
        OpCode::BuildSlice => {
            if ins.arg != 3 && ins.arg != 2 {
                return Err(JitVerdict::UnsupportedOpcode("BUILD_SLICE (shape)"));
            }
            if ins.arg == 3 {
                let step = stack.pop().ok_or(JitVerdict::StackUnderflow)?;
                if !step.none_const {
                    return Err(JitVerdict::UnsupportedOpcode("BUILD_SLICE (step)"));
                }
            }
            let stop = stack.pop().ok_or(JitVerdict::StackUnderflow)?;
            let start = stack.pop().ok_or(JitVerdict::StackUnderflow)?;
            let bound = |e: &ESlot| -> Result<bool, JitVerdict> {
                if e.none_const {
                    return Ok(false);
                }
                if !e.is_plain() || e.ty != JitType::Int {
                    return Err(JitVerdict::UnsupportedOpcode("BUILD_SLICE (bound)"));
                }
                Ok(true)
            };
            let has_start = bound(&start)?;
            let has_stop = bound(&stop)?;
            // The native bounds stay on the model stack *below* the
            // marker, in start-then-stop order (matching what the
            // `ListSlice` lowering pops).
            if has_start {
                stack.push(start);
            }
            if has_stop {
                stack.push(stop);
            }
            stack.push(ESlot {
                slice: Some((has_start, has_stop)),
                ..ESlot::val(JitType::Unknown)
            });
            *max_stack = (*max_stack).max(stack.len() as u32);
        }
        // RFC 0061 WS5 — pinned-list element read/write. Inference
        // already pinned the container slot's lane; emission just
        // re-validates the operand lanes it sees.
        OpCode::BinarySubscr => {
            // RFC 0071 WS4 — the slice marker's only consumer: the
            // fresh sliced list on the container's own lane. The
            // deopt point is the erased `BUILD_SLICE` (must be the
            // immediately preceding instruction).
            if let Some(&marker) = stack.last() {
                if let Some((has_start, has_stop)) = marker.slice {
                    if i == 0 || !matches!(code.instructions[i - 1].op, OpCode::BuildSlice) {
                        return Err(JitVerdict::UnsupportedOpcode("slice (non-adjacent)"));
                    }
                    stack.pop();
                    if has_stop {
                        pop_val(stack)?;
                    }
                    if has_start {
                        pop_val(stack)?;
                    }
                    let cont = pop_val(stack)?;
                    if cont.elem_lane().is_none() {
                        return Err(JitVerdict::UnsupportedOpcode("slice container lane"));
                    }
                    stmts.push(TStmt {
                        pc: (i - 1) as u32,
                        op: TOp::ListSlice {
                            start: has_start,
                            stop: has_stop,
                        },
                    });
                    stack.push(ESlot::val(cont));
                    *max_stack = (*max_stack).max(stack.len() as u32);
                    return Ok(());
                }
            }
            let idx = pop_val(stack)?;
            if idx != JitType::Int {
                return Err(JitVerdict::UnsupportedOpcode("subscript index lane"));
            }
            let cont = pop_val(stack)?;
            // RFC 0071 WS6 — `bytes[i]` through the registered helper.
            if cont == JitType::Bytes {
                push(TOp::BytesGetItem, Some(JitType::Int), stack, stmts);
                return Ok(());
            }
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
            ArithKind::Add
            | ArithKind::Sub
            | ArithKind::Mul
            | ArithKind::TrueDiv
            | ArithKind::FloorDiv
            | ArithKind::Mod => Ok((TOp::FloatArith(kind), JitType::Float)),
            _ => Err(JitVerdict::UnsupportedOpcode("float bitop")),
        }
    } else {
        Err(JitVerdict::MixedArithTypes)
    }
}

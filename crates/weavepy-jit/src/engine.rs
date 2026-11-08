//! The compiled-frame engine and entry point.
//!
//! A [`JitEngine`] lowers each frame to `weavepy-codegen`'s IR and installs
//! its machine code in the process's code arena, which is never freed, so
//! a compiled frame's function pointer stays valid for the process. The
//! engine is intended to be a per-thread singleton (the VM keeps it in
//! thread-local storage, under the GIL).

use std::mem;

use weavepy_codegen::{types, AbiParam, ExtFuncData, FuncRef, FuncTarget, Function, Type};

use crate::analyze::{JitVerdict, Probes};
use crate::ir::{
    ArithKind, AttrSiteMeta, CalleeSpanMeta, CompSavedMeta, GlobalGuard, IterLoopMeta,
    ListLoopMeta, MathFunc, MathGuardMeta, MethodSiteMeta, MethodSpanMeta, OsrEntry, RangeLoopMeta,
    ResolvedGlobal, StrMethod, TFunc, TOp, TTerm,
};
use crate::lower::build_function;
use crate::runtime::{self, JitFrame, JitStatus};
use crate::value::JitType;
use weavepy_compiler::CodeObject;

/// The native ABI of a compiled frame: takes a `*mut JitFrame`, returns
/// an `i64` [`JitStatus`].
pub(crate) type NativeFn = unsafe extern "C" fn(*mut JitFrame) -> i64;

/// A compiled frame plus the metadata the VM needs to marshal values in
/// and out and to apply the entry guard.
#[derive(Debug)]
pub struct CompiledFrame {
    func: NativeFn,
    /// Local slots to type-guard + pack before entry (read-before-write).
    pub livein: Vec<u32>,
    /// Stable lane of each local slot (`None` = not JIT-managed).
    pub local_types: Vec<Option<JitType>>,
    /// Max abstract operand-stack depth, for sizing the spill buffer.
    pub max_stack: u32,
    /// Number of local slots, *including* synthetic range-loop slots.
    pub n_locals: u32,
    /// Global resolutions burned into the code; the embedder must
    /// re-validate each before every native entry (RFC 0058 WS4).
    pub global_guards: Vec<GlobalGuard>,
    /// Rewritten range loops, outermost-first, for rebuilding the live
    /// iterators on the interpreter stack after a mid-loop deopt.
    pub range_loops: Vec<RangeLoopMeta>,
    /// RFC 0071 WS4 — rewritten list loops, ascending `live_from`
    /// (interleaved with [`Self::range_loops`] by `live_from` when
    /// rebuilding the interpreter stack).
    pub list_loops: Vec<ListLoopMeta>,
    /// RFC 0071 WS4 — rewritten opaque-iterator loops, ascending
    /// `live_from` (the pinned iterator object rebuilds directly onto
    /// the interpreter stack).
    pub iter_loops: Vec<IterLoopMeta>,
    /// RFC 0073 WS1 — inlined-comprehension saved-target spans (the
    /// parked `Unbound` re-inserted at its depth on a mid-span deopt).
    pub comp_saved: Vec<CompSavedMeta>,
    /// Inlined-comprehension target slots (see
    /// [`crate::ir::TFunc::comp_target_slots`]).
    pub comp_target_slots: Vec<u32>,
    /// Cold-exit pcs (see [`crate::ir::TFunc::cold_exits`]).
    pub cold_exits: Vec<u32>,
    /// See [`crate::ir::TFunc::region_exits`].
    pub region_exits: Vec<u32>,
    /// See [`crate::ir::TFunc::region_roots`].
    pub region_roots: Vec<u32>,
    /// See [`crate::ir::TFunc::stayed_heads`].
    pub stayed_heads: Vec<u32>,
    /// See [`crate::ir::TFunc::env_heads`].
    pub env_heads: Vec<u32>,
    /// Whether the interpreter should enter this body directly: a
    /// loop-free body that round-trips into the interpreter (generic
    /// calls, dynamic attributes or membership) gains nothing native
    /// over its interpreted leaf paths and pays the entry/exit on top.
    /// Native callers use it regardless.
    pub interp_entry: bool,
    /// Erased Python callees (RFC 0059 WS3), for rebuilding the callee
    /// object on the interpreter stack after a mid-arguments deopt.
    pub callee_spans: Vec<CalleeSpanMeta>,
    /// RFC 0065 WS5 — erased `len` builtins (see [`TFunc::len_spans`]).
    pub len_spans: Vec<CalleeSpanMeta>,
    /// RFC 0065 WS5 — erased bound-method receivers (`list.append`).
    pub method_spans: Vec<MethodSpanMeta>,
    /// RFC 0065 WS5 — burned-in attribute-access sites; the embedder
    /// snapshots one guard fingerprint per site before first entry.
    pub attr_sites: Vec<AttrSiteMeta>,
    /// RFC 0069 WS1 — burned-in method-call sites, indexed by the
    /// `wpjit_call_method` token; the embedder keeps its method table
    /// parallel to this.
    pub method_sites: Vec<MethodSiteMeta>,
    /// RFC 0073 WS3 — burned-in native `str`-method sites (static —
    /// no embedder table).
    pub str_method_sites: Vec<StrMethod>,
    /// RFC 0073 WS3 — erased `str`-method receivers (`token` indexes
    /// [`Self::str_method_sites`]).
    pub str_method_spans: Vec<MethodSpanMeta>,
    /// RFC 0069 WS2 — burned-in math-intrinsic guards; the embedder
    /// snapshots the resolved `(module, function)` pair per guard and
    /// re-validates it at entry and per poll stride.
    pub math_guards: Vec<MathGuardMeta>,
    /// RFC 0069 WS2 — erased `math` intrinsic callables riding the
    /// interpreter stack (`token` indexes [`Self::math_guards`]).
    pub math_spans: Vec<CalleeSpanMeta>,
    /// RFC 0074 WS2 — interpreter-only self-or-null `Unbound` markers
    /// of opaque-call sites (`token` unused; a deopt strictly inside
    /// the span re-inserts the marker at `interp_depth`).
    pub null_spans: Vec<CalleeSpanMeta>,
    /// RFC 0069 WS1 — every return site is provably `None` (the
    /// procedure shape; feeds callers' [`crate::ir::MethodRet::None`]).
    pub ret_none: bool,
    /// Loop-header pcs enterable mid-frame via `entry_pc` (OSR).
    pub osr_entries: Vec<OsrEntry>,
    /// RFC 0071 WS5 — generator resume pcs enterable via `entry_pc`
    /// (the sent value passed in `ret_bits`, object-lane packed).
    pub resume_entries: Vec<OsrEntry>,
    /// Widest `CallPy` argument count, for sizing the marshal buffers.
    pub max_call_args: u32,
    /// The function's own stable scalar return lane, when every return
    /// site agrees (feeds callers' `PyFunc` classification).
    pub ret_lane: Option<JitType>,
    scalar_leaf: bool,
    op_mix: OpMix,
    /// The engine's handle for this function, for direct calls from
    /// frames compiled later (see [`Self::direct_leaf`]).
    /// The machine code's entry, for direct callers.
    code: usize,
}

/// A compiled scalar leaf another frame may call directly (see
/// [`CompiledFrame::direct_leaf`]): its function and frame layout, and
/// the lanes of its parameters and result.
#[derive(Clone, Debug)]
pub struct DirectLeaf {
    /// The machine code's entry, for direct callers.
    code: usize,
    n_locals: u32,
    max_stack: u32,
    params: Vec<JitType>,
    ret: JitType,
    /// Per parameter, the bits of the default a call that leaves it out
    /// binds (in the parameter's lane), when the caller burns it in.
    defaults: Vec<Option<u64>>,
    /// A method whose body never reads its receiver (see
    /// [`CompiledFrame::direct_method_leaf`]): local 0 is `self`, and
    /// [`Self::params`] are the parameters after it.
    receiver: bool,
}

impl DirectLeaf {
    /// The lanes of the leaf's parameters.
    #[must_use]
    pub fn params(&self) -> &[JitType] {
        &self.params
    }

    /// Let calls that leave parameters out bind `defaults` (per
    /// parameter, the default's bits in its lane): the embedder guards
    /// that they stay the function's defaults.
    #[must_use]
    pub fn with_defaults(mut self, defaults: Vec<Option<u64>>) -> Self {
        self.defaults = defaults;
        self
    }
}

/// A method body a guarded call site runs in line (see
/// [`InlineMethod::of`]): its operations, which read only the call's
/// scalar arguments, constants and scalar fields of `self` and of its
/// object arguments, and compute a scalar result. Nothing it does is
/// observable, so any surprise (a receiver of another class, a field of
/// another lane or position, an overflow, a zero divisor) restarts the call
/// through the method helper.
#[derive(Clone, Debug)]
pub struct InlineMethod {
    pub(crate) ops: Vec<TOp>,
    pub(crate) n_locals: u32,
    /// The lanes of the parameters after `self`: scalars, and objects whose
    /// fields the body reads.
    pub(crate) params: Vec<JitType>,
    pub(crate) ret: JitType,
    /// Per field read (the body's attribute site), the attribute's name
    /// and lane, the parameter it's read off, and where the receiver's
    /// class keeps it (see [`Self::with_fields`]).
    pub(crate) field_names: Vec<String>,
    pub(crate) field_lanes: Vec<JitType>,
    pub(crate) field_recv: Vec<u32>,
    pub(crate) field_at: Vec<FieldAt>,
    /// Per parameter (`self` first), the class version an object argument
    /// whose fields the body reads must have (`0` for none; the site's
    /// method guard checks `self`'s class).
    pub(crate) recv_ver: Vec<u64>,
}

/// Where an in-line method body finds one field of its receiver, an
/// instance of the class the body was resolved against (see
/// [`InlineMethod::with_fields`]): fixed for as long as the class has the
/// version the site checks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FieldAt {
    /// Not resolved: the body can't run in line.
    Unknown,
    /// Its index in the instance's split values over the class's names.
    Split(u32),
    /// Its position among the member slots the class lays out, and the
    /// layout's address, which the instance's slots must be laid out over.
    Slot { idx: u32, layout: usize },
}

/// Operations an in-line method body may hold besides its locals and
/// field reads: pure scalar work whose only failure is a guard.
fn inline_op_ok(op: TOp) -> bool {
    matches!(
        op,
        TOp::PushConstInt(_)
            | TOp::PushConstFloat(_)
            | TOp::PushConstBool(_)
            | TOp::IntArith(
                ArithKind::Add
                    | ArithKind::Sub
                    | ArithKind::Mul
                    | ArithKind::FloorDiv
                    | ArithKind::Mod
                    | ArithKind::And
                    | ArithKind::Or
                    | ArithKind::Xor
                    | ArithKind::TrueDiv
            )
            | TOp::FloatArith(
                ArithKind::Add | ArithKind::Sub | ArithKind::Mul | ArithKind::TrueDiv
            )
            | TOp::IntTrueDiv
            | TOp::IntCmp(_)
            | TOp::FloatCmp(_)
            | TOp::IntNeg
            | TOp::FloatNeg
            | TOp::IntInvert
            | TOp::IntNot
            | TOp::FloatNot
            | TOp::Pop
            | TOp::Dup { .. }
            | TOp::Swap2
            | TOp::SwapN { .. }
            | TOp::IntToFloatTos { .. }
            | TOp::IntToFloatSecond { .. }
    )
}

impl InlineMethod {
    /// The most operations an in-line body may hold.
    const MAX_OPS: usize = 32;

    /// `tfunc`, a method's analysis with its `arg_count` parameters
    /// (`self` included) typed as one call site passes them, as an in-line
    /// body: one block returning a scalar, no guards, loops or calls, every
    /// local a scalar except `self` and object parameters, which only feed
    /// scalar field reads (no stores) straight off the instances, and every
    /// other operation pure scalar work.
    #[must_use]
    pub fn of(tfunc: &TFunc, arg_count: u32) -> Option<InlineMethod> {
        let scalar = |t: JitType| matches!(t, JitType::Int | JitType::Float | JitType::Bool);
        let [block] = tfunc.blocks.as_slice() else {
            return None;
        };
        let ret = tfunc.ret_lane.filter(|&t| scalar(t))?;
        let n = arg_count as usize;
        // An object local: `self`, or a parameter typed as one.
        let object =
            |s: usize| s < n && matches!(tfunc.local_types.get(s), Some(None | Some(JitType::Obj)));
        let shape_ok = tfunc.entry_block == 0
            && block.entry_stack.is_empty()
            && block.stmts.len() <= Self::MAX_OPS
            && matches!(block.term, TTerm::Return)
            && !tfunc.ret_none
            && n >= 1
            && tfunc.local_types.len() >= n
            && matches!(tfunc.local_types[0], None | Some(JitType::Obj))
            && tfunc
                .local_types
                .iter()
                .enumerate()
                .skip(1)
                .all(|(s, t)| t.is_none_or(|t| scalar(t) || (s < n && t == JitType::Obj)))
            && tfunc.livein_locals.iter().all(|&s| (s as usize) < n)
            && tfunc.global_guards.is_empty()
            && tfunc.math_guards.is_empty()
            && tfunc.range_loops.is_empty()
            && tfunc.list_loops.is_empty()
            && tfunc.iter_loops.is_empty()
            && tfunc.comp_saved.is_empty()
            && tfunc.resume_entries.is_empty()
            && tfunc.osr_entries.is_empty()
            && tfunc.callee_spans.is_empty()
            && tfunc.len_spans.is_empty()
            && tfunc.method_spans.is_empty()
            && tfunc.method_sites.is_empty()
            && tfunc.str_method_sites.is_empty()
            && tfunc.str_method_spans.is_empty()
            && tfunc.math_spans.is_empty()
            && tfunc.null_spans.is_empty()
            && tfunc.max_call_args == 0
            && tfunc.attr_sites.iter().all(|s| {
                object(s.slot as usize)
                    && s.path.is_empty()
                    && !s.store
                    && !s.new_key
                    && s.ctor.is_none()
                    && s.self_ctor.is_none()
                    && scalar(s.lane)
            });
        if !shape_ok {
            return None;
        }
        let params = tfunc.local_types[1..n]
            .iter()
            .map(|t| t.filter(|&t| scalar(t) || t == JitType::Obj))
            .collect::<Option<Vec<_>>>()?;
        let ops_ok = block.stmts.iter().all(|st| match st.op {
            // Only `self` and object parameters are objects, and only a
            // field read consumes one (no other operation accepts the
            // object lane); they're never rebound.
            TOp::LoadLocal(s) => (s as usize) < tfunc.local_types.len(),
            TOp::StoreLocal(s) => !object(s as usize) && (s as usize) < tfunc.local_types.len(),
            TOp::AttrGet { site, out } => {
                tfunc.attr_sites.get(site as usize).map(|s| s.lane) == Some(out)
            }
            op => inline_op_ok(op),
        });
        if !ops_ok {
            return None;
        }
        Some(InlineMethod {
            ops: block.stmts.iter().map(|st| st.op).collect(),
            n_locals: tfunc.n_locals,
            params,
            ret,
            field_names: tfunc.attr_sites.iter().map(|s| s.name.clone()).collect(),
            field_lanes: tfunc.attr_sites.iter().map(|s| s.lane).collect(),
            field_recv: tfunc.attr_sites.iter().map(|s| s.slot).collect(),
            field_at: vec![FieldAt::Unknown; tfunc.attr_sites.len()],
            recv_ver: vec![0; n],
        })
    }

    /// The names of the fields the body reads, by attribute site.
    #[must_use]
    pub fn field_names(&self) -> &[String] {
        &self.field_names
    }

    /// The lanes of the fields the body reads, by attribute site.
    #[must_use]
    pub fn field_lanes(&self) -> &[JitType] {
        &self.field_lanes
    }

    /// The parameter (`0` for `self`) each field is read off, by attribute
    /// site.
    #[must_use]
    pub fn field_receivers(&self) -> &[u32] {
        &self.field_recv
    }

    /// Where each field read finds its value (`at`, by attribute site), in
    /// an instance of the class each receiver must belong to: for `self`,
    /// the class the call site's guard pins, and for the object parameters,
    /// the one whose version `recv_ver` holds (by parameter, `self` first).
    /// A class's names and slot layout never move, so the version binds the
    /// positions for good. `None` when a field is unresolved, a receiver
    /// has no version to check, or the counts don't match.
    #[must_use]
    pub fn with_fields(mut self, at: Vec<FieldAt>, recv_ver: Vec<u64>) -> Option<Self> {
        if at.len() != self.field_at.len()
            || at.contains(&FieldAt::Unknown)
            || recv_ver.len() != self.recv_ver.len()
        {
            return None;
        }
        for (k, &r) in self.field_recv.iter().enumerate() {
            if r != 0 && recv_ver.get(r as usize).is_none_or(|&v| v == 0) {
                return None;
            }
            // One receiver's slots are laid out over one layout.
            if let FieldAt::Slot { layout, .. } = at[k] {
                let other = self.field_recv.iter().zip(&at).any(|(&r2, a)| {
                    r2 == r && matches!(*a, FieldAt::Slot { layout: l2, .. } if l2 != layout)
                });
                if other || layout == 0 {
                    return None;
                }
            }
        }
        self.field_at = at;
        self.recv_ver = recv_ver;
        Some(self)
    }
}

impl CompiledFrame {
    /// Whether this frame has one bounded block of pure scalar operations.
    /// It cannot access pins or namespaces or run a poll. Its only helpers
    /// are context-free math intrinsics; the caller must validate global and
    /// math guards before entry. Its exits are a scalar return or a numeric
    /// deopt after which the pure body can be restarted.
    #[must_use]
    pub fn is_scalar_leaf(&self) -> bool {
        self.scalar_leaf
    }

    /// This frame as a direct-call target taking `arg_count` arguments:
    /// a scalar leaf with no global or math guards (so nothing about
    /// the namespace can invalidate it) and scalar parameter lanes. A
    /// caller compiled on the same engine calls it in native code; its
    /// numeric deopt restarts the call through the ordinary path.
    #[must_use]
    pub fn direct_leaf(&self, arg_count: u32) -> Option<DirectLeaf> {
        let scalar = |t: JitType| matches!(t, JitType::Int | JitType::Float | JitType::Bool);
        if !self.scalar_leaf || !self.global_guards.is_empty() || !self.math_guards.is_empty() {
            return None;
        }
        let ret = self.ret_lane.filter(|&t| scalar(t))?;
        let params = self
            .local_types
            .get(..arg_count as usize)?
            .iter()
            .map(|t| t.filter(|&t| scalar(t)))
            .collect::<Option<Vec<_>>>()?;
        Some(DirectLeaf {
            code: self.code,
            n_locals: self.n_locals,
            max_stack: self.max_stack,
            defaults: vec![None; params.len()],
            params,
            ret,
            receiver: false,
        })
    }

    /// This frame as the direct-call target of a method taking
    /// `arg_count` parameters, `self` included: a [`Self::direct_leaf`]
    /// whose body never reads `self` (its slot has no lane), so a guarded
    /// method call enters it with the remaining parameters alone. The
    /// caller's guard (the receiver's class version, no instance
    /// attribute shadowing the name, the function's `__code__`) is what
    /// binds the call to this body.
    #[must_use]
    pub fn direct_method_leaf(&self, arg_count: u32) -> Option<DirectLeaf> {
        if arg_count == 0 || self.local_types.first().copied().flatten().is_some() {
            return None;
        }
        let scalar = |t: JitType| matches!(t, JitType::Int | JitType::Float | JitType::Bool);
        if !self.scalar_leaf || !self.global_guards.is_empty() || !self.math_guards.is_empty() {
            return None;
        }
        let ret = self.ret_lane.filter(|&t| scalar(t))?;
        let params = self
            .local_types
            .get(1..arg_count as usize)?
            .iter()
            .map(|t| t.filter(|&t| scalar(t)))
            .collect::<Option<Vec<_>>>()?;
        Some(DirectLeaf {
            code: self.code,
            n_locals: self.n_locals,
            max_stack: self.max_stack,
            defaults: vec![None; params.len()],
            params,
            ret,
            receiver: true,
        })
    }

    /// `(generic, total)` statement counts: how many of the compiled
    /// statements go through the interpreter's generic object protocol
    /// (dynamic calls and attribute accesses).
    #[must_use]
    pub fn op_mix(&self) -> (u32, u32) {
        let m = self.op_mix;
        (m.dyn_calls + m.dyn_attrs + m.dyn_other, m.total)
    }

    /// The statement counts by kind behind [`Self::op_mix`].
    #[must_use]
    pub fn op_kinds(&self) -> OpMix {
        self.op_mix
    }

    /// Enter the compiled frame.
    ///
    /// # Safety
    ///
    /// `frame` must point to a fully-initialised [`JitFrame`] whose
    /// `locals` / `stack_spill` / `stack_tags` buffers are at least
    /// `n_locals` / `max_stack` wide, and the owning [`JitEngine`] must
    /// still be alive (its `JITModule` backs this function pointer).
    #[must_use]
    pub unsafe fn enter(&self, frame: *mut JitFrame) -> JitStatus {
        // SAFETY: the caller upholds the buffer-size and liveness
        // invariants documented above; the function pointer was produced
        // by `JITModule::get_finalized_function` for this exact signature.
        let raw = unsafe { (self.func)(frame) };
        JitStatus::from_raw(raw)
    }
}

/// Lowers frames and installs their code, keeping the function buffer.
/// Dropping the engine unmaps its code.
pub struct JitEngine {
    func: Function,
    code: weavepy_codegen::CodeArena,
    ptr_ty: Type,
    next_id: u32,
}

impl std::fmt::Debug for JitEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JitEngine")
            .field("ptr_ty", &self.ptr_ty)
            .field("next_id", &self.next_id)
            .finish_non_exhaustive()
    }
}

impl JitEngine {
    /// Build a fresh engine for the host target. Returns `None` if the
    /// host ISA can't be configured (e.g. an unsupported platform), in
    /// which case the VM simply never tiers up.
    #[must_use]
    pub fn new() -> Option<JitEngine> {
        if !weavepy_codegen::SUPPORTED {
            return None;
        }
        Some(JitEngine {
            func: Function::new(),
            code: weavepy_codegen::CodeArena::new(),
            ptr_ty: types::I64,
            next_id: 0,
        })
    }

    /// Drop the function for an empty one: clearing it keeps its buffers'
    /// capacity, the largest function's size, for the rest of the thread.
    fn release(&mut self) {
        self.func = Function::new();
    }

    /// Analyze and compile a code object. `resolve` reports what each
    /// `LOAD_GLOBAL` name currently resolves to (see
    /// [`ResolvedGlobal`]); the caller must re-validate every resolution
    /// listed in [`CompiledFrame::global_guards`] before each entry.
    /// Returns the compiled frame, or the [`JitVerdict`] explaining why
    /// the code is not JITable.
    pub fn compile(
        &mut self,
        code: &CodeObject,
        resolve: &mut dyn FnMut(&str) -> ResolvedGlobal,
    ) -> Result<CompiledFrame, JitVerdict> {
        self.compile_with_probe(code, resolve, &mut |_| None)
    }

    /// [`Self::compile`] with a pinned-list lane probe (RFC 0061 WS5):
    /// `probe_list` reports the observed homogeneous element lane of a
    /// subscripted local in the requesting activation.
    pub fn compile_with_probe(
        &mut self,
        code: &CodeObject,
        resolve: &mut dyn FnMut(&str) -> ResolvedGlobal,
        probe_list: &mut dyn FnMut(u32) -> Option<JitType>,
    ) -> Result<CompiledFrame, JitVerdict> {
        self.compile_with_probes(code, resolve, probe_list, &mut |_, _, _| None)
    }

    /// [`Self::compile_with_probe`] with an attribute-site probe
    /// (RFC 0065 WS5): `probe_attr(slot, name, store)` reports the
    /// observed scalar value lane of an eligible instance-dict
    /// attribute on the local currently in `slot`.
    pub fn compile_with_probes(
        &mut self,
        code: &CodeObject,
        resolve: &mut dyn FnMut(&str) -> ResolvedGlobal,
        probe_list: &mut dyn FnMut(u32) -> Option<JitType>,
        probe_attr: &mut dyn FnMut(u32, &str, bool) -> Option<JitType>,
    ) -> Result<CompiledFrame, JitVerdict> {
        let tfunc = crate::analyze::analyze_with_probes(code, resolve, probe_list, probe_attr)?;
        self.compile_tfunc(&tfunc)
    }

    /// [`Self::compile_with_probes`] with the full probe bundle
    /// (RFC 0069 WS1/WS2 adds the method-resolution and math-module
    /// probes).
    pub fn compile_frame(
        &mut self,
        code: &CodeObject,
        resolve: &mut dyn FnMut(&str) -> ResolvedGlobal,
        probes: &mut Probes<'_>,
    ) -> Result<CompiledFrame, JitVerdict> {
        self.compile_frame_direct(
            code,
            resolve,
            probes,
            &mut |_| None,
            &mut |_| None,
            &mut |_, _, _| None,
            &mut |_| false,
        )
    }

    /// [`Self::compile_frame`] with the direct-call targets of the callee
    /// tokens: `direct(token)` names a scalar leaf compiled on this
    /// engine (see [`CompiledFrame::direct_leaf`]) that a matching call
    /// site enters in native code, `direct_method(token)` one a matching
    /// method site enters once its guard holds (see
    /// [`CompiledFrame::direct_method_leaf`]), and `inline_method(token,
    /// lanes, pc)` the body the method site whose call is at `pc`, passing
    /// arguments of those lanes, runs in line once its guard holds (see
    /// [`InlineMethod::of`]). `slot_member(site)` says whether an attribute
    /// site's receiver keeps the attribute in a laid-out member slot (see
    /// [`AttrSiteMeta::slot_member`]).
    #[allow(clippy::too_many_arguments)]
    pub fn compile_frame_direct(
        &mut self,
        code: &CodeObject,
        resolve: &mut dyn FnMut(&str) -> ResolvedGlobal,
        probes: &mut Probes<'_>,
        direct: &mut dyn FnMut(u32) -> Option<DirectLeaf>,
        direct_method: &mut dyn FnMut(u32) -> Option<DirectLeaf>,
        inline_method: &mut dyn FnMut(u32, &[JitType], u32) -> Option<InlineMethod>,
        slot_member: &mut dyn FnMut(&AttrSiteMeta) -> bool,
    ) -> Result<CompiledFrame, JitVerdict> {
        let tfunc = analyze_admitted(code, resolve, probes, slot_member)?;
        self.compile_tfunc_methods(&tfunc, direct, direct_method, inline_method)
    }

    /// [`Self::compile_frame_direct`] on the engine in `engine`, built
    /// (see [`Self::new`]) only once the analysis admits the code: code
    /// the analysis rejects never pays for building one. When the host
    /// can't have one, `engine` stays empty and the verdict is
    /// [`NO_HOST_ENGINE`].
    #[allow(clippy::too_many_arguments)]
    pub fn compile_frame_lazily(
        engine: &mut Option<JitEngine>,
        code: &CodeObject,
        resolve: &mut dyn FnMut(&str) -> ResolvedGlobal,
        probes: &mut Probes<'_>,
        direct: &mut dyn FnMut(u32) -> Option<DirectLeaf>,
        direct_method: &mut dyn FnMut(u32) -> Option<DirectLeaf>,
        inline_method: &mut dyn FnMut(u32, &[JitType], u32) -> Option<InlineMethod>,
        slot_member: &mut dyn FnMut(&AttrSiteMeta) -> bool,
    ) -> Result<CompiledFrame, JitVerdict> {
        let tfunc = analyze_admitted(code, resolve, probes, slot_member)?;
        if engine.is_none() {
            *engine = JitEngine::new();
        }
        let Some(engine) = engine.as_mut() else {
            return Err(JitVerdict::UnsupportedOpcode(NO_HOST_ENGINE));
        };
        engine.compile_tfunc_methods(&tfunc, direct, direct_method, inline_method)
    }

    /// Compile an already-analyzed [`TFunc`] (also the unit-test entry).
    pub fn compile_tfunc(&mut self, tfunc: &TFunc) -> Result<CompiledFrame, JitVerdict> {
        self.compile_tfunc_direct(tfunc, &mut |_| None)
    }

    /// [`Self::compile_tfunc`] with direct-call targets (see
    /// [`Self::compile_frame_direct`]).
    pub fn compile_tfunc_direct(
        &mut self,
        tfunc: &TFunc,
        direct: &mut dyn FnMut(u32) -> Option<DirectLeaf>,
    ) -> Result<CompiledFrame, JitVerdict> {
        self.compile_tfunc_methods(tfunc, direct, &mut |_| None, &mut |_, _, _| None)
    }

    /// [`Self::compile_tfunc_direct`] with direct method targets (see
    /// [`Self::compile_frame_direct`]).
    pub fn compile_tfunc_methods(
        &mut self,
        tfunc: &TFunc,
        direct: &mut dyn FnMut(u32) -> Option<DirectLeaf>,
        direct_method: &mut dyn FnMut(u32) -> Option<DirectLeaf>,
        inline_method: &mut dyn FnMut(u32, &[JitType], u32) -> Option<InlineMethod>,
    ) -> Result<CompiledFrame, JitVerdict> {
        // These operations each lower to a dedicated embedder helper.
        // Reject missing registrations before embedding an absolute address.
        for stmt in tfunc.blocks.iter().flat_map(|block| &block.stmts) {
            let required = match stmt.op {
                TOp::PushConstStr { .. } => Some((
                    runtime::const_str_helper_addr(),
                    "str constant (no helper registered)",
                )),
                TOp::PushConstTuple { .. } => Some((
                    runtime::const_tuple_helper_addr(),
                    "tuple constant (no helper registered)",
                )),
                TOp::TupleLen => Some((
                    runtime::tuple_len_helper_addr(),
                    "tuple length (no helper registered)",
                )),
                TOp::UnboxInt { .. } => Some((
                    runtime::unbox_int_helper_addr(),
                    "integer guard (no helper registered)",
                )),
                TOp::UnboxFloat { .. } => Some((
                    runtime::unbox_float_helper_addr(),
                    "float guard (no helper registered)",
                )),
                TOp::DynBinary { .. } => Some((
                    runtime::dyn_binop_helper_addr(),
                    "generic binary op (no helper registered)",
                )),
                TOp::DynCompare { .. } => Some((
                    runtime::dyn_compare_helper_addr(),
                    "generic comparison (no helper registered)",
                )),
                TOp::DynGetItem => Some((
                    runtime::dyn_getitem_helper_addr(),
                    "generic subscript (no helper registered)",
                )),
                TOp::DynSetItem => Some((
                    runtime::dyn_setitem_helper_addr(),
                    "generic item store (no helper registered)",
                )),
                TOp::DynUnary { .. } => Some((
                    runtime::dyn_unary_helper_addr(),
                    "generic unary op (no helper registered)",
                )),
                TOp::CallDyn {
                    int_result: true, ..
                } => Some((
                    runtime::call_dyn_int_helper_addr(),
                    "integer call (no helper registered)",
                )),
                _ => None,
            };
            if let Some((0, reason)) = required {
                return Err(JitVerdict::UnsupportedOpcode(reason));
            }
        }
        // A frame with native Python-to-Python calls needs the embedder's
        // call helper burned in (RFC 0059 WS3).
        if !tfunc.callee_spans.is_empty() && runtime::call_py_helper_addr() == 0 {
            return Err(JitVerdict::UnsupportedOpcode("CALL (no helper registered)"));
        }
        // RFC 0061 WS5 — likewise for the pinned-list helpers.
        let has_list_ops = tfunc.blocks.iter().any(|b| {
            b.stmts
                .iter()
                .any(|s| matches!(s.op, TOp::ListGet { .. } | TOp::ListSet))
        });
        if has_list_ops
            && (runtime::list_get_helper_addr() == 0 || runtime::list_set_helper_addr() == 0)
        {
            return Err(JitVerdict::UnsupportedOpcode(
                "SUBSCR (no list helper registered)",
            ));
        }
        // RFC 0065 WS5 — the length/append and attribute helpers.
        let has_list_extra = tfunc.blocks.iter().any(|b| {
            b.stmts
                .iter()
                .any(|s| matches!(s.op, TOp::ListLen | TOp::ListAppend))
        });
        if has_list_extra
            && (runtime::list_len_helper_addr() == 0 || runtime::list_append_helper_addr() == 0)
        {
            return Err(JitVerdict::UnsupportedOpcode(
                "len/append (no helper registered)",
            ));
        }
        let has_attr_ops = tfunc.blocks.iter().any(|b| {
            b.stmts
                .iter()
                .any(|s| matches!(s.op, TOp::AttrGet { .. } | TOp::AttrSet { .. }))
        });
        if has_attr_ops
            && (runtime::attr_get_helper_addr() == 0 || runtime::attr_set_helper_addr() == 0)
        {
            return Err(JitVerdict::UnsupportedOpcode("ATTR (no helper registered)"));
        }
        // RFC 0069 WS1 — the method-call helper.
        if !tfunc.method_sites.is_empty() && runtime::call_method_helper_addr() == 0 {
            return Err(JitVerdict::UnsupportedOpcode(
                "CALL method (no helper registered)",
            ));
        }
        // RFC 0069 WS2 — the libm-backed sin/cos and the Python-
        // semantics float floor-div / mod helpers.
        let needs_math_unary = tfunc.blocks.iter().any(|b| {
            b.stmts
                .iter()
                .any(|s| matches!(s.op, TOp::MathIntrinsic(MathFunc::Sin | MathFunc::Cos)))
        });
        if needs_math_unary
            && (runtime::math_sin_helper_addr() == 0 || runtime::math_cos_helper_addr() == 0)
        {
            return Err(JitVerdict::UnsupportedOpcode(
                "math sin/cos (no helper registered)",
            ));
        }
        // RFC 0074 — the frame-coverage helpers (obj globals, the
        // opaque-call lane, dynamic attributes, generic/pair
        // iteration, str %-format and slice), checked as a group.
        let needs_wave10 = tfunc.blocks.iter().any(|b| {
            matches!(b.term, crate::ir::TTerm::ForIterPair { .. })
                || b.stmts.iter().any(|s| {
                    matches!(
                        s.op,
                        TOp::PushGlobalObj { .. }
                            | TOp::CallDyn { .. }
                            | TOp::DynAttrGet { .. }
                            | TOp::DynAttrSet { .. }
                            | TOp::StrMod
                            | TOp::StrSlice { .. }
                            | TOp::IterCapture {
                                materialize: true,
                                ..
                            }
                    )
                })
        });
        if needs_wave10
            && (runtime::call_dyn_helper_addr() == 0
                || runtime::dyn_attr_get_helper_addr() == 0
                || runtime::dyn_attr_set_helper_addr() == 0
                || runtime::global_obj_helper_addr() == 0
                || runtime::iter_new_helper_addr() == 0
                || runtime::iter_next_pair_helper_addr() == 0
                || runtime::str_mod_helper_addr() == 0
                || runtime::str_slice_helper_addr() == 0)
        {
            return Err(JitVerdict::UnsupportedOpcode(
                "frame coverage (no helper registered)",
            ));
        }
        if runtime::is_obj_helper_addr() == 0
            && tfunc
                .blocks
                .iter()
                .any(|b| b.stmts.iter().any(|s| matches!(s.op, TOp::IsObj { .. })))
        {
            return Err(JitVerdict::UnsupportedOpcode(
                "IS_OP (no helper registered)",
            ));
        }
        let needs_float_divmod = tfunc.blocks.iter().any(|b| {
            b.stmts
                .iter()
                .any(|s| matches!(s.op, TOp::FloatArith(ArithKind::FloorDiv | ArithKind::Mod)))
        });
        if needs_float_divmod
            && (runtime::float_floordiv_helper_addr() == 0 || runtime::float_mod_helper_addr() == 0)
        {
            return Err(JitVerdict::UnsupportedOpcode(
                "float floordiv/mod (no helper registered)",
            ));
        }
        self.func.clear();

        // Signature: (frame: ptr) -> i64.
        self.func.signature.params.push(AbiParam::new(self.ptr_ty));
        self.func.signature.returns.push(AbiParam::new(types::I64));
        let name = format!("wpjit_{}", self.next_id);
        self.next_id += 1;
        // Every compiled frame has this signature: a direct call of one
        // (the frame itself, or a leaf) names it.
        let own_sig = self.func.signature.clone();
        let import = |func: &mut Function, target: FuncTarget| -> FuncRef {
            let mut b = weavepy_codegen::FunctionBuilder::new(func);
            let sig = b.import_signature(own_sig.clone());
            b.import_function(ExtFuncData { target, sig })
        };
        // A scalar frame calls itself directly.
        let self_func = (runtime::self_call_helper_addrs().is_some()
            && self_direct_eligible(tfunc))
        .then(|| import(&mut self.func, FuncTarget::SelfFn));
        // Direct leaf targets, per call token whose sites agree with the
        // leaf's arity and result lane (the lowering checks the argument
        // lanes at each site).
        let mut leaves: Vec<(u32, FuncRef, DirectLeaf)> = Vec::new();
        if runtime::self_call_helper_addrs().is_some() {
            for stmt in tfunc.blocks.iter().flat_map(|b| &b.stmts) {
                // A site that leaves parameters out binds the defaults the
                // leaf carries (checked per site by the lowering).
                let (token, argc, ret) = match stmt.op {
                    TOp::CallPy {
                        token,
                        argc,
                        ret,
                        is_self: false,
                    } => (token, argc, ret),
                    TOp::CallPyKw {
                        token,
                        argc,
                        kwc,
                        ret,
                        ..
                    } => (token, argc + kwc, ret),
                    _ => continue,
                };
                if leaves.iter().any(|(t, ..)| *t == token) {
                    continue;
                }
                if let Some(leaf) = direct(token) {
                    if leaf.params.len() >= argc as usize && leaf.ret == ret {
                        let fref = import(&mut self.func, FuncTarget::Addr(leaf.code));
                        leaves.push((token, fref, leaf));
                    }
                }
            }
        }

        // Direct method targets, per method token whose sites pass the
        // leaf's parameters (lanes checked per site) and expect its result
        // lane.
        if runtime::self_call_helper_addrs().is_some() && runtime::method_enter_helper_addr() != 0 {
            let mut seen: Vec<u32> = Vec::new();
            for stmt in tfunc.blocks.iter().flat_map(|b| &b.stmts) {
                let TOp::CallMethod {
                    token,
                    argc,
                    ret: crate::ir::MethodRet::Scalar(ret),
                } = stmt.op
                else {
                    continue;
                };
                if token & runtime::METHOD_NATIVE != 0 || seen.contains(&token) {
                    continue;
                }
                seen.push(token);
                if let Some(leaf) = direct_method(token) {
                    if leaf.receiver && leaf.params.len() == argc as usize && leaf.ret == ret {
                        let fref = import(&mut self.func, FuncTarget::Addr(leaf.code));
                        leaves.push((token, fref, leaf));
                    }
                }
            }
        }

        let sound = build_function(
            &mut self.func,
            tfunc,
            self.ptr_ty,
            self_func,
            leaves
                .into_iter()
                .map(|(token, fref, leaf)| crate::lower::LeafTarget {
                    token,
                    func: fref,
                    n_locals: leaf.n_locals,
                    max_stack: leaf.max_stack,
                    params: leaf.params,
                    defaults: leaf.defaults,
                    receiver: leaf.receiver,
                })
                .collect(),
            inline_method,
        );
        if !sound {
            self.release();
            return Err(JitVerdict::NotConverged);
        }
        if std::env::var_os("WEAVEPY_JIT_CLIF_DUMP").is_some() {
            eprintln!("jit ir {name}:\n{}", self.func.display());
        }
        let stats = std::env::var_os("WEAVEPY_JIT_CLIF_STATS").is_some();
        let t0 = stats.then(std::time::Instant::now);
        let n_insts = self.func.dfg.num_insts();
        let mut func = mem::take(&mut self.func);
        let built = weavepy_codegen::compile(&mut func).and_then(|bytes| self.code.install(&bytes));
        self.func = func;
        self.release();
        let code_ptr = built.map_err(|_| JitVerdict::NotConverged)?;
        if let Some(t0) = t0 {
            eprintln!("jit compile {name}: {n_insts} insts in {:?}", t0.elapsed());
        }
        // SAFETY: `code_ptr` is the entry of machine code generated for
        // exactly the `(*mut JitFrame) -> i64` signature declared above,
        // in a code arena never freed.
        let func: NativeFn = unsafe { mem::transmute::<*const u8, NativeFn>(code_ptr) };

        let has_loop = tfunc.blocks.iter().enumerate().any(|(bi, b)| match b.term {
            TTerm::ForRange { .. }
            | TTerm::ForList { .. }
            | TTerm::ForIter { .. }
            | TTerm::ForIterPair { .. } => true,
            TTerm::Jump(t) => t <= bi,
            TTerm::BranchFalse {
                target,
                fallthrough,
            }
            | TTerm::BranchTrue {
                target,
                fallthrough,
            } => target <= bi || fallthrough <= bi,
            _ => false,
        });
        let round_trips = tfunc.blocks.iter().flat_map(|b| &b.stmts).any(|s| {
            matches!(
                s.op,
                TOp::CallDyn { .. }
                    | TOp::DynAttrGet { .. }
                    | TOp::DynAttrSet { .. }
                    | TOp::ContainsDyn { .. }
                    | TOp::DynBinary { .. }
                    | TOp::DynCompare { .. }
                    | TOp::DynGetItem
                    | TOp::DynSetItem
                    | TOp::DynUnary { .. }
            )
        });
        let interp_entry = (has_loop || !round_trips) && tfunc.pc0_entry;
        Ok(CompiledFrame {
            func,
            livein: tfunc.livein_locals.clone(),
            local_types: tfunc.local_types.clone(),
            max_stack: tfunc.max_stack,
            n_locals: tfunc.n_locals,
            global_guards: tfunc.global_guards.clone(),
            range_loops: tfunc.range_loops.clone(),
            list_loops: tfunc.list_loops.clone(),
            iter_loops: tfunc.iter_loops.clone(),
            comp_saved: tfunc.comp_saved.clone(),
            comp_target_slots: tfunc.comp_target_slots.clone(),
            cold_exits: tfunc.cold_exits.clone(),
            region_exits: tfunc.region_exits.clone(),
            region_roots: tfunc.region_roots.clone(),
            stayed_heads: tfunc.stayed_heads.clone(),
            env_heads: tfunc.env_heads.clone(),
            interp_entry,
            callee_spans: tfunc.callee_spans.clone(),
            len_spans: tfunc.len_spans.clone(),
            method_spans: tfunc.method_spans.clone(),
            attr_sites: tfunc.attr_sites.clone(),
            method_sites: tfunc.method_sites.clone(),
            str_method_sites: tfunc.str_method_sites.clone(),
            str_method_spans: tfunc.str_method_spans.clone(),
            math_guards: tfunc.math_guards.clone(),
            math_spans: tfunc.math_spans.clone(),
            null_spans: tfunc.null_spans.clone(),
            osr_entries: tfunc.osr_entries.clone(),
            resume_entries: tfunc.resume_entries.clone(),
            max_call_args: tfunc.max_call_args,
            ret_lane: tfunc.ret_lane,
            ret_none: tfunc.ret_none,
            scalar_leaf: is_scalar_leaf(tfunc),
            op_mix: op_mix(tfunc),
            code: code_ptr as usize,
        })
    }
}

/// Whether native code would cost a loop-free body more than it saves:
/// it makes a dynamic Python call, which from native code takes the
/// interpreter's generic call path (a nested run instead of the inline
/// activation an interpreted caller uses), while the compile itself is
/// the largest cost a short-lived method ever pays.
fn calls_dynamically(tfunc: &TFunc) -> bool {
    let has_loop = !tfunc.range_loops.is_empty()
        || !tfunc.list_loops.is_empty()
        || !tfunc.iter_loops.is_empty()
        || tfunc.blocks.iter().enumerate().any(|(i, b)| {
            use crate::ir::TTerm;
            match b.term {
                TTerm::Jump(t) => t as usize <= i,
                TTerm::BranchFalse {
                    target,
                    fallthrough,
                }
                | TTerm::BranchTrue {
                    target,
                    fallthrough,
                } => target as usize <= i || fallthrough as usize <= i,
                _ => false,
            }
        });
    if has_loop {
        return false;
    }
    op_mix(tfunc).dyn_calls > 0
}

/// Whether `tfunc`'s loops run operations generic often enough that
/// native code would run them slower than the interpreter (see
/// [`generic_share_limit`]): each generic operation (a dynamic call or
/// attribute access, an operation on a value of no known type) goes
/// through a helper that redoes the lookups the interpreter's caches
/// remember, while each typed one saves only a dispatch. A loop-free
/// body is left alone: other compiled code calls it directly.
/// The [`JitEngine::compile_frame_lazily`] verdict for a host the code
/// generator can't target.
pub const NO_HOST_ENGINE: &str = "no code generator for the host";

/// Analyze `code` for [`JitEngine::compile_frame_direct`] and apply the
/// admission rules that need no code generator.
fn analyze_admitted(
    code: &CodeObject,
    resolve: &mut dyn FnMut(&str) -> ResolvedGlobal,
    probes: &mut Probes<'_>,
    slot_member: &mut dyn FnMut(&AttrSiteMeta) -> bool,
) -> Result<TFunc, JitVerdict> {
    let mut tfunc = crate::analyze::analyze_frame(code, resolve, probes)?;
    if calls_dynamically(&tfunc) {
        return Err(JitVerdict::UnsupportedOpcode("dynamic call (loop-free)"));
    }
    if mostly_generic(&tfunc) {
        return Err(JitVerdict::UnsupportedOpcode("mostly generic operations"));
    }
    if attr_bound(&tfunc) {
        return Err(JitVerdict::UnsupportedOpcode("attribute-bound loop"));
    }
    if unpacks_pairs(&tfunc) {
        return Err(JitVerdict::UnsupportedOpcode("tuple-target loop"));
    }
    for site in &mut tfunc.attr_sites {
        site.slot_member = slot_member(site);
    }
    Ok(tfunc)
}

/// Whether `tfunc` runs a tuple-target loop (`for a, b in pairs`) natively.
/// Its step goes through a helper that pins each unpacked object, three
/// times the frame JIT's step, and loops that iterate pairs (a graph's
/// `(neighbor, weight)` edges, a sparse row's `(column, value)` entries)
/// do little typed work per pair to make up for it: compiling them cost
/// `graph_search` a tenth and `scimark`'s sparse product a twentieth. The
/// frame JIT runs such code instead. A pair of scalars (`enumerate`'s index
/// and a byte) steps without pins, and a function with enough typed
/// arithmetic repays the step (an n-body pair loop): tier 2 runs those
/// several times faster than the frame JIT, so it keeps them.
fn unpacks_pairs(tfunc: &TFunc) -> bool {
    use crate::value::JitType;
    if GENERIC_LOOPS_ADMITTED.load(std::sync::atomic::Ordering::Relaxed) {
        return false;
    }
    // (A pair of scalars, `enumerate`'s index and a byte or a number,
    // steps without pins.)
    let scalar = |t: JitType| matches!(t, JitType::Int | JitType::Float | JitType::Bool);
    let pins = tfunc.blocks.iter().any(|b| {
        matches!(b.term, crate::ir::TTerm::ForIterPair { elem1, elem2, .. }
            if !scalar(elem1) || !scalar(elem2))
    });
    pins && typed_arith(tfunc) < PAIR_LOOP_TYPED_WORK
}

/// The typed arithmetic statements a function with a tuple-target loop
/// needs before tier 2 takes it (see [`unpacks_pairs`]).
const PAIR_LOOP_TYPED_WORK: usize = 12;

/// How many of `tfunc`'s statements are typed int or float arithmetic and
/// compares.
fn typed_arith(tfunc: &TFunc) -> usize {
    use crate::ir::TOp;
    tfunc
        .blocks
        .iter()
        .flat_map(|b| &b.stmts)
        .filter(|s| {
            matches!(
                s.op,
                TOp::IntArith(_)
                    | TOp::FloatArith(_)
                    | TOp::IntTrueDiv
                    | TOp::IntCmp(_)
                    | TOp::FloatCmp(_)
                    | TOp::IntNeg
                    | TOp::FloatNeg
                    | TOp::IntInvert
            )
        })
        .count()
}

fn mostly_generic(tfunc: &TFunc) -> bool {
    let Some((percent, loops_only)) = generic_share_limit() else {
        return false;
    };
    if !has_loop(tfunc) {
        return false;
    }
    let mix = if loops_only {
        mix_of(loop_blocks(tfunc))
    } else {
        op_mix(tfunc)
    };
    (mix.dyn_calls + mix.dyn_attrs + mix.dyn_other) * 100 > mix.total * percent
}

/// The blocks of `tfunc` that some back edge spans (blocks are in
/// instruction order).
fn loop_blocks(tfunc: &TFunc) -> impl Iterator<Item = &crate::ir::TBlock> {
    let mut in_loop = vec![false; tfunc.blocks.len()];
    for (i, b) in tfunc.blocks.iter().enumerate() {
        use crate::ir::TTerm;
        let back = |t: usize| (t <= i).then_some(t);
        let top = match b.term {
            TTerm::Jump(t) => back(t),
            TTerm::BranchFalse {
                target,
                fallthrough,
            }
            | TTerm::BranchTrue {
                target,
                fallthrough,
            } => back(target).into_iter().chain(back(fallthrough)).min(),
            _ => None,
        };
        if let Some(top) = top {
            in_loop[top..=i].iter_mut().for_each(|x| *x = true);
        }
    }
    tfunc
        .blocks
        .iter()
        .zip(in_loop)
        .filter(|(_, l)| *l)
        .map(|(b, _)| b)
}

/// Whether `tfunc`'s loops access attributes more often than they do
/// typed arithmetic: each attribute read or write of a tier-2 loop goes
/// through a helper that redoes its guards (about 50 host instructions),
/// where the frame JIT reads or writes the field in line, so a loop of
/// attribute traffic runs faster in the frame JIT unless typed work in
/// registers repays it. (Calls don't count: a driver loop that calls
/// compiled functions keeps them native-to-native in tier 2.)
/// `WEAVEPY_T2_ATTRS=0` turns the rule off, a tuning aid.
fn attr_bound(tfunc: &TFunc) -> bool {
    use crate::ir::TOp;
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if GENERIC_LOOPS_ADMITTED.load(std::sync::atomic::Ordering::Relaxed)
        || ATTR_LOOPS_ADMITTED.load(std::sync::atomic::Ordering::Relaxed)
        || !*ON.get_or_init(|| std::env::var_os("WEAVEPY_T2_ATTRS").is_none_or(|v| v != "0"))
        || !has_loop(tfunc)
    {
        return false;
    }
    let (mut attrs, mut typed) = (0usize, 0usize);
    for st in loop_blocks(tfunc).flat_map(|b| &b.stmts) {
        match st.op {
            TOp::AttrGet { .. }
            | TOp::AttrSet { .. }
            | TOp::DynAttrGet { .. }
            | TOp::DynAttrSet { .. } => attrs += 1,
            TOp::IntArith(_)
            | TOp::FloatArith(_)
            | TOp::IntTrueDiv
            | TOp::IntCmp(_)
            | TOp::FloatCmp(_)
            | TOp::IntNeg
            | TOp::FloatNeg
            | TOp::IntInvert => typed += 1,
            _ => {}
        }
    }
    attrs > typed
}

/// The generic share [`mostly_generic`] admits, in percent, and whether it
/// counts only loop bodies: 20% of the whole frame unless
/// `WEAVEPY_T2_GENERIC` says otherwise (`off`, or a percent with an
/// optional `loop` suffix, as in `12loop`) or
/// [`admit_generic_loops`] turned the rule off.
fn generic_share_limit() -> Option<(u32, bool)> {
    static LIMIT: std::sync::OnceLock<Option<(u32, bool)>> = std::sync::OnceLock::new();
    if GENERIC_LOOPS_ADMITTED.load(std::sync::atomic::Ordering::Relaxed) {
        return None;
    }
    *LIMIT.get_or_init(|| {
        let v = std::env::var("WEAVEPY_T2_GENERIC").unwrap_or_else(|_| "20loop".to_owned());
        let (num, loops_only) = match v.strip_suffix("loop") {
            Some(n) => (n, true),
            None => (v.as_str(), false),
        };
        num.parse().ok().map(|p| (p, loops_only))
    })
}

/// Set by [`admit_attr_loops`].
static ATTR_LOOPS_ADMITTED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Compile loops however attribute-bound they are (see [`attr_bound`]),
/// from now on in this process: for tests of tier 2's attribute paths.
pub fn admit_attr_loops() {
    ATTR_LOOPS_ADMITTED.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// Set by [`admit_generic_loops`].
static GENERIC_LOOPS_ADMITTED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Compile loops however generic their operations are, from now on in
/// this process: for tests that exercise the generic helpers in compiled
/// loops (see [`mostly_generic`]).
pub fn admit_generic_loops() {
    GENERIC_LOOPS_ADMITTED.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// Whether [`admit_generic_loops`] ran: the VM then keeps call-dense
/// loops native too, for the tests of its native calls.
pub fn generic_loops_admitted() -> bool {
    GENERIC_LOOPS_ADMITTED.load(std::sync::atomic::Ordering::Relaxed)
}

/// Whether `tfunc` loops (a rewritten loop, or a backward jump).
fn has_loop(tfunc: &TFunc) -> bool {
    !tfunc.range_loops.is_empty()
        || !tfunc.list_loops.is_empty()
        || !tfunc.iter_loops.is_empty()
        || tfunc.blocks.iter().enumerate().any(|(i, b)| {
            use crate::ir::TTerm;
            match b.term {
                TTerm::Jump(t) => t <= i,
                TTerm::BranchFalse {
                    target,
                    fallthrough,
                }
                | TTerm::BranchTrue {
                    target,
                    fallthrough,
                } => target <= i || fallthrough <= i,
                _ => false,
            }
        })
}

/// `(generic, total)`: statements that hand an operation to the
/// interpreter's generic object protocol (dynamic calls and attribute
/// accesses) against all statements (see [`CompiledFrame::op_mix`]).
fn op_mix(tfunc: &TFunc) -> OpMix {
    if std::env::var_os("WEAVEPY_JIT_DUMP").is_some() {
        for (bi, b) in tfunc.blocks.iter().enumerate() {
            eprintln!("jit block {bi} entry {:?}", b.entry_stack);
            for st in &b.stmts {
                eprintln!("    {:4} {:?}", st.pc, st.op);
            }
            eprintln!("    term {:?}", b.term);
        }
    }
    if std::env::var_os("WEAVEPY_JIT_OPS").is_some() {
        let mut hist: std::collections::BTreeMap<String, u32> = std::collections::BTreeMap::new();
        for b in &tfunc.blocks {
            for st in &b.stmts {
                let name = format!("{:?}", st.op);
                let name = name.split(['(', ' ', '{']).next().unwrap_or("").to_owned();
                *hist.entry(name).or_default() += 1;
            }
        }
        eprintln!("jit ops {hist:?}");
    }
    mix_of(tfunc.blocks.iter())
}

/// [`op_mix`] over `blocks`' statements.
fn mix_of<'a>(blocks: impl Iterator<Item = &'a crate::ir::TBlock>) -> OpMix {
    let mut mix = OpMix::default();
    for b in blocks {
        for st in &b.stmts {
            mix.total += 1;
            match st.op {
                TOp::CallDyn { .. } => mix.dyn_calls += 1,
                TOp::DynAttrGet { .. } | TOp::DynAttrSet { .. } => mix.dyn_attrs += 1,
                TOp::ContainsDyn { .. }
                | TOp::DynBinary { .. }
                | TOp::DynCompare { .. }
                | TOp::DynGetItem
                | TOp::DynSetItem
                | TOp::DynUnary { .. } => {
                    mix.dyn_other += 1;
                }
                TOp::CallPy { .. }
                | TOp::CallPyKw { .. }
                | TOp::CallMethod { .. }
                | TOp::ObjGetItem { .. } => {
                    mix.guarded_calls += 1;
                }
                _ => {}
            }
        }
    }
    mix
}

/// Statement counts by how the compiled code runs them (see
/// [`CompiledFrame::op_kinds`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OpMix {
    /// `CallDyn`: the interpreter's generic call protocol.
    pub dyn_calls: u32,
    /// `DynAttrGet` / `DynAttrSet`: generic attribute access.
    pub dyn_attrs: u32,
    /// Other generic protocol statements (`ContainsDyn`).
    pub dyn_other: u32,
    /// Calls through guarded direct lanes (`CallPy`, `CallPyKw`,
    /// `CallMethod`).
    pub guarded_calls: u32,
    /// Every statement.
    pub total: u32,
}

fn is_scalar_leaf(tfunc: &TFunc) -> bool {
    let [block] = tfunc.blocks.as_slice() else {
        return false;
    };
    tfunc.entry_block == 0
        && block.entry_stack.is_empty()
        && block.stmts.len() <= 64
        && matches!(block.term, crate::ir::TTerm::Return)
        && matches!(tfunc.ret_lane, Some(JitType::Int | JitType::Bool | JitType::Float))
        && !tfunc.ret_none
        && tfunc.local_types.iter().all(|ty| {
            matches!(ty, None | Some(JitType::Int | JitType::Bool | JitType::Float))
        })
        && tfunc.range_loops.is_empty()
        && tfunc.list_loops.is_empty()
        && tfunc.iter_loops.is_empty()
        && tfunc.comp_saved.is_empty()
        && tfunc.resume_entries.is_empty()
        && tfunc.callee_spans.is_empty()
        && tfunc.len_spans.is_empty()
        && tfunc.method_spans.is_empty()
        && tfunc.attr_sites.is_empty()
        && tfunc.method_sites.is_empty()
        && tfunc.str_method_sites.is_empty()
        && tfunc.str_method_spans.is_empty()
        && tfunc.null_spans.is_empty()
        && tfunc.osr_entries.is_empty()
        && tfunc.max_call_args == 0
        // Keep an explicit allowlist: new operations must never silently
        // acquire permission to execute without an embedder context.
        && block.stmts.iter().all(|stmt| {
            matches!(
                stmt.op,
                TOp::PushConstInt(_)
                    | TOp::PushConstFloat(_)
                    | TOp::PushConstBool(_)
                    | TOp::LoadLocal(_)
                    | TOp::StoreLocal(_)
                    | TOp::IntArith(
                        ArithKind::Add
                            | ArithKind::Sub
                            | ArithKind::Mul
                            | ArithKind::FloorDiv
                            | ArithKind::Mod
                            | ArithKind::And
                            | ArithKind::Or
                            | ArithKind::Xor
                            | ArithKind::TrueDiv
                    )
                    | TOp::FloatArith(
                        ArithKind::Add | ArithKind::Sub | ArithKind::Mul | ArithKind::TrueDiv
                    )
                    | TOp::IntTrueDiv
                    | TOp::IntCmp(_)
                    | TOp::FloatCmp(_)
                    | TOp::IntNeg
                    | TOp::FloatNeg
                    | TOp::IntInvert
                    | TOp::IntNot
                    | TOp::FloatNot
                    | TOp::Pop
                    | TOp::Dup { .. }
                    | TOp::Swap2
                    | TOp::SwapN { .. }
                    | TOp::IntToFloatTos { .. }
                    | TOp::IntToFloatSecond { .. }
                    | TOp::MathIntrinsic(
                        crate::ir::MathFunc::Sqrt
                            | crate::ir::MathFunc::Fabs
                            | crate::ir::MathFunc::Sin
                            | crate::ir::MathFunc::Cos
                    )
            )
        })
}

/// Whether `tfunc` may call itself directly (see
/// [`crate::runtime::SelfEnterHelper`]): it makes a self call, and every
/// local, operand and result is a scalar and every operation pure scalar
/// work or a self call, so no activation of it ever pins an object. Its
/// activations can then share one embedder context (the pin table, the
/// parked result) and live on the native stack.
fn self_direct_eligible(tfunc: &TFunc) -> bool {
    let scalar = |t: JitType| matches!(t, JitType::Int | JitType::Float | JitType::Bool);
    let mut any_self = false;
    let ops_ok = tfunc.blocks.iter().all(|b| {
        let term_ok = matches!(
            b.term,
            TTerm::Return
                | TTerm::Jump(_)
                | TTerm::BranchFalse { .. }
                | TTerm::BranchTrue { .. }
                | TTerm::Deopt { .. }
        );
        term_ok
            && b.stmts.iter().all(|st| match st.op {
                TOp::CallPy {
                    is_self: true, ret, ..
                } => {
                    any_self = true;
                    scalar(ret)
                }
                TOp::PushConstInt(_)
                | TOp::PushConstFloat(_)
                | TOp::PushConstBool(_)
                | TOp::LoadLocal(_)
                | TOp::StoreLocal(_)
                | TOp::IntArith(_)
                | TOp::FloatArith(_)
                | TOp::IntTrueDiv
                | TOp::IntCmp(_)
                | TOp::FloatCmp(_)
                | TOp::IntNeg
                | TOp::FloatNeg
                | TOp::IntInvert
                | TOp::IntNot
                | TOp::FloatNot
                | TOp::Pop
                | TOp::Dup { .. }
                | TOp::Swap2
                | TOp::SwapN { .. }
                | TOp::IntToFloatTos { .. }
                | TOp::IntToFloatSecond { .. } => true,
                _ => false,
            })
    });
    any_self
        && ops_ok
        && tfunc.ret_lane.is_some_and(scalar)
        && tfunc.local_types.iter().flatten().all(|t| scalar(*t))
}

#[cfg(test)]
mod tests;

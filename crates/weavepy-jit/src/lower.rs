//! Lower the typed IR ([`TFunc`]) to a Cranelift function.
//!
//! Locals become Cranelift *variables* (the SSA builder inserts phis at
//! merges); the operand stack is an explicit `Vec` of SSA values.
//! Cross-block operand values (RFC 0069 WS2 — ternaries, short-circuit
//! chains) are carried as Cranelift *block parameters*: each block
//! declares one param per [`crate::ir::TBlock::entry_stack`] lane and
//! every jump/branch passes the live stack as block arguments. Integer
//! arithmetic is emitted with explicit overflow / divide-by-zero checks
//! that branch to per-op *side-exit* blocks; a side exit writes the live
//! locals + spilled stack back into the [`JitFrame`] and returns
//! [`JitStatus::Deopt`] so the interpreter resumes at the exact pc.

use cranelift_codegen::entity::EntityRef;
use cranelift_codegen::ir::condcodes::{FloatCC, IntCC};
use cranelift_codegen::ir::{
    types, AbiParam, Block, BlockArg, FuncRef, Function, InstBuilder, MemFlags, SigRef, Signature,
    StackSlot, StackSlotData, StackSlotKind, Type, Value,
};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext, Variable};

use crate::engine::{FieldAt, InlineMethod};
use crate::ir::{ArithKind, CmpKind, MathFunc, MethodRet, SliceOrigin, TFunc, TOp, TStmt, TTerm};
use crate::runtime::{self, JitFrame, JitStatus, SlotTag};
use crate::value::JitType;

const OFF_LOCALS: i32 = core::mem::offset_of!(JitFrame, locals) as i32;
const OFF_ENTRY_PC: i32 = core::mem::offset_of!(JitFrame, entry_pc) as i32;
const OFF_RET_BITS: i32 = core::mem::offset_of!(JitFrame, ret_bits) as i32;
const OFF_RET_TAG: i32 = core::mem::offset_of!(JitFrame, ret_tag) as i32;
const OFF_DEOPT_PC: i32 = core::mem::offset_of!(JitFrame, deopt_pc) as i32;
const OFF_STACK_SPILL: i32 = core::mem::offset_of!(JitFrame, stack_spill) as i32;
const OFF_STACK_TAGS: i32 = core::mem::offset_of!(JitFrame, stack_tags) as i32;
const OFF_STACK_LEN: i32 = core::mem::offset_of!(JitFrame, stack_len) as i32;
const OFF_CALL_ARGS: i32 = core::mem::offset_of!(JitFrame, call_args) as i32;
const OFF_CALL_TAGS: i32 = core::mem::offset_of!(JitFrame, call_tags) as i32;
const OFF_N_LOCALS: i32 = core::mem::offset_of!(JitFrame, n_locals) as i32;
const OFF_STACK_CAP: i32 = core::mem::offset_of!(JitFrame, stack_cap) as i32;
const OFF_CTX: i32 = core::mem::offset_of!(JitFrame, ctx) as i32;

/// A call token whose sites may enter a compiled scalar leaf directly
/// (see `engine::CompiledFrame::direct_leaf`): the leaf's function in
/// this module, its frame layout, and its parameter lanes.
pub(crate) struct LeafTarget {
    pub token: u32,
    pub func: FuncRef,
    pub n_locals: u32,
    pub max_stack: u32,
    pub params: Vec<JitType>,
    /// Per parameter, the default a call that leaves it out binds.
    pub defaults: Vec<Option<u64>>,
    /// A method target (see `engine::CompiledFrame::direct_method_leaf`):
    /// `token` is a method token, local 0 the unread receiver, and
    /// `params` the parameters after it.
    pub receiver: bool,
}

/// Where a direct leaf call takes one parameter from: the call's `j`th
/// value (positionals, then keyword values), or a burned-in default.
#[derive(Clone, Copy)]
enum LeafArg {
    Arg(usize),
    Default(u64),
}

/// The scalar lane a method site expects back (a direct method target
/// returns nothing else); `None` for the procedure and object shapes.
fn leaf_ret_lane(ret: MethodRet) -> Option<JitType> {
    match ret {
        MethodRet::Scalar(t @ (JitType::Int | JitType::Float | JitType::Bool)) => Some(t),
        _ => None,
    }
}

/// A `CallPy`/`CallPyKw` site's operands, for its ordinary call.
#[derive(Clone, Copy)]
struct SiteCall {
    token: u32,
    argc: u8,
    kwc: u8,
    perm: u32,
    gaps: u32,
}

/// Build the Cranelift function body for `tfunc` into `func`; `false` when
/// the body assigned a local the write-back plan missed (the build must be
/// discarded).
pub(crate) fn build_function(
    func: &mut Function,
    fbctx: &mut FunctionBuilderContext,
    tfunc: &TFunc,
    ptr_ty: Type,
    self_func: Option<FuncRef>,
    leaves: Vec<LeafTarget>,
    inline_method: &mut dyn FnMut(u32, &[JitType], u32) -> Option<InlineMethod>,
) -> bool {
    let mut builder = FunctionBuilder::new(func, fbctx);
    let mut lc = Lowerer::new(&mut builder, tfunc, ptr_ty);
    lc.self_func = self_func;
    lc.leaves = leaves;
    lc.inline_method = Some(inline_method);
    lc.build();
    let sound = !lc.unlisted_assignment;
    builder.seal_all_blocks();
    builder.finalize();
    sound
}

struct Lowerer<'a, 'b> {
    b: &'a mut FunctionBuilder<'b>,
    tfunc: &'a TFunc,
    ptr_ty: Type,
    /// One Cranelift block per (reachable) TBlock.
    cl_blocks: Vec<Block>,
    /// One variable per managed local slot (others unused).
    vars: Vec<Option<Variable>>,
    frame_ptr: Value,
    locals_base: Value,
    spill_base: Value,
    tags_base: Value,
    /// Argument marshal bases (RFC 0059 WS3); only loaded when the
    /// function contains `CallPy` statements.
    call_args_base: Value,
    call_tags_base: Value,
    /// Imported signature of the `wpjit_call_py` helper (lazy).
    call_sig: Option<SigRef>,
    /// RFC 0074 WS2 — imported signature of the `wpjit_call_dyn`
    /// helper (lazy).
    call_dyn_sig: Option<SigRef>,
    /// RFC 0069 WS1 — imported signature of the `wpjit_call_method`
    /// helper (lazy).
    call_method_sig: Option<SigRef>,
    /// RFC 0069 WS2 — imported `f64 -> f64` signature of the libm
    /// `sin`/`cos` helpers (lazy).
    math_unary_sig: Option<SigRef>,
    /// RFC 0069 WS2 — imported `(f64, f64) -> f64` signature of the
    /// float floor-div / mod helpers (lazy).
    math_binary_sig: Option<SigRef>,
    /// Imported signature shared by the `wpjit_list_get`/`_set` and
    /// `wpjit_attr_get`/`_set` helpers — all `(frame, i64, i64) -> i64`
    /// (RFC 0061/0065 WS5, lazy).
    list_sig: Option<SigRef>,
    /// Imported signature shared by the `wpjit_list_len`/`_append`
    /// helpers — `(frame, pin) -> i64` (RFC 0065 WS5, lazy).
    pin_sig: Option<SigRef>,
    /// RFC 0073 WS2 — imported signature shared by the dict-lane
    /// helpers — `(frame, pin, key, key_tag, val_tag) -> i64` (lazy).
    dict_sig: Option<SigRef>,
    /// RFC 0071 WS4 — imported signature shared by the
    /// `wpjit_build_list`/`wpjit_list_slice` helpers — all
    /// `(frame, i64, i64, i64) -> i64` (lazy).
    quad_sig: Option<SigRef>,
    /// RFC 0067 WS2 — imported `(frame) -> i64` signature of the
    /// eval-breaker poll helper (lazy).
    poll_sig: Option<SigRef>,
    /// RFC 0067 WS2 — the per-activation poll countdown register.
    /// `Some` only when the embedder registered a poll helper and the
    /// function has loop headers to instrument.
    poll_countdown: Option<Variable>,
    /// The abstract operand stack: SSA value + lane.
    vstack: Vec<(Value, JitType)>,
    /// This function itself, for direct self calls (see
    /// `engine::self_direct_eligible`); `None` when it makes none.
    self_func: Option<FuncRef>,
    /// The callee frame of a direct self call (lazy): its `JitFrame`
    /// then its buffers.
    self_slot: Option<StackSlot>,
    /// Imported signatures of the self-call helpers (lazy): `(frame) ->
    /// i64` and the slow path's `(frame, callee, status, token, tag) ->
    /// i64`.
    self_sig: Option<SigRef>,
    self_slow_sig: Option<SigRef>,
    /// Direct scalar-leaf call targets by token.
    leaves: Vec<LeafTarget>,
    /// The in-line body of a method site, by method token and the lanes
    /// of its arguments (see `engine::InlineMethod::of`).
    inline_method: Option<&'a mut dyn FnMut(u32, &[JitType], u32) -> Option<InlineMethod>>,
    /// While lowering an in-line method body: where its guards branch
    /// (the method helper's call) instead of a side exit.
    miss_redirect: Option<Block>,
    /// Per local slot, whether the body assigns it (see
    /// [`assigned_slots`]): a local it never assigns still holds the
    /// frame's own value, so exits and calls don't write it back.
    assignable: Vec<bool>,
    /// The slot each local's variable stands for, by variable index.
    var_slots: Vec<u32>,
    /// Set when the body assigned a local [`assigned_slots`] missed: the
    /// build is then unsound and `build_function` reports it.
    unlisted_assignment: bool,
}

/// Which local slots `tfunc`'s body assigns after the entry loads them:
/// every operation and loop step that defines a local's variable (see
/// [`Lowerer::def_local`], which checks the list).
fn assigned_slots(tfunc: &TFunc) -> Vec<bool> {
    let mut out = vec![false; tfunc.n_locals as usize];
    let mut mark = |slot: u32| {
        if let Some(a) = out.get_mut(slot as usize) {
            *a = true;
        }
    };
    for b in &tfunc.blocks {
        for st in &b.stmts {
            match st.op {
                TOp::StoreLocal(slot)
                | TOp::IterCapture {
                    iter_slot: slot, ..
                }
                | TOp::DictIterNew { iter_slot: slot } => mark(slot),
                _ => {}
            }
        }
        match b.term {
            TTerm::ForRange {
                cur_slot, var_slot, ..
            } => {
                mark(cur_slot);
                mark(var_slot);
            }
            TTerm::ForList {
                idx_slot, var_slot, ..
            } => {
                mark(idx_slot);
                mark(var_slot);
            }
            TTerm::ForIter { var_slot, .. } => mark(var_slot),
            TTerm::ForIterPair {
                var1_slot,
                var2_slot,
                ..
            } => {
                mark(var1_slot);
                mark(var2_slot);
            }
            _ => {}
        }
    }
    out
}

impl<'a, 'b> Lowerer<'a, 'b> {
    fn new(b: &'a mut FunctionBuilder<'b>, tfunc: &'a TFunc, ptr_ty: Type) -> Lowerer<'a, 'b> {
        // Placeholders overwritten at the top of `build` before any use.
        let dummy = Value::from_u32(0);
        Lowerer {
            b,
            tfunc,
            ptr_ty,
            cl_blocks: Vec::new(),
            vars: Vec::new(),
            frame_ptr: dummy,
            locals_base: dummy,
            spill_base: dummy,
            tags_base: dummy,
            call_args_base: dummy,
            call_tags_base: dummy,
            call_sig: None,
            call_dyn_sig: None,
            call_method_sig: None,
            math_unary_sig: None,
            math_binary_sig: None,
            list_sig: None,
            pin_sig: None,
            dict_sig: None,
            quad_sig: None,
            poll_sig: None,
            poll_countdown: None,
            vstack: Vec::new(),
            self_func: None,
            self_slot: None,
            self_sig: None,
            self_slow_sig: None,
            leaves: Vec::new(),
            inline_method: None,
            miss_redirect: None,
            assignable: Vec::new(),
            var_slots: Vec::new(),
            unlisted_assignment: false,
        }
    }

    /// Assign the managed local `var` (anything but its entry load).
    fn def_local(&mut self, var: Variable, v: Value) {
        let listed = self
            .var_slots
            .get(var.index())
            .and_then(|&slot| self.assignable.get(slot as usize))
            .copied()
            .unwrap_or(false);
        if !listed {
            debug_assert!(false, "an assignment `assigned_slots` doesn't list");
            self.unlisted_assignment = true;
        }
        self.b.def_var(var, v);
    }

    fn cl_ty(ty: JitType) -> Type {
        match ty {
            JitType::Float => types::F64,
            _ => types::I64,
        }
    }

    fn tag(ty: JitType) -> i64 {
        match ty {
            JitType::Int => SlotTag::Int as i64,
            JitType::Float => SlotTag::Float as i64,
            JitType::Bool => SlotTag::Bool as i64,
            JitType::ListInt
            | JitType::ListFloat
            | JitType::ListObj
            | JitType::ListListFloat
            | JitType::ListListInt => SlotTag::ListPin as i64,
            // RFC 0071 WS6 — `Str`/`Bytes` pins spill like object pins
            // (the rebuild resolves the pin to the real payload).
            // RFC 0073 WS2 — `Dict` pins ride the same tag.
            JitType::Obj | JitType::Str | JitType::Bytes | JitType::Dict => SlotTag::ObjPin as i64,
            JitType::Unknown => SlotTag::Int as i64,
        }
    }

    fn build(&mut self) {
        let trusted = MemFlags::trusted();

        // Entry / prologue block carries the function param (frame ptr).
        let entry = self.b.create_block();
        self.b.append_block_params_for_function_params(entry);
        self.b.switch_to_block(entry);
        self.frame_ptr = self.b.block_params(entry)[0];
        self.locals_base = self
            .b
            .ins()
            .load(self.ptr_ty, trusted, self.frame_ptr, OFF_LOCALS);
        self.spill_base = self
            .b
            .ins()
            .load(self.ptr_ty, trusted, self.frame_ptr, OFF_STACK_SPILL);
        self.tags_base = self
            .b
            .ins()
            .load(self.ptr_ty, trusted, self.frame_ptr, OFF_STACK_TAGS);
        // RFC 0071 WS4 — `BuildList` stages its elements through the
        // same marshal buffer as call arguments, so a call-free
        // function that builds lists still needs the base loaded.
        // RFC 0074 — the opaque-call and generic-store lanes stage
        // through it too, and the pair-loop terminator reads its
        // second element back from it.
        let stages_elements = self.tfunc.blocks.iter().any(|tb| {
            matches!(tb.term, TTerm::ForIterPair { .. })
                || tb.stmts.iter().any(|s| {
                    matches!(
                        s.op,
                        TOp::BuildList {
                            none_fill: false,
                            ..
                        } | TOp::BuildTuple { .. }
                            | TOp::BuildMap { .. }
                            | TOp::BuildSet { .. }
                            | TOp::BuildString { .. }
                            | TOp::CallDyn { .. }
                            | TOp::DynAttrSet { .. }
                            | TOp::ContainsDyn { .. }
                            | TOp::DynBinary { .. }
                            | TOp::DynCompare { .. }
                            | TOp::DynGetItem
                            | TOp::DynSetItem
                            | TOp::DynUnary { .. }
                    )
                })
        });
        if !self.tfunc.callee_spans.is_empty()
            || !self.tfunc.method_sites.is_empty()
            || !self.tfunc.str_method_sites.is_empty()
            || stages_elements
        {
            self.call_args_base =
                self.b
                    .ins()
                    .load(self.ptr_ty, trusted, self.frame_ptr, OFF_CALL_ARGS);
            self.call_tags_base =
                self.b
                    .ins()
                    .load(self.ptr_ty, trusted, self.frame_ptr, OFF_CALL_TAGS);
        }

        // One Cranelift block per TBlock, with one block parameter per
        // entry-stack lane (RFC 0069 WS2).
        self.cl_blocks = Vec::with_capacity(self.tfunc.blocks.len());
        for tb in &self.tfunc.blocks {
            let cl = self.b.create_block();
            for &ty in &tb.entry_stack {
                self.b.append_block_param(cl, Self::cl_ty(ty));
            }
            self.cl_blocks.push(cl);
        }

        // RFC 0067 WS2 — the poll countdown. Seeded once per
        // activation; every loop-header visit decrements it and calls
        // the embedder's poll helper on expiry (GIL hand-off inline,
        // deopt at the header only for interpreter-required work).
        // Skipped entirely when the embedder registered no helper
        // (this crate's standalone unit tests) or the function has no
        // loops to instrument.
        if runtime::poll_helper_addr() != 0 && !self.tfunc.osr_entries.is_empty() {
            let var = self.b.declare_var(types::I64);
            let seed = self.b.ins().iconst(types::I64, runtime::JIT_POLL_STRIDE);
            self.b.def_var(var, seed);
            self.poll_countdown = Some(var);
        }

        // Declare + initialise a variable per managed local.
        self.vars = vec![None; self.tfunc.n_locals as usize];
        self.assignable = assigned_slots(self.tfunc);
        for slot in 0..self.tfunc.local_types.len() {
            if let Some(ty) = self.tfunc.local_types[slot] {
                let cl = Self::cl_ty(ty);
                let var = self.b.declare_var(cl);
                let off = (slot as i32) * 8;
                let v = self.b.ins().load(cl, trusted, self.locals_base, off);
                self.b.def_var(var, v);
                self.vars[slot] = Some(var);
                if self.var_slots.len() <= var.index() {
                    self.var_slots.resize(var.index() + 1, u32::MAX);
                }
                self.var_slots[var.index()] = slot as u32;
            }
        }

        // Entry dispatch (RFC 0059 WS3b): `entry_pc == 0` enters at the
        // function start; a recognized loop-header pc OSR-enters its
        // block (all OSR blocks have empty boundary stacks). The VM
        // guarantees every managed local was packed before an OSR entry.
        // RFC 0071 WS5 — a generator *resume* pc enters its yield's
        // continuation block, whose single boundary value (the sent
        // value, object lane) arrives in `ret_bits`.
        let entry_target = self.cl_blocks[self.tfunc.entry_block];
        if self.tfunc.osr_entries.is_empty() && self.tfunc.resume_entries.is_empty() {
            self.b.ins().jump(entry_target, &[]);
        } else {
            let entry_pc = self
                .b
                .ins()
                .load(types::I32, trusted, self.frame_ptr, OFF_ENTRY_PC);
            for e in &self.tfunc.osr_entries {
                let hit = self
                    .b
                    .ins()
                    .icmp_imm(IntCC::Equal, entry_pc, i64::from(e.pc));
                let next = self.b.create_block();
                let target = self.cl_blocks[e.block];
                self.b.ins().brif(hit, target, &[], next, &[]);
                self.b.switch_to_block(next);
            }
            for e in &self.tfunc.resume_entries {
                let hit = self
                    .b
                    .ins()
                    .icmp_imm(IntCC::Equal, entry_pc, i64::from(e.pc));
                let next = self.b.create_block();
                let target = self.cl_blocks[e.block];
                let sent = self
                    .b
                    .ins()
                    .load(types::I64, trusted, self.frame_ptr, OFF_RET_BITS);
                self.b
                    .ins()
                    .brif(hit, target, &[BlockArg::from(sent)], next, &[]);
                self.b.switch_to_block(next);
            }
            self.b.ins().jump(entry_target, &[]);
        }

        // Emit each block body, seeding the abstract stack from the
        // block's parameters.
        for bi in 0..self.tfunc.blocks.len() {
            let cl = self.cl_blocks[bi];
            self.b.switch_to_block(cl);
            self.vstack.clear();
            let params: Vec<Value> = self.b.block_params(cl).to_vec();
            for (v, &ty) in params.iter().zip(&self.tfunc.blocks[bi].entry_stack) {
                self.vstack.push((*v, ty));
            }
            self.emit_block(bi);
        }
    }

    /// The current abstract stack as block-call arguments.
    fn block_args(&self) -> Vec<BlockArg> {
        self.vstack
            .iter()
            .map(|&(v, _)| BlockArg::from(v))
            .collect()
    }

    fn emit_block(&mut self, bi: usize) {
        // RFC 0067 WS2 — instrument loop headers with the eval-breaker
        // poll. Headers are exactly the OSR-enterable blocks (backward-
        // jump targets with an empty boundary stack), so a pending-work
        // deopt here resumes the interpreter in a state the existing
        // spill/rebuild machinery already describes.
        if let Some(cd_var) = self.poll_countdown {
            if let Some(header_pc) = self
                .tfunc
                .osr_entries
                .iter()
                .find(|e| e.block == bi)
                .map(|e| e.pc)
            {
                self.emit_poll(cd_var, header_pc);
            } else if let TTerm::ForList { pc, .. }
            | TTerm::ForIter { pc, .. }
            | TTerm::ForIterPair { pc, .. } = self.tfunc.blocks[bi].term
            {
                // RFC 0073 WS1 — a comprehension loop header is not
                // OSR-enterable (its boundary stack is non-empty), but
                // it is still a loop header: poll here too, spilling
                // the live entry stack so the pending-work deopt
                // resumes exactly at the `FOR_ITER`.
                self.emit_poll(cd_var, pc);
            }
        }
        let block = self.tfunc.blocks[bi].clone();
        let chain_helper = runtime::attr_get_chain_helper_addr();
        let cached_chain_helper = runtime::cached_attr_chain_helper_addr();
        let mut i = 0;
        while i < block.stmts.len() {
            let stmt = block.stmts[i];
            if let Some(window) = block.stmts.get(i..i + 4) {
                if self.repeated_scalar_read(window) {
                    self.emit_stmt(window[0]);
                    self.emit_stmt(window[1]);
                    // The first guarded read establishes a callback-free scalar
                    // value. The second local load cannot change the receiver.
                    self.vstack.push(*self.vstack.last().expect("scalar read"));
                    i += 4;
                    continue;
                }
            }
            if cached_chain_helper != 0
                && matches!(
                    stmt.op,
                    TOp::AttrGet {
                        out: JitType::Obj,
                        ..
                    } | TOp::DynAttrGet { .. }
                )
            {
                let mut end = i;
                let mut guarded = 0u32;
                let mut first_site = 0u32;
                let mut dynamic = false;
                while end < block.stmts.len() && end - i < runtime::MAX_CACHED_ATTR_CHAIN_LEN {
                    let next = block.stmts[end];
                    if stmt.pc.checked_add((end - i) as u32) != Some(next.pc)
                        || self
                            .tfunc
                            .null_spans
                            .iter()
                            .any(|span| span.live_from == next.pc)
                    {
                        break;
                    }
                    match next.op {
                        TOp::AttrGet {
                            site,
                            out: JitType::Obj,
                        } if !dynamic => {
                            if guarded == 0 {
                                first_site = site;
                            }
                            if first_site.checked_add(guarded) != Some(site)
                                || guarded as usize >= runtime::MAX_ATTR_CHAIN_LEN
                            {
                                break;
                            }
                            guarded += 1;
                        }
                        TOp::DynAttrGet { .. } => dynamic = true,
                        _ => break,
                    }
                    end += 1;
                }
                let total = end - i;
                if dynamic && total >= 2 {
                    // Keep existing analyzer/type rules. Fuse only its explicit guard of
                    // an adjacent result at depth zero, never infer a new integer lane.
                    let int_result = block.stmts.get(end).is_some_and(|next| {
                        matches!(next.op, TOp::UnboxInt { depth: 0 })
                            && stmt.pc.checked_add(total as u32) == Some(next.pc)
                    });
                    let resume = end + usize::from(int_result);
                    self.emit_cached_attr_chain(
                        cached_chain_helper,
                        first_site,
                        guarded,
                        total as u32,
                        int_result,
                        &block.stmts[i..resume],
                    );
                    i = resume;
                    continue;
                }
            }

            if chain_helper != 0 {
                if let TOp::AttrGet {
                    site,
                    out: JitType::Obj,
                } = stmt.op
                {
                    let mut end = i + 1;
                    let mut out = JitType::Obj;
                    while end < block.stmts.len()
                        && end - i < runtime::MAX_ATTR_CHAIN_LEN
                        && out == JitType::Obj
                    {
                        let next = block.stmts[end];
                        let TOp::AttrGet {
                            site: next_site,
                            out: next_out,
                        } = next.op
                        else {
                            break;
                        };
                        let distance = (end - i) as u32;
                        if site.checked_add(distance) != Some(next_site)
                            || stmt.pc.checked_add(distance) != Some(next.pc)
                        {
                            break;
                        }
                        out = next_out;
                        end += 1;
                    }
                    if end - i >= 2 {
                        self.emit_attr_get_chain(
                            chain_helper,
                            site,
                            (end - i) as u32,
                            out,
                            stmt.pc,
                        );
                        i = end;
                        continue;
                    }
                }
            }
            self.emit_stmt(stmt);
            i += 1;
        }
        match block.term {
            TTerm::Return => self.emit_return(),
            TTerm::ReturnNone => self.emit_return_none(),
            TTerm::Jump(t) => {
                let args = self.block_args();
                let target = self.cl_blocks[t];
                self.b.ins().jump(target, &args);
            }
            TTerm::BranchFalse {
                target,
                fallthrough,
            } => {
                let (cond, ty) = self.pop();
                let truthy = self.truth(cond, ty);
                let args = self.block_args();
                let tb = self.cl_blocks[target];
                let fb = self.cl_blocks[fallthrough];
                // if truthy → fallthrough else → target.
                self.b.ins().brif(truthy, fb, &args, tb, &args);
            }
            TTerm::BranchTrue {
                target,
                fallthrough,
            } => {
                let (cond, ty) = self.pop();
                let truthy = self.truth(cond, ty);
                let args = self.block_args();
                let tb = self.cl_blocks[target];
                let fb = self.cl_blocks[fallthrough];
                self.b.ins().brif(truthy, tb, &args, fb, &args);
            }
            TTerm::ForRange {
                cur_slot,
                stop_slot,
                var_slot,
                body,
                exit,
            } => {
                // if cur < stop { var = cur; cur += 1; goto body }
                // else { goto exit }. `cur < stop <= i64::MAX` makes the
                // unit-step increment overflow-free.
                let cur_var = self.vars[cur_slot as usize].expect("managed range cur");
                let stop_var = self.vars[stop_slot as usize].expect("managed range stop");
                let loop_var = self.vars[var_slot as usize].expect("managed loop var");
                let cur = self.b.use_var(cur_var);
                let stop = self.b.use_var(stop_var);
                let cond = self.b.ins().icmp(IntCC::SignedLessThan, cur, stop);
                let body_pre = self.b.create_block();
                let eb = self.cl_blocks[exit];
                self.b.ins().brif(cond, body_pre, &[], eb, &[]);
                self.b.switch_to_block(body_pre);
                self.def_local(loop_var, cur);
                let next = self.b.ins().iadd_imm(cur, 1);
                self.def_local(cur_var, next);
                let bb = self.cl_blocks[body];
                self.b.ins().jump(bb, &[]);
            }
            // RFC 0071 WS4 — the list-loop step: the registered
            // `wpjit_list_next` helper re-checks the index against the
            // live length and re-validates the element lane. `0` →
            // element in `ret_bits` (store into the loop variable,
            // bump the index, enter the body); `1` → exhausted (take
            // the exit edge); anything else → deopt at the header pc,
            // where the interpreter resumes on the rebuilt iterator.
            TTerm::ForList {
                seq_slot,
                idx_slot,
                var_slot,
                elem,
                pc,
                body,
                exit,
            } => {
                let trusted = MemFlags::trusted();
                let seq_var = self.vars[seq_slot as usize].expect("managed list seq");
                let idx_var = self.vars[idx_slot as usize].expect("managed list idx");
                let loop_var = self.vars[var_slot as usize].expect("managed loop var");
                let seq = self.b.use_var(seq_var);
                let idx = self.b.use_var(idx_var);
                // RFC 0073 WS1 — a comprehension loop carries its
                // accumulator (and any surrounding expression stack)
                // through the header: successors take it as block
                // args, and a header deopt spills it.
                let snapshot = self.vstack.clone();
                let args = self.block_args();
                // A scalar element of a scalar-lane list, or the end of
                // the list, in line.
                let got_b = self.b.create_block();
                self.b.append_block_param(got_b, Self::cl_ty(elem));
                let mut native_done = None;
                if let (Some(l), JitType::Int | JitType::Float | JitType::Bool) =
                    (runtime::obj_layout(), elem)
                {
                    let l = *l;
                    let miss = self.b.create_block();
                    let (items, len, _) = self.pinned_list(&l, seq, &[elem], miss, true);
                    let negative = self.b.ins().icmp_imm(IntCC::SignedLessThan, idx, 0);
                    self.miss_if(negative, miss);
                    let at_end = self.b.ins().icmp(IntCC::SignedGreaterThanOrEqual, idx, len);
                    let more = self.b.create_block();
                    let end = self.b.create_block();
                    self.b.ins().brif(at_end, end, &[], more, &[]);
                    self.b.switch_to_block(more);
                    let off = self.b.ins().ishl_imm(idx, 4);
                    let e = self.b.ins().iadd(items, off);
                    let (want, voff, ty) = Self::lane_tag(&l, elem).expect("scalar lane");
                    let tag = self.b.ins().uload8(types::I32, trusted, e, 0);
                    let other = self.b.ins().icmp_imm(IntCC::NotEqual, tag, i64::from(want));
                    self.miss_if(other, miss);
                    let v = self.b.ins().load(ty, trusted, e, voff);
                    let v = if ty == types::I8 {
                        self.b.ins().uextend(types::I64, v)
                    } else {
                        v
                    };
                    self.b.ins().jump(got_b, &[BlockArg::from(v)]);
                    self.b.switch_to_block(miss);
                    native_done = Some(end);
                }
                let sig = self.list_helper_sig();
                let helper = self
                    .b
                    .ins()
                    .iconst(self.ptr_ty, runtime::list_next_helper_addr() as i64);
                let call = self
                    .b
                    .ins()
                    .call_indirect(sig, helper, &[self.frame_ptr, seq, idx]);
                let status = self.b.inst_results(call)[0];
                let fetched = self.b.create_block();
                let rest_b = self.b.create_block();
                let is_got = self.b.ins().icmp_imm(IntCC::Equal, status, 0);
                self.b.ins().brif(is_got, fetched, &[], rest_b, &[]);
                self.b.switch_to_block(fetched);
                let res =
                    self.b
                        .ins()
                        .load(Self::cl_ty(elem), trusted, self.frame_ptr, OFF_RET_BITS);
                self.b.ins().jump(got_b, &[BlockArg::from(res)]);
                if let Some(end) = native_done {
                    self.b.switch_to_block(end);
                    let eb = self.cl_blocks[exit];
                    self.b.ins().jump(eb, &args);
                }
                self.b.switch_to_block(got_b);
                let res = self.b.block_params(got_b)[0];
                self.def_local(loop_var, res);
                let next = self.b.ins().iadd_imm(idx, 1);
                self.def_local(idx_var, next);
                let bb = self.cl_blocks[body];
                self.b.ins().jump(bb, &args);
                self.b.switch_to_block(rest_b);
                let deopt_b = self.b.create_block();
                let eb = self.cl_blocks[exit];
                let is_done = self.b.ins().icmp_imm(IntCC::Equal, status, 1);
                self.b.ins().brif(is_done, eb, &args, deopt_b, &[]);
                self.b.switch_to_block(deopt_b);
                self.emit_exit(pc, &snapshot, JitStatus::Deopt);
            }
            // RFC 0071 WS4 — the opaque-iterator step: the registered
            // `wpjit_iter_next` helper advances the pinned iterator
            // through the interpreter core (it *runs Python*). `0` →
            // element in `ret_bits` (store into the loop variable,
            // enter the body); `1` → exhausted (the helper reaped the
            // pin; take the exit edge); `2` → deopt at the header
            // (nothing consumed); `3` → the consumed element is
            // outside the compiled lane: deopt at the fused store's pc
            // with the raw element (pinned, `ret_bits`) spilled on
            // top, so the interpreter's `STORE_FAST` consumes it
            // exactly once; `4` → raise at the header pc.
            TTerm::ForIter {
                iter_slot,
                var_slot,
                elem,
                pc,
                store_pc,
                body,
                exit,
            } => {
                let trusted = MemFlags::trusted();
                let iter_var = self.vars[iter_slot as usize].expect("managed iter slot");
                let loop_var = self.vars[var_slot as usize].expect("managed loop var");
                let iter = self.b.use_var(iter_var);
                // RFC 0073 WS1 — see `ForList`: a comprehension loop's
                // boundary stack rides through the header.
                let snapshot = self.vstack.clone();
                let args = self.block_args();
                let sig = self.list_helper_sig();
                let helper = self
                    .b
                    .ins()
                    .iconst(self.ptr_ty, runtime::iter_next_helper_addr() as i64);
                // RFC 0073 WS2 — an exact-`str` element lane (dict-keys
                // loops) needs its own discriminant: `tag` collapses
                // `Str` into `ObjPin`, whose packing only admits
                // instances.
                let elem_disc = if elem == JitType::Str {
                    runtime::ITER_ELEM_STR
                } else {
                    Self::tag(elem)
                };
                let tag = self.b.ins().iconst(types::I64, elem_disc);
                let call = self
                    .b
                    .ins()
                    .call_indirect(sig, helper, &[self.frame_ptr, iter, tag]);
                let status = self.b.inst_results(call)[0];
                let got_b = self.b.create_block();
                let rest_b = self.b.create_block();
                let is_got = self.b.ins().icmp_imm(IntCC::Equal, status, 0);
                self.b.ins().brif(is_got, got_b, &[], rest_b, &[]);
                self.b.switch_to_block(got_b);
                let res =
                    self.b
                        .ins()
                        .load(Self::cl_ty(elem), trusted, self.frame_ptr, OFF_RET_BITS);
                self.def_local(loop_var, res);
                let bb = self.cl_blocks[body];
                self.b.ins().jump(bb, &args);
                self.b.switch_to_block(rest_b);
                let more_b = self.b.create_block();
                let eb = self.cl_blocks[exit];
                let is_done = self.b.ins().icmp_imm(IntCC::Equal, status, 1);
                self.b.ins().brif(is_done, eb, &args, more_b, &[]);
                self.b.switch_to_block(more_b);
                let lane_b = self.b.create_block();
                let hdr_b = self.b.create_block();
                let is_lane = self.b.ins().icmp_imm(IntCC::Equal, status, 3);
                self.b.ins().brif(is_lane, lane_b, &[], hdr_b, &[]);
                self.b.switch_to_block(lane_b);
                let elem_pin = self
                    .b
                    .ins()
                    .load(types::I64, trusted, self.frame_ptr, OFF_RET_BITS);
                let mut lane_spill = snapshot.clone();
                lane_spill.push((elem_pin, JitType::Obj));
                self.emit_exit(store_pc, &lane_spill, JitStatus::Deopt);
                self.b.switch_to_block(hdr_b);
                let raise_b = self.b.create_block();
                let deopt_b = self.b.create_block();
                let is_raise = self.b.ins().icmp_imm(IntCC::Equal, status, 4);
                self.b.ins().brif(is_raise, raise_b, &[], deopt_b, &[]);
                self.b.switch_to_block(raise_b);
                self.emit_exit(pc, &snapshot, JitStatus::Raised);
                self.b.switch_to_block(deopt_b);
                self.emit_exit(pc, &snapshot, JitStatus::Deopt);
            }
            // RFC 0074 WS3 — the tuple-target step: the registered
            // `wpjit_iter_next_pair` helper advances the pinned
            // iterator through the interpreter core (it *runs
            // Python*) and unpacks the yielded 2-sequence. `0` →
            // elements in `ret_bits` / `call_args[0]` (store into
            // both loop variables, enter the body); `1` → exhausted;
            // `2` → deopt at the header (nothing consumed); `3` → the
            // consumed element is not a 2-sequence in the compiled
            // lanes: deopt at the erased `UNPACK_SEQUENCE`'s pc with
            // the raw element (pinned, `ret_bits`) spilled on top;
            // `4` → raise at the header pc.
            TTerm::ForIterPair {
                iter_slot,
                var1_slot,
                var2_slot,
                elem1,
                elem2,
                pc,
                store_pc,
                body,
                exit,
            } => {
                let trusted = MemFlags::trusted();
                let iter_var = self.vars[iter_slot as usize].expect("managed iter slot");
                let var1 = self.vars[var1_slot as usize].expect("managed loop var");
                let var2 = self.vars[var2_slot as usize].expect("managed loop var");
                let iter = self.b.use_var(iter_var);
                let snapshot = self.vstack.clone();
                let args = self.block_args();
                let sig = self.quad_helper_sig();
                let helper = self
                    .b
                    .ins()
                    .iconst(self.ptr_ty, runtime::iter_next_pair_helper_addr() as i64);
                let disc = |e: JitType| {
                    if e == JitType::Str {
                        runtime::ITER_ELEM_STR
                    } else {
                        Self::tag(e)
                    }
                };
                let tag1 = self.b.ins().iconst(types::I64, disc(elem1));
                let tag2 = self.b.ins().iconst(types::I64, disc(elem2));
                let call =
                    self.b
                        .ins()
                        .call_indirect(sig, helper, &[self.frame_ptr, iter, tag1, tag2]);
                let status = self.b.inst_results(call)[0];
                let got_b = self.b.create_block();
                let rest_b = self.b.create_block();
                let is_got = self.b.ins().icmp_imm(IntCC::Equal, status, 0);
                self.b.ins().brif(is_got, got_b, &[], rest_b, &[]);
                self.b.switch_to_block(got_b);
                let e1 =
                    self.b
                        .ins()
                        .load(Self::cl_ty(elem1), trusted, self.frame_ptr, OFF_RET_BITS);
                self.def_local(var1, e1);
                let e2 = self
                    .b
                    .ins()
                    .load(Self::cl_ty(elem2), trusted, self.call_args_base, 0);
                self.def_local(var2, e2);
                let bb = self.cl_blocks[body];
                self.b.ins().jump(bb, &args);
                self.b.switch_to_block(rest_b);
                let more_b = self.b.create_block();
                let eb = self.cl_blocks[exit];
                let is_done = self.b.ins().icmp_imm(IntCC::Equal, status, 1);
                self.b.ins().brif(is_done, eb, &args, more_b, &[]);
                self.b.switch_to_block(more_b);
                let lane_b = self.b.create_block();
                let hdr_b = self.b.create_block();
                let is_lane = self.b.ins().icmp_imm(IntCC::Equal, status, 3);
                self.b.ins().brif(is_lane, lane_b, &[], hdr_b, &[]);
                self.b.switch_to_block(lane_b);
                let elem_pin = self
                    .b
                    .ins()
                    .load(types::I64, trusted, self.frame_ptr, OFF_RET_BITS);
                let mut lane_spill = snapshot.clone();
                lane_spill.push((elem_pin, JitType::Obj));
                self.emit_exit(store_pc, &lane_spill, JitStatus::Deopt);
                self.b.switch_to_block(hdr_b);
                let raise_b = self.b.create_block();
                let deopt_b = self.b.create_block();
                let is_raise = self.b.ins().icmp_imm(IntCC::Equal, status, 4);
                self.b.ins().brif(is_raise, raise_b, &[], deopt_b, &[]);
                self.b.switch_to_block(raise_b);
                self.emit_exit(pc, &snapshot, JitStatus::Raised);
                self.b.switch_to_block(deopt_b);
                self.emit_exit(pc, &snapshot, JitStatus::Deopt);
            }
            // RFC 0070 WS2 — a yield is an unconditional deopt-shaped
            // exit parked *at* the `YIELD_VALUE` pc: the full stack
            // (yielded value on top) spills, and the embedder resumes
            // interpretation on the yield instruction itself, whose
            // ordinary execution performs the suspension.
            TTerm::Yield { pc } => {
                let snapshot = self.vstack.clone();
                self.emit_exit(pc, &snapshot, JitStatus::Yielded);
            }
            TTerm::Deopt { pc } => {
                let snapshot = self.vstack.clone();
                self.emit_exit(pc, &snapshot, JitStatus::Deopt);
            }
        }
    }

    fn emit_return(&mut self) {
        let trusted = MemFlags::trusted();
        let (val, ty) = self.pop();
        self.b
            .ins()
            .store(trusted, val, self.frame_ptr, OFF_RET_BITS);
        let tag = self.b.ins().iconst(types::I32, Self::tag(ty));
        self.b
            .ins()
            .store(trusted, tag, self.frame_ptr, OFF_RET_TAG);
        let status = self.b.ins().iconst(types::I64, JitStatus::Returned as i64);
        self.b.ins().return_(&[status]);
    }

    /// RFC 0069 WS1 — `return None`: nothing native to pop; the
    /// [`SlotTag::None`] tag alone tells the embedder to rebuild the
    /// `None` singleton.
    fn emit_return_none(&mut self) {
        let trusted = MemFlags::trusted();
        let zero = self.b.ins().iconst(types::I64, 0);
        self.b
            .ins()
            .store(trusted, zero, self.frame_ptr, OFF_RET_BITS);
        let tag = self.b.ins().iconst(types::I32, SlotTag::None as i64);
        self.b
            .ins()
            .store(trusted, tag, self.frame_ptr, OFF_RET_TAG);
        let status = self.b.ins().iconst(types::I64, JitStatus::Returned as i64);
        self.b.ins().return_(&[status]);
    }

    /// Reuse only an adjacent, identical numeric field read. No calls, stores,
    /// polling points, block boundaries, or erased NULL markers may intervene.
    fn repeated_scalar_read(&self, stmts: &[TStmt]) -> bool {
        let [TStmt {
            op: TOp::LoadLocal(first),
            ..
        }, TStmt {
            op: TOp::AttrGet { site: a, out: lane },
            ..
        }, TStmt {
            op: TOp::LoadLocal(second),
            ..
        }, TStmt {
            op: TOp::AttrGet {
                site: b,
                out: other,
            },
            ..
        }] = stmts
        else {
            return false;
        };
        if first != second
            || lane != other
            || !matches!(lane, JitType::Int | JitType::Float | JitType::Bool)
        {
            return false;
        }
        let (Some(a), Some(b)) = (
            self.tfunc.attr_sites.get(*a as usize),
            self.tfunc.attr_sites.get(*b as usize),
        ) else {
            return false;
        };
        a == b
            && a.slot == *first
            && a.path.is_empty()
            && a.lane == *lane
            && !a.store
            && !a.new_key
            && stmts.iter().enumerate().all(|(offset, stmt)| {
                stmts[0].pc.checked_add(offset as u32) == Some(stmt.pc)
                    && !self
                        .tfunc
                        .null_spans
                        .iter()
                        .any(|span| span.live_from == stmt.pc || span.live_to == stmt.pc)
            })
    }

    fn emit_stmt(&mut self, stmt: TStmt) {
        match stmt.op {
            TOp::PushConstInt(v) => {
                let val = self.b.ins().iconst(types::I64, v);
                self.vstack.push((val, JitType::Int));
            }
            TOp::PushConstBool(v) => {
                let val = self.b.ins().iconst(types::I64, i64::from(v));
                self.vstack.push((val, JitType::Bool));
            }
            TOp::PushConstFloat(bits) => {
                let val = self.b.ins().f64const(f64::from_bits(bits));
                self.vstack.push((val, JitType::Float));
            }
            TOp::LoadLocal(slot) => {
                let ty = self.tfunc.local_types[slot as usize].unwrap_or(JitType::Int);
                let var = self.vars[slot as usize].expect("managed local");
                let v = self.b.use_var(var);
                self.vstack.push((v, ty));
            }
            TOp::StoreLocal(slot) => {
                let (v, _) = self.pop();
                let var = self.vars[slot as usize].expect("managed local");
                self.def_local(var, v);
            }
            TOp::IntArith(kind) => self.emit_int_arith(kind, stmt.pc),
            TOp::FloatArith(kind) => self.emit_float_arith(kind, stmt.pc),
            TOp::IntTrueDiv => self.emit_int_truediv(stmt.pc),
            TOp::IntCmp(kind) => self.emit_int_cmp(kind),
            TOp::FloatCmp(kind) => self.emit_float_cmp(kind),
            TOp::IntNeg => self.emit_int_neg(stmt.pc),
            TOp::FloatNeg => {
                let (a, _) = self.pop();
                let r = self.b.ins().fneg(a);
                self.vstack.push((r, JitType::Float));
            }
            TOp::IntInvert => {
                let (a, _) = self.pop();
                let r = self.b.ins().bnot(a);
                self.vstack.push((r, JitType::Int));
            }
            TOp::IntNot => {
                let (a, _) = self.pop();
                let z = self.b.ins().iconst(types::I64, 0);
                let cmp = self.b.ins().icmp(IntCC::Equal, a, z);
                let r = self.b.ins().uextend(types::I64, cmp);
                self.vstack.push((r, JitType::Bool));
            }
            TOp::FloatNot => {
                let (a, _) = self.pop();
                let z = self.b.ins().f64const(0.0);
                let cmp = self.b.ins().fcmp(FloatCC::Equal, a, z);
                let r = self.b.ins().uextend(types::I64, cmp);
                self.vstack.push((r, JitType::Bool));
            }
            TOp::Pop => {
                self.pop();
            }
            TOp::Dup { depth } => {
                let len = self.vstack.len();
                let entry = self.vstack[len - depth as usize];
                self.vstack.push(entry);
            }
            TOp::Swap2 => {
                let len = self.vstack.len();
                self.vstack.swap(len - 1, len - 2);
            }
            TOp::SwapN { depth } => {
                let len = self.vstack.len();
                self.vstack.swap(len - 1, len - depth as usize);
            }
            TOp::IntToFloatTos { guarded } => {
                let depth = self.vstack.len() - 1;
                self.emit_int_to_float(depth, guarded, stmt.pc);
            }
            TOp::IntToFloatSecond { guarded } => {
                let depth = self.vstack.len() - 2;
                self.emit_int_to_float(depth, guarded, stmt.pc);
            }
            TOp::CallPy {
                token,
                argc,
                ret,
                is_self,
            } => {
                if is_self && self.self_func.is_some() {
                    self.emit_call_self(token, argc, ret, stmt.pc);
                } else if let Some((ix, from)) = self.leaf_for(token, argc, 0, 0) {
                    let call = SiteCall {
                        token,
                        argc,
                        kwc: 0,
                        perm: 0,
                        gaps: 0,
                    };
                    self.emit_call_leaf(ix, &from, call, ret, stmt.pc);
                } else {
                    self.emit_call_py(token, argc, 0, 0, 0, ret, stmt.pc);
                }
            }
            TOp::CallPyKw {
                token,
                argc,
                kwc,
                perm,
                gaps,
                ret,
            } => {
                if let Some((ix, from)) = self.leaf_for(token, argc, kwc, perm) {
                    let call = SiteCall {
                        token,
                        argc,
                        kwc,
                        perm,
                        gaps,
                    };
                    self.emit_call_leaf(ix, &from, call, ret, stmt.pc);
                } else {
                    self.emit_call_py(token, argc, kwc, perm, gaps, ret, stmt.pc);
                }
            }
            TOp::ListGet { elem } => self.emit_list_get(elem, stmt.pc),
            TOp::ListSet => self.emit_list_set(stmt.pc),
            TOp::CellGet { idx, lane } => self.emit_cell_get(idx, lane, stmt.pc),
            TOp::CellSet { idx, lane } => self.emit_cell_set(idx, lane, stmt.pc),
            TOp::ListLen => self.emit_list_len(stmt.pc),
            TOp::ListAppend => self.emit_list_append(stmt.pc),
            TOp::ListAppendKeep => self.emit_list_append_keep(stmt.pc),
            TOp::AttrGet { site, out } => self.emit_attr_get(site, out, stmt.pc),
            TOp::AttrSet { site } => self.emit_attr_set(site, stmt.pc),
            // RFC 0070 WS1 — nullable object lanes: `None` is the
            // machine value `-1` (never a valid pin-table index).
            TOp::IsNone { negate } => {
                let (v, _) = self.pop();
                let cc = if negate {
                    IntCC::NotEqual
                } else {
                    IntCC::Equal
                };
                let cmp = self.b.ins().icmp_imm(cc, v, -1);
                let r = self.b.ins().uextend(types::I64, cmp);
                self.vstack.push((r, JitType::Bool));
            }
            TOp::PushNone => {
                let v = self.b.ins().iconst(types::I64, -1);
                self.vstack.push((v, JitType::Obj));
            }
            TOp::GuardNotNone => {
                let snapshot = self.vstack.clone();
                let &(v, _) = self.vstack.last().expect("guard on empty stack");
                let is_none = self.b.ins().icmp_imm(IntCC::Equal, v, -1);
                let cont = self.guard(is_none, stmt.pc, &snapshot);
                self.b.switch_to_block(cont);
            }
            TOp::MathIntrinsic(func) => self.emit_math_intrinsic(func, stmt.pc),
            TOp::CallMethod { token, argc, ret } => {
                if let Some(body) = self.inline_body_for(token, argc, ret, stmt.pc) {
                    self.emit_call_method_inline(&body, token, argc, ret, stmt.pc);
                } else if let Some(ix) = self.method_leaf_for(token, argc, ret) {
                    self.emit_call_method_leaf(ix, token, argc, ret, stmt.pc);
                } else {
                    self.emit_call_method(token, argc, ret, stmt.pc);
                }
            }
            TOp::GuardMethod { token } => {
                let snapshot = self.vstack.clone();
                let &(pin, _) = self.vstack.last().expect("guard on empty stack");
                let sig = self.list_helper_sig();
                let helper = self
                    .b
                    .ins()
                    .iconst(self.ptr_ty, runtime::guard_method_helper_addr() as i64);
                let tokenv = self.b.ins().iconst(types::I64, i64::from(token));
                let call = self
                    .b
                    .ins()
                    .call_indirect(sig, helper, &[self.frame_ptr, pin, tokenv]);
                let status = self.b.inst_results(call)[0];
                let bad = self.b.ins().icmp_imm(IntCC::NotEqual, status, 0);
                let cont = self.guard(bad, stmt.pc, &snapshot);
                self.b.switch_to_block(cont);
            }
            TOp::ObjGetItem { token, int_result } => {
                let ret = MethodRet::Scalar(if int_result {
                    JitType::Int
                } else {
                    JitType::Obj
                });
                self.emit_method_helper_call(
                    runtime::obj_getitem_helper_addr(),
                    token,
                    1,
                    ret,
                    stmt.pc,
                    false,
                );
            }
            TOp::CallStrMethod { site, argc, ret } => {
                self.emit_call_str_method(site, argc, ret, stmt.pc);
            }
            TOp::StrEq { negate } => self.emit_str_eq(negate, stmt.pc),
            TOp::IsObj { negate } => self.emit_is_obj(negate, stmt.pc),
            TOp::StrLen => self.emit_pin_len(runtime::str_len_helper_addr(), stmt.pc),
            TOp::BytesLen => self.emit_pin_len(runtime::bytes_len_helper_addr(), stmt.pc),
            TOp::BytesGetItem => self.emit_bytes_get(stmt.pc),
            TOp::DictGet { key, val } => self.emit_dict_get(key, val, stmt.pc),
            TOp::DictSet { key, val } => self.emit_dict_set(key, val, stmt.pc),
            TOp::DictDel { key } => self.emit_dict_del(key, stmt.pc),
            TOp::DictContains { negate, key } => self.emit_dict_contains(negate, key, stmt.pc),
            TOp::DictLen => self.emit_pin_len(runtime::dict_len_helper_addr(), stmt.pc),
            TOp::TupleLen => self.emit_pin_len(runtime::tuple_len_helper_addr(), stmt.pc),
            TOp::UnboxInt { depth } => self.emit_unbox_int(depth, stmt.pc),
            TOp::UnboxFloat { depth, promote } => self.emit_unbox_float(depth, promote, stmt.pc),
            TOp::IterCapture {
                iter_slot,
                materialize,
            } => {
                if materialize {
                    self.emit_iter_new(iter_slot, stmt.pc);
                } else {
                    self.emit_iter_capture(iter_slot, stmt.pc);
                }
            }
            TOp::DictIterNew { iter_slot } => self.emit_dict_iter_new(iter_slot, stmt.pc),
            TOp::BuildList {
                n,
                elem,
                none_fill,
                mixed,
                konst,
            } => {
                self.emit_build_list(n, elem, none_fill, mixed, konst, stmt.pc);
            }
            TOp::BuildTuple { n } => self.emit_build_tuple(n, stmt.pc),
            TOp::BuildMap { n } => self.emit_build_map(n, stmt.pc),
            TOp::PushConstStr { idx } => {
                self.emit_const_pin(idx, stmt.pc, runtime::const_str_helper_addr(), JitType::Str)
            }
            TOp::PushConstTuple { idx } => self.emit_const_pin(
                idx,
                stmt.pc,
                runtime::const_tuple_helper_addr(),
                JitType::Obj,
            ),
            TOp::StrConcat => self.emit_str_concat(stmt.pc),
            TOp::StrGetItem => self.emit_str_get(stmt.pc),
            TOp::BuildString { n } => self.emit_build_string(n, stmt.pc),
            TOp::ListRepeat => self.emit_list_repeat(stmt.pc),
            TOp::ListFromRange { pops, deopt_pc } => self.emit_list_from_range(pops, deopt_pc),
            TOp::ListSlice {
                start,
                stop,
                origin,
            } => self.emit_list_slice(start, stop, origin, stmt.pc),
            TOp::PushGlobalObj { token, lane } => self.emit_push_global_obj(token, lane, stmt.pc),
            TOp::CallDyn {
                argc,
                kwc,
                names,
                int_result,
            } => self.emit_call_dyn(argc, kwc, names, int_result, stmt.pc),
            TOp::DynAttrGet { name } => self.emit_dyn_attr_get(name, stmt.pc),
            TOp::DynAttrSet { name } => self.emit_dyn_attr_set(name, stmt.pc),
            TOp::Truth => self.emit_truth(stmt.pc),
            TOp::ContainsDyn { negate } => self.emit_contains_dyn(negate, stmt.pc),
            TOp::DynBinary { arg } => {
                self.emit_dyn_op(runtime::dyn_binop_helper_addr(), arg, JitType::Obj, stmt.pc);
            }
            TOp::DynGetItem => {
                self.emit_dyn_op(runtime::dyn_getitem_helper_addr(), 0, JitType::Obj, stmt.pc);
            }
            TOp::DynSetItem => self.emit_dyn_setitem(stmt.pc),
            TOp::DynUnary { arg } => self.emit_dyn_unary(arg, stmt.pc),
            TOp::DynCompare { arg } => {
                let lane = if arg & weavepy_compiler::COMPARE_OP_TO_BOOL_FLAG != 0 {
                    JitType::Bool
                } else {
                    JitType::Obj
                };
                self.emit_dyn_op(runtime::dyn_compare_helper_addr(), arg, lane, stmt.pc);
            }
            TOp::BuildSet { n } => self.emit_build_set(n, stmt.pc),
            TOp::StrMod => self.emit_str_mod(stmt.pc),
            TOp::StrSlice {
                start,
                stop,
                origin,
            } => self.emit_str_slice(start, stop, origin, stmt.pc),
        }
    }

    /// RFC 0074 WS1 — an identity-guarded obj-global pin via the
    /// memoizing `wpjit_global_obj` helper. `-1` is a *value* — the
    /// object lane's nullable `None` encoding, answered when the
    /// snapshot itself is `None` (so a downstream `IsNone` fence sees
    /// it; RFC 0076 WS5). `-2` deopts (cap pressure; a lane surprise
    /// is impossible while the identity guard holds) and the
    /// interpreter re-executes the `LOAD_GLOBAL`.
    fn emit_push_global_obj(&mut self, token: u32, lane: JitType, pc: u32) {
        let snapshot = self.vstack.clone();
        let sig = self.pin_helper_sig();
        let helper = self
            .b
            .ins()
            .iconst(self.ptr_ty, runtime::global_obj_helper_addr() as i64);
        let tokenv = self
            .b
            .ins()
            .iconst(types::I64, runtime::global_obj_list_code(token, lane));
        let call = self
            .b
            .ins()
            .call_indirect(sig, helper, &[self.frame_ptr, tokenv]);
        let res = self.b.inst_results(call)[0];
        let bad = self.b.ins().icmp_imm(IntCC::SignedLessThan, res, -1);
        let cont = self.guard(bad, pc, &snapshot);
        self.b.switch_to_block(cont);
        self.vstack.push((res, lane));
    }

    /// RFC 0074 WS3 — the *generic* iterator capture (the
    /// materializing arm of `IterCapture`): `wpjit_iter_new` builds
    /// `iter(x)` through the interpreter core and answers the fresh
    /// iterator's pin, which lands in the iter slot. A negative
    /// status deopts with the iterable spilled and the interpreter
    /// executes the `GET_ITER` — and the loop — generically.
    fn emit_iter_new(&mut self, iter_slot: u32, pc: u32) {
        let snapshot = self.vstack.clone();
        let (pin, _) = self.pop();
        let sig = self.pin_helper_sig();
        let helper = self
            .b
            .ins()
            .iconst(self.ptr_ty, runtime::iter_new_helper_addr() as i64);
        let call = self
            .b
            .ins()
            .call_indirect(sig, helper, &[self.frame_ptr, pin]);
        let res = self.b.inst_results(call)[0];
        let bad = self.b.ins().icmp_imm(IntCC::SignedLessThan, res, 0);
        let cont = self.guard(bad, pc, &snapshot);
        self.b.switch_to_block(cont);
        let iter_var = self.vars[iter_slot as usize].expect("managed iter slot");
        self.def_local(iter_var, res);
    }

    /// RFC 0074 WS2 — the opaque-call lane via `wpjit_call_dyn`:
    /// marshal the `argc + kwc` tag-typed values, pop the callee pin,
    /// and dispatch on the returned [`crate::runtime::CallStatus`] —
    /// `Ok` pushes the pinned object-lane result and native execution
    /// continues; `Raised` exits at the call pc; `Boxed` deopts at
    /// `pc + 1` with the parked result (never re-executed); `Reject`
    /// (defensive pin miss) deopts at the call pc with the callee and
    /// arguments spilled, so the interpreter re-executes the call.
    fn emit_call_dyn(&mut self, argc: u8, kwc: u8, names: u32, int_result: bool, pc: u32) {
        let trusted = MemFlags::trusted();
        let n = argc as usize + kwc as usize;
        // Snapshot *including* callee + args for the Reject exit.
        let snapshot_full = self.vstack.clone();
        let base = self.vstack.len() - n;
        for (j, &(v, ty)) in self.vstack[base..].iter().enumerate() {
            let voff = (j as i32) * 8;
            let toff = (j as i32) * 4;
            self.b.ins().store(trusted, v, self.call_args_base, voff);
            let tagv = self.b.ins().iconst(types::I32, Self::tag(ty));
            self.b.ins().store(trusted, tagv, self.call_tags_base, toff);
        }
        self.vstack.truncate(base);
        let (callee, _) = self.pop();
        let snapshot = self.vstack.clone();

        self.writeback_locals();
        self.store_call_site_pc(pc);

        let sig = self.call_dyn_helper_sig();
        let address = if int_result {
            runtime::call_dyn_int_helper_addr()
        } else {
            runtime::call_dyn_helper_addr()
        };
        let helper = self.b.ins().iconst(self.ptr_ty, address as i64);
        let argcv = self.b.ins().iconst(types::I32, i64::from(argc));
        let kwcv = self.b.ins().iconst(types::I32, i64::from(kwc));
        let namesv = self.b.ins().iconst(types::I32, i64::from(names));
        let call =
            self.b
                .ins()
                .call_indirect(sig, helper, &[self.frame_ptr, callee, argcv, kwcv, namesv]);
        let status = self.b.inst_results(call)[0];

        let ok_b = self.b.create_block();
        let bad_b = self.b.create_block();
        let is_ok = self.b.ins().icmp_imm(IntCC::Equal, status, 0);
        self.b.ins().brif(is_ok, ok_b, &[], bad_b, &[]);

        self.b.switch_to_block(bad_b);
        let raised_b = self.b.create_block();
        let not_raised_b = self.b.create_block();
        let is_raised = self.b.ins().icmp_imm(IntCC::Equal, status, 1);
        self.b
            .ins()
            .brif(is_raised, raised_b, &[], not_raised_b, &[]);
        self.b.switch_to_block(raised_b);
        self.emit_exit(pc, &snapshot, JitStatus::Raised);
        self.b.switch_to_block(not_raised_b);
        let boxed_b = self.b.create_block();
        let reject_b = self.b.create_block();
        let is_boxed = self.b.ins().icmp_imm(IntCC::Equal, status, 2);
        self.b.ins().brif(is_boxed, boxed_b, &[], reject_b, &[]);
        self.b.switch_to_block(boxed_b);
        self.emit_exit(pc + 1, &snapshot, JitStatus::Deopt);
        self.b.switch_to_block(reject_b);
        self.emit_exit(pc, &snapshot_full, JitStatus::Deopt);

        self.b.switch_to_block(ok_b);
        let res = self
            .b
            .ins()
            .load(types::I64, trusted, self.frame_ptr, OFF_RET_BITS);
        self.vstack.push((
            res,
            if int_result {
                JitType::Int
            } else {
                JitType::Obj
            },
        ));
    }

    /// The imported signature of the `wpjit_call_dyn` helper (RFC
    /// 0074 WS2, lazy).
    fn call_dyn_helper_sig(&mut self) -> SigRef {
        if let Some(sig) = self.call_dyn_sig {
            return sig;
        }
        let mut sig = Signature::new(self.b.func.signature.call_conv);
        sig.params.push(AbiParam::new(self.ptr_ty)); // frame
        sig.params.push(AbiParam::new(types::I64)); // callee pin
        sig.params.push(AbiParam::new(types::I32)); // argc
        sig.params.push(AbiParam::new(types::I32)); // kwc
        sig.params.push(AbiParam::new(types::I32)); // names const idx
        sig.returns.push(AbiParam::new(types::I64)); // CallStatus
        let r = self.b.import_signature(sig);
        self.call_dyn_sig = Some(r);
        r
    }

    /// RFC 0076 WS8 — truthiness on a pinned/object-lane value via
    /// `wpjit_truth`: pop the pin, compute the interpreter's exact
    /// `bool(x)` (pure for `None`/scalars/container emptiness; the
    /// `__bool__`/`__len__` protocol for instances — arbitrary
    /// Python), push the `Bool`. Status protocol as
    /// [`Self::emit_dyn_attr_get`].
    fn emit_truth(&mut self, pc: u32) {
        let trusted = MemFlags::trusted();
        let snapshot_full = self.vstack.clone();
        let (pin, _) = self.pop();
        let snapshot = self.vstack.clone();

        self.writeback_locals();

        let sig = self.list_helper_sig();
        let helper = self
            .b
            .ins()
            .iconst(self.ptr_ty, runtime::truth_helper_addr() as i64);
        let zero = self.b.ins().iconst(types::I64, 0);
        let call = self
            .b
            .ins()
            .call_indirect(sig, helper, &[self.frame_ptr, pin, zero]);
        let status = self.b.inst_results(call)[0];

        let ok_b = self.b.create_block();
        let bad_b = self.b.create_block();
        let is_ok = self.b.ins().icmp_imm(IntCC::Equal, status, 0);
        self.b.ins().brif(is_ok, ok_b, &[], bad_b, &[]);

        self.b.switch_to_block(bad_b);
        let raised_b = self.b.create_block();
        let not_raised_b = self.b.create_block();
        let is_raised = self.b.ins().icmp_imm(IntCC::Equal, status, 1);
        self.b
            .ins()
            .brif(is_raised, raised_b, &[], not_raised_b, &[]);
        self.b.switch_to_block(raised_b);
        self.emit_exit(pc, &snapshot, JitStatus::Raised);
        self.b.switch_to_block(not_raised_b);
        let boxed_b = self.b.create_block();
        let reject_b = self.b.create_block();
        let is_boxed = self.b.ins().icmp_imm(IntCC::Equal, status, 2);
        self.b.ins().brif(is_boxed, boxed_b, &[], reject_b, &[]);
        self.b.switch_to_block(boxed_b);
        self.emit_exit(pc + 1, &snapshot, JitStatus::Deopt);
        self.b.switch_to_block(reject_b);
        self.emit_exit(pc, &snapshot_full, JitStatus::Deopt);

        self.b.switch_to_block(ok_b);
        let res = self
            .b
            .ins()
            .load(types::I64, trusted, self.frame_ptr, OFF_RET_BITS);
        self.vstack.push((res, JitType::Bool));
    }

    /// RFC 0076 WS8 — generic membership via `wpjit_contains_dyn`:
    /// the item (below the container) stages tag-typed in
    /// `call_args[0]` / `call_tags[0]`, the container pin pops
    /// natively, and the helper runs the interpreter's exact `in`
    /// protocol (arbitrary Python), pushing the already-negated
    /// `Bool`. Status protocol as [`Self::emit_truth`].
    fn emit_contains_dyn(&mut self, negate: bool, pc: u32) {
        let trusted = MemFlags::trusted();
        let snapshot_full = self.vstack.clone();
        let (pin, _) = self.pop();
        let (val, vty) = self.pop();
        self.b.ins().store(trusted, val, self.call_args_base, 0);
        let tagv = self.b.ins().iconst(types::I32, Self::tag(vty));
        self.b.ins().store(trusted, tagv, self.call_tags_base, 0);
        let snapshot = self.vstack.clone();

        self.writeback_locals();

        let sig = self.list_helper_sig();
        let helper = self
            .b
            .ins()
            .iconst(self.ptr_ty, runtime::contains_dyn_helper_addr() as i64);
        let negv = self.b.ins().iconst(types::I64, i64::from(negate));
        let call = self
            .b
            .ins()
            .call_indirect(sig, helper, &[self.frame_ptr, pin, negv]);
        let status = self.b.inst_results(call)[0];

        let ok_b = self.b.create_block();
        let bad_b = self.b.create_block();
        let is_ok = self.b.ins().icmp_imm(IntCC::Equal, status, 0);
        self.b.ins().brif(is_ok, ok_b, &[], bad_b, &[]);

        self.b.switch_to_block(bad_b);
        let raised_b = self.b.create_block();
        let not_raised_b = self.b.create_block();
        let is_raised = self.b.ins().icmp_imm(IntCC::Equal, status, 1);
        self.b
            .ins()
            .brif(is_raised, raised_b, &[], not_raised_b, &[]);
        self.b.switch_to_block(raised_b);
        self.emit_exit(pc, &snapshot, JitStatus::Raised);
        self.b.switch_to_block(not_raised_b);
        let boxed_b = self.b.create_block();
        let reject_b = self.b.create_block();
        let is_boxed = self.b.ins().icmp_imm(IntCC::Equal, status, 2);
        self.b.ins().brif(is_boxed, boxed_b, &[], reject_b, &[]);
        self.b.switch_to_block(boxed_b);
        self.emit_exit(pc + 1, &snapshot, JitStatus::Deopt);
        self.b.switch_to_block(reject_b);
        self.emit_exit(pc, &snapshot_full, JitStatus::Deopt);

        self.b.switch_to_block(ok_b);
        let res = self
            .b
            .ins()
            .load(types::I64, trusted, self.frame_ptr, OFF_RET_BITS);
        self.vstack.push((res, JitType::Bool));
    }

    /// [`TOp::DynSetItem`]: stage value, container, and index (the
    /// interpreter's stack order) and run the interpreter's item store.
    /// Exits as [`Self::emit_dyn_op`] does; nothing is pushed.
    fn emit_dyn_setitem(&mut self, pc: u32) {
        let trusted = MemFlags::trusted();
        let snapshot_full = self.vstack.clone();
        let (idx, ity) = self.pop();
        let (cont, cty) = self.pop();
        let (val, vty) = self.pop();
        for (k, (v, ty)) in [(val, vty), (cont, cty), (idx, ity)]
            .into_iter()
            .enumerate()
        {
            self.b
                .ins()
                .store(trusted, v, self.call_args_base, (k as i32) * 8);
            let tagv = self.b.ins().iconst(types::I32, Self::tag(ty));
            self.b
                .ins()
                .store(trusted, tagv, self.call_tags_base, (k as i32) * 4);
        }
        self.emit_dyn_call(runtime::dyn_setitem_helper_addr(), 0, pc, &snapshot_full);
    }

    /// [`TOp::DynUnary`]: stage the operand and run the interpreter's
    /// unary operation, pushing its result pin.
    fn emit_dyn_unary(&mut self, arg: u32, pc: u32) {
        let trusted = MemFlags::trusted();
        let snapshot_full = self.vstack.clone();
        let (v, ty) = self.pop();
        self.b.ins().store(trusted, v, self.call_args_base, 0);
        let tagv = self.b.ins().iconst(types::I32, Self::tag(ty));
        self.b.ins().store(trusted, tagv, self.call_tags_base, 0);
        self.emit_dyn_call(runtime::dyn_unary_helper_addr(), arg, pc, &snapshot_full);
        let res = self
            .b
            .ins()
            .load(types::I64, trusted, self.frame_ptr, OFF_RET_BITS);
        self.vstack.push((res, JitType::Obj));
    }

    /// The call and exits shared by the generic operation helpers: with
    /// the operands already staged and popped, call `helper_addr` with
    /// `arg` and leave on a raise (`1`, at `pc`), a parked result (`2`,
    /// after `pc`), or a decline (`3`, at `pc` with `snapshot_full`);
    /// continue in a fresh block on success.
    fn emit_dyn_call(
        &mut self,
        helper_addr: usize,
        arg: u32,
        pc: u32,
        snapshot_full: &[(Value, JitType)],
    ) {
        let snapshot = self.vstack.clone();
        self.writeback_locals();
        let sig = self.list_helper_sig();
        let helper = self.b.ins().iconst(self.ptr_ty, helper_addr as i64);
        let argv = self.b.ins().iconst(types::I64, i64::from(arg));
        let zero = self.b.ins().iconst(types::I64, 0);
        let call = self
            .b
            .ins()
            .call_indirect(sig, helper, &[self.frame_ptr, argv, zero]);
        let status = self.b.inst_results(call)[0];
        let ok_b = self.b.create_block();
        let bad_b = self.b.create_block();
        let is_ok = self.b.ins().icmp_imm(IntCC::Equal, status, 0);
        self.b.ins().brif(is_ok, ok_b, &[], bad_b, &[]);
        self.b.switch_to_block(bad_b);
        let raised_b = self.b.create_block();
        let not_raised_b = self.b.create_block();
        let is_raised = self.b.ins().icmp_imm(IntCC::Equal, status, 1);
        self.b
            .ins()
            .brif(is_raised, raised_b, &[], not_raised_b, &[]);
        self.b.switch_to_block(raised_b);
        self.emit_exit(pc, &snapshot, JitStatus::Raised);
        self.b.switch_to_block(not_raised_b);
        let boxed_b = self.b.create_block();
        let reject_b = self.b.create_block();
        let is_boxed = self.b.ins().icmp_imm(IntCC::Equal, status, 2);
        self.b.ins().brif(is_boxed, boxed_b, &[], reject_b, &[]);
        self.b.switch_to_block(boxed_b);
        self.emit_exit(pc + 1, &snapshot, JitStatus::Deopt);
        self.b.switch_to_block(reject_b);
        self.emit_exit(pc, snapshot_full, JitStatus::Deopt);
        self.b.switch_to_block(ok_b);
    }

    /// [`TOp::DynBinary`] / [`TOp::DynCompare`]: stage both operands in
    /// the marshal buffer, run the interpreter's operation through the
    /// helper at `helper_addr`, and push its result on `lane`. Exits as
    /// [`Self::emit_contains_dyn`] does.
    fn emit_dyn_op(&mut self, helper_addr: usize, arg: u32, lane: JitType, pc: u32) {
        let trusted = MemFlags::trusted();
        let snapshot_full = self.vstack.clone();
        let (b, bty) = self.pop();
        let (a, aty) = self.pop();
        for (k, (val, ty)) in [(a, aty), (b, bty)].into_iter().enumerate() {
            let off = (k as i32) * 8;
            self.b.ins().store(trusted, val, self.call_args_base, off);
            let tagv = self.b.ins().iconst(types::I32, Self::tag(ty));
            self.b
                .ins()
                .store(trusted, tagv, self.call_tags_base, (k as i32) * 4);
        }
        let snapshot = self.vstack.clone();

        self.writeback_locals();

        let sig = self.list_helper_sig();
        let helper = self.b.ins().iconst(self.ptr_ty, helper_addr as i64);
        let argv = self.b.ins().iconst(types::I64, i64::from(arg));
        let zero = self.b.ins().iconst(types::I64, 0);
        let call = self
            .b
            .ins()
            .call_indirect(sig, helper, &[self.frame_ptr, argv, zero]);
        let status = self.b.inst_results(call)[0];

        let ok_b = self.b.create_block();
        let bad_b = self.b.create_block();
        let is_ok = self.b.ins().icmp_imm(IntCC::Equal, status, 0);
        self.b.ins().brif(is_ok, ok_b, &[], bad_b, &[]);

        self.b.switch_to_block(bad_b);
        let raised_b = self.b.create_block();
        let not_raised_b = self.b.create_block();
        let is_raised = self.b.ins().icmp_imm(IntCC::Equal, status, 1);
        self.b
            .ins()
            .brif(is_raised, raised_b, &[], not_raised_b, &[]);
        self.b.switch_to_block(raised_b);
        self.emit_exit(pc, &snapshot, JitStatus::Raised);
        self.b.switch_to_block(not_raised_b);
        let boxed_b = self.b.create_block();
        let reject_b = self.b.create_block();
        let is_boxed = self.b.ins().icmp_imm(IntCC::Equal, status, 2);
        self.b.ins().brif(is_boxed, boxed_b, &[], reject_b, &[]);
        self.b.switch_to_block(boxed_b);
        self.emit_exit(pc + 1, &snapshot, JitStatus::Deopt);
        self.b.switch_to_block(reject_b);
        self.emit_exit(pc, &snapshot_full, JitStatus::Deopt);

        self.b.switch_to_block(ok_b);
        let res = self
            .b
            .ins()
            .load(types::I64, trusted, self.frame_ptr, OFF_RET_BITS);
        self.vstack.push((res, lane));
    }

    /// RFC 0074 WS2/WS4: offer an object-lane attribute read to the
    /// embedder. Pop the receiver pin and push the loaded value's pin
    /// when the helper completes the lookup.
    /// `1` = raised at this pc (receiver consumed); `2` = completed
    /// but a guard was invalidated / cap pressure — deopt at the
    /// *next* pc with the parked result (never re-executed); `3` =
    /// rejected before any Python ran — deopt at this pc with the
    /// receiver spilled and re-execute generically.
    fn emit_dyn_attr_get(&mut self, name: u32, pc: u32) {
        let trusted = MemFlags::trusted();
        let snapshot_full = self.vstack.clone();
        let (pin, _) = self.pop();
        let snapshot = self.vstack.clone();

        self.writeback_locals();
        // The native read reuses this exact instruction's inline cache.
        self.store_call_site_pc(pc);

        let sig = self.list_helper_sig();
        let helper = self
            .b
            .ins()
            .iconst(self.ptr_ty, runtime::dyn_attr_get_helper_addr() as i64);
        let namev = self.b.ins().iconst(types::I64, i64::from(name));
        let call = self
            .b
            .ins()
            .call_indirect(sig, helper, &[self.frame_ptr, pin, namev]);
        let status = self.b.inst_results(call)[0];

        let ok_b = self.b.create_block();
        let bad_b = self.b.create_block();
        let is_ok = self.b.ins().icmp_imm(IntCC::Equal, status, 0);
        self.b.ins().brif(is_ok, ok_b, &[], bad_b, &[]);

        self.b.switch_to_block(bad_b);
        let raised_b = self.b.create_block();
        let not_raised_b = self.b.create_block();
        let is_raised = self.b.ins().icmp_imm(IntCC::Equal, status, 1);
        self.b
            .ins()
            .brif(is_raised, raised_b, &[], not_raised_b, &[]);
        self.b.switch_to_block(raised_b);
        self.emit_exit(pc, &snapshot, JitStatus::Raised);
        self.b.switch_to_block(not_raised_b);
        let boxed_b = self.b.create_block();
        let reject_b = self.b.create_block();
        let is_boxed = self.b.ins().icmp_imm(IntCC::Equal, status, 2);
        self.b.ins().brif(is_boxed, boxed_b, &[], reject_b, &[]);
        self.b.switch_to_block(boxed_b);
        self.emit_exit(pc + 1, &snapshot, JitStatus::Deopt);
        self.b.switch_to_block(reject_b);
        self.emit_exit(pc, &snapshot_full, JitStatus::Deopt);

        self.b.switch_to_block(ok_b);
        let res = self
            .b
            .ins()
            .load(types::I64, trusted, self.frame_ptr, OFF_RET_BITS);
        self.vstack.push((res, JitType::Obj));
    }

    /// RFC 0074 WS4 — the eager generic attribute store via
    /// `wpjit_dyn_attr_set`: the value (below the receiver) is staged
    /// tag-typed in `call_args[0]` / `call_tags[0]`; the receiver pin
    /// pops natively. Status protocol as [`Self::emit_dyn_attr_get`]
    /// (no result on ok; `Boxed` means the store *completed* with
    /// invalidated guards — deopt at the next pc, never re-executed).
    fn emit_dyn_attr_set(&mut self, name: u32, pc: u32) {
        let trusted = MemFlags::trusted();
        let snapshot_full = self.vstack.clone();
        let (pin, _) = self.pop();
        let (val, vty) = self.pop();
        self.b.ins().store(trusted, val, self.call_args_base, 0);
        let tagv = self.b.ins().iconst(types::I32, Self::tag(vty));
        self.b.ins().store(trusted, tagv, self.call_tags_base, 0);
        let snapshot = self.vstack.clone();

        self.writeback_locals();
        // The native store reuses this exact instruction's inline cache.
        self.store_call_site_pc(pc);

        let sig = self.list_helper_sig();
        let helper = self
            .b
            .ins()
            .iconst(self.ptr_ty, runtime::dyn_attr_set_helper_addr() as i64);
        let namev = self.b.ins().iconst(types::I64, i64::from(name));
        let call = self
            .b
            .ins()
            .call_indirect(sig, helper, &[self.frame_ptr, pin, namev]);
        let status = self.b.inst_results(call)[0];

        let ok_b = self.b.create_block();
        let bad_b = self.b.create_block();
        let is_ok = self.b.ins().icmp_imm(IntCC::Equal, status, 0);
        self.b.ins().brif(is_ok, ok_b, &[], bad_b, &[]);

        self.b.switch_to_block(bad_b);
        let raised_b = self.b.create_block();
        let not_raised_b = self.b.create_block();
        let is_raised = self.b.ins().icmp_imm(IntCC::Equal, status, 1);
        self.b
            .ins()
            .brif(is_raised, raised_b, &[], not_raised_b, &[]);
        self.b.switch_to_block(raised_b);
        self.emit_exit(pc, &snapshot, JitStatus::Raised);
        self.b.switch_to_block(not_raised_b);
        let boxed_b = self.b.create_block();
        let reject_b = self.b.create_block();
        let is_boxed = self.b.ins().icmp_imm(IntCC::Equal, status, 2);
        self.b.ins().brif(is_boxed, boxed_b, &[], reject_b, &[]);
        self.b.switch_to_block(boxed_b);
        self.emit_exit(pc + 1, &snapshot, JitStatus::Deopt);
        self.b.switch_to_block(reject_b);
        self.emit_exit(pc, &snapshot_full, JitStatus::Deopt);

        self.b.switch_to_block(ok_b);
    }

    /// RFC 0074 WS5 — `str % x` via `wpjit_str_mod`: pop the rhs (any
    /// lane, passed with its tag) and the lhs pin; on ok push the
    /// fresh exact-`str` pin. `1` = raised at this pc; `2` = the
    /// format *completed* but the result surprised (or guards
    /// invalidated / cap pressure) — deopt at the next pc with the
    /// parked result, formatting side effects never re-run; `3` =
    /// rejected before running — deopt at this pc and re-execute.
    fn emit_str_mod(&mut self, pc: u32) {
        let trusted = MemFlags::trusted();
        let snapshot_full = self.vstack.clone();
        let (rhs, rty) = self.pop();
        let (lhs, _) = self.pop();
        let snapshot = self.vstack.clone();

        self.writeback_locals();

        let sig = self.quad_helper_sig();
        let helper = self
            .b
            .ins()
            .iconst(self.ptr_ty, runtime::str_mod_helper_addr() as i64);
        // An f64 rhs travels through an i64 parameter: bitcast.
        let rhs_bits = if rty == JitType::Float {
            self.b.ins().bitcast(types::I64, MemFlags::new(), rhs)
        } else {
            rhs
        };
        let tagv = self.b.ins().iconst(types::I64, Self::tag(rty));
        let call = self
            .b
            .ins()
            .call_indirect(sig, helper, &[self.frame_ptr, lhs, rhs_bits, tagv]);
        let status = self.b.inst_results(call)[0];

        let ok_b = self.b.create_block();
        let bad_b = self.b.create_block();
        let is_ok = self.b.ins().icmp_imm(IntCC::Equal, status, 0);
        self.b.ins().brif(is_ok, ok_b, &[], bad_b, &[]);

        self.b.switch_to_block(bad_b);
        let raised_b = self.b.create_block();
        let not_raised_b = self.b.create_block();
        let is_raised = self.b.ins().icmp_imm(IntCC::Equal, status, 1);
        self.b
            .ins()
            .brif(is_raised, raised_b, &[], not_raised_b, &[]);
        self.b.switch_to_block(raised_b);
        self.emit_exit(pc, &snapshot, JitStatus::Raised);
        self.b.switch_to_block(not_raised_b);
        let boxed_b = self.b.create_block();
        let reject_b = self.b.create_block();
        let is_boxed = self.b.ins().icmp_imm(IntCC::Equal, status, 2);
        self.b.ins().brif(is_boxed, boxed_b, &[], reject_b, &[]);
        self.b.switch_to_block(boxed_b);
        self.emit_exit(pc + 1, &snapshot, JitStatus::Deopt);
        self.b.switch_to_block(reject_b);
        self.emit_exit(pc, &snapshot_full, JitStatus::Deopt);

        self.b.switch_to_block(ok_b);
        let res = self
            .b
            .ins()
            .load(types::I64, trusted, self.frame_ptr, OFF_RET_BITS);
        self.vstack.push((res, JitType::Str));
    }

    /// RFC 0074 WS5 — `s[a:b]` (unit step) via `wpjit_str_slice`,
    /// mirroring [`Self::emit_list_slice`]: absent bounds pass as
    /// `i64::MIN`, a negative status deopts at the *erased
    /// `BUILD_SLICE`'s* pc with the bound operands spilled (absent
    /// ones as materialized `None`s) so the interpreter rebuilds the
    /// slice object and executes the subscript generically.
    fn emit_str_slice(&mut self, start: bool, stop: bool, origin: SliceOrigin, pc: u32) {
        let missing = i64::MIN;
        let stop_v = if stop {
            self.pop().0
        } else {
            self.b.ins().iconst(types::I64, missing)
        };
        let start_v = if start {
            self.pop().0
        } else {
            self.b.ins().iconst(types::I64, missing)
        };
        let (pin, _) = self.pop();
        // Spill shape at the erased BUILD_SLICE: [.., str, start,
        // stop, optional step=None], `None` bounds as object-lane `-1`. The
        // folded `LOAD_CONST slice(...)` shape deopts at the
        // `LOAD_CONST` with just [.., str].
        let mut snapshot = self.vstack.clone();
        snapshot.push((pin, JitType::Str));
        if origin != SliceOrigin::Constant {
            let none = self.b.ins().iconst(types::I64, -1);
            snapshot.push(if start {
                (start_v, JitType::Int)
            } else {
                (none, JitType::Obj)
            });
            snapshot.push(if stop {
                (stop_v, JitType::Int)
            } else {
                (none, JitType::Obj)
            });
            if origin == SliceOrigin::ThreeBounds {
                snapshot.push((none, JitType::Obj));
            }
        }
        // A real minimum-integer stop clamps to zero for every valid
        // Rust slice length. Keep it distinct from the helper's missing-
        // bound sentinel, and retain stop_v unchanged in the deopt snapshot.
        let stop_arg = if stop {
            let minimum = self.b.ins().icmp_imm(IntCC::Equal, stop_v, i64::MIN);
            let zero = self.b.ins().iconst(types::I64, 0);
            self.b.ins().select(minimum, zero, stop_v)
        } else {
            stop_v
        };
        let sig = self.quad_helper_sig();
        let helper = self
            .b
            .ins()
            .iconst(self.ptr_ty, runtime::str_slice_helper_addr() as i64);
        let call =
            self.b
                .ins()
                .call_indirect(sig, helper, &[self.frame_ptr, pin, start_v, stop_arg]);
        let res = self.b.inst_results(call)[0];
        let bad = self.b.ins().icmp_imm(IntCC::SignedLessThan, res, 0);
        let cont = self.guard(bad, pc, &snapshot);
        self.b.switch_to_block(cont);
        self.vstack.push((res, JitType::Str));
    }

    /// RFC 0071 WS4 — the opaque-iterator capture behind an erased
    /// `GET_ITER`: `wpjit_get_iter` admits only identity iterables
    /// (`iter(x) is x`); anything else deopts with the operand
    /// spilled and the interpreter executes the `GET_ITER` — and the
    /// loop — generically.
    fn emit_iter_capture(&mut self, iter_slot: u32, pc: u32) {
        let snapshot = self.vstack.clone();
        let (pin, _) = self.pop();
        let sig = self.pin_helper_sig();
        let helper = self
            .b
            .ins()
            .iconst(self.ptr_ty, runtime::get_iter_helper_addr() as i64);
        let call = self
            .b
            .ins()
            .call_indirect(sig, helper, &[self.frame_ptr, pin]);
        let status = self.b.inst_results(call)[0];
        let bad = self.b.ins().icmp_imm(IntCC::NotEqual, status, 0);
        let cont = self.guard(bad, pc, &snapshot);
        self.b.switch_to_block(cont);
        let iter_var = self.vars[iter_slot as usize].expect("managed iter slot");
        self.def_local(iter_var, pin);
    }

    /// RFC 0073 WS2 — the dict-loop capture behind an erased
    /// `GET_ITER`: `wpjit_dict_iter_new` materializes the pinned
    /// dict's real `DictKeys` iterator and answers its fresh pin,
    /// which lands in the iter slot. A negative status deopts with
    /// the dict spilled and the interpreter executes the `GET_ITER` —
    /// and the loop — generically.
    fn emit_dict_iter_new(&mut self, iter_slot: u32, pc: u32) {
        let snapshot = self.vstack.clone();
        let (pin, _) = self.pop();
        let sig = self.pin_helper_sig();
        let helper = self
            .b
            .ins()
            .iconst(self.ptr_ty, runtime::dict_iter_helper_addr() as i64);
        let call = self
            .b
            .ins()
            .call_indirect(sig, helper, &[self.frame_ptr, pin]);
        let res = self.b.inst_results(call)[0];
        let bad = self.b.ins().icmp_imm(IntCC::SignedLessThan, res, 0);
        let cont = self.guard(bad, pc, &snapshot);
        self.b.switch_to_block(cont);
        let iter_var = self.vars[iter_slot as usize].expect("managed iter slot");
        self.def_local(iter_var, res);
    }

    /// RFC 0071 WS4 — `BUILD_LIST k`: the elements are staged through
    /// the marshal buffer (like `CallPy` arguments); `none_fill`
    /// passes an empty buffer and the helper writes `n` `None`s. RFC
    /// 0073 WS1 — a `mixed` literal also stages per-element tags and
    /// passes `-1` as the lane tag, telling the helper to box each
    /// element by its own tag. The helper answers the fresh pin
    /// index, negative on cap pressure (deopt: the interpreter
    /// re-executes the `BUILD_LIST`).
    fn emit_build_list(
        &mut self,
        n: u32,
        elem: JitType,
        none_fill: bool,
        mixed: bool,
        konst: bool,
        pc: u32,
    ) {
        // The deopt re-executes the `BUILD_LIST`, so the spill must
        // hold all `n` elements: the natives still on the stack, or —
        // for `none_fill` — materialized `None` markers (`-1` on the
        // object lane), which never occupied native slots. The folded
        // constant literal re-executes `BUILD_LIST 0`: nothing to hold.
        let mut snapshot = self.vstack.clone();
        if konst {
            snapshot.truncate(snapshot.len() - n as usize);
        }
        if none_fill {
            for _ in 0..n {
                let none = self.b.ins().iconst(types::I64, -1);
                snapshot.push((none, JitType::Obj));
            }
        } else {
            self.stage_elements(n, mixed);
        }
        let sig = self.quad_helper_sig();
        let helper = self
            .b
            .ins()
            .iconst(self.ptr_ty, runtime::build_list_helper_addr() as i64);
        let nv = self.b.ins().iconst(types::I64, i64::from(n));
        let tag_imm = if mixed { -1 } else { Self::tag(elem) };
        let tag = self.b.ins().iconst(types::I64, tag_imm);
        let fill = self.b.ins().iconst(types::I64, i64::from(none_fill));
        let call = self
            .b
            .ins()
            .call_indirect(sig, helper, &[self.frame_ptr, nv, tag, fill]);
        let res = self.b.inst_results(call)[0];
        let bad = self.b.ins().icmp_imm(IntCC::SignedLessThan, res, 0);
        let cont = self.guard(bad, pc, &snapshot);
        self.b.switch_to_block(cont);
        let lane = JitType::list_of(elem).unwrap_or(JitType::ListObj);
        self.vstack.push((res, lane));
    }

    /// Pop the top `n` native stack values into the marshal buffer
    /// (bottom-to-top); with `tags`, also write each value's
    /// [`SlotTag`] into the parallel tag buffer.
    fn stage_elements(&mut self, n: u32, tags: bool) {
        let trusted = MemFlags::trusted();
        let base = self.vstack.len() - n as usize;
        for (j, &(v, ty)) in self.vstack[base..].iter().enumerate() {
            let voff = (j as i32) * 8;
            self.b.ins().store(trusted, v, self.call_args_base, voff);
            if tags {
                let t = self.b.ins().iconst(types::I32, Self::tag(ty));
                let toff = (j as i32) * 4;
                self.b.ins().store(trusted, t, self.call_tags_base, toff);
            }
        }
        self.vstack.truncate(base);
    }

    /// RFC 0073 WS1 — `BUILD_TUPLE k` through `wpjit_build_tuple`:
    /// elements stage with per-element tags (a tuple literal mixes
    /// lanes freely), the fresh tuple pins on the object lane.
    /// Negative status deopts (cap pressure) and the interpreter
    /// re-executes the `BUILD_TUPLE`.
    fn emit_build_tuple(&mut self, n: u32, pc: u32) {
        let snapshot = self.vstack.clone();
        self.stage_elements(n, true);
        let sig = self.pin_helper_sig();
        let helper = self
            .b
            .ins()
            .iconst(self.ptr_ty, runtime::build_tuple_helper_addr() as i64);
        let nv = self.b.ins().iconst(types::I64, i64::from(n));
        let call = self
            .b
            .ins()
            .call_indirect(sig, helper, &[self.frame_ptr, nv]);
        let res = self.b.inst_results(call)[0];
        let bad = self.b.ins().icmp_imm(IntCC::SignedLessThan, res, 0);
        let cont = self.guard(bad, pc, &snapshot);
        self.b.switch_to_block(cont);
        self.vstack.push((res, JitType::Obj));
    }

    /// RFC 0076 WS8 — `BUILD_SET k` through `wpjit_build_set`:
    /// elements stage with per-element tags (like a tuple literal),
    /// the fresh set pins on the object lane. Negative status deopts
    /// (cap pressure, or an element whose hashing could run Python)
    /// and the interpreter re-executes the `BUILD_SET`.
    fn emit_build_set(&mut self, n: u32, pc: u32) {
        let snapshot = self.vstack.clone();
        self.stage_elements(n, true);
        let sig = self.pin_helper_sig();
        let helper = self
            .b
            .ins()
            .iconst(self.ptr_ty, runtime::build_set_helper_addr() as i64);
        let nv = self.b.ins().iconst(types::I64, i64::from(n));
        let call = self
            .b
            .ins()
            .call_indirect(sig, helper, &[self.frame_ptr, nv]);
        let res = self.b.inst_results(call)[0];
        let bad = self.b.ins().icmp_imm(IntCC::SignedLessThan, res, 0);
        let cont = self.guard(bad, pc, &snapshot);
        self.b.switch_to_block(cont);
        self.vstack.push((res, JitType::Obj));
    }

    /// RFC 0073 WS2 — `BUILD_MAP n` through `wpjit_build_map`:
    /// `2n` interleaved key/value entries stage with per-element tags,
    /// the fresh dict pins on the dict lane. Negative status deopts
    /// (cap pressure, key-lane surprise) and the interpreter
    /// re-executes the `BUILD_MAP`.
    fn emit_build_map(&mut self, n: u32, pc: u32) {
        let snapshot = self.vstack.clone();
        self.stage_elements(2 * n, true);
        let sig = self.pin_helper_sig();
        let helper = self
            .b
            .ins()
            .iconst(self.ptr_ty, runtime::build_map_helper_addr() as i64);
        let nv = self.b.ins().iconst(types::I64, i64::from(n));
        let call = self
            .b
            .ins()
            .call_indirect(sig, helper, &[self.frame_ptr, nv]);
        let res = self.b.inst_results(call)[0];
        let bad = self.b.ins().icmp_imm(IntCC::SignedLessThan, res, 0);
        let cont = self.guard(bad, pc, &snapshot);
        self.b.switch_to_block(cont);
        self.vstack.push((res, JitType::Dict));
    }

    /// RFC 0073 WS3 — `BUILD_STRING n` through `wpjit_build_string`:
    /// `str`-pin parts stage in order, the joined string pins on the
    /// `Str` lane. Negative status deopts (cap pressure, pin
    /// surprise) and the interpreter re-executes the `BUILD_STRING`.
    fn emit_build_string(&mut self, n: u32, pc: u32) {
        let snapshot = self.vstack.clone();
        self.stage_elements(n, false);
        let sig = self.pin_helper_sig();
        let helper = self
            .b
            .ins()
            .iconst(self.ptr_ty, runtime::build_string_helper_addr() as i64);
        let nv = self.b.ins().iconst(types::I64, i64::from(n));
        let call = self
            .b
            .ins()
            .call_indirect(sig, helper, &[self.frame_ptr, nv]);
        let res = self.b.inst_results(call)[0];
        let bad = self.b.ins().icmp_imm(IntCC::SignedLessThan, res, 0);
        let cont = self.guard(bad, pc, &snapshot);
        self.b.switch_to_block(cont);
        self.vstack.push((res, JitType::Str));
    }

    /// RFC 0073 WS3 — guarded exact-`str` `+` through
    /// `wpjit_str_concat`: pops the two pins, pushes the fresh joined
    /// pin. Negative status deopts (cap pressure, pin surprise) and
    /// the interpreter re-executes the add.
    fn emit_str_concat(&mut self, pc: u32) {
        let snapshot = self.vstack.clone();
        let (b_pin, _) = self.pop();
        let (a_pin, _) = self.pop();
        let sig = self.list_helper_sig();
        let helper = self
            .b
            .ins()
            .iconst(self.ptr_ty, runtime::str_concat_helper_addr() as i64);
        let call = self
            .b
            .ins()
            .call_indirect(sig, helper, &[self.frame_ptr, a_pin, b_pin]);
        let res = self.b.inst_results(call)[0];
        let bad = self.b.ins().icmp_imm(IntCC::SignedLessThan, res, 0);
        let cont = self.guard(bad, pc, &snapshot);
        self.b.switch_to_block(cont);
        self.vstack.push((res, JitType::Str));
    }

    /// RFC 0073 WS3 — `s[i]` through `wpjit_str_get`: ASCII-only O(1)
    /// byte indexing, single-codepoint result pinned. A non-ASCII
    /// receiver or an out-of-range index deopts at this pc (the exact
    /// `IndexError` comes from the interpreter's re-execution).
    fn emit_str_get(&mut self, pc: u32) {
        let snapshot = self.vstack.clone();
        let (idx, _) = self.pop();
        let (pin, _) = self.pop();
        let sig = self.list_helper_sig();
        let helper = self
            .b
            .ins()
            .iconst(self.ptr_ty, runtime::str_get_helper_addr() as i64);
        let call = self
            .b
            .ins()
            .call_indirect(sig, helper, &[self.frame_ptr, pin, idx]);
        let res = self.b.inst_results(call)[0];
        let bad = self.b.ins().icmp_imm(IntCC::SignedLessThan, res, 0);
        let cont = self.guard(bad, pc, &snapshot);
        self.b.switch_to_block(cont);
        self.vstack.push((res, JitType::Str));
    }

    /// Guard an opaque operand without consuming it on failure. Successful
    /// guards replace only its native representation; the pin stays owned
    /// by this activation until its ordinary cleanup.
    fn emit_unbox_int(&mut self, depth: u8, pc: u32) {
        let snapshot = self.vstack.clone();
        let index = self.vstack.len() - 1 - usize::from(depth);
        let (pin, lane) = self.vstack[index];
        debug_assert_eq!(lane, JitType::Obj);
        let sig = self.pin_helper_sig();
        let helper = self
            .b
            .ins()
            .iconst(self.ptr_ty, runtime::unbox_int_helper_addr() as i64);
        let call = self
            .b
            .ins()
            .call_indirect(sig, helper, &[self.frame_ptr, pin]);
        let status = self.b.inst_results(call)[0];
        let bad = self.b.ins().icmp_imm(IntCC::NotEqual, status, 0);
        let cont = self.guard(bad, pc, &snapshot);
        self.b.switch_to_block(cont);
        let value = self.b.ins().load(
            types::I64,
            MemFlags::trusted(),
            self.frame_ptr,
            OFF_RET_BITS,
        );
        self.vstack[index] = (value, JitType::Int);
    }

    /// [`TOp::UnboxFloat`]: guard an object operand as a `float` (see
    /// the op for `promote`) and replace it with its unboxed value. A
    /// miss deopts with the operand stack as it was.
    fn emit_unbox_float(&mut self, depth: u8, promote: u8, pc: u32) {
        let snapshot = self.vstack.clone();
        let index = self.vstack.len() - 1 - usize::from(depth);
        let (pin, lane) = self.vstack[index];
        debug_assert_eq!(lane, JitType::Obj);
        let sig = self.pin_helper_sig();
        let helper = self
            .b
            .ins()
            .iconst(self.ptr_ty, runtime::unbox_float_helper_addr() as i64);
        // The `None` pin (-1) stays negative; any other pin carries the
        // mode in its high bits.
        let mode = self.b.ins().iconst(types::I64, i64::from(promote) << 32);
        let packed = self.b.ins().bor(pin, mode);
        let none = self.b.ins().icmp_imm(IntCC::SignedLessThan, pin, 0);
        let arg = self.b.ins().select(none, pin, packed);
        let call = self
            .b
            .ins()
            .call_indirect(sig, helper, &[self.frame_ptr, arg]);
        let status = self.b.inst_results(call)[0];
        let bad = self.b.ins().icmp_imm(IntCC::NotEqual, status, 0);
        let cont = self.guard(bad, pc, &snapshot);
        self.b.switch_to_block(cont);
        let value = self.b.ins().load(
            types::F64,
            MemFlags::trusted(),
            self.frame_ptr,
            OFF_RET_BITS,
        );
        self.vstack[index] = (value, JitType::Float);
    }

    /// A code constant through its memoizing pin helper. Negative status
    /// deopts and the interpreter re-executes LOAD_CONST.
    fn emit_const_pin(&mut self, idx: u32, pc: u32, helper_addr: usize, lane: JitType) {
        let snapshot = self.vstack.clone();
        let sig = self.pin_helper_sig();
        let helper = self.b.ins().iconst(self.ptr_ty, helper_addr as i64);
        let idxv = self.b.ins().iconst(types::I64, i64::from(idx));
        let call = self
            .b
            .ins()
            .call_indirect(sig, helper, &[self.frame_ptr, idxv]);
        let res = self.b.inst_results(call)[0];
        let bad = self.b.ins().icmp_imm(IntCC::SignedLessThan, res, 0);
        let cont = self.guard(bad, pc, &snapshot);
        self.b.switch_to_block(cont);
        self.vstack.push((res, lane));
    }

    /// RFC 0071 WS4 — `list * int` through `wpjit_list_repeat`
    /// (element `Arc`s shared — CPython's aliasing). Negative status
    /// deopts (cap pressure) and the interpreter re-executes the
    /// multiply.
    fn emit_list_repeat(&mut self, pc: u32) {
        let snapshot = self.vstack.clone();
        let (count, _) = self.pop();
        let (pin, lane) = self.pop();
        let sig = self.list_helper_sig();
        let helper = self
            .b
            .ins()
            .iconst(self.ptr_ty, runtime::list_repeat_helper_addr() as i64);
        let call = self
            .b
            .ins()
            .call_indirect(sig, helper, &[self.frame_ptr, pin, count]);
        let res = self.b.inst_results(call)[0];
        let bad = self.b.ins().icmp_imm(IntCC::SignedLessThan, res, 0);
        let cont = self.guard(bad, pc, &snapshot);
        self.b.switch_to_block(cont);
        self.vstack.push((res, lane));
    }

    /// `list(range(...))` through `wpjit_list_from_range`. The bounds
    /// pop first, so the deopt snapshot is the stack the erased
    /// `LOAD_GLOBAL list` (`deopt_pc`) saw: the interpreter re-runs the
    /// whole expression.
    fn emit_list_from_range(&mut self, pops: u8, deopt_pc: u32) {
        let (stop, _) = self.pop();
        let start = if pops == 2 {
            self.pop().0
        } else {
            self.b.ins().iconst(types::I64, 0)
        };
        let snapshot = self.vstack.clone();
        let sig = self.list_helper_sig();
        let helper = self
            .b
            .ins()
            .iconst(self.ptr_ty, runtime::list_from_range_helper_addr() as i64);
        let call = self
            .b
            .ins()
            .call_indirect(sig, helper, &[self.frame_ptr, start, stop]);
        let res = self.b.inst_results(call)[0];
        let bad = self.b.ins().icmp_imm(IntCC::SignedLessThan, res, 0);
        let cont = self.guard(bad, deopt_pc, &snapshot);
        self.b.switch_to_block(cont);
        self.vstack.push((res, JitType::ListInt));
    }

    /// RFC 0071 WS4 — `xs[a:b]` (unit step) through
    /// `wpjit_list_slice`. Absent bounds (the `None` markers of the
    /// erased `BUILD_SLICE`) pass as `i64::MIN`; present bounds pop
    /// `stop` above `start`. A negative status deopts at the *erased
    /// `BUILD_SLICE`'s* pc (`pc` here — the analyzer guarantees the
    /// `BINARY_SUBSCR` immediately follows it), with the original bound
    /// operands spilled — absent ones as materialized `None`s — so
    /// the interpreter rebuilds the slice object and executes the
    /// subscript generically.
    fn emit_list_slice(&mut self, start: bool, stop: bool, origin: SliceOrigin, pc: u32) {
        let missing = i64::MIN;
        let stop_v = if stop {
            self.pop().0
        } else {
            self.b.ins().iconst(types::I64, missing)
        };
        let start_v = if start {
            self.pop().0
        } else {
            self.b.ins().iconst(types::I64, missing)
        };
        let (pin, lane) = self.pop();
        // Spill shape at the erased BUILD_SLICE: [.., list, start,
        // stop, optional step=None], `None` bounds as object-lane `-1`. The
        // folded `LOAD_CONST slice(...)` shape deopts at the
        // `LOAD_CONST` with just [.., list].
        let mut snapshot = self.vstack.clone();
        snapshot.push((pin, lane));
        if origin != SliceOrigin::Constant {
            let none = self.b.ins().iconst(types::I64, -1);
            snapshot.push(if start {
                (start_v, JitType::Int)
            } else {
                (none, JitType::Obj)
            });
            snapshot.push(if stop {
                (stop_v, JitType::Int)
            } else {
                (none, JitType::Obj)
            });
            if origin == SliceOrigin::ThreeBounds {
                snapshot.push((none, JitType::Obj));
            }
        }
        // A real minimum-integer stop clamps to zero for every valid
        // Rust slice length. Keep it distinct from the helper's missing-
        // bound sentinel, and retain stop_v unchanged in the deopt snapshot.
        let stop_arg = if stop {
            let minimum = self.b.ins().icmp_imm(IntCC::Equal, stop_v, i64::MIN);
            let zero = self.b.ins().iconst(types::I64, 0);
            self.b.ins().select(minimum, zero, stop_v)
        } else {
            stop_v
        };
        let sig = self.quad_helper_sig();
        let helper = self
            .b
            .ins()
            .iconst(self.ptr_ty, runtime::list_slice_helper_addr() as i64);
        let call =
            self.b
                .ins()
                .call_indirect(sig, helper, &[self.frame_ptr, pin, start_v, stop_arg]);
        let res = self.b.inst_results(call)[0];
        let bad = self.b.ins().icmp_imm(IntCC::SignedLessThan, res, 0);
        let cont = self.guard(bad, pc, &snapshot);
        self.b.switch_to_block(cont);
        self.vstack.push((res, lane));
    }

    /// The shared `(frame, i64, i64, i64) -> i64` signature of the
    /// `wpjit_build_list`/`wpjit_list_slice` helpers (RFC 0071 WS4,
    /// lazy).
    fn quad_helper_sig(&mut self) -> SigRef {
        if let Some(sig) = self.quad_sig {
            return sig;
        }
        let mut sig = Signature::new(self.b.func.signature.call_conv);
        sig.params.push(AbiParam::new(self.ptr_ty)); // frame
        sig.params.push(AbiParam::new(types::I64));
        sig.params.push(AbiParam::new(types::I64));
        sig.params.push(AbiParam::new(types::I64));
        sig.returns.push(AbiParam::new(types::I64)); // pin / status
        let r = self.b.import_signature(sig);
        self.quad_sig = Some(r);
        r
    }

    /// RFC 0071 WS6 — pinned-`str` equality via `wpjit_str_eq`. The
    /// helper answers `0`/`1`; any other status is a pin miss
    /// (defensive) and deopts with both operands spilled.
    fn emit_str_eq(&mut self, negate: bool, pc: u32) {
        let snapshot = self.vstack.clone();
        let (b_pin, _) = self.pop();
        let (a_pin, _) = self.pop();
        let sig = self.list_helper_sig();
        let helper = self
            .b
            .ins()
            .iconst(self.ptr_ty, runtime::str_eq_helper_addr() as i64);
        let call = self
            .b
            .ins()
            .call_indirect(sig, helper, &[self.frame_ptr, a_pin, b_pin]);
        let status = self.b.inst_results(call)[0];
        let bad = self.b.ins().icmp_imm(IntCC::UnsignedGreaterThan, status, 1);
        let cont = self.guard(bad, pc, &snapshot);
        self.b.switch_to_block(cont);
        let res = if negate {
            self.b.ins().bxor_imm(status, 1)
        } else {
            status
        };
        self.vstack.push((res, JitType::Bool));
    }

    /// `a is b` on two object lanes (see [`TOp::IsObj`]): equal machine
    /// values (the same pin, or both `None`) are the same object; any
    /// other pair asks the helper, whose status above `1` deopts.
    fn emit_is_obj(&mut self, negate: bool, pc: u32) {
        let snapshot = self.vstack.clone();
        let (b_pin, _) = self.pop();
        let (a_pin, _) = self.pop();
        let same = self.b.ins().icmp(IntCC::Equal, a_pin, b_pin);
        let slow = self.b.create_block();
        let merge = self.b.create_block();
        self.b.append_block_param(merge, types::I64);
        let one = self.b.ins().iconst(types::I64, 1);
        self.b.ins().brif(same, merge, &[one.into()], slow, &[]);
        self.b.switch_to_block(slow);
        let sig = self.list_helper_sig();
        let helper = self
            .b
            .ins()
            .iconst(self.ptr_ty, runtime::is_obj_helper_addr() as i64);
        let call = self
            .b
            .ins()
            .call_indirect(sig, helper, &[self.frame_ptr, a_pin, b_pin]);
        let status = self.b.inst_results(call)[0];
        let bad = self.b.ins().icmp_imm(IntCC::UnsignedGreaterThan, status, 1);
        let cont = self.guard(bad, pc, &snapshot);
        self.b.switch_to_block(cont);
        self.b.ins().jump(merge, &[status.into()]);
        self.b.switch_to_block(merge);
        let r = self.b.block_params(merge)[0];
        let res = if negate {
            self.b.ins().bxor_imm(r, 1)
        } else {
            r
        };
        self.vstack.push((res, JitType::Bool));
    }

    /// RFC 0071 WS6 — pinned-`str`/`bytes` length (helper selected by
    /// address; both share the [`Self::pin_helper_sig`] shape and the
    /// negative-status deopt of `emit_list_len`).
    fn emit_pin_len(&mut self, helper_addr: usize, pc: u32) {
        let snapshot = self.vstack.clone();
        let (pin, _) = self.pop();
        let sig = self.pin_helper_sig();
        let helper = self.b.ins().iconst(self.ptr_ty, helper_addr as i64);
        let call = self
            .b
            .ins()
            .call_indirect(sig, helper, &[self.frame_ptr, pin]);
        let len = self.b.inst_results(call)[0];
        let bad = self.b.ins().icmp_imm(IntCC::SignedLessThan, len, 0);
        let cont = self.guard(bad, pc, &snapshot);
        self.b.switch_to_block(cont);
        self.vstack.push((len, JitType::Int));
    }

    /// RFC 0071 WS6 — pinned-`bytes` subscript via `wpjit_bytes_get`
    /// (bounds-checked; out of range deopts and the interpreter
    /// re-executes the subscript to raise the exact `IndexError`).
    fn emit_bytes_get(&mut self, pc: u32) {
        let trusted = MemFlags::trusted();
        let snapshot = self.vstack.clone();
        let (idx, _) = self.pop();
        let (pin, _) = self.pop();
        let sig = self.list_helper_sig();
        let helper = self
            .b
            .ins()
            .iconst(self.ptr_ty, runtime::bytes_get_helper_addr() as i64);
        let call = self
            .b
            .ins()
            .call_indirect(sig, helper, &[self.frame_ptr, pin, idx]);
        let status = self.b.inst_results(call)[0];
        let bad = self.b.ins().icmp_imm(IntCC::NotEqual, status, 0);
        let cont = self.guard(bad, pc, &snapshot);
        self.b.switch_to_block(cont);
        let res = self
            .b
            .ins()
            .load(types::I64, trusted, self.frame_ptr, OFF_RET_BITS);
        self.vstack.push((res, JitType::Int));
    }

    /// RFC 0073 WS2 — the per-site dict key-lane discriminant the
    /// helpers dispatch on.
    fn dict_key_tag(key: JitType) -> i64 {
        match key {
            JitType::Str => runtime::DICT_KEY_STR,
            _ => runtime::DICT_KEY_INT,
        }
    }

    /// RFC 0073 WS2 — the per-site dict value-lane discriminant.
    fn dict_val_tag(val: JitType) -> i64 {
        match val {
            JitType::Float => runtime::DICT_VAL_FLOAT,
            JitType::Obj => runtime::DICT_VAL_OBJ,
            _ => runtime::DICT_VAL_INT,
        }
    }

    /// RFC 0073 WS2 — `d[k]` on a pinned exact dict. Three-way status:
    /// ok (value in `ret_bits`, trained lane), *raised* (a missing key
    /// — the helper parked the exact `KeyError(key)`; the exit resumes
    /// past the subscript with the operands consumed), or deopt (lane
    /// surprise — the interpreter re-executes the subscript with both
    /// operands restored).
    fn emit_dict_get(&mut self, key: JitType, val: JitType, pc: u32) {
        let trusted = MemFlags::trusted();
        let snapshot_full = self.vstack.clone();
        let (k, _) = self.pop();
        let (pin, _) = self.pop();
        let snapshot = self.vstack.clone();
        let status = self.emit_dict_call(runtime::dict_get_helper_addr(), pin, k, key, val);
        let ok_b = self.b.create_block();
        let bad_b = self.b.create_block();
        let is_ok = self.b.ins().icmp_imm(IntCC::Equal, status, 0);
        self.b.ins().brif(is_ok, ok_b, &[], bad_b, &[]);
        self.b.switch_to_block(bad_b);
        let raised_b = self.b.create_block();
        let deopt_b = self.b.create_block();
        let is_raised = self.b.ins().icmp_imm(IntCC::Equal, status, 2);
        self.b.ins().brif(is_raised, raised_b, &[], deopt_b, &[]);
        self.b.switch_to_block(raised_b);
        self.emit_exit(pc, &snapshot, JitStatus::Raised);
        self.b.switch_to_block(deopt_b);
        self.emit_exit(pc, &snapshot_full, JitStatus::Deopt);
        self.b.switch_to_block(ok_b);
        let res = self
            .b
            .ins()
            .load(Self::cl_ty(val), trusted, self.frame_ptr, OFF_RET_BITS);
        self.vstack.push((res, val));
    }

    /// RFC 0073 WS2 — `d[k] = v` on a pinned exact dict. The value is
    /// staged through `ret_bits` (the `emit_list_set` trick); any
    /// helper refusal (watchers active, reapable displaced value, lane
    /// surprise) deopts at this pc and the interpreter re-executes the
    /// store generically.
    fn emit_dict_set(&mut self, key: JitType, val: JitType, pc: u32) {
        let trusted = MemFlags::trusted();
        let snapshot = self.vstack.clone();
        let (k, _) = self.pop();
        let (pin, _) = self.pop();
        let (v, _) = self.pop();
        self.b.ins().store(trusted, v, self.frame_ptr, OFF_RET_BITS);
        let status = self.emit_dict_call(runtime::dict_set_helper_addr(), pin, k, key, val);
        let bad = self.b.ins().icmp_imm(IntCC::NotEqual, status, 0);
        let cont = self.guard(bad, pc, &snapshot);
        self.b.switch_to_block(cont);
    }

    /// `del d[k]` on a pinned exact dict: any non-zero status deopts
    /// with both operands spilled (the delete did not happen).
    fn emit_dict_del(&mut self, key: JitType, pc: u32) {
        let snapshot = self.vstack.clone();
        let (k, _) = self.pop();
        let (pin, _) = self.pop();
        let status =
            self.emit_dict_call(runtime::dict_del_helper_addr(), pin, k, key, JitType::Int);
        let bad = self.b.ins().icmp_imm(IntCC::NotEqual, status, 0);
        let cont = self.guard(bad, pc, &snapshot);
        self.b.switch_to_block(cont);
    }

    /// RFC 0073 WS2 — `k in d` / `k not in d` on a pinned exact dict.
    /// The membership result lands in `ret_bits`; `negate` inverts it
    /// natively.
    fn emit_dict_contains(&mut self, negate: bool, key: JitType, pc: u32) {
        let trusted = MemFlags::trusted();
        let snapshot = self.vstack.clone();
        let (pin, _) = self.pop();
        let (k, _) = self.pop();
        let status = self.emit_dict_call(
            runtime::dict_contains_helper_addr(),
            pin,
            k,
            key,
            JitType::Int,
        );
        let bad = self.b.ins().icmp_imm(IntCC::NotEqual, status, 0);
        let cont = self.guard(bad, pc, &snapshot);
        self.b.switch_to_block(cont);
        let res = self
            .b
            .ins()
            .load(types::I64, trusted, self.frame_ptr, OFF_RET_BITS);
        let res = if negate {
            self.b.ins().bxor_imm(res, 1)
        } else {
            res
        };
        self.vstack.push((res, JitType::Bool));
    }

    /// Shared dict-helper invocation: `(frame, pin, key_bits, key_tag,
    /// val_tag) -> status`.
    fn emit_dict_call(
        &mut self,
        helper_addr: usize,
        pin: Value,
        key_bits: Value,
        key: JitType,
        val: JitType,
    ) -> Value {
        let sig = self.dict_helper_sig();
        let helper = self.b.ins().iconst(self.ptr_ty, helper_addr as i64);
        let key_tag = self.b.ins().iconst(types::I64, Self::dict_key_tag(key));
        let val_tag = self.b.ins().iconst(types::I64, Self::dict_val_tag(val));
        let call = self.b.ins().call_indirect(
            sig,
            helper,
            &[self.frame_ptr, pin, key_bits, key_tag, val_tag],
        );
        self.b.inst_results(call)[0]
    }

    /// The shared `(frame, pin, key, key_tag, val_tag) -> status`
    /// signature of the dict-lane helpers (RFC 0073 WS2, lazy).
    fn dict_helper_sig(&mut self) -> SigRef {
        if let Some(sig) = self.dict_sig {
            return sig;
        }
        let mut sig = Signature::new(self.b.func.signature.call_conv);
        sig.params.push(AbiParam::new(self.ptr_ty)); // frame
        sig.params.push(AbiParam::new(types::I64)); // pin
        sig.params.push(AbiParam::new(types::I64)); // key bits
        sig.params.push(AbiParam::new(types::I64)); // key tag
        sig.params.push(AbiParam::new(types::I64)); // val tag
        sig.returns.push(AbiParam::new(types::I64)); // status
        let r = self.b.import_signature(sig);
        self.dict_sig = Some(r);
        r
    }

    /// RFC 0069 WS2 — a burned-in `math` intrinsic. The domain guard
    /// deopts *before* the operation with the operand still on the
    /// spilled stack, so the interpreter re-executes the call (the
    /// enclosing math span rebuilds the `[func, null]` pair below the
    /// argument) and raises the exact `ValueError`/`OverflowError`:
    ///
    /// - `sqrt(x)` requires `x >= 0.0` (`NaN` and `-0.0` pass, exactly
    ///   like libm);
    /// - `sin`/`cos` require non-infinite input (`NaN` propagates
    ///   without error, matching CPython);
    /// - `fabs` never errors.
    fn emit_math_intrinsic(&mut self, func: MathFunc, pc: u32) {
        match func {
            MathFunc::Sqrt => {
                let snapshot = self.vstack.clone();
                let (x, _) = self.pop();
                let z = self.b.ins().f64const(0.0);
                let neg = self.b.ins().fcmp(FloatCC::LessThan, x, z);
                let cont = self.guard(neg, pc, &snapshot);
                self.b.switch_to_block(cont);
                let r = self.b.ins().sqrt(x);
                self.vstack.push((r, JitType::Float));
            }
            MathFunc::Fabs => {
                let (x, _) = self.pop();
                let r = self.b.ins().fabs(x);
                self.vstack.push((r, JitType::Float));
            }
            MathFunc::Sin | MathFunc::Cos => {
                let snapshot = self.vstack.clone();
                let (x, _) = self.pop();
                let mag = self.b.ins().fabs(x);
                let inf = self.b.ins().f64const(f64::INFINITY);
                let is_inf = self.b.ins().fcmp(FloatCC::Equal, mag, inf);
                let cont = self.guard(is_inf, pc, &snapshot);
                self.b.switch_to_block(cont);
                let addr = if matches!(func, MathFunc::Sin) {
                    runtime::math_sin_helper_addr()
                } else {
                    runtime::math_cos_helper_addr()
                };
                let sig = self.math_unary_helper_sig();
                let helper = self.b.ins().iconst(self.ptr_ty, addr as i64);
                let call = self.b.ins().call_indirect(sig, helper, &[x]);
                let r = self.b.inst_results(call)[0];
                self.vstack.push((r, JitType::Float));
            }
        }
    }

    /// RFC 0069 WS2 — float floor-div / mod through the registered
    /// Python-semantics helpers, with the zero-divisor deopt *before*
    /// the call (the interpreter re-executes and raises the exact
    /// `ZeroDivisionError`).
    fn emit_float_divmod_helper(&mut self, kind: ArithKind, pc: u32) {
        let snapshot = self.vstack.clone();
        let (b, _) = self.pop();
        let (a, _) = self.pop();
        let z = self.b.ins().f64const(0.0);
        let is_zero = self.b.ins().fcmp(FloatCC::Equal, b, z);
        let cont = self.guard(is_zero, pc, &snapshot);
        self.b.switch_to_block(cont);
        let addr = if matches!(kind, ArithKind::FloorDiv) {
            runtime::float_floordiv_helper_addr()
        } else {
            runtime::float_mod_helper_addr()
        };
        let sig = self.math_binary_helper_sig();
        let helper = self.b.ins().iconst(self.ptr_ty, addr as i64);
        let call = self.b.ins().call_indirect(sig, helper, &[a, b]);
        let r = self.b.inst_results(call)[0];
        self.vstack.push((r, JitType::Float));
    }

    /// The imported `f64 -> f64` signature of the libm-backed unary
    /// math helpers (RFC 0069 WS2, lazy).
    fn math_unary_helper_sig(&mut self) -> SigRef {
        if let Some(sig) = self.math_unary_sig {
            return sig;
        }
        let mut sig = Signature::new(self.b.func.signature.call_conv);
        sig.params.push(AbiParam::new(types::F64));
        sig.returns.push(AbiParam::new(types::F64));
        let r = self.b.import_signature(sig);
        self.math_unary_sig = Some(r);
        r
    }

    /// The imported `(f64, f64) -> f64` signature of the float
    /// floor-div / mod helpers (RFC 0069 WS2, lazy).
    fn math_binary_helper_sig(&mut self) -> SigRef {
        if let Some(sig) = self.math_binary_sig {
            return sig;
        }
        let mut sig = Signature::new(self.b.func.signature.call_conv);
        sig.params.push(AbiParam::new(types::F64));
        sig.params.push(AbiParam::new(types::F64));
        sig.returns.push(AbiParam::new(types::F64));
        let r = self.b.import_signature(sig);
        self.math_binary_sig = Some(r);
        r
    }

    /// RFC 0069 WS1 — a guarded method call via `wpjit_call_method`.
    /// Marshals the `argc` scalar arguments (receiver excluded — its
    /// pin travels as a helper parameter), then dispatches on the
    /// returned [`crate::runtime::CallStatus`]:
    ///
    /// - `Ok` — push the lane-checked result (procedure-shaped callees
    ///   push nothing; their `None` lives only on the interpreter
    ///   stack).
    /// - `Raised` — exit `Raised` at the call pc, operands consumed.
    /// - `Boxed` — the call *completed* with an unrepresentable result
    ///   (or invalidated caller guard): deopt at `pc + 1`, the parked
    ///   result is pushed by the embedder. Never re-executes.
    /// - `Reject` — the guard failed *before* the call ran: deopt at
    ///   the call pc with the receiver and arguments still on the
    ///   spilled stack (the open method span rebuilds the receiver as
    ///   the bound method + `Unbound` pair), so the interpreter
    ///   re-executes the call generically.
    fn emit_call_method(&mut self, token: u32, argc: u8, ret: MethodRet, pc: u32) {
        let native = token & runtime::METHOD_NATIVE != 0;
        let helper = if native {
            runtime::call_native_method_helper_addr()
        } else {
            runtime::call_method_helper_addr()
        };
        self.emit_method_helper_call(helper, token, argc, ret, pc, !native);
    }

    /// [`Self::emit_call_method`]'s body over any helper sharing the
    /// method helper's shape and status protocol (`wpjit_obj_getitem`
    /// stages its index as the one argument). `writeback` stores the
    /// locals first, for a helper that may run Python on this
    /// activation's behalf.
    fn emit_method_helper_call(
        &mut self,
        helper_addr: usize,
        token: u32,
        argc: u8,
        ret: MethodRet,
        pc: u32,
        writeback: bool,
    ) {
        let trusted = MemFlags::trusted();
        let n = argc as usize;
        // Snapshot *including* receiver + args: the Reject exit re-runs
        // the CALL in the interpreter.
        let snapshot_full = self.vstack.clone();
        let base = self.vstack.len() - n;
        for (j, &(v, ty)) in self.vstack[base..].iter().enumerate() {
            let voff = (j as i32) * 8;
            let toff = (j as i32) * 4;
            self.b.ins().store(trusted, v, self.call_args_base, voff);
            let tagv = self.b.ins().iconst(types::I32, Self::tag(ty));
            self.b.ins().store(trusted, tagv, self.call_tags_base, toff);
        }
        // What a field update would add: the `int` argument, or (none
        // passed) the method's literal.
        let update_arg = match &self.vstack[base..] {
            [] => Some(None),
            [(v, JitType::Int)] => Some(Some(*v)),
            _ => None,
        };
        self.vstack.truncate(base);
        let (pin, _) = self.pop();
        let snapshot = self.vstack.clone();

        // A method whose body is a field update, run in line once the
        // helper has armed it (a Python method's helper only); anything
        // else takes the helper below.
        let inline = match (runtime::obj_layout(), ret, update_arg) {
            (Some(l), MethodRet::Scalar(JitType::Int), Some(arg))
                if helper_addr == runtime::call_method_helper_addr() =>
            {
                Some((*l, arg))
            }
            _ => None,
        };
        let mut join = None;
        if let Some((l, arg)) = inline {
            let miss = self.b.create_block();
            let done = self.b.create_block();
            self.b.append_block_param(done, types::I64);
            let v = self.inline_method_update(&l, pin, token, arg, miss);
            self.b.ins().jump(done, &[BlockArg::from(v)]);
            self.b.switch_to_block(miss);
            join = Some(done);
        }

        if writeback {
            self.writeback_locals();
        }
        self.store_call_site_pc(pc);

        let expect = match ret {
            MethodRet::None => SlotTag::None as i64,
            MethodRet::Scalar(t) => Self::tag(t),
        };
        let sig = self.call_method_helper_sig();
        let helper = self.b.ins().iconst(self.ptr_ty, helper_addr as i64);
        let tokenv = self.b.ins().iconst(types::I32, i64::from(token));
        let argcv = self.b.ins().iconst(types::I32, i64::from(argc));
        let expectv = self.b.ins().iconst(types::I32, expect);
        let call =
            self.b
                .ins()
                .call_indirect(sig, helper, &[self.frame_ptr, tokenv, pin, argcv, expectv]);
        let status = self.b.inst_results(call)[0];

        let ok_b = self.b.create_block();
        let bad_b = self.b.create_block();
        let is_ok = self.b.ins().icmp_imm(IntCC::Equal, status, 0);
        self.b.ins().brif(is_ok, ok_b, &[], bad_b, &[]);

        self.b.switch_to_block(bad_b);
        let raised_b = self.b.create_block();
        let not_raised_b = self.b.create_block();
        let is_raised = self.b.ins().icmp_imm(IntCC::Equal, status, 1);
        self.b
            .ins()
            .brif(is_raised, raised_b, &[], not_raised_b, &[]);
        self.b.switch_to_block(raised_b);
        self.emit_exit(pc, &snapshot, JitStatus::Raised);
        self.b.switch_to_block(not_raised_b);
        let boxed_b = self.b.create_block();
        let reject_b = self.b.create_block();
        let is_boxed = self.b.ins().icmp_imm(IntCC::Equal, status, 2);
        self.b.ins().brif(is_boxed, boxed_b, &[], reject_b, &[]);
        self.b.switch_to_block(boxed_b);
        self.emit_exit(pc + 1, &snapshot, JitStatus::Deopt);
        self.b.switch_to_block(reject_b);
        self.emit_exit(pc, &snapshot_full, JitStatus::Deopt);

        self.b.switch_to_block(ok_b);
        if let MethodRet::Scalar(t) = ret {
            let res = self
                .b
                .ins()
                .load(Self::cl_ty(t), trusted, self.frame_ptr, OFF_RET_BITS);
            let res = match join {
                Some(done) => {
                    self.b.ins().jump(done, &[BlockArg::from(res)]);
                    self.b.switch_to_block(done);
                    self.b.block_params(done)[0]
                }
                None => res,
            };
            self.vstack.push((res, t));
        }
    }

    /// The armed field update of method token `token` on the pinned
    /// receiver `pin` (see [`runtime::ObjLayout::method_upd_idx`]): the
    /// updated field's new value, with the current block continuing past
    /// the store. A receiver of another class version, values not split
    /// over the class's names or numerous enough to shadow the method, a
    /// swapped `__code__`, a non-`int` field, an overflow, or an active
    /// observer, dict watcher or exotic key branches to `miss` (the helper,
    /// which runs the call exactly) before anything is stored.
    fn inline_method_update(
        &mut self,
        l: &runtime::ObjLayout,
        pin: Value,
        token: u32,
        arg: Option<Value>,
        miss: Block,
    ) -> Value {
        let t = MemFlags::trusted();
        let ptr = self.ptr_ty;
        let ctx = self.b.ins().load(ptr, t, self.frame_ptr, OFF_CTX);
        // The entry: armed, for this call's increment form.
        let methods = self.b.ins().load(ptr, t, ctx, l.ctx_methods);
        let n = self.b.ins().load(types::I64, t, methods, l.methods_len);
        let out = self
            .b
            .ins()
            .icmp_imm(IntCC::UnsignedLessThanOrEqual, n, i64::from(token));
        self.miss_if(out, miss);
        let mbuf = self.b.ins().load(ptr, t, methods, l.methods_buf);
        let e = self
            .b
            .ins()
            .iadd_imm(mbuf, i64::from(token) * i64::from(l.method_size));
        let idx = self.b.ins().uload32(t, e, l.method_upd_idx);
        let from_arg = self.b.ins().uload32(t, e, l.method_upd_from_arg);
        let unarmed = self
            .b
            .ins()
            .icmp_imm(IntCC::Equal, idx, i64::from(u32::MAX));
        let form = self
            .b
            .ins()
            .icmp_imm(IntCC::NotEqual, from_arg, i64::from(arg.is_some()));
        // The function's code, and the gates.
        let code_at = self.b.ins().load(ptr, t, e, l.method_upd_code_at);
        let want = self.b.ins().load(ptr, t, e, l.method_upd_code);
        let observers = self.b.ins().iconst(ptr, l.observers as i64);
        let observers = self.b.ins().load(types::I64, t, observers, 0);
        let watchers = self.b.ins().iconst(ptr, l.dict_watchers as i64);
        let watchers = self.b.ins().uload8(types::I64, t, watchers, 0);
        let exotic = self.b.ins().iconst(ptr, l.exotic_keys as i64);
        let exotic = self.b.ins().load(types::I64, t, exotic, 0);
        let gates = self.b.ins().bor(observers, watchers);
        let gates = self.b.ins().bor(gates, exotic);
        let gated = self.b.ins().icmp_imm(IntCC::NotEqual, gates, 0);
        let bad = self.b.ins().bor(unarmed, form);
        let bad = self.b.ins().bor(bad, gated);
        self.miss_if(bad, miss);
        let code = self.b.ins().load(ptr, t, code_at, 0);
        let swapped = self.b.ins().icmp(IntCC::NotEqual, code, want);
        self.miss_if(swapped, miss);
        // The pin: an instance.
        let n = self.b.ins().load(types::I64, t, ctx, l.ctx_pins_len);
        let out = self.b.ins().icmp(IntCC::UnsignedGreaterThanOrEqual, pin, n);
        self.miss_if(out, miss);
        let buf = self.b.ins().load(ptr, t, ctx, l.ctx_pins_ptr);
        let off = self.b.ins().imul_imm(pin, i64::from(l.pin_size));
        let p = self.b.ins().iadd(buf, off);
        let ptag = self.b.ins().uload8(types::I32, t, p, l.pin_tag);
        let otag = self.b.ins().uload8(types::I32, t, p, l.pin_obj);
        let not_obj = self
            .b
            .ins()
            .icmp_imm(IntCC::NotEqual, ptag, i64::from(l.pin_obj_tag));
        let not_inst = self
            .b
            .ins()
            .icmp_imm(IntCC::NotEqual, otag, i64::from(l.tag_instance));
        let bad = self.b.ins().bor(not_obj, not_inst);
        self.miss_if(bad, miss);
        let inst = self.b.ins().load(ptr, t, p, l.pin_obj + 8);
        // Its class and split values, as `split_field_addr` checks them
        // for a write, against the entry's version.
        let ver = self.b.ins().load(types::I64, t, e, l.method_ver);
        let cls = self.b.ins().load(ptr, t, inst, l.inst_class);
        let cver = self.b.ins().load(types::I64, t, cls, l.type_attr_version);
        let lazy = self.b.ins().load(ptr, t, inst, l.inst_dict_lazy);
        let flag = self.b.ins().iconst(ptr, l.cells_unguarded as i64);
        let shared = self.b.ins().uload8(types::I64, t, flag, 0);
        let borrow = self.b.ins().sload32(t, inst, l.inst_split_borrow);
        let block = self.b.ins().load(ptr, t, inst, l.inst_split_block);
        let stale = self.b.ins().icmp(IntCC::NotEqual, cver, ver);
        let busy = self.b.ins().icmp_imm(IntCC::NotEqual, borrow, 0);
        let empty = self.b.ins().icmp_imm(IntCC::Equal, block, 0);
        let other = self.b.ins().bor(lazy, shared);
        let other = self.b.ins().icmp_imm(IntCC::NotEqual, other, 0);
        let bad = self.b.ins().bor(stale, busy);
        let bad = self.b.ins().bor(bad, empty);
        let bad = self.b.ins().bor(bad, other);
        self.miss_if(bad, miss);
        // The class's names, holding the field but not the method's name.
        let keys = self.b.ins().load(ptr, t, block, l.split_keys);
        let ckeys = self.b.ins().load(ptr, t, cls, l.type_shared_keys);
        let len = self.b.ins().uload32(t, block, l.split_len);
        let shadow = self.b.ins().uload32(t, e, l.method_upd_shadow);
        let foreign = self.b.ins().icmp(IntCC::NotEqual, keys, ckeys);
        let absent = self
            .b
            .ins()
            .icmp(IntCC::UnsignedGreaterThanOrEqual, idx, len);
        let shadowed = self.b.ins().icmp(IntCC::UnsignedGreaterThan, len, shadow);
        let bad = self.b.ins().bor(foreign, absent);
        let bad = self.b.ins().bor(bad, shadowed);
        self.miss_if(bad, miss);
        let off = self.b.ins().ishl_imm(idx, 4);
        let at = self.b.ins().iadd(block, off);
        let at = self.b.ins().iadd_imm(at, i64::from(l.split_values));
        // An `int` field, plus the increment without overflow.
        let tag = self.b.ins().uload8(types::I32, t, at, 0);
        let not_int = self
            .b
            .ins()
            .icmp_imm(IntCC::NotEqual, tag, i64::from(l.tag_int));
        self.miss_if(not_int, miss);
        let old = self.b.ins().load(types::I64, t, at, 8);
        let inc = match arg {
            Some(a) => a,
            None => self.b.ins().load(types::I64, t, e, l.method_upd_inc),
        };
        let (new, overflow) = self.checked_add(old, inc);
        self.miss_if(overflow, miss);
        self.b.ins().store(t, new, at, 8);
        new
    }

    /// RFC 0073 WS3 — burned-in native `str`-method call via the
    /// registered `wpjit_str_method` helper (the [`Self::emit_call_method`]
    /// discipline, sharing its signature). The helper receives the
    /// burned [`crate::ir::StrMethod`] discriminant in the token
    /// parameter; the receiver pin pops natively; the `argc`
    /// lane-typed arguments ride the marshal buffer with per-slot
    /// tags. Status: 0 = result in `ret_bits` on the expected lane,
    /// 1 = raised, 2 = lane-surprise deopt *after* the call (the
    /// parked result rides the interpreter stack), else = reject
    /// (deopt re-executing the `CALL` — pure `str` methods make the
    /// re-execution exact).
    fn emit_call_str_method(&mut self, site: u32, argc: u8, ret: JitType, pc: u32) {
        let trusted = MemFlags::trusted();
        let method = self.tfunc.str_method_sites[site as usize];
        let n = argc as usize;
        // Snapshot *including* receiver + args: the Reject exit re-runs
        // the CALL in the interpreter.
        let snapshot_full = self.vstack.clone();
        let base = self.vstack.len() - n;
        for (j, &(v, ty)) in self.vstack[base..].iter().enumerate() {
            let voff = (j as i32) * 8;
            let toff = (j as i32) * 4;
            self.b.ins().store(trusted, v, self.call_args_base, voff);
            let tagv = self.b.ins().iconst(types::I32, Self::tag(ty));
            self.b.ins().store(trusted, tagv, self.call_tags_base, toff);
        }
        self.vstack.truncate(base);
        let (pin, _) = self.pop();
        let snapshot = self.vstack.clone();

        self.writeback_locals();

        let sig = self.call_method_helper_sig();
        let helper = self
            .b
            .ins()
            .iconst(self.ptr_ty, runtime::str_method_helper_addr() as i64);
        let methodv = self.b.ins().iconst(types::I32, i64::from(method as u32));
        let argcv = self.b.ins().iconst(types::I32, i64::from(argc));
        let expectv = self.b.ins().iconst(types::I32, Self::tag(ret));
        let call = self.b.ins().call_indirect(
            sig,
            helper,
            &[self.frame_ptr, methodv, pin, argcv, expectv],
        );
        let status = self.b.inst_results(call)[0];

        let ok_b = self.b.create_block();
        let bad_b = self.b.create_block();
        let is_ok = self.b.ins().icmp_imm(IntCC::Equal, status, 0);
        self.b.ins().brif(is_ok, ok_b, &[], bad_b, &[]);

        self.b.switch_to_block(bad_b);
        let raised_b = self.b.create_block();
        let not_raised_b = self.b.create_block();
        let is_raised = self.b.ins().icmp_imm(IntCC::Equal, status, 1);
        self.b
            .ins()
            .brif(is_raised, raised_b, &[], not_raised_b, &[]);
        self.b.switch_to_block(raised_b);
        self.emit_exit(pc, &snapshot, JitStatus::Raised);
        self.b.switch_to_block(not_raised_b);
        let boxed_b = self.b.create_block();
        let reject_b = self.b.create_block();
        let is_boxed = self.b.ins().icmp_imm(IntCC::Equal, status, 2);
        self.b.ins().brif(is_boxed, boxed_b, &[], reject_b, &[]);
        self.b.switch_to_block(boxed_b);
        self.emit_exit(pc + 1, &snapshot, JitStatus::Deopt);
        self.b.switch_to_block(reject_b);
        self.emit_exit(pc, &snapshot_full, JitStatus::Deopt);

        self.b.switch_to_block(ok_b);
        let res = self
            .b
            .ins()
            .load(Self::cl_ty(ret), trusted, self.frame_ptr, OFF_RET_BITS);
        self.vstack.push((res, ret));
    }

    /// The imported signature of the `wpjit_call_method` helper
    /// (RFC 0069 WS1, lazy).
    fn call_method_helper_sig(&mut self) -> SigRef {
        if let Some(sig) = self.call_method_sig {
            return sig;
        }
        let mut sig = Signature::new(self.b.func.signature.call_conv);
        sig.params.push(AbiParam::new(self.ptr_ty)); // frame
        sig.params.push(AbiParam::new(types::I32)); // token
        sig.params.push(AbiParam::new(types::I64)); // receiver pin
        sig.params.push(AbiParam::new(types::I32)); // argc
        sig.params.push(AbiParam::new(types::I32)); // expect_tag
        sig.returns.push(AbiParam::new(types::I64)); // CallStatus
        let r = self.b.import_signature(sig);
        self.call_method_sig = Some(r);
        r
    }

    /// RFC 0065 WS5 — pinned-list length via `wpjit_list_len`. The
    /// helper returns the length directly (never negative in a correct
    /// build); a negative return is a defensive pin-table miss that
    /// deopts at the `CALL` pc with the pin spilled, where the
    /// enclosing `len` span rebuilds the interpreter's
    /// `[len, list]` stack shape.
    fn emit_list_len(&mut self, pc: u32) {
        let snapshot = self.vstack.clone();
        let (pin, _) = self.pop();
        let sig = self.pin_helper_sig();
        let helper = self
            .b
            .ins()
            .iconst(self.ptr_ty, runtime::list_len_helper_addr() as i64);
        let call = self
            .b
            .ins()
            .call_indirect(sig, helper, &[self.frame_ptr, pin]);
        let len = self.b.inst_results(call)[0];
        let bad = self.b.ins().icmp_imm(IntCC::SignedLessThan, len, 0);
        let cont = self.guard(bad, pc, &snapshot);
        self.b.switch_to_block(cont);
        self.vstack.push((len, JitType::Int));
    }

    /// RFC 0065 WS5 — pinned-list append via `wpjit_list_append`. The
    /// value is staged through `ret_bits` and `ret_tag`, allowing a
    /// generic list to box native scalars. A non-zero status deopts at the `CALL` pc,
    /// where the enclosing method span rebuilds the receiver as the
    /// bound `list.append` and the interpreter re-executes the call.
    fn emit_list_append(&mut self, pc: u32) {
        let trusted = MemFlags::trusted();
        let snapshot = self.vstack.clone();
        let (val, ty) = self.pop();
        let (pin, _) = self.pop();
        let appended = self.inline_list_append(pin, val, ty);
        self.b
            .ins()
            .store(trusted, val, self.frame_ptr, OFF_RET_BITS);
        let tag = self.b.ins().iconst(types::I32, Self::tag(ty));
        self.b
            .ins()
            .store(trusted, tag, self.frame_ptr, OFF_RET_TAG);
        let sig = self.pin_helper_sig();
        let helper = self
            .b
            .ins()
            .iconst(self.ptr_ty, runtime::list_append_helper_addr() as i64);
        let call = self
            .b
            .ins()
            .call_indirect(sig, helper, &[self.frame_ptr, pin]);
        let status = self.b.inst_results(call)[0];
        let bad = self.b.ins().icmp_imm(IntCC::NotEqual, status, 0);
        let cont = self.guard(bad, pc, &snapshot);
        self.b.switch_to_block(cont);
        if let Some(done) = appended {
            self.b.ins().jump(done, &[]);
            self.b.switch_to_block(done);
        }
        // `append` returns `None`, which the following `POP_TOP`
        // consumes — neither ever exists on the native stack.
    }

    /// `append` of a scalar to a pinned list with room for it, in line;
    /// the block to continue at, with the current block the helper's
    /// path (`None` when not in line).
    fn inline_list_append(&mut self, pin: Value, val: Value, ty: JitType) -> Option<Block> {
        let l = *runtime::obj_layout()?;
        // The lanes the helper boxes this scalar into.
        let lanes: &[JitType] = match ty {
            JitType::Int => &[JitType::Int, JitType::Obj],
            JitType::Float => &[JitType::Float, JitType::Obj],
            JitType::Bool => &[JitType::Obj],
            _ => return None,
        };
        let t = MemFlags::trusted();
        let miss = self.b.create_block();
        let done = self.b.create_block();
        let (items, len, list) = self.pinned_list(&l, pin, lanes, miss, false);
        let cap = self.b.ins().load(types::I64, t, list, l.list_cap);
        let full = self
            .b
            .ins()
            .icmp(IntCC::UnsignedGreaterThanOrEqual, len, cap);
        self.miss_if(full, miss);
        let off = self.b.ins().ishl_imm(len, 4);
        let at = self.b.ins().iadd(items, off);
        let (tag, voff, _) = Self::lane_tag(&l, ty).expect("scalar lane");
        let v = if ty == JitType::Bool {
            self.b.ins().ireduce(types::I8, val)
        } else {
            val
        };
        let tagv = self.b.ins().iconst(types::I8, i64::from(tag));
        self.b.ins().store(t, tagv, at, 0);
        self.b.ins().store(t, v, at, voff);
        let len1 = self.b.ins().iadd_imm(len, 1);
        self.b.ins().store(t, len1, list, l.list_len);
        self.b.ins().jump(done, &[]);
        self.b.switch_to_block(miss);
        Some(done)
    }

    /// RFC 0073 WS1 — `LIST_APPEND` inside an inlined comprehension:
    /// identical to [`Self::emit_list_append`] except the accumulator
    /// pin stays on the native stack (it is live across the whole
    /// loop, consumed only after the loop exits).
    fn emit_list_append_keep(&mut self, pc: u32) {
        let trusted = MemFlags::trusted();
        let snapshot = self.vstack.clone();
        let (val, ty) = self.pop();
        let &(pin, _) = self.vstack.last().expect("comp append on empty stack");
        self.b
            .ins()
            .store(trusted, val, self.frame_ptr, OFF_RET_BITS);
        let tag = self.b.ins().iconst(types::I32, Self::tag(ty));
        self.b
            .ins()
            .store(trusted, tag, self.frame_ptr, OFF_RET_TAG);
        let sig = self.pin_helper_sig();
        let helper = self
            .b
            .ins()
            .iconst(self.ptr_ty, runtime::list_append_helper_addr() as i64);
        let call = self
            .b
            .ins()
            .call_indirect(sig, helper, &[self.frame_ptr, pin]);
        let status = self.b.inst_results(call)[0];
        let bad = self.b.ins().icmp_imm(IntCC::NotEqual, status, 0);
        let cont = self.guard(bad, pc, &snapshot);
        self.b.switch_to_block(cont);
    }

    /// RFC 0065 WS5 — pinned-instance attribute read via
    /// `wpjit_attr_get`. A non-zero status deopts at this pc with the
    /// receiver spilled (its `ObjPin` tag rebuilds the real instance),
    /// so the interpreter re-executes the `LOAD_ATTR` generically.
    fn emit_attr_get(&mut self, site: u32, out: JitType, pc: u32) {
        let trusted = MemFlags::trusted();
        let snapshot = self.vstack.clone();
        let (pin, _) = self.pop();
        // A scalar field of a split-layout instance, read in line; any
        // other shape takes the helper below.
        let inline = match (runtime::obj_layout(), out) {
            (Some(l), JitType::Int | JitType::Float | JitType::Bool) => Some(*l),
            _ => None,
        };
        let mut join = None;
        if let Some(l) = inline {
            let miss = self.b.create_block();
            let done = self.b.create_block();
            self.b.append_block_param(done, Self::cl_ty(out));
            let addr = self.split_field_addr(&l, pin, site, miss, true);
            let (want, off, ty) = match out {
                JitType::Int => (l.tag_int, 8, types::I64),
                JitType::Float => (l.tag_float, 8, types::F64),
                _ => (l.tag_bool, 1, types::I8),
            };
            let tag = self.b.ins().uload8(types::I32, trusted, addr, 0);
            let other = self.b.ins().icmp_imm(IntCC::NotEqual, tag, i64::from(want));
            self.miss_if(other, miss);
            let v = self.b.ins().load(ty, trusted, addr, off);
            let v = if ty == types::I8 {
                self.b.ins().uextend(types::I64, v)
            } else {
                v
            };
            self.b.ins().jump(done, &[BlockArg::from(v)]);
            self.b.switch_to_block(miss);
            join = Some(done);
        }
        let sig = self.list_helper_sig();
        let helper = self
            .b
            .ins()
            .iconst(self.ptr_ty, runtime::attr_get_helper_addr() as i64);
        let sitev = self.b.ins().iconst(types::I64, i64::from(site));
        let call = self
            .b
            .ins()
            .call_indirect(sig, helper, &[self.frame_ptr, pin, sitev]);
        let status = self.b.inst_results(call)[0];
        let bad = self.b.ins().icmp_imm(IntCC::NotEqual, status, 0);
        let cont = self.guard(bad, pc, &snapshot);
        self.b.switch_to_block(cont);
        let res = self
            .b
            .ins()
            .load(Self::cl_ty(out), trusted, self.frame_ptr, OFF_RET_BITS);
        let res = match join {
            Some(done) => {
                self.b.ins().jump(done, &[BlockArg::from(res)]);
                self.b.switch_to_block(done);
                self.b.block_params(done)[0]
            }
            None => res,
        };
        self.vstack.push((res, out));
    }

    /// Branch to `miss` when `cond`; continue in a fresh block otherwise.
    fn miss_if(&mut self, cond: Value, miss: Block) {
        let ok = self.b.create_block();
        self.b.ins().brif(cond, miss, &[], ok, &[]);
        self.b.switch_to_block(ok);
    }

    /// The address of the field the attribute guard of `site` names in the
    /// pinned instance `pin`'s split values (see [`runtime::ObjLayout`]):
    /// an instance pin whose class still has the guard's version, whose
    /// values are split over its class's names, unborrowed (`read`) or
    /// unborrowed and exclusive (a write), and hold the guard's index; or,
    /// for a site on a member slot (see [`crate::ir::AttrSiteMeta::slot_member`]),
    /// whose slots are laid out over the guard's layout, under the same
    /// borrow rule (the slot may be unset). Anything else branches to
    /// `miss`.
    fn split_field_addr(
        &mut self,
        l: &runtime::ObjLayout,
        pin: Value,
        site: u32,
        miss: Block,
        read: bool,
    ) -> Value {
        let t = MemFlags::trusted();
        let ptr = self.ptr_ty;
        let slot_member = l.slots_ok
            && self
                .tfunc
                .attr_sites
                .get(site as usize)
                .is_some_and(|s| s.slot_member);
        let ctx = self.b.ins().load(ptr, t, self.frame_ptr, OFF_CTX);
        // The guard first: a site whose field has no position of this kind
        // leaves straight away.
        let guards = self.b.ins().load(ptr, t, ctx, l.ctx_guards);
        let gbuf = self.b.ins().load(ptr, t, guards, l.guards_buf);
        let g = self
            .b
            .ins()
            .iadd_imm(gbuf, i64::from(site) * i64::from(l.guard_size));
        let idx = self.b.ins().uload32(t, g, l.guard_split_idx);
        let none = if slot_member {
            let kind = self.b.ins().band_imm(idx, i64::from(runtime::SLOT_FIELD));
            self.b.ins().icmp_imm(IntCC::Equal, kind, 0)
        } else {
            self.b
                .ins()
                .icmp_imm(IntCC::Equal, idx, i64::from(u32::MAX))
        };
        // The pin.
        let n = self.b.ins().load(types::I64, t, ctx, l.ctx_pins_len);
        let out = self.b.ins().icmp(IntCC::UnsignedGreaterThanOrEqual, pin, n);
        let bad = self.b.ins().bor(none, out);
        self.miss_if(bad, miss);
        let buf = self.b.ins().load(ptr, t, ctx, l.ctx_pins_ptr);
        let off = self.b.ins().imul_imm(pin, i64::from(l.pin_size));
        let p = self.b.ins().iadd(buf, off);
        let ptag = self.b.ins().uload8(types::I32, t, p, l.pin_tag);
        let otag = self.b.ins().uload8(types::I32, t, p, l.pin_obj);
        let not_obj = self
            .b
            .ins()
            .icmp_imm(IntCC::NotEqual, ptag, i64::from(l.pin_obj_tag));
        let not_inst = self
            .b
            .ins()
            .icmp_imm(IntCC::NotEqual, otag, i64::from(l.tag_instance));
        let bad = self.b.ins().bor(not_obj, not_inst);
        self.miss_if(bad, miss);
        let inst = self.b.ins().load(ptr, t, p, l.pin_obj + 8);
        let ver = self.b.ins().load(types::I64, t, g, l.guard_ver);
        let cls = self.b.ins().load(ptr, t, inst, l.inst_class);
        let cver = self.b.ins().load(types::I64, t, cls, l.type_attr_version);
        let flag = self.b.ins().iconst(ptr, l.cells_unguarded as i64);
        let shared = self.b.ins().uload8(types::I64, t, flag, 0);
        let stale = self.b.ins().icmp(IntCC::NotEqual, cver, ver);
        if slot_member {
            // The member slot at its position in the guard's layout.
            let shared = self.b.ins().icmp_imm(IntCC::NotEqual, shared, 0);
            let bad = self.b.ins().bor(stale, shared);
            self.miss_if(bad, miss);
            let layout = self.b.ins().load(ptr, t, g, l.guard_slot_layout);
            let (vals, bad) = self.laid_out_slots(l, inst, layout, read);
            self.miss_if(bad, miss);
            let i = self.b.ins().band_imm(idx, i64::from(!runtime::SLOT_FIELD));
            let off = self.b.ins().ishl_imm(i, 4);
            return self.b.ins().iadd(vals, off);
        }
        // The split values (grouped by what each group's loads need proven:
        // fewer blocks compile faster).
        let lazy = self.b.ins().load(ptr, t, inst, l.inst_dict_lazy);
        let borrow = self.b.ins().sload32(t, inst, l.inst_split_borrow);
        let block = self.b.ins().load(ptr, t, inst, l.inst_split_block);
        let busy = if read {
            self.b.ins().icmp_imm(IntCC::SignedLessThan, borrow, 0)
        } else {
            self.b.ins().icmp_imm(IntCC::NotEqual, borrow, 0)
        };
        let empty = self.b.ins().icmp_imm(IntCC::Equal, block, 0);
        let other = self.b.ins().bor(lazy, shared);
        let other = self.b.ins().icmp_imm(IntCC::NotEqual, other, 0);
        let bad = self.b.ins().bor(stale, busy);
        let bad = self.b.ins().bor(bad, empty);
        let bad = self.b.ins().bor(bad, other);
        self.miss_if(bad, miss);
        let keys = self.b.ins().load(ptr, t, block, l.split_keys);
        let ckeys = self.b.ins().load(ptr, t, cls, l.type_shared_keys);
        let len = self.b.ins().uload32(t, block, l.split_len);
        let foreign = self.b.ins().icmp(IntCC::NotEqual, keys, ckeys);
        let absent = self
            .b
            .ins()
            .icmp(IntCC::UnsignedGreaterThanOrEqual, idx, len);
        let bad = self.b.ins().bor(foreign, absent);
        self.miss_if(bad, miss);
        let off = self.b.ins().ishl_imm(idx, 4);
        let at = self.b.ins().iadd(block, off);
        self.b.ins().iadd_imm(at, i64::from(l.split_values))
    }

    /// The address the new-key store at `site` appends its field to, in
    /// the pinned instance `pin`'s split values (see
    /// [`runtime::ObjLayout`]), having counted it in: an ordinary instance
    /// pin whose class still has the guard's version, whose values are
    /// split over its class's names, unborrowed and exclusive, hold exactly
    /// the names before the guard's index and have room for one more, with
    /// no dict watcher active. Anything else branches to `miss` before
    /// anything is written.
    fn split_append_addr(
        &mut self,
        l: &runtime::ObjLayout,
        pin: Value,
        site: u32,
        miss: Block,
    ) -> Value {
        let t = MemFlags::trusted();
        let ptr = self.ptr_ty;
        let ctx = self.b.ins().load(ptr, t, self.frame_ptr, OFF_CTX);
        let guards = self.b.ins().load(ptr, t, ctx, l.ctx_guards);
        let gbuf = self.b.ins().load(ptr, t, guards, l.guards_buf);
        let g = self
            .b
            .ins()
            .iadd_imm(gbuf, i64::from(site) * i64::from(l.guard_size));
        let idx = self.b.ins().uload32(t, g, l.guard_split_idx);
        let none = self
            .b
            .ins()
            .icmp_imm(IntCC::Equal, idx, i64::from(u32::MAX));
        let n = self.b.ins().load(types::I64, t, ctx, l.ctx_pins_len);
        let out = self.b.ins().icmp(IntCC::UnsignedGreaterThanOrEqual, pin, n);
        let bad = self.b.ins().bor(none, out);
        self.miss_if(bad, miss);
        let buf = self.b.ins().load(ptr, t, ctx, l.ctx_pins_ptr);
        let off = self.b.ins().imul_imm(pin, i64::from(l.pin_size));
        let p = self.b.ins().iadd(buf, off);
        let ptag = self.b.ins().uload8(types::I32, t, p, l.pin_tag);
        let otag = self.b.ins().uload8(types::I32, t, p, l.pin_obj);
        let not_obj = self
            .b
            .ins()
            .icmp_imm(IntCC::NotEqual, ptag, i64::from(l.pin_obj_tag));
        let not_inst = self
            .b
            .ins()
            .icmp_imm(IntCC::NotEqual, otag, i64::from(l.tag_instance));
        let bad = self.b.ins().bor(not_obj, not_inst);
        self.miss_if(bad, miss);
        let inst = self.b.ins().load(ptr, t, p, l.pin_obj + 8);
        let ver = self.b.ins().load(types::I64, t, g, l.guard_ver);
        let cls = self.b.ins().load(ptr, t, inst, l.inst_class);
        let cver = self.b.ins().load(types::I64, t, cls, l.type_attr_version);
        let lazy = self.b.ins().load(ptr, t, inst, l.inst_dict_lazy);
        let body = self.b.ins().load(ptr, t, inst, l.inst_c_body);
        let flag = self.b.ins().iconst(ptr, l.cells_unguarded as i64);
        let shared = self.b.ins().uload8(types::I64, t, flag, 0);
        let watchers = self.b.ins().iconst(ptr, l.dict_watchers as i64);
        let watchers = self.b.ins().uload8(types::I64, t, watchers, 0);
        let borrow = self.b.ins().sload32(t, inst, l.inst_split_borrow);
        let block = self.b.ins().load(ptr, t, inst, l.inst_split_block);
        let stale = self.b.ins().icmp(IntCC::NotEqual, cver, ver);
        let busy = self.b.ins().icmp_imm(IntCC::NotEqual, borrow, 0);
        let empty = self.b.ins().icmp_imm(IntCC::Equal, block, 0);
        let other = self.b.ins().bor(lazy, body);
        let other = self.b.ins().bor(other, shared);
        let other = self.b.ins().bor(other, watchers);
        let other = self.b.ins().icmp_imm(IntCC::NotEqual, other, 0);
        let bad = self.b.ins().bor(stale, busy);
        let bad = self.b.ins().bor(bad, empty);
        let bad = self.b.ins().bor(bad, other);
        self.miss_if(bad, miss);
        let keys = self.b.ins().load(ptr, t, block, l.split_keys);
        let ckeys = self.b.ins().load(ptr, t, cls, l.type_shared_keys);
        let len = self.b.ins().uload32(t, block, l.split_len);
        let cap = self.b.ins().uload32(t, block, l.split_cap);
        let foreign = self.b.ins().icmp(IntCC::NotEqual, keys, ckeys);
        let elsewhere = self.b.ins().icmp(IntCC::NotEqual, idx, len);
        let full = self
            .b
            .ins()
            .icmp(IntCC::UnsignedGreaterThanOrEqual, len, cap);
        let bad = self.b.ins().bor(foreign, elsewhere);
        let bad = self.b.ins().bor(bad, full);
        self.miss_if(bad, miss);
        let len1 = self.b.ins().iadd_imm(len, 1);
        let len1 = self.b.ins().ireduce(types::I32, len1);
        self.b.ins().store(t, len1, block, l.split_len);
        let off = self.b.ins().ishl_imm(idx, 4);
        let at = self.b.ins().iadd(block, off);
        self.b.ins().iadd_imm(at, i64::from(l.split_values))
    }

    /// The pinned list `pin`'s items (buffer pointer and length; and its
    /// capacity, and where the length lives, for an append): a list pin
    /// whose element lane is one of `lanes`, unborrowed (`read`) or
    /// unborrowed and exclusive (a write). Anything else branches to
    /// `miss`.
    fn pinned_list(
        &mut self,
        l: &runtime::ObjLayout,
        pin: Value,
        lanes: &[JitType],
        miss: Block,
        read: bool,
    ) -> (Value, Value, Value) {
        let t = MemFlags::trusted();
        let ptr = self.ptr_ty;
        let ctx = self.b.ins().load(ptr, t, self.frame_ptr, OFF_CTX);
        let n = self.b.ins().load(types::I64, t, ctx, l.ctx_pins_len);
        let out = self.b.ins().icmp(IntCC::UnsignedGreaterThanOrEqual, pin, n);
        self.miss_if(out, miss);
        let buf = self.b.ins().load(ptr, t, ctx, l.ctx_pins_ptr);
        let off = self.b.ins().imul_imm(pin, i64::from(l.pin_size));
        let p = self.b.ins().iadd(buf, off);
        let ptag = self.b.ins().uload8(types::I32, t, p, l.pin_tag);
        let lane = self.b.ins().uload8(types::I32, t, p, l.pin_list_elem);
        let mut bad = self
            .b
            .ins()
            .icmp_imm(IntCC::NotEqual, ptag, i64::from(l.pin_list_tag));
        let mut ok_lane = self.b.ins().iconst(types::I8, 0);
        for &want in lanes {
            let hit = self.b.ins().icmp_imm(IntCC::Equal, lane, want as i64);
            ok_lane = self.b.ins().bor(ok_lane, hit);
        }
        let wrong_lane = self.b.ins().bxor_imm(ok_lane, 1);
        bad = self.b.ins().bor(bad, wrong_lane);
        self.miss_if(bad, miss);
        let list = self.b.ins().load(ptr, t, p, l.pin_list);
        let flag = self.b.ins().iconst(ptr, l.cells_unguarded as i64);
        let shared = self.b.ins().uload8(types::I32, t, flag, 0);
        let borrow = self.b.ins().sload32(t, list, l.list_borrow);
        let busy = if read {
            self.b.ins().icmp_imm(IntCC::SignedLessThan, borrow, 0)
        } else {
            self.b.ins().icmp_imm(IntCC::NotEqual, borrow, 0)
        };
        let shared = self.b.ins().icmp_imm(IntCC::NotEqual, shared, 0);
        let bad = self.b.ins().bor(busy, shared);
        self.miss_if(bad, miss);
        let items = self.b.ins().load(ptr, t, list, l.list_ptr);
        let len = self.b.ins().load(types::I64, t, list, l.list_len);
        (items, len, list)
    }

    /// The element address for a (possibly negative) in-range `idx` of
    /// `len` items at `items`; out of range branches to `miss`.
    fn list_elem(&mut self, items: Value, len: Value, idx: Value, miss: Block) -> Value {
        let neg = self.b.ins().icmp_imm(IntCC::SignedLessThan, idx, 0);
        let wrapped = self.b.ins().iadd(idx, len);
        let i = self.b.ins().select(neg, wrapped, idx);
        let out = self.b.ins().icmp(IntCC::UnsignedGreaterThanOrEqual, i, len);
        self.miss_if(out, miss);
        let off = self.b.ins().ishl_imm(i, 4);
        self.b.ins().iadd(items, off)
    }

    /// The object tag of a scalar lane.
    fn lane_tag(l: &runtime::ObjLayout, lane: JitType) -> Option<(u8, i32, Type)> {
        match lane {
            JitType::Int => Some((l.tag_int, 8, types::I64)),
            JitType::Float => Some((l.tag_float, 8, types::F64)),
            JitType::Bool => Some((l.tag_bool, 1, types::I8)),
            _ => None,
        }
    }

    /// Consecutive callback-free reads can replay from the first read on a
    /// later miss. Preserve its original receiver in the deopt snapshot.
    fn emit_attr_get_chain(
        &mut self,
        address: usize,
        site: u32,
        count: u32,
        out: JitType,
        pc: u32,
    ) {
        let snapshot = self.vstack.clone();
        let (pin, _) = self.pop();
        let sig = self.quad_helper_sig();
        let helper = self.b.ins().iconst(self.ptr_ty, address as i64);
        let sitev = self.b.ins().iconst(types::I64, i64::from(site));
        let countv = self.b.ins().iconst(types::I64, i64::from(count));
        let call = self
            .b
            .ins()
            .call_indirect(sig, helper, &[self.frame_ptr, pin, sitev, countv]);
        let status = self.b.inst_results(call)[0];
        let bad = self.b.ins().icmp_imm(IntCC::NotEqual, status, 0);
        let cont = self.guard(bad, pc, &snapshot);
        self.b.switch_to_block(cont);
        let res = self.b.ins().load(
            Self::cl_ty(out),
            MemFlags::trusted(),
            self.frame_ptr,
            OFF_RET_BITS,
        );
        self.vstack.push((res, out));
    }

    /// Try a callback-free borrowed walk, then join with the original native
    /// sequence on a cache miss. Each ordinary helper keeps its own exit PC.
    fn emit_cached_attr_chain(
        &mut self,
        address: usize,
        first_site: u32,
        guarded: u32,
        total: u32,
        int_result: bool,
        original: &[TStmt],
    ) {
        let snapshot = self.vstack.clone();
        let (pin, _) = self.pop();
        let pc = original[0].pc;
        self.store_call_site_pc(pc);
        let mut signature = Signature::new(self.b.func.signature.call_conv);
        signature.params.push(AbiParam::new(self.ptr_ty));
        for _ in 0..5 {
            signature.params.push(AbiParam::new(types::I64));
        }
        signature.returns.push(AbiParam::new(types::I64));
        let sig = self.b.import_signature(signature);
        let helper = self.b.ins().iconst(self.ptr_ty, address as i64);
        let firstv = self.b.ins().iconst(types::I64, i64::from(first_site));
        let guardedv = self.b.ins().iconst(types::I64, i64::from(guarded));
        let totalv = self.b.ins().iconst(types::I64, i64::from(total));
        let integerv = self.b.ins().iconst(types::I64, i64::from(int_result));
        let call = self.b.ins().call_indirect(
            sig,
            helper,
            &[self.frame_ptr, pin, firstv, guardedv, totalv, integerv],
        );
        let status = self.b.inst_results(call)[0];
        let success = self.b.create_block();
        let ordinary = self.b.create_block();
        let join = self.b.create_block();
        self.b.append_block_param(join, types::I64);
        let ok = self.b.ins().icmp_imm(IntCC::Equal, status, 0);
        self.b.ins().brif(ok, success, &[], ordinary, &[]);

        self.b.switch_to_block(success);
        let result = self.b.ins().load(
            types::I64,
            MemFlags::trusted(),
            self.frame_ptr,
            OFF_RET_BITS,
        );
        self.b.ins().jump(join, &[BlockArg::from(result)]);

        self.b.switch_to_block(ordinary);
        self.vstack = snapshot.clone();
        let start = if guarded != 0 {
            let ready = self.b.create_block();
            let read = self.b.create_block();
            let prefix_join = self.b.create_block();
            self.b.append_block_param(prefix_join, types::I64);
            let prefix_done = self.b.ins().icmp_imm(IntCC::Equal, status, 2);
            self.b.ins().brif(prefix_done, ready, &[], read, &[]);

            self.b.switch_to_block(ready);
            let prefix = self.b.ins().load(
                types::I64,
                MemFlags::trusted(),
                self.frame_ptr,
                OFF_RET_BITS,
            );
            self.b.ins().jump(prefix_join, &[BlockArg::from(prefix)]);

            self.b.switch_to_block(read);
            // A prefix guard miss still uses the original read and snapshot.
            // Keep its existing fusion when the optional helper is available.
            let known = runtime::attr_get_chain_helper_addr();
            if guarded >= 2 && known != 0 {
                self.emit_attr_get_chain(known, first_site, guarded, JitType::Obj, pc);
            } else {
                for &stmt in &original[..guarded as usize] {
                    self.emit_stmt(stmt);
                }
            }
            let (prefix, out) = self.pop();
            debug_assert_eq!(out, JitType::Obj);
            self.b.ins().jump(prefix_join, &[BlockArg::from(prefix)]);

            self.b.switch_to_block(prefix_join);
            self.vstack = snapshot.clone();
            self.vstack.pop();
            let prefix = self.b.block_params(prefix_join)[0];
            self.vstack.push((prefix, JitType::Obj));
            guarded as usize
        } else {
            0
        };
        for &stmt in &original[start..] {
            self.emit_stmt(stmt);
        }
        let (result, out) = self.pop();
        let expected = if int_result {
            JitType::Int
        } else {
            JitType::Obj
        };
        debug_assert_eq!(out, expected);
        self.b.ins().jump(join, &[BlockArg::from(result)]);

        self.b.switch_to_block(join);
        self.vstack = snapshot;
        self.vstack.pop();
        let result = self.b.block_params(join)[0];
        self.vstack.push((result, expected));
    }

    /// RFC 0065 WS5 — pinned-instance attribute write via
    /// `wpjit_attr_set`. The value is staged through `ret_bits`; a
    /// non-zero status deopts at this pc with `[value, receiver]`
    /// spilled, so the interpreter re-executes the `STORE_ATTR`.
    fn emit_attr_set(&mut self, site: u32, pc: u32) {
        let trusted = MemFlags::trusted();
        let snapshot = self.vstack.clone();
        let (pin, _) = self.pop();
        let (val, lane) = self.pop();
        // A scalar over a scalar field of a split-layout instance, stored
        // in line (no release, no collector bookkeeping); any other shape
        // takes the helper below.
        let inline = match (runtime::obj_layout(), lane) {
            (Some(l), JitType::Int | JitType::Float | JitType::Bool) => Some(*l),
            _ => None,
        };
        let new_key = self
            .tfunc
            .attr_sites
            .get(site as usize)
            .is_some_and(|s| s.new_key);
        let mut done = None;
        if let (Some(l), true) = (inline, new_key) {
            // The constructor pattern: a fresh instance's next field.
            let miss = self.b.create_block();
            let stored = self.b.create_block();
            let addr = self.split_append_addr(&l, pin, site, miss);
            let (tag, off, v) = match lane {
                JitType::Int => (l.tag_int, 8, val),
                JitType::Float => (l.tag_float, 8, val),
                _ => (l.tag_bool, 1, self.b.ins().ireduce(types::I8, val)),
            };
            let tagv = self.b.ins().iconst(types::I8, i64::from(tag));
            self.b.ins().store(trusted, tagv, addr, 0);
            self.b.ins().store(trusted, v, addr, off);
            self.b.ins().jump(stored, &[]);
            self.b.switch_to_block(miss);
            done = Some(stored);
        } else if let Some(l) = inline {
            let miss = self.b.create_block();
            let stored = self.b.create_block();
            let addr = self.split_field_addr(&l, pin, site, miss, false);
            let old = self.b.ins().uload8(types::I64, trusted, addr, 0);
            let one = self.b.ins().iconst(types::I64, 1);
            let bit = self.b.ins().ishl(one, old);
            // (An unset member slot holds nothing to release either.)
            let scalars = (1i64 << l.tag_int)
                | (1i64 << l.tag_float)
                | (1i64 << l.tag_bool)
                | (1i64 << l.tag_none)
                | (1i64 << l.tag_unbound);
            let m = self.b.ins().band_imm(bit, scalars);
            let heap = self.b.ins().icmp_imm(IntCC::Equal, m, 0);
            self.miss_if(heap, miss);
            let (tag, off, v) = match lane {
                JitType::Int => (l.tag_int, 8, val),
                JitType::Float => (l.tag_float, 8, val),
                _ => (l.tag_bool, 1, self.b.ins().ireduce(types::I8, val)),
            };
            let tagv = self.b.ins().iconst(types::I8, i64::from(tag));
            self.b.ins().store(trusted, tagv, addr, 0);
            self.b.ins().store(trusted, v, addr, off);
            self.b.ins().jump(stored, &[]);
            self.b.switch_to_block(miss);
            done = Some(stored);
        }
        self.b
            .ins()
            .store(trusted, val, self.frame_ptr, OFF_RET_BITS);
        let sig = self.list_helper_sig();
        let helper = self
            .b
            .ins()
            .iconst(self.ptr_ty, runtime::attr_set_helper_addr() as i64);
        let sitev = self.b.ins().iconst(types::I64, i64::from(site));
        let call = self
            .b
            .ins()
            .call_indirect(sig, helper, &[self.frame_ptr, pin, sitev]);
        let status = self.b.inst_results(call)[0];
        let bad = self.b.ins().icmp_imm(IntCC::NotEqual, status, 0);
        let cont = self.guard(bad, pc, &snapshot);
        self.b.switch_to_block(cont);
        if let Some(stored) = done {
            self.b.ins().jump(stored, &[]);
            self.b.switch_to_block(stored);
        }
    }

    /// RFC 0076 WS6 — closure-cell read via `wpjit_cell_get`. The cell
    /// index is a compile-time constant (the frame's cell array is
    /// fixed for the activation's lifetime); the helper re-validates
    /// the burned lane per access and a non-zero status deopts at this
    /// pc, so the interpreter re-executes the `LOAD_DEREF` (raising
    /// the exact `NameError` for an unbound cell).
    fn emit_cell_get(&mut self, idx: u32, lane: JitType, pc: u32) {
        let trusted = MemFlags::trusted();
        let snapshot = self.vstack.clone();
        let sig = self.list_helper_sig();
        let helper = self
            .b
            .ins()
            .iconst(self.ptr_ty, runtime::cell_get_helper_addr() as i64);
        let idxv = self.b.ins().iconst(types::I64, i64::from(idx));
        let lanev = self.b.ins().iconst(
            types::I64,
            lane.cell_lane_code().expect("analyzer admits scalar lanes"),
        );
        let call = self
            .b
            .ins()
            .call_indirect(sig, helper, &[self.frame_ptr, idxv, lanev]);
        let status = self.b.inst_results(call)[0];
        let bad = self.b.ins().icmp_imm(IntCC::NotEqual, status, 0);
        let cont = self.guard(bad, pc, &snapshot);
        self.b.switch_to_block(cont);
        let res = self
            .b
            .ins()
            .load(Self::cl_ty(lane), trusted, self.frame_ptr, OFF_RET_BITS);
        self.vstack.push((res, lane));
    }

    /// RFC 0076 WS6 — closure-cell write via `wpjit_cell_set`. The
    /// value is staged through `ret_bits`; a non-zero status (displaced
    /// heap value, lane surprise) deopts at this pc with the value
    /// spilled, so the interpreter re-executes the `STORE_DEREF`.
    fn emit_cell_set(&mut self, idx: u32, lane: JitType, pc: u32) {
        let trusted = MemFlags::trusted();
        let snapshot = self.vstack.clone();
        let (val, _) = self.pop();
        self.b
            .ins()
            .store(trusted, val, self.frame_ptr, OFF_RET_BITS);
        let sig = self.list_helper_sig();
        let helper = self
            .b
            .ins()
            .iconst(self.ptr_ty, runtime::cell_set_helper_addr() as i64);
        let idxv = self.b.ins().iconst(types::I64, i64::from(idx));
        let lanev = self.b.ins().iconst(
            types::I64,
            lane.cell_lane_code().expect("analyzer admits scalar lanes"),
        );
        let call = self
            .b
            .ins()
            .call_indirect(sig, helper, &[self.frame_ptr, idxv, lanev]);
        let status = self.b.inst_results(call)[0];
        let bad = self.b.ins().icmp_imm(IntCC::NotEqual, status, 0);
        let cont = self.guard(bad, pc, &snapshot);
        self.b.switch_to_block(cont);
    }

    /// RFC 0067 WS2 — the loop-header poll: decrement the countdown;
    /// on expiry call the embedder's poll helper (which performs the
    /// GIL hand-off inline), reset the countdown, and take the
    /// standard deopt exit at the header pc iff the helper reports
    /// interpreter-required pending work. The boundary stack is empty
    /// at every header, so the deopt snapshot is empty and the
    /// embedder's range-loop metadata rebuilds any live iterators.
    fn emit_poll(&mut self, cd_var: Variable, header_pc: u32) {
        let cd = self.b.use_var(cd_var);
        let next = self.b.ins().iadd_imm(cd, -1);
        self.b.def_var(cd_var, next);
        let poll_b = self.b.create_block();
        let cont_b = self.b.create_block();
        let expired = self.b.ins().icmp_imm(IntCC::Equal, next, 0);
        self.b.ins().brif(expired, poll_b, &[], cont_b, &[]);

        self.b.switch_to_block(poll_b);
        let seed = self.b.ins().iconst(types::I64, runtime::JIT_POLL_STRIDE);
        self.b.def_var(cd_var, seed);
        let sig = self.poll_helper_sig();
        let helper = self
            .b
            .ins()
            .iconst(self.ptr_ty, runtime::poll_helper_addr() as i64);
        let call = self.b.ins().call_indirect(sig, helper, &[self.frame_ptr]);
        let pending = self.b.inst_results(call)[0];
        let exit_b = self.b.create_block();
        let has_work = self.b.ins().icmp_imm(IntCC::NotEqual, pending, 0);
        self.b.ins().brif(has_work, exit_b, &[], cont_b, &[]);
        self.b.switch_to_block(exit_b);
        // RFC 0073 WS1 — spill the live entry stack (empty for OSR
        // headers; the accumulator for comprehension headers).
        let snapshot = self.vstack.clone();
        self.emit_exit(header_pc, &snapshot, JitStatus::Deopt);

        self.b.switch_to_block(cont_b);
    }

    /// The imported `(frame) -> i64` signature of the eval-breaker
    /// poll helper (RFC 0067 WS2, lazy).
    fn poll_helper_sig(&mut self) -> SigRef {
        if let Some(sig) = self.poll_sig {
            return sig;
        }
        let mut sig = Signature::new(self.b.func.signature.call_conv);
        sig.params.push(AbiParam::new(self.ptr_ty)); // frame
        sig.returns.push(AbiParam::new(types::I64)); // pending?
        let r = self.b.import_signature(sig);
        self.poll_sig = Some(r);
        r
    }

    /// The shared `(frame, pin) -> i64` signature of the pinned-list
    /// length/append helpers (RFC 0065 WS5, lazy).
    fn pin_helper_sig(&mut self) -> SigRef {
        if let Some(sig) = self.pin_sig {
            return sig;
        }
        let mut sig = Signature::new(self.b.func.signature.call_conv);
        sig.params.push(AbiParam::new(self.ptr_ty)); // frame
        sig.params.push(AbiParam::new(types::I64)); // pin
        sig.returns.push(AbiParam::new(types::I64)); // len / status
        let r = self.b.import_signature(sig);
        self.pin_sig = Some(r);
        r
    }

    /// RFC 0061 WS5 — pinned-list element read via the registered
    /// `wpjit_list_get` helper. A non-zero status deopts at this pc
    /// with both operands spilled (the pin reference rebuilds into the
    /// real list object through its [`SlotTag::ListPin`] tag), so the
    /// interpreter re-executes the subscript — and raises the exact
    /// `IndexError`/`TypeError` itself when warranted.
    fn emit_list_get(&mut self, elem: JitType, pc: u32) {
        let trusted = MemFlags::trusted();
        let snapshot = self.vstack.clone();
        let (idx, _) = self.pop();
        let (pin, _) = self.pop();
        // A scalar element of a scalar-lane list, read in line.
        let mut join = None;
        if let (Some(l), JitType::Int | JitType::Float) = (runtime::obj_layout(), elem) {
            let l = *l;
            let miss = self.b.create_block();
            let done = self.b.create_block();
            self.b.append_block_param(done, Self::cl_ty(elem));
            let (items, len, _) = self.pinned_list(&l, pin, &[elem], miss, true);
            let e = self.list_elem(items, len, idx, miss);
            let (want, off, ty) = Self::lane_tag(&l, elem).expect("scalar lane");
            let tag = self.b.ins().uload8(types::I32, trusted, e, 0);
            let other = self.b.ins().icmp_imm(IntCC::NotEqual, tag, i64::from(want));
            self.miss_if(other, miss);
            let v = self.b.ins().load(ty, trusted, e, off);
            self.b.ins().jump(done, &[BlockArg::from(v)]);
            self.b.switch_to_block(miss);
            join = Some(done);
        }
        let sig = self.list_helper_sig();
        let helper = self
            .b
            .ins()
            .iconst(self.ptr_ty, runtime::list_get_helper_addr() as i64);
        let call = self
            .b
            .ins()
            .call_indirect(sig, helper, &[self.frame_ptr, pin, idx]);
        let status = self.b.inst_results(call)[0];
        let bad = self.b.ins().icmp_imm(IntCC::NotEqual, status, 0);
        let cont = self.guard(bad, pc, &snapshot);
        self.b.switch_to_block(cont);
        let res = self
            .b
            .ins()
            .load(Self::cl_ty(elem), trusted, self.frame_ptr, OFF_RET_BITS);
        let res = match join {
            Some(done) => {
                self.b.ins().jump(done, &[BlockArg::from(res)]);
                self.b.switch_to_block(done);
                self.b.block_params(done)[0]
            }
            None => res,
        };
        self.vstack.push((res, elem));
    }

    /// RFC 0061 WS5 — pinned-list element write. The value is staged
    /// through `ret_bits` (dead between calls, same trick as the call
    /// helper's out-slot) so one helper signature serves both ops.
    fn emit_list_set(&mut self, pc: u32) {
        let trusted = MemFlags::trusted();
        let snapshot = self.vstack.clone();
        let (idx, _) = self.pop();
        let (pin, _) = self.pop();
        let (val, vty) = self.pop();
        // A scalar over a scalar element of a list of that lane, stored in
        // line (no release).
        let mut stored = None;
        if let (Some(l), JitType::Int | JitType::Float) = (runtime::obj_layout(), vty) {
            let l = *l;
            let miss = self.b.create_block();
            let done = self.b.create_block();
            let (items, len, _) = self.pinned_list(&l, pin, &[vty], miss, false);
            let e = self.list_elem(items, len, idx, miss);
            self.store_scalar_over_scalar(&l, e, val, vty, miss);
            self.b.ins().jump(done, &[]);
            self.b.switch_to_block(miss);
            stored = Some(done);
        }
        // Typed store: an F64 value lands as its bit pattern.
        self.b
            .ins()
            .store(trusted, val, self.frame_ptr, OFF_RET_BITS);
        let sig = self.list_helper_sig();
        let helper = self
            .b
            .ins()
            .iconst(self.ptr_ty, runtime::list_set_helper_addr() as i64);
        let call = self
            .b
            .ins()
            .call_indirect(sig, helper, &[self.frame_ptr, pin, idx]);
        let status = self.b.inst_results(call)[0];
        let bad = self.b.ins().icmp_imm(IntCC::NotEqual, status, 0);
        let cont = self.guard(bad, pc, &snapshot);
        self.b.switch_to_block(cont);
        if let Some(done) = stored {
            self.b.ins().jump(done, &[]);
            self.b.switch_to_block(done);
        }
    }

    /// Store the scalar `val` (of `lane`) over the value at `at` when that
    /// one is a scalar too (nothing to release); otherwise branch to
    /// `miss`, untouched.
    fn store_scalar_over_scalar(
        &mut self,
        l: &runtime::ObjLayout,
        at: Value,
        val: Value,
        lane: JitType,
        miss: Block,
    ) {
        let t = MemFlags::trusted();
        let old = self.b.ins().uload8(types::I64, t, at, 0);
        let one = self.b.ins().iconst(types::I64, 1);
        let bit = self.b.ins().ishl(one, old);
        let scalars = (1i64 << l.tag_int)
            | (1i64 << l.tag_float)
            | (1i64 << l.tag_bool)
            | (1i64 << l.tag_none);
        let m = self.b.ins().band_imm(bit, scalars);
        let heap = self.b.ins().icmp_imm(IntCC::Equal, m, 0);
        self.miss_if(heap, miss);
        let (tag, off, _) = Self::lane_tag(l, lane).expect("scalar lane");
        let v = if lane == JitType::Bool {
            self.b.ins().ireduce(types::I8, val)
        } else {
            val
        };
        let tagv = self.b.ins().iconst(types::I8, i64::from(tag));
        self.b.ins().store(t, tagv, at, 0);
        self.b.ins().store(t, v, at, off);
    }

    /// The shared `(frame, pin, idx) -> status` signature of the
    /// pinned-list helpers (RFC 0061 WS5, lazy).
    fn list_helper_sig(&mut self) -> SigRef {
        if let Some(sig) = self.list_sig {
            return sig;
        }
        let mut sig = Signature::new(self.b.func.signature.call_conv);
        sig.params.push(AbiParam::new(self.ptr_ty)); // frame
        sig.params.push(AbiParam::new(types::I64)); // pin
        sig.params.push(AbiParam::new(types::I64)); // idx
        sig.returns.push(AbiParam::new(types::I64)); // status
        let r = self.b.import_signature(sig);
        self.list_sig = Some(r);
        r
    }

    /// Lower a native Python-to-Python call (RFC 0059 WS3): marshal the
    /// arguments, write back the managed locals (the callee may observe
    /// the caller frame), call the registered `wpjit_call_py` helper,
    /// then dispatch on its [`crate::runtime::CallStatus`]:
    ///
    /// - `Ok`  — the result (already lane-checked by the helper) is in
    ///   `ret_bits`; load and push it.
    /// - `Raised` — exit with [`JitStatus::Raised`] at the call's pc
    ///   (the helper parked the exception in the embedder).
    /// - `Boxed` — the call *completed* but its result is
    ///   unrepresentable (or a caller guard was invalidated by callee
    ///   side effects): exit with [`JitStatus::Deopt`] at `pc + 1`; the
    ///   embedder pushes the parked result after rebuilding the stack.
    ///   The call is never re-executed.
    /// RFC 0073 WS5 — the keyword form (`kwc > 0`) marshals the same
    /// way with a compile-time slot shuffle: the top `kwc` stack values
    /// are keyword values whose destination slots come from `perm`
    /// (4 bits each, tier-1's `CallPyKwNames` packing). The analyzer
    /// validated the filled set to be exactly `0..argc+kwc`, so the
    /// helper still sees a plain positional prefix.
    /// Lower a direct self call (see `engine::self_direct_eligible`):
    /// charge the activation through the enter helper, fill a callee
    /// `JitFrame` on this function's native stack frame (the arguments
    /// in its first locals, the caller's embedder context shared), call
    /// this function itself, and release the charge. A callee that
    /// deopts or raises finishes through the slow helper, whose
    /// [`crate::runtime::CallStatus`] the caller handles as it does the
    /// call helper's. When the enter helper declines (recursion limit,
    /// pending interpreter work, observers), the ordinary call runs.
    fn emit_call_self(&mut self, token: u32, argc: u8, ret: JitType, pc: u32) {
        let trusted = MemFlags::trusted();
        let (enter_addr, exit_addr, slow_addr) =
            runtime::self_call_helper_addrs().expect("checked by the engine");
        let n = argc as usize;
        let base = self.vstack.len() - n;
        let args: Vec<(Value, JitType)> = self.vstack[base..].to_vec();
        self.vstack.truncate(base);
        let snapshot = self.vstack.clone();
        self.writeback_locals();
        self.store_call_site_pc(pc);

        let sig = self.self_sig();
        let enter = self.b.ins().iconst(self.ptr_ty, enter_addr as i64);
        let call = self.b.ins().call_indirect(sig, enter, &[self.frame_ptr]);
        let declined = self.b.inst_results(call)[0];

        let direct_b = self.b.create_block();
        let generic_b = self.b.create_block();
        let join_b = self.b.create_block();
        self.b.append_block_param(join_b, Self::cl_ty(ret));
        let go = self.b.ins().icmp_imm(IntCC::Equal, declined, 0);
        self.b.ins().brif(go, direct_b, &[], generic_b, &[]);

        // Declined: the ordinary call helper.
        self.b.switch_to_block(generic_b);
        self.vstack.extend(args.iter().copied());
        self.emit_call_py(token, argc, 0, 0, 0, ret, pc);
        let (v, _) = self.vstack.pop().expect("the call's result");
        self.b.ins().jump(join_b, &[v.into()]);

        // Direct: the callee frame and buffers, laid out in one slot.
        self.b.switch_to_block(direct_b);
        let n_locals = self.tfunc.n_locals.max(1) as i32;
        let cap = self.tfunc.max_stack as i32 + 1;
        let call_cap = self.tfunc.max_call_args.max(1) as i32;
        let frame_size = core::mem::size_of::<JitFrame>() as i32;
        let off_locals = frame_size;
        let off_spill = off_locals + n_locals * 8;
        let off_tags = off_spill + cap * 8;
        let off_call_args = (off_tags + cap * 4 + 7) & !7;
        let off_call_tags = off_call_args + call_cap * 8;
        let size = off_call_tags + call_cap * 4;
        let slot = match self.self_slot {
            Some(slot) => slot,
            None => {
                let slot = self.b.create_sized_stack_slot(StackSlotData::new(
                    StackSlotKind::ExplicitSlot,
                    size as u32,
                    3,
                ));
                self.self_slot = Some(slot);
                slot
            }
        };
        let fp = self.b.ins().stack_addr(self.ptr_ty, slot, 0);
        let at = |b: &mut FunctionBuilder<'_>, ptr_ty: Type, off: i32| {
            b.ins().stack_addr(ptr_ty, slot, off)
        };
        let locals = at(self.b, self.ptr_ty, off_locals);
        let spill = at(self.b, self.ptr_ty, off_spill);
        let tags = at(self.b, self.ptr_ty, off_tags);
        let cargs = at(self.b, self.ptr_ty, off_call_args);
        let ctags = at(self.b, self.ptr_ty, off_call_tags);
        let ctx = self
            .b
            .ins()
            .load(self.ptr_ty, trusted, self.frame_ptr, OFF_CTX);
        let zero64 = self.b.ins().iconst(types::I64, 0);
        let zero32 = self.b.ins().iconst(types::I32, 0);
        self.b.ins().store(trusted, locals, fp, OFF_LOCALS);
        let nl = self.b.ins().iconst(types::I32, i64::from(n_locals));
        self.b.ins().store(trusted, nl, fp, OFF_N_LOCALS);
        self.b.ins().store(trusted, zero32, fp, OFF_ENTRY_PC);
        self.b.ins().store(trusted, zero64, fp, OFF_RET_BITS);
        self.b.ins().store(trusted, zero32, fp, OFF_RET_TAG);
        self.b.ins().store(trusted, zero32, fp, OFF_DEOPT_PC);
        self.b.ins().store(trusted, spill, fp, OFF_STACK_SPILL);
        self.b.ins().store(trusted, tags, fp, OFF_STACK_TAGS);
        self.b.ins().store(trusted, zero32, fp, OFF_STACK_LEN);
        let capv = self.b.ins().iconst(types::I32, i64::from(cap));
        self.b.ins().store(trusted, capv, fp, OFF_STACK_CAP);
        self.b.ins().store(trusted, ctx, fp, OFF_CTX);
        self.b.ins().store(trusted, cargs, fp, OFF_CALL_ARGS);
        self.b.ins().store(trusted, ctags, fp, OFF_CALL_TAGS);
        // The arguments bind the first locals (their lanes are the
        // parameters' own); every other local starts zeroed, as the
        // framed entries leave it.
        for slot_ix in 0..n_locals {
            let off = slot_ix * 8;
            match args.get(slot_ix as usize) {
                Some(&(v, _)) => {
                    self.b.ins().store(trusted, v, locals, off);
                }
                None => {
                    self.b.ins().store(trusted, zero64, locals, off);
                }
            }
        }
        let self_func = self.self_func.expect("checked by the caller");
        let call = self.b.ins().call(self_func, &[fp]);
        let status = self.b.inst_results(call)[0];
        let exit = self.b.ins().iconst(self.ptr_ty, exit_addr as i64);
        self.b.ins().call_indirect(sig, exit, &[self.frame_ptr]);

        let returned_b = self.b.create_block();
        let slow_b = self.b.create_block();
        let is_ret = self
            .b
            .ins()
            .icmp_imm(IntCC::Equal, status, JitStatus::Returned as i64);
        self.b.ins().brif(is_ret, returned_b, &[], slow_b, &[]);

        self.b.switch_to_block(returned_b);
        let v = self
            .b
            .ins()
            .load(Self::cl_ty(ret), trusted, fp, OFF_RET_BITS);
        self.b.ins().jump(join_b, &[v.into()]);

        // Deopted or raised: finished by the slow helper.
        self.b.switch_to_block(slow_b);
        let slow_sig = self.self_slow_sig();
        let slow = self.b.ins().iconst(self.ptr_ty, slow_addr as i64);
        let tokenv = self.b.ins().iconst(types::I64, i64::from(token));
        let tagv = self.b.ins().iconst(types::I64, Self::tag(ret));
        let call =
            self.b
                .ins()
                .call_indirect(slow_sig, slow, &[self.frame_ptr, fp, status, tokenv, tagv]);
        let cstatus = self.b.inst_results(call)[0];
        let ok_b = self.b.create_block();
        let bad_b = self.b.create_block();
        let is_ok = self.b.ins().icmp_imm(IntCC::Equal, cstatus, 0);
        self.b.ins().brif(is_ok, ok_b, &[], bad_b, &[]);
        self.b.switch_to_block(bad_b);
        let raised_b = self.b.create_block();
        let boxed_b = self.b.create_block();
        let is_raised = self.b.ins().icmp_imm(IntCC::Equal, cstatus, 1);
        self.b.ins().brif(is_raised, raised_b, &[], boxed_b, &[]);
        self.b.switch_to_block(raised_b);
        self.emit_exit(pc, &snapshot, JitStatus::Raised);
        self.b.switch_to_block(boxed_b);
        self.emit_exit(pc + 1, &snapshot, JitStatus::Deopt);
        self.b.switch_to_block(ok_b);
        let v = self
            .b
            .ins()
            .load(Self::cl_ty(ret), trusted, self.frame_ptr, OFF_RET_BITS);
        self.b.ins().jump(join_b, &[v.into()]);

        self.b.switch_to_block(join_b);
        let v = self.b.block_params(join_b)[0];
        self.vstack.push((v, ret));
    }

    /// The direct leaf target of `token`, and where each of its
    /// parameters comes from, when this site's `argc` positional and `kwc`
    /// keyword values (keyword `j` binding slot `(perm >> 4j) & 0xF`) have
    /// the leaf's parameter lanes and every parameter they leave out has a
    /// default the leaf carries.
    fn leaf_for(&self, token: u32, argc: u8, kwc: u8, perm: u32) -> Option<(usize, Vec<LeafArg>)> {
        let ix = self
            .leaves
            .iter()
            .position(|l| l.token == token && !l.receiver)?;
        let leaf = &self.leaves[ix];
        let n = argc as usize + kwc as usize;
        let base = self.vstack.len().checked_sub(n)?;
        let mut from: Vec<Option<LeafArg>> = vec![None; leaf.params.len()];
        for j in 0..n {
            let slot = if j < argc as usize {
                j
            } else {
                ((perm >> (4 * (j - argc as usize))) & 0xF) as usize
            };
            let at = from.get_mut(slot)?;
            if at.is_some() || self.vstack[base + j].1 != leaf.params[slot] {
                return None;
            }
            *at = Some(LeafArg::Arg(j));
        }
        let from = from
            .iter()
            .zip(&leaf.defaults)
            .map(|(&f, &d)| f.or(d.map(LeafArg::Default)))
            .collect::<Option<Vec<_>>>()?;
        Some((ix, from))
    }

    /// Lower a direct call of a compiled scalar leaf (see [`LeafTarget`]):
    /// charge the activation through the self-call enter helper, fill the
    /// leaf's `JitFrame` on this function's native stack frame (the
    /// parameters from the call's values and the leaf's burned-in
    /// defaults, per `from`), call it, and release the charge. The leaf
    /// only computes, so when the enter helper declines or the leaf deopts
    /// (an overflow, a zero divisor), the ordinary call helper runs the
    /// call from the start.
    fn emit_call_leaf(
        &mut self,
        ix: usize,
        from: &[LeafArg],
        call: SiteCall,
        ret: JitType,
        pc: u32,
    ) {
        let (enter_addr, _, _) = runtime::self_call_helper_addrs().expect("checked by the engine");
        let n = call.argc as usize + call.kwc as usize;
        let base = self.vstack.len() - n;
        let args: Vec<(Value, JitType)> = self.vstack[base..].to_vec();
        self.vstack.truncate(base);
        self.writeback_locals();
        self.store_call_site_pc(pc);

        let sig = self.self_sig();
        let enter = self.b.ins().iconst(self.ptr_ty, enter_addr as i64);
        let c = self.b.ins().call_indirect(sig, enter, &[self.frame_ptr]);
        let declined = self.b.inst_results(c)[0];

        let direct_b = self.b.create_block();
        let generic_b = self.b.create_block();
        let join_b = self.b.create_block();
        self.b.append_block_param(join_b, Self::cl_ty(ret));
        let go = self.b.ins().icmp_imm(IntCC::Equal, declined, 0);
        self.b.ins().brif(go, direct_b, &[], generic_b, &[]);

        self.b.switch_to_block(direct_b);
        let mut params: Vec<Option<Value>> = Vec::with_capacity(from.len());
        for f in from {
            params.push(Some(match *f {
                LeafArg::Arg(j) => args[j].0,
                LeafArg::Default(bits) => self.b.ins().iconst(types::I64, bits as i64),
            }));
        }
        self.call_leaf_body(ix, &params, ret, join_b, generic_b);

        // Declined or deopted: the ordinary call, from the start.
        self.b.switch_to_block(generic_b);
        self.vstack.extend(args.iter().copied());
        self.emit_call_py(
            call.token, call.argc, call.kwc, call.perm, call.gaps, ret, pc,
        );
        let (v, _) = self.vstack.pop().expect("the call's result");
        self.b.ins().jump(join_b, &[v.into()]);

        self.b.switch_to_block(join_b);
        let v = self.b.block_params(join_b)[0];
        self.vstack.push((v, ret));
    }

    /// The direct method target of method token `token` (see
    /// [`LeafTarget::receiver`]), when this site's `argc` arguments have
    /// the method's parameter lanes and it expects a scalar result.
    fn method_leaf_for(&self, token: u32, argc: u8, ret: MethodRet) -> Option<usize> {
        if runtime::method_enter_helper_addr() == 0 || leaf_ret_lane(ret).is_none() {
            return None;
        }
        let ix = self
            .leaves
            .iter()
            .position(|l| l.token == token && l.receiver)?;
        let leaf = &self.leaves[ix];
        let n = argc as usize;
        let base = self.vstack.len().checked_sub(n)?;
        let lanes_ok = leaf.params.len() == n
            && self.vstack[base..]
                .iter()
                .zip(&leaf.params)
                .all(|(&(_, ty), &want)| ty == want);
        lanes_ok.then_some(ix)
    }

    /// Lower a guarded direct call of a compiled method (see
    /// [`LeafTarget::receiver`]). The site's guard (the receiver's class
    /// version, no instance attribute shadowing the name, the function's
    /// `__code__`) runs in line where the embedder published its layout,
    /// then the self-call enter helper charges the activation; a miss asks
    /// the method enter helper, which applies the guard exactly and charges
    /// in one call. The method's body then runs in its own `JitFrame` on
    /// this function's native stack frame, its receiver slot unread, and
    /// the exit helper releases the charge. The body only computes, so when
    /// the helpers decline (a guard miss, pending interpreter work, an
    /// observer, the recursion limit) or the body deopts, the method helper
    /// runs the call from the start, exactly as the site would without a
    /// direct target.
    fn emit_call_method_leaf(&mut self, ix: usize, token: u32, argc: u8, ret: MethodRet, pc: u32) {
        let lane = leaf_ret_lane(ret).expect("checked by method_leaf_for");
        let n = argc as usize;
        let base = self.vstack.len() - n;
        let args: Vec<(Value, JitType)> = self.vstack[base..].to_vec();
        self.vstack.truncate(base);
        let recv = self.pop();
        self.writeback_locals();
        self.store_call_site_pc(pc);

        let direct_b = self.b.create_block();
        let generic_b = self.b.create_block();
        let join_b = self.b.create_block();
        self.b.append_block_param(join_b, Self::cl_ty(lane));

        if let Some(l) = runtime::obj_layout().copied() {
            let slow_b = self.b.create_block();
            self.inline_method_guard(&l, recv.0, token, slow_b);
            let (enter_addr, _, _) =
                runtime::self_call_helper_addrs().expect("checked by the engine");
            let sig = self.self_sig();
            let enter = self.b.ins().iconst(self.ptr_ty, enter_addr as i64);
            let c = self.b.ins().call_indirect(sig, enter, &[self.frame_ptr]);
            let declined = self.b.inst_results(c)[0];
            let go = self.b.ins().icmp_imm(IntCC::Equal, declined, 0);
            self.b.ins().brif(go, direct_b, &[], generic_b, &[]);
            self.b.switch_to_block(slow_b);
        }
        let sig = self.list_helper_sig();
        let enter = self
            .b
            .ins()
            .iconst(self.ptr_ty, runtime::method_enter_helper_addr() as i64);
        let tokenv = self.b.ins().iconst(types::I64, i64::from(token));
        let c = self
            .b
            .ins()
            .call_indirect(sig, enter, &[self.frame_ptr, recv.0, tokenv]);
        let declined = self.b.inst_results(c)[0];
        let go = self.b.ins().icmp_imm(IntCC::Equal, declined, 0);
        self.b.ins().brif(go, direct_b, &[], generic_b, &[]);

        self.b.switch_to_block(direct_b);
        let params: Vec<Option<Value>> = std::iter::once(None)
            .chain(args.iter().map(|&(v, _)| Some(v)))
            .collect();
        self.call_leaf_body(ix, &params, lane, join_b, generic_b);

        // Declined or deopted: the method helper, from the start.
        self.b.switch_to_block(generic_b);
        self.vstack.push(recv);
        self.vstack.extend(args.iter().copied());
        self.emit_call_method(token, argc, ret, pc);
        let (v, _) = self.vstack.pop().expect("the call's result");
        self.b.ins().jump(join_b, &[v.into()]);

        self.b.switch_to_block(join_b);
        let v = self.b.block_params(join_b)[0];
        self.vstack.push((v, lane));
    }

    /// The guard of method token `token` on the pinned receiver `pin`, in
    /// line (see [`runtime::ObjLayout::method_upd_shadow`]): an instance
    /// pin whose class still has the entry's version, whose values are
    /// split over its class's names (or empty) and too few to hold the
    /// method's name, and whose function still wears the entry's code.
    /// Anything else branches to `miss` (the method enter helper, which
    /// decides exactly). Returns the receiver's split values' block
    /// pointer (null for none) and its payload pointer.
    fn inline_method_guard(
        &mut self,
        l: &runtime::ObjLayout,
        pin: Value,
        token: u32,
        miss: Block,
    ) -> (Value, Value) {
        let t = MemFlags::trusted();
        let ptr = self.ptr_ty;
        let ctx = self.b.ins().load(ptr, t, self.frame_ptr, OFF_CTX);
        // The entry, armed with its function's code.
        let methods = self.b.ins().load(ptr, t, ctx, l.ctx_methods);
        let n = self.b.ins().load(types::I64, t, methods, l.methods_len);
        let out = self
            .b
            .ins()
            .icmp_imm(IntCC::UnsignedLessThanOrEqual, n, i64::from(token));
        self.miss_if(out, miss);
        let mbuf = self.b.ins().load(ptr, t, methods, l.methods_buf);
        let e = self
            .b
            .ins()
            .iadd_imm(mbuf, i64::from(token) * i64::from(l.method_size));
        let code_at = self.b.ins().load(ptr, t, e, l.method_upd_code_at);
        let unarmed = self.b.ins().icmp_imm(IntCC::Equal, code_at, 0);
        // The pin.
        let n = self.b.ins().load(types::I64, t, ctx, l.ctx_pins_len);
        let out = self.b.ins().icmp(IntCC::UnsignedGreaterThanOrEqual, pin, n);
        let bad = self.b.ins().bor(unarmed, out);
        self.miss_if(bad, miss);
        // The function still wears the code, and the pin is an instance.
        let want = self.b.ins().load(ptr, t, e, l.method_upd_code);
        let code = self.b.ins().load(ptr, t, code_at, 0);
        let swapped = self.b.ins().icmp(IntCC::NotEqual, code, want);
        let buf = self.b.ins().load(ptr, t, ctx, l.ctx_pins_ptr);
        let off = self.b.ins().imul_imm(pin, i64::from(l.pin_size));
        let p = self.b.ins().iadd(buf, off);
        let ptag = self.b.ins().uload8(types::I32, t, p, l.pin_tag);
        let otag = self.b.ins().uload8(types::I32, t, p, l.pin_obj);
        let not_obj = self
            .b
            .ins()
            .icmp_imm(IntCC::NotEqual, ptag, i64::from(l.pin_obj_tag));
        let not_inst = self
            .b
            .ins()
            .icmp_imm(IntCC::NotEqual, otag, i64::from(l.tag_instance));
        let bad = self.b.ins().bor(not_obj, not_inst);
        let bad = self.b.ins().bor(bad, swapped);
        self.miss_if(bad, miss);
        let inst = self.b.ins().load(ptr, t, p, l.pin_obj + 8);
        // Its class against the entry's version, and its values: split (no
        // published dict), not being written, and not shared between
        // threads.
        let ver = self.b.ins().load(types::I64, t, e, l.method_ver);
        let cls = self.b.ins().load(ptr, t, inst, l.inst_class);
        let cver = self.b.ins().load(types::I64, t, cls, l.type_attr_version);
        let lazy = self.b.ins().load(ptr, t, inst, l.inst_dict_lazy);
        let flag = self.b.ins().iconst(ptr, l.cells_unguarded as i64);
        let shared = self.b.ins().uload8(types::I64, t, flag, 0);
        let borrow = self.b.ins().sload32(t, inst, l.inst_split_borrow);
        let block = self.b.ins().load(ptr, t, inst, l.inst_split_block);
        let stale = self.b.ins().icmp(IntCC::NotEqual, cver, ver);
        let busy = self.b.ins().icmp_imm(IntCC::SignedLessThan, borrow, 0);
        let other = self.b.ins().bor(lazy, shared);
        let other = self.b.ins().icmp_imm(IntCC::NotEqual, other, 0);
        let bad = self.b.ins().bor(stale, busy);
        let bad = self.b.ins().bor(bad, other);
        self.miss_if(bad, miss);
        // No values can't shadow the name; any must be over the class's
        // names and fewer than precede the name there.
        let hit = self.b.create_block();
        let values = self.b.create_block();
        self.b.ins().brif(block, values, &[], hit, &[]);
        self.b.switch_to_block(values);
        let keys = self.b.ins().load(ptr, t, block, l.split_keys);
        let ckeys = self.b.ins().load(ptr, t, cls, l.type_shared_keys);
        let len = self.b.ins().uload32(t, block, l.split_len);
        let shadow = self.b.ins().uload32(t, e, l.method_upd_shadow);
        let foreign = self.b.ins().icmp(IntCC::NotEqual, keys, ckeys);
        let shadowed = self.b.ins().icmp(IntCC::UnsignedGreaterThan, len, shadow);
        let bad = self.b.ins().bor(foreign, shadowed);
        self.b.ins().brif(bad, miss, &[], hit, &[]);
        self.b.switch_to_block(hit);
        (block, inst)
    }

    /// The in-line body of method token `token` (see
    /// `engine::InlineMethod::of`) for this site (its call at `pc`), when
    /// the embedder published its object layout and offers one for these
    /// argument lanes whose result is the scalar the site expects, and
    /// every local the body reads is a parameter or was stored first.
    fn inline_body_for(
        &mut self,
        token: u32,
        argc: u8,
        ret: MethodRet,
        pc: u32,
    ) -> Option<InlineMethod> {
        if token & runtime::METHOD_NATIVE != 0 || runtime::obj_layout().is_none() {
            return None;
        }
        let lane = leaf_ret_lane(ret)?;
        let n = argc as usize;
        let base = self.vstack.len().checked_sub(n)?;
        if self.vstack.get(base.checked_sub(1)?)?.1 != JitType::Obj {
            return None;
        }
        let lanes: Vec<JitType> = self.vstack[base..].iter().map(|&(_, t)| t).collect();
        if !lanes.iter().all(|t| {
            matches!(
                t,
                JitType::Int | JitType::Float | JitType::Bool | JitType::Obj
            )
        }) {
            return None;
        }
        let lookup = self.inline_method.as_mut()?;
        let body = lookup(token, &lanes, pc)?;
        if body.ret != lane
            || body.params != lanes
            || body.field_at.contains(&FieldAt::Unknown)
            || (body
                .field_at
                .iter()
                .any(|a| matches!(a, FieldAt::Slot { .. }))
                && !runtime::obj_layout().is_some_and(|l| l.slots_ok))
        {
            return None;
        }
        let mut defined = vec![false; body.n_locals.max(1) as usize];
        for d in defined.iter_mut().take(n + 1) {
            *d = true;
        }
        for &op in &body.ops {
            match op {
                TOp::LoadLocal(s) if !defined.get(s as usize).copied().unwrap_or(false) => {
                    return None;
                }
                TOp::StoreLocal(s) => *defined.get_mut(s as usize)? = true,
                TOp::AttrGet { site, .. } if site as usize >= body.field_at.len() => return None,
                _ => {}
            }
        }
        Some(body)
    }

    /// Lower a guarded method call whose body runs in line (see
    /// `engine::InlineMethod`): the site's guard as for a direct method
    /// call, the gates a call checks (no observer or pending interpreter
    /// work, room under the recursion limit), each object argument's class
    /// version, then the body's operations on its receivers' fields and the
    /// call's arguments. The body does nothing observable, so any miss (a
    /// guard, a field of another lane or position, an overflow, a zero
    /// divisor) runs the call through the method helper from the start.
    fn emit_call_method_inline(
        &mut self,
        body: &InlineMethod,
        token: u32,
        argc: u8,
        ret: MethodRet,
        pc: u32,
    ) {
        let lane = leaf_ret_lane(ret).expect("checked by inline_body_for");
        let l = *runtime::obj_layout().expect("checked by inline_body_for");
        let n = argc as usize;
        let base = self.vstack.len() - n;
        let args: Vec<(Value, JitType)> = self.vstack[base..].to_vec();
        self.vstack.truncate(base);
        let recv = self.pop();

        let generic_b = self.b.create_block();
        let join_b = self.b.create_block();
        self.b.append_block_param(join_b, Self::cl_ty(lane));
        let (block, inst) = self.inline_method_guard(&l, recv.0, token, generic_b);
        self.inline_call_gates(&l, generic_b);
        // Where each receiver keeps the fields the body reads: `self`'s
        // split values from the guard, an object argument's once its class
        // is checked, and either's laid-out member slots.
        let mut bases: Vec<(Option<Value>, Option<Value>)> = vec![(None, None); n + 1];
        bases[0].0 = Some(block);
        let mut payloads: Vec<Option<Value>> = vec![None; n + 1];
        payloads[0] = Some(inst);
        for (k, &r) in body.field_recv.iter().enumerate() {
            let r = r as usize;
            let need = match body.field_at[k] {
                FieldAt::Split(_) => bases[r].0.is_none(),
                FieldAt::Slot { .. } => bases[r].1.is_none(),
                FieldAt::Unknown => unreachable!("checked by inline_body_for"),
            };
            if !need {
                continue;
            }
            let at = match payloads[r] {
                Some(at) => at,
                None => {
                    let at = self.inline_arg_guard(&l, args[r - 1].0, body.recv_ver[r], generic_b);
                    payloads[r] = Some(at);
                    at
                }
            };
            match body.field_at[k] {
                FieldAt::Split(_) => {
                    let b = self.inline_split_values(&l, at, generic_b);
                    bases[r].0 = Some(b);
                }
                FieldAt::Slot { layout, .. } => {
                    let layout = self.b.ins().iconst(self.ptr_ty, layout as i64);
                    let (vals, bad) = self.laid_out_slots(&l, at, layout, true);
                    self.miss_if(bad, generic_b);
                    bases[r].1 = Some(vals);
                }
                FieldAt::Unknown => {}
            }
        }

        let outer = std::mem::take(&mut self.vstack);
        let outer_miss = self.miss_redirect.replace(generic_b);
        let mut locals: Vec<Option<(Value, JitType)>> = vec![None; body.n_locals.max(1) as usize];
        locals[0] = Some(recv);
        for (j, &a) in args.iter().enumerate() {
            locals[j + 1] = Some(a);
        }
        for &op in &body.ops {
            match op {
                TOp::LoadLocal(s) => {
                    let v = locals[s as usize].expect("checked by inline_body_for");
                    self.vstack.push(v);
                }
                TOp::StoreLocal(s) => {
                    let v = self.pop();
                    locals[s as usize] = Some(v);
                }
                TOp::AttrGet { site, out } => {
                    self.pop();
                    let r = body.field_recv[site as usize] as usize;
                    let v = match body.field_at[site as usize] {
                        FieldAt::Split(idx) => {
                            let block = bases[r].0.expect("guarded above");
                            self.inline_field_read(&l, block, idx, out, generic_b)
                        }
                        FieldAt::Slot { idx, .. } => {
                            let vals = bases[r].1.expect("guarded above");
                            self.inline_slot_read(&l, vals, idx, out, generic_b)
                        }
                        FieldAt::Unknown => unreachable!("checked by inline_body_for"),
                    };
                    self.vstack.push((v, out));
                }
                op => self.emit_stmt(TStmt { pc, op }),
            }
        }
        let (v, _) = self.pop();
        self.miss_redirect = outer_miss;
        self.vstack = outer;
        self.b.ins().jump(join_b, &[v.into()]);

        // A miss: the method helper, from the start.
        self.b.switch_to_block(generic_b);
        self.vstack.push(recv);
        self.vstack.extend(args.iter().copied());
        self.emit_call_method(token, argc, ret, pc);
        let (v, _) = self.pop();
        self.b.ins().jump(join_b, &[v.into()]);

        self.b.switch_to_block(join_b);
        let v = self.b.block_params(join_b)[0];
        self.vstack.push((v, lane));
    }

    /// An in-line body's object argument `pin`, in line: an instance pin
    /// whose class has version `ver`, with cells unshared. Returns its
    /// payload pointer; anything else branches to `miss`.
    fn inline_arg_guard(
        &mut self,
        l: &runtime::ObjLayout,
        pin: Value,
        ver: u64,
        miss: Block,
    ) -> Value {
        let t = MemFlags::trusted();
        let ptr = self.ptr_ty;
        let ctx = self.b.ins().load(ptr, t, self.frame_ptr, OFF_CTX);
        let n = self.b.ins().load(types::I64, t, ctx, l.ctx_pins_len);
        let out = self.b.ins().icmp(IntCC::UnsignedGreaterThanOrEqual, pin, n);
        self.miss_if(out, miss);
        let buf = self.b.ins().load(ptr, t, ctx, l.ctx_pins_ptr);
        let off = self.b.ins().imul_imm(pin, i64::from(l.pin_size));
        let p = self.b.ins().iadd(buf, off);
        let ptag = self.b.ins().uload8(types::I32, t, p, l.pin_tag);
        let otag = self.b.ins().uload8(types::I32, t, p, l.pin_obj);
        let not_obj = self
            .b
            .ins()
            .icmp_imm(IntCC::NotEqual, ptag, i64::from(l.pin_obj_tag));
        let not_inst = self
            .b
            .ins()
            .icmp_imm(IntCC::NotEqual, otag, i64::from(l.tag_instance));
        let bad = self.b.ins().bor(not_obj, not_inst);
        self.miss_if(bad, miss);
        let inst = self.b.ins().load(ptr, t, p, l.pin_obj + 8);
        let cls = self.b.ins().load(ptr, t, inst, l.inst_class);
        let cver = self.b.ins().load(types::I64, t, cls, l.type_attr_version);
        let flag = self.b.ins().iconst(ptr, l.cells_unguarded as i64);
        let shared = self.b.ins().uload8(types::I64, t, flag, 0);
        let want = self.b.ins().iconst(types::I64, ver as i64);
        let stale = self.b.ins().icmp(IntCC::NotEqual, cver, want);
        let shared = self.b.ins().icmp_imm(IntCC::NotEqual, shared, 0);
        let bad = self.b.ins().bor(stale, shared);
        self.miss_if(bad, miss);
        inst
    }

    /// The split values of the instance at `inst` (its payload pointer,
    /// its class checked): unpublished, not being written, and over its
    /// class's names. Returns the block pointer; anything else (no values
    /// included) branches to `miss`.
    fn inline_split_values(&mut self, l: &runtime::ObjLayout, inst: Value, miss: Block) -> Value {
        let t = MemFlags::trusted();
        let ptr = self.ptr_ty;
        let lazy = self.b.ins().load(ptr, t, inst, l.inst_dict_lazy);
        let borrow = self.b.ins().sload32(t, inst, l.inst_split_borrow);
        let block = self.b.ins().load(ptr, t, inst, l.inst_split_block);
        let busy = self.b.ins().icmp_imm(IntCC::SignedLessThan, borrow, 0);
        let other = self.b.ins().icmp_imm(IntCC::NotEqual, lazy, 0);
        let empty = self.b.ins().icmp_imm(IntCC::Equal, block, 0);
        let bad = self.b.ins().bor(busy, other);
        let bad = self.b.ins().bor(bad, empty);
        self.miss_if(bad, miss);
        let cls = self.b.ins().load(ptr, t, inst, l.inst_class);
        let keys = self.b.ins().load(ptr, t, block, l.split_keys);
        let ckeys = self.b.ins().load(ptr, t, cls, l.type_shared_keys);
        let foreign = self.b.ins().icmp(IntCC::NotEqual, keys, ckeys);
        self.miss_if(foreign, miss);
        block
    }

    /// The member slots of the instance at `inst` (its payload pointer),
    /// as the embedder lays them out (see
    /// [`runtime::ObjLayout::slots_laid_out`]): their values' pointer, and
    /// whether they're unusable here (borrowed for a write, or at all when
    /// not `read`, or not laid out over the layout whose address is
    /// `layout`). The caller checks that cells are unshared.
    fn laid_out_slots(
        &mut self,
        l: &runtime::ObjLayout,
        inst: Value,
        layout: Value,
        read: bool,
    ) -> (Value, Value) {
        let t = MemFlags::trusted();
        let borrow = self.b.ins().sload32(t, inst, l.inst_slots_borrow);
        let form = self.b.ins().uload8(types::I32, t, inst, l.inst_slots_tag);
        let names = self.b.ins().load(self.ptr_ty, t, inst, l.inst_slots_layout);
        let vals = self.b.ins().load(self.ptr_ty, t, inst, l.inst_slots_values);
        let busy = if read {
            self.b.ins().icmp_imm(IntCC::SignedLessThan, borrow, 0)
        } else {
            self.b.ins().icmp_imm(IntCC::NotEqual, borrow, 0)
        };
        let other = self
            .b
            .ins()
            .icmp_imm(IntCC::NotEqual, form, i64::from(l.slots_laid_out));
        let foreign = self.b.ins().icmp(IntCC::NotEqual, names, layout);
        let bad = self.b.ins().bor(busy, other);
        let bad = self.b.ins().bor(bad, foreign);
        (vals, bad)
    }

    /// The `lane` value of member slot `idx` among the laid-out values at
    /// `vals` (see [`Self::laid_out_slots`]): set, with that lane's tag.
    /// Anything else branches to `miss`.
    fn inline_slot_read(
        &mut self,
        l: &runtime::ObjLayout,
        vals: Value,
        idx: u32,
        lane: JitType,
        miss: Block,
    ) -> Value {
        let t = MemFlags::trusted();
        let at = self.b.ins().iadd_imm(vals, i64::from(idx) * 16);
        let (want, off, ty) = Self::lane_tag(l, lane).expect("a scalar field");
        let tag = self.b.ins().uload8(types::I32, t, at, 0);
        let other = self.b.ins().icmp_imm(IntCC::NotEqual, tag, i64::from(want));
        self.miss_if(other, miss);
        let v = self.b.ins().load(ty, t, at, off);
        if ty == types::I8 {
            self.b.ins().uextend(types::I64, v)
        } else {
            v
        }
    }

    /// The checks a call makes before its body runs that an in-line body
    /// still needs: no observer that must see the call and no pending
    /// interpreter work (the gate words read zero), and a call depth under
    /// the recursion limit. Anything else branches to `miss`.
    fn inline_call_gates(&mut self, l: &runtime::ObjLayout, miss: Block) {
        let t = MemFlags::trusted();
        let ptr = self.ptr_ty;
        let observers = self.b.ins().iconst(ptr, l.observers as i64);
        let observers = self.b.ins().load(types::I64, t, observers, 0);
        let hot = self.b.ins().iconst(ptr, l.hot_gates as i64);
        let hot = self.b.ins().uload32(t, hot, 0);
        let gates = self.b.ins().bor(observers, hot);
        let ctx = self.b.ins().load(ptr, t, self.frame_ptr, OFF_CTX);
        let cell = self.b.ins().load(ptr, t, ctx, l.ctx_depth_cell);
        let depth = self.b.ins().load(types::I64, t, cell, 0);
        let limit = self.b.ins().iconst(ptr, l.recursion_limit as i64);
        let limit = self.b.ins().load(types::I64, t, limit, 0);
        let deep = self
            .b
            .ins()
            .icmp(IntCC::UnsignedGreaterThanOrEqual, depth, limit);
        let gated = self.b.ins().icmp_imm(IntCC::NotEqual, gates, 0);
        let bad = self.b.ins().bor(gated, deep);
        self.miss_if(bad, miss);
    }

    /// The `lane` value of the receiver's split field `idx`, in line: the
    /// receiver's split values (`block`, which the method guard checked
    /// are over its class's names) hold the field, with that lane's tag.
    /// Anything else branches to `miss`.
    fn inline_field_read(
        &mut self,
        l: &runtime::ObjLayout,
        block: Value,
        idx: u32,
        lane: JitType,
        miss: Block,
    ) -> Value {
        let t = MemFlags::trusted();
        let empty = self.b.ins().icmp_imm(IntCC::Equal, block, 0);
        self.miss_if(empty, miss);
        let len = self.b.ins().uload32(t, block, l.split_len);
        let absent = self
            .b
            .ins()
            .icmp_imm(IntCC::UnsignedLessThanOrEqual, len, i64::from(idx));
        self.miss_if(absent, miss);
        let at = i64::from(idx) * 16 + i64::from(l.split_values);
        let at = self.b.ins().iadd_imm(block, at);
        let (want, off, ty) = match lane {
            JitType::Int => (l.tag_int, 8, types::I64),
            JitType::Float => (l.tag_float, 8, types::F64),
            _ => (l.tag_bool, 1, types::I8),
        };
        let tag = self.b.ins().uload8(types::I32, t, at, 0);
        let other = self.b.ins().icmp_imm(IntCC::NotEqual, tag, i64::from(want));
        self.miss_if(other, miss);
        let v = self.b.ins().load(ty, t, at, off);
        if ty == types::I8 {
            self.b.ins().uextend(types::I64, v)
        } else {
            v
        }
    }

    /// Run the charged leaf `ix` in a `JitFrame` laid out in a stack slot
    /// of this function, its locals bound to `params` (a `None`, and every
    /// local past them, starts zeroed), then release the charge through the
    /// self-call exit helper. A returned body jumps to `join` with its
    /// `ret`-lane result; anything else to `fallback`.
    fn call_leaf_body(
        &mut self,
        ix: usize,
        params: &[Option<Value>],
        ret: JitType,
        join: Block,
        fallback: Block,
    ) {
        let trusted = MemFlags::trusted();
        let (_, exit_addr, _) = runtime::self_call_helper_addrs().expect("checked by the engine");
        let leaf = &self.leaves[ix];
        let (func, n_locals, cap) = (
            leaf.func,
            leaf.n_locals.max(1) as i32,
            leaf.max_stack as i32 + 1,
        );
        let frame_size = core::mem::size_of::<JitFrame>() as i32;
        let off_locals = frame_size;
        let off_spill = off_locals + n_locals * 8;
        let off_tags = off_spill + cap * 8;
        let size = off_tags + cap * 4;
        let slot = self.b.create_sized_stack_slot(StackSlotData::new(
            StackSlotKind::ExplicitSlot,
            size as u32,
            3,
        ));
        let fp = self.b.ins().stack_addr(self.ptr_ty, slot, 0);
        let locals = self.b.ins().stack_addr(self.ptr_ty, slot, off_locals);
        let spill = self.b.ins().stack_addr(self.ptr_ty, slot, off_spill);
        let tags = self.b.ins().stack_addr(self.ptr_ty, slot, off_tags);
        let null = self.b.ins().iconst(self.ptr_ty, 0);
        let zero64 = self.b.ins().iconst(types::I64, 0);
        let zero32 = self.b.ins().iconst(types::I32, 0);
        self.b.ins().store(trusted, locals, fp, OFF_LOCALS);
        let nl = self.b.ins().iconst(types::I32, i64::from(n_locals));
        self.b.ins().store(trusted, nl, fp, OFF_N_LOCALS);
        self.b.ins().store(trusted, zero32, fp, OFF_ENTRY_PC);
        self.b.ins().store(trusted, zero64, fp, OFF_RET_BITS);
        self.b.ins().store(trusted, zero32, fp, OFF_RET_TAG);
        self.b.ins().store(trusted, zero32, fp, OFF_DEOPT_PC);
        self.b.ins().store(trusted, spill, fp, OFF_STACK_SPILL);
        self.b.ins().store(trusted, tags, fp, OFF_STACK_TAGS);
        self.b.ins().store(trusted, zero32, fp, OFF_STACK_LEN);
        let capv = self.b.ins().iconst(types::I32, i64::from(cap));
        self.b.ins().store(trusted, capv, fp, OFF_STACK_CAP);
        // A scalar leaf reads no embedder context and marshals no calls.
        self.b.ins().store(trusted, null, fp, OFF_CTX);
        self.b.ins().store(trusted, null, fp, OFF_CALL_ARGS);
        self.b.ins().store(trusted, null, fp, OFF_CALL_TAGS);
        for slot_ix in 0..n_locals {
            let v = params
                .get(slot_ix as usize)
                .copied()
                .flatten()
                .unwrap_or(zero64);
            self.b.ins().store(trusted, v, locals, slot_ix * 8);
        }
        let sig = self.self_sig();
        let c = self.b.ins().call(func, &[fp]);
        let status = self.b.inst_results(c)[0];
        let exit = self.b.ins().iconst(self.ptr_ty, exit_addr as i64);
        self.b.ins().call_indirect(sig, exit, &[self.frame_ptr]);
        let returned_b = self.b.create_block();
        let is_ret = self
            .b
            .ins()
            .icmp_imm(IntCC::Equal, status, JitStatus::Returned as i64);
        self.b.ins().brif(is_ret, returned_b, &[], fallback, &[]);
        self.b.switch_to_block(returned_b);
        let v = self
            .b
            .ins()
            .load(Self::cl_ty(ret), trusted, fp, OFF_RET_BITS);
        self.b.ins().jump(join, &[v.into()]);
    }

    /// The `(frame) -> i64` signature of the self-call enter/exit
    /// helpers (lazy).
    fn self_sig(&mut self) -> SigRef {
        if let Some(sig) = self.self_sig {
            return sig;
        }
        let mut sig = Signature::new(self.b.func.signature.call_conv);
        sig.params.push(AbiParam::new(self.ptr_ty));
        sig.returns.push(AbiParam::new(types::I64));
        let r = self.b.import_signature(sig);
        self.self_sig = Some(r);
        r
    }

    /// The slow self-call helper's signature (lazy).
    fn self_slow_sig(&mut self) -> SigRef {
        if let Some(sig) = self.self_slow_sig {
            return sig;
        }
        let mut sig = Signature::new(self.b.func.signature.call_conv);
        sig.params.push(AbiParam::new(self.ptr_ty)); // frame
        sig.params.push(AbiParam::new(self.ptr_ty)); // callee frame
        sig.params.push(AbiParam::new(types::I64)); // callee status
        sig.params.push(AbiParam::new(types::I64)); // token
        sig.params.push(AbiParam::new(types::I64)); // expected tag
        sig.returns.push(AbiParam::new(types::I64)); // call status
        let r = self.b.import_signature(sig);
        self.self_slow_sig = Some(r);
        r
    }

    #[allow(clippy::too_many_arguments)]
    fn emit_call_py(
        &mut self,
        token: u32,
        argc: u8,
        kwc: u8,
        perm: u32,
        gaps: u32,
        ret: JitType,
        pc: u32,
    ) {
        let trusted = MemFlags::trusted();
        let n = argc as usize + kwc as usize;
        // Skipped defaulted slots: the helper binds them (see
        // `SlotTag::Default`).
        let mut g = gaps;
        while g != 0 {
            let slot = g.trailing_zeros() as i32;
            g &= g - 1;
            let tagv = self
                .b
                .ins()
                .iconst(types::I32, runtime::SlotTag::Default as i64);
            self.b
                .ins()
                .store(trusted, tagv, self.call_tags_base, slot * 4);
        }
        let base = self.vstack.len() - n;
        for (j, &(v, ty)) in self.vstack[base..].iter().enumerate() {
            let dst = if j < argc as usize {
                j
            } else {
                ((perm >> (4 * (j - argc as usize))) & 0xF) as usize
            };
            let voff = (dst as i32) * 8;
            let toff = (dst as i32) * 4;
            // Store f64 lanes by value (same bit pattern, typed store).
            self.b.ins().store(trusted, v, self.call_args_base, voff);
            let tagv = self.b.ins().iconst(types::I32, Self::tag(ty));
            self.b.ins().store(trusted, tagv, self.call_tags_base, toff);
        }
        self.vstack.truncate(base);
        let snapshot = self.vstack.clone();

        // Keep the frame's local slots observably current across the
        // call (`sys._getframe`, tracebacks through this frame, and the
        // Raised/Boxed exits below all read them).
        self.writeback_locals();
        self.store_call_site_pc(pc);

        let sig = self.call_py_sig();
        let helper = self
            .b
            .ins()
            .iconst(self.ptr_ty, runtime::call_py_helper_addr() as i64);
        let tokenv = self.b.ins().iconst(types::I32, i64::from(token));
        // The helper receives the prefix length — keyword values were
        // shuffled into parameter slots above, with any skipped
        // defaulted slots flagged.
        let filled = if gaps == 0 {
            n as i64
        } else {
            i64::from((n as u32 + gaps.count_ones()) | runtime::CALL_GAPS)
        };
        let argcv = self.b.ins().iconst(types::I32, filled);
        let expect = self.b.ins().iconst(types::I32, Self::tag(ret));
        let call =
            self.b
                .ins()
                .call_indirect(sig, helper, &[self.frame_ptr, tokenv, argcv, expect]);
        let status = self.b.inst_results(call)[0];

        let ok_b = self.b.create_block();
        let bad_b = self.b.create_block();
        let is_ok = self.b.ins().icmp_imm(IntCC::Equal, status, 0);
        self.b.ins().brif(is_ok, ok_b, &[], bad_b, &[]);

        self.b.switch_to_block(bad_b);
        let raised_b = self.b.create_block();
        let boxed_b = self.b.create_block();
        let is_raised = self.b.ins().icmp_imm(IntCC::Equal, status, 1);
        self.b.ins().brif(is_raised, raised_b, &[], boxed_b, &[]);
        self.b.switch_to_block(raised_b);
        self.emit_exit(pc, &snapshot, JitStatus::Raised);
        self.b.switch_to_block(boxed_b);
        self.emit_exit(pc + 1, &snapshot, JitStatus::Deopt);

        self.b.switch_to_block(ok_b);
        let res = self
            .b
            .ins()
            .load(Self::cl_ty(ret), trusted, self.frame_ptr, OFF_RET_BITS);
        self.vstack.push((res, ret));
    }

    /// Store the call site's bytecode pc into the frame's `deopt_pc`
    /// slot right before a call helper runs (RFC 0076). A *frameless*
    /// activation (native call lanes, direct interpreter→native entry)
    /// has no interpreter frame; when its call falls back to the
    /// interpreter, the VM pushes a spine shell for this activation so
    /// callee-side stack walkers (`sys._getframe`,
    /// `traceback.walk_stack`) still observe it — and the shell reads
    /// this slot for its `f_lineno`. The slot is dead between exits
    /// (every exit path overwrites it), so the store is unconditional
    /// and costs one word.
    fn store_call_site_pc(&mut self, pc: u32) {
        let trusted = MemFlags::trusted();
        let pcv = self.b.ins().iconst(types::I32, i64::from(pc));
        self.b
            .ins()
            .store(trusted, pcv, self.frame_ptr, OFF_DEOPT_PC);
    }

    /// The imported signature of the `wpjit_call_py` helper (lazy).
    fn call_py_sig(&mut self) -> SigRef {
        if let Some(sig) = self.call_sig {
            return sig;
        }
        let mut sig = Signature::new(self.b.func.signature.call_conv);
        sig.params.push(AbiParam::new(self.ptr_ty)); // frame
        sig.params.push(AbiParam::new(types::I32)); // token
        sig.params.push(AbiParam::new(types::I32)); // argc
        sig.params.push(AbiParam::new(types::I32)); // expect_tag
        sig.returns.push(AbiParam::new(types::I64)); // CallStatus
        let r = self.b.import_signature(sig);
        self.call_sig = Some(r);
        r
    }

    /// Promote the integral value at `vstack[depth]` to a float in
    /// place. When `guarded`, deopt unless `|v| <= 2^53` — the range
    /// where `fcvt_from_sint` is exact — with the stack spilled in its
    /// original, unpromoted order.
    fn emit_int_to_float(&mut self, depth: usize, guarded: bool, pc: u32) {
        let v = self.vstack[depth].0;
        if guarded {
            let snapshot = self.vstack.clone();
            const EXACT: i64 = 1 << 53;
            let hi = self.b.ins().iconst(types::I64, EXACT);
            let lo = self.b.ins().iconst(types::I64, -EXACT);
            let too_big = self.b.ins().icmp(IntCC::SignedGreaterThan, v, hi);
            let too_small = self.b.ins().icmp(IntCC::SignedLessThan, v, lo);
            let inexact = self.b.ins().bor(too_big, too_small);
            let cont = self.guard(inexact, pc, &snapshot);
            self.b.switch_to_block(cont);
        }
        let f = self.b.ins().fcvt_from_sint(types::F64, v);
        self.vstack[depth] = (f, JitType::Float);
    }

    // ---- arithmetic ------------------------------------------------

    fn emit_int_arith(&mut self, kind: ArithKind, pc: u32) {
        match kind {
            ArithKind::Add | ArithKind::Sub | ArithKind::Mul => {
                let snapshot = self.vstack.clone();
                let (b, _) = self.pop();
                let (a, _) = self.pop();
                let (r, ovf) = match kind {
                    ArithKind::Add => self.checked_add(a, b),
                    ArithKind::Sub => self.checked_sub(a, b),
                    _ => self.checked_mul(a, b),
                };
                let cont = self.guard(ovf, pc, &snapshot);
                self.b.switch_to_block(cont);
                self.vstack.push((r, JitType::Int));
            }
            ArithKind::FloorDiv => self.emit_floordiv(pc),
            ArithKind::Mod => self.emit_mod(pc),
            ArithKind::And => self.emit_int_bitop(BitOp::And),
            ArithKind::Or => self.emit_int_bitop(BitOp::Or),
            ArithKind::Xor => self.emit_int_bitop(BitOp::Xor),
            ArithKind::TrueDiv => self.emit_int_truediv(pc),
            ArithKind::Pow => unreachable!("int pow is never admitted"),
        }
    }

    fn emit_int_bitop(&mut self, op: BitOp) {
        let (b, _) = self.pop();
        let (a, _) = self.pop();
        let r = match op {
            BitOp::And => self.b.ins().band(a, b),
            BitOp::Or => self.b.ins().bor(a, b),
            BitOp::Xor => self.b.ins().bxor(a, b),
        };
        self.vstack.push((r, JitType::Int));
    }

    fn emit_float_arith(&mut self, kind: ArithKind, pc: u32) {
        if matches!(kind, ArithKind::TrueDiv) {
            self.emit_float_truediv(pc);
            return;
        }
        // RFC 0069 WS2 — Python-semantics floor-div / mod ride the
        // registered helpers.
        if matches!(kind, ArithKind::FloorDiv | ArithKind::Mod) {
            self.emit_float_divmod_helper(kind, pc);
            return;
        }
        if matches!(kind, ArithKind::Pow) {
            self.emit_float_pow(pc);
            return;
        }
        let (b, _) = self.pop();
        let (a, _) = self.pop();
        let r = match kind {
            ArithKind::Add => self.b.ins().fadd(a, b),
            ArithKind::Sub => self.b.ins().fsub(a, b),
            ArithKind::Mul => self.b.ins().fmul(a, b),
            _ => unreachable!("non-jitable float arith reached lowering"),
        };
        self.vstack.push((r, JitType::Float));
    }

    /// Float `**` through the registered libm `pow` helper — the same
    /// function the interpreter's `float_pow` calls — for a positive
    /// base only, with a finite result: a zero, negative, or NaN base
    /// (division by zero, complex results) and any non-finite result
    /// (an overflow raises; an infinite exponent is the interpreter's
    /// to judge) deopt, and the interpreter re-executes the operation.
    fn emit_float_pow(&mut self, pc: u32) {
        let snapshot = self.vstack.clone();
        let (b, _) = self.pop();
        let (a, _) = self.pop();
        let z = self.b.ins().f64const(0.0);
        let not_pos = self.b.ins().fcmp(FloatCC::UnorderedOrLessThanOrEqual, a, z);
        let cont = self.guard(not_pos, pc, &snapshot);
        self.b.switch_to_block(cont);
        let sig = self.math_binary_helper_sig();
        let helper = self
            .b
            .ins()
            .iconst(self.ptr_ty, runtime::float_pow_helper_addr() as i64);
        let call = self.b.ins().call_indirect(sig, helper, &[a, b]);
        let r = self.b.inst_results(call)[0];
        // `|r| == inf` or NaN: `r - r` is NaN exactly then.
        let d = self.b.ins().fsub(r, r);
        let bad = self.b.ins().fcmp(FloatCC::Unordered, d, d);
        let cont = self.guard(bad, pc, &snapshot);
        self.b.switch_to_block(cont);
        self.vstack.push((r, JitType::Float));
    }

    fn emit_float_truediv(&mut self, pc: u32) {
        // Python raises ZeroDivisionError on float `/ 0.0`; deopt so the
        // interpreter raises with the right traceback.
        let snapshot = self.vstack.clone();
        let (b, _) = self.pop();
        let (a, _) = self.pop();
        let z = self.b.ins().f64const(0.0);
        let is_zero = self.b.ins().fcmp(FloatCC::Equal, b, z);
        let cont = self.guard(is_zero, pc, &snapshot);
        self.b.switch_to_block(cont);
        let r = self.b.ins().fdiv(a, b);
        self.vstack.push((r, JitType::Float));
    }

    fn emit_int_truediv(&mut self, pc: u32) {
        let snapshot = self.vstack.clone();
        let (b, _) = self.pop();
        let (a, _) = self.pop();
        let z = self.b.ins().iconst(types::I64, 0);
        let is_zero = self.b.ins().icmp(IntCC::Equal, b, z);
        // Converting an inexact integer operand to f64 before division
        // can round the final quotient incorrectly. Match the interpreter's
        // exact range and let its integer-division path handle larger values.
        // Wrapping addition maps [-EXACT, EXACT] to [0, 2 * EXACT]; every
        // other i64 maps outside that unsigned interval, including MIN.
        const EXACT: i64 = 1 << 53;
        let a_offset = self.b.ins().iadd_imm(a, EXACT);
        let b_offset = self.b.ins().iadd_imm(b, EXACT);
        let a_large = self
            .b
            .ins()
            .icmp_imm(IntCC::UnsignedGreaterThan, a_offset, 2 * EXACT);
        let b_large = self
            .b
            .ins()
            .icmp_imm(IntCC::UnsignedGreaterThan, b_offset, 2 * EXACT);
        let large = self.b.ins().bor(a_large, b_large);
        let invalid = self.b.ins().bor(is_zero, large);
        let cont = self.guard(invalid, pc, &snapshot);
        self.b.switch_to_block(cont);
        let af = self.b.ins().fcvt_from_sint(types::F64, a);
        let bf = self.b.ins().fcvt_from_sint(types::F64, b);
        let r = self.b.ins().fdiv(af, bf);
        self.vstack.push((r, JitType::Float));
    }

    fn emit_int_neg(&mut self, pc: u32) {
        let snapshot = self.vstack.clone();
        let (a, _) = self.pop();
        let min = self.b.ins().iconst(types::I64, i64::MIN);
        let ovf = self.b.ins().icmp(IntCC::Equal, a, min);
        let cont = self.guard(ovf, pc, &snapshot);
        self.b.switch_to_block(cont);
        let r = self.b.ins().ineg(a);
        self.vstack.push((r, JitType::Int));
    }

    /// Python floor division on `i64`. Deopts on a zero divisor or the
    /// `MIN / -1` overflow, then applies the round-toward-negative-
    /// infinity correction.
    fn emit_floordiv(&mut self, pc: u32) {
        let snapshot = self.vstack.clone();
        let (b, _) = self.pop();
        let (a, _) = self.pop();
        let should = self.div_guard_cond(a, b);
        let cont = self.guard(should, pc, &snapshot);
        self.b.switch_to_block(cont);

        let q = self.b.ins().sdiv(a, b);
        let r = self.b.ins().srem(a, b);
        // if r != 0 && (r<0) != (b<0) { q - 1 } else { q }
        let adj = self.floor_adjust(r, b);
        let qm1 = self.b.ins().iadd(q, adj);
        self.vstack.push((qm1, JitType::Int));
    }

    /// Python modulo on `i64` (result takes the divisor's sign).
    fn emit_mod(&mut self, pc: u32) {
        let snapshot = self.vstack.clone();
        let (b, _) = self.pop();
        let (a, _) = self.pop();
        let should = self.div_guard_cond(a, b);
        let cont = self.guard(should, pc, &snapshot);
        self.b.switch_to_block(cont);

        let r = self.b.ins().srem(a, b);
        // if r != 0 && (r<0) != (b<0) { r + b } else { r }
        let needs = self.floor_needs_adjust(r, b);
        let rplusb = self.b.ins().iadd(r, b);
        let res = self.b.ins().select(needs, rplusb, r);
        self.vstack.push((res, JitType::Int));
    }

    /// `b == 0 || (a == MIN && b == -1)`.
    fn div_guard_cond(&mut self, a: Value, b: Value) -> Value {
        let zero = self.b.ins().iconst(types::I64, 0);
        let is_zero = self.b.ins().icmp(IntCC::Equal, b, zero);
        let min = self.b.ins().iconst(types::I64, i64::MIN);
        let neg1 = self.b.ins().iconst(types::I64, -1);
        let a_min = self.b.ins().icmp(IntCC::Equal, a, min);
        let b_neg1 = self.b.ins().icmp(IntCC::Equal, b, neg1);
        let overflow = self.b.ins().band(a_min, b_neg1);
        self.b.ins().bor(is_zero, overflow)
    }

    /// `(r != 0) && ((r < 0) != (b < 0))` as an I8 boolean.
    fn floor_needs_adjust(&mut self, r: Value, b: Value) -> Value {
        let zero = self.b.ins().iconst(types::I64, 0);
        let r_nz = self.b.ins().icmp(IntCC::NotEqual, r, zero);
        let r_neg = self.b.ins().icmp(IntCC::SignedLessThan, r, zero);
        let b_neg = self.b.ins().icmp(IntCC::SignedLessThan, b, zero);
        let signs_differ = self.b.ins().bxor(r_neg, b_neg);
        self.b.ins().band(r_nz, signs_differ)
    }

    /// `-1` when the floor correction applies, else `0` (to add to `q`).
    fn floor_adjust(&mut self, r: Value, b: Value) -> Value {
        let needs = self.floor_needs_adjust(r, b);
        let neg1 = self.b.ins().iconst(types::I64, -1);
        let zero = self.b.ins().iconst(types::I64, 0);
        self.b.ins().select(needs, neg1, zero)
    }

    // ---- comparisons ----------------------------------------------

    fn emit_int_cmp(&mut self, kind: CmpKind) {
        let (b, _) = self.pop();
        let (a, _) = self.pop();
        let cc = match kind {
            CmpKind::Lt => IntCC::SignedLessThan,
            CmpKind::Le => IntCC::SignedLessThanOrEqual,
            CmpKind::Eq => IntCC::Equal,
            CmpKind::Ne => IntCC::NotEqual,
            CmpKind::Gt => IntCC::SignedGreaterThan,
            CmpKind::Ge => IntCC::SignedGreaterThanOrEqual,
        };
        let c = self.b.ins().icmp(cc, a, b);
        let r = self.b.ins().uextend(types::I64, c);
        self.vstack.push((r, JitType::Bool));
    }

    fn emit_float_cmp(&mut self, kind: CmpKind) {
        let (b, _) = self.pop();
        let (a, _) = self.pop();
        let cc = match kind {
            CmpKind::Lt => FloatCC::LessThan,
            CmpKind::Le => FloatCC::LessThanOrEqual,
            CmpKind::Eq => FloatCC::Equal,
            CmpKind::Ne => FloatCC::NotEqual,
            CmpKind::Gt => FloatCC::GreaterThan,
            CmpKind::Ge => FloatCC::GreaterThanOrEqual,
        };
        let c = self.b.ins().fcmp(cc, a, b);
        let r = self.b.ins().uextend(types::I64, c);
        self.vstack.push((r, JitType::Bool));
    }

    // ---- overflow helpers (portable signed-overflow detection) -----

    fn checked_add(&mut self, a: Value, b: Value) -> (Value, Value) {
        let r = self.b.ins().iadd(a, b);
        let axr = self.b.ins().bxor(a, r);
        let bxr = self.b.ins().bxor(b, r);
        let and = self.b.ins().band(axr, bxr);
        let zero = self.b.ins().iconst(types::I64, 0);
        let ovf = self.b.ins().icmp(IntCC::SignedLessThan, and, zero);
        (r, ovf)
    }

    fn checked_sub(&mut self, a: Value, b: Value) -> (Value, Value) {
        let r = self.b.ins().isub(a, b);
        let axb = self.b.ins().bxor(a, b);
        let axr = self.b.ins().bxor(a, r);
        let and = self.b.ins().band(axb, axr);
        let zero = self.b.ins().iconst(types::I64, 0);
        let ovf = self.b.ins().icmp(IntCC::SignedLessThan, and, zero);
        (r, ovf)
    }

    fn checked_mul(&mut self, a: Value, b: Value) -> (Value, Value) {
        let lo = self.b.ins().imul(a, b);
        let hi = self.b.ins().smulhi(a, b);
        let sign = self.b.ins().sshr_imm(lo, 63);
        let ovf = self.b.ins().icmp(IntCC::NotEqual, hi, sign);
        (lo, ovf)
    }

    // ---- deopt / side exits ---------------------------------------

    /// Emit `if cond { deopt(pc, snapshot) } else { cont }` and return
    /// the `cont` block (the caller continues lowering there).
    fn guard(&mut self, cond: Value, pc: u32, snapshot: &[(Value, JitType)]) -> Block {
        let cont = self.b.create_block();
        // An in-line method body restarts the call instead.
        if let Some(miss) = self.miss_redirect {
            self.b.ins().brif(cond, miss, &[], cont, &[]);
            return cont;
        }
        let se = self.b.create_block();
        self.b.ins().brif(cond, se, &[], cont, &[]);
        self.b.switch_to_block(se);
        self.emit_deopt(pc, snapshot);
        cont
    }

    fn emit_deopt(&mut self, pc: u32, snapshot: &[(Value, JitType)]) {
        self.emit_exit(pc, snapshot, JitStatus::Deopt);
    }

    /// Write back every managed local into the frame's locals buffer.
    /// Store the locals the body assigns back to the frame (the others
    /// still hold the values the entry loaded: reading one here would only
    /// keep it live to every exit and call).
    fn writeback_locals(&mut self) {
        let trusted = MemFlags::trusted();
        for slot in 0..self.vars.len() {
            if let Some(var) = self.vars[slot] {
                if !self.assignable.get(slot).copied().unwrap_or(true) {
                    continue;
                }
                let v = self.b.use_var(var);
                let off = (slot as i32) * 8;
                self.b.ins().store(trusted, v, self.locals_base, off);
            }
        }
    }

    /// Full side-exit: write back locals, spill `snapshot`, set
    /// `deopt_pc = pc`, and return `status`.
    fn emit_exit(&mut self, pc: u32, snapshot: &[(Value, JitType)], status: JitStatus) {
        let trusted = MemFlags::trusted();
        self.writeback_locals();
        // Spill the abstract stack bottom-to-top.
        for (idx, (val, ty)) in snapshot.iter().enumerate() {
            let voff = (idx as i32) * 8;
            self.b.ins().store(trusted, *val, self.spill_base, voff);
            let toff = (idx as i32) * 4;
            let tagv = self.b.ins().iconst(types::I32, Self::tag(*ty));
            self.b.ins().store(trusted, tagv, self.tags_base, toff);
        }
        let len = self.b.ins().iconst(types::I32, snapshot.len() as i64);
        self.b
            .ins()
            .store(trusted, len, self.frame_ptr, OFF_STACK_LEN);
        let pcv = self.b.ins().iconst(types::I32, i64::from(pc));
        self.b
            .ins()
            .store(trusted, pcv, self.frame_ptr, OFF_DEOPT_PC);
        let status = self.b.ins().iconst(types::I64, status as i64);
        self.b.ins().return_(&[status]);
    }

    // ---- helpers ---------------------------------------------------

    fn truth(&mut self, val: Value, ty: JitType) -> Value {
        match ty {
            JitType::Float => {
                let z = self.b.ins().f64const(0.0);
                self.b.ins().fcmp(FloatCC::NotEqual, val, z)
            }
            _ => {
                let z = self.b.ins().iconst(types::I64, 0);
                self.b.ins().icmp(IntCC::NotEqual, val, z)
            }
        }
    }

    fn pop(&mut self) -> (Value, JitType) {
        self.vstack.pop().expect("operand stack underflow in lower")
    }
}

#[derive(Clone, Copy)]
enum BitOp {
    And,
    Or,
    Xor,
}

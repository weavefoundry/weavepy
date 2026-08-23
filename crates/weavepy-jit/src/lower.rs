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

use cranelift_codegen::ir::condcodes::{FloatCC, IntCC};
use cranelift_codegen::ir::{
    types, AbiParam, Block, BlockArg, Function, InstBuilder, MemFlags, SigRef, Signature, Type,
    Value,
};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext, Variable};

use crate::ir::{ArithKind, CmpKind, MathFunc, MethodRet, TFunc, TOp, TStmt, TTerm};
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

/// Build the Cranelift function body for `tfunc` into `func`.
pub(crate) fn build_function(
    func: &mut Function,
    fbctx: &mut FunctionBuilderContext,
    tfunc: &TFunc,
    ptr_ty: Type,
) {
    let mut builder = FunctionBuilder::new(func, fbctx);
    let mut lc = Lowerer::new(&mut builder, tfunc, ptr_ty);
    lc.build();
    builder.seal_all_blocks();
    builder.finalize();
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
            call_method_sig: None,
            math_unary_sig: None,
            math_binary_sig: None,
            list_sig: None,
            pin_sig: None,
            quad_sig: None,
            poll_sig: None,
            poll_countdown: None,
            vstack: Vec::new(),
        }
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
            JitType::ListInt | JitType::ListFloat | JitType::ListObj => SlotTag::ListPin as i64,
            // RFC 0071 WS6 — `Str`/`Bytes` pins spill like object pins
            // (the rebuild resolves the pin to the real payload).
            JitType::Obj | JitType::Str | JitType::Bytes => SlotTag::ObjPin as i64,
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
        let stages_elements = self.tfunc.blocks.iter().any(|tb| {
            tb.stmts.iter().any(|s| {
                matches!(
                    s.op,
                    TOp::BuildList {
                        none_fill: false,
                        ..
                    }
                )
            })
        });
        if !self.tfunc.callee_spans.is_empty()
            || !self.tfunc.method_sites.is_empty()
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
        for slot in 0..self.tfunc.local_types.len() {
            if let Some(ty) = self.tfunc.local_types[slot] {
                let cl = Self::cl_ty(ty);
                let var = self.b.declare_var(cl);
                let off = (slot as i32) * 8;
                let v = self.b.ins().load(cl, trusted, self.locals_base, off);
                self.b.def_var(var, v);
                self.vars[slot] = Some(var);
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
            }
        }
        let block = self.tfunc.blocks[bi].clone();
        for stmt in &block.stmts {
            self.emit_stmt(*stmt);
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
                self.b.def_var(loop_var, cur);
                let next = self.b.ins().iadd_imm(cur, 1);
                self.b.def_var(cur_var, next);
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
                let got_b = self.b.create_block();
                let rest_b = self.b.create_block();
                let is_got = self.b.ins().icmp_imm(IntCC::Equal, status, 0);
                self.b.ins().brif(is_got, got_b, &[], rest_b, &[]);
                self.b.switch_to_block(got_b);
                let res =
                    self.b
                        .ins()
                        .load(Self::cl_ty(elem), trusted, self.frame_ptr, OFF_RET_BITS);
                self.b.def_var(loop_var, res);
                let next = self.b.ins().iadd_imm(idx, 1);
                self.b.def_var(idx_var, next);
                let bb = self.cl_blocks[body];
                self.b.ins().jump(bb, &[]);
                self.b.switch_to_block(rest_b);
                let deopt_b = self.b.create_block();
                let eb = self.cl_blocks[exit];
                let is_done = self.b.ins().icmp_imm(IntCC::Equal, status, 1);
                self.b.ins().brif(is_done, eb, &[], deopt_b, &[]);
                self.b.switch_to_block(deopt_b);
                self.emit_exit(pc, &[], JitStatus::Deopt);
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
                let sig = self.list_helper_sig();
                let helper = self
                    .b
                    .ins()
                    .iconst(self.ptr_ty, runtime::iter_next_helper_addr() as i64);
                let tag = self.b.ins().iconst(types::I64, Self::tag(elem));
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
                self.b.def_var(loop_var, res);
                let bb = self.cl_blocks[body];
                self.b.ins().jump(bb, &[]);
                self.b.switch_to_block(rest_b);
                let more_b = self.b.create_block();
                let eb = self.cl_blocks[exit];
                let is_done = self.b.ins().icmp_imm(IntCC::Equal, status, 1);
                self.b.ins().brif(is_done, eb, &[], more_b, &[]);
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
                self.emit_exit(store_pc, &[(elem_pin, JitType::Obj)], JitStatus::Deopt);
                self.b.switch_to_block(hdr_b);
                let raise_b = self.b.create_block();
                let deopt_b = self.b.create_block();
                let is_raise = self.b.ins().icmp_imm(IntCC::Equal, status, 4);
                self.b.ins().brif(is_raise, raise_b, &[], deopt_b, &[]);
                self.b.switch_to_block(raise_b);
                self.emit_exit(pc, &[], JitStatus::Raised);
                self.b.switch_to_block(deopt_b);
                self.emit_exit(pc, &[], JitStatus::Deopt);
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
                self.b.def_var(var, v);
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
            TOp::Dup => {
                let top = *self.vstack.last().expect("dup on empty");
                self.vstack.push(top);
            }
            TOp::Swap2 => {
                let len = self.vstack.len();
                self.vstack.swap(len - 1, len - 2);
            }
            TOp::IntToFloatTos { guarded } => {
                let depth = self.vstack.len() - 1;
                self.emit_int_to_float(depth, guarded, stmt.pc);
            }
            TOp::IntToFloatSecond { guarded } => {
                let depth = self.vstack.len() - 2;
                self.emit_int_to_float(depth, guarded, stmt.pc);
            }
            TOp::CallPy { token, argc, ret } => self.emit_call_py(token, argc, ret, stmt.pc),
            TOp::ListGet { elem } => self.emit_list_get(elem, stmt.pc),
            TOp::ListSet => self.emit_list_set(stmt.pc),
            TOp::ListLen => self.emit_list_len(stmt.pc),
            TOp::ListAppend => self.emit_list_append(stmt.pc),
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
                self.emit_call_method(token, argc, ret, stmt.pc);
            }
            TOp::StrEq { negate } => self.emit_str_eq(negate, stmt.pc),
            TOp::StrLen => self.emit_pin_len(runtime::str_len_helper_addr(), stmt.pc),
            TOp::BytesLen => self.emit_pin_len(runtime::bytes_len_helper_addr(), stmt.pc),
            TOp::BytesGetItem => self.emit_bytes_get(stmt.pc),
            TOp::IterCapture { iter_slot } => self.emit_iter_capture(iter_slot, stmt.pc),
            TOp::BuildList { n, elem, none_fill } => {
                self.emit_build_list(n, elem, none_fill, stmt.pc);
            }
            TOp::ListRepeat => self.emit_list_repeat(stmt.pc),
            TOp::ListSlice { start, stop } => self.emit_list_slice(start, stop, stmt.pc),
        }
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
        self.b.def_var(iter_var, pin);
    }

    /// RFC 0071 WS4 — `BUILD_LIST k`: the elements are staged through
    /// the marshal buffer (like `CallPy` arguments); `none_fill`
    /// passes an empty buffer and the helper writes `n` `None`s. The
    /// helper answers the fresh pin index, negative on cap pressure
    /// (deopt: the interpreter re-executes the `BUILD_LIST`).
    fn emit_build_list(&mut self, n: u32, elem: JitType, none_fill: bool, pc: u32) {
        let trusted = MemFlags::trusted();
        // The deopt re-executes the `BUILD_LIST`, so the spill must
        // hold all `n` elements: the natives still on the stack, or —
        // for `none_fill` — materialized `None` markers (`-1` on the
        // object lane), which never occupied native slots.
        let mut snapshot = self.vstack.clone();
        if none_fill {
            for _ in 0..n {
                let none = self.b.ins().iconst(types::I64, -1);
                snapshot.push((none, JitType::Obj));
            }
        } else {
            let base = self.vstack.len() - n as usize;
            for (j, &(v, _)) in self.vstack[base..].iter().enumerate() {
                let voff = (j as i32) * 8;
                self.b.ins().store(trusted, v, self.call_args_base, voff);
            }
            self.vstack.truncate(base);
        }
        let sig = self.quad_helper_sig();
        let helper = self
            .b
            .ins()
            .iconst(self.ptr_ty, runtime::build_list_helper_addr() as i64);
        let nv = self.b.ins().iconst(types::I64, i64::from(n));
        let tag = self.b.ins().iconst(types::I64, Self::tag(elem));
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

    /// RFC 0071 WS4 — `xs[a:b]` (unit step) through
    /// `wpjit_list_slice`. Absent bounds (the `None` markers of the
    /// erased `BUILD_SLICE`) pass as `i64::MIN`; present bounds pop
    /// `stop` above `start`. A negative status deopts at the *erased
    /// `BUILD_SLICE`'s* pc (`pc` here — the analyzer guarantees the
    /// `BINARY_SUBSCR` immediately follows it), with all three bound
    /// operands spilled — absent ones as materialized `None`s — so
    /// the interpreter rebuilds the slice object and executes the
    /// subscript generically.
    fn emit_list_slice(&mut self, start: bool, stop: bool, pc: u32) {
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
        // stop, step=None], `None` bounds as object-lane `-1`.
        let mut snapshot = self.vstack.clone();
        snapshot.push((pin, lane));
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
        snapshot.push((none, JitType::Obj));
        let sig = self.quad_helper_sig();
        let helper = self
            .b
            .ins()
            .iconst(self.ptr_ty, runtime::list_slice_helper_addr() as i64);
        let call = self
            .b
            .ins()
            .call_indirect(sig, helper, &[self.frame_ptr, pin, start_v, stop_v]);
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
        self.vstack.truncate(base);
        let (pin, _) = self.pop();
        let snapshot = self.vstack.clone();

        self.writeback_locals();

        let expect = match ret {
            MethodRet::None => SlotTag::None as i64,
            MethodRet::Scalar(t) => Self::tag(t),
        };
        let sig = self.call_method_helper_sig();
        let helper = self
            .b
            .ins()
            .iconst(self.ptr_ty, runtime::call_method_helper_addr() as i64);
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
            self.vstack.push((res, t));
        }
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
    /// value is staged through `ret_bits` (same trick as `ListSet`);
    /// the analyzer guaranteed its lane matches the pinned element
    /// lane. A non-zero status (defensive) deopts at the `CALL` pc,
    /// where the enclosing method span rebuilds the receiver as the
    /// bound `list.append` and the interpreter re-executes the call.
    fn emit_list_append(&mut self, pc: u32) {
        let trusted = MemFlags::trusted();
        let snapshot = self.vstack.clone();
        let (val, _) = self.pop();
        let (pin, _) = self.pop();
        self.b
            .ins()
            .store(trusted, val, self.frame_ptr, OFF_RET_BITS);
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
        // `append` returns `None`, which the following `POP_TOP`
        // consumes — neither ever exists on the native stack.
    }

    /// RFC 0065 WS5 — pinned-instance attribute read via
    /// `wpjit_attr_get`. A non-zero status deopts at this pc with the
    /// receiver spilled (its `ObjPin` tag rebuilds the real instance),
    /// so the interpreter re-executes the `LOAD_ATTR` generically.
    fn emit_attr_get(&mut self, site: u32, out: JitType, pc: u32) {
        let trusted = MemFlags::trusted();
        let snapshot = self.vstack.clone();
        let (pin, _) = self.pop();
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
        self.vstack.push((res, out));
    }

    /// RFC 0065 WS5 — pinned-instance attribute write via
    /// `wpjit_attr_set`. The value is staged through `ret_bits`; a
    /// non-zero status deopts at this pc with `[value, receiver]`
    /// spilled, so the interpreter re-executes the `STORE_ATTR`.
    fn emit_attr_set(&mut self, site: u32, pc: u32) {
        let trusted = MemFlags::trusted();
        let snapshot = self.vstack.clone();
        let (pin, _) = self.pop();
        let (val, _) = self.pop();
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
        self.emit_exit(header_pc, &[], JitStatus::Deopt);

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
        let (val, _) = self.pop();
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
    fn emit_call_py(&mut self, token: u32, argc: u8, ret: JitType, pc: u32) {
        let trusted = MemFlags::trusted();
        let n = argc as usize;
        let base = self.vstack.len() - n;
        for (j, &(v, ty)) in self.vstack[base..].iter().enumerate() {
            let voff = (j as i32) * 8;
            let toff = (j as i32) * 4;
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

        let sig = self.call_py_sig();
        let helper = self
            .b
            .ins()
            .iconst(self.ptr_ty, runtime::call_py_helper_addr() as i64);
        let tokenv = self.b.ins().iconst(types::I32, i64::from(token));
        let argcv = self.b.ins().iconst(types::I32, i64::from(argc));
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
        let cont = self.guard(is_zero, pc, &snapshot);
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
        let se = self.b.create_block();
        let cont = self.b.create_block();
        self.b.ins().brif(cond, se, &[], cont, &[]);
        self.b.switch_to_block(se);
        self.emit_deopt(pc, snapshot);
        cont
    }

    fn emit_deopt(&mut self, pc: u32, snapshot: &[(Value, JitType)]) {
        self.emit_exit(pc, snapshot, JitStatus::Deopt);
    }

    /// Write back every managed local into the frame's locals buffer.
    fn writeback_locals(&mut self) {
        let trusted = MemFlags::trusted();
        for slot in 0..self.vars.len() {
            if let Some(var) = self.vars[slot] {
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

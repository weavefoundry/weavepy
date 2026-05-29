//! Lower the typed IR ([`TFunc`]) to a Cranelift function.
//!
//! Locals become Cranelift *variables* (the SSA builder inserts phis at
//! merges); the operand stack is an explicit `Vec` of SSA values, which
//! the v1 subset guarantees is empty at every block boundary. Integer
//! arithmetic is emitted with explicit overflow / divide-by-zero checks
//! that branch to per-op *side-exit* blocks; a side exit writes the live
//! locals + spilled stack back into the [`JitFrame`] and returns
//! [`JitStatus::Deopt`] so the interpreter resumes at the exact pc.

use cranelift_codegen::ir::condcodes::{FloatCC, IntCC};
use cranelift_codegen::ir::{types, Block, Function, InstBuilder, MemFlags, Type, Value};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext, Variable};

use crate::ir::{ArithKind, CmpKind, TFunc, TOp, TStmt, TTerm};
use crate::runtime::{JitFrame, JitStatus, SlotTag};
use crate::value::JitType;

const OFF_LOCALS: i32 = core::mem::offset_of!(JitFrame, locals) as i32;
const OFF_RET_BITS: i32 = core::mem::offset_of!(JitFrame, ret_bits) as i32;
const OFF_RET_TAG: i32 = core::mem::offset_of!(JitFrame, ret_tag) as i32;
const OFF_DEOPT_PC: i32 = core::mem::offset_of!(JitFrame, deopt_pc) as i32;
const OFF_STACK_SPILL: i32 = core::mem::offset_of!(JitFrame, stack_spill) as i32;
const OFF_STACK_TAGS: i32 = core::mem::offset_of!(JitFrame, stack_tags) as i32;
const OFF_STACK_LEN: i32 = core::mem::offset_of!(JitFrame, stack_len) as i32;

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

        // One Cranelift block per TBlock.
        self.cl_blocks = (0..self.tfunc.blocks.len())
            .map(|_| self.b.create_block())
            .collect();

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

        let entry_target = self.cl_blocks[self.tfunc.entry_block];
        self.b.ins().jump(entry_target, &[]);

        // Emit each block body.
        for bi in 0..self.tfunc.blocks.len() {
            let cl = self.cl_blocks[bi];
            self.b.switch_to_block(cl);
            self.vstack.clear();
            self.emit_block(bi);
        }
    }

    fn emit_block(&mut self, bi: usize) {
        let block = self.tfunc.blocks[bi].clone();
        for stmt in &block.stmts {
            self.emit_stmt(*stmt);
        }
        match block.term {
            TTerm::Return => self.emit_return(),
            TTerm::Jump(t) => {
                let target = self.cl_blocks[t];
                self.b.ins().jump(target, &[]);
            }
            TTerm::BranchFalse {
                target,
                fallthrough,
            } => {
                let (cond, ty) = self.pop();
                let truthy = self.truth(cond, ty);
                let tb = self.cl_blocks[target];
                let fb = self.cl_blocks[fallthrough];
                // if truthy → fallthrough else → target.
                self.b.ins().brif(truthy, fb, &[], tb, &[]);
            }
            TTerm::BranchTrue {
                target,
                fallthrough,
            } => {
                let (cond, ty) = self.pop();
                let truthy = self.truth(cond, ty);
                let tb = self.cl_blocks[target];
                let fb = self.cl_blocks[fallthrough];
                self.b.ins().brif(truthy, tb, &[], fb, &[]);
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
        }
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
        let trusted = MemFlags::trusted();
        // Write back every managed local.
        for (slot, var) in self.vars.iter().enumerate() {
            if let Some(var) = *var {
                let v = self.b.use_var(var);
                let off = (slot as i32) * 8;
                self.b.ins().store(trusted, v, self.locals_base, off);
            }
        }
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
        let status = self.b.ins().iconst(types::I64, JitStatus::Deopt as i64);
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

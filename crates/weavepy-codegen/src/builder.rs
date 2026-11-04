//! Building a [`Function`], with Cranelift's `FunctionBuilder` names.

use crate::ir::{
    condcodes::{FloatCC, IntCC},
    types, Alu, Block, BlockArg, BlockData, Edge, Div, Ext, Fop, Function, Inst, InstData,
    JumpTable, JumpTableData, MemFlags, Op, Overflow, SigRef, Signature, Type, Value, ValueDef,
};

/// Builds a function's blocks and instructions.
#[derive(Debug)]
pub struct FunctionBuilder<'a> {
    pub func: &'a mut Function,
    cur: Option<Block>,
}

impl<'a> FunctionBuilder<'a> {
    pub fn new(func: &'a mut Function) -> Self {
        FunctionBuilder { func, cur: None }
    }

    pub fn create_block(&mut self) -> Block {
        let b = Block(self.func.dfg.blocks.len() as u32);
        self.func.dfg.blocks.push(BlockData::default());
        b
    }

    pub fn switch_to_block(&mut self, block: Block) {
        let data = &mut self.func.dfg.blocks[block.index()];
        if !data.inserted {
            data.inserted = true;
            self.func.layout.push(block);
        }
        self.cur = Some(block);
    }

    pub fn current_block(&self) -> Option<Block> {
        self.cur
    }

    pub fn set_cold_block(&mut self, block: Block) {
        self.func.dfg.blocks[block.index()].cold = true;
    }

    pub fn append_block_param(&mut self, block: Block, ty: Type) -> Value {
        let n = self.func.dfg.blocks[block.index()].params.len() as u32;
        let v = self.func.dfg.make_value(ty, ValueDef::Param(block, n));
        self.func.dfg.blocks[block.index()].params.push(v);
        v
    }

    pub fn append_block_params_for_function_params(&mut self, block: Block) {
        let tys: Vec<Type> = self
            .func
            .signature
            .params
            .iter()
            .map(|p| p.value_type)
            .collect();
        for ty in tys {
            self.append_block_param(block, ty);
        }
    }

    pub fn block_params(&self, block: Block) -> &[Value] {
        &self.func.dfg.blocks[block.index()].params
    }

    pub fn seal_block(&mut self, _block: Block) {}

    pub fn seal_all_blocks(&mut self) {}

    pub fn finalize(self) {}

    pub fn import_signature(&mut self, sig: Signature) -> SigRef {
        if let Some(i) = self.func.dfg.sigs.iter().position(|s| *s == sig) {
            return SigRef(i as u32);
        }
        self.func.dfg.sigs.push(sig);
        SigRef(self.func.dfg.sigs.len() as u32 - 1)
    }

    pub fn create_jump_table(&mut self, data: JumpTableData) -> JumpTable {
        self.func.dfg.tables.push(data);
        JumpTable(self.func.dfg.tables.len() as u32 - 1)
    }

    pub fn inst_results(&self, inst: Inst) -> &[Value] {
        self.func.dfg.results(inst)
    }

    pub fn ins<'s>(&'s mut self) -> FuncInstBuilder<'s, 'a> {
        FuncInstBuilder { b: self }
    }

    fn push(&mut self, op: Op, args: &[Value], results: &[Type]) -> Inst {
        let block = self.cur.expect("an instruction outside a block");
        let dfg = &mut self.func.dfg;
        let inst = Inst(dfg.insts.len() as u32);
        let args = dfg.intern(args);
        let start = dfg.pool.len() as u32;
        for (i, &ty) in results.iter().enumerate() {
            let v = dfg.make_value(ty, ValueDef::Result(inst, i as u32));
            dfg.pool.push(v);
        }
        dfg.insts.push(InstData {
            op,
            args,
            results: (start, dfg.pool.len() as u32),
            ty: results.first().copied().unwrap_or(types::I64),
        });
        dfg.blocks[block.index()].insts.push(inst);
        inst
    }

    fn push1(&mut self, op: Op, args: &[Value], ty: Type) -> Value {
        let inst = self.push(op, args, &[ty]);
        self.func.dfg.results(inst)[0]
    }

    fn edge(&mut self, block: Block, args: &[BlockArg]) -> Edge {
        let start = self.func.dfg.pool.len() as u32;
        self.func.dfg.pool.extend(args.iter().map(|a| a.0));
        Edge {
            block,
            args: (start, self.func.dfg.pool.len() as u32),
        }
    }

    fn ty(&self, v: Value) -> Type {
        self.func.dfg.value_type(v)
    }
}

/// An immediate in a type's own width, sign-extended (the form every
/// consumer of it expects).
pub(crate) fn canon_imm(ty: Type, imm: i64) -> i64 {
    match ty.bits() {
        8 => i64::from(imm as i8),
        16 => i64::from(imm as i16),
        32 => i64::from(imm as i32),
        _ => imm,
    }
}

/// Inserts instructions at the end of the current block.
#[derive(Debug)]
pub struct FuncInstBuilder<'s, 'a> {
    b: &'s mut FunctionBuilder<'a>,
}

macro_rules! binary {
    ($($name:ident => $op:expr),* $(,)?) => {
        $(pub fn $name(self, x: Value, y: Value) -> Value {
            let ty = self.b.ty(x);
            self.b.push1($op, &[x, y], ty)
        })*
    };
}

macro_rules! binary_imm {
    ($($name:ident => $alu:expr),* $(,)?) => {
        $(pub fn $name(self, x: Value, imm: i64) -> Value {
            let ty = self.b.ty(x);
            let imm = canon_imm(ty, imm);
            self.b.push1(Op::AluImm($alu, imm), &[x], ty)
        })*
    };
}

macro_rules! load_ext {
    ($($name:ident => ($bytes:expr, $ext:expr)),* $(,)?) => {
        $(pub fn $name(self, ty: Type, _flags: MemFlags, p: Value, off: impl Into<i32>) -> Value {
            self.b.push1(Op::Load { bytes: $bytes, ext: $ext, off: off.into() }, &[p], ty)
        })*
    };
}

macro_rules! store_narrow {
    ($($name:ident => $bytes:expr),* $(,)?) => {
        $(pub fn $name(self, _flags: MemFlags, x: Value, p: Value, off: impl Into<i32>) -> Inst {
            self.b.push(Op::Store { bytes: $bytes, off: off.into() }, &[x, p], &[])
        })*
    };
}

impl FuncInstBuilder<'_, '_> {
    pub fn iconst(self, ty: Type, imm: i64) -> Value {
        let imm = canon_imm(ty, imm);
        self.b.push1(Op::Iconst(imm), &[], ty)
    }

    pub fn f64const(self, x: f64) -> Value {
        self.b
            .push1(Op::Fconst(x.to_bits()), &[], types::F64)
    }

    pub fn load(self, ty: Type, _flags: MemFlags, p: Value, off: impl Into<i32>) -> Value {
        self.b.push1(
            Op::Load {
                bytes: ty.bytes() as u8,
                ext: Ext::None,
                off: off.into(),
            },
            &[p],
            ty,
        )
    }

    load_ext! {
        uload8 => (1, Ext::Zero),
        sload8 => (1, Ext::Sign),
        uload16 => (2, Ext::Zero),
        sload16 => (2, Ext::Sign),
    }

    pub fn uload32(self, _flags: MemFlags, p: Value, off: impl Into<i32>) -> Value {
        self.b.push1(
            Op::Load {
                bytes: 4,
                ext: Ext::Zero,
                off: off.into(),
            },
            &[p],
            types::I64,
        )
    }

    pub fn sload32(self, _flags: MemFlags, p: Value, off: impl Into<i32>) -> Value {
        self.b.push1(
            Op::Load {
                bytes: 4,
                ext: Ext::Sign,
                off: off.into(),
            },
            &[p],
            types::I64,
        )
    }

    pub fn store(self, _flags: MemFlags, x: Value, p: Value, off: impl Into<i32>) -> Inst {
        let bytes = self.b.ty(x).bytes() as u8;
        self.b.push(
            Op::Store {
                bytes,
                off: off.into(),
            },
            &[x, p],
            &[],
        )
    }

    store_narrow! {
        istore8 => 1,
        istore16 => 2,
        istore32 => 4,
    }

    pub fn icmp(self, cc: IntCC, x: Value, y: Value) -> Value {
        self.b.push1(Op::Icmp(cc), &[x, y], types::I8)
    }

    pub fn icmp_imm(self, cc: IntCC, x: Value, imm: i64) -> Value {
        let imm = canon_imm(self.b.ty(x), imm);
        self.b.push1(Op::IcmpImm(cc, imm), &[x], types::I8)
    }

    pub fn fcmp(self, cc: FloatCC, x: Value, y: Value) -> Value {
        self.b.push1(Op::Fcmp(cc), &[x, y], types::I8)
    }

    binary! {
        iadd => Op::Alu(Alu::Add),
        isub => Op::Alu(Alu::Sub),
        imul => Op::Alu(Alu::Mul),
        band => Op::Alu(Alu::And),
        bor => Op::Alu(Alu::Or),
        bxor => Op::Alu(Alu::Xor),
        ishl => Op::Alu(Alu::Shl),
        ushr => Op::Alu(Alu::Ushr),
        sshr => Op::Alu(Alu::Sshr),
        udiv => Op::Div(Div::Udiv),
        urem => Op::Div(Div::Urem),
        sdiv => Op::Div(Div::Sdiv),
        srem => Op::Div(Div::Srem),
        smulhi => Op::Smulhi,
        fadd => Op::Fop(Fop::Add),
        fsub => Op::Fop(Fop::Sub),
        fmul => Op::Fop(Fop::Mul),
        fdiv => Op::Fop(Fop::Div),
    }

    binary_imm! {
        iadd_imm => Alu::Add,
        imul_imm => Alu::Mul,
        band_imm => Alu::And,
        bor_imm => Alu::Or,
        bxor_imm => Alu::Xor,
        ishl_imm => Alu::Shl,
        ushr_imm => Alu::Ushr,
        sshr_imm => Alu::Sshr,
    }

    pub fn isub_imm(self, x: Value, imm: i64) -> Value {
        self.iadd_imm(x, imm.wrapping_neg())
    }

    pub fn bnot(self, x: Value) -> Value {
        let ty = self.b.ty(x);
        self.b.push1(Op::Bnot, &[x], ty)
    }

    pub fn ineg(self, x: Value) -> Value {
        let ty = self.b.ty(x);
        self.b.push1(Op::Ineg, &[x], ty)
    }

    pub fn fneg(self, x: Value) -> Value {
        let ty = self.b.ty(x);
        self.b.push1(Op::Fneg, &[x], ty)
    }

    pub fn fabs(self, x: Value) -> Value {
        let ty = self.b.ty(x);
        self.b.push1(Op::Fabs, &[x], ty)
    }

    pub fn sqrt(self, x: Value) -> Value {
        let ty = self.b.ty(x);
        self.b.push1(Op::Sqrt, &[x], ty)
    }

    pub fn select(self, c: Value, x: Value, y: Value) -> Value {
        let ty = self.b.ty(x);
        self.b.push1(Op::Select, &[c, x, y], ty)
    }

    pub fn ireduce(self, ty: Type, x: Value) -> Value {
        self.b.push1(Op::Ireduce, &[x], ty)
    }

    pub fn uextend(self, ty: Type, x: Value) -> Value {
        self.b.push1(Op::Uextend, &[x], ty)
    }

    pub fn sextend(self, ty: Type, x: Value) -> Value {
        self.b.push1(Op::Sextend, &[x], ty)
    }

    fn overflow(self, op: Overflow, x: Value, y: Value) -> (Value, Value) {
        let ty = self.b.ty(x);
        let inst = self.b.push(Op::Overflow(op), &[x, y], &[ty, types::I8]);
        let r = self.b.func.dfg.results(inst);
        (r[0], r[1])
    }

    pub fn sadd_overflow(self, x: Value, y: Value) -> (Value, Value) {
        self.overflow(Overflow::Add, x, y)
    }

    pub fn ssub_overflow(self, x: Value, y: Value) -> (Value, Value) {
        self.overflow(Overflow::Sub, x, y)
    }

    pub fn smul_overflow(self, x: Value, y: Value) -> (Value, Value) {
        self.overflow(Overflow::Mul, x, y)
    }

    pub fn bitcast(self, ty: Type, _flags: MemFlags, x: Value) -> Value {
        self.b.push1(Op::Bitcast, &[x], ty)
    }

    pub fn fcvt_from_sint(self, ty: Type, x: Value) -> Value {
        self.b.push1(Op::FcvtFromSint, &[x], ty)
    }

    pub fn call_indirect(self, sig: SigRef, callee: Value, args: &[Value]) -> Inst {
        let rets: Vec<Type> = self.b.func.dfg.sigs[sig.index()]
            .returns
            .iter()
            .map(|r| r.value_type)
            .collect();
        let mut all = Vec::with_capacity(args.len() + 1);
        all.push(callee);
        all.extend_from_slice(args);
        self.b.push(Op::CallIndirect(sig), &all, &rets)
    }

    pub fn jump(self, block: Block, args: &[BlockArg]) -> Inst {
        let e = self.b.edge(block, args);
        self.b.push(Op::Jump(e), &[], &[])
    }

    pub fn brif(
        self,
        c: Value,
        then_block: Block,
        then_args: &[BlockArg],
        else_block: Block,
        else_args: &[BlockArg],
    ) -> Inst {
        let t = self.b.edge(then_block, then_args);
        let e = self.b.edge(else_block, else_args);
        self.b.push(Op::Brif(t, e), &[c], &[])
    }

    pub fn br_table(self, x: Value, jt: JumpTable) -> Inst {
        self.b.push(Op::BrTable(jt), &[x], &[])
    }

    pub fn return_(self, rets: &[Value]) -> Inst {
        self.b.push(Op::Return, rets, &[])
    }
}

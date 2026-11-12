//! The intermediate representation: SSA values in basic blocks with block
//! parameters, the subset of Cranelift's IR that the frame JIT lowers to,
//! under Cranelift's names.

use std::fmt;

/// A value's type.
#[derive(Copy, Clone, PartialEq, Eq, Hash)]
pub struct Type(u8);

/// The value types.
pub mod types {
    pub use super::Type;
    pub const I8: Type = Type(1);
    pub const I16: Type = Type(2);
    pub const I32: Type = Type(3);
    pub const I64: Type = Type(4);
    pub const F32: Type = Type(5);
    pub const F64: Type = Type(6);
}

impl Type {
    /// The type's width in bytes.
    pub fn bytes(self) -> u32 {
        match self.0 {
            1 => 1,
            2 => 2,
            3 | 5 => 4,
            _ => 8,
        }
    }

    pub fn bits(self) -> u32 {
        self.bytes() * 8
    }

    pub fn is_float(self) -> bool {
        self.0 >= 5
    }

    pub fn is_int(self) -> bool {
        !self.is_float()
    }
}

impl fmt::Debug for Type {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self.0 {
            1 => "i8",
            2 => "i16",
            3 => "i32",
            4 => "i64",
            5 => "f32",
            _ => "f64",
        })
    }
}

impl fmt::Display for Type {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

macro_rules! entity {
    ($name:ident, $prefix:literal) => {
        #[derive(Copy, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name(pub(crate) u32);

        impl $name {
            pub fn from_u32(n: u32) -> Self {
                $name(n)
            }

            pub fn as_u32(self) -> u32 {
                self.0
            }

            /// The entity's number (Cranelift's `EntityRef::index`).
            pub fn index(self) -> usize {
                self.0 as usize
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, concat!($prefix, "{}"), self.0)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt::Debug::fmt(self, f)
            }
        }
    };
}

entity!(Value, "v");
entity!(Block, "block");
entity!(Inst, "inst");
entity!(SigRef, "sig");
entity!(JumpTable, "jt");
entity!(Variable, "var");
entity!(StackSlot, "ss");
entity!(FuncRef, "fn");

/// What a stack slot is for (only explicit slots exist here).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum StackSlotKind {
    ExplicitSlot,
}

/// A stack slot's size and alignment (`1 << align_shift` bytes).
#[derive(Copy, Clone, Debug)]
pub struct StackSlotData {
    pub kind: StackSlotKind,
    pub size: u32,
    pub align_shift: u8,
}

impl StackSlotData {
    pub fn new(kind: StackSlotKind, size: u32, align_shift: u8) -> Self {
        StackSlotData {
            kind,
            size,
            align_shift,
        }
    }
}

/// A directly called function: code at a known address, or the function
/// being compiled.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum FuncTarget {
    Addr(usize),
    SelfFn,
}

#[derive(Copy, Clone, Debug)]
pub struct ExtFuncData {
    pub target: FuncTarget,
    pub sig: SigRef,
}

/// Memory access flags: accepted for Cranelift's signature, and ignored
/// (every access the frame JIT makes is aligned and can't trap).
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct MemFlags(u8);

impl MemFlags {
    pub const fn new() -> Self {
        MemFlags(0)
    }

    pub const fn trusted() -> Self {
        MemFlags(1)
    }
}

/// Integer comparisons.
pub mod condcodes {
    #[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
    pub enum IntCC {
        Equal,
        NotEqual,
        SignedLessThan,
        SignedGreaterThanOrEqual,
        SignedGreaterThan,
        SignedLessThanOrEqual,
        UnsignedLessThan,
        UnsignedGreaterThanOrEqual,
        UnsignedGreaterThan,
        UnsignedLessThanOrEqual,
    }

    impl IntCC {
        /// The condition with its operands swapped (`a < b` is `b > a`).
        pub fn swapped(self) -> IntCC {
            use IntCC::*;
            match self {
                Equal => Equal,
                NotEqual => NotEqual,
                SignedLessThan => SignedGreaterThan,
                SignedGreaterThanOrEqual => SignedLessThanOrEqual,
                SignedGreaterThan => SignedLessThan,
                SignedLessThanOrEqual => SignedGreaterThanOrEqual,
                UnsignedLessThan => UnsignedGreaterThan,
                UnsignedGreaterThanOrEqual => UnsignedLessThanOrEqual,
                UnsignedGreaterThan => UnsignedLessThan,
                UnsignedLessThanOrEqual => UnsignedGreaterThanOrEqual,
            }
        }

        /// The condition's negation.
        pub fn inverse(self) -> IntCC {
            use IntCC::*;
            match self {
                Equal => NotEqual,
                NotEqual => Equal,
                SignedLessThan => SignedGreaterThanOrEqual,
                SignedGreaterThanOrEqual => SignedLessThan,
                SignedGreaterThan => SignedLessThanOrEqual,
                SignedLessThanOrEqual => SignedGreaterThan,
                UnsignedLessThan => UnsignedGreaterThanOrEqual,
                UnsignedGreaterThanOrEqual => UnsignedLessThan,
                UnsignedGreaterThan => UnsignedLessThanOrEqual,
                UnsignedLessThanOrEqual => UnsignedGreaterThan,
            }
        }
    }

    /// Float comparisons, as Cranelift defines them: `NotEqual` holds for
    /// unordered operands, and the orderings don't.
    #[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
    pub enum FloatCC {
        Ordered,
        Unordered,
        Equal,
        NotEqual,
        OrderedNotEqual,
        UnorderedOrEqual,
        LessThan,
        LessThanOrEqual,
        GreaterThan,
        GreaterThanOrEqual,
        UnorderedOrLessThan,
        UnorderedOrLessThanOrEqual,
        UnorderedOrGreaterThan,
        UnorderedOrGreaterThanOrEqual,
    }

    impl FloatCC {
        pub fn inverse(self) -> FloatCC {
            use FloatCC::*;
            match self {
                Ordered => Unordered,
                Unordered => Ordered,
                Equal => NotEqual,
                NotEqual => Equal,
                OrderedNotEqual => UnorderedOrEqual,
                UnorderedOrEqual => OrderedNotEqual,
                LessThan => UnorderedOrGreaterThanOrEqual,
                LessThanOrEqual => UnorderedOrGreaterThan,
                GreaterThan => UnorderedOrLessThanOrEqual,
                GreaterThanOrEqual => UnorderedOrLessThan,
                UnorderedOrLessThan => GreaterThanOrEqual,
                UnorderedOrLessThanOrEqual => GreaterThan,
                UnorderedOrGreaterThan => LessThanOrEqual,
                UnorderedOrGreaterThanOrEqual => LessThan,
            }
        }
    }
}

use condcodes::{FloatCC, IntCC};

/// A calling convention.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum CallConv {
    SystemV,
    WindowsFastcall,
    AppleAarch64,
    /// AAPCS64 off Apple platforms.
    Aapcs64,
}

impl CallConv {
    /// The host's C calling convention.
    pub fn host() -> CallConv {
        if cfg!(target_arch = "aarch64") {
            if cfg!(target_vendor = "apple") {
                CallConv::AppleAarch64
            } else {
                CallConv::Aapcs64
            }
        } else if cfg!(windows) {
            CallConv::WindowsFastcall
        } else {
            CallConv::SystemV
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct AbiParam {
    pub value_type: Type,
}

impl AbiParam {
    pub fn new(value_type: Type) -> Self {
        AbiParam { value_type }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Signature {
    pub params: Vec<AbiParam>,
    pub returns: Vec<AbiParam>,
    pub call_conv: CallConv,
}

impl Signature {
    pub fn new(call_conv: CallConv) -> Self {
        Signature {
            params: Vec::new(),
            returns: Vec::new(),
            call_conv,
        }
    }
}

/// A branch's argument.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct BlockArg(pub Value);

impl From<Value> for BlockArg {
    fn from(v: Value) -> Self {
        BlockArg(v)
    }
}

/// A branch target with its arguments.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlockCall {
    pub block: Block,
    pub args: Vec<Value>,
}

/// A `br_table`'s targets.
#[derive(Clone, Debug)]
pub struct JumpTableData {
    pub(crate) default: BlockCall,
    pub(crate) table: Vec<BlockCall>,
}

impl JumpTableData {
    pub fn new(default: BlockCall, table: &[BlockCall]) -> Self {
        JumpTableData {
            default,
            table: table.to_vec(),
        }
    }
}

/// How a load extends what it reads.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Ext {
    /// The value's own width.
    None,
    Zero,
    Sign,
}

/// Binary integer operations.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Alu {
    Add,
    Sub,
    Mul,
    And,
    Or,
    Xor,
    Shl,
    Ushr,
    Sshr,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Div {
    Udiv,
    Urem,
    Sdiv,
    Srem,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Fop {
    Add,
    Sub,
    Mul,
    Div,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Overflow {
    Add,
    Sub,
    Mul,
}

/// An instruction. Operand values are in [`InstData::args`].
#[derive(Clone, Debug)]
pub(crate) enum Op {
    Iconst(i64),
    Fconst(u64),
    /// `[args[0] + off]`, `bytes` wide, extended to the result type.
    Load {
        bytes: u8,
        ext: Ext,
        off: i32,
    },
    /// `[args[1] + off] = args[0]`, as wide as `bytes`.
    Store {
        bytes: u8,
        off: i32,
    },
    Icmp(IntCC),
    IcmpImm(IntCC, i64),
    Fcmp(FloatCC),
    Alu(Alu),
    AluImm(Alu, i64),
    Div(Div),
    Bnot,
    Ineg,
    Smulhi,
    /// `args[0] ? args[1] : args[2]`.
    Select,
    Ireduce,
    Uextend,
    Sextend,
    /// Two results: the wrapped value and the overflow flag (`i8`).
    Overflow(Overflow),
    Bitcast,
    Fop(Fop),
    Fneg,
    Fabs,
    Sqrt,
    FcvtFromSint,
    /// `args[0]` is the callee.
    CallIndirect(SigRef),
    Call(FuncRef),
    /// The address of a stack slot plus an offset.
    StackAddr(StackSlot, i32),
    Jump(Edge),
    Brif(Edge, Edge),
    BrTable(JumpTable),
    Return,
}

/// A branch's target and its arguments (a range of the pool).
#[derive(Copy, Clone, Debug)]
pub(crate) struct Edge {
    pub block: Block,
    pub args: (u32, u32),
}

#[derive(Clone, Debug)]
pub(crate) struct InstData {
    pub op: Op,
    /// The operands and the results: ranges of the pool.
    pub args: (u32, u32),
    pub results: (u32, u32),
    /// The result type, for single-result instructions.
    pub ty: Type,
}

/// Where a value comes from.
#[derive(Copy, Clone, Debug)]
pub(crate) enum ValueDef {
    Result(Inst, u32),
    #[allow(dead_code)]
    Param(Block, u32),
    /// A variable's read, resolved (and replaced) when the blocks are
    /// sealed.
    Var,
}

#[derive(Clone, Debug)]
pub(crate) struct ValueData {
    pub ty: Type,
    pub def: ValueDef,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct BlockData {
    pub params: Vec<Value>,
    pub insts: Vec<Inst>,
    pub cold: bool,
    /// Whether the block is in the layout (switched to at least once).
    pub inserted: bool,
}

/// A function's values, instructions and blocks.
#[derive(Clone, Debug, Default)]
pub struct DataFlowGraph {
    pub(crate) values: Vec<ValueData>,
    pub(crate) insts: Vec<InstData>,
    pub(crate) blocks: Vec<BlockData>,
    pub(crate) sigs: Vec<Signature>,
    pub(crate) tables: Vec<JumpTableData>,
    /// Instruction operands, results and branch arguments.
    pub(crate) pool: Vec<Value>,
    pub(crate) stack_slots: Vec<StackSlotData>,
    pub(crate) funcs: Vec<ExtFuncData>,
}

impl DataFlowGraph {
    pub fn num_insts(&self) -> usize {
        self.insts.len()
    }

    pub fn num_blocks(&self) -> usize {
        self.blocks.len()
    }

    pub fn value_type(&self, v: Value) -> Type {
        self.values[v.index()].ty
    }

    pub fn block_call(&mut self, block: Block, args: &[BlockArg]) -> BlockCall {
        BlockCall {
            block,
            args: args.iter().map(|a| a.0).collect(),
        }
    }

    pub(crate) fn slice(&self, r: (u32, u32)) -> &[Value] {
        &self.pool[r.0 as usize..r.1 as usize]
    }

    pub(crate) fn args(&self, inst: Inst) -> &[Value] {
        self.slice(self.insts[inst.index()].args)
    }

    pub(crate) fn results(&self, inst: Inst) -> &[Value] {
        self.slice(self.insts[inst.index()].results)
    }

    pub(crate) fn intern(&mut self, vals: &[Value]) -> (u32, u32) {
        let start = self.pool.len() as u32;
        self.pool.extend_from_slice(vals);
        (start, self.pool.len() as u32)
    }

    pub(crate) fn make_value(&mut self, ty: Type, def: ValueDef) -> Value {
        let v = Value(self.values.len() as u32);
        self.values.push(ValueData { ty, def });
        v
    }
}

/// A function: its signature, and its body.
#[derive(Clone, Debug)]
pub struct Function {
    pub signature: Signature,
    pub dfg: DataFlowGraph,
    /// The blocks in layout order (as first switched to).
    pub(crate) layout: Vec<Block>,
}

impl Default for Function {
    fn default() -> Self {
        Function::new()
    }
}

impl Function {
    pub fn new() -> Self {
        Function {
            signature: Signature::new(CallConv::host()),
            dfg: DataFlowGraph::default(),
            layout: Vec::new(),
        }
    }

    /// Empty the function for reuse, keeping its buffers.
    pub fn clear(&mut self) {
        self.signature.params.clear();
        self.signature.returns.clear();
        self.signature.call_conv = CallConv::host();
        self.dfg.values.clear();
        self.dfg.insts.clear();
        self.dfg.blocks.clear();
        self.dfg.sigs.clear();
        self.dfg.tables.clear();
        self.dfg.pool.clear();
        self.dfg.stack_slots.clear();
        self.dfg.funcs.clear();
        self.layout.clear();
    }

    /// Make room for about `insts` instructions in `blocks` blocks, so a
    /// function lowered into an empty one doesn't grow its tables by
    /// doubling (copying each) as it goes.
    pub fn reserve(&mut self, insts: usize, blocks: usize) {
        self.dfg.insts.reserve(insts);
        self.dfg.values.reserve(insts);
        self.dfg.pool.reserve(insts * 2);
        self.dfg.blocks.reserve(blocks);
        self.layout.reserve(blocks);
    }

    pub fn display(&self) -> impl fmt::Display + '_ {
        DisplayFunction(self)
    }
}

struct DisplayFunction<'a>(&'a Function);

impl fmt::Display for DisplayFunction<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let func = self.0;
        let dfg = &func.dfg;
        writeln!(f, "function {:?} {{", func.signature)?;
        for (i, s) in dfg.sigs.iter().enumerate() {
            writeln!(f, "    sig{i} = {s:?}")?;
        }
        let fmt_call = |block: Block, args: &[Value]| {
            let args: Vec<String> = args.iter().map(|a| format!("{a}")).collect();
            if args.is_empty() {
                format!("{block}")
            } else {
                format!("{block}({})", args.join(", "))
            }
        };
        let call = |c: &BlockCall| fmt_call(c.block, &c.args);
        let edge = |e: &Edge| fmt_call(e.block, dfg.slice(e.args));
        for &b in &func.layout {
            let bd = &dfg.blocks[b.index()];
            let params: Vec<String> = bd
                .params
                .iter()
                .map(|&p| format!("{p}: {}", dfg.value_type(p)))
                .collect();
            writeln!(
                f,
                "\n{b}({}){}:",
                params.join(", "),
                if bd.cold { " cold" } else { "" }
            )?;
            for &inst in &bd.insts {
                let d = &dfg.insts[inst.index()];
                let res: Vec<String> = dfg
                    .slice(d.results)
                    .iter()
                    .map(|r| format!("{r}"))
                    .collect();
                let args: Vec<String> = dfg.slice(d.args).iter().map(|a| format!("{a}")).collect();
                let lhs = if res.is_empty() {
                    String::new()
                } else {
                    format!("{} = ", res.join(", "))
                };
                let op = match &d.op {
                    Op::Jump(c) => format!("jump {}", edge(c)),
                    Op::Brif(t, e) => format!("brif {}, {}, {}", args[0], edge(t), edge(e)),
                    Op::BrTable(jt) => {
                        let t = &dfg.tables[jt.index()];
                        let tab: Vec<String> = t.table.iter().map(call).collect();
                        format!(
                            "br_table {}, {}, [{}]",
                            args[0],
                            call(&t.default),
                            tab.join(", ")
                        )
                    }
                    op => format!("{op:?}.{} {}", d.ty, args.join(", ")),
                };
                writeln!(f, "    {lhs}{op}")?;
            }
        }
        writeln!(f, "}}")
    }
}

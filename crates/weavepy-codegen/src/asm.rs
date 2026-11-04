//! What the code generator asks of a target's assembler.

use crate::ir::{condcodes::FloatCC, Alu, CallConv, Div, Ext, Fop, Overflow, Type};

/// A machine register: `0..32` are integer registers, `32..64` float
/// registers, numbered as the target encodes them.
pub(crate) type Reg = u8;
pub(crate) const FLOAT: Reg = 32;
pub(crate) const NOREG: Reg = u8::MAX;

pub(crate) fn is_float(r: Reg) -> bool {
    (FLOAT..64).contains(&r)
}

pub(crate) fn bit(r: Reg) -> u64 {
    1u64 << r
}

/// An instruction's second operand.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum Opnd {
    R(Reg),
    I(i64),
}

/// A branch condition on integer flags.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum Cond {
    Eq,
    Ne,
    Slt,
    Sge,
    Sgt,
    Sle,
    Ult,
    Uge,
    Ugt,
    Ule,
    /// Signed overflow from the last add, sub or mul.
    O,
    No,
}

impl Cond {
    pub fn from_int(cc: crate::ir::condcodes::IntCC) -> Cond {
        use crate::ir::condcodes::IntCC::*;
        match cc {
            Equal => Cond::Eq,
            NotEqual => Cond::Ne,
            SignedLessThan => Cond::Slt,
            SignedGreaterThanOrEqual => Cond::Sge,
            SignedGreaterThan => Cond::Sgt,
            SignedLessThanOrEqual => Cond::Sle,
            UnsignedLessThan => Cond::Ult,
            UnsignedGreaterThanOrEqual => Cond::Uge,
            UnsignedGreaterThan => Cond::Ugt,
            UnsignedLessThanOrEqual => Cond::Ule,
        }
    }

    pub fn inverse(self) -> Cond {
        match self {
            Cond::Eq => Cond::Ne,
            Cond::Ne => Cond::Eq,
            Cond::Slt => Cond::Sge,
            Cond::Sge => Cond::Slt,
            Cond::Sgt => Cond::Sle,
            Cond::Sle => Cond::Sgt,
            Cond::Ult => Cond::Uge,
            Cond::Uge => Cond::Ult,
            Cond::Ugt => Cond::Ule,
            Cond::Ule => Cond::Ugt,
            Cond::O => Cond::No,
            Cond::No => Cond::O,
        }
    }

    pub fn swapped(self) -> Cond {
        match self {
            Cond::Slt => Cond::Sgt,
            Cond::Sgt => Cond::Slt,
            Cond::Sge => Cond::Sle,
            Cond::Sle => Cond::Sge,
            Cond::Ult => Cond::Ugt,
            Cond::Ugt => Cond::Ult,
            Cond::Uge => Cond::Ule,
            Cond::Ule => Cond::Uge,
            c => c,
        }
    }
}

/// A code label.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) struct Label(pub u32);

/// A calling convention's registers, as the code generator sees them.
#[derive(Debug)]
pub(crate) struct Abi {
    pub int_args: &'static [Reg],
    pub float_args: &'static [Reg],
    /// Arguments take the next register of either class by position
    /// (Win64), rather than the next of their own class.
    pub positional: bool,
    pub int_rets: &'static [Reg],
    pub float_rets: &'static [Reg],
    /// The registers a callee may change.
    pub clobbers: u64,
    /// Space the caller reserves below a call's stack arguments (Win64's
    /// home area).
    pub shadow: u32,
}

/// The frame the prologue sets up.
#[derive(Debug, Default)]
pub(crate) struct Frame {
    /// The callee-saved registers the body uses.
    pub saved: u64,
    /// Bytes of spill slots, and of the outgoing argument area below them.
    pub slots: u32,
    pub out: u32,
}

/// A target's assembler. Integer operations on types narrower than 64
/// bits may leave the register's upper bits undefined; everything that
/// reads a value reads only its own width.
pub(crate) trait Asm {
    /// Allocatable integer and float registers, the caller-saved ones
    /// first, in the order to prefer them.
    const INT_REGS: &'static [Reg];
    const FLOAT_REGS: &'static [Reg];
    /// The callee-saved registers among them (for the function's own
    /// convention).
    fn callee_saved(conv: CallConv) -> u64;
    /// How many values may stay in callee-saved registers for the whole
    /// function.
    const MAX_PINNED: usize;
    /// x86-64's fixed registers: a shift's count, and division's pair.
    const SHIFT_REG: Option<Reg>;
    const DIV_REGS: Option<(Reg, Reg)>;

    fn abi(conv: CallConv) -> Option<&'static Abi>;

    fn new() -> Self;
    fn new_label(&mut self) -> Label;
    fn bind(&mut self, l: Label);
    fn offset(&self) -> usize;

    // Moves.
    fn mov(&mut self, dst: Reg, src: Reg);
    fn swap(&mut self, a: Reg, b: Reg);
    fn imm(&mut self, dst: Reg, imm: i64);
    fn fimm(&mut self, dst: Reg, bits: u64, ty: Type);
    /// Load or store a spill slot or outgoing argument at `off` bytes
    /// above the stack pointer (a float register moves 8 bytes).
    fn ld_sp(&mut self, dst: Reg, off: i32);
    fn st_sp(&mut self, src: Reg, off: i32);

    // Memory.
    fn load(&mut self, bytes: u8, ext: Ext, ty: Type, dst: Reg, base: Reg, off: i32);
    fn store(&mut self, bytes: u8, src: Reg, base: Reg, off: i32);
    /// Store an immediate, if the target can without a register.
    fn store_imm(&mut self, bytes: u8, imm: i64, base: Reg, off: i32) -> bool;

    // Integer operations.
    /// Whether `op` takes `imm` as its immediate operand.
    fn alu_imm_ok(op: Alu, imm: i64) -> bool;
    fn alu(&mut self, op: Alu, ty: Type, dst: Reg, a: Reg, b: Opnd);
    /// `dst = a op b` with x86's registers in place (see `DIV_REGS`) or
    /// any.
    fn div(&mut self, op: Div, ty: Type, dst: Reg, a: Reg, b: Reg);
    fn smulhi(&mut self, ty: Type, dst: Reg, a: Reg, b: Reg);
    fn not(&mut self, ty: Type, dst: Reg, a: Reg);
    fn neg(&mut self, ty: Type, dst: Reg, a: Reg);
    fn extend(&mut self, signed: bool, from: Type, to: Type, dst: Reg, a: Reg);
    /// Set flags from `a - b`, at `ty`'s width.
    fn cmp(&mut self, ty: Type, a: Reg, b: Opnd);
    /// Whether `cmp` takes `imm` as its immediate operand.
    fn cmp_imm_ok(imm: i64) -> bool;
    /// Set flags from `a`'s own width being zero or not (`Ne`: nonzero).
    fn test(&mut self, ty: Type, a: Reg);
    fn setcc(&mut self, c: Cond, dst: Reg);
    /// `dst = c ? x : y` from the flags `cmp` or `test` set.
    fn csel(&mut self, c: Cond, ty: Type, dst: Reg, x: Reg, y: Reg);
    /// `dst = a op b`, setting flags: the condition that holds on signed
    /// overflow.
    fn overflow(&mut self, op: Overflow, ty: Type, dst: Reg, a: Reg, b: Reg) -> Cond;

    // Floats.
    fn fop(&mut self, op: Fop, ty: Type, dst: Reg, a: Reg, b: Reg);
    fn fneg(&mut self, ty: Type, dst: Reg, a: Reg);
    fn fabs(&mut self, ty: Type, dst: Reg, a: Reg);
    fn fsqrt(&mut self, ty: Type, dst: Reg, a: Reg);
    fn fcvt_from_sint(&mut self, from: Type, to: Type, dst: Reg, a: Reg);
    /// Move bits between an integer and a float register.
    fn bitcast(&mut self, ty: Type, dst: Reg, a: Reg);
    /// Compare `a` with `b` for `cc` (the target may swap them).
    fn fcmp(&mut self, cc: FloatCC, ty: Type, a: Reg, b: Reg);
    /// `dst = cc` (0 or 1) from `fcmp`'s flags.
    fn fsetcc(&mut self, cc: FloatCC, dst: Reg);
    /// Branch to `l` on `fcmp`'s flags meeting `cc`.
    fn fjcc(&mut self, cc: FloatCC, l: Label);

    // Control.
    fn jmp(&mut self, l: Label);
    fn jcc(&mut self, c: Cond, l: Label);
    fn call(&mut self, target: Reg);
    /// Jump to `table[idx]`, or `default` when `idx` is out of range.
    fn br_table(&mut self, idx: Reg, ty: Type, default: Label, table: &[Label]);
    /// The epilogue: the frame torn down and a return.
    fn epilogue(&mut self, frame: &Frame);
    /// The finished code, the prologue for `frame` in front of the body.
    fn finish(self, frame: &Frame) -> Result<Vec<u8>, crate::Error>;

    /// The scratch registers the code generator may use between
    /// instructions: integer and float.
    const SCRATCH: Reg;
    const FSCRATCH: Reg;
    /// The register an indirect call's target goes in (not an argument
    /// register; may be `SCRATCH`).
    const CALL_REG: Reg;
}

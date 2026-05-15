//! Bytecode instruction set for the WeavePy virtual machine.
//!
//! Currently a tiny placeholder. The real instruction set will be defined here
//! and shared between the compiler (which emits) and the VM (which executes).
//! Keeping it in the compiler crate avoids a circular dependency while still
//! allowing the VM crate to depend on the bytecode types.

/// Operand-less opcode for a stack machine instruction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum OpCode {
    /// Push a constant from the code object's constant pool.
    LoadConst = 0,
    /// Pop the top of stack and discard it.
    Pop = 1,
    /// Return the top of stack from the current frame.
    ReturnValue = 2,
    /// Halt the interpreter for the current frame; used for `<module>`.
    Halt = 3,
}

/// A bytecode instruction: an opcode plus an optional inline operand.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Instruction {
    pub op: OpCode,
    pub arg: u32,
}

impl Instruction {
    #[inline]
    pub const fn new(op: OpCode, arg: u32) -> Self {
        Self { op, arg }
    }
}

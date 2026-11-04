//! A small, fast machine-code generator for WeavePy's frame JIT.
//!
//! The frame JIT lowers a hot code object to SSA instructions over basic
//! blocks with block parameters, the subset of Cranelift's IR it needs,
//! built through the same names ([`FunctionBuilder`], `ins()`, `brif`,
//! `call_indirect` and so on). This crate turns such a function into
//! machine code in one linear pass (see [`codegen`]), without Cranelift's
//! register allocator, which cost the frame JIT about 11,000 host
//! instructions and a kilobyte of memory per IR instruction.
//!
//! Supported targets: x86-64 (System V and Win64) and AArch64.

#![allow(clippy::new_without_default, clippy::wrong_self_convention)]

#[cfg(target_arch = "aarch64")]
mod aarch64;
mod asm;
mod builder;
mod codegen;
pub mod ir;
mod liveness;
mod mem;
mod simplify;
#[cfg(target_arch = "x86_64")]
mod x64;

pub use builder::{FuncInstBuilder, FunctionBuilder};
pub use ir::{
    types, AbiParam, Block, BlockArg, BlockCall, CallConv, Function, Inst, JumpTable,
    JumpTableData, MemFlags, SigRef, Signature, Type, Value,
};
pub use mem::mapped_bytes;

/// Why a function didn't compile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// Something this generator doesn't do.
    Unsupported(&'static str),
    /// A broken invariant: a bug.
    Internal(&'static str),
    /// No executable memory.
    Memory,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Unsupported(what) => write!(f, "unsupported: {what}"),
            Error::Internal(what) => write!(f, "internal error: {what}"),
            Error::Memory => f.write_str("out of executable memory"),
        }
    }
}

impl std::error::Error for Error {}

/// Whether this target has a code generator.
pub const SUPPORTED: bool = cfg!(any(target_arch = "x86_64", target_arch = "aarch64"));

/// The machine code for `func`, position-independent.
pub fn compile(func: &mut Function) -> Result<Vec<u8>, Error> {
    #[cfg(target_arch = "x86_64")]
    {
        codegen::compile::<x64::X64>(func)
    }
    #[cfg(target_arch = "aarch64")]
    {
        codegen::compile::<aarch64::A64>(func)
    }
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    {
        let _ = func;
        Err(Error::Unsupported("this target"))
    }
}

/// Write `code` to `$WPCG_DUMP_DIR/<n>.bin`, when set (a debugging aid).
fn dump(code: &[u8]) {
    static DIR: std::sync::OnceLock<Option<std::path::PathBuf>> = std::sync::OnceLock::new();
    static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    if let Some(dir) = DIR.get_or_init(|| std::env::var_os("WPCG_DUMP_DIR").map(Into::into)) {
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let _ = std::fs::write(dir.join(format!("{n}.bin")), code);
    }
}

/// Compile `func` and copy its code into executable memory: its entry.
pub fn compile_and_install(func: &mut Function) -> Result<*const u8, Error> {
    let code = compile(func)?;
    mem::install(&code)
}

/// Copy machine code into executable memory: its address.
pub fn install(code: &[u8]) -> Result<*const u8, Error> {
    dump(code);
    mem::install(code)
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod fuzz;

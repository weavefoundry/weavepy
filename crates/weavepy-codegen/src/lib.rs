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

#![allow(
    clippy::new_without_default,
    clippy::wrong_self_convention,
    clippy::enum_glob_use
)]

#[cfg(target_arch = "aarch64")]
mod aarch64;
mod asm;
mod builder;
mod codegen;
mod fold;
pub mod fxhash;
pub mod ir;
mod liveness;
mod mem;
mod simplify;
mod ssa;
#[cfg(target_arch = "x86_64")]
mod x64;

pub use builder::{FuncInstBuilder, FunctionBuilder};
pub use ir::{
    types, AbiParam, Block, BlockArg, BlockCall, CallConv, ExtFuncData, FuncRef, FuncTarget,
    Function, Inst, JumpTable, JumpTableData, MemFlags, SigRef, Signature, StackSlot,
    StackSlotData, StackSlotKind, Type, Value, Variable,
};
pub use mem::{mapped_bytes, CodeArena};

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

/// A function's machine code, and the places in it that address code or
/// data outside it: each a 32-bit displacement, relative to its own end,
/// to the absolute address beside it, written as the code is installed
/// (see [`install`]). Code without any is position-independent.
#[derive(Debug, Clone, Default)]
pub struct Code {
    pub bytes: Vec<u8>,
    pub relocs: Vec<(u32, u64)>,
}

impl std::ops::Deref for Code {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        &self.bytes
    }
}

impl AsRef<[u8]> for Code {
    fn as_ref(&self) -> &[u8] {
        &self.bytes
    }
}

impl From<Vec<u8>> for Code {
    fn from(bytes: Vec<u8>) -> Self {
        Code {
            bytes,
            relocs: Vec::new(),
        }
    }
}

impl Code {
    /// The bytes as they run at `at`: each displacement written.
    fn placed(&self, at: usize) -> Result<std::borrow::Cow<'_, [u8]>, Error> {
        if self.relocs.is_empty() {
            return Ok(std::borrow::Cow::Borrowed(&self.bytes));
        }
        let mut out = self.bytes.clone();
        for &(off, target) in &self.relocs {
            let end = (at + off as usize + 4) as i64;
            let rel = i32::try_from(target as i64 - end).map_err(|_| Error::Memory)?;
            out[off as usize..off as usize + 4].copy_from_slice(&rel.to_le_bytes());
        }
        Ok(std::borrow::Cow::Owned(out))
    }
}

/// The machine code for `func`. Where code memory lies within reach of
/// this program's own code (see `mem`), calls of its functions and the
/// addresses of its code and data take 32-bit displacements, which
/// [`install`] writes.
pub fn compile(func: &mut Function) -> Result<Code, Error> {
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
pub(crate) fn dump(code: &[u8]) {
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

/// Copy machine code into executable memory, its displacements written:
/// its address.
pub fn install(code: &Code) -> Result<*const u8, Error> {
    mem::install(code)
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod fuzz;

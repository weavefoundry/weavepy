//! RFC 0032 — tier-2 Cranelift JIT for WeavePy's unboxed numeric frames.
//!
//! This crate compiles the *unboxed numeric/control-flow core* of a
//! [`weavepy_compiler::CodeObject`] — `int`/`float`/`bool` arithmetic,
//! comparisons, the conditional and unconditional jumps, `range`
//! iteration, and `return` — to native machine code via Cranelift.
//! Everything outside that subset (containers, attribute access, calls
//! out, exceptions, generators) stays in the interpreter; a frame whose
//! hot region touches an unsupported opcode is reported
//! [`JitStatus::NotJitable`] and never re-attempted.
//!
//! The crate deliberately does **not** depend on `weavepy-vm`: it speaks
//! only in `i64`/`f64`/`bool` lanes plus the side-exit protocol in
//! [`runtime`], so the VM owns the `Object` model and marshals values in
//! and out of a [`runtime::JitFrame`] around each native entry. That
//! keeps the unsafe FFI surface tiny and the dependency graph acyclic.
//!
//! # Safety
//!
//! Entering compiled code is `unsafe` by nature (an indirect call
//! through a function pointer with a `#[repr(C)]` argument). The unsafe
//! is confined to [`engine`] and [`runtime`]; callers interact through
//! the safe [`JitEngine`] API and the [`runtime::JitFrame`] struct.

mod analyze;
mod engine;
mod ir;
mod lower;
mod runtime;
mod value;

pub use analyze::{analyze, JitVerdict};
pub use engine::{CompiledFrame, JitEngine};
pub use ir::{ArithKind, BlockId, CmpKind, TBlock, TFunc, TOp, TStmt, TTerm};
pub use runtime::{JitFrame, JitStatus, SlotTag};
pub use value::JitType;

/// Outcome of attempting to compile a code object.
#[derive(Debug)]
pub enum CompileOutcome {
    /// The code object compiled; the engine cached the native function.
    Compiled(CompiledFrame),
    /// The code object is outside the JITable subset. The caller should
    /// record this verdict and stop re-attempting compilation.
    NotJitable(JitVerdict),
}

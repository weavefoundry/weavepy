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

pub use analyze::{
    analyze, analyze_frame, analyze_with_probe, analyze_with_probes, returns_none_syntactically,
    returns_self_syntactically, JitVerdict, MethodResolution, PathArena, Probes, ELEM_SENTINEL,
};
pub use engine::{CompiledFrame, JitEngine};
pub use ir::{
    ArithKind, AttrSiteMeta, BlockId, CalleeSpanMeta, CmpKind, CompSavedMeta, CtorFieldSrc,
    GlobalGuard, IterLoopMeta, ListLoopMeta, MathFunc, MathGuardMeta, MethodRet, MethodSiteMeta,
    MethodSpanMeta, OsrEntry, RangeLoopMeta, ResolvedGlobal, StrMethod, TBlock, TFunc, TOp, TStmt,
    TTerm,
};
pub use runtime::{
    register_attr_helpers, register_build_map_helper, register_call_method_helper,
    register_call_py_helper, register_const_str_helper, register_dict_helpers,
    register_dict_iter_helper, register_iter_helpers, register_list_extra_helpers,
    register_list_helpers, register_list_next_helper, register_math_helpers, register_poll_helper,
    register_str_helpers, register_str_method_helper, register_str_write_helpers, AttrGetHelper,
    AttrSetHelper, BuildListHelper, BuildTupleHelper, BytesGetHelper, CallMethodHelper,
    CallPyHelper, CallStatus, DictAccessHelper, GetIterHelper, IterNextHelper, JitFrame, JitStatus,
    ListAppendHelper, ListGetHelper, ListLenHelper, ListNextHelper, ListRepeatHelper,
    ListSetHelper, ListSliceHelper, MathBinaryHelper, MathUnaryHelper, PollHelper, SlotTag,
    StrEqHelper, StrLenHelper, DICT_KEY_INT, DICT_KEY_STR, DICT_VAL_FLOAT, DICT_VAL_INT,
    DICT_VAL_OBJ, ITER_ELEM_STR, JIT_POLL_STRIDE,
};
pub use value::JitType;

/// Outcome of attempting to compile a code object.
#[derive(Debug)]
pub enum CompileOutcome {
    /// The code object compiled; the engine cached the native function.
    /// Boxed: the frame metadata (spans, sites, guards) dwarfs the
    /// verdict arm.
    Compiled(Box<CompiledFrame>),
    /// The code object is outside the JITable subset. The caller should
    /// record this verdict and stop re-attempting compilation.
    NotJitable(JitVerdict),
}

//! The Cranelift JIT module lifecycle and the compiled-frame entry
//! point.
//!
//! A [`JitEngine`] owns one [`JITModule`]; every compiled frame is a
//! native function defined into it. The engine is intended to be a
//! per-thread singleton (the VM keeps it in thread-local storage, under
//! the GIL), so the function pointers stay valid for the thread's
//! lifetime and there is no cross-thread aliasing.

use std::mem;

use cranelift_codegen::ir::{types, AbiParam, Type};
use cranelift_codegen::settings::{self, Configurable};
use cranelift_codegen::Context;
use cranelift_frontend::FunctionBuilderContext;
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::{Linkage, Module};

use crate::analyze::JitVerdict;
use crate::ir::{CalleeSpanMeta, GlobalGuard, OsrEntry, RangeLoopMeta, ResolvedGlobal, TFunc};
use crate::lower::build_function;
use crate::runtime::{self, JitFrame, JitStatus};
use crate::value::JitType;
use weavepy_compiler::CodeObject;

/// The native ABI of a compiled frame: takes a `*mut JitFrame`, returns
/// an `i64` [`JitStatus`].
pub(crate) type NativeFn = unsafe extern "C" fn(*mut JitFrame) -> i64;

/// A compiled frame plus the metadata the VM needs to marshal values in
/// and out and to apply the entry guard.
#[derive(Debug)]
pub struct CompiledFrame {
    func: NativeFn,
    /// Local slots to type-guard + pack before entry (read-before-write).
    pub livein: Vec<u32>,
    /// Stable lane of each local slot (`None` = not JIT-managed).
    pub local_types: Vec<Option<JitType>>,
    /// Max abstract operand-stack depth, for sizing the spill buffer.
    pub max_stack: u32,
    /// Number of local slots, *including* synthetic range-loop slots.
    pub n_locals: u32,
    /// Global resolutions burned into the code; the embedder must
    /// re-validate each before every native entry (RFC 0058 WS4).
    pub global_guards: Vec<GlobalGuard>,
    /// Rewritten range loops, outermost-first, for rebuilding the live
    /// iterators on the interpreter stack after a mid-loop deopt.
    pub range_loops: Vec<RangeLoopMeta>,
    /// Erased Python callees (RFC 0059 WS3), for rebuilding the callee
    /// object on the interpreter stack after a mid-arguments deopt.
    pub callee_spans: Vec<CalleeSpanMeta>,
    /// Loop-header pcs enterable mid-frame via `entry_pc` (OSR).
    pub osr_entries: Vec<OsrEntry>,
    /// Widest `CallPy` argument count, for sizing the marshal buffers.
    pub max_call_args: u32,
    /// The function's own stable scalar return lane, when every return
    /// site agrees (feeds callers' `PyFunc` classification).
    pub ret_lane: Option<JitType>,
}

impl CompiledFrame {
    /// Enter the compiled frame.
    ///
    /// # Safety
    ///
    /// `frame` must point to a fully-initialised [`JitFrame`] whose
    /// `locals` / `stack_spill` / `stack_tags` buffers are at least
    /// `n_locals` / `max_stack` wide, and the owning [`JitEngine`] must
    /// still be alive (its `JITModule` backs this function pointer).
    #[must_use]
    pub unsafe fn enter(&self, frame: *mut JitFrame) -> JitStatus {
        // SAFETY: the caller upholds the buffer-size and liveness
        // invariants documented above; the function pointer was produced
        // by `JITModule::get_finalized_function` for this exact signature.
        let raw = unsafe { (self.func)(frame) };
        JitStatus::from_raw(raw)
    }
}

/// Owns the Cranelift JIT module and reusable codegen contexts.
pub struct JitEngine {
    module: JITModule,
    ctx: Context,
    fbctx: FunctionBuilderContext,
    ptr_ty: Type,
    next_id: u32,
}

impl std::fmt::Debug for JitEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JitEngine")
            .field("ptr_ty", &self.ptr_ty)
            .field("next_id", &self.next_id)
            .finish_non_exhaustive()
    }
}

impl JitEngine {
    /// Build a fresh engine for the host target. Returns `None` if the
    /// host ISA can't be configured (e.g. an unsupported platform), in
    /// which case the VM simply never tiers up.
    #[must_use]
    pub fn new() -> Option<JitEngine> {
        let mut flag_builder = settings::builder();
        // A JIT that emits absolute addresses and resolves libcalls
        // in-process.
        flag_builder.set("use_colocated_libcalls", "false").ok()?;
        flag_builder.set("is_pic", "false").ok()?;
        // Favour fast compiles over the last few percent of codegen.
        flag_builder.set("opt_level", "speed").ok()?;
        let isa_builder = cranelift_native::builder().ok()?;
        let isa = isa_builder
            .finish(settings::Flags::new(flag_builder))
            .ok()?;
        let builder = JITBuilder::with_isa(isa, cranelift_module::default_libcall_names());
        let module = JITModule::new(builder);
        let ptr_ty = module.target_config().pointer_type();
        let ctx = module.make_context();
        Some(JitEngine {
            module,
            ctx,
            fbctx: FunctionBuilderContext::new(),
            ptr_ty,
            next_id: 0,
        })
    }

    /// Analyze and compile a code object. `resolve` reports what each
    /// `LOAD_GLOBAL` name currently resolves to (see
    /// [`ResolvedGlobal`]); the caller must re-validate every resolution
    /// listed in [`CompiledFrame::global_guards`] before each entry.
    /// Returns the compiled frame, or the [`JitVerdict`] explaining why
    /// the code is not JITable.
    pub fn compile(
        &mut self,
        code: &CodeObject,
        resolve: &mut dyn FnMut(&str) -> ResolvedGlobal,
    ) -> Result<CompiledFrame, JitVerdict> {
        self.compile_with_probe(code, resolve, &mut |_| None)
    }

    /// [`Self::compile`] with a pinned-list lane probe (RFC 0061 WS5):
    /// `probe_list` reports the observed homogeneous element lane of a
    /// subscripted local in the requesting activation.
    pub fn compile_with_probe(
        &mut self,
        code: &CodeObject,
        resolve: &mut dyn FnMut(&str) -> ResolvedGlobal,
        probe_list: &mut dyn FnMut(u32) -> Option<JitType>,
    ) -> Result<CompiledFrame, JitVerdict> {
        let tfunc = crate::analyze::analyze_with_probe(code, resolve, probe_list)?;
        self.compile_tfunc(&tfunc)
    }

    /// Compile an already-analyzed [`TFunc`] (also the unit-test entry).
    pub fn compile_tfunc(&mut self, tfunc: &TFunc) -> Result<CompiledFrame, JitVerdict> {
        // A frame with native Python-to-Python calls needs the embedder's
        // call helper burned in (RFC 0059 WS3).
        if !tfunc.callee_spans.is_empty() && runtime::call_py_helper_addr() == 0 {
            return Err(JitVerdict::UnsupportedOpcode("CALL (no helper registered)"));
        }
        // RFC 0061 WS5 — likewise for the pinned-list helpers.
        let has_list_ops = tfunc.blocks.iter().any(|b| {
            b.stmts.iter().any(|s| {
                matches!(
                    s.op,
                    crate::ir::TOp::ListGet { .. } | crate::ir::TOp::ListSet
                )
            })
        });
        if has_list_ops
            && (runtime::list_get_helper_addr() == 0 || runtime::list_set_helper_addr() == 0)
        {
            return Err(JitVerdict::UnsupportedOpcode(
                "SUBSCR (no list helper registered)",
            ));
        }
        self.module.clear_context(&mut self.ctx);

        // Signature: (frame: ptr) -> i64.
        self.ctx
            .func
            .signature
            .params
            .push(AbiParam::new(self.ptr_ty));
        self.ctx
            .func
            .signature
            .returns
            .push(AbiParam::new(types::I64));

        build_function(&mut self.ctx.func, &mut self.fbctx, tfunc, self.ptr_ty);

        let name = format!("wpjit_{}", self.next_id);
        self.next_id += 1;
        let id = self
            .module
            .declare_function(&name, Linkage::Local, &self.ctx.func.signature)
            .map_err(|_| JitVerdict::NotConverged)?;
        self.module
            .define_function(id, &mut self.ctx)
            .map_err(|_| JitVerdict::NotConverged)?;
        self.module.clear_context(&mut self.ctx);
        self.module
            .finalize_definitions()
            .map_err(|_| JitVerdict::NotConverged)?;

        let code_ptr = self.module.get_finalized_function(id);
        // SAFETY: `code_ptr` is a finalized function with exactly the
        // `(*mut JitFrame) -> i64` signature declared above; the module
        // keeps the code alive for the engine's lifetime.
        let func: NativeFn = unsafe { mem::transmute::<*const u8, NativeFn>(code_ptr) };

        Ok(CompiledFrame {
            func,
            livein: tfunc.livein_locals.clone(),
            local_types: tfunc.local_types.clone(),
            max_stack: tfunc.max_stack,
            n_locals: tfunc.n_locals,
            global_guards: tfunc.global_guards.clone(),
            range_loops: tfunc.range_loops.clone(),
            callee_spans: tfunc.callee_spans.clone(),
            osr_entries: tfunc.osr_entries.clone(),
            max_call_args: tfunc.max_call_args,
            ret_lane: tfunc.ret_lane,
        })
    }
}

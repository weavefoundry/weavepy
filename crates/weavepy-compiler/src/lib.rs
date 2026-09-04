//! AST-to-bytecode compiler for WeavePy.
//!
//! Walks a [`weavepy_parser::Module`] and produces a [`CodeObject`]
//! containing the bytecode plus the constants, names, varnames,
//! cellvars, and freevars tables the VM needs.
//!
//! The compiler runs two passes per code unit:
//!
//! 1. **Scope analysis**: classify every name as local, global,
//!    cell (referenced by inner scope), or free (referenced from outer).
//! 2. **Emission**: walk the AST again and emit instructions, using
//!    the scope classification to pick `LOAD_FAST`/`LOAD_GLOBAL`/
//!    `LOAD_DEREF`.
//!
//! # Compatibility level
//!
//! - **Tracks CPython** for opcode names, scope classification, and
//!   the lowering of comprehensions to anonymous functions.
//! - **Experimental** for the exact instruction sequence — CPython's
//!   peephole optimizer and adaptive specialization produce different
//!   shapes that we deliberately don't reproduce.

use std::collections::HashSet;
use std::rc::Rc;

use bytecode::intrinsic;
use indexmap::IndexMap;
use thiserror::Error;
use weavepy_parser::ast::{
    Arguments as AstArguments, BinOp, BoolOp, CmpOp, Comprehension, Constant as AstConstant,
    ExceptHandler, Expr, ExprKind, Keyword as KwArg, MatchCase, Module, Pattern, Stmt, StmtKind,
    TypeParamKind, UnaryOp, WithItem,
};

mod ast_opt;
pub mod bytecode;
pub mod cpython_code;
mod flowgraph;
mod mangle;
mod validate;

pub use bytecode::{
    BinOpKind, CacheTable, CompareKind, InlineCache, Instruction, OpCode, UnaryKind,
    BINARY_OP_INPLACE_FLAG, COMMON_CONSTANT_ALL, COMMON_CONSTANT_ANY,
    COMMON_CONSTANT_ASSERTION_ERROR, COMMON_CONSTANT_NOT_IMPLEMENTED_ERROR, COMMON_CONSTANT_TUPLE,
    COMPARE_OP_TO_BOOL_FLAG, COOLDOWN, SPECIAL_AENTER, SPECIAL_AEXIT, SPECIAL_ENTER, SPECIAL_EXIT,
};
pub use cpython_code::{CpythonCode, Position};

/// CPython compile.c `STACK_USE_GUIDELINE`: literal displays and call
/// sites with more operands than this compile through accumulator
/// shapes (append/add/update loops) instead of pushing every operand,
/// keeping `co_stacksize` O(1) in the source length.
const STACK_USE_GUIDELINE: usize = 30;

/// Line-table sentinel for CPython's `NEXT_LOCATION`: the instruction
/// takes the following instruction's location once the flowgraph has
/// settled the final order (`assemble_location_info`).
pub(crate) const NEXT_LOCATION_LINE: u32 = u32::MAX;

/// Placeholder for an exception-table `depth` that can only be known
/// after the stream is final: handlers emitted inside an *inlined*
/// comprehension protect a region whose base stack depth depends on
/// the surrounding expression. `Compiler::finish` resolves these with
/// a static stack simulation (`cpython_code::compute_startdepths`),
/// reading the depth at the region's *start*.
pub(crate) const HANDLER_DEPTH_SENTINEL: u32 = u32::MAX;

/// Anchored variant: `HANDLER_DEPTH_ANCHOR_FLAG | insn_index` resolves
/// to the static depth at `insn_index` instead of the region start.
/// Needed when a covered range begins above its own baseline — a
/// `with`'s coverage starts at the `__enter__`-result bind (one slot
/// above the kept `__exit__`), so its depth anchors at the first body
/// instruction. Instruction indices stay below 2^31; the plain
/// sentinel (`u32::MAX`) is disjoint because no anchor uses index
/// `0x7fff_ffff`. Stream-rewriting passes (`compact_stream`) remap the
/// packed index alongside `start`/`end`/`handler`.
pub(crate) const HANDLER_DEPTH_ANCHOR_FLAG: u32 = 0x8000_0000;

// ---------- error type ----------

/// A compile-phase `SyntaxError`. The message matches CPython verbatim
/// (tests assert on these strings) and, when known, `span` carries the
/// byte range of the offending construct so the raise site can populate
/// `SyntaxError.lineno`/`.offset`/`.end_lineno`/`.end_offset`. CPython's
/// compile/symtable-stage errors report *byte*-based columns (the raw
/// AST `col_offset + 1`), unlike parser errors which are
/// character-based — converters must honour that.
#[derive(Debug, Clone, Error, PartialEq, Eq)]
#[error("{message}")]
pub struct CompileError {
    pub message: String,
    pub span: Option<weavepy_lexer::Span>,
    /// Whether CPython raises this error in the PEG *parser* (its
    /// `invalid_*` grammar rules — e.g. "cannot assign to literal")
    /// rather than in symtable/compile. Parser-stage errors report
    /// character-based columns and always populate `SyntaxError.text`;
    /// compile-stage errors report byte-based columns and leave `.text`
    /// as `None` for non-file sources.
    pub parser_stage: bool,
}

impl CompileError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            span: None,
            parser_stage: false,
        }
    }

    pub fn spanned(message: impl Into<String>, span: weavepy_lexer::Span) -> Self {
        Self {
            message: message.into(),
            span: Some(span),
            parser_stage: false,
        }
    }

    /// An error CPython raises from its PEG parser's `invalid_*` rules.
    pub fn parser_spanned(message: impl Into<String>, span: weavepy_lexer::Span) -> Self {
        Self {
            message: message.into(),
            span: Some(span),
            parser_stage: true,
        }
    }

    pub fn not_implemented(feature: &str, hint: &str) -> Self {
        Self::new(format!(
            "`{feature}` is not yet supported by the compiler ({hint})"
        ))
    }

    pub fn internal(message: impl std::fmt::Display) -> Self {
        Self::new(format!("internal compiler error: {message}"))
    }
}

pub use weavepy_parser::ast::expr_name;

// ---------- code object ----------

/// RFC 0061 (WS2a): an opaque, VM-owned per-code-object extension slot.
///
/// The VM stashes derived, execution-only state here (today: the
/// materialized constant-object table, so `LOAD_CONST` is an indexed
/// clone instead of a per-execution `Constant` deep-clone + conversion).
/// The compiler crate stays Object-free: the payload is type-erased and
/// only the VM ever downcasts it.
///
/// Semantics mirror [`CacheTable`]: derived state does not follow
/// clones (a `replace()`d code object may change `constants`, so a
/// cloned code object starts with an empty slot), never participates in
/// equality, and is not serialized.
#[derive(Default)]
pub struct VmExt(pub std::sync::OnceLock<std::sync::Arc<dyn std::any::Any + Send + Sync>>);

impl Clone for VmExt {
    fn clone(&self) -> Self {
        Self::default()
    }
}

impl PartialEq for VmExt {
    fn eq(&self, _other: &Self) -> bool {
        true
    }
}

impl std::fmt::Debug for VmExt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(if self.0.get().is_some() {
            "VmExt(populated)"
        } else {
            "VmExt(empty)"
        })
    }
}

/// RFC 0067 — a "the tier-2 JIT measured this code as not jitable"
/// hint, denormalized onto the code object so the per-activation
/// tier-up probe for never-compilable code (kwargs / defaults /
/// generator shapes — the bulk of call-heavy workloads) is one
/// relaxed atomic load instead of a thread-local borrow plus a
/// pointer-keyed hash lookup on every call. Purely an optimization
/// gate: unset means "ask the tier cache", set means "skip tier-up".
/// The flag is monotonic (unset → set only). Like [`VmExt`], it does
/// not follow clones (a `replace()`d code object may change shape),
/// never participates in equality, and is not serialized.
#[derive(Default)]
pub struct JitHint(std::sync::atomic::AtomicBool);

impl JitHint {
    #[must_use]
    pub fn is_not_jitable(&self) -> bool {
        self.0.load(std::sync::atomic::Ordering::Relaxed)
    }

    pub fn mark_not_jitable(&self) {
        self.0.store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

impl Clone for JitHint {
    fn clone(&self) -> Self {
        Self::default()
    }
}

impl PartialEq for JitHint {
    fn eq(&self, _other: &Self) -> bool {
        true
    }
}

impl std::fmt::Debug for JitHint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(if self.is_not_jitable() {
            "JitHint(not-jitable)"
        } else {
            "JitHint(unset)"
        })
    }
}

/// A compiled Python code object. Mirrors the subset of
/// `PyCodeObject` we need to emulate.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CodeObject {
    pub name: String,
    /// Dotted qualified name (PEP 3155), computed at compile time from the
    /// lexical scope nesting: `outer.<locals>.inner` for a function nested
    /// in `outer`, `C.method` for a method of class `C`. Equals `name` for
    /// module-level definitions. Drives `function.__qualname__` /
    /// `type.__qualname__` (and thus reprs, error messages, and pickling).
    pub qualname: String,
    /// Source filename or `<string>`. Used for diagnostics only.
    pub filename: String,
    pub instructions: Vec<Instruction>,
    /// Per-instruction inline cache slots (RFC 0021 — adaptive
    /// specialization). Same length as [`Self::instructions`]; not
    /// serialised by marshal (caches are re-warmed on the next run
    /// because the type pointers they capture wouldn't be valid).
    pub caches: CacheTable,
    /// RFC 0061 (WS2a): VM-owned derived state (see [`VmExt`]).
    pub vm_ext: VmExt,
    /// RFC 0067: tier-2 "not jitable" fast-out hint (see [`JitHint`]).
    pub jit_hint: JitHint,
    pub constants: Vec<Constant>,
    /// Names referenced by `LOAD_NAME` / `LOAD_GLOBAL` / `STORE_NAME` etc.
    pub names: Vec<String>,
    /// Local variable names (positional + keyword + `*args`/`**kwargs` + locals).
    pub varnames: Vec<String>,
    /// Free variables — read from an enclosing scope.
    pub freevars: Vec<String>,
    /// Cell variables — locally defined but referenced by an inner scope.
    pub cellvars: Vec<String>,
    /// Out-of-line exception handlers. Looked up by current PC when a
    /// `RuntimeError::PyException` propagates through this code object.
    pub exception_table: Vec<ExcHandler>,
    /// Source line number (1-based) per emitted instruction. Same length
    /// as `instructions`. Used for traceback rendering.
    pub linetable: Vec<u32>,
    /// PEP-657 fine-grained column spans, one per instruction (same length
    /// as `instructions` once emission finishes). Drives the column fields
    /// of `co_positions()`. Empty when never populated (e.g. code objects
    /// reconstructed from marshal, which doesn't carry columns).
    pub coltable: Vec<ColSpan>,
    /// Number of positional + keyword arguments (excluding `*args`/`**kwargs`).
    pub arg_count: u32,
    /// Number of positional-only arguments.
    pub posonly_count: u32,
    /// Number of keyword-only arguments.
    pub kwonly_count: u32,
    /// Set when this code object accepts `*args`.
    pub has_varargs: bool,
    /// Set when this code object accepts `**kwargs`.
    pub has_varkeywords: bool,
    /// `True` when this code object is the body of a `class` statement.
    pub is_class_body: bool,
    /// `True` when this code object is a generator function (contains
    /// a `yield` or `yield from` expression). Calling such a function
    /// returns a `PyGenerator` instead of running the body eagerly.
    pub is_generator: bool,
    /// `True` when this code object was produced by an `async def`
    /// without `yield`. Calling such a function returns an
    /// `Object::Coroutine`.
    pub is_coroutine: bool,
    /// `True` when this code object was produced by an `async def`
    /// that *also* contains `yield`. Calling such a function returns
    /// an `Object::AsyncGenerator`.
    pub is_async_generator: bool,
    /// `True` when a generator code object was marked with
    /// `types.coroutine` (CPython's `CO_ITERABLE_COROUTINE`). Such a
    /// generator is accepted by `await` and may `yield from` a
    /// coroutine. Never set by the compiler — only by the runtime
    /// marking helper and marshal round-trips.
    pub is_iterable_coroutine: bool,
    /// CPython 3.14 `CO_HAS_DOCSTRING`: `constants[0]` is this scope's
    /// docstring. Functions without one no longer carry a `None` in
    /// slot 0, so this flag (not the shape of `co_consts`) is what
    /// `__doc__` consults.
    pub has_docstring: bool,
    /// CPython 3.14 `CO_METHOD`: a function-like scope defined
    /// directly inside a class body (`ste_method`).
    pub is_method: bool,
    /// CPython `ste_nested` (`CO_NESTED`): the scope was created
    /// inside a function-like scope, a comprehension (inlined or not),
    /// or another nested scope. Tracked explicitly because an inlined
    /// comprehension leaves no `<locals>` in the qualname to infer
    /// it from (`K.<lambda>` inside `[lambda: v for v in r]`).
    pub is_nested: bool,
    /// `CO_FUTURE_*` bits active for this code object (the module's
    /// own `__future__` imports merged with any inherited/compile-flag
    /// bits). Reported through `co_flags` so `compile(...,
    /// dont_inherit=False)` can inherit the caller's futures the way
    /// CPython does (RFC 0052).
    pub future_flags: u32,
    /// Memoised [`Self::to_cpython`] encoding (never compared, resets
    /// on clone).
    pub cp_cache: cpython_code::CpCache,
    /// Raw CPython-wire overrides installed by `CodeType(...)` or
    /// `code.replace(co_code=…)` (RFC 0060). `None` for compiler-produced
    /// code objects; when set, the `co_code`/`co_linetable`/… attribute
    /// surface reports these bytes verbatim instead of re-encoding the
    /// instruction stream, so constructor/replace round-trips are exact.
    pub wire: Option<Box<WireOverrides>>,
    /// Sorted indices of `JumpBackward` instructions that encode as
    /// `JUMP_BACKWARD_NO_INTERRUPT` on the CPython wire (RFC 0068).
    /// CPython emits JUMP_NO_INTERRUPT for every synthetic scope-exit
    /// jump (exception-handler exits, `with`-suppress exits, cold-block
    /// rejoins); the distinction only becomes visible when the edge
    /// ends up backward. The VM executes both identically (the
    /// eval-breaker poll is not observable here), so this is pure
    /// wire/dis metadata.
    pub no_interrupt_jumps: Vec<u32>,
    /// One [`bytecode::wire`] mark per instruction: how the codec
    /// presents it on the CPython wire (borrowing/checked loads,
    /// superinstruction fusion). Empty means all plain.
    pub wire_marks: Vec<u8>,
    /// Locals that were fast-hidden at some point (CPython's
    /// `u_fasthidden` keys): the targets of PEP 709 inlined
    /// comprehensions in a non-function scope. They carry
    /// `CO_FAST_HIDDEN` in `co_localspluskinds` and stay out of
    /// `locals()`.
    pub hidden_locals: Vec<String>,
}

/// Raw CPython-3.13 wire fields pinned on a [`CodeObject`] by the
/// `CodeType` constructor or `code.replace` (RFC 0060). Each `Some`
/// field wins over the value derived from the instruction stream.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct WireOverrides {
    pub co_code: Option<Vec<u8>>,
    pub co_linetable: Option<Vec<u8>>,
    pub co_exceptiontable: Option<Vec<u8>>,
    pub stacksize: Option<u32>,
    pub flags: Option<u32>,
    /// Set when the pinned `co_code` couldn't be decoded back into
    /// WeavePy instructions. Executing such a code object raises
    /// `SystemError` with this message (CPython: "unknown opcode N").
    pub exec_error: Option<String>,
}

/// A per-instruction source-column span (PEP-657). `col`/`end_col` are
/// 0-based UTF-8 byte offsets within their respective source lines, and
/// are `-1` when the column was not tracked. `end_lineno` is `0` when
/// unknown (callers fall back to the instruction's start line).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ColSpan {
    pub end_lineno: u32,
    pub col: i32,
    pub end_col: i32,
}

impl Default for ColSpan {
    fn default() -> Self {
        // "Unknown" sentinel — matches an instruction with no tracked span.
        Self {
            end_lineno: 0,
            col: -1,
            end_col: -1,
        }
    }
}

/// One entry in a code object's exception table. Mirrors the
/// PEP 657-style out-of-line model CPython 3.11+ uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExcHandler {
    /// First instruction protected (inclusive).
    pub start: u32,
    /// First instruction past the protected range (exclusive).
    pub end: u32,
    /// Handler entry point.
    pub handler: u32,
    /// Stack depth to restore before pushing the exception value and
    /// jumping into the handler.
    pub depth: u32,
    /// CPython's `lasti` exception-table flag: the handler is a
    /// *cleanup* block (with-exit, except-variable unbind) whose
    /// trailing `RERAISE` restores `f_lasti` to the original raise
    /// site so `frame.f_lineno` stays accurate (PEP 626).
    pub push_lasti: bool,
}

impl CodeObject {
    /// Content equality in the sense of CPython's `code_richcompare`:
    /// name, arity, flags, first line, instruction stream, constants
    /// (recursively), names, locals layout, line/position tables, and
    /// the exception table. Everything the wire encoding depends on.
    /// CPython's `compute_code_flags`: `co_flags` for this scope.
    /// Function-like scopes (functions, lambdas, comprehensions,
    /// annotation scopes) are `CO_OPTIMIZED | CO_NEWLOCALS`, carry
    /// `CO_NESTED` when enclosed by a function (PEP 3155's `<locals>`
    /// qualname segment records exactly that), and the 3.14
    /// docstring/method bits; module and class bodies report only the
    /// feature bits. `__future__` bits recorded at compile time ride
    /// along (RFC 0052).
    pub fn co_flags(&self) -> u32 {
        const CO_OPTIMIZED: u32 = 0x0001;
        const CO_NEWLOCALS: u32 = 0x0002;
        const CO_VARARGS: u32 = 0x0004;
        const CO_VARKEYWORDS: u32 = 0x0008;
        const CO_NESTED: u32 = 0x0010;
        const CO_GENERATOR: u32 = 0x0020;
        const CO_COROUTINE: u32 = 0x0080;
        const CO_ITERABLE_COROUTINE: u32 = 0x0100;
        const CO_ASYNC_GENERATOR: u32 = 0x0200;
        const CO_HAS_DOCSTRING: u32 = 0x0400_0000;
        const CO_METHOD: u32 = 0x0800_0000;
        let mut f = 0u32;
        if !self.is_class_body && self.name != "<module>" {
            f |= CO_OPTIMIZED | CO_NEWLOCALS;
            if self.is_nested {
                f |= CO_NESTED;
            }
            if self.has_docstring {
                f |= CO_HAS_DOCSTRING;
            }
            if self.is_method {
                f |= CO_METHOD;
            }
        }
        if self.has_varargs {
            f |= CO_VARARGS;
        }
        if self.has_varkeywords {
            f |= CO_VARKEYWORDS;
        }
        if self.is_generator {
            f |= CO_GENERATOR;
        }
        if self.is_coroutine {
            f |= CO_COROUTINE;
        }
        if self.is_iterable_coroutine {
            f |= CO_ITERABLE_COROUTINE;
        }
        if self.is_async_generator {
            f |= CO_ASYNC_GENERATOR;
        }
        f | self.future_flags
    }

    pub fn content_eq(&self, other: &Self) -> bool {
        self.name == other.name
            && self.qualname == other.qualname
            && self.arg_count == other.arg_count
            && self.posonly_count == other.posonly_count
            && self.kwonly_count == other.kwonly_count
            && self.has_varargs == other.has_varargs
            && self.has_varkeywords == other.has_varkeywords
            && self.is_class_body == other.is_class_body
            && self.is_generator == other.is_generator
            && self.is_coroutine == other.is_coroutine
            && self.is_async_generator == other.is_async_generator
            && self.is_iterable_coroutine == other.is_iterable_coroutine
            && self.has_docstring == other.has_docstring
            && self.is_method == other.is_method
            && self.is_nested == other.is_nested
            && self.future_flags == other.future_flags
            && self.instructions.len() == other.instructions.len()
            && self
                .instructions
                .iter()
                .zip(&other.instructions)
                .all(|(a, b)| a.op == b.op && a.arg == b.arg)
            && self.wire_marks == other.wire_marks
            && self.constants == other.constants
            && self.names == other.names
            && self.varnames == other.varnames
            && self.cellvars == other.cellvars
            && self.freevars == other.freevars
            && self.linetable == other.linetable
            && self.coltable == other.coltable
            && self.exception_table == other.exception_table
            && self.no_interrupt_jumps == other.no_interrupt_jumps
    }

    /// Find or insert a constant; returns its index.
    fn intern_constant(&mut self, c: Constant) -> u32 {
        for (i, existing) in self.constants.iter().enumerate() {
            if existing == &c {
                return i as u32;
            }
        }
        self.constants.push(c);
        (self.constants.len() - 1) as u32
    }

    fn intern_name(&mut self, n: &str) -> u32 {
        for (i, existing) in self.names.iter().enumerate() {
            if existing == n {
                return i as u32;
            }
        }
        self.names.push(n.to_owned());
        (self.names.len() - 1) as u32
    }

    /// Render this code object as a `dis`-style listing.
    pub fn format_dis(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!("Disassembly of <code object {}>:\n", self.name));
        for (offset, ins) in self.instructions.iter().enumerate() {
            out.push_str(&format!(
                "{:>5} {:>20} {:>6}  ",
                offset,
                ins.op.name(),
                ins.arg
            ));
            match ins.op {
                OpCode::LoadConst => {
                    if let Some(c) = self.constants.get(ins.arg as usize) {
                        out.push_str("(");
                        out.push_str(&format_constant(c));
                        out.push(')');
                    }
                }
                OpCode::LoadName
                | OpCode::StoreName
                | OpCode::DeleteName
                | OpCode::LoadGlobal
                | OpCode::StoreGlobal
                | OpCode::DeleteGlobal
                | OpCode::LoadAttr
                | OpCode::StoreAttr
                | OpCode::DeleteAttr
                | OpCode::ImportName
                | OpCode::ImportFrom => {
                    if let Some(n) = self.names.get(ins.arg as usize) {
                        out.push('(');
                        out.push_str(n);
                        out.push(')');
                    }
                }
                OpCode::LoadFast | OpCode::StoreFast | OpCode::DeleteFast => {
                    if let Some(n) = self.varnames.get(ins.arg as usize) {
                        out.push('(');
                        out.push_str(n);
                        out.push(')');
                    }
                }
                OpCode::LoadDeref | OpCode::StoreDeref | OpCode::LoadClosure => {
                    let combined: Vec<&String> =
                        self.cellvars.iter().chain(self.freevars.iter()).collect();
                    if let Some(n) = combined.get(ins.arg as usize) {
                        out.push('(');
                        out.push_str(n);
                        out.push(')');
                    }
                }
                _ => {}
            }
            out.push('\n');
        }
        out
    }
}

fn format_constant(c: &Constant) -> String {
    match c {
        Constant::None => "None".to_owned(),
        Constant::Bool(b) => if *b { "True" } else { "False" }.to_owned(),
        Constant::Int(i) => i.to_string(),
        Constant::BigInt(b) => b.to_string(),
        Constant::Float(f) => {
            if f.fract() == 0.0 && f.is_finite() {
                format!("{f:.1}")
            } else {
                f.to_string()
            }
        }
        Constant::Complex(real, imag) => {
            if *real == 0.0 {
                format!("{imag}j")
            } else {
                let sep = if imag.is_sign_positive() { "+" } else { "" };
                format!("({real}{sep}{imag}j)")
            }
        }
        Constant::Str(s) => format!("'{s}'"),
        Constant::WStr(cps) => {
            // Surrogate-bearing literal; render lone surrogates as `\uXXXX`
            // and scalar code points verbatim (best-effort, for disassembly).
            let mut s = String::from("'");
            for &cp in cps {
                match char::from_u32(cp) {
                    Some(ch) => s.push(ch),
                    None => s.push_str(&format!("\\u{cp:04x}")),
                }
            }
            s.push('\'');
            s
        }
        Constant::Bytes(_) => "b'...'".to_owned(),
        Constant::Tuple(items) => {
            let inner: Vec<_> = items.iter().map(format_constant).collect();
            format!("({})", inner.join(", "))
        }
        Constant::FrozenSet(items) => {
            let inner: Vec<_> = items.iter().map(format_constant).collect();
            format!("frozenset({{{}}})", inner.join(", "))
        }
        Constant::Code(co) => format!("<code object {}>", co.name),
        Constant::Ellipsis => "Ellipsis".to_owned(),
        Constant::Slice(parts) => format!(
            "slice({}, {}, {})",
            format_constant(&parts.0),
            format_constant(&parts.1),
            format_constant(&parts.2)
        ),
        Constant::Unmarshallable => "None".to_owned(),
    }
}

/// Constants embedded in a [`CodeObject`].
///
/// Includes nested [`CodeObject`]s so function definitions can carry
/// their compiled body as a constant (matching CPython's `co_consts`
/// containing nested code objects).
#[derive(Debug, Clone)]
pub enum Constant {
    None,
    Bool(bool),
    Int(i64),
    /// Arbitrary-precision integer (RFC 0019). Stored as a
    /// `num_bigint::BigInt` so the compiler can hand it to the VM
    /// directly without re-parsing.
    BigInt(num_bigint::BigInt),
    Float(f64),
    /// Complex literal `(real, imag)` (RFC 0019).
    Complex(f64, f64),
    Str(String),
    /// A `str` constant carrying at least one lone surrogate, which a Rust
    /// `String` cannot hold (see [`weavepy_parser::ast::Constant::WStr`]).
    /// Lowered to an `Object::WStr` at materialisation time; disjoint from
    /// [`Constant::Str`] (a surrogate-free value is always `Str`).
    WStr(Vec<u32>),
    Bytes(Vec<u8>),
    Tuple(Vec<Constant>),
    /// `frozenset` constant — no literal form; reaches the pool via
    /// `compile()` of a caller-built `ast.Constant` (or the `in (…)`
    /// peephole, matching CPython's frozenset conversion).
    FrozenSet(Vec<Constant>),
    /// Shared (`Arc`, matching the VM's `Object::Code` handle) so
    /// materialising `co_consts` hands out the *same* code object on
    /// every access — dis/inspect compare nested code objects by
    /// identity (test_dis builds its expected Instruction lists from
    /// `outer.__code__.co_consts[1]`).
    Code(std::sync::Arc<CodeObject>),
    Ellipsis,
    /// `slice` constant (CPython 3.14): `a[1:2]` with all-constant bounds
    /// compiles to `LOAD_CONST slice(1, 2, None); BINARY_OP []` instead of
    /// building the slice at runtime. Each bound is a nested constant
    /// (`Constant::None` for an omitted one). Marshals as `TYPE_SLICE`.
    Slice(Box<(Constant, Constant, Constant)>),
    /// A `co_consts` slot holding a value the constant pool cannot
    /// represent (a live type, a set of types, …), produced only by
    /// `code.replace(co_consts=…)` / `types.CodeType(…)` with arbitrary
    /// objects. Materialises as `None` at runtime; `marshal` refuses
    /// code objects containing it ("unmarshallable object"), matching
    /// CPython's `w_object` failing on the underlying value
    /// (test_marshal `test_unmarshallable [code]`, gh-106287).
    Unmarshallable,
}

impl PartialEq for Constant {
    fn eq(&self, other: &Self) -> bool {
        use Constant as C;
        match (self, other) {
            (C::None, C::None) => true,
            (C::Bool(a), C::Bool(b)) => a == b,
            (C::Int(a), C::Int(b)) => a == b,
            (C::BigInt(a), C::BigInt(b)) => a == b,
            (C::Float(a), C::Float(b)) => a.to_bits() == b.to_bits(),
            (C::Complex(ar, ai), C::Complex(br, bi)) => {
                ar.to_bits() == br.to_bits() && ai.to_bits() == bi.to_bits()
            }
            (C::Str(a), C::Str(b)) => a == b,
            (C::WStr(a), C::WStr(b)) => a == b,
            (C::Bytes(a), C::Bytes(b)) => a == b,
            (C::Tuple(a), C::Tuple(b)) => a == b,
            // `_PyCode_ConstantKey` keys a frozenset by its *contents*
            // (a frozenset of the element keys), so `{"True", "False",
            // "None"}` and `{"False", "None", "True"}` share one const
            // slot; the elements' source order is not part of the value.
            (C::FrozenSet(a), C::FrozenSet(b)) => {
                a.len() == b.len()
                    && a.iter().all(|x| b.contains(x))
                    && b.iter().all(|x| a.contains(x))
            }
            // CPython's `code_richcompare` compares code objects by
            // content, so the const-pool merge folds two identical
            // nested code objects into one slot (the `any(genexpr)`
            // speculation compiles its generator twice and lands on a
            // single const).
            (C::Code(a), C::Code(b)) => std::sync::Arc::ptr_eq(a, b) || a.content_eq(b),
            (C::Ellipsis, C::Ellipsis) => true,
            (C::Slice(a), C::Slice(b)) => a == b,
            // Cross-type equality is intentionally rejected so that
            // the const-pool deduplication preserves CPython's
            // `1 != 1.0` semantics for interned constants.
            _ => false,
        }
    }
}

impl From<AstConstant> for Constant {
    fn from(c: AstConstant) -> Self {
        match c {
            AstConstant::None => Self::None,
            AstConstant::Bool(b) => Self::Bool(b),
            AstConstant::Int(i) => Self::Int(i),
            AstConstant::BigInt(s) => match s.parse::<num_bigint::BigInt>() {
                Ok(b) => Self::BigInt(b),
                // The AST parser only produces a `BigInt` variant when
                // the string is well-formed; round-tripping should be
                // total. Defensive fallback to zero.
                Err(_) => Self::Int(0),
            },
            AstConstant::Complex(real, imag) => Self::Complex(real, imag),
            AstConstant::Float(f) => Self::Float(f),
            AstConstant::Str(s) => Self::Str(s),
            AstConstant::WStr(cps) => Self::WStr(cps),
            AstConstant::Bytes(b) => Self::Bytes(b),
            AstConstant::Tuple(xs) => Self::Tuple(xs.into_iter().map(Self::from).collect()),
            AstConstant::FrozenSet(xs) => Self::FrozenSet(xs.into_iter().map(Self::from).collect()),
            AstConstant::Ellipsis => Self::Ellipsis,
        }
    }
}

// ---------- compile flags (CPython `Include/cpython/compile.h`) ----------

/// CPython compiler-flag constants. The `CO_FUTURE_*` values are the
/// exact bits `__future__.CO_*` exposes and `co_flags` carries; the
/// `PyCF_*` values are the `compile()` control bits `ast` re-exports.
pub mod flags {
    pub const CO_FUTURE_DIVISION: u32 = 0x0002_0000;
    pub const CO_FUTURE_ABSOLUTE_IMPORT: u32 = 0x0004_0000;
    pub const CO_FUTURE_WITH_STATEMENT: u32 = 0x0008_0000;
    pub const CO_FUTURE_PRINT_FUNCTION: u32 = 0x0010_0000;
    pub const CO_FUTURE_UNICODE_LITERALS: u32 = 0x0020_0000;
    pub const CO_FUTURE_BARRY_AS_BDFL: u32 = 0x0040_0000;
    pub const CO_FUTURE_GENERATOR_STOP: u32 = 0x0080_0000;
    pub const CO_FUTURE_ANNOTATIONS: u32 = 0x0100_0000;

    /// All future-statement bits (CPython `PyCF_MASK`).
    pub const PYCF_MASK: u32 = CO_FUTURE_DIVISION
        | CO_FUTURE_ABSOLUTE_IMPORT
        | CO_FUTURE_WITH_STATEMENT
        | CO_FUTURE_PRINT_FUNCTION
        | CO_FUTURE_UNICODE_LITERALS
        | CO_FUTURE_BARRY_AS_BDFL
        | CO_FUTURE_GENERATOR_STOP
        | CO_FUTURE_ANNOTATIONS;
    /// Formerly-meaningful bits accepted and ignored (CPython
    /// `PyCF_MASK_OBSOLETE` — `CO_NESTED`).
    pub const PYCF_MASK_OBSOLETE: u32 = 0x0010;

    pub const PYCF_SOURCE_IS_UTF8: u32 = 0x0100;
    pub const PYCF_DONT_IMPLY_DEDENT: u32 = 0x0200;
    pub const PYCF_ONLY_AST: u32 = 0x0400;
    pub const PYCF_IGNORE_COOKIE: u32 = 0x0800;
    pub const PYCF_TYPE_COMMENTS: u32 = 0x1000;
    pub const PYCF_ALLOW_TOP_LEVEL_AWAIT: u32 = 0x2000;
    pub const PYCF_ALLOW_INCOMPLETE_INPUT: u32 = 0x4000;
    pub const PYCF_OPTIMIZED_AST: u32 = 0x8000 | PYCF_ONLY_AST;

    /// All non-future bits `compile()` accepts (CPython
    /// `PyCF_COMPILE_MASK`).
    pub const PYCF_COMPILE_MASK: u32 = PYCF_ONLY_AST
        | PYCF_ALLOW_TOP_LEVEL_AWAIT
        | PYCF_TYPE_COMMENTS
        | PYCF_DONT_IMPLY_DEDENT
        | PYCF_ALLOW_INCOMPLETE_INPUT
        | PYCF_OPTIMIZED_AST;

    /// Map a `__future__` feature name to its `CO_FUTURE_*` bit.
    /// Returns 0 for features that predate the flag scheme entirely
    /// (there are none — every known feature has a bit).
    pub fn future_feature_bit(name: &str) -> Option<u32> {
        Some(match name {
            "division" => CO_FUTURE_DIVISION,
            "absolute_import" => CO_FUTURE_ABSOLUTE_IMPORT,
            "with_statement" => CO_FUTURE_WITH_STATEMENT,
            "print_function" => CO_FUTURE_PRINT_FUNCTION,
            "unicode_literals" => CO_FUTURE_UNICODE_LITERALS,
            "barry_as_FLUFL" => CO_FUTURE_BARRY_AS_BDFL,
            "generator_stop" => CO_FUTURE_GENERATOR_STOP,
            "annotations" => CO_FUTURE_ANNOTATIONS,
            // `nested_scopes` / `generators` are always-on features with
            // no live bit in 3.x.
            "nested_scopes" | "generators" => 0,
            _ => return None,
        })
    }
}

/// Options threaded from `compile()` into the compiler (RFC 0052).
#[derive(Debug, Clone, Copy, Default)]
pub struct CompileOptions {
    /// `CO_FUTURE_*` bits active *before* the module's own
    /// `__future__` imports are folded in (inherited from the calling
    /// code or passed via `compile(flags=...)`), plus any `PyCF_*`
    /// control bits (`PyCF_ALLOW_TOP_LEVEL_AWAIT` is honoured here).
    pub flags: u32,
    /// Resolved optimization level (0, 1 or 2 — the caller resolves
    /// `-1` against the interpreter default before compiling).
    pub optimize: u8,
}

/// The per-compilation parameters shared by a top-level compiler and
/// every nested scope it spawns.
#[derive(Debug, Clone, Default)]
struct CompileParams {
    future_annotations: bool,
    optimize: u8,
    /// Merged `CO_FUTURE_*` bits (inherited + module's own imports),
    /// stamped onto every produced code object.
    future_flags: u32,
    allow_top_level_await: bool,
    /// Names bound by an `import`/`from-import` in the *module* scope
    /// (CPython symtable `DEF_IMPORT` on `st_top`). The method-call
    /// optimization refuses an attribute base that is import-originated
    /// (`maybe_optimize_method_call` → `is_import_originated`), so
    /// `os.getcwd()` compiles as `LOAD_GLOBAL os; PUSH_NULL; …`, not as
    /// a flagged method load.
    module_imports: Rc<HashSet<String>>,
    /// `True` when the name `super` appears *directly* in the module
    /// scope (CPython `can_optimize_super_call`'s shadowing check:
    /// `_PyST_GetScope(c->c_st->st_top, "super") != 0`). Any
    /// module-level mention — read or store, including in decorators,
    /// defaults and class bases, which evaluate in module scope —
    /// disables the `LOAD_SUPER_ATTR` lowering module-wide.
    module_mentions_super: bool,
}

// ---------- public entry point ----------

/// PEP 563: does this module open with `from __future__ import annotations`?
/// When it does, every annotation in the module (and all nested scopes) is
/// left *unevaluated* — the compiler stores its verbatim source text as a
/// string instead of emitting code to evaluate it at definition time. A
/// `__future__` import is only legal at the top of the module, so a single
/// scan of the module body suffices.
fn has_future_annotations(module: &Module) -> bool {
    module.body.iter().any(|stmt| {
        matches!(
            &stmt.kind,
            StmtKind::ImportFrom { module: Some(m), names, level: 0 }
                if m == "__future__" && names.iter().any(|a| a.name == "annotations")
        )
    })
}

/// The module's own `__future__` feature bits (`CO_FUTURE_*`). The
/// validator has already rejected unknown features and misplaced
/// imports by the time this runs, so a plain scan suffices.
fn module_future_flags(module: &Module) -> u32 {
    let mut bits = 0u32;
    for stmt in &module.body {
        if let StmtKind::ImportFrom {
            module: Some(m),
            names,
            level: 0,
        } = &stmt.kind
        {
            if m == "__future__" {
                for a in names {
                    bits |= flags::future_feature_bit(&a.name).unwrap_or(0);
                }
            }
        }
    }
    bits
}

thread_local! {
    /// PEP 563 active for the compilation currently running on this
    /// thread. Consulted by the free-variable scans
    /// ([`collect_reads_stmt`]) so stringified annotations don't
    /// contribute reads to scope analysis (no spurious cells for
    /// `def bar(): inner: outer = 1` — test_future's
    /// `test_annotations_symbol_table_pass`).
    static PEP563_ACTIVE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn pep563_active() -> bool {
    PEP563_ACTIVE.with(std::cell::Cell::get)
}

/// RAII guard installing the PEP 563 flag for the current compilation.
struct Pep563Guard(bool);

impl Pep563Guard {
    fn install(active: bool) -> Self {
        Pep563Guard(PEP563_ACTIVE.with(|c| c.replace(active)))
    }
}

impl Drop for Pep563Guard {
    fn drop(&mut self) {
        PEP563_ACTIVE.with(|c| c.set(self.0));
    }
}

/// Build the shared per-compilation parameters from caller options +
/// the module's own `__future__` imports.
fn make_params(module: &Module, opts: CompileOptions) -> CompileParams {
    let future_flags = (opts.flags & flags::PYCF_MASK) | module_future_flags(module);
    CompileParams {
        future_annotations: has_future_annotations(module)
            || future_flags & flags::CO_FUTURE_ANNOTATIONS != 0,
        optimize: opts.optimize,
        future_flags,
        allow_top_level_await: opts.flags & flags::PYCF_ALLOW_TOP_LEVEL_AWAIT != 0,
        module_imports: Rc::new(collect_module_imports(&module.body)),
        module_mentions_super: module_mentions_super(&module.body),
    }
}

/// Names bound by `import` / `from-import` statements in the module
/// scope, including those nested in `if`/`try`/`for`/`while`/`with`
/// blocks (same symtable scope) but not inside `def`/`class` bodies
/// (their own scopes). Mirrors CPython symtable's `DEF_IMPORT` flags
/// on the top-level scope, as consulted by `is_import_originated`.
fn collect_module_imports(body: &[Stmt]) -> HashSet<String> {
    fn walk(body: &[Stmt], out: &mut HashSet<String>) {
        for stmt in body {
            match &stmt.kind {
                StmtKind::Import(aliases) => {
                    for a in aliases {
                        match &a.asname {
                            Some(n) => {
                                out.insert(n.clone());
                            }
                            // `import a.b.c` binds the first segment.
                            None => {
                                let first = a.name.split('.').next().unwrap_or(&a.name);
                                out.insert(first.to_owned());
                            }
                        }
                    }
                }
                StmtKind::ImportFrom { names, .. } => {
                    for a in names {
                        if a.name == "*" {
                            continue;
                        }
                        out.insert(a.asname.clone().unwrap_or_else(|| a.name.clone()));
                    }
                }
                StmtKind::If { body, orelse, .. }
                | StmtKind::While { body, orelse, .. }
                | StmtKind::For { body, orelse, .. }
                | StmtKind::AsyncFor { body, orelse, .. } => {
                    walk(body, out);
                    walk(orelse, out);
                }
                StmtKind::Try {
                    body,
                    handlers,
                    orelse,
                    finalbody,
                } => {
                    walk(body, out);
                    for h in handlers {
                        walk(&h.body, out);
                    }
                    walk(orelse, out);
                    walk(finalbody, out);
                }
                StmtKind::With { body, .. } | StmtKind::AsyncWith { body, .. } => walk(body, out),
                StmtKind::Match { cases, .. } => {
                    for case in cases {
                        walk(&case.body, out);
                    }
                }
                _ => {}
            }
        }
    }
    let mut out = HashSet::new();
    walk(body, &mut out);
    out
}

/// `True` when the name `super` appears *directly* in the module scope
/// (any read, store or import binding). Mirrors the module-level half
/// of CPython's `can_optimize_super_call` shadowing check: the module
/// symtable owns names used in module-level statements — including
/// decorators, argument defaults/annotations and class bases, which
/// evaluate in module scope — but *not* names inside nested function,
/// class or lambda bodies (their own symtable scopes).
fn module_mentions_super(body: &[Stmt]) -> bool {
    fn in_expr(e: &Expr) -> bool {
        match &e.kind {
            ExprKind::Name(n) => n == "super",
            ExprKind::Constant(_) => false,
            ExprKind::Attribute { value, .. } | ExprKind::Starred(value) => in_expr(value),
            ExprKind::Subscript { value, slice } => in_expr(value) || in_expr(slice),
            ExprKind::Slice { lower, upper, step } => [lower, upper, step]
                .into_iter()
                .flatten()
                .any(|x| in_expr(x)),
            ExprKind::BinOp { left, right, .. } => in_expr(left) || in_expr(right),
            ExprKind::BoolOp { values, .. } => values.iter().any(in_expr),
            ExprKind::UnaryOp { operand, .. } => in_expr(operand),
            ExprKind::Compare {
                left, comparators, ..
            } => in_expr(left) || comparators.iter().any(in_expr),
            ExprKind::IfExp { test, body, orelse } => {
                in_expr(test) || in_expr(body) || in_expr(orelse)
            }
            ExprKind::NamedExpr { target, value } => in_expr(target) || in_expr(value),
            // Lambda bodies are their own symtable scope; only the
            // defaults evaluate (and are recorded) in this scope.
            ExprKind::Lambda { args, .. } | ExprKind::TypeParamFn { args, .. } => {
                args.defaults.iter().any(in_expr) || args.kw_defaults.iter().flatten().any(in_expr)
            }
            ExprKind::Call {
                func,
                args,
                keywords,
            } => {
                in_expr(func)
                    || args.iter().any(in_expr)
                    || keywords.iter().any(|k| in_expr(&k.value))
            }
            ExprKind::Tuple(items) | ExprKind::List(items) | ExprKind::Set(items) => {
                items.iter().any(in_expr)
            }
            ExprKind::Dict { keys, values } => {
                keys.iter().flatten().any(in_expr) || values.iter().any(in_expr)
            }
            // PEP 709: list/set/dict comprehensions are inlined into
            // the enclosing scope in 3.13, so all their names count.
            ExprKind::ListComp { elt, generators } | ExprKind::SetComp { elt, generators } => {
                in_expr(elt) || generators.iter().any(in_comp)
            }
            ExprKind::DictComp {
                key,
                value,
                generators,
            } => in_expr(key) || in_expr(value) || generators.iter().any(in_comp),
            // Generator expressions stay a separate scope; only the
            // first (eagerly evaluated) iterable belongs here.
            ExprKind::GeneratorExp { generators, .. } => {
                generators.first().is_some_and(|g| in_expr(&g.iter))
            }
            ExprKind::Yield(v) => v.as_deref().is_some_and(in_expr),
            ExprKind::YieldFrom(v) | ExprKind::Await(v) => in_expr(v),
            ExprKind::JoinedStr(parts) | ExprKind::TemplateStr(parts) => parts.iter().any(in_expr),
            ExprKind::FormattedValue {
                value, format_spec, ..
            }
            | ExprKind::Interpolation {
                value, format_spec, ..
            } => in_expr(value) || format_spec.as_deref().is_some_and(in_expr),
        }
    }
    fn in_comp(g: &Comprehension) -> bool {
        in_expr(&g.target) || in_expr(&g.iter) || g.ifs.iter().any(in_expr)
    }
    fn in_args_outer(args: &AstArguments) -> bool {
        // Defaults and annotations evaluate in the enclosing scope.
        args.defaults.iter().any(in_expr)
            || args.kw_defaults.iter().flatten().any(in_expr)
            || args
                .posonlyargs
                .iter()
                .chain(&args.args)
                .chain(&args.kwonlyargs)
                .chain(&args.vararg)
                .chain(&args.kwarg)
                .any(|a| a.annotation.as_deref().is_some_and(in_expr))
    }
    fn in_stmt(s: &Stmt) -> bool {
        match &s.kind {
            // Nested function/class bodies are separate scopes; their
            // decorators, defaults, annotations and bases are ours.
            StmtKind::FunctionDef {
                args,
                decorator_list,
                returns,
                ..
            }
            | StmtKind::AsyncFunctionDef {
                args,
                decorator_list,
                returns,
                ..
            } => {
                decorator_list.iter().any(in_expr)
                    || in_args_outer(args)
                    || returns.as_deref().is_some_and(in_expr)
            }
            StmtKind::ClassDef {
                bases,
                keywords,
                decorator_list,
                ..
            } => {
                decorator_list.iter().any(in_expr)
                    || bases.iter().any(in_expr)
                    || keywords.iter().any(|k| in_expr(&k.value))
            }
            // The alias value is a lazy PEP 695 annotation scope.
            StmtKind::TypeAlias { .. } => false,
            StmtKind::Return(v) => v.as_ref().is_some_and(in_expr),
            StmtKind::Assign { targets, value } => targets.iter().any(in_expr) || in_expr(value),
            StmtKind::AugAssign { target, value, .. } => in_expr(target) || in_expr(value),
            StmtKind::AnnAssign {
                target,
                annotation,
                value,
                ..
            } => in_expr(target) || in_expr(annotation) || value.as_ref().is_some_and(in_expr),
            StmtKind::If { test, body, orelse } | StmtKind::While { test, body, orelse } => {
                in_expr(test) || body.iter().any(in_stmt) || orelse.iter().any(in_stmt)
            }
            StmtKind::For {
                target,
                iter,
                body,
                orelse,
            }
            | StmtKind::AsyncFor {
                target,
                iter,
                body,
                orelse,
            } => {
                in_expr(target)
                    || in_expr(iter)
                    || body.iter().any(in_stmt)
                    || orelse.iter().any(in_stmt)
            }
            StmtKind::Try {
                body,
                handlers,
                orelse,
                finalbody,
            } => {
                body.iter().any(in_stmt)
                    || handlers.iter().any(|h| {
                        h.type_.as_ref().is_some_and(in_expr)
                            || h.name.as_deref() == Some("super")
                            || h.body.iter().any(in_stmt)
                    })
                    || orelse.iter().any(in_stmt)
                    || finalbody.iter().any(in_stmt)
            }
            StmtKind::Raise { exc, cause } => {
                exc.as_ref().is_some_and(in_expr) || cause.as_ref().is_some_and(in_expr)
            }
            StmtKind::With { items, body } | StmtKind::AsyncWith { items, body } => {
                items.iter().any(|it| {
                    in_expr(&it.context_expr) || it.optional_vars.as_ref().is_some_and(in_expr)
                }) || body.iter().any(in_stmt)
            }
            StmtKind::Import(aliases) => aliases.iter().any(|a| {
                a.asname.as_deref() == Some("super")
                    || (a.asname.is_none() && a.name.split('.').next() == Some("super"))
            }),
            StmtKind::ImportFrom { names, .. } => aliases_bind_super(names),
            StmtKind::Global(names) | StmtKind::Nonlocal(names) => {
                names.iter().any(|n| n == "super")
            }
            StmtKind::Match { subject, cases } => {
                in_expr(subject)
                    || cases.iter().any(|c| {
                        c.guard.as_ref().is_some_and(in_expr) || c.body.iter().any(in_stmt)
                    })
            }
            StmtKind::Expr(e) => in_expr(e),
            StmtKind::Delete(targets) => targets.iter().any(in_expr),
            StmtKind::Assert { test, msg } => in_expr(test) || msg.as_ref().is_some_and(in_expr),
            StmtKind::Pass | StmtKind::Break | StmtKind::Continue => false,
        }
    }
    fn aliases_bind_super(names: &[weavepy_parser::ast::Alias]) -> bool {
        names
            .iter()
            .any(|a| a.asname.as_deref().unwrap_or(&a.name) == "super")
    }
    body.iter().any(in_stmt)
}

/// Run only the parse-adjacent validation pass (the symtable-stage
/// checks CPython performs while *building* the symbol table:
/// `global`/`nonlocal` directive conflicts, `__future__` placement,
/// annotation-scope restrictions, …) without generating code.
/// `_symtable.symtable()` uses this so symtable-build-time
/// `SyntaxError`s surface with CPython's messages and locations.
pub fn validate_module_only(module: &Module, source: &str) -> Result<(), CompileError> {
    let params = make_params(module, CompileOptions::default());
    validate::validate_module(module, source, params.future_annotations)
}

/// Compile a parsed module into a top-level [`CodeObject`].
pub fn compile_module(module: &Module) -> Result<CodeObject, CompileError> {
    compile_module_with_filename(module, "<module>")
}

/// As [`compile_module`] but lets the caller name the source file
/// (used in the `dis` listing).
pub fn compile_module_with_filename(
    module: &Module,
    filename: &str,
) -> Result<CodeObject, CompileError> {
    compile_module_with_source(module, "", filename)
}

/// Compile with access to the original source so the resulting code
/// object can carry per-instruction line numbers for tracebacks.
pub fn compile_module_with_source(
    module: &Module,
    source: &str,
    filename: &str,
) -> Result<CodeObject, CompileError> {
    compile_module_with_options(module, source, filename, CompileOptions::default())
}

/// As [`compile_module_with_source`] with explicit `compile()` options
/// (future/`PyCF_*` flags + optimization level) — RFC 0052.
pub fn compile_module_with_options(
    module: &Module,
    source: &str,
    filename: &str,
    opts: CompileOptions,
) -> Result<CodeObject, CompileError> {
    let params = make_params(module, opts);
    let _pep563 = Pep563Guard::install(params.future_annotations);
    validate::validate_module(module, source, params.future_annotations)?;
    let mut folded = module.clone();
    ast_opt::fold_module(&mut folded, params.future_annotations);
    let module = &folded;
    let line_index = LineIndex::new(source);
    let mut top = Compiler::new(
        "<module>".to_owned(),
        filename.to_owned(),
        CodeKind::Module,
        Rc::new(line_index),
        Rc::from(source),
        params,
    );
    top.compile_module_body(module)?;
    Ok(top.finish())
}

/// Compile in interactive ("single") mode: identical to
/// [`compile_module_with_source`] except top-level expression
/// statements echo their value through `sys.displayhook`
/// (`OpCode::PrintExpr`) the way CPython's `compile(src, fn, "single")`
/// does. Powers the REPL (`code`/`codeop`) and `doctest`.
pub fn compile_interactive_with_source(
    module: &Module,
    source: &str,
    filename: &str,
) -> Result<CodeObject, CompileError> {
    compile_interactive_with_options(module, source, filename, CompileOptions::default())
}

/// As [`compile_interactive_with_source`] with explicit options.
pub fn compile_interactive_with_options(
    module: &Module,
    source: &str,
    filename: &str,
    opts: CompileOptions,
) -> Result<CodeObject, CompileError> {
    let params = make_params(module, opts);
    let _pep563 = Pep563Guard::install(params.future_annotations);
    validate::validate_module(module, source, params.future_annotations)?;
    let mut folded = module.clone();
    ast_opt::fold_module(&mut folded, params.future_annotations);
    let module = &folded;
    let line_index = LineIndex::new(source);
    let mut top = Compiler::new(
        "<module>".to_owned(),
        filename.to_owned(),
        CodeKind::Module,
        Rc::new(line_index),
        Rc::from(source),
        params,
    );
    top.interactive = true;
    top.compile_module_body(module)?;
    Ok(top.finish())
}

/// Compile in `eval` mode: the single top-level expression *returns* its
/// value (via `OpCode::ReturnValue`) so the resulting code object,
/// evaluated by `eval(...)`, produces the expression result rather than
/// discarding it. Mirrors CPython's `compile(src, fn, "eval")`.
pub fn compile_eval_with_source(
    module: &Module,
    source: &str,
    filename: &str,
) -> Result<CodeObject, CompileError> {
    compile_eval_with_options(module, source, filename, CompileOptions::default())
}

/// As [`compile_eval_with_source`] with explicit options.
pub fn compile_eval_with_options(
    module: &Module,
    source: &str,
    filename: &str,
    opts: CompileOptions,
) -> Result<CodeObject, CompileError> {
    // CPython's `eval` grammar only admits a single expression; any
    // statement syntax (`del x`, `x = 1`, a second statement, …) is a
    // bare "invalid syntax" pointing at the first token the expression
    // grammar can't accept — *before* any statement-level diagnostics
    // like "cannot delete f-string expression" can fire.
    if let Some(bad) = eval_mode_invalid_stmt(module) {
        let span = match &bad.kind {
            // The expression grammar chokes on the `=`: locate it
            // between the last target and the value.
            StmtKind::Assign { targets, value } => {
                let from = targets
                    .last()
                    .map(|t| t.span.end.0 as usize)
                    .unwrap_or(bad.span.start.0 as usize);
                let to = value.span.start.0 as usize;
                let eq = source
                    .get(from..to)
                    .and_then(|s| s.find('='))
                    .map(|i| (from + i) as u32)
                    .unwrap_or(bad.span.start.0);
                weavepy_lexer::Span::new(eq, eq + 1)
            }
            // Statement keywords (`del`, `pass`, …) span the keyword.
            StmtKind::Delete(_) => weavepy_lexer::Span::new(bad.span.start.0, bad.span.start.0 + 3),
            _ => weavepy_lexer::Span::new(bad.span.start.0, bad.span.start.0 + 1),
        };
        return Err(CompileError::parser_spanned(
            "invalid syntax".to_owned(),
            span,
        ));
    }
    let params = make_params(module, opts);
    let _pep563 = Pep563Guard::install(params.future_annotations);
    validate::validate_module(module, source, params.future_annotations)?;
    let mut folded = module.clone();
    ast_opt::fold_module(&mut folded, params.future_annotations);
    let module = &folded;
    let line_index = LineIndex::new(source);
    let mut top = Compiler::new(
        "<module>".to_owned(),
        filename.to_owned(),
        CodeKind::Module,
        Rc::new(line_index),
        Rc::from(source),
        params,
    );
    top.eval_mode = true;
    top.compile_module_body(module)?;
    Ok(top.finish())
}

/// The first statement that breaks `eval` mode's single-expression
/// shape: any non-`Expr` statement, or any statement after the first.
fn eval_mode_invalid_stmt(module: &Module) -> Option<&Stmt> {
    for (i, stmt) in module.body.iter().enumerate() {
        if i > 0 || !matches!(stmt.kind, StmtKind::Expr(_)) {
            return Some(stmt);
        }
    }
    None
}

/// Lookup table that maps a byte offset back to a 1-based line number.
/// Filled once per top-level compile and shared by reference into every
/// nested `Compiler` for cheap per-instruction line lookups.
#[derive(Debug, Default)]
struct LineIndex {
    line_starts: Vec<u32>,
}

impl LineIndex {
    fn new(source: &str) -> Self {
        // `\n`, `\r\n`, and lone `\r` all terminate a line, matching the
        // tokenizer's universal-newline handling.
        let bytes = source.as_bytes();
        let mut starts = vec![0u32];
        for (i, &b) in bytes.iter().enumerate() {
            if b == b'\n' || (b == b'\r' && bytes.get(i + 1) != Some(&b'\n')) {
                starts.push((i + 1) as u32);
            }
        }
        Self {
            line_starts: starts,
        }
    }

    fn line_for(&self, byte: u32) -> u32 {
        if self.line_starts.is_empty() {
            return 0;
        }
        let idx = self
            .line_starts
            .partition_point(|&start| start <= byte)
            .saturating_sub(1);
        (idx as u32) + 1
    }

    /// 1-based line and 0-based byte column for a source byte offset.
    /// Returns `(0, 0)` when the index is empty.
    fn pos_for(&self, byte: u32) -> (u32, u32) {
        let line = self.line_for(byte);
        if line == 0 {
            return (0, 0);
        }
        let line_start = self.line_starts[(line - 1) as usize];
        (line, byte.saturating_sub(line_start))
    }
}

// ---------- scope kinds ----------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CodeKind {
    Module,
    Function,
    Comprehension,
    Class,
}

/// One symbol an inlined comprehension binds, as
/// `codegen_push_inlined_comprehension_locals` treats it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CompSym {
    /// An iteration variable: a hidden fast local of the enclosing
    /// scope, made a cell (`MAKE_CELL` after the save) when a real
    /// scope nested in the comprehension closes over it.
    Target { cell: bool },
    /// A walrus target the enclosing scope resolves as an explicit
    /// global: saved and restored like a local, stored through
    /// `STORE_GLOBAL`.
    GlobalWalrus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Binding {
    Local,
    Global,
    /// Reserved for a future pass that distinguishes `nonlocal x`
    /// from a regular free variable in error messages. Today we
    /// collapse it into `Free` during scope analysis.
    #[allow(dead_code)]
    Nonlocal,
    Free,
    Cell,
    /// Class-body only: a name that is *assigned in the class body*
    /// (so the body's own loads/stores use the namespace, like
    /// `Global`) but that nested scopes must still resolve to an
    /// enclosing function's cell — Python skips class scopes when
    /// binding closures (`def f(): T = str; class C: T = int;
    /// def m(): return T` → `m` sees `str`). The name sits in the
    /// class body's `free_order` purely to *forward* the enclosing
    /// cell to those nested scopes.
    ClassPassthrough,
}

// ---------- compiler ----------

struct Compiler {
    co: CodeObject,
    kind: CodeKind,
    /// Which comprehension form this scope lowers (`None` outside
    /// comprehensions). Drives CPython's "'yield' inside list
    /// comprehension"-style messages.
    comp_kind: Option<CompKind>,
    /// Name → binding for the current scope.
    bindings: IndexMap<String, Binding>,
    /// Names declared `global` by an explicit `global` statement in this
    /// scope. A nested `def`/`class` whose name is in this set gets a bare
    /// `__qualname__` (CPython's `compiler_set_qualname` GLOBAL_EXPLICIT
    /// rule), which is what makes `global P; class P: ...` pickleable.
    explicit_globals: HashSet<String>,
    /// Class scopes only: names whose class-level binding is an explicit
    /// `global`, but where an *enclosing function* binds the same name.
    /// PEP 227 makes class scopes invisible to nested scopes, so a
    /// comprehension/def below the class still closes over the enclosing
    /// function's cell — the class forwards it via `free_order` without
    /// changing its own (global) loads and stores.
    class_transparent_frees: HashSet<String>,
    /// Free variables (in declaration order) — populated by inner
    /// scopes looking up to their lexical parents.
    free_order: Vec<String>,
    /// Loop stack: each frame holds (continue_target, break_patch_sites).
    loop_stack: Vec<LoopFrame>,
    /// Pending `finally` clauses, innermost last. Used by
    /// `return`/`break`/`continue` to inline their bodies on exit so
    /// the cleanup runs even when the try body is being short-circuited.
    finally_stack: Vec<FinallyFrame>,
    /// Monotonic id stamped on every [`FinallyFrame`] when it is pushed.
    /// Used to associate a return/break/continue-path *inline copy* of a
    /// finally body with the specific `try`/`with` it belongs to.
    next_finally_id: u32,
    /// PC ranges where a `finally` (or `with`-exit) body was inlined for a
    /// non-exceptional exit (`return`/`break`/`continue`), keyed by the
    /// owning frame's id. A `raise` inside such an inlined body must
    /// propagate to an *enclosing* try — not re-enter (and re-run) the
    /// same try's finally — so [`Self::push_body_exc_entries`] punches
    /// these ranges out of that try's exception-table coverage, matching
    /// CPython (whose return-path finally lives outside the protected
    /// body range). Each entry is `(frame_id, start, end)`.
    finally_holes: Vec<(u32, u32, u32)>,
    /// Source byte→line table shared by every nested compiler from the
    /// same `compile_module_*` call.
    line_index: Rc<LineIndex>,
    /// Line number assigned to the next emitted instruction; updated as
    /// the compiler descends through the AST.
    current_line: u32,
    /// When set, every emitted instruction gets exactly this line
    /// (and no column span), regardless of `current_line`/AST spans.
    /// `Some(0)` emits "no line" — CPython's `NO_LOCATION`, used for
    /// handler-entry `PUSH_EXC_INFO` and synthetic cleanup blocks so
    /// they never surface as `'line'` trace events (RFC 0051 WS4).
    line_pinned: Option<u32>,
    /// Column span accompanying a *nonzero* [`Self::line_pinned`]:
    /// CPython's statement-level locations for synthetic cleanup carry
    /// the full location of the anchoring statement, never a bare line
    /// (co_positions None-counts are 0, 3 or 4 — test_code's
    /// test_co_positions_artificial_instructions).
    pinned_colspan: ColSpan,
    /// Source byte span `(start, end)` for the AST node currently being
    /// emitted. Drives PEP-657 column tracking in [`Self::emit`]. Updated
    /// at statement and expression granularity as the compiler descends.
    current_span: (u32, u32),
    /// Unconditional jumps CPython emits as `JUMP_NO_INTERRUPT`
    /// (synthetic scope exits: handler exits, `with`-suppress exits,
    /// the send-dance edges). A backward one encodes on the wire as
    /// `JUMP_BACKWARD_NO_INTERRUPT`; forward it is indistinguishable
    /// from `JUMP_FORWARD`. The flowgraph never threads a conditional
    /// through one of these (RFC 0077 WS9).
    no_interrupt_jumps: HashSet<u32>,
    /// Unconditional jumps CPython's codegen emits as the plain,
    /// eval-breaker-polling `JUMP` pseudo-op (loop back edges,
    /// `break`/`continue`, `match` case-body exits, the inlined
    /// `all`/`any` early exits). Every unconditional jump in neither
    /// set is classified structurally by the flowgraph builder.
    plain_jumps: HashSet<u32>,
    /// `Nop`s standing in for a located `SETUP_*` pseudo-op (the `try:`
    /// line's NOP is really `SETUP_FINALLY`). The flowgraph keeps them
    /// as block-push instructions until `convert_pseudo_ops`.
    setup_nops: std::collections::HashMap<u32, OpCode>,
    /// Suppress-path exit jumps parked by
    /// [`Self::emit_with_except_finish`] for its caller to patch.
    with_exit_jumps: Vec<u32>,
    /// `Nop`s standing in for a `POP_BLOCK`: `label_exception_targets`
    /// turns the pseudo-op into a NOP without assigning `i_except`.
    popblock_nops: HashSet<u32>,
    /// Conditional jumps that stand for the `JUMP_IF_FALSE` /
    /// `JUMP_IF_TRUE` pseudo-ops of a value-position `and` / `or`,
    /// together with the `CopyTop 1; ToBool` emitted in front of them.
    /// CPython's codegen emits the pseudo-op alone and
    /// `convert_pseudo_conditional_jumps` inserts the two instructions
    /// late; the flowgraph builder folds the three back into the
    /// pseudo-op so the passes see CPython's shape.
    pseudo_cond_jumps: HashSet<u32>,
    /// Conditional jumps whose target lies *before* them (the inlined
    /// `all`/`any` loop test): their `arg` is the backward distance.
    /// CPython's `normalize_jumps` inverts them over a trampoline.
    backward_conds: HashSet<u32>,
    /// Whether [`Self::finish`] wraps a generator-family body in the
    /// PEP 479 `STOPITERATION_ERROR` handler (`codegen_wrap_in_
    /// stopiteration_handler`): function bodies and generator
    /// expressions do, lambdas do not.
    stopiteration_wrap: bool,
    /// Nesting depth of PEP 709 *inlined* comprehension emission.
    /// While > 0, `compile_comp_body` skips the `.0`-argument dance at
    /// generator depth 0 (the caller pushed the ready iterator) and
    /// registers its exception handlers with
    /// [`HANDLER_DEPTH_SENTINEL`] depths for `finish` to resolve.
    inline_comp: u32,
    /// Plain (non-cell) iteration targets of the inlined comprehensions
    /// currently being emitted *in module scope*. CPython's
    /// `push_inlined_comprehension_state` temporarily overwrites the
    /// enclosing scope's symbol entry for every name whose scope differs
    /// inside the comprehension; in module scope that enclosing entry is
    /// `st_top`, the very table `is_import_originated` consults, so an
    /// import-bound base (`_opcode.has_arg(op)` in `opcode.py`'s
    /// `hasarg` comprehension) *does* take the method-call form there.
    /// The one shape that keeps its `DEF_IMPORT` flag is a plain target
    /// (LOCAL both inside and out, so nothing is overwritten).
    module_comp_plain_targets: Vec<String>,
    /// Number of *live exception values* sitting on the operand stack at
    /// the current compile point: a `finally` body (or the unmatched
    /// re-raise path of a `try/except`) runs with the propagating
    /// exception on the stack until the trailing `RERAISE` pops it.
    /// Exception-table entries registered for code nested inside such
    /// regions must include these slots in their `depth`, or the
    /// dispatch loop would truncate the live exception away and the
    /// `RERAISE` would underflow.
    exc_on_stack: u32,
    /// One hole id per live exception region (parallel to
    /// [`Self::exc_on_stack`]): the region's own `SETUP_CLEANUP`
    /// coverage is punched from the `POP_EXCEPT` of every
    /// return/break/continue unwind that leaves it (CPython's
    /// `FINALLY_END` fblock pops the block there).
    exc_region_ids: Vec<(u32, u32)>,
    /// Push-order stamps of the pending return values riding the
    /// operand stack (parallel to [`Self::pending_retvals`]).
    rv_seqs: Vec<u32>,
    /// Monotonic stamp handed to every unwindable block on push
    /// (loops, finally frames, exception regions, pending return
    /// values), so an unwind can replay CPython's single fblock stack
    /// in true recency order across WeavePy's per-kind stacks.
    fblock_seq: u32,
    /// Number of `except` handler bodies (each with a live
    /// `PUSH_EXC_INFO` entry) enclosing the current compile point.
    /// `break`/`continue` jumping out of a handler must POP_EXCEPT
    /// the levels they exit.
    handler_depth: u32,
    /// Number of pending return values on the operand stack at the
    /// current compile point: while `return` inlines its `finally`
    /// bodies, the value being returned stays on the stack under them.
    /// A `break`/`continue` inside such an inline abandons the return
    /// and must POP_TOP the pending value(s)
    /// (test_grammar.test_break_in_finally_after_return).
    pending_retvals: u32,
    /// `True` for methods compiled inside a class body. Such methods
    /// implicitly capture the class's `__class__` cell so `super()`
    /// works without arguments.
    inside_class_body: bool,
    /// Tracks whether this scope's `__annotations__` dict has been
    /// initialised yet (lazily, on the first `x: T` statement in a
    /// class or module body). Used by
    /// [`Self::compile_annotation_record`].
    annotations_initialized: bool,
    /// Mirror of [`Self::code_kind`] used by annotation logic; we
    /// expose it here rather than threading the value through every
    /// call site.
    code_kind: CodeKind,
    /// `True` for the top-level code object compiled in interactive
    /// ("single") mode. Module-level expression *statements* then echo
    /// their value through `sys.displayhook` (via `OpCode::PrintExpr`)
    /// instead of being discarded — the REPL / `code` / `doctest`
    /// behaviour. Never set on nested function/class scopes (they get
    /// fresh `Compiler` instances), matching CPython's
    /// `c_interactive && nestlevel <= 1` rule.
    interactive: bool,
    /// `True` for the top-level code object compiled in `eval` mode.
    /// The (single) top-level expression *returns* its value via
    /// `OpCode::ReturnValue` so `eval(compile(src, fn, "eval"))` yields
    /// the expression result instead of discarding it. Never set on
    /// nested scopes.
    eval_mode: bool,
    /// The original module source. Used to slice the verbatim text of an
    /// annotation under PEP 563 (see [`Self::future_annotations`]). Empty
    /// when the caller compiled without source (then PEP 563 is inert).
    source: Rc<str>,
    /// PEP 563 (`from __future__ import annotations`): when set, parameter
    /// and variable annotations are emitted as their unevaluated source
    /// strings rather than being evaluated at definition time. Propagated
    /// to every nested function/class scope.
    future_annotations: bool,
    /// Per-compilation options shared with every nested scope
    /// (optimize level, merged `CO_FUTURE_*` bits, top-level-await
    /// permission) — RFC 0052.
    params: CompileParams,
    /// CPython `ste_private`: the name of the innermost enclosing class,
    /// inherited by every scope textually inside it. Used to *demangle*
    /// def/class binding names back to their source spelling for
    /// `__name__`/`__qualname__` (the AST pass mangles the bindings).
    private: Option<Rc<str>>,
    /// Set while *this* compiler is a PEP 695 hidden scope
    /// (`<generic parameters of X>`), an `__annotate__`, or a PEP 695
    /// thunk: CPython `compiler_set_qualname` looks through a
    /// `COMPILE_SCOPE_ANNOTATIONS` parent to the grandparent, so this
    /// scope's children take the *third* element as their qualname
    /// prefix (the grandparent's base: `outer.<locals>.`, `C.`, `` at
    /// module level, or `<generic parameters of X>.` when the
    /// grandparent is itself an annotation scope, which never gets
    /// `.<locals>`). A hidden scope also stores `(X's display name,
    /// the qualname X gets in the enclosing scope)` for its wrapped
    /// statement (the enclosing scope's `global X` still applies).
    /// [`Self::compute_child_qualname`] consults this first.
    annotation_qualname: Option<(String, String, String)>,
    /// Set while *this* compiler is a PEP 695 hidden scope: the name
    /// of the generic `def`/`class`/`type` statement it wraps. The
    /// statement's *value* is returned from the hidden scope and bound
    /// by the enclosing scope, so the name is not a local of the hidden
    /// scope even though the analysis body contains the definition
    /// (symtable: `symtable_add_def` of the name happens in the
    /// enclosing block, before `symtable_enter_type_param_block`).
    pep695_unbound: Option<String>,
    /// Set while *this* compiler is a PEP 695 hidden scope for a
    /// generic `def` whose defaults were hoisted into the hidden
    /// scope's `.defaults` / `.kwdefaults` parameters: the
    /// `MAKE_FUNCTION` flags they satisfy (`0x01` / `0x02`).
    /// [`Self::build_function_object_full`] takes this and loads the
    /// parameters instead of compiling the default expressions
    /// (`codegen_function`: `codegen_default_arguments` runs in the
    /// *enclosing* scope; the hidden scope re-pushes them with
    /// `LOAD_FAST`).
    pep695_defaults: Option<u32>,
    /// Set while *this* compiler is a PEP 695 annotation scope that
    /// can see a class namespace (CPython `ste_can_see_class_scope`):
    /// a hidden `<generic parameters of X>` scope, a type-param
    /// bound/default thunk, or a `type` alias thunk, textually inside
    /// a class body. Name loads that would be `LoadGlobal`/`LoadDeref`
    /// instead consult the `__classdict__` cell first
    /// (`LOAD_FROM_DICT_OR_{GLOBALS,DEREF}`).
    lazy_class_ctx: Option<Rc<LazyClassCtx>>,
    /// Handoff slot for [`Self::lazy_class_ctx`]: set on the parent
    /// just before a child scope is created, which takes it.
    pending_lazy_class_ctx: Option<Rc<LazyClassCtx>>,
    /// Class-body compilers only: every name assigned at the body's
    /// own level. Feeds [`LazyClassCtx::assigned`].
    class_assigned: HashSet<String>,
    /// PEP 649 (RFC 0077 WS10): the simple-name annotated assignments
    /// this module or class body has met so far, in source order
    /// (CPython `u_deferred_annotations`). Compiled into the body's
    /// `__annotate__` function once every statement has been visited.
    deferred_annotations: Vec<DeferredAnnotation>,
    /// Next `__conditional_annotations__` index to hand out
    /// (`u_next_conditional_annotation_index`).
    next_cond_annotation_index: u32,
    /// Depth of enclosing `if`/`for`/`while`/`try`/`with`/`match`
    /// statements in this block (`u_in_conditional_block`): a class
    /// body annotation met inside one is only recorded in
    /// `__annotations__` when its statement actually executed.
    in_conditional_block: u32,
}

/// One module- or class-level `name: annotation` deferred into the
/// block's `__annotate__` function (PEP 649).
#[derive(Debug, Clone)]
struct DeferredAnnotation {
    /// The (already mangled) key in the annotations dict.
    name: String,
    annotation: Expr,
    /// The annotated statement's span (`LOC(st)` in
    /// `codegen_deferred_annotations_body`).
    span: weavepy_lexer::Span,
    /// Index in the block's `__conditional_annotations__` set, when the
    /// annotation is conditional (always at module level; inside a
    /// compound statement in a class body).
    cond_index: Option<u32>,
}

/// The `loc` CPython threads through an annotation scope's prologue,
/// epilogue, and closure construction: a full source span (a `def`
/// statement, or a module's first statement), or a bare line with an
/// empty column span (a class body's `LOCATION(firstlineno,
/// firstlineno, 0, 0)`).
#[derive(Debug, Clone, Copy)]
struct AnnotateLoc {
    line: u32,
    span: Option<weavepy_lexer::Span>,
}

/// `_Py_ANNOTATE_FORMAT_VALUE_WITH_FAKE_GLOBALS`: the largest
/// `annotationlib.Format` a compiler-generated `__annotate__` handles
/// itself; anything above raises `NotImplementedError`.
const ANNOTATE_FORMAT_VALUE_WITH_FAKE_GLOBALS: u32 = 2;

/// How a PEP 695 annotation scope resolves names against the class
/// body it can see (CPython's symtable consults the class block's
/// symbols — `analyze_name`'s `class_entry` shortcut — when
/// compiling such scopes).
struct LazyClassCtx {
    /// Names assigned in the class body (excluding explicit globals):
    /// these load as `LOAD_FROM_DICT_OR_GLOBALS` — the class dict
    /// first, then *globals* — never an enclosing function's cell,
    /// even if one exists (`test_binding_uses_global`).
    assigned: HashSet<String>,
    /// Names declared `global` in the class body: plain `LoadGlobal`,
    /// skipping the class dict (`test_explicit_global`).
    globals: HashSet<String>,
}

struct LoopFrame {
    /// Offset of the first instruction of the loop body — branched
    /// to by `continue` and at the bottom of the loop after each
    /// iteration.
    continue_target: u32,
    /// Sites that need to be patched to jump past the loop on `break`.
    break_sites: Vec<u32>,
    /// `for` loops keep the iterator on the stack between iterations.
    /// `break` therefore needs to drop it.
    is_for_loop: bool,
    /// `exc_on_stack` when the loop was entered. `break`/`continue`
    /// from inside a `finally` body running on the *exception path*
    /// must discard both the propagating exception object (still on
    /// the value stack, awaiting the RERAISE we're now skipping) and
    /// its PUSH_EXC_INFO handler state — CPython's unwind of the
    /// EXCEPTION_HANDLER fblock (`for … try: 1/0 finally: continue`,
    /// test_grammar.test_continue_in_finally).
    exc_on_stack_at_entry: u32,
    /// `pending_retvals` when the loop was entered. `break`/`continue`
    /// from inside a `finally` body inlined on a *return path* must
    /// discard the pending return value(s) sitting on the operand
    /// stack under the loop's frame (the return is abandoned —
    /// test_grammar.test_break_in_finally_after_return).
    pending_retvals_at_entry: u32,
    /// Push order across every kind of unwindable block (see
    /// [`Compiler::next_fblock_seq`]).
    seq: u32,
}

/// One pending `finally` clause. We hold the AST so `return`,
/// `break`, and `continue` can each inline a fresh copy of the
/// clause's bytecode before transferring control out.
enum FinallyKind {
    /// Body of a `finally:` clause; emitted by re-compiling the
    /// statements at the non-normal exit site.
    Stmts(Vec<Stmt>),
    /// Synthetic frame for a `with` block. The *bound* `__exit__`
    /// captured by `BEFORE_WITH` lives on the operand stack for the
    /// whole body (CPython 3.13's SETUP_WITH discipline): the inline
    /// emits `TOS(None, None, None)` — swapping a preserved return
    /// value out of the way first. CPython looks `__exit__` up once
    /// (special lookup, bypassing instance `__getattribute__`) and
    /// reuses the bound method on every exit path (test_descr
    /// test_special_method_lookup).
    /// `line`/`span` carry the `with` statement's own location: the
    /// inlined `__exit__` call is stamped with it (CPython's L3 exit
    /// block re-reports the `with` line when e.g. a `break` leaves the
    /// body — test_sys_settrace test_early_exit_with).
    WithExit { line: u32, span: (u32, u32) },
    /// Synthetic frame for an `async with` block: emit
    /// `await TOS(None, None, None)`. Mirrors `WithExit` but awaits
    /// the `__aexit__` coroutine, so a `return`/`break`/`continue`
    /// out of an `async with` body still runs the exit.
    AsyncWithExit { line: u32, span: (u32, u32) },
    /// CPython's `TRY_EXCEPT` fblock: the body of a `try` with
    /// `except` clauses. Unwinding emits only the `POP_BLOCK` that
    /// ends the body's coverage.
    TryExcept,
}

struct FinallyFrame {
    /// What this frame fires at non-normal exit.
    kind: FinallyKind,
    /// Length of `loop_stack` when this frame was pushed. Used to
    /// determine whether `break`/`continue` should run this finally
    /// (only if the relevant loop is outside the finally scope).
    loop_depth_at_push: usize,
    /// Stable id (see [`Compiler::next_finally_id`]). Lets a
    /// return/break/continue-path inline copy of this frame's body be
    /// excluded from the owning try's exception-table coverage.
    id: u32,
    /// This frame is an `except` clause's exit cleanup (the
    /// `e = None; del e` unbind, or an empty body for a bare
    /// `except:`): a `return` leaving the handler emits `POP_EXCEPT`
    /// *before* inlining the unbind (CPython's `HANDLER_CLEANUP`
    /// unwind: `POP_BLOCK; [SWAP 2]; POP_BLOCK; POP_EXCEPT; e = None;
    /// del e`). The VM reaps the handled exception at the store that
    /// displaces `e`, so `pickle.load`'s `except _Stop: return
    /// stopinst.value` still releases the unpickled graph promptly.
    pop_except_after: bool,
    /// For a handler-cleanup frame: the hole id the except region's
    /// own cleanup coverage (`SETUP_CLEANUP cleanup`) is punched with
    /// on this clause's return/break/continue paths. Distinct from
    /// `id` (which punches the clause body's unbind coverage) because
    /// CPython pops the two blocks one instruction apart: a preserved
    /// return value's `SWAP 2` sits between them, covered by the region
    /// cleanup but not by the unbind guard. `0` when not a handler
    /// frame.
    region_hole_id: u32,
    /// `exc_on_stack` when this frame was pushed. `break`/`continue`
    /// unwinding interleaves exception-region pops with frame inlines
    /// in recency order: regions *newer* than a frame sit above its
    /// stack state (notably a with's on-stack `__exit__`) and must be
    /// drained before the frame's inline runs.
    exc_at_push: u32,
    /// `handler_depth` when this frame was pushed (same interleaving,
    /// for enclosing `except`-handler bodies' saved-prev slots).
    handler_at_push: u32,
    /// `pending_retvals` when this frame was pushed (same
    /// interleaving, for pending return values).
    rv_at_push: u32,
    /// Push order across every kind of unwindable block (see
    /// [`Compiler::next_fblock_seq`]).
    seq: u32,
}

/// CPython's `location *ploc` threaded through an fblock unwind;
/// `None` is `NO_LOCATION`.
type Ploc = Option<(u32, u32)>;

/// How far an unwind goes: the `return` case drains everything; a
/// `break`/`continue` stops at the innermost loop's snapshots.
struct UnwindFloor {
    exc: u32,
    loops: usize,
    rv: u32,
    /// Index into `finally_stack` of the first frame to inline.
    frames: usize,
}

impl Compiler {
    fn new(
        name: String,
        filename: String,
        kind: CodeKind,
        line_index: Rc<LineIndex>,
        source: Rc<str>,
        params: CompileParams,
    ) -> Self {
        let future_annotations = params.future_annotations;
        let mut co = CodeObject::default();
        // Default qualname == name; nested scopes overwrite this via
        // `compute_child_qualname` once the parent context is known.
        co.qualname = name.clone();
        co.name = name;
        co.filename = filename;
        co.is_class_body = matches!(kind, CodeKind::Class);
        co.future_flags = params.future_flags;
        Self {
            co,
            kind,
            comp_kind: None,
            bindings: IndexMap::new(),
            explicit_globals: HashSet::new(),
            class_transparent_frees: HashSet::new(),
            free_order: Vec::new(),
            loop_stack: Vec::new(),
            finally_stack: Vec::new(),
            next_finally_id: 0,
            finally_holes: Vec::new(),
            line_index,
            current_line: 0,
            line_pinned: None,
            pinned_colspan: ColSpan::default(),
            current_span: (0, 0),
            no_interrupt_jumps: HashSet::new(),
            plain_jumps: HashSet::new(),
            setup_nops: std::collections::HashMap::new(),
            with_exit_jumps: Vec::new(),
            popblock_nops: HashSet::new(),
            pseudo_cond_jumps: HashSet::new(),
            backward_conds: HashSet::new(),
            stopiteration_wrap: true,
            inline_comp: 0,
            module_comp_plain_targets: Vec::new(),
            exc_on_stack: 0,
            exc_region_ids: Vec::new(),
            rv_seqs: Vec::new(),
            fblock_seq: 0,
            pending_retvals: 0,
            handler_depth: 0,
            inside_class_body: false,
            annotations_initialized: false,
            code_kind: kind,
            interactive: false,
            eval_mode: false,
            source,
            future_annotations,
            params,
            private: None,
            annotation_qualname: None,
            pep695_unbound: None,
            pep695_defaults: None,
            lazy_class_ctx: None,
            pending_lazy_class_ctx: None,
            class_assigned: HashSet::new(),
            deferred_annotations: Vec::new(),
            next_cond_annotation_index: 0,
            in_conditional_block: 0,
        }
    }

    /// CPython `ste_new`: a child scope is nested when this scope is
    /// function-like (a def, lambda, or comprehension, including the
    /// body of a PEP 709 inlined comprehension) or is itself nested.
    fn child_is_nested(&self) -> bool {
        self.co.is_nested
            || self.inline_comp > 0
            || !matches!(self.kind, CodeKind::Module | CodeKind::Class)
    }

    /// The [`LazyClassCtx`] a PEP 695 annotation scope created at
    /// *this* compiler's level should carry, or `None` when no class
    /// namespace is visible here. A class body mints a fresh context
    /// from its own symbols; an annotation scope nested in another
    /// annotation scope inherits the class context it was given.
    fn make_lazy_ctx(&self) -> Option<Rc<LazyClassCtx>> {
        if self.kind == CodeKind::Class {
            Some(Rc::new(LazyClassCtx {
                assigned: self.class_assigned.clone(),
                globals: self.explicit_globals.clone(),
            }))
        } else {
            self.lazy_class_ctx.clone()
        }
    }

    /// The source spelling of a (possibly mangled) binding name, for
    /// `__name__`/`__qualname__` display. CPython mangles the *binding*
    /// of `def __m`/`class __C` inside a class but keeps display names
    /// unmangled.
    fn display_name<'a>(&self, name: &'a str) -> &'a str {
        match &self.private {
            Some(class_name) => crate::mangle::demangle_name(class_name, name),
            None => name,
        }
    }

    /// Compute the PEP 3155 `__qualname__` for a function/class named
    /// `name` defined directly inside *this* (the parent) scope. Mirrors
    /// CPython's `compiler_set_qualname` (`Python/compile.c`):
    ///
    /// - A definition whose parent is the module gets the bare `name`.
    /// - Otherwise the parent's qualname is the base, with `.<locals>`
    ///   appended when the parent is a function/lambda scope (so a nested
    ///   `def`/`class` reads `outer.<locals>.inner`), and just the parent
    ///   qualname when the parent is a class body (so a method reads
    ///   `C.method`). The child name is then dotted onto that base.
    fn compute_child_qualname(&self, name: &str) -> String {
        // Annotation scopes are transparent for qualnames: the generic
        // def/class defined inside `<generic parameters of X>` gets the
        // qualname it would have had in the enclosing scope, and a
        // lambda inside an `__annotate__` or thunk is named from the
        // thunk's parent.
        if let Some((child, qualname, prefix)) = &self.annotation_qualname {
            if !child.is_empty() && child == name {
                return qualname.clone();
            }
            return format!("{prefix}{name}");
        }
        if matches!(self.kind, CodeKind::Module) {
            return name.to_owned();
        }
        // CPython's GLOBAL_EXPLICIT rule: `global P` in the enclosing scope
        // resets the nested def/class qualname to the bare name.
        if self.explicit_globals.contains(name) {
            return name.to_owned();
        }
        let mut base = self.co.qualname.clone();
        if matches!(self.kind, CodeKind::Function) {
            base.push_str(".<locals>");
        }
        base.push('.');
        base.push_str(name);
        base
    }

    /// The qualname prefix an annotation scope created by *this*
    /// scope hands to its own children (`compiler_set_qualname` with
    /// an annotation-scope parent uses the grandparent, i.e. this
    /// scope, as the base): nothing at module level, `qualname.` for a
    /// class body or an annotation scope (`COMPILE_SCOPE_ANNOTATIONS`
    /// never adds `.<locals>`), `qualname.<locals>.` for a function.
    fn annotation_child_prefix(&self) -> String {
        if matches!(self.kind, CodeKind::Module) {
            return String::new();
        }
        let mut base = self.co.qualname.clone();
        if matches!(self.kind, CodeKind::Function) && self.annotation_qualname.is_none() {
            base.push_str(".<locals>");
        }
        base.push('.');
        base
    }

    fn finish(mut self) -> CodeObject {
        // Canonicalize `co_cellvars` to CPython's localsplus-slot order:
        // cells aliasing a local (escaping parameters) come first, in
        // varname order (they share the parameter's slot), then the
        // rest alphabetically (symtable `dictbytype` sorts cell names;
        // non-aliased cells take slots after the plain locals in that
        // order). Emission used promotion order — remap every emitted
        // cell index. Keeping the internal order equal to slot order
        // makes the RFC 0068 wire codec's deref mapping invertible.
        if self.co.cellvars.len() > 1 {
            let mut sorted: Vec<String> = self.co.cellvars.clone();
            sorted.sort_unstable_by_key(|c| match self.co.varnames.iter().position(|v| v == c) {
                Some(p) => (0usize, p, String::new()),
                None => (1usize, 0, c.clone()),
            });
            if sorted != self.co.cellvars {
                let remap: Vec<u32> = self
                    .co
                    .cellvars
                    .iter()
                    .map(|n| sorted.iter().position(|s| s == n).unwrap() as u32)
                    .collect();
                let ncells = self.co.cellvars.len() as u32;
                for ins in self.co.instructions.iter_mut() {
                    if matches!(
                        ins.op,
                        OpCode::LoadDeref
                            | OpCode::StoreDeref
                            | OpCode::DeleteDeref
                            | OpCode::LoadClosure
                            | OpCode::MakeCell
                            | OpCode::LoadClassdictOrDeref
                    ) && ins.arg < ncells
                    {
                        ins.arg = remap[ins.arg as usize];
                    }
                }
                self.co.cellvars = sorted;
            }
        }
        // Place freevars at the end of the cells/freevars combined
        // index space. CPython orders `co_freevars` alphabetically
        // (symtable `dictbytype`), while emission used discovery
        // order — sort here and remap every emitted deref index
        // accordingly (test_builtin test_exec_closure hands
        // `exec(code, closure=…)` a tuple built positionally against
        // `co_freevars`).
        if self.free_order.len() > 1 {
            let mut sorted = self.free_order.clone();
            sorted.sort_unstable();
            if sorted != self.free_order {
                let ncells = self.co.cellvars.len() as u32;
                let remap: Vec<u32> = self
                    .free_order
                    .iter()
                    .map(|n| ncells + sorted.iter().position(|s| s == n).unwrap() as u32)
                    .collect();
                for ins in self.co.instructions.iter_mut() {
                    if matches!(
                        ins.op,
                        OpCode::LoadDeref
                            | OpCode::StoreDeref
                            | OpCode::DeleteDeref
                            | OpCode::LoadClosure
                            | OpCode::LoadClassdictOrDeref
                    ) && ins.arg >= ncells
                    {
                        ins.arg = remap[(ins.arg - ncells) as usize];
                    }
                }
                self.free_order = sorted;
            }
        }
        self.co.freevars = self.free_order.clone();

        // `codegen_wrap_in_stopiteration_handler` (PEP 479): the
        // generator-family body ends in an explicit `return None`, and
        // a `SETUP_CLEANUP` heading the sequence routes every escaping
        // exception through a `STOPITERATION_ERROR; RERAISE 1` block.
        // The `SETUP_CLEANUP` itself is materialised by the flowgraph
        // builder (it sits before `RESUME`, in the entry block); its
        // range is recorded here.
        let wrap = self.stopiteration_wrap
            && (self.co.is_generator || self.co.is_coroutine || self.co.is_async_generator);
        if wrap {
            let none_idx = self.co.intern_constant(Constant::None);
            self.emit_no_line(OpCode::LoadConst, none_idx);
            self.emit_no_line(OpCode::ReturnValue, 0);
            let handler = self.next_offset();
            self.emit_no_line(OpCode::StopIterationError, 0);
            self.emit_no_line(OpCode::Reraise, 1);
            let start = self
                .co
                .instructions
                .iter()
                .position(|i| i.op == OpCode::Resume)
                .unwrap_or(0) as u32;
            self.co.exception_table.push(ExcHandler {
                start,
                end: handler,
                handler,
                depth: 0,
                push_lasti: true,
            });
        }
        // `_PyCodegen_AddReturnAtEnd`: every stream falling off its end
        // returns None (and no jump target is out of bounds).
        let none_idx = self.co.intern_constant(Constant::None);
        self.emit_no_line(OpCode::LoadConst, none_idx);
        self.emit_no_line(OpCode::ReturnValue, 0);

        let nparams = self.co.arg_count
            + self.co.kwonly_count
            + u32::from(self.co.has_varargs)
            + u32::from(self.co.has_varkeywords);
        let input = flowgraph::BuildInput {
            plain_jumps: &self.plain_jumps,
            no_interrupt_jumps: &self.no_interrupt_jumps,
            setup_nops: &self.setup_nops,
            popblock_nops: &self.popblock_nops,
            pseudo_cond_jumps: &self.pseudo_cond_jumps,
            backward_conds: &self.backward_conds,
            stopiteration_wrap: wrap,
            nparams: nparams as usize,
        };
        flowgraph::optimize(&mut self.co, &input);
        // RFC 0021: size the inline-cache side-table to match the
        // emitted instruction stream so the VM can index into it
        // without bounds checks on the hot path.
        self.co.caches.resize(self.co.instructions.len());
        self.co
    }

    /// The line the next emitted instruction would carry (the current
    /// span's start line, `0` for `NO_LOCATION`).
    fn current_location_line(&self) -> u32 {
        if let Some(pin) = self.line_pinned {
            return pin;
        }
        match self.current_span {
            (0, 0) => self.current_line,
            (u32::MAX, u32::MAX) => 0,
            (start, _) => {
                let l = self.line_index.line_for(start);
                if l == 0 {
                    self.current_line
                } else {
                    l
                }
            }
        }
    }

    fn emit(&mut self, op: OpCode, arg: u32) -> u32 {
        let offset = self.co.instructions.len() as u32;
        self.co.instructions.push(Instruction { op, arg });
        if let Some(pin) = self.line_pinned {
            // Pinned region: a fixed line (or 0 = "no line") — CPython's
            // NO_LOCATION / statement-level locations for synthetic
            // cleanup code. A real pinned line carries the anchoring
            // statement's column span too: CPython never produces a
            // line-with-no-columns location.
            self.co.linetable.push(pin);
            self.co.coltable.push(if pin == 0 {
                ColSpan::default()
            } else {
                self.pinned_colspan
            });
            return offset;
        }
        // An instruction's line is its *own* location's start line
        // (CPython 3.11+ locations), not the enclosing statement's —
        // a traceback through a multiline expression points at the
        // sub-expression that raised.
        let line = match self.current_span {
            (0, 0) => self.current_line,
            // A synthesized node carrying `Span::NO_LOCATION`.
            (u32::MAX, u32::MAX) => {
                self.co.linetable.push(0);
                self.co.coltable.push(ColSpan::default());
                return offset;
            }
            (start, _) => {
                let l = self.line_index.line_for(start);
                if l == 0 {
                    self.current_line
                } else {
                    l
                }
            }
        };
        self.co.linetable.push(line);
        self.co.coltable.push(self.resolve_colspan());
        offset
    }

    /// Emit with CPython's `NEXT_LOCATION`: the instruction adopts the
    /// location of whatever follows it in the assembled stream (a
    /// terminator gets `NO_LOCATION`).
    fn emit_next_location(&mut self, op: OpCode, arg: u32) -> u32 {
        let offset = self.co.instructions.len() as u32;
        self.co.instructions.push(Instruction { op, arg });
        self.co.linetable.push(NEXT_LOCATION_LINE);
        self.co.coltable.push(ColSpan::default());
        offset
    }

    /// Emit with CPython's `NO_LOCATION`: the instruction never fires a
    /// `'line'` trace event and shows as `--` in `dis` output.
    fn emit_no_line(&mut self, op: OpCode, arg: u32) -> u32 {
        let saved = self.line_pinned;
        self.line_pinned = Some(0);
        let off = self.emit(op, arg);
        self.line_pinned = saved;
        off
    }

    /// Stand-in for a `SETUP_FINALLY` / `SETUP_CLEANUP` / `SETUP_WITH`
    /// pseudo-op at the current location. The flowgraph builder turns
    /// it back into the block push whose handler range opens right
    /// behind it; CPython's `convert_pseudo_ops` NOPs it out again,
    /// leaving the stale instruction slot the exception-table
    /// assembler is sensitive to (RFC 0077 WS9).
    fn emit_setup(&mut self, kind: OpCode) -> u32 {
        debug_assert!(matches!(
            kind,
            OpCode::SetupFinally | OpCode::SetupCleanup | OpCode::SetupWith
        ));
        let at = self.emit(OpCode::Nop, 0);
        self.setup_nops.insert(at, kind);
        at
    }

    /// [`Self::emit_setup`] with `NO_LOCATION`.
    fn emit_setup_no_line(&mut self, kind: OpCode) -> u32 {
        debug_assert!(matches!(
            kind,
            OpCode::SetupFinally | OpCode::SetupCleanup | OpCode::SetupWith
        ));
        let at = self.emit_no_line(OpCode::Nop, 0);
        self.setup_nops.insert(at, kind);
        at
    }

    /// Stand-in for a `POP_BLOCK` pseudo-op at the current location
    /// (`label_exception_targets` NOPs it without a handler).
    fn emit_pop_block(&mut self) -> u32 {
        let at = self.emit(OpCode::Nop, 0);
        self.popblock_nops.insert(at);
        at
    }

    /// [`Self::emit_pop_block`] with `NO_LOCATION`.
    fn emit_pop_block_no_line(&mut self) -> u32 {
        let at = self.emit_no_line(OpCode::Nop, 0);
        self.popblock_nops.insert(at);
        at
    }

    /// CPython `USE_LABEL`: the offset the label names. Only labels
    /// some instruction targets start a basic block (the flowgraph
    /// derives those from the jumps and the exception table), so this
    /// records nothing; it marks the codegen sites and hands back the
    /// offset the callers patch jumps or ranges against.
    fn use_label(&mut self) -> u32 {
        self.next_offset()
    }

    fn set_line_from(&mut self, byte: u32) {
        let line = self.line_index.line_for(byte);
        if line != 0 {
            self.current_line = line;
        }
    }

    /// PEP 654: `break`/`continue`/`return` may not leave an `except*`
    /// clause body. `break`/`continue` are fine when their target loop
    /// began inside the clause; `return` never is (nested `def`s are
    /// their own code unit and aren't descended into).
    fn validate_star_clause_jumps(stmts: &[Stmt], in_loop: bool) -> Result<(), CompileError> {
        const MSG: &str = "'break', 'continue' and 'return' cannot appear in an except* block";
        for s in stmts {
            match &s.kind {
                StmtKind::Break | StmtKind::Continue if !in_loop => {
                    return Err(CompileError::spanned(MSG, s.span));
                }
                StmtKind::Return(_) => {
                    return Err(CompileError::spanned(MSG, s.span));
                }
                StmtKind::If { body, orelse, .. } => {
                    Self::validate_star_clause_jumps(body, in_loop)?;
                    Self::validate_star_clause_jumps(orelse, in_loop)?;
                }
                StmtKind::While { body, orelse, .. }
                | StmtKind::For { body, orelse, .. }
                | StmtKind::AsyncFor { body, orelse, .. } => {
                    Self::validate_star_clause_jumps(body, true)?;
                    // A loop `else` belongs to the *outer* context.
                    Self::validate_star_clause_jumps(orelse, in_loop)?;
                }
                StmtKind::With { body, .. } | StmtKind::AsyncWith { body, .. } => {
                    Self::validate_star_clause_jumps(body, in_loop)?;
                }
                StmtKind::Try {
                    body,
                    handlers,
                    orelse,
                    finalbody,
                } => {
                    Self::validate_star_clause_jumps(body, in_loop)?;
                    for h in handlers {
                        Self::validate_star_clause_jumps(&h.body, in_loop)?;
                    }
                    Self::validate_star_clause_jumps(orelse, in_loop)?;
                    Self::validate_star_clause_jumps(finalbody, in_loop)?;
                }
                StmtKind::Match { cases, .. } => {
                    for case in cases {
                        Self::validate_star_clause_jumps(&case.body, in_loop)?;
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// Emit CPython 3.13+'s function-construction shape: a bare
    /// `MAKE_FUNCTION` followed by one `SET_FUNCTION_ATTRIBUTE` per
    /// present attribute, consumed top-down (closure 0x08 sits nearest
    /// the top of the stack, defaults 0x01 deepest — the reverse of the
    /// push order; `codegen_make_closure` orders closure, annotations,
    /// annotate (PEP 649, 0x10), kwdefaults, defaults).
    fn emit_make_function(&mut self, flags: u32) {
        self.emit(OpCode::MakeFunction, 0);
        for bit in [0x08u32, 0x04, 0x10, 0x02, 0x01] {
            if flags & bit != 0 {
                self.emit(OpCode::SetFunctionAttribute, bit);
            }
        }
    }

    /// Emit the entry `Resume` for a function/class/comprehension body.
    /// CPython 3.11+ places `RESUME` at the header line with a zero-width
    /// `0..0` column span (GH-93249), so synthesized tracebacks pointing
    /// at `tb_lasti == 0` render an (empty) caret row rather than none.
    fn emit_entry_resume(&mut self) {
        let idx = self.emit(OpCode::Resume, 0) as usize;
        self.co.coltable[idx] = ColSpan {
            end_lineno: self.co.linetable[idx],
            col: 0,
            end_col: 0,
        };
    }

    /// Resolve [`Self::current_span`] into a PEP-657 [`ColSpan`] for the
    /// next emitted instruction. Columns are 0-based byte offsets into
    /// their source lines; a degenerate `(0, 0)` span yields "unknown".
    fn resolve_colspan(&self) -> ColSpan {
        let (start, end) = self.current_span;
        if start == 0 && end == 0 {
            return ColSpan::default();
        }
        let (_start_line, start_col) = self.line_index.pos_for(start);
        let (end_line, end_col) = self.line_index.pos_for(end);
        ColSpan {
            end_lineno: end_line,
            col: start_col as i32,
            end_col: end_col as i32,
        }
    }

    /// Point [`Self::current_span`] at an AST node's source span so the
    /// instructions emitted for it carry the node's columns.
    #[inline]
    fn set_span(&mut self, span: weavepy_lexer::Span) {
        self.current_span = (span.start.0, span.end.0);
    }

    /// CPython's `update_start_location_to_match_attr`: when an
    /// attribute access (or method call) spans multiple lines, the
    /// `LOAD/STORE/DELETE_ATTR` — and the `CALL` on a method — report
    /// the *attribute name* as their start location, so tracebacks
    /// point at `.method`, not at the start of a multiline receiver.
    /// Runs `f` with the adjusted location, then restores it.
    fn with_attr_location<F: FnOnce(&mut Self)>(&mut self, attr_end: u32, attr_len: u32, f: F) {
        let saved_span = self.current_span;
        let saved_line = self.current_line;
        let (start, end) = self.current_span;
        if !(start == 0 && end == 0) {
            let start_line = self.line_index.line_for(start);
            let attr_line = self.line_index.line_for(attr_end);
            if start_line != attr_line {
                let new_start = attr_end.saturating_sub(attr_len);
                self.current_span = (new_start, end.max(attr_end));
                self.set_line_from(new_start);
            }
        }
        f(self);
        self.current_span = saved_span;
        self.current_line = saved_line;
    }

    /// CPython 3.14 `maybe_optimize_function_call` eligibility: a call
    /// of the bare name `all`, `any`, or `tuple` with exactly one
    /// positional argument that is a (non-async) generator expression
    /// and no keywords. Returns the `LOAD_COMMON_CONSTANT` oparg of
    /// the builtin to guard against, plus the generator expression.
    fn genexp_call_optimization<'e>(
        &self,
        func: &Expr,
        args: &'e [Expr],
        keywords: &[KwArg],
    ) -> Option<(u32, &'e Expr)> {
        let ExprKind::Name(name) = &func.kind else {
            return None;
        };
        let const_oparg = match name.as_str() {
            "all" => COMMON_CONSTANT_ALL,
            "any" => COMMON_CONSTANT_ANY,
            "tuple" => COMMON_CONSTANT_TUPLE,
            _ => return None,
        };
        let [arg] = args else {
            return None;
        };
        if !keywords.is_empty() {
            return None;
        }
        let ExprKind::GeneratorExp { elt, generators } = &arg.kind else {
            return None;
        };
        // An async generator expression (`ste_coroutine`) is left to the
        // plain call.
        if comp_clause_is_async(generators, elt, None) {
            return None;
        }
        Some((const_oparg, arg))
    }

    /// Emit CPython 3.14's inlined `all/any/tuple(<genexp>)` guard and
    /// loop (`maybe_optimize_function_call`). The caller has already
    /// loaded the callable; on entry the stack holds just it. Every
    /// unit takes the callable's location (already current) except the
    /// NO_LOCATION iterator pops, and the generator expression carries
    /// its own. Returns `(skip_optimization site, end-jump sites)`: the
    /// first must be patched to the plain call's `PUSH_NULL`, the rest
    /// to the instruction after the plain call.
    fn emit_genexp_call_optimization(
        &mut self,
        func: &Expr,
        genexp: &Expr,
        const_oparg: u32,
    ) -> Result<(u32, Vec<u32>), CompileError> {
        let is_tuple = const_oparg == COMMON_CONSTANT_TUPLE;
        let stamp = |c: &mut Self| {
            c.set_line_from(func.span.start.0);
            c.set_span(func.span);
        };
        self.emit(OpCode::CopyTop, 1);
        self.emit(OpCode::LoadCommonConstant, const_oparg);
        self.emit(OpCode::IsOp, 0);
        let skip = self.emit(OpCode::PopJumpIfFalse, 0);
        self.emit(OpCode::PopTop, 0);
        if is_tuple {
            self.emit(OpCode::BuildList, 0);
        }
        self.compile_expr(genexp)?;
        stamp(self);
        let loop_top = self.next_offset();
        let for_site = self.emit(OpCode::ForIter, 0);
        if is_tuple {
            self.emit(OpCode::ListAppend, 2);
            let back = self.emit(OpCode::JumpBackward, 0);
            self.plain_jumps.insert(back);
            self.patch_jump(back, loop_top);
        } else {
            // `maybe_optimize_function_call`: `TO_BOOL; POP_JUMP_IF_TRUE
            // loop` (all) / `POP_JUMP_IF_FALSE loop` (any) — a backward
            // conditional the flowgraph's `normalize_jumps` inverts over
            // a `NOT_TAKEN; JUMP loop` trampoline.
            self.emit(OpCode::ToBool, 0);
            let cond = if const_oparg == COMMON_CONSTANT_ALL {
                OpCode::PopJumpIfTrue
            } else {
                OpCode::PopJumpIfFalse
            };
            let back = self.emit(cond, 0);
            self.patch_jump(back, loop_top);
        }
        self.emit_no_line(OpCode::PopIter, 0);
        stamp(self);
        let mut end_jumps = Vec::with_capacity(2);
        if !is_tuple {
            // Early exit: the opposite of the builtin's initial result.
            let idx = self
                .co
                .intern_constant(Constant::Bool(const_oparg != COMMON_CONSTANT_ALL));
            self.emit(OpCode::LoadConst, idx);
        }
        let j = self.emit(OpCode::JumpForward, 0);
        self.plain_jumps.insert(j);
        end_jumps.push(j);
        let cleanup = self.next_offset();
        self.patch_jump(for_site, cleanup);
        self.emit_no_line(OpCode::EndFor, 0);
        self.emit_no_line(OpCode::PopIter, 0);
        stamp(self);
        if is_tuple {
            self.emit(OpCode::ListToTuple, 0);
        } else {
            let idx = self
                .co
                .intern_constant(Constant::Bool(const_oparg == COMMON_CONSTANT_ALL));
            self.emit(OpCode::LoadConst, idx);
        }
        let j = self.emit(OpCode::JumpForward, 0);
        self.plain_jumps.insert(j);
        end_jumps.push(j);
        Ok((skip, end_jumps))
    }

    /// CPython 3.13 `can_optimize_super_call`: `True` when a Load of
    /// `attr_expr` (an `Attribute` whose base is a `super(...)` call)
    /// may lower to the fused `LOAD_SUPER_ATTR`. Requires: the call is
    /// `super()` or `super(a, b)` (no keywords/stars), the attribute is
    /// not `__class__`, `super` is an implicit global both here and at
    /// module level, and — for the zero-argument form — the enclosing
    /// function has a positional parameter and a `__class__` freevar.
    fn super_attr_optimizable(&self, attr_expr: &Expr) -> bool {
        let ExprKind::Attribute { value, attr } = &attr_expr.kind else {
            return false;
        };
        let ExprKind::Call {
            func,
            args,
            keywords,
        } = &value.kind
        else {
            return false;
        };
        if !matches!(&func.kind, ExprKind::Name(n) if n == "super")
            || attr == "__class__"
            || !keywords.is_empty()
        {
            return false;
        }
        // Statically visible shadowing of `super`: a binding in the
        // current scope (local, cell/free capture, `global`/`nonlocal`
        // declaration) or any module-level mention disables the fusion.
        if self.bindings.get("super").is_some() || self.params.module_mentions_super {
            return false;
        }
        match args.len() {
            2 => !args.iter().any(|a| matches!(a.kind, ExprKind::Starred(_))),
            0 => {
                self.co.arg_count >= 1
                    && matches!(self.bindings.get("__class__"), Some(Binding::Free))
            }
            _ => false,
        }
    }

    /// CPython `load_args_for_super` + the fused super-attribute load:
    /// `LOAD_GLOBAL super`, then either the two explicit arguments or
    /// the `__class__` cell + first parameter, then `LOAD_SUPER_ATTR`
    /// (arg = `namei << 2 | method | two_arg << 1`) at the attribute
    /// expression's full location and a trailing `NOP` at the
    /// attr-name-adjusted location. The caller must have validated via
    /// [`Self::super_attr_optimizable`] and set `current_span` to the
    /// attribute expression's span.
    fn emit_super_attr(&mut self, attr_expr: &Expr, method: bool) -> Result<(), CompileError> {
        let ExprKind::Attribute { value, attr } = &attr_expr.kind else {
            unreachable!("emit_super_attr requires an attribute expression");
        };
        let ExprKind::Call { func, args, .. } = &value.kind else {
            unreachable!("emit_super_attr requires a super() call base");
        };
        let saved = self.current_span;
        self.set_span(func.span);
        self.emit_load_name("super");
        self.current_span = saved;
        let two_arg = args.len() == 2;
        if two_arg {
            self.compile_expr(&args[0])?;
            self.compile_expr(&args[1])?;
        } else {
            let saved = self.current_span;
            self.set_span(value.span);
            self.emit_load_name("__class__");
            let first_param = self
                .co
                .varnames
                .first()
                .cloned()
                .expect("super_attr_optimizable checked arg_count >= 1");
            self.emit_load_name(&first_param);
            self.current_span = saved;
        }
        let namei = self.co.intern_name(attr);
        let arg = (namei << 2) | u32::from(method) | (u32::from(two_arg) << 1);
        self.emit(OpCode::LoadSuperAttr, arg);
        self.with_attr_location(attr_expr.span.end.0, attr.len() as u32, |c| {
            c.emit(OpCode::Nop, 0);
        });
        Ok(())
    }

    fn next_offset(&self) -> u32 {
        self.co.instructions.len() as u32
    }

    fn patch_jump(&mut self, site: u32, target: u32) {
        let ins = &mut self.co.instructions[site as usize];
        let from = site + 1;
        match ins.op {
            // Unconditional jumps are direction-agnostic pseudo-ops
            // until the flowgraph flattens them; pick the form that
            // encodes the distance.
            OpCode::JumpForward | OpCode::JumpBackward => {
                if target >= from {
                    ins.op = OpCode::JumpForward;
                    ins.arg = target - from;
                } else {
                    ins.op = OpCode::JumpBackward;
                    ins.arg = from - target;
                }
            }
            OpCode::PopJumpIfFalse
            | OpCode::PopJumpIfTrue
            | OpCode::PopJumpIfNone
            | OpCode::PopJumpIfNotNone => {
                if target >= from {
                    ins.arg = target - from;
                    self.backward_conds.remove(&site);
                } else {
                    ins.arg = from - target;
                    self.backward_conds.insert(site);
                }
            }
            OpCode::ForIter | OpCode::Send => {
                ins.arg = target.saturating_sub(from);
            }
            other => panic!("patch_jump on non-jump op {other:?}"),
        }
    }

    // ---------- module body ----------

    fn compile_module_body(&mut self, module: &Module) -> Result<(), CompileError> {
        self.analyze_scope_module(module);
        // PyCF_ALLOW_TOP_LEVEL_AWAIT: a module body that awaits is a
        // coroutine code object, and (like generator functions) the VM's
        // bootstrap requires RETURN_GENERATOR as the first instruction —
        // so this must be decided before emission starts.
        if self.allows_top_level_await() && body_has_top_level_await(&module.body) {
            self.co.is_coroutine = true;
            self.emit(OpCode::ReturnGenerator, 0);
            // CPython 3.13 prologue: every resume pushes the sent value
            // (None on the first), discarded here.
            self.emit(OpCode::PopTop, 0);
        }
        self.emit(OpCode::Resume, 0);
        // `start_location(stmts)`: the module prologue (the PEP 649
        // `__annotate__` store, `__conditional_annotations__`, and
        // SETUP_ANNOTATIONS) sits at the first statement's location
        // (dis_annot_stmt_str asserts the line).
        let module_loc = module.body.first().map(|first| AnnotateLoc {
            line: self.line_index.line_for(first.span.start.0),
            span: Some(first.span),
        });
        let has_annotations = block_has_annotations(&module.body);
        // `ANNOTATIONS_PLACEHOLDER` (`codegen_enter_scope` for a module):
        // the module's `__annotate__` is defined *first*, before any
        // statement runs, but compiled *last* — after the body has
        // collected the deferred annotations — so its code constant and
        // the `__annotate__` name are interned after the body's. Reserve
        // the three instructions now and patch their operands then.
        let mut annotate_placeholder = None;
        if !self.future_annotations && block_has_deferred_annotations(&module.body) {
            if let Some(loc) = module_loc {
                self.apply_annotate_loc(loc);
                let load = self.emit(OpCode::LoadConst, 0);
                self.emit(OpCode::MakeFunction, 0);
                let store = self.emit(OpCode::StoreName, 0);
                annotate_placeholder = Some((load, store));
            }
        }
        // `_PyCodegen_Module`: every module-level annotation is
        // conditional (the module may be partially executed), so an
        // annotated module tracks the executed ones in
        // `__conditional_annotations__`. The symtable also cooks up an
        // implicit (never dereferenced) cell of that name for the
        // module block (`_PyCompile_EnterScope`), so `co_cellvars`
        // carries it and the code starts with a `MAKE_CELL`.
        if has_annotations {
            self.co
                .cellvars
                .push("__conditional_annotations__".to_owned());
            if let Some(loc) = module_loc {
                self.apply_annotate_loc(loc);
            }
            self.emit(OpCode::BuildSet, 0);
            let idx = self.co.intern_name("__conditional_annotations__");
            self.emit(OpCode::StoreName, idx);
        }
        // Under PEP 563 (`codegen_body`): SETUP_ANNOTATIONS as the first
        // real instruction of an annotated module, so code preceding the
        // first annotation can already read `__annotations__`
        // (ann_module.py does `__annotations__[1] = 2` at module top).
        if self.future_annotations && has_annotations {
            if let Some(loc) = module_loc {
                self.apply_annotate_loc(loc);
            }
            self.emit(OpCode::SetupAnnotations, 0);
            self.annotations_initialized = true;
        }
        // CPython's compiler_body stores a module's leading string
        // literal as `__doc__` (exec mode only — the REPL echoes it and
        // eval mode can't contain it) and skips re-evaluating it as an
        // expression statement. Under `-OO` (optimize >= 2) the
        // docstring is dropped entirely.
        let mut body: &[Stmt] = &module.body;
        if !self.interactive && !self.eval_mode {
            if let Some(doc) = first_stmt_docstring(&module.body) {
                if self.params.optimize < 2 {
                    let doc_const = self.co.intern_constant(Constant::Str(clean_docstring(doc)));
                    let doc_name = self.co.intern_name("__doc__");
                    self.set_line_from(module.body[0].span.start.0);
                    self.set_span(module.body[0].span);
                    self.emit(OpCode::LoadConst, doc_const);
                    self.emit(OpCode::StoreName, doc_name);
                }
                body = &module.body[1..];
            }
        }
        for stmt in body {
            self.compile_stmt(stmt)?;
        }
        // PEP 649: compile the module's `__annotate__` from the deferred
        // annotations and splice it into the reserved prologue slot.
        if let Some((load, store)) = annotate_placeholder {
            let loc = module_loc.expect("placeholder implies a first statement");
            let code = self
                .compile_deferred_annotations(loc)?
                .expect("placeholder implies deferred annotations");
            debug_assert!(code.freevars.is_empty());
            let code_idx = self
                .co
                .intern_constant(Constant::Code(std::sync::Arc::new(code)));
            self.co.instructions[load as usize].arg = code_idx;
            let name_idx = self.co.intern_name("__annotate__");
            self.co.instructions[store as usize].arg = name_idx;
        }
        Ok(())
    }

    // ---------- scope analysis ----------

    fn analyze_scope_module(&mut self, module: &Module) {
        // At module scope every assigned name is a global (CPython
        // does the same — locals at module scope ARE the globals).
        let mut assigned = HashSet::new();
        for s in &module.body {
            collect_assigned(s, &mut assigned);
            collect_walrus_stmt(s, &mut assigned);
        }
        for n in assigned {
            self.bindings.insert(n, Binding::Global);
        }
        // A `global x` declaration *anywhere* in the module — including
        // inside nested functions and classes — marks `x` GLOBAL_EXPLICIT
        // in the module block too (CPython symtable), so top-level
        // accesses compile to the *_GLOBAL opcodes instead of *_NAME.
        // Observable when `exec` runs with distinct globals/locals
        // mappings (test_scope.testGlobalInParallelNestedFunctions).
        let mut explicit = HashSet::new();
        for s in &module.body {
            collect_global_decls_deep(s, &mut explicit);
        }
        for n in &explicit {
            self.bindings.insert(n.clone(), Binding::Global);
        }
        // A walrus inside a module-level comprehension binds the
        // module global explicitly (`symtable_extend_namedexpr_scope`
        // adds `DEF_GLOBAL` to both scopes), so every top-level access
        // of that name compiles to the *_GLOBAL ops.
        for s in &module.body {
            let mut comp_walruses = Vec::new();
            collect_comp_walrus_targets_stmt(s, &mut comp_walruses);
            for n in comp_walruses {
                self.bindings.insert(n.clone(), Binding::Global);
                explicit.insert(n);
            }
        }
        self.explicit_globals = explicit;
        let mut comp_cells = HashSet::new();
        for s in &module.body {
            self.collect_comp_cells_stmt(s, &mut comp_cells);
        }
        self.register_comp_cells(comp_cells);
    }

    /// The [`FreeScan`] for nested-scope analysis run from this scope.
    fn free_scan(&self, inline_comps: bool) -> FreeScan {
        let class_body = matches!(self.kind, CodeKind::Class);
        // The class body itself, or a scope that sees one through
        // `__classdict__` (a hidden scope, whose thunks and wrapped
        // statement see the same namespace).
        let class_binds = if class_body {
            Some(
                self.class_assigned
                    .union(&self.explicit_globals)
                    .cloned()
                    .collect(),
            )
        } else {
            self.lazy_class_ctx
                .as_ref()
                .map(|ctx| ctx.assigned.union(&ctx.globals).cloned().collect())
        };
        FreeScan {
            inline_comps,
            async_ok: self.in_async_context(),
            class_body,
            class_binds,
        }
    }

    /// Names read by nested scopes, under this scope's PEP 709
    /// inlining decisions (an inlined comprehension is transparent; a
    /// generator expression or non-inlined comprehension is one more
    /// nested scope with reads of its own).
    fn needed_in_inner(
        &self,
        inline_comps: bool,
        collect: impl Fn(&FreeScan, &mut HashSet<String>),
    ) -> HashSet<String> {
        let scan = self.free_scan(inline_comps);
        let mut needed = HashSet::new();
        collect(&scan, &mut needed);
        needed
    }

    /// CPython's `DEF_COMP_CELL`: register the iteration variables
    /// that inlined comprehensions in this scope turn into cells (a
    /// real scope nested in the comprehension closes over them). They
    /// join `co_cellvars` (sorted with the rest, `dictbytype`) and,
    /// unless this scope resolves the name some other way (explicit
    /// global, free), the scope's own binding becomes the cell too:
    /// `inline_comprehension` copies an absent name with the
    /// comprehension's CELL scope and `analyze_cells` promotes a
    /// local one.
    fn register_comp_cells(&mut self, comp_cells: HashSet<String>) {
        if comp_cells.is_empty() {
            return;
        }
        for name in &comp_cells {
            if matches!(self.kind, CodeKind::Module | CodeKind::Class) {
                // Module/class names keep their namespace semantics
                // outside the comprehension (`LOCAL` in a non-function
                // block compiles to the *_NAME ops).
                continue;
            }
            match self.bindings.get(name) {
                Some(Binding::Local) | None => {
                    self.bindings.insert(name.clone(), Binding::Cell);
                }
                _ => {}
            }
        }
        let mut all: Vec<String> = self.co.cellvars.clone();
        for name in comp_cells {
            if !all.contains(&name) {
                all.push(name);
            }
        }
        all.sort_unstable();
        self.co.cellvars = all;
    }

    fn analyze_scope_function(
        &mut self,
        params: &[String],
        body: &[Stmt],
        enclosing: &[&IndexMap<String, Binding>],
    ) {
        for p in params {
            self.bindings.insert(p.clone(), Binding::Local);
        }
        let mut globals = HashSet::new();
        let mut nonlocals = HashSet::new();
        let mut assigned = HashSet::new();
        for s in body {
            collect_decls(s, &mut globals, &mut nonlocals, &mut assigned);
            // Walrus targets bind in this scope too (PEP 572) but live inside
            // expressions that `collect_decls` doesn't descend into.
            collect_walrus_stmt(s, &mut assigned);
        }
        self.explicit_globals = globals.clone();
        for n in globals {
            self.bindings.insert(n, Binding::Global);
        }
        for n in nonlocals {
            // `nonlocal x` makes x a free variable in this scope —
            // it'll be looked up in the cell array. Reserve its
            // free-order slot now so the cell index aligns with the
            // freevars list emitted alongside the code object.
            self.bindings.insert(n.clone(), Binding::Free);
            if !self.free_order.contains(&n) {
                self.free_order.push(n);
            }
        }
        // A PEP 695 hidden scope's analysis body contains the wrapped
        // `def`/`class`, but the *enclosing* scope binds its name.
        if let Some(unbound) = &self.pep695_unbound {
            assigned.remove(unbound);
        }
        for n in assigned {
            self.bindings.entry(n).or_insert(Binding::Local);
        }
        // Names referenced by directly-emitted bytecode in this scope.
        let mut reads = HashSet::new();
        for s in body {
            collect_reads_stmt_fn(s, &mut reads);
        }
        // symtable.c `analyze_name`'s `class_entry` shortcut: in a scope
        // that can see a class namespace, a name the class body binds
        // (or declares `global`) is GLOBAL_IMPLICIT / GLOBAL_EXPLICIT —
        // resolved through the class dict and then globals, never an
        // enclosing function's cell — so this scope's own reads of it
        // are not free-variable candidates. Nested scopes don't see
        // the class namespace, so their needs still pass through.
        if let Some(ctx) = &self.lazy_class_ctx {
            reads.retain(|n| !ctx.assigned.contains(n) && !ctx.globals.contains(n));
        }
        // Names needed by ANY nested scope (lambda, comp, def). They
        // also flow through us: if an inner scope reads `threshold`
        // and we don't bind it, we must surface it as a free var here
        // so our enclosing scope can hand us a cell to forward.
        let needed_in_inner = self.needed_in_inner(self.lazy_class_ctx.is_none(), |scan, out| {
            for s in body {
                collect_inner_free(s, &self.bindings, out, scan);
            }
        });
        let mut free_candidates = reads.clone();
        free_candidates.extend(needed_in_inner.iter().cloned());
        // Iterate in sorted order: `free_candidates` is a `HashSet`, and
        // its iteration order would otherwise leak into `free_order` (→
        // `co_freevars`), making two compiles of the same source disagree
        // (`test_compile_ast` asserts source-vs-AST code equality). CPython
        // sorts these names too (`dictbytype`).
        let mut free_candidates: Vec<String> = free_candidates.into_iter().collect();
        free_candidates.sort_unstable();
        for name in free_candidates {
            if self.bindings.contains_key(&name) {
                continue;
            }
            for env in enclosing {
                if let Some(b) = env.get(&name) {
                    match b {
                        Binding::Local
                        | Binding::Cell
                        | Binding::Free
                        | Binding::Nonlocal
                        | Binding::ClassPassthrough => {
                            self.bindings.insert(name.clone(), Binding::Free);
                            self.free_order.push(name.clone());
                            break;
                        }
                        Binding::Global => {}
                    }
                }
            }
        }
        // Promote our own locals to cellvars when an inner scope
        // reads or declares them as free / nonlocal. We do this
        // BEFORE emission so the very first `STORE_*` for each
        // promoted name routes through the cell.
        // Sorted for the same determinism reason as above — the promotion
        // order becomes the `co_cellvars` order.
        let mut needed_in_inner: Vec<String> = needed_in_inner.into_iter().collect();
        needed_in_inner.sort_unstable();
        for name in needed_in_inner {
            if matches!(self.bindings.get(&name), Some(Binding::Local)) {
                self.bindings.insert(name.clone(), Binding::Cell);
                if !self.co.cellvars.contains(&name) {
                    self.co.cellvars.push(name);
                }
            }
        }
        if self.lazy_class_ctx.is_none() {
            let mut comp_cells = HashSet::new();
            for s in body {
                self.collect_comp_cells_stmt(s, &mut comp_cells);
            }
            self.register_comp_cells(comp_cells);
        }
    }

    // ---------- statements ----------

    fn compile_stmt(&mut self, stmt: &Stmt) -> Result<(), CompileError> {
        // `CODEGEN_COND_BLOCK`: the bodies of these statements execute
        // conditionally, which PEP 649 class bodies record so
        // `__annotate__` only reports annotations whose statement ran.
        if matches!(
            stmt.kind,
            StmtKind::If { .. }
                | StmtKind::While { .. }
                | StmtKind::For { .. }
                | StmtKind::AsyncFor { .. }
                | StmtKind::With { .. }
                | StmtKind::AsyncWith { .. }
                | StmtKind::Try { .. }
                | StmtKind::Match { .. }
        ) {
            self.in_conditional_block += 1;
            let result = self.compile_stmt_inner(stmt);
            self.in_conditional_block -= 1;
            return result;
        }
        self.compile_stmt_inner(stmt)
    }

    fn compile_stmt_inner(&mut self, stmt: &Stmt) -> Result<(), CompileError> {
        self.set_line_from(stmt.span.start.0);
        self.set_span(stmt.span);
        match &stmt.kind {
            StmtKind::Expr(e) => {
                // CPython folds a constant expression statement into a
                // bare NOP: the value never reaches `co_consts`
                // (test_code's `optimize_away` doctest). Docstrings are
                // pooled separately (`doc_slot`), so nothing is lost.
                if !self.eval_mode && !self.interactive && matches!(e.kind, ExprKind::Constant(_)) {
                    self.emit(OpCode::Nop, 0);
                    return Ok(());
                }
                self.compile_expr(e)?;
                // `eval` mode: the single top-level expression returns its
                // value so `eval(compile(src, fn, "eval"))` yields it.
                // Interactive ("single") mode: a top-level expression
                // statement echoes its value via `sys.displayhook`
                // instead of being discarded. Only the top-level compiler
                // sets these flags; nested scopes get fresh `Compiler`
                // instances, so this never fires inside functions/classes.
                // CPython stamps the consuming op with the *value
                // expression's* location, not the statement head — a
                // parenthesized call statement must not surface the
                // paren line in co_lines (test_lineno_procedure_call).
                let saved_line = self.current_line;
                let saved_span = self.current_span;
                self.set_line_from(e.span.start.0);
                self.set_span(e.span);
                if self.eval_mode {
                    self.emit(OpCode::ReturnValue, 0);
                } else if self.interactive {
                    self.emit(OpCode::PrintExpr, 0);
                } else {
                    // CPython emits the statement's POP_TOP with NO_LOCATION
                    // and lets flowgraph line propagation fill it in when it
                    // follows the value in the same basic block. At a
                    // multi-predecessor join (a boolop merge target) it stays
                    // location-less, so a debugger stepping by opcode sees
                    // `f_lineno is None` there and moves on to the next real
                    // line (gh-127321; test_pdb_issue_gh_127321 stops at the
                    // line *after* a mid-expression `set_trace()`).
                    self.emit_no_line(OpCode::PopTop, 0);
                }
                self.current_line = saved_line;
                self.current_span = saved_span;
            }
            StmtKind::TypeAlias {
                name,
                type_params,
                value,
                ..
            } => {
                self.compile_type_alias(stmt, name, type_params, value)?;
            }
            StmtKind::Pass => {
                // CPython lowers `pass` to a NOP carrying the statement's
                // location (its optimizer only deletes NOPs whose line is
                // already covered by a neighbour), so a traced `pass`
                // line fires a 'line' event (test_21_repeated_pass).
                self.emit(OpCode::Nop, 0);
            }
            StmtKind::Delete(targets) => {
                for target in targets {
                    self.compile_delete(target)?;
                }
            }
            StmtKind::Assert { test, msg } => {
                // `assert test [, msg]` lowers to:
                //   <test>; POP_JUMP_IF_TRUE end
                //   LOAD_NAME AssertionError
                //   [<msg>; CALL 1]
                //   RAISE_VARARGS 1
                // end:
                //
                // Under `-O`/`-OO` (optimize >= 1) assertions compile
                // to nothing, exactly like CPython's compiler_assert.
                if self.params.optimize >= 1 {
                    return Ok(());
                }
                let stmt_span = stmt.span;
                // Location split mirrors CPython codegen_assert: the
                // branch carries the test's span, LOAD_COMMON_CONSTANT
                // and the msg CALL carry the whole statement's, and
                // RAISE_VARARGS carries the test's again so PEP-657
                // carets underline the failed condition.
                let mut skip = Vec::new();
                self.compile_jump_if(test, true, &mut skip)?;
                // The *builtin* AssertionError, immune to shadowing
                // (CPython 3.14 `LOAD_COMMON_CONSTANT 0`, bpo-34880).
                self.set_span(stmt_span);
                self.emit(OpCode::LoadCommonConstant, COMMON_CONSTANT_ASSERTION_ERROR);
                if let Some(m) = msg {
                    self.compile_expr(m)?;
                    self.set_span(stmt_span);
                    // The message rides the wire view's self slot
                    // (CPython compiler_assert: `CALL 0`).
                    self.emit(OpCode::CallSelf, 1);
                }
                self.set_span(test.span);
                self.emit(OpCode::RaiseVarargs, 1);
                self.set_span(stmt_span);
                let end = self.next_offset();
                for j in skip {
                    self.patch_jump(j, end);
                }
            }
            StmtKind::Assign { targets, value } => {
                let n = targets.len();
                for t in targets.iter() {
                    if matches!(t.kind, ExprKind::Yield(_) | ExprKind::YieldFrom(_)) {
                        // CPython distinguishes a bare `yield` in a chained
                        // assignment (`x = yield = y`) from a parenthesised
                        // sole target (`(yield x) = y`).
                        return Err(CompileError::parser_spanned(
                            if n > 1 {
                                "assignment to yield expression not possible"
                            } else {
                                "cannot assign to yield expression here. Maybe you meant '==' \
                                 instead of '='?"
                            },
                            t.span,
                        ));
                    }
                }
                self.compile_expr(value)?;
                for (i, t) in targets.iter().enumerate() {
                    if i + 1 < n {
                        self.emit(OpCode::CopyTop, 0);
                    }
                    self.compile_assign(t)?;
                }
            }
            StmtKind::AugAssign { target, op, value } => {
                if matches!(target.kind, ExprKind::Yield(_) | ExprKind::YieldFrom(_)) {
                    return Err(CompileError::parser_spanned(
                        "'yield expression' is an illegal expression for augmented assignment",
                        target.span,
                    ));
                }
                let bin_arg = bin_op_kind(*op) as u32 | crate::bytecode::BINARY_OP_INPLACE_FLAG;
                // CPython's codegen_augassign evaluates the target primary
                // *once* and shuffles with COPY/SWAP (an attribute or
                // subscript receiver with side effects must not run twice);
                // it also keeps the line sequence exact — re-evaluating
                // `o` would fire the receiver's line again before the
                // store (test_compile test_lineno_attribute).
                match &target.kind {
                    ExprKind::Attribute { value: obj, attr } => {
                        if attr == "__debug__" {
                            return Err(CompileError::spanned(
                                "cannot assign to __debug__",
                                target.span,
                            ));
                        }
                        self.compile_expr(obj)?;
                        let saved = self.current_span;
                        self.set_span(target.span);
                        self.emit(OpCode::CopyTop, 1);
                        let idx = self.co.intern_name(attr);
                        self.with_attr_location(target.span.end.0, attr.len() as u32, |c| {
                            c.emit(OpCode::LoadAttr, idx);
                        });
                        self.current_span = saved;
                        self.compile_expr(value)?;
                        self.emit(OpCode::BinaryOp, bin_arg);
                        let saved = self.current_span;
                        self.set_span(target.span);
                        // Both the SWAP and the STORE_ATTR carry the
                        // attr-adjusted location (CPython applies
                        // update_start_location_to_match_attr to both).
                        self.with_attr_location(target.span.end.0, attr.len() as u32, |c| {
                            c.emit(OpCode::Swap, 2);
                            c.emit(OpCode::StoreAttr, idx);
                        });
                        self.current_span = saved;
                    }
                    ExprKind::Subscript { value: obj, slice } => {
                        // CPython 3.14 `codegen_augassign`: the two-element
                        // slice form keeps the three operands live with
                        // `COPY 3` x3 and stores through `STORE_SLICE`.
                        let two = should_apply_two_element_slice_optimization(slice);
                        self.compile_expr(obj)?;
                        if two {
                            self.compile_slice_two_parts(slice)?;
                        } else {
                            self.compile_expr(slice)?;
                        }
                        let saved = self.current_span;
                        self.set_span(target.span);
                        if two {
                            self.emit(OpCode::CopyTop, 3);
                            self.emit(OpCode::CopyTop, 3);
                            self.emit(OpCode::CopyTop, 3);
                            self.emit(OpCode::BinarySlice, 0);
                        } else {
                            self.emit(OpCode::CopyTop, 2);
                            self.emit(OpCode::CopyTop, 2);
                            self.emit(OpCode::BinarySubscr, 0);
                        }
                        self.current_span = saved;
                        self.compile_expr(value)?;
                        self.emit(OpCode::BinaryOp, bin_arg);
                        let saved = self.current_span;
                        self.set_span(target.span);
                        if two {
                            self.emit(OpCode::Swap, 4);
                            self.emit(OpCode::Swap, 3);
                            self.emit(OpCode::Swap, 2);
                            self.emit(OpCode::StoreSlice, 0);
                        } else {
                            self.emit(OpCode::Swap, 3);
                            self.emit(OpCode::Swap, 2);
                            self.emit(OpCode::StoreSubscr, 0);
                        }
                        self.current_span = saved;
                    }
                    _ => {
                        // Name target: the load and the store sit at the
                        // target's own location, the in-place op at the
                        // statement's.
                        let saved = self.current_span;
                        self.set_span(target.span);
                        self.compile_load_target(target)?;
                        self.current_span = saved;
                        self.compile_expr(value)?;
                        self.emit(OpCode::BinaryOp, bin_arg);
                        self.set_span(target.span);
                        self.compile_assign(target)?;
                        self.current_span = saved;
                    }
                }
            }
            StmtKind::AnnAssign {
                target,
                annotation,
                value,
                simple,
            } => {
                // Always assign the value if provided, matching CPython
                // semantics: `x: int = 3` both binds `x` and records the
                // annotation.
                if let Some(v) = value {
                    self.compile_expr(v)?;
                    self.compile_assign(target)?;
                }
                // In class and module bodies, record the annotation
                // so `cls.__annotations__[name] = annotation` is
                // observable (used by `dataclasses`, `typing`). Only
                // *simple* targets annotate — `(pars): bool = True`
                // binds `pars` without an `__annotations__` entry.
                if matches!(self.code_kind, CodeKind::Class | CodeKind::Module) && *simple {
                    if let ExprKind::Name(name) = &target.kind {
                        if self.future_annotations {
                            // PEP 563: `__annotations__[name] = '<source>'`
                            // right here.
                            self.compile_annotation_record(name, annotation)?;
                        } else {
                            // PEP 649 (`_PyCompile_AddDeferredAnnotation`):
                            // the annotation is evaluated by the block's
                            // `__annotate__` function instead. A
                            // conditional annotation (every module-level
                            // one; a class-level one inside a compound
                            // statement) also records its index in
                            // `__conditional_annotations__` so
                            // `__annotate__` skips the ones whose
                            // statement never ran.
                            let cond_index = if matches!(self.code_kind, CodeKind::Module)
                                || self.in_conditional_block > 0
                            {
                                let i = self.next_cond_annotation_index;
                                self.next_cond_annotation_index += 1;
                                Some(i)
                            } else {
                                None
                            };
                            self.deferred_annotations.push(DeferredAnnotation {
                                name: name.clone(),
                                annotation: annotation.clone(),
                                span: stmt.span,
                                cond_index,
                            });
                            if let Some(i) = cond_index {
                                if matches!(self.code_kind, CodeKind::Class) {
                                    let idx =
                                        self.cell_or_free_index("__conditional_annotations__");
                                    self.emit(OpCode::LoadDeref, idx);
                                } else {
                                    let idx = self.co.intern_name("__conditional_annotations__");
                                    self.emit(OpCode::LoadName, idx);
                                }
                                let c = self.co.intern_constant(Constant::Int(i64::from(i)));
                                self.emit(OpCode::LoadConst, c);
                                self.emit(OpCode::SetAdd, 1);
                                self.emit(OpCode::PopTop, 0);
                            }
                        }
                    }
                }
                // `codegen_annassign` side-effect evaluation for targets
                // that don't record an annotation: an unassigned
                // attribute/subscript target evaluates its subexpressions
                // (`codegen_check_ann_expr` / `codegen_check_ann_subscr`,
                // each `POP_TOP` at the evaluated expression's location).
                // The annotation of a non-simple target is never
                // evaluated in 3.14 (`codegen_check_annotation` is only
                // reached under PEP 563, where it is a no-op).
                if value.is_none() {
                    match &target.kind {
                        ExprKind::Attribute { value: obj, .. } => {
                            self.compile_check_ann_expr(obj)?;
                        }
                        ExprKind::Subscript { value: obj, slice } => {
                            self.compile_check_ann_expr(obj)?;
                            self.compile_check_ann_subscr(slice)?;
                        }
                        _ => {}
                    }
                }
            }
            StmtKind::If { test, body, orelse } => {
                // `codegen_if`.
                let mut jump_else = Vec::new();
                self.compile_jump_if(test, false, &mut jump_else)?;
                for s in body {
                    self.compile_stmt(s)?;
                }
                if orelse.is_empty() {
                    let target = self.next_offset();
                    for j in jump_else {
                        self.patch_jump(j, target);
                    }
                } else {
                    // Structural join jump: CPython's NO_LOCATION
                    // JUMP_NO_INTERRUPT (`codegen_if`).
                    let jump_end = self.emit_no_line(OpCode::JumpForward, 0);
                    self.no_interrupt_jumps.insert(jump_end);
                    let else_target = self.next_offset();
                    for j in jump_else {
                        self.patch_jump(j, else_target);
                    }
                    for s in orelse {
                        self.compile_stmt(s)?;
                    }
                    let end_target = self.next_offset();
                    self.patch_jump(jump_end, end_target);
                }
            }
            StmtKind::While { test, body, orelse } => {
                // CPython 3.14 `codegen_while`: the condition is compiled
                // once at the loop head (`loop: jump_if(test, anchor,
                // false); body; JUMP loop`). 3.12/3.13 duplicated the test
                // after the body (a rotated loop that exited from the
                // bottom copy); 3.14 retired that shape, so the test line
                // fires once per pass when the back edge lands on it.
                //
                // `while 1:` / `while True:` (a constant-true test) gets
                // no test at all: a NOP carrying the `while` line is the
                // loop head (so the header fires a `line` event on entry
                // and each time the back edge lands on it). The back edge
                // itself has NO_LOCATION in CPython and so inherits the
                // preceding body instruction's line.
                //
                // `while 1:` / `while True:` compiles the test like any
                // other; the flowgraph folds `LOAD_CONST; TO_BOOL;
                // POP_JUMP_IF_FALSE` down to a NOP carrying the `while`
                // line, which stays as the loop head (so the header fires
                // a `line` event on entry and each time the back edge
                // lands on it).
                let loop_start = self.next_offset();
                let mut jump_exit = Vec::new();
                self.compile_jump_if(test, false, &mut jump_exit)?;
                let seq = self.next_fblock_seq();
                self.loop_stack.push(LoopFrame {
                    // `continue` is `JUMP loop`: re-run the test (or land
                    // on the header NOP of a constant-true loop).
                    continue_target: loop_start,
                    break_sites: Vec::new(),
                    is_for_loop: false,
                    exc_on_stack_at_entry: self.exc_on_stack,
                    pending_retvals_at_entry: self.pending_retvals,
                    seq,
                });
                for s in body {
                    self.compile_stmt(s)?;
                }
                // `codegen_while`: a NO_LOCATION plain `JUMP loop`.
                let back = self.emit_no_line(OpCode::JumpBackward, 0);
                self.plain_jumps.insert(back);
                self.patch_jump(back, loop_start);
                let frame = self.loop_stack.pop().expect("loop frame");
                // Natural exit: condition went false. Run the
                // `orelse` block.
                let orelse_target = self.next_offset();
                for site in jump_exit {
                    self.patch_jump(site, orelse_target);
                }
                for s in orelse {
                    self.compile_stmt(s)?;
                }
                // `break` jumps here, *past* the `orelse`. This
                // is the CPython semantics for while/else +
                // break — the else only runs when the loop
                // exits via its condition.
                let exit_target = self.next_offset();
                for site in frame.break_sites {
                    self.patch_jump(site, exit_target);
                }
            }
            StmtKind::For {
                target,
                iter,
                body,
                orelse,
            } => {
                self.compile_expr(iter)?;
                // PEP-657: `GET_ITER` (iter() failure) and `FOR_ITER`
                // (__next__ failure) report the iterator *expression* as
                // the error location, matching CPython's traceback columns.
                self.set_span(iter.span);
                self.emit(OpCode::GetIter, 0);
                let loop_top = self.next_offset();
                self.set_span(iter.span);
                let for_site = self.emit(OpCode::ForIter, 0);
                // Remember FOR_ITER's source line so END_FOR can reuse it (see
                // the END_FOR emission below).
                let for_line = self.current_line;
                // `codegen_for`: "Add NOP to ensure correct line tracing
                // of multiline for statements. It will be removed later
                // if redundant." Its slot outlives it: after the NOP is
                // dropped the body block's trailing slot still carries
                // this handler, which is what the NOT_TAKEN appended
                // by `normalize_jumps` picks up (stale `i_except`).
                self.set_line_from(target.span.start.0);
                self.set_span(target.span);
                self.emit(OpCode::Nop, 0);
                self.compile_assign(target)?;
                let seq = self.next_fblock_seq();
                self.loop_stack.push(LoopFrame {
                    continue_target: loop_top,
                    break_sites: Vec::new(),
                    is_for_loop: true,
                    exc_on_stack_at_entry: self.exc_on_stack,
                    pending_retvals_at_entry: self.pending_retvals,
                    seq,
                });
                for s in body {
                    self.compile_stmt(s)?;
                }
                // `codegen_for`: a NO_LOCATION plain `JUMP start`.
                let back = self.emit_no_line(OpCode::JumpBackward, 0);
                self.plain_jumps.insert(back);
                self.patch_jump(back, loop_top);
                let frame = self.loop_stack.pop().expect("loop frame");
                let after = self.next_offset();
                self.patch_jump(for_site, after);
                // Attribute END_FOR to the iterator expression (the `for` line),
                // matching CPython. FOR_ITER already fired a line event for this
                // line on the final iteration, so reusing the line prevents a
                // spurious `line` event for the loop body after exhaustion.
                self.set_span(iter.span);
                self.current_line = for_line;
                // CPython 3.14's loop-exit pair: END_FOR then POP_ITER
                // (the exhausted FOR_ITER pops the iterator and jumps
                // *past* both at runtime — they exist as the jump
                // target and for instrumentation; test_dis asserts the
                // shape). The VM skips them the same way.
                self.emit(OpCode::EndFor, 0);
                self.emit(OpCode::PopIter, 0);
                for s in orelse {
                    self.compile_stmt(s)?;
                }
                let break_target = self.next_offset();
                for site in frame.break_sites {
                    self.patch_jump(site, break_target);
                }
            }
            StmtKind::AsyncFor {
                target,
                iter,
                body,
                orelse,
            } => {
                if !self.in_async_context() {
                    if self.allows_top_level_await() {
                        self.co.is_coroutine = true;
                    } else {
                        return Err(CompileError::spanned(
                            "'async for' outside async function",
                            stmt.span,
                        ));
                    }
                }
                self.compile_async_for(target, iter, body, orelse)?;
            }
            StmtKind::FunctionDef {
                name,
                args,
                body,
                decorator_list,
                type_params,
                returns,
            } => {
                if type_params.is_empty() {
                    self.compile_function_def(
                        name,
                        args,
                        body,
                        decorator_list,
                        returns.as_deref(),
                    )?;
                } else {
                    self.compile_generic_def(stmt)?;
                }
            }
            StmtKind::AsyncFunctionDef {
                name,
                args,
                body,
                decorator_list,
                type_params,
                returns,
            } => {
                if type_params.is_empty() {
                    self.compile_async_function_def(
                        name,
                        args,
                        body,
                        decorator_list,
                        returns.as_deref(),
                    )?;
                } else {
                    self.compile_generic_def(stmt)?;
                }
            }
            StmtKind::ClassDef {
                name,
                bases,
                keywords,
                body,
                decorator_list,
                type_params,
            } => {
                if type_params.is_empty() {
                    self.compile_class_def(name, bases, keywords, body, decorator_list)?;
                } else {
                    self.compile_generic_def(stmt)?;
                }
            }
            StmtKind::Try {
                body,
                handlers,
                orelse,
                finalbody,
            } => {
                self.compile_try(body, handlers, orelse, finalbody)?;
            }
            StmtKind::Raise { exc, cause } => {
                match (exc, cause) {
                    (None, _) => self.emit(OpCode::RaiseVarargs, 0),
                    (Some(e), None) => {
                        self.compile_expr(e)?;
                        self.emit(OpCode::RaiseVarargs, 1)
                    }
                    (Some(e), Some(c)) => {
                        self.compile_expr(e)?;
                        self.compile_expr(c)?;
                        self.emit(OpCode::RaiseVarargs, 2)
                    }
                };
            }
            StmtKind::With { items, body } => {
                self.compile_with(items, body)?;
            }
            StmtKind::AsyncWith { items, body } => {
                if !self.in_async_context() {
                    if self.allows_top_level_await() {
                        self.co.is_coroutine = true;
                    } else {
                        return Err(CompileError::spanned(
                            "'async with' outside async function",
                            stmt.span,
                        ));
                    }
                }
                self.compile_async_with(items, body)?;
            }
            StmtKind::Return(value) => {
                if self.kind != CodeKind::Function {
                    return Err(CompileError::spanned(
                        "'return' outside function",
                        stmt.span,
                    ));
                }
                // PEP 525: async generators cannot return a value (the
                // flag is set before the body compiles, so this sees it).
                if self.co.is_async_generator && value.is_some() {
                    return Err(CompileError::spanned(
                        "'return' with value in async generator",
                        stmt.span,
                    ));
                }
                // CPython's `preserve_tos`: a constant return value is
                // *not* pushed before the inlined finally bodies run —
                // codegen emits a located NOP at the `return` (the
                // traced line), the unwind, then LOAD_CONST +
                // RETURN_VALUE (fused to RETURN_CONST on the wire) at
                // the end (test_dis test_disassemble_try_finally,
                // _tryfinallyconst).
                let const_ret = match value {
                    None => true,
                    Some(v) => matches!(v.kind, ExprKind::Constant(_)),
                };
                // `codegen_return` locations: a constant value moves
                // `loc` to the value (a located NOP marks it); a bare
                // `return`, or a value on another line, adds a NOP at
                // the statement and returns there. The flowgraph drops
                // whichever NOPs turn out redundant.
                let mut ret_span = stmt.span;
                if !const_ret {
                    match value {
                        Some(v) => self.compile_expr(v)?,
                        None => unreachable!("const_ret covers None"),
                    }
                } else if let Some(v) = value {
                    ret_span = v.span;
                    self.set_span(ret_span);
                    self.emit(OpCode::Nop, 0);
                }
                let value_line = value
                    .as_ref()
                    .map(|v| self.line_index.line_for(v.span.start.0));
                if value.is_none()
                    || value_line != Some(self.line_index.line_for(stmt.span.start.0))
                {
                    ret_span = stmt.span;
                    self.set_span(ret_span);
                    self.emit(OpCode::Nop, 0);
                }
                self.set_span(ret_span);
                // CPython `codegen_return`: unwind the fblock stack
                // innermost-out (`codegen_unwind_fblock_stack`), then
                // `RETURN_VALUE` (or `LOAD_CONST; RETURN_VALUE`) at the
                // location the unwind left in `*ploc`. A non-constant
                // return value stays *on the operand stack* while the
                // inlined bodies run (they are stack-neutral) — a
                // synthetic `.retvalN` local would leak into
                // co_varnames, which test_dis grades verbatim.
                let const_value = if const_ret {
                    Some(match value {
                        Some(Expr {
                            kind: ExprKind::Constant(c),
                            ..
                        }) => c.clone().into(),
                        None => Constant::None,
                        Some(_) => unreachable!("const_ret guards the kind"),
                    })
                } else {
                    None
                };
                let (ploc, holes) =
                    self.unwind_for_return(Some((ret_span.start.0, ret_span.end.0)), !const_ret)?;
                if let Some(c) = const_value {
                    let idx = self.co.intern_constant(c);
                    self.emit_at(ploc, OpCode::LoadConst, idx);
                }
                self.emit_at(ploc, OpCode::ReturnValue, 0);
                // The inlined cleanups ran here for the return. Exclude
                // each frame's inline — through the RETURN_VALUE itself,
                // which CPython leaves uncovered (test_dis's try/finally
                // exception table) — from its owning try's coverage, so
                // a `raise` inside a return-path finally propagates
                // outward instead of re-running it.
                self.close_unwind_holes(holes);
            }
            StmtKind::Break => {
                // `codegen_break`: a located NOP so tracing reports the
                // `break` line before any inlined `finally` body runs
                // (test_sys_settrace test_break_through_finally), the
                // fblock unwind up to the loop, the loop's own unwind
                // (a `for` pops its iterator), then `JUMP exit` — the
                // last two at the location the unwind left.
                self.emit(OpCode::Nop, 0);
                let frame_top = self
                    .loop_stack
                    .last()
                    .ok_or_else(|| CompileError::spanned("'break' outside loop", stmt.span))?;
                let is_for = frame_top.is_for_loop;
                let (ploc, holes) =
                    self.unwind_for_loop_exit(Some((stmt.span.start.0, stmt.span.end.0)))?;
                if is_for {
                    self.emit_at(ploc, OpCode::PopTop, 0);
                }
                let site = self.emit_at(ploc, OpCode::JumpForward, 0);
                self.close_unwind_holes(holes);
                self.loop_stack
                    .last_mut()
                    .expect("loop frame")
                    .break_sites
                    .push(site);
            }
            StmtKind::Continue => {
                // `codegen_continue`: located NOP, unwind, `JUMP loop`.
                self.emit(OpCode::Nop, 0);
                let frame_top = self.loop_stack.last().ok_or_else(|| {
                    CompileError::spanned("'continue' not properly in loop", stmt.span)
                })?;
                let target = frame_top.continue_target;
                let (ploc, holes) =
                    self.unwind_for_loop_exit(Some((stmt.span.start.0, stmt.span.end.0)))?;
                let site = self.emit_at(ploc, OpCode::JumpBackward, 0);
                self.patch_jump(site, target);
                self.close_unwind_holes(holes);
            }
            StmtKind::Global(_) | StmtKind::Nonlocal(_) => {
                // Scope analysis handled these — no code emission needed.
            }
            StmtKind::Import(aliases) => {
                self.compile_import(aliases)?;
            }
            StmtKind::ImportFrom {
                module,
                names,
                level,
            } => {
                self.compile_import_from(module.as_deref(), names, *level)?;
            }
            StmtKind::Match { subject, cases } => {
                self.compile_match(subject, cases)?;
            }
        }
        Ok(())
    }

    /// `import a`, `import a as b`, `import a.b.c`, `import a.b.c as x`.
    ///
    /// CPython emits, per alias:
    /// ```text
    /// LOAD_CONST  0          ; level
    /// LOAD_CONST  None       ; fromlist
    /// IMPORT_NAME a.b.c
    /// (no asname): STORE_NAME a                    ; bind top-level
    /// (asname  x): LOAD_ATTR b, LOAD_ATTR c, STORE_NAME x
    /// ```
    fn compile_import(
        &mut self,
        aliases: &[weavepy_parser::ast::Alias],
    ) -> Result<(), CompileError> {
        for alias in aliases {
            let level_idx = self.co.intern_constant(Constant::Int(0));
            self.emit(OpCode::LoadConst, level_idx);
            let none_idx = self.co.intern_constant(Constant::None);
            self.emit(OpCode::LoadConst, none_idx);
            let name_idx = self.co.intern_name(&alias.name);
            self.emit(OpCode::ImportName, name_idx);
            match &alias.asname {
                None => {
                    // `import a.b.c` binds the top-level package name `a`.
                    let top = alias.name.split('.').next().unwrap_or(&alias.name);
                    self.emit_store_name(top);
                }
                Some(asname) => {
                    // `import a.b.c as x` walks the chain with IMPORT_FROM,
                    // not plain attribute loads (bpo-30024): IMPORT_FROM
                    // falls back to the (possibly still-initialising)
                    // submodule in `sys.modules`, which is what makes
                    // circular `import numpy._core.multiarray as ma`
                    // inside `numpy._core.__init__` resolvable.
                    //
                    // CPython's `codegen_import_as`: the SWAP/POP_TOP
                    // discards the *previous* module only between
                    // attributes; after the last IMPORT_FROM the leaf
                    // is stored and the module below it is popped.
                    let mut parts = alias.name.split('.');
                    let _ = parts.next();
                    let attrs: Vec<&str> = parts.collect();
                    if attrs.is_empty() {
                        self.emit_store_name(asname);
                    } else {
                        for (i, part) in attrs.iter().enumerate() {
                            let idx = self.co.intern_name(part);
                            self.emit(OpCode::ImportFrom, idx);
                            if i + 1 < attrs.len() {
                                self.emit(OpCode::Swap, 2);
                                self.emit(OpCode::PopTop, 0);
                            }
                        }
                        self.emit_store_name(asname);
                        self.emit(OpCode::PopTop, 0);
                    }
                }
            }
        }
        Ok(())
    }

    /// `from m import a, b as c` / `from . import x` / `from .pkg import y`.
    ///
    /// Per CPython:
    /// ```text
    /// LOAD_CONST  <level>
    /// LOAD_CONST  (name1, name2, ...)
    /// IMPORT_NAME m
    /// IMPORT_FROM name1
    /// STORE_NAME  name1_or_asname
    /// IMPORT_FROM name2
    /// STORE_NAME  name2_or_asname
    /// POP_TOP                  ; discard the module
    /// ```
    fn compile_import_from(
        &mut self,
        module: Option<&str>,
        names: &[weavepy_parser::ast::Alias],
        level: u32,
    ) -> Result<(), CompileError> {
        let level_idx = self.co.intern_constant(Constant::Int(i64::from(level)));
        self.emit(OpCode::LoadConst, level_idx);
        let from_tuple: Vec<Constant> = names
            .iter()
            .map(|a| Constant::Str(a.name.clone()))
            .collect();
        let from_idx = self.co.intern_constant(Constant::Tuple(from_tuple));
        self.emit(OpCode::LoadConst, from_idx);
        let module_name = module.unwrap_or("");
        let name_idx = self.co.intern_name(module_name);
        self.emit(OpCode::ImportName, name_idx);

        // `from m import *` is its own opcode and binds every public name.
        // CPython lowers it to CALL_INTRINSIC_1(IMPORT_STAR) which returns
        // None, popped by an explicit POP_TOP — the VM's ImportStar pushes
        // None to match (test_dis test_intrinsic_1 grades the pair).
        if names.len() == 1 && names[0].name == "*" {
            self.emit(OpCode::ImportStar, 0);
            self.emit(OpCode::PopTop, 0);
            return Ok(());
        }

        for alias in names {
            let from_idx = self.co.intern_name(&alias.name);
            self.emit(OpCode::ImportFrom, from_idx);
            let target = alias.asname.as_deref().unwrap_or(&alias.name);
            self.emit_store_name(target);
        }
        self.emit(OpCode::PopTop, 0);
        Ok(())
    }

    // ---------- structural pattern matching (RFC 0009) ----------
    //
    // Faithful port of CPython's `compile.c` pattern codegen
    // (`compiler_match_inner` and the `codegen_pattern_*` family).
    // The key invariants, quoting CPython:
    //
    // - `on_top` tracks the number of *working* items currently on the
    //   top of the stack (subjects being examined, unpacked element
    //   tuples, …). They are popped by the fail-pop chain on failure.
    // - Captured values are *not* stored immediately: they are rotated
    //   *underneath* the working items and recorded in `stores`; the
    //   actual `STORE_NAME`s happen only once the entire case pattern
    //   has matched. This is what makes a failed `|` alternative (or a
    //   failed later sub-pattern) leave no bindings behind.
    // - Every conditional failure jumps to `fail_pops[k]` where `k` is
    //   the number of stack items to discard; the chain of `POP_TOP`s
    //   is emitted after the success jump, attributed to the pattern's
    //   source location (not the last line of the body).

    /// Lower `match subject: case ...:` into bytecode
    /// (CPython `compiler_match_inner`).
    fn compile_match(&mut self, subject: &Expr, cases: &[MatchCase]) -> Result<(), CompileError> {
        // CPython's compile-stage pattern validation (PEP 634): duplicate
        // name bindings, unreachable alternatives, mismatched `|` binding
        // sets, duplicate literal mapping keys, repeated class-pattern
        // attributes, multiple stars. An irrefutable pattern (bare capture
        // or wildcard) is only allowed on the last case or under a guard.
        let cases_len = cases.len();
        for (i, case) in cases.iter().enumerate() {
            let allow_irrefutable = case.guard.is_some() || i + 1 == cases_len;
            let mut stores: Vec<String> = Vec::new();
            validate_case_pattern(&case.pattern, allow_irrefutable, &mut stores)?;
        }
        self.compile_expr(subject)?;
        // A trailing `case _:` saves the redundant COPY/POP_TOP dance:
        // the second-to-last case consumes the subject directly and the
        // default body runs with a clean stack.
        let has_default = matches!(
            cases[cases_len - 1].pattern.kind,
            weavepy_parser::ast::PatternKind::Capture(None)
        ) && cases_len > 1;
        let ncompiled = cases_len - usize::from(has_default);
        let mut end_jumps: Vec<u32> = Vec::new();
        for (i, case) in cases.iter().take(ncompiled).enumerate() {
            self.set_line_from(case.pattern.span.start.0);
            self.set_span(case.pattern.span);
            // Only copy the subject if we're *not* on the last case:
            if i != ncompiled - 1 {
                self.emit(OpCode::CopyTop, 0);
            }
            let mut pc = PatmaCtx::default();
            self.compile_pattern(&case.pattern, &mut pc)?;
            debug_assert_eq!(pc.on_top, 0);
            // It's a match! Store all of the captured names (they're on
            // the stack, first capture on top).
            self.set_line_from(case.pattern.span.start.0);
            self.set_span(case.pattern.span);
            let stores = std::mem::take(&mut pc.stores);
            for name in &stores {
                self.compile_assign(&Expr {
                    kind: ExprKind::Name(name.clone()),
                    span: case.pattern.span,
                })?;
            }
            if let Some(guard) = &case.guard {
                // Guard failure jumps to fail_pops[0]: bindings from the
                // matched pattern intentionally survive (PEP 634).
                if pc.fail_pops.is_empty() {
                    pc.fail_pops.push(Vec::new());
                }
                let mut g = Vec::new();
                self.compile_jump_if(guard, false, &mut g)?;
                pc.fail_pops[0].extend(g);
                self.set_line_from(case.pattern.span.start.0);
                self.set_span(case.pattern.span);
            }
            // Success! Pop the subject off, we're done with it:
            if i != ncompiled - 1 {
                // "Use the next location to give better locations for
                // branch events" (`codegen_match_inner`).
                self.emit_next_location(OpCode::PopTop, 0);
            }
            for s in &case.body {
                self.compile_stmt(s)?;
            }
            // `codegen_match_inner`: a NO_LOCATION plain `JUMP end`;
            // the flowgraph's `propagate_line_numbers` stamps it with
            // the body's last location when the block allows.
            let j = self.emit_no_line(OpCode::JumpForward, 0);
            self.plain_jumps.insert(j);
            end_jumps.push(j);
            // The cleanup chain is associated with the failed pattern,
            // not the last line of the body:
            self.set_line_from(case.pattern.span.start.0);
            self.set_span(case.pattern.span);
            self.patma_emit_fail_pops(&mut pc);
        }
        if has_default {
            let case = &cases[cases_len - 1];
            self.set_line_from(case.pattern.span.start.0);
            self.set_span(case.pattern.span);
            // The subject was consumed by the previous case (which did
            // not copy); a NOP still gives the `case _:` line coverage.
            self.emit(OpCode::Nop, 0);
            if let Some(guard) = &case.guard {
                self.compile_jump_if(guard, false, &mut end_jumps)?;
            }
            for s in &case.body {
                self.compile_stmt(s)?;
            }
        }
        let end = self.next_offset();
        for j in end_jumps {
            self.patch_jump(j, end);
        }
        Ok(())
    }

    /// CPython `jump_to_fail_pop`: emit `op` jumping to the fail-pop
    /// block that discards everything this pattern currently has in
    /// flight (working items + deferred captures).
    fn patma_jump_to_fail_pop(&mut self, pc: &mut PatmaCtx, op: OpCode) {
        let pops = pc.on_top + pc.stores.len();
        if pc.fail_pops.len() <= pops {
            pc.fail_pops.resize_with(pops + 1, Vec::new);
        }
        let site = self.emit(op, 0);
        pc.fail_pops[pops].push(site);
    }

    /// CPython `emit_and_reset_fail_pop`: lay out the cascade
    /// `fail_pops[k]: POP_TOP; fail_pops[k-1]: POP_TOP; … fail_pops[0]:`
    /// so a jump to level `k` pops exactly `k` items, then falls
    /// through to the "no match" continuation.
    fn patma_emit_fail_pops(&mut self, pc: &mut PatmaCtx) {
        let fail_pops = std::mem::take(&mut pc.fail_pops);
        if fail_pops.is_empty() {
            return;
        }
        for k in (1..fail_pops.len()).rev() {
            let here = self.next_offset();
            for site in &fail_pops[k] {
                self.patch_jump(*site, here);
            }
            self.emit(OpCode::PopTop, 0);
        }
        let here = self.next_offset();
        for site in &fail_pops[0] {
            self.patch_jump(*site, here);
        }
    }

    /// CPython `pattern_helper_rotate`: move TOS down `count - 1`
    /// places (below the items currently above that slot).
    fn patma_rotate(&mut self, count: usize) {
        let mut count = count;
        while count > 1 {
            self.emit(OpCode::Swap, count as u32);
            count -= 1;
        }
    }

    /// CPython `pattern_helper_store_name`: defer the capture at TOS by
    /// rotating it underneath the working items and previous captures.
    /// `None` (wildcard) just pops. Duplicate-name errors were already
    /// raised by `validate_case_pattern`.
    fn patma_store_name(&mut self, name: Option<&str>, pc: &mut PatmaCtx) {
        match name {
            None => {
                self.emit(OpCode::PopTop, 0);
            }
            Some(n) => {
                let rotations = pc.on_top + pc.stores.len() + 1;
                self.patma_rotate(rotations);
                pc.stores.push(n.to_owned());
            }
        }
    }

    /// Compile a pattern (CPython `compiler_pattern`). The subject is
    /// at TOS. On success it is consumed (captures deferred beneath the
    /// working items); on failure control jumps into `pc.fail_pops`.
    fn compile_pattern(&mut self, pat: &Pattern, pc: &mut PatmaCtx) -> Result<(), CompileError> {
        use weavepy_parser::ast::PatternKind;
        self.set_line_from(pat.span.start.0);
        self.set_span(pat.span);
        match &pat.kind {
            PatternKind::Value(expr) => {
                self.compile_expr(expr)?;
                self.set_span(pat.span);
                self.emit(OpCode::CompareOp, CompareKind::Eq as u32);
                // `codegen_pattern_value`: the explicit TO_BOOL folds
                // into the COMPARE_OP's bool bit in the flowgraph.
                self.emit(OpCode::ToBool, 0);
                self.patma_jump_to_fail_pop(pc, OpCode::PopJumpIfFalse);
            }
            PatternKind::Singleton(c) => {
                let idx = self.co.intern_constant(c.clone().into());
                self.emit(OpCode::LoadConst, idx);
                self.emit(OpCode::IsOp, 0);
                self.patma_jump_to_fail_pop(pc, OpCode::PopJumpIfFalse);
            }
            PatternKind::Capture(name) => {
                self.patma_store_name(name.as_deref(), pc);
            }
            PatternKind::Star(name) => {
                self.patma_store_name(name.as_deref(), pc);
            }
            PatternKind::Sequence(items) => {
                self.compile_sequence_pattern(pat, items, pc)?;
            }
            PatternKind::Mapping {
                keys,
                patterns,
                rest,
            } => {
                self.compile_mapping_pattern(pat, keys, patterns, rest.as_ref(), pc)?;
            }
            PatternKind::Class {
                cls,
                positionals,
                keywords,
            } => {
                self.compile_class_pattern(pat, cls, positionals, keywords, pc)?;
            }
            PatternKind::Or(alts) => {
                self.compile_or_pattern(pat, alts, pc)?;
            }
            PatternKind::As { pattern, name } => {
                // Need to make a copy for (possibly) storing later:
                pc.on_top += 1;
                self.emit(OpCode::CopyTop, 0);
                self.compile_pattern(pattern, pc)?;
                // Success! Store it:
                pc.on_top -= 1;
                self.set_line_from(pat.span.start.0);
                self.set_span(pat.span);
                self.patma_store_name(Some(name), pc);
            }
        }
        Ok(())
    }

    /// CPython `compiler_pattern_sequence`.
    fn compile_sequence_pattern(
        &mut self,
        pat: &Pattern,
        items: &[Pattern],
        pc: &mut PatmaCtx,
    ) -> Result<(), CompileError> {
        use weavepy_parser::ast::PatternKind;
        let size = items.len();
        let star = items
            .iter()
            .position(|p| matches!(p.kind, PatternKind::Star(_)));
        let star_wildcard = star.is_some_and(|i| matches!(items[i].kind, PatternKind::Star(None)));
        let only_wildcard = items.iter().all(|p| {
            matches!(p.kind, PatternKind::Capture(None))
                || matches!(p.kind, PatternKind::Star(None))
        });
        // We need to keep the subject on top during the sequence and
        // length checks:
        pc.on_top += 1;
        self.emit(OpCode::MatchSequence, 0);
        self.patma_jump_to_fail_pop(pc, OpCode::PopJumpIfFalse);
        match star {
            None => {
                // No star: len(subject) == size
                self.emit(OpCode::GetLen, 0);
                let idx = self.co.intern_constant(Constant::Int(size as i64));
                self.emit(OpCode::LoadConst, idx);
                self.emit(OpCode::CompareOp, CompareKind::Eq as u32);
                self.patma_jump_to_fail_pop(pc, OpCode::PopJumpIfFalse);
            }
            Some(_) if size > 1 => {
                // Star: len(subject) >= size - 1
                self.emit(OpCode::GetLen, 0);
                let idx = self.co.intern_constant(Constant::Int((size - 1) as i64));
                self.emit(OpCode::LoadConst, idx);
                self.emit(OpCode::CompareOp, CompareKind::GtE as u32);
                self.patma_jump_to_fail_pop(pc, OpCode::PopJumpIfFalse);
            }
            // A lone `[*_]` / `[*x]` matches any length: no len() call
            // (Sequence-registered classes needn't have a usable __len__).
            Some(_) => {}
        }
        // Whatever comes next should consume the subject:
        pc.on_top -= 1;
        if only_wildcard {
            // Patterns like: [] / [_] / [_, _] / [*_] / [_, *_] / etc.
            self.emit(OpCode::PopTop, 0);
        } else if star_wildcard {
            self.patma_sequence_subscr(pat, items, star.unwrap(), pc)?;
        } else {
            self.patma_sequence_unpack(pat, items, star, pc)?;
        }
        Ok(())
    }

    /// CPython `pattern_helper_sequence_unpack`: UNPACK the subject and
    /// match each element (the unpacked items count toward `on_top`).
    fn patma_sequence_unpack(
        &mut self,
        pat: &Pattern,
        items: &[Pattern],
        star: Option<usize>,
        pc: &mut PatmaCtx,
    ) -> Result<(), CompileError> {
        let n = items.len();
        match star {
            Some(si) => {
                if si >= (1 << 8) || n - si > (1 << 8) {
                    return Err(CompileError::spanned(
                        "too many expressions in star-unpacking sequence pattern",
                        pat.span,
                    ));
                }
                // Our UnpackEx encoding: before in the high byte.
                self.emit(OpCode::UnpackEx, ((si as u32) << 8) | (n - si - 1) as u32);
            }
            None => {
                self.emit(OpCode::UnpackSequence, n as u32);
            }
        }
        // We've now got a bunch of new subjects on the stack (first
        // element on top). They need to remain there after each
        // subpattern match:
        pc.on_top += n;
        for item in items {
            // One less item to keep track of each time we loop through:
            pc.on_top -= 1;
            self.compile_pattern(item, pc)?;
        }
        Ok(())
    }

    /// CPython `pattern_helper_sequence_subscr`: for patterns with a
    /// starred wildcard, index the needed elements instead of unpacking.
    fn patma_sequence_subscr(
        &mut self,
        pat: &Pattern,
        items: &[Pattern],
        star: usize,
        pc: &mut PatmaCtx,
    ) -> Result<(), CompileError> {
        use weavepy_parser::ast::PatternKind;
        // We need to keep the subject around for extracting elements:
        pc.on_top += 1;
        let size = items.len();
        for (i, item) in items.iter().enumerate() {
            if matches!(item.kind, PatternKind::Capture(None)) {
                continue;
            }
            if i == star {
                continue;
            }
            self.set_line_from(pat.span.start.0);
            self.set_span(pat.span);
            self.emit(OpCode::CopyTop, 0);
            if i < star {
                let idx = self.co.intern_constant(Constant::Int(i as i64));
                self.emit(OpCode::LoadConst, idx);
            } else {
                // The subject may not support negative indexing! Compute
                // a nonnegative index:
                self.emit(OpCode::GetLen, 0);
                let idx = self.co.intern_constant(Constant::Int((size - i) as i64));
                self.emit(OpCode::LoadConst, idx);
                self.emit(OpCode::BinaryOp, BinOpKind::Sub as u32);
            }
            self.emit(OpCode::BinarySubscr, 0);
            self.compile_pattern(item, pc)?;
        }
        // Pop the subject, we're done with it:
        pc.on_top -= 1;
        self.set_line_from(pat.span.start.0);
        self.set_span(pat.span);
        self.emit(OpCode::PopTop, 0);
        Ok(())
    }

    /// CPython `compiler_pattern_mapping`.
    fn compile_mapping_pattern(
        &mut self,
        pat: &Pattern,
        keys: &[Expr],
        patterns: &[Pattern],
        rest: Option<&Option<String>>,
        pc: &mut PatmaCtx,
    ) -> Result<(), CompileError> {
        let size = keys.len();
        // We need to keep the subject on top during the mapping and
        // length checks:
        pc.on_top += 1;
        self.emit(OpCode::MatchMapping, 0);
        self.patma_jump_to_fail_pop(pc, OpCode::PopJumpIfFalse);
        if size == 0 && rest.is_none() {
            // If the pattern is just "{}", we're done! Pop the subject:
            pc.on_top -= 1;
            self.emit(OpCode::PopTop, 0);
            return Ok(());
        }
        if size > 0 {
            // If the pattern has any keys in it, perform a length check:
            self.emit(OpCode::GetLen, 0);
            let idx = self.co.intern_constant(Constant::Int(size as i64));
            self.emit(OpCode::LoadConst, idx);
            self.emit(OpCode::CompareOp, CompareKind::GtE as u32);
            self.patma_jump_to_fail_pop(pc, OpCode::PopJumpIfFalse);
        }
        // Collect all of the keys into a tuple for MATCH_KEYS and
        // **rest (duplicate literal keys were rejected at validation;
        // value-pattern collisions are a runtime ValueError):
        for k in keys {
            self.compile_expr(k)?;
        }
        self.set_line_from(pat.span.start.0);
        self.set_span(pat.span);
        self.emit(OpCode::BuildTuple, size as u32);
        // MATCH_KEYS peeks both; there's now a tuple of keys and a
        // tuple of values (or None) on top of the subject:
        self.emit(OpCode::MatchKeys, 0);
        pc.on_top += 2;
        self.emit(OpCode::CopyTop, 0);
        let none_idx = self.co.intern_constant(Constant::None);
        self.emit(OpCode::LoadConst, none_idx);
        self.emit(OpCode::IsOp, 1);
        self.patma_jump_to_fail_pop(pc, OpCode::PopJumpIfFalse);
        // So far so good. Use that tuple of values on the stack to
        // match sub-patterns against:
        self.emit(OpCode::UnpackSequence, size as u32);
        pc.on_top += size;
        pc.on_top -= 1;
        for p in patterns {
            pc.on_top -= 1;
            self.compile_pattern(p, pc)?;
        }
        // If we get this far, it's a match! Whatever happens next
        // should consume the tuple of keys and the subject:
        pc.on_top -= 2;
        self.set_line_from(pat.span.start.0);
        self.set_span(pat.span);
        if let Some(rest_name) = rest {
            // `**rest`: rest = dict(subject); for key in keys: del rest[key].
            // `DictUpdate 2` (arg `(depth - 1) << 1`) is CPython's
            // `DICT_UPDATE 2`: the target dict sits two slots below the
            // operand.
            self.emit(OpCode::BuildMap, 0); //      [subject, keys, {}]
            self.emit(OpCode::Swap, 3); //          [{}, keys, subject]
            self.emit(OpCode::DictUpdate, 2); //    [copy, keys]
            self.emit(OpCode::UnpackSequence, size as u32); // [copy, k_n..k_1]
            let mut remaining = size;
            while remaining > 0 {
                self.emit(OpCode::CopyTop, (1 + remaining) as u32); // [copy, keys.., copy]
                self.emit(OpCode::Swap, 2); //                         [copy, keys.., copy, key]
                self.emit(OpCode::DeleteSubscr, 0); //                 [copy, keys..]
                remaining -= 1;
            }
            self.patma_store_name(rest_name.as_deref(), pc);
        } else {
            self.emit(OpCode::PopTop, 0); // Tuple of keys.
            self.emit(OpCode::PopTop, 0); // Subject.
        }
        Ok(())
    }

    /// CPython `compiler_pattern_class`.
    fn compile_class_pattern(
        &mut self,
        pat: &Pattern,
        cls: &Expr,
        positionals: &[Pattern],
        keywords: &[(String, Pattern)],
        pc: &mut PatmaCtx,
    ) -> Result<(), CompileError> {
        use weavepy_parser::ast::PatternKind;
        let nargs = positionals.len();
        let nattrs = keywords.len();
        self.compile_expr(cls)?;
        self.set_line_from(pat.span.start.0);
        self.set_span(pat.span);
        let kw_names: Vec<Constant> = keywords
            .iter()
            .map(|(n, _)| Constant::Str(n.clone()))
            .collect();
        let kw_idx = self.co.intern_constant(Constant::Tuple(kw_names));
        self.emit(OpCode::LoadConst, kw_idx);
        self.emit(OpCode::MatchClass, nargs as u32);
        self.emit(OpCode::CopyTop, 0);
        let none_idx = self.co.intern_constant(Constant::None);
        self.emit(OpCode::LoadConst, none_idx);
        self.emit(OpCode::IsOp, 1);
        // TOS is now a tuple of (nargs + nattrs) attributes (or None):
        pc.on_top += 1;
        self.patma_jump_to_fail_pop(pc, OpCode::PopJumpIfFalse);
        self.emit(OpCode::UnpackSequence, (nargs + nattrs) as u32);
        pc.on_top += nargs + nattrs;
        pc.on_top -= 1;
        for i in 0..(nargs + nattrs) {
            pc.on_top -= 1;
            let pattern = if i < nargs {
                &positionals[i]
            } else {
                &keywords[i - nargs].1
            };
            if matches!(pattern.kind, PatternKind::Capture(None)) {
                self.emit(OpCode::PopTop, 0);
                continue;
            }
            self.compile_pattern(pattern, pc)?;
        }
        Ok(())
    }

    /// CPython `compiler_pattern_or`.
    fn compile_or_pattern(
        &mut self,
        pat: &Pattern,
        alts: &[Pattern],
        pc: &mut PatmaCtx,
    ) -> Result<(), CompileError> {
        let mut end_jumps: Vec<u32> = Vec::new();
        // `control` is the list of names bound by the first alternative;
        // later alternatives must bind the same set (validated earlier)
        // and get their stack slots reordered to match.
        let mut control: Option<Vec<String>> = None;
        for alt in alts {
            // Each alternative runs in a fresh sub-context against a
            // fresh copy of the subject:
            let mut sub = PatmaCtx::default();
            self.set_line_from(alt.span.start.0);
            self.set_span(alt.span);
            self.emit(OpCode::CopyTop, 0);
            self.compile_pattern(alt, &mut sub)?;
            // Success!
            let nstores = sub.stores.len();
            match &control {
                None => {
                    // First alternative: its stores become the control.
                    control = Some(sub.stores.clone());
                }
                Some(ctrl) => {
                    debug_assert_eq!(ctrl.len(), nstores);
                    // Reorder the captures on the stack (stores[0] is the
                    // item nearest TOS) to match the control order:
                    let ctrl = ctrl.clone();
                    let mut stores = sub.stores.clone();
                    self.set_line_from(alt.span.start.0);
                    self.set_span(alt.span);
                    for icontrol in (0..nstores).rev() {
                        let name = &ctrl[icontrol];
                        let istores = stores
                            .iter()
                            .position(|s| s == name)
                            .expect("validated: alternatives bind the same names");
                        if icontrol != istores {
                            debug_assert!(istores < icontrol);
                            let rotations = istores + 1;
                            // Perform the same rotation on the list:
                            // rotated = stores[:rotations]
                            // del stores[:rotations]
                            // stores[icontrol-istores:icontrol-istores] = rotated
                            let rotated: Vec<String> = stores.drain(0..rotations).collect();
                            let at = icontrol - istores;
                            for (k, n) in rotated.into_iter().enumerate() {
                                stores.insert(at + k, n);
                            }
                            // Do the same thing to the stack:
                            for _ in 0..rotations {
                                self.patma_rotate(icontrol + 1);
                            }
                        }
                    }
                    debug_assert_eq!(stores, ctrl);
                }
            }
            // `codegen_pattern_or`: the `JUMP end` and this alternative's
            // fail-pop cascade sit at `LOC(alt)`, not wherever the
            // alternative's last subpattern left the location.
            self.set_line_from(alt.span.start.0);
            self.set_span(alt.span);
            end_jumps.push(self.emit(OpCode::JumpForward, 0));
            self.patma_emit_fail_pops(&mut sub);
        }
        // No match. Pop the remaining copy of the subject and fail:
        self.set_line_from(pat.span.start.0);
        self.set_span(pat.span);
        self.emit(OpCode::PopTop, 0);
        self.patma_jump_to_fail_pop(pc, OpCode::JumpForward);
        // Success target:
        let end = self.next_offset();
        for j in end_jumps {
            self.patch_jump(j, end);
        }
        let control = control.expect("|-pattern has at least one alternative");
        // There's a bunch of stuff on the stack between where the new
        // stores are and where they need to be: the other new stores, a
        // copy of the subject, anything on top, and any previous stores.
        let nstores = control.len();
        let nrots = nstores + 1 + pc.on_top + pc.stores.len();
        for name in control {
            // Rotate this capture to its proper place on the stack
            // (duplicates against outer stores were rejected earlier):
            self.patma_rotate(nrots);
            pc.stores.push(name);
        }
        // Pop the copy of the subject:
        self.emit(OpCode::PopTop, 0);
        Ok(())
    }

    /// Compile a PEP 695 generic `def`/`async def`/`class` statement
    /// the way CPython's `codegen_function` / `codegen_class` do when
    /// `is_generic`: decorators and the function's *default values*
    /// evaluate in this scope; a hidden `<generic parameters of X>`
    /// scope (symtable `TypeParametersBlock`) then binds the type
    /// parameters as ordinary locals through `CALL_INTRINSIC_1/2`,
    /// defines the `def`/`class` inside it (so annotations, bases, and
    /// nested bodies close over the parameters), and returns the
    /// result, which this scope decorates and stores:
    ///
    /// ```text
    /// @dec
    /// def f[T](a: T = d()) -> T: return T
    /// # this scope
    /// dec; (d(),); <generic parameters of f>; SWAP 2; CALL 0; CALL 0; STORE f
    /// # <generic parameters of f>(.defaults)
    /// T = CALL_INTRINSIC_1 TYPEVAR('T'); (T,)
    /// LOAD_FAST .defaults; <annotate>; f = MAKE_FUNCTION
    /// SWAP 2; CALL_INTRINSIC_2 SET_FUNCTION_TYPE_PARAMS; RETURN_VALUE
    /// ```
    ///
    /// A generic class gets an implicit trailing `Generic[T, …]` base
    /// (`INTRINSIC_SUBSCRIPT_GENERIC`) and its `__type_params__` stored
    /// in the class namespace before the body runs. A *class's* type
    /// parameters are private-name mangled against the class's own
    /// name (`class Foo[__T]` binds `_Foo__T`), so references from the
    /// (independently mangled) class body resolve; a *function's* were
    /// already mangled against the enclosing class by
    /// [`mangle::mangle_class_body`]. The hidden scope never leaks:
    /// nothing else can observe the parameter bindings, and qualnames
    /// skip it (see [`Self::compute_child_qualname`]).
    fn compile_generic_def(&mut self, stmt: &Stmt) -> Result<(), CompileError> {
        let span = stmt.span;
        let (name, decorator_list, type_params) = match &stmt.kind {
            StmtKind::FunctionDef {
                name,
                decorator_list,
                type_params,
                ..
            }
            | StmtKind::AsyncFunctionDef {
                name,
                decorator_list,
                type_params,
                ..
            }
            | StmtKind::ClassDef {
                name,
                decorator_list,
                type_params,
                ..
            } => (name, decorator_list, type_params),
            _ => unreachable!("compile_generic_def: not a def/class"),
        };
        let is_class = matches!(stmt.kind, StmtKind::ClassDef { .. });
        let display = self.display_name(name).to_owned();
        let hidden_name = format!("<generic parameters of {display}>");
        // A decorated statement's code objects (hidden scope, body)
        // start at the *first decorator* (`codegen_function` /
        // `codegen_class` compute `firstlineno` once and hand it to
        // both).
        let entry_line = decorator_list
            .first()
            .map(|d| self.line_index.line_for(d.span.start.0))
            .filter(|l| *l != 0);
        let stmt_line = self.current_line;
        for d in decorator_list {
            self.compile_expr(d)?;
        }
        self.current_line = stmt_line;
        self.set_span(span);

        // A class's own type parameters (and the header expressions
        // that mention them) mangle against the class's name.
        let mut type_params = type_params.clone();
        let mut own_params: HashSet<String> = HashSet::new();
        let mangle_header = is_class && !display.trim_start_matches('_').is_empty();
        if mangle_header {
            // Source spellings: `mangle_only_in_expr` renames exactly
            // these identifiers in the header expressions.
            own_params = type_params.iter().map(|tp| tp.name.clone()).collect();
            for tp in &mut type_params {
                tp.name = mangle::mangle_ident(&display, &tp.name);
            }
            for tp in &mut type_params {
                if let TypeParamKind::TypeVar { bound: Some(b) } = &mut tp.kind {
                    mangle::mangle_only_in_expr(&display, &own_params, b);
                }
                if let Some(d) = &mut tp.default {
                    mangle::mangle_only_in_expr(&display, &own_params, d);
                }
            }
        }

        // Scope-analysis stand-ins for the type-parameter binders: each
        // binds its name in the hidden scope, and its bound / default
        // is a nested zero-argument thunk.
        let thunk = |e: &Expr| Expr {
            kind: ExprKind::TypeParamFn {
                args: AstArguments::default(),
                body: Box::new(e.clone()),
            },
            span: e.span,
        };
        let mut analysis: Vec<Stmt> = Vec::new();
        for tp in &type_params {
            let mut values: Vec<Expr> = Vec::new();
            if let TypeParamKind::TypeVar { bound: Some(b) } = &tp.kind {
                values.push(thunk(b));
            }
            if let Some(d) = &tp.default {
                values.push(thunk(match &d.kind {
                    ExprKind::Starred(inner) => inner,
                    _ => d,
                }));
            }
            analysis.push(Stmt {
                kind: StmtKind::Assign {
                    targets: vec![Expr {
                        kind: ExprKind::Name(tp.name.clone()),
                        span: tp.span,
                    }],
                    value: Expr {
                        kind: ExprKind::Tuple(values),
                        span: tp.span,
                    },
                },
                span: tp.span,
            });
        }

        match &stmt.kind {
            StmtKind::FunctionDef {
                args,
                body,
                returns,
                ..
            }
            | StmtKind::AsyncFunctionDef {
                args,
                body,
                returns,
                ..
            } => {
                let is_async = matches!(stmt.kind, StmtKind::AsyncFunctionDef { .. });
                // `codegen_default_arguments` runs here; the values ride
                // into the hidden scope as its `.defaults` /
                // `.kwdefaults` parameters.
                let hoisted = self.compile_default_arguments(args)?;
                let num_args = (hoisted & 0x01) + ((hoisted & 0x02) >> 1);
                // The symtable always declares `.defaults` in the hidden
                // scope (`symtable_enter_type_param_block` tests the
                // defaults *sequence*, not its length), so it heads
                // `co_varnames` even when nothing is passed; the actual
                // arguments fill the first `num_args` slots.
                let mut hidden_params: Vec<String> = vec![".defaults".to_owned()];
                if hoisted & 0x02 != 0 {
                    hidden_params.push(".kwdefaults".to_owned());
                }
                if num_args == 2 {
                    self.emit(OpCode::Swap, 2);
                }
                // The inner `def` carries no defaults of its own (they
                // are parameters now) and no decorators.
                let mut inner_args = args.clone();
                inner_args.defaults.clear();
                for d in &mut inner_args.kw_defaults {
                    *d = None;
                }
                let inner_stmt = Stmt {
                    kind: if is_async {
                        StmtKind::AsyncFunctionDef {
                            name: name.clone(),
                            args: inner_args.clone(),
                            body: body.clone(),
                            decorator_list: Vec::new(),
                            type_params: Vec::new(),
                            returns: returns.clone(),
                        }
                    } else {
                        StmtKind::FunctionDef {
                            name: name.clone(),
                            args: inner_args.clone(),
                            body: body.clone(),
                            decorator_list: Vec::new(),
                            type_params: Vec::new(),
                            returns: returns.clone(),
                        }
                    },
                    span,
                };
                analysis.push(inner_stmt);
                let returns = returns.as_deref();
                let code = self.compile_type_params_scope(
                    &hidden_name,
                    &display,
                    &hidden_params,
                    num_args,
                    &analysis,
                    name,
                    entry_line,
                    false,
                    |inner| {
                        inner.emit_type_params(&type_params)?;
                        // `codegen_function_body` at `LOC(s)`; the
                        // `def` line (not a decorator's) owns
                        // `MAKE_FUNCTION`.
                        inner.current_line = stmt_line;
                        inner.set_span(span);
                        inner.pep695_defaults = Some(hoisted);
                        inner.build_function_object_full(
                            name,
                            &inner_args,
                            body,
                            returns,
                            is_async,
                            entry_line,
                        )?;
                        inner.emit(OpCode::Swap, 2);
                        inner.emit(OpCode::CallIntrinsic2, intrinsic::SET_FUNCTION_TYPE_PARAMS);
                        inner.emit(OpCode::ReturnValue, 0);
                        Ok(())
                    },
                )?;
                self.emit_hidden_scope_closure(code);
                if num_args > 0 {
                    // `SWAP n+1; CALL n-1`: the hidden function under
                    // its hoisted arguments (the first rides the wire
                    // `CALL`'s self slot).
                    self.emit(OpCode::Swap, num_args + 1);
                    self.emit(OpCode::CallSelf, num_args);
                } else {
                    self.emit(OpCode::PushNull, 0);
                    self.emit(OpCode::Call, 0);
                }
            }
            StmtKind::ClassDef {
                bases,
                keywords,
                body,
                ..
            } => {
                let mut bases = bases.clone();
                let mut keywords = keywords.clone();
                if mangle_header {
                    for b in &mut bases {
                        mangle::mangle_only_in_expr(&display, &own_params, b);
                    }
                    for k in &mut keywords {
                        mangle::mangle_only_in_expr(&display, &own_params, &mut k.value);
                    }
                }
                analysis.push(Stmt {
                    kind: StmtKind::ClassDef {
                        name: name.clone(),
                        bases: bases.clone(),
                        keywords: keywords.clone(),
                        body: body.clone(),
                        decorator_list: Vec::new(),
                        type_params: Vec::new(),
                    },
                    span,
                });
                let code = self.compile_type_params_scope(
                    &hidden_name,
                    &display,
                    &[],
                    0,
                    &analysis,
                    name,
                    entry_line,
                    true,
                    |inner| {
                        inner.emit_type_params(&type_params)?;
                        // `STORE_DEREF .type_params` and everything
                        // after sit at `LOC(s)`.
                        inner.current_line = stmt_line;
                        inner.set_span(span);
                        inner.emit_store_name(".type_params");
                        inner
                            .compile_class_value(name, &bases, &keywords, body, entry_line, true)?;
                        inner.emit(OpCode::ReturnValue, 0);
                        Ok(())
                    },
                )?;
                self.emit_hidden_scope_closure(code);
                self.emit(OpCode::PushNull, 0);
                self.emit(OpCode::Call, 0);
            }
            _ => unreachable!(),
        }

        for d in decorator_list.iter().rev() {
            let saved = self.current_span;
            self.set_span(d.span);
            // The decorated value rides the self slot (CPython
            // `codegen_apply_decorators`: `CALL 0`, no PUSH_NULL).
            self.emit(OpCode::CallSelf, 1);
            self.current_span = saved;
        }
        self.compile_assign(&Expr {
            kind: ExprKind::Name(name.clone()),
            span,
        })
    }

    /// Build a PEP 695 hidden scope (`<generic parameters of X>`, or
    /// the same shape for a generic `type` alias) and return its code
    /// object. `hidden_params` are its leading locals, the first
    /// `arg_count` of them parameters (a generic `def`'s hoisted
    /// `.defaults` / `.kwdefaults`); `analysis_body`
    /// stands in for its statements in the free-variable analysis;
    /// `unbound` is the wrapped statement's name, which the *enclosing*
    /// scope binds; `body` emits the scope's code after the entry
    /// `RESUME` (and must end with `RETURN_VALUE`). A `class_scope`
    /// owns the `.type_params` cell the class body reads and the
    /// `.generic_base` local (`codegen_class`).
    ///
    /// The scope is function-like (`_PyST_IsFunctionLike`), never a
    /// method, and sees an enclosing class namespace through
    /// `__classdict__` (`ste_can_see_class_scope`).
    #[allow(clippy::too_many_arguments)]
    fn compile_type_params_scope(
        &mut self,
        hidden_name: &str,
        display: &str,
        hidden_params: &[String],
        arg_count: u32,
        analysis_body: &[Stmt],
        unbound: &str,
        entry_line: Option<u32>,
        class_scope: bool,
        body: impl FnOnce(&mut Compiler) -> Result<(), CompileError>,
    ) -> Result<CodeObject, CompileError> {
        let mut inner = Compiler::new(
            hidden_name.to_owned(),
            self.co.filename.clone(),
            CodeKind::Function,
            self.line_index.clone(),
            self.source.clone(),
            self.params.clone(),
        );
        inner.private = self.private.clone();
        inner.stopiteration_wrap = false;
        inner.co.is_method = false;
        inner.co.is_nested = self.child_is_nested();
        // The wrapped statement's qualname skips the hidden scope
        // (`compiler_set_qualname` looks through the annotations
        // scope to the grandparent).
        inner.annotation_qualname = Some((
            display.to_owned(),
            self.compute_child_qualname(display),
            self.annotation_child_prefix(),
        ));
        inner.pep695_unbound = Some(unbound.to_owned());
        inner.lazy_class_ctx = self.make_lazy_ctx();
        if inner.lazy_class_ctx.is_some() {
            inner
                .bindings
                .insert("__classdict__".to_owned(), Binding::Free);
            inner.free_order.push("__classdict__".to_owned());
        }
        inner.co.qualname = self.compute_child_qualname(hidden_name);
        inner.co.arg_count = arg_count;
        inner.co.varnames = hidden_params.to_vec();
        inner.current_line = entry_line.unwrap_or(self.current_line);
        if class_scope {
            // `.type_params` is read by the class body (always a cell,
            // sorted first: `.` precedes every identifier character);
            // `.generic_base` is a plain local.
            inner
                .bindings
                .insert(".type_params".to_owned(), Binding::Cell);
            inner.co.cellvars.push(".type_params".to_owned());
            inner
                .bindings
                .insert(".generic_base".to_owned(), Binding::Local);
        }
        inner.analyze_scope_function(hidden_params, analysis_body, &[&self.bindings]);
        for free in &inner.free_order {
            if matches!(self.bindings.get(free), Some(Binding::Local)) {
                self.bindings.insert(free.clone(), Binding::Cell);
                if !self.co.cellvars.contains(free) {
                    self.co.cellvars.push(free.clone());
                }
            }
        }
        inner.emit_entry_resume();
        body(&mut inner)?;
        Ok(inner.finish())
    }

    /// `codegen_make_closure(c, loc, co, 0)` for a hidden scope's code
    /// object, at the current location. Leaves the function on the
    /// stack.
    fn emit_hidden_scope_closure(&mut self, code: CodeObject) {
        self.emit_annotate_closure(code);
    }

    /// `codegen_type_params`: bind each type parameter in the current
    /// (hidden) scope and leave the tuple of them on the stack.
    ///
    /// ```text
    /// LOAD_CONST 'T'; [thunk]; CALL_INTRINSIC_{1,2} TYPEVAR…; [thunk;
    /// CALL_INTRINSIC_2 SET_TYPEPARAM_DEFAULT]; COPY 1; STORE_FAST T
    /// … BUILD_TUPLE n
    /// ```
    ///
    /// Each parameter's instructions carry its own location; the
    /// `BUILD_TUPLE` carries the first parameter's.
    fn emit_type_params(
        &mut self,
        type_params: &[weavepy_parser::ast::TypeParam],
    ) -> Result<(), CompileError> {
        for tp in type_params {
            let at_param = |c: &mut Compiler| {
                c.set_line_from(tp.span.start.0);
                c.set_span(tp.span);
            };
            at_param(self);
            let name_idx = self
                .co
                .intern_constant(Constant::Str(tp.source_name.clone()));
            self.emit(OpCode::LoadConst, name_idx);
            match &tp.kind {
                TypeParamKind::TypeVar { bound: None } => {
                    self.emit(OpCode::CallIntrinsic1, intrinsic::TYPEVAR);
                }
                TypeParamKind::TypeVar { bound: Some(b) } => {
                    self.emit_type_param_thunk(&tp.source_name, b, false)?;
                    at_param(self);
                    // A parenthesized tuple bound is a constraints list.
                    let id = if matches!(b.kind, ExprKind::Tuple(_)) {
                        intrinsic::TYPEVAR_WITH_CONSTRAINTS
                    } else {
                        intrinsic::TYPEVAR_WITH_BOUND
                    };
                    self.emit(OpCode::CallIntrinsic2, id);
                }
                TypeParamKind::TypeVarTuple => {
                    self.emit(OpCode::CallIntrinsic1, intrinsic::TYPEVARTUPLE);
                }
                TypeParamKind::ParamSpec => {
                    self.emit(OpCode::CallIntrinsic1, intrinsic::PARAMSPEC);
                }
            }
            if let Some(d) = &tp.default {
                // Only a TypeVarTuple's default may be starred
                // (`*Ts = *tuple[int]`).
                let allow_starred = matches!(tp.kind, TypeParamKind::TypeVarTuple);
                self.emit_type_param_thunk(&tp.source_name, d, allow_starred)?;
                at_param(self);
                self.emit(OpCode::CallIntrinsic2, intrinsic::SET_TYPEPARAM_DEFAULT);
            }
            self.emit(OpCode::CopyTop, 1);
            self.emit_store_name(&tp.name);
        }
        if let Some(first) = type_params.first() {
            self.set_line_from(first.span.start.0);
            self.set_span(first.span);
        }
        self.emit(OpCode::BuildTuple, type_params.len() as u32);
        Ok(())
    }

    /// `codegen_type_param_bound_or_default`: a zero-argument-style
    /// annotation scope (one `.format` parameter, defaulted to `1`)
    /// that evaluates a type parameter's bound, constraints, or
    /// default lazily. Its code object is named after the parameter.
    /// Leaves the function on the stack; every instruction carries
    /// `LOC(e)`.
    fn emit_type_param_thunk(
        &mut self,
        name: &str,
        e: &Expr,
        allow_starred: bool,
    ) -> Result<(), CompileError> {
        let loc = AnnotateLoc {
            line: self.line_index.line_for(e.span.start.0),
            span: Some(e.span),
        };
        self.apply_annotate_loc(loc);
        let defaults = self
            .co
            .intern_constant(Constant::Tuple(vec![Constant::Int(1)]));
        self.emit(OpCode::LoadConst, defaults);
        let value: &Expr = match (&e.kind, allow_starred) {
            (ExprKind::Starred(inner), true) => inner,
            _ => e,
        };
        let analysis = [Stmt {
            kind: StmtKind::Return(Some(value.clone())),
            span: e.span,
        }];
        let starred = !std::ptr::eq(value, e);
        let code = self.compile_annotation_scope(name, loc, &analysis, |inner| {
            inner.compile_expr(value)?;
            if starred {
                // `*Ts = *tuple[int]`: the thunk yields the unpacked
                // single element.
                inner.apply_annotate_loc(loc);
                inner.emit(OpCode::UnpackSequence, 1);
            }
            Ok(())
        })?;
        self.apply_annotate_loc(loc);
        let mut flags = 0x01;
        if !code.freevars.is_empty() {
            for free in &code.freevars {
                let idx = self.cell_or_free_index(free);
                self.emit(OpCode::LoadClosure, idx);
            }
            self.emit(OpCode::BuildTuple, code.freevars.len() as u32);
            flags |= 0x08;
        }
        let code_idx = self
            .co
            .intern_constant(Constant::Code(std::sync::Arc::new(code)));
        self.emit(OpCode::LoadConst, code_idx);
        self.emit_make_function(flags);
        Ok(())
    }

    /// Compile a `type` alias statement (`codegen_typealias`):
    ///
    /// ```text
    /// LOAD_CONST 'Alias'; LOAD_CONST None | <type params tuple>;
    /// <value thunk>; BUILD_TUPLE 3; CALL_INTRINSIC_1 TYPEALIAS
    /// ```
    ///
    /// A generic alias runs that inside a `<generic parameters of
    /// Alias>` hidden scope, exactly like a generic `def`.
    fn compile_type_alias(
        &mut self,
        stmt: &Stmt,
        name: &str,
        type_params: &[weavepy_parser::ast::TypeParam],
        value: &Expr,
    ) -> Result<(), CompileError> {
        let span = stmt.span;
        let display = self.display_name(name).to_owned();
        let thunk = |e: &Expr| Expr {
            kind: ExprKind::TypeParamFn {
                args: AstArguments::default(),
                body: Box::new(e.clone()),
            },
            span: e.span,
        };
        if type_params.is_empty() {
            let name_idx = self.co.intern_constant(Constant::Str(display.clone()));
            self.emit(OpCode::LoadConst, name_idx);
            let none_idx = self.co.intern_constant(Constant::None);
            self.emit(OpCode::LoadConst, none_idx);
            self.emit_type_alias_body(&display, value, span)?;
        } else {
            let hidden_name = format!("<generic parameters of {display}>");
            let mut analysis: Vec<Stmt> = Vec::new();
            for tp in type_params {
                let mut values: Vec<Expr> = Vec::new();
                if let TypeParamKind::TypeVar { bound: Some(b) } = &tp.kind {
                    values.push(thunk(b));
                }
                if let Some(d) = &tp.default {
                    values.push(thunk(match &d.kind {
                        ExprKind::Starred(inner) => inner,
                        _ => d,
                    }));
                }
                analysis.push(Stmt {
                    kind: StmtKind::Assign {
                        targets: vec![Expr {
                            kind: ExprKind::Name(tp.name.clone()),
                            span: tp.span,
                        }],
                        value: Expr {
                            kind: ExprKind::Tuple(values),
                            span: tp.span,
                        },
                    },
                    span: tp.span,
                });
            }
            analysis.push(Stmt {
                kind: StmtKind::Expr(thunk(value)),
                span: value.span,
            });
            let stmt_line = self.current_line;
            let code = self.compile_type_params_scope(
                &hidden_name,
                &display,
                &[],
                0,
                &analysis,
                "",
                None,
                false,
                |inner| {
                    inner.current_line = stmt_line;
                    inner.set_span(span);
                    let name_idx = inner.co.intern_constant(Constant::Str(display.clone()));
                    inner.emit(OpCode::LoadConst, name_idx);
                    inner.emit_type_params(type_params)?;
                    inner.current_line = stmt_line;
                    inner.set_span(span);
                    inner.emit_type_alias_body(&display, value, span)?;
                    inner.emit(OpCode::ReturnValue, 0);
                    Ok(())
                },
            )?;
            self.emit_hidden_scope_closure(code);
            self.emit(OpCode::PushNull, 0);
            self.emit(OpCode::Call, 0);
        }
        self.compile_assign(&Expr {
            kind: ExprKind::Name(name.to_owned()),
            span,
        })
    }

    /// `codegen_typealias_body`: the lazily-evaluated value thunk (an
    /// annotation scope named after the alias, `.format` defaulted to
    /// `1`), then `BUILD_TUPLE 3; CALL_INTRINSIC_1 INTRINSIC_TYPEALIAS`
    /// over the name, type-params tuple (or `None`), and thunk already
    /// on the stack. All at `LOC(s)`.
    fn emit_type_alias_body(
        &mut self,
        display: &str,
        value: &Expr,
        span: weavepy_lexer::Span,
    ) -> Result<(), CompileError> {
        let loc = AnnotateLoc {
            line: self.current_line,
            span: Some(span),
        };
        let defaults = self
            .co
            .intern_constant(Constant::Tuple(vec![Constant::Int(1)]));
        self.emit(OpCode::LoadConst, defaults);
        let analysis = [Stmt {
            kind: StmtKind::Return(Some(value.clone())),
            span: value.span,
        }];
        let code = self
            .compile_annotation_scope(display, loc, &analysis, |inner| inner.compile_expr(value))?;
        self.apply_annotate_loc(loc);
        let mut flags = 0x01;
        if !code.freevars.is_empty() {
            for free in &code.freevars {
                let idx = self.cell_or_free_index(free);
                self.emit(OpCode::LoadClosure, idx);
            }
            self.emit(OpCode::BuildTuple, code.freevars.len() as u32);
            flags |= 0x08;
        }
        let code_idx = self
            .co
            .intern_constant(Constant::Code(std::sync::Arc::new(code)));
        self.emit(OpCode::LoadConst, code_idx);
        self.emit_make_function(flags);
        self.emit(OpCode::BuildTuple, 3);
        self.emit(OpCode::CallIntrinsic1, intrinsic::TYPEALIAS);
        Ok(())
    }

    /// `codegen_default_arguments`: push the positional defaults tuple
    /// and the keyword-only defaults dict (each only if non-empty) and
    /// return the `MAKE_FUNCTION` flags they satisfy.
    fn compile_default_arguments(&mut self, args: &AstArguments) -> Result<u32, CompileError> {
        let mut flags: u32 = 0;
        if !args.defaults.is_empty() {
            for d in &args.defaults {
                self.compile_expr(d)?;
            }
            self.emit(OpCode::BuildTuple, args.defaults.len() as u32);
            flags |= 0x01;
        }
        // Keyword-only defaults are stored as a (name, value) dict —
        // CPython does the same. We build it on the stack as
        // `[name, value, name, value, ...]` and let BuildMap fold it
        // into a dict that MakeFunction will pop.
        let kw_default_pairs: Vec<(&str, &Expr)> = args
            .kwonlyargs
            .iter()
            .zip(args.kw_defaults.iter())
            .filter_map(|(arg, d)| d.as_ref().map(|d| (arg.name.as_str(), d)))
            .collect();
        if !kw_default_pairs.is_empty() {
            for (name, default) in &kw_default_pairs {
                let idx = self.co.intern_constant(Constant::Str((*name).into()));
                self.emit(OpCode::LoadConst, idx);
                self.compile_expr(default)?;
            }
            self.emit(OpCode::BuildMap, kw_default_pairs.len() as u32);
            flags |= 0x02;
        }
        Ok(flags)
    }

    fn compile_function_def(
        &mut self,
        name: &str,
        args: &AstArguments,
        body: &[Stmt],
        decorator_list: &[Expr],
        returns: Option<&Expr>,
    ) -> Result<(), CompileError> {
        self.compile_function_def_inner(name, args, body, decorator_list, returns, false)
    }

    fn compile_async_function_def(
        &mut self,
        name: &str,
        args: &AstArguments,
        body: &[Stmt],
        decorator_list: &[Expr],
        returns: Option<&Expr>,
    ) -> Result<(), CompileError> {
        self.compile_function_def_inner(name, args, body, decorator_list, returns, true)
    }

    fn compile_function_def_inner(
        &mut self,
        name: &str,
        args: &AstArguments,
        body: &[Stmt],
        decorator_list: &[Expr],
        returns: Option<&Expr>,
        is_async: bool,
    ) -> Result<(), CompileError> {
        // The statement span was set by `compile_stmt` and starts at the
        // `def` keyword (decorators are separate nodes); the final STORE
        // carries it, like CPython's `compiler_nameop(c, LOC(s), …)`.
        let def_span = self.current_span;
        let def_line = self.current_line;
        // A decorated function's code object starts at the *first
        // decorator* (CPython: RESUME location / co_firstlineno point at
        // `@dec`, so the 'call' trace event reports that line).
        let entry_line = decorator_list
            .first()
            .map(|d| self.line_index.line_for(d.span.start.0))
            .filter(|l| *l != 0);
        for d in decorator_list {
            self.compile_expr(d)?;
        }
        // MakeFunction (and the closing STORE below) belong to the `def`
        // line, not to whatever line the last decorator expression ended
        // on.
        self.current_line = def_line;
        self.current_span = def_span;
        self.build_function_object_full(name, args, body, returns, is_async, entry_line)?;
        // Decorators apply innermost-first; each application CALL carries
        // the *decorator expression's* location (CPython points the
        // traceback at `@dec`, not at the `def` line).
        for d in decorator_list.iter().rev() {
            let saved = self.current_span;
            self.set_span(d.span);
            // Decorated function rides the self slot (wire `CALL 0`).
            self.emit(OpCode::CallSelf, 1);
            self.current_span = saved;
        }
        let name_expr = Expr {
            kind: ExprKind::Name(name.to_owned()),
            span: weavepy_lexer::Span::new(def_span.0, def_span.1),
        };
        self.compile_assign(&name_expr)
    }

    /// Build a function object and leave it on the stack. Shared
    /// between `def` statements and `lambda` expressions.
    fn build_function_object(
        &mut self,
        name: &str,
        args: &AstArguments,
        body: &[Stmt],
    ) -> Result<(), CompileError> {
        self.build_function_object_full(name, args, body, None, false, None)
    }

    /// `entry_line`: line the child code object *starts* at when it
    /// differs from the enclosing statement's current line — the first
    /// decorator's line for a decorated `def` (CPython points RESUME /
    /// `co_firstlineno` there).
    fn build_function_object_full(
        &mut self,
        name: &str,
        args: &AstArguments,
        body: &[Stmt],
        returns: Option<&Expr>,
        is_async: bool,
        entry_line: Option<u32>,
    ) -> Result<(), CompileError> {
        // Fast-local slots follow CPython's order exactly:
        // positional-only, positional-or-keyword, keyword-only, then
        // `*args`, then `**kwargs`. The keyword-only names sit *before*
        // the `*args` slot — this is what `co_varnames` exposes and what
        // tools like `inspect` and `dis` expect.
        let mut param_names: Vec<String> = Vec::new();
        for a in &args.posonlyargs {
            param_names.push(a.name.clone());
        }
        for a in &args.args {
            param_names.push(a.name.clone());
        }
        for a in &args.kwonlyargs {
            param_names.push(a.name.clone());
        }
        if let Some(va) = &args.vararg {
            param_names.push(va.name.clone());
        }
        if let Some(kw) = &args.kwarg {
            param_names.push(kw.name.clone());
        }
        let posonly_count = args.posonlyargs.len() as u32;
        let arg_count = (args.posonlyargs.len() + args.args.len()) as u32;
        let kwonly_count = args.kwonlyargs.len() as u32;

        // `def __m` inside a class binds `_C__m` but keeps `__m` as its
        // `__name__`/`__qualname__`.
        let display = self.display_name(name);
        let mut inner = Compiler::new(
            display.to_owned(),
            self.co.filename.clone(),
            CodeKind::Function,
            self.line_index.clone(),
            self.source.clone(),
            self.params.clone(),
        );
        inner.private = self.private.clone();
        // `codegen_lambda` assembles a generator lambda directly, without
        // `codegen_wrap_in_stopiteration_handler` (only
        // `codegen_function_body` wraps).
        inner.stopiteration_wrap = display != "<lambda>";
        // CPython 3.14 `ste_method`: a *function* block whose parent is
        // the class block (methods, lambdas in the class body). A
        // generic method's parent is the hidden
        // `<generic parameters of X>` scope (a `TypeParametersBlock`),
        // so it is not a method.
        inner.co.is_method = self.co.is_class_body;
        inner.co.is_nested = self.child_is_nested();
        // PEP 695 lazy scope: the child consults `__classdict__` for
        // free/global loads; give it the cell as a free variable so
        // MakeFunction forwards it (the enclosing class body owns the
        // cell; intermediate hidden scopes forward it like any free).
        inner.lazy_class_ctx = self.pending_lazy_class_ctx.take();
        if inner.lazy_class_ctx.is_some() {
            inner
                .bindings
                .insert("__classdict__".to_owned(), Binding::Free);
            inner.free_order.push("__classdict__".to_owned());
        }
        inner.co.qualname = self.compute_child_qualname(display);
        inner.co.arg_count = arg_count;
        inner.co.posonly_count = posonly_count;
        inner.co.kwonly_count = kwonly_count;
        inner.co.has_varargs = args.vararg.is_some();
        inner.co.has_varkeywords = args.kwarg.is_some();
        inner.co.varnames = param_names.clone();
        inner.current_line = entry_line.unwrap_or(self.current_line);
        // Methods compiled inside a class body get an implicit
        // `__class__` free variable so `super()` (and explicit
        // `__class__` references) work without arguments. A scope that
        // already carries `__class__` (a method, or a PEP 695 hidden
        // scope inside a class body) forwards it to nested functions
        // the same way.
        // `Binding::Local` too (RFC 0076 WS4): CPython resolves the
        // implicit `__class__` through *normal* lexical scoping, so a
        // plain enclosing-function local satisfies it — attrs' generated
        // slots-`__getattr__` is compiled inside `def wrapper(_cls):
        // __class__ = _cls; def __getattr__(self, …): … super().…`, and
        // zero-arg `super()` must close over that local (the generic
        // child-free promotion below turns it into the wrapper's cell).
        let parent_forwards_class = self.inside_class_body
            || matches!(
                self.bindings.get("__class__"),
                Some(Binding::Free | Binding::Cell | Binding::Local)
            );
        if parent_forwards_class && method_references_class(body) {
            inner.bindings.insert("__class__".to_owned(), Binding::Free);
            inner.free_order.push("__class__".to_owned());
        }
        // CPython symtable: every `nonlocal` must resolve to a binding
        // in some enclosing *function* scope. Our scope analysis is
        // chained — each scope only consults its parent — but that's
        // sufficient: any name an outer function chain provides is
        // already forwarded into `self.bindings` (as Local / Cell /
        // Free) by the time this child compiles. The module scope
        // never satisfies a nonlocal, and a class-body Local is a class
        // attribute, not a nonlocal binding target.
        {
            let mut ng = HashSet::new();
            let mut nl = HashSet::new();
            let mut na = HashSet::new();
            for s in body {
                collect_decls(s, &mut ng, &mut nl, &mut na);
            }
            let mut nl: Vec<String> = nl.into_iter().collect();
            nl.sort_unstable();
            for n in nl {
                let ok = match self.kind {
                    CodeKind::Module => false,
                    // A class body always owns an implicit `__class__` cell
                    // (created on demand for `super()`/`__class__` uses), so
                    // `nonlocal __class__` in a method resolves against it
                    // even though no explicit binding exists (test_super's
                    // pathology-repair tearDown).
                    CodeKind::Class if n == "__class__" => true,
                    CodeKind::Class => matches!(
                        self.bindings.get(&n),
                        Some(Binding::Free | Binding::Nonlocal | Binding::ClassPassthrough)
                    ),
                    CodeKind::Function | CodeKind::Comprehension => matches!(
                        self.bindings.get(&n),
                        Some(
                            Binding::Local
                                | Binding::Cell
                                | Binding::Free
                                | Binding::Nonlocal
                                | Binding::ClassPassthrough
                        )
                    ),
                };
                if !ok {
                    let span = find_nonlocal_decl_span(body, &n).unwrap_or_else(|| {
                        body.first()
                            .map_or(weavepy_lexer::Span::new(0, 0), |s| s.span)
                    });
                    return Err(CompileError::spanned(
                        format!("no binding for nonlocal '{n}' found"),
                        span,
                    ));
                }
            }
        }
        // The scope's flavour is settled before the scope analysis:
        // whether an async comprehension may inline depends on it.
        let has_yield = body_is_generator(body);
        if is_async {
            // PEP 492: `async def` with `yield` is an async generator;
            // otherwise it's a coroutine. Both shapes share the
            // generator-style suspended-frame infrastructure.
            inner.co.is_async_generator = has_yield;
            inner.co.is_coroutine = !has_yield;
        } else {
            inner.co.is_generator = has_yield;
        }
        inner.analyze_scope_function(&param_names, body, &[&self.bindings]);
        for free in &inner.free_order {
            if matches!(self.bindings.get(free), Some(Binding::Local)) {
                self.bindings.insert(free.clone(), Binding::Cell);
                if !self.co.cellvars.contains(free) {
                    self.co.cellvars.push(free.clone());
                }
            }
        }
        if is_async || has_yield {
            inner.emit(OpCode::ReturnGenerator, 0);
            inner.emit(OpCode::PopTop, 0);
        }
        inner.emit_entry_resume();
        // CPython 3.14: a function with a docstring stores it in
        // `co_consts[0]` and sets `CO_HAS_DOCSTRING`; one without gets
        // *no* placeholder (3.13's leading `None` is gone, so `def
        // h(x): return x + 300` has `co_consts == (300,)`). Under `-OO`
        // the AST preprocessor has already removed the docstring, so
        // neither the constant nor the flag appears.
        if let Some(doc) = first_stmt_docstring(body).filter(|_| self.params.optimize < 2) {
            inner
                .co
                .intern_constant(Constant::Str(clean_docstring(doc)));
            inner.co.has_docstring = true;
        }
        // The docstring statement itself generates *no* code in a
        // function body (CPython consumes it into `co_consts[0]`); a
        // NOP here would fire a spurious `'line'` trace event on the
        // docstring line (test_trace test_issue9936).
        let stmts = if first_stmt_docstring(body).is_some() {
            &body[1..]
        } else {
            body
        };
        // `codegen_lambda`: a generator lambda's body value is left on
        // the stack and `_PyCodegen_AddReturnAtEnd(c, 0)` supplies a
        // NO_LOCATION `RETURN_VALUE` (a non-generator lambda's carries
        // the body location, which the synthetic `return` provides).
        // The location matters: `duplicate_exits_without_lineno` gives
        // each jump into a location-less return its own copy.
        let generator_lambda_body = match stmts {
            [Stmt {
                kind: StmtKind::Return(Some(value)),
                ..
            }] if name == "<lambda>" && has_yield && !is_async => Some(value),
            _ => None,
        };
        if let Some(value) = generator_lambda_body {
            inner.set_line_from(value.span.start.0);
            inner.set_span(value.span);
            inner.compile_expr(value)?;
            inner.emit_no_line(OpCode::ReturnValue, 0);
        } else {
            for s in stmts {
                inner.compile_stmt(s)?;
            }
        }
        let inner_code = inner.finish();
        let inner_freevars = inner_code.freevars.clone();

        let mut flags: u32 = if let Some(hoisted) = self.pep695_defaults.take() {
            // PEP 695 hidden scope: the defaults were evaluated by the
            // enclosing scope and passed in as `.defaults` /
            // `.kwdefaults`; re-push them (`codegen_function`:
            // `LOAD_FAST` of the first `num_typeparam_args` locals).
            for i in 0..(hoisted & 0x01) + ((hoisted & 0x02) >> 1) {
                self.emit(OpCode::LoadFast, i);
            }
            hoisted
        } else {
            self.compile_default_arguments(args)?
        };
        // PEP 649: the parameter and return annotations become an
        // `__annotate__` function (`MAKE_FUNCTION_ANNOTATE`) that
        // evaluates them on demand — `func.__annotations__` calls it the
        // first time it is read. Under PEP 563 the same function
        // returns the annotations' source strings.
        flags |= self.compile_function_annotations(args, returns)?;
        if !inner_freevars.is_empty() {
            for free in &inner_freevars {
                let idx = self.cell_or_free_index(free);
                self.emit(OpCode::LoadClosure, idx);
            }
            self.emit(OpCode::BuildTuple, inner_freevars.len() as u32);
            flags |= 0x08;
        }
        let code_idx = self
            .co
            .intern_constant(Constant::Code(std::sync::Arc::new(inner_code)));
        self.emit(OpCode::LoadConst, code_idx);
        self.emit_make_function(flags);
        Ok(())
    }

    /// PEP 649 (RFC 0077 WS10) — CPython `codegen_setup_annotations_scope`
    /// through `codegen_finish_annotations_scope`: compile the
    /// `__annotate__(format, /)` code object for this scope's
    /// annotations. `loc` is the location CPython threads through the
    /// prologue, epilogue, and closure construction; `analysis_body`
    /// stands in for the annotation scope's statements in the
    /// free-variable analysis (its reads are what the function closes
    /// over); `body` emits the annotations dict onto the stack.
    ///
    /// The scope is function-like (`_PyST_IsFunctionLike`): a single
    /// positional-only parameter, named `.format` in the symbol table
    /// so an annotation that spells `format` still reaches the
    /// builtin, and renamed `format` in `co_varnames` afterwards so
    /// `inspect.signature(f.__annotate__)` reads `(format, /)`.
    fn compile_annotate_code(
        &mut self,
        loc: AnnotateLoc,
        analysis_body: &[Stmt],
        body: impl FnOnce(&mut Compiler) -> Result<(), CompileError>,
    ) -> Result<CodeObject, CompileError> {
        self.compile_annotation_scope("__annotate__", loc, analysis_body, body)
    }

    /// The annotation-scope builder shared by `__annotate__` and the
    /// PEP 695 thunks (`codegen_setup_annotations_scope` /
    /// `codegen_leave_annotations_scope`): a type parameter's bound or
    /// default and a `type` alias's value compile as the same
    /// `(.format, /)` function, named after the parameter or alias.
    /// Those keep `.format` in `co_varnames` and always see an
    /// enclosing class namespace (their `__classdict__` use doesn't
    /// depend on PEP 563).
    fn compile_annotation_scope(
        &mut self,
        name: &str,
        loc: AnnotateLoc,
        analysis_body: &[Stmt],
        body: impl FnOnce(&mut Compiler) -> Result<(), CompileError>,
    ) -> Result<CodeObject, CompileError> {
        const FORMAT_PARAM: &str = ".format";
        let is_annotate = name == "__annotate__";
        let mut inner = Compiler::new(
            name.to_owned(),
            self.co.filename.clone(),
            CodeKind::Function,
            self.line_index.clone(),
            self.source.clone(),
            self.params.clone(),
        );
        inner.private = self.private.clone();
        inner.stopiteration_wrap = false;
        // An `AnnotationBlock` is never `ste_method`; it is nested when
        // its parent is (or is function-like), like any other child.
        inner.co.is_method = false;
        inner.co.is_nested = self.child_is_nested();
        // A class body's annotations (and the annotations of a `def`
        // written directly in one) see the class namespace through
        // `__classdict__` (`ste_can_see_class_scope`); a scope already
        // inside such a context hands it on. Not under PEP 563: the
        // strings read nothing, and `symtable_visit_annotation` skips
        // the `__classdict__` use (`current_type == ClassBlock &&
        // !future_annotations`).
        if !is_annotate || !self.future_annotations {
            inner.lazy_class_ctx = self.make_lazy_ctx();
        }
        if inner.lazy_class_ctx.is_some() {
            inner
                .bindings
                .insert("__classdict__".to_owned(), Binding::Free);
            inner.free_order.push("__classdict__".to_owned());
        }
        inner.co.qualname = self.compute_child_qualname(name);
        // A lambda or comprehension inside the annotation scope is
        // named from *this* scope (`compiler_set_qualname` looks
        // through the annotation parent).
        inner.annotation_qualname =
            Some((String::new(), String::new(), self.annotation_child_prefix()));
        inner.co.arg_count = 1;
        inner.co.posonly_count = 1;
        inner.co.varnames = vec![FORMAT_PARAM.to_owned()];
        inner.current_line = loc.line;
        inner.analyze_scope_function(&[FORMAT_PARAM.to_owned()], analysis_body, &[&self.bindings]);
        for free in &inner.free_order {
            if matches!(self.bindings.get(free), Some(Binding::Local)) {
                self.bindings.insert(free.clone(), Binding::Cell);
                if !self.co.cellvars.contains(free) {
                    self.co.cellvars.push(free.clone());
                }
            }
        }
        inner.emit_entry_resume();
        // `codegen_emit_annotations_prologue`:
        //   if format > VALUE_WITH_FAKE_GLOBALS: raise NotImplementedError
        // The format constant is `co_consts[0]` (interned first, and so
        // kept even though the flowgraph lowers its load to
        // LOAD_SMALL_INT — `remove_unused_consts` never drops slot 0).
        inner.apply_annotate_loc(loc);
        inner.emit(OpCode::LoadFast, 0);
        let fake_globals = inner.co.intern_constant(Constant::Int(i64::from(
            ANNOTATE_FORMAT_VALUE_WITH_FAKE_GLOBALS,
        )));
        inner.emit(OpCode::LoadConst, fake_globals);
        inner.emit(OpCode::CompareOp, CompareKind::Gt as u32);
        let to_body = inner.emit(OpCode::PopJumpIfFalse, 0);
        inner.emit(
            OpCode::LoadCommonConstant,
            COMMON_CONSTANT_NOT_IMPLEMENTED_ERROR,
        );
        inner.emit(OpCode::RaiseVarargs, 1);
        let body_label = inner.use_label();
        inner.patch_jump(to_body, body_label);
        body(&mut inner)?;
        inner.apply_annotate_loc(loc);
        inner.emit(OpCode::ReturnValue, 0);
        inner.line_pinned = None;
        let mut code = inner.finish();
        // `co->co_localsplusnames = ("format", *co->co_localsplusnames[1:])`
        if is_annotate {
            code.varnames[0] = "format".to_owned();
        }
        Ok(code)
    }

    /// `codegen_make_closure(c, loc, co, 0)` for an `__annotate__` code
    /// object: the closure tuple (when the scope has free variables),
    /// `MAKE_FUNCTION`, and `SET_FUNCTION_ATTRIBUTE closure`. Leaves the
    /// function on the stack; the caller has positioned the location.
    fn emit_annotate_closure(&mut self, code: CodeObject) {
        let mut flags = 0;
        if !code.freevars.is_empty() {
            for free in &code.freevars {
                let idx = self.cell_or_free_index(free);
                self.emit(OpCode::LoadClosure, idx);
            }
            self.emit(OpCode::BuildTuple, code.freevars.len() as u32);
            flags |= 0x08;
        }
        let code_idx = self
            .co
            .intern_constant(Constant::Code(std::sync::Arc::new(code)));
        self.emit(OpCode::LoadConst, code_idx);
        self.emit_make_function(flags);
    }

    /// Point the location state at an [`AnnotateLoc`].
    fn apply_annotate_loc(&mut self, loc: AnnotateLoc) {
        match loc.span {
            Some(span) => {
                self.line_pinned = None;
                self.current_line = loc.line;
                self.set_span(span);
            }
            None => {
                self.current_line = loc.line;
                self.line_pinned = Some(loc.line);
                self.pinned_colspan = ColSpan {
                    end_lineno: loc.line,
                    col: 0,
                    end_col: 0,
                };
            }
        }
    }

    /// The annotations of a `def` (`codegen_function_annotations`):
    /// when any parameter or the return is annotated, build the
    /// `__annotate__` function that evaluates them and leave it on
    /// the stack, returning `MAKE_FUNCTION_ANNOTATE`; otherwise emit
    /// nothing and return 0. The dict's key order is CPython's
    /// (`codegen_annotations_in_scope`): positional-or-keyword
    /// parameters *before* positional-only ones, then `*args`,
    /// keyword-only parameters, `**kwargs`, and `'return'`.
    fn compile_function_annotations(
        &mut self,
        args: &AstArguments,
        returns: Option<&Expr>,
    ) -> Result<u32, CompileError> {
        let mut annotated: Vec<(String, &Expr)> = Vec::new();
        for a in args
            .args
            .iter()
            .chain(args.posonlyargs.iter())
            .chain(args.vararg.iter())
            .chain(args.kwonlyargs.iter())
            .chain(args.kwarg.iter())
        {
            if let Some(ann) = a.annotation.as_ref() {
                annotated.push((a.name.clone(), ann));
            }
        }
        if let Some(ret) = returns {
            annotated.push(("return".to_owned(), ret));
        }
        if annotated.is_empty() {
            return Ok(0);
        }
        let loc = AnnotateLoc {
            line: self.current_line,
            span: Some(weavepy_lexer::Span::new(
                self.current_span.0,
                self.current_span.1,
            )),
        };
        // Under PEP 563 the annotations are string constants and read
        // nothing; otherwise every name they mention is a read of the
        // annotation scope.
        let analysis_body: Vec<Stmt> = if self.future_annotations {
            Vec::new()
        } else {
            annotated
                .iter()
                .map(|(_, ann)| Stmt {
                    kind: StmtKind::Expr((*ann).clone()),
                    span: ann.span,
                })
                .collect()
        };
        let n = annotated.len() as u32;
        let code = self.compile_annotate_code(loc, &analysis_body, |inner| {
            for (pname, ann) in &annotated {
                inner.apply_annotate_loc(loc);
                let idx = inner.co.intern_constant(Constant::Str(pname.clone()));
                inner.emit(OpCode::LoadConst, idx);
                inner.emit_annotation_value(ann, loc)?;
            }
            inner.apply_annotate_loc(loc);
            inner.emit(OpCode::BuildMap, n);
            Ok(())
        })?;
        self.apply_annotate_loc(loc);
        self.emit_annotate_closure(code);
        Ok(0x10)
    }

    /// `codegen_argannotation`'s value half: the PEP 563 string
    /// (`codegen_visit_annexpr`, at the annotation's location), or the
    /// evaluated expression — a PEP 646 `*args: *Ts` unpacks its single
    /// element with an `UNPACK_SEQUENCE 1` carried at `loc`.
    fn emit_annotation_value(
        &mut self,
        annotation: &Expr,
        loc: AnnotateLoc,
    ) -> Result<(), CompileError> {
        if self.future_annotations {
            self.line_pinned = None;
            self.set_line_from(annotation.span.start.0);
            self.set_span(annotation.span);
            return self.emit_annotation(annotation);
        }
        self.line_pinned = None;
        if let ExprKind::Starred(inner) = &annotation.kind {
            self.compile_expr(inner)?;
            self.apply_annotate_loc(loc);
            self.emit(OpCode::UnpackSequence, 1);
            return Ok(());
        }
        self.compile_expr(annotation)
    }

    /// `codegen_process_deferred_annotations`: the `__annotate__`
    /// function for the simple-name annotations a module or class
    /// body collected, left on the stack (`None` when the body had
    /// none). Its body builds an empty dict and stores each
    /// annotation into it, skipping conditional ones whose index is
    /// missing from `__conditional_annotations__`.
    fn compile_deferred_annotations(
        &mut self,
        loc: AnnotateLoc,
    ) -> Result<Option<CodeObject>, CompileError> {
        let deferred = std::mem::take(&mut self.deferred_annotations);
        if deferred.is_empty() {
            return Ok(None);
        }
        let is_class = matches!(self.kind, CodeKind::Class);
        let mut analysis_body: Vec<Stmt> = deferred
            .iter()
            .map(|d| Stmt {
                kind: StmtKind::Expr(d.annotation.clone()),
                span: d.annotation.span,
            })
            .collect();
        if is_class && deferred.iter().any(|d| d.cond_index.is_some()) {
            analysis_body.push(Stmt {
                kind: StmtKind::Expr(Expr {
                    kind: ExprKind::Name("__conditional_annotations__".to_owned()),
                    span: weavepy_lexer::Span::NO_LOCATION,
                }),
                span: weavepy_lexer::Span::NO_LOCATION,
            });
        }
        let code = self.compile_annotate_code(loc, &analysis_body, |inner| {
            inner.apply_annotate_loc(loc);
            inner.emit(OpCode::BuildMap, 0);
            for d in &deferred {
                let stmt_line = inner.line_index.line_for(d.span.start.0);
                let at_stmt = |inner: &mut Compiler| {
                    inner.line_pinned = None;
                    if stmt_line != 0 {
                        inner.current_line = stmt_line;
                    }
                    inner.set_span(d.span);
                };
                let mut skip: Option<u32> = None;
                if let Some(i) = d.cond_index {
                    at_stmt(inner);
                    let c = inner.co.intern_constant(Constant::Int(i64::from(i)));
                    inner.emit(OpCode::LoadConst, c);
                    if is_class {
                        let idx = inner.cell_or_free_index("__conditional_annotations__");
                        inner.emit(OpCode::LoadDeref, idx);
                    } else {
                        let idx = inner.co.intern_name("__conditional_annotations__");
                        inner.emit(OpCode::LoadGlobal, idx);
                    }
                    inner.emit(OpCode::ContainsOp, 0);
                    skip = Some(inner.emit(OpCode::PopJumpIfFalse, 0));
                }
                inner.line_pinned = None;
                inner.compile_expr(&d.annotation)?;
                at_stmt(inner);
                inner.emit(OpCode::CopyTop, 2);
                let key = inner.co.intern_constant(Constant::Str(d.name.clone()));
                inner.emit(OpCode::LoadConst, key);
                inner.apply_annotate_loc(loc);
                inner.emit(OpCode::StoreSubscr, 0);
                if let Some(site) = skip {
                    let target = inner.use_label();
                    inner.patch_jump(site, target);
                }
            }
            Ok(())
        })?;
        Ok(Some(code))
    }

    /// Compile a `class` statement. Emits the standard CPython recipe:
    /// `LOAD_BUILD_CLASS, build body, name, bases, [keywords], CALL`.
    /// Decorators wrap the result before it's stored.
    fn compile_class_def(
        &mut self,
        name: &str,
        bases: &[Expr],
        keywords: &[KwArg],
        body: &[Stmt],
        decorator_list: &[Expr],
    ) -> Result<(), CompileError> {
        // Statement span (starts at `class`, decorators are separate
        // nodes) — the final STORE carries it, like CPython's
        // `compiler_nameop(c, LOC(s), …)`.
        let class_span = self.current_span;
        let class_line = self.current_line;
        // A decorated class's body code starts at the *first decorator*
        // (CPython: RESUME location / `co_firstlineno` /
        // `__firstlineno__` point at `@dec`, so the class body's 'call'
        // trace event reports that line).
        let entry_line = decorator_list
            .first()
            .map(|d| self.line_index.line_for(d.span.start.0))
            .filter(|l| *l != 0);
        for d in decorator_list {
            self.compile_expr(d)?;
        }
        self.current_line = class_line;
        self.current_span = class_span;
        self.compile_class_value(name, bases, keywords, body, entry_line, false)?;
        for d in decorator_list.iter().rev() {
            let saved = self.current_span;
            self.set_span(d.span);
            // Decorated class rides the self slot (wire `CALL 0`).
            self.emit(OpCode::CallSelf, 1);
            self.current_span = saved;
        }
        let name_expr = Expr {
            kind: ExprKind::Name(name.to_owned()),
            span: weavepy_lexer::Span::new(class_span.0, class_span.1),
        };
        self.compile_assign(&name_expr)
    }

    /// Emit the `__build_class__` call for a `class` statement, leaving
    /// the new class on the stack (CPython `codegen_class` between the
    /// decorators and `codegen_apply_decorators`). `generic` is set by
    /// the PEP 695 hidden scope: the class then gets an implicit
    /// trailing `Generic[*type_params]` base
    /// (`INTRINSIC_SUBSCRIPT_GENERIC` on the `.type_params` cell,
    /// parked in the `.generic_base` local), and its body stores
    /// `__type_params__`.
    fn compile_class_value(
        &mut self,
        name: &str,
        bases: &[Expr],
        keywords: &[KwArg],
        body: &[Stmt],
        entry_line: Option<u32>,
        generic: bool,
    ) -> Result<(), CompileError> {
        let class_span = self.current_span;
        let generic_body;
        let generic_bases;
        let (body, bases): (&[Stmt], &[Expr]) = if generic {
            // `__type_params__ = .type_params` marker for the body
            // prologue (`build_class_body` hoists it after
            // `__firstlineno__`); it sits after the docstring so the
            // docstring stays `body[0]`.
            let mut b = body.to_vec();
            let at = usize::from(first_stmt_docstring(body).is_some());
            let sp = weavepy_lexer::Span::new(class_span.0, class_span.1);
            let name_at = |n: &str| Expr {
                kind: ExprKind::Name(n.to_owned()),
                span: sp,
            };
            b.insert(
                at,
                Stmt {
                    kind: StmtKind::Assign {
                        targets: vec![name_at("__type_params__")],
                        value: name_at(".type_params"),
                    },
                    span: sp,
                },
            );
            generic_body = b;
            let mut bs = bases.to_vec();
            bs.push(name_at(".generic_base"));
            generic_bases = bs;
            (&generic_body, &generic_bases)
        } else {
            (body, bases)
        };
        self.emit(OpCode::LoadBuildClass, 0);
        // `__build_class__` is a NULL-style callable (CPython:
        // `LOAD_BUILD_CLASS; PUSH_NULL; …`).
        self.emit(OpCode::PushNull, 0);

        // A `**kwds` in the class header (or a `*bases` splat) can't be
        // expressed with the fixed-arity `Call`/`CallKw` shapes, so fall
        // back to the same `CallEx` lowering the function-call site uses:
        // build a single positional args tuple `(body, name, *bases)` and
        // a merged keyword dict, then unpack both into `__build_class__`.
        let has_kw_splat = keywords.iter().any(|k| k.arg.is_none());
        let has_starred_base = bases.iter().any(|b| matches!(b.kind, ExprKind::Starred(_)));

        // `__build_class__` receives the *source* name (binding may be
        // mangled for a private nested class).
        let display = self.display_name(name).to_owned();
        if has_kw_splat
            || has_starred_base
            || bases.len() + keywords.len() * 2 > STACK_USE_GUIDELINE
        {
            // `codegen_call_helper(c, loc, 2, bases, keywords)` taking
            // the `ex_call` route: the two already-pushed operands
            // (body, name) seed the positional list.
            self.build_class_body(name, body, entry_line)?;
            let name_idx = self.co.intern_constant(Constant::Str(display.clone()));
            self.emit(OpCode::LoadConst, name_idx);
            if generic {
                self.emit_generic_base();
            }
            self.compile_ex_call_args(bases, None, 2)?;
            if keywords.is_empty() {
                self.emit(OpCode::PushNull, 0);
            } else {
                self.compile_ex_call_kwargs(keywords)?;
            }
            self.emit(OpCode::CallEx, 0);
        } else {
            self.build_class_body(name, body, entry_line)?;
            let name_idx = self.co.intern_constant(Constant::Str(display));
            self.emit(OpCode::LoadConst, name_idx);
            if generic {
                self.emit_generic_base();
            }
            for b in bases {
                self.compile_expr(b)?;
            }
            if keywords.is_empty() {
                self.emit(OpCode::Call, (bases.len() + 2) as u32);
            } else {
                let mut names: Vec<Constant> = Vec::with_capacity(keywords.len());
                for k in keywords {
                    let n = k
                        .arg
                        .clone()
                        .expect("kw splat handled by CallEx path above");
                    names.push(Constant::Str(n));
                    self.compile_expr(&k.value)?;
                }
                let tup_idx = self.co.intern_constant(Constant::Tuple(names));
                self.emit(OpCode::LoadConst, tup_idx);
                self.emit(OpCode::CallKw, (bases.len() + 2) as u32);
            }
        }
        Ok(())
    }

    /// `codegen_class` (generic): `LOAD_DEREF .type_params;
    /// CALL_INTRINSIC_1 INTRINSIC_SUBSCRIPT_GENERIC; STORE_FAST
    /// .generic_base`, emitted between the class name and the bases.
    fn emit_generic_base(&mut self) {
        self.emit_load_name(".type_params");
        self.emit(OpCode::CallIntrinsic1, intrinsic::SUBSCRIPT_GENERIC);
        self.emit_store_name(".generic_base");
    }

    /// Build the class-body function object and leave it on the stack.
    /// `entry_line`: first decorator's line for a decorated class (the
    /// child code object starts there — see `compile_class_def`).
    fn build_class_body(
        &mut self,
        name: &str,
        body: &[Stmt],
        entry_line: Option<u32>,
    ) -> Result<(), CompileError> {
        // `name` is the (possibly mangled) binding; the class's
        // *source* name drives `__name__`, `__qualname__`, and its own
        // mangling context.
        let name = self.display_name(name).to_owned();
        let name = name.as_str();
        // Index of the `__type_params__ = .type_params` marker a generic
        // class's hidden scope planted (right after any docstring). It
        // stays in `body` so the scope analysis below sees the
        // `.type_params` read, but it's emitted from the prologue.
        let type_params_marker = body.iter().take(2).position(|s| {
            matches!(&s.kind, StmtKind::Assign { targets, value }
                if targets.len() == 1
                    && matches!(&targets[0].kind, ExprKind::Name(n) if n == "__type_params__")
                    && matches!(&value.kind, ExprKind::Name(v) if v == ".type_params"))
        });
        // Private name mangling (CPython `_Py_Mangle`): rewrite `__spam`
        // identifiers throughout the class's textual scope before
        // compiling. Done on a clone so the caller's AST is untouched.
        // `__static_attributes__` reads the *source* spelling (the
        // symtable records `e->v.Attribute.attr` before mangling).
        let unmangled_body = body;
        let mangled_body;
        let body: &[Stmt] = if name.trim_start_matches('_').is_empty() {
            body
        } else {
            let mut b = body.to_vec();
            crate::mangle::mangle_class_body(name, &mut b);
            mangled_body = b;
            &mangled_body
        };
        let mut inner = Compiler::new(
            name.to_owned(),
            self.co.filename.clone(),
            CodeKind::Class,
            self.line_index.clone(),
            self.source.clone(),
            self.params.clone(),
        );
        inner.private = Some(Rc::from(name));
        // A class body never reports `CO_NESTED` itself (no
        // `CO_OPTIMIZED`), but its methods inherit the nesting.
        inner.co.is_nested = self.child_is_nested();
        inner.co.qualname = self.compute_child_qualname(name);
        inner.current_line = entry_line.unwrap_or(self.current_line);
        // CPython only gives a class body the `__class__` closure cell —
        // and the trailing `__classcell__` store — when a method actually
        // needs it (references zero-arg `super()` or `__class__`); see
        // `ste_needs_class_closure`. Otherwise a user-written
        // `__classcell__ = <value>` must survive into the namespace so
        // `type.__new__` can reject a non-cell (test_slots_special2). We
        // reuse the same free-variable analysis the body relies on, so the
        // signal can't be a false negative relative to what super() needs.
        let needs_class_closure = {
            // Inlined comprehensions are transparent here: a
            // `super()` written directly in one resolves `__class__`
            // as an implicit global (`inline_comprehension` drops the
            // comprehension's own `__class__` from the class's free
            // set), so only a real nested scope claims the cell.
            let scan = FreeScan {
                inline_comps: true,
                async_ok: false,
                class_body: false,
                class_binds: None,
            };
            let mut needed = HashSet::new();
            for s in body {
                collect_inner_free(s, &self.bindings, &mut needed, &scan);
            }
            // A method's `super` read surfaces as `__class__` (the
            // `FunctionDef` arm of `collect_inner_free`); a nested
            // class's methods are satisfied by *its* cell and never
            // reach here (the `ClassDef` arm drops `__class__`). A raw
            // `super` read at class-body level, or from a nested class,
            // is just the builtin — the symtable only special-cases
            // `super` in function-like scopes.
            needed.contains("__class__")
                // `collect_inner_free` misses one shape CPython's symtable
                // special-cases ("Special-case super: it counts as a use of
                // __class__", symtable.c): a method that *locally binds*
                // `super` and loads it — `sub = super = None; … if super is
                // None` (matplotlib `_mathtext.Parser.subsuper`). The method
                // side (`method_references_class` in `build_function_object`)
                // claims the `__class__` freevar for exactly that shape, so
                // the class must own the cell or the claim dangles ("bad
                // cell index" at runtime).
                || class_body_defs_claim_class_cell(body)
        };
        if needs_class_closure {
            inner.co.cellvars.push("__class__".to_owned());
            inner.bindings.insert("__class__".to_owned(), Binding::Cell);
        }
        // PEP 695 (RFC 0051): annotation scopes created in this class
        // body (generic def/class headers, `type` alias thunks) close
        // over a `__classdict__` cell that the VM seeds with the live
        // namespace mapping before the body runs.
        let future_annotations = self.params.future_annotations;
        let needs_classdict = body
            .iter()
            .any(|s| stmt_needs_classdict(s, future_annotations));
        if needs_classdict {
            inner.co.cellvars.push("__classdict__".to_owned());
            inner
                .bindings
                .insert("__classdict__".to_owned(), Binding::Cell);
        }
        // PEP 649: a class body with an annotation inside a compound
        // statement owns a `__conditional_annotations__` cell (symtable
        // `ste_has_conditional_annotations`; `_PyCompile_EnterScope`
        // cooks the implicit cell up after `__class__`/`__classdict__`)
        // — whether or not PEP 563 later turns the annotations into
        // strings.
        let has_conditional_annotations = block_has_conditional_annotations(body);
        if has_conditional_annotations {
            inner
                .co
                .cellvars
                .push("__conditional_annotations__".to_owned());
            inner
                .bindings
                .insert("__conditional_annotations__".to_owned(), Binding::Cell);
        }

        let mut assigned = HashSet::new();
        for s in body {
            collect_assigned(s, &mut assigned);
        }
        for n in &assigned {
            inner.bindings.insert(n.clone(), Binding::Global);
        }
        // Remembered for [`LazyClassCtx`]: annotation scopes created in
        // this body resolve these through the class dict, then globals.
        inner.class_assigned = assigned;
        // Track explicit `global X` declarations in the class body so a
        // nested `def X`/`class X` gets a bare qualname (see
        // `compute_child_qualname`).
        {
            let mut globals = HashSet::new();
            let mut nonlocals = HashSet::new();
            let mut decl_assigned = HashSet::new();
            for s in body {
                collect_decls(s, &mut globals, &mut nonlocals, &mut decl_assigned);
            }
            inner.explicit_globals = globals;
            // `nonlocal x` in a *class body* rebinds through the
            // enclosing function's cell — loads and stores bypass the
            // class namespace entirely, and the name never becomes a
            // class attribute (test_scope.testNonLocalClass).
            for n in &nonlocals {
                inner.bindings.insert(n.clone(), Binding::Free);
                if !inner.free_order.contains(n) {
                    inner.free_order.push(n.clone());
                }
                inner.class_assigned.remove(n);
            }
        }
        // A `__class__` read *in the class body itself* never sees the
        // implicit class cell: CPython resolves it class-dict-first, then
        // the *enclosing function's* `__class__` cell (LOAD_CLASSDEREF on
        // a same-named freevar), or as a plain name when no enclosing
        // function provides one (test_super
        // test_various___class___pathologies). Only reserve the freevar
        // when the enclosing scope can actually supply the cell.
        if class_body_reads_dunder_class(body)
            && !matches!(inner.bindings.get("__class__"), Some(Binding::Free))
            && matches!(
                self.bindings.get("__class__"),
                Some(Binding::Local | Binding::Cell | Binding::Free | Binding::Nonlocal)
            )
            && !inner.free_order.contains(&"__class__".to_owned())
        {
            inner.free_order.push("__class__".to_owned());
        }

        let outer_inside_class = inner.inside_class_body;
        inner.inside_class_body = true;
        let _ = outer_inside_class;

        // Resolve outer-scope free vars for names read by the body that
        // aren't bound locally.
        let mut reads = HashSet::new();
        for s in body {
            collect_reads_stmt(s, &mut reads);
        }
        let needed_in_inner = inner.needed_in_inner(true, |scan, out| {
            for s in body {
                collect_inner_free(s, &inner.bindings, out, scan);
            }
        });
        let mut free_candidates = reads;
        free_candidates.extend(needed_in_inner.iter().cloned());
        free_candidates.remove("__class__");
        // Sorted so `free_order` (→ `co_freevars`) is deterministic across
        // compiles — see the function-scope analogue above.
        let mut free_candidates: Vec<String> = free_candidates.into_iter().collect();
        free_candidates.sort_unstable();
        for name in free_candidates {
            if inner.bindings.contains_key(&name) {
                continue;
            }
            if let Some(b) = self.bindings.get(&name) {
                if matches!(
                    b,
                    Binding::Local
                        | Binding::Cell
                        | Binding::Free
                        | Binding::Nonlocal
                        | Binding::ClassPassthrough
                ) {
                    inner.bindings.insert(name.clone(), Binding::Free);
                    inner.free_order.push(name);
                }
            }
        }
        // Names assigned in the class body that a nested scope *also*
        // needs, where an enclosing function scope binds the same name:
        // Python skips class scopes when resolving closures, so the
        // nested scope must reach the enclosing cell. The class body
        // forwards it (the name joins `co_freevars`) while its own
        // loads/stores keep namespace semantics — see
        // [`Binding::ClassPassthrough`].
        let mut needed_in_inner: Vec<String> = needed_in_inner.into_iter().collect();
        needed_in_inner.sort_unstable();
        for name in &needed_in_inner {
            if name == "__class__" || name == "__classdict__" {
                continue;
            }
            if !matches!(inner.bindings.get(name), Some(Binding::Global)) {
                continue;
            }
            if matches!(
                self.bindings.get(name),
                Some(
                    Binding::Local
                        | Binding::Cell
                        | Binding::Free
                        | Binding::Nonlocal
                        | Binding::ClassPassthrough
                )
            ) {
                if inner.explicit_globals.contains(name) {
                    // `global y` in the class body: the class's own
                    // loads/stores stay global, but nested scopes skip
                    // the class scope (PEP 227) and still reach the
                    // enclosing function's `y` — forward the cell
                    // without touching the class-level binding.
                    inner.class_transparent_frees.insert(name.clone());
                } else {
                    inner
                        .bindings
                        .insert(name.clone(), Binding::ClassPassthrough);
                }
                if !inner.free_order.contains(name) {
                    inner.free_order.push(name.clone());
                }
            }
        }
        // Comprehension cells sort ahead of the implicit `__class__` /
        // `__classdict__` cells (`dictbytype` first, the implicit
        // cells appended by `compiler_enter_scope`).
        {
            let mut comp_cells = HashSet::new();
            for s in body {
                inner.collect_comp_cells_stmt(s, &mut comp_cells);
            }
            if !comp_cells.is_empty() {
                let implicit: Vec<String> = inner
                    .co
                    .cellvars
                    .iter()
                    .filter(|n| {
                        matches!(
                            n.as_str(),
                            "__class__" | "__classdict__" | "__conditional_annotations__"
                        )
                    })
                    .cloned()
                    .collect();
                inner.co.cellvars.retain(|n| !implicit.contains(n));
                inner.register_comp_cells(comp_cells);
                inner.co.cellvars.extend(implicit);
            }
        }

        inner.emit_entry_resume();
        // `codegen_class_body`: the prologue (`__module__ = __name__`,
        // `__qualname__ = <computed>`, `__firstlineno__ = N`, and the
        // `__classdict__` seed) carries `LOCATION(firstlineno,
        // firstlineno, 0, 0)` -- the class line with an empty column
        // span. The class body stores its full PEP 3155 qualname (e.g.
        // `Outer.method.<locals>.C`), not the bare name, so
        // `C.__qualname__` and `repr`s built from it match CPython.
        let firstlineno = entry_line.unwrap_or(self.current_line);
        {
            let saved_pin = inner.line_pinned;
            let saved_col = inner.pinned_colspan;
            inner.line_pinned = Some(firstlineno);
            inner.pinned_colspan = ColSpan {
                end_lineno: firstlineno,
                col: 0,
                end_col: 0,
            };
            inner.emit_load_name("__name__");
            inner.emit_store_name("__module__");
            let qualname_str = inner.co.qualname.clone();
            let qualname_const = inner.co.intern_constant(Constant::Str(qualname_str));
            inner.emit(OpCode::LoadConst, qualname_const);
            inner.emit_store_name("__qualname__");
            // A `nonlocal __firstlineno__` declaration in the class body
            // redirects this store to the enclosing function's cell -- the
            // class dict then carries no `__firstlineno__` and
            // `inspect.getsource` reports "source code not available"
            // (test_inspect test_getsource_on_class_without_firstlineno).
            // `emit_store_name` routes Free/Nonlocal bindings through
            // STORE_DEREF, exactly like `codegen_nameop`.
            let line_const = inner
                .co
                .intern_constant(Constant::Int(i64::from(firstlineno)));
            inner.emit(OpCode::LoadConst, line_const);
            inner.emit_store_name("__firstlineno__");
            // `codegen_class_body`: a generic class stores
            // `__type_params__` (read from the hidden scope's
            // `.type_params` cell) right after `__firstlineno__`, before
            // the `__classdict__` seed and the docstring. The marker
            // statement `compile_generic_def` planted in the body is
            // consumed here and skipped below.
            if type_params_marker.is_some() {
                inner.emit_load_name(".type_params");
                inner.emit_store_name("__type_params__");
            }
            if needs_classdict {
                // `STORE_DEREF` straight into the cellvar: `codegen_nameop`
                // would pick STORE_NAME in a class namespace.
                inner.emit(OpCode::LoadLocals, 0);
                let idx = inner.cell_or_free_index("__classdict__");
                inner.emit(OpCode::StoreDeref, idx);
            }
            // `__conditional_annotations__ = set()` (PEP 649).
            if has_conditional_annotations {
                inner.emit(OpCode::BuildSet, 0);
                let idx = inner.cell_or_free_index("__conditional_annotations__");
                inner.emit(OpCode::StoreDeref, idx);
            }
            inner.line_pinned = saved_pin;
            inner.pinned_colspan = saved_col;
        }

        // Under PEP 563 only (`codegen_body`): SETUP_ANNOTATIONS before
        // the first body statement when the class block contains an
        // annotated statement at its own level (CPython symtable
        // `ste_annotations_used`), so a read of `__annotations__`
        // preceding the first annotation sees the dict. It is emitted
        // *before* the docstring store, at the class-line prologue
        // location. Without the future import the annotations are
        // deferred into `__annotate_func__` at the end of the body.
        if future_annotations && block_has_annotations(body) {
            let saved_pin = inner.line_pinned;
            let saved_col = inner.pinned_colspan;
            inner.line_pinned = Some(firstlineno);
            inner.pinned_colspan = ColSpan {
                end_lineno: firstlineno,
                col: 0,
                end_col: 0,
            };
            inner.emit(OpCode::SetupAnnotations, 0);
            inner.line_pinned = saved_pin;
            inner.pinned_colspan = saved_col;
            inner.annotations_initialized = true;
        }

        // CPython stores a class body's leading string literal as
        // `__doc__` via a `STORE_NAME` at the top of the body. Mirror
        // that so `Cls.__doc__` is faithful (classes without a docstring
        // get `None` stamped by `__build_class__`). Unlike a function
        // body — where the docstring lives in `co_consts[0]` — a class
        // body reserves that slot for the qualname, so it must be an
        // explicit store rather than a constant-slot convention.
        let class_has_docstring = first_stmt_docstring(body).is_some();
        if let Some(doc) = first_stmt_docstring(body) {
            // `-OO` (optimize >= 2) strips class docstrings too.
            if self.params.optimize < 2 {
                let doc_const = inner
                    .co
                    .intern_constant(Constant::Str(clean_docstring(doc)));
                // `codegen_body`: the LOAD_CONST sits at the docstring
                // *expression*, so tracing a class body fires a `'line'`
                // event on the docstring line
                // (test_class_creation_with_docstrings); the STORE_NAME
                // is NO_LOCATION.
                let doc_span = match &body[0].kind {
                    StmtKind::Expr(e) => e.span,
                    _ => body[0].span,
                };
                inner.set_line_from(doc_span.start.0);
                inner.set_span(doc_span);
                inner.emit(OpCode::LoadConst, doc_const);
                let saved_pin = inner.line_pinned;
                inner.line_pinned = Some(0);
                inner.emit_store_name("__doc__");
                inner.line_pinned = saved_pin;
            }
        }

        // The docstring statement was consumed by the `__doc__` store
        // above; compiling it again would add a second traced NOP.
        let stmts = if class_has_docstring {
            &body[1..]
        } else {
            body
        };
        for (i, s) in stmts.iter().enumerate() {
            if type_params_marker == Some(i + usize::from(class_has_docstring)) {
                continue;
            }
            inner.compile_stmt(s)?;
        }

        // PEP 649 (`codegen_process_deferred_annotations`, the tail of
        // `codegen_body`): the simple-name annotations collected while
        // compiling the body become `__annotate_func__`, built and
        // stored at the class-line prologue location.
        if !future_annotations {
            let loc = AnnotateLoc {
                line: firstlineno,
                span: None,
            };
            if let Some(code) = inner.compile_deferred_annotations(loc)? {
                inner.apply_annotate_loc(loc);
                inner.emit_annotate_closure(code);
                inner.emit_store_name("__annotate_func__");
                inner.line_pinned = None;
            }
        }

        // The class-body tail (`__static_attributes__` store,
        // `__classcell__` store, implicit `return None`) is synthetic:
        // CPython emits it with NO_LOCATION so a body ending in a
        // branch join fires no extra `'line'` event and the `'return'`
        // event reports the path's own last line
        // (test_implicit_return_in_class). Pin line 0 through
        // `finish()`.
        inner.line_pinned = Some(0);

        // `__static_attributes__` (sorted names assigned through
        // `self.X` lexically inside the class) — stored after the body
        // runs, exactly where CPython 3.13's compiler emits it.
        // CPython's `compiler_maybe_add_static_attribute_to_class`
        // matches the literal name `self` in *any* unit nested under
        // the class scope (nested functions included; nested classes
        // collect their own), regardless of parameter names.
        {
            let mut attrs: HashSet<String> = HashSet::new();
            collect_self_attr_stores(unmangled_body, "self", &mut attrs);
            let mut attrs: Vec<String> = attrs.into_iter().collect();
            attrs.sort();
            let tup = Constant::Tuple(attrs.into_iter().map(Constant::Str).collect());
            let tup_const = inner.co.intern_constant(tup);
            let tup_name = inner.co.intern_name("__static_attributes__");
            inner.emit(OpCode::LoadConst, tup_const);
            inner.emit(OpCode::StoreName, tup_name);
        }

        // Expose the `__class__` cell via `__classcell__` so the
        // `__build_class__` builtin can patch it — only when a method
        // closed over it (see `needs_class_closure` above).
        // `__classdictcell__` first (CPython `type_new_set_classdictcell`
        // pops it and re-points the cell at the type's dict).
        if needs_classdict {
            let idx = inner.cell_or_free_index("__classdict__");
            inner.emit(OpCode::LoadClosure, idx);
            inner.emit_store_name("__classdictcell__");
        }
        if needs_class_closure {
            let class_cell_idx = inner.cell_or_free_index("__class__");
            inner.emit(OpCode::LoadClosure, class_cell_idx);
            inner.emit(OpCode::CopyTop, 1);
            inner.emit_store_name("__classcell__");
            inner.emit(OpCode::ReturnValue, 0);
        }

        let inner_code = inner.finish();
        let inner_freevars = inner_code.freevars.clone();

        for free in &inner_freevars {
            if matches!(self.bindings.get(free), Some(Binding::Local)) {
                self.bindings.insert(free.clone(), Binding::Cell);
                if !self.co.cellvars.contains(free) {
                    self.co.cellvars.push(free.clone());
                }
            }
        }

        let mut flags = 0u32;
        if !inner_freevars.is_empty() {
            for free in &inner_freevars {
                let idx = self.cell_or_free_index(free);
                self.emit(OpCode::LoadClosure, idx);
            }
            self.emit(OpCode::BuildTuple, inner_freevars.len() as u32);
            flags |= 0x08;
        }
        let code_idx = self
            .co
            .intern_constant(Constant::Code(std::sync::Arc::new(inner_code)));
        self.emit(OpCode::LoadConst, code_idx);
        self.emit_make_function(flags);
        Ok(())
    }

    /// Compile `try / except / else / finally`. The body is protected
    /// by an exception table entry; matched handlers fall through to
    /// the `else` branch, unmatched ones re-raise. `finally` runs on
    /// every exit path.
    /// Allocate a fresh, process-wide-unique id for a [`FinallyFrame`].
    fn fresh_finally_id(&mut self) -> u32 {
        self.next_finally_id += 1;
        self.next_finally_id
    }

    /// Push exception-table entries covering `[start, end)` → `handler`,
    /// but with the `finally`/`with`-exit inline copies that belong to
    /// frame `frame_id` punched out as "holes". A `return`/`break`/
    /// `continue` inlines its pending finally bodies at the exit site,
    /// physically *inside* the protected body; without the holes a
    /// `raise` from such an inlined finally would be re-caught here and
    /// run the finally a second time (CPython runs it exactly once,
    /// because its return-path finally sits outside the body's covered
    /// range). `frame_id == None` punches nothing.
    fn push_body_exc_entries(
        &mut self,
        start: u32,
        end: u32,
        handler: u32,
        depth: u32,
        push_lasti: bool,
        frame_id: Option<u32>,
    ) {
        let ids: Vec<u32> = frame_id.into_iter().collect();
        self.push_body_exc_entries_ids(start, end, handler, depth, push_lasti, &ids);
    }

    /// [`Self::push_body_exc_entries`] with holes punched for *several*
    /// frames' inline copies (an except-region's segments must exclude
    /// every clause's return-path unwind run).
    fn push_body_exc_entries_ids(
        &mut self,
        start: u32,
        end: u32,
        handler: u32,
        depth: u32,
        push_lasti: bool,
        frame_ids: &[u32],
    ) {
        if end <= start {
            return;
        }
        // RFC 0068 WS1: with CPython's on-stack exception discipline
        // (PUSH_EXC_INFO's saved previous exception and lasti offsets
        // occupy real stack slots) hand-maintained depth counts no
        // longer track the true entry depth in nested handler contexts.
        // Record the sentinel instead; `finish` resolves each entry to
        // the static stack depth at its protected range's start — which
        // is exactly CPython's handler depth. Callers whose range starts
        // above its own baseline pass an *anchored* depth
        // (`HANDLER_DEPTH_ANCHOR_FLAG | insn`), kept as-is.
        let depth = if depth & HANDLER_DEPTH_ANCHOR_FLAG != 0 && depth != HANDLER_DEPTH_SENTINEL {
            depth
        } else {
            HANDLER_DEPTH_SENTINEL
        };
        let mut holes: Vec<(u32, u32)> = self
            .finally_holes
            .iter()
            .filter(|(id, hs, he)| frame_ids.contains(id) && *hs < end && *he > start)
            .map(|(_, hs, he)| ((*hs).max(start), (*he).min(end)))
            .collect();
        holes.sort_by_key(|(hs, _)| *hs);
        let mut cur = start;
        for (hs, he) in holes {
            if hs > cur {
                self.co.exception_table.push(ExcHandler {
                    start: cur,
                    end: hs,
                    handler,
                    depth,
                    push_lasti,
                });
            }
            cur = cur.max(he);
        }
        if cur < end {
            self.co.exception_table.push(ExcHandler {
                start: cur,
                end,
                handler,
                depth,
                push_lasti,
            });
        }
    }

    /// Fresh push-order stamp for an unwindable block.
    fn next_fblock_seq(&mut self) -> u32 {
        self.fblock_seq += 1;
        self.fblock_seq
    }

    /// Emit at CPython's unwind location `*ploc` (`None` is
    /// `NO_LOCATION`).
    fn emit_at(&mut self, ploc: Ploc, op: OpCode, arg: u32) -> u32 {
        match ploc {
            None => self.emit_no_line(op, arg),
            Some(span) => {
                self.current_span = span;
                self.emit(op, arg)
            }
        }
    }

    /// `POP_BLOCK` pseudo-op at `*ploc`.
    fn emit_pop_block_at(&mut self, ploc: Ploc) -> u32 {
        match ploc {
            None => self.emit_pop_block_no_line(),
            Some(span) => {
                self.current_span = span;
                self.emit_pop_block()
            }
        }
    }

    /// CPython `codegen_unwind_fblock_stack` for a `return`: unwind
    /// every live block innermost-out (finally clauses, `with` exits,
    /// `except`-clause cleanups, exception-path finally copies,
    /// `for`-loop iterators, pending return values of enclosing
    /// return-path inlines), a non-constant return value riding TOS
    /// throughout (`preserve_tos`). Returns the location the unwind
    /// leaves in `*ploc` and the coverage holes it opened (closed by
    /// the caller with [`Self::close_unwind_holes`] after the
    /// `RETURN_VALUE`).
    fn unwind_for_return(
        &mut self,
        ploc: Ploc,
        preserve_tos: bool,
    ) -> Result<(Ploc, Vec<(u32, u32)>), CompileError> {
        let floor = UnwindFloor {
            exc: 0,
            loops: 0,
            rv: 0,
            frames: 0,
        };
        self.unwind_fblocks(ploc, preserve_tos, floor)
    }

    /// `codegen_unwind_fblock_stack` for a `break`/`continue`: unwind
    /// everything pushed inside the innermost loop, stopping at the
    /// loop itself (the caller pops a `for` iterator for `break`).
    fn unwind_for_loop_exit(
        &mut self,
        ploc: Ploc,
    ) -> Result<(Ploc, Vec<(u32, u32)>), CompileError> {
        let loop_depth = self.loop_stack.len();
        let top = self.loop_stack.last().expect("loop frame");
        let frames = self
            .finally_stack
            .iter()
            .position(|f| f.loop_depth_at_push >= loop_depth)
            .unwrap_or(self.finally_stack.len());
        let floor = UnwindFloor {
            exc: top.exc_on_stack_at_entry,
            loops: loop_depth,
            rv: top.pending_retvals_at_entry,
            frames,
        };
        self.unwind_fblocks(ploc, false, floor)
    }

    /// Replay CPython's fblock-stack unwind over WeavePy's per-kind
    /// stacks: repeatedly pick the most recently pushed live block
    /// above `floor` and emit its `codegen_unwind_fblock` shape at
    /// `*ploc`, threading `ploc` exactly as CPython does (a `with`
    /// exit moves it to the `with` statement, a finally body to
    /// `NO_LOCATION`). Every block whose coverage ends at its
    /// unwind opens a hole `(id, start)` that the caller closes once
    /// the trailing return/jump is emitted.
    fn unwind_fblocks(
        &mut self,
        mut ploc: Ploc,
        preserve_tos: bool,
        floor: UnwindFloor,
    ) -> Result<(Ploc, Vec<(u32, u32)>), CompileError> {
        #[derive(Clone, Copy)]
        enum Item {
            Frame,
            ExcRegion,
            Loop,
            RetVal,
        }
        let saved = std::mem::take(&mut self.finally_stack);
        let mut next_frame = saved.len();
        let mut cur_exc = self.exc_on_stack;
        let mut cur_loops = self.loop_stack.len();
        let mut cur_rv = self.pending_retvals;
        let mut holes: Vec<(u32, u32)> = Vec::new();
        let mut result: Result<(), CompileError> = Ok(());
        loop {
            let mut best: Option<(u32, Item)> = None;
            let mut consider = |seq: u32, item: Item| {
                if best.is_none_or(|(s, _)| seq > s) {
                    best = Some((seq, item));
                }
            };
            if next_frame > floor.frames {
                consider(saved[next_frame - 1].seq, Item::Frame);
            }
            if cur_exc > floor.exc {
                consider(self.exc_region_ids[cur_exc as usize - 1].1, Item::ExcRegion);
            }
            if cur_loops > floor.loops {
                consider(self.loop_stack[cur_loops - 1].seq, Item::Loop);
            }
            if cur_rv > floor.rv {
                consider(self.rv_seqs[cur_rv as usize - 1], Item::RetVal);
            }
            let Some((_, item)) = best else { break };
            match item {
                Item::Loop => {
                    // FOR_LOOP: pop the iterator (WHILE_LOOP: nothing).
                    cur_loops -= 1;
                    if self.loop_stack[cur_loops].is_for_loop {
                        if preserve_tos {
                            self.emit_at(ploc, OpCode::Swap, 2);
                        }
                        self.emit_at(ploc, OpCode::PopTop, 0);
                    }
                }
                Item::ExcRegion => {
                    // FINALLY_END: [prev, exc] under a pending value —
                    // drop the exception, pop the region's cleanup
                    // block, restore the previous exc_info.
                    if preserve_tos {
                        self.emit_at(ploc, OpCode::Swap, 2);
                    }
                    self.emit_at(ploc, OpCode::PopTop, 0);
                    if preserve_tos {
                        self.emit_at(ploc, OpCode::Swap, 2);
                    }
                    self.emit_pop_block_at(ploc);
                    cur_exc -= 1;
                    holes.push((self.exc_region_ids[cur_exc as usize].0, self.next_offset()));
                    self.emit_at(ploc, OpCode::PopExcept, 0);
                }
                Item::RetVal => {
                    // POP_VALUE: an enclosing return's pending value.
                    if preserve_tos {
                        self.emit_at(ploc, OpCode::Swap, 2);
                    }
                    self.emit_at(ploc, OpCode::PopTop, 0);
                    cur_rv -= 1;
                }
                Item::Frame => {
                    next_frame -= 1;
                    let frame = &saved[next_frame];
                    // While compiling this frame's body, hide it (and
                    // everything inside it) so nested unwinds don't
                    // re-inline it.
                    self.finally_stack = saved
                        .iter()
                        .take(next_frame)
                        .map(clone_finally_frame)
                        .collect();
                    match self.emit_unwind_frame(frame, &mut ploc, preserve_tos, &mut holes) {
                        Ok(()) => {}
                        Err(e) => {
                            result = Err(e);
                            break;
                        }
                    }
                }
            }
        }
        self.finally_stack = saved;
        result?;
        Ok((ploc, holes))
    }

    /// Close the coverage holes opened by an unwind at the current
    /// offset (just past the `RETURN_VALUE` or the `break`/`continue`
    /// jump: CPython's fblock stack has already been popped when those
    /// are emitted, so they belong to the enclosing coverage).
    fn close_unwind_holes(&mut self, holes: Vec<(u32, u32)>) {
        let end = self.next_offset();
        for (id, start) in holes {
            self.finally_holes.push((id, start, end));
        }
    }

    /// `codegen_unwind_fblock` for one `FinallyFrame`.
    fn emit_unwind_frame(
        &mut self,
        frame: &FinallyFrame,
        ploc: &mut Ploc,
        preserve_tos: bool,
        holes: &mut Vec<(u32, u32)>,
    ) -> Result<(), CompileError> {
        match &frame.kind {
            FinallyKind::Stmts(body) if frame.pop_except_after => {
                // HANDLER_CLEANUP: `POP_BLOCK` (named clause: the
                // unbind guard), `[SWAP 2]`, `POP_BLOCK` (the region
                // cleanup), `POP_EXCEPT`, then `e = None; del e` — all
                // at `*ploc`. The swap sits between the two pops, so
                // it is covered by the region cleanup but not by the
                // unbind guard; nothing from the POP_EXCEPT on is.
                let named = !body.is_empty();
                if named {
                    self.emit_pop_block_at(*ploc);
                }
                let unbind_hole = self.next_offset();
                if preserve_tos {
                    self.emit_at(*ploc, OpCode::Swap, 2);
                }
                self.emit_pop_block_at(*ploc);
                if frame.region_hole_id != 0 {
                    holes.push((frame.region_hole_id, self.next_offset()));
                }
                self.emit_at(*ploc, OpCode::PopExcept, 0);
                holes.push((frame.id, unbind_hole));
                if named {
                    let saved_pin = self.line_pinned;
                    let saved_col = self.pinned_colspan;
                    let saved_line = self.current_line;
                    let saved_span = self.current_span;
                    let (line, col) = match *ploc {
                        None => (0, ColSpan::default()),
                        Some(span) => {
                            self.current_span = span;
                            (self.current_location_line(), self.resolve_colspan())
                        }
                    };
                    self.line_pinned = Some(line);
                    self.pinned_colspan = col;
                    let r = body.iter().try_for_each(|s| self.compile_stmt(s));
                    self.line_pinned = saved_pin;
                    self.pinned_colspan = saved_col;
                    self.current_line = saved_line;
                    self.current_span = saved_span;
                    r?;
                }
                Ok(())
            }
            FinallyKind::Stmts(body) => {
                // FINALLY_TRY: `POP_BLOCK` at the unwinding statement,
                // the finally body at its own locations (a pending
                // return value rides above it as a POP_VALUE block),
                // and the unwind continues with `NO_LOCATION`.
                holes.push((frame.id, self.next_offset()));
                self.emit_pop_block_at(*ploc);
                if preserve_tos {
                    self.pending_retvals += 1;
                    let seq = self.next_fblock_seq();
                    self.rv_seqs.push(seq);
                }
                let r = body.iter().try_for_each(|s| self.compile_stmt(s));
                if preserve_tos {
                    self.rv_seqs.pop();
                    self.pending_retvals -= 1;
                }
                r?;
                *ploc = None;
                Ok(())
            }
            FinallyKind::WithExit { line, span } => {
                // WITH: `*ploc` becomes the `with` statement; `POP_BLOCK`,
                // `[SWAP 3; SWAP 2]`, `__exit__(None, None, None)`
                // (the pair pushed by the LOAD_SPECIAL dance is on the
                // operand stack), `POP_TOP`.
                *ploc = Some(*span);
                self.current_line = *line;
                self.current_span = *span;
                holes.push((frame.id, self.next_offset()));
                self.emit_pop_block();
                if preserve_tos {
                    self.emit(OpCode::Swap, 3);
                    self.emit(OpCode::Swap, 2);
                }
                self.emit_call_exit_with_nones();
                self.emit(OpCode::PopTop, 0);
                // "The exit block should appear to execute after the
                // statement causing the unwinding": the rest of the
                // unwind is artificial.
                *ploc = None;
                Ok(())
            }
            FinallyKind::TryExcept => {
                holes.push((frame.id, self.next_offset()));
                self.emit_pop_block_at(*ploc);
                Ok(())
            }
            FinallyKind::AsyncWithExit { line, span } => {
                // ASYNC_WITH: as WITH, awaiting the `__aexit__` result.
                *ploc = Some(*span);
                self.current_line = *line;
                self.current_span = *span;
                holes.push((frame.id, self.next_offset()));
                self.emit_pop_block();
                if preserve_tos {
                    self.emit(OpCode::Swap, 3);
                    self.emit(OpCode::Swap, 2);
                }
                self.emit_call_exit_with_nones();
                self.compile_await_dance(2);
                self.emit(OpCode::PopTop, 0);
                *ploc = None;
                Ok(())
            }
        }
    }

    /// The implicit `e = None; del e` CPython runs when an
    /// `except E as e:` block exits by *any* path — fallthrough,
    /// `return`/`break`/`continue`, or a propagating exception. The
    /// assignment-first shape means the delete can never raise (the
    /// body may itself have `del e`'d), and synthesizing AST keeps
    /// name-scoping decisions (fast/global/cell) in one place.
    fn except_unbind_stmts(name: &str, span: weavepy_lexer::Span) -> Vec<Stmt> {
        let name_expr = |kind_span| Expr {
            kind: ExprKind::Name(name.to_owned()),
            span: kind_span,
        };
        vec![
            Stmt {
                kind: StmtKind::Assign {
                    targets: vec![name_expr(span)],
                    value: Expr {
                        kind: ExprKind::Constant(AstConstant::None),
                        span,
                    },
                },
                span,
            },
            Stmt {
                kind: StmtKind::Delete(vec![name_expr(span)]),
                span,
            },
        ]
    }

    fn compile_try(
        &mut self,
        body: &[Stmt],
        handlers: &[ExceptHandler],
        orelse: &[Stmt],
        finalbody: &[Stmt],
    ) -> Result<(), CompileError> {
        let has_handlers = !handlers.is_empty();
        let has_finally = !finalbody.is_empty();
        if !has_handlers && !has_finally {
            for s in body {
                self.compile_stmt(s)?;
            }
            return Ok(());
        }
        // The `SETUP_FINALLY` pseudo-ops carry the statement's location
        // (PEP 626: the `try:` line is "executed"; the flowgraph keeps
        // one located NOP for it). `codegen_try_finally` wraps the
        // whole try/except part in its own `SETUP_FINALLY end`, then
        // `codegen_try_except` opens `SETUP_FINALLY except`; each is
        // followed by its `body` label.
        let try_stmt_span = self.current_span;
        // PEP 654 static check, before anything else compiles so the
        // `except*` jump error wins over e.g. a `return` in a module-
        // level `finally` (matching CPython's reporting order).
        if handlers.iter().any(|h| h.is_star) {
            for h in handlers {
                Self::validate_star_clause_jumps(&h.body, false)?;
            }
        }
        // Approximate stack depth at handler entry. The dispatch
        // loop truncates everything above `depth`, so we need to
        // preserve any state the surrounding control-flow stitched
        // into the stack — iterators kept live across `for` loop
        // iterations, and any propagating exception a surrounding
        // `finally` keeps on the stack for its trailing RERAISE.
        let body_depth =
            self.loop_stack.iter().filter(|fr| fr.is_for_loop).count() as u32 + self.exc_on_stack;
        // CPython nests `try/except/else/finally` as a try/finally
        // whose body is the whole try/except part (compiler_try_finally
        // → compiler_try_except): one shared normal finally copy after
        // the try/except region, one exceptional copy at the end. The
        // frame makes the finally body visible to `return`/`break`/
        // `continue` nested anywhere inside body/orelse/handlers.
        let wrap_frame_id = if has_finally {
            let id = self.fresh_finally_id();
            let seq = self.next_fblock_seq();
            self.finally_stack.push(FinallyFrame {
                kind: FinallyKind::Stmts(finalbody.to_vec()),
                loop_depth_at_push: self.loop_stack.len(),
                id,
                pop_except_after: false,
                region_hole_id: 0,
                exc_at_push: self.exc_on_stack,
                handler_at_push: self.handler_depth,
                rv_at_push: self.pending_retvals,
                seq,
            });
            Some(id)
        } else {
            None
        };
        // CPython's `TRY_EXCEPT` fblock around the body: a `return`/
        // `break`/`continue` leaving it emits `POP_BLOCK`, and the body's
        // coverage is punched from there.
        let body_frame_id = if has_handlers {
            let id = self.fresh_finally_id();
            let seq = self.next_fblock_seq();
            self.finally_stack.push(FinallyFrame {
                kind: FinallyKind::TryExcept,
                loop_depth_at_push: self.loop_stack.len(),
                id,
                pop_except_after: false,
                region_hole_id: 0,
                exc_at_push: self.exc_on_stack,
                handler_at_push: self.handler_depth,
                rv_at_push: self.pending_retvals,
                seq,
            });
            Some(id)
        } else {
            None
        };
        let wrap_start = if has_finally {
            self.current_span = try_stmt_span;
            self.emit_setup(OpCode::SetupFinally);
            self.use_label()
        } else {
            self.next_offset()
        };
        let body_start = if has_handlers {
            self.current_span = try_stmt_span;
            self.emit_setup(OpCode::SetupFinally);
            self.use_label()
        } else {
            self.next_offset()
        };
        for s in body {
            self.compile_stmt(s)?;
        }
        let body_end = self.next_offset();
        if has_handlers {
            self.finally_stack.pop();
        }
        let mut normal_skip = None;
        if has_handlers {
            // `POP_BLOCK` closes the body's coverage; the else clause
            // runs only on normal body completion, inline right after
            // the body and *outside* the handled range (CPython
            // compiles it before the handlers): an exception raised in
            // `else` does not reach this statement's own `except`
            // clauses — only an enclosing `finally`.
            self.emit_pop_block_no_line();
            for s in orelse {
                self.compile_stmt(s)?;
            }
            // Normal-exit hop over the handler region: CPython's
            // NO_LOCATION JUMP_NO_INTERRUPT to `end` (backward — and
            // thus visible as JUMP_BACKWARD_NO_INTERRUPT — once the
            // cold-block pass moves the handlers to the stream end).
            let j = self.emit_no_line(OpCode::JumpForward, 0);
            self.no_interrupt_jumps.insert(j);
            normal_skip = Some(j);
        }

        // Handlers begin here (reachable only via exception edges; the
        // cold-block pass relocates them, as CPython's flowgraph does).
        // The `except` label heads a `SETUP_CLEANUP cleanup`; the
        // region's own cleanup coverage opens behind it.
        let handlers_start = self.next_offset();
        let region_start = if has_handlers {
            self.emit_setup_no_line(OpCode::SetupCleanup);
            self.next_offset()
        } else {
            handlers_start
        };
        let is_star_try = handlers.iter().any(|h| h.is_star);
        if has_handlers && is_star_try {
            // PEP 654 / RFC 0018: `except*` lowering, mirroring
            // CPython's `compiler_try_star_except`:
            // - each clause consumes a sub-group of the caught exception;
            // - exceptions raised by clause *bodies* don't propagate
            //   immediately — they're collected, and after all clauses
            //   ran they are combined with the unmatched remainder via
            //   `PREP_RERAISE_STAR` (so `raise X` inside one clause
            //   still lets the other clauses run, and the final
            //   exception groups everything that's still alive);
            // - inside a clause body the *matched* sub-group is the
            //   active exception (`sys.exc_info()`, bare `raise`).
            self.push_body_exc_entries(
                body_start,
                body_end,
                handlers_start,
                body_depth,
                false,
                body_frame_id,
            );
            // Back-patched to the pc past the handler region (see the
            // non-`except*` branch for the rationale). No location, as
            // in the non-star branch.
            let push_exc_site = self.emit_no_line(OpCode::PushExcInfo, 0);
            // CPython `compiler_try_star_except` keeps the whole group
            // state on the operand stack — during clause bodies it is
            // `[prev, orig, res, rest]` (saved previous exc_info, the
            // original group, the raised/reraised list, the running
            // remainder). No synthetic locals: consuming the values in
            // PREP_RERAISE_STAR releases the group by refcount the
            // moment the statement ends (RFC 0054 taskgroups refcycle
            // timing) and the wire shape matches CPython's exactly
            // (test_dis, test_monitoring branch/jump offsets).
            let n_handlers = handlers.len();
            // Jumps into the PREP_RERAISE_STAR epilogue (each exit path
            // of the *last* clause inlines its own `LIST_APPEND 1` +
            // jump, as CPython's flowgraph small-block inlining does).
            let mut reraise_star_jumps: Vec<u32> = Vec::new();
            // Jumps to the next clause's match sequence.
            let mut next_clause_jumps: Vec<u32> = Vec::new();
            for (i, h) in handlers.iter().enumerate() {
                let clause_start = self.next_offset();
                for site in next_clause_jumps.drain(..) {
                    self.patch_jump(site, clause_start);
                }
                // The whole match sequence carries the clause's own
                // location (CPython locates BUILD_LIST/COPY/CHECK_EG_MATCH
                // on the `except*` clause), so entering the handler fires
                // exactly one `'line'` event there
                // (test_sys_settrace test_try_except_star_exception_caught).
                self.set_span(h.span);
                self.set_line_from(h.span.start.0);
                if i == 0 {
                    // [prev, exc] → [prev, orig, res, rest]
                    self.emit(OpCode::BuildList, 0);
                    self.emit(OpCode::CopyTop, 2);
                }
                let ty = h
                    .type_
                    .as_ref()
                    .expect("except* requires a type expression — parser must reject bare except*");
                self.compile_expr(ty)?;
                // [prev, orig, res, rest, type]. CPython locates
                // CHECK_EG_MATCH on the whole clause; the implicit
                // wrapper around a naked exception gets its traceback
                // entry here (gh-128799).
                self.set_span(h.span);
                self.set_line_from(h.span.start.0);
                self.emit(OpCode::CheckEGMatch, 0);
                // [prev, orig, res, rest, match?]
                self.emit(OpCode::CopyTop, 1);
                let no_match = self.emit(OpCode::PopJumpIfNone, 0);
                // Matched (not None): bind or discard. CHECK_EG_MATCH
                // already made it the active exception for the clause
                // body (`sys.exc_info()`, bare `raise` context).
                if let Some(n) = &h.name {
                    let name_expr = Expr {
                        kind: ExprKind::Name(n.clone()),
                        span: h.span,
                    };
                    self.compile_assign(&name_expr)?;
                } else {
                    self.emit(OpCode::PopTop, 0);
                }
                // [prev, orig, res, rest]. `SETUP_CLEANUP cleanup_end` at
                // the clause's location, then the `cleanup_body` label.
                self.set_span(h.span);
                self.emit_setup(OpCode::SetupCleanup);
                let clause_body_start = self.use_label();
                // `e` is unbound on every exit from the block (CPython
                // behaviour); `break`/`continue`/`return` cannot leave
                // an `except*` block at all (PEP 654), enforced by
                // `validate_star_clause_jumps`.
                let unbind_stmts = h
                    .name
                    .as_deref()
                    .map(|n| Self::except_unbind_stmts(n, h.span));
                if let Some(stmts) = &unbind_stmts {
                    let id = self.fresh_finally_id();
                    let seq = self.next_fblock_seq();
                    self.finally_stack.push(FinallyFrame {
                        kind: FinallyKind::Stmts(stmts.clone()),
                        loop_depth_at_push: self.loop_stack.len(),
                        id,
                        pop_except_after: false,
                        region_hole_id: 0,
                        exc_at_push: self.exc_on_stack,
                        handler_at_push: self.handler_depth,
                        rv_at_push: self.pending_retvals,
                        seq,
                    });
                }
                for s in &h.body {
                    self.compile_stmt(s)?;
                }
                if unbind_stmts.is_some() {
                    self.finally_stack.pop();
                }
                let clause_body_end = self.next_offset();
                self.emit_pop_block_no_line();
                // Fallthrough exit: the unbind (`e = None; del e`) and
                // the inlined `LIST_APPEND 1` + hop carry the clause
                // body's last line — CPython emits them NO_LOCATION and
                // its flowgraph's `propagate_line_numbers` copies the
                // preceding location, so no fresh `'line'` event fires
                // on normal clause exit (and the JUMP event reports the
                // body line: test_monitoring test_except_star). The
                // propagated location must be computed the way CPython
                // does: nothing flows into a jump target (a clause body
                // ending in a nested `try` exits by *jumping* here, so
                // the tail stays location-free — the nested
                // test_sys_settrace test_try_except_star_nested shape),
                // and the walk back stops at block boundaries.
                let (pin_line, pin_col) = {
                    let tgt = clause_body_end;
                    let is_jump_target = |k: u32| -> bool {
                        self.co.instructions.iter().enumerate().any(|(j, ins)| {
                            let from = j as u32 + 1;
                            match ins.op {
                                OpCode::JumpForward
                                | OpCode::PopJumpIfFalse
                                | OpCode::PopJumpIfTrue
                                | OpCode::PopJumpIfNone
                                | OpCode::PopJumpIfNotNone
                                | OpCode::ForIter
                                | OpCode::Send => from + ins.arg == k,
                                OpCode::JumpBackward => from.saturating_sub(ins.arg) == k,
                                _ => false,
                            }
                        })
                    };
                    let mut line = 0u32;
                    let mut col = ColSpan::default();
                    if !is_jump_target(tgt) {
                        let mut k = clause_body_end;
                        while k > clause_body_start {
                            let k1 = (k - 1) as usize;
                            if matches!(
                                self.co.instructions[k1].op,
                                OpCode::JumpForward
                                    | OpCode::JumpBackward
                                    | OpCode::ReturnValue
                                    | OpCode::RaiseVarargs
                                    | OpCode::Reraise
                            ) {
                                break;
                            }
                            if self.co.linetable[k1] != 0
                                && self.co.linetable[k1] != NEXT_LOCATION_LINE
                            {
                                line = self.co.linetable[k1];
                                col = self.co.coltable[k1];
                                break;
                            }
                            if is_jump_target(k1 as u32) {
                                break;
                            }
                            k -= 1;
                        }
                    }
                    (line, col)
                };
                {
                    let saved_line = self.current_line;
                    let saved_span = self.current_span;
                    let saved_pin = self.line_pinned;
                    let saved_col = self.pinned_colspan;
                    self.line_pinned = Some(pin_line);
                    self.pinned_colspan = pin_col;
                    if let Some(stmts) = &unbind_stmts {
                        for s in stmts {
                            self.compile_stmt(s)?;
                        }
                    }
                    if i == n_handlers - 1 {
                        // [prev, orig, res, rest] → [prev, orig, res]
                        self.emit(OpCode::ListAppend, 1);
                        let j = self.emit(OpCode::JumpForward, 0);
                        self.no_interrupt_jumps.insert(j);
                        reraise_star_jumps.push(j);
                    } else {
                        let j = self.emit(OpCode::JumpForward, 0);
                        self.no_interrupt_jumps.insert(j);
                        next_clause_jumps.push(j);
                    }
                    self.line_pinned = saved_pin;
                    self.pinned_colspan = saved_col;
                    self.current_line = saved_line;
                    self.current_span = saved_span;
                }
                // Collector: an exception raised by the clause body
                // lands here with `[prev, orig, res, rest, lasti, exc]`
                // (lasti-flagged entry; the raise chained `__context__`
                // to the matched group already). Unbind, add it to the
                // res list, drop the lasti, and run the next clause —
                // all NO_LOCATION (CPython's cleanup_end block).
                let collector = self.next_offset();
                self.co.exception_table.push(ExcHandler {
                    start: clause_body_start,
                    end: clause_body_end,
                    handler: collector,
                    depth: HANDLER_DEPTH_SENTINEL,
                    push_lasti: true,
                });
                if let Some(stmts) = &unbind_stmts {
                    let saved_pin = self.line_pinned;
                    self.line_pinned = Some(0);
                    for s in stmts {
                        self.compile_stmt(s)?;
                    }
                    self.line_pinned = saved_pin;
                }
                self.emit_no_line(OpCode::ListAppend, 3);
                // [prev, orig, res, rest, lasti]
                self.emit_no_line(OpCode::PopTop, 0);
                // [prev, orig, res, rest]
                if i == n_handlers - 1 {
                    self.emit_no_line(OpCode::ListAppend, 1);
                    let j = self.emit_no_line(OpCode::JumpForward, 0);
                    self.no_interrupt_jumps.insert(j);
                    reraise_star_jumps.push(j);
                } else {
                    let j = self.emit_no_line(OpCode::JumpForward, 0);
                    self.no_interrupt_jumps.insert(j);
                    next_clause_jumps.push(j);
                }
                // No-match path: discard the copied None. Carries the
                // clause's location (CPython) — already traced when the
                // match check ran, so no fresh `'line'` event fires.
                let no_match_target = self.next_offset();
                self.patch_jump(no_match, no_match_target);
                self.set_span(h.span);
                self.set_line_from(h.span.start.0);
                self.emit(OpCode::PopTop, 0);
                // [prev, orig, res, rest]; the last clause appends the
                // unhandled remainder and falls into the epilogue, the
                // others fall into the next clause's match sequence.
                if i == n_handlers - 1 {
                    self.emit(OpCode::ListAppend, 1);
                }
            }
            // PREP_RERAISE_STAR epilogue — entirely NO_LOCATION in
            // CPython (dis shows `--`): reaching it must not fire a
            // `'line'` event, whatever line the last clause body left
            // behind (test_try_except_star_nested).
            let reraise_star = self.next_offset();
            for site in reraise_star_jumps {
                self.patch_jump(site, reraise_star);
            }
            // [prev, orig, res] → [prev, result] (None when everything
            // was handled).
            self.emit_no_line(OpCode::PrepReraiseStar, 0);
            self.emit_no_line(OpCode::CopyTop, 1);
            let rer = self.emit_no_line(OpCode::PopJumpIfNotNone, 0);
            // Nothing to re-raise: [prev, None] → drop both, restore
            // the previous exc_info.
            self.emit_no_line(OpCode::PopTop, 0);
            // The machinery's own cleanup coverage (CPython's
            // SETUP_CLEANUP around the whole except* region) ends here:
            // both exit runs below pop the exc_info explicitly, so a
            // RERAISE from them must propagate to the *enclosing*
            // handler, not bounce back into this region's cleanup.
            let coverage_end = self.next_offset();
            self.emit_no_line(OpCode::PopExcept, 0);
            let exit = self.emit_no_line(OpCode::JumpForward, 0);
            self.no_interrupt_jumps.insert(exit);
            // Re-raise path (CPython: POP_BLOCK; SWAP 2; POP_EXCEPT;
            // RERAISE 0): restore the previous exc_info first, then
            // re-raise without recording the re-raise site and without
            // re-chaining `__context__`.
            let reraise = self.next_offset();
            self.patch_jump(rer, reraise);
            self.emit_no_line(OpCode::Swap, 2);
            self.emit_no_line(OpCode::PopExcept, 0);
            self.emit_no_line(OpCode::Reraise, 0);
            // Cleanup tail for exceptions escaping the machinery itself
            // (match evaluation, PREP_RERAISE_STAR, …): restore the
            // previous exc_info and re-raise at the original site —
            // CPython's POP_EXCEPT_AND_RERAISE behind the region-wide
            // SETUP_CLEANUP. Clause bodies keep their own (narrower)
            // collector coverage; the innermost-wins partition in
            // `finish()` resolves the overlap.
            let cleanup_start = self.next_offset();
            self.emit_no_line(OpCode::CopyTop, 3);
            self.emit_no_line(OpCode::PopExcept, 0);
            self.emit_no_line(OpCode::Reraise, 1);
            self.co.exception_table.push(ExcHandler {
                start: region_start,
                end: coverage_end,
                handler: cleanup_start,
                depth: HANDLER_DEPTH_ANCHOR_FLAG | region_start,
                push_lasti: true,
            });
            let end = self.next_offset();
            self.patch_jump(exit, end);
            // Record the handler-body end on PUSH_EXC_INFO (see below).
            self.co.instructions[push_exc_site as usize].arg = end;
        } else if has_handlers {
            self.push_body_exc_entries(
                body_start,
                body_end,
                handlers_start,
                body_depth,
                false,
                body_frame_id,
            );
            // The arg is back-patched below to the pc just past this
            // handler region; the VM tags the active-handler entry with
            // it so an exception escaping the handler to an enclosing
            // `try` correctly unwinds `sys.exc_info()` (see
            // `Interpreter::handle_exception`). No location (CPython) —
            // entering the handler must not fire a `'line'` event of
            // its own; the first clause-check instruction does.
            let push_exc_site = self.emit_no_line(OpCode::PushExcInfo, 0);
            // Stack on entry: [prev, exc] (dispatch pushed exc,
            // PUSH_EXC_INFO slid prev underneath).
            let mut next_handler_sites: Vec<u32> = Vec::new();
            let mut handler_exit_jumps: Vec<u32> = Vec::new();
            // Except-region coverage → the COPY 3 cleanup tail, in
            // *segments*: CPython's SETUP_CLEANUP coverage is off
            // during each clause's exit sequence (POP_EXCEPT/unbind/
            // JUMP), so those slices stay uncovered (or fall to an
            // enclosing finally via the wrapper's whole-range entry
            // and the innermost-wins partition).
            let mut segments: Vec<(u32, u32)> = Vec::new();
            // Every clause's own finally frame id: their return-path
            // inline runs must be punched out of the segments.
            let mut clause_frame_ids: Vec<u32> = Vec::new();
            let mut seg_start = region_start;
            for (i, h) in handlers.iter().enumerate() {
                // Patch the previous handler's "no-match" branch.
                if i > 0 {
                    let prev = next_handler_sites.pop();
                    if let Some(site) = prev {
                        let cur = self.next_offset();
                        self.patch_jump(site, cur);
                    }
                }
                // The whole clause-check sequence is located on the
                // `except E:` clause (CPython puts CHECK_EXC_MATCH and
                // friends at the handler location), so entering the
                // handler fires exactly one `'line'` event there.
                self.set_line_from(h.span.start.0);
                self.set_span(h.span);
                match &h.type_ {
                    Some(t) => {
                        // Stack: [exc] → [exc, type] → [exc, bool]
                        // (CHECK_EXC_MATCH peeks the exception).
                        self.compile_expr(t)?;
                        self.set_line_from(h.span.start.0);
                        self.set_span(h.span);
                        self.emit(OpCode::CheckExcMatch, 0);
                        let no_match = self.emit(OpCode::PopJumpIfFalse, 0);
                        next_handler_sites.push(no_match);
                        // Matched: Stack still [exc]. Bind or discard.
                        if let Some(n) = &h.name {
                            // `codegen_nameop(c, LOC(handler), name, Store)`:
                            // the bind carries the clause's location.
                            let name_expr = Expr {
                                kind: ExprKind::Name(n.clone()),
                                span: h.span,
                            };
                            self.compile_assign(&name_expr)?;
                            // `SETUP_CLEANUP cleanup_end` at the clause's
                            // location guards the body for the unbind.
                            self.set_span(h.span);
                            self.emit_setup(OpCode::SetupCleanup);
                        } else {
                            self.emit(OpCode::PopTop, 0);
                        }
                    }
                    None => {
                        // Bare `except:` matches anything; just discard exc.
                        self.emit(OpCode::PopTop, 0);
                    }
                }
                // `cleanup_body` label.
                self.use_label();
                // `except E as e:` unbinds `e` on every exit from the
                // clause body (CPython wraps the body in
                // `try: … finally: e = None; del e`). The finally-stack
                // frame covers `return`/`break`/`continue`; the inline
                // copy below covers fallthrough; the exception-table
                // entry further below covers a propagating exception.
                let unbind_stmts = h
                    .name
                    .as_deref()
                    .map(|n| Self::except_unbind_stmts(n, h.span));
                // Always push a handler-exit frame — carrying the unbind
                // stmts when the clause binds a name, empty for a bare
                // `except:` — flagged `pop_except_after` so a `return`
                // leaving the handler body emits `POP_EXCEPT` right after
                // the inlined unbind (CPython's return-path order:
                // `e = None; del e; POP_EXCEPT; RETURN_VALUE`).
                let unbind_frame_id: Option<u32>;
                {
                    let id = self.fresh_finally_id();
                    let region_hole_id = self.fresh_finally_id();
                    unbind_frame_id = Some(id);
                    clause_frame_ids.push(region_hole_id);
                    let seq = self.next_fblock_seq();
                    self.finally_stack.push(FinallyFrame {
                        kind: FinallyKind::Stmts(unbind_stmts.clone().unwrap_or_default()),
                        loop_depth_at_push: self.loop_stack.len(),
                        id,
                        pop_except_after: true,
                        region_hole_id,
                        exc_at_push: self.exc_on_stack,
                        handler_at_push: self.handler_depth,
                        rv_at_push: self.pending_retvals,
                        seq,
                    });
                }
                let hbody_start = self.next_offset();
                self.handler_depth += 1;
                for s in &h.body {
                    self.compile_stmt(s)?;
                }
                self.handler_depth -= 1;
                let hbody_end = self.next_offset();
                self.finally_stack.pop();
                // Coverage segment ends right before the exit
                // POP_EXCEPT: CPython's SETUP_CLEANUP is popped there
                // (`POP_BLOCK`, twice for a named clause: the unbind
                // guard first), so the exit run is uncovered by this
                // region's own cleanup.
                segments.push((seg_start, self.next_offset()));
                if unbind_stmts.is_some() {
                    self.emit_pop_block_no_line();
                }
                self.emit_pop_block_no_line();
                // Handler-exit: POP_EXCEPT, then the fallthrough unbind
                // (`e = None; del e`), then the hop to `end` — all
                // NO_LOCATION in CPython (its flowgraph copies the
                // single jump predecessor's location; emitting
                // location-free gives the same event stream —
                // test_no_tracing_of_named_except_cleanup,
                // test_nested_try_if).
                self.emit_no_line(OpCode::PopExcept, 0);
                if let Some(stmts) = &unbind_stmts {
                    let saved_line = self.current_line;
                    let saved_span = self.current_span;
                    let saved_pin = self.line_pinned;
                    self.line_pinned = Some(0);
                    for s in stmts {
                        self.compile_stmt(s)?;
                    }
                    self.line_pinned = saved_pin;
                    self.current_line = saved_line;
                    self.current_span = saved_span;
                }
                // CPython's NO_LOCATION JUMP_NO_INTERRUPT to `end`.
                let exit = self.emit_no_line(OpCode::JumpForward, 0);
                self.no_interrupt_jumps.insert(exit);
                handler_exit_jumps.push(exit);
                seg_start = self.next_offset();
                if let Some(stmts) = &unbind_stmts {
                    if hbody_end > hbody_start {
                        // Exception escaping the clause body: unbind the
                        // name, then re-raise at the original site
                        // (`RERAISE 1` — the entry is lasti-flagged).
                        // Reached only via the exception table; the
                        // whole block is location-free (CPython
                        // NO_LOCATION). No PUSH_EXC_INFO: the handler
                        // context is still the clause's own.
                        let cleanup_start = self.next_offset();
                        let saved_line = self.current_line;
                        let saved_span = self.current_span;
                        let saved_pin = self.line_pinned;
                        self.line_pinned = Some(0);
                        for s in stmts {
                            self.compile_stmt(s)?;
                        }
                        self.line_pinned = saved_pin;
                        self.current_line = saved_line;
                        self.current_span = saved_span;
                        self.emit_no_line(OpCode::Reraise, 1);
                        // Punch out this clause's own `return`-path
                        // `del e` inline so a `raise` from it (or a
                        // following inlined finally) doesn't re-enter
                        // the unbind cleanup.
                        self.push_body_exc_entries(
                            hbody_start,
                            hbody_end,
                            cleanup_start,
                            body_depth,
                            true,
                            unbind_frame_id,
                        );
                    }
                }
            }
            // Unmatched: re-raise. Patch the last failed-match jump
            // (the trailing `except` label starts a block either way).
            let reraise_at = self.use_label();
            while let Some(site) = next_handler_sites.pop() {
                self.patch_jump(site, reraise_at);
            }
            // CPython emits the unmatched-exception `RERAISE 0` with
            // NO_LOCATION; the flowgraph's `propagate_line_numbers`
            // gives it the last `except` clause's location when it
            // follows the clause check in-line (a bare `except:` ends
            // in a jump, so nothing propagates). Stack: [prev, exc] —
            // RERAISE 0 pops exc and propagates it (through the cleanup
            // tail below, which restores the previous exc_info).
            self.emit_no_line(OpCode::Reraise, 0);
            segments.push((seg_start, self.next_offset()));
            // CPython's except-region cleanup tail: an exception raised
            // inside the region (match check, a clause's bare `raise`,
            // the RERAISE above) lands here (lasti-flagged): restore
            // the previous exc_info, then re-raise with the original
            // raise offset (`COPY 3; POP_EXCEPT; RERAISE 1`).
            let cleanup_start = self.next_offset();
            self.emit_no_line(OpCode::CopyTop, 3);
            self.emit_no_line(OpCode::PopExcept, 0);
            self.emit_no_line(OpCode::Reraise, 1);
            // Segment depths anchor at the PUSH_EXC_INFO: CPython's
            // declared depth is the stack depth at the SETUP point
            // ([prev] above the base), not the depth a given covered
            // slice happens to run at (the RERAISE-0 block, entered
            // with [prev, exc], still declares the SETUP depth).
            for (s, e) in segments {
                self.push_body_exc_entries_ids(
                    s,
                    e,
                    cleanup_start,
                    HANDLER_DEPTH_ANCHOR_FLAG | handlers_start,
                    true,
                    &clause_frame_ids,
                );
            }
            // Patch handler-exit jumps to end.
            let end = self.next_offset();
            for site in handler_exit_jumps {
                self.patch_jump(site, end);
            }
            // Record the handler-body end on PUSH_EXC_INFO (see above).
            self.co.instructions[push_exc_site as usize].arg = end;
        }
        // Patch the normal-exit hop past the handler region.
        if let Some(j) = normal_skip {
            let end = self.next_offset();
            self.patch_jump(j, end);
        }
        let wrap_end = self.next_offset();

        // The try/finally wrapper tail (CPython compiler_try_finally):
        // one shared normal copy (all normal completions of the
        // try/except part fall through or jump here), then the
        // exceptional copy behind the whole-range coverage.
        if has_finally {
            self.finally_stack.pop();
            // `POP_BLOCK` (NO_LOCATION) closes the try/except part.
            self.emit_pop_block_no_line();
            for s in finalbody {
                self.compile_stmt(s)?;
            }
            // CPython's NO_LOCATION JUMP_NO_INTERRUPT over the
            // exceptional copy.
            let exit_j = self.emit_no_line(OpCode::JumpForward, 0);
            self.no_interrupt_jumps.insert(exit_j);
            // Exceptional copy, headed by the `end` label and a
            // `SETUP_CLEANUP cleanup`. The dispatch loop pushed the
            // propagating exception; PUSH_EXC_INFO slides the previous
            // one underneath. The exception stays on the stack across
            // `finalbody` — every statement compiles to stack-balanced
            // bytecode — then RERAISE 0 pops and re-raises it.
            // No location (CPython): the first finally-body line fires
            // the handler-entry `'line'` event.
            let fexc_label = self.use_label();
            self.emit_setup_no_line(OpCode::SetupCleanup);
            let fexc_start = self.next_offset();
            let push_exc_site = self.emit_no_line(OpCode::PushExcInfo, 0);
            let fexc_region_id = self.fresh_finally_id();
            self.exc_on_stack += 1;
            let region_seq = self.next_fblock_seq();
            self.exc_region_ids.push((fexc_region_id, region_seq));
            for s in finalbody {
                self.compile_stmt(s)?;
            }
            self.exc_region_ids.pop();
            self.exc_on_stack -= 1;
            self.emit_no_line(OpCode::Reraise, 0);
            // CPython's finally-cleanup tail: a `raise` from inside the
            // exceptional copy itself lands here (lasti-flagged,
            // keeping the saved previous exception at the bottom):
            // restore the handled-exception state, then re-raise the
            // new exception with the original raise offset (`COPY 3;
            // POP_EXCEPT; RERAISE 1` — test_dis
            // test_disassemble_try_finally).
            let fcleanup_start = self.next_offset();
            self.emit_no_line(OpCode::CopyTop, 3);
            self.emit_no_line(OpCode::PopExcept, 0);
            self.emit_no_line(OpCode::Reraise, 1);
            // Whole-range coverage of the try/except part (body,
            // orelse, handler region and its cleanup blocks alike —
            // the innermost-wins partition carves out the slices owned
            // by inner entries), punched for this frame's own
            // return-path inlines.
            self.push_body_exc_entries(
                wrap_start,
                wrap_end,
                fexc_label,
                body_depth,
                false,
                wrap_frame_id,
            );
            // The exceptional copy's own cleanup coverage, punched from
            // the `POP_EXCEPT` of any unwind that leaves it.
            self.push_body_exc_entries(
                fexc_start,
                fcleanup_start,
                fcleanup_start,
                HANDLER_DEPTH_SENTINEL,
                true,
                Some(fexc_region_id),
            );
            // Tag the active-handler entry with the pc just past the
            // cleanup tail so the unwinder keeps it live while the
            // cleanup runs (its POP_EXCEPT pops it) and drops it when a
            // `raise` inside the finally escapes to an enclosing `try`.
            let end = self.next_offset();
            self.patch_jump(exit_j, end);
            self.co.instructions[push_exc_site as usize].arg = end;
        }
        Ok(())
    }

    /// CPython 3.14's context-manager setup dance (`codegen_with_inner`
    /// / `codegen_async_with_inner`): with the manager at TOS, leave
    /// `[exit_func, exit_self_or_null, enter_result]` (the enter result
    /// is the awaitable for the async flavour).
    fn emit_with_setup(&mut self, exit_special: u32, enter_special: u32) {
        self.emit(OpCode::CopyTop, 1);
        self.emit(OpCode::LoadSpecial, exit_special);
        self.emit(OpCode::Swap, 2);
        self.emit(OpCode::Swap, 3);
        self.emit(OpCode::LoadSpecial, enter_special);
        self.emit(OpCode::Call, 0);
    }

    /// CPython's `codegen_slice_two_parts`: push the lower and upper
    /// bounds of a slice expression, each defaulting to `None`.
    fn compile_slice_two_parts(&mut self, slice: &Expr) -> Result<(), CompileError> {
        let ExprKind::Slice { lower, upper, .. } = &slice.kind else {
            return self.compile_expr(slice);
        };
        for part in [lower, upper] {
            match part {
                Some(e) => self.compile_expr(e)?,
                None => {
                    // `codegen_slice_two_parts`: `ADDOP_LOAD_CONST(c,
                    // LOC(s), Py_None)` with the Slice node's location.
                    let idx = self.co.intern_constant(Constant::None);
                    let saved_line = self.current_line;
                    let saved_span = self.current_span;
                    self.set_line_from(slice.span.start.0);
                    self.set_span(slice.span);
                    self.emit(OpCode::LoadConst, idx);
                    self.current_line = saved_line;
                    self.current_span = saved_span;
                }
            }
        }
        Ok(())
    }

    /// CPython's `codegen_call_exit_with_nones`: with the exit pair at
    /// the top of the stack, `__exit__(None, None, None)`.
    fn emit_call_exit_with_nones(&mut self) {
        let none_idx = self.co.intern_constant(Constant::None);
        self.emit(OpCode::LoadConst, none_idx);
        self.emit(OpCode::LoadConst, none_idx);
        self.emit(OpCode::LoadConst, none_idx);
        self.emit(OpCode::Call, 3);
    }

    /// Compile a `with` statement. Each item is desugared via a
    /// synthetic local that holds the context manager so the normal
    /// and exception exit paths can both reach `__exit__`.
    fn compile_with(&mut self, items: &[WithItem], body: &[Stmt]) -> Result<(), CompileError> {
        if items.is_empty() {
            for s in body {
                self.compile_stmt(s)?;
            }
            return Ok(());
        }
        // Multi-item recursion happens at the body site below:
        // `with a, b: body` ≡ `with a: with b: body`.
        let (item, rest) = items.split_first().expect("nonempty");
        // PEP 657: the whole setup/`__exit__` dance for this item is
        // attributed to the context-manager *expression* itself, so a
        // traceback through `__init__`/`__enter__`/`__exit__` pinpoints
        // the precise manager in `with A(), B(), C():` (CPython
        // `testExceptionLocation`).
        self.set_line_from(item.context_expr.span.start.0);
        self.set_span(item.context_expr.span);
        let with_line = self.current_line;
        let with_span = self.current_span;
        // Evaluate cm, then CPython 3.14's LOAD_SPECIAL dance: the
        // `__exit__` (method, self-or-null) pair stays on the operand
        // stack for the whole body under the `__enter__` result —
        // CPython's SETUP_WITH discipline (test_dis grades the exact
        // shape, and co_varnames must not contain synthetic slots).
        //   COPY 1; LOAD_SPECIAL __exit__; SWAP 2; SWAP 3;
        //   LOAD_SPECIAL __enter__; CALL 0
        self.compile_expr(&item.context_expr)?;
        self.current_line = with_line;
        self.current_span = with_span;
        self.emit_with_setup(SPECIAL_EXIT, SPECIAL_ENTER);
        // `SETUP_WITH final` at the item's location, then the `block`
        // label: CPython's exception coverage starts right after the
        // enter call, so the bind (or POP_TOP) of the `__enter__`
        // result is inside it.
        self.emit_setup(OpCode::SetupWith);
        let cover_start = self.use_label();
        if let Some(target) = &item.optional_vars {
            self.compile_assign(target)?;
        } else {
            self.emit(OpCode::PopTop, 0);
        }
        // Coverage starts at the bind, one slot *above* the body's
        // baseline (the `__enter__` result rides over `__exit__`), so
        // the handler depth anchors at the first body instruction.
        let depth_anchor = self.next_offset();

        // Push a synthetic finally frame so `return`, `break`, and
        // `continue` from inside the body call the on-stack
        // `__exit__(None, None, None)` before transferring control.
        let with_loop_depth = self.loop_stack.len();
        let with_frame_id = self.fresh_finally_id();
        let seq = self.next_fblock_seq();
        self.finally_stack.push(FinallyFrame {
            kind: FinallyKind::WithExit {
                line: with_line,
                span: with_span,
            },
            loop_depth_at_push: with_loop_depth,
            id: with_frame_id,
            pop_except_after: false,
            region_hole_id: 0,
            exc_at_push: self.exc_on_stack,
            handler_at_push: self.handler_depth,
            rv_at_push: self.pending_retvals,
            seq,
        });

        let body_start = cover_start;
        let body_result = if rest.is_empty() {
            body.iter().try_for_each(|s| self.compile_stmt(s))
        } else {
            self.compile_with(rest, body)
        };
        body_result?;
        let body_end = self.next_offset();
        // `POP_BLOCK` (NO_LOCATION) closes the body's coverage.
        self.emit_pop_block_no_line();

        // Pop the synthetic frame; the explicit normal-exit path
        // below emits the same call inline.
        self.finally_stack.pop();

        // Attribute the whole exit path to this item's expression.
        self.current_line = with_line;
        self.current_span = with_span;

        // Normal exit: the `__exit__` pair is on the stack; call it
        // with three `None`s (CPython `codegen_call_exit_with_nones`:
        // `CALL 3`).
        self.emit_call_exit_with_nones();
        self.emit(OpCode::PopTop, 0);
        let end_jump = self.emit(OpCode::JumpForward, 0);
        self.plain_jumps.insert(end_jump);

        // Exception handler (CPython 3.14 shape):
        //   L3: SETUP_CLEANUP; PUSH_EXC_INFO; WITH_EXCEPT_START; TO_BOOL;
        //       POP_JUMP_IF_TRUE L4; RERAISE 2
        //   L4: POP_TOP; POP_BLOCK
        //   L5: POP_EXCEPT; POP_TOP; POP_TOP; POP_TOP
        //   --  COPY 3; POP_EXCEPT; RERAISE 1   (cleanup, covers L3..L5)
        // The `final` label heads the `SETUP_CLEANUP`; the cleanup
        // range opens behind it.
        let final_label = self.use_label();
        self.emit_setup(OpCode::SetupCleanup);
        let handler_start = self.next_offset();
        // CPython's SETUP_WITH cleanup carries the lasti flag: when
        // __exit__ doesn't suppress, RERAISE restores f_lasti to the
        // raising instruction inside the body (PEP 626). A `return`/
        // `break`/`continue` from the body inlines `__exit__(None,None,
        // None)` here; punch that inline out so a `raise` from it isn't
        // re-caught and `__exit__` re-invoked with the exception triple.
        // Depth resolves statically at the body baseline (anchored
        // sentinel): keep the on-stack `__exit__` and everything below.
        self.push_body_exc_entries(
            body_start,
            body_end,
            final_label,
            HANDLER_DEPTH_ANCHOR_FLAG | depth_anchor,
            true,
            Some(with_frame_id),
        );
        // Entry stack: [exit_func, exit_self, lasti, exc]. Record the
        // propagating exception as the active handled exception for the
        // duration of the `__exit__` call so a `raise` inside `__exit__`
        // chains it as the new exception's implicit `__context__`
        // (PEP 3134) — `contextlib.ExitStack`'s `_fix_exception_context`
        // walks each callback exception's context back to
        // `sys.exc_info()[1]`. After PUSH_EXC_INFO:
        // [exit_func, exit_self, lasti, prev, exc].
        let push_exc_site = self.emit(OpCode::PushExcInfo, 0);
        // Calls `__exit__(type(exc), exc, exc.__traceback__)` peeking
        // the exit pair at depths 5 and 4; pushes the result.
        self.emit(OpCode::WithExceptStart, 0);
        let cleanup_cover_end = self.emit_with_except_finish();
        // Cleanup tail: a `raise` out of `__exit__` itself (or the
        // RERAISE) lands here with [exit_func, exit_self, lasti, prev]
        // preserved plus the new lasti/exception: restore the
        // handled-exception state and re-raise (CPython `COPY 3;
        // POP_EXCEPT; RERAISE 1`).
        let cleanup_start = self.next_offset();
        self.emit_no_line(OpCode::CopyTop, 3);
        self.emit_no_line(OpCode::PopExcept, 0);
        self.emit_no_line(OpCode::Reraise, 1);
        self.co.exception_table.push(ExcHandler {
            start: handler_start,
            end: cleanup_cover_end,
            handler: cleanup_start,
            depth: HANDLER_DEPTH_SENTINEL,
            push_lasti: true,
        });
        let end = self.next_offset();
        self.patch_jump(end_jump, end);
        for site in std::mem::take(&mut self.with_exit_jumps) {
            self.patch_jump(site, end);
        }
        // Tag the active-handler entry with the pc just past the whole
        // handler region: the swallow path's POP_EXCEPT (or the cleanup
        // tail's) pops it; an escape beyond `end` drops it in the
        // unwinder.
        self.co.instructions[push_exc_site as usize].arg = end;
        Ok(())
    }

    fn cell_or_free_index(&mut self, name: &str) -> u32 {
        // Layout: cellvars first, then freevars. A name this scope
        // resolves as free but that an inlined comprehension also made
        // a cell (`DEF_COMP_CELL`) sits in both lists; the scope's own
        // accesses go through the free slot.
        let prefer_free = matches!(
            self.bindings.get(name),
            Some(Binding::Free | Binding::Nonlocal)
        ) && self.free_order.iter().any(|n| n == name);
        if !prefer_free {
            if let Some(i) = self.co.cellvars.iter().position(|n| n == name) {
                return i as u32;
            }
        }
        if let Some(i) = self.free_order.iter().position(|n| n == name) {
            return (self.co.cellvars.len() + i) as u32;
        }
        // Promote: this is a free in the inner but we haven't
        // recorded it here. Add as free.
        self.free_order.push(name.to_owned());
        (self.co.cellvars.len() + self.free_order.len() - 1) as u32
    }

    // ---------- assignment ----------

    /// Emit the *value* of a single annotation expression onto the stack.
    ///
    /// Under PEP 563 (`from __future__ import annotations`) annotations are
    /// not evaluated: we push the annotation's verbatim source text as a
    /// string constant, so `__annotations__` ends up storing e.g.
    /// `'list[int]'` instead of the runtime object. This is what lets
    /// forward references and not-yet-imported names (e.g. `IO[str]` typed
    /// only for the type checker) appear in annotations without raising at
    /// definition time. Falls back to evaluating the expression when the
    /// future flag is off, or when no source is available to slice.
    fn emit_annotation(&mut self, annotation: &Expr) -> Result<(), CompileError> {
        if self.future_annotations {
            // CPython stores the *unparsed* AST (`_PyAST_ExprAsUnicode`),
            // which normalises quoting and whitespace — `List[list["C2"]]`
            // annotates as "List[list['C2']]". Fall back to the raw source
            // slice for nodes the unparser doesn't cover.
            let mut text = weavepy_parser::unparse::unparse_expr(annotation)
                .or_else(|| self.annotation_source(annotation));
            // The Rust AST doesn't carry `Constant.kind`, so a legacy
            // `u'…'` prefix (which CPython's unparser preserves) is
            // recovered from the source text.
            if let (Some(t), ExprKind::Constant(AstConstant::Str(_))) =
                (text.as_deref(), &annotation.kind)
            {
                if t.starts_with(['\'', '"'])
                    && self
                        .annotation_source(annotation)
                        .is_some_and(|src| src.starts_with(['u', 'U']))
                {
                    text = Some(format!("u{t}"));
                }
            }
            if let Some(text) = text {
                let idx = self.co.intern_constant(Constant::Str(text));
                self.emit(OpCode::LoadConst, idx);
                return Ok(());
            }
        }
        // PEP 646: an unpacked `*args` annotation — `def f(*args: *Ts)`,
        // `*args: *tuple[int, ...]`. CPython evaluates it as the single
        // element of `iter(value)` (a `TypeVarTuple` yields `Unpack[Ts]`,
        // an unpacked `tuple[...]` alias yields itself), i.e. it compiles
        // the inner value then `UNPACK_SEQUENCE 1`. A bare `Starred` is
        // otherwise rejected by `compile_expr`, so we special-case it here.
        if let ExprKind::Starred(inner) = &annotation.kind {
            self.compile_expr(inner)?;
            self.emit(OpCode::UnpackSequence, 1);
            return Ok(());
        }
        self.compile_expr(annotation)
    }

    /// The verbatim source text covered by `expr`'s span, trimmed of
    /// surrounding whitespace. Returns `None` when the compiler holds no
    /// source (an AST was compiled directly) or the span is degenerate, so
    /// the caller can fall back to eager evaluation.
    fn annotation_source(&self, expr: &Expr) -> Option<String> {
        let start = expr.span.start.0 as usize;
        let end = expr.span.end.0 as usize;
        if self.source.is_empty() || end <= start || end > self.source.len() {
            return None;
        }
        let text = self.source.get(start..end)?.trim();
        if text.is_empty() {
            None
        } else {
            Some(text.to_owned())
        }
    }

    /// `codegen_check_ann_expr`: evaluate a sub-expression of an
    /// annotated target for its side effects and discard it (the
    /// `POP_TOP` sits at the expression's own location).
    fn compile_check_ann_expr(&mut self, e: &Expr) -> Result<(), CompileError> {
        self.compile_expr(e)?;
        let saved = self.current_span;
        self.set_span(e.span);
        self.emit(OpCode::PopTop, 0);
        self.current_span = saved;
        Ok(())
    }

    /// `codegen_check_ann_subscr`: the pieces of an annotated
    /// subscript target's index that must be evaluated — each bound
    /// of a slice, every element of an extended slice, or the index
    /// expression itself.
    fn compile_check_ann_subscr(&mut self, e: &Expr) -> Result<(), CompileError> {
        match &e.kind {
            ExprKind::Slice { lower, upper, step } => {
                for part in [lower, upper, step].into_iter().flatten() {
                    self.compile_check_ann_expr(part)?;
                }
                Ok(())
            }
            ExprKind::Tuple(elts) => {
                for elt in elts {
                    self.compile_check_ann_subscr(elt)?;
                }
                Ok(())
            }
            _ => self.compile_check_ann_expr(e),
        }
    }

    /// PEP 563 (`codegen_annassign` under `CO_FUTURE_ANNOTATIONS`):
    /// `__annotations__[name] = '<source>'` for a class- or module-body
    /// `x: T = ...`. The dict itself comes from the block prologue's
    /// `SETUP_ANNOTATIONS` (emitted when the body has any annotation);
    /// the guard below only covers a body compiled without one.
    /// The string constant sits at the annotation's location
    /// (`codegen_visit_annexpr`), the store at the statement's.
    fn compile_annotation_record(
        &mut self,
        name: &str,
        annotation: &Expr,
    ) -> Result<(), CompileError> {
        let dict_name = "__annotations__";
        if !self.annotations_initialized {
            self.emit(OpCode::BuildMap, 0);
            let idx = self.co.intern_name(dict_name);
            self.emit(OpCode::StoreName, idx);
            self.annotations_initialized = true;
        }
        let saved_span = self.current_span;
        let saved_line = self.current_line;
        self.set_line_from(annotation.span.start.0);
        self.set_span(annotation.span);
        self.emit_annotation(annotation)?;
        self.current_line = saved_line;
        self.current_span = saved_span;
        let dict_idx = self.co.intern_name(dict_name);
        self.emit(OpCode::LoadName, dict_idx);
        let key_idx = self.co.intern_constant(Constant::Str(name.to_owned()));
        self.emit(OpCode::LoadConst, key_idx);
        self.emit(OpCode::StoreSubscr, 0);
        Ok(())
    }

    fn compile_assign(&mut self, target: &Expr) -> Result<(), CompileError> {
        match &target.kind {
            ExprKind::Name(n) if n == "__debug__" => Err(CompileError::spanned(
                "cannot assign to __debug__",
                target.span,
            )),
            ExprKind::Name(n) => {
                // CPython attributes the STORE to the Name node itself,
                // not the enclosing statement. Only observable when the
                // target sits on its own line — `with cm \ as x:` or a
                // parenthesized multi-line unpack — where the store must
                // fire its own `'line'` trace event (test_sys_settrace
                // test_jump_out_of_with_assignment /
                // test_jump_extended_args_unpack_ex_tricky).
                let saved_line = self.current_line;
                let saved_span = self.current_span;
                self.set_line_from(target.span.start.0);
                self.set_span(target.span);
                self.emit_store_name(n);
                self.current_line = saved_line;
                self.current_span = saved_span;
                Ok(())
            }
            // `obj.__debug__ = 1` — CPython's `forbidden_name` check
            // applies to attribute targets too.
            ExprKind::Attribute { attr, .. } if attr == "__debug__" => Err(CompileError::spanned(
                "cannot assign to __debug__",
                target.span,
            )),
            ExprKind::Attribute { value, attr } => {
                self.compile_expr(value)?;
                let idx = self.co.intern_name(attr);
                let saved = self.current_span;
                self.set_span(target.span);
                self.with_attr_location(target.span.end.0, attr.len() as u32, |c| {
                    c.emit(OpCode::StoreAttr, idx);
                });
                self.current_span = saved;
                Ok(())
            }
            ExprKind::Subscript { value, slice } => {
                // `codegen_subscript`: the store carries the Subscript
                // node's own location.
                self.compile_expr(value)?;
                let saved_line = self.current_line;
                let saved_span = self.current_span;
                if should_apply_two_element_slice_optimization(slice) {
                    self.compile_slice_two_parts(slice)?;
                    self.set_line_from(target.span.start.0);
                    self.set_span(target.span);
                    self.emit(OpCode::StoreSlice, 0);
                } else {
                    self.compile_expr(slice)?;
                    self.set_line_from(target.span.start.0);
                    self.set_span(target.span);
                    self.emit(OpCode::StoreSubscr, 0);
                }
                self.current_line = saved_line;
                self.current_span = saved_span;
                Ok(())
            }
            ExprKind::Tuple(items) | ExprKind::List(items) => {
                // PEP 3132 — starred sub-target. Exactly one `*x` may
                // appear; everything before becomes the head, everything
                // after becomes the tail, and `*x` captures the middle
                // as a list.
                let starred_idx = items
                    .iter()
                    .position(|t| matches!(t.kind, ExprKind::Starred(_)));
                // CPython's compiler rejects a second `*x` before emitting
                // anything (test_unpack_ex doctests).
                if let Some(second) = items
                    .iter()
                    .enumerate()
                    .filter(|(_, t)| matches!(t.kind, ExprKind::Starred(_)))
                    .nth(1)
                {
                    return Err(CompileError::parser_spanned(
                        "multiple starred expressions in assignment",
                        second.1.span,
                    ));
                }
                if let Some(idx) = starred_idx {
                    let before = idx as u32;
                    let after = (items.len() - idx - 1) as u32;
                    if before > 0xFF || after > 0xFF {
                        // CPython's limit check (`compile.c`
                        // `assignment_helper`): 255 leading names is the
                        // UNPACK_EX operand ceiling.
                        return Err(CompileError::parser_spanned(
                            "too many expressions in star-unpacking assignment",
                            target.span,
                        ));
                    }
                    // `unpack_helper` runs at the Tuple/List node's own
                    // location; each sub-target then carries its own.
                    let saved_line = self.current_line;
                    let saved_span = self.current_span;
                    self.set_line_from(target.span.start.0);
                    self.set_span(target.span);
                    self.emit(OpCode::UnpackEx, (before << 8) | after);
                    self.current_line = saved_line;
                    self.current_span = saved_span;
                    for t in items {
                        match &t.kind {
                            ExprKind::Starred(inner) => self.compile_assign(inner)?,
                            _ => self.compile_assign(t)?,
                        }
                    }
                } else {
                    let saved_line = self.current_line;
                    let saved_span = self.current_span;
                    self.set_line_from(target.span.start.0);
                    self.set_span(target.span);
                    self.emit(OpCode::UnpackSequence, items.len() as u32);
                    self.current_line = saved_line;
                    self.current_span = saved_span;
                    for t in items {
                        self.compile_assign(t)?;
                    }
                }
                Ok(())
            }
            ExprKind::Starred(_) => {
                // The tuple/list arm above unwraps its starred element
                // before recursing, so reaching here means a *bare*
                // top-level starred target (`*a = xs`) — a SyntaxError in
                // CPython (`*a,` parses as a one-element tuple and never
                // lands here).
                Err(CompileError::parser_spanned(
                    "starred assignment target must be in a list or tuple",
                    target.span,
                ))
            }
            _ => Err(CompileError::parser_spanned(
                format!("cannot assign to {}", expr_name(target)),
                target.span,
            )),
        }
    }

    /// `starunpack_helper_impl`: a list/tuple/set display, or the
    /// positional operands of a CALL_FUNCTION_EX, with `pushed` operands
    /// already on the stack. Without a splat and under the stack-use
    /// guideline everything is pushed and built in one go; otherwise a
    /// container is started at the first splat (or immediately when
    /// big) and elements fold in with `add`/`extend`. `tuple` builds
    /// through a list and converts with `INTRINSIC_LIST_TO_TUPLE`.
    #[allow(clippy::too_many_arguments)]
    fn starunpack_helper(
        &mut self,
        elts: &[Expr],
        injected_arg: Option<&str>,
        pushed: u32,
        build: OpCode,
        add: OpCode,
        extend: OpCode,
        tuple: bool,
    ) -> Result<(), CompileError> {
        let n = elts.len();
        let big = n + pushed as usize + usize::from(injected_arg.is_some()) > STACK_USE_GUIDELINE;
        let seen_star = elts.iter().any(|e| matches!(e.kind, ExprKind::Starred(_)));
        if !seen_star && !big {
            for e in elts {
                self.compile_expr(e)?;
            }
            let mut n = n as u32;
            if let Some(name) = injected_arg {
                self.emit_load_name(name);
                n += 1;
            }
            if tuple {
                self.emit(OpCode::BuildTuple, n + pushed);
            } else {
                self.emit(build, n + pushed);
            }
            return Ok(());
        }
        let mut sequence_built = false;
        if big {
            self.emit(build, pushed);
            sequence_built = true;
        }
        for (i, e) in elts.iter().enumerate() {
            if let ExprKind::Starred(inner) = &e.kind {
                if !sequence_built {
                    self.emit(build, i as u32 + pushed);
                    sequence_built = true;
                }
                self.compile_expr(inner)?;
                self.emit(extend, 1);
            } else {
                self.compile_expr(e)?;
                if sequence_built {
                    self.emit(add, 1);
                }
            }
        }
        debug_assert!(sequence_built);
        if let Some(name) = injected_arg {
            self.emit_load_name(name);
            self.emit(add, 1);
        }
        if tuple {
            self.emit(OpCode::ListToTuple, 0);
        }
        Ok(())
    }

    /// The positional-arguments half of `ex_call`: a lone `*x` with
    /// nothing pushed passes `x` through raw (the VM's `CallEx`
    /// converts it, branding a non-iterable with the callable's name:
    /// "g() argument after * must be an iterable, not Nothing");
    /// anything else folds into a tuple via [`Self::starunpack_helper`].
    fn compile_ex_call_args(
        &mut self,
        args: &[Expr],
        injected_arg: Option<&str>,
        pushed: u32,
    ) -> Result<(), CompileError> {
        if pushed == 0 && args.len() == 1 {
            if let ExprKind::Starred(inner) = &args[0].kind {
                return self.compile_expr(inner);
            }
        }
        self.starunpack_helper(
            args,
            injected_arg,
            pushed,
            OpCode::BuildList,
            OpCode::ListAppend,
            OpCode::ListExtend,
            true,
        )
    }

    /// `codegen_dict`: `{k: v, ...}` displays. Runs of explicit pairs
    /// go through [`Self::compile_subdict`] in chunks that keep the
    /// stack under `STACK_USE_GUIDELINE` (a chunk closes when it holds
    /// sixteen pairs *and* another explicit pair follows); each `**m`
    /// and each chunk after the first merges with `DICT_UPDATE 1`.
    fn compile_dict(&mut self, keys: &[Option<Expr>], values: &[Expr]) -> Result<(), CompileError> {
        let n = values.len();
        let mut have_dict = false;
        let mut elements = 0usize;
        for i in 0..n {
            let is_unpacking = keys[i].is_none();
            if is_unpacking {
                if elements > 0 {
                    self.compile_subdict(keys, values, i - elements, i)?;
                    if have_dict {
                        self.emit(OpCode::DictUpdate, 0);
                    }
                    have_dict = true;
                    elements = 0;
                }
                if !have_dict {
                    self.emit(OpCode::BuildMap, 0);
                    have_dict = true;
                }
                self.compile_expr(&values[i])?;
                self.emit(OpCode::DictUpdate, 0);
            } else if elements * 2 > STACK_USE_GUIDELINE {
                self.compile_subdict(keys, values, i - elements, i + 1)?;
                if have_dict {
                    self.emit(OpCode::DictUpdate, 0);
                }
                have_dict = true;
                elements = 0;
            } else {
                elements += 1;
            }
        }
        if elements > 0 {
            self.compile_subdict(keys, values, n - elements, n)?;
            if have_dict {
                self.emit(OpCode::DictUpdate, 0);
            }
            have_dict = true;
        }
        if !have_dict {
            self.emit(OpCode::BuildMap, 0);
        }
        Ok(())
    }

    /// `codegen_subdict`: pairs `begin..end` as one dict — `BUILD_MAP n`
    /// under the guideline, else `BUILD_MAP 0` fed by `MAP_ADD 1`s.
    fn compile_subdict(
        &mut self,
        keys: &[Option<Expr>],
        values: &[Expr],
        begin: usize,
        end: usize,
    ) -> Result<(), CompileError> {
        let n = end - begin;
        let big = n * 2 > STACK_USE_GUIDELINE;
        if big {
            self.emit(OpCode::BuildMap, 0);
        }
        for i in begin..end {
            let key = keys[i].as_ref().expect("explicit pair");
            self.compile_expr(key)?;
            self.compile_expr(&values[i])?;
            if big {
                self.emit(OpCode::MapAdd, 1);
            }
        }
        if !big {
            self.emit(OpCode::BuildMap, n as u32);
        }
        Ok(())
    }

    /// `codegen_subkwargs`: a run of named keywords as one dict —
    /// `BUILD_MAP n` under the guideline, else a `NO_LOCATION`
    /// `BUILD_MAP 0` fed by `MAP_ADD 1`s.
    fn compile_subkwargs(
        &mut self,
        keywords: &[weavepy_parser::ast::Keyword],
    ) -> Result<(), CompileError> {
        let n = keywords.len();
        debug_assert!(n > 0);
        let big = n * 2 > STACK_USE_GUIDELINE;
        if big {
            self.emit_no_line(OpCode::BuildMap, 0);
        }
        for kw in keywords {
            let name = kw.arg.clone().expect("named keyword run");
            let idx = self.co.intern_constant(Constant::Str(name));
            self.emit(OpCode::LoadConst, idx);
            self.compile_expr(&kw.value)?;
            if big {
                self.emit_no_line(OpCode::MapAdd, 1);
            }
        }
        if !big {
            self.emit(OpCode::BuildMap, n as u32);
        }
        Ok(())
    }

    /// The keyword half of `ex_call`: runs of named keywords become
    /// sub-dicts (`codegen_subkwargs`), each `**d` and each later run
    /// is folded into the first dict with `DICT_MERGE 1` (whose
    /// operand must be a mapping — "argument after ** must be a
    /// mapping, not list" — and which rejects duplicate keywords).
    fn compile_ex_call_kwargs(
        &mut self,
        keywords: &[weavepy_parser::ast::Keyword],
    ) -> Result<(), CompileError> {
        let mut have_dict = false;
        let mut nseen = 0usize;
        for (i, kw) in keywords.iter().enumerate() {
            if kw.arg.is_none() {
                if nseen > 0 {
                    self.compile_subkwargs(&keywords[i - nseen..i])?;
                    if have_dict {
                        self.emit(OpCode::DictUpdate, 1);
                    }
                    have_dict = true;
                    nseen = 0;
                }
                if !have_dict {
                    self.emit(OpCode::BuildMap, 0);
                    have_dict = true;
                }
                self.compile_expr(&kw.value)?;
                self.emit(OpCode::DictUpdate, 1);
            } else {
                nseen += 1;
            }
        }
        if nseen > 0 {
            let n = keywords.len();
            self.compile_subkwargs(&keywords[n - nseen..])?;
            if have_dict {
                self.emit(OpCode::DictUpdate, 1);
            }
            have_dict = true;
        }
        debug_assert!(have_dict);
        Ok(())
    }

    fn compile_delete(&mut self, target: &Expr) -> Result<(), CompileError> {
        match &target.kind {
            ExprKind::Name(n) if n == "__debug__" => Err(CompileError::spanned(
                "cannot delete __debug__",
                target.span,
            )),
            ExprKind::Name(n) => {
                // `codegen_visit_expr`: every Del-context node carries
                // its own location.
                let saved_line = self.current_line;
                let saved_span = self.current_span;
                self.set_line_from(target.span.start.0);
                self.set_span(target.span);
                self.emit_delete_name(n);
                self.current_line = saved_line;
                self.current_span = saved_span;
                Ok(())
            }
            ExprKind::Attribute { value, attr } => {
                self.compile_expr(value)?;
                let idx = self.co.intern_name(attr);
                let saved_line = self.current_line;
                let saved = self.current_span;
                self.set_line_from(target.span.start.0);
                self.set_span(target.span);
                self.with_attr_location(target.span.end.0, attr.len() as u32, |c| {
                    c.emit(OpCode::DeleteAttr, idx);
                });
                self.current_line = saved_line;
                self.current_span = saved;
                Ok(())
            }
            ExprKind::Subscript { value, slice } => {
                self.compile_expr(value)?;
                self.compile_expr(slice)?;
                let saved_line = self.current_line;
                let saved_span = self.current_span;
                self.set_line_from(target.span.start.0);
                self.set_span(target.span);
                self.emit(OpCode::DeleteSubscr, 0);
                self.current_line = saved_line;
                self.current_span = saved_span;
                Ok(())
            }
            ExprKind::Tuple(items) | ExprKind::List(items) => {
                for t in items {
                    self.compile_delete(t)?;
                }
                Ok(())
            }
            _ => Err(CompileError::parser_spanned(
                format!("cannot delete {}", expr_name(target)),
                target.span,
            )),
        }
    }

    fn emit_delete_name(&mut self, name: &str) {
        let binding = self.classify_for_store(name);
        match binding {
            Binding::Local => {
                let idx = self.var_index_or_add(name);
                self.emit(OpCode::DeleteFast, idx);
            }
            Binding::Cell | Binding::Free | Binding::Nonlocal => {
                // `del NAME` clears the cell's contents. This must NOT
                // touch the value stack (unlike `StoreDeref`, which pops
                // its operand) — emitting `StoreDeref` here underflows
                // the stack. `DeleteDeref` empties the cell and raises
                // NameError at runtime if it was already empty.
                let idx = self.cell_or_free_index(name);
                self.emit(OpCode::DeleteDeref, idx);
            }
            Binding::Global | Binding::ClassPassthrough => {
                let idx = self.co.intern_name(name);
                // GLOBAL_EXPLICIT: `global x` in a class body — or in any
                // scope nested under this module block — bypasses the
                // local namespace entirely.
                let explicit_global =
                    matches!(binding, Binding::Global) && self.explicit_globals.contains(name);
                if matches!(self.kind, CodeKind::Module | CodeKind::Class) && !explicit_global {
                    self.emit(OpCode::DeleteName, idx);
                } else {
                    self.emit(OpCode::DeleteGlobal, idx);
                }
            }
        }
    }

    fn emit_store_name(&mut self, name: &str) {
        let binding = self.classify_for_store(name);
        // `nonlocal __class__` in a class body stores through the
        // *enclosing function's* cell (the same-named freevar), never the
        // implicit class cellvar `cell_or_free_index` would find first.
        if self.kind == CodeKind::Class
            && name == "__class__"
            && matches!(binding, Binding::Free | Binding::Nonlocal)
        {
            if let Some(pos) = self.free_order.iter().position(|n| n == "__class__") {
                let idx = (self.co.cellvars.len() + pos) as u32;
                self.emit(OpCode::StoreDeref, idx);
                return;
            }
        }
        match binding {
            Binding::Local => {
                let idx = self.var_index_or_add(name);
                self.emit(OpCode::StoreFast, idx);
            }
            Binding::Cell | Binding::Free | Binding::Nonlocal => {
                let idx = self.cell_or_free_index(name);
                self.emit(OpCode::StoreDeref, idx);
            }
            Binding::Global | Binding::ClassPassthrough => {
                let idx = self.co.intern_name(name);
                // GLOBAL_EXPLICIT → STORE_GLOBAL: an explicit `global x`
                // in a class body bypasses the class namespace; at module
                // level, a `global x` declared in *any* nested scope makes
                // the top-level store hit the true globals mapping (visible
                // when exec runs with distinct globals/locals).
                let explicit_global =
                    matches!(binding, Binding::Global) && self.explicit_globals.contains(name);
                if matches!(self.kind, CodeKind::Module | CodeKind::Class) && !explicit_global {
                    self.emit(OpCode::StoreName, idx);
                } else {
                    self.emit(OpCode::StoreGlobal, idx);
                }
            }
        }
    }

    fn var_index_or_add(&mut self, name: &str) -> u32 {
        if let Some(i) = self.co.varnames.iter().position(|n| n == name) {
            return i as u32;
        }
        self.co.varnames.push(name.to_owned());
        (self.co.varnames.len() - 1) as u32
    }

    fn classify_for_store(&mut self, name: &str) -> Binding {
        match self.bindings.get(name) {
            Some(b) => *b,
            None => {
                if matches!(self.kind, CodeKind::Module | CodeKind::Class) {
                    self.bindings.insert(name.to_owned(), Binding::Global);
                    Binding::Global
                } else {
                    self.bindings.insert(name.to_owned(), Binding::Local);
                    Binding::Local
                }
            }
        }
    }

    fn compile_load_target(&mut self, target: &Expr) -> Result<(), CompileError> {
        match &target.kind {
            ExprKind::Name(n) => {
                self.emit_load_name(n);
                Ok(())
            }
            _ => self.compile_expr(target),
        }
    }

    fn emit_load_name(&mut self, name: &str) {
        // `__debug__` is a compile-time constant in CPython: `True`
        // at optimize 0, `False` under `-O`/`-OO` (RFC 0052).
        if name == "__debug__" {
            let idx = self
                .co
                .intern_constant(Constant::Bool(self.params.optimize == 0));
            self.emit(OpCode::LoadConst, idx);
            return;
        }
        let binding = self.bindings.get(name).copied();
        // PEP 695 annotation scope inside a class body: free,
        // (implicit-)global, and *cell* loads consult the
        // `__classdict__` mapping first — CPython's
        // `LOAD_FROM_DICT_OR_{DEREF,GLOBALS}` (`codegen_nameop` routes
        // every `OP_DEREF` load through the dict when
        // `ste_can_see_class_scope`). The scope's plain fast locals
        // (a type parameter no nested scope captures, the hoisted
        // `.defaults`, `.generic_base`) resolve normally below.
        if let Some(ctx) = self.lazy_class_ctx.clone() {
            if matches!(binding, Some(Binding::Cell)) {
                let dict_idx = self.cell_or_free_index("__classdict__");
                self.emit(OpCode::LoadDeref, dict_idx);
                let idx = self.cell_or_free_index(name);
                self.emit(OpCode::LoadClassdictOrDeref, idx);
                return;
            }
            let own = matches!(binding, Some(Binding::Local));
            if !own && name != "__classdict__" {
                // Mirrors CPython symtable.c `analyze_name`'s
                // `class_entry` shortcut: names the visible class body
                // declares `global` load straight from globals; names
                // it binds load from the class dict then *globals*,
                // never an enclosing function's cell.
                if ctx.globals.contains(name) {
                    let idx = self.co.intern_name(name);
                    self.emit(OpCode::LoadGlobal, idx);
                } else if !ctx.assigned.contains(name)
                    && matches!(binding, Some(Binding::Free | Binding::Nonlocal))
                {
                    let dict_idx = self.cell_or_free_index("__classdict__");
                    self.emit(OpCode::LoadDeref, dict_idx);
                    let idx = self.cell_or_free_index(name);
                    self.emit(OpCode::LoadClassdictOrDeref, idx);
                } else {
                    let dict_idx = self.cell_or_free_index("__classdict__");
                    self.emit(OpCode::LoadDeref, dict_idx);
                    let idx = self.co.intern_name(name);
                    self.emit(OpCode::LoadClassdictOrGlobal, idx);
                }
                return;
            }
        }
        // Class-body `__class__` reads never see the implicit class cell
        // (CPython's symtable keeps the body's own use FREE or GLOBAL
        // even when methods force the `__class__` cellvar): resolve
        // class-dict-first through the enclosing function's cell when
        // one was reserved, else as a plain name.
        if self.kind == CodeKind::Class
            && name == "__class__"
            && !matches!(binding, Some(Binding::Global | Binding::ClassPassthrough))
        {
            if let Some(pos) = self.free_order.iter().position(|n| n == "__class__") {
                let idx = (self.co.cellvars.len() + pos) as u32;
                self.emit(OpCode::LoadLocals, 0);
                self.emit(OpCode::LoadClassdictOrDeref, idx);
            } else {
                let idx = self.co.intern_name(name);
                self.emit(OpCode::LoadName, idx);
            }
            return;
        }
        // An inlined comprehension in a class body resolves names the
        // way its own (function-like) scope would
        // (`_PyCompile_TweakInlinedComprehensionScopes` overrides every
        // symbol while the body compiles): the class namespace is
        // invisible, so an unbound name is a plain global, a free
        // name a plain deref, and a class attribute the class also
        // forwards from an enclosing function reaches that cell.
        if self.kind == CodeKind::Class && self.inline_comp > 0 {
            match binding {
                Some(Binding::Local) => {
                    let idx = self.var_index_or_add(name);
                    self.emit(OpCode::LoadFast, idx);
                }
                Some(Binding::Cell | Binding::Nonlocal | Binding::Free) => {
                    let idx = self.cell_or_free_index(name);
                    self.emit(OpCode::LoadDeref, idx);
                }
                Some(Binding::ClassPassthrough) if self.free_order.iter().any(|n| n == name) => {
                    let idx = self.cell_or_free_index(name);
                    self.emit(OpCode::LoadDeref, idx);
                }
                Some(Binding::Global | Binding::ClassPassthrough) | None => {
                    let idx = self.co.intern_name(name);
                    self.emit(OpCode::LoadGlobal, idx);
                }
            }
            return;
        }
        match binding {
            Some(Binding::Local) => {
                let idx = self.var_index_or_add(name);
                self.emit(OpCode::LoadFast, idx);
            }
            Some(Binding::Cell) | Some(Binding::Nonlocal) => {
                let idx = self.cell_or_free_index(name);
                self.emit(OpCode::LoadDeref, idx);
            }
            Some(Binding::Free) => {
                let idx = self.cell_or_free_index(name);
                // Inside a class body, a free name might shadow a class-local
                // attribute (rare but legal). CPython 3.14 spells the
                // namespace-first load as `LOAD_LOCALS;
                // LOAD_FROM_DICT_OR_DEREF` (codegen_nameop's
                // `LOAD_DEREF` in a class block).
                if self.kind == CodeKind::Class {
                    self.emit(OpCode::LoadLocals, 0);
                    self.emit(OpCode::LoadClassdictOrDeref, idx);
                } else {
                    self.emit(OpCode::LoadDeref, idx);
                }
            }
            Some(Binding::Global) | Some(Binding::ClassPassthrough) | None => {
                let idx = self.co.intern_name(name);
                // GLOBAL_EXPLICIT: `global x` in a class body — or in any
                // scope nested under this module block — loads straight
                // from globals, skipping the local namespace.
                let explicit_global = matches!(binding, Some(Binding::Global))
                    && self.explicit_globals.contains(name);
                if matches!(self.kind, CodeKind::Module | CodeKind::Class) && !explicit_global {
                    self.emit(OpCode::LoadName, idx);
                } else {
                    self.emit(OpCode::LoadGlobal, idx);
                }
            }
        }
    }

    // ---------- expressions ----------

    fn compile_expr(&mut self, e: &Expr) -> Result<(), CompileError> {
        // PEP-657 column tracking: emit this node's instructions under its
        // own source span. Sub-expressions are compiled through this same
        // wrapper, so each restores the parent span on return — leaving
        // `current_span` pointing at *this* node when its own opcode is
        // finally emitted (e.g. the `BinaryOp` after both operands).
        let saved = self.current_span;
        self.set_span(e.span);
        let r = self.compile_expr_inner(e);
        self.current_span = saved;
        r
    }

    fn compile_expr_inner(&mut self, e: &Expr) -> Result<(), CompileError> {
        match &e.kind {
            ExprKind::Constant(c) => {
                let idx = self.co.intern_constant(c.clone().into());
                self.emit(OpCode::LoadConst, idx);
            }
            ExprKind::Name(n) => self.emit_load_name(n),
            ExprKind::BinOp { left, op, right } => {
                self.compile_expr(left)?;
                self.compile_expr(right)?;
                self.emit(OpCode::BinaryOp, bin_op_kind(*op) as u32);
            }
            ExprKind::BoolOp { op, values } => {
                // Short-circuit lowering:
                // and: jump-if-false to end, push value; else discard and recurse
                // or: jump-if-true to end, push value; else discard and recurse
                let jump_op = match op {
                    BoolOp::And => OpCode::PopJumpIfFalse,
                    BoolOp::Or => OpCode::PopJumpIfTrue,
                };
                // `codegen_boolop`: a `JUMP_IF_*` pseudo-op (the
                // flowgraph threads it while it is still a non-popping
                // jump, then lowers it to `COPY 1; TO_BOOL;
                // POP_JUMP_IF_*`) and a `POP_TOP` between operands, all
                // at the boolop's own location.
                let mut jumps = Vec::new();
                let n = values.len();
                for (i, v) in values.iter().enumerate() {
                    self.compile_expr(v)?;
                    if i + 1 < n {
                        self.set_span(e.span);
                        self.emit(OpCode::CopyTop, 1);
                        self.emit(OpCode::ToBool, 0);
                        let j = self.emit(jump_op, 0);
                        self.pseudo_cond_jumps.insert(j);
                        jumps.push(j);
                        self.emit(OpCode::PopTop, 0);
                    }
                }
                let end = self.next_offset();
                for j in jumps {
                    self.patch_jump(j, end);
                }
            }
            ExprKind::UnaryOp { op, operand } => {
                // CPython's AST optimizer folds `not` over an identity /
                // membership test into the inverted operator (`not (x is
                // y)` → `x is not y`), so no UNARY_NOT reaches the
                // bytecode (test_positional_only_arg
                // test_annotations_constant_fold).
                let inverted = if matches!(op, UnaryOp::Not) {
                    match &operand.kind {
                        ExprKind::Compare {
                            left,
                            ops,
                            comparators,
                        } if ops.len() == 1 => match ops[0] {
                            CmpOp::Is => Some((left, CmpOp::IsNot, comparators)),
                            CmpOp::IsNot => Some((left, CmpOp::Is, comparators)),
                            CmpOp::In => Some((left, CmpOp::NotIn, comparators)),
                            CmpOp::NotIn => Some((left, CmpOp::In, comparators)),
                            _ => None,
                        },
                        _ => None,
                    }
                } else {
                    None
                };
                if let Some((left, inv, comparators)) = inverted {
                    self.compile_compare(e, left, &[inv], comparators)?;
                } else {
                    self.compile_expr(operand)?;
                    let kind = match op {
                        UnaryOp::UAdd => UnaryKind::Pos,
                        UnaryOp::USub => UnaryKind::Neg,
                        UnaryOp::Not => UnaryKind::Not,
                        UnaryOp::Invert => UnaryKind::Invert,
                    };
                    // `codegen_visit_expr`: `not x` is `TO_BOOL;
                    // UNARY_NOT` (the flowgraph drops the `TO_BOOL`
                    // after a comparison or another `not`).
                    if matches!(op, UnaryOp::Not) {
                        self.emit(OpCode::ToBool, 0);
                    }
                    self.emit(OpCode::UnaryOp, kind as u32);
                }
            }
            ExprKind::Compare {
                left,
                ops,
                comparators,
            } => {
                self.compile_compare(e, left, ops, comparators)?;
            }
            ExprKind::IfExp { test, body, orelse } => {
                // `codegen_ifexp`.
                let mut to_else = Vec::new();
                self.compile_jump_if(test, false, &mut to_else)?;
                self.compile_expr(body)?;
                let jump_end = self.emit_no_line(OpCode::JumpForward, 0);
                self.no_interrupt_jumps.insert(jump_end);
                let else_target = self.next_offset();
                for j in to_else {
                    self.patch_jump(j, else_target);
                }
                self.compile_expr(orelse)?;
                let end = self.next_offset();
                self.patch_jump(jump_end, end);
            }
            ExprKind::NamedExpr { target, value } => {
                self.compile_expr(value)?;
                self.emit(OpCode::CopyTop, 0);
                self.compile_assign(target)?;
            }
            ExprKind::Lambda { args, body } => {
                // The implicit RETURN_VALUE carries the lambda *body*'s
                // location, not the whole lambda expression's
                // (test_compile's test_lambda_return_position).
                let synthetic = Stmt {
                    kind: StmtKind::Return(Some((**body).clone())),
                    span: body.span,
                };
                // `co_firstlineno` is the *lambda expression's* own line,
                // not the enclosing statement's — `inspect.getsource` of a
                // lambda buried in a multiline initializer must start at
                // the `lambda` token (test_inspect TestOneliners
                // test_lambda_in_list / test_parenthesized_multiline_lambda).
                let lambda_line =
                    Some(self.line_index.line_for(e.span.start.0)).filter(|l| *l != 0);
                self.build_function_object_full(
                    "<lambda>",
                    args,
                    &[synthetic],
                    None,
                    false,
                    lambda_line,
                )?;
            }
            ExprKind::TypeParamFn { args, body } => {
                // A PEP 695 annotation-scope thunk (type-param bound /
                // default, `type` alias value, or a generic alias's
                // parameter binder). Compiles like a lambda, but when
                // it sits (textually) in a class body — or inside
                // another annotation scope that does — its free names
                // resolve through `__classdict__` first.
                let synthetic = Stmt {
                    kind: StmtKind::Return(Some((**body).clone())),
                    span: e.span,
                };
                self.pending_lazy_class_ctx = self.make_lazy_ctx();
                self.build_function_object("<lambda>", args, &[synthetic])?;
            }
            ExprKind::Call {
                func,
                args,
                keywords,
            } => {
                let has_starred = args.iter().any(|a| matches!(a.kind, ExprKind::Starred(_)));
                let has_kw_splat = keywords.iter().any(|k| k.arg.is_none());
                // CPython `maybe_optimize_method_call`: an attribute
                // callable whose base is not import-originated, with no
                // splats and a small operand count, loads via the
                // method-flagged LOAD_ATTR — the receiver rides the wire
                // view's self slot instead of a PUSH_NULL.
                let method_form = !has_starred
                    && !has_kw_splat
                    && args.len() + keywords.len() + usize::from(!keywords.is_empty())
                        < STACK_USE_GUIDELINE
                    && match &func.kind {
                        ExprKind::Attribute { value, .. } => match &value.kind {
                            ExprKind::Name(n) => {
                                !self.params.module_imports.contains(n)
                                    || self.import_flag_shadowed_by_module_comp(n)
                            }
                            _ => true,
                        },
                        _ => false,
                    };
                // Method calls report the method name as the CALL's
                // start location (`maybe_optimize_method_call` adjusts
                // via `update_start_location_to_match_attr`); a plain
                // call sits at the whole Call node.
                let meth = match &func.kind {
                    ExprKind::Attribute { attr, .. } if method_form => {
                        Some((func.span.end.0, attr.len() as u32))
                    }
                    _ => None,
                };
                let emit_call = |c: &mut Self, op: OpCode, arg: u32| match meth {
                    Some((attr_end, attr_len)) => {
                        c.with_attr_location(attr_end, attr_len, |c| {
                            c.emit(op, arg);
                        });
                    }
                    None => {
                        c.emit(op, arg);
                    }
                };
                // `(skip_optimization site, end-jump sites)` of an inlined
                // `all/any/tuple(<genexp>)` guard (see below).
                let mut genexp_opt_rejoin: Option<(u32, Vec<u32>)> = None;
                if method_form {
                    let ExprKind::Attribute { value, attr } = &func.kind else {
                        unreachable!("method_form implies an attribute callable");
                    };
                    if self.super_attr_optimizable(func) {
                        // `super().meth(...)` — the fused method-flagged
                        // LOAD_SUPER_ATTR (CPython LOAD_[ZERO_]SUPER_METHOD).
                        let saved_span = self.current_span;
                        self.set_span(func.span);
                        self.emit_super_attr(func, true)?;
                        self.current_span = saved_span;
                    } else {
                        self.compile_expr(value)?;
                        let idx = self.co.intern_name(attr);
                        // The method load carries the *attribute expression's*
                        // span (not the whole call's), exactly like a plain
                        // `ExprKind::Attribute` visit.
                        let saved_span = self.current_span;
                        self.set_span(func.span);
                        self.with_attr_location(func.span.end.0, attr.len() as u32, |c| {
                            c.emit(OpCode::LoadMethodAttr, idx);
                        });
                        self.current_span = saved_span;
                    }
                } else {
                    self.compile_expr(func)?;
                    // CPython 3.14 `maybe_optimize_function_call`:
                    // `all(<genexp>)` / `any(<genexp>)` / `tuple(<genexp>)`
                    // guard on the callable being the builtin and run the
                    // generator's loop inline, falling back to the plain
                    // call otherwise. Both arms rejoin after the call.
                    let genexp_opt = self.genexp_call_optimization(func, args, keywords);
                    if let Some((const_oparg, genexp)) = genexp_opt {
                        let saved_span = self.current_span;
                        let saved_line = self.current_line;
                        self.set_line_from(func.span.start.0);
                        self.set_span(func.span);
                        let skip = self.emit_genexp_call_optimization(func, genexp, const_oparg)?;
                        self.current_span = saved_span;
                        self.current_line = saved_line;
                        genexp_opt_rejoin = Some(skip);
                    }
                    // The callable's NULL mate, at the callable's own
                    // location (CPython codegen_call: `ADDOP(c,
                    // LOC(func), PUSH_NULL)`).
                    let saved_span = self.current_span;
                    let saved_line = self.current_line;
                    self.set_line_from(func.span.start.0);
                    self.set_span(func.span);
                    let push_null = self.emit(OpCode::PushNull, 0);
                    if let Some((skip_site, _)) = genexp_opt_rejoin {
                        self.patch_jump(skip_site, push_null);
                    }
                    self.current_span = saved_span;
                    self.current_line = saved_line;
                }
                if has_starred
                    || has_kw_splat
                    || args.len() + keywords.len() * 2 > STACK_USE_GUIDELINE
                {
                    // `codegen_call_helper_impl`'s `ex_call`: splats, `**`,
                    // or an operand count over the stack-use guideline
                    // (each keyword weighs double: name + value) go
                    // through CALL_FUNCTION_EX with a positional tuple and
                    // a keyword dict — `co_stacksize` stays O(1)
                    // (test_compile TestExpressionStackSize).
                    self.compile_ex_call_args(args, None, 0)?;
                    if keywords.is_empty() {
                        // CPython 3.14's CALL_FUNCTION_EX always carries a
                        // kwargs slot; a call without keywords pushes NULL.
                        self.emit(OpCode::PushNull, 0);
                    } else {
                        self.compile_ex_call_kwargs(keywords)?;
                    }
                    emit_call(self, OpCode::CallEx, 0);
                } else if keywords.is_empty() {
                    for a in args {
                        self.compile_expr(a)?;
                    }
                    emit_call(self, OpCode::Call, args.len() as u32);
                } else {
                    for a in args {
                        self.compile_expr(a)?;
                    }
                    let mut names: Vec<Constant> = Vec::with_capacity(keywords.len());
                    for k in keywords {
                        let n = k.arg.clone().expect("checked above");
                        names.push(Constant::Str(n));
                        self.compile_expr(&k.value)?;
                    }
                    let tup_idx = self.co.intern_constant(Constant::Tuple(names));
                    if let Some((attr_end, attr_len)) = meth {
                        // `maybe_optimize_method_call` threads the
                        // method attribute's location (`LOC(meth)`,
                        // start-adjusted) into the keyword-names const.
                        let saved_line = self.current_line;
                        let saved_span = self.current_span;
                        self.set_line_from(func.span.start.0);
                        self.set_span(func.span);
                        self.with_attr_location(attr_end, attr_len, |c| {
                            c.emit(OpCode::LoadConst, tup_idx);
                        });
                        self.current_line = saved_line;
                        self.current_span = saved_span;
                    } else {
                        self.emit(OpCode::LoadConst, tup_idx);
                    }
                    emit_call(self, OpCode::CallKw, args.len() as u32);
                }
                if let Some((_, end_jumps)) = genexp_opt_rejoin {
                    // `USE_LABEL(c, skip_normal_call)`: the inlined arms
                    // rejoin past the plain call.
                    let end = self.next_offset();
                    for site in end_jumps {
                        self.patch_jump(site, end);
                    }
                }
            }
            ExprKind::Attribute { value, attr } => {
                if self.super_attr_optimizable(e) {
                    self.emit_super_attr(e, false)?;
                } else {
                    self.compile_expr(value)?;
                    let idx = self.co.intern_name(attr);
                    self.with_attr_location(e.span.end.0, attr.len() as u32, |c| {
                        c.emit(OpCode::LoadAttr, idx);
                    });
                }
            }
            ExprKind::Subscript { value, slice } => {
                self.compile_expr(value)?;
                // CPython 3.14 `codegen_subscript`: a non-constant
                // two-element slice skips the slice object entirely
                // (`BINARY_SLICE`); everything else subscripts with the
                // materialized key (`BINARY_OP NB_SUBSCR`).
                if should_apply_two_element_slice_optimization(slice) {
                    self.compile_slice_two_parts(slice)?;
                    self.emit(OpCode::BinarySlice, 0);
                } else {
                    self.compile_expr(slice)?;
                    self.emit(OpCode::BinarySubscr, 0);
                }
            }
            ExprKind::Slice { lower, upper, step } => {
                // CPython 3.14 `codegen_slice`: an all-constant slice is
                // folded into a single `slice` constant (`a[1:2]` loads
                // `slice(1, 2, None)` from `co_consts`); otherwise the
                // two bounds (defaulting to None) plus the optional step
                // feed `BUILD_SLICE 2|3`.
                if let Some(folded) = constant_slice(lower, upper, step) {
                    let idx = self.co.intern_constant(folded);
                    self.emit(OpCode::LoadConst, idx);
                } else {
                    self.compile_slice_two_parts(e)?;
                    if let Some(st) = step {
                        self.compile_expr(st)?;
                        self.emit(OpCode::BuildSlice, 3);
                    } else {
                        self.emit(OpCode::BuildSlice, 2);
                    }
                }
            }
            ExprKind::Tuple(items) => {
                // `codegen_tuple`: `BUILD_TUPLE n` (through a list when
                // a splat or the stack-use guideline calls for it). An
                // all-constant display folds into one `LoadConst` in
                // the flowgraph (`fold_tuple_of_constants`), which
                // also puts the folded tuple's `co_consts` slot after
                // every codegen-time constant.
                self.starunpack_helper(
                    items,
                    None,
                    0,
                    OpCode::BuildList,
                    OpCode::ListAppend,
                    OpCode::ListExtend,
                    true,
                )?;
            }
            ExprKind::List(items) => {
                self.starunpack_helper(
                    items,
                    None,
                    0,
                    OpCode::BuildList,
                    OpCode::ListAppend,
                    OpCode::ListExtend,
                    false,
                )?;
            }
            ExprKind::Set(items) => {
                self.starunpack_helper(
                    items,
                    None,
                    0,
                    OpCode::BuildSet,
                    OpCode::SetAdd,
                    OpCode::SetUpdate,
                    false,
                )?;
            }
            ExprKind::Dict { keys, values } => {
                self.compile_dict(keys, values)?;
            }
            ExprKind::ListComp { elt, generators }
            | ExprKind::SetComp { elt, generators }
            | ExprKind::GeneratorExp { elt, generators } => {
                let kind = match &e.kind {
                    ExprKind::ListComp { .. } => CompKind::List,
                    ExprKind::SetComp { .. } => CompKind::Set,
                    ExprKind::GeneratorExp { .. } => CompKind::Generator,
                    _ => unreachable!(),
                };
                self.compile_comprehension(kind, elt, None, generators)?;
            }
            ExprKind::DictComp {
                key,
                value,
                generators,
            } => {
                self.compile_comprehension(CompKind::Dict, key, Some(value), generators)?;
            }
            ExprKind::Starred(_) => {
                return Err(CompileError::spanned(
                    "can't use starred expression here",
                    e.span,
                ));
            }
            ExprKind::JoinedStr(parts) => {
                self.compile_joined_str(parts)?;
            }
            ExprKind::FormattedValue {
                value,
                conversion,
                format_spec,
            } => {
                self.compile_formatted_value(value, *conversion, format_spec.as_deref())?;
            }
            ExprKind::TemplateStr(parts) => {
                self.compile_template_str(parts)?;
            }
            // Only reachable inside a `TemplateStr` (handled there); a
            // bare node can only come from a caller-built `ast` tree.
            ExprKind::Interpolation { .. } => {
                return Err(CompileError::spanned(
                    "t-string interpolation outside t-string",
                    e.span,
                ));
            }
            ExprKind::Yield(value) => {
                // `yield` is only legal in a function body. At module or
                // class scope (or inside a comprehension's own frame) it is
                // a SyntaxError — CPython reports "'yield' outside function"
                // (or "'yield' inside list comprehension" etc.). Catching it
                // here also prevents a non-generator frame from ever
                // executing `YIELD_VALUE` at runtime.
                if self.kind != CodeKind::Function {
                    return Err(self.yield_placement_error("yield", e.span));
                }
                if let Some(v) = value {
                    self.compile_expr(v)?;
                } else {
                    let idx = self.co.intern_constant(Constant::None);
                    self.emit(OpCode::LoadConst, idx);
                }
                // CPython 3.13 own-yield shape. An async generator's *own*
                // `yield` produces a value for the consumer (`__anext__`),
                // distinct from the `YIELD_VALUE 1` the `await`/`yield from`
                // dance emits to pass an inner suspension's value through;
                // the ASYNC_GEN_WRAP intrinsic marks it (CPython's
                // `PyAsyncGenWrappedValue`). The RESUME lands the sent
                // value on the stack — it is this expression's result.
                if self.co.is_async_generator {
                    self.emit(OpCode::AsyncGenWrap, 0);
                }
                self.emit(OpCode::YieldValue, 0);
                self.emit(OpCode::Resume, 1);
            }
            ExprKind::YieldFrom(iter) => {
                if self.kind != CodeKind::Function {
                    return Err(self.yield_placement_error("yield from", e.span));
                }
                // PEP 525: `yield from` is forbidden in `async def`
                // (only plain `yield` makes an async generator).
                if self.in_async_context() {
                    return Err(CompileError::spanned(
                        "'yield from' inside async function",
                        e.span,
                    ));
                }
                self.compile_expr(iter)?;
                self.emit(OpCode::GetYieldFromIter, 0);
                // RESUME 2 = after `yield from` (CPython
                // RESUME_AFTER_YIELD_FROM).
                self.emit_send_dance(2);
            }
            ExprKind::Await(value) => {
                if !self.in_async_context() {
                    if self.allows_top_level_await() {
                        // PyCF_ALLOW_TOP_LEVEL_AWAIT: the module code
                        // becomes a coroutine (CPython marks it
                        // CO_COROUTINE) — the asyncio REPL contract.
                        self.co.is_coroutine = true;
                    } else {
                        return Err(CompileError::spanned(
                            if self.kind == CodeKind::Function {
                                "'await' outside async function"
                            } else {
                                "'await' outside function"
                            },
                            e.span,
                        ));
                    }
                }
                self.compile_expr(value)?;
                self.compile_await_dance(0);
            }
        }
        Ok(())
    }

    /// CPython's symtable wording for a misplaced `yield` / `yield from`:
    /// inside a comprehension scope the message names the comprehension
    /// form, otherwise it's "outside function".
    fn yield_placement_error(&self, kw: &str, span: weavepy_lexer::Span) -> CompileError {
        // CPython's symtable always says "'yield' inside …" for the
        // comprehension case, even for `yield from`; only the
        // "outside function" form names `yield from` distinctly.
        let msg = match self.comp_kind {
            Some(CompKind::List) => "'yield' inside list comprehension".to_owned(),
            Some(CompKind::Set) => "'yield' inside set comprehension".to_owned(),
            Some(CompKind::Dict) => "'yield' inside dict comprehension".to_owned(),
            Some(CompKind::Generator) => "'yield' inside generator expression".to_owned(),
            None => format!("'{kw}' outside function"),
        };
        CompileError::spanned(msg, span)
    }

    /// Emit the "drive awaitable to completion" instruction sequence
    /// CPython 3.13 uses for `await`. Stack on entry: `[awaitable]`;
    /// stack on exit: `[result]`. `awaitable_arg` is passed to
    /// `GET_AWAITABLE` and selects the error message (CPython's
    /// numbering): 0 = plain `await`, 1 = `async with`'s `__aenter__`
    /// result, 2 = its `__aexit__` result. `async for` uses no
    /// GET_AWAITABLE — GET_ANEXT coerces its own result.
    fn compile_await_dance(&mut self, awaitable_arg: u32) {
        self.emit(OpCode::GetAwaitable, awaitable_arg);
        // RESUME 3 = after `await` (CPython RESUME_AFTER_AWAIT).
        self.emit_send_dance(3);
    }

    /// CPython 3.13 `codegen_add_yield_from`: drive the iterator at TOS
    /// to completion. Stack on entry: `[iter]`; on exit: `[result]`.
    ///
    ///   LOAD_CONST None
    /// send: SEND -> exit        ; pushes yielded value, or jumps with
    ///                           ; [iter, retval] on StopIteration
    ///   YIELD_VALUE 1           ; passthrough suspension
    ///   RESUME resume_arg       ; sent value lands at TOS
    ///   JUMP_BACKWARD_NO_INTERRUPT -> send
    /// fail: CLEANUP_THROW       ; StopIteration from a throw()/close()
    ///                           ; injected at the YIELD -> [None, value]
    /// exit: END_SEND            ; [iter, value] -> [value]
    ///
    /// The virtual try around the YIELD_VALUE targets `fail`
    /// (CLEANUP_THROW); the cold-block pass in `finish` moves that
    /// block to the stream tail with an explicit rejoin jump, exactly
    /// like CPython's `push_cold_blocks_to_end`.
    fn emit_send_dance(&mut self, resume_arg: u32) {
        let none_idx = self.co.intern_constant(Constant::None);
        self.emit(OpCode::LoadConst, none_idx);
        let loop_start = self.use_label();
        let send = self.emit(OpCode::Send, 0);
        // The virtual `SETUP_FINALLY fail` / `POP_BLOCK` around the
        // YIELD_VALUE: the stand-ins carry the handler's entry depth
        // for `calculate_stackdepth` (`co_stacksize` counts the
        // CLEANUP_THROW block's three slots).
        self.emit_setup(OpCode::SetupFinally);
        let yield_at = self.emit(OpCode::YieldValue, 1);
        self.emit_pop_block_no_line();
        self.emit(OpCode::Resume, resume_arg);
        let back = self.emit(OpCode::JumpBackward, 0);
        self.patch_jump(back, loop_start);
        let fail = self.emit(OpCode::CleanupThrow, 0);
        let end = self.next_offset();
        self.patch_jump(send, end);
        // Stack: [iter, value]. Drop the iterator, keep the value.
        self.emit(OpCode::EndSend, 0);
        // Covered range is exactly the YIELD_VALUE; the depth resolves
        // statically at that instruction ([.., iter, value] kept).
        self.co.exception_table.push(ExcHandler {
            start: yield_at,
            end: yield_at + 1,
            handler: fail,
            depth: HANDLER_DEPTH_SENTINEL,
            push_lasti: false,
        });
    }

    /// `True` if the current code object is the body of an `async def`
    /// (coroutine or async-generator). Comprehensions inherit their
    /// parent's flavour because they compile a synthetic function;
    /// we conservatively let async-flavoured comprehensions through
    /// at the parse layer and rely on the synthetic function being
    /// produced with the right flag.
    fn in_async_context(&self) -> bool {
        self.co.is_coroutine || self.co.is_async_generator
    }

    /// PyCF_ALLOW_TOP_LEVEL_AWAIT (RFC 0052): `await` / `async for` /
    /// `async with` are legal at module top level; using one turns the
    /// module code object into a coroutine.
    fn allows_top_level_await(&self) -> bool {
        self.params.allow_top_level_await && matches!(self.kind, CodeKind::Module)
    }

    fn compile_async_for(
        &mut self,
        target: &Expr,
        iter: &Expr,
        body: &[Stmt],
        orelse: &[Stmt],
    ) -> Result<(), CompileError> {
        let stmt_line = self.current_line;
        let stmt_span = self.current_span;
        self.compile_expr(iter)?;
        // `GET_AITER` (and the closing `END_ASYNC_FOR`) sit at the
        // iterator expression's location (`codegen_async_for`).
        self.set_span(iter.span);
        self.emit(OpCode::GetAiter, 0);
        self.current_span = stmt_span;
        // `start` label, then `SETUP_FINALLY except` guarding the
        // `__anext__` await.
        let loop_top = self.use_label();
        self.emit_setup(OpCode::SetupFinally);
        // GetAnext peeks the aiter and pushes a *coerced* awaitable
        // (CPython's GET_ANEXT applies `_PyCoro_GetAwaitableIter`
        // itself — no GET_AWAITABLE in the async-for dance). The
        // send-dance drives it; on success we land at the STORE_FAST
        // target. On StopAsyncIteration, control flows to the cleanup
        // block.
        let guard_start = self.next_offset();
        self.emit(OpCode::GetAnext, 0);
        self.emit_send_dance(3);
        // The StopAsyncIteration window closes here: only the
        // `__anext__` await may end the loop. An exception raised by
        // the assignment target or the body — even a
        // StopAsyncIteration — propagates (bpo-44895). CPython's
        // `POP_BLOCK` here carries the statement's location.
        let dance_end = self.next_offset();
        self.emit_pop_block();
        // CPython 3.14's codegen_async_for places a `NOT_TAKEN`
        // instrumentation anchor on the success path, just past the
        // `__anext__` coverage window (it is the branch-right target
        // of END_ASYNC_FOR for `sys.monitoring`).
        self.emit(OpCode::NotTaken, 0);
        // Stack: [aiter, value]. Move the value into the target.
        self.compile_assign(target)?;
        let seq = self.next_fblock_seq();
        self.loop_stack.push(LoopFrame {
            continue_target: loop_top,
            break_sites: Vec::new(),
            is_for_loop: true,
            exc_on_stack_at_entry: self.exc_on_stack,
            pending_retvals_at_entry: self.pending_retvals,
            seq,
        });
        for s in body {
            self.compile_stmt(s)?;
        }
        // CPython emits the async-for loop-closing jump NO_LOCATION
        // (codegen_async_for) — it must not trace whatever line the
        // body ended on (test_async_for_backwards_jump_has_no_line).
        let back = self.emit_no_line(OpCode::JumpBackward, 0);
        self.patch_jump(back, loop_top);
        let frame = self.loop_stack.pop().expect("loop frame");
        // Register an exception-table handler covering only the
        // `__anext__` await (loop header) so its `StopAsyncIteration`
        // lands at the cleanup label; body exceptions propagate. The
        // aiter stays at stack depth 1 across the whole loop body —
        // every per-iteration push lives above it.
        let cleanup_target = self.next_offset();
        // Sentinel depth: resolved by `finish` to the static stack
        // depth at `loop_top`, which preserves every enclosing loop's
        // iterator, the aiter itself, and any pinned exception slots.
        self.co.exception_table.push(ExcHandler {
            start: guard_start,
            end: dance_end,
            handler: cleanup_target,
            depth: HANDLER_DEPTH_SENTINEL,
            push_lasti: false,
        });
        // Cleanup: pop aiter + exception, then run the `else` clause.
        // Located on the iterator expression (CPython: "the
        // END_ASYNC_FOR succeeds the `for`, not the body"): exhaustion
        // — and an implicit function-ending return after it — report
        // the loop's line, not the body's last line.
        self.current_line = stmt_line;
        self.set_span(iter.span);
        // CPython 3.14's END_ASYNC_FOR carries a backward oparg to the
        // dance's END_SEND (`dis` renders `(from Lx)`); the wire codec
        // derives it from this handler's coverage end, so the internal
        // arg stays 0.
        self.emit(OpCode::EndAsyncFor, 0);
        for s in orelse {
            self.compile_stmt(s)?;
        }
        let break_target = self.next_offset();
        for site in frame.break_sites {
            self.patch_jump(site, break_target);
        }
        Ok(())
    }

    fn compile_async_with(
        &mut self,
        items: &[WithItem],
        body: &[Stmt],
    ) -> Result<(), CompileError> {
        if items.is_empty() {
            return Ok(());
        }
        let (head, rest) = items.split_first().expect("nonempty");
        // See `compile_with`: the whole setup/exit dance is attributed
        // to this item's context-manager expression (PEP 657).
        self.set_line_from(head.context_expr.span.start.0);
        self.set_span(head.context_expr.span);
        let with_line = self.current_line;
        let with_span = self.current_span;
        self.compile_expr(&head.context_expr)?;
        self.current_line = with_line;
        self.current_span = with_span;
        // The LOAD_SPECIAL dance leaves [aexit_func, aexit_self,
        // awaitable(aenter)]. The `__aexit__` pair stays on the operand
        // stack for the whole body — the async counterpart of
        // `compile_with`'s SETUP_WITH shape.
        self.emit_with_setup(SPECIAL_AEXIT, SPECIAL_AENTER);
        self.compile_await_dance(1);
        // `SETUP_WITH final` at the item's location, then the `block`
        // label: coverage starts at the bind (or POP_TOP) of the
        // awaited `__aenter__` result.
        self.emit_setup(OpCode::SetupWith);
        let cover_start = self.use_label();
        if let Some(target) = &head.optional_vars {
            self.compile_assign(target)?;
        } else {
            self.emit(OpCode::PopTop, 0);
        }
        // Coverage starts one slot above the body baseline (the
        // `__aenter__` result rides over `__aexit__`): anchor the
        // handler depth at the first body instruction.
        let depth_anchor = self.next_offset();

        // Synthetic finally frame so `return`/`break`/`continue` out of
        // the body still `await __aexit__(None, None, None)`. Mirrors the
        // `WithExit` frame `compile_with` pushes; without it an early exit
        // from an `async with` body skipped the exit entirely (e.g. an
        // `@asynccontextmanager` used as a decorator never ran its
        // post-`yield` cleanup).
        let awith_loop_depth = self.loop_stack.len();
        let awith_frame_id = self.fresh_finally_id();
        let seq = self.next_fblock_seq();
        self.finally_stack.push(FinallyFrame {
            kind: FinallyKind::AsyncWithExit {
                line: with_line,
                span: with_span,
            },
            loop_depth_at_push: awith_loop_depth,
            id: awith_frame_id,
            pop_except_after: false,
            region_hole_id: 0,
            exc_at_push: self.exc_on_stack,
            handler_at_push: self.handler_depth,
            rv_at_push: self.pending_retvals,
            seq,
        });

        let body_start = cover_start;
        let body_result = if rest.is_empty() {
            body.iter().try_for_each(|s| self.compile_stmt(s))
        } else {
            self.compile_async_with(rest, body)
        };
        body_result?;
        let body_end = self.next_offset();

        // Pop the synthetic frame; the explicit normal-exit and
        // exception-cleanup paths below emit their own `__aexit__` call.
        self.finally_stack.pop();

        // Attribute the whole exit path to the `async with` line.
        // Unlike the sync form, `codegen_async_with_inner` locates the
        // closing `POP_BLOCK`.
        self.current_line = with_line;
        self.current_span = with_span;
        self.emit_pop_block();

        // Normal exit: the `__aexit__` pair is on the stack; `await
        // aexit(None, None, None)` (wire `CALL 3`).
        self.emit_call_exit_with_nones();
        self.compile_await_dance(2);
        self.emit(OpCode::PopTop, 0);
        let end_jump = self.emit(OpCode::JumpForward, 0);
        self.plain_jumps.insert(end_jump);

        // Exception handler — the async counterpart of `compile_with`'s:
        //   final: SETUP_CLEANUP; PUSH_EXC_INFO; WITH_EXCEPT_START;
        //          <await>; TO_BOOL; POP_JUMP_IF_TRUE suppress; RERAISE 2
        //   suppress: POP_TOP; POP_BLOCK; POP_EXCEPT; POP_TOP x3;
        //             JUMP_NO_INTERRUPT exit
        //   cleanup: COPY 3; POP_EXCEPT; RERAISE 1
        let final_label = self.use_label();
        self.emit_setup(OpCode::SetupCleanup);
        let handler_start = self.next_offset();
        // Same lasti semantics as the sync `with` cleanup. Punch out the
        // body's `return`/`break`/`continue`-path `await __aexit__(None,
        // None, None)` inline so a `raise` from it isn't re-caught and
        // `__aexit__` re-awaited with the exception triple. Depth is
        // anchored at the body baseline (keeps the on-stack `__aexit__`).
        self.push_body_exc_entries(
            body_start,
            body_end,
            final_label,
            HANDLER_DEPTH_ANCHOR_FLAG | depth_anchor,
            true,
            Some(awith_frame_id),
        );
        // Entry stack: [aexit_func, aexit_self, lasti, exc]. Record the
        // propagating exception as the active handled exception for the
        // duration of the awaited `__aexit__` so a `raise` inside it
        // chains as the new exception's implicit `__context__`
        // (PEP 3134) — `contextlib.AsyncExitStack`'s
        // `_fix_exception_context` walks each callback exception's
        // context back to `sys.exc_info()[1]`. After PUSH_EXC_INFO:
        // [aexit_func, aexit_self, lasti, prev, exc].
        let push_exc_site = self.emit(OpCode::PushExcInfo, 0);
        // Calls `__aexit__(type(exc), exc, exc.__traceback__)` peeking
        // the exit pair at depths 5 and 4; pushes the coroutine, which
        // the dance awaits into the suppress flag.
        self.emit(OpCode::WithExceptStart, 0);
        self.compile_await_dance(2);
        let cleanup_cover_end = self.emit_with_except_finish();
        let cleanup_start = self.next_offset();
        self.emit_no_line(OpCode::CopyTop, 3);
        self.emit_no_line(OpCode::PopExcept, 0);
        self.emit_no_line(OpCode::Reraise, 1);
        self.co.exception_table.push(ExcHandler {
            start: handler_start,
            end: cleanup_cover_end,
            handler: cleanup_start,
            depth: HANDLER_DEPTH_SENTINEL,
            push_lasti: true,
        });
        let end = self.next_offset();
        self.patch_jump(end_jump, end);
        for site in std::mem::take(&mut self.with_exit_jumps) {
            self.patch_jump(site, end);
        }
        // Tag the active-handler entry with the pc just past the whole
        // handler region: the suppress path's POP_EXCEPT (or the cleanup
        // tail's) pops it; an escape beyond `end` drops it in the
        // unwinder.
        self.co.instructions[push_exc_site as usize].arg = end;
        Ok(())
    }

    /// CPython `codegen_with_except_finish` up to the `cleanup` label,
    /// all `NO_LOCATION`: `TO_BOOL; POP_JUMP_IF_TRUE suppress; RERAISE 2;
    /// suppress: POP_TOP; POP_BLOCK; POP_EXCEPT; POP_TOP; POP_TOP;
    /// POP_TOP; JUMP_NO_INTERRUPT exit`. Returns the offset where the
    /// cleanup entry's coverage ends (the `POP_BLOCK`); the exit jump
    /// is parked in `with_exit_jumps` for the caller to patch.
    fn emit_with_except_finish(&mut self) -> u32 {
        self.emit_no_line(OpCode::ToBool, 0);
        let suppress = self.emit_no_line(OpCode::PopJumpIfTrue, 0);
        // Falsy: re-raise `exc` (TOS), restoring f_lasti from the slot
        // two below it (CPython RERAISE 2) — no entry is recorded for
        // the re-raise site and the original traceback is preserved.
        self.emit_no_line(OpCode::Reraise, 2);
        let suppress_target = self.next_offset();
        self.patch_jump(suppress, suppress_target);
        // Suppressed: [exit_func, exit_self, lasti, prev, exc] — drain.
        self.emit_no_line(OpCode::PopTop, 0);
        // The cleanup entry's coverage ends here: the `POP_BLOCK` pops
        // it and the drains below cannot raise.
        let cleanup_cover_end = self.next_offset();
        self.emit_pop_block_no_line();
        self.emit_no_line(OpCode::PopExcept, 0);
        self.emit_no_line(OpCode::PopTop, 0);
        self.emit_no_line(OpCode::PopTop, 0);
        self.emit_no_line(OpCode::PopTop, 0);
        let exit = self.emit_no_line(OpCode::JumpForward, 0);
        self.no_interrupt_jumps.insert(exit);
        self.with_exit_jumps.push(exit);
        cleanup_cover_end
    }

    /// Lower an `f"..."` literal into a chain of `FORMAT_VALUE` /
    /// `BUILD_STRING` instructions. Plain `Constant::Str` parts are
    /// pushed as-is; `FormattedValue` parts go through the format
    /// machinery.
    fn compile_joined_str(&mut self, parts: &[Expr]) -> Result<(), CompileError> {
        if parts.is_empty() {
            let idx = self.co.intern_constant(Constant::Str(String::new()));
            self.emit(OpCode::LoadConst, idx);
            return Ok(());
        }
        if parts.len() == 1 {
            return self.compile_expr(&parts[0]);
        }
        for p in parts {
            self.compile_expr(p)?;
        }
        self.emit(OpCode::BuildString, parts.len() as u32);
        Ok(())
    }

    /// PEP 750 t-string (`-X lang=next`, RFC 0076 WS15). Lowers to
    /// CPython 3.14's opcode shape:
    ///
    /// ```text
    /// LOAD_CONST s0 … LOAD_CONST sn; BUILD_TUPLE n+1     (strings)
    /// per field: value; LOAD_CONST expr_text; [spec];
    ///            BUILD_INTERPOLATION conv|spec_flag
    /// BUILD_TUPLE n                                      (interpolations)
    /// BUILD_TEMPLATE
    /// ```
    ///
    /// The `strings` tuple is the PEP 750 canonical form: exactly
    /// `n_interpolations + 1` entries, with `""` fillers between
    /// adjacent fields and at the ends.
    fn compile_template_str(&mut self, parts: &[Expr]) -> Result<(), CompileError> {
        // Code-point accumulation so surrogate-bearing (`WStr`) literal
        // fragments survive concatenation, as in `parse_string_concat`.
        fn cps_of(c: &AstConstant) -> Vec<u32> {
            match c {
                AstConstant::Str(s) => s.chars().map(|ch| ch as u32).collect(),
                AstConstant::WStr(cps) => cps.clone(),
                _ => unreachable!("checked by caller"),
            }
        }
        fn cps_to_const(cps: Vec<u32>) -> Constant {
            if cps.iter().all(|&cp| char::from_u32(cp).is_some()) {
                Constant::Str(
                    cps.iter()
                        .map(|&cp| char::from_u32(cp).expect("checked"))
                        .collect(),
                )
            } else {
                Constant::WStr(cps)
            }
        }
        let mut strings: Vec<Constant> = Vec::new();
        let mut interps: Vec<&Expr> = Vec::new();
        let mut cur: Vec<u32> = Vec::new();
        for p in parts {
            match &p.kind {
                ExprKind::Constant(c @ (AstConstant::Str(_) | AstConstant::WStr(_))) => {
                    cur.extend(cps_of(c));
                }
                ExprKind::Interpolation { .. } => {
                    strings.push(cps_to_const(std::mem::take(&mut cur)));
                    interps.push(p);
                }
                _ => {
                    return Err(CompileError::spanned("invalid t-string part", p.span));
                }
            }
        }
        strings.push(cps_to_const(cur));
        for s in strings.iter().cloned() {
            let idx = self.co.intern_constant(s);
            self.emit(OpCode::LoadConst, idx);
        }
        self.emit(OpCode::BuildTuple, strings.len() as u32);
        // `codegen_interpolation`: the expression text constant and
        // `BUILD_INTERPOLATION` sit at the `Interpolation` node's own
        // location (`{expr!r:spec}` with its braces); the template's
        // tuple and `BUILD_TEMPLATE` return to the t-string's.
        let template_span = self.current_span;
        let template_line = self.current_line;
        for p in &interps {
            let ExprKind::Interpolation {
                value,
                text,
                conversion,
                format_spec,
            } = &p.kind
            else {
                unreachable!("collected above");
            };
            self.compile_expr(value)?;
            self.set_line_from(p.span.start.0);
            self.set_span(p.span);
            let idx = self.co.intern_constant(Constant::Str(text.clone()));
            self.emit(OpCode::LoadConst, idx);
            let conv: u32 = match *conversion {
                -1 => 0,
                115 => 1, // 's'
                114 => 2, // 'r'
                97 => 3,  // 'a'
                other => {
                    return Err(CompileError::internal(format!(
                        "unknown t-string conversion {other}"
                    )));
                }
            };
            // CPython's `codegen_interpolation` oparg layout: bit 1 is
            // always set (the base `2`), bit 0 flags a format spec on the
            // stack, and the FVC_* conversion sits in bits 2+.
            let mut arg = 2 | (conv << 2);
            if let Some(spec) = format_spec {
                // The spec is a JoinedStr, eagerly evaluated to a str at
                // template construction time (PEP 750).
                self.compile_expr(spec)?;
                self.set_line_from(p.span.start.0);
                self.set_span(p.span);
                arg |= 1;
            }
            self.emit(OpCode::BuildInterpolation, arg);
        }
        self.current_span = template_span;
        self.current_line = template_line;
        self.emit(OpCode::BuildTuple, interps.len() as u32);
        self.emit(OpCode::BuildTemplate, 0);
        Ok(())
    }

    /// Emit `value` then `FORMAT_VALUE arg`. Encoding:
    /// bits 0-1: conversion (`0` = none, `1` = !s, `2` = !r, `3` = !a)
    /// bit 2: spec-on-stack flag (the spec is below the value).
    fn compile_formatted_value(
        &mut self,
        value: &Expr,
        conversion: i32,
        spec: Option<&Expr>,
    ) -> Result<(), CompileError> {
        self.compile_expr(value)?;
        let conv: u32 = match conversion {
            -1 => 0,
            115 => 1, // 's'
            114 => 2, // 'r'
            97 => 3,  // 'a'
            other => {
                return Err(CompileError::internal(format!(
                    "unknown f-string conversion {other}"
                )));
            }
        };
        // CPython 3.13 shape: the conversion is its own CONVERT_VALUE
        // instruction, then the spec (if any), then FORMAT_SIMPLE /
        // FORMAT_WITH_SPEC (the internal FormatValue keeps only the
        // spec-on-stack bit).
        if conv != 0 {
            self.emit(OpCode::ConvertValue, conv);
        }
        let mut arg: u32 = 0;
        if let Some(spec_expr) = spec {
            self.compile_expr(spec_expr)?;
            arg |= 0x04;
        }
        self.emit(OpCode::FormatValue, arg);
        Ok(())
    }

    /// `codegen_compare`: a single comparison is `left; right;
    /// COMPARE_OP`; a chain `a OP1 b OP2 c` keeps the shared operand on
    /// the stack with `SWAP 2; COPY 2` and short-circuits through a
    /// `cleanup` block that drops it (`SWAP 2; POP_TOP`), leaving the
    /// first false result as the value.
    fn compile_compare(
        &mut self,
        e: &Expr,
        left: &Expr,
        ops: &[CmpOp],
        comparators: &[Expr],
    ) -> Result<(), CompileError> {
        self.compile_expr(left)?;
        let n = ops.len() - 1;
        if n == 0 {
            self.compile_expr(&comparators[0])?;
            self.set_span(e.span);
            emit_cmp_op(self, ops[0]);
            return Ok(());
        }
        let mut cleanup_jumps = Vec::new();
        for i in 0..n {
            self.compile_expr(&comparators[i])?;
            self.set_span(e.span);
            self.emit(OpCode::Swap, 2);
            self.emit(OpCode::CopyTop, 2);
            emit_cmp_op(self, ops[i]);
            self.emit(OpCode::CopyTop, 1);
            self.emit(OpCode::ToBool, 0);
            cleanup_jumps.push(self.emit(OpCode::PopJumpIfFalse, 0));
            self.emit(OpCode::PopTop, 0);
        }
        self.compile_expr(&comparators[n])?;
        self.set_span(e.span);
        emit_cmp_op(self, ops[n]);
        let end_jump = self.emit_no_line(OpCode::JumpForward, 0);
        self.no_interrupt_jumps.insert(end_jump);
        let cleanup = self.next_offset();
        for j in cleanup_jumps {
            self.patch_jump(j, cleanup);
        }
        self.set_span(e.span);
        self.emit(OpCode::Swap, 2);
        self.emit(OpCode::PopTop, 0);
        let end = self.next_offset();
        self.patch_jump(end_jump, end);
        Ok(())
    }

    /// `codegen_jump_if`: compile `e` as a branch condition that jumps
    /// to the caller's `next` label when its truth value equals `cond`.
    /// Every jump site aimed at `next` is appended to `to_next`; the
    /// caller patches them once the label's offset is known. `not`
    /// flips the sense, `and`/`or` chains short-circuit without
    /// materialising a value, a conditional expression branches on each
    /// arm, and a comparison chain shares its middle operands via
    /// `SWAP 2; COPY 2`. Everything else is `<expr>; TO_BOOL;
    /// POP_JUMP_IF_*` -- the flowgraph folds the `TO_BOOL` away after
    /// comparisons, `is`/`in`, `not`, and constants exactly as CPython's
    /// `optimize_basic_block` does.
    fn compile_jump_if(
        &mut self,
        e: &Expr,
        cond: bool,
        to_next: &mut Vec<u32>,
    ) -> Result<(), CompileError> {
        match &e.kind {
            ExprKind::UnaryOp {
                op: UnaryOp::Not,
                operand,
            } => return self.compile_jump_if(operand, !cond, to_next),
            ExprKind::BoolOp { op, values } => {
                let n = values.len() - 1;
                let cond2 = matches!(op, BoolOp::Or);
                let mut next2: Vec<u32> = Vec::new();
                for v in &values[..n] {
                    if cond2 == cond {
                        self.compile_jump_if(v, cond2, to_next)?;
                    } else {
                        self.compile_jump_if(v, cond2, &mut next2)?;
                    }
                }
                self.compile_jump_if(&values[n], cond, to_next)?;
                let here = self.next_offset();
                for j in next2 {
                    self.patch_jump(j, here);
                }
                return Ok(());
            }
            ExprKind::IfExp { test, body, orelse } => {
                let mut next2: Vec<u32> = Vec::new();
                self.compile_jump_if(test, false, &mut next2)?;
                self.compile_jump_if(body, cond, to_next)?;
                let end = self.emit_no_line(OpCode::JumpForward, 0);
                self.no_interrupt_jumps.insert(end);
                let here = self.next_offset();
                for j in next2 {
                    self.patch_jump(j, here);
                }
                self.compile_jump_if(orelse, cond, to_next)?;
                let here = self.next_offset();
                self.patch_jump(end, here);
                return Ok(());
            }
            ExprKind::Compare {
                left,
                ops,
                comparators,
            } if ops.len() > 1 => {
                let saved = self.current_span;
                self.compile_expr(left)?;
                let n = ops.len() - 1;
                let mut cleanup_jumps = Vec::new();
                for i in 0..n {
                    self.compile_expr(&comparators[i])?;
                    self.set_span(e.span);
                    self.emit(OpCode::Swap, 2);
                    self.emit(OpCode::CopyTop, 2);
                    emit_cmp_op(self, ops[i]);
                    self.emit(OpCode::ToBool, 0);
                    cleanup_jumps.push(self.emit(OpCode::PopJumpIfFalse, 0));
                }
                self.compile_expr(&comparators[n])?;
                self.set_span(e.span);
                emit_cmp_op(self, ops[n]);
                self.emit(OpCode::ToBool, 0);
                to_next.push(self.emit(
                    if cond {
                        OpCode::PopJumpIfTrue
                    } else {
                        OpCode::PopJumpIfFalse
                    },
                    0,
                ));
                let end = self.emit_no_line(OpCode::JumpForward, 0);
                self.no_interrupt_jumps.insert(end);
                let cleanup = self.next_offset();
                for j in cleanup_jumps {
                    self.patch_jump(j, cleanup);
                }
                self.set_span(e.span);
                self.emit(OpCode::PopTop, 0);
                if !cond {
                    let j = self.emit_no_line(OpCode::JumpForward, 0);
                    self.no_interrupt_jumps.insert(j);
                    to_next.push(j);
                }
                let here = self.next_offset();
                self.patch_jump(end, here);
                self.current_span = saved;
                return Ok(());
            }
            _ => {}
        }
        self.compile_expr(e)?;
        let saved = self.current_span;
        self.set_span(e.span);
        self.emit(OpCode::ToBool, 0);
        to_next.push(self.emit(
            if cond {
                OpCode::PopJumpIfTrue
            } else {
                OpCode::PopJumpIfFalse
            },
            0,
        ));
        self.current_span = saved;
        Ok(())
    }

    // ---------- comprehensions ----------

    /// Whether this comprehension takes the PEP 709 inlined lowering
    /// (`ste_comp_inlined`): every list/set/dict comprehension does,
    /// except inside a scope that can see a class namespace (a PEP 695
    /// annotation scope, whose name loads go through `__classdict__`
    /// while a comprehension's must skip the class), plus the two
    /// shapes the nested-function path exists to diagnose (`yield` in
    /// the comprehension, an async comprehension in a sync scope).
    fn comp_inline_eligible(
        &self,
        kind: CompKind,
        elt: &Expr,
        value: Option<&Expr>,
        generators: &[Comprehension],
    ) -> bool {
        if matches!(kind, CompKind::Generator) {
            return false;
        }
        if self.lazy_class_ctx.is_some() {
            return false;
        }
        comp_inlines(elt, value, generators, &self.free_scan(true))
    }

    /// Does an inlined comprehension being emitted in *module* scope
    /// hide `name`'s `DEF_IMPORT` flag from `is_import_originated`? See
    /// [`Self::module_comp_plain_targets`]: every name the comprehension
    /// mentions has its `st_top` entry swapped for the comprehension's
    /// own (flag-less) entry, except a plain iteration target, whose
    /// scope is LOCAL on both sides and is left alone.
    fn import_flag_shadowed_by_module_comp(&self, name: &str) -> bool {
        self.kind == CodeKind::Module
            && self.inline_comp > 0
            && !self.module_comp_plain_targets.iter().any(|n| n == name)
    }

    /// `[y := i for i in r]`: does the walrus target resolve as an
    /// explicit global here? At module level (and for a name this
    /// function declared `global`) `symtable_extend_namedexpr_scope`
    /// marks it `DEF_GLOBAL` in the comprehension too, which makes it
    /// one of the saved/restored hidden locals; inside a function it's
    /// `DEF_NONLOCAL` and left alone.
    fn comp_walrus_is_global(&self, name: &str) -> bool {
        matches!(self.kind, CodeKind::Module) || self.explicit_globals.contains(name)
    }

    /// Iteration variables of this comprehension that a real scope
    /// nested inside it (lambda, generator expression, nested def)
    /// closes over: CELL in the comprehension's symbol table.
    fn comp_own_cells(
        &self,
        elt: &Expr,
        value: Option<&Expr>,
        generators: &[Comprehension],
    ) -> HashSet<String> {
        let mut needed = HashSet::new();
        collect_comp_scope_inner_free(
            elt,
            value,
            generators,
            &self.bindings,
            &mut needed,
            &self.free_scan(true),
        );
        let mut targets = HashSet::new();
        for g in generators {
            collect_target_names(&g.target, &mut targets);
        }
        needed.retain(|n| targets.contains(n));
        needed
    }

    /// The comprehension's bound symbols in CPython symtable order
    /// (`ste_symbols` insertion order): its own iteration variables
    /// and explicit-global walrus targets as the symtable visits them
    /// (outermost target, its filters, then each inner clause's
    /// target/iterable/filters, the value, the element), followed by
    /// the symbols merged from each nested inlined comprehension in
    /// visit order (`inline_comprehension` appends a child's symbols
    /// after the parent's, keeping an existing entry's scope).
    fn comp_symbols(
        &self,
        elt: &Expr,
        value: Option<&Expr>,
        generators: &[Comprehension],
        out: &mut Vec<(String, CompSym)>,
    ) {
        let cells = self.comp_own_cells(elt, value, generators);
        let mut own: Vec<(String, CompSym)> = Vec::new();
        for (gi, g) in generators.iter().enumerate() {
            let mut names = Vec::new();
            collect_target_names_ordered(&g.target, &mut names);
            for n in names {
                if !own.iter().any(|(m, _)| *m == n) {
                    let cell = cells.contains(&n);
                    own.push((n, CompSym::Target { cell }));
                }
            }
            if gi > 0 {
                self.collect_global_walrus_shallow(&g.iter, &mut own);
            }
            for cond in &g.ifs {
                self.collect_global_walrus_shallow(cond, &mut own);
            }
        }
        if let Some(v) = value {
            self.collect_global_walrus_shallow(v, &mut own);
        }
        self.collect_global_walrus_shallow(elt, &mut own);
        for entry in own {
            if !out.iter().any(|(m, _)| *m == entry.0) {
                out.push(entry);
            }
        }
        for (gi, g) in generators.iter().enumerate() {
            self.collect_nested_comp_symbols(&g.target, out);
            if gi > 0 {
                self.collect_nested_comp_symbols(&g.iter, out);
            }
            for cond in &g.ifs {
                self.collect_nested_comp_symbols(cond, out);
            }
        }
        if let Some(v) = value {
            self.collect_nested_comp_symbols(v, out);
        }
        self.collect_nested_comp_symbols(elt, out);
    }

    /// Explicit-global walrus targets bound directly in the current
    /// comprehension scope (not inside a nested lambda body or a
    /// nested comprehension, whose symbols merge separately; a nested
    /// comprehension's outermost iterable still belongs here).
    fn collect_global_walrus_shallow(&self, expr: &Expr, out: &mut Vec<(String, CompSym)>) {
        match &expr.kind {
            ExprKind::NamedExpr { target, value } => {
                if let ExprKind::Name(n) = &target.kind {
                    if self.comp_walrus_is_global(n) && !out.iter().any(|(m, _)| m == n) {
                        out.push((n.clone(), CompSym::GlobalWalrus));
                    }
                }
                self.collect_global_walrus_shallow(value, out);
            }
            ExprKind::Lambda { args, .. } | ExprKind::TypeParamFn { args, .. } => {
                for d in &args.defaults {
                    self.collect_global_walrus_shallow(d, out);
                }
                for d in args.kw_defaults.iter().flatten() {
                    self.collect_global_walrus_shallow(d, out);
                }
            }
            ExprKind::ListComp { generators, .. }
            | ExprKind::SetComp { generators, .. }
            | ExprKind::DictComp { generators, .. }
            | ExprKind::GeneratorExp { generators, .. } => {
                if let Some(first) = generators.first() {
                    self.collect_global_walrus_shallow(&first.iter, out);
                }
            }
            _ => {
                validate::for_each_child_expr(expr, &mut |c| {
                    self.collect_global_walrus_shallow(c, out)
                });
            }
        }
    }

    /// Symbols of the inlined comprehensions nested in `expr`, merged
    /// in visit order (see [`Self::comp_symbols`]).
    fn collect_nested_comp_symbols(&self, expr: &Expr, out: &mut Vec<(String, CompSym)>) {
        let (kind, elt, value, generators) = match &expr.kind {
            ExprKind::ListComp { elt, generators } => (CompKind::List, elt, None, generators),
            ExprKind::SetComp { elt, generators } => (CompKind::Set, elt, None, generators),
            ExprKind::DictComp {
                key,
                value,
                generators,
            } => (CompKind::Dict, key, Some(&**value), generators),
            ExprKind::GeneratorExp { generators, .. } => {
                // Only the outermost iterable evaluates in this scope.
                self.collect_nested_comp_symbols(&generators[0].iter, out);
                return;
            }
            ExprKind::Lambda { args, .. } | ExprKind::TypeParamFn { args, .. } => {
                for d in &args.defaults {
                    self.collect_nested_comp_symbols(d, out);
                }
                for d in args.kw_defaults.iter().flatten() {
                    self.collect_nested_comp_symbols(d, out);
                }
                return;
            }
            _ => {
                validate::for_each_child_expr(expr, &mut |c| {
                    self.collect_nested_comp_symbols(c, out)
                });
                return;
            }
        };
        self.collect_nested_comp_symbols(&generators[0].iter, out);
        if !self.comp_inline_eligible(kind, elt, value, generators) {
            return;
        }
        self.comp_symbols(elt, value, generators, out);
    }

    /// Every iteration variable that some inlined comprehension in
    /// `expr` (at any nesting depth, not crossing into real nested
    /// scopes) turns into a cell — the names `DEF_COMP_CELL` flags in
    /// this scope's symbol table.
    fn collect_comp_cells_expr(&self, expr: &Expr, out: &mut HashSet<String>) {
        let (kind, elt, value, generators) = match &expr.kind {
            ExprKind::ListComp { elt, generators } => (CompKind::List, elt, None, generators),
            ExprKind::SetComp { elt, generators } => (CompKind::Set, elt, None, generators),
            ExprKind::DictComp {
                key,
                value,
                generators,
            } => (CompKind::Dict, key, Some(&**value), generators),
            ExprKind::GeneratorExp { generators, .. } => {
                self.collect_comp_cells_expr(&generators[0].iter, out);
                return;
            }
            ExprKind::Lambda { args, .. } | ExprKind::TypeParamFn { args, .. } => {
                for d in &args.defaults {
                    self.collect_comp_cells_expr(d, out);
                }
                for d in args.kw_defaults.iter().flatten() {
                    self.collect_comp_cells_expr(d, out);
                }
                return;
            }
            _ => {
                validate::for_each_child_expr(expr, &mut |c| self.collect_comp_cells_expr(c, out));
                return;
            }
        };
        self.collect_comp_cells_expr(&generators[0].iter, out);
        if !self.comp_inline_eligible(kind, elt, value, generators) {
            return;
        }
        out.extend(self.comp_own_cells(elt, value, generators));
        for (gi, g) in generators.iter().enumerate() {
            self.collect_comp_cells_expr(&g.target, out);
            if gi > 0 {
                self.collect_comp_cells_expr(&g.iter, out);
            }
            for cond in &g.ifs {
                self.collect_comp_cells_expr(cond, out);
            }
        }
        if let Some(v) = value {
            self.collect_comp_cells_expr(v, out);
        }
        self.collect_comp_cells_expr(elt, out);
    }

    /// [`Self::collect_comp_cells_expr`] over the expressions a
    /// statement evaluates in *this* scope (nested def/class bodies
    /// are their own scopes; their decorators, defaults, annotations,
    /// bases, and keywords are ours).
    fn collect_comp_cells_stmt(&self, stmt: &Stmt, out: &mut HashSet<String>) {
        for_each_scope_expr(stmt, &mut |e| self.collect_comp_cells_expr(e, out));
    }

    /// PEP 709 inlined lowering (CPython codegen_comprehension with
    /// an inlined symtable entry): the loop compiles into the current
    /// stream. The comp's bound names become hidden fast locals of the
    /// enclosing scope, saved with LOAD_FAST_AND_CLEAR before the loop
    /// and restored after — including on the exception path, via a
    /// cleanup handler that re-raises. A name a nested scope closes
    /// over gets a fresh cell for the comprehension's duration
    /// (`MAKE_CELL` right after the save), the enclosing scope's own
    /// cell riding the stack until the restore.
    fn compile_inlined_comprehension(
        &mut self,
        kind: CompKind,
        elt: &Expr,
        value: Option<&Expr>,
        generators: &[Comprehension],
    ) -> Result<(), CompileError> {
        let collector_op = match kind {
            CompKind::List => OpCode::BuildList,
            CompKind::Set => OpCode::BuildSet,
            CompKind::Dict => OpCode::BuildMap,
            CompKind::Generator => unreachable!("genexps never inline"),
        };
        let append_op = match kind {
            CompKind::List => OpCode::ListAppend,
            CompKind::Set => OpCode::SetAdd,
            CompKind::Dict => OpCode::MapAdd,
            CompKind::Generator => unreachable!("genexps never inline"),
        };
        let comp_line = self.current_line;
        let comp_span = self.current_span;
        // `codegen_push_inlined_comprehension_locals` walks the
        // comprehension's symbols in table order and saves every name
        // it binds (`DEF_LOCAL` without `DEF_NONLOCAL`).
        let mut symbols: Vec<(String, CompSym)> = Vec::new();
        self.comp_symbols(elt, value, generators, &mut symbols);
        // The outermost iterable evaluates in the *enclosing* scope —
        // before the comp-local overrides shadow anything
        // (`[x for x in x]` iterates the outer `x`).
        self.compile_expr(&generators[0].iter)?;
        let iter_span = generators[0].iter.span;
        self.set_line_from(iter_span.start.0);
        self.set_span(iter_span);
        if generators[0].is_async {
            self.emit(OpCode::GetAiter, 0);
        } else {
            self.emit(OpCode::GetIter, 0);
        }
        self.current_line = comp_line;
        self.current_span = comp_span;
        // `_PyCompile_TweakInlinedComprehensionScopes`: a name whose
        // scope differs inside the comprehension takes the inner
        // scope for the duration. A comprehension-cell over a name
        // free in this scope stays free ("it's *_DEREF either way").
        let mut overrides: Vec<(String, Option<Binding>)> = Vec::new();
        let non_function = matches!(self.kind, CodeKind::Module | CodeKind::Class);
        let mut slots: Vec<u32> = Vec::with_capacity(symbols.len());
        for (name, sym) in &symbols {
            let outer = self.bindings.get(name).copied();
            let outer_is_free = matches!(outer, Some(Binding::Free | Binding::Nonlocal));
            let inner = match sym {
                CompSym::Target { cell: true } if outer_is_free => None,
                CompSym::Target { cell: true } => Some(Binding::Cell),
                CompSym::Target { cell: false } => Some(Binding::Local),
                CompSym::GlobalWalrus => None,
            };
            if let Some(b) = inner {
                if outer != Some(b) {
                    let prev = self.bindings.insert(name.clone(), b);
                    overrides.push((name.clone(), prev));
                }
            }
            let slot = self.var_index_or_add(name);
            slots.push(slot);
            if non_function && !self.co.hidden_locals.contains(name) {
                self.co.hidden_locals.push(name.clone());
            }
            // Save the enclosing value (a cell, when the slot is one)
            // and clear; a comprehension cell then gets a fresh cell in
            // its `cellvars` slot — or its `freevars` slot when this
            // scope only sees the name free (CPython keeps that quirk:
            // the free cell is what the closure captures, and the
            // restore below leaves it in place).
            self.emit(OpCode::LoadFastAndClear, slot);
            if matches!(sym, CompSym::Target { cell: true }) {
                let idx = if outer_is_free {
                    if !self.free_order.iter().any(|n| n == name) {
                        self.free_order.push(name.clone());
                    }
                    let pos = self.free_order.iter().position(|n| n == name).unwrap_or(0);
                    (self.co.cellvars.len() + pos) as u32
                } else {
                    if !self.co.cellvars.contains(name) {
                        // Normally pre-registered by the scope's
                        // analysis (`register_comp_cells`); a late
                        // arrival still gets its slot.
                        self.co.cellvars.push(name.clone());
                    }
                    self.co.cellvars.iter().position(|n| n == name).unwrap_or(0) as u32
                };
                self.emit(OpCode::MakeCell, idx);
            }
        }
        // `SWAP n+1` brings the iterator back to the top (rotating the
        // saved values; the restore's matching swap undoes that).
        let npops = slots.len() as u32;
        if npops > 0 {
            self.emit(OpCode::Swap, npops + 1);
            // `SETUP_FINALLY cleanup` at the comprehension's location.
            self.emit_setup(OpCode::SetupFinally);
        }
        // Accumulator goes *under* the iterator; this is where the
        // protected region begins (behind the `SETUP_FINALLY cleanup`).
        let protect_start = self.next_offset();
        self.emit(collector_op, 0);
        self.emit(OpCode::Swap, 2);
        self.inline_comp += 1;
        let plain_targets_mark = self.module_comp_plain_targets.len();
        if self.kind == CodeKind::Module {
            self.module_comp_plain_targets.extend(
                symbols
                    .iter()
                    .filter(|(_, sym)| matches!(sym, CompSym::Target { cell: false }))
                    .map(|(name, _)| name.clone()),
            );
        }
        let comp_loc = CompLoc::current(self);
        let body = compile_comp_body(self, comp_loc, generators, 0, 1, elt, value, append_op);
        self.module_comp_plain_targets.truncate(plain_targets_mark);
        self.inline_comp -= 1;
        body?;
        // Loop done: stack is [saved.., accumulator].
        let protect_end = self.next_offset();
        self.current_line = comp_line;
        self.current_span = comp_span;
        if npops > 0 {
            // `codegen_pop_inlined_comprehension_locals`: POP_BLOCK,
            // a NO_LOCATION JUMP_NO_INTERRUPT over the exceptional
            // restore, the cleanup handler, then the normal-path
            // restore at `end` (so it shares a block with whatever
            // consumes the result, which lets `apply_static_swaps`
            // fold the swap into the following stores).
            self.emit_pop_block_no_line();
            let over = self.emit_no_line(OpCode::JumpForward, 0);
            self.no_interrupt_jumps.insert(over);
            // Exception-path restore: [saved.., acc, exc] -> drop the
            // partial accumulator, restore, re-raise. Depth is
            // resolved in `finish` (base depth of the surrounding
            // expression isn't known here).
            let handler = self.next_offset();
            self.co.exception_table.push(ExcHandler {
                start: protect_start,
                end: protect_end,
                handler,
                depth: HANDLER_DEPTH_SENTINEL,
                push_lasti: false,
            });
            self.emit_no_line(OpCode::Swap, 2);
            self.emit_no_line(OpCode::PopTop, 0);
            self.emit_inlined_comprehension_restore(&slots);
            self.emit_no_line(OpCode::Reraise, 0);
            let after = self.next_offset();
            self.patch_jump(over, after);
            self.emit_inlined_comprehension_restore(&slots);
        }
        for (n, prev) in overrides.into_iter().rev() {
            match prev {
                Some(b) => {
                    self.bindings.insert(n, b);
                }
                None => {
                    self.bindings.shift_remove(&n);
                }
            }
        }
        Ok(())
    }

    /// `restore_inlined_comprehension_locals`: `SWAP n+1` puts the
    /// comprehension result (or the in-flight exception) back on top,
    /// then the saved values are stored back in reverse push order with
    /// `STORE_FAST_MAYBE_NULL` (a saved slot may have been unbound).
    fn emit_inlined_comprehension_restore(&mut self, slots: &[u32]) {
        self.emit(OpCode::Swap, slots.len() as u32 + 1);
        for &s in slots.iter().rev() {
            self.emit(OpCode::StoreFastMaybeNull, s);
        }
    }

    fn compile_comprehension(
        &mut self,
        kind: CompKind,
        elt: &Expr,
        value: Option<&Expr>,
        generators: &[Comprehension],
    ) -> Result<(), CompileError> {
        // Comprehensions are lowered to anonymous functions taking
        // a single argument (.0) that holds the iterator of the
        // outermost generator. This matches CPython's lowering.
        // PEP 530: a comprehension that uses `async for` (or `await`
        // inside the element / filter) compiles to a coroutine; the
        // caller awaits the resulting coroutine to get the value.
        // A comprehension is a coroutine if it has an `async for`
        // clause, directly contains an `await`, *or* its element/value
        // is itself an async comprehension. The last case is PEP 530's
        // implicit propagation: in `[[x async for x in a] for j in b]`
        // the inner async comp evaluates to a coroutine, so the outer
        // (otherwise synchronous) comprehension must `await` it and is
        // therefore async too. `expr_contains_await` deliberately stops
        // at nested comprehension scopes, so we detect the nested-async
        // case separately with `expr_contains_async_comp`.
        let is_async_comp = comp_clause_is_async(generators, elt, value);
        // PEP 572: enforce the symtable-stage named-expression rules
        // once per comprehension nest, *before* choosing a lowering —
        // an inlined comp must reject a walrus in its iterable exactly
        // like the nested-function form (test_named_expressions). The
        // outermost comprehension of a nest sees the whole nest; nested
        // ones (classic `CodeKind::Comprehension` scopes or inlined
        // bodies) were already covered by that outermost walk.
        if !matches!(self.kind, CodeKind::Comprehension) && self.inline_comp == 0 {
            let mut stack = Vec::new();
            check_comp_walrus_nest(
                matches!(self.kind, CodeKind::Class),
                elt,
                value,
                generators,
                &mut stack,
            )?;
        }
        // PEP 709: list/set/dict comprehensions inline into the
        // enclosing scope — no nested code object, no call, the loop
        // ops (LIST_APPEND & co.) land in the enclosing stream
        // (test_compile TestSourcePositions multiline comprehensions).
        // `comp_inline_eligible` keeps the classic nested-function
        // lowering for the shapes phase 1 doesn't cover.
        if self.comp_inline_eligible(kind, elt, value, generators) {
            return self.compile_inlined_comprehension(kind, elt, value, generators);
        }
        // `compile_expr` set the current span to the whole comprehension
        // expression before dispatching here.
        let whole_span = self.current_span;
        let name = match kind {
            CompKind::List => "<listcomp>",
            CompKind::Set => "<setcomp>",
            CompKind::Dict => "<dictcomp>",
            CompKind::Generator => "<genexpr>",
        };
        let mut inner = Compiler::new(
            name.to_owned(),
            self.co.filename.clone(),
            CodeKind::Comprehension,
            self.line_index.clone(),
            self.source.clone(),
            self.params.clone(),
        );
        // `codegen_enter_scope(..., e->lineno, ...)`: the scope's first
        // line is the comprehension expression's own, not the enclosing
        // statement's.
        inner.set_line_from(whole_span.0);
        inner.comp_kind = Some(kind);
        inner.private = self.private.clone();
        // A non-inlined comprehension (genexpr) directly in a class
        // body is a function block under the class block: `CO_METHOD`.
        inner.co.is_method = self.co.is_class_body;
        inner.co.is_nested = self.child_is_nested();
        // PEP 3155: a comprehension scope gets a dotted qualname like any
        // other nested scope (`C.m.<locals>.<genexpr>`); CPython's
        // `compiler_set_qualname` doesn't special-case comprehensions.
        inner.co.qualname = self.compute_child_qualname(name);
        inner.co.arg_count = 1;
        inner.co.varnames.push(".0".to_owned());
        inner.bindings.insert(".0".to_owned(), Binding::Local);
        if is_async_comp && !matches!(kind, CompKind::Generator) {
            inner.co.is_coroutine = true;
        }
        if is_async_comp && matches!(kind, CompKind::Generator) {
            // `(x async for x in xs)` becomes an async generator.
            inner.co.is_async_generator = true;
            inner.co.is_generator = true;
        }

        let collector_op = match kind {
            CompKind::List => Some(OpCode::BuildList),
            CompKind::Set => Some(OpCode::BuildSet),
            CompKind::Dict => Some(OpCode::BuildMap),
            CompKind::Generator => None,
        };
        let append_op = match kind {
            CompKind::List => OpCode::ListAppend,
            CompKind::Set => OpCode::SetAdd,
            CompKind::Dict => OpCode::MapAdd,
            CompKind::Generator => OpCode::YieldValue,
        };
        // Free-variable resolution from outer scope. The *outermost*
        // iterable is excluded: it is evaluated eagerly in the enclosing
        // scope and handed in as `.0` (CPython's symtable does the same),
        // so a name read only there must NOT become a freevar — that would
        // spuriously cell-promote the enclosing local. Load-bearing for
        // `sys.getrefcount` parity: `any(x.f() for x in self.things)`
        // must not promote `self` to a cell, because the cell seed holds
        // an extra strong reference for the whole activation, and pandas'
        // chained-assignment detection (`getrefcount(self) <= 3` in
        // `DataFrame.__setitem__`) is calibrated against CPython counts.
        let mut reads = HashSet::new();
        collect_reads_expr(elt, &mut reads);
        if let Some(v) = value {
            collect_reads_expr(v, &mut reads);
        }
        for (gi, g) in generators.iter().enumerate() {
            if gi > 0 {
                collect_reads_expr(&g.iter, &mut reads);
            }
            collect_reads_expr(&g.target, &mut reads);
            for i in &g.ifs {
                collect_reads_expr(i, &mut reads);
            }
        }
        // A comprehension's `for` targets are *local to the comprehension*
        // and shadow any same-named variable in the enclosing scope. Bind
        // them BEFORE free-variable resolution: otherwise a target like `f`
        // in `{f for f in xs}` whose name also exists as an enclosing local
        // `f` is mistaken for a free reference to that outer `f`. That spuriously
        // cell-promotes the enclosing local and shifts every freevar index by
        // one — silently aliasing later closure reads. CPython's symtable binds
        // comprehension targets first for exactly this reason.
        for g in generators {
            let mut assigned = HashSet::new();
            collect_target_names(&g.target, &mut assigned);
            for n in assigned {
                inner.bindings.insert(n, Binding::Local);
            }
        }
        // The PEP 572 named-expression rules were enforced above,
        // before the lowering choice. Bind each walrus target in the nearest enclosing
        // non-comprehension scope: a comprehension in a *function* stores
        // it through a cell (implicit `nonlocal`), a comprehension at
        // module scope stores a global, and an intermediate comprehension
        // just forwards its own enclosing binding. The enclosing
        // function's side of this — the name existing as a local at all —
        // is handled by `collect_walrus_stmt`'s descent at scope entry.
        {
            let mut walrus_names: Vec<String> = Vec::new();
            collect_comp_scope_walruses(elt, value, generators, &mut |n| {
                if !walrus_names.iter().any(|w| w == n) {
                    walrus_names.push(n.to_owned());
                }
            });
            for name in walrus_names {
                if inner.bindings.contains_key(&name) {
                    continue;
                }
                let enclosing = self.bindings.get(&name).copied();
                let binding = match (self.kind, enclosing) {
                    // Explicit `global` declarations win in any scope;
                    // module scope binds globals by definition. (A class
                    // body already errored in the check above.)
                    (_, Some(Binding::Global)) | (CodeKind::Module | CodeKind::Class, _) => {
                        Binding::Global
                    }
                    // An intermediate comprehension forwards whatever its
                    // own creation recorded (Free towards a function cell,
                    // or Global). A missing record degrades to Global.
                    (CodeKind::Comprehension, Some(Binding::Free)) => Binding::Free,
                    (CodeKind::Comprehension, None) => Binding::Global,
                    // Function scope: route through a cell, creating the
                    // enclosing local if the pre-pass didn't already.
                    _ => {
                        if matches!(enclosing, None | Some(Binding::Local)) {
                            self.bindings.insert(name.clone(), Binding::Cell);
                            if !self.co.cellvars.contains(&name) {
                                self.co.cellvars.push(name.clone());
                            }
                        }
                        Binding::Free
                    }
                };
                if matches!(binding, Binding::Free) {
                    inner.bindings.insert(name.clone(), Binding::Free);
                    if !inner.free_order.contains(&name) {
                        inner.free_order.push(name);
                    }
                } else {
                    inner.bindings.insert(name, binding);
                }
            }
        }
        for name in reads {
            if inner.bindings.contains_key(&name) {
                continue;
            }
            if let Some(b) = self.bindings.get(&name) {
                if matches!(
                    b,
                    Binding::Local
                        | Binding::Cell
                        | Binding::Free
                        | Binding::Nonlocal
                        | Binding::ClassPassthrough
                ) {
                    inner.bindings.insert(name.clone(), Binding::Free);
                    inner.free_order.push(name);
                } else if matches!(b, Binding::Global)
                    && self.class_transparent_frees.contains(&name)
                {
                    // `global y` in the enclosing *class* body doesn't
                    // reach into the comprehension: class scopes are
                    // invisible to nested scopes, so the name still
                    // closes over the enclosing function's cell (which
                    // the class forwards — see `class_transparent_frees`).
                    inner.bindings.insert(name.clone(), Binding::Free);
                    inner.free_order.push(name);
                }
            }
        }

        // RFC 0037 (WS2): a comprehension target (or `.0`) that an inner
        // scope — a *nested* comprehension or a lambda inside the
        // element / value / filter / inner-iterable — closes over must be
        // a **cell**, and that has to be decided *before* the loop body
        // is emitted. Otherwise `compile_comp_body` stores the target
        // with `STORE_FAST` into a plain local slot while the inner scope
        // reads it via `LOAD_DEREF` from an (unwritten) cell — yielding
        // `None`, exactly the `[[x for y in ys] for x in xs]` bug.
        // Mirrors `analyze_scope_function`'s pre-emission cell promotion.
        {
            let needed_in_inner =
                inner.needed_in_inner(inner.lazy_class_ctx.is_none(), |scan, out| {
                    collect_inner_free_expr(elt, &inner.bindings, out, scan);
                    if let Some(v) = value {
                        collect_inner_free_expr(v, &inner.bindings, out, scan);
                    }
                    for (gi, g) in generators.iter().enumerate() {
                        // generators[0].iter is evaluated in the
                        // *enclosing* scope (passed in as `.0`); every
                        // later iter, every filter, and every *target
                        // sub-expression* (a nested comprehension can
                        // sit in a subscripted target — `for a[[x for x
                        // in [1] if _C][0]] in …` — and close over this
                        // comprehension's variables) runs inside this
                        // comprehension.
                        if gi > 0 {
                            collect_inner_free_expr(&g.iter, &inner.bindings, out, scan);
                        }
                        collect_inner_free_expr(&g.target, &inner.bindings, out, scan);
                        for cond in &g.ifs {
                            collect_inner_free_expr(cond, &inner.bindings, out, scan);
                        }
                    }
                });
            let mut needed_in_inner: Vec<String> = needed_in_inner.into_iter().collect();
            needed_in_inner.sort_unstable();
            for name in needed_in_inner {
                if matches!(inner.bindings.get(&name), Some(Binding::Local)) {
                    inner.bindings.insert(name.clone(), Binding::Cell);
                    if !inner.co.cellvars.contains(&name) {
                        inner.co.cellvars.push(name);
                    }
                }
            }
            if inner.lazy_class_ctx.is_none() {
                let mut comp_cells = HashSet::new();
                for (gi, g) in generators.iter().enumerate() {
                    if gi > 0 {
                        inner.collect_comp_cells_expr(&g.iter, &mut comp_cells);
                    }
                    inner.collect_comp_cells_expr(&g.target, &mut comp_cells);
                    for cond in &g.ifs {
                        inner.collect_comp_cells_expr(cond, &mut comp_cells);
                    }
                }
                if let Some(v) = value {
                    inner.collect_comp_cells_expr(v, &mut comp_cells);
                }
                inner.collect_comp_cells_expr(elt, &mut comp_cells);
                inner.register_comp_cells(comp_cells);
            }
        }

        if matches!(kind, CompKind::Generator) && !is_async_comp {
            inner.co.is_generator = true;
            inner.emit(OpCode::ReturnGenerator, 0);
            inner.emit(OpCode::PopTop, 0);
        } else if is_async_comp {
            // Both async-generator comps and async list/set/dict
            // comps use the suspended-frame infrastructure.
            inner.emit(OpCode::ReturnGenerator, 0);
            inner.emit(OpCode::PopTop, 0);
        }
        inner.emit_entry_resume();
        // `codegen_comprehension` stamps the accumulator build and the
        // outermost `LOAD_FAST .0` with the whole expression's span.
        inner.current_span = whole_span;
        inner.set_line_from(whole_span.0);
        if let Some(op) = collector_op {
            inner.emit(op, 0);
        }
        // Outermost iterator comes in as `.0`. CPython 3.14 iterates
        // it at the call site (`GET_ITER; CALL 0`) and the body goes
        // straight to `FOR_ITER` (3.13 re-iterated defensively with a
        // second GET_ITER here); the async depth-0 arm converts with
        // GET_AITER inside the body instead.
        inner.emit(OpCode::LoadFast, 0);
        let comp_loc = CompLoc::current(&inner);
        compile_comp_body(
            &mut inner, comp_loc, generators, 0, 1, elt, value, append_op,
        )?;
        if matches!(kind, CompKind::Generator) {
            // ForIter pops the iterator on exhaustion. Return None
            // so the generator finishes cleanly (the VM converts
            // this to `StopIteration`)
            // (test_multiline_generator_expression). An *async* genexp
            // stamps that return with the whole expression's span
            // (test_multiline_async_generator_expression); a sync one
            // inherits the loop-exit location.
            if is_async_comp {
                inner.current_span = whole_span;
                inner.set_line_from(whole_span.0);
            }
            let none_idx = inner.co.intern_constant(Constant::None);
            inner.emit(OpCode::LoadConst, none_idx);
            inner.emit(OpCode::ReturnValue, 0);
        } else {
            // `codegen_comprehension`: the collection's `RETURN_VALUE`
            // sits at `LOC(e)`, the whole comprehension.
            inner.current_span = whole_span;
            inner.set_line_from(whole_span.0);
            inner.emit(OpCode::ReturnValue, 0);
        }

        let inner_code = inner.finish();
        let inner_freevars = inner_code.freevars.clone();

        // Promote our locals to cells where needed.
        for free in &inner_freevars {
            if matches!(self.bindings.get(free), Some(Binding::Local)) {
                self.bindings.insert(free.clone(), Binding::Cell);
                if !self.co.cellvars.contains(free) {
                    self.co.cellvars.push(free.clone());
                }
            }
        }

        let mut flags = 0u32;
        if !inner_freevars.is_empty() {
            for free in &inner_freevars {
                let idx = self.cell_or_free_index(free);
                self.emit(OpCode::LoadClosure, idx);
            }
            self.emit(OpCode::BuildTuple, inner_freevars.len() as u32);
            flags |= 0x08;
        }
        let code_idx = self
            .co
            .intern_constant(Constant::Code(std::sync::Arc::new(inner_code)));
        self.emit(OpCode::LoadConst, code_idx);
        self.emit_make_function(flags);
        // Push the outermost generator's iterator as `.0`: CPython
        // 3.14's `codegen_comprehension_iter` runs GET_ITER (or
        // GET_AITER for an async outermost generator) at the call site.
        self.compile_expr(&generators[0].iter)?;
        // The GET_ITER and the invoking CALL carry the *iterable
        // expression's* location, not the whole comprehension's: an
        // exception raised from `iter()`/`__next__` must anchor its
        // traceback at the iterable (CPython 3.12+ inlined comprehensions
        // put FOR_ITER at that span; `test_listcomps.test_exception_
        // locations` asserts the resulting `colno`/`end_colno`).
        let iter_span = generators[0].iter.span;
        self.set_line_from(iter_span.start.0);
        self.set_span(iter_span);
        if generators[0].is_async {
            self.emit(OpCode::GetAiter, 0);
        } else {
            self.emit(OpCode::GetIter, 0);
        }
        // The iterator rides the self slot (CPython's comprehension
        // invocation is `CALL 0`, back at the whole expression's
        // location, as is the await of an async comprehension).
        self.current_span = whole_span;
        self.set_line_from(whole_span.0);
        self.emit(OpCode::CallSelf, 1);
        // For an async list/set/dict comprehension the call returned
        // a coroutine; the enclosing async function awaits it so the
        // final value (list/set/dict) ends up on the stack.
        if is_async_comp && !matches!(kind, CompKind::Generator) {
            if !self.in_async_context() {
                return Err(CompileError::new(
                    "asynchronous comprehension outside of an asynchronous function",
                ));
            }
            self.compile_await_dance(0);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy)]
enum CompKind {
    List,
    Set,
    Dict,
    Generator,
}

// ---------- PEP 572: named expressions in comprehensions ----------

/// Walrus target names bound *through* one comprehension scope, in
/// syntactic order. Covers the comprehension's element/value, filters,
/// non-outermost iterables, targets (their non-name sub-expressions), and
/// every nested comprehension (a walrus there extends through this scope
/// too, per `symtable_extend_namedexpr_scope`). The **outermost iterable
/// is excluded** — it is evaluated in the enclosing scope, so a nested
/// comprehension's walrus inside it never routes through *this* scope.
/// Lambda/def bodies are opaque (their walruses bind in them); lambda
/// defaults evaluate here and are included.
fn collect_comp_scope_walruses(
    elt: &Expr,
    value: Option<&Expr>,
    generators: &[Comprehension],
    out: &mut dyn FnMut(&str),
) {
    fn visit(e: &Expr, out: &mut dyn FnMut(&str)) {
        match &e.kind {
            ExprKind::NamedExpr { target, value } => {
                if let ExprKind::Name(n) = &target.kind {
                    out(n);
                }
                visit(value, out);
            }
            ExprKind::ListComp { elt, generators }
            | ExprKind::SetComp { elt, generators }
            | ExprKind::GeneratorExp { elt, generators } => {
                collect_comp_scope_walruses(elt, None, generators, out);
            }
            ExprKind::DictComp {
                key,
                value,
                generators,
            } => {
                collect_comp_scope_walruses(key, Some(value), generators, out);
            }
            ExprKind::Lambda { args, .. } | ExprKind::TypeParamFn { args, .. } => {
                for d in &args.defaults {
                    visit(d, out);
                }
                for d in args.kw_defaults.iter().flatten() {
                    visit(d, out);
                }
            }
            ExprKind::Attribute { value, .. } | ExprKind::Starred(value) => visit(value, out),
            ExprKind::Subscript { value, slice } => {
                visit(value, out);
                visit(slice, out);
            }
            ExprKind::Slice { lower, upper, step } => {
                for x in [lower.as_deref(), upper.as_deref(), step.as_deref()]
                    .into_iter()
                    .flatten()
                {
                    visit(x, out);
                }
            }
            ExprKind::BinOp { left, right, .. } => {
                visit(left, out);
                visit(right, out);
            }
            ExprKind::BoolOp { values, .. } => {
                for v in values {
                    visit(v, out);
                }
            }
            ExprKind::UnaryOp { operand, .. } => visit(operand, out),
            ExprKind::Compare {
                left, comparators, ..
            } => {
                visit(left, out);
                for c in comparators {
                    visit(c, out);
                }
            }
            ExprKind::IfExp { test, body, orelse } => {
                visit(test, out);
                visit(body, out);
                visit(orelse, out);
            }
            ExprKind::Call {
                func,
                args,
                keywords,
            } => {
                visit(func, out);
                for a in args {
                    visit(a, out);
                }
                for k in keywords {
                    visit(&k.value, out);
                }
            }
            ExprKind::Tuple(items) | ExprKind::List(items) | ExprKind::Set(items) => {
                for x in items {
                    visit(x, out);
                }
            }
            ExprKind::Dict { keys, values } => {
                for k in keys.iter().flatten() {
                    visit(k, out);
                }
                for v in values {
                    visit(v, out);
                }
            }
            ExprKind::Yield(v) => {
                if let Some(v) = v {
                    visit(v, out);
                }
            }
            ExprKind::YieldFrom(v) | ExprKind::Await(v) => visit(v, out),
            ExprKind::JoinedStr(parts) | ExprKind::TemplateStr(parts) => {
                for p in parts {
                    visit(p, out);
                }
            }
            ExprKind::FormattedValue {
                value, format_spec, ..
            }
            | ExprKind::Interpolation {
                value, format_spec, ..
            } => {
                visit(value, out);
                if let Some(fs) = format_spec.as_deref() {
                    visit(fs, out);
                }
            }
            ExprKind::Name(_) | ExprKind::Constant(_) => {}
        }
    }
    for (gi, g) in generators.iter().enumerate() {
        if gi > 0 {
            visit(&g.iter, out);
        }
        visit(&g.target, out);
        for cond in &g.ifs {
            visit(cond, out);
        }
    }
    visit(elt, out);
    if let Some(v) = value {
        visit(v, out);
    }
}

/// Presentation form of a possibly-mangled private name: the AST reaching
/// the compiler already carries PEP 8 private-name mangling (`__x` in
/// `class Foo` arrives as `_Foo__x`), but CPython's symtable errors show
/// the *source* spelling. Strip the `_ClassName` prefix back off.
fn unmangled(name: &str) -> &str {
    if name.starts_with('_') && !name.starts_with("__") {
        if let Some(i) = name.find("__") {
            return &name[i..];
        }
    }
    name
}

/// PEP 572: no assignment expression may appear *anywhere lexically
/// inside* a comprehension's iterable expression — CPython flags even a
/// walrus buried in a lambda body or a nested comprehension there
/// (`ste_comp_iter_expr` stays raised across those symtable entries).
fn reject_walrus_in_iterable(e: &Expr) -> Result<(), CompileError> {
    struct Found(weavepy_lexer::Span);
    fn scan(e: &Expr) -> Result<(), Found> {
        if let ExprKind::NamedExpr { .. } = &e.kind {
            return Err(Found(e.span));
        }
        match &e.kind {
            ExprKind::Lambda { args, body } | ExprKind::TypeParamFn { args, body } => {
                for d in &args.defaults {
                    scan(d)?;
                }
                for d in args.kw_defaults.iter().flatten() {
                    scan(d)?;
                }
                scan(body)
            }
            ExprKind::ListComp { elt, generators }
            | ExprKind::SetComp { elt, generators }
            | ExprKind::GeneratorExp { elt, generators } => {
                for g in generators {
                    scan(&g.iter)?;
                    scan(&g.target)?;
                    for c in &g.ifs {
                        scan(c)?;
                    }
                }
                scan(elt)
            }
            ExprKind::DictComp {
                key,
                value,
                generators,
            } => {
                for g in generators {
                    scan(&g.iter)?;
                    scan(&g.target)?;
                    for c in &g.ifs {
                        scan(c)?;
                    }
                }
                scan(key)?;
                scan(value)
            }
            ExprKind::Attribute { value, .. } | ExprKind::Starred(value) => scan(value),
            ExprKind::Subscript { value, slice } => {
                scan(value)?;
                scan(slice)
            }
            ExprKind::Slice { lower, upper, step } => {
                for x in [lower.as_deref(), upper.as_deref(), step.as_deref()]
                    .into_iter()
                    .flatten()
                {
                    scan(x)?;
                }
                Ok(())
            }
            ExprKind::BinOp { left, right, .. } => {
                scan(left)?;
                scan(right)
            }
            ExprKind::BoolOp { values, .. } => {
                for v in values {
                    scan(v)?;
                }
                Ok(())
            }
            ExprKind::UnaryOp { operand, .. } => scan(operand),
            ExprKind::Compare {
                left, comparators, ..
            } => {
                scan(left)?;
                for c in comparators {
                    scan(c)?;
                }
                Ok(())
            }
            ExprKind::IfExp { test, body, orelse } => {
                scan(test)?;
                scan(body)?;
                scan(orelse)
            }
            ExprKind::Call {
                func,
                args,
                keywords,
            } => {
                scan(func)?;
                for a in args {
                    scan(a)?;
                }
                for k in keywords {
                    scan(&k.value)?;
                }
                Ok(())
            }
            ExprKind::Tuple(items) | ExprKind::List(items) | ExprKind::Set(items) => {
                for x in items {
                    scan(x)?;
                }
                Ok(())
            }
            ExprKind::Dict { keys, values } => {
                for k in keys.iter().flatten() {
                    scan(k)?;
                }
                for v in values {
                    scan(v)?;
                }
                Ok(())
            }
            ExprKind::Yield(v) => match v {
                Some(v) => scan(v),
                None => Ok(()),
            },
            ExprKind::YieldFrom(v) | ExprKind::Await(v) => scan(v),
            ExprKind::JoinedStr(parts) | ExprKind::TemplateStr(parts) => {
                for p in parts {
                    scan(p)?;
                }
                Ok(())
            }
            ExprKind::FormattedValue {
                value, format_spec, ..
            }
            | ExprKind::Interpolation {
                value, format_spec, ..
            } => {
                scan(value)?;
                if let Some(fs) = format_spec.as_deref() {
                    scan(fs)?;
                }
                Ok(())
            }
            ExprKind::Name(_) | ExprKind::Constant(_) | ExprKind::NamedExpr { .. } => Ok(()),
        }
    }
    match scan(e) {
        Ok(()) => Ok(()),
        Err(Found(span)) => Err(CompileError::spanned(
            "assignment expression cannot be used in a comprehension iterable expression",
            span,
        )),
    }
}

/// One comprehension scope on the PEP 572 checker's stack.
#[derive(Default)]
struct CompWalrusScope {
    /// Iteration-variable names bound so far (syntactic order — a later
    /// `for` clause is "not yet bound" while an earlier filter runs).
    iter_vars: HashSet<String>,
    /// Walrus target names recorded so far. Extension marks the name in
    /// every comprehension scope it passes through on its way to the
    /// binding scope, exactly like CPython's `DEF_LOCAL` marking.
    walrus_targets: HashSet<String>,
}

/// Enforce CPython's four symtable-stage named-expression rules over a
/// comprehension nest (`symtable.c`): no walrus in any comprehension
/// iterable expression, no rebinding an iteration variable, no `for`
/// target rebinding an earlier walrus target, and no comprehension
/// walrus binding into a class body. Called once per *outermost*
/// comprehension (`compile_comprehension` skips it when the enclosing
/// scope is itself a comprehension — the outermost run already walked
/// the whole nest). Lambda/def bodies are separate scopes and are
/// skipped; their own comprehensions get checked when they compile.
fn check_comp_walrus_nest(
    in_class_body: bool,
    elt: &Expr,
    value: Option<&Expr>,
    generators: &[Comprehension],
    stack: &mut Vec<CompWalrusScope>,
) -> Result<(), CompileError> {
    fn visit(
        e: &Expr,
        in_class_body: bool,
        stack: &mut Vec<CompWalrusScope>,
    ) -> Result<(), CompileError> {
        match &e.kind {
            ExprKind::NamedExpr { target, value } => {
                if let ExprKind::Name(n) = &target.kind {
                    // Rebinding outranks the class-body diagnostic
                    // (the extension walk hits comprehension scopes
                    // before it reaches the class block).
                    for scope in stack.iter() {
                        if scope.iter_vars.contains(n) {
                            return Err(CompileError::spanned(
                                format!(
                                    "assignment expression cannot rebind comprehension \
                                     iteration variable '{}'",
                                    unmangled(n)
                                ),
                                e.span,
                            ));
                        }
                    }
                    if in_class_body {
                        return Err(CompileError::spanned(
                            "assignment expression within a comprehension cannot be used in a \
                             class body",
                            e.span,
                        ));
                    }
                    for scope in stack.iter_mut() {
                        scope.walrus_targets.insert(n.clone());
                    }
                }
                visit(value, in_class_body, stack)
            }
            ExprKind::ListComp { elt, generators }
            | ExprKind::SetComp { elt, generators }
            | ExprKind::GeneratorExp { elt, generators } => {
                check_comp_walrus_nest(in_class_body, elt, None, generators, stack)
            }
            ExprKind::DictComp {
                key,
                value,
                generators,
            } => check_comp_walrus_nest(in_class_body, key, Some(value), generators, stack),
            ExprKind::Lambda { args, .. } | ExprKind::TypeParamFn { args, .. } => {
                for d in &args.defaults {
                    visit(d, in_class_body, stack)?;
                }
                for d in args.kw_defaults.iter().flatten() {
                    visit(d, in_class_body, stack)?;
                }
                Ok(())
            }
            ExprKind::Attribute { value, .. } | ExprKind::Starred(value) => {
                visit(value, in_class_body, stack)
            }
            ExprKind::Subscript { value, slice } => {
                visit(value, in_class_body, stack)?;
                visit(slice, in_class_body, stack)
            }
            ExprKind::Slice { lower, upper, step } => {
                for x in [lower.as_deref(), upper.as_deref(), step.as_deref()]
                    .into_iter()
                    .flatten()
                {
                    visit(x, in_class_body, stack)?;
                }
                Ok(())
            }
            ExprKind::BinOp { left, right, .. } => {
                visit(left, in_class_body, stack)?;
                visit(right, in_class_body, stack)
            }
            ExprKind::BoolOp { values, .. } => {
                for v in values {
                    visit(v, in_class_body, stack)?;
                }
                Ok(())
            }
            ExprKind::UnaryOp { operand, .. } => visit(operand, in_class_body, stack),
            ExprKind::Compare {
                left, comparators, ..
            } => {
                visit(left, in_class_body, stack)?;
                for c in comparators {
                    visit(c, in_class_body, stack)?;
                }
                Ok(())
            }
            ExprKind::IfExp { test, body, orelse } => {
                visit(test, in_class_body, stack)?;
                visit(body, in_class_body, stack)?;
                visit(orelse, in_class_body, stack)
            }
            ExprKind::Call {
                func,
                args,
                keywords,
            } => {
                visit(func, in_class_body, stack)?;
                for a in args {
                    visit(a, in_class_body, stack)?;
                }
                for k in keywords {
                    visit(&k.value, in_class_body, stack)?;
                }
                Ok(())
            }
            ExprKind::Tuple(items) | ExprKind::List(items) | ExprKind::Set(items) => {
                for x in items {
                    visit(x, in_class_body, stack)?;
                }
                Ok(())
            }
            ExprKind::Dict { keys, values } => {
                for k in keys.iter().flatten() {
                    visit(k, in_class_body, stack)?;
                }
                for v in values {
                    visit(v, in_class_body, stack)?;
                }
                Ok(())
            }
            ExprKind::Yield(v) => match v {
                Some(v) => visit(v, in_class_body, stack),
                None => Ok(()),
            },
            ExprKind::YieldFrom(v) | ExprKind::Await(v) => visit(v, in_class_body, stack),
            ExprKind::JoinedStr(parts) | ExprKind::TemplateStr(parts) => {
                for p in parts {
                    visit(p, in_class_body, stack)?;
                }
                Ok(())
            }
            ExprKind::FormattedValue {
                value, format_spec, ..
            }
            | ExprKind::Interpolation {
                value, format_spec, ..
            } => {
                visit(value, in_class_body, stack)?;
                if let Some(fs) = format_spec.as_deref() {
                    visit(fs, in_class_body, stack)?;
                }
                Ok(())
            }
            ExprKind::Name(_) | ExprKind::Constant(_) => Ok(()),
        }
    }

    stack.push(CompWalrusScope::default());
    for g in generators {
        // Every iterable — outermost included — rejects walruses
        // *anywhere lexically inside it*: CPython flags even a walrus
        // buried in a lambda body or a nested comprehension within the
        // iterable (test_named_expressions' "Lambda expression" /
        // "Nested comprehension body" cases).
        reject_walrus_in_iterable(&g.iter)?;
        let mut names = HashSet::new();
        collect_target_names(&g.target, &mut names);
        for scope in stack.iter() {
            for n in &names {
                if scope.walrus_targets.contains(n) {
                    return Err(CompileError::spanned(
                        format!(
                            "comprehension inner loop cannot rebind assignment expression \
                             target '{}'",
                            unmangled(n)
                        ),
                        g.target.span,
                    ));
                }
            }
        }
        stack
            .last_mut()
            .expect("scope pushed above")
            .iter_vars
            .extend(names);
        // Non-name parts of the target (subscript bases/indices, …)
        // evaluate inside the comprehension scope.
        visit(&g.target, in_class_body, stack)?;
        for cond in &g.ifs {
            visit(cond, in_class_body, stack)?;
        }
    }
    visit(elt, in_class_body, stack)?;
    if let Some(v) = value {
        visit(v, in_class_body, stack)?;
    }
    stack.pop();
    Ok(())
}

/// Stamp the compiler's current location with the comprehension
/// element's span (for dict comps, the combined `key: value` span).
/// CPython gives this location to the loop's back edge and — via jump
/// threading — to the comp-`if` conditional jumps that land on it
/// (test_compile's TestSourcePositions multiline comprehension family).
/// `elt_loc` in `codegen_sync_comprehension_generator`: the element's
/// location, widened to the `key: value` pair only for the *innermost*
/// generator of a dict comprehension (the widening happens in the
/// `COMP_DICTCOMP` arm that only the last generator reaches; an outer
/// generator's back edge keeps `LOC(elt)`, the key alone).
fn stamp_comp_elt_span(
    inner: &mut Compiler,
    elt: &Expr,
    value: Option<&Expr>,
    append_op: OpCode,
    innermost: bool,
) {
    let end = match (append_op, value) {
        (OpCode::MapAdd, Some(v)) if innermost => v.span.end.0,
        _ => elt.span.end.0,
    };
    inner.current_span = (elt.span.start.0, end);
    inner.set_line_from(elt.span.start.0);
}

/// CPython's `loc` threaded through `codegen_comprehension_generator`:
/// the whole comprehension's location, carried by the async loop's
/// `GET_ANEXT`/send dance/`END_ASYNC_FOR` at any nesting depth.
#[derive(Clone, Copy)]
struct CompLoc {
    line: u32,
    span: (u32, u32),
}

impl CompLoc {
    fn current(c: &Compiler) -> Self {
        Self {
            line: c.current_line,
            span: c.current_span,
        }
    }

    fn apply(self, c: &mut Compiler) {
        c.current_line = self.line;
        c.current_span = self.span;
    }
}

fn compile_comp_body(
    inner: &mut Compiler,
    comp_loc: CompLoc,
    generators: &[Comprehension],
    depth: usize,
    // Iterators live on the stack while the body runs; CPython threads
    // this count separately from `depth` because the assignment-idiom
    // fast path below creates no iterator for its generator.
    iters_on_stack: usize,
    elt: &Expr,
    value: Option<&Expr>,
    append_op: OpCode,
) -> Result<(), CompileError> {
    if depth >= generators.len() {
        // Innermost: append (or map_add) to the accumulator. For
        // generator expressions, yield the element instead.
        match append_op {
            OpCode::MapAdd => {
                let val = value.expect("dict comp needs value");
                inner.compile_expr(elt)?;
                inner.compile_expr(val)?;
                let i = iters_on_stack + 1; // stack depth to accumulator
                                            // CPython stamps MAP_ADD with the `key: value` span
                                            // (test_compile test_multiline_dict_comprehension).
                inner.current_span = (elt.span.start.0, val.span.end.0);
                inner.set_line_from(elt.span.start.0);
                inner.emit(OpCode::MapAdd, i as u32);
            }
            OpCode::YieldValue => {
                inner.compile_expr(elt)?;
                // CPython stamps the yield (and the discarding POP_TOP)
                // with the element's span
                // (test_multiline_generator_expression).
                stamp_comp_elt_span(inner, elt, value, append_op, true);
                // CPython 3.13 own-yield shape: an async-generator
                // comprehension wraps the value (ASYNC_GEN_WRAP intrinsic)
                // so the runtime can tell a consumer value from an
                // inner-await passthrough; every own yield is `YIELD_VALUE
                // 0` + `RESUME 1`, and the sent value pushed on resume is
                // discarded.
                if inner.co.is_async_generator {
                    inner.emit(OpCode::AsyncGenWrap, 0);
                }
                inner.emit(OpCode::YieldValue, 0);
                inner.emit(OpCode::Resume, 1);
                inner.emit(OpCode::PopTop, 0);
            }
            _ => {
                inner.compile_expr(elt)?;
                let i = iters_on_stack + 1;
                // CPython stamps LIST_APPEND/SET_ADD with the element's
                // span (test_compile test_multiline_list_comprehension).
                inner.set_span(elt.span);
                inner.set_line_from(elt.span.start.0);
                inner.emit(append_op, i as u32);
            }
        }
        return Ok(());
    }
    let gen = &generators[depth];
    if gen.is_async {
        // depth==0: the outermost aiter is already on the stack — the
        // call site ran GET_AITER before `CALL 0` (CPython 3.14's
        // `codegen_comprehension_iter`), and an inlined comprehension
        // pushed the ready aiter the same way. Deeper generators
        // compute their own.
        if depth > 0 {
            inner.compile_expr(&gen.iter)?;
            inner.set_span(gen.iter.span);
            inner.set_line_from(gen.iter.span.start.0);
            inner.emit(OpCode::GetAiter, 0);
        }
        comp_loc.apply(inner);
        // Compute the live stack depth that should survive an
        // exception in this loop: the accumulator (if any) + the
        // aiters of every previous async generator + this aiter.
        let accumulator_depth = match append_op {
            OpCode::YieldValue => 0,
            _ => 1,
        };
        // At depth 0 the `.0` slot was converted in place (still one
        // iterator on the stack); deeper levels push a fresh aiter.
        let iters_here = if depth == 0 {
            iters_on_stack
        } else {
            iters_on_stack + 1
        };
        // Inlined comps sit on an unknown base stack depth — resolved
        // by `finish`'s static simulation via the sentinel.
        let cleanup_depth = if inner.inline_comp > 0 {
            HANDLER_DEPTH_SENTINEL
        } else {
            accumulator_depth + iters_here as u32
        };
        // `start` label, `SETUP_FINALLY except` guarding the
        // `__anext__` await, `POP_BLOCK` (located) after the dance.
        let loop_top = inner.use_label();
        inner.emit_setup(OpCode::SetupFinally);
        let guard_start = inner.next_offset();
        inner.emit(OpCode::GetAnext, 0);
        inner.emit_send_dance(3);
        // As in `compile_async_for`: only the `__anext__` await may end
        // the loop via StopAsyncIteration (bpo-44895).
        let dance_end = inner.next_offset();
        inner.emit_pop_block();
        inner.compile_assign(&gen.target)?;
        let mut filter_jumps = Vec::new();
        for cond in &gen.ifs {
            inner.compile_jump_if(cond, false, &mut filter_jumps)?;
        }
        compile_comp_body(
            inner,
            comp_loc,
            generators,
            depth + 1,
            iters_here,
            elt,
            value,
            append_op,
        )?;
        for jf in filter_jumps {
            let cur = inner.next_offset();
            inner.patch_jump(jf, cur);
        }
        // The loop's back edge carries the element span, as CPython
        // does (test_multiline_async_*_comprehension).
        stamp_comp_elt_span(inner, elt, value, append_op, depth + 1 == generators.len());
        let back = inner.emit(OpCode::JumpBackward, 0);
        inner.patch_jump(back, loop_top);
        comp_loc.apply(inner);
        let cleanup_target = inner.next_offset();
        inner.co.exception_table.push(ExcHandler {
            start: guard_start,
            end: dance_end,
            handler: cleanup_target,
            depth: cleanup_depth,
            push_lasti: false,
        });
        inner.emit(OpCode::EndAsyncFor, 0);
        return Ok(());
    }
    // For depth 0, the iterator is already on the stack (`.0` was
    // pushed). For deeper levels, push and iter the source.
    if depth > 0 {
        // CPython's temporary-variable "assignment idiom" fast path
        // (compiler_comprehension_generator): a sub-iterable that is a
        // one-element list/tuple display — `for y in [f(x)]` — compiles
        // to a plain assignment with no iterator and no FOR_ITER loop
        // (test_peepholer's test_assignment_idiom_in_comprehensions).
        let single = match &gen.iter.kind {
            ExprKind::List(elts) | ExprKind::Tuple(elts) if elts.len() == 1 => {
                let e0 = &elts[0];
                (!matches!(e0.kind, ExprKind::Starred(_))).then_some(e0)
            }
            _ => None,
        };
        if let Some(e0) = single {
            inner.compile_expr(e0)?;
            inner.compile_assign(&gen.target)?;
            let mut filter_jumps = Vec::new();
            for cond in &gen.ifs {
                inner.compile_jump_if(cond, false, &mut filter_jumps)?;
            }
            // No new iterator on the stack: the body runs exactly once.
            compile_comp_body(
                inner,
                comp_loc,
                generators,
                depth + 1,
                iters_on_stack,
                elt,
                value,
                append_op,
            )?;
            for jf in filter_jumps {
                let cur = inner.next_offset();
                inner.patch_jump(jf, cur);
            }
            return Ok(());
        }
        inner.compile_expr(&gen.iter)?;
        inner.set_span(gen.iter.span);
        inner.set_line_from(gen.iter.span.start.0);
        inner.emit(OpCode::GetIter, 0);
    }
    // FOR_ITER carries the *iterable expression's* location (CPython
    // compiler_comprehension_generator uses LOC(gen->iter)):
    // test_compile's test_line_number_genexp grades the loop head on
    // the iterable's line, distinct from the prologue's.
    let loop_top = inner.next_offset();
    inner.set_span(gen.iter.span);
    inner.set_line_from(gen.iter.span.start.0);
    let for_site = inner.emit(OpCode::ForIter, 0);
    let for_line = inner.current_line;
    inner.compile_assign(&gen.target)?;
    let mut filter_jumps = Vec::new();
    for cond in &gen.ifs {
        inner.compile_jump_if(cond, false, &mut filter_jumps)?;
    }
    let iters_here = if depth == 0 {
        iters_on_stack
    } else {
        iters_on_stack + 1
    };
    compile_comp_body(
        inner,
        comp_loc,
        generators,
        depth + 1,
        iters_here,
        elt,
        value,
        append_op,
    )?;
    for jf in filter_jumps {
        let cur = inner.next_offset();
        inner.patch_jump(jf, cur);
    }
    // The loop's back edge carries the element span, as CPython does
    // (test_multiline_*_comprehension).
    stamp_comp_elt_span(inner, elt, value, append_op, depth + 1 == generators.len());
    let back = inner.emit(OpCode::JumpBackward, 0);
    inner.patch_jump(back, loop_top);
    let after = inner.next_offset();
    inner.patch_jump(for_site, after);
    // Keep END_FOR on the iterator line (see statement-level for loop) so a
    // comprehension's loop exhaustion does not emit a spurious `line` event.
    inner.set_span(gen.iter.span);
    inner.current_line = for_line;
    // END_FOR + POP_ITER, as in the statement-level loop above.
    inner.emit(OpCode::EndFor, 0);
    inner.emit(OpCode::PopIter, 0);
    Ok(())
}

fn emit_cmp_op(compiler: &mut Compiler, op: CmpOp) {
    match op {
        CmpOp::Eq => {
            compiler.emit(OpCode::CompareOp, CompareKind::Eq as u32);
        }
        CmpOp::NotEq => {
            compiler.emit(OpCode::CompareOp, CompareKind::NotEq as u32);
        }
        CmpOp::Lt => {
            compiler.emit(OpCode::CompareOp, CompareKind::Lt as u32);
        }
        CmpOp::LtE => {
            compiler.emit(OpCode::CompareOp, CompareKind::LtE as u32);
        }
        CmpOp::Gt => {
            compiler.emit(OpCode::CompareOp, CompareKind::Gt as u32);
        }
        CmpOp::GtE => {
            compiler.emit(OpCode::CompareOp, CompareKind::GtE as u32);
        }
        CmpOp::Is => {
            compiler.emit(OpCode::IsOp, 0);
        }
        CmpOp::IsNot => {
            compiler.emit(OpCode::IsOp, 1);
        }
        CmpOp::In => {
            compiler.emit(OpCode::ContainsOp, 0);
        }
        CmpOp::NotIn => {
            compiler.emit(OpCode::ContainsOp, 1);
        }
    }
}

/// Clone a `FinallyFrame` deep enough to push onto a separate stack
/// (used while emitting an inline copy without losing the original).
fn clone_finally_frame(f: &FinallyFrame) -> FinallyFrame {
    let kind = match &f.kind {
        FinallyKind::Stmts(body) => FinallyKind::Stmts(body.clone()),
        FinallyKind::WithExit { line, span } => FinallyKind::WithExit {
            line: *line,
            span: *span,
        },
        FinallyKind::AsyncWithExit { line, span } => FinallyKind::AsyncWithExit {
            line: *line,
            span: *span,
        },
        FinallyKind::TryExcept => FinallyKind::TryExcept,
    };
    FinallyFrame {
        kind,
        loop_depth_at_push: f.loop_depth_at_push,
        id: f.id,
        pop_except_after: f.pop_except_after,
        region_hole_id: f.region_hole_id,
        exc_at_push: f.exc_at_push,
        handler_at_push: f.handler_at_push,
        rv_at_push: f.rv_at_push,
        seq: f.seq,
    }
}

fn bin_op_kind(op: BinOp) -> BinOpKind {
    match op {
        BinOp::Add => BinOpKind::Add,
        BinOp::Sub => BinOpKind::Sub,
        BinOp::Mult => BinOpKind::Mult,
        BinOp::MatMult => BinOpKind::MatMult,
        BinOp::Div => BinOpKind::Div,
        BinOp::Mod => BinOpKind::Mod,
        BinOp::Pow => BinOpKind::Pow,
        BinOp::LShift => BinOpKind::LShift,
        BinOp::RShift => BinOpKind::RShift,
        BinOp::BitOr => BinOpKind::BitOr,
        BinOp::BitXor => BinOpKind::BitXor,
        BinOp::BitAnd => BinOpKind::BitAnd,
        BinOp::FloorDiv => BinOpKind::FloorDiv,
    }
}

// ---------- AST helpers: walkers ----------

/// Walk inner function definitions reachable from `stmt` and
/// collect every name they reference that isn't bound locally
/// inside them. Caller intersects this with its own locals to
/// determine which need promoting to cells.
/// Reads made by a PEP 695 generic statement's header expressions —
/// type-parameter bounds/defaults and, for functions, parameter/return
/// annotations. These all evaluate inside the hidden
/// `<generic parameters of …>` scope (a nested scope of the one being
/// analyzed), so their outer-name reads must surface for cell
/// promotion. References to the statement's own type parameters are
/// excluded (they're hidden-scope locals). All of these scopes can see
/// an enclosing class namespace, so [`collect_class_visible_reads`]
/// applies the scan's `class_binds` shortcut to their own reads.
fn collect_pep695_header_reads(
    stmt: &Stmt,
    outer_bindings: &IndexMap<String, Binding>,
    out: &mut HashSet<String>,
    scan: &FreeScan,
) {
    let mut reads = HashSet::new();
    let mut read = |e: &Expr| collect_class_visible_reads(e, outer_bindings, &mut reads, scan);
    let (type_params, fn_parts) = match &stmt.kind {
        StmtKind::FunctionDef {
            type_params,
            args,
            returns,
            ..
        }
        | StmtKind::AsyncFunctionDef {
            type_params,
            args,
            returns,
            ..
        } => (type_params, Some((args, returns.as_deref()))),
        StmtKind::ClassDef { type_params, .. } => (type_params, None),
        // A `type` alias's value (and its parameters' bounds/defaults)
        // evaluate in annotation scopes, generic or not.
        StmtKind::TypeAlias {
            type_params, value, ..
        } => {
            read(value);
            for tp in type_params {
                if let TypeParamKind::TypeVar { bound: Some(b) } = &tp.kind {
                    read(b);
                }
                if let Some(d) = &tp.default {
                    read(d);
                }
            }
            let own: HashSet<&str> = type_params.iter().map(|tp| tp.name.as_str()).collect();
            for r in reads {
                if !own.contains(r.as_str()) {
                    out.insert(r);
                }
            }
            return;
        }
        _ => return,
    };
    if type_params.is_empty() {
        return;
    }
    for tp in type_params {
        if let TypeParamKind::TypeVar { bound: Some(b) } = &tp.kind {
            read(b);
        }
        if let Some(d) = &tp.default {
            read(d);
        }
    }
    if let Some((args, returns)) = fn_parts {
        for a in args
            .posonlyargs
            .iter()
            .chain(&args.args)
            .chain(&args.kwonlyargs)
            .chain(args.vararg.iter())
            .chain(args.kwarg.iter())
        {
            if let Some(ann) = &a.annotation {
                read(ann);
            }
        }
        if let Some(r) = returns {
            read(r);
        }
    } else if let StmtKind::ClassDef {
        bases, keywords, ..
    } = &stmt.kind
    {
        // A generic class's bases and keywords move into the hidden
        // scope, so their reads flow through it too (`def f(): T = str;
        // class C: class D[U](T): ...` needs `T` forwarded).
        for b in bases {
            read(b);
        }
        for k in keywords {
            read(&k.value);
        }
    }
    let own: HashSet<&str> = type_params.iter().map(|tp| tp.name.as_str()).collect();
    for r in reads {
        if !own.contains(r.as_str()) {
            out.insert(r);
        }
    }
}

/// CPython 3.14's `ste_needs_classdict` for a class body statement:
/// a child scope that can see the class scope closes over the body's
/// `__classdict__` cell. Since PEP 649 every `def` directly in the
/// class enters an `__annotate__` block (annotated or not,
/// `symtable_visit_annotations`), as does a class-level annotated
/// assignment (`symtable_visit_annotation`) — except under `from
/// __future__ import annotations`, when annotation blocks are detached
/// from the symbol table (`symtable_enter_existing_block`) and their
/// `__classdict__` use never reaches the class; PEP 695
/// type-parameter scopes (generic `def`/`class` headers, `type` alias
/// thunks) count regardless. Compound statements are the same block;
/// nested `def`/`class` *bodies* are their own scopes and are not
/// descended into.
fn stmt_needs_classdict(stmt: &Stmt, future_annotations: bool) -> bool {
    let rec = |stmts: &[Stmt]| {
        stmts
            .iter()
            .any(|s| stmt_needs_classdict(s, future_annotations))
    };
    match &stmt.kind {
        StmtKind::FunctionDef { type_params, .. }
        | StmtKind::AsyncFunctionDef { type_params, .. } => {
            !future_annotations || !type_params.is_empty()
        }
        StmtKind::ClassDef { type_params, .. } => !type_params.is_empty(),
        StmtKind::TypeAlias { .. } => true,
        StmtKind::Assign { value, .. } => expr_contains_typeparamfn(value),
        StmtKind::Expr(e) | StmtKind::Return(Some(e)) => expr_contains_typeparamfn(e),
        StmtKind::AnnAssign { value, .. } => {
            !future_annotations || value.as_ref().is_some_and(expr_contains_typeparamfn)
        }
        StmtKind::If { body, orelse, .. } | StmtKind::While { body, orelse, .. } => {
            rec(body) || rec(orelse)
        }
        StmtKind::For { body, orelse, .. } | StmtKind::AsyncFor { body, orelse, .. } => {
            rec(body) || rec(orelse)
        }
        StmtKind::With { body, .. } | StmtKind::AsyncWith { body, .. } => rec(body),
        StmtKind::Try {
            body,
            handlers,
            orelse,
            finalbody,
        } => rec(body) || rec(orelse) || rec(finalbody) || handlers.iter().any(|h| rec(&h.body)),
        StmtKind::Match { cases, .. } => cases.iter().any(|c| rec(&c.body)),
        _ => false,
    }
}

/// Recursive scan for a PEP 695 annotation-scope thunk anywhere in an
/// expression tree (including inside lambda bodies — a `type` alias
/// statement always desugars to a call tree of these at the top, but
/// scanning deep is cheap and safe).
fn expr_contains_typeparamfn(e: &Expr) -> bool {
    if matches!(e.kind, ExprKind::TypeParamFn { .. }) {
        return true;
    }
    let mut found = false;
    let mut check = |x: &Expr| {
        if !found {
            found = expr_contains_typeparamfn(x);
        }
    };
    match &e.kind {
        ExprKind::TypeParamFn { args, body } | ExprKind::Lambda { args, body } => {
            for d in args
                .defaults
                .iter()
                .chain(args.kw_defaults.iter().flatten())
            {
                check(d);
            }
            check(body);
        }
        ExprKind::Constant(_) | ExprKind::Name(_) => {}
        ExprKind::Attribute { value, .. } => check(value),
        ExprKind::Subscript { value, slice } => {
            check(value);
            check(slice);
        }
        ExprKind::Slice { lower, upper, step } => {
            for p in [lower, upper, step].into_iter().flatten() {
                check(p);
            }
        }
        ExprKind::BinOp { left, right, .. } => {
            check(left);
            check(right);
        }
        ExprKind::BoolOp { values, .. } => values.iter().for_each(&mut check),
        ExprKind::UnaryOp { operand, .. } => check(operand),
        ExprKind::Compare {
            left, comparators, ..
        } => {
            check(left);
            comparators.iter().for_each(&mut check);
        }
        ExprKind::IfExp { test, body, orelse } => {
            check(test);
            check(body);
            check(orelse);
        }
        ExprKind::NamedExpr { target, value } => {
            check(target);
            check(value);
        }
        ExprKind::Call {
            func,
            args,
            keywords,
        } => {
            check(func);
            args.iter().for_each(&mut check);
            for k in keywords {
                check(&k.value);
            }
        }
        ExprKind::Tuple(items) | ExprKind::List(items) | ExprKind::Set(items) => {
            items.iter().for_each(&mut check)
        }
        ExprKind::Dict { keys, values } => {
            keys.iter().flatten().for_each(&mut check);
            values.iter().for_each(&mut check);
        }
        ExprKind::ListComp { elt, generators }
        | ExprKind::SetComp { elt, generators }
        | ExprKind::GeneratorExp { elt, generators } => {
            check(elt);
            for g in generators {
                check(&g.target);
                check(&g.iter);
                g.ifs.iter().for_each(&mut check);
            }
        }
        ExprKind::DictComp {
            key,
            value,
            generators,
        } => {
            check(key);
            check(value);
            for g in generators {
                check(&g.target);
                check(&g.iter);
                g.ifs.iter().for_each(&mut check);
            }
        }
        ExprKind::Starred(inner) => check(inner),
        ExprKind::Yield(v) => {
            if let Some(v) = v {
                check(v);
            }
        }
        ExprKind::YieldFrom(v) | ExprKind::Await(v) => check(v),
        ExprKind::JoinedStr(parts) | ExprKind::TemplateStr(parts) => {
            parts.iter().for_each(&mut check)
        }
        ExprKind::FormattedValue {
            value, format_spec, ..
        }
        | ExprKind::Interpolation {
            value, format_spec, ..
        } => {
            check(value);
            if let Some(fs) = format_spec {
                check(fs);
            }
        }
    }
    found
}

fn collect_inner_free(
    stmt: &Stmt,
    outer_bindings: &IndexMap<String, Binding>,
    out: &mut HashSet<String>,
    scan: &FreeScan,
) {
    // A generic statement's type parameters are bound by its hidden
    // scope, so the names never reach this scope as free reads (an
    // enclosing `T = …` must not become a cell for a nested
    // `def f[T](x: T)`).
    if let Some(type_params) = generic_type_params(stmt) {
        let mut inner = HashSet::new();
        collect_pep695_header_reads(stmt, outer_bindings, &mut inner, scan);
        collect_inner_free_impl(stmt, outer_bindings, &mut inner, scan);
        remove_type_param_names(stmt, type_params, &mut inner);
        out.extend(inner);
        // Decorators and default values evaluate *outside* the hidden
        // scope, where a same-named enclosing binding is visible.
        for_each_outside_hidden_scope_expr(stmt, &mut |e| {
            collect_inner_free_expr(e, outer_bindings, out, scan)
        });
        return;
    }
    collect_pep695_header_reads(stmt, outer_bindings, out, scan);
    collect_inner_free_impl(stmt, outer_bindings, out, scan);
}

/// The expressions of a generic `def`/`class` that evaluate in the
/// enclosing scope rather than in the hidden type-parameter scope
/// (`codegen_function` / `codegen_class`: decorators and default
/// values run before the hidden scope is entered).
fn for_each_outside_hidden_scope_expr(stmt: &Stmt, f: &mut dyn FnMut(&Expr)) {
    match &stmt.kind {
        StmtKind::FunctionDef {
            args,
            decorator_list,
            ..
        }
        | StmtKind::AsyncFunctionDef {
            args,
            decorator_list,
            ..
        } => {
            decorator_list.iter().for_each(&mut *f);
            for d in args
                .defaults
                .iter()
                .chain(args.kw_defaults.iter().flatten())
            {
                f(d);
            }
        }
        StmtKind::ClassDef { decorator_list, .. } => decorator_list.iter().for_each(&mut *f),
        _ => {}
    }
}

/// The type parameters of a generic `def`/`class`, if any.
fn generic_type_params(stmt: &Stmt) -> Option<&[weavepy_parser::ast::TypeParam]> {
    match &stmt.kind {
        StmtKind::FunctionDef { type_params, .. }
        | StmtKind::AsyncFunctionDef { type_params, .. }
        | StmtKind::ClassDef { type_params, .. }
            if !type_params.is_empty() =>
        {
            Some(type_params)
        }
        _ => None,
    }
}

/// Drop a generic statement's own type-parameter names from a set of
/// names read by its header and body. A class's parameters bind
/// mangled against the class's name, and its (independently mangled)
/// body reads that spelling, so both forms go.
fn remove_type_param_names(
    stmt: &Stmt,
    type_params: &[weavepy_parser::ast::TypeParam],
    names: &mut HashSet<String>,
) {
    for tp in type_params {
        names.remove(&tp.name);
        if let StmtKind::ClassDef { name, .. } = &stmt.kind {
            names.remove(&mangle::mangle_ident(name, &tp.name));
        }
    }
}

fn collect_inner_free_impl(
    stmt: &Stmt,
    outer_bindings: &IndexMap<String, Binding>,
    out: &mut HashSet<String>,
    scan: &FreeScan,
) {
    match &stmt.kind {
        StmtKind::FunctionDef {
            args,
            body,
            decorator_list,
            returns,
            ..
        }
        | StmtKind::AsyncFunctionDef {
            args,
            body,
            decorator_list,
            returns,
            ..
        } => {
            // Decorators and default values evaluate in the *enclosing*
            // scope, but may themselves contain nested scopes
            // (`@lambda f: null(f)` — PEP 614) that close over our locals.
            for d in decorator_list {
                collect_inner_free_expr(d, outer_bindings, out, scan);
            }
            for d in args
                .defaults
                .iter()
                .chain(args.kw_defaults.iter().flatten())
            {
                collect_inner_free_expr(d, outer_bindings, out, scan);
            }
            // PEP 649: the annotations evaluate inside the def's
            // `__annotate__` scope, so every name they read is a read
            // from a nested scope (which binds nothing but its own
            // `format` parameter, and sees an enclosing class
            // namespace). Under PEP 563 they are strings and read
            // nothing.
            if !pep563_active() {
                for a in args
                    .posonlyargs
                    .iter()
                    .chain(&args.args)
                    .chain(&args.kwonlyargs)
                    .chain(&args.vararg)
                    .chain(&args.kwarg)
                {
                    if let Some(ann) = &a.annotation {
                        collect_class_visible_reads(ann, outer_bindings, out, scan);
                    }
                }
                if let Some(r) = returns {
                    collect_class_visible_reads(r, outer_bindings, out, scan);
                }
            }
            // The body is its own scope, no longer directly in a class
            // body.
            let scan = &scan.nested();
            let mut inner_locals: HashSet<String> = HashSet::new();
            for a in &args.posonlyargs {
                inner_locals.insert(a.name.clone());
            }
            for a in &args.args {
                inner_locals.insert(a.name.clone());
            }
            if let Some(va) = &args.vararg {
                inner_locals.insert(va.name.clone());
            }
            for a in &args.kwonlyargs {
                inner_locals.insert(a.name.clone());
            }
            if let Some(kw) = &args.kwarg {
                inner_locals.insert(kw.name.clone());
            }
            let mut inner_globals = HashSet::new();
            let mut inner_nonlocals = HashSet::new();
            let mut inner_assigned = HashSet::new();
            for s in body {
                collect_decls(
                    s,
                    &mut inner_globals,
                    &mut inner_nonlocals,
                    &mut inner_assigned,
                );
                collect_walrus_stmt(s, &mut inner_assigned);
            }
            inner_locals.extend(inner_assigned);
            // `nonlocal x` deliberately reaches up — record `x` as
            // needed-from-outer regardless of whether `outer_bindings`
            // knows about it yet (it'll be promoted on the way down).
            for n in &inner_nonlocals {
                out.insert(n.clone());
            }
            // Reads inside the inner that aren't locals there →
            // candidates for promotion.
            let mut inner_reads = HashSet::new();
            for s in body {
                collect_reads_stmt_fn(s, &mut inner_reads);
            }
            // Zero-arg `super()` implicitly captures `__class__` through
            // normal lexical scoping (RFC 0076 WS4): if the inner function
            // uses `super`, surface `__class__` as needed-from-outer so an
            // enclosing function's `__class__ = _cls` local is promoted to
            // a cell *before* emission — attrs' generated slots-
            // `__getattr__` wrapper is exactly this shape, and a post-hoc
            // promotion left the already-emitted store writing the dead
            // local slot ("cannot access free variable '__class__'").
            if !inner_locals.contains("__class__") && body_reads_super_or_class(body) {
                out.insert("__class__".to_owned());
            }
            for r in inner_reads {
                if !inner_locals.contains(&r) && !inner_globals.contains(&r) {
                    out.insert(r);
                }
            }
            // Recurse into inner function bodies — their inner
            // functions may pull names from us too, but only for names
            // the inner function doesn't itself bind: a grandchild's
            // read of the child's own parameter or local resolves to
            // the child's cell, not ours (`def _tp_cache(func): def
            // decorator(func): def inner(): func` must not promote the
            // outer `func`). `nonlocal` in the child reaches through.
            let mut deeper = HashSet::new();
            for s in body {
                collect_inner_free(s, outer_bindings, &mut deeper, scan);
            }
            for n in deeper {
                if inner_nonlocals.contains(&n)
                    || (!inner_locals.contains(&n) && !inner_globals.contains(&n))
                {
                    out.insert(n);
                }
            }
        }
        StmtKind::ClassDef {
            name,
            bases,
            keywords,
            body,
            decorator_list,
            ..
        } => {
            // The class body itself is a nested scope. Any name it
            // (or its inner methods) read that isn't bound inside
            // surfaces here so the outer scope can promote it.
            for d in decorator_list {
                collect_inner_free_expr(d, outer_bindings, out, scan);
            }
            for b in bases {
                collect_inner_free_expr(b, outer_bindings, out, scan);
            }
            for k in keywords {
                collect_inner_free_expr(&k.value, outer_bindings, out, scan);
            }
            // Analyze the body as the compiler will actually emit it:
            // private names mangle against the class (`__T` inside
            // `class Foo` reads `_Foo__T`), so the *mangled* spelling
            // is what must be promoted in the enclosing scope
            // (`class Foo[__T]: param = __T` reaches the hidden
            // scope's `_Foo__T` cell).
            let mangled_body;
            let body: &[Stmt] = if name.trim_start_matches('_').is_empty() {
                body
            } else {
                let mut b = body.clone();
                crate::mangle::mangle_class_body(name, &mut b);
                mangled_body = b;
                &mangled_body
            };
            let mut class_assigned = HashSet::new();
            let mut class_globals = HashSet::new();
            let mut class_nonlocals = HashSet::new();
            for s in body {
                collect_assigned(s, &mut class_assigned);
                collect_decls(
                    s,
                    &mut class_globals,
                    &mut class_nonlocals,
                    &mut HashSet::new(),
                );
            }
            // Names referenced *anywhere* in the class body (including
            // method bodies) that aren't bound inside the class are
            // candidates for outer-scope free promotion.
            let mut class_reads = HashSet::new();
            for s in body {
                collect_reads_stmt(s, &mut class_reads);
            }
            let mut from_body = HashSet::new();
            for r in class_reads {
                if !class_assigned.contains(&r) {
                    from_body.insert(r);
                }
            }
            // The nested scopes are analyzed as *the class body* sees
            // them, not as this scope does: an inlined comprehension in
            // a class body still resolves its reads past the class
            // namespace (`class C[T]: T = "x"; [T for _ in y]` reaches
            // the hidden scope's `T`, not the class attribute), so
            // `class_body` must be set even when the enclosing scope
            // is a function or a PEP 695 annotation scope. Class-visible
            // annotation scopes resolve the body's own names through
            // the class dict (`class_binds`), matching what
            // `build_class_body`'s scan will decide.
            let class_binds: HashSet<String> = class_assigned
                .iter()
                .filter(|n| !class_nonlocals.contains(*n))
                .chain(class_globals.iter())
                .cloned()
                .collect();
            let class_scan = FreeScan {
                inline_comps: true,
                async_ok: false,
                class_body: true,
                class_binds: Some(class_binds),
            };
            for s in body {
                collect_inner_free(s, outer_bindings, &mut from_body, &class_scan);
            }
            // A method's zero-argument `super()` (or explicit `__class__`
            // read) is satisfied by *this* class's own `__class__` cell
            // (symtable `drop_class_free`); it never reaches an enclosing
            // function — `class KeyedRef` inside `_WeakValueDictionary.
            // __init__` must not make `__init__` close over a `__class__`
            // the outer class would then have to own.
            from_body.remove("__class__");
            out.extend(from_body);
        }
        StmtKind::If { test, body, orelse } | StmtKind::While { test, body, orelse } => {
            collect_inner_free_expr(test, outer_bindings, out, scan);
            for s in body {
                collect_inner_free(s, outer_bindings, out, scan);
            }
            for s in orelse {
                collect_inner_free(s, outer_bindings, out, scan);
            }
        }
        StmtKind::For {
            target,
            iter,
            body,
            orelse,
        }
        | StmtKind::AsyncFor {
            target,
            iter,
            body,
            orelse,
        } => {
            // The iterable expression evaluates in the loop's
            // surrounding scope. If it contains a comprehension that
            // captures one of our locals (a frequent shape — e.g.
            // `for x in foo([item for item in items])`), the outer
            // scope still needs to know so it can promote the local
            // to a cell. Historically the iter was skipped, which
            // produced an unfilled cell at the comp-call site.
            collect_inner_free_expr(target, outer_bindings, out, scan);
            collect_inner_free_expr(iter, outer_bindings, out, scan);
            for s in body {
                collect_inner_free(s, outer_bindings, out, scan);
            }
            for s in orelse {
                collect_inner_free(s, outer_bindings, out, scan);
            }
        }
        StmtKind::Try {
            body,
            handlers,
            orelse,
            finalbody,
        } => {
            for s in body {
                collect_inner_free(s, outer_bindings, out, scan);
            }
            for h in handlers {
                if let Some(t) = &h.type_ {
                    collect_inner_free_expr(t, outer_bindings, out, scan);
                }
                for s in &h.body {
                    collect_inner_free(s, outer_bindings, out, scan);
                }
            }
            for s in orelse {
                collect_inner_free(s, outer_bindings, out, scan);
            }
            for s in finalbody {
                collect_inner_free(s, outer_bindings, out, scan);
            }
        }
        StmtKind::Raise { exc, cause } => {
            if let Some(e) = exc {
                collect_inner_free_expr(e, outer_bindings, out, scan);
            }
            if let Some(c) = cause {
                collect_inner_free_expr(c, outer_bindings, out, scan);
            }
        }
        StmtKind::With { items, body } | StmtKind::AsyncWith { items, body } => {
            for it in items {
                collect_inner_free_expr(&it.context_expr, outer_bindings, out, scan);
            }
            for s in body {
                collect_inner_free(s, outer_bindings, out, scan);
            }
        }
        StmtKind::Expr(e) | StmtKind::Return(Some(e)) => {
            collect_inner_free_expr(e, outer_bindings, out, scan);
        }
        StmtKind::Assign { value, .. } => {
            collect_inner_free_expr(value, outer_bindings, out, scan);
        }
        StmtKind::AugAssign { value, .. } => {
            collect_inner_free_expr(value, outer_bindings, out, scan);
        }
        StmtKind::AnnAssign {
            target,
            annotation,
            value,
            simple,
        } => {
            if let Some(value) = value {
                collect_inner_free_expr(value, outer_bindings, out, scan);
            }
            // PEP 649: a class body's simple-name annotation evaluates
            // inside the class's `__annotate__` scope, so its reads are
            // a nested scope's reads (a function body's annotations
            // are never evaluated; PEP 563 strings read nothing).
            if scan.class_body
                && *simple
                && matches!(target.kind, ExprKind::Name(_))
                && !pep563_active()
            {
                collect_reads_expr(annotation, out);
            }
        }
        StmtKind::Assert { test, msg } => {
            // `assert <comp> [, <comp>]` evaluates both expressions in this
            // scope. A comprehension here captures our locals just like one
            // in an `Expr`/`Assign` statement, so its outer reads must drive
            // cell promotion — otherwise the pre-pass leaves the name a plain
            // local (STORE_FAST) while `compile_comprehension` later promotes
            // it to a cell, and the comp-call reads an unfilled cell
            // (`UnboundLocalError`). Mirrors `collect_reads_stmt`.
            collect_inner_free_expr(test, outer_bindings, out, scan);
            if let Some(m) = msg {
                collect_inner_free_expr(m, outer_bindings, out, scan);
            }
        }
        StmtKind::Delete(targets) => {
            // `del x[<comp>]` / `del x.attr` evaluate the container/slice in
            // this scope; a comprehension in a subscript captures our locals.
            for t in targets {
                collect_inner_free_expr(t, outer_bindings, out, scan);
            }
        }
        StmtKind::Match { subject, cases } => {
            // The subject and every guard are ordinary expressions evaluated
            // in this scope and may contain capturing comprehensions; case
            // bodies are statements that recurse normally.
            collect_inner_free_expr(subject, outer_bindings, out, scan);
            for c in cases {
                if let Some(g) = &c.guard {
                    collect_inner_free_expr(g, outer_bindings, out, scan);
                }
                for s in &c.body {
                    collect_inner_free(s, outer_bindings, out, scan);
                }
            }
        }
        _ => {}
    }
}

/// `True` when a method body references `super` or `__class__` so the
/// compiler knows to capture the class's `__class__` cell.
fn method_references_class(body: &[Stmt]) -> bool {
    let mut globals = HashSet::new();
    let mut nonlocals = HashSet::new();
    let mut assigned = HashSet::new();
    for s in body {
        collect_decls(s, &mut globals, &mut nonlocals, &mut assigned);
        collect_walrus_stmt(s, &mut assigned);
    }
    // `nonlocal __class__` (test_super's pathology-repair tearDown) binds
    // the implicit class cell for *writing* without ever reading it —
    // CPython's symtable treats the declaration itself as a use.
    if nonlocals.contains("__class__") {
        return true;
    }
    // A method that *binds* `__class__` itself (`__class__ =
    // loader_state['__class__']` in importlib's `_LazyModule.
    // __getattribute__`) has a plain local (or an explicit global):
    // the symtable's `DEF_LOCAL` wins over the implicit use, so the
    // class cell is never claimed.
    if globals.contains("__class__") || assigned.contains("__class__") {
        return false;
    }
    body_reads_super_or_class(body)
}

/// CPython's "Special-case super: it counts as a use of `__class__`"
/// (symtable.c) applies in function-like scopes, and a nested class's
/// methods resolve their `super()` against *that* class's own cell. So:
/// does `body` read `super` or `__class__` with nested functions
/// descended into but nested class bodies opaque (their headers —
/// decorators, bases, keywords — still evaluate in this scope)?
fn body_reads_super_or_class(body: &[Stmt]) -> bool {
    fn strip_class_bodies(stmts: &mut [Stmt]) {
        for s in stmts.iter_mut() {
            match &mut s.kind {
                StmtKind::ClassDef { body, .. } => body.clear(),
                StmtKind::FunctionDef { body, .. }
                | StmtKind::AsyncFunctionDef { body, .. }
                | StmtKind::With { body, .. }
                | StmtKind::AsyncWith { body, .. } => strip_class_bodies(body),
                StmtKind::If { body, orelse, .. }
                | StmtKind::While { body, orelse, .. }
                | StmtKind::For { body, orelse, .. }
                | StmtKind::AsyncFor { body, orelse, .. } => {
                    strip_class_bodies(body);
                    strip_class_bodies(orelse);
                }
                StmtKind::Try {
                    body,
                    handlers,
                    orelse,
                    finalbody,
                } => {
                    strip_class_bodies(body);
                    for h in handlers.iter_mut() {
                        strip_class_bodies(&mut h.body);
                    }
                    strip_class_bodies(orelse);
                    strip_class_bodies(finalbody);
                }
                StmtKind::Match { cases, .. } => {
                    for c in cases.iter_mut() {
                        strip_class_bodies(&mut c.body);
                    }
                }
                _ => {}
            }
        }
    }
    let has_nested_class = |stmts: &[Stmt]| -> bool {
        fn any_class(stmts: &[Stmt]) -> bool {
            stmts.iter().any(|s| match &s.kind {
                StmtKind::ClassDef { .. } => true,
                StmtKind::FunctionDef { body, .. }
                | StmtKind::AsyncFunctionDef { body, .. }
                | StmtKind::With { body, .. }
                | StmtKind::AsyncWith { body, .. } => any_class(body),
                StmtKind::If { body, orelse, .. }
                | StmtKind::While { body, orelse, .. }
                | StmtKind::For { body, orelse, .. }
                | StmtKind::AsyncFor { body, orelse, .. } => any_class(body) || any_class(orelse),
                StmtKind::Try {
                    body,
                    handlers,
                    orelse,
                    finalbody,
                } => {
                    any_class(body)
                        || any_class(orelse)
                        || any_class(finalbody)
                        || handlers.iter().any(|h| any_class(&h.body))
                }
                StmtKind::Match { cases, .. } => cases.iter().any(|c| any_class(&c.body)),
                _ => false,
            })
        }
        any_class(stmts)
    };
    let mut reads = HashSet::new();
    if has_nested_class(body) {
        // Only clone when there is a class body to blank out.
        let mut stripped = body.to_vec();
        strip_class_bodies(&mut stripped);
        for s in &stripped {
            collect_reads_stmt_fn(s, &mut reads);
        }
    } else {
        for s in body {
            collect_reads_stmt_fn(s, &mut reads);
        }
    }
    reads.contains("super") || reads.contains("__class__")
}

/// `True` when a `def` compiled directly inside a class body (at any
/// block-statement depth — `if`/`for`/`with`/`try`/`match` arms still
/// compile at class scope) will claim the implicit `__class__` free
/// variable. This mirrors the exact predicate `build_function_object`
/// applies per method (`method_references_class`), so the class-side
/// cell decision can never disagree with a child's claim. Nested
/// classes provide their own `__class__` cell, so they are opaque.
fn class_body_defs_claim_class_cell(body: &[Stmt]) -> bool {
    body.iter().any(|s| match &s.kind {
        StmtKind::FunctionDef { body: fbody, .. }
        | StmtKind::AsyncFunctionDef { body: fbody, .. } => method_references_class(fbody),
        StmtKind::ClassDef { .. } => false,
        StmtKind::If { body, orelse, .. } | StmtKind::While { body, orelse, .. } => {
            class_body_defs_claim_class_cell(body) || class_body_defs_claim_class_cell(orelse)
        }
        StmtKind::For { body, orelse, .. } | StmtKind::AsyncFor { body, orelse, .. } => {
            class_body_defs_claim_class_cell(body) || class_body_defs_claim_class_cell(orelse)
        }
        StmtKind::With { body, .. } | StmtKind::AsyncWith { body, .. } => {
            class_body_defs_claim_class_cell(body)
        }
        StmtKind::Try {
            body,
            handlers,
            orelse,
            finalbody,
        } => {
            class_body_defs_claim_class_cell(body)
                || handlers
                    .iter()
                    .any(|h| class_body_defs_claim_class_cell(&h.body))
                || class_body_defs_claim_class_cell(orelse)
                || class_body_defs_claim_class_cell(finalbody)
        }
        StmtKind::Match { cases, .. } => cases
            .iter()
            .any(|c| class_body_defs_claim_class_cell(&c.body)),
        _ => false,
    })
}

/// The docstring of a body, per CPython's rule: the first statement is a
/// bare string-literal *expression statement*. An assignment whose RHS is
/// a string (`x = "s"`), an f-string, or any non-string first statement is
/// **not** a docstring. Returns the string slice when present.
fn first_stmt_docstring(body: &[Stmt]) -> Option<&str> {
    match &body.first()?.kind {
        StmtKind::Expr(expr) => match &expr.kind {
            ExprKind::Constant(AstConstant::Str(s)) => Some(s.as_str()),
            _ => None,
        },
        _ => None,
    }
}

/// CPython 3.13's `_PyCompile_CleanDoc` (compile.c, gh-81283): docstrings
/// are cleaned at compile time like `inspect.cleandoc` — tabs expanded
/// (tab stops of 8), the first line's leading spaces stripped, and the
/// minimum space-indent of the non-blank continuation lines removed from
/// every continuation line. Unlike `inspect.cleandoc`, leading/trailing
/// blank lines are *kept* (preserves line numbers). Only ASCII spaces
/// count as margin, exactly as upstream.
fn clean_docstring(doc: &str) -> String {
    // str.expandtabs() with the default tabsize of 8: each code point
    // advances the column by one; '\n'/'\r' reset it.
    let doc: std::borrow::Cow<'_, str> = if doc.contains('\t') {
        let mut out = String::with_capacity(doc.len());
        let mut col = 0usize;
        for ch in doc.chars() {
            match ch {
                '\t' => {
                    let pad = 8 - col % 8;
                    out.extend(std::iter::repeat_n(' ', pad));
                    col += pad;
                }
                '\n' | '\r' => {
                    out.push(ch);
                    col = 0;
                }
                _ => {
                    out.push(ch);
                    col += 1;
                }
            }
        }
        out.into()
    } else {
        doc.into()
    };

    let lines: Vec<&str> = doc.split('\n').collect();
    let leading_spaces = |line: &str| line.len() - line.trim_start_matches(' ').len();

    // Minimum indentation of non-blank lines after the first (a line of
    // only spaces is blank and contributes nothing).
    let mut margin = usize::MAX;
    for line in &lines[1..] {
        let n = leading_spaces(line);
        if n < line.len() {
            margin = margin.min(n);
        }
    }
    if margin == usize::MAX {
        margin = 0;
    }

    let first_indent = leading_spaces(lines[0]);
    if first_indent == 0 && margin == 0 {
        return doc.into_owned();
    }

    let mut out = String::with_capacity(doc.len());
    out.push_str(&lines[0][first_indent..]);
    for line in &lines[1..] {
        out.push('\n');
        // Blank (all-space) lines may hold fewer spaces than the margin.
        out.push_str(&line[margin.min(leading_spaces(line))..]);
    }
    out
}

/// `True` if any statement in `body` contains a `yield` or `yield from`
/// in the immediate scope. Does NOT recurse into nested `def` / `lambda`
/// / comprehension bodies — those have their own scopes.
fn body_is_generator(body: &[Stmt]) -> bool {
    body.iter().any(stmt_contains_yield)
}

/// Pre-scan for PyCF_ALLOW_TOP_LEVEL_AWAIT (RFC 0052): does the module
/// body use `await` / `async for` / `async with` / an inline-awaited
/// async comprehension at its own scope level? When it does, the module
/// code object must be a coroutine, and — like generator functions —
/// that has to be known *before* emission so `RETURN_GENERATOR` can be
/// the first instruction (the VM's generator bootstrap stops there).
/// Nested `def`/`class` bodies don't count; their awaits are their own.
fn body_has_top_level_await(body: &[Stmt]) -> bool {
    fn expr_hit(e: &Expr) -> bool {
        match &e.kind {
            ExprKind::Await(_) => true,
            // Lambda bodies are their own scope, but default values
            // evaluate here.
            ExprKind::Lambda { args, .. } | ExprKind::TypeParamFn { args, .. } => {
                args.defaults.iter().any(expr_hit)
                    || args.kw_defaults.iter().flatten().any(expr_hit)
            }
            // An inline-awaited async list/set/dict comprehension awaits
            // *in this scope*; so does anything in the first `for`
            // clause's iterable (evaluated here, passed in as `.0`) —
            // including a nested async comprehension
            // (`[1 for x in {y async for y in a}]`).
            ExprKind::ListComp { elt, generators } | ExprKind::SetComp { elt, generators } => {
                comp_clause_is_async(generators, elt, None)
                    || generators.first().is_some_and(|g| expr_hit(&g.iter))
            }
            ExprKind::DictComp {
                key,
                value,
                generators,
            } => {
                comp_clause_is_async(generators, key, Some(value))
                    || generators.first().is_some_and(|g| expr_hit(&g.iter))
            }
            // An async genexp is just an async-generator object — only
            // its first iterable evaluates here.
            ExprKind::GeneratorExp { generators, .. } => {
                generators.first().is_some_and(|g| expr_hit(&g.iter))
            }
            ExprKind::Yield(v) => v.as_deref().is_some_and(expr_hit),
            ExprKind::YieldFrom(v) => expr_hit(v),
            ExprKind::JoinedStr(parts) | ExprKind::TemplateStr(parts) => parts.iter().any(expr_hit),
            ExprKind::FormattedValue {
                value, format_spec, ..
            }
            | ExprKind::Interpolation {
                value, format_spec, ..
            } => expr_hit(value) || format_spec.as_deref().is_some_and(expr_hit),
            ExprKind::BinOp { left, right, .. } => expr_hit(left) || expr_hit(right),
            ExprKind::BoolOp { values, .. } => values.iter().any(expr_hit),
            ExprKind::UnaryOp { operand, .. } => expr_hit(operand),
            ExprKind::Compare {
                left, comparators, ..
            } => expr_hit(left) || comparators.iter().any(expr_hit),
            ExprKind::IfExp { test, body, orelse } => {
                expr_hit(test) || expr_hit(body) || expr_hit(orelse)
            }
            ExprKind::NamedExpr { target, value } => expr_hit(target) || expr_hit(value),
            ExprKind::Call {
                func,
                args,
                keywords,
            } => {
                expr_hit(func)
                    || args.iter().any(expr_hit)
                    || keywords.iter().any(|k| expr_hit(&k.value))
            }
            ExprKind::Attribute { value, .. } => expr_hit(value),
            ExprKind::Subscript { value, slice } => expr_hit(value) || expr_hit(slice),
            ExprKind::Slice { lower, upper, step } => {
                lower.as_deref().is_some_and(expr_hit)
                    || upper.as_deref().is_some_and(expr_hit)
                    || step.as_deref().is_some_and(expr_hit)
            }
            ExprKind::Tuple(items) | ExprKind::List(items) | ExprKind::Set(items) => {
                items.iter().any(expr_hit)
            }
            ExprKind::Dict { keys, values } => {
                keys.iter().any(|k| k.as_ref().is_some_and(expr_hit)) || values.iter().any(expr_hit)
            }
            ExprKind::Starred(inner) => expr_hit(inner),
            ExprKind::Constant(_) | ExprKind::Name(_) => false,
        }
    }
    fn stmt_hit(stmt: &Stmt) -> bool {
        match &stmt.kind {
            StmtKind::AsyncFor { .. } | StmtKind::AsyncWith { .. } => true,
            StmtKind::FunctionDef { .. }
            | StmtKind::AsyncFunctionDef { .. }
            | StmtKind::ClassDef { .. } => false,
            StmtKind::Expr(e) => expr_hit(e),
            StmtKind::Assign { targets, value } => expr_hit(value) || targets.iter().any(expr_hit),
            StmtKind::AugAssign { target, value, .. } => expr_hit(target) || expr_hit(value),
            StmtKind::AnnAssign { target, value, .. } => {
                expr_hit(target) || value.as_ref().is_some_and(expr_hit)
            }
            StmtKind::Return(v) => v.as_ref().is_some_and(expr_hit),
            StmtKind::If { test, body, orelse } | StmtKind::While { test, body, orelse } => {
                expr_hit(test) || body.iter().any(stmt_hit) || orelse.iter().any(stmt_hit)
            }
            StmtKind::For {
                target,
                iter,
                body,
                orelse,
            } => {
                expr_hit(target)
                    || expr_hit(iter)
                    || body.iter().any(stmt_hit)
                    || orelse.iter().any(stmt_hit)
            }
            StmtKind::With { items, body } => {
                items.iter().any(|w| {
                    expr_hit(&w.context_expr) || w.optional_vars.as_ref().is_some_and(expr_hit)
                }) || body.iter().any(stmt_hit)
            }
            StmtKind::Try {
                body,
                handlers,
                orelse,
                finalbody,
            } => {
                body.iter().any(stmt_hit)
                    || handlers.iter().any(|h| h.body.iter().any(stmt_hit))
                    || orelse.iter().any(stmt_hit)
                    || finalbody.iter().any(stmt_hit)
            }
            StmtKind::Raise { exc, cause } => {
                exc.as_ref().is_some_and(expr_hit) || cause.as_ref().is_some_and(expr_hit)
            }
            StmtKind::Match { subject, cases } => {
                expr_hit(subject)
                    || cases.iter().any(|c| {
                        c.guard.as_ref().is_some_and(expr_hit) || c.body.iter().any(stmt_hit)
                    })
            }
            StmtKind::Global(_)
            | StmtKind::Nonlocal(_)
            | StmtKind::Import(_)
            | StmtKind::ImportFrom { .. }
            | StmtKind::Pass
            | StmtKind::Break
            | StmtKind::Continue => false,
            StmtKind::Delete(targets) => targets.iter().any(expr_hit),
            StmtKind::Assert { test, msg } => expr_hit(test) || msg.as_ref().is_some_and(expr_hit),
            // `await` is rejected inside type-alias values at parse time.
            StmtKind::TypeAlias { .. } => false,
        }
    }
    body.iter().any(stmt_hit)
}

fn stmt_contains_yield(stmt: &Stmt) -> bool {
    match &stmt.kind {
        StmtKind::FunctionDef { .. }
        | StmtKind::AsyncFunctionDef { .. }
        | StmtKind::ClassDef { .. } => false,
        StmtKind::Expr(e) => expr_contains_yield(e),
        StmtKind::Assign { targets, value } => {
            expr_contains_yield(value) || targets.iter().any(expr_contains_yield)
        }
        StmtKind::AugAssign { target, value, .. } => {
            expr_contains_yield(target) || expr_contains_yield(value)
        }
        StmtKind::AnnAssign { target, value, .. } => {
            expr_contains_yield(target) || value.as_ref().is_some_and(expr_contains_yield)
        }
        StmtKind::Return(v) => v.as_ref().is_some_and(expr_contains_yield),
        StmtKind::If { test, body, orelse } | StmtKind::While { test, body, orelse } => {
            expr_contains_yield(test)
                || body.iter().any(stmt_contains_yield)
                || orelse.iter().any(stmt_contains_yield)
        }
        StmtKind::For {
            target,
            iter,
            body,
            orelse,
        }
        | StmtKind::AsyncFor {
            target,
            iter,
            body,
            orelse,
        } => {
            expr_contains_yield(target)
                || expr_contains_yield(iter)
                || body.iter().any(stmt_contains_yield)
                || orelse.iter().any(stmt_contains_yield)
        }
        StmtKind::With { items, body } | StmtKind::AsyncWith { items, body } => {
            items.iter().any(|w| {
                expr_contains_yield(&w.context_expr)
                    || w.optional_vars.as_ref().is_some_and(expr_contains_yield)
            }) || body.iter().any(stmt_contains_yield)
        }
        StmtKind::Try {
            body,
            handlers,
            orelse,
            finalbody,
        } => {
            body.iter().any(stmt_contains_yield)
                || handlers
                    .iter()
                    .any(|h| h.body.iter().any(stmt_contains_yield))
                || orelse.iter().any(stmt_contains_yield)
                || finalbody.iter().any(stmt_contains_yield)
        }
        StmtKind::Raise { exc, cause } => {
            exc.as_ref().is_some_and(expr_contains_yield)
                || cause.as_ref().is_some_and(expr_contains_yield)
        }
        StmtKind::Match { subject, cases } => {
            expr_contains_yield(subject)
                || cases.iter().any(|c| {
                    c.guard.as_ref().is_some_and(expr_contains_yield)
                        || c.body.iter().any(stmt_contains_yield)
                })
        }
        StmtKind::Global(_)
        | StmtKind::Nonlocal(_)
        | StmtKind::Import(_)
        | StmtKind::ImportFrom { .. }
        | StmtKind::Pass
        | StmtKind::Break
        | StmtKind::Continue => false,
        StmtKind::Delete(targets) => targets.iter().any(expr_contains_yield),
        StmtKind::Assert { test, msg } => {
            expr_contains_yield(test) || msg.as_ref().is_some_and(expr_contains_yield)
        }
        // `yield` is rejected inside type-alias values at parse time.
        StmtKind::TypeAlias { .. } => false,
    }
}

fn expr_contains_yield(expr: &Expr) -> bool {
    match &expr.kind {
        ExprKind::Yield(_) | ExprKind::YieldFrom(_) => true,
        ExprKind::Await(inner) => expr_contains_yield(inner),
        // A lambda body runs in its own scope, but its *default argument
        // values* are evaluated in the enclosing scope — so a `yield` there
        // belongs to the enclosing function, e.g. `def f(): lambda x=(yield): 1`
        // makes `f` a generator. The body is excluded.
        ExprKind::Lambda { args, .. } | ExprKind::TypeParamFn { args, .. } => {
            args.defaults.iter().any(expr_contains_yield)
                || args.kw_defaults.iter().flatten().any(expr_contains_yield)
        }
        // A comprehension runs in its own scope, but the *leftmost* `for`
        // clause's iterable is evaluated in the enclosing scope and passed
        // in as the `.0` argument. A `yield` there therefore belongs to the
        // enclosing function and makes it a generator — e.g.
        // `def f(): list(i for i in [(yield 26)])`. (A `yield` anywhere else
        // in a comprehension is a SyntaxError, so only the first iterable
        // can contribute.)
        ExprKind::GeneratorExp { generators, .. } => generators
            .first()
            .is_some_and(|g| expr_contains_yield(&g.iter)),
        ExprKind::JoinedStr(parts) | ExprKind::TemplateStr(parts) => {
            parts.iter().any(expr_contains_yield)
        }
        ExprKind::FormattedValue {
            value, format_spec, ..
        }
        | ExprKind::Interpolation {
            value, format_spec, ..
        } => expr_contains_yield(value) || format_spec.as_deref().is_some_and(expr_contains_yield),
        ExprKind::BinOp { left, right, .. } => {
            expr_contains_yield(left) || expr_contains_yield(right)
        }
        ExprKind::BoolOp { values, .. } => values.iter().any(expr_contains_yield),
        ExprKind::UnaryOp { operand, .. } => expr_contains_yield(operand),
        ExprKind::Compare {
            left, comparators, ..
        } => expr_contains_yield(left) || comparators.iter().any(expr_contains_yield),
        ExprKind::IfExp { test, body, orelse } => {
            expr_contains_yield(test) || expr_contains_yield(body) || expr_contains_yield(orelse)
        }
        ExprKind::NamedExpr { target, value } => {
            expr_contains_yield(target) || expr_contains_yield(value)
        }
        ExprKind::Call {
            func,
            args,
            keywords,
        } => {
            expr_contains_yield(func)
                || args.iter().any(expr_contains_yield)
                || keywords.iter().any(|k| expr_contains_yield(&k.value))
        }
        ExprKind::Attribute { value, .. } => expr_contains_yield(value),
        ExprKind::Subscript { value, slice } => {
            expr_contains_yield(value) || expr_contains_yield(slice)
        }
        ExprKind::Slice { lower, upper, step } => {
            lower.as_deref().is_some_and(expr_contains_yield)
                || upper.as_deref().is_some_and(expr_contains_yield)
                || step.as_deref().is_some_and(expr_contains_yield)
        }
        ExprKind::Tuple(items) | ExprKind::List(items) | ExprKind::Set(items) => {
            items.iter().any(expr_contains_yield)
        }
        ExprKind::Dict { keys, values } => {
            keys.iter()
                .any(|k| k.as_ref().is_some_and(expr_contains_yield))
                || values.iter().any(expr_contains_yield)
        }
        ExprKind::ListComp { generators, .. }
        | ExprKind::SetComp { generators, .. }
        | ExprKind::DictComp { generators, .. } => generators
            .first()
            .is_some_and(|g| expr_contains_yield(&g.iter)),
        ExprKind::Starred(inner) => expr_contains_yield(inner),
        ExprKind::Constant(_) | ExprKind::Name(_) => false,
    }
}

/// `true` if `expr` contains an `await` at the surface scope (does
/// not descend into nested lambdas or comprehensions). Used to mark
/// comprehensions as coroutines.
/// `yield`/`yield from` anywhere in a comprehension's *own* scope: the
/// element/value, filters, targets and non-outermost iterables, plus
/// the outermost iterable of any comprehension nested in those
/// positions (it evaluates in this scope). Stops at scope boundaries
/// (lambda bodies and the rest of a nested comprehension) — those
/// scopes run their own placement checks when they compile.
fn comp_scope_contains_yield(
    elt: &Expr,
    value: Option<&Expr>,
    generators: &[Comprehension],
) -> bool {
    expr_yields_in_scope(elt)
        || value.is_some_and(expr_yields_in_scope)
        || generators.iter().enumerate().any(|(gi, g)| {
            (gi > 0 && expr_yields_in_scope(&g.iter))
                || expr_yields_in_scope(&g.target)
                || g.ifs.iter().any(expr_yields_in_scope)
        })
}

fn expr_yields_in_scope(expr: &Expr) -> bool {
    match &expr.kind {
        ExprKind::Yield(_) | ExprKind::YieldFrom(_) => true,
        ExprKind::Lambda { .. } | ExprKind::TypeParamFn { .. } => false,
        ExprKind::GeneratorExp { generators, .. }
        | ExprKind::ListComp { generators, .. }
        | ExprKind::SetComp { generators, .. }
        | ExprKind::DictComp { generators, .. } => generators
            .first()
            .is_some_and(|g| expr_yields_in_scope(&g.iter)),
        ExprKind::Await(v) => expr_yields_in_scope(v),
        ExprKind::JoinedStr(parts) | ExprKind::TemplateStr(parts) => {
            parts.iter().any(expr_yields_in_scope)
        }
        ExprKind::FormattedValue {
            value, format_spec, ..
        }
        | ExprKind::Interpolation {
            value, format_spec, ..
        } => {
            expr_yields_in_scope(value) || format_spec.as_deref().is_some_and(expr_yields_in_scope)
        }
        ExprKind::BinOp { left, right, .. } => {
            expr_yields_in_scope(left) || expr_yields_in_scope(right)
        }
        ExprKind::BoolOp { values, .. } => values.iter().any(expr_yields_in_scope),
        ExprKind::UnaryOp { operand, .. } => expr_yields_in_scope(operand),
        ExprKind::Compare {
            left, comparators, ..
        } => expr_yields_in_scope(left) || comparators.iter().any(expr_yields_in_scope),
        ExprKind::IfExp { test, body, orelse } => {
            expr_yields_in_scope(test) || expr_yields_in_scope(body) || expr_yields_in_scope(orelse)
        }
        ExprKind::NamedExpr { target, value } => {
            expr_yields_in_scope(target) || expr_yields_in_scope(value)
        }
        ExprKind::Call {
            func,
            args,
            keywords,
        } => {
            expr_yields_in_scope(func)
                || args.iter().any(expr_yields_in_scope)
                || keywords.iter().any(|k| expr_yields_in_scope(&k.value))
        }
        ExprKind::Attribute { value, .. } => expr_yields_in_scope(value),
        ExprKind::Subscript { value, slice } => {
            expr_yields_in_scope(value) || expr_yields_in_scope(slice)
        }
        ExprKind::Slice { lower, upper, step } => {
            lower.as_deref().is_some_and(expr_yields_in_scope)
                || upper.as_deref().is_some_and(expr_yields_in_scope)
                || step.as_deref().is_some_and(expr_yields_in_scope)
        }
        ExprKind::Tuple(items) | ExprKind::List(items) | ExprKind::Set(items) => {
            items.iter().any(expr_yields_in_scope)
        }
        ExprKind::Dict { keys, values } => {
            keys.iter()
                .any(|k| k.as_ref().is_some_and(expr_yields_in_scope))
                || values.iter().any(expr_yields_in_scope)
        }
        ExprKind::Starred(inner) => expr_yields_in_scope(inner),
        ExprKind::Constant(_) | ExprKind::Name(_) => false,
    }
}

fn expr_contains_await(expr: &Expr) -> bool {
    match &expr.kind {
        ExprKind::Await(_) => true,
        ExprKind::Yield(v) => v.as_deref().is_some_and(expr_contains_await),
        ExprKind::YieldFrom(v) => expr_contains_await(v),
        ExprKind::Lambda { .. } | ExprKind::TypeParamFn { .. } => false,
        ExprKind::GeneratorExp { .. }
        | ExprKind::ListComp { .. }
        | ExprKind::SetComp { .. }
        | ExprKind::DictComp { .. } => false,
        ExprKind::JoinedStr(parts) | ExprKind::TemplateStr(parts) => {
            parts.iter().any(expr_contains_await)
        }
        ExprKind::FormattedValue {
            value, format_spec, ..
        }
        | ExprKind::Interpolation {
            value, format_spec, ..
        } => expr_contains_await(value) || format_spec.as_deref().is_some_and(expr_contains_await),
        ExprKind::BinOp { left, right, .. } => {
            expr_contains_await(left) || expr_contains_await(right)
        }
        ExprKind::BoolOp { values, .. } => values.iter().any(expr_contains_await),
        ExprKind::UnaryOp { operand, .. } => expr_contains_await(operand),
        ExprKind::Compare {
            left, comparators, ..
        } => expr_contains_await(left) || comparators.iter().any(expr_contains_await),
        ExprKind::IfExp { test, body, orelse } => {
            expr_contains_await(test) || expr_contains_await(body) || expr_contains_await(orelse)
        }
        ExprKind::NamedExpr { target, value } => {
            expr_contains_await(target) || expr_contains_await(value)
        }
        ExprKind::Call {
            func,
            args,
            keywords,
        } => {
            expr_contains_await(func)
                || args.iter().any(expr_contains_await)
                || keywords.iter().any(|k| expr_contains_await(&k.value))
        }
        ExprKind::Attribute { value, .. } => expr_contains_await(value),
        ExprKind::Subscript { value, slice } => {
            expr_contains_await(value) || expr_contains_await(slice)
        }
        ExprKind::Slice { lower, upper, step } => {
            lower.as_deref().is_some_and(expr_contains_await)
                || upper.as_deref().is_some_and(expr_contains_await)
                || step.as_deref().is_some_and(expr_contains_await)
        }
        ExprKind::Tuple(items) | ExprKind::List(items) | ExprKind::Set(items) => {
            items.iter().any(expr_contains_await)
        }
        ExprKind::Dict { keys, values } => {
            keys.iter()
                .any(|k| k.as_ref().is_some_and(expr_contains_await))
                || values.iter().any(expr_contains_await)
        }
        ExprKind::Starred(inner) => expr_contains_await(inner),
        ExprKind::Constant(_) | ExprKind::Name(_) => false,
    }
}

/// Does evaluating `expr` produce (and inline-await) the result of a
/// nested *async* list/set/dict comprehension? This drives PEP 530's
/// implicit async propagation: a comprehension whose element contains
/// an async comprehension becomes async itself. We recurse through
/// ordinary sub-expressions but stop at scope boundaries (`lambda`),
/// and we do **not** treat a nested async *generator expression* as
/// propagating — `(x async for x in a)` evaluates to an async-generator
/// object that is not awaited in place.
fn comp_clause_is_async(generators: &[Comprehension], elt: &Expr, value: Option<&Expr>) -> bool {
    generators.iter().any(|g| g.is_async)
        || expr_contains_await(elt)
        || value.map(expr_contains_await).unwrap_or(false)
        || generators
            .iter()
            .any(|g| expr_contains_await(&g.iter) || g.ifs.iter().any(expr_contains_await))
        || expr_contains_async_comp(elt)
        || value.map(expr_contains_async_comp).unwrap_or(false)
        || generators
            .iter()
            .any(|g| g.ifs.iter().any(expr_contains_async_comp))
}

fn expr_contains_async_comp(expr: &Expr) -> bool {
    match &expr.kind {
        ExprKind::ListComp { elt, generators } | ExprKind::SetComp { elt, generators } => {
            comp_clause_is_async(generators, elt, None)
        }
        ExprKind::DictComp {
            key,
            value,
            generators,
        } => comp_clause_is_async(generators, key, Some(value)),
        // An async genexpr is an async-generator object, not an
        // inline-awaited value, so it does not propagate.
        ExprKind::GeneratorExp { .. } => false,
        // Scope boundary: an async comprehension inside a lambda body
        // belongs to that lambda, not the enclosing comprehension.
        ExprKind::Lambda { .. } | ExprKind::TypeParamFn { .. } => false,
        ExprKind::Await(_) => false,
        ExprKind::Yield(v) => v.as_deref().is_some_and(expr_contains_async_comp),
        ExprKind::YieldFrom(v) => expr_contains_async_comp(v),
        ExprKind::JoinedStr(parts) | ExprKind::TemplateStr(parts) => {
            parts.iter().any(expr_contains_async_comp)
        }
        ExprKind::FormattedValue {
            value, format_spec, ..
        }
        | ExprKind::Interpolation {
            value, format_spec, ..
        } => {
            expr_contains_async_comp(value)
                || format_spec.as_deref().is_some_and(expr_contains_async_comp)
        }
        ExprKind::BinOp { left, right, .. } => {
            expr_contains_async_comp(left) || expr_contains_async_comp(right)
        }
        ExprKind::BoolOp { values, .. } => values.iter().any(expr_contains_async_comp),
        ExprKind::UnaryOp { operand, .. } => expr_contains_async_comp(operand),
        ExprKind::Compare {
            left, comparators, ..
        } => expr_contains_async_comp(left) || comparators.iter().any(expr_contains_async_comp),
        ExprKind::IfExp { test, body, orelse } => {
            expr_contains_async_comp(test)
                || expr_contains_async_comp(body)
                || expr_contains_async_comp(orelse)
        }
        ExprKind::NamedExpr { target, value } => {
            expr_contains_async_comp(target) || expr_contains_async_comp(value)
        }
        ExprKind::Call {
            func,
            args,
            keywords,
        } => {
            expr_contains_async_comp(func)
                || args.iter().any(expr_contains_async_comp)
                || keywords.iter().any(|k| expr_contains_async_comp(&k.value))
        }
        ExprKind::Attribute { value, .. } => expr_contains_async_comp(value),
        ExprKind::Subscript { value, slice } => {
            expr_contains_async_comp(value) || expr_contains_async_comp(slice)
        }
        ExprKind::Slice { lower, upper, step } => {
            lower.as_deref().is_some_and(expr_contains_async_comp)
                || upper.as_deref().is_some_and(expr_contains_async_comp)
                || step.as_deref().is_some_and(expr_contains_async_comp)
        }
        ExprKind::Tuple(items) | ExprKind::List(items) | ExprKind::Set(items) => {
            items.iter().any(expr_contains_async_comp)
        }
        ExprKind::Dict { keys, values } => {
            keys.iter()
                .any(|k| k.as_ref().is_some_and(expr_contains_async_comp))
                || values.iter().any(expr_contains_async_comp)
        }
        ExprKind::Starred(inner) => expr_contains_async_comp(inner),
        ExprKind::Constant(_) | ExprKind::Name(_) => false,
    }
}

/// Enclosing-scope facts the nested-scope free-name scan needs to
/// decide which comprehensions take the PEP 709 inlined lowering, so
/// the pre-emission cell analysis and `comp_inline_eligible` agree.
struct FreeScan {
    /// False in PEP 695 annotation scopes (`ste_can_see_class_scope`),
    /// whose comprehensions keep the nested-function lowering.
    inline_comps: bool,
    /// Whether an async comprehension may inline here (the scope is a
    /// coroutine or async generator).
    async_ok: bool,
    /// The enclosing scope is a class body: an inlined comprehension's
    /// own reads still resolve like a nested scope's (skipping the
    /// class namespace), so they count as needed-from-outer too.
    class_body: bool,
    /// Set while scanning the statements *directly* in a class body:
    /// the names that body binds or declares `global`. An annotation
    /// scope that can see the class namespace (a generic statement's
    /// hidden scope and thunks, a `def`'s `__annotate__`) resolves its
    /// own reads of those names through the class dict and then
    /// globals (symtable.c `analyze_name`'s `class_entry` shortcut),
    /// so they are not needed from an enclosing function. Scopes
    /// nested inside those (a lambda in a bound) don't see the class
    /// namespace, and their reads still are.
    class_binds: Option<HashSet<String>>,
}

impl FreeScan {
    /// The scan for statements *inside* a nested function-like scope
    /// (its body no longer sits directly in the class body).
    fn nested(&self) -> FreeScan {
        FreeScan {
            inline_comps: self.inline_comps,
            async_ok: self.async_ok,
            class_body: self.class_body,
            class_binds: None,
        }
    }
}

/// Reads made by a class-visible annotation scope evaluating `expr`
/// (a type-parameter bound or default, a `type` alias value, a generic
/// class's base or keyword, a parameter annotation), as seen from the
/// enclosing scope: everything the expression reads, minus the
/// scope's own reads of names the visible class body binds
/// ([`FreeScan::class_binds`]); reads from scopes nested inside it
/// always count.
fn collect_class_visible_reads(
    expr: &Expr,
    outer_bindings: &IndexMap<String, Binding>,
    out: &mut HashSet<String>,
    scan: &FreeScan,
) {
    let mut all = HashSet::new();
    collect_reads_expr(expr, &mut all);
    if let Some(binds) = &scan.class_binds {
        all.retain(|n| !binds.contains(n));
        // Comprehensions don't inline in a class-visible scope
        // (`ste_can_see_class_scope`), so every comprehension here is
        // a real nested scope.
        let nested_scan = FreeScan {
            inline_comps: false,
            async_ok: false,
            class_body: false,
            class_binds: None,
        };
        collect_inner_free_expr(expr, outer_bindings, &mut all, &nested_scan);
    }
    out.extend(all);
}

/// CPython `symtable.c`: every list/set/dict comprehension inlines
/// unless the enclosing scope can see a class namespace (the caller
/// handles that and generator expressions). A `yield` inside the
/// comprehension is a SyntaxError the nested-function path reports; an
/// async comprehension outside an async scope keeps that path too.
fn comp_inlines(
    elt: &Expr,
    value: Option<&Expr>,
    generators: &[Comprehension],
    scan: &FreeScan,
) -> bool {
    if !scan.inline_comps {
        return false;
    }
    if !scan.async_ok && comp_clause_is_async(generators, elt, value) {
        return false;
    }
    !comp_scope_contains_yield(elt, value, generators)
}

/// Nested-scope free names of an inlined comprehension, as seen from
/// the enclosing scope: whatever the real scopes inside it (lambdas,
/// generator expressions, nested defs) need, minus the comprehension's
/// own iteration variables (they resolve to the comprehension, which
/// makes them *its* cells — see `comp_symbols`). The outermost
/// iterable runs in the enclosing scope, so its nested scopes report
/// unfiltered.
fn collect_inlined_comp_inner_free(
    elt: &Expr,
    value: Option<&Expr>,
    generators: &[Comprehension],
    outer_bindings: &IndexMap<String, Binding>,
    out: &mut HashSet<String>,
    scan: &FreeScan,
) {
    if let Some(first) = generators.first() {
        collect_inner_free_expr(&first.iter, outer_bindings, out, scan);
    }
    let mut inner = HashSet::new();
    collect_comp_scope_inner_free(elt, value, generators, outer_bindings, &mut inner, scan);
    if scan.class_body {
        // The class namespace is invisible from inside the
        // comprehension: its direct reads reach an enclosing
        // function's cell (or globals), never a class attribute.
        for (gi, g) in generators.iter().enumerate() {
            if gi > 0 {
                collect_reads_expr(&g.iter, &mut inner);
            }
            collect_reads_assign_target(&g.target, &mut inner);
            for cond in &g.ifs {
                collect_reads_expr(cond, &mut inner);
            }
        }
        if let Some(v) = value {
            collect_reads_expr(v, &mut inner);
        }
        collect_reads_expr(elt, &mut inner);
    }
    let mut bound = HashSet::new();
    for g in generators {
        collect_target_names(&g.target, &mut bound);
    }
    for b in &bound {
        inner.remove(b);
    }
    out.extend(inner);
}

/// Nested-scope free names of the parts of a comprehension that run
/// *inside* its scope (everything but the outermost iterable), before
/// the comprehension's own bindings are subtracted.
fn collect_comp_scope_inner_free(
    elt: &Expr,
    value: Option<&Expr>,
    generators: &[Comprehension],
    outer_bindings: &IndexMap<String, Binding>,
    out: &mut HashSet<String>,
    scan: &FreeScan,
) {
    for (gi, g) in generators.iter().enumerate() {
        if gi > 0 {
            collect_inner_free_expr(&g.iter, outer_bindings, out, scan);
        }
        collect_inner_free_expr(&g.target, outer_bindings, out, scan);
        for cond in &g.ifs {
            collect_inner_free_expr(cond, outer_bindings, out, scan);
        }
    }
    if let Some(v) = value {
        collect_inner_free_expr(v, outer_bindings, out, scan);
    }
    collect_inner_free_expr(elt, outer_bindings, out, scan);
}

fn collect_inner_free_expr(
    expr: &Expr,
    outer_bindings: &IndexMap<String, Binding>,
    out: &mut HashSet<String>,
    scan: &FreeScan,
) {
    match &expr.kind {
        // A thunk is itself an annotation scope that sees whatever
        // class namespace its parent sees: its own reads of names that
        // class binds take the `class_entry` shortcut, while scopes
        // nested inside it (a lambda in a bound) don't.
        ExprKind::TypeParamFn { body, .. } if scan.class_binds.is_some() => {
            collect_class_visible_reads(body, outer_bindings, out, scan);
        }
        ExprKind::Lambda { args, body } | ExprKind::TypeParamFn { args, body } => {
            let mut inner_locals: HashSet<String> = HashSet::new();
            for a in &args.posonlyargs {
                inner_locals.insert(a.name.clone());
            }
            for a in &args.args {
                inner_locals.insert(a.name.clone());
            }
            if let Some(va) = &args.vararg {
                inner_locals.insert(va.name.clone());
            }
            for a in &args.kwonlyargs {
                inner_locals.insert(a.name.clone());
            }
            if let Some(kw) = &args.kwarg {
                inner_locals.insert(kw.name.clone());
            }
            let mut reads = HashSet::new();
            collect_reads_deep(body, &mut reads);
            for r in reads {
                if !inner_locals.contains(&r) {
                    out.insert(r);
                }
            }
        }
        ExprKind::ListComp { elt, generators } | ExprKind::SetComp { elt, generators }
            if comp_inlines(elt, None, generators, scan) =>
        {
            // PEP 709: an inlined comprehension is no scope of its own;
            // its reads are this scope's reads (`collect_reads_expr`
            // already sees through it), and only the nested scopes
            // inside it close over anything.
            collect_inlined_comp_inner_free(elt, None, generators, outer_bindings, out, scan);
        }
        ExprKind::DictComp {
            key,
            value,
            generators,
        } if comp_inlines(key, Some(value), generators, scan) => {
            collect_inlined_comp_inner_free(
                key,
                Some(value),
                generators,
                outer_bindings,
                out,
                scan,
            );
        }
        ExprKind::ListComp { elt, generators }
        | ExprKind::SetComp { elt, generators }
        | ExprKind::GeneratorExp { elt, generators } => {
            let mut inner_locals: HashSet<String> = HashSet::new();
            for g in generators {
                collect_target_names(&g.target, &mut inner_locals);
            }
            let mut reads = HashSet::new();
            collect_reads_deep(elt, &mut reads);
            for (gi, g) in generators.iter().enumerate() {
                // The outermost iterable evaluates in the *enclosing*
                // scope (it's passed in as `.0`), so its direct name
                // reads are ordinary enclosing-scope reads — recording
                // them here would spuriously cell-promote enclosing
                // locals (breaking `sys.getrefcount` parity for e.g.
                // `any(... for b in self._mgr.blocks)` inside pandas'
                // `DataFrame.__setitem__`). A nested scope *inside*
                // that iterable still closes over the enclosing frame,
                // so recurse for those.
                if gi == 0 {
                    collect_inner_free_expr(&g.iter, outer_bindings, out, scan);
                } else {
                    collect_reads_deep(&g.iter, &mut reads);
                }
                for i in &g.ifs {
                    collect_reads_deep(i, &mut reads);
                }
            }
            for r in reads {
                if !inner_locals.contains(&r) {
                    out.insert(r);
                }
            }
        }
        ExprKind::DictComp {
            key,
            value,
            generators,
        } => {
            let mut inner_locals: HashSet<String> = HashSet::new();
            for g in generators {
                collect_target_names(&g.target, &mut inner_locals);
            }
            let mut reads = HashSet::new();
            collect_reads_deep(key, &mut reads);
            collect_reads_deep(value, &mut reads);
            for (gi, g) in generators.iter().enumerate() {
                // See the ListComp arm: the outermost iterable belongs
                // to the enclosing scope.
                if gi == 0 {
                    collect_inner_free_expr(&g.iter, outer_bindings, out, scan);
                } else {
                    collect_reads_deep(&g.iter, &mut reads);
                }
                for i in &g.ifs {
                    collect_reads_deep(i, &mut reads);
                }
            }
            for r in reads {
                if !inner_locals.contains(&r) {
                    out.insert(r);
                }
            }
        }
        ExprKind::Call {
            func,
            args,
            keywords,
        } => {
            collect_inner_free_expr(func, outer_bindings, out, scan);
            for a in args {
                collect_inner_free_expr(a, outer_bindings, out, scan);
            }
            for k in keywords {
                collect_inner_free_expr(&k.value, outer_bindings, out, scan);
            }
        }
        ExprKind::BinOp { left, right, .. } => {
            collect_inner_free_expr(left, outer_bindings, out, scan);
            collect_inner_free_expr(right, outer_bindings, out, scan);
        }
        ExprKind::BoolOp { values, .. } => {
            for v in values {
                collect_inner_free_expr(v, outer_bindings, out, scan);
            }
        }
        ExprKind::UnaryOp { operand, .. } => {
            collect_inner_free_expr(operand, outer_bindings, out, scan)
        }
        ExprKind::Compare {
            left, comparators, ..
        } => {
            collect_inner_free_expr(left, outer_bindings, out, scan);
            for c in comparators {
                collect_inner_free_expr(c, outer_bindings, out, scan);
            }
        }
        ExprKind::IfExp { test, body, orelse } => {
            collect_inner_free_expr(test, outer_bindings, out, scan);
            collect_inner_free_expr(body, outer_bindings, out, scan);
            collect_inner_free_expr(orelse, outer_bindings, out, scan);
        }
        ExprKind::Tuple(items) | ExprKind::List(items) | ExprKind::Set(items) => {
            for x in items {
                collect_inner_free_expr(x, outer_bindings, out, scan);
            }
        }
        ExprKind::Dict { keys, values } => {
            for k in keys.iter().flatten() {
                collect_inner_free_expr(k, outer_bindings, out, scan);
            }
            for v in values {
                collect_inner_free_expr(v, outer_bindings, out, scan);
            }
        }
        ExprKind::Attribute { value, .. } | ExprKind::Starred(value) => {
            collect_inner_free_expr(value, outer_bindings, out, scan)
        }
        ExprKind::Subscript { value, slice } => {
            collect_inner_free_expr(value, outer_bindings, out, scan);
            collect_inner_free_expr(slice, outer_bindings, out, scan);
        }
        ExprKind::FormattedValue {
            value, format_spec, ..
        }
        | ExprKind::Interpolation {
            value, format_spec, ..
        } => {
            collect_inner_free_expr(value, outer_bindings, out, scan);
            if let Some(fs) = format_spec.as_deref() {
                collect_inner_free_expr(fs, outer_bindings, out, scan);
            }
        }
        ExprKind::JoinedStr(parts) | ExprKind::TemplateStr(parts) => {
            for p in parts {
                collect_inner_free_expr(p, outer_bindings, out, scan);
            }
        }
        ExprKind::Slice { lower, upper, step } => {
            for x in [lower.as_deref(), upper.as_deref(), step.as_deref()]
                .into_iter()
                .flatten()
            {
                collect_inner_free_expr(x, outer_bindings, out, scan);
            }
        }
        // `await`, `yield`, and `yield from` are arbitrary
        // expressions that can themselves reference outer-scope
        // locals — recurse so the comprehension / lambda detection
        // upstream sees those reads. NamedExpr (walrus `:=`) carries
        // a value subtree that needs the same treatment.
        ExprKind::Await(v) | ExprKind::YieldFrom(v) => {
            collect_inner_free_expr(v, outer_bindings, out, scan);
        }
        ExprKind::Yield(value) => {
            if let Some(v) = value {
                collect_inner_free_expr(v, outer_bindings, out, scan);
            }
        }
        ExprKind::NamedExpr { value, .. } => {
            collect_inner_free_expr(value, outer_bindings, out, scan);
        }
        ExprKind::Name(_) | ExprKind::Constant(_) => {}
    }
}

/// Collect attribute names assigned through the method's first
/// parameter (`self.x = …`, including tuple unpacking, `for self.x in`,
/// `with … as self.x`, augmented and annotated assignment) — the
/// contents of CPython 3.13's `__static_attributes__` class tuple.
fn collect_self_attr_stores(stmts: &[Stmt], self_name: &str, out: &mut HashSet<String>) {
    fn target(e: &Expr, self_name: &str, out: &mut HashSet<String>) {
        match &e.kind {
            ExprKind::Attribute { value, attr } => {
                if matches!(&value.kind, ExprKind::Name(n) if n == self_name) {
                    out.insert(attr.clone());
                }
            }
            ExprKind::Tuple(elts) | ExprKind::List(elts) => {
                for el in elts {
                    target(el, self_name, out);
                }
            }
            ExprKind::Starred(inner) => target(inner, self_name, out),
            _ => {}
        }
    }
    for stmt in stmts {
        match &stmt.kind {
            StmtKind::Assign { targets, .. } => {
                for t in targets {
                    target(t, self_name, out);
                }
            }
            // `_PyCompile_MaybeAddStaticAttributeToClass` runs from the
            // Store-context `Attribute` visit only. `codegen_augassign`
            // emits its `STORE_ATTR` directly (`self._value += n` is
            // *not* recorded), and an annotation without a value never
            // stores (`self.x: int` isn't either).
            StmtKind::AugAssign { .. } | StmtKind::AnnAssign { value: None, .. } => {}
            StmtKind::AnnAssign {
                target: t,
                value: Some(_),
                ..
            } => {
                target(t, self_name, out);
            }
            StmtKind::For {
                target: t,
                body,
                orelse,
                ..
            }
            | StmtKind::AsyncFor {
                target: t,
                body,
                orelse,
                ..
            } => {
                target(t, self_name, out);
                collect_self_attr_stores(body, self_name, out);
                collect_self_attr_stores(orelse, self_name, out);
            }
            StmtKind::While { body, orelse, .. } | StmtKind::If { body, orelse, .. } => {
                collect_self_attr_stores(body, self_name, out);
                collect_self_attr_stores(orelse, self_name, out);
            }
            StmtKind::With { items, body } | StmtKind::AsyncWith { items, body } => {
                for it in items {
                    if let Some(v) = &it.optional_vars {
                        target(v, self_name, out);
                    }
                }
                collect_self_attr_stores(body, self_name, out);
            }
            StmtKind::Try {
                body,
                handlers,
                orelse,
                finalbody,
            } => {
                collect_self_attr_stores(body, self_name, out);
                for h in handlers {
                    collect_self_attr_stores(&h.body, self_name, out);
                }
                collect_self_attr_stores(orelse, self_name, out);
                collect_self_attr_stores(finalbody, self_name, out);
            }
            StmtKind::Match { cases, .. } => {
                for c in cases {
                    collect_self_attr_stores(&c.body, self_name, out);
                }
            }
            // Nested functions at any depth contribute to the nearest
            // enclosing class; nested classes collect their own
            // (CPython walks the compiler stack to the first
            // COMPILER_SCOPE_CLASS — test_compile's
            // test_nested_function / test_nested_class).
            StmtKind::FunctionDef { body, .. } | StmtKind::AsyncFunctionDef { body, .. } => {
                collect_self_attr_stores(body, self_name, out);
            }
            StmtKind::ClassDef { .. } => {}
            _ => {}
        }
    }
}

/// Does this block contain an annotated statement *at its own scope level*?
/// Mirrors CPython's symtable `ste_annotations_used`: `AnnAssign` anywhere
/// in the block — including inside `if`/`for`/`while`/`with`/`try`/`match`
/// bodies — counts, but nested function/class scopes do not (they set up
/// their own annotations).
fn block_has_annotations(body: &[Stmt]) -> bool {
    fn stmt_has(s: &Stmt) -> bool {
        match &s.kind {
            StmtKind::AnnAssign { .. } => true,
            StmtKind::If { body, orelse, .. } | StmtKind::While { body, orelse, .. } => {
                block_has_annotations(body) || block_has_annotations(orelse)
            }
            StmtKind::For { body, orelse, .. } | StmtKind::AsyncFor { body, orelse, .. } => {
                block_has_annotations(body) || block_has_annotations(orelse)
            }
            StmtKind::With { body, .. } | StmtKind::AsyncWith { body, .. } => {
                block_has_annotations(body)
            }
            StmtKind::Try {
                body,
                handlers,
                orelse,
                finalbody,
                ..
            } => {
                block_has_annotations(body)
                    || block_has_annotations(orelse)
                    || block_has_annotations(finalbody)
                    || handlers.iter().any(|h| block_has_annotations(&h.body))
            }
            StmtKind::Match { cases, .. } => cases.iter().any(|c| block_has_annotations(&c.body)),
            _ => false,
        }
    }
    body.iter().any(stmt_has)
}

/// Does this block contain a *simple-name* annotated statement at its
/// own scope level — one `_PyCompile_AddDeferredAnnotation` records, so
/// the block gets an `__annotate__` function under PEP 649?
/// (`(x): int` and `obj.attr: int` annotate nothing.)
fn block_has_deferred_annotations(body: &[Stmt]) -> bool {
    fn stmt_has(s: &Stmt) -> bool {
        match &s.kind {
            StmtKind::AnnAssign { target, simple, .. } => {
                *simple && matches!(target.kind, ExprKind::Name(_))
            }
            StmtKind::If { body, orelse, .. } | StmtKind::While { body, orelse, .. } => {
                block_has_deferred_annotations(body) || block_has_deferred_annotations(orelse)
            }
            StmtKind::For { body, orelse, .. } | StmtKind::AsyncFor { body, orelse, .. } => {
                block_has_deferred_annotations(body) || block_has_deferred_annotations(orelse)
            }
            StmtKind::With { body, .. } | StmtKind::AsyncWith { body, .. } => {
                block_has_deferred_annotations(body)
            }
            StmtKind::Try {
                body,
                handlers,
                orelse,
                finalbody,
                ..
            } => {
                block_has_deferred_annotations(body)
                    || block_has_deferred_annotations(orelse)
                    || block_has_deferred_annotations(finalbody)
                    || handlers
                        .iter()
                        .any(|h| block_has_deferred_annotations(&h.body))
            }
            StmtKind::Match { cases, .. } => cases
                .iter()
                .any(|c| block_has_deferred_annotations(&c.body)),
            _ => false,
        }
    }
    body.iter().any(stmt_has)
}

/// CPython's symtable `ste_has_conditional_annotations` for a *class*
/// block: an annotated statement at the block's own level that sits
/// inside a compound statement (`ENTER_CONDITIONAL_BLOCK` around the
/// bodies of `if`/`while`/`for`/`try`/`with`/`match`). Such a body
/// tracks which annotations actually executed in a
/// `__conditional_annotations__` set. (A module block counts as
/// conditional whenever it has any annotation at all, since the module
/// may be only partially executed.)
fn block_has_conditional_annotations(body: &[Stmt]) -> bool {
    body.iter().any(|s| {
        matches!(
            s.kind,
            StmtKind::If { .. }
                | StmtKind::While { .. }
                | StmtKind::For { .. }
                | StmtKind::AsyncFor { .. }
                | StmtKind::With { .. }
                | StmtKind::AsyncWith { .. }
                | StmtKind::Try { .. }
                | StmtKind::Match { .. }
        ) && block_has_annotations(std::slice::from_ref(s))
    })
}

/// Visit every expression a statement evaluates in its *own* scope,
/// recursing through compound statements. Nested `def`/`class` bodies
/// are their own scopes and are skipped; their decorators, defaults,
/// annotations, bases, and keywords evaluate here and are visited.
fn for_each_scope_expr<'a>(stmt: &'a Stmt, f: &mut dyn FnMut(&'a Expr)) {
    fn body<'a>(stmts: &'a [Stmt], f: &mut dyn FnMut(&'a Expr)) {
        for s in stmts {
            for_each_scope_expr(s, f);
        }
    }
    match &stmt.kind {
        StmtKind::FunctionDef {
            args,
            decorator_list,
            returns,
            ..
        }
        | StmtKind::AsyncFunctionDef {
            args,
            decorator_list,
            returns,
            ..
        } => {
            decorator_list.iter().for_each(&mut *f);
            for d in args
                .defaults
                .iter()
                .chain(args.kw_defaults.iter().flatten())
            {
                f(d);
            }
            if !pep563_active() {
                for a in args
                    .posonlyargs
                    .iter()
                    .chain(&args.args)
                    .chain(&args.kwonlyargs)
                    .chain(&args.vararg)
                    .chain(&args.kwarg)
                {
                    if let Some(ann) = &a.annotation {
                        f(ann);
                    }
                }
                if let Some(r) = returns {
                    f(r);
                }
            }
        }
        StmtKind::ClassDef {
            bases,
            keywords,
            decorator_list,
            ..
        } => {
            decorator_list.iter().for_each(&mut *f);
            bases.iter().for_each(&mut *f);
            for k in keywords {
                f(&k.value);
            }
        }
        StmtKind::TypeAlias { .. } => {}
        StmtKind::Return(v) => {
            if let Some(v) = v {
                f(v);
            }
        }
        StmtKind::Assign { targets, value } => {
            targets.iter().for_each(&mut *f);
            f(value);
        }
        StmtKind::AugAssign { target, value, .. } => {
            f(target);
            f(value);
        }
        StmtKind::AnnAssign {
            target,
            annotation,
            value,
            ..
        } => {
            f(target);
            if !pep563_active() {
                f(annotation);
            }
            if let Some(v) = value {
                f(v);
            }
        }
        StmtKind::If {
            test,
            body: b,
            orelse,
        }
        | StmtKind::While {
            test,
            body: b,
            orelse,
        } => {
            f(test);
            body(b, f);
            body(orelse, f);
        }
        StmtKind::For {
            target,
            iter,
            body: b,
            orelse,
        }
        | StmtKind::AsyncFor {
            target,
            iter,
            body: b,
            orelse,
        } => {
            f(target);
            f(iter);
            body(b, f);
            body(orelse, f);
        }
        StmtKind::Try {
            body: b,
            handlers,
            orelse,
            finalbody,
        } => {
            body(b, f);
            for h in handlers {
                if let Some(t) = &h.type_ {
                    f(t);
                }
                body(&h.body, f);
            }
            body(orelse, f);
            body(finalbody, f);
        }
        StmtKind::Raise { exc, cause } => {
            if let Some(e) = exc {
                f(e);
            }
            if let Some(c) = cause {
                f(c);
            }
        }
        StmtKind::With { items, body: b } | StmtKind::AsyncWith { items, body: b } => {
            for it in items {
                f(&it.context_expr);
                if let Some(v) = &it.optional_vars {
                    f(v);
                }
            }
            body(b, f);
        }
        StmtKind::Match { subject, cases } => {
            f(subject);
            for c in cases {
                if let Some(g) = &c.guard {
                    f(g);
                }
                body(&c.body, f);
            }
        }
        StmtKind::Expr(e) => f(e),
        StmtKind::Delete(targets) => targets.iter().for_each(&mut *f),
        StmtKind::Assert { test, msg } => {
            f(test);
            if let Some(m) = msg {
                f(m);
            }
        }
        StmtKind::Import(_)
        | StmtKind::ImportFrom { .. }
        | StmtKind::Global(_)
        | StmtKind::Nonlocal(_)
        | StmtKind::Pass
        | StmtKind::Break
        | StmtKind::Continue => {}
    }
}

/// Walrus targets bound inside comprehension scopes (list/set/dict
/// comprehensions and generator expressions, nested to any depth)
/// that this statement's scope owns — not those inside a lambda body,
/// which bind in the lambda.
fn collect_comp_walrus_targets_stmt(stmt: &Stmt, out: &mut Vec<String>) {
    fn walk(e: &Expr, out: &mut Vec<String>) {
        match &e.kind {
            ExprKind::ListComp { elt, generators }
            | ExprKind::SetComp { elt, generators }
            | ExprKind::GeneratorExp { elt, generators } => {
                walk(&generators[0].iter, out);
                collect_comp_scope_walruses(elt, None, generators, &mut |n| {
                    if !out.iter().any(|m| m == n) {
                        out.push(n.to_owned());
                    }
                });
            }
            ExprKind::DictComp {
                key,
                value,
                generators,
            } => {
                walk(&generators[0].iter, out);
                collect_comp_scope_walruses(key, Some(value), generators, &mut |n| {
                    if !out.iter().any(|m| m == n) {
                        out.push(n.to_owned());
                    }
                });
            }
            ExprKind::Lambda { args, .. } | ExprKind::TypeParamFn { args, .. } => {
                for d in &args.defaults {
                    walk(d, out);
                }
                for d in args.kw_defaults.iter().flatten() {
                    walk(d, out);
                }
            }
            _ => validate::for_each_child_expr(e, &mut |c| walk(c, out)),
        }
    }
    for_each_scope_expr(stmt, &mut |e| walk(e, out));
}

fn collect_assigned(stmt: &Stmt, out: &mut HashSet<String>) {
    match &stmt.kind {
        StmtKind::Assign { targets, .. } => {
            for t in targets {
                collect_target_names(t, out);
            }
        }
        StmtKind::AugAssign { target, .. } | StmtKind::AnnAssign { target, .. } => {
            collect_target_names(target, out);
        }
        StmtKind::For {
            target,
            body,
            orelse,
            ..
        }
        | StmtKind::AsyncFor {
            target,
            body,
            orelse,
            ..
        } => {
            collect_target_names(target, out);
            for s in body {
                collect_assigned(s, out);
            }
            for s in orelse {
                collect_assigned(s, out);
            }
        }
        StmtKind::While { body, orelse, .. } | StmtKind::If { body, orelse, .. } => {
            for s in body {
                collect_assigned(s, out);
            }
            for s in orelse {
                collect_assigned(s, out);
            }
        }
        StmtKind::FunctionDef { name, .. }
        | StmtKind::AsyncFunctionDef { name, .. }
        | StmtKind::ClassDef { name, .. }
        | StmtKind::TypeAlias { name, .. } => {
            out.insert(name.clone());
        }
        StmtKind::Try {
            body,
            handlers,
            orelse,
            finalbody,
        } => {
            for s in body {
                collect_assigned(s, out);
            }
            for h in handlers {
                if let Some(n) = &h.name {
                    out.insert(n.clone());
                }
                for s in &h.body {
                    collect_assigned(s, out);
                }
            }
            for s in orelse {
                collect_assigned(s, out);
            }
            for s in finalbody {
                collect_assigned(s, out);
            }
        }
        StmtKind::With { items, body } | StmtKind::AsyncWith { items, body } => {
            for it in items {
                if let Some(target) = &it.optional_vars {
                    collect_target_names(target, out);
                }
            }
            for s in body {
                collect_assigned(s, out);
            }
        }
        StmtKind::Import(aliases) => {
            for a in aliases {
                let bind = a
                    .asname
                    .clone()
                    .unwrap_or_else(|| a.name.split('.').next().unwrap_or(&a.name).to_owned());
                out.insert(bind);
            }
        }
        StmtKind::ImportFrom { names, .. } => {
            for a in names {
                let bind = a.asname.clone().unwrap_or_else(|| a.name.clone());
                if bind != "*" {
                    out.insert(bind);
                }
            }
        }
        StmtKind::Match { cases, .. } => {
            for case in cases {
                collect_pattern_names(&case.pattern, out);
                for s in &case.body {
                    collect_assigned(s, out);
                }
            }
        }
        _ => {}
    }
}

/// Collect walrus (`:=`) target names that bind in the *current* scope.
///
/// PEP 572: a named expression binds in the nearest enclosing function or
/// module scope, including walruses written inside an `if`/`while`
/// condition, a `return`/`assert`/expression statement, or a
/// default/decorator/base expression. [`collect_decls`]/[`collect_assigned`]
/// only walk *statements*, so they miss these expression-borne bindings —
/// which is why a comprehension that reads such a name failed to
/// cell-promote it (`UnboundLocalError`). We deliberately treat nested
/// `def`/`lambda`/comprehension scopes as opaque: a walrus there binds to
/// *that* scope (the comprehension-leak case is handled at emission time),
/// so descending would over-collect.
fn collect_walrus_stmt(stmt: &Stmt, out: &mut HashSet<String>) {
    match &stmt.kind {
        StmtKind::Expr(e) | StmtKind::Return(Some(e)) => collect_walrus_expr(e, out),
        StmtKind::Assign { targets, value } => {
            collect_walrus_expr(value, out);
            for t in targets {
                collect_walrus_expr(t, out);
            }
        }
        StmtKind::AugAssign { target, value, .. } => {
            collect_walrus_expr(target, out);
            collect_walrus_expr(value, out);
        }
        StmtKind::AnnAssign {
            target,
            annotation,
            value,
            ..
        } => {
            collect_walrus_expr(target, out);
            collect_walrus_expr(annotation, out);
            if let Some(v) = value {
                collect_walrus_expr(v, out);
            }
        }
        StmtKind::If { test, body, orelse } | StmtKind::While { test, body, orelse } => {
            collect_walrus_expr(test, out);
            for s in body {
                collect_walrus_stmt(s, out);
            }
            for s in orelse {
                collect_walrus_stmt(s, out);
            }
        }
        StmtKind::For {
            target,
            iter,
            body,
            orelse,
        }
        | StmtKind::AsyncFor {
            target,
            iter,
            body,
            orelse,
        } => {
            collect_walrus_expr(target, out);
            collect_walrus_expr(iter, out);
            for s in body {
                collect_walrus_stmt(s, out);
            }
            for s in orelse {
                collect_walrus_stmt(s, out);
            }
        }
        // Nested `def`/`class` scopes: only the parts evaluated in THIS scope
        // (decorators, default args, bases, keywords) can bind a walrus here;
        // the body belongs to the inner scope.
        StmtKind::FunctionDef {
            args,
            decorator_list,
            ..
        }
        | StmtKind::AsyncFunctionDef {
            args,
            decorator_list,
            ..
        } => {
            for d in decorator_list {
                collect_walrus_expr(d, out);
            }
            for d in &args.defaults {
                collect_walrus_expr(d, out);
            }
            for d in args.kw_defaults.iter().flatten() {
                collect_walrus_expr(d, out);
            }
        }
        StmtKind::ClassDef {
            bases,
            keywords,
            decorator_list,
            ..
        } => {
            for d in decorator_list {
                collect_walrus_expr(d, out);
            }
            for b in bases {
                collect_walrus_expr(b, out);
            }
            for k in keywords {
                collect_walrus_expr(&k.value, out);
            }
        }
        StmtKind::Try {
            body,
            handlers,
            orelse,
            finalbody,
        } => {
            for s in body {
                collect_walrus_stmt(s, out);
            }
            for h in handlers {
                if let Some(t) = &h.type_ {
                    collect_walrus_expr(t, out);
                }
                for s in &h.body {
                    collect_walrus_stmt(s, out);
                }
            }
            for s in orelse {
                collect_walrus_stmt(s, out);
            }
            for s in finalbody {
                collect_walrus_stmt(s, out);
            }
        }
        StmtKind::Raise { exc, cause } => {
            if let Some(e) = exc {
                collect_walrus_expr(e, out);
            }
            if let Some(c) = cause {
                collect_walrus_expr(c, out);
            }
        }
        StmtKind::With { items, body } | StmtKind::AsyncWith { items, body } => {
            for it in items {
                collect_walrus_expr(&it.context_expr, out);
                if let Some(t) = &it.optional_vars {
                    collect_walrus_expr(t, out);
                }
            }
            for s in body {
                collect_walrus_stmt(s, out);
            }
        }
        StmtKind::Assert { test, msg } => {
            collect_walrus_expr(test, out);
            if let Some(m) = msg {
                collect_walrus_expr(m, out);
            }
        }
        StmtKind::Delete(targets) => {
            for t in targets {
                collect_walrus_expr(t, out);
            }
        }
        _ => {}
    }
}

/// Collect walrus target names bound by an expression in the current scope
/// (see [`collect_walrus_stmt`]). Mirrors the structure of
/// [`collect_reads_deep`] but records `NAME := …` targets and stops at
/// `lambda`/comprehension boundaries (their walruses are a separate scope's
/// concern).
fn collect_walrus_expr(expr: &Expr, out: &mut HashSet<String>) {
    match &expr.kind {
        ExprKind::NamedExpr { target, value } => {
            collect_target_names(target, out);
            collect_walrus_expr(value, out);
        }
        ExprKind::Name(_) | ExprKind::Constant(_) => {}
        ExprKind::Attribute { value, .. } | ExprKind::Starred(value) => {
            collect_walrus_expr(value, out);
        }
        ExprKind::Subscript { value, slice } => {
            collect_walrus_expr(value, out);
            collect_walrus_expr(slice, out);
        }
        ExprKind::Slice { lower, upper, step } => {
            for x in [lower.as_deref(), upper.as_deref(), step.as_deref()]
                .into_iter()
                .flatten()
            {
                collect_walrus_expr(x, out);
            }
        }
        ExprKind::BinOp { left, right, .. } => {
            collect_walrus_expr(left, out);
            collect_walrus_expr(right, out);
        }
        ExprKind::BoolOp { values, .. } => {
            for v in values {
                collect_walrus_expr(v, out);
            }
        }
        ExprKind::UnaryOp { operand, .. } => collect_walrus_expr(operand, out),
        ExprKind::Compare {
            left, comparators, ..
        } => {
            collect_walrus_expr(left, out);
            for c in comparators {
                collect_walrus_expr(c, out);
            }
        }
        ExprKind::IfExp { test, body, orelse } => {
            collect_walrus_expr(test, out);
            collect_walrus_expr(body, out);
            collect_walrus_expr(orelse, out);
        }
        ExprKind::Call {
            func,
            args,
            keywords,
        } => {
            collect_walrus_expr(func, out);
            for a in args {
                collect_walrus_expr(a, out);
            }
            for k in keywords {
                collect_walrus_expr(&k.value, out);
            }
        }
        // `lambda` defaults evaluate in this scope; its body is a separate
        // scope whose walruses bind there.
        ExprKind::Lambda { args, .. } | ExprKind::TypeParamFn { args, .. } => {
            for d in &args.defaults {
                collect_walrus_expr(d, out);
            }
            for d in args.kw_defaults.iter().flatten() {
                collect_walrus_expr(d, out);
            }
        }
        ExprKind::Tuple(items) | ExprKind::List(items) | ExprKind::Set(items) => {
            for x in items {
                collect_walrus_expr(x, out);
            }
        }
        ExprKind::Dict { keys, values } => {
            for k in keys.iter().flatten() {
                collect_walrus_expr(k, out);
            }
            for v in values {
                collect_walrus_expr(v, out);
            }
        }
        // PEP 572: a named expression inside a comprehension binds in the
        // *nearest enclosing non-comprehension scope* — i.e. right here.
        // Collecting through the comprehension boundary is what makes
        // `res = [(y := f(x)) for x in xs]` create a real local `y` in
        // this scope (the comprehension itself stores through a cell /
        // global; see `compile_comprehension`'s walrus binding pass).
        // Lambda/def bodies inside the comprehension stay opaque.
        ExprKind::ListComp { elt, generators }
        | ExprKind::SetComp { elt, generators }
        | ExprKind::GeneratorExp { elt, generators } => {
            collect_comp_scope_walruses(elt, None, generators, &mut |n| {
                out.insert(n.to_owned());
            });
        }
        ExprKind::DictComp {
            key,
            value,
            generators,
        } => {
            collect_comp_scope_walruses(key, Some(value), generators, &mut |n| {
                out.insert(n.to_owned());
            });
        }
        ExprKind::Yield(value) => {
            if let Some(v) = value {
                collect_walrus_expr(v, out);
            }
        }
        ExprKind::YieldFrom(v) | ExprKind::Await(v) => collect_walrus_expr(v, out),
        ExprKind::JoinedStr(parts) | ExprKind::TemplateStr(parts) => {
            for p in parts {
                collect_walrus_expr(p, out);
            }
        }
        ExprKind::FormattedValue {
            value, format_spec, ..
        }
        | ExprKind::Interpolation {
            value, format_spec, ..
        } => {
            collect_walrus_expr(value, out);
            if let Some(fs) = format_spec.as_deref() {
                collect_walrus_expr(fs, out);
            }
        }
    }
}

/// Every name that appears in a `global` statement anywhere below (and
/// including) `stmt` — descending into nested `def`/`class` bodies.
/// CPython's symtable marks such names GLOBAL_EXPLICIT in the *module*
/// block too, so top-level accesses use the `*_GLOBAL` opcodes.
fn collect_global_decls_deep(stmt: &Stmt, out: &mut HashSet<String>) {
    match &stmt.kind {
        StmtKind::Global(ns) => {
            for n in ns {
                out.insert(n.clone());
            }
        }
        StmtKind::FunctionDef { body, .. }
        | StmtKind::AsyncFunctionDef { body, .. }
        | StmtKind::ClassDef { body, .. }
        | StmtKind::With { body, .. }
        | StmtKind::AsyncWith { body, .. } => {
            for s in body {
                collect_global_decls_deep(s, out);
            }
        }
        StmtKind::For { body, orelse, .. }
        | StmtKind::AsyncFor { body, orelse, .. }
        | StmtKind::While { body, orelse, .. }
        | StmtKind::If { body, orelse, .. } => {
            for s in body.iter().chain(orelse) {
                collect_global_decls_deep(s, out);
            }
        }
        StmtKind::Try {
            body,
            handlers,
            orelse,
            finalbody,
        } => {
            for s in body.iter().chain(orelse).chain(finalbody) {
                collect_global_decls_deep(s, out);
            }
            for h in handlers {
                for s in &h.body {
                    collect_global_decls_deep(s, out);
                }
            }
        }
        StmtKind::Match { cases, .. } => {
            for case in cases {
                for s in &case.body {
                    collect_global_decls_deep(s, out);
                }
            }
        }
        _ => {}
    }
}

/// Does the class *body itself* (not a nested `def`/`class` body) read
/// `__class__`? Decorators, default arguments, annotations, and base
/// lists of nested definitions evaluate at class-body level and count;
/// the nested bodies do not. Lambda bodies are included (they evaluate
/// lazily but CPython resolves their `__class__` through the implicit
/// cell, so the over-approximation only adds an unused freevar).
fn class_body_reads_dunder_class(body: &[Stmt]) -> bool {
    fn stmt_reads(stmt: &Stmt, out: &mut HashSet<String>) {
        match &stmt.kind {
            StmtKind::FunctionDef {
                args,
                decorator_list,
                returns,
                ..
            }
            | StmtKind::AsyncFunctionDef {
                args,
                decorator_list,
                returns,
                ..
            } => {
                for d in decorator_list {
                    collect_reads_expr(d, out);
                }
                for d in args
                    .defaults
                    .iter()
                    .chain(args.kw_defaults.iter().flatten())
                {
                    collect_reads_expr(d, out);
                }
                for a in args
                    .posonlyargs
                    .iter()
                    .chain(&args.args)
                    .chain(&args.kwonlyargs)
                    .chain(&args.vararg)
                    .chain(&args.kwarg)
                {
                    if let Some(ann) = &a.annotation {
                        collect_reads_expr(ann, out);
                    }
                }
                if let Some(r) = returns {
                    collect_reads_expr(r, out);
                }
            }
            StmtKind::ClassDef {
                bases,
                keywords,
                decorator_list,
                ..
            } => {
                for d in decorator_list {
                    collect_reads_expr(d, out);
                }
                for b in bases {
                    collect_reads_expr(b, out);
                }
                for k in keywords {
                    collect_reads_expr(&k.value, out);
                }
            }
            StmtKind::For {
                target: _,
                iter,
                body,
                orelse,
            }
            | StmtKind::AsyncFor {
                target: _,
                iter,
                body,
                orelse,
            } => {
                collect_reads_expr(iter, out);
                for s in body.iter().chain(orelse) {
                    stmt_reads(s, out);
                }
            }
            StmtKind::If { test, body, orelse } | StmtKind::While { test, body, orelse } => {
                collect_reads_expr(test, out);
                for s in body.iter().chain(orelse) {
                    stmt_reads(s, out);
                }
            }
            StmtKind::With { items, body } | StmtKind::AsyncWith { items, body } => {
                for it in items {
                    collect_reads_expr(&it.context_expr, out);
                }
                for s in body {
                    stmt_reads(s, out);
                }
            }
            StmtKind::Try {
                body,
                handlers,
                orelse,
                finalbody,
            } => {
                for s in body.iter().chain(orelse).chain(finalbody) {
                    stmt_reads(s, out);
                }
                for h in handlers {
                    if let Some(t) = &h.type_ {
                        collect_reads_expr(t, out);
                    }
                    for s in &h.body {
                        stmt_reads(s, out);
                    }
                }
            }
            StmtKind::Match { subject, cases } => {
                collect_reads_expr(subject, out);
                for case in cases {
                    for s in &case.body {
                        stmt_reads(s, out);
                    }
                }
            }
            // Everything else has no nested scope to exclude — reuse the
            // full expression-read collector.
            other_stmt => {
                let _ = other_stmt;
                collect_reads_stmt(stmt, out);
            }
        }
    }
    let mut reads = HashSet::new();
    for s in body {
        stmt_reads(s, &mut reads);
    }
    reads.contains("__class__")
}

fn collect_decls(
    stmt: &Stmt,
    globals: &mut HashSet<String>,
    nonlocals: &mut HashSet<String>,
    assigned: &mut HashSet<String>,
) {
    match &stmt.kind {
        StmtKind::Global(ns) => {
            for n in ns {
                globals.insert(n.clone());
            }
        }
        StmtKind::Nonlocal(ns) => {
            for n in ns {
                nonlocals.insert(n.clone());
            }
        }
        StmtKind::Assign { targets, .. } => {
            for t in targets {
                collect_target_names(t, assigned);
            }
        }
        StmtKind::AugAssign { target, .. } => {
            collect_target_names(target, assigned);
        }
        // CPython symtable (AnnAssign): a *simple* annotated name is
        // DEF_LOCAL even without a value (`x: int` → UnboundLocalError
        // on read); a parenthesized one only binds when it has a value
        // (`(x): int` alone leaves `x` resolving globally → NameError).
        StmtKind::AnnAssign {
            target,
            value,
            simple,
            ..
        } => {
            if *simple || value.is_some() {
                collect_target_names(target, assigned);
            }
        }
        // `del NAME` is a binding operation in CPython (`DEF_LOCAL`): the
        // name is local to this scope, and — crucially — a nested scope
        // declaring it `nonlocal` resolves to (and cells) it here. Bare
        // names only; `del obj[i]` / `del obj.attr` bind nothing.
        StmtKind::Delete(targets) => {
            for t in targets {
                collect_target_names(t, assigned);
            }
        }
        StmtKind::For {
            target,
            body,
            orelse,
            ..
        }
        | StmtKind::AsyncFor {
            target,
            body,
            orelse,
            ..
        } => {
            collect_target_names(target, assigned);
            for s in body {
                collect_decls(s, globals, nonlocals, assigned);
            }
            for s in orelse {
                collect_decls(s, globals, nonlocals, assigned);
            }
        }
        StmtKind::While { body, orelse, .. } | StmtKind::If { body, orelse, .. } => {
            for s in body {
                collect_decls(s, globals, nonlocals, assigned);
            }
            for s in orelse {
                collect_decls(s, globals, nonlocals, assigned);
            }
        }
        StmtKind::FunctionDef { name, .. }
        | StmtKind::AsyncFunctionDef { name, .. }
        | StmtKind::ClassDef { name, .. }
        | StmtKind::TypeAlias { name, .. } => {
            assigned.insert(name.clone());
        }
        StmtKind::Try {
            body,
            handlers,
            orelse,
            finalbody,
        } => {
            for s in body {
                collect_decls(s, globals, nonlocals, assigned);
            }
            for h in handlers {
                if let Some(n) = &h.name {
                    assigned.insert(n.clone());
                }
                for s in &h.body {
                    collect_decls(s, globals, nonlocals, assigned);
                }
            }
            for s in orelse {
                collect_decls(s, globals, nonlocals, assigned);
            }
            for s in finalbody {
                collect_decls(s, globals, nonlocals, assigned);
            }
        }
        StmtKind::With { items, body } | StmtKind::AsyncWith { items, body } => {
            for it in items {
                if let Some(target) = &it.optional_vars {
                    collect_target_names(target, assigned);
                }
            }
            for s in body {
                collect_decls(s, globals, nonlocals, assigned);
            }
        }
        // `import a.b.c` binds the top-level package `a` (or the
        // asname); `from m import x as y` binds `y`. These are real
        // local bindings and must be tracked so a name captured by a
        // nested scope is promoted to a cellvar (CPython parity).
        StmtKind::Import(aliases) => {
            for a in aliases {
                let bind = a
                    .asname
                    .clone()
                    .unwrap_or_else(|| a.name.split('.').next().unwrap_or(&a.name).to_owned());
                assigned.insert(bind);
            }
        }
        StmtKind::ImportFrom { names, .. } => {
            for a in names {
                let bind = a.asname.clone().unwrap_or_else(|| a.name.clone());
                if bind != "*" {
                    assigned.insert(bind);
                }
            }
        }
        StmtKind::Match { cases, .. } => {
            for case in cases {
                collect_pattern_names(&case.pattern, assigned);
                for s in &case.body {
                    collect_decls(s, globals, nonlocals, assigned);
                }
            }
        }
        _ => {}
    }
}

/// Locate the `nonlocal NAME` statement declaring `name` within this
/// scope's body (recursing into compound statements but not into nested
/// scopes), for error anchoring.
fn find_nonlocal_decl_span(body: &[Stmt], name: &str) -> Option<weavepy_lexer::Span> {
    for s in body {
        match &s.kind {
            StmtKind::Nonlocal(ns) if ns.iter().any(|n| n == name) => return Some(s.span),
            StmtKind::For { body, orelse, .. }
            | StmtKind::AsyncFor { body, orelse, .. }
            | StmtKind::While { body, orelse, .. }
            | StmtKind::If { body, orelse, .. } => {
                if let Some(sp) = find_nonlocal_decl_span(body, name)
                    .or_else(|| find_nonlocal_decl_span(orelse, name))
                {
                    return Some(sp);
                }
            }
            StmtKind::Try {
                body,
                handlers,
                orelse,
                finalbody,
            } => {
                if let Some(sp) = find_nonlocal_decl_span(body, name)
                    .or_else(|| {
                        handlers
                            .iter()
                            .find_map(|h| find_nonlocal_decl_span(&h.body, name))
                    })
                    .or_else(|| find_nonlocal_decl_span(orelse, name))
                    .or_else(|| find_nonlocal_decl_span(finalbody, name))
                {
                    return Some(sp);
                }
            }
            StmtKind::With { body, .. } | StmtKind::AsyncWith { body, .. } => {
                if let Some(sp) = find_nonlocal_decl_span(body, name) {
                    return Some(sp);
                }
            }
            StmtKind::Match { cases, .. } => {
                if let Some(sp) = cases
                    .iter()
                    .find_map(|c| find_nonlocal_decl_span(&c.body, name))
                {
                    return Some(sp);
                }
            }
            _ => {}
        }
    }
    None
}

/// Like [`collect_target_names`] but preserving first-seen source
/// order (deterministic hidden-local slot assignment for inlined
/// comprehensions — `co_varnames` must not vary between builds).
fn collect_target_names_ordered(expr: &Expr, out: &mut Vec<String>) {
    match &expr.kind {
        ExprKind::Name(n) => {
            if !out.iter().any(|x| x == n) {
                out.push(n.clone());
            }
        }
        ExprKind::Starred(inner) => collect_target_names_ordered(inner, out),
        ExprKind::Tuple(items) | ExprKind::List(items) => {
            for item in items {
                collect_target_names_ordered(item, out);
            }
        }
        // Attribute/subscript targets store into existing objects and
        // bind no comp-local name.
        _ => {}
    }
}

fn collect_target_names(expr: &Expr, out: &mut HashSet<String>) {
    match &expr.kind {
        ExprKind::Name(n) => {
            out.insert(n.clone());
        }
        ExprKind::Tuple(items) | ExprKind::List(items) => {
            for x in items {
                collect_target_names(x, out);
            }
        }
        ExprKind::Starred(inner) => collect_target_names(inner, out),
        _ => {}
    }
}

/// Collect the names a `match` capture pattern binds (`case [x, *rest]:`,
/// `case {…, **rest}:`, `case Cls(a, key=b):`, `pattern as name`). Like
/// `collect_target_names` for `match` — without it, a name bound in a case
/// body/pattern and read by a nested scope is never promoted to a cell, so
/// the binding `STORE_FAST`s while the closure `LOAD_DEREF`s an empty cell
/// (`test_statistics` `kde` kernels).
fn collect_pattern_names(pat: &Pattern, out: &mut HashSet<String>) {
    use weavepy_parser::ast::PatternKind;
    match &pat.kind {
        PatternKind::Value(_)
        | PatternKind::Singleton(_)
        | PatternKind::Capture(None)
        | PatternKind::Star(None) => {}
        PatternKind::Capture(Some(n)) | PatternKind::Star(Some(n)) => {
            out.insert(n.clone());
        }
        PatternKind::Sequence(items) => {
            for p in items {
                collect_pattern_names(p, out);
            }
        }
        PatternKind::Mapping { patterns, rest, .. } => {
            for p in patterns {
                collect_pattern_names(p, out);
            }
            if let Some(Some(n)) = rest {
                out.insert(n.clone());
            }
        }
        PatternKind::Class {
            positionals,
            keywords,
            ..
        } => {
            for p in positionals {
                collect_pattern_names(p, out);
            }
            for (_, p) in keywords {
                collect_pattern_names(p, out);
            }
        }
        PatternKind::Or(alts) => {
            for p in alts {
                collect_pattern_names(p, out);
            }
        }
        PatternKind::As { pattern, name } => {
            out.insert(name.clone());
            collect_pattern_names(pattern, out);
        }
    }
}

/// Record a name bound by a pattern, rejecting rebinds within the same
/// case (CPython compile.c `pattern_helper_store_name`).
fn bind_pattern_name(
    name: &str,
    stores: &mut Vec<String>,
    span: weavepy_lexer::Span,
) -> Result<(), CompileError> {
    if stores.iter().any(|s| s == name) {
        return Err(CompileError::spanned(
            format!("multiple assignments to name '{name}' in pattern"),
            span,
        ));
    }
    stores.push(name.to_owned());
    Ok(())
}

/// CPython 3.14 `is_constant_slice`: every present bound of a slice
/// expression is a (post-AST-optimizer) constant, so the slice object
/// itself can live in `co_consts`.
fn constant_slice(
    lower: &Option<Box<Expr>>,
    upper: &Option<Box<Expr>>,
    step: &Option<Box<Expr>>,
) -> Option<Constant> {
    let part = |x: &Option<Box<Expr>>| -> Option<Constant> {
        match x {
            None => Some(Constant::None),
            Some(e) => match &e.kind {
                ExprKind::Constant(c) => Some(c.clone().into()),
                _ => None,
            },
        }
    };
    Some(Constant::Slice(Box::new((
        part(lower)?,
        part(upper)?,
        part(step)?,
    ))))
}

/// CPython 3.14 `should_apply_two_element_slice_optimization`: a
/// non-constant slice without a step compiles to `BINARY_SLICE` /
/// `STORE_SLICE` instead of materializing a slice object.
fn should_apply_two_element_slice_optimization(slice: &Expr) -> bool {
    match &slice.kind {
        ExprKind::Slice { lower, upper, step } => {
            step.is_none() && constant_slice(lower, upper, step).is_none()
        }
        _ => false,
    }
}

/// Fold a literal-pattern expression (mapping key) to its constant value:
/// plain literals, `-literal`, and the `real ± imaginary` complex form.
/// Attribute lookups (value-pattern keys) fold to `None` — their duplicate
/// check happens at runtime in `MATCH_KEYS`.
fn fold_pattern_literal(expr: &Expr) -> Option<AstConstant> {
    match &expr.kind {
        ExprKind::Constant(c) => Some(c.clone()),
        ExprKind::UnaryOp {
            op: UnaryOp::USub,
            operand,
        } => match &operand.kind {
            ExprKind::Constant(AstConstant::Int(i)) => i.checked_neg().map(AstConstant::Int),
            ExprKind::Constant(AstConstant::Float(f)) => Some(AstConstant::Float(-f)),
            ExprKind::Constant(AstConstant::Complex(r, i)) => Some(AstConstant::Complex(-r, -i)),
            ExprKind::Constant(AstConstant::BigInt(s)) => Some(AstConstant::BigInt(
                if let Some(stripped) = s.strip_prefix('-') {
                    stripped.to_owned()
                } else {
                    format!("-{s}")
                },
            )),
            _ => None,
        },
        ExprKind::BinOp { left, op, right } if matches!(op, BinOp::Add | BinOp::Sub) => {
            let (lr, li) = pattern_const_as_complex(&fold_pattern_literal(left)?)?;
            let (rr, ri) = pattern_const_as_complex(&fold_pattern_literal(right)?)?;
            Some(match op {
                BinOp::Add => AstConstant::Complex(lr + rr, li + ri),
                _ => AstConstant::Complex(lr - rr, li - ri),
            })
        }
        _ => None,
    }
}

/// Numeric constant as `(real, imag)`; `None` for non-numbers.
fn pattern_const_as_complex(c: &AstConstant) -> Option<(f64, f64)> {
    match c {
        AstConstant::Bool(b) => Some((f64::from(u8::from(*b)), 0.0)),
        AstConstant::Int(i) => Some((*i as f64, 0.0)),
        AstConstant::BigInt(s) => s.parse::<f64>().ok().map(|f| (f, 0.0)),
        AstConstant::Float(f) => Some((*f, 0.0)),
        AstConstant::Complex(r, i) => Some((*r, *i)),
        _ => None,
    }
}

/// Python `==` between two literal mapping keys: cross-type numeric
/// equality (`0 == False == 0.0 == -0 == 0j`), exact for integers.
fn pattern_keys_equal(a: &AstConstant, b: &AstConstant) -> bool {
    fn exact_int(c: &AstConstant) -> Option<String> {
        match c {
            AstConstant::Bool(b) => Some(i64::from(*b).to_string()),
            AstConstant::Int(i) => Some(i.to_string()),
            AstConstant::BigInt(s) => Some(s.clone()),
            _ => None,
        }
    }
    if let (Some(x), Some(y)) = (exact_int(a), exact_int(b)) {
        return x == y;
    }
    if let (Some(x), Some(y)) = (pattern_const_as_complex(a), pattern_const_as_complex(b)) {
        return x == y;
    }
    match (a, b) {
        (AstConstant::Str(x), AstConstant::Str(y)) => x == y,
        (AstConstant::Bytes(x), AstConstant::Bytes(y)) => x == y,
        (AstConstant::None, AstConstant::None) => true,
        (AstConstant::Ellipsis, AstConstant::Ellipsis) => true,
        _ => false,
    }
}

/// `repr()`-ish rendering of a literal key for the duplicate-key message.
fn pattern_key_repr(c: &AstConstant) -> String {
    match c {
        AstConstant::None => "None".to_owned(),
        AstConstant::Bool(true) => "True".to_owned(),
        AstConstant::Bool(false) => "False".to_owned(),
        AstConstant::Int(i) => i.to_string(),
        AstConstant::BigInt(s) => s.clone(),
        AstConstant::Float(f) => format!("{f:?}"),
        AstConstant::Complex(r, i) if *r == 0.0 => format!("{i:?}j"),
        AstConstant::Complex(r, i) => format!("({r:?}{}{:?}j)", if *i < 0.0 { "" } else { "+" }, i),
        AstConstant::Str(s) => format!("'{}'", s.replace('\\', "\\\\").replace('\'', "\\'")),
        other => format!("{other:?}"),
    }
}

/// CPython's compile-stage PEP 634 pattern validation (`compile.c`
/// `codegen_pattern_*`). `stores` accumulates the names the case binds;
/// `allow_irrefutable` is only true for the last `match` case (or one
/// with a guard) and for the final `|` alternative.
fn validate_case_pattern(
    pat: &Pattern,
    allow_irrefutable: bool,
    stores: &mut Vec<String>,
) -> Result<(), CompileError> {
    use weavepy_parser::ast::PatternKind;
    match &pat.kind {
        PatternKind::Value(_) | PatternKind::Singleton(_) => Ok(()),
        PatternKind::Capture(None) => {
            if !allow_irrefutable {
                return Err(CompileError::spanned(
                    "wildcard makes remaining patterns unreachable",
                    pat.span,
                ));
            }
            Ok(())
        }
        PatternKind::Capture(Some(name)) => {
            if !allow_irrefutable {
                return Err(CompileError::spanned(
                    format!("name capture '{name}' makes remaining patterns unreachable"),
                    pat.span,
                ));
            }
            bind_pattern_name(name, stores, pat.span)
        }
        PatternKind::Star(name) => {
            if let Some(n) = name {
                bind_pattern_name(n, stores, pat.span)?;
            }
            Ok(())
        }
        PatternKind::Sequence(items) => {
            let stars = items
                .iter()
                .filter(|p| matches!(p.kind, PatternKind::Star(_)))
                .count();
            if stars > 1 {
                return Err(CompileError::spanned(
                    "multiple starred names in sequence pattern",
                    pat.span,
                ));
            }
            for item in items {
                // Subpatterns may always be irrefutable (`case [x]:`).
                validate_case_pattern(item, true, stores)?;
            }
            Ok(())
        }
        PatternKind::Mapping {
            keys,
            patterns,
            rest,
        } => {
            let folded: Vec<Option<AstConstant>> = keys.iter().map(fold_pattern_literal).collect();
            for i in 0..keys.len() {
                if let Some(ci) = &folded[i] {
                    for cj in folded[..i].iter().flatten() {
                        if pattern_keys_equal(ci, cj) {
                            return Err(CompileError::spanned(
                                format!(
                                    "mapping pattern checks duplicate key ({})",
                                    pattern_key_repr(ci)
                                ),
                                keys[i].span,
                            ));
                        }
                    }
                }
            }
            for p in patterns {
                validate_case_pattern(p, true, stores)?;
            }
            if let Some(Some(n)) = rest {
                bind_pattern_name(n, stores, pat.span)?;
            }
            Ok(())
        }
        PatternKind::Class {
            positionals,
            keywords,
            ..
        } => {
            for (i, (name, _)) in keywords.iter().enumerate() {
                if keywords[..i].iter().any(|(m, _)| m == name) {
                    return Err(CompileError::spanned(
                        format!("attribute name repeated in class pattern: {name}"),
                        pat.span,
                    ));
                }
            }
            for p in positionals {
                validate_case_pattern(p, true, stores)?;
            }
            for (_, p) in keywords {
                validate_case_pattern(p, true, stores)?;
            }
            Ok(())
        }
        PatternKind::Or(alts) => {
            let base_len = stores.len();
            let last = alts.len() - 1;
            let mut first_added: Option<Vec<String>> = None;
            for (i, alt) in alts.iter().enumerate() {
                let mut local = stores[..base_len].to_vec();
                validate_case_pattern(alt, allow_irrefutable && i == last, &mut local)?;
                let mut added: Vec<String> = local[base_len..].to_vec();
                added.sort();
                match &first_added {
                    None => first_added = Some(added),
                    Some(f) if *f != added => {
                        return Err(CompileError::spanned(
                            "alternative patterns bind different names",
                            pat.span,
                        ));
                    }
                    _ => {}
                }
            }
            if let Some(added) = first_added {
                stores.extend(added);
            }
            Ok(())
        }
        PatternKind::As { pattern, name } => {
            validate_case_pattern(pattern, allow_irrefutable, stores)?;
            bind_pattern_name(name, stores, pat.span)
        }
    }
}

/// CPython's `pattern_context`: per-case (or per-`|`-alternative)
/// bookkeeping for pattern codegen. See the commentary at the
/// "structural pattern matching" section of the `Compiler` impl.
#[derive(Default)]
struct PatmaCtx {
    /// Names of deferred captures, in capture order. `stores[0]`'s
    /// value is the one nearest the top of the stack.
    stores: Vec<String>,
    /// Number of working items currently on top of the stack (they
    /// must be discarded on failure, and captures rotate below them).
    on_top: usize,
    /// `fail_pops[k]` holds the jump sites that need to discard `k`
    /// items; resolved by [`Compiler::patma_emit_fail_pops`].
    fail_pops: Vec<Vec<u32>>,
}

/// Walk a STORE target (`a = …`, `a, b = …`, `a.b = …`, `a[i] = …`)
/// and collect *reads* it implicitly performs. Bare `Name` targets are
/// pure writes and contribute no reads; everything else (attribute,
/// subscript, tuple / list unpacking, starred elements) reads its
/// container.
fn collect_reads_assign_target(expr: &Expr, out: &mut HashSet<String>) {
    match &expr.kind {
        ExprKind::Name(_) => {}
        ExprKind::Attribute { value, .. } => collect_reads_expr(value, out),
        ExprKind::Subscript { value, slice } => {
            collect_reads_expr(value, out);
            collect_reads_expr(slice, out);
        }
        ExprKind::Tuple(items) | ExprKind::List(items) => {
            for it in items {
                collect_reads_assign_target(it, out);
            }
        }
        ExprKind::Starred(inner) => collect_reads_assign_target(inner, out),
        _ => collect_reads_expr(expr, out),
    }
}

fn collect_reads_stmt(stmt: &Stmt, out: &mut HashSet<String>) {
    collect_reads_stmt_in(stmt, out, false);
}

/// [`collect_reads_stmt`] for a statement in a *function* body: PEP 649
/// leaves the annotation of a local variable (`x: T = 1` inside a
/// `def`) unevaluated, so its names are not reads there
/// (`ste_in_unevaluated_annotation`); module and class bodies evaluate
/// theirs (in `__annotate__`).
fn collect_reads_stmt_fn(stmt: &Stmt, out: &mut HashSet<String>) {
    collect_reads_stmt_in(stmt, out, true);
}

fn collect_reads_stmt_in(stmt: &Stmt, out: &mut HashSet<String>, fn_scope: bool) {
    // See `collect_inner_free`: a generic statement's type parameters
    // are its hidden scope's locals, never reads of this scope.
    if let Some(type_params) = generic_type_params(stmt) {
        let mut inner = HashSet::new();
        collect_reads_stmt_in_impl(stmt, &mut inner, fn_scope);
        remove_type_param_names(stmt, type_params, &mut inner);
        out.extend(inner);
        for_each_outside_hidden_scope_expr(stmt, &mut |e| collect_reads_expr(e, out));
        return;
    }
    collect_reads_stmt_in_impl(stmt, out, fn_scope);
}

fn collect_reads_stmt_in_impl(stmt: &Stmt, out: &mut HashSet<String>, fn_scope: bool) {
    match &stmt.kind {
        StmtKind::Expr(e) | StmtKind::Return(Some(e)) => collect_reads_expr(e, out),
        StmtKind::Assign { targets, value } => {
            collect_reads_expr(value, out);
            // Compound assignment targets (`a.b = ...`, `a[i] = ...`,
            // `a, b = ...`) contain READS of the containing object.
            // Without this, nested scopes can't see attributes /
            // subscripts written through an outer variable.
            for t in targets {
                collect_reads_assign_target(t, out);
            }
        }
        StmtKind::AugAssign { target, value, .. } => {
            collect_reads_expr(target, out);
            collect_reads_expr(value, out);
        }
        StmtKind::AnnAssign {
            target,
            annotation,
            value,
            ..
        } => {
            collect_reads_expr(target, out);
            // PEP 563: stringified annotations are never evaluated and
            // must not participate in scope analysis; nor is the
            // annotation of a function-local variable (PEP 649).
            if !pep563_active() && !fn_scope {
                collect_reads_expr(annotation, out);
            }
            if let Some(v) = value {
                collect_reads_expr(v, out);
            }
        }
        StmtKind::If { test, body, orelse } | StmtKind::While { test, body, orelse } => {
            collect_reads_expr(test, out);
            for s in body {
                collect_reads_stmt_in(s, out, fn_scope);
            }
            for s in orelse {
                collect_reads_stmt_in(s, out, fn_scope);
            }
        }
        StmtKind::For {
            target,
            iter,
            body,
            orelse,
        }
        | StmtKind::AsyncFor {
            target,
            iter,
            body,
            orelse,
        } => {
            collect_reads_expr(target, out);
            collect_reads_expr(iter, out);
            for s in body {
                collect_reads_stmt_in(s, out, fn_scope);
            }
            for s in orelse {
                collect_reads_stmt_in(s, out, fn_scope);
            }
        }
        StmtKind::FunctionDef {
            body,
            args,
            decorator_list,
            returns,
            ..
        }
        | StmtKind::AsyncFunctionDef {
            body,
            args,
            decorator_list,
            returns,
            ..
        } => {
            // Defaults / annotations and decorators evaluate in the
            // OUTER scope.
            for d in decorator_list {
                collect_reads_expr(d, out);
            }
            for d in &args.defaults {
                collect_reads_expr(d, out);
            }
            for d in args.kw_defaults.iter().flatten() {
                collect_reads_expr(d, out);
            }
            if !pep563_active() {
                for a in args
                    .posonlyargs
                    .iter()
                    .chain(&args.args)
                    .chain(&args.kwonlyargs)
                    .chain(&args.vararg)
                    .chain(&args.kwarg)
                {
                    if let Some(ann) = &a.annotation {
                        collect_reads_expr(ann, out);
                    }
                }
                if let Some(r) = returns {
                    collect_reads_expr(r, out);
                }
            }
            // Only names *free* in the nested function surface as reads
            // here: its aggregate body reads minus its own params and
            // assigned locals (recursion applies the same subtraction to
            // deeper nestings). Descending raw leaked e.g. an inner
            // parameter named `f` as a read of this scope, spuriously
            // cell-promoting an enclosing `def f` (test_dis's `outer`
            // fodder). `global` names read the module (never a promotion
            // source); `nonlocal` names reach up even when only written.
            let mut nested_reads = HashSet::new();
            for s in body {
                collect_reads_stmt_in(s, &mut nested_reads, true);
            }
            let mut nested_locals: HashSet<String> = HashSet::new();
            for a in args
                .posonlyargs
                .iter()
                .chain(&args.args)
                .chain(&args.kwonlyargs)
                .chain(&args.vararg)
                .chain(&args.kwarg)
            {
                nested_locals.insert(a.name.clone());
            }
            let mut nested_globals = HashSet::new();
            let mut nested_nonlocals = HashSet::new();
            let mut nested_assigned = HashSet::new();
            for s in body {
                collect_decls(
                    s,
                    &mut nested_globals,
                    &mut nested_nonlocals,
                    &mut nested_assigned,
                );
            }
            nested_locals.extend(nested_assigned);
            for n in &nested_nonlocals {
                out.insert(n.clone());
            }
            for r in nested_reads {
                if !nested_locals.contains(&r) && !nested_globals.contains(&r) {
                    out.insert(r);
                }
            }
        }
        // The alias value and its parameters' bounds/defaults are
        // annotation scopes nested here; their free names surface as
        // reads, minus the alias's own type parameters.
        StmtKind::TypeAlias {
            type_params, value, ..
        } => {
            let mut nested = HashSet::new();
            collect_reads_expr(value, &mut nested);
            for tp in type_params {
                if let TypeParamKind::TypeVar { bound: Some(b) } = &tp.kind {
                    collect_reads_expr(b, &mut nested);
                }
                if let Some(d) = &tp.default {
                    collect_reads_expr(d, &mut nested);
                }
            }
            for tp in type_params {
                nested.remove(&tp.name);
            }
            out.extend(nested);
        }
        StmtKind::ClassDef {
            name,
            bases,
            keywords,
            body,
            decorator_list,
            ..
        } => {
            for d in decorator_list {
                collect_reads_expr(d, out);
            }
            for b in bases {
                collect_reads_expr(b, out);
            }
            for k in keywords {
                collect_reads_expr(&k.value, out);
            }
            // See `collect_inner_free`: the body's private names read
            // in their mangled spelling.
            let mangled_body;
            let body: &[Stmt] = if name.trim_start_matches('_').is_empty() {
                body
            } else {
                let mut b = body.clone();
                crate::mangle::mangle_class_body(name, &mut b);
                mangled_body = b;
                &mangled_body
            };
            for s in body {
                collect_reads_stmt_in(s, out, false);
            }
        }
        StmtKind::Try {
            body,
            handlers,
            orelse,
            finalbody,
        } => {
            for s in body {
                collect_reads_stmt_in(s, out, fn_scope);
            }
            for h in handlers {
                if let Some(t) = &h.type_ {
                    collect_reads_expr(t, out);
                }
                for s in &h.body {
                    collect_reads_stmt_in(s, out, fn_scope);
                }
            }
            for s in orelse {
                collect_reads_stmt_in(s, out, fn_scope);
            }
            for s in finalbody {
                collect_reads_stmt_in(s, out, fn_scope);
            }
        }
        StmtKind::Raise { exc, cause } => {
            if let Some(e) = exc {
                collect_reads_expr(e, out);
            }
            if let Some(c) = cause {
                collect_reads_expr(c, out);
            }
        }
        StmtKind::With { items, body } | StmtKind::AsyncWith { items, body } => {
            for it in items {
                collect_reads_expr(&it.context_expr, out);
                // `with cm as obj.attr:` / `as obj[i]:` reads the
                // target's container.
                if let Some(t) = &it.optional_vars {
                    collect_reads_assign_target(t, out);
                }
            }
            for s in body {
                collect_reads_stmt_in(s, out, fn_scope);
            }
        }
        StmtKind::Delete(targets) => {
            // `del x.attr` / `del x[i]` *read* the container `x` (it must be
            // loaded to perform the delete), so the name must surface for
            // free-variable promotion. A bare `del x` is a binding op, not a
            // read — `collect_reads_assign_target` handles that distinction.
            for t in targets {
                collect_reads_assign_target(t, out);
            }
        }
        StmtKind::Assert { test, msg } => {
            collect_reads_expr(test, out);
            if let Some(m) = msg {
                collect_reads_expr(m, out);
            }
        }
        StmtKind::Match { subject, cases } => {
            // Patterns read names too: value patterns (`case Color.RED:`),
            // mapping keys, and class-pattern heads all resolve in the
            // enclosing scope, so they must surface for free-variable
            // promotion (test_patma_198: `Color` closed over from the
            // enclosing test method).
            collect_reads_expr(subject, out);
            for case in cases {
                collect_pattern_reads(&case.pattern, out);
                if let Some(g) = &case.guard {
                    collect_reads_expr(g, out);
                }
                for s in &case.body {
                    collect_reads_stmt_in(s, out, fn_scope);
                }
            }
        }
        _ => {}
    }
}

/// Names *read* by a `match` pattern: value-pattern expressions, mapping
/// keys, and class-pattern heads. Capture/star/rest names are bindings,
/// not reads ([`collect_pattern_names`] tracks those).
fn collect_pattern_reads(pat: &Pattern, out: &mut HashSet<String>) {
    use weavepy_parser::ast::PatternKind;
    match &pat.kind {
        PatternKind::Value(e) => collect_reads_expr(e, out),
        PatternKind::Singleton(_) | PatternKind::Capture(_) | PatternKind::Star(_) => {}
        PatternKind::Sequence(items) | PatternKind::Or(items) => {
            for p in items {
                collect_pattern_reads(p, out);
            }
        }
        PatternKind::Mapping { keys, patterns, .. } => {
            for k in keys {
                collect_reads_expr(k, out);
            }
            for p in patterns {
                collect_pattern_reads(p, out);
            }
        }
        PatternKind::Class {
            cls,
            positionals,
            keywords,
        } => {
            collect_reads_expr(cls, out);
            for p in positionals {
                collect_pattern_reads(p, out);
            }
            for (_, p) in keywords {
                collect_pattern_reads(p, out);
            }
        }
        PatternKind::As { pattern, .. } => collect_pattern_reads(pattern, out),
    }
}

/// Every name `expr` reads that isn't bound by a scope nested inside
/// it: what a nested closure will need from the scope being analyzed.
/// Once `collect_reads_expr` became scope-correct (lambda parameters
/// and comprehension iteration variables are their own scope's) the
/// two walks coincide; the name survives for its call sites' intent.
fn collect_reads_deep(expr: &Expr, out: &mut HashSet<String>) {
    collect_reads_expr(expr, out);
}

/// Names a comprehension makes its enclosing scope read: the
/// outermost iterable (evaluated there) plus everything the
/// comprehension scope reads without binding — its iteration
/// variables are its own (CPython symtable), whether the comprehension
/// ends up inlined or not.
fn collect_comp_reads(
    elt: &Expr,
    value: Option<&Expr>,
    generators: &[Comprehension],
    out: &mut HashSet<String>,
) {
    if let Some(first) = generators.first() {
        collect_reads_expr(&first.iter, out);
    }
    let mut inner = HashSet::new();
    for (gi, g) in generators.iter().enumerate() {
        if gi > 0 {
            collect_reads_expr(&g.iter, &mut inner);
        }
        // A non-name target (`for tgt[0] in …`) reads its container;
        // filters read their condition.
        collect_reads_assign_target(&g.target, &mut inner);
        for i in &g.ifs {
            collect_reads_expr(i, &mut inner);
        }
    }
    if let Some(v) = value {
        collect_reads_expr(v, &mut inner);
    }
    collect_reads_expr(elt, &mut inner);
    let mut bound = HashSet::new();
    for g in generators {
        collect_target_names(&g.target, &mut bound);
    }
    for b in &bound {
        inner.remove(b);
    }
    out.extend(inner);
}

fn collect_reads_expr(expr: &Expr, out: &mut HashSet<String>) {
    match &expr.kind {
        ExprKind::Name(n) => {
            out.insert(n.clone());
        }
        ExprKind::Attribute { value, .. } | ExprKind::Starred(value) => {
            collect_reads_expr(value, out);
        }
        ExprKind::Subscript { value, slice } => {
            collect_reads_expr(value, out);
            collect_reads_expr(slice, out);
        }
        ExprKind::Slice { lower, upper, step } => {
            for x in [lower.as_deref(), upper.as_deref(), step.as_deref()]
                .into_iter()
                .flatten()
            {
                collect_reads_expr(x, out);
            }
        }
        ExprKind::BinOp { left, right, .. } => {
            collect_reads_expr(left, out);
            collect_reads_expr(right, out);
        }
        ExprKind::BoolOp { values, .. } => {
            for v in values {
                collect_reads_expr(v, out);
            }
        }
        ExprKind::UnaryOp { operand, .. } => collect_reads_expr(operand, out),
        ExprKind::Compare {
            left, comparators, ..
        } => {
            collect_reads_expr(left, out);
            for c in comparators {
                collect_reads_expr(c, out);
            }
        }
        ExprKind::IfExp { test, body, orelse } => {
            collect_reads_expr(test, out);
            collect_reads_expr(body, out);
            collect_reads_expr(orelse, out);
        }
        ExprKind::NamedExpr { target, value } => {
            collect_reads_expr(target, out);
            collect_reads_expr(value, out);
        }
        ExprKind::Call {
            func,
            args,
            keywords,
        } => {
            collect_reads_expr(func, out);
            for a in args {
                collect_reads_expr(a, out);
            }
            for k in keywords {
                collect_reads_expr(&k.value, out);
            }
        }
        ExprKind::Lambda { args, body } | ExprKind::TypeParamFn { args, body } => {
            // Defaults evaluate in the outer scope.
            for d in &args.defaults {
                collect_reads_expr(d, out);
            }
            for d in args.kw_defaults.iter().flatten() {
                collect_reads_expr(d, out);
            }
            // The body is its own scope: what it reads reaches us only
            // when the lambda doesn't bind it itself (its parameters).
            let mut inner = HashSet::new();
            collect_reads_expr(body, &mut inner);
            for a in args
                .posonlyargs
                .iter()
                .chain(&args.args)
                .chain(&args.kwonlyargs)
                .chain(&args.vararg)
                .chain(&args.kwarg)
            {
                inner.remove(&a.name);
            }
            out.extend(inner);
        }
        ExprKind::Tuple(items) | ExprKind::List(items) | ExprKind::Set(items) => {
            for x in items {
                collect_reads_expr(x, out);
            }
        }
        ExprKind::Dict { keys, values } => {
            for k in keys.iter().flatten() {
                collect_reads_expr(k, out);
            }
            for v in values {
                collect_reads_expr(v, out);
            }
        }
        ExprKind::ListComp { elt, generators }
        | ExprKind::SetComp { elt, generators }
        | ExprKind::GeneratorExp { elt, generators } => {
            collect_comp_reads(elt, None, generators, out);
        }
        ExprKind::DictComp {
            key,
            value,
            generators,
        } => {
            collect_comp_reads(key, Some(value), generators, out);
        }
        ExprKind::FormattedValue {
            value, format_spec, ..
        }
        | ExprKind::Interpolation {
            value, format_spec, ..
        } => {
            collect_reads_expr(value, out);
            if let Some(fs) = format_spec.as_deref() {
                collect_reads_expr(fs, out);
            }
        }
        ExprKind::JoinedStr(parts) | ExprKind::TemplateStr(parts) => {
            for p in parts {
                collect_reads_expr(p, out);
            }
        }
        ExprKind::Yield(value) => {
            if let Some(v) = value {
                collect_reads_expr(v, out);
            }
        }
        ExprKind::YieldFrom(v) | ExprKind::Await(v) => {
            collect_reads_expr(v, out);
        }
        ExprKind::Constant(_) => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use weavepy_parser::parse_module;

    fn compile(src: &str) -> CodeObject {
        let module = parse_module(src).expect("parse");
        compile_module(&module).expect("compile")
    }

    #[test]
    fn empty_module_compiles_to_return_none() {
        let co = compile("");
        let ops: Vec<_> = co.instructions.iter().map(|i| i.op).collect();
        assert_eq!(
            ops,
            vec![OpCode::Resume, OpCode::LoadConst, OpCode::ReturnValue]
        );
    }

    #[test]
    fn simple_expression_emits_load_and_pop() {
        // Named operands: `1 + 2` is now constant-folded away
        // (ast_opt), leaving no BINARY_OP behind.
        let co = compile("a + b\n");
        let ops: Vec<_> = co.instructions.iter().map(|i| i.op).collect();
        assert!(ops.contains(&OpCode::BinaryOp));
        assert!(ops.contains(&OpCode::PopTop));
    }

    #[test]
    fn function_def_makes_function() {
        let co = compile("def f(x):\n    return x + 1\n");
        let ops: Vec<_> = co.instructions.iter().map(|i| i.op).collect();
        assert!(ops.contains(&OpCode::MakeFunction));
        assert!(ops.contains(&OpCode::StoreName));
    }

    #[test]
    fn for_loop_uses_get_iter_for_iter() {
        let co = compile("for i in range(10):\n    pass\n");
        let ops: Vec<_> = co.instructions.iter().map(|i| i.op).collect();
        assert!(ops.contains(&OpCode::GetIter));
        assert!(ops.contains(&OpCode::ForIter));
    }

    #[test]
    fn dis_listing_includes_opcode_names() {
        let co = compile("x = 1\n");
        let dis = co.format_dis();
        assert!(dis.contains("LOAD_CONST"));
        assert!(dis.contains("STORE_NAME"));
    }
}

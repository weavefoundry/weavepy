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

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use indexmap::IndexMap;
use thiserror::Error;
use weavepy_parser::ast::{
    Arg as AstArg, Arguments as AstArguments, BinOp, BoolOp, CmpOp, Comprehension,
    Constant as AstConstant, ExceptHandler, Expr, ExprKind, Keyword as KwArg, MatchCase, Module,
    Pattern, Stmt, StmtKind, TypeParamKind, UnaryOp, WithItem,
};

mod ast_opt;
pub mod bytecode;
pub mod cpython_code;
mod mangle;
mod validate;

pub use bytecode::{
    BinOpKind, CacheTable, CompareKind, InlineCache, Instruction, OpCode, UnaryKind,
    BINARY_OP_INPLACE_FLAG, COOLDOWN,
};
pub use cpython_code::{CpythonCode, Position};

/// CPython compile.c `STACK_USE_GUIDELINE`: literal displays and call
/// sites with more operands than this compile through accumulator
/// shapes (append/add/update loops) instead of pushing every operand,
/// keeping `co_stacksize` O(1) in the source length.
const STACK_USE_GUIDELINE: usize = 30;

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
            (C::FrozenSet(a), C::FrozenSet(b)) => a == b,
            (C::Code(_), C::Code(_)) => false,
            (C::Ellipsis, C::Ellipsis) => true,
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
            ExprKind::JoinedStr(parts) => parts.iter().any(in_expr),
            ExprKind::FormattedValue {
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

/// PEP 695 `type` statements arrive first-class from the parser (so
/// `ast.parse` and `symtable` observe the real node); rewrite each to
/// its lazy `__weavepy_type_alias__` assignment form *before* any
/// compiler pass (validation, mangling, scope analysis, codegen)
/// runs, so every later pass sees the same shape the parser used to
/// emit directly. Returns the module untouched (borrowed) when no
/// `type` statement exists — the common case — to avoid cloning the
/// AST.
fn lower_type_aliases(module: &Module) -> std::borrow::Cow<'_, Module> {
    fn block_lists(kind: &StmtKind) -> Vec<&[Stmt]> {
        match kind {
            StmtKind::FunctionDef { body, .. }
            | StmtKind::AsyncFunctionDef { body, .. }
            | StmtKind::ClassDef { body, .. }
            | StmtKind::With { body, .. }
            | StmtKind::AsyncWith { body, .. } => vec![body],
            StmtKind::If { body, orelse, .. }
            | StmtKind::While { body, orelse, .. }
            | StmtKind::For { body, orelse, .. }
            | StmtKind::AsyncFor { body, orelse, .. } => vec![body, orelse],
            StmtKind::Try {
                body,
                handlers,
                orelse,
                finalbody,
            } => {
                let mut out: Vec<&[Stmt]> = vec![body, orelse, finalbody];
                out.extend(handlers.iter().map(|h| h.body.as_slice()));
                out
            }
            StmtKind::Match { cases, .. } => cases.iter().map(|c| c.body.as_slice()).collect(),
            _ => Vec::new(),
        }
    }
    fn contains(stmts: &[Stmt]) -> bool {
        stmts.iter().any(|s| {
            matches!(s.kind, StmtKind::TypeAlias { .. })
                || block_lists(&s.kind).into_iter().any(contains)
        })
    }
    fn rewrite(stmts: &mut [Stmt]) {
        for s in stmts {
            if matches!(s.kind, StmtKind::TypeAlias { .. }) {
                *s = weavepy_parser::lower_type_alias_stmt(s);
                continue;
            }
            match &mut s.kind {
                StmtKind::FunctionDef { body, .. }
                | StmtKind::AsyncFunctionDef { body, .. }
                | StmtKind::ClassDef { body, .. }
                | StmtKind::With { body, .. }
                | StmtKind::AsyncWith { body, .. } => rewrite(body),
                StmtKind::If { body, orelse, .. }
                | StmtKind::While { body, orelse, .. }
                | StmtKind::For { body, orelse, .. }
                | StmtKind::AsyncFor { body, orelse, .. } => {
                    rewrite(body);
                    rewrite(orelse);
                }
                StmtKind::Try {
                    body,
                    handlers,
                    orelse,
                    finalbody,
                } => {
                    rewrite(body);
                    rewrite(orelse);
                    rewrite(finalbody);
                    for h in handlers {
                        rewrite(&mut h.body);
                    }
                }
                StmtKind::Match { cases, .. } => {
                    for c in cases {
                        rewrite(&mut c.body);
                    }
                }
                _ => {}
            }
        }
    }
    if !contains(&module.body) {
        return std::borrow::Cow::Borrowed(module);
    }
    let mut lowered = module.clone();
    rewrite(&mut lowered.body);
    std::borrow::Cow::Owned(lowered)
}

/// Run only the parse-adjacent validation pass (the symtable-stage
/// checks CPython performs while *building* the symbol table:
/// `global`/`nonlocal` directive conflicts, `__future__` placement,
/// annotation-scope restrictions, …) without generating code.
/// `_symtable.symtable()` uses this so symtable-build-time
/// `SyntaxError`s surface with CPython's messages and locations.
pub fn validate_module_only(module: &Module, source: &str) -> Result<(), CompileError> {
    let module = lower_type_aliases(module);
    let module = &*module;
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
    let module = lower_type_aliases(module);
    let module = &*module;
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
    let module = lower_type_aliases(module);
    let module = &*module;
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
    let module = lower_type_aliases(module);
    let module = &*module;
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
    /// Monotonic counter for synthetic locals used by chained
    /// comparisons (`.chain0`, `.chain1`, …).
    chain_counter: u32,
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
    /// Offsets of *structural* jumps — instructions CPython emits with
    /// `NO_LOCATION` (loop back edges, `if`/`else` join jumps, `match`
    /// end jumps). CPython's flowgraph optimizer may thread a jump
    /// *through* these (they carry no observable line of their own),
    /// but never through an explicit-statement jump on a different
    /// line (`continue` keeps its own 'line' trace event —
    /// test_break_to_continue1). Our linetable stamps them with the
    /// preceding instruction's line (CPython's `propagate_line_numbers`
    /// result), so threading eligibility needs this side channel.
    synthetic_jumps: HashSet<u32>,
    /// Rejoin jumps synthesized by [`Self::push_cold_blocks_to_end`]
    /// (a moved cold block's fallthrough edge made explicit). CPython
    /// creates these *after* `label_exception_targets`, so they carry
    /// no exception coverage — the PEP 479 epilogue's complement
    /// coverage must skip them.
    cold_rejoins: HashSet<u32>,
    /// Jumps CPython emits as `JUMP_NO_INTERRUPT` (synthetic scope
    /// exits: handler exits, `with`-suppress exits). When such a jump
    /// ends up backward on the wire it encodes as
    /// `JUMP_BACKWARD_NO_INTERRUPT`; forward it's indistinguishable
    /// from `JUMP_FORWARD`. Copied into
    /// [`CodeObject::no_interrupt_jumps`] by [`Self::finish`].
    no_interrupt_jumps: HashSet<u32>,
    /// Nesting depth of PEP 709 *inlined* comprehension emission.
    /// While > 0, `compile_comp_body` skips the `.0`-argument dance at
    /// generator depth 0 (the caller pushed the ready iterator) and
    /// registers its exception handlers with
    /// [`HANDLER_DEPTH_SENTINEL`] depths for `finish` to resolve.
    inline_comp: u32,
    /// Number of *live exception values* sitting on the operand stack at
    /// the current compile point: a `finally` body (or the unmatched
    /// re-raise path of a `try/except`) runs with the propagating
    /// exception on the stack until the trailing `RERAISE` pops it.
    /// Exception-table entries registered for code nested inside such
    /// regions must include these slots in their `depth`, or the
    /// dispatch loop would truncate the live exception away and the
    /// `RERAISE` would underflow.
    exc_on_stack: u32,
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
    /// (`<generic parameters of X>`): `(X's display name, the qualname
    /// X would get in the scope containing the generic statement)`.
    /// [`Self::compute_child_qualname`] returns the stored qualname
    /// for the matching child so the hidden scope never appears in
    /// qualnames (CPython `compiler_set_qualname` skips
    /// `TypeParametersBlock` scopes).
    pep695_qualname: Option<(String, String)>,
    /// Handoff slot: [`Self::compile_generic_def`] stores the pair
    /// here just before building the hidden function, and
    /// [`Self::build_function_object_inner`] moves it into the child
    /// compiler's [`Self::pep695_qualname`].
    pending_pep695_qualname: Option<(String, String)>,
    /// Set while *this* compiler is a PEP 695 annotation scope that
    /// can see a class namespace (CPython `ste_can_see_class_scope`):
    /// a hidden `<generic parameters of X>` scope, a type-param
    /// bound/default thunk, or a `type` alias thunk, textually inside
    /// a class body. Name loads that would be `LoadGlobal`/`LoadDeref`
    /// instead consult the `__classdict__` cell first
    /// (`LOAD_FROM_DICT_OR_{GLOBALS,DEREF}`).
    lazy_class_ctx: Option<Rc<LazyClassCtx>>,
    /// Handoff slot for [`Self::lazy_class_ctx`], mirroring
    /// [`Self::pending_pep695_qualname`]'s pattern.
    pending_lazy_class_ctx: Option<Rc<LazyClassCtx>>,
    /// Class-body compilers only: every name assigned at the body's
    /// own level. Feeds [`LazyClassCtx::assigned`].
    class_assigned: HashSet<String>,
}

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
    /// `handler_depth` when the loop was entered. `break`/`continue`
    /// from inside an `except` handler body must POP_EXCEPT each
    /// handler level they exit (CPython unwinds the exception-handler
    /// blocks; without this the handled exception leaks until frame
    /// exit — test_exceptions.testExceptionCleanupState).
    handler_depth_at_entry: u32,
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
    /// `except:`): a `return` leaving the handler must emit
    /// `POP_EXCEPT` right after inlining it — after the unbind so the
    /// pop's prompt-reap cascade sees the handled exception's true
    /// refcount (CPython frees the exception at handler exit;
    /// `pickle.load`'s `except _Stop: return stopinst.value` relies on
    /// this to release the unpickled graph immediately).
    pop_except_after: bool,
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
            chain_counter: 0,
            next_finally_id: 0,
            finally_holes: Vec::new(),
            line_index,
            current_line: 0,
            line_pinned: None,
            pinned_colspan: ColSpan::default(),
            current_span: (0, 0),
            synthetic_jumps: HashSet::new(),
            cold_rejoins: HashSet::new(),
            no_interrupt_jumps: HashSet::new(),
            inline_comp: 0,
            exc_on_stack: 0,
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
            pep695_qualname: None,
            pending_pep695_qualname: None,
            lazy_class_ctx: None,
            pending_lazy_class_ctx: None,
            class_assigned: HashSet::new(),
        }
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
        // PEP 695 hidden scopes are transparent for qualnames: the
        // generic def/class defined inside `<generic parameters of X>`
        // gets the qualname it would have had in the enclosing scope.
        if let Some((child, qualname)) = &self.pep695_qualname {
            if child == name {
                return qualname.clone();
            }
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

    fn finish(mut self) -> CodeObject {
        // CPython's `optimize_basic_block` folds branches on constant
        // conditions and constant tuple builds before any jump surgery
        // (`if 0:` bodies then die as unreachable code below; function
        // default tuples become a single constant).
        self.fold_const_branches();
        self.fold_const_pops();
        self.fold_const_tuples();
        // Always terminate the code object with an implicit `return None`,
        // matching CPython's "fall off the end of the function" shape.
        //
        // It is *not* enough to check whether the textually-last instruction
        // is a `ReturnValue`: a function whose body ends in an `if/else`
        // where the `else` branch returns leaves a `ReturnValue` last, yet
        // the `if` branch can still *fall through* to the end-of-code offset
        // via a forward jump. If we skip the implicit return in that case the
        // jump lands one past the final instruction and the VM trips a
        // "pc out of bounds" `InternalError`. Emitting an unconditional
        // trailing `return None` keeps the end-of-code offset a valid target;
        // when it is genuinely unreachable it is harmless dead code (two
        // instructions) exactly as in CPython.
        // CPython's flowgraph optimizer threads jump-to-jump chains: a
        // branch whose target is itself an unconditional jump goes
        // straight to the final destination. Beyond saving a hop, this
        // is *observable* under sys.settrace: an `if` body inside a
        // loop must not bounce through the join-point `JUMP_BACKWARD`
        // sitting on the `else` body's line (that would fire a spurious
        // 'line' event there — test_sys_settrace's no_pop_tops /
        // break_to_break family). Threading keeps execution off the
        // intermediate instruction entirely; the retargeted jump keeps
        // its own source location (gh-123048).
        //
        // Eligibility mirrors CPython's `jump_thread`: only hop through
        // a jump that is *synthetic* (CPython would have emitted it
        // with NO_LOCATION) or one sharing the source line of the jump
        // being threaded. An explicit `continue` on its own line stays
        // a distinct hop so its 'line' event still fires.
        let n = self.next_offset();
        for i in 0..n {
            let ins = self.co.instructions[i as usize];
            let (is_cond, target) = match ins.op {
                OpCode::JumpForward => (false, i + 1 + ins.arg),
                OpCode::PopJumpIfFalse
                | OpCode::PopJumpIfTrue
                | OpCode::PopJumpIfNone
                | OpCode::PopJumpIfNotNone => (true, i + 1 + ins.arg),
                _ => continue,
            };
            let site_line = self.co.linetable[i as usize];
            let mut t = target;
            let mut hops = 0u32;
            while t < n && hops <= n {
                if !(self.synthetic_jumps.contains(&t)
                    || self.co.linetable[t as usize] == site_line)
                {
                    break;
                }
                let tin = self.co.instructions[t as usize];
                match tin.op {
                    OpCode::JumpForward => t = t + 1 + tin.arg,
                    // Our conditional jump opcodes only encode forward
                    // displacements, so they stop at a backward hop.
                    OpCode::JumpBackward if !is_cond => t = (t + 1) - tin.arg,
                    _ => break,
                }
                hops += 1;
            }
            if t == target || t >= n || hops > n {
                continue;
            }
            if t > i {
                self.co.instructions[i as usize].arg = t - (i + 1);
            } else if !is_cond {
                self.co.instructions[i as usize].op = OpCode::JumpBackward;
                self.co.instructions[i as usize].arg = (i + 1) - t;
            }
        }
        let none_idx = self.co.intern_constant(Constant::None);
        let epilogue = self.next_offset();
        // NO_LOCATION: the implicit `return None` inherits the
        // preceding instruction's line via the fall-through propagation
        // below (CPython emits it location-free and propagates), not
        // whatever `current_line` was left at — a multi-line unpack
        // ends on its *last target's* line, not the statement head
        // (test_trace_unpack_long_sequence).
        self.emit_no_line(OpCode::LoadConst, none_idx);
        // arg 1: the implicit `return None` is CPython's RETURN_CONST
        // (the wire encoder fuses only codegen-origin constant returns;
        // an optimizer-produced LOAD_CONST + RETURN_VALUE pair stays
        // split, as in 3.13 — test_compile test_consts_in_conditionals).
        self.emit_no_line(OpCode::ReturnValue, 1);
        // A synthetic no-location run directly before the epilogue
        // (the class-body tail: `__static_attributes__` /
        // `__classcell__` stores, emitted with `line_pinned = 0`)
        // belongs to the return sequence: CPython duplicates the whole
        // tail into each predecessor block and propagates locations,
        // so each path's copy carries *that path's* line. Extend the
        // duplication below to cover it. Only straight-line code is
        // eligible — a jump or a handler entry inside the run keeps
        // the shared tail.
        let mut tail_start = epilogue;
        while tail_start > 0
            && self.co.linetable[(tail_start - 1) as usize] == 0
            && matches!(
                self.co.instructions[(tail_start - 1) as usize].op,
                OpCode::LoadConst | OpCode::StoreName | OpCode::LoadClosure
            )
        {
            tail_start -= 1;
        }
        if self
            .co
            .exception_table
            .iter()
            .any(|h| h.handler >= tail_start && h.handler < epilogue)
        {
            tail_start = epilogue;
        }
        if tail_start < epilogue {
            // A jump landing *inside* the run (past its start) would be
            // orphaned by per-path duplication; keep the shared tail.
            for i in 0..tail_start {
                let ins = self.co.instructions[i as usize];
                let target = match ins.op {
                    OpCode::JumpForward
                    | OpCode::PopJumpIfFalse
                    | OpCode::PopJumpIfTrue
                    | OpCode::PopJumpIfNone
                    | OpCode::PopJumpIfNotNone => i + 1 + ins.arg,
                    OpCode::JumpBackward => (i + 1).saturating_sub(ins.arg),
                    _ => continue,
                };
                if target > tail_start && target <= epilogue + 1 {
                    tail_start = epilogue;
                    break;
                }
            }
        }
        // CPython duplicates the implicit `return None` into every
        // predecessor path (its cfg copies RETURN_CONST backward into
        // each basic block that jumps to it), so each return carries
        // *that path's* line — an `except` clause exiting a
        // function-ending `try` reports its `'return'` trace event on
        // the handler body's last line, not on wherever a shared
        // epilogue happens to sit. Mirror that: retarget every forward
        // jump landing exactly on the epilogue to its own `return
        // None` copy stamped with the jump site's location. This also
        // keeps the epilogue from being a multi-line jump target,
        // which would otherwise fire spurious `'line'` trace events
        // (RFC 0051 WS4).
        //
        // Jumps are *threaded* first: nested `if`/`try` join points
        // produce forward-jump chains (`x = 4` jumps to the inner
        // join, which jumps to the outer join, which falls into the
        // epilogue), and CPython's CFG optimizer collapses those
        // before duplicating returns — so the copy must carry the
        // line of the *original* jump site, not an intermediate hop.
        // Forward jumps strictly increase the offset, so the chase
        // terminates.
        //
        // The chase honours the same eligibility rule as the
        // threading pass above: hop only through synthetic jumps or
        // ones sharing the site's line. A nested `break` targeting an
        // outer `break`'s jump on its own line must keep the hop so
        // the outer line's 'line' event still fires
        // (test_break_to_break).
        let synth = &self.synthetic_jumps;
        let resolve = |co: &CodeObject, site_line: u32, mut t: u32| -> u32 {
            while (t as usize) < co.instructions.len() {
                let ins = co.instructions[t as usize];
                if ins.op == OpCode::JumpForward
                    && (synth.contains(&t) || co.linetable[t as usize] == site_line)
                {
                    t = t + 1 + ins.arg;
                } else {
                    break;
                }
            }
            t
        };
        let jump_sites: Vec<u32> = (0..tail_start)
            .filter(|&i| {
                let ins = self.co.instructions[i as usize];
                matches!(
                    ins.op,
                    OpCode::JumpForward
                        | OpCode::PopJumpIfFalse
                        | OpCode::PopJumpIfTrue
                        | OpCode::PopJumpIfNone
                        | OpCode::PopJumpIfNotNone
                ) && resolve(&self.co, self.co.linetable[i as usize], i + 1 + ins.arg) == tail_start
            })
            .collect();
        if std::env::var_os("WP_DBG_FINISH").is_some() {
            eprintln!(
                "[finish] {} epilogue={} sites={:?}",
                self.co.qualname, epilogue, jump_sites
            );
            for (i, ins) in self.co.instructions.iter().enumerate() {
                eprintln!(
                    "  {:>3} {:?} {} line={}",
                    i, ins.op, ins.arg, self.co.linetable[i]
                );
            }
        }
        // Jump targets and handler entries: block heads the location
        // walk-back below must not cross (multi-predecessor joins —
        // CPython's `propagate_line_numbers` only flows a location into
        // a single-predecessor successor, so a NO_LOCATION join like
        // the except* PREP_RERAISE_STAR epilogue stays location-free
        // rather than borrowing whatever path was emitted last).
        let mut block_head = vec![false; self.co.instructions.len() + 1];
        for (i, ins) in self.co.instructions.iter().enumerate() {
            let from = i as u32 + 1;
            match ins.op {
                OpCode::JumpForward
                | OpCode::PopJumpIfFalse
                | OpCode::PopJumpIfTrue
                | OpCode::PopJumpIfNone
                | OpCode::PopJumpIfNotNone
                | OpCode::ForIter
                | OpCode::Send => {
                    if let Some(b) = block_head.get_mut((from + ins.arg) as usize) {
                        *b = true;
                    }
                }
                OpCode::JumpBackward => {
                    if let Some(b) = block_head.get_mut(from.saturating_sub(ins.arg) as usize) {
                        *b = true;
                    }
                }
                _ => {}
            }
        }
        for h in &self.co.exception_table {
            if let Some(b) = block_head.get_mut(h.handler as usize) {
                *b = true;
            }
        }
        for site in jump_sites {
            let mut line = self.co.linetable[site as usize];
            let mut col = self.co.coltable[site as usize];
            // A NO_LOCATION jump site (e.g. the synthetic handler-exit
            // hop after POP_EXCEPT): the copy inherits the predecessor
            // block's last located instruction, as CPython's
            // `propagate_line_numbers` + exit-line guarantee would —
            // the `'return'` event reports the handler body's line,
            // never `None`. The walk stays within the fallthrough
            // chain: it stops at multi-predecessor block heads and at
            // instructions that end the fallthrough (returns, raises,
            // unconditional jumps), where CPython would not propagate.
            if line == 0 {
                for k in (0..site).rev() {
                    if matches!(
                        self.co.instructions[k as usize].op,
                        OpCode::JumpForward
                            | OpCode::JumpBackward
                            | OpCode::ReturnValue
                            | OpCode::RaiseVarargs
                            | OpCode::Reraise
                    ) {
                        break;
                    }
                    if self.co.linetable[k as usize] != 0 {
                        line = self.co.linetable[k as usize];
                        col = self.co.coltable[k as usize];
                        break;
                    }
                    if block_head[k as usize] {
                        break;
                    }
                }
            }
            let copy = self.next_offset();
            for j in tail_start..=(epilogue + 1) {
                self.co.instructions.push(self.co.instructions[j as usize]);
                self.co.linetable.push(line);
                self.co.coltable.push(col);
            }
            self.patch_jump(site, copy);
        }
        // Fall-through copy of the tail: propagate the preceding
        // instruction's location into the no-location run (CPython's
        // `propagate_line_numbers`), so `co_lines()` and the frame's
        // `f_lineno` report the path's line at the implicit return.
        for j in tail_start..=(epilogue + 1) {
            let j = j as usize;
            if self.co.linetable[j] == 0 && j > 0 {
                self.co.linetable[j] = self.co.linetable[j - 1];
                self.co.coltable[j] = self.co.coltable[j - 1];
            }
        }
        // CPython's `basicblock_inline_small_or_no_lineno_blocks`
        // copies a small scope-exiting target block into each
        // unconditional-jump predecessor. The flat-stream equivalent
        // handled here is the single-instruction case: an unconditional
        // jump straight at a RETURN_VALUE becomes that return, keeping
        // the *return's* location (the inlined copy carries its own
        // loc, as CPython's copy does) — test_peepholer's
        // test_elim_jump_to_return.
        {
            let n = self.co.instructions.len() as u32;
            for i in 0..n as usize {
                let ins = self.co.instructions[i];
                let target = match ins.op {
                    OpCode::JumpForward => i as u32 + 1 + ins.arg,
                    OpCode::JumpBackward => (i as u32 + 1).saturating_sub(ins.arg),
                    _ => continue,
                };
                if target < n && self.co.instructions[target as usize].op == OpCode::ReturnValue {
                    self.co.instructions[i] = Instruction {
                        op: OpCode::ReturnValue,
                        // Keep the RETURN_CONST-fusability tag of the
                        // duplicated return (arg 1 = codegen-origin
                        // constant return; see `StmtKind::Return`).
                        arg: self.co.instructions[target as usize].arg,
                    };
                    self.co.linetable[i] = self.co.linetable[target as usize];
                    self.co.coltable[i] = self.co.coltable[target as usize];
                }
            }
        }
        // Literal pack-then-unpack becomes direct stack shuffling.
        self.optimize_pack_unpack();
        // …and remaining SWAPs over plain stores/pops are applied
        // statically by reordering the consumers.
        self.apply_static_swaps();
        // CPython flowgraph `inline_small_or_no_lineno_blocks` (the
        // small-exit-block half): an unconditional jump to a block of
        // ≤ MAX_COPY_SIZE instructions that exits the scope is replaced
        // by a copy of the block — a `with`'s normal-exit and swallow
        // paths each fall into their own copy of the statement tail
        // (test_dis test_disassemble_with).
        self.inline_small_exit_blocks();
        // CPython's flowgraph removes unreachable basic blocks
        // entirely (`remove_unreachable` + `eliminate_empty_basic_blocks`),
        // so dead code after a return/raise or an unconditional jump
        // never survives into co_code (test_peepholer's
        // test_elim_jump_after_return1 asserts the absence of the dead
        // loop back-edge). Compact the flat stream accordingly.
        self.eliminate_unreachable();
        // Conditional jumps must not land on unconditional back edges.
        self.normalize_backward_conditionals();
        // Normalisation retargets the conditional through a fresh
        // trampoline; if that conditional was the *only* way into the
        // original back edge (`if …: break` as the loop body's last
        // statement), the old edge is now dead — CPython re-runs
        // `remove_unreachable` after jump normalisation for the same
        // reason (test_dis's jumpy has no stray JUMP_BACKWARD after
        // the break's JUMP_FORWARD).
        self.eliminate_unreachable();
        // CPython `push_cold_blocks_to_end`: exception-only-reachable
        // blocks (handlers, send-dance CLEANUP_THROW tails) move to
        // the stream tail in original order, with explicit rejoin
        // jumps for severed fallthrough edges.
        self.push_cold_blocks_to_end();
        // Drop NOPs whose line a same-block neighbour covers, then NOP
        // out jumps whose target is the next instruction and repeat —
        // CPython's `remove_redundant_nops_and_jumps` fixpoint
        // (test_compile's test_false_while_loop: `while False: pass`
        // must compile to just RESUME; RETURN_CONST).
        loop {
            self.remove_redundant_nops();
            if !self.remove_jumps_to_next() {
                break;
            }
        }
        // Finally, prune constants nothing loads any more (CPython's
        // `remove_unused_consts` — folded branches and inlined returns
        // leave orphans behind, and test_dis indexes co_consts).
        self.remove_unused_consts();
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
                            | OpCode::LoadClassderef
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
        // CPython flowgraph `duplicate_exits_without_lineno`: a shared
        // location-free exit/eval-break block (e.g. the POP_EXCEPT +
        // JUMP_BACKWARD continue tail two handlers converge on) is
        // duplicated per jump predecessor so each copy can take that
        // path's line in the propagation below.
        self.duplicate_exits_without_lineno();
        // CPython flowgraph `propagate_line_numbers`: fill remaining
        // NO_LOCATION instructions from their basic block's previous
        // located instruction, seeding single-predecessor successors —
        // a synthetic loop-continue jump after POP_EXCEPT reports the
        // handler body's line, never None (test_compile
        // test_line_number_synthetic_jump_multiple_predecessors*).
        self.propagate_line_numbers();
        // CPython's table is a disjoint innermost-wins partition (built
        // from per-instruction `i_except` pointers); flatten nested
        // entries the same way — a with-cleanup range wrapping a
        // send-dance's CLEANUP_THROW entry splits around it.
        self.partition_exception_table();
        // PEP 479: the generator-family epilogue handler and its
        // whole-body complement coverage, plus RESUME depth-1 flags.
        // Emitted after line propagation — the block is NO_LOCATION
        // (dis shows `--`), matching CPython's
        // `wrap_in_stopiteration_handler`.
        self.emit_stopiteration_epilogue();
        // Resolve sentinel exception depths (inlined-comprehension
        // handlers) now that offsets are final: the handler's depth is
        // the static stack depth at its protected region's start.
        if self
            .co
            .exception_table
            .iter()
            .any(|h| h.depth & HANDLER_DEPTH_ANCHOR_FLAG != 0)
        {
            let depths = crate::cpython_code::compute_startdepths(&self.co);
            for h in self.co.exception_table.iter_mut() {
                if h.depth & HANDLER_DEPTH_ANCHOR_FLAG != 0 {
                    let at = if h.depth == HANDLER_DEPTH_SENTINEL {
                        h.start
                    } else {
                        h.depth & !HANDLER_DEPTH_ANCHOR_FLAG
                    };
                    let d = depths.get(at as usize).copied().unwrap_or(-1);
                    h.depth = u32::try_from(d).unwrap_or(0);
                }
            }
        }
        // Drop entries whose covered range contains only NOPs — they can
        // never fire, and CPython's table omits them (a `return <const>`
        // try-body reduces to a lone located NOP: test_dis
        // test_disassemble_try_finally, _tryfinallyconst). Runs *after*
        // sentinel resolution: the dropped entry may have been the seed
        // that gave its (now-dead) handler block a static depth.
        {
            let instrs = &self.co.instructions;
            self.co.exception_table.retain(|h| {
                instrs
                    .get(h.start as usize..(h.end as usize).min(instrs.len()))
                    .is_none_or(|r| r.iter().any(|i| i.op != OpCode::Nop))
            });
        }
        // CPython emits the table in instruction order (it scans the
        // assembled stream); dis prints it verbatim.
        self.co.exception_table.sort_by_key(|h| (h.start, h.end));
        // Wire metadata: which backward jumps encode as
        // JUMP_BACKWARD_NO_INTERRUPT (see field docs).
        let mut noint: Vec<u32> = self
            .no_interrupt_jumps
            .iter()
            .copied()
            .filter(|&s| {
                self.co
                    .instructions
                    .get(s as usize)
                    .is_some_and(|i| i.op == OpCode::JumpBackward)
            })
            .collect();
        noint.sort_unstable();
        self.co.no_interrupt_jumps = noint;
        // RFC 0021: size the inline-cache side-table to match the
        // emitted instruction stream so the VM can index into it
        // without bounds checks on the hot path.
        self.co.caches.resize(self.co.instructions.len());
        self.co
    }

    /// Flatten the exception table into a disjoint innermost-wins
    /// partition. Emission pushes *nested* ranges (a with/finally
    /// cleanup covering a whole handler region that itself contains a
    /// send-dance CLEANUP_THROW entry); CPython's assembled table has
    /// one owner per instruction, with outer ranges split around inner
    /// ones. Plain depth sentinels are converted to anchored form
    /// first so a split half doesn't re-resolve at its own (shifted)
    /// start.
    fn partition_exception_table(&mut self) {
        let n = self.co.instructions.len();
        if self.co.exception_table.len() < 2 || n == 0 {
            return;
        }
        for h in self.co.exception_table.iter_mut() {
            if h.depth == HANDLER_DEPTH_SENTINEL {
                h.depth = HANDLER_DEPTH_ANCHOR_FLAG | h.start;
            }
        }
        // owner[i] = index of the innermost (smallest-span) entry
        // covering instruction i.
        let mut owner: Vec<Option<usize>> = vec![None; n];
        for (idx, h) in self.co.exception_table.iter().enumerate() {
            let span = h.end.saturating_sub(h.start);
            for k in h.start..h.end.min(n as u32) {
                let slot = &mut owner[k as usize];
                let replace = match slot {
                    None => true,
                    Some(prev) => {
                        let p = &self.co.exception_table[*prev];
                        span < p.end.saturating_sub(p.start)
                    }
                };
                if replace {
                    *slot = Some(idx);
                }
            }
        }
        let old = std::mem::take(&mut self.co.exception_table);
        let mut i = 0usize;
        while i < n {
            let Some(o) = owner[i] else {
                i += 1;
                continue;
            };
            let s = i;
            while i < n && owner[i] == Some(o) {
                i += 1;
            }
            let h = &old[o];
            self.co.exception_table.push(ExcHandler {
                start: s as u32,
                end: i as u32,
                handler: h.handler,
                depth: h.depth,
                push_lasti: h.push_lasti,
            });
        }
    }

    /// Port of CPython flowgraph.c `duplicate_exits_without_lineno`,
    /// over the flat stream. A basic block whose instructions are all
    /// NO_LOCATION and which exits the scope (return/raise/reraise) or
    /// ends in an eval-break back edge (JUMP_BACKWARD) cannot take a
    /// single meaningful line when several branches converge on it —
    /// CPython clones it per jump predecessor and lets
    /// `propagate_line_numbers` give each copy its path's line
    /// (test_compile test_line_number_synthetic_jump_* nested shapes).
    /// Copies are appended to the stream tail with their exception
    /// coverage replicated.
    fn duplicate_exits_without_lineno(&mut self) {
        // WeavePy's handler-exit codegen shares unwind tails between
        // paths (CPython's codegen inlines the POP_EXCEPT chain per
        // path), so one duplication round can expose another shared
        // tail — iterate to a fixpoint (bounded: each round strictly
        // reduces multi-predecessor location-free exits).
        for _ in 0..20 {
            if !self.duplicate_exits_without_lineno_once() {
                break;
            }
        }
    }

    fn duplicate_exits_without_lineno_once(&mut self) -> bool {
        let n = self.co.instructions.len();
        if n == 0 {
            return false;
        }
        let insns = &self.co.instructions;
        let jump_target = |i: usize| -> Option<usize> {
            let ins = insns[i];
            let from = i + 1;
            match ins.op {
                OpCode::JumpForward
                | OpCode::PopJumpIfFalse
                | OpCode::PopJumpIfTrue
                | OpCode::PopJumpIfNone
                | OpCode::PopJumpIfNotNone
                | OpCode::ForIter
                | OpCode::Send => Some(from + ins.arg as usize),
                OpCode::JumpBackward => Some(from.saturating_sub(ins.arg as usize)),
                _ => None,
            }
        };
        let no_fallthrough = |i: usize| {
            matches!(
                insns[i].op,
                OpCode::JumpForward
                    | OpCode::JumpBackward
                    | OpCode::ReturnValue
                    | OpCode::RaiseVarargs
                    | OpCode::Reraise
            )
        };
        let mut leader = vec![false; n];
        leader[0] = true;
        for h in &self.co.exception_table {
            if (h.handler as usize) < n {
                leader[h.handler as usize] = true;
            }
        }
        for i in 0..n {
            if let Some(t) = jump_target(i) {
                if t < n {
                    leader[t] = true;
                }
            }
            if (jump_target(i).is_some() || no_fallthrough(i)) && i + 1 < n {
                leader[i + 1] = true;
            }
        }
        // Block extent and candidacy per leader.
        let block_end = |start: usize| -> usize {
            let mut e = start;
            while e + 1 < n && !leader[e + 1] {
                e += 1;
            }
            e
        };
        // Jump predecessor sites and whether a fallthrough edge exists,
        // per target leader.
        let mut jump_preds: HashMap<usize, Vec<usize>> = HashMap::new();
        let mut has_fallthrough_pred = vec![false; n];
        for i in 0..n {
            if let Some(t) = jump_target(i) {
                if t < n && leader[t] {
                    jump_preds.entry(t).or_default().push(i);
                }
            }
            if i + 1 < n && leader[i + 1] && !no_fallthrough(i) {
                has_fallthrough_pred[i + 1] = true;
            }
        }
        let candidates: Vec<usize> = (0..n)
            .filter(|&t| {
                if !leader[t] {
                    return false;
                }
                let e = block_end(t);
                // All instructions location-free…
                (t..=e).all(|k| self.co.linetable[k] == 0)
                    // …and the block exits scope, polls the eval
                    // breaker, or is a bare connector hopping to one
                    // (a shared POP_EXCEPT unwind tail).
                    && matches!(
                        self.co.instructions[e].op,
                        OpCode::ReturnValue
                            | OpCode::RaiseVarargs
                            | OpCode::Reraise
                            | OpCode::JumpBackward
                            | OpCode::JumpForward
                    )
            })
            .collect();
        let mut changed = false;
        for t in candidates {
            let e = block_end(t);
            let Some(preds) = jump_preds.get(&t) else {
                continue;
            };
            // Only unconditional and conditional *jump* predecessors can
            // be retargeted; keep one edge on the original (the
            // fallthrough when present, else the last jump).
            let total_edges = preds.len() + usize::from(has_fallthrough_pred[t]);
            if total_edges < 2 {
                continue;
            }
            let mut may_duplicate = total_edges - 1;
            for &p in preds {
                if may_duplicate == 0 {
                    break;
                }
                may_duplicate -= 1;
                changed = true;
                let copy_start = self.co.instructions.len();
                for k in t..=e {
                    let mut ins = self.co.instructions[k];
                    if matches!(ins.op, OpCode::JumpBackward | OpCode::JumpForward) {
                        // Re-anchor the copied jump from its new offset
                        // (the copy sits at the stream tail, so a
                        // forward hop to an earlier block flips to a
                        // backward jump).
                        let orig_target = if ins.op == OpCode::JumpBackward {
                            (k + 1) - ins.arg as usize
                        } else {
                            k + 1 + ins.arg as usize
                        };
                        let new_pos = copy_start + (k - t);
                        if orig_target > new_pos {
                            ins.op = OpCode::JumpForward;
                            ins.arg = (orig_target - (new_pos + 1)) as u32;
                        } else {
                            ins.op = OpCode::JumpBackward;
                            ins.arg = (new_pos + 1 - orig_target) as u32;
                        }
                    }
                    self.co.instructions.push(ins);
                    self.co.linetable.push(0);
                    self.co.coltable.push(ColSpan::default());
                }
                let copy_end = self.co.instructions.len() as u32;
                // Replicate exception coverage (innermost-first order is
                // preserved: appended ranges are disjoint from all
                // existing ones).
                let covering: Vec<ExcHandler> = self
                    .co
                    .exception_table
                    .iter()
                    .filter(|h| (h.start as usize) <= t && e < h.end as usize)
                    .copied()
                    .collect();
                for h in covering {
                    self.co.exception_table.push(ExcHandler {
                        start: copy_start as u32,
                        end: copy_end,
                        ..h
                    });
                }
                self.retarget_jump(p, copy_start);
            }
        }
        changed
    }

    /// Point the jump at `site` to `target`, switching between
    /// JUMP_FORWARD and JUMP_BACKWARD when the direction flips
    /// (conditional jumps are forward-only and keep their op).
    fn retarget_jump(&mut self, site: usize, target: usize) {
        let from = site + 1;
        let ins = &mut self.co.instructions[site];
        match ins.op {
            OpCode::JumpForward | OpCode::JumpBackward => {
                if target >= from {
                    ins.op = OpCode::JumpForward;
                    ins.arg = (target - from) as u32;
                } else {
                    ins.op = OpCode::JumpBackward;
                    ins.arg = (from - target) as u32;
                }
            }
            _ => {
                debug_assert!(target >= from);
                ins.arg = (target - from) as u32;
            }
        }
    }

    /// Port of CPython flowgraph.c `propagate_line_numbers`, over the
    /// flat stream. Within each basic block a NO_LOCATION instruction
    /// (linetable slot 0) inherits the previous located instruction's
    /// location. At block end, the running location seeds the successor
    /// block's first instruction when that block has exactly one
    /// incoming normal edge (exception-handler entries have none, so
    /// CPython's cleanup runs — PUSH_EXC_INFO / COPY / POP_EXCEPT /
    /// RERAISE tails — stay location-free, exactly as 3.13's do).
    fn propagate_line_numbers(&mut self) {
        // A seed can flow backwards in stream order (a duplicated tail
        // at the end of the stream seeding an earlier block), so run
        // the block walk to a fixpoint.
        for _ in 0..10 {
            if !self.propagate_line_numbers_once() {
                break;
            }
        }
    }

    fn propagate_line_numbers_once(&mut self) -> bool {
        let n = self.co.instructions.len();
        if n == 0 {
            return false;
        }
        let mut changed = false;
        let jump_target = |i: usize| -> Option<usize> {
            let ins = self.co.instructions[i];
            let from = i + 1;
            match ins.op {
                OpCode::JumpForward
                | OpCode::PopJumpIfFalse
                | OpCode::PopJumpIfTrue
                | OpCode::PopJumpIfNone
                | OpCode::PopJumpIfNotNone
                | OpCode::ForIter
                | OpCode::Send => Some(from + ins.arg as usize),
                OpCode::JumpBackward => Some(from.saturating_sub(ins.arg as usize)),
                _ => None,
            }
        };
        let no_fallthrough = |i: usize| {
            matches!(
                self.co.instructions[i].op,
                OpCode::JumpForward
                    | OpCode::JumpBackward
                    | OpCode::ReturnValue
                    | OpCode::RaiseVarargs
                    | OpCode::Reraise
            )
        };
        // Block leaders: entry, jump targets, exception-handler entries,
        // and every instruction after a jump or scope exit.
        let mut leader = vec![false; n];
        leader[0] = true;
        for h in &self.co.exception_table {
            if (h.handler as usize) < n {
                leader[h.handler as usize] = true;
            }
        }
        for i in 0..n {
            if let Some(t) = jump_target(i) {
                if t < n {
                    leader[t] = true;
                }
            }
            if (jump_target(i).is_some() || no_fallthrough(i)) && i + 1 < n {
                leader[i + 1] = true;
            }
        }
        // Incoming normal-edge counts per leader (CPython's
        // b_predecessors: fallthrough + jump edges; exception edges
        // don't count).
        let mut preds = vec![0u32; n];
        for i in 0..n {
            if let Some(t) = jump_target(i) {
                if t < n {
                    preds[t] += 1;
                }
            }
            if i + 1 < n && leader[i + 1] && !no_fallthrough(i) {
                preds[i + 1] += 1;
            }
        }
        // Walk blocks in stream order, filling and seeding as CPython
        // does (seeds cascade into later blocks).
        let mut i = 0usize;
        while i < n {
            debug_assert!(leader[i]);
            let mut prev_line = 0u32;
            let mut prev_col = ColSpan::default();
            let mut end = i;
            loop {
                if self.co.linetable[end] == 0 {
                    if prev_line != 0 {
                        self.co.linetable[end] = prev_line;
                        self.co.coltable[end] = prev_col;
                        changed = true;
                    }
                } else {
                    prev_line = self.co.linetable[end];
                    prev_col = self.co.coltable[end];
                }
                if end + 1 >= n || leader[end + 1] {
                    break;
                }
                end += 1;
            }
            if prev_line != 0 {
                // Seed the fallthrough successor.
                if !no_fallthrough(end)
                    && end + 1 < n
                    && preds[end + 1] == 1
                    && self.co.linetable[end + 1] == 0
                {
                    self.co.linetable[end + 1] = prev_line;
                    self.co.coltable[end + 1] = prev_col;
                    changed = true;
                }
                // Seed the jump target.
                if let Some(t) = jump_target(end) {
                    if t < n && preds[t] == 1 && self.co.linetable[t] == 0 {
                        self.co.linetable[t] = prev_line;
                        self.co.coltable[t] = prev_col;
                        changed = true;
                    }
                }
            }
            i = end + 1;
        }
        changed
    }

    /// Drop instructions no execution path can reach, remapping every
    /// relative jump displacement, the exception table, and the
    /// line/column tables. Successors are the fallthrough (except
    /// after returns/raises/unconditional jumps), jump targets, and —
    /// once any instruction in a protected range is reachable — that
    /// range's exception handler (iterated to a fixpoint, since a
    /// handler body can itself be protected by another handler).
    fn eliminate_unreachable(&mut self) {
        let n = self.co.instructions.len();
        if n == 0 {
            return;
        }
        let mut reachable = vec![false; n];
        let mut stack: Vec<u32> = vec![0];
        let mut handler_seen = vec![false; self.co.exception_table.len()];
        loop {
            while let Some(i) = stack.pop() {
                let i = i as usize;
                if i >= n || reachable[i] {
                    continue;
                }
                reachable[i] = true;
                let ins = self.co.instructions[i];
                let from = i as u32 + 1;
                match ins.op {
                    OpCode::ReturnValue | OpCode::RaiseVarargs | OpCode::Reraise => {}
                    OpCode::JumpForward => stack.push(from + ins.arg),
                    OpCode::JumpBackward => stack.push(from.saturating_sub(ins.arg)),
                    OpCode::PopJumpIfFalse
                    | OpCode::PopJumpIfTrue
                    | OpCode::PopJumpIfNone
                    | OpCode::PopJumpIfNotNone
                    | OpCode::ForIter
                    | OpCode::Send => {
                        stack.push(from + ins.arg);
                        stack.push(from);
                    }
                    _ => stack.push(from),
                }
            }
            let mut changed = false;
            for (hi, h) in self.co.exception_table.iter().enumerate() {
                if handler_seen[hi] {
                    continue;
                }
                let lo = h.start as usize;
                let end = (h.end as usize).min(n);
                if (lo..end).any(|k| reachable[k]) {
                    handler_seen[hi] = true;
                    stack.push(h.handler);
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        self.compact_stream(&reachable);
    }

    /// Drop every instruction whose `keep` slot is false, remapping
    /// relative jump displacements, `PUSH_EXC_INFO`'s absolute
    /// body-end tag, the exception table, the line/column tables, and
    /// the synthetic-jump set. A jump *to* a dropped instruction lands
    /// on the next kept one.
    // Index-driven on purpose: one instruction offset indexes the
    // parallel `keep`/`instructions`/`linetable`/`coltable` arrays.
    #[allow(clippy::needless_range_loop)]
    fn compact_stream(&mut self, keep: &[bool]) {
        let n = self.co.instructions.len();
        debug_assert_eq!(keep.len(), n);
        if keep.iter().all(|&r| r) {
            return;
        }
        // new_off[x] = number of kept instructions before x;
        // n + 1 entries so exclusive range ends map too.
        let mut new_off = vec![0u32; n + 1];
        let mut cnt = 0u32;
        for i in 0..n {
            new_off[i] = cnt;
            if keep[i] {
                cnt += 1;
            }
        }
        new_off[n] = cnt;
        let mut k = 0usize;
        for i in 0..n {
            if keep[i] {
                self.co.instructions[k] = self.co.instructions[i];
                self.co.linetable[k] = self.co.linetable[i];
                self.co.coltable[k] = self.co.coltable[i];
                k += 1;
            }
        }
        self.co.instructions.truncate(k);
        self.co.linetable.truncate(k);
        self.co.coltable.truncate(k);
        for i in 0..n {
            if !keep[i] {
                continue;
            }
            let ins = self.co.instructions[new_off[i] as usize];
            let from_old = i as u32 + 1;
            // PUSH_EXC_INFO carries an *absolute* offset (the pc just
            // past its handler body, used to tag the unwinder entry).
            if ins.op == OpCode::PushExcInfo {
                self.co.instructions[new_off[i] as usize].arg = new_off[(ins.arg as usize).min(n)];
                continue;
            }
            let (t_old, backward) = match ins.op {
                OpCode::JumpForward
                | OpCode::PopJumpIfFalse
                | OpCode::PopJumpIfTrue
                | OpCode::PopJumpIfNone
                | OpCode::PopJumpIfNotNone
                | OpCode::ForIter
                | OpCode::Send => (from_old + ins.arg, false),
                OpCode::JumpBackward => (from_old.saturating_sub(ins.arg), true),
                _ => continue,
            };
            let from_new = new_off[i] + 1;
            let t_new = new_off[(t_old as usize).min(n)];
            self.co.instructions[new_off[i] as usize].arg = if backward {
                from_new - t_new
            } else {
                t_new - from_new
            };
        }
        self.co.exception_table.retain_mut(|h| {
            let lo = h.start as usize;
            let end = (h.end as usize).min(n);
            if lo >= end {
                return false;
            }
            let s = new_off[lo];
            let e = new_off[end];
            if s == e {
                return false;
            }
            h.start = s;
            h.end = e;
            h.handler = new_off[h.handler as usize];
            if h.depth & HANDLER_DEPTH_ANCHOR_FLAG != 0 && h.depth != HANDLER_DEPTH_SENTINEL {
                let anchor = (h.depth & !HANDLER_DEPTH_ANCHOR_FLAG) as usize;
                h.depth = HANDLER_DEPTH_ANCHOR_FLAG | new_off[anchor.min(n)];
            }
            true
        });
        self.synthetic_jumps = self
            .synthetic_jumps
            .iter()
            .filter(|&&s| (s as usize) < n && keep[s as usize])
            .map(|&s| new_off[s as usize])
            .collect();
        self.cold_rejoins = self
            .cold_rejoins
            .iter()
            .filter(|&&s| (s as usize) < n && keep[s as usize])
            .map(|&s| new_off[s as usize])
            .collect();
        self.no_interrupt_jumps = self
            .no_interrupt_jumps
            .iter()
            .filter(|&&s| (s as usize) < n && keep[s as usize])
            .map(|&s| new_off[s as usize])
            .collect();
    }

    /// Jump targets and exception-handler entries: positions a
    /// same-block peephole scan must not cross.
    fn block_leaders(&self) -> Vec<bool> {
        let n = self.co.instructions.len();
        let mut leader = vec![false; n + 1];
        for i in 0..n {
            let ins = self.co.instructions[i];
            let from = i as u32 + 1;
            match ins.op {
                OpCode::JumpForward
                | OpCode::PopJumpIfFalse
                | OpCode::PopJumpIfTrue
                | OpCode::PopJumpIfNone
                | OpCode::PopJumpIfNotNone
                | OpCode::ForIter
                | OpCode::Send => leader[((from + ins.arg) as usize).min(n)] = true,
                OpCode::JumpBackward => {
                    leader[(from.saturating_sub(ins.arg) as usize).min(n)] = true;
                }
                _ => {}
            }
        }
        for h in &self.co.exception_table {
            leader[(h.handler as usize).min(n)] = true;
        }
        leader
    }

    /// CPython's `optimize_basic_block`: a conditional jump on a
    /// constant becomes either a plain jump (branch always taken) or
    /// nothing (never taken); `if 0:` / `while False:` bodies then die
    /// as unreachable code (test_compile's test_false_while_loop and
    /// test_consts_in_conditionals).
    fn fold_const_branches(&mut self) {
        let n = self.co.instructions.len();
        if n == 0 {
            return;
        }
        let leader = self.block_leaders();
        for i in 1..n {
            let is_true_jump = match self.co.instructions[i].op {
                OpCode::PopJumpIfTrue => true,
                OpCode::PopJumpIfFalse => false,
                _ => continue,
            };
            // A branch that is itself a jump target has predecessors
            // pushing runtime values; the pop must stay.
            if leader[i] {
                continue;
            }
            // CPython's `optimize_load_const` also sees through a
            // `COPY 1` between the constant load and the branch — the
            // value-position `and`/`or` shape (`LOAD_CONST; COPY 1;
            // POP_JUMP_IF_x`). The COPY dies; the LOAD_CONST survives
            // as the expression's value (test_consts_in_conditionals).
            let prev_idx = if self.co.instructions[i - 1].op == OpCode::CopyTop
                && self.co.instructions[i - 1].arg <= 1
                && !leader[i - 1]
                && i >= 2
            {
                i - 2
            } else {
                i - 1
            };
            let prev = self.co.instructions[prev_idx];
            if prev.op != OpCode::LoadConst {
                continue;
            }
            // `Unmarshallable` never occurs here (only via
            // `code.replace`, after compilation).
            let truthy = match &self.co.constants[prev.arg as usize] {
                Constant::None => false,
                Constant::Bool(b) => *b,
                Constant::Int(v) => *v != 0,
                Constant::BigInt(v) => !num_traits::Zero::is_zero(v),
                Constant::Float(f) => *f != 0.0,
                Constant::Complex(r, i) => *r != 0.0 || *i != 0.0,
                Constant::Str(s) => !s.is_empty(),
                Constant::WStr(p) => !p.is_empty(),
                Constant::Bytes(b) => !b.is_empty(),
                Constant::Tuple(t) | Constant::FrozenSet(t) => !t.is_empty(),
                Constant::Code(_) | Constant::Ellipsis | Constant::Unmarshallable => true,
            };
            // Non-copy form: the LOAD_CONST at i-1 dies (the branch
            // consumed it). Copy form: the COPY at i-1 dies and the
            // LOAD_CONST at i-2 stays — its value is the expression
            // result on the taken path, or feed for the fallthrough's
            // POP_TOP (which the const/pop pair pass then removes).
            self.co.instructions[i - 1] = Instruction {
                op: OpCode::Nop,
                arg: 0,
            };
            if truthy == is_true_jump {
                self.co.instructions[i].op = OpCode::JumpForward;
            } else {
                self.co.instructions[i] = Instruction {
                    op: OpCode::Nop,
                    arg: 0,
                };
            }
        }
    }

    /// CPython's `remove_redundant_nops_and_pairs`: a `LOAD_CONST`
    /// whose value is immediately discarded by a same-block `POP_TOP`
    /// (only NOPs between) — both die. This is what reduces the
    /// fallthrough leg of a constant `and`/`or` operand after
    /// [`Self::fold_const_branches`] neutralised the branch.
    // Index-driven on purpose: `i` addresses both `leader` and the
    // mutated `instructions`, and is stored into `prev_load`.
    #[allow(clippy::needless_range_loop)]
    fn fold_const_pops(&mut self) {
        let n = self.co.instructions.len();
        let leader = self.block_leaders();
        let mut prev_load: Option<usize> = None;
        for i in 0..n {
            if leader[i] {
                prev_load = None;
            }
            match self.co.instructions[i].op {
                OpCode::LoadConst => prev_load = Some(i),
                OpCode::Nop => {}
                OpCode::PopTop => {
                    if let Some(p) = prev_load.take() {
                        self.co.instructions[p] = Instruction {
                            op: OpCode::Nop,
                            arg: 0,
                        };
                        self.co.instructions[i] = Instruction {
                            op: OpCode::Nop,
                            arg: 0,
                        };
                    }
                }
                _ => prev_load = None,
            }
        }
    }

    /// CPython's `fold_tuple_of_constants`: `BUILD_TUPLE n` over `n`
    /// constant loads collapses to a single tuple constant. Function
    /// default tuples are the load-bearing case — test_dis indexes
    /// `outer.__code__.co_consts[1]` expecting the folded shape.
    fn fold_const_tuples(&mut self) {
        let n = self.co.instructions.len();
        let leader = self.block_leaders();
        for i in 0..n {
            if self.co.instructions[i].op != OpCode::BuildTuple {
                continue;
            }
            let count = self.co.instructions[i].arg as usize;
            if count > i {
                continue;
            }
            let start = i - count;
            if (start..i).any(|k| self.co.instructions[k].op != OpCode::LoadConst)
                || ((start + 1)..=i).any(|k| leader[k])
            {
                continue;
            }
            let items: Vec<Constant> = (start..i)
                .map(|k| self.co.constants[self.co.instructions[k].arg as usize].clone())
                .collect();
            let idx = self.co.intern_constant(Constant::Tuple(items));
            for k in start..i {
                self.co.instructions[k] = Instruction {
                    op: OpCode::Nop,
                    arg: 0,
                };
            }
            self.co.instructions[i] = Instruction {
                op: OpCode::LoadConst,
                arg: idx,
            };
        }
    }

    /// CPython's `remove_unused_consts`: drop pool entries nothing
    /// references, keeping slot 0 (the docstring slot) and renumbering
    /// every LOAD_CONST.
    fn remove_unused_consts(&mut self) {
        let nconsts = self.co.constants.len();
        if nconsts == 0 {
            return;
        }
        let mut used = vec![false; nconsts];
        used[0] = true;
        for ins in &self.co.instructions {
            if ins.op == OpCode::LoadConst {
                if let Some(u) = used.get_mut(ins.arg as usize) {
                    *u = true;
                }
            }
        }
        if used.iter().all(|&u| u) {
            return;
        }
        let mut remap = vec![0u32; nconsts];
        let mut kept = Vec::with_capacity(nconsts);
        for (i, &keep) in used.iter().enumerate() {
            if keep {
                remap[i] = kept.len() as u32;
                kept.push(self.co.constants[i].clone());
            }
        }
        self.co.constants = kept;
        for ins in self.co.instructions.iter_mut() {
            if ins.op == OpCode::LoadConst {
                ins.arg = remap[ins.arg as usize];
            }
        }
    }

    /// CPython's `optimize_basic_block` rewrites a literal pack
    /// immediately unpacked — `a, b = b, a` — into direct stack
    /// shuffling: `BUILD_TUPLE 1; UNPACK_SEQUENCE 1` becomes nothing,
    /// and for 2 or 3 elements the pair becomes a single `SWAP`
    /// (test_peepholer's test_pack_unpack).
    fn optimize_pack_unpack(&mut self) {
        let n = self.co.instructions.len();
        // CPython's pass runs per basic block: the pair must not be
        // fused when the UNPACK_SEQUENCE is a jump target or handler
        // entry — another path reaches it *without* the BUILD_TUPLE
        // (`a, b = f() if c else (x, y)` merges a one-value branch
        // into the unpack; fusing corrupted that branch's stack).
        let mut leader = vec![false; n + 1];
        for i in 0..n {
            let ins = self.co.instructions[i];
            let from = i as u32 + 1;
            match ins.op {
                OpCode::JumpForward
                | OpCode::PopJumpIfFalse
                | OpCode::PopJumpIfTrue
                | OpCode::PopJumpIfNone
                | OpCode::PopJumpIfNotNone
                | OpCode::ForIter
                | OpCode::Send => leader[((from + ins.arg) as usize).min(n)] = true,
                OpCode::JumpBackward => {
                    leader[(from.saturating_sub(ins.arg) as usize).min(n)] = true;
                }
                _ => {}
            }
        }
        for h in &self.co.exception_table {
            leader[(h.handler as usize).min(n)] = true;
        }
        for i in 0..n.saturating_sub(1) {
            let a = self.co.instructions[i];
            let b = self.co.instructions[i + 1];
            if a.op != OpCode::BuildTuple || b.op != OpCode::UnpackSequence || a.arg != b.arg {
                continue;
            }
            if leader[i + 1] {
                continue;
            }
            match a.arg {
                1 => {
                    self.co.instructions[i] = Instruction {
                        op: OpCode::Nop,
                        arg: 0,
                    };
                    self.co.instructions[i + 1] = Instruction {
                        op: OpCode::Nop,
                        arg: 0,
                    };
                }
                2 | 3 => {
                    self.co.instructions[i] = Instruction {
                        op: OpCode::Nop,
                        arg: 0,
                    };
                    self.co.instructions[i + 1] = Instruction {
                        op: OpCode::Swap,
                        arg: a.arg,
                    };
                }
                _ => {}
            }
        }
    }

    /// CPython's `apply_static_swaps`: a `SWAP k` whose next `k`
    /// same-line consumers are plain stores/pops is applied
    /// *statically* by exchanging the first and k-th consumer
    /// instructions instead — match statements and unpacking
    /// assignments compile without any runtime SWAP (test_peepholer's
    /// test_static_swaps family).
    fn apply_static_swaps(&mut self) {
        let n = self.co.instructions.len();
        // Same-block scans must not cross a jump target or handler entry.
        let mut leader = vec![false; n + 1];
        for i in 0..n {
            let ins = self.co.instructions[i];
            let from = i as u32 + 1;
            match ins.op {
                OpCode::JumpForward
                | OpCode::PopJumpIfFalse
                | OpCode::PopJumpIfTrue
                | OpCode::PopJumpIfNone
                | OpCode::PopJumpIfNotNone
                | OpCode::ForIter
                | OpCode::Send => leader[((from + ins.arg) as usize).min(n)] = true,
                OpCode::JumpBackward => {
                    leader[(from.saturating_sub(ins.arg) as usize).min(n)] = true;
                }
                _ => {}
            }
        }
        for h in &self.co.exception_table {
            leader[(h.handler as usize).min(n)] = true;
        }
        let swappable = |op: OpCode| matches!(op, OpCode::StoreFast | OpCode::PopTop);
        let stores_to =
            |ins: Instruction| -> Option<u32> { (ins.op == OpCode::StoreFast).then_some(ins.arg) };
        // Next swappable instruction after `i`, skipping NOPs, staying
        // in-block, and (when `lineno` is a real line) on that line.
        let next_swappable = |co: &CodeObject, leader: &[bool], mut i: usize, lineno: u32| loop {
            i += 1;
            if i >= n || leader[i] {
                return None;
            }
            if lineno != 0 && co.linetable[i] != lineno {
                return None;
            }
            match co.instructions[i].op {
                OpCode::Nop => {}
                op if swappable(op) => return Some(i),
                _ => return None,
            }
        };
        // Right-to-left, as CPython's downward scan: a SWAP chain
        // (`SWAP 3; SWAP 2; stores…`) resolves the rightmost SWAP
        // first so the earlier one can see through the fresh NOP.
        for i in (0..n).rev() {
            let swap = self.co.instructions[i];
            if swap.op != OpCode::Swap {
                continue;
            }
            if swap.arg <= 1 {
                self.co.instructions[i] = Instruction {
                    op: OpCode::Nop,
                    arg: 0,
                };
                continue;
            }
            let Some(j) = next_swappable(&self.co, &leader, i, 0) else {
                continue;
            };
            let lineno = self.co.linetable[j];
            let mut k = j;
            let mut ok = true;
            for _ in 1..swap.arg {
                match next_swappable(&self.co, &leader, k, lineno) {
                    Some(nk) => k = nk,
                    None => {
                        ok = false;
                        break;
                    }
                }
            }
            if !ok {
                continue;
            }
            // Reordering two stores to the same local — or hopping a
            // store over another to the same slot — changes results.
            let store_j = stores_to(self.co.instructions[j]);
            let store_k = stores_to(self.co.instructions[k]);
            if store_j.is_some() || store_k.is_some() {
                if store_j == store_k {
                    continue;
                }
                if ((j + 1)..k).any(|idx| {
                    let s = stores_to(self.co.instructions[idx]);
                    s.is_some() && (s == store_j || s == store_k)
                }) {
                    continue;
                }
            }
            self.co.instructions[i] = Instruction {
                op: OpCode::Nop,
                arg: 0,
            };
            self.co.instructions.swap(j, k);
            self.co.linetable.swap(j, k);
            self.co.coltable.swap(j, k);
        }
    }

    /// CPython's `normalize_jumps`: a conditional jump that (after
    /// threading) would land on an eligible unconditional back edge is
    /// inverted to fall into a fresh `JUMP_BACKWARD` trampoline
    /// carrying the conditional's own location — runtime conditional
    /// jumps only encode forward displacements (test_peepholer's
    /// test_elim_jump_to_uncond_jump4).
    fn normalize_backward_conditionals(&mut self) {
        let n = self.co.instructions.len();
        // (site, final backward target) pairs, in stream order.
        let mut sites: Vec<(usize, u32)> = Vec::new();
        for i in 0..n {
            let ins = self.co.instructions[i];
            if !matches!(ins.op, OpCode::PopJumpIfFalse | OpCode::PopJumpIfTrue) {
                continue;
            }
            let t = i as u32 + 1 + ins.arg;
            if (t as usize) >= n {
                continue;
            }
            let tin = self.co.instructions[t as usize];
            // Same eligibility as jump threading: hop only through a
            // synthetic (NO_LOCATION) jump or one on the site's line.
            if tin.op == OpCode::JumpBackward
                && (self.synthetic_jumps.contains(&t)
                    || self.co.linetable[t as usize] == self.co.linetable[i])
            {
                sites.push((i, (t + 1).saturating_sub(tin.arg)));
            }
        }
        if sites.is_empty() {
            return;
        }
        // new(x) = x + number of trampolines inserted before x.
        let shift = |x: u32| -> u32 {
            x + sites.iter().take_while(|&&(s, _)| (s as u32) < x).count() as u32
        };
        let mut instructions = Vec::with_capacity(n + sites.len());
        let mut linetable = Vec::with_capacity(n + sites.len());
        let mut coltable = Vec::with_capacity(n + sites.len());
        let mut site_iter = sites.iter().peekable();
        for i in 0..n {
            let mut ins = self.co.instructions[i];
            let from_old = i as u32 + 1;
            let from_new = shift(i as u32) + 1;
            if let Some(&&(s, back_target)) = site_iter.peek() {
                if s == i {
                    site_iter.next();
                    // Inverted condition falls past the trampoline
                    // into the old fallthrough (always displacement 1).
                    ins.op = if ins.op == OpCode::PopJumpIfFalse {
                        OpCode::PopJumpIfTrue
                    } else {
                        OpCode::PopJumpIfFalse
                    };
                    ins.arg = 1;
                    instructions.push(ins);
                    linetable.push(self.co.linetable[i]);
                    coltable.push(self.co.coltable[i]);
                    // The trampoline takes the conditional's location
                    // (CPython normalize_jumps_in_block uses last->i_loc).
                    let p = from_new; // trampoline's new position
                    let t_new = shift(back_target);
                    if t_new <= p + 1 {
                        instructions.push(Instruction {
                            op: OpCode::JumpBackward,
                            arg: (p + 1) - t_new,
                        });
                    } else {
                        instructions.push(Instruction {
                            op: OpCode::JumpForward,
                            arg: t_new - (p + 1),
                        });
                    }
                    linetable.push(self.co.linetable[i]);
                    coltable.push(self.co.coltable[i]);
                    continue;
                }
            }
            match ins.op {
                OpCode::PushExcInfo => {
                    ins.arg = shift(ins.arg);
                }
                OpCode::JumpForward
                | OpCode::PopJumpIfFalse
                | OpCode::PopJumpIfTrue
                | OpCode::PopJumpIfNone
                | OpCode::PopJumpIfNotNone
                | OpCode::ForIter
                | OpCode::Send => {
                    ins.arg = shift(from_old + ins.arg) - from_new;
                }
                OpCode::JumpBackward => {
                    ins.arg = from_new - shift(from_old - ins.arg);
                }
                _ => {}
            }
            instructions.push(ins);
            linetable.push(self.co.linetable[i]);
            coltable.push(self.co.coltable[i]);
        }
        self.co.instructions = instructions;
        self.co.linetable = linetable;
        self.co.coltable = coltable;
        for h in self.co.exception_table.iter_mut() {
            h.start = shift(h.start);
            h.end = shift(h.end);
            h.handler = shift(h.handler);
            if h.depth & HANDLER_DEPTH_ANCHOR_FLAG != 0 && h.depth != HANDLER_DEPTH_SENTINEL {
                h.depth = HANDLER_DEPTH_ANCHOR_FLAG | shift(h.depth & !HANDLER_DEPTH_ANCHOR_FLAG);
            }
        }
        self.synthetic_jumps = self.synthetic_jumps.iter().map(|&s| shift(s)).collect();
        self.cold_rejoins = self.cold_rejoins.iter().map(|&s| shift(s)).collect();
        self.no_interrupt_jumps = self.no_interrupt_jumps.iter().map(|&s| shift(s)).collect();
    }

    /// Port of CPython flowgraph.c `push_cold_blocks_to_end` over the
    /// flat stream. Blocks unreachable through normal control flow
    /// (fallthrough + jumps from the entry) are *cold* — they can only
    /// be entered through the exception table. CPython moves every
    /// cold block to the end of the function, preserving their
    /// relative order, and materialises any cold→warm fallthrough
    /// edge as an explicit `JUMP_NO_INTERRUPT` (a send-dance's
    /// CLEANUP_THROW rejoining its END_SEND). The synthesized jumps
    /// are recorded in [`Self::cold_rejoins`]: CPython creates them
    /// after `label_exception_targets`, so they carry no exception
    /// coverage.
    fn push_cold_blocks_to_end(&mut self) {
        let n = self.co.instructions.len();
        if n == 0 {
            return;
        }
        let insns = &self.co.instructions;
        let jump_target = |i: usize| -> Option<usize> {
            let ins = insns[i];
            let from = i + 1;
            match ins.op {
                OpCode::JumpForward
                | OpCode::PopJumpIfFalse
                | OpCode::PopJumpIfTrue
                | OpCode::PopJumpIfNone
                | OpCode::PopJumpIfNotNone
                | OpCode::ForIter
                | OpCode::Send => Some(from + ins.arg as usize),
                OpCode::JumpBackward => Some(from.saturating_sub(ins.arg as usize)),
                _ => None,
            }
        };
        let no_fallthrough = |i: usize| {
            matches!(
                insns[i].op,
                OpCode::JumpForward
                    | OpCode::JumpBackward
                    | OpCode::ReturnValue
                    | OpCode::RaiseVarargs
                    | OpCode::Reraise
            )
        };
        // Basic-block leaders.
        let mut leader = vec![false; n];
        leader[0] = true;
        for i in 0..n {
            if let Some(t) = jump_target(i) {
                if t < n {
                    leader[t] = true;
                }
            }
            if (jump_target(i).is_some() || no_fallthrough(i)) && i + 1 < n {
                leader[i + 1] = true;
            }
        }
        for h in &self.co.exception_table {
            if (h.handler as usize) < n {
                leader[h.handler as usize] = true;
            }
        }
        let mut block_starts: Vec<usize> = (0..n).filter(|&i| leader[i]).collect();
        block_starts.push(n);
        let nb = block_starts.len() - 1;
        let mut block_of = vec![0usize; n];
        for b in 0..nb {
            block_of[block_starts[b]..block_starts[b + 1]].fill(b);
        }
        // Warm = reachable from block 0 via normal control flow only.
        let mut warm = vec![false; nb];
        let mut stack = vec![0usize];
        while let Some(b) = stack.pop() {
            if warm[b] {
                continue;
            }
            warm[b] = true;
            let last = block_starts[b + 1] - 1;
            if let Some(t) = jump_target(last) {
                if t < n {
                    stack.push(block_of[t]);
                }
            }
            if !no_fallthrough(last) && b + 1 < nb {
                stack.push(b + 1);
            }
        }
        if warm.iter().all(|&w| w) {
            return;
        }
        // Already laid out warm-then-cold? Nothing to do — this keeps
        // the pass a no-op for the shapes codegen already emits in
        // CPython's canonical order.
        let first_cold = warm.iter().position(|&w| !w).unwrap();
        if warm[first_cold..].iter().all(|&w| !w) {
            return;
        }
        // A conditional jump crossing the warm/cold boundary would
        // need direction normalisation this pass doesn't do — bail
        // out (CPython can't produce such an edge either: a cold
        // block's conditional targets are cold, a warm one's warm).
        for i in 0..n {
            if matches!(
                insns[i].op,
                OpCode::PopJumpIfFalse
                    | OpCode::PopJumpIfTrue
                    | OpCode::PopJumpIfNone
                    | OpCode::PopJumpIfNotNone
                    | OpCode::ForIter
                    | OpCode::Send
            ) {
                if let Some(t) = jump_target(i) {
                    if t < n && warm[block_of[i]] != warm[block_of[t]] {
                        return;
                    }
                }
            }
        }
        // New layout: warm blocks in order, then cold blocks in order.
        // `Rejoin(target)` marks a synthesized explicit jump for a
        // moved cold block whose fallthrough successor is warm.
        enum Slot {
            Old(usize),
            Rejoin(usize),
        }
        let mut layout: Vec<Slot> = Vec::with_capacity(n + 4);
        for &want_warm in &[true, false] {
            for b in 0..nb {
                if warm[b] != want_warm {
                    continue;
                }
                for k in block_starts[b]..block_starts[b + 1] {
                    layout.push(Slot::Old(k));
                }
                let last = block_starts[b + 1] - 1;
                if !want_warm && !no_fallthrough(last) && b + 1 < nb && warm[b + 1] {
                    layout.push(Slot::Rejoin(block_starts[b + 1]));
                }
            }
        }
        // Map old index -> new index.
        let mut new_index = vec![0u32; n + 1];
        for (pos, slot) in layout.iter().enumerate() {
            if let Slot::Old(k) = slot {
                new_index[*k] = pos as u32;
            }
        }
        new_index[n] = layout.len() as u32;
        // Convert plain depth sentinels to anchored form *before* the
        // reorder: a plain sentinel resolves at the entry's start, and
        // splitting/remapping the range must not change which
        // instruction that is.
        for h in self.co.exception_table.iter_mut() {
            if h.depth == HANDLER_DEPTH_SENTINEL {
                h.depth = HANDLER_DEPTH_ANCHOR_FLAG | h.start;
            }
        }
        // Rebuild the stream.
        let old_insns = std::mem::take(&mut self.co.instructions);
        let old_lines = std::mem::take(&mut self.co.linetable);
        let old_cols = std::mem::take(&mut self.co.coltable);
        let mut rejoin_sites: Vec<u32> = Vec::new();
        for slot in &layout {
            match slot {
                Slot::Old(k) => {
                    let mut ins = old_insns[*k];
                    let from_new = new_index[*k] + 1;
                    match ins.op {
                        OpCode::PushExcInfo => {
                            ins.arg = new_index[(ins.arg as usize).min(n)];
                        }
                        OpCode::JumpForward | OpCode::JumpBackward => {
                            let t_old = if ins.op == OpCode::JumpBackward {
                                (*k as u32 + 1).saturating_sub(ins.arg)
                            } else {
                                *k as u32 + 1 + ins.arg
                            };
                            let t_new = new_index[(t_old as usize).min(n)];
                            if t_new >= from_new {
                                ins.op = OpCode::JumpForward;
                                ins.arg = t_new - from_new;
                            } else {
                                ins.op = OpCode::JumpBackward;
                                ins.arg = from_new - t_new;
                            }
                        }
                        OpCode::PopJumpIfFalse
                        | OpCode::PopJumpIfTrue
                        | OpCode::PopJumpIfNone
                        | OpCode::PopJumpIfNotNone
                        | OpCode::ForIter
                        | OpCode::Send => {
                            let t_old = *k as u32 + 1 + ins.arg;
                            let t_new = new_index[(t_old as usize).min(n)];
                            debug_assert!(t_new >= from_new);
                            ins.arg = t_new - from_new;
                        }
                        _ => {}
                    }
                    self.co.instructions.push(ins);
                    self.co.linetable.push(old_lines[*k]);
                    self.co.coltable.push(old_cols[*k]);
                }
                Slot::Rejoin(target_old) => {
                    let pos = self.co.instructions.len() as u32;
                    let t_new = new_index[*target_old];
                    debug_assert!(t_new < pos, "rejoin target must be warm (already placed)");
                    self.co.instructions.push(Instruction {
                        op: OpCode::JumpBackward,
                        arg: (pos + 1) - t_new,
                    });
                    // NO_LOCATION, like CPython's explicit_jump;
                    // `propagate_line_numbers` fills it later.
                    self.co.linetable.push(0);
                    self.co.coltable.push(ColSpan::default());
                    rejoin_sites.push(pos);
                }
            }
        }
        // Remap the exception table. A range whose instructions end up
        // non-contiguous is split into one entry per contiguous run.
        let old_table = std::mem::take(&mut self.co.exception_table);
        for h in old_table {
            let mut positions: Vec<u32> = (h.start..h.end.min(n as u32))
                .map(|k| new_index[k as usize])
                .collect();
            positions.sort_unstable();
            let handler = new_index[(h.handler as usize).min(n)];
            let depth =
                if h.depth & HANDLER_DEPTH_ANCHOR_FLAG != 0 && h.depth != HANDLER_DEPTH_SENTINEL {
                    let anchor = (h.depth & !HANDLER_DEPTH_ANCHOR_FLAG) as usize;
                    HANDLER_DEPTH_ANCHOR_FLAG | new_index[anchor.min(n)]
                } else {
                    h.depth
                };
            let mut run_start = 0usize;
            while run_start < positions.len() {
                let mut run_end = run_start + 1;
                while run_end < positions.len() && positions[run_end] == positions[run_end - 1] + 1
                {
                    run_end += 1;
                }
                self.co.exception_table.push(ExcHandler {
                    start: positions[run_start],
                    end: positions[run_end - 1] + 1,
                    handler,
                    depth,
                    push_lasti: h.push_lasti,
                });
                run_start = run_end;
            }
        }
        self.synthetic_jumps = self
            .synthetic_jumps
            .iter()
            .map(|&s| new_index[(s as usize).min(n)])
            .collect();
        self.no_interrupt_jumps = self
            .no_interrupt_jumps
            .iter()
            .map(|&s| new_index[(s as usize).min(n)])
            .collect();
        // This pass runs once, before any rejoins exist; the set holds
        // exactly the jumps synthesized above (in new coordinates).
        debug_assert!(self.cold_rejoins.is_empty());
        for &s in &rejoin_sites {
            self.cold_rejoins.insert(s);
            self.synthetic_jumps.insert(s);
            // flowgraph.c synthesizes rejoins as JUMP_NO_INTERRUPT.
            self.no_interrupt_jumps.insert(s);
        }
    }

    /// CPython's `wrap_in_stopiteration_handler` (PEP 479): every
    /// generator-family code object gets an outermost virtual handler
    /// covering the whole body — an escaping `StopIteration` (or, for
    /// async generators, `StopAsyncIteration`) is converted to a
    /// RuntimeError by the STOPITERATION_ERROR intrinsic and
    /// re-raised with the original raise offset (`RERAISE 1`).
    ///
    /// The flat table holds only the innermost entry per instruction,
    /// so the epilogue's coverage is the *complement*: every
    /// instruction from the entry RESUME up to the epilogue block
    /// that no other entry covers — except the cold-rejoin jumps,
    /// which CPython synthesizes after `label_exception_targets` and
    /// which therefore carry no coverage at all.
    ///
    /// Also stamps CPython's `RESUME_OPARG_DEPTH1_MASK` (bit 2): a
    /// post-yield RESUME whose preceding YIELD_VALUE is covered
    /// directly by the epilogue sits at exception-handler depth 1,
    /// which `gen.close()` uses as its no-finally fast path.
    fn emit_stopiteration_epilogue(&mut self) {
        if !(self.co.is_generator || self.co.is_coroutine || self.co.is_async_generator) {
            return;
        }
        let n = self.co.instructions.len() as u32;
        let Some(resume_at) = self
            .co
            .instructions
            .iter()
            .position(|i| i.op == OpCode::Resume)
        else {
            return;
        };
        let handler = n;
        self.emit_no_line(OpCode::StopIterationError, 0);
        self.emit_no_line(OpCode::Reraise, 1);
        let mut covered = vec![false; n as usize];
        for h in &self.co.exception_table {
            for k in h.start..h.end.min(n) {
                covered[k as usize] = true;
            }
        }
        // RESUME depth-1 flags: decided *before* the epilogue entries
        // are added — an own-yield at depth 1 is exactly one not yet
        // covered by anything.
        for i in 1..n as usize {
            let ins = self.co.instructions[i];
            if ins.op != OpCode::Resume || ins.arg == 0 {
                continue;
            }
            if self.co.instructions[i - 1].op == OpCode::YieldValue && !covered[i - 1] {
                self.co.instructions[i].arg |= 4;
            }
        }
        for &j in &self.cold_rejoins {
            if j < n {
                covered[j as usize] = true;
            }
        }
        let mut i = resume_at as u32;
        while i < n {
            if covered[i as usize] {
                i += 1;
                continue;
            }
            let s = i;
            while i < n && !covered[i as usize] {
                i += 1;
            }
            self.co.exception_table.push(ExcHandler {
                start: s,
                end: i,
                handler,
                depth: 0,
                push_lasti: true,
            });
        }
    }

    /// CPython flowgraph `basicblock_inline_small_or_no_lineno_blocks`,
    /// small-exit-block half: replace an unconditional forward jump
    /// whose target block exits the scope (return / raise / reraise)
    /// in at most `MAX_COPY_SIZE` wire instructions with an inline copy
    /// of that block. The location-free half of CPython's pass is
    /// handled by `duplicate_exits_without_lineno`. Restricted to
    /// sites and targets outside all exception coverage: CPython's
    /// copies keep their per-instruction `i_except`, which a flat
    /// range-based table cannot express for a mid-range insertion.
    fn inline_small_exit_blocks(&mut self) {
        const MAX_COPY_SIZE: usize = 4;
        let n = self.co.instructions.len();
        if n == 0 {
            return;
        }
        let covered = |i: u32| {
            self.co
                .exception_table
                .iter()
                .any(|h| h.start <= i && i < h.end)
        };
        // Block leaders, as in `duplicate_exits_without_lineno`.
        let mut leader = vec![false; n + 1];
        leader[0] = true;
        for h in &self.co.exception_table {
            if (h.handler as usize) < n {
                leader[h.handler as usize] = true;
            }
        }
        for i in 0..n {
            let ins = self.co.instructions[i];
            let from = i + 1;
            let target = match ins.op {
                OpCode::JumpForward
                | OpCode::PopJumpIfFalse
                | OpCode::PopJumpIfTrue
                | OpCode::PopJumpIfNone
                | OpCode::PopJumpIfNotNone
                | OpCode::ForIter
                | OpCode::Send => Some(from + ins.arg as usize),
                OpCode::JumpBackward => Some(from.saturating_sub(ins.arg as usize)),
                _ => None,
            };
            if let Some(t) = target {
                if t <= n {
                    leader[t] = true;
                }
                leader[(i + 1).min(n)] = true;
            }
            if matches!(
                ins.op,
                OpCode::ReturnValue | OpCode::RaiseVarargs | OpCode::Reraise
            ) {
                leader[(i + 1).min(n)] = true;
            }
        }
        // (jump site, target block range) pairs, in stream order.
        let mut sites: Vec<(usize, usize, usize)> = Vec::new();
        // Sites inside an exception-covered range whose inlined copy
        // must be punched out of the covering entries (see below).
        let mut covered_sites: Vec<usize> = Vec::new();
        for i in 0..n {
            let ins = self.co.instructions[i];
            if ins.op != OpCode::JumpForward {
                continue;
            }
            let t = i + 1 + ins.arg as usize;
            if t >= n {
                continue;
            }
            let mut e = t;
            while e + 1 < n && !leader[e + 1] {
                e += 1;
            }
            if !matches!(
                self.co.instructions[e].op,
                OpCode::ReturnValue | OpCode::RaiseVarargs | OpCode::Reraise
            ) {
                continue;
            }
            // A jump site inside an exception-covered range: CPython
            // still inlines (its per-instruction handler annotation
            // keeps the copied return *uncovered*, splitting the wire
            // table around it — the except* epilogue's POP_EXCEPT;
            // RETURN_CONST exit shows exactly that shape). Range-based
            // coverage can only mirror this for copies that cannot
            // raise: restrict to pure constant returns and punch the
            // copy out of every covering entry afterwards. Raising
            // exits (RAISE/RERAISE) keep the shared block — inlining
            // them under coverage would change which handler wins.
            let site_covered = covered(i as u32);
            if site_covered {
                let pure_return = self.co.instructions[e].op == OpCode::ReturnValue
                    && (e == t || (e == t + 1 && self.co.instructions[t].op == OpCode::LoadConst));
                if !pure_return {
                    continue;
                }
            }
            // Wire size: a fusable LOAD_CONST; RETURN_VALUE pair encodes
            // as one RETURN_CONST (CPython counts post-fold blocks).
            let mut size = e - t + 1;
            if self.co.instructions[e].op == OpCode::ReturnValue
                && self.co.instructions[e].arg != 0
                && e > t
                && self.co.instructions[e - 1].op == OpCode::LoadConst
            {
                size -= 1;
            }
            if size > MAX_COPY_SIZE {
                continue;
            }
            // PUSH_EXC_INFO carries an absolute offset and marks handler
            // context; never part of a copyable exit tail.
            if (t..=e)
                .any(|k| self.co.instructions[k].op == OpCode::PushExcInfo || covered(k as u32))
            {
                continue;
            }
            if site_covered {
                covered_sites.push(i);
            }
            sites.push((i, t, e));
        }
        if sites.is_empty() {
            return;
        }
        // new(x) = x + growth from copies inserted before x. Each site
        // replaces 1 jump with (e - t + 1) instructions.
        let shift = |x: u32| -> u32 {
            x + sites
                .iter()
                .take_while(|&&(s, _, _)| (s as u32) < x)
                .map(|&(_, t, e)| (e - t) as u32)
                .sum::<u32>()
        };
        let grown: usize = sites.iter().map(|&(_, t, e)| e - t).sum();
        let mut instructions = Vec::with_capacity(n + grown);
        let mut linetable = Vec::with_capacity(n + grown);
        let mut coltable = Vec::with_capacity(n + grown);
        let mut site_iter = sites.iter().peekable();
        for i in 0..n {
            if let Some(&&(s, t, e)) = site_iter.peek() {
                if s == i {
                    site_iter.next();
                    for k in t..=e {
                        instructions.push(self.co.instructions[k]);
                        linetable.push(self.co.linetable[k]);
                        coltable.push(self.co.coltable[k]);
                    }
                    continue;
                }
            }
            let mut ins = self.co.instructions[i];
            let from_old = i as u32 + 1;
            let from_new = shift(i as u32) + 1;
            match ins.op {
                OpCode::PushExcInfo => {
                    ins.arg = shift(ins.arg);
                }
                OpCode::JumpForward
                | OpCode::PopJumpIfFalse
                | OpCode::PopJumpIfTrue
                | OpCode::PopJumpIfNone
                | OpCode::PopJumpIfNotNone
                | OpCode::ForIter
                | OpCode::Send => {
                    ins.arg = shift(from_old + ins.arg) - from_new;
                }
                OpCode::JumpBackward => {
                    ins.arg = from_new - shift(from_old - ins.arg);
                }
                _ => {}
            }
            instructions.push(ins);
            linetable.push(self.co.linetable[i]);
            coltable.push(self.co.coltable[i]);
        }
        self.co.instructions = instructions;
        self.co.linetable = linetable;
        self.co.coltable = coltable;
        for h in self.co.exception_table.iter_mut() {
            h.start = shift(h.start);
            h.end = shift(h.end);
            h.handler = shift(h.handler);
            if h.depth & HANDLER_DEPTH_ANCHOR_FLAG != 0 && h.depth != HANDLER_DEPTH_SENTINEL {
                let anchor = h.depth & !HANDLER_DEPTH_ANCHOR_FLAG;
                h.depth = HANDLER_DEPTH_ANCHOR_FLAG | shift(anchor);
            }
        }
        let inlined: HashSet<u32> = sites.iter().map(|&(s, _, _)| s as u32).collect();
        self.synthetic_jumps = self
            .synthetic_jumps
            .iter()
            .filter(|s| !inlined.contains(s))
            .map(|&s| shift(s))
            .collect();
        self.no_interrupt_jumps = self
            .no_interrupt_jumps
            .iter()
            .filter(|s| !inlined.contains(s))
            .map(|&s| shift(s))
            .collect();
        // Punch every covered site's inlined return out of the entries
        // that covered the original jump: the copy stands in for the
        // (uncovered) shared exit block, so the wire table must split
        // around it exactly as CPython's per-instruction handler info
        // does.
        for &s in &covered_sites {
            let (_, t, e) = *sites.iter().find(|&&(si, _, _)| si == s).unwrap();
            let copy_start = shift(s as u32);
            let copy_end = copy_start + (e - t + 1) as u32;
            let mut split: Vec<ExcHandler> = Vec::new();
            for h in self.co.exception_table.iter_mut() {
                if h.start < copy_end && h.end > copy_start {
                    // Pin a plain-sentinel depth to the *original* range
                    // start first: a split half must keep resolving at
                    // the covering region's entry depth (CPython's SETUP
                    // point), not re-anchor at its own shifted start.
                    if h.depth == HANDLER_DEPTH_SENTINEL {
                        h.depth = HANDLER_DEPTH_ANCHOR_FLAG | h.start;
                    }
                    if h.end > copy_end {
                        split.push(ExcHandler {
                            start: copy_end,
                            end: h.end,
                            handler: h.handler,
                            depth: h.depth,
                            push_lasti: h.push_lasti,
                        });
                    }
                    h.end = h.start.max(copy_start);
                }
            }
            self.co.exception_table.extend(split);
            self.co.exception_table.retain(|h| h.end > h.start);
        }
    }

    /// CPython's `remove_redundant_nops`: drop a NOP when its line is
    /// already covered by a neighbour in the same basic block (or when
    /// it has no location at all), so `pass` on its own line keeps its
    /// trace event while pack/unpack leftovers vanish.
    fn remove_redundant_nops(&mut self) {
        let n = self.co.instructions.len();
        if n == 0 {
            return;
        }
        // Block leaders: jump targets and exception-handler entries. A
        // NOP that starts a block must not borrow its predecessor's
        // line (that predecessor belongs to another path), and a
        // neighbour that starts a block is no in-block neighbour.
        let mut leader = vec![false; n + 1];
        for i in 0..n {
            let ins = self.co.instructions[i];
            let from = i as u32 + 1;
            match ins.op {
                OpCode::JumpForward
                | OpCode::PopJumpIfFalse
                | OpCode::PopJumpIfTrue
                | OpCode::PopJumpIfNone
                | OpCode::PopJumpIfNotNone
                | OpCode::ForIter
                | OpCode::Send => leader[((from + ins.arg) as usize).min(n)] = true,
                OpCode::JumpBackward => {
                    leader[(from.saturating_sub(ins.arg) as usize).min(n)] = true;
                }
                _ => {}
            }
        }
        for h in &self.co.exception_table {
            leader[(h.handler as usize).min(n)] = true;
        }
        let ends_block = |op: OpCode| {
            matches!(
                op,
                OpCode::JumpForward
                    | OpCode::JumpBackward
                    | OpCode::PopJumpIfFalse
                    | OpCode::PopJumpIfTrue
                    | OpCode::PopJumpIfNone
                    | OpCode::PopJumpIfNotNone
                    | OpCode::ForIter
                    | OpCode::Send
                    | OpCode::ReturnValue
                    | OpCode::RaiseVarargs
                    | OpCode::Reraise
            )
        };
        // Sequential like CPython's per-block dest/src walk: the
        // "previous" check reads the last *kept* instruction, so a run
        // of same-line NOPs collapses to one survivor rather than each
        // covering the other into mutual annihilation (`if 1:` must
        // keep its located NOP — test_sys_settrace test_02_arigo2).
        let mut keep = vec![true; n];
        let mut prev_kept: Option<usize> = None;
        for i in 0..n {
            if leader[i] {
                prev_kept = None;
            }
            if self.co.instructions[i].op != OpCode::Nop {
                prev_kept = Some(i);
                continue;
            }
            let line = self.co.linetable[i];
            if line == 0 {
                keep[i] = false;
                continue;
            }
            let prev_covers = prev_kept.is_some_and(|p| {
                !ends_block(self.co.instructions[p].op) && self.co.linetable[p] == line
            });
            let mut next_covers = i + 1 < n && !leader[i + 1] && self.co.linetable[i + 1] == line;
            // CPython's `propagate_line_numbers` runs before NOP
            // removal: a located NOP flows its location onto following
            // NO_LOCATION instructions in the same block, after which
            // the NOP is redundant (a `pass`-only except clause puts
            // its line on the handler-exit POP_EXCEPT/RETURN run —
            // the except* burn's outer `except KeyError: pass` shape).
            if !next_covers && i + 1 < n && !leader[i + 1] && self.co.linetable[i + 1] == 0 {
                let mut k = i + 1;
                while k < n && !leader[k] && self.co.linetable[k] == 0 {
                    self.co.linetable[k] = line;
                    self.co.coltable[k] = self.co.coltable[i];
                    if ends_block(self.co.instructions[k].op) {
                        break;
                    }
                    k += 1;
                }
                next_covers = true;
            }
            if prev_covers || next_covers {
                keep[i] = false;
            } else {
                prev_kept = Some(i);
            }
        }
        self.compact_stream(&keep);
    }

    /// CPython's `remove_redundant_jumps`: an unconditional jump whose
    /// target is the very next instruction is a NOP. Returns whether
    /// anything changed (the caller loops with NOP removal to a
    /// fixpoint, as `remove_redundant_nops_and_jumps` does).
    fn remove_jumps_to_next(&mut self) -> bool {
        let mut changed = false;
        for ins in self.co.instructions.iter_mut() {
            if matches!(ins.op, OpCode::JumpForward | OpCode::JumpBackward) && ins.arg == 0 {
                *ins = Instruction {
                    op: OpCode::Nop,
                    arg: 0,
                };
                changed = true;
            }
        }
        changed
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

    /// Emit with CPython's `NO_LOCATION`: the instruction never fires a
    /// `'line'` trace event and shows as `--` in `dis` output.
    fn emit_no_line(&mut self, op: OpCode, arg: u32) -> u32 {
        let saved = self.line_pinned;
        self.line_pinned = Some(0);
        let off = self.emit(op, arg);
        self.line_pinned = saved;
        off
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

    /// Emit CPython 3.13's function-construction shape: a bare
    /// `MAKE_FUNCTION` followed by one `SET_FUNCTION_ATTRIBUTE` per
    /// present attribute, consumed top-down (closure 0x08 sits nearest
    /// the top of the stack, defaults 0x01 deepest — the reverse of the
    /// push order).
    fn emit_make_function(&mut self, flags: u32) {
        self.emit(OpCode::MakeFunction, 0);
        for bit in [0x08u32, 0x04, 0x02, 0x01] {
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
            OpCode::JumpForward
            | OpCode::PopJumpIfFalse
            | OpCode::PopJumpIfTrue
            | OpCode::PopJumpIfNone
            | OpCode::PopJumpIfNotNone
            | OpCode::ForIter
            | OpCode::Send => {
                ins.arg = target.saturating_sub(from);
            }
            OpCode::JumpBackward => {
                ins.arg = from.saturating_sub(target);
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
        // CPython's symtable marks a module block containing any annotated
        // statement (at the block's own level) and the compiler emits
        // SETUP_ANNOTATIONS as its first real instruction — code preceding
        // the first annotation can already read `__annotations__`
        // (ann_module.py does `__annotations__[1] = 2` at module top).
        if block_has_annotations(&module.body) {
            // CPython locates SETUP_ANNOTATIONS on the module's first
            // statement (compiler_body seeds `loc` from stmt 0 before
            // emitting it — dis_annot_stmt_str asserts the line).
            if let Some(first) = module.body.first() {
                self.set_line_from(first.span.start.0);
                self.set_span(first.span);
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
        self.explicit_globals = explicit;
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
        for n in assigned {
            self.bindings.entry(n).or_insert(Binding::Local);
        }
        // Names referenced by directly-emitted bytecode in this scope.
        let mut reads = HashSet::new();
        for s in body {
            collect_reads_stmt(s, &mut reads);
        }
        // Names needed by ANY nested scope (lambda, comp, def). They
        // also flow through us: if an inner scope reads `threshold`
        // and we don't bind it, we must surface it as a free var here
        // so our enclosing scope can hand us a cell to forward.
        let mut needed_in_inner: HashSet<String> = HashSet::new();
        for s in body {
            collect_inner_free(s, &self.bindings, &mut needed_in_inner);
        }
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
    }

    // ---------- statements ----------

    fn compile_stmt(&mut self, stmt: &Stmt) -> Result<(), CompileError> {
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
            StmtKind::TypeAlias { .. } => {
                // Normally rewritten at the compiler entry
                // (`lower_type_aliases`); handled here too so a caller
                // compiling a raw parse AST still works.
                let lowered = weavepy_parser::lower_type_alias_stmt(stmt);
                self.compile_stmt(&lowered)?;
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
                let (cond, invert) = strip_not_chain(test);
                self.compile_expr(cond)?;
                if !expr_is_bool(cond) {
                    self.set_span(test.span);
                    self.emit(OpCode::ToBool, 0);
                }
                // Location split mirrors CPython compiler_assert: the
                // branch carries the test's span, LOAD_ASSERTION_ERROR
                // and the msg CALL carry the whole statement's, and
                // RAISE_VARARGS carries the test's again so PEP-657
                // carets underline the failed condition.
                self.set_span(test.span);
                let skip = self.emit(
                    if invert {
                        OpCode::PopJumpIfFalse
                    } else {
                        OpCode::PopJumpIfTrue
                    },
                    0,
                );
                // The *builtin* AssertionError, immune to shadowing
                // (CPython LOAD_ASSERTION_ERROR, bpo-34880).
                self.set_span(stmt_span);
                self.emit(OpCode::LoadAssertionError, 0);
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
                self.patch_jump(skip, end);
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
                        self.compile_expr(obj)?;
                        self.compile_expr(slice)?;
                        let saved = self.current_span;
                        self.set_span(target.span);
                        self.emit(OpCode::CopyTop, 2);
                        self.emit(OpCode::CopyTop, 2);
                        self.emit(OpCode::BinarySubscr, 0);
                        self.current_span = saved;
                        self.compile_expr(value)?;
                        self.emit(OpCode::BinaryOp, bin_arg);
                        let saved = self.current_span;
                        self.set_span(target.span);
                        self.emit(OpCode::Swap, 3);
                        self.emit(OpCode::Swap, 2);
                        self.emit(OpCode::StoreSubscr, 0);
                        self.current_span = saved;
                    }
                    _ => {
                        self.compile_load_target(target)?;
                        self.compile_expr(value)?;
                        self.emit(OpCode::BinaryOp, bin_arg);
                        self.compile_assign(target)?;
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
                        self.compile_annotation_record(name, annotation)?;
                    }
                }
                // CPython's compiler_annassign side-effect evaluation for
                // targets that don't record an annotation: an unassigned
                // attribute/subscript target evaluates its subexpressions
                // (check_ann_expr / check_ann_subscr), and — outside PEP
                // 563 mode, at module/class scope only — a non-simple
                // statement evaluates the annotation itself and discards
                // it (check_annotation; dis_annot_stmt_str grades the
                // LOAD_NAME/POP_TOP tail).
                if value.is_none() {
                    match &target.kind {
                        ExprKind::Attribute { value: obj, .. } => {
                            self.compile_expr(obj)?;
                            self.emit(OpCode::PopTop, 0);
                        }
                        ExprKind::Subscript { value: obj, slice } => {
                            self.compile_expr(obj)?;
                            self.emit(OpCode::PopTop, 0);
                            self.compile_expr(slice)?;
                            self.emit(OpCode::PopTop, 0);
                        }
                        _ => {}
                    }
                }
                if !*simple
                    && !self.future_annotations
                    && matches!(self.code_kind, CodeKind::Class | CodeKind::Module)
                {
                    self.compile_expr(annotation)?;
                    self.emit(OpCode::PopTop, 0);
                }
            }
            StmtKind::If { test, body, orelse } => {
                let (cond, invert) = strip_not_chain(test);
                self.compile_expr(cond)?;
                if !expr_is_bool(cond) {
                    self.emit(OpCode::ToBool, 0);
                }
                let jump_else = self.emit(
                    if invert {
                        OpCode::PopJumpIfTrue
                    } else {
                        OpCode::PopJumpIfFalse
                    },
                    0,
                );
                for s in body {
                    self.compile_stmt(s)?;
                }
                if orelse.is_empty() {
                    let target = self.next_offset();
                    self.patch_jump(jump_else, target);
                } else {
                    // Structural join jump: NO_LOCATION in CPython.
                    let jump_end = self.emit(OpCode::JumpForward, 0);
                    self.synthetic_jumps.insert(jump_end);
                    let else_target = self.next_offset();
                    self.patch_jump(jump_else, else_target);
                    for s in orelse {
                        self.compile_stmt(s)?;
                    }
                    let end_target = self.next_offset();
                    self.patch_jump(jump_end, end_target);
                }
            }
            StmtKind::While { test, body, orelse } => {
                // CPython 3.13 *rotates* while loops: the condition is
                // compiled once at the top (loop entry) and duplicated
                // after the body, so a finishing iteration exits from
                // the bottom test without ever taking the backward
                // jump. This shape is what gives CPython its `line`
                // event cadence — the test line fires once on entry
                // plus once per *completed* iteration (RFC 0051 WS4);
                // a top-test-only loop overcounts by one.
                //
                // `while 1:` / `while True:` (a constant-true test) gets
                // CPython's other shape: no test at all — a NOP carrying
                // the `while` line (so the header fires one `line` event
                // on entry) and a back edge pinned to that line (so each
                // iteration re-fires it before re-entering the body).
                let const_true = match &test.kind {
                    ExprKind::Constant(weavepy_parser::ast::Constant::Bool(b)) => *b,
                    ExprKind::Constant(weavepy_parser::ast::Constant::Int(n)) => *n != 0,
                    _ => false,
                };
                let while_line = self.current_line;
                let while_span = self.current_span;
                let loop_start = self.next_offset();
                let mut jump_exit_top = None;
                if const_true {
                    self.set_span(test.span);
                    self.emit(OpCode::Nop, 0);
                } else {
                    let (cond, invert) = strip_not_chain(test);
                    self.compile_expr(cond)?;
                    self.set_span(test.span);
                    if !expr_is_bool(cond) {
                        self.emit(OpCode::ToBool, 0);
                    }
                    jump_exit_top = Some(self.emit(
                        if invert {
                            OpCode::PopJumpIfTrue
                        } else {
                            OpCode::PopJumpIfFalse
                        },
                        0,
                    ));
                }
                let body_start = self.next_offset();
                self.loop_stack.push(LoopFrame {
                    // `continue` re-runs the top copy of the test: same
                    // semantics as CPython's jump to the bottom copy
                    // (evaluate condition, then exit or re-enter body).
                    continue_target: if const_true { body_start } else { loop_start },
                    break_sites: Vec::new(),
                    is_for_loop: false,
                    handler_depth_at_entry: self.handler_depth,
                    exc_on_stack_at_entry: self.exc_on_stack,
                    pending_retvals_at_entry: self.pending_retvals,
                });
                for s in body {
                    self.compile_stmt(s)?;
                }
                let mut jump_exit_bottom = None;
                if const_true {
                    // Back edge on the `while` line: landing in the body
                    // (different line) fires the header line each pass.
                    self.current_line = while_line;
                    self.current_span = while_span;
                    self.set_span(test.span);
                } else {
                    let (cond, invert) = strip_not_chain(test);
                    self.compile_expr(cond)?;
                    self.set_span(test.span);
                    if !expr_is_bool(cond) {
                        self.emit(OpCode::ToBool, 0);
                    }
                    jump_exit_bottom = Some(self.emit(
                        if invert {
                            OpCode::PopJumpIfTrue
                        } else {
                            OpCode::PopJumpIfFalse
                        },
                        0,
                    ));
                }
                let back = self.emit(OpCode::JumpBackward, 0);
                self.synthetic_jumps.insert(back);
                self.patch_jump(back, body_start);
                let frame = self.loop_stack.pop().expect("loop frame");
                // Natural exit: condition went false. Run the
                // `orelse` block.
                let orelse_target = self.next_offset();
                if let Some(site) = jump_exit_top {
                    self.patch_jump(site, orelse_target);
                }
                if let Some(site) = jump_exit_bottom {
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
                self.compile_assign(target)?;
                self.loop_stack.push(LoopFrame {
                    continue_target: loop_top,
                    break_sites: Vec::new(),
                    is_for_loop: true,
                    handler_depth_at_entry: self.handler_depth,
                    exc_on_stack_at_entry: self.exc_on_stack,
                    pending_retvals_at_entry: self.pending_retvals,
                });
                for s in body {
                    self.compile_stmt(s)?;
                }
                let back = self.emit(OpCode::JumpBackward, 0);
                self.synthetic_jumps.insert(back);
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
                // CPython 3.13's loop-exit pair: END_FOR then POP_TOP
                // (the exhausted FOR_ITER pops the iterator and jumps
                // *past* both at runtime — they exist as the jump
                // target and for instrumentation; test_dis asserts the
                // shape). The VM skips them the same way.
                self.emit(OpCode::EndFor, 0);
                self.emit(OpCode::PopTop, 0);
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
                if !const_ret {
                    match value {
                        Some(v) => self.compile_expr(v)?,
                        None => unreachable!("const_ret covers None"),
                    }
                } else if !self.finally_stack.is_empty() {
                    self.emit(OpCode::Nop, 0);
                }
                // Inline every pending finally clause from innermost
                // outward so each runs before we leave the function.
                // A non-constant return value stays *on the operand
                // stack* while the inlined bodies run (they are
                // stack-neutral), exactly as CPython's duplicated
                // finally shape does — a synthetic `.retvalN` local
                // would leak into co_varnames, which test_dis grades
                // verbatim. If a body raises, the unwinder's depth
                // truncation discards the pending value, matching
                // CPython.
                if !self.finally_stack.is_empty() {
                    let frames = std::mem::take(&mut self.finally_stack);
                    let mut compiled: Result<(), CompileError> = Ok(());
                    let mut hole_starts: Vec<(u32, u32)> = Vec::new();
                    // The value being returned rides the operand stack
                    // under the inlined finally bodies; a `break`/
                    // `continue` inside one of them abandons the return
                    // and must pop it (see `pending_retvals`).
                    if !const_ret {
                        self.pending_retvals += 1;
                    }
                    let mut cur_exc = self.exc_on_stack;
                    let mut cur_loops = self.loop_stack.len();
                    for (i, frame) in frames.iter().enumerate().rev() {
                        // Exception regions ([prev, exc] of an
                        // exception-path finally copy) entered *after*
                        // this frame was pushed sit above its stack
                        // state. Frames that consume specific slots — a
                        // with's on-stack `__exit__`, a handler-exit's
                        // POP_EXCEPT — need them drained first
                        // (CPython's FINALLY_END unwind), preserving the
                        // pending return value on top.
                        let positional = frame.pop_except_after
                            || matches!(
                                frame.kind,
                                FinallyKind::WithExit { .. } | FinallyKind::AsyncWithExit { .. }
                            );
                        if positional {
                            // Everything pushed *after* this frame sits above
                            // the slot its inline consumes and must be
                            // drained first, in recency order: exception
                            // regions ([prev, exc]) *and* `for`-loop
                            // iterators (CPython's FOR_LOOP fblock unwind
                            // pops the iterator — without it a `return` from
                            // a `for` inside a `with` called the *iterator*
                            // as if it were `__exit__`; pdb.find_function's
                            // `with fp: for … return` shape hit this).
                            // Recency between the two kinds is recovered
                            // from each loop's `exc_on_stack_at_entry`.
                            while cur_exc > frame.exc_at_push
                                || cur_loops > frame.loop_depth_at_push
                            {
                                let loop_is_newer = cur_loops > frame.loop_depth_at_push
                                    && (cur_exc == frame.exc_at_push
                                        || self.loop_stack[cur_loops - 1].exc_on_stack_at_entry
                                            >= cur_exc);
                                if loop_is_newer {
                                    cur_loops -= 1;
                                    if self.loop_stack[cur_loops].is_for_loop {
                                        if const_ret {
                                            self.emit(OpCode::PopTop, 0);
                                        } else {
                                            // [iter, rv] → [rv]
                                            self.emit(OpCode::Swap, 2);
                                            self.emit(OpCode::PopTop, 0);
                                        }
                                    }
                                } else if const_ret {
                                    self.emit(OpCode::PopTop, 0);
                                    self.emit(OpCode::PopExcept, 0);
                                    cur_exc -= 1;
                                } else {
                                    // [prev, exc, rv] → [rv]
                                    self.emit(OpCode::Swap, 2);
                                    self.emit(OpCode::PopTop, 0);
                                    self.emit(OpCode::Swap, 2);
                                    self.emit(OpCode::PopExcept, 0);
                                    cur_exc -= 1;
                                }
                            }
                        }
                        // While compiling this finally body, hide it
                        // from the stack so nested `return`s inside the
                        // body don't recurse infinitely.
                        let saved_finally: Vec<FinallyFrame> =
                            frames.iter().take(i).map(clone_finally_frame).collect();
                        self.finally_stack = saved_finally;
                        let inline_start = self.next_offset();
                        if let Err(e) = self.emit_finally_frame(frame, !const_ret) {
                            compiled = Err(e);
                        }
                        // Returning out of an `except` handler body:
                        // discard its handled-exception state right after
                        // the unbind ran (CPython's return-path
                        // `e = None; del e; POP_EXCEPT` order), so the
                        // pop's prompt-reap cascade can free the handled
                        // exception — and everything its traceback pins.
                        // The saved previous exception (PUSH_EXC_INFO's
                        // stack slot) sits *under* a pending return
                        // value; CPython swaps them (only when a value is
                        // preserved) so POP_EXCEPT consumes the right
                        // slot.
                        if compiled.is_ok() && frame.pop_except_after {
                            if !const_ret {
                                self.emit(OpCode::Swap, 2);
                            }
                            self.emit(OpCode::PopExcept, 0);
                        }
                        hole_starts.push((frame.id, inline_start));
                        self.finally_stack.clear();
                        if compiled.is_err() {
                            break;
                        }
                    }
                    if !const_ret {
                        self.pending_retvals -= 1;
                    }
                    self.finally_stack = frames;
                    compiled?;
                    // The constant value materialises *after* the
                    // unwind, adjacent to RETURN_VALUE and at the
                    // current (finally-end) location, so the encoder's
                    // RETURN_CONST fusion applies — same-line check
                    // included (arg 1 marks a codegen-origin constant
                    // return as fusable).
                    if const_ret {
                        let c = match value {
                            Some(Expr {
                                kind: ExprKind::Constant(c),
                                ..
                            }) => c.clone().into(),
                            None => Constant::None,
                            Some(_) => unreachable!("const_ret guards the kind"),
                        };
                        let idx = self.co.intern_constant(c);
                        self.emit(OpCode::LoadConst, idx);
                    }
                    self.emit(OpCode::ReturnValue, u32::from(const_ret));
                    // The inlined finally bodies ran here for the return.
                    // Exclude each frame's inline — through the
                    // RETURN_VALUE itself, which CPython leaves uncovered
                    // (test_dis's try/finally exception table) — from its
                    // owning try's exception coverage, so a `raise` inside
                    // a return-path finally propagates outward instead of
                    // re-running it.
                    let inline_end = self.next_offset();
                    for (id, start) in hole_starts {
                        self.finally_holes.push((id, start, inline_end));
                    }
                    return Ok(());
                }
                // No finally clauses: materialise a constant value here
                // (the non-constant case compiled it above). arg 1 marks
                // a codegen-origin constant return as RETURN_CONST-
                // fusable on the wire (CPython emits RETURN_CONST from
                // codegen only; the flowgraph never re-fuses an
                // optimizer-produced LOAD_CONST + RETURN_VALUE pair).
                if const_ret {
                    match value {
                        Some(v) => self.compile_expr(v)?,
                        None => {
                            let idx = self.co.intern_constant(Constant::None);
                            self.emit(OpCode::LoadConst, idx);
                        }
                    }
                }
                self.emit(OpCode::ReturnValue, u32::from(const_ret));
            }
            StmtKind::Break => {
                // CPython's codegen_break emits a located NOP so tracing
                // reports the `break` line before any inlined `finally`
                // body runs (test_sys_settrace test_break_through_finally).
                self.emit(OpCode::Nop, 0);
                let frame_top = self
                    .loop_stack
                    .last()
                    .ok_or_else(|| CompileError::spanned("'break' outside loop", stmt.span))?;
                let is_for = frame_top.is_for_loop;
                let tgt_exc = frame_top.exc_on_stack_at_entry;
                let tgt_handler = frame_top.handler_depth_at_entry;
                let tgt_rv = frame_top.pending_retvals_at_entry;
                // Inline the `finally` clauses between us and the loop
                // (innermost-out), interleaved with pops for exception
                // regions / handler bodies / pending return values in
                // recency order.
                self.unwind_for_loop_exit(tgt_exc, tgt_handler, tgt_rv)?;
                if is_for {
                    self.emit(OpCode::PopTop, 0);
                }
                // Route through `emit` so the line/column side-tables stay
                // length-aligned with the instruction stream.
                let site = self.emit(OpCode::JumpForward, 0);
                self.loop_stack
                    .last_mut()
                    .expect("loop frame")
                    .break_sites
                    .push(site);
            }
            StmtKind::Continue => {
                // Located NOP, mirroring codegen_continue (see Break above).
                self.emit(OpCode::Nop, 0);
                let frame_top = self.loop_stack.last().ok_or_else(|| {
                    CompileError::spanned("'continue' not properly in loop", stmt.span)
                })?;
                let target = frame_top.continue_target;
                let tgt_exc = frame_top.exc_on_stack_at_entry;
                let tgt_handler = frame_top.handler_depth_at_entry;
                let tgt_rv = frame_top.pending_retvals_at_entry;
                // See Break: recency-ordered unwind.
                self.unwind_for_loop_exit(tgt_exc, tgt_handler, tgt_rv)?;
                let site = self.emit(OpCode::JumpBackward, 0);
                self.patch_jump(site, target);
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
                    let mut parts = alias.name.split('.');
                    let _ = parts.next();
                    for part in parts {
                        let idx = self.co.intern_name(part);
                        self.emit(OpCode::ImportFrom, idx);
                        self.emit(OpCode::Swap, 2);
                        self.emit(OpCode::PopTop, 0);
                    }
                    self.emit_store_name(asname);
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
                self.compile_expr(guard)?;
                self.set_span(guard.span);
                let g = self.emit(OpCode::PopJumpIfFalse, 0);
                pc.fail_pops[0].push(g);
                self.set_line_from(case.pattern.span.start.0);
                self.set_span(case.pattern.span);
            }
            // Success! Pop the subject off, we're done with it:
            if i != ncompiled - 1 {
                self.emit(OpCode::PopTop, 0);
            }
            for s in &case.body {
                self.compile_stmt(s)?;
            }
            // CPython emits this jump with NO_LOCATION, but its
            // flowgraph pass (`propagate_line_numbers`) then stamps it
            // with the preceding instruction's location — which is what
            // we have right now (the body's last statement). Same line
            // ⇒ no spurious trace event, and `dis` sees a located jump
            // (gh-123048 / test_jump_threading).
            let j = self.emit(OpCode::JumpForward, 0);
            self.synthetic_jumps.insert(j);
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
                self.compile_expr(guard)?;
                self.set_span(guard.span);
                end_jumps.push(self.emit(OpCode::PopJumpIfFalse, 0));
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
            // Our DICT_UPDATE takes [dict, other] adjacent, so the walk
            // differs slightly from CPython's SWAP 3 dance:
            self.emit(OpCode::Swap, 2); //          [keys, subject]
            self.emit(OpCode::BuildMap, 0); //      [keys, subject, {}]
            self.emit(OpCode::Swap, 2); //          [keys, {}, subject]
            self.emit(OpCode::DictUpdate, 0); //    [keys, copy]
            self.emit(OpCode::Swap, 2); //          [copy, keys]
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
    /// by reproducing CPython's *hidden scope* (symtable
    /// `TypeParametersBlock`, shown in tracebacks as
    /// `<generic parameters of X>`): the statement lowers to an
    /// immediately-invoked synthetic function that binds the type
    /// parameters as ordinary locals, defines the `def`/`class` inside
    /// it (so annotations, bases, and nested bodies close over the
    /// parameters), stamps `__type_params__`, and returns the result:
    ///
    /// ```text
    /// @dec
    /// def f[T](a: T = d()) -> T: return T
    /// # lowers to
    /// def <generic parameters of f>(.defaults0):
    ///     T = __weavepy_typevar__('T')
    ///     def f(a: T = .defaults0) -> T: return T
    ///     f.__type_params__ = (T,)
    ///     return f
    /// f = dec(<generic parameters of f>(d()))
    /// ```
    ///
    /// Mirroring CPython:
    /// - decorators and *default values* evaluate in the enclosing
    ///   scope (defaults are hoisted into hidden-scope parameters);
    /// - a generic class gets an implicit trailing
    ///   `Generic[T, …]` base (`INTRINSIC_SUBSCRIPT_GENERIC`) and its
    ///   `__type_params__` stored in the class namespace before the
    ///   body runs;
    /// - a *class's* type parameters are private-name mangled against
    ///   the class's own name (`class Foo[__T]` binds `_Foo__T`), so
    ///   references from the (independently mangled) class body
    ///   resolve; a *function's* were already mangled against the
    ///   enclosing class by [`mangle::mangle_class_body`];
    /// - the hidden scope never leaks: nothing else can observe the
    ///   parameter bindings, and qualnames skip it (see
    ///   [`Self::compute_child_qualname`]).
    fn compile_generic_def(&mut self, stmt: &Stmt) -> Result<(), CompileError> {
        let span = stmt.span;
        let name_expr = |n: &str| Expr {
            kind: ExprKind::Name(n.to_owned()),
            span,
        };

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
            } => (name.as_str(), decorator_list, type_params),
            _ => unreachable!("compile_generic_def requires a def/class statement"),
        };
        let is_class = matches!(stmt.kind, StmtKind::ClassDef { .. });
        let display = self.display_name(name).to_owned();
        let hidden_name = format!("<generic parameters of {display}>");

        // The names the parameters bind to inside the hidden scope.
        let own_params: HashSet<String> = type_params.iter().map(|tp| tp.name.clone()).collect();
        let binding_names: Vec<String> = type_params
            .iter()
            .map(|tp| {
                if is_class {
                    crate::mangle::mangle_ident(&display, &tp.name)
                } else {
                    tp.name.clone()
                }
            })
            .collect();

        let mut hidden_body: Vec<Stmt> = Vec::new();

        // Prologue: `T = __weavepy_typevar__('T')` (with bound /
        // constraints thunks per the parameter's syntax). Later
        // parameters' bounds and defaults close over the earlier
        // bindings (`class C[T, U: T]`). A PEP 696 default attaches
        // *after* the binding is live — its thunk may reference the
        // parameter itself (`def f[T = [T for T in [T]]](): ...`).
        for (tp, binding) in type_params.iter().zip(&binding_names) {
            let mut tp = tp.clone();
            if is_class {
                if let TypeParamKind::TypeVar { bound: Some(b) } = &mut tp.kind {
                    crate::mangle::mangle_only_in_expr(&display, &own_params, b);
                }
                if let Some(d) = &mut tp.default {
                    crate::mangle::mangle_only_in_expr(&display, &own_params, d);
                }
            }
            hidden_body.push(Stmt {
                kind: StmtKind::Assign {
                    targets: vec![name_expr(binding)],
                    value: tp.constructor_expr(),
                },
                span,
            });
            if tp.default.is_some() {
                hidden_body.push(Stmt {
                    kind: StmtKind::Expr(tp.apply_default_expr(name_expr(binding))),
                    span,
                });
            }
        }

        let params_tuple = Expr {
            kind: ExprKind::Tuple(binding_names.iter().map(|n| name_expr(n)).collect()),
            span,
        };
        // Classes read their `__type_params__` through a hidden
        // `.type_params` binding (CPython does exactly this): the
        // class body can't name the parameters directly because a
        // same-named class-level assignment (`class C[T]: T = ...`)
        // must not shadow the tuple's contents.
        if is_class {
            hidden_body.push(Stmt {
                kind: StmtKind::Assign {
                    targets: vec![name_expr(".type_params")],
                    value: params_tuple.clone(),
                },
                span,
            });
        }

        // Default values are hoisted out: each becomes a parameter of
        // the hidden function, passed at the call below, so they
        // evaluate in the enclosing scope exactly like CPython (a
        // default must not see the type parameters).
        let mut hoisted: Vec<Expr> = Vec::new();
        let mut hidden_params: Vec<String> = Vec::new();

        // The original statement, stripped of decorators (applied
        // outside) and type parameters (bound above).
        let inner_stmt = match &stmt.kind {
            StmtKind::FunctionDef {
                name,
                args,
                body,
                returns,
                ..
            }
            | StmtKind::AsyncFunctionDef {
                name,
                args,
                body,
                returns,
                ..
            } => {
                let mut args = args.clone();
                for d in args
                    .defaults
                    .iter_mut()
                    .chain(args.kw_defaults.iter_mut().flatten())
                {
                    let pname = format!(".defaults{}", hoisted.len());
                    let replacement = Expr {
                        kind: ExprKind::Name(pname.clone()),
                        span: d.span,
                    };
                    hoisted.push(std::mem::replace(d, replacement));
                    hidden_params.push(pname);
                }
                let kind = if matches!(stmt.kind, StmtKind::AsyncFunctionDef { .. }) {
                    StmtKind::AsyncFunctionDef {
                        name: name.clone(),
                        args,
                        body: body.clone(),
                        decorator_list: Vec::new(),
                        type_params: Vec::new(),
                        returns: returns.clone(),
                    }
                } else {
                    StmtKind::FunctionDef {
                        name: name.clone(),
                        args,
                        body: body.clone(),
                        decorator_list: Vec::new(),
                        type_params: Vec::new(),
                        returns: returns.clone(),
                    }
                };
                Stmt { kind, span }
            }
            StmtKind::ClassDef {
                name,
                bases,
                keywords,
                body,
                ..
            } => {
                // Bases and keywords evaluate inside the hidden scope;
                // references to the class's own (renamed) parameters
                // must follow.
                let mut bases = bases.clone();
                let mut keywords = keywords.clone();
                for b in &mut bases {
                    crate::mangle::mangle_only_in_expr(&display, &own_params, b);
                }
                for k in &mut keywords {
                    crate::mangle::mangle_only_in_expr(&display, &own_params, &mut k.value);
                }
                // Implicit trailing `Generic[T, …]` base (CPython's
                // `CALL_INTRINSIC_1 INTRINSIC_SUBSCRIPT_GENERIC`).
                // `typing._generic_init_subclass` rejects an explicit
                // duplicate, so the append is unconditional.
                bases.push(Expr {
                    kind: ExprKind::Call {
                        func: Box::new(name_expr("__weavepy_generic_base__")),
                        args: binding_names.iter().map(|n| name_expr(n)).collect(),
                        keywords: Vec::new(),
                    },
                    span,
                });
                // `__type_params__` goes into the class namespace
                // *before* the body runs, read through the hidden
                // `.type_params` binding (CPython's `STORE_NAME
                // __type_params__` from the `.type_params` cell at the
                // top of the class body) so a class-level assignment
                // to a parameter's name can't shadow it. Placed after
                // any docstring so `__doc__` detection still sees the
                // leading string.
                let mut body = body.clone();
                let insert_at = usize::from(first_stmt_docstring(&body).is_some());
                body.insert(
                    insert_at,
                    Stmt {
                        kind: StmtKind::Assign {
                            targets: vec![name_expr("__type_params__")],
                            value: name_expr(".type_params"),
                        },
                        span,
                    },
                );
                Stmt {
                    kind: StmtKind::ClassDef {
                        name: name.clone(),
                        bases,
                        keywords,
                        body,
                        decorator_list: Vec::new(),
                        type_params: Vec::new(),
                    },
                    span,
                }
            }
            _ => unreachable!(),
        };
        hidden_body.push(inner_stmt);

        // Functions get `__type_params__` stamped after creation
        // (CPython's `INTRINSIC_SET_FUNCTION_TYPE_PARAMS`), before
        // decorators see the object.
        if !is_class {
            hidden_body.push(Stmt {
                kind: StmtKind::Assign {
                    targets: vec![Expr {
                        kind: ExprKind::Attribute {
                            value: Box::new(name_expr(name)),
                            attr: "__type_params__".to_owned(),
                        },
                        span,
                    }],
                    value: params_tuple,
                },
                span,
            });
        }
        hidden_body.push(Stmt {
            kind: StmtKind::Return(Some(name_expr(name))),
            span,
        });

        // Emit: decorators, then build-and-call the hidden function,
        // then apply decorators to its return value, then bind.
        for d in decorator_list {
            self.compile_expr(d)?;
        }
        let hidden_args = AstArguments {
            args: hidden_params
                .iter()
                .map(|n| AstArg {
                    name: n.clone(),
                    annotation: None,
                    span,
                })
                .collect(),
            ..AstArguments::default()
        };
        // Qualnames skip the hidden scope (CPython: `outer.<locals>.f`,
        // not `outer.<locals>.<generic parameters of f>.<locals>.f`).
        self.pending_pep695_qualname =
            Some((display.clone(), self.compute_child_qualname(&display)));
        // A generic statement in a class body evaluates its whole
        // header (bases, keywords, annotations, bound/default thunks)
        // in a scope that can see the class namespace via
        // `__classdict__` (CPython `ste_can_see_class_scope`).
        self.pending_lazy_class_ctx = self.make_lazy_ctx();
        self.build_function_object_inner(&hidden_name, &hidden_args, &hidden_body, None, false)?;
        // The hidden function is a NULL-style callable (CPython:
        // `MAKE_FUNCTION; PUSH_NULL; CALL 0`).
        self.emit(OpCode::PushNull, 0);
        for e in &hoisted {
            self.compile_expr(e)?;
        }
        self.emit(OpCode::Call, hoisted.len() as u32);
        for d in decorator_list.iter().rev() {
            let saved = self.current_span;
            self.set_span(d.span);
            // The decorated value rides the self slot (CPython
            // compiler_apply_decorators: `CALL 0`, no PUSH_NULL).
            self.emit(OpCode::CallSelf, 1);
            self.current_span = saved;
        }
        self.compile_assign(&name_expr(name))
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

    fn build_function_object_inner(
        &mut self,
        name: &str,
        args: &AstArguments,
        body: &[Stmt],
        returns: Option<&Expr>,
        is_async: bool,
    ) -> Result<(), CompileError> {
        self.build_function_object_full(name, args, body, returns, is_async, None)
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
        // PEP 695: a hidden `<generic parameters of X>` scope being
        // built takes the pending qualname pair; the def/class it
        // wraps then reads it back via `compute_child_qualname`.
        inner.pep695_qualname = self.pending_pep695_qualname.take();
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
        let parent_forwards_class = self.inside_class_body
            || matches!(
                self.bindings.get("__class__"),
                Some(Binding::Free | Binding::Cell)
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
        inner.analyze_scope_function(&param_names, body, &[&self.bindings]);
        for free in &inner.free_order {
            if matches!(self.bindings.get(free), Some(Binding::Local)) {
                self.bindings.insert(free.clone(), Binding::Cell);
                if !self.co.cellvars.contains(free) {
                    self.co.cellvars.push(free.clone());
                }
            }
        }
        let has_yield = body_is_generator(body);
        if is_async {
            // PEP 492: `async def` with `yield` is an async generator;
            // otherwise it's a coroutine. Both shapes share the
            // generator-style suspended-frame infrastructure.
            inner.co.is_async_generator = has_yield;
            inner.co.is_coroutine = !has_yield;
            inner.emit(OpCode::ReturnGenerator, 0);
            inner.emit(OpCode::PopTop, 0);
        } else {
            inner.co.is_generator = has_yield;
            if has_yield {
                inner.emit(OpCode::ReturnGenerator, 0);
                inner.emit(OpCode::PopTop, 0);
            }
        }
        inner.emit_entry_resume();
        // CPython reserves `co_consts[0]` for the function docstring (or
        // `None`). Mirror that here so `__doc__` is *only* the leading
        // bare string-literal statement — never an unrelated string
        // constant that merely happens to be interned first (e.g. the
        // RHS of `x = "s"` as the first statement). `intern_constant`
        // dedups, so a real docstring shares this slot with its own
        // `LoadConst`, and a `None` slot is reused by the implicit
        // `return None`.
        let doc_slot = match first_stmt_docstring(body) {
            // `-OO` (optimize >= 2) strips docstrings; the slot decays
            // to the shared `None` constant like CPython's.
            Some(doc) if self.params.optimize < 2 => Constant::Str(clean_docstring(doc)),
            _ => Constant::None,
        };
        inner.co.intern_constant(doc_slot);
        // The docstring statement itself generates *no* code in a
        // function body (CPython consumes it into `co_consts[0]`); a
        // NOP here would fire a spurious `'line'` trace event on the
        // docstring line (test_trace test_issue9936).
        let stmts = if first_stmt_docstring(body).is_some() {
            &body[1..]
        } else {
            body
        };
        for s in stmts {
            inner.compile_stmt(s)?;
        }
        let inner_code = inner.finish();
        let inner_freevars = inner_code.freevars.clone();

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
        // Build an annotations dict from any ``arg: T`` annotations
        // attached to ordinary, ``*args``, or ``**kwargs`` parameters.
        // CPython exposes the resulting dict as
        // ``func.__annotations__``; we pop it inside MakeFunction
        // when flag 0x04 is set.
        // CPython's compiler_visit_annotations order: posonly, args,
        // *args, kwonly, **kwargs, then 'return'.
        let mut annotated_params: Vec<(String, &Expr)> = Vec::new();
        for a in args
            .posonlyargs
            .iter()
            .chain(args.args.iter())
            .chain(args.vararg.iter())
            .chain(args.kwonlyargs.iter())
            .chain(args.kwarg.iter())
        {
            if let Some(ann) = a.annotation.as_ref() {
                annotated_params.push((a.name.clone(), ann));
            }
        }
        // `-> R` joins the same dict under the `'return'` key — at
        // MakeFunction time, *before* decorators see the function
        // (CPython compiles all annotations into one dict).
        if let Some(ret) = returns {
            annotated_params.push(("return".to_owned(), ret));
        }
        if !annotated_params.is_empty() {
            for (pname, ann) in &annotated_params {
                let idx = self.co.intern_constant(Constant::Str(pname.clone()));
                self.emit(OpCode::LoadConst, idx);
                self.emit_annotation(ann)?;
            }
            self.emit(OpCode::BuildMap, annotated_params.len() as u32);
            flags |= 0x04;
        }
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
        if has_kw_splat || has_starred_base {
            self.build_class_body(name, body, entry_line)?;
            let name_idx = self.co.intern_constant(Constant::Str(display.clone()));
            self.emit(OpCode::LoadConst, name_idx);
            self.emit(OpCode::BuildTuple, 2);
            self.compile_starred_args_tuple(bases)?;
            self.emit(OpCode::BinaryOp, BinOpKind::Add as u32);
            if keywords.is_empty() {
                self.emit(OpCode::CallEx, 0);
            } else {
                self.compile_kwargs_dict(keywords)?;
                self.emit(OpCode::CallEx, 1);
            }
        } else {
            self.build_class_body(name, body, entry_line)?;
            let name_idx = self.co.intern_constant(Constant::Str(display));
            self.emit(OpCode::LoadConst, name_idx);
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
        // Private name mangling (CPython `_Py_Mangle`): rewrite `__spam`
        // identifiers throughout the class's textual scope before
        // compiling. Done on a clone so the caller's AST is untouched.
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
            let mut needed = HashSet::new();
            for s in body {
                collect_inner_free(s, &self.bindings, &mut needed);
            }
            needed.contains("super")
                || needed.contains("__class__")
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
        if body.iter().any(stmt_needs_classdict) {
            inner.co.cellvars.push("__classdict__".to_owned());
            inner
                .bindings
                .insert("__classdict__".to_owned(), Binding::Cell);
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
        let mut needed_in_inner: HashSet<String> = HashSet::new();
        for s in body {
            collect_inner_free(s, &inner.bindings, &mut needed_in_inner);
        }
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

        inner.emit_entry_resume();
        // `__module__ = __name__` and `__qualname__ = <computed>`
        // boilerplate. The class body stores its full PEP 3155 qualname
        // (e.g. `Outer.method.<locals>.C`), not the bare name, so
        // `C.__qualname__` and `repr`s built from it match CPython.
        let qualname_str = inner.co.qualname.clone();
        let qualname_const = inner.co.intern_constant(Constant::Str(qualname_str));
        let qualname_idx = inner.co.intern_name("__qualname__");
        inner.emit(OpCode::LoadConst, qualname_const);
        inner.emit(OpCode::StoreName, qualname_idx);

        // CPython 3.13 compiler extra: `__firstlineno__` (the line of
        // the `class` statement). Its sibling `__static_attributes__`
        // is stored *after* the body statements (see below), matching
        // CPython's emission order — a `__prepare__` mapping with an
        // instrumented `__setitem__` observes it last (test_metaclass).
        {
            let line_const = inner.co.intern_constant(Constant::Int(i64::from(
                entry_line.unwrap_or(self.current_line),
            )));
            let line_name = inner.co.intern_name("__firstlineno__");
            inner.emit(OpCode::LoadConst, line_const);
            // A `nonlocal __firstlineno__` declaration in the class body
            // redirects this store to the enclosing function's cell — the
            // class dict then carries no `__firstlineno__` and
            // `inspect.getsource` reports "source code not available"
            // (test_inspect test_getsource_on_class_without_firstlineno).
            match inner.classify_for_store("__firstlineno__") {
                Binding::Free | Binding::Nonlocal | Binding::Cell => {
                    let idx = inner.cell_or_free_index("__firstlineno__");
                    inner.emit(OpCode::StoreDeref, idx);
                }
                _ => {
                    inner.emit(OpCode::StoreName, line_name);
                }
            }
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
                let doc_name = inner.co.intern_name("__doc__");
                // CPython locates the `__doc__` store at the docstring
                // statement, so tracing a class body fires a `'line'`
                // event on the docstring line
                // (test_class_creation_with_docstrings).
                inner.set_line_from(body[0].span.start.0);
                inner.set_span(body[0].span);
                inner.emit(OpCode::LoadConst, doc_const);
                inner.emit(OpCode::StoreName, doc_name);
            }
        }

        // SETUP_ANNOTATIONS before the first body statement when the class
        // block contains an annotated statement at its own level (CPython
        // symtable `ste_annotations_used`), so a read of `__annotations__`
        // preceding the first annotation sees the dict.
        if block_has_annotations(body) {
            // Located on the body's first statement, as in compiler_body.
            if let Some(first) = body.first() {
                inner.set_line_from(first.span.start.0);
                inner.set_span(first.span);
            }
            inner.emit(OpCode::SetupAnnotations, 0);
            inner.annotations_initialized = true;
        }

        // The docstring statement was consumed by the `__doc__` store
        // above; compiling it again would add a second traced NOP.
        let stmts = if class_has_docstring {
            &body[1..]
        } else {
            body
        };
        for s in stmts {
            inner.compile_stmt(s)?;
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
            collect_self_attr_stores(body, "self", &mut attrs);
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
        if needs_class_closure {
            let class_cell_idx = inner.cell_or_free_index("__class__");
            inner.emit(OpCode::LoadClosure, class_cell_idx);
            let classcell_name = inner.co.intern_name("__classcell__");
            inner.emit(OpCode::StoreName, classcell_name);
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

    /// Unwind the operand stack for a `break`/`continue`: inline every
    /// `finally` clause that lives *inside* the enclosing loop (i.e.
    /// was pushed after the current loop frame) in innermost-out
    /// order, interleaved — in recency order — with pops for
    /// exception regions ([prev, exc] of an exception-path finally
    /// copy), handler bodies ([prev] of an `except` clause), and
    /// pending return values that sit *above* each frame's stack
    /// state. Ordering matters now that a `with`'s bound `__exit__`
    /// is a real stack slot: its inline consumes TOS, so everything
    /// newer must be drained first. `tgt_*` are the loop-entry
    /// snapshots; everything above them is drained by the end.
    fn unwind_for_loop_exit(
        &mut self,
        tgt_exc: u32,
        tgt_handler: u32,
        tgt_rv: u32,
    ) -> Result<(), CompileError> {
        let loop_depth = self.loop_stack.len();
        let mut to_inline: Vec<FinallyFrame> = Vec::new();
        for frame in self.finally_stack.iter().rev() {
            if frame.loop_depth_at_push >= loop_depth {
                to_inline.push(clone_finally_frame(frame));
            } else {
                break;
            }
        }
        let mut cur_exc = self.exc_on_stack;
        let mut cur_handler = self.handler_depth;
        let mut cur_rv = self.pending_retvals;
        let drain = |c: &mut Self,
                     cur_exc: &mut u32,
                     cur_handler: &mut u32,
                     cur_rv: &mut u32,
                     exc_floor: u32,
                     handler_floor: u32,
                     rv_floor: u32| {
            // Exception regions: [prev, exc] — pop the value, then
            // restore the handled-exception state (CPython order).
            while *cur_exc > exc_floor {
                c.emit(OpCode::PopTop, 0);
                c.emit(OpCode::PopExcept, 0);
                *cur_exc -= 1;
            }
            // Handler bodies: [prev] — POP_EXCEPT during block unwind.
            while *cur_handler > handler_floor {
                c.emit(OpCode::PopExcept, 0);
                *cur_handler -= 1;
            }
            // Abandoned pending return values
            // (test_grammar.test_break_in_finally_after_return).
            while *cur_rv > rv_floor {
                c.emit(OpCode::PopTop, 0);
                *cur_rv -= 1;
            }
        };
        let saved = std::mem::take(&mut self.finally_stack);
        // Walk innermost out; on each iteration further trim the
        // finally stack so a `return` nested inside a finally body
        // can't re-inline its own ancestors infinitely.
        let mut hole_starts: Vec<(u32, u32)> = Vec::new();
        let mut result: Result<(), CompileError> = Ok(());
        for (offset, frame) in to_inline.iter().enumerate() {
            drain(
                self,
                &mut cur_exc,
                &mut cur_handler,
                &mut cur_rv,
                frame.exc_at_push.max(tgt_exc),
                frame.handler_at_push.max(tgt_handler),
                frame.rv_at_push.max(tgt_rv),
            );
            let outer_count = saved.len().saturating_sub(offset + 1);
            self.finally_stack = saved
                .iter()
                .take(outer_count)
                .map(clone_finally_frame)
                .collect();
            let inline_start = self.next_offset();
            if let Err(e) = self.emit_finally_frame(frame, false) {
                result = Err(e);
                break;
            }
            hole_starts.push((frame.id, inline_start));
        }
        self.finally_stack = saved;
        result?;
        drain(
            self,
            &mut cur_exc,
            &mut cur_handler,
            &mut cur_rv,
            tgt_exc,
            tgt_handler,
            tgt_rv,
        );
        // Each inlined finally runs once, here, for the loop exit; a
        // `raise` from it must skip this try's own coverage (and that of
        // any inner try) and propagate to an enclosing one. Extend every
        // hole to the end of the whole inline run so an outer frame's
        // `raise` is excluded from inner tries too. The trailing
        // `PopTop`/jump the caller emits cannot raise.
        let inline_end = self.next_offset();
        for (id, start) in hole_starts {
            self.finally_holes.push((id, start, inline_end));
        }
        Ok(())
    }

    /// Emit cleanup code for one `FinallyFrame`. `Stmts` frames
    /// re-compile the AST body; `WithExit` frames call the on-stack
    /// bound `__exit__` with three `None`s. `preserve_tos` marks a
    /// pending return value riding on top of the frame's stack state
    /// (CPython's `preserve_tos` in `codegen_unwind_fblock`): the
    /// with-exits SWAP it out of the way before consuming their slot.
    fn emit_finally_frame(
        &mut self,
        frame: &FinallyFrame,
        preserve_tos: bool,
    ) -> Result<(), CompileError> {
        match &frame.kind {
            FinallyKind::Stmts(body) => {
                for s in body {
                    self.compile_stmt(s)?;
                }
                Ok(())
            }
            FinallyKind::WithExit { line, span } => {
                // The bound `__exit__` (pushed by BEFORE_WITH) is on
                // the operand stack; call it in place (no `LoadAttr`).
                // Located on the `with` statement itself (CPython).
                self.current_line = *line;
                self.current_span = *span;
                if preserve_tos {
                    self.emit(OpCode::Swap, 2);
                }
                let none_idx = self.co.intern_constant(Constant::None);
                self.emit(OpCode::LoadConst, none_idx);
                self.emit(OpCode::LoadConst, none_idx);
                self.emit(OpCode::LoadConst, none_idx);
                // First None rides the self slot (CPython's with-exit
                // shape is `CALL 2`).
                self.emit(OpCode::CallSelf, 3);
                self.emit(OpCode::PopTop, 0);
                Ok(())
            }
            FinallyKind::AsyncWithExit { line, span } => {
                // `await TOS(None, None, None)`. The bound coroutine
                // method was pushed by BEFORE_ASYNC_WITH. Located on
                // the `async with` statement itself (CPython).
                self.current_line = *line;
                self.current_span = *span;
                if preserve_tos {
                    self.emit(OpCode::Swap, 2);
                }
                let none_idx = self.co.intern_constant(Constant::None);
                self.emit(OpCode::LoadConst, none_idx);
                self.emit(OpCode::LoadConst, none_idx);
                self.emit(OpCode::LoadConst, none_idx);
                // First None rides the self slot (wire `CALL 2`).
                self.emit(OpCode::CallSelf, 3);
                self.compile_await_dance(2);
                self.emit(OpCode::PopTop, 0);
                // `return`/`break`/`continue` leaves the `async with` for
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
        // PEP 626: the `try:` line is "executed" and must fire a line
        // event even though it compiles to nothing (CPython emits a NOP
        // carrying the statement's location; trace consumers — and
        // test_sys_settrace's relative-line bookkeeping — count on it).
        self.emit(OpCode::Nop, 0);
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
            self.finally_stack.push(FinallyFrame {
                kind: FinallyKind::Stmts(finalbody.to_vec()),
                loop_depth_at_push: self.loop_stack.len(),
                id,
                pop_except_after: false,
                exc_at_push: self.exc_on_stack,
                handler_at_push: self.handler_depth,
                rv_at_push: self.pending_retvals,
            });
            Some(id)
        } else {
            None
        };
        // The finally frame whose return/break/continue-path inlines must
        // be punched out of this statement's body coverage. With a
        // `finally` it is this try's own frame (just pushed, now on top);
        // without one it is the innermost *enclosing* finally, since a
        // `return` through this body still inlines that enclosing finally
        // here. Captured before the body compiles so nested frames pushed
        // by inner `try`s don't shadow it.
        let body_frame_id = self.finally_stack.last().map(|f| f.id);
        let wrap_start = self.next_offset();
        let body_start = self.next_offset();
        for s in body {
            self.compile_stmt(s)?;
        }
        let body_end = self.next_offset();
        let mut normal_skip = None;
        if has_handlers {
            // Else clause runs only on normal body completion, inline
            // right after the body and *outside* the handled range
            // (CPython compiles it before the handlers): an exception
            // raised in `else` does not reach this statement's own
            // `except` clauses — only an enclosing `finally`.
            for s in orelse {
                self.compile_stmt(s)?;
            }
            // Normal-exit hop over the handler region: CPython's
            // NO_LOCATION JUMP_NO_INTERRUPT to `end` (backward — and
            // thus visible as JUMP_BACKWARD_NO_INTERRUPT — once the
            // cold-block pass moves the handlers to the stream end).
            let j = self.emit_no_line(OpCode::JumpForward, 0);
            self.synthetic_jumps.insert(j);
            self.no_interrupt_jumps.insert(j);
            normal_skip = Some(j);
        }

        // Handlers begin here (reachable only via exception edges; the
        // cold-block pass relocates them, as CPython's flowgraph does).
        let handlers_start = self.next_offset();
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
                // [prev, orig, res, rest]
                let clause_body_start = self.next_offset();
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
                    self.finally_stack.push(FinallyFrame {
                        kind: FinallyKind::Stmts(stmts.clone()),
                        loop_depth_at_push: self.loop_stack.len(),
                        id,
                        pop_except_after: false,
                        exc_at_push: self.exc_on_stack,
                        handler_at_push: self.handler_depth,
                        rv_at_push: self.pending_retvals,
                    });
                }
                for s in &h.body {
                    self.compile_stmt(s)?;
                }
                if unbind_stmts.is_some() {
                    self.finally_stack.pop();
                }
                let clause_body_end = self.next_offset();
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
                            if self.co.linetable[k1] != 0 {
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
                    self.synthetic_jumps.insert(j);
                    self.no_interrupt_jumps.insert(j);
                    reraise_star_jumps.push(j);
                } else {
                    let j = self.emit_no_line(OpCode::JumpForward, 0);
                    self.synthetic_jumps.insert(j);
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
            self.synthetic_jumps.insert(exit);
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
                start: handlers_start,
                end: coverage_end,
                handler: cleanup_start,
                depth: HANDLER_DEPTH_ANCHOR_FLAG | handlers_start,
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
            let mut seg_start = handlers_start;
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
                            let name_expr = Expr {
                                kind: ExprKind::Name(n.clone()),
                                span: t.span,
                            };
                            self.compile_assign(&name_expr)?;
                        } else {
                            self.emit(OpCode::PopTop, 0);
                        }
                    }
                    None => {
                        // Bare `except:` matches anything; just discard exc.
                        self.emit(OpCode::PopTop, 0);
                    }
                }
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
                    unbind_frame_id = Some(id);
                    clause_frame_ids.push(id);
                    self.finally_stack.push(FinallyFrame {
                        kind: FinallyKind::Stmts(unbind_stmts.clone().unwrap_or_default()),
                        loop_depth_at_push: self.loop_stack.len(),
                        id,
                        pop_except_after: true,
                        exc_at_push: self.exc_on_stack,
                        handler_at_push: self.handler_depth,
                        rv_at_push: self.pending_retvals,
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
                // POP_EXCEPT: CPython's SETUP_CLEANUP is popped there,
                // so the exit run is uncovered by this region's own
                // cleanup.
                segments.push((seg_start, self.next_offset()));
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
                self.synthetic_jumps.insert(exit);
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
            // Unmatched: re-raise. Patch the last failed-match jump.
            while let Some(site) = next_handler_sites.pop() {
                let cur = self.next_offset();
                self.patch_jump(site, cur);
            }
            // CPython locates the unmatched-exception RERAISE on the
            // *last* `except` clause. Stack: [prev, exc] — RERAISE 0
            // pops exc and propagates it (through the cleanup tail
            // below, which restores the previous exc_info).
            if let Some(h) = handlers.last() {
                self.set_line_from(h.span.start.0);
                self.set_span(h.span);
            }
            self.emit(OpCode::Reraise, 0);
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
            for s in finalbody {
                self.compile_stmt(s)?;
            }
            // CPython's NO_LOCATION JUMP_NO_INTERRUPT over the
            // exceptional copy.
            let exit_j = self.emit_no_line(OpCode::JumpForward, 0);
            self.synthetic_jumps.insert(exit_j);
            self.no_interrupt_jumps.insert(exit_j);
            // Exceptional copy. The dispatch loop pushed the
            // propagating exception; PUSH_EXC_INFO slides the previous
            // one underneath. The exception stays on the stack across
            // `finalbody` — every statement compiles to stack-balanced
            // bytecode — then RERAISE 0 pops and re-raises it.
            // No location (CPython): the first finally-body line fires
            // the handler-entry `'line'` event.
            let fexc_start = self.next_offset();
            let push_exc_site = self.emit_no_line(OpCode::PushExcInfo, 0);
            self.exc_on_stack += 1;
            for s in finalbody {
                self.compile_stmt(s)?;
            }
            self.exc_on_stack -= 1;
            self.emit(OpCode::Reraise, 0);
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
                fexc_start,
                body_depth,
                false,
                wrap_frame_id,
            );
            self.co.exception_table.push(ExcHandler {
                start: fexc_start,
                end: fcleanup_start,
                handler: fcleanup_start,
                depth: HANDLER_DEPTH_SENTINEL,
                push_lasti: true,
            });
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
        // Evaluate cm; BEFORE_WITH does the special-method lookup once
        // and pushes the *bound* `__exit__` under the `__enter__`
        // result. The `__exit__` stays on the operand stack for the
        // whole body — CPython 3.13's SETUP_WITH discipline (test_dis
        // grades the exact shape, and co_varnames must not contain
        // synthetic slots).
        self.compile_expr(&item.context_expr)?;
        self.current_line = with_line;
        self.current_span = with_span;
        self.emit(OpCode::BeforeWith, 0);
        // CPython's exception coverage starts right after BEFORE_WITH:
        // the bind (or POP_TOP) of the `__enter__` result is inside it.
        let cover_start = self.next_offset();
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
        self.finally_stack.push(FinallyFrame {
            kind: FinallyKind::WithExit {
                line: with_line,
                span: with_span,
            },
            loop_depth_at_push: with_loop_depth,
            id: with_frame_id,
            pop_except_after: false,
            exc_at_push: self.exc_on_stack,
            handler_at_push: self.handler_depth,
            rv_at_push: self.pending_retvals,
        });

        let body_start = cover_start;
        let body_result = if rest.is_empty() {
            body.iter().try_for_each(|s| self.compile_stmt(s))
        } else {
            self.compile_with(rest, body)
        };
        body_result?;
        let body_end = self.next_offset();

        // Pop the synthetic frame; the explicit normal-exit path
        // below emits the same call inline.
        self.finally_stack.pop();

        // Attribute the whole exit path to this item's expression.
        self.current_line = with_line;
        self.current_span = with_span;

        // Normal exit: TOS is the bound `__exit__`; call it with three
        // `None`s. First None rides the self slot (CPython's with-exit
        // `CALL 2`).
        let none_idx = self.co.intern_constant(Constant::None);
        self.emit(OpCode::LoadConst, none_idx);
        self.emit(OpCode::LoadConst, none_idx);
        self.emit(OpCode::LoadConst, none_idx);
        self.emit(OpCode::CallSelf, 3);
        self.emit(OpCode::PopTop, 0);
        let end_jump = self.emit(OpCode::JumpForward, 0);

        // Exception handler (CPython 3.13 shape):
        //   L3: PUSH_EXC_INFO; WITH_EXCEPT_START; TO_BOOL;
        //       POP_JUMP_IF_TRUE L4; RERAISE 2
        //   L4: POP_TOP
        //   L5: POP_EXCEPT; POP_TOP; POP_TOP
        //   --  COPY 3; POP_EXCEPT; RERAISE 1   (cleanup, covers L3..L5)
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
            handler_start,
            HANDLER_DEPTH_ANCHOR_FLAG | depth_anchor,
            true,
            Some(with_frame_id),
        );
        // Entry stack: [__exit__, lasti, exc]. Record the propagating
        // exception as the active handled exception for the duration of
        // the `__exit__` call so a `raise` inside `__exit__` chains it
        // as the new exception's implicit `__context__` (PEP 3134) —
        // `contextlib.ExitStack`'s `_fix_exception_context` walks each
        // callback exception's context back to `sys.exc_info()[1]`.
        // After PUSH_EXC_INFO: [__exit__, lasti, prev, exc].
        let push_exc_site = self.emit(OpCode::PushExcInfo, 0);
        // Calls `__exit__(type(exc), exc, exc.__traceback__)` peeking
        // the exit at depth 4; pushes the result.
        self.emit(OpCode::WithExceptStart, 0);
        self.emit(OpCode::ToBool, 0);
        let swallow = self.emit(OpCode::PopJumpIfTrue, 0);
        // Falsy: re-raise `exc` (TOS), restoring f_lasti from the slot
        // two below it (CPython RERAISE 2) — no entry is recorded for
        // the re-raise site and the original traceback is preserved.
        self.emit(OpCode::Reraise, 2);
        let swallow_target = self.next_offset();
        self.patch_jump(swallow, swallow_target);
        // Swallowed: [__exit__, lasti, prev, exc] — drain.
        self.emit(OpCode::PopTop, 0);
        // The cleanup entry's coverage ends here (CPython's L5): the
        // drains below cannot raise.
        let cleanup_cover_end = self.next_offset();
        self.emit(OpCode::PopExcept, 0);
        self.emit(OpCode::PopTop, 0);
        self.emit(OpCode::PopTop, 0);
        let swallow_exit = self.emit_no_line(OpCode::JumpForward, 0);
        self.synthetic_jumps.insert(swallow_exit);
        // CPython's suppress exit is a JUMP_NO_INTERRUPT.
        self.no_interrupt_jumps.insert(swallow_exit);
        // Cleanup tail: a `raise` out of `__exit__` itself (or the
        // RERAISE) lands here with [__exit__, lasti, prev] preserved
        // plus the new lasti/exception: restore the handled-exception
        // state and re-raise (CPython `COPY 3; POP_EXCEPT; RERAISE 1`).
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
        self.patch_jump(swallow_exit, end);
        // Tag the active-handler entry with the pc just past the whole
        // handler region: the swallow path's POP_EXCEPT (or the cleanup
        // tail's) pops it; an escape beyond `end` drops it in the
        // unwinder.
        self.co.instructions[push_exc_site as usize].arg = end;
        Ok(())
    }

    fn cell_or_free_index(&mut self, name: &str) -> u32 {
        // Layout: cellvars first, then freevars.
        if let Some(i) = self.co.cellvars.iter().position(|n| n == name) {
            return i as u32;
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

    /// Emit code that ensures the current scope's `__annotations__`
    /// dict exists and records `annotation` against `name`. Used
    /// for class- and module-body `x: T = ...` statements.
    fn compile_annotation_record(
        &mut self,
        name: &str,
        annotation: &Expr,
    ) -> Result<(), CompileError> {
        // `__annotations__` is created lazily as an ordinary local
        // binding for class bodies (so we use STORE_NAME), and as a
        // global for module bodies. The setup code is idempotent:
        // `__annotations__ = __annotations__` is a no-op if it's
        // already present.
        //
        // The actual sequence emitted here for each annotation is:
        //   try: __annotations__
        //   except NameError: __annotations__ = {}
        //   __annotations__[name] = annotation
        //
        // We don't have try/except as an opcode-level construct
        // here, so we fall back to a guarded LOAD that defaults to
        // an empty dict if absent. This is implemented via the
        // SETUP_ANNOTATIONS pattern CPython uses, but simplified:
        // a plain BuildMap + STORE_NAME when missing.
        let dict_name = "__annotations__";
        // SETUP_ANNOTATIONS-equivalent: ensure the dict exists.
        // The simplest correct emission is: BUILD_MAP 0; STORE_NAME
        // __annotations__ — but this would overwrite an existing
        // dict every time. Instead we guard with a small subroutine:
        //
        //   if `__annotations__` not in scope: __annotations__ = {}
        //
        // ... which we approximate by calling a helper builtin.
        // Since we have neither, the practical approach is to lift
        // the dict creation to once-per-class-body via a flag.
        if !self.annotations_initialized {
            // BUILD_MAP 0; STORE_NAME __annotations__
            self.emit(OpCode::BuildMap, 0);
            let idx = self.co.intern_name(dict_name);
            self.emit(OpCode::StoreName, idx);
            self.annotations_initialized = true;
        }
        // __annotations__[name] = annotation
        self.emit_annotation(annotation)?;
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
                self.compile_expr(value)?;
                self.compile_expr(slice)?;
                self.emit(OpCode::StoreSubscr, 0);
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
                    self.emit(OpCode::UnpackEx, (before << 8) | after);
                    for t in items {
                        match &t.kind {
                            ExprKind::Starred(inner) => self.compile_assign(inner)?,
                            _ => self.compile_assign(t)?,
                        }
                    }
                } else {
                    self.emit(OpCode::UnpackSequence, items.len() as u32);
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

    /// Lower a positional argument list containing one or more
    /// `*x` splats into a single tuple on the stack. Each contiguous
    /// run of non-starred args becomes a `BuildTuple`; each `*x` is
    /// added as another tuple. We then concatenate by repeated
    /// `BinaryOp::Add` because that already does the right thing for
    /// tuples.
    fn compile_starred_args_tuple(&mut self, args: &[Expr]) -> Result<(), CompileError> {
        self.compile_splat_list(args)?;
        self.emit(OpCode::ListToTuple, 0);
        Ok(())
    }

    /// Lower a positional-argument (or display-element) list containing
    /// `*x` splats into a single `list` on the stack, CPython-style:
    /// `BUILD_LIST 0`, plain elements folded in with `LIST_APPEND`, each
    /// splat with `LIST_EXTEND` (whose non-iterable error is "Value
    /// after * must be an iterable, not X" — test_extcall).
    fn compile_splat_list(&mut self, args: &[Expr]) -> Result<(), CompileError> {
        self.emit(OpCode::BuildList, 0);
        for a in args {
            match &a.kind {
                ExprKind::Starred(inner) => {
                    self.compile_expr(inner)?;
                    self.emit(OpCode::ListExtend, 1);
                }
                _ => {
                    self.compile_expr(a)?;
                    self.emit(OpCode::ListAppend, 1);
                }
            }
        }
        Ok(())
    }

    /// Lower a list/set *display* containing one or more PEP 448 `*x`
    /// splats. Build an empty container with `build` (count 0), then fold
    /// each element in: a plain element via the `single` method
    /// (`list.append` / `set.add`) and each `*x` via the `spread` method
    /// (`list.extend` / `set.update`). The empty container comes from the
    /// opcode itself, so the lowering is robust against `list`/`set`
    /// being shadowed in the enclosing scope (unlike the call-site tuple
    /// path, which loads the `tuple` builtin by name).
    fn compile_unpacking_sequence(
        &mut self,
        items: &[Expr],
        build: OpCode,
        single: &str,
        spread: &str,
    ) -> Result<(), CompileError> {
        self.emit(build, 0);
        for item in items {
            self.emit(OpCode::CopyTop, 0);
            match &item.kind {
                ExprKind::Starred(inner) => {
                    let m = self.co.intern_name(spread);
                    self.emit(OpCode::LoadMethodAttr, m);
                    self.compile_expr(inner)?;
                }
                _ => {
                    let m = self.co.intern_name(single);
                    self.emit(OpCode::LoadMethodAttr, m);
                    self.compile_expr(item)?;
                }
            }
            self.emit(OpCode::Call, 1);
            self.emit(OpCode::PopTop, 0);
        }
        Ok(())
    }

    /// Lower a keyword-argument list, possibly with `**d` spreads,
    /// into a single dict on the stack. Each named kwarg becomes a
    /// `(name, value)` pair; each `**d` is merged in with `dict.update`.
    fn compile_kwargs_dict(
        &mut self,
        kwargs: &[weavepy_parser::ast::Keyword],
    ) -> Result<(), CompileError> {
        // First materialise the named kwargs in a single BuildMap so
        // we have a base dict on the stack. Then fold each ** splat
        // in with `dict.update(...)`.
        let mut explicit_count: u32 = 0;
        for k in kwargs {
            if let Some(name) = &k.arg {
                let const_idx = self.co.intern_constant(Constant::Str(name.clone()));
                self.emit(OpCode::LoadConst, const_idx);
                self.compile_expr(&k.value)?;
                explicit_count += 1;
            }
        }
        self.emit(OpCode::BuildMap, explicit_count);
        for k in kwargs {
            if k.arg.is_none() {
                // `arg = 1` selects CPython's DICT_MERGE semantics
                // (call-site `**` splat): the operand must be a mapping
                // ("argument after ** must be a mapping, not list") and
                // duplicate keywords raise, unlike the dict-display
                // DICT_UPDATE which last-writer-wins.
                self.compile_expr(&k.value)?;
                self.emit(OpCode::DictUpdate, 1);
            }
        }
        Ok(())
    }

    fn compile_delete(&mut self, target: &Expr) -> Result<(), CompileError> {
        match &target.kind {
            ExprKind::Name(n) if n == "__debug__" => Err(CompileError::spanned(
                "cannot delete __debug__",
                target.span,
            )),
            ExprKind::Name(n) => {
                self.emit_delete_name(n);
                Ok(())
            }
            ExprKind::Attribute { value, attr } => {
                self.compile_expr(value)?;
                let idx = self.co.intern_name(attr);
                let saved = self.current_span;
                self.set_span(target.span);
                self.with_attr_location(target.span.end.0, attr.len() as u32, |c| {
                    c.emit(OpCode::DeleteAttr, idx);
                });
                self.current_span = saved;
                Ok(())
            }
            ExprKind::Subscript { value, slice } => {
                self.compile_expr(value)?;
                self.compile_expr(slice)?;
                self.emit(OpCode::DeleteSubscr, 0);
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
        // PEP 695 annotation scope inside a class body: free and
        // (implicit-)global loads consult the `__classdict__` mapping
        // first — CPython's `LOAD_FROM_DICT_OR_{DEREF,GLOBALS}`. The
        // scope's own locals/cells (the type parameters themselves)
        // resolve normally below.
        if let Some(ctx) = self.lazy_class_ctx.clone() {
            // Names local to the annotation scope itself (the type
            // parameters, hoisted `.defaults`, …) resolve normally.
            let own = matches!(binding, Some(Binding::Local | Binding::Cell));
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
                self.emit(OpCode::LoadClassderef, idx);
            } else {
                let idx = self.co.intern_name(name);
                self.emit(OpCode::LoadName, idx);
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
                // attribute (rare but legal). LOAD_CLASSDEREF tries the class
                // namespace first, then falls back to the cell.
                if self.kind == CodeKind::Class {
                    self.emit(OpCode::LoadClassderef, idx);
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
                let mut jumps = Vec::new();
                let n = values.len();
                for (i, v) in values.iter().enumerate() {
                    self.compile_expr(v)?;
                    if i + 1 < n {
                        self.emit(OpCode::CopyTop, 0);
                        let j = self.emit(jump_op, 0);
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
                    self.compile_compare(left, &[inv], comparators)?;
                } else {
                    self.compile_expr(operand)?;
                    let kind = match op {
                        UnaryOp::UAdd => UnaryKind::Pos,
                        UnaryOp::USub => UnaryKind::Neg,
                        UnaryOp::Not => UnaryKind::Not,
                        UnaryOp::Invert => UnaryKind::Invert,
                    };
                    self.emit(OpCode::UnaryOp, kind as u32);
                }
            }
            ExprKind::Compare {
                left,
                ops,
                comparators,
            } => {
                self.compile_compare(left, ops, comparators)?;
            }
            ExprKind::IfExp { test, body, orelse } => {
                let (cond, invert) = strip_not_chain(test);
                self.compile_expr(cond)?;
                if !expr_is_bool(cond) {
                    self.emit(OpCode::ToBool, 0);
                }
                let jump_else = self.emit(
                    if invert {
                        OpCode::PopJumpIfTrue
                    } else {
                        OpCode::PopJumpIfFalse
                    },
                    0,
                );
                self.compile_expr(body)?;
                let jump_end = self.emit(OpCode::JumpForward, 0);
                let else_target = self.next_offset();
                self.patch_jump(jump_else, else_target);
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
                // Method calls report the method name as the CALL's
                // start location (CPython adjusts via
                // `update_start_location_to_match_attr`).
                let meth = match &func.kind {
                    ExprKind::Attribute { attr, .. } => Some((func.span.end.0, attr.len() as u32)),
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
                            ExprKind::Name(n) => !self.params.module_imports.contains(n),
                            _ => true,
                        },
                        _ => false,
                    };
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
                    // The callable's NULL mate, at the callable's own
                    // location (CPython codegen_call: `ADDOP(c,
                    // LOC(func), PUSH_NULL)`).
                    let saved_span = self.current_span;
                    let saved_line = self.current_line;
                    self.set_line_from(func.span.start.0);
                    self.set_span(func.span);
                    self.emit(OpCode::PushNull, 0);
                    self.current_span = saved_span;
                    self.current_line = saved_line;
                }
                if has_starred || has_kw_splat {
                    // `f(*x)` with a lone splat passes `x` through raw —
                    // the VM's `CallEx` converts it, branding a
                    // non-iterable with the callable's name (CPython
                    // `do_call`: "g() argument after * must be an
                    // iterable, not Nothing"). Mixed positionals fold
                    // into a list via LIST_APPEND/LIST_EXTEND instead.
                    if let [a] = args.as_slice() {
                        if let ExprKind::Starred(inner) = &a.kind {
                            self.compile_expr(inner.as_ref())?;
                        } else {
                            self.compile_splat_list(args)?;
                        }
                    } else {
                        self.compile_splat_list(args)?;
                    }
                    if !keywords.is_empty() || has_kw_splat {
                        self.compile_kwargs_dict(keywords)?;
                        emit_call(self, OpCode::CallEx, 1);
                    } else {
                        emit_call(self, OpCode::CallEx, 0);
                    }
                } else if args.len() + keywords.len() * 2 > STACK_USE_GUIDELINE {
                    // CPython's big-call path (codegen_call_helper): when
                    // the operand count exceeds the stack-use guideline
                    // (each keyword weighs double: name + value),
                    // positionals accumulate through a list into a tuple
                    // and keywords into a dict, called via
                    // CALL_FUNCTION_EX — `co_stacksize` stays O(1)
                    // (test_compile TestExpressionStackSize).
                    if args.is_empty() {
                        let idx = self.co.intern_constant(Constant::Tuple(Vec::new()));
                        self.emit(OpCode::LoadConst, idx);
                    } else {
                        self.emit(OpCode::BuildList, 0);
                        for a in args {
                            self.compile_expr(a)?;
                            self.emit(OpCode::ListAppend, 1);
                        }
                        self.emit(OpCode::ListToTuple, 0);
                    }
                    if keywords.is_empty() {
                        emit_call(self, OpCode::CallEx, 0);
                    } else {
                        self.emit(OpCode::BuildMap, 0);
                        for k in keywords {
                            let n = k.arg.clone().expect("checked above");
                            let idx = self.co.intern_constant(Constant::Str(n));
                            self.emit(OpCode::LoadConst, idx);
                            self.compile_expr(&k.value)?;
                            self.emit(OpCode::MapAdd, 1);
                        }
                        emit_call(self, OpCode::CallEx, 1);
                    }
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
                    self.emit(OpCode::LoadConst, tup_idx);
                    emit_call(self, OpCode::CallKw, args.len() as u32);
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
                self.compile_expr(slice)?;
                self.emit(OpCode::BinarySubscr, 0);
            }
            ExprKind::Slice { lower, upper, step } => {
                let push_or_none =
                    |this: &mut Self, x: &Option<Box<Expr>>| -> Result<u32, CompileError> {
                        if let Some(e) = x {
                            this.compile_expr(e)?;
                            Ok(1)
                        } else {
                            let idx = this.co.intern_constant(Constant::None);
                            this.emit(OpCode::LoadConst, idx);
                            Ok(1)
                        }
                    };
                push_or_none(self, lower)?;
                push_or_none(self, upper)?;
                let has_step = step.is_some();
                push_or_none(self, step)?;
                let _ = has_step;
                self.emit(OpCode::BuildSlice, 3);
            }
            ExprKind::Tuple(items) => {
                if items.iter().any(|x| matches!(x.kind, ExprKind::Starred(_))) {
                    // PEP 448: `(*a, b, *c)` — reuse the call-site splat
                    // lowering, which concatenates tuple segments.
                    self.compile_starred_args_tuple(items)?;
                } else if let Some(folded) = fold_const_tuple(items) {
                    // CPython's AST optimizer folds an all-constant tuple
                    // display into a single constant: one `LoadConst` at
                    // the tuple's own line — element lines never fire
                    // `'line'` trace events (test_trace test_issue9936's
                    // multi-line `return (1,\n2,\n3)`).
                    let idx = self.co.intern_constant(folded);
                    self.emit(OpCode::LoadConst, idx);
                } else if items.len() > STACK_USE_GUIDELINE {
                    // CPython's starunpack_helper "big" path: accumulate
                    // through a list so `co_stacksize` stays O(1)
                    // (test_compile TestExpressionStackSize).
                    self.emit(OpCode::BuildList, 0);
                    for x in items {
                        self.compile_expr(x)?;
                        self.emit(OpCode::ListAppend, 1);
                    }
                    self.emit(OpCode::ListToTuple, 0);
                } else {
                    for x in items {
                        self.compile_expr(x)?;
                    }
                    self.emit(OpCode::BuildTuple, items.len() as u32);
                }
            }
            ExprKind::List(items) => {
                if items.iter().any(|x| matches!(x.kind, ExprKind::Starred(_))) {
                    self.compile_splat_list(items)?;
                } else if items.len() > STACK_USE_GUIDELINE {
                    self.emit(OpCode::BuildList, 0);
                    for x in items {
                        self.compile_expr(x)?;
                        self.emit(OpCode::ListAppend, 1);
                    }
                } else {
                    for x in items {
                        self.compile_expr(x)?;
                    }
                    self.emit(OpCode::BuildList, items.len() as u32);
                }
            }
            ExprKind::Set(items) => {
                if items.iter().any(|x| matches!(x.kind, ExprKind::Starred(_))) {
                    self.compile_unpacking_sequence(items, OpCode::BuildSet, "add", "update")?;
                } else if items.len() > STACK_USE_GUIDELINE {
                    self.emit(OpCode::BuildSet, 0);
                    for x in items {
                        self.compile_expr(x)?;
                        self.emit(OpCode::SetAdd, 1);
                    }
                } else {
                    for x in items {
                        self.compile_expr(x)?;
                    }
                    self.emit(OpCode::BuildSet, items.len() as u32);
                }
            }
            ExprKind::Dict { keys, values } => {
                // Two emission paths: the "no spread" common case
                // emits a single `BuildMap`, while the spread case
                // builds an empty dict and accumulates via runs of
                // `BuildMap` for explicit `{k: v}` chunks separated
                // by `DictUpdate` for each `**other` segment.
                let has_spread = keys.iter().any(|k| k.is_none());
                if !has_spread && keys.len() * 2 > STACK_USE_GUIDELINE {
                    // CPython codegen_dict "big" path: an empty map plus
                    // MAP_ADD per pair keeps the stack O(1).
                    self.emit(OpCode::BuildMap, 0);
                    for (k, v) in keys.iter().zip(values.iter()) {
                        if let Some(ke) = k {
                            self.compile_expr(ke)?;
                            self.compile_expr(v)?;
                            self.emit(OpCode::MapAdd, 1);
                        }
                    }
                } else if !has_spread {
                    for (k, v) in keys.iter().zip(values.iter()) {
                        if let Some(ke) = k {
                            self.compile_expr(ke)?;
                            self.compile_expr(v)?;
                        }
                    }
                    self.emit(OpCode::BuildMap, keys.len() as u32);
                } else {
                    self.emit(OpCode::BuildMap, 0);
                    let mut pending: u32 = 0;
                    let flush_pending = |slf: &mut Self, pending: &mut u32| {
                        if *pending > 0 {
                            slf.emit(OpCode::BuildMap, *pending);
                            slf.emit(OpCode::DictUpdate, 0);
                            *pending = 0;
                        }
                    };
                    for (k, v) in keys.iter().zip(values.iter()) {
                        match k {
                            Some(ke) => {
                                self.compile_expr(ke)?;
                                self.compile_expr(v)?;
                                pending += 1;
                            }
                            None => {
                                flush_pending(self, &mut pending);
                                self.compile_expr(v)?;
                                self.emit(OpCode::DictUpdate, 0);
                            }
                        }
                    }
                    flush_pending(self, &mut pending);
                }
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
        let loop_start = self.next_offset();
        let send = self.emit(OpCode::Send, 0);
        let yield_at = self.emit(OpCode::YieldValue, 1);
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
        self.emit(OpCode::GetAiter, 0);
        let loop_top = self.next_offset();
        // GetAnext peeks the aiter and pushes a *coerced* awaitable
        // (CPython's GET_ANEXT applies `_PyCoro_GetAwaitableIter`
        // itself — no GET_AWAITABLE in the async-for dance). The
        // send-dance drives it; on success we land at the STORE_FAST
        // target. On StopAsyncIteration, control flows to the cleanup
        // block.
        self.emit(OpCode::GetAnext, 0);
        self.emit_send_dance(3);
        // The StopAsyncIteration window closes here: only the
        // `__anext__` await may end the loop. An exception raised by
        // the assignment target or the body — even a
        // StopAsyncIteration — propagates (bpo-44895).
        let dance_end = self.next_offset();
        // Stack: [aiter, value]. Move the value into the target.
        self.compile_assign(target)?;
        self.loop_stack.push(LoopFrame {
            continue_target: loop_top,
            break_sites: Vec::new(),
            is_for_loop: true,
            handler_depth_at_entry: self.handler_depth,
            exc_on_stack_at_entry: self.exc_on_stack,
            pending_retvals_at_entry: self.pending_retvals,
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
            start: loop_top,
            end: dance_end,
            handler: cleanup_target,
            depth: HANDLER_DEPTH_SENTINEL,
            push_lasti: false,
        });
        // Cleanup: pop aiter + exception, then run the `else` clause.
        // Located on the `async for` statement (CPython): exhaustion —
        // and an implicit function-ending return after it — report the
        // loop's line, not the body's last line.
        self.current_line = stmt_line;
        self.current_span = stmt_span;
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
        // BEFORE_ASYNC_WITH leaves [aexit, awaitable(aenter)]. The bound
        // `__aexit__` stays on the operand stack for the whole body —
        // the async counterpart of `compile_with`'s SETUP_WITH shape.
        self.emit(OpCode::BeforeAsyncWith, 0);
        self.compile_await_dance(1);
        // Stack: [aexit, value]. Exception coverage starts at the bind.
        let cover_start = self.next_offset();
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
        self.finally_stack.push(FinallyFrame {
            kind: FinallyKind::AsyncWithExit {
                line: with_line,
                span: with_span,
            },
            loop_depth_at_push: awith_loop_depth,
            id: awith_frame_id,
            pop_except_after: false,
            exc_at_push: self.exc_on_stack,
            handler_at_push: self.handler_depth,
            rv_at_push: self.pending_retvals,
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
        self.current_line = with_line;
        self.current_span = with_span;

        // Normal exit: TOS is the bound `__aexit__`; `await
        // aexit(None, None, None)`. First None rides the self slot
        // (wire `CALL 2`).
        let none_idx = self.co.intern_constant(Constant::None);
        self.emit(OpCode::LoadConst, none_idx);
        self.emit(OpCode::LoadConst, none_idx);
        self.emit(OpCode::LoadConst, none_idx);
        self.emit(OpCode::CallSelf, 3);
        self.compile_await_dance(2);
        self.emit(OpCode::PopTop, 0);
        let end_jump = self.emit(OpCode::JumpForward, 0);

        // Exception handler — the async counterpart of `compile_with`'s:
        //   PUSH_EXC_INFO; WITH_EXCEPT_START; <await>; TO_BOOL;
        //   POP_JUMP_IF_TRUE swallow; RERAISE 2
        //   swallow: POP_TOP; POP_EXCEPT; POP_TOP; POP_TOP
        //   cleanup: COPY 3; POP_EXCEPT; RERAISE 1
        let handler_start = self.next_offset();
        // Same lasti semantics as the sync `with` cleanup. Punch out the
        // body's `return`/`break`/`continue`-path `await __aexit__(None,
        // None, None)` inline so a `raise` from it isn't re-caught and
        // `__aexit__` re-awaited with the exception triple. Depth is
        // anchored at the body baseline (keeps the on-stack `__aexit__`).
        self.push_body_exc_entries(
            body_start,
            body_end,
            handler_start,
            HANDLER_DEPTH_ANCHOR_FLAG | depth_anchor,
            true,
            Some(awith_frame_id),
        );
        // Entry stack: [aexit, lasti, exc]. Record the propagating
        // exception as the active handled exception for the duration of
        // the awaited `__aexit__` so a `raise` inside it chains as the
        // new exception's implicit `__context__` (PEP 3134) —
        // `contextlib.AsyncExitStack`'s `_fix_exception_context` walks
        // each callback exception's context back to `sys.exc_info()[1]`.
        // After PUSH_EXC_INFO: [aexit, lasti, prev, exc].
        let push_exc_site = self.emit(OpCode::PushExcInfo, 0);
        // Calls `__aexit__(type(exc), exc, exc.__traceback__)` peeking
        // the exit at depth 4; pushes the coroutine, which the dance
        // awaits into the suppress flag.
        self.emit(OpCode::WithExceptStart, 0);
        self.compile_await_dance(2);
        self.emit(OpCode::ToBool, 0);
        let swallow = self.emit(OpCode::PopJumpIfTrue, 0);
        // Falsy: re-raise `exc` (TOS), restoring f_lasti from the slot
        // two below (CPython RERAISE 2); the original traceback is
        // preserved (no entry recorded for the re-raise site).
        self.emit(OpCode::Reraise, 2);
        let swallow_target = self.next_offset();
        self.patch_jump(swallow, swallow_target);
        // Swallowed: [aexit, lasti, prev, exc] — drain.
        self.emit(OpCode::PopTop, 0);
        // Cleanup coverage ends here: the drains below cannot raise.
        let cleanup_cover_end = self.next_offset();
        self.emit(OpCode::PopExcept, 0);
        self.emit(OpCode::PopTop, 0);
        self.emit(OpCode::PopTop, 0);
        let swallow_exit = self.emit_no_line(OpCode::JumpForward, 0);
        self.synthetic_jumps.insert(swallow_exit);
        // CPython's suppress exit is a JUMP_NO_INTERRUPT.
        self.no_interrupt_jumps.insert(swallow_exit);
        // Cleanup tail: a `raise` out of the awaited `__aexit__` (or the
        // RERAISE) lands here with [aexit, lasti, prev] preserved plus
        // the new lasti/exception: restore the handled-exception state
        // and re-raise (CPython `COPY 3; POP_EXCEPT; RERAISE 1`).
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
        self.patch_jump(swallow_exit, end);
        // Tag the active-handler entry with the pc just past the whole
        // handler region: the swallow path's POP_EXCEPT (or the cleanup
        // tail's) pops it; an escape beyond `end` drops it in the
        // unwinder.
        self.co.instructions[push_exc_site as usize].arg = end;
        Ok(())
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

    fn compile_compare(
        &mut self,
        left: &Expr,
        ops: &[CmpOp],
        comparators: &[Expr],
    ) -> Result<(), CompileError> {
        // Single comparison — straightforward.
        if ops.len() == 1 {
            self.compile_expr(left)?;
            self.compile_expr(&comparators[0])?;
            emit_cmp_op(self, ops[0]);
            return Ok(());
        }
        // Chained: `a OP1 b OP2 c` ⇒ `(a OP1 b) and (b OP2 c)` with
        // `b` evaluated exactly once. We borrow a synthetic local
        // per intermediate operand to hold the value across the
        // chain. CPython uses COPY/SWAP; the slice favours clarity.
        let tmp = format!(".chain{}", self.chain_counter);
        self.chain_counter += 1;
        let tmp_idx = self.var_index_or_add(&tmp);

        self.compile_expr(left)?;
        let mut short_circuit_jumps = Vec::new();
        let last = ops.len() - 1;
        for i in 0..ops.len() {
            let rhs = &comparators[i];
            self.compile_expr(rhs)?;
            if i < last {
                // Stack: ..., lhs, rhs. Stash rhs in temp so we can
                // reuse it as next lhs.
                self.emit(OpCode::CopyTop, 0); // [.., lhs, rhs, rhs]
                self.emit(OpCode::StoreFast, tmp_idx); // [.., lhs, rhs]
                emit_cmp_op(self, ops[i]); // [.., result]
                let jf = self.emit(OpCode::PopJumpIfFalse, 0);
                short_circuit_jumps.push(jf);
                self.emit(OpCode::LoadFast, tmp_idx); // restore lhs
            } else {
                emit_cmp_op(self, ops[i]);
            }
        }
        let end_jump = self.emit(OpCode::JumpForward, 0);
        let false_target = self.next_offset();
        for jf in short_circuit_jumps {
            self.patch_jump(jf, false_target);
        }
        let false_idx = self.co.intern_constant(Constant::Bool(false));
        self.emit(OpCode::LoadConst, false_idx);
        let end = self.next_offset();
        self.patch_jump(end_jump, end);
        Ok(())
    }

    // ---------- comprehensions ----------

    /// Whether this comprehension takes the PEP 709 inlined lowering.
    /// Phase-1 conservative gates — anything rejected here keeps the
    /// classic nested-function lowering (still correct, just a
    /// different frame shape):
    /// - generator expressions never inline (per the PEP);
    /// - class bodies keep the fallback (hidden fast locals in a class
    ///   frame need bespoke handling);
    /// - async comprehensions inline only inside an async context;
    /// - no walrus targets in the comp scope;
    /// - no comp-local may be closed over by a nested scope (would
    ///   need a parent cell);
    /// - comp-locals must not collide with enclosing
    ///   cell/free/nonlocal names.
    fn comp_inline_eligible(
        &self,
        kind: CompKind,
        elt: &Expr,
        value: Option<&Expr>,
        generators: &[Comprehension],
        is_async_comp: bool,
    ) -> bool {
        if matches!(kind, CompKind::Generator) {
            return false;
        }
        if matches!(self.kind, CodeKind::Class) {
            return false;
        }
        // A PEP 695 annotation scope that can see a class namespace
        // resolves name loads through `__classdict__` — but a
        // comprehension is a real nested scope whose reads must *skip*
        // the class namespace (`type Alias = [T for _ in (1,)]` in a
        // class body reads the global `T`, test_type_params
        // test_nested_scope_in_generic_alias). Inlining would leak the
        // classdict resolution into the comp body; keep the
        // nested-function lowering, whose free-variable analysis
        // already skips class scopes.
        if self.lazy_class_ctx.is_some() {
            return false;
        }
        if is_async_comp && !self.in_async_context() {
            return false;
        }
        let mut has_walrus = false;
        collect_comp_scope_walruses(elt, value, generators, &mut |_| has_walrus = true);
        if has_walrus {
            return false;
        }
        // A comprehension-scope `yield`/`yield from` is a SyntaxError
        // regardless of inlining; take the nested-function path whose
        // Comprehension-kind compiler reports it ("'yield' inside list
        // comprehension", test_grammar test_yield_in_comprehensions).
        // Inlining instead would compile the yield into the enclosing
        // function, where it is legal.
        if comp_scope_contains_yield(elt, value, generators) {
            return false;
        }
        let mut locals_map: IndexMap<String, Binding> = IndexMap::new();
        for g in generators {
            let mut names = HashSet::new();
            collect_target_names(&g.target, &mut names);
            for n in names {
                locals_map.insert(n, Binding::Local);
            }
        }
        let mut needed = HashSet::new();
        collect_inner_free_expr(elt, &locals_map, &mut needed);
        if let Some(v) = value {
            collect_inner_free_expr(v, &locals_map, &mut needed);
        }
        for (gi, g) in generators.iter().enumerate() {
            if gi > 0 {
                collect_inner_free_expr(&g.iter, &locals_map, &mut needed);
            }
            collect_inner_free_expr(&g.target, &locals_map, &mut needed);
            for cond in &g.ifs {
                collect_inner_free_expr(cond, &locals_map, &mut needed);
            }
        }
        if !needed.is_empty() {
            return false;
        }
        for name in locals_map.keys() {
            if matches!(
                self.bindings.get(name),
                Some(Binding::Cell | Binding::Free | Binding::Nonlocal | Binding::ClassPassthrough)
            ) {
                return false;
            }
        }
        true
    }

    /// PEP 709 inlined lowering (CPython codegen_comprehension with
    /// an inlined symtable entry): the loop compiles into the current
    /// stream. The comp's for-targets become hidden fast locals of the
    /// enclosing scope, saved with LOAD_FAST_AND_CLEAR before the loop
    /// and restored after — including on the exception path, via a
    /// cleanup handler that re-raises.
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
        // Comp-locals in first-seen order (deterministic co_varnames).
        let mut names: Vec<String> = Vec::new();
        for g in generators {
            collect_target_names_ordered(&g.target, &mut names);
        }
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
        // Shadow the comp-locals for the duration of the body.
        let mut overrides: Vec<(String, Option<Binding>)> = Vec::new();
        for n in &names {
            let prev = self.bindings.insert(n.clone(), Binding::Local);
            overrides.push((n.clone(), prev));
        }
        let slots: Vec<u32> = names.iter().map(|n| self.var_index_or_add(n)).collect();
        // Save and clear each hidden local, keeping the iterator on top.
        for &s in &slots {
            self.emit(OpCode::LoadFastAndClear, s);
            self.emit(OpCode::Swap, 2);
        }
        // Accumulator goes *under* the iterator; this is where the
        // protected region begins (CPython's L1).
        let protect_start = self.next_offset();
        self.emit(collector_op, 0);
        self.emit(OpCode::Swap, 2);
        self.inline_comp += 1;
        let body = compile_comp_body(self, generators, 0, 1, elt, value, append_op);
        self.inline_comp -= 1;
        body?;
        // Loop done: stack is [saved.., accumulator].
        let protect_end = self.next_offset();
        self.current_line = comp_line;
        self.current_span = comp_span;
        if !slots.is_empty() {
            // Normal-path restore.
            for &s in slots.iter().rev() {
                self.emit(OpCode::Swap, 2);
                self.emit(OpCode::StoreFast, s);
            }
            let over = self.emit_no_line(OpCode::JumpForward, 0);
            self.synthetic_jumps.insert(over);
            // Exception-path restore: [saved.., acc, exc] → drop the
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
            for &s in slots.iter().rev() {
                self.emit(OpCode::Swap, 2);
                self.emit(OpCode::StoreFast, s);
            }
            self.emit(OpCode::Reraise, 0);
            let after = self.next_offset();
            self.patch_jump(over, after);
        }
        for (n, prev) in overrides {
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
        if self.comp_inline_eligible(kind, elt, value, generators, is_async_comp) {
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
        inner.current_line = self.current_line;
        inner.comp_kind = Some(kind);
        inner.private = self.private.clone();
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
            let mut needed_in_inner: HashSet<String> = HashSet::new();
            collect_inner_free_expr(elt, &inner.bindings, &mut needed_in_inner);
            if let Some(v) = value {
                collect_inner_free_expr(v, &inner.bindings, &mut needed_in_inner);
            }
            for (gi, g) in generators.iter().enumerate() {
                // generators[0].iter is evaluated in the *enclosing*
                // scope (passed in as `.0`); every later iter, every
                // filter, and every *target sub-expression* (a nested
                // comprehension can sit in a subscripted target —
                // `for a[[x for x in [1] if _C][0]] in …` — and close
                // over this comprehension's variables) runs inside
                // this comprehension.
                if gi > 0 {
                    collect_inner_free_expr(&g.iter, &inner.bindings, &mut needed_in_inner);
                }
                collect_inner_free_expr(&g.target, &inner.bindings, &mut needed_in_inner);
                for cond in &g.ifs {
                    collect_inner_free_expr(cond, &inner.bindings, &mut needed_in_inner);
                }
            }
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
        if let Some(op) = collector_op {
            inner.emit(op, 0);
        }
        // Outermost iterator comes in as `.0`. CPython 3.13 re-iters it
        // defensively (`LOAD_FAST .0; GET_ITER` — the argument is any
        // iterable as far as the frame surface is concerned); the async
        // depth-0 arm converts with GET_AITER inside the body instead.
        inner.emit(OpCode::LoadFast, 0);
        if !generators[0].is_async {
            inner.emit(OpCode::GetIter, 0);
        }
        compile_comp_body(&mut inner, generators, 0, 1, elt, value, append_op)?;
        if matches!(kind, CompKind::Generator) {
            // ForIter pops the iterator on exhaustion. Return None
            // so the generator finishes cleanly (the VM converts
            // this to `StopIteration`). arg 1: codegen-origin constant
            // return, fused to RETURN_CONST on the wire
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
            inner.emit(OpCode::ReturnValue, 1);
        } else {
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
        // Push iterator of outermost generator as `.0`. For an async
        // comprehension we still pass the raw source — the inner
        // body fetches `aiter()` when it sees `is_async`.
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
        if !(is_async_comp && generators[0].is_async) {
            self.emit(OpCode::GetIter, 0);
        }
        // The iterator rides the self slot (CPython's comprehension
        // invocation is `CALL 0`).
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
            ExprKind::JoinedStr(parts) => {
                for p in parts {
                    visit(p, out);
                }
            }
            ExprKind::FormattedValue {
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
            ExprKind::JoinedStr(parts) => {
                for p in parts {
                    scan(p)?;
                }
                Ok(())
            }
            ExprKind::FormattedValue {
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
            ExprKind::JoinedStr(parts) => {
                for p in parts {
                    visit(p, in_class_body, stack)?;
                }
                Ok(())
            }
            ExprKind::FormattedValue {
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
fn stamp_comp_elt_span(inner: &mut Compiler, elt: &Expr, value: Option<&Expr>, append_op: OpCode) {
    let end = match (append_op, value) {
        (OpCode::MapAdd, Some(v)) => v.span.end.0,
        _ => elt.span.end.0,
    };
    inner.current_span = (elt.span.start.0, end);
    inner.set_line_from(elt.span.start.0);
}

fn compile_comp_body(
    inner: &mut Compiler,
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
                stamp_comp_elt_span(inner, elt, value, append_op);
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
        // depth==0: caller pushed the source expr (not yet GetAiter'd)
        // because compile_comprehension uses GetIter for the .0 arg.
        // We need to convert to async-iter here for the body. An
        // *inlined* comprehension pushed the ready aiter instead —
        // nothing to convert.
        if depth == 0 && inner.inline_comp == 0 {
            inner.emit(OpCode::PopTop, 0);
            inner.emit(OpCode::LoadFast, 0);
            inner.emit(OpCode::GetAiter, 0);
            inner.emit(OpCode::CopyTop, 0);
            inner.emit(OpCode::StoreFast, 0);
        } else if depth > 0 {
            inner.compile_expr(&gen.iter)?;
            inner.emit(OpCode::GetAiter, 0);
        }
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
        let loop_top = inner.next_offset();
        inner.emit(OpCode::GetAnext, 0);
        inner.emit_send_dance(3);
        // As in `compile_async_for`: only the `__anext__` await may end
        // the loop via StopAsyncIteration (bpo-44895).
        let dance_end = inner.next_offset();
        inner.compile_assign(&gen.target)?;
        let mut filter_jumps = Vec::new();
        for cond in &gen.ifs {
            let (c, invert) = strip_not_chain(cond);
            inner.compile_expr(c)?;
            if !expr_is_bool(c) {
                inner.emit(OpCode::ToBool, 0);
            }
            // The comp-`if` jump lands on the elt-located back edge; in
            // CPython, jump threading gives it that same location.
            stamp_comp_elt_span(inner, elt, value, append_op);
            let jf = inner.emit(
                if invert {
                    OpCode::PopJumpIfTrue
                } else {
                    OpCode::PopJumpIfFalse
                },
                0,
            );
            filter_jumps.push(jf);
        }
        compile_comp_body(
            inner,
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
        stamp_comp_elt_span(inner, elt, value, append_op);
        let back = inner.emit(OpCode::JumpBackward, 0);
        inner.patch_jump(back, loop_top);
        let cleanup_target = inner.next_offset();
        inner.co.exception_table.push(ExcHandler {
            start: loop_top,
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
                let (c, invert) = strip_not_chain(cond);
                inner.compile_expr(c)?;
                if !expr_is_bool(c) {
                    inner.emit(OpCode::ToBool, 0);
                }
                stamp_comp_elt_span(inner, elt, value, append_op);
                let jf = inner.emit(
                    if invert {
                        OpCode::PopJumpIfTrue
                    } else {
                        OpCode::PopJumpIfFalse
                    },
                    0,
                );
                filter_jumps.push(jf);
            }
            // No new iterator on the stack: the body runs exactly once.
            compile_comp_body(
                inner,
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
        let (c, invert) = strip_not_chain(cond);
        inner.compile_expr(c)?;
        if !expr_is_bool(c) {
            inner.emit(OpCode::ToBool, 0);
        }
        // As in the async arm: the comp-`if` jump takes the element
        // span its back-edge target carries (CPython jump threading).
        stamp_comp_elt_span(inner, elt, value, append_op);
        let jf = inner.emit(
            if invert {
                OpCode::PopJumpIfTrue
            } else {
                OpCode::PopJumpIfFalse
            },
            0,
        );
        filter_jumps.push(jf);
    }
    let iters_here = if depth == 0 {
        iters_on_stack
    } else {
        iters_on_stack + 1
    };
    compile_comp_body(
        inner,
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
    stamp_comp_elt_span(inner, elt, value, append_op);
    let back = inner.emit(OpCode::JumpBackward, 0);
    inner.patch_jump(back, loop_top);
    let after = inner.next_offset();
    inner.patch_jump(for_site, after);
    // Keep END_FOR on the iterator line (see statement-level for loop) so a
    // comprehension's loop exhaustion does not emit a spurious `line` event.
    inner.set_span(gen.iter.span);
    inner.current_line = for_line;
    // END_FOR + POP_TOP, as in the statement-level loop above.
    inner.emit(OpCode::EndFor, 0);
    inner.emit(OpCode::PopTop, 0);
    Ok(())
}

/// Strip a chain of `not`s off a branch condition, returning the
/// innermost operand and whether the branch sense is inverted.
/// CPython's `compiler_jump_if` (Not_kind case) compiles `if not x:`
/// as a test of `x` with the opposite jump instead of emitting
/// UNARY_NOT + POP_JUMP_IF_FALSE; test_peepholer's `test_unot`
/// grades this shape.
/// `true` if the expression statically produces an exact `bool`, so a
/// conditional jump on it needs no `TO_BOOL`. Mirrors CPython's
/// post-optimization shape: codegen emits `TO_BOOL` before every
/// `POP_JUMP_IF_*`, then `optimize_basic_block` folds it into
/// `COMPARE_OP`'s bool bit / drops it after `IS_OP`, `CONTAINS_OP`,
/// `UNARY_NOT`, and bool constants.
fn expr_is_bool(e: &Expr) -> bool {
    match &e.kind {
        ExprKind::Compare { .. } => true,
        ExprKind::UnaryOp {
            op: UnaryOp::Not, ..
        } => true,
        // Any constant: truthiness is static — CPython's optimizer
        // folds `TO_BOOL` after `LOAD_CONST` into the bool constant,
        // and `fold_const_branches` here resolves the branch entirely.
        ExprKind::Constant(_) => true,
        // The value-shape short circuit leaves whichever operand was
        // selected: bool only if every operand is.
        ExprKind::BoolOp { values, .. } => values.iter().all(expr_is_bool),
        ExprKind::NamedExpr { value, .. } => expr_is_bool(value),
        _ => false,
    }
}

fn strip_not_chain(mut test: &Expr) -> (&Expr, bool) {
    let mut invert = false;
    while let ExprKind::UnaryOp {
        op: UnaryOp::Not,
        operand,
    } = &test.kind
    {
        invert = !invert;
        test = operand;
    }
    (test, invert)
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
    };
    FinallyFrame {
        kind,
        loop_depth_at_push: f.loop_depth_at_push,
        id: f.id,
        pop_except_after: f.pop_except_after,
        exc_at_push: f.exc_at_push,
        handler_at_push: f.handler_at_push,
        rv_at_push: f.rv_at_push,
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
/// excluded (they're hidden-scope locals).
fn collect_pep695_header_reads(stmt: &Stmt, out: &mut HashSet<String>) {
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
        _ => return,
    };
    if type_params.is_empty() {
        return;
    }
    let mut reads = HashSet::new();
    for tp in type_params {
        if let TypeParamKind::TypeVar { bound: Some(b) } = &tp.kind {
            collect_reads_expr(b, &mut reads);
        }
        if let Some(d) = &tp.default {
            collect_reads_expr(d, &mut reads);
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
                collect_reads_expr(ann, &mut reads);
            }
        }
        if let Some(r) = returns {
            collect_reads_expr(r, &mut reads);
        }
    } else if let StmtKind::ClassDef {
        bases, keywords, ..
    } = &stmt.kind
    {
        // A generic class's bases and keywords move into the hidden
        // scope, so their reads flow through it too (`def f(): T = str;
        // class C: class D[U](T): ...` needs `T` forwarded).
        for b in bases {
            collect_reads_expr(b, &mut reads);
        }
        for k in keywords {
            collect_reads_expr(&k.value, &mut reads);
        }
    }
    let own: HashSet<&str> = type_params.iter().map(|tp| tp.name.as_str()).collect();
    for r in reads {
        if !own.contains(r.as_str()) {
            out.insert(r);
        }
    }
}

/// `true` if a class body statement will compile a PEP 695 annotation
/// scope (a generic `def`/`class` header, or a `type` alias's
/// desugared [`ExprKind::TypeParamFn`] thunks) — those scopes close
/// over the class body's `__classdict__` cell. Nested `def`/`class`
/// *bodies* are their own scopes and are not descended into.
fn stmt_needs_classdict(stmt: &Stmt) -> bool {
    match &stmt.kind {
        StmtKind::FunctionDef { type_params, .. }
        | StmtKind::AsyncFunctionDef { type_params, .. }
        | StmtKind::ClassDef { type_params, .. } => !type_params.is_empty(),
        StmtKind::Assign { value, .. } => expr_contains_typeparamfn(value),
        StmtKind::Expr(e) | StmtKind::Return(Some(e)) => expr_contains_typeparamfn(e),
        StmtKind::AnnAssign { value, .. } => value.as_ref().is_some_and(expr_contains_typeparamfn),
        StmtKind::If { body, orelse, .. } | StmtKind::While { body, orelse, .. } => {
            body.iter().chain(orelse).any(stmt_needs_classdict)
        }
        StmtKind::For { body, orelse, .. } | StmtKind::AsyncFor { body, orelse, .. } => {
            body.iter().chain(orelse).any(stmt_needs_classdict)
        }
        StmtKind::With { body, .. } | StmtKind::AsyncWith { body, .. } => {
            body.iter().any(stmt_needs_classdict)
        }
        StmtKind::Try {
            body,
            handlers,
            orelse,
            finalbody,
        } => {
            body.iter()
                .chain(orelse)
                .chain(finalbody)
                .any(stmt_needs_classdict)
                || handlers
                    .iter()
                    .any(|h| h.body.iter().any(stmt_needs_classdict))
        }
        StmtKind::Match { cases, .. } => cases
            .iter()
            .any(|c| c.body.iter().any(stmt_needs_classdict)),
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
        ExprKind::JoinedStr(parts) => parts.iter().for_each(&mut check),
        ExprKind::FormattedValue {
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
) {
    collect_pep695_header_reads(stmt, out);
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
            // Decorators, default values, and annotations evaluate in the
            // *enclosing* scope, but may themselves contain nested scopes
            // (`@lambda f: null(f)` — PEP 614) that close over our locals.
            for d in decorator_list {
                collect_inner_free_expr(d, outer_bindings, out);
            }
            for d in args
                .defaults
                .iter()
                .chain(args.kw_defaults.iter().flatten())
            {
                collect_inner_free_expr(d, outer_bindings, out);
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
                        collect_inner_free_expr(ann, outer_bindings, out);
                    }
                }
                if let Some(r) = returns {
                    collect_inner_free_expr(r, outer_bindings, out);
                }
            }
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
                collect_reads_stmt(s, &mut inner_reads);
            }
            for r in inner_reads {
                if !inner_locals.contains(&r) && !inner_globals.contains(&r) {
                    out.insert(r);
                }
            }
            // Recurse into inner function bodies — their inner
            // functions may pull names from us too.
            for s in body {
                collect_inner_free(s, outer_bindings, out);
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
                collect_inner_free_expr(d, outer_bindings, out);
            }
            for b in bases {
                collect_inner_free_expr(b, outer_bindings, out);
            }
            for k in keywords {
                collect_inner_free_expr(&k.value, outer_bindings, out);
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
            for s in body {
                collect_assigned(s, &mut class_assigned);
            }
            // Names referenced *anywhere* in the class body (including
            // method bodies) that aren't bound inside the class are
            // candidates for outer-scope free promotion.
            let mut class_reads = HashSet::new();
            for s in body {
                collect_reads_stmt(s, &mut class_reads);
            }
            for r in class_reads {
                if !class_assigned.contains(&r) {
                    out.insert(r);
                }
            }
            for s in body {
                collect_inner_free(s, outer_bindings, out);
            }
        }
        StmtKind::If { test, body, orelse } | StmtKind::While { test, body, orelse } => {
            collect_inner_free_expr(test, outer_bindings, out);
            for s in body {
                collect_inner_free(s, outer_bindings, out);
            }
            for s in orelse {
                collect_inner_free(s, outer_bindings, out);
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
            collect_inner_free_expr(target, outer_bindings, out);
            collect_inner_free_expr(iter, outer_bindings, out);
            for s in body {
                collect_inner_free(s, outer_bindings, out);
            }
            for s in orelse {
                collect_inner_free(s, outer_bindings, out);
            }
        }
        StmtKind::Try {
            body,
            handlers,
            orelse,
            finalbody,
        } => {
            for s in body {
                collect_inner_free(s, outer_bindings, out);
            }
            for h in handlers {
                if let Some(t) = &h.type_ {
                    collect_inner_free_expr(t, outer_bindings, out);
                }
                for s in &h.body {
                    collect_inner_free(s, outer_bindings, out);
                }
            }
            for s in orelse {
                collect_inner_free(s, outer_bindings, out);
            }
            for s in finalbody {
                collect_inner_free(s, outer_bindings, out);
            }
        }
        StmtKind::Raise { exc, cause } => {
            if let Some(e) = exc {
                collect_inner_free_expr(e, outer_bindings, out);
            }
            if let Some(c) = cause {
                collect_inner_free_expr(c, outer_bindings, out);
            }
        }
        StmtKind::With { items, body } | StmtKind::AsyncWith { items, body } => {
            for it in items {
                collect_inner_free_expr(&it.context_expr, outer_bindings, out);
            }
            for s in body {
                collect_inner_free(s, outer_bindings, out);
            }
        }
        StmtKind::Expr(e) | StmtKind::Return(Some(e)) => {
            collect_inner_free_expr(e, outer_bindings, out);
        }
        StmtKind::Assign { value, .. } => {
            collect_inner_free_expr(value, outer_bindings, out);
        }
        StmtKind::AugAssign { value, .. }
        | StmtKind::AnnAssign {
            value: Some(value), ..
        } => {
            collect_inner_free_expr(value, outer_bindings, out);
        }
        StmtKind::Assert { test, msg } => {
            // `assert <comp> [, <comp>]` evaluates both expressions in this
            // scope. A comprehension here captures our locals just like one
            // in an `Expr`/`Assign` statement, so its outer reads must drive
            // cell promotion — otherwise the pre-pass leaves the name a plain
            // local (STORE_FAST) while `compile_comprehension` later promotes
            // it to a cell, and the comp-call reads an unfilled cell
            // (`UnboundLocalError`). Mirrors `collect_reads_stmt`.
            collect_inner_free_expr(test, outer_bindings, out);
            if let Some(m) = msg {
                collect_inner_free_expr(m, outer_bindings, out);
            }
        }
        StmtKind::Delete(targets) => {
            // `del x[<comp>]` / `del x.attr` evaluate the container/slice in
            // this scope; a comprehension in a subscript captures our locals.
            for t in targets {
                collect_inner_free_expr(t, outer_bindings, out);
            }
        }
        StmtKind::Match { subject, cases } => {
            // The subject and every guard are ordinary expressions evaluated
            // in this scope and may contain capturing comprehensions; case
            // bodies are statements that recurse normally.
            collect_inner_free_expr(subject, outer_bindings, out);
            for c in cases {
                if let Some(g) = &c.guard {
                    collect_inner_free_expr(g, outer_bindings, out);
                }
                for s in &c.body {
                    collect_inner_free(s, outer_bindings, out);
                }
            }
        }
        _ => {}
    }
}

/// `True` when a method body references `super` or `__class__` so the
/// compiler knows to capture the class's `__class__` cell.
fn method_references_class(body: &[Stmt]) -> bool {
    let mut reads = HashSet::new();
    for s in body {
        collect_reads_stmt(s, &mut reads);
    }
    if reads.contains("super") || reads.contains("__class__") {
        return true;
    }
    // `nonlocal __class__` (test_super's pathology-repair tearDown) binds
    // the implicit class cell for *writing* without ever reading it —
    // CPython's symtable treats the declaration itself as a use.
    let mut globals = HashSet::new();
    let mut nonlocals = HashSet::new();
    let mut assigned = HashSet::new();
    for s in body {
        collect_decls(s, &mut globals, &mut nonlocals, &mut assigned);
    }
    nonlocals.contains("__class__")
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
            ExprKind::JoinedStr(parts) => parts.iter().any(expr_hit),
            ExprKind::FormattedValue {
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
        ExprKind::JoinedStr(parts) => parts.iter().any(expr_contains_yield),
        ExprKind::FormattedValue {
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
        ExprKind::JoinedStr(parts) => parts.iter().any(expr_yields_in_scope),
        ExprKind::FormattedValue {
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
        ExprKind::JoinedStr(parts) => parts.iter().any(expr_contains_await),
        ExprKind::FormattedValue {
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
        ExprKind::JoinedStr(parts) => parts.iter().any(expr_contains_async_comp),
        ExprKind::FormattedValue {
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

fn collect_inner_free_expr(
    expr: &Expr,
    outer_bindings: &IndexMap<String, Binding>,
    out: &mut HashSet<String>,
) {
    match &expr.kind {
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
                    collect_inner_free_expr(&g.iter, outer_bindings, out);
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
                    collect_inner_free_expr(&g.iter, outer_bindings, out);
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
            collect_inner_free_expr(func, outer_bindings, out);
            for a in args {
                collect_inner_free_expr(a, outer_bindings, out);
            }
            for k in keywords {
                collect_inner_free_expr(&k.value, outer_bindings, out);
            }
        }
        ExprKind::BinOp { left, right, .. } => {
            collect_inner_free_expr(left, outer_bindings, out);
            collect_inner_free_expr(right, outer_bindings, out);
        }
        ExprKind::BoolOp { values, .. } => {
            for v in values {
                collect_inner_free_expr(v, outer_bindings, out);
            }
        }
        ExprKind::UnaryOp { operand, .. } => collect_inner_free_expr(operand, outer_bindings, out),
        ExprKind::Compare {
            left, comparators, ..
        } => {
            collect_inner_free_expr(left, outer_bindings, out);
            for c in comparators {
                collect_inner_free_expr(c, outer_bindings, out);
            }
        }
        ExprKind::IfExp { test, body, orelse } => {
            collect_inner_free_expr(test, outer_bindings, out);
            collect_inner_free_expr(body, outer_bindings, out);
            collect_inner_free_expr(orelse, outer_bindings, out);
        }
        ExprKind::Tuple(items) | ExprKind::List(items) | ExprKind::Set(items) => {
            for x in items {
                collect_inner_free_expr(x, outer_bindings, out);
            }
        }
        ExprKind::Dict { keys, values } => {
            for k in keys.iter().flatten() {
                collect_inner_free_expr(k, outer_bindings, out);
            }
            for v in values {
                collect_inner_free_expr(v, outer_bindings, out);
            }
        }
        ExprKind::Attribute { value, .. } | ExprKind::Starred(value) => {
            collect_inner_free_expr(value, outer_bindings, out)
        }
        ExprKind::Subscript { value, slice } => {
            collect_inner_free_expr(value, outer_bindings, out);
            collect_inner_free_expr(slice, outer_bindings, out);
        }
        ExprKind::FormattedValue {
            value, format_spec, ..
        } => {
            collect_inner_free_expr(value, outer_bindings, out);
            if let Some(fs) = format_spec.as_deref() {
                collect_inner_free_expr(fs, outer_bindings, out);
            }
        }
        ExprKind::JoinedStr(parts) => {
            for p in parts {
                collect_inner_free_expr(p, outer_bindings, out);
            }
        }
        ExprKind::Slice { lower, upper, step } => {
            for x in [lower.as_deref(), upper.as_deref(), step.as_deref()]
                .into_iter()
                .flatten()
            {
                collect_inner_free_expr(x, outer_bindings, out);
            }
        }
        // `await`, `yield`, and `yield from` are arbitrary
        // expressions that can themselves reference outer-scope
        // locals — recurse so the comprehension / lambda detection
        // upstream sees those reads. NamedExpr (walrus `:=`) carries
        // a value subtree that needs the same treatment.
        ExprKind::Await(v) | ExprKind::YieldFrom(v) => {
            collect_inner_free_expr(v, outer_bindings, out);
        }
        ExprKind::Yield(value) => {
            if let Some(v) = value {
                collect_inner_free_expr(v, outer_bindings, out);
            }
        }
        ExprKind::NamedExpr { value, .. } => {
            collect_inner_free_expr(value, outer_bindings, out);
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
            StmtKind::AugAssign { target: t, .. } | StmtKind::AnnAssign { target: t, .. } => {
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
        | StmtKind::ClassDef { name, .. } => {
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
        ExprKind::JoinedStr(parts) => {
            for p in parts {
                collect_walrus_expr(p, out);
            }
        }
        ExprKind::FormattedValue {
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
        | StmtKind::ClassDef { name, .. } => {
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

/// Fold an all-constant tuple display (nested tuples included) into a
/// pooled [`Constant::Tuple`], mirroring CPython's AST optimizer.
/// Returns `None` when any element is non-constant.
fn fold_const_tuple(items: &[Expr]) -> Option<Constant> {
    let mut out = Vec::with_capacity(items.len());
    for x in items {
        match &x.kind {
            ExprKind::Constant(c) => out.push(c.clone().into()),
            ExprKind::Tuple(inner) => out.push(fold_const_tuple(inner)?),
            _ => return None,
        }
    }
    Some(Constant::Tuple(out))
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
            // must not participate in scope analysis.
            if !pep563_active() {
                collect_reads_expr(annotation, out);
            }
            if let Some(v) = value {
                collect_reads_expr(v, out);
            }
        }
        StmtKind::If { test, body, orelse } | StmtKind::While { test, body, orelse } => {
            collect_reads_expr(test, out);
            for s in body {
                collect_reads_stmt(s, out);
            }
            for s in orelse {
                collect_reads_stmt(s, out);
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
                collect_reads_stmt(s, out);
            }
            for s in orelse {
                collect_reads_stmt(s, out);
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
                collect_reads_stmt(s, &mut nested_reads);
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
                collect_reads_stmt(s, out);
            }
        }
        StmtKind::Try {
            body,
            handlers,
            orelse,
            finalbody,
        } => {
            for s in body {
                collect_reads_stmt(s, out);
            }
            for h in handlers {
                if let Some(t) = &h.type_ {
                    collect_reads_expr(t, out);
                }
                for s in &h.body {
                    collect_reads_stmt(s, out);
                }
            }
            for s in orelse {
                collect_reads_stmt(s, out);
            }
            for s in finalbody {
                collect_reads_stmt(s, out);
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
                collect_reads_stmt(s, out);
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
                    collect_reads_stmt(s, out);
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

/// Recursively collect every name *referenced* by `expr`, ignoring
/// what would normally be considered "outer scope only" — i.e. dive
/// into lambda bodies and every part of comprehensions. Used by the
/// outer scope to identify what names its inner closures will need to
/// promote to cells.
fn collect_reads_deep(expr: &Expr, out: &mut HashSet<String>) {
    match &expr.kind {
        ExprKind::Name(n) => {
            out.insert(n.clone());
        }
        ExprKind::Attribute { value, .. } | ExprKind::Starred(value) => {
            collect_reads_deep(value, out);
        }
        ExprKind::Subscript { value, slice } => {
            collect_reads_deep(value, out);
            collect_reads_deep(slice, out);
        }
        ExprKind::Slice { lower, upper, step } => {
            for x in [lower.as_deref(), upper.as_deref(), step.as_deref()]
                .into_iter()
                .flatten()
            {
                collect_reads_deep(x, out);
            }
        }
        ExprKind::BinOp { left, right, .. } => {
            collect_reads_deep(left, out);
            collect_reads_deep(right, out);
        }
        ExprKind::BoolOp { values, .. } => {
            for v in values {
                collect_reads_deep(v, out);
            }
        }
        ExprKind::UnaryOp { operand, .. } => collect_reads_deep(operand, out),
        ExprKind::Compare {
            left, comparators, ..
        } => {
            collect_reads_deep(left, out);
            for c in comparators {
                collect_reads_deep(c, out);
            }
        }
        ExprKind::IfExp { test, body, orelse } => {
            collect_reads_deep(test, out);
            collect_reads_deep(body, out);
            collect_reads_deep(orelse, out);
        }
        ExprKind::NamedExpr { target, value } => {
            collect_reads_deep(target, out);
            collect_reads_deep(value, out);
        }
        ExprKind::Call {
            func,
            args,
            keywords,
        } => {
            collect_reads_deep(func, out);
            for a in args {
                collect_reads_deep(a, out);
            }
            for k in keywords {
                collect_reads_deep(&k.value, out);
            }
        }
        ExprKind::Lambda { args, body } | ExprKind::TypeParamFn { args, body } => {
            for d in &args.defaults {
                collect_reads_deep(d, out);
            }
            for d in args.kw_defaults.iter().flatten() {
                collect_reads_deep(d, out);
            }
            collect_reads_deep(body, out);
        }
        ExprKind::Tuple(items) | ExprKind::List(items) | ExprKind::Set(items) => {
            for x in items {
                collect_reads_deep(x, out);
            }
        }
        ExprKind::Dict { keys, values } => {
            for k in keys.iter().flatten() {
                collect_reads_deep(k, out);
            }
            for v in values {
                collect_reads_deep(v, out);
            }
        }
        ExprKind::ListComp { elt, generators }
        | ExprKind::SetComp { elt, generators }
        | ExprKind::GeneratorExp { elt, generators } => {
            collect_reads_deep(elt, out);
            for g in generators {
                collect_reads_deep(&g.iter, out);
                collect_reads_deep(&g.target, out);
                for i in &g.ifs {
                    collect_reads_deep(i, out);
                }
            }
        }
        ExprKind::DictComp {
            key,
            value,
            generators,
        } => {
            collect_reads_deep(key, out);
            collect_reads_deep(value, out);
            for g in generators {
                collect_reads_deep(&g.iter, out);
                collect_reads_deep(&g.target, out);
                for i in &g.ifs {
                    collect_reads_deep(i, out);
                }
            }
        }
        // `await`, `yield`, `yield from`, and f-string parts can each
        // carry name reads in arbitrarily nested positions. They were
        // historically ignored here — which silently dropped free
        // variables used only inside an `await` from the outer
        // scope's "needs a cell" set, so a comprehension referencing
        // `val` only inside `await f(val)` would close over an
        // unfilled cell. Recurse like every other compound form.
        ExprKind::Yield(value) => {
            if let Some(v) = value {
                collect_reads_deep(v, out);
            }
        }
        ExprKind::YieldFrom(v) | ExprKind::Await(v) => {
            collect_reads_deep(v, out);
        }
        ExprKind::JoinedStr(parts) => {
            for p in parts {
                collect_reads_deep(p, out);
            }
        }
        ExprKind::FormattedValue {
            value, format_spec, ..
        } => {
            collect_reads_deep(value, out);
            if let Some(fs) = format_spec.as_deref() {
                collect_reads_deep(fs, out);
            }
        }
        ExprKind::Constant(_) => {}
    }
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
            collect_reads_expr(body, out);
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
            // Outermost iterator evaluates in the outer scope.
            if let Some(first) = generators.first() {
                collect_reads_expr(&first.iter, out);
            }
            for g in generators.iter().skip(1) {
                collect_reads_expr(&g.iter, out);
            }
            // Names free in the comprehension body propagate to the
            // enclosing scope (CPython symtable). A non-name target
            // (`for tgt[0] in …`) reads its container; filters read
            // their condition.
            for g in generators {
                collect_reads_assign_target(&g.target, out);
                for i in &g.ifs {
                    collect_reads_expr(i, out);
                }
            }
            collect_reads_expr(elt, out);
        }
        ExprKind::DictComp {
            key,
            value,
            generators,
        } => {
            if let Some(first) = generators.first() {
                collect_reads_expr(&first.iter, out);
            }
            for g in generators.iter().skip(1) {
                collect_reads_expr(&g.iter, out);
            }
            for g in generators {
                collect_reads_assign_target(&g.target, out);
                for i in &g.ifs {
                    collect_reads_expr(i, out);
                }
            }
            collect_reads_expr(key, out);
            collect_reads_expr(value, out);
        }
        ExprKind::FormattedValue {
            value, format_spec, ..
        } => {
            collect_reads_expr(value, out);
            if let Some(fs) = format_spec.as_deref() {
                collect_reads_expr(fs, out);
            }
        }
        ExprKind::JoinedStr(parts) => {
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

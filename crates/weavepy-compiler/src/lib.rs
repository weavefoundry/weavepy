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

use indexmap::IndexMap;
use thiserror::Error;
use weavepy_parser::ast::{
    Arguments as AstArguments, BinOp, BoolOp, CmpOp, Comprehension, Constant as AstConstant,
    ExceptHandler, Expr, ExprKind, Keyword as KwArg, MatchCase, Module, Pattern, Stmt, StmtKind,
    UnaryOp, WithItem,
};

pub mod bytecode;
pub mod cpython_code;

pub use bytecode::{
    BinOpKind, CacheTable, CompareKind, InlineCache, Instruction, OpCode, UnaryKind,
    BINARY_OP_INPLACE_FLAG, COOLDOWN,
};
pub use cpython_code::{CpythonCode, Position};

// ---------- error type ----------

#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum CompileError {
    #[error("`{0}` is not a valid assignment target")]
    BadAssignmentTarget(String),
    /// A syntax error whose message must match CPython verbatim
    /// (doctests assert on these strings).
    #[error("{0}")]
    SyntaxExact(String),
    #[error("`break` outside loop")]
    BreakOutsideLoop,
    #[error("`continue` outside loop")]
    ContinueOutsideLoop,
    #[error("`return` outside function")]
    ReturnOutsideFunction,
    /// A `yield` / `yield from` expression outside a function body.
    /// `{0}` is the keyword (`yield` or `yield from`) so the message
    /// matches CPython's `SyntaxError: 'yield' outside function`.
    #[error("'{0}' outside function")]
    YieldOutsideFunction(&'static str),
    #[error("`{0}` is not yet supported by the compiler ({1})")]
    NotImplemented(&'static str, &'static str),
    #[error("internal compiler error: {0}")]
    Internal(String),
}

// ---------- code object ----------

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
        Constant::Bytes(_) => "b'...'".to_owned(),
        Constant::Tuple(items) => {
            let inner: Vec<_> = items.iter().map(format_constant).collect();
            format!("({})", inner.join(", "))
        }
        Constant::Code(co) => format!("<code object {}>", co.name),
        Constant::Ellipsis => "Ellipsis".to_owned(),
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
    Bytes(Vec<u8>),
    Tuple(Vec<Constant>),
    Code(Box<CodeObject>),
    Ellipsis,
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
            (C::Bytes(a), C::Bytes(b)) => a == b,
            (C::Tuple(a), C::Tuple(b)) => a == b,
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
            AstConstant::Bytes(b) => Self::Bytes(b),
            AstConstant::Tuple(xs) => Self::Tuple(xs.into_iter().map(Self::from).collect()),
            AstConstant::Ellipsis => Self::Ellipsis,
        }
    }
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
            StmtKind::ImportFrom { module: Some(m), names, .. }
                if m == "__future__" && names.iter().any(|a| a.name == "annotations")
        )
    })
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
    let line_index = LineIndex::new(source);
    let mut top = Compiler::new(
        "<module>".to_owned(),
        filename.to_owned(),
        CodeKind::Module,
        Rc::new(line_index),
        Rc::from(source),
        has_future_annotations(module),
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
    let line_index = LineIndex::new(source);
    let mut top = Compiler::new(
        "<module>".to_owned(),
        filename.to_owned(),
        CodeKind::Module,
        Rc::new(line_index),
        Rc::from(source),
        has_future_annotations(module),
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
    let line_index = LineIndex::new(source);
    let mut top = Compiler::new(
        "<module>".to_owned(),
        filename.to_owned(),
        CodeKind::Module,
        Rc::new(line_index),
        Rc::from(source),
        has_future_annotations(module),
    );
    top.eval_mode = true;
    top.compile_module_body(module)?;
    Ok(top.finish())
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
        let mut starts = vec![0u32];
        for (i, b) in source.bytes().enumerate() {
            if b == b'\n' {
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
}

// ---------- compiler ----------

struct Compiler {
    co: CodeObject,
    kind: CodeKind,
    /// Name → binding for the current scope.
    bindings: IndexMap<String, Binding>,
    /// Names declared `global` by an explicit `global` statement in this
    /// scope. A nested `def`/`class` whose name is in this set gets a bare
    /// `__qualname__` (CPython's `compiler_set_qualname` GLOBAL_EXPLICIT
    /// rule), which is what makes `global P; class P: ...` pickleable.
    explicit_globals: HashSet<String>,
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
    /// Monotonic counter for synthetic `with`-statement locals.
    with_counter: u32,
    /// Monotonic counter for synthetic locals used by `return` inside
    /// `try/finally` to preserve the value across the finally body.
    finally_counter: u32,
    /// Source byte→line table shared by every nested compiler from the
    /// same `compile_module_*` call.
    line_index: Rc<LineIndex>,
    /// Line number assigned to the next emitted instruction; updated as
    /// the compiler descends through the AST.
    current_line: u32,
    /// Source byte span `(start, end)` for the AST node currently being
    /// emitted. Drives PEP-657 column tracking in [`Self::emit`]. Updated
    /// at statement and expression granularity as the compiler descends.
    current_span: (u32, u32),
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
}

/// One pending `finally` clause. We hold the AST so `return`,
/// `break`, and `continue` can each inline a fresh copy of the
/// clause's bytecode before transferring control out.
enum FinallyKind {
    /// Body of a `finally:` clause; emitted by re-compiling the
    /// statements at the non-normal exit site.
    Stmts(Vec<Stmt>),
    /// Synthetic frame for a `with` block: emit
    /// `<cm_local>.__exit__(None, None, None)` directly using the
    /// stored fast-local index. We can't represent this as an AST
    /// Name node because the synthetic local name (".with_cm0")
    /// isn't a valid identifier and would fail name resolution.
    WithExit { cm_idx: u32 },
    /// Synthetic frame for an `async with` block: emit
    /// `await <aexit_local>(None, None, None)`. Mirrors `WithExit`
    /// but awaits the `__aexit__` coroutine, so a `return`/`break`/
    /// `continue` out of an `async with` body still runs the exit.
    AsyncWithExit { aexit_idx: u32 },
}

struct FinallyFrame {
    /// What this frame fires at non-normal exit.
    kind: FinallyKind,
    /// Length of `loop_stack` when this frame was pushed. Used to
    /// determine whether `break`/`continue` should run this finally
    /// (only if the relevant loop is outside the finally scope).
    loop_depth_at_push: usize,
}

impl Compiler {
    fn new(
        name: String,
        filename: String,
        kind: CodeKind,
        line_index: Rc<LineIndex>,
        source: Rc<str>,
        future_annotations: bool,
    ) -> Self {
        let mut co = CodeObject::default();
        // Default qualname == name; nested scopes overwrite this via
        // `compute_child_qualname` once the parent context is known.
        co.qualname = name.clone();
        co.name = name;
        co.filename = filename;
        co.is_class_body = matches!(kind, CodeKind::Class);
        Self {
            co,
            kind,
            bindings: IndexMap::new(),
            explicit_globals: HashSet::new(),
            free_order: Vec::new(),
            loop_stack: Vec::new(),
            finally_stack: Vec::new(),
            chain_counter: 0,
            with_counter: 0,
            finally_counter: 0,
            line_index,
            current_line: 0,
            current_span: (0, 0),
            exc_on_stack: 0,
            handler_depth: 0,
            inside_class_body: false,
            annotations_initialized: false,
            code_kind: kind,
            interactive: false,
            eval_mode: false,
            source,
            future_annotations,
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
        let none_idx = self.co.intern_constant(Constant::None);
        self.emit(OpCode::LoadConst, none_idx);
        self.emit(OpCode::ReturnValue, 0);
        // Place freevars (in declaration order) at the end of the
        // cells/freevars combined index space.
        self.co.freevars = self.free_order.clone();
        // RFC 0021: size the inline-cache side-table to match the
        // emitted instruction stream so the VM can index into it
        // without bounds checks on the hot path.
        self.co.caches.resize(self.co.instructions.len());
        self.co
    }

    fn emit(&mut self, op: OpCode, arg: u32) -> u32 {
        let offset = self.co.instructions.len() as u32;
        self.co.instructions.push(Instruction { op, arg });
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
                StmtKind::Break | StmtKind::Continue => {
                    if !in_loop {
                        return Err(CompileError::SyntaxExact(MSG.to_owned()));
                    }
                }
                StmtKind::Return(_) => {
                    return Err(CompileError::SyntaxExact(MSG.to_owned()));
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
        self.emit(OpCode::Resume, 0);
        for stmt in &module.body {
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
        }
        for n in assigned {
            self.bindings.insert(n, Binding::Global);
        }
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
        for name in free_candidates {
            if self.bindings.contains_key(&name) {
                continue;
            }
            for env in enclosing {
                if let Some(b) = env.get(&name) {
                    match b {
                        Binding::Local | Binding::Cell | Binding::Free | Binding::Nonlocal => {
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
                self.compile_expr(e)?;
                // `eval` mode: the single top-level expression returns its
                // value so `eval(compile(src, fn, "eval"))` yields it.
                // Interactive ("single") mode: a top-level expression
                // statement echoes its value via `sys.displayhook`
                // instead of being discarded. Only the top-level compiler
                // sets these flags; nested scopes get fresh `Compiler`
                // instances, so this never fires inside functions/classes.
                if self.eval_mode {
                    self.emit(OpCode::ReturnValue, 0);
                } else if self.interactive {
                    self.emit(OpCode::PrintExpr, 0);
                } else {
                    self.emit(OpCode::PopTop, 0);
                }
            }
            StmtKind::Pass => {}
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
                // We don't yet strip assertions under `-O`; the VM
                // checks `sys.flags.optimize` at runtime if it wants
                // to elide the AssertionError raise.
                self.compile_expr(test)?;
                let skip = self.emit(OpCode::PopJumpIfTrue, 0);
                // The *builtin* AssertionError, immune to shadowing
                // (CPython LOAD_ASSERTION_ERROR, bpo-34880).
                self.emit(OpCode::LoadAssertionError, 0);
                if let Some(m) = msg {
                    self.compile_expr(m)?;
                    self.emit(OpCode::Call, 1);
                }
                self.emit(OpCode::RaiseVarargs, 1);
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
                        return Err(CompileError::SyntaxExact(if n > 1 {
                            "assignment to yield expression not possible".to_owned()
                        } else {
                            "cannot assign to yield expression here. Maybe you meant '==' \
                             instead of '='?"
                                .to_owned()
                        }));
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
                    return Err(CompileError::SyntaxExact(
                        "'yield expression' is an illegal expression for augmented assignment"
                            .to_owned(),
                    ));
                }
                self.compile_load_target(target)?;
                self.compile_expr(value)?;
                self.emit(
                    OpCode::BinaryOp,
                    bin_op_kind(*op) as u32 | crate::bytecode::BINARY_OP_INPLACE_FLAG,
                );
                self.compile_assign(target)?;
            }
            StmtKind::AnnAssign {
                target,
                annotation,
                value,
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
                // observable (used by `dataclasses`, `typing`).
                if matches!(self.code_kind, CodeKind::Class | CodeKind::Module) {
                    if let ExprKind::Name(name) = &target.kind {
                        self.compile_annotation_record(name, annotation)?;
                    }
                }
            }
            StmtKind::If { test, body, orelse } => {
                self.compile_expr(test)?;
                let jump_else = self.emit(OpCode::PopJumpIfFalse, 0);
                for s in body {
                    self.compile_stmt(s)?;
                }
                if orelse.is_empty() {
                    let target = self.next_offset();
                    self.patch_jump(jump_else, target);
                } else {
                    let jump_end = self.emit(OpCode::JumpForward, 0);
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
                let loop_start = self.next_offset();
                self.compile_expr(test)?;
                let jump_exit = self.emit(OpCode::PopJumpIfFalse, 0);
                self.loop_stack.push(LoopFrame {
                    continue_target: loop_start,
                    break_sites: Vec::new(),
                    is_for_loop: false,
                    handler_depth_at_entry: self.handler_depth,
                });
                for s in body {
                    self.compile_stmt(s)?;
                }
                let back = self.emit(OpCode::JumpBackward, 0);
                self.patch_jump(back, loop_start);
                let frame = self.loop_stack.pop().expect("loop frame");
                // Natural exit: condition went false. Run the
                // `orelse` block.
                let orelse_target = self.next_offset();
                self.patch_jump(jump_exit, orelse_target);
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
                });
                for s in body {
                    self.compile_stmt(s)?;
                }
                let back = self.emit(OpCode::JumpBackward, 0);
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
                self.emit(OpCode::EndFor, 0);
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
                    return Err(CompileError::NotImplemented(
                        "`async for` outside `async def`",
                        "wrap the loop in an `async def` function",
                    ));
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
                self.compile_pep695_prologue(type_params, stmt.span)?;
                self.compile_function_def(name, args, body, decorator_list, returns.as_deref())?;
                self.compile_pep695_epilogue(name, type_params, stmt.span)?;
            }
            StmtKind::AsyncFunctionDef {
                name,
                args,
                body,
                decorator_list,
                type_params,
                returns,
            } => {
                self.compile_pep695_prologue(type_params, stmt.span)?;
                self.compile_async_function_def(
                    name,
                    args,
                    body,
                    decorator_list,
                    returns.as_deref(),
                )?;
                self.compile_pep695_epilogue(name, type_params, stmt.span)?;
            }
            StmtKind::ClassDef {
                name,
                bases,
                keywords,
                body,
                decorator_list,
                type_params,
            } => {
                self.compile_pep695_prologue(type_params, stmt.span)?;
                self.compile_class_def(name, bases, keywords, body, decorator_list)?;
                self.compile_pep695_epilogue(name, type_params, stmt.span)?;
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
                    return Err(CompileError::NotImplemented(
                        "`async with` outside `async def`",
                        "wrap the block in an `async def` function",
                    ));
                }
                self.compile_async_with(items, body)?;
            }
            StmtKind::Return(value) => {
                if self.kind != CodeKind::Function {
                    return Err(CompileError::ReturnOutsideFunction);
                }
                // PEP 525: async generators cannot return a value (the
                // flag is set before the body compiles, so this sees it).
                if self.co.is_async_generator && value.is_some() {
                    return Err(CompileError::SyntaxExact(
                        "'return' with value in async generator".to_owned(),
                    ));
                }
                match value {
                    Some(v) => self.compile_expr(v)?,
                    None => {
                        let idx = self.co.intern_constant(Constant::None);
                        self.emit(OpCode::LoadConst, idx);
                    }
                }
                // Inline every pending finally clause from innermost
                // outward so each runs before we leave the function.
                // We stash the return value in a synthetic local across
                // each finally body to keep it alive even if the body
                // mutates the stack.
                if !self.finally_stack.is_empty() {
                    let tmp = format!(".retval{}", self.finally_counter);
                    self.finally_counter += 1;
                    let tmp_idx = self.var_index_or_add(&tmp);
                    self.emit(OpCode::StoreFast, tmp_idx);
                    let frames = std::mem::take(&mut self.finally_stack);
                    let mut compiled: Result<(), CompileError> = Ok(());
                    for (i, frame) in frames.iter().enumerate().rev() {
                        // While compiling this finally body, hide it
                        // from the stack so nested `return`s inside the
                        // body don't recurse infinitely.
                        let saved_finally: Vec<FinallyFrame> =
                            frames.iter().take(i).map(clone_finally_frame).collect();
                        self.finally_stack = saved_finally;
                        if let Err(e) = self.emit_finally_frame(frame) {
                            compiled = Err(e);
                        }
                        self.finally_stack.clear();
                        if compiled.is_err() {
                            break;
                        }
                    }
                    self.finally_stack = frames;
                    compiled?;
                    self.emit(OpCode::LoadFast, tmp_idx);
                }
                self.emit(OpCode::ReturnValue, 0);
            }
            StmtKind::Break => {
                let frame_top = self
                    .loop_stack
                    .last()
                    .ok_or(CompileError::BreakOutsideLoop)?;
                let is_for = frame_top.is_for_loop;
                let exc_to_pop = self.handler_depth.saturating_sub(frame_top.handler_depth_at_entry);
                // Leaving `except` handler bodies on the way out: discard
                // their handled-exception state (CPython POP_EXCEPT
                // during block unwind).
                for _ in 0..exc_to_pop {
                    self.emit(OpCode::PopExcept, 0);
                }
                // Run any `finally` clauses that lie between us and
                // the enclosing loop, in innermost-out order.
                self.inline_finally_for_loop_exit()?;
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
                let frame_top = self
                    .loop_stack
                    .last()
                    .ok_or(CompileError::ContinueOutsideLoop)?;
                let target = frame_top.continue_target;
                let exc_to_pop = self.handler_depth.saturating_sub(frame_top.handler_depth_at_entry);
                for _ in 0..exc_to_pop {
                    self.emit(OpCode::PopExcept, 0);
                }
                self.inline_finally_for_loop_exit()?;
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
                    // `import a.b.c as x` walks the attribute chain.
                    let mut parts = alias.name.split('.');
                    let _ = parts.next();
                    for part in parts {
                        let idx = self.co.intern_name(part);
                        self.emit(OpCode::LoadAttr, idx);
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
        if names.len() == 1 && names[0].name == "*" {
            self.emit(OpCode::ImportStar, 0);
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

    /// Lower `match subject: case ...:` into bytecode.
    ///
    /// At runtime the subject sits on the stack while each case is
    /// tried; we pop it (and any extracted values) on a successful
    /// match before jumping to the chosen body. The subject is also
    /// popped before falling off the end of the match.
    fn compile_match(&mut self, subject: &Expr, cases: &[MatchCase]) -> Result<(), CompileError> {
        self.compile_expr(subject)?;
        let mut end_jumps: Vec<u32> = Vec::new();
        for case in cases {
            let mut fail_sites: Vec<u32> = Vec::new();
            self.emit(OpCode::CopyTop, 0);
            self.compile_pattern(&case.pattern, &mut fail_sites)?;
            if let Some(guard) = &case.guard {
                self.compile_expr(guard)?;
                let g = self.emit(OpCode::PopJumpIfFalse, 0);
                fail_sites.push(g);
            }
            self.emit(OpCode::PopTop, 0);
            for s in &case.body {
                self.compile_stmt(s)?;
            }
            let jump_end = self.emit(OpCode::JumpForward, 0);
            end_jumps.push(jump_end);
            let fail_target = self.next_offset();
            for site in fail_sites {
                self.patch_jump(site, fail_target);
            }
        }
        self.emit(OpCode::PopTop, 0);
        let end = self.next_offset();
        for j in end_jumps {
            self.patch_jump(j, end);
        }
        Ok(())
    }

    /// Compile a pattern. The subject is at TOS when this is called
    /// and must still be there on the failure path. On success TOS
    /// remains the subject and any captures have been stored.
    fn compile_pattern(
        &mut self,
        pat: &Pattern,
        fail_sites: &mut Vec<u32>,
    ) -> Result<(), CompileError> {
        match pat {
            Pattern::Value(expr) => {
                self.compile_expr(expr)?;
                self.emit(OpCode::CompareOp, CompareKind::Eq as u32);
                let j = self.emit(OpCode::PopJumpIfFalse, 0);
                fail_sites.push(j);
            }
            Pattern::Singleton(c) => {
                let idx = self.co.intern_constant(c.clone().into());
                self.emit(OpCode::LoadConst, idx);
                self.emit(OpCode::IsOp, 0);
                let j = self.emit(OpCode::PopJumpIfFalse, 0);
                fail_sites.push(j);
            }
            Pattern::Capture(None) => {
                self.emit(OpCode::PopTop, 0);
            }
            Pattern::Capture(Some(name)) => {
                let name_expr = Expr {
                    kind: ExprKind::Name(name.clone()),
                    span: weavepy_lexer::Span::new(0, 0),
                };
                self.compile_assign(&name_expr)?;
            }
            Pattern::Sequence(items) => {
                self.compile_sequence_pattern(items, fail_sites)?;
            }
            Pattern::Star(_) => {
                return Err(CompileError::Internal(
                    "`*name` patterns may only appear inside a sequence".to_owned(),
                ));
            }
            Pattern::Mapping {
                keys,
                patterns,
                rest,
            } => {
                self.compile_mapping_pattern(keys, patterns, rest.as_ref(), fail_sites)?;
            }
            Pattern::Class {
                cls,
                positionals,
                keywords,
            } => {
                self.compile_class_pattern(cls, positionals, keywords, fail_sites)?;
            }
            Pattern::Or(alts) => {
                let mut end_jumps: Vec<u32> = Vec::new();
                let n = alts.len();
                for (i, alt) in alts.iter().enumerate() {
                    let mut local_fail: Vec<u32> = Vec::new();
                    if i + 1 < n {
                        self.emit(OpCode::CopyTop, 0);
                    }
                    self.compile_pattern(alt, &mut local_fail)?;
                    if i + 1 < n {
                        let j = self.emit(OpCode::JumpForward, 0);
                        end_jumps.push(j);
                        let fail_target = self.next_offset();
                        for site in local_fail {
                            self.patch_jump(site, fail_target);
                        }
                    } else {
                        for site in local_fail {
                            fail_sites.push(site);
                        }
                    }
                }
                let end = self.next_offset();
                for j in end_jumps {
                    self.patch_jump(j, end);
                }
            }
            Pattern::As { pattern, name } => {
                self.emit(OpCode::CopyTop, 0);
                let name_expr = Expr {
                    kind: ExprKind::Name(name.clone()),
                    span: weavepy_lexer::Span::new(0, 0),
                };
                self.compile_assign(&name_expr)?;
                self.compile_pattern(pattern, fail_sites)?;
            }
        }
        Ok(())
    }

    fn compile_sequence_pattern(
        &mut self,
        items: &[Pattern],
        fail_sites: &mut Vec<u32>,
    ) -> Result<(), CompileError> {
        self.emit(OpCode::MatchSequence, 0);
        let j = self.emit(OpCode::PopJumpIfFalse, 0);
        fail_sites.push(j);
        let star_index = items.iter().position(|p| matches!(p, Pattern::Star(_)));
        let expected_len = if star_index.is_some() {
            items.len() - 1
        } else {
            items.len()
        };
        self.emit(OpCode::GetLen, 0);
        let len_idx = self.co.intern_constant(Constant::Int(expected_len as i64));
        self.emit(OpCode::LoadConst, len_idx);
        if star_index.is_some() {
            self.emit(OpCode::CompareOp, CompareKind::GtE as u32);
        } else {
            self.emit(OpCode::CompareOp, CompareKind::Eq as u32);
        }
        let j = self.emit(OpCode::PopJumpIfFalse, 0);
        fail_sites.push(j);
        for (i, pat) in items.iter().enumerate() {
            self.emit(OpCode::CopyTop, 0);
            match pat {
                Pattern::Star(name) => {
                    let tail = items.len() - i - 1;
                    self.emit_pattern_subscript_slice(i, tail);
                    if let Some(n) = name {
                        let name_expr = Expr {
                            kind: ExprKind::Name(n.clone()),
                            span: weavepy_lexer::Span::new(0, 0),
                        };
                        self.compile_assign(&name_expr)?;
                    } else {
                        self.emit(OpCode::PopTop, 0);
                    }
                }
                _ => {
                    let idx = if let Some(si) = star_index {
                        if i > si {
                            // negative index from end
                            -((items.len() - i) as i64)
                        } else {
                            i as i64
                        }
                    } else {
                        i as i64
                    };
                    let cidx = self.co.intern_constant(Constant::Int(idx));
                    self.emit(OpCode::LoadConst, cidx);
                    self.emit(OpCode::BinarySubscr, 0);
                    self.compile_pattern(pat, fail_sites)?;
                }
            }
        }
        Ok(())
    }

    /// Emit a slice subscription `subject[head:len-tail]` for a `*name`
    /// position inside a sequence pattern. Leaves the slice list on the
    /// stack.
    fn emit_pattern_subscript_slice(&mut self, head: usize, tail: usize) {
        let lower = self.co.intern_constant(Constant::Int(head as i64));
        self.emit(OpCode::LoadConst, lower);
        if tail == 0 {
            let none = self.co.intern_constant(Constant::None);
            self.emit(OpCode::LoadConst, none);
        } else {
            let neg = self.co.intern_constant(Constant::Int(-(tail as i64)));
            self.emit(OpCode::LoadConst, neg);
        }
        let none = self.co.intern_constant(Constant::None);
        self.emit(OpCode::LoadConst, none);
        self.emit(OpCode::BuildSlice, 3);
        self.emit(OpCode::BinarySubscr, 0);
    }

    fn compile_mapping_pattern(
        &mut self,
        keys: &[Expr],
        patterns: &[Pattern],
        rest: Option<&Option<String>>,
        fail_sites: &mut Vec<u32>,
    ) -> Result<(), CompileError> {
        self.emit(OpCode::MatchMapping, 0);
        let j = self.emit(OpCode::PopJumpIfFalse, 0);
        fail_sites.push(j);
        if !keys.is_empty() {
            for k in keys {
                self.compile_expr(k)?;
            }
            self.emit(OpCode::BuildTuple, keys.len() as u32);
            self.emit(OpCode::MatchKeys, 0);
            let none_idx = self.co.intern_constant(Constant::None);
            self.emit(OpCode::LoadConst, none_idx);
            self.emit(OpCode::IsOp, 1);
            let j = self.emit(OpCode::PopJumpIfFalse, 0);
            fail_sites.push(j);
            for (i, pat) in patterns.iter().enumerate() {
                self.emit(OpCode::CopyTop, 0);
                let idx = self.co.intern_constant(Constant::Int(i as i64));
                self.emit(OpCode::LoadConst, idx);
                self.emit(OpCode::BinarySubscr, 0);
                self.compile_pattern(pat, fail_sites)?;
            }
            self.emit(OpCode::PopTop, 0);
        }
        if let Some(rest_name) = rest {
            self.emit(OpCode::CopyTop, 0);
            self.emit_dict_copy_without_keys(keys.len());
            if let Some(n) = rest_name {
                let name_expr = Expr {
                    kind: ExprKind::Name(n.clone()),
                    span: weavepy_lexer::Span::new(0, 0),
                };
                self.compile_assign(&name_expr)?;
            } else {
                self.emit(OpCode::PopTop, 0);
            }
        }
        Ok(())
    }

    fn emit_dict_copy_without_keys(&mut self, _key_count: usize) {
        // Stub: the VM provides this as a builtin call via dict.copy()
        // for now; real CPython uses a dedicated opcode.
        let idx = self.co.intern_name("dict");
        self.emit(OpCode::LoadGlobal, idx);
        self.emit(OpCode::Swap, 1);
        self.emit(OpCode::Call, 1);
    }

    fn compile_class_pattern(
        &mut self,
        cls: &Expr,
        positionals: &[Pattern],
        keywords: &[(String, Pattern)],
        fail_sites: &mut Vec<u32>,
    ) -> Result<(), CompileError> {
        // Stack on entry (top-down): subject_copy. We must end with
        // the subject_copy popped on success, and the subject_copy
        // popped (and fail_sites taken) on failure.
        self.compile_expr(cls)?;
        let kw_names: Vec<Constant> = keywords
            .iter()
            .map(|(n, _)| Constant::Str(n.clone()))
            .collect();
        let kw_idx = self.co.intern_constant(Constant::Tuple(kw_names));
        self.emit(OpCode::LoadConst, kw_idx);
        self.emit(OpCode::MatchClass, positionals.len() as u32);
        // Stack now: [..., result_or_none]
        self.emit(OpCode::CopyTop, 0);
        let none_idx = self.co.intern_constant(Constant::None);
        self.emit(OpCode::LoadConst, none_idx);
        self.emit(OpCode::IsOp, 0);
        let bad = self.emit(OpCode::PopJumpIfTrue, 0);
        // Result is a tuple. Inner patterns get their own fail list
        // so we can pop the tuple before joining the outer fail path.
        let mut local_fails: Vec<u32> = Vec::new();
        for (i, pat) in positionals.iter().enumerate() {
            self.emit(OpCode::CopyTop, 0);
            let idx = self.co.intern_constant(Constant::Int(i as i64));
            self.emit(OpCode::LoadConst, idx);
            self.emit(OpCode::BinarySubscr, 0);
            self.compile_pattern(pat, &mut local_fails)?;
        }
        for (i, (_, pat)) in keywords.iter().enumerate() {
            self.emit(OpCode::CopyTop, 0);
            let idx = self
                .co
                .intern_constant(Constant::Int((positionals.len() + i) as i64));
            self.emit(OpCode::LoadConst, idx);
            self.emit(OpCode::BinarySubscr, 0);
            self.compile_pattern(pat, &mut local_fails)?;
        }
        self.emit(OpCode::PopTop, 0); // drop result tuple
        let success = self.emit(OpCode::JumpForward, 0);
        // On inner failure path: stack has the result tuple. Drop it
        // and join the outer fail_sites.
        let inner_fail_target = self.next_offset();
        for site in local_fails {
            self.patch_jump(site, inner_fail_target);
        }
        self.emit(OpCode::PopTop, 0); // drop result tuple
        fail_sites.push(self.emit(OpCode::JumpForward, 0));
        // bad path: result was None; pop and join outer fail_sites.
        let bad_target = self.next_offset();
        self.patch_jump(bad, bad_target);
        self.emit(OpCode::PopTop, 0); // drop the None
        fail_sites.push(self.emit(OpCode::JumpForward, 0));
        let end = self.next_offset();
        self.patch_jump(success, end);
        Ok(())
    }

    /// Compile a function definition statement: builds the function
    /// object, threads it through any decorators, and binds the result
    /// to `name` in the enclosing scope.
    /// PEP 695 lowering, part 1: bind each type parameter as a
    /// `TypeVar` *before* the `def`/`class` compiles, so parameter and
    /// return annotations referencing `T` resolve at definition time:
    ///
    /// ```text
    /// T = __weavepy_typevar__('T')
    /// def f(a: T): ...
    /// f.__type_params__ = (T,)
    /// f.__annotations__['return'] = R
    /// del T
    /// ```
    ///
    /// CPython gives the parameters a dedicated lexical scope; the
    /// flat-block approximation is observably equivalent here because
    /// nothing reads the names after the epilogue's `del`.
    fn compile_pep695_prologue(
        &mut self,
        type_params: &[String],
        span: weavepy_lexer::Span,
    ) -> Result<(), CompileError> {
        if type_params.is_empty() {
            return Ok(());
        }
        let name_expr = |n: &str| Expr {
            kind: ExprKind::Name(n.to_owned()),
            span,
        };
        for tp in type_params {
            let assign = Stmt {
                kind: StmtKind::Assign {
                    targets: vec![name_expr(tp)],
                    value: Expr {
                        kind: ExprKind::Call {
                            func: Box::new(name_expr("__weavepy_typevar__")),
                            args: vec![Expr {
                                kind: ExprKind::Constant(AstConstant::Str(tp.clone())),
                                span,
                            }],
                            keywords: Vec::new(),
                        },
                        span,
                    },
                },
                span,
            };
            self.compile_stmt(&assign)?;
        }
        Ok(())
    }

    /// PEP 695 lowering, part 2: after the `def`/`class` statement has
    /// bound its name, stamp `__type_params__`, then drop the temporary
    /// bindings. (The return annotation is *not* handled here — it goes
    /// into the annotations dict at MakeFunction time, before
    /// decorators wrap the function, exactly like CPython.)
    fn compile_pep695_epilogue(
        &mut self,
        name: &str,
        type_params: &[String],
        span: weavepy_lexer::Span,
    ) -> Result<(), CompileError> {
        if type_params.is_empty() {
            return Ok(());
        }
        let name_expr = |n: &str| Expr {
            kind: ExprKind::Name(n.to_owned()),
            span,
        };
        let set_params = Stmt {
            kind: StmtKind::Assign {
                targets: vec![Expr {
                    kind: ExprKind::Attribute {
                        value: Box::new(name_expr(name)),
                        attr: "__type_params__".to_owned(),
                    },
                    span,
                }],
                value: Expr {
                    kind: ExprKind::Tuple(type_params.iter().map(|t| name_expr(t)).collect()),
                    span,
                },
            },
            span,
        };
        self.compile_stmt(&set_params)?;
        let del = Stmt {
            kind: StmtKind::Delete(type_params.iter().map(|t| name_expr(t)).collect()),
            span,
        };
        self.compile_stmt(&del)?;
        Ok(())
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
        for d in decorator_list {
            self.compile_expr(d)?;
        }
        self.build_function_object_inner(name, args, body, returns, is_async)?;
        // Decorators apply innermost-first; each application CALL carries
        // the *decorator expression's* location (CPython points the
        // traceback at `@dec`, not at the `def` line).
        for d in decorator_list.iter().rev() {
            let saved = self.current_span;
            self.set_span(d.span);
            self.emit(OpCode::Call, 1);
            self.current_span = saved;
        }
        let name_expr = Expr {
            kind: ExprKind::Name(name.to_owned()),
            span: weavepy_lexer::Span::new(0, 0),
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
        self.build_function_object_inner(name, args, body, None, false)
    }

    fn build_function_object_inner(
        &mut self,
        name: &str,
        args: &AstArguments,
        body: &[Stmt],
        returns: Option<&Expr>,
        is_async: bool,
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

        let mut inner = Compiler::new(
            name.to_owned(),
            self.co.filename.clone(),
            CodeKind::Function,
            self.line_index.clone(),
            self.source.clone(),
            self.future_annotations,
        );
        inner.co.qualname = self.compute_child_qualname(name);
        inner.co.arg_count = arg_count;
        inner.co.posonly_count = posonly_count;
        inner.co.kwonly_count = kwonly_count;
        inner.co.has_varargs = args.vararg.is_some();
        inner.co.has_varkeywords = args.kwarg.is_some();
        inner.co.varnames = param_names.clone();
        inner.current_line = self.current_line;
        // Methods compiled inside a class body get an implicit
        // `__class__` free variable so `super()` (and explicit
        // `__class__` references) work without arguments.
        if self.inside_class_body && method_references_class(body) {
            inner.bindings.insert("__class__".to_owned(), Binding::Free);
            inner.free_order.push("__class__".to_owned());
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
        } else {
            inner.co.is_generator = has_yield;
            if has_yield {
                inner.emit(OpCode::ReturnGenerator, 0);
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
            Some(doc) => Constant::Str(doc.to_owned()),
            None => Constant::None,
        };
        inner.co.intern_constant(doc_slot);
        for s in body {
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
        let mut annotated_params: Vec<(String, &Expr)> = Vec::new();
        for a in args
            .posonlyargs
            .iter()
            .chain(args.args.iter())
            .chain(args.kwonlyargs.iter())
        {
            if let Some(ann) = a.annotation.as_ref() {
                annotated_params.push((a.name.clone(), ann));
            }
        }
        if let Some(va) = &args.vararg {
            if let Some(ann) = va.annotation.as_ref() {
                annotated_params.push((va.name.clone(), ann));
            }
        }
        if let Some(kw) = &args.kwarg {
            if let Some(ann) = kw.annotation.as_ref() {
                annotated_params.push((kw.name.clone(), ann));
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
            .intern_constant(Constant::Code(Box::new(inner_code)));
        self.emit(OpCode::LoadConst, code_idx);
        self.emit(OpCode::MakeFunction, flags);
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
        for d in decorator_list {
            self.compile_expr(d)?;
        }
        self.emit(OpCode::LoadBuildClass, 0);

        // A `**kwds` in the class header (or a `*bases` splat) can't be
        // expressed with the fixed-arity `Call`/`CallKw` shapes, so fall
        // back to the same `CallEx` lowering the function-call site uses:
        // build a single positional args tuple `(body, name, *bases)` and
        // a merged keyword dict, then unpack both into `__build_class__`.
        let has_kw_splat = keywords.iter().any(|k| k.arg.is_none());
        let has_starred_base = bases.iter().any(|b| matches!(b.kind, ExprKind::Starred(_)));

        if has_kw_splat || has_starred_base {
            self.build_class_body(name, body)?;
            let name_idx = self.co.intern_constant(Constant::Str(name.to_owned()));
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
            self.build_class_body(name, body)?;
            let name_idx = self.co.intern_constant(Constant::Str(name.to_owned()));
            self.emit(OpCode::LoadConst, name_idx);
            for b in bases {
                self.compile_expr(b)?;
            }
            if keywords.is_empty() {
                self.emit(OpCode::Call, (bases.len() + 2) as u32);
            } else {
                let mut names: Vec<Constant> = Vec::with_capacity(keywords.len());
                for k in keywords {
                    let n = k.arg.clone().expect("kw splat handled by CallEx path above");
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
            self.emit(OpCode::Call, 1);
            self.current_span = saved;
        }
        let name_expr = Expr {
            kind: ExprKind::Name(name.to_owned()),
            span: weavepy_lexer::Span::new(0, 0),
        };
        self.compile_assign(&name_expr)
    }

    /// Build the class-body function object and leave it on the stack.
    fn build_class_body(&mut self, name: &str, body: &[Stmt]) -> Result<(), CompileError> {
        let mut inner = Compiler::new(
            name.to_owned(),
            self.co.filename.clone(),
            CodeKind::Class,
            self.line_index.clone(),
            self.source.clone(),
            self.future_annotations,
        );
        inner.co.qualname = self.compute_child_qualname(name);
        inner.current_line = self.current_line;
        // Every class body carries a `__class__` cell so methods can
        // close over it. `__build_class__` patches the cell with the
        // resulting type once construction finishes.
        inner.co.cellvars.push("__class__".to_owned());
        inner.bindings.insert("__class__".to_owned(), Binding::Cell);

        let mut assigned = HashSet::new();
        for s in body {
            collect_assigned(s, &mut assigned);
        }
        for n in assigned {
            inner.bindings.insert(n, Binding::Global);
        }
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
        for name in free_candidates {
            if inner.bindings.contains_key(&name) {
                continue;
            }
            if let Some(b) = self.bindings.get(&name) {
                if matches!(
                    b,
                    Binding::Local | Binding::Cell | Binding::Free | Binding::Nonlocal
                ) {
                    inner.bindings.insert(name.clone(), Binding::Free);
                    inner.free_order.push(name);
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

        // CPython stores a class body's leading string literal as
        // `__doc__` via a `STORE_NAME` at the top of the body. Mirror
        // that so `Cls.__doc__` is faithful (classes without a docstring
        // get `None` stamped by `__build_class__`). Unlike a function
        // body — where the docstring lives in `co_consts[0]` — a class
        // body reserves that slot for the qualname, so it must be an
        // explicit store rather than a constant-slot convention.
        if let Some(doc) = first_stmt_docstring(body) {
            let doc_const = inner.co.intern_constant(Constant::Str(doc.to_owned()));
            let doc_name = inner.co.intern_name("__doc__");
            inner.emit(OpCode::LoadConst, doc_const);
            inner.emit(OpCode::StoreName, doc_name);
        }

        for s in body {
            inner.compile_stmt(s)?;
        }
        // Expose the `__class__` cell via `__classcell__` so the
        // `__build_class__` builtin can patch it.
        let class_cell_idx = inner.cell_or_free_index("__class__");
        inner.emit(OpCode::LoadClosure, class_cell_idx);
        let classcell_name = inner.co.intern_name("__classcell__");
        inner.emit(OpCode::StoreName, classcell_name);

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
            .intern_constant(Constant::Code(Box::new(inner_code)));
        self.emit(OpCode::LoadConst, code_idx);
        self.emit(OpCode::MakeFunction, flags);
        Ok(())
    }

    /// Compile `try / except / else / finally`. The body is protected
    /// by an exception table entry; matched handlers fall through to
    /// the `else` branch, unmatched ones re-raise. `finally` runs on
    /// every exit path.
    /// Inline every `finally` clause that lives *inside* the
    /// enclosing loop (i.e. was pushed after the current loop frame),
    /// in innermost-out order. Used by `break` / `continue` so the
    /// cleanup runs before we transfer control out of the loop.
    fn inline_finally_for_loop_exit(&mut self) -> Result<(), CompileError> {
        let loop_depth = self.loop_stack.len();
        let mut to_inline: Vec<FinallyFrame> = Vec::new();
        for frame in self.finally_stack.iter().rev() {
            if frame.loop_depth_at_push >= loop_depth {
                to_inline.push(clone_finally_frame(frame));
            } else {
                break;
            }
        }
        if to_inline.is_empty() {
            return Ok(());
        }
        let saved = std::mem::take(&mut self.finally_stack);
        // Walk innermost out; on each iteration further trim the
        // finally stack so a `return` nested inside a finally body
        // can't re-inline its own ancestors infinitely.
        for (offset, frame) in to_inline.iter().enumerate() {
            let outer_count = saved.len().saturating_sub(offset + 1);
            self.finally_stack = saved
                .iter()
                .take(outer_count)
                .map(clone_finally_frame)
                .collect();
            self.emit_finally_frame(frame)?;
        }
        self.finally_stack = saved;
        Ok(())
    }

    /// Emit cleanup code for one `FinallyFrame`. `Stmts` frames
    /// re-compile the AST body; `WithExit` frames emit
    /// `<cm_local>.__exit__(None, None, None)` directly.
    fn emit_finally_frame(&mut self, frame: &FinallyFrame) -> Result<(), CompileError> {
        match &frame.kind {
            FinallyKind::Stmts(body) => {
                for s in body {
                    self.compile_stmt(s)?;
                }
                Ok(())
            }
            FinallyKind::WithExit { cm_idx } => {
                self.emit(OpCode::LoadFast, *cm_idx);
                let exit_name = self.co.intern_name("__exit__");
                self.emit(OpCode::LoadAttr, exit_name);
                let none_idx = self.co.intern_constant(Constant::None);
                self.emit(OpCode::LoadConst, none_idx);
                self.emit(OpCode::LoadConst, none_idx);
                self.emit(OpCode::LoadConst, none_idx);
                self.emit(OpCode::Call, 3);
                self.emit(OpCode::PopTop, 0);
                Ok(())
            }
            FinallyKind::AsyncWithExit { aexit_idx } => {
                // `await <aexit>(None, None, None)`. The bound coroutine
                // method was stashed at `aexit_idx` by `compile_async_with`.
                self.emit(OpCode::LoadFast, *aexit_idx);
                let none_idx = self.co.intern_constant(Constant::None);
                self.emit(OpCode::LoadConst, none_idx);
                self.emit(OpCode::LoadConst, none_idx);
                self.emit(OpCode::LoadConst, none_idx);
                self.emit(OpCode::Call, 3);
                self.compile_await_dance(3);
                self.emit(OpCode::PopTop, 0);
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
        let body_depth = self.loop_stack.iter().filter(|fr| fr.is_for_loop).count() as u32
            + self.exc_on_stack;
        // Make the finally body visible to any `return`/`break`/
        // `continue` nested inside `body`/`orelse`/handlers. We pop it
        // before emitting the *direct* normal-/exception-exit copies
        // below — those copies must not see themselves on the stack.
        let pushed_finally = has_finally;
        if pushed_finally {
            self.finally_stack.push(FinallyFrame {
                kind: FinallyKind::Stmts(finalbody.to_vec()),
                loop_depth_at_push: self.loop_stack.len(),
            });
        }
        let body_start = self.next_offset();
        for s in body {
            self.compile_stmt(s)?;
        }
        let body_end = self.next_offset();
        // Else clause runs only on normal body completion. It sits
        // *outside* the handled range: an exception raised in `else`
        // does not reach this statement's own `except` clauses (it
        // still passes through `finally` via the cleanup entries
        // registered below).
        let orelse_start = self.next_offset();
        for s in orelse {
            self.compile_stmt(s)?;
        }
        let orelse_end = self.next_offset();

        // Normal-exit finally + jump to end. Falls through to the
        // finally body, then skips past the exception handlers.
        // We temporarily pop the finally frame so the inline copy here
        // doesn't see itself as still-pending (which would double-run
        // it on a nested `return`).
        let saved_frame = if pushed_finally {
            self.finally_stack.pop()
        } else {
            None
        };
        let normal_skip = if has_handlers || has_finally {
            for s in finalbody {
                self.compile_stmt(s)?;
            }
            self.emit(OpCode::JumpForward, 0)
        } else {
            self.next_offset()
        };
        if let Some(frame) = saved_frame {
            self.finally_stack.push(frame);
        }

        // Handlers begin here.
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
            self.co.exception_table.push(ExcHandler {
                start: body_start,
                end: body_end,
                handler: handlers_start,
                depth: body_depth,
                push_lasti: false,
            });
            // Back-patched to the pc past the handler region (see the
            // non-`except*` branch for the rationale).
            let push_exc_site = self.emit(OpCode::PushExcInfo, 0);
            // Stack on entry: [exc]. Stash the original (for
            // PREP_RERAISE_STAR), the running remainder, and the
            // raised-collection list in synthetic locals.
            let uid = self.with_counter;
            self.with_counter += 1;
            let orig_idx = self.var_index_or_add(&format!(".eg_orig{uid}"));
            let rem_idx = self.var_index_or_add(&format!(".eg_remaining{uid}"));
            let raised_idx = self.var_index_or_add(&format!(".eg_raised{uid}"));
            self.emit(OpCode::CopyTop, 0);
            self.emit(OpCode::StoreFast, orig_idx);
            self.emit(OpCode::StoreFast, rem_idx);
            self.emit(OpCode::BuildList, 0);
            self.emit(OpCode::StoreFast, raised_idx);

            let none_idx = self.co.intern_constant(Constant::None);
            for h in handlers {
                // [.remaining]
                self.emit(OpCode::LoadFast, rem_idx);
                let ty = h
                    .type_
                    .as_ref()
                    .expect("except* requires a type expression — parser must reject bare except*");
                self.compile_expr(ty)?;
                // [.remaining, type]. CPython locates CHECK_EG_MATCH on
                // the whole clause; the implicit wrapper around a naked
                // exception gets its traceback entry here (gh-128799).
                self.set_span(h.span);
                self.set_line_from(h.span.start.0);
                self.emit(OpCode::CheckEGMatch, 0);
                // [rest, matched]
                self.emit(OpCode::Swap, 2);
                // [matched, rest]
                self.emit(OpCode::StoreFast, rem_idx);
                // [matched]
                self.emit(OpCode::CopyTop, 0);
                // [matched, matched]
                self.emit(OpCode::LoadConst, none_idx);
                // [matched, matched, None]
                self.emit(OpCode::IsOp, 0);
                // [matched, is_none]
                let skip_body = self.emit(OpCode::PopJumpIfTrue, 0);
                // matched is on stack and is not None. It becomes the
                // active exception while the clause body runs —
                // back-patched below to tag the body's extent.
                let push_match_site = self.emit(OpCode::PushExcInfo, 0);
                if let Some(n) = &h.name {
                    let name_expr = Expr {
                        kind: ExprKind::Name(n.clone()),
                        span: h.span,
                    };
                    self.compile_assign(&name_expr)?;
                } else {
                    self.emit(OpCode::PopTop, 0);
                }
                let clause_body_start = self.next_offset();
                // `e` is unbound on every exit from the block (CPython
                // behaviour); `break`/`continue`/`return` cannot leave
                // an `except*` block at all (PEP 654), enforced via the
                // loop-mark pushed here.
                let unbind_stmts = h.name.as_deref().map(|n| Self::except_unbind_stmts(n, h.span));
                if let Some(stmts) = &unbind_stmts {
                    self.finally_stack.push(FinallyFrame {
                        kind: FinallyKind::Stmts(stmts.clone()),
                        loop_depth_at_push: self.loop_stack.len(),
                    });
                }
                for s in &h.body {
                    self.compile_stmt(s)?;
                }
                if let Some(stmts) = &unbind_stmts {
                    self.finally_stack.pop();
                    for s in stmts {
                        self.compile_stmt(s)?;
                    }
                }
                let clause_body_end = self.next_offset();
                self.co.instructions[push_match_site as usize].arg = clause_body_end;
                self.emit(OpCode::PopExcept, 0);
                let after_body = self.emit(OpCode::JumpForward, 0);

                // Collector: an exception raised by the clause body
                // lands here with `[raised_exc]` on the stack (its
                // `__context__` already chained to the matched group by
                // the raise itself). Stash it and run the next clause.
                let collector = self.next_offset();
                self.co.exception_table.push(ExcHandler {
                    start: clause_body_start,
                    end: clause_body_end,
                    handler: collector,
                    depth: body_depth,
                    push_lasti: false,
                });
                self.emit(OpCode::LoadFast, raised_idx);
                // [exc, list]
                self.emit(OpCode::Swap, 2);
                // [list, exc]
                self.emit(OpCode::ListAppend, 1);
                // [list]
                self.emit(OpCode::PopTop, 0);
                if let Some(stmts) = &unbind_stmts {
                    for s in stmts {
                        self.compile_stmt(s)?;
                    }
                }
                let after_collect = self.emit(OpCode::JumpForward, 0);

                let skip_target = self.next_offset();
                self.patch_jump(skip_body, skip_target);
                // matched is on stack still (was a None) — discard.
                self.emit(OpCode::PopTop, 0);
                let after_skip = self.next_offset();
                self.patch_jump(after_body, after_skip);
                self.patch_jump(after_collect, after_skip);
            }
            // After all clauses: excs = raised + [remainder]; compute
            // the exception to propagate (None when fully handled).
            self.emit(OpCode::LoadFast, raised_idx);
            self.emit(OpCode::LoadFast, rem_idx);
            // [list, rem]
            self.emit(OpCode::ListAppend, 1);
            // [list]
            self.emit(OpCode::LoadFast, orig_idx);
            // [list, orig]
            self.emit(OpCode::PrepReraiseStar, 0);
            // [result]
            self.emit(OpCode::CopyTop, 0);
            self.emit(OpCode::LoadConst, none_idx);
            self.emit(OpCode::IsOp, 0);
            let all_handled = self.emit(OpCode::PopJumpIfTrue, 0);
            // Re-raise without recording the re-raise site and without
            // re-chaining `__context__` (CPython RERAISE).
            self.emit(OpCode::Reraise, 0);
            let after_raise = self.next_offset();
            self.patch_jump(all_handled, after_raise);
            // [None] — discard, drop the original from exc_info.
            self.emit(OpCode::PopTop, 0);
            self.emit(OpCode::PopExcept, 0);
            let saved = if pushed_finally {
                self.finally_stack.pop()
            } else {
                None
            };
            for s in finalbody {
                self.compile_stmt(s)?;
            }
            if let Some(f) = saved {
                self.finally_stack.push(f);
            }
            let exit = self.emit(OpCode::JumpForward, 0);
            // Shared finally-cleanup for exceptions escaping the
            // `except*` machinery — clause-internal raises are collected
            // (above), so this covers match evaluation and the final
            // re-raise. Reached only via the exception-table entry;
            // normal flow jumps past.
            if has_finally {
                let cleanup_start = self.next_offset();
                let cleanup_push = self.emit(OpCode::PushExcInfo, 0);
                let saved = self.finally_stack.pop();
                self.exc_on_stack += 1;
                for s in finalbody {
                    self.compile_stmt(s)?;
                }
                self.exc_on_stack -= 1;
                if let Some(f) = saved {
                    self.finally_stack.push(f);
                }
                self.emit(OpCode::Reraise, 0);
                let cleanup_end = self.next_offset();
                self.co.instructions[cleanup_push as usize].arg = cleanup_end;
                // Registered after the per-clause collector entries so
                // the forward innermost-first scan prefers those.
                self.co.exception_table.push(ExcHandler {
                    start: handlers_start,
                    end: after_raise,
                    handler: cleanup_start,
                    depth: body_depth,
                    push_lasti: false,
                });
                if orelse_end > orelse_start {
                    self.co.exception_table.push(ExcHandler {
                        start: orelse_start,
                        end: orelse_end,
                        handler: cleanup_start,
                        depth: body_depth,
                        push_lasti: false,
                    });
                }
            }
            let end = self.next_offset();
            self.patch_jump(exit, end);
            // Record the handler-body end on PUSH_EXC_INFO (see below).
            self.co.instructions[push_exc_site as usize].arg = end;
        } else if has_handlers {
            self.co.exception_table.push(ExcHandler {
                start: body_start,
                end: body_end,
                handler: handlers_start,
                depth: body_depth,
                push_lasti: false,
            });
            // The arg is back-patched below to the pc just past this
            // handler region; the VM tags the active-handler entry with
            // it so an exception escaping the handler to an enclosing
            // `try` correctly unwinds `sys.exc_info()` (see
            // `Interpreter::handle_exception`).
            let push_exc_site = self.emit(OpCode::PushExcInfo, 0);
            // Stack on entry: [exc] (pushed by dispatch loop).
            let mut next_handler_sites: Vec<u32> = Vec::new();
            let mut handler_exit_jumps: Vec<u32> = Vec::new();
            // With a `finally`, an exception raised *inside* an except
            // clause (match check, bind, or body — e.g. a bare
            // `raise`) must still run the finally before propagating.
            // We record each clause's covered range (excluding the
            // inline finally copies) and point them at a shared
            // cleanup block emitted after the re-raise path.
            let mut cleanup_ranges: Vec<(u32, u32)> = Vec::new();
            // Each except clause's body lives between the body and the
            // catch-all `RERAISE` at the bottom. If a clause's `type_`
            // doesn't match we fall through to the next clause via the
            // patched `next_handler_sites`. After running a clause we
            // POP_EXCEPT and jump to `handler_exit_jumps`.
            for (i, h) in handlers.iter().enumerate() {
                // Patch the previous handler's "no-match" branch.
                if i > 0 {
                    let prev = next_handler_sites.pop();
                    if let Some(site) = prev {
                        let cur = self.next_offset();
                        self.patch_jump(site, cur);
                    }
                }
                let clause_start = self.next_offset();
                match &h.type_ {
                    Some(t) => {
                        // Stack: [exc] → [exc, type] → [exc, bool]
                        self.emit(OpCode::CopyTop, 0);
                        self.compile_expr(t)?;
                        self.emit(OpCode::CheckExcMatch, 0);
                        let no_match = self.emit(OpCode::PopJumpIfFalse, 0);
                        next_handler_sites.push(no_match);
                        // Matched: Stack still [exc]. Bind or discard.
                        if let Some(n) = &h.name {
                            let name_expr = Expr {
                                kind: ExprKind::Name(n.clone()),
                                span: h.span,
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
                let unbind_stmts = h.name.as_deref().map(|n| Self::except_unbind_stmts(n, h.span));
                if let Some(stmts) = &unbind_stmts {
                    self.finally_stack.push(FinallyFrame {
                        kind: FinallyKind::Stmts(stmts.clone()),
                        loop_depth_at_push: self.loop_stack.len(),
                    });
                }
                let hbody_start = self.next_offset();
                self.handler_depth += 1;
                for s in &h.body {
                    self.compile_stmt(s)?;
                }
                self.handler_depth -= 1;
                let hbody_end = self.next_offset();
                if let Some(stmts) = &unbind_stmts {
                    self.finally_stack.pop();
                    for s in stmts {
                        self.compile_stmt(s)?;
                    }
                }
                self.emit(OpCode::PopExcept, 0);
                if let Some(stmts) = &unbind_stmts {
                    if hbody_end > hbody_start {
                        // Exception escaping the clause body: unbind the
                        // name, then keep propagating. Normal flow jumps
                        // over this block.
                        let over = self.emit(OpCode::JumpForward, 0);
                        let cleanup_start = self.next_offset();
                        let cleanup_push = self.emit(OpCode::PushExcInfo, 0);
                        self.exc_on_stack += 1;
                        for s in stmts {
                            self.compile_stmt(s)?;
                        }
                        self.exc_on_stack -= 1;
                        self.emit(OpCode::Reraise, 0);
                        let cleanup_end = self.next_offset();
                        self.co.instructions[cleanup_push as usize].arg = cleanup_end;
                        self.patch_jump(over, cleanup_end);
                        self.co.exception_table.push(ExcHandler {
                            start: hbody_start,
                            end: hbody_end,
                            handler: cleanup_start,
                            depth: body_depth,
                            // CPython marks the unbind-cleanup with the
                            // lasti flag: its RERAISE restores f_lasti to
                            // the raise site inside the except body.
                            push_lasti: true,
                        });
                    }
                }
                if has_finally {
                    // Includes the unbind-cleanup RERAISE so the escaping
                    // exception still runs this statement's finally.
                    cleanup_ranges.push((clause_start, self.next_offset()));
                }
                // Run finally on the matched path.
                let saved = if pushed_finally {
                    self.finally_stack.pop()
                } else {
                    None
                };
                for s in finalbody {
                    self.compile_stmt(s)?;
                }
                if let Some(f) = saved {
                    self.finally_stack.push(f);
                }
                let exit = self.emit(OpCode::JumpForward, 0);
                handler_exit_jumps.push(exit);
            }
            // Unmatched: re-raise. Patch the last failed-match jump.
            while let Some(site) = next_handler_sites.pop() {
                let cur = self.next_offset();
                self.patch_jump(site, cur);
            }
            // Run finally on the re-raise path before propagating. The
            // unmatched exception stays on the stack until RERAISE.
            let saved = if pushed_finally {
                self.finally_stack.pop()
            } else {
                None
            };
            self.exc_on_stack += 1;
            for s in finalbody {
                self.compile_stmt(s)?;
            }
            self.exc_on_stack -= 1;
            if let Some(f) = saved {
                self.finally_stack.push(f);
            }
            self.emit(OpCode::Reraise, 0);
            // Shared finally-cleanup block for exceptions escaping an
            // except clause or the `else` body. Reached only through
            // the exception-table entries registered below; normal
            // flow jumps past it (handler exits patch to `end`).
            if has_finally {
                let cleanup_start = self.next_offset();
                let cleanup_push = self.emit(OpCode::PushExcInfo, 0);
                let saved = self.finally_stack.pop();
                // The escaping exception is on the stack until RERAISE.
                self.exc_on_stack += 1;
                for s in finalbody {
                    self.compile_stmt(s)?;
                }
                self.exc_on_stack -= 1;
                if let Some(f) = saved {
                    self.finally_stack.push(f);
                }
                self.emit(OpCode::Reraise, 0);
                let cleanup_end = self.next_offset();
                self.co.instructions[cleanup_push as usize].arg = cleanup_end;
                if orelse_end > orelse_start {
                    cleanup_ranges.push((orelse_start, orelse_end));
                }
                // Appended after any entries pushed while compiling
                // nested statements, so the forward "innermost-first"
                // scan in the VM still prefers those.
                for (s, e) in cleanup_ranges {
                    self.co.exception_table.push(ExcHandler {
                        start: s,
                        end: e,
                        handler: cleanup_start,
                        depth: body_depth,
                        push_lasti: false,
                    });
                }
            }
            // Patch handler-exit jumps to end.
            let end = self.next_offset();
            for site in handler_exit_jumps {
                self.patch_jump(site, end);
            }
            // Record the handler-body end on PUSH_EXC_INFO (see above).
            self.co.instructions[push_exc_site as usize].arg = end;
        } else if has_finally {
            // `try/finally` without except. The dispatch loop has
            // pushed the exception onto the value stack. We leave it
            // there across `finalbody` — every statement compiles to
            // stack-balanced bytecode — then RERAISE 0 pops it and
            // re-raises. Popping the exception eagerly (as we did
            // historically) left RERAISE with nothing to pop and
            // produced a `stack underflow` once the finally ran.
            self.co.exception_table.push(ExcHandler {
                start: body_start,
                end: body_end,
                handler: handlers_start,
                depth: body_depth,
                push_lasti: false,
            });
            // Record the propagating exception as the active handled
            // exception for the duration of the finally body. Without
            // this a `raise` inside `finally` (e.g. a `@contextmanager`
            // generator's `finally: raise`) gets no implicit
            // `__context__`, breaking PEP 3134 chaining. `PUSH_EXC_INFO`
            // only peeks the value-stack top in this VM, so the
            // exception stays put for the trailing `RERAISE 0`.
            let push_exc_site = self.emit(OpCode::PushExcInfo, 0);
            let saved = self.finally_stack.pop();
            // The propagating exception is on the stack until RERAISE;
            // nested handlers registered inside the finally body must
            // preserve that slot.
            self.exc_on_stack += 1;
            for s in finalbody {
                self.compile_stmt(s)?;
            }
            self.exc_on_stack -= 1;
            if let Some(f) = saved {
                self.finally_stack.push(f);
            }
            self.emit(OpCode::Reraise, 0);
            // Tag the active-handler entry with the pc just past the
            // RERAISE so the unwinder drops it when a `raise` inside the
            // finally escapes to an enclosing `try` (mirrors the
            // except-handler path above).
            let end = self.next_offset();
            self.co.instructions[push_exc_site as usize].arg = end;
        }
        // Patch normal exit jump to land after handlers/finally.
        if has_handlers || has_finally {
            let end = self.next_offset();
            self.patch_jump(normal_skip, end);
        }
        if pushed_finally {
            self.finally_stack.pop();
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
        let cm_name = format!(".with_cm{}", self.with_counter);
        self.with_counter += 1;
        let cm_idx = self.var_index_or_add(&cm_name);

        // Evaluate cm and stash it for later __exit__ access.
        self.compile_expr(&item.context_expr)?;
        self.current_line = with_line;
        self.current_span = with_span;
        self.emit(OpCode::StoreFast, cm_idx);

        // Call __enter__ and bind (or discard).
        self.emit(OpCode::LoadFast, cm_idx);
        self.emit(OpCode::BeforeWith, 0);
        if let Some(target) = &item.optional_vars {
            self.compile_assign(target)?;
        } else {
            self.emit(OpCode::PopTop, 0);
        }
        // After BEFORE_WITH the bound __exit__ remains at TOS. We
        // immediately pop it — the exit-path emission re-derives it
        // from the synthetic local.
        self.emit(OpCode::PopTop, 0);

        // Push a synthetic finally frame so `return`, `break`, and
        // `continue` from inside the body run `cm.__exit__(None, None, None)`
        // before transferring control. CPython does this via
        // CLEANUP_THROW / SETUP_WITH; we encode it as a `WithExit`
        // frame that emits the call from the cm's fast-local index.
        let with_loop_depth = self.loop_stack.len();
        self.finally_stack.push(FinallyFrame {
            kind: FinallyKind::WithExit { cm_idx },
            loop_depth_at_push: with_loop_depth,
        });

        let body_start = self.next_offset();
        if rest.is_empty() {
            for s in body {
                self.compile_stmt(s)?;
            }
        } else {
            self.compile_with(rest, body)?;
        }
        let body_end = self.next_offset();

        // Pop the synthetic frame; the explicit normal-exit path
        // below emits the same call inline.
        self.finally_stack.pop();

        // Attribute the whole exit path to this item's expression.
        self.current_line = with_line;
        self.current_span = with_span;

        // Normal exit: cm.__exit__(None, None, None).
        self.emit(OpCode::LoadFast, cm_idx);
        let exit_name = self.co.intern_name("__exit__");
        self.emit(OpCode::LoadAttr, exit_name);
        let none_idx = self.co.intern_constant(Constant::None);
        self.emit(OpCode::LoadConst, none_idx);
        self.emit(OpCode::LoadConst, none_idx);
        self.emit(OpCode::LoadConst, none_idx);
        self.emit(OpCode::Call, 3);
        self.emit(OpCode::PopTop, 0);
        let end_jump = self.emit(OpCode::JumpForward, 0);

        // Exception handler: __exit__(type(exc), exc, None); if truthy, swallow.
        let handler_start = self.next_offset();
        // RFC 0037 (WS2): the operand-stack depth to restore before
        // entering the handler must preserve every enclosing for-loop's
        // iterator (each lives on the stack for the loop's duration).
        // Hardcoding `0` truncated the stack to empty, so a `with` that
        // *suppressed* an exception inside a `for` lost the iterator and
        // the next `FOR_ITER` found an empty stack. This matches the
        // `body_depth` convention used by `try`/`except` handlers above.
        let body_depth = self.loop_stack.iter().filter(|fr| fr.is_for_loop).count() as u32
            + self.exc_on_stack;
        self.co.exception_table.push(ExcHandler {
            start: body_start,
            end: body_end,
            handler: handler_start,
            depth: body_depth,
            // CPython's SETUP_WITH cleanup carries the lasti flag: when
            // __exit__ doesn't suppress, RERAISE restores f_lasti to the
            // raising instruction inside the body (PEP 626).
            push_lasti: true,
        });
        // Stack: [exc]. Record the propagating exception as the active
        // handled exception for the duration of the `__exit__` call so a
        // `raise` inside `__exit__` chains it as the new exception's
        // implicit `__context__` (PEP 3134). This is what makes
        // `contextlib.ExitStack`'s `_fix_exception_context` work — it
        // walks each callback exception's context back to
        // `sys.exc_info()[1]`. `PUSH_EXC_INFO` only peeks the value-stack
        // top in this VM, so `[exc]` is preserved for `WITH_EXCEPT_START`.
        let push_exc_site = self.emit(OpCode::PushExcInfo, 0);
        self.emit(OpCode::LoadFast, cm_idx);
        self.emit(OpCode::LoadAttr, exit_name);
        // Stack: [exc, __exit__]
        self.emit(OpCode::Swap, 2);
        // Stack: [__exit__, exc]
        self.emit(OpCode::WithExceptStart, 0);
        // Stack: [__exit__, exc, result]
        let swallow = self.emit(OpCode::PopJumpIfTrue, 0);
        // Falsy: re-raise. Stack: [__exit__, exc]. CPython uses RERAISE
        // here: the original traceback is preserved and no entry is
        // recorded for the re-raise site.
        self.emit(OpCode::Swap, 2);
        self.emit(OpCode::PopTop, 0);
        self.emit(OpCode::Reraise, 0);
        let swallow_target = self.next_offset();
        self.patch_jump(swallow, swallow_target);
        // Swallowed: Stack: [__exit__, exc]. Drop the active handled-exc
        // entry now that the suppressing `__exit__` returned cleanly.
        self.emit(OpCode::PopExcept, 0);
        self.emit(OpCode::PopTop, 0);
        self.emit(OpCode::PopTop, 0);
        let end = self.next_offset();
        self.patch_jump(end_jump, end);
        // Tag the active-handler entry with the pc just past the handler
        // so the unwinder drops it if `__exit__` raises and the new
        // exception escapes to an enclosing `try`.
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
            if let Some(text) = self.annotation_source(annotation) {
                let idx = self.co.intern_constant(Constant::Str(text));
                self.emit(OpCode::LoadConst, idx);
                return Ok(());
            }
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
            ExprKind::Name(n) => {
                self.emit_store_name(n);
                Ok(())
            }
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
                if let Some(idx) = starred_idx {
                    let before = idx as u32;
                    let after = (items.len() - idx - 1) as u32;
                    if before > 0xFF || after > 0xFF {
                        return Err(CompileError::NotImplemented(
                            "starred unpack with more than 255 leading or trailing names",
                            "too many names on either side of the star",
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
            ExprKind::Starred(inner) => {
                // A bare top-level starred target (`*a = xs` outside
                // of any tuple/list pattern) is a `SyntaxError` in
                // CPython, but a `*a,` on its own is the special
                // one-element-tuple form. Compile the inner — the
                // surrounding tuple/list path is responsible for
                // emitting the UNPACK_EX.
                self.compile_assign(inner)
            }
            _ => Err(CompileError::BadAssignmentTarget(format!(
                "{:?}",
                target.kind
            ))),
        }
    }

    /// Lower a positional argument list containing one or more
    /// `*x` splats into a single tuple on the stack. Each contiguous
    /// run of non-starred args becomes a `BuildTuple`; each `*x` is
    /// added as another tuple. We then concatenate by repeated
    /// `BinaryOp::Add` because that already does the right thing for
    /// tuples.
    fn compile_starred_args_tuple(&mut self, args: &[Expr]) -> Result<(), CompileError> {
        let mut pending: Vec<&Expr> = Vec::new();
        let mut tuple_count: u32 = 0;
        let emit_pending = |slf: &mut Self,
                            pending: &mut Vec<&Expr>,
                            tuple_count: &mut u32|
         -> Result<(), CompileError> {
            if pending.is_empty() {
                return Ok(());
            }
            for e in pending.iter() {
                slf.compile_expr(e)?;
            }
            slf.emit(OpCode::BuildTuple, pending.len() as u32);
            pending.clear();
            *tuple_count += 1;
            Ok(())
        };
        for a in args {
            match &a.kind {
                ExprKind::Starred(inner) => {
                    emit_pending(self, &mut pending, &mut tuple_count)?;
                    // Coerce arbitrary iterable into a tuple. We load
                    // `tuple` first so the resulting stack lines up
                    // with `Call`'s expected layout (callable below
                    // args), then evaluate the iterable as its sole
                    // argument.
                    let tup_idx = self.co.intern_name("tuple");
                    self.emit(OpCode::LoadGlobal, tup_idx);
                    self.compile_expr(inner)?;
                    self.emit(OpCode::Call, 1);
                    tuple_count += 1;
                }
                _ => pending.push(a),
            }
        }
        emit_pending(self, &mut pending, &mut tuple_count)?;
        if tuple_count == 0 {
            self.emit(OpCode::BuildTuple, 0);
        } else {
            for _ in 1..tuple_count {
                self.emit(OpCode::BinaryOp, BinOpKind::Add as u32);
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
                    self.emit(OpCode::LoadAttr, m);
                    self.compile_expr(inner)?;
                }
                _ => {
                    let m = self.co.intern_name(single);
                    self.emit(OpCode::LoadAttr, m);
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
                let update_idx = self.co.intern_name("update");
                self.emit(OpCode::CopyTop, 0);
                self.emit(OpCode::LoadAttr, update_idx);
                self.compile_expr(&k.value)?;
                self.emit(OpCode::Call, 1);
                self.emit(OpCode::PopTop, 0);
            }
        }
        Ok(())
    }

    fn compile_delete(&mut self, target: &Expr) -> Result<(), CompileError> {
        match &target.kind {
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
            _ => Err(CompileError::BadAssignmentTarget(format!(
                "delete target: {:?}",
                target.kind
            ))),
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
            Binding::Global => {
                let idx = self.co.intern_name(name);
                if matches!(self.kind, CodeKind::Module | CodeKind::Class) {
                    self.emit(OpCode::DeleteName, idx);
                } else {
                    self.emit(OpCode::DeleteGlobal, idx);
                }
            }
        }
    }

    fn emit_store_name(&mut self, name: &str) {
        let binding = self.classify_for_store(name);
        match binding {
            Binding::Local => {
                let idx = self.var_index_or_add(name);
                self.emit(OpCode::StoreFast, idx);
            }
            Binding::Cell | Binding::Free | Binding::Nonlocal => {
                let idx = self.cell_or_free_index(name);
                self.emit(OpCode::StoreDeref, idx);
            }
            Binding::Global => {
                let idx = self.co.intern_name(name);
                if matches!(self.kind, CodeKind::Module | CodeKind::Class) {
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
        let binding = self.bindings.get(name).copied();
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
            Some(Binding::Global) | None => {
                let idx = self.co.intern_name(name);
                if matches!(self.kind, CodeKind::Module | CodeKind::Class) {
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
                self.compile_expr(operand)?;
                let kind = match op {
                    UnaryOp::UAdd => UnaryKind::Pos,
                    UnaryOp::USub => UnaryKind::Neg,
                    UnaryOp::Not => UnaryKind::Not,
                    UnaryOp::Invert => UnaryKind::Invert,
                };
                self.emit(OpCode::UnaryOp, kind as u32);
            }
            ExprKind::Compare {
                left,
                ops,
                comparators,
            } => {
                self.compile_compare(left, ops, comparators)?;
            }
            ExprKind::IfExp { test, body, orelse } => {
                self.compile_expr(test)?;
                let jump_else = self.emit(OpCode::PopJumpIfFalse, 0);
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
                let synthetic = Stmt {
                    kind: StmtKind::Return(Some((**body).clone())),
                    span: e.span,
                };
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
                    ExprKind::Attribute { attr, .. } => {
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
                self.compile_expr(func)?;
                if has_starred || has_kw_splat {
                    // Build a single args tuple by concatenating
                    // positional groups split on each `*x`. The VM's
                    // `CallEx` unpacks it once we land on the call.
                    self.compile_starred_args_tuple(args)?;
                    if !keywords.is_empty() || has_kw_splat {
                        self.compile_kwargs_dict(keywords)?;
                        emit_call(self, OpCode::CallEx, 1);
                    } else {
                        emit_call(self, OpCode::CallEx, 0);
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
                self.compile_expr(value)?;
                let idx = self.co.intern_name(attr);
                self.with_attr_location(e.span.end.0, attr.len() as u32, |c| {
                    c.emit(OpCode::LoadAttr, idx);
                });
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
                } else {
                    for x in items {
                        self.compile_expr(x)?;
                    }
                    self.emit(OpCode::BuildTuple, items.len() as u32);
                }
            }
            ExprKind::List(items) => {
                if items.iter().any(|x| matches!(x.kind, ExprKind::Starred(_))) {
                    self.compile_unpacking_sequence(items, OpCode::BuildList, "append", "extend")?;
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
                if !has_spread {
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
                return Err(CompileError::NotImplemented(
                    "starred expression",
                    "the slice doesn't support `*x` in this position",
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
                // a SyntaxError — CPython reports "'yield' outside function".
                // Catching it here also prevents a non-generator frame from
                // ever executing `YIELD_VALUE` at runtime.
                if self.kind != CodeKind::Function {
                    return Err(CompileError::YieldOutsideFunction("yield"));
                }
                if let Some(v) = value {
                    self.compile_expr(v)?;
                } else {
                    let idx = self.co.intern_constant(Constant::None);
                    self.emit(OpCode::LoadConst, idx);
                }
                // An async generator's *own* `yield` produces a value for the
                // consumer (`__anext__`), distinct from the `YIELD_VALUE` the
                // `await`/`yield from` dance emits to pass an inner
                // suspension's value through (oparg 0). The runtime uses this
                // marker (CPython's `PyAsyncGenWrappedValue`) to tell "the
                // agen yielded X" from "the agen is suspended on an inner
                // await that yielded X".
                let yield_arg = u32::from(self.co.is_async_generator);
                self.emit(OpCode::YieldValue, yield_arg);
            }
            ExprKind::YieldFrom(iter) => {
                if self.kind != CodeKind::Function {
                    return Err(CompileError::YieldOutsideFunction("yield from"));
                }
                // PEP 525: `yield from` is forbidden in `async def`
                // (only plain `yield` makes an async generator).
                if self.in_async_context() {
                    return Err(CompileError::SyntaxExact(
                        "'yield from' inside async function".to_owned(),
                    ));
                }
                // CPython 3.13 pattern:
                //   <iter>
                //   GET_YIELD_FROM_ITER
                //   LOAD_CONST None
                // loop:
                //   SEND end          ; pushes value or jumps with [iter, value]
                //   YIELD_VALUE       ; suspend; on resume sent_value at TOS
                //   JUMP_BACKWARD loop
                // end:
                //   END_SEND          ; stack: [iter, value] -> [value]
                self.compile_expr(iter)?;
                self.emit(OpCode::GetYieldFromIter, 0);
                let idx = self.co.intern_constant(Constant::None);
                self.emit(OpCode::LoadConst, idx);
                let loop_start = self.next_offset();
                let send = self.emit(OpCode::Send, 0);
                self.emit(OpCode::YieldValue, 0);
                let back = self.emit(OpCode::JumpBackward, 0);
                self.patch_jump(back, loop_start);
                let end = self.next_offset();
                self.patch_jump(send, end);
                self.emit(OpCode::EndSend, 0);
            }
            ExprKind::Await(value) => {
                if !self.in_async_context() {
                    return Err(CompileError::NotImplemented(
                        "`await` outside `async def`",
                        "wrap the expression in an `async def` function",
                    ));
                }
                self.compile_expr(value)?;
                self.compile_await_dance(0);
            }
        }
        Ok(())
    }

    /// Emit the "drive awaitable to completion" instruction sequence
    /// CPython 3.13 uses for `await`. Stack on entry: `[awaitable]`;
    /// stack on exit: `[result]`. `awaitable_arg` is passed to
    /// `GET_AWAITABLE` and selects the error message: 0 = plain
    /// `await`, 1 = `async for`'s `__anext__` result, 2 = `async
    /// with`'s `__aenter__` result, 3 = its `__aexit__` result.
    fn compile_await_dance(&mut self, awaitable_arg: u32) {
        self.emit(OpCode::GetAwaitable, awaitable_arg);
        let none_idx = self.co.intern_constant(Constant::None);
        self.emit(OpCode::LoadConst, none_idx);
        let loop_start = self.next_offset();
        let send = self.emit(OpCode::Send, 0);
        self.emit(OpCode::YieldValue, 0);
        let back = self.emit(OpCode::JumpBackward, 0);
        self.patch_jump(back, loop_start);
        let end = self.next_offset();
        self.patch_jump(send, end);
        // Stack: [iter, value]. Drop the iterator, keep the value.
        self.emit(OpCode::EndSend, 0);
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

    fn compile_async_for(
        &mut self,
        target: &Expr,
        iter: &Expr,
        body: &[Stmt],
        orelse: &[Stmt],
    ) -> Result<(), CompileError> {
        self.compile_expr(iter)?;
        self.emit(OpCode::GetAiter, 0);
        let loop_top = self.next_offset();
        // GetAnext peeks the aiter and pushes an awaitable. The
        // await-dance drives it; on success we land at the
        // STORE_FAST target. On StopAsyncIteration, control flows
        // to the cleanup block.
        let anext_site = self.emit(OpCode::GetAnext, 0);
        let _ = anext_site;
        self.compile_await_dance(1);
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
        });
        for s in body {
            self.compile_stmt(s)?;
        }
        let back = self.emit(OpCode::JumpBackward, 0);
        self.patch_jump(back, loop_top);
        let frame = self.loop_stack.pop().expect("loop frame");
        // Register an exception-table handler covering only the
        // `__anext__` await (loop header) so its `StopAsyncIteration`
        // lands at the cleanup label; body exceptions propagate. The
        // aiter stays at stack depth 1 across the whole loop body —
        // every per-iteration push lives above it.
        let cleanup_target = self.next_offset();
        self.co.exception_table.push(ExcHandler {
            start: loop_top,
            end: dance_end,
            handler: cleanup_target,
            depth: 1 + self.exc_on_stack,
            push_lasti: false,
        });
        // Cleanup: pop aiter + exception, then run the `else` clause.
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
        // BEFORE_ASYNC_WITH leaves [aexit, awaitable(aenter)].
        self.emit(OpCode::BeforeAsyncWith, 0);
        self.compile_await_dance(2);
        // Stack: [aexit, value].
        if let Some(target) = &head.optional_vars {
            self.compile_assign(target)?;
        } else {
            self.emit(OpCode::PopTop, 0);
        }
        // Stash aexit in a synthetic local so we can recover it on
        // both the normal-exit and the exception-cleanup paths.
        let slot = format!(".aexit{}", self.with_counter);
        self.with_counter += 1;
        let slot_idx = self.var_index_or_add(&slot);
        self.emit(OpCode::StoreFast, slot_idx);

        // Synthetic finally frame so `return`/`break`/`continue` out of
        // the body still `await __aexit__(None, None, None)`. Mirrors the
        // `WithExit` frame `compile_with` pushes; without it an early exit
        // from an `async with` body skipped the exit entirely (e.g. an
        // `@asynccontextmanager` used as a decorator never ran its
        // post-`yield` cleanup).
        let awith_loop_depth = self.loop_stack.len();
        self.finally_stack.push(FinallyFrame {
            kind: FinallyKind::AsyncWithExit {
                aexit_idx: slot_idx,
            },
            loop_depth_at_push: awith_loop_depth,
        });

        let body_start = self.next_offset();
        if rest.is_empty() {
            for s in body {
                self.compile_stmt(s)?;
            }
        } else {
            self.compile_async_with(rest, body)?;
        }
        let body_end = self.next_offset();

        // Pop the synthetic frame; the explicit normal-exit and
        // exception-cleanup paths below emit their own `__aexit__` call.
        self.finally_stack.pop();

        // Attribute the whole exit path to the `async with` line.
        self.current_line = with_line;
        self.current_span = with_span;

        // Normal exit: `await aexit(None, None, None)`.
        self.emit(OpCode::LoadFast, slot_idx);
        let none_idx = self.co.intern_constant(Constant::None);
        self.emit(OpCode::LoadConst, none_idx);
        self.emit(OpCode::LoadConst, none_idx);
        self.emit(OpCode::LoadConst, none_idx);
        self.emit(OpCode::Call, 3);
        self.compile_await_dance(3);
        self.emit(OpCode::PopTop, 0);
        let end_jump = self.emit(OpCode::JumpForward, 0);

        // Exception-cleanup path — the async counterpart of the handler
        // emitted by `compile_with`: `result = await aexit(type(exc), exc,
        // None)`; if `result` is truthy the exception is swallowed,
        // otherwise it is re-raised. The previous codegen omitted this
        // entirely, so an exception escaping an `async with` body never
        // reached `__aexit__` and could not be suppressed (the `with`
        // statement's `__exit__` already had this).
        let handler_start = self.next_offset();
        // Preserve enclosing for-loop iterators on the operand stack, the
        // same depth convention used by `try`/`except` and `compile_with`.
        let body_depth = self.loop_stack.iter().filter(|fr| fr.is_for_loop).count() as u32
            + self.exc_on_stack;
        self.co.exception_table.push(ExcHandler {
            start: body_start,
            end: body_end,
            handler: handler_start,
            depth: body_depth,
            // Same lasti semantics as the sync `with` cleanup.
            push_lasti: true,
        });
        // Stack: [exc]. Record the propagating exception as the active
        // handled exception for the duration of the awaited `__aexit__`,
        // exactly as the sync `with` handler does. Without it the body's
        // exception isn't visible via `sys.exc_info()` inside `__aexit__`
        // (a coroutine driven by the await dance below), so a `raise`
        // there gets no implicit `__context__` and
        // `contextlib.AsyncExitStack`'s `_fix_exception_context` (which
        // walks each callback exception back to `sys.exc_info()[1]`)
        // cannot reconstruct the chain.
        let push_exc_site = self.emit(OpCode::PushExcInfo, 0);
        self.emit(OpCode::LoadFast, slot_idx);
        // Stack: [exc, aexit]
        self.emit(OpCode::Swap, 2);
        // Stack: [aexit, exc]
        self.emit(OpCode::WithExceptStart, 0);
        // Stack: [aexit, exc, awaitable] — await the `__aexit__` coroutine.
        self.compile_await_dance(3);
        // Stack: [aexit, exc, result]
        let swallow = self.emit(OpCode::PopJumpIfTrue, 0);
        // Falsy: re-raise. Stack: [aexit, exc]. RERAISE preserves the
        // original traceback (no entry for the re-raise site).
        self.emit(OpCode::Swap, 2);
        self.emit(OpCode::PopTop, 0);
        self.emit(OpCode::Reraise, 0);
        let swallow_target = self.next_offset();
        self.patch_jump(swallow, swallow_target);
        // Swallowed: Stack: [aexit, exc]. Drop the active handled-exc
        // entry now that the suppressing `__aexit__` returned cleanly.
        self.emit(OpCode::PopExcept, 0);
        self.emit(OpCode::PopTop, 0);
        self.emit(OpCode::PopTop, 0);
        let end = self.next_offset();
        self.patch_jump(end_jump, end);
        // Tag the active-handler entry with the pc just past the handler
        // so the unwinder drops it if `__aexit__` raises a new exception.
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
        let mut arg: u32 = match conversion {
            -1 => 0,
            115 => 1, // 's'
            114 => 2, // 'r'
            97 => 3,  // 'a'
            other => {
                return Err(CompileError::Internal(format!(
                    "unknown f-string conversion {other}"
                )));
            }
        };
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
            self.future_annotations,
        );
        inner.current_line = self.current_line;
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
        // Free-variable resolution from outer scope.
        let mut reads = HashSet::new();
        collect_reads_expr(elt, &mut reads);
        if let Some(v) = value {
            collect_reads_expr(v, &mut reads);
        }
        for g in generators {
            collect_reads_expr(&g.iter, &mut reads);
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
        for name in reads {
            if inner.bindings.contains_key(&name) {
                continue;
            }
            if let Some(b) = self.bindings.get(&name) {
                if matches!(
                    b,
                    Binding::Local | Binding::Cell | Binding::Free | Binding::Nonlocal
                ) {
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
                // scope (passed in as `.0`); every later iter and every
                // filter runs inside this comprehension.
                if gi > 0 {
                    collect_inner_free_expr(&g.iter, &inner.bindings, &mut needed_in_inner);
                }
                for cond in &g.ifs {
                    collect_inner_free_expr(cond, &inner.bindings, &mut needed_in_inner);
                }
            }
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
        } else if is_async_comp {
            // Both async-generator comps and async list/set/dict
            // comps use the suspended-frame infrastructure.
            inner.emit(OpCode::ReturnGenerator, 0);
        }
        inner.emit_entry_resume();
        if let Some(op) = collector_op {
            inner.emit(op, 0);
        }
        // Outermost iterator comes in as `.0`.
        inner.emit(OpCode::LoadFast, 0);
        compile_comp_body(&mut inner, generators, 0, elt, value, append_op)?;
        if matches!(kind, CompKind::Generator) {
            // ForIter pops the iterator on exhaustion. Return None
            // so the generator finishes cleanly (the VM converts
            // this to `StopIteration`).
            let none_idx = inner.co.intern_constant(Constant::None);
            inner.emit(OpCode::LoadConst, none_idx);
            inner.emit(OpCode::ReturnValue, 0);
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
            .intern_constant(Constant::Code(Box::new(inner_code)));
        self.emit(OpCode::LoadConst, code_idx);
        self.emit(OpCode::MakeFunction, flags);
        // Push iterator of outermost generator as `.0`. For an async
        // comprehension we still pass the raw source — the inner
        // body fetches `aiter()` when it sees `is_async`.
        self.compile_expr(&generators[0].iter)?;
        if !(is_async_comp && generators[0].is_async) {
            self.emit(OpCode::GetIter, 0);
        }
        self.emit(OpCode::Call, 1);
        // For an async list/set/dict comprehension the call returned
        // a coroutine; the enclosing async function awaits it so the
        // final value (list/set/dict) ends up on the stack.
        if is_async_comp && !matches!(kind, CompKind::Generator) {
            if !self.in_async_context() {
                return Err(CompileError::NotImplemented(
                    "async comprehension outside `async def`",
                    "wrap in an `async def` function",
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

fn compile_comp_body(
    inner: &mut Compiler,
    generators: &[Comprehension],
    depth: usize,
    elt: &Expr,
    value: Option<&Expr>,
    append_op: OpCode,
) -> Result<(), CompileError> {
    if depth >= generators.len() {
        // Innermost: append (or map_add) to the accumulator. For
        // generator expressions, yield the element instead.
        match append_op {
            OpCode::MapAdd => {
                inner.compile_expr(elt)?;
                inner.compile_expr(value.expect("dict comp needs value"))?;
                let i = generators.len() + 1; // stack depth to accumulator
                inner.emit(OpCode::MapAdd, i as u32);
            }
            OpCode::YieldValue => {
                inner.compile_expr(elt)?;
                // An async-generator comprehension `(x async for x in xs)`
                // yields a consumer value here; mark it (arg 1) like a plain
                // async-gen `yield` so the runtime's passthrough machinery
                // doesn't mistake it for an inner-await suspension. Sync
                // genexps stay arg 0.
                inner.emit(OpCode::YieldValue, u32::from(inner.co.is_async_generator));
                inner.emit(OpCode::PopTop, 0);
            }
            _ => {
                inner.compile_expr(elt)?;
                let i = generators.len() + 1;
                inner.emit(append_op, i as u32);
            }
        }
        return Ok(());
    }
    let gen = &generators[depth];
    if gen.is_async {
        // depth==0: caller pushed the source expr (not yet GetAiter'd)
        // because compile_comprehension uses GetIter for the .0 arg.
        // We need to convert to async-iter here for the body.
        if depth == 0 {
            inner.emit(OpCode::PopTop, 0);
            inner.emit(OpCode::LoadFast, 0);
            inner.emit(OpCode::GetAiter, 0);
            inner.emit(OpCode::CopyTop, 0);
            inner.emit(OpCode::StoreFast, 0);
        } else {
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
        let outer_iters: u32 = generators.iter().take(depth).map(|_| 1u32).sum();
        let cleanup_depth = accumulator_depth + outer_iters + 1;
        let loop_top = inner.next_offset();
        inner.emit(OpCode::GetAnext, 0);
        inner.compile_await_dance(1);
        // As in `compile_async_for`: only the `__anext__` await may end
        // the loop via StopAsyncIteration (bpo-44895).
        let dance_end = inner.next_offset();
        inner.compile_assign(&gen.target)?;
        let mut filter_jumps = Vec::new();
        for cond in &gen.ifs {
            inner.compile_expr(cond)?;
            let jf = inner.emit(OpCode::PopJumpIfFalse, 0);
            filter_jumps.push(jf);
        }
        compile_comp_body(inner, generators, depth + 1, elt, value, append_op)?;
        for jf in filter_jumps {
            let cur = inner.next_offset();
            inner.patch_jump(jf, cur);
        }
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
        inner.compile_expr(&gen.iter)?;
        inner.emit(OpCode::GetIter, 0);
    }
    let loop_top = inner.next_offset();
    let for_site = inner.emit(OpCode::ForIter, 0);
    let for_line = inner.current_line;
    inner.compile_assign(&gen.target)?;
    let mut filter_jumps = Vec::new();
    for cond in &gen.ifs {
        inner.compile_expr(cond)?;
        let jf = inner.emit(OpCode::PopJumpIfFalse, 0);
        filter_jumps.push(jf);
    }
    compile_comp_body(inner, generators, depth + 1, elt, value, append_op)?;
    for jf in filter_jumps {
        let cur = inner.next_offset();
        inner.patch_jump(jf, cur);
    }
    let back = inner.emit(OpCode::JumpBackward, 0);
    inner.patch_jump(back, loop_top);
    let after = inner.next_offset();
    inner.patch_jump(for_site, after);
    // Keep END_FOR on the iterator line (see statement-level for loop) so a
    // comprehension's loop exhaustion does not emit a spurious `line` event.
    inner.set_span(gen.iter.span);
    inner.current_line = for_line;
    inner.emit(OpCode::EndFor, 0);
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
        FinallyKind::WithExit { cm_idx } => FinallyKind::WithExit { cm_idx: *cm_idx },
        FinallyKind::AsyncWithExit { aexit_idx } => {
            FinallyKind::AsyncWithExit { aexit_idx: *aexit_idx }
        }
    };
    FinallyFrame {
        kind,
        loop_depth_at_push: f.loop_depth_at_push,
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
fn collect_inner_free(
    stmt: &Stmt,
    outer_bindings: &IndexMap<String, Binding>,
    out: &mut HashSet<String>,
) {
    match &stmt.kind {
        StmtKind::FunctionDef { args, body, .. }
        | StmtKind::AsyncFunctionDef { args, body, .. } => {
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
    reads.contains("super") || reads.contains("__class__")
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

/// `True` if any statement in `body` contains a `yield` or `yield from`
/// in the immediate scope. Does NOT recurse into nested `def` / `lambda`
/// / comprehension bodies — those have their own scopes.
fn body_is_generator(body: &[Stmt]) -> bool {
    body.iter().any(stmt_contains_yield)
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
        ExprKind::Lambda { args, .. } => {
            args.defaults.iter().any(expr_contains_yield)
                || args
                    .kw_defaults
                    .iter()
                    .flatten()
                    .any(expr_contains_yield)
        }
        // A comprehension runs in its own scope, but the *leftmost* `for`
        // clause's iterable is evaluated in the enclosing scope and passed
        // in as the `.0` argument. A `yield` there therefore belongs to the
        // enclosing function and makes it a generator — e.g.
        // `def f(): list(i for i in [(yield 26)])`. (A `yield` anywhere else
        // in a comprehension is a SyntaxError, so only the first iterable
        // can contribute.)
        ExprKind::GeneratorExp { generators, .. } => {
            generators.first().is_some_and(|g| expr_contains_yield(&g.iter))
        }
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
        | ExprKind::DictComp { generators, .. } => {
            generators.first().is_some_and(|g| expr_contains_yield(&g.iter))
        }
        ExprKind::Starred(inner) => expr_contains_yield(inner),
        ExprKind::Constant(_) | ExprKind::Name(_) => false,
    }
}

/// `true` if `expr` contains an `await` at the surface scope (does
/// not descend into nested lambdas or comprehensions). Used to mark
/// comprehensions as coroutines.
fn expr_contains_await(expr: &Expr) -> bool {
    match &expr.kind {
        ExprKind::Await(_) => true,
        ExprKind::Yield(v) => v.as_deref().is_some_and(expr_contains_await),
        ExprKind::YieldFrom(v) => expr_contains_await(v),
        ExprKind::Lambda { .. } => false,
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
fn comp_clause_is_async(
    generators: &[Comprehension],
    elt: &Expr,
    value: Option<&Expr>,
) -> bool {
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
        ExprKind::Lambda { .. } => false,
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
        ExprKind::Lambda { args, body } => {
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
            for g in generators {
                collect_reads_deep(&g.iter, &mut reads);
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
            for g in generators {
                collect_reads_deep(&g.iter, &mut reads);
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
        _ => {}
    }
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
        StmtKind::AugAssign { target, .. } | StmtKind::AnnAssign { target, .. } => {
            collect_target_names(target, assigned);
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
        } => {
            collect_reads_expr(target, out);
            collect_reads_expr(annotation, out);
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
            ..
        }
        | StmtKind::AsyncFunctionDef {
            body,
            args,
            decorator_list,
            ..
        } => {
            // Reads inside an inner function are not "reads" in the
            // current scope from the perspective of scope analysis,
            // but defaults / annotations and decorators evaluate in
            // the OUTER scope.
            for d in decorator_list {
                collect_reads_expr(d, out);
            }
            for d in &args.defaults {
                collect_reads_expr(d, out);
            }
            for d in args.kw_defaults.iter().flatten() {
                collect_reads_expr(d, out);
            }
            for s in body {
                collect_reads_stmt(s, out);
            }
        }
        StmtKind::ClassDef {
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
        _ => {}
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
        ExprKind::Lambda { args, body } => {
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
        ExprKind::Lambda { args, body } => {
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
        let co = compile("1 + 2\n");
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

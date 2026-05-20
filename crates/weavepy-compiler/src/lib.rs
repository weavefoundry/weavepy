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
    ExceptHandler, Expr, ExprKind, Keyword as KwArg, Module, Stmt, StmtKind, UnaryOp, WithItem,
};

pub mod bytecode;

pub use bytecode::{BinOpKind, CompareKind, Instruction, OpCode, UnaryKind};

// ---------- error type ----------

#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum CompileError {
    #[error("`{0}` is not a valid assignment target")]
    BadAssignmentTarget(String),
    #[error("`break` outside loop")]
    BreakOutsideLoop,
    #[error("`continue` outside loop")]
    ContinueOutsideLoop,
    #[error("`return` outside function")]
    ReturnOutsideFunction,
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
    /// Source filename or `<string>`. Used for diagnostics only.
    pub filename: String,
    pub instructions: Vec<Instruction>,
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
                | OpCode::DeleteAttr => {
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
        Constant::Float(f) => {
            if f.fract() == 0.0 && f.is_finite() {
                format!("{f:.1}")
            } else {
                f.to_string()
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
#[derive(Debug, Clone, PartialEq)]
pub enum Constant {
    None,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    Bytes(Vec<u8>),
    Tuple(Vec<Constant>),
    Code(Box<CodeObject>),
    Ellipsis,
}

impl From<AstConstant> for Constant {
    fn from(c: AstConstant) -> Self {
        match c {
            AstConstant::None => Self::None,
            AstConstant::Bool(b) => Self::Bool(b),
            AstConstant::Int(i) => Self::Int(i),
            AstConstant::Float(f) => Self::Float(f),
            AstConstant::Str(s) => Self::Str(s),
            AstConstant::Bytes(b) => Self::Bytes(b),
            AstConstant::Tuple(xs) => Self::Tuple(xs.into_iter().map(Self::from).collect()),
            AstConstant::Ellipsis => Self::Ellipsis,
        }
    }
}

// ---------- public entry point ----------

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
    );
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
    /// Free variables (in declaration order) — populated by inner
    /// scopes looking up to their lexical parents.
    free_order: Vec<String>,
    /// Loop stack: each frame holds (continue_target, break_patch_sites).
    loop_stack: Vec<LoopFrame>,
    /// Monotonic counter for synthetic locals used by chained
    /// comparisons (`.chain0`, `.chain1`, …).
    chain_counter: u32,
    /// Monotonic counter for synthetic `with`-statement locals.
    with_counter: u32,
    /// Source byte→line table shared by every nested compiler from the
    /// same `compile_module_*` call.
    line_index: Rc<LineIndex>,
    /// Line number assigned to the next emitted instruction; updated as
    /// the compiler descends through the AST.
    current_line: u32,
    /// `True` for methods compiled inside a class body. Such methods
    /// implicitly capture the class's `__class__` cell so `super()`
    /// works without arguments.
    inside_class_body: bool,
}

struct LoopFrame {
    /// Offset of the first instruction of the loop body — branched
    /// to by `continue` and at the bottom of the loop after each
    /// iteration.
    continue_target: u32,
    /// Sites that need to be patched to jump past the loop on `break`.
    break_sites: Vec<u32>,
}

impl Compiler {
    fn new(name: String, filename: String, kind: CodeKind, line_index: Rc<LineIndex>) -> Self {
        let mut co = CodeObject::default();
        co.name = name;
        co.filename = filename;
        co.is_class_body = matches!(kind, CodeKind::Class);
        Self {
            co,
            kind,
            bindings: IndexMap::new(),
            free_order: Vec::new(),
            loop_stack: Vec::new(),
            chain_counter: 0,
            with_counter: 0,
            line_index,
            current_line: 0,
            inside_class_body: false,
        }
    }

    fn finish(mut self) -> CodeObject {
        // Emit an implicit `return None` if the trailing instruction
        // isn't already a return — matches CPython's module-level shape.
        let needs_return = self
            .co
            .instructions
            .last()
            .is_none_or(|ins| ins.op != OpCode::ReturnValue);
        if needs_return {
            let none_idx = self.co.intern_constant(Constant::None);
            self.emit(OpCode::LoadConst, none_idx);
            self.emit(OpCode::ReturnValue, 0);
        }
        // Place freevars (in declaration order) at the end of the
        // cells/freevars combined index space.
        self.co.freevars = self.free_order.clone();
        self.co
    }

    fn emit(&mut self, op: OpCode, arg: u32) -> u32 {
        let offset = self.co.instructions.len() as u32;
        self.co.instructions.push(Instruction { op, arg });
        self.co.linetable.push(self.current_line);
        offset
    }

    fn set_line_from(&mut self, byte: u32) {
        let line = self.line_index.line_for(byte);
        if line != 0 {
            self.current_line = line;
        }
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
            | OpCode::ForIter => {
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
        match &stmt.kind {
            StmtKind::Expr(e) => {
                self.compile_expr(e)?;
                self.emit(OpCode::PopTop, 0);
            }
            StmtKind::Pass => {}
            StmtKind::Assign { targets, value } => {
                self.compile_expr(value)?;
                let n = targets.len();
                for (i, t) in targets.iter().enumerate() {
                    if i + 1 < n {
                        self.emit(OpCode::CopyTop, 0);
                    }
                    self.compile_assign(t)?;
                }
            }
            StmtKind::AugAssign { target, op, value } => {
                self.compile_load_target(target)?;
                self.compile_expr(value)?;
                self.emit(OpCode::BinaryOp, bin_op_kind(*op) as u32);
                self.compile_assign(target)?;
            }
            StmtKind::AnnAssign {
                target,
                annotation: _,
                value,
            } => {
                if let Some(v) = value {
                    self.compile_expr(v)?;
                    self.compile_assign(target)?;
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
                });
                for s in body {
                    self.compile_stmt(s)?;
                }
                let back = self.emit(OpCode::JumpBackward, 0);
                self.patch_jump(back, loop_start);
                let frame = self.loop_stack.pop().expect("loop frame");
                let exit_target = self.next_offset();
                self.patch_jump(jump_exit, exit_target);
                for site in frame.break_sites {
                    self.patch_jump(site, exit_target);
                }
                for s in orelse {
                    self.compile_stmt(s)?;
                }
            }
            StmtKind::For {
                target,
                iter,
                body,
                orelse,
            } => {
                self.compile_expr(iter)?;
                self.emit(OpCode::GetIter, 0);
                let loop_top = self.next_offset();
                let for_site = self.emit(OpCode::ForIter, 0);
                self.compile_assign(target)?;
                self.loop_stack.push(LoopFrame {
                    continue_target: loop_top,
                    break_sites: Vec::new(),
                });
                for s in body {
                    self.compile_stmt(s)?;
                }
                let back = self.emit(OpCode::JumpBackward, 0);
                self.patch_jump(back, loop_top);
                let frame = self.loop_stack.pop().expect("loop frame");
                let after = self.next_offset();
                self.patch_jump(for_site, after);
                // `END_FOR` pops the exhausted iterator, matching
                // CPython's stack discipline.
                self.emit(OpCode::EndFor, 0);
                for site in frame.break_sites {
                    self.patch_jump(site, self.next_offset());
                }
                for s in orelse {
                    self.compile_stmt(s)?;
                }
            }
            StmtKind::FunctionDef {
                name,
                args,
                body,
                decorator_list,
            } => {
                self.compile_function_def(name, args, body, decorator_list)?;
            }
            StmtKind::ClassDef {
                name,
                bases,
                keywords,
                body,
                decorator_list,
            } => {
                self.compile_class_def(name, bases, keywords, body, decorator_list)?;
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
            StmtKind::Return(value) => {
                if self.kind != CodeKind::Function {
                    return Err(CompileError::ReturnOutsideFunction);
                }
                match value {
                    Some(v) => self.compile_expr(v)?,
                    None => {
                        let idx = self.co.intern_constant(Constant::None);
                        self.emit(OpCode::LoadConst, idx);
                    }
                }
                self.emit(OpCode::ReturnValue, 0);
            }
            StmtKind::Break => {
                let frame = self
                    .loop_stack
                    .last_mut()
                    .ok_or(CompileError::BreakOutsideLoop)?;
                let site = self.co.instructions.len() as u32;
                self.co.instructions.push(Instruction {
                    op: OpCode::JumpForward,
                    arg: 0,
                });
                frame.break_sites.push(site);
            }
            StmtKind::Continue => {
                let target = self
                    .loop_stack
                    .last()
                    .ok_or(CompileError::ContinueOutsideLoop)?
                    .continue_target;
                let site = self.emit(OpCode::JumpBackward, 0);
                self.patch_jump(site, target);
            }
            StmtKind::Global(_) | StmtKind::Nonlocal(_) => {
                // Scope analysis handled these — no code emission needed.
            }
            StmtKind::Import(_) | StmtKind::ImportFrom { .. } => {
                return Err(CompileError::NotImplemented(
                    "import",
                    "the slice parses imports but doesn't execute them",
                ));
            }
        }
        Ok(())
    }

    /// Compile a function definition statement: builds the function
    /// object, threads it through any decorators, and binds the result
    /// to `name` in the enclosing scope.
    fn compile_function_def(
        &mut self,
        name: &str,
        args: &AstArguments,
        body: &[Stmt],
        decorator_list: &[Expr],
    ) -> Result<(), CompileError> {
        for d in decorator_list {
            self.compile_expr(d)?;
        }
        self.build_function_object(name, args, body)?;
        for _ in decorator_list {
            self.emit(OpCode::Call, 1);
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
        let mut param_names: Vec<String> = Vec::new();
        for a in &args.posonlyargs {
            param_names.push(a.name.clone());
        }
        for a in &args.args {
            param_names.push(a.name.clone());
        }
        if let Some(va) = &args.vararg {
            param_names.push(va.name.clone());
        }
        for a in &args.kwonlyargs {
            param_names.push(a.name.clone());
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
        );
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
        inner.emit(OpCode::Resume, 0);
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
                let n = k.arg.clone().ok_or_else(|| {
                    CompileError::NotImplemented(
                        "**kwargs splat in class header",
                        "use explicit metaclass=… keyword form",
                    )
                })?;
                names.push(Constant::Str(n));
                self.compile_expr(&k.value)?;
            }
            let tup_idx = self.co.intern_constant(Constant::Tuple(names));
            self.emit(OpCode::LoadConst, tup_idx);
            self.emit(OpCode::CallKw, (bases.len() + 2) as u32);
        }
        for _ in decorator_list {
            self.emit(OpCode::Call, 1);
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
        );
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

        inner.emit(OpCode::Resume, 0);
        // `__module__ = __name__` and `__qualname__ = name` boilerplate.
        let name_const = inner.co.intern_constant(Constant::Str(name.to_owned()));
        let qualname_idx = inner.co.intern_name("__qualname__");
        inner.emit(OpCode::LoadConst, name_const);
        inner.emit(OpCode::StoreName, qualname_idx);

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
            // Empty try is meaningless; parse already rejected.
            for s in body {
                self.compile_stmt(s)?;
            }
            return Ok(());
        }
        // The handler entry point lives *after* the body. Exception
        // tables are walked when an exception fires; the dispatch
        // loop truncates the stack to `depth` and pushes the
        // exception value before jumping.
        let body_depth = 0u32;
        let body_start = self.next_offset();
        for s in body {
            self.compile_stmt(s)?;
        }
        // Else clause runs only on normal body completion.
        for s in orelse {
            self.compile_stmt(s)?;
        }
        let body_end = self.next_offset();

        // Normal-exit finally + jump to end. Falls through to the
        // finally body, then skips past the exception handlers.
        let normal_skip = if has_handlers || has_finally {
            // Run finally on normal exit.
            for s in finalbody {
                self.compile_stmt(s)?;
            }
            self.emit(OpCode::JumpForward, 0)
        } else {
            self.next_offset()
        };

        // Handlers begin here.
        let handlers_start = self.next_offset();
        if has_handlers {
            self.co.exception_table.push(ExcHandler {
                start: body_start,
                end: body_end,
                handler: handlers_start,
                depth: body_depth,
            });
            self.emit(OpCode::PushExcInfo, 0);
            // Stack on entry: [exc] (pushed by dispatch loop).
            let mut next_handler_sites: Vec<u32> = Vec::new();
            let mut handler_exit_jumps: Vec<u32> = Vec::new();
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
                for s in &h.body {
                    self.compile_stmt(s)?;
                }
                self.emit(OpCode::PopExcept, 0);
                // Run finally on the matched path.
                for s in finalbody {
                    self.compile_stmt(s)?;
                }
                let exit = self.emit(OpCode::JumpForward, 0);
                handler_exit_jumps.push(exit);
            }
            // Unmatched: re-raise. Patch the last failed-match jump.
            while let Some(site) = next_handler_sites.pop() {
                let cur = self.next_offset();
                self.patch_jump(site, cur);
            }
            // Run finally on the re-raise path before propagating.
            for s in finalbody {
                self.compile_stmt(s)?;
            }
            self.emit(OpCode::Reraise, 0);
            // Patch handler-exit jumps to end.
            let end = self.next_offset();
            for site in handler_exit_jumps {
                self.patch_jump(site, end);
            }
        } else if has_finally {
            // `try/finally` without except. Exception path runs
            // finally and re-raises.
            self.co.exception_table.push(ExcHandler {
                start: body_start,
                end: body_end,
                handler: handlers_start,
                depth: body_depth,
            });
            self.emit(OpCode::PopTop, 0); // discard pushed exc; we'll re-raise below
            for s in finalbody {
                self.compile_stmt(s)?;
            }
            self.emit(OpCode::Reraise, 0);
        }
        // Patch normal exit jump to land after handlers/finally.
        if has_handlers || has_finally {
            let end = self.next_offset();
            self.patch_jump(normal_skip, end);
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
        // Recurse on multi-item: `with a, b: body` ≡ `with a: with b: body`.
        if items.len() > 1 {
            let inner = vec![Stmt {
                kind: StmtKind::With {
                    items: items[1..].to_vec(),
                    body: body.to_vec(),
                },
                span: weavepy_lexer::Span::new(0, 0),
            }];
            return self.compile_with(&items[..1], &inner);
        }
        let item = &items[0];
        let cm_name = format!(".with_cm{}", self.with_counter);
        self.with_counter += 1;
        let cm_idx = self.var_index_or_add(&cm_name);

        // Evaluate cm and stash it for later __exit__ access.
        self.compile_expr(&item.context_expr)?;
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

        let body_start = self.next_offset();
        for s in body {
            self.compile_stmt(s)?;
        }
        let body_end = self.next_offset();

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
        self.co.exception_table.push(ExcHandler {
            start: body_start,
            end: body_end,
            handler: handler_start,
            depth: 0,
        });
        // Stack: [exc]
        self.emit(OpCode::LoadFast, cm_idx);
        self.emit(OpCode::LoadAttr, exit_name);
        // Stack: [exc, __exit__]
        self.emit(OpCode::Swap, 2);
        // Stack: [__exit__, exc]
        self.emit(OpCode::WithExceptStart, 0);
        // Stack: [__exit__, exc, result]
        let swallow = self.emit(OpCode::PopJumpIfTrue, 0);
        // Falsy: re-raise. Stack: [__exit__, exc]
        self.emit(OpCode::Swap, 2);
        self.emit(OpCode::PopTop, 0);
        self.emit(OpCode::RaiseVarargs, 1);
        let swallow_target = self.next_offset();
        self.patch_jump(swallow, swallow_target);
        // Swallowed: Stack: [__exit__, exc]
        self.emit(OpCode::PopTop, 0);
        self.emit(OpCode::PopTop, 0);
        let end = self.next_offset();
        self.patch_jump(end_jump, end);
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

    fn compile_assign(&mut self, target: &Expr) -> Result<(), CompileError> {
        match &target.kind {
            ExprKind::Name(n) => {
                self.emit_store_name(n);
                Ok(())
            }
            ExprKind::Attribute { value, attr } => {
                self.compile_expr(value)?;
                let idx = self.co.intern_name(attr);
                self.emit(OpCode::StoreAttr, idx);
                Ok(())
            }
            ExprKind::Subscript { value, slice } => {
                self.compile_expr(value)?;
                self.compile_expr(slice)?;
                self.emit(OpCode::StoreSubscr, 0);
                Ok(())
            }
            ExprKind::Tuple(items) | ExprKind::List(items) => {
                self.emit(OpCode::UnpackSequence, items.len() as u32);
                for t in items {
                    self.compile_assign(t)?;
                }
                Ok(())
            }
            ExprKind::Starred(_) => Err(CompileError::NotImplemented(
                "starred assignment target",
                "tracked in RFC 0001 follow-ups",
            )),
            _ => Err(CompileError::BadAssignmentTarget(format!(
                "{:?}",
                target.kind
            ))),
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
                self.compile_expr(func)?;
                for a in args {
                    self.compile_expr(a)?;
                }
                if keywords.is_empty() {
                    self.emit(OpCode::Call, args.len() as u32);
                } else {
                    // Pack kw names into a tuple constant and push
                    // the values; the VM zips them at call time.
                    let mut names: Vec<Constant> = Vec::with_capacity(keywords.len());
                    for k in keywords {
                        let n = k.arg.clone().ok_or_else(|| {
                            CompileError::NotImplemented(
                                "**kwargs splat",
                                "the slice handles named kwargs but not **splat",
                            )
                        })?;
                        names.push(Constant::Str(n));
                        self.compile_expr(&k.value)?;
                    }
                    let tup_idx = self.co.intern_constant(Constant::Tuple(names));
                    self.emit(OpCode::LoadConst, tup_idx);
                    self.emit(OpCode::CallKw, args.len() as u32);
                }
            }
            ExprKind::Attribute { value, attr } => {
                self.compile_expr(value)?;
                let idx = self.co.intern_name(attr);
                self.emit(OpCode::LoadAttr, idx);
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
                for x in items {
                    self.compile_expr(x)?;
                }
                self.emit(OpCode::BuildTuple, items.len() as u32);
            }
            ExprKind::List(items) => {
                for x in items {
                    self.compile_expr(x)?;
                }
                self.emit(OpCode::BuildList, items.len() as u32);
            }
            ExprKind::Set(items) => {
                for x in items {
                    self.compile_expr(x)?;
                }
                self.emit(OpCode::BuildSet, items.len() as u32);
            }
            ExprKind::Dict { keys, values } => {
                for (k, v) in keys.iter().zip(values.iter()) {
                    match k {
                        Some(ke) => {
                            self.compile_expr(ke)?;
                            self.compile_expr(v)?;
                        }
                        None => {
                            return Err(CompileError::NotImplemented(
                                "**dict spread literal",
                                "the slice supports `{k: v}` but not `{**d}`",
                            ));
                        }
                    }
                }
                self.emit(OpCode::BuildMap, keys.len() as u32);
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
        }
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
        );
        inner.current_line = self.current_line;
        inner.co.arg_count = 1;
        inner.co.varnames.push(".0".to_owned());
        inner.bindings.insert(".0".to_owned(), Binding::Local);

        let collector_op = match kind {
            CompKind::List => OpCode::BuildList,
            CompKind::Set => OpCode::BuildSet,
            CompKind::Dict => OpCode::BuildMap,
            CompKind::Generator => OpCode::BuildList, // sliced as list for now
        };
        let append_op = match kind {
            CompKind::List | CompKind::Generator => OpCode::ListAppend,
            CompKind::Set => OpCode::SetAdd,
            CompKind::Dict => OpCode::MapAdd,
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
        // Collect names assigned by comprehension targets — they're locals.
        for g in generators {
            let mut assigned = HashSet::new();
            collect_target_names(&g.target, &mut assigned);
            for n in assigned {
                inner.bindings.insert(n, Binding::Local);
            }
        }

        inner.emit(OpCode::Resume, 0);
        inner.emit(collector_op, 0);
        // Outermost iterator comes in as `.0`.
        inner.emit(OpCode::LoadFast, 0);
        compile_comp_body(&mut inner, generators, 0, elt, value, append_op)?;
        let none_idx = inner.co.intern_constant(Constant::None);
        inner.emit(OpCode::LoadConst, none_idx);
        // ListAppend etc. left the accumulator on the stack; the
        // function returns that. We pushed None just to terminate
        // the implicit return — instead, return the accumulator.
        // Drop the None we just pushed.
        inner.emit(OpCode::PopTop, 0);
        inner.emit(OpCode::ReturnValue, 0);

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
        // Push iterator of outermost generator as `.0`.
        self.compile_expr(&generators[0].iter)?;
        self.emit(OpCode::GetIter, 0);
        self.emit(OpCode::Call, 1);
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
        // Innermost: append (or map_add) to the accumulator.
        match append_op {
            OpCode::MapAdd => {
                inner.compile_expr(elt)?;
                inner.compile_expr(value.expect("dict comp needs value"))?;
                let i = generators.len() + 1; // stack depth to accumulator
                inner.emit(OpCode::MapAdd, i as u32);
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
    // For depth 0, the iterator is already on the stack (`.0` was
    // pushed). For deeper levels, push and iter the source.
    if depth > 0 {
        inner.compile_expr(&gen.iter)?;
        inner.emit(OpCode::GetIter, 0);
    }
    let loop_top = inner.next_offset();
    let for_site = inner.emit(OpCode::ForIter, 0);
    inner.compile_assign(&gen.target)?;
    // Apply filters.
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
        StmtKind::FunctionDef { args, body, .. } => {
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
        StmtKind::If { body, orelse, .. }
        | StmtKind::While { body, orelse, .. }
        | StmtKind::For { body, orelse, .. } => {
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
        StmtKind::With { items, body } => {
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
        _ => {}
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
        StmtKind::FunctionDef { name, .. } | StmtKind::ClassDef { name, .. } => {
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
        StmtKind::With { items, body } => {
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
        StmtKind::For {
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
        StmtKind::FunctionDef { name, .. } | StmtKind::ClassDef { name, .. } => {
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
        StmtKind::With { items, body } => {
            for it in items {
                if let Some(target) = &it.optional_vars {
                    collect_target_names(target, assigned);
                }
            }
            for s in body {
                collect_decls(s, globals, nonlocals, assigned);
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

fn collect_reads_stmt(stmt: &Stmt, out: &mut HashSet<String>) {
    match &stmt.kind {
        StmtKind::Expr(e) | StmtKind::Return(Some(e)) => collect_reads_expr(e, out),
        StmtKind::Assign { value, .. } => collect_reads_expr(value, out),
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
        StmtKind::With { items, body } => {
            for it in items {
                collect_reads_expr(&it.context_expr, out);
            }
            for s in body {
                collect_reads_stmt(s, out);
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
        _ => {}
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
            collect_reads_expr(key, out);
            collect_reads_expr(value, out);
        }
        _ => {}
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

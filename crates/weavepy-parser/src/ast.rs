//! Abstract syntax tree for WeavePy.
//!
//! Node names and field names track CPython's `ast` module so that
//! tools written against `ast.parse` are easy to port. Each node has
//! a [`Span`] into the original source; spans are byte-based to match
//! the lexer.
//!
//! # Compatibility level
//!
//! - **Tracks CPython** for node names (`Module`, `FunctionDef`, etc.)
//!   and field names (`body`, `orelse`, `args`, `kwargs`, `target`,
//!   `iter`, `value`, …).
//! - **Experimental** for the slice of the grammar represented. The
//!   following CPython AST nodes are deliberately absent and are
//!   tracked in `docs/rfcs/0001-executable-slice.md`: `ClassDef`,
//!   `Try`, `ExceptHandler`, `Raise`, `With`/`AsyncWith`,
//!   `Match`/`MatchValue`, `Async*`, `Await`, `Yield`,
//!   `YieldFrom`, `JoinedStr`/`FormattedValue` (f-strings).

use weavepy_lexer::Span;

// ---------- top-level ----------

/// A complete Python module — the result of parsing a source file.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Module {
    pub body: Vec<Stmt>,
}

// ---------- statements ----------

#[derive(Debug, Clone, PartialEq)]
pub struct Stmt {
    pub kind: StmtKind,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub enum StmtKind {
    /// `def name(args): body`
    FunctionDef {
        name: String,
        args: Arguments,
        body: Vec<Stmt>,
        decorator_list: Vec<Expr>,
    },
    /// `class name(bases, **keywords): body`
    ClassDef {
        name: String,
        bases: Vec<Expr>,
        keywords: Vec<Keyword>,
        body: Vec<Stmt>,
        decorator_list: Vec<Expr>,
    },
    /// `return value`
    Return(Option<Expr>),
    /// `target = value` (and multi-target: `a = b = c = ...`)
    Assign {
        targets: Vec<Expr>,
        value: Expr,
    },
    /// `target op= value`
    AugAssign {
        target: Expr,
        op: BinOp,
        value: Expr,
    },
    /// `target: annotation = value` (annotation kept for parity with CPython;
    /// not interpreted by the compiler yet).
    AnnAssign {
        target: Expr,
        annotation: Expr,
        value: Option<Expr>,
    },
    /// `if test: body [else: orelse]` with elif chains folded into `orelse`.
    If {
        test: Expr,
        body: Vec<Stmt>,
        orelse: Vec<Stmt>,
    },
    /// `while test: body else: orelse`
    While {
        test: Expr,
        body: Vec<Stmt>,
        orelse: Vec<Stmt>,
    },
    /// `for target in iter: body else: orelse`
    For {
        target: Expr,
        iter: Expr,
        body: Vec<Stmt>,
        orelse: Vec<Stmt>,
    },
    /// `try: body except: handlers else: orelse finally: finalbody`
    Try {
        body: Vec<Stmt>,
        handlers: Vec<ExceptHandler>,
        orelse: Vec<Stmt>,
        finalbody: Vec<Stmt>,
    },
    /// `raise [exc [from cause]]`
    Raise {
        exc: Option<Expr>,
        cause: Option<Expr>,
    },
    /// `with items: body`
    With {
        items: Vec<WithItem>,
        body: Vec<Stmt>,
    },
    /// `import a, b as c`
    Import(Vec<Alias>),
    /// `from m import a, b as c` (or `from m import *`)
    ImportFrom {
        module: Option<String>,
        names: Vec<Alias>,
        level: u32,
    },
    /// `global a, b`
    Global(Vec<String>),
    /// `nonlocal a, b`
    Nonlocal(Vec<String>),
    /// Expression statement.
    Expr(Expr),
    Pass,
    Break,
    Continue,
}

/// `except [E [as e]]: body` clause.
#[derive(Debug, Clone, PartialEq)]
pub struct ExceptHandler {
    /// `None` means a bare `except:` clause.
    pub type_: Option<Expr>,
    pub name: Option<String>,
    pub body: Vec<Stmt>,
    pub span: Span,
}

/// `with expr [as target]` element. Multi-context `with` carries one per
/// listed item.
#[derive(Debug, Clone, PartialEq)]
pub struct WithItem {
    pub context_expr: Expr,
    pub optional_vars: Option<Expr>,
}

/// `import x as y` / `from m import (n[, n]…)` element.
#[derive(Debug, Clone, PartialEq)]
pub struct Alias {
    pub name: String,
    pub asname: Option<String>,
}

// ---------- function arguments ----------

/// Parameter list of a function or lambda. Mirrors CPython's
/// `ast.arguments` shape (with `posonlyargs`/`args`/`vararg`/
/// `kwonlyargs`/`kwarg`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Arguments {
    pub posonlyargs: Vec<Arg>,
    pub args: Vec<Arg>,
    pub vararg: Option<Arg>,
    pub kwonlyargs: Vec<Arg>,
    pub kw_defaults: Vec<Option<Expr>>,
    pub kwarg: Option<Arg>,
    pub defaults: Vec<Expr>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Arg {
    pub name: String,
    pub annotation: Option<Box<Expr>>,
    pub span: Span,
}

// ---------- expressions ----------

#[derive(Debug, Clone, PartialEq)]
pub struct Expr {
    pub kind: ExprKind,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ExprKind {
    /// Literal constant: `None`, `True`, `42`, `"hello"`, etc.
    Constant(Constant),
    /// Bare name reference.
    Name(String),
    /// `obj.attr`
    Attribute {
        value: Box<Expr>,
        attr: String,
    },
    /// `obj[idx]` — `idx` may be a `Slice`.
    Subscript {
        value: Box<Expr>,
        slice: Box<Expr>,
    },
    /// `a:b:c` — explicit slice node, used inside `Subscript.slice`.
    Slice {
        lower: Option<Box<Expr>>,
        upper: Option<Box<Expr>>,
        step: Option<Box<Expr>>,
    },
    BinOp {
        left: Box<Expr>,
        op: BinOp,
        right: Box<Expr>,
    },
    BoolOp {
        op: BoolOp,
        values: Vec<Expr>,
    },
    UnaryOp {
        op: UnaryOp,
        operand: Box<Expr>,
    },
    Compare {
        left: Box<Expr>,
        ops: Vec<CmpOp>,
        comparators: Vec<Expr>,
    },
    /// `value if test else orelse`
    IfExp {
        test: Box<Expr>,
        body: Box<Expr>,
        orelse: Box<Expr>,
    },
    /// `target := value`
    NamedExpr {
        target: Box<Expr>,
        value: Box<Expr>,
    },
    /// `lambda args: body`
    Lambda {
        args: Arguments,
        body: Box<Expr>,
    },
    Call {
        func: Box<Expr>,
        args: Vec<Expr>,
        keywords: Vec<Keyword>,
    },
    Tuple(Vec<Expr>),
    List(Vec<Expr>),
    Set(Vec<Expr>),
    Dict {
        keys: Vec<Option<Expr>>,
        values: Vec<Expr>,
    },
    ListComp {
        elt: Box<Expr>,
        generators: Vec<Comprehension>,
    },
    SetComp {
        elt: Box<Expr>,
        generators: Vec<Comprehension>,
    },
    DictComp {
        key: Box<Expr>,
        value: Box<Expr>,
        generators: Vec<Comprehension>,
    },
    GeneratorExp {
        elt: Box<Expr>,
        generators: Vec<Comprehension>,
    },
    /// `*expr` in a call / tuple / list. (CPython has a `Starred` node.)
    Starred(Box<Expr>),
}

/// `**kw=value` keyword argument in a call.
#[derive(Debug, Clone, PartialEq)]
pub struct Keyword {
    /// `None` represents `**kwargs` splat.
    pub arg: Option<String>,
    pub value: Expr,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Comprehension {
    pub target: Expr,
    pub iter: Expr,
    pub ifs: Vec<Expr>,
    pub is_async: bool,
}

// ---------- literal constants ----------

#[derive(Debug, Clone, PartialEq)]
pub enum Constant {
    None,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    Bytes(Vec<u8>),
    Tuple(Vec<Constant>),
    Ellipsis,
}

// ---------- operators ----------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mult,
    MatMult,
    Div,
    Mod,
    Pow,
    LShift,
    RShift,
    BitOr,
    BitXor,
    BitAnd,
    FloorDiv,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BoolOp {
    And,
    Or,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryOp {
    Invert,
    Not,
    UAdd,
    USub,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CmpOp {
    Eq,
    NotEq,
    Lt,
    LtE,
    Gt,
    GtE,
    Is,
    IsNot,
    In,
    NotIn,
}

impl CmpOp {
    pub fn as_str(self) -> &'static str {
        match self {
            CmpOp::Eq => "Eq",
            CmpOp::NotEq => "NotEq",
            CmpOp::Lt => "Lt",
            CmpOp::LtE => "LtE",
            CmpOp::Gt => "Gt",
            CmpOp::GtE => "GtE",
            CmpOp::Is => "Is",
            CmpOp::IsNot => "IsNot",
            CmpOp::In => "In",
            CmpOp::NotIn => "NotIn",
        }
    }
}

impl BinOp {
    pub fn as_str(self) -> &'static str {
        match self {
            BinOp::Add => "Add",
            BinOp::Sub => "Sub",
            BinOp::Mult => "Mult",
            BinOp::MatMult => "MatMult",
            BinOp::Div => "Div",
            BinOp::Mod => "Mod",
            BinOp::Pow => "Pow",
            BinOp::LShift => "LShift",
            BinOp::RShift => "RShift",
            BinOp::BitOr => "BitOr",
            BinOp::BitXor => "BitXor",
            BinOp::BitAnd => "BitAnd",
            BinOp::FloorDiv => "FloorDiv",
        }
    }
}

impl UnaryOp {
    pub fn as_str(self) -> &'static str {
        match self {
            UnaryOp::Invert => "Invert",
            UnaryOp::Not => "Not",
            UnaryOp::UAdd => "UAdd",
            UnaryOp::USub => "USub",
        }
    }
}

impl BoolOp {
    pub fn as_str(self) -> &'static str {
        match self {
            BoolOp::And => "And",
            BoolOp::Or => "Or",
        }
    }
}

// ---------- ast.dump-style rendering ----------

/// Render a [`Module`] in a form close to CPython's
/// `ast.dump(tree, indent=2)`. Used by the conformance harness.
pub fn dump_module(module: &Module) -> String {
    let mut out = String::new();
    out.push_str("Module(\n  body=[");
    if !module.body.is_empty() {
        out.push('\n');
        for s in &module.body {
            indent(&mut out, 4);
            dump_stmt(&mut out, s, 4);
            out.push_str(",\n");
        }
        indent(&mut out, 2);
    }
    out.push_str("],\n  type_ignores=[])");
    out
}

fn indent(out: &mut String, n: usize) {
    for _ in 0..n {
        out.push(' ');
    }
}

fn dump_stmt(out: &mut String, s: &Stmt, depth: usize) {
    use StmtKind as S;
    match &s.kind {
        S::FunctionDef {
            name,
            args,
            body,
            decorator_list,
        } => {
            out.push_str("FunctionDef(name='");
            out.push_str(name);
            out.push_str("', args=");
            dump_arguments(out, args, depth + 2);
            out.push_str(", body=");
            dump_stmt_block(out, body, depth + 2);
            out.push_str(", decorator_list=[");
            for (i, d) in decorator_list.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                dump_expr(out, d, depth);
            }
            out.push_str("], returns=None, type_comment=None)");
        }
        S::ClassDef {
            name,
            bases,
            keywords,
            body,
            decorator_list,
        } => {
            out.push_str("ClassDef(name='");
            out.push_str(name);
            out.push_str("', bases=[");
            for (i, b) in bases.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                dump_expr(out, b, depth);
            }
            out.push_str("], keywords=[");
            for (i, k) in keywords.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                out.push_str("keyword(arg=");
                match &k.arg {
                    Some(n) => {
                        out.push('\'');
                        out.push_str(n);
                        out.push('\'');
                    }
                    None => out.push_str("None"),
                }
                out.push_str(", value=");
                dump_expr(out, &k.value, depth);
                out.push(')');
            }
            out.push_str("], body=");
            dump_stmt_block(out, body, depth + 2);
            out.push_str(", decorator_list=[");
            for (i, d) in decorator_list.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                dump_expr(out, d, depth);
            }
            out.push_str("])");
        }
        S::Try {
            body,
            handlers,
            orelse,
            finalbody,
        } => {
            out.push_str("Try(body=");
            dump_stmt_block(out, body, depth + 2);
            out.push_str(", handlers=[");
            for (i, h) in handlers.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                out.push_str("ExceptHandler(type=");
                match &h.type_ {
                    Some(t) => dump_expr(out, t, depth),
                    None => out.push_str("None"),
                }
                out.push_str(", name=");
                match &h.name {
                    Some(n) => {
                        out.push('\'');
                        out.push_str(n);
                        out.push('\'');
                    }
                    None => out.push_str("None"),
                }
                out.push_str(", body=");
                dump_stmt_block(out, &h.body, depth + 2);
                out.push(')');
            }
            out.push_str("], orelse=");
            dump_stmt_block(out, orelse, depth + 2);
            out.push_str(", finalbody=");
            dump_stmt_block(out, finalbody, depth + 2);
            out.push(')');
        }
        S::Raise { exc, cause } => {
            out.push_str("Raise(exc=");
            match exc {
                Some(e) => dump_expr(out, e, depth),
                None => out.push_str("None"),
            }
            out.push_str(", cause=");
            match cause {
                Some(c) => dump_expr(out, c, depth),
                None => out.push_str("None"),
            }
            out.push(')');
        }
        S::With { items, body } => {
            out.push_str("With(items=[");
            for (i, it) in items.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                out.push_str("withitem(context_expr=");
                dump_expr(out, &it.context_expr, depth);
                out.push_str(", optional_vars=");
                match &it.optional_vars {
                    Some(v) => dump_expr(out, v, depth),
                    None => out.push_str("None"),
                }
                out.push(')');
            }
            out.push_str("], body=");
            dump_stmt_block(out, body, depth + 2);
            out.push(')');
        }
        S::Return(value) => {
            out.push_str("Return(value=");
            if let Some(v) = value {
                dump_expr(out, v, depth);
            } else {
                out.push_str("None");
            }
            out.push(')');
        }
        S::Assign { targets, value } => {
            out.push_str("Assign(targets=[");
            for (i, t) in targets.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                dump_expr(out, t, depth);
            }
            out.push_str("], value=");
            dump_expr(out, value, depth);
            out.push(')');
        }
        S::AugAssign { target, op, value } => {
            out.push_str("AugAssign(target=");
            dump_expr(out, target, depth);
            out.push_str(", op=");
            out.push_str(op.as_str());
            out.push_str("(), value=");
            dump_expr(out, value, depth);
            out.push(')');
        }
        S::AnnAssign {
            target,
            annotation,
            value,
        } => {
            out.push_str("AnnAssign(target=");
            dump_expr(out, target, depth);
            out.push_str(", annotation=");
            dump_expr(out, annotation, depth);
            out.push_str(", value=");
            if let Some(v) = value {
                dump_expr(out, v, depth);
            } else {
                out.push_str("None");
            }
            out.push_str(", simple=1)");
        }
        S::If { test, body, orelse } => {
            out.push_str("If(test=");
            dump_expr(out, test, depth);
            out.push_str(", body=");
            dump_stmt_block(out, body, depth + 2);
            out.push_str(", orelse=");
            dump_stmt_block(out, orelse, depth + 2);
            out.push(')');
        }
        S::While { test, body, orelse } => {
            out.push_str("While(test=");
            dump_expr(out, test, depth);
            out.push_str(", body=");
            dump_stmt_block(out, body, depth + 2);
            out.push_str(", orelse=");
            dump_stmt_block(out, orelse, depth + 2);
            out.push(')');
        }
        S::For {
            target,
            iter,
            body,
            orelse,
        } => {
            out.push_str("For(target=");
            dump_expr(out, target, depth);
            out.push_str(", iter=");
            dump_expr(out, iter, depth);
            out.push_str(", body=");
            dump_stmt_block(out, body, depth + 2);
            out.push_str(", orelse=");
            dump_stmt_block(out, orelse, depth + 2);
            out.push(')');
        }
        S::Import(aliases) => {
            out.push_str("Import(names=[");
            for (i, a) in aliases.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                dump_alias(out, a);
            }
            out.push_str("])");
        }
        S::ImportFrom {
            module,
            names,
            level,
        } => {
            out.push_str("ImportFrom(module=");
            match module {
                Some(m) => {
                    out.push('\'');
                    out.push_str(m);
                    out.push('\'');
                }
                None => out.push_str("None"),
            }
            out.push_str(", names=[");
            for (i, a) in names.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                dump_alias(out, a);
            }
            out.push_str("], level=");
            out.push_str(&level.to_string());
            out.push(')');
        }
        S::Global(names) => {
            out.push_str("Global(names=[");
            for (i, n) in names.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                out.push('\'');
                out.push_str(n);
                out.push('\'');
            }
            out.push_str("])");
        }
        S::Nonlocal(names) => {
            out.push_str("Nonlocal(names=[");
            for (i, n) in names.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                out.push('\'');
                out.push_str(n);
                out.push('\'');
            }
            out.push_str("])");
        }
        S::Expr(e) => {
            out.push_str("Expr(value=");
            dump_expr(out, e, depth);
            out.push(')');
        }
        S::Pass => out.push_str("Pass()"),
        S::Break => out.push_str("Break()"),
        S::Continue => out.push_str("Continue()"),
    }
}

fn dump_stmt_block(out: &mut String, stmts: &[Stmt], depth: usize) {
    out.push('[');
    for (i, s) in stmts.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        dump_stmt(out, s, depth);
    }
    out.push(']');
}

fn dump_alias(out: &mut String, a: &Alias) {
    out.push_str("alias(name='");
    out.push_str(&a.name);
    out.push_str("', asname=");
    match &a.asname {
        Some(n) => {
            out.push('\'');
            out.push_str(n);
            out.push('\'');
        }
        None => out.push_str("None"),
    }
    out.push(')');
}

fn dump_arguments(out: &mut String, a: &Arguments, depth: usize) {
    out.push_str("arguments(posonlyargs=[");
    for (i, x) in a.posonlyargs.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        dump_arg(out, x);
    }
    out.push_str("], args=[");
    for (i, x) in a.args.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        dump_arg(out, x);
    }
    out.push_str("], vararg=");
    match &a.vararg {
        Some(x) => dump_arg(out, x),
        None => out.push_str("None"),
    }
    out.push_str(", kwonlyargs=[");
    for (i, x) in a.kwonlyargs.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        dump_arg(out, x);
    }
    out.push_str("], kw_defaults=[");
    for (i, d) in a.kw_defaults.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        match d {
            Some(e) => dump_expr(out, e, depth),
            None => out.push_str("None"),
        }
    }
    out.push_str("], kwarg=");
    match &a.kwarg {
        Some(x) => dump_arg(out, x),
        None => out.push_str("None"),
    }
    out.push_str(", defaults=[");
    for (i, e) in a.defaults.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        dump_expr(out, e, depth);
    }
    out.push_str("])");
}

fn dump_arg(out: &mut String, a: &Arg) {
    out.push_str("arg(arg='");
    out.push_str(&a.name);
    out.push_str("', annotation=");
    match &a.annotation {
        Some(e) => dump_expr(out, e.as_ref(), 0),
        None => out.push_str("None"),
    }
    out.push_str(", type_comment=None)");
}

fn dump_expr(out: &mut String, e: &Expr, depth: usize) {
    use ExprKind as E;
    match &e.kind {
        E::Constant(c) => {
            out.push_str("Constant(value=");
            dump_constant(out, c);
            out.push_str(", kind=None)");
        }
        E::Name(n) => {
            out.push_str("Name(id='");
            out.push_str(n);
            out.push_str("', ctx=Load())");
        }
        E::Attribute { value, attr } => {
            out.push_str("Attribute(value=");
            dump_expr(out, value, depth);
            out.push_str(", attr='");
            out.push_str(attr);
            out.push_str("', ctx=Load())");
        }
        E::Subscript { value, slice } => {
            out.push_str("Subscript(value=");
            dump_expr(out, value, depth);
            out.push_str(", slice=");
            dump_expr(out, slice, depth);
            out.push_str(", ctx=Load())");
        }
        E::Slice { lower, upper, step } => {
            out.push_str("Slice(lower=");
            match lower {
                Some(e) => dump_expr(out, e, depth),
                None => out.push_str("None"),
            }
            out.push_str(", upper=");
            match upper {
                Some(e) => dump_expr(out, e, depth),
                None => out.push_str("None"),
            }
            out.push_str(", step=");
            match step {
                Some(e) => dump_expr(out, e, depth),
                None => out.push_str("None"),
            }
            out.push(')');
        }
        E::BinOp { left, op, right } => {
            out.push_str("BinOp(left=");
            dump_expr(out, left, depth);
            out.push_str(", op=");
            out.push_str(op.as_str());
            out.push_str("(), right=");
            dump_expr(out, right, depth);
            out.push(')');
        }
        E::BoolOp { op, values } => {
            out.push_str("BoolOp(op=");
            out.push_str(op.as_str());
            out.push_str("(), values=[");
            for (i, v) in values.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                dump_expr(out, v, depth);
            }
            out.push_str("])");
        }
        E::UnaryOp { op, operand } => {
            out.push_str("UnaryOp(op=");
            out.push_str(op.as_str());
            out.push_str("(), operand=");
            dump_expr(out, operand, depth);
            out.push(')');
        }
        E::Compare {
            left,
            ops,
            comparators,
        } => {
            out.push_str("Compare(left=");
            dump_expr(out, left, depth);
            out.push_str(", ops=[");
            for (i, o) in ops.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                out.push_str(o.as_str());
                out.push_str("()");
            }
            out.push_str("], comparators=[");
            for (i, c) in comparators.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                dump_expr(out, c, depth);
            }
            out.push_str("])");
        }
        E::IfExp { test, body, orelse } => {
            out.push_str("IfExp(test=");
            dump_expr(out, test, depth);
            out.push_str(", body=");
            dump_expr(out, body, depth);
            out.push_str(", orelse=");
            dump_expr(out, orelse, depth);
            out.push(')');
        }
        E::NamedExpr { target, value } => {
            out.push_str("NamedExpr(target=");
            dump_expr(out, target, depth);
            out.push_str(", value=");
            dump_expr(out, value, depth);
            out.push(')');
        }
        E::Lambda { args, body } => {
            out.push_str("Lambda(args=");
            dump_arguments(out, args, depth + 2);
            out.push_str(", body=");
            dump_expr(out, body, depth);
            out.push(')');
        }
        E::Call {
            func,
            args,
            keywords,
        } => {
            out.push_str("Call(func=");
            dump_expr(out, func, depth);
            out.push_str(", args=[");
            for (i, a) in args.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                dump_expr(out, a, depth);
            }
            out.push_str("], keywords=[");
            for (i, k) in keywords.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                out.push_str("keyword(arg=");
                match &k.arg {
                    Some(n) => {
                        out.push('\'');
                        out.push_str(n);
                        out.push('\'');
                    }
                    None => out.push_str("None"),
                }
                out.push_str(", value=");
                dump_expr(out, &k.value, depth);
                out.push(')');
            }
            out.push_str("])");
        }
        E::Tuple(items) | E::List(items) | E::Set(items) => {
            let label = match &e.kind {
                E::Tuple(_) => "Tuple",
                E::List(_) => "List",
                E::Set(_) => "Set",
                _ => unreachable!(),
            };
            out.push_str(label);
            out.push_str("(elts=[");
            for (i, x) in items.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                dump_expr(out, x, depth);
            }
            out.push_str("], ctx=Load())");
        }
        E::Dict { keys, values } => {
            out.push_str("Dict(keys=[");
            for (i, k) in keys.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                match k {
                    Some(e) => dump_expr(out, e, depth),
                    None => out.push_str("None"),
                }
            }
            out.push_str("], values=[");
            for (i, v) in values.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                dump_expr(out, v, depth);
            }
            out.push_str("])");
        }
        E::ListComp { elt, generators }
        | E::SetComp { elt, generators }
        | E::GeneratorExp { elt, generators } => {
            let label = match &e.kind {
                E::ListComp { .. } => "ListComp",
                E::SetComp { .. } => "SetComp",
                E::GeneratorExp { .. } => "GeneratorExp",
                _ => unreachable!(),
            };
            out.push_str(label);
            out.push_str("(elt=");
            dump_expr(out, elt, depth);
            out.push_str(", generators=[");
            for (i, g) in generators.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                dump_comprehension(out, g, depth);
            }
            out.push_str("])");
        }
        E::DictComp {
            key,
            value,
            generators,
        } => {
            out.push_str("DictComp(key=");
            dump_expr(out, key, depth);
            out.push_str(", value=");
            dump_expr(out, value, depth);
            out.push_str(", generators=[");
            for (i, g) in generators.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                dump_comprehension(out, g, depth);
            }
            out.push_str("])");
        }
        E::Starred(e) => {
            out.push_str("Starred(value=");
            dump_expr(out, e, depth);
            out.push_str(", ctx=Load())");
        }
    }
}

fn dump_comprehension(out: &mut String, c: &Comprehension, depth: usize) {
    out.push_str("comprehension(target=");
    dump_expr(out, &c.target, depth);
    out.push_str(", iter=");
    dump_expr(out, &c.iter, depth);
    out.push_str(", ifs=[");
    for (i, x) in c.ifs.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        dump_expr(out, x, depth);
    }
    out.push_str("], is_async=");
    out.push_str(if c.is_async { "1" } else { "0" });
    out.push(')');
}

fn dump_constant(out: &mut String, c: &Constant) {
    match c {
        Constant::None => out.push_str("None"),
        Constant::Bool(b) => out.push_str(if *b { "True" } else { "False" }),
        Constant::Int(i) => out.push_str(&i.to_string()),
        Constant::Float(f) => {
            // Match CPython repr style for common floats; full
            // parity is out of scope for the slice.
            if f.fract() == 0.0 && f.is_finite() {
                out.push_str(&format!("{:.1}", f));
            } else {
                out.push_str(&format!("{}", f));
            }
        }
        Constant::Str(s) => {
            out.push('\'');
            for ch in s.chars() {
                match ch {
                    '\\' => out.push_str("\\\\"),
                    '\'' => out.push_str("\\'"),
                    '\n' => out.push_str("\\n"),
                    '\r' => out.push_str("\\r"),
                    '\t' => out.push_str("\\t"),
                    c => out.push(c),
                }
            }
            out.push('\'');
        }
        Constant::Bytes(b) => {
            out.push_str("b'");
            for byte in b {
                match *byte {
                    b'\\' => out.push_str("\\\\"),
                    b'\'' => out.push_str("\\'"),
                    b'\n' => out.push_str("\\n"),
                    b'\r' => out.push_str("\\r"),
                    b'\t' => out.push_str("\\t"),
                    c if (0x20..0x7f).contains(&c) => out.push(c as char),
                    c => out.push_str(&format!("\\x{:02x}", c)),
                }
            }
            out.push('\'');
        }
        Constant::Tuple(items) => {
            out.push('(');
            for (i, x) in items.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                dump_constant(out, x);
            }
            if items.len() == 1 {
                out.push(',');
            }
            out.push(')');
        }
        Constant::Ellipsis => out.push_str("Ellipsis"),
    }
}

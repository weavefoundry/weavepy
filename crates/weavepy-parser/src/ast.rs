//! Abstract syntax tree types for WeavePy.
//!
//! The shape of these types intentionally tracks CPython's `ast` module so
//! that tools written against `ast.parse` can be ported with minimal changes.
//! Only a small placeholder subset is defined for now; the full grammar will
//! be filled in as the parser is built out.

use weavepy_lexer::Span;

/// Top-level Python module: the result of parsing a complete source file.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Module {
    pub body: Vec<Stmt>,
}

/// A Python statement.
#[derive(Debug, Clone, PartialEq)]
pub struct Stmt {
    pub kind: StmtKind,
    pub span: Span,
}

/// Variants of [`Stmt`]. Currently a placeholder.
#[derive(Debug, Clone, PartialEq)]
pub enum StmtKind {
    /// `pass`
    Pass,
    /// An expression evaluated for its side effects.
    Expr(Expr),
}

/// A Python expression.
#[derive(Debug, Clone, PartialEq)]
pub struct Expr {
    pub kind: ExprKind,
    pub span: Span,
}

/// Variants of [`Expr`]. Currently a placeholder.
#[derive(Debug, Clone, PartialEq)]
pub enum ExprKind {
    /// A numeric, string, or other literal constant.
    Constant(Constant),
    /// A bare name reference.
    Name(String),
}

/// Literal constants representable in Python source.
#[derive(Debug, Clone, PartialEq)]
pub enum Constant {
    None,
    Bool(bool),
    Int(i128),
    Float(f64),
    Str(String),
}

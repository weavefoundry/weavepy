//! Expression unparser mirroring CPython's `Python/ast_unparse.c`.
//!
//! PEP 563 (`from __future__ import annotations`) stores annotations as
//! the *unparsed* AST text, not the verbatim source slice: quotes are
//! normalised (`list["C2"]` → `list['C2']`), whitespace is canonicalised
//! (`List[ int ]` → `List[int]`), and redundant parentheses are dropped.
//!
//! Returns `None` for nodes the unparser does not cover (f-strings with
//! interpolations, compiler-internal nodes); callers fall back to the
//! raw source slice in that case.

use crate::ast::{
    Arguments, BinOp, BoolOp, CmpOp, Comprehension, Constant, Expr, ExprKind, UnaryOp,
};

/// Operator precedence levels, ordered exactly like `ast_unparse.c`'s
/// `enum { PR_TUPLE, ... PR_ATOM }`. A subexpression is parenthesised
/// when its own level is *lower* than the level demanded by context.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Level {
    Tuple = 0,
    Test = 1,    // `if`-`else`, `lambda`
    Or = 2,      // `or`
    And = 3,     // `and`
    Not = 4,     // `not`
    Cmp = 5,     // comparisons
    BOr = 6,     // `|` (PR_EXPR)
    BXor = 7,    // `^`
    BAnd = 8,    // `&`
    Shift = 9,   // `<<`, `>>`
    Arith = 10,  // `+`, `-`
    Term = 11,   // `*`, `@`, `/`, `%`, `//`
    Factor = 12, // unary `+`, `-`, `~`
    Power = 13,  // `**`
    Await = 14,  // `await`
    Atom = 15,
}

impl Level {
    fn succ(self) -> Level {
        match self {
            Level::Tuple => Level::Test,
            Level::Test => Level::Or,
            Level::Or => Level::And,
            Level::And => Level::Not,
            Level::Not => Level::Cmp,
            Level::Cmp => Level::BOr,
            Level::BOr => Level::BXor,
            Level::BXor => Level::BAnd,
            Level::BAnd => Level::Shift,
            Level::Shift => Level::Arith,
            Level::Arith => Level::Term,
            Level::Term => Level::Factor,
            Level::Factor => Level::Power,
            Level::Power => Level::Await,
            Level::Await | Level::Atom => Level::Atom,
        }
    }
}

/// Unparse `e` as CPython's `ast.unparse` would. `None` when the tree
/// contains a node this unparser does not support.
pub fn unparse_expr(e: &Expr) -> Option<String> {
    let mut out = String::new();
    write_expr(&mut out, e, Level::Test)?;
    Some(out)
}

fn write_expr(out: &mut String, e: &Expr, level: Level) -> Option<()> {
    match &e.kind {
        ExprKind::Constant(c) => write_constant(out, c),
        ExprKind::Name(n) => {
            out.push_str(n);
            Some(())
        }
        ExprKind::Attribute { value, attr } => {
            write_expr(out, value, Level::Atom)?;
            // `(1).x` needs the space to stay parseable (C comment:
            // "integers require a space for attribute access").
            if matches!(
                &value.kind,
                ExprKind::Constant(Constant::Int(_) | Constant::BigInt(_))
            ) {
                out.push_str(" .");
            } else {
                out.push('.');
            }
            out.push_str(attr);
            Some(())
        }
        ExprKind::Subscript { value, slice } => {
            write_expr(out, value, Level::Atom)?;
            out.push('[');
            write_expr(out, slice, Level::Tuple)?;
            out.push(']');
            Some(())
        }
        ExprKind::Slice { lower, upper, step } => {
            if let Some(l) = lower {
                write_expr(out, l, Level::Test)?;
            }
            out.push(':');
            if let Some(u) = upper {
                write_expr(out, u, Level::Test)?;
            }
            if let Some(s) = step {
                out.push(':');
                write_expr(out, s, Level::Test)?;
            }
            Some(())
        }
        ExprKind::BinOp { left, op, right } => {
            let (text, pr, rassoc) = binop_info(*op);
            paren(out, level > pr, |out| {
                write_expr(out, left, if rassoc { pr.succ() } else { pr })?;
                out.push(' ');
                out.push_str(text);
                out.push(' ');
                write_expr(out, right, if rassoc { pr } else { pr.succ() })
            })
        }
        ExprKind::BoolOp { op, values } => {
            let (text, pr) = match op {
                BoolOp::And => (" and ", Level::And),
                BoolOp::Or => (" or ", Level::Or),
            };
            paren(out, level > pr, |out| {
                for (i, v) in values.iter().enumerate() {
                    if i > 0 {
                        out.push_str(text);
                    }
                    write_expr(out, v, pr.succ())?;
                }
                Some(())
            })
        }
        ExprKind::UnaryOp { op, operand } => {
            let (text, pr) = match op {
                UnaryOp::Invert => ("~", Level::Factor),
                UnaryOp::Not => ("not ", Level::Not),
                UnaryOp::UAdd => ("+", Level::Factor),
                UnaryOp::USub => ("-", Level::Factor),
            };
            paren(out, level > pr, |out| {
                out.push_str(text);
                write_expr(out, operand, pr)
            })
        }
        ExprKind::Compare {
            left,
            ops,
            comparators,
        } => paren(out, level > Level::Cmp, |out| {
            write_expr(out, left, Level::Cmp.succ())?;
            for (op, right) in ops.iter().zip(comparators) {
                out.push(' ');
                out.push_str(cmpop_text(*op));
                out.push(' ');
                write_expr(out, right, Level::Cmp.succ())?;
            }
            Some(())
        }),
        ExprKind::IfExp { test, body, orelse } => paren(out, level > Level::Test, |out| {
            write_expr(out, body, Level::Test.succ())?;
            out.push_str(" if ");
            write_expr(out, test, Level::Test.succ())?;
            out.push_str(" else ");
            write_expr(out, orelse, Level::Test)
        }),
        ExprKind::NamedExpr { target, value } => paren(out, level > Level::Tuple, |out| {
            write_expr(out, target, Level::Atom)?;
            out.push_str(" := ");
            write_expr(out, value, Level::Atom)
        }),
        ExprKind::Lambda { args, body } => paren(out, level > Level::Test, |out| {
            if args_empty(args) {
                out.push_str("lambda");
            } else {
                out.push_str("lambda ");
                write_args(out, args)?;
            }
            out.push_str(": ");
            write_expr(out, body, Level::Test)
        }),
        ExprKind::Call {
            func,
            args,
            keywords,
        } => {
            write_expr(out, func, Level::Atom)?;
            out.push('(');
            let mut first = true;
            for a in args {
                if !first {
                    out.push_str(", ");
                }
                first = false;
                write_expr(out, a, Level::Test)?;
            }
            for kw in keywords {
                if !first {
                    out.push_str(", ");
                }
                first = false;
                match &kw.arg {
                    Some(name) => {
                        out.push_str(name);
                        out.push('=');
                        write_expr(out, &kw.value, Level::Test)?;
                    }
                    None => {
                        out.push_str("**");
                        write_expr(out, &kw.value, Level::BOr)?;
                    }
                }
            }
            out.push(')');
            Some(())
        }
        ExprKind::Tuple(items) => {
            if items.is_empty() {
                out.push_str("()");
                return Some(());
            }
            paren(out, level > Level::Tuple, |out| {
                if items.len() == 1 {
                    write_expr(out, &items[0], Level::Tuple)?;
                    out.push(',');
                    Some(())
                } else {
                    write_comma_seq(out, items)
                }
            })
        }
        ExprKind::List(items) => {
            out.push('[');
            write_comma_seq(out, items)?;
            out.push(']');
            Some(())
        }
        ExprKind::Set(items) => {
            out.push('{');
            write_comma_seq(out, items)?;
            out.push('}');
            Some(())
        }
        ExprKind::Dict { keys, values } => {
            out.push('{');
            for (i, (k, v)) in keys.iter().zip(values).enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                match k {
                    Some(k) => {
                        write_expr(out, k, Level::Test)?;
                        out.push_str(": ");
                        write_expr(out, v, Level::Test)?;
                    }
                    None => {
                        out.push_str("**");
                        write_expr(out, v, Level::BOr)?;
                    }
                }
            }
            out.push('}');
            Some(())
        }
        ExprKind::Starred(inner) => {
            out.push('*');
            write_expr(out, inner, Level::BOr)
        }
        ExprKind::ListComp { elt, generators } => {
            out.push('[');
            write_expr(out, elt, Level::Test)?;
            write_comprehensions(out, generators)?;
            out.push(']');
            Some(())
        }
        ExprKind::SetComp { elt, generators } => {
            out.push('{');
            write_expr(out, elt, Level::Test)?;
            write_comprehensions(out, generators)?;
            out.push('}');
            Some(())
        }
        ExprKind::DictComp {
            key,
            value,
            generators,
        } => {
            out.push('{');
            write_expr(out, key, Level::Test)?;
            out.push_str(": ");
            write_expr(out, value, Level::Test)?;
            write_comprehensions(out, generators)?;
            out.push('}');
            Some(())
        }
        ExprKind::GeneratorExp { elt, generators } => {
            out.push('(');
            write_expr(out, elt, Level::Test)?;
            write_comprehensions(out, generators)?;
            out.push(')');
            Some(())
        }
        ExprKind::Await(inner) => paren(out, level > Level::Await, |out| {
            out.push_str("await ");
            write_expr(out, inner, Level::Await)
        }),
        ExprKind::Yield(inner) => paren(out, level > Level::Tuple, |out| {
            match inner {
                Some(v) => {
                    out.push_str("yield ");
                    write_expr(out, v, Level::Test)?;
                }
                None => out.push_str("yield"),
            }
            Some(())
        }),
        ExprKind::YieldFrom(inner) => paren(out, level > Level::Tuple, |out| {
            out.push_str("yield from ");
            write_expr(out, inner, Level::Test)
        }),
        // f-strings with interpolations and compiler-internal nodes:
        // no faithful unparse — the caller falls back to raw source.
        ExprKind::JoinedStr(_) | ExprKind::FormattedValue { .. } | ExprKind::TypeParamFn { .. } => {
            None
        }
    }
}

fn binop_info(op: BinOp) -> (&'static str, Level, bool) {
    match op {
        BinOp::Add => ("+", Level::Arith, false),
        BinOp::Sub => ("-", Level::Arith, false),
        BinOp::Mult => ("*", Level::Term, false),
        BinOp::MatMult => ("@", Level::Term, false),
        BinOp::Div => ("/", Level::Term, false),
        BinOp::Mod => ("%", Level::Term, false),
        BinOp::Pow => ("**", Level::Power, true),
        BinOp::LShift => ("<<", Level::Shift, false),
        BinOp::RShift => (">>", Level::Shift, false),
        BinOp::BitOr => ("|", Level::BOr, false),
        BinOp::BitXor => ("^", Level::BXor, false),
        BinOp::BitAnd => ("&", Level::BAnd, false),
        BinOp::FloorDiv => ("//", Level::Term, false),
    }
}

fn cmpop_text(op: CmpOp) -> &'static str {
    match op {
        CmpOp::Eq => "==",
        CmpOp::NotEq => "!=",
        CmpOp::Lt => "<",
        CmpOp::LtE => "<=",
        CmpOp::Gt => ">",
        CmpOp::GtE => ">=",
        CmpOp::Is => "is",
        CmpOp::IsNot => "is not",
        CmpOp::In => "in",
        CmpOp::NotIn => "not in",
    }
}

fn paren<F>(out: &mut String, needed: bool, f: F) -> Option<()>
where
    F: FnOnce(&mut String) -> Option<()>,
{
    if needed {
        out.push('(');
    }
    f(out)?;
    if needed {
        out.push(')');
    }
    Some(())
}

fn write_comma_seq(out: &mut String, items: &[Expr]) -> Option<()> {
    for (i, x) in items.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        write_expr(out, x, Level::Test)?;
    }
    Some(())
}

fn write_comprehensions(out: &mut String, gens: &[Comprehension]) -> Option<()> {
    for g in gens {
        out.push_str(if g.is_async { " async for " } else { " for " });
        write_expr(out, &g.target, Level::Tuple)?;
        out.push_str(" in ");
        write_expr(out, &g.iter, Level::Test.succ())?;
        for cond in &g.ifs {
            out.push_str(" if ");
            write_expr(out, cond, Level::Test.succ())?;
        }
    }
    Some(())
}

fn args_empty(a: &Arguments) -> bool {
    a.posonlyargs.is_empty()
        && a.args.is_empty()
        && a.vararg.is_none()
        && a.kwonlyargs.is_empty()
        && a.kwarg.is_none()
}

fn write_args(out: &mut String, a: &Arguments) -> Option<()> {
    let mut first = true;
    let n_pos = a.posonlyargs.len() + a.args.len();
    let n_defaults = a.defaults.len();
    let default_for = |i: usize| -> Option<&Expr> {
        // Defaults right-align against the positional parameters.
        (i + n_defaults)
            .checked_sub(n_pos)
            .map(|di| &a.defaults[di])
    };
    for (i, arg) in a.posonlyargs.iter().chain(&a.args).enumerate() {
        if !first {
            out.push_str(", ");
        }
        first = false;
        out.push_str(&arg.name);
        if let Some(d) = default_for(i) {
            out.push('=');
            write_expr(out, d, Level::Test)?;
        }
        if i + 1 == a.posonlyargs.len() {
            out.push_str(", /");
        }
    }
    if let Some(v) = &a.vararg {
        if !first {
            out.push_str(", ");
        }
        first = false;
        out.push('*');
        out.push_str(&v.name);
    } else if !a.kwonlyargs.is_empty() {
        if !first {
            out.push_str(", ");
        }
        first = false;
        out.push('*');
    }
    for (arg, d) in a.kwonlyargs.iter().zip(&a.kw_defaults) {
        if !first {
            out.push_str(", ");
        }
        first = false;
        out.push_str(&arg.name);
        if let Some(d) = d {
            out.push('=');
            write_expr(out, d, Level::Test)?;
        }
    }
    if let Some(k) = &a.kwarg {
        if !first {
            out.push_str(", ");
        }
        out.push_str("**");
        out.push_str(&k.name);
    }
    Some(())
}

fn write_constant(out: &mut String, c: &Constant) -> Option<()> {
    match c {
        Constant::None => out.push_str("None"),
        Constant::Bool(b) => out.push_str(if *b { "True" } else { "False" }),
        Constant::Int(i) => out.push_str(&i.to_string()),
        Constant::BigInt(repr) => out.push_str(repr),
        Constant::Float(f) => {
            if f.is_infinite() {
                // `append_ast_constant` writes an eval-able overflow
                // literal for infinities.
                out.push_str(if *f > 0.0 { "1e309" } else { "-1e309" });
            } else if f.fract() == 0.0 && f.abs() < 1e16 {
                out.push_str(&format!("{f:.1}"));
            } else {
                out.push_str(&format!("{f}"));
            }
        }
        Constant::Complex(real, imag) => {
            if *real == 0.0 {
                out.push_str(&format!("{imag}j"));
            } else {
                out.push('(');
                out.push_str(&format!("{real}"));
                if imag.is_sign_positive() {
                    out.push('+');
                }
                out.push_str(&format!("{imag}j)"));
            }
        }
        Constant::Str(s) => write_str_repr(out, s),
        Constant::WStr(_) => return None,
        Constant::Bytes(b) => {
            out.push_str("b'");
            for byte in b {
                match *byte {
                    b'\\' => out.push_str("\\\\"),
                    b'\'' => out.push_str("\\'"),
                    b'\n' => out.push_str("\\n"),
                    b'\r' => out.push_str("\\r"),
                    b'\t' => out.push_str("\\t"),
                    v if (0x20..0x7f).contains(&v) => out.push(v as char),
                    v => out.push_str(&format!("\\x{v:02x}")),
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
                write_constant(out, x)?;
            }
            if items.len() == 1 {
                out.push(',');
            }
            out.push(')');
        }
        // `ast.unparse` renders the Ellipsis constant as its literal.
        Constant::Ellipsis => out.push_str("..."),
    }
    Some(())
}

/// CPython `unicode_repr` quote selection: single quotes unless the
/// string contains `'` but no `"`.
fn write_str_repr(out: &mut String, s: &str) {
    let quote = if s.contains('\'') && !s.contains('"') {
        '"'
    } else {
        '\''
    };
    out.push(quote);
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if (c as u32) < 0x20 || (c as u32) == 0x7f => {
                out.push_str(&format!("\\x{:02x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push(quote);
}

//! CPython 3.14's AST preprocessor (`Python/ast_preprocess.c`) plus the
//! constant evaluators its flowgraph folds use (RFC 0068 WS1, RFC 0077
//! WS9).
//!
//! 3.14 moved constant folding out of the AST pass and into the
//! flowgraph optimizer (`fold_const_binop`, `fold_const_unaryop`,
//! `fold_tuple_of_constants`, `optimize_lists_and_sets`, ...). What is
//! left at the AST level is the `%`-format rewrite (`"%s" % (x,)` into
//! a JoinedStr) and the walk itself; `crate::flowgraph` calls the
//! evaluators below on `LOAD_CONST` operands instead.
//!
//! Evaluation is *exact*: a fold only happens when the compile-time
//! value is bit-identical to what the VM would compute at run time
//! (IEEE-754 arithmetic for floats, CPython's Smith-scaled complex
//! division, floor-division/modulo sign rules for ints). Anything else
//! — errors, overflow-prone conversions, out-of-range subscripts — is
//! left to run time, like CPython clearing the error and skipping the
//! fold.

use num_bigint::BigInt;
use num_integer::Integer;
use num_traits::{FromPrimitive, Pow, Signed, ToPrimitive, Zero};
use weavepy_parser::ast::{
    BinOp, Comprehension, Constant, Expr, ExprKind, Module, Stmt, StmtKind, TypeParamKind, UnaryOp,
};

/// `ast_opt.c` guards: don't create constants above these sizes.
const MAX_INT_SIZE: u64 = 128; // bits
const MAX_COLLECTION_SIZE: i64 = 256; // items
const MAX_STR_SIZE: i64 = 4096; // characters/bytes
const MAX_TOTAL_ITEMS: i64 = 1024; // including nested containers

/// Fold a whole module in place (the compiler entry points call this
/// on their working copy after validation). `pep563` mirrors
/// `ast_opt.c`'s `CO_FUTURE_ANNOTATIONS` check: under `from __future__
/// import annotations` the annotation expressions are stringified from
/// the *unoptimized* AST, so folding them would corrupt the recorded
/// source text (`test_future_stmt` asserts `'1 + 2 + 3'`, not `'6'`).
pub(crate) fn fold_module(module: &mut Module, pep563: bool) {
    for stmt in &mut module.body {
        fold_stmt(stmt, pep563);
    }
}

fn fold_body(body: &mut [Stmt], pep563: bool) {
    for stmt in body {
        fold_stmt(stmt, pep563);
    }
}

fn fold_stmt(stmt: &mut Stmt, pep563: bool) {
    match &mut stmt.kind {
        StmtKind::FunctionDef {
            args,
            body,
            decorator_list,
            type_params,
            returns,
            ..
        }
        | StmtKind::AsyncFunctionDef {
            args,
            body,
            decorator_list,
            type_params,
            returns,
            ..
        } => {
            for d in decorator_list {
                fold_expr(d, true);
            }
            for tp in type_params {
                if let TypeParamKind::TypeVar { bound: Some(b) } = &mut tp.kind {
                    fold_expr(b, true);
                }
            }
            for d in &mut args.defaults {
                fold_expr(d, true);
            }
            for d in args.kw_defaults.iter_mut().flatten() {
                fold_expr(d, true);
            }
            if !pep563 {
                for a in args
                    .posonlyargs
                    .iter_mut()
                    .chain(args.args.iter_mut())
                    .chain(args.vararg.iter_mut())
                    .chain(args.kwonlyargs.iter_mut())
                    .chain(args.kwarg.iter_mut())
                {
                    if let Some(ann) = &mut a.annotation {
                        fold_expr(ann, true);
                    }
                }
                if let Some(r) = returns {
                    fold_expr(r, true);
                }
            }
            fold_body(body, pep563);
        }
        StmtKind::ClassDef {
            bases,
            keywords,
            body,
            decorator_list,
            type_params,
            ..
        } => {
            for d in decorator_list {
                fold_expr(d, true);
            }
            for tp in type_params {
                if let TypeParamKind::TypeVar { bound: Some(b) } = &mut tp.kind {
                    fold_expr(b, true);
                }
            }
            for b in bases {
                fold_expr(b, true);
            }
            for kw in keywords {
                fold_expr(&mut kw.value, true);
            }
            fold_body(body, pep563);
        }
        StmtKind::TypeAlias { value, .. } => fold_expr(value, true),
        StmtKind::Return(Some(v)) => fold_expr(v, true),
        StmtKind::Return(None) => {}
        StmtKind::Assign { targets, value } => {
            for t in targets {
                fold_expr(t, false);
            }
            fold_expr(value, true);
        }
        StmtKind::AugAssign { target, value, .. } => {
            fold_expr(target, false);
            fold_expr(value, true);
        }
        StmtKind::AnnAssign {
            target,
            annotation,
            value,
            ..
        } => {
            fold_expr(target, false);
            if !pep563 {
                fold_expr(annotation, true);
            }
            if let Some(v) = value {
                fold_expr(v, true);
            }
        }
        StmtKind::If { test, body, orelse } | StmtKind::While { test, body, orelse } => {
            fold_expr(test, true);
            fold_body(body, pep563);
            fold_body(orelse, pep563);
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
            fold_expr(target, false);
            fold_expr(iter, true);
            fold_body(body, pep563);
            fold_body(orelse, pep563);
        }
        StmtKind::Try {
            body,
            handlers,
            orelse,
            finalbody,
        } => {
            fold_body(body, pep563);
            for h in handlers {
                if let Some(t) = &mut h.type_ {
                    fold_expr(t, true);
                }
                fold_body(&mut h.body, pep563);
            }
            fold_body(orelse, pep563);
            fold_body(finalbody, pep563);
        }
        StmtKind::Raise { exc, cause } => {
            if let Some(e) = exc {
                fold_expr(e, true);
            }
            if let Some(c) = cause {
                fold_expr(c, true);
            }
        }
        StmtKind::With { items, body } | StmtKind::AsyncWith { items, body } => {
            for item in items {
                fold_expr(&mut item.context_expr, true);
                if let Some(v) = &mut item.optional_vars {
                    fold_expr(v, false);
                }
            }
            fold_body(body, pep563);
        }
        StmtKind::Match { subject, cases } => {
            fold_expr(subject, true);
            for case in cases {
                // Patterns keep their literal shapes (codegen folds
                // pattern literals itself); guard and body fold.
                if let Some(g) = &mut case.guard {
                    fold_expr(g, true);
                }
                fold_body(&mut case.body, pep563);
            }
        }
        StmtKind::Expr(e) => fold_expr(e, true),
        StmtKind::Delete(targets) => {
            for t in targets {
                fold_expr(t, false);
            }
        }
        StmtKind::Assert { test, msg } => {
            fold_expr(test, true);
            if let Some(m) = msg {
                fold_expr(m, true);
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

fn fold_comprehensions(generators: &mut [Comprehension]) {
    for gen in generators {
        fold_expr(&mut gen.target, false);
        fold_expr(&mut gen.iter, true);
        for cond in &mut gen.ifs {
            fold_expr(cond, true);
        }
    }
}

/// Post-order walk. `load` is false in assignment/delete target
/// position (kept for the `ctx == Load` guards CPython's walk has).
fn fold_expr(e: &mut Expr, load: bool) {
    match &mut e.kind {
        ExprKind::Constant(_) | ExprKind::Name(_) => {}
        ExprKind::Attribute { value, .. } => fold_expr(value, true),
        ExprKind::Subscript { value, slice } => {
            fold_expr(value, true);
            fold_expr(slice, true);
        }
        ExprKind::Slice { lower, upper, step } => {
            for part in [lower, upper, step].into_iter().flatten() {
                fold_expr(part, true);
            }
        }
        ExprKind::BinOp { left, right, .. } => {
            fold_expr(left, true);
            fold_expr(right, true);
            fold_format(e);
        }
        ExprKind::BoolOp { values, .. } => {
            for v in values {
                fold_expr(v, true);
            }
        }
        ExprKind::UnaryOp { operand, .. } => {
            fold_expr(operand, true);
        }
        ExprKind::Compare {
            left, comparators, ..
        } => {
            fold_expr(left, true);
            for c in comparators.iter_mut() {
                fold_expr(c, true);
            }
        }
        ExprKind::IfExp { test, body, orelse } => {
            fold_expr(test, true);
            fold_expr(body, true);
            fold_expr(orelse, true);
        }
        ExprKind::NamedExpr { target, value } => {
            fold_expr(target, false);
            fold_expr(value, true);
        }
        ExprKind::Lambda { args, body } | ExprKind::TypeParamFn { args, body } => {
            for d in &mut args.defaults {
                fold_expr(d, true);
            }
            for d in args.kw_defaults.iter_mut().flatten() {
                fold_expr(d, true);
            }
            fold_expr(body, true);
        }
        ExprKind::Call {
            func,
            args,
            keywords,
        } => {
            fold_expr(func, true);
            for a in args {
                fold_expr(a, true);
            }
            for kw in keywords {
                fold_expr(&mut kw.value, true);
            }
        }
        ExprKind::Tuple(elts) => {
            for el in elts.iter_mut() {
                fold_expr(el, load);
            }
        }
        ExprKind::List(elts) => {
            for el in elts.iter_mut() {
                fold_expr(el, load);
            }
        }
        ExprKind::Set(elts) => {
            for el in elts.iter_mut() {
                fold_expr(el, true);
            }
        }
        ExprKind::Dict { keys, values } => {
            for k in keys.iter_mut().flatten() {
                fold_expr(k, true);
            }
            for v in values {
                fold_expr(v, true);
            }
        }
        ExprKind::ListComp { elt, generators } | ExprKind::SetComp { elt, generators } => {
            fold_expr(elt, true);
            fold_comprehensions(generators);
        }
        ExprKind::GeneratorExp { elt, generators } => {
            fold_expr(elt, true);
            fold_comprehensions(generators);
        }
        ExprKind::DictComp {
            key,
            value,
            generators,
        } => {
            fold_expr(key, true);
            fold_expr(value, true);
            fold_comprehensions(generators);
        }
        ExprKind::Starred(inner) => fold_expr(inner, load),
        ExprKind::Yield(Some(v)) => fold_expr(v, true),
        ExprKind::Yield(None) => {}
        ExprKind::YieldFrom(v) | ExprKind::Await(v) => fold_expr(v, true),
        ExprKind::JoinedStr(values) | ExprKind::TemplateStr(values) => {
            for v in values {
                fold_expr(v, true);
            }
        }
        ExprKind::FormattedValue {
            value, format_spec, ..
        }
        | ExprKind::Interpolation {
            value, format_spec, ..
        } => {
            fold_expr(value, true);
            if let Some(spec) = format_spec {
                fold_expr(spec, true);
            }
        }
    }
}

// ---------- individual folds ----------

/// `fold_binop` + `optimize_format`: `"literal %s %r" % (a, b)` with a
/// `str` constant on the left and a starless tuple display on the
/// right becomes the equivalent f-string (a JoinedStr of literal
/// pieces and FormattedValues). Only the `%s` / `%r` / `%a`
/// conversions with at most a width and precision qualify; anything
/// else leaves the expression alone.
fn fold_format(e: &mut Expr) {
    let ExprKind::BinOp { left, op, right } = &e.kind else {
        return;
    };
    if *op != BinOp::Mod {
        return;
    }
    let ExprKind::Constant(Constant::Str(fmt)) = &left.kind else {
        return;
    };
    let ExprKind::Tuple(elts) = &right.kind else {
        return;
    };
    if elts
        .iter()
        .any(|el| matches!(el.kind, ExprKind::Starred(_)))
    {
        return;
    }
    let Some(seq) = optimize_format(fmt, elts) else {
        return;
    };
    e.kind = ExprKind::JoinedStr(seq);
}

fn optimize_format(fmt: &str, elts: &[Expr]) -> Option<Vec<Expr>> {
    let chars: Vec<char> = fmt.chars().collect();
    let mut pos = 0usize;
    let mut cnt = 0usize;
    let mut seq: Vec<Expr> = Vec::with_capacity(elts.len() * 2 + 1);
    loop {
        if let Some(lit) = parse_literal(&chars, &mut pos) {
            seq.push(lit);
        }
        if pos >= chars.len() {
            break;
        }
        if cnt >= elts.len() {
            // More format units than items.
            return None;
        }
        debug_assert_eq!(chars[pos], '%');
        pos += 1;
        let expr = parse_format(&chars, &mut pos, &elts[cnt])?;
        cnt += 1;
        seq.push(expr);
    }
    if cnt < elts.len() {
        // More items than format units.
        return None;
    }
    Some(seq)
}

/// `parse_literal`: the run of non-`%` text (with `%%` collapsed) from
/// `*pos`, as a `Constant` node carrying no location (CPython builds
/// it with `-1` positions); `None` when empty.
fn parse_literal(chars: &[char], pos: &mut usize) -> Option<Expr> {
    let start = *pos;
    let mut p = start;
    let mut has_percents = false;
    while p < chars.len() {
        if chars[p] != '%' {
            p += 1;
        } else if p + 1 < chars.len() && chars[p + 1] == '%' {
            has_percents = true;
            p += 2;
        } else {
            break;
        }
    }
    *pos = p;
    if p == start {
        return None;
    }
    let mut text: String = chars[start..p].iter().collect();
    if has_percents {
        text = text.replace("%%", "%");
    }
    Some(Expr {
        kind: ExprKind::Constant(Constant::Str(text)),
        span: weavepy_lexer::Span::NO_LOCATION,
    })
}

const MAXDIGITS: usize = 3;

/// `simple_format_arg_parse`: flags, width, precision and the
/// conversion character of one format unit.
fn simple_format_arg_parse(
    chars: &[char],
    pos: &mut usize,
) -> Option<(char, bool, Option<u32>, Option<u32>)> {
    let mut p = *pos;
    let mut ljust = false;
    let mut ch;
    loop {
        ch = *chars.get(p)?;
        p += 1;
        match ch {
            '-' => {
                ljust = true;
                continue;
            }
            '+' | ' ' | '#' | '0' => continue,
            _ => break,
        }
    }
    let mut width = None;
    if ch.is_ascii_digit() {
        let mut w = 0u32;
        let mut digits = 0usize;
        while ch.is_ascii_digit() {
            w = w * 10 + ch.to_digit(10).unwrap();
            ch = *chars.get(p)?;
            p += 1;
            digits += 1;
            if digits >= MAXDIGITS {
                return None;
            }
        }
        width = Some(w);
    }
    let mut prec = None;
    if ch == '.' {
        ch = *chars.get(p)?;
        p += 1;
        let mut pr = 0u32;
        if ch.is_ascii_digit() {
            let mut digits = 0usize;
            while ch.is_ascii_digit() {
                pr = pr * 10 + ch.to_digit(10).unwrap();
                ch = *chars.get(p)?;
                p += 1;
                digits += 1;
                if digits >= MAXDIGITS {
                    return None;
                }
            }
        }
        prec = Some(pr);
    }
    *pos = p;
    Some((ch, ljust, width, prec))
}

/// `parse_format`: one `%s` / `%r` / `%a` unit as a FormattedValue.
fn parse_format(chars: &[char], pos: &mut usize, arg: &Expr) -> Option<Expr> {
    let (spec, ljust, width, prec) = simple_format_arg_parse(chars, pos)?;
    if !matches!(spec, 's' | 'r' | 'a') {
        return None;
    }
    let mut buf = String::new();
    if !ljust && width.is_some_and(|w| w > 0) {
        buf.push('>');
    }
    if let Some(w) = width {
        buf.push_str(&w.to_string());
    }
    if let Some(p) = prec {
        buf.push('.');
        buf.push_str(&p.to_string());
    }
    // CPython hands the spec over as a bare `Constant` with `-1`
    // positions (no `JoinedStr` wrapper, no location).
    let format_spec = if buf.is_empty() {
        None
    } else {
        Some(Box::new(Expr {
            kind: ExprKind::Constant(Constant::Str(buf)),
            span: weavepy_lexer::Span::NO_LOCATION,
        }))
    };
    Some(Expr {
        kind: ExprKind::FormattedValue {
            value: Box::new(arg.clone()),
            conversion: spec as i32,
            format_spec,
        },
        span: arg.span,
    })
}

/// CPython's `eval_const_unaryop` (flowgraph): the result of `op` on a
/// constant operand, or `None` when the runtime would raise (or when
/// the fold is deliberately skipped).
pub(crate) fn eval_unaryop_const(op: UnaryOp, c: &Constant) -> Option<Constant> {
    match op {
        UnaryOp::Not => Some(Constant::Bool(!truthy(c))),
        // `~bool` is deprecated (gh-103487); leave it unfolded so the
        // runtime DeprecationWarning fires (test_bool.test_math asserts
        // `eval("~False")` warns inside the assertWarns block).
        UnaryOp::Invert if matches!(c, Constant::Bool(_)) => None,
        UnaryOp::Invert => int_of(c).map(|v| int_const(!v)),
        UnaryOp::UAdd => match c {
            Constant::Int(_) | Constant::BigInt(_) => Some(c.clone()),
            Constant::Bool(b) => Some(Constant::Int(i64::from(*b))),
            Constant::Float(_) | Constant::Complex(..) => Some(c.clone()),
            _ => None,
        },
        UnaryOp::USub => match c {
            Constant::Bool(b) => Some(Constant::Int(-i64::from(*b))),
            Constant::Int(v) => Some(match v.checked_neg() {
                Some(n) => Constant::Int(n),
                None => int_const(-BigInt::from(*v)),
            }),
            Constant::BigInt(_) => int_of(c).map(|v| int_const(-v)),
            Constant::Float(f) => Some(Constant::Float(-f)),
            Constant::Complex(r, i) => Some(Constant::Complex(-r, -i)),
            _ => None,
        },
    }
}

/// CPython's `eval_const_binop` (flowgraph).
pub(crate) fn eval_binop_const(lv: &Constant, op: BinOp, rv: &Constant) -> Option<Constant> {
    eval_binop(lv, op, rv)
}

/// `PyObject_GetItem` on constants for an integer index (sequence
/// indexing of `str` / `bytes` / `tuple`); `None` when the runtime
/// would raise.
pub(crate) fn eval_subscr_const(container: &Constant, index: &Constant) -> Option<Constant> {
    let idx = int_of(index)?;
    let get = |len: i64| -> Option<usize> {
        let mut i = idx.to_i64()?;
        if i < 0 {
            i += len;
        }
        usize::try_from(i).ok().filter(|&i| (i as i64) < len)
    };
    match container {
        Constant::Str(s) => {
            let chars: Vec<char> = s.chars().collect();
            get(chars.len() as i64).map(|i| Constant::Str(chars[i].to_string()))
        }
        Constant::WStr(points) => get(points.len() as i64).map(|i| make_str(vec![points[i]])),
        Constant::Bytes(b) => get(b.len() as i64).map(|i| Constant::Int(i64::from(b[i]))),
        Constant::Tuple(items) => get(items.len() as i64).map(|i| items[i].clone()),
        _ => None,
    }
}

// ---------- the constant domain ----------

fn int_of(c: &Constant) -> Option<BigInt> {
    match c {
        Constant::Bool(b) => Some(BigInt::from(i64::from(*b))),
        Constant::Int(v) => Some(BigInt::from(*v)),
        Constant::BigInt(s) => s.parse().ok(),
        _ => None,
    }
}

fn int_const(v: BigInt) -> Constant {
    match v.to_i64() {
        Some(i) => Constant::Int(i),
        None => Constant::BigInt(v.to_string()),
    }
}

/// `PyLong_AsDouble` semantics: exact IEEE round-to-nearest, erroring
/// (→ no fold) when the magnitude exceeds the double range.
fn int_to_f64(v: &BigInt) -> Option<f64> {
    let f = v.to_f64()?;
    if f.is_finite() {
        Some(f)
    } else {
        None
    }
}

fn float_of(c: &Constant) -> Option<f64> {
    match c {
        Constant::Float(f) => Some(*f),
        _ => int_of(c).as_ref().and_then(int_to_f64),
    }
}

fn complex_of(c: &Constant) -> Option<(f64, f64)> {
    match c {
        Constant::Complex(r, i) => Some((*r, *i)),
        _ => float_of(c).map(|f| (f, 0.0)),
    }
}

pub(crate) fn truthy(c: &Constant) -> bool {
    match c {
        Constant::None => false,
        Constant::Bool(b) => *b,
        Constant::Int(v) => *v != 0,
        Constant::BigInt(s) => s.parse::<BigInt>().map(|v| !v.is_zero()).unwrap_or(true),
        Constant::Float(f) => *f != 0.0,
        Constant::Complex(r, i) => *r != 0.0 || *i != 0.0,
        Constant::Str(s) => !s.is_empty(),
        Constant::WStr(p) => !p.is_empty(),
        Constant::Bytes(b) => !b.is_empty(),
        Constant::Tuple(t) => !t.is_empty(),
        Constant::FrozenSet(s) => !s.is_empty(),
        Constant::Ellipsis => true,
    }
}

fn is_int_kind(c: &Constant) -> bool {
    matches!(
        c,
        Constant::Bool(_) | Constant::Int(_) | Constant::BigInt(_)
    )
}

fn is_float_kind(c: &Constant) -> bool {
    matches!(c, Constant::Float(_))
}

fn is_complex_kind(c: &Constant) -> bool {
    matches!(c, Constant::Complex(..))
}

fn is_numeric(c: &Constant) -> bool {
    is_int_kind(c) || is_float_kind(c) || is_complex_kind(c)
}

fn str_points(c: &Constant) -> Option<Vec<u32>> {
    match c {
        Constant::Str(s) => Some(s.chars().map(|ch| ch as u32).collect()),
        Constant::WStr(p) => Some(p.clone()),
        _ => None,
    }
}

fn make_str(points: Vec<u32>) -> Constant {
    if points.iter().all(|&p| char::from_u32(p).is_some()) {
        Constant::Str(
            points
                .into_iter()
                .map(|p| char::from_u32(p).unwrap())
                .collect(),
        )
    } else {
        Constant::WStr(points)
    }
}

/// `check_complexity`: items in nested containers, against a limit.
fn check_complexity(c: &Constant, mut limit: i64) -> i64 {
    match c {
        Constant::Tuple(items) | Constant::FrozenSet(items) => {
            limit -= items.len() as i64;
            for item in items {
                if limit < 0 {
                    break;
                }
                limit = check_complexity(item, limit);
            }
            limit
        }
        _ => limit,
    }
}

fn eval_binop(lv: &Constant, op: BinOp, rv: &Constant) -> Option<Constant> {
    match op {
        BinOp::Add => eval_add(lv, rv),
        BinOp::Sub => eval_numeric(lv, rv, |a, b| a - b, |a, b| Some(a - b), c_sub),
        BinOp::Mult => eval_mult(lv, rv),
        BinOp::Div => eval_div(lv, rv),
        BinOp::FloorDiv => eval_floordiv(lv, rv),
        BinOp::Mod => eval_mod(lv, rv),
        BinOp::Pow => eval_pow(lv, rv),
        BinOp::LShift => {
            let (a, b) = (int_of(lv)?, int_of(rv)?);
            if b.is_negative() {
                return None; // ValueError at run time
            }
            // safe_lshift: bits(a) + b > MAX_INT_SIZE → don't fold.
            if !a.is_zero() {
                let shift = b.to_u64()?;
                if a.bits() + shift > MAX_INT_SIZE {
                    return None;
                }
            }
            Some(int_const(a << b.to_u64()?))
        }
        BinOp::RShift => {
            let (a, b) = (int_of(lv)?, int_of(rv)?);
            if b.is_negative() {
                return None;
            }
            let shift = b.to_u64().unwrap_or(u64::MAX);
            if shift > 1_000_000 {
                // Result is 0 or -1; cheap either way, but stay simple.
                return Some(int_const(if a.is_negative() {
                    BigInt::from(-1)
                } else {
                    BigInt::from(0)
                }));
            }
            Some(int_const(a >> shift))
        }
        // `bool op bool` stays bool (`True & True is True` —
        // test_bool.test_boolean); anything else goes through int.
        BinOp::BitOr => Some(match (lv, rv) {
            (Constant::Bool(a), Constant::Bool(b)) => Constant::Bool(a | b),
            _ => int_const(int_of(lv)? | int_of(rv)?),
        }),
        BinOp::BitXor => Some(match (lv, rv) {
            (Constant::Bool(a), Constant::Bool(b)) => Constant::Bool(a ^ b),
            _ => int_const(int_of(lv)? ^ int_of(rv)?),
        }),
        BinOp::BitAnd => Some(match (lv, rv) {
            (Constant::Bool(a), Constant::Bool(b)) => Constant::Bool(a & b),
            _ => int_const(int_of(lv)? & int_of(rv)?),
        }),
        BinOp::MatMult => None,
    }
}

/// Numeric-only op with per-domain implementations (int exact, float
/// IEEE, complex componentwise). Mixed operands promote upward.
fn eval_numeric(
    lv: &Constant,
    rv: &Constant,
    int_op: impl Fn(BigInt, BigInt) -> BigInt,
    float_op: impl Fn(f64, f64) -> Option<f64>,
    complex_op: impl Fn((f64, f64), (f64, f64)) -> Option<(f64, f64)>,
) -> Option<Constant> {
    if !is_numeric(lv) || !is_numeric(rv) {
        return None;
    }
    if is_complex_kind(lv) || is_complex_kind(rv) {
        let (a, b) = (complex_of(lv)?, complex_of(rv)?);
        return complex_op(a, b).map(|(r, i)| Constant::Complex(r, i));
    }
    if is_float_kind(lv) || is_float_kind(rv) {
        let (a, b) = (float_of(lv)?, float_of(rv)?);
        return float_op(a, b).map(Constant::Float);
    }
    Some(int_const(int_op(int_of(lv)?, int_of(rv)?)))
}

fn c_sub(a: (f64, f64), b: (f64, f64)) -> Option<(f64, f64)> {
    Some((a.0 - b.0, a.1 - b.1))
}

fn eval_add(lv: &Constant, rv: &Constant) -> Option<Constant> {
    if is_numeric(lv) && is_numeric(rv) {
        return eval_numeric(
            lv,
            rv,
            |a, b| a + b,
            |a, b| Some(a + b),
            |a, b| Some((a.0 + b.0, a.1 + b.1)),
        );
    }
    if let (Some(a), Some(b)) = (str_points(lv), str_points(rv)) {
        let mut out = a;
        out.extend(b);
        return Some(make_str(out));
    }
    if let (Constant::Bytes(a), Constant::Bytes(b)) = (lv, rv) {
        let mut out = a.clone();
        out.extend_from_slice(b);
        return Some(Constant::Bytes(out));
    }
    if let (Constant::Tuple(a), Constant::Tuple(b)) = (lv, rv) {
        let mut out = a.clone();
        out.extend_from_slice(b);
        return Some(Constant::Tuple(out));
    }
    None
}

/// `safe_multiply` + `PyNumber_Multiply`.
fn eval_mult(lv: &Constant, rv: &Constant) -> Option<Constant> {
    if is_numeric(lv) && is_numeric(rv) {
        if is_int_kind(lv) && is_int_kind(rv) {
            let (a, b) = (int_of(lv)?, int_of(rv)?);
            // safe_multiply: both non-zero and bit-sum over the cap.
            if !a.is_zero() && !b.is_zero() && a.bits() + b.bits() > MAX_INT_SIZE {
                return None;
            }
            return Some(int_const(a * b));
        }
        return eval_numeric(
            lv,
            rv,
            |a, b| a * b,
            |a, b| Some(a * b),
            |a, b| Some(c_mul(a, b)),
        );
    }
    // int * str/bytes/tuple with the ast_opt size caps.
    let (n_c, seq) = if is_int_kind(lv) {
        (lv, rv)
    } else if is_int_kind(rv) {
        (rv, lv)
    } else {
        return None;
    };
    let n = int_of(n_c)?.to_i64()?;
    match seq {
        Constant::Str(_) | Constant::WStr(_) => {
            let points = str_points(seq)?;
            let size = points.len() as i64;
            if size > 0 && (n < 0 || n.checked_mul(size)? > MAX_STR_SIZE) {
                return None;
            }
            let reps = usize::try_from(n.max(0)).ok()?;
            Some(make_str(points.repeat(reps)))
        }
        Constant::Bytes(b) => {
            let size = b.len() as i64;
            if size > 0 && (n < 0 || n.checked_mul(size)? > MAX_STR_SIZE) {
                return None;
            }
            Some(Constant::Bytes(b.repeat(usize::try_from(n.max(0)).ok()?)))
        }
        Constant::Tuple(items) => {
            let size = items.len() as i64;
            if size > 0 {
                if n < 0 || n.checked_mul(size)? > MAX_COLLECTION_SIZE {
                    return None;
                }
                if n > 0 && check_complexity(seq, MAX_TOTAL_ITEMS / n) < 0 {
                    return None;
                }
            }
            let reps = usize::try_from(n.max(0)).ok()?;
            let mut out = Vec::with_capacity(items.len() * reps);
            for _ in 0..reps {
                out.extend(items.iter().cloned());
            }
            Some(Constant::Tuple(out))
        }
        _ => None,
    }
}

fn c_mul(a: (f64, f64), b: (f64, f64)) -> (f64, f64) {
    (a.0 * b.0 - a.1 * b.1, a.0 * b.1 + a.1 * b.0)
}

/// CPython's `_Py_c_quot` (Smith's scaled algorithm) — bit-exact with
/// what the VM computes at run time.
fn c_quot(a: (f64, f64), b: (f64, f64)) -> Option<(f64, f64)> {
    let (areal, aimag) = a;
    let (breal, bimag) = b;
    let abs_breal = breal.abs();
    let abs_bimag = bimag.abs();
    if abs_breal >= abs_bimag {
        if abs_breal == 0.0 {
            return None; // ZeroDivisionError at run time
        }
        let ratio = bimag / breal;
        let denom = breal + bimag * ratio;
        Some((
            (areal + aimag * ratio) / denom,
            (aimag - areal * ratio) / denom,
        ))
    } else if abs_bimag >= abs_breal {
        let ratio = breal / bimag;
        let denom = breal * ratio + bimag;
        Some((
            (areal * ratio + aimag) / denom,
            (aimag * ratio - areal) / denom,
        ))
    } else {
        Some((f64::NAN, f64::NAN))
    }
}

fn eval_div(lv: &Constant, rv: &Constant) -> Option<Constant> {
    if !is_numeric(lv) || !is_numeric(rv) {
        return None;
    }
    if is_complex_kind(lv) || is_complex_kind(rv) {
        return c_quot(complex_of(lv)?, complex_of(rv)?).map(|(r, i)| Constant::Complex(r, i));
    }
    if is_float_kind(lv) || is_float_kind(rv) {
        let (a, b) = (float_of(lv)?, float_of(rv)?);
        if b == 0.0 {
            return None;
        }
        return Some(Constant::Float(a / b));
    }
    // int / int → float, computed exactly like CPython's long_true_divide
    // for values within double range; skip the fold when either side
    // needs the correctly-rounded big-int path.
    let (a, b) = (int_of(lv)?, int_of(rv)?);
    if b.is_zero() {
        return None;
    }
    let (af, bf) = (int_to_f64(&a)?, int_to_f64(&b)?);
    if a.bits() > 53 || b.bits() > 53 {
        return None; // conversion would round; leave to the VM
    }
    Some(Constant::Float(af / bf))
}

fn eval_floordiv(lv: &Constant, rv: &Constant) -> Option<Constant> {
    if is_int_kind(lv) && is_int_kind(rv) {
        let (a, b) = (int_of(lv)?, int_of(rv)?);
        if b.is_zero() {
            return None;
        }
        return Some(int_const(a.div_floor(&b)));
    }
    if (is_float_kind(lv) || is_float_kind(rv))
        && is_numeric(lv)
        && is_numeric(rv)
        && !is_complex_kind(lv)
        && !is_complex_kind(rv)
    {
        let (a, b) = (float_of(lv)?, float_of(rv)?);
        if b == 0.0 {
            return None;
        }
        return Some(Constant::Float(py_float_floordiv(a, b)));
    }
    None
}

/// CPython's `_float_div_mod` floordiv leg, bit-exact.
fn py_float_floordiv(vx: f64, wx: f64) -> f64 {
    let m = vx % wx;
    let mut div = (vx - m) / wx;
    if m != 0.0 && (wx < 0.0) != (m < 0.0) {
        div -= 1.0;
    }
    if div != 0.0 {
        let floordiv = div.floor();
        if div - floordiv > 0.5 {
            return floordiv + 1.0;
        }
        floordiv
    } else {
        0.0_f64.copysign(vx / wx)
    }
}

/// Python float modulo: result has the divisor's sign.
fn py_fmod(a: f64, b: f64) -> Option<f64> {
    if b == 0.0 {
        return None;
    }
    let mut m = a % b;
    if m != 0.0 {
        if (m < 0.0) != (b < 0.0) {
            m += b;
        }
    } else {
        m = 0.0_f64.copysign(b);
    }
    Some(m)
}

fn eval_mod(lv: &Constant, rv: &Constant) -> Option<Constant> {
    // safe_mod: never fold str/bytes % (formatting).
    if matches!(
        lv,
        Constant::Str(_) | Constant::WStr(_) | Constant::Bytes(_)
    ) {
        return None;
    }
    if is_int_kind(lv) && is_int_kind(rv) {
        let (a, b) = (int_of(lv)?, int_of(rv)?);
        if b.is_zero() {
            return None;
        }
        return Some(int_const(a.mod_floor(&b)));
    }
    if (is_float_kind(lv) || is_float_kind(rv))
        && is_numeric(lv)
        && is_numeric(rv)
        && !is_complex_kind(lv)
        && !is_complex_kind(rv)
    {
        let (a, b) = (float_of(lv)?, float_of(rv)?);
        return py_fmod(a, b).map(Constant::Float);
    }
    None
}

fn eval_pow(lv: &Constant, rv: &Constant) -> Option<Constant> {
    if is_int_kind(lv) && is_int_kind(rv) {
        let (a, b) = (int_of(lv)?, int_of(rv)?);
        if b.is_negative() {
            // int ** -int → float; 0 base raises at run time. Restrict
            // to exactly-representable operands so the fold is
            // bit-identical to the VM's float fallback.
            if a.is_zero() || a.bits() > 53 || b.bits() > 53 {
                return None;
            }
            let (af, bf) = (int_to_f64(&a)?, int_to_f64(&b)?);
            return Some(Constant::Float(af.powf(bf)));
        }
        // safe_power: bits(base) * exp over the cap → don't fold.
        if !a.is_zero() && !b.is_zero() {
            let exp = b.to_u64()?;
            if a.bits() > MAX_INT_SIZE / exp.max(1) {
                return None;
            }
        }
        let exp = b.to_u64()?;
        return Some(int_const(a.pow(exp)));
    }
    if (is_float_kind(lv) || is_float_kind(rv))
        && is_numeric(lv)
        && is_numeric(rv)
        && !is_complex_kind(lv)
        && !is_complex_kind(rv)
    {
        let (a, b) = (float_of(lv)?, float_of(rv)?);
        // Negative base with a non-integral exponent goes to complex
        // (CPython falls back to complex_pow); 0.0 ** negative raises.
        if a < 0.0 && b.fract() != 0.0 {
            return None;
        }
        if a == 0.0 && b < 0.0 {
            return None;
        }
        return Some(Constant::Float(a.powf(b)));
    }
    // complex pow uses CPython's c_powi/c_pow rounding; leave to the VM.
    None
}

// ---------- Python-equality dedup for frozenset folds ----------

fn py_eq(a: &Constant, b: &Constant) -> bool {
    if is_numeric(a) && is_numeric(b) {
        if is_int_kind(a) && is_int_kind(b) {
            return int_of(a) == int_of(b);
        }
        if !is_complex_kind(a) && !is_complex_kind(b) {
            // int vs float: exact value comparison.
            let (Some(fa), Some(fb)) = (float_of(a), float_of(b)) else {
                return false;
            };
            if is_int_kind(a) || is_int_kind(b) {
                // Only exact when the float is integral and in range.
                let (int_side, float_side) = if is_int_kind(a) { (a, fb) } else { (b, fa) };
                if float_side.fract() != 0.0 || !float_side.is_finite() {
                    return false;
                }
                let Some(iv) = int_of(int_side) else {
                    return false;
                };
                return BigInt::from_f64(float_side)
                    .map(|fv| fv == iv)
                    .unwrap_or(false);
            }
            return fa == fb;
        }
        let (Some(ca), Some(cb)) = (complex_of(a), complex_of(b)) else {
            return false;
        };
        return ca == cb;
    }
    match (a, b) {
        (Constant::None, Constant::None) | (Constant::Ellipsis, Constant::Ellipsis) => true,
        (Constant::Str(_) | Constant::WStr(_), Constant::Str(_) | Constant::WStr(_)) => {
            str_points(a) == str_points(b)
        }
        (Constant::Bytes(x), Constant::Bytes(y)) => x == y,
        (Constant::Tuple(x), Constant::Tuple(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(i, j)| py_eq(i, j))
        }
        (Constant::FrozenSet(x), Constant::FrozenSet(y)) => {
            x.len() == y.len() && x.iter().all(|i| y.iter().any(|j| py_eq(i, j)))
        }
        _ => false,
    }
}

/// Set-literal dedup with Python equality (first occurrence wins,
/// matching `PyFrozenSet_New` insertion order semantics).
pub(crate) fn dedup_py(items: Vec<Constant>) -> Vec<Constant> {
    let mut out: Vec<Constant> = Vec::with_capacity(items.len());
    for item in items {
        if !out.iter().any(|existing| py_eq(existing, &item)) {
            out.push(item);
        }
    }
    out
}

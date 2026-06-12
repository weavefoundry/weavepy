//! Symtable-style validation pass (CPython `symtable.c` / `future.c`).
//!
//! Runs over the AST before code emission and raises the
//! compile-stage `SyntaxError`s CPython produces from its symbol-table
//! and `__future__` analysis: `global`/`nonlocal` declaration
//! conflicts, misplaced `import *`, duplicate parameters, annotation
//! target rules, comprehension/walrus rebinding rules, `__future__`
//! import placement and feature names, and `except` clause ordering.
//!
//! Error spans follow CPython: the reported location is the AST node's
//! position, with **byte**-based columns (`col_offset + 1`).

use weavepy_lexer::Span;
use weavepy_parser::ast::{
    Arguments, Comprehension, ExceptHandler, Expr, ExprKind, MatchCase, Module, Pattern, Stmt,
    StmtKind,
};

use crate::CompileError;

/// `__future__` features understood by CPython 3.13. All are mandatory
/// (no-ops) except `annotations`, which the compiler reads separately.
const KNOWN_FUTURES: &[&str] = &[
    "nested_scopes",
    "generators",
    "division",
    "absolute_import",
    "with_statement",
    "print_function",
    "unicode_literals",
    "barry_as_FLUFL",
    "generator_stop",
    "annotations",
];

#[derive(Clone, Copy, PartialEq, Eq)]
enum ScopeKind {
    Module,
    Function,
    Class,
}

/// One declaration recorded by a `global`/`nonlocal` statement —
/// CPython's `ste_directives`. The span is the *statement*'s position
/// (used as the error anchor for late-detected conflicts).
struct Directive {
    name: String,
    span: Span,
    is_global: bool,
}

struct Scope {
    kind: ScopeKind,
    /// Parameter names of a function scope.
    params: Vec<String>,
    directives: Vec<Directive>,
}

impl Scope {
    fn directive_for(&self, name: &str) -> Option<&Directive> {
        self.directives.iter().find(|d| d.name == name)
    }
}

pub(crate) fn validate_module(module: &Module, source: &str) -> Result<(), CompileError> {
    let mut v = Validator {
        source,
        scopes: vec![Scope {
            kind: ScopeKind::Module,
            params: Vec::new(),
            directives: Vec::new(),
        }],
    };
    // `from __future__ import …` placement / feature validation
    // (CPython `future.c`). Only a docstring, comments, and other
    // future imports may precede one.
    let mut prologue = true;
    for (i, stmt) in module.body.iter().enumerate() {
        match &stmt.kind {
            StmtKind::Expr(e)
                if i == 0
                    && matches!(
                        e.kind,
                        ExprKind::Constant(weavepy_parser::ast::Constant::Str(_))
                    ) =>
            {
                // Module docstring keeps the prologue open.
            }
            StmtKind::ImportFrom { module: m, .. } if m.as_deref() == Some("__future__") => {
                if !prologue {
                    return Err(CompileError::spanned(
                        "from __future__ imports must occur at the beginning of the file",
                        stmt.span,
                    ));
                }
            }
            _ => prologue = false,
        }
    }
    for stmt in &module.body {
        v.visit_stmt(stmt)?;
    }
    Ok(())
}

struct Validator<'src> {
    source: &'src str,
    scopes: Vec<Scope>,
}

impl Validator<'_> {
    fn scope(&self) -> &Scope {
        self.scopes.last().expect("scope stack never empty")
    }

    fn scope_mut(&mut self) -> &mut Scope {
        self.scopes.last_mut().expect("scope stack never empty")
    }

    fn visit_body(&mut self, body: &[Stmt]) -> Result<(), CompileError> {
        for s in body {
            self.visit_stmt(s)?;
        }
        Ok(())
    }

    fn visit_function(
        &mut self,
        args: &Arguments,
        body: &[Stmt],
        decorators: &[Expr],
        defaults_scope_ok: bool,
    ) -> Result<(), CompileError> {
        let _ = defaults_scope_ok;
        for d in decorators {
            self.visit_expr(d)?;
        }
        // Defaults and annotations evaluate in the *enclosing* scope.
        for d in &args.defaults {
            self.visit_expr(d)?;
        }
        for d in args.kw_defaults.iter().flatten() {
            self.visit_expr(d)?;
        }
        let mut params: Vec<(&str, Span)> = Vec::new();
        for a in args
            .posonlyargs
            .iter()
            .chain(&args.args)
            .chain(&args.kwonlyargs)
            .chain(&args.vararg)
            .chain(&args.kwarg)
        {
            if let Some(ann) = &a.annotation {
                self.visit_expr(ann)?;
            }
            if params.iter().any(|(n, _)| *n == a.name) {
                return Err(CompileError::spanned(
                    format!("duplicate argument '{}' in function definition", a.name),
                    a.span,
                ));
            }
            params.push((&a.name, a.span));
        }
        self.scopes.push(Scope {
            kind: ScopeKind::Function,
            params: params.iter().map(|(n, _)| (*n).to_owned()).collect(),
            directives: Vec::new(),
        });
        let result = self.visit_body(body);
        self.scopes.pop();
        result
    }

    fn visit_stmt(&mut self, stmt: &Stmt) -> Result<(), CompileError> {
        match &stmt.kind {
            StmtKind::FunctionDef {
                args,
                body,
                decorator_list,
                ..
            }
            | StmtKind::AsyncFunctionDef {
                args,
                body,
                decorator_list,
                ..
            } => {
                self.visit_function(args, body, decorator_list, true)?;
            }
            StmtKind::ClassDef {
                body,
                decorator_list,
                bases,
                keywords,
                ..
            } => {
                for d in decorator_list {
                    self.visit_expr(d)?;
                }
                for b in bases {
                    self.visit_expr(b)?;
                }
                for k in keywords {
                    self.visit_expr(&k.value)?;
                }
                self.scopes.push(Scope {
                    kind: ScopeKind::Class,
                    params: Vec::new(),
                    directives: Vec::new(),
                });
                let result = self.visit_body(body);
                self.scopes.pop();
                result?;
            }
            StmtKind::Global(names) => {
                let span = stmt.span;
                for n in names {
                    let scope = self.scope();
                    if scope.params.iter().any(|p| p == n) {
                        return Err(CompileError::spanned(
                            format!("name '{n}' is parameter and global"),
                            span,
                        ));
                    }
                    if let Some(d) = scope.directive_for(n) {
                        if !d.is_global {
                            // Earlier `nonlocal` — anchor at the first
                            // directive, as CPython's symtable does.
                            let at = d.span;
                            return Err(CompileError::spanned(
                                format!("name '{n}' is nonlocal and global"),
                                at,
                            ));
                        }
                    } else {
                        self.scope_mut().directives.push(Directive {
                            name: n.clone(),
                            span,
                            is_global: true,
                        });
                    }
                }
            }
            StmtKind::Nonlocal(names) => {
                let span = stmt.span;
                for n in names {
                    let scope = self.scope();
                    if scope.kind == ScopeKind::Module {
                        return Err(CompileError::spanned(
                            "nonlocal declaration not allowed at module level",
                            span,
                        ));
                    }
                    if scope.params.iter().any(|p| p == n) {
                        return Err(CompileError::spanned(
                            format!("name '{n}' is parameter and nonlocal"),
                            span,
                        ));
                    }
                    if let Some(d) = scope.directive_for(n) {
                        if d.is_global {
                            let at = d.span;
                            return Err(CompileError::spanned(
                                format!("name '{n}' is nonlocal and global"),
                                at,
                            ));
                        }
                    } else {
                        self.scope_mut().directives.push(Directive {
                            name: n.clone(),
                            span,
                            is_global: false,
                        });
                    }
                }
            }
            StmtKind::ImportFrom { names, .. } => {
                if names.iter().any(|a| a.name == "*")
                    && self.scope().kind != ScopeKind::Module
                {
                    // Anchor at the `*` itself — the last byte of the
                    // statement span.
                    let star = Span {
                        start: weavepy_lexer::BytePos(stmt.span.end.0.saturating_sub(1)),
                        end: stmt.span.end,
                    };
                    return Err(CompileError::spanned(
                        "import * only allowed at module level",
                        star,
                    ));
                }
            }
            StmtKind::AnnAssign {
                target,
                annotation,
                value,
            } => {
                match &target.kind {
                    ExprKind::Tuple(_) | ExprKind::List(_) => {
                        // Raised by CPython's pegen `invalid_ann_assign_target`.
                        return Err(CompileError::parser_spanned(
                            "only single target (not tuple) can be annotated",
                            target.span,
                        ));
                    }
                    ExprKind::Name(_) | ExprKind::Attribute { .. } | ExprKind::Subscript { .. } => {
                    }
                    _ => {
                        return Err(CompileError::spanned(
                            "illegal target for annotation",
                            target.span,
                        ));
                    }
                }
                self.visit_expr(annotation)?;
                if let Some(v) = value {
                    self.visit_expr(v)?;
                }
            }
            StmtKind::Try {
                body,
                handlers,
                orelse,
                finalbody,
            } => {
                self.validate_handlers(handlers)?;
                self.visit_body(body)?;
                for h in handlers {
                    if let Some(t) = &h.type_ {
                        self.visit_expr(t)?;
                    }
                    self.visit_body(&h.body)?;
                }
                self.visit_body(orelse)?;
                self.visit_body(finalbody)?;
            }
            StmtKind::Assign { targets, value } => {
                for t in targets {
                    self.visit_expr(t)?;
                }
                self.visit_expr(value)?;
            }
            StmtKind::AugAssign { target, value, .. } => {
                self.visit_expr(target)?;
                self.visit_expr(value)?;
            }
            StmtKind::Return(v) => {
                if let Some(v) = v {
                    self.visit_expr(v)?;
                }
            }
            StmtKind::Delete(targets) => {
                for t in targets {
                    self.visit_expr(t)?;
                }
            }
            StmtKind::If { test, body, orelse } => {
                self.visit_expr(test)?;
                self.visit_body(body)?;
                self.visit_body(orelse)?;
            }
            StmtKind::While { test, body, orelse } => {
                self.visit_expr(test)?;
                self.visit_body(body)?;
                self.visit_body(orelse)?;
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
                self.visit_expr(target)?;
                self.visit_expr(iter)?;
                self.visit_body(body)?;
                self.visit_body(orelse)?;
            }
            StmtKind::With { items, body } | StmtKind::AsyncWith { items, body } => {
                for it in items {
                    self.visit_expr(&it.context_expr)?;
                    if let Some(v) = &it.optional_vars {
                        self.visit_expr(v)?;
                    }
                }
                self.visit_body(body)?;
            }
            StmtKind::Raise { exc, cause } => {
                if let Some(e) = exc {
                    self.visit_expr(e)?;
                }
                if let Some(c) = cause {
                    self.visit_expr(c)?;
                }
            }
            StmtKind::Assert { test, msg } => {
                self.visit_expr(test)?;
                if let Some(m) = msg {
                    self.visit_expr(m)?;
                }
            }
            StmtKind::Match { subject, cases } => {
                self.visit_expr(subject)?;
                for case in cases {
                    self.visit_case(case)?;
                }
            }
            StmtKind::Expr(e) => self.visit_expr(e)?,
            _ => {}
        }
        // `from __future__ import …` feature names are checked wherever
        // the statement appears (CPython validates names even for
        // misplaced imports — placement was checked at module level).
        if let StmtKind::ImportFrom {
            module: Some(m),
            names,
            ..
        } = &stmt.kind
        {
            if m == "__future__" {
                for a in names {
                    if a.name == "braces" {
                        return Err(CompileError::spanned(
                            "not a chance",
                            self.alias_span(stmt, &a.name),
                        ));
                    }
                    if a.name != "*" && !KNOWN_FUTURES.contains(&a.name.as_str()) {
                        return Err(CompileError::spanned(
                            format!("future feature {} is not defined", a.name),
                            self.alias_span(stmt, &a.name),
                        ));
                    }
                }
            }
        }
        Ok(())
    }

    fn validate_handlers(&self, handlers: &[ExceptHandler]) -> Result<(), CompileError> {
        for (i, h) in handlers.iter().enumerate() {
            if h.type_.is_none() && !h.is_star && i + 1 < handlers.len() {
                return Err(CompileError::spanned(
                    "default 'except:' must be last",
                    h.span,
                ));
            }
        }
        Ok(())
    }

    fn visit_case(&mut self, case: &MatchCase) -> Result<(), CompileError> {
        self.visit_pattern(&case.pattern)?;
        if let Some(g) = &case.guard {
            self.visit_expr(g)?;
        }
        self.visit_body(&case.body)
    }

    fn visit_pattern(&mut self, pattern: &Pattern) -> Result<(), CompileError> {
        match pattern {
            Pattern::Value(e) => self.visit_expr(e)?,
            Pattern::Sequence(items) | Pattern::Or(items) => {
                for p in items {
                    self.visit_pattern(p)?;
                }
            }
            Pattern::Mapping { keys, patterns, .. } => {
                for k in keys {
                    self.visit_expr(k)?;
                }
                for p in patterns {
                    self.visit_pattern(p)?;
                }
            }
            Pattern::Class {
                cls,
                positionals,
                keywords,
            } => {
                self.visit_expr(cls)?;
                for p in positionals {
                    self.visit_pattern(p)?;
                }
                for (_, p) in keywords {
                    self.visit_pattern(p)?;
                }
            }
            Pattern::As { pattern, .. } => self.visit_pattern(pattern)?,
            _ => {}
        }
        Ok(())
    }

    fn visit_expr(&mut self, expr: &Expr) -> Result<(), CompileError> {
        match &expr.kind {
            ExprKind::Lambda { args, body } => {
                self.visit_function(args, &[], &[], true)?;
                // Lambda bodies are expressions; visit inside a
                // function scope for nested checks.
                let mut params: Vec<String> = Vec::new();
                for a in args
                    .posonlyargs
                    .iter()
                    .chain(&args.args)
                    .chain(&args.kwonlyargs)
                    .chain(&args.vararg)
                    .chain(&args.kwarg)
                {
                    params.push(a.name.clone());
                }
                self.scopes.push(Scope {
                    kind: ScopeKind::Function,
                    params,
                    directives: Vec::new(),
                });
                let result = self.visit_expr(body);
                self.scopes.pop();
                result?;
            }
            ExprKind::ListComp { elt, generators }
            | ExprKind::SetComp { elt, generators }
            | ExprKind::GeneratorExp { elt, generators } => {
                self.visit_comprehension(generators, &[elt])?;
            }
            ExprKind::DictComp {
                key,
                value,
                generators,
            } => {
                self.visit_comprehension(generators, &[key, value])?;
            }
            ExprKind::BoolOp { values, .. } => {
                for v in values {
                    self.visit_expr(v)?;
                }
            }
            ExprKind::BinOp { left, right, .. } => {
                self.visit_expr(left)?;
                self.visit_expr(right)?;
            }
            ExprKind::UnaryOp { operand, .. } => self.visit_expr(operand)?,
            ExprKind::Compare {
                left, comparators, ..
            } => {
                self.visit_expr(left)?;
                for c in comparators {
                    self.visit_expr(c)?;
                }
            }
            ExprKind::IfExp { test, body, orelse } => {
                self.visit_expr(test)?;
                self.visit_expr(body)?;
                self.visit_expr(orelse)?;
            }
            ExprKind::NamedExpr { target, value } => {
                self.visit_expr(target)?;
                self.visit_expr(value)?;
            }
            ExprKind::Call {
                func,
                args,
                keywords,
            } => {
                self.visit_expr(func)?;
                for a in args {
                    self.visit_expr(a)?;
                }
                for k in keywords {
                    self.visit_expr(&k.value)?;
                }
            }
            ExprKind::Attribute { value, .. } => self.visit_expr(value)?,
            ExprKind::Subscript { value, slice } => {
                self.visit_expr(value)?;
                self.visit_expr(slice)?;
            }
            ExprKind::Slice { lower, upper, step } => {
                for part in [lower, upper, step].into_iter().flatten() {
                    self.visit_expr(part)?;
                }
            }
            ExprKind::Tuple(items) | ExprKind::List(items) | ExprKind::Set(items) => {
                for i in items {
                    self.visit_expr(i)?;
                }
            }
            ExprKind::Dict { keys, values } => {
                for k in keys.iter().flatten() {
                    self.visit_expr(k)?;
                }
                for v in values {
                    self.visit_expr(v)?;
                }
            }
            ExprKind::Starred(inner)
            | ExprKind::Yield(Some(inner))
            | ExprKind::YieldFrom(inner)
            | ExprKind::Await(inner) => self.visit_expr(inner)?,
            ExprKind::JoinedStr(parts) => {
                for p in parts {
                    self.visit_expr(p)?;
                }
            }
            ExprKind::FormattedValue {
                value, format_spec, ..
            } => {
                self.visit_expr(value)?;
                if let Some(s) = format_spec {
                    self.visit_expr(s)?;
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// CPython's comprehension/walrus rebinding rules (`symtable.c`):
    /// processed in source order, a `for` target may not rebind a name
    /// already bound by a named expression in the same comprehension,
    /// and a named expression may not rebind an iteration variable.
    fn visit_comprehension(
        &mut self,
        generators: &[Comprehension],
        elements: &[&Expr],
    ) -> Result<(), CompileError> {
        let mut iter_vars: Vec<String> = Vec::new();
        let mut walrus_vars: Vec<String> = Vec::new();
        for g in generators {
            // Iteration target: reject names already bound by a walrus
            // earlier in this comprehension.
            let mut targets: Vec<(&str, Span)> = Vec::new();
            collect_name_targets(&g.target, &mut targets);
            for (name, span) in &targets {
                if walrus_vars.iter().any(|w| w == name) {
                    return Err(CompileError::spanned(
                        format!(
                            "comprehension inner loop cannot rebind assignment expression \
                             target '{name}'"
                        ),
                        *span,
                    ));
                }
                iter_vars.push((*name).to_owned());
            }
            self.visit_expr(&g.iter)?;
            self.check_walrus(&g.iter, &iter_vars, &mut walrus_vars)?;
            for cond in &g.ifs {
                self.visit_expr(cond)?;
                self.check_walrus(cond, &iter_vars, &mut walrus_vars)?;
            }
        }
        for e in elements {
            self.visit_expr(e)?;
            self.check_walrus(e, &iter_vars, &mut walrus_vars)?;
        }
        Ok(())
    }

    /// Record walrus targets in `expr` (without descending into nested
    /// comprehension/lambda scopes) and reject rebinds of comprehension
    /// iteration variables.
    fn check_walrus(
        &self,
        expr: &Expr,
        iter_vars: &[String],
        walrus_vars: &mut Vec<String>,
    ) -> Result<(), CompileError> {
        let mut found: Vec<(&str, Span)> = Vec::new();
        collect_walrus_targets(expr, &mut found);
        for (name, span) in found {
            if iter_vars.iter().any(|v| v == name) {
                return Err(CompileError::spanned(
                    format!(
                        "assignment expression cannot rebind comprehension iteration \
                         variable '{name}'"
                    ),
                    span,
                ));
            }
            walrus_vars.push(name.to_owned());
        }
        Ok(())
    }
}

impl Validator<'_> {
    /// Best-effort span of `from X import NAME`'s alias: find the name
    /// token textually inside the statement span. The AST doesn't carry
    /// alias positions, but the name always appears after the `import`
    /// keyword, so a substring search anchored past it is exact.
    fn alias_span(&self, stmt: &Stmt, name: &str) -> Span {
        let start = stmt.span.start.0 as usize;
        let end = (stmt.span.end.0 as usize).min(self.source.len());
        if start < end {
            let text = &self.source[start..end];
            if let Some(imp) = text.find("import") {
                let after = imp + "import".len();
                if let Some(rel) = text[after..].find(name) {
                    let abs = (start + after + rel) as u32;
                    return Span {
                        start: weavepy_lexer::BytePos(abs),
                        end: weavepy_lexer::BytePos(abs + name.len() as u32),
                    };
                }
            }
        }
        stmt.span
    }
}

fn collect_name_targets<'a>(expr: &'a Expr, out: &mut Vec<(&'a str, Span)>) {
    match &expr.kind {
        ExprKind::Name(n) => out.push((n, expr.span)),
        ExprKind::Tuple(items) | ExprKind::List(items) => {
            for i in items {
                collect_name_targets(i, out);
            }
        }
        ExprKind::Starred(inner) => collect_name_targets(inner, out),
        _ => {}
    }
}

/// Walrus targets within an expression, *not* descending into nested
/// comprehension or lambda scopes (those bind in their own scope).
fn collect_walrus_targets<'a>(expr: &'a Expr, out: &mut Vec<(&'a str, Span)>) {
    match &expr.kind {
        ExprKind::NamedExpr { target, value } => {
            if let ExprKind::Name(n) = &target.kind {
                out.push((n, target.span));
            }
            collect_walrus_targets(value, out);
        }
        ExprKind::ListComp { .. }
        | ExprKind::SetComp { .. }
        | ExprKind::DictComp { .. }
        | ExprKind::GeneratorExp { .. }
        | ExprKind::Lambda { .. } => {}
        ExprKind::BoolOp { values, .. } => {
            for v in values {
                collect_walrus_targets(v, out);
            }
        }
        ExprKind::BinOp { left, right, .. } => {
            collect_walrus_targets(left, out);
            collect_walrus_targets(right, out);
        }
        ExprKind::UnaryOp { operand, .. } => collect_walrus_targets(operand, out),
        ExprKind::Compare {
            left, comparators, ..
        } => {
            collect_walrus_targets(left, out);
            for c in comparators {
                collect_walrus_targets(c, out);
            }
        }
        ExprKind::IfExp { test, body, orelse } => {
            collect_walrus_targets(test, out);
            collect_walrus_targets(body, out);
            collect_walrus_targets(orelse, out);
        }
        ExprKind::Call {
            func,
            args,
            keywords,
        } => {
            collect_walrus_targets(func, out);
            for a in args {
                collect_walrus_targets(a, out);
            }
            for k in keywords {
                collect_walrus_targets(&k.value, out);
            }
        }
        ExprKind::Attribute { value, .. } => collect_walrus_targets(value, out),
        ExprKind::Subscript { value, slice } => {
            collect_walrus_targets(value, out);
            collect_walrus_targets(slice, out);
        }
        ExprKind::Slice { lower, upper, step } => {
            for part in [lower, upper, step].into_iter().flatten() {
                collect_walrus_targets(part, out);
            }
        }
        ExprKind::Tuple(items) | ExprKind::List(items) | ExprKind::Set(items) => {
            for i in items {
                collect_walrus_targets(i, out);
            }
        }
        ExprKind::Dict { keys, values } => {
            for k in keys.iter().flatten() {
                collect_walrus_targets(k, out);
            }
            for v in values {
                collect_walrus_targets(v, out);
            }
        }
        ExprKind::Starred(inner)
        | ExprKind::Yield(Some(inner))
        | ExprKind::YieldFrom(inner)
        | ExprKind::Await(inner) => collect_walrus_targets(inner, out),
        ExprKind::JoinedStr(parts) => {
            for p in parts {
                collect_walrus_targets(p, out);
            }
        }
        ExprKind::FormattedValue {
            value, format_spec, ..
        } => {
            collect_walrus_targets(value, out);
            if let Some(s) = format_spec {
                collect_walrus_targets(s, out);
            }
        }
        _ => {}
    }
}

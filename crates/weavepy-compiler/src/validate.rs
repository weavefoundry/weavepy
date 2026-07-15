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
    /// The implicit PEP 695 scope holding a generic `def`/`class`'s
    /// type parameters. `nonlocal` may never rebind a name bound here
    /// (CPython symtable: "nonlocal binding not allowed for type
    /// parameter").
    TypeParams,
    /// A comprehension's implicit function scope. Names read inside it
    /// don't count as uses of the enclosing scope (the outermost
    /// iterable is visited in the enclosing scope before this is
    /// pushed), and walrus targets bind through it.
    Comprehension,
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
    /// Names bound in this scope: parameters plus body assignments
    /// for functions; the parameter names themselves for
    /// [`ScopeKind::TypeParams`]. Consulted when resolving `nonlocal`
    /// declarations from nested scopes.
    bound: std::collections::HashSet<String>,
    /// Names *read* so far, in source order (CPython's `USE` flag) —
    /// a later `global`/`nonlocal` for one of these is "used prior to
    /// … declaration".
    used: std::collections::HashSet<String>,
    /// Names assigned/deleted/bound so far (`DEF_LOCAL`) — a later
    /// declaration is "assigned to before … declaration".
    assigned: std::collections::HashSet<String>,
    /// Names annotated so far (`DEF_ANNOT`) — can never be declared
    /// global/nonlocal in this scope, before *or* after.
    annotated: std::collections::HashSet<String>,
}

impl Scope {
    fn new(kind: ScopeKind) -> Scope {
        Scope {
            kind,
            params: Vec::new(),
            directives: Vec::new(),
            bound: std::collections::HashSet::new(),
            used: std::collections::HashSet::new(),
            assigned: std::collections::HashSet::new(),
            annotated: std::collections::HashSet::new(),
        }
    }

    fn directive_for(&self, name: &str) -> Option<&Directive> {
        self.directives.iter().find(|d| d.name == name)
    }
}

pub(crate) fn validate_module(
    module: &Module,
    source: &str,
    future_annotations: bool,
) -> Result<(), CompileError> {
    let mut v = Validator {
        source,
        scopes: vec![Scope::new(ScopeKind::Module)],
        future_annotations,
    };
    // `from __future__ import …` placement / feature validation
    // (CPython `future.c`). Only a docstring, comments, and other
    // future imports may precede one. Relative imports
    // (`from .__future__ import x`) are ordinary imports, not future
    // statements.
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
            StmtKind::ImportFrom {
                module: m,
                level: 0,
                ..
            } if m.as_deref() == Some("__future__") => {
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
    /// PEP 563 active (module has `from __future__ import annotations`
    /// or the caller passed `CO_FUTURE_ANNOTATIONS`): annotations are
    /// never evaluated, so their names don't participate in scope
    /// analysis, but yield/await/walrus inside them become errors.
    future_annotations: bool,
}

impl Validator<'_> {
    fn scope(&self) -> &Scope {
        self.scopes.last().expect("scope stack never empty")
    }

    fn scope_mut(&mut self) -> &mut Scope {
        self.scopes.last_mut().expect("scope stack never empty")
    }

    /// Record a *read* of `name` in the current scope (CPython `USE`).
    fn mark_use(&mut self, name: &str) {
        self.scope_mut().used.insert(name.to_owned());
    }

    /// Record a binding of `name` (CPython `DEF_LOCAL`). Comprehension
    /// scopes are transparent to bindings: a walrus inside one binds
    /// in the enclosing function/class/module scope.
    fn mark_assigned(&mut self, name: &str) {
        let idx = self
            .scopes
            .iter()
            .rposition(|s| s.kind != ScopeKind::Comprehension)
            .expect("scope stack always has a non-comprehension scope");
        self.scopes[idx].assigned.insert(name.to_owned());
    }

    /// Visit an annotation expression. Under PEP 563 the annotation is
    /// never evaluated: its names don't count as uses for scope
    /// analysis, but yield/await/named expressions inside it are
    /// compile-time errors (CPython symtable).
    fn visit_annotation(&mut self, ann: &Expr) -> Result<(), CompileError> {
        if self.future_annotations {
            check_annotation_expr(ann)
        } else {
            self.visit_expr(ann)
        }
    }

    /// Visit an assignment target: bare names (and names inside
    /// tuple/list/starred unpacking) are bindings, while
    /// attribute/subscript targets *read* their base expression
    /// (CPython marks `x` as `USE` in `x[0] = 1`).
    fn visit_target(&mut self, expr: &Expr) -> Result<(), CompileError> {
        match &expr.kind {
            ExprKind::Name(n) => {
                let n = n.clone();
                self.mark_assigned(&n);
            }
            ExprKind::Tuple(items) | ExprKind::List(items) => {
                for i in items {
                    self.visit_target(i)?;
                }
            }
            ExprKind::Starred(inner) => self.visit_target(inner)?,
            _ => self.visit_expr(expr)?,
        }
        Ok(())
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
        returns: Option<&Expr>,
    ) -> Result<(), CompileError> {
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
        if let Some(r) = returns {
            self.visit_annotation(r)?;
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
                self.visit_annotation(ann)?;
            }
            // CPython `forbidden_name`: `__debug__` is a compile-time
            // constant, so binding it as a parameter (`def f(__debug__)`,
            // `lambda __debug__: 0`, `*args`/`**kwargs` spelled
            // `__debug__`, keyword-only, …) is a SyntaxError, exactly like
            // assigning to it.
            if a.name == "__debug__" {
                return Err(CompileError::spanned(
                    "cannot assign to __debug__".to_owned(),
                    a.span,
                ));
            }
            if params.iter().any(|(n, _)| *n == a.name) {
                return Err(CompileError::spanned(
                    format!("duplicate argument '{}' in function definition", a.name),
                    a.span,
                ));
            }
            params.push((&a.name, a.span));
        }
        // Body-level bindings (assignments, defs, imports, walrus
        // targets) participate in `nonlocal` resolution from nested
        // scopes.
        let mut bound: std::collections::HashSet<String> =
            params.iter().map(|(n, _)| (*n).to_owned()).collect();
        {
            let mut globals = std::collections::HashSet::new();
            let mut nonlocals = std::collections::HashSet::new();
            let mut assigned = std::collections::HashSet::new();
            for s in body {
                crate::collect_decls(s, &mut globals, &mut nonlocals, &mut assigned);
                crate::collect_walrus_stmt(s, &mut assigned);
            }
            bound.extend(assigned);
        }
        self.scopes.push(Scope {
            params: params.iter().map(|(n, _)| (*n).to_owned()).collect(),
            bound,
            ..Scope::new(ScopeKind::Function)
        });
        let result = self.visit_body(body);
        self.scopes.pop();
        result
    }

    /// Push the implicit PEP 695 scope holding a generic statement's
    /// type parameters (the caller pops it). Returns whether a scope
    /// was pushed.
    fn push_type_params_scope(&mut self, type_params: &[weavepy_parser::ast::TypeParam]) -> bool {
        if type_params.is_empty() {
            return false;
        }
        self.scopes.push(Scope {
            bound: type_params.iter().map(|tp| tp.name.clone()).collect(),
            ..Scope::new(ScopeKind::TypeParams)
        });
        true
    }

    fn visit_stmt(&mut self, stmt: &Stmt) -> Result<(), CompileError> {
        match &stmt.kind {
            StmtKind::FunctionDef {
                name,
                args,
                body,
                decorator_list,
                type_params,
                returns,
            }
            | StmtKind::AsyncFunctionDef {
                name,
                args,
                body,
                decorator_list,
                type_params,
                returns,
            } => {
                // The def's name binds in the enclosing scope.
                let name = name.clone();
                self.mark_assigned(&name);
                let pushed = self.push_type_params_scope(type_params);
                let result = self.visit_function(args, body, decorator_list, returns.as_deref());
                if pushed {
                    self.scopes.pop();
                }
                result?;
            }
            StmtKind::ClassDef {
                name,
                body,
                decorator_list,
                bases,
                keywords,
                type_params,
                ..
            } => {
                let name = name.clone();
                self.mark_assigned(&name);
                for d in decorator_list {
                    self.visit_expr(d)?;
                }
                let pushed = self.push_type_params_scope(type_params);
                let result = (|| -> Result<(), CompileError> {
                    for b in bases {
                        self.visit_expr(b)?;
                    }
                    for k in keywords {
                        self.visit_expr(&k.value)?;
                    }
                    self.scopes.push(Scope::new(ScopeKind::Class));
                    let result = self.visit_body(body);
                    self.scopes.pop();
                    result
                })();
                if pushed {
                    self.scopes.pop();
                }
                result?;
            }
            StmtKind::Global(names) => {
                let span = stmt.span;
                for n in names {
                    let scope = self.scope();
                    // CPython symtable priority: PARAM, USE, ANNOT,
                    // ASSIGN — re-checked on every declaration, so a
                    // *duplicate* `global x` after an intervening
                    // use/assignment still errors.
                    if scope.params.iter().any(|p| p == n) {
                        return Err(CompileError::spanned(
                            format!("name '{n}' is parameter and global"),
                            span,
                        ));
                    }
                    if scope.used.contains(n) {
                        return Err(CompileError::spanned(
                            format!("name '{n}' is used prior to global declaration"),
                            span,
                        ));
                    }
                    if scope.annotated.contains(n) {
                        return Err(CompileError::spanned(
                            format!("annotated name '{n}' can't be global"),
                            span,
                        ));
                    }
                    if scope.assigned.contains(n) {
                        return Err(CompileError::spanned(
                            format!("name '{n}' is assigned to before global declaration"),
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
                    if scope.used.contains(n) {
                        return Err(CompileError::spanned(
                            format!("name '{n}' is used prior to nonlocal declaration"),
                            span,
                        ));
                    }
                    if scope.annotated.contains(n) {
                        return Err(CompileError::spanned(
                            format!("annotated name '{n}' can't be nonlocal"),
                            span,
                        ));
                    }
                    if scope.assigned.contains(n) {
                        return Err(CompileError::spanned(
                            format!("name '{n}' is assigned to before nonlocal declaration"),
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
                    // PEP 695: resolve outward (class scopes are
                    // transparent, as for any closure). If the nearest
                    // scope binding `n` is a type-parameter scope, the
                    // rebinding is rejected (CPython symtable).
                    for s in self.scopes[..self.scopes.len() - 1].iter().rev() {
                        match s.kind {
                            ScopeKind::Class | ScopeKind::Comprehension => {}
                            ScopeKind::TypeParams => {
                                if s.bound.contains(n) {
                                    return Err(CompileError::spanned(
                                        format!(
                                            "nonlocal binding not allowed for type parameter '{n}'"
                                        ),
                                        span,
                                    ));
                                }
                            }
                            ScopeKind::Function => {
                                if s.bound.contains(n) {
                                    break;
                                }
                            }
                            ScopeKind::Module => break,
                        }
                    }
                }
            }
            StmtKind::ImportFrom { names, .. }
                if names.iter().any(|a| a.name == "*")
                    && self.scope().kind != ScopeKind::Module =>
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
            StmtKind::AnnAssign {
                target,
                annotation,
                value,
                simple,
            } => {
                match &target.kind {
                    ExprKind::Tuple(_) | ExprKind::List(_) => {
                        // Raised by CPython's pegen `invalid_ann_assign_target`.
                        return Err(CompileError::parser_spanned(
                            "only single target (not tuple) can be annotated",
                            target.span,
                        ));
                    }
                    ExprKind::Name(n) if n == "__debug__" => {
                        return Err(CompileError::spanned(
                            "cannot assign to __debug__",
                            target.span,
                        ));
                    }
                    ExprKind::Attribute { attr, .. } if attr == "__debug__" => {
                        return Err(CompileError::spanned(
                            "cannot assign to __debug__",
                            target.span,
                        ));
                    }
                    ExprKind::Name(n) => {
                        // Simple (unparenthesized) targets are
                        // annotations (`DEF_ANNOT`): incompatible with
                        // a global/nonlocal directive in either order.
                        // Parenthesized ones only bind (`DEF_LOCAL`).
                        if *simple {
                            // CPython skips this check at module scope
                            // (`ste_symbols == st_global`): `global x` +
                            // `x: int` at top level is valid.
                            if self.scope().kind != ScopeKind::Module {
                                if let Some(d) = self.scope().directive_for(n) {
                                    let what = if d.is_global { "global" } else { "nonlocal" };
                                    return Err(CompileError::spanned(
                                        format!("annotated name '{n}' can't be {what}"),
                                        stmt.span,
                                    ));
                                }
                            }
                            let n = n.clone();
                            self.scope_mut().annotated.insert(n.clone());
                            self.mark_assigned(&n);
                        } else {
                            let n = n.clone();
                            self.mark_assigned(&n);
                        }
                    }
                    ExprKind::Attribute { .. } | ExprKind::Subscript { .. } => {
                        self.visit_expr(target)?;
                    }
                    _ => {
                        return Err(CompileError::spanned(
                            "illegal target for annotation",
                            target.span,
                        ));
                    }
                }
                self.visit_annotation(annotation)?;
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
                    if let Some(n) = &h.name {
                        let n = n.clone();
                        self.mark_assigned(&n);
                    }
                    self.visit_body(&h.body)?;
                }
                self.visit_body(orelse)?;
                self.visit_body(finalbody)?;
            }
            StmtKind::Assign { targets, value } => {
                for t in targets {
                    self.visit_target(t)?;
                }
                self.visit_expr(value)?;
            }
            StmtKind::AugAssign { target, value, .. } => {
                self.visit_target(target)?;
                self.visit_expr(value)?;
            }
            StmtKind::Return(v) => {
                if let Some(v) = v {
                    self.visit_expr(v)?;
                }
            }
            StmtKind::Delete(targets) => {
                // `del x` binds (CPython `DEF_LOCAL`), same as assignment.
                for t in targets {
                    self.visit_target(t)?;
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
                self.visit_target(target)?;
                self.visit_expr(iter)?;
                self.visit_body(body)?;
                self.visit_body(orelse)?;
            }
            StmtKind::With { items, body } | StmtKind::AsyncWith { items, body } => {
                for it in items {
                    self.visit_expr(&it.context_expr)?;
                    if let Some(v) = &it.optional_vars {
                        self.visit_target(v)?;
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
            level: 0,
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
                    if !KNOWN_FUTURES.contains(&a.name.as_str()) {
                        // `from __future__ import *` gets the same
                        // "not defined" diagnostic (CPython future.c).
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
            Pattern::Capture(Some(n)) | Pattern::Star(Some(n)) => {
                let n = n.clone();
                self.mark_assigned(&n);
            }
            Pattern::Sequence(items) | Pattern::Or(items) => {
                for p in items {
                    self.visit_pattern(p)?;
                }
            }
            Pattern::Mapping {
                keys,
                patterns,
                rest,
            } => {
                for k in keys {
                    self.visit_expr(k)?;
                }
                for p in patterns {
                    self.visit_pattern(p)?;
                }
                if let Some(Some(n)) = rest {
                    let n = n.clone();
                    self.mark_assigned(&n);
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
            Pattern::As { pattern, name } => {
                self.visit_pattern(pattern)?;
                let name = name.clone();
                self.mark_assigned(&name);
            }
            _ => {}
        }
        Ok(())
    }

    fn visit_expr(&mut self, expr: &Expr) -> Result<(), CompileError> {
        match &expr.kind {
            ExprKind::Name(n) => {
                let n = n.clone();
                self.mark_use(&n);
            }
            ExprKind::Lambda { args, body } => {
                self.visit_function(args, &[], &[], None)?;
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
                    params: params.clone(),
                    bound: params.into_iter().collect(),
                    ..Scope::new(ScopeKind::Function)
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
                if let ExprKind::Name(n) = &target.kind {
                    let n = n.clone();
                    self.mark_assigned(&n);
                } else {
                    self.visit_expr(target)?;
                }
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
        // The outermost iterable evaluates in the enclosing scope;
        // everything else lives in the comprehension's implicit
        // function scope, so reads there don't count as uses of the
        // enclosing scope (`[x for y in q]` then `global x` is fine,
        // `[1 for y in x]` then `global x` is not).
        if let Some(first) = generators.first() {
            self.visit_expr(&first.iter)?;
        }
        self.scopes.push(Scope::new(ScopeKind::Comprehension));
        let result = self.visit_comprehension_inner(generators, elements);
        self.scopes.pop();
        result
    }

    fn visit_comprehension_inner(
        &mut self,
        generators: &[Comprehension],
        elements: &[&Expr],
    ) -> Result<(), CompileError> {
        let mut iter_vars: Vec<String> = Vec::new();
        let mut walrus_vars: Vec<String> = Vec::new();
        for (gi, g) in generators.iter().enumerate() {
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
                // Iteration variables bind in the comprehension scope
                // itself, not the enclosing one.
                let s = self.scope_mut();
                s.assigned.insert((*name).to_owned());
                s.bound.insert((*name).to_owned());
            }
            if gi > 0 {
                self.visit_expr(&g.iter)?;
            }
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
        &mut self,
        expr: &Expr,
        iter_vars: &[String],
        walrus_vars: &mut Vec<String>,
    ) -> Result<(), CompileError> {
        let mut found: Vec<(String, Span)> = Vec::new();
        {
            let mut borrowed: Vec<(&str, Span)> = Vec::new();
            collect_walrus_targets(expr, &mut borrowed);
            found.extend(borrowed.into_iter().map(|(n, s)| (n.to_owned(), s)));
        }
        for (name, span) in found {
            if iter_vars.iter().any(|v| v == &name) {
                return Err(CompileError::spanned(
                    format!(
                        "assignment expression cannot rebind comprehension iteration \
                         variable '{name}'"
                    ),
                    span,
                ));
            }
            // Walrus targets bind through the comprehension scope into
            // the enclosing function/class/module scope.
            self.mark_assigned(&name);
            walrus_vars.push(name);
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

/// PEP 563: yield / await / named expressions may not appear anywhere
/// inside an annotation once `from __future__ import annotations` is
/// active (CPython symtable's `_check_no_deferred_annotation` rules).
/// Lambdas open a new scope, so their bodies are exempt.
fn check_annotation_expr(expr: &Expr) -> Result<(), CompileError> {
    match &expr.kind {
        ExprKind::Yield(_) | ExprKind::YieldFrom(_) => {
            return Err(CompileError::spanned(
                "yield expression cannot be used within an annotation",
                expr.span,
            ));
        }
        ExprKind::Await(_) => {
            return Err(CompileError::spanned(
                "await expression cannot be used within an annotation",
                expr.span,
            ));
        }
        ExprKind::NamedExpr { target, .. } => {
            return Err(CompileError::spanned(
                "named expression cannot be used within an annotation",
                target.span,
            ));
        }
        ExprKind::Lambda { args, .. } => {
            // The lambda body is a new scope, but its defaults belong
            // to the annotation's own scope.
            for d in args
                .defaults
                .iter()
                .chain(args.kw_defaults.iter().flatten())
            {
                check_annotation_expr(d)?;
            }
            return Ok(());
        }
        _ => {}
    }
    let mut result = Ok(());
    for_each_child_expr(expr, &mut |child| {
        if result.is_ok() {
            result = check_annotation_expr(child);
        }
    });
    result
}

/// Call `f` on every direct child expression of `expr`.
fn for_each_child_expr<'a>(expr: &'a Expr, f: &mut dyn FnMut(&'a Expr)) {
    match &expr.kind {
        ExprKind::BoolOp { values, .. } => values.iter().for_each(f),
        ExprKind::BinOp { left, right, .. } => {
            f(left);
            f(right);
        }
        ExprKind::UnaryOp { operand, .. } => f(operand),
        ExprKind::Lambda { args, body } => {
            for d in &args.defaults {
                f(d);
            }
            for d in args.kw_defaults.iter().flatten() {
                f(d);
            }
            f(body);
        }
        ExprKind::IfExp { test, body, orelse } => {
            f(test);
            f(body);
            f(orelse);
        }
        ExprKind::Dict { keys, values } => {
            keys.iter().flatten().for_each(&mut *f);
            values.iter().for_each(f);
        }
        ExprKind::ListComp { elt, generators }
        | ExprKind::SetComp { elt, generators }
        | ExprKind::GeneratorExp { elt, generators } => {
            f(elt);
            for g in generators {
                f(&g.target);
                f(&g.iter);
                g.ifs.iter().for_each(&mut *f);
            }
        }
        ExprKind::DictComp {
            key,
            value,
            generators,
        } => {
            f(key);
            f(value);
            for g in generators {
                f(&g.target);
                f(&g.iter);
                g.ifs.iter().for_each(&mut *f);
            }
        }
        ExprKind::Compare {
            left, comparators, ..
        } => {
            f(left);
            comparators.iter().for_each(f);
        }
        ExprKind::Call {
            func,
            args,
            keywords,
        } => {
            f(func);
            args.iter().for_each(&mut *f);
            for k in keywords {
                f(&k.value);
            }
        }
        ExprKind::NamedExpr { target, value } => {
            f(target);
            f(value);
        }
        ExprKind::Attribute { value, .. } => f(value),
        ExprKind::Subscript { value, slice } => {
            f(value);
            f(slice);
        }
        ExprKind::Slice { lower, upper, step } => {
            [lower, upper, step]
                .into_iter()
                .flatten()
                .for_each(|e| f(e));
        }
        ExprKind::Tuple(items) | ExprKind::List(items) | ExprKind::Set(items) => {
            items.iter().for_each(f);
        }
        ExprKind::Starred(inner)
        | ExprKind::Yield(Some(inner))
        | ExprKind::YieldFrom(inner)
        | ExprKind::Await(inner) => f(inner),
        ExprKind::JoinedStr(parts) => parts.iter().for_each(f),
        ExprKind::FormattedValue {
            value, format_spec, ..
        } => {
            f(value);
            if let Some(s) = format_spec {
                f(s);
            }
        }
        _ => {}
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

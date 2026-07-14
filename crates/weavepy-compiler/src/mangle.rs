//! Private name mangling (CPython `_Py_Mangle`).
//!
//! Inside a class scope, every identifier of the form `__spam` (at
//! least two leading underscores, at most one trailing underscore) is
//! textually replaced with `_ClassName__spam`, where `ClassName` is
//! the current class name with leading underscores stripped. CPython
//! performs this at symtable/compile time for *all* identifiers in the
//! class body — bindings, attribute names, keyword arguments,
//! `global`/`nonlocal` declarations, import bindings, and pattern
//! captures — including those nested inside method bodies.
//!
//! The pass rewrites a clone of the class-body AST before compilation.
//! It recurses through functions/lambdas/comprehensions (which are in
//! the class's textual scope) but *not* into nested class bodies: those
//! are mangled against their own (innermost) class name when their own
//! body is compiled.

use std::collections::HashSet;

use weavepy_parser::ast::{
    Arguments, Comprehension, ExceptHandler, Expr, ExprKind, MatchCase, Pattern, Stmt, StmtKind,
    TypeParamKind,
};

/// Recover the source spelling of a binding that was mangled against
/// `class_name` (used for `__name__` / `__qualname__`, which CPython
/// keeps unmangled). Inverse of the [`Mangler`] name rewrite.
pub(crate) fn demangle_name<'a>(class_name: &str, ident: &'a str) -> &'a str {
    let stripped = class_name.trim_start_matches('_');
    if stripped.is_empty() {
        return ident;
    }
    match ident.strip_prefix(&format!("_{stripped}")) {
        Some(rest) if rest.starts_with("__") && !rest.ends_with("__") => rest,
        _ => ident,
    }
}

/// Apply private-name mangling for class `class_name` to `body`.
pub(crate) fn mangle_class_body(class_name: &str, body: &mut [Stmt]) {
    let stripped = class_name.trim_start_matches('_');
    if stripped.is_empty() {
        // A class named entirely of underscores never mangles.
        return;
    }
    let m = Mangler {
        prefix: format!("_{stripped}"),
        skip: HashSet::new(),
        only: None,
    };
    for s in body {
        m.stmt(s);
    }
}

/// Mangle a single identifier against `class_name` (`_Py_Mangle` for
/// one name). Used by the PEP 695 hidden-scope lowering: a *class's*
/// type parameters bind mangled against the class's own name
/// (`class Foo[__T]` binds `_Foo__T`) even outside any class body.
pub(crate) fn mangle_ident(class_name: &str, ident: &str) -> String {
    let stripped = class_name.trim_start_matches('_');
    if stripped.is_empty()
        || !ident.starts_with("__")
        || ident.ends_with("__")
        || ident.contains('.')
    {
        return ident.to_owned();
    }
    format!("_{stripped}{ident}")
}

/// Rewrite only the identifiers in `names` within `expr`, mangling
/// them against `class_name`. The PEP 695 hidden-scope lowering uses
/// this on a generic class's header expressions (bases, keywords, and
/// type-parameter bounds/defaults evaluate inside the hidden scope,
/// where references to the class's own — mangled — type parameters
/// must follow the binding rename).
pub(crate) fn mangle_only_in_expr(class_name: &str, names: &HashSet<String>, expr: &mut Expr) {
    let stripped = class_name.trim_start_matches('_');
    if stripped.is_empty() {
        return;
    }
    let m = Mangler {
        prefix: format!("_{stripped}"),
        skip: HashSet::new(),
        only: Some(names.clone()),
    };
    m.expr(expr);
}

struct Mangler {
    /// `_ClassName` (leading underscores of the class name stripped).
    prefix: String,
    /// Names exempt from this pass. Used for a nested generic class's
    /// own PEP 695 type parameters: those mangle against the *nested*
    /// class's name (CPython mangles `class Foo[__T]`'s binding to
    /// `_Foo__T` even at module level), which the compiler's hidden-
    /// scope lowering applies when the nested class itself compiles.
    skip: HashSet<String>,
    /// When set, mangle *only* these identifiers (the inverse of
    /// `skip`) — the [`mangle_only_in_expr`] targeted-rename mode.
    only: Option<HashSet<String>>,
}

impl Mangler {
    /// `_Py_Mangle`: rewrite `__spam` (but not `__spam__` or dotted
    /// names) to `_ClassName__spam`.
    fn name(&self, ident: &mut String) {
        if !ident.starts_with("__") {
            return;
        }
        if ident.ends_with("__") || ident.contains('.') {
            return;
        }
        if self.skip.contains(ident.as_str()) {
            return;
        }
        if let Some(only) = &self.only {
            if !only.contains(ident.as_str()) {
                return;
            }
        }
        *ident = format!("{}{}", self.prefix, ident);
    }

    fn opt_name(&self, ident: &mut Option<String>) {
        if let Some(n) = ident {
            self.name(n);
        }
    }

    fn stmt(&self, s: &mut Stmt) {
        match &mut s.kind {
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
                // The *binding* is mangled; the compiler demangles for
                // `__name__`/`__qualname__` (CPython keeps those
                // unmangled — see `demangle_name`).
                self.name(name);
                // A *function's* PEP 695 type parameters mangle against
                // the enclosing class (CPython binds `def meth[__U]`
                // inside `Outer` as `_Outer__U`; the TypeVar's
                // `__name__` is demangled back at lowering time).
                for tp in type_params {
                    self.name(&mut tp.name);
                    if let TypeParamKind::TypeVar { bound: Some(b) } = &mut tp.kind {
                        self.expr(b);
                    }
                    if let Some(d) = &mut tp.default {
                        self.expr(d);
                    }
                }
                self.arguments(args);
                for d in decorator_list {
                    self.expr(d);
                }
                if let Some(r) = returns {
                    self.expr(r);
                }
                for st in body {
                    self.stmt(st);
                }
            }
            StmtKind::ClassDef {
                name,
                bases,
                keywords,
                decorator_list,
                type_params,
                ..
            } => {
                // The binding is mangled (demangled again for display
                // names). The nested *body* is mangled against the inner
                // class's own name when its `build_class_body` runs —
                // don't descend here.
                self.name(name);
                for d in decorator_list {
                    self.expr(d);
                }
                // A *class's* type parameters mangle against the class
                // itself (CPython: `class Foo[__T]` binds `_Foo__T`
                // even at module level) — applied by the compiler's
                // hidden-scope lowering, so this pass must leave the
                // parameter names (and references to them in the
                // header, which evaluates inside the hidden scope)
                // untouched.
                let own: HashSet<String> = type_params.iter().map(|tp| tp.name.clone()).collect();
                let sub = Mangler {
                    prefix: self.prefix.clone(),
                    skip: self.skip.union(&own).cloned().collect(),
                    only: self.only.clone(),
                };
                for tp in type_params {
                    if let TypeParamKind::TypeVar { bound: Some(b) } = &mut tp.kind {
                        sub.expr(b);
                    }
                    if let Some(d) = &mut tp.default {
                        sub.expr(d);
                    }
                }
                for b in bases {
                    sub.expr(b);
                }
                for k in keywords {
                    sub.expr(&mut k.value);
                }
            }
            StmtKind::TypeAlias {
                name,
                type_params,
                value,
                ..
            } => {
                // Normally dead — the compiler lowers `type` statements
                // to their assignment form before mangling — but kept
                // faithful for safety: the binding and the type-parameter
                // names mangle; bounds/defaults/value are ordinary
                // expressions (matching what the lowered form produces).
                self.name(name);
                for tp in type_params {
                    self.name(&mut tp.name);
                    if let TypeParamKind::TypeVar { bound: Some(b) } = &mut tp.kind {
                        self.expr(b);
                    }
                    if let Some(d) = &mut tp.default {
                        self.expr(d);
                    }
                }
                self.expr(value);
            }
            StmtKind::Return(v) => {
                if let Some(v) = v {
                    self.expr(v);
                }
            }
            StmtKind::Assign { targets, value } => {
                for t in targets {
                    self.expr(t);
                }
                self.expr(value);
            }
            StmtKind::AugAssign { target, value, .. } => {
                self.expr(target);
                self.expr(value);
            }
            StmtKind::AnnAssign {
                target,
                annotation,
                value,
                ..
            } => {
                self.expr(target);
                self.expr(annotation);
                if let Some(v) = value {
                    self.expr(v);
                }
            }
            StmtKind::If { test, body, orelse } | StmtKind::While { test, body, orelse } => {
                self.expr(test);
                for st in body {
                    self.stmt(st);
                }
                for st in orelse {
                    self.stmt(st);
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
                self.expr(target);
                self.expr(iter);
                for st in body {
                    self.stmt(st);
                }
                for st in orelse {
                    self.stmt(st);
                }
            }
            StmtKind::Try {
                body,
                handlers,
                orelse,
                finalbody,
            } => {
                for st in body {
                    self.stmt(st);
                }
                for h in handlers {
                    self.handler(h);
                }
                for st in orelse {
                    self.stmt(st);
                }
                for st in finalbody {
                    self.stmt(st);
                }
            }
            StmtKind::Raise { exc, cause } => {
                if let Some(e) = exc {
                    self.expr(e);
                }
                if let Some(c) = cause {
                    self.expr(c);
                }
            }
            StmtKind::With { items, body } | StmtKind::AsyncWith { items, body } => {
                for it in items {
                    self.expr(&mut it.context_expr);
                    if let Some(v) = &mut it.optional_vars {
                        self.expr(v);
                    }
                }
                for st in body {
                    self.stmt(st);
                }
            }
            StmtKind::Import(aliases) | StmtKind::ImportFrom { names: aliases, .. } => {
                // The *binding* name is mangled (`import x as __y` binds
                // `_C__y`); the module path itself is untouched.
                for a in aliases {
                    match &mut a.asname {
                        Some(n) => self.name(n),
                        None => {
                            // `import __x` binds `__x`; `from m import __x`
                            // likewise. Mangle only non-dotted bindings.
                            if !a.name.contains('.') {
                                let mut n = a.name.clone();
                                self.name(&mut n);
                                if n != a.name {
                                    a.asname = Some(n);
                                }
                            }
                        }
                    }
                }
            }
            StmtKind::Global(names) | StmtKind::Nonlocal(names) => {
                for n in names {
                    self.name(n);
                }
            }
            StmtKind::Match { subject, cases } => {
                self.expr(subject);
                for c in cases {
                    self.case(c);
                }
            }
            StmtKind::Expr(e) => self.expr(e),
            StmtKind::Delete(targets) => {
                for t in targets {
                    self.expr(t);
                }
            }
            StmtKind::Assert { test, msg } => {
                self.expr(test);
                if let Some(m) = msg {
                    self.expr(m);
                }
            }
            StmtKind::Pass | StmtKind::Break | StmtKind::Continue => {}
        }
    }

    fn handler(&self, h: &mut ExceptHandler) {
        if let Some(t) = &mut h.type_ {
            self.expr(t);
        }
        self.opt_name(&mut h.name);
        for st in &mut h.body {
            self.stmt(st);
        }
    }

    fn case(&self, c: &mut MatchCase) {
        self.pattern(&mut c.pattern);
        if let Some(g) = &mut c.guard {
            self.expr(g);
        }
        for st in &mut c.body {
            self.stmt(st);
        }
    }

    fn pattern(&self, p: &mut Pattern) {
        match p {
            Pattern::Value(e) => self.expr(e),
            Pattern::Singleton(_) => {}
            Pattern::Capture(n) => self.opt_name(n),
            Pattern::Sequence(items) | Pattern::Or(items) => {
                for x in items {
                    self.pattern(x);
                }
            }
            Pattern::Star(n) => self.opt_name(n),
            Pattern::Mapping {
                keys,
                patterns,
                rest,
            } => {
                for k in keys {
                    self.expr(k);
                }
                for x in patterns {
                    self.pattern(x);
                }
                if let Some(r) = rest {
                    self.opt_name(r);
                }
            }
            Pattern::Class {
                cls,
                positionals,
                keywords,
            } => {
                self.expr(cls);
                for x in positionals {
                    self.pattern(x);
                }
                for (n, x) in keywords {
                    self.name(n);
                    self.pattern(x);
                }
            }
            Pattern::As { pattern, name } => {
                self.pattern(pattern);
                self.name(name);
            }
        }
    }

    fn arguments(&self, a: &mut Arguments) {
        for x in a
            .posonlyargs
            .iter_mut()
            .chain(a.args.iter_mut())
            .chain(a.kwonlyargs.iter_mut())
            .chain(a.vararg.iter_mut())
            .chain(a.kwarg.iter_mut())
        {
            self.name(&mut x.name);
            if let Some(ann) = &mut x.annotation {
                self.expr(ann);
            }
        }
        for d in &mut a.defaults {
            self.expr(d);
        }
        for d in a.kw_defaults.iter_mut().flatten() {
            self.expr(d);
        }
    }

    fn comprehension(&self, c: &mut Comprehension) {
        self.expr(&mut c.target);
        self.expr(&mut c.iter);
        for i in &mut c.ifs {
            self.expr(i);
        }
    }

    fn expr(&self, e: &mut Expr) {
        match &mut e.kind {
            ExprKind::Constant(_) => {}
            ExprKind::Name(n) => self.name(n),
            ExprKind::Attribute { value, attr } => {
                self.expr(value);
                self.name(attr);
            }
            ExprKind::Subscript { value, slice } => {
                self.expr(value);
                self.expr(slice);
            }
            ExprKind::Slice { lower, upper, step } => {
                for part in [lower, upper, step].into_iter().flatten() {
                    self.expr(part);
                }
            }
            ExprKind::BinOp { left, right, .. } => {
                self.expr(left);
                self.expr(right);
            }
            ExprKind::BoolOp { values, .. } => {
                for v in values {
                    self.expr(v);
                }
            }
            ExprKind::UnaryOp { operand, .. } => self.expr(operand),
            ExprKind::Compare {
                left, comparators, ..
            } => {
                self.expr(left);
                for c in comparators {
                    self.expr(c);
                }
            }
            ExprKind::IfExp { test, body, orelse } => {
                self.expr(test);
                self.expr(body);
                self.expr(orelse);
            }
            ExprKind::NamedExpr { target, value } => {
                self.expr(target);
                self.expr(value);
            }
            ExprKind::Lambda { args, body } | ExprKind::TypeParamFn { args, body } => {
                self.arguments(args);
                self.expr(body);
            }
            ExprKind::Call {
                func,
                args,
                keywords,
            } => {
                self.expr(func);
                for a in args {
                    self.expr(a);
                }
                // Keyword argument *names* are not mangled (CPython quirk:
                // `dict(__k=1)` in a class body keeps the literal key).
                for k in keywords {
                    self.expr(&mut k.value);
                }
            }
            ExprKind::Tuple(items) | ExprKind::List(items) | ExprKind::Set(items) => {
                for x in items {
                    self.expr(x);
                }
            }
            ExprKind::Dict { keys, values } => {
                for k in keys.iter_mut().flatten() {
                    self.expr(k);
                }
                for v in values {
                    self.expr(v);
                }
            }
            ExprKind::ListComp { elt, generators }
            | ExprKind::SetComp { elt, generators }
            | ExprKind::GeneratorExp { elt, generators } => {
                self.expr(elt);
                for g in generators {
                    self.comprehension(g);
                }
            }
            ExprKind::DictComp {
                key,
                value,
                generators,
            } => {
                self.expr(key);
                self.expr(value);
                for g in generators {
                    self.comprehension(g);
                }
            }
            ExprKind::Starred(inner)
            | ExprKind::Yield(Some(inner))
            | ExprKind::YieldFrom(inner)
            | ExprKind::Await(inner) => self.expr(inner),
            ExprKind::Yield(None) => {}
            ExprKind::JoinedStr(parts) => {
                for p in parts {
                    self.expr(p);
                }
            }
            ExprKind::FormattedValue {
                value, format_spec, ..
            } => {
                self.expr(value);
                if let Some(s) = format_spec {
                    self.expr(s);
                }
            }
        }
    }
}

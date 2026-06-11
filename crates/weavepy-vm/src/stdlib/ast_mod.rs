//! `_ast` — the thin native core behind the frozen `ast` module (RFC 0033).
//!
//! CPython's `_ast` is the C extension that *defines* the AST node
//! classes; WeavePy instead defines the node classes in pure Python
//! (`stdlib/python/ast.py`) and uses this module for the one thing that
//! genuinely needs the engine: turning source text into a tree.
//!
//! [`parse`] runs WeavePy's real lexer + parser and walks the resulting
//! [`weavepy_parser::ast`] tree into a *spec* tree built from ordinary
//! Python values:
//!
//! - every node becomes a `dict` whose `"_type"` key names the CPython
//!   node class (`"BinOp"`, `"Name"`, …) and whose remaining keys are the
//!   node's CPython `_fields`, plus the four location attributes
//!   (`lineno`, `col_offset`, `end_lineno`, `end_col_offset`),
//! - lists become Python `list`s, optionals become the value or `None`,
//!   identifiers become `str`, and literal values become their runtime
//!   objects (`int`, `str`, `bytes`, `float`, `complex`, `bool`, `None`).
//!
//! `ast.py` then rebuilds real node instances from these dicts. Keeping
//! the bridge value-based (rather than re-`eval`-ing a dumped string)
//! makes arbitrary string/bytes literals and source locations round-trip
//! losslessly.

use crate::sync::Rc;
use crate::sync::RefCell;

use weavepy_lexer::token::Span;
use weavepy_parser::ast as past;

use crate::error::{value_error, RuntimeError};
use crate::import::ModuleCache;
use crate::object::{BuiltinFn, DictData, DictKey, Object, PyModule};

pub fn build(_cache: &ModuleCache) -> Rc<PyModule> {
    let dict = Rc::new(RefCell::new(DictData::new()));
    {
        let mut d = dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("__name__")),
            Object::from_static("_ast"),
        );
        d.insert(
            DictKey(Object::from_static("__doc__")),
            Object::from_static("WeavePy native AST parsing core (RFC 0033)."),
        );
        let bf = BuiltinFn {
            name: "parse",
            call: Box::new(parse),
            call_kw: None,
        };
        d.insert(
            DictKey(Object::from_static("parse")),
            Object::Builtin(Rc::new(bf)),
        );
    }
    Rc::new(PyModule {
        name: "_ast".to_owned(),
        filename: None,
        dict,
    })
}

/// `_ast.parse(source, filename='<unknown>', mode='exec')` → spec tree.
pub fn parse(args: &[Object]) -> Result<Object, RuntimeError> {
    let source = match args.first() {
        Some(Object::Str(s)) => s.to_string(),
        Some(Object::Bytes(b)) => String::from_utf8_lossy(b).into_owned(),
        _ => return Err(value_error("ast.parse() requires a str or bytes source")),
    };
    let filename = match args.get(1) {
        Some(Object::Str(s)) => s.to_string(),
        _ => "<unknown>".to_owned(),
    };
    let mode = match args.get(2) {
        Some(Object::Str(s)) => s.to_string(),
        _ => "exec".to_owned(),
    };
    // CPython raises `SyntaxError` (never `ValueError`) from
    // `ast.parse` — callers like `traceback`'s caret-anchor probe rely
    // on `except SyntaxError` swallowing bad segments.
    let module = weavepy_parser::parse_module(&source)
        .map_err(|e| crate::parse_error_to_syntax_error(&e, &source, &filename))?;
    let lm = LineMap::new(&source);
    let b = Builder { lm: &lm };
    Ok(b.module(&module, &mode))
}

/// Byte-offset → (1-based line, 0-based UTF-8 column) resolver.
struct LineMap {
    /// Byte offset of each `'\n'` in the source.
    newlines: Vec<usize>,
}

impl LineMap {
    fn new(source: &str) -> Self {
        let newlines = source
            .bytes()
            .enumerate()
            .filter_map(|(i, b)| (b == b'\n').then_some(i))
            .collect();
        Self { newlines }
    }

    /// Resolve a byte position into a `(lineno, col_offset)` pair.
    fn pos(&self, byte: u32) -> (i64, i64) {
        let byte = byte as usize;
        // Number of newlines strictly before `byte` == 0-based line index.
        let line_idx = self.newlines.partition_point(|&nl| nl < byte);
        let line_start = if line_idx == 0 {
            0
        } else {
            self.newlines[line_idx - 1] + 1
        };
        (
            (line_idx as i64) + 1,
            (byte.saturating_sub(line_start)) as i64,
        )
    }
}

/// Walks a parsed module into the value-based spec tree.
struct Builder<'a> {
    lm: &'a LineMap,
}

/// Build a node `dict` with `_type`, the given fields, and the four
/// location attributes derived from `span`.
fn node(ty: &str, fields: Vec<(&str, Object)>, span: Span, lm: &LineMap) -> Object {
    let mut d = DictData::new();
    d.insert(DictKey(Object::from_static("_type")), Object::from_str(ty));
    for (k, v) in fields {
        d.insert(DictKey(Object::from_str(k)), v);
    }
    let (lineno, col) = lm.pos(span.start.0);
    let (end_lineno, end_col) = lm.pos(span.end.0);
    d.insert(DictKey(Object::from_static("lineno")), Object::Int(lineno));
    d.insert(DictKey(Object::from_static("col_offset")), Object::Int(col));
    d.insert(
        DictKey(Object::from_static("end_lineno")),
        Object::Int(end_lineno),
    );
    d.insert(
        DictKey(Object::from_static("end_col_offset")),
        Object::Int(end_col),
    );
    Object::Dict(Rc::new(RefCell::new(d)))
}

/// Build a node `dict` with no location attributes (used for the handful
/// of CPython nodes that carry no positions: `arguments`, `comprehension`,
/// `keyword`*, `alias`*, `withitem`, `match_case`). (* some do carry
/// positions in 3.13; WeavePy lacks spans for them, so we omit.)
fn node_noloc(ty: &str, fields: Vec<(&str, Object)>) -> Object {
    let mut d = DictData::new();
    d.insert(DictKey(Object::from_static("_type")), Object::from_str(ty));
    for (k, v) in fields {
        d.insert(DictKey(Object::from_str(k)), v);
    }
    Object::Dict(Rc::new(RefCell::new(d)))
}

/// A bare singleton node (operators / contexts): `Add()`, `Load()`, …
fn singleton(ty: &str) -> Object {
    node_noloc(ty, vec![])
}

fn ident(s: &str) -> Object {
    Object::from_str(s)
}

fn opt_ident(s: Option<&str>) -> Object {
    match s {
        Some(v) => Object::from_str(v),
        None => Object::None,
    }
}

fn list_of<T>(items: &[T], mut f: impl FnMut(&T) -> Object) -> Object {
    Object::new_list(items.iter().map(&mut f).collect())
}

impl Builder<'_> {
    fn module(&self, m: &past::Module, mode: &str) -> Object {
        let body = list_of(&m.body, |s| self.stmt(s));
        match mode {
            "eval" => {
                // Expression(body=<expr>): only valid for a single Expr stmt.
                let inner = m.body.first().and_then(|s| match &s.kind {
                    past::StmtKind::Expr(e) => Some(self.expr(e)),
                    _ => None,
                });
                node_noloc("Expression", vec![("body", inner.unwrap_or(Object::None))])
            }
            "single" => node_noloc("Interactive", vec![("body", body)]),
            _ => node_noloc(
                "Module",
                vec![("body", body), ("type_ignores", Object::new_list(vec![]))],
            ),
        }
    }

    fn stmt(&self, s: &past::Stmt) -> Object {
        use past::StmtKind as S;
        let sp = s.span;
        match &s.kind {
            S::FunctionDef {
                name,
                args,
                body,
                decorator_list,
                returns,
                ..
            } => node(
                "FunctionDef",
                vec![
                    ("name", ident(name)),
                    ("args", self.arguments(args)),
                    ("body", list_of(body, |x| self.stmt(x))),
                    ("decorator_list", list_of(decorator_list, |x| self.expr(x))),
                    (
                        "returns",
                        returns.as_deref().map_or(Object::None, |r| self.expr(r)),
                    ),
                    ("type_comment", Object::None),
                    ("type_params", Object::new_list(vec![])),
                ],
                sp,
                self.lm,
            ),
            S::AsyncFunctionDef {
                name,
                args,
                body,
                decorator_list,
                returns,
                ..
            } => node(
                "AsyncFunctionDef",
                vec![
                    ("name", ident(name)),
                    ("args", self.arguments(args)),
                    ("body", list_of(body, |x| self.stmt(x))),
                    ("decorator_list", list_of(decorator_list, |x| self.expr(x))),
                    (
                        "returns",
                        returns.as_deref().map_or(Object::None, |r| self.expr(r)),
                    ),
                    ("type_comment", Object::None),
                    ("type_params", Object::new_list(vec![])),
                ],
                sp,
                self.lm,
            ),
            S::ClassDef {
                name,
                bases,
                keywords,
                body,
                decorator_list,
                ..
            } => node(
                "ClassDef",
                vec![
                    ("name", ident(name)),
                    ("bases", list_of(bases, |x| self.expr(x))),
                    ("keywords", list_of(keywords, |k| self.keyword(k))),
                    ("body", list_of(body, |x| self.stmt(x))),
                    ("decorator_list", list_of(decorator_list, |x| self.expr(x))),
                    ("type_params", Object::new_list(vec![])),
                ],
                sp,
                self.lm,
            ),
            S::Return(value) => node(
                "Return",
                vec![("value", self.opt_expr(value.as_ref()))],
                sp,
                self.lm,
            ),
            S::Assign { targets, value } => node(
                "Assign",
                vec![
                    ("targets", list_of(targets, |x| self.expr(x))),
                    ("value", self.expr(value)),
                    ("type_comment", Object::None),
                ],
                sp,
                self.lm,
            ),
            S::AugAssign { target, op, value } => node(
                "AugAssign",
                vec![
                    ("target", self.expr(target)),
                    ("op", singleton(op.as_str())),
                    ("value", self.expr(value)),
                ],
                sp,
                self.lm,
            ),
            S::AnnAssign {
                target,
                annotation,
                value,
            } => node(
                "AnnAssign",
                vec![
                    ("target", self.expr(target)),
                    ("annotation", self.expr(annotation)),
                    ("value", self.opt_expr(value.as_ref())),
                    ("simple", Object::Int(1)),
                ],
                sp,
                self.lm,
            ),
            S::If { test, body, orelse } => node(
                "If",
                vec![
                    ("test", self.expr(test)),
                    ("body", list_of(body, |x| self.stmt(x))),
                    ("orelse", list_of(orelse, |x| self.stmt(x))),
                ],
                sp,
                self.lm,
            ),
            S::While { test, body, orelse } => node(
                "While",
                vec![
                    ("test", self.expr(test)),
                    ("body", list_of(body, |x| self.stmt(x))),
                    ("orelse", list_of(orelse, |x| self.stmt(x))),
                ],
                sp,
                self.lm,
            ),
            S::For {
                target,
                iter,
                body,
                orelse,
            } => node(
                "For",
                vec![
                    ("target", self.expr(target)),
                    ("iter", self.expr(iter)),
                    ("body", list_of(body, |x| self.stmt(x))),
                    ("orelse", list_of(orelse, |x| self.stmt(x))),
                    ("type_comment", Object::None),
                ],
                sp,
                self.lm,
            ),
            S::AsyncFor {
                target,
                iter,
                body,
                orelse,
            } => node(
                "AsyncFor",
                vec![
                    ("target", self.expr(target)),
                    ("iter", self.expr(iter)),
                    ("body", list_of(body, |x| self.stmt(x))),
                    ("orelse", list_of(orelse, |x| self.stmt(x))),
                    ("type_comment", Object::None),
                ],
                sp,
                self.lm,
            ),
            S::Try {
                body,
                handlers,
                orelse,
                finalbody,
            } => {
                // CPython models `try/except*` as a distinct `TryStar`
                // node; WeavePy carries the star flag on each handler.
                let is_star = handlers.iter().any(|h| h.is_star);
                node(
                    if is_star { "TryStar" } else { "Try" },
                    vec![
                        ("body", list_of(body, |x| self.stmt(x))),
                        ("handlers", list_of(handlers, |h| self.handler(h))),
                        ("orelse", list_of(orelse, |x| self.stmt(x))),
                        ("finalbody", list_of(finalbody, |x| self.stmt(x))),
                    ],
                    sp,
                    self.lm,
                )
            }
            S::Raise { exc, cause } => node(
                "Raise",
                vec![
                    ("exc", self.opt_expr(exc.as_ref())),
                    ("cause", self.opt_expr(cause.as_ref())),
                ],
                sp,
                self.lm,
            ),
            S::With { items, body } => node(
                "With",
                vec![
                    ("items", list_of(items, |i| self.withitem(i))),
                    ("body", list_of(body, |x| self.stmt(x))),
                    ("type_comment", Object::None),
                ],
                sp,
                self.lm,
            ),
            S::AsyncWith { items, body } => node(
                "AsyncWith",
                vec![
                    ("items", list_of(items, |i| self.withitem(i))),
                    ("body", list_of(body, |x| self.stmt(x))),
                    ("type_comment", Object::None),
                ],
                sp,
                self.lm,
            ),
            S::Import(aliases) => node(
                "Import",
                vec![("names", list_of(aliases, alias))],
                sp,
                self.lm,
            ),
            S::ImportFrom {
                module,
                names,
                level,
            } => node(
                "ImportFrom",
                vec![
                    ("module", opt_ident(module.as_deref())),
                    ("names", list_of(names, alias)),
                    ("level", Object::Int(i64::from(*level))),
                ],
                sp,
                self.lm,
            ),
            S::Global(names) => node(
                "Global",
                vec![("names", list_of(names, |n| ident(n)))],
                sp,
                self.lm,
            ),
            S::Nonlocal(names) => node(
                "Nonlocal",
                vec![("names", list_of(names, |n| ident(n)))],
                sp,
                self.lm,
            ),
            S::Match { subject, cases } => node(
                "Match",
                vec![
                    ("subject", self.expr(subject)),
                    ("cases", list_of(cases, |c| self.match_case(c))),
                ],
                sp,
                self.lm,
            ),
            S::Expr(e) => node("Expr", vec![("value", self.expr(e))], sp, self.lm),
            S::Pass => node("Pass", vec![], sp, self.lm),
            S::Break => node("Break", vec![], sp, self.lm),
            S::Continue => node("Continue", vec![], sp, self.lm),
            S::Delete(targets) => node(
                "Delete",
                vec![("targets", list_of(targets, |x| self.expr(x)))],
                sp,
                self.lm,
            ),
            S::Assert { test, msg } => node(
                "Assert",
                vec![
                    ("test", self.expr(test)),
                    ("msg", self.opt_expr(msg.as_ref())),
                ],
                sp,
                self.lm,
            ),
        }
    }

    fn expr(&self, e: &past::Expr) -> Object {
        use past::ExprKind as E;
        let sp = e.span;
        match &e.kind {
            E::Constant(c) => node(
                "Constant",
                vec![("value", constant(c)), ("kind", Object::None)],
                sp,
                self.lm,
            ),
            E::Name(id) => node(
                "Name",
                vec![("id", ident(id)), ("ctx", singleton("Load"))],
                sp,
                self.lm,
            ),
            E::Attribute { value, attr } => node(
                "Attribute",
                vec![
                    ("value", self.expr(value)),
                    ("attr", ident(attr)),
                    ("ctx", singleton("Load")),
                ],
                sp,
                self.lm,
            ),
            E::Subscript { value, slice } => node(
                "Subscript",
                vec![
                    ("value", self.expr(value)),
                    ("slice", self.expr(slice)),
                    ("ctx", singleton("Load")),
                ],
                sp,
                self.lm,
            ),
            E::Slice { lower, upper, step } => node(
                "Slice",
                vec![
                    ("lower", self.opt_boxed(lower.as_deref())),
                    ("upper", self.opt_boxed(upper.as_deref())),
                    ("step", self.opt_boxed(step.as_deref())),
                ],
                sp,
                self.lm,
            ),
            E::BinOp { left, op, right } => node(
                "BinOp",
                vec![
                    ("left", self.expr(left)),
                    ("op", singleton(op.as_str())),
                    ("right", self.expr(right)),
                ],
                sp,
                self.lm,
            ),
            E::BoolOp { op, values } => node(
                "BoolOp",
                vec![
                    ("op", singleton(op.as_str())),
                    ("values", list_of(values, |x| self.expr(x))),
                ],
                sp,
                self.lm,
            ),
            E::UnaryOp { op, operand } => node(
                "UnaryOp",
                vec![
                    ("op", singleton(op.as_str())),
                    ("operand", self.expr(operand)),
                ],
                sp,
                self.lm,
            ),
            E::Compare {
                left,
                ops,
                comparators,
            } => node(
                "Compare",
                vec![
                    ("left", self.expr(left)),
                    ("ops", list_of(ops, |o| singleton(o.as_str()))),
                    ("comparators", list_of(comparators, |x| self.expr(x))),
                ],
                sp,
                self.lm,
            ),
            E::IfExp { test, body, orelse } => node(
                "IfExp",
                vec![
                    ("test", self.expr(test)),
                    ("body", self.expr(body)),
                    ("orelse", self.expr(orelse)),
                ],
                sp,
                self.lm,
            ),
            E::NamedExpr { target, value } => node(
                "NamedExpr",
                vec![("target", self.expr(target)), ("value", self.expr(value))],
                sp,
                self.lm,
            ),
            E::Lambda { args, body } => node(
                "Lambda",
                vec![("args", self.arguments(args)), ("body", self.expr(body))],
                sp,
                self.lm,
            ),
            E::Call {
                func,
                args,
                keywords,
            } => node(
                "Call",
                vec![
                    ("func", self.expr(func)),
                    ("args", list_of(args, |x| self.expr(x))),
                    ("keywords", list_of(keywords, |k| self.keyword(k))),
                ],
                sp,
                self.lm,
            ),
            E::Tuple(items) => node(
                "Tuple",
                vec![
                    ("elts", list_of(items, |x| self.expr(x))),
                    ("ctx", singleton("Load")),
                ],
                sp,
                self.lm,
            ),
            E::List(items) => node(
                "List",
                vec![
                    ("elts", list_of(items, |x| self.expr(x))),
                    ("ctx", singleton("Load")),
                ],
                sp,
                self.lm,
            ),
            E::Set(items) => node(
                "Set",
                vec![("elts", list_of(items, |x| self.expr(x)))],
                sp,
                self.lm,
            ),
            E::Dict { keys, values } => node(
                "Dict",
                vec![
                    ("keys", list_of(keys, |k| self.opt_expr(k.as_ref()))),
                    ("values", list_of(values, |x| self.expr(x))),
                ],
                sp,
                self.lm,
            ),
            E::ListComp { elt, generators } => node(
                "ListComp",
                vec![
                    ("elt", self.expr(elt)),
                    ("generators", list_of(generators, |g| self.comprehension(g))),
                ],
                sp,
                self.lm,
            ),
            E::SetComp { elt, generators } => node(
                "SetComp",
                vec![
                    ("elt", self.expr(elt)),
                    ("generators", list_of(generators, |g| self.comprehension(g))),
                ],
                sp,
                self.lm,
            ),
            E::DictComp {
                key,
                value,
                generators,
            } => node(
                "DictComp",
                vec![
                    ("key", self.expr(key)),
                    ("value", self.expr(value)),
                    ("generators", list_of(generators, |g| self.comprehension(g))),
                ],
                sp,
                self.lm,
            ),
            E::GeneratorExp { elt, generators } => node(
                "GeneratorExp",
                vec![
                    ("elt", self.expr(elt)),
                    ("generators", list_of(generators, |g| self.comprehension(g))),
                ],
                sp,
                self.lm,
            ),
            E::Starred(value) => node(
                "Starred",
                vec![("value", self.expr(value)), ("ctx", singleton("Load"))],
                sp,
                self.lm,
            ),
            E::Yield(value) => node(
                "Yield",
                vec![("value", self.opt_boxed(value.as_deref()))],
                sp,
                self.lm,
            ),
            E::YieldFrom(value) => {
                node("YieldFrom", vec![("value", self.expr(value))], sp, self.lm)
            }
            E::Await(value) => node("Await", vec![("value", self.expr(value))], sp, self.lm),
            E::JoinedStr(parts) => node(
                "JoinedStr",
                vec![("values", list_of(parts, |x| self.expr(x)))],
                sp,
                self.lm,
            ),
            E::FormattedValue {
                value,
                conversion,
                format_spec,
            } => node(
                "FormattedValue",
                vec![
                    ("value", self.expr(value)),
                    ("conversion", Object::Int(i64::from(*conversion))),
                    ("format_spec", self.opt_boxed(format_spec.as_deref())),
                ],
                sp,
                self.lm,
            ),
        }
    }

    fn opt_expr(&self, e: Option<&past::Expr>) -> Object {
        match e {
            Some(x) => self.expr(x),
            None => Object::None,
        }
    }

    fn opt_boxed(&self, e: Option<&past::Expr>) -> Object {
        match e {
            Some(x) => self.expr(x),
            None => Object::None,
        }
    }

    fn keyword(&self, k: &past::Keyword) -> Object {
        node_noloc(
            "keyword",
            vec![
                ("arg", opt_ident(k.arg.as_deref())),
                ("value", self.expr(&k.value)),
            ],
        )
    }

    fn comprehension(&self, c: &past::Comprehension) -> Object {
        node_noloc(
            "comprehension",
            vec![
                ("target", self.expr(&c.target)),
                ("iter", self.expr(&c.iter)),
                ("ifs", list_of(&c.ifs, |x| self.expr(x))),
                ("is_async", Object::Int(i64::from(c.is_async))),
            ],
        )
    }

    fn handler(&self, h: &past::ExceptHandler) -> Object {
        // Both `except` and `except*` use the `ExceptHandler` node class;
        // the star-ness lives on the enclosing `Try`/`TryStar`.
        node(
            "ExceptHandler",
            vec![
                ("type", self.opt_expr(h.type_.as_ref())),
                ("name", opt_ident(h.name.as_deref())),
                ("body", list_of(&h.body, |x| self.stmt(x))),
            ],
            h.span,
            self.lm,
        )
    }

    fn withitem(&self, w: &past::WithItem) -> Object {
        node_noloc(
            "withitem",
            vec![
                ("context_expr", self.expr(&w.context_expr)),
                ("optional_vars", self.opt_expr(w.optional_vars.as_ref())),
            ],
        )
    }

    fn match_case(&self, c: &past::MatchCase) -> Object {
        node_noloc(
            "match_case",
            vec![
                ("pattern", self.pattern(&c.pattern)),
                ("guard", self.opt_expr(c.guard.as_ref())),
                ("body", list_of(&c.body, |x| self.stmt(x))),
            ],
        )
    }

    fn pattern(&self, p: &past::Pattern) -> Object {
        use past::Pattern as P;
        match p {
            P::Value(e) => node_noloc("MatchValue", vec![("value", self.expr(e))]),
            P::Singleton(c) => node_noloc("MatchSingleton", vec![("value", constant(c))]),
            P::Capture(name) => node_noloc(
                "MatchAs",
                vec![
                    ("pattern", Object::None),
                    ("name", opt_ident(name.as_deref())),
                ],
            ),
            P::Sequence(items) => node_noloc(
                "MatchSequence",
                vec![("patterns", list_of(items, |x| self.pattern(x)))],
            ),
            P::Star(name) => node_noloc("MatchStar", vec![("name", opt_ident(name.as_deref()))]),
            P::Mapping {
                keys,
                patterns,
                rest,
            } => node_noloc(
                "MatchMapping",
                vec![
                    ("keys", list_of(keys, |k| self.expr(k))),
                    ("patterns", list_of(patterns, |x| self.pattern(x))),
                    (
                        "rest",
                        match rest {
                            Some(Some(n)) => Object::from_str(n.clone()),
                            _ => Object::None,
                        },
                    ),
                ],
            ),
            P::Class {
                cls,
                positionals,
                keywords,
            } => node_noloc(
                "MatchClass",
                vec![
                    ("cls", self.expr(cls)),
                    ("patterns", list_of(positionals, |x| self.pattern(x))),
                    ("kwd_attrs", list_of(keywords, |(n, _)| ident(n))),
                    ("kwd_patterns", list_of(keywords, |(_, p)| self.pattern(p))),
                ],
            ),
            P::Or(items) => node_noloc(
                "MatchOr",
                vec![("patterns", list_of(items, |x| self.pattern(x)))],
            ),
            P::As { pattern, name } => node_noloc(
                "MatchAs",
                vec![
                    ("pattern", self.pattern(pattern)),
                    ("name", Object::from_str(name.clone())),
                ],
            ),
        }
    }

    fn arguments(&self, a: &past::Arguments) -> Object {
        node_noloc(
            "arguments",
            vec![
                ("posonlyargs", list_of(&a.posonlyargs, |x| self.arg(x))),
                ("args", list_of(&a.args, |x| self.arg(x))),
                ("vararg", self.opt_arg(a.vararg.as_ref())),
                ("kwonlyargs", list_of(&a.kwonlyargs, |x| self.arg(x))),
                (
                    "kw_defaults",
                    list_of(&a.kw_defaults, |d| self.opt_expr(d.as_ref())),
                ),
                ("kwarg", self.opt_arg(a.kwarg.as_ref())),
                ("defaults", list_of(&a.defaults, |x| self.expr(x))),
            ],
        )
    }

    fn arg(&self, a: &past::Arg) -> Object {
        let annotation = match &a.annotation {
            Some(e) => self.expr(e),
            None => Object::None,
        };
        node(
            "arg",
            vec![
                ("arg", ident(&a.name)),
                ("annotation", annotation),
                ("type_comment", Object::None),
            ],
            a.span,
            self.lm,
        )
    }

    fn opt_arg(&self, a: Option<&past::Arg>) -> Object {
        match a {
            Some(x) => self.arg(x),
            None => Object::None,
        }
    }
}

fn alias(a: &past::Alias) -> Object {
    node_noloc(
        "alias",
        vec![
            ("name", ident(&a.name)),
            ("asname", opt_ident(a.asname.as_deref())),
        ],
    )
}

/// Lower a parser literal into the runtime value `ast.Constant.value`
/// should hold.
fn constant(c: &past::Constant) -> Object {
    use past::Constant as C;
    match c {
        C::None => Object::None,
        C::Bool(b) => Object::Bool(*b),
        C::Int(i) => Object::Int(*i),
        C::BigInt(repr) => repr
            .parse::<num_bigint::BigInt>()
            .map(Object::int_from_bigint)
            .unwrap_or(Object::Int(0)),
        C::Float(f) => Object::Float(*f),
        C::Complex(re, im) => Object::new_complex(*re, *im),
        C::Str(s) => Object::from_str(s.clone()),
        C::Bytes(b) => Object::new_bytes(b.clone()),
        C::Tuple(items) => Object::new_tuple(items.iter().map(constant).collect()),
        C::Ellipsis => crate::vm_singletons::ellipsis(),
    }
}

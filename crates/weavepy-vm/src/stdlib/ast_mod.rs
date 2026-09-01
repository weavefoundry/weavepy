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
    let dict = Rc::new(RefCell::new(DictData::default()));
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
            binds_instance: false,
            call: Box::new(parse),
            call_kw: None,
        };
        d.insert(
            DictKey(Object::from_static("parse")),
            Object::Builtin(Rc::new(bf)),
        );
        // `compile()` control flags (CPython `_ast` exposes these;
        // `ast.py` re-exports them) — RFC 0052.
        use weavepy_compiler::flags as cf;
        for (name, value) in [
            ("PyCF_ONLY_AST", cf::PYCF_ONLY_AST),
            ("PyCF_TYPE_COMMENTS", cf::PYCF_TYPE_COMMENTS),
            ("PyCF_ALLOW_TOP_LEVEL_AWAIT", cf::PYCF_ALLOW_TOP_LEVEL_AWAIT),
            ("PyCF_OPTIMIZED_AST", cf::PYCF_OPTIMIZED_AST),
        ] {
            d.insert(
                DictKey(Object::from_static(name)),
                Object::Int(i64::from(value)),
            );
        }
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
    // PEP 484 signature type comments: `(t1, t2) -> ret` parses under
    // its own start rule into a `FunctionType` root (a `mod` — no
    // position attributes).
    if mode == "func_type" {
        let (argtypes, returns) = weavepy_parser::parse_func_type(&source)
            .map_err(|e| crate::parse_error_to_syntax_error(&e, &source, &filename))?;
        let lm = LineMap::new(&source);
        let b = Builder {
            lm: &lm,
            src: &source,
            tc_stmts: std::collections::HashMap::default(),
            tc_args: std::collections::HashMap::default(),
            tc_ignores: Vec::new(),
        };
        let spec = node_noloc(
            "FunctionType",
            vec![
                ("argtypes", list_of(&argtypes, |e| b.expr(e))),
                ("returns", b.expr(&returns)),
            ],
        );
        fix_contexts(&spec);
        return Ok(spec);
    }
    // PyCF_TYPE_COMMENTS (`ast.parse(..., type_comments=True)`): the
    // parser collects `# type:` comments into side tables — statement /
    // per-argument `type_comment` strings plus `Module.type_ignores` —
    // and rejects misplaced ones, mirroring pegen's TYPE_COMMENT tokens.
    let type_comments = matches!(args.get(3), Some(Object::Bool(true)));
    // CPython raises `SyntaxError` (never `ValueError`) from
    // `ast.parse` — callers like `traceback`'s caret-anchor probe rely
    // on `except SyntaxError` swallowing bad segments.
    let (module, tc) = if type_comments {
        let (m, t) = weavepy_parser::parse_module_type_comments(&source)
            .map_err(|e| crate::parse_error_to_syntax_error(&e, &source, &filename))?;
        (m, Some(t))
    } else {
        let m = weavepy_parser::parse_module(&source)
            .map_err(|e| crate::parse_error_to_syntax_error(&e, &source, &filename))?;
        (m, None)
    };
    let lm = LineMap::new(&source);
    let (tc_stmts, tc_args, tc_ignores) = match tc {
        Some(t) => (
            t.stmts.into_iter().collect(),
            t.args.into_iter().collect(),
            t.ignores,
        ),
        None => Default::default(),
    };
    let b = Builder {
        lm: &lm,
        src: &source,
        tc_stmts,
        tc_args,
        tc_ignores,
    };
    let spec = b.module(&module, &mode);
    fix_contexts(&spec);
    Ok(spec)
}

/// A field of a spec-node dict, by key.
fn spec_field(node: &Object, key: &'static str) -> Option<Object> {
    match node {
        Object::Dict(d) => d.borrow().get(&DictKey(Object::from_static(key))).cloned(),
        _ => None,
    }
}

/// The `_type` tag of a spec-node dict.
fn spec_type(node: &Object) -> Option<Rc<str>> {
    match spec_field(node, "_type") {
        Some(Object::Str(s)) => Some(s),
        _ => None,
    }
}

/// The string payload of a `Constant` spec node (`None` for any other
/// node shape or a non-str constant).
fn const_str_of(node: &Object) -> Option<Rc<str>> {
    if !matches!(spec_type(node).as_deref(), Some("Constant")) {
        return None;
    }
    match spec_field(node, "value") {
        Some(Object::Str(s)) => Some(s),
        _ => None,
    }
}

/// Stamp `ctx` onto an expression in a store/del position, recursing
/// through tuple/list/starred targets (CPython's `set_context`).
/// `Attribute`/`Subscript` only flip their own `ctx`; their
/// `.value`/`.slice` stay `Load`.
fn set_ctx(node: &Object, ctx: &'static str) {
    let Some(ty) = spec_type(node) else { return };
    if !matches!(
        &*ty,
        "Name" | "Attribute" | "Subscript" | "Starred" | "List" | "Tuple"
    ) {
        return;
    }
    if let Object::Dict(d) = node {
        d.borrow_mut()
            .insert(DictKey(Object::from_static("ctx")), singleton(ctx));
    }
    match &*ty {
        "List" | "Tuple" => {
            if let Some(Object::List(elts)) = spec_field(node, "elts") {
                let elts = elts.borrow().clone();
                for elt in &elts {
                    set_ctx(elt, ctx);
                }
            }
        }
        "Starred" => {
            if let Some(v) = spec_field(node, "value") {
                set_ctx(&v, ctx);
            }
        }
        _ => {}
    }
}

/// The parser doesn't track expression contexts, and the [`Builder`]
/// stamps `Load` everywhere; rewrite `ctx` to `Store`/`Del` for
/// assignment/deletion targets so `ast.dump` matches CPython. Done here,
/// on the spec dicts, rather than in `ast.py` — the Python tree re-walk
/// used to dominate `ast.parse` (~3x the cost of node construction).
fn fix_contexts(root: &Object) {
    let mut todo: Vec<Object> = vec![root.clone()];
    while let Some(cur) = todo.pop() {
        match &cur {
            Object::List(items) => todo.extend(items.borrow().iter().cloned()),
            Object::Dict(d) => {
                if let Some(ty) = spec_type(&cur) {
                    match &*ty {
                        "Assign" => {
                            if let Some(Object::List(ts)) = spec_field(&cur, "targets") {
                                let ts = ts.borrow().clone();
                                for t in &ts {
                                    set_ctx(t, "Store");
                                }
                            }
                        }
                        "AugAssign" | "AnnAssign" | "NamedExpr" | "For" | "AsyncFor"
                        | "comprehension" => {
                            if let Some(t) = spec_field(&cur, "target") {
                                set_ctx(&t, "Store");
                            }
                        }
                        "Delete" => {
                            if let Some(Object::List(ts)) = spec_field(&cur, "targets") {
                                let ts = ts.borrow().clone();
                                for t in &ts {
                                    set_ctx(t, "Del");
                                }
                            }
                        }
                        "With" | "AsyncWith" => {
                            if let Some(Object::List(items)) = spec_field(&cur, "items") {
                                let items = items.borrow().clone();
                                for item in &items {
                                    match spec_field(item, "optional_vars") {
                                        Some(Object::None) | None => {}
                                        Some(ov) => set_ctx(&ov, "Store"),
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
                }
                let children: Vec<Object> = d
                    .borrow()
                    .values()
                    .filter(|v| matches!(v, Object::Dict(_) | Object::List(_)))
                    .cloned()
                    .collect();
                todo.extend(children);
            }
            _ => {}
        }
    }
}

/// Byte-offset → (1-based line, 0-based UTF-8 column) resolver.
struct LineMap {
    /// Byte offset of each `'\n'` in the source.
    newlines: Vec<usize>,
}

impl LineMap {
    fn new(source: &str) -> Self {
        // The tokenizer treats `\n`, `\r\n`, and a lone `\r` as line
        // terminators (test_source_segment_endings); record the offset of
        // each terminator's final byte.
        let bytes = source.as_bytes();
        let mut newlines = Vec::new();
        for (i, &b) in bytes.iter().enumerate() {
            if b == b'\n' || (b == b'\r' && bytes.get(i + 1) != Some(&b'\n')) {
                newlines.push(i);
            }
        }
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
    /// Original source text — consulted for details the Rust AST does
    /// not carry (the `u` string-prefix that populates `Constant.kind`).
    src: &'a str,
    /// PEP 484 type-comment side tables (`type_comments=True`), keyed by
    /// node span-start byte offset; empty on the default path.
    tc_stmts: std::collections::HashMap<u32, String>,
    tc_args: std::collections::HashMap<u32, String>,
    /// `# type: ignore<tag>` comments: (comment start offset, tag).
    tc_ignores: Vec<(u32, String)>,
}

/// Build a node `dict` with `_type`, the given fields, and the four
/// location attributes derived from `span`.
fn node(ty: &str, fields: Vec<(&str, Object)>, span: Span, lm: &LineMap) -> Object {
    let mut d = DictData::default();
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
    let mut d = DictData::default();
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
    /// The claimed `# type:` comment for the statement starting at
    /// `sp.start`, or `None`.
    fn stmt_type_comment(&self, sp: Span) -> Object {
        match self.tc_stmts.get(&sp.start.0) {
            Some(t) => Object::from_str(t.clone()),
            None => Object::None,
        }
    }

    fn arg_type_comment(&self, sp: Span) -> Object {
        match self.tc_args.get(&sp.start.0) {
            Some(t) => Object::from_str(t.clone()),
            None => Object::None,
        }
    }

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
            _ => {
                let ignores = self
                    .tc_ignores
                    .iter()
                    .map(|(off, tag)| {
                        let (lineno, _) = self.lm.pos(*off);
                        node_noloc(
                            "TypeIgnore",
                            vec![
                                ("lineno", Object::Int(lineno)),
                                ("tag", Object::from_str(tag.clone())),
                            ],
                        )
                    })
                    .collect();
                node_noloc(
                    "Module",
                    vec![("body", body), ("type_ignores", Object::new_list(ignores))],
                )
            }
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
                type_params,
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
                    ("type_comment", self.stmt_type_comment(sp)),
                    ("type_params", self.type_params(type_params)),
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
                type_params,
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
                    ("type_comment", self.stmt_type_comment(sp)),
                    ("type_params", self.type_params(type_params)),
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
                type_params,
            } => node(
                "ClassDef",
                vec![
                    ("name", ident(name)),
                    ("bases", list_of(bases, |x| self.expr(x))),
                    ("keywords", list_of(keywords, |k| self.keyword(k))),
                    ("body", list_of(body, |x| self.stmt(x))),
                    ("decorator_list", list_of(decorator_list, |x| self.expr(x))),
                    ("type_params", self.type_params(type_params)),
                ],
                sp,
                self.lm,
            ),
            S::TypeAlias {
                name,
                name_span,
                type_params,
                value,
            } => node(
                "TypeAlias",
                vec![
                    (
                        "name",
                        node(
                            "Name",
                            vec![("id", ident(name)), ("ctx", singleton("Store"))],
                            *name_span,
                            self.lm,
                        ),
                    ),
                    ("type_params", self.type_params(type_params)),
                    ("value", self.expr(value)),
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
                    ("type_comment", self.stmt_type_comment(sp)),
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
                simple,
            } => node(
                "AnnAssign",
                vec![
                    ("target", self.expr(target)),
                    ("annotation", self.expr(annotation)),
                    ("value", self.opt_expr(value.as_ref())),
                    ("simple", Object::Int(i64::from(*simple))),
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
                    ("type_comment", self.stmt_type_comment(sp)),
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
                    ("type_comment", self.stmt_type_comment(sp)),
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
                    ("type_comment", self.stmt_type_comment(sp)),
                ],
                sp,
                self.lm,
            ),
            S::AsyncWith { items, body } => node(
                "AsyncWith",
                vec![
                    ("items", list_of(items, |i| self.withitem(i))),
                    ("body", list_of(body, |x| self.stmt(x))),
                    ("type_comment", self.stmt_type_comment(sp)),
                ],
                sp,
                self.lm,
            ),
            S::Import(aliases) => node(
                "Import",
                vec![("names", list_of(aliases, |a| self.alias(a)))],
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
                    ("names", list_of(names, |a| self.alias(a))),
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
            E::Constant(c) => {
                // `Constant.kind` is `"u"` for a u-prefixed str literal
                // (PEP 414), `None` otherwise. The Rust AST doesn't keep
                // the prefix, so consult the literal's source text: the
                // prefix letter must be *immediately* followed by a quote,
                // or this is not a literal prefix (e.g. the text piece of
                // an f-string whose span starts mid-literal at a `u`).
                let kind = match c {
                    past::Constant::Str(_) | past::Constant::WStr(_)
                        if matches!(
                            self.src.as_bytes().get(sp.start.0 as usize),
                            Some(b'u' | b'U')
                        ) && matches!(
                            self.src.as_bytes().get(sp.start.0 as usize + 1),
                            Some(b'\'' | b'"')
                        ) =>
                    {
                        Object::from_static("u")
                    }
                    _ => Object::None,
                };
                node(
                    "Constant",
                    vec![("value", constant(c)), ("kind", kind)],
                    sp,
                    self.lm,
                )
            }
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
            // `TypeParamFn` is compiler-generated (PEP 695 lowering)
            // and never reaches user-visible ASTs, but render it as
            // the lambda it is shaped like just in case.
            E::Lambda { args, body } | E::TypeParamFn { args, body } => node(
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
                vec![("values", self.joinedstr_values(parts))],
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
            // PEP 750 t-strings (`-X lang=next`): CPython 3.14 node
            // shapes (`TemplateStr(values)`, `Interpolation(value, str,
            // conversion, format_spec)`).
            E::TemplateStr(parts) => node(
                "TemplateStr",
                vec![("values", self.joinedstr_values(parts))],
                sp,
                self.lm,
            ),
            E::Interpolation {
                value,
                text,
                conversion,
                format_spec,
            } => node(
                "Interpolation",
                vec![
                    ("value", self.expr(value)),
                    ("str", Object::from_str(text.clone())),
                    ("conversion", Object::Int(i64::from(*conversion))),
                    ("format_spec", self.opt_boxed(format_spec.as_deref())),
                ],
                sp,
                self.lm,
            ),
        }
    }

    /// `JoinedStr.values` with adjacent string constants coalesced —
    /// CPython's parser emits one `Constant` for consecutive literal
    /// segments (the text between `{}` fields, `=`-debug prefixes,
    /// implicit concatenation), so `f"{a=} {b=}"` has `Constant(' b=')`,
    /// not `Constant(' ')`, `Constant('b=')`. `ast.unparse` round-trips
    /// rely on the merged shape (test_unparse on e.g. test_pow.py).
    fn joinedstr_values(&self, parts: &[past::Expr]) -> Object {
        let mut out: Vec<Object> = Vec::with_capacity(parts.len());
        for p in parts {
            let built = self.expr(p);
            if let Some(prev) = out.last() {
                if let (Some(a), Some(b)) = (const_str_of(prev), const_str_of(&built)) {
                    if let Object::Dict(d) = prev {
                        let mut d = d.borrow_mut();
                        d.insert(
                            DictKey(Object::from_static("value")),
                            Object::from_str(format!("{a}{b}")),
                        );
                        // Extend the merged constant's span to the end of
                        // the absorbed part (compile-from-AST maps
                        // locations back to source bytes).
                        for key in ["end_lineno", "end_col_offset"] {
                            if let Some(v) = spec_field(&built, key) {
                                d.insert(DictKey(Object::from_static(key)), v);
                            }
                        }
                    }
                    continue;
                }
            }
            out.push(built);
        }
        Object::new_list(out)
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
        node(
            "keyword",
            vec![
                ("arg", opt_ident(k.arg.as_deref())),
                ("value", self.expr(&k.value)),
            ],
            k.span,
            self.lm,
        )
    }

    /// PEP 695 type-parameter list → `[ast.TypeVar | ast.TypeVarTuple |
    /// ast.ParamSpec, …]` (with PEP 696 `default_value`).
    fn type_params(&self, tps: &[past::TypeParam]) -> Object {
        list_of(tps, |tp| {
            let default_value = tp.default.as_deref().map_or(Object::None, |d| self.expr(d));
            let fields = match &tp.kind {
                past::TypeParamKind::TypeVar { bound } => vec![
                    ("name", ident(&tp.source_name)),
                    (
                        "bound",
                        bound.as_deref().map_or(Object::None, |b| self.expr(b)),
                    ),
                    ("default_value", default_value),
                ],
                past::TypeParamKind::TypeVarTuple | past::TypeParamKind::ParamSpec => vec![
                    ("name", ident(&tp.source_name)),
                    ("default_value", default_value),
                ],
            };
            let ty = match &tp.kind {
                past::TypeParamKind::TypeVar { .. } => "TypeVar",
                past::TypeParamKind::TypeVarTuple => "TypeVarTuple",
                past::TypeParamKind::ParamSpec => "ParamSpec",
            };
            node(ty, fields, tp.span, self.lm)
        })
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
        use past::PatternKind as P;
        let sp = p.span;
        match &p.kind {
            P::Value(e) => node("MatchValue", vec![("value", self.expr(e))], sp, self.lm),
            P::Singleton(c) => node("MatchSingleton", vec![("value", constant(c))], sp, self.lm),
            P::Capture(name) => node(
                "MatchAs",
                vec![
                    ("pattern", Object::None),
                    ("name", opt_ident(name.as_deref())),
                ],
                sp,
                self.lm,
            ),
            P::Sequence(items) => node(
                "MatchSequence",
                vec![("patterns", list_of(items, |x| self.pattern(x)))],
                sp,
                self.lm,
            ),
            P::Star(name) => node(
                "MatchStar",
                vec![("name", opt_ident(name.as_deref()))],
                sp,
                self.lm,
            ),
            P::Mapping {
                keys,
                patterns,
                rest,
            } => node(
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
                sp,
                self.lm,
            ),
            P::Class {
                cls,
                positionals,
                keywords,
            } => node(
                "MatchClass",
                vec![
                    ("cls", self.expr(cls)),
                    ("patterns", list_of(positionals, |x| self.pattern(x))),
                    ("kwd_attrs", list_of(keywords, |(n, _)| ident(n))),
                    ("kwd_patterns", list_of(keywords, |(_, p)| self.pattern(p))),
                ],
                sp,
                self.lm,
            ),
            P::Or(items) => node(
                "MatchOr",
                vec![("patterns", list_of(items, |x| self.pattern(x)))],
                sp,
                self.lm,
            ),
            P::As { pattern, name } => node(
                "MatchAs",
                vec![
                    ("pattern", self.pattern(pattern)),
                    ("name", Object::from_str(name.clone())),
                ],
                sp,
                self.lm,
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
        let type_comment = self.arg_type_comment(a.span);
        node(
            "arg",
            vec![
                ("arg", ident(&a.name)),
                ("annotation", annotation),
                ("type_comment", type_comment),
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

    fn alias(&self, a: &past::Alias) -> Object {
        node(
            "alias",
            vec![
                ("name", ident(&a.name)),
                ("asname", opt_ident(a.asname.as_deref())),
            ],
            a.span,
            self.lm,
        )
    }
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
        C::WStr(cps) => Object::str_from_codepoints(cps.clone()),
        C::Bytes(b) => Object::new_bytes(b.clone()),
        C::Tuple(items) => Object::new_tuple(items.iter().map(constant).collect()),
        C::FrozenSet(items) => Object::new_frozenset_from(items.iter().map(constant)),
        C::Ellipsis => crate::vm_singletons::ellipsis(),
    }
}

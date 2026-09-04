//! Python-AST → Rust-AST lowering for `compile()` (RFC 0052).
//!
//! CPython's `compile()` accepts an `ast.AST` tree and compiles the
//! tree the caller built — the contract pytest's assertion rewriting,
//! coverage.py, and every AST-mutating tool rely on. This module is
//! WeavePy's analogue of `Python/Python-ast.c`'s `obj2ast_*` family:
//! it walks a Python node tree (any objects exposing the node class
//! names and `_fields`-shaped attributes) and rebuilds the
//! [`weavepy_parser::ast`] tree the compiler consumes.
//!
//! # Position synthesis
//!
//! The Rust AST carries *byte spans* into source text, while Python
//! AST nodes carry `(lineno, col_offset)` pairs — and a synthetic tree
//! has no source text at all. We bridge the two by building a
//! **synthetic source**: pass 1 walks the tree recording the maximum
//! column used on every line, pass 2 lays the lines out as runs of
//! spaces so that byte offset ↔ `(line, col)` is a bijection matching
//! the node positions exactly. The compiler's `LineIndex` over that
//! synthetic source then reproduces every node's line and column in
//! `co_positions()`, tracebacks, and error locations — CPython
//! likewise trusts the tree's positions verbatim.

use crate::sync::Rc;

use weavepy_lexer::token::Span;
use weavepy_parser::ast as past;

use crate::error::{type_error, value_error, RuntimeError};
use crate::object::{Object, StrKey};
use crate::types::PyInstance;

/// Which root node a `compile()` mode requires.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RootMode {
    Exec,
    Eval,
    Single,
}

impl RootMode {
    pub fn from_mode(mode: &str) -> Option<Self> {
        match mode {
            "exec" => Some(Self::Exec),
            "eval" => Some(Self::Eval),
            "single" => Some(Self::Single),
            _ => None,
        }
    }

    fn expected_node(self) -> &'static str {
        match self {
            Self::Exec => "Module",
            Self::Eval => "Expression",
            Self::Single => "Interactive",
        }
    }
}

/// The result of lowering a Python AST object: a parser-shaped module
/// (an `Expression` root becomes a single-statement module the eval
/// compile entry accepts) plus the synthetic source whose `LineIndex`
/// reproduces the tree's positions.
#[derive(Debug)]
pub struct ConvertedAst {
    pub module: past::Module,
    pub synthetic_source: String,
}

/// Is `obj` an AST node instance? Mirrors `PyAST_Check` by duck-typing
/// on the class hierarchy: every `ast.AST` subclass carries `_fields`.
pub fn is_ast_object(obj: &Object) -> bool {
    match obj {
        Object::Instance(inst) => inst.cls().lookup("_fields").is_some(),
        _ => false,
    }
}

/// Lower a Python AST root object into a compilable module.
pub fn convert_ast_root(obj: &Object, mode: RootMode) -> Result<ConvertedAst, RuntimeError> {
    let inst = match obj {
        Object::Instance(inst) => inst,
        _ => {
            return Err(type_error(format!(
                "expected {} node, got {}",
                mode.expected_node(),
                obj.type_name()
            )))
        }
    };
    let name = inst.cls().name.clone();
    if name != mode.expected_node() {
        return Err(type_error(format!(
            "expected {} node, got {}",
            mode.expected_node(),
            name
        )));
    }

    // Pass 1: collect per-line maximum columns for span synthesis.
    let mut collector = PosCollector::default();
    collector.walk(obj, 0)?;
    let pos = collector.finish();

    let mut conv = Conv { pos: &pos };
    let module = match mode {
        RootMode::Exec | RootMode::Single => {
            let body = field(inst, "body").ok_or_else(|| missing_field("body", &name))?;
            past::Module {
                body: conv.stmt_list(&body, &name)?,
            }
        }
        RootMode::Eval => {
            let body = conv.req_node(inst, "body", "Expression")?;
            let expr = conv.expr(&body)?;
            let span = expr.span;
            past::Module {
                body: vec![past::Stmt {
                    kind: past::StmtKind::Expr(expr),
                    span,
                }],
            }
        }
    };
    // Semantic validation (`_PyAST_Validate`) already ran: `compile()`
    // invokes the frozen ast module's `_validate` (the pure-Python port)
    // before this lowering, matching CPython's obj2ast → validate order.
    Ok(ConvertedAst {
        module,
        synthetic_source: pos.synthetic_source(),
    })
}

// ---------------------------------------------------------------------------
// Position synthesis
// ---------------------------------------------------------------------------

/// Recursion limit for tree walks — synthetic trees can be arbitrarily
/// deep; CPython fails with RecursionError, so do we.
const MAX_DEPTH: usize = 2000;

#[derive(Default)]
struct PosCollector {
    /// 1-based line → maximum (0-based) column observed.
    line_max: std::collections::BTreeMap<i64, i64>,
}

impl PosCollector {
    fn record(&mut self, line: i64, col: i64) {
        if line >= 1 && col >= 0 {
            let e = self.line_max.entry(line).or_insert(0);
            if col > *e {
                *e = col;
            }
        }
    }

    fn walk(&mut self, obj: &Object, depth: usize) -> Result<(), RuntimeError> {
        if depth > MAX_DEPTH {
            return Err(crate::error::recursion_error(
                "maximum recursion depth exceeded during compilation",
            ));
        }
        match obj {
            Object::Instance(inst) => {
                let d = inst.dict.borrow();
                let int_of = |name: &str| -> Option<i64> {
                    match d.get(&StrKey(name)) {
                        Some(Object::Int(i)) => Some(*i),
                        Some(Object::Bool(b)) => Some(i64::from(*b)),
                        _ => None,
                    }
                };
                if let (Some(l), Some(c)) = (int_of("lineno"), int_of("col_offset")) {
                    self.record(l, c);
                }
                if let (Some(l), Some(c)) = (int_of("end_lineno"), int_of("end_col_offset")) {
                    self.record(l, c);
                }
                // Recurse into attribute values (fields hold the child
                // nodes; unrelated attributes are harmless to scan).
                let children: Vec<Object> = d
                    .iter()
                    .filter(|(_, v)| matches!(v, Object::Instance(_) | Object::List(_)))
                    .map(|(_, v)| v.clone())
                    .collect();
                drop(d);
                for child in children {
                    self.walk(&child, depth + 1)?;
                }
            }
            Object::List(items) => {
                let snapshot: Vec<Object> = items.borrow().clone();
                for item in snapshot {
                    self.walk(&item, depth + 1)?;
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn finish(self) -> PosMap {
        let max_line = self.line_max.keys().next_back().copied().unwrap_or(1);
        let mut line_lengths = vec![0u32; max_line.max(1) as usize];
        for (line, col) in &self.line_max {
            // Line length must admit the max column as a valid offset
            // (plus one so `end == start + 1` spans stay in-line).
            line_lengths[(*line - 1) as usize] = (*col as u32) + 1;
        }
        let mut line_starts = Vec::with_capacity(line_lengths.len());
        let mut acc = 0u32;
        for len in &line_lengths {
            line_starts.push(acc);
            acc += len + 1; // '\n'
        }
        PosMap {
            line_lengths,
            line_starts,
        }
    }
}

/// The byte-offset ↔ `(line, col)` bijection over the synthetic source.
struct PosMap {
    line_lengths: Vec<u32>,
    line_starts: Vec<u32>,
}

impl PosMap {
    fn byte(&self, line: i64, col: i64) -> u32 {
        if line < 1 || self.line_starts.is_empty() {
            return 0;
        }
        let idx = ((line - 1) as usize).min(self.line_starts.len() - 1);
        let col = (col.max(0) as u32).min(self.line_lengths[idx]);
        self.line_starts[idx] + col
    }

    fn synthetic_source(&self) -> String {
        let total: usize = self
            .line_lengths
            .iter()
            .map(|l| *l as usize + 1)
            .sum::<usize>();
        let mut s = String::with_capacity(total);
        for len in &self.line_lengths {
            for _ in 0..*len {
                s.push(' ');
            }
            s.push('\n');
        }
        s
    }
}

// ---------------------------------------------------------------------------
// Field access helpers
// ---------------------------------------------------------------------------

fn field(inst: &Rc<PyInstance>, name: &str) -> Option<Object> {
    if let Some(v) = inst.dict.borrow().get(&StrKey(name)) {
        return Some(v.clone());
    }
    // Class-level defaults (CPython's obj2ast reads through
    // `PyObject_GetAttr`, which sees class attributes too).
    inst.cls().lookup(name)
}

fn missing_field(name: &str, node: &str) -> RuntimeError {
    type_error(format!("required field \"{name}\" missing from {node}"))
}

/// The class name of a node instance, or a `TypeError` shaped like
/// CPython's "expected some sort of {category}, but got {value}".
fn node_name(obj: &Object, category: &str) -> Result<(Rc<PyInstance>, String), RuntimeError> {
    match obj {
        Object::Instance(inst) => {
            let name = inst.cls().name.clone();
            Ok((inst.clone(), name))
        }
        other => Err(type_error(format!(
            "expected some sort of {category}, but got {}",
            repr_lite(other)
        ))),
    }
}

/// A best-effort `repr` for error messages (no interpreter handle here).
fn repr_lite(obj: &Object) -> String {
    match obj {
        Object::None => "None".to_owned(),
        Object::Bool(b) => if *b { "True" } else { "False" }.to_owned(),
        Object::Int(i) => i.to_string(),
        Object::Float(f) => f.to_string(),
        Object::Str(s) => format!("{s:?}").replace('"', "'"),
        Object::Instance(inst) => instance_repr(inst),
        other => format!("<{}>", other.type_name()),
    }
}

/// Default-object-repr shape for a node instance
/// (`<ast.expr object at 0x…>`), module-qualified like CPython —
/// `test_invalid_sum` greps for the `<ast.` prefix.
fn instance_repr(inst: &Rc<PyInstance>) -> String {
    let cls = inst.cls();
    let module = match cls.lookup("__module__") {
        Some(Object::Str(m)) if &*m != "builtins" => format!("{m}."),
        _ => String::new(),
    };
    format!(
        "<{module}{} object at {:#x}>",
        cls.name,
        Rc::as_ptr(inst) as usize
    )
}

fn identifier(obj: &Object) -> Result<String, RuntimeError> {
    match obj {
        Object::Str(s) => Ok(s.to_string()),
        _ => Err(type_error("AST identifier must be of type str")),
    }
}

fn opt_identifier(obj: Option<Object>) -> Result<Option<String>, RuntimeError> {
    match obj {
        None | Some(Object::None) => Ok(None),
        Some(v) => Ok(Some(identifier(&v)?)),
    }
}

fn int_field(obj: &Object, what: &str) -> Result<i64, RuntimeError> {
    match obj {
        Object::Int(i) => Ok(*i),
        Object::Bool(b) => Ok(i64::from(*b)),
        _ => Err(value_error(format!(
            "invalid integer value for field {what}"
        ))),
    }
}

fn list_items(obj: &Object, node: &str, fieldname: &str) -> Result<Vec<Object>, RuntimeError> {
    match obj {
        Object::List(items) => Ok(items.borrow().clone()),
        // CPython accepts only exact lists here.
        other => Err(type_error(format!(
            "{node} field \"{fieldname}\" must be a list, not a {}",
            other.type_name()
        ))),
    }
}

// ---------------------------------------------------------------------------
// Node conversion
// ---------------------------------------------------------------------------

struct Conv<'a> {
    pos: &'a PosMap,
}

impl Conv<'_> {
    fn span_of(&self, inst: &Rc<PyInstance>, category: &str) -> Result<Span, RuntimeError> {
        let lineno = field(inst, "lineno")
            .ok_or_else(|| missing_field("lineno", category))
            .and_then(|v| int_field(&v, "lineno"))?;
        let col = field(inst, "col_offset")
            .ok_or_else(|| missing_field("col_offset", category))
            .and_then(|v| int_field(&v, "col_offset"))?;
        let end_lineno = match field(inst, "end_lineno") {
            Some(Object::None) | None => lineno,
            Some(v) => int_field(&v, "end_lineno")?,
        };
        let end_col = match field(inst, "end_col_offset") {
            Some(Object::None) | None => col,
            Some(v) => int_field(&v, "end_col_offset")?,
        };
        let start = self.pos.byte(lineno, col);
        let end = self.pos.byte(end_lineno, end_col).max(start);
        Ok(Span::new(start, end))
    }

    /// Optional positions (keyword / alias nodes may omit them).
    fn span_opt(&self, inst: &Rc<PyInstance>, fallback: Span) -> Span {
        let int_of = |name: &str| -> Option<i64> {
            match field(inst, name) {
                Some(Object::Int(i)) => Some(i),
                _ => None,
            }
        };
        match (int_of("lineno"), int_of("col_offset")) {
            (Some(l), Some(c)) => {
                let start = self.pos.byte(l, c);
                let end = match (int_of("end_lineno"), int_of("end_col_offset")) {
                    (Some(el), Some(ec)) => self.pos.byte(el, ec).max(start),
                    _ => start,
                };
                Span::new(start, end)
            }
            _ => fallback,
        }
    }

    fn stmt_list(&mut self, obj: &Object, node: &str) -> Result<Vec<past::Stmt>, RuntimeError> {
        list_items(obj, node, "body")?
            .iter()
            .map(|s| match s {
                // CPython's obj2ast lowers a `None` list item to NULL and
                // lets the validator report it; surface the validator's
                // message here since our tree can't carry the hole.
                Object::None => Err(value_error("None disallowed in statement list")),
                s => self.stmt(s),
            })
            .collect()
    }

    fn expr_list(
        &mut self,
        obj: &Object,
        node: &str,
        fieldname: &str,
    ) -> Result<Vec<past::Expr>, RuntimeError> {
        list_items(obj, node, fieldname)?
            .iter()
            .map(|e| match e {
                Object::None => Err(value_error("None disallowed in expression list")),
                e => self.expr(e),
            })
            .collect()
    }

    fn opt_expr(&mut self, obj: Option<Object>) -> Result<Option<past::Expr>, RuntimeError> {
        match obj {
            None | Some(Object::None) => Ok(None),
            Some(v) => Ok(Some(self.expr(&v)?)),
        }
    }

    fn opt_boxed(&mut self, obj: Option<Object>) -> Result<Option<Box<past::Expr>>, RuntimeError> {
        Ok(self.opt_expr(obj)?.map(Box::new))
    }

    fn req(&self, inst: &Rc<PyInstance>, name: &str, node: &str) -> Result<Object, RuntimeError> {
        field(inst, name).ok_or_else(|| missing_field(name, node))
    }

    /// A required *node-valued* field: a missing attribute is a
    /// `TypeError` (as [`Self::req`]), but an explicit `None` is
    /// CPython's obj2ast `ValueError` (`"field 'value' is required for
    /// YieldFrom"` — `test_empty_yield_from` / `test_none_checks`).
    fn req_node(
        &self,
        inst: &Rc<PyInstance>,
        name: &str,
        node: &str,
    ) -> Result<Object, RuntimeError> {
        match field(inst, name) {
            None => Err(missing_field(name, node)),
            Some(Object::None) => Err(value_error(format!(
                "field '{name}' is required for {node}"
            ))),
            Some(v) => Ok(v),
        }
    }

    // ---------------- statements ----------------

    fn stmt(&mut self, obj: &Object) -> Result<past::Stmt, RuntimeError> {
        let (inst, name) = node_name(obj, "stmt")?;
        let span = self.span_of(&inst, "stmt")?;
        let kind = match name.as_str() {
            "FunctionDef" | "AsyncFunctionDef" => {
                let args = self.arguments(&self.req_node(&inst, "args", &name)?)?;
                let body = self.stmt_list(&self.req(&inst, "body", &name)?, &name)?;
                let decorator_list = self.expr_list(
                    &self.req(&inst, "decorator_list", &name)?,
                    &name,
                    "decorator_list",
                )?;
                let returns = self.opt_boxed(field(&inst, "returns"))?;
                let type_params = self.type_params(field(&inst, "type_params"))?;
                let fname = identifier(&self.req_node(&inst, "name", &name)?)?;
                if name == "FunctionDef" {
                    past::StmtKind::FunctionDef {
                        name: fname,
                        args,
                        body,
                        decorator_list,
                        type_params,
                        returns,
                    }
                } else {
                    past::StmtKind::AsyncFunctionDef {
                        name: fname,
                        args,
                        body,
                        decorator_list,
                        type_params,
                        returns,
                    }
                }
            }
            "ClassDef" => past::StmtKind::ClassDef {
                name: identifier(&self.req_node(&inst, "name", "ClassDef")?)?,
                bases: self.expr_list(
                    &self.req(&inst, "bases", "ClassDef")?,
                    "ClassDef",
                    "bases",
                )?,
                keywords: self.keywords(&self.req(&inst, "keywords", "ClassDef")?)?,
                body: self.stmt_list(&self.req(&inst, "body", "ClassDef")?, "ClassDef")?,
                decorator_list: self.expr_list(
                    &self.req(&inst, "decorator_list", "ClassDef")?,
                    "ClassDef",
                    "decorator_list",
                )?,
                type_params: self.type_params(field(&inst, "type_params"))?,
            },
            "Return" => past::StmtKind::Return(self.opt_expr(field(&inst, "value"))?),
            "Delete" => past::StmtKind::Delete(self.expr_list(
                &self.req(&inst, "targets", "Delete")?,
                "Delete",
                "targets",
            )?),
            "Assign" => past::StmtKind::Assign {
                targets: self.expr_list(
                    &self.req(&inst, "targets", "Assign")?,
                    "Assign",
                    "targets",
                )?,
                value: self.expr(&self.req_node(&inst, "value", "Assign")?)?,
            },
            "AugAssign" => past::StmtKind::AugAssign {
                target: self.expr(&self.req_node(&inst, "target", "AugAssign")?)?,
                op: bin_op(&self.req_node(&inst, "op", "AugAssign")?)?,
                value: self.expr(&self.req_node(&inst, "value", "AugAssign")?)?,
            },
            "AnnAssign" => past::StmtKind::AnnAssign {
                target: self.expr(&self.req_node(&inst, "target", "AnnAssign")?)?,
                annotation: self.expr(&self.req_node(&inst, "annotation", "AnnAssign")?)?,
                value: self.opt_expr(field(&inst, "value"))?,
                simple: int_field(&self.req_node(&inst, "simple", "AnnAssign")?, "simple")? != 0,
            },
            "TypeAlias" => {
                let target = self.expr(&self.req_node(&inst, "name", "TypeAlias")?)?;
                let alias_name = match &target.kind {
                    past::ExprKind::Name(n) => n.clone(),
                    // CPython: Python/compile.c `codegen_typealias` message.
                    _ => return Err(type_error("TypeAlias with non-Name name")),
                };
                let value = self.expr(&self.req_node(&inst, "value", "TypeAlias")?)?;
                let type_params = self.type_params(field(&inst, "type_params"))?;
                past::StmtKind::TypeAlias {
                    name: alias_name,
                    name_span: target.span,
                    type_params,
                    value: Box::new(value),
                }
            }
            "For" | "AsyncFor" => {
                let target = self.expr(&self.req_node(&inst, "target", &name)?)?;
                let iter = self.expr(&self.req_node(&inst, "iter", &name)?)?;
                let body = self.stmt_list(&self.req(&inst, "body", &name)?, &name)?;
                let orelse = self.stmt_list(&self.req(&inst, "orelse", &name)?, &name)?;
                if name == "For" {
                    past::StmtKind::For {
                        target,
                        iter,
                        body,
                        orelse,
                    }
                } else {
                    past::StmtKind::AsyncFor {
                        target,
                        iter,
                        body,
                        orelse,
                    }
                }
            }
            "While" => past::StmtKind::While {
                test: self.expr(&self.req_node(&inst, "test", "While")?)?,
                body: self.stmt_list(&self.req(&inst, "body", "While")?, "While")?,
                orelse: self.stmt_list(&self.req(&inst, "orelse", "While")?, "While")?,
            },
            "If" => past::StmtKind::If {
                test: self.expr(&self.req_node(&inst, "test", "If")?)?,
                body: self.stmt_list(&self.req(&inst, "body", "If")?, "If")?,
                orelse: self.stmt_list(&self.req(&inst, "orelse", "If")?, "If")?,
            },
            "With" | "AsyncWith" => {
                let items = self.withitems(&self.req(&inst, "items", &name)?)?;
                let body = self.stmt_list(&self.req(&inst, "body", &name)?, &name)?;
                if name == "With" {
                    past::StmtKind::With { items, body }
                } else {
                    past::StmtKind::AsyncWith { items, body }
                }
            }
            "Match" => past::StmtKind::Match {
                subject: self.expr(&self.req_node(&inst, "subject", "Match")?)?,
                cases: list_items(&self.req(&inst, "cases", "Match")?, "Match", "cases")?
                    .iter()
                    .map(|c| self.match_case(c))
                    .collect::<Result<_, _>>()?,
            },
            "Raise" => past::StmtKind::Raise {
                exc: self.opt_expr(field(&inst, "exc"))?,
                cause: self.opt_expr(field(&inst, "cause"))?,
            },
            "Try" | "TryStar" => {
                let is_star = name == "TryStar";
                let handlers = list_items(&self.req(&inst, "handlers", &name)?, &name, "handlers")?
                    .iter()
                    .map(|h| self.handler(h, is_star))
                    .collect::<Result<_, _>>()?;
                past::StmtKind::Try {
                    body: self.stmt_list(&self.req(&inst, "body", &name)?, &name)?,
                    handlers,
                    orelse: self.stmt_list(&self.req(&inst, "orelse", &name)?, &name)?,
                    finalbody: self.stmt_list(&self.req(&inst, "finalbody", &name)?, &name)?,
                }
            }
            "Assert" => past::StmtKind::Assert {
                test: self.expr(&self.req_node(&inst, "test", "Assert")?)?,
                msg: self.opt_expr(field(&inst, "msg"))?,
            },
            "Import" => past::StmtKind::Import(
                list_items(&self.req(&inst, "names", "Import")?, "Import", "names")?
                    .iter()
                    .map(|o| alias(self, span, o))
                    .collect::<Result<_, _>>()?,
            ),
            "ImportFrom" => past::StmtKind::ImportFrom {
                module: opt_identifier(field(&inst, "module"))?,
                names: list_items(
                    &self.req(&inst, "names", "ImportFrom")?,
                    "ImportFrom",
                    "names",
                )?
                .iter()
                .map(|o| alias(self, span, o))
                .collect::<Result<_, _>>()?,
                level: match field(&inst, "level") {
                    None | Some(Object::None) => 0,
                    Some(v) => int_field(&v, "level")?.max(0) as u32,
                },
            },
            "Global" => past::StmtKind::Global(
                list_items(&self.req(&inst, "names", "Global")?, "Global", "names")?
                    .iter()
                    .map(identifier)
                    .collect::<Result<_, _>>()?,
            ),
            "Nonlocal" => past::StmtKind::Nonlocal(
                list_items(&self.req(&inst, "names", "Nonlocal")?, "Nonlocal", "names")?
                    .iter()
                    .map(identifier)
                    .collect::<Result<_, _>>()?,
            ),
            "Expr" => past::StmtKind::Expr(self.expr(&self.req_node(&inst, "value", "Expr")?)?),
            "Pass" => past::StmtKind::Pass,
            "Break" => past::StmtKind::Break,
            "Continue" => past::StmtKind::Continue,
            _ => {
                return Err(type_error(format!(
                    "expected some sort of stmt, but got {}",
                    instance_repr(&inst)
                )))
            }
        };
        Ok(past::Stmt { kind, span })
    }

    // ---------------- expressions ----------------

    fn expr(&mut self, obj: &Object) -> Result<past::Expr, RuntimeError> {
        let (inst, name) = node_name(obj, "expr")?;
        let span = self.span_of(&inst, "expr")?;
        let kind = match name.as_str() {
            "Constant" => {
                let value = self.req(&inst, "value", "Constant")?;
                past::ExprKind::Constant(constant_value(&value)?)
            }
            "Name" => past::ExprKind::Name(identifier(&self.req_node(&inst, "id", "Name")?)?),
            "Attribute" => past::ExprKind::Attribute {
                value: Box::new(self.expr(&self.req_node(&inst, "value", "Attribute")?)?),
                attr: identifier(&self.req_node(&inst, "attr", "Attribute")?)?,
            },
            "Subscript" => past::ExprKind::Subscript {
                value: Box::new(self.expr(&self.req_node(&inst, "value", "Subscript")?)?),
                slice: Box::new(self.expr(&self.req_node(&inst, "slice", "Subscript")?)?),
            },
            "Slice" => past::ExprKind::Slice {
                lower: self.opt_boxed(field(&inst, "lower"))?,
                upper: self.opt_boxed(field(&inst, "upper"))?,
                step: self.opt_boxed(field(&inst, "step"))?,
            },
            "BinOp" => past::ExprKind::BinOp {
                left: Box::new(self.expr(&self.req_node(&inst, "left", "BinOp")?)?),
                op: bin_op(&self.req_node(&inst, "op", "BinOp")?)?,
                right: Box::new(self.expr(&self.req_node(&inst, "right", "BinOp")?)?),
            },
            "BoolOp" => past::ExprKind::BoolOp {
                op: bool_op(&self.req_node(&inst, "op", "BoolOp")?)?,
                values: self.expr_list(
                    &self.req(&inst, "values", "BoolOp")?,
                    "BoolOp",
                    "values",
                )?,
            },
            "UnaryOp" => past::ExprKind::UnaryOp {
                op: unary_op(&self.req_node(&inst, "op", "UnaryOp")?)?,
                operand: Box::new(self.expr(&self.req_node(&inst, "operand", "UnaryOp")?)?),
            },
            "Compare" => {
                let ops: Vec<_> =
                    list_items(&self.req(&inst, "ops", "Compare")?, "Compare", "ops")?
                        .iter()
                        .map(cmp_op)
                        .collect::<Result<_, _>>()?;
                let comparators = self.expr_list(
                    &self.req(&inst, "comparators", "Compare")?,
                    "Compare",
                    "comparators",
                )?;
                // CPython's validate_expr rejects these shapes before
                // compilation; the compiler assumes ops/comparators pair up.
                if comparators.is_empty() {
                    return Err(value_error("Compare with no comparators".to_owned()));
                }
                if ops.len() != comparators.len() {
                    return Err(value_error(
                        "Compare has a different number of comparators and operands".to_owned(),
                    ));
                }
                past::ExprKind::Compare {
                    left: Box::new(self.expr(&self.req_node(&inst, "left", "Compare")?)?),
                    ops,
                    comparators,
                }
            }
            "IfExp" => past::ExprKind::IfExp {
                test: Box::new(self.expr(&self.req_node(&inst, "test", "IfExp")?)?),
                body: Box::new(self.expr(&self.req_node(&inst, "body", "IfExp")?)?),
                orelse: Box::new(self.expr(&self.req_node(&inst, "orelse", "IfExp")?)?),
            },
            "NamedExpr" => {
                let target_obj = self.req_node(&inst, "target", "NamedExpr")?;
                // CPython's `validate_expr` rejects this before
                // compilation (gh-109351).
                if !matches!(node_name(&target_obj, "expression"), Ok((_, n)) if n == "Name") {
                    return Err(type_error("NamedExpr target must be a Name"));
                }
                past::ExprKind::NamedExpr {
                    target: Box::new(self.expr(&target_obj)?),
                    value: Box::new(self.expr(&self.req_node(&inst, "value", "NamedExpr")?)?),
                }
            }
            "Lambda" => past::ExprKind::Lambda {
                args: self.arguments(&self.req_node(&inst, "args", "Lambda")?)?,
                body: Box::new(self.expr(&self.req_node(&inst, "body", "Lambda")?)?),
            },
            "Call" => past::ExprKind::Call {
                func: Box::new(self.expr(&self.req_node(&inst, "func", "Call")?)?),
                args: self.expr_list(&self.req(&inst, "args", "Call")?, "Call", "args")?,
                keywords: self.keywords(&self.req(&inst, "keywords", "Call")?)?,
            },
            "Tuple" => past::ExprKind::Tuple(self.expr_list(
                &self.req(&inst, "elts", "Tuple")?,
                "Tuple",
                "elts",
            )?),
            "List" => past::ExprKind::List(self.expr_list(
                &self.req(&inst, "elts", "List")?,
                "List",
                "elts",
            )?),
            "Set" => past::ExprKind::Set(self.expr_list(
                &self.req(&inst, "elts", "Set")?,
                "Set",
                "elts",
            )?),
            "Dict" => {
                let keys = list_items(&self.req(&inst, "keys", "Dict")?, "Dict", "keys")?
                    .iter()
                    .map(|k| match k {
                        Object::None => Ok(None),
                        other => Ok(Some(self.expr(other)?)),
                    })
                    .collect::<Result<Vec<_>, RuntimeError>>()?;
                past::ExprKind::Dict {
                    keys,
                    values: self.expr_list(
                        &self.req(&inst, "values", "Dict")?,
                        "Dict",
                        "values",
                    )?,
                }
            }
            "ListComp" | "SetComp" | "GeneratorExp" => {
                let elt = Box::new(self.expr(&self.req_node(&inst, "elt", &name)?)?);
                let generators =
                    self.comprehensions(&self.req(&inst, "generators", &name)?, &name)?;
                match name.as_str() {
                    "ListComp" => past::ExprKind::ListComp { elt, generators },
                    "SetComp" => past::ExprKind::SetComp { elt, generators },
                    _ => past::ExprKind::GeneratorExp { elt, generators },
                }
            }
            "DictComp" => past::ExprKind::DictComp {
                key: Box::new(self.expr(&self.req_node(&inst, "key", "DictComp")?)?),
                value: Box::new(self.expr(&self.req_node(&inst, "value", "DictComp")?)?),
                generators: self
                    .comprehensions(&self.req(&inst, "generators", "DictComp")?, "DictComp")?,
            },
            "Starred" => past::ExprKind::Starred(Box::new(
                self.expr(&self.req_node(&inst, "value", "Starred")?)?,
            )),
            "Yield" => past::ExprKind::Yield(self.opt_boxed(field(&inst, "value"))?),
            "YieldFrom" => past::ExprKind::YieldFrom(Box::new(self.expr(&self.req_node(
                &inst,
                "value",
                "YieldFrom",
            )?)?)),
            "Await" => past::ExprKind::Await(Box::new(
                self.expr(&self.req_node(&inst, "value", "Await")?)?,
            )),
            "JoinedStr" => past::ExprKind::JoinedStr(self.expr_list(
                &self.req(&inst, "values", "JoinedStr")?,
                "JoinedStr",
                "values",
            )?),
            "FormattedValue" => past::ExprKind::FormattedValue {
                value: Box::new(self.expr(&self.req_node(&inst, "value", "FormattedValue")?)?),
                conversion: match field(&inst, "conversion") {
                    None | Some(Object::None) => -1,
                    Some(v) => int_field(&v, "conversion")? as i32,
                },
                format_spec: self.opt_boxed(field(&inst, "format_spec"))?,
            },
            _ => {
                return Err(type_error(format!(
                    "expected some sort of expr, but got {}",
                    instance_repr(&inst)
                )))
            }
        };
        Ok(past::Expr { kind, span })
    }

    // ---------------- supporting nodes ----------------

    fn arguments(&mut self, obj: &Object) -> Result<past::Arguments, RuntimeError> {
        let (inst, _name) = node_name(obj, "arguments")?;
        let args_of = |conv: &mut Self, fieldname: &str| -> Result<Vec<past::Arg>, RuntimeError> {
            match field(&inst, fieldname) {
                None | Some(Object::None) => Ok(Vec::new()),
                Some(v) => list_items(&v, "arguments", fieldname)?
                    .iter()
                    .map(|a| conv.arg(a))
                    .collect(),
            }
        };
        let posonlyargs = args_of(self, "posonlyargs")?;
        let args = args_of(self, "args")?;
        let kwonlyargs = args_of(self, "kwonlyargs")?;
        let vararg = match field(&inst, "vararg") {
            None | Some(Object::None) => None,
            Some(v) => Some(self.arg(&v)?),
        };
        let kwarg = match field(&inst, "kwarg") {
            None | Some(Object::None) => None,
            Some(v) => Some(self.arg(&v)?),
        };
        let kw_defaults = match field(&inst, "kw_defaults") {
            None | Some(Object::None) => Vec::new(),
            Some(v) => list_items(&v, "arguments", "kw_defaults")?
                .iter()
                .map(|d| match d {
                    Object::None => Ok(None),
                    other => Ok(Some(self.expr(other)?)),
                })
                .collect::<Result<Vec<_>, RuntimeError>>()?,
        };
        let defaults = match field(&inst, "defaults") {
            None | Some(Object::None) => Vec::new(),
            Some(v) => list_items(&v, "arguments", "defaults")?
                .iter()
                .map(|e| match e {
                    Object::None => Err(value_error("None disallowed in expression list")),
                    e => self.expr(e),
                })
                .collect::<Result<Vec<_>, RuntimeError>>()?,
        };
        Ok(past::Arguments {
            posonlyargs,
            args,
            vararg,
            kwonlyargs,
            kw_defaults,
            kwarg,
            defaults,
        })
    }

    fn arg(&mut self, obj: &Object) -> Result<past::Arg, RuntimeError> {
        let (inst, _name) = node_name(obj, "arg")?;
        let span = self.span_of(&inst, "arg")?;
        Ok(past::Arg {
            name: identifier(&self.req_node(&inst, "arg", "arg")?)?,
            annotation: self.opt_boxed(field(&inst, "annotation"))?,
            span,
        })
    }

    fn keywords(&mut self, obj: &Object) -> Result<Vec<past::Keyword>, RuntimeError> {
        list_items(obj, "Call", "keywords")?
            .iter()
            .map(|k| {
                let (inst, _name) = node_name(k, "keyword")?;
                let span = self.span_of(&inst, "keyword")?;
                Ok(past::Keyword {
                    arg: opt_identifier(field(&inst, "arg"))?,
                    value: self.expr(&self.req_node(&inst, "value", "keyword")?)?,
                    span,
                })
            })
            .collect()
    }

    fn comprehensions(
        &mut self,
        obj: &Object,
        node: &str,
    ) -> Result<Vec<past::Comprehension>, RuntimeError> {
        list_items(obj, node, "generators")?
            .iter()
            .map(|c| {
                let (inst, _name) = node_name(c, "comprehension")?;
                Ok(past::Comprehension {
                    target: self.expr(&self.req_node(&inst, "target", "comprehension")?)?,
                    iter: self.expr(&self.req_node(&inst, "iter", "comprehension")?)?,
                    ifs: self.expr_list(
                        &self.req(&inst, "ifs", "comprehension")?,
                        "comprehension",
                        "ifs",
                    )?,
                    is_async: match field(&inst, "is_async") {
                        None | Some(Object::None) => false,
                        Some(v) => int_field(&v, "is_async")? != 0,
                    },
                })
            })
            .collect()
    }

    fn withitems(&mut self, obj: &Object) -> Result<Vec<past::WithItem>, RuntimeError> {
        list_items(obj, "With", "items")?
            .iter()
            .map(|w| {
                let (inst, _name) = node_name(w, "withitem")?;
                Ok(past::WithItem {
                    context_expr: self.expr(&self.req_node(
                        &inst,
                        "context_expr",
                        "withitem",
                    )?)?,
                    optional_vars: self.opt_expr(field(&inst, "optional_vars"))?,
                })
            })
            .collect()
    }

    fn handler(
        &mut self,
        obj: &Object,
        is_star: bool,
    ) -> Result<past::ExceptHandler, RuntimeError> {
        let (inst, _name) = node_name(obj, "excepthandler")?;
        let span = self.span_of(&inst, "excepthandler")?;
        Ok(past::ExceptHandler {
            type_: self.opt_expr(field(&inst, "type"))?,
            name: opt_identifier(field(&inst, "name"))?,
            body: self.stmt_list(&self.req(&inst, "body", "ExceptHandler")?, "ExceptHandler")?,
            span,
            is_star,
        })
    }

    fn match_case(&mut self, obj: &Object) -> Result<past::MatchCase, RuntimeError> {
        let (inst, _name) = node_name(obj, "match_case")?;
        let pattern_obj = self.req_node(&inst, "pattern", "match_case")?;
        let pattern = self.pattern(&pattern_obj)?;
        // `match_case` carries no positions in CPython; fall back to
        // the pattern node's span.
        let span = match &pattern_obj {
            Object::Instance(pinst) => self.span_opt(pinst, Span::new(0, 0)),
            _ => Span::new(0, 0),
        };
        Ok(past::MatchCase {
            pattern,
            guard: self.opt_expr(field(&inst, "guard"))?,
            body: self.stmt_list(&self.req(&inst, "body", "match_case")?, "match_case")?,
            span,
        })
    }

    fn pattern(&mut self, obj: &Object) -> Result<past::Pattern, RuntimeError> {
        use past::PatternKind;
        let (inst, name) = node_name(obj, "pattern")?;
        let span = self.span_opt(&inst, Span::new(0, 0));
        let kind = match name.as_str() {
            "MatchValue" => {
                PatternKind::Value(self.expr(&self.req_node(&inst, "value", "MatchValue")?)?)
            }
            "MatchSingleton" => PatternKind::Singleton(constant_value(&self.req(
                &inst,
                "value",
                "MatchSingleton",
            )?)?),
            "MatchSequence" => PatternKind::Sequence(
                list_items(
                    &self.req(&inst, "patterns", "MatchSequence")?,
                    "MatchSequence",
                    "patterns",
                )?
                .iter()
                .map(|p| self.pattern(p))
                .collect::<Result<_, _>>()?,
            ),
            "MatchStar" => PatternKind::Star(opt_identifier(field(&inst, "name"))?),
            "MatchMapping" => PatternKind::Mapping {
                keys: self.expr_list(
                    &self.req(&inst, "keys", "MatchMapping")?,
                    "MatchMapping",
                    "keys",
                )?,
                patterns: list_items(
                    &self.req(&inst, "patterns", "MatchMapping")?,
                    "MatchMapping",
                    "patterns",
                )?
                .iter()
                .map(|p| self.pattern(p))
                .collect::<Result<_, _>>()?,
                rest: match opt_identifier(field(&inst, "rest"))? {
                    Some(n) => Some(Some(n)),
                    None => None,
                },
            },
            "MatchClass" => {
                let kwd_attrs = list_items(
                    &self.req(&inst, "kwd_attrs", "MatchClass")?,
                    "MatchClass",
                    "kwd_attrs",
                )?
                .iter()
                .map(identifier)
                .collect::<Result<Vec<_>, _>>()?;
                let kwd_patterns = list_items(
                    &self.req(&inst, "kwd_patterns", "MatchClass")?,
                    "MatchClass",
                    "kwd_patterns",
                )?
                .iter()
                .map(|p| self.pattern(p))
                .collect::<Result<Vec<_>, _>>()?;
                if kwd_attrs.len() != kwd_patterns.len() {
                    return Err(value_error(
                        "MatchClass doesn't have the same number of keyword attributes as patterns",
                    ));
                }
                PatternKind::Class {
                    cls: self.expr(&self.req_node(&inst, "cls", "MatchClass")?)?,
                    positionals: list_items(
                        &self.req(&inst, "patterns", "MatchClass")?,
                        "MatchClass",
                        "patterns",
                    )?
                    .iter()
                    .map(|p| self.pattern(p))
                    .collect::<Result<_, _>>()?,
                    keywords: kwd_attrs.into_iter().zip(kwd_patterns).collect(),
                }
            }
            "MatchOr" => PatternKind::Or(
                list_items(
                    &self.req(&inst, "patterns", "MatchOr")?,
                    "MatchOr",
                    "patterns",
                )?
                .iter()
                .map(|p| self.pattern(p))
                .collect::<Result<_, _>>()?,
            ),
            "MatchAs" => {
                let sub = match field(&inst, "pattern") {
                    None | Some(Object::None) => None,
                    Some(p) => Some(self.pattern(&p)?),
                };
                let capture = opt_identifier(field(&inst, "name"))?;
                match (sub, capture) {
                    (None, n) => PatternKind::Capture(n),
                    (Some(p), Some(n)) => PatternKind::As {
                        pattern: Box::new(p),
                        name: n,
                    },
                    (Some(_), None) => {
                        return Err(value_error(
                            "MatchAs must specify a target name if a pattern is given",
                        ))
                    }
                }
            }
            other => {
                return Err(type_error(format!(
                    "expected some sort of pattern, but got <{other} object>"
                )))
            }
        };
        Ok(past::Pattern { kind, span })
    }

    fn type_params(&mut self, obj: Option<Object>) -> Result<Vec<past::TypeParam>, RuntimeError> {
        let list = match obj {
            None | Some(Object::None) => return Ok(Vec::new()),
            Some(v) => list_items(&v, "type_params", "type_params")?,
        };
        list.iter()
            .map(|tp| {
                let (inst, name) = node_name(tp, "type_param")?;
                let span = self.span_opt(&inst, Span::new(0, 0));
                let pname = identifier(&self.req_node(&inst, "name", &name)?)?;
                let default = self.opt_boxed(field(&inst, "default_value"))?;
                let kind = match name.as_str() {
                    "TypeVar" => past::TypeParamKind::TypeVar {
                        bound: self.opt_boxed(field(&inst, "bound"))?,
                    },
                    "TypeVarTuple" => past::TypeParamKind::TypeVarTuple,
                    "ParamSpec" => past::TypeParamKind::ParamSpec,
                    other => {
                        return Err(type_error(format!(
                            "expected some sort of type_param, but got <{other} object>"
                        )))
                    }
                };
                Ok(past::TypeParam {
                    name: pname.clone(),
                    source_name: pname,
                    kind,
                    default,
                    span,
                })
            })
            .collect()
    }
}

// ---------------------------------------------------------------------------
// Operators and constants
// ---------------------------------------------------------------------------

fn op_name(obj: &Object, category: &str) -> Result<String, RuntimeError> {
    match obj {
        Object::Instance(inst) => Ok(inst.cls().name.clone()),
        other => Err(type_error(format!(
            "expected some sort of {category}, but got {}",
            repr_lite(other)
        ))),
    }
}

fn bin_op(obj: &Object) -> Result<past::BinOp, RuntimeError> {
    Ok(match op_name(obj, "operator")?.as_str() {
        "Add" => past::BinOp::Add,
        "Sub" => past::BinOp::Sub,
        "Mult" => past::BinOp::Mult,
        "MatMult" => past::BinOp::MatMult,
        "Div" => past::BinOp::Div,
        "Mod" => past::BinOp::Mod,
        "Pow" => past::BinOp::Pow,
        "LShift" => past::BinOp::LShift,
        "RShift" => past::BinOp::RShift,
        "BitOr" => past::BinOp::BitOr,
        "BitXor" => past::BinOp::BitXor,
        "BitAnd" => past::BinOp::BitAnd,
        "FloorDiv" => past::BinOp::FloorDiv,
        other => {
            return Err(type_error(format!(
                "expected some sort of operator, but got <{other} object>"
            )))
        }
    })
}

fn bool_op(obj: &Object) -> Result<past::BoolOp, RuntimeError> {
    Ok(match op_name(obj, "boolop")?.as_str() {
        "And" => past::BoolOp::And,
        "Or" => past::BoolOp::Or,
        other => {
            return Err(type_error(format!(
                "expected some sort of boolop, but got <{other} object>"
            )))
        }
    })
}

fn unary_op(obj: &Object) -> Result<past::UnaryOp, RuntimeError> {
    Ok(match op_name(obj, "unaryop")?.as_str() {
        "Invert" => past::UnaryOp::Invert,
        "Not" => past::UnaryOp::Not,
        "UAdd" => past::UnaryOp::UAdd,
        "USub" => past::UnaryOp::USub,
        other => {
            return Err(type_error(format!(
                "expected some sort of unaryop, but got <{other} object>"
            )))
        }
    })
}

fn cmp_op(obj: &Object) -> Result<past::CmpOp, RuntimeError> {
    Ok(match op_name(obj, "cmpop")?.as_str() {
        "Eq" => past::CmpOp::Eq,
        "NotEq" => past::CmpOp::NotEq,
        "Lt" => past::CmpOp::Lt,
        "LtE" => past::CmpOp::LtE,
        "Gt" => past::CmpOp::Gt,
        "GtE" => past::CmpOp::GtE,
        "Is" => past::CmpOp::Is,
        "IsNot" => past::CmpOp::IsNot,
        "In" => past::CmpOp::In,
        "NotIn" => past::CmpOp::NotIn,
        other => {
            return Err(type_error(format!(
                "expected some sort of cmpop, but got <{other} object>"
            )))
        }
    })
}

fn alias(conv: &Conv<'_>, fallback: Span, obj: &Object) -> Result<past::Alias, RuntimeError> {
    let (inst, _name) = node_name(obj, "alias")?;
    let name = match field(&inst, "name") {
        None => return Err(missing_field("name", "alias")),
        Some(Object::None) => return Err(value_error("field 'name' is required for alias")),
        Some(v) => identifier(&v)?,
    };
    Ok(past::Alias {
        name,
        asname: opt_identifier(field(&inst, "asname"))?,
        span: conv.span_opt(&inst, fallback),
    })
}

/// Lower a `Constant.value` runtime object back into a parser
/// constant. Mirrors CPython's compiler validation: only genuinely
/// constant types are admitted.
fn constant_value(obj: &Object) -> Result<past::Constant, RuntimeError> {
    Ok(match obj {
        Object::None => past::Constant::None,
        Object::Bool(b) => past::Constant::Bool(*b),
        Object::Int(i) => past::Constant::Int(*i),
        Object::Long(b) => past::Constant::BigInt(b.to_string()),
        Object::Float(f) => past::Constant::Float(*f),
        Object::Complex(c) => past::Constant::Complex(c.real, c.imag),
        Object::Str(s) => past::Constant::Str(s.to_string()),
        Object::WStr(cps) => past::Constant::WStr(cps.to_vec()),
        Object::Bytes(b) => past::Constant::Bytes(b.to_vec()),
        Object::FrozenSet(s) => past::Constant::FrozenSet(
            s.iter()
                .map(|k| constant_value(&k.0))
                .collect::<Result<_, _>>()?,
        ),
        Object::Tuple(items) => {
            past::Constant::Tuple(items.iter().map(constant_value).collect::<Result<_, _>>()?)
        }
        other => {
            if crate::vm_singletons::is_ellipsis(other) {
                past::Constant::Ellipsis
            } else {
                // CPython's validate_constant raises TypeError here.
                return Err(type_error(format!(
                    "got an invalid type in Constant: {}",
                    other.type_name()
                )));
            }
        }
    })
}

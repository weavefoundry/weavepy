//! `_symtable` — the native scope-analysis core behind the frozen
//! `symtable` module (RFC 0033).
//!
//! CPython's `_symtable` is a C extension that runs the compiler's
//! symbol-table pass and hands the resulting block tree back to the
//! pure-Python `symtable.py` wrapper. WeavePy mirrors that split: this
//! module re-implements CPython 3.13's two-phase analysis
//! (`Python/symtable.c`) over WeavePy's own parser AST and returns the
//! raw block tree as ordinary Python values (a nested `dict`), which
//! `stdlib/python/symtable.py` then wraps in `SymbolTable`/`Symbol`.
//!
//! Phase 1 ([`Builder`]) walks the AST, entering a block per
//! module/function/class/lambda/generator-expression and recording the
//! `DEF_*`/`USE` flags for every name. Phase 2 ([`Analyzer`]) resolves
//! each name's scope (`LOCAL`/`CELL`/`FREE`/`GLOBAL_*`) using the same
//! free-variable propagation CPython performs, and folds the scope into
//! the high bits of each symbol's flag word.
//!
//! Comprehensions follow PEP 709: list/set/dict comprehensions are
//! *inlined* into the enclosing block (no child scope), while generator
//! expressions still get their own `genexpr` block with a `.0` argument.

use crate::sync::Rc;
use crate::sync::RefCell;

use indexmap::IndexMap;
use std::collections::{HashMap, HashSet};

use weavepy_lexer::token::Span;
use weavepy_parser::ast as past;

use crate::error::{type_error, value_error, RuntimeError};
use crate::import::ModuleCache;
use crate::object::{BuiltinFn, DictData, DictKey, Object, PyModule};

// ---- symbol flag bits (CPython 3.13 `pycore_symtable.h`) ----
const DEF_GLOBAL: i64 = 1;
const DEF_LOCAL: i64 = 2;
const DEF_PARAM: i64 = 4;
const DEF_NONLOCAL: i64 = 8;
const USE: i64 = 16;
const DEF_FREE_CLASS: i64 = 64;
const DEF_IMPORT: i64 = 128;
const DEF_ANNOT: i64 = 256;
/// CPython `DEF_TYPE_PARAM` (`2<<9`): the name is a PEP 695 type
/// parameter.
const DEF_TYPE_PARAM: i64 = 2 << 9;
const DEF_BOUND: i64 = DEF_LOCAL | DEF_PARAM | DEF_IMPORT; // 134

const SCOPE_OFF: i64 = 12;
#[allow(dead_code)]
const SCOPE_MASK: i64 = 15;

// ---- scopes ----
const LOCAL: i64 = 1;
const GLOBAL_EXPLICIT: i64 = 2;
const GLOBAL_IMPLICIT: i64 = 3;
const FREE: i64 = 4;
const CELL: i64 = 5;

// ---- block types ----
const TYPE_FUNCTION: i64 = 0;
const TYPE_CLASS: i64 = 1;
const TYPE_MODULE: i64 = 2;
const TYPE_ANNOTATION: i64 = 3;
const TYPE_TYPE_ALIAS: i64 = 4;
const TYPE_TYPE_PARAMETERS: i64 = 5;
const TYPE_TYPE_VARIABLE: i64 = 6;

#[derive(Clone, Copy, PartialEq, Eq)]
enum BlockType {
    Function,
    Class,
    Module,
    /// PEP 695 `type X = …` value scope (CPython `TypeAliasBlock`).
    TypeAlias,
    /// PEP 695 hidden `[T, …]` scope wrapping a generic
    /// function/class/alias (CPython `TypeParametersBlock`).
    TypeParameters,
    /// PEP 695 scope for one type parameter's bound/constraints/
    /// default expression (CPython `TypeVariableBlock`).
    TypeVariable,
}

impl BlockType {
    /// CPython `_PyST_IsFunctionLike`: the PEP 695 annotation scopes
    /// resolve names like function scopes do.
    fn is_function_like(self) -> bool {
        matches!(
            self,
            BlockType::Function
                | BlockType::TypeAlias
                | BlockType::TypeParameters
                | BlockType::TypeVariable
        )
    }
    fn cpython(self) -> i64 {
        match self {
            BlockType::Function => TYPE_FUNCTION,
            BlockType::Class => TYPE_CLASS,
            BlockType::Module => TYPE_MODULE,
            BlockType::TypeAlias => TYPE_TYPE_ALIAS,
            BlockType::TypeParameters => TYPE_TYPE_PARAMETERS,
            BlockType::TypeVariable => TYPE_TYPE_VARIABLE,
        }
    }
}

struct Block {
    ty: BlockType,
    name: String,
    lineno: i64,
    nested: bool,
    /// CPython `ste_can_see_class_scope`: a PEP 695 annotation scope
    /// immediately inside a class body closes over `__classdict__`.
    can_see_class_scope: bool,
    /// name → accumulated flag word (def bits during phase 1; the scope
    /// is OR'd into the high bits during phase 2).
    symbols: IndexMap<String, i64>,
    /// parameter names in declaration order (plus `.0` for genexprs).
    varnames: Vec<String>,
    children: Vec<usize>,
    id: i64,
}

pub fn build(_cache: &ModuleCache) -> Rc<PyModule> {
    let dict = Rc::new(RefCell::new(DictData::default()));
    {
        let mut d = dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("__name__")),
            Object::from_static("_symtable"),
        );
        d.insert(
            DictKey(Object::from_static("__doc__")),
            Object::from_static("WeavePy native symbol-table core (RFC 0033)."),
        );
        let consts: &[(&str, i64)] = &[
            ("USE", USE),
            ("DEF_GLOBAL", DEF_GLOBAL),
            ("DEF_NONLOCAL", DEF_NONLOCAL),
            ("DEF_LOCAL", DEF_LOCAL),
            ("DEF_PARAM", DEF_PARAM),
            ("DEF_IMPORT", DEF_IMPORT),
            ("DEF_BOUND", DEF_BOUND),
            ("DEF_ANNOT", DEF_ANNOT),
            ("DEF_FREE_CLASS", DEF_FREE_CLASS),
            ("SCOPE_OFF", SCOPE_OFF),
            ("SCOPE_MASK", SCOPE_MASK),
            ("LOCAL", LOCAL),
            ("GLOBAL_EXPLICIT", GLOBAL_EXPLICIT),
            ("GLOBAL_IMPLICIT", GLOBAL_IMPLICIT),
            ("FREE", FREE),
            ("CELL", CELL),
            ("TYPE_FUNCTION", TYPE_FUNCTION),
            ("TYPE_CLASS", TYPE_CLASS),
            ("TYPE_MODULE", TYPE_MODULE),
            ("TYPE_ANNOTATION", TYPE_ANNOTATION),
            ("TYPE_TYPE_ALIAS", TYPE_TYPE_ALIAS),
            ("TYPE_TYPE_PARAMETERS", TYPE_TYPE_PARAMETERS),
            ("TYPE_TYPE_VARIABLE", TYPE_TYPE_VARIABLE),
        ];
        for (k, v) in consts {
            d.insert(DictKey(Object::from_str(*k)), Object::Int(*v));
        }
        let bf = BuiltinFn {
            name: "symtable",
            binds_instance: false,
            call: Box::new(symtable),
            call_kw: None,
        };
        d.insert(
            DictKey(Object::from_static("symtable")),
            Object::Builtin(Rc::new(bf)),
        );
    }
    Rc::new(PyModule {
        name: "_symtable".to_owned(),
        filename: None,
        dict,
    })
}

/// `_symtable.symtable(source, filename, compile_type)` → raw block tree.
///
/// Argument conversion mirrors CPython's `_symtablemodule.c` clinic
/// order: `filename` goes through `PyUnicode_FSDecoder` (str/bytes),
/// then `compile_type` must be a `str` naming a compile mode. Parse
/// errors and symtable-build-time errors surface as `SyntaxError`s
/// carrying `filename`/`lineno`/`offset`/`text` like CPython's.
pub fn symtable(args: &[Object]) -> Result<Object, RuntimeError> {
    // `filename` — CPython's FSDecoder: str, bytes, or os.PathLike.
    // (PathLike needs a VM re-entry for `__fspath__`; str/bytes covers
    // every real caller, and everything else is the same TypeError.)
    let filename = match args.get(1) {
        Some(Object::Str(s)) => s.to_string(),
        Some(Object::Bytes(b)) => String::from_utf8_lossy(b).into_owned(),
        Some(other) => {
            return Err(type_error(format!(
                "symtable() argument 'filename' must be str, bytes or os.PathLike, not {}",
                other.type_name()
            )))
        }
        None => return Err(type_error("symtable expected 3 arguments")),
    };
    let compile_type = match args.get(2) {
        Some(Object::Str(s)) => s.to_string(),
        Some(other) => {
            return Err(type_error(format!(
                "symtable() argument 'compile_type' must be str, not {}",
                other.type_name()
            )))
        }
        None => return Err(type_error("symtable expected 3 arguments")),
    };
    if !matches!(compile_type.as_str(), "exec" | "eval" | "single") {
        return Err(value_error(
            "symtable() arg 3 must be 'exec' or 'eval' or 'single'",
        ));
    }
    let source = match args.first() {
        Some(Object::Str(s)) => s.to_string(),
        // Bytes decode per PEP 263 (BOM + `# -*- coding: … -*-`).
        Some(Object::Bytes(b)) => crate::decode_compile_source_bytes(b, &filename)?,
        _ => {
            return Err(type_error(
                "symtable() argument 'source' must be str or bytes",
            ))
        }
    };
    let module = weavepy_parser::parse_module(&source)
        .map_err(|e| crate::parse_error_to_syntax_error(&e, &source, &filename))?;
    // CPython's symtable build raises `SyntaxError` for directive
    // conflicts (`global` vs. parameter, …) — run the compiler's
    // validation pass (no codegen) to surface the same diagnostics.
    weavepy_compiler::validate_module_only(&module, &source)
        .map_err(|e| crate::compile_error_to_syntax_error(&e, &source, &filename))?;

    let mut b = Builder::new(&source);
    let root = b.run(&module);
    let mut analyzer = Analyzer {
        arena: &mut b.arena,
    };
    analyzer.analyze(root);
    Ok(to_object(&b.arena, root))
}

// ---------------------------------------------------------------------------
// Phase 1 — build the block tree and record DEF_*/USE flags.
// ---------------------------------------------------------------------------

struct Builder {
    arena: Vec<Block>,
    stack: Vec<usize>,
    newlines: Vec<usize>,
    next_id: i64,
}

impl Builder {
    fn new(source: &str) -> Self {
        let newlines = source
            .bytes()
            .enumerate()
            .filter_map(|(i, c)| (c == b'\n').then_some(i))
            .collect();
        Self {
            arena: Vec::new(),
            stack: Vec::new(),
            newlines,
            next_id: 0,
        }
    }

    fn lineno(&self, span: Span) -> i64 {
        let byte = span.start.0 as usize;
        (self.newlines.partition_point(|&nl| nl < byte) as i64) + 1
    }

    fn run(&mut self, m: &past::Module) -> usize {
        let root = self.enter(BlockType::Module, "top", 0);
        for s in &m.body {
            self.visit_stmt(s);
        }
        self.exit();
        root
    }

    fn cur(&self) -> usize {
        *self.stack.last().expect("block stack underflow")
    }

    fn enter(&mut self, ty: BlockType, name: &str, lineno: i64) -> usize {
        let nested = self
            .stack
            .last()
            .map(|&p| self.arena[p].ty.is_function_like() || self.arena[p].nested)
            .unwrap_or(false);
        let idx = self.arena.len();
        self.next_id += 1;
        self.arena.push(Block {
            ty,
            name: name.to_owned(),
            lineno,
            nested,
            can_see_class_scope: false,
            symbols: IndexMap::new(),
            varnames: Vec::new(),
            children: Vec::new(),
            id: self.next_id,
        });
        if let Some(&parent) = self.stack.last() {
            self.arena[parent].children.push(idx);
        }
        self.stack.push(idx);
        idx
    }

    fn exit(&mut self) {
        self.stack.pop();
    }

    fn add_def(&mut self, name: &str, flag: i64) {
        let cur = self.cur();
        let entry = self.arena[cur].symbols.entry(name.to_owned()).or_insert(0);
        *entry |= flag;
    }

    /// Mirror a flag into the module (root) block. CPython records every
    /// `DEF_GLOBAL` in `st_global` so a `global X` anywhere surfaces `X`
    /// as `declared_global` in the top-level table.
    fn add_def_root(&mut self, name: &str, flag: i64) {
        let entry = self.arena[0].symbols.entry(name.to_owned()).or_insert(0);
        *entry |= flag;
    }

    fn add_param(&mut self, name: &str) {
        self.add_def(name, DEF_PARAM);
        let cur = self.cur();
        if !self.arena[cur].varnames.iter().any(|v| v == name) {
            self.arena[cur].varnames.push(name.to_owned());
        }
    }

    fn add_params(&mut self, args: &past::Arguments) {
        // CPython's `symtable_visit_arguments` registers params in the order
        // posonly, args, kwonly, vararg, kwarg — which is the order
        // `get_parameters()` (identifier order) reports them.
        for a in &args.posonlyargs {
            self.add_param(&a.name);
        }
        for a in &args.args {
            self.add_param(&a.name);
        }
        for a in &args.kwonlyargs {
            self.add_param(&a.name);
        }
        if let Some(a) = &args.vararg {
            self.add_param(&a.name);
        }
        if let Some(a) = &args.kwarg {
            self.add_param(&a.name);
        }
    }

    /// Visit parameter defaults in the *enclosing* scope (CPython
    /// evaluates them where the `def`/`lambda` appears).
    fn visit_defaults(&mut self, args: &past::Arguments) {
        for d in &args.defaults {
            self.visit_expr(d);
        }
        for d in args.kw_defaults.iter().flatten() {
            self.visit_expr(d);
        }
    }

    /// Visit parameter and return annotations. For a generic `def`
    /// the caller enters the hidden type-parameters block first, so
    /// these resolve in that annotation scope (CPython
    /// `symtable_visit_annotations`).
    fn visit_annotations(&mut self, args: &past::Arguments, returns: Option<&past::Expr>) {
        let all = args
            .posonlyargs
            .iter()
            .chain(&args.args)
            .chain(args.vararg.iter())
            .chain(&args.kwonlyargs)
            .chain(args.kwarg.iter());
        for a in all {
            if let Some(ann) = &a.annotation {
                self.visit_expr(ann);
            }
        }
        if let Some(r) = returns {
            self.visit_expr(r);
        }
    }

    /// CPython `symtable_enter_type_param_block`: open the hidden
    /// `TypeParametersBlock` wrapping a generic `def`/`class`/`type`
    /// statement.
    fn enter_type_param_block(
        &mut self,
        name: &str,
        lineno: i64,
        is_class_def: bool,
        has_defaults: bool,
        has_kwdefaults: bool,
    ) {
        let parent_is_class = self.arena[self.cur()].ty == BlockType::Class;
        self.enter(BlockType::TypeParameters, name, lineno);
        if parent_is_class {
            let cur = self.cur();
            self.arena[cur].can_see_class_scope = true;
            self.add_def("__classdict__", USE);
        }
        if is_class_def {
            // "Set" when the type-params tuple is created, "used" when
            // the bases are built; `.generic_base` powers the implicit
            // `Generic[…]` base.
            self.add_def(".type_params", DEF_LOCAL);
            self.add_def(".type_params", USE);
            self.add_def(".generic_base", DEF_LOCAL);
            self.add_def(".generic_base", USE);
        }
        if has_defaults {
            self.add_def(".defaults", DEF_PARAM);
        }
        if has_kwdefaults {
            self.add_def(".kwdefaults", DEF_PARAM);
        }
    }

    /// CPython `symtable_visit_type_param`: bind each parameter in
    /// the current (type-parameters) block; bounds/constraints and
    /// PEP 696 defaults each evaluate in their own
    /// `TypeVariableBlock`.
    fn visit_type_params(&mut self, type_params: &[past::TypeParam]) {
        for tp in type_params {
            self.add_def(&tp.name, DEF_TYPE_PARAM | DEF_LOCAL);
            if let past::TypeParamKind::TypeVar { bound: Some(b) } = &tp.kind {
                self.visit_type_var_block(&tp.name, b);
            }
            if let Some(d) = &tp.default {
                self.visit_type_var_block(&tp.name, d);
            }
        }
    }

    /// One `TypeVariableBlock` holding a type parameter's bound,
    /// constraints, or default expression.
    fn visit_type_var_block(&mut self, name: &str, e: &past::Expr) {
        let can_see = self.arena[self.cur()].can_see_class_scope;
        self.enter(BlockType::TypeVariable, name, self.lineno(e.span));
        if can_see {
            let cur = self.cur();
            self.arena[cur].can_see_class_scope = true;
            self.add_def("__classdict__", USE);
        }
        self.visit_expr(e);
        self.exit();
    }

    fn visit_stmt(&mut self, s: &past::Stmt) {
        use past::StmtKind as S;
        let lineno = self.lineno(s.span);
        match &s.kind {
            S::FunctionDef {
                name,
                args,
                body,
                decorator_list,
                returns,
                type_params,
            }
            | S::AsyncFunctionDef {
                name,
                args,
                body,
                decorator_list,
                returns,
                type_params,
            } => {
                self.add_def(name, DEF_LOCAL);
                self.visit_defaults(args);
                for d in decorator_list {
                    self.visit_expr(d);
                }
                let generic = !type_params.is_empty();
                if generic {
                    let has_defaults = !args.defaults.is_empty();
                    let has_kwdefaults = args.kw_defaults.iter().any(Option::is_some);
                    self.enter_type_param_block(name, lineno, false, has_defaults, has_kwdefaults);
                    self.visit_type_params(type_params);
                }
                // Annotations resolve inside the hidden type-parameters
                // block when the `def` is generic.
                self.visit_annotations(args, returns.as_deref());
                self.enter(BlockType::Function, name, lineno);
                self.add_params(args);
                for st in body {
                    self.visit_stmt(st);
                }
                self.exit();
                if generic {
                    self.exit();
                }
            }
            S::ClassDef {
                name,
                bases,
                keywords,
                body,
                decorator_list,
                type_params,
            } => {
                self.add_def(name, DEF_LOCAL);
                for d in decorator_list {
                    self.visit_expr(d);
                }
                let generic = !type_params.is_empty();
                if generic {
                    self.enter_type_param_block(name, lineno, true, false, false);
                    self.visit_type_params(type_params);
                }
                // A generic class's bases/keywords evaluate inside the
                // hidden type-parameters block.
                for b in bases {
                    self.visit_expr(b);
                }
                for k in keywords {
                    self.visit_expr(&k.value);
                }
                self.enter(BlockType::Class, name, lineno);
                if generic {
                    self.add_def("__type_params__", DEF_LOCAL);
                    self.add_def(".type_params", USE);
                }
                for st in body {
                    self.visit_stmt(st);
                }
                self.exit();
                if generic {
                    self.exit();
                }
            }
            S::TypeAlias {
                name,
                type_params,
                value,
                ..
            } => {
                // The alias name is a Store in the enclosing scope.
                self.add_def(name, DEF_LOCAL);
                let is_in_class = self.arena[self.cur()].ty == BlockType::Class;
                let generic = !type_params.is_empty();
                if generic {
                    self.enter_type_param_block(name, lineno, false, false, false);
                    self.visit_type_params(type_params);
                }
                self.enter(BlockType::TypeAlias, name, lineno);
                if is_in_class {
                    let cur = self.cur();
                    self.arena[cur].can_see_class_scope = true;
                    self.add_def("__classdict__", USE);
                }
                self.visit_expr(value);
                self.exit();
                if generic {
                    self.exit();
                }
            }
            S::Return(v) => {
                if let Some(e) = v {
                    self.visit_expr(e);
                }
            }
            S::Assign { targets, value } => {
                self.visit_expr(value);
                for t in targets {
                    self.bind_target(t);
                }
            }
            S::AugAssign { target, value, .. } => {
                // CPython visits the augmented target as a Store (DEF_LOCAL
                // only, no USE), then the value.
                self.bind_target(target);
                self.visit_expr(value);
            }
            S::AnnAssign {
                target,
                annotation,
                value,
                ..
            } => {
                if let past::ExprKind::Name(n) = &target.kind {
                    self.add_def(n, DEF_ANNOT);
                    self.add_def(n, DEF_LOCAL);
                } else {
                    self.bind_target(target);
                }
                self.visit_expr(annotation);
                if let Some(v) = value {
                    self.visit_expr(v);
                }
            }
            S::If { test, body, orelse }
            | S::While {
                test, body, orelse, ..
            } => {
                self.visit_expr(test);
                self.visit_block(body);
                self.visit_block(orelse);
            }
            S::For {
                target,
                iter,
                body,
                orelse,
            }
            | S::AsyncFor {
                target,
                iter,
                body,
                orelse,
            } => {
                self.bind_target(target);
                self.visit_expr(iter);
                self.visit_block(body);
                self.visit_block(orelse);
            }
            S::Try {
                body,
                handlers,
                orelse,
                finalbody,
            } => {
                self.visit_block(body);
                for h in handlers {
                    if let Some(t) = &h.type_ {
                        self.visit_expr(t);
                    }
                    if let Some(n) = &h.name {
                        self.add_def(n, DEF_LOCAL);
                    }
                    self.visit_block(&h.body);
                }
                self.visit_block(orelse);
                self.visit_block(finalbody);
            }
            S::Raise { exc, cause } => {
                if let Some(e) = exc {
                    self.visit_expr(e);
                }
                if let Some(c) = cause {
                    self.visit_expr(c);
                }
            }
            S::With { items, body } | S::AsyncWith { items, body } => {
                for it in items {
                    self.visit_expr(&it.context_expr);
                    if let Some(v) = &it.optional_vars {
                        self.bind_target(v);
                    }
                }
                self.visit_block(body);
            }
            S::Import(aliases) => {
                for a in aliases {
                    // `import a.b.c` binds `a`; `import a.b as c` binds `c`.
                    let bound = match &a.asname {
                        Some(n) => n.as_str(),
                        None => a.name.split('.').next().unwrap_or(&a.name),
                    };
                    self.add_def(bound, DEF_IMPORT);
                }
            }
            S::ImportFrom { names, .. } => {
                for a in names {
                    if a.name == "*" {
                        continue;
                    }
                    let bound = a.asname.as_deref().unwrap_or(&a.name);
                    self.add_def(bound, DEF_IMPORT);
                }
            }
            S::Global(names) => {
                for n in names {
                    self.add_def(n, DEF_GLOBAL);
                    self.add_def_root(n, DEF_GLOBAL);
                }
            }
            S::Nonlocal(names) => {
                for n in names {
                    self.add_def(n, DEF_NONLOCAL);
                }
            }
            S::Match { subject, cases } => {
                self.visit_expr(subject);
                for c in cases {
                    self.visit_pattern(&c.pattern);
                    if let Some(g) = &c.guard {
                        self.visit_expr(g);
                    }
                    self.visit_block(&c.body);
                }
            }
            S::Expr(e) => self.visit_expr(e),
            S::Pass | S::Break | S::Continue => {}
            S::Delete(targets) => {
                for t in targets {
                    self.bind_target(t);
                }
            }
            S::Assert { test, msg } => {
                self.visit_expr(test);
                if let Some(m) = msg {
                    self.visit_expr(m);
                }
            }
        }
    }

    fn visit_block(&mut self, stmts: &[past::Stmt]) {
        for s in stmts {
            self.visit_stmt(s);
        }
    }

    /// Record a name appearing in store/del position.
    fn bind_target(&mut self, e: &past::Expr) {
        use past::ExprKind as E;
        match &e.kind {
            E::Name(n) => self.add_def(n, DEF_LOCAL),
            E::Tuple(items) | E::List(items) => {
                for it in items {
                    self.bind_target(it);
                }
            }
            E::Starred(inner) => self.bind_target(inner),
            E::Attribute { value, .. } => self.visit_expr(value),
            E::Subscript { value, slice } => {
                self.visit_expr(value);
                self.visit_expr(slice);
            }
            _ => self.visit_expr(e),
        }
    }

    fn visit_expr(&mut self, e: &past::Expr) {
        use past::ExprKind as E;
        let span = e.span;
        match &e.kind {
            E::Constant(_) => {}
            E::Name(n) => {
                self.add_def(n, USE);
                // Zero-argument `super()` implicitly closes over `__class__`;
                // CPython models a `super` load as a use of `__class__`.
                if n == "super" && self.arena[self.cur()].ty.is_function_like() {
                    self.add_def("__class__", USE);
                }
            }
            E::Attribute { value, .. } => self.visit_expr(value),
            E::Subscript { value, slice } => {
                self.visit_expr(value);
                self.visit_expr(slice);
            }
            E::Slice { lower, upper, step } => {
                for o in [lower, upper, step].into_iter().flatten() {
                    self.visit_expr(o);
                }
            }
            E::BinOp { left, right, .. } => {
                self.visit_expr(left);
                self.visit_expr(right);
            }
            E::BoolOp { values, .. } => {
                for v in values {
                    self.visit_expr(v);
                }
            }
            E::UnaryOp { operand, .. } => self.visit_expr(operand),
            E::Compare {
                left, comparators, ..
            } => {
                self.visit_expr(left);
                for c in comparators {
                    self.visit_expr(c);
                }
            }
            E::IfExp { test, body, orelse } => {
                self.visit_expr(test);
                self.visit_expr(body);
                self.visit_expr(orelse);
            }
            E::NamedExpr { target, value } => {
                // Walrus binds in the current scope (the comprehension-leak
                // special case is intentionally not modelled).
                self.visit_expr(value);
                if let past::ExprKind::Name(n) = &target.kind {
                    self.add_def(n, DEF_LOCAL);
                } else {
                    self.bind_target(target);
                }
            }
            E::Lambda { args, body } | E::TypeParamFn { args, body } => {
                self.visit_defaults(args);
                self.enter(BlockType::Function, "lambda", self.lineno(span));
                self.add_params(args);
                self.visit_expr(body);
                self.exit();
            }
            E::Call {
                func,
                args,
                keywords,
            } => {
                self.visit_expr(func);
                for a in args {
                    self.visit_expr(a);
                }
                for k in keywords {
                    self.visit_expr(&k.value);
                }
            }
            E::Tuple(items) | E::List(items) | E::Set(items) => {
                for it in items {
                    self.visit_expr(it);
                }
            }
            E::Dict { keys, values } => {
                for k in keys.iter().flatten() {
                    self.visit_expr(k);
                }
                for v in values {
                    self.visit_expr(v);
                }
            }
            // PEP 709: list/set/dict comprehensions are inlined into the
            // enclosing block — visit their parts here, no child scope.
            E::ListComp { elt, generators } | E::SetComp { elt, generators } => {
                self.visit_inline_comp(generators, &[elt]);
            }
            E::DictComp {
                key,
                value,
                generators,
            } => {
                self.visit_inline_comp(generators, &[key, value]);
            }
            // Generator expressions keep their own `genexpr` block.
            E::GeneratorExp { elt, generators } => {
                self.visit_genexpr(generators, &[elt], self.lineno(span));
            }
            E::Starred(value) => self.visit_expr(value),
            E::Yield(value) => {
                if let Some(v) = value {
                    self.visit_expr(v);
                }
            }
            E::YieldFrom(value) | E::Await(value) => self.visit_expr(value),
            E::JoinedStr(parts) => {
                for p in parts {
                    self.visit_expr(p);
                }
            }
            E::FormattedValue {
                value, format_spec, ..
            } => {
                self.visit_expr(value);
                if let Some(s) = format_spec {
                    self.visit_expr(s);
                }
            }
        }
    }

    /// Inlined comprehension (list/set/dict): everything is analyzed in the
    /// current block.
    fn visit_inline_comp(&mut self, generators: &[past::Comprehension], elts: &[&past::Expr]) {
        for (i, g) in generators.iter().enumerate() {
            self.visit_expr(&g.iter);
            // Inlined comprehension targets become locals of the enclosing
            // block, matching CPython 3.13's symbol table.
            self.bind_target(&g.target);
            let _ = i;
            for cond in &g.ifs {
                self.visit_expr(cond);
            }
        }
        for e in elts {
            self.visit_expr(e);
        }
    }

    /// Generator expression: its own `genexpr` block with a `.0` argument;
    /// the outermost iterable is evaluated in the enclosing block.
    fn visit_genexpr(
        &mut self,
        generators: &[past::Comprehension],
        elts: &[&past::Expr],
        lineno: i64,
    ) {
        if let Some(first) = generators.first() {
            self.visit_expr(&first.iter);
        }
        self.enter(BlockType::Function, "genexpr", lineno);
        self.add_param(".0");
        if let Some(first) = generators.first() {
            self.bind_target(&first.target);
            for cond in &first.ifs {
                self.visit_expr(cond);
            }
        }
        for g in generators.iter().skip(1) {
            self.visit_expr(&g.iter);
            self.bind_target(&g.target);
            for cond in &g.ifs {
                self.visit_expr(cond);
            }
        }
        for e in elts {
            self.visit_expr(e);
        }
        self.exit();
    }

    fn visit_pattern(&mut self, p: &past::Pattern) {
        use past::Pattern as P;
        match p {
            P::Value(e) => self.visit_expr(e),
            P::Singleton(_) => {}
            P::Capture(Some(n)) => self.add_def(n, DEF_LOCAL),
            P::Capture(None) => {}
            P::Sequence(items) | P::Or(items) => {
                for it in items {
                    self.visit_pattern(it);
                }
            }
            P::Star(Some(n)) => self.add_def(n, DEF_LOCAL),
            P::Star(None) => {}
            P::Mapping {
                keys,
                patterns,
                rest,
            } => {
                for k in keys {
                    self.visit_expr(k);
                }
                for pat in patterns {
                    self.visit_pattern(pat);
                }
                if let Some(Some(n)) = rest {
                    self.add_def(n, DEF_LOCAL);
                }
            }
            P::Class {
                cls,
                positionals,
                keywords,
            } => {
                self.visit_expr(cls);
                for pat in positionals {
                    self.visit_pattern(pat);
                }
                for (_, pat) in keywords {
                    self.visit_pattern(pat);
                }
            }
            P::As { pattern, name } => {
                self.visit_pattern(pattern);
                self.add_def(name, DEF_LOCAL);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Phase 2 — resolve scopes (CPython's analyze_block / analyze_name).
// ---------------------------------------------------------------------------

struct Analyzer<'a> {
    arena: &'a mut Vec<Block>,
}

impl Analyzer<'_> {
    fn analyze(&mut self, root: usize) {
        let mut bound = HashSet::new();
        let mut free = HashSet::new();
        let mut global = HashSet::new();
        self.analyze_block(root, &mut bound, &mut free, &mut global);
    }

    fn analyze_block(
        &mut self,
        idx: usize,
        bound: &mut HashSet<String>,
        free: &mut HashSet<String>,
        global: &mut HashSet<String>,
    ) {
        let ty = self.arena[idx].ty;
        let func_like = ty.is_function_like();
        let is_class = ty == BlockType::Class;

        let mut local: HashSet<String> = HashSet::new();
        let mut scopes: HashMap<String, i64> = HashMap::new();
        let mut newglobal: HashSet<String> = HashSet::new();
        let mut newfree: HashSet<String> = HashSet::new();
        let mut newbound: HashSet<String> = HashSet::new();

        // Class bindings aren't visible to nested functions, so seed the
        // child sets before analyzing the class's own names.
        if is_class {
            newglobal.extend(global.iter().cloned());
            newbound.extend(bound.iter().cloned());
        }

        let syms: Vec<(String, i64)> = self.arena[idx]
            .symbols
            .iter()
            .map(|(k, v)| (k.clone(), *v))
            .collect();
        for (name, flags) in &syms {
            analyze_name(&mut scopes, name, *flags, bound, &mut local, free, global);
        }

        if !is_class {
            if func_like {
                newbound.extend(local.iter().cloned());
            }
            newbound.extend(bound.iter().cloned());
            newglobal.extend(global.iter().cloned());
        } else {
            // Classes provide implicit cells for `__class__` and
            // `__classdict__` to nested scopes.
            newbound.insert("__class__".to_owned());
            newbound.insert("__classdict__".to_owned());
        }

        let children = self.arena[idx].children.clone();
        let mut allfree: HashSet<String> = HashSet::new();
        for c in children {
            let mut cb = newbound.clone();
            let mut cf: HashSet<String> = HashSet::new();
            let mut cg = newglobal.clone();
            self.analyze_block(c, &mut cb, &mut cf, &mut cg);
            allfree.extend(cf);
        }
        newfree.extend(allfree);

        if func_like {
            analyze_cells(&mut scopes, &mut newfree);
        } else if is_class {
            newfree.remove("__class__");
            newfree.remove("__classdict__");
        }

        let classflag = is_class || self.arena[idx].can_see_class_scope;
        update_symbols(
            &mut self.arena[idx].symbols,
            &scopes,
            bound,
            &newfree,
            classflag,
        );

        free.extend(newfree);
    }
}

fn analyze_name(
    scopes: &mut HashMap<String, i64>,
    name: &str,
    flags: i64,
    bound: &mut HashSet<String>,
    local: &mut HashSet<String>,
    free: &mut HashSet<String>,
    global: &mut HashSet<String>,
) {
    if flags & DEF_GLOBAL != 0 {
        scopes.insert(name.to_owned(), GLOBAL_EXPLICIT);
        global.insert(name.to_owned());
        bound.remove(name);
        return;
    }
    if flags & DEF_NONLOCAL != 0 {
        scopes.insert(name.to_owned(), FREE);
        free.insert(name.to_owned());
        return;
    }
    if flags & DEF_BOUND != 0 {
        scopes.insert(name.to_owned(), LOCAL);
        local.insert(name.to_owned());
        global.remove(name);
        return;
    }
    if bound.contains(name) {
        scopes.insert(name.to_owned(), FREE);
        free.insert(name.to_owned());
        return;
    }
    if global.contains(name) {
        scopes.insert(name.to_owned(), GLOBAL_IMPLICIT);
        return;
    }
    scopes.insert(name.to_owned(), GLOBAL_IMPLICIT);
}

/// Promote locals referenced by nested scopes to cell variables.
fn analyze_cells(scopes: &mut HashMap<String, i64>, free: &mut HashSet<String>) {
    let locals: Vec<String> = scopes
        .iter()
        .filter(|(_, &s)| s == LOCAL)
        .map(|(n, _)| n.clone())
        .collect();
    for n in locals {
        if free.contains(&n) {
            scopes.insert(n.clone(), CELL);
            free.remove(&n);
        }
    }
}

fn update_symbols(
    symbols: &mut IndexMap<String, i64>,
    scopes: &HashMap<String, i64>,
    bound: &HashSet<String>,
    free: &HashSet<String>,
    classflag: bool,
) {
    for (name, flags) in symbols.iter_mut() {
        if let Some(&scope) = scopes.get(name) {
            *flags |= scope << SCOPE_OFF;
        }
    }
    for name in free {
        if let Some(&flags) = symbols.get(name) {
            if classflag && (flags & (DEF_BOUND | DEF_GLOBAL)) != 0 {
                symbols.insert(name.clone(), flags | DEF_FREE_CLASS);
            }
            continue;
        }
        if !bound.contains(name) {
            continue; // resolved to a global, not propagated
        }
        symbols.insert(name.clone(), FREE << SCOPE_OFF);
    }
}

// ---------------------------------------------------------------------------
// Raw-table conversion (block tree → nested dict for `symtable.py`).
// ---------------------------------------------------------------------------

fn to_object(arena: &[Block], idx: usize) -> Object {
    let b = &arena[idx];
    let mut d = DictData::default();
    d.insert(
        DictKey(Object::from_static("type")),
        Object::Int(b.ty.cpython()),
    );
    d.insert(DictKey(Object::from_static("id")), Object::Int(b.id));
    d.insert(
        DictKey(Object::from_static("name")),
        Object::from_str(b.name.clone()),
    );
    d.insert(
        DictKey(Object::from_static("lineno")),
        Object::Int(b.lineno),
    );
    d.insert(
        DictKey(Object::from_static("nested")),
        Object::Bool(b.nested),
    );

    let mut syms = DictData::default();
    for (name, flags) in &b.symbols {
        syms.insert(DictKey(Object::from_str(name.clone())), Object::Int(*flags));
    }
    d.insert(
        DictKey(Object::from_static("symbols")),
        Object::Dict(Rc::new(RefCell::new(syms))),
    );

    let varnames = b
        .varnames
        .iter()
        .map(|v| Object::from_str(v.clone()))
        .collect();
    d.insert(
        DictKey(Object::from_static("varnames")),
        Object::new_list(varnames),
    );

    let children = b.children.iter().map(|&c| to_object(arena, c)).collect();
    d.insert(
        DictKey(Object::from_static("children")),
        Object::new_list(children),
    );

    Object::Dict(Rc::new(RefCell::new(d)))
}

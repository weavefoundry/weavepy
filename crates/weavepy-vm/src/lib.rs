//! WeavePy virtual machine.
//!
//! Drives a stack of [`Frame`]s through a [`weavepy_compiler::CodeObject`]
//! and produces the same observable effects as CPython for the
//! subset of Python defined by RFC 0001.
//!
//! [`Interpreter`] is the embedding API. A typical caller wires
//! source through `weavepy_lexer` / `weavepy_parser` / `weavepy_compiler`
//! and then hands the resulting code object to [`Interpreter::run_module`].
//!
//! # Output capture
//!
//! Programs print via `print(...)`, which writes to a sink supplied
//! through [`Interpreter::set_stdout`]. Hosts that want to capture
//! output (REPL, test runners, the conformance harness) plug in a
//! `Vec<u8>` writer; the CLI uses the process stdout.

use std::cell::RefCell;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use weavepy_compiler::{
    BinOpKind, CodeObject, CompareKind, Constant, ExcHandler, OpCode, UnaryKind,
};

pub mod builtin_types;
pub mod builtins;
pub mod error;
pub mod import;
pub mod object;
pub mod stdlib;
pub mod types;

use crate::builtin_types::{builtin_types, instance_is_subclass, make_exception_with_class};
use crate::error::{
    attribute_error, import_error, index_error, key_error, module_not_found_error, name_error,
    runtime_error, type_error, value_error, zero_division_error, TracebackEntry,
};
pub use crate::error::{PyException, RuntimeError};
pub use crate::import::ModuleCache;
use crate::import::{package_search_path, resolve_relative};
use crate::object::{
    BoundMethod, BuiltinFn, DictData, DictKey, Object, PyFunction, PyModule, PySlice,
};
use crate::types::{PyInstance, TypeObject};

// ---------- frame ----------

struct Frame {
    code: Rc<CodeObject>,
    /// Local variables, indexed by `LOAD_FAST` / `STORE_FAST`.
    locals: Vec<Object>,
    /// Cell storage. Layout: `code.cellvars` first, then `code.freevars`.
    cells: Vec<Rc<RefCell<Object>>>,
    /// Evaluation stack.
    stack: Vec<Object>,
    /// Globals shared across frames within the same module.
    globals: Rc<RefCell<DictData>>,
    /// For class-body frames, names are stored here instead of globals.
    /// `None` for ordinary function and module frames.
    class_namespace: Option<Rc<RefCell<DictData>>>,
    /// Stack of currently-handled exceptions. `PUSH_EXC_INFO` pushes
    /// onto this; `POP_EXCEPT` pops; `RERAISE 1` re-raises the top.
    exc_handlers: Vec<PyException>,
    /// pc *before* the current instruction — used to look up the
    /// exception handler when an opcode raises.
    pc: u32,
}

impl Frame {
    fn push(&mut self, v: Object) {
        self.stack.push(v);
    }

    fn pop(&mut self) -> Result<Object, RuntimeError> {
        self.stack
            .pop()
            .ok_or_else(|| RuntimeError::Internal("stack underflow".to_owned()))
    }

    fn top(&self) -> Result<&Object, RuntimeError> {
        self.stack
            .last()
            .ok_or_else(|| RuntimeError::Internal("stack empty".to_owned()))
    }
}

// ---------- interpreter ----------

/// Output sink. Either the process's stdout or a `Vec<u8>` for
/// embedding callers.
pub type Stdout = Rc<RefCell<dyn Write>>;

/// The top-level entry point for executing WeavePy bytecode.
#[allow(missing_debug_implementations)]
pub struct Interpreter {
    stdout: Stdout,
    builtins: Rc<RefCell<DictData>>,
    cache: ModuleCache,
}

impl Default for Interpreter {
    fn default() -> Self {
        let stdout: Stdout = Rc::new(RefCell::new(std::io::stdout()));
        let builtins = Rc::new(RefCell::new(builtins::default_builtins()));
        let cache = ModuleCache::default();
        stdlib::register_all(&cache);
        Self {
            stdout,
            builtins,
            cache,
        }
    }
}

impl Interpreter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Plug in a custom stdout sink (e.g. a `Vec<u8>` for tests).
    pub fn set_stdout(&mut self, stdout: Stdout) {
        self.stdout = stdout;
    }

    /// Expose the module cache (so the embedding host can poke
    /// `sys.modules`, register custom built-in modules, etc.).
    pub fn module_cache(&self) -> &ModuleCache {
        &self.cache
    }

    /// Replace `sys.argv` with the given values. The first entry is
    /// the script name; subsequent entries are passed-through args.
    pub fn set_argv<I, S>(&self, values: I)
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let mut argv = self.cache.argv.borrow_mut();
        argv.clear();
        for v in values {
            argv.push(Object::from_str(v.into()));
        }
    }

    /// Prepend a directory to `sys.path`. Idempotent.
    pub fn prepend_path(&self, dir: impl Into<PathBuf>) {
        let s = dir.into().to_string_lossy().into_owned();
        let mut path = self.cache.path.borrow_mut();
        if !path_contains(&path, &s) {
            path.insert(0, Object::from_str(s));
        }
    }

    /// Append a directory to `sys.path`. Idempotent.
    pub fn append_path(&self, dir: impl Into<PathBuf>) {
        let s = dir.into().to_string_lossy().into_owned();
        let mut path = self.cache.path.borrow_mut();
        if !path_contains(&path, &s) {
            path.push(Object::from_str(s));
        }
    }

    /// Wire `print` (and friends) to this interpreter's stdout.
    /// `print` is installed as a special builtin — the VM intercepts
    /// the call so it can dispatch `__str__` on user types.
    fn install_print_into(&self, dict: &mut DictData) {
        let f = BuiltinFn {
            name: "print",
            call: Box::new(move |_args: &[Object]| {
                Err(runtime_error("internal: print called outside VM"))
            }),
        };
        dict.insert(
            DictKey(Object::from_static("print")),
            Object::Builtin(Rc::new(f)),
        );
    }

    /// Run a module-level [`CodeObject`] as `__main__`. The most
    /// common entry point for the CLI and embedding hosts.
    pub fn run_module(&mut self, code: &CodeObject) -> Result<Object, RuntimeError> {
        self.run_module_as(code, "__main__", None)
    }

    /// As [`run_module`], but lets the caller pick the `__name__` and
    /// optional `__file__` to install in the module's globals.
    pub fn run_module_as(
        &mut self,
        code: &CodeObject,
        name: &str,
        file: Option<&str>,
    ) -> Result<Object, RuntimeError> {
        let globals = self.build_module_globals(name, file, None);
        let code_rc = Rc::new(code.clone());
        let mut frame = self.make_frame(code_rc, Vec::new(), Vec::new(), globals, true);
        self.run_frame(&mut frame)
    }

    /// Populate a fresh module-globals dict with builtins, builtin
    /// types, and the standard module dunders. Used by both
    /// `run_module_as` and the import loader.
    fn build_module_globals(
        &self,
        name: &str,
        file: Option<&str>,
        package: Option<&str>,
    ) -> Rc<RefCell<DictData>> {
        let globals = Rc::new(RefCell::new(DictData::new()));
        let mut g = globals.borrow_mut();
        self.install_print_into(&mut g);
        for (n, value) in builtin_types().as_globals() {
            g.insert(DictKey(Object::from_str(n)), value);
        }
        g.insert(
            DictKey(Object::from_static("__name__")),
            Object::from_str(name),
        );
        g.insert(DictKey(Object::from_static("__doc__")), Object::None);
        g.insert(
            DictKey(Object::from_static("__package__")),
            match package {
                Some(p) => Object::from_str(p),
                None => Object::from_static(""),
            },
        );
        if let Some(f) = file {
            g.insert(
                DictKey(Object::from_static("__file__")),
                Object::from_str(f),
            );
        }
        g.insert(
            DictKey(Object::from_static("__builtins__")),
            Object::Dict(self.builtins.clone()),
        );
        drop(g);
        globals
    }

    fn make_frame(
        &self,
        code: Rc<CodeObject>,
        positional: Vec<Object>,
        closure: Vec<Object>,
        globals: Rc<RefCell<DictData>>,
        _is_module: bool,
    ) -> Frame {
        let mut locals = vec![Object::None; code.varnames.len()];
        for (i, v) in positional.into_iter().enumerate() {
            if i < locals.len() {
                locals[i] = v;
            }
        }
        // Build cells: cellvars come first (fresh), then freevars
        // (provided by the caller via `closure`).
        let mut cells: Vec<Rc<RefCell<Object>>> =
            Vec::with_capacity(code.cellvars.len() + code.freevars.len());
        for cell_name in &code.cellvars {
            // If a cellvar matches a parameter name, the initial
            // value comes from `locals` — promote it.
            let initial = code
                .varnames
                .iter()
                .position(|n| n == cell_name)
                .map_or(Object::None, |idx| locals[idx].clone());
            cells.push(Rc::new(RefCell::new(initial)));
        }
        for cell in closure {
            match cell {
                Object::Cell(c) => cells.push(c),
                other => cells.push(Rc::new(RefCell::new(other))),
            }
        }
        Frame {
            code,
            locals,
            cells,
            stack: Vec::with_capacity(16),
            globals,
            class_namespace: None,
            exc_handlers: Vec::new(),
            pc: 0,
        }
    }

    // ---------- dispatch ----------

    fn run_frame(&mut self, frame: &mut Frame) -> Result<Object, RuntimeError> {
        loop {
            match self.step(frame) {
                Ok(StepOutcome::Continue) => {}
                Ok(StepOutcome::Return(v)) => return Ok(v),
                Err(err) => {
                    // Try to find a handler in the exception table.
                    if let RuntimeError::PyException(exc) = err {
                        match self.handle_exception(frame, exc) {
                            Ok(Some(())) => continue,
                            Ok(None) => unreachable!(),
                            Err(e) => return Err(e),
                        }
                    } else {
                        return Err(err);
                    }
                }
            }
        }
    }

    /// Run a single instruction. The `pc` is advanced past it; if the
    /// instruction returns from the frame we surface that via
    /// `StepOutcome::Return`.
    fn step(&mut self, frame: &mut Frame) -> Result<StepOutcome, RuntimeError> {
        let raised_at = frame.pc;
        let ins = frame
            .code
            .instructions
            .get(frame.pc as usize)
            .copied()
            .ok_or_else(|| {
                RuntimeError::Internal(format!(
                    "pc {} out of bounds in {}",
                    frame.pc, frame.code.name
                ))
            })?;
        let _ = raised_at;
        frame.pc += 1;
        match ins.op {
            OpCode::Nop | OpCode::Resume => {}
            OpCode::LoadConst => {
                let c = frame
                    .code
                    .constants
                    .get(ins.arg as usize)
                    .ok_or_else(|| RuntimeError::Internal("bad const index".to_owned()))?
                    .clone();
                frame.push(constant_to_object(c));
            }
            OpCode::LoadName => {
                let name = self.name_at(&frame.code, ins.arg)?;
                let from_ns = frame
                    .class_namespace
                    .as_ref()
                    .and_then(|ns| ns.borrow().get(&DictKey(Object::from_str(&name))).cloned());
                let v = match from_ns {
                    Some(v) => v,
                    None => self.lookup_global_or_builtin(&frame.globals, &name)?,
                };
                frame.push(v);
            }
            OpCode::LoadGlobal => {
                let name = self.name_at(&frame.code, ins.arg)?;
                let v = self.lookup_global_or_builtin(&frame.globals, &name)?;
                frame.push(v);
            }
            OpCode::LoadFast => {
                let v = frame
                    .locals
                    .get(ins.arg as usize)
                    .cloned()
                    .ok_or_else(|| RuntimeError::Internal("bad local index".to_owned()))?;
                if matches!(v, Object::None)
                    && frame
                        .code
                        .varnames
                        .get(ins.arg as usize)
                        .map(|n| !is_param(&frame.code, n))
                        .unwrap_or(false)
                {
                    // It's possible the variable hasn't been
                    // assigned yet. We push None as a placeholder.
                }
                frame.push(v);
            }
            OpCode::StoreFast => {
                let v = frame.pop()?;
                let slot = ins.arg as usize;
                if slot < frame.locals.len() {
                    frame.locals[slot] = v;
                }
            }
            OpCode::DeleteFast => {
                let slot = ins.arg as usize;
                if slot < frame.locals.len() {
                    frame.locals[slot] = Object::None;
                }
            }
            OpCode::StoreName => {
                let v = frame.pop()?;
                let name = self.name_at(&frame.code, ins.arg)?;
                if let Some(ns) = &frame.class_namespace {
                    ns.borrow_mut().insert(DictKey(Object::from_str(name)), v);
                } else {
                    frame
                        .globals
                        .borrow_mut()
                        .insert(DictKey(Object::from_str(name)), v);
                }
            }
            OpCode::StoreGlobal => {
                let v = frame.pop()?;
                let name = self.name_at(&frame.code, ins.arg)?;
                frame
                    .globals
                    .borrow_mut()
                    .insert(DictKey(Object::from_str(name)), v);
            }
            OpCode::DeleteName => {
                let name = self.name_at(&frame.code, ins.arg)?;
                if let Some(ns) = &frame.class_namespace {
                    if ns
                        .borrow_mut()
                        .shift_remove(&DictKey(Object::from_str(&name)))
                        .is_some()
                    {
                        return Ok(StepOutcome::Continue);
                    }
                }
                frame
                    .globals
                    .borrow_mut()
                    .shift_remove(&DictKey(Object::from_str(name)));
            }
            OpCode::DeleteGlobal => {
                let name = self.name_at(&frame.code, ins.arg)?;
                frame
                    .globals
                    .borrow_mut()
                    .shift_remove(&DictKey(Object::from_str(name)));
            }
            OpCode::LoadDeref => {
                let cell = frame
                    .cells
                    .get(ins.arg as usize)
                    .cloned()
                    .ok_or_else(|| RuntimeError::Internal("bad cell index".to_owned()))?;
                let v = cell.borrow().clone();
                frame.push(v);
            }
            OpCode::StoreDeref => {
                let v = frame.pop()?;
                let cell = frame
                    .cells
                    .get(ins.arg as usize)
                    .cloned()
                    .ok_or_else(|| RuntimeError::Internal("bad cell index".to_owned()))?;
                *cell.borrow_mut() = v;
            }
            OpCode::MakeCell => {
                let slot = ins.arg as usize;
                if slot >= frame.cells.len() {
                    return Err(RuntimeError::Internal(
                        "MakeCell index out of bounds".to_owned(),
                    ));
                }
            }
            OpCode::LoadClosure => {
                let cell = frame
                    .cells
                    .get(ins.arg as usize)
                    .cloned()
                    .ok_or_else(|| RuntimeError::Internal("bad cell index".to_owned()))?;
                frame.push(Object::Cell(cell));
            }
            OpCode::LoadAttr => {
                let obj = frame.pop()?;
                let name = self.name_at(&frame.code, ins.arg)?;
                let v = self.load_attr(&obj, &name)?;
                frame.push(v);
            }
            OpCode::StoreAttr => {
                let obj = frame.pop()?;
                let val = frame.pop()?;
                let name = self.name_at(&frame.code, ins.arg)?;
                self.store_attr(&obj, &name, val)?;
            }
            OpCode::DeleteAttr => {
                let obj = frame.pop()?;
                let name = self.name_at(&frame.code, ins.arg)?;
                self.delete_attr(&obj, &name)?;
            }
            OpCode::BinarySubscr => {
                let i = frame.pop()?;
                let v = frame.pop()?;
                let r = self.binary_subscr(&v, &i)?;
                frame.push(r);
            }
            OpCode::StoreSubscr => {
                let i = frame.pop()?;
                let target = frame.pop()?;
                let value = frame.pop()?;
                self.store_subscr(&target, &i, value)?;
            }
            OpCode::DeleteSubscr => {
                let i = frame.pop()?;
                let target = frame.pop()?;
                self.delete_subscr(&target, &i)?;
            }
            OpCode::BinaryOp => {
                let b = frame.pop()?;
                let a = frame.pop()?;
                let kind: BinOpKind = unsafe { std::mem::transmute(ins.arg as u8) };
                let r = self.dispatch_binary_op(&a, &b, kind, &frame.globals)?;
                frame.push(r);
            }
            OpCode::UnaryOp => {
                let v = frame.pop()?;
                let kind: UnaryKind = unsafe { std::mem::transmute(ins.arg as u8) };
                let r = unary_op(&v, kind)?;
                frame.push(r);
            }
            OpCode::CompareOp => {
                let b = frame.pop()?;
                let a = frame.pop()?;
                let kind: CompareKind = unsafe { std::mem::transmute(ins.arg as u8) };
                let r = self.dispatch_compare_op(&a, &b, kind, &frame.globals)?;
                frame.push(Object::Bool(r));
            }
            OpCode::IsOp => {
                let b = frame.pop()?;
                let a = frame.pop()?;
                let same = a.is_same(&b);
                let result = if ins.arg == 1 { !same } else { same };
                frame.push(Object::Bool(result));
            }
            OpCode::ContainsOp => {
                let container = frame.pop()?;
                let item = frame.pop()?;
                let found = container.contains(&item)?;
                let result = if ins.arg == 1 { !found } else { found };
                frame.push(Object::Bool(result));
            }
            OpCode::PopTop => {
                frame.pop()?;
            }
            OpCode::CopyTop => {
                let v = frame.top()?.clone();
                frame.push(v);
            }
            OpCode::Swap => {
                let depth = ins.arg as usize;
                let n = frame.stack.len();
                if depth >= 1 && depth < n {
                    frame.stack.swap(n - 1, n - depth);
                }
            }
            OpCode::Call => {
                let argc = ins.arg as usize;
                let split_at = frame.stack.len().saturating_sub(argc);
                let mut args: Vec<Object> = frame.stack.split_off(split_at);
                let callable = frame.pop()?;
                // Zero-arg super(): inject __class__ from the free
                // cell named "__class__" and `self` from local 0.
                if args.is_empty() && is_super_callable(&callable) {
                    if let Some(class_cell) = find_cell(frame, "__class__") {
                        let class_obj = class_cell.borrow().clone();
                        if !matches!(class_obj, Object::None) {
                            let self_obj = frame.locals.first().cloned().unwrap_or(Object::None);
                            args.push(class_obj);
                            args.push(self_obj);
                        }
                    }
                }
                let r = self.call(&callable, &args, &[], &frame.globals)?;
                frame.push(r);
            }
            OpCode::CallKw => {
                let argc = ins.arg as usize;
                // Stack (top-down): kw_names_tuple, kw_values..., positional_values..., callable
                let names_obj = frame.pop()?;
                let names: Vec<String> = match names_obj {
                    Object::Tuple(items) => items.iter().map(|x| x.to_str()).collect(),
                    _ => {
                        return Err(RuntimeError::Internal(
                            "CallKw expects a tuple of names".to_owned(),
                        ))
                    }
                };
                let kwc = names.len();
                let split_kw_at = frame.stack.len().saturating_sub(kwc);
                let kw_values: Vec<Object> = frame.stack.split_off(split_kw_at);
                let split_pos_at = frame.stack.len().saturating_sub(argc);
                let pos_args: Vec<Object> = frame.stack.split_off(split_pos_at);
                let callable = frame.pop()?;
                let kw_pairs: Vec<(String, Object)> = names.into_iter().zip(kw_values).collect();
                let r = self.call(&callable, &pos_args, &kw_pairs, &frame.globals)?;
                frame.push(r);
            }
            OpCode::ReturnValue => {
                return Ok(StepOutcome::Return(frame.pop()?));
            }
            OpCode::PopJumpIfFalse => {
                let v = frame.pop()?;
                if !v.is_truthy() {
                    frame.pc += ins.arg;
                }
            }
            OpCode::PopJumpIfTrue => {
                let v = frame.pop()?;
                if v.is_truthy() {
                    frame.pc += ins.arg;
                }
            }
            OpCode::JumpForward => {
                frame.pc += ins.arg;
            }
            OpCode::JumpBackward => {
                frame.pc = frame.pc.saturating_sub(ins.arg);
            }
            OpCode::GetIter => {
                let v = frame.pop()?;
                let it = self.make_iter(&v, &frame.globals)?;
                frame.push(it);
            }
            OpCode::ForIter => {
                let it_obj = frame
                    .stack
                    .last()
                    .cloned()
                    .ok_or_else(|| RuntimeError::Internal("FOR_ITER no iter".to_owned()))?;
                let next = match &it_obj {
                    Object::Iter(it) => it.borrow_mut().next_value(),
                    Object::Instance(_) => {
                        // Call __next__; treat StopIteration as exhaustion.
                        match instance_method(&it_obj, "__next__") {
                            Some(m) => match self.call(&m, &[], &[], &frame.globals) {
                                Ok(v) => Some(v),
                                Err(RuntimeError::PyException(exc))
                                    if exc.type_name() == "StopIteration" =>
                                {
                                    None
                                }
                                Err(e) => return Err(e),
                            },
                            None => {
                                return Err(type_error(
                                    "iter() returned non-iterator without __next__",
                                ));
                            }
                        }
                    }
                    _ => {
                        return Err(RuntimeError::Internal(
                            "FOR_ITER expects iterator on stack".to_owned(),
                        ))
                    }
                };
                match next {
                    Some(v) => frame.push(v),
                    None => {
                        frame.pop()?;
                        frame.pc += ins.arg;
                    }
                }
            }
            OpCode::EndFor => {
                // Iterator was already popped by the exhausted
                // branch of FOR_ITER. Nothing to do.
            }
            OpCode::BuildList => {
                let n = ins.arg as usize;
                let split = frame.stack.len().saturating_sub(n);
                let items = frame.stack.split_off(split);
                frame.push(Object::new_list(items));
            }
            OpCode::BuildTuple => {
                let n = ins.arg as usize;
                let split = frame.stack.len().saturating_sub(n);
                let items = frame.stack.split_off(split);
                frame.push(Object::new_tuple(items));
            }
            OpCode::BuildSet => {
                // No native set yet — represent as list-deduped value
                // wrapped in a dict whose values are None.
                let n = ins.arg as usize;
                let split = frame.stack.len().saturating_sub(n);
                let items = frame.stack.split_off(split);
                let mut d = DictData::new();
                for x in items {
                    d.insert(DictKey(x), Object::None);
                }
                frame.push(Object::Dict(Rc::new(RefCell::new(d))));
            }
            OpCode::BuildMap => {
                let n = ins.arg as usize;
                let split = frame.stack.len().saturating_sub(2 * n);
                let pairs = frame.stack.split_off(split);
                let mut d = DictData::new();
                let mut it = pairs.into_iter();
                for _ in 0..n {
                    let k = it.next().ok_or_else(|| {
                        RuntimeError::Internal("BUILD_MAP missing key".to_owned())
                    })?;
                    let v = it.next().ok_or_else(|| {
                        RuntimeError::Internal("BUILD_MAP missing value".to_owned())
                    })?;
                    d.insert(DictKey(k), v);
                }
                frame.push(Object::Dict(Rc::new(RefCell::new(d))));
            }
            OpCode::BuildString => {
                let n = ins.arg as usize;
                let split = frame.stack.len().saturating_sub(n);
                let parts = frame.stack.split_off(split);
                let mut s = String::new();
                for p in parts {
                    s.push_str(&p.to_str());
                }
                frame.push(Object::from_str(s));
            }
            OpCode::ListAppend => {
                let v = frame.pop()?;
                let depth = ins.arg as usize;
                let lst = frame
                    .stack
                    .get(frame.stack.len().wrapping_sub(depth))
                    .cloned()
                    .ok_or_else(|| {
                        RuntimeError::Internal("LIST_APPEND depth out of range".to_owned())
                    })?;
                if let Object::List(lst) = lst {
                    lst.borrow_mut().push(v);
                } else {
                    return Err(RuntimeError::Internal(
                        "LIST_APPEND target is not a list".to_owned(),
                    ));
                }
            }
            OpCode::SetAdd => {
                let v = frame.pop()?;
                let depth = ins.arg as usize;
                let s = frame
                    .stack
                    .get(frame.stack.len().wrapping_sub(depth))
                    .cloned()
                    .ok_or_else(|| {
                        RuntimeError::Internal("SET_ADD depth out of range".to_owned())
                    })?;
                if let Object::Dict(d) = s {
                    d.borrow_mut().insert(DictKey(v), Object::None);
                }
            }
            OpCode::MapAdd => {
                let v = frame.pop()?;
                let k = frame.pop()?;
                let depth = ins.arg as usize;
                let d = frame
                    .stack
                    .get(frame.stack.len().wrapping_sub(depth))
                    .cloned()
                    .ok_or_else(|| {
                        RuntimeError::Internal("MAP_ADD depth out of range".to_owned())
                    })?;
                if let Object::Dict(d) = d {
                    d.borrow_mut().insert(DictKey(k), v);
                }
            }
            OpCode::UnpackSequence => {
                let n = ins.arg as usize;
                let v = frame.pop()?;
                let items: Vec<Object> = match v {
                    Object::Tuple(items) => items.iter().cloned().collect(),
                    Object::List(items) => items.borrow().clone(),
                    Object::Str(s) => s.chars().map(|c| Object::from_str(c.to_string())).collect(),
                    Object::Range(r) => {
                        let mut out = Vec::new();
                        let mut cur = r.start;
                        while (r.step > 0 && cur < r.stop) || (r.step < 0 && cur > r.stop) {
                            out.push(Object::Int(cur));
                            cur += r.step;
                        }
                        out
                    }
                    _ => {
                        return Err(type_error(format!(
                            "cannot unpack non-iterable {} object",
                            v.type_name()
                        )))
                    }
                };
                if items.len() != n {
                    return Err(value_error(format!(
                        "expected {} values to unpack, got {}",
                        n,
                        items.len()
                    )));
                }
                // Push in reverse so the first element ends up
                // at the lowest stack position — matches the
                // grouped STORE_FAST sequence emitted by the
                // compiler.
                for x in items.into_iter().rev() {
                    frame.push(x);
                }
            }
            OpCode::MakeFunction => {
                let code_obj = frame.pop()?;
                let code = match code_obj {
                    Object::Code(c) => c,
                    _ => {
                        return Err(RuntimeError::Internal(
                            "MAKE_FUNCTION expects code on top".to_owned(),
                        ))
                    }
                };
                let flags = ins.arg;
                let mut closure: Vec<Object> = Vec::new();
                if flags & 0x08 != 0 {
                    let tup = frame.pop()?;
                    if let Object::Tuple(items) = tup {
                        closure = items.iter().cloned().collect();
                    }
                }
                if flags & 0x04 != 0 {
                    frame.pop()?; // annotations dict — discarded
                }
                if flags & 0x02 != 0 {
                    frame.pop()?; // kw defaults dict — discarded
                }
                let mut defaults: Vec<Object> = Vec::new();
                if flags & 0x01 != 0 {
                    let tup = frame.pop()?;
                    if let Object::Tuple(items) = tup {
                        defaults = items.iter().cloned().collect();
                    }
                }
                let name = code.name.clone();
                let f = PyFunction {
                    name,
                    code,
                    globals: frame.globals.clone(),
                    defaults,
                    kw_defaults: Vec::new(),
                    closure,
                };
                frame.push(Object::Function(Rc::new(f)));
            }
            OpCode::BuildSlice => {
                let step = frame.pop()?;
                let stop = frame.pop()?;
                let start = frame.pop()?;
                frame.push(Object::Slice(Rc::new(PySlice { start, stop, step })));
            }
            OpCode::PrintExpr => {
                let v = frame.pop()?;
                if !matches!(v, Object::None) {
                    let mut sink = self.stdout.borrow_mut();
                    let _ = writeln!(sink, "{}", v.repr());
                }
            }
            OpCode::LoadBuildClass => {
                let f = builtins::build_class_builtin();
                frame.push(Object::Builtin(Rc::new(f)));
            }
            OpCode::LoadClassderef => {
                let idx = ins.arg as usize;
                let free_offset = frame.code.cellvars.len();
                let free_index = idx.saturating_sub(free_offset);
                let name = frame
                    .code
                    .freevars
                    .get(free_index)
                    .cloned()
                    .unwrap_or_default();
                let from_ns = frame
                    .class_namespace
                    .as_ref()
                    .and_then(|ns| ns.borrow().get(&DictKey(Object::from_str(&name))).cloned());
                let v = match from_ns {
                    Some(v) => v,
                    None => {
                        let cell =
                            frame.cells.get(idx).cloned().ok_or_else(|| {
                                RuntimeError::Internal("bad cell index".to_owned())
                            })?;
                        let v = cell.borrow().clone();
                        v
                    }
                };
                frame.push(v);
            }
            OpCode::RaiseVarargs => {
                let exc = match ins.arg {
                    0 => {
                        // Re-raise the currently-handled exception.
                        let top = frame
                            .exc_handlers
                            .last()
                            .cloned()
                            .ok_or_else(|| runtime_error("No active exception to re-raise"))?;
                        top
                    }
                    1 => {
                        let arg = frame.pop()?;
                        Self::normalize_exception(arg, None)?
                    }
                    2 => {
                        let cause = frame.pop()?;
                        let arg = frame.pop()?;
                        Self::normalize_exception(arg, Some(cause))?
                    }
                    other => {
                        return Err(RuntimeError::Internal(format!(
                            "RAISE_VARARGS arg out of range: {other}"
                        )))
                    }
                };
                return Err(RuntimeError::PyException(exc));
            }
            OpCode::CheckExcMatch => {
                // Stack on entry: [exc, type_or_tuple]
                // CPython's CHECK_EXC_MATCH pops `type` and peeks
                // `exc`. We push a bool onto the stack and leave
                // `exc` in place so the handler can bind it.
                let ty = frame.pop()?;
                let exc =
                    frame.stack.last().cloned().ok_or_else(|| {
                        RuntimeError::Internal("CHECK_EXC_MATCH no exc".to_owned())
                    })?;
                let matched = self.exception_matches(&exc, &ty)?;
                frame.push(Object::Bool(matched));
            }
            OpCode::PushExcInfo => {
                // The handler entry leaves the exception on top —
                // we record it for the duration of the handler.
                let exc = frame.top()?.clone();
                if let Object::Instance(_) = &exc {
                    let pe = PyException::new(exc);
                    frame.exc_handlers.push(pe);
                }
            }
            OpCode::PopExcept => {
                frame.exc_handlers.pop();
            }
            OpCode::Reraise => {
                let exc = if ins.arg == 0 {
                    frame.pop()?
                } else {
                    // `RERAISE 1` re-raises the currently-active exc.
                    let exc = frame
                        .exc_handlers
                        .last()
                        .map(|pe| pe.instance.clone())
                        .ok_or_else(|| runtime_error("No active exception to re-raise"))?;
                    exc
                };
                let pe = Self::normalize_exception(exc, None)?;
                return Err(RuntimeError::PyException(pe));
            }
            OpCode::BeforeWith => {
                let cm = frame.pop()?;
                let exit_method = self.load_attr(&cm, "__exit__")?;
                let enter_method = self.load_attr(&cm, "__enter__")?;
                let entered = self.call(&enter_method, &[], &[], &frame.globals)?;
                // Stack on exit: [exit_method, entered_value]
                frame.push(exit_method);
                frame.push(entered);
            }
            OpCode::WithExceptStart => {
                // Stack on entry (top → bottom): [exc, exit]
                // We call exit(type(exc), exc, None) and push the
                // result, leaving exc and exit beneath.
                let exc = frame
                    .stack
                    .last()
                    .cloned()
                    .ok_or_else(|| RuntimeError::Internal("WITH_EXCEPT_START".to_owned()))?;
                let exit_method = frame
                    .stack
                    .get(frame.stack.len().wrapping_sub(2))
                    .cloned()
                    .ok_or_else(|| RuntimeError::Internal("WITH_EXCEPT_START".to_owned()))?;
                let ty = match &exc {
                    Object::Instance(inst) => Object::Type(inst.class.clone()),
                    _ => Object::None,
                };
                let result =
                    self.call(&exit_method, &[ty, exc, Object::None], &[], &frame.globals)?;
                frame.push(result);
            }
            OpCode::ImportName => {
                let fromlist = frame.pop()?;
                let level_obj = frame.pop()?;
                let level = match level_obj {
                    Object::Int(i) if i >= 0 => i as u32,
                    Object::Int(_) => {
                        return Err(value_error("relative import level must be non-negative"))
                    }
                    other => {
                        return Err(type_error(format!(
                            "level must be int, not '{}'",
                            other.type_name()
                        )))
                    }
                };
                let name = self.name_at(&frame.code, ins.arg)?;
                let module = self.do_import(&name, &fromlist, level, &frame.globals)?;
                frame.push(module);
            }
            OpCode::ImportFrom => {
                let module =
                    frame.stack.last().cloned().ok_or_else(|| {
                        RuntimeError::Internal("IMPORT_FROM empty stack".to_owned())
                    })?;
                let name = self.name_at(&frame.code, ins.arg)?;
                let attr = self.import_from(&module, &name)?;
                frame.push(attr);
            }
            OpCode::ImportStar => {
                let module = frame.pop()?;
                self.import_star(&module, &frame.globals)?;
            }
        }
        Ok(StepOutcome::Continue)
    }

    // ---------- exception handling ----------

    /// Look up a handler for `exc` at the current pc. If found,
    /// truncate the stack and jump to the handler. Otherwise propagate.
    fn handle_exception(
        &self,
        frame: &mut Frame,
        mut exc: PyException,
    ) -> Result<Option<()>, RuntimeError> {
        // Note `pc` was already advanced past the raising instruction,
        // so the protected range matches against `pc - 1`.
        let raise_pc = frame.pc.saturating_sub(1);
        if let Some(handler) = find_handler(&frame.code.exception_table, raise_pc) {
            // Drop entries above the recorded stack depth.
            while frame.stack.len() > handler.depth as usize {
                frame.stack.pop();
            }
            // Push the exception instance for the handler to bind /
            // CHECK_EXC_MATCH to inspect.
            frame.stack.push(exc.instance.clone());
            frame.pc = handler.handler;
            return Ok(Some(()));
        }
        // Record this frame in the traceback and propagate.
        let line = frame
            .code
            .linetable
            .get(raise_pc as usize)
            .copied()
            .unwrap_or(0);
        exc.push_traceback(TracebackEntry {
            filename: frame.code.filename.clone(),
            funcname: frame.code.name.clone(),
            lineno: line,
        });
        Err(RuntimeError::PyException(exc))
    }

    /// Materialise a raised value into a [`PyException`]. Accepts an
    /// exception class (instantiates it) or an instance.
    fn normalize_exception(
        value: Object,
        cause: Option<Object>,
    ) -> Result<PyException, RuntimeError> {
        let bt = builtin_types();
        let inst = match value {
            Object::Type(t) => {
                if !t.flags.is_exception {
                    return Err(type_error(format!(
                        "exceptions must derive from BaseException, not '{}'",
                        t.name
                    )));
                }
                make_exception_with_class(t, "")
            }
            Object::Instance(inst) => {
                if !inst.class.flags.is_exception && !inst.class.is_subclass_of(&bt.base_exception)
                {
                    return Err(type_error("exceptions must derive from BaseException"));
                }
                Object::Instance(inst)
            }
            other => {
                return Err(type_error(format!(
                    "exceptions must derive from BaseException, not '{}'",
                    other.type_name()
                )));
            }
        };
        let mut pe = PyException::new(inst);
        if let Some(c) = cause {
            let cpe = Self::normalize_exception(c, None)?;
            pe.cause = Some(Box::new(cpe));
        }
        Ok(pe)
    }

    /// `True` if `exc`'s class is a subclass of the given type or any
    /// element of a tuple of types.
    fn exception_matches(&self, exc: &Object, ty: &Object) -> Result<bool, RuntimeError> {
        match ty {
            Object::Type(t) => Ok(instance_is_subclass(exc, t)),
            Object::Tuple(items) => {
                for t in items.iter() {
                    if let Object::Type(t) = t {
                        if instance_is_subclass(exc, t) {
                            return Ok(true);
                        }
                    }
                }
                Ok(false)
            }
            _ => Err(type_error(
                "catching classes that do not inherit from BaseException is not allowed",
            )),
        }
    }

    // ---------- helpers ----------

    fn name_at(&self, code: &CodeObject, arg: u32) -> Result<String, RuntimeError> {
        code.names
            .get(arg as usize)
            .cloned()
            .ok_or_else(|| RuntimeError::Internal("bad name index".to_owned()))
    }

    fn lookup_global_or_builtin(
        &self,
        globals: &Rc<RefCell<DictData>>,
        name: &str,
    ) -> Result<Object, RuntimeError> {
        let key = DictKey(Object::from_str(name));
        if let Some(v) = globals.borrow().get(&key) {
            return Ok(v.clone());
        }
        if let Some(v) = self.builtins.borrow().get(&key) {
            return Ok(v.clone());
        }
        Err(name_error(format!("name '{name}' is not defined")))
    }

    fn load_attr(&self, obj: &Object, name: &str) -> Result<Object, RuntimeError> {
        match obj {
            Object::Instance(inst) => {
                // Super proxies stash the real receiver under
                // `__self__`. Re-bind methods looked up via the proxy
                // so they run against the right `self`.
                let super_receiver = inst
                    .dict
                    .borrow()
                    .get(&DictKey(Object::from_static("__self__")))
                    .cloned();
                if name != "__self__" {
                    if let Some(receiver) = super_receiver {
                        if let Some(v) = inst.class.lookup(name) {
                            return Ok(self.maybe_bind(&receiver, v));
                        }
                        return Err(attribute_error(format!(
                            "'super' object has no attribute '{}'",
                            name
                        )));
                    }
                }
                if let Some(v) = inst.dict.borrow().get(&DictKey(Object::from_str(name))) {
                    return Ok(v.clone());
                }
                if let Some(v) = inst.class.lookup(name) {
                    return Ok(self.maybe_bind(obj, v));
                }
                Err(attribute_error(format!(
                    "'{}' object has no attribute '{}'",
                    inst.class.name, name
                )))
            }
            Object::Type(ty) => {
                if let Some(v) = ty.lookup(name) {
                    return Ok(v);
                }
                match name {
                    "__name__" => return Ok(Object::from_str(&ty.name)),
                    "__qualname__" => return Ok(Object::from_str(&ty.name)),
                    "__bases__" => {
                        let bases = ty.bases.iter().map(|b| Object::Type(b.clone())).collect();
                        return Ok(Object::new_tuple(bases));
                    }
                    "__mro__" => {
                        let mro = ty
                            .mro
                            .borrow()
                            .iter()
                            .map(|b| Object::Type(b.clone()))
                            .collect();
                        return Ok(Object::new_tuple(mro));
                    }
                    _ => {}
                }
                Err(attribute_error(format!(
                    "type object '{}' has no attribute '{}'",
                    ty.name, name
                )))
            }
            Object::Module(m) => {
                if let Some(v) = m.dict.borrow().get(&DictKey(Object::from_str(name))) {
                    return Ok(v.clone());
                }
                match name {
                    "__name__" => return Ok(Object::from_str(&m.name)),
                    "__file__" => {
                        return Ok(m.filename.as_deref().map_or(Object::None, Object::from_str));
                    }
                    "__dict__" => return Ok(Object::Dict(m.dict.clone())),
                    _ => {}
                }
                Err(attribute_error(format!(
                    "module '{}' has no attribute '{}'",
                    m.name, name
                )))
            }
            _ => {
                if let Some(method) = self.lookup_method(obj, name) {
                    return Ok(Object::BoundMethod(Rc::new(BoundMethod {
                        receiver: obj.clone(),
                        function: method,
                    })));
                }
                Err(attribute_error(format!(
                    "'{}' object has no attribute '{}'",
                    obj.type_name(),
                    name
                )))
            }
        }
    }

    fn maybe_bind(&self, receiver: &Object, attr: Object) -> Object {
        match &attr {
            Object::Function(_) | Object::Builtin(_) => Object::BoundMethod(Rc::new(BoundMethod {
                receiver: receiver.clone(),
                function: attr,
            })),
            _ => attr,
        }
    }

    fn lookup_method(&self, obj: &Object, name: &str) -> Option<Object> {
        builtins::lookup_method(obj, name)
    }

    /// `print(*args, sep=' ', end='\n', file=...)`. We honour `sep`
    /// and `end` from kwargs; `file` is ignored (always our stdout).
    fn do_print(
        &mut self,
        args: &[Object],
        kwargs: &[(String, Object)],
        globals: &Rc<RefCell<DictData>>,
    ) -> Result<Object, RuntimeError> {
        let mut sep = String::from(" ");
        let mut end = String::from("\n");
        for (k, v) in kwargs {
            match k.as_str() {
                "sep" => sep = v.to_str(),
                "end" => end = v.to_str(),
                "file" | "flush" => {}
                other => {
                    return Err(type_error(format!(
                        "'{other}' is an invalid keyword argument for print()"
                    )))
                }
            }
        }
        let mut sink = self.stdout.borrow_mut();
        for (i, a) in args.iter().enumerate() {
            if i > 0 {
                let _ = write!(sink, "{sep}");
            }
            drop(sink);
            let s = self.stringify(a, globals)?;
            sink = self.stdout.borrow_mut();
            let _ = write!(sink, "{s}");
        }
        let _ = write!(sink, "{end}");
        Ok(Object::None)
    }

    fn do_str_call(
        &mut self,
        v: &Object,
        globals: &Rc<RefCell<DictData>>,
    ) -> Result<Object, RuntimeError> {
        Ok(Object::from_str(self.stringify(v, globals)?))
    }

    fn do_repr_call(
        &mut self,
        v: &Object,
        globals: &Rc<RefCell<DictData>>,
    ) -> Result<Object, RuntimeError> {
        Ok(Object::from_str(self.repr_of(v, globals)?))
    }

    fn do_len_call(
        &mut self,
        v: &Object,
        globals: &Rc<RefCell<DictData>>,
    ) -> Result<Object, RuntimeError> {
        if let Some(method) = instance_method(v, "__len__") {
            let r = self.call(&method, &[], &[], globals)?;
            return match r {
                Object::Int(i) => Ok(Object::Int(i)),
                other => Err(type_error(format!(
                    "'__len__' should return int, not '{}'",
                    other.type_name()
                ))),
            };
        }
        Ok(Object::Int(v.len()? as i64))
    }

    /// Run `__str__` on instances, falling back to `__repr__` then
    /// the default. Built-in types use their existing `to_str`.
    fn stringify(
        &mut self,
        v: &Object,
        globals: &Rc<RefCell<DictData>>,
    ) -> Result<String, RuntimeError> {
        if let Object::Instance(_) = v {
            if let Some(method) = instance_method(v, "__str__") {
                let r = self.call(&method, &[], &[], globals)?;
                return Ok(r.to_str());
            }
            return self.repr_of(v, globals);
        }
        Ok(v.to_str())
    }

    fn repr_of(
        &mut self,
        v: &Object,
        globals: &Rc<RefCell<DictData>>,
    ) -> Result<String, RuntimeError> {
        if let Object::Instance(_) = v {
            if let Some(method) = instance_method(v, "__repr__") {
                let r = self.call(&method, &[], &[], globals)?;
                return Ok(r.to_str());
            }
        }
        Ok(v.repr())
    }

    /// Either build a native iterator (for built-ins) or call
    /// `__iter__` and return whatever the user method produced.
    fn make_iter(
        &mut self,
        v: &Object,
        globals: &Rc<RefCell<DictData>>,
    ) -> Result<Object, RuntimeError> {
        match v {
            Object::Instance(_) => {
                if let Some(method) = instance_method(v, "__iter__") {
                    return self.call(&method, &[], &[], globals);
                }
                Err(type_error(format!(
                    "'{}' object is not iterable",
                    v.type_name_owned()
                )))
            }
            _ => {
                let it = v.make_iter()?;
                Ok(Object::Iter(Rc::new(RefCell::new(it))))
            }
        }
    }

    fn dispatch_binary_op(
        &mut self,
        a: &Object,
        b: &Object,
        op: BinOpKind,
        globals: &Rc<RefCell<DictData>>,
    ) -> Result<Object, RuntimeError> {
        let (dunder, rdunder) = binop_dunders(op);
        // `a.__op__(b)` first, then `b.__rop__(a)` if it returns
        // NotImplemented. Our slice treats "no method" as
        // NotImplemented and the missing-symmetric falls through to
        // [`binary_op`] for built-in types.
        if let Some(method) = instance_method(a, dunder) {
            return self.call(&method, std::slice::from_ref(b), &[], globals);
        }
        if let Some(method) = instance_method(b, rdunder) {
            return self.call(&method, std::slice::from_ref(a), &[], globals);
        }
        binary_op(a, b, op)
    }

    fn dispatch_compare_op(
        &mut self,
        a: &Object,
        b: &Object,
        op: CompareKind,
        globals: &Rc<RefCell<DictData>>,
    ) -> Result<bool, RuntimeError> {
        let (dunder, swapped) = cmp_dunder(op);
        if let Some(method) = instance_method(a, dunder) {
            let r = self.call(&method, std::slice::from_ref(b), &[], globals)?;
            return Ok(r.is_truthy());
        }
        if let Some(method) = instance_method(b, swapped) {
            let r = self.call(&method, std::slice::from_ref(a), &[], globals)?;
            return Ok(r.is_truthy());
        }
        compare_op(a, b, op)
    }

    fn store_attr(&self, obj: &Object, name: &str, value: Object) -> Result<(), RuntimeError> {
        match obj {
            Object::Instance(inst) => {
                inst.dict
                    .borrow_mut()
                    .insert(DictKey(Object::from_str(name)), value);
                Ok(())
            }
            Object::Type(ty) => {
                if ty.flags.is_builtin {
                    return Err(type_error(format!(
                        "cannot set '{name}' attribute of immutable type '{}'",
                        ty.name
                    )));
                }
                ty.dict
                    .borrow_mut()
                    .insert(DictKey(Object::from_str(name)), value);
                Ok(())
            }
            Object::Module(m) => {
                m.dict
                    .borrow_mut()
                    .insert(DictKey(Object::from_str(name)), value);
                Ok(())
            }
            _ => Err(type_error(format!(
                "'{}' object has no attribute '{}'",
                obj.type_name(),
                name
            ))),
        }
    }

    fn delete_attr(&self, obj: &Object, name: &str) -> Result<(), RuntimeError> {
        match obj {
            Object::Instance(inst) => {
                if inst
                    .dict
                    .borrow_mut()
                    .shift_remove(&DictKey(Object::from_str(name)))
                    .is_none()
                {
                    return Err(attribute_error(format!(
                        "'{}' object has no attribute '{}'",
                        inst.class.name, name
                    )));
                }
                Ok(())
            }
            _ => Err(type_error(format!(
                "'{}' object has no attribute '{}'",
                obj.type_name(),
                name
            ))),
        }
    }

    fn binary_subscr(&self, container: &Object, index: &Object) -> Result<Object, RuntimeError> {
        match (container, index) {
            (Object::List(items), Object::Int(i)) => {
                let items = items.borrow();
                let idx = normalize_index(*i, items.len())?;
                Ok(items[idx].clone())
            }
            (Object::Tuple(items), Object::Int(i)) => {
                let idx = normalize_index(*i, items.len())?;
                Ok(items[idx].clone())
            }
            (Object::Str(s), Object::Int(i)) => {
                let chars: Vec<char> = s.chars().collect();
                let idx = normalize_index(*i, chars.len())?;
                Ok(Object::from_str(chars[idx].to_string()))
            }
            (Object::Dict(d), key) => {
                let d = d.borrow();
                d.get(&DictKey(key.clone()))
                    .cloned()
                    .ok_or_else(|| key_error(key.repr()))
            }
            (Object::List(items), Object::Slice(s)) => {
                let items = items.borrow();
                let sliced = slice_seq(&items, s)?;
                Ok(Object::new_list(sliced))
            }
            (Object::Tuple(items), Object::Slice(s)) => {
                let v: Vec<Object> = items.iter().cloned().collect();
                let sliced = slice_seq(&v, s)?;
                Ok(Object::new_tuple(sliced))
            }
            (Object::Str(s), Object::Slice(slc)) => {
                let chars: Vec<char> = s.chars().collect();
                let obj_chars: Vec<Object> = chars
                    .iter()
                    .map(|c| Object::from_str(c.to_string()))
                    .collect();
                let sliced = slice_seq(&obj_chars, slc)?;
                let s: String = sliced.iter().map(|o| o.to_str()).collect();
                Ok(Object::from_str(s))
            }
            (_, _) => Err(type_error(format!(
                "'{}' object is not subscriptable with '{}'",
                container.type_name(),
                index.type_name()
            ))),
        }
    }

    fn store_subscr(
        &self,
        container: &Object,
        index: &Object,
        value: Object,
    ) -> Result<(), RuntimeError> {
        match (container, index) {
            (Object::List(items), Object::Int(i)) => {
                let mut items = items.borrow_mut();
                let idx = normalize_index(*i, items.len())?;
                items[idx] = value;
                Ok(())
            }
            (Object::Dict(d), key) => {
                d.borrow_mut().insert(DictKey(key.clone()), value);
                Ok(())
            }
            _ => Err(type_error(format!(
                "'{}' object does not support item assignment",
                container.type_name()
            ))),
        }
    }

    fn delete_subscr(&self, container: &Object, index: &Object) -> Result<(), RuntimeError> {
        match (container, index) {
            (Object::List(items), Object::Int(i)) => {
                let mut items = items.borrow_mut();
                let idx = normalize_index(*i, items.len())?;
                items.remove(idx);
                Ok(())
            }
            (Object::Dict(d), key) => {
                if d.borrow_mut().shift_remove(&DictKey(key.clone())).is_none() {
                    return Err(key_error(key.repr()));
                }
                Ok(())
            }
            _ => Err(type_error(format!(
                "'{}' object does not support item deletion",
                container.type_name()
            ))),
        }
    }

    // ---------- calling ----------

    fn call(
        &mut self,
        callable: &Object,
        args: &[Object],
        kwargs: &[(String, Object)],
        outer_globals: &Rc<RefCell<DictData>>,
    ) -> Result<Object, RuntimeError> {
        let _ = outer_globals;
        match callable {
            Object::Builtin(b) => {
                if b.name == builtins::BUILD_CLASS_NAME {
                    return self.build_class(args, kwargs);
                }
                if b.name == "print" {
                    return self.do_print(args, kwargs, outer_globals);
                }
                if b.name == "str" && args.len() == 1 {
                    return self.do_str_call(&args[0], outer_globals);
                }
                if b.name == "repr" && args.len() == 1 {
                    return self.do_repr_call(&args[0], outer_globals);
                }
                if b.name == "len" && args.len() == 1 {
                    return self.do_len_call(&args[0], outer_globals);
                }
                if !kwargs.is_empty() {
                    return Err(type_error(format!(
                        "builtin '{}' does not accept keyword arguments",
                        b.name
                    )));
                }
                (b.call)(args)
            }
            Object::Function(f) => self.call_python(f, args, kwargs),
            Object::BoundMethod(bm) => {
                let mut combined: Vec<Object> = Vec::with_capacity(args.len() + 1);
                combined.push(bm.receiver.clone());
                combined.extend_from_slice(args);
                self.call(&bm.function, &combined, kwargs, outer_globals)
            }
            Object::Type(ty) => self.instantiate(ty.clone(), args, kwargs),
            Object::Instance(inst) => {
                // Honour __call__ if defined.
                if let Some(m) = inst.class.lookup("__call__") {
                    let bound = Object::BoundMethod(Rc::new(BoundMethod {
                        receiver: Object::Instance(inst.clone()),
                        function: m,
                    }));
                    self.call(&bound, args, kwargs, outer_globals)
                } else {
                    Err(type_error(format!(
                        "'{}' object is not callable",
                        inst.class.name
                    )))
                }
            }
            _ => Err(type_error(format!(
                "'{}' object is not callable",
                callable.type_name()
            ))),
        }
    }

    /// Run a `class` statement.
    ///
    /// `args[0]` is the class body function, `args[1]` is the class
    /// name, and the rest are explicit bases. Keyword arguments carry
    /// `metaclass=` and similar (unsupported here).
    fn build_class(
        &mut self,
        args: &[Object],
        _kwargs: &[(String, Object)],
    ) -> Result<Object, RuntimeError> {
        if args.len() < 2 {
            return Err(type_error("__build_class__ takes at least 2 args"));
        }
        let body_fn = match &args[0] {
            Object::Function(f) => f.clone(),
            _ => return Err(type_error("__build_class__ arg 1 must be a function")),
        };
        let name = match &args[1] {
            Object::Str(s) => s.to_string(),
            _ => return Err(type_error("__build_class__ arg 2 must be a str")),
        };
        let mut bases: Vec<Rc<TypeObject>> = Vec::new();
        for b in &args[2..] {
            match b {
                Object::Type(t) => bases.push(t.clone()),
                other => {
                    return Err(type_error(format!(
                        "base of class '{}' must be a class, got '{}'",
                        name,
                        other.type_name()
                    )));
                }
            }
        }
        if bases.is_empty() {
            bases.push(builtin_types().object_.clone());
        }
        let class_ns = Rc::new(RefCell::new(DictData::new()));
        {
            let mut ns = class_ns.borrow_mut();
            ns.insert(
                DictKey(Object::from_static("__name__")),
                Object::from_str(&name),
            );
            ns.insert(
                DictKey(Object::from_static("__qualname__")),
                Object::from_str(&name),
            );
        }
        // Build a frame for the class body. Locals are unused; names
        // store and load through `class_ns`. The body's `__class__`
        // cell is captured so methods can reference it.
        let code = body_fn.code.clone();
        let mut frame = self.make_frame(
            code,
            Vec::new(),
            body_fn.closure.clone(),
            body_fn.globals.clone(),
            false,
        );
        frame.class_namespace = Some(class_ns.clone());
        let _ = self.run_frame(&mut frame)?;
        let dict = class_ns.borrow().clone();
        let ty = TypeObject::new_user(&name, bases, dict)?;
        // If the body produced a `__class__` cell (because a method
        // references super or __class__), point it at the new type.
        for (i, cell_name) in body_fn.code.cellvars.iter().enumerate() {
            if cell_name == "__class__" {
                if let Some(cell) = frame.cells.get(i) {
                    *cell.borrow_mut() = Object::Type(ty.clone());
                }
            }
        }
        Ok(Object::Type(ty))
    }

    /// Allocate an instance of `cls` and call `__init__` if defined.
    fn instantiate(
        &mut self,
        cls: Rc<TypeObject>,
        args: &[Object],
        kwargs: &[(String, Object)],
    ) -> Result<Object, RuntimeError> {
        // Built-in conversion types route to the underlying builtin
        // function so `int("3")`, `range(5)`, `list(xs)` keep working.
        if cls.flags.is_builtin {
            if let Some(builtin) = self.builtin_constructor_for(&cls) {
                if !kwargs.is_empty() {
                    return Err(type_error(format!(
                        "{}() does not accept keyword arguments",
                        cls.name
                    )));
                }
                return (builtin.call)(args);
            }
            if cls.flags.is_exception {
                return Ok(self.build_exception_instance(cls, args));
            }
        }
        let instance = Object::Instance(Rc::new(PyInstance::new(cls.clone())));
        if let Some(init) = cls.lookup("__init__") {
            let bound = Object::BoundMethod(Rc::new(BoundMethod {
                receiver: instance.clone(),
                function: init,
            }));
            let result = self.call(
                &bound,
                args,
                kwargs,
                &Rc::new(RefCell::new(DictData::new())),
            )?;
            if !matches!(result, Object::None) {
                return Err(type_error(format!(
                    "__init__() should return None, not '{}'",
                    result.type_name()
                )));
            }
        } else if !args.is_empty() || !kwargs.is_empty() {
            return Err(type_error(format!("{}() takes no arguments", cls.name)));
        }
        Ok(instance)
    }

    /// Construct a built-in exception instance carrying `args` as the
    /// canonical Python `.args` tuple. Used by both `raise` and
    /// explicit `ExceptionClass(...)` calls.
    fn build_exception_instance(&self, cls: Rc<TypeObject>, args: &[Object]) -> Object {
        let inst = PyInstance::new(cls);
        let args_tuple = Object::new_tuple(args.to_vec());
        let mut dict = inst.dict.borrow_mut();
        dict.insert(DictKey(Object::from_static("args")), args_tuple);
        if let Some(first) = args.first() {
            dict.insert(DictKey(Object::from_static("message")), first.clone());
        }
        drop(dict);
        Object::Instance(Rc::new(inst))
    }

    /// Look up the existing built-in callable that mirrors `cls`'s
    /// constructor — `int`, `range`, `list`, etc.
    fn builtin_constructor_for(&self, cls: &TypeObject) -> Option<Rc<BuiltinFn>> {
        let key = DictKey(Object::from_str(&cls.name));
        match self.builtins.borrow().get(&key).cloned() {
            Some(Object::Builtin(b)) => Some(b),
            _ => None,
        }
    }

    fn call_python(
        &mut self,
        f: &Rc<PyFunction>,
        args: &[Object],
        kwargs: &[(String, Object)],
    ) -> Result<Object, RuntimeError> {
        let code = f.code.clone();
        let total_args = code.arg_count as usize;
        let has_varargs = code.has_varargs;
        // Bind positional args; remainder go to *args if present, else error.
        let mut positional: Vec<Object> = vec![Object::None; code.varnames.len()];
        let mut filled = vec![false; code.varnames.len()];
        let provided = args.len();
        let direct = provided.min(total_args);
        for (i, v) in args.iter().take(direct).enumerate() {
            positional[i] = v.clone();
            filled[i] = true;
        }
        if has_varargs {
            let star_idx = total_args;
            let rest: Vec<Object> = args.iter().skip(direct).cloned().collect();
            positional[star_idx] = Object::new_tuple(rest);
            filled[star_idx] = true;
        } else if provided > total_args {
            return Err(type_error(format!(
                "{}() takes {} positional arguments but {} were given",
                f.name, total_args, provided
            )));
        }
        // Apply defaults for any positional arg slot that wasn't filled.
        if filled.iter().take(total_args).any(|x| !x) {
            let needed = total_args;
            let first_default = needed.saturating_sub(f.defaults.len());
            for (i, d) in f.defaults.iter().enumerate() {
                let slot = first_default + i;
                if slot < needed && !filled[slot] {
                    positional[slot] = d.clone();
                    filled[slot] = true;
                }
            }
        }
        // Keyword args: match by name, error on unknown.
        for (name, value) in kwargs {
            let Some(slot) = code.varnames.iter().position(|n| n == name) else {
                return Err(type_error(format!(
                    "{}() got an unexpected keyword argument '{}'",
                    f.name, name
                )));
            };
            if filled[slot] {
                return Err(type_error(format!(
                    "{}() got multiple values for argument '{}'",
                    f.name, name
                )));
            }
            positional[slot] = value.clone();
            filled[slot] = true;
        }
        for (i, was_filled) in filled.iter().take(total_args).enumerate() {
            if !was_filled {
                return Err(type_error(format!(
                    "{}() missing required argument: '{}'",
                    f.name, code.varnames[i]
                )));
            }
        }
        let mut frame = self.make_frame(
            code,
            positional,
            f.closure.clone(),
            f.globals.clone(),
            false,
        );
        self.run_frame(&mut frame)
    }

    // ---------- imports (RFC 0012) ----------

    /// `IMPORT_NAME` runtime side. Resolves relative imports against
    /// the current frame's `__package__`/`__name__`, walks dotted
    /// names, and returns either the top-level package (when
    /// `fromlist` is empty/None) or the leaf module (otherwise).
    fn do_import(
        &mut self,
        name: &str,
        fromlist: &Object,
        level: u32,
        current_globals: &Rc<RefCell<DictData>>,
    ) -> Result<Object, RuntimeError> {
        let package = current_package(current_globals);
        let absolute = resolve_relative(package.as_deref(), name, level).map_err(import_error)?;
        let leaf = self.import_path(&absolute)?;

        // CPython: with no fromlist, return the top-level package.
        // Otherwise return the leaf module — and pre-load any items
        // listed in `fromlist` that look like submodules (so that
        // `from pkg import sub` triggers loading of `pkg.sub`).
        let want_leaf = !matches!(fromlist, Object::None);
        if !want_leaf {
            let top_name = absolute.split('.').next().unwrap_or(&absolute);
            return self
                .cache
                .get(top_name)
                .ok_or_else(|| module_not_found_error(format!("import of '{top_name}' failed")));
        }
        if let Object::Tuple(items) = fromlist {
            for item in items.iter() {
                if let Object::Str(s) = item {
                    if s.as_ref() == "*" {
                        continue;
                    }
                    let sub_name = format!("{absolute}.{s}");
                    let _ = self.import_path(&sub_name);
                }
            }
        }
        Ok(leaf)
    }

    /// Walk a dotted name (`a.b.c`), loading each part lazily and
    /// linking submodules into their parents' dicts. Returns the
    /// leaf module.
    fn import_path(&mut self, full: &str) -> Result<Object, RuntimeError> {
        let parts: Vec<&str> = full.split('.').collect();
        let mut so_far = String::new();
        let mut current: Option<Object> = None;
        for (i, part) in parts.iter().enumerate() {
            if i > 0 {
                so_far.push('.');
            }
            so_far.push_str(part);
            let module = self.load_one(&so_far)?;
            if let Some(Object::Module(parent_mod)) = current.as_ref() {
                parent_mod
                    .dict
                    .borrow_mut()
                    .insert(DictKey(Object::from_str(*part)), module.clone());
            }
            current = Some(module);
        }
        current.ok_or_else(|| import_error("empty module name"))
    }

    /// Load a single fully-qualified module name. Honours the cache
    /// first, then the built-in registry, then the filesystem.
    fn load_one(&mut self, full: &str) -> Result<Object, RuntimeError> {
        if let Some(cached) = self.cache.get(full) {
            return Ok(cached);
        }
        if let Some(factory) = self.cache.builtin_factory(full) {
            let module = factory(&self.cache);
            let obj = Object::Module(module);
            self.cache.insert(full, obj.clone());
            return Ok(obj);
        }
        let (path, is_package) = self
            .cache
            .find_source(full)
            .ok_or_else(|| module_not_found_error(format!("No module named '{full}'")))?;
        self.load_from_file(full, &path, is_package)
    }

    /// Read, parse, compile, and execute the module's source.
    /// The module is inserted into `sys.modules` *before* its body
    /// runs so that circular imports observe a partially-initialised
    /// module instead of looping.
    fn load_from_file(
        &mut self,
        full: &str,
        path: &Path,
        is_package: bool,
    ) -> Result<Object, RuntimeError> {
        let source = std::fs::read_to_string(path)
            .map_err(|e| import_error(format!("failed to read '{}': {e}", path.display())))?;
        let module = weavepy_parser::parse_module(&source)
            .map_err(|e| import_error(format!("parse error in '{}': {e}", path.display())))?;
        let filename = path.to_string_lossy().into_owned();
        let code = weavepy_compiler::compile_module_with_source(&module, &source, &filename)
            .map_err(|e| import_error(format!("compile error in '{}': {e}", path.display())))?;

        // Build the module value first and seed sys.modules so that
        // circular imports see a stub.
        let package = if is_package {
            full.to_owned()
        } else {
            full.rsplit_once('.')
                .map_or(String::new(), |(p, _)| p.to_owned())
        };
        let pkg_for_globals = if package.is_empty() {
            None
        } else {
            Some(package.as_str())
        };
        let globals = self.build_module_globals(full, Some(&filename), pkg_for_globals);
        if is_package {
            globals.borrow_mut().insert(
                DictKey(Object::from_static("__path__")),
                package_search_path(path),
            );
        }
        let module_obj = Object::Module(Rc::new(PyModule {
            name: full.to_owned(),
            filename: Some(filename.clone()),
            dict: globals.clone(),
        }));
        self.cache.insert(full, module_obj.clone());

        // Run the body. On failure, drop the partial module so a
        // subsequent retry can try again from scratch.
        let code_rc = Rc::new(code);
        let mut frame = self.make_frame(code_rc, Vec::new(), Vec::new(), globals, true);
        if let Err(e) = self.run_frame(&mut frame) {
            self.cache.remove(full);
            return Err(e);
        }
        Ok(module_obj)
    }

    /// `IMPORT_FROM` runtime side. Looks up `name` on the module on
    /// top of the stack, returning the attribute or
    /// `ImportError("cannot import name 'name' from 'module'")`.
    fn import_from(&mut self, module: &Object, name: &str) -> Result<Object, RuntimeError> {
        match module {
            Object::Module(m) => {
                if let Some(v) = m.dict.borrow().get(&DictKey(Object::from_str(name))) {
                    return Ok(v.clone());
                }
                // Submodule that we deferred loading: try loading it
                // on demand. Matches CPython's `_handle_fromlist`.
                let candidate = format!("{}.{}", m.name, name);
                if self.cache.get(&candidate).is_some()
                    || self.cache.builtin_factory(&candidate).is_some()
                    || self.cache.find_source(&candidate).is_some()
                {
                    let sub = self.load_one(&candidate)?;
                    m.dict
                        .borrow_mut()
                        .insert(DictKey(Object::from_str(name)), sub.clone());
                    return Ok(sub);
                }
                Err(import_error(format!(
                    "cannot import name '{name}' from '{}'",
                    m.name
                )))
            }
            other => Err(type_error(format!(
                "IMPORT_FROM on non-module: '{}'",
                other.type_name()
            ))),
        }
    }

    /// `IMPORT_STAR` runtime side. Copies every public name from the
    /// module into the current globals. Honours `__all__` if defined.
    fn import_star(
        &mut self,
        module: &Object,
        globals: &Rc<RefCell<DictData>>,
    ) -> Result<(), RuntimeError> {
        let m = match module {
            Object::Module(m) => m,
            other => {
                return Err(type_error(format!(
                    "IMPORT_STAR on non-module: '{}'",
                    other.type_name()
                )))
            }
        };
        let dict = m.dict.borrow();
        if let Some(Object::List(all_list)) = dict.get(&DictKey(Object::from_static("__all__"))) {
            let names: Vec<String> = all_list
                .borrow()
                .iter()
                .filter_map(|o| match o {
                    Object::Str(s) => Some(s.to_string()),
                    _ => None,
                })
                .collect();
            let mut g = globals.borrow_mut();
            for n in names {
                if let Some(v) = dict.get(&DictKey(Object::from_str(&n))) {
                    g.insert(DictKey(Object::from_str(n)), v.clone());
                }
            }
            return Ok(());
        }
        let mut g = globals.borrow_mut();
        for (k, v) in dict.iter() {
            let name = match &k.0 {
                Object::Str(s) => s.to_string(),
                _ => continue,
            };
            if name.starts_with('_') {
                continue;
            }
            g.insert(DictKey(Object::from_str(name)), v.clone());
        }
        Ok(())
    }
}

/// Read the current module's `__package__` (or fall back to
/// `__name__`'s parent) so relative imports can resolve.
fn current_package(globals: &Rc<RefCell<DictData>>) -> Option<String> {
    let dict = globals.borrow();
    if let Some(Object::Str(p)) = dict.get(&DictKey(Object::from_static("__package__"))) {
        let s = p.to_string();
        if !s.is_empty() {
            return Some(s);
        }
    }
    if let Some(Object::Str(n)) = dict.get(&DictKey(Object::from_static("__name__"))) {
        let s = n.to_string();
        if let Some((parent, _)) = s.rsplit_once('.') {
            return Some(parent.to_owned());
        }
    }
    None
}

fn slice_seq(seq: &[Object], s: &PySlice) -> Result<Vec<Object>, RuntimeError> {
    let len = seq.len() as i64;
    let step = match &s.step {
        Object::None => 1i64,
        Object::Int(i) => *i,
        _ => {
            return Err(type_error(
                "slice indices must be integers or None or have an __index__ method",
            ))
        }
    };
    if step == 0 {
        return Err(value_error("slice step cannot be zero"));
    }
    let extract = |o: &Object, default: i64| -> Result<i64, RuntimeError> {
        match o {
            Object::None => Ok(default),
            Object::Int(i) => Ok(*i),
            _ => Err(type_error(
                "slice indices must be integers or None or have an __index__ method",
            )),
        }
    };
    let start = extract(&s.start, if step > 0 { 0 } else { len - 1 })?;
    let stop = extract(&s.stop, if step > 0 { len } else { -1 })?;
    let norm = |x: i64| -> i64 {
        if x < 0 {
            let n = x + len;
            if n < 0 && step > 0 {
                0
            } else {
                n
            }
        } else if x > len {
            len
        } else {
            x
        }
    };
    let mut i = norm(start);
    let stop_norm = norm(stop);
    let mut out = Vec::new();
    if step > 0 {
        while i < stop_norm {
            if (0..len).contains(&i) {
                out.push(seq[i as usize].clone());
            }
            i += step;
        }
    } else {
        while i > stop_norm {
            if (0..len).contains(&i) {
                out.push(seq[i as usize].clone());
            }
            i += step;
        }
    }
    Ok(out)
}

fn path_contains(path: &[Object], needle: &str) -> bool {
    path.iter()
        .any(|o| matches!(o, Object::Str(s) if s.as_ref() == needle))
}

fn normalize_index(i: i64, len: usize) -> Result<usize, RuntimeError> {
    let n = len as i64;
    let idx = if i < 0 { i + n } else { i };
    if idx < 0 || idx >= n {
        return Err(index_error("index out of range"));
    }
    Ok(idx as usize)
}

/// Outcome of executing a single instruction.
enum StepOutcome {
    Continue,
    Return(Object),
}

/// If `obj` is an instance and its class defines `name`, return the
/// bound method. Used by dunder dispatch to avoid the full
/// `load_attr` codepath (and the AttributeError if missing).
fn instance_method(obj: &Object, name: &str) -> Option<Object> {
    let inst = match obj {
        Object::Instance(i) => i.clone(),
        _ => return None,
    };
    let m = inst.class.lookup(name)?;
    Some(Object::BoundMethod(Rc::new(BoundMethod {
        receiver: Object::Instance(inst),
        function: m,
    })))
}

fn binop_dunders(op: BinOpKind) -> (&'static str, &'static str) {
    use BinOpKind as B;
    match op {
        B::Add => ("__add__", "__radd__"),
        B::Sub => ("__sub__", "__rsub__"),
        B::Mult => ("__mul__", "__rmul__"),
        B::MatMult => ("__matmul__", "__rmatmul__"),
        B::Div => ("__truediv__", "__rtruediv__"),
        B::FloorDiv => ("__floordiv__", "__rfloordiv__"),
        B::Mod => ("__mod__", "__rmod__"),
        B::Pow => ("__pow__", "__rpow__"),
        B::LShift => ("__lshift__", "__rlshift__"),
        B::RShift => ("__rshift__", "__rrshift__"),
        B::BitOr => ("__or__", "__ror__"),
        B::BitXor => ("__xor__", "__rxor__"),
        B::BitAnd => ("__and__", "__rand__"),
    }
}

fn cmp_dunder(op: CompareKind) -> (&'static str, &'static str) {
    use CompareKind as C;
    match op {
        C::Eq => ("__eq__", "__eq__"),
        C::NotEq => ("__ne__", "__ne__"),
        C::Lt => ("__lt__", "__gt__"),
        C::LtE => ("__le__", "__ge__"),
        C::Gt => ("__gt__", "__lt__"),
        C::GtE => ("__ge__", "__le__"),
    }
}

fn find_handler(table: &[ExcHandler], pc: u32) -> Option<&ExcHandler> {
    // Innermost-first: nested `compile_try` calls push the inner
    // entry before the outer one, so a forward scan finds the
    // tightest range first.
    table.iter().find(|h| pc >= h.start && pc < h.end)
}

fn is_super_callable(obj: &Object) -> bool {
    matches!(obj, Object::Builtin(b) if b.name == "super")
}

fn find_cell(frame: &Frame, name: &str) -> Option<Rc<RefCell<Object>>> {
    let cells = &frame.cells;
    let cellvars = &frame.code.cellvars;
    let freevars = &frame.code.freevars;
    cellvars
        .iter()
        .position(|n| n == name)
        .or_else(|| {
            freevars
                .iter()
                .position(|n| n == name)
                .map(|i| i + cellvars.len())
        })
        .and_then(|idx| cells.get(idx).cloned())
}

fn is_param(code: &CodeObject, name: &str) -> bool {
    let total = (code.arg_count + code.kwonly_count) as usize
        + usize::from(code.has_varargs)
        + usize::from(code.has_varkeywords);
    code.varnames.iter().take(total).any(|n| n == name)
}

// ---------- arithmetic ----------

fn constant_to_object(c: Constant) -> Object {
    match c {
        Constant::None => Object::None,
        Constant::Bool(b) => Object::Bool(b),
        Constant::Int(i) => Object::Int(i),
        Constant::Float(f) => Object::Float(f),
        Constant::Str(s) => Object::from_str(s),
        Constant::Bytes(_) => Object::None,
        Constant::Tuple(xs) => Object::new_tuple(xs.into_iter().map(constant_to_object).collect()),
        Constant::Code(c) => Object::Code(Rc::from(*c)),
        Constant::Ellipsis => Object::None,
    }
}

fn binary_op(a: &Object, b: &Object, op: BinOpKind) -> Result<Object, RuntimeError> {
    use BinOpKind as B;
    use Object as O;
    // Promote bool → int where appropriate.
    let (a, b) = (promote_bool(a), promote_bool(b));
    match (&a, &b, op) {
        (O::Int(x), O::Int(y), B::Add) => Ok(O::Int(x.wrapping_add(*y))),
        (O::Int(x), O::Int(y), B::Sub) => Ok(O::Int(x.wrapping_sub(*y))),
        (O::Int(x), O::Int(y), B::Mult) => Ok(O::Int(x.wrapping_mul(*y))),
        (O::Int(x), O::Int(y), B::Div) => {
            if *y == 0 {
                Err(zero_division_error("division by zero"))
            } else {
                Ok(O::Float(*x as f64 / *y as f64))
            }
        }
        (O::Int(x), O::Int(y), B::FloorDiv) => {
            if *y == 0 {
                return Err(zero_division_error("integer division or modulo by zero"));
            }
            // Python's `//` floors toward -∞. Rust's `/` truncates
            // toward 0, so we adjust when the remainder is non-zero
            // and the operand signs disagree.
            let q = x / y;
            let r = x % y;
            let adjusted = if r != 0 && ((r < 0) != (*y < 0)) {
                q - 1
            } else {
                q
            };
            Ok(O::Int(adjusted))
        }
        (O::Int(x), O::Int(y), B::Mod) => {
            if *y == 0 {
                return Err(zero_division_error("integer division or modulo by zero"));
            }
            // Python's `%` has the sign of the divisor.
            let r = x % y;
            let adjusted = if r != 0 && ((r < 0) != (*y < 0)) {
                r + *y
            } else {
                r
            };
            Ok(O::Int(adjusted))
        }
        (O::Int(x), O::Int(y), B::Pow) => {
            if *y < 0 {
                Ok(O::Float((*x as f64).powf(*y as f64)))
            } else {
                Ok(O::Int(x.pow(*y as u32)))
            }
        }
        (O::Int(x), O::Int(y), B::LShift) => Ok(O::Int(x.wrapping_shl(*y as u32))),
        (O::Int(x), O::Int(y), B::RShift) => Ok(O::Int(x.wrapping_shr(*y as u32))),
        (O::Int(x), O::Int(y), B::BitOr) => Ok(O::Int(x | y)),
        (O::Int(x), O::Int(y), B::BitXor) => Ok(O::Int(x ^ y)),
        (O::Int(x), O::Int(y), B::BitAnd) => Ok(O::Int(x & y)),

        (O::Float(x), O::Float(y), B::Add) => Ok(O::Float(x + y)),
        (O::Float(x), O::Float(y), B::Sub) => Ok(O::Float(x - y)),
        (O::Float(x), O::Float(y), B::Mult) => Ok(O::Float(x * y)),
        (O::Float(x), O::Float(y), B::Div) => {
            if *y == 0.0 {
                Err(zero_division_error("float division by zero"))
            } else {
                Ok(O::Float(x / y))
            }
        }
        (O::Float(x), O::Float(y), B::Mod) => Ok(O::Float(x.rem_euclid(*y))),
        (O::Float(x), O::Float(y), B::Pow) => Ok(O::Float(x.powf(*y))),
        (O::Float(x), O::Float(y), B::FloorDiv) => Ok(O::Float((x / y).floor())),

        (O::Int(x), O::Float(y), op) => binary_op(&O::Float(*x as f64), &O::Float(*y), op),
        (O::Float(x), O::Int(y), op) => binary_op(&O::Float(*x), &O::Float(*y as f64), op),

        (O::Str(x), O::Str(y), B::Add) => {
            let mut out = String::with_capacity(x.len() + y.len());
            out.push_str(x);
            out.push_str(y);
            Ok(Object::from_str(out))
        }
        (O::Str(x), O::Int(n), B::Mult) | (O::Int(n), O::Str(x), B::Mult) => {
            let times = if *n < 0 { 0 } else { *n as usize };
            let mut out = String::with_capacity(x.len() * times);
            for _ in 0..times {
                out.push_str(x);
            }
            Ok(Object::from_str(out))
        }

        (O::List(x), O::List(y), B::Add) => {
            let mut out = x.borrow().clone();
            out.extend(y.borrow().iter().cloned());
            Ok(Object::new_list(out))
        }
        (O::List(x), O::Int(n), B::Mult) | (O::Int(n), O::List(x), B::Mult) => {
            let times = if *n < 0 { 0 } else { *n as usize };
            let body = x.borrow().clone();
            let mut out = Vec::with_capacity(body.len() * times);
            for _ in 0..times {
                out.extend(body.iter().cloned());
            }
            Ok(Object::new_list(out))
        }
        (O::Tuple(x), O::Tuple(y), B::Add) => {
            let mut out: Vec<Object> = x.iter().cloned().collect();
            out.extend(y.iter().cloned());
            Ok(Object::new_tuple(out))
        }

        _ => Err(type_error(format!(
            "unsupported operand type(s) for {}: '{}' and '{}'",
            op.as_str(),
            a.type_name(),
            b.type_name()
        ))),
    }
}

fn unary_op(v: &Object, op: UnaryKind) -> Result<Object, RuntimeError> {
    use Object as O;
    match (op, v) {
        (UnaryKind::Pos, O::Int(i)) => Ok(O::Int(*i)),
        (UnaryKind::Neg, O::Int(i)) => Ok(O::Int(-i)),
        (UnaryKind::Pos, O::Float(f)) => Ok(O::Float(*f)),
        (UnaryKind::Neg, O::Float(f)) => Ok(O::Float(-f)),
        (UnaryKind::Pos, O::Bool(b)) => Ok(O::Int(i64::from(*b))),
        (UnaryKind::Neg, O::Bool(b)) => Ok(O::Int(-i64::from(*b))),
        (UnaryKind::Invert, O::Int(i)) => Ok(O::Int(!i)),
        (UnaryKind::Invert, O::Bool(b)) => Ok(O::Int(!i64::from(*b))),
        (UnaryKind::Not, x) => Ok(O::Bool(!x.is_truthy())),
        _ => Err(type_error(format!(
            "bad operand type for unary {}: '{}'",
            op.as_str(),
            v.type_name()
        ))),
    }
}

fn compare_op(a: &Object, b: &Object, op: CompareKind) -> Result<bool, RuntimeError> {
    match op {
        CompareKind::Eq => Ok(a.eq_value(b)),
        CompareKind::NotEq => Ok(!a.eq_value(b)),
        CompareKind::Lt => Ok(a.cmp(b)?.is_lt()),
        CompareKind::LtE => Ok(a.cmp(b)?.is_le()),
        CompareKind::Gt => Ok(a.cmp(b)?.is_gt()),
        CompareKind::GtE => Ok(a.cmp(b)?.is_ge()),
    }
}

fn promote_bool(o: &Object) -> Object {
    match o {
        Object::Bool(b) => Object::Int(i64::from(*b)),
        other => other.clone(),
    }
}

// ---------- public re-exports ----------

pub use object::Object as Value;

#[cfg(test)]
mod tests {
    use super::*;
    use weavepy_compiler::compile_module;
    use weavepy_parser::parse_module;

    fn run(src: &str) -> String {
        let module = parse_module(src).expect("parse");
        let code = compile_module(&module).expect("compile");
        let mut interp = Interpreter::new();
        let buf: Rc<RefCell<Vec<u8>>> = Rc::new(RefCell::new(Vec::new()));
        let writer: Stdout = buf.clone() as Rc<RefCell<dyn Write>>;
        interp.set_stdout(writer);
        interp.run_module(&code).expect("run");
        let bytes = buf.borrow().clone();
        String::from_utf8(bytes).expect("utf-8")
    }

    #[test]
    fn runs_print_int() {
        assert_eq!(run("print(42)\n"), "42\n");
    }

    #[test]
    fn arithmetic_precedence() {
        assert_eq!(run("print(1 + 2 * 3)\n"), "7\n");
    }

    #[test]
    fn variable_assignment() {
        assert_eq!(run("x = 10\ny = 20\nprint(x + y)\n"), "30\n");
    }

    #[test]
    fn if_else() {
        assert_eq!(
            run("x = 5\nif x > 0:\n    print('positive')\nelse:\n    print('np')\n"),
            "positive\n"
        );
    }

    #[test]
    fn while_loop() {
        assert_eq!(
            run("i = 0\nwhile i < 3:\n    print(i)\n    i = i + 1\n"),
            "0\n1\n2\n"
        );
    }

    #[test]
    fn for_loop_range() {
        assert_eq!(run("for i in range(3):\n    print(i)\n"), "0\n1\n2\n");
    }

    #[test]
    fn function_call() {
        let src = "def add(a, b):\n    return a + b\nprint(add(2, 3))\n";
        assert_eq!(run(src), "5\n");
    }

    #[test]
    fn closure() {
        let src = "def make_adder(x):\n    def add(y):\n        return x + y\n    return add\nadd5 = make_adder(5)\nprint(add5(3))\n";
        assert_eq!(run(src), "8\n");
    }

    #[test]
    fn list_comprehension() {
        let src = "xs = [x * x for x in range(4)]\nprint(xs)\n";
        assert_eq!(run(src), "[0, 1, 4, 9]\n");
    }

    #[test]
    fn fibonacci() {
        let src = "def fib(n):\n    if n < 2:\n        return n\n    return fib(n - 1) + fib(n - 2)\nprint(fib(10))\n";
        assert_eq!(run(src), "55\n");
    }

    #[test]
    fn simple_class() {
        let src = "class Greeter:\n    def __init__(self, name):\n        self.name = name\n    def hello(self):\n        return 'hello, ' + self.name\ng = Greeter('Owen')\nprint(g.hello())\n";
        assert_eq!(run(src), "hello, Owen\n");
    }

    #[test]
    fn class_attribute_default() {
        let src = "class C:\n    x = 1\nc = C()\nprint(c.x)\n";
        assert_eq!(run(src), "1\n");
    }

    #[test]
    fn single_inheritance() {
        let src = "class Animal:\n    def speak(self):\n        return 'generic'\nclass Dog(Animal):\n    def speak(self):\n        return 'woof'\nprint(Dog().speak())\nprint(Animal().speak())\n";
        assert_eq!(run(src), "woof\ngeneric\n");
    }

    #[test]
    fn isinstance_with_class() {
        let src = "class A: pass\nclass B(A): pass\nb = B()\nprint(isinstance(b, A))\nprint(isinstance(b, B))\nprint(isinstance(1, int))\n";
        assert_eq!(run(src), "True\nTrue\nTrue\n");
    }

    #[test]
    fn try_except_catches() {
        let src = "try:\n    raise ValueError('boom')\nexcept ValueError as e:\n    print('caught', e.args[0])\n";
        assert_eq!(run(src), "caught boom\n");
    }

    #[test]
    fn try_finally_runs() {
        let src = "x = 0\ntry:\n    x = 1\nfinally:\n    x = x + 10\nprint(x)\n";
        assert_eq!(run(src), "11\n");
    }

    #[test]
    fn raise_propagates_from_function() {
        let src = "def boom():\n    raise ValueError('nope')\ntry:\n    boom()\nexcept ValueError:\n    print('handled')\n";
        assert_eq!(run(src), "handled\n");
    }

    #[test]
    fn with_statement_calls_exit() {
        let src = "class CM:\n    def __enter__(self):\n        print('enter')\n        return self\n    def __exit__(self, t, v, tb):\n        print('exit')\nwith CM() as c:\n    print('body')\n";
        assert_eq!(run(src), "enter\nbody\nexit\n");
    }

    #[test]
    fn except_does_not_match_other() {
        let src = "try:\n    raise KeyError('k')\nexcept ValueError:\n    print('value')\nexcept KeyError:\n    print('key')\n";
        assert_eq!(run(src), "key\n");
    }

    #[test]
    fn dunder_add_dispatch() {
        let src = "class Vec:\n    def __init__(self, x):\n        self.x = x\n    def __add__(self, other):\n        return Vec(self.x + other.x)\nv = Vec(2) + Vec(3)\nprint(v.x)\n";
        assert_eq!(run(src), "5\n");
    }

    #[test]
    fn dunder_repr_used_by_print() {
        let src = "class P:\n    def __repr__(self):\n        return 'P!'\nprint(P())\n";
        assert_eq!(run(src), "P!\n");
    }

    #[test]
    fn dunder_str_overrides_repr() {
        let src = concat!(
            "class P:\n",
            "    def __repr__(self):\n",
            "        return 'P-repr'\n",
            "    def __str__(self):\n",
            "        return 'P-str'\n",
            "print(P())\n",
            "print(repr(P()))\n"
        );
        assert_eq!(run(src), "P-str\nP-repr\n");
    }

    #[test]
    fn dunder_len_dispatch() {
        let src = concat!(
            "class C:\n",
            "    def __len__(self):\n",
            "        return 7\n",
            "print(len(C()))\n"
        );
        assert_eq!(run(src), "7\n");
    }

    #[test]
    fn super_two_arg_form() {
        let src = concat!(
            "class A:\n",
            "    def hello(self):\n",
            "        return 'A'\n",
            "class B(A):\n",
            "    def hello(self):\n",
            "        return 'B-' + super(B, self).hello()\n",
            "print(B().hello())\n"
        );
        assert_eq!(run(src), "B-A\n");
    }

    #[test]
    fn nested_try_except() {
        let src = concat!(
            "try:\n",
            "    try:\n",
            "        raise ValueError('inner')\n",
            "    except ValueError:\n",
            "        print('caught inner')\n",
            "        raise RuntimeError('outer')\n",
            "except RuntimeError as r:\n",
            "    print('caught outer', r.args[0])\n"
        );
        assert_eq!(run(src), "caught inner\ncaught outer outer\n");
    }

    #[test]
    fn raise_from_chains_cause() {
        let src = concat!(
            "try:\n",
            "    try:\n",
            "        raise ValueError('inner')\n",
            "    except ValueError as e:\n",
            "        raise RuntimeError('outer') from e\n",
            "except RuntimeError as r:\n",
            "    print(type(r).__name__)\n",
            "    print(r.args[0])\n"
        );
        assert_eq!(run(src), "RuntimeError\nouter\n");
    }

    #[test]
    fn imports_math_module() {
        let src = concat!(
            "import math\n",
            "print(math.floor(3.7))\n",
            "print(int(math.sqrt(9)))\n",
        );
        assert_eq!(run(src), "3\n3\n");
    }

    #[test]
    fn from_import_binds_names() {
        let src = concat!(
            "from math import floor, gcd\n",
            "print(floor(2.9))\n",
            "print(gcd(8, 12))\n",
        );
        assert_eq!(run(src), "2\n4\n");
    }

    #[test]
    fn import_as_renames() {
        let src = concat!(
            "import math as m\n",
            "from math import pi as P\n",
            "print(int(m.pow(2, 5)))\n",
            "print(round(P, 4))\n",
        );
        assert_eq!(run(src), "32\n3.1416\n");
    }

    #[test]
    fn missing_module_raises_module_not_found_error() {
        let src = concat!(
            "try:\n",
            "    import does_not_exist\n",
            "except ModuleNotFoundError as e:\n",
            "    print('caught', type(e).__name__)\n",
        );
        assert_eq!(run(src), "caught ModuleNotFoundError\n");
    }

    #[test]
    fn dotted_import_walks_attributes() {
        let src = concat!(
            "import os.path\n",
            "print(os.path.basename('/a/b/c.txt'))\n",
        );
        assert_eq!(run(src), "c.txt\n");
    }

    #[test]
    fn class_iter_protocol() {
        let src = concat!(
            "class Count:\n",
            "    def __init__(self, n):\n",
            "        self.n = n\n",
            "        self.i = 0\n",
            "    def __iter__(self):\n",
            "        return self\n",
            "    def __next__(self):\n",
            "        if self.i >= self.n:\n",
            "            raise StopIteration\n",
            "        v = self.i\n",
            "        self.i = v + 1\n",
            "        return v\n",
            "for x in Count(3):\n",
            "    print(x)\n"
        );
        assert_eq!(run(src), "0\n1\n2\n");
    }
}

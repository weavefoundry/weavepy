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
use std::rc::Rc;

use weavepy_compiler::{BinOpKind, CodeObject, CompareKind, Constant, OpCode, UnaryKind};

pub mod builtins;
pub mod error;
pub mod object;

use crate::error::{
    attribute_error, index_error, key_error, name_error, type_error, value_error,
    zero_division_error,
};
use crate::object::{BoundMethod, BuiltinFn, DictData, DictKey, Object, PyFunction, PySlice};

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
}

impl Default for Interpreter {
    fn default() -> Self {
        let stdout: Stdout = Rc::new(RefCell::new(std::io::stdout()));
        let builtins = Rc::new(RefCell::new(builtins::default_builtins()));
        Self { stdout, builtins }
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

    /// Wire `print` (and friends) to this interpreter's stdout.
    fn install_print_into(&self, dict: &mut DictData) {
        let sink = self.stdout.clone();
        let f = BuiltinFn {
            name: "print",
            call: Box::new(move |args: &[Object]| {
                let mut sink = sink.borrow_mut();
                let mut first = true;
                for a in args {
                    if !first {
                        write!(sink, " ").ok();
                    }
                    first = false;
                    write!(sink, "{}", a.to_str()).ok();
                }
                writeln!(sink).ok();
                Ok(Object::None)
            }),
        };
        dict.insert(
            DictKey(Object::from_static("print")),
            Object::Builtin(Rc::new(f)),
        );
    }

    /// Run a module-level [`CodeObject`] to completion.
    pub fn run_module(&mut self, code: &CodeObject) -> Result<Object, RuntimeError> {
        let globals = Rc::new(RefCell::new(DictData::new()));
        self.install_print_into(&mut globals.borrow_mut());
        let code_rc = Rc::new(code.clone());
        let mut frame = self.make_frame(code_rc, Vec::new(), Vec::new(), globals, true);
        self.run_frame(&mut frame)
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
            pc: 0,
        }
    }

    // ---------- dispatch ----------

    fn run_frame(&mut self, frame: &mut Frame) -> Result<Object, RuntimeError> {
        loop {
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
                    let v = self.lookup_global_or_builtin(&frame.globals, &name)?;
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
                    frame
                        .globals
                        .borrow_mut()
                        .insert(DictKey(Object::from_str(name)), v);
                }
                OpCode::StoreGlobal => {
                    let v = frame.pop()?;
                    let name = self.name_at(&frame.code, ins.arg)?;
                    frame
                        .globals
                        .borrow_mut()
                        .insert(DictKey(Object::from_str(name)), v);
                }
                OpCode::DeleteName | OpCode::DeleteGlobal => {
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
                    let _obj = frame.pop()?;
                    let _val = frame.pop()?;
                    return Err(type_error(
                        "attribute assignment on builtin types not supported in the slice",
                    ));
                }
                OpCode::DeleteAttr => {
                    let _obj = frame.pop()?;
                    return Err(type_error(
                        "attribute deletion on builtin types not supported in the slice",
                    ));
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
                    let r = binary_op(&a, &b, kind)?;
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
                    let r = compare_op(&a, &b, kind)?;
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
                    let args: Vec<Object> = frame.stack.split_off(split_at);
                    let callable = frame.pop()?;
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
                    let kw_pairs: Vec<(String, Object)> =
                        names.into_iter().zip(kw_values).collect();
                    let r = self.call(&callable, &pos_args, &kw_pairs, &frame.globals)?;
                    frame.push(r);
                }
                OpCode::ReturnValue => {
                    return frame.pop();
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
                    let it = v.make_iter()?;
                    frame.push(Object::Iter(Rc::new(RefCell::new(it))));
                }
                OpCode::ForIter => {
                    let it_obj = frame
                        .stack
                        .last()
                        .cloned()
                        .ok_or_else(|| RuntimeError::Internal("FOR_ITER no iter".to_owned()))?;
                    let next = match &it_obj {
                        Object::Iter(it) => it.borrow_mut().next_value(),
                        _ => {
                            return Err(RuntimeError::Internal(
                                "FOR_ITER expects iterator on stack".to_owned(),
                            ))
                        }
                    };
                    match next {
                        Some(v) => frame.push(v),
                        None => {
                            // Exhausted: drop the iterator and jump out.
                            // CPython peeks the iterator with FOR_ITER
                            // and END_FOR pops on exit, but for the slice
                            // we pop here and skip the END_FOR.
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
                        Object::Str(s) => {
                            s.chars().map(|c| Object::from_str(c.to_string())).collect()
                        }
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
            }
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

    fn lookup_method(&self, obj: &Object, name: &str) -> Option<Object> {
        builtins::lookup_method(obj, name)
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
            _ => Err(type_error(format!(
                "'{}' object is not callable",
                callable.type_name()
            ))),
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

fn normalize_index(i: i64, len: usize) -> Result<usize, RuntimeError> {
    let n = len as i64;
    let idx = if i < 0 { i + n } else { i };
    if idx < 0 || idx >= n {
        return Err(index_error("index out of range"));
    }
    Ok(idx as usize)
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

pub use error::{PyException, RuntimeError};
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
}

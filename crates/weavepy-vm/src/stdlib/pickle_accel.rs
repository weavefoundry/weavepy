//! Guarded protocol-4/5 encoding and decoding of built-in pickle data.
//!
//! A first pass checks the complete supported stream and its container graph
//! without creating Python objects. Cycles, deep graphs, custom reconstruction,
//! and unsupported or malformed input return to the existing unpickler.

mod encode;

use crate::shared_value::{SharedSlice, SharedStr};
use num_bigint::BigInt;
use weavepy_compiler::CodeObject;

use crate::error::RuntimeError;
use crate::import::ModuleCache;
use crate::object::{BuiltinFn, DictData, DictKey, Object, PyFunction, PyModule, StrKey};
use crate::sync::{Rc, RefCell, Weak};
use crate::types::TypeObject;

struct FunctionGuard {
    function: Weak<PyFunction>,
    code: Weak<CodeObject>,
}

impl FunctionGuard {
    fn new(value: &Object) -> Option<Self> {
        let Object::Function(function) = value else {
            return None;
        };
        let guard = Self {
            function: Rc::downgrade(function),
            code: Rc::downgrade(&function.code.borrow()),
        };
        guard.holds(value).then_some(guard)
    }

    fn holds(&self, value: &Object) -> bool {
        let Object::Function(function) = value else {
            return false;
        };
        if Rc::as_ptr(function) != self.function.as_ptr()
            || Rc::as_ptr(&function.code.borrow()) != self.code.as_ptr()
        {
            return false;
        }
        let slots = function.slots.borrow();
        !slots.contains_key(&StrKey("__defaults__"))
            && !slots.contains_key(&StrKey("__kwdefaults__"))
    }
}

struct UnpicklerGuard {
    class: Weak<TypeObject>,
    version: u64,
    init: FunctionGuard,
    load: FunctionGuard,
    dispatch: Weak<RefCell<DictData>>,
    entries: Vec<(u8, FunctionGuard)>,
}

impl UnpicklerGuard {
    fn new(class: &Rc<TypeObject>) -> Option<Self> {
        let Object::Dict(dispatch) = class.lookup("dispatch")? else {
            return None;
        };
        let entries = dispatch
            .borrow()
            .iter()
            .map(|(key, value)| {
                let Object::Int(key) = key.0 else {
                    return None;
                };
                Some((u8::try_from(key).ok()?, FunctionGuard::new(value)?))
            })
            .collect::<Option<Vec<_>>>()?;
        Some(Self {
            class: Rc::downgrade(class),
            version: class.attr_version.get(),
            init: FunctionGuard::new(&class.lookup("__init__")?)?,
            load: FunctionGuard::new(&class.lookup("load")?)?,
            dispatch: Rc::downgrade(&dispatch),
            entries,
        })
    }

    fn matches_class(&self, class: &Rc<TypeObject>) -> bool {
        Rc::as_ptr(class) == self.class.as_ptr() && class.attr_version.get() == self.version
    }

    fn holds(&self, class: &Rc<TypeObject>, opcodes: &Opcodes) -> bool {
        if !self.matches_class(class)
            || !class
                .lookup("__init__")
                .is_some_and(|f| self.init.holds(&f))
            || !class.lookup("load").is_some_and(|f| self.load.holds(&f))
        {
            return false;
        }
        let Some(dispatch) = self.dispatch.upgrade() else {
            return false;
        };
        let dispatch = dispatch.borrow();
        // Keep the dispatch table's shape check, but only inspect function
        // code/defaults for instructions the validated stream executes. An
        // unused handler cannot affect the result. This avoids activating
        // the global PEP 509 registry and locking every dict mutation.
        dispatch.len() == self.entries.len()
            && dispatch
                .iter()
                .zip(&self.entries)
                .all(|((key, value), (opcode, guard))| {
                    matches!(key.0, Object::Int(current) if current == i64::from(*opcode))
                        && (!opcodes.contains(*opcode) || guard.holds(value))
                })
    }
}

pub fn build(_cache: &ModuleCache) -> Rc<PyModule> {
    let factory = Object::Builtin(Rc::new(BuiltinFn {
        name: "make_loads",
        binds_instance: false,
        call: Box::new(make_loads),
        call_kw: None,
    }));
    crate::descr_registry::register_module(&factory, "_weave_pickle");
    let mut dict = DictData::default();
    dict.insert(
        DictKey(Object::from_static("__name__")),
        Object::from_static("_weave_pickle"),
    );
    dict.insert(DictKey(Object::from_static("make_loads")), factory);
    let factory = Object::Builtin(Rc::new(BuiltinFn {
        name: "make_dumps",
        binds_instance: false,
        call: Box::new(make_dumps),
        call_kw: None,
    }));
    crate::descr_registry::register_module(&factory, "_weave_pickle");
    dict.insert(DictKey(Object::from_static("make_dumps")), factory);
    Rc::new(PyModule {
        name: "_weave_pickle".to_owned(),
        filename: None,
        dict: Rc::new(RefCell::new(dict)),
    })
}

#[allow(clippy::unnecessary_wraps)]
fn make_loads(args: &[Object]) -> Result<Object, RuntimeError> {
    let [Object::Type(class)] = args else {
        return Ok(Object::None);
    };
    let Some(guard) = UnpicklerGuard::new(class) else {
        return Ok(Object::None);
    };
    // Weak guards cannot create a hidden Rust-closure cycle through the
    // unpickler's Python functions and their module globals.
    Ok(Object::Builtin(Rc::new(BuiltinFn {
        name: "try_loads",
        binds_instance: false,
        call: Box::new(move |args| {
            let [Object::Bytes(data), Object::Type(class), Object::Bool(_), Object::Str(encoding), Object::Str(errors), Object::None] =
                args
            else {
                return Ok(Object::None);
            };
            if encoding.as_ref() != "ASCII"
                || errors.as_ref() != "strict"
                || !guard.matches_class(class)
            {
                return Ok(Object::None);
            }
            // A one-element tuple distinguishes a decoded None from a
            // request to use the existing unpickler.
            Ok(decode(data, |opcodes| guard.holds(class, opcodes))
                .map_or(Object::None, |value| Object::new_tuple_array([value])))
        }),
        call_kw: None,
    })))
}

struct ClassFunctionsGuard {
    class: Weak<TypeObject>,
    version: u64,
    functions: Vec<FunctionGuard>,
}

impl ClassFunctionsGuard {
    fn new(class: &Rc<TypeObject>) -> Option<Self> {
        let functions = class
            .dict
            .borrow()
            .iter()
            .filter(|(_, value)| matches!(value, Object::Function(_)))
            .map(|(_, value)| FunctionGuard::new(value))
            .collect::<Option<Vec<_>>>()?;
        Some(Self {
            class: Rc::downgrade(class),
            version: class.attr_version.get(),
            functions,
        })
    }

    fn holds(&self) -> bool {
        self.class.upgrade().is_some_and(|class| {
            class.attr_version.get() == self.version
                && self.functions.iter().all(|guard| {
                    guard
                        .function
                        .upgrade()
                        .is_some_and(|f| guard.holds(&Object::Function(f)))
                })
        })
    }
}

struct DumpDescriptorGuard {
    instance: Weak<crate::types::PyInstance>,
    class: ClassFunctionsGuard,
    function: FunctionGuard,
}

impl DumpDescriptorGuard {
    fn new(value: &Object) -> Option<Self> {
        let Object::Instance(instance) = value else {
            return None;
        };
        Some(Self {
            instance: Rc::downgrade(instance),
            class: ClassFunctionsGuard::new(&instance.class.borrow())?,
            function: FunctionGuard::new(&instance.slot_get("_func")?)?,
        })
    }

    fn holds(&self, value: &Object) -> bool {
        let Object::Instance(instance) = value else {
            return false;
        };
        Rc::as_ptr(instance) == self.instance.as_ptr()
            && Rc::as_ptr(&instance.class.borrow()) == self.class.class.as_ptr()
            && self.class.holds()
            && instance
                .slot_get("_func")
                .is_some_and(|f| self.function.holds(&f))
    }
}

struct PicklerGuard {
    class: Weak<TypeObject>,
    hierarchy: Vec<ClassFunctionsGuard>,
    dump: DumpDescriptorGuard,
    dispatch: Weak<RefCell<DictData>>,
    entries: Vec<(Weak<TypeObject>, FunctionGuard)>,
}

impl PicklerGuard {
    fn new(class: &Rc<TypeObject>) -> Option<Self> {
        let Object::Dict(dispatch) = class.lookup("dispatch")? else {
            return None;
        };
        let entries = dispatch
            .borrow()
            .iter()
            .map(|(key, value)| {
                let Object::Type(key) = &key.0 else {
                    return None;
                };
                Some((Rc::downgrade(key), FunctionGuard::new(value)?))
            })
            .collect::<Option<Vec<_>>>()?;
        let mut hierarchy = vec![ClassFunctionsGuard::new(class)?];
        for base in class.mro.borrow().iter() {
            if !Rc::ptr_eq(base, class) {
                hierarchy.push(ClassFunctionsGuard::new(base)?);
            }
        }
        Some(Self {
            class: Rc::downgrade(class),
            hierarchy,
            dump: DumpDescriptorGuard::new(&class.lookup("dump")?)?,
            dispatch: Rc::downgrade(&dispatch),
            entries,
        })
    }

    fn holds(&self, class: &Rc<TypeObject>) -> bool {
        if Rc::as_ptr(class) != self.class.as_ptr()
            || !self.hierarchy.iter().all(ClassFunctionsGuard::holds)
            || !class
                .lookup("dump")
                .is_some_and(|value| self.dump.holds(&value))
        {
            return false;
        }
        let Some(dispatch) = self.dispatch.upgrade() else {
            return false;
        };
        let dispatch = dispatch.borrow();
        dispatch.len() == self.entries.len()
            && dispatch
                .iter()
                .zip(&self.entries)
                .all(|((key, value), (kind, function))| {
                    matches!(&key.0, Object::Type(current) if Rc::as_ptr(current) == kind.as_ptr())
                        && function.holds(value)
                })
    }
}

#[allow(clippy::unnecessary_wraps)]
fn make_dumps(args: &[Object]) -> Result<Object, RuntimeError> {
    let [Object::Type(class)] = args else {
        return Ok(Object::None);
    };
    let Some(guard) = PicklerGuard::new(class) else {
        return Ok(Object::None);
    };
    Ok(Object::Builtin(Rc::new(BuiltinFn {
        name: "try_dumps",
        binds_instance: false,
        call: Box::new(move |args| {
            let [value, Object::Type(class), protocol, Object::Bool(_), Object::None] = args else {
                return Ok(Object::None);
            };
            let protocol = match protocol {
                Object::None => 5,
                Object::Int(value) if *value < 0 => 5,
                Object::Int(4) => 4,
                Object::Int(5) => 5,
                _ => return Ok(Object::None),
            };
            if !guard.holds(class) {
                return Ok(Object::None);
            }
            Ok(encode::encode(value, protocol)
                .map_or(Object::None, |data| Object::Bytes(SharedSlice::from(data))))
        }),
        call_kw: None,
    })))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Container {
    List,
    Dict,
    Tuple,
}

enum Scalar<'a> {
    None,
    Bool(bool),
    Int(i64),
    Long(&'a [u8]),
    Float(f64),
    Str(&'a str),
    Bytes(&'a [u8]),
}

trait Sink {
    type Value: Clone;

    fn opcode(&mut self, _value: u8) {}
    fn scalar(&mut self, value: Scalar<'_>) -> Self::Value;
    fn empty(&mut self, kind: Container) -> Option<Self::Value>;
    fn tuple(&mut self, items: Vec<Self::Value>) -> Option<Self::Value>;
    fn extend(&mut self, target: &Self::Value, items: Vec<Self::Value>) -> Option<()>;
    fn setitems(&mut self, target: &Self::Value, items: Vec<Self::Value>) -> Option<()>;
}

fn push<T>(items: &mut Vec<T>, value: T) -> Option<()> {
    items.try_reserve(1).ok()?;
    items.push(value);
    Some(())
}

struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
    frame_end: Option<usize>,
}

impl<'a> Reader<'a> {
    fn read(&mut self, size: usize) -> Option<&'a [u8]> {
        if self.frame_end == Some(self.pos) {
            self.frame_end = None;
        }
        let end = self.pos.checked_add(size)?;
        if self.frame_end.is_some_and(|limit| end > limit) {
            return None;
        }
        let value = self.data.get(self.pos..end)?;
        self.pos = end;
        Some(value)
    }

    fn byte(&mut self) -> Option<u8> {
        Some(self.read(1)?[0])
    }

    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.read(4)?.try_into().ok()?))
    }

    fn size64(&mut self) -> Option<usize> {
        usize::try_from(u64::from_le_bytes(self.read(8)?.try_into().ok()?)).ok()
    }

    fn frame(&mut self) -> Option<()> {
        // FRAME is valid only after the preceding frame has ended. In
        // particular, its argument cannot be read out of an active frame.
        if self.frame_end.is_some() {
            return None;
        }
        let size = self.size64()?;
        let end = self.pos.checked_add(size)?;
        if end > self.data.len() {
            return None;
        }
        self.frame_end = Some(end);
        Some(())
    }
}

fn take<V>(stack: &mut Vec<V>, count: usize, marks: &[usize]) -> Option<Vec<V>> {
    let begin = stack.len().checked_sub(count)?;
    if marks.last().is_some_and(|mark| begin < *mark) {
        return None;
    }
    Some(stack.split_off(begin))
}

fn top<'a, V>(stack: &'a [V], marks: &[usize]) -> Option<&'a V> {
    if marks.last() == Some(&stack.len()) {
        return None;
    }
    stack.last()
}

#[allow(clippy::too_many_lines)]
fn parse<S: Sink>(data: &[u8], sink: &mut S) -> Option<S::Value> {
    if data.len() < 3 || data[0] != 0x80 || !matches!(data[1], 4 | 5) {
        return None;
    }
    sink.opcode(0x80);
    let mut reader = Reader {
        data,
        pos: 2,
        frame_end: None,
    };
    let mut stack = Vec::new();
    let mut marks = Vec::new();
    let mut memo: Vec<S::Value> = Vec::new();
    loop {
        let opcode = reader.byte()?;
        sink.opcode(opcode);
        let value = match opcode {
            b'.' => {
                return (marks.is_empty() && stack.len() == 1)
                    .then(|| stack.pop())
                    .flatten();
            }
            0x95 => {
                reader.frame()?;
                continue;
            }
            b'(' => {
                push(&mut marks, stack.len())?;
                continue;
            }
            0x94 => {
                push(&mut memo, top(&stack, &marks)?.clone())?;
                continue;
            }
            b'h' => memo.get(usize::from(reader.byte()?))?.clone(),
            b'j' => memo.get(usize::try_from(reader.u32()?).ok()?)?.clone(),
            b'N' => sink.scalar(Scalar::None),
            0x88 => sink.scalar(Scalar::Bool(true)),
            0x89 => sink.scalar(Scalar::Bool(false)),
            b'K' => sink.scalar(Scalar::Int(i64::from(reader.byte()?))),
            b'M' => sink.scalar(Scalar::Int(i64::from(u16::from_le_bytes(
                reader.read(2)?.try_into().ok()?,
            )))),
            b'J' => sink.scalar(Scalar::Int(i64::from(i32::from_le_bytes(
                reader.read(4)?.try_into().ok()?,
            )))),
            0x8a | 0x8b => {
                let size = if data[reader.pos - 1] == 0x8a {
                    usize::from(reader.byte()?)
                } else {
                    let size = i32::from_le_bytes(reader.read(4)?.try_into().ok()?);
                    usize::try_from(size).ok()?
                };
                sink.scalar(Scalar::Long(reader.read(size)?))
            }
            b'G' => sink.scalar(Scalar::Float(f64::from_be_bytes(
                reader.read(8)?.try_into().ok()?,
            ))),
            opcode @ (0x8c | b'X' | 0x8d | b'C' | b'B' | 0x8e) => {
                let size = match opcode {
                    0x8c | b'C' => usize::from(reader.byte()?),
                    b'X' | b'B' => usize::try_from(reader.u32()?).ok()?,
                    _ => reader.size64()?,
                };
                let bytes = reader.read(size)?;
                sink.scalar(if matches!(opcode, 0x8c | b'X' | 0x8d) {
                    // Surrogate-pass strings stay on the existing WStr path.
                    Scalar::Str(std::str::from_utf8(bytes).ok()?)
                } else {
                    Scalar::Bytes(bytes)
                })
            }
            b']' => sink.empty(Container::List)?,
            b'}' => sink.empty(Container::Dict)?,
            b')' => sink.empty(Container::Tuple)?,
            0x85..=0x87 => {
                let count = usize::from(data[reader.pos - 1] - 0x84);
                sink.tuple(take(&mut stack, count, &marks)?)?
            }
            b't' => {
                let mark = marks.pop()?;
                sink.tuple(stack.split_off(mark))?
            }
            b'a' => {
                let items = take(&mut stack, 1, &marks)?;
                sink.extend(top(&stack, &marks)?, items)?;
                continue;
            }
            b'e' => {
                let mark = marks.pop()?;
                let items = stack.split_off(mark);
                sink.extend(top(&stack, &marks)?, items)?;
                continue;
            }
            b's' => {
                let items = take(&mut stack, 2, &marks)?;
                sink.setitems(top(&stack, &marks)?, items)?;
                continue;
            }
            b'u' => {
                let mark = marks.pop()?;
                let items = stack.split_off(mark);
                sink.setitems(top(&stack, &marks)?, items)?;
                continue;
            }
            // GLOBAL, REDUCE, persistent IDs, external buffers, legacy
            // protocols, and all other operations retain the full unpickler.
            _ => return None,
        };
        push(&mut stack, value)?;
    }
}

#[derive(Clone, Copy)]
enum Kind {
    Key,
    Float,
    Container(u32),
}

struct Node {
    kind: Container,
    children: Vec<u32>,
}

struct Opcodes([bool; 256]);

impl Opcodes {
    fn insert(&mut self, opcode: u8) {
        // One indexed store avoids a shift and a read-modify-write on each
        // decoded instruction. This scratch is local to one preflight.
        self.0[usize::from(opcode)] = true;
    }

    fn contains(&self, opcode: u8) -> bool {
        self.0[usize::from(opcode)]
    }
}

struct Probe<const RECORD_OPCODES: bool> {
    nodes: Vec<Node>,
    opcodes: Opcodes,
}

impl<const RECORD_OPCODES: bool> Default for Probe<RECORD_OPCODES> {
    fn default() -> Self {
        Self {
            nodes: Vec::new(),
            opcodes: Opcodes([!RECORD_OPCODES; 256]),
        }
    }
}

impl<const RECORD_OPCODES: bool> Probe<RECORD_OPCODES> {
    fn node(&mut self, kind: Container, items: &[Kind]) -> Option<Kind> {
        let index = u32::try_from(self.nodes.len()).ok()?;
        let mut children = Vec::new();
        for item in items {
            if let Kind::Container(child) = item {
                push(&mut children, *child)?;
            }
        }
        push(&mut self.nodes, Node { kind, children })?;
        Some(Kind::Container(index))
    }

    fn acyclic_and_bounded(&self) -> Option<()> {
        const MAX_DEPTH: usize = 128;
        let mut colors = Vec::new();
        colors.try_reserve(self.nodes.len()).ok()?;
        colors.resize(self.nodes.len(), 0u8);
        let mut heights = Vec::new();
        heights.try_reserve(self.nodes.len()).ok()?;
        heights.resize(self.nodes.len(), 0usize);
        let mut stack = Vec::new();
        for root in 0..self.nodes.len() {
            if colors[root] != 0 {
                continue;
            }
            colors[root] = 1;
            push(&mut stack, (root, 0usize))?;
            while let Some((index, next)) = stack.last_mut() {
                let node = &self.nodes[*index];
                if let Some(&child) = node.children.get(*next) {
                    *next += 1;
                    let child = child as usize;
                    match colors[child] {
                        1 => return None,
                        0 => {
                            if stack.len() >= MAX_DEPTH {
                                return None;
                            }
                            colors[child] = 1;
                            push(&mut stack, (child, 0))?;
                        }
                        _ => {}
                    }
                } else {
                    let height = 1 + node
                        .children
                        .iter()
                        .map(|&child| heights[child as usize])
                        .max()
                        .unwrap_or(0);
                    if height > MAX_DEPTH {
                        return None;
                    }
                    heights[*index] = height;
                    colors[*index] = 2;
                    stack.pop();
                }
            }
        }
        Some(())
    }
}

impl<const RECORD_OPCODES: bool> Sink for Probe<RECORD_OPCODES> {
    type Value = Kind;

    fn opcode(&mut self, value: u8) {
        if RECORD_OPCODES {
            self.opcodes.insert(value);
        }
    }

    fn scalar(&mut self, value: Scalar<'_>) -> Kind {
        if matches!(value, Scalar::Float(_)) {
            Kind::Float
        } else {
            Kind::Key
        }
    }

    fn empty(&mut self, kind: Container) -> Option<Kind> {
        self.node(kind, &[])
    }

    fn tuple(&mut self, items: Vec<Kind>) -> Option<Kind> {
        self.node(Container::Tuple, &items)
    }

    fn extend(&mut self, target: &Kind, items: Vec<Kind>) -> Option<()> {
        let Kind::Container(index) = target else {
            return None;
        };
        let node = self.nodes.get_mut(*index as usize)?;
        if node.kind != Container::List {
            return None;
        }
        for item in items {
            if let Kind::Container(child) = item {
                push(&mut node.children, child)?;
            }
        }
        Some(())
    }

    fn setitems(&mut self, target: &Kind, items: Vec<Kind>) -> Option<()> {
        let Kind::Container(index) = target else {
            return None;
        };
        let node = self.nodes.get_mut(*index as usize)?;
        if node.kind != Container::Dict || !items.len().is_multiple_of(2) {
            return None;
        }
        for pair in items.as_chunks::<2>().0 {
            // Tuple and float keys retain the full unpickler, including
            // arbitrary nesting and NaN key-identity behavior.
            if !matches!(pair[0], Kind::Key) {
                return None;
            }
            if let Kind::Container(child) = pair[1] {
                push(&mut node.children, child)?;
            }
        }
        Some(())
    }
}

struct Objects;

impl Sink for Objects {
    type Value = Object;

    fn scalar(&mut self, value: Scalar<'_>) -> Object {
        match value {
            Scalar::None => Object::None,
            Scalar::Bool(value) => Object::Bool(value),
            Scalar::Int(value) => Object::Int(value),
            Scalar::Long(bytes) => Object::int_from_bigint(BigInt::from_signed_bytes_le(bytes)),
            Scalar::Float(value) => Object::Float(value),
            Scalar::Str(value) => Object::Str(SharedStr::from(value)),
            Scalar::Bytes(value) => Object::new_bytes(value),
        }
    }

    fn empty(&mut self, kind: Container) -> Option<Object> {
        let value = match kind {
            Container::List => Object::new_list(Vec::new()),
            Container::Dict => Object::new_dict(),
            Container::Tuple => Object::new_tuple(Vec::new()),
        };
        // Even an acyclic decoded container can acquire a cycle through
        // later mutation. Register every mutable container, including those
        // nested below the returned object. Tuples are traced through owners.
        if matches!(kind, Container::List | Container::Dict) {
            crate::gc_trace::track(value.clone());
        }
        Some(value)
    }

    fn tuple(&mut self, items: Vec<Object>) -> Option<Object> {
        Some(Object::new_tuple(items))
    }

    fn extend(&mut self, target: &Object, items: Vec<Object>) -> Option<()> {
        let Object::List(list) = target else {
            return None;
        };
        let mut list = list.borrow_mut();
        list.try_reserve(items.len()).ok()?;
        list.extend(items);
        Some(())
    }

    fn setitems(&mut self, target: &Object, items: Vec<Object>) -> Option<()> {
        let Object::Dict(dict) = target else {
            return None;
        };
        if !items.len().is_multiple_of(2) {
            return None;
        }
        let mut dict = dict.borrow_mut();
        dict.try_reserve(items.len() / 2).ok()?;
        let mut items = items.into_iter();
        while let Some(key) = items.next() {
            dict.insert(DictKey(key), items.next()?);
        }
        Some(())
    }
}

/// Decode only after the preflight succeeds without creating Python objects.
fn decode(data: &[u8], validate: impl FnOnce(&Opcodes) -> bool) -> Option<Object> {
    if data.len() < 3 || data[0] != 0x80 || !matches!(data[1], 4 | 5) {
        return None;
    }
    // Checking the whole dispatch table has a fixed cost. For longer
    // streams, avoid recording every opcode and pay that fixed cost once.
    // Both specializations still validate the entire supported stream.
    if data.len() <= 512 {
        decode_with_probe::<true>(data, validate)
    } else {
        // A replaced reader may ignore the input or reject it immediately.
        // Check the full table before scanning a large stream so that such
        // readers keep their existing fallback cost.
        if !validate(&Opcodes([true; 256])) {
            return None;
        }
        decode_with_probe::<false>(data, |_| true)
    }
}

fn decode_with_probe<const RECORD_OPCODES: bool>(
    data: &[u8],
    validate: impl FnOnce(&Opcodes) -> bool,
) -> Option<Object> {
    let mut probe = Probe::<RECORD_OPCODES>::default();
    parse(data, &mut probe)?;
    probe.acyclic_and_bounded()?;
    // Unsupported inputs return before checking Python dispatch functions.
    // Guard supported instructions before any Python objects are constructed.
    if !validate(&probe.opcodes) {
        return None;
    }
    // Drop the graph scratch before allocating the returned Python objects.
    drop(probe);
    parse(data, &mut Objects)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode(data: &[u8]) -> Option<Object> {
        super::decode(data, |_| true)
    }

    #[test]
    fn decoded_strings_keep_utf8_and_memo_aliases() {
        let empty = decode(b"\x80\x05\x8c\x00.").unwrap();
        assert!(matches!(empty, Object::Str(value) if value.is_empty()));
        let result = decode(b"\x80\x05]\x94(\x8c\x05\xc3\xa9\xe2\x82\xac\x94h\x01e.").unwrap();
        let Object::List(items) = result else {
            panic!("expected list");
        };
        let items = items.borrow();
        assert_eq!(items.len(), 2);
        assert!(items[0].is_same(&items[1]));
        assert!(matches!(&items[0], Object::Str(value) if value.as_ref() == "é€"));
    }

    #[test]
    fn decode_scalars_frames_and_memo_aliases() {
        assert!(matches!(decode(b"\x80\x04N."), Some(Object::None)));
        let value = decode(b"\x80\x05\x95\x04\0\0\0\0\0\0\0K\x07\x85.").unwrap();
        let Object::Tuple(tuple) = value else {
            panic!("expected tuple");
        };
        assert_eq!(tuple.len(), 1);
        assert!(matches!(tuple[0], Object::Int(7)));
        let value = decode(b"\x80\x05]\x94(]\x94h\x01e.").unwrap();
        let Object::List(list) = value else {
            panic!("expected list");
        };
        let list = list.borrow();
        assert_eq!(list.len(), 2);
        assert!(list[0].is_same(&list[1]));
    }

    #[test]
    fn preflight_rejects_cycles_deep_graphs_and_unsupported_keys() {
        for data in [
            &b"\x80\x05]\x94h\x00a."[..],
            &b"\x80\x05]\x94(h\x00\x85e."[..],
            &b"\x80\x05}]K\x01s."[..],
            &b"\x80\x05})K\x01s."[..],
        ] {
            assert!(decode(data).is_none(), "{data:?}");
        }
        let mut deep = b"\x80\x05)".to_vec();
        deep.extend(std::iter::repeat_n(0x85, 129));
        deep.push(b'.');
        assert!(decode(&deep).is_none());
    }

    #[test]
    fn preflight_rejects_bad_frames_arguments_and_text() {
        for data in [
            &b"\x80\x05\x95\xff\xff\xff\xff\xff\xff\xff\x7fN."[..],
            &b"\x80\x05\x95\x02\0\0\0\0\0\0\0G\0\0\0\0\0\0\0\0."[..],
            &b"\x80\x05h\0."[..],
            &b"\x80\x05\x8c\x01\xff."[..],
            &b"\x80\x05\x8b\xff\xff\xff\xff."[..],
            &b"\x80\x05e."[..],
            &b"\x80\x05(N."[..],
            &b"\x80\x05N"[..],
            &b"\x80\x03N."[..],
        ] {
            assert!(decode(data).is_none(), "{data:?}");
        }
    }
}

//! Guarded protocol-4/5 encoding and decoding of built-in pickle data and of
//! instances of plain classes.
//!
//! A first pass checks the complete supported stream and its container graph
//! without creating Python objects. Cycles, deep graphs, custom reconstruction,
//! and unsupported or malformed input return to the existing unpickler.
//!
//! Classes are resolved from `sys.modules` by plain dictionary probes, and
//! `NEWOBJ` and `BUILD` are accepted only for classes whose type metadata
//! proves that the unpickler would call `object.__new__`, update `__dict__`,
//! and fill `__slots__` members without reaching any Python hook.

mod classes;
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

/// Module-level Python functions that the full engine calls for instances.
struct ModuleFunctionsGuard {
    dict: Weak<RefCell<DictData>>,
    functions: Vec<(&'static str, FunctionGuard)>,
}

impl ModuleFunctionsGuard {
    fn new(module: &Object, names: &[&'static str]) -> Option<Self> {
        let Object::Module(module) = module else {
            return None;
        };
        let functions = {
            let dict = module.dict.borrow();
            names
                .iter()
                .map(|name| {
                    let function = classes::pure_get(&dict, name)??;
                    Some((*name, FunctionGuard::new(&function)?))
                })
                .collect::<Option<Vec<_>>>()?
        };
        Some(Self {
            dict: Rc::downgrade(&module.dict),
            functions,
        })
    }

    fn holds(&self, dict: &DictData) -> bool {
        self.functions.iter().all(|(name, guard)| {
            matches!(classes::pure_get(dict, name), Some(Some(value)) if guard.holds(&value))
        })
    }
}

struct BuiltinGuard(Weak<BuiltinFn>);

impl BuiltinGuard {
    fn new(dict: &DictData, name: &str) -> Option<Self> {
        let Object::Builtin(function) = classes::pure_get(dict, name)?? else {
            return None;
        };
        Some(Self(Rc::downgrade(&function)))
    }

    fn holds(&self, dict: &DictData, name: &str) -> bool {
        matches!(
            classes::pure_get(dict, name),
            Some(Some(Object::Builtin(function))) if Rc::as_ptr(&function) == self.0.as_ptr()
        )
    }
}

/// Interpreter-wide state that a class lookup depends on. The Python engine
/// calls `__import__` and reads `sys.modules` for every global.
struct ImportGuard {
    sys: Weak<RefCell<DictData>>,
    builtins: Weak<RefCell<DictData>>,
    import: BuiltinGuard,
    intern: BuiltinGuard,
}

impl ImportGuard {
    fn new(sys: &Object, builtins: &Object) -> Option<Self> {
        let (Object::Module(sys), Object::Module(builtins)) = (sys, builtins) else {
            return None;
        };
        Some(Self {
            import: BuiltinGuard::new(&builtins.dict.borrow(), "__import__")?,
            intern: BuiltinGuard::new(&sys.dict.borrow(), "intern")?,
            sys: Rc::downgrade(&sys.dict),
            builtins: Rc::downgrade(&builtins.dict),
        })
    }

    /// `sys.modules`, when nothing can observe or redirect a class lookup.
    fn modules(&self) -> Option<Rc<RefCell<DictData>>> {
        if !classes::ambient_state_is_plain()
            || !self
                .import
                .holds(&self.builtins.upgrade()?.borrow(), "__import__")
        {
            return None;
        }
        let sys = self.sys.upgrade()?;
        let sys = sys.borrow();
        if !self.intern.holds(&sys, "intern") {
            return None;
        }
        let Object::Dict(modules) = classes::pure_get(&sys, "modules")?? else {
            return None;
        };
        Some(modules)
    }
}

struct LoadsInstances {
    imports: ImportGuard,
    pickle: ModuleFunctionsGuard,
}

impl LoadsInstances {
    fn new(args: &[Object]) -> Option<Self> {
        let [sys, builtins, pickle] = args else {
            return None;
        };
        Some(Self {
            imports: ImportGuard::new(sys, builtins)?,
            pickle: ModuleFunctionsGuard::new(pickle, &["_getattribute"])?,
        })
    }

    fn context(&self) -> Option<DecodeContext> {
        let modules = self.imports.modules()?;
        self.pickle
            .holds(&self.pickle.dict.upgrade()?.borrow())
            .then_some(DecodeContext { modules })
    }
}

struct DumpsInstances {
    imports: ImportGuard,
    pickle: ModuleFunctionsGuard,
    copyreg: ModuleFunctionsGuard,
}

impl DumpsInstances {
    fn new(args: &[Object]) -> Option<Self> {
        let [sys, builtins, pickle, copyreg] = args else {
            return None;
        };
        Some(Self {
            imports: ImportGuard::new(sys, builtins)?,
            pickle: ModuleFunctionsGuard::new(pickle, &["whichmodule", "_getattribute"])?,
            copyreg: ModuleFunctionsGuard::new(
                copyreg,
                &[
                    "_reduce_newobj",
                    "_default_getstate",
                    "_slotnames",
                    "_lookup_special",
                    "__newobj__",
                ],
            )?,
        })
    }

    fn context(&self) -> Option<encode::InstanceContext> {
        let modules = self.imports.modules()?;
        if !self.copyreg.holds(&self.copyreg.dict.upgrade()?.borrow()) {
            return None;
        }
        let pickle = self.pickle.dict.upgrade()?;
        let pickle = pickle.borrow();
        if !self.pickle.holds(&pickle) {
            return None;
        }
        // `save` reads the module's `dispatch_table`, and `save_global`
        // prefers a registered extension code to a class reference.
        let Object::Dict(dispatch_table) = classes::pure_get(&pickle, "dispatch_table")?? else {
            return None;
        };
        let Object::Dict(extensions) = classes::pure_get(&pickle, "_extension_registry")?? else {
            return None;
        };
        if !extensions.borrow().is_empty() {
            return None;
        }
        Some(encode::InstanceContext {
            modules,
            dispatch_table,
        })
    }
}

struct UnpicklerGuard {
    class: Weak<TypeObject>,
    version: u64,
    init: FunctionGuard,
    load: FunctionGuard,
    find_class: Option<FunctionGuard>,
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
            // Without this guard, streams that name classes keep the
            // existing unpickler.
            find_class: class
                .lookup("find_class")
                .and_then(|function| FunctionGuard::new(&function)),
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
        // STACK_GLOBAL is the only supported instruction that calls a
        // method outside the dispatch table.
        if opcodes.contains(STACK_GLOBAL)
            && !self
                .find_class
                .as_ref()
                .is_some_and(|guard| class.lookup("find_class").is_some_and(|f| guard.holds(&f)))
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
    let [Object::Type(class), modules @ ..] = args else {
        return Ok(Object::None);
    };
    let Some(guard) = UnpicklerGuard::new(class) else {
        return Ok(Object::None);
    };
    // Without the `sys`, `builtins`, and `pickle` modules, only built-in
    // data is decoded natively.
    let instances = LoadsInstances::new(modules);
    // Weak guards cannot create a hidden Rust-closure cycle through the
    // unpickler's Python functions and their module globals.
    Ok(Object::Builtin(Rc::new(BuiltinFn {
        name: "try_loads",
        binds_instance: false,
        call: Box::new(move |args| {
            let [data, Object::Type(class), Object::Bool(_), Object::Str(encoding), Object::Str(errors), Object::None] =
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
            let context = || instances.as_ref()?.context();
            let decoded = match data {
                Object::Bytes(data) => {
                    decode(data, |opcodes| guard.holds(class, opcodes), &context)
                }
                // Decoding runs no Python code, so the borrow is safe.
                Object::ByteArray(data) => decode(
                    &data.borrow(),
                    |opcodes| guard.holds(class, opcodes),
                    &context,
                ),
                _ => None,
            };
            Ok(decoded.map_or(Object::None, |value| Object::new_tuple_array([value])))
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
    let [Object::Type(class), modules @ ..] = args else {
        return Ok(Object::None);
    };
    let Some(guard) = PicklerGuard::new(class) else {
        return Ok(Object::None);
    };
    // Without the `sys`, `builtins`, `pickle`, and `copyreg` modules, only
    // built-in data is encoded natively.
    let instances = DumpsInstances::new(modules);
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
            let context = || instances.as_ref()?.context();
            Ok(encode::encode(value, protocol, &context)
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

const STACK_GLOBAL: u8 = 0x93;
const NEWOBJ: u8 = 0x81;
const BUILD: u8 = b'b';

/// `sys.modules`, produced on demand once a stream names a class.
struct DecodeContext {
    modules: Rc<RefCell<DictData>>,
}

trait Sink<'a> {
    type Value: Clone;

    fn opcode(&mut self, _value: u8) {}
    fn scalar(&mut self, value: Scalar<'a>) -> Self::Value;
    fn empty(&mut self, kind: Container) -> Option<Self::Value>;
    fn tuple(&mut self, items: Vec<Self::Value>) -> Option<Self::Value>;
    fn extend(&mut self, target: &Self::Value, items: Batch<'_, Self::Value>) -> Option<()>;
    fn setitems(&mut self, target: &Self::Value, items: Batch<'_, Self::Value>) -> Option<()>;
    fn global(&mut self, module: &Self::Value, name: &Self::Value) -> Option<Self::Value>;
    fn newobj(&mut self, class: &Self::Value, args: &Self::Value) -> Option<Self::Value>;
    fn build(&mut self, target: &Self::Value, state: Self::Value) -> Option<()>;
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

/// The items an APPEND(S) or SETITEM(S) moves into their container.
type Batch<'s, V> = std::vec::Drain<'s, V>;

fn take<V>(stack: &mut Vec<V>, count: usize, marks: &[usize]) -> Option<Vec<V>> {
    let begin = stack.len().checked_sub(count)?;
    if marks.last().is_some_and(|mark| begin < *mark) {
        return None;
    }
    Some(stack.split_off(begin))
}

fn pop<V>(stack: &mut Vec<V>, marks: &[usize]) -> Option<V> {
    let begin = stack.len().checked_sub(1)?;
    if marks.last().is_some_and(|mark| begin < *mark) {
        return None;
    }
    stack.pop()
}

fn top<'a, V>(stack: &'a [V], marks: &[usize]) -> Option<&'a V> {
    if marks.last() == Some(&stack.len()) {
        return None;
    }
    stack.last()
}

/// The container below the items from `begin` on, and those items. Both
/// must lie above the innermost mark.
fn batch<'s, V: Clone>(
    stack: &'s mut Vec<V>,
    begin: usize,
    marks: &[usize],
) -> Option<(V, Batch<'s, V>)> {
    if begin == 0 || begin > stack.len() || marks.last().is_some_and(|mark| begin <= *mark) {
        return None;
    }
    let target = stack[begin - 1].clone();
    Some((target, stack.drain(begin..)))
}

#[allow(clippy::too_many_lines)]
fn parse<'a, S: Sink<'a>>(data: &'a [u8], sink: &mut S) -> Option<S::Value> {
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
    stack.try_reserve(64).ok()?;
    let mut marks = Vec::new();
    let mut memo: Vec<S::Value> = Vec::new();
    memo.try_reserve(data.len() / 16).ok()?;
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
                let begin = stack.len().checked_sub(1)?;
                let (target, items) = batch(&mut stack, begin, &marks)?;
                sink.extend(&target, items)?;
                continue;
            }
            b'e' => {
                let mark = marks.pop()?;
                let (target, items) = batch(&mut stack, mark, &marks)?;
                sink.extend(&target, items)?;
                continue;
            }
            b's' => {
                let begin = stack.len().checked_sub(2)?;
                let (target, items) = batch(&mut stack, begin, &marks)?;
                sink.setitems(&target, items)?;
                continue;
            }
            b'u' => {
                let mark = marks.pop()?;
                let (target, items) = batch(&mut stack, mark, &marks)?;
                sink.setitems(&target, items)?;
                continue;
            }
            STACK_GLOBAL => {
                let name = pop(&mut stack, &marks)?;
                let module = pop(&mut stack, &marks)?;
                sink.global(&module, &name)?
            }
            NEWOBJ => {
                let args = pop(&mut stack, &marks)?;
                let class = pop(&mut stack, &marks)?;
                sink.newobj(&class, &args)?
            }
            BUILD => {
                let state = pop(&mut stack, &marks)?;
                sink.build(top(&stack, &marks)?, state)?;
                continue;
            }
            // GLOBAL, REDUCE, NEWOBJ_EX, persistent IDs, external buffers,
            // legacy protocols, and all other operations retain the full
            // unpickler.
            _ => return None,
        };
        push(&mut stack, value)?;
    }
}

#[derive(Clone, Copy)]
enum Kind<'a> {
    None,
    Key,
    Str(&'a str),
    Float,
    /// An index into [`Probe::classes`]. Only NEWOBJ may consume it.
    Class(u32),
    /// An index into [`Probe::nodes`]: a container or an instance.
    Container(u32),
}

/// What BUILD needs to know about one item of a two-item state tuple.
#[derive(Clone, Copy)]
enum StateItem {
    None,
    Node(u32),
    Other,
}

enum Shape<'a> {
    List,
    Tuple {
        len: usize,
        pair: [StateItem; 2],
    },
    /// `keys` lists the string keys; `only_str` reports that there are no
    /// others, so the keys are complete.
    Dict {
        keys: Vec<&'a str>,
        only_str: bool,
    },
    Instance {
        class: u32,
    },
}

struct Node<'a> {
    shape: Shape<'a>,
    children: Vec<u32>,
}

/// A class that NEWOBJ and BUILD can handle without calling Python code.
struct PlainClass {
    class: Rc<TypeObject>,
    has_dict: bool,
}

impl PlainClass {
    fn resolve(context: &DecodeContext, module: &str, name: &str) -> Option<Self> {
        let class = classes::resolve_global(&context.modules, module, name)?;
        let has_dict = classes::plain_layout(&class, classes::DECODE_HOOKS)?;
        if !classes::benign_metaclass(&class) {
            return None;
        }
        // `object.__new__` refuses a class with abstract methods.
        match class.lookup("__abstractmethods__") {
            None => {}
            Some(Object::FrozenSet(names)) if names.is_empty() => {}
            Some(_) => return None,
        }
        Some(Self { class, has_dict })
    }
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

/// One BUILD's state containers: the `__dict__` state and the slot state.
#[derive(Clone, Copy)]
struct StateNodes {
    dict: Option<u32>,
    slots: Option<u32>,
}

/// What the first pass tells the second, all in stream order.
struct Plan {
    classes: Vec<Rc<TypeObject>>,
    /// Per node: whether the result can reach it. An unreachable container
    /// can never be mutated, so it needs no cycle-collector registration.
    reachable: Vec<bool>,
    /// Per BUILD: which state containers only that BUILD consumes, so that
    /// their storage can move into the instance.
    builds: Vec<StateNodes>,
}

struct Probe<'a, 'c, const RECORD_OPCODES: bool> {
    nodes: Vec<Node<'a>>,
    opcodes: Opcodes,
    /// Every class that STACK_GLOBAL resolved, in stream order. The second
    /// pass consumes the same list instead of repeating the lookups.
    classes: Vec<PlainClass>,
    /// The state of every BUILD, in stream order, with the pair tuple.
    builds: Vec<(StateNodes, Option<u32>)>,
    context: &'c dyn Fn() -> Option<DecodeContext>,
    resolved: Option<DecodeContext>,
}

impl<'a, 'c, const RECORD_OPCODES: bool> Probe<'a, 'c, RECORD_OPCODES> {
    fn new(context: &'c dyn Fn() -> Option<DecodeContext>) -> Self {
        Self {
            nodes: Vec::new(),
            opcodes: Opcodes([!RECORD_OPCODES; 256]),
            classes: Vec::new(),
            builds: Vec::new(),
            context,
            resolved: None,
        }
    }

    /// Count the references to each node that the result keeps: the root,
    /// container items, and the state pair's items when the pair itself is
    /// kept. BUILD copies its state, so its edge keeps nothing.
    fn plan(self, root: Kind<'_>) -> Option<Plan> {
        let mut references = Vec::new();
        references.try_reserve(self.nodes.len()).ok()?;
        references.resize(self.nodes.len(), 0u32);
        if let Kind::Container(root) = root {
            references[root as usize] = 1;
        }
        let mut is_pair = Vec::new();
        is_pair.try_reserve(self.nodes.len()).ok()?;
        is_pair.resize(self.nodes.len(), false);
        for pair in self.builds.iter().filter_map(|(_, pair)| *pair) {
            is_pair[pair as usize] = true;
        }
        for (index, node) in self.nodes.iter().enumerate() {
            if matches!(node.shape, Shape::Instance { .. }) || is_pair[index] {
                continue;
            }
            for &child in &node.children {
                references[child as usize] += 1;
            }
        }
        // A tuple's items precede it, so a pair inside a kept pair is seen
        // after the reference to it has been counted.
        for (index, node) in self.nodes.iter().enumerate().rev() {
            if is_pair[index] && references[index] > 0 {
                for &child in &node.children {
                    references[child as usize] += 1;
                }
            }
        }
        let mut uses = Vec::new();
        uses.try_reserve(self.nodes.len()).ok()?;
        uses.resize(self.nodes.len(), 0u32);
        for (state, _) in &self.builds {
            for index in [state.dict, state.slots].into_iter().flatten() {
                uses[index as usize] += 1;
            }
        }
        let movable = |index: Option<u32>| {
            index.filter(|&i| references[i as usize] == 0 && uses[i as usize] == 1)
        };
        let mut builds = Vec::new();
        builds.try_reserve(self.builds.len()).ok()?;
        builds.extend(self.builds.iter().map(|(state, _)| StateNodes {
            dict: movable(state.dict),
            slots: movable(state.slots),
        }));
        Some(Plan {
            classes: self.classes.into_iter().map(|c| c.class).collect(),
            reachable: references.into_iter().map(|n| n > 0).collect(),
            builds,
        })
    }

    fn node(&mut self, shape: Shape<'a>, items: &[Kind<'a>]) -> Option<Kind<'a>> {
        let index = u32::try_from(self.nodes.len()).ok()?;
        let mut children = Vec::new();
        for item in items {
            match item {
                Kind::Container(child) => push(&mut children, *child)?,
                // A class is only ever the operand of NEWOBJ.
                Kind::Class(_) => return None,
                _ => {}
            }
        }
        push(&mut self.nodes, Node { shape, children })?;
        Some(Kind::Container(index))
    }

    /// The string keys of a state dictionary, or `None` when the node is
    /// not a dictionary keyed only by strings.
    fn state_keys(&self, index: u32) -> Option<&[&'a str]> {
        match &self.nodes.get(index as usize)?.shape {
            Shape::Dict {
                keys,
                only_str: true,
            } => Some(keys),
            _ => None,
        }
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

impl<'a, const RECORD_OPCODES: bool> Sink<'a> for Probe<'a, '_, RECORD_OPCODES> {
    type Value = Kind<'a>;

    fn opcode(&mut self, value: u8) {
        if RECORD_OPCODES {
            self.opcodes.insert(value);
        }
    }

    fn scalar(&mut self, value: Scalar<'a>) -> Kind<'a> {
        match value {
            Scalar::None => Kind::None,
            Scalar::Float(_) => Kind::Float,
            Scalar::Str(text) => Kind::Str(text),
            _ => Kind::Key,
        }
    }

    fn empty(&mut self, kind: Container) -> Option<Kind<'a>> {
        let shape = match kind {
            Container::List => Shape::List,
            Container::Dict => Shape::Dict {
                keys: Vec::new(),
                only_str: true,
            },
            Container::Tuple => Shape::Tuple {
                len: 0,
                pair: [StateItem::Other; 2],
            },
        };
        self.node(shape, &[])
    }

    fn tuple(&mut self, items: Vec<Kind<'a>>) -> Option<Kind<'a>> {
        let item = |kind: &Kind<'a>| match kind {
            Kind::None => StateItem::None,
            Kind::Container(index) => StateItem::Node(*index),
            _ => StateItem::Other,
        };
        let pair = match items.as_slice() {
            [first, second] => [item(first), item(second)],
            _ => [StateItem::Other; 2],
        };
        let len = items.len();
        self.node(Shape::Tuple { len, pair }, &items)
    }

    fn extend(&mut self, target: &Kind<'a>, items: Batch<'_, Kind<'a>>) -> Option<()> {
        let Kind::Container(index) = target else {
            return None;
        };
        let node = self.nodes.get_mut(*index as usize)?;
        if !matches!(node.shape, Shape::List) {
            return None;
        }
        for item in items {
            match item {
                Kind::Container(child) => push(&mut node.children, child)?,
                Kind::Class(_) => return None,
                _ => {}
            }
        }
        Some(())
    }

    fn setitems(&mut self, target: &Kind<'a>, mut items: Batch<'_, Kind<'a>>) -> Option<()> {
        let Kind::Container(index) = target else {
            return None;
        };
        let node = self.nodes.get_mut(*index as usize)?;
        let Shape::Dict { keys, only_str } = &mut node.shape else {
            return None;
        };
        if !items.len().is_multiple_of(2) {
            return None;
        }
        while let Some(key) = items.next() {
            // Tuple and float keys retain the full unpickler, including
            // arbitrary nesting and NaN key-identity behavior. So do
            // instances, whose hashing can call Python code.
            match key {
                Kind::Str(key) => push(keys, key)?,
                Kind::None | Kind::Key => *only_str = false,
                _ => return None,
            }
            match items.next()? {
                Kind::Container(child) => push(&mut node.children, child)?,
                Kind::Class(_) => return None,
                _ => {}
            }
        }
        Some(())
    }

    fn global(&mut self, module: &Kind<'a>, name: &Kind<'a>) -> Option<Kind<'a>> {
        let (Kind::Str(module), Kind::Str(name)) = (module, name) else {
            return None;
        };
        if self.resolved.is_none() {
            self.resolved = Some((self.context)()?);
        }
        // An absent module keeps the full unpickler, which imports it.
        let class = PlainClass::resolve(self.resolved.as_ref()?, module, name)?;
        let index = u32::try_from(self.classes.len()).ok()?;
        push(&mut self.classes, class)?;
        Some(Kind::Class(index))
    }

    fn newobj(&mut self, class: &Kind<'a>, args: &Kind<'a>) -> Option<Kind<'a>> {
        let (Kind::Class(class), Kind::Container(args)) = (class, args) else {
            return None;
        };
        // `object.__new__(cls)` accepts no further arguments.
        if !matches!(
            self.nodes.get(*args as usize)?.shape,
            Shape::Tuple { len: 0, .. }
        ) {
            return None;
        }
        self.node(Shape::Instance { class: *class }, &[])
    }

    fn build(&mut self, target: &Kind<'a>, state: Kind<'a>) -> Option<()> {
        let (Kind::Container(target), Kind::Container(state)) = (*target, state) else {
            return None;
        };
        let Shape::Instance { class } = &self.nodes.get(target as usize)?.shape else {
            return None;
        };
        let class = self.classes.get(*class as usize)?;
        // The default state: a dictionary for `__dict__`, or the pair
        // `(dictionary or None, {slot: value})`.
        let (nodes, pair) = match &self.nodes.get(state as usize)?.shape {
            Shape::Dict { .. } => (
                StateNodes {
                    dict: Some(state),
                    slots: None,
                },
                None,
            ),
            Shape::Tuple {
                len: 2,
                pair: [first, StateItem::Node(slots)],
            } => {
                let dict = match first {
                    StateItem::None => None,
                    StateItem::Node(dict) => Some(*dict),
                    StateItem::Other => return None,
                };
                (
                    StateNodes {
                        dict,
                        slots: Some(*slots),
                    },
                    Some(state),
                )
            }
            _ => return None,
        };
        if let Some(dict) = nodes.dict {
            if !self.state_keys(dict)?.is_empty() && !class.has_dict {
                return None;
            }
        }
        if let Some(slots) = nodes.slots {
            // `setattr` must reach a member descriptor of `__slots__`.
            if !self
                .state_keys(slots)?
                .iter()
                .all(|name| classes::is_member_slot(&class.class, name))
            {
                return None;
            }
        }
        push(&mut self.builds, (nodes, pair))?;
        push(&mut self.nodes.get_mut(target as usize)?.children, state)
    }
}

/// The second pass, driven by the first pass's [`Plan`].
struct Objects {
    classes: std::vec::IntoIter<Rc<TypeObject>>,
    reachable: Vec<bool>,
    builds: std::vec::IntoIter<StateNodes>,
    /// The index of the next node, in the first pass's numbering.
    next_node: usize,
}

impl Objects {
    /// Register a container with the cycle collector when the result can
    /// reach it. Even an acyclic decoded container can acquire a cycle
    /// through later mutation, so every reachable mutable container is
    /// registered, including those nested below the returned object.
    fn created(&mut self, value: &Object) {
        let index = self.next_node;
        self.next_node += 1;
        if self.reachable.get(index).copied().unwrap_or(true)
            && matches!(
                value,
                Object::List(_) | Object::Dict(_) | Object::Instance(_)
            )
        {
            crate::gc_trace::track(value.clone());
        }
    }

    /// `inst.__dict__[sys.intern(key)] = value` for every item, moving
    /// the state's storage when nothing else can reach it.
    fn apply_dict_state(
        instance: &crate::types::PyInstance,
        state: &RefCell<DictData>,
        movable: bool,
    ) -> Option<()> {
        if movable && instance.dict.get().is_none() {
            // The unpickled state holds arbitrary objects, so the
            // instance can no longer be left untracked; track it before
            // publishing a dictionary that would bypass the write
            // barrier (the record is retired with it).
            instance.ensure_gc_tracked();
            let data = std::mem::take(&mut *state.borrow_mut());
            let interned = data.keys().all(|key| match &key.0 {
                Object::Str(name) => {
                    SharedStr::ptr_eq(&crate::stdlib::sys::intern_shared(name), name)
                }
                _ => false,
            });
            if interned {
                instance.dict.get_or_init(|| Rc::new(RefCell::new(data)));
                return Some(());
            }
            let mut attributes = instance.dict_cell().borrow_mut();
            attributes.try_reserve(data.len()).ok()?;
            for (key, value) in data {
                let Object::Str(name) = &key.0 else {
                    return None;
                };
                let name = crate::stdlib::sys::intern_shared(name);
                attributes.insert(DictKey(Object::Str(name)), value);
            }
            return Some(());
        }
        let state = state.borrow();
        let mut attributes = instance.dict_cell().borrow_mut();
        attributes.try_reserve(state.len()).ok()?;
        for (key, value) in state.iter() {
            let Object::Str(name) = &key.0 else {
                return None;
            };
            let name = crate::stdlib::sys::intern_shared(name);
            attributes.insert(DictKey(Object::Str(name)), value.clone());
        }
        Some(())
    }
}

impl<'a> Sink<'a> for Objects {
    type Value = Object;

    fn scalar(&mut self, value: Scalar<'a>) -> Object {
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
        // Tuples are traced through their owners.
        self.created(&value);
        Some(value)
    }

    fn tuple(&mut self, items: Vec<Object>) -> Option<Object> {
        let value = Object::new_tuple(items);
        self.created(&value);
        Some(value)
    }

    fn extend(&mut self, target: &Object, items: Batch<'_, Object>) -> Option<()> {
        let Object::List(list) = target else {
            return None;
        };
        let mut list = list.borrow_mut();
        list.try_reserve(items.len()).ok()?;
        list.extend(items);
        Some(())
    }

    fn setitems(&mut self, target: &Object, mut items: Batch<'_, Object>) -> Option<()> {
        let Object::Dict(dict) = target else {
            return None;
        };
        if !items.len().is_multiple_of(2) {
            return None;
        }
        let mut dict = dict.borrow_mut();
        dict.try_reserve(items.len() / 2).ok()?;
        while let Some(key) = items.next() {
            dict.insert(DictKey(key), items.next()?);
        }
        Some(())
    }

    fn global(&mut self, _module: &Object, _name: &Object) -> Option<Object> {
        Some(Object::Type(self.classes.next()?))
    }

    fn newobj(&mut self, class: &Object, _args: &Object) -> Option<Object> {
        let Object::Type(class) = class else {
            return None;
        };
        // `object.__new__(cls)`, including its cycle-collector registration.
        let instance = Object::Instance(Rc::new(crate::types::PyInstance::new(class.clone())));
        self.created(&instance);
        Some(instance)
    }

    fn build(&mut self, target: &Object, state: Object) -> Option<()> {
        let Object::Instance(instance) = target else {
            return None;
        };
        let (dict, slots) = match &state {
            Object::Dict(dict) => (Some(dict), None),
            Object::Tuple(items) if items.len() == 2 => {
                let Object::Dict(slots) = &items[1] else {
                    return None;
                };
                match &items[0] {
                    Object::None => (None, Some(slots)),
                    Object::Dict(dict) => (Some(dict), Some(slots)),
                    _ => return None,
                }
            }
            _ => return None,
        };
        let movable = self.builds.next()?;
        if let Some(dict) = dict {
            if !dict.borrow().is_empty() {
                Self::apply_dict_state(instance, dict, movable.dict.is_some())?;
            }
        }
        if let Some(slots) = slots {
            // `setattr(inst, key, value)` on a verified member descriptor.
            instance.ensure_gc_tracked();
            let mut storage = instance.slots.borrow_mut();
            if movable.slots.is_some() {
                let data = std::mem::take(&mut *slots.borrow_mut());
                for (key, value) in data {
                    let Object::Str(name) = &key.0 else {
                        return None;
                    };
                    storage.insert_shared(name, value);
                }
            } else {
                let slots = slots.borrow();
                for (key, value) in slots.iter() {
                    let Object::Str(name) = &key.0 else {
                        return None;
                    };
                    storage.insert_shared(name, value.clone());
                }
            }
        }
        Some(())
    }
}

/// Decode only after the preflight succeeds without creating Python objects.
fn decode(
    data: &[u8],
    validate: impl FnOnce(&Opcodes) -> bool,
    context: &dyn Fn() -> Option<DecodeContext>,
) -> Option<Object> {
    if data.len() < 3 || data[0] != 0x80 || !matches!(data[1], 4 | 5) {
        return None;
    }
    // Checking the whole dispatch table has a fixed cost. For longer
    // streams, avoid recording every opcode and pay that fixed cost once.
    // Both specializations still validate the entire supported stream.
    if data.len() <= 512 {
        decode_with_probe::<true>(data, validate, context)
    } else {
        // A replaced reader may ignore the input or reject it immediately.
        // Check the full table before scanning a large stream so that such
        // readers keep their existing fallback cost.
        if !validate(&Opcodes([true; 256])) {
            return None;
        }
        decode_with_probe::<false>(data, |_| true, context)
    }
}

fn decode_with_probe<const RECORD_OPCODES: bool>(
    data: &[u8],
    validate: impl FnOnce(&Opcodes) -> bool,
    context: &dyn Fn() -> Option<DecodeContext>,
) -> Option<Object> {
    let mut probe = Probe::<RECORD_OPCODES>::new(context);
    let root = parse(data, &mut probe)?;
    // A class is only ever the operand of NEWOBJ, never a result.
    if matches!(root, Kind::Class(_)) {
        return None;
    }
    probe.acyclic_and_bounded()?;
    // Unsupported inputs return before checking Python dispatch functions.
    // Guard supported instructions before any Python objects are constructed.
    if !validate(&probe.opcodes) {
        return None;
    }
    // Drop the graph scratch before allocating the returned Python objects.
    let plan = probe.plan(root)?;
    parse(
        data,
        &mut Objects {
            classes: plan.classes.into_iter(),
            reachable: plan.reachable,
            builds: plan.builds.into_iter(),
            next_node: 0,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode(data: &[u8]) -> Option<Object> {
        super::decode(data, |_| true, &|| None)
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

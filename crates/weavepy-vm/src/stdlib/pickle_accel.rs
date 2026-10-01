//! Guarded protocol-4/5 encoding and decoding of built-in pickle data and of
//! instances of plain classes.
//!
//! Decoding builds the result in one pass and publishes it only once the
//! whole stream has been accepted. Cycles, deep graphs, custom
//! reconstruction, and unsupported or malformed input drop everything built
//! so far, which runs no Python code, and return to the existing unpickler.
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
use crate::object::{BuiltinFn, DictData, DictKey, DictMap, Object, PyFunction, PyModule, StrKey};
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
        Rc::as_ptr(function) == self.function.as_ptr() && self.holds_function(function)
    }

    /// Whether the guarded function is still alive and unchanged.
    fn holds_live(&self) -> bool {
        if self.function.strong_count() == 0 {
            return false;
        }
        // SAFETY: a live strong count keeps the function allocated, and
        // nothing here can release it.
        self.holds_function(unsafe { &*self.function.as_ptr() })
    }

    /// `function` (the guarded one) still runs the guarded code with its
    /// compiled defaults.
    fn holds_function(&self, function: &PyFunction) -> bool {
        if Rc::as_ptr(&function.code.borrow()) != self.code.as_ptr() {
            return false;
        }
        // Only an assignment to `__defaults__` or `__kwdefaults__` raises
        // the flag, so the common function skips the slot probes.
        if !function.defaults_maybe_overridden() {
            return true;
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
        // An unchanged version proves the class holds the same functions;
        // each one's code and defaults can still change in place.
        self.class.strong_count() > 0
            // SAFETY: a live strong count keeps the class allocated.
            && unsafe { &*self.class.as_ptr() }.attr_version.get() == self.version
            && self.functions.iter().all(FunctionGuard::holds_live)
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

const STACK_GLOBAL: u8 = 0x93;
const NEWOBJ: u8 = 0x81;
const BUILD: u8 = b'b';
/// The deepest container nesting that is decoded natively.
const MAX_DEPTH: u32 = 128;

/// `sys.modules`, produced on demand once a stream names a class.
struct DecodeContext {
    modules: Rc<RefCell<DictData>>,
}

#[inline(always)]
fn push<T>(items: &mut Vec<T>, value: T) -> Option<()> {
    // (The fallible reservation only when the vector is full.)
    if items.len() == items.capacity() {
        items.try_reserve(1).ok()?;
    }
    items.push(value);
    Some(())
}

struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
    /// The end of the current frame, or of the data outside a frame.
    limit: usize,
    framed: bool,
}

impl<'a> Reader<'a> {
    fn new(data: &'a [u8], pos: usize) -> Self {
        Self {
            data,
            pos,
            limit: data.len(),
            framed: false,
        }
    }

    #[inline(always)]
    fn read(&mut self, size: usize) -> Option<&'a [u8]> {
        if size <= self.limit - self.pos {
            let value = self.data.get(self.pos..self.pos + size)?;
            self.pos += size;
            return Some(value);
        }
        self.read_past_limit(size)
    }

    /// A read beyond the current limit, which leaves a frame that ends
    /// where the read begins, and fails otherwise.
    #[cold]
    fn read_past_limit(&mut self, size: usize) -> Option<&'a [u8]> {
        if !self.framed || self.pos != self.limit {
            return None;
        }
        self.framed = false;
        self.limit = self.data.len();
        let end = self.pos.checked_add(size)?;
        let value = self.data.get(self.pos..end)?;
        self.pos = end;
        Some(value)
    }

    #[inline(always)]
    fn byte(&mut self) -> Option<u8> {
        if self.pos < self.limit {
            let value = *self.data.get(self.pos)?;
            self.pos += 1;
            return Some(value);
        }
        Some(self.read_past_limit(1)?[0])
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
        if self.framed {
            return None;
        }
        let size = self.size64()?;
        let end = self.pos.checked_add(size)?;
        if end > self.data.len() {
            return None;
        }
        self.framed = true;
        self.limit = end;
        Some(())
    }
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

/// Whether the items from `begin` on and the container below them all lie
/// above the innermost mark.
fn is_batch<V>(stack: &[V], begin: usize, marks: &[usize]) -> bool {
    begin != 0 && begin <= stack.len() && marks.last().is_none_or(|mark| begin > *mark)
}

/// A value on the stack or in the memo, with the number of the [`Node`]
/// that describes it, [`LEAF`], or [`CLASS`].
type Entry = (Object, u32);

/// The node number of a value that isn't a container.
const LEAF: u32 = 0;
/// The node number of a class, which only NEWOBJ may consume.
const CLASS: u32 = u32::MAX;
/// The node number of a memo slot whose container's storage moved into an
/// instance. Reading it returns to the full unpickler.
const SPENT: u32 = u32::MAX - 1;
/// [`Node::memo`] for a container that no memo slot holds.
const UNMEMOIZED: u32 = u32::MAX;
/// [`Node::memo`] for a container that more than one memo slot holds.
const MEMOIZED_TWICE: u32 = u32::MAX - 1;

/// What the decoder knows about one container or instance it built.
struct Node {
    /// One more than the height of the tallest container inside it.
    height: u32,
    /// Whether a container, tuple, or BUILD has taken it. Only a container
    /// that nothing holds yet may receive items, so an item can never reach
    /// its own container (a cycle), and no height ever has to propagate
    /// to an enclosing container.
    held: bool,
    /// The memo slot that holds it, or one of the markers above.
    memo: u32,
    /// For a two-item tuple, the node numbers of its items.
    pair: [u32; 2],
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
        // decoded instruction. This scratch is local to one decode.
        self.0[usize::from(opcode)] = true;
    }

    fn contains(&self, opcode: u8) -> bool {
        self.0[usize::from(opcode)]
    }
}

/// Builds the result in one pass over the stream. The objects stay private
/// until the whole stream has been accepted: any reason to return to the
/// full unpickler drops them, and dropping them runs no Python code (an
/// instance's class is proven to have no finalizer).
struct Decoder<'c, const RECORD_OPCODES: bool> {
    opcodes: Opcodes,
    nodes: Vec<Node>,
    memo: Vec<Entry>,
    /// Every class that STACK_GLOBAL resolved.
    classes: Vec<PlainClass>,
    context: &'c dyn Fn() -> Option<DecodeContext>,
    resolved: Option<DecodeContext>,
    /// The `__slots__` members already verified in this stream (see
    /// `classes::is_member_slot`): every instance of a class names the
    /// same ones.
    member_slots: Vec<(*const TypeObject, SharedStr)>,
    /// State keys and their interned strings (see [`Self::intern`]).
    interned: Vec<(SharedStr, SharedStr)>,
    /// Every list and dictionary, and every instance tracked from birth, in
    /// creation order. Those the result can reach join the cycle collector
    /// once the stream is accepted.
    containers: Vec<Object>,
}

/// A decoder's vectors, kept empty between calls with their capacity, so a
/// `loads` loop neither reallocates nor regrows them.
#[derive(Default)]
struct Scratch {
    stack: Vec<Entry>,
    marks: Vec<usize>,
    memo: Vec<Entry>,
    nodes: Vec<Node>,
    containers: Vec<Object>,
}

impl Scratch {
    /// The largest vector kept between calls.
    const MAX_KEPT: usize = 1 << 14;

    fn take() -> Self {
        SPARE_SCRATCH
            .with(std::cell::Cell::take)
            .unwrap_or_default()
    }

    /// Empty the vectors (releasing what they hold, which runs no Python
    /// code) and keep them for the next call.
    fn keep(mut self) {
        self.stack.clear();
        self.marks.clear();
        self.memo.clear();
        self.nodes.clear();
        self.containers.clear();
        if self.stack.capacity().max(self.marks.capacity()) <= Self::MAX_KEPT
            && self.memo.capacity().max(self.nodes.capacity()) <= Self::MAX_KEPT
            && self.containers.capacity() <= Self::MAX_KEPT
        {
            SPARE_SCRATCH.with(|spare| spare.set(Some(self)));
        }
    }
}

thread_local! {
    static SPARE_SCRATCH: std::cell::Cell<Option<Scratch>> = const { std::cell::Cell::new(None) };
}

impl<'c, const RECORD_OPCODES: bool> Decoder<'c, RECORD_OPCODES> {
    fn new(context: &'c dyn Fn() -> Option<DecodeContext>, scratch: &mut Scratch) -> Self {
        Self {
            opcodes: Opcodes([!RECORD_OPCODES; 256]),
            nodes: std::mem::take(&mut scratch.nodes),
            memo: std::mem::take(&mut scratch.memo),
            classes: Vec::new(),
            context,
            resolved: None,
            member_slots: Vec::new(),
            interned: Vec::new(),
            containers: std::mem::take(&mut scratch.containers),
        }
    }

    /// Return the vectors to `scratch`.
    fn recycle(self, scratch: &mut Scratch) {
        scratch.nodes = self.nodes;
        scratch.memo = self.memo;
        scratch.containers = self.containers;
    }

    #[inline(always)]
    fn opcode(&mut self, value: u8) {
        if RECORD_OPCODES {
            self.opcodes.insert(value);
        }
    }

    fn node(&mut self, height: u32, pair: [u32; 2]) -> Option<u32> {
        if height > MAX_DEPTH {
            return None;
        }
        let number = u32::try_from(self.nodes.len())
            .ok()?
            .checked_add(1)
            .filter(|&number| number < SPENT)?;
        push(
            &mut self.nodes,
            Node {
                height,
                held: false,
                memo: UNMEMOIZED,
                pair,
            },
        )?;
        Some(number)
    }

    /// A new, empty list or dictionary.
    fn container(&mut self, value: Object) -> Option<Entry> {
        let node = self.node(1, [LEAF; 2])?;
        push(&mut self.containers, value.clone())?;
        Some((value, node))
    }

    /// Mark the value of node `node` as taken by a container, and return its
    /// height. A class can't be taken.
    #[inline]
    fn hold(&mut self, node: u32) -> Option<u32> {
        if node == LEAF {
            return Some(0);
        }
        // (CLASS lies beyond every node.)
        let node = self.nodes.get_mut(node as usize - 1)?;
        node.held = true;
        Some(node.height)
    }

    /// Let the container of node `node`, which nothing may hold yet, take
    /// items no taller than `height`.
    fn grow(&mut self, node: u32, height: u32) -> Option<()> {
        let node = self.nodes.get_mut((node as usize).checked_sub(1)?)?;
        if node.held {
            return None;
        }
        node.height = node.height.max(height + 1);
        (node.height <= MAX_DEPTH).then_some(())
    }

    fn memoize(&mut self, entry: &Entry) -> Option<()> {
        let slot = u32::try_from(self.memo.len()).ok()?;
        if let Some(node) = (entry.1 as usize)
            .checked_sub(1)
            .and_then(|index| self.nodes.get_mut(index))
        {
            node.memo = if node.memo == UNMEMOIZED && slot < MEMOIZED_TWICE {
                slot
            } else {
                MEMOIZED_TWICE
            };
        }
        push(&mut self.memo, entry.clone())
    }

    fn get(&self, slot: usize) -> Option<Entry> {
        let entry = self.memo.get(slot)?;
        (entry.1 != SPENT).then(|| entry.clone())
    }

    /// The tallest of `items`, after marking them taken.
    fn hold_all(&mut self, items: &[Entry]) -> Option<u32> {
        let mut height = 0;
        for &(_, node) in items {
            height = height.max(self.hold(node)?);
        }
        Some(height)
    }

    fn tuple(&mut self, stack: &mut Vec<Entry>, begin: usize) -> Option<Entry> {
        let height = self.hold_all(&stack[begin..])?;
        let pair = match &stack[begin..] {
            [(_, first), (_, second)] => [*first, *second],
            _ => [LEAF; 2],
        };
        let value = if begin == stack.len() {
            Object::new_tuple(Vec::new())
        } else {
            Object::Tuple(crate::tuple_storage::TupleStorage::from_exact_iter(
                stack.drain(begin..).map(|(value, _)| value),
            ))
        };
        Some((value, self.node(height + 1, pair)?))
    }

    /// APPEND(S): the items from `begin` on join the list below them.
    fn extend(&mut self, stack: &mut Vec<Entry>, begin: usize, marks: &[usize]) -> Option<()> {
        if !is_batch(stack, begin, marks) {
            return None;
        }
        // Taking the items first rejects a list that receives itself.
        let height = self.hold_all(&stack[begin..])?;
        let (Object::List(list), target) = &stack[begin - 1] else {
            return None;
        };
        let list = list.clone();
        self.grow(*target, height)?;
        let mut list = list.borrow_mut();
        list.try_reserve(stack.len() - begin).ok()?;
        list.extend(stack.drain(begin..).map(|(value, _)| value));
        Some(())
    }

    /// SETITEM(S): the pairs from `begin` on join the dictionary below them.
    fn setitems(&mut self, stack: &mut Vec<Entry>, begin: usize, marks: &[usize]) -> Option<()> {
        if !is_batch(stack, begin, marks) || !(stack.len() - begin).is_multiple_of(2) {
            return None;
        }
        let mut height = 0;
        for pair in stack[begin..].chunks_exact(2) {
            // Tuple and float keys retain the full unpickler, including
            // arbitrary nesting and NaN key-identity behavior. So do
            // instances, whose hashing can call Python code.
            if pair[0].1 != LEAF
                || !matches!(
                    pair[0].0,
                    Object::Str(_)
                        | Object::None
                        | Object::Bool(_)
                        | Object::Int(_)
                        | Object::Long(_)
                        | Object::Bytes(_)
                )
            {
                return None;
            }
            height = height.max(self.hold(pair[1].1)?);
        }
        let (Object::Dict(dict), target) = &stack[begin - 1] else {
            return None;
        };
        let dict = dict.clone();
        self.grow(*target, height)?;
        let mut dict = dict.borrow_mut();
        let map: &mut DictMap = &mut dict;
        map.try_reserve((stack.len() - begin) / 2).ok()?;
        let mut items = stack.drain(begin..);
        while let Some((key, _)) = items.next() {
            map.insert(DictKey(key), items.next()?.0);
        }
        Some(())
    }

    fn global(&mut self, module: &Object, name: &Object) -> Option<Entry> {
        let (Object::Str(module), Object::Str(name)) = (module, name) else {
            return None;
        };
        if self.resolved.is_none() {
            self.resolved = Some((self.context)()?);
        }
        // An absent module keeps the full unpickler, which imports it.
        let class = PlainClass::resolve(self.resolved.as_ref()?, module, name)?;
        let value = Object::Type(class.class.clone());
        push(&mut self.classes, class)?;
        Some((value, CLASS))
    }

    fn newobj(&mut self, class: Entry, args: Entry) -> Option<Entry> {
        let ((Object::Type(class), CLASS), (Object::Tuple(args), _)) = (class, args) else {
            return None;
        };
        // `object.__new__(cls)` accepts no further arguments.
        if !args.is_empty() {
            return None;
        }
        // `object.__new__(cls)`, including its cycle-collector registration:
        // deferred, as for a plain class's ordinary construction, until the
        // instance could hold a non-atomic value (the state paths below
        // track it then).
        let value = if class.native_kind.get() == 0
            && !class.flags.is_builtin
            && !class.instances_need_finalize()
        {
            Object::Instance(crate::types::PyInstance::new_deferred(class))
        } else {
            let value = Object::Instance(Rc::new(crate::types::PyInstance::new(class)));
            push(&mut self.containers, value.clone())?;
            value
        };
        Some((value, self.node(1, [LEAF; 2])?))
    }

    /// [`classes::is_member_slot`], remembered for this stream. (No Python
    /// runs during decoding, so the class can't change in between.)
    fn is_member_slot(&mut self, class: &Rc<TypeObject>, name: &SharedStr) -> bool {
        let key = Rc::as_ptr(class);
        if self
            .member_slots
            .iter()
            .any(|(c, n)| *c == key && n.as_ref() == name.as_ref())
        {
            return true;
        }
        let yes = classes::is_member_slot(class, name);
        if yes && self.member_slots.len() < 64 {
            self.member_slots.push((key, name.clone()));
        }
        yes
    }

    /// Whether a container of node `node`, with `count` strong references,
    /// is reachable only through the `holders` references the caller
    /// accounts for and its one memo slot. Then its storage may move, and
    /// the memo slot (returned, when there is one) must be spent.
    fn movable(&self, count: usize, node: u32, holders: usize) -> Option<Option<u32>> {
        let node = self.nodes.get((node as usize).checked_sub(1)?)?;
        match node.memo {
            UNMEMOIZED => (count == holders).then_some(None),
            MEMOIZED_TWICE => None,
            slot => (count == holders + 1).then_some(Some(slot)),
        }
    }

    /// `sys.intern(name)`, remembered for this stream: every instance's
    /// state usually names the same memoized strings.
    fn intern(&mut self, name: &SharedStr) -> SharedStr {
        if let Some((_, interned)) = self
            .interned
            .iter()
            .find(|(seen, _)| SharedStr::ptr_eq(seen, name))
        {
            return interned.clone();
        }
        let interned = crate::stdlib::sys::intern_shared(name);
        if self.interned.len() < 64 {
            self.interned.push((name.clone(), interned.clone()));
        }
        interned
    }

    /// `inst.__dict__[sys.intern(key)] = value` for every item, moving the
    /// state's storage when nothing else can reach it.
    fn apply_dict_state(
        &mut self,
        instance: &crate::types::PyInstance,
        state: &RefCell<DictData>,
        movable: bool,
    ) -> Option<()> {
        if movable {
            // The unpickled state holds arbitrary objects, so the
            // instance can no longer be left untracked; track it before
            // publishing a table that bypasses the write barrier (the
            // record is retired with it).
            instance.ensure_gc_tracked();
            let mut table: DictMap = std::mem::take(&mut **state.borrow_mut());
            // An interned key is equal to the key it replaces and has the
            // same hash, so the table stays valid.
            for (key, _) in indexmap::map::MutableKeys::iter_mut2(&mut table) {
                let Object::Str(name) = &key.0 else {
                    return None;
                };
                key.0 = Object::Str(self.intern(name));
            }
            let mut attributes = instance.dict_cell().borrow_mut();
            if attributes.is_empty() {
                **attributes = table;
                return Some(());
            }
            attributes.try_reserve(table.len()).ok()?;
            for (key, value) in table {
                attributes.insert(key, value);
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
            let name = self.intern(name);
            attributes.insert(DictKey(Object::Str(name)), value.clone());
        }
        Some(())
    }

    fn build(&mut self, target: &Entry, state: Entry) -> Option<()> {
        let (Object::Instance(instance), target) = target else {
            return None;
        };
        let (state, state_node) = state;
        let class = instance.cls();
        let has_dict = self
            .classes
            .iter()
            .find(|plain| Rc::ptr_eq(&plain.class, &class))?
            .has_dict;
        // The default state: a dictionary for `__dict__`, or the pair
        // `(dictionary or None, {slot: value})`.
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
        if let Some(dict) = dict {
            let dict = dict.borrow();
            if !dict.keys().all(|key| matches!(key.0, Object::Str(_)))
                || (!dict.is_empty() && !has_dict)
            {
                return None;
            }
        }
        if let Some(slots) = slots {
            // `setattr` must reach a member descriptor of `__slots__`.
            for key in slots.borrow().keys() {
                let Object::Str(name) = &key.0 else {
                    return None;
                };
                if !self.is_member_slot(&class, name) {
                    return None;
                }
            }
        }
        // The instance takes the state's items, so it must be free to
        // change, and the state can't reach it.
        let height = self.hold(state_node)?;
        self.grow(*target, height)?;

        // A state container that nothing else can reach gives its storage
        // to the instance. Its memo slot is spent, and so is the pair's:
        // reading either would see the emptied container.
        let mut spent = [None; 3];
        let (move_dict, move_slots) = match &state {
            Object::Tuple(pair) => {
                let items = self.nodes.get((state_node as usize).checked_sub(1)?)?.pair;
                match self.movable(
                    crate::shared_value::ThinArc::strong_count(pair),
                    state_node,
                    1,
                ) {
                    None => (false, false),
                    Some(slot) => {
                        // Each item is held by the pair and by `containers`.
                        let dict =
                            dict.and_then(|d| self.movable(Rc::strong_count(d), items[0], 2));
                        let slots =
                            slots.and_then(|s| self.movable(Rc::strong_count(s), items[1], 2));
                        if dict.is_some() || slots.is_some() {
                            spent = [slot, dict.flatten(), slots.flatten()];
                        }
                        (dict.is_some(), slots.is_some())
                    }
                }
            }
            _ => {
                // Held by `state` and by `containers`.
                let dict = dict.and_then(|d| self.movable(Rc::strong_count(d), state_node, 2));
                spent[0] = dict.flatten();
                (dict.is_some(), false)
            }
        };
        if let Some(dict) = dict {
            if !dict.borrow().is_empty() {
                self.apply_dict_state(instance, dict, move_dict)?;
            }
        }
        if let Some(slots) = slots {
            // `setattr(inst, key, value)` on a verified member descriptor,
            // whose write barrier tracks a deferred instance at its first
            // non-atomic value.
            if slots.borrow().iter().any(|(_, v)| !v.is_gc_atomic()) {
                instance.ensure_gc_tracked();
            }
            let mut storage = instance.slots.borrow_mut();
            if move_slots {
                let data = std::mem::take(&mut *slots.borrow_mut());
                for (key, value) in data {
                    let Object::Str(name) = &key.0 else {
                        return None;
                    };
                    storage.insert_shared(name, value);
                }
            } else {
                for (key, value) in slots.borrow().iter() {
                    let Object::Str(name) = &key.0 else {
                        return None;
                    };
                    storage.insert_shared(name, value.clone());
                }
            }
        }
        for slot in spent.into_iter().flatten() {
            *self.memo.get_mut(slot as usize)? = (Object::None, SPENT);
        }
        Some(())
    }

    #[allow(clippy::too_many_lines)]
    fn parse(
        &mut self,
        data: &[u8],
        stack: &mut Vec<Entry>,
        marks: &mut Vec<usize>,
    ) -> Option<Entry> {
        self.opcode(0x80);
        let mut reader = Reader::new(data, 2);
        stack.try_reserve(64).ok()?;
        self.memo.try_reserve(data.len() / 16).ok()?;
        self.nodes.try_reserve(data.len() / 16).ok()?;
        loop {
            let opcode = reader.byte()?;
            self.opcode(opcode);
            let entry = match opcode {
                b'.' => {
                    return (marks.is_empty() && stack.len() == 1)
                        .then(|| stack.pop())
                        .flatten()
                        // A class is only ever the operand of NEWOBJ.
                        .filter(|(_, node)| *node != CLASS);
                }
                0x95 => {
                    reader.frame()?;
                    continue;
                }
                b'(' => {
                    push(&mut *marks, stack.len())?;
                    continue;
                }
                0x94 => {
                    self.memoize(top(&*stack, &*marks)?)?;
                    continue;
                }
                b'h' => self.get(usize::from(reader.byte()?))?,
                b'j' => self.get(usize::try_from(reader.u32()?).ok()?)?,
                b'N' => (Object::None, LEAF),
                0x88 => (Object::Bool(true), LEAF),
                0x89 => (Object::Bool(false), LEAF),
                b'K' => (Object::Int(i64::from(reader.byte()?)), LEAF),
                b'M' => (
                    Object::Int(i64::from(u16::from_le_bytes(
                        reader.read(2)?.try_into().ok()?,
                    ))),
                    LEAF,
                ),
                b'J' => (
                    Object::Int(i64::from(i32::from_le_bytes(
                        reader.read(4)?.try_into().ok()?,
                    ))),
                    LEAF,
                ),
                0x8a | 0x8b => {
                    let size = if opcode == 0x8a {
                        usize::from(reader.byte()?)
                    } else {
                        let size = i32::from_le_bytes(reader.read(4)?.try_into().ok()?);
                        usize::try_from(size).ok()?
                    };
                    let bytes = reader.read(size)?;
                    (
                        Object::int_from_bigint(BigInt::from_signed_bytes_le(bytes)),
                        LEAF,
                    )
                }
                b'G' => (
                    Object::Float(f64::from_be_bytes(reader.read(8)?.try_into().ok()?)),
                    LEAF,
                ),
                0x8c | b'X' | 0x8d | b'C' | b'B' | 0x8e => {
                    let size = match opcode {
                        0x8c | b'C' => usize::from(reader.byte()?),
                        b'X' | b'B' => usize::try_from(reader.u32()?).ok()?,
                        _ => reader.size64()?,
                    };
                    let bytes = reader.read(size)?;
                    let value = if matches!(opcode, 0x8c | b'X' | 0x8d) {
                        // Surrogate-pass strings stay on the existing WStr path.
                        let text = if bytes.is_ascii() {
                            // SAFETY: ASCII text is valid UTF-8.
                            unsafe { std::str::from_utf8_unchecked(bytes) }
                        } else {
                            std::str::from_utf8(bytes).ok()?
                        };
                        Object::Str(SharedStr::from(text))
                    } else {
                        Object::new_bytes(bytes)
                    };
                    (value, LEAF)
                }
                b']' => self.container(Object::new_list(Vec::new()))?,
                b'}' => self.container(Object::new_dict())?,
                b')' => (Object::new_tuple(Vec::new()), self.node(1, [LEAF; 2])?),
                0x85..=0x87 => {
                    let count = usize::from(opcode - 0x84);
                    let begin = stack.len().checked_sub(count)?;
                    if marks.last().is_some_and(|mark| begin < *mark) {
                        return None;
                    }
                    self.tuple(&mut *stack, begin)?
                }
                b't' => {
                    let mark = marks.pop()?;
                    self.tuple(&mut *stack, mark)?
                }
                b'a' => {
                    let begin = stack.len().checked_sub(1)?;
                    self.extend(&mut *stack, begin, &*marks)?;
                    continue;
                }
                b'e' => {
                    let mark = marks.pop()?;
                    self.extend(&mut *stack, mark, &*marks)?;
                    continue;
                }
                b's' => {
                    let begin = stack.len().checked_sub(2)?;
                    self.setitems(&mut *stack, begin, &*marks)?;
                    continue;
                }
                b'u' => {
                    let mark = marks.pop()?;
                    self.setitems(&mut *stack, mark, &*marks)?;
                    continue;
                }
                STACK_GLOBAL => {
                    let (name, _) = pop(&mut *stack, &*marks)?;
                    let (module, _) = pop(&mut *stack, &*marks)?;
                    self.global(&module, &name)?
                }
                NEWOBJ => {
                    let args = pop(&mut *stack, &*marks)?;
                    let class = pop(&mut *stack, &*marks)?;
                    self.newobj(class, args)?
                }
                BUILD => {
                    let state = pop(&mut *stack, &*marks)?;
                    self.build(top(&*stack, &*marks)?, state)?;
                    continue;
                }
                // GLOBAL, REDUCE, NEWOBJ_EX, persistent IDs, external buffers,
                // legacy protocols, and all other operations retain the full
                // unpickler.
                _ => return None,
            };
            push(&mut *stack, entry)?;
        }
    }

    /// Publish the accepted result. Every container that the result can
    /// reach is registered with the cycle collector: even an acyclic decoded
    /// container can acquire a cycle through later mutation. Releasing the
    /// memo first leaves the result's references as the only ones beyond
    /// `containers`' own.
    fn finish(&mut self, root: Object) -> Object {
        self.memo.clear();
        for value in self.containers.drain(..) {
            let reachable = match &value {
                Object::List(list) => Rc::strong_count(list) > 1,
                Object::Dict(dict) => Rc::strong_count(dict) > 1,
                Object::Instance(instance) => Rc::strong_count(instance) > 1,
                _ => false,
            };
            if reachable {
                crate::gc_trace::track_built(&value);
            }
        }
        root
    }
}

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
        decode_with::<true>(data, validate, context)
    } else {
        // A replaced reader may ignore the input or reject it immediately.
        // Check the full table before scanning a large stream so that such
        // readers keep their existing fallback cost.
        if !validate(&Opcodes([true; 256])) {
            return None;
        }
        decode_with::<false>(data, |_| true, context)
    }
}

fn decode_with<const RECORD_OPCODES: bool>(
    data: &[u8],
    validate: impl FnOnce(&Opcodes) -> bool,
    context: &dyn Fn() -> Option<DecodeContext>,
) -> Option<Object> {
    let mut scratch = Scratch::take();
    let mut decoder = Decoder::<RECORD_OPCODES>::new(context, &mut scratch);
    let result = decoder
        .parse(data, &mut scratch.stack, &mut scratch.marks)
        // A short stream's Python dispatch functions are checked only once
        // the stream proves supported. Rejecting it then discards what was
        // built.
        .filter(|_| validate(&decoder.opcodes))
        .map(|(root, _)| decoder.finish(root));
    decoder.recycle(&mut scratch);
    scratch.keep();
    result
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
            // A list that another list holds can't change (conservatively,
            // although this one would not form a cycle).
            &b"\x80\x05]\x94]\x94ah\x01K\x01a\x86."[..],
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

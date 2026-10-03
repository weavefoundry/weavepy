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

use std::hash::BuildHasher;

use crate::shared_value::{SharedSlice, SharedStr};
use indexmap::map::raw_entry_v1::{RawEntryApiV1, RawEntryMut};
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
        let slots = function.slots().borrow();
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

    /// Whether the functions that a stream of `opcodes` would run are still
    /// the guarded ones, with their guarded code and defaults. Checked once
    /// the stream proves supported, after [`Self::matches_class`]: an
    /// unchanged class version (which every change to the class or its
    /// bases advances) proves that `__init__`, `load`, and `find_class`
    /// still resolve to the guarded functions.
    fn holds(&self, opcodes: &Opcodes) -> bool {
        if !self.init.holds_live() || !self.load.holds_live() {
            return false;
        }
        // STACK_GLOBAL is the only supported instruction that calls a
        // method outside the dispatch table.
        if opcodes.contains(STACK_GLOBAL)
            && !self
                .find_class
                .as_ref()
                .is_some_and(FunctionGuard::holds_live)
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
                Object::Bytes(data) => decode(data, |opcodes| guard.holds(opcodes), &context),
                // Decoding runs no Python code, so the borrow is safe.
                Object::ByteArray(data) => {
                    decode(&data.borrow(), |opcodes| guard.holds(opcodes), &context)
                }
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

/// Push `$value` (with node number `$node`, by default [`LEAF`]) onto the
/// decoder stack, written straight into its slot. A value built in a
/// temporary (with a byte store for its variant) and then copied with word
/// loads stalls store forwarding, and the decoder's opcode arms otherwise
/// share such a temporary.
macro_rules! push_entry {
    ($stack:expr, $value:expr) => {
        push_entry!($stack, $value, LEAF)
    };
    ($stack:expr, $value:expr, $node:expr) => {
        push_in_place!($stack, ($value, $node))
    };
}

/// Push `$value` onto the vector `$items`, written straight into its slot
/// (see [`push_entry`]).
macro_rules! push_in_place {
    ($items:expr, $value:expr) => {{
        let items = &mut *$items;
        if items.len() == items.capacity() {
            items.try_reserve(1).ok()?;
        }
        items.spare_capacity_mut().first_mut()?.write($value);
        // SAFETY: the slot past the end has just been initialized.
        unsafe { items.set_len(items.len() + 1) };
    }};
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

    /// Whether the current frame (or the data outside one) continues with
    /// `bytes`, compared byte by byte (not with a call to `memcmp`).
    #[inline(always)]
    fn next_is<const N: usize>(&self, bytes: [u8; N]) -> bool {
        self.limit - self.pos >= N
            && bytes
                .iter()
                .enumerate()
                .all(|(i, byte)| self.data.get(self.pos + i) == Some(byte))
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
type Entry = (Object, NodeId);

/// A node number. It is word-sized, like the rest of an [`Entry`], so an
/// entry is copied with the same word stores that wrote it.
type NodeId = u64;

/// `value.clone()`, in line for the kinds that streams memoize, as a copy
/// of the existing value's words: a value rebuilt in a temporary would be
/// copied with word loads that stall on the byte store of its variant.
#[inline(always)]
fn clone_value(value: &Object) -> Object {
    match value {
        Object::Str(text) => std::mem::forget(text.clone()),
        Object::Tuple(items) => std::mem::forget(items.clone()),
        Object::List(items) => std::mem::forget(items.clone()),
        Object::Dict(items) => std::mem::forget(items.clone()),
        Object::Instance(instance) => std::mem::forget(instance.clone()),
        Object::Type(class) => std::mem::forget(class.clone()),
        Object::None | Object::Bool(_) | Object::Int(_) | Object::Float(_) => {}
        _ => return value.clone(),
    }
    // SAFETY: each kind above is a plain value or a single counted
    // reference, whose clone is a copy of the same words holding one more
    // reference: the one just taken and forgotten, which the copy owns.
    unsafe { std::ptr::read(value) }
}

/// The dictionary (or None) and the slot dictionary of a BUILD state pair.
#[allow(clippy::type_complexity)]
fn state_dicts<'s>(
    first: &'s Object,
    second: &'s Object,
) -> Option<(Option<&'s Rc<RefCell<DictData>>>, &'s Rc<RefCell<DictData>>)> {
    let Object::Dict(slots) = second else {
        return None;
    };
    match first {
        Object::None => Some((None, slots)),
        Object::Dict(dict) => Some((Some(dict), slots)),
        _ => None,
    }
}

/// The node number of a value that isn't a container.
const LEAF: NodeId = 0;
/// The node number of a class, which only NEWOBJ may consume.
const CLASS: NodeId = NodeId::MAX;
/// The node number of a memo slot whose container's storage moved into an
/// instance. Reading it returns to the full unpickler.
const SPENT: NodeId = NodeId::MAX - 1;
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
    pair: [NodeId; 2],
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
struct Decoder<'c> {
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
    /// Emptied state dictionaries that nothing else can reach, with their
    /// tables' capacity, for the next EMPTY_DICT.
    spare_dicts: Vec<Rc<RefCell<DictData>>>,
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

impl<'c> Decoder<'c> {
    fn new(context: &'c dyn Fn() -> Option<DecodeContext>, scratch: &mut Scratch) -> Self {
        Self {
            opcodes: Opcodes([false; 256]),
            nodes: std::mem::take(&mut scratch.nodes),
            memo: std::mem::take(&mut scratch.memo),
            classes: Vec::new(),
            context,
            resolved: None,
            member_slots: Vec::new(),
            interned: Vec::new(),
            spare_dicts: Vec::new(),
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
        self.opcodes.insert(value);
    }

    fn node(&mut self, height: u32, pair: [NodeId; 2]) -> Option<NodeId> {
        if height > MAX_DEPTH {
            return None;
        }
        let number = NodeId::try_from(self.nodes.len())
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

    /// Push a new, empty dictionary, reusing a moved state's when one waits.
    fn empty_dict(&mut self, stack: &mut Vec<Entry>) -> Option<()> {
        let node = self.node(1, [LEAF; 2])?;
        let dict = match self.spare_dicts.pop() {
            // (A spare is already in `containers`.)
            Some(dict) => dict,
            None => {
                let dict = Rc::new(RefCell::new(DictData::default()));
                push_in_place!(&mut self.containers, Object::Dict(dict.clone()));
                dict
            }
        };
        push_entry!(stack, Object::Dict(dict), node);
        Some(())
    }

    /// Push a new, empty list.
    fn empty_list(&mut self, stack: &mut Vec<Entry>) -> Option<()> {
        let node = self.node(1, [LEAF; 2])?;
        let list = Rc::new(RefCell::new(Vec::new()));
        push_in_place!(&mut self.containers, Object::List(list.clone()));
        push_entry!(stack, Object::List(list), node);
        Some(())
    }

    /// Mark the value of node `node` as taken by a container, and return its
    /// height. A class can't be taken.
    #[inline]
    fn hold(&mut self, node: NodeId) -> Option<u32> {
        if node == LEAF {
            return Some(0);
        }
        // (CLASS lies beyond every node.)
        let node = self.nodes.get_mut(usize::try_from(node - 1).ok()?)?;
        node.held = true;
        Some(node.height)
    }

    /// Let the container of node `node`, which nothing may hold yet, take
    /// items no taller than `height`.
    fn grow(&mut self, node: NodeId, height: u32) -> Option<()> {
        let node = self
            .nodes
            .get_mut(usize::try_from(node.checked_sub(1)?).ok()?)?;
        if node.held {
            return None;
        }
        node.height = node.height.max(height + 1);
        (node.height <= MAX_DEPTH).then_some(())
    }

    fn memoize(&mut self, entry: &Entry) -> Option<()> {
        let slot = u32::try_from(self.memo.len()).ok()?;
        if let Some(node) = (entry.1.checked_sub(1))
            .and_then(|index| usize::try_from(index).ok())
            .and_then(|index| self.nodes.get_mut(index))
        {
            node.memo = if node.memo == UNMEMOIZED && slot < MEMOIZED_TWICE {
                slot
            } else {
                MEMOIZED_TWICE
            };
        }
        // The value usually was just pushed, its variant with a byte store:
        // each kind is rebuilt from its fields (not copied by words) into
        // the memo's slot.
        let node = entry.1;
        let memo = &mut self.memo;
        match &entry.0 {
            Object::Str(text) => push_entry!(memo, Object::Str(text.clone()), node),
            Object::Tuple(items) => push_entry!(memo, Object::Tuple(items.clone()), node),
            Object::List(items) => push_entry!(memo, Object::List(items.clone()), node),
            Object::Dict(items) => push_entry!(memo, Object::Dict(items.clone()), node),
            Object::Instance(instance) => {
                push_entry!(memo, Object::Instance(instance.clone()), node);
            }
            Object::Type(class) => push_entry!(memo, Object::Type(class.clone()), node),
            other => push_entry!(memo, other.clone(), node),
        }
        Some(())
    }

    /// A MEMOIZE that follows the instruction that pushed the value, taken
    /// without a dispatch: the pickler memoizes nearly every value it
    /// writes.
    #[inline(always)]
    fn memoize_next(&mut self, reader: &mut Reader<'_>, stack: &[Entry]) -> Option<()> {
        if reader.next_is([0x94]) {
            reader.pos += 1;
            self.opcode(0x94);
            self.memoize(stack.last()?)?;
        }
        Some(())
    }

    /// GET: push the value of memo slot `slot`.
    #[inline]
    fn push_memo(&self, stack: &mut Vec<Entry>, slot: usize) -> Option<()> {
        let (value, node) = self.memo.get(slot)?;
        if *node == SPENT {
            return None;
        }
        push_entry!(stack, clone_value(value), *node);
        Some(())
    }

    /// The tallest of `items`, after marking them taken.
    fn hold_all(&mut self, items: &[Entry]) -> Option<u32> {
        let mut height = 0;
        for &(_, node) in items {
            height = height.max(self.hold(node)?);
        }
        Some(height)
    }

    /// Replace the items from `begin` on with a tuple of them.
    fn tuple(&mut self, stack: &mut Vec<Entry>, begin: usize) -> Option<()> {
        let height = self.hold_all(&stack[begin..])?;
        let pair = match &stack[begin..] {
            [(_, first), (_, second)] => [*first, *second],
            _ => [LEAF; 2],
        };
        let node = self.node(height + 1, pair)?;
        if begin == stack.len() {
            push_entry!(stack, Object::new_tuple(Vec::new()), node);
        } else {
            let items = crate::tuple_storage::TupleStorage::from_exact_iter(
                stack.drain(begin..).map(|(value, _)| value),
            );
            push_entry!(stack, Object::Tuple(items), node);
        }
        Some(())
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
            let (value, _) = items.next()?;
            let Object::Str(name) = key else {
                map.insert(DictKey(key), value);
                continue;
            };
            // `insert` for a string key, which equals only another string:
            // the cached hash and a direct comparison, in line.
            let hash = crate::fasthash::FxBuildHasher.hash_one(SharedStr::hash_cached(&name));
            match map.raw_entry_mut_v1().from_hash(hash, |key| {
                matches!(&key.0, Object::Str(other) if SharedStr::ptr_eq(other, &name) || *other == name)
            }) {
                RawEntryMut::Occupied(mut entry) => {
                    entry.insert(value);
                }
                RawEntryMut::Vacant(entry) => {
                    entry.insert_hashed_nocheck(hash, DictKey(Object::Str(name)), value);
                }
            }
        }
        Some(())
    }

    fn global(&mut self, module: &Object, name: &Object) -> Option<Object> {
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
        Some(value)
    }

    fn newobj(&mut self, stack: &mut Vec<Entry>, class: Entry, args: Entry) -> Option<()> {
        let ((Object::Type(class), CLASS), (Object::Tuple(args), _)) = (class, args) else {
            return None;
        };
        // `object.__new__(cls)` accepts no further arguments.
        if !args.is_empty() {
            return None;
        }
        let node = self.node(1, [LEAF; 2])?;
        // `object.__new__(cls)`, including its cycle-collector registration:
        // deferred, as for a plain class's ordinary construction, until the
        // instance could hold a non-atomic value (the state paths below
        // track it then).
        let instance = if class.native_kind.get() == 0
            && !class.flags.is_builtin
            && !class.instances_need_finalize()
        {
            crate::types::PyInstance::new_deferred(class)
        } else {
            let instance = Rc::new(crate::types::PyInstance::new(class));
            push_in_place!(&mut self.containers, Object::Instance(instance.clone()));
            instance
        };
        push_entry!(stack, Object::Instance(instance), node);
        Some(())
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
    fn movable(&self, count: usize, node: NodeId, holders: usize) -> Option<Option<u32>> {
        let node = self
            .nodes
            .get(usize::try_from(node.checked_sub(1)?).ok()?)?;
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
            let mut state = state.borrow_mut();
            let table: &mut DictMap = &mut state;
            // An interned key is equal to the key it replaces and has the
            // same hash, so the table stays valid.
            for (key, _) in indexmap::map::MutableKeys::iter_mut2(&mut *table) {
                let Object::Str(name) = &key.0 else {
                    return None;
                };
                key.0 = Object::Str(self.intern(name));
            }
            let mut attributes = instance.dict_cell().borrow_mut();
            let attributes: &mut DictMap = &mut attributes;
            if attributes.is_empty() {
                // The state keeps the instance's empty table and its
                // capacity, for reuse (see `spare_dicts`).
                std::mem::swap(attributes, table);
                return Some(());
            }
            attributes.try_reserve(table.len()).ok()?;
            attributes.extend(table.drain(..));
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
        let (state, state_node) = state;
        // The instance takes the state's items, so it must be free to
        // change, and the state can't reach it.
        let height = self.hold(state_node)?;
        match &state {
            Object::Dict(dict) => {
                // Held by `state` and by `containers`.
                let movable = self.movable(Rc::strong_count(dict), state_node, 2);
                self.build_from(target, height, Some(dict), None, [movable, None], None)
            }
            Object::Tuple(pair) if pair.len() == 2 => {
                let [first, second] = self
                    .nodes
                    .get(usize::try_from(state_node.checked_sub(1)?).ok()?)?
                    .pair;
                let (dict, slots) = state_dicts(&pair[0], &pair[1])?;
                // The items move only when nothing else can reach the pair.
                let count = crate::shared_value::ThinArc::strong_count(pair);
                let Some(pair_slot) = self.movable(count, state_node, 1) else {
                    return self.build_from(target, height, dict, Some(slots), [None, None], None);
                };
                // Each item is held by the pair and by `containers`.
                let moves = [
                    dict.and_then(|d| self.movable(Rc::strong_count(d), first, 2)),
                    self.movable(Rc::strong_count(slots), second, 2),
                ];
                self.build_from(target, height, dict, Some(slots), moves, pair_slot)
            }
            _ => None,
        }
    }

    /// TUPLE2, MEMOIZE, and BUILD, the way the pickler writes a state with
    /// `__slots__`, without creating the pair. Its memo slot is spent from
    /// the start, so reading it returns to the full unpickler.
    fn build_pair(&mut self, stack: &mut Vec<Entry>, marks: &[usize]) -> Option<()> {
        let second = pop(stack, marks)?;
        let first = pop(stack, marks)?;
        // The pair takes its items.
        let height = 1 + self.hold(first.1)?.max(self.hold(second.1)?);
        if height > MAX_DEPTH {
            return None;
        }
        push(&mut self.memo, (Object::None, SPENT))?;
        let (dict, slots) = state_dicts(&first.0, &second.0)?;
        // Each item is held by `first` or `second` and by `containers`.
        let moves = [
            dict.and_then(|d| self.movable(Rc::strong_count(d), first.1, 2)),
            self.movable(Rc::strong_count(slots), second.1, 2),
        ];
        self.build_from(top(stack, marks)?, height, dict, Some(slots), moves, None)
    }

    /// BUILD for the state dictionaries `dict` and `slots`, whose state is
    /// `height` high. `moves` tells, for each, whether its storage may move
    /// (and its memo slot, which is then spent, as is `pair_slot`).
    fn build_from(
        &mut self,
        target: &Entry,
        height: u32,
        dict: Option<&Rc<RefCell<DictData>>>,
        slots: Option<&Rc<RefCell<DictData>>>,
        moves: [Option<Option<u32>>; 2],
        pair_slot: Option<u32>,
    ) -> Option<()> {
        let (Object::Instance(instance), target) = target else {
            return None;
        };
        let class = instance.cls();
        let has_dict = self
            .classes
            .iter()
            .find(|plain| Rc::ptr_eq(&plain.class, &class))?
            .has_dict;
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
        self.grow(*target, height)?;

        // A state container that nothing else can reach gives its storage
        // to the instance. Its memo slot is spent, and so is the pair's:
        // reading either would see the emptied container.
        let [move_dict, move_slots] = moves.map(|movable| movable.is_some());
        let spent = if move_dict || move_slots {
            [pair_slot, moves[0].flatten(), moves[1].flatten()]
        } else {
            [None; 3]
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
            if move_slots && storage.get_index(0).is_none() {
                // Into empty storage, the distinct keys in order: the
                // result of setting each in turn, built in one step. The
                // emptied dictionary keeps its capacity for reuse.
                let mut data = slots.borrow_mut();
                let data: &mut DictMap = &mut data;
                let mut entries = Vec::new();
                entries.try_reserve_exact(data.len()).ok()?;
                entries.extend(data.drain(..));
                *storage = crate::types::SlotStorage::from_entries(entries);
            } else if move_slots {
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
        // A moved state dictionary is now empty and out of reach, so the
        // next EMPTY_DICT can reuse it. (It remains in `containers`.)
        for (moved, dict) in [(move_dict, dict), (move_slots, slots)] {
            if let (true, Some(dict)) = (moved, dict) {
                if self.spare_dicts.len() < 64 {
                    self.spare_dicts.push(dict.clone());
                }
            }
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
            // Every arm pushes its own result: a value that passes through a
            // shared temporary (or an `Option`) is copied with unaligned
            // loads that stall on the stores that just wrote it.
            match opcode {
                b'.' => {
                    return (marks.is_empty() && stack.len() == 1)
                        .then(|| stack.pop())
                        .flatten()
                        // A class is only ever the operand of NEWOBJ.
                        .filter(|(_, node)| *node != CLASS);
                }
                0x95 => reader.frame()?,
                b'(' => push(&mut *marks, stack.len())?,
                0x94 => self.memoize(top(&*stack, &*marks)?)?,
                b'h' => self.push_memo(stack, usize::from(reader.byte()?))?,
                b'j' => self.push_memo(stack, usize::try_from(reader.u32()?).ok()?)?,
                b'N' => push_entry!(stack, Object::None),
                0x88 => push_entry!(stack, Object::Bool(true)),
                0x89 => push_entry!(stack, Object::Bool(false)),
                b'K' => {
                    let value = i64::from(reader.byte()?);
                    push_entry!(stack, Object::Int(value));
                }
                b'M' => {
                    let value = u16::from_le_bytes(reader.read(2)?.try_into().ok()?);
                    push_entry!(stack, Object::Int(i64::from(value)));
                }
                b'J' => {
                    let value = i32::from_le_bytes(reader.read(4)?.try_into().ok()?);
                    push_entry!(stack, Object::Int(i64::from(value)));
                }
                0x8a | 0x8b => {
                    let size = if opcode == 0x8a {
                        usize::from(reader.byte()?)
                    } else {
                        let size = i32::from_le_bytes(reader.read(4)?.try_into().ok()?);
                        usize::try_from(size).ok()?
                    };
                    let bytes = reader.read(size)?;
                    let value = Object::int_from_bigint(BigInt::from_signed_bytes_le(bytes));
                    push_entry!(stack, value);
                }
                b'G' => {
                    let value = f64::from_be_bytes(reader.read(8)?.try_into().ok()?);
                    push_entry!(stack, Object::Float(value));
                }
                0x8c | b'X' | 0x8d => {
                    let size = match opcode {
                        0x8c => usize::from(reader.byte()?),
                        b'X' => usize::try_from(reader.u32()?).ok()?,
                        _ => reader.size64()?,
                    };
                    let bytes = reader.read(size)?;
                    // Surrogate-pass strings stay on the existing WStr path.
                    let text = if bytes.is_ascii() {
                        // SAFETY: ASCII text is valid UTF-8.
                        unsafe { std::str::from_utf8_unchecked(bytes) }
                    } else {
                        std::str::from_utf8(bytes).ok()?
                    };
                    let text = SharedStr::from(text);
                    push_entry!(stack, Object::Str(text));
                    self.memoize_next(&mut reader, stack)?;
                }
                b'C' | b'B' | 0x8e => {
                    let size = match opcode {
                        b'C' => usize::from(reader.byte()?),
                        b'B' => usize::try_from(reader.u32()?).ok()?,
                        _ => reader.size64()?,
                    };
                    let value = Object::new_bytes(reader.read(size)?);
                    push_entry!(stack, value);
                    self.memoize_next(&mut reader, stack)?;
                }
                b']' => {
                    self.empty_list(stack)?;
                    self.memoize_next(&mut reader, stack)?;
                }
                b'}' => {
                    self.empty_dict(stack)?;
                    self.memoize_next(&mut reader, stack)?;
                }
                b')' => {
                    let node = self.node(1, [LEAF; 2])?;
                    push(stack, (Object::new_tuple(Vec::new()), node))?;
                }
                // The pickler writes a state with `__slots__` this way.
                0x86 if reader.next_is([0x94, BUILD]) => {
                    reader.pos += 2;
                    self.opcode(0x94);
                    self.opcode(BUILD);
                    self.build_pair(stack, &*marks)?;
                }
                0x85..=0x87 => {
                    let count = usize::from(opcode - 0x84);
                    let begin = stack.len().checked_sub(count)?;
                    if marks.last().is_some_and(|mark| begin < *mark) {
                        return None;
                    }
                    self.tuple(stack, begin)?;
                    self.memoize_next(&mut reader, stack)?;
                }
                b't' => {
                    let mark = marks.pop()?;
                    self.tuple(stack, mark)?;
                    self.memoize_next(&mut reader, stack)?;
                }
                b'a' => {
                    let begin = stack.len().checked_sub(1)?;
                    self.extend(stack, begin, &*marks)?;
                }
                b'e' => {
                    let mark = marks.pop()?;
                    self.extend(stack, mark, &*marks)?;
                }
                b's' => {
                    let begin = stack.len().checked_sub(2)?;
                    self.setitems(stack, begin, &*marks)?;
                }
                b'u' => {
                    let mark = marks.pop()?;
                    self.setitems(stack, mark, &*marks)?;
                }
                STACK_GLOBAL => {
                    let (name, _) = pop(&mut *stack, &*marks)?;
                    let (module, _) = pop(&mut *stack, &*marks)?;
                    let class = self.global(&module, &name)?;
                    push(stack, (class, CLASS))?;
                    self.memoize_next(&mut reader, stack)?;
                }
                NEWOBJ => {
                    let args = pop(&mut *stack, &*marks)?;
                    let class = pop(&mut *stack, &*marks)?;
                    self.newobj(stack, class, args)?;
                    self.memoize_next(&mut reader, stack)?;
                }
                BUILD => {
                    let state = pop(&mut *stack, &*marks)?;
                    self.build(top(&*stack, &*marks)?, state)?;
                }
                // GLOBAL, REDUCE, NEWOBJ_EX, persistent IDs, external buffers,
                // legacy protocols, and all other operations retain the full
                // unpickler.
                _ => return None,
            }
        }
    }

    /// Publish the accepted result. Every container that the result can
    /// reach is registered with the cycle collector: even an acyclic decoded
    /// container can acquire a cycle through later mutation. Releasing the
    /// memo and the spare dictionaries first leaves the result's references
    /// as the only ones beyond `containers`' own.
    fn finish(&mut self, root: Object) -> Object {
        self.memo.clear();
        self.spare_dicts.clear();
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
    let mut scratch = Scratch::take();
    let mut decoder = Decoder::new(context, &mut scratch);
    let result = decoder
        .parse(data, &mut scratch.stack, &mut scratch.marks)
        // The code of the Python functions that the stream's instructions
        // would run is checked once the stream proves supported. Rejecting
        // it then discards what was built.
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

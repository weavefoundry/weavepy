//! The `_abc` accelerator module, a port of CPython's `Modules/_abc.c`.
//!
//! `abc.ABCMeta` delegates registration and the instance and subclass
//! checks here. Each ABC keeps its virtual-subclass registry, positive
//! cache, and negative cache on its [`TypeObject`]. Classes are held
//! weakly, so registering or checking a class never keeps it alive. A
//! foreign extension type has no weak form and is held strongly; such types
//! live for the whole process in practice.

use crate::sync::{Cell, Rc, RefCell, Weak};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::error::{assertion_error, runtime_error, type_error, RuntimeError};
use crate::import::ModuleCache;
use crate::object::{BuiltinFn, DictData, DictKey, Object, PyModule};
use crate::types::TypeObject;
use crate::weakref_registry::{id_of, ObjectId};
use crate::Interpreter;

/// CPython's `abc_invalidation_counter`: bumped by every registration, so
/// each ABC's negative cache knows when it may be stale.
static INVALIDATION_COUNTER: AtomicU64 = AtomicU64::new(0);

/// A class held by an ABC cache or registry.
enum Held {
    Type(Weak<TypeObject>),
    Other(Object),
}

impl Held {
    fn new(class: &Object) -> Self {
        match class {
            Object::Type(t) => Self::Type(Rc::downgrade(t)),
            other => Self::Other(other.clone()),
        }
    }

    fn get(&self) -> Option<Object> {
        match self {
            Self::Type(t) => t.upgrade().map(Object::Type),
            Self::Other(o) => Some(o.clone()),
        }
    }
}

/// A set of classes keyed by identity. A weak entry keeps its allocation,
/// so a dead class's identity is never reused while its entry remains.
#[derive(Default)]
struct ClassSet {
    entries: indexmap::IndexMap<ObjectId, Held, crate::fasthash::FxBuildHasher>,
    /// Length at which dead entries are next pruned.
    prune_at: usize,
}

impl ClassSet {
    fn contains(&self, class: &Object) -> bool {
        self.entries.contains_key(&id_of(class))
    }

    fn insert(&mut self, class: &Object) {
        self.entries
            .entry(id_of(class))
            .or_insert_with(|| Held::new(class));
        if self.entries.len() >= self.prune_at.max(64) {
            self.entries.retain(|_, held| held.get().is_some());
            self.prune_at = self.entries.len() * 2;
        }
    }

    fn clear(&mut self) {
        self.entries.clear();
    }

    fn live(&self) -> Vec<Object> {
        self.entries.values().filter_map(Held::get).collect()
    }
}

/// CPython's `_abc_data`: one ABC's registry and caches.
pub struct AbcState {
    registry: RefCell<ClassSet>,
    cache: RefCell<ClassSet>,
    negative_cache: RefCell<ClassSet>,
    negative_cache_version: Cell<u64>,
}

impl std::fmt::Debug for AbcState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AbcState")
            .field("registered", &self.registry.borrow().entries.len())
            .finish_non_exhaustive()
    }
}

impl AbcState {
    fn new() -> Self {
        Self {
            registry: RefCell::default(),
            cache: RefCell::default(),
            negative_cache: RefCell::default(),
            negative_cache_version: Cell::new(INVALIDATION_COUNTER.load(Ordering::Relaxed)),
        }
    }
}

pub fn build(_cache: &ModuleCache) -> Rc<PyModule> {
    let dict = Rc::new(RefCell::new(DictData::default()));
    {
        let mut d = dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("__name__")),
            Object::from_static("_abc"),
        );
        for (name, fn_) in [
            (
                "get_cache_token",
                abc_get_cache_token as fn(&[Object]) -> Result<Object, RuntimeError>,
            ),
            ("_abc_init", abc_init),
            ("_abc_register", abc_register),
            ("_abc_instancecheck", abc_instancecheck),
            ("_abc_subclasscheck", abc_subclasscheck),
            ("_get_dump", abc_get_dump),
            ("_reset_registry", abc_reset_registry),
            ("_reset_caches", abc_reset_caches),
        ] {
            d.insert(
                DictKey(Object::from_static(name)),
                Object::Builtin(Rc::new(BuiltinFn {
                    name,
                    binds_instance: false,
                    call: Box::new(fn_),
                    call_kw: None,
                })),
            );
        }
    }
    Rc::new(PyModule {
        name: "_abc".to_owned(),
        filename: None,
        dict,
    })
}

fn interpreter() -> Result<&'static mut Interpreter, RuntimeError> {
    let ptr = crate::vm_singletons::current_interpreter_ptr()
        .ok_or_else(|| runtime_error("_abc requires a running interpreter"))?;
    // SAFETY: published by the enclosing VM frame on this thread, which
    // outlives this builtin call.
    Ok(unsafe { &mut *ptr })
}

fn args_exact<const N: usize>(args: &[Object], name: &str) -> Result<[Object; N], RuntimeError> {
    <&[Object; N]>::try_from(args).cloned().map_err(|_| {
        type_error(format!(
            "{name} expected {N} argument{}, got {}",
            if N == 1 { "" } else { "s" },
            args.len()
        ))
    })
}

/// The ABC state of `cls`, inherited through the MRO the way CPython's
/// `_abc_impl` attribute is.
fn state_of(cls: &Object) -> Result<Rc<TypeObject>, RuntimeError> {
    if let Object::Type(t) = cls {
        if t.abc_state.get().is_some() {
            return Ok(t.clone());
        }
        if let Some(owner) = t
            .mro
            .borrow()
            .iter()
            .find(|base| base.abc_state.get().is_some())
        {
            return Ok(owner.clone());
        }
    }
    Err(type_error("_abc_impl is set to a wrong type"))
}

fn state(owner: &TypeObject) -> &AbcState {
    owner.abc_state.get().expect("checked by state_of")
}

fn is_class(interp: &mut Interpreter, obj: &Object) -> Result<bool, RuntimeError> {
    match obj {
        Object::Type(_) => Ok(true),
        Object::Foreign(_) => {
            let type_type = Object::Type(crate::builtin_types::builtin_types().type_.clone());
            interp.isinstance_public(obj, &type_type)
        }
        _ => Ok(false),
    }
}

fn call_method(
    interp: &mut Interpreter,
    receiver: &Object,
    name: &str,
    args: &[Object],
) -> Result<Object, RuntimeError> {
    let method = interp.load_attr_public(receiver, name)?;
    let globals = interp.builtins_dict();
    interp.call(&method, args, &[], &globals)
}

/// `getattr(obj, name, None)`, propagating everything but `AttributeError`.
fn attr_or_none(
    interp: &mut Interpreter,
    obj: &Object,
    name: &str,
) -> Result<Option<Object>, RuntimeError> {
    match interp.load_attr_public(obj, name) {
        Ok(v) => Ok(Some(v)),
        Err(e) if interp.is_attribute_error(&e) => Ok(None),
        Err(e) => Err(e),
    }
}

/// CPython `_PyObject_IsAbstract`.
fn is_abstract(interp: &mut Interpreter, obj: &Object) -> Result<bool, RuntimeError> {
    let flag = match obj {
        // A plain function carries the flag only in its `__dict__`, and
        // built-in data values never do: answer both without an
        // attribute lookup that would build an `AttributeError`.
        Object::Function(f) => f.attr_get("__isabstractmethod__"),
        Object::None
        | Object::Bool(_)
        | Object::Int(_)
        | Object::Long(_)
        | Object::Float(_)
        | Object::Str(_)
        | Object::Bytes(_)
        | Object::Tuple(_) => None,
        _ => attr_or_none(interp, obj, "__isabstractmethod__")?,
    };
    match flag {
        Some(flag) => interp.op_truth(&flag),
        None => Ok(false),
    }
}

fn abc_get_cache_token(args: &[Object]) -> Result<Object, RuntimeError> {
    args_exact::<0>(args, "get_cache_token")?;
    Ok(Object::Int(
        INVALIDATION_COUNTER.load(Ordering::Relaxed) as i64
    ))
}

/// `_abc_init(cls)`: compute `__abstractmethods__`, fold
/// `__abc_tpflags__`, and give the class fresh caches.
fn abc_init(args: &[Object]) -> Result<Object, RuntimeError> {
    let [cls] = args_exact::<1>(args, "_abc_init")?;
    let interp = interpreter()?;
    compute_abstract_methods(interp, &cls)?;
    let Object::Type(t) = &cls else {
        return Ok(Object::None);
    };
    // CPython's `_abc_init` consumes `__abc_tpflags__`: a class may not
    // claim both Py_TPFLAGS_SEQUENCE and Py_TPFLAGS_MAPPING. The collection
    // bits are kept under a private key that `flags_bits` and pattern
    // matching read.
    const COLLECTION_FLAGS: i64 = (1 << 5) | (1 << 6);
    let key = DictKey(Object::from_static("__abc_tpflags__"));
    let tpflags = t.dict.borrow().get(&key).cloned();
    if let Some(flags) = tpflags {
        if let Some(val) = flags.as_i64() {
            if (val & COLLECTION_FLAGS) == COLLECTION_FLAGS {
                return Err(type_error(
                    "__abc_tpflags__ cannot be both Py_TPFLAGS_SEQUENCE and Py_TPFLAGS_MAPPING",
                ));
            }
            t.dict.borrow_mut().insert(
                DictKey(Object::from_static("_abc_collection_flags")),
                Object::Int(val & COLLECTION_FLAGS),
            );
        }
        t.dict.borrow_mut().shift_remove(&key);
    }
    match t.abc_state.get() {
        Some(existing) => {
            existing.registry.borrow_mut().clear();
            existing.cache.borrow_mut().clear();
            existing.negative_cache.borrow_mut().clear();
            existing
                .negative_cache_version
                .set(INVALIDATION_COUNTER.load(Ordering::Relaxed));
        }
        None => {
            let _ = t.abc_state.set(Box::new(AbcState::new()));
        }
    }
    Ok(Object::None)
}

/// CPython `compute_abstract_methods`.
fn compute_abstract_methods(interp: &mut Interpreter, cls: &Object) -> Result<(), RuntimeError> {
    let mut abstracts: Vec<Object> = Vec::new();
    // Stage 1: the class's own abstract methods.
    let own: Vec<(Object, Object)> = match cls {
        Object::Type(t) => t
            .dict
            .borrow()
            .iter()
            .map(|(k, v)| (k.0.clone(), v.clone()))
            .collect(),
        _ => Vec::new(),
    };
    for (name, value) in own {
        if is_abstract(interp, &value)? {
            abstracts.push(name);
        }
    }
    // Stage 2: inherited abstract methods the class didn't override.
    let bases = interp.load_attr_public(cls, "__bases__")?;
    let Object::Tuple(bases) = bases else {
        return Err(type_error("__bases__ is not tuple"));
    };
    let globals = interp.builtins_dict();
    for base in bases.iter() {
        let Some(inherited) = attr_or_none(interp, base, "__abstractmethods__")? else {
            continue;
        };
        for name in interp.collect_iterable(&inherited, &globals)? {
            let Object::Str(s) = &name else {
                continue;
            };
            let Some(value) = attr_or_none(interp, cls, s)? else {
                continue;
            };
            if is_abstract(interp, &value)? && !abstracts.iter().any(|a| a.is_same(&name)) {
                abstracts.push(name);
            }
        }
    }
    interp.store_attr_public(
        cls,
        "__abstractmethods__",
        Object::new_frozenset_from(abstracts),
    )
}

/// `_abc_register(cls, subclass)`.
fn abc_register(args: &[Object]) -> Result<Object, RuntimeError> {
    let [cls, subclass] = args_exact::<2>(args, "_abc_register")?;
    let interp = interpreter()?;
    if !is_class(interp, &subclass)? {
        return Err(type_error("Can only register classes"));
    }
    if interp.issubclass_public(&subclass, &cls)? {
        return Ok(subclass); // Already a subclass.
    }
    // Test for cycles *after* testing for "already a subclass", so that
    // `X.register(X)` is a no-op.
    if interp.issubclass_public(&cls, &subclass)? {
        return Err(runtime_error("Refusing to create an inheritance cycle"));
    }
    let owner = state_of(&cls)?;
    state(&owner).registry.borrow_mut().insert(&subclass);
    INVALIDATION_COUNTER.fetch_add(1, Ordering::Relaxed);
    // Late registration on a Sequence or Mapping ABC must make pattern
    // matching treat the class accordingly (CPython sets the type flag
    // recursively; the VM reads this marker through the MRO).
    if let (Object::Type(abc), Object::Type(sub)) = (&cls, &subclass) {
        let flag = abc.collection_flags();
        if flag != 0 && !sub.flags.is_builtin {
            sub.dict.borrow_mut().insert(
                DictKey(Object::from_static("_abc_collection_flags")),
                Object::Int(flag),
            );
        }
    }
    Ok(subclass)
}

/// `_abc_instancecheck(cls, instance)`.
fn abc_instancecheck(args: &[Object]) -> Result<Object, RuntimeError> {
    let [cls, instance] = args_exact::<2>(args, "_abc_instancecheck")?;
    let interp = interpreter()?;
    let owner = state_of(&cls)?;
    let subclass = interp.load_attr_public(&instance, "__class__")?;
    if state(&owner).cache.borrow().contains(&subclass) {
        return Ok(Object::Bool(true));
    }
    let subtype = Object::Type(crate::builtins::class_of(&instance));
    if subtype.is_same(&subclass) {
        let data = state(&owner);
        if data.negative_cache_version.get() == INVALIDATION_COUNTER.load(Ordering::Relaxed)
            && data.negative_cache.borrow().contains(&subclass)
        {
            return Ok(Object::Bool(false));
        }
        return call_method(interp, &cls, "__subclasscheck__", &[subclass]);
    }
    let result = call_method(interp, &cls, "__subclasscheck__", &[subclass])?;
    if interp.op_truth(&result)? {
        return Ok(result);
    }
    call_method(interp, &cls, "__subclasscheck__", &[subtype])
}

/// `_abc_subclasscheck(cls, subclass)`.
fn abc_subclasscheck(args: &[Object]) -> Result<Object, RuntimeError> {
    let [cls, subclass] = args_exact::<2>(args, "_abc_subclasscheck")?;
    let interp = interpreter()?;
    if !is_class(interp, &subclass)? {
        return Err(type_error("issubclass() arg 1 must be a class"));
    }
    let owner = state_of(&cls)?;
    let data = state(&owner);
    // 1. The positive cache.
    if data.cache.borrow().contains(&subclass) {
        return Ok(Object::Bool(true));
    }
    // 2. The negative cache, invalidated by any registration since.
    let counter = INVALIDATION_COUNTER.load(Ordering::Relaxed);
    if data.negative_cache_version.get() < counter {
        data.negative_cache.borrow_mut().clear();
        data.negative_cache_version.set(counter);
    } else if data.negative_cache.borrow().contains(&subclass) {
        return Ok(Object::Bool(false));
    }
    let found = |data: &AbcState| {
        data.cache.borrow_mut().insert(&subclass);
        Ok(Object::Bool(true))
    };
    // 3. The subclass hook.
    let ok = call_method(
        interp,
        &cls,
        "__subclasshook__",
        std::slice::from_ref(&subclass),
    )?;
    match ok {
        Object::Bool(true) => return found(data),
        Object::Bool(false) => {
            data.negative_cache.borrow_mut().insert(&subclass);
            return Ok(Object::Bool(false));
        }
        ref other if crate::vm_singletons::is_not_implemented(other) => {}
        _ => {
            return Err(assertion_error(
                "__subclasshook__ must return either False, True, or NotImplemented",
            ))
        }
    }
    // 4. A direct subclass.
    let direct = match (&subclass, &cls) {
        (Object::Type(sub), Object::Type(abc)) => sub.is_subclass_of(abc),
        _ => match attr_or_none(interp, &subclass, "__mro__")? {
            Some(Object::Tuple(mro)) => mro.iter().any(|c| c.is_same(&cls)),
            _ => false,
        },
    };
    if direct {
        return found(data);
    }
    // 5. A subclass of a registered class (recursive).
    let registered = data.registry.borrow().live();
    for rcls in registered {
        if interp.issubclass_public(&subclass, &rcls)? {
            return found(data);
        }
    }
    // 6. A subclass of a subclass (recursive).
    let subclasses = call_method(interp, &cls, "__subclasses__", &[])?;
    let globals = interp.builtins_dict();
    for scls in interp.collect_iterable(&subclasses, &globals)? {
        if interp.issubclass_public(&subclass, &scls)? {
            return found(data);
        }
    }
    data.negative_cache.borrow_mut().insert(&subclass);
    Ok(Object::Bool(false))
}

/// `_get_dump(cls)`: weak references to the registry and caches, plus the
/// negative-cache version (used by the refleak hunter and `_dump_registry`).
fn abc_get_dump(args: &[Object]) -> Result<Object, RuntimeError> {
    let [cls] = args_exact::<1>(args, "_get_dump")?;
    let interp = interpreter()?;
    let owner = state_of(&cls)?;
    let data = state(&owner);
    let globals = interp.builtins_dict();
    let weakref = interp.do_import("_weakref", &Object::None, 0, &globals)?;
    let make_ref = interp.load_attr_public(&weakref, "ref")?;
    let mut weak_set = |classes: Vec<Object>| -> Result<Object, RuntimeError> {
        let mut refs = Vec::with_capacity(classes.len());
        for class in classes {
            refs.push(interp.call(&make_ref, &[class], &[], &globals)?);
        }
        Ok(Object::new_set_from(refs))
    };
    let registry = weak_set(data.registry.borrow().live())?;
    let cache = weak_set(data.cache.borrow().live())?;
    let negative = weak_set(data.negative_cache.borrow().live())?;
    Ok(Object::new_tuple_array([
        registry,
        cache,
        negative,
        Object::Int(data.negative_cache_version.get() as i64),
    ]))
}

fn abc_reset_registry(args: &[Object]) -> Result<Object, RuntimeError> {
    let [cls] = args_exact::<1>(args, "_reset_registry")?;
    let owner = state_of(&cls)?;
    state(&owner).registry.borrow_mut().clear();
    Ok(Object::None)
}

fn abc_reset_caches(args: &[Object]) -> Result<Object, RuntimeError> {
    let [cls] = args_exact::<1>(args, "_reset_caches")?;
    let owner = state_of(&cls)?;
    let data = state(&owner);
    data.cache.borrow_mut().clear();
    data.negative_cache.borrow_mut().clear();
    Ok(Object::None)
}

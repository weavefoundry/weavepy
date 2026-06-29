//! Descriptor-kind side table for built-in type-dict entries.
//!
//! CPython exposes four distinct descriptor types for the entries it
//! stores in a built-in type's `tp_dict`:
//!
//! - `method_descriptor`  — `tp_methods` entries (`str.lower`),
//! - `wrapper_descriptor` — slot wrappers (`int.__add__`, `object.__repr__`),
//! - `getset_descriptor`  — `tp_getset` computed attributes (`float.real`),
//! - `member_descriptor`  — `tp_members` struct members (`complex.real`).
//!
//! `type(str.lower).__name__ == 'method_descriptor'` and friends
//! (test_descr `test_qualname`/`test_descrdoc`) depend on the distinction,
//! as does `str.lower.__qualname__ == 'str.lower'`.
//!
//! WeavePy keeps representing these as `Object::Builtin` / `Object::Property`
//! (so the call / binding / identity machinery is unchanged) and records the
//! *kind* and metadata in a pointer-keyed side table populated once at
//! interpreter start. The descriptors live for the process lifetime (they sit
//! in the built-in type dicts / the slot-wrapper cache), so their `Rc`
//! addresses are stable keys.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::LazyLock;

use crate::object::Object;
use crate::sync::Rc;
use crate::types::TypeObject;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DescrKind {
    Method,
    Wrapper,
    GetSet,
    Member,
}

#[derive(Clone, Debug)]
pub struct DescrMeta {
    pub kind: DescrKind,
    pub objclass: Rc<TypeObject>,
    /// `objclass.__qualname__ + '.' + name`, e.g. `"str.lower"`.
    pub qualname: String,
    pub name: String,
    pub doc: Option<&'static str>,
}

thread_local! {
    static DESCR_META: RefCell<HashMap<usize, DescrMeta>> = RefCell::new(HashMap::new());
}

/// `__module__` attribution for native builtin functions that do *not*
/// live in `builtins` (e.g. the `_operator` accelerator, every `os.*` /
/// `math.*` module function). Keyed by the same pointer identity as
/// [`DESCR_META`]. A builtin absent from this table reports
/// `__module__ == "builtins"` (CPython's default for an un-attributed
/// `builtin_function_or_method`). `pickle` relies on the right answer:
/// `operator.pow.__module__ == "_operator"` so `getattr(_operator, "pow")
/// is operator.pow`, and `os.getpid.__module__ == "os"` so a bare `os.*`
/// submitted to a `spawn`/`forkserver` `ProcessPoolExecutor` worker is
/// picklable by reference.
///
/// PROCESS-GLOBAL (not thread-local): native module objects — and thus the
/// `Rc<BuiltinFn>` they hold — are *shared* across every OS thread through
/// the shared [`crate::import::ModuleCache`]. A module built on the main
/// thread must still report the right `__module__` when pickled on a
/// `multiprocessing.Queue` feeder thread. The `Rc` pointer key is stable for
/// the process lifetime and the value is `&'static str`, so sharing is sound.
static BUILTIN_MODULE: LazyLock<parking_lot::RwLock<HashMap<usize, &'static str>>> =
    LazyLock::new(|| parking_lot::RwLock::new(HashMap::new()));

/// Attribute `obj` (a native builtin function) to module `module`, so its
/// `__module__` reports that instead of the default `"builtins"`.
pub fn register_module(obj: &Object, module: &'static str) {
    let Some(k) = key(obj) else { return };
    BUILTIN_MODULE.write().insert(k, module);
}

/// The module a builtin function was attributed to via [`register_module`],
/// or `None` (→ caller uses `"builtins"`).
pub fn module_of(obj: &Object) -> Option<&'static str> {
    let k = key(obj)?;
    BUILTIN_MODULE.read().get(&k).copied()
}

/// As [`module_of`] but keyed directly off a `BuiltinFn` handle — used by the
/// dispatch loop's by-name builtin fast-paths to tell a real `builtins`
/// function apart from a same-named accelerator (e.g. `_operator.pow` must
/// not hit the 3-arg modular `pow` fast-path).
pub fn module_of_builtin(b: &Rc<crate::object::BuiltinFn>) -> Option<&'static str> {
    let k = Rc::as_ptr(b).cast::<()>() as usize;
    BUILTIN_MODULE.read().get(&k).copied()
}

thread_local! {
    /// Writable `__module__` for a `builtin_function_or_method` (RFC 0046,
    /// wave 4). CPython's `PyCFunctionObject` exposes `m_module` as a
    /// writable member, and extensions assign it at import time — numpy's
    /// `multiarray.py` does `_reconstruct.__module__ = 'numpy._core.multiarray'`
    /// so the reconstructor pickles by reference. We store the assigned
    /// object keyed by the builtin's `Rc` identity (stable for the process
    /// lifetime) and let [`module_of`]'s static attribution remain the
    /// fallback. Thread-local: extension import runs on one interpreter
    /// thread, matching [`DESCR_META`].
    static BUILTIN_WRITABLE_MODULE: RefCell<HashMap<usize, Object>> =
        RefCell::new(HashMap::new());
}

/// Record a runtime `__module__` assignment on a builtin function.
/// Returns `false` if `obj` is not a taggable representation.
pub fn set_builtin_module(obj: &Object, value: Object) -> bool {
    let Some(k) = key(obj) else { return false };
    BUILTIN_WRITABLE_MODULE.with(|m| m.borrow_mut().insert(k, value));
    true
}

/// A runtime `__module__` assigned via [`set_builtin_module`], if any.
pub fn builtin_module_value(obj: &Object) -> Option<Object> {
    let k = key(obj)?;
    BUILTIN_WRITABLE_MODULE.with(|m| m.borrow().get(&k).cloned())
}

/// The pointer key for a descriptor object, or `None` if `obj` is not a
/// representation we ever tag.
fn key(obj: &Object) -> Option<usize> {
    match obj {
        Object::Builtin(b) => Some(Rc::as_ptr(b).cast::<()>() as usize),
        Object::Property(p) => Some(Rc::as_ptr(p).cast::<()>() as usize),
        _ => None,
    }
}

/// Tag `obj` as a built-in descriptor of `kind` owned by `objclass`.
pub fn register(
    obj: &Object,
    kind: DescrKind,
    objclass: Rc<TypeObject>,
    name: &str,
    doc: Option<&'static str>,
) {
    let Some(k) = key(obj) else { return };
    // `__qualname__` excludes the module prefix (CPython:
    // `descr.__qualname__ == objclass.__qualname__ + '.' + descr.__name__`).
    // Use the bare type name (a field read) — `qualified_display_name()`
    // would re-borrow `objclass.dict`, which a caller may hold open.
    let qualname = format!("{}.{}", objclass.name, name);
    DESCR_META.with(|m| {
        m.borrow_mut().insert(
            k,
            DescrMeta {
                kind,
                objclass,
                qualname,
                name: name.to_owned(),
                doc,
            },
        );
    });
}

/// The recorded metadata for `obj`, if it was tagged.
pub fn lookup(obj: &Object) -> Option<DescrMeta> {
    let k = key(obj)?;
    DESCR_META.with(|m| m.borrow().get(&k).cloned())
}

/// The CPython descriptor *type* for `obj`, if tagged — used by `class_of`.
pub fn descr_type(obj: &Object) -> Option<Rc<TypeObject>> {
    let meta = lookup(obj)?;
    let bt = crate::builtin_types::builtin_types();
    Some(match meta.kind {
        DescrKind::Method => bt.method_descriptor_.clone(),
        DescrKind::Wrapper => bt.wrapper_descriptor_.clone(),
        DescrKind::GetSet => bt.getset_descriptor_.clone(),
        DescrKind::Member => bt.member_descriptor_.clone(),
    })
}

/// True when `name` is a dunder backed by a C *slot* (so its type-dict
/// entry is a `wrapper_descriptor`, not a `method_descriptor`). The set
/// mirrors CPython's slotdefs — operator/protocol dunders are slots, while
/// `tp_methods` dunders (`__reduce__`, `__sizeof__`, …) are plain methods.
pub fn is_slot_wrapper_name(name: &str) -> bool {
    matches!(
        name,
        "__add__"
            | "__radd__"
            | "__sub__"
            | "__rsub__"
            | "__mul__"
            | "__rmul__"
            | "__matmul__"
            | "__rmatmul__"
            | "__truediv__"
            | "__rtruediv__"
            | "__floordiv__"
            | "__rfloordiv__"
            | "__mod__"
            | "__rmod__"
            | "__divmod__"
            | "__rdivmod__"
            | "__pow__"
            | "__rpow__"
            | "__lshift__"
            | "__rlshift__"
            | "__rshift__"
            | "__rrshift__"
            | "__and__"
            | "__rand__"
            | "__or__"
            | "__ror__"
            | "__xor__"
            | "__rxor__"
            | "__neg__"
            | "__pos__"
            | "__abs__"
            | "__invert__"
            | "__bool__"
            | "__int__"
            | "__float__"
            | "__index__"
            | "__round__"
            | "__iadd__"
            | "__isub__"
            | "__imul__"
            | "__imatmul__"
            | "__itruediv__"
            | "__ifloordiv__"
            | "__imod__"
            | "__ipow__"
            | "__ilshift__"
            | "__irshift__"
            | "__iand__"
            | "__ior__"
            | "__ixor__"
            | "__len__"
            | "__getitem__"
            | "__setitem__"
            | "__delitem__"
            | "__contains__"
            | "__iter__"
            | "__next__"
            | "__reversed__"
            | "__repr__"
            | "__str__"
            | "__hash__"
            | "__call__"
            | "__eq__"
            | "__ne__"
            | "__lt__"
            | "__le__"
            | "__gt__"
            | "__ge__"
            | "__getattribute__"
            | "__getattr__"
            | "__setattr__"
            | "__delattr__"
            | "__get__"
            | "__set__"
            | "__delete__"
            | "__init__"
            | "__del__"
    )
}

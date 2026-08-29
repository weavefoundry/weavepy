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
use std::collections::{HashMap, HashSet};
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
    /// A `staticmethod`-wrapped C function (`str.maketrans`,
    /// `object.__new__`): carries `__qualname__`/`__objclass__` metadata
    /// like a descriptor, but its *type* stays
    /// `builtin_function_or_method`, as in CPython.
    StaticBuiltin,
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

/// Pointers of the `BuiltinFn`s that back a harvested C descriptor's
/// getter/setter (a `tp_getset` computed attribute or a `tp_members`
/// struct field, decoded in `weavepy-capi`'s `getset` module).
///
/// These accessor closures are `Object::Builtin`s named after the C
/// attribute — and that name can collide with a real `builtins` function
/// (numpy's `dtype.str` getset getter is a `BuiltinFn { name: "str" }`).
/// The dispatch loop's by-name builtin fast-paths key purely on
/// `BuiltinFn::name`, so without this marker `dtype.str` would be hijacked
/// by the `str(obj)` fast-path — which calls the dtype's `tp_str`
/// (numpy's `_dtype.__str__`, which itself reads `dtype.str`) and spins
/// into unbounded recursion. Descriptor invocation consults this set and
/// calls the accessor's own closure directly, never the name fast-path.
///
/// PROCESS-GLOBAL for the same reason as [`BUILTIN_MODULE`]: a bridged
/// type harvested on the import thread is shared across every interpreter
/// thread through the module cache, and its descriptors may be read from
/// any of them.
static NATIVE_DESCR_ACCESSOR: LazyLock<parking_lot::RwLock<HashSet<usize>>> =
    LazyLock::new(|| parking_lot::RwLock::new(HashSet::new()));

/// Type-dict entries that exist for *introspection only* (RFC 0056 WS4):
/// CPython materializes every slot wrapper in `tp_dict` (`'__lt__' in
/// vars(dict)`, `'__init__' in vars(ValueError)`), and doctest / `help()`
/// enumerate those dicts directly. WeavePy's dispatch, however, treats
/// "name present in a type dict" as "custom override" in several places
/// (`instance_method`, `lookup_exception_init`, …). Entries in this set
/// are therefore *skipped by [`TypeObject::lookup`]'s MRO walk* — they
/// are visible through `__dict__` / `dir()` / type-level `getattr` (which
/// falls back to the same synthesized wrapper), but never change method
/// dispatch.
///
/// PROCESS-GLOBAL for the same reason as [`BUILTIN_MODULE`]: the type
/// singletons and their dict entries are shared across threads.
static SURFACE_ONLY: LazyLock<parking_lot::RwLock<HashSet<usize>>> =
    LazyLock::new(|| parking_lot::RwLock::new(HashSet::new()));

/// The default-allocator `__new__` builtins (`make_default_new` /
/// `make_owned_new`), by identity. Several *real* constructing builtins
/// are also named `__new__` (`mappingproxy`, exception groups, struct
/// sequences…), so the instantiation path cannot key on the name alone
/// now that the allocators are stored as raw builtins.
static DEFAULT_NEW: LazyLock<parking_lot::RwLock<HashSet<usize>>> =
    LazyLock::new(|| parking_lot::RwLock::new(HashSet::new()));

/// Tag `obj` as a default-allocator `__new__` (see [`is_default_new`]).
pub fn mark_default_new(obj: &Object) {
    if let Object::Builtin(b) = obj {
        let k = Rc::as_ptr(b).cast::<()>() as usize;
        DEFAULT_NEW.write().insert(k);
    }
}

/// True when `obj` is one of the default-allocator `__new__` builtins
/// tagged via [`mark_default_new`].
pub fn is_default_new(obj: &Object) -> bool {
    let Object::Builtin(b) = obj else {
        return false;
    };
    let k = Rc::as_ptr(b).cast::<()>() as usize;
    DEFAULT_NEW.read().contains(&k)
}

/// Tag `obj` (a builtin placed in a type dict) as introspection-only:
/// [`TypeObject::lookup`] will skip it during dispatch.
pub fn mark_surface_only(obj: &Object) {
    if let Some(k) = key(obj) {
        SURFACE_ONLY.write().insert(k);
    }
}

/// True when `obj` is a surface-only type-dict entry (see
/// [`mark_surface_only`]). Cheap for non-descriptor objects (no lock).
pub fn is_surface_only(obj: &Object) -> bool {
    let Some(k) = key(obj) else { return false };
    SURFACE_ONLY.read().contains(&k)
}

/// Tag `obj` as the getter/setter closure of a harvested C descriptor, so
/// [`is_native_descr_accessor`] recognizes it and the dispatch loop routes
/// the call to its own closure instead of a same-named builtin fast-path.
pub fn mark_native_descr_accessor(obj: &Object) {
    if let Object::Builtin(b) = obj {
        let k = Rc::as_ptr(b).cast::<()>() as usize;
        NATIVE_DESCR_ACCESSOR.write().insert(k);
    }
}

/// True when `b` backs a harvested C getset/member descriptor (tagged via
/// [`mark_native_descr_accessor`]). Such a builtin must be invoked through
/// its own closure, bypassing the by-name builtin fast-paths.
pub fn is_native_descr_accessor(b: &Rc<crate::object::BuiltinFn>) -> bool {
    let k = Rc::as_ptr(b).cast::<()>() as usize;
    NATIVE_DESCR_ACCESSOR.read().contains(&k)
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

thread_local! {
    /// Per-object `__text_signature__` overrides — Argument-Clinic
    /// strings attached to descriptors minted at runtime (the
    /// `_weave_descr.method_descriptor` shim helper), where the static
    /// name-keyed table in `builtin_text_signature` can't reach.
    static TEXT_SIGNATURE: RefCell<HashMap<usize, &'static str>> = RefCell::new(HashMap::new());
}

/// Attach an Argument-Clinic `__text_signature__` string to `obj`.
pub fn register_text_signature(obj: &Object, sig: &'static str) {
    if let Some(k) = key(obj) {
        TEXT_SIGNATURE.with(|m| {
            m.borrow_mut().insert(k, sig);
        });
    }
}

/// The `__text_signature__` recorded for `obj`, if any.
pub fn text_signature_of(obj: &Object) -> Option<&'static str> {
    let k = key(obj)?;
    TEXT_SIGNATURE.with(|m| m.borrow().get(&k).copied())
}

// ------------------------------------------------------------------
// Live C docstrings (RFC 0075 WS8).
//
// numpy's `add_docstring`/`add_newdoc` machinery attaches docstrings
// *after* type-ready time by writing straight into mutable C struct
// fields — `PyTypeObject.tp_doc` for classes, `PyMethodDef.ml_doc`
// for methods (numpy `compiled_base.c::arr_add_docstring`). WeavePy
// harvests docstrings once, when the extension type is bridged, so
// those post-hoc writes were invisible: `np.float64.__doc__` stayed
// empty and `inspect.signature(np.float64)` — which parses the
// `name(sig)\n--\n\n` line out of the C doc — raised ValueError for
// every scalar type and method (96 rows of numpy's own
// test_scalar_methods TestSignature). These registries let the
// bridge attach a *reader* that consults the C field on each access,
// so whatever the extension wrote after ready is served live.

/// Decodes the current docstring behind `addr`, a C struct address
/// whose meaning the installer fixed (`weavepy-capi` passes the
/// `PyMethodDef` entry address and re-reads its `ml_doc`).
pub type LiveDocReader = unsafe fn(usize) -> Option<String>;

/// Per-object doc readers. PROCESS-GLOBAL for the same reason as
/// [`BUILTIN_MODULE`]: the descriptor objects travel across threads
/// through the shared module cache. The addresses point into the
/// extension's method tables, which live for the process lifetime.
static LIVE_C_DOC: LazyLock<parking_lot::RwLock<HashMap<usize, (usize, LiveDocReader)>>> =
    LazyLock::new(|| parking_lot::RwLock::new(HashMap::new()));

/// Attach a live C-doc reader to `obj` (a bridged method descriptor).
pub fn register_live_c_doc(obj: &Object, addr: usize, read: LiveDocReader) {
    if let Some(k) = key(obj) {
        LIVE_C_DOC.write().insert(k, (addr, read));
    }
}

/// The current C-side docstring for `obj`, re-read on every call.
pub fn live_c_doc_of(obj: &Object) -> Option<String> {
    let k = key(obj)?;
    let (addr, read) = *LIVE_C_DOC.read().get(&k)?;
    unsafe { read(addr) }
}

/// Class-doc hook: given a class, returns the current `tp_doc` of the
/// backing C `PyTypeObject`, or `None` for pure-Python classes.
/// Installed once by `weavepy-capi`'s extension loader.
static EXT_TYPE_DOC_HOOK: std::sync::OnceLock<fn(&Rc<TypeObject>) -> Option<String>> =
    std::sync::OnceLock::new();

pub fn install_ext_type_doc_hook(f: fn(&Rc<TypeObject>) -> Option<String>) {
    let _ = EXT_TYPE_DOC_HOOK.set(f);
}

/// The bridged class's current C `tp_doc`, if `cls` is an extension type.
pub fn ext_type_doc(cls: &Rc<TypeObject>) -> Option<String> {
    EXT_TYPE_DOC_HOOK.get()?(cls)
}

/// Class-flags hook: the C `tp_flags` of the type backing `cls`, or
/// `None` for pure-Python classes. Lets `type.__flags__` correct the
/// heap-type bit for *static* extension types: the VM synthesizes
/// `Py_TPFLAGS_HEAPTYPE` for every non-builtin class, but numpy's
/// `_needs_add_docstring` keys on that bit to decide whether a type's
/// docstring can only be attached through `add_docstring`, and a
/// static scalar type misreported as a heap type draws the
/// "add_newdoc was used on a pure-python object" UserWarning at import
/// (RFC 0075 WS8).
static EXT_TYPE_FLAGS_HOOK: std::sync::OnceLock<fn(&Rc<TypeObject>) -> Option<u64>> =
    std::sync::OnceLock::new();

pub fn install_ext_type_flags_hook(f: fn(&Rc<TypeObject>) -> Option<u64>) {
    let _ = EXT_TYPE_FLAGS_HOOK.set(f);
}

/// The bridged class's C `tp_flags`, if `cls` is an extension type.
pub fn ext_type_c_flags(cls: &Rc<TypeObject>) -> Option<u64> {
    EXT_TYPE_FLAGS_HOOK.get()?(cls)
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
        DescrKind::StaticBuiltin => bt.builtin_function_.clone(),
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

//! The runtime object model.
//!
//! Every Python value at runtime is an [`Object`]. The enum is a
//! tagged union over the immortal singletons (None, bool, small int)
//! and `Rc<…>` handles for the rest. Cloning an [`Object`] copies
//! the small inline payload or bumps an `Rc`; it never deep-copies.
//!
//! This is a placeholder representation. Identity (`is`) is computed
//! by [`Object::is_same`] using `Rc::ptr_eq` for the heap variants
//! and value equality for the value-type variants. RFC 0002 will
//! replace this with a proper type-slot-based object model.

use crate::sync::Rc;
use crate::sync::{Cell, RefCell};
use std::cmp::Ordering;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::io::{Read, Write};

use num_bigint::BigInt;
use num_traits::{Signed, ToPrimitive, Zero};
use weavepy_compiler::CodeObject;

use crate::error::{os_error, type_error, value_error, RuntimeError};
use crate::types::{PyInstance, TypeObject};

/// RFC 0025 compile-time gate: every `Object` variant — and therefore
/// the heap closure can cross OS thread boundaries via
/// `_thread.start_new_thread`. The proof is that the Rust compiler
/// successfully derives `Object: Send + Sync` from this assertion;
/// if any future variant introduces a non-`Send` payload (a raw
/// pointer, a `Box<dyn Fn>` without the bounds, a `std::cell::Cell`,
/// etc.), this assertion fails to compile.
const _: () = {
    const fn assert_send<T: Send>() {}
    const fn assert_sync<T: Sync>() {}
    assert_send::<Object>();
    assert_sync::<Object>();
};

/// A Python value as seen by the interpreter.
#[derive(Clone)]
pub enum Object {
    None,
    Bool(bool),
    Int(i64),
    /// Arbitrary-precision integer (RFC 0019). Created on i64 overflow,
    /// large literals, or explicit `int.from_bytes(...)` /
    /// `int(...)` parsing of large strings. `Object::int_from_bigint`
    /// auto-demotes to `Int(i64)` when the value fits.
    Long(Rc<BigInt>),
    Float(f64),
    /// Complex number with rectangular components (RFC 0019).
    Complex(Rc<PyComplex>),
    Str(Rc<str>),
    Tuple(Rc<[Object]>),
    List(Rc<RefCell<Vec<Object>>>),
    Dict(Rc<RefCell<DictData>>),
    Range(Rc<Range>),
    Function(Rc<PyFunction>),
    Builtin(Rc<BuiltinFn>),
    BoundMethod(Rc<BoundMethod>),
    Code(Rc<CodeObject>),
    Cell(Rc<RefCell<Object>>),
    Iter(Rc<RefCell<PyIterator>>),
    Slice(Rc<PySlice>),
    Type(Rc<TypeObject>),
    Instance(Rc<PyInstance>),
    /// A loaded module (`Object::Module(Rc<PyModule>)`). Attribute
    /// access goes through `module.dict`. Introduced in RFC 0012.
    Module(Rc<PyModule>),
    /// A live generator object (RFC 0006). Holds a suspended frame
    /// shared via `Rc<RefCell<…>>` so it can be resumed by `next()`,
    /// `.send(v)`, `.throw(e)`, and `.close()`.
    Generator(Rc<PyGenerator>),
    /// A live coroutine object (PEP 492, RFC 0016) — the value
    /// returned from calling an `async def`. Reuses [`PyGenerator`]'s
    /// suspended-frame machinery; the variant tag distinguishes it
    /// for the awaitable protocol and `isinstance` checks.
    Coroutine(Rc<PyGenerator>),
    /// A live async-generator object (PEP 525, RFC 0016) — the value
    /// returned from calling an `async def` that contains `yield`.
    /// Consumable via `async for`.
    AsyncGenerator(Rc<PyGenerator>),
    /// Immutable byte string `b"..."`.
    Bytes(Rc<[u8]>),
    /// Mutable byte string `bytearray(...)`.
    ByteArray(Rc<RefCell<Vec<u8>>>),
    /// Mutable set `{1, 2, 3}` / `set(...)`. Backed by an
    /// `IndexSet<DictKey>` so iteration order is insertion order
    /// (CPython 3.7+ semantics).
    Set(Rc<RefCell<SetData>>),
    /// Immutable set `frozenset(...)`.
    FrozenSet(Rc<SetData>),
    /// File-like object — opened by `open()`, returned by
    /// `io.StringIO`/`io.BytesIO`, and the values behind
    /// `sys.stdin`/`stdout`/`stderr`.
    File(Rc<PyFile>),
    /// `@property` descriptor (RFC 0015). Resolves through the
    /// data-descriptor path on attribute access.
    Property(Rc<PyProperty>),
    /// `@staticmethod` descriptor (RFC 0015). Returns the wrapped
    /// callable unchanged when accessed via instance or class.
    StaticMethod(Rc<Object>),
    /// `@classmethod` descriptor (RFC 0015). Returns a method bound
    /// to the class (not the instance) on access.
    ClassMethod(Rc<Object>),
    /// Slot descriptor created by `__slots__` (RFC 0015). Stores a
    /// per-instance value under the slot's name in `__dict__`,
    /// enforcing the slot-list at the class level.
    SlotDescriptor(Rc<SlotDescriptor>),
    /// Python-visible frame object (RFC 0018). Created on demand by
    /// `sys._getframe`, the `traceback` module, and the unwind
    /// machinery. Holds the locals snapshot, globals reference, and
    /// the chain of outer frames.
    Frame(Rc<PyFrame>),
    /// Python-visible traceback object (RFC 0018). Built as
    /// exceptions propagate so user code can walk `tb_next` /
    /// `tb_frame` / `tb_lineno` like CPython.
    Traceback(Rc<PyTraceback>),
    /// Memory view over a bytes-like object (RFC 0023). Supports the
    /// buffer protocol minimum: indexing, slicing, `len`, `cast`,
    /// `tobytes`, `tolist`, `release`, `nbytes`, `format`, `itemsize`,
    /// `ndim`, `shape`, `strides`, `readonly`, `c_contiguous`,
    /// `f_contiguous`, `contiguous`.
    MemoryView(Rc<PyMemoryView>),
    /// A read-only mapping view (RFC 0023). Used by `cls.__dict__`,
    /// `module.__dict__` reads through `vars()`, and dataclass
    /// `__match_args__` exposure. Backed by a shared `DictData`
    /// reference; mutations through this view raise `TypeError`.
    MappingProxy(Rc<RefCell<DictData>>),
    /// A view over `dict.keys() / .values() / .items()` (RFC 0023).
    /// Keeps a reference to the source dict so the view stays live
    /// across mutations, matching CPython's dict view semantics.
    DictView(Rc<PyDictView>),
    /// `types.SimpleNamespace` instance (RFC 0023). Used by
    /// `sys.implementation`, `argparse.Namespace`-shaped fixtures, and
    /// the conformance harness.
    SimpleNamespace(Rc<RefCell<DictData>>),
}

impl fmt::Debug for Object {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Object::None => write!(f, "None"),
            Object::Bool(b) => write!(f, "{}", if *b { "True" } else { "False" }),
            Object::Int(i) => write!(f, "{i}"),
            Object::Long(b) => write!(f, "{b}"),
            Object::Complex(c) => write!(f, "complex({}, {})", c.real, c.imag),
            Object::Float(x) => write!(f, "{x}"),
            Object::Str(s) => write!(f, "{s:?}"),
            Object::Tuple(items) => f.debug_list().entries(items.iter()).finish(),
            Object::List(items) => f.debug_list().entries(items.borrow().iter()).finish(),
            Object::Dict(d) => {
                let d = d.borrow();
                let mut m = f.debug_map();
                for (k, v) in d.iter() {
                    m.entry(&k.0, v);
                }
                m.finish()
            }
            Object::Range(r) => write!(f, "range({}, {}, {})", r.start, r.stop, r.step),
            Object::Function(func) => write!(f, "<function {}>", func.name),
            Object::Builtin(b) => write!(f, "<built-in function {}>", b.name),
            Object::BoundMethod(bm) => write!(f, "<bound method {:?}>", bm.function),
            Object::Code(c) => write!(f, "<code object {}>", c.name),
            Object::Cell(c) => f.debug_tuple("Cell").field(&c.borrow()).finish(),
            Object::Iter(_) => write!(f, "<iterator>"),
            Object::Slice(s) => write!(f, "slice({:?}, {:?}, {:?})", s.start, s.stop, s.step),
            Object::Type(t) => write!(f, "<class '{}'>", t.name),
            Object::Instance(i) => write!(f, "<{} object>", i.class.name),
            Object::Module(m) => write!(f, "<module {:?}>", m.name),
            Object::Generator(g) => write!(f, "<generator object {}>", g.name),
            Object::Coroutine(g) => write!(f, "<coroutine object {}>", g.name),
            Object::AsyncGenerator(g) => write!(f, "<async_generator object {}>", g.name),
            Object::Bytes(b) => write!(f, "Bytes({})", b.len()),
            Object::ByteArray(b) => write!(f, "ByteArray({})", b.borrow().len()),
            Object::Set(s) => f.debug_set().entries(s.borrow().iter()).finish(),
            Object::FrozenSet(s) => f.debug_set().entries(s.iter()).finish(),
            Object::File(file) => write!(f, "<file {:?}>", file.name),
            Object::Property(_) => write!(f, "<property>"),
            Object::StaticMethod(inner) => write!(f, "<staticmethod {:?}>", inner.as_ref()),
            Object::ClassMethod(inner) => write!(f, "<classmethod {:?}>", inner.as_ref()),
            Object::SlotDescriptor(sd) => write!(f, "<slot {:?} of {:?}>", sd.name, sd.class_name),
            Object::Frame(fr) => write!(f, "<frame at 0x{:x}>", Rc::as_ptr(fr) as usize),
            Object::Traceback(tb) => write!(f, "<traceback at 0x{:x}>", Rc::as_ptr(tb) as usize),
            Object::MemoryView(mv) => write!(f, "<memory at 0x{:x}>", Rc::as_ptr(mv) as usize),
            Object::MappingProxy(d) => {
                let d = d.borrow();
                let mut m = f.debug_map();
                for (k, v) in d.iter() {
                    m.entry(&k.0, v);
                }
                m.finish()
            }
            Object::DictView(v) => write!(f, "<{} {:?}>", v.kind.type_name(), v),
            Object::SimpleNamespace(d) => {
                let d = d.borrow();
                let mut m = f.debug_struct("namespace");
                for (k, v) in d.iter() {
                    m.field(&k.0.to_str(), v);
                }
                m.finish()
            }
        }
    }
}

/// Internal payload for [`Object::Frame`]. Captures the live state
/// of a frame at the point the snapshot is materialised — the VM
/// updates [`PyFrame::lasti`] before any opcode that may inspect
/// frames so `frame.f_lineno` reads correctly.
pub struct PyFrame {
    pub code: Rc<CodeObject>,
    pub globals: Rc<RefCell<DictData>>,
    pub builtins: Rc<RefCell<DictData>>,
    /// Index of the current instruction inside [`Self::code`]. Updated
    /// per-instruction by the dispatch loop while this frame is the
    /// active one.
    pub lasti: Cell<u32>,
    /// The enclosing frame (the next-outer in the call stack), `None`
    /// for the module frame.
    pub back: RefCell<Option<Rc<PyFrame>>>,
    /// Lazy snapshot of the locals dict. Built on first access to
    /// `frame.f_locals`. Subsequent accesses see the cached dict;
    /// CPython does the same. The cached dict is *not* a live view —
    /// writes to it don't propagate back to the frame's `locals`
    /// array.
    pub locals_cache: RefCell<Option<Object>>,
    /// Provider closure that materialises the locals dict on first
    /// access. Captures the (interior-mutable) locals array at the
    /// time the snapshot is taken so the same provider can be called
    /// again after a `clear()` to refresh.
    pub locals_provider: RefCell<Option<Rc<dyn Fn() -> Object + Send + Sync>>>,
    /// Shared, mutable mirror of the running frame's `locals` array.
    /// The VM updates this between steps so `f_locals` reflects live
    /// state. `None` once the frame has returned.
    pub locals_mirror: RefCell<Option<Rc<RefCell<Vec<Object>>>>>,
    /// Per-frame trace function. Returned by `sys.settrace`'s hook
    /// (or by a previous per-frame trace), this callable receives
    /// subsequent `'line'` / `'return'` / `'exception'` events on
    /// the frame. `Object::None` disables tracing for the frame.
    pub trace: RefCell<Object>,
    /// Per-frame `f_lineno` override. CPython lets debuggers set
    /// `f_lineno` to jump to a different line; we keep storage so
    /// reads round-trip, even though writes don't actually move the
    /// program counter.
    pub override_lineno: Cell<Option<u32>>,
    /// Most recently observed source line on this frame, used by
    /// the dispatcher to know when to fire a `'line'` event. `None`
    /// means "no line event has fired on this frame yet" — the
    /// next `step` will fire one.
    pub last_line: Cell<Option<u32>>,
}

impl fmt::Debug for PyFrame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "<frame code={:?} lineno={}>",
            self.code.name,
            self.current_lineno()
        )
    }
}

impl PyFrame {
    pub fn current_lineno(&self) -> u32 {
        if let Some(v) = self.override_lineno.get() {
            return v;
        }
        let pc = self.lasti.get() as usize;
        self.code.linetable.get(pc).copied().unwrap_or(0)
    }

    /// Materialise the locals dict, caching the result. Subsequent
    /// calls return the same dict object so `id(frame.f_locals)` is
    /// stable.
    pub fn locals(&self) -> Object {
        if let Some(v) = self.locals_cache.borrow().as_ref() {
            return v.clone();
        }
        let provider = self.locals_provider.borrow().clone();
        let dict = provider
            .as_ref()
            .map_or_else(Object::new_dict, |provider| provider());
        *self.locals_cache.borrow_mut() = Some(dict.clone());
        dict
    }

    /// Force re-materialisation on the next `locals()` call. Used by
    /// the VM after the frame has executed enough to make the cached
    /// snapshot stale (function entry, generator resume).
    pub fn invalidate_locals(&self) {
        self.locals_cache.borrow_mut().take();
    }
}

/// Internal payload for [`Object::Traceback`]. Built lazily by the
/// unwind machinery and chained outward through [`Self::next`].
#[derive(Debug)]
pub struct PyTraceback {
    pub frame: Rc<PyFrame>,
    pub lineno: u32,
    pub lasti: u32,
    pub next: RefCell<Option<Rc<PyTraceback>>>,
}

/// Backing buffer for a [`PyMemoryView`].
#[derive(Debug)]
pub enum MemoryViewBuffer {
    /// Immutable bytes; the view exposes them read-only.
    Bytes(Rc<[u8]>),
    /// Mutable bytearray; the view participates in `bytearray`'s
    /// shared-state semantics so writes through the view land in
    /// the underlying buffer.
    ByteArray(Rc<RefCell<Vec<u8>>>),
}

/// `memoryview(obj)` — a thin window into another bytes-like object.
/// We only implement the byte-format minimum CPython itself ships
/// (`format='B'`, `itemsize=1`, `ndim=1`). NumPy-style N-dimensional
/// views are RFC 0023 future work; the surface is enough for
/// `pickle.PickleBuffer`, `socket.recv_into`, and the standard
/// `memoryview(b'hello').tobytes()` patterns.
#[derive(Debug)]
pub struct PyMemoryView {
    pub buffer: MemoryViewBuffer,
    /// Inclusive start offset into the buffer (in bytes).
    pub start: Cell<usize>,
    /// Number of bytes covered by the view.
    pub len: Cell<usize>,
    pub readonly: Cell<bool>,
    pub released: Cell<bool>,
    pub format: RefCell<String>,
    pub itemsize: Cell<usize>,
}

impl PyMemoryView {
    pub fn from_bytes(b: Rc<[u8]>) -> Self {
        let len = b.len();
        Self {
            buffer: MemoryViewBuffer::Bytes(b),
            start: Cell::new(0),
            len: Cell::new(len),
            readonly: Cell::new(true),
            released: Cell::new(false),
            format: RefCell::new("B".to_owned()),
            itemsize: Cell::new(1),
        }
    }

    pub fn from_bytearray(b: Rc<RefCell<Vec<u8>>>) -> Self {
        let len = b.borrow().len();
        Self {
            buffer: MemoryViewBuffer::ByteArray(b),
            start: Cell::new(0),
            len: Cell::new(len),
            readonly: Cell::new(false),
            released: Cell::new(false),
            format: RefCell::new("B".to_owned()),
            itemsize: Cell::new(1),
        }
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let start = self.start.get();
        let end = start + self.len.get();
        match &self.buffer {
            MemoryViewBuffer::Bytes(b) => b[start..end].to_vec(),
            MemoryViewBuffer::ByteArray(b) => b.borrow()[start..end].to_vec(),
        }
    }
}

/// Discriminator carried on [`Object::DictView`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DictViewKind {
    Keys,
    Values,
    Items,
}

impl DictViewKind {
    pub fn type_name(self) -> &'static str {
        match self {
            DictViewKind::Keys => "dict_keys",
            DictViewKind::Values => "dict_values",
            DictViewKind::Items => "dict_items",
        }
    }
}

/// Live view over a backing `dict`. Iteration order mirrors the
/// underlying `IndexMap`. Mutations to the dict propagate through
/// the view (CPython invariant).
#[derive(Debug)]
pub struct PyDictView {
    pub dict: Rc<RefCell<DictData>>,
    pub kind: DictViewKind,
}

/// Internal payload for [`Object::Property`].
#[derive(Debug)]
pub struct PyProperty {
    pub fget: Object,
    pub fset: Object,
    pub fdel: Object,
    pub doc: Object,
}

impl PyProperty {
    pub fn new(fget: Object, fset: Object, fdel: Object, doc: Object) -> Self {
        Self {
            fget,
            fset,
            fdel,
            doc,
        }
    }

    /// Return a clone of `self` with the given attribute replaced. Used
    /// by `property.getter`/`setter`/`deleter` (which CPython models as
    /// methods that return a *new* property carrying the patched
    /// callable plus the existing ones).
    pub fn with(&self, which: PropertyAttr, fn_: Object) -> Self {
        let mut next = Self {
            fget: self.fget.clone(),
            fset: self.fset.clone(),
            fdel: self.fdel.clone(),
            doc: self.doc.clone(),
        };
        match which {
            PropertyAttr::Get => next.fget = fn_,
            PropertyAttr::Set => next.fset = fn_,
            PropertyAttr::Del => next.fdel = fn_,
        }
        next
    }
}

#[derive(Debug, Clone, Copy)]
pub enum PropertyAttr {
    Get,
    Set,
    Del,
}

/// Internal payload for [`Object::SlotDescriptor`].
#[derive(Debug)]
pub struct SlotDescriptor {
    /// The attribute name this slot guards. Used both for storage in
    /// the instance dict and for error messages.
    pub name: String,
    /// The class that declared this slot, kept by name for nicer
    /// error messages (we do not need a hard reference back to the
    /// type for correctness).
    pub class_name: String,
}

/// Ordered set backing for [`Object::Set`] and [`Object::FrozenSet`].
pub type SetData = indexmap::IndexSet<DictKey>;

// ---------- supporting types ----------

/// Rectangular-form complex number (RFC 0019). CPython's
/// `complex` is also stored as `(double real, double imag)`; we
/// match the layout for compatibility.
#[derive(Debug, Clone, Copy)]
pub struct PyComplex {
    pub real: f64,
    pub imag: f64,
}

impl PyComplex {
    pub fn new(real: f64, imag: f64) -> Self {
        Self { real, imag }
    }

    /// Construct from a `(real, imag)` tuple. Convenience for the
    /// many call sites that build one inline.
    pub fn from_tuple(t: (f64, f64)) -> Self {
        Self {
            real: t.0,
            imag: t.1,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Range {
    pub start: i64,
    pub stop: i64,
    pub step: i64,
}

/// A loaded Python module: a name, an optional source filename, and
/// a dict that doubles as `module.__dict__` and the globals namespace
/// of code that runs *inside* the module.
///
/// Built-in modules (`sys`, `math`, …) have `filename = None`; modules
/// loaded from a `.py` file carry the path used to find them. Modules
/// are cheap-to-clone via the wrapping `Rc`, and identity (`is`)
/// reduces to pointer equality on that `Rc`.
pub struct PyModule {
    pub name: String,
    pub filename: Option<String>,
    pub dict: Rc<RefCell<DictData>>,
}

impl fmt::Debug for PyModule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "<module {:?}>", self.name)
    }
}

/// Dictionary key: a hashable [`Object`] wrapped to satisfy the
/// `Hash + Eq` requirements imposed by `HashMap` / `IndexMap`.
#[derive(Clone, Debug)]
pub struct DictKey(pub Object);

/// Reach for the running interpreter to compute a user instance's Python
/// `__hash__`. `DictKey`'s `Hash`/`Eq` impls have no interpreter handle, so
/// they borrow the thread's published interpreter pointer — the same bridge
/// `_imp`/`_thread`/the C-API iterator use. Returns `None` when no
/// interpreter is active (e.g. a dict built from pure-Rust setup), so the
/// caller falls back to the native structural behaviour.
fn current_interp_hash(obj: &Object) -> Option<i64> {
    let ptr = crate::vm_singletons::current_interpreter_ptr()?;
    // SAFETY: the pointer is published by the bytecode dispatch loop for the
    // running thread and used only to re-enter the interpreter synchronously,
    // mirroring the established reentrant-callback pattern in `_imp`/`_thread`.
    let interp = unsafe { &mut *ptr };
    interp.reentrant_py_hash(obj)
}

/// Companion to [`current_interp_hash`] for `a == b` via Python `__eq__`.
fn current_interp_eq(a: &Object, b: &Object) -> Option<bool> {
    let ptr = crate::vm_singletons::current_interpreter_ptr()?;
    // SAFETY: see `current_interp_hash`.
    let interp = unsafe { &mut *ptr };
    interp.reentrant_py_eq(a, b)
}

/// True when `obj` is a user instance whose class supplies a *callable*
/// `name` dunder (a real Python `def`, not the inherited identity default).
/// Used to gate the reentrant `__eq__` dispatch so plain instances keep the
/// native identity fast path.
fn instance_has_custom_dunder(obj: &Object, name: &str) -> bool {
    matches!(
        obj,
        Object::Instance(inst)
            if matches!(
                inst.class.lookup(name),
                Some(Object::Function(_) | Object::BoundMethod(_))
            )
    )
}

impl PartialEq for DictKey {
    fn eq(&self, other: &Self) -> bool {
        // CPython compares dict/set keys with `a is b or a == b`; the identity
        // half makes a stored `nan` findable by itself (`{nan}` contains its
        // own nan even though `nan != nan`).
        if self.0.is_same(&other.0) {
            return true;
        }
        // Native fast path also covers instance *identity* (`Rc::ptr_eq`),
        // which is the `a is b` half of CPython's dict-key comparison.
        if self.0.eq_value(&other.0) {
            return true;
        }
        // Distinct user instances with a custom `__eq__` compare through it
        // so a class defining `__eq__`/`__hash__` works as a `set`/`dict`
        // key. Plain instances (no custom `__eq__`) keep identity semantics,
        // already decided by the `eq_value` fast path above.
        if instance_has_custom_dunder(&self.0, "__eq__")
            || instance_has_custom_dunder(&other.0, "__eq__")
        {
            if let Some(eq) = current_interp_eq(&self.0, &other.0) {
                return eq;
            }
        }
        false
    }
}

impl Eq for DictKey {}

impl Hash for DictKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        // Bucket every key by its single canonical Python hash value, so any
        // two keys Python deems equal-and-hashable collide here regardless of
        // their Rust representation: equal numeric types (`1 == 1.0 == True`),
        // an `int`/`str`/… subclass and its wrapped value, and — crucially —
        // a custom `__hash__` that returns a built-in value (e.g.
        // `hash('halibut')`) and the string itself. `DictKey::eq` then decides
        // actual equality within the bucket. Identity-hashable objects
        // (functions, types, plain instances, …) fold in their allocation
        // identity; truly unhashable keys share a constant bucket and the
        // runtime raises lazily when used.
        let h = py_hash_value(&self.0).unwrap_or_else(|| identity_hash(&self.0));
        h.hash(state);
    }
}

pub type DictData = indexmap::IndexMap<DictKey, Object>;

#[derive(Clone)]
pub struct PyFunction {
    pub name: String,
    pub code: Rc<CodeObject>,
    /// Module-level globals shared with the defining module.
    pub globals: Rc<RefCell<DictData>>,
    pub defaults: Vec<Object>,
    pub kw_defaults: Vec<(String, Object)>,
    /// Closure cells matching `code.freevars` in order.
    pub closure: Vec<Object>,
    /// `__dict__` for arbitrary attribute assignment on the
    /// function — e.g. `@functools.wraps`, `@abstractmethod`'s
    /// `__isabstractmethod__`, or any decorator that stashes
    /// per-callable metadata.
    pub attrs: Rc<RefCell<DictData>>,
}

impl fmt::Debug for PyFunction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "<function {}>", self.name)
    }
}

#[derive(Clone)]
pub struct BoundMethod {
    pub receiver: Object,
    pub function: Object,
}

impl fmt::Debug for BoundMethod {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "<bound method>")
    }
}

/// A Rust function exposed to Python code.
///
/// The `Send + Sync` bound on the closure is what makes `Object` itself
/// `Send + Sync` (RFC 0025). Every stdlib factory that builds a
/// `BuiltinFn` captures only `Send + Sync` state — a property the
/// audit checks at construction time.
pub struct BuiltinFn {
    pub name: &'static str,
    pub call: Box<dyn Fn(&[Object]) -> Result<Object, RuntimeError> + Send + Sync>,
    /// Kwargs-aware entry point. When `Some`, the VM dispatch loop
    /// calls this with both positional and keyword arguments instead
    /// of falling through the legacy positional-only path. Builtins
    /// that don't accept kwargs leave this as `None`; the dispatcher
    /// then errors on any kwargs the caller passes.
    pub call_kw: Option<
        Box<dyn Fn(&[Object], &[(String, Object)]) -> Result<Object, RuntimeError> + Send + Sync>,
    >,
}

impl BuiltinFn {
    /// Build a positional-only builtin (the common case).
    pub fn new<F>(name: &'static str, body: F) -> Self
    where
        F: Fn(&[Object]) -> Result<Object, RuntimeError> + Send + Sync + 'static,
    {
        Self {
            name,
            call: Box::new(body),
            call_kw: None,
        }
    }

    /// Build a kwargs-aware builtin. The positional-only `call` field
    /// stays wired (using a body that ignores kwargs) so dispatchers
    /// that don't bother to check `call_kw` still work.
    pub fn with_kwargs<F>(name: &'static str, body: F) -> Self
    where
        F: Fn(&[Object], &[(String, Object)]) -> Result<Object, RuntimeError>
            + Send
            + Sync
            + Clone
            + 'static,
    {
        let body_pos = body.clone();
        Self {
            name,
            call: Box::new(move |args| body_pos(args, &[])),
            call_kw: Some(Box::new(move |args, kwargs| body(args, kwargs))),
        }
    }
}

impl fmt::Debug for BuiltinFn {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "<built-in function {}>", self.name)
    }
}

#[derive(Debug, Clone)]
pub struct PySlice {
    pub start: Object,
    pub stop: Object,
    pub step: Object,
}

/// A live Python generator (RFC 0006). Each generator wraps a frame
/// that the interpreter resumes from the last `YIELD_VALUE`. The frame
/// itself is opaque to outside code — it's owned by the VM module via
/// `state` and only legal to inspect via interpreter methods.
pub struct PyGenerator {
    pub name: String,
    pub state: RefCell<GeneratorState>,
}

impl PyGenerator {
    pub fn new(name: impl Into<String>, frame: Box<dyn std::any::Any + Send + Sync>) -> Self {
        Self {
            name: name.into(),
            state: RefCell::new(GeneratorState::Created(frame)),
        }
    }

    pub fn is_finished(&self) -> bool {
        matches!(&*self.state.borrow(), GeneratorState::Finished)
    }
}

impl fmt::Debug for PyGenerator {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "<generator {}>", self.name)
    }
}

/// Flavour of a `PyGenerator`. Stored alongside the suspended frame
/// so the same suspension machinery serves all three async-shaped
/// objects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoroutineKind {
    Generator,
    Coroutine,
    AsyncGenerator,
}

/// State machine for an active or exhausted generator. The frame is
/// stored as `Box<dyn Any>` because `PyGenerator` lives in the
/// `object` module but `Frame` lives in `vm::lib`.
pub enum GeneratorState {
    /// Created but not yet started — body hasn't executed past the
    /// initial `RETURN_GENERATOR`.
    Created(Box<dyn std::any::Any + Send + Sync>),
    /// Paused at a `YIELD_VALUE`.
    Suspended(Box<dyn std::any::Any + Send + Sync>),
    /// Body returned (cleanly or via exception). Subsequent
    /// `next`/`send` raise `StopIteration`.
    Finished,
    /// Currently executing — re-entry would be illegal.
    Running,
}

impl fmt::Debug for GeneratorState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Created(_) => write!(f, "Created"),
            Self::Suspended(_) => write!(f, "Suspended"),
            Self::Finished => write!(f, "Finished"),
            Self::Running => write!(f, "Running"),
        }
    }
}

/// File-like object exposed to Python. Wraps a [`FileBackend`] so
/// the same wrapper can talk to a real file, an in-memory buffer
/// (`io.StringIO`/`io.BytesIO`), or the interpreter's stdout/stderr
/// sinks.
pub struct PyFile {
    pub name: String,
    pub mode: String,
    pub binary: bool,
    pub backend: RefCell<FileBackend>,
    pub closed: RefCell<bool>,
}

impl PyFile {
    pub fn new(name: impl Into<String>, mode: impl Into<String>, backend: FileBackend) -> Self {
        let mode_s = mode.into();
        let binary = mode_s.contains('b');
        Self {
            name: name.into(),
            mode: mode_s,
            binary,
            backend: RefCell::new(backend),
            closed: RefCell::new(false),
        }
    }

    pub fn is_closed(&self) -> bool {
        *self.closed.borrow()
    }

    pub fn check_open(&self) -> Result<(), RuntimeError> {
        if self.is_closed() {
            Err(value_error("I/O operation on closed file."))
        } else {
            Ok(())
        }
    }

    pub fn close(&self) {
        *self.closed.borrow_mut() = true;
    }

    /// Read up to `n` bytes from the backend; `None` reads everything.
    /// Returns bytes regardless of mode — the caller decides whether
    /// to wrap them as `str` (text mode) or `bytes` (binary mode).
    pub fn read_bytes(&self, n: Option<usize>) -> Result<Vec<u8>, RuntimeError> {
        self.check_open()?;
        let mut backend = self.backend.borrow_mut();
        let mut buf = Vec::new();
        match (&mut *backend, n) {
            (FileBackend::Disk(f), Some(n)) => {
                buf.resize(n, 0);
                let read = f
                    .read(&mut buf)
                    .map_err(|e| os_error(format!("read: {e}")))?;
                buf.truncate(read);
            }
            (FileBackend::Disk(f), None) => {
                f.read_to_end(&mut buf)
                    .map_err(|e| os_error(format!("read: {e}")))?;
            }
            (FileBackend::MemBytes { data, pos }, None) => {
                buf.extend_from_slice(&data[*pos..]);
                *pos = data.len();
            }
            (FileBackend::MemBytes { data, pos }, Some(n)) => {
                let end = (*pos + n).min(data.len());
                buf.extend_from_slice(&data[*pos..end]);
                *pos = end;
            }
            (FileBackend::MemText { data, pos }, None) => {
                buf.extend_from_slice(&data.as_bytes()[*pos..]);
                *pos = data.len();
            }
            (FileBackend::MemText { data, pos }, Some(n)) => {
                let bytes = data.as_bytes();
                let mut end = *pos;
                let mut taken = 0;
                while taken < n && end < bytes.len() {
                    let ch_len = u32::from(bytes[end]).leading_ones().max(1) as usize;
                    let ch_len = if bytes[end] < 0x80 { 1 } else { ch_len };
                    let stop = (end + ch_len).min(bytes.len());
                    buf.extend_from_slice(&bytes[end..stop]);
                    end = stop;
                    taken += 1;
                }
                *pos = end;
            }
            (FileBackend::Stdout(_) | FileBackend::Stderr(_), _) => {
                return Err(os_error("not readable"));
            }
            (FileBackend::Stdin, _) => {
                let mut s = String::new();
                std::io::stdin()
                    .read_to_string(&mut s)
                    .map_err(|e| os_error(format!("read: {e}")))?;
                buf = s.into_bytes();
            }
        }
        Ok(buf)
    }

    /// Read until the next ``\n`` (inclusive) or EOF and return the
    /// resulting string. Used by iteration over text-mode files and
    /// ``StringIO`` instances; binary-mode files refuse the call to
    /// avoid silently decoding non-UTF-8 bytes.
    pub fn readline_unbounded(&self) -> Result<String, RuntimeError> {
        let mut out: Vec<u8> = Vec::new();
        loop {
            let b = self.read_bytes(Some(1))?;
            if b.is_empty() {
                break;
            }
            out.extend_from_slice(&b);
            if b[0] == b'\n' {
                break;
            }
        }
        if self.binary {
            // Binary mode: caller should iterate by ``readline``
            // explicitly; the iterator protocol implicitly decodes
            // to str, which would be wrong for bytes.
            let _ = out;
            return Err(type_error(
                "binary mode files are not iterable in text mode".to_owned(),
            ));
        }
        String::from_utf8(out).map_err(|e| value_error(e.to_string()))
    }

    pub fn write_bytes(&self, data: &[u8]) -> Result<usize, RuntimeError> {
        self.check_open()?;
        let mut backend = self.backend.borrow_mut();
        let n = match &mut *backend {
            FileBackend::Disk(f) => f.write(data).map_err(|e| os_error(format!("write: {e}")))?,
            FileBackend::MemBytes { data: buf, pos } => {
                if *pos == buf.len() {
                    buf.extend_from_slice(data);
                } else {
                    let end = (*pos + data.len()).min(buf.len());
                    buf[*pos..end].copy_from_slice(&data[..end - *pos]);
                    if data.len() > end - *pos {
                        buf.extend_from_slice(&data[end - *pos..]);
                    }
                }
                *pos += data.len();
                data.len()
            }
            FileBackend::MemText { data: buf, pos } => {
                let s = std::str::from_utf8(data)
                    .map_err(|_| value_error("StringIO requires utf-8 bytes"))?;
                if *pos == buf.len() {
                    buf.push_str(s);
                } else {
                    // Simple replace: drop trailing & append.
                    buf.truncate(*pos);
                    buf.push_str(s);
                }
                *pos = buf.len();
                data.len()
            }
            FileBackend::Stdout(sink) => sink
                .borrow_mut()
                .write(data)
                .map_err(|e| os_error(format!("write: {e}")))?,
            FileBackend::Stderr(sink) => sink
                .borrow_mut()
                .write(data)
                .map_err(|e| os_error(format!("write: {e}")))?,
            FileBackend::Stdin => return Err(os_error("not writable")),
        };
        Ok(n)
    }

    pub fn flush(&self) -> Result<(), RuntimeError> {
        self.check_open()?;
        let mut backend = self.backend.borrow_mut();
        match &mut *backend {
            FileBackend::Disk(f) => f.flush().map_err(|e| os_error(format!("flush: {e}")))?,
            FileBackend::Stdout(sink) => sink
                .borrow_mut()
                .flush()
                .map_err(|e| os_error(format!("flush: {e}")))?,
            FileBackend::Stderr(sink) => sink
                .borrow_mut()
                .flush()
                .map_err(|e| os_error(format!("flush: {e}")))?,
            _ => {}
        }
        Ok(())
    }

    /// Read every byte remaining and decode as UTF-8. Convenience for
    /// stdlib helpers (e.g. `json.load`) that want text regardless of
    /// the file's nominal mode.
    pub fn read_text_all(&self) -> Result<String, RuntimeError> {
        let bytes = self.read_bytes(None)?;
        String::from_utf8(bytes).map_err(|_| value_error("invalid UTF-8 in input"))
    }

    /// Write a UTF-8 string as bytes. Mirrors `read_text_all` and is
    /// safe to call on any mode supporting writes.
    pub fn write_text(&self, s: &str) -> Result<usize, RuntimeError> {
        self.write_bytes(s.as_bytes())
    }

    /// Current position. Works for both in-memory buffers and disk files.
    pub fn position(&self) -> usize {
        match &mut *self.backend.borrow_mut() {
            FileBackend::MemBytes { pos, .. } | FileBackend::MemText { pos, .. } => *pos,
            FileBackend::Disk(f) => {
                use std::io::Seek;
                f.stream_position().map(|n| n as usize).unwrap_or(0)
            }
            _ => 0,
        }
    }

    /// Seek to absolute position. Returns the new position.
    pub fn seek(&self, offset: isize, whence: i32) -> Result<usize, RuntimeError> {
        self.check_open()?;
        let mut backend = self.backend.borrow_mut();
        match &mut *backend {
            FileBackend::MemBytes { data, pos } => {
                let new_pos = match whence {
                    0 => offset.max(0) as usize,
                    1 => (*pos as isize + offset).max(0) as usize,
                    2 => (data.len() as isize + offset).max(0) as usize,
                    _ => return Err(value_error("invalid whence")),
                };
                *pos = new_pos.min(data.len());
                Ok(*pos)
            }
            FileBackend::MemText { data, pos } => {
                let new_pos = match whence {
                    0 => offset.max(0) as usize,
                    1 => (*pos as isize + offset).max(0) as usize,
                    2 => (data.len() as isize + offset).max(0) as usize,
                    _ => return Err(value_error("invalid whence")),
                };
                *pos = new_pos.min(data.len());
                Ok(*pos)
            }
            FileBackend::Disk(f) => {
                use std::io::Seek;
                let whence_pos = match whence {
                    0 => std::io::SeekFrom::Start(offset.max(0) as u64),
                    1 => std::io::SeekFrom::Current(offset as i64),
                    2 => std::io::SeekFrom::End(offset as i64),
                    _ => return Err(value_error("invalid whence")),
                };
                let n = f
                    .seek(whence_pos)
                    .map_err(|e| os_error(format!("seek: {e}")))?;
                Ok(n as usize)
            }
            _ => Err(os_error("stream is not seekable")),
        }
    }

    /// Extract the entire buffer of an in-memory StringIO/BytesIO.
    /// Used by `StringIO.getvalue()` / `BytesIO.getvalue()`.
    pub fn getvalue(&self) -> Option<Object> {
        match &*self.backend.borrow() {
            FileBackend::MemBytes { data, .. } => Some(Object::Bytes(Rc::from(data.as_slice()))),
            FileBackend::MemText { data, .. } => Some(Object::from_str(data.clone())),
            _ => None,
        }
    }
}

impl fmt::Debug for PyFile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "<file {:?} mode={:?}>", self.name, self.mode)
    }
}

/// Concrete backing store for a [`PyFile`].
pub enum FileBackend {
    /// A real file on disk.
    Disk(std::fs::File),
    /// In-memory byte buffer (`io.BytesIO`).
    MemBytes { data: Vec<u8>, pos: usize },
    /// In-memory UTF-8 buffer (`io.StringIO`).
    MemText { data: String, pos: usize },
    /// The interpreter's process stdout sink.
    Stdout(Rc<RefCell<dyn Write + Send + Sync>>),
    /// The interpreter's process stderr sink.
    Stderr(Rc<RefCell<dyn Write + Send + Sync>>),
    /// Process stdin (read-only).
    Stdin,
}

impl fmt::Debug for FileBackend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Disk(_) => write!(f, "Disk"),
            Self::MemBytes { .. } => write!(f, "MemBytes"),
            Self::MemText { .. } => write!(f, "MemText"),
            Self::Stdout(_) => write!(f, "Stdout"),
            Self::Stderr(_) => write!(f, "Stderr"),
            Self::Stdin => write!(f, "Stdin"),
        }
    }
}

/// State of an active iterator. Slim by design — every iterable
/// type implements its own iteration here (no Python-level iterator
/// protocol yet).
#[derive(Debug, Clone)]
pub enum PyIterator {
    List {
        items: Rc<RefCell<Vec<Object>>>,
        index: usize,
    },
    Tuple {
        items: Rc<[Object]>,
        index: usize,
    },
    Str {
        s: Rc<str>,
        index: usize,
    },
    Range {
        current: i64,
        stop: i64,
        step: i64,
    },
    DictKeys {
        keys: Vec<DictKey>,
        index: usize,
    },
    Bytes {
        data: Rc<[u8]>,
        index: usize,
    },
    /// Lazy `enumerate(...)`. Holds a *shared* handle to the wrapped
    /// iterator so consuming the enumerate also advances the original
    /// (CPython: `enumerate(it)` yields from the same `it`, leaving it
    /// positioned right after the last item produced).
    Enumerate {
        inner: Rc<RefCell<PyIterator>>,
        count: i64,
    },
}

impl PyIterator {
    /// Pull the next value out of the iterator, or `None` if exhausted.
    pub fn next_value(&mut self) -> Option<Object> {
        match self {
            PyIterator::List { items, index } => {
                let v = items.borrow().get(*index).cloned()?;
                *index += 1;
                Some(v)
            }
            PyIterator::Tuple { items, index } => {
                let v = items.get(*index).cloned()?;
                *index += 1;
                Some(v)
            }
            PyIterator::Str { s, index } => {
                let bytes = s.as_bytes();
                if *index >= bytes.len() {
                    return None;
                }
                // Advance by one UTF-8 code point.
                let rest = &s[*index..];
                let ch = rest.chars().next()?;
                let len = ch.len_utf8();
                *index += len;
                Some(Object::Str(Rc::from(ch.to_string().as_str())))
            }
            PyIterator::Range {
                current,
                stop,
                step,
            } => {
                let exhausted = if *step > 0 {
                    *current >= *stop
                } else if *step < 0 {
                    *current <= *stop
                } else {
                    true
                };
                if exhausted {
                    return None;
                }
                let v = *current;
                *current += *step;
                Some(Object::Int(v))
            }
            PyIterator::DictKeys { keys, index } => {
                let k = keys.get(*index)?.clone();
                *index += 1;
                Some(k.0)
            }
            PyIterator::Bytes { data, index } => {
                let v = data.get(*index).copied()?;
                *index += 1;
                Some(Object::Int(i64::from(v)))
            }
            PyIterator::Enumerate { inner, count } => {
                let v = inner.borrow_mut().next_value()?;
                let i = *count;
                *count += 1;
                Some(Object::new_tuple(vec![Object::Int(i), v]))
            }
        }
    }

    /// Number of items remaining, when cheaply known. Backs the
    /// `__length_hint__` slot CPython's built-in iterators expose
    /// (`operator.length_hint`, list pre-sizing, …). Returns `None`
    /// for sources whose remaining length isn't known in O(1).
    pub fn remaining(&self) -> Option<usize> {
        match self {
            PyIterator::List { items, index } => {
                Some(items.borrow().len().saturating_sub(*index))
            }
            PyIterator::Tuple { items, index } => Some(items.len().saturating_sub(*index)),
            PyIterator::Str { s, index } => Some(s[(*index).min(s.len())..].chars().count()),
            PyIterator::DictKeys { keys, index } => Some(keys.len().saturating_sub(*index)),
            PyIterator::Bytes { data, index } => Some(data.len().saturating_sub(*index)),
            PyIterator::Enumerate { inner, .. } => inner.borrow().remaining(),
            PyIterator::Range {
                current,
                stop,
                step,
            } => {
                if *step > 0 && *current < *stop {
                    Some((((*stop - *current) as i128 + i128::from(*step) - 1)
                        / i128::from(*step)) as usize)
                } else if *step < 0 && *current > *stop {
                    Some((((*current - *stop) as i128 + i128::from(-*step) - 1)
                        / i128::from(-*step)) as usize)
                } else {
                    Some(0)
                }
            }
        }
    }
}

// ---------- behavior ----------

impl Object {
    /// Python truthiness.
    pub fn is_truthy(&self) -> bool {
        match self {
            Object::None => false,
            Object::Bool(b) => *b,
            Object::Int(i) => *i != 0,
            Object::Long(b) => !b.is_zero(),
            Object::Complex(c) => c.real != 0.0 || c.imag != 0.0,
            Object::Float(f) => *f != 0.0 && !f.is_nan(),
            Object::Str(s) => !s.is_empty(),
            Object::Tuple(items) => !items.is_empty(),
            Object::List(items) => !items.borrow().is_empty(),
            Object::Dict(d) => !d.borrow().is_empty(),
            Object::Range(r) => {
                if r.step > 0 {
                    r.start < r.stop
                } else if r.step < 0 {
                    r.start > r.stop
                } else {
                    false
                }
            }
            Object::Function(_)
            | Object::Builtin(_)
            | Object::BoundMethod(_)
            | Object::Code(_)
            | Object::Iter(_)
            | Object::Slice(_)
            | Object::Type(_)
            | Object::Module(_)
            | Object::Generator(_)
            | Object::Coroutine(_)
            | Object::AsyncGenerator(_)
            | Object::File(_)
            | Object::Property(_)
            | Object::StaticMethod(_)
            | Object::ClassMethod(_)
            | Object::SlotDescriptor(_)
            | Object::Frame(_)
            | Object::Traceback(_) => true,
            Object::MemoryView(mv) => mv.len.get() > 0,
            Object::MappingProxy(d) => !d.borrow().is_empty(),
            Object::DictView(v) => !v.dict.borrow().is_empty(),
            Object::SimpleNamespace(_) => true,
            Object::Bytes(b) => !b.is_empty(),
            Object::ByteArray(b) => !b.borrow().is_empty(),
            Object::Set(s) => !s.borrow().is_empty(),
            Object::FrozenSet(s) => !s.is_empty(),
            Object::Cell(inner) => inner.borrow().is_truthy(),
            Object::Instance(inst) => {
                // int/str/… subclass instances are truthy per their
                // wrapped value unless the class overrides __bool__/__len__.
                if inst.class.lookup("__bool__").is_none() && inst.class.lookup("__len__").is_none()
                {
                    if let Some(native) = &inst.native {
                        return native.is_truthy();
                    }
                }
                // Honour __bool__ then __len__ before defaulting to True.
                if let Some(m) = inst.class.lookup("__bool__") {
                    // Caller dispatches; we cannot run Python here.
                    // Default to True; the dispatch site handles the
                    // dunder dispatch when it has interpreter access.
                    let _ = m;
                    true
                } else if let Some(m) = inst.class.lookup("__len__") {
                    let _ = m;
                    true
                } else {
                    true
                }
            }
        }
    }

    /// `is` operator semantics.
    pub fn is_same(&self, other: &Self) -> bool {
        match (self, other) {
            (Object::None, Object::None) => true,
            (Object::Bool(a), Object::Bool(b)) => a == b,
            (Object::Int(a), Object::Int(b)) => a == b,
            (Object::Long(a), Object::Long(b)) => Rc::ptr_eq(a, b) || **a == **b,
            // Cross-type identity: Int and Long with the same value
            // are *equal* but not the *same* object — `is` checks
            // identity, not value. Mirror CPython by reporting only
            // structural identity, never cross-representation
            // identity. A future RFC may intern the small-int range
            // explicitly.
            (Object::Complex(a), Object::Complex(b)) => {
                Rc::ptr_eq(a, b)
                    || (a.real.to_bits() == b.real.to_bits()
                        && a.imag.to_bits() == b.imag.to_bits())
            }
            (Object::Float(a), Object::Float(b)) => a.to_bits() == b.to_bits(),
            (Object::Str(a), Object::Str(b)) => Rc::ptr_eq(a, b),
            (Object::Tuple(a), Object::Tuple(b)) => Rc::ptr_eq(a, b),
            (Object::List(a), Object::List(b)) => Rc::ptr_eq(a, b),
            (Object::Dict(a), Object::Dict(b)) => Rc::ptr_eq(a, b),
            (Object::Range(a), Object::Range(b)) => Rc::ptr_eq(a, b),
            (Object::Function(a), Object::Function(b)) => Rc::ptr_eq(a, b),
            (Object::Builtin(a), Object::Builtin(b)) => Rc::ptr_eq(a, b),
            (Object::BoundMethod(a), Object::BoundMethod(b)) => Rc::ptr_eq(a, b),
            (Object::Code(a), Object::Code(b)) => Rc::ptr_eq(a, b),
            (Object::Iter(a), Object::Iter(b)) => Rc::ptr_eq(a, b),
            (Object::Slice(a), Object::Slice(b)) => Rc::ptr_eq(a, b),
            (Object::Cell(a), Object::Cell(b)) => Rc::ptr_eq(a, b),
            (Object::Type(a), Object::Type(b)) => Rc::ptr_eq(a, b),
            (Object::Instance(a), Object::Instance(b)) => Rc::ptr_eq(a, b),
            (Object::Module(a), Object::Module(b)) => Rc::ptr_eq(a, b),
            (Object::Generator(a), Object::Generator(b)) => Rc::ptr_eq(a, b),
            (Object::Coroutine(a), Object::Coroutine(b)) => Rc::ptr_eq(a, b),
            (Object::AsyncGenerator(a), Object::AsyncGenerator(b)) => Rc::ptr_eq(a, b),
            (Object::Bytes(a), Object::Bytes(b)) => Rc::ptr_eq(a, b),
            (Object::ByteArray(a), Object::ByteArray(b)) => Rc::ptr_eq(a, b),
            (Object::Set(a), Object::Set(b)) => Rc::ptr_eq(a, b),
            (Object::FrozenSet(a), Object::FrozenSet(b)) => Rc::ptr_eq(a, b),
            (Object::File(a), Object::File(b)) => Rc::ptr_eq(a, b),
            (Object::Property(a), Object::Property(b)) => Rc::ptr_eq(a, b),
            (Object::StaticMethod(a), Object::StaticMethod(b)) => Rc::ptr_eq(a, b),
            (Object::ClassMethod(a), Object::ClassMethod(b)) => Rc::ptr_eq(a, b),
            (Object::SlotDescriptor(a), Object::SlotDescriptor(b)) => Rc::ptr_eq(a, b),
            (Object::Frame(a), Object::Frame(b)) => Rc::ptr_eq(a, b),
            (Object::Traceback(a), Object::Traceback(b)) => Rc::ptr_eq(a, b),
            (Object::MemoryView(a), Object::MemoryView(b)) => Rc::ptr_eq(a, b),
            (Object::MappingProxy(a), Object::MappingProxy(b)) => Rc::ptr_eq(a, b),
            (Object::DictView(a), Object::DictView(b)) => Rc::ptr_eq(a, b),
            (Object::SimpleNamespace(a), Object::SimpleNamespace(b)) => Rc::ptr_eq(a, b),
            _ => false,
        }
    }

    /// `==` operator semantics — recursive value equality.
    pub fn eq_value(&self, other: &Self) -> bool {
        // Subclasses of immutable built-ins (`class C(int)`,
        // `enum.IntEnum`, `_NamedIntConstant`, …) compare by the value
        // they wrap, so `C(5) == 5` and two distinct instances with the
        // same value are equal — exactly like CPython.
        let lhs_native = self.native_value();
        let rhs_native = other.native_value();
        if lhs_native.is_some() || rhs_native.is_some() {
            let l = lhs_native.as_ref().unwrap_or(self);
            let r = rhs_native.as_ref().unwrap_or(other);
            return l.eq_value(r);
        }
        match (self, other) {
            (Object::None, Object::None) => true,
            (Object::Bool(a), Object::Bool(b)) => a == b,
            (Object::Int(a), Object::Int(b)) => a == b,
            (Object::Long(a), Object::Long(b)) => **a == **b,
            (Object::Int(a), Object::Long(b)) | (Object::Long(b), Object::Int(a)) => {
                **b == BigInt::from(*a)
            }
            (Object::Bool(a), Object::Int(b)) | (Object::Int(b), Object::Bool(a)) => {
                i64::from(*a) == *b
            }
            (Object::Bool(a), Object::Long(b)) | (Object::Long(b), Object::Bool(a)) => {
                **b == BigInt::from(i64::from(*a))
            }
            (Object::Float(a), Object::Float(b)) => a == b,
            (Object::Int(a), Object::Float(b)) | (Object::Float(b), Object::Int(a)) => {
                i64_eq_f64(*a, *b)
            }
            (Object::Long(a), Object::Float(b)) | (Object::Float(b), Object::Long(a)) => {
                bigint_eq_f64(a, *b)
            }
            (Object::Bool(a), Object::Float(b)) | (Object::Float(b), Object::Bool(a)) => {
                f64::from(i64::from(*a) as i32) == *b
            }
            (Object::Complex(a), Object::Complex(b)) => a.real == b.real && a.imag == b.imag,
            (Object::Complex(c), Object::Int(i)) | (Object::Int(i), Object::Complex(c)) => {
                c.imag == 0.0 && i64_eq_f64(*i, c.real)
            }
            (Object::Complex(c), Object::Float(f)) | (Object::Float(f), Object::Complex(c)) => {
                c.imag == 0.0 && c.real == *f
            }
            (Object::Complex(c), Object::Long(b)) | (Object::Long(b), Object::Complex(c)) => {
                c.imag == 0.0 && bigint_eq_f64(b, c.real)
            }
            (Object::Str(a), Object::Str(b)) => a == b,
            // Sequence comparison is element-wise `PyObject_RichCompareBool`,
            // which is identity-first — so `[nan] == [nan]` (same nan) is true.
            (Object::Tuple(a), Object::Tuple(b)) => {
                a.len() == b.len()
                    && a.iter()
                        .zip(b.iter())
                        .all(|(x, y)| x.is_same(y) || x.eq_value(y))
            }
            (Object::List(a), Object::List(b)) => {
                let a = a.borrow();
                let b = b.borrow();
                a.len() == b.len()
                    && a.iter()
                        .zip(b.iter())
                        .all(|(x, y)| x.is_same(y) || x.eq_value(y))
            }
            (Object::Dict(a), Object::Dict(b)) => {
                let a = a.borrow();
                let b = b.borrow();
                if a.len() != b.len() {
                    return false;
                }
                for (k, v) in a.iter() {
                    match b.get(k) {
                        Some(v2) if v.eq_value(v2) => {}
                        _ => return false,
                    }
                }
                true
            }
            (Object::Bytes(a), Object::Bytes(b)) => a[..] == b[..],
            (Object::ByteArray(a), Object::ByteArray(b)) => *a.borrow() == *b.borrow(),
            (Object::Bytes(a), Object::ByteArray(b)) | (Object::ByteArray(b), Object::Bytes(a)) => {
                a[..] == *b.borrow()
            }
            (Object::Set(a), Object::Set(b)) => sets_equal(&a.borrow(), &b.borrow()),
            (Object::FrozenSet(a), Object::FrozenSet(b)) => sets_equal(a, b),
            (Object::Set(a), Object::FrozenSet(b)) | (Object::FrozenSet(b), Object::Set(a)) => {
                sets_equal(&a.borrow(), b)
            }
            // `slice` objects compare as the `(start, stop, step)` triple
            // (CPython's `slice_richcompare`), identity-first per field so
            // `slice(None)` fields (NaN-free here, but consistent) match.
            (Object::Slice(a), Object::Slice(b)) => {
                (a.start.is_same(&b.start) || a.start.eq_value(&b.start))
                    && (a.stop.is_same(&b.stop) || a.stop.eq_value(&b.stop))
                    && (a.step.is_same(&b.step) || a.step.eq_value(&b.step))
            }
            // Reference-identity equality for class / module / function
            // / builtin / method values. CPython falls back to identity
            // here, and our `in` / dict-key checks rely on it.
            (Object::Type(a), Object::Type(b)) => Rc::ptr_eq(a, b),
            (Object::Module(a), Object::Module(b)) => Rc::ptr_eq(a, b),
            (Object::Function(a), Object::Function(b)) => Rc::ptr_eq(a, b),
            (Object::Builtin(a), Object::Builtin(b)) => Rc::ptr_eq(a, b),
            (Object::Instance(a), Object::Instance(b)) => Rc::ptr_eq(a, b),
            // Bound methods compare like CPython's `method_richcompare`:
            // `__func__` by equality, `__self__` by identity. Two freshly
            // bound references to the same method on the same object are
            // therefore equal even though they're distinct allocations.
            (Object::BoundMethod(a), Object::BoundMethod(b)) => {
                a.function.eq_value(&b.function) && a.receiver.is_same(&b.receiver)
            }
            _ => false,
        }
    }

    /// Total-order comparison for the (small) set of orderable
    /// types: ints, floats, strings, tuples, lists. Other
    /// combinations return [`Err`] mapping to Python's `TypeError`.
    pub fn cmp(&self, other: &Self) -> Result<Ordering, RuntimeError> {
        use Object as O;
        // Order `int`/`str`/… subclass instances by the value they wrap.
        let lhs_native = self.native_value();
        let rhs_native = other.native_value();
        if lhs_native.is_some() || rhs_native.is_some() {
            let l = lhs_native.as_ref().unwrap_or(self);
            let r = rhs_native.as_ref().unwrap_or(other);
            return l.cmp(r);
        }
        match (self, other) {
            (O::Int(a), O::Int(b)) => Ok(a.cmp(b)),
            (O::Long(a), O::Long(b)) => Ok((**a).cmp(b)),
            (O::Int(a), O::Long(b)) => Ok(BigInt::from(*a).cmp(b)),
            (O::Long(a), O::Int(b)) => Ok((**a).cmp(&BigInt::from(*b))),
            (O::Float(a), O::Float(b)) => Ok(a
                .partial_cmp(b)
                .ok_or_else(|| value_error(format!("cannot order {a} and {b} (NaN)")))?),
            (O::Int(a), O::Float(b)) => i64_cmp_f64(*a, *b),
            (O::Float(a), O::Int(b)) => Ok(i64_cmp_f64(*b, *a)?.reverse()),
            (O::Long(a), O::Float(b)) => Ok(bigint_cmp_f64(a, *b)?),
            (O::Float(a), O::Long(b)) => Ok(bigint_cmp_f64(b, *a)?.reverse()),
            (O::Bool(a), O::Bool(b)) => Ok(a.cmp(b)),
            (O::Bool(a), O::Int(b)) => Ok(i64::from(*a).cmp(b)),
            (O::Int(a), O::Bool(b)) => Ok(a.cmp(&i64::from(*b))),
            (O::Bool(a), O::Long(b)) => Ok(BigInt::from(i64::from(*a)).cmp(b)),
            (O::Long(a), O::Bool(b)) => Ok((**a).cmp(&BigInt::from(i64::from(*b)))),
            (O::Bool(a), O::Float(b)) => Ok((i64::from(*a) as f64)
                .partial_cmp(b)
                .ok_or_else(|| value_error("cannot order with NaN"))?),
            (O::Float(a), O::Bool(b)) => Ok(a
                .partial_cmp(&(i64::from(*b) as f64))
                .ok_or_else(|| value_error("cannot order with NaN"))?),
            (O::Str(a), O::Str(b)) => Ok(a.cmp(b)),
            (O::Tuple(a), O::Tuple(b)) => seq_cmp(a, b),
            (O::List(a), O::List(b)) => {
                let a = a.borrow();
                let b = b.borrow();
                seq_cmp(&a, &b)
            }
            _ => Err(type_error(format!(
                "'<' not supported between instances of '{}' and '{}'",
                self.type_name(),
                other.type_name()
            ))),
        }
    }

    /// Membership: `x in container`.
    pub fn contains(&self, item: &Self) -> Result<bool, RuntimeError> {
        match self {
            // CPython's `PyObject_RichCompareBool` short-circuits on identity
            // before `==`, so `nan in [nan]` (the *same* nan) is `True`.
            Object::Tuple(items) => Ok(items.iter().any(|x| x.is_same(item) || x.eq_value(item))),
            Object::List(items) => Ok(items
                .borrow()
                .iter()
                .any(|x| x.is_same(item) || x.eq_value(item))),
            Object::Str(haystack) => match item {
                Object::Str(needle) => Ok(haystack.contains(&**needle)),
                _ => Err(type_error(
                    "'in <string>' requires string as left operand".to_owned(),
                )),
            },
            Object::Dict(d) => Ok(d.borrow().contains_key(&DictKey(item.clone()))),
            Object::Set(s) => Ok(s.borrow().contains(&DictKey(item.clone()))),
            Object::FrozenSet(s) => Ok(s.contains(&DictKey(item.clone()))),
            Object::Bytes(haystack) => match item {
                Object::Int(i) => Ok(*i >= 0 && *i <= 255 && haystack.contains(&(*i as u8))),
                Object::Bytes(needle) => Ok(bytes_contains(haystack, needle)),
                Object::ByteArray(needle) => Ok(bytes_contains(haystack, &needle.borrow())),
                _ => Err(type_error(
                    "a bytes-like object is required, not '".to_owned() + item.type_name() + "'",
                )),
            },
            Object::ByteArray(haystack) => match item {
                Object::Int(i) => {
                    Ok(*i >= 0 && *i <= 255 && haystack.borrow().contains(&(*i as u8)))
                }
                Object::Bytes(needle) => Ok(bytes_contains(&haystack.borrow(), needle)),
                Object::ByteArray(needle) => {
                    Ok(bytes_contains(&haystack.borrow(), &needle.borrow()))
                }
                _ => Err(type_error(
                    "a bytes-like object is required, not '".to_owned() + item.type_name() + "'",
                )),
            },
            Object::Range(r) => {
                if let Object::Int(i) = item {
                    if r.step > 0 {
                        Ok(*i >= r.start && *i < r.stop && (*i - r.start) % r.step == 0)
                    } else if r.step < 0 {
                        Ok(*i <= r.start && *i > r.stop && (r.start - *i) % (-r.step) == 0)
                    } else {
                        Ok(false)
                    }
                } else {
                    Ok(false)
                }
            }
            Object::MappingProxy(d) => Ok(d.borrow().contains_key(&DictKey(item.clone()))),
            Object::DictView(v) => {
                let d = v.dict.borrow();
                match v.kind {
                    DictViewKind::Keys => Ok(d.contains_key(&DictKey(item.clone()))),
                    DictViewKind::Values => Ok(d.values().any(|x| x.eq_value(item))),
                    DictViewKind::Items => {
                        if let Object::Tuple(t) = item {
                            if t.len() == 2 {
                                if let Some(v) = d.get(&DictKey(t[0].clone())) {
                                    return Ok(v.eq_value(&t[1]));
                                }
                            }
                        }
                        Ok(false)
                    }
                }
            }
            Object::MemoryView(mv) => match item {
                Object::Int(i) => Ok(*i >= 0 && *i <= 255 && mv.to_bytes().contains(&(*i as u8))),
                _ => Err(type_error(
                    "a bytes-like object is required for memoryview membership",
                )),
            },
            _ => Err(type_error(format!(
                "argument of type '{}' is not iterable",
                self.type_name()
            ))),
        }
    }

    /// Make an iterator over `self`. The returned [`PyIterator`]
    /// drains the source on each `next_value` call.
    pub fn make_iter(&self) -> Result<PyIterator, RuntimeError> {
        match self {
            Object::List(items) => Ok(PyIterator::List {
                items: items.clone(),
                index: 0,
            }),
            Object::Tuple(items) => Ok(PyIterator::Tuple {
                items: items.clone(),
                index: 0,
            }),
            Object::Str(s) => Ok(PyIterator::Str {
                s: s.clone(),
                index: 0,
            }),
            Object::Range(r) => Ok(PyIterator::Range {
                current: r.start,
                stop: r.stop,
                step: r.step,
            }),
            Object::Dict(d) => {
                let keys: Vec<DictKey> = d.borrow().keys().cloned().collect();
                Ok(PyIterator::DictKeys { keys, index: 0 })
            }
            Object::Set(s) => {
                let items: Vec<Object> = s.borrow().iter().map(|k| k.0.clone()).collect();
                Ok(PyIterator::List {
                    items: Rc::new(RefCell::new(items)),
                    index: 0,
                })
            }
            Object::FrozenSet(s) => {
                let items: Vec<Object> = s.iter().map(|k| k.0.clone()).collect();
                Ok(PyIterator::List {
                    items: Rc::new(RefCell::new(items)),
                    index: 0,
                })
            }
            Object::Bytes(b) => Ok(PyIterator::Bytes {
                data: b.clone(),
                index: 0,
            }),
            Object::ByteArray(b) => {
                let snapshot: Rc<[u8]> = Rc::from(b.borrow().as_slice());
                Ok(PyIterator::Bytes {
                    data: snapshot,
                    index: 0,
                })
            }
            Object::MemoryView(mv) => {
                if mv.released.get() {
                    return Err(value_error("memoryview: released"));
                }
                let snapshot: Rc<[u8]> = Rc::from(mv.to_bytes().into_boxed_slice());
                Ok(PyIterator::Bytes {
                    data: snapshot,
                    index: 0,
                })
            }
            Object::DictView(v) => {
                let d = v.dict.borrow();
                match v.kind {
                    DictViewKind::Keys => {
                        let keys: Vec<DictKey> = d.keys().cloned().collect();
                        Ok(PyIterator::DictKeys { keys, index: 0 })
                    }
                    DictViewKind::Values => {
                        let vs: Vec<Object> = d.values().cloned().collect();
                        Ok(PyIterator::List {
                            items: Rc::new(RefCell::new(vs)),
                            index: 0,
                        })
                    }
                    DictViewKind::Items => {
                        let items: Vec<Object> = d
                            .iter()
                            .map(|(k, v)| Object::new_tuple(vec![k.0.clone(), v.clone()]))
                            .collect();
                        Ok(PyIterator::List {
                            items: Rc::new(RefCell::new(items)),
                            index: 0,
                        })
                    }
                }
            }
            Object::MappingProxy(d) => {
                let keys: Vec<DictKey> = d.borrow().keys().cloned().collect();
                Ok(PyIterator::DictKeys { keys, index: 0 })
            }
            Object::File(file) => {
                // CPython iterates a text-mode file by repeatedly
                // invoking ``readline`` until it returns ``""``. We
                // realise the buffer up front (for the in-memory
                // backends; real OS handles read on demand inside
                // ``readline``) and wrap the lines in a ``List``
                // iterator — same observable behaviour.
                let mut lines: Vec<Object> = Vec::new();
                loop {
                    let line = file.readline_unbounded()?;
                    if line.is_empty() {
                        break;
                    }
                    lines.push(Object::from_str(line));
                }
                Ok(PyIterator::List {
                    items: Rc::new(RefCell::new(lines)),
                    index: 0,
                })
            }
            // A native iterator is its own iterable: `iter(it) is it` in
            // Python, and passing one to a plain builtin (`dict.fromkeys`,
            // `set`, …) must drain it rather than raise. We hand back a
            // clone of the underlying cursor, which yields the remaining
            // elements (the shared source `Rc` is preserved).
            Object::Iter(it) => Ok(it.borrow().clone()),
            _ => Err(type_error(format!(
                "'{}' object is not iterable",
                self.type_name()
            ))),
        }
    }

    pub fn type_name(&self) -> &'static str {
        match self {
            Object::None => "NoneType",
            Object::Bool(_) => "bool",
            Object::Int(_) => "int",
            Object::Long(_) => "int",
            Object::Complex(_) => "complex",
            Object::Float(_) => "float",
            Object::Str(_) => "str",
            Object::Tuple(_) => "tuple",
            Object::List(_) => "list",
            Object::Dict(_) => "dict",
            Object::Range(_) => "range",
            Object::Function(_) => "function",
            Object::Builtin(_) => "builtin_function_or_method",
            Object::BoundMethod(_) => "method",
            Object::Code(_) => "code",
            Object::Iter(_) => "iterator",
            Object::Slice(_) => "slice",
            Object::Cell(_) => "cell",
            Object::Type(_) => "type",
            // For Instance we'd ideally return the class name, but
            // type_name returns &'static; callers that need the real
            // name use Object::type_name_owned below.
            Object::Instance(_) => "object",
            Object::Module(_) => "module",
            Object::Generator(_) => "generator",
            Object::Coroutine(_) => "coroutine",
            Object::AsyncGenerator(_) => "async_generator",
            Object::Bytes(_) => "bytes",
            Object::ByteArray(_) => "bytearray",
            Object::Set(_) => "set",
            Object::FrozenSet(_) => "frozenset",
            Object::File(_) => "file",
            Object::Property(_) => "property",
            Object::StaticMethod(_) => "staticmethod",
            Object::ClassMethod(_) => "classmethod",
            Object::SlotDescriptor(_) => "member_descriptor",
            Object::Frame(_) => "frame",
            Object::Traceback(_) => "traceback",
            Object::MemoryView(_) => "memoryview",
            Object::MappingProxy(_) => "mappingproxy",
            Object::DictView(v) => v.kind.type_name(),
            Object::SimpleNamespace(_) => "SimpleNamespace",
        }
    }

    /// Like [`type_name`], but returns the user-class name for
    /// `Object::Instance` instead of the static placeholder.
    pub fn type_name_owned(&self) -> String {
        match self {
            Object::Instance(inst) => inst.class.name.clone(),
            Object::Type(t) => format!("type[{}]", t.name),
            other => other.type_name().to_owned(),
        }
    }

    /// Python `repr()` — produces a string that, when fed back, would
    /// round-trip to the same value for the basic types we support.
    pub fn repr(&self) -> String {
        match self {
            Object::None => "None".to_owned(),
            Object::Bool(b) => if *b { "True" } else { "False" }.to_owned(),
            Object::Int(i) => i.to_string(),
            Object::Long(b) => b.to_string(),
            Object::Complex(c) => complex_repr(c.real, c.imag),
            Object::Float(f) => float_repr(*f),
            Object::Str(s) => {
                // CPython quote selection (Objects/unicodeobject.c
                // `unicode_repr`): use '\'' unless the string contains a
                // single quote and no double quote, in which case use '"'
                // so the single quotes need not be escaped.
                let has_single = s.contains('\'');
                let has_double = s.contains('"');
                let quote = if has_single && !has_double { '"' } else { '\'' };
                let mut out = String::with_capacity(s.len() + 2);
                out.push(quote);
                for c in s.chars() {
                    match c {
                        '\\' => out.push_str("\\\\"),
                        '\n' => out.push_str("\\n"),
                        '\r' => out.push_str("\\r"),
                        '\t' => out.push_str("\\t"),
                        c if c == quote => {
                            out.push('\\');
                            out.push(quote);
                        }
                        c if char_is_printable(c) => out.push(c),
                        // Non-printable code points are escaped the way
                        // CPython's `unicode_repr` does: \xNN, \uNNNN or
                        // \UNNNNNNNN depending on the code-point width.
                        c => {
                            let n = c as u32;
                            if n <= 0xff {
                                out.push_str(&format!("\\x{n:02x}"));
                            } else if n <= 0xffff {
                                out.push_str(&format!("\\u{n:04x}"));
                            } else {
                                out.push_str(&format!("\\U{n:08x}"));
                            }
                        }
                    }
                }
                out.push(quote);
                out
            }
            Object::Tuple(items) => {
                let mut s = String::from("(");
                for (i, x) in items.iter().enumerate() {
                    if i > 0 {
                        s.push_str(", ");
                    }
                    s.push_str(&x.repr());
                }
                if items.len() == 1 {
                    s.push(',');
                }
                s.push(')');
                s
            }
            Object::List(items) => {
                let items = items.borrow();
                let mut s = String::from("[");
                for (i, x) in items.iter().enumerate() {
                    if i > 0 {
                        s.push_str(", ");
                    }
                    s.push_str(&x.repr());
                }
                s.push(']');
                s
            }
            Object::Dict(d) => {
                let d = d.borrow();
                let mut s = String::from("{");
                for (i, (k, v)) in d.iter().enumerate() {
                    if i > 0 {
                        s.push_str(", ");
                    }
                    s.push_str(&k.0.repr());
                    s.push_str(": ");
                    s.push_str(&v.repr());
                }
                s.push('}');
                s
            }
            Object::Range(r) => {
                if r.step == 1 {
                    format!("range({}, {})", r.start, r.stop)
                } else {
                    format!("range({}, {}, {})", r.start, r.stop, r.step)
                }
            }
            Object::Function(f) => {
                format!("<function {} at 0x{:x}>", f.name, Rc::as_ptr(f) as usize)
            }
            Object::Builtin(b) => format!("<built-in function {}>", b.name),
            Object::BoundMethod(_) => "<bound method>".to_owned(),
            Object::Code(c) => format!("<code object {}>", c.name),
            Object::Iter(_) => "<iterator>".to_owned(),
            Object::Slice(s) => format!(
                "slice({}, {}, {})",
                s.start.repr(),
                s.stop.repr(),
                s.step.repr()
            ),
            Object::Cell(inner) => format!("<cell: {}>", inner.borrow().repr()),
            Object::Type(t) => format!("<class '{}'>", t.name),
            Object::Module(m) => match &m.filename {
                Some(path) => format!("<module '{}' from '{}'>", m.name, path),
                None => format!("<module '{}' (built-in)>", m.name),
            },
            Object::Generator(g) => format!(
                "<generator object {} at 0x{:x}>",
                g.name,
                Rc::as_ptr(g) as usize
            ),
            Object::Coroutine(g) => format!(
                "<coroutine object {} at 0x{:x}>",
                g.name,
                Rc::as_ptr(g) as usize
            ),
            Object::AsyncGenerator(g) => format!(
                "<async_generator object {} at 0x{:x}>",
                g.name,
                Rc::as_ptr(g) as usize
            ),
            Object::Bytes(b) => bytes_repr(b),
            Object::ByteArray(b) => format!("bytearray({})", bytes_repr(&b.borrow())),
            Object::Set(s) => set_repr(&s.borrow(), "set"),
            Object::FrozenSet(s) => set_repr(s, "frozenset"),
            Object::File(file) => format!(
                "<_io.{} name='{}' mode='{}'>",
                if file.binary {
                    "BufferedReader"
                } else {
                    "TextIOWrapper"
                },
                file.name,
                file.mode
            ),
            Object::Instance(inst) => {
                // Defer to __repr__ on the class if present; otherwise
                // synthesize a default. The caller is expected to run
                // __repr__ through the interpreter for user methods —
                // here we only handle the default case.
                let key = DictKey(Object::from_static("__repr__"));
                let has_user_repr = inst
                    .class
                    .mro
                    .borrow()
                    .iter()
                    .any(|t| t.dict.borrow().contains_key(&key));
                if has_user_repr {
                    format!("<{} object>", inst.class.name)
                } else {
                    format!(
                        "<{} object at 0x{:x}>",
                        inst.class.name,
                        Rc::as_ptr(inst) as usize
                    )
                }
            }
            Object::Property(_) => "<property object>".to_owned(),
            // CPython 3.10+: `<staticmethod(<function f at 0x..>)>` — the
            // wrapped callable's repr is embedded so the address matches
            // `'{!r}'.format(func)`.
            Object::StaticMethod(inner) => format!("<staticmethod({})>", inner.repr()),
            Object::ClassMethod(inner) => format!("<classmethod({})>", inner.repr()),
            Object::SlotDescriptor(sd) => {
                format!("<member '{}' of '{}' objects>", sd.name, sd.class_name)
            }
            Object::Frame(fr) => format!("<frame at 0x{:x}>", Rc::as_ptr(fr) as usize),
            Object::Traceback(tb) => format!("<traceback at 0x{:x}>", Rc::as_ptr(tb) as usize),
            Object::MemoryView(mv) => format!("<memory at 0x{:x}>", Rc::as_ptr(mv) as usize),
            Object::MappingProxy(d) => {
                let body = Object::Dict(d.clone()).repr();
                format!("mappingproxy({body})")
            }
            Object::DictView(v) => {
                let d = v.dict.borrow();
                let body: Vec<String> = match v.kind {
                    DictViewKind::Keys => d.keys().map(|k| k.0.repr()).collect(),
                    DictViewKind::Values => d.values().map(|v| v.repr()).collect(),
                    DictViewKind::Items => d
                        .iter()
                        .map(|(k, v)| format!("({}, {})", k.0.repr(), v.repr()))
                        .collect(),
                };
                format!("{}([{}])", v.kind.type_name(), body.join(", "))
            }
            Object::SimpleNamespace(d) => {
                let d = d.borrow();
                let parts: Vec<String> = d
                    .iter()
                    .map(|(k, v)| format!("{}={}", k.0.to_str(), v.repr()))
                    .collect();
                format!("namespace({})", parts.join(", "))
            }
        }
    }

    /// Python `str()` — like [`repr`] but strings render without
    /// surrounding quotes.
    pub fn to_str(&self) -> String {
        match self {
            Object::Str(s) => s.to_string(),
            _ => self.repr(),
        }
    }

    /// Length, where defined. Returns `Err(TypeError)` otherwise.
    pub fn len(&self) -> Result<usize, RuntimeError> {
        match self {
            Object::Str(s) => Ok(s.chars().count()),
            Object::Tuple(items) => Ok(items.len()),
            Object::List(items) => Ok(items.borrow().len()),
            Object::Dict(d) => Ok(d.borrow().len()),
            Object::Range(r) => {
                let span = if r.step > 0 {
                    (r.stop - r.start).max(0)
                } else if r.step < 0 {
                    (r.start - r.stop).max(0)
                } else {
                    return Err(value_error("range step cannot be zero"));
                };
                let step = r.step.unsigned_abs() as i64;
                Ok(((span + step - 1) / step).max(0) as usize)
            }
            Object::Bytes(b) => Ok(b.len()),
            Object::ByteArray(b) => Ok(b.borrow().len()),
            Object::Set(s) => Ok(s.borrow().len()),
            Object::FrozenSet(s) => Ok(s.len()),
            Object::MemoryView(mv) => Ok(mv.len.get()),
            Object::MappingProxy(d) => Ok(d.borrow().len()),
            Object::DictView(v) => Ok(v.dict.borrow().len()),
            // A subclass of a built-in container (`class C(list)`, …)
            // measures the length of the native payload it wraps.
            Object::Instance(inst) => match &inst.native {
                Some(native) => native.len(),
                None => Err(type_error(format!(
                    "object of type '{}' has no len()",
                    self.type_name()
                ))),
            },
            _ => Err(type_error(format!(
                "object of type '{}' has no len()",
                self.type_name()
            ))),
        }
    }
}

fn sets_equal(a: &SetData, b: &SetData) -> bool {
    a.len() == b.len() && a.iter().all(|k| b.contains(k))
}

/// Compare a BigInt against a float.  Mirrors CPython: `inf` always
/// compares greater than any int, `-inf` always less, NaN is
/// uncomparable.
pub(crate) fn bigint_cmp_f64(a: &BigInt, b: f64) -> Result<Ordering, RuntimeError> {
    if b.is_nan() {
        return Err(value_error("cannot order with NaN"));
    }
    if b == f64::INFINITY {
        return Ok(Ordering::Less);
    }
    if b == f64::NEG_INFINITY {
        return Ok(Ordering::Greater);
    }
    // Compare integer parts first by comparing the BigInt to floor(b)
    // converted to BigInt; tie-break on fractional part.
    let trunc = b.trunc();
    let bi_trunc = bigint_from_f64_trunc(trunc);
    match a.cmp(&bi_trunc) {
        Ordering::Equal => {
            let frac = b - trunc;
            if frac == 0.0 {
                Ok(Ordering::Equal)
            } else if frac > 0.0 {
                Ok(Ordering::Less)
            } else {
                Ok(Ordering::Greater)
            }
        }
        other => Ok(other),
    }
}

pub(crate) fn bigint_eq_f64(a: &BigInt, b: f64) -> bool {
    if !b.is_finite() {
        return false;
    }
    if b.fract() != 0.0 {
        return false;
    }
    let bi = bigint_from_f64_trunc(b);
    *a == bi
}

/// Smallest power of two that is *not* exactly representable beyond the
/// f64 integer-precision boundary; `2f64.powi(63)` as a literal so the
/// i64-range checks below stay branch-cheap.
const TWO_POW_63: f64 = 9_223_372_036_854_775_808.0;

/// Exact `i64 == f64`. A plain `a as f64 == b` loses precision for
/// `|a| > 2**53`, making e.g. `float(2**53 + 1) == 2**53 + 1` wrongly
/// `True`. CPython compares an int and a float *exactly*; this mirrors
/// that without allocating a `BigInt` for the common in-range case.
pub(crate) fn i64_eq_f64(a: i64, b: f64) -> bool {
    if !b.is_finite() || b.fract() != 0.0 {
        return false;
    }
    // `b` is integral; it can equal an `i64` only inside `[-2**63, 2**63)`.
    if (-TWO_POW_63..TWO_POW_63).contains(&b) {
        (b as i64) == a
    } else {
        false
    }
}

/// Exact `i64` vs `f64` ordering (see [`i64_eq_f64`]).
pub(crate) fn i64_cmp_f64(a: i64, b: f64) -> Result<Ordering, RuntimeError> {
    if b.is_nan() {
        return Err(value_error("cannot order with NaN"));
    }
    if b == f64::INFINITY {
        return Ok(Ordering::Less);
    }
    if b == f64::NEG_INFINITY {
        return Ok(Ordering::Greater);
    }
    let trunc = b.trunc();
    if (-TWO_POW_63..TWO_POW_63).contains(&trunc) {
        let ti = trunc as i64;
        match a.cmp(&ti) {
            Ordering::Equal => {
                let frac = b - trunc;
                if frac == 0.0 {
                    Ok(Ordering::Equal)
                } else if frac > 0.0 {
                    Ok(Ordering::Less)
                } else {
                    Ok(Ordering::Greater)
                }
            }
            other => Ok(other),
        }
    } else if trunc > 0.0 {
        // |b| ≥ 2**63 is larger in magnitude than any i64.
        Ok(Ordering::Less)
    } else {
        Ok(Ordering::Greater)
    }
}

/// CPython's hash for ints: `value mod (2**61 - 1)` for 64-bit
/// platforms.  This keeps `hash(1) == hash(1.0) == hash(True)` and
/// also `hash(big) == hash(int(big))` for any big int.
pub(crate) const PYTHON_HASH_MODULUS: u64 = (1u64 << 61) - 1;

pub(crate) fn python_int_hash_i64(value: i64) -> i64 {
    let modulus = i128::from(PYTHON_HASH_MODULUS);
    let r = i128::from(value).rem_euclid(modulus) as i64;
    // CPython returns -2 instead of -1 for hash collisions with the
    // sentinel; we don't need the singularity handling at this level
    // because the broader Rust hasher xors in additional bits.
    r
}

pub(crate) fn python_int_hash_bigint(value: &BigInt) -> i64 {
    use num_integer::Integer;
    let modulus = BigInt::from(PYTHON_HASH_MODULUS);
    let (_, rem) = value.div_mod_floor(&modulus);
    rem.to_i64().unwrap_or(0)
}

/// Width of the Python numeric hash reduction: `_PyHASH_BITS` (61 on
/// 64-bit, so the modulus is the Mersenne prime `2**61 - 1`).
const PY_HASH_BITS: u32 = 61;
/// `sys.hash_info.inf` — the hash of `±inf` (CPython `_PyHASH_INF`).
pub(crate) const PY_HASH_INF: i64 = 314_159;
/// `sys.hash_info.imag` — the multiplier for a complex's imaginary part.
const PY_HASH_IMAG: u64 = 1_000_003;

/// C `frexp`: split `x` into `(m, e)` with `x == m * 2**e` and
/// `0.5 <= |m| < 1` (or `m == 0`). Handles subnormals; callers guard
/// against non-finite inputs.
fn py_frexp(x: f64) -> (f64, i32) {
    if x == 0.0 || !x.is_finite() {
        return (x, 0);
    }
    let bits = x.to_bits();
    let raw_exp = ((bits >> 52) & 0x7ff) as i32;
    if raw_exp == 0 {
        // Subnormal: scale into the normal range (× 2**54), then correct.
        let (m, e) = py_frexp(x * 18_014_398_509_481_984.0_f64);
        return (m, e - 54);
    }
    // Normal value = ±(1.frac) * 2**(raw_exp-1023). Forcing the stored
    // exponent field to 1022 (factor 2**-1) yields a mantissa in [0.5, 1);
    // the true binary exponent is then `raw_exp - 1022`.
    let e = raw_exp - 1022;
    let m = f64::from_bits((bits & 0x800f_ffff_ffff_ffff) | (1022u64 << 52));
    (m, e)
}

/// CPython `_Py_HashDouble`: the canonical hash of a finite double via
/// reduction modulo `2**61 - 1`, so an integer-valued float hashes equal
/// to the corresponding `int` and a `Fraction`/`Decimal` of equal value.
pub(crate) fn py_hash_double(v: f64) -> i64 {
    const MOD: u64 = (1u64 << PY_HASH_BITS) - 1;
    if !v.is_finite() {
        if v.is_infinite() {
            return if v > 0.0 { PY_HASH_INF } else { -PY_HASH_INF };
        }
        // NaN. CPython 3.10+ uses the object's identity; for value hashing
        // 0 is a stable, collision-tolerant choice (matches sys.hash_info.nan).
        return 0;
    }
    let (mut m, mut e) = py_frexp(v);
    let sign: i64 = if m < 0.0 {
        m = -m;
        -1
    } else {
        1
    };
    // Accumulate 28 bits of mantissa at a time, rotating left within the
    // 61-bit field (mirrors the C loop exactly).
    let mut x: u64 = 0;
    while m != 0.0 {
        x = ((x << 28) & MOD) | (x >> (PY_HASH_BITS - 28));
        m *= 268_435_456.0; // 2**28
        e -= 28;
        let y = m as u64;
        m -= y as f64;
        x += y;
        if x >= MOD {
            x -= MOD;
        }
    }
    // Fold in the leftover power of two via a 61-bit rotate.
    let mut e = e % (PY_HASH_BITS as i32);
    if e < 0 {
        e += PY_HASH_BITS as i32;
    }
    let e = e as u32;
    x = ((x << e) & MOD) | (x >> (PY_HASH_BITS - e));
    let mut res = (x as i64) * sign;
    if res == -1 {
        res = -2;
    }
    res
}

/// CPython `long_hash` for a machine int: `sign * (|n| mod (2**61-1))`,
/// with the reserved `-1` remapped to `-2`.
pub(crate) fn py_hash_long_i64(n: i64) -> i64 {
    const MOD: u128 = (1u128 << PY_HASH_BITS) - 1;
    let mut x = ((n as i128).unsigned_abs() % MOD) as i64;
    if n < 0 {
        x = -x;
    }
    if x == -1 {
        x = -2;
    }
    x
}

/// CPython `long_hash` for a big int. `BigInt %` is truncating, so it
/// already carries the dividend's sign with magnitude `|n| mod P`.
pub(crate) fn py_hash_long_bigint(value: &BigInt) -> i64 {
    let modulus = BigInt::from((1u64 << PY_HASH_BITS) - 1);
    let rem = value % &modulus;
    let mut x = rem.to_i64().unwrap_or(0);
    if x == -1 {
        x = -2;
    }
    x
}

/// CPython `complex_hash`: `hash(real) + _PyHASH_IMAG * hash(imag)` in
/// wrapping (mod 2**64) arithmetic, with `-1` remapped to `-2`. A
/// zero-imaginary complex therefore hashes equal to the bare float.
pub(crate) fn py_hash_complex(re: f64, im: f64) -> i64 {
    let hr = py_hash_double(re) as u64;
    let hi = py_hash_double(im) as u64;
    let combined = hr.wrapping_add(PY_HASH_IMAG.wrapping_mul(hi));
    let res = combined as i64;
    if res == -1 {
        -2
    } else {
        res
    }
}

/// Exact CPython `hash()` for the built-in numeric types, so that equal
/// values across `bool`/`int`/`float`/`complex` (and the pure-Python
/// `Fraction`/`Decimal`, which implement the same reduction) all agree.
/// Returns `None` for non-numeric objects.
pub(crate) fn numeric_hash(obj: &Object) -> Option<i64> {
    match obj {
        Object::Bool(b) => Some(py_hash_long_i64(i64::from(*b))),
        Object::Int(i) => Some(py_hash_long_i64(*i)),
        Object::Long(b) => Some(py_hash_long_bigint(b)),
        Object::Float(f) => Some(py_hash_double(*f)),
        Object::Complex(c) => Some(py_hash_complex(c.real, c.imag)),
        _ => None,
    }
}

/// `hash(None)` — CPython 3.12 returns this fixed constant rather than a
/// pointer-derived value.
const PY_HASH_NONE: i64 = 0xFCA8_6420;

/// Deterministic structural hash for a byte slice (backs both `str` and
/// `bytes`). CPython randomises string hashing per process via SipHash, so
/// we don't need to reproduce its exact output — only to be stable within a
/// run so equal strings bucket together. `hash("") == hash(b"") == 0`,
/// matching CPython, and the reserved `-1` is remapped to `-2`.
fn py_hash_bytes_slice(bytes: &[u8]) -> i64 {
    if bytes.is_empty() {
        return 0;
    }
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut h);
    let v = h.finish() as i64;
    if v == -1 {
        -2
    } else {
        v
    }
}

/// Identity-based hash for objects that hash by allocation identity in
/// CPython (functions, types, modules, plain instances without a custom
/// `__hash__`, …). Mirrors CPython's pointer hash: rotate so the low
/// alignment zero-bits don't waste bucket entropy, remapping `-1` to `-2`.
pub(crate) fn identity_hash(obj: &Object) -> i64 {
    fn rot(p: *const ()) -> i64 {
        let u = p as usize as u64;
        let v = (u >> 4 | u << 60) as i64;
        if v == -1 {
            -2
        } else {
            v
        }
    }
    match obj {
        Object::Function(r) => rot(Rc::as_ptr(r).cast()),
        Object::Builtin(r) => rot(Rc::as_ptr(r).cast()),
        Object::BoundMethod(r) => rot(Rc::as_ptr(r).cast()),
        Object::Code(r) => rot(Rc::as_ptr(r).cast()),
        Object::Cell(r) => rot(Rc::as_ptr(r).cast()),
        Object::Iter(r) => rot(Rc::as_ptr(r).cast()),
        Object::Slice(r) => rot(Rc::as_ptr(r).cast()),
        Object::Type(r) => rot(Rc::as_ptr(r).cast()),
        Object::Instance(r) => rot(Rc::as_ptr(r).cast()),
        Object::Module(r) => rot(Rc::as_ptr(r).cast()),
        Object::Generator(r) | Object::Coroutine(r) | Object::AsyncGenerator(r) => {
            rot(Rc::as_ptr(r).cast())
        }
        Object::File(r) => rot(Rc::as_ptr(r).cast()),
        Object::Property(r) => rot(Rc::as_ptr(r).cast()),
        Object::StaticMethod(r) => rot(Rc::as_ptr(r).cast()),
        Object::ClassMethod(r) => rot(Rc::as_ptr(r).cast()),
        Object::SlotDescriptor(r) => rot(Rc::as_ptr(r).cast()),
        Object::Frame(r) => rot(Rc::as_ptr(r).cast()),
        Object::Traceback(r) => rot(Rc::as_ptr(r).cast()),
        Object::MemoryView(r) => rot(Rc::as_ptr(r).cast()),
        Object::SimpleNamespace(r) => rot(Rc::as_ptr(r).cast()),
        // Value-hashable variants never reach here (handled by
        // `py_hash_value`); anything else gets a stable constant.
        _ => 0,
    }
}

/// Canonical Python `hash(obj)` value, shared by the `hash()` builtin and
/// the [`DictKey`] hasher. Bucketing every key by this single value (rather
/// than a type-tagged structural hash) is what lets objects Python considers
/// equal-and-hashable collide regardless of Rust representation — e.g. a
/// custom `__hash__` returning `hash('halibut')` buckets with the actual
/// string, so a `set`/`dict` can dedup them via [`DictKey::eq`].
///
/// Returns `None` for objects with no *value* hash (identity-hashable or
/// unhashable); callers fall back to [`identity_hash`].
pub(crate) fn py_hash_value(obj: &Object) -> Option<i64> {
    if let Some(h) = numeric_hash(obj) {
        return Some(h);
    }
    match obj {
        Object::None => Some(PY_HASH_NONE),
        Object::Str(s) => Some(py_hash_bytes_slice(s.as_bytes())),
        Object::Bytes(b) => Some(py_hash_bytes_slice(b)),
        Object::Tuple(items) => {
            // Order-sensitive mix (FNV-style) over element hashes so equal
            // tuples bucket together; unhashable elements would raise at the
            // `hash()` builtin, here they just fold their identity in.
            let mut acc: u64 = 0x345678;
            for x in items.iter() {
                let eh = py_hash_value(x).unwrap_or_else(|| identity_hash(x)) as u64;
                acc = (acc ^ eh).wrapping_mul(1_000_003).wrapping_add(items.len() as u64);
            }
            let v = acc as i64;
            Some(if v == -1 { -2 } else { v })
        }
        Object::FrozenSet(s) => {
            // Order-independent: xor scrambled element hashes (mirrors the
            // key property of CPython's `frozenset_hash`).
            let mut acc: u64 = 0;
            for k in s.iter() {
                let eh = py_hash_value(&k.0).unwrap_or_else(|| identity_hash(&k.0)) as u64;
                acc ^= eh
                    .wrapping_mul(89_869_747)
                    .wrapping_add(0x2545_F491_4F6C_DD1D);
            }
            let v = acc as i64;
            Some(if v == -1 { -2 } else { v })
        }
        Object::Instance(inst) => {
            if let Some(native) = &inst.native {
                // int/str/… subclass instance hashes as the wrapped value.
                return py_hash_value(native);
            }
            // Custom `__hash__` via the interpreter; `None` (no active
            // interpreter or only the inherited identity hash) falls through
            // to `identity_hash` at the call site.
            current_interp_hash(obj)
        }
        _ => None,
    }
}

pub(crate) fn f64_to_i64_exact(f: f64) -> Option<i64> {
    if !f.is_finite() {
        return None;
    }
    if f.fract() != 0.0 {
        return None;
    }
    if f < (i64::MIN as f64) || f > (i64::MAX as f64) {
        return None;
    }
    Some(f as i64)
}

pub(crate) fn bigint_from_f64_trunc(f: f64) -> BigInt {
    // 32-bit-sized chunks; works for any finite float.
    if f == 0.0 {
        return BigInt::from(0);
    }
    let neg = f < 0.0;
    let mut x = f.abs();
    let mut bytes: Vec<u8> = Vec::new();
    while x >= 1.0 {
        let lo = (x % 256.0) as u8;
        bytes.push(lo);
        x = (x / 256.0).floor();
    }
    bytes.reverse();
    let bi = BigInt::from_bytes_be(num_bigint::Sign::Plus, &bytes);
    if neg {
        -bi
    } else {
        bi
    }
}

/// Render a `complex` the way CPython does: bare `Xj` if real is
/// zero, `(R+Ij)` / `(R-Ij)` otherwise.  Special-cases `nan` and
/// signed zeros to match CPython's `repr` exactly.
/// CPython-compatible `repr(float)` — the shortest decimal string that
/// round-trips, switching to exponential notation exactly when CPython
/// does (`decpt <= -4 || decpt > 16`, i.e. magnitudes below 1e-4 or at
/// or above 1e16). Mirrors `float_repr` /
/// `PyOS_double_to_string(v, 'r', 0, Py_DTSF_ADD_DOT_0, ...)`.
///
/// Rust's `f64::to_string()` is *also* shortest-round-trip, but never
/// uses exponential form, so `1e100` would otherwise print as a 101-digit
/// integer. We recover the shortest digits + decimal exponent from
/// `{:e}` (Ryū) and reassemble them under CPython's rules.
pub(crate) fn float_repr(f: f64) -> String {
    if f.is_nan() {
        return "nan".to_owned();
    }
    if f.is_infinite() {
        return if f < 0.0 { "-inf" } else { "inf" }.to_owned();
    }
    if f == 0.0 {
        return if f.is_sign_negative() { "-0.0" } else { "0.0" }.to_owned();
    }
    let neg = f.is_sign_negative();
    let a = f.abs();
    let sci = format!("{a:e}");
    let (mant, exp_str) = sci.split_once('e').expect("scientific form has 'e'");
    let exp: i32 = exp_str.parse().expect("valid exponent");
    let digits: String = mant.chars().filter(|c| *c != '.').collect();
    let ndigits = digits.len() as i32;
    let decpt = exp + 1; // count of digits left of the decimal point
    let body = if decpt <= -4 || decpt > 16 {
        let e = decpt - 1;
        let mut s = digits[..1].to_owned();
        if digits.len() > 1 {
            s.push('.');
            s.push_str(&digits[1..]);
        }
        s.push('e');
        s.push(if e < 0 { '-' } else { '+' });
        s.push_str(&format!("{:02}", e.unsigned_abs()));
        s
    } else if decpt <= 0 {
        let mut s = String::from("0.");
        for _ in 0..(-decpt) {
            s.push('0');
        }
        s.push_str(&digits);
        s
    } else if decpt >= ndigits {
        let mut s = digits.clone();
        for _ in 0..(decpt - ndigits) {
            s.push('0');
        }
        s.push_str(".0");
        s
    } else {
        let d = decpt as usize;
        format!("{}.{}", &digits[..d], &digits[d..])
    };
    if neg {
        format!("-{body}")
    } else {
        body
    }
}

/// `repr`-shortest rendering of a single complex component. Unlike
/// `float`, CPython renders integer-valued complex components without a
/// trailing `.0` (e.g. `4j`, not `4.0j`), but otherwise uses the same
/// shortest/exponential rules.
pub(crate) fn complex_component_repr(p: f64) -> String {
    let r = float_repr(p);
    match r.strip_suffix(".0") {
        Some(stripped) => stripped.to_owned(),
        None => r,
    }
}

pub(crate) fn complex_repr(real: f64, imag: f64) -> String {
    let fmt_part = complex_component_repr;
    if real == 0.0 && real.is_sign_positive() {
        format!("{}j", fmt_part(imag))
    } else {
        // Insert the joining sign based on the *rendered* imaginary part,
        // not its raw sign bit: `-nan` keeps a set sign bit yet renders as
        // "nan" (no leading '-'), so CPython prints `(nan+nanj)`, and a
        // genuine negative like -2.0 renders "-2" and needs no extra '+'.
        let im = fmt_part(imag);
        let sep = if im.starts_with('-') { "" } else { "+" };
        format!("({}{sep}{im}j)", fmt_part(real))
    }
}

fn bytes_contains(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() {
        return true;
    }
    if needle.len() > haystack.len() {
        return false;
    }
    haystack.windows(needle.len()).any(|w| w == needle)
}

/// CPython's `Py_UNICODE_ISPRINTABLE`: every character is printable
/// except those in the "Other" (Cc, Cf, Cs, Co, Cn) and "Separator"
/// (Zl, Zp, Zs) general categories, with U+0020 (space) treated as
/// printable. Used by `repr(str)` (and `str.isprintable`).
pub(crate) fn char_is_printable(c: char) -> bool {
    if c == ' ' {
        return true;
    }
    use unicode_properties::{GeneralCategory as GC, UnicodeGeneralCategory};
    !matches!(
        c.general_category(),
        GC::Control
            | GC::Format
            | GC::Surrogate
            | GC::PrivateUse
            | GC::Unassigned
            | GC::LineSeparator
            | GC::ParagraphSeparator
            | GC::SpaceSeparator
    )
}

fn bytes_repr(b: &[u8]) -> String {
    let mut out = String::with_capacity(b.len() + 3);
    out.push('b');
    out.push('\'');
    for &c in b {
        match c {
            b'\\' => out.push_str("\\\\"),
            b'\'' => out.push_str("\\'"),
            b'\n' => out.push_str("\\n"),
            b'\r' => out.push_str("\\r"),
            b'\t' => out.push_str("\\t"),
            0x20..=0x7E => out.push(c as char),
            _ => out.push_str(&format!("\\x{c:02x}")),
        }
    }
    out.push('\'');
    out
}

fn set_repr(s: &SetData, name: &str) -> String {
    if s.is_empty() {
        return format!("{name}()");
    }
    let mut out = String::new();
    let use_braces = name == "set";
    if !use_braces {
        out.push_str(name);
        out.push('(');
    }
    out.push('{');
    for (i, k) in s.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        out.push_str(&k.0.repr());
    }
    out.push('}');
    if !use_braces {
        out.push(')');
    }
    out
}

fn seq_cmp(a: &[Object], b: &[Object]) -> Result<Ordering, RuntimeError> {
    for (x, y) in a.iter().zip(b.iter()) {
        match x.cmp(y)? {
            Ordering::Equal => continue,
            ord => return Ok(ord),
        }
    }
    Ok(a.len().cmp(&b.len()))
}

// ---------- factory helpers ----------

impl Object {
    pub fn from_str(s: impl Into<String>) -> Self {
        Object::Str(Rc::from(s.into().as_str()))
    }

    pub fn from_static(s: &'static str) -> Self {
        Object::Str(Rc::from(s))
    }

    pub fn new_list(items: Vec<Object>) -> Self {
        Object::List(Rc::new(RefCell::new(items)))
    }

    pub fn new_tuple(items: Vec<Object>) -> Self {
        Object::Tuple(Rc::from(items.into_boxed_slice()))
    }

    pub fn new_dict() -> Self {
        Object::Dict(Rc::new(RefCell::new(DictData::new())))
    }

    /// Build a fresh module value whose dict is empty.
    pub fn new_module(name: impl Into<String>, filename: Option<String>) -> Self {
        Object::Module(Rc::new(PyModule {
            name: name.into(),
            filename,
            dict: Rc::new(RefCell::new(DictData::new())),
        }))
    }

    pub fn new_set() -> Self {
        Object::Set(Rc::new(RefCell::new(SetData::new())))
    }

    pub fn new_set_from(iter: impl IntoIterator<Item = Object>) -> Self {
        let mut s = SetData::new();
        for v in iter {
            s.insert(DictKey(v));
        }
        Object::Set(Rc::new(RefCell::new(s)))
    }

    pub fn new_frozenset_from(iter: impl IntoIterator<Item = Object>) -> Self {
        let mut s = SetData::new();
        for v in iter {
            s.insert(DictKey(v));
        }
        Object::FrozenSet(Rc::new(s))
    }

    pub fn new_bytes(data: impl Into<Vec<u8>>) -> Self {
        let v = data.into();
        Object::Bytes(Rc::from(v.as_slice()))
    }

    pub fn new_bytearray(data: impl Into<Vec<u8>>) -> Self {
        Object::ByteArray(Rc::new(RefCell::new(data.into())))
    }

    /// Construct an integer object from a `BigInt`. Auto-demotes to
    /// `Object::Int(i64)` when the value fits, preserving the
    /// small-int fast path.
    pub fn int_from_bigint(b: BigInt) -> Self {
        match b.to_i64() {
            Some(v) => Object::Int(v),
            None => Object::Long(Rc::new(b)),
        }
    }

    /// Construct an integer from an i128, promoting to `Long` when
    /// the value doesn't fit in `i64`.
    pub fn int_from_i128(v: i128) -> Self {
        if let Ok(small) = i64::try_from(v) {
            Object::Int(small)
        } else {
            Object::Long(Rc::new(BigInt::from(v)))
        }
    }

    /// Construct a complex number.
    pub fn new_complex(real: f64, imag: f64) -> Self {
        Object::Complex(Rc::new(PyComplex::new(real, imag)))
    }

    /// View this object as a `BigInt`, treating `Bool`/`Int`/`Long`
    /// uniformly. Returns `None` for any other type.
    pub fn as_bigint(&self) -> Option<BigInt> {
        match self {
            Object::Bool(b) => Some(BigInt::from(i64::from(*b))),
            Object::Int(i) => Some(BigInt::from(*i)),
            Object::Long(b) => Some((**b).clone()),
            _ => None,
        }
    }

    /// View this object as `i64`, succeeding only when the value
    /// genuinely fits in 64 bits. Returns `None` for `Long`s that
    /// don't fit, and for non-integer types.
    /// For an instance of a subclass of a primitive built-in
    /// (`int`, `str`, …) return a clone of the underlying value the
    /// instance wraps; `None` for everything else. The wrapped value
    /// is always itself a primitive (never another `Instance`), so
    /// callers can recurse exactly once.
    #[inline]
    pub fn native_value(&self) -> Option<Object> {
        match self {
            Object::Instance(inst) => inst.native.clone(),
            _ => None,
        }
    }

    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Object::Bool(b) => Some(i64::from(*b)),
            Object::Int(i) => Some(*i),
            Object::Long(b) => b.to_i64(),
            Object::Instance(inst) => inst.native.as_ref().and_then(Object::as_i64),
            _ => None,
        }
    }

    /// View this object as a non-negative `usize`, returning `None`
    /// for negative or out-of-range values.
    pub fn as_usize(&self) -> Option<usize> {
        match self {
            Object::Bool(b) => Some(usize::from(*b)),
            Object::Int(i) if *i >= 0 => usize::try_from(*i).ok(),
            Object::Long(b) if !b.is_negative() => b.to_usize(),
            Object::Instance(inst) => inst.native.as_ref().and_then(Object::as_usize),
            _ => None,
        }
    }

    /// Test whether this object is `int`-flavoured (Bool, Int, or
    /// Long). Useful in the VM where the slot machinery treats all
    /// three uniformly.
    pub fn is_int_like(&self) -> bool {
        matches!(self, Object::Bool(_) | Object::Int(_) | Object::Long(_))
    }

    /// View this object as `f64`, with float / int / bool / long all
    /// converting losslessly-where-possible.
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Object::Bool(b) => Some(f64::from(i32::from(*b))),
            Object::Int(i) => Some(*i as f64),
            Object::Long(b) => b.to_f64(),
            Object::Float(f) => Some(*f),
            _ => None,
        }
    }

    /// View as `(real, imag)`. Includes int → real promotion.
    pub fn as_complex(&self) -> Option<(f64, f64)> {
        match self {
            Object::Complex(c) => Some((c.real, c.imag)),
            other => other.as_f64().map(|r| (r, 0.0)),
        }
    }

    /// Try to view this value as bytes (works for `bytes`, `bytearray`,
    /// and contiguous `memoryview`). Returns `None` for any other type.
    pub fn as_bytes_view(&self) -> Option<Vec<u8>> {
        match self {
            Object::Bytes(b) => Some(b.to_vec()),
            Object::ByteArray(b) => Some(b.borrow().clone()),
            Object::MemoryView(mv) => Some(mv.to_bytes()),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truthiness_matches_python_basics() {
        assert!(!Object::None.is_truthy());
        assert!(!Object::Bool(false).is_truthy());
        assert!(Object::Bool(true).is_truthy());
        assert!(!Object::Int(0).is_truthy());
        assert!(Object::Int(1).is_truthy());
        assert!(!Object::Float(0.0).is_truthy());
        assert!(!Object::Float(f64::NAN).is_truthy());
        assert!(Object::Float(1.5).is_truthy());
        assert!(!Object::from_str("").is_truthy());
        assert!(Object::from_str("x").is_truthy());
        assert!(!Object::new_list(vec![]).is_truthy());
        assert!(Object::new_list(vec![Object::Int(1)]).is_truthy());
    }

    #[test]
    fn equality_handles_numeric_coercion() {
        assert!(Object::Int(1).eq_value(&Object::Bool(true)));
        assert!(Object::Float(1.0).eq_value(&Object::Int(1)));
        assert!(!Object::Int(1).eq_value(&Object::Int(2)));
    }

    #[test]
    fn identity_of_same_list_handle() {
        let a = Object::new_list(vec![Object::Int(1)]);
        let b = a.clone();
        let c = Object::new_list(vec![Object::Int(1)]);
        assert!(a.is_same(&b));
        assert!(!a.is_same(&c));
    }

    #[test]
    fn list_iter_yields_elements_in_order() {
        let lst = Object::new_list(vec![Object::Int(1), Object::Int(2), Object::Int(3)]);
        let mut it = lst.make_iter().expect("iter");
        let a = it.next_value().and_then(|x| match x {
            Object::Int(i) => Some(i),
            _ => None,
        });
        let b = it.next_value().and_then(|x| match x {
            Object::Int(i) => Some(i),
            _ => None,
        });
        let c = it.next_value().and_then(|x| match x {
            Object::Int(i) => Some(i),
            _ => None,
        });
        let d = it.next_value();
        assert_eq!(a, Some(1));
        assert_eq!(b, Some(2));
        assert_eq!(c, Some(3));
        assert!(d.is_none());
    }
}

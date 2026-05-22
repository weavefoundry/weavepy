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

use std::cell::{Cell, RefCell};
use std::cmp::Ordering;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::io::{Read, Write};
use std::rc::Rc;

use num_bigint::BigInt;
use num_traits::{Signed, ToPrimitive, Zero};
use weavepy_compiler::CodeObject;

use crate::error::{os_error, type_error, value_error, RuntimeError};
use crate::types::{PyInstance, TypeObject};

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
    pub locals_provider: RefCell<Option<Rc<dyn Fn() -> Object>>>,
    /// Shared, mutable mirror of the running frame's `locals` array.
    /// The VM updates this between steps so `f_locals` reflects live
    /// state. `None` once the frame has returned.
    pub locals_mirror: RefCell<Option<Rc<RefCell<Vec<Object>>>>>,
    /// Per-frame trace function (PEP 669 surface — `sys.settrace` is
    /// a no-op today, but storage exists so user code can set/get the
    /// value without raising).
    pub trace: RefCell<Object>,
    /// Per-frame `f_lineno` override. CPython lets debuggers set
    /// `f_lineno` to jump to a different line; we keep storage so
    /// reads round-trip, even though writes don't actually move the
    /// program counter.
    pub override_lineno: Cell<Option<u32>>,
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

impl PartialEq for DictKey {
    fn eq(&self, other: &Self) -> bool {
        self.0.eq_value(&other.0)
    }
}

impl Eq for DictKey {}

impl Hash for DictKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        match &self.0 {
            Object::None => 0u8.hash(state),
            Object::Bool(b) => {
                1u8.hash(state);
                b.hash(state);
            }
            Object::Int(i) => {
                // Hash compatibly with Long: route through the
                // BigInt hash so `Int(0).hash() == Long(0).hash()`.
                2u8.hash(state);
                python_int_hash_i64(*i).hash(state);
            }
            Object::Long(b) => {
                2u8.hash(state);
                python_int_hash_bigint(b).hash(state);
            }
            Object::Float(f) => {
                3u8.hash(state);
                if f.fract() == 0.0 && f.is_finite() {
                    // For values representable as integers, hash
                    // through the int path so `1 == 1.0` implies
                    // `hash(1) == hash(1.0)`.
                    if let Some(as_int) = f64_to_i64_exact(*f) {
                        2u8.hash(state);
                        python_int_hash_i64(as_int).hash(state);
                    } else {
                        f.to_bits().hash(state);
                    }
                } else {
                    f.to_bits().hash(state);
                }
            }
            Object::Complex(c) => {
                // Pure-real complex hashes as the underlying float;
                // imaginary component contributes a separate
                // factor (CPython: `hash(complex) = hash(real) ^
                // (hash(imag) * IMAG)`). Constant good-enough.
                3u8.hash(state);
                c.real.to_bits().hash(state);
                c.imag.to_bits().hash(state);
            }
            Object::Str(s) => {
                4u8.hash(state);
                s.hash(state);
            }
            Object::Tuple(items) => {
                5u8.hash(state);
                items.len().hash(state);
                for x in items.iter() {
                    DictKey(x.clone()).hash(state);
                }
            }
            _ => {
                // Unhashable types — hash to a constant. Python would
                // raise TypeError; we keep it well-defined for now and
                // let the runtime raise lazily when this key is used.
                255u8.hash(state);
            }
        }
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
pub struct BuiltinFn {
    pub name: &'static str,
    pub call: Box<dyn Fn(&[Object]) -> Result<Object, RuntimeError>>,
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
    pub fn new(name: impl Into<String>, frame: Box<dyn std::any::Any>) -> Self {
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
    Created(Box<dyn std::any::Any>),
    /// Paused at a `YIELD_VALUE`.
    Suspended(Box<dyn std::any::Any>),
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
    Stdout(Rc<RefCell<dyn Write>>),
    /// The interpreter's process stderr sink.
    Stderr(Rc<RefCell<dyn Write>>),
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
#[derive(Debug)]
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
            Object::Bytes(b) => !b.is_empty(),
            Object::ByteArray(b) => !b.borrow().is_empty(),
            Object::Set(s) => !s.borrow().is_empty(),
            Object::FrozenSet(s) => !s.is_empty(),
            Object::Cell(inner) => inner.borrow().is_truthy(),
            Object::Instance(inst) => {
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
            _ => false,
        }
    }

    /// `==` operator semantics — recursive value equality.
    pub fn eq_value(&self, other: &Self) -> bool {
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
                (*a as f64) == *b
            }
            (Object::Long(a), Object::Float(b)) | (Object::Float(b), Object::Long(a)) => {
                bigint_eq_f64(a, *b)
            }
            (Object::Bool(a), Object::Float(b)) | (Object::Float(b), Object::Bool(a)) => {
                f64::from(i64::from(*a) as i32) == *b
            }
            (Object::Complex(a), Object::Complex(b)) => a.real == b.real && a.imag == b.imag,
            (Object::Complex(c), Object::Int(i)) | (Object::Int(i), Object::Complex(c)) => {
                c.imag == 0.0 && c.real == (*i as f64)
            }
            (Object::Complex(c), Object::Float(f)) | (Object::Float(f), Object::Complex(c)) => {
                c.imag == 0.0 && c.real == *f
            }
            (Object::Complex(c), Object::Long(b)) | (Object::Long(b), Object::Complex(c)) => {
                c.imag == 0.0 && bigint_eq_f64(b, c.real)
            }
            (Object::Str(a), Object::Str(b)) => a == b,
            (Object::Tuple(a), Object::Tuple(b)) => {
                a.len() == b.len() && a.iter().zip(b.iter()).all(|(x, y)| x.eq_value(y))
            }
            (Object::List(a), Object::List(b)) => {
                let a = a.borrow();
                let b = b.borrow();
                a.len() == b.len() && a.iter().zip(b.iter()).all(|(x, y)| x.eq_value(y))
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
            // Reference-identity equality for class / module / function
            // / builtin / method values. CPython falls back to identity
            // here, and our `in` / dict-key checks rely on it.
            (Object::Type(a), Object::Type(b)) => Rc::ptr_eq(a, b),
            (Object::Module(a), Object::Module(b)) => Rc::ptr_eq(a, b),
            (Object::Function(a), Object::Function(b)) => Rc::ptr_eq(a, b),
            (Object::Builtin(a), Object::Builtin(b)) => Rc::ptr_eq(a, b),
            (Object::Instance(a), Object::Instance(b)) => Rc::ptr_eq(a, b),
            _ => false,
        }
    }

    /// Total-order comparison for the (small) set of orderable
    /// types: ints, floats, strings, tuples, lists. Other
    /// combinations return [`Err`] mapping to Python's `TypeError`.
    pub fn cmp(&self, other: &Self) -> Result<Ordering, RuntimeError> {
        use Object as O;
        match (self, other) {
            (O::Int(a), O::Int(b)) => Ok(a.cmp(b)),
            (O::Long(a), O::Long(b)) => Ok((**a).cmp(b)),
            (O::Int(a), O::Long(b)) => Ok(BigInt::from(*a).cmp(b)),
            (O::Long(a), O::Int(b)) => Ok((**a).cmp(&BigInt::from(*b))),
            (O::Float(a), O::Float(b)) => Ok(a
                .partial_cmp(b)
                .ok_or_else(|| value_error(format!("cannot order {a} and {b} (NaN)")))?),
            (O::Int(a), O::Float(b)) => Ok((*a as f64)
                .partial_cmp(b)
                .ok_or_else(|| value_error("cannot order with NaN"))?),
            (O::Float(a), O::Int(b)) => Ok(a
                .partial_cmp(&(*b as f64))
                .ok_or_else(|| value_error("cannot order with NaN"))?),
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
            Object::Tuple(items) => Ok(items.iter().any(|x| x.eq_value(item))),
            Object::List(items) => Ok(items.borrow().iter().any(|x| x.eq_value(item))),
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
            Object::Float(f) => {
                if f.fract() == 0.0 && f.is_finite() {
                    format!("{f:.1}")
                } else {
                    f.to_string()
                }
            }
            Object::Str(s) => {
                let mut out = String::with_capacity(s.len() + 2);
                out.push('\'');
                for c in s.chars() {
                    match c {
                        '\\' => out.push_str("\\\\"),
                        '\'' => out.push_str("\\'"),
                        '\n' => out.push_str("\\n"),
                        '\r' => out.push_str("\\r"),
                        '\t' => out.push_str("\\t"),
                        c => out.push(c),
                    }
                }
                out.push('\'');
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
            Object::StaticMethod(_) => "<staticmethod object>".to_owned(),
            Object::ClassMethod(_) => "<classmethod object>".to_owned(),
            Object::SlotDescriptor(sd) => {
                format!("<member '{}' of '{}' objects>", sd.name, sd.class_name)
            }
            Object::Frame(fr) => format!("<frame at 0x{:x}>", Rc::as_ptr(fr) as usize),
            Object::Traceback(tb) => format!("<traceback at 0x{:x}>", Rc::as_ptr(tb) as usize),
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

/// CPython's hash for ints: `value mod (2**61 - 1)` for 64-bit
/// platforms.  This keeps `hash(1) == hash(1.0) == hash(True)` and
/// also `hash(big) == hash(int(big))` for any big int.
pub(crate) const PYTHON_HASH_MODULUS: u64 = (1u64 << 61) - 1;

pub(crate) fn python_int_hash_i64(value: i64) -> i64 {
    let modulus = PYTHON_HASH_MODULUS as i128;
    let r = (value as i128).rem_euclid(modulus) as i64;
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
    if neg { -bi } else { bi }
}

/// Render a `complex` the way CPython does: bare `Xj` if real is
/// zero, `(R+Ij)` / `(R-Ij)` otherwise.  Special-cases `nan` and
/// signed zeros to match CPython's `repr` exactly.
pub(crate) fn complex_repr(real: f64, imag: f64) -> String {
    fn fmt_part(p: f64) -> String {
        if p.fract() == 0.0 && p.is_finite() {
            format!("{p:.1}")
        } else {
            format!("{p}")
        }
    }
    if real == 0.0 && real.is_sign_positive() {
        format!("{}j", fmt_part(imag))
    } else {
        let sep = if imag.is_sign_negative() { "" } else { "+" };
        format!("({}{sep}{}j)", fmt_part(real), fmt_part(imag))
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
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Object::Bool(b) => Some(i64::from(*b)),
            Object::Int(i) => Some(*i),
            Object::Long(b) => b.to_i64(),
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

    /// Try to view this value as bytes (works for both `bytes` and
    /// `bytearray`). Returns `None` for any other type.
    pub fn as_bytes_view(&self) -> Option<Vec<u8>> {
        match self {
            Object::Bytes(b) => Some(b.to_vec()),
            Object::ByteArray(b) => Some(b.borrow().clone()),
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

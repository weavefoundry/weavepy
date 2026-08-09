//! WeavePy singleton values exposed in `builtins` — `NotImplemented`
//! and `Ellipsis`. CPython hands out the *same* object for every
//! reference: `a is NotImplemented` is an identity test, not a
//! comparison. We mirror that by building both once at process start
//! and serving the same `Rc` for the lifetime of the interpreter.
//!
//! Both values are modelled as bare `object()` instances backed by a
//! per-singleton anonymous type. This is enough for the comparison
//! sentinel use case (`return NotImplemented` from `__lt__` etc.) and
//! for the indexing protocol value bound to the `...` literal. We
//! don't yet wire either into the type system as `types.EllipsisType`
//! / `types.NotImplementedType`; nothing in the stdlib reaches for
//! those directly.

use std::sync::OnceLock;

use parking_lot::Mutex;

use crate::sync::{Rc, RefCell};

use crate::object::Object;
use crate::types::{PyInstance, TypeObject};

// `NotImplemented` / `Ellipsis` are **process-global** singletons, not
// per-thread: CPython's `x is NotImplemented` identity test must hold no
// matter which OS thread minted the value. A thread-local here was a real
// bug — `object.__subclasshook__` (and every `return NotImplemented`
// site) handed back the *current thread's* instance, so an ABC
// `issubclass()` running on a worker thread saw `ok is not NotImplemented`
// and tripped `_py_abc`'s `assert isinstance(ok, bool)` (e.g. importing
// `decimal`/`numbers` on a `multiprocessing.managers` accepter thread).
// `Object` is `Send + Sync` (it is `Arc`/`GilCell`-backed), so a single
// shared instance is safe to serve everywhere.
//
// These are stored as a plain `OnceLock<Object>` (not a `Mutex`): they are
// read on *every* `return NotImplemented` rich-compare/binop fallback and on
// every `...`/`Ellipsis` reference — one of the hottest paths in the VM. A
// per-call `Mutex::lock()` there serialised the path and measurably slowed
// io/comparison-heavy suites (test_io/test_tarfile/test_zipfile ran ~3-5×
// slower). `OnceLock` is a one-time atomic init, then a lock-free read.

thread_local! {
    /// Pending `__del__` finalizer invocations queued by the cycle
    /// GC. Drained at the next eval-loop tick by the interpreter.
    /// See [`crate::gc_trace::run_finalizer`] for the producer side.
    pub(crate) static PENDING_FINALIZERS: RefCell<Vec<Object>> = const { RefCell::new(Vec::new()) };
    /// Pending weakref-callback invocations `(callback, weakref_obj)`
    /// queued when a referent dies (cycle GC, refcount reap, registry
    /// sweep). Drained alongside the finalizer queue.
    pub(crate) static PENDING_WEAKREF_CALLBACKS: RefCell<Vec<(Object, Object)>> =
        const { RefCell::new(Vec::new()) };
}

/// Process-wide count of parked `__del__` requests across all threads'
/// [`PENDING_FINALIZERS`] queues (RFC 0058 WS2). The eval loop probes
/// for pending finalizers *every instruction*; a macOS thread-local
/// access plus a `RefCell` borrow there is measurably expensive, so
/// this relaxed atomic is the fast gate and the thread-local queue
/// stays the precise, per-thread source of truth.
static PENDING_FINALIZER_COUNT: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

/// Push an instance whose `__del__` should run at the next safe
/// point. Called by the cycle GC during its clear phase.
pub fn push_pending_finalizer(obj: Object) {
    PENDING_FINALIZERS.with(|cell| {
        cell.borrow_mut().push(obj);
    });
    PENDING_FINALIZER_COUNT.fetch_add(1, std::sync::atomic::Ordering::Release);
    crate::hot_gates::set(crate::hot_gates::PENDING_FINALIZERS);
}

/// Like [`push_pending_finalizer`], but callable from `Drop` impls:
/// tolerates thread-teardown (destroyed TLS) and re-entrant borrows
/// by silently dropping the request.
pub fn try_push_pending_finalizer(obj: Object) {
    let pushed = PENDING_FINALIZERS
        .try_with(|cell| {
            if let Ok(mut queue) = cell.try_borrow_mut() {
                queue.push(obj);
                true
            } else {
                false
            }
        })
        .unwrap_or(false);
    if pushed {
        PENDING_FINALIZER_COUNT.fetch_add(1, std::sync::atomic::Ordering::Release);
        crate::hot_gates::set(crate::hot_gates::PENDING_FINALIZERS);
    }
}

/// Drain the pending-finalizer queue. The eval loop calls this
/// at every eval-breaker tick that has the GC flag set.
pub fn drain_pending_finalizers() -> Vec<Object> {
    // Clear-drain-recheck (RFC 0059 WS2): lower the hot-gate bit before
    // draining so a producer racing this drain re-raises it; re-set it
    // ourselves if other threads' queues still hold parked work.
    crate::hot_gates::clear(crate::hot_gates::PENDING_FINALIZERS);
    let taken = PENDING_FINALIZERS.with(|cell| std::mem::take(&mut *cell.borrow_mut()));
    if !taken.is_empty() {
        PENDING_FINALIZER_COUNT.fetch_sub(taken.len(), std::sync::atomic::Ordering::Release);
    }
    if PENDING_FINALIZER_COUNT.load(std::sync::atomic::Ordering::Acquire) > 0 {
        crate::hot_gates::set(crate::hot_gates::PENDING_FINALIZERS);
    }
    taken
}

/// Whether any `__del__` requests are parked on this thread's queue —
/// the eval loop's between-bytecodes gate for running them promptly.
/// One relaxed atomic load in the (overwhelmingly common) empty case;
/// the thread-local queue is consulted only when *some* thread has
/// parked work. Teardown-safe.
pub fn has_pending_finalizers() -> bool {
    if PENDING_FINALIZER_COUNT.load(std::sync::atomic::Ordering::Acquire) == 0 {
        return false;
    }
    PENDING_FINALIZERS
        .try_with(|cell| cell.try_borrow().map(|q| !q.is_empty()).unwrap_or(false))
        .unwrap_or(false)
}

/// Queue a weakref callback `(callback, weakref_obj)` for invocation at
/// the next safe point. Teardown-safe (callable from sweep paths).
pub fn push_pending_weakref_callback(callback: Object, weakref_obj: Object) {
    let _ = PENDING_WEAKREF_CALLBACKS.try_with(|cell| {
        if let Ok(mut queue) = cell.try_borrow_mut() {
            queue.push((callback, weakref_obj));
        }
    });
}

/// Drain the pending weakref-callback queue.
pub fn drain_pending_weakref_callbacks() -> Vec<(Object, Object)> {
    PENDING_WEAKREF_CALLBACKS.with(|cell| std::mem::take(&mut *cell.borrow_mut()))
}

/// Build a singleton instance of the given built-in registry type.
/// The instance carries an empty dict — the canonical repr text
/// ("Ellipsis" / "NotImplemented") is supplied by `Object::repr`'s
/// type-keyed special case rather than a `__repr__` dict entry, so the
/// singleton's `dir()` stays identical to `object()`'s (test_descr
/// test_dir: `dir(Ellipsis) == dir(object())`).
fn make_singleton(cls: Rc<TypeObject>) -> Object {
    Object::Instance(Rc::new(PyInstance::new(cls)))
}

/// Return the unique `NotImplemented` instance, allocating it on
/// first access. Subsequent calls hand back the same `Rc`-shared
/// object so `x is NotImplemented` works. Its class is the registry's
/// `NotImplementedType` (an `object` subclass), so `type(NotImplemented)`
/// and the MRO match CPython.
pub fn not_implemented() -> Object {
    static SLOT: OnceLock<Object> = OnceLock::new();
    SLOT.get_or_init(|| {
        let cls = crate::builtin_types::builtin_types()
            .not_implemented_type_
            .clone();
        make_singleton(cls)
    })
    .clone()
}

/// Same idea for `Ellipsis` (the value of `...`); its class is the
/// registry's `ellipsis` type.
pub fn ellipsis() -> Object {
    static SLOT: OnceLock<Object> = OnceLock::new();
    SLOT.get_or_init(|| {
        let cls = crate::builtin_types::builtin_types().ellipsis_.clone();
        make_singleton(cls)
    })
    .clone()
}

/// `True` if `obj` is the canonical `Ellipsis` singleton — an instance of
/// the registry `ellipsis` type. Keyed on the type identity (there is only
/// ever one instance of it), mirroring `Object::repr`'s detection. The
/// C-API bridge uses this to hand stock extensions the static
/// `_Py_EllipsisObject` so code that tests `x == Py_Ellipsis` by pointer
/// (numpy's `prepare_index`) takes the right branch rather than rejecting a
/// freshly-boxed proxy with "only integers, slices … are valid indices".
pub fn is_ellipsis(obj: &Object) -> bool {
    if let Object::Instance(inst) = obj {
        return Rc::ptr_eq(
            &inst.cls(),
            &crate::builtin_types::builtin_types().ellipsis_,
        );
    }
    false
}

/// `True` if `obj` is the canonical `NotImplemented` singleton. The C-API
/// bridge maps it to the static `_Py_NotImplementedStruct` so extensions
/// that compare against `Py_NotImplemented` by pointer behave correctly.
pub fn is_not_implemented(obj: &Object) -> bool {
    if let Object::Instance(inst) = obj {
        return Rc::ptr_eq(
            &inst.cls(),
            &crate::builtin_types::builtin_types().not_implemented_type_,
        );
    }
    false
}

/// CPython's `help`/`copyright`/`license`/`credits` builtins are
/// `_Printer` instances: `repr(copyright)` returns the body, but
/// `copyright()` also prints it. We model them as
/// `builtin_function_or_method` callables that print + return None.
pub fn interactive_printer(name: &'static str, body: &'static str) -> Object {
    use crate::object::BuiltinFn;
    let body_for_repr = body.to_owned();
    let body_for_call = body.to_owned();
    let f = BuiltinFn {
        name,
        binds_instance: false,
        call: Box::new(move |_args: &[Object]| {
            // We can't reach the interpreter's stdout from a static
            // builtin; route through Rust's stdout for the
            // interactive case. Tests/REPL go through `print`, which
            // uses the configured sink.
            println!("{}", body_for_call);
            Ok(Object::None)
        }),
        call_kw: None,
    };
    let printer = Object::Builtin(Rc::new(f));
    // Store the message as a side-channel for the VM to surface via
    // repr if it ever cares; for now repr falls back to the
    // builtin's default "<built-in function ...>".
    let _ = body_for_repr;
    printer
}

// ---------------------------------------------------------------------------
// RFC 0025 — process-global interpreter seed.
//
// Each call to `Interpreter::default()` updates the seed; worker
// threads spawned via `_thread.start_new_thread` use the seed to
// build their own per-thread interpreter that shares the heap with
// the parent. Without this hook, workers would have to reconstruct
// the entire `sys.modules` table from scratch, which would break
// `from threading import _active`-style cross-thread visibility.
// ---------------------------------------------------------------------------

static INTERPRETER_SEED: OnceLock<Mutex<Option<crate::Interpreter>>> = OnceLock::new();
static WORKER_THREAD_ID: OnceLock<Mutex<std::collections::HashMap<u64, u64>>> = OnceLock::new();
/// The seed thread's built-in type registry. Workers adopt it (see
/// [`snapshot_interpreter`]) so `type`/`object`/… compare pointer-equal
/// across threads — class statements check metaclasses by identity.
static SEED_BUILTIN_TYPES: OnceLock<
    Mutex<Option<crate::sync::Rc<crate::builtin_types::BuiltinTypes>>>,
> = OnceLock::new();

fn seed_slot() -> &'static Mutex<Option<crate::Interpreter>> {
    INTERPRETER_SEED.get_or_init(|| Mutex::new(None))
}

fn seed_types_slot() -> &'static Mutex<Option<crate::sync::Rc<crate::builtin_types::BuiltinTypes>>>
{
    SEED_BUILTIN_TYPES.get_or_init(|| Mutex::new(None))
}

fn worker_map() -> &'static Mutex<std::collections::HashMap<u64, u64>> {
    WORKER_THREAD_ID.get_or_init(|| Mutex::new(std::collections::HashMap::new()))
}

/// Stash the parent's [`crate::Interpreter`] so future
/// `start_new_thread` calls can fork from it. Called once by
/// `Interpreter::default()`. Idempotent for repeat calls (the most
/// recent interpreter wins).
pub fn publish_interpreter_seed(interp: &crate::Interpreter) {
    let mut slot = seed_slot().lock();
    *slot = Some(interp.fork_for_thread());
    drop(slot);
    *seed_types_slot().lock() = Some(crate::builtin_types::builtin_types());
}

/// Hand out a fresh worker [`crate::Interpreter`] cloned from the
/// last-published seed. Returns `None` if no seed has been published
/// yet (callers fall back to `Interpreter::new()`).
///
/// Also installs the seed's built-in type registry on the calling
/// thread (no-op if this thread already built one) — class statements
/// executed by the worker must see the same `TypeObject`s as the seed.
pub fn snapshot_interpreter() -> Option<crate::Interpreter> {
    if let Some(bt) = seed_types_slot().lock().clone() {
        crate::builtin_types::install_shared(bt);
    }
    let slot = seed_slot().lock();
    slot.as_ref().map(|i| i.fork_for_thread())
}

/// Install the synthetic thread id (`_thread.get_ident()` value) for
/// the currently-running OS thread. Called by `start_new_thread`'s
/// worker body so `get_ident()` from inside the worker returns the
/// id `threading.Thread.ident` reports, not the raw OS thread id.
pub fn install_worker_thread_id(id: u64) {
    let native = crate::gil::current_native_thread_id();
    worker_map().lock().insert(native, id);
}

/// Clear the worker thread id on exit. Called by the worker body
/// right before the OS thread terminates.
pub fn clear_worker_thread_id() {
    let native = crate::gil::current_native_thread_id();
    worker_map().lock().remove(&native);
}

/// Drop every Python object held in this thread's TLS *now*, while the
/// caller still holds the GIL. A worker thread's TLS destructors run after
/// the GIL guard is gone; the `Rc` decrements they perform race a peer
/// thread's in-flight GC mark phase (which seeds reachability from
/// `Rc::strong_count` snapshots) and can make the peer's live objects look
/// like garbage. Called from the worker teardown in `thread_real.rs`.
pub fn clear_thread_python_tls() {
    // The process-global fast-gate counts mirror the *sum* of every
    // thread's queue lengths; entries discarded here must come off the
    // counts too, or the eval loop's per-instruction gates stay
    // permanently "hot" and every thread pays the slow thread-local
    // probe forever (the counts never reach zero again).
    let dropped_finalizers =
        PENDING_FINALIZERS.try_with(|cell| std::mem::take(&mut *cell.borrow_mut()).len());
    if let Ok(n) = dropped_finalizers {
        if n > 0 {
            PENDING_FINALIZER_COUNT.fetch_sub(n, std::sync::atomic::Ordering::Release);
        }
    }
    let _ = PENDING_WEAKREF_CALLBACKS.try_with(|cell| cell.borrow_mut().clear());
    let _ = CURRENT_THREAD_HANDLES.try_with(|cell| cell.borrow_mut().clear());
    let dropped_cext =
        PENDING_CEXT_DROPS.try_with(|cell| std::mem::take(&mut *cell.borrow_mut()).len());
    if let Ok(n) = dropped_cext {
        if n > 0 {
            PENDING_CEXT_COUNT.fetch_sub(n, std::sync::atomic::Ordering::Release);
        }
    }
    crate::builtin_types::clear_thread_type_registry();
}

/// Look up the worker thread id for the currently-running OS thread,
/// falling back to the raw OS thread id if no override is set
/// (i.e. we're on the main thread).
pub fn current_worker_thread_id() -> u64 {
    let native = crate::gil::current_native_thread_id();
    if let Some(id) = worker_map().lock().get(&native).copied() {
        return id;
    }
    native
}

/// `True` when the calling OS thread was spawned by WeavePy's own thread
/// machinery (`_thread.start_new_thread`). Foreign threads — the process
/// main thread, or a host-application thread embedding its *own*
/// interpreter (e.g. `cargo test` running several `run_source` calls in
/// parallel) — are not workers of the finalizing interpreter and must not
/// be killed by the daemon-thread shutdown check in the dispatch loop.
pub fn current_thread_is_spawned_worker() -> bool {
    let native = crate::gil::current_native_thread_id();
    worker_map().lock().contains_key(&native)
}

/// `True` when `id` is the public ident (`threading.get_ident()`
/// value) of a currently-live thread: the caller itself, a live
/// worker, or the main interpreter thread. Backs
/// `PyThreadState_SetAsyncExc`'s "number of thread states modified"
/// return (0 for a nonsense id).
pub fn thread_ident_is_live(id: u64) -> bool {
    if id == current_worker_thread_id() {
        return true;
    }
    if worker_map().lock().values().any(|v| *v == id) {
        return true;
    }
    id == crate::gil::main_thread_id()
}

// ---------------------------------------------------------------------------
// RFC 0025 — per-thread interpreter routing.
//
// The frozen `sys` module captures one set of [`Rc`] handles into the
// **main** interpreter's frame stack, exception stack, and hooks at
// process start. Worker threads spawned via
// `_thread.start_new_thread` get their own forked interpreter with
// independent `frame_stack` and `exc_info_stack`. Left alone, that
// means `sys.exc_info()` called from a worker would read the *parent*
// thread's exception, not the worker's — observable as bogus
// `AttributeError`s leaking into `threading.excepthook`.
//
// `CURRENT_THREAD_HANDLES` plugs that hole: every entry to user
// Python code (`Interpreter::call_object`, the worker bootstrap)
// installs the active interpreter's per-thread handles into this
// thread-local. The `sys` builtins read through it, so they always
// see the *current* thread's state regardless of which interpreter
// originally registered the closure.
// ---------------------------------------------------------------------------

/// Snapshot of per-thread interpreter handles. All fields are
/// [`crate::sync::Rc`] (i.e. `Arc`) so cloning into / out of the
/// thread-local is cheap and the values can outlive the interpreter
/// frame that registered them (e.g. when a builtin re-enters the VM).
#[derive(Clone, Debug)]
pub struct ThreadHandles {
    pub frame_stack: crate::object::FrameStack,
    pub exc_info_stack: Rc<RefCell<Vec<crate::error::PyException>>>,
    pub excepthook: Rc<RefCell<Object>>,
    pub unraisable_hook: Rc<RefCell<Object>>,
}

thread_local! {
    /// Stack of handles. We use a stack (not a single `Option`)
    /// so re-entrant calls — e.g. a C-extension that runs Python
    /// which runs another C-extension — restore the right
    /// frame/exception state on unwind.
    static CURRENT_THREAD_HANDLES: RefCell<Vec<ThreadHandles>> =
        const { RefCell::new(Vec::new()) };

    /// RFC 0057 WS6: one-shot registration of this OS thread's frame
    /// stack with `faulthandler`'s cross-thread registry (the analogue of
    /// CPython's per-interpreter tstate list, which
    /// `_Py_DumpTracebackThreads` walks). The guard's `Drop` at thread
    /// exit removes the entry.
    static FAULTHANDLER_REG: std::cell::RefCell<Option<FaulthandlerThreadGuard>> =
        const { std::cell::RefCell::new(None) };
}

struct FaulthandlerThreadGuard {
    ident: u64,
}

impl Drop for FaulthandlerThreadGuard {
    fn drop(&mut self) {
        crate::stdlib::faulthandler_mod::note_thread_exit(self.ident);
    }
}

/// Push `handles` as the active per-thread state. Returns a guard
/// that pops on drop, so callers can use the standard
/// "scope-guard" idiom:
///
/// ```ignore
/// let _g = vm_singletons::activate_thread_handles(handles);
/// run_user_code();
/// // guard drops here, restoring the prior state.
/// ```
pub fn activate_thread_handles(handles: ThreadHandles) -> ThreadHandlesGuard {
    // First activation on this OS thread: publish its frame stack to the
    // faulthandler registry (workers install their synthetic ident before
    // their first activation, so `current_worker_thread_id` matches
    // `threading.get_ident()`).
    let _ = FAULTHANDLER_REG.try_with(|slot| {
        if slot.borrow().is_none() {
            let ident = current_worker_thread_id();
            crate::stdlib::faulthandler_mod::note_thread_start(ident, handles.frame_stack.clone());
            *slot.borrow_mut() = Some(FaulthandlerThreadGuard { ident });
        }
    });
    CURRENT_THREAD_HANDLES.with(|cell| cell.borrow_mut().push(handles));
    ThreadHandlesGuard { _private: () }
}

/// Read-only view of the current thread's handles. Returns `None`
/// if no interpreter has activated yet on this thread (e.g. the C
/// shim is being called before `Py_Initialize`). The `sys` module
/// builtins call this on every invocation, so cloning [`Rc`]s here
/// is the price of admission for cross-thread correctness.
pub fn current_thread_handles() -> Option<ThreadHandles> {
    CURRENT_THREAD_HANDLES.with(|cell| cell.borrow().last().cloned())
}

/// Scope guard returned by [`activate_thread_handles`]. Pops the
/// most-recently-pushed handles on drop.
#[derive(Debug)]
pub struct ThreadHandlesGuard {
    _private: (),
}

impl Drop for ThreadHandlesGuard {
    fn drop(&mut self) {
        CURRENT_THREAD_HANDLES.with(|cell| {
            let _ = cell.borrow_mut().pop();
        });
    }
}

thread_local! {
    /// Stack of `*mut Interpreter` pointers, one per active
    /// VM-entry call (`call_object`, `iter_object`, …). The C-API
    /// reads the top of this stack to find a live VM when an
    /// extension function calls back into the runtime
    /// (`PyObject_CallObject(cls, ...)`, `PyObject_GetBuffer(...)`,
    /// etc.).
    ///
    /// Stored as a raw pointer because the VM owns the storage —
    /// the guard pops on drop so the pointer never outlives the
    /// owning `&mut Interpreter` borrow.
    static CURRENT_INTERPRETER_PTR: RefCell<Vec<*mut crate::Interpreter>> =
        const { RefCell::new(Vec::new()) };
}

/// RAII guard that pushes `interp` onto [`CURRENT_INTERPRETER_PTR`]
/// for the lifetime of the guard. Used by VM entry points that
/// might run user code which re-enters the C-API.
#[derive(Debug)]
pub struct InterpreterGuard {
    _private: (),
}

impl Drop for InterpreterGuard {
    fn drop(&mut self) {
        CURRENT_INTERPRETER_PTR.with(|cell| {
            let _ = cell.borrow_mut().pop();
        });
    }
}

thread_local! {
    /// Deferred `ResourceWarning` messages produced by object destructors
    /// (`impl Drop for PyFile`, …). A destructor cannot synthesise a Python
    /// warning *in place*: an `Rc` can hit zero references mid-instruction,
    /// while a container the VM is iterating is still borrowed, so re-entering
    /// `warnings.warn` from `drop` panics with `BorrowMutError`. Instead the
    /// destructor enqueues the message and the eval loop drains it at the same
    /// between-bytecodes safe point it uses for prompt `__del__` finalization
    /// (and `gc.collect()` drains it after a collection), giving CPython's
    /// "unclosed file" warning the right timing without the reentrancy hazard.
    /// Each entry carries the message and, when known, the dying object's
    /// allocation address — the token `warnings.warn(..., source=)` and
    /// `tracemalloc.get_object_traceback` use to look up the allocation
    /// traceback after the object itself is gone.
    static PENDING_RESOURCE_WARNINGS: RefCell<Vec<(String, Option<usize>)>> =
        const { RefCell::new(Vec::new()) };
}

/// Cheap "is the deferred-warning queue non-empty?" probe set whenever a
/// destructor enqueues. A relaxed atomic so the eval-loop safe point pays a
/// single load in the common (empty) case rather than a thread-local borrow.
static PENDING_RW_FLAG: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Enqueue a deferred `ResourceWarning` message from a destructor. Drained by
/// [`crate::Interpreter::drain_pending_resource_warnings`] at the next safe
/// point. Enqueueing stays open during shutdown: CPython *does* report a
/// file leaked until `Py_FinalizeEx`'s module sweep (`<sys>:0:
/// ResourceWarning: unclosed file …`, `test_warnings.test_late_resource_warning`)
/// — the shutdown sequence drains the queue one last time after clearing
/// `__main__`, and anything enqueued later simply evaporates with the process.
pub fn push_pending_resource_warning(message: String) {
    PENDING_RESOURCE_WARNINGS.with(|cell| cell.borrow_mut().push((message, None)));
    PENDING_RW_FLAG.store(true, std::sync::atomic::Ordering::Release);
    crate::hot_gates::set(crate::hot_gates::RESOURCE_WARNINGS);
}

/// As [`push_pending_resource_warning`], carrying the dying object's
/// allocation address as the warning's `source` token (CPython's
/// `PyErr_ResourceWarning(source, …)` passes the object itself; ours is
/// already mid-drop, so the address stands in for the tracemalloc lookup).
/// Pins the object's allocation frames first so a recycled address from
/// `linecache.getline` during warning formatting cannot overwrite them.
pub fn push_pending_resource_warning_with_source(message: String, source_key: usize) {
    crate::stdlib::tracemalloc_real::pin_object_traceback(source_key);
    PENDING_RESOURCE_WARNINGS.with(|cell| cell.borrow_mut().push((message, Some(source_key))));
    PENDING_RW_FLAG.store(true, std::sync::atomic::Ordering::Release);
    crate::hot_gates::set(crate::hot_gates::RESOURCE_WARNINGS);
}

/// Cheap probe for the eval-loop safe point: are any deferred resource
/// warnings queued on this thread?
pub fn has_pending_resource_warnings() -> bool {
    PENDING_RW_FLAG.load(std::sync::atomic::Ordering::Acquire)
}

/// Drain and return all queued deferred resource-warning messages on this
/// thread, clearing the fast-path flag.
pub fn take_pending_resource_warnings() -> Vec<(String, Option<usize>)> {
    // Clear-drain-recheck (RFC 0059 WS2). The RW flag and queue are
    // per-thread; a producer on another thread re-raises the shared bit
    // itself, so no recheck-and-reset pass is needed here.
    crate::hot_gates::clear(crate::hot_gates::RESOURCE_WARNINGS);
    PENDING_RW_FLAG.store(false, std::sync::atomic::Ordering::Release);
    PENDING_RESOURCE_WARNINGS.with(|cell| std::mem::take(&mut *cell.borrow_mut()))
}

thread_local! {
    /// Depth of live VM→C-extension transitions on this thread's stack
    /// (RFC 0047, wave 5). Bumped by `weavepy-capi`'s single bridged-call
    /// choke point (`interp::ensure_active`) for the duration of every C
    /// slot / `PyCFunction` / descriptor invocation, including the
    /// bytecode a C extension re-enters through `PyObject_Call`.
    ///
    /// The prompt reaper consults this to decide whether it may *reclaim*
    /// a refcount-dead subgraph containing C-escaped instances: while any
    /// extension frame is live (`depth > 0`), C code may hold a borrowed
    /// (uncounted) body pointer across its re-entrant call into the VM, so
    /// freeing the body would be a use-after-free; at `depth == 0` the VM
    /// is executing plain bytecode with no extension frame below it, no
    /// borrow can be in flight, and reclaiming is exactly as safe as
    /// CPython's own refcount-driven `tp_dealloc` at the same point.
    static CEXT_CALL_DEPTH: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

thread_local! {
    /// Objects whose last C-side reference was dropped *inside* an
    /// extension call (RFC 0047, wave 5). CPython would run their
    /// `tp_dealloc` chain at that instant — clearing weakrefs of
    /// anything that died with them — but WeavePy's prompt reaper only
    /// fires from eval-loop sites, and a C-internal drop (a Cython
    /// `self.blocks = new` setter decref'ing the old tuple of `Block`s)
    /// leaves the dead objects pinned by their GC handles with their
    /// weakrefs still live. The capi boundary parks the dropped payload
    /// here instead; the eval loop reaps it at the next
    /// between-bytecodes safe point — before any subsequent Python-level
    /// weakref read can observe the stale referent.
    static PENDING_CEXT_DROPS: RefCell<Vec<Object>> = const { RefCell::new(Vec::new()) };
    /// Cheap "is this thread's C-drop queue non-empty?" probe for the
    /// eval-loop safe point. Thread-local (unlike `PENDING_RW_FLAG`)
    /// because the queues are: a global flag cleared by whichever thread
    /// drains first would strand another thread's queued objects with
    /// their weakrefs never cleared.
    static PENDING_CEXT_FLAG: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

thread_local! {
    /// Object ids currently being torn down by the prompt reaper's cascade
    /// on this thread (RFC 0047, wave 5). While an object is mid-cascade,
    /// the teardown itself crosses it back into C — `traverse_object` runs
    /// the GC bridge's `tp_traverse`, which mints a transient self box and
    /// releases it — and that release would *re-queue* the object through
    /// [`queue_cext_dropped`]. The queued clone then makes the cascade's
    /// own deadness re-check see the object as externally alive, aborting
    /// the teardown, and the drained clone starts the cascade over: a
    /// livelock that pinned `repr(DataFrame)` at 100% CPU indefinitely.
    /// A queue request for an id in this set is the cascade observing
    /// itself and is dropped; requests for *other* objects (a child body
    /// whose last C pin fell during the teardown) still queue normally.
    static CASCADING_IDS: RefCell<std::collections::HashSet<u64>> =
        RefCell::new(std::collections::HashSet::new());
}

/// RAII marker for one object's trip through the prompt reaper's cascade;
/// see [`CASCADING_IDS`]. Created by [`enter_cascade`].
#[derive(Debug)]
pub struct CascadeGuard {
    id: u64,
    owner: bool,
}

impl Drop for CascadeGuard {
    fn drop(&mut self) {
        if self.owner {
            let _ = CASCADING_IDS.try_with(|c| {
                if let Ok(mut set) = c.try_borrow_mut() {
                    set.remove(&self.id);
                }
            });
        }
    }
}

/// Mark `id` as mid-cascade for the guard's lifetime. Nesting-safe: a
/// guard for an id already in the set is a no-op on drop (the outer
/// guard owns the entry).
pub fn enter_cascade(id: u64) -> CascadeGuard {
    let owner = CASCADING_IDS
        .try_with(|c| {
            c.try_borrow_mut()
                .map(|mut set| set.insert(id))
                .unwrap_or(false)
        })
        .unwrap_or(false);
    CascadeGuard { id, owner }
}

/// Is `id` currently being torn down by the prompt reaper's cascade on
/// this thread?
fn in_cascade(id: u64) -> bool {
    CASCADING_IDS
        .try_with(|c| c.try_borrow().map(|set| set.contains(&id)).unwrap_or(true))
        .unwrap_or(false)
}

/// Park an object dropped by C extension code for a prompt-reap pass at
/// the next eval-loop safe point. Only object kinds that can carry (or
/// anchor) finalizers/weakrefs/tracked children are queued; scalars and
/// other leaves drop inline as before. Teardown-safe: silently drops the
/// request when thread-local storage is gone.
pub fn queue_cext_dropped(obj: &Object) {
    if !matches!(
        obj,
        Object::Instance(_)
            | Object::List(_)
            | Object::Tuple(_)
            | Object::Dict(_)
            | Object::Set(_)
            | Object::FrozenSet(_)
            | Object::Generator(_)
            | Object::Coroutine(_)
            | Object::AsyncGenerator(_)
    ) {
        return;
    }
    queue_parked_drop(obj);
}

/// Park a value evicted from a native container by a *mutating* method or
/// opcode — `dict.clear`/`__delitem__`/replacing `__setitem__`, `del d[k]`,
/// `list.remove`/`clear`/slice assignment, `set.discard`, … — for a
/// prompt-reap pass at the next eval-loop safe point. These mutators run as
/// plain builtin fns without interpreter access, so the reference they drop
/// can't go through `prompt_reap_dropped` inline; without the park, an
/// object whose *last* reference lived in the container stayed pinned by
/// its weakref registry entry / GC handle until the next cyclic collection.
/// CPython frees it on the spot (pandas' `_item_cache.clear()` relies on
/// the evicted Series — and its CoW `Block` — dying immediately: a stale
/// block kept `refs.has_reference()` true, misfiring the chained-assignment
/// `FutureWarning` in `Series.__setitem__`). Same kind filter as
/// [`queue_cext_dropped`]: scalars and other leaves drop inline as before.
pub fn queue_container_removed(obj: &Object) {
    queue_cext_dropped(obj);
}

/// As [`queue_cext_dropped`] but with **no kind filter** — the entry point
/// for the prompt reaper's escaped-subgraph park (RFC 0047, wave 5). The
/// reaper has already established the object is refcount-dead and anchors
/// an escaped instance, so the park must not lose it: a closure *function*
/// (the compiler's `<genexpr>`/`<listcomp>` temporary, or any `def` whose
/// parameter is promoted to a cell) is GC-tracked yet fell through the
/// C-drop kind filter above, so its park request was silently discarded —
/// leaving it pinned by its own GC handle, still holding `cell(self)`.
/// Every `PyObject_GetAttr(mgr, "shape")` a Cython caller issued leaked
/// one reference to the manager that way (pandas' `shape` getter compiles
/// with `self` as a cellvar), inflating `sys.getrefcount` and keeping CoW
/// intermediates alive until the next full collection.
pub fn queue_parked_drop(obj: &Object) {
    // The cycle collector marshals candidates into transient C boxes for
    // `tp_traverse`/`tp_clear` (gc_bridge); freeing those boxes lands here.
    // Queuing a strong clone of an object *currently under collection*
    // would (a) inflate its externally-visible refcount — the mark phase
    // seeds reachability from `Rc::strong_count`, so each pass would make
    // the candidate look more reachable, pinning cyclic garbage forever —
    // and (b) re-pin objects the collector is about to reclaim. Anything
    // dropped mid-collection is either a candidate (the collector owns its
    // fate) or reachable from one (the next pass sees it); either way the
    // queue adds nothing but the pin.
    if crate::gc_trace::collector_active() {
        return;
    }
    // The prompt reaper's own teardown of this object (see
    // [`CASCADING_IDS`]): the transient C crossings the cascade performs
    // must not re-queue the object it is in the middle of freeing.
    if in_cascade(crate::weakref_registry::id_of(obj)) {
        return;
    }
    let pushed = PENDING_CEXT_DROPS
        .try_with(|cell| {
            if let Ok(mut queue) = cell.try_borrow_mut() {
                queue.push(obj.clone());
                true
            } else {
                false
            }
        })
        .unwrap_or(false);
    if pushed {
        if std::env::var_os("WEAVEPY_REAP_TRACE").is_some() {
            eprintln!(
                "[CEXT-DROP] queued {} id={:#x}",
                obj.type_name_owned(),
                crate::weakref_registry::id_of(obj)
            );
            if std::env::var_os("WEAVEPY_REAP_BT").is_some() {
                thread_local! {
                    static N: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
                }
                let n = N.with(|c| {
                    let v = c.get() + 1;
                    c.set(v);
                    v
                });
                let every: usize = std::env::var("WEAVEPY_REAP_BT_EVERY")
                    .ok()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(5000);
                if n.is_multiple_of(every) {
                    eprintln!(
                        "[CEXT-DROP-BT]\n{}",
                        std::backtrace::Backtrace::force_capture()
                    );
                }
            }
        }
        let _ = PENDING_CEXT_FLAG.try_with(|c| c.set(true));
        PENDING_CEXT_COUNT.fetch_add(1, std::sync::atomic::Ordering::Release);
        crate::hot_gates::set(crate::hot_gates::PENDING_CEXT);
    }
}

/// Process-wide count of parked C-dropped objects (RFC 0058 WS2): the
/// eval loop's per-instruction fast gate, saving the thread-local
/// flag read (and the `cext_call_active` thread-local that follows it)
/// in the common empty case.
static PENDING_CEXT_COUNT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Cheap probe for the eval-loop safe point: are any C-dropped objects
/// awaiting a prompt-reap pass on this thread?
pub fn has_pending_cext_drops() -> bool {
    if PENDING_CEXT_COUNT.load(std::sync::atomic::Ordering::Acquire) == 0 {
        return false;
    }
    PENDING_CEXT_FLAG
        .try_with(std::cell::Cell::get)
        .unwrap_or(false)
}

/// Drain this thread's queue of C-dropped objects.
pub fn drain_pending_cext_drops() -> Vec<Object> {
    // Clear-drain-recheck (RFC 0059 WS2): see `drain_pending_finalizers`.
    crate::hot_gates::clear(crate::hot_gates::PENDING_CEXT);
    let _ = PENDING_CEXT_FLAG.try_with(|c| c.set(false));
    let taken: Vec<Object> = PENDING_CEXT_DROPS
        .try_with(|cell| {
            cell.try_borrow_mut()
                .map(|mut q| std::mem::take(&mut *q))
                .unwrap_or_default()
        })
        .unwrap_or_default();
    if !taken.is_empty() {
        PENDING_CEXT_COUNT.fetch_sub(taken.len(), std::sync::atomic::Ordering::Release);
    }
    if PENDING_CEXT_COUNT.load(std::sync::atomic::Ordering::Acquire) > 0 {
        crate::hot_gates::set(crate::hot_gates::PENDING_CEXT);
    }
    taken
}

/// RAII guard for one VM→C-extension transition; see [`enter_cext_call`].
#[derive(Debug)]
pub struct CextCallGuard(());

impl Drop for CextCallGuard {
    fn drop(&mut self) {
        let _ = CEXT_CALL_DEPTH.try_with(|c| c.set(c.get().saturating_sub(1)));
    }
}

/// Record that a C-extension call is now live on this thread's stack.
/// The returned guard decrements the depth when dropped.
pub fn enter_cext_call() -> CextCallGuard {
    let _ = CEXT_CALL_DEPTH.try_with(|c| c.set(c.get() + 1));
    CextCallGuard(())
}

/// Is any C-extension call live on this thread's stack? `false` means
/// the VM is executing plain bytecode and no extension can hold a
/// borrowed pointer across the current instruction.
pub fn cext_call_active() -> bool {
    CEXT_CALL_DEPTH.try_with(|c| c.get() > 0).unwrap_or(true)
}

/// `True` once interpreter shutdown (finalizer sweep) has begun —
/// CPython's `_Py_IsFinalizing()`. Fresh imports are refused while
/// set (already-imported modules keep working), and
/// `sys.is_finalizing()` reads it.
static FINALIZING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn set_finalizing(value: bool) {
    FINALIZING.store(value, std::sync::atomic::Ordering::Release);
    // Level-style hot-gate bit (RFC 0059 WS2): maintained on the state
    // transition itself, so the dispatch loop's daemon-kill probe is
    // covered by the single fused load.
    if value {
        crate::hot_gates::set(crate::hot_gates::FINALIZING);
    } else {
        crate::hot_gates::clear(crate::hot_gates::FINALIZING);
    }
}

pub fn is_finalizing() -> bool {
    FINALIZING.load(std::sync::atomic::Ordering::Acquire)
}

/// PEP 657 column info enabled? Cleared by `-X no_debug_ranges` /
/// `PYTHONNODEBUGRANGES`; `co_positions()` then reports `None`
/// columns and traceback carets disappear, like CPython.
static DEBUG_RANGES: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

pub fn set_debug_ranges(value: bool) {
    DEBUG_RANGES.store(value, std::sync::atomic::Ordering::Release);
}

pub fn debug_ranges() -> bool {
    DEBUG_RANGES.load(std::sync::atomic::Ordering::Acquire)
}

/// `-X dev` / `PYTHONDEVMODE`. Dev mode turns on eager validation
/// that CPython otherwise defers (e.g. `bytes(s, encoding, errors=…)`
/// looks up the error handler immediately; bpo-37388).
static DEV_MODE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn set_dev_mode(value: bool) {
    DEV_MODE.store(value, std::sync::atomic::Ordering::Release);
}

pub fn dev_mode() -> bool {
    DEV_MODE.load(std::sync::atomic::Ordering::Acquire)
}

/// Raw OS bytes for the (astronomically rare) argv elements whose
/// decoded text lands in the plane-16 PUA bridge window
/// (U+10F800..U+10FFFF): a *genuine* such character is
/// indistinguishable in the bridged-`String` transport from an escaped
/// lone surrogate, so `os_args_bridged` records the original bytes
/// here and `argv_str_to_object` decodes from them instead
/// (`test_cmd_line.test_osx_android_utf8` passes a real U+10FFFF).
static RAW_ARGS: std::sync::Mutex<Vec<(String, Vec<u8>)>> = std::sync::Mutex::new(Vec::new());

pub fn register_raw_arg(transport: String, bytes: Vec<u8>) {
    if let Ok(mut v) = RAW_ARGS.lock() {
        if !v.iter().any(|(t, _)| *t == transport) {
            v.push((transport, bytes));
        }
    }
}

pub fn raw_arg_bytes(transport: &str) -> Option<Vec<u8>> {
    RAW_ARGS
        .lock()
        .ok()?
        .iter()
        .find(|(t, _)| t == transport)
        .map(|(_, b)| b.clone())
}

/// `-u` / `PYTHONUNBUFFERED`: standard-stream writes are pushed to the
/// descriptor immediately (and `sys.stdout.write_through` reports True).
static STDIO_UNBUFFERED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn set_stdio_unbuffered(value: bool) {
    STDIO_UNBUFFERED.store(value, std::sync::atomic::Ordering::Release);
}

pub fn stdio_unbuffered() -> bool {
    STDIO_UNBUFFERED.load(std::sync::atomic::Ordering::Acquire)
}

/// Whether the process-global stdout buffer (Rust's line-buffered
/// `std::io::Stdout`) may be holding unflushed bytes: true after a write
/// whose cumulative stream doesn't end in a newline. Needed because
/// Rust's stdout deliberately *swallows* `EBADF` (`handle_ebadf` in
/// std), so a shutdown flush to a closed fd 1 reports success while
/// dropping data — CPython exits 120 with "Exception ignored on
/// flushing sys.stdout" instead (`test_cmd_line.test_stdout_flush_at_shutdown`).
static STDOUT_TAIL_PENDING: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

pub fn note_stdout_write(chunk: &[u8]) {
    if let Some(last) = chunk.last() {
        STDOUT_TAIL_PENDING.store(*last != b'\n', std::sync::atomic::Ordering::Release);
    }
}

/// Read-and-clear the pending flag (the caller is about to flush).
pub fn take_stdout_pending() -> bool {
    STDOUT_TAIL_PENDING.swap(false, std::sync::atomic::Ordering::AcqRel)
}

/// `-X cpu_count=N` / `PYTHON_CPU_COUNT` (gh-109595): overrides what
/// `os.cpu_count()` / `os.process_cpu_count()` report. `0` = no override.
static CPU_COUNT_OVERRIDE: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);

pub fn set_cpu_count_override(value: i64) {
    CPU_COUNT_OVERRIDE.store(value, std::sync::atomic::Ordering::Release);
}

pub fn cpu_count_override() -> Option<i64> {
    match CPU_COUNT_OVERRIDE.load(std::sync::atomic::Ordering::Acquire) {
        0 => None,
        n => Some(n),
    }
}

/// PEP 540 UTF-8 mode. WeavePy stores `str` as UTF-8 so this defaults to
/// `true`; the CLI lowers it for `-X utf8=0` (read by `io.text_encoding`).
static UTF8_MODE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

pub fn set_utf8_mode(value: bool) {
    UTF8_MODE.store(value, std::sync::atomic::Ordering::Release);
}

pub fn utf8_mode() -> bool {
    UTF8_MODE.load(std::sync::atomic::Ordering::Acquire)
}

/// PEP 597 `-X warn_default_encoding` / `PYTHONWARNDEFAULTENCODING`. When set,
/// the native `io.open` / `io.text_encoding` text paths emit an
/// `EncodingWarning` for an implicit (locale) encoding, mirroring CPython's
/// `_PyInterpreterState_GetConfig(interp)->warn_default_encoding` gate. Cached
/// here so Rust call sites avoid reading `sys.flags` on every open.
static WARN_DEFAULT_ENCODING: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

pub fn set_warn_default_encoding(value: bool) {
    WARN_DEFAULT_ENCODING.store(value, std::sync::atomic::Ordering::Release);
}

pub fn warn_default_encoding() -> bool {
    WARN_DEFAULT_ENCODING.load(std::sync::atomic::Ordering::Acquire)
}

/// Publish `interp` as the live VM pointer for the duration of
/// the returned guard. Re-entrant calls produce a stack so the
/// most recent guard wins on `current_interpreter_ptr` lookups.
pub fn publish_interpreter_ptr(interp: *mut crate::Interpreter) -> InterpreterGuard {
    CURRENT_INTERPRETER_PTR.with(|cell| cell.borrow_mut().push(interp));
    InterpreterGuard { _private: () }
}

/// Read the most recently published interpreter pointer, or
/// `None` if no VM entry frame is on this thread.
pub fn current_interpreter_ptr() -> Option<*mut crate::Interpreter> {
    CURRENT_INTERPRETER_PTR.with(|cell| cell.borrow().last().copied())
}

/// `quit` and `exit` — interactive sentinels that raise `SystemExit`.
pub fn quitter(name: &'static str) -> Object {
    use crate::object::BuiltinFn;
    let f = BuiltinFn {
        name,
        binds_instance: false,
        call: Box::new(|args: &[Object]| {
            let code = args.first().cloned().unwrap_or(Object::None);
            let bt = crate::builtin_types::builtin_types();
            let inst = crate::builtin_types::make_exception_with_class(
                bt.system_exit.clone(),
                code.to_str(),
            );
            if let Object::Instance(inst_rc) = &inst {
                inst_rc.slot_set("code", code.clone());
                inst_rc.slot_set("args", Object::new_tuple(vec![code]));
            }
            Err(crate::error::RuntimeError::PyException(
                crate::error::PyException::new(inst),
            ))
        }),
        call_kw: None,
    };
    Object::Builtin(Rc::new(f))
}

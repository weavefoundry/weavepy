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

thread_local! {
    static NOT_IMPLEMENTED: RefCell<Option<Object>> = const { RefCell::new(None) };
    static ELLIPSIS: RefCell<Option<Object>> = const { RefCell::new(None) };
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

/// Push an instance whose `__del__` should run at the next safe
/// point. Called by the cycle GC during its clear phase.
pub fn push_pending_finalizer(obj: Object) {
    PENDING_FINALIZERS.with(|cell| {
        cell.borrow_mut().push(obj);
    });
}

/// Like [`push_pending_finalizer`], but callable from `Drop` impls:
/// tolerates thread-teardown (destroyed TLS) and re-entrant borrows
/// by silently dropping the request.
pub fn try_push_pending_finalizer(obj: Object) {
    let _ = PENDING_FINALIZERS.try_with(|cell| {
        if let Ok(mut queue) = cell.try_borrow_mut() {
            queue.push(obj);
        }
    });
}

/// Drain the pending-finalizer queue. The eval loop calls this
/// at every eval-breaker tick that has the GC flag set.
pub fn drain_pending_finalizers() -> Vec<Object> {
    PENDING_FINALIZERS.with(|cell| std::mem::take(&mut *cell.borrow_mut()))
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
    NOT_IMPLEMENTED.with(|slot| {
        let mut s = slot.borrow_mut();
        if let Some(v) = s.as_ref() {
            return v.clone();
        }
        let cls = crate::builtin_types::builtin_types()
            .not_implemented_type_
            .clone();
        let v = make_singleton(cls);
        *s = Some(v.clone());
        v
    })
}

/// Same idea for `Ellipsis` (the value of `...`); its class is the
/// registry's `ellipsis` type.
pub fn ellipsis() -> Object {
    ELLIPSIS.with(|slot| {
        let mut s = slot.borrow_mut();
        if let Some(v) = s.as_ref() {
            return v.clone();
        }
        let cls = crate::builtin_types::builtin_types().ellipsis_.clone();
        let v = make_singleton(cls);
        *s = Some(v.clone());
        v
    })
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
static SEED_BUILTIN_TYPES: OnceLock<Mutex<Option<crate::sync::Rc<crate::builtin_types::BuiltinTypes>>>> =
    OnceLock::new();

fn seed_slot() -> &'static Mutex<Option<crate::Interpreter>> {
    INTERPRETER_SEED.get_or_init(|| Mutex::new(None))
}

fn seed_types_slot(
) -> &'static Mutex<Option<crate::sync::Rc<crate::builtin_types::BuiltinTypes>>> {
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
    pub frame_stack: Rc<RefCell<Vec<Rc<crate::object::PyFrame>>>>,
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

/// `True` once interpreter shutdown (finalizer sweep) has begun —
/// CPython's `_Py_IsFinalizing()`. Fresh imports are refused while
/// set (already-imported modules keep working), and
/// `sys.is_finalizing()` reads it.
static FINALIZING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn set_finalizing(value: bool) {
    FINALIZING.store(value, std::sync::atomic::Ordering::Release);
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
                inst_rc.dict.borrow_mut().insert(
                    crate::object::DictKey(Object::from_static("code")),
                    code.clone(),
                );
                inst_rc.dict.borrow_mut().insert(
                    crate::object::DictKey(Object::from_static("args")),
                    Object::new_tuple(vec![code]),
                );
            }
            Err(crate::error::RuntimeError::PyException(
                crate::error::PyException::new(inst),
            ))
        }),
        call_kw: None,
    };
    Object::Builtin(Rc::new(f))
}

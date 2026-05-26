//! Bridge between the C-API and the running WeavePy interpreter.
//!
//! When the VM dispatches into a C function it briefly publishes a
//! pointer to itself plus the callee's globals/builtins via a thread-
//! local cell. The C-API helpers ([`crate::abstract_::PyObject_Call`],
//! [`crate::module::PyImport_ImportModule`], etc.) read from that
//! cell to find the live interpreter.
//!
//! Calls *into* the C extension always run on the same OS thread
//! that owns the interpreter (WeavePy is single-threaded today),
//! so the cell can be a plain `RefCell<Option<…>>`.

use std::sync::atomic::{AtomicPtr, Ordering};
use std::sync::Once;
use weavepy_vm::sync::Rc;
use weavepy_vm::sync::RefCell;

use weavepy_vm::object::{DictData, Object};
use weavepy_vm::Interpreter;

thread_local! {
    /// The interpreter currently executing C code. `None` when no
    /// extension call is on the stack.
    static ACTIVE: RefCell<Option<ActiveContext>> = const { RefCell::new(None) };
}

/// Snapshot of "what's running right now" — the live interpreter,
/// the globals dict the active call site is binding into, and the
/// extension module that was the target of the import. The C-API
/// helpers consult this to (a) call back into the VM and (b)
/// publish module-level state.
pub struct ActiveContext {
    pub interp: *mut Interpreter,
    pub globals: Option<Rc<weavepy_vm::sync::RefCell<DictData>>>,
    pub current_module: Option<Object>,
}

/// Push an active context for the duration of `body`. Also caches
/// the live interpreter pointer in [`LAST_INTERPRETER`] so post-
/// extension callbacks (dunder shims, class methods) can find a
/// VM even after `body` returns.
pub fn with_active<R>(ctx: ActiveContext, body: impl FnOnce() -> R) -> R {
    if !ctx.interp.is_null() {
        LAST_INTERPRETER.store(ctx.interp, Ordering::SeqCst);
    }
    ACTIVE.with(|cell| {
        let prev = cell.borrow_mut().replace(ctx);
        let out = body();
        *cell.borrow_mut() = prev;
        out
    })
}

/// Read the active context. Returns `None` when no extension call
/// is on the stack.
pub fn with_current<R>(f: impl FnOnce(&ActiveContext) -> R) -> Option<R> {
    ACTIVE.with(|cell| cell.borrow().as_ref().map(f))
}

/// Live interpreter pointer if any.
pub fn current_interpreter_mut() -> Option<*mut Interpreter> {
    ACTIVE.with(|cell| cell.borrow().as_ref().map(|c| c.interp))
}

/// Run a closure with a `&mut Interpreter` borrow if a context is
/// active, returning `None` otherwise.
pub fn with_interp_mut<R>(f: impl FnOnce(&mut Interpreter) -> R) -> Option<R> {
    let p = effective_interpreter_mut()?;
    if p.is_null() {
        return None;
    }
    Some(f(unsafe { &mut *p }))
}

/// "Last known" interpreter pointer. Updated whenever an
/// extension call sets up an active context, and consulted by
/// re-entrant C-API calls (dunder shims, method wrappers) that
/// happen *after* the original entry point unwound — at that point
/// the ACTIVE thread-local has been cleared but we still need to
/// route `PyObject_CallObject(cls, …)` back into the VM.
static LAST_INTERPRETER: AtomicPtr<Interpreter> = AtomicPtr::new(std::ptr::null_mut());

/// Resolve the most relevant interpreter pointer:
///   1. The currently-active extension context, if any.
///   2. The interpreter on the VM's `publish_interpreter_ptr` stack
///      (set on every `call_object` / `iter_object` /
///      `iter_next_object` entry — guaranteed to be live for the
///      duration of that call).
///   3. Otherwise the most recently seen interpreter from any
///      `with_active` call on this process. (Fallback for legacy
///      paths; the pointer here may be stale if the owning frame
///      has unwound, so it's only consulted as a last resort.)
pub fn effective_interpreter_mut() -> Option<*mut Interpreter> {
    if let Some(p) = current_interpreter_mut() {
        if !p.is_null() {
            return Some(p);
        }
    }
    if let Some(p) = weavepy_vm::vm_singletons::current_interpreter_ptr() {
        if !p.is_null() {
            return Some(p);
        }
    }
    let last = LAST_INTERPRETER.load(Ordering::SeqCst);
    if last.is_null() {
        None
    } else {
        Some(last)
    }
}

/// If no active context is on the stack, push a temporary one
/// pointing at the most recent known interpreter. Otherwise run
/// `body` directly. Used by dunder shims and class-method wrappers
/// so re-entrant `PyObject_CallObject` calls find a live VM even
/// when the original entry point left no ACTIVE behind.
///
/// Resolves the interpreter pointer using the same priority as
/// [`effective_interpreter_mut`]: VM-published first (always
/// live for the duration of the call), then `LAST_INTERPRETER`
/// (best-effort, may be stale).
pub fn ensure_active<R>(body: impl FnOnce() -> R) -> R {
    if current_interpreter_mut().is_some() {
        return body();
    }
    let interp = if let Some(p) = weavepy_vm::vm_singletons::current_interpreter_ptr() {
        p
    } else {
        let last = LAST_INTERPRETER.load(Ordering::SeqCst);
        if last.is_null() {
            return body();
        }
        last
    };
    let ctx = ActiveContext {
        interp,
        globals: None,
        current_module: None,
    };
    with_active(ctx, body)
}

static INIT: Once = Once::new();

/// Initialise everything that needs to be live before any
/// extension code runs: static type bridges, singleton `ob_type`s,
/// statically-initialised exception pointers.
pub fn ensure_initialised() {
    INIT.call_once(|| {
        crate::types::init_static_types();
        crate::singletons::init_singleton_types(
            crate::types::_PyNone_Type.as_ptr(),
            crate::types::PyBool_Type.as_ptr(),
            crate::types::_PyNotImplemented_Type.as_ptr(),
            crate::types::PyEllipsis_Type.as_ptr(),
        );
        crate::errors::init_static_exceptions();
    });
}

/// Execute `body` with `ctx` active and clear the per-thread
/// "current exception" cell on entry. Mirrors the CPython
/// invariant that a fresh C call sees no pending error.
pub fn enter_extension_call<R>(ctx: ActiveContext, body: impl FnOnce() -> R) -> R {
    ensure_initialised();
    crate::errors::clear_thread_local();
    with_active(ctx, body)
}

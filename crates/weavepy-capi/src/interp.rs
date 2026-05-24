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

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Once;

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
    pub globals: Option<Rc<std::cell::RefCell<DictData>>>,
    pub current_module: Option<Object>,
}

/// Push an active context for the duration of `body`.
pub fn with_active<R>(ctx: ActiveContext, body: impl FnOnce() -> R) -> R {
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
    let p = current_interpreter_mut()?;
    if p.is_null() {
        return None;
    }
    Some(f(unsafe { &mut *p }))
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

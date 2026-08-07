//! `_asyncio` — CPython's asyncio C accelerator, implemented natively
//! (RFC 0054 WS1).
//!
//! The frozen `asyncio/futures.py`, `asyncio/tasks.py`, and
//! `asyncio/events.py` are verbatim CPython 3.13 and carry the standard
//! adoption hooks (`try: from _asyncio import …`). This module makes
//! those hooks bind, exactly as they do on CPython:
//!
//! - `Future` / `Task` become native types (`asyncio.Future is
//!   _asyncio.Future`), with the C implementation's observable
//!   semantics: the two-step `__await__` iterator protocol, eager task
//!   start (`eager_start=True`, the contract behind
//!   `asyncio.eager_task_factory`), `cancelling()`/`uncancel()`
//!   bookkeeping, and `_make_cancelled_error` chaining.
//! - The per-thread running-loop slot (`_get_running_loop` /
//!   `_set_running_loop` / `get_running_loop` / `get_event_loop`)
//!   lives in Rust.
//! - The task registries (`_scheduled_tasks` WeakSet, `_eager_tasks`
//!   set, `_current_tasks` dict) are module attributes shared with the
//!   pure-Python `tasks.py` helpers, so `asyncio.all_tasks()` sees
//!   native and duck-typed tasks alike.
//!
//! ## Storage
//!
//! Future/Task state lives in a process-global registry keyed by an
//! integer handle stored on the instance dict (the `socket_mod`
//! pattern; `Rc`/`RefCell` alias `Arc`/`GilCell` under RFC 0025, so
//! the cells are Send + Sync and the GIL serialises access). All
//! re-entry into Python (loop.call_soon, coro.send, callbacks) happens
//! with no state borrow held.

use crate::sync::Rc;
use crate::sync::RefCell;
use std::collections::HashMap;

use crate::error::{type_error, value_error, PyException, RuntimeError};
use crate::import::ModuleCache;
use crate::object::{BoundMethod, BuiltinFn, DictData, DictKey, Object, PyModule, PyProperty};
use crate::types::{PyInstance, TypeObject};

// ---- interpreter re-entry ----

type Interp = crate::Interpreter;

fn interp<'a>() -> Result<&'a mut Interp, RuntimeError> {
    let ptr = crate::vm_singletons::current_interpreter_ptr()
        .ok_or_else(|| RuntimeError::Internal("_asyncio: no running interpreter".to_owned()))?;
    // SAFETY: published by an enclosing VM frame still live on this thread;
    // the GIL keeps the access exclusive.
    Ok(unsafe { &mut *ptr })
}

fn call(interp: &mut Interp, f: &Object, args: &[Object]) -> Result<Object, RuntimeError> {
    let globals = interp.builtins_dict();
    interp.call_object_with_globals(f, args, &[], &globals)
}

fn call_kw(
    interp: &mut Interp,
    f: &Object,
    args: &[Object],
    kwargs: &[(String, Object)],
) -> Result<Object, RuntimeError> {
    let globals = interp.builtins_dict();
    interp.call_object_with_globals(f, args, kwargs, &globals)
}

fn call_method(
    interp: &mut Interp,
    obj: &Object,
    name: &str,
    args: &[Object],
) -> Result<Object, RuntimeError> {
    let m = interp.load_attr_public(obj, name)?;
    call(interp, &m, args)
}

fn call_method_kw(
    interp: &mut Interp,
    obj: &Object,
    name: &str,
    args: &[Object],
    kwargs: &[(String, Object)],
) -> Result<Object, RuntimeError> {
    let m = interp.load_attr_public(obj, name)?;
    call_kw(interp, &m, args, kwargs)
}

/// `__import__(name, fromlist=['*'])` — returns the *leaf* module.
fn import_module(interp: &mut Interp, name: &str) -> Result<Object, RuntimeError> {
    let import_fn = interp
        .builtins_dict()
        .borrow()
        .get(&DictKey(Object::from_static("__import__")))
        .cloned()
        .ok_or_else(|| RuntimeError::Internal("_asyncio: no __import__".to_owned()))?;
    call(
        interp,
        &import_fn,
        &[
            Object::from_str(name),
            Object::None,
            Object::None,
            Object::new_list(vec![Object::from_static("*")]),
        ],
    )
}

fn import_attr(interp: &mut Interp, module: &str, attr: &str) -> Result<Object, RuntimeError> {
    let m = import_module(interp, module)?;
    interp.load_attr_public(&m, attr)
}

fn py_repr(interp: &mut Interp, obj: &Object) -> String {
    let repr_fn = interp
        .builtins_dict()
        .borrow()
        .get(&DictKey(Object::from_static("repr")))
        .cloned();
    match repr_fn {
        Some(f) => match call(interp, &f, std::slice::from_ref(obj)) {
            Ok(s) => s.to_str().clone(),
            Err(_) => format!("<{}>", obj.type_name()),
        },
        None => format!("<{}>", obj.type_name()),
    }
}

fn py_setattr(
    interp: &mut Interp,
    obj: &Object,
    name: &str,
    value: Object,
) -> Result<(), RuntimeError> {
    let setattr_fn = interp
        .builtins_dict()
        .borrow()
        .get(&DictKey(Object::from_static("setattr")))
        .cloned()
        .ok_or_else(|| RuntimeError::Internal("_asyncio: no setattr".to_owned()))?;
    call(
        interp,
        &setattr_fn,
        &[obj.clone(), Object::from_str(name), value],
    )?;
    Ok(())
}

fn truthy(obj: &Object) -> bool {
    !matches!(
        obj,
        Object::None | Object::Bool(false) | Object::Int(0) | Object::Unbound
    )
}

fn same_object(a: &Object, b: &Object) -> bool {
    a.is_same(b)
}

/// Build an exception instance of an `asyncio.exceptions` class.
fn asyncio_exc_instance(
    interp: &mut Interp,
    name: &str,
    args: &[Object],
) -> Result<Object, RuntimeError> {
    let cls = import_attr(interp, "asyncio.exceptions", name)?;
    call(interp, &cls, args)
}

fn raise_asyncio(interp: &mut Interp, name: &str, msg: &str) -> RuntimeError {
    match asyncio_exc_instance(interp, name, &[Object::from_str(msg)]) {
        Ok(inst) => RuntimeError::PyException(PyException::new(inst)),
        Err(e) => e,
    }
}

fn runtime_err(msg: impl Into<String>) -> RuntimeError {
    RuntimeError::PyException(PyException::new(crate::builtin_types::make_exception(
        "RuntimeError",
        msg.into(),
    )))
}

fn is_instance_of_named(interp: &mut Interp, obj: &Object, module: &str, name: &str) -> bool {
    let Object::Instance(inst) = obj else {
        return false;
    };
    let Ok(Object::Type(cls)) = import_attr(interp, module, name) else {
        return false;
    };
    inst.cls().is_subclass_of(&cls)
}

fn is_builtin_exc(obj: &Object, name: &str) -> bool {
    let Object::Instance(inst) = obj else {
        return false;
    };
    let bt = crate::builtin_types::builtin_types();
    match bt.by_name(name) {
        Some(cls) => inst.cls().is_subclass_of(&cls),
        None => false,
    }
}

// ---- state registry ----

#[derive(Clone, Copy, PartialEq, Eq)]
enum FutStatus {
    Pending,
    Cancelled,
    Finished,
}

impl FutStatus {
    fn as_str(self) -> &'static str {
        match self {
            FutStatus::Pending => "PENDING",
            FutStatus::Cancelled => "CANCELLED",
            FutStatus::Finished => "FINISHED",
        }
    }
}

struct FutState {
    status: FutStatus,
    result: Object,
    exception: Object,
    /// `exception.__traceback__` as of `set_exception` — restored on every
    /// raise so re-awaiting the same future doesn't accumulate awaiter
    /// frames (CPython's `fut_exception_tb`, test_futures2).
    exception_tb: Object,
    cancelled_exc: Object,
    loop_: Object,
    callbacks: Vec<(Object, Object)>,
    cancel_message: Object,
    log_traceback: bool,
    blocking: bool,
    source_traceback: Object,
    initialized: bool,
    // Task-only fields.
    coro: Object,
    name: Object,
    context: Object,
    must_cancel: bool,
    fut_waiter: Object,
    num_cancels_requested: i64,
    log_destroy_pending: bool,
}

impl Default for FutState {
    fn default() -> Self {
        Self {
            status: FutStatus::Pending,
            result: Object::None,
            exception: Object::None,
            exception_tb: Object::None,
            cancelled_exc: Object::None,
            loop_: Object::None,
            callbacks: Vec::new(),
            cancel_message: Object::None,
            log_traceback: false,
            blocking: false,
            source_traceback: Object::None,
            initialized: false,
            coro: Object::None,
            name: Object::None,
            context: Object::None,
            must_cancel: false,
            fut_waiter: Object::None,
            num_cancels_requested: 0,
            log_destroy_pending: true,
        }
    }
}

fn registry() -> &'static parking_lot::Mutex<HashMap<i64, Rc<RefCell<FutState>>>> {
    static REGISTRY: std::sync::OnceLock<parking_lot::Mutex<HashMap<i64, Rc<RefCell<FutState>>>>> =
        std::sync::OnceLock::new();
    REGISTRY.get_or_init(|| parking_lot::Mutex::new(HashMap::new()))
}

const HANDLE_KEY: &str = "__weavepy_fut_handle__";

fn attach_state(inst: &Rc<PyInstance>) -> Rc<RefCell<FutState>> {
    use std::sync::atomic::{AtomicI64, Ordering};
    static NEXT: AtomicI64 = AtomicI64::new(1);
    let handle = NEXT.fetch_add(1, Ordering::Relaxed);
    let st = Rc::new(RefCell::new(FutState::default()));
    registry().lock().insert(handle, st.clone());
    inst.dict.borrow_mut().insert(
        DictKey(Object::from_static(HANDLE_KEY)),
        Object::Int(handle),
    );
    st
}

/// The already-attached state cell of a Future/Task instance, without
/// creating one. Safe to call from GC callbacks.
fn existing_state_of(inst: &PyInstance) -> Option<Rc<RefCell<FutState>>> {
    let existing = inst
        .dict
        .try_borrow()
        .ok()?
        .get(&DictKey(Object::from_static(HANDLE_KEY)))
        .cloned();
    if let Some(Object::Int(h)) = existing {
        return registry().lock().get(&h).cloned();
    }
    None
}

/// The state cell for a Future/Task instance, creating a default
/// (uninitialized) one on first touch — `Future.__new__(Future)` without
/// `__init__` still supports `repr()`, `done()`, `_loop`, … (CPython's
/// "not initialized" surface).
fn state_of_instance(inst: &Rc<PyInstance>) -> Rc<RefCell<FutState>> {
    if let Some(st) = existing_state_of(inst) {
        return st;
    }
    attach_state(inst)
}

fn future_self(args: &[Object]) -> Result<Rc<PyInstance>, RuntimeError> {
    match args.first() {
        Some(Object::Instance(inst)) => Ok(inst.clone()),
        _ => Err(type_error("Future method requires a Future instance")),
    }
}

fn state_of(args: &[Object]) -> Result<(Rc<PyInstance>, Rc<RefCell<FutState>>), RuntimeError> {
    let inst = future_self(args)?;
    let st = state_of_instance(&inst);
    Ok((inst, st))
}

fn state_of_obj(obj: &Object) -> Result<Rc<RefCell<FutState>>, RuntimeError> {
    match obj {
        Object::Instance(inst) => Ok(state_of_instance(inst)),
        _ => Err(type_error("expected a Future instance")),
    }
}

/// Drop the registry entry when the interpreter tears the instance down.
/// The instance dict entry keeps the handle alive; without an explicit
/// drop hook we accept the state living until process exit for leaked
/// futures — bounded by the same lifetime CPython gives them.
fn release_state(inst: &Rc<PyInstance>) {
    let existing = inst
        .dict
        .borrow()
        .get(&DictKey(Object::from_static(HANDLE_KEY)))
        .cloned();
    if let Some(Object::Int(h)) = existing {
        registry().lock().remove(&h);
    }
}

// ---- cycle-collector integration (RFC 0044 pattern) ----
//
// The state cell lives in a module-private registry the collector cannot
// see; without traverse/clear hooks a `task → coro frame → future →
// wakeup-callback → task` loop (CPython's canonical asyncio cycle,
// test_log_destroyed_pending_task) is pinned forever. The hooks mirror
// `FutureObj_traverse`/`FutureObj_clear` in CPython's _asynciomodule.c.

fn fut_gc_matches(obj: &Object) -> bool {
    match obj {
        Object::Instance(i) => i
            .dict
            .try_borrow()
            .is_ok_and(|d| d.get(&DictKey(Object::from_static(HANDLE_KEY))).is_some()),
        _ => false,
    }
}

fn fut_gc_traverse(obj: &Object, visit: &mut dyn FnMut(&Object)) {
    let Object::Instance(inst) = obj else { return };
    let Some(st) = existing_state_of(inst) else {
        return;
    };
    let Ok(s) = st.try_borrow() else { return };
    for o in [
        &s.result,
        &s.exception,
        &s.exception_tb,
        &s.cancelled_exc,
        &s.loop_,
        &s.cancel_message,
        &s.source_traceback,
        &s.coro,
        &s.name,
        &s.context,
        &s.fut_waiter,
    ] {
        visit(o);
    }
    for (cb, ctx) in &s.callbacks {
        visit(cb);
        visit(ctx);
    }
}

fn fut_gc_clear(obj: &Object) {
    let Object::Instance(inst) = obj else { return };
    let Some(st) = existing_state_of(inst) else {
        return;
    };
    // Move the children out under the borrow, drop them after releasing
    // it — their drops can re-enter arbitrary code.
    let mut dropped: Vec<Object> = Vec::new();
    if let Ok(mut s) = st.try_borrow_mut() {
        let s = &mut *s;
        let fields: [&mut Object; 11] = [
            &mut s.result,
            &mut s.exception,
            &mut s.exception_tb,
            &mut s.cancelled_exc,
            &mut s.loop_,
            &mut s.cancel_message,
            &mut s.source_traceback,
            &mut s.coro,
            &mut s.name,
            &mut s.context,
            &mut s.fut_waiter,
        ];
        for o in fields {
            dropped.push(std::mem::replace(o, Object::None));
        }
        for (cb, ctx) in s.callbacks.drain(..) {
            dropped.push(cb);
            dropped.push(ctx);
        }
    }
    // The registry entry itself dies too — the instance dict (and with it
    // the handle) is cleared right after this hook runs.
    release_state(inst);
    drop(dropped);
}

fn register_gc_hooks() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        crate::gc_trace::register_traverse(fut_gc_matches, fut_gc_traverse);
        crate::gc_trace::register_clear(fut_gc_matches, fut_gc_clear);
    });
}

// ---- running-loop slot (per OS thread) ----

thread_local! {
    static RUNNING_LOOP: RefCell<Object> = const { RefCell::new(Object::None) };
}

fn running_loop() -> Object {
    RUNNING_LOOP.with(|slot| slot.borrow().clone())
}

fn set_running_loop(loop_: Object) {
    RUNNING_LOOP.with(|slot| *slot.borrow_mut() = loop_);
}

// ---- task registries (shared with tasks.py) ----

fn scheduled_tasks_obj() -> &'static std::sync::OnceLock<Object> {
    static CELL: std::sync::OnceLock<Object> = std::sync::OnceLock::new();
    &CELL
}

fn eager_tasks_obj() -> &'static std::sync::OnceLock<Object> {
    static CELL: std::sync::OnceLock<Object> = std::sync::OnceLock::new();
    &CELL
}

fn current_tasks_dict() -> Rc<RefCell<DictData>> {
    static CELL: std::sync::OnceLock<Rc<RefCell<DictData>>> = std::sync::OnceLock::new();
    CELL.get_or_init(|| Rc::new(RefCell::new(DictData::default())))
        .clone()
}

fn register_task_impl(interp: &mut Interp, task: &Object) -> Result<(), RuntimeError> {
    if let Some(ws) = scheduled_tasks_obj().get() {
        call_method(interp, &ws.clone(), "add", std::slice::from_ref(task))?;
    }
    Ok(())
}

fn unregister_task_impl(interp: &mut Interp, task: &Object) -> Result<(), RuntimeError> {
    if let Some(ws) = scheduled_tasks_obj().get() {
        call_method(interp, &ws.clone(), "discard", std::slice::from_ref(task))?;
    }
    Ok(())
}

fn register_eager_task_impl(interp: &mut Interp, task: &Object) -> Result<(), RuntimeError> {
    if let Some(s) = eager_tasks_obj().get() {
        call_method(interp, &s.clone(), "add", std::slice::from_ref(task))?;
    }
    Ok(())
}

fn unregister_eager_task_impl(interp: &mut Interp, task: &Object) -> Result<(), RuntimeError> {
    if let Some(s) = eager_tasks_obj().get() {
        call_method(interp, &s.clone(), "discard", std::slice::from_ref(task))?;
    }
    Ok(())
}

fn enter_task_impl(interp: &mut Interp, loop_: &Object, task: &Object) -> Result<(), RuntimeError> {
    let dict = current_tasks_dict();
    let key = DictKey(loop_.clone());
    let existing = dict.borrow().get(&key).cloned();
    if let Some(cur) = existing {
        if !matches!(cur, Object::None) {
            let task_r = py_repr(interp, task);
            let cur_r = py_repr(interp, &cur);
            return Err(runtime_err(format!(
                "Cannot enter into task {task_r} while another task {cur_r} is being executed."
            )));
        }
    }
    dict.borrow_mut().insert(key, task.clone());
    Ok(())
}

fn leave_task_impl(interp: &mut Interp, loop_: &Object, task: &Object) -> Result<(), RuntimeError> {
    let dict = current_tasks_dict();
    let key = DictKey(loop_.clone());
    let existing = dict.borrow().get(&key).cloned();
    match existing {
        Some(cur) if same_object(&cur, task) => {
            dict.borrow_mut().shift_remove(&key);
            Ok(())
        }
        other => {
            let task_r = py_repr(interp, task);
            let cur_r = py_repr(interp, &other.unwrap_or(Object::None));
            Err(runtime_err(format!(
                "Leaving task {task_r} does not match the current task {cur_r}."
            )))
        }
    }
}

fn swap_current_task_impl(loop_: &Object, task: &Object) -> Object {
    let dict = current_tasks_dict();
    let key = DictKey(loop_.clone());
    let prev = dict.borrow().get(&key).cloned().unwrap_or(Object::None);
    if matches!(task, Object::None) {
        dict.borrow_mut().shift_remove(&key);
    } else {
        dict.borrow_mut().insert(key, task.clone());
    }
    prev
}

// ---- event-loop plumbing ----

/// The C `get_event_loop()`: running loop if any, else
/// `asyncio.events.get_event_loop_policy().get_event_loop()`.
fn get_event_loop_impl(interp: &mut Interp) -> Result<Object, RuntimeError> {
    let rl = running_loop();
    if !matches!(rl, Object::None) {
        return Ok(rl);
    }
    let policy_fn = import_attr(interp, "asyncio.events", "get_event_loop_policy")?;
    let policy = call(interp, &policy_fn, &[])?;
    call_method(interp, &policy, "get_event_loop", &[])
}

fn get_running_loop_impl(interp: &mut Interp) -> Result<Object, RuntimeError> {
    let rl = running_loop();
    if matches!(rl, Object::None) {
        return Err(RuntimeError::PyException(PyException::new(
            crate::builtin_types::make_exception("RuntimeError", "no running event loop"),
        )));
    }
    let _ = interp;
    Ok(rl)
}

// ---- Future core semantics ----

fn future_done_st(st: &Rc<RefCell<FutState>>) -> bool {
    st.borrow().status != FutStatus::Pending
}

/// `loop.call_soon(cb, fut, context=ctx)` for every queued callback.
fn schedule_callbacks(
    interp: &mut Interp,
    self_obj: &Object,
    st: &Rc<RefCell<FutState>>,
) -> Result<(), RuntimeError> {
    let (loop_, callbacks) = {
        let mut s = st.borrow_mut();
        (s.loop_.clone(), std::mem::take(&mut s.callbacks))
    };
    for (cb, ctx) in callbacks {
        call_method_kw(
            interp,
            &loop_,
            "call_soon",
            &[cb, self_obj.clone()],
            &[("context".to_owned(), ctx)],
        )?;
    }
    Ok(())
}

fn ensure_initialized(interp: &mut Interp, st: &Rc<RefCell<FutState>>) -> Result<(), RuntimeError> {
    if !st.borrow().initialized {
        let _ = interp;
        return Err(runtime_err("Future object is not initialized."));
    }
    Ok(())
}

fn make_cancelled_error(
    interp: &mut Interp,
    st: &Rc<RefCell<FutState>>,
) -> Result<Object, RuntimeError> {
    let (saved, msg) = {
        let mut s = st.borrow_mut();
        let saved = std::mem::replace(&mut s.cancelled_exc, Object::None);
        (saved, s.cancel_message.clone())
    };
    if !matches!(saved, Object::None) {
        return Ok(saved);
    }
    let args: Vec<Object> = if matches!(msg, Object::None) {
        vec![]
    } else {
        vec![msg]
    };
    asyncio_exc_instance(interp, "CancelledError", &args)
}

fn raise_cancelled(interp: &mut Interp, st: &Rc<RefCell<FutState>>) -> RuntimeError {
    match make_cancelled_error(interp, st) {
        Ok(inst) => RuntimeError::PyException(PyException::new(inst)),
        Err(e) => e,
    }
}

fn future_result_impl(
    interp: &mut Interp,
    self_obj: &Object,
    st: &Rc<RefCell<FutState>>,
) -> Result<Object, RuntimeError> {
    let _ = self_obj;
    let status = st.borrow().status;
    match status {
        FutStatus::Cancelled => Err(raise_cancelled(interp, st)),
        FutStatus::Pending => Err(raise_asyncio(
            interp,
            "InvalidStateError",
            "Result is not set.",
        )),
        FutStatus::Finished => {
            let (exc, tb, result) = {
                let mut s = st.borrow_mut();
                s.log_traceback = false;
                (
                    s.exception.clone(),
                    s.exception_tb.clone(),
                    s.result.clone(),
                )
            };
            if matches!(exc, Object::None) {
                Ok(result)
            } else {
                // Re-raise with the traceback captured at `set_exception`
                // time (`raise exc.with_traceback(self._exception_tb)` in
                // futures.py): awaiting the same future repeatedly must not
                // accumulate awaiter frames (test_futures2).
                if let Object::Instance(i) = &exc {
                    i.slot_set("__traceback__", tb);
                }
                Err(RuntimeError::PyException(PyException::new(exc)))
            }
        }
    }
}

fn future_exception_impl(
    interp: &mut Interp,
    st: &Rc<RefCell<FutState>>,
) -> Result<Object, RuntimeError> {
    let status = st.borrow().status;
    match status {
        FutStatus::Cancelled => Err(raise_cancelled(interp, st)),
        FutStatus::Pending => Err(raise_asyncio(
            interp,
            "InvalidStateError",
            "Exception is not set.",
        )),
        FutStatus::Finished => {
            let mut s = st.borrow_mut();
            s.log_traceback = false;
            Ok(s.exception.clone())
        }
    }
}

fn invalid_state_already(interp: &mut Interp, what: &str, self_obj: &Object) -> RuntimeError {
    let r = py_repr(interp, self_obj);
    raise_asyncio(interp, "InvalidStateError", &format!("{what}: {r}"))
}

fn future_set_result_impl(
    interp: &mut Interp,
    self_obj: &Object,
    st: &Rc<RefCell<FutState>>,
    result: Object,
) -> Result<(), RuntimeError> {
    ensure_initialized(interp, st)?;
    if future_done_st(st) {
        return Err(invalid_state_already(interp, "invalid state", self_obj));
    }
    {
        let mut s = st.borrow_mut();
        s.result = result;
        s.status = FutStatus::Finished;
    }
    schedule_callbacks(interp, self_obj, st)
}

fn future_set_exception_impl(
    interp: &mut Interp,
    self_obj: &Object,
    st: &Rc<RefCell<FutState>>,
    exc: Object,
) -> Result<(), RuntimeError> {
    ensure_initialized(interp, st)?;
    if future_done_st(st) {
        return Err(invalid_state_already(interp, "invalid state", self_obj));
    }
    // Accept an exception class (instantiate) or instance.
    let exc_inst = match &exc {
        Object::Type(cls) => {
            let bt = crate::builtin_types::builtin_types();
            if !cls.is_subclass_of(&bt.base_exception) {
                return Err(type_error("invalid exception object"));
            }
            call(interp, &exc, &[])?
        }
        Object::Instance(inst) => {
            let bt = crate::builtin_types::builtin_types();
            if !inst.cls().is_subclass_of(&bt.base_exception) {
                return Err(type_error("invalid exception object"));
            }
            exc.clone()
        }
        _ => return Err(type_error("invalid exception object")),
    };
    // A StopIteration is stored as a RuntimeError chained to it (PEP 479
    // interaction; CPython converts rather than rejecting).
    let exc_inst = if is_builtin_exc(&exc_inst, "StopIteration") {
        let new_exc = crate::builtin_types::make_exception(
            "RuntimeError",
            "StopIteration interacts badly with generators and cannot be raised into a Future",
        );
        py_setattr(interp, &new_exc, "__cause__", exc_inst.clone())?;
        py_setattr(interp, &new_exc, "__context__", exc_inst)?;
        new_exc
    } else {
        exc_inst
    };
    {
        let mut s = st.borrow_mut();
        s.exception_tb = exc_traceback_of(&exc_inst);
        s.exception = exc_inst;
        s.status = FutStatus::Finished;
        s.log_traceback = true;
    }
    schedule_callbacks(interp, self_obj, st)
}

/// `exc.__traceback__` at storage time (CPython's `fut_exception_tb`).
fn exc_traceback_of(exc: &Object) -> Object {
    match exc {
        Object::Instance(i) => i.slot_get("__traceback__").unwrap_or(Object::None),
        _ => Object::None,
    }
}

/// `Future.cancel(msg=None)` — the base (non-Task) cancellation.
fn future_cancel_impl(
    interp: &mut Interp,
    self_obj: &Object,
    st: &Rc<RefCell<FutState>>,
    msg: Object,
) -> Result<bool, RuntimeError> {
    {
        let mut s = st.borrow_mut();
        s.log_traceback = false;
        if s.status != FutStatus::Pending {
            return Ok(false);
        }
        s.status = FutStatus::Cancelled;
        s.cancel_message = msg;
    }
    schedule_callbacks(interp, self_obj, st)?;
    Ok(true)
}

fn future_add_done_callback_impl(
    interp: &mut Interp,
    self_obj: &Object,
    st: &Rc<RefCell<FutState>>,
    cb: Object,
    context: Object,
) -> Result<(), RuntimeError> {
    ensure_initialized(interp, st)?;
    let ctx = if matches!(context, Object::None) {
        let copy_context = import_attr(interp, "contextvars", "copy_context")?;
        call(interp, &copy_context, &[])?
    } else {
        context
    };
    if future_done_st(st) {
        let loop_ = st.borrow().loop_.clone();
        call_method_kw(
            interp,
            &loop_,
            "call_soon",
            &[cb, self_obj.clone()],
            &[("context".to_owned(), ctx)],
        )?;
    } else {
        st.borrow_mut().callbacks.push((cb, ctx));
    }
    Ok(())
}

/// Equality for `remove_done_callback` — identity, native value equality,
/// then Python `==` (bound methods compare by `__func__`/`__self__`).
fn py_eq(interp: &mut Interp, a: &Object, b: &Object) -> bool {
    if a.is_same(b) || a.eq_value(b) {
        return true;
    }
    let eq_fn = import_attr(interp, "operator", "eq");
    match eq_fn {
        Ok(f) => match call(interp, &f, &[a.clone(), b.clone()]) {
            Ok(v) => truthy(&v),
            Err(_) => false,
        },
        Err(_) => false,
    }
}

// ---- Future method bindings ----

fn fut_init(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let (inst, st) = state_of(args)?;
    let interp = interp()?;
    let mut loop_ = Object::None;
    for (k, v) in kwargs {
        match k.as_str() {
            "loop" => loop_ = v.clone(),
            other => {
                return Err(type_error(format!(
                    "__init__() got an unexpected keyword argument '{other}'"
                )))
            }
        }
    }
    if args.len() > 1 {
        return Err(type_error(
            "Future.__init__() takes no positional arguments",
        ));
    }
    let loop_ = if matches!(loop_, Object::None) {
        get_event_loop_impl(interp)?
    } else {
        loop_
    };
    let debug = call_method(interp, &loop_, "get_debug", &[])
        .map(|v| truthy(&v))
        .unwrap_or(false);
    {
        let mut s = st.borrow_mut();
        s.loop_ = loop_;
        s.initialized = true;
    }
    if debug {
        let extract_stack = import_attr(interp, "asyncio.format_helpers", "extract_stack")?;
        let stack = call(interp, &extract_stack, &[])?;
        st.borrow_mut().source_traceback = stack;
    }
    let _ = inst;
    Ok(Object::None)
}

fn fut_result(args: &[Object]) -> Result<Object, RuntimeError> {
    let (inst, st) = state_of(args)?;
    let interp = interp()?;
    future_result_impl(interp, &Object::Instance(inst), &st)
}

fn fut_exception(args: &[Object]) -> Result<Object, RuntimeError> {
    let (_inst, st) = state_of(args)?;
    let interp = interp()?;
    future_exception_impl(interp, &st)
}

fn fut_set_result(args: &[Object]) -> Result<Object, RuntimeError> {
    let (inst, st) = state_of(args)?;
    let interp = interp()?;
    let result = args.get(1).cloned().ok_or_else(|| {
        type_error("set_result() missing 1 required positional argument: 'result'")
    })?;
    future_set_result_impl(interp, &Object::Instance(inst), &st, result)?;
    Ok(Object::None)
}

fn fut_set_exception(args: &[Object]) -> Result<Object, RuntimeError> {
    let (inst, st) = state_of(args)?;
    let interp = interp()?;
    let exc = args.get(1).cloned().ok_or_else(|| {
        type_error("set_exception() missing 1 required positional argument: 'exception'")
    })?;
    future_set_exception_impl(interp, &Object::Instance(inst), &st, exc)?;
    Ok(Object::None)
}

fn fut_add_done_callback(
    args: &[Object],
    kwargs: &[(String, Object)],
) -> Result<Object, RuntimeError> {
    let (inst, st) = state_of(args)?;
    let interp = interp()?;
    let cb = args.get(1).cloned().ok_or_else(|| {
        type_error("add_done_callback() missing 1 required positional argument: 'fn'")
    })?;
    let mut context = Object::None;
    for (k, v) in kwargs {
        match k.as_str() {
            "context" => context = v.clone(),
            other => {
                return Err(type_error(format!(
                    "add_done_callback() got an unexpected keyword argument '{other}'"
                )))
            }
        }
    }
    future_add_done_callback_impl(interp, &Object::Instance(inst), &st, cb, context)?;
    Ok(Object::None)
}

fn fut_remove_done_callback(args: &[Object]) -> Result<Object, RuntimeError> {
    let (_inst, st) = state_of(args)?;
    let interp = interp()?;
    ensure_initialized(interp, &st)?;
    let target = args.get(1).cloned().ok_or_else(|| {
        type_error("remove_done_callback() missing 1 required positional argument: 'fn'")
    })?;
    let callbacks = st.borrow().callbacks.clone();
    let mut kept: Vec<(Object, Object)> = Vec::with_capacity(callbacks.len());
    let mut removed = 0i64;
    for (cb, ctx) in callbacks {
        if py_eq(interp, &cb, &target) {
            removed += 1;
        } else {
            kept.push((cb, ctx));
        }
    }
    st.borrow_mut().callbacks = kept;
    Ok(Object::Int(removed))
}

fn fut_cancel(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let (inst, st) = state_of(args)?;
    let interp = interp()?;
    ensure_initialized(interp, &st)?;
    let mut msg = args.get(1).cloned().unwrap_or(Object::None);
    for (k, v) in kwargs {
        match k.as_str() {
            "msg" => msg = v.clone(),
            other => {
                return Err(type_error(format!(
                    "cancel() got an unexpected keyword argument '{other}'"
                )))
            }
        }
    }
    let cancelled = future_cancel_impl(interp, &Object::Instance(inst), &st, msg)?;
    Ok(Object::Bool(cancelled))
}

fn fut_cancelled(args: &[Object]) -> Result<Object, RuntimeError> {
    let (_inst, st) = state_of(args)?;
    let out = Object::Bool(st.borrow().status == FutStatus::Cancelled);
    Ok(out)
}

fn fut_done(args: &[Object]) -> Result<Object, RuntimeError> {
    let (_inst, st) = state_of(args)?;
    Ok(Object::Bool(future_done_st(&st)))
}

fn fut_get_loop(args: &[Object]) -> Result<Object, RuntimeError> {
    let (_inst, st) = state_of(args)?;
    let interp = interp()?;
    ensure_initialized(interp, &st)?;
    let out = st.borrow().loop_.clone();
    Ok(out)
}

fn fut_make_cancelled_error(args: &[Object]) -> Result<Object, RuntimeError> {
    let (_inst, st) = state_of(args)?;
    let interp = interp()?;
    make_cancelled_error(interp, &st)
}

fn fut_await(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = future_self(args)?;
    Ok(future_iter_new(&Object::Instance(inst)))
}

fn fut_repr(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = future_self(args)?;
    let interp = interp()?;
    let repr_fn = import_attr(interp, "asyncio.base_futures", "_future_repr")?;
    call(interp, &repr_fn, &[Object::Instance(inst)])
}

fn fut_del(args: &[Object]) -> Result<Object, RuntimeError> {
    // Un-retrieved exception logging (CPython `future_finalize`): if the
    // future holds an exception nobody consumed, hand it to the loop's
    // exception handler. Best-effort — a torn-down loop swallows it.
    let Ok(inst) = future_self(args) else {
        return Ok(Object::None);
    };
    let st = state_of_instance(&inst);
    let (log, exc, loop_) = {
        let s = st.borrow();
        (s.log_traceback, s.exception.clone(), s.loop_.clone())
    };
    if log && !matches!(exc, Object::None) && !matches!(loop_, Object::None) {
        if let Ok(interp) = interp() {
            let self_obj = Object::Instance(inst.clone());
            let mut ctx = DictData::default();
            ctx.insert(
                DictKey(Object::from_static("message")),
                Object::from_str(format!(
                    "{} exception was never retrieved",
                    inst.cls().name.clone()
                )),
            );
            ctx.insert(DictKey(Object::from_static("exception")), exc);
            ctx.insert(DictKey(Object::from_static("future")), self_obj);
            let source_tb = st.borrow().source_traceback.clone();
            if !matches!(source_tb, Object::None) {
                ctx.insert(DictKey(Object::from_static("source_traceback")), source_tb);
            }
            let _ = call_method(
                interp,
                &loop_,
                "call_exception_handler",
                &[Object::Dict(Rc::new(RefCell::new(ctx)))],
            );
        }
    }
    release_state(&inst);
    Ok(Object::None)
}

fn class_getitem(args: &[Object]) -> Result<Object, RuntimeError> {
    // `Future[int]` / `Task[int]` → types.GenericAlias(cls, item).
    let interp = interp()?;
    let cls = args
        .first()
        .cloned()
        .ok_or_else(|| type_error("__class_getitem__ requires a class"))?;
    let item = args.get(1).cloned().unwrap_or(Object::None);
    let ga = import_attr(interp, "types", "GenericAlias")?;
    call(interp, &ga, &[cls, item])
}

// ---- Future getsets ----

fn futprop_state(args: &[Object]) -> Result<Object, RuntimeError> {
    let (_inst, st) = state_of(args)?;
    let out = Object::from_static(st.borrow().status.as_str());
    Ok(out)
}

fn futprop_loop(args: &[Object]) -> Result<Object, RuntimeError> {
    let (_inst, st) = state_of(args)?;
    let out = st.borrow().loop_.clone();
    Ok(out)
}

fn futprop_callbacks(args: &[Object]) -> Result<Object, RuntimeError> {
    let (_inst, st) = state_of(args)?;
    let cbs: Vec<Object> = st
        .borrow()
        .callbacks
        .iter()
        .map(|(cb, ctx)| Object::new_tuple(vec![cb.clone(), ctx.clone()]))
        .collect();
    // The C getter reports `None` for an empty callback list (a fresh copy
    // otherwise) — `test_callbacks_copy` pins this.
    if cbs.is_empty() {
        return Ok(Object::None);
    }
    Ok(Object::new_list(cbs))
}

fn futprop_result(args: &[Object]) -> Result<Object, RuntimeError> {
    let (_inst, st) = state_of(args)?;
    let out = st.borrow().result.clone();
    Ok(out)
}

fn futprop_exception(args: &[Object]) -> Result<Object, RuntimeError> {
    let (_inst, st) = state_of(args)?;
    let out = st.borrow().exception.clone();
    Ok(out)
}

fn futprop_log_traceback(args: &[Object]) -> Result<Object, RuntimeError> {
    let (_inst, st) = state_of(args)?;
    let out = Object::Bool(st.borrow().log_traceback);
    Ok(out)
}

fn futprop_set_log_traceback(args: &[Object]) -> Result<Object, RuntimeError> {
    let (_inst, st) = state_of(args)?;
    let v = args.get(1).cloned().unwrap_or(Object::None);
    if truthy(&v) {
        return Err(value_error("_log_traceback can only be set to False"));
    }
    st.borrow_mut().log_traceback = false;
    Ok(Object::None)
}

fn futprop_source_traceback(args: &[Object]) -> Result<Object, RuntimeError> {
    let (_inst, st) = state_of(args)?;
    let out = st.borrow().source_traceback.clone();
    Ok(out)
}

fn futprop_cancel_message(args: &[Object]) -> Result<Object, RuntimeError> {
    let (_inst, st) = state_of(args)?;
    let out = st.borrow().cancel_message.clone();
    Ok(out)
}

fn futprop_set_cancel_message(args: &[Object]) -> Result<Object, RuntimeError> {
    let (_inst, st) = state_of(args)?;
    st.borrow_mut().cancel_message = args.get(1).cloned().unwrap_or(Object::None);
    Ok(Object::None)
}

fn futprop_blocking(args: &[Object]) -> Result<Object, RuntimeError> {
    let (_inst, st) = state_of(args)?;
    let out = Object::Bool(st.borrow().blocking);
    Ok(out)
}

fn futprop_set_blocking(args: &[Object]) -> Result<Object, RuntimeError> {
    let (_inst, st) = state_of(args)?;
    let v = args.get(1).cloned().unwrap_or(Object::None);
    st.borrow_mut().blocking = truthy(&v);
    Ok(Object::None)
}

// ---- Task getsets ----

fn taskprop_coro(args: &[Object]) -> Result<Object, RuntimeError> {
    let (_inst, st) = state_of(args)?;
    let out = st.borrow().coro.clone();
    Ok(out)
}

fn taskprop_fut_waiter(args: &[Object]) -> Result<Object, RuntimeError> {
    let (_inst, st) = state_of(args)?;
    let out = st.borrow().fut_waiter.clone();
    Ok(out)
}

fn taskprop_must_cancel(args: &[Object]) -> Result<Object, RuntimeError> {
    let (_inst, st) = state_of(args)?;
    let out = Object::Bool(st.borrow().must_cancel);
    Ok(out)
}

fn taskprop_log_destroy_pending(args: &[Object]) -> Result<Object, RuntimeError> {
    let (_inst, st) = state_of(args)?;
    let out = Object::Bool(st.borrow().log_destroy_pending);
    Ok(out)
}

fn taskprop_set_log_destroy_pending(args: &[Object]) -> Result<Object, RuntimeError> {
    let (_inst, st) = state_of(args)?;
    let v = args.get(1).cloned().unwrap_or(Object::None);
    st.borrow_mut().log_destroy_pending = truthy(&v);
    Ok(Object::None)
}

fn taskprop_num_cancels(args: &[Object]) -> Result<Object, RuntimeError> {
    let (_inst, st) = state_of(args)?;
    let out = Object::Int(st.borrow().num_cancels_requested);
    Ok(out)
}

// ---- FutureIter ----

const FI_FUT_KEY: &str = "__weavepy_fi_fut__";
const FI_DONE_KEY: &str = "__weavepy_fi_done__";

fn future_iter_class() -> Rc<TypeObject> {
    static CELL: std::sync::OnceLock<Rc<TypeObject>> = std::sync::OnceLock::new();
    CELL.get_or_init(|| {
        let bt = crate::builtin_types::builtin_types();
        let mut dict = DictData::default();
        dict.insert(
            DictKey(Object::from_static("__module__")),
            Object::from_static("_asyncio"),
        );
        for (name, body) in [
            (
                "__iter__",
                fi_iter as fn(&[Object]) -> Result<Object, RuntimeError>,
            ),
            ("__next__", fi_next),
            ("send", fi_send),
            ("throw", fi_throw),
            ("close", fi_close),
        ] {
            dict.insert(
                DictKey(Object::from_static(name)),
                Object::Builtin(Rc::new(BuiltinFn {
                    name,
                    binds_instance: true,
                    call: Box::new(body),
                    call_kw: None,
                })),
            );
        }
        TypeObject::new_user("FutureIter", vec![bt.object_.clone()], dict)
            .expect("FutureIter class must linearise")
    })
    .clone()
}

fn future_iter_new(fut: &Object) -> Object {
    let inst = Rc::new(PyInstance::new(future_iter_class()));
    inst.dict
        .borrow_mut()
        .insert(DictKey(Object::from_static(FI_FUT_KEY)), fut.clone());
    inst.dict.borrow_mut().insert(
        DictKey(Object::from_static(FI_DONE_KEY)),
        Object::Bool(false),
    );
    let out = Object::Instance(inst);
    // Built outside the ordinary instantiation path, so enrol with the
    // collector by hand: a suspended `await` parks this iterator on the
    // coroutine's value stack, and its strong `fut` edge must be
    // subtractable for task↔future cycles to collapse. Prompt-reclaim
    // enrollment keeps its death by "refcount" observable — a lingering
    // iterator pins the awaited Task, its coroutine frame, and every local
    // in it (test_ssl's weakref leak tests watch exactly that chain).
    crate::gc_trace::track_prompt_reclaim(out.clone());
    out
}

fn fi_self(args: &[Object]) -> Result<Rc<PyInstance>, RuntimeError> {
    match args.first() {
        Some(Object::Instance(inst)) => Ok(inst.clone()),
        _ => Err(type_error("FutureIter method requires FutureIter self")),
    }
}

fn fi_iter(args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(args.first().cloned().unwrap_or(Object::None))
}

fn fi_next(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = fi_self(args)?;
    let (fut, done) = {
        let d = inst.dict.borrow();
        (
            d.get(&DictKey(Object::from_static(FI_FUT_KEY)))
                .cloned()
                .unwrap_or(Object::None),
            matches!(
                d.get(&DictKey(Object::from_static(FI_DONE_KEY))),
                Some(Object::Bool(true))
            ),
        )
    };
    if done || matches!(fut, Object::None) {
        return Err(crate::error::stop_iteration_with(Object::None));
    }
    let st = state_of_obj(&fut)?;
    if !future_done_st(&st) {
        // First step of the two-step protocol: yield the future itself.
        // A second `__next__` while still pending means nobody consumed
        // the yielded future (CPython `futureiter_iternext`).
        let already_blocking = st.borrow().blocking;
        if !already_blocking {
            st.borrow_mut().blocking = true;
            return Ok(fut);
        }
        return Err(runtime_err("await wasn't used with future"));
    }
    inst.dict.borrow_mut().insert(
        DictKey(Object::from_static(FI_DONE_KEY)),
        Object::Bool(true),
    );
    let interp = interp()?;
    let value = future_result_impl(interp, &fut, &st)?;
    Err(crate::error::stop_iteration_with(value))
}

fn fi_send(args: &[Object]) -> Result<Object, RuntimeError> {
    // `send(value)` ignores the value (CPython `futureiter_send`).
    fi_next(&args[..1])
}

fn fi_throw(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = fi_self(args)?;
    let interp = interp()?;
    let typ_or_inst = args
        .get(1)
        .cloned()
        .ok_or_else(|| type_error("throw() requires an exception"))?;
    // The multi-arg form is deprecated (gh-102209); validation happens
    // *before* the iterator is marked done, matching `futureiter_throw`.
    if args.len() > 2 {
        let warn = import_attr(interp, "warnings", "warn")?;
        let dep_cls = crate::builtin_types::builtin_types()
            .by_name("DeprecationWarning")
            .map(Object::Type)
            .unwrap_or(Object::None);
        call(
            interp,
            &warn,
            &[
                Object::from_static(
                    "the (type, exc, tb) signature of throw() is deprecated, \
                     use the single-arg signature instead.",
                ),
                dep_cls,
            ],
        )?;
    }
    let val = args.get(2).cloned().unwrap_or(Object::None);
    let tb = args.get(3).cloned().unwrap_or(Object::None);
    if !matches!(tb, Object::None | Object::Traceback(_)) {
        return Err(type_error("throw() third argument must be a traceback"));
    }
    let bt = crate::builtin_types::builtin_types();
    let exc_inst = match &typ_or_inst {
        Object::Type(cls) if cls.is_subclass_of(&bt.base_exception) => {
            let call_args: Vec<Object> = match &val {
                Object::None => vec![],
                Object::Tuple(items) => items.to_vec(),
                other => vec![other.clone()],
            };
            call(interp, &typ_or_inst, &call_args)?
        }
        Object::Instance(i) if i.cls().is_subclass_of(&bt.base_exception) => {
            if !matches!(val, Object::None) {
                return Err(type_error(
                    "instance exception may not have a separate value",
                ));
            }
            typ_or_inst
        }
        other => {
            return Err(type_error(format!(
                "exceptions must be classes or instances deriving from BaseException, not {}",
                other.type_name()
            )))
        }
    };
    inst.dict.borrow_mut().insert(
        DictKey(Object::from_static(FI_DONE_KEY)),
        Object::Bool(true),
    );
    Err(RuntimeError::PyException(PyException::new(exc_inst)))
}

fn fi_close(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = fi_self(args)?;
    inst.dict.borrow_mut().insert(
        DictKey(Object::from_static(FI_DONE_KEY)),
        Object::Bool(true),
    );
    Ok(Object::None)
}

// ---- Task ----

fn task_name_counter() -> i64 {
    use std::sync::atomic::{AtomicI64, Ordering};
    static NEXT: AtomicI64 = AtomicI64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// The `self.__step` callable handed to `loop.call_soon` — CPython's
/// `TaskStepMethWrapper`. Accepts an optional exception argument.
///
/// Built as a `BoundMethod` with the task as receiver rather than a
/// closure capturing it: `BoundMethod` is traversed by the cycle
/// collector (receiver + function), so the strong task reference held
/// through a loop handle or a future's callback list is a *visible*
/// edge and task↔future reference cycles stay collectible (CPython's
/// `TaskStepMethWrapper_traverse`; test_log_destroyed_pending_task).
fn task_step_callable(task: Object) -> Object {
    static STEP_FN: std::sync::OnceLock<Object> = std::sync::OnceLock::new();
    let f = STEP_FN.get_or_init(|| {
        Object::Builtin(Rc::new(BuiltinFn {
            name: "Task.__step",
            binds_instance: false,
            call: Box::new(|args| {
                let task = args
                    .first()
                    .cloned()
                    .ok_or_else(|| type_error("__step() missing task"))?;
                task_step(
                    &task,
                    args.get(1).cloned().filter(|o| !matches!(o, Object::None)),
                )?;
                Ok(Object::None)
            }),
            call_kw: None,
        }))
    });
    Object::BoundMethod(Rc::new(BoundMethod::new(task, f.clone())))
}

/// The `self.__wakeup` done-callback. Same `BoundMethod` shape as
/// [`task_step_callable`], for the same GC-visibility reason.
fn task_wakeup_callable(task: Object) -> Object {
    static WAKEUP_FN: std::sync::OnceLock<Object> = std::sync::OnceLock::new();
    let f = WAKEUP_FN.get_or_init(|| {
        Object::Builtin(Rc::new(BuiltinFn {
            name: "Task.__wakeup",
            binds_instance: false,
            call: Box::new(|args| {
                let task = args
                    .first()
                    .cloned()
                    .ok_or_else(|| type_error("__wakeup() missing task"))?;
                let future = args
                    .get(1)
                    .cloned()
                    .ok_or_else(|| type_error("__wakeup() missing future"))?;
                let interp = interp()?;
                match call_method(interp, &future, "result", &[]) {
                    Ok(_) => task_step(&task, None)?,
                    Err(RuntimeError::PyException(pe)) => task_step(&task, Some(pe.instance))?,
                    Err(other) => return Err(other),
                }
                Ok(Object::None)
            }),
            call_kw: None,
        }))
    });
    Object::BoundMethod(Rc::new(BoundMethod::new(task, f.clone())))
}

fn task_init(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let (inst, st) = state_of(args)?;
    let interp = interp()?;
    let self_obj = Object::Instance(inst.clone());
    let coro = args
        .get(1)
        .cloned()
        .ok_or_else(|| type_error("Task.__init__() missing 1 required argument: 'coro'"))?;
    let mut loop_ = Object::None;
    let mut name = Object::None;
    let mut context = Object::None;
    let mut eager_start = false;
    for (k, v) in kwargs {
        match k.as_str() {
            "loop" => loop_ = v.clone(),
            "name" => name = v.clone(),
            "context" => context = v.clone(),
            "eager_start" => eager_start = truthy(v),
            other => {
                return Err(type_error(format!(
                    "Task.__init__() got an unexpected keyword argument '{other}'"
                )))
            }
        }
    }
    // Future.__init__(loop=loop) part.
    let loop_ = if matches!(loop_, Object::None) {
        get_event_loop_impl(interp)?
    } else {
        loop_
    };
    let debug = call_method(interp, &loop_, "get_debug", &[])
        .map(|v| truthy(&v))
        .unwrap_or(false);
    {
        let mut s = st.borrow_mut();
        s.loop_ = loop_.clone();
        s.initialized = true;
    }
    if debug {
        let extract_stack = import_attr(interp, "asyncio.format_helpers", "extract_stack")?;
        // Native __init__ contributes no Python frame, so the extracted
        // stack already starts at our caller — same shape as the C Task's
        // `extract_stack(sys._getframe(1))` (no pop needed;
        // `test_task_source_traceback` checks `[-2]` is the test body).
        let stack = call(interp, &extract_stack, &[])?;
        st.borrow_mut().source_traceback = stack;
    }
    // Coroutine validation.
    let iscoroutine = import_attr(interp, "asyncio.coroutines", "iscoroutine")?;
    let is_coro = call(interp, &iscoroutine, std::slice::from_ref(&coro))?;
    if !truthy(&is_coro) {
        st.borrow_mut().log_destroy_pending = false;
        let r = py_repr(interp, &coro);
        // C Task's wording ("expected"), not the Python Task's ("required").
        return Err(type_error(format!("a coroutine was expected, got {r}")));
    }
    let name_obj = if matches!(name, Object::None) {
        Object::from_str(format!("Task-{}", task_name_counter()))
    } else if matches!(name, Object::Str(_)) {
        name
    } else {
        // `str(name)`, with errors propagating (a raising `__str__` must
        // abort __init__ — gh-126083 / test_proper_refcounts).
        let str_type = import_attr(interp, "builtins", "str")?;
        call(interp, &str_type, &[name])?
    };
    let ctx = if matches!(context, Object::None) {
        let copy_context = import_attr(interp, "contextvars", "copy_context")?;
        call(interp, &copy_context, &[])?
    } else {
        context
    };
    {
        let mut s = st.borrow_mut();
        s.coro = coro;
        s.name = name_obj;
        s.context = ctx.clone();
        s.num_cancels_requested = 0;
        s.must_cancel = false;
        s.fut_waiter = Object::None;
    }
    let loop_running = call_method(interp, &loop_, "is_running", &[])
        .map(|v| truthy(&v))
        .unwrap_or(false);
    if eager_start && loop_running {
        task_eager_start(interp, &self_obj, &st)?;
    } else {
        call_method_kw(
            interp,
            &loop_,
            "call_soon",
            &[task_step_callable(self_obj.clone())],
            &[("context".to_owned(), ctx)],
        )?;
        register_task_impl(interp, &self_obj)?;
    }
    Ok(Object::None)
}

fn task_eager_start(
    interp: &mut Interp,
    self_obj: &Object,
    st: &Rc<RefCell<FutState>>,
) -> Result<(), RuntimeError> {
    let loop_ = st.borrow().loop_.clone();
    let prev = swap_current_task_impl(&loop_, self_obj);
    register_eager_task_impl(interp, self_obj)?;
    // ctx.run(step_run_and_handle, None) — run the first step synchronously
    // inside the task's context, without the enter/leave pair (the swap
    // above already installed us as current).
    let ctx = st.borrow().context.clone();
    let step_fn = {
        let task = self_obj.clone();
        Object::Builtin(Rc::new(BuiltinFn {
            name: "Task.__eager_step",
            binds_instance: false,
            call: Box::new(move |_args| {
                task_step_inner(&task, None, false)?;
                Ok(Object::None)
            }),
            call_kw: None,
        }))
    };
    let run_result = call_method(interp, &ctx, "run", &[step_fn]);
    let unregister_result = unregister_eager_task_impl(interp, self_obj);
    swap_current_task_impl(&loop_, &prev);
    run_result?;
    unregister_result?;
    if !future_done_st(st) {
        // The eager task suspended: it "graduates" to a scheduled task.
        register_task_impl(interp, self_obj)?;
    } else {
        st.borrow_mut().coro = Object::None;
    }
    Ok(())
}

fn task_step(task: &Object, exc: Option<Object>) -> Result<(), RuntimeError> {
    task_step_inner(task, exc, true)
}

fn task_step_inner(
    task: &Object,
    exc_in: Option<Object>,
    manage_current: bool,
) -> Result<(), RuntimeError> {
    let st = state_of_obj(task)?;
    let interp = interp()?;
    if future_done_st(&st) {
        let task_r = py_repr(interp, task);
        let exc_r = match &exc_in {
            Some(e) => py_repr(interp, e),
            None => "None".to_owned(),
        };
        return Err(raise_asyncio(
            interp,
            "InvalidStateError",
            &format!("__step(): already done: {task_r}, {exc_r}"),
        ));
    }
    // must_cancel: deliver a CancelledError if the pending exception isn't
    // one already.
    let mut exc = exc_in;
    let must_cancel_now = st.borrow().must_cancel;
    if must_cancel_now {
        let is_cancelled = exc
            .as_ref()
            .map(|e| is_instance_of_named(interp, e, "asyncio.exceptions", "CancelledError"))
            .unwrap_or(false);
        if !is_cancelled {
            exc = Some(make_cancelled_error(interp, &st)?);
        }
        st.borrow_mut().must_cancel = false;
    }
    st.borrow_mut().fut_waiter = Object::None;
    let (coro, loop_, context) = {
        let s = st.borrow();
        (s.coro.clone(), s.loop_.clone(), s.context.clone())
    };
    if manage_current {
        enter_task_impl(interp, &loop_, task)?;
    }
    let send_result = match &exc {
        Some(e) => {
            let throw_m = interp.load_attr_public(&coro, "throw");
            match throw_m {
                Ok(m) => call(interp, &m, std::slice::from_ref(e)),
                Err(err) => Err(err),
            }
        }
        None => match interp.load_attr_public(&coro, "send") {
            Ok(m) => call(interp, &m, &[Object::None]),
            Err(err) => Err(err),
        },
    };
    let step_result = task_step_handle_result(interp, task, &st, &loop_, &context, send_result);
    if manage_current {
        let leave = leave_task_impl(interp, &loop_, task);
        step_result?;
        leave?;
        return Ok(());
    }
    step_result
}

fn schedule_step_with_exc(
    interp: &mut Interp,
    task: &Object,
    loop_: &Object,
    context: &Object,
    exc: Object,
) -> Result<(), RuntimeError> {
    call_method_kw(
        interp,
        loop_,
        "call_soon",
        &[task_step_callable(task.clone()), exc],
        &[("context".to_owned(), context.clone())],
    )?;
    Ok(())
}

fn task_step_handle_result(
    interp: &mut Interp,
    task: &Object,
    st: &Rc<RefCell<FutState>>,
    loop_: &Object,
    context: &Object,
    send_result: Result<Object, RuntimeError>,
) -> Result<(), RuntimeError> {
    match send_result {
        Err(RuntimeError::PyException(pe)) => {
            let inst = pe.instance.clone();
            if is_builtin_exc(&inst, "StopIteration") {
                // Coroutine completed.
                let value = stop_iteration_value(&inst);
                let must_cancel = st.borrow().must_cancel;
                if must_cancel {
                    st.borrow_mut().must_cancel = false;
                    let msg = st.borrow().cancel_message.clone();
                    future_cancel_impl(interp, task, st, msg)?;
                } else {
                    // Internal set_result (bypasses Task's override).
                    if future_done_st(st) {
                        return Err(invalid_state_already(interp, "invalid state", task));
                    }
                    {
                        let mut s = st.borrow_mut();
                        s.result = value;
                        s.status = FutStatus::Finished;
                    }
                    schedule_callbacks(interp, task, st)?;
                }
                Ok(())
            } else if is_instance_of_named(interp, &inst, "asyncio.exceptions", "CancelledError") {
                st.borrow_mut().cancelled_exc = inst;
                future_cancel_impl(interp, task, st, Object::None)?;
                Ok(())
            } else if is_builtin_exc(&inst, "KeyboardInterrupt")
                || is_builtin_exc(&inst, "SystemExit")
            {
                task_internal_set_exception(interp, task, st, inst)?;
                Err(RuntimeError::PyException(pe))
            } else {
                task_internal_set_exception(interp, task, st, inst)?;
                Ok(())
            }
        }
        Err(other) => Err(other),
        Ok(result) => {
            // The coroutine suspended, yielding `result`.
            let blocking_attr = interp
                .load_attr_public(&result, "_asyncio_future_blocking")
                .ok();
            if let Some(blocking) = blocking_attr.filter(|b| !matches!(b, Object::None)) {
                // Future-like protocol.
                let result_loop = get_loop_of(interp, &result);
                if !same_object(&result_loop, loop_) {
                    let t = py_repr(interp, task);
                    let r = py_repr(interp, &result);
                    let new_exc = crate::builtin_types::make_exception(
                        "RuntimeError",
                        format!("Task {t} got Future {r} attached to a different loop"),
                    );
                    schedule_step_with_exc(interp, task, loop_, context, new_exc)
                } else if truthy(&blocking) {
                    if same_object(&result, task) {
                        let t = py_repr(interp, task);
                        let new_exc = crate::builtin_types::make_exception(
                            "RuntimeError",
                            format!("Task cannot await on itself: {t}"),
                        );
                        schedule_step_with_exc(interp, task, loop_, context, new_exc)
                    } else {
                        set_future_blocking_false(interp, &result)?;
                        call_method_kw(
                            interp,
                            &result,
                            "add_done_callback",
                            &[task_wakeup_callable(task.clone())],
                            &[("context".to_owned(), context.clone())],
                        )?;
                        st.borrow_mut().fut_waiter = result.clone();
                        let must_cancel = st.borrow().must_cancel;
                        if must_cancel {
                            let msg = st.borrow().cancel_message.clone();
                            let cancelled = call_method_kw(
                                interp,
                                &result,
                                "cancel",
                                &[],
                                &[("msg".to_owned(), msg)],
                            )?;
                            if truthy(&cancelled) {
                                st.borrow_mut().must_cancel = false;
                            }
                        }
                        Ok(())
                    }
                } else {
                    let t = py_repr(interp, task);
                    let r = py_repr(interp, &result);
                    let new_exc = crate::builtin_types::make_exception(
                        "RuntimeError",
                        format!("yield was used instead of yield from in task {t} with {r}"),
                    );
                    schedule_step_with_exc(interp, task, loop_, context, new_exc)
                }
            } else if matches!(result, Object::None) {
                // Bare `yield` — reschedule.
                call_method_kw(
                    interp,
                    loop_,
                    "call_soon",
                    &[task_step_callable(task.clone())],
                    &[("context".to_owned(), context.clone())],
                )?;
                Ok(())
            } else if matches!(result, Object::Generator(_)) {
                let t = py_repr(interp, task);
                let r = py_repr(interp, &result);
                let new_exc = crate::builtin_types::make_exception(
                    "RuntimeError",
                    format!(
                        "yield was used instead of yield from for generator in task {t} with {r}"
                    ),
                );
                schedule_step_with_exc(interp, task, loop_, context, new_exc)
            } else {
                let r = py_repr(interp, &result);
                let new_exc = crate::builtin_types::make_exception(
                    "RuntimeError",
                    format!("Task got bad yield: {r}"),
                );
                schedule_step_with_exc(interp, task, loop_, context, new_exc)
            }
        }
    }
}

/// Internal `Future.set_exception` used by the task step (bypasses the
/// Task-level "does not support set_exception" override).
fn task_internal_set_exception(
    interp: &mut Interp,
    task: &Object,
    st: &Rc<RefCell<FutState>>,
    exc_inst: Object,
) -> Result<(), RuntimeError> {
    if future_done_st(st) {
        return Err(invalid_state_already(interp, "invalid state", task));
    }
    {
        let mut s = st.borrow_mut();
        s.exception_tb = exc_traceback_of(&exc_inst);
        s.exception = exc_inst;
        s.status = FutStatus::Finished;
        s.log_traceback = true;
    }
    schedule_callbacks(interp, task, st)
}

fn stop_iteration_value(inst: &Object) -> Object {
    if let Object::Instance(i) = inst {
        if let Some(v) = crate::builtin_types::exc_attr(i, "value") {
            return v;
        }
        if let Some(Object::Tuple(items)) = crate::builtin_types::exc_attr(i, "args") {
            if let Some(first) = items.first() {
                return first.clone();
            }
        }
    }
    Object::None
}

/// `futures._get_loop(fut)`: `fut.get_loop()` when available, else `_loop`.
fn get_loop_of(interp: &mut Interp, fut: &Object) -> Object {
    if let Ok(m) = interp.load_attr_public(fut, "get_loop") {
        if let Ok(l) = call(interp, &m, &[]) {
            return l;
        }
    }
    interp
        .load_attr_public(fut, "_loop")
        .unwrap_or(Object::None)
}

fn set_future_blocking_false(interp: &mut Interp, fut: &Object) -> Result<(), RuntimeError> {
    // Fast path for native futures; foreign future-likes get a setattr.
    if let Object::Instance(inst) = fut {
        if inst
            .dict
            .borrow()
            .get(&DictKey(Object::from_static(HANDLE_KEY)))
            .is_some()
        {
            state_of_instance(inst).borrow_mut().blocking = false;
            return Ok(());
        }
    }
    py_setattr(interp, fut, "_asyncio_future_blocking", Object::Bool(false))
}

// ---- Task method bindings ----

fn task_cancel(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let (inst, st) = state_of(args)?;
    let interp = interp()?;
    let _self_obj = Object::Instance(inst);
    let mut msg = args.get(1).cloned().unwrap_or(Object::None);
    for (k, v) in kwargs {
        match k.as_str() {
            "msg" => msg = v.clone(),
            other => {
                return Err(type_error(format!(
                    "cancel() got an unexpected keyword argument '{other}'"
                )))
            }
        }
    }
    st.borrow_mut().log_traceback = false;
    if future_done_st(&st) {
        return Ok(Object::Bool(false));
    }
    st.borrow_mut().num_cancels_requested += 1;
    let fut_waiter = st.borrow().fut_waiter.clone();
    if !matches!(fut_waiter, Object::None) {
        let cancelled = call_method_kw(
            interp,
            &fut_waiter,
            "cancel",
            &[],
            &[("msg".to_owned(), msg.clone())],
        )?;
        if truthy(&cancelled) {
            return Ok(Object::Bool(true));
        }
    }
    {
        let mut s = st.borrow_mut();
        s.must_cancel = true;
        s.cancel_message = msg;
    }
    Ok(Object::Bool(true))
}

fn task_cancelling(args: &[Object]) -> Result<Object, RuntimeError> {
    let (_inst, st) = state_of(args)?;
    let out = Object::Int(st.borrow().num_cancels_requested);
    Ok(out)
}

fn task_uncancel(args: &[Object]) -> Result<Object, RuntimeError> {
    let (_inst, st) = state_of(args)?;
    let mut s = st.borrow_mut();
    if s.num_cancels_requested > 0 {
        s.num_cancels_requested -= 1;
        if s.num_cancels_requested == 0 {
            s.must_cancel = false;
        }
    }
    Ok(Object::Int(s.num_cancels_requested))
}

fn task_get_coro(args: &[Object]) -> Result<Object, RuntimeError> {
    let (_inst, st) = state_of(args)?;
    let out = st.borrow().coro.clone();
    Ok(out)
}

fn task_get_context(args: &[Object]) -> Result<Object, RuntimeError> {
    let (_inst, st) = state_of(args)?;
    let out = st.borrow().context.clone();
    Ok(out)
}

fn task_get_name(args: &[Object]) -> Result<Object, RuntimeError> {
    let (_inst, st) = state_of(args)?;
    let name = st.borrow().name.clone();
    Ok(if matches!(name, Object::None) {
        Object::from_static("")
    } else {
        name
    })
}

fn task_set_name(args: &[Object]) -> Result<Object, RuntimeError> {
    let (_inst, st) = state_of(args)?;
    let value = args
        .get(1)
        .cloned()
        .ok_or_else(|| type_error("set_name() missing 1 required positional argument"))?;
    let name = if matches!(value, Object::Str(_)) {
        value
    } else {
        let interp = interp()?;
        let str_fn = interp
            .builtins_dict()
            .borrow()
            .get(&DictKey(Object::from_static("str")))
            .cloned()
            .ok_or_else(|| RuntimeError::Internal("no str builtin".to_owned()))?;
        call(interp, &str_fn, &[value])?
    };
    st.borrow_mut().name = name;
    Ok(Object::None)
}

fn task_set_result_disallowed(args: &[Object]) -> Result<Object, RuntimeError> {
    let _ = state_of(args)?;
    Err(runtime_err("Task does not support set_result operation"))
}

fn task_set_exception_disallowed(args: &[Object]) -> Result<Object, RuntimeError> {
    let _ = state_of(args)?;
    Err(runtime_err("Task does not support set_exception operation"))
}

fn task_get_stack(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let inst = future_self(args)?;
    let interp = interp()?;
    let mut limit = Object::None;
    for (k, v) in kwargs {
        if k == "limit" {
            limit = v.clone();
        }
    }
    let f = import_attr(interp, "asyncio.base_tasks", "_task_get_stack")?;
    call(interp, &f, &[Object::Instance(inst), limit])
}

fn task_print_stack(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let inst = future_self(args)?;
    let interp = interp()?;
    let mut limit = Object::None;
    let mut file = Object::None;
    for (k, v) in kwargs {
        match k.as_str() {
            "limit" => limit = v.clone(),
            "file" => file = v.clone(),
            _ => {}
        }
    }
    let f = import_attr(interp, "asyncio.base_tasks", "_task_print_stack")?;
    call(interp, &f, &[Object::Instance(inst), limit, file])
}

fn task_repr(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = future_self(args)?;
    let interp = interp()?;
    let repr_fn = import_attr(interp, "asyncio.base_tasks", "_task_repr")?;
    call(interp, &repr_fn, &[Object::Instance(inst)])
}

fn task_del(args: &[Object]) -> Result<Object, RuntimeError> {
    let Ok(inst) = future_self(args) else {
        return Ok(Object::None);
    };
    let st = state_of_instance(&inst);
    let (pending, log, loop_) = {
        let s = st.borrow();
        (
            s.status == FutStatus::Pending && s.initialized,
            s.log_destroy_pending,
            s.loop_.clone(),
        )
    };
    if pending && log && !matches!(loop_, Object::None) {
        if let Ok(interp) = interp() {
            let self_obj = Object::Instance(inst.clone());
            let mut ctx = DictData::default();
            ctx.insert(
                DictKey(Object::from_static("message")),
                Object::from_static("Task was destroyed but it is pending!"),
            );
            ctx.insert(DictKey(Object::from_static("task")), self_obj);
            let source_tb = st.borrow().source_traceback.clone();
            if !matches!(source_tb, Object::None) {
                ctx.insert(DictKey(Object::from_static("source_traceback")), source_tb);
            }
            let _ = call_method(
                interp,
                &loop_,
                "call_exception_handler",
                &[Object::Dict(Rc::new(RefCell::new(ctx)))],
            );
        }
    } else {
        // Fall through to the Future finalizer for unretrieved exceptions.
        let _ = fut_del(args);
        return Ok(Object::None);
    }
    release_state(&inst);
    Ok(Object::None)
}

// ---- class construction ----

fn method(name: &'static str, body: fn(&[Object]) -> Result<Object, RuntimeError>) -> Object {
    Object::Builtin(Rc::new(BuiltinFn {
        name,
        binds_instance: true,
        call: Box::new(body),
        call_kw: None,
    }))
}

fn method_kw(
    name: &'static str,
    body: fn(&[Object], &[(String, Object)]) -> Result<Object, RuntimeError>,
) -> Object {
    Object::Builtin(Rc::new(BuiltinFn {
        name,
        binds_instance: true,
        call: Box::new(move |args| body(args, &[])),
        call_kw: Some(Box::new(body)),
    }))
}

fn install_getset(
    cls: &Rc<TypeObject>,
    name: &'static str,
    getter: fn(&[Object]) -> Result<Object, RuntimeError>,
    setter: Option<fn(&[Object]) -> Result<Object, RuntimeError>>,
) {
    let fset = match setter {
        Some(s) => Object::Builtin(Rc::new(BuiltinFn {
            name,
            binds_instance: true,
            call: Box::new(s),
            call_kw: None,
        })),
        None => Object::None,
    };
    let prop = Object::Property(Rc::new(PyProperty::new(
        Object::Builtin(Rc::new(BuiltinFn {
            name,
            binds_instance: true,
            call: Box::new(getter),
            call_kw: None,
        })),
        fset,
        Object::None,
        Object::None,
    )));
    crate::descr_registry::register(
        &prop,
        crate::descr_registry::DescrKind::GetSet,
        cls.clone(),
        name,
        None,
    );
    cls.dict
        .borrow_mut()
        .insert(DictKey(Object::from_static(name)), prop);
}

fn future_class() -> Rc<TypeObject> {
    static CELL: std::sync::OnceLock<Rc<TypeObject>> = std::sync::OnceLock::new();
    CELL.get_or_init(|| {
        let bt = crate::builtin_types::builtin_types();
        let mut dict = DictData::default();
        dict.insert(
            DictKey(Object::from_static("__module__")),
            Object::from_static("_asyncio"),
        );
        dict.insert(
            DictKey(Object::from_static("__init__")),
            method_kw("__init__", fut_init),
        );
        dict.insert(
            DictKey(Object::from_static("result")),
            method("result", fut_result),
        );
        dict.insert(
            DictKey(Object::from_static("exception")),
            method("exception", fut_exception),
        );
        dict.insert(
            DictKey(Object::from_static("set_result")),
            method("set_result", fut_set_result),
        );
        dict.insert(
            DictKey(Object::from_static("set_exception")),
            method("set_exception", fut_set_exception),
        );
        dict.insert(
            DictKey(Object::from_static("add_done_callback")),
            method_kw("add_done_callback", fut_add_done_callback),
        );
        dict.insert(
            DictKey(Object::from_static("remove_done_callback")),
            method("remove_done_callback", fut_remove_done_callback),
        );
        dict.insert(
            DictKey(Object::from_static("cancel")),
            method_kw("cancel", fut_cancel),
        );
        dict.insert(
            DictKey(Object::from_static("cancelled")),
            method("cancelled", fut_cancelled),
        );
        dict.insert(
            DictKey(Object::from_static("done")),
            method("done", fut_done),
        );
        dict.insert(
            DictKey(Object::from_static("get_loop")),
            method("get_loop", fut_get_loop),
        );
        dict.insert(
            DictKey(Object::from_static("_make_cancelled_error")),
            method("_make_cancelled_error", fut_make_cancelled_error),
        );
        dict.insert(
            DictKey(Object::from_static("__await__")),
            method("__await__", fut_await),
        );
        dict.insert(
            DictKey(Object::from_static("__iter__")),
            method("__iter__", fut_await),
        );
        dict.insert(
            DictKey(Object::from_static("__repr__")),
            method("__repr__", fut_repr),
        );
        dict.insert(
            DictKey(Object::from_static("__del__")),
            method("__del__", fut_del),
        );
        dict.insert(
            DictKey(Object::from_static("__class_getitem__")),
            method("__class_getitem__", class_getitem),
        );
        let cls = TypeObject::new_user("Future", vec![bt.object_.clone()], dict)
            .expect("Future class must linearise");
        install_getset(&cls, "_state", futprop_state, None);
        install_getset(&cls, "_loop", futprop_loop, None);
        install_getset(&cls, "_callbacks", futprop_callbacks, None);
        install_getset(&cls, "_result", futprop_result, None);
        install_getset(&cls, "_exception", futprop_exception, None);
        install_getset(
            &cls,
            "_log_traceback",
            futprop_log_traceback,
            Some(futprop_set_log_traceback),
        );
        install_getset(&cls, "_source_traceback", futprop_source_traceback, None);
        install_getset(
            &cls,
            "_cancel_message",
            futprop_cancel_message,
            Some(futprop_set_cancel_message),
        );
        install_getset(
            &cls,
            "_asyncio_future_blocking",
            futprop_blocking,
            Some(futprop_set_blocking),
        );
        cls
    })
    .clone()
}

fn task_class() -> Rc<TypeObject> {
    static CELL: std::sync::OnceLock<Rc<TypeObject>> = std::sync::OnceLock::new();
    CELL.get_or_init(|| {
        let mut dict = DictData::default();
        dict.insert(
            DictKey(Object::from_static("__module__")),
            Object::from_static("_asyncio"),
        );
        dict.insert(
            DictKey(Object::from_static("__init__")),
            method_kw("__init__", task_init),
        );
        dict.insert(
            DictKey(Object::from_static("cancel")),
            method_kw("cancel", task_cancel),
        );
        dict.insert(
            DictKey(Object::from_static("cancelling")),
            method("cancelling", task_cancelling),
        );
        dict.insert(
            DictKey(Object::from_static("uncancel")),
            method("uncancel", task_uncancel),
        );
        dict.insert(
            DictKey(Object::from_static("get_coro")),
            method("get_coro", task_get_coro),
        );
        dict.insert(
            DictKey(Object::from_static("get_context")),
            method("get_context", task_get_context),
        );
        dict.insert(
            DictKey(Object::from_static("get_name")),
            method("get_name", task_get_name),
        );
        dict.insert(
            DictKey(Object::from_static("set_name")),
            method("set_name", task_set_name),
        );
        dict.insert(
            DictKey(Object::from_static("set_result")),
            method("set_result", task_set_result_disallowed),
        );
        dict.insert(
            DictKey(Object::from_static("set_exception")),
            method("set_exception", task_set_exception_disallowed),
        );
        dict.insert(
            DictKey(Object::from_static("get_stack")),
            method_kw("get_stack", task_get_stack),
        );
        dict.insert(
            DictKey(Object::from_static("print_stack")),
            method_kw("print_stack", task_print_stack),
        );
        dict.insert(
            DictKey(Object::from_static("__repr__")),
            method("__repr__", task_repr),
        );
        dict.insert(
            DictKey(Object::from_static("__del__")),
            method("__del__", task_del),
        );
        dict.insert(
            DictKey(Object::from_static("__class_getitem__")),
            method("__class_getitem__", class_getitem),
        );
        let cls = TypeObject::new_user("Task", vec![future_class()], dict)
            .expect("Task class must linearise");
        install_getset(&cls, "_coro", taskprop_coro, None);
        install_getset(&cls, "_fut_waiter", taskprop_fut_waiter, None);
        install_getset(&cls, "_must_cancel", taskprop_must_cancel, None);
        install_getset(
            &cls,
            "_log_destroy_pending",
            taskprop_log_destroy_pending,
            Some(taskprop_set_log_destroy_pending),
        );
        install_getset(&cls, "_num_cancels_requested", taskprop_num_cancels, None);
        cls
    })
    .clone()
}

// ---- module functions ----

fn modfn(name: &'static str, body: fn(&[Object]) -> Result<Object, RuntimeError>) -> Object {
    Object::Builtin(Rc::new(BuiltinFn {
        name,
        binds_instance: false,
        call: Box::new(body),
        call_kw: None,
    }))
}

fn mod_get_event_loop(_args: &[Object]) -> Result<Object, RuntimeError> {
    get_event_loop_impl(interp()?)
}

fn mod_get_running_loop(_args: &[Object]) -> Result<Object, RuntimeError> {
    get_running_loop_impl(interp()?)
}

fn mod_get_running_loop_raw(_args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(running_loop())
}

fn mod_set_running_loop_raw(args: &[Object]) -> Result<Object, RuntimeError> {
    set_running_loop(args.first().cloned().unwrap_or(Object::None));
    Ok(Object::None)
}

fn mod_current_task(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let interp = interp()?;
    let mut loop_ = args.first().cloned().unwrap_or(Object::None);
    for (k, v) in kwargs {
        if k == "loop" {
            loop_ = v.clone();
        }
    }
    let loop_ = if matches!(loop_, Object::None) {
        get_running_loop_impl(interp)?
    } else {
        loop_
    };
    let dict = current_tasks_dict();
    let key = DictKey(loop_);
    let out = dict.borrow().get(&key).cloned().unwrap_or(Object::None);
    Ok(out)
}

fn mod_register_task(args: &[Object]) -> Result<Object, RuntimeError> {
    let task = args
        .first()
        .cloned()
        .ok_or_else(|| type_error("_register_task() missing argument"))?;
    register_task_impl(interp()?, &task)?;
    Ok(Object::None)
}

fn mod_unregister_task(args: &[Object]) -> Result<Object, RuntimeError> {
    let task = args
        .first()
        .cloned()
        .ok_or_else(|| type_error("_unregister_task() missing argument"))?;
    unregister_task_impl(interp()?, &task)?;
    Ok(Object::None)
}

fn mod_register_eager_task(args: &[Object]) -> Result<Object, RuntimeError> {
    let task = args
        .first()
        .cloned()
        .ok_or_else(|| type_error("_register_eager_task() missing argument"))?;
    register_eager_task_impl(interp()?, &task)?;
    Ok(Object::None)
}

fn mod_unregister_eager_task(args: &[Object]) -> Result<Object, RuntimeError> {
    let task = args
        .first()
        .cloned()
        .ok_or_else(|| type_error("_unregister_eager_task() missing argument"))?;
    unregister_eager_task_impl(interp()?, &task)?;
    Ok(Object::None)
}

fn mod_enter_task(args: &[Object]) -> Result<Object, RuntimeError> {
    let loop_ = args
        .first()
        .cloned()
        .ok_or_else(|| type_error("_enter_task() missing loop"))?;
    let task = args
        .get(1)
        .cloned()
        .ok_or_else(|| type_error("_enter_task() missing task"))?;
    enter_task_impl(interp()?, &loop_, &task)?;
    Ok(Object::None)
}

fn mod_leave_task(args: &[Object]) -> Result<Object, RuntimeError> {
    let loop_ = args
        .first()
        .cloned()
        .ok_or_else(|| type_error("_leave_task() missing loop"))?;
    let task = args
        .get(1)
        .cloned()
        .ok_or_else(|| type_error("_leave_task() missing task"))?;
    leave_task_impl(interp()?, &loop_, &task)?;
    Ok(Object::None)
}

fn mod_swap_current_task(args: &[Object]) -> Result<Object, RuntimeError> {
    let loop_ = args
        .first()
        .cloned()
        .ok_or_else(|| type_error("_swap_current_task() missing loop"))?;
    let task = args.get(1).cloned().unwrap_or(Object::None);
    Ok(swap_current_task_impl(&loop_, &task))
}

// ---- module entry ----

pub fn build(_cache: &ModuleCache) -> Rc<PyModule> {
    register_gc_hooks();
    let dict = Rc::new(RefCell::new(DictData::default()));
    {
        let mut d = dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("Future")),
            Object::Type(future_class()),
        );
        d.insert(
            DictKey(Object::from_static("Task")),
            Object::Type(task_class()),
        );
        d.insert(
            DictKey(Object::from_static("get_event_loop")),
            modfn("get_event_loop", mod_get_event_loop),
        );
        d.insert(
            DictKey(Object::from_static("get_running_loop")),
            modfn("get_running_loop", mod_get_running_loop),
        );
        d.insert(
            DictKey(Object::from_static("_get_running_loop")),
            modfn("_get_running_loop", mod_get_running_loop_raw),
        );
        d.insert(
            DictKey(Object::from_static("_set_running_loop")),
            modfn("_set_running_loop", mod_set_running_loop_raw),
        );
        d.insert(
            DictKey(Object::from_static("current_task")),
            Object::Builtin(Rc::new(BuiltinFn {
                name: "current_task",
                binds_instance: false,
                call: Box::new(|args| mod_current_task(args, &[])),
                call_kw: Some(Box::new(mod_current_task)),
            })),
        );
        d.insert(
            DictKey(Object::from_static("_register_task")),
            modfn("_register_task", mod_register_task),
        );
        d.insert(
            DictKey(Object::from_static("_unregister_task")),
            modfn("_unregister_task", mod_unregister_task),
        );
        d.insert(
            DictKey(Object::from_static("_register_eager_task")),
            modfn("_register_eager_task", mod_register_eager_task),
        );
        d.insert(
            DictKey(Object::from_static("_unregister_eager_task")),
            modfn("_unregister_eager_task", mod_unregister_eager_task),
        );
        d.insert(
            DictKey(Object::from_static("_enter_task")),
            modfn("_enter_task", mod_enter_task),
        );
        d.insert(
            DictKey(Object::from_static("_leave_task")),
            modfn("_leave_task", mod_leave_task),
        );
        d.insert(
            DictKey(Object::from_static("_swap_current_task")),
            modfn("_swap_current_task", mod_swap_current_task),
        );
        d.insert(
            DictKey(Object::from_static("_current_tasks")),
            Object::Dict(current_tasks_dict()),
        );
    }
    // The task registries shared with `tasks.py`: a `weakref.WeakSet` for
    // scheduled tasks (so leaked tasks don't accumulate) and a plain `set`
    // for eager ones — created through the interpreter so they are real
    // Python objects the frozen helpers can iterate.
    if let Ok(interp) = interp() {
        let scheduled = import_attr(interp, "weakref", "WeakSet")
            .and_then(|ws| call(interp, &ws, &[]))
            .unwrap_or(Object::None);
        let eager = {
            let set_cls = interp
                .builtins_dict()
                .borrow()
                .get(&DictKey(Object::from_static("set")))
                .cloned();
            match set_cls {
                Some(cls) => call(interp, &cls, &[]).unwrap_or(Object::None),
                None => Object::None,
            }
        };
        let _ = scheduled_tasks_obj().set(scheduled.clone());
        let _ = eager_tasks_obj().set(eager.clone());
        let mut d = dict.borrow_mut();
        d.insert(DictKey(Object::from_static("_scheduled_tasks")), scheduled);
        d.insert(DictKey(Object::from_static("_eager_tasks")), eager);
    }
    Rc::new(PyModule {
        name: "_asyncio".to_owned(),
        filename: None,
        dict,
    })
}

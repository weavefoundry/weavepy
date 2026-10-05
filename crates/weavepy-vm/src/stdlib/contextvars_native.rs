//! `_weave_contextvars`: native bodies for the frozen `contextvars`
//! classes (PEP 567), CPython's `Python/context.c` in spirit.
//!
//! The classes stay Python classes with `__slots__` (their reprs, the
//! `Mapping` surface of `Context`, `__class_getitem__` and the
//! subclassing guard are rare paths), and `contextvars.install` hands
//! them to [`install`]. The hot operations are native: `ContextVar.get`,
//! `set` and `reset`, `Context.run` and `copy`, and `copy_context`.
//! asyncio calls them for every task step and every scheduled callback.
//!
//! A context's mapping is a `dict` in its `_data` slot, shared between
//! copies and copied on the first write while shared (CPython shares an
//! immutable HAMT instead): `copy_context()` is O(1), and a `set` in a
//! context whose mapping no copy shares mutates it in place. The
//! mapping's strong count tells whether it's shared; a transient
//! reference held elsewhere only costs a spurious copy.
//!
//! Each OS thread has its own current context, created empty on first
//! use (a new thread doesn't inherit its creator's values). Greenlets
//! swap it through [`take_current`] and [`set_current`].

use std::sync::atomic::{AtomicBool, Ordering};

use crate::error::{runtime_error, type_error, value_error, RuntimeError};
use crate::import::ModuleCache;
use crate::object::{BuiltinFn, DictData, DictKey, Object, PyModule};
use crate::shared_value::SharedSlice;
use crate::sync::{Rc, RefCell};
use crate::types::{PyInstance, SlotStorage, TypeObject};

// Slot positions (hints; the name is always checked).
const CTX_DATA: usize = 0;
const CTX_ENTERED: usize = 1;
const CTX_PREV: usize = 2;
const VAR_DEFAULT: usize = 1;
const TOK_VAR: usize = 0;
const TOK_OLD: usize = 1;
const TOK_USED: usize = 2;
const TOK_CTX: usize = 3;

/// The installed classes and the slot layouts natively built instances
/// share.
pub(crate) struct State {
    context_cls: Rc<TypeObject>,
    var_cls: Rc<TypeObject>,
    token_cls: Rc<TypeObject>,
    /// `Token.MISSING`.
    missing: Object,
    context_layout: SharedSlice<DictKey>,
    token_layout: SharedSlice<DictKey>,
}

static STATE: parking_lot::RwLock<Option<Rc<State>>> = parking_lot::RwLock::new(None);

fn state() -> Result<Rc<State>, RuntimeError> {
    STATE
        .read()
        .clone()
        .ok_or_else(|| runtime_error("contextvars is not initialized"))
}

thread_local! {
    /// The thread's current context (`None` until first used). Never
    /// dropped at thread exit: the interpreter may be gone by then.
    static CURRENT: std::mem::ManuallyDrop<std::cell::RefCell<Object>> =
        const { std::mem::ManuallyDrop::new(std::cell::RefCell::new(Object::None)) };
}

/// The context-switch watcher (`_testcapi`'s `PyContext_AddWatcher`
/// fixture), called with the new current context after every switch.
static SWITCH_HOOK: parking_lot::Mutex<Option<Object>> = parking_lot::Mutex::new(None);
static HOOK_SET: AtomicBool = AtomicBool::new(false);

fn interp<'a>() -> Result<&'a mut crate::Interpreter, RuntimeError> {
    let ptr = crate::vm_singletons::current_interpreter_ptr()
        .ok_or_else(|| runtime_error("contextvars: no running interpreter"))?;
    // SAFETY: published by an enclosing VM frame still live on this
    // thread; the GIL keeps the access exclusive.
    Ok(unsafe { &mut *ptr })
}

fn call(f: &Object, args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let interp = interp()?;
    let globals = interp.builtins_dict();
    interp.call(f, args, kwargs, &globals)
}

fn repr(obj: &Object) -> String {
    match interp() {
        Ok(interp) => {
            let globals = interp.builtins_dict();
            interp
                .repr_of(obj, &globals)
                .unwrap_or_else(|_| format!("<{} object>", obj.type_name()))
        }
        Err(_) => format!("<{} object>", obj.type_name()),
    }
}

fn fire_switch_hook(ctx: &Object) -> Result<(), RuntimeError> {
    if !HOOK_SET.load(Ordering::Relaxed) {
        return Ok(());
    }
    let hook = SWITCH_HOOK.lock().clone();
    match hook {
        Some(h) => call(&h, std::slice::from_ref(ctx), &[]).map(drop),
        None => Ok(()),
    }
}

// ---------------------------------------------------------------------
// Instances.

fn is_exact(obj: &Object, cls: &Rc<TypeObject>) -> bool {
    matches!(obj, Object::Instance(i) if Rc::ptr_eq(&i.class.borrow(), cls))
}

fn new_dict() -> Object {
    let d = Object::Dict(Rc::new(RefCell::new(DictData::default())));
    crate::gc_trace::track_built(&d);
    d
}

/// A natively built `Context` over `data` (shared, not copied).
fn new_context(st: &State, data: Object) -> Object {
    let mut inst = PyInstance::new(st.context_cls.clone());
    inst.slots = RefCell::new(SlotStorage::from_layout(
        st.context_layout.clone(),
        vec![data, Object::Bool(false), Object::None],
    ));
    let obj = Object::Instance(Rc::new(inst));
    crate::gc_trace::track(&obj);
    obj
}

fn new_token(st: &State, var: Object, old: Object, ctx: Object) -> Object {
    let mut inst = PyInstance::new(st.token_cls.clone());
    inst.slots = RefCell::new(SlotStorage::from_layout(
        st.token_layout.clone(),
        vec![var, old, Object::Bool(false), ctx],
    ));
    let obj = Object::Instance(Rc::new(inst));
    crate::gc_trace::track(&obj);
    obj
}

/// A context's mapping (the `dict` in its `_data` slot).
fn data_of(ctx: &PyInstance) -> Option<Rc<RefCell<DictData>>> {
    match ctx.slots.borrow().get_hinted(CTX_DATA, "_data") {
        Some(Object::Dict(d)) => Some(d.clone()),
        _ => None,
    }
}

/// The thread's current context, created (empty) on first use.
fn current(st: &State) -> Object {
    CURRENT.with(|c| {
        let mut c = c.borrow_mut();
        if matches!(*c, Object::None) {
            *c = new_context(st, new_dict());
        }
        c.clone()
    })
}

/// The thread's current context, or `None` before one exists. For
/// greenlet switches: the slot is emptied.
pub(crate) fn take_current() -> Object {
    CURRENT.with(|c| std::mem::replace(&mut *c.borrow_mut(), Object::None))
}

/// The thread's current context without creating one (`None` if none).
pub(crate) fn peek_current() -> Object {
    CURRENT.with(|c| c.borrow().clone())
}

/// Make `ctx` (a context or `None`) the thread's current one.
pub(crate) fn set_current(ctx: Object) {
    let old = CURRENT.with(|c| std::mem::replace(&mut *c.borrow_mut(), ctx));
    drop(old);
}

/// `contextvars.copy_context()`, for native callers (asyncio).
pub(crate) fn copy_current() -> Result<Object, RuntimeError> {
    let st = state()?;
    let cur = current(&st);
    let Object::Instance(i) = &cur else {
        unreachable!("the current context is an instance")
    };
    let data = data_of(i).unwrap_or_else(new_dict_raw);
    Ok(new_context(&st, Object::Dict(data)))
}

fn new_dict_raw() -> Rc<RefCell<DictData>> {
    match new_dict() {
        Object::Dict(d) => d,
        _ => unreachable!(),
    }
}

/// Whether `obj` is a `contextvars.Context`.
pub(crate) fn is_context(obj: &Object) -> bool {
    match STATE.read().as_ref() {
        Some(st) => is_exact(obj, &st.context_cls),
        None => false,
    }
}

// ---------------------------------------------------------------------
// ContextVar.

fn var_receiver<'a>(
    args: &'a [Object],
    st: &State,
    method: &str,
) -> Result<&'a Rc<PyInstance>, RuntimeError> {
    match args.first() {
        Some(Object::Instance(i)) if Rc::ptr_eq(&i.class.borrow(), &st.var_cls) => Ok(i),
        Some(other) => Err(not_applicable(method, "ContextVar", other)),
        None => Err(type_error(format!(
            "unbound method ContextVar.{method}() needs an argument"
        ))),
    }
}

fn not_applicable(method: &str, cls: &str, obj: &Object) -> RuntimeError {
    let tname = match obj {
        Object::Instance(i) => i.cls().name.clone(),
        other => other.type_name().to_owned(),
    };
    type_error(format!(
        "descriptor '{method}' for 'contextvars.{cls}' objects doesn't apply to a '{tname}' object"
    ))
}

/// `ContextVar.get([default])`.
fn var_get(args: &[Object]) -> Result<Object, RuntimeError> {
    let st = state()?;
    let var = var_receiver(args, &st, "get")?;
    if args.len() > 2 {
        return Err(type_error(format!(
            "get expected at most 1 argument, got {}",
            args.len() - 1
        )));
    }
    let key = DictKey(args[0].clone());
    let found = CURRENT.with(|c| {
        let c = c.borrow();
        match &*c {
            Object::Instance(ctx) => data_of(ctx).and_then(|d| d.borrow().get(&key).cloned()),
            _ => None,
        }
    });
    if let Some(v) = found {
        return Ok(v);
    }
    if let Some(d) = args.get(1) {
        return Ok(d.clone());
    }
    let default = var
        .slots
        .borrow()
        .get_hinted(VAR_DEFAULT, "_default")
        .cloned()
        .unwrap_or(Object::None);
    if !default.is_same(&st.missing) {
        return Ok(default);
    }
    Err(RuntimeError::PyException(crate::error::PyException::new(
        crate::builtin_types::make_exception_with_object("LookupError", args[0].clone()),
    )))
}

/// Write `var = value` into `ctx`'s mapping, copying it first if another
/// context shares it. Returns the previous value (or `missing`).
fn ctx_store(ctx: &PyInstance, var: &Object, value: Option<Object>, missing: &Object) -> Object {
    let mut slots = ctx.slots.borrow_mut();
    let Some(slot) = slots.get_hinted_mut(CTX_DATA, "_data") else {
        return missing.clone();
    };
    let shared = match slot {
        Object::Dict(d) => Rc::strong_count(d) > 1,
        _ => true,
    };
    let mut released = None;
    if shared {
        let copy = match slot {
            Object::Dict(d) => d.borrow().clone(),
            _ => DictData::default(),
        };
        let fresh = Object::Dict(Rc::new(RefCell::new(copy)));
        crate::gc_trace::track_built(&fresh);
        released = Some(std::mem::replace(slot, fresh));
    }
    let Object::Dict(d) = slot else {
        unreachable!("just stored a dict")
    };
    let key = DictKey(var.clone());
    let old = match value {
        Some(v) => d.borrow_mut().insert(key, v),
        None => d.borrow_mut().shift_remove(&key),
    };
    drop(slots);
    if released.is_some() {
        drop(released);
        crate::gc_trace::mark_maybe_dead();
    }
    old.unwrap_or_else(|| missing.clone())
}

/// `ContextVar.set(value)`.
fn var_set(args: &[Object]) -> Result<Object, RuntimeError> {
    let st = state()?;
    var_receiver(args, &st, "set")?;
    let [var, value] = args else {
        return Err(type_error(format!(
            "ContextVar.set() takes exactly one argument ({} given)",
            args.len().saturating_sub(1)
        )));
    };
    let ctx = current(&st);
    let Object::Instance(ci) = &ctx else {
        unreachable!("the current context is an instance")
    };
    let old = ctx_store(ci, var, Some(value.clone()), &st.missing);
    Ok(new_token(&st, var.clone(), old, ctx.clone()))
}

/// `ContextVar.reset(token)`.
fn var_reset(args: &[Object]) -> Result<Object, RuntimeError> {
    let st = state()?;
    var_receiver(args, &st, "reset")?;
    let [var, token] = args else {
        return Err(type_error(format!(
            "ContextVar.reset() takes exactly one argument ({} given)",
            args.len().saturating_sub(1)
        )));
    };
    if !is_exact(token, &st.token_cls) {
        return Err(type_error(format!(
            "expected an instance of Token, got {}",
            repr(token)
        )));
    }
    let Object::Instance(ti) = token else {
        unreachable!()
    };
    let (tvar, told, tused, tctx) = {
        let s = ti.slots.borrow();
        (
            s.get_hinted(TOK_VAR, "_var")
                .cloned()
                .unwrap_or(Object::None),
            s.get_hinted(TOK_OLD, "_old")
                .cloned()
                .unwrap_or(Object::None),
            s.get_hinted(TOK_USED, "_used")
                .is_some_and(Object::is_truthy),
            s.get_hinted(TOK_CTX, "_ctx")
                .cloned()
                .unwrap_or(Object::None),
        )
    };
    if tused {
        return Err(runtime_error(format!(
            "{} has already been used once",
            repr(token)
        )));
    }
    if !tvar.is_same(var) {
        return Err(value_error(format!(
            "{} was created by a different ContextVar",
            repr(token)
        )));
    }
    let ctx = current(&st);
    if !tctx.is_same(&ctx) {
        return Err(value_error(format!(
            "{} was created in a different Context",
            repr(token)
        )));
    }
    if let Some(slot) = ti.slots.borrow_mut().get_hinted_mut(TOK_USED, "_used") {
        *slot = Object::Bool(true);
    }
    let Object::Instance(ci) = &ctx else {
        unreachable!()
    };
    let value = if told.is_same(&st.missing) {
        None
    } else {
        Some(told)
    };
    let prev = ctx_store(ci, var, value, &st.missing);
    drop(prev);
    crate::gc_trace::mark_maybe_dead();
    Ok(Object::None)
}

// ---------------------------------------------------------------------
// Context.

fn ctx_receiver<'a>(
    args: &'a [Object],
    st: &State,
    method: &str,
) -> Result<&'a Rc<PyInstance>, RuntimeError> {
    match args.first() {
        Some(Object::Instance(i)) if Rc::ptr_eq(&i.class.borrow(), &st.context_cls) => Ok(i),
        Some(other) => Err(not_applicable(method, "Context", other)),
        None => Err(type_error(format!(
            "unbound method Context.{method}() needs an argument"
        ))),
    }
}

/// Mark `ctx` entered and make it current; returns the previous current
/// context. Errors if it's already entered.
fn enter(ctx_obj: &Object, ctx: &PyInstance) -> Result<Object, RuntimeError> {
    {
        let mut slots = ctx.slots.borrow_mut();
        match slots.get_hinted_mut(CTX_ENTERED, "_entered") {
            Some(Object::Bool(false)) | None => {}
            Some(slot) if !slot.is_truthy() => {}
            Some(_) => {
                drop(slots);
                return Err(runtime_error(format!(
                    "cannot enter context: {} is already entered",
                    repr(ctx_obj)
                )));
            }
        }
        match slots.get_hinted_mut(CTX_ENTERED, "_entered") {
            Some(slot) => *slot = Object::Bool(true),
            None => {
                slots.insert("_entered", Object::Bool(true));
            }
        }
    }
    Ok(CURRENT.with(|c| std::mem::replace(&mut *c.borrow_mut(), ctx_obj.clone())))
}

/// Undo [`enter`]: restore `prev` as the current context.
fn exit(ctx: &PyInstance, prev: Object) {
    if let Some(slot) = ctx
        .slots
        .borrow_mut()
        .get_hinted_mut(CTX_ENTERED, "_entered")
    {
        *slot = Object::Bool(false);
    }
    let me = CURRENT.with(|c| std::mem::replace(&mut *c.borrow_mut(), prev));
    drop(me);
}

/// `Context.run(callable, *args, **kwargs)`.
fn ctx_run(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let st = state()?;
    let ctx = ctx_receiver(args, &st, "run")?;
    let Some(f) = args.get(1) else {
        return Err(type_error("run() missing 1 required positional argument"));
    };
    run_in(&args[0], ctx, f, &args[2..], kwargs)
}

fn run_in(
    ctx_obj: &Object,
    ctx: &PyInstance,
    f: &Object,
    args: &[Object],
    kwargs: &[(String, Object)],
) -> Result<Object, RuntimeError> {
    let prev = enter(ctx_obj, ctx)?;
    let hooked = HOOK_SET.load(Ordering::Relaxed);
    let hook_prev = if hooked { prev.clone() } else { Object::None };
    let mut result = match fire_switch_hook(ctx_obj) {
        Ok(()) => call(f, args, kwargs),
        Err(e) => Err(e),
    };
    exit(ctx, prev);
    if hooked {
        if let Err(e) = fire_switch_hook(&hook_prev) {
            if result.is_ok() {
                result = Err(e);
            }
        }
    }
    result
}

/// `ctx.run(f, *args)` for native callers (asyncio): `ctx` must be a
/// `Context`.
pub(crate) fn run_in_context(
    ctx_obj: &Object,
    f: &Object,
    args: &[Object],
) -> Result<Object, RuntimeError> {
    match ctx_obj {
        Object::Instance(ctx) if is_context(ctx_obj) => run_in(ctx_obj, ctx, f, args, &[]),
        _ => {
            let interp = interp()?;
            let run = interp.load_attr_public(ctx_obj, "run")?;
            let mut all = Vec::with_capacity(args.len() + 1);
            all.push(f.clone());
            all.extend_from_slice(args);
            call(&run, &all, &[])
        }
    }
}

/// `Context.copy()`.
fn ctx_copy(args: &[Object]) -> Result<Object, RuntimeError> {
    let st = state()?;
    let ctx = ctx_receiver(args, &st, "copy")?;
    let data = data_of(ctx).unwrap_or_else(new_dict_raw);
    Ok(new_context(&st, Object::Dict(data)))
}

/// `Context()`: the `_data` slot of a context built by `__init__`.
fn new_mapping(_args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(new_dict())
}

/// `copy_context()`.
fn copy_context(args: &[Object]) -> Result<Object, RuntimeError> {
    if !args.is_empty() {
        return Err(type_error(format!(
            "_contextvars.copy_context() takes no arguments ({} given)",
            args.len()
        )));
    }
    copy_current()
}

/// `_enter_context(ctx)` (`PyContext_Enter`): the previous context is
/// kept in the context's `_prev` slot, as CPython keeps `ctx_prev`.
fn enter_context(args: &[Object]) -> Result<Object, RuntimeError> {
    let st = state()?;
    let ctx_obj = args.first().cloned().unwrap_or(Object::None);
    let Object::Instance(ctx) = &ctx_obj else {
        return Err(type_error(format!(
            "a Context was expected, got {}",
            repr(&ctx_obj)
        )));
    };
    if !is_exact(&ctx_obj, &st.context_cls) {
        return Err(type_error(format!(
            "a Context was expected, got {}",
            repr(&ctx_obj)
        )));
    }
    let prev = enter(&ctx_obj, ctx)?;
    if let Some(slot) = ctx.slots.borrow_mut().get_hinted_mut(CTX_PREV, "_prev") {
        *slot = prev;
    }
    fire_switch_hook(&ctx_obj)?;
    Ok(Object::None)
}

/// `_exit_context(ctx)` (`PyContext_Exit`).
fn exit_context(args: &[Object]) -> Result<Object, RuntimeError> {
    let st = state()?;
    let ctx_obj = args.first().cloned().unwrap_or(Object::None);
    let Object::Instance(ctx) = &ctx_obj else {
        return Err(type_error(format!(
            "a Context was expected, got {}",
            repr(&ctx_obj)
        )));
    };
    if !is_exact(&ctx_obj, &st.context_cls) {
        return Err(type_error(format!(
            "a Context was expected, got {}",
            repr(&ctx_obj)
        )));
    }
    let entered = ctx
        .slots
        .borrow()
        .get_hinted(CTX_ENTERED, "_entered")
        .is_some_and(Object::is_truthy);
    if !entered {
        return Err(runtime_error(format!(
            "cannot exit context: {} has not been entered",
            repr(&ctx_obj)
        )));
    }
    let prev = match ctx.slots.borrow_mut().get_hinted_mut(CTX_PREV, "_prev") {
        Some(slot) => std::mem::replace(slot, Object::None),
        None => Object::None,
    };
    let hook_prev = prev.clone();
    exit(ctx, prev);
    fire_switch_hook(&hook_prev)?;
    Ok(Object::None)
}

fn set_switch_hook(args: &[Object]) -> Result<Object, RuntimeError> {
    let hook = args.first().cloned().unwrap_or(Object::None);
    let mut slot = SWITCH_HOOK.lock();
    HOOK_SET.store(!matches!(hook, Object::None), Ordering::Relaxed);
    *slot = if matches!(hook, Object::None) {
        None
    } else {
        Some(hook)
    };
    Ok(Object::None)
}

/// `clear_context_stack` in `Modules/_testcapi/watchers.c`: drop the
/// thread's base context so the next switch reports `None` on exit.
fn clear_context_stack(_args: &[Object]) -> Result<Object, RuntimeError> {
    let cur = peek_current();
    if let Object::Instance(ctx) = &cur {
        let entered = ctx
            .slots
            .borrow()
            .get_hinted(CTX_ENTERED, "_entered")
            .is_some_and(Object::is_truthy);
        if entered {
            return Err(runtime_error("must first exit all non-base contexts"));
        }
        set_current(Object::None);
    }
    Ok(Object::None)
}

/// `current_context()`: the thread's current context, created if needed.
fn current_context(_args: &[Object]) -> Result<Object, RuntimeError> {
    let st = state()?;
    Ok(current(&st))
}

/// `install(Context, ContextVar, Token, MISSING)`, called at the end of
/// the `contextvars` module body.
fn install(args: &[Object]) -> Result<Object, RuntimeError> {
    let [Object::Type(context_cls), Object::Type(var_cls), Object::Type(token_cls), missing] = args
    else {
        return Err(type_error("install(Context, ContextVar, Token, MISSING)"));
    };
    let layout = |names: &[&str]| -> SharedSlice<DictKey> {
        names
            .iter()
            .map(|n| crate::types::slot_key(n))
            .collect::<Vec<_>>()
            .into()
    };
    let st = State {
        context_cls: context_cls.clone(),
        var_cls: var_cls.clone(),
        token_cls: token_cls.clone(),
        missing: missing.clone(),
        context_layout: layout(&["_data", "_entered", "_prev"]),
        token_layout: layout(&["_var", "_old", "_used", "_ctx"]),
    };
    let old = STATE.write().replace(Rc::new(st));
    drop(old);
    Ok(Object::None)
}

pub fn build(_cache: &ModuleCache) -> Rc<PyModule> {
    let dict = Rc::new(RefCell::new(DictData::default()));
    {
        let mut d = dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("__name__")),
            Object::from_static("_weave_contextvars"),
        );
        let mut reg = |name: &'static str,
                       binds: bool,
                       f: fn(&[Object]) -> Result<Object, RuntimeError>|
         -> Rc<BuiltinFn> {
            let b = Rc::new(BuiltinFn {
                name,
                binds_instance: binds,
                call: Box::new(f),
                call_kw: None,
            });
            d.insert(
                DictKey(Object::from_static(name)),
                Object::Builtin(b.clone()),
            );
            b
        };
        let get = reg("get", true, var_get);
        reg("set", true, var_set);
        reg("reset", true, var_reset);
        reg("copy", true, ctx_copy);
        reg("_new_mapping", false, new_mapping);
        reg("copy_context", false, copy_context);
        reg("_enter_context", false, enter_context);
        reg("_exit_context", false, exit_context);
        reg("_set_switch_hook", false, set_switch_hook);
        reg("_clear_context_stack", false, clear_context_stack);
        reg("_current_context", false, current_context);
        reg("install", false, install);
        // `get` runs no Python code and releases nothing.
        crate::leaf_builtins::register(&get);
        d.insert(
            DictKey(Object::from_static("run")),
            Object::Builtin(Rc::new(BuiltinFn {
                name: "run",
                binds_instance: true,
                call: Box::new(|args| ctx_run(args, &[])),
                call_kw: Some(Box::new(ctx_run)),
            })),
        );
    }
    Rc::new(PyModule {
        name: "_weave_contextvars".to_owned(),
        filename: None,
        dict,
    })
}

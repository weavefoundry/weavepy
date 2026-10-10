//! `_weave_asyncio`: native fast paths for the event loop's callback
//! queue.
//!
//! CPython runs `BaseEventLoop.call_soon` and `Handle._run` in Python;
//! together with the C `Future`/`Task` they're the bulk of every task
//! step. The frozen `asyncio/events.py` and `asyncio/base_events.py`
//! (verbatim CPython otherwise) hand their classes to [`install_handle`]
//! and [`install_loop`] at the end of their module bodies:
//!
//! - `Handle._run` becomes native: it runs the callback in the handle's
//!   context and hands any exception other than `SystemExit` and
//!   `KeyboardInterrupt` to the Python `except` body it replaced
//!   (`events._handle_run_failed`).
//! - `BaseEventLoop.call_soon` becomes native. For a loop whose
//!   `_call_soon`, `_check_closed` and `get_debug` are the stock methods,
//!   that isn't closed and isn't in debug mode, it builds the `Handle`
//!   natively and appends it to `_ready`; anything else calls the Python
//!   method it replaced, so observable behavior is unchanged.
//!
//! The native `Future`/`Task` (`asyncio_mod`) schedule their callbacks
//! through [`call_soon`], which takes the same fast path without a
//! Python-level call when the loop resolves `call_soon` to the native
//! method.

use crate::error::{type_error, RuntimeError};
use crate::import::ModuleCache;
use crate::object::{BuiltinFn, DictData, DictKey, Object, PyModule};
use crate::shared_value::SharedSlice;
use crate::sync::{Rc, RefCell};
use crate::types::{PyInstance, SlotStorage, TypeObject};

// The slot order `Handle.__init__` assigns (hints for the lookups; the
// natively built handles share it).
const H_CONTEXT: usize = 0;
const H_CALLBACK: usize = 2;
const H_ARGS: usize = 3;
const HANDLE_SLOTS: [&str; 7] = [
    "_context",
    "_loop",
    "_callback",
    "_args",
    "_cancelled",
    "_repr",
    "_source_traceback",
];

struct HandleState {
    cls: Rc<TypeObject>,
    layout: SharedSlice<DictKey>,
    /// The Python `Handle._run` the native replaced.
    run_py: Object,
    /// `events._handle_run_failed(handle, exc)`: `_run`'s `except` body.
    run_failed: Object,
}

struct LoopState {
    cls: Rc<TypeObject>,
    /// The stock methods the fast path requires, and the Python
    /// `call_soon` the native replaced.
    call_soon_py: Object,
    /// The native `call_soon` installed on the class.
    call_soon_native: std::sync::OnceLock<Object>,
    call_soon_inner: Object,
    check_closed: Object,
    get_debug: Object,
    /// The Python `create_task` and `create_future` the natives replaced.
    create_task_py: Object,
    create_future_py: Object,
    /// `asyncio.base_events`'s namespace (its `tasks` and `futures`
    /// globals name the classes `create_task`/`create_future` build).
    globals: Rc<RefCell<DictData>>,
}

static HANDLE: parking_lot::RwLock<Option<Rc<HandleState>>> = parking_lot::RwLock::new(None);
static LOOP: parking_lot::RwLock<Option<Rc<LoopState>>> = parking_lot::RwLock::new(None);

fn interp<'a>() -> Result<&'a mut crate::Interpreter, RuntimeError> {
    let ptr = crate::vm_singletons::current_interpreter_ptr()
        .ok_or_else(|| crate::error::runtime_error("asyncio: no running interpreter"))?;
    // SAFETY: published by an enclosing VM frame still live on this
    // thread; the GIL keeps the access exclusive.
    Ok(unsafe { &mut *ptr })
}

fn call(f: &Object, args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let interp = interp()?;
    let globals = interp.builtins_dict();
    interp.call(f, args, kwargs, &globals)
}

fn same(a: &Object, b: &Object) -> bool {
    a.is_same(b)
}

fn is_exc(obj: &Object, name: &str) -> bool {
    let Object::Instance(inst) = obj else {
        return false;
    };
    match crate::builtin_types::builtin_types().by_name(name) {
        Some(cls) => inst.cls().is_subclass_of(&cls),
        None => false,
    }
}

// ---------------------------------------------------------------------
// Handle.

/// `Handle._run(self)`, for the `Handle` class `hs` describes.
fn handle_run(hs: &HandleState, args: &[Object]) -> Result<Object, RuntimeError> {
    let Some(Object::Instance(h)) = args.first() else {
        return call(&hs.run_py, args, &[]);
    };
    let (ctx, cb, cb_args) = {
        let s = h.slots.borrow();
        (
            s.get_hinted(H_CONTEXT, "_context").cloned(),
            s.get_hinted(H_CALLBACK, "_callback").cloned(),
            s.get_hinted(H_ARGS, "_args").cloned(),
        )
    };
    let (Some(ctx), Some(cb), Some(Object::Tuple(cb_args))) = (ctx, cb, cb_args) else {
        // A cancelled or malformed handle: the Python body's errors.
        return call(&hs.run_py, args, &[]);
    };
    match crate::stdlib::contextvars_native::run_in_context(&ctx, &cb, &cb_args) {
        Ok(_) => Ok(Object::None),
        Err(RuntimeError::PyException(pe))
            if !is_exc(&pe.instance, "SystemExit")
                && !is_exc(&pe.instance, "KeyboardInterrupt") =>
        {
            call(&hs.run_failed, &[args[0].clone(), pe.instance.clone()], &[])?;
            Ok(Object::None)
        }
        Err(e) => Err(e),
    }
}

/// A `Handle(callback, args, loop, context)` built natively for a loop
/// not in debug mode (so `_source_traceback` is `None`).
fn new_handle(hs: &HandleState, cb: Object, args: Object, loop_: Object, ctx: Object) -> Object {
    let mut inst = PyInstance::new(hs.cls.clone());
    inst.slots = RefCell::new(SlotStorage::from_layout(
        hs.layout.clone(),
        vec![
            ctx,
            loop_,
            cb,
            args,
            Object::Bool(false),
            Object::None,
            Object::None,
        ],
    ));
    let obj = Object::Instance(Rc::new(inst));
    crate::gc_trace::track(&obj);
    obj
}

/// `install_handle(Handle, run_failed)`.
fn install_handle(args: &[Object]) -> Result<Object, RuntimeError> {
    let [Object::Type(cls), run_failed] = args else {
        return Err(type_error("install_handle(Handle, run_failed)"));
    };
    let run_py = cls
        .dict
        .borrow()
        .get(&DictKey(Object::from_static("_run")))
        .cloned()
        .ok_or_else(|| type_error("Handle has no _run"))?;
    let layout: SharedSlice<DictKey> = HANDLE_SLOTS
        .iter()
        .map(|n| crate::types::slot_key(n))
        .collect::<Vec<_>>()
        .into();
    let st = Rc::new(HandleState {
        cls: cls.clone(),
        layout,
        run_py,
        run_failed: run_failed.clone(),
    });
    // The native holds its own class's state (a re-imported
    // `asyncio.events` installs a fresh one on its fresh class).
    let own = st.clone();
    let native = Object::Builtin(Rc::new(BuiltinFn {
        name: "_run",
        binds_instance: true,
        call: Box::new(move |args| handle_run(&own, args)),
        call_kw: None,
    }));
    cls.dict
        .borrow_mut()
        .insert(DictKey(Object::from_static("_run")), native);
    cls.bump_attr_version();
    drop(HANDLE.write().replace(st));
    Ok(Object::None)
}

// ---------------------------------------------------------------------
// BaseEventLoop.call_soon.

/// A name the fast paths look up: its Python hash, and where an instance
/// of the loop class last held it (see [`crate::inst_dict::PosMemo`]).
struct AttrName {
    name: &'static str,
    hash: i64,
    memo: crate::inst_dict::PosMemo,
}

impl AttrName {
    fn new(name: &'static str) -> Self {
        Self {
            name,
            hash: crate::object::py_str_hash(name),
            memo: crate::inst_dict::PosMemo::new(),
        }
    }
}

/// The instance attributes the fast paths read, and the module globals
/// they check.
struct Names {
    debug: AttrName,
    closed: AttrName,
    ready: AttrName,
    call_soon: AttrName,
    get_debug: AttrName,
    task_factory: AttrName,
    events: AttrName,
    handle: AttrName,
    tasks: AttrName,
    task: AttrName,
    futures: AttrName,
    future: AttrName,
}

fn names() -> &'static Names {
    static NAMES: std::sync::OnceLock<Names> = std::sync::OnceLock::new();
    NAMES.get_or_init(|| Names {
        debug: AttrName::new("_debug"),
        closed: AttrName::new("_closed"),
        ready: AttrName::new("_ready"),
        call_soon: AttrName::new("call_soon"),
        get_debug: AttrName::new("get_debug"),
        task_factory: AttrName::new("_task_factory"),
        events: AttrName::new("events"),
        handle: AttrName::new("Handle"),
        tasks: AttrName::new("tasks"),
        task: AttrName::new("Task"),
        futures: AttrName::new("futures"),
        future: AttrName::new("Future"),
    })
}

/// Instance attribute `n`, read without running code (a split layout
/// answers a never-set name from its filter, and a set one from the
/// position it last had).
fn inst_attr(inst: &PyInstance, n: &AttrName) -> Option<Object> {
    match inst.dict.published() {
        Some(d) => d
            .borrow()
            .get(&crate::object::StrKeyHashed {
                s: n.name,
                hash: n.hash,
            })
            .cloned(),
        None => {
            let split = inst.dict.split_cell().borrow();
            split.get_memo(n.name, n.hash, &n.memo).cloned()
        }
    }
}

/// A class (by address and attribute version, which no other class
/// state ever reuses) a check below already approved on this thread.
type Verified = std::cell::Cell<(usize, u64)>;

thread_local! {
    static STOCK_LOOP: Verified = const { std::cell::Cell::new((0, 0)) };
    static NATIVE_CALL_SOON: Verified = const { std::cell::Cell::new((0, 0)) };
    static NATIVE_GET_DEBUG: Verified = const { std::cell::Cell::new((0, 0)) };
    static NATIVE_DEQUE: Verified = const { std::cell::Cell::new((0, 0)) };
}

/// Whether `cls` passes `check`, remembering a pass in `cache`.
fn verified(
    cache: &'static std::thread::LocalKey<Verified>,
    cls: &TypeObject,
    check: impl FnOnce() -> bool,
) -> bool {
    let key = (std::ptr::from_ref(cls) as usize, cls.attr_version.get());
    if cache.with(Verified::get) == key {
        return true;
    }
    let ok = check();
    if ok {
        cache.with(|c| c.set(key));
    }
    ok
}

/// Whether `cls` resolves `name` to `expected`.
fn resolves_to(cls: &TypeObject, name: &str, expected: &Object) -> bool {
    cls.lookup(name).is_some_and(|v| same(&v, expected))
}

/// The `_ready` deque of a stock loop, if its `append` is the native one.
fn ready_deque(inst: &PyInstance) -> Option<Object> {
    let ready = inst_attr(inst, &names().ready)?;
    let Object::Instance(r) = &ready else {
        return None;
    };
    let ok = verified(&NATIVE_DEQUE, &r.class.borrow(), || {
        let rcls = r.cls();
        rcls.name == "deque"
            && matches!(rcls.lookup("append"), Some(Object::Builtin(b)) if b.name == "append")
    });
    ok.then_some(ready)
}

/// The native `call_soon` for a stock loop: `Ok(None)` when `loop_`
/// isn't one (the caller then takes the Python path).
fn fast_call_soon(
    ls: &LoopState,
    loop_: &Object,
    cb: &Object,
    cb_args: &[Object],
    ctx: &Object,
) -> Result<Option<Object>, RuntimeError> {
    if !plain_loop(ls, loop_) {
        return Ok(None);
    }
    let Object::Instance(li) = loop_ else {
        return Ok(None);
    };
    let Some(hs) = HANDLE.read().clone() else {
        return Ok(None);
    };
    // The class the loop's module would build (`events.Handle`, looked up
    // at call time: it may have been replaced, or come from another
    // import of `asyncio.events` than the one `hs` describes).
    let current = global_module_attr(&ls.globals, &names().events, &names().handle)
        .is_some_and(|h| matches!(h, Object::Type(t) if Rc::ptr_eq(&t, &hs.cls)));
    if !current {
        return Ok(None);
    }
    let Some(ready) = ready_deque(li) else {
        return Ok(None);
    };
    let ctx = if matches!(ctx, Object::None) {
        crate::stdlib::contextvars_native::copy_current()?
    } else {
        ctx.clone()
    };
    let handle = new_handle(
        &hs,
        cb.clone(),
        Object::new_tuple(cb_args.to_vec()),
        loop_.clone(),
        ctx,
    );
    crate::stdlib::collections_native::deque_append(&[ready, handle.clone()])?;
    Ok(Some(handle))
}

/// `BaseEventLoop.call_soon(self, callback, *args, context=None)`.
fn loop_call_soon_method(
    ls: &LoopState,
    args: &[Object],
    kwargs: &[(String, Object)],
) -> Result<Object, RuntimeError> {
    let mut ctx = Object::None;
    let mut plain = args.len() >= 2;
    for (k, v) in kwargs {
        if k == "context" {
            ctx = v.clone();
        } else {
            plain = false;
        }
    }
    if plain {
        if let Some(h) = fast_call_soon(ls, &args[0], &args[1], &args[2..], &ctx)? {
            return Ok(h);
        }
    }
    call(&ls.call_soon_py, args, kwargs)
}

/// `loop.call_soon(cb, *args, context=ctx)` for native callers.
pub(crate) fn call_soon(
    loop_: &Object,
    cb: Object,
    cb_args: &[Object],
    ctx: Object,
) -> Result<Object, RuntimeError> {
    let ls = LOOP.read().clone();
    if let (Some(ls), Object::Instance(li)) = (&ls, loop_) {
        let native = verified(&NATIVE_CALL_SOON, &li.class.borrow(), || {
            ls.call_soon_native
                .get()
                .is_some_and(|n| resolves_to(&li.cls(), "call_soon", n))
        }) && inst_attr(li, &names().call_soon).is_none();
        if native {
            if let Some(h) = fast_call_soon(ls, loop_, &cb, cb_args, &ctx)? {
                return Ok(h);
            }
            let mut all = Vec::with_capacity(cb_args.len() + 2);
            all.push(loop_.clone());
            all.push(cb);
            all.extend_from_slice(cb_args);
            return call(&ls.call_soon_py, &all, &[("context".to_owned(), ctx)]);
        }
    }
    let interp = interp()?;
    let m = interp.load_attr_public(loop_, "call_soon")?;
    let mut all = Vec::with_capacity(cb_args.len() + 1);
    all.push(cb);
    all.extend_from_slice(cb_args);
    call(&m, &all, &[("context".to_owned(), ctx)])
}

/// `loop.get_debug()` for native callers: the `_debug` attribute of a
/// loop whose `get_debug` is the stock method.
pub(crate) fn get_debug(loop_: &Object) -> Result<bool, RuntimeError> {
    let ls = LOOP.read().clone();
    if let (Some(ls), Object::Instance(li)) = (&ls, loop_) {
        let native = verified(&NATIVE_GET_DEBUG, &li.class.borrow(), || {
            resolves_to(&li.cls(), "get_debug", &ls.get_debug)
        }) && inst_attr(li, &names().get_debug).is_none();
        if native {
            if let Some(Object::Bool(b)) = inst_attr(li, &names().debug) {
                return Ok(b);
            }
        }
    }
    let interp = interp()?;
    let m = interp.load_attr_public(loop_, "get_debug")?;
    Ok(call(&m, &[], &[])?.is_truthy())
}

/// A stock loop that is open and not in debug mode (the natives below
/// then do exactly what the Python methods would).
fn plain_loop(ls: &LoopState, loop_: &Object) -> bool {
    let Object::Instance(li) = loop_ else {
        return false;
    };
    let stock = verified(&STOCK_LOOP, &li.class.borrow(), || {
        let cls = li.cls();
        cls.is_subclass_of(&ls.cls)
            && resolves_to(&cls, "_call_soon", &ls.call_soon_inner)
            && resolves_to(&cls, "_check_closed", &ls.check_closed)
            && resolves_to(&cls, "get_debug", &ls.get_debug)
    });
    let n = names();
    stock
        && matches!(inst_attr(li, &n.debug), Some(Object::Bool(false)))
        && matches!(
            inst_attr(li, &n.closed),
            Some(Object::Bool(false))
        )
        && inst_attr(li, &n.get_debug).is_none()
}

/// `module.attr`, for the module bound to global `module` in `globals`.
fn global_module_attr(
    globals: &Rc<RefCell<DictData>>,
    module: &AttrName,
    attr: &AttrName,
) -> Option<Object> {
    let m = globals
        .borrow()
        .get(&crate::object::StrKeyHashed {
            s: module.name,
            hash: module.hash,
        })
        .cloned()?;
    let Object::Module(m) = m else {
        return None;
    };
    let v = m
        .dict
        .borrow()
        .get(&crate::object::StrKeyHashed {
            s: attr.name,
            hash: attr.hash,
        })
        .cloned();
    v
}

/// `BaseEventLoop.create_task(self, coro, **kwargs)`.
fn loop_create_task_method(
    ls: &LoopState,
    args: &[Object],
    kwargs: &[(String, Object)],
) -> Result<Object, RuntimeError> {
    if let [loop_, coro] = args {
        let plain_kwargs = kwargs
            .iter()
            .all(|(k, _)| matches!(k.as_str(), "name" | "context" | "eager_start"));
        if plain_kwargs && plain_loop(ls, loop_) {
            let Object::Instance(li) = loop_ else {
                unreachable!("plain_loop checks for an instance")
            };
            let no_factory = matches!(
                inst_attr(li, &names().task_factory),
                Some(Object::None)
            );
            let native = global_module_attr(&ls.globals, &names().tasks, &names().task)
                .is_some_and(|t| crate::stdlib::asyncio_mod::is_task_class(&t));
            if no_factory && native {
                return crate::stdlib::asyncio_mod::new_task(loop_, coro.clone(), kwargs);
            }
        }
    }
    call(&ls.create_task_py, args, kwargs)
}

/// `BaseEventLoop.create_future(self)`.
fn loop_create_future_method(ls: &LoopState, args: &[Object]) -> Result<Object, RuntimeError> {
    if let [loop_] = args {
        let native = global_module_attr(&ls.globals, &names().futures, &names().future)
            .is_some_and(|f| crate::stdlib::asyncio_mod::is_future_class(&f));
        // (Not in debug mode: there the Python method's frame shows in
        // the future's `_source_traceback`.)
        if native && plain_loop(ls, loop_) {
            return crate::stdlib::asyncio_mod::new_future(loop_);
        }
    }
    call(&ls.create_future_py, args, &[])
}

/// `install_loop(BaseEventLoop, globals())`.
fn install_loop(args: &[Object]) -> Result<Object, RuntimeError> {
    let [Object::Type(cls), Object::Dict(globals)] = args else {
        return Err(type_error("install_loop(BaseEventLoop, globals())"));
    };
    let get = |name: &'static str| -> Result<Object, RuntimeError> {
        cls.dict
            .borrow()
            .get(&DictKey(Object::from_static(name)))
            .cloned()
            .ok_or_else(|| type_error(format!("BaseEventLoop has no {name}")))
    };
    let call_soon_py = get("call_soon")?;
    let call_soon_inner = get("_call_soon")?;
    let check_closed = get("_check_closed")?;
    let get_debug = get("get_debug")?;
    let create_task_py = get("create_task")?;
    let create_future_py = get("create_future")?;
    let st = Rc::new(LoopState {
        cls: cls.clone(),
        call_soon_py,
        call_soon_native: std::sync::OnceLock::new(),
        call_soon_inner,
        check_closed,
        get_debug,
        create_task_py,
        create_future_py,
        globals: globals.clone(),
    });
    // The natives hold their own class's state (a re-imported
    // `asyncio.base_events` installs a fresh one on its fresh class).
    let (a, b, c, d, e) = (st.clone(), st.clone(), st.clone(), st.clone(), st.clone());
    let native = Object::Builtin(Rc::new(BuiltinFn {
        name: "call_soon",
        binds_instance: true,
        call: Box::new(move |args| loop_call_soon_method(&a, args, &[])),
        call_kw: Some(Box::new(move |args, kwargs| {
            loop_call_soon_method(&b, args, kwargs)
        })),
    }));
    let _ = st.call_soon_native.set(native.clone());
    {
        let mut dict = cls.dict.borrow_mut();
        dict.insert(DictKey(Object::from_static("call_soon")), native);
        dict.insert(
            DictKey(Object::from_static("create_task")),
            Object::Builtin(Rc::new(BuiltinFn {
                name: "create_task",
                binds_instance: true,
                call: Box::new(move |args| loop_create_task_method(&c, args, &[])),
                call_kw: Some(Box::new(move |args, kwargs| {
                    loop_create_task_method(&d, args, kwargs)
                })),
            })),
        );
        dict.insert(
            DictKey(Object::from_static("create_future")),
            Object::Builtin(Rc::new(BuiltinFn {
                name: "create_future",
                binds_instance: true,
                call: Box::new(move |args| loop_create_future_method(&e, args)),
                call_kw: None,
            })),
        );
    }
    cls.bump_attr_version();
    drop(LOOP.write().replace(st));
    Ok(Object::None)
}

pub fn build(_cache: &ModuleCache) -> Rc<PyModule> {
    let dict = Rc::new(RefCell::new(DictData::default()));
    {
        let mut d = dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("__name__")),
            Object::from_static("_weave_asyncio"),
        );
        for (name, f) in [
            (
                "install_handle",
                install_handle as fn(&[Object]) -> Result<Object, RuntimeError>,
            ),
            ("install_loop", install_loop),
        ] {
            d.insert(
                DictKey(Object::from_static(name)),
                Object::Builtin(Rc::new(BuiltinFn {
                    name,
                    binds_instance: false,
                    call: Box::new(f),
                    call_kw: None,
                })),
            );
        }
    }
    Rc::new(PyModule {
        name: "_weave_asyncio".to_owned(),
        filename: None,
        dict,
    })
}

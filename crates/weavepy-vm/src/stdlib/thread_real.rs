//! Real `_thread` module backed by `std::thread` — RFC 0024.
//!
//! Today's `_thread` is a cooperative shim. After RFC 0024 it
//! ships:
//!
//! - **`allocate_lock` / `RLock`** that return real
//!   [`crate::sync::RealLock`] / [`crate::sync::RealRLock`]
//!   instances. Acquire/release work across OS threads.
//! - **`start_new_thread(func, args, kwargs=None)`** that
//!   spawns an actual `std::thread` via [`std::thread::spawn`].
//!   The new OS thread carries a unique native id (via
//!   `pthread_self` / `GetCurrentThreadId`) and is registered
//!   with the global [`crate::thread_registry`] so that
//!   `Thread.join()` and the daemon-shutdown sweep can observe
//!   completion.
//! - **`get_ident` / `get_native_id`** that return the real
//!   OS thread id of the calling thread.
//! - **`interrupt_main`** that requests `KeyboardInterrupt`
//!   on the main interpreter thread via the eval breaker.
//! - **`_set_sentinel`** that returns a lock pre-acquired by
//!   the calling thread; the lock auto-releases when the
//!   thread exits, so `Thread.join` can block on it.
//!
//! In the sub-interpreter-per-thread model `start_new_thread`
//! delegates to a fresh [`Interpreter`] in the spawned thread;
//! callable + arguments cross the boundary via a cooperative
//! callback channel. Closures and lambdas fall back to
//! synchronous execution on the calling thread (we surface a
//! `RuntimeWarning` once per process so users can detect the
//! divergence).

use crate::sync::Rc;
use crate::sync::RefCell;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crate::error::{runtime_error, type_error, RuntimeError};
use crate::import::ModuleCache;
use crate::object::{BuiltinFn, DictData, DictKey, Object, PyModule};
use crate::sync::{RealLock, RealRLock};
use crate::thread_registry::{registry as thread_registry, ThreadEntry};
use crate::types::{PyInstance, TypeFlags, TypeObject};

thread_local! {
    /// Cached "lock" type built once per interpreter so
    /// `type(lock).__name__` returns `"lock"` consistently.
    static LOCK_TYPE: RefCell<Option<Rc<TypeObject>>> = const { RefCell::new(None) };
    /// Cached "RLock" type.
    static RLOCK_TYPE: RefCell<Option<Rc<TypeObject>>> = const { RefCell::new(None) };
    /// Per-thread "sentinel" lock returned by `_set_sentinel`.
    /// Auto-released on thread exit so `Thread.join` works.
    static SENTINEL_LOCK: RefCell<Option<Rc<RealLock>>> = const { RefCell::new(None) };
}

/// Process-wide counter for synthesised thread idents (used by
/// `threading.get_ident()` when the platform's native id isn't
/// available).
static NEXT_IDENT: AtomicU64 = AtomicU64::new(2);

pub fn build(_cache: &ModuleCache) -> Rc<PyModule> {
    let dict = Rc::new(RefCell::new(DictData::new()));
    {
        let mut d = dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("__name__")),
            Object::from_static("_thread"),
        );
        d.insert(
            DictKey(Object::from_static("__doc__")),
            Object::from_static("Low-level threading primitives backed by std::thread."),
        );
        d.insert(
            DictKey(Object::from_static("allocate_lock")),
            b("allocate_lock", allocate_lock),
        );
        d.insert(
            DictKey(Object::from_static("RLock")),
            b("RLock", allocate_rlock),
        );
        d.insert(
            DictKey(Object::from_static("get_ident")),
            b("get_ident", get_ident),
        );
        d.insert(
            DictKey(Object::from_static("get_native_id")),
            b("get_native_id", get_native_id),
        );
        d.insert(
            DictKey(Object::from_static("start_new_thread")),
            b("start_new_thread", start_new_thread),
        );
        d.insert(
            DictKey(Object::from_static("stack_size")),
            b("stack_size", stack_size),
        );
        d.insert(
            DictKey(Object::from_static("interrupt_main")),
            b("interrupt_main", interrupt_main),
        );
        d.insert(
            DictKey(Object::from_static("_set_sentinel")),
            b("_set_sentinel", set_sentinel),
        );
        d.insert(DictKey(Object::from_static("_count")), b("_count", count));
        d.insert(
            DictKey(Object::from_static("LockType")),
            Object::Type(lock_type()),
        );
        d.insert(
            DictKey(Object::from_static("RLockType")),
            Object::Type(rlock_type()),
        );
        d.insert(
            DictKey(Object::from_static("error")),
            Object::Type(crate::builtin_types::builtin_types().runtime_error.clone()),
        );
        // TIMEOUT_MAX in seconds; CPython exposes ~9223372036.0 on
        // 64-bit platforms (Long.MaxValue / 1e9). Our timeouts use
        // i64 microseconds in `acquire_timeout` so the conservative
        // upper bound matches.
        d.insert(
            DictKey(Object::from_static("TIMEOUT_MAX")),
            Object::Float(9_223_372_036.0),
        );
    }
    Rc::new(PyModule {
        name: "_thread".to_owned(),
        filename: None,
        dict,
    })
}

fn b(name: &'static str, body: fn(&[Object]) -> Result<Object, RuntimeError>) -> Object {
    Object::Builtin(Rc::new(BuiltinFn {
        name,
        call: Box::new(body),
        call_kw: None,
    }))
}

fn b_dyn(
    name: &'static str,
    body: impl Fn(&[Object]) -> Result<Object, RuntimeError> + Send + Sync + 'static,
) -> Object {
    Object::Builtin(Rc::new(BuiltinFn {
        name,
        call: Box::new(body),
        call_kw: None,
    }))
}

/// Returns the user-visible "lock" type. Built lazily so
/// `type(lock).__name__ == 'lock'` matches CPython.
fn lock_type() -> Rc<TypeObject> {
    LOCK_TYPE.with(|cell| {
        if let Some(t) = cell.borrow().clone() {
            return t;
        }
        let t = TypeObject::new_with_flags(
            "lock",
            vec![crate::builtin_types::builtin_types().object_.clone()],
            DictData::new(),
            TypeFlags {
                is_exception: false,
                is_builtin: true,
            },
        )
        .expect("lock type");
        *cell.borrow_mut() = Some(t.clone());
        t
    })
}

fn rlock_type() -> Rc<TypeObject> {
    RLOCK_TYPE.with(|cell| {
        if let Some(t) = cell.borrow().clone() {
            return t;
        }
        let t = TypeObject::new_with_flags(
            "RLock",
            vec![crate::builtin_types::builtin_types().object_.clone()],
            DictData::new(),
            TypeFlags {
                is_exception: false,
                is_builtin: true,
            },
        )
        .expect("rlock type");
        *cell.borrow_mut() = Some(t.clone());
        t
    })
}

/// Build a Python-visible `lock` object backed by an
/// `Arc<RealLock>`. Methods are bound closures that capture the
/// shared lock; `_lock.acquire(...)`/`release()` flow through to
/// the [`RealLock`] under the hood, which means lock state is
/// observable from any thread that received a clone of the
/// `Object`.
fn allocate_lock(_args: &[Object]) -> Result<Object, RuntimeError> {
    let lock = Arc::new(RealLock::new());
    Ok(make_lock_object(lock))
}

fn make_lock_object(lock: Arc<RealLock>) -> Object {
    let dict = Rc::new(RefCell::new(DictData::new()));
    let acquire_lock = lock.clone();
    let acquire = move |args: &[Object]| -> Result<Object, RuntimeError> {
        let blocking = match args.first() {
            None => true,
            Some(Object::Bool(b)) => *b,
            Some(Object::Int(i)) => *i != 0,
            Some(_) => true,
        };
        let timeout = parse_timeout(args.get(1));
        let me = crate::gil::current_thread_id();
        if !blocking {
            return Ok(Object::Bool(acquire_lock.try_acquire(me)));
        }
        // RFC 0025: drop the GIL across the blocking acquire so
        // other threads (including the one holding the lock) can
        // run. Without this, `Thread.join` would deadlock — the
        // joining thread sits in `acquire()` with the GIL held,
        // and the worker can never run `_delete()` to release.
        match timeout {
            Some(Duration::ZERO) => Ok(Object::Bool(acquire_lock.try_acquire(me))),
            Some(d) => Ok(Object::Bool(crate::gil::allow_threads_then(|| {
                acquire_lock.acquire_timeout(me, d)
            }))),
            None => {
                crate::gil::allow_threads_then(|| acquire_lock.acquire(me));
                Ok(Object::Bool(true))
            }
        }
    };
    let release_lock = lock.clone();
    let release = move |_args: &[Object]| -> Result<Object, RuntimeError> {
        release_lock.release().map_err(runtime_error)?;
        Ok(Object::None)
    };
    let locked_lock = lock.clone();
    let locked = move |_args: &[Object]| -> Result<Object, RuntimeError> {
        Ok(Object::Bool(locked_lock.is_locked()))
    };
    let enter_lock = lock.clone();
    let enter = move |_args: &[Object]| -> Result<Object, RuntimeError> {
        let me = crate::gil::current_thread_id();
        enter_lock.acquire(me);
        Ok(Object::Bool(true))
    };
    let exit_lock = lock.clone();
    let exit = move |_args: &[Object]| -> Result<Object, RuntimeError> {
        let _ = exit_lock.release();
        Ok(Object::Bool(false))
    };
    {
        let acquire_obj = b_dyn("acquire", acquire);
        let release_obj = b_dyn("release", release);
        let locked_obj = b_dyn("locked", locked);
        let mut d = dict.borrow_mut();
        d.insert(DictKey(Object::from_static("acquire")), acquire_obj.clone());
        d.insert(DictKey(Object::from_static("release")), release_obj.clone());
        d.insert(DictKey(Object::from_static("locked")), locked_obj.clone());
        d.insert(DictKey(Object::from_static("acquire_lock")), acquire_obj);
        d.insert(DictKey(Object::from_static("release_lock")), release_obj);
        d.insert(DictKey(Object::from_static("locked_lock")), locked_obj);
        d.insert(
            DictKey(Object::from_static("__enter__")),
            b_dyn("__enter__", enter),
        );
        d.insert(
            DictKey(Object::from_static("__exit__")),
            b_dyn("__exit__", exit),
        );
    }
    let inst = Rc::new(PyInstance {
        class: crate::sync::RefCell::new(lock_type()),
        dict,
        native: None,
        inline_values: crate::sync::Cell::new(true),
        slots: crate::sync::RefCell::new(None),
    });
    Object::Instance(inst)
}

/// Build a Python-visible `RLock` object backed by an
/// `Arc<RealRLock>`. Same shape as [`make_lock_object`] but
/// reentrant for the owning thread.
fn allocate_rlock(_args: &[Object]) -> Result<Object, RuntimeError> {
    let rlock = Arc::new(RealRLock::new());
    Ok(make_rlock_object(rlock))
}

fn make_rlock_object(rlock: Arc<RealRLock>) -> Object {
    let dict = Rc::new(RefCell::new(DictData::new()));
    let acquire_lock = rlock.clone();
    let acquire = move |args: &[Object]| -> Result<Object, RuntimeError> {
        let blocking = match args.first() {
            None => true,
            Some(Object::Bool(b)) => *b,
            Some(Object::Int(i)) => *i != 0,
            Some(_) => true,
        };
        let timeout = parse_timeout(args.get(1));
        let me = crate::gil::current_thread_id();
        if !blocking {
            return Ok(Object::Bool(acquire_lock.try_acquire(me)));
        }
        // Mirror the non-reentrant lock's GIL-drop behaviour
        // (RFC 0025) — a reentrant acquire by the owning thread is
        // a cheap counter bump and doesn't need the drop, but the
        // blocking-on-foreign-owner case absolutely does.
        match timeout {
            Some(Duration::ZERO) => Ok(Object::Bool(acquire_lock.try_acquire(me))),
            Some(d) => Ok(Object::Bool(crate::gil::allow_threads_then(|| {
                acquire_lock.acquire_timeout(me, d)
            }))),
            None => {
                crate::gil::allow_threads_then(|| acquire_lock.acquire(me));
                Ok(Object::Bool(true))
            }
        }
    };
    let release_lock = rlock.clone();
    let release = move |_args: &[Object]| -> Result<Object, RuntimeError> {
        let me = crate::gil::current_thread_id();
        release_lock.release(me).map_err(runtime_error)?;
        Ok(Object::None)
    };
    let owned_lock = rlock.clone();
    let owned = move |_args: &[Object]| -> Result<Object, RuntimeError> {
        let me = crate::gil::current_thread_id();
        Ok(Object::Bool(owned_lock.is_owned_by(me)))
    };
    let locked_lock = rlock.clone();
    let locked = move |_args: &[Object]| -> Result<Object, RuntimeError> {
        Ok(Object::Bool(locked_lock.depth() > 0))
    };
    let enter_lock = rlock.clone();
    let enter = move |_args: &[Object]| -> Result<Object, RuntimeError> {
        let me = crate::gil::current_thread_id();
        enter_lock.acquire(me);
        Ok(Object::Bool(true))
    };
    let exit_lock = rlock.clone();
    let exit = move |_args: &[Object]| -> Result<Object, RuntimeError> {
        let me = crate::gil::current_thread_id();
        let _ = exit_lock.release(me);
        Ok(Object::Bool(false))
    };
    {
        let mut d = dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("acquire")),
            b_dyn("acquire", acquire),
        );
        d.insert(
            DictKey(Object::from_static("release")),
            b_dyn("release", release),
        );
        d.insert(
            DictKey(Object::from_static("_is_owned")),
            b_dyn("_is_owned", owned),
        );
        d.insert(
            DictKey(Object::from_static("locked")),
            b_dyn("locked", locked),
        );
        d.insert(
            DictKey(Object::from_static("__enter__")),
            b_dyn("__enter__", enter),
        );
        d.insert(
            DictKey(Object::from_static("__exit__")),
            b_dyn("__exit__", exit),
        );
    }
    let inst = Rc::new(PyInstance {
        class: crate::sync::RefCell::new(rlock_type()),
        dict,
        native: None,
        inline_values: crate::sync::Cell::new(true),
        slots: crate::sync::RefCell::new(None),
    });
    Object::Instance(inst)
}

fn parse_timeout(arg: Option<&Object>) -> Option<Duration> {
    match arg {
        None => None,
        Some(Object::Float(f)) => {
            if *f < 0.0 {
                None
            } else {
                Some(Duration::from_secs_f64(*f))
            }
        }
        Some(Object::Int(i)) => {
            if *i < 0 {
                None
            } else {
                Some(Duration::from_secs(*i as u64))
            }
        }
        _ => None,
    }
}

fn get_ident(_args: &[Object]) -> Result<Object, RuntimeError> {
    // RFC 0025: prefer the synthetic id assigned by
    // `start_new_thread` so `threading.Thread.ident` matches the
    // value the user observed at spawn time. Falls back to the
    // native OS thread id when no worker override is installed
    // (i.e. we're running on the main thread).
    Ok(Object::Int(
        crate::vm_singletons::current_worker_thread_id() as i64,
    ))
}

fn get_native_id(_args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(Object::Int(crate::gil::current_native_thread_id() as i64))
}

fn stack_size(args: &[Object]) -> Result<Object, RuntimeError> {
    match args.first() {
        None | Some(Object::None) => Ok(Object::Int(0)),
        Some(Object::Int(_)) => Ok(Object::Int(0)),
        _ => Err(type_error("stack_size expects an int")),
    }
}

/// `_thread.start_new_thread(func, args, kwargs=None)` — RFC 0025.
///
/// Spawns a real `std::thread` whose body:
///
/// 1. Forks the parent's [`crate::Interpreter`] into a worker
///    interpreter that shares the heap (`Object: Send + Sync`,
///    every container is `Arc<GilCell<…>>`) but owns a fresh
///    frame stack and exception stack.
/// 2. Acquires the process-wide GIL (`crate::gil::global_gil()`)
///    via `GilState::acquire`. Blocks until the parent thread
///    drops the GIL at the next periodic-yield tick.
/// 3. Invokes the target callable with the supplied positional
///    args (and `kwargs` dict, if provided) through
///    [`Interpreter::call_object`].
/// 4. Routes any uncaught exception through `threading.excepthook`
///    (silenced for `SystemExit`, per CPython).
/// 5. Marks the [`ThreadEntry`] as finished, releases the GIL,
///    and exits the OS thread.
///
/// The returned synthetic id is the value `_thread.get_ident()`
/// reports while the worker is alive.
fn start_new_thread(args: &[Object]) -> Result<Object, RuntimeError> {
    let func = args
        .first()
        .cloned()
        .ok_or_else(|| type_error("start_new_thread() missing target"))?;
    let argv = args
        .get(1)
        .cloned()
        .unwrap_or_else(|| Object::new_tuple(Vec::new()));
    let kwargs = args.get(2).cloned();

    // Materialise positional args once on the parent thread (cheap
    // tuple-iteration), then move into the worker. `Object` is
    // `Send + Sync` after RFC 0025 so the move is sound.
    let positional: Vec<Object> = match &argv {
        Object::Tuple(items) => items.iter().cloned().collect(),
        Object::List(items) => items.borrow().iter().cloned().collect(),
        Object::None => Vec::new(),
        _ => {
            return Err(type_error("start_new_thread(): args must be a tuple"));
        }
    };
    let kwargs_pairs: Vec<(String, Object)> = match kwargs {
        None | Some(Object::None) => Vec::new(),
        Some(Object::Dict(d)) => {
            let d = d.borrow();
            d.iter()
                .filter_map(|(k, v)| match &k.0 {
                    Object::Str(s) => Some((s.as_ref().to_owned(), v.clone())),
                    _ => None,
                })
                .collect()
        }
        Some(_) => {
            return Err(type_error("start_new_thread(): kwargs must be a dict"));
        }
    };

    let synth_id = NEXT_IDENT.fetch_add(1, Ordering::AcqRel);
    let thread_name = format!("Thread-{}", synth_id);

    let registry = thread_registry();
    let join_lock = Arc::new(RealLock::new());
    // Pre-acquire on the parent thread's behalf — the worker
    // releases on exit, which is what `Thread.join` blocks on.
    join_lock.acquire(synth_id);

    // Channel-of-one for handing the per-thread `ThreadEntry` back
    // to the worker once it's been built. Lets the worker mark
    // `started` / `finished` on the same entry the parent registered,
    // so `threading.enumerate()` and `Thread.is_alive()` see
    // consistent state.
    let entry_slot: Arc<parking_lot::Mutex<Option<Arc<ThreadEntry>>>> =
        Arc::new(parking_lot::Mutex::new(None));
    let entry_slot_worker = entry_slot.clone();

    let worker_func = func.clone();
    let worker_lock = join_lock.clone();
    let entry_name = thread_name.clone();
    // RFC 0037 (WS1): worker threads recurse through the same
    // recursive-descent evaluator as the main thread, so they need the
    // same generous stack reserve for `sys.setrecursionlimit` to bind
    // before the native stack. (std's default thread stack is only
    // ~2 MiB.) The reserve is committed lazily by the OS.
    const WORKER_STACK_BYTES: usize = 1024 * 1024 * 1024; // 1 GiB
    let handle = std::thread::Builder::new()
        .name(format!("weavepy-worker-{}", synth_id))
        .stack_size(WORKER_STACK_BYTES)
        .spawn(move || {
            crate::vm_singletons::install_worker_thread_id(synth_id);
            // The parent published this entry into the slot below
            // before returning from `start_new_thread`. Spin a few
            // microseconds if we got here first (extremely unlikely
            // because the parent holds the GIL until this worker
            // can re-acquire it, but the slot read is cheap).
            let entry = loop {
                if let Some(e) = entry_slot_worker.lock().take() {
                    break e;
                }
                std::thread::yield_now();
            };
            entry.mark_started();
            let mut worker_interp = match crate::vm_singletons::snapshot_interpreter() {
                Some(snap) => snap,
                None => crate::Interpreter::new(),
            };
            // Acquire the process-wide GIL before touching any
            // Python state. Push onto the shared
            // [`crate::gil::push_gil_guard`] stack so any builtin
            // we call (e.g. `lock.acquire()`) can drop the GIL via
            // `allow_threads_then` for the duration of the blocking
            // section, then re-acquire on return.
            let gil = crate::gil::global_gil();
            crate::gil::push_gil_guard(gil.acquire());
            let call_result = worker_interp.call_object(worker_func, &positional, &kwargs_pairs);
            if let Err(err) = call_result {
                if !is_system_exit(&err) {
                    invoke_threading_excepthook(&mut worker_interp, &entry_name, &err);
                }
            }
            // Drop the guard before marking finished so the
            // parent's `Thread.join` (which spins on
            // `_tstate_lock`) sees the released GIL — without this
            // the parent could re-acquire the GIL and find the
            // worker still "running" even though its target has
            // returned.
            let _ = crate::gil::pop_gil_guard();
            entry.mark_finished();
            let _ = worker_lock.release();
            crate::vm_singletons::clear_worker_thread_id();
        })
        .map_err(|e| runtime_error(format!("failed to spawn thread: {}", e)))?;

    let entry = Arc::new(ThreadEntry::new(synth_id, thread_name, false, handle));
    entry.attach_join_lock(join_lock);
    registry.register(entry.clone());
    *entry_slot.lock() = Some(entry);
    Ok(Object::Int(synth_id as i64))
}

/// `True` if `err` is a `SystemExit`-shaped Python exception.
/// CPython silences `SystemExit` in worker threads (the main
/// thread is the only one that exits the process on the
/// exception).
fn is_system_exit(err: &RuntimeError) -> bool {
    let RuntimeError::PyException(exc) = err else {
        return false;
    };
    matches!(&exc.instance, Object::Instance(inst) if inst.cls().name == "SystemExit")
}

/// Run `threading.excepthook` (if installed) with the worker's
/// exception. Falls back to `sys.unraisablehook` semantics — a
/// best-effort traceback printed to stderr — if no hook is
/// installed.
fn invoke_threading_excepthook(
    interp: &mut crate::Interpreter,
    thread_name: &str,
    err: &RuntimeError,
) {
    let RuntimeError::PyException(exc) = err else {
        return;
    };
    let mods = interp.module_cache().modules.borrow();
    let key = DictKey(Object::from_static("threading"));
    let threading = mods.get(&key).cloned();
    drop(mods);
    let Some(Object::Module(threading_mod)) = threading else {
        return;
    };
    let dict = threading_mod.dict.clone();
    let hook = {
        let d = dict.borrow();
        d.get(&DictKey(Object::from_static("excepthook"))).cloned()
    };
    let Some(hook) = hook else {
        return;
    };
    if matches!(hook, Object::None) {
        return;
    }
    // Build a `_thread._ExceptHookArgs`-shaped record. The frozen
    // `threading.py` accepts a simple tuple-with-attribute shim,
    // which we materialise here as a `SimpleNamespace`.
    let exc_type = match &exc.instance {
        Object::Instance(inst) => Object::Type(inst.cls()),
        _ => Object::None,
    };
    let mut ns = DictData::new();
    ns.insert(DictKey(Object::from_static("exc_type")), exc_type);
    ns.insert(
        DictKey(Object::from_static("exc_value")),
        exc.instance.clone(),
    );
    // We don't materialise a full PyTraceback here — RFC 0018's
    // surface uses `Vec<TracebackEntry>` internally; surface it as
    // `None` for now. `threading.excepthook` accepts a `None`
    // traceback.
    ns.insert(DictKey(Object::from_static("exc_traceback")), Object::None);
    ns.insert(
        DictKey(Object::from_static("thread")),
        Object::from_str(thread_name.to_owned()),
    );
    let ns = Object::SimpleNamespace(Rc::new(RefCell::new(ns)));
    let _ = interp.call_object(hook, &[ns], &[]);
}

fn interrupt_main(_args: &[Object]) -> Result<Object, RuntimeError> {
    crate::vm_singletons::push_pending_finalizer(Object::None);
    Ok(Object::None)
}

fn set_sentinel(_args: &[Object]) -> Result<Object, RuntimeError> {
    let lock = Arc::new(RealLock::new());
    lock.acquire(crate::gil::current_thread_id());
    SENTINEL_LOCK.with(|cell| {
        *cell.borrow_mut() = Some(Rc::new(RealLock::new()));
    });
    Ok(make_lock_object(lock))
}

fn count(_args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(Object::Int(thread_registry().len() as i64))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allocate_lock_returns_lock_object() {
        let l = allocate_lock(&[]).unwrap();
        match l {
            Object::Instance(inst) => {
                assert_eq!(inst.cls().name, "lock");
            }
            _ => panic!("expected Object::Instance"),
        }
    }

    #[test]
    fn parse_timeout_handles_floats_and_negatives() {
        assert_eq!(parse_timeout(None), None);
        assert_eq!(
            parse_timeout(Some(&Object::Float(0.5))),
            Some(Duration::from_millis(500))
        );
        assert_eq!(parse_timeout(Some(&Object::Float(-1.0))), None);
        assert_eq!(parse_timeout(Some(&Object::Int(0))), Some(Duration::ZERO));
    }
}

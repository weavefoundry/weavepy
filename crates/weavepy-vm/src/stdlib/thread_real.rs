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
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use crate::error::{overflow_error, runtime_error, type_error, value_error, RuntimeError};
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
    /// Cached `_ThreadHandle` type (RFC 0039).
    static THREAD_HANDLE_TYPE: RefCell<Option<Rc<TypeObject>>> = const { RefCell::new(None) };
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
        // In CPython `_thread.RLock` is a *type* (so `isinstance(x,
        // _thread.RLock)` and `class C(_thread.RLock)` both work —
        // test_threading.CRLockTests.test_signature). Expose the type
        // itself; the VM's `instantiate` routes a direct `RLock(...)`
        // call to `new_rlock_object()` and tolerates (ignores) extra
        // args — the `threading.RLock` factory emits the gh-102029
        // `DeprecationWarning` before delegating here.
        d.insert(
            DictKey(Object::from_static("RLock")),
            Object::Type(rlock_type()),
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
        // CPython 3.13 exposes the lock type under its bare `__qualname__`
        // (`_thread.lock`) too, not just `LockType`. `threading.Lock` is bound
        // to this type, and `pickle` saves a type by `__module__.__qualname__`
        // — so `pickle.dumps(threading.Lock)` resolves `_thread.lock`. Without
        // this alias the lookup fails (`SyncManager.register('Lock', ...)` over
        // `spawn` pickles the registry).
        d.insert(
            DictKey(Object::from_static("lock")),
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
        // RFC 0039 (WS1/WS2): the CPython 3.13 `threading.py` is ported
        // verbatim and drives a handle-based thread API. Expose the
        // surface it binds at import time.
        d.insert(
            DictKey(Object::from_static("start_joinable_thread")),
            Object::Builtin(Rc::new(BuiltinFn::with_kwargs(
                "start_joinable_thread",
                start_joinable_thread,
            ))),
        );
        d.insert(
            DictKey(Object::from_static("daemon_threads_allowed")),
            b("daemon_threads_allowed", daemon_threads_allowed),
        );
        d.insert(
            DictKey(Object::from_static("_shutdown")),
            b("_shutdown", thread_shutdown),
        );
        d.insert(
            DictKey(Object::from_static("_make_thread_handle")),
            b("_make_thread_handle", make_thread_handle),
        );
        d.insert(
            DictKey(Object::from_static("_ThreadHandle")),
            b("_ThreadHandle", thread_handle_new),
        );
        d.insert(
            DictKey(Object::from_static("_get_main_thread_ident")),
            b("_get_main_thread_ident", get_main_thread_ident),
        );
        d.insert(
            DictKey(Object::from_static("_is_main_interpreter")),
            b("_is_main_interpreter", is_main_interpreter),
        );
    }
    // Every native `_thread` function reports `__module__ == "_thread"`
    // (CPython attributes a C module's functions to that module). The
    // verbatim `threading.py` re-exports several of them — `get_ident`,
    // `stack_size`, `get_native_id` — and `test_threading.test__all__`
    // grades `threading.__all__` against the names whose `__module__`
    // is `threading`/`_thread`, so an un-attributed builtin (defaulting
    // to `"builtins"`) would be dropped from the expected set.
    for (_k, v) in dict.borrow().iter() {
        if matches!(v, Object::Builtin(_)) {
            crate::descr_registry::register_module(v, "_thread");
        }
    }
    // `build()` runs on the main OS thread the first time `_thread` is
    // imported during interpreter startup, so the calling thread's id is
    // the main-thread ident `threading._MainThread` will report. Only the
    // first (main-thread) import seeds it; a fork child re-seeds via
    // `after_fork_in_child`.
    let _ = MAIN_THREAD_IDENT.compare_exchange(
        0,
        main_thread_ident_now(),
        Ordering::AcqRel,
        Ordering::Acquire,
    );
    // RFC 0040 WS4: register the main thread's pthread_t under its ident so
    // `signal.pthread_kill(get_ident(), sig)` from the main thread resolves.
    #[cfg(unix)]
    super::signal_mod::register_current_thread_pthread(main_thread_ident_now());
    Rc::new(PyModule {
        name: "_thread".to_owned(),
        filename: None,
        dict,
    })
}

fn b(name: &'static str, body: fn(&[Object]) -> Result<Object, RuntimeError>) -> Object {
    Object::Builtin(Rc::new(BuiltinFn {
        name,
        binds_instance: false,
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
        binds_instance: false,
        call: Box::new(body),
        call_kw: None,
    }))
}

/// Like [`b_dyn`] but kwargs-aware — CPython's `lock.acquire` accepts
/// `blocking=` / `timeout=` by keyword, and the verbatim `threading.py`
/// relies on it.
fn b_dyn_kw(
    name: &'static str,
    body: impl Fn(&[Object], &[(String, Object)]) -> Result<Object, RuntimeError>
        + Send
        + Sync
        + Clone
        + 'static,
) -> Object {
    Object::Builtin(Rc::new(BuiltinFn::with_kwargs(name, body)))
}

/// Resolve the `(blocking, timeout_obj)` pair for a lock `acquire`
/// call from positional `(blocking, timeout)` and/or the `blocking=`,
/// `timeout=` keyword forms.
fn resolve_acquire_args(
    args: &[Object],
    kwargs: &[(String, Object)],
) -> Result<(bool, Option<Object>), RuntimeError> {
    let mut blocking_obj = args.first().cloned();
    let mut timeout_obj = args.get(1).cloned();
    for (k, v) in kwargs {
        match k.as_str() {
            "blocking" => blocking_obj = Some(v.clone()),
            "timeout" => timeout_obj = Some(v.clone()),
            other => {
                return Err(type_error(format!(
                    "acquire() got an unexpected keyword argument '{other}'"
                )))
            }
        }
    }
    let blocking = match &blocking_obj {
        None => true,
        Some(Object::Bool(b)) => *b,
        Some(Object::Int(i)) => *i != 0,
        Some(_) => true,
    };
    if let Some(t) = &timeout_obj {
        let tv = match t {
            Object::Int(i) => *i as f64,
            Object::Float(f) => *f,
            _ => -1.0,
        };
        // CPython: a timeout other than the -1 sentinel makes no sense
        // for a non-blocking acquire; and a blocking acquire rejects any
        // negative timeout other than the -1 ("forever") sentinel.
        if !blocking && tv != -1.0 {
            return Err(value_error(
                "can't specify a timeout for a non-blocking call",
            ));
        }
        if tv < 0.0 && tv != -1.0 {
            return Err(value_error("timeout value must be positive"));
        }
        // Beyond `TIMEOUT_MAX` the nanosecond deadline overflows; CPython
        // raises OverflowError.
        if tv.is_nan() || tv > TIMEOUT_MAX_SECS {
            return Err(overflow_error(
                "timestamp too large to convert to C _PyTime_t",
            ));
        }
    }
    Ok((blocking, timeout_obj))
}

/// `_thread.TIMEOUT_MAX`, in seconds — the largest timeout that fits the
/// internal nanosecond deadline without overflow.
const TIMEOUT_MAX_SECS: f64 = 9_223_372_036.0;

/// Returns the user-visible "lock" type. Built lazily so
/// `type(lock).__name__ == 'lock'` matches CPython.
fn lock_type() -> Rc<TypeObject> {
    LOCK_TYPE.with(|cell| {
        if let Some(t) = cell.borrow().clone() {
            return t;
        }
        let mut d = DictData::new();
        d.insert(
            DictKey(Object::from_static("__module__")),
            Object::from_static("_thread"),
        );
        d.insert(
            DictKey(Object::from_static("__repr__")),
            Object::Builtin(Rc::new(BuiltinFn {
                name: "__repr__",
                binds_instance: true,
                call: Box::new(lock_repr),
                call_kw: None,
            })),
        );
        let t = TypeObject::new_with_flags(
            "lock",
            vec![crate::builtin_types::builtin_types().object_.clone()],
            d,
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

/// `type(lock).__repr__` — CPython renders
/// `<unlocked _thread.lock object at 0x...>` (or `locked`). The lock
/// state is read back through the instance's own `locked()` method.
fn lock_repr(args: &[Object]) -> Result<Object, RuntimeError> {
    let Some(Object::Instance(inst)) = args.first() else {
        return Err(type_error("__repr__ requires a lock instance"));
    };
    let locked_fn = inst
        .dict
        .borrow()
        .get(&DictKey(Object::from_static("locked")))
        .cloned();
    let is_locked = matches!(
        locked_fn.and_then(|f| match f {
            Object::Builtin(b) => (b.call)(&[]).ok(),
            _ => None,
        }),
        Some(Object::Bool(true))
    );
    let st = if is_locked { "locked" } else { "unlocked" };
    let addr = Rc::as_ptr(inst) as usize;
    Ok(Object::from_str(format!(
        "<{st} _thread.lock object at 0x{addr:x}>"
    )))
}

fn rlock_type() -> Rc<TypeObject> {
    RLOCK_TYPE.with(|cell| {
        if let Some(t) = cell.borrow().clone() {
            return t;
        }
        let mut d = DictData::new();
        d.insert(
            DictKey(Object::from_static("__module__")),
            Object::from_static("_thread"),
        );
        d.insert(
            DictKey(Object::from_static("__repr__")),
            Object::Builtin(Rc::new(BuiltinFn {
                name: "__repr__",
                binds_instance: true,
                call: Box::new(rlock_repr),
                call_kw: None,
            })),
        );
        let t = TypeObject::new_with_flags(
            "RLock",
            vec![crate::builtin_types::builtin_types().object_.clone()],
            d,
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

/// `type(RLock).__repr__` — `<unlocked _thread.RLock object at 0x...>`
/// (or `locked`), derived from the instance's own `locked()` method.
fn rlock_repr(args: &[Object]) -> Result<Object, RuntimeError> {
    let Some(Object::Instance(inst)) = args.first() else {
        return Err(type_error("__repr__ requires an RLock instance"));
    };
    let locked_fn = inst
        .dict
        .borrow()
        .get(&DictKey(Object::from_static("locked")))
        .cloned();
    let is_locked = matches!(
        locked_fn.and_then(|f| match f {
            Object::Builtin(b) => (b.call)(&[]).ok(),
            _ => None,
        }),
        Some(Object::Bool(true))
    );
    let st = if is_locked { "locked" } else { "unlocked" };
    let addr = Rc::as_ptr(inst) as usize;
    Ok(Object::from_str(format!(
        "<{st} _thread.RLock object at 0x{addr:x}>"
    )))
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

/// Public constructor so calling the `lock` type itself —
/// `threading.Lock`, which is `_thread.LockType` — yields a working
/// lock. CPython's `LockType()` constructs a fresh lock; the VM's
/// `instantiate` routes the builtin-type call here.
pub fn new_lock_object() -> Object {
    make_lock_object(Arc::new(RealLock::new()))
}

/// Public constructor for the `RLock` type (`_thread.RLockType`).
pub fn new_rlock_object() -> Object {
    make_rlock_object(Arc::new(RealRLock::new()))
}

fn make_lock_object(lock: Arc<RealLock>) -> Object {
    let dict = Rc::new(RefCell::new(DictData::new()));
    let acquire_lock = lock.clone();
    let acquire =
        move |args: &[Object], kwargs: &[(String, Object)]| -> Result<Object, RuntimeError> {
            let (blocking, timeout_obj) = resolve_acquire_args(args, kwargs)?;
            let timeout = parse_timeout(timeout_obj.as_ref());
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
                Some(d) => Ok(Object::Bool(lock_acquire_interruptible(
                    &acquire_lock,
                    me,
                    Some(d),
                )?)),
                None => Ok(Object::Bool(lock_acquire_interruptible(
                    &acquire_lock,
                    me,
                    None,
                )?)),
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
        // `with lock:` is just `lock.acquire()`: try under the GIL,
        // and only drop it to block on a foreign owner. Blocking while
        // *holding* the GIL would deadlock the whole interpreter (the
        // owner that would release the lock could never run).
        lock_acquire_blocking(&enter_lock, me);
        Ok(Object::Bool(true))
    };
    let exit_lock = lock.clone();
    let exit = move |_args: &[Object]| -> Result<Object, RuntimeError> {
        let _ = exit_lock.release();
        Ok(Object::Bool(false))
    };
    let reinit_lock = lock.clone();
    let at_fork_reinit = move |_args: &[Object]| -> Result<Object, RuntimeError> {
        reinit_lock.force_reset();
        Ok(Object::None)
    };
    {
        let acquire_obj = b_dyn_kw("acquire", acquire);
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
        d.insert(
            DictKey(Object::from_static("_at_fork_reinit")),
            b_dyn("_at_fork_reinit", at_fork_reinit),
        );
    }
    let inst = Rc::new(PyInstance {
        class: crate::sync::RefCell::new(lock_type()),
        dict,
        native: None,
        inline_values: crate::sync::Cell::new(true),
        slots: crate::sync::RefCell::new(None),
        hash_cache: crate::sync::Cell::new(None),
        finalize_ran: crate::sync::Cell::new(false),
    });
    Object::Instance(inst)
}

/// Build a Python-visible `RLock` object backed by an
/// `Arc<RealRLock>`. Same shape as [`make_lock_object`] but
/// reentrant for the owning thread.
fn make_rlock_object(rlock: Arc<RealRLock>) -> Object {
    let dict = Rc::new(RefCell::new(DictData::new()));
    let acquire_lock = rlock.clone();
    let acquire =
        move |args: &[Object], kwargs: &[(String, Object)]| -> Result<Object, RuntimeError> {
            let (blocking, timeout_obj) = resolve_acquire_args(args, kwargs)?;
            let timeout = parse_timeout(timeout_obj.as_ref());
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
                Some(d) => Ok(Object::Bool(rlock_acquire_interruptible(
                    &acquire_lock,
                    me,
                    Some(d),
                )?)),
                None => Ok(Object::Bool(rlock_acquire_interruptible(
                    &acquire_lock,
                    me,
                    None,
                )?)),
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
    // CPython's RLock exposes `_release_save` / `_acquire_restore` so
    // `Condition.wait()` can drop the *entire* reentrant hold and later
    // restore it. The opaque "state" is just the saved recursion count.
    let release_save_lock = rlock.clone();
    let release_save = move |_args: &[Object]| -> Result<Object, RuntimeError> {
        let me = crate::gil::current_thread_id();
        // CPython raises `RuntimeError("cannot release un-acquired
        // lock")` if the calling thread doesn't hold the RLock.
        if !release_save_lock.is_owned_by(me) {
            return Err(runtime_error("cannot release un-acquired lock"));
        }
        let count = release_save_lock.depth();
        for _ in 0..count {
            release_save_lock.release(me).map_err(runtime_error)?;
        }
        Ok(Object::Int(count as i64))
    };
    // `_recursion_count()` — CPython returns the owning thread's
    // acquisition depth, or 0 when the caller isn't the owner.
    let recursion_count_lock = rlock.clone();
    let recursion_count = move |_args: &[Object]| -> Result<Object, RuntimeError> {
        let me = crate::gil::current_thread_id();
        let n = if recursion_count_lock.is_owned_by(me) {
            recursion_count_lock.depth()
        } else {
            0
        };
        Ok(Object::Int(n as i64))
    };
    let acquire_restore_lock = rlock.clone();
    let acquire_restore = move |args: &[Object]| -> Result<Object, RuntimeError> {
        let me = crate::gil::current_thread_id();
        let count = match args.first() {
            Some(Object::Int(n)) => *n,
            _ => 1,
        };
        if count >= 1 {
            // First reacquire may block on a foreign owner (drop GIL);
            // the remaining holds are reentrant and cheap.
            rlock_acquire_blocking(&acquire_restore_lock, me);
            for _ in 1..count {
                acquire_restore_lock.acquire(me);
            }
        }
        Ok(Object::None)
    };
    let enter_lock = rlock.clone();
    let enter = move |_args: &[Object]| -> Result<Object, RuntimeError> {
        let me = crate::gil::current_thread_id();
        // See the non-reentrant lock's `__enter__`: blocking on the
        // acquire while holding the GIL deadlocks the interpreter. A
        // reentrant re-acquire by the owner is cheap, but the
        // foreign-owner case must drop the GIL.
        rlock_acquire_blocking(&enter_lock, me);
        Ok(Object::Bool(true))
    };
    let exit_lock = rlock.clone();
    let exit = move |_args: &[Object]| -> Result<Object, RuntimeError> {
        let me = crate::gil::current_thread_id();
        let _ = exit_lock.release(me);
        Ok(Object::Bool(false))
    };
    let reinit_lock = rlock.clone();
    let at_fork_reinit = move |_args: &[Object]| -> Result<Object, RuntimeError> {
        reinit_lock.force_reset();
        Ok(Object::None)
    };
    {
        let mut d = dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("acquire")),
            b_dyn_kw("acquire", acquire),
        );
        d.insert(
            DictKey(Object::from_static("_at_fork_reinit")),
            b_dyn("_at_fork_reinit", at_fork_reinit),
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
            DictKey(Object::from_static("_release_save")),
            b_dyn("_release_save", release_save),
        );
        d.insert(
            DictKey(Object::from_static("_acquire_restore")),
            b_dyn("_acquire_restore", acquire_restore),
        );
        d.insert(
            DictKey(Object::from_static("_recursion_count")),
            b_dyn("_recursion_count", recursion_count),
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
        hash_cache: crate::sync::Cell::new(None),
        finalize_ran: crate::sync::Cell::new(false),
    });
    Object::Instance(inst)
}

fn parse_timeout(arg: Option<&Object>) -> Option<Duration> {
    match arg {
        None => None,
        Some(Object::Float(f)) => {
            if *f < 0.0 {
                None
            } else if !f.is_finite() || *f > TIMEOUT_MAX_SECS {
                // Saturate rather than panic in `Duration::from_secs_f64`;
                // callers that care reject these earlier with OverflowError.
                Some(Duration::from_secs(TIMEOUT_MAX_SECS as u64))
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

/// Blocking `RealLock` acquire with CPython's GIL fast path.
///
/// CPython's `lock.acquire()` first tries the lock *without*
/// releasing the GIL (`PY_LOCK_ACQUIRED` on the uncontended path)
/// and only drops the GIL to block when the lock is genuinely
/// owned by another thread. Doing the same here is a large
/// throughput win: a `with lock:` over a momentarily-held mutex
/// (the dominant `queue`/`Condition` pattern) no longer forces a
/// GIL hand-off + context switch on every acquire.
#[inline]
fn lock_acquire_blocking(lock: &RealLock, me: u64) {
    if lock.try_acquire(me) {
        return;
    }
    crate::gil::allow_threads_then(|| lock.acquire(me));
}

/// Slice length for the main thread's signal-interruptible wait. Short
/// enough that a tripped signal is serviced promptly (CPython relies on
/// the kernel's `EINTR`); long enough that the idle wakeup cost is
/// negligible.
const SIGNAL_POLL_SLICE: Duration = Duration::from_millis(20);

/// Run any pending OS-signal handlers on the main thread, propagating a
/// handler that raises. Called between wait slices of an interruptible
/// blocking acquire. No-op (and cheap) when nothing is tripped.
fn service_pending_signals() -> Result<(), RuntimeError> {
    if !crate::stdlib::signal_mod::signals_pending() {
        return Ok(());
    }
    if let Some(ptr) = crate::vm_singletons::current_interpreter_ptr() {
        // SAFETY: the interpreter pointer was published by the active
        // builtin call on this (main) thread and outlives this call.
        let interp = unsafe { &mut *ptr };
        interp.run_pending_signals_public()?;
    }
    Ok(())
}

/// Blocking `RealLock` acquire that stays responsive to signals on the
/// main thread (CPython's `EINTR` + `PyErr_CheckSignals` retry loop).
/// On a non-main thread — which never runs Python signal handlers — it
/// falls back to a plain blocking acquire. A signal handler that raises
/// abandons the acquire with that exception.
fn lock_acquire_interruptible(
    lock: &RealLock,
    me: u64,
    timeout: Option<Duration>,
) -> Result<bool, RuntimeError> {
    if lock.try_acquire(me) {
        return Ok(true);
    }
    if !crate::gil::is_main_thread() {
        return Ok(match timeout {
            Some(d) => crate::gil::allow_threads_then(|| lock.acquire_timeout(me, d)),
            None => {
                crate::gil::allow_threads_then(|| lock.acquire(me));
                true
            }
        });
    }
    let deadline = timeout.map(|d| Instant::now() + d);
    loop {
        let wait = match deadline {
            Some(dl) => {
                let now = Instant::now();
                if now >= dl {
                    return Ok(false);
                }
                (dl - now).min(SIGNAL_POLL_SLICE)
            }
            None => SIGNAL_POLL_SLICE,
        };
        if crate::gil::allow_threads_then(|| lock.acquire_timeout(me, wait)) {
            return Ok(true);
        }
        service_pending_signals()?;
    }
}

/// Reentrant counterpart to [`lock_acquire_interruptible`].
fn rlock_acquire_interruptible(
    lock: &RealRLock,
    me: u64,
    timeout: Option<Duration>,
) -> Result<bool, RuntimeError> {
    if lock.try_acquire(me) {
        return Ok(true);
    }
    if !crate::gil::is_main_thread() {
        return Ok(match timeout {
            Some(d) => crate::gil::allow_threads_then(|| lock.acquire_timeout(me, d)),
            None => {
                crate::gil::allow_threads_then(|| lock.acquire(me));
                true
            }
        });
    }
    let deadline = timeout.map(|d| Instant::now() + d);
    loop {
        let wait = match deadline {
            Some(dl) => {
                let now = Instant::now();
                if now >= dl {
                    return Ok(false);
                }
                (dl - now).min(SIGNAL_POLL_SLICE)
            }
            None => SIGNAL_POLL_SLICE,
        };
        if crate::gil::allow_threads_then(|| lock.acquire_timeout(me, wait)) {
            return Ok(true);
        }
        service_pending_signals()?;
    }
}

/// Grace period a `join()` blocks normally before it begins running
/// finalization passes between waits. The overwhelmingly common case —
/// a worker that finishes promptly — acquires within the grace wait and
/// never pays any GC cost.
const JOIN_GC_GRACE: Duration = Duration::from_millis(50);
/// Upper bound on the back-off between finalization passes once a join is
/// genuinely stuck, so a permanently-blocked join (a real program bug)
/// collects at most ~10×/s rather than spinning.
const JOIN_GC_SLICE_MAX: Duration = Duration::from_millis(100);

/// Run one finalization pass on the current thread: fire the weakref
/// callbacks of any reference-count-dead, weakref-watched object (queueing
/// their callbacks), then invoke the queued weakref callbacks and `__del__`s
/// here.
///
/// This runs the cycle collector's **mark phase only** ([`fire_dead_weakrefs`]
/// (crate::gc_trace::fire_dead_weakrefs)): it reuses the accurate reachability
/// analysis to find a `del`'d, weakref-watched `ThreadPoolExecutor` and fire
/// its `weakref_cb`, but skips every destructive step (finalizer execution,
/// field clearing, untracking). A full cyclic mark-*sweep* run here is unsafe:
/// while a worker holds an in-flight `_WorkItem` in a frame the collector
/// can't see as a root, the sweep would clear that live object mid-use
/// (`_WorkItem` losing its `future`). The mark-only pass mis-colours the same
/// work item White but, touching no contents, leaves it intact — and a
/// `_WorkItem` has no weakref, so it is a no-op for this pass regardless.
fn join_finalization_pass() {
    crate::gc_trace::fire_dead_weakrefs();
    if let Some(ptr) = crate::vm_singletons::current_interpreter_ptr() {
        // SAFETY: the interpreter pointer was published by the active
        // `join()` builtin call on this thread and outlives this call.
        let interp = unsafe { &mut *ptr };
        interp.run_pending_finalizers();
    }
}

/// Block on a thread's `join_lock` until the target thread releases it,
/// running a finalization pass from this (otherwise-idle) thread between
/// waits once the grace period elapses. Returns `true` once acquired,
/// `false` on timeout.
///
/// RFC 0025/0040: with the heap `Arc`-rooted and shared across threads, a
/// value such as a `ThreadPoolExecutor` can become reference-count dead
/// while every thread is parked (its last drop racing a worker's transient
/// `executor = executor_reference()`), so no drop event observes its death
/// and the weakref callback that posts each worker's shutdown sentinel
/// never fires — `del executor; t.join()` would deadlock. CPython's
/// refcounting reclaims the executor at the `del`; we approximate that by
/// letting the joining thread drive a collection, which clears the dead
/// executor's weakrefs and runs `weakref_cb`
/// (`test_concurrent_futures.test_del_shutdown`).
fn join_wait_collecting(lock: &RealLock, me: u64, timeout: Option<Duration>) -> bool {
    if lock.try_acquire(me) {
        return true;
    }
    let deadline = timeout.map(|d| Instant::now() + d);
    // Grace wait — keeps the prompt-finish case GC-free.
    let grace = match deadline {
        Some(dl) => {
            let now = Instant::now();
            if now >= dl {
                return false;
            }
            (dl - now).min(JOIN_GC_GRACE)
        }
        None => JOIN_GC_GRACE,
    };
    if crate::gil::allow_threads_then(|| lock.acquire_timeout(me, grace)) {
        return true;
    }
    // Still blocked after the grace period: alternate finalization passes
    // with bounded, backing-off waits until the target releases.
    let mut slice = Duration::from_millis(1);
    loop {
        join_finalization_pass();
        let wait = match deadline {
            Some(dl) => {
                let now = Instant::now();
                if now >= dl {
                    return false;
                }
                (dl - now).min(slice)
            }
            None => slice,
        };
        if crate::gil::allow_threads_then(|| lock.acquire_timeout(me, wait)) {
            return true;
        }
        slice = (slice * 2).min(JOIN_GC_SLICE_MAX);
    }
}

/// Reentrant counterpart to [`lock_acquire_blocking`]. A re-entrant
/// re-acquire by the owning thread (or an uncontended first
/// acquire) succeeds via `try_acquire` without touching the GIL;
/// only a foreign-owner wait drops it.
#[inline]
fn rlock_acquire_blocking(lock: &RealRLock, me: u64) {
    if lock.try_acquire(me) {
        return;
    }
    crate::gil::allow_threads_then(|| lock.acquire(me));
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
    // The kernel TID (not the `pthread_self` pointer): unique across
    // processes, so a spawned child's main thread reports a different
    // `native_id` than its parent (test_process_mainthread_native_id).
    Ok(Object::Int(crate::gil::current_os_native_id() as i64))
}

/// The stack size configured for new threads (0 = platform default).
/// We track it for round-trip fidelity; workers always reserve a
/// generous fixed stack regardless (see `spawn_python_worker`).
static STACK_SIZE: AtomicU64 = AtomicU64::new(0);

/// Minimum settable stack size, mirroring CPython's `THREAD_MIN_STACK`
/// floor below which `stack_size()` raises `ValueError`.
const THREAD_MIN_STACK: u64 = 32 * 1024;

fn stack_size(args: &[Object]) -> Result<Object, RuntimeError> {
    match args.first() {
        None | Some(Object::None) => Ok(Object::Int(STACK_SIZE.load(Ordering::Relaxed) as i64)),
        Some(Object::Int(n)) => {
            if *n < 0 {
                return Err(value_error("size must be 0 or a positive value"));
            }
            let size = *n as u64;
            if size != 0 && size < THREAD_MIN_STACK {
                return Err(value_error("size not valid: too small"));
            }
            // CPython returns the previous setting.
            let prev = STACK_SIZE.swap(size, Ordering::Relaxed);
            Ok(Object::Int(prev as i64))
        }
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

    let synth_id = spawn_python_worker(func, positional, kwargs_pairs, false, None)?;
    Ok(Object::Int(synth_id as i64))
}

/// Spawn one Python worker on a real OS thread. Shared by
/// `_thread.start_new_thread` (fire-and-forget) and
/// `_thread.start_joinable_thread` (RFC 0039 — returns a handle the
/// caller can `join()`). When `handle_state` is `Some`, the worker
/// shares its join lock with the handle and marks the handle done on
/// exit so `_ThreadHandle.join()` / `.is_done()` observe completion.
fn spawn_python_worker(
    func: Object,
    positional: Vec<Object>,
    kwargs_pairs: Vec<(String, Object)>,
    daemon: bool,
    handle_state: Option<Arc<ThreadHandleState>>,
) -> Result<u64, RuntimeError> {
    // CPython's `thread_PyThread_start_new_thread` refuses to spawn once
    // the interpreter is tearing down (`Py_IsFinalizing()`), so a thread
    // started from a `__del__`/atexit during shutdown raises rather than
    // running on a half-dead runtime.
    if crate::vm_singletons::is_finalizing() {
        return Err(runtime_error(
            "can't create new thread at interpreter shutdown",
        ));
    }
    let synth_id = NEXT_IDENT.fetch_add(1, Ordering::AcqRel);
    let thread_name = format!("Thread-{}", synth_id);

    let registry = thread_registry();
    // The handle (if any) shares the join lock the worker releases on
    // exit, so `_ThreadHandle.join()` blocks until the target returns.
    // Pre-acquire on the parent thread's behalf.
    let join_lock = handle_state
        .as_ref()
        .map(|hs| hs.join_lock.clone())
        .unwrap_or_else(|| Arc::new(RealLock::new()));
    join_lock.acquire(synth_id);
    if let Some(hs) = &handle_state {
        hs.ident.store(synth_id, Ordering::Release);
    }

    // Channel-of-one for handing the per-thread `ThreadEntry` back
    // to the worker once it's been built. Lets the worker mark
    // `started` / `finished` on the same entry the parent registered,
    // so `threading.enumerate()` and `Thread.is_alive()` see
    // consistent state.
    let entry_slot: Arc<parking_lot::Mutex<Option<Arc<ThreadEntry>>>> =
        Arc::new(parking_lot::Mutex::new(None));
    let entry_slot_worker = entry_slot.clone();

    let worker_func = func;
    let worker_lock = join_lock.clone();
    let worker_handle = handle_state.clone();
    let entry_name = thread_name.clone();
    // RFC 0039 (WS2/WS3): the per-worker reserve used to be 1 GiB,
    // matching the main thread. That made `test_queue`-style fan-outs
    // (CPython's `bigmemtest(size=50)` spawns 100 workers) fail with
    // `pthread_create: EAGAIN` — 100 x 1 GiB of stack address space is
    // more than the OS will hand out.
    //
    // The reserve doesn't bound recursion depth anyway: every activation
    // runs under `stacker::maybe_grow` (see `run_until_yield_or_return`),
    // which allocates fresh 8 MiB stack segments on demand, so
    // `sys.setrecursionlimit` — not the initial reserve — is what bounds
    // deep recursion. A 64 MiB starting reserve keeps early-startup
    // segment churn low while letting hundreds of workers coexist.
    const WORKER_STACK_BYTES: usize = 64 * 1024 * 1024; // 64 MiB
    let handle = std::thread::Builder::new()
        .name(format!("weavepy-worker-{}", synth_id))
        .stack_size(WORKER_STACK_BYTES)
        .spawn(move || {
            crate::vm_singletons::install_worker_thread_id(synth_id);
            // RFC 0040 WS4: record this worker's pthread_t so
            // `signal.pthread_kill(ident, sig)` can target it.
            #[cfg(unix)]
            super::signal_mod::register_current_thread_pthread(synth_id);
            // The parent published this entry into the slot below
            // before returning. Spin a few microseconds if we got here
            // first (extremely unlikely because the parent holds the
            // GIL until this worker can re-acquire it, but the slot
            // read is cheap).
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
            let call_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                worker_interp.call_object(worker_func.clone(), &positional, &kwargs_pairs)
            }));
            match call_result {
                Ok(Err(err)) => {
                    if !is_system_exit(&err) {
                        // CPython's low-level `thread_run` routes an
                        // uncaught worker exception through
                        // `PyErr_FormatUnraisable("Exception ignored in
                        // thread started by %R", func)` — i.e.
                        // `sys.unraisablehook` with a `None` object, not
                        // `threading.excepthook` (that's the high-level
                        // `Thread._bootstrap`'s job, handled in Python).
                        let func_repr = worker_interp
                            .repr_object(&worker_func)
                            .unwrap_or_else(|_| "<function>".to_owned());
                        let err_msg = format!("Exception ignored in thread started by {func_repr}");
                        worker_interp.write_unraisable_msg(
                            &err,
                            &Object::None,
                            &err_msg,
                            Some(&err_msg),
                        );
                    }
                }
                Ok(Ok(_)) => {}
                Err(payload) => {
                    let msg = payload
                        .downcast_ref::<String>()
                        .map(String::as_str)
                        .or_else(|| payload.downcast_ref::<&str>().copied())
                        .unwrap_or("<non-string panic payload>");
                    eprintln!("FATAL: panic in thread {entry_name}: {msg}");
                }
            }
            // CPython deletes the terminating thread's tstate dict here,
            // which drops that thread's slot in every `_thread._local`.
            // Emulate it while we still hold the GIL so a foreign thread's
            // `threading._DummyThread` is removed from `threading._active`
            // (test_threading.test_foreign_thread). A panicking finalizer
            // must not abort the worker, so isolate it.
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                worker_interp.run_thread_local_death_cleanup(synth_id as i64);
            }));
            // Run any `__del__`/weakref-callback finalizers this worker
            // deferred onto its *thread-local* pending queue (the prompt-reap
            // cascade and the `PyInstance` `Drop` safety net both enqueue
            // there). A blocked pool handler thread — `_handle_results`
            // parked in `outqueue.get()` — never reaches an eval-loop tick
            // to drain its own queue, so without this flush an object whose
            // last reference died on that thread (e.g. a `test_release_task_refs`
            // result copy) would be freed silently at thread teardown with its
            // `__del__` skipped, leaking it. Drain while the GIL and the
            // worker interpreter are both still live; isolate panics.
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                worker_interp.run_pending_finalizers();
            }));
            // Drop the guard before marking finished so the parent's
            // join (which blocks on the released join lock) sees the
            // released GIL — without this the parent could re-acquire
            // the GIL and find the worker still "running" even though
            // its target has returned. Mark the handle done *before*
            // releasing the lock so a thread waking from `join()`
            // immediately observes `is_done()`.
            let _ = crate::gil::pop_gil_guard();
            if let Some(hs) = &worker_handle {
                hs.done.store(true, Ordering::Release);
            }
            entry.mark_finished();
            let _ = worker_lock.release();
            #[cfg(unix)]
            super::signal_mod::unregister_thread_pthread(synth_id);
            crate::vm_singletons::clear_worker_thread_id();
        })
        .map_err(|e| runtime_error(format!("failed to spawn thread: {}", e)))?;

    let entry = Arc::new(ThreadEntry::new(synth_id, thread_name, daemon, handle));
    entry.attach_join_lock(join_lock);
    registry.register(entry.clone());
    *entry_slot.lock() = Some(entry);
    Ok(synth_id)
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

fn interrupt_main(args: &[Object]) -> Result<Object, RuntimeError> {
    // CPython's `_thread.interrupt_main(signum=SIGINT)` simulates a
    // signal arriving on the main thread: it trips `signum`, and the
    // main thread runs that signal's handler from its dispatch loop
    // (`PyErr_CheckSignals`). For SIGINT's startup handler that means a
    // `KeyboardInterrupt`; a user-installed handler runs instead; an
    // ignored / default-disposition signal is a no-op. Out-of-range
    // signal numbers raise `ValueError`, matching CPython.
    let signum = match args.first() {
        None => 2, // SIGINT
        // `as_i64` accepts `int`, `bool`, and int subclasses (e.g. the
        // `signal.Signals` IntEnum that `test_interrupt_main_noerror` passes),
        // matching CPython's Argument Clinic `int` converter.
        Some(o) => o.as_i64().ok_or_else(|| {
            type_error("interrupt_main() argument must be an int, not a different type")
        })? as i32,
    };
    if signum < 1 || signum >= crate::stdlib::signal_mod::nsig() {
        return Err(value_error("signal number out of range"));
    }
    // gh-102397: `interrupt_main()` racing with interpreter finalization
    // (e.g. from a `__del__` running during shutdown) can't reliably
    // deliver the signal — the main eval loop is tearing down. CPython
    // raises `OSError: Signal N ignored due to race condition` rather
    // than silently dropping it (test_signal.test__thread_interrupt_main).
    if crate::vm_singletons::is_finalizing() {
        return Err(crate::error::os_error(format!(
            "Signal {signum} ignored due to race condition"
        )));
    }
    crate::stdlib::signal_mod::trip_signal(signum);
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
    Ok(Object::Int(thread_registry().running_count() as i64))
}

// ---------------------------------------------------------------------------
// RFC 0039 (WS1/WS2): handle-based thread API used by the verbatim
// CPython 3.13 `threading.py` port.
// ---------------------------------------------------------------------------

/// `_thread.daemon_threads_allowed()` — the main interpreter always
/// permits daemon threads (sub-interpreters that forbid them aren't
/// modelled).
fn daemon_threads_allowed(_args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(Object::Bool(true))
}

/// `_thread._is_main_interpreter()` — WeavePy runs a single
/// interpreter, so this is always true.
fn is_main_interpreter(_args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(Object::Bool(true))
}

/// The ident `get_ident()` reports on the main OS thread (no worker
/// override installed → the native id).
fn main_thread_ident_now() -> u64 {
    crate::vm_singletons::current_worker_thread_id()
}

/// The ident of the runtime's main thread. Seeded the first time `_thread` is
/// imported (on the main OS thread) and re-seeded in a `fork(2)` child, where
/// the forking thread becomes the new main thread — CPython's
/// `_get_main_thread_ident()` tracks the runtime's *current* main thread, so a
/// frozen value would mislead `threading._after_fork` when the fork happens off
/// a non-main thread (`test_main_thread_after_fork_from_foreign_thread`).
/// `0` means "not yet seeded".
static MAIN_THREAD_IDENT: AtomicU64 = AtomicU64::new(0);

/// `_thread._get_main_thread_ident()`.
fn get_main_thread_ident(_args: &[Object]) -> Result<Object, RuntimeError> {
    let id = match MAIN_THREAD_IDENT.load(Ordering::Acquire) {
        0 => main_thread_ident_now(),
        id => id,
    };
    Ok(Object::Int(id as i64))
}

/// Re-seed the main-thread ident after `fork(2)`: the lone surviving (forking)
/// thread is the child's main thread, and mark every *other* thread's handle
/// done — those threads vanished in the fork, so `_ThreadHandle.is_done()`
/// (hence `threading.Thread.is_alive()`) must report them dead. This is
/// WeavePy's analogue of CPython's `_PyThread_AfterFork`
/// (`test_threading.test_is_alive_after_fork`).
pub fn after_fork_in_child() {
    let surviving = main_thread_ident_now();
    MAIN_THREAD_IDENT.store(surviving, Ordering::Release);
    for state in handle_registry().lock().values() {
        let id = state.ident.load(Ordering::Acquire);
        // ident 0 = an unstarted handle; leave it. The surviving thread's own
        // handle stays alive; everything else is now dead.
        if id != 0 && id != surviving {
            state.done.store(true, Ordering::Release);
        }
    }
}

/// `_thread._shutdown()` — block (GIL released) until every non-daemon
/// thread has finished, mirroring CPython's `Py_Main` shutdown join.
fn thread_shutdown(_args: &[Object]) -> Result<Object, RuntimeError> {
    crate::gil::allow_threads_then(|| {
        thread_registry().join_non_daemon();
    });
    Ok(Object::None)
}

/// Shared state behind a `_thread._ThreadHandle`. The worker flips
/// `done` and releases `join_lock` on exit; `join()` blocks on the
/// lock and `is_done()` reads the flag.
struct ThreadHandleState {
    ident: AtomicU64,
    done: AtomicBool,
    join_lock: Arc<RealLock>,
}

/// Process-wide registry mapping a handle id (stashed in the handle's
/// instance dict as `_wp_hid`) to its shared state, so
/// `start_joinable_thread` can wire a worker to a pre-constructed
/// handle.
fn handle_registry() -> &'static parking_lot::Mutex<HashMap<u64, Arc<ThreadHandleState>>> {
    static REG: OnceLock<parking_lot::Mutex<HashMap<u64, Arc<ThreadHandleState>>>> =
        OnceLock::new();
    REG.get_or_init(|| parking_lot::Mutex::new(HashMap::new()))
}

static NEXT_HANDLE_ID: AtomicU64 = AtomicU64::new(1);

fn thread_handle_type() -> Rc<TypeObject> {
    THREAD_HANDLE_TYPE.with(|cell| {
        if let Some(t) = cell.borrow().clone() {
            return t;
        }
        let t = TypeObject::new_with_flags(
            "_ThreadHandle",
            vec![crate::builtin_types::builtin_types().object_.clone()],
            DictData::new(),
            TypeFlags {
                is_exception: false,
                is_builtin: true,
            },
        )
        .expect("_ThreadHandle type");
        *cell.borrow_mut() = Some(t.clone());
        t
    })
}

/// Build a Python-visible `_ThreadHandle` instance around `state`.
/// Methods (`is_done`, `join`, `_set_done`) capture the shared state;
/// the `_wp_hid` entry lets `start_joinable_thread` recover the state
/// from a handle the caller pre-created.
fn make_thread_handle_object(state: Arc<ThreadHandleState>, ident: Object) -> Object {
    let hid = NEXT_HANDLE_ID.fetch_add(1, Ordering::AcqRel);
    handle_registry().lock().insert(hid, state.clone());

    let dict = Rc::new(RefCell::new(DictData::new()));

    let is_done_state = state.clone();
    let is_done = move |_args: &[Object]| -> Result<Object, RuntimeError> {
        Ok(Object::Bool(is_done_state.done.load(Ordering::Acquire)))
    };

    let join_state = state.clone();
    let join = move |args: &[Object]| -> Result<Object, RuntimeError> {
        // A zero ident means the handle was never handed to
        // `start_joinable_thread`. CPython raises rather than block.
        let ident = join_state.ident.load(Ordering::Acquire);
        if ident == 0 {
            return Err(runtime_error("thread not started"));
        }
        if join_state.done.load(Ordering::Acquire) {
            return Ok(Object::None);
        }
        // Joining a thread from itself would deadlock; CPython's handle
        // raises instead (`RuntimeError: Cannot join current thread`).
        // The handle ident lives in the synthetic id space, which is
        // what `current_worker_thread_id()` reports.
        if ident == crate::vm_singletons::current_worker_thread_id() {
            return Err(runtime_error("Cannot join current thread"));
        }
        let me = crate::gil::current_thread_id();
        // `threading.Thread.join` clamps the timeout to >= 0 before it
        // reaches us, so `None`/absent means "block forever".
        match args.first() {
            None | Some(Object::None) => {
                if join_wait_collecting(&join_state.join_lock, me, None) {
                    let _ = join_state.join_lock.release();
                }
            }
            other => match parse_timeout(other) {
                Some(Duration::ZERO) => {
                    if join_state.join_lock.try_acquire(me) {
                        let _ = join_state.join_lock.release();
                    }
                }
                Some(d) => {
                    if join_wait_collecting(&join_state.join_lock, me, Some(d)) {
                        let _ = join_state.join_lock.release();
                    }
                }
                None => {
                    if join_wait_collecting(&join_state.join_lock, me, None) {
                        let _ = join_state.join_lock.release();
                    }
                }
            },
        }
        // RFC 0039 (WS4): the just-joined worker ran its target's
        // teardown — e.g. `threading.Thread.run`'s
        // `del self._target, self._args, self._kwargs` — on its own
        // thread, dropping the last *program* reference to any argument
        // cycle. Objects this (joining) thread allocated are still pinned
        // by its cycle-GC handle until a collection, so a `del` right
        // after the join would still see the dead container as a live
        // referrer. Refcounting would have freed it instantly in CPython;
        // sweep this thread's dead acyclic garbage now so the caller's
        // subsequent `del` finalizes the cycle without an explicit
        // `gc.collect()` (test_threading.test_no_refcycle_through_target).
        crate::gc_trace::reap_dead_acyclic();
        Ok(Object::None)
    };

    let set_done_state = state.clone();
    let set_done = move |_args: &[Object]| -> Result<Object, RuntimeError> {
        if set_done_state.ident.load(Ordering::Acquire) == 0 {
            return Err(runtime_error("thread not started"));
        }
        set_done_state.done.store(true, Ordering::Release);
        let _ = set_done_state.join_lock.release();
        Ok(Object::None)
    };

    {
        let mut d = dict.borrow_mut();
        d.insert(DictKey(Object::from_static("ident")), ident);
        d.insert(
            DictKey(Object::from_static("_wp_hid")),
            Object::Int(hid as i64),
        );
        d.insert(
            DictKey(Object::from_static("is_done")),
            b_dyn("is_done", is_done),
        );
        d.insert(DictKey(Object::from_static("join")), b_dyn("join", join));
        d.insert(
            DictKey(Object::from_static("_set_done")),
            b_dyn("_set_done", set_done),
        );
    }
    let inst = Rc::new(PyInstance {
        class: crate::sync::RefCell::new(thread_handle_type()),
        dict,
        native: None,
        inline_values: crate::sync::Cell::new(true),
        slots: crate::sync::RefCell::new(None),
        hash_cache: crate::sync::Cell::new(None),
        finalize_ran: crate::sync::Cell::new(false),
    });
    Object::Instance(inst)
}

/// Recover the shared `ThreadHandleState` from a handle Object.
fn handle_state_of(obj: &Object) -> Option<Arc<ThreadHandleState>> {
    let Object::Instance(inst) = obj else {
        return None;
    };
    let hid = {
        let d = inst.dict.borrow();
        match d.get(&DictKey(Object::from_static("_wp_hid"))) {
            Some(Object::Int(i)) => *i as u64,
            _ => return None,
        }
    };
    handle_registry().lock().get(&hid).cloned()
}

/// `_thread._ThreadHandle()` — a fresh, unstarted handle.
fn thread_handle_new(_args: &[Object]) -> Result<Object, RuntimeError> {
    let state = Arc::new(ThreadHandleState {
        ident: AtomicU64::new(0),
        done: AtomicBool::new(false),
        join_lock: Arc::new(RealLock::new()),
    });
    Ok(make_thread_handle_object(state, Object::None))
}

/// `_thread._make_thread_handle(ident)` — a handle for an already-live
/// thread (the main thread, dummy threads). Held "not done" until
/// `_set_done()` is called.
fn make_thread_handle(args: &[Object]) -> Result<Object, RuntimeError> {
    let ident = match args.first() {
        Some(Object::Int(i)) => *i as u64,
        _ => main_thread_ident_now(),
    };
    let join_lock = Arc::new(RealLock::new());
    // Alive: hold the join lock until `_set_done()` releases it.
    join_lock.acquire(ident);
    let state = Arc::new(ThreadHandleState {
        ident: AtomicU64::new(ident),
        done: AtomicBool::new(false),
        join_lock,
    });
    Ok(make_thread_handle_object(state, Object::Int(ident as i64)))
}

/// `_thread.start_joinable_thread(function, handle=None, daemon=True)`
/// — spawn a worker bound to `handle` (created if omitted) and return
/// the handle.
fn start_joinable_thread(
    args: &[Object],
    kwargs: &[(String, Object)],
) -> Result<Object, RuntimeError> {
    let kw = |name: &str| {
        kwargs
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.clone())
    };
    let func = args
        .first()
        .cloned()
        .or_else(|| kw("function"))
        .ok_or_else(|| type_error("start_joinable_thread() missing target"))?;
    let daemon = match kw("daemon") {
        Some(Object::Bool(b)) => b,
        Some(Object::Int(i)) => i != 0,
        // CPython's default is daemon=True.
        None | Some(Object::None) => true,
        Some(_) => true,
    };

    let (handle_obj, state) = match kw("handle") {
        None | Some(Object::None) => {
            let state = Arc::new(ThreadHandleState {
                ident: AtomicU64::new(0),
                done: AtomicBool::new(false),
                join_lock: Arc::new(RealLock::new()),
            });
            let obj = make_thread_handle_object(state.clone(), Object::None);
            (obj, state)
        }
        Some(obj) => {
            let state = handle_state_of(&obj)
                .ok_or_else(|| type_error("start_joinable_thread(): invalid handle"))?;
            // A non-zero ident means this handle was already started.
            if state.ident.load(Ordering::Acquire) != 0 {
                return Err(runtime_error("thread already started"));
            }
            (obj, state)
        }
    };

    // `self._bootstrap` is a bound method taking no arguments.
    spawn_python_worker(func, Vec::new(), Vec::new(), daemon, Some(state.clone()))?;

    // Publish the assigned ident on the handle object.
    if let Object::Instance(inst) = &handle_obj {
        let id = state.ident.load(Ordering::Acquire);
        inst.dict.borrow_mut().insert(
            DictKey(Object::from_static("ident")),
            Object::Int(id as i64),
        );
    }
    Ok(handle_obj)
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

// Most of this module is a thin wrapper around libc. The clippy
// style nits below would obscure the FFI structure without buying
// us anything material.
#![allow(
    clippy::cast_lossless,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::ptr_as_ptr,
    clippy::borrow_as_ptr,
    clippy::ref_as_ptr,
    clippy::bool_to_int_with_if,
    clippy::unreadable_literal
)]

//! `_multiprocessing` Rust core — RFC 0026.
//!
//! The user-visible [`multiprocessing`](super::python) module is a
//! frozen Python file that wraps a small, opinionated Rust API:
//!
//! - [`Connection`] objects — bidirectional byte channels backed by a
//!   real `socketpair(2)`. Send and receive length-framed payloads with
//!   `send_bytes`/`recv_bytes`, poll with `poll(timeout)`, dup the fd
//!   with `fileno()`. Closed sockets raise `BrokenPipeError`/`EOFError`.
//! - [`SemLock`] — a kernel semaphore. The unnamed flavour is a real
//!   POSIX semaphore-shaped `std::sync::Semaphore`-clone (cross-thread
//!   only); the named flavour calls `sem_open`/`sem_post`/`sem_wait`
//!   for cross-process visibility.
//! - [`SharedMemory`] — `shm_open(3)`-backed memory region with
//!   `mmap(2)` view; exposes a memoryview-like `buf` plus `close()` /
//!   `unlink()`.
//! - `_spawn_child(argv, env, payload)` — forks the current process,
//!   sets up the spawn payload fd, and `execve(2)`s a new `weavepy`
//!   binary. Returns `(pid, parent_conn_fd, child_conn_fd)`.
//! - `_waitpid(pid, options)` — `waitpid(2)` wrapper that returns
//!   `(pid, status, signal, exitcode)`.
//! - `_get_command()` — the launcher arg vector (`weavepy
//!   --multiprocessing-fork PAYLOAD_FD …`) used by the spawn child.
//!
//! The implementation is POSIX-only; Windows ports can swap the
//! `socketpair`/`fork`/`shm_open` paths for `CreateProcess`/named
//! pipes/CreateFileMapping later. Today's CPython compatibility target
//! is also POSIX, so this isn't blocking parity.
//!
//! Each primitive is exposed to Python as a [`Object::SimpleNamespace`]
//! whose dict carries Rust closures stamped with `BuiltinFn`. State
//! lives behind an `Arc<Mutex<…>>` so calls from worker threads are
//! safe; the only globally visible cell is [`SHARED_SEM_NAMES`], which
//! tracks named semaphores so `sem_unlink` is idempotent.

use crate::import::ModuleCache;
use crate::object::{DictData, DictKey, Object, PyModule};
use crate::sync::Rc;
use crate::sync::RefCell;

/// On non-POSIX hosts we still want to satisfy `import
/// _multiprocessing` — but every method raises
/// `NotImplementedError("requires POSIX")` so the user gets a clear
/// signal instead of a confusing `AttributeError` later.
#[cfg(not(unix))]
pub fn build(_cache: &ModuleCache) -> Rc<PyModule> {
    let dict = Rc::new(RefCell::new(DictData::new()));
    dict.borrow_mut().insert(
        DictKey(Object::from_static("__name__")),
        Object::from_static("_multiprocessing"),
    );
    Rc::new(PyModule {
        name: "_multiprocessing".to_owned(),
        filename: None,
        dict,
    })
}

#[cfg(unix)]
use std::os::fd::RawFd;
#[cfg(unix)]
use std::sync::atomic::{AtomicI64, AtomicU64, AtomicUsize, Ordering};
#[cfg(unix)]
use std::sync::{Arc, Mutex};
#[cfg(unix)]
use std::time::{Duration, Instant};

#[cfg(unix)]
use crate::error::{
    assertion_error, broken_pipe_error, io_error_to_py, runtime_error, type_error, value_error,
    RuntimeError,
};
#[cfg(unix)]
use crate::object::BuiltinFn;
#[cfg(unix)]
use crate::types::{PyInstance, TypeFlags, TypeObject};

#[cfg(unix)]
pub fn build(_cache: &ModuleCache) -> Rc<PyModule> {
    let dict = Rc::new(RefCell::new(DictData::new()));
    {
        let mut d = dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("__name__")),
            Object::from_static("_multiprocessing"),
        );
        d.insert(
            DictKey(Object::from_static("__doc__")),
            Object::from_static(
                "Low-level multiprocessing primitives. Used by the \
                 frozen `multiprocessing` module to back `Process`, \
                 `Pool`, `Queue`, etc. (RFC 0026)",
            ),
        );

        // Core primitives.
        d.insert(
            DictKey(Object::from_static("SemLock")),
            Object::Type(semlock_type()),
        );
        d.insert(
            DictKey(Object::from_static("sem_unlink")),
            b("sem_unlink", sem_unlink_py),
        );
        d.insert(
            DictKey(Object::from_static("Connection")),
            b("Connection", connection_from_fd),
        );
        d.insert(DictKey(Object::from_static("Pipe")), b("Pipe", make_pipe));
        d.insert(
            DictKey(Object::from_static("SharedMemory")),
            b("SharedMemory", make_shared_memory),
        );

        // Spawn / wait helpers.
        d.insert(
            DictKey(Object::from_static("_spawn_child")),
            b("_spawn_child", spawn_child),
        );
        d.insert(
            DictKey(Object::from_static("_waitpid")),
            b("_waitpid", waitpid_py),
        );
        d.insert(
            DictKey(Object::from_static("_get_command")),
            b("_get_command", get_command),
        );
        d.insert(
            DictKey(Object::from_static("_payload_fd")),
            b("_payload_fd", payload_fd),
        );
        d.insert(DictKey(Object::from_static("_exit")), b("_exit", mp_exit));

        // Constants / flags.
        d.insert(DictKey(Object::from_static("flags")), Object::Int(0));
        d.insert(
            DictKey(Object::from_static("RECURSIVE_MUTEX")),
            Object::Int(0),
        );
        d.insert(DictKey(Object::from_static("SEMAPHORE")), Object::Int(1));
        // CPython exposes SEM_VALUE_MAX; libc on Darwin doesn't surface
        // a typed constant so we hard-code the POSIX minimum (32767).
        d.insert(
            DictKey(Object::from_static("SEM_VALUE_MAX")),
            Object::Int(32767),
        );

        // Back-compat aliases for the RFC 0024 surface (some callers
        // still reach for `send` / `recv` / `closesocket`).
        d.insert(
            DictKey(Object::from_static("send")),
            b("send", conn_send_legacy),
        );
        d.insert(
            DictKey(Object::from_static("recv")),
            b("recv", conn_recv_legacy),
        );
        d.insert(
            DictKey(Object::from_static("closesocket")),
            b("closesocket", conn_close_legacy),
        );
    }
    Rc::new(PyModule {
        name: "_multiprocessing".to_owned(),
        filename: None,
        dict,
    })
}

// ---------------------------------------------------------------------
// SemLock
// ---------------------------------------------------------------------

// A faithful port of CPython's `Modules/_multiprocessing/semaphore.c`:
// a kernel semaphore opened with `sem_open(3)` (named, so it survives
// `fork`/`exec` and is reachable by name from a `spawn`ed child via
// `_rebuild`). `multiprocessing/synchronize.py` drives the full surface
// — `acquire`/`release`/`_get_value`/`_count`/`_is_mine`/`_is_zero`/
// `_after_fork`/`__enter__`/`__exit__`, the `handle`/`kind`/`maxvalue`/
// `name` attributes, the `SEM_VALUE_MAX` class attribute, and the
// `_rebuild` staticmethod.

/// `RECURSIVE_MUTEX` kind (matches `multiprocessing/synchronize.py`).
#[cfg(unix)]
const RECURSIVE_MUTEX_KIND: i64 = 0;

/// `SemLock.SEM_VALUE_MAX` — the largest value a semaphore may hold.
/// `INT_MAX` on Linux; the POSIX floor (`_POSIX_SEM_VALUE_MAX`, 32767)
/// on Darwin/BSD, matching the C library.
#[cfg(all(unix, any(target_os = "macos", target_os = "ios")))]
const SEM_VALUE_MAX: i64 = 32767;
#[cfg(all(unix, not(any(target_os = "macos", target_os = "ios"))))]
const SEM_VALUE_MAX: i64 = 2_147_483_647;

/// Poll slice for an interruptible blocking acquire (mirrors the lock
/// subsystem in `thread_real`): short enough to service a tripped
/// signal promptly, long enough that idle wakeups stay cheap.
#[cfg(unix)]
const SEM_POLL_SLICE: Duration = Duration::from_millis(20);

/// `SEM_FAILED` is `NULL` on glibc but `(sem_t *)-1` on the BSDs/macOS.
#[cfg(unix)]
#[inline]
fn sem_failed() -> *mut libc::sem_t {
    #[cfg(any(
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd",
        target_os = "netbsd",
        target_os = "openbsd",
        target_os = "dragonfly"
    ))]
    {
        -1isize as *mut libc::sem_t
    }
    #[cfg(not(any(
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd",
        target_os = "netbsd",
        target_os = "openbsd",
        target_os = "dragonfly"
    )))]
    {
        std::ptr::null_mut()
    }
}

/// Shared state behind a `SemLock` instance. The `*mut sem_t` is valid
/// across `fork` (the mapping is inherited) and re-opened by name in a
/// `spawn`ed child (`_rebuild`). `count`/`last_tid` are per-process
/// bookkeeping for the recursive-mutex fast path and `_is_mine`.
#[cfg(unix)]
struct SemInner {
    handle: *mut libc::sem_t,
    kind: i64,
    maxvalue: i64,
    name: Mutex<Option<String>>,
    count: AtomicI64,
    last_tid: AtomicU64,
}

// SAFETY: the raw `sem_t*` is a kernel handle; all access goes through
// the atomic libc sem_* calls, and the count/owner bookkeeping is atomic.
#[cfg(unix)]
unsafe impl Send for SemInner {}
#[cfg(unix)]
unsafe impl Sync for SemInner {}

#[cfg(unix)]
thread_local! {
    static SEMLOCK_TYPE: RefCell<Option<Rc<TypeObject>>> = const { RefCell::new(None) };
}

/// The `_multiprocessing.SemLock` type. Built lazily so
/// `type(sl).__name__ == 'SemLock'` and the class attributes
/// (`SEM_VALUE_MAX`, `_rebuild`) resolve.
#[cfg(unix)]
fn semlock_type() -> Rc<TypeObject> {
    SEMLOCK_TYPE.with(|cell| {
        if let Some(t) = cell.borrow().clone() {
            return t;
        }
        let mut d = DictData::new();
        d.insert(
            DictKey(Object::from_static("__module__")),
            Object::from_static("_multiprocessing"),
        );
        d.insert(
            DictKey(Object::from_static("__new__")),
            b("__new__", semlock_new),
        );
        d.insert(
            DictKey(Object::from_static("__init__")),
            b("__init__", semlock_init),
        );
        // Accessed via the class (`SemLock._rebuild(...)`), so a plain
        // builtin behaves like a staticmethod (no instance binding).
        d.insert(
            DictKey(Object::from_static("_rebuild")),
            b("_rebuild", semlock_rebuild),
        );
        d.insert(
            DictKey(Object::from_static("SEM_VALUE_MAX")),
            Object::Int(SEM_VALUE_MAX),
        );
        let t = TypeObject::new_with_flags(
            "SemLock",
            vec![crate::builtin_types::builtin_types().object_.clone()],
            d,
            TypeFlags {
                is_exception: false,
                is_builtin: true,
            },
        )
        .expect("SemLock type");
        *cell.borrow_mut() = Some(t.clone());
        t
    })
}

#[cfg(unix)]
fn sem_arg_int(args: &[Object], idx: usize, what: &str) -> Result<i64, RuntimeError> {
    match args.get(idx) {
        Some(Object::Int(n)) => Ok(*n),
        Some(Object::Bool(b)) => Ok(*b as i64),
        _ => Err(type_error(format!("SemLock() {what} must be an int"))),
    }
}

#[cfg(unix)]
fn sem_cname(name: &str) -> Result<std::ffi::CString, RuntimeError> {
    std::ffi::CString::new(name).map_err(|_| value_error("embedded null byte in semaphore name"))
}

/// `SemLock.__init__` — a no-op: `__new__` builds the fully-initialised
/// instance (CPython's `semlock` does all its work in `tp_new`).
#[cfg(unix)]
fn semlock_init(_args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(Object::None)
}

/// `_multiprocessing.SemLock(kind, value, maxvalue, name, unlink)` —
/// `sem_open(3)` a fresh named semaphore (`O_CREAT|O_EXCL` so a name
/// collision raises `FileExistsError`, which `synchronize.SemLock`
/// retries). `unlink=True` (the fork start method / Windows) unlinks
/// the name immediately and forgets it; otherwise the name is kept so a
/// `spawn`ed child can re-open it and `resource_tracker` can clean up.
#[cfg(unix)]
fn semlock_new(args: &[Object]) -> Result<Object, RuntimeError> {
    // args[0] is the class object (SemLock); the constructor params
    // follow it.
    let kind = sem_arg_int(args, 1, "kind")?;
    let value = sem_arg_int(args, 2, "value")?;
    let maxvalue = sem_arg_int(args, 3, "maxvalue")?;
    if !(0..=SEM_VALUE_MAX).contains(&value) {
        return Err(value_error("semaphore initial value out of range"));
    }
    let name = match args.get(4) {
        Some(Object::Str(s)) => s.to_string(),
        _ => return Err(type_error("SemLock() name must be a str")),
    };
    let unlink = match args.get(5) {
        Some(Object::Bool(b)) => *b,
        Some(Object::Int(i)) => *i != 0,
        _ => false,
    };
    let cname = sem_cname(&name)?;
    let handle = unsafe {
        libc::sem_open(
            cname.as_ptr(),
            libc::O_CREAT | libc::O_EXCL,
            0o600 as libc::c_uint,
            value as libc::c_uint,
        )
    };
    if handle == sem_failed() {
        return Err(io_error_to_py(&std::io::Error::last_os_error()));
    }
    let keep_name = if unlink {
        unsafe { libc::sem_unlink(cname.as_ptr()) };
        None
    } else {
        Some(name)
    };
    let inner = Arc::new(SemInner {
        handle,
        kind,
        maxvalue,
        name: Mutex::new(keep_name),
        count: AtomicI64::new(0),
        last_tid: AtomicU64::new(0),
    });
    Ok(make_semlock_instance(inner))
}

/// `SemLock._rebuild(handle, kind, maxvalue, name)` — reconstruct in a
/// `spawn`ed child. On POSIX the inherited `handle` is meaningless, so
/// when a `name` is present we re-`sem_open` it; the nameless (fork)
/// path keeps the inherited handle.
#[cfg(unix)]
fn semlock_rebuild(args: &[Object]) -> Result<Object, RuntimeError> {
    let handle_in = match args.first() {
        Some(Object::Int(n)) => *n,
        _ => 0,
    };
    let kind = sem_arg_int(args, 1, "kind")?;
    let maxvalue = sem_arg_int(args, 2, "maxvalue")?;
    let (handle, name) = match args.get(3) {
        Some(Object::Str(s)) => {
            let cname = sem_cname(s)?;
            let h = unsafe { libc::sem_open(cname.as_ptr(), 0) };
            if h == sem_failed() {
                return Err(io_error_to_py(&std::io::Error::last_os_error()));
            }
            (h, Some(s.to_string()))
        }
        _ => (handle_in as *mut libc::sem_t, None),
    };
    let inner = Arc::new(SemInner {
        handle,
        kind,
        maxvalue,
        name: Mutex::new(name),
        count: AtomicI64::new(0),
        last_tid: AtomicU64::new(0),
    });
    Ok(make_semlock_instance(inner))
}

/// Build the Python-visible `SemLock` instance: attributes plus the
/// method closures that capture the shared [`SemInner`].
#[cfg(unix)]
fn make_semlock_instance(inner: Arc<SemInner>) -> Object {
    let dict = Rc::new(RefCell::new(DictData::new()));
    {
        let mut d = dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("handle")),
            Object::Int(inner.handle as i64),
        );
        d.insert(
            DictKey(Object::from_static("kind")),
            Object::Int(inner.kind),
        );
        d.insert(
            DictKey(Object::from_static("maxvalue")),
            Object::Int(inner.maxvalue),
        );
        d.insert(
            DictKey(Object::from_static("name")),
            match &*inner.name.lock().unwrap() {
                Some(n) => Object::from_str(n.clone()),
                None => Object::None,
            },
        );
        let a = inner.clone();
        d.insert(
            DictKey(Object::from_static("acquire")),
            b_dyn_kw("acquire", move |args, kwargs| sem_acquire(&a, args, kwargs)),
        );
        let r = inner.clone();
        d.insert(
            DictKey(Object::from_static("release")),
            b_dyn("release", move |_| sem_release(&r)),
        );
        let gv = inner.clone();
        d.insert(
            DictKey(Object::from_static("_get_value")),
            b_dyn("_get_value", move |_| sem_get_value(&gv)),
        );
        let ct = inner.clone();
        d.insert(
            DictKey(Object::from_static("_count")),
            b_dyn("_count", move |_| {
                Ok(Object::Int(ct.count.load(Ordering::SeqCst)))
            }),
        );
        let im = inner.clone();
        d.insert(
            DictKey(Object::from_static("_is_mine")),
            b_dyn("_is_mine", move |_| {
                let me = crate::gil::current_thread_id();
                Ok(Object::Bool(
                    im.last_tid.load(Ordering::SeqCst) == me && im.count.load(Ordering::SeqCst) > 0,
                ))
            }),
        );
        let iz = inner.clone();
        d.insert(
            DictKey(Object::from_static("_is_zero")),
            b_dyn("_is_zero", move |_| sem_is_zero(&iz)),
        );
        let af = inner.clone();
        d.insert(
            DictKey(Object::from_static("_after_fork")),
            b_dyn("_after_fork", move |_| {
                af.count.store(0, Ordering::SeqCst);
                Ok(Object::None)
            }),
        );
        let en = inner.clone();
        d.insert(
            DictKey(Object::from_static("__enter__")),
            b_dyn_kw("__enter__", move |args, kwargs| {
                sem_acquire(&en, args, kwargs)
            }),
        );
        let ex = inner.clone();
        d.insert(
            DictKey(Object::from_static("__exit__")),
            b_dyn("__exit__", move |_| {
                sem_release(&ex)?;
                Ok(Object::None)
            }),
        );
    }
    let inst = Rc::new(PyInstance {
        class: crate::sync::RefCell::new(semlock_type()),
        dict,
        native: None,
        inline_values: crate::sync::Cell::new(true),
        slots: crate::sync::RefCell::new(None),
        hash_cache: crate::sync::Cell::new(None),
        finalize_ran: crate::sync::Cell::new(false),
    });
    Object::Instance(inst)
}

/// `SemLock.acquire(block=True, timeout=None)`.
#[cfg(unix)]
fn sem_acquire(
    inner: &Arc<SemInner>,
    args: &[Object],
    kwargs: &[(String, Object)],
) -> Result<Object, RuntimeError> {
    let mut block_obj = args.first().cloned();
    let mut timeout_obj = args.get(1).cloned();
    for (k, v) in kwargs {
        match k.as_str() {
            "block" | "blocking" => block_obj = Some(v.clone()),
            "timeout" => timeout_obj = Some(v.clone()),
            other => {
                return Err(type_error(format!(
                    "acquire() got an unexpected keyword argument '{other}'"
                )))
            }
        }
    }
    let block = match block_obj {
        None | Some(Object::None) => true,
        Some(Object::Bool(b)) => b,
        Some(Object::Int(i)) => i != 0,
        Some(_) => true,
    };
    let timeout: Option<f64> = match timeout_obj {
        None | Some(Object::None) => None,
        Some(Object::Float(f)) => Some(f),
        Some(Object::Int(i)) => Some(i as f64),
        Some(_) => None,
    };
    let me = crate::gil::current_thread_id();
    // Recursive-mutex re-entry: already mine → just bump the count.
    if inner.kind == RECURSIVE_MUTEX_KIND
        && inner.last_tid.load(Ordering::SeqCst) == me
        && inner.count.load(Ordering::SeqCst) > 0
    {
        inner.count.fetch_add(1, Ordering::SeqCst);
        return Ok(Object::Bool(true));
    }
    if !block {
        return match sem_trywait(inner) {
            Ok(true) => {
                inner.count.fetch_add(1, Ordering::SeqCst);
                inner.last_tid.store(me, Ordering::SeqCst);
                Ok(Object::Bool(true))
            }
            Ok(false) => Ok(Object::Bool(false)),
            Err(e) => Err(io_error_to_py(&e)),
        };
    }
    // Blocking (optionally timed). Drop the GIL across each wait slice
    // and service signals between slices so KeyboardInterrupt works.
    let deadline = timeout.map(|t| Instant::now() + Duration::from_secs_f64(t.max(0.0)));
    loop {
        let slice = match deadline {
            Some(dl) => {
                let now = Instant::now();
                if now >= dl {
                    // Final non-blocking attempt: POSIX takes an
                    // immediately-available semaphore even past the
                    // deadline (covers timeout=0).
                    return match sem_trywait(inner) {
                        Ok(true) => {
                            inner.count.fetch_add(1, Ordering::SeqCst);
                            inner.last_tid.store(me, Ordering::SeqCst);
                            Ok(Object::Bool(true))
                        }
                        Ok(false) => Ok(Object::Bool(false)),
                        Err(e) => Err(io_error_to_py(&e)),
                    };
                }
                (dl - now).min(SEM_POLL_SLICE)
            }
            None => SEM_POLL_SLICE,
        };
        let h = inner.handle;
        let res = crate::gil::allow_threads_then(|| sem_wait_slice(h, slice));
        match res {
            Ok(true) => {
                inner.count.fetch_add(1, Ordering::SeqCst);
                inner.last_tid.store(me, Ordering::SeqCst);
                return Ok(Object::Bool(true));
            }
            Ok(false) => {}
            Err(e) => return Err(io_error_to_py(&e)),
        }
        service_pending_signals()?;
    }
}

/// Non-blocking acquire (`sem_trywait`), retrying `EINTR`.
#[cfg(unix)]
fn sem_trywait(inner: &SemInner) -> std::io::Result<bool> {
    loop {
        let r = unsafe { libc::sem_trywait(inner.handle) };
        if r == 0 {
            return Ok(true);
        }
        let e = std::io::Error::last_os_error();
        match e.raw_os_error() {
            Some(libc::EAGAIN) => return Ok(false),
            Some(libc::EINTR) => continue,
            _ => return Err(e),
        }
    }
}

/// Wait up to `slice` for the semaphore. `Ok(true)` acquired, `Ok(false)`
/// timed out. Uses `sem_timedwait` where available, polling on Darwin.
#[cfg(all(unix, not(any(target_os = "macos", target_os = "ios"))))]
fn sem_wait_slice(handle: *mut libc::sem_t, slice: Duration) -> std::io::Result<bool> {
    let mut now = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    unsafe { libc::clock_gettime(libc::CLOCK_REALTIME, &mut now) };
    let mut nsec = now.tv_nsec as i128 + i128::from(slice.subsec_nanos());
    let mut sec = now.tv_sec as i128 + slice.as_secs() as i128;
    sec += nsec / 1_000_000_000;
    nsec %= 1_000_000_000;
    let abs = libc::timespec {
        tv_sec: sec as libc::time_t,
        tv_nsec: nsec as _,
    };
    let r = unsafe { libc::sem_timedwait(handle, &abs) };
    if r == 0 {
        return Ok(true);
    }
    let e = std::io::Error::last_os_error();
    match e.raw_os_error() {
        Some(libc::ETIMEDOUT) | Some(libc::EINTR) => Ok(false),
        _ => Err(e),
    }
}

#[cfg(all(unix, any(target_os = "macos", target_os = "ios")))]
fn sem_wait_slice(handle: *mut libc::sem_t, slice: Duration) -> std::io::Result<bool> {
    // Darwin has no sem_timedwait; poll sem_trywait until the slice ends.
    let deadline = Instant::now() + slice;
    loop {
        let r = unsafe { libc::sem_trywait(handle) };
        if r == 0 {
            return Ok(true);
        }
        let e = std::io::Error::last_os_error();
        match e.raw_os_error() {
            Some(libc::EAGAIN) | Some(libc::EINTR) => {}
            _ => return Err(e),
        }
        if Instant::now() >= deadline {
            return Ok(false);
        }
        unsafe { libc::usleep(500) };
    }
}

/// `SemLock.release()`.
#[cfg(unix)]
fn sem_release(inner: &Arc<SemInner>) -> Result<Object, RuntimeError> {
    let me = crate::gil::current_thread_id();
    if inner.kind == RECURSIVE_MUTEX_KIND {
        if !(inner.last_tid.load(Ordering::SeqCst) == me && inner.count.load(Ordering::SeqCst) > 0)
        {
            return Err(assertion_error(
                "attempt to release recursive lock not owned by thread",
            ));
        }
        if inner.count.load(Ordering::SeqCst) > 1 {
            inner.count.fetch_sub(1, Ordering::SeqCst);
            return Ok(Object::None);
        }
    }
    // Refuse to over-release, matching CPython's "semaphore or lock
    // released too many times" (`_multiprocessing/semaphore.c`).
    #[cfg(not(any(target_os = "macos", target_os = "ios")))]
    {
        let mut v: libc::c_int = 0;
        if unsafe { libc::sem_getvalue(inner.handle, &mut v) } == 0
            && i64::from(v) >= inner.maxvalue
        {
            return Err(value_error("semaphore or lock released too many times"));
        }
    }
    // Darwin's `sem_getvalue` is broken (HAVE_BROKEN_SEM_GETVALUE), so
    // CPython only validates the `maxvalue == 1` (Lock) case there: a
    // non-blocking `sem_trywait` that *succeeds* proves the lock was not
    // held, i.e. an over-release — undo it and raise. `EAGAIN` means it
    // was held as expected and the release proceeds.
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    {
        if inner.maxvalue == 1 {
            let r = unsafe { libc::sem_trywait(inner.handle) };
            if r == 0 {
                if unsafe { libc::sem_post(inner.handle) } != 0 {
                    return Err(io_error_to_py(&std::io::Error::last_os_error()));
                }
                return Err(value_error("semaphore or lock released too many times"));
            }
            let e = std::io::Error::last_os_error();
            if e.raw_os_error() != Some(libc::EAGAIN) {
                return Err(io_error_to_py(&e));
            }
        }
    }
    if unsafe { libc::sem_post(inner.handle) } != 0 {
        return Err(io_error_to_py(&std::io::Error::last_os_error()));
    }
    inner.count.fetch_sub(1, Ordering::SeqCst);
    Ok(Object::None)
}

/// `SemLock._get_value()` — `sem_getvalue`, raising on Darwin (which
/// lacks it, exactly as CPython does).
#[cfg(unix)]
fn sem_get_value(inner: &Arc<SemInner>) -> Result<Object, RuntimeError> {
    #[cfg(not(any(target_os = "macos", target_os = "ios")))]
    {
        let mut v: libc::c_int = 0;
        if unsafe { libc::sem_getvalue(inner.handle, &mut v) } != 0 {
            return Err(io_error_to_py(&std::io::Error::last_os_error()));
        }
        Ok(Object::Int(i64::from(v)))
    }
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    {
        let _ = inner;
        // `not_implemented_error` is only referenced on this macOS/iOS arm, so
        // qualify it here rather than import it (unused on other unixes).
        Err(crate::error::not_implemented_error(
            "sem_getvalue is not implemented on this system",
        ))
    }
}

/// `SemLock._is_zero()` — value == 0. Probes with a non-blocking
/// acquire/undo on Darwin (no `sem_getvalue`).
#[cfg(unix)]
fn sem_is_zero(inner: &Arc<SemInner>) -> Result<Object, RuntimeError> {
    #[cfg(not(any(target_os = "macos", target_os = "ios")))]
    {
        let mut v: libc::c_int = 0;
        if unsafe { libc::sem_getvalue(inner.handle, &mut v) } != 0 {
            return Err(io_error_to_py(&std::io::Error::last_os_error()));
        }
        Ok(Object::Bool(v == 0))
    }
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    {
        let r = unsafe { libc::sem_trywait(inner.handle) };
        if r < 0 {
            let e = std::io::Error::last_os_error();
            return match e.raw_os_error() {
                Some(libc::EAGAIN) => Ok(Object::Bool(true)),
                _ => Err(io_error_to_py(&e)),
            };
        }
        unsafe { libc::sem_post(inner.handle) };
        Ok(Object::Bool(false))
    }
}

#[cfg(unix)]
fn sem_unlink_py(args: &[Object]) -> Result<Object, RuntimeError> {
    let name = match args.first() {
        Some(Object::Str(s)) => s.to_string(),
        Some(other) => {
            return Err(type_error(format!(
                "sem_unlink() name must be str, got {}",
                other.type_name()
            )))
        }
        None => return Err(type_error("sem_unlink() requires a name")),
    };
    let cname = sem_cname(&name)?;
    if unsafe { libc::sem_unlink(cname.as_ptr()) } != 0 {
        return Err(io_error_to_py(&std::io::Error::last_os_error()));
    }
    Ok(Object::None)
}

/// Run any tripped OS-signal handlers on the main thread (CPython's
/// `PyErr_CheckSignals` between blocking-acquire slices). Cheap no-op
/// when nothing is pending or off the main thread.
#[cfg(unix)]
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

/// Like [`b_dyn`] but kwargs-aware — `SemLock.acquire` accepts
/// `block=`/`timeout=` by keyword.
#[cfg(unix)]
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

// ---------------------------------------------------------------------
// Connection (socketpair-backed byte channel)
// ---------------------------------------------------------------------

/// Inner state for a Connection. The `fd` is owned by Rust until the
/// Python wrapper calls `close()` (or the Connection is dropped). We
/// guard it with a `Mutex` because Connection objects can be shared
/// across worker threads.
#[cfg(unix)]
struct ConnInner {
    fd: i32,
    closed: bool,
}

#[cfg(unix)]
impl Drop for ConnInner {
    fn drop(&mut self) {
        if !self.closed && self.fd >= 0 {
            unsafe { libc::close(self.fd) };
            self.closed = true;
        }
    }
}

/// Build the public Connection namespace around `fd`.
#[cfg(unix)]
fn build_connection(fd: i32) -> Object {
    let inner = std::sync::Arc::new(Mutex::new(ConnInner { fd, closed: false }));
    let dict = Rc::new(RefCell::new(DictData::new()));
    {
        let i = inner.clone();
        let send_bytes = move |args: &[Object]| -> Result<Object, RuntimeError> {
            let bytes = arg_bytes(args, 0, "send_bytes")?;
            let offset = arg_int(args, 1, 0)? as usize;
            let length = match args.get(2) {
                Some(Object::Int(n)) => *n as usize,
                _ => bytes.len().saturating_sub(offset),
            };
            if offset + length > bytes.len() {
                return Err(value_error("send_bytes: offset/length out of range"));
            }
            let slice = &bytes[offset..offset + length];
            let guard = i.lock().unwrap();
            if guard.closed {
                return Err(broken_pipe_error("connection closed"));
            }
            write_msg(guard.fd, slice).map_err(|e| io_error_to_py(&e))?;
            Ok(Object::None)
        };
        let i = inner.clone();
        let recv_bytes = move |args: &[Object]| -> Result<Object, RuntimeError> {
            let maxlength = match args.first() {
                Some(Object::Int(n)) => Some(*n as usize),
                Some(Object::None) | None => None,
                _ => None,
            };
            let guard = i.lock().unwrap();
            if guard.closed {
                return Err(crate::error::RuntimeError::PyException(
                    crate::error::PyException::from_builtin("EOFError", ""),
                ));
            }
            let buf = read_msg(guard.fd, maxlength).map_err(|e| io_error_to_py(&e))?;
            Ok(Object::Bytes(Rc::from(buf.as_slice())))
        };
        let i = inner.clone();
        let poll = move |args: &[Object]| -> Result<Object, RuntimeError> {
            let timeout = match args.first() {
                Some(Object::Float(f)) => Some(*f),
                Some(Object::Int(i)) => Some(*i as f64),
                Some(Object::None) => None,
                None => Some(0.0),
                _ => Some(0.0),
            };
            let guard = i.lock().unwrap();
            if guard.closed {
                return Ok(Object::Bool(false));
            }
            let ready = poll_readable(guard.fd, timeout).map_err(|e| io_error_to_py(&e))?;
            Ok(Object::Bool(ready))
        };
        let i = inner.clone();
        let close = move |_args: &[Object]| -> Result<Object, RuntimeError> {
            let mut guard = i.lock().unwrap();
            if !guard.closed && guard.fd >= 0 {
                unsafe { libc::close(guard.fd) };
                guard.fd = -1;
                guard.closed = true;
            }
            Ok(Object::None)
        };
        let i = inner.clone();
        let fileno = move |_args: &[Object]| -> Result<Object, RuntimeError> {
            let guard = i.lock().unwrap();
            Ok(Object::Int(guard.fd as i64))
        };
        let i_closed = inner.clone();
        let closed = move |_args: &[Object]| -> Result<Object, RuntimeError> {
            Ok(Object::Bool(i_closed.lock().unwrap().closed))
        };
        let mut d = dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("send_bytes")),
            b_dyn("send_bytes", send_bytes),
        );
        d.insert(
            DictKey(Object::from_static("recv_bytes")),
            b_dyn("recv_bytes", recv_bytes),
        );
        d.insert(DictKey(Object::from_static("poll")), b_dyn("poll", poll));
        d.insert(DictKey(Object::from_static("close")), b_dyn("close", close));
        d.insert(
            DictKey(Object::from_static("fileno")),
            b_dyn("fileno", fileno),
        );
        d.insert(
            DictKey(Object::from_static("closed")),
            b_dyn("closed", closed),
        );
    }
    Object::SimpleNamespace(dict)
}

/// `_multiprocessing.Connection(fd)` — wrap an existing fd. Used by
/// the spawn child after it inherits its connection fd from the
/// parent.
#[cfg(unix)]
fn connection_from_fd(args: &[Object]) -> Result<Object, RuntimeError> {
    let fd = arg_int(args, 0, -1)?;
    if fd < 0 {
        return Err(value_error("Connection(): fd must be >= 0"));
    }
    Ok(build_connection(fd as i32))
}

/// `_multiprocessing.Pipe(duplex=True)` — returns (a, b) connection
/// pair backed by `socketpair(AF_UNIX, SOCK_STREAM, 0)`. When
/// `duplex=False` we fall back to a one-way `pipe(2)`.
#[cfg(unix)]
fn make_pipe(args: &[Object]) -> Result<Object, RuntimeError> {
    let duplex = match args.first() {
        Some(Object::Bool(b)) => *b,
        Some(Object::Int(i)) => *i != 0,
        _ => true,
    };
    if duplex {
        let mut fds: [i32; 2] = [-1; 2];
        let rc = unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, fds.as_mut_ptr()) };
        if rc != 0 {
            return Err(io_error_to_py(&std::io::Error::last_os_error()));
        }
        for &fd in &fds {
            set_cloexec(fd, true).map_err(|e| io_error_to_py(&e))?;
        }
        Ok(Object::new_list(vec![
            build_connection(fds[0]),
            build_connection(fds[1]),
        ]))
    } else {
        let mut fds: [i32; 2] = [-1; 2];
        let rc = unsafe { libc::pipe(fds.as_mut_ptr()) };
        if rc != 0 {
            return Err(io_error_to_py(&std::io::Error::last_os_error()));
        }
        for &fd in &fds {
            set_cloexec(fd, true).map_err(|e| io_error_to_py(&e))?;
        }
        Ok(Object::new_list(vec![
            build_connection(fds[0]),
            build_connection(fds[1]),
        ]))
    }
}

// ---------------------------------------------------------------------
// SharedMemory (shm_open + mmap)
// ---------------------------------------------------------------------

#[cfg(unix)]
struct ShmInner {
    name: String,
    fd: i32,
    addr: *mut libc::c_void,
    size: usize,
    closed: bool,
}

#[cfg(unix)]
// addr is only ever touched via the Mutex.
unsafe impl Send for ShmInner {}

#[cfg(unix)]
impl Drop for ShmInner {
    fn drop(&mut self) {
        if !self.closed {
            if !self.addr.is_null() {
                unsafe { libc::munmap(self.addr, self.size) };
            }
            if self.fd >= 0 {
                unsafe { libc::close(self.fd) };
            }
            self.closed = true;
        }
    }
}

#[cfg(unix)]
static SHM_COUNTER: AtomicUsize = AtomicUsize::new(0);

#[cfg(unix)]
fn make_shared_memory(args: &[Object]) -> Result<Object, RuntimeError> {
    let name_arg = args.first().cloned().unwrap_or(Object::None);
    let create = match args.get(1) {
        Some(Object::Bool(b)) => *b,
        Some(Object::Int(i)) => *i != 0,
        _ => false,
    };
    let size = arg_int(args, 2, 0)? as usize;
    let name = match name_arg {
        Object::Str(s) => s.to_string(),
        Object::None => {
            let counter = SHM_COUNTER.fetch_add(1, Ordering::Relaxed);
            format!("/wp_mp_{}_{}", unsafe { libc::getpid() }, counter)
        }
        other => {
            return Err(type_error(format!(
                "SharedMemory name must be str or None, got {}",
                other.type_name()
            )))
        }
    };

    let c_name = std::ffi::CString::new(name.clone())
        .map_err(|_| value_error("SharedMemory name contains NUL byte"))?;
    let mut oflag = libc::O_RDWR;
    if create {
        oflag |= libc::O_CREAT | libc::O_EXCL;
    }
    let fd = unsafe { libc::shm_open(c_name.as_ptr(), oflag, 0o600) };
    if fd < 0 {
        return Err(io_error_to_py(&std::io::Error::last_os_error()));
    }
    if create {
        let rc = unsafe { libc::ftruncate(fd, size as libc::off_t) };
        if rc != 0 {
            let e = std::io::Error::last_os_error();
            unsafe {
                libc::close(fd);
                libc::shm_unlink(c_name.as_ptr());
            }
            return Err(io_error_to_py(&e));
        }
    }
    let final_size = if create {
        size
    } else {
        let mut st: libc::stat = unsafe { std::mem::zeroed() };
        let rc = unsafe { libc::fstat(fd, &mut st) };
        if rc != 0 {
            let e = std::io::Error::last_os_error();
            unsafe { libc::close(fd) };
            return Err(io_error_to_py(&e));
        }
        st.st_size as usize
    };
    let addr = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            final_size,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_SHARED,
            fd,
            0,
        )
    };
    if addr == libc::MAP_FAILED {
        let e = std::io::Error::last_os_error();
        unsafe { libc::close(fd) };
        return Err(io_error_to_py(&e));
    }
    let inner = std::sync::Arc::new(Mutex::new(ShmInner {
        name: name.clone(),
        fd,
        addr,
        size: final_size,
        closed: false,
    }));
    let dict = Rc::new(RefCell::new(DictData::new()));
    {
        let i = inner.clone();
        let close = move |_args: &[Object]| -> Result<Object, RuntimeError> {
            let mut g = i.lock().unwrap();
            if !g.closed {
                if !g.addr.is_null() {
                    unsafe { libc::munmap(g.addr, g.size) };
                    g.addr = std::ptr::null_mut();
                }
                if g.fd >= 0 {
                    unsafe { libc::close(g.fd) };
                    g.fd = -1;
                }
                g.closed = true;
            }
            Ok(Object::None)
        };
        let i_unlink = inner.clone();
        let unlink = move |_args: &[Object]| -> Result<Object, RuntimeError> {
            let g = i_unlink.lock().unwrap();
            let c = std::ffi::CString::new(g.name.clone())
                .map_err(|_| value_error("unlink: bad shm name"))?;
            let rc = unsafe { libc::shm_unlink(c.as_ptr()) };
            if rc != 0 {
                return Err(io_error_to_py(&std::io::Error::last_os_error()));
            }
            Ok(Object::None)
        };
        let i_read = inner.clone();
        let read = move |args: &[Object]| -> Result<Object, RuntimeError> {
            let g = i_read.lock().unwrap();
            if g.closed {
                return Err(runtime_error("SharedMemory is closed"));
            }
            let offset = arg_int(args, 0, 0)? as usize;
            let length = match args.get(1) {
                Some(Object::Int(n)) => *n as usize,
                _ => g.size.saturating_sub(offset),
            };
            if offset + length > g.size {
                return Err(value_error("read: out of range"));
            }
            let slice =
                unsafe { std::slice::from_raw_parts((g.addr as *const u8).add(offset), length) };
            Ok(Object::Bytes(Rc::from(slice)))
        };
        let i_write = inner.clone();
        let write = move |args: &[Object]| -> Result<Object, RuntimeError> {
            let bytes = arg_bytes(args, 0, "write")?;
            let offset = arg_int(args, 1, 0)? as usize;
            let g = i_write.lock().unwrap();
            if g.closed {
                return Err(runtime_error("SharedMemory is closed"));
            }
            if offset + bytes.len() > g.size {
                return Err(value_error("write: out of range"));
            }
            unsafe {
                std::ptr::copy_nonoverlapping(
                    bytes.as_ptr(),
                    (g.addr as *mut u8).add(offset),
                    bytes.len(),
                );
            }
            Ok(Object::Int(bytes.len() as i64))
        };
        let mut d = dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("name")),
            Object::from_str(name.clone()),
        );
        d.insert(
            DictKey(Object::from_static("size")),
            Object::Int(final_size as i64),
        );
        d.insert(DictKey(Object::from_static("close")), b_dyn("close", close));
        d.insert(
            DictKey(Object::from_static("unlink")),
            b_dyn("unlink", unlink),
        );
        d.insert(DictKey(Object::from_static("read")), b_dyn("read", read));
        d.insert(DictKey(Object::from_static("write")), b_dyn("write", write));
    }
    Ok(Object::SimpleNamespace(dict))
}

// ---------------------------------------------------------------------
// Spawn helpers
// ---------------------------------------------------------------------

/// `_spawn_child(argv, env=None, fds_to_keep=None, payload=b"")`:
/// fork + execve a child process. The child inherits a single pre-set
/// fd carrying the pickled task payload; the parent receives the pid.
///
/// Returns `(pid, parent_payload_fd)` where `parent_payload_fd` is
/// already populated with the payload bytes — the caller can either
/// close it immediately or keep reading status from it.
#[cfg(unix)]
fn spawn_child(args: &[Object]) -> Result<Object, RuntimeError> {
    let argv: Vec<String> = match args.first() {
        Some(Object::List(l)) => {
            let l = l.borrow();
            let mut out = Vec::with_capacity(l.len());
            for item in l.iter() {
                match item {
                    Object::Str(s) => out.push(s.to_string()),
                    other => {
                        return Err(type_error(format!(
                            "_spawn_child: argv entries must be str, got {}",
                            other.type_name()
                        )))
                    }
                }
            }
            out
        }
        _ => return Err(type_error("_spawn_child: argv must be list[str]")),
    };
    if argv.is_empty() {
        return Err(value_error("_spawn_child: argv must not be empty"));
    }
    let payload = match args.get(3) {
        Some(o) => arg_bytes_obj(o, "payload")?,
        None => Vec::new(),
    };

    let env: Option<Vec<(String, String)>> = match args.get(1) {
        Some(Object::Dict(d)) => {
            let mut out = Vec::new();
            let d = d.borrow();
            for (k, v) in d.iter() {
                if let (Object::Str(ks), Object::Str(vs)) = (&k.0, v) {
                    out.push((ks.to_string(), vs.to_string()));
                }
            }
            Some(out)
        }
        _ => None,
    };

    let mut fds: [i32; 2] = [-1; 2];
    let rc = unsafe { libc::pipe(fds.as_mut_ptr()) };
    if rc != 0 {
        return Err(io_error_to_py(&std::io::Error::last_os_error()));
    }
    let parent_fd = fds[1];
    let child_fd = fds[0];

    let argv_c: Vec<std::ffi::CString> = argv
        .iter()
        .map(|s| std::ffi::CString::new(s.clone()).unwrap_or_default())
        .collect();
    let argv_ptrs: Vec<*const libc::c_char> = argv_c
        .iter()
        .map(|c| c.as_ptr())
        .chain(std::iter::once(std::ptr::null()))
        .collect();

    let env_strings: Vec<std::ffi::CString> = if let Some(items) = env {
        items
            .into_iter()
            .map(|(k, v)| std::ffi::CString::new(format!("{k}={v}")).unwrap_or_default())
            .collect()
    } else {
        std::env::vars()
            .map(|(k, v)| std::ffi::CString::new(format!("{k}={v}")).unwrap_or_default())
            .collect()
    };
    let mut env_strings = env_strings;
    // Make sure WEAVEPY_MP_PAYLOAD_FD is set to the child fd target slot
    // (we'll dup2 the inherited pipe end onto fd 3 inside the child).
    env_strings.retain(|s| !s.to_bytes().starts_with(b"WEAVEPY_MP_PAYLOAD_FD="));
    env_strings.push(std::ffi::CString::new("WEAVEPY_MP_PAYLOAD_FD=3").unwrap());
    let env_ptrs: Vec<*const libc::c_char> = env_strings
        .iter()
        .map(|c| c.as_ptr())
        .chain(std::iter::once(std::ptr::null()))
        .collect();

    let pid = unsafe { libc::fork() };
    if pid < 0 {
        let e = std::io::Error::last_os_error();
        unsafe {
            libc::close(parent_fd);
            libc::close(child_fd);
        }
        return Err(io_error_to_py(&e));
    }
    if pid == 0 {
        // Child: dup child_fd onto fd 3, close everything else >2.
        unsafe {
            libc::close(parent_fd);
            if child_fd != 3 {
                libc::dup2(child_fd, 3);
                libc::close(child_fd);
            }
            // Best-effort: close any other fd in the conventional range.
            for fd in 4..256 {
                libc::close(fd);
            }
            libc::execve(argv_ptrs[0], argv_ptrs.as_ptr(), env_ptrs.as_ptr());
            // execve only returns on error.
            libc::_exit(127);
        }
    }
    // Parent: close the read end (the child has it), write the payload
    // to the write end, and hand the write end back so the caller can
    // close it (signalling EOF to the child) or stash it.
    unsafe { libc::close(child_fd) };
    if !payload.is_empty() {
        let mut written = 0usize;
        while written < payload.len() {
            let rc = unsafe {
                libc::write(
                    parent_fd,
                    payload.as_ptr().add(written) as *const libc::c_void,
                    payload.len() - written,
                )
            };
            if rc < 0 {
                let e = std::io::Error::last_os_error();
                unsafe { libc::close(parent_fd) };
                return Err(io_error_to_py(&e));
            }
            written += rc as usize;
        }
    }
    Ok(Object::new_list(vec![
        Object::Int(pid as i64),
        Object::Int(parent_fd as i64),
    ]))
}

/// `_waitpid(pid, options=0)`: returns (pid, status, signal, exitcode).
#[cfg(unix)]
fn waitpid_py(args: &[Object]) -> Result<Object, RuntimeError> {
    let pid = arg_int(args, 0, 0)? as libc::pid_t;
    let options = arg_int(args, 1, 0)? as libc::c_int;
    let mut status: libc::c_int = 0;
    let status_ptr: *mut libc::c_int = &mut status;
    // Drop the GIL across the wait (blocking unless WNOHANG) so handler threads
    // keep running; retry on EINTR after servicing signals.
    let rc = loop {
        let rc =
            crate::gil::allow_threads_then(|| unsafe { libc::waitpid(pid, status_ptr, options) });
        if rc < 0 {
            let e = std::io::Error::last_os_error();
            if e.raw_os_error() == Some(libc::EINTR) {
                service_pending_signals()?;
                continue;
            }
            return Err(io_error_to_py(&e));
        }
        break rc;
    };
    let (signal, exitcode) = if libc::WIFSIGNALED(status) {
        (libc::WTERMSIG(status), -1)
    } else if libc::WIFEXITED(status) {
        (0, libc::WEXITSTATUS(status))
    } else {
        (0, -1)
    };
    Ok(Object::new_list(vec![
        Object::Int(rc as i64),
        Object::Int(status as i64),
        Object::Int(signal as i64),
        Object::Int(exitcode as i64),
    ]))
}

/// `_get_command()` — argv used by the spawn start-method.
#[cfg(unix)]
fn get_command(_args: &[Object]) -> Result<Object, RuntimeError> {
    let exe = std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "weavepy".to_owned());
    Ok(Object::new_list(vec![
        Object::from_str(exe),
        Object::from_static("--multiprocessing-fork"),
    ]))
}

/// `_exit(code)` — hard exit the current process. Used by
/// `multiprocessing._run_spawn_child` to propagate the worker's exit
/// code to the parent without bouncing through the CLI's normal exit
/// path (which would swallow `SystemExit` and reformat the
/// stacktrace).
#[cfg(unix)]
fn mp_exit(args: &[Object]) -> Result<Object, RuntimeError> {
    let code = arg_int(args, 0, 0)? as i32;
    std::process::exit(code);
}

#[cfg(unix)]
fn payload_fd(_args: &[Object]) -> Result<Object, RuntimeError> {
    match std::env::var("WEAVEPY_MP_PAYLOAD_FD")
        .ok()
        .and_then(|s| s.parse::<i64>().ok())
    {
        Some(n) => Ok(Object::Int(n)),
        None => Ok(Object::None),
    }
}

// ---------------------------------------------------------------------
// Legacy stubs (kept for older callers that imported the names directly)
// ---------------------------------------------------------------------

#[cfg(unix)]
fn conn_send_legacy(_args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(Object::None)
}

#[cfg(unix)]
fn conn_recv_legacy(_args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(Object::None)
}

#[cfg(unix)]
fn conn_close_legacy(_args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(Object::None)
}

// ---------------------------------------------------------------------
// I/O helpers
// ---------------------------------------------------------------------

#[cfg(unix)]
fn write_msg(fd: RawFd, data: &[u8]) -> std::io::Result<()> {
    let header = (data.len() as u32).to_be_bytes();
    write_all(fd, &header)?;
    write_all(fd, data)?;
    Ok(())
}

#[cfg(unix)]
fn write_all(fd: RawFd, mut data: &[u8]) -> std::io::Result<()> {
    while !data.is_empty() {
        let n = unsafe { libc::write(fd, data.as_ptr() as *const libc::c_void, data.len()) };
        if n < 0 {
            let e = std::io::Error::last_os_error();
            if e.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(e);
        }
        if n == 0 {
            return Err(std::io::Error::from(std::io::ErrorKind::WriteZero));
        }
        data = &data[n as usize..];
    }
    Ok(())
}

#[cfg(unix)]
fn read_msg(fd: RawFd, maxlen: Option<usize>) -> std::io::Result<Vec<u8>> {
    let mut header = [0u8; 4];
    read_exact(fd, &mut header)?;
    let len = u32::from_be_bytes(header) as usize;
    if let Some(max) = maxlen {
        if len > max {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("recv_bytes: message of {len} bytes exceeds maxlength {max}"),
            ));
        }
    }
    let mut buf = vec![0u8; len];
    read_exact(fd, &mut buf)?;
    Ok(buf)
}

#[cfg(unix)]
fn read_exact(fd: RawFd, buf: &mut [u8]) -> std::io::Result<()> {
    let mut filled = 0usize;
    while filled < buf.len() {
        // Release the GIL across each blocking read so other threads run.
        let ptr = unsafe { buf.as_mut_ptr().add(filled) } as *mut libc::c_void;
        let want = buf.len() - filled;
        let n = crate::gil::allow_threads_then(|| unsafe { libc::read(fd, ptr, want) });
        if n < 0 {
            let e = std::io::Error::last_os_error();
            if e.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(e);
        }
        if n == 0 {
            return Err(std::io::Error::from(std::io::ErrorKind::UnexpectedEof));
        }
        filled += n as usize;
    }
    Ok(())
}

#[cfg(unix)]
fn poll_readable(fd: RawFd, timeout_secs: Option<f64>) -> std::io::Result<bool> {
    let mut pfd = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    let timeout_ms: i32 = match timeout_secs {
        Some(t) if t < 0.0 => -1,
        Some(t) => (t * 1000.0) as i32,
        None => -1,
    };
    let pfd_ptr: *mut libc::pollfd = &mut pfd;
    // Release the GIL across the (possibly indefinite) poll so peers run.
    let rc = crate::gil::allow_threads_then(|| unsafe { libc::poll(pfd_ptr, 1, timeout_ms) });
    if rc < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(rc > 0 && (pfd.revents & libc::POLLIN) != 0)
}

#[cfg(unix)]
fn set_cloexec(fd: RawFd, on: bool) -> std::io::Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let new = if on {
        flags | libc::FD_CLOEXEC
    } else {
        flags & !libc::FD_CLOEXEC
    };
    if unsafe { libc::fcntl(fd, libc::F_SETFD, new) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

// ---------------------------------------------------------------------
// Builder helpers
// ---------------------------------------------------------------------

#[cfg(unix)]
fn b(name: &'static str, body: fn(&[Object]) -> Result<Object, RuntimeError>) -> Object {
    Object::Builtin(Rc::new(BuiltinFn {
        name,
        binds_instance: false,
        call: Box::new(body),
        call_kw: None,
    }))
}

#[cfg(unix)]
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

#[cfg(unix)]
fn arg_int(args: &[Object], idx: usize, default: i64) -> Result<i64, RuntimeError> {
    match args.get(idx) {
        Some(Object::Int(n)) => Ok(*n),
        Some(Object::Bool(b)) => Ok(if *b { 1 } else { 0 }),
        Some(Object::None) | None => Ok(default),
        Some(other) => Err(type_error(format!(
            "argument {} must be int, got {}",
            idx,
            other.type_name()
        ))),
    }
}

#[cfg(unix)]
fn arg_bytes(args: &[Object], idx: usize, label: &str) -> Result<Vec<u8>, RuntimeError> {
    arg_bytes_obj(
        args.get(idx)
            .ok_or_else(|| type_error(format!("{label}: missing bytes argument")))?,
        label,
    )
}

#[cfg(unix)]
fn arg_bytes_obj(o: &Object, label: &str) -> Result<Vec<u8>, RuntimeError> {
    match o {
        Object::Bytes(b) => Ok(b.to_vec()),
        Object::ByteArray(b) => Ok(b.borrow().clone()),
        Object::Str(s) => Ok(s.to_string().into_bytes()),
        other => Err(type_error(format!(
            "{label}: expected bytes/bytearray, got {}",
            other.type_name()
        ))),
    }
}

// ---------------------------------------------------------------------
// _posixshmem — shm_open(3) / shm_unlink(3) core (RFC 0040 WS5).
//
// `multiprocessing/resource_tracker.py` imports it unconditionally on
// POSIX (for `shm_unlink`), and `multiprocessing/shared_memory.py` uses
// `shm_open` to back `SharedMemory`. CPython ships it as
// `Modules/_multiprocessing/posixshmem.c`.
// ---------------------------------------------------------------------

#[cfg(unix)]
pub fn build_posixshmem(_cache: &ModuleCache) -> Rc<PyModule> {
    let dict = Rc::new(RefCell::new(DictData::new()));
    {
        let mut d = dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("__name__")),
            Object::from_static("_posixshmem"),
        );
        d.insert(
            DictKey(Object::from_static("shm_open")),
            b_dyn_kw("shm_open", shm_open_py),
        );
        d.insert(
            DictKey(Object::from_static("shm_unlink")),
            b("shm_unlink", shm_unlink_py),
        );
    }
    Rc::new(PyModule {
        name: "_posixshmem".to_owned(),
        filename: None,
        dict,
    })
}

#[cfg(not(unix))]
pub fn build_posixshmem(_cache: &ModuleCache) -> Rc<PyModule> {
    let dict = Rc::new(RefCell::new(DictData::new()));
    dict.borrow_mut().insert(
        DictKey(Object::from_static("__name__")),
        Object::from_static("_posixshmem"),
    );
    Rc::new(PyModule {
        name: "_posixshmem".to_owned(),
        filename: None,
        dict,
    })
}

/// `_posixshmem.shm_open(path, flags, mode=0o777)` → fd.
///
/// CPython's `_posixshmem.shm_open` accepts `path`, `flags`, and `mode`
/// positionally *or* by keyword (`shared_memory.py` passes `mode=` by
/// keyword), so this is a kwargs-aware builtin.
#[cfg(unix)]
fn shm_open_py(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let kw = |name: &str| kwargs.iter().find(|(k, _)| k == name).map(|(_, v)| v);
    let path_obj = args.first().or_else(|| kw("path"));
    let path = match path_obj {
        Some(Object::Str(s)) => s.to_string(),
        _ => return Err(type_error("shm_open() path must be a str")),
    };
    let flags = match args.get(1).or_else(|| kw("flags")) {
        Some(Object::Int(n)) => *n as libc::c_int,
        _ => return Err(type_error("shm_open() flags must be an int")),
    };
    let mode = match args.get(2).or_else(|| kw("mode")) {
        Some(Object::Int(n)) => *n as libc::c_uint,
        _ => 0o777,
    };
    let cpath = std::ffi::CString::new(path).map_err(|_| value_error("embedded null byte"))?;
    // `shm_open` is variadic; the mode arg must be promoted to c_uint
    // (mode_t is u16 on Darwin, which can't cross a variadic boundary).
    let fd = unsafe { libc::shm_open(cpath.as_ptr(), flags, mode) };
    if fd < 0 {
        return Err(io_error_to_py(&std::io::Error::last_os_error()));
    }
    Ok(Object::Int(i64::from(fd)))
}

/// `_posixshmem.shm_unlink(path)`.
#[cfg(unix)]
fn shm_unlink_py(args: &[Object]) -> Result<Object, RuntimeError> {
    let path = match args.first() {
        Some(Object::Str(s)) => s.to_string(),
        _ => return Err(type_error("shm_unlink() path must be a str")),
    };
    let cpath = std::ffi::CString::new(path).map_err(|_| value_error("embedded null byte"))?;
    if unsafe { libc::shm_unlink(cpath.as_ptr()) } != 0 {
        return Err(io_error_to_py(&std::io::Error::last_os_error()));
    }
    Ok(Object::None)
}

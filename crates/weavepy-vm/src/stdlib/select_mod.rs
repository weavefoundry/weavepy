//! The `select` built-in module (RFC 0039 WS6).
//!
//! Faithful, `libc`-backed I/O multiplexing primitives:
//!
//!   * `select.select(rlist, wlist, xlist, timeout=None)` — over `poll(2)`,
//!     returning the *original* objects that are ready (CPython maps the
//!     ready descriptors back to the passed-in file objects).
//!   * `select.poll()` — a real `poll(2)` object
//!     (`register`/`modify`/`unregister`/`poll`).
//!   * `select.kqueue()` / `select.kevent(...)` + the `KQ_*` constants
//!     on macOS/BSD — over `kqueue(2)`/`kevent(2)`.
//!
//! All blocking calls drop the GIL (so other threads run) and stay
//! responsive to signals: WeavePy can't rely on the kernel delivering a
//! process-directed signal (`signal.alarm`) to the OS thread the VM is
//! blocked on, so the main thread waits in short slices and runs any
//! tripped Python signal handler between them — a handler that raises
//! abandons the wait with that exception, matching CPython interrupting
//! `select`/`poll`/`kevent` (`test_selectors.test_select_interrupt_*`).
//!
//! The frozen `selectors` module (a verbatim CPython port) layers
//! `SelectSelector` / `PollSelector` / `KqueueSelector` /
//! `DefaultSelector` on top of these, which is what asyncio drives.

use crate::sync::Rc;
use crate::sync::RefCell;

use crate::error::{os_error, type_error, RuntimeError};
use crate::import::ModuleCache;
use crate::object::{BuiltinFn, DictData, DictKey, Object, PyModule};

#[cfg(unix)]
use std::time::{Duration, Instant};

pub fn build(_cache: &ModuleCache) -> Rc<PyModule> {
    let dict = Rc::new(RefCell::new(DictData::new()));
    {
        let mut d = dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("__name__")),
            Object::from_static("select"),
        );
        d.insert(
            DictKey(Object::from_static("__doc__")),
            Object::from_static("Wait for I/O completion on file descriptors."),
        );

        // `poll` event flags — the host's real `poll(2)` values, so the
        // masks we hand `selectors` round-trip through `libc::poll`.
        for (name, val) in poll_constants() {
            d.insert(DictKey(Object::from_str(name)), Object::Int(val));
        }

        d.insert(
            DictKey(Object::from_static("select")),
            b("select", select_select),
        );

        // CPython exposes `select.poll` as a *factory* whose result type
        // is not directly instantiable
        // (`test_select.test_disallow_instantiation`).
        #[cfg(unix)]
        {
            d.insert(
                DictKey(Object::from_static("poll")),
                b("poll", poll_factory),
            );
        }

        // kqueue / kevent + KQ_* on macOS/BSD (no-op elsewhere).
        kqueue_impl::install(&mut d);

        // CPython 3.3+ aliases the module's `error` to OSError.
        d.insert(
            DictKey(Object::from_static("error")),
            Object::Type(crate::builtin_types::builtin_types().os_error.clone()),
        );
    }
    Rc::new(PyModule {
        name: "select".to_owned(),
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

#[cfg(unix)]
fn method(name: &'static str, body: fn(&[Object]) -> Result<Object, RuntimeError>) -> Object {
    Object::Builtin(Rc::new(BuiltinFn {
        name,
        binds_instance: true,
        call: Box::new(body),
        call_kw: None,
    }))
}

#[cfg(unix)]
fn poll_constants() -> Vec<(&'static str, i64)> {
    vec![
        ("POLLIN", i64::from(libc::POLLIN)),
        ("POLLPRI", i64::from(libc::POLLPRI)),
        ("POLLOUT", i64::from(libc::POLLOUT)),
        ("POLLERR", i64::from(libc::POLLERR)),
        ("POLLHUP", i64::from(libc::POLLHUP)),
        ("POLLNVAL", i64::from(libc::POLLNVAL)),
        ("POLLRDNORM", i64::from(libc::POLLRDNORM)),
        ("POLLRDBAND", i64::from(libc::POLLRDBAND)),
        ("POLLWRNORM", i64::from(libc::POLLWRNORM)),
        ("POLLWRBAND", i64::from(libc::POLLWRBAND)),
    ]
}

#[cfg(not(unix))]
fn poll_constants() -> Vec<(&'static str, i64)> {
    vec![
        ("POLLIN", 0x001),
        ("POLLPRI", 0x002),
        ("POLLOUT", 0x004),
        ("POLLERR", 0x008),
        ("POLLHUP", 0x010),
        ("POLLNVAL", 0x020),
    ]
}

// ---------------------------------------------------------------------------
// Signal-aware blocking
// ---------------------------------------------------------------------------

/// Run any pending OS-signal handlers on the main thread, propagating a
/// handler that raises. No-op (and cheap) when nothing is tripped.
#[cfg(unix)]
fn service_pending_signals() -> Result<(), RuntimeError> {
    if !crate::stdlib::signal_mod::signals_pending() {
        return Ok(());
    }
    if let Some(ptr) = crate::vm_singletons::current_interpreter_ptr() {
        // SAFETY: published by the active builtin call on this (main)
        // thread; the interpreter outlives this call.
        let interp = unsafe { &mut *ptr };
        interp.run_pending_signals_public()?;
    }
    Ok(())
}

/// Slice length for the main thread's signal-interruptible wait,
/// matching `_thread`'s interruptible lock acquire. WeavePy can't rely
/// on the kernel delivering a process-directed signal (`signal.alarm`)
/// to the OS thread the VM happens to be blocked on, so — rather than
/// CPython's pure `EINTR` loop — the main thread waits in short slices
/// and re-checks tripped signals between them. Short enough that a
/// handler runs promptly; long enough that idle wakeup cost is small.
#[cfg(unix)]
const SIGNAL_POLL_SLICE: Duration = Duration::from_millis(20);

/// Drive a blocking multiplexing syscall to readiness with the GIL
/// released. `poll_once(slice)` performs exactly one syscall waiting up
/// to `slice` (`None` = block forever) and returns the number of ready
/// events (0 = the slice elapsed with nothing ready); it must touch no
/// Python objects (the GIL is dropped around it). Returns the ready
/// count of the terminating syscall (0 on overall timeout).
///
/// On the main thread we slice the wait at [`SIGNAL_POLL_SLICE`] and run
/// any tripped signal handlers between slices (and after `EINTR`); a
/// handler that raises abandons the wait with that exception. Off the
/// main thread (no Python signal handlers run there) we block for the
/// whole remaining time in one call. A zero `timeout` still performs one
/// non-blocking syscall.
#[cfg(unix)]
fn blocking_retry(
    timeout: Option<Duration>,
    mut poll_once: impl FnMut(Option<Duration>) -> std::io::Result<usize>,
) -> Result<usize, RuntimeError> {
    let on_main = crate::gil::is_main_thread();
    // `None` = wait forever (also the graceful degradation for an absurd
    // timeout that overflows `Instant`).
    let deadline = timeout.and_then(|t| Instant::now().checked_add(t));
    loop {
        let remaining = deadline.map(|dl| dl.saturating_duration_since(Instant::now()));
        let slice = match (on_main, remaining) {
            // Deadline reached (or a zero timeout): one non-blocking pass.
            (_, Some(r)) if r.is_zero() => Some(Duration::ZERO),
            (true, Some(r)) => Some(r.min(SIGNAL_POLL_SLICE)),
            (true, None) => Some(SIGNAL_POLL_SLICE),
            (false, r) => r,
        };
        let res = crate::gil::allow_threads_then(|| poll_once(slice));
        match res {
            Ok(n) if n > 0 => return Ok(n),
            Ok(_) => {
                if on_main {
                    service_pending_signals()?;
                }
                if remaining.is_some_and(|r| r.is_zero())
                    || deadline.is_some_and(|dl| Instant::now() >= dl)
                {
                    return Ok(0);
                }
            }
            Err(e) if e.raw_os_error() == Some(libc::EINTR) => {
                if on_main {
                    service_pending_signals()?;
                }
                if deadline.is_some_and(|dl| Instant::now() >= dl) {
                    return Ok(0);
                }
            }
            Err(e) => return Err(crate::error::io_error_to_py(&e)),
        }
    }
}

/// Convert a remaining `Duration` (or `None` = forever) to a `poll(2)`
/// millisecond timeout, rounding up so we wait *at least* the requested
/// span (sub-millisecond rounds to 1ms, never 0).
#[cfg(unix)]
fn timeout_to_poll_ms(remaining: Option<Duration>) -> libc::c_int {
    match remaining {
        None => -1,
        Some(d) => {
            if d.is_zero() {
                0
            } else {
                let ms = d.as_millis().max(1);
                ms.min(libc::c_int::MAX as u128) as libc::c_int
            }
        }
    }
}

// ---------------------------------------------------------------------------
// File-descriptor extraction
// ---------------------------------------------------------------------------

/// Resolve a file descriptor from an int (or bool), or — for any other
/// object — its `fileno()` method (CPython's `PyObject_AsFileDescriptor`).
#[cfg(unix)]
fn fd_of(obj: &Object) -> Result<i32, RuntimeError> {
    match obj {
        Object::Int(n) => Ok(*n as i32),
        Object::Bool(b) => Ok(i32::from(*b)),
        _ => {
            let ptr = crate::vm_singletons::current_interpreter_ptr()
                .ok_or_else(|| type_error("argument must be an int, or have a fileno() method"))?;
            // SAFETY: published by the active builtin call on this thread.
            let interp = unsafe { &mut *ptr };
            let fileno = interp
                .load_attr_public(obj, "fileno")
                .map_err(|_| type_error("argument must be an int, or have a fileno() method"))?;
            match interp.call_object(fileno, &[], &[])? {
                Object::Int(n) => Ok(n as i32),
                Object::Bool(b) => Ok(i32::from(b)),
                _ => Err(type_error("fileno() returned a non-integer")),
            }
        }
    }
}

/// Collect `(fd, original_object)` pairs from a `select` argument. Lists
/// are walked by *index*, re-reading length and releasing the borrow
/// around each `fileno()` call, so a `fileno()` that mutates the list
/// is observed exactly like CPython (`test_select.test_select_mutated`).
#[cfg(unix)]
fn collect_fd_objects(arg: Option<&Object>) -> Result<Vec<(i32, Object)>, RuntimeError> {
    match arg {
        None | Some(Object::None) => Ok(Vec::new()),
        Some(Object::List(l)) => {
            let mut out = Vec::new();
            let mut i = 0usize;
            loop {
                let item = {
                    let b = l.borrow();
                    if i >= b.len() {
                        break;
                    }
                    b[i].clone()
                };
                out.push((fd_of(&item)?, item));
                i += 1;
            }
            Ok(out)
        }
        Some(other) => {
            let ptr = crate::vm_singletons::current_interpreter_ptr()
                .ok_or_else(|| type_error("arguments 1-3 must be sequences"))?;
            let interp = unsafe { &mut *ptr };
            let globals = interp.builtins_dict();
            let items = interp
                .collect_iterable(other, &globals)
                .map_err(|_| type_error("arguments 1-3 must be sequences"))?;
            items.into_iter().map(|o| Ok((fd_of(&o)?, o))).collect()
        }
    }
}

/// `select.select` timeout is in *seconds* (float/int) or `None`.
/// CPython rejects negative timeouts with `ValueError`.
#[cfg(unix)]
fn parse_secs(arg: Option<&Object>) -> Result<Option<Duration>, RuntimeError> {
    match arg {
        None | Some(Object::None) => Ok(None),
        Some(Object::Int(n)) => {
            if *n < 0 {
                return Err(crate::error::value_error("timeout must be non-negative"));
            }
            Ok(Some(Duration::from_secs_f64(*n as f64)))
        }
        Some(Object::Bool(b)) => Ok(Some(Duration::from_secs_f64(f64::from(*b)))),
        Some(Object::Float(f)) => {
            if *f < 0.0 {
                return Err(crate::error::value_error("timeout must be non-negative"));
            }
            Ok(Some(Duration::from_secs_f64(*f)))
        }
        Some(_) => Err(type_error("timeout must be a float or None")),
    }
}

#[cfg(unix)]
fn ebadf_error() -> RuntimeError {
    crate::error::io_error_to_py(&std::io::Error::from_raw_os_error(libc::EBADF))
}

// ---------------------------------------------------------------------------
// select.select
// ---------------------------------------------------------------------------

#[cfg(unix)]
fn select_select(args: &[Object]) -> Result<Object, RuntimeError> {
    let rlist = collect_fd_objects(args.first())?;
    let wlist = collect_fd_objects(args.get(1))?;
    let xlist = collect_fd_objects(args.get(2))?;
    let timeout = parse_secs(args.get(3))?;

    // One pollfd per distinct fd, OR-ing the requested interests.
    let mut order: Vec<i32> = Vec::new();
    let mut pollfds: Vec<libc::pollfd> = Vec::new();
    let mut want = |fd: i32, ev: libc::c_short| {
        if let Some(pos) = order.iter().position(|f| *f == fd) {
            pollfds[pos].events |= ev;
        } else {
            order.push(fd);
            pollfds.push(libc::pollfd {
                fd,
                events: ev,
                revents: 0,
            });
        }
    };
    for (fd, _) in &rlist {
        want(*fd, libc::POLLIN);
    }
    for (fd, _) in &wlist {
        want(*fd, libc::POLLOUT);
    }
    for (fd, _) in &xlist {
        want(*fd, libc::POLLPRI);
    }

    blocking_retry(timeout, |slice| {
        for p in pollfds.iter_mut() {
            p.revents = 0;
        }
        let ms = timeout_to_poll_ms(slice);
        let n = unsafe { libc::poll(pollfds.as_mut_ptr(), pollfds.len() as libc::nfds_t, ms) };
        if n < 0 {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(n as usize)
        }
    })?;

    // `select(2)` fails the whole call on a bad descriptor; `poll(2)`
    // reports it per-fd as POLLNVAL, so surface it as EBADF
    // (`test_select.test_errno`).
    if pollfds.iter().any(|p| p.revents & libc::POLLNVAL != 0) {
        return Err(ebadf_error());
    }

    let revents_of =
        |fd: i32| -> libc::c_short { pollfds.iter().find(|p| p.fd == fd).map_or(0, |p| p.revents) };
    // A hung-up/errored fd reads and writes as ready (CPython's
    // `select(2)` reports it in both sets); preserve input order and the
    // original objects.
    let ready = |list: &[(i32, Object)], mask: libc::c_short| -> Vec<Object> {
        list.iter()
            .filter(|(fd, _)| revents_of(*fd) & mask != 0)
            .map(|(_, o)| o.clone())
            .collect()
    };
    let rmask = libc::POLLIN | libc::POLLHUP | libc::POLLERR;
    let wmask = libc::POLLOUT | libc::POLLHUP | libc::POLLERR;
    let xmask = libc::POLLPRI;
    Ok(Object::new_tuple(vec![
        Object::new_list(ready(&rlist, rmask)),
        Object::new_list(ready(&wlist, wmask)),
        Object::new_list(ready(&xlist, xmask)),
    ]))
}

#[cfg(not(unix))]
fn select_select(_args: &[Object]) -> Result<Object, RuntimeError> {
    Err(os_error("select.select is unavailable on this platform"))
}

// ---------------------------------------------------------------------------
// select.poll()
// ---------------------------------------------------------------------------

#[cfg(unix)]
struct PollReg {
    /// fd → poll event mask, in registration order.
    fds: Vec<(i32, libc::c_short)>,
    /// CPython forbids a concurrent `poll()` on the same object
    /// (`test_poll.test_threaded_poll`); set across the blocking call.
    polling: bool,
}

/// Process-global poll registrations (shared across threads — a poll
/// object created on one thread is polled from another in
/// `test_poll.test_threaded_poll`), keyed by an opaque handle stored on
/// the instance.
#[cfg(unix)]
fn poll_registry() -> &'static std::sync::Mutex<std::collections::HashMap<i64, PollReg>> {
    static R: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<i64, PollReg>>> =
        std::sync::OnceLock::new();
    R.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

#[cfg(unix)]
static NEXT_POLL_HANDLE: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(1);

#[cfg(unix)]
thread_local! {
    static POLL_CLASS: RefCell<Option<Rc<crate::types::TypeObject>>> = const { RefCell::new(None) };
}

#[cfg(unix)]
fn poll_type() -> Rc<crate::types::TypeObject> {
    POLL_CLASS.with(|slot| {
        if let Some(c) = slot.borrow().as_ref() {
            return c.clone();
        }
        let bt = crate::builtin_types::builtin_types();
        let mut dict = DictData::new();
        for (name, m) in [
            ("register", method("register", poll_register)),
            ("modify", method("modify", poll_modify)),
            ("unregister", method("unregister", poll_unregister)),
            ("poll", method("poll", poll_poll)),
            ("__del__", method("__del__", poll_del)),
        ] {
            dict.insert(DictKey(Object::from_static(name)), m);
        }
        // Not directly instantiable: only `select.poll()` (the factory)
        // builds one (`test_select.test_disallow_instantiation`).
        dict.insert(
            DictKey(Object::from_static("__new__")),
            b("__new__", poll_new_disallowed),
        );
        let cls = crate::types::TypeObject::new_user("poll", vec![bt.object_.clone()], dict)
            .expect("poll class must linearise");
        *slot.borrow_mut() = Some(cls.clone());
        cls
    })
}

#[cfg(unix)]
fn poll_new_disallowed(_args: &[Object]) -> Result<Object, RuntimeError> {
    Err(type_error("cannot create 'select.poll' instances"))
}

#[cfg(unix)]
fn poll_factory(_args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = Rc::new(crate::types::PyInstance::new(poll_type()));
    let handle = NEXT_POLL_HANDLE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    poll_registry().lock().unwrap().insert(
        handle,
        PollReg {
            fds: Vec::new(),
            polling: false,
        },
    );
    inst.dict
        .borrow_mut()
        .insert(DictKey(Object::from_static("_handle")), Object::Int(handle));
    Ok(Object::Instance(inst))
}

#[cfg(unix)]
fn poll_handle(args: &[Object]) -> Result<i64, RuntimeError> {
    match args.first() {
        Some(Object::Instance(i)) if i.cls().name == "poll" => {
            match i
                .dict
                .borrow()
                .get(&DictKey(Object::from_static("_handle")))
            {
                Some(Object::Int(h)) => Ok(*h),
                _ => Err(os_error("poll object is closed")),
            }
        }
        _ => Err(type_error("descriptor requires a 'select.poll' object")),
    }
}

#[cfg(unix)]
fn poll_default_mask() -> libc::c_short {
    libc::POLLIN | libc::POLLPRI | libc::POLLOUT
}

/// Parse a poll event mask: negative → `ValueError`, larger than a
/// C `unsigned short` → `OverflowError` (`test_poll.test_poll3`).
#[cfg(unix)]
fn parse_eventmask(arg: Option<&Object>) -> Result<libc::c_short, RuntimeError> {
    match arg {
        None | Some(Object::None) => Ok(poll_default_mask()),
        Some(Object::Int(n)) => {
            if *n < 0 {
                Err(crate::error::value_error("negative event mask"))
            } else if *n > i64::from(u16::MAX) {
                Err(crate::error::overflow_error(
                    "event mask value out of range",
                ))
            } else {
                Ok(*n as u16 as libc::c_short)
            }
        }
        Some(Object::Bool(b)) => Ok(libc::c_short::from(*b)),
        Some(Object::Long(_)) => Err(crate::error::overflow_error(
            "event mask value out of range",
        )),
        Some(_) => Err(type_error("integer argument expected, got non-integer")),
    }
}

#[cfg(unix)]
fn poll_register(args: &[Object]) -> Result<Object, RuntimeError> {
    let handle = poll_handle(args)?;
    let fd = fd_of(
        args.get(1)
            .ok_or_else(|| type_error("register() requires a file descriptor"))?,
    )?;
    let mask = parse_eventmask(args.get(2))?;
    let mut reg = poll_registry().lock().unwrap();
    let st = reg
        .get_mut(&handle)
        .ok_or_else(|| os_error("poll object is closed"))?;
    st.fds.retain(|(f, _)| *f != fd);
    st.fds.push((fd, mask));
    Ok(Object::None)
}

#[cfg(unix)]
fn poll_modify(args: &[Object]) -> Result<Object, RuntimeError> {
    let handle = poll_handle(args)?;
    let fd = fd_of(
        args.get(1)
            .ok_or_else(|| type_error("modify() requires a file descriptor"))?,
    )?;
    let mask = parse_eventmask(args.get(2))?;
    let mut reg = poll_registry().lock().unwrap();
    let st = reg
        .get_mut(&handle)
        .ok_or_else(|| os_error("poll object is closed"))?;
    match st.fds.iter_mut().find(|(f, _)| *f == fd) {
        Some(entry) => {
            entry.1 = mask;
            Ok(Object::None)
        }
        // CPython surfaces the underlying `poll`'s ENOENT.
        None => Err(crate::error::io_error_to_py(
            &std::io::Error::from_raw_os_error(libc::ENOENT),
        )),
    }
}

#[cfg(unix)]
fn poll_unregister(args: &[Object]) -> Result<Object, RuntimeError> {
    let handle = poll_handle(args)?;
    let fd = fd_of(
        args.get(1)
            .ok_or_else(|| type_error("unregister() requires a file descriptor"))?,
    )?;
    let mut reg = poll_registry().lock().unwrap();
    let st = reg
        .get_mut(&handle)
        .ok_or_else(|| os_error("poll object is closed"))?;
    let before = st.fds.len();
    st.fds.retain(|(f, _)| *f != fd);
    if st.fds.len() == before {
        return Err(crate::error::key_error(fd.to_string()));
    }
    Ok(Object::None)
}

/// `poll.poll(timeout=None)` — timeout is *milliseconds* (CPython). A
/// negative or `None` timeout blocks forever; a value larger than a
/// C `int` raises `OverflowError` (`test_poll.test_poll3`).
#[cfg(unix)]
fn poll_timeout(arg: Option<&Object>) -> Result<Option<Duration>, RuntimeError> {
    match arg {
        None | Some(Object::None) => Ok(None),
        Some(Object::Int(ms)) => {
            if *ms < 0 {
                Ok(None)
            } else if *ms > i64::from(libc::c_int::MAX) {
                Err(crate::error::overflow_error("timeout is too large"))
            } else {
                Ok(Some(Duration::from_millis(*ms as u64)))
            }
        }
        Some(Object::Bool(b)) => Ok(Some(Duration::from_millis(u64::from(*b)))),
        Some(Object::Float(ms)) => {
            if *ms < 0.0 {
                Ok(None)
            } else if *ms > f64::from(libc::c_int::MAX) {
                Err(crate::error::overflow_error("timeout is too large"))
            } else {
                Ok(Some(Duration::from_millis(*ms as u64)))
            }
        }
        Some(Object::Long(_)) => Err(crate::error::overflow_error("timeout is too large")),
        Some(_) => Err(type_error("timeout must be an integer or None")),
    }
}

#[cfg(unix)]
fn poll_poll(args: &[Object]) -> Result<Object, RuntimeError> {
    let handle = poll_handle(args)?;
    let timeout = poll_timeout(args.get(1))?;

    // Snapshot the registrations and claim the poll under the lock; an
    // in-flight poll keeps its own snapshot, so a concurrent
    // register/unregister from another thread doesn't disturb it.
    let mut pollfds: Vec<libc::pollfd> = {
        let mut reg = poll_registry().lock().unwrap();
        let st = reg
            .get_mut(&handle)
            .ok_or_else(|| os_error("poll object is closed"))?;
        if st.polling {
            return Err(crate::error::runtime_error("concurrent poll() invocation"));
        }
        st.polling = true;
        st.fds
            .iter()
            .map(|(fd, mask)| libc::pollfd {
                fd: *fd,
                events: *mask,
                revents: 0,
            })
            .collect()
    };

    let result = blocking_retry(timeout, |slice| {
        for p in pollfds.iter_mut() {
            p.revents = 0;
        }
        let ms = timeout_to_poll_ms(slice);
        let n = unsafe { libc::poll(pollfds.as_mut_ptr(), pollfds.len() as libc::nfds_t, ms) };
        if n < 0 {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(n as usize)
        }
    });

    if let Some(st) = poll_registry().lock().unwrap().get_mut(&handle) {
        st.polling = false;
    }
    result?;

    let out: Vec<Object> = pollfds
        .iter()
        .filter(|p| p.revents != 0)
        .map(|p| {
            Object::new_tuple(vec![
                Object::Int(i64::from(p.fd)),
                Object::Int(i64::from(p.revents)),
            ])
        })
        .collect();
    Ok(Object::new_list(out))
}

#[cfg(unix)]
fn poll_del(args: &[Object]) -> Result<Object, RuntimeError> {
    if let Ok(handle) = poll_handle(args) {
        poll_registry().lock().unwrap().remove(&handle);
    }
    Ok(Object::None)
}

// ---------------------------------------------------------------------------
// kqueue / kevent (macOS / BSD)
// ---------------------------------------------------------------------------

#[cfg(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "tvos",
    target_os = "watchos",
    target_os = "freebsd",
    target_os = "openbsd",
    target_os = "netbsd",
    target_os = "dragonfly"
))]
mod kqueue_impl {
    use super::{blocking_retry, fd_of, method};
    use crate::error::{os_error, type_error, RuntimeError};
    use crate::object::{DictData, DictKey, Object};
    use crate::sync::Rc;
    use crate::types::{PyInstance, TypeObject};
    use std::time::Duration;

    fn dur_to_timespec(d: Duration) -> libc::timespec {
        libc::timespec {
            tv_sec: d.as_secs().min(i64::MAX as u64) as libc::time_t,
            tv_nsec: libc::c_long::from(d.subsec_nanos()),
        }
    }

    thread_local! {
        static KQUEUE_CLASS: crate::sync::RefCell<Option<Rc<TypeObject>>> =
            const { crate::sync::RefCell::new(None) };
        static KEVENT_CLASS: crate::sync::RefCell<Option<Rc<TypeObject>>> =
            const { crate::sync::RefCell::new(None) };
    }

    pub(super) fn install(d: &mut DictData) {
        d.insert(
            DictKey(Object::from_static("kqueue")),
            Object::Type(kqueue_class()),
        );
        d.insert(
            DictKey(Object::from_static("kevent")),
            Object::Type(kevent_class()),
        );
        for (name, val) in kq_constants() {
            d.insert(DictKey(Object::from_str(name)), Object::Int(val));
        }
    }

    fn kq_constants() -> Vec<(&'static str, i64)> {
        vec![
            ("KQ_FILTER_READ", i64::from(libc::EVFILT_READ)),
            ("KQ_FILTER_WRITE", i64::from(libc::EVFILT_WRITE)),
            ("KQ_FILTER_AIO", i64::from(libc::EVFILT_AIO)),
            ("KQ_FILTER_VNODE", i64::from(libc::EVFILT_VNODE)),
            ("KQ_FILTER_PROC", i64::from(libc::EVFILT_PROC)),
            ("KQ_FILTER_SIGNAL", i64::from(libc::EVFILT_SIGNAL)),
            ("KQ_FILTER_TIMER", i64::from(libc::EVFILT_TIMER)),
            ("KQ_EV_ADD", i64::from(libc::EV_ADD)),
            ("KQ_EV_DELETE", i64::from(libc::EV_DELETE)),
            ("KQ_EV_ENABLE", i64::from(libc::EV_ENABLE)),
            ("KQ_EV_DISABLE", i64::from(libc::EV_DISABLE)),
            ("KQ_EV_ONESHOT", i64::from(libc::EV_ONESHOT)),
            ("KQ_EV_CLEAR", i64::from(libc::EV_CLEAR)),
            ("KQ_EV_EOF", i64::from(libc::EV_EOF)),
            ("KQ_EV_ERROR", i64::from(libc::EV_ERROR)),
            ("KQ_NOTE_DELETE", i64::from(libc::NOTE_DELETE)),
            ("KQ_NOTE_WRITE", i64::from(libc::NOTE_WRITE)),
            ("KQ_NOTE_EXTEND", i64::from(libc::NOTE_EXTEND)),
            ("KQ_NOTE_ATTRIB", i64::from(libc::NOTE_ATTRIB)),
            ("KQ_NOTE_LINK", i64::from(libc::NOTE_LINK)),
            ("KQ_NOTE_RENAME", i64::from(libc::NOTE_RENAME)),
            ("KQ_NOTE_REVOKE", i64::from(libc::NOTE_REVOKE)),
        ]
    }

    // ---- kevent value object ----

    fn kevent_class() -> Rc<TypeObject> {
        KEVENT_CLASS.with(|slot| {
            if let Some(c) = slot.borrow().as_ref() {
                return c.clone();
            }
            let bt = crate::builtin_types::builtin_types();
            let mut dict = DictData::new();
            for (name, m) in [
                ("__init__", method("__init__", kevent_init)),
                ("__repr__", method("__repr__", kevent_repr)),
                ("__eq__", method("__eq__", kevent_eq)),
                ("__ne__", method("__ne__", kevent_ne)),
                ("__lt__", method("__lt__", kevent_lt)),
                ("__le__", method("__le__", kevent_le)),
                ("__gt__", method("__gt__", kevent_gt)),
                ("__ge__", method("__ge__", kevent_ge)),
            ] {
                dict.insert(DictKey(Object::from_static(name)), m);
            }
            let cls = TypeObject::new_user("kevent", vec![bt.object_.clone()], dict)
                .expect("kevent class must linearise");
            *slot.borrow_mut() = Some(cls.clone());
            cls
        })
    }

    fn set_field(inst: &Rc<PyInstance>, name: &'static str, v: i64) {
        inst.dict
            .borrow_mut()
            .insert(DictKey(Object::from_static(name)), Object::Int(v));
    }

    fn get_field(inst: &PyInstance, name: &str) -> i64 {
        match inst.dict.borrow().get(&DictKey(Object::from_str(name))) {
            Some(Object::Int(n)) => *n,
            Some(Object::Bool(b)) => i64::from(*b),
            _ => 0,
        }
    }

    fn kevent_init(args: &[Object]) -> Result<Object, RuntimeError> {
        let inst = match args.first() {
            Some(Object::Instance(i)) => i.clone(),
            _ => return Err(type_error("kevent.__init__ requires self")),
        };
        let ident = i64::from(fd_of(
            args.get(1)
                .ok_or_else(|| type_error("kevent: ident required"))?,
        )?);
        let int_arg = |idx: usize, default: i64| -> Result<i64, RuntimeError> {
            match args.get(idx) {
                None | Some(Object::None) => Ok(default),
                Some(Object::Int(n)) => Ok(*n),
                Some(Object::Bool(b)) => Ok(i64::from(*b)),
                Some(_) => Err(type_error("kevent: integer field required")),
            }
        };
        set_field(&inst, "ident", ident);
        set_field(&inst, "filter", int_arg(2, i64::from(libc::EVFILT_READ))?);
        set_field(&inst, "flags", int_arg(3, i64::from(libc::EV_ADD))?);
        set_field(&inst, "fflags", int_arg(4, 0)?);
        set_field(&inst, "data", int_arg(5, 0)?);
        set_field(&inst, "udata", int_arg(6, 0)?);
        Ok(Object::None)
    }

    fn kevent_repr(args: &[Object]) -> Result<Object, RuntimeError> {
        let inst = match args.first() {
            Some(Object::Instance(i)) => i,
            _ => return Err(type_error("kevent.__repr__ requires self")),
        };
        Ok(Object::from_str(format!(
            "select.kevent(ident={}, filter={}, flags={}, fflags={}, data={}, udata={})",
            get_field(inst, "ident"),
            get_field(inst, "filter"),
            get_field(inst, "flags"),
            get_field(inst, "fflags"),
            get_field(inst, "data"),
            get_field(inst, "udata"),
        )))
    }

    /// The 6-tuple CPython compares kevents by.
    fn kevent_key(inst: &PyInstance) -> [i64; 6] {
        [
            get_field(inst, "ident"),
            get_field(inst, "filter"),
            get_field(inst, "flags"),
            get_field(inst, "fflags"),
            get_field(inst, "data"),
            get_field(inst, "udata"),
        ]
    }

    /// Run `op` on the two kevents' comparison keys, or return
    /// `NotImplemented` (so Python raises `TypeError` for an ordering
    /// against a non-kevent — `test_kqueue.test_create_event`).
    fn kevent_cmp(
        args: &[Object],
        op: impl Fn(&[i64; 6], &[i64; 6]) -> bool,
    ) -> Result<Object, RuntimeError> {
        match (args.first(), args.get(1)) {
            (Some(Object::Instance(a)), Some(Object::Instance(b)))
                if a.cls().name == "kevent" && b.cls().name == "kevent" =>
            {
                Ok(Object::Bool(op(&kevent_key(a), &kevent_key(b))))
            }
            _ => Ok(crate::vm_singletons::not_implemented()),
        }
    }

    fn kevent_eq(args: &[Object]) -> Result<Object, RuntimeError> {
        kevent_cmp(args, |a, b| a == b)
    }
    fn kevent_ne(args: &[Object]) -> Result<Object, RuntimeError> {
        kevent_cmp(args, |a, b| a != b)
    }
    fn kevent_lt(args: &[Object]) -> Result<Object, RuntimeError> {
        kevent_cmp(args, |a, b| a < b)
    }
    fn kevent_le(args: &[Object]) -> Result<Object, RuntimeError> {
        kevent_cmp(args, |a, b| a <= b)
    }
    fn kevent_gt(args: &[Object]) -> Result<Object, RuntimeError> {
        kevent_cmp(args, |a, b| a > b)
    }
    fn kevent_ge(args: &[Object]) -> Result<Object, RuntimeError> {
        kevent_cmp(args, |a, b| a >= b)
    }

    fn make_kevent(ev: &libc::kevent) -> Object {
        let inst = Rc::new(PyInstance::new(kevent_class()));
        set_field(&inst, "ident", ev.ident as i64);
        set_field(&inst, "filter", i64::from(ev.filter));
        set_field(&inst, "flags", i64::from(ev.flags));
        set_field(&inst, "fflags", i64::from(ev.fflags));
        set_field(&inst, "data", ev.data as i64);
        set_field(&inst, "udata", ev.udata as usize as i64);
        Object::Instance(inst)
    }

    fn kevent_to_native(obj: &Object) -> Result<libc::kevent, RuntimeError> {
        let inst = match obj {
            Object::Instance(i) if i.cls().name == "kevent" => i,
            _ => return Err(type_error("changelist must contain select.kevent objects")),
        };
        Ok(libc::kevent {
            ident: get_field(inst, "ident") as libc::uintptr_t,
            filter: get_field(inst, "filter") as i16,
            flags: get_field(inst, "flags") as u16,
            fflags: get_field(inst, "fflags") as u32,
            data: get_field(inst, "data") as libc::intptr_t,
            udata: get_field(inst, "udata") as usize as *mut libc::c_void,
        })
    }

    // ---- kqueue object ----
    //
    // State (the control fd and a closed flag) lives in the instance
    // dict — shared (Arc) across threads and read directly as the
    // `kq.closed` attribute (`test_kqueue.test_close`).

    fn kqueue_class() -> Rc<TypeObject> {
        KQUEUE_CLASS.with(|slot| {
            if let Some(c) = slot.borrow().as_ref() {
                return c.clone();
            }
            let bt = crate::builtin_types::builtin_types();
            let mut dict = DictData::new();
            for (name, m) in [
                ("__init__", method("__init__", kqueue_init)),
                ("__enter__", method("__enter__", kqueue_enter)),
                ("__exit__", method("__exit__", kqueue_exit)),
                ("control", method("control", kqueue_control)),
                ("fileno", method("fileno", kqueue_fileno)),
                ("close", method("close", kqueue_close)),
            ] {
                dict.insert(DictKey(Object::from_static(name)), m);
            }
            dict.insert(
                DictKey(Object::from_static("fromfd")),
                super::b("fromfd", kqueue_fromfd),
            );
            let cls = TypeObject::new_user("kqueue", vec![bt.object_.clone()], dict)
                .expect("kqueue class must linearise");
            *slot.borrow_mut() = Some(cls.clone());
            cls
        })
    }

    fn set_cloexec(fd: libc::c_int) {
        unsafe {
            let flags = libc::fcntl(fd, libc::F_GETFD);
            if flags >= 0 {
                libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC);
            }
        }
    }

    fn store_kqueue(inst: &Rc<PyInstance>, fd: libc::c_int) {
        let mut d = inst.dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("_fd")),
            Object::Int(i64::from(fd)),
        );
        d.insert(DictKey(Object::from_static("closed")), Object::Bool(false));
    }

    /// Return `(self, fd, closed)` for a kqueue method receiver.
    fn kqueue_state(args: &[Object]) -> Result<(Rc<PyInstance>, libc::c_int, bool), RuntimeError> {
        match args.first() {
            Some(Object::Instance(i)) if i.cls().name == "kqueue" => {
                let d = i.dict.borrow();
                let fd = match d.get(&DictKey(Object::from_static("_fd"))) {
                    Some(Object::Int(f)) => *f as libc::c_int,
                    _ => return Err(os_error("kqueue object is uninitialised")),
                };
                let closed = matches!(
                    d.get(&DictKey(Object::from_static("closed"))),
                    Some(Object::Bool(true))
                );
                Ok((i.clone(), fd, closed))
            }
            _ => Err(type_error("descriptor requires a 'select.kqueue' object")),
        }
    }

    fn kqueue_init(args: &[Object]) -> Result<Object, RuntimeError> {
        let inst = match args.first() {
            Some(Object::Instance(i)) => i.clone(),
            _ => return Err(type_error("kqueue.__init__ requires self")),
        };
        let fd = unsafe { libc::kqueue() };
        if fd < 0 {
            return Err(crate::error::io_error_to_py(
                &std::io::Error::last_os_error(),
            ));
        }
        set_cloexec(fd);
        store_kqueue(&inst, fd);
        Ok(Object::None)
    }

    /// `select.kqueue.fromfd(fd)` — wrap an existing control fd.
    fn kqueue_fromfd(args: &[Object]) -> Result<Object, RuntimeError> {
        let fd = fd_of(
            args.first()
                .ok_or_else(|| type_error("fromfd() requires an fd"))?,
        )?;
        let inst = Rc::new(PyInstance::new(kqueue_class()));
        store_kqueue(&inst, fd);
        Ok(Object::Instance(inst))
    }

    fn kqueue_enter(args: &[Object]) -> Result<Object, RuntimeError> {
        let (inst, _fd, closed) = kqueue_state(args)?;
        if closed {
            return Err(crate::error::value_error(
                "I/O operation on closed kqueue object",
            ));
        }
        Ok(Object::Instance(inst))
    }

    fn kqueue_exit(args: &[Object]) -> Result<Object, RuntimeError> {
        kqueue_close(args)?;
        Ok(Object::Bool(false))
    }

    fn kqueue_fileno(args: &[Object]) -> Result<Object, RuntimeError> {
        let (_inst, fd, closed) = kqueue_state(args)?;
        if closed {
            return Err(crate::error::value_error(
                "I/O operation on closed kqueue object",
            ));
        }
        Ok(Object::Int(i64::from(fd)))
    }

    fn kqueue_close(args: &[Object]) -> Result<Object, RuntimeError> {
        let (inst, fd, closed) = kqueue_state(args)?;
        if !closed {
            unsafe {
                libc::close(fd);
            }
            inst.dict
                .borrow_mut()
                .insert(DictKey(Object::from_static("closed")), Object::Bool(true));
        }
        Ok(Object::None)
    }

    /// `kqueue.control(changelist, max_events, timeout=None)`.
    fn kqueue_control(args: &[Object]) -> Result<Object, RuntimeError> {
        let (_inst, kq, closed) = kqueue_state(args)?;
        if closed {
            return Err(crate::error::value_error(
                "I/O operation on closed kqueue object",
            ));
        }

        // changelist: None or *any* iterable of kevent
        // (`test_kqueue.test_issue30058`).
        let changes: Vec<libc::kevent> = match args.get(1) {
            None | Some(Object::None) => Vec::new(),
            Some(other) => {
                let ptr = crate::vm_singletons::current_interpreter_ptr()
                    .ok_or_else(|| type_error("changelist must be an iterable or None"))?;
                let interp = unsafe { &mut *ptr };
                let globals = interp.builtins_dict();
                let items = interp
                    .collect_iterable(other, &globals)
                    .map_err(|_| type_error("changelist must be an iterable or None"))?;
                items
                    .iter()
                    .map(kevent_to_native)
                    .collect::<Result<_, _>>()?
            }
        };

        let max_events = match args.get(2) {
            None | Some(Object::None) => 0,
            Some(Object::Int(n)) if *n < 0 => {
                return Err(crate::error::value_error(
                    "Length of eventlist must be 0 or positive",
                ))
            }
            Some(Object::Int(n)) => *n as usize,
            Some(Object::Bool(b)) => usize::from(*b),
            Some(_) => return Err(type_error("max_events must be an integer")),
        };

        // With no eventlist, `kevent` only applies the changelist and
        // returns immediately regardless of timeout — model that as a
        // single non-blocking pass (a blocking slice loop would spin).
        let timeout = if max_events == 0 {
            Some(Duration::ZERO)
        } else {
            super::parse_secs(args.get(3))?
        };

        let mut events: Vec<libc::kevent> = (0..max_events)
            .map(|_| unsafe { std::mem::zeroed() })
            .collect();
        let mut changes_opt = Some(changes);

        let n = blocking_retry(timeout, |slice| {
            let (cptr, clen) = match &changes_opt {
                Some(c) if !c.is_empty() => (c.as_ptr(), c.len() as libc::c_int),
                _ => (std::ptr::null(), 0),
            };
            // `None` slice (off-main, block forever) → NULL timespec.
            let ts = slice.map(dur_to_timespec);
            let tsp = ts
                .as_ref()
                .map_or(std::ptr::null(), std::ptr::from_ref);
            let r = unsafe {
                libc::kevent(
                    kq,
                    cptr,
                    clen,
                    events.as_mut_ptr(),
                    max_events as libc::c_int,
                    tsp,
                )
            };
            // The changelist is applied on the first syscall; never
            // re-apply it on a subsequent slice / retry.
            changes_opt = None;
            if r < 0 {
                Err(std::io::Error::last_os_error())
            } else {
                Ok(r as usize)
            }
        })?;

        let out: Vec<Object> = events.iter().take(n).map(make_kevent).collect();
        Ok(Object::new_list(out))
    }
}

#[cfg(not(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "tvos",
    target_os = "watchos",
    target_os = "freebsd",
    target_os = "openbsd",
    target_os = "netbsd",
    target_os = "dragonfly"
)))]
mod kqueue_impl {
    use crate::object::DictData;
    pub(super) fn install(_d: &mut DictData) {}
}

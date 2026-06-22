//! The `socket` built-in module.
//!
//! Provides BSD-style sockets — TCP, UDP, and (on Unix) `AF_UNIX` —
//! backed by `socket2::Socket`. Sockets are Python instances of a
//! `socket.socket` class registered with the type system, so
//! `isinstance(s, socket.socket)` works.
//!
//! ## Storage
//!
//! The Rust-side state (the underlying `socket2::Socket`, the
//! timeout, the blocking flag) lives in a thread-local registry
//! keyed by an integer "handle id". The Python-visible instance
//! carries that integer as `_handle` plus mirrors `family`, `type`,
//! `proto`, and `timeout` for `getattr` access. We use the same
//! id for `fileno()`, which means `socket.fileno()` returns the
//! underlying OS file descriptor on Unix (matching CPython).
//!
//! ## Scope
//!
//! Covered: `socket(family, type, proto)`, `bind`, `listen`,
//! `accept`, `connect`, `connect_ex`, `send`, `sendall`, `sendto`,
//! `recv`, `recv_into`, `recvfrom`, `setblocking`, `settimeout`,
//! `gettimeout`, `setsockopt`, `getsockopt`, `getsockname`,
//! `getpeername`, `fileno`, `close`, `shutdown`, `detach`,
//! `makefile`, the module-level `gethostname`/`gethostbyname`/
//! `getaddrinfo`/`getnameinfo`/`create_connection`/`create_server`/
//! `inet_aton`/`inet_ntoa`/`inet_pton`/`inet_ntop`/`htons`/`htonl`/
//! `ntohs`/`ntohl`/`socketpair`, the full set of `AF_*` / `SOCK_*` /
//! `IPPROTO_*` / `SOL_SOCKET` / `SO_*` / `TCP_*` / `IP_*` / `MSG_*` /
//! `SHUT_*` / `AI_*` / `NI_*` constants on POSIX, and a subset on
//! Windows.
//!
//! Deferred: platform-specific options (`SO_BINDTODEVICE`,
//! `TCP_FASTOPEN`, `IP_TRANSPARENT`), `if_*` interface enumeration,
//! and `recvmsg`/`sendmsg` ancillary-data passing.

use crate::sync::Rc;
use crate::sync::RefCell;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, Shutdown, SocketAddr, ToSocketAddrs};
use std::time::Duration;

use socket2::{Domain, Protocol, SockAddr, Socket, Type};

use crate::error::{
    blocking_io_error, io_error_to_py, os_error, type_error, value_error, RuntimeError,
};
use crate::import::ModuleCache;
use crate::object::{BuiltinFn, DictData, DictKey, Object, PyModule};
use crate::types::{PyInstance, TypeObject};

// ---- registry ----

struct SocketState {
    inner: Option<Socket>,
    family: i32,
    kind: i32,
    proto: i32,
    timeout: Option<Duration>,
    blocking: bool,
}

// The socket registry is process-global (shared across all OS threads),
// *not* thread-local. RFC 0039 gives WeavePy real threads, and CPython
// sockets are usable from any thread — most critically asyncio's self-pipe
// write end (`loop._csock`) is created on the loop thread but written from
// executor worker threads inside `call_soon_threadsafe` to wake the
// selector. A thread-local registry made that socket resolve to "fd -1 /
// already closed" off its creating thread, so the wakeup byte was silently
// dropped and any loop blocked in `select()` waiting on a cross-thread
// result (`run_in_executor`, `call_soon_threadsafe`) deadlocked forever.
// `Rc`/`RefCell` here alias `Arc`/`GilCell` (RFC 0025), so the stored
// `SocketState` handles are already `Send + Sync`.
fn registry() -> &'static parking_lot::Mutex<HashMap<i64, Rc<RefCell<SocketState>>>> {
    static REGISTRY: std::sync::OnceLock<
        parking_lot::Mutex<HashMap<i64, Rc<RefCell<SocketState>>>>,
    > = std::sync::OnceLock::new();
    REGISTRY.get_or_init(|| parking_lot::Mutex::new(HashMap::new()))
}

fn next_handle(state: Rc<RefCell<SocketState>>) -> i64 {
    use std::sync::atomic::{AtomicI64, Ordering};
    // Synthetic-handle counter for sockets without an extractable OS fd.
    static NEXT_HANDLE: AtomicI64 = AtomicI64::new(0);
    // Use the underlying OS fd as the handle if available so `fileno()`
    // returns something a host C library would accept. Fall back to a
    // monotonically *decreasing* synthetic id otherwise. The `state` borrow
    // is a temporary scoped to this statement, so it's released before
    // `state` is moved into the registry below.
    let handle = state
        .borrow()
        .inner
        .as_ref()
        .and_then(raw_fd_of)
        .unwrap_or_else(|| -(NEXT_HANDLE.fetch_add(1, Ordering::Relaxed) + 1));
    registry().lock().insert(handle, state);
    handle
}

#[cfg(unix)]
fn raw_fd_of(sock: &Socket) -> Option<i64> {
    use std::os::unix::io::AsRawFd;
    Some(i64::from(sock.as_raw_fd()))
}

#[cfg(windows)]
fn raw_fd_of(sock: &Socket) -> Option<i64> {
    use std::os::windows::io::AsRawSocket;
    Some(sock.as_raw_socket() as i64)
}

#[cfg(not(any(unix, windows)))]
fn raw_fd_of(_sock: &Socket) -> Option<i64> {
    None
}

/// Consume a `Socket`, releasing its OS file descriptor *without* closing
/// it. This is the ownership transfer `socket.detach()` performs: the
/// Python object stops managing the fd, but the fd stays open for the
/// caller. Dropping the `Socket` (as `Option::take` then drop would) is
/// wrong here — it closes the fd, and with socket2's IO-safety that turns
/// a later legitimate close of the same fd into a process abort.
#[cfg(unix)]
fn into_raw_fd_of(sock: Socket) -> i64 {
    use std::os::unix::io::IntoRawFd;
    i64::from(sock.into_raw_fd())
}

#[cfg(windows)]
fn into_raw_fd_of(sock: Socket) -> i64 {
    use std::os::windows::io::IntoRawSocket;
    sock.into_raw_socket() as i64
}

#[cfg(not(any(unix, windows)))]
fn into_raw_fd_of(_sock: Socket) -> i64 {
    -1
}

fn get_state(handle: i64) -> Option<Rc<RefCell<SocketState>>> {
    registry().lock().get(&handle).cloned()
}

fn remove_state(handle: i64) {
    registry().lock().remove(&handle);
}

/// Borrow the raw OS file descriptor for the given socket handle.
/// Used by `_ssl` (RFC 0023) to wrap an existing socket with rustls.
#[allow(dead_code)]
pub(crate) fn raw_fd_for_handle(handle: i64) -> Option<i64> {
    let state = get_state(handle)?;
    let state = state.borrow();
    state.inner.as_ref().and_then(raw_fd_of)
}

// ---- module entry ----

pub fn build(_cache: &ModuleCache) -> Rc<PyModule> {
    let dict = Rc::new(RefCell::new(DictData::new()));
    {
        let mut d = dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("__name__")),
            Object::from_static("socket"),
        );
        d.insert(
            DictKey(Object::from_static("__doc__")),
            Object::from_static("Low-level networking interface."),
        );

        // Address families.
        d.insert(
            DictKey(Object::from_static("AF_INET")),
            Object::Int(libc_af_inet()),
        );
        d.insert(
            DictKey(Object::from_static("AF_INET6")),
            Object::Int(libc_af_inet6()),
        );
        #[cfg(unix)]
        d.insert(DictKey(Object::from_static("AF_UNIX")), Object::Int(1));
        d.insert(DictKey(Object::from_static("AF_UNSPEC")), Object::Int(0));

        // Socket kinds.
        d.insert(
            DictKey(Object::from_static("SOCK_STREAM")),
            Object::Int(libc_sock_stream()),
        );
        d.insert(
            DictKey(Object::from_static("SOCK_DGRAM")),
            Object::Int(libc_sock_dgram()),
        );
        d.insert(DictKey(Object::from_static("SOCK_RAW")), Object::Int(3));
        d.insert(
            DictKey(Object::from_static("SOCK_NONBLOCK")),
            Object::Int(2048),
        );
        d.insert(
            DictKey(Object::from_static("SOCK_CLOEXEC")),
            Object::Int(524_288),
        );

        // Protocol numbers.
        d.insert(DictKey(Object::from_static("IPPROTO_IP")), Object::Int(0));
        d.insert(DictKey(Object::from_static("IPPROTO_TCP")), Object::Int(6));
        d.insert(DictKey(Object::from_static("IPPROTO_UDP")), Object::Int(17));
        d.insert(
            DictKey(Object::from_static("IPPROTO_IPV6")),
            Object::Int(41),
        );
        d.insert(DictKey(Object::from_static("IPPROTO_ICMP")), Object::Int(1));

        // IPv6 socket options. `IPV6_V6ONLY` differs by platform
        // (BSD/macOS use 27, Linux uses 26); asyncio's `create_server`
        // sets it on dual-stack listeners.
        d.insert(
            DictKey(Object::from_static("IPV6_V6ONLY")),
            Object::Int(if cfg!(any(target_os = "macos", target_os = "ios")) {
                27
            } else {
                26
            }),
        );

        // Option levels.
        d.insert(
            DictKey(Object::from_static("SOL_SOCKET")),
            Object::Int(libc_sol_socket()),
        );
        d.insert(DictKey(Object::from_static("SOL_TCP")), Object::Int(6));
        d.insert(DictKey(Object::from_static("SOL_UDP")), Object::Int(17));

        // SO_* socket options.
        d.insert(
            DictKey(Object::from_static("SO_REUSEADDR")),
            Object::Int(libc_so_reuseaddr()),
        );
        d.insert(
            DictKey(Object::from_static("SO_REUSEPORT")),
            Object::Int(libc_so_reuseport()),
        );
        d.insert(
            DictKey(Object::from_static("SO_BROADCAST")),
            Object::Int(libc_so_broadcast()),
        );
        d.insert(
            DictKey(Object::from_static("SO_KEEPALIVE")),
            Object::Int(libc_so_keepalive()),
        );
        d.insert(
            DictKey(Object::from_static("SO_LINGER")),
            Object::Int(libc_so_linger()),
        );
        d.insert(
            DictKey(Object::from_static("SO_OOBINLINE")),
            Object::Int(10),
        );
        d.insert(
            DictKey(Object::from_static("SO_SNDBUF")),
            Object::Int(libc_so_sndbuf()),
        );
        d.insert(
            DictKey(Object::from_static("SO_RCVBUF")),
            Object::Int(libc_so_rcvbuf()),
        );
        d.insert(DictKey(Object::from_static("SO_SNDTIMEO")), Object::Int(21));
        d.insert(DictKey(Object::from_static("SO_RCVTIMEO")), Object::Int(20));
        d.insert(DictKey(Object::from_static("SO_ERROR")), Object::Int(4));
        d.insert(DictKey(Object::from_static("SO_TYPE")), Object::Int(3));

        // TCP_*
        d.insert(DictKey(Object::from_static("TCP_NODELAY")), Object::Int(1));
        d.insert(DictKey(Object::from_static("TCP_MAXSEG")), Object::Int(2));
        d.insert(DictKey(Object::from_static("TCP_KEEPIDLE")), Object::Int(4));
        d.insert(
            DictKey(Object::from_static("TCP_KEEPINTVL")),
            Object::Int(5),
        );
        d.insert(DictKey(Object::from_static("TCP_KEEPCNT")), Object::Int(6));

        // IP_*
        d.insert(DictKey(Object::from_static("IP_TOS")), Object::Int(1));
        d.insert(DictKey(Object::from_static("IP_TTL")), Object::Int(2));
        d.insert(
            DictKey(Object::from_static("IP_MULTICAST_TTL")),
            Object::Int(10),
        );
        d.insert(
            DictKey(Object::from_static("IP_MULTICAST_LOOP")),
            Object::Int(11),
        );

        // Recv flags.
        d.insert(DictKey(Object::from_static("MSG_OOB")), Object::Int(1));
        d.insert(DictKey(Object::from_static("MSG_PEEK")), Object::Int(2));
        d.insert(DictKey(Object::from_static("MSG_WAITALL")), Object::Int(64));
        d.insert(
            DictKey(Object::from_static("MSG_DONTWAIT")),
            Object::Int(128),
        );
        // Ancillary-data control-message types. `SCM_RIGHTS` carries file
        // descriptors over an AF_UNIX socket via `sendmsg`/`recvmsg`; it is
        // what `multiprocessing.reduction.send_handle`/`recv_handle` (and so
        // the `forkserver` start method, `resource_sharer`, and Connection fd
        // handoff) require. Its presence here is what makes
        // `reduction.HAVE_SEND_HANDLE` true.
        #[cfg(unix)]
        d.insert(
            DictKey(Object::from_static("SCM_RIGHTS")),
            Object::Int(i64::from(libc::SCM_RIGHTS)),
        );
        #[cfg(target_os = "linux")]
        d.insert(
            DictKey(Object::from_static("SCM_CREDENTIALS")),
            Object::Int(i64::from(libc::SCM_CREDENTIALS)),
        );

        // shutdown(how) — match CPython numbering.
        d.insert(DictKey(Object::from_static("SHUT_RD")), Object::Int(0));
        d.insert(DictKey(Object::from_static("SHUT_WR")), Object::Int(1));
        d.insert(DictKey(Object::from_static("SHUT_RDWR")), Object::Int(2));

        // getaddrinfo flags.
        d.insert(DictKey(Object::from_static("AI_PASSIVE")), Object::Int(1));
        d.insert(DictKey(Object::from_static("AI_CANONNAME")), Object::Int(2));
        d.insert(
            DictKey(Object::from_static("AI_NUMERICHOST")),
            Object::Int(4),
        );
        d.insert(
            DictKey(Object::from_static("AI_NUMERICSERV")),
            Object::Int(8),
        );

        // getnameinfo flags.
        d.insert(
            DictKey(Object::from_static("NI_NUMERICHOST")),
            Object::Int(1),
        );
        d.insert(
            DictKey(Object::from_static("NI_NUMERICSERV")),
            Object::Int(2),
        );
        d.insert(DictKey(Object::from_static("NI_NAMEREQD")), Object::Int(4));
        d.insert(DictKey(Object::from_static("NI_DGRAM")), Object::Int(16));

        // Sentinels.
        d.insert(DictKey(Object::from_static("INADDR_ANY")), Object::Int(0));
        d.insert(
            DictKey(Object::from_static("INADDR_LOOPBACK")),
            Object::Int(0x7F00_0001),
        );
        d.insert(
            DictKey(Object::from_static("INADDR_BROADCAST")),
            Object::Int(0xFFFF_FFFF_i64.wrapping_neg()),
        );
        d.insert(DictKey(Object::from_static("has_ipv6")), Object::Bool(true));

        // Capabilities.
        d.insert(
            DictKey(Object::from_static("socket")),
            Object::Type(socket_class()),
        );
        d.insert(
            DictKey(Object::from_static("SocketType")),
            Object::Type(socket_class()),
        );
        d.insert(
            DictKey(Object::from_static("error")),
            Object::Type(crate::builtin_types::builtin_types().os_error.clone()),
        );
        d.insert(
            DictKey(Object::from_static("herror")),
            Object::Type(crate::builtin_types::builtin_types().os_error.clone()),
        );
        d.insert(
            DictKey(Object::from_static("gaierror")),
            Object::Type(crate::builtin_types::builtin_types().os_error.clone()),
        );
        d.insert(
            DictKey(Object::from_static("timeout")),
            Object::Type(crate::builtin_types::builtin_types().timeout_error.clone()),
        );

        // Module-level functions.
        for (name, body) in module_functions() {
            d.insert(DictKey(Object::from_static(name)), b(name, *body));
        }
    }

    Rc::new(PyModule {
        name: "socket".to_owned(),
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

// ---- socket class construction ----

fn socket_class() -> Rc<TypeObject> {
    // Process-global (shared across threads) so a socket built on a worker
    // thread is an instance of the *same* class object as `socket.socket`
    // exported from the module, keeping `isinstance` correct cross-thread.
    // Construction never re-enters `socket_class()`, so `get_or_init` is safe.
    static SOCKET_CLASS: std::sync::OnceLock<Rc<TypeObject>> = std::sync::OnceLock::new();
    SOCKET_CLASS
        .get_or_init(|| {
            let bt = crate::builtin_types::builtin_types();
            let mut dict = DictData::new();
            for (name, method) in socket_methods() {
                dict.insert(DictKey(Object::from_str(name)), method);
            }
            let cls = TypeObject::new_user("socket", vec![bt.object_.clone()], dict)
                .expect("socket class must linearise");
            // Expose `family`/`type`/`proto`/`timeout` as class-level getset
            // descriptors so they show up in `dir(socket.socket)` (CPython
            // parity); this is what `unittest.mock`'s `spec=` allow-list and
            // a number of `test_asyncio` transport tests depend on.
            install_socket_getset(&cls);
            // The constructor lives on the class as `__init__`, and the
            // module-level `socket.socket(...)` callable goes through
            // `Vm::instantiate` which dispatches it.
            cls
        })
        .clone()
}

fn socket_methods() -> Vec<(&'static str, Object)> {
    macro_rules! m {
        ($name:literal, $body:expr) => {
            (
                $name,
                Object::Builtin(Rc::new(BuiltinFn {
                    name: $name,
                    binds_instance: true,
                    call: Box::new($body),
                    call_kw: None,
                })),
            )
        };
    }
    vec![
        // `__init__` is kwargs-aware: `socket(family=..., type=..., proto=...,
        // fileno=...)` is idiomatic CPython (e.g. asyncio's `_connect_sock`).
        (
            "__init__",
            Object::Builtin(Rc::new(BuiltinFn {
                name: "__init__",
                binds_instance: true,
                call: Box::new(|args| sock_init_kw(args, &[])),
                call_kw: Some(Box::new(sock_init_kw)),
            })),
        ),
        m!("__enter__", sock_enter),
        m!("__exit__", sock_exit),
        m!("__repr__", sock_repr),
        m!("bind", sock_bind),
        m!("listen", sock_listen),
        m!("accept", sock_accept),
        m!("connect", sock_connect),
        m!("connect_ex", sock_connect_ex),
        m!("send", sock_send),
        m!("sendall", sock_sendall),
        m!("sendto", sock_sendto),
        m!("recv", sock_recv),
        m!("recv_into", sock_recv_into),
        m!("recvfrom", sock_recvfrom),
        m!("sendmsg", sock_sendmsg),
        m!("recvmsg", sock_recvmsg),
        m!("setblocking", sock_setblocking),
        m!("getblocking", sock_getblocking),
        m!("settimeout", sock_settimeout),
        m!("gettimeout", sock_gettimeout),
        m!("setsockopt", sock_setsockopt),
        m!("getsockopt", sock_getsockopt),
        m!("getsockname", sock_getsockname),
        m!("getpeername", sock_getpeername),
        m!("fileno", sock_fileno),
        m!("close", sock_close),
        m!("shutdown", sock_shutdown),
        m!("detach", sock_detach),
        m!("dup", sock_dup),
        m!("makefile", sock_makefile),
        m!("family_get", sock_family_attr),
        m!("type_get", sock_type_attr),
        m!("proto_get", sock_proto_attr),
    ]
}

fn extract_self(args: &[Object]) -> Result<Rc<PyInstance>, RuntimeError> {
    match args.first() {
        Some(Object::Instance(inst)) if inst.cls().name == "socket" => Ok(inst.clone()),
        _ => Err(type_error("socket method requires socket self")),
    }
}

fn extract_handle(inst: &PyInstance) -> Result<i64, RuntimeError> {
    let dict = inst.dict.borrow();
    match dict.get(&DictKey(Object::from_static("_handle"))) {
        Some(Object::Int(h)) => Ok(*h),
        _ => Err(os_error("socket already closed")),
    }
}

fn state_of(args: &[Object]) -> Result<Rc<RefCell<SocketState>>, RuntimeError> {
    let inst = extract_self(args)?;
    let handle = extract_handle(&inst)?;
    get_state(handle).ok_or_else(|| os_error("socket already closed"))
}

/// Wrap an already-open OS file descriptor in a `socket2::Socket`,
/// taking ownership of it (matching CPython's `socket(fileno=fd)`, which
/// does *not* dup the fd). Used by the `fileno=` constructor path.
#[cfg(unix)]
fn wrap_fd_socket(fd: i64) -> Result<Socket, RuntimeError> {
    use std::os::unix::io::FromRawFd;
    if fd < 0 {
        return Err(os_error("negative file descriptor"));
    }
    Ok(unsafe { Socket::from_raw_fd(fd as std::os::unix::io::RawFd) })
}

#[cfg(windows)]
fn wrap_fd_socket(fd: i64) -> Result<Socket, RuntimeError> {
    use std::os::windows::io::FromRawSocket;
    Ok(unsafe { Socket::from_raw_socket(fd as u64 as std::os::windows::io::RawSocket) })
}

#[cfg(not(any(unix, windows)))]
fn wrap_fd_socket(_fd: i64) -> Result<Socket, RuntimeError> {
    Err(os_error(
        "fileno argument is not supported on this platform",
    ))
}

/// Reconstruct a *non-owning* `Socket` view over an already-open fd.
///
/// The returned `ManuallyDrop` deliberately never runs `Socket`'s
/// destructor, so dropping it does **not** close the descriptor — the
/// real owner stays inside `SocketState::inner`. Callers only ever take
/// `&*view`.
#[cfg(unix)]
fn fd_to_socket_view(fd: i64) -> std::mem::ManuallyDrop<Socket> {
    use std::os::unix::io::FromRawFd;
    std::mem::ManuallyDrop::new(unsafe { Socket::from_raw_fd(fd as std::os::unix::io::RawFd) })
}

#[cfg(windows)]
fn fd_to_socket_view(fd: i64) -> std::mem::ManuallyDrop<Socket> {
    use std::os::windows::io::FromRawSocket;
    std::mem::ManuallyDrop::new(unsafe {
        Socket::from_raw_socket(fd as u64 as std::os::windows::io::RawSocket)
    })
}

/// Drive a blocking socket syscall with the GIL released and *without*
/// holding the `SocketState` cell borrow.
///
/// RFC 0039 (real threads + GIL): a blocking syscall must mirror
/// CPython's `Py_BEGIN_ALLOW_THREADS … Py_END_ALLOW_THREADS`, otherwise
/// two failure modes appear once sockets are touched from more than one
/// OS thread:
///
/// 1. **Cell deadlock.** Holding the socket's `RefCell`/`GilCell` borrow
///    across the syscall parks any peer thread that tries to
///    `borrow`/`borrow_mut` the *same* socket — e.g. the loop thread
///    closing a listener during teardown while a server thread is parked
///    in `accept()`, or an executor worker closing a socket the loop is
///    reading. The observed `test_streams` hang was exactly this: the
///    loop thread blocked in `close()`'s `borrow_mut` behind a server
///    thread blocked in `accept()`.
/// 2. **GIL starvation.** Keeping the GIL held across the syscall stops
///    every other Python thread from running for the syscall's whole
///    (unbounded) duration.
///
/// We snapshot the raw fd, drop the borrow, then run the syscall through
/// [`allow_threads_then`]. Peers can run — and may even legitimately
/// `close()` this fd to interrupt us, in which case the syscall fails
/// with `EBADF`, exactly as on CPython.
/// One blocking attempt of `f` with the GIL released and *without* holding the
/// `SocketState` cell borrow. Returns the raw syscall result so the caller can
/// decide whether to retry (e.g. on `EINTR`). See [`blocking_socket_io`] for
/// the GIL/borrow rationale.
#[cfg(any(unix, windows))]
fn socket_call_once<R>(
    state: &Rc<RefCell<SocketState>>,
    f: &mut dyn FnMut(&Socket) -> std::io::Result<R>,
) -> Result<std::io::Result<R>, RuntimeError> {
    let fd = {
        let b = state.borrow();
        let sock = b.inner.as_ref().ok_or_else(|| os_error("socket closed"))?;
        raw_fd_of(sock).ok_or_else(|| os_error("socket has no file descriptor"))?
    };
    let view = fd_to_socket_view(fd);
    Ok(crate::gil::allow_threads_then(|| f(&view)))
}

/// Run the Python signal handlers a just-observed `EINTR` may have tripped,
/// now that the GIL is re-acquired (PEP 475). A handler that raises (e.g.
/// `KeyboardInterrupt` from the default `SIGINT` handler) propagates and
/// aborts the surrounding retry loop, exactly like CPython's `sock_call_ex`.
#[cfg(unix)]
fn run_pending_signals_after_eintr() -> Result<(), RuntimeError> {
    if crate::stdlib::signal_mod::signals_pending() {
        if let Some(ptr) = crate::vm_singletons::current_interpreter_ptr() {
            // SAFETY: the GIL is held (we just returned from `allow_threads_then`),
            // so the interpreter pointer is exclusively ours.
            unsafe { (*ptr).run_pending_signals_public()? };
        }
    }
    Ok(())
}

/// Drive a blocking *single-syscall* socket op, retrying on `EINTR` after
/// running pending Python signal handlers (PEP 475 — "Retry system calls
/// failing with EINTR"). A signal that interrupts a blocking `accept`/`recv`/
/// `send` no longer surfaces as `InterruptedError`; instead the handler runs
/// and the call resumes, matching CPython.
/// `_test_multiprocessing`'s `TestIgnoreEINTR.test_ignore_listener` relies on
/// a `SIGUSR1` interrupting a child's blocking `accept()` and the accept then
/// resuming so the parent can connect.
///
/// Only ops that map to a *single* syscall route through here, so a plain retry
/// resumes correctly — `sendall` loops per-chunk in its caller (re-sending
/// committed bytes would corrupt the stream) and `connect` uses
/// [`socket_call_once`] directly (a signal-interrupted blocking connect
/// completes asynchronously, so re-issuing `connect(2)` would return
/// `EISCONN`/`EALREADY`).
#[cfg(any(unix, windows))]
fn blocking_socket_io<R>(
    state: &Rc<RefCell<SocketState>>,
    mut f: impl FnMut(&Socket) -> std::io::Result<R>,
) -> Result<R, RuntimeError> {
    loop {
        match socket_call_once(state, &mut f)? {
            Ok(v) => return Ok(v),
            #[cfg(unix)]
            Err(ref e) if e.raw_os_error() == Some(libc::EINTR) => {
                run_pending_signals_after_eintr()?;
                continue;
            }
            Err(e) => return Err(io_error_to_py(&e)),
        }
    }
}

#[cfg(not(any(unix, windows)))]
fn socket_call_once<R>(
    _state: &Rc<RefCell<SocketState>>,
    _f: &mut dyn FnMut(&Socket) -> std::io::Result<R>,
) -> Result<std::io::Result<R>, RuntimeError> {
    Err(os_error("sockets are not supported on this platform"))
}

#[cfg(not(any(unix, windows)))]
fn blocking_socket_io<R>(
    _state: &Rc<RefCell<SocketState>>,
    _f: impl FnMut(&Socket) -> std::io::Result<R>,
) -> Result<R, RuntimeError> {
    Err(os_error("sockets are not supported on this platform"))
}

fn sock_init(args: &[Object]) -> Result<Object, RuntimeError> {
    sock_init_kw(args, &[])
}

fn sock_init_kw(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    // CPython signature: socket(family=-1, type=-1, proto=-1, fileno=None).
    // args[0] is self; the rest fill family/type/proto/fileno positionally,
    // and any of those four may instead be passed by keyword.
    let inst = extract_self(args)?;
    const NAMES: [&str; 4] = ["family", "type", "proto", "fileno"];
    let pos = &args[1..];
    if pos.len() > NAMES.len() {
        return Err(type_error(format!(
            "socket() takes at most {} arguments ({} given)",
            NAMES.len(),
            pos.len()
        )));
    }
    let mut slots: [Option<Object>; 4] = [None, None, None, None];
    for (i, v) in pos.iter().enumerate() {
        slots[i] = Some(v.clone());
    }
    for (k, v) in kwargs {
        match NAMES.iter().position(|n| n == k) {
            Some(idx) if slots[idx].is_some() => {
                return Err(type_error(format!(
                    "socket() got multiple values for argument '{k}'"
                )));
            }
            Some(idx) => slots[idx] = Some(v.clone()),
            None => {
                return Err(type_error(format!(
                    "socket() got an unexpected keyword argument '{k}'"
                )));
            }
        }
    }
    let as_i32 = |slot: &Option<Object>, default: i32, what: &str| -> Result<i32, RuntimeError> {
        match slot {
            // CPython treats the -1 sentinel as "use the default".
            Some(Object::Int(i)) if *i == -1 => Ok(default),
            Some(Object::Int(i)) => Ok(*i as i32),
            None | Some(Object::None) => Ok(default),
            _ => Err(type_error(format!("{what} must be int"))),
        }
    };
    let family = as_i32(&slots[0], libc_af_inet() as i32, "family")?;
    let kind = as_i32(&slots[1], libc_sock_stream() as i32, "type")?;
    let proto = as_i32(&slots[2], 0, "proto")?;
    let fileno = match &slots[3] {
        None | Some(Object::None) => None,
        Some(Object::Int(fd)) => Some(*fd),
        _ => return Err(type_error("fileno must be int or None")),
    };
    let inner = match fileno {
        Some(fd) => wrap_fd_socket(fd)?,
        None => Socket::new(
            Domain::from(family),
            Type::from(kind),
            Some(Protocol::from(proto)),
        )
        .map_err(|e| io_error_to_py(&e))?,
    };
    let state = Rc::new(RefCell::new(SocketState {
        inner: Some(inner),
        family,
        kind,
        proto,
        timeout: None,
        blocking: true,
    }));
    let handle = next_handle(state);
    let mut dict = inst.dict.borrow_mut();
    dict.insert(DictKey(Object::from_static("_handle")), Object::Int(handle));
    dict.insert(
        DictKey(Object::from_static("family")),
        Object::Int(i64::from(family)),
    );
    dict.insert(
        DictKey(Object::from_static("type")),
        Object::Int(i64::from(kind)),
    );
    dict.insert(
        DictKey(Object::from_static("proto")),
        Object::Int(i64::from(proto)),
    );
    Ok(Object::None)
}

fn sock_enter(args: &[Object]) -> Result<Object, RuntimeError> {
    args.first()
        .cloned()
        .ok_or_else(|| type_error("missing self"))
}

fn sock_exit(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = extract_self(args)?;
    if let Ok(handle) = extract_handle(&inst) {
        if let Some(state) = get_state(handle) {
            state.borrow_mut().inner.take();
        }
        remove_state(handle);
    }
    inst.dict
        .borrow_mut()
        .insert(DictKey(Object::from_static("_handle")), Object::Int(-1));
    Ok(Object::Bool(false))
}

fn sock_repr(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = extract_self(args)?;
    let dict = inst.dict.borrow();
    let family = dict
        .get(&DictKey(Object::from_static("family")))
        .cloned()
        .unwrap_or(Object::Int(0));
    let kind = dict
        .get(&DictKey(Object::from_static("type")))
        .cloned()
        .unwrap_or(Object::Int(0));
    let proto = dict
        .get(&DictKey(Object::from_static("proto")))
        .cloned()
        .unwrap_or(Object::Int(0));
    Ok(Object::from_str(format!(
        "<socket.socket family={} type={} proto={}>",
        family.repr(),
        kind.repr(),
        proto.repr()
    )))
}

fn sock_bind(args: &[Object]) -> Result<Object, RuntimeError> {
    let state = state_of(args)?;
    let family = state.borrow().family;
    let addr = parse_sockaddr2(args.get(1), family)?;
    let s_borrow = state.borrow();
    let sock = s_borrow
        .inner
        .as_ref()
        .ok_or_else(|| os_error("socket closed"))?;
    sock.bind(&addr).map_err(|e| io_error_to_py(&e))?;
    Ok(Object::None)
}

fn sock_listen(args: &[Object]) -> Result<Object, RuntimeError> {
    let state = state_of(args)?;
    let backlog = match args.get(1) {
        Some(Object::Int(n)) => *n as i32,
        None | Some(Object::None) => 128,
        _ => return Err(type_error("backlog must be int")),
    };
    let s_borrow = state.borrow();
    let sock = s_borrow
        .inner
        .as_ref()
        .ok_or_else(|| os_error("socket closed"))?;
    sock.listen(backlog).map_err(|e| io_error_to_py(&e))?;
    Ok(Object::None)
}

fn sock_accept(args: &[Object]) -> Result<Object, RuntimeError> {
    let state = state_of(args)?;
    // Use `accept_raw` (a bare `accept(2)`) rather than socket2's `accept`,
    // which on Apple platforms *also* runs `setsockopt(SO_NOSIGPIPE)` on the
    // freshly accepted fd. When the peer connected and then *closed* (and its
    // process exited) before we accept, the new connection's protocol control
    // block is already torn down and that post-accept setsockopt fails with
    // EINVAL — turning a perfectly valid accept (the bytes the peer sent are
    // still queued and readable) into an error. CPython issues a plain
    // `accept(2)`, never setting NOSIGPIPE at accept time (SIGPIPE is ignored
    // process-wide; see the `signal` init), so it returns the connection and a
    // subsequent `recv()` drains the buffered data then sees EOF.
    // `_test_multiprocessing`'s `WithProcessesTestListenerClient.test_issue14725`
    // exercises exactly this race (child writes, closes, exits; parent accepts).
    let (new_sock, addr) = blocking_socket_io(&state, |sock| sock.accept_raw())?;
    // PEP 446: accepted descriptors are non-inheritable. `F_SETFD` acts on the
    // descriptor-table entry (not the connection), so it succeeds even for a
    // peer-closed connection; ignore any error to stay non-fatal regardless.
    let _ = new_sock.set_cloexec(true);
    let (family, kind, proto) = {
        let s = state.borrow();
        (s.family, s.kind, s.proto)
    };
    let new_state = Rc::new(RefCell::new(SocketState {
        inner: Some(new_sock),
        family,
        kind,
        proto,
        timeout: None,
        blocking: true,
    }));
    let handle = next_handle(new_state);
    let cls = socket_class();
    let inst = Rc::new(PyInstance::new(cls));
    {
        let mut d = inst.dict.borrow_mut();
        d.insert(DictKey(Object::from_static("_handle")), Object::Int(handle));
        d.insert(
            DictKey(Object::from_static("family")),
            Object::Int(i64::from(family)),
        );
        d.insert(
            DictKey(Object::from_static("type")),
            Object::Int(i64::from(kind)),
        );
        d.insert(
            DictKey(Object::from_static("proto")),
            Object::Int(i64::from(proto)),
        );
    }
    let addr_tuple = sockaddr_to_tuple(&addr, family);
    Ok(Object::new_tuple(vec![Object::Instance(inst), addr_tuple]))
}

fn sock_connect(args: &[Object]) -> Result<Object, RuntimeError> {
    let state = state_of(args)?;
    let (family, timeout) = {
        let b = state.borrow();
        (b.family, b.timeout)
    };
    let sockaddr = parse_sockaddr2(args.get(1), family)?;
    let mut connect_fn = move |sock: &Socket| match timeout {
        // A strictly-positive timeout means "timeout mode": bound the
        // connect with `connect_timeout`.
        Some(t) if !t.is_zero() => sock.connect_timeout(&sockaddr, t),
        // `Some(0)` is non-blocking mode (CPython couples
        // `setblocking(False)`/`settimeout(0)` to a zero timeout) and
        // `None` is blocking. In both cases issue a plain `connect`: on a
        // non-blocking fd it surfaces `EINPROGRESS`/`EWOULDBLOCK`, exactly
        // like CPython, instead of being mis-read as a 0-second deadline.
        _ => sock.connect(&sockaddr),
    };
    // Single attempt — *no* EINTR retry: a signal-interrupted blocking
    // connect continues asynchronously, so a second `connect(2)` would return
    // `EISCONN`/`EALREADY` rather than completing it. Surface the syscall
    // result directly (a handled signal still ran via the eval breaker).
    match socket_call_once(&state, &mut connect_fn)? {
        Ok(()) => Ok(Object::None),
        Err(e) => Err(io_error_to_py(&e)),
    }
}

fn sock_connect_ex(args: &[Object]) -> Result<Object, RuntimeError> {
    match sock_connect(args) {
        Ok(_) => Ok(Object::Int(0)),
        // CPython's `connect_ex` returns the raw C errno instead of
        // raising. asyncio's `loop.sock_connect` depends on this: it
        // treats `EINPROGRESS`/`EWOULDBLOCK` as "in flight" and anything
        // else as a hard failure. `io_error_to_py` stashes the errno on
        // the exception's `.errno`, so recover it from there.
        Err(RuntimeError::PyException(p)) => {
            let errno = errno_of_exception(&p).unwrap_or(i64::from(libc::EINVAL));
            Ok(Object::Int(errno))
        }
        Err(e) => Err(e),
    }
}

/// Recover the integer `errno` an `OSError`-family exception was built with
/// (see [`crate::error::io_error_to_py`]), if present.
fn errno_of_exception(p: &crate::error::PyException) -> Option<i64> {
    if let Object::Instance(inst) = &p.instance {
        if let Some(Object::Int(n)) = inst
            .dict
            .borrow()
            .get(&DictKey(Object::from_static("errno")))
        {
            return Some(*n);
        }
    }
    None
}

fn sock_send(args: &[Object]) -> Result<Object, RuntimeError> {
    let state = state_of(args)?;
    let data = extract_bytes(args.get(1))?;
    let n = blocking_socket_io(&state, |sock| sock.send(&data))?;
    Ok(Object::Int(n as i64))
}

fn sock_sendall(args: &[Object]) -> Result<Object, RuntimeError> {
    let state = state_of(args)?;
    let data = extract_bytes(args.get(1))?;
    // Loop per chunk in the caller (not inside one `blocking_socket_io`
    // closure) so a PEP 475 `EINTR` retry resumes from the current `offset`
    // instead of re-running the whole send and duplicating already-committed
    // bytes. Each individual `send` is the single retryable syscall.
    let mut offset = 0;
    while offset < data.len() {
        let n = blocking_socket_io(&state, |sock| sock.send(&data[offset..]))?;
        if n == 0 {
            return Err(io_error_to_py(&std::io::Error::from(
                std::io::ErrorKind::BrokenPipe,
            )));
        }
        offset += n;
    }
    Ok(Object::None)
}

fn sock_sendto(args: &[Object]) -> Result<Object, RuntimeError> {
    let state = state_of(args)?;
    let data = extract_bytes(args.get(1))?;
    let family = state.borrow().family;
    let addr = parse_socket_address(args.get(2), family)?;
    let sockaddr = SockAddr::from(addr);
    let n = blocking_socket_io(&state, |sock| sock.send_to(&data, &sockaddr))?;
    Ok(Object::Int(n as i64))
}

fn sock_recv(args: &[Object]) -> Result<Object, RuntimeError> {
    let state = state_of(args)?;
    let bufsize = match args.get(1) {
        Some(Object::Int(n)) => *n as usize,
        _ => return Err(type_error("recv: bufsize must be int")),
    };
    let mut buf: Vec<std::mem::MaybeUninit<u8>> = vec![std::mem::MaybeUninit::uninit(); bufsize];
    let n = blocking_socket_io(&state, |sock| sock.recv(&mut buf))?;
    let initialised: Vec<u8> = buf[..n]
        .iter()
        .map(|m| unsafe { m.assume_init() })
        .collect();
    Ok(Object::new_bytes(initialised))
}

fn sock_recv_into(args: &[Object]) -> Result<Object, RuntimeError> {
    let state = state_of(args)?;
    let buffer = args.get(1);
    let nbytes = match args.get(2) {
        Some(Object::Int(n)) => *n as usize,
        _ => 0,
    };
    let cap = match buffer {
        Some(Object::ByteArray(b)) => {
            if nbytes == 0 {
                b.borrow().len()
            } else {
                nbytes.min(b.borrow().len())
            }
        }
        _ => return Err(type_error("recv_into expects a bytearray")),
    };
    let mut buf = vec![std::mem::MaybeUninit::<u8>::uninit(); cap];
    let n = blocking_socket_io(&state, |sock| sock.recv(&mut buf))?;
    if let Some(Object::ByteArray(b)) = buffer {
        let mut bytes = b.borrow_mut();
        for i in 0..n {
            bytes[i] = unsafe { buf[i].assume_init() };
        }
    }
    Ok(Object::Int(n as i64))
}

fn sock_recvfrom(args: &[Object]) -> Result<Object, RuntimeError> {
    let state = state_of(args)?;
    let bufsize = match args.get(1) {
        Some(Object::Int(n)) => *n as usize,
        _ => return Err(type_error("recvfrom: bufsize must be int")),
    };
    let mut buf = vec![std::mem::MaybeUninit::<u8>::uninit(); bufsize];
    let (n, addr) = blocking_socket_io(&state, |sock| sock.recv_from(&mut buf))?;
    let initialised: Vec<u8> = buf[..n]
        .iter()
        .map(|m| unsafe { m.assume_init() })
        .collect();
    let family = state.borrow().family;
    Ok(Object::new_tuple(vec![
        Object::new_bytes(initialised),
        sockaddr_to_tuple(&addr, family),
    ]))
}

/// `socket.sendmsg(buffers[, ancdata[, flags[, address]]])` — send normal
/// data plus ancillary data (control messages) on a connected socket.
///
/// WeavePy uses this for `multiprocessing`'s `SCM_RIGHTS` file-descriptor
/// hand-off (`reduction.sendfds`): the `forkserver` start method, the
/// `resource_sharer`, and `Connection` fd transfer all push fds through an
/// AF_UNIX socket this way. The `address` argument (unconnected send) is not
/// supported — every WeavePy caller uses a connected pair.
#[cfg(unix)]
fn sock_sendmsg(args: &[Object]) -> Result<Object, RuntimeError> {
    use std::os::raw::c_void;
    let state = state_of(args)?;
    let buffers = extract_iov_buffers(args.get(1))?;
    let ancdata = extract_ancdata(args.get(2))?;
    let flags: libc::c_int = match args.get(3) {
        None | Some(Object::None) => 0,
        Some(Object::Int(n)) => *n as libc::c_int,
        _ => return Err(type_error("sendmsg: flags must be an integer")),
    };
    if matches!(args.get(4), Some(o) if !matches!(o, Object::None)) {
        return Err(os_error("sendmsg: address argument is not supported"));
    }

    let mut iovecs: Vec<libc::iovec> = buffers
        .iter()
        .map(|b| libc::iovec {
            iov_base: b.as_ptr() as *mut c_void,
            iov_len: b.len(),
        })
        .collect();

    let controllen: usize = ancdata
        .iter()
        .map(|(_, _, d)| unsafe { libc::CMSG_SPACE(d.len() as u32) } as usize)
        .sum();
    let mut control: Vec<u8> = vec![0u8; controllen];

    let fd = snapshot_raw_fd(&state)?;
    let iov_ptr = iovecs.as_mut_ptr();
    let iov_len = iovecs.len();
    let ctrl_ptr = if controllen > 0 {
        control.as_mut_ptr()
    } else {
        std::ptr::null_mut()
    };

    let sent = crate::gil::allow_threads_then(move || unsafe {
        let mut msg: libc::msghdr = std::mem::zeroed();
        msg.msg_iov = iov_ptr;
        msg.msg_iovlen = iov_len as _;
        if controllen > 0 {
            msg.msg_control = ctrl_ptr as *mut c_void;
            msg.msg_controllen = controllen as _;
            let mut cmsg = libc::CMSG_FIRSTHDR(&msg);
            for (level, ctype, data) in &ancdata {
                if cmsg.is_null() {
                    break;
                }
                (*cmsg).cmsg_level = *level;
                (*cmsg).cmsg_type = *ctype;
                (*cmsg).cmsg_len = libc::CMSG_LEN(data.len() as u32) as _;
                std::ptr::copy_nonoverlapping(
                    data.as_ptr(),
                    libc::CMSG_DATA(cmsg).cast::<u8>(),
                    data.len(),
                );
                cmsg = libc::CMSG_NXTHDR(&msg, cmsg);
            }
        }
        libc::sendmsg(fd, &msg, flags)
    });
    if sent < 0 {
        return Err(io_error_to_py(&std::io::Error::last_os_error()));
    }
    Ok(Object::Int(sent as i64))
}

/// `socket.recvmsg(bufsize[, ancbufsize[, flags]])` — receive normal data
/// plus ancillary data, returning `(data, ancdata, msg_flags, address)`.
/// `ancdata` is a list of `(cmsg_level, cmsg_type, cmsg_data)` triples (the
/// shape `multiprocessing.reduction.recvfds` decodes). `address` is `None`
/// (the WeavePy callers all use connected sockets).
#[cfg(unix)]
fn sock_recvmsg(args: &[Object]) -> Result<Object, RuntimeError> {
    use std::os::raw::c_void;
    let state = state_of(args)?;
    let bufsize = match args.get(1) {
        Some(Object::Int(n)) if *n >= 0 => *n as usize,
        Some(Object::Int(_)) => return Err(value_error("negative buffersize in recvmsg")),
        _ => return Err(type_error("recvmsg: bufsize must be an integer")),
    };
    let ancbufsize = match args.get(2) {
        None | Some(Object::None) => 0usize,
        Some(Object::Int(n)) if *n >= 0 => *n as usize,
        Some(Object::Int(_)) => return Err(value_error("negative ancillary data buffer size")),
        _ => return Err(type_error("recvmsg: ancbufsize must be an integer")),
    };
    let flags: libc::c_int = match args.get(3) {
        None | Some(Object::None) => 0,
        Some(Object::Int(n)) => *n as libc::c_int,
        _ => return Err(type_error("recvmsg: flags must be an integer")),
    };

    let mut databuf = vec![0u8; bufsize];
    let mut control = vec![0u8; ancbufsize];
    let fd = snapshot_raw_fd(&state)?;

    let mut iov = [libc::iovec {
        iov_base: databuf.as_mut_ptr() as *mut c_void,
        iov_len: bufsize,
    }];
    let iov_ptr = iov.as_mut_ptr();
    let ctrl_ptr = if ancbufsize > 0 {
        control.as_mut_ptr()
    } else {
        std::ptr::null_mut()
    };

    let (n, msg_flags, used_controllen) = crate::gil::allow_threads_then(move || unsafe {
        let mut msg: libc::msghdr = std::mem::zeroed();
        msg.msg_iov = iov_ptr;
        msg.msg_iovlen = 1 as _;
        if ancbufsize > 0 {
            msg.msg_control = ctrl_ptr as *mut c_void;
            msg.msg_controllen = ancbufsize as _;
        }
        let n = libc::recvmsg(fd, &mut msg, flags);
        (n, i64::from(msg.msg_flags), msg.msg_controllen as usize)
    });
    if n < 0 {
        return Err(io_error_to_py(&std::io::Error::last_os_error()));
    }
    databuf.truncate(n as usize);

    let mut ancdata_items: Vec<Object> = Vec::new();
    if ancbufsize > 0 && used_controllen > 0 {
        unsafe {
            let mut msg: libc::msghdr = std::mem::zeroed();
            msg.msg_control = control.as_mut_ptr() as *mut c_void;
            msg.msg_controllen = used_controllen as _;
            let mut cmsg = libc::CMSG_FIRSTHDR(&msg);
            while !cmsg.is_null() {
                let level = (*cmsg).cmsg_level;
                let ctype = (*cmsg).cmsg_type;
                let cmsg_len = (*cmsg).cmsg_len as usize;
                let data_ptr = libc::CMSG_DATA(cmsg);
                // Payload length = cmsg_len minus the (aligned) header up to
                // CMSG_DATA — never `CMSG_LEN(0)`, which omits the alignment.
                let data_offset = (data_ptr as usize).saturating_sub(cmsg as usize);
                let data_len = cmsg_len.saturating_sub(data_offset);
                let data = std::slice::from_raw_parts(data_ptr.cast::<u8>(), data_len).to_vec();
                ancdata_items.push(Object::new_tuple(vec![
                    Object::Int(i64::from(level)),
                    Object::Int(i64::from(ctype)),
                    Object::new_bytes(data),
                ]));
                cmsg = libc::CMSG_NXTHDR(&msg, cmsg);
            }
        }
    }

    Ok(Object::new_tuple(vec![
        Object::new_bytes(databuf),
        Object::new_list(ancdata_items),
        Object::Int(msg_flags),
        Object::None,
    ]))
}

#[cfg(not(unix))]
fn sock_sendmsg(_args: &[Object]) -> Result<Object, RuntimeError> {
    Err(os_error("sendmsg is not supported on this platform"))
}

#[cfg(not(unix))]
fn sock_recvmsg(_args: &[Object]) -> Result<Object, RuntimeError> {
    Err(os_error("recvmsg is not supported on this platform"))
}

/// Snapshot the raw fd of `state`, dropping the borrow before the syscall
/// (a peer thread may legitimately `close()` it; we then see `EBADF`).
#[cfg(unix)]
fn snapshot_raw_fd(state: &Rc<RefCell<SocketState>>) -> Result<libc::c_int, RuntimeError> {
    let b = state.borrow();
    let sock = b.inner.as_ref().ok_or_else(|| os_error("socket closed"))?;
    let fd = raw_fd_of(sock).ok_or_else(|| os_error("socket has no file descriptor"))?;
    Ok(fd as libc::c_int)
}

/// Extract the `sendmsg` iovec list — an iterable of bytes-like objects.
fn extract_iov_buffers(arg: Option<&Object>) -> Result<Vec<Vec<u8>>, RuntimeError> {
    let items: Vec<Object> = match arg {
        Some(Object::List(l)) => l.borrow().clone(),
        Some(Object::Tuple(t)) => t.to_vec(),
        _ => {
            return Err(type_error(
                "sendmsg(): argument 1 must be an iterable of bytes-like objects",
            ))
        }
    };
    items.iter().map(|o| extract_bytes(Some(o))).collect()
}

/// Extract `sendmsg` ancillary data: an iterable of `(cmsg_level, cmsg_type,
/// cmsg_data)` triples. `cmsg_data` may be any bytes-like object, including
/// the `array.array('i', fds)` `multiprocessing.reduction.sendfds` passes.
#[cfg(unix)]
fn extract_ancdata(
    arg: Option<&Object>,
) -> Result<Vec<(libc::c_int, libc::c_int, Vec<u8>)>, RuntimeError> {
    let items: Vec<Object> = match arg {
        None | Some(Object::None) => return Ok(Vec::new()),
        Some(Object::List(l)) => l.borrow().clone(),
        Some(Object::Tuple(t)) => t.to_vec(),
        _ => {
            return Err(type_error(
                "sendmsg(): ancillary data must be an iterable of zero or more triples",
            ))
        }
    };
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        let triple = match &item {
            Object::Tuple(t) if t.len() == 3 => t.to_vec(),
            Object::List(l) if l.borrow().len() == 3 => l.borrow().clone(),
            _ => {
                return Err(type_error(
                    "sendmsg(): ancillary data items must be (cmsg_level, cmsg_type, cmsg_data) triples",
                ))
            }
        };
        let level = match &triple[0] {
            Object::Int(n) => *n as libc::c_int,
            _ => return Err(type_error("sendmsg(): an integer is required (cmsg_level)")),
        };
        let ctype = match &triple[1] {
            Object::Int(n) => *n as libc::c_int,
            _ => return Err(type_error("sendmsg(): an integer is required (cmsg_type)")),
        };
        let data = cmsg_data_bytes(&triple[2])?;
        out.push((level, ctype, data));
    }
    Ok(out)
}

/// Bytes of a control-message payload. Falls back to the buffer protocol via
/// `obj.tobytes()` for objects that aren't native bytes/bytearray/memoryview
/// — notably `array.array`, which `multiprocessing` uses for the fd array.
#[cfg(unix)]
fn cmsg_data_bytes(obj: &Object) -> Result<Vec<u8>, RuntimeError> {
    if let Ok(b) = extract_bytes(Some(obj)) {
        return Ok(b);
    }
    if let Some(ptr) = crate::vm_singletons::current_interpreter_ptr() {
        // SAFETY: published by the active builtin call on this thread; the
        // interpreter outlives this call.
        let interp = unsafe { &mut *ptr };
        if let Ok(method) = interp.load_attr_public(obj, "tobytes") {
            if let Object::Bytes(b) = interp.call_object(method, &[], &[])? {
                return Ok(b.to_vec());
            }
        }
    }
    Err(type_error(
        "sendmsg(): ancillary data must be a bytes-like object",
    ))
}

fn sock_setblocking(args: &[Object]) -> Result<Object, RuntimeError> {
    let state = state_of(args)?;
    let flag = match args.get(1) {
        Some(Object::Bool(b)) => *b,
        Some(Object::Int(n)) => *n != 0,
        _ => return Err(type_error("setblocking: arg must be bool")),
    };
    {
        let s_borrow = state.borrow();
        let sock = s_borrow
            .inner
            .as_ref()
            .ok_or_else(|| os_error("socket closed"))?;
        sock.set_nonblocking(!flag)
            .map_err(|e| io_error_to_py(&e))?;
    }
    {
        let mut s = state.borrow_mut();
        s.blocking = flag;
        // CPython couples blocking-mode and timeout: `setblocking(False)`
        // is exactly `settimeout(0.0)` and `setblocking(True)` is
        // `settimeout(None)`. asyncio relies on `gettimeout() == 0` to
        // confirm a socket is non-blocking, so keep them in lockstep.
        s.timeout = if flag {
            None
        } else {
            Some(Duration::from_secs(0))
        };
    }
    Ok(Object::None)
}

fn sock_getblocking(args: &[Object]) -> Result<Object, RuntimeError> {
    let state = state_of(args)?;
    let blocking = state.borrow().blocking;
    Ok(Object::Bool(blocking))
}

fn sock_settimeout(args: &[Object]) -> Result<Object, RuntimeError> {
    let state = state_of(args)?;
    let timeout = match args.get(1) {
        None | Some(Object::None) => None,
        Some(Object::Float(f)) => Some(Duration::from_secs_f64(*f)),
        Some(Object::Int(n)) => Some(Duration::from_secs(*n as u64)),
        _ => return Err(type_error("settimeout: arg must be number or None")),
    };
    // CPython: a zero timeout puts the socket in non-blocking mode; a
    // positive timeout is "timeout mode" (also non-blocking at the fd
    // level, with the wait bounded by the runtime); `None` is blocking.
    {
        let s_borrow = state.borrow();
        let sock = s_borrow
            .inner
            .as_ref()
            .ok_or_else(|| os_error("socket closed"))?;
        match timeout {
            // Zero ⇒ pure non-blocking; don't program a 0-duration SO_*TIMEO
            // (some platforms read that as "block forever").
            Some(d) if d.is_zero() => {
                sock.set_nonblocking(true).map_err(|e| io_error_to_py(&e))?;
            }
            Some(d) => {
                sock.set_read_timeout(Some(d))
                    .map_err(|e| io_error_to_py(&e))?;
                sock.set_write_timeout(Some(d))
                    .map_err(|e| io_error_to_py(&e))?;
            }
            None => {
                sock.set_nonblocking(false)
                    .map_err(|e| io_error_to_py(&e))?;
                sock.set_read_timeout(None)
                    .map_err(|e| io_error_to_py(&e))?;
                sock.set_write_timeout(None)
                    .map_err(|e| io_error_to_py(&e))?;
            }
        }
    }
    {
        let mut s = state.borrow_mut();
        s.timeout = timeout;
        s.blocking = timeout.is_none();
    }
    Ok(Object::None)
}

fn sock_gettimeout(args: &[Object]) -> Result<Object, RuntimeError> {
    let state = state_of(args)?;
    let timeout = state.borrow().timeout;
    match timeout {
        None => Ok(Object::None),
        Some(d) => Ok(Object::Float(d.as_secs_f64())),
    }
}

fn sock_setsockopt(args: &[Object]) -> Result<Object, RuntimeError> {
    let state = state_of(args)?;
    let level = match args.get(1) {
        Some(Object::Int(n)) => *n as i32,
        _ => return Err(type_error("setsockopt: level must be int")),
    };
    let optname = match args.get(2) {
        Some(Object::Int(n)) => *n as i32,
        _ => return Err(type_error("setsockopt: optname must be int")),
    };
    let value = args.get(3);
    let s_borrow = state.borrow();
    let sock = s_borrow
        .inner
        .as_ref()
        .ok_or_else(|| os_error("socket closed"))?;
    // We only implement the option names most user code reaches for
    // by name rather than passing arbitrary bytes through to libc.
    let want = match value {
        Some(Object::Int(n)) => *n,
        Some(Object::Bool(b)) => i64::from(*b),
        _ => 0,
    };
    if optname == libc_so_reuseaddr() as i32 {
        sock.set_reuse_address(want != 0)
            .map_err(|e| io_error_to_py(&e))?;
    } else if optname == libc_so_reuseport() as i32 {
        #[cfg(unix)]
        sock.set_reuse_port(want != 0)
            .map_err(|e| io_error_to_py(&e))?;
    } else if optname == libc_so_keepalive() as i32 {
        sock.set_keepalive(want != 0)
            .map_err(|e| io_error_to_py(&e))?;
    } else if optname == libc_so_broadcast() as i32 {
        sock.set_broadcast(want != 0)
            .map_err(|e| io_error_to_py(&e))?;
    } else if level == 6 && optname == 1 {
        // TCP_NODELAY (level IPPROTO_TCP/SOL_TCP).
        sock.set_nodelay(want != 0)
            .map_err(|e| io_error_to_py(&e))?;
    } else if optname == libc_so_sndbuf() as i32 {
        sock.set_send_buffer_size(want as usize)
            .map_err(|e| io_error_to_py(&e))?;
    } else if optname == libc_so_rcvbuf() as i32 {
        sock.set_recv_buffer_size(want as usize)
            .map_err(|e| io_error_to_py(&e))?;
    }
    Ok(Object::None)
}

fn sock_getsockopt(args: &[Object]) -> Result<Object, RuntimeError> {
    let state = state_of(args)?;
    let level = match args.get(1) {
        Some(Object::Int(n)) => *n as i32,
        _ => return Err(type_error("getsockopt: level must be int")),
    };
    let optname = match args.get(2) {
        Some(Object::Int(n)) => *n as i32,
        _ => return Err(type_error("getsockopt: optname must be int")),
    };
    let s_borrow = state.borrow();
    let sock = s_borrow
        .inner
        .as_ref()
        .ok_or_else(|| os_error("socket closed"))?;
    let as_int = |b: bool| Object::Int(i64::from(b));
    // TCP_NODELAY lives at the IPPROTO_TCP/SOL_TCP level (6); disambiguate
    // it from SOL_SOCKET options that share the numeric optname 1.
    if level == 6 && optname == 1 {
        return Ok(as_int(sock.nodelay().map_err(|e| io_error_to_py(&e))?));
    }
    if optname == 4 {
        // SO_ERROR — return last error number, or 0.
        let err = sock.take_error().ok().flatten();
        return Ok(Object::Int(
            err.map_or(0, |e| i64::from(e.raw_os_error().unwrap_or(0))),
        ));
    }
    if optname == 3 {
        // SO_TYPE — return our recorded kind.
        return Ok(Object::Int(i64::from(s_borrow.kind)));
    }
    // Read back the SO_* options we know how to set, so a
    // setsockopt/getsockopt round-trip reflects reality (CPython parity;
    // asyncio's `_set_nodelay` and several transport tests rely on this).
    if optname == libc_so_reuseaddr() as i32 {
        return Ok(as_int(
            sock.reuse_address().map_err(|e| io_error_to_py(&e))?,
        ));
    }
    #[cfg(unix)]
    if optname == libc_so_reuseport() as i32 {
        return Ok(as_int(sock.reuse_port().map_err(|e| io_error_to_py(&e))?));
    }
    if optname == libc_so_keepalive() as i32 {
        return Ok(as_int(sock.keepalive().map_err(|e| io_error_to_py(&e))?));
    }
    if optname == libc_so_broadcast() as i32 {
        return Ok(as_int(sock.broadcast().map_err(|e| io_error_to_py(&e))?));
    }
    if optname == libc_so_sndbuf() as i32 {
        return Ok(Object::Int(
            sock.send_buffer_size().map_err(|e| io_error_to_py(&e))? as i64,
        ));
    }
    if optname == libc_so_rcvbuf() as i32 {
        return Ok(Object::Int(
            sock.recv_buffer_size().map_err(|e| io_error_to_py(&e))? as i64,
        ));
    }
    // For anything else, return 0 as a safe default.
    Ok(Object::Int(0))
}

fn sock_getsockname(args: &[Object]) -> Result<Object, RuntimeError> {
    let state = state_of(args)?;
    let s_borrow = state.borrow();
    let sock = s_borrow
        .inner
        .as_ref()
        .ok_or_else(|| os_error("socket closed"))?;
    let addr = sock.local_addr().map_err(|e| io_error_to_py(&e))?;
    Ok(sockaddr_to_tuple(&addr, s_borrow.family))
}

fn sock_getpeername(args: &[Object]) -> Result<Object, RuntimeError> {
    let state = state_of(args)?;
    let s_borrow = state.borrow();
    let sock = s_borrow
        .inner
        .as_ref()
        .ok_or_else(|| os_error("socket closed"))?;
    let addr = sock.peer_addr().map_err(|e| io_error_to_py(&e))?;
    Ok(sockaddr_to_tuple(&addr, s_borrow.family))
}

fn sock_fileno(args: &[Object]) -> Result<Object, RuntimeError> {
    // `fileno()` must return the real OS file descriptor — `select` /
    // `selectors` / `mio` all use it directly via the kernel's
    // multiplexer. We keep the opaque WeavePy handle separately on
    // `_handle` so the C-ish API still works for code that wants to
    // reach the socket by id.
    let inst = extract_self(args)?;
    let handle = extract_handle(&inst).unwrap_or(-1);
    if handle < 0 {
        return Ok(Object::Int(-1));
    }
    let state = match get_state(handle) {
        Some(s) => s,
        None => return Ok(Object::Int(-1)),
    };
    let borrow = state.borrow();
    if let Some(sock) = borrow.inner.as_ref() {
        if let Some(fd) = raw_fd_of(sock) {
            return Ok(Object::Int(fd));
        }
    }
    Ok(Object::Int(-1))
}

fn sock_close(args: &[Object]) -> Result<Object, RuntimeError> {
    sock_exit(args)?;
    Ok(Object::None)
}

fn sock_shutdown(args: &[Object]) -> Result<Object, RuntimeError> {
    let state = state_of(args)?;
    let how = match args.get(1) {
        Some(Object::Int(n)) => *n,
        _ => return Err(type_error("shutdown: arg must be int")),
    };
    let shutdown = match how {
        0 => Shutdown::Read,
        1 => Shutdown::Write,
        _ => Shutdown::Both,
    };
    let s_borrow = state.borrow();
    let sock = s_borrow
        .inner
        .as_ref()
        .ok_or_else(|| os_error("socket closed"))?;
    sock.shutdown(shutdown).map_err(|e| io_error_to_py(&e))?;
    Ok(Object::None)
}

fn sock_detach(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = extract_self(args)?;
    let h = extract_handle(&inst)?;
    // Release the fd without closing it, and report the real OS fd.
    let mut fd = h;
    if let Some(state) = get_state(h) {
        if let Some(sock) = state.borrow_mut().inner.take() {
            fd = into_raw_fd_of(sock);
        }
    }
    remove_state(h);
    inst.dict
        .borrow_mut()
        .insert(DictKey(Object::from_static("_handle")), Object::Int(-1));
    Ok(Object::Int(fd))
}

/// `socket.dup()` — duplicate the underlying fd (real `dup(2)`) and wrap
/// it in a fresh `socket` object that shares the family/type/proto. The
/// duplicate is an independent fd: closing one leaves the other usable,
/// matching CPython's `socket.dup()`.
#[cfg(unix)]
fn sock_dup(args: &[Object]) -> Result<Object, RuntimeError> {
    let state = state_of(args)?;
    let (family, kind, proto) = {
        let s = state.borrow();
        (s.family, s.kind, s.proto)
    };
    let new_fd = {
        let b = state.borrow();
        let sock = b.inner.as_ref().ok_or_else(|| os_error("socket closed"))?;
        let fd = raw_fd_of(sock).ok_or_else(|| os_error("socket has no file descriptor"))?;
        let dup = unsafe { libc::dup(fd as i32) };
        if dup < 0 {
            return Err(io_error_to_py(&std::io::Error::last_os_error()));
        }
        i64::from(dup)
    };
    let inner = wrap_fd_socket(new_fd)?;
    let new_state = Rc::new(RefCell::new(SocketState {
        inner: Some(inner),
        family,
        kind,
        proto,
        timeout: None,
        blocking: true,
    }));
    let handle = next_handle(new_state);
    let cls = socket_class();
    let inst = Rc::new(PyInstance::new(cls));
    {
        let mut d = inst.dict.borrow_mut();
        d.insert(DictKey(Object::from_static("_handle")), Object::Int(handle));
        d.insert(
            DictKey(Object::from_static("family")),
            Object::Int(i64::from(family)),
        );
        d.insert(
            DictKey(Object::from_static("type")),
            Object::Int(i64::from(kind)),
        );
        d.insert(
            DictKey(Object::from_static("proto")),
            Object::Int(i64::from(proto)),
        );
    }
    Ok(Object::Instance(inst))
}

/// On non-Unix platforms WeavePy has no `dup(2)`-backed fd duplication, so
/// `socket.dup()` is unsupported (mirrors the `#[cfg(not(unix))]` stubs used
/// elsewhere in this module and in `select`).
#[cfg(not(unix))]
fn sock_dup(args: &[Object]) -> Result<Object, RuntimeError> {
    let _ = state_of(args)?;
    Err(os_error("socket.dup is only supported on Unix"))
}

fn sock_makefile(args: &[Object]) -> Result<Object, RuntimeError> {
    // We don't expose a real FileBackend::Socket variant; return a
    // tiny adapter dict instead. Most user code calls .read()/.write()
    // on the socket directly via this helper.
    let _ = state_of(args)?;
    let self_obj = args
        .first()
        .cloned()
        .ok_or_else(|| type_error("missing self"))?;
    let dict = Rc::new(RefCell::new(DictData::new()));
    let self_for_read = self_obj.clone();
    let read = move |a: &[Object]| -> Result<Object, RuntimeError> {
        let n = match a.first() {
            Some(Object::Int(n)) => *n as usize,
            _ => 4096,
        };
        sock_recv(&[self_for_read.clone(), Object::Int(n as i64)])
    };
    let self_for_write = self_obj.clone();
    let write = move |a: &[Object]| -> Result<Object, RuntimeError> {
        let data = a.first().cloned().unwrap_or(Object::None);
        sock_sendall(&[self_for_write.clone(), data])
    };
    let self_for_close = self_obj;
    let close = move |_a: &[Object]| -> Result<Object, RuntimeError> {
        sock_close(std::slice::from_ref(&self_for_close))
    };
    {
        let mut d = dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("read")),
            Object::Builtin(Rc::new(BuiltinFn {
                name: "read",
                binds_instance: false,
                call: Box::new(read),
                call_kw: None,
            })),
        );
        d.insert(
            DictKey(Object::from_static("write")),
            Object::Builtin(Rc::new(BuiltinFn {
                name: "write",
                binds_instance: false,
                call: Box::new(write),
                call_kw: None,
            })),
        );
        d.insert(
            DictKey(Object::from_static("close")),
            Object::Builtin(Rc::new(BuiltinFn {
                name: "close",
                binds_instance: false,
                call: Box::new(close),
                call_kw: None,
            })),
        );
    }
    Ok(Object::Dict(dict))
}

fn sock_family_attr(args: &[Object]) -> Result<Object, RuntimeError> {
    let state = state_of(args)?;
    let v = state.borrow().family;
    Ok(Object::Int(i64::from(v)))
}

fn sock_type_attr(args: &[Object]) -> Result<Object, RuntimeError> {
    let state = state_of(args)?;
    let v = state.borrow().kind;
    Ok(Object::Int(i64::from(v)))
}

fn sock_proto_attr(args: &[Object]) -> Result<Object, RuntimeError> {
    let state = state_of(args)?;
    let v = state.borrow().proto;
    Ok(Object::Int(i64::from(v)))
}

/// Read one of the `family`/`type`/`proto` ints from the instance dict
/// (where `sock_init` stashed them). Used by the class-level getset
/// descriptors below: reading the dict keeps the value available after
/// `close()` (CPython keeps `family`/`type`/`proto` on a closed socket)
/// and avoids touching the live `SocketState`.
fn sock_dict_int(args: &[Object], key: &'static str) -> Result<Object, RuntimeError> {
    let inst = extract_self(args)?;
    let v = inst
        .dict
        .borrow()
        .get(&DictKey(Object::from_static(key)))
        .cloned();
    Ok(v.unwrap_or(Object::Int(-1)))
}

fn sock_family_prop(args: &[Object]) -> Result<Object, RuntimeError> {
    sock_dict_int(args, "family")
}

fn sock_type_prop(args: &[Object]) -> Result<Object, RuntimeError> {
    sock_dict_int(args, "type")
}

fn sock_proto_prop(args: &[Object]) -> Result<Object, RuntimeError> {
    sock_dict_int(args, "proto")
}

fn sock_timeout_prop(args: &[Object]) -> Result<Object, RuntimeError> {
    // Mirror `gettimeout()`: float seconds, or None for blocking mode.
    match state_of(args) {
        Ok(state) => match state.borrow().timeout {
            Some(d) => Ok(Object::Float(d.as_secs_f64())),
            None => Ok(Object::None),
        },
        Err(_) => Ok(Object::None),
    }
}

/// Install `family`/`type`/`proto`/`timeout` as class-level getset
/// descriptors. CPython exposes these as getset descriptors on
/// `socket.socket`, so they appear in `dir(socket.socket)` — which is
/// what `unittest.mock.Mock(spec=socket.socket)` builds its attribute
/// allow-list from. Without them, mocked sockets reject `sock.family`
/// (breaking large swaths of `test_asyncio`'s transport tests).
fn install_socket_getset(cls: &Rc<TypeObject>) {
    let props: [(&'static str, fn(&[Object]) -> Result<Object, RuntimeError>); 4] = [
        ("family", sock_family_prop),
        ("type", sock_type_prop),
        ("proto", sock_proto_prop),
        ("timeout", sock_timeout_prop),
    ];
    for (name, getter) in props {
        let prop = Object::Property(Rc::new(crate::object::PyProperty::new(
            Object::Builtin(Rc::new(BuiltinFn {
                name,
                binds_instance: true,
                call: Box::new(getter),
                call_kw: None,
            })),
            Object::None,
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
}

// ---- helpers ----

fn parse_socket_address(arg: Option<&Object>, family: i32) -> Result<SocketAddr, RuntimeError> {
    let tup = match arg {
        Some(Object::Tuple(t)) => t,
        Some(Object::List(l)) => {
            let borrowed = l.borrow();
            return parse_socket_address(
                Some(&Object::new_tuple(borrowed.iter().cloned().collect())),
                family,
            );
        }
        _ => return Err(type_error("address must be a tuple")),
    };
    let host = match tup.first() {
        Some(Object::Str(s)) => s.to_string(),
        _ => return Err(type_error("address[0] must be str")),
    };
    let port = match tup.get(1) {
        Some(Object::Int(n)) => *n as u16,
        _ => return Err(type_error("address[1] must be int")),
    };
    let host_for_lookup = if host.is_empty() {
        if family == libc_af_inet6() as i32 {
            "::"
        } else {
            "0.0.0.0"
        }
    } else {
        host.as_str()
    };
    let candidates: Vec<SocketAddr> = format!("{host_for_lookup}:{port}")
        .to_socket_addrs()
        .map_err(|e| io_error_to_py(&e))?
        .collect();
    // Respect the socket's address family. A name like "localhost" resolves
    // to *both* ::1 and 127.0.0.1; binding/connecting an AF_INET socket to an
    // IPv6 sockaddr (or vice-versa) fails with EAFNOSUPPORT, so pick a
    // candidate matching the socket family, falling back to the first.
    let parsed = if family == libc_af_inet6() as i32 {
        candidates.iter().find(|a| a.is_ipv6()).copied()
    } else if family == libc_af_inet() as i32 {
        candidates.iter().find(|a| a.is_ipv4()).copied()
    } else {
        None
    }
    .or_else(|| candidates.first().copied())
    .ok_or_else(|| os_error("could not resolve address"))?;
    Ok(parsed)
}

/// Build a `socket2::SockAddr` for an `AF_UNIX` path. Handles both
/// pathname sockets (NUL-terminated on the wire) and Linux abstract-namespace
/// sockets (a leading NUL, length-delimited, no terminator). `multiprocessing`
/// (Manager/forkserver) and `socketserver`'s `UnixStreamServer` bind such
/// addresses on POSIX, so `bind`/`connect` must accept a bare path here.
#[cfg(unix)]
fn sockaddr_unix_from_bytes(path: &[u8]) -> Result<SockAddr, RuntimeError> {
    use std::mem;
    let mut storage: libc::sockaddr_storage = unsafe { mem::zeroed() };
    // SAFETY: `sockaddr_storage` is large enough to alias a `sockaddr_un`.
    let su = unsafe { &mut *(std::ptr::addr_of_mut!(storage).cast::<libc::sockaddr_un>()) };
    su.sun_family = libc::AF_UNIX as libc::sa_family_t;
    let cap = su.sun_path.len();
    let is_abstract = path.first() == Some(&0);
    // Pathname sockets reserve one byte for the trailing NUL.
    let max = if is_abstract {
        cap
    } else {
        cap.saturating_sub(1)
    };
    if path.len() > max {
        return Err(os_error("AF_UNIX path too long"));
    }
    for (i, &b) in path.iter().enumerate() {
        su.sun_path[i] = b as libc::c_char;
    }
    // `offsetof(sockaddr_un, sun_path)` portably: total size minus the
    // path array (2 on both Linux and macOS). Pathname sockets add the
    // terminator to the length they report to the kernel.
    let offset = mem::size_of::<libc::sockaddr_un>() - cap;
    let len = if is_abstract {
        offset + path.len()
    } else {
        offset + path.len() + 1
    };
    // SAFETY: `storage` holds a fully-initialised `sockaddr_un` of `len` bytes.
    Ok(unsafe { SockAddr::new(storage, len as libc::socklen_t) })
}

/// Extract the path from an `AF_UNIX` `SockAddr`, or `None` if it isn't one.
/// Pathname sockets are returned NUL-trimmed; abstract sockets keep their
/// leading NUL (CPython surfaces them the same way).
#[cfg(unix)]
fn sockaddr_unix_path(addr: &SockAddr) -> Option<String> {
    if addr.family() != libc::AF_UNIX as libc::sa_family_t {
        return None;
    }
    // SAFETY: family is AF_UNIX, so the storage is a `sockaddr_un`.
    let su: &libc::sockaddr_un = unsafe { &*(addr.as_ptr().cast::<libc::sockaddr_un>()) };
    let total = addr.len() as usize;
    let base = std::mem::size_of::<libc::sockaddr_un>() - su.sun_path.len();
    if total <= base {
        return Some(String::new()); // unnamed
    }
    let path_len = (total - base).min(su.sun_path.len());
    // SAFETY: `sun_path` holds at least `path_len` initialised bytes.
    let bytes: &[u8] =
        unsafe { std::slice::from_raw_parts(su.sun_path.as_ptr().cast::<u8>(), path_len) };
    // Linux abstract namespace: a leading NUL is significant and the rest is
    // *not* NUL-terminated, so surface the whole window. This convention does
    // not exist on the BSDs/macOS, where `sun_path` is an ordinary
    // NUL-terminated C string — a leading NUL there just means the empty
    // (unnamed) address, which CPython decodes to `""` (and which the kernel
    // hands back for an autobind/unnamed peer that `accept(2)` reports with a
    // zeroed, full-width `sun_path`).
    #[cfg(target_os = "linux")]
    if bytes.first() == Some(&0) {
        return Some(String::from_utf8_lossy(bytes).into_owned());
    }
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    Some(String::from_utf8_lossy(&bytes[..end]).into_owned())
}

/// Resolve a Python socket-address argument into a `socket2::SockAddr`,
/// dispatching on the socket's address family: an `AF_UNIX` socket takes a
/// `str`/`bytes` path, everything else the `(host, port[, …])` tuple parsed
/// by [`parse_socket_address`].
fn parse_sockaddr2(arg: Option<&Object>, family: i32) -> Result<SockAddr, RuntimeError> {
    #[cfg(unix)]
    if family == libc::AF_UNIX as i32 {
        let path: Vec<u8> = match arg {
            Some(Object::Str(s)) => s.as_bytes().to_vec(),
            Some(Object::Bytes(b)) => b.to_vec(),
            Some(Object::ByteArray(b)) => b.borrow().clone(),
            _ => return Err(type_error("AF_UNIX address must be a str or bytes path")),
        };
        return sockaddr_unix_from_bytes(&path);
    }
    let addr = parse_socket_address(arg, family)?;
    Ok(SockAddr::from(addr))
}

fn sockaddr_to_tuple(addr: &SockAddr, _family: i32) -> Object {
    if let Some(v4) = addr.as_socket_ipv4() {
        Object::new_tuple(vec![
            Object::from_str(v4.ip().to_string()),
            Object::Int(i64::from(v4.port())),
        ])
    } else if let Some(v6) = addr.as_socket_ipv6() {
        Object::new_tuple(vec![
            Object::from_str(v6.ip().to_string()),
            Object::Int(i64::from(v6.port())),
            Object::Int(i64::from(v6.flowinfo())),
            Object::Int(i64::from(v6.scope_id())),
        ])
    } else {
        // `AF_UNIX` (and the unnamed/empty case): CPython returns the path
        // as a plain `str`, not a `(host, port)` tuple.
        #[cfg(unix)]
        if let Some(path) = sockaddr_unix_path(addr) {
            return Object::from_str(path);
        }
        Object::new_tuple(vec![Object::from_static(""), Object::Int(0)])
    }
}

fn extract_bytes(arg: Option<&Object>) -> Result<Vec<u8>, RuntimeError> {
    match arg {
        Some(Object::Bytes(b)) => Ok(b.to_vec()),
        Some(Object::ByteArray(b)) => Ok(b.borrow().clone()),
        Some(Object::Str(s)) => Ok(s.as_bytes().to_vec()),
        // `memoryview` is a bytes-like object; asyncio's sendfile fallback
        // sends `view[:read]` slices. `to_bytes()` materialises the (possibly
        // sliced/strided) window.
        Some(Object::MemoryView(mv)) => {
            if mv.released.get() {
                return Err(value_error(
                    "operation forbidden on released memoryview object",
                ));
            }
            Ok(mv.to_bytes())
        }
        _ => Err(type_error("expected bytes-like object")),
    }
}

// ---- module-level functions ----

fn module_functions() -> &'static [(&'static str, fn(&[Object]) -> Result<Object, RuntimeError>)] {
    &[
        ("gethostname", mod_gethostname),
        ("gethostbyname", mod_gethostbyname),
        ("gethostbyaddr", mod_gethostbyaddr),
        ("getaddrinfo", mod_getaddrinfo),
        ("getnameinfo", mod_getnameinfo),
        ("getfqdn", mod_getfqdn),
        ("create_connection", mod_create_connection),
        ("create_server", mod_create_server),
        ("socketpair", mod_socketpair),
        ("inet_aton", mod_inet_aton),
        ("inet_ntoa", mod_inet_ntoa),
        ("inet_pton", mod_inet_pton),
        ("inet_ntop", mod_inet_ntop),
        ("htons", mod_htons),
        ("htonl", mod_htonl),
        ("ntohs", mod_htons),
        ("ntohl", mod_htonl),
        ("getdefaulttimeout", mod_getdefaulttimeout),
        ("setdefaulttimeout", mod_setdefaulttimeout),
        // Ancillary-data sizing helpers (functions, not constants, exactly
        // like CPython's `socket` module). Needed by `reduction.recvfds`.
        ("CMSG_LEN", mod_cmsg_len),
        ("CMSG_SPACE", mod_cmsg_space),
    ]
}

/// `socket.CMSG_LEN(length)` — bytes an ancillary-data item of `length`
/// payload occupies, including the `cmsghdr` (but not the trailing pad).
fn mod_cmsg_len(args: &[Object]) -> Result<Object, RuntimeError> {
    let length = cmsg_size_arg(args.first())?;
    #[cfg(unix)]
    {
        Ok(Object::Int(unsafe { libc::CMSG_LEN(length) } as i64))
    }
    #[cfg(not(unix))]
    {
        Err(os_error("CMSG_LEN is not supported on this platform"))
    }
}

/// `socket.CMSG_SPACE(length)` — bytes to allocate in a control buffer for
/// one ancillary-data item of `length` payload, including alignment pad.
fn mod_cmsg_space(args: &[Object]) -> Result<Object, RuntimeError> {
    let length = cmsg_size_arg(args.first())?;
    #[cfg(unix)]
    {
        Ok(Object::Int(unsafe { libc::CMSG_SPACE(length) } as i64))
    }
    #[cfg(not(unix))]
    {
        Err(os_error("CMSG_SPACE is not supported on this platform"))
    }
}

#[allow(dead_code)]
fn cmsg_size_arg(arg: Option<&Object>) -> Result<u32, RuntimeError> {
    match arg {
        Some(Object::Int(n)) if *n >= 0 => Ok(*n as u32),
        Some(Object::Int(_)) => Err(value_error("CMSG_LEN() argument out of range")),
        _ => Err(type_error("an integer is required")),
    }
}

fn mod_gethostname(_args: &[Object]) -> Result<Object, RuntimeError> {
    let name = std::env::var("HOSTNAME")
        .or_else(|_| std::env::var("COMPUTERNAME"))
        .unwrap_or_else(|_| "localhost".to_string());
    Ok(Object::from_str(name))
}

fn mod_gethostbyname(args: &[Object]) -> Result<Object, RuntimeError> {
    let name = match args.first() {
        Some(Object::Str(s)) => s.to_string(),
        _ => return Err(type_error("gethostbyname: arg must be str")),
    };
    let mut addrs = (name.as_str(), 0_u16)
        .to_socket_addrs()
        .map_err(|e| io_error_to_py(&e))?;
    if let Some(addr) = addrs.next() {
        Ok(Object::from_str(addr.ip().to_string()))
    } else {
        Err(os_error("name resolution failed"))
    }
}

fn mod_gethostbyaddr(args: &[Object]) -> Result<Object, RuntimeError> {
    let addr = match args.first() {
        Some(Object::Str(s)) => s.to_string(),
        _ => return Err(type_error("gethostbyaddr: arg must be str")),
    };
    Ok(Object::new_tuple(vec![
        Object::from_str(addr.clone()),
        Object::new_list(Vec::new()),
        Object::new_list(vec![Object::from_str(addr)]),
    ]))
}

fn mod_getfqdn(args: &[Object]) -> Result<Object, RuntimeError> {
    if let Some(Object::Str(s)) = args.first() {
        if !s.is_empty() {
            return Ok(Object::from_str(s.to_string()));
        }
    }
    mod_gethostname(args)
}

fn mod_getaddrinfo(args: &[Object]) -> Result<Object, RuntimeError> {
    let host = match args.first() {
        Some(Object::Str(s)) => s.to_string(),
        Some(Object::None) | None => "0.0.0.0".to_string(),
        _ => return Err(type_error("getaddrinfo: host must be str or None")),
    };
    let port = match args.get(1) {
        Some(Object::Int(n)) => *n as u16,
        Some(Object::Str(s)) => s.parse::<u16>().unwrap_or(0),
        Some(Object::None) | None => 0,
        _ => return Err(type_error("getaddrinfo: port must be int, str, or None")),
    };
    let family_req = match args.get(2) {
        Some(Object::Int(n)) => *n as i32,
        _ => 0,
    };
    let kind = match args.get(3) {
        Some(Object::Int(n)) => *n as i32,
        _ => libc_sock_stream() as i32,
    };
    let proto = match args.get(4) {
        Some(Object::Int(n)) => *n as i32,
        _ => 0,
    };

    let resolved = (host.as_str(), port)
        .to_socket_addrs()
        .map_err(|e| io_error_to_py(&e))?;

    let mut out = Vec::new();
    for sa in resolved {
        let fam = match sa {
            SocketAddr::V4(_) => libc_af_inet() as i32,
            SocketAddr::V6(_) => libc_af_inet6() as i32,
        };
        if family_req != 0 && family_req != fam {
            continue;
        }
        let addr_tuple = match sa {
            SocketAddr::V4(v4) => Object::new_tuple(vec![
                Object::from_str(v4.ip().to_string()),
                Object::Int(i64::from(v4.port())),
            ]),
            SocketAddr::V6(v6) => Object::new_tuple(vec![
                Object::from_str(v6.ip().to_string()),
                Object::Int(i64::from(v6.port())),
                Object::Int(i64::from(v6.flowinfo())),
                Object::Int(i64::from(v6.scope_id())),
            ]),
        };
        out.push(Object::new_tuple(vec![
            Object::Int(i64::from(fam)),
            Object::Int(i64::from(kind)),
            Object::Int(i64::from(proto)),
            Object::from_static(""),
            addr_tuple,
        ]));
    }
    Ok(Object::new_list(out))
}

fn mod_getnameinfo(args: &[Object]) -> Result<Object, RuntimeError> {
    let addr_obj = match args.first() {
        Some(o) => o,
        None => return Err(type_error("getnameinfo: missing argument")),
    };
    let tup = match addr_obj {
        Object::Tuple(t) => t,
        _ => return Err(type_error("getnameinfo: address must be tuple")),
    };
    let host = match tup.first() {
        Some(Object::Str(s)) => s.to_string(),
        _ => return Err(type_error("getnameinfo: address[0] must be str")),
    };
    let port = match tup.get(1) {
        Some(Object::Int(n)) => *n as u16,
        _ => return Err(type_error("getnameinfo: address[1] must be int")),
    };
    Ok(Object::new_tuple(vec![
        Object::from_str(host),
        Object::from_str(port.to_string()),
    ]))
}

fn mod_create_connection(args: &[Object]) -> Result<Object, RuntimeError> {
    // create_connection(address, timeout=...) returns a connected
    // socket.socket. We build one via socket_class().
    let addr_obj = args
        .first()
        .ok_or_else(|| type_error("create_connection: missing address"))?
        .clone();
    let cls = socket_class();
    let inst = Rc::new(PyInstance::new(cls));
    let inst_obj = Object::Instance(inst.clone());
    sock_init(&[
        inst_obj.clone(),
        Object::Int(libc_af_inet()),
        Object::Int(libc_sock_stream()),
        Object::Int(0),
    ])?;
    if let Some(timeout) = args.get(1).cloned() {
        sock_settimeout(&[inst_obj.clone(), timeout])?;
    }
    sock_connect(&[inst_obj.clone(), addr_obj])?;
    Ok(inst_obj)
}

fn mod_create_server(args: &[Object]) -> Result<Object, RuntimeError> {
    let addr_obj = args
        .first()
        .ok_or_else(|| type_error("create_server: missing address"))?
        .clone();
    let family = match args.get(1) {
        Some(Object::Int(n)) => *n as i32,
        _ => libc_af_inet() as i32,
    };
    let backlog = match args.get(2) {
        Some(Object::Int(n)) => *n,
        _ => 100,
    };
    let reuse_port = match args.get(3) {
        Some(Object::Bool(b)) => *b,
        _ => false,
    };
    let cls = socket_class();
    let inst = Rc::new(PyInstance::new(cls));
    let inst_obj = Object::Instance(inst);
    sock_init(&[
        inst_obj.clone(),
        Object::Int(i64::from(family)),
        Object::Int(libc_sock_stream()),
        Object::Int(0),
    ])?;
    sock_setsockopt(&[
        inst_obj.clone(),
        Object::Int(libc_sol_socket()),
        Object::Int(libc_so_reuseaddr()),
        Object::Int(1),
    ])?;
    if reuse_port {
        sock_setsockopt(&[
            inst_obj.clone(),
            Object::Int(libc_sol_socket()),
            Object::Int(libc_so_reuseport()),
            Object::Int(1),
        ])?;
    }
    sock_bind(&[inst_obj.clone(), addr_obj])?;
    sock_listen(&[inst_obj.clone(), Object::Int(backlog)])?;
    Ok(inst_obj)
}

fn mod_socketpair(args: &[Object]) -> Result<Object, RuntimeError> {
    // CPython signature: socketpair(family=AF_UNIX, type=SOCK_STREAM, proto=0).
    // The AF_UNIX default is load-bearing: `multiprocessing`'s `Connection`
    // pipes and the `forkserver` control channel both rely on a real
    // `socketpair(2)` over which `SCM_RIGHTS` fd passing works (a TCP
    // loopback pair cannot carry ancillary data).
    let family = match args.first() {
        None | Some(Object::None) => default_socketpair_family(),
        Some(Object::Int(n)) => *n as i32,
        _ => return Err(type_error("socketpair: family must be an integer")),
    };
    let sock_type = match args.get(1) {
        None | Some(Object::None) => libc_sock_stream() as i32,
        Some(Object::Int(n)) => *n as i32,
        _ => return Err(type_error("socketpair: type must be an integer")),
    };
    let proto = match args.get(2) {
        None | Some(Object::None) => 0,
        Some(Object::Int(n)) => *n as i32,
        _ => return Err(type_error("socketpair: proto must be an integer")),
    };

    #[cfg(unix)]
    if family == 1 {
        return unix_socketpair(family, sock_type, proto);
    }

    inet_socketpair_emulation()
}

/// Default `socketpair` family — AF_UNIX on POSIX (CPython parity), AF_INET
/// where AF_UNIX is unavailable.
fn default_socketpair_family() -> i32 {
    #[cfg(unix)]
    {
        1 // AF_UNIX
    }
    #[cfg(not(unix))]
    {
        libc_af_inet() as i32
    }
}

/// A genuine `socketpair(2)` — the connected AF_UNIX pair that carries
/// `SCM_RIGHTS` ancillary data for fd passing.
#[cfg(unix)]
fn unix_socketpair(family: i32, sock_type: i32, proto: i32) -> Result<Object, RuntimeError> {
    use std::os::unix::io::FromRawFd;
    let mut fds = [0 as libc::c_int; 2];
    let rc = unsafe { libc::socketpair(family, sock_type, proto, fds.as_mut_ptr()) };
    if rc != 0 {
        return Err(io_error_to_py(&std::io::Error::last_os_error()));
    }
    let make = |fd: libc::c_int| -> Object {
        // SAFETY: `fd` is a fresh, owned descriptor from `socketpair(2)`.
        let sock = unsafe { Socket::from_raw_fd(fd) };
        let state = Rc::new(RefCell::new(SocketState {
            inner: Some(sock),
            family,
            kind: sock_type,
            proto,
            timeout: None,
            blocking: true,
        }));
        let h = next_handle(state);
        let inst = Rc::new(PyInstance::new(socket_class()));
        inst.dict
            .borrow_mut()
            .insert(DictKey(Object::from_static("_handle")), Object::Int(h));
        Object::Instance(inst)
    };
    Ok(Object::new_tuple(vec![make(fds[0]), make(fds[1])]))
}

/// Loopback-TCP emulation for the AF_INET case (and platforms without
/// `AF_UNIX`). Builds a connected pair via a transient listener.
fn inet_socketpair_emulation() -> Result<Object, RuntimeError> {
    use socket2::{Domain, Socket, Type};
    let listener = Socket::new(Domain::IPV4, Type::STREAM, None).map_err(|e| io_error_to_py(&e))?;
    listener
        .bind(&SockAddr::from(SocketAddr::new(
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            0,
        )))
        .map_err(|e| io_error_to_py(&e))?;
    listener.listen(1).map_err(|e| io_error_to_py(&e))?;
    let addr = listener.local_addr().map_err(|e| io_error_to_py(&e))?;
    let client = Socket::new(Domain::IPV4, Type::STREAM, None).map_err(|e| io_error_to_py(&e))?;
    client.connect(&addr).map_err(|e| io_error_to_py(&e))?;
    let (server, _) = listener.accept().map_err(|e| io_error_to_py(&e))?;

    let make_inst = |sock: Socket| -> Object {
        let state = Rc::new(RefCell::new(SocketState {
            inner: Some(sock),
            family: libc_af_inet() as i32,
            kind: libc_sock_stream() as i32,
            proto: 0,
            timeout: None,
            blocking: true,
        }));
        let h = next_handle(state);
        let cls = socket_class();
        let inst = Rc::new(PyInstance::new(cls));
        inst.dict
            .borrow_mut()
            .insert(DictKey(Object::from_static("_handle")), Object::Int(h));
        Object::Instance(inst)
    };
    Ok(Object::new_tuple(vec![
        make_inst(client),
        make_inst(server),
    ]))
}

fn mod_inet_aton(args: &[Object]) -> Result<Object, RuntimeError> {
    let s = match args.first() {
        Some(Object::Str(s)) => s.to_string(),
        _ => return Err(type_error("inet_aton: arg must be str")),
    };
    let ip: Ipv4Addr = s
        .parse()
        .map_err(|_| os_error("illegal IP address string passed to inet_aton"))?;
    Ok(Object::new_bytes(ip.octets().to_vec()))
}

fn mod_inet_ntoa(args: &[Object]) -> Result<Object, RuntimeError> {
    let bytes = match args.first() {
        Some(Object::Bytes(b)) => b.to_vec(),
        Some(Object::ByteArray(b)) => b.borrow().clone(),
        _ => return Err(type_error("inet_ntoa: expects bytes-like")),
    };
    if bytes.len() != 4 {
        return Err(os_error("packed IP wrong length"));
    }
    Ok(Object::from_str(
        Ipv4Addr::new(bytes[0], bytes[1], bytes[2], bytes[3]).to_string(),
    ))
}

fn mod_inet_pton(args: &[Object]) -> Result<Object, RuntimeError> {
    let family = match args.first() {
        Some(Object::Int(n)) => *n as i32,
        _ => return Err(type_error("inet_pton: family must be int")),
    };
    let s = match args.get(1) {
        Some(Object::Str(s)) => s.to_string(),
        _ => return Err(type_error("inet_pton: addr must be str")),
    };
    // CPython's `inet_pton` reports a malformed address with `OSError`
    // ("illegal IP address string passed to inet_pton"), *not* `ValueError`.
    // asyncio's `_ipaddr_info` relies on this: it calls `inet_pton` to test
    // whether a host is already a literal IP and treats `OSError` as "needs
    // DNS resolution", so raising the wrong type breaks `sock_connect`.
    if family == libc_af_inet() as i32 {
        let ip: Ipv4Addr = s
            .parse()
            .map_err(|_| os_error("illegal IP address string passed to inet_pton"))?;
        Ok(Object::new_bytes(ip.octets().to_vec()))
    } else if family == libc_af_inet6() as i32 {
        let ip: Ipv6Addr = s
            .parse()
            .map_err(|_| os_error("illegal IP address string passed to inet_pton"))?;
        Ok(Object::new_bytes(ip.octets().to_vec()))
    } else {
        Err(os_error("inet_pton: unsupported family"))
    }
}

fn mod_inet_ntop(args: &[Object]) -> Result<Object, RuntimeError> {
    let family = match args.first() {
        Some(Object::Int(n)) => *n as i32,
        _ => return Err(type_error("inet_ntop: family must be int")),
    };
    let bytes = match args.get(1) {
        Some(Object::Bytes(b)) => b.to_vec(),
        Some(Object::ByteArray(b)) => b.borrow().clone(),
        _ => return Err(type_error("inet_ntop: addr must be bytes")),
    };
    if family == libc_af_inet() as i32 && bytes.len() == 4 {
        Ok(Object::from_str(
            Ipv4Addr::new(bytes[0], bytes[1], bytes[2], bytes[3]).to_string(),
        ))
    } else if family == libc_af_inet6() as i32 && bytes.len() == 16 {
        let mut octets = [0u8; 16];
        octets.copy_from_slice(&bytes);
        Ok(Object::from_str(Ipv6Addr::from(octets).to_string()))
    } else {
        Err(os_error("inet_ntop: bad address length"))
    }
}

fn mod_htons(args: &[Object]) -> Result<Object, RuntimeError> {
    match args.first() {
        Some(Object::Int(n)) => Ok(Object::Int(i64::from((*n as u16).to_be()))),
        _ => Err(type_error("htons: arg must be int")),
    }
}

fn mod_htonl(args: &[Object]) -> Result<Object, RuntimeError> {
    match args.first() {
        Some(Object::Int(n)) => Ok(Object::Int(i64::from((*n as u32).to_be()))),
        _ => Err(type_error("htonl: arg must be int")),
    }
}

// Process-global, matching CPython: `socket.setdefaulttimeout()` affects
// every thread's newly created sockets, not just the calling thread's.
fn default_timeout() -> &'static parking_lot::Mutex<Option<f64>> {
    static DEFAULT_TIMEOUT: std::sync::OnceLock<parking_lot::Mutex<Option<f64>>> =
        std::sync::OnceLock::new();
    DEFAULT_TIMEOUT.get_or_init(|| parking_lot::Mutex::new(None))
}

fn mod_getdefaulttimeout(_args: &[Object]) -> Result<Object, RuntimeError> {
    match *default_timeout().lock() {
        None => Ok(Object::None),
        Some(f) => Ok(Object::Float(f)),
    }
}

fn mod_setdefaulttimeout(args: &[Object]) -> Result<Object, RuntimeError> {
    let value = match args.first() {
        None | Some(Object::None) => None,
        Some(Object::Float(f)) => Some(*f),
        Some(Object::Int(n)) => Some(*n as f64),
        _ => return Err(type_error("setdefaulttimeout: arg must be float or None")),
    };
    *default_timeout().lock() = value;
    Ok(Object::None)
}

// ---- platform-aware constants ----

#[allow(clippy::unnecessary_wraps)]
fn libc_af_inet() -> i64 {
    2
}

#[cfg(unix)]
fn libc_af_inet6() -> i64 {
    30
}

#[cfg(not(unix))]
fn libc_af_inet6() -> i64 {
    23
}

fn libc_sock_stream() -> i64 {
    1
}

fn libc_sock_dgram() -> i64 {
    2
}

#[cfg(target_os = "macos")]
fn libc_sol_socket() -> i64 {
    0xFFFF
}

#[cfg(not(target_os = "macos"))]
fn libc_sol_socket() -> i64 {
    1
}

#[cfg(target_os = "macos")]
fn libc_so_reuseaddr() -> i64 {
    0x0004
}

#[cfg(not(target_os = "macos"))]
fn libc_so_reuseaddr() -> i64 {
    2
}

#[cfg(target_os = "macos")]
fn libc_so_reuseport() -> i64 {
    0x0200
}

#[cfg(not(target_os = "macos"))]
fn libc_so_reuseport() -> i64 {
    15
}

#[cfg(target_os = "macos")]
fn libc_so_keepalive() -> i64 {
    0x0008
}

#[cfg(not(target_os = "macos"))]
fn libc_so_keepalive() -> i64 {
    9
}

#[cfg(target_os = "macos")]
fn libc_so_broadcast() -> i64 {
    0x0020
}

#[cfg(not(target_os = "macos"))]
fn libc_so_broadcast() -> i64 {
    6
}

#[cfg(target_os = "macos")]
fn libc_so_linger() -> i64 {
    0x1080
}

#[cfg(not(target_os = "macos"))]
fn libc_so_linger() -> i64 {
    13
}

#[cfg(target_os = "macos")]
fn libc_so_sndbuf() -> i64 {
    0x1001
}

#[cfg(not(target_os = "macos"))]
fn libc_so_sndbuf() -> i64 {
    7
}

#[cfg(target_os = "macos")]
fn libc_so_rcvbuf() -> i64 {
    0x1002
}

#[cfg(not(target_os = "macos"))]
fn libc_so_rcvbuf() -> i64 {
    8
}

// Silence "unused import" warnings for items only referenced under
// platform `cfg` arms.
#[allow(dead_code)]
fn _avoid_unused() {
    let _ = blocking_io_error("");
    let _: Option<IpAddr> = None;
    fn _r<T: Read>(_t: &mut T) {}
    fn _w<T: Write>(_t: &mut T) {}
}

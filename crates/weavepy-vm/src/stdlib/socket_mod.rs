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
// Only the unix-gated interface/name lookups use `CStr` directly.
#[cfg(unix)]
use std::ffi::CStr;
use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, Shutdown, SocketAddr, ToSocketAddrs};
use std::time::Duration;

use socket2::{Domain, Protocol, SockAddr, Socket, Type};

use crate::error::{
    blocking_io_error, io_error_to_py, os_error, os_error_with_errno, overflow_error,
    timeout_error, type_error, value_error, RuntimeError,
};

/// `OSError([Errno 9] Bad file descriptor)` for operations on a closed
/// socket. Carrying `EBADF` (rather than a bare message) lets callers that
/// branch on `errno` — `asyncore`'s `_DISCONNECTED` set, `selectors`,
/// `ssl` teardown — treat it as a graceful disconnect, matching CPython.
/// `py_errno::EBADF` rather than `libc::EBADF`: identical on POSIX, and on
/// Windows it's the CRT value 9 that CPython raises for a closed Python
/// socket on every platform (`sock_fd` is -1 → `EBADF`, socketmodule.c).
fn closed_socket_error() -> RuntimeError {
    os_error_with_errno(crate::py_errno::EBADF, "Bad file descriptor")
}
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
    /// Whether this state owns the underlying OS descriptor. `false` for an
    /// *alias* created by `socket(fileno=other.fileno())` over a descriptor
    /// another live socket already owns: such a state must never close the
    /// fd, or the real owner's later close becomes a double close.
    owns_fd: bool,
}

impl Drop for SocketState {
    fn drop(&mut self) {
        if let Some(sock) = self.inner.take() {
            if self.owns_fd {
                // Errors are meaningless during finalization; only an
                // explicit .close() reports them.
                let _ = close_owned_socket(sock);
            } else {
                release_socket(sock);
            }
        }
    }
}

/// Close the descriptor backing `sock` the way CPython's `close()` does: a
/// raw `close(2)` that tolerates `EBADF` rather than aborting the process.
///
/// socket2's `OwnedFd` destructor escalates a double close into a hard
/// process abort ("IO Safety violation: owned file descriptor already
/// closed"). That is wrong for Python semantics, where
/// `socket(fileno=other.fileno())` aliases a descriptor and either object
/// may legitimately close it first. We therefore extract the raw fd
/// (`into_raw_fd`, which does *not* close) and issue the syscall ourselves,
/// swallowing the error.
#[cfg(unix)]
fn close_owned_socket(sock: Socket) -> Result<(), RuntimeError> {
    let fd = into_raw_fd_of(sock);
    if fd >= 0 {
        let r = unsafe { libc::close(fd as libc::c_int) };
        if r < 0 {
            let err = std::io::Error::last_os_error();
            // CPython's sock_close swallows only ECONNRESET.
            if err.raw_os_error() != Some(libc::ECONNRESET) {
                return Err(io_error_to_py(&err));
            }
        }
    }
    Ok(())
}

#[cfg(not(unix))]
fn close_owned_socket(sock: Socket) -> Result<(), RuntimeError> {
    drop(sock);
    Ok(())
}

/// Drop a non-owning alias `Socket` *without* closing its descriptor — the
/// real owner keeps it open. `into_raw_fd` releases the fd back to the
/// caller-of-record (a no-op here) instead of running the closing
/// destructor.
fn release_socket(sock: Socket) {
    let _ = into_raw_fd_of(sock);
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
    if let Some(old) = registry().lock().insert(handle, state) {
        // A state still registered under this fd number is stale: the kernel
        // just handed *us* the descriptor, so the old state's fd was already
        // closed (its owner is somewhere between `close(2)` and its registry
        // remove). Release the stale `Socket` without closing — letting it
        // drop with `owns_fd` would issue `close(fd)` and destroy the brand
        // new socket that now legitimately owns the number.
        let taken = old.borrow_mut().inner.take();
        if let Some(sock) = taken {
            release_socket(sock);
        }
    }
    handle
}

/// Register an *alias* state under a synthetic handle that can never collide
/// with a real fd (or with `next_handle`'s small negative synthetic ids), so
/// inserting it does not evict — and thereby prematurely close — the real
/// owner keyed by its fd. `fileno()` still reports the true OS fd because it
/// reads it from `inner`, not from the handle.
fn insert_alias_handle(state: Rc<RefCell<SocketState>>) -> i64 {
    use std::sync::atomic::{AtomicI64, Ordering};
    static NEXT_ALIAS: AtomicI64 = AtomicI64::new(0);
    let handle = -(1_000_000_000 + NEXT_ALIAS.fetch_add(1, Ordering::Relaxed));
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
    // CPython performs WSAStartup once when `_socket` is imported
    // (PyInit__socket, socketmodule.c) — not lazily on first use — so any
    // Winsock call made right after import (getaddrinfo, select on a
    // pre-existing SOCKET, …) finds the stack initialized. Mirror that.
    #[cfg(windows)]
    winsock::ensure_started();
    let dict = Rc::new(RefCell::new(DictData::default()));
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
        // SOCK_RDM/SOCK_SEQPACKET share their numbering across unix
        // platforms (test_socket's testCrucialConstants touches both).
        d.insert(DictKey(Object::from_static("SOCK_RDM")), Object::Int(4));
        d.insert(
            DictKey(Object::from_static("SOCK_SEQPACKET")),
            Object::Int(5),
        );
        // SOCK_NONBLOCK/SOCK_CLOEXEC are Linux-only `socket(2)` type flags;
        // CPython gates them on the platform headers. Exporting them on
        // macOS makes `socket(AF_INET, SOCK_STREAM | SOCK_NONBLOCK)` reach
        // the kernel verbatim, which rejects the unknown type bits with
        // EPROTONOSUPPORT (test_base_events.test_create_server_stream_bittype
        // gates on `hasattr(socket, 'SOCK_NONBLOCK')`).
        #[cfg(target_os = "linux")]
        {
            d.insert(
                DictKey(Object::from_static("SOCK_NONBLOCK")),
                Object::Int(i64::from(libc::SOCK_NONBLOCK)),
            );
            d.insert(
                DictKey(Object::from_static("SOCK_CLOEXEC")),
                Object::Int(i64::from(libc::SOCK_CLOEXEC)),
            );
        }

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
        // RFC 3542 advanced-API options (test_socket's test3542SocketOptions
        // asserts every one of these exists). The numbering below is the
        // Darwin <netinet6/in6.h> set; Linux uses different values for most.
        #[cfg(any(target_os = "macos", target_os = "ios"))]
        for (name, val) in [
            ("IPV6_CHECKSUM", 26),
            ("IPV6_DONTFRAG", 62),
            ("IPV6_DSTOPTS", 50),
            ("IPV6_HOPLIMIT", 47),
            ("IPV6_HOPOPTS", 49),
            ("IPV6_NEXTHOP", 48),
            ("IPV6_PATHMTU", 44),
            ("IPV6_PKTINFO", 46),
            ("IPV6_RECVDSTOPTS", 40),
            ("IPV6_RECVHOPLIMIT", 37),
            ("IPV6_RECVHOPOPTS", 39),
            ("IPV6_RECVPATHMTU", 43),
            ("IPV6_RECVPKTINFO", 61),
            ("IPV6_RECVRTHDR", 38),
            ("IPV6_RECVTCLASS", 35),
            ("IPV6_RTHDR", 51),
            ("IPV6_RTHDRDSTOPTS", 57),
            ("IPV6_RTHDR_TYPE_0", 0),
            ("IPV6_TCLASS", 36),
            ("IPV6_USE_MIN_MTU", 42),
            ("IPV6_JOIN_GROUP", 12),
            ("IPV6_LEAVE_GROUP", 13),
            ("IPV6_MULTICAST_HOPS", 10),
            ("IPV6_MULTICAST_IF", 9),
            ("IPV6_MULTICAST_LOOP", 11),
            ("IPV6_UNICAST_HOPS", 4),
        ] {
            d.insert(DictKey(Object::from_static(name)), Object::Int(val));
        }
        #[cfg(target_os = "linux")]
        for (name, val) in [
            ("IPV6_CHECKSUM", 7),
            ("IPV6_DONTFRAG", 62),
            ("IPV6_DSTOPTS", 59),
            ("IPV6_HOPLIMIT", 52),
            ("IPV6_HOPOPTS", 54),
            ("IPV6_NEXTHOP", 9),
            ("IPV6_PATHMTU", 61),
            ("IPV6_PKTINFO", 50),
            ("IPV6_RECVDSTOPTS", 58),
            ("IPV6_RECVHOPLIMIT", 51),
            ("IPV6_RECVHOPOPTS", 53),
            ("IPV6_RECVPATHMTU", 60),
            ("IPV6_RECVPKTINFO", 49),
            ("IPV6_RECVRTHDR", 56),
            ("IPV6_RECVTCLASS", 66),
            ("IPV6_RTHDR", 57),
            ("IPV6_RTHDRDSTOPTS", 55),
            ("IPV6_RTHDR_TYPE_0", 0),
            ("IPV6_TCLASS", 67),
            ("IPV6_USE_MIN_MTU", 63),
            ("IPV6_JOIN_GROUP", 20),
            ("IPV6_LEAVE_GROUP", 21),
            ("IPV6_MULTICAST_HOPS", 18),
            ("IPV6_MULTICAST_IF", 17),
            ("IPV6_MULTICAST_LOOP", 19),
            ("IPV6_UNICAST_HOPS", 16),
        ] {
            d.insert(DictKey(Object::from_static(name)), Object::Int(val));
        }

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
        // These five must be the platform's *real* numbering: the old
        // Linux-shaped virtualization collided on macOS, where SO_REUSEADDR
        // is 4 — the same number the fake SO_ERROR used — so
        // `getsockopt(SOL_SOCKET, SO_REUSEADDR)` read the wrong option
        // (testSetSockOpt's set/get round-trip caught it).
        #[cfg(unix)]
        {
            d.insert(
                DictKey(Object::from_static("SO_OOBINLINE")),
                Object::Int(i64::from(libc::SO_OOBINLINE)),
            );
            d.insert(
                DictKey(Object::from_static("SO_SNDTIMEO")),
                Object::Int(i64::from(libc::SO_SNDTIMEO)),
            );
            d.insert(
                DictKey(Object::from_static("SO_RCVTIMEO")),
                Object::Int(i64::from(libc::SO_RCVTIMEO)),
            );
            d.insert(
                DictKey(Object::from_static("SO_ERROR")),
                Object::Int(i64::from(libc::SO_ERROR)),
            );
            d.insert(
                DictKey(Object::from_static("SO_TYPE")),
                Object::Int(i64::from(libc::SO_TYPE)),
            );
        }
        #[cfg(not(unix))]
        {
            d.insert(
                DictKey(Object::from_static("SO_OOBINLINE")),
                Object::Int(10),
            );
            d.insert(DictKey(Object::from_static("SO_SNDTIMEO")), Object::Int(21));
            d.insert(DictKey(Object::from_static("SO_RCVTIMEO")), Object::Int(20));
            d.insert(DictKey(Object::from_static("SO_ERROR")), Object::Int(4));
            d.insert(DictKey(Object::from_static("SO_TYPE")), Object::Int(3));
        }
        d.insert(
            DictKey(Object::from_static("SO_SNDBUF")),
            Object::Int(libc_so_sndbuf()),
        );
        d.insert(
            DictKey(Object::from_static("SO_RCVBUF")),
            Object::Int(libc_so_rcvbuf()),
        );

        // TCP_*
        d.insert(DictKey(Object::from_static("TCP_NODELAY")), Object::Int(1));
        d.insert(DictKey(Object::from_static("TCP_MAXSEG")), Object::Int(2));
        d.insert(DictKey(Object::from_static("TCP_KEEPIDLE")), Object::Int(4));
        d.insert(
            DictKey(Object::from_static("TCP_KEEPINTVL")),
            Object::Int(5),
        );
        d.insert(DictKey(Object::from_static("TCP_KEEPCNT")), Object::Int(6));
        // Darwin's spelling of the keepalive-idle option (<netinet/tcp.h>
        // TCP_KEEPALIVE = 0x10) — TestMacOSTCPFlags asserts it exists.
        #[cfg(any(target_os = "macos", target_os = "ios"))]
        d.insert(
            DictKey(Object::from_static("TCP_KEEPALIVE")),
            Object::Int(0x10),
        );

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

        // Send/recv flags — real libc values (test_socket's
        // testCrucialConstants and the RecvmsgTests assert on
        // MSG_CTRUNC/MSG_TRUNC/MSG_EOR presence, RFC 0068 WS8).
        #[cfg(unix)]
        for (name, val) in [
            ("MSG_OOB", libc::MSG_OOB),
            ("MSG_PEEK", libc::MSG_PEEK),
            ("MSG_DONTROUTE", libc::MSG_DONTROUTE),
            ("MSG_TRUNC", libc::MSG_TRUNC),
            ("MSG_CTRUNC", libc::MSG_CTRUNC),
            ("MSG_WAITALL", libc::MSG_WAITALL),
            ("MSG_DONTWAIT", libc::MSG_DONTWAIT),
            ("MSG_EOR", libc::MSG_EOR),
        ] {
            d.insert(
                DictKey(Object::from_str(name.to_owned())),
                Object::Int(i64::from(val)),
            );
        }
        #[cfg(not(unix))]
        {
            d.insert(DictKey(Object::from_static("MSG_OOB")), Object::Int(1));
            d.insert(DictKey(Object::from_static("MSG_PEEK")), Object::Int(2));
            d.insert(DictKey(Object::from_static("MSG_WAITALL")), Object::Int(8));
        }
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

        // getaddrinfo flags — real libc values (they are passed straight
        // into `hints.ai_flags`, and differ per platform: AI_NUMERICSERV
        // is 0x1000 on Darwin, 0x400 on Linux). aiohttp's resolver reads
        // `AI_ADDRCONFIG` at import.
        #[cfg(unix)]
        for (name, val) in [
            ("AI_PASSIVE", libc::AI_PASSIVE),
            ("AI_CANONNAME", libc::AI_CANONNAME),
            ("AI_NUMERICHOST", libc::AI_NUMERICHOST),
            ("AI_NUMERICSERV", libc::AI_NUMERICSERV),
            ("AI_ADDRCONFIG", libc::AI_ADDRCONFIG),
            ("AI_ALL", libc::AI_ALL),
            ("AI_V4MAPPED", libc::AI_V4MAPPED),
        ] {
            d.insert(
                DictKey(Object::from_str(name.to_owned())),
                Object::Int(i64::from(val)),
            );
        }
        // On non-unix these are the ws2tcpip.h values — they are passed
        // straight into Winsock's `getaddrinfo` hints.
        #[cfg(not(unix))]
        {
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
            d.insert(
                DictKey(Object::from_static("AI_ADDRCONFIG")),
                Object::Int(0x0400),
            );
            d.insert(DictKey(Object::from_static("AI_ALL")), Object::Int(0x0100));
            d.insert(
                DictKey(Object::from_static("AI_V4MAPPED")),
                Object::Int(0x0800),
            );
        }

        // getaddrinfo error codes — `gaierror.errno` values, published with
        // the platform's own numbering exactly as CPython does (negative on
        // Linux, positive on Darwin). gevent's resolver package imports
        // `EAI_NONAME`/`EAI_SERVICE` from `_socket` at module scope
        // (RFC 0072 WS2).
        #[cfg(unix)]
        for (name, val) in [
            ("EAI_AGAIN", libc::EAI_AGAIN),
            ("EAI_BADFLAGS", libc::EAI_BADFLAGS),
            ("EAI_FAIL", libc::EAI_FAIL),
            ("EAI_FAMILY", libc::EAI_FAMILY),
            ("EAI_MEMORY", libc::EAI_MEMORY),
            ("EAI_NONAME", libc::EAI_NONAME),
            ("EAI_OVERFLOW", libc::EAI_OVERFLOW),
            ("EAI_SERVICE", libc::EAI_SERVICE),
            ("EAI_SOCKTYPE", libc::EAI_SOCKTYPE),
            ("EAI_SYSTEM", libc::EAI_SYSTEM),
        ] {
            d.insert(
                DictKey(Object::from_str(name.to_owned())),
                Object::Int(i64::from(val)),
            );
        }
        #[cfg(any(target_os = "macos", all(target_os = "linux", target_env = "gnu")))]
        d.insert(
            DictKey(Object::from_static("EAI_NODATA")),
            Object::Int(i64::from(libc::EAI_NODATA)),
        );
        // glibc netdb.h value; the libc crate doesn't expose the
        // deprecated EAI_ADDRFAMILY for linux-gnu.
        #[cfg(all(target_os = "linux", target_env = "gnu"))]
        d.insert(
            DictKey(Object::from_static("EAI_ADDRFAMILY")),
            Object::Int(-9),
        );
        // Darwin-only codes CPython also publishes there (netdb.h values;
        // the libc crate doesn't carry the deprecated EAI_ADDRFAMILY).
        #[cfg(target_os = "macos")]
        for (name, val) in [
            ("EAI_ADDRFAMILY", 1i32),
            ("EAI_BADHINTS", 12),
            ("EAI_PROTOCOL", 13),
            ("EAI_MAX", 15),
        ] {
            d.insert(
                DictKey(Object::from_str(name.to_owned())),
                Object::Int(i64::from(val)),
            );
        }
        // Winsock spelling (ws2tcpip.h maps EAI_* onto WSA error codes).
        #[cfg(not(unix))]
        for (name, val) in [
            ("EAI_AGAIN", 11002i64),
            ("EAI_BADFLAGS", 10022),
            ("EAI_FAIL", 11003),
            ("EAI_FAMILY", 10047),
            ("EAI_MEMORY", 8),
            ("EAI_NODATA", 11004),
            ("EAI_NONAME", 11001),
            ("EAI_SERVICE", 10109),
            ("EAI_SOCKTYPE", 10044),
        ] {
            d.insert(DictKey(Object::from_str(name.to_owned())), Object::Int(val));
        }

        // getnameinfo flags — like AI_*, these reach the resolver verbatim,
        // so publish the platform's own numbering (ws2tcpip.h on Windows,
        // where NI_NUMERICHOST is 2 and NI_NUMERICSERV is 8).
        #[cfg(windows)]
        for (name, val) in [
            ("NI_NOFQDN", 0x01),
            ("NI_NUMERICHOST", 0x02),
            ("NI_NAMEREQD", 0x04),
            ("NI_NUMERICSERV", 0x08),
            ("NI_DGRAM", 0x10),
        ] {
            d.insert(DictKey(Object::from_static(name)), Object::Int(val));
        }
        // POSIX doesn't fix the NI_* numbering: Linux uses 1/2/4/8/16 for
        // NOFQDN..DGRAM but macOS/BSD use 1/2/4/8/16 in a *different order*
        // (NUMERICHOST=2, NAMEREQD=4, NUMERICSERV=8). Take libc's values so
        // the flags reach getnameinfo(3) meaning what the caller asked
        // (test_getnameinfo_ipv6_scopeid_symbolic passes NI_NUMERICSERV and
        // asserts the port comes back numeric).
        #[cfg(unix)]
        for (name, val) in [
            ("NI_NOFQDN", libc::NI_NOFQDN),
            ("NI_NUMERICHOST", libc::NI_NUMERICHOST),
            ("NI_NAMEREQD", libc::NI_NAMEREQD),
            ("NI_NUMERICSERV", libc::NI_NUMERICSERV),
            ("NI_DGRAM", libc::NI_DGRAM),
        ] {
            d.insert(
                DictKey(Object::from_static(name)),
                Object::Int(i64::from(val)),
            );
        }
        #[cfg(not(any(windows, unix)))]
        {
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
        }

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
            Object::Type(herror_class()),
        );
        d.insert(
            DictKey(Object::from_static("gaierror")),
            Object::Type(gaierror_class()),
        );
        d.insert(
            DictKey(Object::from_static("timeout")),
            Object::Type(crate::builtin_types::builtin_types().timeout_error.clone()),
        );

        // Module-level functions.
        for (name, body) in module_functions() {
            d.insert(DictKey(Object::from_static(name)), b(name, body));
        }
        // `getaddrinfo(host, port, family=0, type=0, proto=0, flags=0)` is
        // routinely called with keyword arguments (e.g. CPython's bundled
        // `smtpd`/`asyncio` pass `type=SOCK_STREAM`), so register a
        // kwargs-aware variant over the positional core.
        d.insert(
            DictKey(Object::from_static("getaddrinfo")),
            Object::Builtin(Rc::new(BuiltinFn {
                name: "getaddrinfo",
                binds_instance: false,
                call: Box::new(mod_getaddrinfo),
                call_kw: Some(Box::new(mod_getaddrinfo_kw)),
            })),
        );
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

/// `socket.gaierror` — a real `OSError` subclass, as in CPython
/// (`test_exception_hierarchy` asserts `gaierror.__base__ is OSError`).
fn gaierror_class() -> Rc<TypeObject> {
    static GAIERROR: std::sync::OnceLock<Rc<TypeObject>> = std::sync::OnceLock::new();
    GAIERROR
        .get_or_init(|| {
            let bt = crate::builtin_types::builtin_types();
            TypeObject::new_exception("gaierror", bt.os_error.clone()).expect("socket.gaierror")
        })
        .clone()
}

/// Build a raised `socket.gaierror(code, msg)` the way CPython's
/// `set_gaierror` does: `args = (code, msg)` with `errno`/`strerror`
/// populated so `str(e)` renders `[Errno code] msg`. The message source
/// is `gai_strerror` on POSIX and `FormatMessageW` on Windows (where
/// ws2tcpip.h's gai_strerror is itself a FormatMessage wrapper).
#[cfg(any(unix, windows))]
fn gaierror(code: i32, msg: String) -> crate::error::RuntimeError {
    let exc = crate::builtin_types::make_exception_with_class(gaierror_class(), &msg);
    if let Object::Instance(inst) = &exc {
        inst.slot_set(
            "args",
            Object::new_tuple(vec![Object::Int(i64::from(code)), Object::from_str(&msg)]),
        );
        inst.slot_set("errno", Object::Int(i64::from(code)));
        inst.slot_set("strerror", Object::from_str(msg));
    }
    crate::error::RuntimeError::PyException(crate::error::PyException::new(exc))
}

/// `socket.herror` — likewise a direct `OSError` subclass.
fn herror_class() -> Rc<TypeObject> {
    static HERROR: std::sync::OnceLock<Rc<TypeObject>> = std::sync::OnceLock::new();
    HERROR
        .get_or_init(|| {
            let bt = crate::builtin_types::builtin_types();
            TypeObject::new_exception("herror", bt.os_error.clone()).expect("socket.herror")
        })
        .clone()
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
            let mut dict = DictData::default();
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
            // `_socket.socket` is an immutable type in CPython (no
            // Py_TPFLAGS_BASETYPE mutation; test_socket_type asserts the
            // TypeError). Subclassing stays allowed, like `int`.
            cls.immutable.set(true);
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
    #[cfg_attr(not(unix), allow(unused_mut))]
    let mut methods = vec![
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
        m!("__del__", sock_del),
        m!("bind", sock_bind),
        m!("listen", sock_listen),
        m!("accept", sock_accept),
        m!("_accept", sock_accept_fd),
        m!("connect", sock_connect),
        m!("connect_ex", sock_connect_ex),
        m!("send", sock_send),
        m!("sendall", sock_sendall),
        m!("sendto", sock_sendto),
        m!("recv", sock_recv),
        m!("recv_into", sock_recv_into),
        m!("recvfrom", sock_recvfrom),
        m!("recvfrom_into", sock_recvfrom_into),
        m!("setblocking", sock_setblocking),
        m!("getblocking", sock_getblocking),
        m!("settimeout", sock_settimeout),
        m!("gettimeout", sock_gettimeout),
        m!("setsockopt", sock_setsockopt),
        m!("getsockopt", sock_getsockopt),
        m!("getsockname", sock_getsockname),
        m!("getpeername", sock_getpeername),
        m!("fileno", sock_fileno),
        m!("get_inheritable", sock_get_inheritable),
        m!("set_inheritable", sock_set_inheritable),
        m!("close", sock_close),
        m!("shutdown", sock_shutdown),
        m!("detach", sock_detach),
        m!("dup", sock_dup),
        m!("makefile", sock_makefile),
        m!("family_get", sock_family_attr),
        m!("type_get", sock_type_attr),
        m!("proto_get", sock_proto_attr),
    ];
    // `sendmsg`/`recvmsg` exist only where CMSG ancillary data does:
    // CPython compiles them under `#ifdef CMSG_LEN` (socketmodule.c), so on
    // Windows the names are simply *absent* — `hasattr` gates like
    // `multiprocessing.reduction.HAVE_SEND_HANDLE` rely on that signal.
    #[cfg(unix)]
    methods.extend([
        m!("sendmsg", sock_sendmsg),
        m!("recvmsg", sock_recvmsg),
        m!("recvmsg_into", sock_recvmsg_into),
    ]);
    methods
}

/// CPython's `sock_finalize`: deallocating a socket whose descriptor is
/// still open emits `ResourceWarning("unclosed %R")` *before* the fd is
/// torn down, using the (possibly subclass) live repr — test_dealloc_warn
/// asserts the rich socket.py repr appears in the message.
fn sock_del(args: &[Object]) -> Result<Object, RuntimeError> {
    let Ok(inst) = extract_self(args) else {
        return Ok(Object::None);
    };
    let Ok(handle) = extract_handle(&inst) else {
        return Ok(Object::None);
    };
    if handle == -1 {
        return Ok(Object::None);
    }
    let Some(state) = get_state(handle) else {
        return Ok(Object::None);
    };
    if state.borrow().inner.is_none() {
        return Ok(Object::None);
    }
    if let Some(ptr) = crate::vm_singletons::current_interpreter_ptr() {
        // SAFETY: published by the interpreter driving this finalizer on
        // this thread; it outlives the call.
        let interp = unsafe { &mut *ptr };
        let repr = interp
            .repr_object(&args[0])
            .unwrap_or_else(|_| "<socket object>".to_owned());
        let _ = interp.warn_resource_from_builtin(format!("unclosed {repr}"));
    }
    Ok(Object::None)
}

fn extract_self(args: &[Object]) -> Result<Rc<PyInstance>, RuntimeError> {
    match args.first() {
        // Accept the native `socket` and any subclass of it (e.g. the public
        // `socket.socket` wrapper and `ssl.SSLSocket`) by checking the whole
        // MRO, not just the instance's immediate class name.
        Some(Object::Instance(inst))
            if inst.cls().mro.borrow().iter().any(|t| t.name == "socket") =>
        {
            Ok(inst.clone())
        }
        _ => Err(type_error("socket method requires socket self")),
    }
}

fn extract_handle(inst: &PyInstance) -> Result<i64, RuntimeError> {
    let dict = inst.dict.borrow();
    match dict.get(&DictKey(Object::from_static("_handle"))) {
        Some(Object::Int(h)) => Ok(*h),
        _ => Err(closed_socket_error()),
    }
}

fn state_of(args: &[Object]) -> Result<Rc<RefCell<SocketState>>, RuntimeError> {
    let inst = extract_self(args)?;
    let handle = extract_handle(&inst)?;
    get_state(handle).ok_or_else(closed_socket_error)
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
        let sock = b.inner.as_ref().ok_or_else(closed_socket_error)?;
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
    // Python signal handlers only run on the main thread (CPython), so a
    // worker-thread socket op that trips `EINTR` just retries — the main
    // thread services the handler when it next checks.
    if !crate::gil::is_main_thread() || !crate::stdlib::signal_mod::signals_pending() {
        return Ok(());
    }
    if let Some(ptr) = crate::vm_singletons::current_interpreter_ptr() {
        // SAFETY: the GIL is held (we just returned from `allow_threads_then`),
        // so the interpreter pointer is exclusively ours.
        unsafe { (*ptr).run_pending_signals_public()? };
    }
    Ok(())
}

/// Bounded readiness wait on one SOCKET via Winsock `select()` — the
/// Windows twin of the `libc::poll` loop in `sock_accept` (CPython's
/// `internal_select`, socketmodule.c). GIL released for the wait; `0`
/// ready descriptors at the deadline surfaces as `socket.timeout`,
/// a `SOCKET_ERROR` as the WSA-code `OSError` via the WS1 error bridge.
#[cfg(windows)]
fn wait_readable_win(
    sock: windows_sys::Win32::Networking::WinSock::SOCKET,
    timeout: Duration,
) -> Result<(), RuntimeError> {
    use windows_sys::Win32::Networking::WinSock as ws;
    let mut fds = ws::FD_SET {
        fd_count: 1,
        fd_array: [0; 64],
    };
    fds.fd_array[0] = sock;
    // Round the timeout *up* so we wait at least the requested span.
    let mut us = timeout.as_micros();
    if u128::from(timeout.subsec_nanos()) % 1_000 != 0 {
        us += 1;
    }
    let us = us.min(i32::MAX as u128 * 1_000_000);
    let tv = ws::TIMEVAL {
        tv_sec: (us / 1_000_000) as i32,
        tv_usec: (us % 1_000_000) as i32,
    };
    let n = crate::gil::allow_threads_then(|| {
        // SAFETY: `fds` and `tv` outlive the call; nfds is ignored on
        // Windows; NULL write/except sets are allowed.
        unsafe {
            ws::select(
                0,
                &raw mut fds,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &raw const tv,
            )
        }
    });
    match n {
        0 => Err(timeout_error("timed out")),
        ws::SOCKET_ERROR => Err(crate::stdlib::nt_support::win32_error_to_py(
            unsafe { ws::WSAGetLastError() },
            None,
        )),
        _ => Ok(()),
    }
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
/// One bounded readiness wait against an absolute deadline (the poll leg of
/// CPython's `sock_call_ex`). `events` is `POLLIN`/`POLLOUT`.
#[cfg(unix)]
fn wait_fd_until(
    fd: libc::c_int,
    events: libc::c_short,
    deadline: std::time::Instant,
) -> Result<(), RuntimeError> {
    loop {
        let remain = deadline.saturating_duration_since(std::time::Instant::now());
        if remain.is_zero() {
            return Err(timeout_error("timed out"));
        }
        let ms = remain.as_millis().min(i32::MAX as u128) as i32;
        let mut pfd = libc::pollfd {
            fd,
            events,
            revents: 0,
        };
        let r = crate::gil::allow_threads_then(|| unsafe {
            libc::poll(std::ptr::addr_of_mut!(pfd), 1, ms)
        });
        match r {
            0 => return Err(timeout_error("timed out")),
            n if n > 0 => return Ok(()),
            _ => {
                let err = std::io::Error::last_os_error();
                if err.raw_os_error() == Some(libc::EINTR) {
                    run_pending_signals_after_eintr()?;
                    continue;
                }
                return Err(io_error_to_py(&err));
            }
        }
    }
}

/// Readiness masks for [`blocking_socket_io`]. On unix these are the real
/// `poll(2)` bits; on Windows the wait leg (`wait_readable_win`) ignores
/// the mask (Winsock `select` is used instead), so only the type has to
/// line up — `libc` does not export `POLLIN`/`POLLOUT` there.
#[cfg(unix)]
const POLL_IN: libc::c_short = libc::POLLIN;
#[cfg(unix)]
const POLL_OUT: libc::c_short = libc::POLLOUT;
#[cfg(not(unix))]
const POLL_IN: libc::c_short = 0x0001;
#[cfg(not(unix))]
const POLL_OUT: libc::c_short = 0x0004;

/// CPython's `sock_call_ex` shape (RFC 0068 WS8): a socket in timeout mode
/// keeps its fd non-blocking — the deadline is enforced with a poll-based
/// readiness wait *around* the syscall, never with `SO_RCVTIMEO`. `events`
/// says which readiness the op needs (`POLLIN` for the recv family and
/// accept, `POLLOUT` for the send family).
#[cfg(any(unix, windows))]
fn blocking_socket_io<R>(
    state: &Rc<RefCell<SocketState>>,
    events: libc::c_short,
    mut f: impl FnMut(&Socket) -> std::io::Result<R>,
) -> Result<R, RuntimeError> {
    // Timeout mode computes one absolute deadline for the whole operation;
    // an EAGAIN after a wakeup (readiness stolen by a peer thread) loops
    // back into the wait with the *remaining* budget, exactly like CPython.
    #[cfg(unix)]
    let deadline = {
        let timeout = state.borrow().timeout;
        timeout
            .filter(|t| !t.is_zero())
            .map(|t| std::time::Instant::now() + t)
    };
    #[cfg(windows)]
    let win_timeout = state.borrow().timeout.filter(|t| !t.is_zero());
    // The only retry arms are unix-only (EINTR, timeout-mode EAGAIN), so on
    // Windows every pass through the body returns.
    #[cfg_attr(windows, allow(clippy::never_loop))]
    loop {
        #[cfg(unix)]
        if let Some(dl) = deadline {
            let fd = snapshot_raw_fd(state)? as libc::c_int;
            wait_fd_until(fd, events, dl)?;
        }
        #[cfg(windows)]
        if let Some(t) = win_timeout {
            let sock = snapshot_raw_fd(state)?;
            let _ = events;
            wait_readable_win(sock as windows_sys::Win32::Networking::WinSock::SOCKET, t)?;
        }
        match socket_call_once(state, &mut f)? {
            Ok(v) => return Ok(v),
            #[cfg(unix)]
            Err(ref e) if e.raw_os_error() == Some(libc::EINTR) => {
                run_pending_signals_after_eintr()?;
                continue;
            }
            Err(e) => {
                #[cfg(unix)]
                if e.kind() == std::io::ErrorKind::WouldBlock && deadline.is_some() {
                    // Readiness came and went before our syscall (another
                    // thread drained the socket): wait again on the same
                    // deadline rather than failing.
                    continue;
                }
                return Err(io_error_to_py(&e));
            }
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
            None | Some(Object::None) => Ok(default),
            // `as_i64` also unwraps int-subclass instances — `socket.py`
            // promotes the constants to `AddressFamily`/`SocketKind`
            // IntEnum members, and CPython accepts any int here.
            Some(o) => match o.as_i64() {
                // CPython treats the -1 sentinel as "use the default".
                Some(-1) => Ok(default),
                Some(i) => Ok(i as i32),
                None => Err(type_error(format!("{what} must be int"))),
            },
        }
    };
    // Keep the -1 "unspecified" sentinel through the fileno branch: CPython
    // infers a missing family/type from the descriptor itself (getsockname /
    // SO_TYPE), so `socket(fileno=unix_fd).family == AF_UNIX` — only sockets
    // created from scratch default to AF_INET/SOCK_STREAM.
    let mut family = as_i32(&slots[0], -1, "family")?;
    let mut kind = as_i32(&slots[1], -1, "type")?;
    let mut proto = as_i32(&slots[2], -1, "proto")?;
    let fileno = match &slots[3] {
        None | Some(Object::None) => None,
        Some(Object::Int(fd)) => Some(*fd),
        _ => return Err(type_error("fileno must be int or None")),
    };
    // PEP 578: `socket.__new__(self, family, type, proto)` — fired
    // from CPython's `sock_initobj` before the descriptor exists.
    crate::stdlib::sys::audit_event(
        "socket.__new__",
        &[
            args[0].clone(),
            Object::Int(i64::from(family)),
            Object::Int(i64::from(kind)),
            Object::Int(i64::from(proto)),
        ],
    )?;
    let (inner, owns_fd) = match fileno {
        Some(fd) => {
            // CPython's sock_initobj: a negative fd is a *ValueError*, and a
            // fd that isn't a live socket raises the getsockname errno
            // (EBADF / ENOTSOCK — test_socket_fileno_requires_valid_fd and
            // _requires_socket_fd probe both).
            if fd < 0 {
                return Err(value_error("negative file descriptor"));
            }
            #[cfg(unix)]
            {
                let mut addrbuf = [0u8; 128];
                let mut addrlen = addrbuf.len() as libc::socklen_t;
                let r = unsafe {
                    libc::getsockname(
                        fd as libc::c_int,
                        addrbuf.as_mut_ptr().cast::<libc::sockaddr>(),
                        &raw mut addrlen,
                    )
                };
                if r == 0 {
                    if family == -1 {
                        // sa_family sits at the head of every sockaddr.
                        let sa = unsafe { &*addrbuf.as_ptr().cast::<libc::sockaddr>() };
                        family = i32::from(sa.sa_family);
                    }
                } else {
                    let err = std::io::Error::last_os_error();
                    // Mirror CPython: EBADF/ENOTSOCK always raise; other
                    // getsockname failures only matter when the family
                    // would have to be inferred from the address.
                    let fatal =
                        matches!(err.raw_os_error(), Some(libc::EBADF) | Some(libc::ENOTSOCK))
                            || family == -1;
                    if fatal {
                        return Err(io_error_to_py(&err));
                    }
                }
                if kind == -1 {
                    let mut stype: libc::c_int = 0;
                    let mut slen = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
                    let r = unsafe {
                        libc::getsockopt(
                            fd as libc::c_int,
                            libc::SOL_SOCKET,
                            libc::SO_TYPE,
                            (&raw mut stype).cast(),
                            &raw mut slen,
                        )
                    };
                    if r != 0 {
                        return Err(io_error_to_py(&std::io::Error::last_os_error()));
                    }
                    kind = stype;
                }
                if proto == -1 {
                    // SO_PROTOCOL exists on Linux; elsewhere CPython falls
                    // back to 0 just the same.
                    #[cfg(target_os = "linux")]
                    {
                        let mut p: libc::c_int = 0;
                        let mut slen = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
                        let r = unsafe {
                            libc::getsockopt(
                                fd as libc::c_int,
                                libc::SOL_SOCKET,
                                libc::SO_PROTOCOL,
                                (&raw mut p).cast(),
                                &raw mut slen,
                            )
                        };
                        proto = if r == 0 { p } else { 0 };
                    }
                    #[cfg(not(target_os = "linux"))]
                    {
                        proto = 0;
                    }
                }
            }
            // `socket(fileno=other.fileno())` co-owns the descriptor, exactly
            // like CPython (which is why `ssl.py` detaches the original after
            // wrapping): closing either object closes the fd, and the other's
            // close then reports EBADF (testCloseException). When another live
            // WeavePy socket is registered under this fd number we register
            // the new state under a synthetic handle so both objects keep
            // their own state.
            (wrap_fd_socket(fd)?, true)
        }
        None => {
            if family == -1 {
                family = libc_af_inet() as i32;
            }
            if kind == -1 {
                kind = libc_sock_stream() as i32;
            }
            if proto == -1 {
                proto = 0;
            }
            (
                Socket::new(
                    Domain::from(family),
                    Type::from(kind),
                    Some(Protocol::from(proto)),
                )
                .map_err(|e| io_error_to_py(&e))?,
                true,
            )
        }
    };
    // Any leftover sentinel (fileno path on non-unix platforms) falls back
    // to the defaults so the recorded attributes stay well-formed.
    if family == -1 {
        family = libc_af_inet() as i32;
    }
    if kind == -1 {
        kind = libc_sock_stream() as i32;
    }
    if proto == -1 {
        proto = 0;
    }
    // CPython's init_sockobject: every new socket object starts with the
    // module-wide default timeout, and a non-None default puts the fd in
    // non-blocking mode right away (testDefaultTimeout / the accept
    // inheritance dance in socket.py's accept()).
    let timeout = (*default_timeout().lock()).map(Duration::from_secs_f64);
    if timeout.is_some() {
        let _ = inner.set_nonblocking(true);
        mirror_timeout_sockopts(&inner, timeout);
    }
    let state = Rc::new(RefCell::new(SocketState {
        inner: Some(inner),
        family,
        kind,
        proto,
        timeout,
        blocking: timeout.is_none(),
        owns_fd,
    }));
    // A fileno= socket whose fd number is already registered to another live
    // socket keeps its own state under a synthetic handle (both objects own
    // the shared descriptor, per CPython).
    let already_registered =
        fileno.is_some_and(|fd| get_state(fd).is_some_and(|s| s.borrow().inner.is_some()));
    let handle = if already_registered {
        insert_alias_handle(state)
    } else {
        next_handle(state)
    };
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
        // Remove the registry entry *before* issuing `close(2)`. The registry
        // is keyed by fd number, and the kernel recycles a closed number
        // immediately: with close-then-remove there was a window where a peer
        // thread's `accept()`/`socket()` received this same number and
        // registered it under the key we were about to remove — our
        // `remove_state` then stripped the *new* socket's entry, and dropping
        // it closed the fresh descriptor underneath its owner (test_ftplib's
        // dummy-server thread intermittently lost its just-accepted data
        // connection this way, surfacing as EBADF in `create_connection` or a
        // dead command channel).
        let state = registry().lock().remove(&handle);
        // Mark the object closed *before* issuing close(2), like CPython's
        // sock_close (fd invalidated first, so a double .close() on the same
        // object is a no-op even when the close itself errors).
        inst.dict
            .borrow_mut()
            .insert(DictKey(Object::from_static("_handle")), Object::Int(-1));
        if let Some(state) = state {
            let (sock, owns) = {
                let mut b = state.borrow_mut();
                (b.inner.take(), b.owns_fd)
            };
            if let Some(sock) = sock {
                if owns {
                    // CPython raises close(2) failures (swallowing only
                    // ECONNRESET): after another socket object sharing the
                    // descriptor closed it, this close reports EBADF
                    // (testCloseException).
                    close_owned_socket(sock)?;
                } else {
                    release_socket(sock);
                }
            }
        }
        return Ok(Object::Bool(false));
    }
    inst.dict
        .borrow_mut()
        .insert(DictKey(Object::from_static("_handle")), Object::Int(-1));
    Ok(Object::Bool(false))
}

fn sock_repr(args: &[Object]) -> Result<Object, RuntimeError> {
    // CPython `sock_repr` (socketmodule.c): "<socket object, fd=%ld,
    // family=%d, type=%d, proto=%d>" with fd=-1 once closed
    // (test_csocket_repr asserts the exact string; `socket.socket`'s richer
    // repr lives in socket.py on top of this).
    let inst = extract_self(args)?;
    let attr_int = |name: &'static str| {
        inst.dict
            .borrow()
            .get(&DictKey(Object::from_static(name)))
            .and_then(Object::as_i64)
            .unwrap_or(0)
    };
    let fd = match sock_fileno(args)? {
        Object::Int(n) => n,
        _ => -1,
    };
    Ok(Object::from_str(format!(
        "<socket object, fd={fd}, family={}, type={}, proto={}>",
        attr_int("family"),
        attr_int("type"),
        attr_int("proto")
    )))
}

fn sock_bind(args: &[Object]) -> Result<Object, RuntimeError> {
    let state = state_of(args)?;
    let family = state.borrow().family;
    // PEP 578: `socket.bind(self, address)` with the pre-parse address.
    crate::stdlib::sys::audit_event(
        "socket.bind",
        &[
            args[0].clone(),
            args.get(1).cloned().unwrap_or(Object::None),
        ],
    )?;
    let addr = parse_sockaddr2(args.get(1), family)?;
    let s_borrow = state.borrow();
    let sock = s_borrow.inner.as_ref().ok_or_else(closed_socket_error)?;
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
    let sock = s_borrow.inner.as_ref().ok_or_else(closed_socket_error)?;
    sock.listen(backlog).map_err(|e| io_error_to_py(&e))?;
    Ok(Object::None)
}

fn sock_accept(args: &[Object]) -> Result<Object, RuntimeError> {
    let (new_sock, addr, family, kind, proto) = accept_parts(args)?;
    let new_state = Rc::new(RefCell::new(SocketState {
        inner: Some(new_sock),
        family,
        kind,
        proto,
        timeout: None,
        blocking: true,
        owns_fd: true,
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

/// `_socket.socket._accept()` — the C-level accept: returns `(fd, addr)`
/// with ownership of the raw descriptor transferred to the caller.
/// CPython's `socket.py` wraps it via `socket(fileno=fd)` (RFC 0068 WS8:
/// the verbatim socket.py replaces the WeavePy-shaped shim).
fn sock_accept_fd(args: &[Object]) -> Result<Object, RuntimeError> {
    let (new_sock, addr, family, _, _) = accept_parts(args)?;
    #[cfg(unix)]
    let fd = {
        use std::os::unix::io::IntoRawFd;
        i64::from(new_sock.into_raw_fd())
    };
    #[cfg(windows)]
    let fd = {
        use std::os::windows::io::IntoRawSocket;
        new_sock.into_raw_socket() as i64
    };
    let addr_tuple = sockaddr_to_tuple(&addr, family);
    Ok(Object::new_tuple(vec![Object::Int(fd), addr_tuple]))
}

fn accept_parts(
    args: &[Object],
) -> Result<(Socket, socket2::SockAddr, i32, i32, i32), RuntimeError> {
    let state = state_of(args)?;
    // The timeout-mode readiness wait (CPython's `sock_call_ex` poll loop)
    // lives inside `blocking_socket_io` — `SO_RCVTIMEO` never bounded
    // `accept(2)` on macOS/BSD anyway, which is why CPython always waits
    // with poll/select first. test_ftplib's `TestTimeouts.server` thread
    // relies on this: its `accept()` must raise `TimeoutError` so the
    // thread exits and `tearDown`'s `join()` returns.
    //
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
    let (new_sock, addr) = blocking_socket_io(&state, POLL_IN, |sock| sock.accept_raw())?;
    // PEP 446: accepted descriptors are non-inheritable. `F_SETFD` acts on the
    // descriptor-table entry (not the connection), so it succeeds even for a
    // peer-closed connection; ignore any error to stay non-fatal regardless.
    // `set_cloexec` is a POSIX (`FD_CLOEXEC`) helper; non-inheritability is
    // handled differently on Windows, so the call is Unix-only.
    #[cfg(unix)]
    let _ = new_sock.set_cloexec(true);
    let (family, kind, proto) = {
        let s = state.borrow();
        (s.family, s.kind, s.proto)
    };
    Ok((new_sock, addr, family, kind, proto))
}

fn sock_connect(args: &[Object]) -> Result<Object, RuntimeError> {
    let state = state_of(args)?;
    let (family, timeout) = {
        let b = state.borrow();
        (b.family, b.timeout)
    };
    // The connect-timeout poll leg below is unix-only; keep the binding
    // from tripping `-D unused_variables` on Windows.
    #[cfg(windows)]
    let _ = timeout;
    let sockaddr = parse_sockaddr2(args.get(1), family)?;
    let mut connect_fn = |sock: &Socket| sock.connect(&sockaddr);
    // Single attempt — *no* EINTR retry: a signal-interrupted blocking
    // connect continues asynchronously, so a second `connect(2)` would return
    // `EISCONN`/`EALREADY` rather than completing it. Surface the syscall
    // result directly (a handled signal still ran via the eval breaker).
    match socket_call_once(&state, &mut connect_fn)? {
        Ok(()) => Ok(Object::None),
        Err(e) => {
            // Timeout mode (fd non-blocking, positive deadline): connect
            // reports EINPROGRESS immediately; CPython's internal_connect
            // then waits for writability up to the deadline and reads the
            // final status from SO_ERROR. Plain non-blocking (`settimeout(0)`)
            // surfaces EINPROGRESS to the caller as BlockingIOError.
            #[cfg(unix)]
            if matches!(
                e.raw_os_error(),
                Some(libc::EINPROGRESS) | Some(libc::EWOULDBLOCK)
            ) {
                if let Some(t) = timeout.filter(|t| !t.is_zero()) {
                    let fd = snapshot_raw_fd(&state)?;
                    wait_fd_until(fd, libc::POLLOUT, std::time::Instant::now() + t)?;
                    let mut err: libc::c_int = 0;
                    let mut len = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
                    let r = unsafe {
                        libc::getsockopt(
                            fd,
                            libc::SOL_SOCKET,
                            libc::SO_ERROR,
                            (&raw mut err).cast(),
                            &raw mut len,
                        )
                    };
                    if r != 0 {
                        return Err(io_error_to_py(&std::io::Error::last_os_error()));
                    }
                    if err != 0 {
                        return Err(io_error_to_py(&std::io::Error::from_raw_os_error(err)));
                    }
                    return Ok(Object::None);
                }
            }
            Err(io_error_to_py(&e))
        }
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
            let errno = errno_of_exception(&p).unwrap_or_else(|| {
                // A timeout-mode connect that hits its deadline carries no
                // errno (CPython's internal_connect reports EWOULDBLOCK for
                // it — test_timeout_connect_ex asserts EAGAIN/EWOULDBLOCK).
                if p.type_name() == "TimeoutError" {
                    i64::from(libc::EWOULDBLOCK)
                } else {
                    i64::from(libc::EINVAL)
                }
            });
            Ok(Object::Int(errno))
        }
        Err(e) => Err(e),
    }
}

/// Recover the integer `errno` an `OSError`-family exception was built with
/// (see [`crate::error::io_error_to_py`]), if present.
fn errno_of_exception(p: &crate::error::PyException) -> Option<i64> {
    if let Object::Instance(inst) = &p.instance {
        if let Some(Object::Int(n)) = crate::builtin_types::exc_attr(inst, "errno") {
            return Some(n);
        }
    }
    None
}

/// Strict bytes-like conversion for the send family: CPython's `y*`
/// converter — `str` is *not* bytes-like ("a bytes-like object is required,
/// not 'str'", testSendtoErrors), and arbitrary buffer exporters work via
/// `tobytes()`.
fn send_data_arg(arg: Option<&Object>) -> Result<Vec<u8>, RuntimeError> {
    match arg {
        Some(Object::Str(_)) | Some(Object::WStr(_)) => Err(type_error(
            "a bytes-like object is required, not 'str'".to_string(),
        )),
        Some(o) => extract_bytes(Some(o))
            .ok()
            .filter(|_| !matches!(o, Object::Str(_)))
            .or_else(|| buffer_protocol_bytes(o))
            .ok_or_else(|| {
                type_error(format!(
                    "a bytes-like object is required, not '{}'",
                    o.type_name()
                ))
            }),
        None => Err(type_error("a bytes-like object is required")),
    }
}

fn sock_send(args: &[Object]) -> Result<Object, RuntimeError> {
    let state = state_of(args)?;
    let data = send_data_arg(args.get(1))?;
    let n = blocking_socket_io(&state, POLL_OUT, |sock| sock.send(&data))?;
    Ok(Object::Int(n as i64))
}

fn sock_sendall(args: &[Object]) -> Result<Object, RuntimeError> {
    let state = state_of(args)?;
    let data = send_data_arg(args.get(1))?;
    // Loop per chunk in the caller (not inside one `blocking_socket_io`
    // closure) so a PEP 475 `EINTR` retry resumes from the current `offset`
    // instead of re-running the whole send and duplicating already-committed
    // bytes. Each individual `send` is the single retryable syscall.
    let mut offset = 0;
    while offset < data.len() {
        let n = blocking_socket_io(&state, POLL_OUT, |sock| sock.send(&data[offset..]))?;
        if n == 0 {
            return Err(io_error_to_py(&std::io::Error::from(
                std::io::ErrorKind::BrokenPipe,
            )));
        }
        offset += n;
        // A blocking `send` that has already written some bytes when a signal
        // arrives returns the short count (success) on macOS/BSD rather than
        // `EINTR`, so the `blocking_socket_io` EINTR path above never sees it.
        // Service any tripped Python signal handler after every partial write
        // (PEP 475) — a `SIGALRM` handler that raises then aborts the loop
        // instead of blocking forever on the next, now-saturated `send`
        // (`test_socket`'s `test_sendall_interrupted`). Mirrors the buffered
        // file-writer's `write_drain_fd_intr`.
        #[cfg(unix)]
        run_pending_signals_after_eintr()?;
    }
    Ok(Object::None)
}

fn sock_sendto(args: &[Object]) -> Result<Object, RuntimeError> {
    let state = state_of(args)?;
    // CPython accepts `sendto(data, address)` and `sendto(data, flags,
    // address)` only; other arities are a TypeError with the given count
    // (testSendtoErrors asserts the exact messages).
    let n_given = args.len().saturating_sub(1);
    if !(2..=3).contains(&n_given) {
        return Err(type_error(format!(
            "sendto() takes 2 or 3 arguments ({n_given} given)"
        )));
    }
    let data = send_data_arg(args.get(1))?;
    let addr_arg = if n_given == 3 {
        // The 3-arg form's middle argument is flags; it must be an int.
        if args.get(2).and_then(Object::as_i64).is_none() {
            return Err(type_error(format!(
                "'{}' object cannot be interpreted as an integer",
                args.get(2).map_or("NoneType", |o| o.type_name())
            )));
        }
        args.get(3)
    } else {
        args.get(2)
    };
    let family = state.borrow().family;
    // A non-tuple destination on the inet families is CPython's
    // "sendto(): AF_INET address must be tuple, not NoneType".
    if family != libc_af_unix_i32()
        && !matches!(addr_arg, Some(Object::Tuple(_)) | Some(Object::List(_)))
    {
        let fam_name = if family == libc_af_inet6() as i32 {
            "AF_INET6"
        } else {
            "AF_INET"
        };
        return Err(type_error(format!(
            "sendto(): {fam_name} address must be tuple, not {}",
            addr_arg.map_or("NoneType", |o| o.type_name())
        )));
    }
    // The destination for an `AF_UNIX` datagram socket is a bare path
    // (`str`/`bytes`), not an `(host, port)` tuple — use the family-aware
    // resolver (test_socketserver's Unix datagram servers).
    let sockaddr = parse_sockaddr2(addr_arg, family)?;
    let n = blocking_socket_io(&state, POLL_OUT, |sock| sock.send_to(&data, &sockaddr))?;
    Ok(Object::Int(n as i64))
}

/// AF_UNIX's numeric value where it exists (1 on every unix), or a sentinel
/// that never matches on platforms without it.
fn libc_af_unix_i32() -> i32 {
    #[cfg(unix)]
    {
        1
    }
    #[cfg(not(unix))]
    {
        -2
    }
}

fn sock_recv(args: &[Object]) -> Result<Object, RuntimeError> {
    let state = state_of(args)?;
    let bufsize = match args.get(1) {
        Some(Object::Int(n)) if *n >= 0 => *n as usize,
        Some(Object::Int(_)) => return Err(value_error("negative buffersize in recv")),
        _ => return Err(type_error("recv: bufsize must be int")),
    };
    let mut buf: Vec<std::mem::MaybeUninit<u8>> = vec![std::mem::MaybeUninit::uninit(); bufsize];
    let n = blocking_socket_io(&state, POLL_IN, |sock| sock.recv(&mut buf))?;
    let initialised: Vec<u8> = buf[..n]
        .iter()
        .map(|m| unsafe { m.assume_init() })
        .collect();
    Ok(Object::new_bytes(initialised))
}

fn sock_recv_into(args: &[Object]) -> Result<Object, RuntimeError> {
    let state = state_of(args)?;
    // Any writable bytes-like destination works (bytearray, writable
    // memoryview, PEP 688 exporter) — asyncio's `BufferedProtocol` path
    // passes memoryview slices from `get_buffer()` (RFC 0054 WS2).
    let (dst, start, buflen) = super::io::writable_buffer_dst(args.get(1), "recv_into")?;
    let nbytes = match args.get(2) {
        Some(Object::Int(n)) if *n >= 0 => *n as usize,
        Some(Object::Int(_)) => return Err(value_error("negative buffersize in recv_into")),
        _ => 0,
    };
    let cap = if nbytes == 0 {
        buflen
    } else if nbytes > buflen {
        return Err(value_error(
            "nbytes is greater than the length of the buffer",
        ));
    } else {
        nbytes
    };
    let mut buf = vec![std::mem::MaybeUninit::<u8>::uninit(); cap];
    let n = blocking_socket_io(&state, POLL_IN, |sock| sock.recv(&mut buf))?;
    {
        let mut bytes = dst.borrow_mut();
        for i in 0..n {
            bytes[start + i] = unsafe { buf[i].assume_init() };
        }
    }
    Ok(Object::Int(n as i64))
}

fn sock_recvfrom(args: &[Object]) -> Result<Object, RuntimeError> {
    let state = state_of(args)?;
    let bufsize = match args.get(1) {
        Some(Object::Int(n)) if *n >= 0 => *n as usize,
        Some(Object::Int(_)) => return Err(value_error("negative buffersize in recvfrom")),
        _ => return Err(type_error("recvfrom: bufsize must be int")),
    };
    let mut buf = vec![std::mem::MaybeUninit::<u8>::uninit(); bufsize];
    let (n, addr) = blocking_socket_io(&state, POLL_IN, |sock| sock.recv_from(&mut buf))?;
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

/// `socket.recvfrom_into(buffer[, nbytes[, flags]])` — like `recv_into` but
/// also returns the sender's address (the datagram counterpart). Returns
/// `(nbytes_received, address)`.
fn sock_recvfrom_into(args: &[Object]) -> Result<Object, RuntimeError> {
    let state = state_of(args)?;
    let (dst, start, buflen) = super::io::writable_buffer_dst(args.get(1), "recvfrom_into")?;
    let nbytes = match args.get(2) {
        Some(Object::Int(n)) if *n >= 0 => *n as usize,
        Some(Object::Int(_)) => return Err(value_error("negative buffersize in recvfrom_into")),
        _ => 0,
    };
    let cap = if nbytes == 0 {
        buflen
    } else if nbytes > buflen {
        return Err(value_error(
            "nbytes is greater than the length of the buffer",
        ));
    } else {
        nbytes
    };
    let mut buf = vec![std::mem::MaybeUninit::<u8>::uninit(); cap];
    let (n, addr) = blocking_socket_io(&state, POLL_IN, |sock| sock.recv_from(&mut buf))?;
    {
        let mut bytes = dst.borrow_mut();
        for i in 0..n {
            bytes[start + i] = unsafe { buf[i].assume_init() };
        }
    }
    let family = state.borrow().family;
    Ok(Object::new_tuple(vec![
        Object::Int(n as i64),
        sockaddr_to_tuple(&addr, family),
    ]))
}

/// Bound a raw-`msghdr` syscall with the socket's timeout, CPython's
/// `sock_call_ex` readiness wait: poll the fd for `events` up to the
/// configured timeout, raising `TimeoutError` on expiry. No-op in
/// blocking (`None`) and non-blocking (`0`) modes.
#[cfg(unix)]
fn wait_ready_for_timeout(
    state: &Rc<RefCell<SocketState>>,
    events: libc::c_short,
) -> Result<(), RuntimeError> {
    let timeout = state.borrow().timeout;
    let Some(t) = timeout else { return Ok(()) };
    if t.is_zero() {
        return Ok(());
    }
    let fd = snapshot_raw_fd(state)?;
    let deadline = std::time::Instant::now() + t;
    loop {
        let remain = deadline.saturating_duration_since(std::time::Instant::now());
        if remain.is_zero() {
            return Err(timeout_error("timed out"));
        }
        let ms = remain.as_millis().min(i32::MAX as u128) as i32;
        let mut pfd = libc::pollfd {
            fd,
            events,
            revents: 0,
        };
        let r = crate::gil::allow_threads_then(|| unsafe {
            libc::poll(std::ptr::addr_of_mut!(pfd), 1, ms)
        });
        match r {
            0 => return Err(timeout_error("timed out")),
            n if n > 0 => return Ok(()),
            _ => {
                let err = std::io::Error::last_os_error();
                if err.raw_os_error() == Some(libc::EINTR) {
                    run_pending_signals_after_eintr()?;
                    continue;
                }
                return Err(io_error_to_py(&err));
            }
        }
    }
}

/// `socket.sendmsg(buffers[, ancdata[, flags[, address]]])` — send normal
/// data plus ancillary data (control messages), optionally to an explicit
/// destination address (unconnected UDP — test_socket's SendmsgUDPTest
/// sends via `sendmsgToServer`, RFC 0068 WS8).
///
/// `multiprocessing` uses this for `SCM_RIGHTS` file-descriptor hand-off
/// (`reduction.sendfds`): the `forkserver` start method, the
/// `resource_sharer`, and `Connection` fd transfer all push fds through an
/// AF_UNIX socket this way.
#[cfg(unix)]
fn sock_sendmsg(args: &[Object]) -> Result<Object, RuntimeError> {
    use std::os::raw::c_void;
    let state = state_of(args)?;
    let buffers = extract_iov_buffers(args.get(1))?;
    let ancdata = extract_ancdata(args.get(2))?;
    // `Object::as_i64` also accepts int subclasses (`socket.MsgFlag`
    // IntFlag members — the suite passes `socket.MSG_DONTROUTE`).
    let flags: libc::c_int = match args.get(3) {
        None | Some(Object::None) => 0,
        Some(o) => match o.as_i64() {
            Some(n) => n as libc::c_int,
            None => return Err(type_error("sendmsg: flags must be an integer")),
        },
    };
    let dest: Option<SockAddr> = match args.get(4) {
        None | Some(Object::None) => None,
        some => {
            let family = state.borrow().family;
            Some(parse_sockaddr2(some, family)?)
        }
    };

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

    // Timeout mode: bounded readiness wait first (CPython `sock_call_ex`).
    wait_ready_for_timeout(&state, libc::POLLOUT)?;
    // PEP 475: retry after EINTR (running any tripped Python signal
    // handlers first). EINTR means nothing was committed, so a plain
    // re-issue is safe.
    let sent = loop {
        let ancdata_ref = &ancdata;
        let dest_ref = &dest;
        let sent = crate::gil::allow_threads_then(move || unsafe {
            let mut msg: libc::msghdr = std::mem::zeroed();
            if let Some(d) = dest_ref {
                msg.msg_name = d.as_ptr() as *mut c_void;
                msg.msg_namelen = d.len();
            }
            msg.msg_iov = iov_ptr;
            msg.msg_iovlen = iov_len as _;
            if controllen > 0 {
                msg.msg_control = ctrl_ptr.cast::<c_void>();
                msg.msg_controllen = controllen as _;
                let mut cmsg = libc::CMSG_FIRSTHDR(&raw const msg);
                for (level, ctype, data) in ancdata_ref {
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
                    cmsg = libc::CMSG_NXTHDR(&raw const msg, cmsg);
                }
            }
            libc::sendmsg(fd, &raw const msg, flags)
        });
        if sent < 0 {
            let err = std::io::Error::last_os_error();
            if err.raw_os_error() == Some(libc::EINTR) {
                run_pending_signals_after_eintr()?;
                continue;
            }
            return Err(io_error_to_py(&err));
        }
        break sent;
    };
    Ok(Object::Int(sent as i64))
}

/// Shared engine for `recvmsg`/`recvmsg_into`: one bounded `recvmsg(2)`
/// into `databuf` (whole buffer as a single iovec — the kernel fills
/// scatter buffers in order, so distributing afterwards is equivalent),
/// returning `(nbytes, ancdata, msg_flags, address)` parts.
#[cfg(unix)]
#[allow(clippy::type_complexity)]
fn recvmsg_engine(
    state: &Rc<RefCell<SocketState>>,
    databuf: &mut [u8],
    ancbufsize: usize,
    flags: libc::c_int,
) -> Result<(usize, Vec<Object>, i64, Object), RuntimeError> {
    use std::os::raw::c_void;
    let mut control = vec![0u8; ancbufsize];
    let fd = snapshot_raw_fd(state)?;

    let bufsize = databuf.len();
    let mut iov = [libc::iovec {
        iov_base: databuf.as_mut_ptr().cast::<c_void>(),
        iov_len: bufsize,
    }];
    let iov_ptr = iov.as_mut_ptr();
    let ctrl_ptr = if ancbufsize > 0 {
        control.as_mut_ptr()
    } else {
        std::ptr::null_mut()
    };

    // Timeout mode: bounded readiness wait first (CPython `sock_call_ex`
    // — testRecvmsgTimeout asserts the TimeoutError).
    wait_ready_for_timeout(state, libc::POLLIN)?;

    // PEP 475: a signal-interrupted recvmsg (EINTR) must run pending Python
    // signal handlers and retry rather than raising InterruptedError. The
    // forkserver's listener loop (`multiprocessing.forkserver.main`) sits in
    // `reduction.recvfds` and takes SIGCHLD every time a forked worker exits;
    // surfacing EINTR there killed the whole forkserver, and every later
    // `Pool._repopulate_pool` in the parent then died with BrokenPipeError /
    // "did not receive acknowledgement of fd" and the pool hung.
    let mut name_storage: libc::sockaddr_storage = unsafe { std::mem::zeroed() };
    let name_ptr = std::ptr::addr_of_mut!(name_storage);
    let (n, msg_flags, used_controllen, namelen) = loop {
        let res = crate::gil::allow_threads_then(move || unsafe {
            let mut msg: libc::msghdr = std::mem::zeroed();
            msg.msg_name = name_ptr.cast::<c_void>();
            msg.msg_namelen = std::mem::size_of::<libc::sockaddr_storage>() as _;
            msg.msg_iov = iov_ptr;
            msg.msg_iovlen = 1 as _;
            if ancbufsize > 0 {
                msg.msg_control = ctrl_ptr.cast::<c_void>();
                msg.msg_controllen = ancbufsize as _;
            }
            let n = libc::recvmsg(fd, &raw mut msg, flags);
            (
                n,
                i64::from(msg.msg_flags),
                msg.msg_controllen as usize,
                msg.msg_namelen,
            )
        });
        if res.0 < 0 {
            let err = std::io::Error::last_os_error();
            if err.raw_os_error() == Some(libc::EINTR) {
                run_pending_signals_after_eintr()?;
                continue;
            }
            return Err(io_error_to_py(&err));
        }
        break res;
    };

    let mut ancdata_items: Vec<Object> = Vec::new();
    if ancbufsize > 0 && used_controllen > 0 {
        unsafe {
            let mut msg: libc::msghdr = std::mem::zeroed();
            msg.msg_control = control.as_mut_ptr().cast::<c_void>();
            msg.msg_controllen = used_controllen as _;
            let control_base = control.as_ptr() as usize;
            let mut cmsg = libc::CMSG_FIRSTHDR(&raw const msg);
            while !cmsg.is_null() {
                let level = (*cmsg).cmsg_level;
                let ctype = (*cmsg).cmsg_type;
                let cmsg_len = (*cmsg).cmsg_len as usize;
                let data_ptr = libc::CMSG_DATA(cmsg);
                // Payload length = cmsg_len minus the (aligned) header up to
                // CMSG_DATA — never `CMSG_LEN(0)`, which omits the alignment.
                let data_offset = (data_ptr as usize).saturating_sub(cmsg as usize);
                let mut data_len = cmsg_len.saturating_sub(data_offset);
                // On MSG_CTRUNC the kernel reports the *untruncated* cmsg_len
                // but only `msg_controllen` bytes of buffer are valid —
                // CPython's `get_cmsg_data_len` clamps the payload to what
                // fits and stops the walk (the CmsgTrunc* family asserts the
                // partial byte counts; reading past the window returned
                // garbage that the SCM_RIGHTS tests then close()d as fds).
                let data_start = (data_ptr as usize).saturating_sub(control_base);
                let avail = used_controllen.saturating_sub(data_start);
                let truncated = data_len > avail;
                if truncated {
                    data_len = avail;
                }
                let data = std::slice::from_raw_parts(data_ptr.cast::<u8>(), data_len).to_vec();
                ancdata_items.push(Object::new_tuple(vec![
                    Object::Int(i64::from(level)),
                    Object::Int(i64::from(ctype)),
                    Object::new_bytes(data),
                ]));
                if truncated {
                    break;
                }
                cmsg = libc::CMSG_NXTHDR(&raw const msg, cmsg);
            }
        }
    }

    // The source address (CPython `makesockaddr`): a datagram's sender,
    // or `None` when the kernel reported no name (connected stream).
    let address = if namelen > 0 {
        let family = state.borrow().family;
        let addr = unsafe { SockAddr::new(name_storage, namelen) };
        sockaddr_to_tuple(&addr, family)
    } else {
        Object::None
    };

    Ok((n as usize, ancdata_items, msg_flags, address))
}

/// `socket.recvmsg(bufsize[, ancbufsize[, flags]])` — receive normal data
/// plus ancillary data, returning `(data, ancdata, msg_flags, address)`.
/// `ancdata` is a list of `(cmsg_level, cmsg_type, cmsg_data)` triples (the
/// shape `multiprocessing.reduction.recvfds` decodes).
#[cfg(unix)]
fn sock_recvmsg(args: &[Object]) -> Result<Object, RuntimeError> {
    let state = state_of(args)?;
    let bufsize = match args.get(1).and_then(Object::as_i64) {
        Some(n) if n >= 0 => n as usize,
        Some(_) => return Err(value_error("negative buffersize in recvmsg")),
        None => return Err(type_error("recvmsg: bufsize must be an integer")),
    };
    let ancbufsize = match args.get(2) {
        None | Some(Object::None) => 0usize,
        Some(o) => match o.as_i64() {
            Some(n) if n >= 0 => n as usize,
            Some(_) => return Err(value_error("negative ancillary data buffer size")),
            None => return Err(type_error("recvmsg: ancbufsize must be an integer")),
        },
    };
    let flags: libc::c_int = match args.get(3) {
        None | Some(Object::None) => 0,
        Some(o) => match o.as_i64() {
            Some(n) => n as libc::c_int,
            None => return Err(type_error("recvmsg: flags must be an integer")),
        },
    };

    let mut databuf = vec![0u8; bufsize];
    let (n, ancdata_items, msg_flags, address) =
        recvmsg_engine(&state, &mut databuf, ancbufsize, flags)?;
    databuf.truncate(n);

    Ok(Object::new_tuple(vec![
        Object::new_bytes(databuf),
        Object::new_list(ancdata_items),
        Object::Int(msg_flags),
        address,
    ]))
}

/// `socket.recvmsg_into(buffers[, ancbufsize[, flags]])` — scatter the
/// normal data across the caller's writable buffers (in order), returning
/// `(nbytes, ancdata, msg_flags, address)` (RFC 0068 WS8:
/// test_socket's RecvmsgIntoMixin suites).
#[cfg(unix)]
fn sock_recvmsg_into(args: &[Object]) -> Result<Object, RuntimeError> {
    let state = state_of(args)?;
    // An iterable of writable bytes-like objects.
    let buf_objs: Vec<Object> = match args.get(1) {
        Some(Object::List(l)) => l.borrow().clone(),
        Some(Object::Tuple(t)) => t.to_vec(),
        Some(other) => {
            let ptr = crate::vm_singletons::current_interpreter_ptr()
                .ok_or_else(|| type_error("recvmsg_into() argument 1 must be an iterable"))?;
            // SAFETY: published by the active builtin call on this thread.
            let interp = unsafe { &mut *ptr };
            let it = interp
                .iter_object(other.clone())
                .map_err(|_| type_error("recvmsg_into() argument 1 must be an iterable"))?;
            let mut out = Vec::new();
            while let Some(item) = interp.iter_next_object(it.clone())? {
                out.push(item);
            }
            out
        }
        None => return Err(type_error("recvmsg_into() argument 1 must be an iterable")),
    };
    let mut dests = Vec::with_capacity(buf_objs.len());
    let mut total = 0usize;
    for obj in &buf_objs {
        let (dst, start, len) = super::io::writable_buffer_dst(Some(obj), "recvmsg_into")?;
        total += len;
        dests.push((dst, start, len));
    }
    let ancbufsize = match args.get(2) {
        None | Some(Object::None) => 0usize,
        Some(o) => match o.as_i64() {
            Some(n) if n >= 0 => n as usize,
            Some(_) => return Err(value_error("negative ancillary data buffer size")),
            None => return Err(type_error("recvmsg_into: ancbufsize must be an integer")),
        },
    };
    let flags: libc::c_int = match args.get(3) {
        None | Some(Object::None) => 0,
        Some(o) => match o.as_i64() {
            Some(n) => n as libc::c_int,
            None => return Err(type_error("recvmsg_into: flags must be an integer")),
        },
    };

    let mut databuf = vec![0u8; total];
    let (n, ancdata_items, msg_flags, address) =
        recvmsg_engine(&state, &mut databuf, ancbufsize, flags)?;

    // Distribute the received bytes across the buffers in order.
    let mut off = 0usize;
    for (dst, start, len) in &dests {
        if off >= n {
            break;
        }
        let take = (*len).min(n - off);
        let mut bytes = dst.borrow_mut();
        bytes[*start..*start + take].copy_from_slice(&databuf[off..off + take]);
        off += take;
    }

    Ok(Object::new_tuple(vec![
        Object::Int(n as i64),
        Object::new_list(ancdata_items),
        Object::Int(msg_flags),
        address,
    ]))
}

/// Snapshot the raw fd of `state`, dropping the borrow before the syscall
/// (a peer thread may legitimately `close()` it; we then see `EBADF`).
#[cfg(unix)]
fn snapshot_raw_fd(state: &Rc<RefCell<SocketState>>) -> Result<libc::c_int, RuntimeError> {
    let b = state.borrow();
    let sock = b.inner.as_ref().ok_or_else(closed_socket_error)?;
    let fd = raw_fd_of(sock).ok_or_else(|| os_error("socket has no file descriptor"))?;
    Ok(fd as libc::c_int)
}

/// Windows twin of [`snapshot_raw_fd`]: the raw SOCKET handle as `i64`
/// (callers cast to `winsock::SOCKET` at the wait site).
#[cfg(windows)]
fn snapshot_raw_fd(state: &Rc<RefCell<SocketState>>) -> Result<i64, RuntimeError> {
    let b = state.borrow();
    let sock = b.inner.as_ref().ok_or_else(closed_socket_error)?;
    raw_fd_of(sock).ok_or_else(|| os_error("socket has no file descriptor"))
}

/// Extract the `sendmsg` iovec list — an iterable of bytes-like objects.
#[cfg_attr(not(unix), allow(dead_code))]
fn extract_iov_buffers(arg: Option<&Object>) -> Result<Vec<Vec<u8>>, RuntimeError> {
    let items: Vec<Object> = match arg {
        Some(Object::List(l)) => l.borrow().clone(),
        Some(Object::Tuple(t)) => t.to_vec(),
        // Any other iterable is drained through the interpreter — asyncio's
        // `_SelectorSocketTransport._write_sendmsg` passes
        // `itertools.islice(iter(buffer), SC_IOV_MAX)`, a lazy iterator.
        Some(other) => {
            let ptr = crate::vm_singletons::current_interpreter_ptr().ok_or_else(|| {
                type_error("sendmsg(): argument 1 must be an iterable of bytes-like objects")
            })?;
            // SAFETY: published by the active builtin call on this thread; the
            // GIL keeps the access exclusive.
            let interp = unsafe { &mut *ptr };
            let it = interp.iter_object(other.clone()).map_err(|_| {
                type_error("sendmsg(): argument 1 must be an iterable of bytes-like objects")
            })?;
            let mut out = Vec::new();
            while let Some(item) = interp.iter_next_object(it.clone())? {
                out.push(item);
            }
            out
        }
        None => {
            return Err(type_error(
                "sendmsg(): argument 1 must be an iterable of bytes-like objects",
            ))
        }
    };
    items
        .iter()
        .map(|o| {
            // Buffer-protocol fallback covers `array.array("B", ...)`
            // (test_socket's testSendmsgArray) and other exporters that
            // aren't native bytes/bytearray/memoryview.
            extract_bytes(Some(o))
                .ok()
                .or_else(|| buffer_protocol_bytes(o))
                .ok_or_else(|| {
                    type_error("sendmsg(): argument 1 must be an iterable of bytes-like objects")
                })
        })
        .collect()
}

/// Integer conversion through `__index__`, like the C "i" arg converter:
/// native ints (and IntEnum members) short-circuit, anything else gets its
/// `__index__` called through the interpreter.
#[cfg_attr(not(unix), allow(dead_code))]
fn index_arg(obj: &Object) -> Option<i64> {
    if let Some(n) = obj.as_i64() {
        return Some(n);
    }
    let ptr = crate::vm_singletons::current_interpreter_ptr()?;
    // SAFETY: published by the active builtin call on this thread; the
    // interpreter outlives this call.
    let interp = unsafe { &mut *ptr };
    let method = interp.load_attr_public(obj, "__index__").ok()?;
    interp.call_object(method, &[], &[]).ok()?.as_i64()
}

/// Bytes of an arbitrary buffer-protocol exporter, via its `tobytes()`
/// method (the escape hatch for objects the native `extract_bytes` doesn't
/// know, e.g. `array.array`).
fn buffer_protocol_bytes(obj: &Object) -> Option<Vec<u8>> {
    let ptr = crate::vm_singletons::current_interpreter_ptr()?;
    // SAFETY: published by the active builtin call on this thread; the
    // interpreter outlives this call.
    let interp = unsafe { &mut *ptr };
    let method = interp.load_attr_public(obj, "tobytes").ok()?;
    match interp.call_object(method, &[], &[]).ok()? {
        Object::Bytes(b) => Some(b.to_vec()),
        _ => None,
    }
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
        // Any other iterable is drained through the interpreter — the suite
        // passes a generator expression (testSendmsgAncillaryGenerator).
        Some(other) => {
            let bad_iterable = || {
                type_error("sendmsg(): ancillary data must be an iterable of zero or more triples")
            };
            let ptr = crate::vm_singletons::current_interpreter_ptr().ok_or_else(bad_iterable)?;
            // SAFETY: published by the active builtin call on this thread;
            // the GIL keeps the access exclusive.
            let interp = unsafe { &mut *ptr };
            let it = interp
                .iter_object(other.clone())
                .map_err(|_| bad_iterable())?;
            let mut out = Vec::new();
            while let Some(item) = interp.iter_next_object(it.clone())? {
                out.push(item);
            }
            out
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
        // cmsg_level/cmsg_type go through the C "i" converter, which calls
        // `__index__` on arbitrary objects — the reentrant-mutation test
        // passes an object whose `__index__` clears the ancillary list
        // mid-parse (the list was already snapshotted above, like CPython's
        // PySequence_Fast).
        let level = match index_arg(&triple[0]) {
            Some(n) => n as libc::c_int,
            None => return Err(type_error("sendmsg(): an integer is required (cmsg_level)")),
        };
        let ctype = match index_arg(&triple[1]) {
            Some(n) => n as libc::c_int,
            None => return Err(type_error("sendmsg(): an integer is required (cmsg_type)")),
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

/// Best-effort mirror of the Python-level timeout into
/// `SO_RCVTIMEO`/`SO_SNDTIMEO`. CPython never programs these (the fd is
/// `O_NONBLOCK` and waits happen in poll), but WeavePy's native `_ssl`
/// works on a `dup(2)` of the fd and cannot see the Python-level timeout
/// — the sockopt is the one channel both sides of the dup share, letting
/// the TLS layer distinguish "timeout mode" (poll up to the deadline)
/// from genuine non-blocking (raise `SSLWantRead/WriteError`). Under
/// `O_NONBLOCK` the kernel never consults `SO_*TIMEO`, so this is
/// invisible to socket semantics. Failures (macOS EINVAL on a
/// peer-closed AF_UNIX socket) are ignored.
fn mirror_timeout_sockopts(sock: &Socket, timeout: Option<Duration>) {
    let d = match timeout {
        Some(d) if !d.is_zero() => Some(d),
        _ => None,
    };
    let _ = sock.set_read_timeout(d);
    let _ = sock.set_write_timeout(d);
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
        let sock = s_borrow.inner.as_ref().ok_or_else(closed_socket_error)?;
        sock.set_nonblocking(!flag)
            .map_err(|e| io_error_to_py(&e))?;
        mirror_timeout_sockopts(sock, None);
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
    // CPython: `getblocking() == (gettimeout() != 0)` — a socket in timeout
    // mode still reports blocking; only `settimeout(0)`/`setblocking(False)`
    // is non-blocking.
    let blocking = !matches!(state.borrow().timeout, Some(d) if d.is_zero());
    Ok(Object::Bool(blocking))
}

fn sock_settimeout(args: &[Object]) -> Result<Object, RuntimeError> {
    let state = state_of(args)?;
    let timeout = match args.get(1) {
        None | Some(Object::None) => None,
        Some(Object::Float(f)) => {
            if !f.is_finite() || *f < 0.0 {
                return Err(value_error("Timeout value out of range"));
            }
            Some(Duration::from_secs_f64(*f))
        }
        Some(Object::Int(n)) => {
            if *n < 0 {
                return Err(value_error("Timeout value out of range"));
            }
            Some(Duration::from_secs(*n as u64))
        }
        _ => return Err(type_error("settimeout: arg must be number or None")),
    };
    // CPython's model (socketmodule.c `internal_setblocking`): *any*
    // timeout — zero or positive — puts the fd in O_NONBLOCK; only `None`
    // is a genuinely blocking fd. Positive timeouts are enforced by the
    // poll-based readiness waits in `blocking_socket_io`, never by
    // `SO_RCVTIMEO`/`SO_SNDTIMEO` (which CPython never programs — and which
    // macOS rejects with EINVAL on a peer-closed AF_UNIX socket anyway).
    // NonBlockingTCPTests.testSetBlocking asserts the fd-level flag for
    // every mode via fcntl(F_GETFL).
    {
        let s_borrow = state.borrow();
        let sock = s_borrow.inner.as_ref().ok_or_else(closed_socket_error)?;
        sock.set_nonblocking(timeout.is_some())
            .map_err(|e| io_error_to_py(&e))?;
        mirror_timeout_sockopts(sock, timeout);
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
    let sock = s_borrow.inner.as_ref().ok_or_else(closed_socket_error)?;
    let want = match value {
        Some(Object::Int(n)) => *n,
        Some(Object::Bool(b)) => i64::from(*b),
        _ => 0,
    };
    let sol_socket = libc_sol_socket() as i32;
    // socket2 fast paths for the SOL_SOCKET/TCP options user code reaches
    // for by name; anything else falls through to a raw `setsockopt(2)`
    // so every (level, optname) works — asyncio's dual-stack server sets
    // `(IPPROTO_IPV6, IPV6_V6ONLY)` (RFC 0054 WS2), multicast code sets
    // `IP_ADD_MEMBERSHIP` with packed bytes, etc.
    if level == sol_socket && optname == libc_so_reuseaddr() as i32 {
        sock.set_reuse_address(want != 0)
            .map_err(|e| io_error_to_py(&e))?;
    } else if level == sol_socket && optname == libc_so_reuseport() as i32 {
        #[cfg(unix)]
        sock.set_reuse_port(want != 0)
            .map_err(|e| io_error_to_py(&e))?;
    } else if level == sol_socket && optname == libc_so_keepalive() as i32 {
        sock.set_keepalive(want != 0)
            .map_err(|e| io_error_to_py(&e))?;
    } else if level == sol_socket && optname == libc_so_broadcast() as i32 {
        sock.set_broadcast(want != 0)
            .map_err(|e| io_error_to_py(&e))?;
    } else if level == 6 && optname == 1 {
        // TCP_NODELAY (level IPPROTO_TCP/SOL_TCP — 6 on every platform).
        sock.set_nodelay(want != 0)
            .map_err(|e| io_error_to_py(&e))?;
    } else if level == sol_socket && optname == libc_so_sndbuf() as i32 {
        sock.set_send_buffer_size(want as usize)
            .map_err(|e| io_error_to_py(&e))?;
    } else if level == sol_socket && optname == libc_so_rcvbuf() as i32 {
        sock.set_recv_buffer_size(want as usize)
            .map_err(|e| io_error_to_py(&e))?;
    } else {
        // Raw passthrough — every exported constant now carries the real
        // platform numbering (including the SOL_SOCKET level), so unknown
        // (level, optname) pairs reach libc verbatim. POSIX-only: the
        // Windows libc crate exposes no setsockopt surface, so unknown
        // options stay a no-op there (the pre-RFC-0054 behavior).
        #[cfg(unix)]
        {
            let fd = raw_fd_of(sock).ok_or_else(closed_socket_error)? as i32;
            let rc = match value {
                Some(Object::Bytes(b)) => unsafe {
                    libc::setsockopt(
                        fd,
                        level,
                        optname,
                        b.as_ptr().cast(),
                        b.len() as libc::socklen_t,
                    )
                },
                Some(Object::ByteArray(b)) => {
                    let buf = b.borrow();
                    unsafe {
                        libc::setsockopt(
                            fd,
                            level,
                            optname,
                            buf.as_ptr().cast(),
                            buf.len() as libc::socklen_t,
                        )
                    }
                }
                _ => {
                    let v = want as libc::c_int;
                    unsafe {
                        libc::setsockopt(
                            fd,
                            level,
                            optname,
                            std::ptr::addr_of!(v).cast(),
                            std::mem::size_of::<libc::c_int>() as libc::socklen_t,
                        )
                    }
                }
            };
            if rc != 0 {
                return Err(io_error_to_py(&std::io::Error::last_os_error()));
            }
        }
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
    let sock = s_borrow.inner.as_ref().ok_or_else(closed_socket_error)?;
    let as_int = |b: bool| Object::Int(i64::from(b));
    let sol_socket = libc_sol_socket() as i32;
    // TCP_NODELAY lives at the IPPROTO_TCP/SOL_TCP level (6); disambiguate
    // it from SOL_SOCKET options that share the numeric optname 1.
    if level == 6 && optname == 1 {
        return Ok(as_int(sock.nodelay().map_err(|e| io_error_to_py(&e))?));
    }
    #[cfg(unix)]
    if level == sol_socket && optname == libc::SO_ERROR {
        // SO_ERROR — return last error number, or 0.
        let err = sock.take_error().ok().flatten();
        return Ok(Object::Int(
            err.map_or(0, |e| i64::from(e.raw_os_error().unwrap_or(0))),
        ));
    }
    #[cfg(unix)]
    if level == sol_socket && optname == libc::SO_TYPE {
        // SO_TYPE — return our recorded kind.
        return Ok(Object::Int(i64::from(s_borrow.kind)));
    }
    // Read back the SO_* options we know how to set, so a
    // setsockopt/getsockopt round-trip reflects reality (CPython parity;
    // asyncio's `_set_nodelay` and several transport tests rely on this).
    if level == sol_socket && optname == libc_so_reuseaddr() as i32 {
        return Ok(as_int(
            sock.reuse_address().map_err(|e| io_error_to_py(&e))?,
        ));
    }
    #[cfg(unix)]
    if level == sol_socket && optname == libc_so_reuseport() as i32 {
        return Ok(as_int(sock.reuse_port().map_err(|e| io_error_to_py(&e))?));
    }
    if level == sol_socket && optname == libc_so_keepalive() as i32 {
        return Ok(as_int(sock.keepalive().map_err(|e| io_error_to_py(&e))?));
    }
    if level == sol_socket && optname == libc_so_broadcast() as i32 {
        return Ok(as_int(sock.broadcast().map_err(|e| io_error_to_py(&e))?));
    }
    if level == sol_socket && optname == libc_so_sndbuf() as i32 {
        return Ok(Object::Int(
            sock.send_buffer_size().map_err(|e| io_error_to_py(&e))? as i64,
        ));
    }
    if level == sol_socket && optname == libc_so_rcvbuf() as i32 {
        return Ok(Object::Int(
            sock.recv_buffer_size().map_err(|e| io_error_to_py(&e))? as i64,
        ));
    }
    // Everything else reads back through a raw `getsockopt(2)` — the module
    // now exports real platform numbering for every level including
    // SOL_SOCKET, so the passthrough is safe (asyncio's dual-stack
    // `create_server` verifies `IPV6_V6ONLY` this way).
    #[cfg(unix)]
    {
        let fd = raw_fd_of(sock).ok_or_else(closed_socket_error)? as i32;
        // An explicit buflen argument requests the raw bytes form.
        if let Some(buflen) = args.get(3).and_then(Object::as_i64) {
            if !(0..=1024).contains(&buflen) {
                return Err(value_error("getsockopt buflen out of range"));
            }
            let mut buf = vec![0u8; buflen as usize];
            let mut len = buflen as libc::socklen_t;
            let rc = unsafe {
                libc::getsockopt(fd, level, optname, buf.as_mut_ptr().cast(), &raw mut len)
            };
            if rc != 0 {
                return Err(io_error_to_py(&std::io::Error::last_os_error()));
            }
            buf.truncate(len as usize);
            return Ok(Object::new_bytes(buf));
        }
        let mut v: libc::c_int = 0;
        let mut len = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
        let rc = unsafe {
            libc::getsockopt(
                fd,
                level,
                optname,
                std::ptr::addr_of_mut!(v).cast(),
                &raw mut len,
            )
        };
        if rc == 0 {
            return Ok(Object::Int(i64::from(v)));
        }
        Err(io_error_to_py(&std::io::Error::last_os_error()))
    }
    // For anything else, return 0 as a safe default.
    #[cfg(not(unix))]
    Ok(Object::Int(0))
}

fn sock_getsockname(args: &[Object]) -> Result<Object, RuntimeError> {
    let state = state_of(args)?;
    let s_borrow = state.borrow();
    let sock = s_borrow.inner.as_ref().ok_or_else(closed_socket_error)?;
    let addr = sock.local_addr().map_err(|e| io_error_to_py(&e))?;
    Ok(sockaddr_to_tuple(&addr, s_borrow.family))
}

fn sock_getpeername(args: &[Object]) -> Result<Object, RuntimeError> {
    let state = state_of(args)?;
    let s_borrow = state.borrow();
    let sock = s_borrow.inner.as_ref().ok_or_else(closed_socket_error)?;
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
    // -1 is the closed marker. Other negative handles are synthetic
    // registry keys (fd shared with another socket object) whose real fd
    // still comes from the state's inner socket below.
    if handle == -1 {
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

/// PEP 446 inheritability, read through `FD_CLOEXEC` (inheritable ==
/// *not* close-on-exec), exactly like `os.get_inheritable`.
#[cfg(unix)]
fn sock_get_inheritable(args: &[Object]) -> Result<Object, RuntimeError> {
    let state = state_of(args)?;
    let borrow = state.borrow();
    let sock = borrow.inner.as_ref().ok_or_else(closed_socket_error)?;
    let fd = raw_fd_of(sock).ok_or_else(closed_socket_error)?;
    let flags = unsafe { libc::fcntl(fd as i32, libc::F_GETFD) };
    if flags < 0 {
        return Err(io_error_to_py(&std::io::Error::last_os_error()));
    }
    Ok(Object::Bool(flags & libc::FD_CLOEXEC == 0))
}

#[cfg(unix)]
fn sock_set_inheritable(args: &[Object]) -> Result<Object, RuntimeError> {
    let state = state_of(args)?;
    let inheritable = args
        .get(1)
        .is_some_and(super::super::object::Object::is_truthy);
    let borrow = state.borrow();
    let sock = borrow.inner.as_ref().ok_or_else(closed_socket_error)?;
    let fd = raw_fd_of(sock).ok_or_else(closed_socket_error)?;
    let flags = unsafe { libc::fcntl(fd as i32, libc::F_GETFD) };
    if flags < 0 {
        return Err(io_error_to_py(&std::io::Error::last_os_error()));
    }
    let new_flags = if inheritable {
        flags & !libc::FD_CLOEXEC
    } else {
        flags | libc::FD_CLOEXEC
    };
    if unsafe { libc::fcntl(fd as i32, libc::F_SETFD, new_flags) } < 0 {
        return Err(io_error_to_py(&std::io::Error::last_os_error()));
    }
    Ok(Object::None)
}

/// PEP 446 inheritability on Windows: a SOCKET is a kernel HANDLE, so the
/// inheritable bit is `HANDLE_FLAG_INHERIT` read/written through
/// `GetHandleInformation`/`SetHandleInformation` — exactly CPython's
/// `sock_get_inheritable`/`sock_set_inheritable` (socketmodule.c).
#[cfg(windows)]
fn sock_get_inheritable(args: &[Object]) -> Result<Object, RuntimeError> {
    use windows_sys::Win32::Foundation::{GetHandleInformation, HANDLE_FLAG_INHERIT};
    let state = state_of(args)?;
    let handle = {
        let b = state.borrow();
        let sock = b.inner.as_ref().ok_or_else(closed_socket_error)?;
        raw_fd_of(sock).ok_or_else(closed_socket_error)?
    };
    let mut flags = 0u32;
    let ok =
        unsafe { GetHandleInformation(handle as usize as *mut std::ffi::c_void, &raw mut flags) };
    if ok == 0 {
        return Err(crate::stdlib::nt_support::last_win32_error_to_py(None));
    }
    Ok(Object::Bool(flags & HANDLE_FLAG_INHERIT != 0))
}

#[cfg(windows)]
fn sock_set_inheritable(args: &[Object]) -> Result<Object, RuntimeError> {
    use windows_sys::Win32::Foundation::{SetHandleInformation, HANDLE_FLAG_INHERIT};
    let state = state_of(args)?;
    let inheritable = args
        .get(1)
        .is_some_and(super::super::object::Object::is_truthy);
    let handle = {
        let b = state.borrow();
        let sock = b.inner.as_ref().ok_or_else(closed_socket_error)?;
        raw_fd_of(sock).ok_or_else(closed_socket_error)?
    };
    let ok = unsafe {
        SetHandleInformation(
            handle as usize as *mut std::ffi::c_void,
            HANDLE_FLAG_INHERIT,
            if inheritable { HANDLE_FLAG_INHERIT } else { 0 },
        )
    };
    if ok == 0 {
        return Err(crate::stdlib::nt_support::last_win32_error_to_py(None));
    }
    Ok(Object::None)
}

/// Stub for targets with neither `FD_CLOEXEC` nor Win32 handles. CPython
/// creates sockets non-inheritable by default, so report that.
#[cfg(not(any(unix, windows)))]
fn sock_get_inheritable(args: &[Object]) -> Result<Object, RuntimeError> {
    let _ = state_of(args)?;
    Ok(Object::Bool(false))
}

#[cfg(not(any(unix, windows)))]
fn sock_set_inheritable(args: &[Object]) -> Result<Object, RuntimeError> {
    let _ = state_of(args)?;
    Ok(Object::None)
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
    let sock = s_borrow.inner.as_ref().ok_or_else(closed_socket_error)?;
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

/// Duplicate a raw socket descriptor. POSIX: a real `dup(2)`. Windows:
/// CPython's `dup_socket` (socketmodule.c) — a SOCKET is *not* a CRT fd,
/// so the duplicate goes through `WSADuplicateSocketW` into a
/// `WSAPROTOCOL_INFOW` consumed by `WSASocketW(FROM_PROTOCOL_INFO, …)`,
/// created non-inheritable (`WSA_FLAG_NO_HANDLE_INHERIT`, PEP 446).
#[cfg(unix)]
fn dup_raw_fd(fd: i64) -> Result<i64, RuntimeError> {
    let dup = unsafe { libc::dup(fd as i32) };
    if dup < 0 {
        return Err(io_error_to_py(&std::io::Error::last_os_error()));
    }
    Ok(i64::from(dup))
}

#[cfg(windows)]
fn dup_raw_fd(fd: i64) -> Result<i64, RuntimeError> {
    use windows_sys::Win32::Networking::WinSock as ws;
    let mut info: ws::WSAPROTOCOL_INFOW = unsafe { std::mem::zeroed() };
    let pid = unsafe { windows_sys::Win32::System::Threading::GetCurrentProcessId() };
    let rc = unsafe { ws::WSADuplicateSocketW(fd as ws::SOCKET, pid, &raw mut info) };
    if rc != 0 {
        return Err(crate::stdlib::nt_support::win32_error_to_py(
            unsafe { ws::WSAGetLastError() },
            None,
        ));
    }
    let dup = unsafe {
        ws::WSASocketW(
            ws::FROM_PROTOCOL_INFO,
            ws::FROM_PROTOCOL_INFO,
            ws::FROM_PROTOCOL_INFO,
            &raw const info,
            0,
            ws::WSA_FLAG_NO_HANDLE_INHERIT,
        )
    };
    if dup == ws::INVALID_SOCKET {
        return Err(crate::stdlib::nt_support::win32_error_to_py(
            unsafe { ws::WSAGetLastError() },
            None,
        ));
    }
    Ok(dup as i64)
}

/// `socket.dup()` — duplicate the underlying descriptor (see
/// [`dup_raw_fd`]) and wrap it in a fresh `socket` object that shares the
/// family/type/proto. The duplicate is independent: closing one leaves
/// the other usable, matching CPython's `socket.dup()`.
#[cfg(any(unix, windows))]
fn sock_dup(args: &[Object]) -> Result<Object, RuntimeError> {
    let state = state_of(args)?;
    let (family, kind, proto) = {
        let s = state.borrow();
        (s.family, s.kind, s.proto)
    };
    let new_fd = {
        let b = state.borrow();
        let sock = b.inner.as_ref().ok_or_else(closed_socket_error)?;
        let fd = raw_fd_of(sock).ok_or_else(|| os_error("socket has no file descriptor"))?;
        dup_raw_fd(fd)?
    };
    let inner = wrap_fd_socket(new_fd)?;
    let new_state = Rc::new(RefCell::new(SocketState {
        inner: Some(inner),
        family,
        kind,
        proto,
        timeout: None,
        blocking: true,
        owns_fd: true,
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

/// No descriptor-duplication primitive on other targets.
#[cfg(not(any(unix, windows)))]
fn sock_dup(args: &[Object]) -> Result<Object, RuntimeError> {
    let _ = state_of(args)?;
    Err(os_error("socket.dup is not supported on this platform"))
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
    let dict = Rc::new(RefCell::new(DictData::default()));
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
        // CPython's `getsockaddrarg` clamps the port to 0-65535 and raises
        // OverflowError otherwise (test_socketserver's `test_tcpserver_bind_leak`
        // binds port -1 expecting exactly this), rather than wrapping `as u16`.
        Some(Object::Int(n)) => {
            if *n < 0 || *n > 65535 {
                return Err(overflow_error("getsockaddrarg: port must be 0-65535."));
            }
            *n as u16
        }
        Some(Object::Long(_)) => {
            return Err(overflow_error("getsockaddrarg: port must be 0-65535."))
        }
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
    // The optional IPv6 4-tuple members. CPython's getsockaddrarg bounds
    // flowinfo to the 20-bit field and raises OverflowError otherwise
    // (test_flowinfo binds with flowinfo=-10 expecting exactly that).
    if tup.len() > 2 {
        let Some(flowinfo) = tup.get(2).and_then(Object::as_i64) else {
            return Err(type_error("address flowinfo must be int"));
        };
        if !(0..=0xfffff).contains(&flowinfo) {
            return Err(overflow_error(
                "getsockaddrarg: flowinfo must be 0-1048575.",
            ));
        }
        let flowinfo = flowinfo as u32;
        let scope_id = tup.get(3).and_then(Object::as_i64).unwrap_or(0) as u32;
        if let SocketAddr::V6(v6) = parsed {
            let mut v6 = v6;
            v6.set_flowinfo(flowinfo);
            if tup.len() > 3 {
                v6.set_scope_id(scope_id);
            }
            return Ok(SocketAddr::V6(v6));
        }
    }
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
            // Lone surrogates carry raw filesystem bytes (PEP 383):
            // `b.decode('ascii', 'surrogateescape')` round-trips through
            // the fs codec back to the original bytes (testSurrogateescapeBind).
            Some(w @ Object::WStr(_)) => {
                crate::stdlib::codecs_mod::encode_obj(w, "utf-8", "surrogateescape")?
            }
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

fn module_functions() -> Vec<(&'static str, fn(&[Object]) -> Result<Object, RuntimeError>)> {
    #[cfg_attr(not(unix), allow(unused_mut))]
    let mut fns: Vec<(&'static str, fn(&[Object]) -> Result<Object, RuntimeError>)> = vec![
        ("gethostname", mod_gethostname),
        ("gethostbyname", mod_gethostbyname),
        ("gethostbyname_ex", mod_gethostbyname_ex),
        ("gethostbyaddr", mod_gethostbyaddr),
        ("getservbyname", mod_getservbyname),
        ("getservbyport", mod_getservbyport),
        ("getaddrinfo", mod_getaddrinfo),
        ("getnameinfo", mod_getnameinfo),
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
        // The verbatim `socket.py` (RFC 0068 WS8) builds `sock.dup()` and
        // fd-taking `socket.close(fd)` from these module-level primitives.
        ("dup", mod_dup),
        ("close", mod_close_fd),
        ("getprotobyname", mod_getprotobyname),
    ];
    #[cfg(unix)]
    fns.extend([
        (
            "if_nameindex",
            mod_if_nameindex as fn(&[Object]) -> Result<Object, RuntimeError>,
        ),
        ("if_nametoindex", mod_if_nametoindex),
        ("if_indextoname", mod_if_indextoname),
    ]);
    // Ancillary-data sizing helpers (functions, not constants, exactly
    // like CPython's `socket` module). Needed by `reduction.recvfds`.
    // Absent on Windows, matching CPython's `#ifdef CMSG_LEN` gating.
    #[cfg(unix)]
    fns.extend([
        (
            "CMSG_LEN",
            mod_cmsg_len as fn(&[Object]) -> Result<Object, RuntimeError>,
        ),
        ("CMSG_SPACE", mod_cmsg_space),
    ]);
    fns
}

/// `_socket.dup(fd)` — duplicate a socket descriptor (non-inheritable,
/// like CPython's `_socket.dup`, which uses `F_DUPFD_CLOEXEC`).
fn mod_dup(args: &[Object]) -> Result<Object, RuntimeError> {
    let Some(fd) = args.first().and_then(Object::as_i64) else {
        return Err(type_error("an integer is required"));
    };
    #[cfg(unix)]
    {
        let new = unsafe { libc::fcntl(fd as libc::c_int, libc::F_DUPFD_CLOEXEC, 0) };
        if new < 0 {
            return Err(io_error_to_py(&std::io::Error::last_os_error()));
        }
        Ok(Object::Int(i64::from(new)))
    }
    #[cfg(not(unix))]
    {
        let _ = fd;
        Err(os_error("dup is not supported on this platform"))
    }
}

/// `_socket.close(fd)` — close a socket descriptor, swallowing `ECONNRESET`
/// (CPython's `sock_close`; `EBADF` and friends still raise).
fn mod_close_fd(args: &[Object]) -> Result<Object, RuntimeError> {
    let Some(fd) = args.first().and_then(Object::as_i64) else {
        return Err(type_error("an integer is required"));
    };
    #[cfg(unix)]
    {
        let r = unsafe { libc::close(fd as libc::c_int) };
        if r < 0 {
            let err = std::io::Error::last_os_error();
            if err.raw_os_error() != Some(libc::ECONNRESET) {
                return Err(io_error_to_py(&err));
            }
        }
        Ok(Object::None)
    }
    #[cfg(not(unix))]
    {
        let _ = fd;
        Err(os_error("close is not supported on this platform"))
    }
}

/// `socket.getprotobyname(name)` — protocol number from `/etc/protocols`.
fn mod_getprotobyname(args: &[Object]) -> Result<Object, RuntimeError> {
    let Some(Object::Str(name)) = args.first() else {
        return Err(type_error("getprotobyname() argument must be str"));
    };
    #[cfg(unix)]
    {
        let c = std::ffi::CString::new(name.as_bytes())
            .map_err(|_| value_error("embedded null character"))?;
        let ent = unsafe { libc::getprotobyname(c.as_ptr()) };
        if ent.is_null() {
            return Err(os_error("protocol not found"));
        }
        Ok(Object::Int(i64::from(unsafe { (*ent).p_proto })))
    }
    #[cfg(not(unix))]
    {
        match name.as_ref() {
            "tcp" => Ok(Object::Int(6)),
            "udp" => Ok(Object::Int(17)),
            "icmp" => Ok(Object::Int(1)),
            _ => Err(os_error("protocol not found")),
        }
    }
}

/// `socket.if_nameindex()` — list of `(index, name)` for the interfaces.
#[cfg(unix)]
fn mod_if_nameindex(_args: &[Object]) -> Result<Object, RuntimeError> {
    let head = unsafe { libc::if_nameindex() };
    if head.is_null() {
        return Err(io_error_to_py(&std::io::Error::last_os_error()));
    }
    let mut out = Vec::new();
    let mut cur = head;
    unsafe {
        while (*cur).if_index != 0 && !(*cur).if_name.is_null() {
            let name = CStr::from_ptr((*cur).if_name)
                .to_string_lossy()
                .into_owned();
            out.push(Object::new_tuple(vec![
                Object::Int(i64::from((*cur).if_index)),
                Object::from_str(name),
            ]));
            cur = cur.add(1);
        }
        libc::if_freenameindex(head);
    }
    Ok(Object::new_list(out))
}

/// `socket.if_nametoindex(name)`.
#[cfg(unix)]
fn mod_if_nametoindex(args: &[Object]) -> Result<Object, RuntimeError> {
    let Some(Object::Str(name)) = args.first() else {
        return Err(type_error("if_nametoindex() argument must be str"));
    };
    let c = std::ffi::CString::new(name.as_bytes())
        .map_err(|_| value_error("embedded null character"))?;
    let idx = unsafe { libc::if_nametoindex(c.as_ptr()) };
    if idx == 0 {
        // The failure OSError must carry an errno
        // (testInvalidInterfaceNameToIndex asserts `.errno is not None`);
        // if_nametoindex(3) leaves ENXIO/ENODEV in errno on BSDs and Linux.
        return Err(io_error_to_py(&std::io::Error::last_os_error()));
    }
    Ok(Object::Int(i64::from(idx)))
}

/// `socket.if_indextoname(index)`.
#[cfg(unix)]
fn mod_if_indextoname(args: &[Object]) -> Result<Object, RuntimeError> {
    // The index converts as a C unsigned int: a negative value or one that
    // doesn't fit 32 bits is an OverflowError, while a non-int is a
    // TypeError (testInvalidInterfaceIndexToName probes -1, 2**1000, and
    // '_DEADBEEF' separately).
    let arg = args.first();
    let idx = match arg.and_then(Object::as_i64) {
        Some(n) if (0..=i64::from(u32::MAX)).contains(&n) => n,
        Some(_) => {
            return Err(crate::error::overflow_error(
                "if_indextoname() argument out of range",
            ))
        }
        None if matches!(arg, Some(Object::Long(_))) => {
            return Err(crate::error::overflow_error(
                "if_indextoname() argument out of range",
            ))
        }
        None => return Err(type_error("if_indextoname() argument must be int")),
    };
    let mut buf = [0u8; libc::IF_NAMESIZE];
    let r = unsafe { libc::if_indextoname(idx as libc::c_uint, buf.as_mut_ptr().cast()) };
    if r.is_null() {
        return Err(io_error_to_py(&std::io::Error::last_os_error()));
    }
    let name = unsafe { CStr::from_ptr(buf.as_ptr().cast()) }
        .to_string_lossy()
        .into_owned();
    Ok(Object::from_str(name))
}

/// `socket.CMSG_LEN(length)` — bytes an ancillary-data item of `length`
/// payload occupies, including the `cmsghdr` (but not the trailing pad).
#[cfg(unix)]
fn mod_cmsg_len(args: &[Object]) -> Result<Object, RuntimeError> {
    // CPython's get_CMSG_LEN: the result must fit SOCKLEN_T_LIMIT
    // (INT_MAX), so the payload cap is INT_MAX - CMSG_LEN(0).
    let limit = i64::from(libc::c_int::MAX) - i64::from(unsafe { libc::CMSG_LEN(0) });
    let length = cmsg_size_arg(args.first(), limit)?;
    Ok(Object::Int(i64::from(unsafe { libc::CMSG_LEN(length) })))
}

/// `socket.CMSG_SPACE(length)` — bytes to allocate in a control buffer for
/// one ancillary-data item of `length` payload, including alignment pad.
#[cfg(unix)]
fn mod_cmsg_space(args: &[Object]) -> Result<Object, RuntimeError> {
    // CPython's get_CMSG_SPACE caps at INT_MAX - CMSG_SPACE(1) — SPACE(1)
    // rather than SPACE(0) accounts for padding before *and* after the
    // payload (CmsgMacroTests.testCMSG_SPACE probes the exact boundary).
    let limit = i64::from(libc::c_int::MAX) - i64::from(unsafe { libc::CMSG_SPACE(1) });
    let length = cmsg_size_arg(args.first(), limit)?;
    Ok(Object::Int(i64::from(unsafe { libc::CMSG_SPACE(length) })))
}

#[cfg(unix)]
fn cmsg_size_arg(arg: Option<&Object>, limit: i64) -> Result<u32, RuntimeError> {
    // Out-of-range values — negatives, huge ints, anything whose result
    // would overflow a socklen_t — are an *OverflowError* (CmsgMacroTests
    // probes -1, the exact limit + 1, and sys.maxsize).
    let out_of_range = || crate::error::overflow_error("CMSG_LEN() argument out of range");
    let n = match arg.and_then(Object::as_i64) {
        Some(n) => n,
        None if matches!(arg, Some(Object::Long(_))) => return Err(out_of_range()),
        None => return Err(type_error("an integer is required")),
    };
    if n < 0 || n > limit {
        return Err(out_of_range());
    }
    Ok(n as u32)
}

/// `gethostname()` over the real libc call. urllib's `file://` handler
/// compares `gethostbyname(gethostname())` against the URL's host to
/// decide "local file" — an env-var placeholder here made every
/// non-`localhost` file URL look remote (test_urllib2.HandlerTests).
#[cfg(unix)]
fn mod_gethostname(_args: &[Object]) -> Result<Object, RuntimeError> {
    crate::stdlib::sys::audit_event("socket.gethostname", &[])?;
    let mut buf = [0_u8; 1024];
    let rc = unsafe { libc::gethostname(buf.as_mut_ptr().cast::<libc::c_char>(), buf.len()) };
    if rc != 0 {
        return Err(os_error("gethostname failed"));
    }
    buf[buf.len() - 1] = 0;
    let cstr = unsafe { std::ffi::CStr::from_ptr(buf.as_ptr().cast::<libc::c_char>()) };
    Ok(Object::from_str(cstr.to_string_lossy().into_owned()))
}

/// Non-POSIX fallback (the Windows libc crate has no `gethostname`):
/// the environment's machine name, like the pre-RFC-0054 placeholder.
#[cfg(not(unix))]
fn mod_gethostname(_args: &[Object]) -> Result<Object, RuntimeError> {
    crate::stdlib::sys::audit_event("socket.gethostname", &[])?;
    let name = std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "localhost".to_string());
    Ok(Object::from_str(name))
}

/// `gethostbyname(name)` → IPv4 dotted-quad. CPython's implementation
/// resolves with `AF_INET` hints, so IPv6-only answers (`localhost` →
/// `::1` first on macOS) must be filtered out, not returned.
fn mod_gethostbyname(args: &[Object]) -> Result<Object, RuntimeError> {
    let name = match args.first() {
        Some(Object::Str(s)) => s.to_string(),
        _ => return Err(type_error("gethostbyname: arg must be str")),
    };
    let addrs = (name.as_str(), 0_u16)
        .to_socket_addrs()
        .map_err(|e| io_error_to_py(&e))?;
    for addr in addrs {
        if let SocketAddr::V4(v4) = addr {
            return Ok(Object::from_str(v4.ip().to_string()));
        }
    }
    Err(os_error("name resolution failed"))
}

/// Optional `proto` argument shared by `getservbyname`/`getservbyport`
/// (`"tcp"`/`"udp"` or omitted/`None`).
fn servby_proto(
    arg: Option<&Object>,
    who: &str,
) -> Result<Option<std::ffi::CString>, RuntimeError> {
    match arg {
        None | Some(Object::None) => Ok(None),
        Some(Object::Str(s)) => std::ffi::CString::new(s.to_string())
            .map(Some)
            .map_err(|_| value_error(format!("{who}: embedded null character"))),
        _ => Err(type_error(format!("{who}() argument must be str or None"))),
    }
}

/// `getservbyname(servicename[, protocolname])` → port number, via the
/// platform's `/etc/services` (or `%SystemRoot%\…\services`) lookup. CPython
/// holds the GIL across the (static-buffered, non-reentrant) call; so do we.
fn mod_getservbyname(args: &[Object]) -> Result<Object, RuntimeError> {
    let name = match args.first() {
        Some(Object::Str(s)) => s.to_string(),
        _ => return Err(type_error("getservbyname() argument 1 must be str")),
    };
    let c_name = std::ffi::CString::new(name)
        .map_err(|_| value_error("getservbyname: embedded null character"))?;
    let proto = servby_proto(args.get(1), "getservbyname")?;
    let port = match servbyname_lookup(&c_name, proto.as_deref()) {
        Some(p) => p,
        None => return Err(os_error("service/proto not found")),
    };
    Ok(Object::Int(i64::from(port)))
}

/// `getservbyport(port[, protocolname])` → service name. `port` must be a
/// 0–65535 `int` (CPython raises `OverflowError` otherwise).
fn mod_getservbyport(args: &[Object]) -> Result<Object, RuntimeError> {
    let port = match args.first() {
        Some(Object::Int(n)) => *n,
        _ => return Err(type_error("getservbyport() argument 1 must be int")),
    };
    if !(0..=65535).contains(&port) {
        return Err(overflow_error("getservbyport: port must be 0-65535."));
    }
    let proto = servby_proto(args.get(1), "getservbyport")?;
    // The service database lookup wants the port in network byte order (`htons`).
    let net_port = i32::from((port as u16).to_be());
    let name = match servbyport_lookup(net_port, proto.as_deref()) {
        Some(n) => n,
        None => return Err(os_error("port/proto not found")),
    };
    Ok(Object::from_str(name))
}

// ---- `getservby*` platform shim ----
//
// POSIX exposes `getservbyname(3)`/`getservbyport(3)` through `libc`; Windows
// keeps the same calls in Winsock (`ws2_32`), which the `libc` crate does not
// bind on `*-pc-windows-*`. The thin wrappers below let the `socket` module's
// service-name lookups build and behave the same on every target.

/// Look up a service port by name, returning it in host byte order
/// (`None` when the service/proto pair is unknown).
#[cfg(unix)]
fn servbyname_lookup(name: &std::ffi::CStr, proto: Option<&std::ffi::CStr>) -> Option<u16> {
    let proto_ptr = proto.map_or(std::ptr::null(), |c| c.as_ptr());
    let sp = unsafe { libc::getservbyname(name.as_ptr(), proto_ptr) };
    if sp.is_null() {
        return None;
    }
    // `s_port` is stored in network byte order; CPython returns `ntohs(s_port)`.
    Some(u16::from_be(unsafe { (*sp).s_port } as u16))
}

/// Look up a service name by (network-byte-order) port, returning an owned
/// copy of the name (`None` when the port/proto pair is unknown).
#[cfg(unix)]
fn servbyport_lookup(net_port: i32, proto: Option<&std::ffi::CStr>) -> Option<String> {
    let proto_ptr = proto.map_or(std::ptr::null(), |c| c.as_ptr());
    let sp = unsafe { libc::getservbyport(net_port, proto_ptr) };
    if sp.is_null() {
        return None;
    }
    let name = unsafe { std::ffi::CStr::from_ptr((*sp).s_name) };
    Some(name.to_string_lossy().into_owned())
}

#[cfg(windows)]
fn servbyname_lookup(name: &std::ffi::CStr, proto: Option<&std::ffi::CStr>) -> Option<u16> {
    winsock::ensure_started();
    let proto_ptr = proto.map_or(std::ptr::null(), |c| c.as_ptr());
    let sp = unsafe { winsock::getservbyname(name.as_ptr(), proto_ptr) };
    if sp.is_null() {
        return None;
    }
    // `s_port` is stored in network byte order; CPython returns `ntohs(s_port)`.
    Some(u16::from_be(unsafe { (*sp).s_port } as u16))
}

#[cfg(windows)]
fn servbyport_lookup(net_port: i32, proto: Option<&std::ffi::CStr>) -> Option<String> {
    winsock::ensure_started();
    let proto_ptr = proto.map_or(std::ptr::null(), |c| c.as_ptr());
    let sp = unsafe { winsock::getservbyport(net_port, proto_ptr) };
    if sp.is_null() {
        return None;
    }
    let name = unsafe { std::ffi::CStr::from_ptr((*sp).s_name) };
    Some(name.to_string_lossy().into_owned())
}

#[cfg(not(any(unix, windows)))]
fn servbyname_lookup(_name: &std::ffi::CStr, _proto: Option<&std::ffi::CStr>) -> Option<u16> {
    None
}

#[cfg(not(any(unix, windows)))]
fn servbyport_lookup(_net_port: i32, _proto: Option<&std::ffi::CStr>) -> Option<String> {
    None
}

/// Winsock (`ws2_32`) bindings for the service-database lookups that `libc`
/// only exposes on Unix. The `servent` layout here is the 64-bit `_WIN64`
/// one from `<winsock2.h>` — where `s_port` precedes `s_proto` — which is the
/// only Windows architecture WeavePy builds for.
#[cfg(windows)]
mod winsock {
    use std::os::raw::{c_char, c_int, c_short};
    use std::sync::Once;

    #[allow(dead_code)] // `s_aliases`/`s_proto` are part of the C layout, never read.
    #[repr(C)]
    pub(super) struct Servent {
        pub(super) s_name: *mut c_char,
        pub(super) s_aliases: *mut *mut c_char,
        pub(super) s_port: c_short,
        pub(super) s_proto: *mut c_char,
    }

    #[link(name = "ws2_32")]
    extern "system" {
        pub(super) fn getservbyname(name: *const c_char, proto: *const c_char) -> *mut Servent;
        pub(super) fn getservbyport(port: c_int, proto: *const c_char) -> *mut Servent;
        #[link_name = "WSAStartup"]
        fn wsa_startup(version: u16, data: *mut u8) -> c_int;
    }

    /// Winsock requires `WSAStartup` before any name-resolution call. `std`
    /// and `socket2` arm it on first socket use, but `getservby*` can be a
    /// program's first networking call, so initialize defensively. The call
    /// is refcounted and idempotent (CPython likewise starts Winsock when
    /// `_socket` is imported); we never pair it with `WSACleanup`, matching
    /// CPython's process-lifetime initialization.
    pub(super) fn ensure_started() {
        static START: Once = Once::new();
        START.call_once(|| {
            // `WSADATA` is ~408 bytes on x64; 512 leaves headroom and we never
            // read it back. `MAKEWORD(2, 2)` requests Winsock 2.2.
            let mut data = [0u8; 512];
            unsafe {
                let _ = wsa_startup(0x0202, data.as_mut_ptr());
            }
        });
    }
}

/// `gethostbyname_ex(name)` → `(hostname, aliaslist, ipaddrlist)`.
/// CPython returns only the IPv4 addresses; the alias list is empty for
/// the loopback/getaddrinfo-backed resolution we do here.
fn mod_gethostbyname_ex(args: &[Object]) -> Result<Object, RuntimeError> {
    let name = match args.first() {
        Some(Object::Str(s)) => s.to_string(),
        _ => return Err(type_error("gethostbyname_ex: arg must be str")),
    };
    let addrs = (name.as_str(), 0_u16)
        .to_socket_addrs()
        .map_err(|e| io_error_to_py(&e))?;
    let mut ips = Vec::new();
    for sa in addrs {
        if let SocketAddr::V4(v4) = sa {
            let ip = Object::from_str(v4.ip().to_string());
            if !ips
                .iter()
                .any(|existing: &Object| existing.repr() == ip.repr())
            {
                ips.push(ip);
            }
        }
    }
    if ips.is_empty() {
        return Err(os_error("name resolution failed"));
    }
    Ok(Object::new_tuple(vec![
        Object::from_str(name),
        Object::new_list(Vec::new()),
        Object::new_list(ips),
    ]))
}

#[cfg(unix)]
fn mod_gethostbyaddr(args: &[Object]) -> Result<Object, RuntimeError> {
    use std::ffi::{CStr, CString};
    let addr = match args.first() {
        Some(Object::Str(s)) => s.to_string(),
        _ => return Err(type_error("gethostbyaddr: arg must be str")),
    };
    // CPython's setipaddr leg: the argument (a numeric address *or* a name)
    // must forward-resolve — junk like '0.1.1.~1' raises gaierror here
    // (test_host_resolution_bad_address).
    let c_addr =
        CString::new(addr).map_err(|_| value_error("gethostbyaddr: embedded null character"))?;
    let mut hints: libc::addrinfo = unsafe { std::mem::zeroed() };
    hints.ai_family = libc::AF_UNSPEC;
    hints.ai_socktype = libc::SOCK_DGRAM;
    let mut res: *mut libc::addrinfo = std::ptr::null_mut();
    let res_ptr = std::ptr::addr_of_mut!(res);
    let addr_ptr = c_addr.as_ptr();
    let rc = crate::gil::allow_threads_then(|| unsafe {
        libc::getaddrinfo(addr_ptr, std::ptr::null(), &raw const hints, res_ptr)
    });
    if rc != 0 {
        let msg = unsafe {
            let p = libc::gai_strerror(rc);
            if p.is_null() {
                "getaddrinfo failed".to_owned()
            } else {
                CStr::from_ptr(p).to_string_lossy().into_owned()
            }
        };
        return Err(gaierror(rc, msg));
    }
    // SAFETY: rc == 0 guarantees a valid chain until `freeaddrinfo`.
    let ai = unsafe { &*res };
    let sa = ai.ai_addr;
    let salen = ai.ai_addrlen;
    let mut namebuf = [0i8; 1025];
    let mut numbuf = [0i8; 1025];
    let name_out = namebuf.as_mut_ptr();
    let num_out = numbuf.as_mut_ptr();
    // Reverse (PTR) lookup for the canonical hostname; hosts without a PTR
    // record fall back to the numeric form rather than erroring, keeping
    // loopback/CI environments working.
    let rev = crate::gil::allow_threads_then(|| unsafe {
        libc::getnameinfo(
            sa,
            salen,
            name_out.cast(),
            1025,
            std::ptr::null_mut(),
            0,
            libc::NI_NAMEREQD,
        )
    });
    let num_rc = unsafe {
        libc::getnameinfo(
            sa,
            salen,
            num_out.cast(),
            1025,
            std::ptr::null_mut(),
            0,
            libc::NI_NUMERICHOST,
        )
    };
    unsafe { libc::freeaddrinfo(res) };
    let numeric = if num_rc == 0 {
        unsafe { CStr::from_ptr(numbuf.as_ptr().cast()) }
            .to_string_lossy()
            .into_owned()
    } else {
        c_addr.to_string_lossy().into_owned()
    };
    let name = if rev == 0 {
        unsafe { CStr::from_ptr(namebuf.as_ptr().cast()) }
            .to_string_lossy()
            .into_owned()
    } else {
        numeric.clone()
    };
    Ok(Object::new_tuple(vec![
        Object::from_str(name),
        Object::new_list(Vec::new()),
        Object::new_list(vec![Object::from_str(numeric)]),
    ]))
}

#[cfg(not(unix))]
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

/// `getaddrinfo(host, port, family=0, type=0, proto=0, flags=0)` over the
/// real libc resolver, so hints behave exactly as CPython's: `host=None` +
/// `AI_PASSIVE` yields the wildcard addresses for *both* stacks (`::` and
/// `0.0.0.0` — dual-stack `create_server(host=None)`, RFC 0054 WS2),
/// `type=0` enumerates every socktype, and `AI_CANONNAME` populates the
/// canonical-name column.
#[cfg(unix)]
fn mod_getaddrinfo(args: &[Object]) -> Result<Object, RuntimeError> {
    use std::ffi::{CStr, CString};
    let nul_err = || value_error("getaddrinfo: embedded null character in argument");
    let host: Option<CString> = match args.first() {
        Some(Object::Str(s)) => Some(CString::new(s.as_bytes()).map_err(|_| nul_err())?),
        // A surrogate-bearing host can't reach the C resolver: encoding it
        // (idna/utf-8) raises UnicodeEncodeError, matching CPython.
        Some(w @ Object::WStr(_)) => {
            let b = crate::stdlib::codecs_mod::encode_obj(w, "utf-8", "strict")?;
            Some(CString::new(b).map_err(|_| nul_err())?)
        }
        Some(Object::Bytes(b)) => Some(CString::new(&b[..]).map_err(|_| nul_err())?),
        Some(Object::None) | None => None,
        _ => return Err(type_error("getaddrinfo: host must be str, bytes, or None")),
    };
    let service: Option<CString> = match args.get(1) {
        // gh-74895: an int port of *any* magnitude is formatted as its
        // decimal string and left to the platform resolver — CPython no
        // longer raises OverflowError for values outside C long
        // (test_getaddrinfo_int_port_overflow feeds ULONG_MAX + 1).
        Some(Object::Int(n)) => Some(CString::new(n.to_string()).expect("digits have no NUL")),
        Some(Object::Long(b)) => Some(CString::new(b.to_string()).expect("digits have no NUL")),
        Some(Object::Str(s)) => Some(CString::new(s.as_bytes()).map_err(|_| nul_err())?),
        // A lone surrogate in the service string surfaces as the codec's
        // UnicodeEncodeError (testGetaddrinfo probes '\uD800').
        Some(w @ Object::WStr(_)) => {
            let b = crate::stdlib::codecs_mod::encode_obj(w, "utf-8", "strict")?;
            Some(CString::new(b).map_err(|_| nul_err())?)
        }
        Some(Object::Bytes(b)) => Some(CString::new(&b[..]).map_err(|_| nul_err())?),
        Some(Object::None) | None => None,
        _ => {
            return Err(type_error(
                "getaddrinfo: port must be int, str, bytes, or None",
            ))
        }
    };
    // `as_i64` unwraps IntEnum members too — `socket.py` promotes the
    // constants to `AddressFamily`/`SocketKind`, and callers pass e.g.
    // `getaddrinfo(host, port, 0, SOCK_STREAM)` with the enum.
    let int_at = |i: usize| args.get(i).and_then(Object::as_i64).unwrap_or(0) as i32;
    let (family, kind, proto, flags) = (int_at(2), int_at(3), int_at(4), int_at(5));

    let mut hints: libc::addrinfo = unsafe { std::mem::zeroed() };
    hints.ai_family = if family == 0 { libc::AF_UNSPEC } else { family };
    hints.ai_socktype = kind;
    hints.ai_protocol = proto;
    hints.ai_flags = flags;
    let host_ptr = host.as_ref().map_or(std::ptr::null(), |c| c.as_ptr());
    let serv_ptr = service.as_ref().map_or(std::ptr::null(), |c| c.as_ptr());
    let mut res: *mut libc::addrinfo = std::ptr::null_mut();
    let res_ptr = std::ptr::addr_of_mut!(res);
    let rc = crate::gil::allow_threads_then(|| unsafe {
        libc::getaddrinfo(host_ptr, serv_ptr, &raw const hints, res_ptr)
    });
    if rc != 0 {
        let msg = unsafe {
            let p = libc::gai_strerror(rc);
            if p.is_null() {
                "getaddrinfo failed".to_owned()
            } else {
                CStr::from_ptr(p).to_string_lossy().into_owned()
            }
        };
        return Err(gaierror(rc, msg));
    }

    let mut out = Vec::new();
    let mut cur = res;
    while !cur.is_null() {
        let ai = unsafe { &*cur };
        cur = ai.ai_next;
        let addr_tuple = match ai.ai_family {
            f if f == libc::AF_INET => {
                let sin = unsafe { &*ai.ai_addr.cast::<libc::sockaddr_in>() };
                let ip = std::net::Ipv4Addr::from(u32::from_be(sin.sin_addr.s_addr));
                Object::new_tuple(vec![
                    Object::from_str(ip.to_string()),
                    Object::Int(i64::from(u16::from_be(sin.sin_port))),
                ])
            }
            f if f == libc::AF_INET6 => {
                let sin6 = unsafe { &*ai.ai_addr.cast::<libc::sockaddr_in6>() };
                let ip = std::net::Ipv6Addr::from(sin6.sin6_addr.s6_addr);
                Object::new_tuple(vec![
                    Object::from_str(ip.to_string()),
                    Object::Int(i64::from(u16::from_be(sin6.sin6_port))),
                    Object::Int(i64::from(u32::from_be(sin6.sin6_flowinfo))),
                    Object::Int(i64::from(sin6.sin6_scope_id)),
                ])
            }
            _ => continue,
        };
        let canonname = if ai.ai_canonname.is_null() {
            Object::from_static("")
        } else {
            let c = unsafe { CStr::from_ptr(ai.ai_canonname) };
            Object::from_str(c.to_string_lossy().into_owned())
        };
        out.push(Object::new_tuple(vec![
            Object::Int(i64::from(ai.ai_family)),
            Object::Int(i64::from(ai.ai_socktype)),
            Object::Int(i64::from(ai.ai_protocol)),
            canonname,
            addr_tuple,
        ]));
    }
    unsafe { libc::freeaddrinfo(res) };
    Ok(Object::new_list(out))
}

/// Windows arm (RFC 0063 WS4): the same call over Winsock's own
/// `getaddrinfo` (ANSI — host/service are idna/ASCII by the time they
/// reach the resolver, exactly the encoding CPython feeds its
/// `getaddrinfo` on Windows), restoring `AI_PASSIVE` wildcard and
/// `AI_CANONNAME` fidelity that the previous `ToSocketAddrs`
/// approximation lost. Mirrors the unix arm above, with windows-sys
/// types and the WSA error domain.
#[cfg(windows)]
fn mod_getaddrinfo(args: &[Object]) -> Result<Object, RuntimeError> {
    use std::ffi::{CStr, CString};
    use windows_sys::Win32::Networking::WinSock as ws;
    let nul_err = || value_error("getaddrinfo: embedded null character in argument");
    let host: Option<CString> = match args.first() {
        Some(Object::Str(s)) => Some(CString::new(s.as_bytes()).map_err(|_| nul_err())?),
        Some(Object::Bytes(b)) => Some(CString::new(&b[..]).map_err(|_| nul_err())?),
        Some(Object::None) | None => None,
        _ => return Err(type_error("getaddrinfo: host must be str, bytes, or None")),
    };
    let service: Option<CString> = match args.get(1) {
        Some(Object::Int(n)) => Some(CString::new(n.to_string()).expect("digits have no NUL")),
        Some(Object::Str(s)) => Some(CString::new(s.as_bytes()).map_err(|_| nul_err())?),
        Some(Object::Bytes(b)) => Some(CString::new(&b[..]).map_err(|_| nul_err())?),
        Some(Object::None) | None => None,
        _ => {
            return Err(type_error(
                "getaddrinfo: port must be int, str, bytes, or None",
            ))
        }
    };
    // `as_i64` unwraps IntEnum members too, like the unix arm.
    let int_at = |i: usize| args.get(i).and_then(Object::as_i64).unwrap_or(0) as i32;
    let (family, kind, proto, flags) = (int_at(2), int_at(3), int_at(4), int_at(5));

    let hints = ws::ADDRINFOA {
        ai_flags: flags,
        // AF_UNSPEC is 0 on Windows too, so family passes through as-is.
        ai_family: family,
        ai_socktype: kind,
        ai_protocol: proto,
        ..Default::default()
    };
    let host_ptr = host
        .as_ref()
        .map_or(std::ptr::null(), |c| c.as_ptr().cast::<u8>());
    let serv_ptr = service
        .as_ref()
        .map_or(std::ptr::null(), |c| c.as_ptr().cast::<u8>());
    let mut res: *mut ws::ADDRINFOA = std::ptr::null_mut();
    let res_ptr = std::ptr::addr_of_mut!(res);
    let rc = crate::gil::allow_threads_then(|| unsafe {
        ws::getaddrinfo(host_ptr, serv_ptr, &raw const hints, res_ptr)
    });
    if rc != 0 {
        // Winsock's getaddrinfo returns the WSA error code directly
        // (WSAHOST_NOT_FOUND, …); CPython raises gaierror with
        // gai_strerror text, which on Windows *is* FormatMessage.
        return Err(gaierror(rc, crate::stdlib::nt_support::format_message(rc)));
    }

    let mut out = Vec::new();
    let mut cur = res;
    while !cur.is_null() {
        // SAFETY: `cur` walks the linked list Winsock just handed us; it
        // stays valid until the `freeaddrinfo` below.
        let ai = unsafe { &*cur };
        cur = ai.ai_next;
        let addr_tuple = match ai.ai_family {
            f if f == i32::from(ws::AF_INET) => {
                // Winsock allocates `ai_addr` with full sockaddr alignment;
                // the SOCKADDR type is only declared 2-byte aligned.
                #[allow(clippy::cast_ptr_alignment)]
                let sin = unsafe { &*ai.ai_addr.cast::<ws::SOCKADDR_IN>() };
                let ip =
                    std::net::Ipv4Addr::from(u32::from_be(unsafe { sin.sin_addr.S_un.S_addr }));
                Object::new_tuple(vec![
                    Object::from_str(ip.to_string()),
                    Object::Int(i64::from(u16::from_be(sin.sin_port))),
                ])
            }
            f if f == i32::from(ws::AF_INET6) => {
                #[allow(clippy::cast_ptr_alignment)] // see AF_INET arm above
                let sin6 = unsafe { &*ai.ai_addr.cast::<ws::SOCKADDR_IN6>() };
                let ip = std::net::Ipv6Addr::from(unsafe { sin6.sin6_addr.u.Byte });
                Object::new_tuple(vec![
                    Object::from_str(ip.to_string()),
                    Object::Int(i64::from(u16::from_be(sin6.sin6_port))),
                    Object::Int(i64::from(u32::from_be(sin6.sin6_flowinfo))),
                    Object::Int(i64::from(unsafe { sin6.Anonymous.sin6_scope_id })),
                ])
            }
            _ => continue,
        };
        let canonname = if ai.ai_canonname.is_null() {
            Object::from_static("")
        } else {
            let c = unsafe { CStr::from_ptr(ai.ai_canonname.cast()) };
            Object::from_str(c.to_string_lossy().into_owned())
        };
        out.push(Object::new_tuple(vec![
            Object::Int(i64::from(ai.ai_family)),
            Object::Int(i64::from(ai.ai_socktype)),
            Object::Int(i64::from(ai.ai_protocol)),
            canonname,
            addr_tuple,
        ]));
    }
    unsafe { ws::freeaddrinfo(res) };
    Ok(Object::new_list(out))
}

/// Fallback resolver over `std::net::ToSocketAddrs` for targets with
/// neither libc nor Winsock `addrinfo`. Loses the hint fidelity of the
/// native paths (`AI_PASSIVE` wildcards, `AI_CANONNAME`) but resolves
/// names/ports correctly — the pre-RFC-0054 behavior.
#[cfg(not(any(unix, windows)))]
fn mod_getaddrinfo(args: &[Object]) -> Result<Object, RuntimeError> {
    let host = match args.first() {
        Some(Object::Str(s)) => s.to_string(),
        Some(Object::Bytes(b)) => String::from_utf8_lossy(b).into_owned(),
        Some(Object::None) | None => "0.0.0.0".to_string(),
        _ => return Err(type_error("getaddrinfo: host must be str, bytes, or None")),
    };
    let port = match args.get(1) {
        Some(Object::Int(n)) => *n as u16,
        Some(Object::Str(s)) => s.parse::<u16>().unwrap_or(0),
        Some(Object::None) | None => 0,
        _ => return Err(type_error("getaddrinfo: port must be int, str, or None")),
    };
    let int_at = |i: usize| match args.get(i) {
        Some(Object::Int(n)) => *n as i32,
        _ => 0,
    };
    let (family_req, mut kind, proto) = (int_at(2), int_at(3), int_at(4));
    if kind == 0 {
        kind = libc_sock_stream() as i32;
    }
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

/// Keyword-aware wrapper over [`mod_getaddrinfo`]. Maps the CPython
/// signature `getaddrinfo(host, port, family=0, type=0, proto=0, flags=0)`
/// — accepting any of the trailing five by keyword — onto the positional
/// core.
fn mod_getaddrinfo_kw(
    args: &[Object],
    kwargs: &[(String, Object)],
) -> Result<Object, RuntimeError> {
    const NAMES: [&str; 6] = ["host", "port", "family", "type", "proto", "flags"];
    let mut slots: [Option<Object>; 6] = std::array::from_fn(|_| None);
    for (i, v) in args.iter().take(6).enumerate() {
        slots[i] = Some(v.clone());
    }
    for (k, v) in kwargs {
        match NAMES.iter().position(|n| n == k) {
            Some(idx) if slots[idx].is_some() => {
                return Err(type_error(format!(
                    "getaddrinfo() got multiple values for argument '{k}'"
                )));
            }
            Some(idx) => slots[idx] = Some(v.clone()),
            None => {
                return Err(type_error(format!(
                    "getaddrinfo() got an unexpected keyword argument '{k}'"
                )));
            }
        }
    }
    let positional: Vec<Object> = slots
        .iter()
        .map(|s| s.clone().unwrap_or(Object::None))
        .collect();
    mod_getaddrinfo(&positional)
}

#[cfg(not(windows))]
fn mod_getnameinfo(args: &[Object]) -> Result<Object, RuntimeError> {
    // CPython's socket_getnameinfo (socketmodule.c): the host must already
    // be a *numeric* address — it is re-parsed through
    // `getaddrinfo(AI_NUMERICHOST)` to build the binary sockaddr (raising
    // gaierror for names like 'mail.python.org' — test_getnameinfo), the
    // 4-tuple's flowinfo/scope-id are patched in (they aren't expressible
    // in the numeric string), and the C `getnameinfo` renders the result —
    // lowercased hex with a `%ifname` scope suffix (the scopeid_symbolic
    // tests assert both).
    use std::ffi::{CStr, CString};
    let tup = match args.first() {
        Some(Object::Tuple(t)) => t,
        Some(_) => return Err(type_error("getnameinfo() argument 1 must be a tuple")),
        None => return Err(type_error("getnameinfo: missing argument")),
    };
    let host = match tup.first() {
        Some(Object::Str(s)) => s.to_string(),
        _ => return Err(type_error("getnameinfo: address[0] must be str")),
    };
    let port = match tup.get(1).and_then(Object::as_i64) {
        Some(n) => n,
        None => return Err(type_error("getnameinfo: address[1] must be int")),
    };
    let flowinfo = tup.get(2).and_then(Object::as_i64).unwrap_or(0);
    if !(0..=0xfffff).contains(&flowinfo) {
        return Err(crate::error::overflow_error(
            "getnameinfo(): flowinfo must be 0-1048575.",
        ));
    }
    let scope_id = tup.get(3).and_then(Object::as_i64).unwrap_or(0) as u32;
    let flags = args.get(1).and_then(Object::as_i64).unwrap_or(0) as libc::c_int;

    let c_host = CString::new(host)
        .map_err(|_| value_error("getnameinfo: embedded null character in argument"))?;
    let c_serv = CString::new(port.to_string()).expect("digits have no NUL");
    let mut hints: libc::addrinfo = unsafe { std::mem::zeroed() };
    hints.ai_family = libc::AF_UNSPEC;
    // SOCK_DGRAM keeps the resolver from returning one row per socktype.
    hints.ai_socktype = libc::SOCK_DGRAM;
    hints.ai_flags = libc::AI_NUMERICHOST;
    let mut res: *mut libc::addrinfo = std::ptr::null_mut();
    let res_ptr = std::ptr::addr_of_mut!(res);
    let host_ptr = c_host.as_ptr();
    let serv_ptr = c_serv.as_ptr();
    let rc = crate::gil::allow_threads_then(|| unsafe {
        libc::getaddrinfo(host_ptr, serv_ptr, &raw const hints, res_ptr)
    });
    if rc != 0 {
        let msg = unsafe {
            let p = libc::gai_strerror(rc);
            if p.is_null() {
                "getaddrinfo failed".to_owned()
            } else {
                CStr::from_ptr(p).to_string_lossy().into_owned()
            }
        };
        return Err(gaierror(rc, msg));
    }
    // SAFETY: rc == 0 guarantees a valid chain until `freeaddrinfo`.
    let ai = unsafe { &*res };
    if ai.ai_family == libc::AF_INET6 {
        let sin6 = unsafe { &mut *ai.ai_addr.cast::<libc::sockaddr_in6>() };
        sin6.sin6_flowinfo = (flowinfo as u32).to_be();
        sin6.sin6_scope_id = scope_id;
    }
    let mut hostbuf = [0i8; 1025]; // NI_MAXHOST
    let mut servbuf = [0i8; 32]; // NI_MAXSERV
    let addr = ai.ai_addr;
    let addrlen = ai.ai_addrlen;
    let host_out = hostbuf.as_mut_ptr();
    let serv_out = servbuf.as_mut_ptr();
    let rc = crate::gil::allow_threads_then(|| unsafe {
        libc::getnameinfo(
            addr,
            addrlen,
            host_out.cast(),
            1025,
            serv_out.cast(),
            32,
            flags,
        )
    });
    unsafe { libc::freeaddrinfo(res) };
    if rc != 0 {
        let msg = unsafe {
            let p = libc::gai_strerror(rc);
            if p.is_null() {
                "getnameinfo failed".to_owned()
            } else {
                CStr::from_ptr(p).to_string_lossy().into_owned()
            }
        };
        return Err(gaierror(rc, msg));
    }
    let host_s = unsafe { CStr::from_ptr(hostbuf.as_ptr().cast()) }
        .to_string_lossy()
        .into_owned();
    let serv_s = unsafe { CStr::from_ptr(servbuf.as_ptr().cast()) }
        .to_string_lossy()
        .into_owned();
    Ok(Object::new_tuple(vec![
        Object::from_str(host_s),
        Object::from_str(serv_s),
    ]))
}

/// `getnameinfo(sockaddr, flags)` over Winsock (RFC 0063 WS4), following
/// CPython's `socket_getnameinfo` (socketmodule.c): the numeric host is
/// first re-parsed through `getaddrinfo(…, AI_NUMERICHOST)` to build the
/// binary sockaddr (patching in the 4-tuple's flowinfo/scope-id for
/// IPv6), which is then handed to `getnameinfo` with the caller's flags.
#[cfg(windows)]
fn mod_getnameinfo(args: &[Object]) -> Result<Object, RuntimeError> {
    use std::ffi::{CStr, CString};
    use windows_sys::Win32::Networking::WinSock as ws;
    let tup = match args.first() {
        Some(Object::Tuple(t)) => t,
        Some(_) => return Err(type_error("getnameinfo: address must be tuple")),
        None => return Err(type_error("getnameinfo: missing argument")),
    };
    let host = match tup.first() {
        Some(Object::Str(s)) => s.to_string(),
        _ => return Err(type_error("getnameinfo: address[0] must be str")),
    };
    let port = match tup.get(1) {
        Some(Object::Int(n)) => *n as u16,
        _ => return Err(type_error("getnameinfo: address[1] must be int")),
    };
    let flowinfo = tup.get(2).and_then(Object::as_i64).unwrap_or(0) as u32;
    let scope_id = tup.get(3).and_then(Object::as_i64).unwrap_or(0) as u32;
    let flags = args.get(1).and_then(Object::as_i64).unwrap_or(0) as i32;

    let c_host = CString::new(host)
        .map_err(|_| value_error("getnameinfo: embedded null character in argument"))?;
    let c_serv = CString::new(port.to_string()).expect("digits have no NUL");
    let hints = ws::ADDRINFOA {
        ai_flags: ws::AI_NUMERICHOST as i32,
        ai_family: i32::from(ws::AF_UNSPEC),
        // SOCK_DGRAM keeps the resolver from returning one row per
        // socktype (CPython does the same).
        ai_socktype: ws::SOCK_DGRAM,
        ..Default::default()
    };
    let mut res: *mut ws::ADDRINFOA = std::ptr::null_mut();
    let res_ptr = std::ptr::addr_of_mut!(res);
    let host_ptr = c_host.as_ptr().cast::<u8>();
    let serv_ptr = c_serv.as_ptr().cast::<u8>();
    let rc = crate::gil::allow_threads_then(|| unsafe {
        ws::getaddrinfo(host_ptr, serv_ptr, &raw const hints, res_ptr)
    });
    if rc != 0 {
        return Err(gaierror(rc, crate::stdlib::nt_support::format_message(rc)));
    }

    // SAFETY: rc == 0 guarantees a non-null, valid result chain until
    // the `freeaddrinfo` below.
    let ai = unsafe { &*res };
    if i32::from(ws::AF_INET6) == ai.ai_family {
        // The 4-tuple's flowinfo/scope-id aren't expressible in the
        // numeric host string; CPython patches them into the sockaddr.
        // Winsock allocates `ai_addr` with full sockaddr alignment.
        #[allow(clippy::cast_ptr_alignment)]
        let sin6 = unsafe { &mut *ai.ai_addr.cast::<ws::SOCKADDR_IN6>() };
        sin6.sin6_flowinfo = flowinfo.to_be();
        sin6.Anonymous.sin6_scope_id = scope_id;
    }
    let mut hostbuf = [0u8; ws::NI_MAXHOST as usize];
    let mut servbuf = [0u8; ws::NI_MAXSERV as usize];
    let (ai_addr, ai_addrlen) = (ai.ai_addr, ai.ai_addrlen);
    let host_out = hostbuf.as_mut_ptr();
    let serv_out = servbuf.as_mut_ptr();
    let rc = crate::gil::allow_threads_then(|| unsafe {
        ws::getnameinfo(
            ai_addr,
            ai_addrlen as ws::socklen_t,
            host_out,
            ws::NI_MAXHOST,
            serv_out,
            ws::NI_MAXSERV,
            flags,
        )
    });
    unsafe { ws::freeaddrinfo(res) };
    if rc != 0 {
        return Err(gaierror(rc, crate::stdlib::nt_support::format_message(rc)));
    }
    let decode = |buf: &[u8]| -> String {
        // SAFETY: getnameinfo NUL-terminates within the buffer on success.
        unsafe { CStr::from_ptr(buf.as_ptr().cast()) }
            .to_string_lossy()
            .into_owned()
    };
    Ok(Object::new_tuple(vec![
        Object::from_str(decode(&hostbuf)),
        Object::from_str(decode(&servbuf)),
    ]))
}

fn mod_socketpair(args: &[Object]) -> Result<Object, RuntimeError> {
    // CPython signature: socketpair(family=AF_UNIX, type=SOCK_STREAM, proto=0).
    // The AF_UNIX default is load-bearing: `multiprocessing`'s `Connection`
    // pipes and the `forkserver` control channel both rely on a real
    // `socketpair(2)` over which `SCM_RIGHTS` fd passing works (a TCP
    // loopback pair cannot carry ancillary data).
    // `as_i64` also unwraps the `AddressFamily`/`SocketKind` IntEnum
    // members `socket.py` promotes the constants to.
    let family = match args.first() {
        None | Some(Object::None) => default_socketpair_family(),
        Some(o) => o
            .as_i64()
            .map(|n| n as i32)
            .ok_or_else(|| type_error("socketpair: family must be an integer"))?,
    };
    let sock_type = match args.get(1) {
        None | Some(Object::None) => libc_sock_stream() as i32,
        Some(o) => o
            .as_i64()
            .map(|n| n as i32)
            .ok_or_else(|| type_error("socketpair: type must be an integer"))?,
    };
    let proto = match args.get(2) {
        None | Some(Object::None) => 0,
        Some(o) => o
            .as_i64()
            .map(|n| n as i32)
            .ok_or_else(|| type_error("socketpair: proto must be an integer"))?,
    };

    #[cfg(unix)]
    if family == 1 {
        return unix_socketpair(family, sock_type, proto);
    }

    // The AF_INET emulation ignores the requested family/type/proto; consume
    // them so they don't read as unused on platforms without `unix_socketpair`.
    #[cfg(not(unix))]
    let _ = (family, sock_type, proto);

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
        // PEP 446: like every descriptor Python creates, the pair is
        // non-inheritable (InheritanceTest.test_socketpair).
        let _ = sock.set_cloexec(true);
        let state = Rc::new(RefCell::new(SocketState {
            inner: Some(sock),
            family,
            kind: sock_type,
            proto,
            timeout: None,
            blocking: true,
            owns_fd: true,
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
            owns_fd: true,
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
    // `as_i64` also unwraps `AddressFamily` IntEnum members.
    let family = match args.first().and_then(Object::as_i64) {
        Some(n) => n as i32,
        None => return Err(type_error("inet_pton: family must be int")),
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
    // `as_i64` also unwraps `AddressFamily` IntEnum members.
    let family = match args.first().and_then(Object::as_i64) {
        Some(n) => n as i32,
        None => return Err(type_error("inet_ntop: family must be int")),
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

/// Shared 16/32-bit unsigned conversion for the byte-order helpers: the
/// value must fit the C type exactly — negatives and larger ints raise
/// OverflowError (testNtoH feeds `1<<34` to every one of the four).
fn byteswap_arg(arg: Option<&Object>, bits: u32, name: &str) -> Result<u64, RuntimeError> {
    let n = match arg.and_then(Object::as_i64) {
        Some(n) => n,
        None if matches!(arg, Some(Object::Long(_))) => {
            return Err(crate::error::overflow_error(format!(
                "{name}: Python int too large to convert to C unsigned"
            )))
        }
        None => return Err(type_error(format!("{name}: arg must be int"))),
    };
    if n < 0 || n >= 1i64 << bits {
        return Err(crate::error::overflow_error(format!(
            "{name}: Python int too large to convert to C unsigned"
        )));
    }
    Ok(n as u64)
}

fn mod_htons(args: &[Object]) -> Result<Object, RuntimeError> {
    let n = byteswap_arg(args.first(), 16, "htons")?;
    Ok(Object::Int(i64::from((n as u16).to_be())))
}

fn mod_htonl(args: &[Object]) -> Result<Object, RuntimeError> {
    let n = byteswap_arg(args.first(), 32, "htonl")?;
    Ok(Object::Int(i64::from((n as u32).to_be())))
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
        Some(Object::Float(f)) => {
            if !f.is_finite() || *f < 0.0 {
                return Err(value_error("Timeout value out of range"));
            }
            Some(*f)
        }
        Some(Object::Int(n)) => {
            if *n < 0 {
                return Err(value_error("Timeout value out of range"));
            }
            Some(*n as f64)
        }
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

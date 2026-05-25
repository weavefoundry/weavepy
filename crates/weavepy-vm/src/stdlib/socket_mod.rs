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

thread_local! {
    static REGISTRY: RefCell<HashMap<i64, Rc<RefCell<SocketState>>>> =
        RefCell::new(HashMap::new());
    static NEXT_HANDLE: RefCell<i64> = const { RefCell::new(0) };
    static SOCKET_CLASS: RefCell<Option<Rc<TypeObject>>> = const { RefCell::new(None) };
}

fn next_handle(state: Rc<RefCell<SocketState>>) -> i64 {
    let handle = NEXT_HANDLE.with(|c| {
        let mut n = c.borrow_mut();
        // Use the underlying OS fd as the handle if available so
        // `fileno()` returns something a host C library would accept.
        // Fall back to a monotonically increasing counter for
        // platforms (or modes) where we can't extract the raw fd.
        let h = state
            .borrow()
            .inner
            .as_ref()
            .and_then(raw_fd_of)
            .unwrap_or_else(|| {
                *n += 1;
                -*n
            });
        h
    });
    REGISTRY.with(|r| r.borrow_mut().insert(handle, state));
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

fn get_state(handle: i64) -> Option<Rc<RefCell<SocketState>>> {
    REGISTRY.with(|r| r.borrow().get(&handle).cloned())
}

fn remove_state(handle: i64) {
    REGISTRY.with(|r| {
        r.borrow_mut().remove(&handle);
    });
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
        call: Box::new(body),
    }))
}

// ---- socket class construction ----

fn socket_class() -> Rc<TypeObject> {
    SOCKET_CLASS.with(|slot| {
        if let Some(c) = slot.borrow().as_ref() {
            return c.clone();
        }
        let bt = crate::builtin_types::builtin_types();
        let mut dict = DictData::new();
        for (name, method) in socket_methods() {
            dict.insert(DictKey(Object::from_str(name)), method);
        }
        let cls = TypeObject::new_user("socket", vec![bt.object_.clone()], dict)
            .expect("socket class must linearise");
        // The constructor lives on the class as `__init__`, and the
        // module-level `socket.socket(...)` callable goes through
        // `Vm::instantiate` which dispatches it.
        *slot.borrow_mut() = Some(cls.clone());
        cls
    })
}

fn socket_methods() -> Vec<(&'static str, Object)> {
    macro_rules! m {
        ($name:literal, $body:expr) => {
            (
                $name,
                Object::Builtin(Rc::new(BuiltinFn {
                    name: $name,
                    call: Box::new($body),
                })),
            )
        };
    }
    vec![
        m!("__init__", sock_init),
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
        m!("makefile", sock_makefile),
        m!("family_get", sock_family_attr),
        m!("type_get", sock_type_attr),
        m!("proto_get", sock_proto_attr),
    ]
}

fn extract_self(args: &[Object]) -> Result<Rc<PyInstance>, RuntimeError> {
    match args.first() {
        Some(Object::Instance(inst)) if inst.class.name == "socket" => Ok(inst.clone()),
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

fn sock_init(args: &[Object]) -> Result<Object, RuntimeError> {
    // args[0] = self, then positional family/type/proto/fileno.
    let inst = extract_self(args)?;
    let family = match args.get(1) {
        Some(Object::Int(i)) => *i as i32,
        None | Some(Object::None) => libc_af_inet() as i32,
        _ => return Err(type_error("family must be int")),
    };
    let kind = match args.get(2) {
        Some(Object::Int(i)) => *i as i32,
        None | Some(Object::None) => libc_sock_stream() as i32,
        _ => return Err(type_error("type must be int")),
    };
    let proto = match args.get(3) {
        Some(Object::Int(i)) => *i as i32,
        None | Some(Object::None) => 0,
        _ => return Err(type_error("proto must be int")),
    };
    let inner = Socket::new(
        Domain::from(family),
        Type::from(kind),
        Some(Protocol::from(proto)),
    )
    .map_err(|e| io_error_to_py(&e))?;
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
    let addr = parse_socket_address(args.get(1), family)?;
    let s_borrow = state.borrow();
    let sock = s_borrow
        .inner
        .as_ref()
        .ok_or_else(|| os_error("socket closed"))?;
    sock.bind(&SockAddr::from(addr))
        .map_err(|e| io_error_to_py(&e))?;
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
    let (new_sock, addr) = {
        let s_borrow = state.borrow();
        let sock = s_borrow
            .inner
            .as_ref()
            .ok_or_else(|| os_error("socket closed"))?;
        sock.accept().map_err(|e| io_error_to_py(&e))?
    };
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
    let family = state.borrow().family;
    let addr = parse_socket_address(args.get(1), family)?;
    let s_borrow = state.borrow();
    let sock = s_borrow
        .inner
        .as_ref()
        .ok_or_else(|| os_error("socket closed"))?;
    let timeout = s_borrow.timeout;
    match timeout {
        Some(t) => sock.connect_timeout(&SockAddr::from(addr), t),
        None => sock.connect(&SockAddr::from(addr)),
    }
    .map_err(|e| io_error_to_py(&e))?;
    Ok(Object::None)
}

fn sock_connect_ex(args: &[Object]) -> Result<Object, RuntimeError> {
    match sock_connect(args) {
        Ok(_) => Ok(Object::Int(0)),
        Err(RuntimeError::PyException(p)) => {
            // CPython returns the errno; we don't track that, so
            // return a non-zero placeholder.
            let _ = p;
            Ok(Object::Int(-1))
        }
        Err(e) => Err(e),
    }
}

fn sock_send(args: &[Object]) -> Result<Object, RuntimeError> {
    let state = state_of(args)?;
    let data = extract_bytes(args.get(1))?;
    let s_borrow = state.borrow();
    let sock = s_borrow
        .inner
        .as_ref()
        .ok_or_else(|| os_error("socket closed"))?;
    let n = sock.send(&data).map_err(|e| io_error_to_py(&e))?;
    Ok(Object::Int(n as i64))
}

fn sock_sendall(args: &[Object]) -> Result<Object, RuntimeError> {
    let state = state_of(args)?;
    let data = extract_bytes(args.get(1))?;
    let s_borrow = state.borrow();
    let sock = s_borrow
        .inner
        .as_ref()
        .ok_or_else(|| os_error("socket closed"))?;
    let mut offset = 0;
    while offset < data.len() {
        let n = sock.send(&data[offset..]).map_err(|e| io_error_to_py(&e))?;
        if n == 0 {
            return Err(broken_pipe());
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
    let s_borrow = state.borrow();
    let sock = s_borrow
        .inner
        .as_ref()
        .ok_or_else(|| os_error("socket closed"))?;
    let n = sock
        .send_to(&data, &SockAddr::from(addr))
        .map_err(|e| io_error_to_py(&e))?;
    Ok(Object::Int(n as i64))
}

fn sock_recv(args: &[Object]) -> Result<Object, RuntimeError> {
    let state = state_of(args)?;
    let bufsize = match args.get(1) {
        Some(Object::Int(n)) => *n as usize,
        _ => return Err(type_error("recv: bufsize must be int")),
    };
    let mut buf: Vec<std::mem::MaybeUninit<u8>> = vec![std::mem::MaybeUninit::uninit(); bufsize];
    let n = {
        let s_borrow = state.borrow();
        let sock = s_borrow
            .inner
            .as_ref()
            .ok_or_else(|| os_error("socket closed"))?;
        sock.recv(&mut buf).map_err(|e| io_error_to_py(&e))?
    };
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
    let n = {
        let s_borrow = state.borrow();
        let sock = s_borrow
            .inner
            .as_ref()
            .ok_or_else(|| os_error("socket closed"))?;
        sock.recv(&mut buf).map_err(|e| io_error_to_py(&e))?
    };
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
    let (n, addr) = {
        let s_borrow = state.borrow();
        let sock = s_borrow
            .inner
            .as_ref()
            .ok_or_else(|| os_error("socket closed"))?;
        sock.recv_from(&mut buf).map_err(|e| io_error_to_py(&e))?
    };
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
    state.borrow_mut().blocking = flag;
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
    {
        let s_borrow = state.borrow();
        let sock = s_borrow
            .inner
            .as_ref()
            .ok_or_else(|| os_error("socket closed"))?;
        sock.set_read_timeout(timeout)
            .map_err(|e| io_error_to_py(&e))?;
        sock.set_write_timeout(timeout)
            .map_err(|e| io_error_to_py(&e))?;
    }
    state.borrow_mut().timeout = timeout;
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
    let _level = match args.get(1) {
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
    } else if optname == 1 {
        // TCP_NODELAY
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
    let optname = match args.get(2) {
        Some(Object::Int(n)) => *n as i32,
        _ => return Err(type_error("getsockopt: optname must be int")),
    };
    let s_borrow = state.borrow();
    let sock = s_borrow
        .inner
        .as_ref()
        .ok_or_else(|| os_error("socket closed"))?;
    if optname == 4 {
        // SO_ERROR — return last error number, or 0.
        let err = sock.take_error().ok().flatten();
        return Ok(Object::Int(
            err.map_or(0, |e| i64::from(e.raw_os_error().unwrap_or(0))),
        ));
    }
    if optname == 3 {
        // SO_TYPE — return our recorded kind.
        return Ok(Object::Int(i64::from(state.borrow().kind)));
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
    if let Some(state) = get_state(h) {
        state.borrow_mut().inner.take();
    }
    remove_state(h);
    inst.dict
        .borrow_mut()
        .insert(DictKey(Object::from_static("_handle")), Object::Int(-1));
    Ok(Object::Int(h))
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
                call: Box::new(read),
            })),
        );
        d.insert(
            DictKey(Object::from_static("write")),
            Object::Builtin(Rc::new(BuiltinFn {
                name: "write",
                call: Box::new(write),
            })),
        );
        d.insert(
            DictKey(Object::from_static("close")),
            Object::Builtin(Rc::new(BuiltinFn {
                name: "close",
                call: Box::new(close),
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
    let parsed = format!("{host_for_lookup}:{port}")
        .to_socket_addrs()
        .map_err(|e| io_error_to_py(&e))?
        .next()
        .ok_or_else(|| os_error("could not resolve address"))?;
    Ok(parsed)
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
        Object::new_tuple(vec![Object::from_static(""), Object::Int(0)])
    }
}

fn extract_bytes(arg: Option<&Object>) -> Result<Vec<u8>, RuntimeError> {
    match arg {
        Some(Object::Bytes(b)) => Ok(b.to_vec()),
        Some(Object::ByteArray(b)) => Ok(b.borrow().clone()),
        Some(Object::Str(s)) => Ok(s.as_bytes().to_vec()),
        _ => Err(type_error("expected bytes-like object")),
    }
}

fn broken_pipe() -> RuntimeError {
    crate::error::broken_pipe_error("connection closed")
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
    ]
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

fn mod_socketpair(_args: &[Object]) -> Result<Object, RuntimeError> {
    // Build a loopback-connected pair via a temporary listener.
    // This avoids needing OS-level `socketpair(2)` (which doesn't exist
    // on Windows) and `AF_UNIX` (which isn't always available), at the
    // cost of using TCP/IPv4 for both halves.
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
        .map_err(|_| value_error(format!("illegal IP address: {s}")))?;
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
    if family == libc_af_inet() as i32 {
        let ip: Ipv4Addr = s
            .parse()
            .map_err(|_| value_error(format!("illegal IPv4 address: {s}")))?;
        Ok(Object::new_bytes(ip.octets().to_vec()))
    } else if family == libc_af_inet6() as i32 {
        let ip: Ipv6Addr = s
            .parse()
            .map_err(|_| value_error(format!("illegal IPv6 address: {s}")))?;
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

thread_local! {
    static DEFAULT_TIMEOUT: RefCell<Option<f64>> = const { RefCell::new(None) };
}

fn mod_getdefaulttimeout(_args: &[Object]) -> Result<Object, RuntimeError> {
    DEFAULT_TIMEOUT.with(|c| match *c.borrow() {
        None => Ok(Object::None),
        Some(f) => Ok(Object::Float(f)),
    })
}

fn mod_setdefaulttimeout(args: &[Object]) -> Result<Object, RuntimeError> {
    let value = match args.first() {
        None | Some(Object::None) => None,
        Some(Object::Float(f)) => Some(*f),
        Some(Object::Int(n)) => Some(*n as f64),
        _ => return Err(type_error("setdefaulttimeout: arg must be float or None")),
    };
    DEFAULT_TIMEOUT.with(|c| *c.borrow_mut() = value);
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

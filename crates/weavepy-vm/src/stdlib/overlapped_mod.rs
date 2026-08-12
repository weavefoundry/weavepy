//! The `_overlapped` built-in module (RFC 0063 WS4) — the IOCP layer
//! under `asyncio.ProactorEventLoop`, transcribed from CPython 3.13's
//! `Modules/overlapped.c`.
//!
//! Surface: the completion-port functions (`CreateIoCompletionPort`,
//! `GetQueuedCompletionStatus`, `PostQueuedCompletionStatus`), the
//! thread-pool wait bridge (`RegisterWaitWithQueue`/`UnregisterWait
//! (Ex)`), event helpers, `BindLocal`/`WSAConnect`/`ConnectPipe`/
//! `FormatMessage`, and the `Overlapped` type whose methods start
//! overlapped operations (`ReadFile`, `WSARecv`, `WSASend`, `AcceptEx`,
//! `ConnectEx`, …) and whose `getresult()` collects them.
//!
//! Two invariants carried over from `overlapped.c`:
//!
//! 1. **The `OVERLAPPED` struct and every buffer an operation hands the
//!    kernel must stay at a stable address until the operation
//!    completes** (or is cancelled *and* drained). CPython embeds the
//!    `OVERLAPPED` in the PyObject (objects never move); WeavePy's
//!    instances have no stable native payload, so each `Overlapped`
//!    heap-allocates an [`OvBlock`] held in a process-global registry
//!    keyed by the `OVERLAPPED`'s address — the same value exposed as
//!    `.address` and returned by `GetQueuedCompletionStatus`, which is
//!    exactly how `IocpProactor._cache` keys its dict.
//! 2. **The wait callback runs on an OS thread-pool thread without the
//!    GIL** and must not touch Python state: like CPython's
//!    `PostToQueueCallback` it only calls `PostQueuedCompletionStatus`
//!    and frees its heap context.

use std::cell::UnsafeCell;
use std::collections::HashMap;
use std::ffi::c_void;

use num_traits::ToPrimitive;

use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, ERROR_BROKEN_PIPE, ERROR_IO_PENDING, ERROR_MORE_DATA,
    ERROR_NOT_FOUND, ERROR_OPERATION_ABORTED, ERROR_PIPE_CONNECTED, ERROR_SUCCESS, GENERIC_READ,
    GENERIC_WRITE, HANDLE, WAIT_TIMEOUT,
};
use windows_sys::Win32::Networking::WinSock as ws;
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, ReadFile, WriteFile, FILE_FLAG_OVERLAPPED, OPEN_EXISTING,
};
use windows_sys::Win32::System::Threading::{
    CreateEventW, RegisterWaitForSingleObject, ResetEvent, SetEvent, UnregisterWait,
    UnregisterWaitEx, WT_EXECUTEINWAITTHREAD, WT_EXECUTEONLYONCE,
};
use windows_sys::Win32::System::IO::{
    CancelIoEx, CreateIoCompletionPort, GetOverlappedResult, GetQueuedCompletionStatus,
    PostQueuedCompletionStatus, OVERLAPPED,
};

use crate::error::{type_error, value_error, RuntimeError};
use crate::import::ModuleCache;
use crate::object::{BuiltinFn, DictData, DictKey, MethodWrapper, Object, PyModule, PyProperty};
use crate::stdlib::nt_support::{last_win32_error_to_py, wide, win32_error_to_py};
use crate::sync::Rc;
use crate::sync::RefCell;
use crate::types::{PyInstance, TypeFlags, TypeObject};

// `ntdef.h` STATUS_PENDING: `HasOverlappedIoCompleted(o)` is
// `o->Internal != STATUS_PENDING`.
const STATUS_PENDING: usize = 0x103;

/// Win32 verdict of a `SOCKET_ERROR`-convention Winsock start call.
fn wsa_start_err(ret: i32) -> u32 {
    if ret < 0 {
        unsafe { ws::WSAGetLastError() as u32 }
    } else {
        ERROR_SUCCESS
    }
}

/// Win32 verdict of a BOOL-returning Winsock (extension) call.
fn wsa_bool_err(ret: i32) -> u32 {
    if ret == 0 {
        unsafe { ws::WSAGetLastError() as u32 }
    } else {
        ERROR_SUCCESS
    }
}

pub fn build(_cache: &ModuleCache) -> Rc<PyModule> {
    // CPython's module exec imports `_socket` first so WSAStartup has
    // run before `initialize_function_pointers`. WeavePy's `_socket`
    // initialises Winsock lazily through std/socket2, so the module
    // arms Winsock itself — WSAStartup is per-process refcounted, so
    // doubling up with `_socket` is harmless.
    ensure_winsock();

    let dict = Rc::new(RefCell::new(DictData::default()));
    {
        let mut d = dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("__name__")),
            Object::from_static("_overlapped"),
        );
        d.insert(
            DictKey(Object::from_static("__doc__")),
            Object::from_static("_overlapped module (RFC 0063; CPython Modules/overlapped.c)"),
        );

        for (name, f) in [
            (
                "CreateIoCompletionPort",
                mod_create_io_completion_port as fn(&[Object]) -> Result<Object, RuntimeError>,
            ),
            (
                "GetQueuedCompletionStatus",
                mod_get_queued_completion_status,
            ),
            (
                "PostQueuedCompletionStatus",
                mod_post_queued_completion_status,
            ),
            ("FormatMessage", mod_format_message),
            ("BindLocal", mod_bind_local),
            ("RegisterWaitWithQueue", mod_register_wait_with_queue),
            ("UnregisterWait", mod_unregister_wait),
            ("UnregisterWaitEx", mod_unregister_wait_ex),
            ("CreateEvent", mod_create_event),
            ("SetEvent", mod_set_event),
            ("ResetEvent", mod_reset_event),
            ("ConnectPipe", mod_connect_pipe),
            ("WSAConnect", mod_wsa_connect),
        ] {
            d.insert(DictKey(Object::from_static(name)), b(name, f));
        }

        d.insert(
            DictKey(Object::from_static("Overlapped")),
            Object::Type(overlapped_type()),
        );

        // The constant family `overlapped_exec` publishes. Handles are
        // unsigned (`F_HANDLE` is "K"), so `INVALID_HANDLE_VALUE` is
        // 2**64-1 on Win64 exactly as CPython exposes it.
        for (name, val) in [
            ("ERROR_IO_PENDING", i64::from(ERROR_IO_PENDING)),
            (
                "ERROR_NETNAME_DELETED",
                i64::from(windows_sys::Win32::Foundation::ERROR_NETNAME_DELETED),
            ),
            (
                "ERROR_OPERATION_ABORTED",
                i64::from(ERROR_OPERATION_ABORTED),
            ),
            (
                "ERROR_SEM_TIMEOUT",
                i64::from(windows_sys::Win32::Foundation::ERROR_SEM_TIMEOUT),
            ),
            (
                "ERROR_PIPE_BUSY",
                i64::from(windows_sys::Win32::Foundation::ERROR_PIPE_BUSY),
            ),
            (
                "ERROR_PORT_UNREACHABLE",
                i64::from(windows_sys::Win32::Foundation::ERROR_PORT_UNREACHABLE),
            ),
            ("INFINITE", i64::from(u32::MAX)),
            ("NULL", 0),
            (
                "SO_UPDATE_ACCEPT_CONTEXT",
                i64::from(ws::SO_UPDATE_ACCEPT_CONTEXT),
            ),
            (
                "SO_UPDATE_CONNECT_CONTEXT",
                i64::from(ws::SO_UPDATE_CONNECT_CONTEXT),
            ),
            ("TF_REUSE_SOCKET", i64::from(ws::TF_REUSE_SOCKET)),
        ] {
            d.insert(DictKey(Object::from_static(name)), Object::Int(val));
        }
        d.insert(
            DictKey(Object::from_static("INVALID_HANDLE_VALUE")),
            uint_obj(usize::MAX),
        );
    }
    Rc::new(PyModule {
        name: "_overlapped".to_owned(),
        filename: None,
        dict,
    })
}

// ---------------------------------------------------------------------------
// Small builders / argument converters.
// ---------------------------------------------------------------------------

fn b(name: &'static str, body: fn(&[Object]) -> Result<Object, RuntimeError>) -> Object {
    Object::Builtin(Rc::new(BuiltinFn {
        name,
        binds_instance: false,
        call: Box::new(body),
        call_kw: None,
    }))
}

fn method(name: &'static str, body: fn(&[Object]) -> Result<Object, RuntimeError>) -> Object {
    Object::Builtin(Rc::new(BuiltinFn {
        name,
        binds_instance: true,
        call: Box::new(body),
        call_kw: None,
    }))
}

/// An unsigned pointer-sized value as a Python int — CPython's
/// `F_HANDLE`/`F_ULONG_PTR` ("K") return convention for handles, keys
/// and `OVERLAPPED` addresses.
fn uint_obj(v: usize) -> Object {
    Object::int_from_i128(v as i128)
}

/// `F_HANDLE`/`F_ULONG_PTR` argument: a Python int reinterpreted as a
/// pointer-sized unsigned (CPython goes through `PyLong_AsVoidPtr`, so
/// both -1 and 2**64-1 name `INVALID_HANDLE_VALUE`).
fn uintptr_arg(o: Option<&Object>, name: &str) -> Result<usize, RuntimeError> {
    match o {
        Some(Object::Int(n)) => Ok(*n as usize),
        Some(Object::Bool(v)) => Ok(usize::from(*v)),
        Some(Object::Long(big)) => big
            .to_u64()
            .map(|v| v as usize)
            .or_else(|| big.to_i64().map(|v| v as usize))
            .ok_or_else(|| {
                crate::error::overflow_error(format!("{name} does not fit in a HANDLE"))
            }),
        Some(other) => Err(type_error(format!(
            "{name} must be an int, not {}",
            other.type_name_owned()
        ))),
        None => Err(type_error(format!("missing required argument {name}"))),
    }
}

/// `F_DWORD` ("k") argument: unsigned-long with CPython's mask
/// semantics (`PyLong_AsUnsignedLongMask` wraps out-of-range values).
fn dword_arg(o: Option<&Object>, name: &str) -> Result<u32, RuntimeError> {
    Ok(uintptr_arg(o, name)? as u32)
}

/// `F_BOOL` ("i") argument, defaulting when absent.
fn bool_arg(o: Option<&Object>, default: bool) -> bool {
    match o {
        None | Some(Object::None) => default,
        Some(v) => v.is_truthy(),
    }
}

/// `y*`-style read buffer: copied out, because the started operation
/// owns its bytes for the whole kernel lifetime (see [`Op`]).
fn bytes_like(o: Option<&Object>, func: &str) -> Result<Vec<u8>, RuntimeError> {
    match o {
        Some(Object::Bytes(bs)) => Ok(bs.to_vec()),
        Some(Object::ByteArray(bs)) => Ok(bs.borrow().clone()),
        Some(Object::MemoryView(mv)) => Ok(mv.to_bytes()),
        Some(other) => Err(type_error(format!(
            "{func}() argument must be a bytes-like object, not '{}'",
            other.type_name_owned()
        ))),
        None => Err(type_error(format!("{func}() missing buffer argument"))),
    }
}

/// A writable Python buffer target for the `*Into` operations. Returns
/// its writable byte length; the object itself is pinned on the
/// instance for the operation lifetime and filled at `getresult` time.
fn writable_len(o: &Object, func: &str) -> Result<usize, RuntimeError> {
    match o {
        Object::ByteArray(bs) => Ok(bs.borrow().len()),
        Object::MemoryView(mv) => {
            if mv.readonly.get() {
                return Err(type_error(format!(
                    "{func}() argument must be read-write buffer"
                )));
            }
            Ok(mv.len.get())
        }
        other => Err(type_error(format!(
            "{func}() argument must be a writable bytes-like object, not '{}'",
            other.type_name_owned()
        ))),
    }
}

/// Copy received bytes back into the pinned Python buffer. WeavePy
/// diverges here from CPython by one step: the kernel writes into a
/// module-owned staging `Vec` (whose address is guaranteed stable) and
/// the bytes land in the user object when `getresult()` collects the
/// operation — the only point the proactor reads the buffer. Direct
/// kernel writes into a `bytearray`'s heap allocation would race any
/// Python-side resize while the operation is pending.
fn copy_out(target: &Object, data: &[u8]) {
    match target {
        Object::ByteArray(bs) => {
            let mut v = bs.borrow_mut();
            let n = data.len().min(v.len());
            v[..n].copy_from_slice(&data[..n]);
        }
        Object::MemoryView(mv) => {
            let start = mv.start.get();
            let len = mv.len.get();
            mv.buffer.with_write(|s| {
                let end = (start + len).min(s.len());
                if start < end {
                    let window = &mut s[start..end];
                    let n = data.len().min(window.len());
                    window[..n].copy_from_slice(&data[..n]);
                }
            });
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// Winsock arming + the Mswsock extension functions.
// ---------------------------------------------------------------------------

fn ensure_winsock() {
    static ARMED: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    ARMED.get_or_init(|| {
        let mut data: ws::WSADATA = unsafe { std::mem::zeroed() };
        // 2.2, like CPython's socketmodule. Failure is surfaced later by
        // the first Winsock call (WSANOTINITIALISED), same as CPython.
        unsafe { ws::WSAStartup(0x0202, &raw mut data) };
    });
}

/// The AcceptEx/ConnectEx/DisconnectEx/TransmitFile entry points do not
/// live in ws2_32's import table; they are fetched per-provider through
/// `WSAIoctl(SIO_GET_EXTENSION_FUNCTION_POINTER)` — CPython's
/// `initialize_function_pointers`, verbatim.
#[derive(Clone, Copy)]
struct WsaExtFns {
    accept_ex: ws::LPFN_ACCEPTEX,
    connect_ex: ws::LPFN_CONNECTEX,
    disconnect_ex: ws::LPFN_DISCONNECTEX,
    transmit_file: ws::LPFN_TRANSMITFILE,
}

fn ext_fns() -> Result<WsaExtFns, RuntimeError> {
    static FNS: std::sync::OnceLock<Result<WsaExtFns, i32>> = std::sync::OnceLock::new();
    FNS.get_or_init(|| {
        ensure_winsock();
        let s = unsafe { ws::socket(i32::from(ws::AF_INET), ws::SOCK_STREAM, ws::IPPROTO_TCP) };
        if s == ws::INVALID_SOCKET {
            return Err(unsafe { ws::WSAGetLastError() });
        }
        let mut fns = WsaExtFns {
            accept_ex: None,
            connect_ex: None,
            disconnect_ex: None,
            transmit_file: None,
        };
        // SAFETY: each output slot is a pointer-sized, null-niched
        // `Option<fn>` — exactly the out-buffer WSAIoctl expects.
        let load = |guid: windows_sys::core::GUID, out: *mut c_void, out_len: u32| -> bool {
            let mut bytes = 0u32;
            let rc = unsafe {
                ws::WSAIoctl(
                    s,
                    ws::SIO_GET_EXTENSION_FUNCTION_POINTER,
                    std::ptr::from_ref(&guid).cast::<c_void>().cast_mut(),
                    u32::try_from(std::mem::size_of::<windows_sys::core::GUID>()).unwrap(),
                    out,
                    out_len,
                    &raw mut bytes,
                    std::ptr::null_mut(),
                    None,
                )
            };
            rc != ws::SOCKET_ERROR
        };
        let fn_size = u32::try_from(std::mem::size_of::<ws::LPFN_ACCEPTEX>()).unwrap();
        let ok = load(
            ws::WSAID_ACCEPTEX,
            std::ptr::from_mut(&mut fns.accept_ex).cast(),
            fn_size,
        ) && load(
            ws::WSAID_CONNECTEX,
            std::ptr::from_mut(&mut fns.connect_ex).cast(),
            fn_size,
        ) && load(
            ws::WSAID_DISCONNECTEX,
            std::ptr::from_mut(&mut fns.disconnect_ex).cast(),
            fn_size,
        ) && load(
            ws::WSAID_TRANSMITFILE,
            std::ptr::from_mut(&mut fns.transmit_file).cast(),
            fn_size,
        );
        let err = unsafe { ws::WSAGetLastError() };
        unsafe { ws::closesocket(s) };
        if ok {
            Ok(fns)
        } else {
            Err(err)
        }
    })
    .map_err(|code| win32_error_to_py(code, None))
}

// ---------------------------------------------------------------------------
// Socket addresses (overlapped.c `parse_address` / `unparse_address`).
// ---------------------------------------------------------------------------

enum SockAddrBuf {
    V4(ws::SOCKADDR_IN),
    V6(ws::SOCKADDR_IN6),
}

impl SockAddrBuf {
    fn as_ptr(&self) -> *const ws::SOCKADDR {
        match self {
            SockAddrBuf::V4(a) => std::ptr::from_ref(a).cast(),
            SockAddrBuf::V6(a) => std::ptr::from_ref(a).cast(),
        }
    }
    fn len(&self) -> i32 {
        match self {
            SockAddrBuf::V4(_) => std::mem::size_of::<ws::SOCKADDR_IN>() as i32,
            SockAddrBuf::V6(_) => std::mem::size_of::<ws::SOCKADDR_IN6>() as i32,
        }
    }
}

fn tuple_str(o: Option<&Object>, what: &str) -> Result<String, RuntimeError> {
    match o {
        Some(Object::Str(s)) => Ok(s.to_string()),
        _ => Err(type_error(format!("{what} must be str"))),
    }
}

fn tuple_u16(o: Option<&Object>, what: &str) -> Result<u16, RuntimeError> {
    match o {
        Some(Object::Int(n)) if (0..=i64::from(u16::MAX)).contains(n) => Ok(*n as u16),
        Some(Object::Int(_) | Object::Long(_)) => Err(crate::error::overflow_error(format!(
            "{what} must be in range(0, 65536)"
        ))),
        Some(Object::Bool(v)) => Ok(u16::from(*v)),
        _ => Err(type_error(format!("{what} must be int"))),
    }
}

/// A `(host, port)` / `(host, port, flowinfo, scopeid)` tuple to a
/// Winsock sockaddr. CPython routes the host text through
/// `WSAStringToAddressW`, which only accepts numeric literals — the
/// std parsers cover the same forms (asyncio always hands this
/// getaddrinfo-resolved numerics).
fn parse_address(o: Option<&Object>) -> Result<SockAddrBuf, RuntimeError> {
    let items: Vec<Object> = match o {
        Some(Object::Tuple(t)) => t.to_vec(),
        Some(Object::List(l)) => l.borrow().clone(),
        _ => return Err(type_error("address must be a tuple")),
    };
    match items.len() {
        2 => {
            let host = tuple_str(items.first(), "address host")?;
            let port = tuple_u16(items.get(1), "address port")?;
            let ip: std::net::Ipv4Addr = host
                .parse()
                .map_err(|_| value_error(format!("invalid IPv4 address: '{host}'")))?;
            let mut sa: ws::SOCKADDR_IN = unsafe { std::mem::zeroed() };
            sa.sin_family = ws::AF_INET;
            sa.sin_port = port.to_be();
            sa.sin_addr = ws::IN_ADDR {
                S_un: ws::IN_ADDR_0 {
                    S_addr: u32::from(ip).to_be(),
                },
            };
            Ok(SockAddrBuf::V4(sa))
        }
        4 => {
            let host = tuple_str(items.first(), "address host")?;
            let port = tuple_u16(items.get(1), "address port")?;
            let flowinfo = dword_arg(items.get(2), "flowinfo")?;
            let scope_id = dword_arg(items.get(3), "scopeid")?;
            let ip: std::net::Ipv6Addr = host
                .parse()
                .map_err(|_| value_error(format!("invalid IPv6 address: '{host}'")))?;
            let mut sa: ws::SOCKADDR_IN6 = unsafe { std::mem::zeroed() };
            sa.sin6_family = ws::AF_INET6;
            sa.sin6_port = port.to_be();
            // CPython stores FlowInfo without byte-swapping (parse_address
            // assigns it raw); mirrored bug-for-bug.
            sa.sin6_flowinfo = flowinfo;
            sa.sin6_addr = ws::IN6_ADDR {
                u: ws::IN6_ADDR_0 { Byte: ip.octets() },
            };
            sa.Anonymous = ws::SOCKADDR_IN6_0 {
                sin6_scope_id: scope_id,
            };
            Ok(SockAddrBuf::V6(sa))
        }
        _ => Err(value_error("expected tuple of length 2 or 4")),
    }
}

/// The reverse direction, for `WSARecvFrom` results (overlapped.c
/// `unparse_address`): `(host, port)` for v4, `(host, port, flowinfo,
/// scopeid)` for v6.
fn unparse_address(sa: &ws::SOCKADDR_IN6) -> Result<Object, RuntimeError> {
    let family = sa.sin6_family;
    if family == ws::AF_INET {
        // SAFETY: family says the storage actually holds a SOCKADDR_IN.
        let v4: &ws::SOCKADDR_IN = unsafe { &*std::ptr::from_ref(sa).cast() };
        let ip = std::net::Ipv4Addr::from(u32::from_be(unsafe { v4.sin_addr.S_un.S_addr }));
        Ok(Object::new_tuple(vec![
            Object::from_str(ip.to_string()),
            Object::Int(i64::from(u16::from_be(v4.sin_port))),
        ]))
    } else if family == ws::AF_INET6 {
        let ip = std::net::Ipv6Addr::from(unsafe { sa.sin6_addr.u.Byte });
        Ok(Object::new_tuple(vec![
            Object::from_str(ip.to_string()),
            Object::Int(i64::from(u16::from_be(sa.sin6_port))),
            // ntohl on unparse, mirroring CPython's asymmetric handling.
            Object::Int(i64::from(u32::from_be(sa.sin6_flowinfo))),
            Object::Int(i64::from(unsafe { sa.Anonymous.sin6_scope_id })),
        ]))
    } else {
        Err(value_error("recvfrom returned unsupported address family"))
    }
}

// ---------------------------------------------------------------------------
// The native operation block behind each Overlapped instance.
// ---------------------------------------------------------------------------

/// Where `WSARecvFrom(Into)` tells the kernel to deposit the sender's
/// address. `UnsafeCell` because the kernel writes these fields from an
/// arbitrary thread while the operation is pending.
struct FromAddr {
    sa: UnsafeCell<ws::SOCKADDR_IN6>,
    len: UnsafeCell<i32>,
}

impl FromAddr {
    fn new() -> Self {
        FromAddr {
            sa: UnsafeCell::new(unsafe { std::mem::zeroed() }),
            len: UnsafeCell::new(std::mem::size_of::<ws::SOCKADDR_IN6>() as i32),
        }
    }
}

/// The operation kind + the buffers it owns — CPython's `type` enum and
/// buffer union rolled together. An active operation OWNS its bytes:
/// the kernel keeps raw pointers into these `Vec`s until completion, so
/// they must never reallocate (they are written once at start and only
/// read back after completion) and the whole block must not drop while
/// an operation is in flight (see `ov_del`).
enum Op {
    /// Freshly constructed — no operation attempted.
    None,
    /// The last start attempt failed; buffers released (CPython's
    /// `TYPE_NOT_STARTED` after `Overlapped_clear`).
    NotStarted,
    /// `ReadFile`/`WSARecv`: module-allocated read target.
    Read {
        buf: Vec<u8>,
    },
    /// `ReadFileInto`/`WSARecvInto`: staging buffer; the user object is
    /// pinned on the instance and filled by `getresult`.
    ReadInto {
        buf: Vec<u8>,
    },
    /// `WriteFile`/`WSASend`/`WSASendTo`: a private copy of the caller's
    /// bytes, pinned for the kernel (field kept only for ownership).
    Write {
        _pinned: Vec<u8>,
    },
    /// `AcceptEx`: the `(sockaddr size + 16) * 2` address buffer.
    Accept {
        _pinned: Vec<u8>,
    },
    Connect,
    Disconnect,
    TransmitFile,
    ConnectNamedPipe,
    /// `WSARecvFrom`.
    ReadFrom {
        buf: Vec<u8>,
        addr: FromAddr,
    },
    /// `WSARecvFromInto`.
    ReadFromInto {
        buf: Vec<u8>,
        addr: FromAddr,
    },
}

impl Op {
    fn attempted(&self) -> bool {
        !matches!(self, Op::None)
    }
}

/// The stable-address native payload of one `Overlapped` instance. The
/// registry owns it boxed; its address never changes between the start
/// of an operation and `ov_del`, which is what makes `.address` a valid
/// completion key and keeps the kernel's pointers alive.
struct OvBlock {
    /// `UnsafeCell`: the kernel writes `Internal`/`InternalHigh` (and
    /// the IOCP machinery reads `hEvent`) concurrently with GIL-side
    /// reads. All access goes through raw pointers.
    ov: UnsafeCell<OVERLAPPED>,
    /// Handle/SOCKET of the operation in flight (CPython stores it too,
    /// for `GetOverlappedResult`/`CancelIoEx`).
    handle: usize,
    /// Win32 error of the last start call / `getresult` — the `.error`
    /// attribute.
    error: u32,
    op: Op,
}

// SAFETY: the raw pointers inside OVERLAPPED are kernel identifiers,
// not thread-affine data; the registry mutex serialises all Rust-side
// access, and kernel-side writes only touch the UnsafeCell interiors.
unsafe impl Send for OvBlock {}

impl OvBlock {
    fn ov_ptr(&self) -> *mut OVERLAPPED {
        self.ov.get()
    }

    fn address(&self) -> usize {
        self.ov.get() as usize
    }

    fn h_event(&self) -> HANDLE {
        // SAFETY: hEvent is only written GIL-side (construction).
        unsafe { (*self.ov_ptr()).hEvent }
    }

    /// `HasOverlappedIoCompleted` — volatile read because the kernel
    /// flips `Internal` from `STATUS_PENDING` on completion.
    fn completed(&self) -> bool {
        let internal =
            unsafe { std::ptr::read_volatile(std::ptr::addr_of!((*self.ov_ptr()).Internal)) };
        internal != STATUS_PENDING
    }

    /// overlapped.c `mark_as_completed`: a start call that failed with
    /// `ERROR_BROKEN_PIPE` will never post a completion, so flag the
    /// struct done (and signal the event) to keep `pending`/dealloc
    /// truthful.
    fn mark_as_completed(&self) {
        unsafe {
            std::ptr::write_volatile(std::ptr::addr_of_mut!((*self.ov_ptr()).Internal), 0);
        }
        let ev = self.h_event();
        if !ev.is_null() {
            unsafe { SetEvent(ev) };
        }
    }
}

/// Process-global block registry, keyed by `OVERLAPPED` address. The
/// proactor thread runs `GetQueuedCompletionStatus` GIL-released while
/// other threads construct/destroy `Overlapped`s, so the table itself
/// takes a real mutex; block interiors are only touched with the GIL
/// held (plus the kernel through the `UnsafeCell`s).
fn registry() -> &'static parking_lot::Mutex<HashMap<usize, Box<OvBlock>>> {
    static REGISTRY: std::sync::OnceLock<parking_lot::Mutex<HashMap<usize, Box<OvBlock>>>> =
        std::sync::OnceLock::new();
    REGISTRY.get_or_init(|| parking_lot::Mutex::new(HashMap::new()))
}

// ---------------------------------------------------------------------------
// The Overlapped type.
// ---------------------------------------------------------------------------

fn overlapped_type() -> Rc<TypeObject> {
    let bt = crate::builtin_types::builtin_types();
    let mut td = DictData::default();
    for (name, f) in [
        (
            "getresult",
            ov_getresult as fn(&[Object]) -> Result<Object, RuntimeError>,
        ),
        ("cancel", ov_cancel),
        ("ReadFile", ov_read_file),
        ("ReadFileInto", ov_read_file_into),
        ("WSARecv", ov_wsa_recv),
        ("WSARecvInto", ov_wsa_recv_into),
        ("WSARecvFrom", ov_wsa_recv_from),
        ("WSARecvFromInto", ov_wsa_recv_from_into),
        ("WriteFile", ov_write_file),
        ("WSASend", ov_wsa_send),
        ("WSASendTo", ov_wsa_send_to),
        ("AcceptEx", ov_accept_ex),
        ("ConnectEx", ov_connect_ex),
        ("DisconnectEx", ov_disconnect_ex),
        ("TransmitFile", ov_transmit_file),
        ("ConnectNamedPipe", ov_connect_named_pipe),
        ("__del__", ov_del),
    ] {
        td.insert(DictKey(Object::from_static(name)), method(name, f));
    }
    // CPython exposes `error`/`event` as members and `address`/
    // `pending` as getsets; all four are dynamic reads of the block.
    for (name, getter) in [
        (
            "address",
            ov_get_address as fn(&[Object]) -> Result<Object, RuntimeError>,
        ),
        ("pending", ov_get_pending),
        ("error", ov_get_error),
        ("event", ov_get_event),
    ] {
        td.insert(
            DictKey(Object::from_static(name)),
            Object::Property(Rc::new(PyProperty::new(
                method(name, getter),
                Object::None,
                Object::None,
                Object::None,
            ))),
        );
    }
    td.insert(
        DictKey(Object::from_static("__module__")),
        Object::from_static("_overlapped"),
    );
    // Construction lives in __new__ (CPython's tp_new); __init__ is a
    // permissive no-op so `type.__call__`'s argument pass-through does
    // not trip object.__init__ arity checks (same shape as mmap.mmap).
    td.insert(
        DictKey(Object::from_static("__new__")),
        Object::StaticMethod(MethodWrapper::new(Object::Builtin(Rc::new(BuiltinFn {
            name: "Overlapped.__new__",
            binds_instance: false,
            call: Box::new(|args| ov_new(args, &[])),
            call_kw: Some(Box::new(ov_new)),
        })))),
    );
    td.insert(
        DictKey(Object::from_static("__init__")),
        Object::Builtin(Rc::new(BuiltinFn {
            name: "__init__",
            binds_instance: true,
            call: Box::new(|_args| Ok(Object::None)),
            call_kw: Some(Box::new(|_args, _kwargs| Ok(Object::None))),
        })),
    );
    TypeObject::new_with_flags(
        "Overlapped",
        vec![bt.object_.clone()],
        td,
        TypeFlags {
            is_exception: false,
            is_builtin: true,
        },
    )
    .expect("_overlapped.Overlapped must linearise")
}

/// `Overlapped(event=INVALID_HANDLE_VALUE)`: the sentinel default means
/// "make me a manual-reset, non-signalled event"; `NULL` (what asyncio
/// passes — completion arrives through the port) means no event at all.
fn ov_new(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let Some(Object::Type(cls)) = args.first() else {
        return Err(type_error("Overlapped.__new__(X): X is not a type object"));
    };
    if args.len() > 2 {
        return Err(type_error("Overlapped() takes at most 1 argument"));
    }
    let mut event_obj = args.get(1).cloned();
    for (k, v) in kwargs {
        if k == "event" {
            if event_obj.is_some() {
                return Err(type_error(
                    "argument for Overlapped() given by name ('event') and position (1)",
                ));
            }
            event_obj = Some(v.clone());
        } else {
            return Err(type_error(format!(
                "'{k}' is an invalid keyword argument for Overlapped()"
            )));
        }
    }
    let mut event = match event_obj {
        Some(o) => uintptr_arg(Some(&o), "event")?,
        None => usize::MAX, // INVALID_HANDLE_VALUE
    };
    if event == usize::MAX {
        let created = unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) };
        if created.is_null() {
            return Err(last_win32_error_to_py(None));
        }
        event = created as usize;
    }

    let mut ov = OVERLAPPED::default();
    ov.hEvent = event as HANDLE;
    let block = Box::new(OvBlock {
        ov: UnsafeCell::new(ov),
        handle: 0,
        error: 0,
        op: Op::None,
    });
    let address = block.address();
    registry().lock().insert(address, block);

    let inst = Rc::new(PyInstance::new(cls.clone()));
    inst.dict
        .borrow_mut()
        .insert(DictKey(Object::from_static("_address")), uint_obj(address));
    Ok(Object::Instance(inst))
}

fn self_arg(args: &[Object]) -> Result<Rc<PyInstance>, RuntimeError> {
    match args.first() {
        Some(Object::Instance(i)) => Ok(i.clone()),
        _ => Err(type_error("Overlapped method: missing self")),
    }
}

/// The registry key of an instance's block, from the `_address` slot
/// minted at construction.
fn block_key(inst: &Rc<PyInstance>) -> Result<usize, RuntimeError> {
    let addr = inst
        .dict
        .borrow()
        .get(&DictKey(Object::from_static("_address")))
        .cloned();
    uintptr_arg(addr.as_ref(), "Overlapped address")
        .map_err(|_| crate::error::os_error("Overlapped object has no native state"))
}

/// Run `f` with the registry locked and the instance's block borrowed.
fn with_block<R>(
    args: &[Object],
    f: impl FnOnce(&mut OvBlock) -> Result<R, RuntimeError>,
) -> Result<R, RuntimeError> {
    let inst = self_arg(args)?;
    let key = block_key(&inst)?;
    let mut map = registry().lock();
    let block = map
        .get_mut(&key)
        .ok_or_else(|| crate::error::os_error("Overlapped object has no native state"))?;
    f(block)
}

// -- attribute getters -------------------------------------------------------

fn ov_get_address(args: &[Object]) -> Result<Object, RuntimeError> {
    with_block(args, |blk| Ok(uint_obj(blk.address())))
}

fn ov_get_pending(args: &[Object]) -> Result<Object, RuntimeError> {
    with_block(args, |blk| {
        // overlapped.c Overlapped_getpending: in flight and the start
        // did not fail. A never-attempted Overlapped reads "completed"
        // (Internal is zero), so `pending` is False, as in CPython.
        Ok(Object::Bool(
            !blk.completed() && !matches!(blk.op, Op::NotStarted),
        ))
    })
}

fn ov_get_error(args: &[Object]) -> Result<Object, RuntimeError> {
    with_block(args, |blk| Ok(Object::Int(i64::from(blk.error))))
}

fn ov_get_event(args: &[Object]) -> Result<Object, RuntimeError> {
    with_block(args, |blk| Ok(uint_obj(blk.h_event() as usize)))
}

// -- lifecycle ----------------------------------------------------------------

/// overlapped.c `Overlapped_dealloc`: an in-flight operation must be
/// cancelled and *drained* before its memory can be released — the
/// kernel owns pointers into the block until then.
fn ov_del(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = self_arg(args)?;
    let Ok(key) = block_key(&inst) else {
        return Ok(Object::None);
    };
    let Some(block) = registry().lock().remove(&key) else {
        return Ok(Object::None);
    };
    if !block.completed() && block.op.attempted() && !matches!(block.op, Op::NotStarted) {
        let handle = block.handle as HANDLE;
        let ov_ptr = block.ov_ptr();
        let drained = crate::gil::allow_threads_then(|| {
            let mut wait = 0;
            if unsafe { CancelIoEx(handle, ov_ptr) } != 0 {
                wait = 1;
            }
            let mut bytes = 0u32;
            let ret = unsafe { GetOverlappedResult(handle, ov_ptr, &raw mut bytes, wait) };
            let err = if ret != 0 {
                ERROR_SUCCESS
            } else {
                unsafe { GetLastError() }
            };
            matches!(
                err,
                ERROR_SUCCESS | ERROR_NOT_FOUND | ERROR_OPERATION_ABORTED
            )
        });
        if !drained {
            // CPython prints an unraisable "still has pending operation
            // at deallocation, the process may crash" and frees anyway.
            // Leaking the block (event handle included) is strictly
            // safer: the kernel may still write through its pointers.
            std::mem::forget(block);
            return Ok(Object::None);
        }
    }
    let ev = block.h_event();
    if !ev.is_null() {
        unsafe { CloseHandle(ev) };
    }
    Ok(Object::None)
}

// -- getresult / cancel --------------------------------------------------------

fn ov_getresult(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = self_arg(args)?;
    let wait = bool_arg(args.get(1), false);
    let key = block_key(&inst)?;

    // Snapshot the raw pointers under the lock, then release it for the
    // (possibly blocking, GIL-released) GetOverlappedResult. The block
    // cannot vanish meanwhile: `inst` holds it live through `_address`
    // and `ov_del` only runs at refcount zero.
    let (handle, ov_ptr) = {
        let mut map = registry().lock();
        let block = map
            .get_mut(&key)
            .ok_or_else(|| crate::error::os_error("Overlapped object has no native state"))?;
        match block.op {
            Op::None => return Err(value_error("operation not yet attempted")),
            Op::NotStarted => return Err(value_error("operation failed to start")),
            _ => {}
        }
        (block.handle as HANDLE, block.ov_ptr())
    };

    let mut transferred = 0u32;
    let ret = crate::gil::allow_threads_then(|| unsafe {
        GetOverlappedResult(handle, ov_ptr, &raw mut transferred, i32::from(wait))
    });
    let err = if ret != 0 {
        ERROR_SUCCESS
    } else {
        unsafe { GetLastError() }
    };

    // Pull the target object (for the *Into ops) before relocking.
    let target = inst
        .dict
        .borrow()
        .get(&DictKey(Object::from_static("_ov_target")))
        .cloned();

    let mut map = registry().lock();
    let block = map
        .get_mut(&key)
        .ok_or_else(|| crate::error::os_error("Overlapped object has no native state"))?;
    block.error = err;
    let broken_pipe_ok = matches!(
        block.op,
        Op::Read { .. } | Op::ReadInto { .. } | Op::ReadFrom { .. }
    );
    match err {
        ERROR_SUCCESS | ERROR_MORE_DATA => {}
        // A broken pipe on a read means clean EOF-ish data (possibly
        // empty) for the read families; everything else raises. For
        // ReadFromInto CPython only tolerates it once a result tuple was
        // already built — first call raises, which is what this mirrors.
        ERROR_BROKEN_PIPE if broken_pipe_ok => {}
        _ => return Err(win32_error_to_py(err as i32, None)),
    }

    let n = transferred as usize;
    match &block.op {
        Op::Read { buf } => Ok(Object::Bytes(buf[..n.min(buf.len())].to_vec().into())),
        Op::ReadInto { buf } => {
            if let Some(t) = &target {
                copy_out(t, &buf[..n.min(buf.len())]);
            }
            Ok(Object::Int(i64::from(transferred)))
        }
        Op::ReadFrom { buf, addr } => {
            let sa = unsafe { *addr.sa.get() };
            Ok(Object::new_tuple(vec![
                Object::Bytes(buf[..n.min(buf.len())].to_vec().into()),
                unparse_address(&sa)?,
            ]))
        }
        Op::ReadFromInto { buf, addr } => {
            if let Some(t) = &target {
                copy_out(t, &buf[..n.min(buf.len())]);
            }
            let sa = unsafe { *addr.sa.get() };
            Ok(Object::new_tuple(vec![
                Object::Int(i64::from(transferred)),
                unparse_address(&sa)?,
            ]))
        }
        _ => Ok(Object::Int(i64::from(transferred))),
    }
}

fn ov_cancel(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = self_arg(args)?;
    let key = block_key(&inst)?;
    let (handle, ov_ptr, skip) = {
        let map = registry().lock();
        let block = map
            .get(&key)
            .ok_or_else(|| crate::error::os_error("Overlapped object has no native state"))?;
        let skip = matches!(block.op, Op::NotStarted) || block.completed();
        (block.handle as HANDLE, block.ov_ptr(), skip)
    };
    if skip {
        return Ok(Object::None);
    }
    let ret = crate::gil::allow_threads_then(|| unsafe { CancelIoEx(handle, ov_ptr) });
    // ERROR_NOT_FOUND: the I/O completed in-between — not an error.
    if ret == 0 {
        let err = unsafe { GetLastError() };
        if err != ERROR_NOT_FOUND {
            return Err(win32_error_to_py(err as i32, None));
        }
    }
    Ok(Object::None)
}

// -- operation starters ---------------------------------------------------------

/// Stage an operation: verify no prior attempt, record handle + kind
/// (with its pinned buffers) and hand back the raw pointers the actual
/// syscall needs. Setting `op` *before* the GIL-released syscall is
/// what makes a concurrent second start observe "already attempted",
/// same as CPython setting `self->type` pre-`Py_BEGIN_ALLOW_THREADS`.
fn stage_op(
    inst: &Rc<PyInstance>,
    handle: usize,
    op: Op,
) -> Result<(usize, *mut OVERLAPPED), RuntimeError> {
    let key = block_key(inst)?;
    let mut map = registry().lock();
    let block = map
        .get_mut(&key)
        .ok_or_else(|| crate::error::os_error("Overlapped object has no native state"))?;
    if block.op.attempted() {
        return Err(value_error("operation already attempted"));
    }
    block.handle = handle;
    block.op = op;
    Ok((key, block.ov_ptr()))
}

/// Raw pointer/length of an `Op`-owned buffer (queried back from the
/// staged block so the pointer is the one the kernel will keep).
fn staged_buf(key: usize) -> (*mut u8, u32) {
    let mut map = registry().lock();
    let block = map.get_mut(&key).expect("staged block must exist");
    match &mut block.op {
        Op::Read { buf }
        | Op::ReadInto { buf }
        | Op::Write { _pinned: buf }
        | Op::Accept { _pinned: buf }
        | Op::ReadFrom { buf, .. }
        | Op::ReadFromInto { buf, .. } => (buf.as_mut_ptr(), buf.len() as u32),
        _ => (std::ptr::null_mut(), 0),
    }
}

/// Record the start-call verdict, shared by every starter: `PENDING`/
/// success family returns `None` to Python; a broken pipe on the read
/// family is marked completed then raised (BrokenPipeError via the
/// errmap — the proactor's `except BrokenPipeError` path); any other
/// error clears the op to NOT_STARTED (buffers freed, kernel holds
/// nothing) and raises.
fn finish_start(
    key: usize,
    err: u32,
    broken_pipe_completes: bool,
    ok: Object,
) -> Result<Object, RuntimeError> {
    let mut map = registry().lock();
    let block = map.get_mut(&key).expect("staged block must exist");
    block.error = err;
    match err {
        ERROR_BROKEN_PIPE if broken_pipe_completes => {
            block.mark_as_completed();
            Err(win32_error_to_py(err as i32, None))
        }
        ERROR_SUCCESS | ERROR_IO_PENDING => Ok(ok),
        ERROR_MORE_DATA if broken_pipe_completes => Ok(ok),
        _ => {
            block.op = Op::NotStarted;
            Err(win32_error_to_py(err as i32, None))
        }
    }
}

/// Pin the user buffer object on the instance for the operation
/// lifetime (`*Into` ops) so it cannot be collected while pending.
fn pin_target(inst: &Rc<PyInstance>, target: &Object) {
    inst.dict
        .borrow_mut()
        .insert(DictKey(Object::from_static("_ov_target")), target.clone());
}

fn ov_read_file(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = self_arg(args)?;
    let handle = uintptr_arg(args.get(1), "handle")?;
    let size = dword_arg(args.get(2), "size")?;
    // CPython allocates max(size, 1) but issues the read for `size`
    // (a zero-byte overlapped read is valid).
    let buf = vec![0u8; (size as usize).max(1)];
    let (key, ov_ptr) = stage_op(&inst, handle, Op::Read { buf })?;
    let (ptr, _) = staged_buf(key);
    let mut nread = 0u32;
    let ret = crate::gil::allow_threads_then(|| unsafe {
        ReadFile(handle as HANDLE, ptr, size, &raw mut nread, ov_ptr)
    });
    let err = if ret != 0 {
        ERROR_SUCCESS
    } else {
        unsafe { GetLastError() }
    };
    finish_start(key, err, true, Object::None)
}

fn ov_read_file_into(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = self_arg(args)?;
    let handle = uintptr_arg(args.get(1), "handle")?;
    let target = args
        .get(2)
        .ok_or_else(|| type_error("ReadFileInto() missing buffer argument"))?
        .clone();
    let len = writable_len(&target, "ReadFileInto")?;
    let buf = vec![0u8; len.max(1)];
    pin_target(&inst, &target);
    let (key, ov_ptr) = stage_op(&inst, handle, Op::ReadInto { buf })?;
    let (ptr, _) = staged_buf(key);
    let mut nread = 0u32;
    let ret = crate::gil::allow_threads_then(|| unsafe {
        ReadFile(handle as HANDLE, ptr, len as u32, &raw mut nread, ov_ptr)
    });
    let err = if ret != 0 {
        ERROR_SUCCESS
    } else {
        unsafe { GetLastError() }
    };
    finish_start(key, err, true, Object::None)
}

fn ov_wsa_recv(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = self_arg(args)?;
    let handle = uintptr_arg(args.get(1), "handle")?;
    let size = dword_arg(args.get(2), "size")?;
    let mut flags = dword_arg(args.get(3), "flags").unwrap_or(0);
    let buf = vec![0u8; (size as usize).max(1)];
    let (key, ov_ptr) = stage_op(&inst, handle, Op::Read { buf })?;
    let (ptr, _) = staged_buf(key);
    let wsabuf = ws::WSABUF {
        len: size,
        buf: ptr,
    };
    let mut nread = 0u32;
    let ret = crate::gil::allow_threads_then(|| unsafe {
        ws::WSARecv(
            handle,
            &raw const wsabuf,
            1,
            &raw mut nread,
            &raw mut flags,
            ov_ptr,
            None,
        )
    });
    let err = wsa_start_err(ret);
    finish_start(key, err, true, Object::None)
}

fn ov_wsa_recv_into(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = self_arg(args)?;
    let handle = uintptr_arg(args.get(1), "handle")?;
    let target = args
        .get(2)
        .ok_or_else(|| type_error("WSARecvInto() missing buffer argument"))?
        .clone();
    let mut flags = dword_arg(args.get(3), "flags")?;
    let len = writable_len(&target, "WSARecvInto")?;
    let buf = vec![0u8; len.max(1)];
    pin_target(&inst, &target);
    let (key, ov_ptr) = stage_op(&inst, handle, Op::ReadInto { buf })?;
    let (ptr, _) = staged_buf(key);
    let wsabuf = ws::WSABUF {
        len: len as u32,
        buf: ptr,
    };
    let mut nread = 0u32;
    let ret = crate::gil::allow_threads_then(|| unsafe {
        ws::WSARecv(
            handle,
            &raw const wsabuf,
            1,
            &raw mut nread,
            &raw mut flags,
            ov_ptr,
            None,
        )
    });
    let err = wsa_start_err(ret);
    finish_start(key, err, true, Object::None)
}

fn ov_wsa_recv_from(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = self_arg(args)?;
    let handle = uintptr_arg(args.get(1), "handle")?;
    let size = dword_arg(args.get(2), "size")?;
    let mut flags = dword_arg(args.get(3), "flags").unwrap_or(0);
    let buf = vec![0u8; (size as usize).max(1)];
    let (key, ov_ptr) = stage_op(
        &inst,
        handle,
        Op::ReadFrom {
            buf,
            addr: FromAddr::new(),
        },
    )?;
    let (ptr, _) = staged_buf(key);
    let (sa_ptr, len_ptr) = staged_from_addr(key);
    let wsabuf = ws::WSABUF {
        len: size,
        buf: ptr,
    };
    let mut nread = 0u32;
    let ret = crate::gil::allow_threads_then(|| unsafe {
        ws::WSARecvFrom(
            handle,
            &raw const wsabuf,
            1,
            &raw mut nread,
            &raw mut flags,
            sa_ptr,
            len_ptr,
            ov_ptr,
            None,
        )
    });
    let err = wsa_start_err(ret);
    finish_start(key, err, true, Object::None)
}

fn ov_wsa_recv_from_into(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = self_arg(args)?;
    let handle = uintptr_arg(args.get(1), "handle")?;
    let target = args
        .get(2)
        .ok_or_else(|| type_error("WSARecvFromInto() missing buffer argument"))?
        .clone();
    let size = dword_arg(args.get(3), "size")?;
    let mut flags = dword_arg(args.get(4), "flags").unwrap_or(0);
    let len = writable_len(&target, "WSARecvFromInto")?;
    if len < size as usize {
        return Err(value_error(
            "nbytes is greater than the length of the buffer",
        ));
    }
    let buf = vec![0u8; (size as usize).max(1)];
    pin_target(&inst, &target);
    let (key, ov_ptr) = stage_op(
        &inst,
        handle,
        Op::ReadFromInto {
            buf,
            addr: FromAddr::new(),
        },
    )?;
    let (ptr, _) = staged_buf(key);
    let (sa_ptr, len_ptr) = staged_from_addr(key);
    let wsabuf = ws::WSABUF {
        len: size,
        buf: ptr,
    };
    let mut nread = 0u32;
    let ret = crate::gil::allow_threads_then(|| unsafe {
        ws::WSARecvFrom(
            handle,
            &raw const wsabuf,
            1,
            &raw mut nread,
            &raw mut flags,
            sa_ptr,
            len_ptr,
            ov_ptr,
            None,
        )
    });
    let err = wsa_start_err(ret);
    finish_start(key, err, true, Object::None)
}

/// Raw pointers to a staged `ReadFrom(Into)`'s address slots — fetched
/// from the block in place so they are the addresses the kernel keeps.
fn staged_from_addr(key: usize) -> (*mut ws::SOCKADDR, *mut i32) {
    let map = registry().lock();
    let block = map.get(&key).expect("staged block must exist");
    match &block.op {
        Op::ReadFrom { addr, .. } | Op::ReadFromInto { addr, .. } => {
            (addr.sa.get().cast::<ws::SOCKADDR>(), addr.len.get())
        }
        _ => (std::ptr::null_mut(), std::ptr::null_mut()),
    }
}

fn ov_write_file(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = self_arg(args)?;
    let handle = uintptr_arg(args.get(1), "handle")?;
    let data = bytes_like(args.get(2), "WriteFile")?;
    let len = data.len() as u32;
    let (key, ov_ptr) = stage_op(&inst, handle, Op::Write { _pinned: data })?;
    let (ptr, _) = staged_buf(key);
    let mut written = 0u32;
    let ret = crate::gil::allow_threads_then(|| unsafe {
        WriteFile(handle as HANDLE, ptr, len, &raw mut written, ov_ptr)
    });
    let err = if ret != 0 {
        ERROR_SUCCESS
    } else {
        unsafe { GetLastError() }
    };
    finish_start(key, err, false, Object::None)
}

fn ov_wsa_send(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = self_arg(args)?;
    let handle = uintptr_arg(args.get(1), "handle")?;
    let data = bytes_like(args.get(2), "WSASend")?;
    let flags = dword_arg(args.get(3), "flags")?;
    let len = data.len() as u32;
    let (key, ov_ptr) = stage_op(&inst, handle, Op::Write { _pinned: data })?;
    let (ptr, _) = staged_buf(key);
    let wsabuf = ws::WSABUF { len, buf: ptr };
    let mut written = 0u32;
    let ret = crate::gil::allow_threads_then(|| unsafe {
        ws::WSASend(
            handle,
            &raw const wsabuf,
            1,
            &raw mut written,
            flags,
            ov_ptr,
            None,
        )
    });
    let err = wsa_start_err(ret);
    finish_start(key, err, false, Object::None)
}

fn ov_wsa_send_to(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = self_arg(args)?;
    let handle = uintptr_arg(args.get(1), "handle")?;
    let data = bytes_like(args.get(2), "WSASendTo")?;
    let flags = dword_arg(args.get(3), "flags")?;
    let addr = parse_address(args.get(4))?;
    let len = data.len() as u32;
    let (key, ov_ptr) = stage_op(&inst, handle, Op::Write { _pinned: data })?;
    let (ptr, _) = staged_buf(key);
    let wsabuf = ws::WSABUF { len, buf: ptr };
    let mut written = 0u32;
    let ret = crate::gil::allow_threads_then(|| unsafe {
        ws::WSASendTo(
            handle,
            &raw const wsabuf,
            1,
            &raw mut written,
            flags,
            addr.as_ptr(),
            addr.len(),
            ov_ptr,
            None,
        )
    });
    let err = wsa_start_err(ret);
    finish_start(key, err, false, Object::None)
}

fn ov_accept_ex(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = self_arg(args)?;
    let listen = uintptr_arg(args.get(1), "listen_handle")?;
    let accept = uintptr_arg(args.get(2), "accept_handle")?;
    let fns = ext_fns()?;
    let accept_ex = fns
        .accept_ex
        .ok_or_else(|| crate::error::os_error("AcceptEx extension function unavailable"))?;
    // Address buffer per AcceptEx contract: (local + remote) each
    // `sizeof(sockaddr) + 16`. windows_events fixes the accept socket
    // up itself via SO_UPDATE_ACCEPT_CONTEXT, so the buffer is never
    // parsed — it just has to exist and outlive the operation.
    let single = std::mem::size_of::<ws::SOCKADDR_IN6>() as u32 + 16;
    let buf = vec![0u8; (single as usize) * 2];
    let (key, ov_ptr) = stage_op(&inst, listen, Op::Accept { _pinned: buf })?;
    let (ptr, _) = staged_buf(key);
    let mut received = 0u32;
    let ret = crate::gil::allow_threads_then(|| unsafe {
        accept_ex(
            listen,
            accept,
            ptr.cast::<c_void>(),
            0,
            single,
            single,
            &raw mut received,
            ov_ptr,
        )
    });
    let err = wsa_bool_err(ret);
    finish_start(key, err, false, Object::None)
}

fn ov_connect_ex(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = self_arg(args)?;
    let sock = uintptr_arg(args.get(1), "client_handle")?;
    let addr = parse_address(args.get(2))?;
    let fns = ext_fns()?;
    let connect_ex = fns
        .connect_ex
        .ok_or_else(|| crate::error::os_error("ConnectEx extension function unavailable"))?;
    let (key, ov_ptr) = stage_op(&inst, sock, Op::Connect)?;
    let ret = crate::gil::allow_threads_then(|| unsafe {
        connect_ex(
            sock,
            addr.as_ptr(),
            addr.len(),
            std::ptr::null(),
            0,
            std::ptr::null_mut(),
            ov_ptr,
        )
    });
    let err = wsa_bool_err(ret);
    finish_start(key, err, false, Object::None)
}

fn ov_disconnect_ex(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = self_arg(args)?;
    let sock = uintptr_arg(args.get(1), "handle")?;
    let flags = dword_arg(args.get(2), "flags")?;
    let fns = ext_fns()?;
    let disconnect_ex = fns
        .disconnect_ex
        .ok_or_else(|| crate::error::os_error("DisconnectEx extension function unavailable"))?;
    let (key, ov_ptr) = stage_op(&inst, sock, Op::Disconnect)?;
    let ret = crate::gil::allow_threads_then(|| unsafe { disconnect_ex(sock, ov_ptr, flags, 0) });
    let err = wsa_bool_err(ret);
    finish_start(key, err, false, Object::None)
}

fn ov_transmit_file(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = self_arg(args)?;
    let sock = uintptr_arg(args.get(1), "socket")?;
    let file = uintptr_arg(args.get(2), "file")?;
    let offset = dword_arg(args.get(3), "offset")?;
    let offset_high = dword_arg(args.get(4), "offset_high")?;
    let count_to_write = dword_arg(args.get(5), "count_to_write")?;
    let count_per_send = dword_arg(args.get(6), "count_per_send")?;
    let flags = dword_arg(args.get(7), "flags")?;
    let fns = ext_fns()?;
    let transmit_file = fns
        .transmit_file
        .ok_or_else(|| crate::error::os_error("TransmitFile extension function unavailable"))?;
    let (key, ov_ptr) = stage_op(&inst, sock, Op::TransmitFile)?;
    // The file position rides in the OVERLAPPED itself.
    unsafe {
        (*ov_ptr).Anonymous.Anonymous.Offset = offset;
        (*ov_ptr).Anonymous.Anonymous.OffsetHigh = offset_high;
    }
    let ret = crate::gil::allow_threads_then(|| unsafe {
        transmit_file(
            sock,
            file as HANDLE,
            count_to_write,
            count_per_send,
            ov_ptr,
            std::ptr::null(),
            flags,
        )
    });
    let err = wsa_bool_err(ret);
    finish_start(key, err, false, Object::None)
}

fn ov_connect_named_pipe(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = self_arg(args)?;
    let pipe = uintptr_arg(args.get(1), "handle")?;
    let (key, ov_ptr) = stage_op(&inst, pipe, Op::ConnectNamedPipe)?;
    let ret = crate::gil::allow_threads_then(|| unsafe {
        windows_sys::Win32::System::Pipes::ConnectNamedPipe(pipe as HANDLE, ov_ptr)
    });
    let err = if ret != 0 {
        ERROR_SUCCESS
    } else {
        unsafe { GetLastError() }
    };
    // ERROR_PIPE_CONNECTED = a client raced us and is already attached:
    // report True (no completion will be posted — mark done), matching
    // IocpProactor.accept_pipe's `if connected:` short-circuit.
    if err == ERROR_PIPE_CONNECTED {
        let mut map = registry().lock();
        let block = map.get_mut(&key).expect("staged block must exist");
        block.error = err;
        block.mark_as_completed();
        return Ok(Object::Bool(true));
    }
    finish_start(key, err, false, Object::Bool(false))
}

// ---------------------------------------------------------------------------
// Module-level functions.
// ---------------------------------------------------------------------------

fn mod_create_io_completion_port(args: &[Object]) -> Result<Object, RuntimeError> {
    let handle = uintptr_arg(args.first(), "handle")?;
    let port = uintptr_arg(args.get(1), "port")?;
    let key = uintptr_arg(args.get(2), "key")?;
    let concurrency = dword_arg(args.get(3), "concurrency")?;
    let ret = crate::gil::allow_threads_then(|| unsafe {
        CreateIoCompletionPort(handle as HANDLE, port as HANDLE, key, concurrency)
    });
    if ret.is_null() {
        return Err(last_win32_error_to_py(None));
    }
    Ok(uint_obj(ret as usize))
}

fn mod_get_queued_completion_status(args: &[Object]) -> Result<Object, RuntimeError> {
    let port = uintptr_arg(args.first(), "port")?;
    let ms = dword_arg(args.get(1), "msecs")?;
    let mut bytes = 0u32;
    let mut key = 0usize;
    let mut ov: *mut OVERLAPPED = std::ptr::null_mut();
    // The proactor blocks here (up to `ms`, possibly INFINITE) — the
    // GIL must be released or every other Python thread stalls.
    let ret = crate::gil::allow_threads_then(|| unsafe {
        GetQueuedCompletionStatus(
            port as HANDLE,
            &raw mut bytes,
            &raw mut key,
            &raw mut ov,
            ms,
        )
    });
    let err = if ret != 0 {
        ERROR_SUCCESS
    } else {
        unsafe { GetLastError() }
    };
    if ov.is_null() {
        // No packet: timeout is None, anything else is a real failure.
        if err == WAIT_TIMEOUT {
            return Ok(Object::None);
        }
        return Err(win32_error_to_py(err as i32, None));
    }
    Ok(Object::new_tuple(vec![
        Object::Int(i64::from(err)),
        Object::Int(i64::from(bytes)),
        uint_obj(key),
        uint_obj(ov as usize),
    ]))
}

fn mod_post_queued_completion_status(args: &[Object]) -> Result<Object, RuntimeError> {
    let port = uintptr_arg(args.first(), "port")?;
    let bytes = dword_arg(args.get(1), "bytes")?;
    let key = uintptr_arg(args.get(2), "key")?;
    let address = uintptr_arg(args.get(3), "address")?;
    let ret = crate::gil::allow_threads_then(|| unsafe {
        PostQueuedCompletionStatus(port as HANDLE, bytes, key, address as *const OVERLAPPED)
    });
    if ret == 0 {
        return Err(last_win32_error_to_py(None));
    }
    Ok(Object::None)
}

fn mod_format_message(args: &[Object]) -> Result<Object, RuntimeError> {
    let code = dword_arg(args.first(), "error_code")?;
    Ok(Object::from_str(crate::stdlib::nt_support::format_message(
        code as i32,
    )))
}

/// Context handed to the OS wait callback. Heap-allocated per
/// registration and freed *by the callback* (CPython PyMem_RawMalloc /
/// PostToQueueCallback PyMem_RawFree). If the wait is unregistered
/// before it ever fires, the allocation leaks — exactly as in CPython,
/// where UnregisterWait(Ex) has no way to reclaim it either.
struct PostCallbackData {
    port: usize,
    overlapped: usize,
}

/// CPython's `PostToQueueCallback`: runs on an OS thread-pool thread
/// with no GIL, so it must not touch Python state — it only forwards
/// the wait outcome into the completion port (`bytes` = whether the
/// wait timed out, key = 0, and the caller's OVERLAPPED address).
unsafe extern "system" fn post_to_queue_callback(param: *mut c_void, timer_or_wait_fired: bool) {
    // SAFETY: `param` is the Box leaked by RegisterWaitWithQueue;
    // WT_EXECUTEONLYONCE guarantees a single invocation.
    let data = unsafe { Box::from_raw(param.cast::<PostCallbackData>()) };
    unsafe {
        // Errors deliberately ignored, like CPython's comment says.
        PostQueuedCompletionStatus(
            data.port as HANDLE,
            u32::from(timer_or_wait_fired),
            0,
            data.overlapped as *const OVERLAPPED,
        );
    }
}

fn mod_register_wait_with_queue(args: &[Object]) -> Result<Object, RuntimeError> {
    let object = uintptr_arg(args.first(), "Object")?;
    let port = uintptr_arg(args.get(1), "CompletionPort")?;
    let overlapped = uintptr_arg(args.get(2), "Overlapped")?;
    let ms = dword_arg(args.get(3), "Timeout")?;
    let pdata = Box::into_raw(Box::new(PostCallbackData { port, overlapped }));
    let mut wait_handle: HANDLE = std::ptr::null_mut();
    let ret = unsafe {
        RegisterWaitForSingleObject(
            &raw mut wait_handle,
            object as HANDLE,
            Some(post_to_queue_callback),
            pdata.cast::<c_void>(),
            ms,
            WT_EXECUTEINWAITTHREAD | WT_EXECUTEONLYONCE,
        )
    };
    if ret == 0 {
        let err = last_win32_error_to_py(None);
        // The callback will never run; reclaim its context.
        drop(unsafe { Box::from_raw(pdata) });
        return Err(err);
    }
    Ok(uint_obj(wait_handle as usize))
}

fn mod_unregister_wait(args: &[Object]) -> Result<Object, RuntimeError> {
    let wait = uintptr_arg(args.first(), "WaitHandle")?;
    let ret = crate::gil::allow_threads_then(|| unsafe { UnregisterWait(wait as HANDLE) });
    if ret == 0 {
        return Err(last_win32_error_to_py(None));
    }
    Ok(Object::None)
}

fn mod_unregister_wait_ex(args: &[Object]) -> Result<Object, RuntimeError> {
    let wait = uintptr_arg(args.first(), "WaitHandle")?;
    let event = uintptr_arg(args.get(1), "Event")?;
    let ret = crate::gil::allow_threads_then(|| unsafe {
        UnregisterWaitEx(wait as HANDLE, event as HANDLE)
    });
    if ret == 0 {
        return Err(last_win32_error_to_py(None));
    }
    Ok(Object::None)
}

fn mod_create_event(args: &[Object]) -> Result<Object, RuntimeError> {
    if !matches!(args.first(), Some(Object::None)) {
        return Err(value_error("EventAttributes must be None"));
    }
    let manual_reset = bool_arg(args.get(1), false);
    let initial_state = bool_arg(args.get(2), false);
    let name_wide = match args.get(3) {
        None | Some(Object::None) => None,
        Some(Object::Str(s)) => Some(wide(s)),
        Some(other) => {
            return Err(type_error(format!(
                "CreateEvent() argument 4 must be str or None, not {}",
                other.type_name_owned()
            )))
        }
    };
    let name_ptr = name_wide.as_ref().map_or(std::ptr::null(), |w| w.as_ptr());
    let event = crate::gil::allow_threads_then(|| unsafe {
        CreateEventW(
            std::ptr::null(),
            i32::from(manual_reset),
            i32::from(initial_state),
            name_ptr,
        )
    });
    if event.is_null() {
        return Err(last_win32_error_to_py(None));
    }
    Ok(uint_obj(event as usize))
}

fn mod_set_event(args: &[Object]) -> Result<Object, RuntimeError> {
    let handle = uintptr_arg(args.first(), "Handle")?;
    let ret = crate::gil::allow_threads_then(|| unsafe { SetEvent(handle as HANDLE) });
    if ret == 0 {
        return Err(last_win32_error_to_py(None));
    }
    Ok(Object::None)
}

fn mod_reset_event(args: &[Object]) -> Result<Object, RuntimeError> {
    let handle = uintptr_arg(args.first(), "Handle")?;
    let ret = crate::gil::allow_threads_then(|| unsafe { ResetEvent(handle as HANDLE) });
    if ret == 0 {
        return Err(last_win32_error_to_py(None));
    }
    Ok(Object::None)
}

/// Bind to an arbitrary local port without a getaddrinfo round-trip —
/// ConnectEx requires a bound socket. CPython binds the wildcard
/// address (INADDR_ANY / in6addr_any), port 0.
fn mod_bind_local(args: &[Object]) -> Result<Object, RuntimeError> {
    let sock = uintptr_arg(args.first(), "handle")?;
    let family = match args.get(1) {
        Some(Object::Int(n)) => *n,
        Some(Object::Bool(v)) => i64::from(*v),
        _ => return Err(type_error("family must be an int")),
    };
    let ret = if family == i64::from(ws::AF_INET) {
        let mut sa: ws::SOCKADDR_IN = unsafe { std::mem::zeroed() };
        sa.sin_family = ws::AF_INET;
        unsafe {
            ws::bind(
                sock,
                std::ptr::from_ref(&sa).cast(),
                std::mem::size_of::<ws::SOCKADDR_IN>() as i32,
            )
        }
    } else if family == i64::from(ws::AF_INET6) {
        let mut sa: ws::SOCKADDR_IN6 = unsafe { std::mem::zeroed() };
        sa.sin6_family = ws::AF_INET6;
        unsafe {
            ws::bind(
                sock,
                std::ptr::from_ref(&sa).cast(),
                std::mem::size_of::<ws::SOCKADDR_IN6>() as i32,
            )
        }
    } else {
        // CPython reuses parse_address's message here, oddly; mirrored.
        return Err(value_error("expected tuple of length 2 or 4"));
    };
    if ret == ws::SOCKET_ERROR {
        return Err(win32_error_to_py(unsafe { ws::WSAGetLastError() }, None));
    }
    Ok(Object::None)
}

/// Blocking connect for connectionless (UDP) sockets — WSAConnect on a
/// datagram socket completes immediately, which is why the proactor
/// skips IOCP registration for it.
fn mod_wsa_connect(args: &[Object]) -> Result<Object, RuntimeError> {
    let sock = uintptr_arg(args.first(), "client_handle")?;
    let addr = parse_address(args.get(1))?;
    let ret = crate::gil::allow_threads_then(|| unsafe {
        ws::WSAConnect(
            sock,
            addr.as_ptr(),
            addr.len(),
            std::ptr::null(),
            std::ptr::null_mut(),
            std::ptr::null(),
            std::ptr::null(),
        )
    });
    if ret == ws::SOCKET_ERROR {
        return Err(win32_error_to_py(unsafe { ws::WSAGetLastError() }, None));
    }
    Ok(Object::None)
}

/// Open the client end of a named pipe for overlapped I/O. There is no
/// overlapped connect for pipe clients, so `IocpProactor.connect_pipe`
/// retries this in a delay loop while it fails with ERROR_PIPE_BUSY.
fn mod_connect_pipe(args: &[Object]) -> Result<Object, RuntimeError> {
    let address = match args.first() {
        Some(Object::Str(s)) => wide(s),
        _ => return Err(type_error("ConnectPipe() argument must be str")),
    };
    let handle = crate::gil::allow_threads_then(|| unsafe {
        CreateFileW(
            address.as_ptr(),
            GENERIC_READ | GENERIC_WRITE,
            0,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_OVERLAPPED,
            std::ptr::null_mut(),
        )
    });
    if handle == windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE {
        return Err(last_win32_error_to_py(None));
    }
    Ok(uint_obj(handle as usize))
}

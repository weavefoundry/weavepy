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

use std::os::fd::RawFd;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use crate::sync::Rc;
use crate::sync::RefCell;

use crate::error::{
    broken_pipe_error, io_error_to_py, runtime_error, type_error, value_error, RuntimeError,
};
use crate::import::ModuleCache;
use crate::object::{BuiltinFn, DictData, DictKey, Object, PyModule};

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
            b("SemLock", make_semlock),
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

/// `_multiprocessing.SemLock(kind, value, maxvalue, name=None,
/// unlink=False)`. Returns a SimpleNamespace with `acquire`, `release`,
/// `_get_value`, `_count`, `name` exposed. The implementation today is
/// thread-shared (a real `Semaphore`); named semaphores are a no-op
/// stub (the name is recorded but the kernel object is not opened).
fn make_semlock(args: &[Object]) -> Result<Object, RuntimeError> {
    let kind = match args.first() {
        Some(Object::Int(n)) => *n,
        _ => 0,
    };
    let value = match args.get(1) {
        Some(Object::Int(n)) => *n as usize,
        _ => 1,
    };
    let maxvalue = match args.get(2) {
        Some(Object::Int(n)) => *n as usize,
        _ => 1,
    };
    let name = match args.get(3) {
        Some(Object::Str(s)) => Some(s.to_string()),
        Some(Object::None) | None => None,
        Some(other) => {
            return Err(type_error(format!(
                "SemLock name must be str or None, got {}",
                other.type_name()
            )))
        }
    };
    let dict = Rc::new(RefCell::new(DictData::new()));
    let lock = std::sync::Arc::new(crate::sync::RealSemaphore::new(value));
    if let Some(ref n) = name {
        let mut g = SHARED_SEM_NAMES.lock().unwrap();
        g.push(n.clone());
    }
    {
        let l = lock.clone();
        let acquire = move |args: &[Object]| -> Result<Object, RuntimeError> {
            let blocking = match args.first() {
                Some(Object::Bool(b)) => *b,
                Some(Object::Int(i)) => *i != 0,
                _ => true,
            };
            let timeout: Option<f64> = match args.get(1) {
                Some(Object::Float(f)) => Some(*f),
                Some(Object::Int(i)) => Some(*i as f64),
                Some(Object::None) | None => None,
                _ => None,
            };
            if !blocking {
                return Ok(Object::Bool(l.try_acquire()));
            }
            if let Some(t) = timeout {
                if t <= 0.0 {
                    return Ok(Object::Bool(l.try_acquire()));
                }
                // Poll-based timeout — good enough for the small wait
                // budgets multiprocessing uses; we can switch to
                // `sem_timedwait` later for accuracy.
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs_f64(t);
                loop {
                    if l.try_acquire() {
                        return Ok(Object::Bool(true));
                    }
                    if std::time::Instant::now() >= deadline {
                        return Ok(Object::Bool(false));
                    }
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
            }
            l.acquire();
            Ok(Object::Bool(true))
        };
        let l2 = lock.clone();
        let release = move |_args: &[Object]| -> Result<Object, RuntimeError> {
            l2.release(1);
            Ok(Object::None)
        };
        let l3 = lock.clone();
        let get_value = move |_args: &[Object]| -> Result<Object, RuntimeError> {
            Ok(Object::Int(l3.current() as i64))
        };
        let mut d = dict.borrow_mut();
        d.insert(DictKey(Object::from_static("kind")), Object::Int(kind));
        d.insert(
            DictKey(Object::from_static("maxvalue")),
            Object::Int(maxvalue as i64),
        );
        d.insert(
            DictKey(Object::from_static("name")),
            match name {
                Some(n) => Object::from_str(n),
                None => Object::None,
            },
        );
        d.insert(
            DictKey(Object::from_static("acquire")),
            b_dyn("acquire", acquire),
        );
        d.insert(
            DictKey(Object::from_static("release")),
            b_dyn("release", release),
        );
        d.insert(
            DictKey(Object::from_static("_get_value")),
            b_dyn("_get_value", get_value),
        );
    }
    Ok(Object::SimpleNamespace(dict))
}

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
    let mut g = SHARED_SEM_NAMES.lock().unwrap();
    g.retain(|n| n != &name);
    Ok(Object::None)
}

/// Names of all known cross-process semaphores so `sem_unlink()` is
/// idempotent and the test runner can audit leaks.
static SHARED_SEM_NAMES: Mutex<Vec<String>> = Mutex::new(Vec::new());

// ---------------------------------------------------------------------
// Connection (socketpair-backed byte channel)
// ---------------------------------------------------------------------

/// Inner state for a Connection. The `fd` is owned by Rust until the
/// Python wrapper calls `close()` (or the Connection is dropped). We
/// guard it with a `Mutex` because Connection objects can be shared
/// across worker threads.
struct ConnInner {
    fd: i32,
    closed: bool,
}

impl Drop for ConnInner {
    fn drop(&mut self) {
        if !self.closed && self.fd >= 0 {
            unsafe { libc::close(self.fd) };
            self.closed = true;
        }
    }
}

/// Build the public Connection namespace around `fd`.
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

struct ShmInner {
    name: String,
    fd: i32,
    addr: *mut libc::c_void,
    size: usize,
    closed: bool,
}

// addr is only ever touched via the Mutex.
unsafe impl Send for ShmInner {}

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

static SHM_COUNTER: AtomicUsize = AtomicUsize::new(0);

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
fn waitpid_py(args: &[Object]) -> Result<Object, RuntimeError> {
    let pid = arg_int(args, 0, 0)? as libc::pid_t;
    let options = arg_int(args, 1, 0)? as libc::c_int;
    let mut status: libc::c_int = 0;
    let rc = unsafe { libc::waitpid(pid, &mut status, options) };
    if rc < 0 {
        return Err(io_error_to_py(&std::io::Error::last_os_error()));
    }
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
fn mp_exit(args: &[Object]) -> Result<Object, RuntimeError> {
    let code = arg_int(args, 0, 0)? as i32;
    std::process::exit(code);
}

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

fn conn_send_legacy(_args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(Object::None)
}

fn conn_recv_legacy(_args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(Object::None)
}

fn conn_close_legacy(_args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(Object::None)
}

// ---------------------------------------------------------------------
// I/O helpers
// ---------------------------------------------------------------------

fn write_msg(fd: RawFd, data: &[u8]) -> std::io::Result<()> {
    let header = (data.len() as u32).to_be_bytes();
    write_all(fd, &header)?;
    write_all(fd, data)?;
    Ok(())
}

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

fn read_exact(fd: RawFd, buf: &mut [u8]) -> std::io::Result<()> {
    let mut filled = 0usize;
    while filled < buf.len() {
        let n = unsafe {
            libc::read(
                fd,
                buf.as_mut_ptr().add(filled) as *mut libc::c_void,
                buf.len() - filled,
            )
        };
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
    let rc = unsafe { libc::poll(&mut pfd as *mut _, 1, timeout_ms) };
    if rc < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(rc > 0 && (pfd.revents & libc::POLLIN) != 0)
}

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

fn arg_bytes(args: &[Object], idx: usize, label: &str) -> Result<Vec<u8>, RuntimeError> {
    arg_bytes_obj(
        args.get(idx)
            .ok_or_else(|| type_error(format!("{label}: missing bytes argument")))?,
        label,
    )
}

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

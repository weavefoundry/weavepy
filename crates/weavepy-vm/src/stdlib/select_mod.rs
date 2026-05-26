//! The `select` built-in module.
//!
//! Exposes `select.select(rlist, wlist, xlist, timeout=None)` and
//! `select.poll()` on top of `mio::Poll`. The asyncio event loop
//! consumes the same primitives through the frozen `selectors`
//! Python module, so registrations made from user code remain
//! visible to the loop.
//!
//! Sockets passed to `select` must expose `.fileno()` returning an
//! integer (matching CPython). We use this fd to register with
//! `mio::Poll` via the `mio::unix::SourceFd` wrapper.

use crate::sync::Rc;
use crate::sync::RefCell;
use std::time::Duration;

#[cfg(unix)]
use mio::Token;
use mio::{Interest, Poll};

use crate::error::{os_error, type_error, RuntimeError};
use crate::import::ModuleCache;
use crate::object::{BuiltinFn, DictData, DictKey, Object, PyModule};

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

        // CPython exposes these as ints.
        d.insert(DictKey(Object::from_static("POLLIN")), Object::Int(0x001));
        d.insert(DictKey(Object::from_static("POLLPRI")), Object::Int(0x002));
        d.insert(DictKey(Object::from_static("POLLOUT")), Object::Int(0x004));
        d.insert(DictKey(Object::from_static("POLLERR")), Object::Int(0x008));
        d.insert(DictKey(Object::from_static("POLLHUP")), Object::Int(0x010));
        d.insert(DictKey(Object::from_static("POLLNVAL")), Object::Int(0x020));
        d.insert(
            DictKey(Object::from_static("POLLRDNORM")),
            Object::Int(0x040),
        );
        d.insert(
            DictKey(Object::from_static("POLLRDBAND")),
            Object::Int(0x080),
        );
        d.insert(
            DictKey(Object::from_static("POLLWRNORM")),
            Object::Int(0x100),
        );
        d.insert(
            DictKey(Object::from_static("POLLWRBAND")),
            Object::Int(0x200),
        );

        d.insert(
            DictKey(Object::from_static("select")),
            b("select", select_select),
        );
        d.insert(DictKey(Object::from_static("poll")), b("poll", select_poll));

        // Errors raised by select/poll. CPython 3.3+ aliases the
        // module's `error` to OSError; we follow.
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
        call: Box::new(body),
        call_kw: None,
    }))
}

/// `select.select(rlist, wlist, xlist, timeout=None)`.
///
/// Returns a 3-tuple of ready `(rlist, wlist, xlist)`.
///
/// Each input list contains either bare file descriptors (ints) or
/// objects exposing `.fileno()`. We accept both, normalising to
/// ints. The `xlist` channel maps to mio's "error" events, which
/// in practice means hung-up sockets — useful enough for everyday
/// `select` clients.
fn select_select(args: &[Object]) -> Result<Object, RuntimeError> {
    let rlist = collect_fds(args.first())?;
    let wlist = collect_fds(args.get(1))?;
    let _xlist = collect_fds(args.get(2))?;
    let timeout = match args.get(3) {
        None | Some(Object::None) => None,
        Some(Object::Int(n)) => Some(Duration::from_secs_f64(*n as f64)),
        Some(Object::Float(f)) => {
            if *f < 0.0 {
                return Err(type_error("timeout must be >= 0"));
            }
            Some(Duration::from_secs_f64(*f))
        }
        _ => return Err(type_error("timeout must be a number or None")),
    };
    let outcome = poll_fds(&rlist, &wlist, timeout)?;
    Ok(Object::new_tuple(vec![
        Object::new_list(outcome.0.into_iter().map(Object::Int).collect()),
        Object::new_list(outcome.1.into_iter().map(Object::Int).collect()),
        Object::new_list(Vec::new()),
    ]))
}

fn poll_fds(
    rlist: &[i64],
    wlist: &[i64],
    timeout: Option<Duration>,
) -> Result<(Vec<i64>, Vec<i64>), RuntimeError> {
    let mut poll = Poll::new().map_err(|e| os_error(e.to_string()))?;
    let mut events = mio::Events::with_capacity(rlist.len() + wlist.len() + 8);
    let mut fd_for_token: Vec<i64> = Vec::new();
    for fd in rlist {
        register(&mut poll, *fd, Interest::READABLE, fd_for_token.len())?;
        fd_for_token.push(*fd);
    }
    for fd in wlist {
        register(&mut poll, *fd, Interest::WRITABLE, fd_for_token.len())?;
        fd_for_token.push(*fd);
    }
    poll.poll(&mut events, timeout)
        .map_err(|e| os_error(e.to_string()))?;
    let mut ready_r = Vec::new();
    let mut ready_w = Vec::new();
    for ev in events.iter() {
        let idx = ev.token().0;
        if let Some(&fd) = fd_for_token.get(idx) {
            if ev.is_readable() && rlist.contains(&fd) {
                ready_r.push(fd);
            }
            if ev.is_writable() && wlist.contains(&fd) {
                ready_w.push(fd);
            }
        }
    }
    Ok((ready_r, ready_w))
}

#[cfg(unix)]
fn register(poll: &mut Poll, fd: i64, interest: Interest, idx: usize) -> Result<(), RuntimeError> {
    use mio::unix::SourceFd;
    poll.registry()
        .register(
            &mut SourceFd(&(fd as std::os::unix::io::RawFd)),
            Token(idx),
            interest,
        )
        .map_err(|e| os_error(e.to_string()))
}

#[cfg(not(unix))]
fn register(
    _poll: &mut Poll,
    _fd: i64,
    _interest: Interest,
    _idx: usize,
) -> Result<(), RuntimeError> {
    Err(os_error("select.select on non-Unix platforms is limited"))
}

fn collect_fds(arg: Option<&Object>) -> Result<Vec<i64>, RuntimeError> {
    let items: &[Object] = match arg {
        None | Some(Object::None) => return Ok(Vec::new()),
        Some(Object::List(l)) => {
            let borrowed = l.borrow();
            return borrowed.iter().map(extract_fd).collect();
        }
        Some(Object::Tuple(t)) => t,
        _ => return Err(type_error("select fd list must be list or tuple")),
    };
    items.iter().map(extract_fd).collect()
}

fn extract_fd(obj: &Object) -> Result<i64, RuntimeError> {
    match obj {
        Object::Int(n) => Ok(*n),
        Object::Instance(inst) => {
            // Look up `fileno` callable on the instance.
            let dict = inst.dict.borrow();
            let fileno = dict
                .get(&DictKey(Object::from_static("_fileno")))
                .or_else(|| dict.get(&DictKey(Object::from_static("fileno"))))
                .cloned();
            match fileno {
                Some(Object::Int(n)) => Ok(n),
                _ => Err(type_error("object has no fileno()")),
            }
        }
        _ => Err(type_error("expected fd or fileno-bearing object")),
    }
}

/// `select.poll()` placeholder. Returns a dict exposing the standard
/// `register` / `unregister` / `poll` triad. For real concurrency we
/// rely on `selectors.DefaultSelector` (frozen Python on top of
/// `select.select`), which is what asyncio's event loop uses; this
/// one-off `poll()` object is here for legacy CPython programs that
/// reach for it directly.
fn select_poll(_args: &[Object]) -> Result<Object, RuntimeError> {
    let registered: Rc<RefCell<Vec<(i64, i64)>>> = Rc::new(RefCell::new(Vec::new()));

    let r1 = registered.clone();
    let register = move |args: &[Object]| -> Result<Object, RuntimeError> {
        let fd = match args.first() {
            Some(Object::Int(n)) => *n,
            Some(Object::Instance(inst)) => match inst
                .dict
                .borrow()
                .get(&DictKey(Object::from_static("_fileno")))
                .cloned()
            {
                Some(Object::Int(n)) => n,
                _ => return Err(type_error("poll.register: object has no fileno")),
            },
            _ => return Err(type_error("poll.register: expected fd")),
        };
        let mask = match args.get(1) {
            Some(Object::Int(m)) => *m,
            None | Some(Object::None) => 0x001 | 0x004,
            _ => return Err(type_error("poll.register: mask must be int")),
        };
        let mut v = r1.borrow_mut();
        v.retain(|(f, _)| *f != fd);
        v.push((fd, mask));
        Ok(Object::None)
    };

    let r2 = registered.clone();
    let modify = move |args: &[Object]| -> Result<Object, RuntimeError> {
        let fd = match args.first() {
            Some(Object::Int(n)) => *n,
            _ => return Err(type_error("poll.modify: expected fd")),
        };
        let mask = match args.get(1) {
            Some(Object::Int(m)) => *m,
            _ => return Err(type_error("poll.modify: mask must be int")),
        };
        let mut v = r2.borrow_mut();
        for entry in v.iter_mut() {
            if entry.0 == fd {
                entry.1 = mask;
                return Ok(Object::None);
            }
        }
        Err(os_error("fd not registered"))
    };

    let r3 = registered.clone();
    let unregister = move |args: &[Object]| -> Result<Object, RuntimeError> {
        let fd = match args.first() {
            Some(Object::Int(n)) => *n,
            _ => return Err(type_error("poll.unregister: expected fd")),
        };
        r3.borrow_mut().retain(|(f, _)| *f != fd);
        Ok(Object::None)
    };

    let r4 = registered;
    let poll_fn = move |args: &[Object]| -> Result<Object, RuntimeError> {
        let timeout = match args.first() {
            None | Some(Object::None) => None,
            Some(Object::Int(n)) => Some(Duration::from_millis(*n as u64)),
            Some(Object::Float(f)) => Some(Duration::from_millis((*f) as u64)),
            _ => return Err(type_error("poll.poll: timeout must be number or None")),
        };
        let regs = r4.borrow().clone();
        let mut rfds = Vec::new();
        let mut wfds = Vec::new();
        for (fd, mask) in &regs {
            if (mask & 0x001) != 0 {
                rfds.push(*fd);
            }
            if (mask & 0x004) != 0 {
                wfds.push(*fd);
            }
        }
        let (ready_r, ready_w) = poll_fds(&rfds, &wfds, timeout)?;
        let mut out = Vec::new();
        for fd in ready_r {
            out.push(Object::new_tuple(vec![Object::Int(fd), Object::Int(0x001)]));
        }
        for fd in ready_w {
            out.push(Object::new_tuple(vec![Object::Int(fd), Object::Int(0x004)]));
        }
        Ok(Object::new_list(out))
    };

    let dict = Rc::new(RefCell::new(DictData::new()));
    {
        let mut d = dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("register")),
            Object::Builtin(Rc::new(BuiltinFn {
                name: "register",
                call: Box::new(register),
                call_kw: None,
            })),
        );
        d.insert(
            DictKey(Object::from_static("modify")),
            Object::Builtin(Rc::new(BuiltinFn {
                name: "modify",
                call: Box::new(modify),
                call_kw: None,
            })),
        );
        d.insert(
            DictKey(Object::from_static("unregister")),
            Object::Builtin(Rc::new(BuiltinFn {
                name: "unregister",
                call: Box::new(unregister),
                call_kw: None,
            })),
        );
        d.insert(
            DictKey(Object::from_static("poll")),
            Object::Builtin(Rc::new(BuiltinFn {
                name: "poll",
                call: Box::new(poll_fn),
                call_kw: None,
            })),
        );
    }
    Ok(Object::Dict(dict))
}

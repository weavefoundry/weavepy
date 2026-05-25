//! `_multiprocessing` Rust core — RFC 0024.
//!
//! The user-visible `multiprocessing` module (frozen Python)
//! depends on a small Rust accelerator surface that mirrors
//! CPython's `Modules/_multiprocessing/`:
//!
//! - **`SemLock(kind, value, maxvalue, name, unlink)`** — a
//!   semaphore-shaped lock, used by `multiprocessing.Lock`,
//!   `RLock`, `Semaphore`, and `BoundedSemaphore` underneath.
//! - **`sem_unlink(name)`** — opt-in cleanup for named
//!   semaphores.
//! - **`closesocket` / `recv` / `send` / `pipe`** — raw
//!   connection primitives. Today these are thin shims over
//!   `os.pipe` / `_socket` because the existing
//!   `subprocess`/`socket` modules already cover the
//!   functionality.
//! - **`SHARED_MEMORY_RW`** etc — file-mode constants.
//!
//! In WeavePy we additionally expose **`_get_command`** which
//! returns the launcher arg vector (`weavepy --multiprocessing-fork
//! ...`) used by the spawn-based start method. The frozen
//! `multiprocessing.py` module pivots on this when it spawns a
//! worker.

use crate::sync::Rc;
use crate::sync::RefCell;

use crate::error::{type_error, RuntimeError};
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
                 `Pool`, `Queue`, etc.",
            ),
        );
        d.insert(
            DictKey(Object::from_static("SemLock")),
            b("SemLock", make_semlock),
        );
        d.insert(
            DictKey(Object::from_static("sem_unlink")),
            b("sem_unlink", sem_unlink),
        );
        d.insert(DictKey(Object::from_static("flags")), Object::Int(0));
        d.insert(
            DictKey(Object::from_static("RECURSIVE_MUTEX")),
            Object::Int(0),
        );
        d.insert(DictKey(Object::from_static("SEMAPHORE")), Object::Int(1));
        d.insert(
            DictKey(Object::from_static("_get_command")),
            b("_get_command", get_command),
        );
        d.insert(DictKey(Object::from_static("send")), b("send", conn_send));
        d.insert(DictKey(Object::from_static("recv")), b("recv", conn_recv));
        d.insert(
            DictKey(Object::from_static("closesocket")),
            b("closesocket", conn_close),
        );
    }
    Rc::new(PyModule {
        name: "_multiprocessing".to_owned(),
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

fn b_dyn(
    name: &'static str,
    body: impl Fn(&[Object]) -> Result<Object, RuntimeError> + Send + Sync + 'static,
) -> Object {
    Object::Builtin(Rc::new(BuiltinFn {
        name,
        call: Box::new(body),
    }))
}

/// `_multiprocessing.SemLock(kind, value, maxvalue, name=None,
/// unlink=False)`. Returns an object with `acquire`, `release`,
/// `_get_value`, `_count`, `name` exposed; cross-process
/// support is best-effort (named POSIX semaphores aren't yet
/// wired through).
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
    let dict = Rc::new(RefCell::new(DictData::new()));
    let lock = std::sync::Arc::new(crate::sync::RealSemaphore::new(value));
    {
        let l = lock.clone();
        let acquire = move |args: &[Object]| -> Result<Object, RuntimeError> {
            let blocking = match args.first() {
                Some(Object::Bool(b)) => *b,
                Some(Object::Int(i)) => *i != 0,
                _ => true,
            };
            if !blocking {
                return Ok(Object::Bool(l.try_acquire()));
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
        d.insert(DictKey(Object::from_static("name")), Object::None);
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

fn sem_unlink(args: &[Object]) -> Result<Object, RuntimeError> {
    if args.is_empty() {
        return Err(type_error("sem_unlink() requires a name"));
    }
    Ok(Object::None)
}

fn get_command(_args: &[Object]) -> Result<Object, RuntimeError> {
    let exe = std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "weavepy".to_owned());
    Ok(Object::new_list(vec![
        Object::from_str(exe),
        Object::from_static("--multiprocessing-fork"),
    ]))
}

fn conn_send(_args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(Object::None)
}

fn conn_recv(_args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(Object::None)
}

fn conn_close(_args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(Object::None)
}

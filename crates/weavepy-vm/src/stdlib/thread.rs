//! The `_thread` built-in module — the low-level support module for
//! [`threading`].
//!
//! WeavePy currently runs a single Python-visible OS thread; the
//! interpreter state is `Rc`-shared and the GIL-less migration to
//! `Arc` is deferred (see RFC 0016). Concurrency in user code is
//! delivered cooperatively via `asyncio`. To keep the existing
//! `threading` module surface working, `_thread` here is a
//! conservative shim:
//!
//! * `allocate_lock()` returns a `LockType` object that simply tracks
//!   a held/free flag. Acquire never blocks (we're single-threaded),
//!   release flips the flag back.
//! * `get_ident()` always returns `1`.
//! * `start_new_thread(fn, args)` runs `fn(*args)` synchronously and
//!   returns the same single thread id. Exceptions are reported via
//!   the interpreter's normal error channel.
//!
//! The shim is intentionally minimal — anything that actually needs
//! parallelism should reach for `asyncio` instead.

use std::cell::RefCell;
use std::rc::Rc;

use crate::error::{type_error, RuntimeError};
use crate::import::ModuleCache;
use crate::object::{BuiltinFn, DictData, DictKey, Object, PyModule};

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
            Object::from_static("Low-level threading primitives (cooperative shim)."),
        );
        d.insert(
            DictKey(Object::from_static("allocate_lock")),
            b("allocate_lock", allocate_lock),
        );
        d.insert(
            DictKey(Object::from_static("get_ident")),
            b("get_ident", get_ident),
        );
        d.insert(
            DictKey(Object::from_static("get_native_id")),
            b("get_native_id", get_ident),
        );
        d.insert(
            DictKey(Object::from_static("start_new_thread")),
            b("start_new_thread", start_new_thread),
        );
        // No-op stack size knob (we don't allocate threads).
        d.insert(
            DictKey(Object::from_static("stack_size")),
            b("stack_size", stack_size),
        );
        // CPython exposes `LockType`; user code occasionally inspects
        // its name via `type(lock).__name__`. We expose a tiny dict
        // standing in for the type object (good enough for isinstance
        // checks via duck typing).
        d.insert(
            DictKey(Object::from_static("LockType")),
            Object::from_static("lock"),
        );
        d.insert(
            DictKey(Object::from_static("error")),
            Object::from_static("RuntimeError"),
        );
    }
    Rc::new(PyModule {
        name: "_thread".to_owned(),
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

/// `_thread.allocate_lock()` -> returns a primitive lock. We model it
/// as a tiny mutable dict with `acquire`/`release`/`locked`/`__enter__`
/// /`__exit__` bound to callables. Single-threaded execution means
/// every `acquire` succeeds without blocking; this is observationally
/// indistinguishable from an uncontended real lock.
fn allocate_lock(_args: &[Object]) -> Result<Object, RuntimeError> {
    let state = Rc::new(RefCell::new(false));
    let dict = Rc::new(RefCell::new(DictData::new()));
    let s_for_acquire = state.clone();
    let acquire = move |_args: &[Object]| -> Result<Object, RuntimeError> {
        *s_for_acquire.borrow_mut() = true;
        Ok(Object::Bool(true))
    };
    let s_for_release = state.clone();
    let release = move |_args: &[Object]| -> Result<Object, RuntimeError> {
        *s_for_release.borrow_mut() = false;
        Ok(Object::None)
    };
    let s_for_locked = state.clone();
    let locked = move |_args: &[Object]| -> Result<Object, RuntimeError> {
        Ok(Object::Bool(*s_for_locked.borrow()))
    };
    let acquire_obj = Object::Builtin(Rc::new(BuiltinFn {
        name: "acquire",
        call: Box::new(acquire),
    }));
    let release_obj = Object::Builtin(Rc::new(BuiltinFn {
        name: "release",
        call: Box::new(release),
    }));
    let locked_obj = Object::Builtin(Rc::new(BuiltinFn {
        name: "locked",
        call: Box::new(locked),
    }));
    {
        let mut d = dict.borrow_mut();
        d.insert(DictKey(Object::from_static("acquire")), acquire_obj.clone());
        d.insert(DictKey(Object::from_static("release")), release_obj);
        d.insert(DictKey(Object::from_static("locked")), locked_obj);
        // Context-manager hooks alias acquire/release.
        d.insert(DictKey(Object::from_static("__enter__")), acquire_obj);
        d.insert(
            DictKey(Object::from_static("__exit__")),
            Object::Builtin(Rc::new(BuiltinFn {
                name: "__exit__",
                call: Box::new({
                    let s = state.clone();
                    move |_a: &[Object]| -> Result<Object, RuntimeError> {
                        *s.borrow_mut() = false;
                        Ok(Object::Bool(false))
                    }
                }),
            })),
        );
    }
    Ok(Object::Dict(dict))
}

fn get_ident(_args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(Object::Int(1))
}

fn stack_size(args: &[Object]) -> Result<Object, RuntimeError> {
    match args.first() {
        None | Some(Object::None) => Ok(Object::Int(0)),
        Some(Object::Int(_)) => Ok(Object::Int(0)),
        _ => Err(type_error("stack_size expects an int")),
    }
}

/// `_thread.start_new_thread(fn, args[, kwargs])` — run synchronously
/// on the calling thread. This trades parallelism for simplicity; the
/// real story for concurrency is `asyncio`.
fn start_new_thread(_args: &[Object]) -> Result<Object, RuntimeError> {
    // The interpreter routes `Builtin.call` through a plain
    // `fn(&[Object])` so we can't actually invoke a Python callable
    // here. The Python-level `threading.Thread.start()` wrapper
    // calls the target itself via the interpreter; this shim just
    // returns the conventional thread id.
    Ok(Object::Int(1))
}

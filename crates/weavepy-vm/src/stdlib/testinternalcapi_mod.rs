//! Native stand-in for CPython's `_testinternalcapi` C test helper.
//!
//! CPython's regression suite imports this extension to observe
//! interpreter internals. WeavePy implements the handful of probes the
//! conformance targets use, mapped onto *our* equivalent internal
//! state rather than faked answers:
//!
//! - `has_inline_values(obj)` — CPython 3.13 reports whether an
//!   instance's attributes still live in the object's inline value
//!   array (no materialised dict escape). WeavePy instances always
//!   carry a dict, but the *observable lifecycle* CPython tests —
//!   fresh managed-dict instances are inline, `del obj.__dict__` /
//!   `obj.__dict__ = d` and attribute-count blowups de-inline — is
//!   tracked faithfully via [`PyInstance::inline_values`] plus a
//!   capacity check mirroring CPython's shared-keys limit (30).

use crate::sync::Rc;
use crate::sync::RefCell;

use crate::error::RuntimeError;
use crate::import::ModuleCache;
use crate::object::{BuiltinFn, DictData, DictKey, Object, PyModule};

use parking_lot::{Condvar, Mutex};
use std::sync::Arc;
use std::thread::JoinHandle;

/// CPython's `SHARED_KEYS_MAX_SIZE`: instances whose dict outgrows the
/// shared-keys capacity stop using inline values.
const INLINE_CAPACITY: usize = 30;

/// A raw, non-Python OS thread spawned by `_spawn_pthread_waiter` that simply
/// blocks until `_end_spawned_pthread` releases it. It deliberately bypasses
/// WeavePy's `_thread`/`threading` machinery so it is invisible to
/// `threading.enumerate()`/`active_count()` — exactly like the raw `pthread`
/// CPython's `_testcapi._spawn_pthread_waiter` creates. Its sole observable
/// effect is bumping the live OS-thread count, which `os.fork()` detects to
/// emit the multi-threaded-fork `DeprecationWarning`
/// (`test_os.ForkTests.test_fork_warns_when_non_python_thread_exists`).
struct PthreadWaiter {
    handle: JoinHandle<()>,
    stop: Arc<WaiterGate>,
}

struct WaiterGate {
    flag: Mutex<bool>,
    cv: Condvar,
}

/// The currently-live raw waiter, if any. A process-global `parking_lot::Mutex`
/// (not the VM's `Rc`-based cells) so the spawn/end pair can stash and reclaim
/// the `JoinHandle` across calls.
static WAITER: Mutex<Option<PthreadWaiter>> = Mutex::new(None);

/// `_testcapi._spawn_pthread_waiter()` — create one raw OS thread that parks
/// until `_end_spawned_pthread()`. Spawning a second without ending the first
/// raises, matching the C helper's single-slot contract.
fn spawn_pthread_waiter(_args: &[Object]) -> Result<Object, RuntimeError> {
    let mut slot = WAITER.lock();
    if slot.is_some() {
        return Err(crate::error::runtime_error(
            "_spawn_pthread_waiter: a waiter thread is already running",
        ));
    }
    let gate = Arc::new(WaiterGate {
        flag: Mutex::new(false),
        cv: Condvar::new(),
    });
    let gate_for_thread = gate.clone();
    let handle = std::thread::Builder::new()
        .name("testcapi-pthread-waiter".to_owned())
        .spawn(move || {
            let mut stopped = gate_for_thread.flag.lock();
            while !*stopped {
                gate_for_thread.cv.wait(&mut stopped);
            }
        })
        .map_err(|e| crate::error::runtime_error(format!("_spawn_pthread_waiter: {e}")))?;
    *slot = Some(PthreadWaiter { handle, stop: gate });
    Ok(Object::None)
}

/// `_testcapi._end_spawned_pthread()` — signal the parked waiter to exit and
/// join it. A no-op if no waiter is live (so a `finally:` cleanup is safe even
/// when spawning failed).
fn end_spawned_pthread(_args: &[Object]) -> Result<Object, RuntimeError> {
    let waiter = WAITER.lock().take();
    if let Some(w) = waiter {
        {
            let mut flag = w.stop.flag.lock();
            *flag = true;
            w.stop.cv.notify_all();
        }
        let _ = w.handle.join();
    }
    Ok(Object::None)
}

fn has_inline_values(args: &[Object]) -> Result<Object, RuntimeError> {
    let inline = match args.first() {
        Some(Object::Instance(inst)) => {
            inst.cls().has_managed_dict()
                && !inst.cls().has_var_sized_base()
                && inst.inline_values.get()
                && inst.dict.borrow().len() <= INLINE_CAPACITY
        }
        _ => false,
    };
    Ok(Object::Bool(inline))
}

pub fn build(_cache: &ModuleCache) -> Rc<PyModule> {
    let dict = Rc::new(RefCell::new(DictData::default()));
    {
        let mut d = dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("__name__")),
            Object::from_static("_testinternalcapi"),
        );
        d.insert(
            DictKey(Object::from_static("__doc__")),
            Object::from_static("WeavePy stand-in for CPython internal-API test probes."),
        );
        d.insert(
            DictKey(Object::from_static("has_inline_values")),
            Object::Builtin(Rc::new(BuiltinFn {
                name: "has_inline_values",
                binds_instance: false,
                call: Box::new(has_inline_values),
                call_kw: None,
            })),
        );
        // Raw-`pthread` spawn/join helpers re-exported by the frozen
        // `_testcapi` shim. These create a genuine non-Python OS thread so
        // `os.fork()`'s multi-threaded-fork `DeprecationWarning` fires even
        // though `threading` never sees the thread
        // (`test_os.test_fork_warns_when_non_python_thread_exists`).
        d.insert(
            DictKey(Object::from_static("_spawn_pthread_waiter")),
            Object::Builtin(Rc::new(BuiltinFn {
                name: "_spawn_pthread_waiter",
                binds_instance: false,
                call: Box::new(spawn_pthread_waiter),
                call_kw: None,
            })),
        );
        d.insert(
            DictKey(Object::from_static("_end_spawned_pthread")),
            Object::Builtin(Rc::new(BuiltinFn {
                name: "_end_spawned_pthread",
                binds_instance: false,
                call: Box::new(end_spawned_pthread),
                call_kw: None,
            })),
        );
        // `get_recursion_depth()` — the live Python call depth on this
        // thread, read straight off the RFC 0037 recursion guard.
        // `test.support.get_recursion_depth()`/`infinite_recursion()` use it
        // to size `sys.setrecursionlimit` windows (RFC 0048).
        d.insert(
            DictKey(Object::from_static("get_recursion_depth")),
            Object::Builtin(Rc::new(BuiltinFn {
                name: "get_recursion_depth",
                binds_instance: false,
                call: Box::new(|_args| {
                    // The builtin call itself does not hold a guard, so the
                    // depth here is the caller's frame depth. `test.support`
                    // subtracts one for its own frame; mirror CPython by
                    // reporting the count including the caller.
                    let depth = crate::recursion::current_depth().max(1);
                    Ok(Object::Int(depth as i64))
                }),
                call_kw: None,
            })),
        );
    }
    {
        let mut d = dict.borrow_mut();
        // `get_config()` — the runtime-config dict `test.support` probes.
        // WeavePy ships PEP 657 column positions (RFC 0033/0037), so
        // `code_debug_ranges` is truthfully 1.
        d.insert(
            DictKey(Object::from_static("get_config")),
            Object::Builtin(Rc::new(BuiltinFn {
                name: "get_config",
                binds_instance: false,
                call: Box::new(|_args| {
                    let cfg = Rc::new(RefCell::new(DictData::default()));
                    {
                        let mut c = cfg.borrow_mut();
                        c.insert(
                            DictKey(Object::from_static("code_debug_ranges")),
                            Object::Int(1),
                        );
                    }
                    Ok(Object::Dict(cfg))
                }),
                call_kw: None,
            })),
        );
        // Immortalization knobs: WeavePy has no deferred-immortalization
        // pass, so suppression is a sound no-op and nothing is deferred.
        d.insert(
            DictKey(Object::from_static("suppress_immortalization")),
            Object::Builtin(Rc::new(BuiltinFn {
                name: "suppress_immortalization",
                binds_instance: false,
                call: Box::new(|_args| Ok(Object::None)),
                call_kw: None,
            })),
        );
        d.insert(
            DictKey(Object::from_static("get_immortalize_deferred")),
            Object::Builtin(Rc::new(BuiltinFn {
                name: "get_immortalize_deferred",
                binds_instance: false,
                call: Box::new(|_args| Ok(Object::Bool(false))),
                call_kw: None,
            })),
        );
    }
    Rc::new(PyModule {
        name: "_testinternalcapi".to_owned(),
        filename: None,
        dict,
    })
}

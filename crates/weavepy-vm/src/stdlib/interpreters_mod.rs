//! PEP 684 sub-interpreters — `interpreters.create()`,
//! `interpreters.run_string()`, `interpreters.destroy()`, plus the
//! cross-interpreter channel/queue object used to pass data
//! between them.
//!
//! Each sub-interpreter owns its own `crate::Interpreter` instance:
//! independent module cache, builtins dict, exception stack, frame
//! stack, and observability state (trace/profile/monitoring hooks
//! don't leak between interpreters, matching PEP 684).
//!
//! Channels are global — they're addressable by ID from any
//! interpreter and back the high-level `interpreters.Channel` /
//! `interpreters.Queue` objects. Only "shareable" values cross the
//! boundary (PEP 684 §4.4): bool, int, float, complex, bytes, str,
//! None, and tuples of shareable values. Anything else raises
//! `interpreters.NotShareableError`.

use std::collections::HashMap;
use std::sync::Mutex;

use crate::error::{runtime_error, type_error, value_error, RuntimeError};
use crate::object::{BuiltinFn, DictData, DictKey, Object, PyModule};
use crate::sync::{Rc, RefCell};

/// A registered sub-interpreter. Each one is an isolated
/// [`crate::Interpreter`] instance — its module cache, builtins,
/// frame stack, and observability state are independent of the
/// owning process's main interpreter.
struct InterpreterEntry {
    interp: Box<crate::Interpreter>,
    /// Per-interpreter `__main__` globals — re-used across
    /// `run_string` calls so user-set names persist between
    /// invocations (matches CPython's `InterpreterPoolExecutor`
    /// semantics).
    globals: Rc<RefCell<DictData>>,
}

/// Process-wide sub-interpreter registry. PEP 684 leaves the
/// concrete storage to the implementation; we use a `Mutex<HashMap>`
/// behind a [`std::sync::OnceLock`] so embedders that share the VM
/// across threads see a consistent view.
struct Registry {
    next_id: u64,
    interps: HashMap<u64, InterpreterEntry>,
    channels: HashMap<u64, ChannelEntry>,
    next_channel: u64,
}

impl Registry {
    fn new() -> Self {
        Self {
            next_id: 1,
            interps: HashMap::new(),
            channels: HashMap::new(),
            next_channel: 1,
        }
    }
}

/// A channel buffers `(value)` tuples between sub-interpreters.
struct ChannelEntry {
    buffer: std::collections::VecDeque<Object>,
    /// `True` once `close()` has been called. Subsequent `send`
    /// raises `ChannelClosedError`; pending `recv`s drain the
    /// buffer and then raise the same error.
    closed: bool,
}

fn registry() -> &'static Mutex<Registry> {
    static REG: std::sync::OnceLock<Mutex<Registry>> = std::sync::OnceLock::new();
    REG.get_or_init(|| Mutex::new(Registry::new()))
}

/// `True` when `obj` is allowed to cross the sub-interpreter
/// boundary. Per PEP 684 these are: `None`, `bool`, `int`, `float`,
/// `bytes`, `str`, `complex` (not modelled today), and tuples of
/// shareable values.
fn is_shareable(obj: &Object) -> bool {
    match obj {
        Object::None
        | Object::Bool(_)
        | Object::Int(_)
        | Object::Float(_)
        | Object::Bytes(_)
        | Object::Str(_) => true,
        Object::Tuple(items) => items.iter().all(is_shareable),
        _ => false,
    }
}

fn shareable_error(name: &str) -> RuntimeError {
    type_error(format!(
        "object of type '{}' is not shareable across interpreters",
        name
    ))
}

pub fn build(_cache: &crate::import::ModuleCache) -> Rc<PyModule> {
    let dict = Rc::new(RefCell::new(DictData::new()));
    {
        let mut d = dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("__name__")),
            Object::from_static("_xxsubinterpreters"),
        );
        d.insert(
            DictKey(Object::from_static("__doc__")),
            Object::from_static(
                "PEP 684 sub-interpreters. Use the `interpreters` package for the friendly API.",
            ),
        );
        d.insert(
            DictKey(Object::from_static("create")),
            builtin("create", i_create),
        );
        d.insert(
            DictKey(Object::from_static("destroy")),
            builtin("destroy", i_destroy),
        );
        d.insert(
            DictKey(Object::from_static("list_all")),
            builtin("list_all", i_list_all),
        );
        d.insert(
            DictKey(Object::from_static("get_current")),
            builtin("get_current", i_get_current),
        );
        d.insert(
            DictKey(Object::from_static("get_main")),
            builtin("get_main", i_get_main),
        );
        d.insert(
            DictKey(Object::from_static("is_running")),
            builtin("is_running", i_is_running),
        );
        d.insert(
            DictKey(Object::from_static("run_string")),
            builtin("run_string", i_run_string),
        );
        d.insert(
            DictKey(Object::from_static("is_shareable")),
            builtin("is_shareable", i_is_shareable),
        );
        d.insert(
            DictKey(Object::from_static("channel_create")),
            builtin("channel_create", c_create),
        );
        d.insert(
            DictKey(Object::from_static("channel_destroy")),
            builtin("channel_destroy", c_destroy),
        );
        d.insert(
            DictKey(Object::from_static("channel_send")),
            builtin("channel_send", c_send),
        );
        d.insert(
            DictKey(Object::from_static("channel_recv")),
            builtin("channel_recv", c_recv),
        );
        d.insert(
            DictKey(Object::from_static("channel_list_all")),
            builtin("channel_list_all", c_list_all),
        );
        d.insert(
            DictKey(Object::from_static("channel_close")),
            builtin("channel_close", c_close),
        );
    }
    Rc::new(PyModule {
        name: "_xxsubinterpreters".to_owned(),
        filename: None,
        dict,
    })
}

fn builtin(name: &'static str, body: fn(&[Object]) -> Result<Object, RuntimeError>) -> Object {
    Object::Builtin(Rc::new(BuiltinFn {
        name,
        binds_instance: false,
        call: Box::new(body),
        call_kw: None,
    }))
}

/// `_xxsubinterpreters.create(*, isolated=True)` — allocate a fresh
/// sub-interpreter and return its integer ID.
fn i_create(_args: &[Object]) -> Result<Object, RuntimeError> {
    let mut reg = registry()
        .lock()
        .map_err(|_| runtime_error("sub-interpreter registry poisoned"))?;
    let id = reg.next_id;
    reg.next_id += 1;
    let mut interp = Box::new(crate::Interpreter::new());
    let globals = interp.build_module_globals_for("__main__", Some("<sub-interpreter>"), None);
    reg.interps.insert(id, InterpreterEntry { interp, globals });
    Ok(Object::Int(id as i64))
}

fn i_destroy(args: &[Object]) -> Result<Object, RuntimeError> {
    let id = read_id(args.first(), "destroy")?;
    let mut reg = registry()
        .lock()
        .map_err(|_| runtime_error("sub-interpreter registry poisoned"))?;
    if reg.interps.remove(&id).is_none() {
        return Err(value_error(format!(
            "interpreter id {id} does not exist or has already been destroyed"
        )));
    }
    Ok(Object::None)
}

fn i_list_all(_args: &[Object]) -> Result<Object, RuntimeError> {
    let reg = registry()
        .lock()
        .map_err(|_| runtime_error("sub-interpreter registry poisoned"))?;
    let mut ids: Vec<u64> = reg.interps.keys().copied().collect();
    ids.sort_unstable();
    Ok(Object::new_list(
        ids.into_iter().map(|i| Object::Int(i as i64)).collect(),
    ))
}

fn i_get_current(_args: &[Object]) -> Result<Object, RuntimeError> {
    // Sub-interpreters run synchronously from this VM, so the
    // currently-executing one is whichever `run_string` is
    // unwinding on this thread. We track a thread-local
    // "current" id.
    Ok(Object::Int(current_id() as i64))
}

fn i_get_main(_args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(Object::Int(0))
}

fn i_is_running(args: &[Object]) -> Result<Object, RuntimeError> {
    let id = read_id(args.first(), "is_running")?;
    let reg = registry()
        .lock()
        .map_err(|_| runtime_error("sub-interpreter registry poisoned"))?;
    Ok(Object::Bool(reg.interps.contains_key(&id)))
}

fn i_is_shareable(args: &[Object]) -> Result<Object, RuntimeError> {
    let obj = args.first().cloned().unwrap_or(Object::None);
    Ok(Object::Bool(is_shareable(&obj)))
}

/// `_xxsubinterpreters.run_string(id, source)` — compile and
/// execute `source` inside the sub-interpreter identified by `id`.
///
/// Returns `None` on success. The function lifts the
/// sub-interpreter out of the registry while it runs so
/// re-entrant `run_string(id, …)` on the same id raises.
fn i_run_string(args: &[Object]) -> Result<Object, RuntimeError> {
    let id = read_id(args.first(), "run_string")?;
    let source = match args.get(1) {
        Some(Object::Str(s)) => s.to_string(),
        Some(other) => {
            return Err(type_error(format!(
                "run_string: source must be str, not '{}'",
                other.type_name()
            )))
        }
        None => return Err(type_error("run_string: missing source")),
    };
    // Pop the entry so concurrent `run_string` on the same id sees
    // it as "running".
    let mut entry = {
        let mut reg = registry()
            .lock()
            .map_err(|_| runtime_error("sub-interpreter registry poisoned"))?;
        reg.interps.remove(&id).ok_or_else(|| {
            value_error(format!(
                "interpreter id {id} does not exist or has already been destroyed"
            ))
        })?
    };
    push_current_id(id);
    let result = (|| -> Result<(), RuntimeError> {
        let module = weavepy_parser::parse_module(&source)
            .map_err(|e| crate::error::value_error(format!("run_string parse error: {e}")))?;
        let code =
            weavepy_compiler::compile_module_with_source(&module, &source, "<sub-interpreter>")
                .map_err(|e| crate::error::value_error(format!("run_string compile error: {e}")))?;
        entry
            .interp
            .exec_module_in(&code, entry.globals.clone())
            .map(|_| ())
    })();
    pop_current_id();
    // Re-insert the entry regardless of success so the id stays
    // alive for the next `run_string` / `destroy`.
    {
        let mut reg = registry()
            .lock()
            .map_err(|_| runtime_error("sub-interpreter registry poisoned"))?;
        reg.interps.insert(id, entry);
    }
    result?;
    Ok(Object::None)
}

// ---------- channels ----------

fn c_create(_args: &[Object]) -> Result<Object, RuntimeError> {
    let mut reg = registry()
        .lock()
        .map_err(|_| runtime_error("sub-interpreter registry poisoned"))?;
    let id = reg.next_channel;
    reg.next_channel += 1;
    reg.channels.insert(
        id,
        ChannelEntry {
            buffer: std::collections::VecDeque::new(),
            closed: false,
        },
    );
    Ok(Object::Int(id as i64))
}

fn c_destroy(args: &[Object]) -> Result<Object, RuntimeError> {
    let id = read_id(args.first(), "channel_destroy")?;
    let mut reg = registry()
        .lock()
        .map_err(|_| runtime_error("sub-interpreter registry poisoned"))?;
    if reg.channels.remove(&id).is_none() {
        return Err(value_error(format!(
            "channel id {id} does not exist or has already been destroyed"
        )));
    }
    Ok(Object::None)
}

fn c_send(args: &[Object]) -> Result<Object, RuntimeError> {
    let id = read_id(args.first(), "channel_send")?;
    let value = args.get(1).cloned().unwrap_or(Object::None);
    if !is_shareable(&value) {
        return Err(shareable_error(value.type_name()));
    }
    let mut reg = registry()
        .lock()
        .map_err(|_| runtime_error("sub-interpreter registry poisoned"))?;
    let entry = reg
        .channels
        .get_mut(&id)
        .ok_or_else(|| value_error(format!("channel id {id} does not exist")))?;
    if entry.closed {
        return Err(runtime_error("channel closed"));
    }
    entry.buffer.push_back(value);
    Ok(Object::None)
}

fn c_recv(args: &[Object]) -> Result<Object, RuntimeError> {
    let id = read_id(args.first(), "channel_recv")?;
    let default = args.get(1).cloned();
    let mut reg = registry()
        .lock()
        .map_err(|_| runtime_error("sub-interpreter registry poisoned"))?;
    let entry = reg
        .channels
        .get_mut(&id)
        .ok_or_else(|| value_error(format!("channel id {id} does not exist")))?;
    if let Some(v) = entry.buffer.pop_front() {
        return Ok(v);
    }
    if let Some(d) = default {
        return Ok(d);
    }
    if entry.closed {
        return Err(runtime_error("channel closed"));
    }
    // No async support — raise `ChannelEmptyError` so user code
    // can poll. CPython's blocking semantics need cross-thread
    // wakeups we don't have on every backend yet (RFC 0032).
    Err(runtime_error("channel is empty"))
}

fn c_list_all(_args: &[Object]) -> Result<Object, RuntimeError> {
    let reg = registry()
        .lock()
        .map_err(|_| runtime_error("sub-interpreter registry poisoned"))?;
    let mut ids: Vec<u64> = reg.channels.keys().copied().collect();
    ids.sort_unstable();
    Ok(Object::new_list(
        ids.into_iter().map(|i| Object::Int(i as i64)).collect(),
    ))
}

fn c_close(args: &[Object]) -> Result<Object, RuntimeError> {
    let id = read_id(args.first(), "channel_close")?;
    let mut reg = registry()
        .lock()
        .map_err(|_| runtime_error("sub-interpreter registry poisoned"))?;
    let entry = reg
        .channels
        .get_mut(&id)
        .ok_or_else(|| value_error(format!("channel id {id} does not exist")))?;
    entry.closed = true;
    Ok(Object::None)
}

fn read_id(arg: Option<&Object>, fn_name: &str) -> Result<u64, RuntimeError> {
    match arg {
        Some(Object::Int(i)) if *i >= 0 => Ok(*i as u64),
        Some(other) => Err(type_error(format!(
            "{}: id must be a non-negative int, not '{}'",
            fn_name,
            other.type_name()
        ))),
        None => Err(type_error(format!("{}: missing id", fn_name))),
    }
}

thread_local! {
    static CURRENT_ID_STACK: RefCell<Vec<u64>> = const { RefCell::new(Vec::new()) };
}

fn push_current_id(id: u64) {
    CURRENT_ID_STACK.with(|cell| cell.borrow_mut().push(id));
}

fn pop_current_id() {
    CURRENT_ID_STACK.with(|cell| {
        let _ = cell.borrow_mut().pop();
    });
}

fn current_id() -> u64 {
    CURRENT_ID_STACK.with(|cell| cell.borrow().last().copied().unwrap_or(0))
}

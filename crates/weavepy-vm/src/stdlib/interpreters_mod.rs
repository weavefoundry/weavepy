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

/// The full PEP 684 `PyInterpreterConfig` an interpreter was created
/// with, kept so `_interpreters.get_config` can report it verbatim
/// (test_interpreters test_get_config compares against the presets).
#[derive(Clone)]
pub(crate) struct SubinterpConfig {
    pub use_main_obmalloc: bool,
    pub allow_fork: bool,
    pub allow_exec: bool,
    pub allow_threads: bool,
    pub allow_daemon_threads: bool,
    pub check_multi_interp_extensions: bool,
    /// `"own"` or `"shared"` (PyInterpreterConfig.gil as the stdlib
    /// spells it; we never report `"default"` back).
    pub gil: &'static str,
}

impl SubinterpConfig {
    /// CPython's `_PyInterpreterConfig_LEGACY_INIT`.
    pub(crate) fn legacy() -> Self {
        Self {
            use_main_obmalloc: true,
            allow_fork: true,
            allow_exec: true,
            allow_threads: true,
            allow_daemon_threads: true,
            check_multi_interp_extensions: false,
            gil: "shared",
        }
    }

    /// CPython's `_PyInterpreterConfig_INIT` (the isolated default).
    pub(crate) fn isolated() -> Self {
        Self {
            use_main_obmalloc: false,
            allow_fork: false,
            allow_exec: false,
            allow_threads: true,
            allow_daemon_threads: false,
            check_multi_interp_extensions: true,
            gil: "own",
        }
    }

    /// The `interp->feature_flags` bits (`Py_RTFLAGS_*`) this config
    /// implies — what `os.fork`, `os.exec*`, and
    /// `_thread.daemon_threads_allowed` consult.
    fn feature_flags(&self) -> u32 {
        let mut flags: u32 = 0;
        if self.use_main_obmalloc {
            flags |= 1 << 5; // Py_RTFLAGS_USE_MAIN_OBMALLOC
        }
        if self.check_multi_interp_extensions {
            flags |= 1 << 8; // Py_RTFLAGS_MULTI_INTERP_EXTENSIONS
        }
        if self.allow_threads {
            flags |= 1 << 10; // Py_RTFLAGS_THREADS
        }
        if self.allow_daemon_threads {
            flags |= 1 << 11; // Py_RTFLAGS_DAEMON_THREADS
        }
        if self.allow_fork {
            flags |= 1 << 15; // Py_RTFLAGS_FORK
        }
        if self.allow_exec {
            flags |= 1 << 16; // Py_RTFLAGS_EXEC
        }
        flags
    }
}

/// A registered sub-interpreter. Each one is an isolated
/// [`crate::Interpreter`] instance — its module cache, builtins,
/// frame stack, and observability state are independent of the
/// owning process's main interpreter.
struct InterpreterEntry {
    /// `None` while a `run_string`/`run_func` holds the interpreter on
    /// some thread (CPython's "running" state — the tstate is active).
    /// The entry itself stays in the registry so `list_all`,
    /// `is_running`, and error shaping keep seeing the id.
    interp: Option<Box<crate::Interpreter>>,
    /// True while the interpreter is lifted out for a `__main__`-level
    /// exec (`run_string`/`run_func`/`exec_interpreter(main=True)`).
    /// CPython's `is_running_main` — what `_interpreters.is_running`
    /// reports; a non-main `exec_interpreter` doesn't count
    /// (test_interpreters TestInterpreterIsRunning "from C-API").
    running_main: bool,
    /// PEP 684 `whence` provenance (`_interpreters.WHENCE_*`):
    /// 2=legacy C-API, 3=C-API, 4=cross-interpreter C-API, 5=stdlib.
    whence: i64,
    /// The registry id of the interpreter that created this one
    /// (0 = main). Linked children die with their creator, mirroring
    /// CPython's id-refcount finalization at Py_EndInterpreter.
    parent: u64,
    /// The creation-time config, reported by `_interpreters.get_config`.
    config: SubinterpConfig,
    /// Per-interpreter `__main__` globals — re-used across
    /// `run_string` calls so user-set names persist between
    /// invocations (matches CPython's `InterpreterPoolExecutor`
    /// semantics).
    globals: Rc<RefCell<DictData>>,
    /// CPython `PyInterpreterState.id_refcount` — the PEP 684 lifetime
    /// refcount `_interpreters.incref/decref` manage (test_capi.test_misc
    /// InterpreterIDTests).
    refcount: i64,
    /// CPython's `_PyInterpreterState_RequireIDRef` latch: when linked,
    /// a decref that reaches 0 destroys the interpreter.
    linked: bool,
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
    queues: HashMap<u64, QueueEntry>,
    next_queue: u64,
}

impl Registry {
    fn new() -> Self {
        Self {
            next_id: 1,
            interps: HashMap::new(),
            channels: HashMap::new(),
            next_channel: 1,
            queues: HashMap::new(),
            next_queue: 1,
        }
    }
}

/// One queued cross-interpreter queue item (CPython `_queueitem`).
struct QueueItem {
    value: Object,
    /// The wrapper's format tag (`_SHARED_ONLY`/`_PICKLED`).
    fmt: i64,
    /// The unbound op to apply if the putting interpreter dies before
    /// the item is received (1=remove, 2=error, 3=replace).
    unboundop: i64,
    /// The registry id of the putting interpreter (0 = main).
    sender: u64,
    /// `Some(op)` once the sender was destroyed (the value is cleared,
    /// like CPython's `_queueitem_clear_data`).
    unbound: Option<i64>,
}

/// A cross-interpreter queue (CPython 3.13's `_interpqueues`,
/// `Modules/_interpqueuesmodule.c`). Like channels, the registry is
/// process-global so any interpreter can address a queue by id; each
/// buffered item carries the `(fmt, unboundop)` pair `put` recorded.
struct QueueEntry {
    buffer: std::collections::VecDeque<QueueItem>,
    maxsize: i64,
    /// Default item format (`_SHARED_ONLY`/`_PICKLED`) and unbound op,
    /// reported by `get_queue_defaults` from any interpreter.
    default_fmt: i64,
    default_unboundop: i64,
    /// `bind`/`release` reference count — the queue is destroyed when
    /// the last wrapper releases it (CPython `queue_release`).
    bindings: i64,
}

/// One queued channel item (CPython `_channelitem`).
struct ChannelItem {
    value: Object,
    /// The unbound op to apply if the sending interpreter dies before
    /// the item is received (per-item override or the channel default).
    unboundop: i64,
    /// The registry id of the sending interpreter (0 = main).
    sender: u64,
    /// `Some(op)` once the sender was destroyed — the item is
    /// "unbound" and `recv` reports the op (CPython `_channelitem`'s
    /// cleared `data` + kept `unboundop`).
    unbound: Option<i64>,
    /// Wakeup token for blocking sends (`_waiting_release`).
    seq: u64,
}

/// A cross-interpreter channel (CPython `_channel_state`), with the
/// full 3.13 surface: per-end interpreter association, per-interpreter
/// release, closing-vs-closed states, and unbound-item bookkeeping.
struct ChannelEntry {
    buffer: std::collections::VecDeque<ChannelItem>,
    /// `True` once the whole channel is closed. Subsequent `send`,
    /// `recv`, `close`, and `list_interpreters` raise
    /// `ChannelClosedError`.
    closed: bool,
    /// CPython 3.13's per-channel default "unbound op"
    /// (`_interpchannels.create(unboundop)` — what a received item
    /// resolves to when its sending interpreter was destroyed).
    /// Stored here so `get_channel_defaults` works from *any*
    /// interpreter, like CPython's process-global `_channels` state.
    default_unboundop: i64,
    /// End-specific close flags (`_interpchannels.close(cid,
    /// send=…, recv=…)`). Closing only the send end puts the channel
    /// in "closing" state until the buffer drains (CPython
    /// `channel_close`); the recv end can't close unforced while
    /// items are pending.
    closed_send: bool,
    closed_recv: bool,
    /// A channel fully closed through `close()` (not `release()`)
    /// drops out of `list_all` (test__interpchannels
    /// test_channel_list_all_closed vs test_channel_list_all_released)
    /// while staying resolvable so later operations report
    /// ChannelClosedError rather than NotFound.
    hidden: bool,
    /// Interpreters associated with each end (successful send/recv),
    /// minus those that have since released the end.
    send_assoc: std::collections::BTreeSet<u64>,
    recv_assoc: std::collections::BTreeSet<u64>,
    /// Interpreters that released an end: their own use of that end
    /// reports ChannelClosedError while the channel stays open for
    /// everyone else (test_partially).
    send_released: std::collections::BTreeSet<u64>,
    recv_released: std::collections::BTreeSet<u64>,
    /// Monotonic sequence for blocking-send wakeup tokens.
    next_seq: u64,
    /// CPython's ChannelID *object* refcount: each live `channelid`
    /// Python object holds a reference; when the last one is
    /// deallocated the channel is destroyed (`_channels_drop_id_object`
    /// — test_interpreters test_channels.TestChannels.test_list_all
    /// relies on earlier tests' channels vanishing with their objects).
    objcount: i64,
}

impl ChannelEntry {
    fn new(default_unboundop: i64) -> Self {
        Self {
            buffer: std::collections::VecDeque::new(),
            closed: false,
            default_unboundop,
            closed_send: false,
            closed_recv: false,
            hidden: false,
            send_assoc: std::collections::BTreeSet::new(),
            recv_assoc: std::collections::BTreeSet::new(),
            send_released: std::collections::BTreeSet::new(),
            recv_released: std::collections::BTreeSet::new(),
            next_seq: 0,
            objcount: 0,
        }
    }

    /// No interpreter is associated with either end any more.
    fn unassociated(&self) -> bool {
        self.send_assoc.is_empty() && self.recv_assoc.is_empty()
    }

    /// Force-close both ends, dropping pending items.
    fn force_close(&mut self) {
        self.buffer.clear();
        self.closed = true;
        self.closed_send = true;
        self.closed_recv = true;
    }
}

/// Wakes blocking `channel_send` waiters whenever a channel's state
/// changes (item received, channel closed, registry mutated).
fn channel_cv() -> &'static std::sync::Condvar {
    static CV: std::sync::OnceLock<std::sync::Condvar> = std::sync::OnceLock::new();
    CV.get_or_init(std::sync::Condvar::new)
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
        // gh-110246: buffer views cross interpreters (`send_buffer`
        // ships one over the sender's object; both sides see the same
        // memory, which our shared heap gives for free).
        Object::MemoryView(_) => true,
        // Channel IDs are shareable (CPython registers `channelid` in
        // the XID registry): the frozen `_interpchannels.ChannelID`
        // class marks itself so a cid can ride `set___main___attrs`
        // (test__interpchannels test_run_string_arg_unresolved) or a
        // channel (test_shareable).
        Object::Instance(inst) => inst.cls().lookup("_weave_xid_shareable").is_some(),
        _ => false,
    }
}

/// Value-decouple a shareable object (CPython's XID buffer round-trip):
/// fresh allocations for str/bytes/tuples so the receiver never sees
/// the sender's object. Memoryviews (send_buffer's shared buffer) and
/// marked instances (channel IDs) pass through by design.
fn xid_rebuild(obj: &Object) -> Object {
    match obj {
        Object::Str(s) => Object::from_str(s.to_string()),
        Object::Bytes(b) => Object::Bytes(crate::sync::Rc::from(&b[..])),
        Object::Tuple(items) => Object::new_tuple(items.iter().map(xid_rebuild).collect()),
        other => other.clone(),
    }
}

fn shareable_error(name: &str) -> RuntimeError {
    type_error(format!(
        "object of type '{}' is not shareable across interpreters",
        name
    ))
}

pub fn build(_cache: &crate::import::ModuleCache) -> Rc<PyModule> {
    let dict = Rc::new(RefCell::new(DictData::default()));
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
            DictKey(Object::from_static("whence")),
            builtin("whence", i_whence),
        );
        d.insert(
            DictKey(Object::from_static("get_config")),
            builtin("get_config", i_get_config),
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
        d.insert(
            DictKey(Object::from_static("channel_release")),
            builtin("channel_release", c_release),
        );
        d.insert(
            DictKey(Object::from_static("channel_list_interpreters")),
            builtin("channel_list_interpreters", c_list_interpreters),
        );
        d.insert(
            DictKey(Object::from_static("channel_get_defaults")),
            builtin("channel_get_defaults", c_get_defaults),
        );
        d.insert(
            DictKey(Object::from_static("channel_incref")),
            builtin("channel_incref", c_incref),
        );
        d.insert(
            DictKey(Object::from_static("channel_decref")),
            builtin("channel_decref", c_decref),
        );
        d.insert(
            DictKey(Object::from_static("channel_get_count")),
            builtin("channel_get_count", c_get_count),
        );
        d.insert(
            DictKey(Object::from_static("channel_get_info")),
            builtin("channel_get_info", c_get_info),
        );
        // Cross-interpreter queue primitives backing the frozen
        // `_interpqueues` shim (CPython 3.13's `_interpqueuesmodule.c`;
        // `test.support.interpreters.queues` is the stdlib consumer).
        for (name, f) in [
            (
                "queue_create",
                q_create as fn(&[Object]) -> Result<Object, RuntimeError>,
            ),
            ("queue_destroy", q_destroy),
            ("queue_list_all", q_list_all),
            ("queue_get_defaults", q_get_defaults),
            ("queue_bind", q_bind),
            ("queue_release", q_release),
            ("queue_get_maxsize", q_get_maxsize),
            ("queue_is_full", q_is_full),
            ("queue_get_count", q_get_count),
            ("queue_put", q_put),
            ("queue_get", q_get),
        ] {
            d.insert(
                DictKey(Object::from_static(name)),
                Object::Builtin(Rc::new(BuiltinFn {
                    name,
                    binds_instance: false,
                    call: Box::new(f),
                    call_kw: None,
                })),
            );
        }
        // RFC 0068 WS3 — the 3.13 `_interpreters` shim (frozen Python)
        // needs a few more primitives than the RFC 0031 surface.
        d.insert(
            DictKey(Object::from_static("set_main_attrs")),
            builtin("set_main_attrs", i_set_main_attrs),
        );
        d.insert(
            DictKey(Object::from_static("run_func")),
            builtin("run_func", i_run_func),
        );
        d.insert(
            DictKey(Object::from_static("_incref")),
            builtin("_incref", i_incref),
        );
        d.insert(
            DictKey(Object::from_static("_decref")),
            builtin("_decref", i_decref),
        );
        d.insert(
            DictKey(Object::from_static("_link")),
            builtin("_link", i_link),
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

/// `_xxsubinterpreters.create([check_multi_interp_extensions, own_gil])`
/// — allocate a fresh sub-interpreter and return its integer ID. The two
/// optional booleans carry the PEP 684 config fields the extension gate
/// consults (`_interpreters.create` passes them from its preset; the
/// bare call keeps the isolated defaults).
fn i_create(args: &[Object]) -> Result<Object, RuntimeError> {
    // PEP 684 default: `create()` gives an *isolated* interpreter —
    // own GIL, multi-interp extension check enforced, fork/exec/daemon
    // threads disallowed.
    let mut cfg = SubinterpConfig::isolated();
    cfg.check_multi_interp_extensions = args.first().is_none_or(Object::is_truthy);
    cfg.gil = if args.get(1).is_none_or(Object::is_truthy) {
        "own"
    } else {
        "shared"
    };
    cfg.allow_fork = args.get(2).is_some_and(Object::is_truthy);
    cfg.allow_exec = args.get(3).is_some_and(Object::is_truthy);
    cfg.allow_threads = args.get(4).is_none_or(Object::is_truthy);
    cfg.allow_daemon_threads = args.get(5).is_some_and(Object::is_truthy);
    cfg.use_main_obmalloc = args.get(6).is_some_and(Object::is_truthy);
    // WHENCE_STDLIB — the `_interpreters` module made it.
    let id = create_registered(cfg, 5)?;
    Ok(Object::Int(id as i64))
}

/// Allocate, configure, and register a fresh sub-interpreter; returns
/// its registry id. Shared by `_xxsubinterpreters.create` (whence
/// STDLIB) and the `_testinternalcapi` fixtures (whence LEGACY_CAPI /
/// CAPI / XI — test_interpreters' `interpreter_from_capi`).
pub(crate) fn create_registered(cfg: SubinterpConfig, whence: i64) -> Result<u64, RuntimeError> {
    // `_testcapi.set_nomemory` failure injection: CPython's counted
    // allocator makes `Py_NewInterpreterFromConfig`'s first malloc fail,
    // which `interp_create` reports as "interpreter creation failed"
    // (test_interpreters test_stress.test_create_interpreter_no_memory).
    if crate::stdlib::testinternalcapi_mod::nomem_alloc_fails() {
        return Err(crate::error::memory_error("interpreter creation failed"));
    }
    let mut reg = registry()
        .lock()
        .map_err(|_| runtime_error("sub-interpreter registry poisoned"))?;
    let id = reg.next_id;
    reg.next_id += 1;
    let mut interp = Box::new(crate::Interpreter::new());
    interp.set_subinterp_config(cfg.check_multi_interp_extensions, cfg.gil == "own");
    // CPython `interp->feature_flags` (Py_RTFLAGS_*): what `os.fork`,
    // `os.exec*`, and `_thread.daemon_threads_allowed` consult
    // (test__interpreters RunStringTests test_fork / test_os_exec /
    // test_create_daemon_thread).
    interp.set_subinterp_feature_flags(cfg.feature_flags());
    interp.inherit_sys_path_from_current();
    // Stamp the registry id so the eval breaker can attribute
    // `pending_identify` probes to this interpreter (and to worker
    // threads it spawns, which snapshot the id).
    interp.set_interp_id(id);
    // A fresh interpreter's `__main__` carries exactly CPython's
    // `add_main_module` shape: __name__/__doc__/__package__(None)/
    // __loader__/__spec__/__annotations__/__builtins__ and nothing else
    // (test__interpreters RunStringTests.test_execution_namespace_is_main
    // snapshots `vars()` and compares exhaustively — no __file__).
    let globals = interp.build_module_globals_for("__main__", None, None);
    {
        let mut g = globals.borrow_mut();
        g.insert(DictKey(Object::from_static("__package__")), Object::None);
        g.insert(
            DictKey(Object::from_static("__annotations__")),
            Object::Dict(Rc::new(RefCell::new(DictData::default()))),
        );
        g.insert(DictKey(Object::from_static("__loader__")), Object::None);
        g.insert(DictKey(Object::from_static("__spec__")), Object::None);
    }
    reg.interps.insert(
        id,
        InterpreterEntry {
            interp: Some(interp),
            running_main: false,
            whence,
            parent: current_id(),
            config: cfg,
            globals,
            refcount: 0,
            linked: false,
        },
    );
    Ok(id)
}

/// The id the next `create()` will hand out (the `_testinternalcapi`
/// `next_interpreter_id` fixture — test_interpreters GetCurrentTests).
pub(crate) fn peek_next_id() -> u64 {
    registry().lock().map(|reg| reg.next_id).unwrap_or(0)
}

/// Lift the interpreter out of its registry entry for a `run_*` call,
/// leaving the entry in place (so the id stays visible) with
/// `interp: None` marking it as running. Distinctive error text — the
/// frozen `_interpreters.py` shim retypes it into `InterpreterError`.
fn take_interp(
    id: u64,
    as_main: bool,
) -> Result<(Box<crate::Interpreter>, Rc<RefCell<DictData>>), RuntimeError> {
    let mut reg = registry()
        .lock()
        .map_err(|_| runtime_error("sub-interpreter registry poisoned"))?;
    let entry = reg.interps.get_mut(&id).ok_or_else(|| {
        value_error(format!(
            "interpreter id {id} does not exist or has already been destroyed"
        ))
    })?;
    let interp = entry
        .interp
        .take()
        .ok_or_else(|| runtime_error(format!("interpreter {id} is already running")))?;
    entry.running_main = as_main;
    Ok((interp, entry.globals.clone()))
}

/// Return the interpreter to its entry after a `run_*` call. The entry
/// may have been destroyed concurrently; the interpreter is then
/// simply dropped, like `Py_EndInterpreter`.
fn put_back_interp(id: u64, interp: Box<crate::Interpreter>) {
    if let Ok(mut reg) = registry().lock() {
        if let Some(entry) = reg.interps.get_mut(&id) {
            entry.interp = Some(interp);
            entry.running_main = false;
        }
    }
}

/// Compile and execute `source` inside the registered interpreter
/// `id`, holding it "running" for the duration. The code always runs in
/// the persistent `__main__` globals (CPython's `exec_interpreter`
/// does, regardless of `main`); `mark_running_main` controls whether
/// `is_running` reports True meanwhile (CPython's
/// `_PyInterpreterState_SetRunningMain` — only the `main=True` fixture
/// path and the stdlib `run_*` set it). With `print_uncaught`, an
/// unhandled exception renders to stderr inside the sub-interpreter
/// (PyRun_SimpleString semantics) and `Ok(-1)` is returned instead of
/// the error propagating.
pub(crate) fn exec_registered(
    id: u64,
    source: &str,
    mark_running_main: bool,
    print_uncaught: bool,
) -> Result<i32, RuntimeError> {
    let (mut interp, globals) = take_interp(id, mark_running_main)?;
    push_current_id(id);
    let result = (|| -> Result<(), RuntimeError> {
        let module = weavepy_parser::parse_module(source)
            .map_err(|e| crate::parse_error_to_syntax_error(&e, source, "<string>"))?;
        let code = weavepy_compiler::compile_module_with_source(&module, source, "<string>")
            .map_err(|e| crate::compile_error_to_syntax_error(&e, source, "<string>"))?;
        interp.exec_module_in(&code, globals).map(|_| ())
    })();
    let outcome = match result {
        Ok(()) => Ok(0),
        Err(RuntimeError::PyException(exc)) if print_uncaught => {
            // Rendered inside the sub-interpreter, where its traceback
            // machinery and sys.stderr live.
            if !interp.print_exception_via_traceback(&exc.instance) {
                eprintln!("{exc}");
            }
            Ok(-1)
        }
        Err(e) => Err(e),
    };
    pop_current_id();
    put_back_interp(id, interp);
    outcome
}

/// Destroy a registered interpreter (shared by `_xxsubinterpreters.destroy`
/// and the `_testinternalcapi` temp-interpreter runners).
pub(crate) fn destroy_registered(id: u64) -> Result<(), RuntimeError> {
    let mut reg = registry()
        .lock()
        .map_err(|_| runtime_error("sub-interpreter registry poisoned"))?;
    match reg.interps.get(&id) {
        None => {
            return Err(value_error(format!(
                "interpreter id {id} does not exist or has already been destroyed"
            )))
        }
        // A `run_string` holds the interpreter on some thread —
        // CPython's `Py_EndInterpreter` refuses ("interpreter still
        // running"; test__interpreters DestroyTests.test_still_running).
        Some(entry) if entry.interp.is_none() => {
            return Err(runtime_error(format!(
                "interpreter {id} is already running"
            )))
        }
        Some(_) => {}
    }
    reg.interps.remove(&id);
    // The dying interpreter lets go of its channel ends and any items
    // it sent become "unbound" (CPython `interp_destroy` →
    // `_channels_drop_interpreter`), and likewise for queued items.
    channels_drop_interpreter(&mut reg, id);
    queues_drop_interpreter(&mut reg, id);
    // Interpreters the dying one created through the *linked* lifetime
    // protocol die with it: in CPython the child's `Interpreter` object
    // (which holds the id ref) is finalized during Py_EndInterpreter and
    // its last decref destroys the child (test_interpreters ListAllTests
    // test_created_with_capi — the STDLIB interp made inside the temp
    // C-API interp is gone once the temp one is).
    let orphans: Vec<u64> = reg
        .interps
        .iter()
        .filter(|(_, e)| e.parent == id && e.linked && e.interp.is_some())
        .map(|(k, _)| *k)
        .collect();
    for child in orphans {
        reg.interps.remove(&child);
        channels_drop_interpreter(&mut reg, child);
        queues_drop_interpreter(&mut reg, child);
    }
    Ok(())
}

/// Whether the interpreter id is registered (used by the
/// `_testinternalcapi` fixtures' lenient teardown).
pub(crate) fn interp_registered(id: u64) -> bool {
    registry()
        .lock()
        .map(|reg| reg.interps.contains_key(&id))
        .unwrap_or(false)
}

fn i_destroy(args: &[Object]) -> Result<Object, RuntimeError> {
    let id = read_id(args.first(), "destroy")?;
    destroy_registered(id)?;
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
    // "Running" means a thread currently holds the interpreter for a
    // `__main__`-level exec (its entry is present with the interpreter
    // lifted out) — CPython's `_PyInterpreterState_IsRunningMain`. A
    // non-main `exec_interpreter` doesn't count (test_interpreters
    // TestInterpreterIsRunning "from C-API (running, but not __main__)").
    Ok(Object::Bool(
        reg.interps
            .get(&id)
            .is_some_and(|e| e.interp.is_none() && e.running_main),
    ))
}

/// `whence(id)` — PEP 684 provenance of the interpreter
/// (`_interpreters.WHENCE_*`; the main interpreter is handled by the
/// frozen shim, which reports WHENCE_RUNTIME for id 0).
fn i_whence(args: &[Object]) -> Result<Object, RuntimeError> {
    let id = read_id(args.first(), "whence")?;
    let reg = registry()
        .lock()
        .map_err(|_| runtime_error("sub-interpreter registry poisoned"))?;
    let entry = reg.interps.get(&id).ok_or_else(|| {
        value_error(format!(
            "interpreter id {id} does not exist or has already been destroyed"
        ))
    })?;
    Ok(Object::Int(entry.whence))
}

/// `get_config(id)` — the PyInterpreterConfig the interpreter was
/// created with, as a dict the frozen shim turns into the
/// `types.SimpleNamespace` `_interpreters.get_config` returns.
fn i_get_config(args: &[Object]) -> Result<Object, RuntimeError> {
    let id = read_id(args.first(), "get_config")?;
    let reg = registry()
        .lock()
        .map_err(|_| runtime_error("sub-interpreter registry poisoned"))?;
    let entry = reg.interps.get(&id).ok_or_else(|| {
        value_error(format!(
            "interpreter id {id} does not exist or has already been destroyed"
        ))
    })?;
    let cfg = &entry.config;
    let mut d = DictData::default();
    for (k, v) in [
        ("use_main_obmalloc", cfg.use_main_obmalloc),
        ("allow_fork", cfg.allow_fork),
        ("allow_exec", cfg.allow_exec),
        ("allow_threads", cfg.allow_threads),
        ("allow_daemon_threads", cfg.allow_daemon_threads),
        (
            "check_multi_interp_extensions",
            cfg.check_multi_interp_extensions,
        ),
    ] {
        d.insert(DictKey(Object::from_static(k)), Object::Bool(v));
    }
    d.insert(
        DictKey(Object::from_static("gil")),
        Object::from_static(cfg.gil),
    );
    Ok(Object::Dict(Rc::new(RefCell::new(d))))
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
    // Lift the interpreter out so concurrent `run_string` on the same
    // id sees it as "running" (the entry itself stays registered).
    let (mut interp, globals) = take_interp(id, true)?;
    push_current_id(id);
    let result = (|| -> Result<(), RuntimeError> {
        // A bad script surfaces as a real `SyntaxError` in the excinfo
        // snapshot `_interpreters.run_string` hands back (CPython runs
        // the script through the ordinary compile pipeline —
        // test__interpreters RunFailedTests.test_invalid_syntax checks
        // `excinfo.type.__name__ == 'SyntaxError'`).
        let module = weavepy_parser::parse_module(&source)
            .map_err(|e| crate::parse_error_to_syntax_error(&e, &source, "<string>"))?;
        let code =
            weavepy_compiler::compile_module_with_source(&module, &source, "<sub-interpreter>")
                .map_err(|e| crate::compile_error_to_syntax_error(&e, &source, "<string>"))?;
        interp.exec_module_in(&code, globals).map(|_| ())
    })();
    pop_current_id();
    put_back_interp(id, interp);
    result?;
    Ok(Object::None)
}

/// `set_main_attrs(id, ns_dict)` — bind shareable values into the
/// sub-interpreter's `__main__` globals (`_interpreters.set___main___attrs`,
/// `Interpreter.prepare_main`).
fn i_set_main_attrs(args: &[Object]) -> Result<Object, RuntimeError> {
    let id = read_id(args.first(), "set_main_attrs")?;
    let Some(Object::Dict(ns)) = args.get(1) else {
        return Err(type_error("set_main_attrs: expected a dict of attrs"));
    };
    for (_, v) in ns.borrow().iter() {
        if !is_shareable(v) {
            return Err(shareable_error(v.type_name()));
        }
    }
    let mut reg = registry()
        .lock()
        .map_err(|_| runtime_error("sub-interpreter registry poisoned"))?;
    let entry = reg.interps.get_mut(&id).ok_or_else(|| {
        value_error(format!(
            "interpreter id {id} does not exist or has already been destroyed"
        ))
    })?;
    // CPython's `_PyXI_Enter` refuses while the interpreter runs
    // (test_interpreters TestInterpreterPrepareMain.test_running — the
    // frozen shim retypes this into InterpreterError).
    if entry.interp.is_none() {
        return Err(runtime_error(format!(
            "interpreter {id} is already running"
        )));
    }
    let mut globals = entry.globals.borrow_mut();
    for (k, v) in ns.borrow().iter() {
        globals.insert(k.clone(), v.clone());
    }
    Ok(Object::None)
}

/// `run_func(id, func_or_code)` — execute a *stateless* function's code
/// object inside the sub-interpreter, with the sub-interpreter's
/// `__main__` globals (CPython's `_interpreters.run_func` execs the code
/// with `main.__dict__`, so a `global w` inside the function resolves to
/// what `set___main___attrs` planted — test__interpreters RunFuncTests).
/// Statelessness (no args, no closure) is validated by the frozen
/// `_interpreters.py` shim before it reaches here. Non-function
/// callables (the legacy `_xxsubinterpreters.run_func` surface) are
/// still called directly.
fn i_run_func(args: &[Object]) -> Result<Object, RuntimeError> {
    let id = read_id(args.first(), "run_func")?;
    let callable = args
        .get(1)
        .cloned()
        .ok_or_else(|| type_error("run_func: missing callable"))?;
    let code: Option<crate::sync::Rc<weavepy_compiler::CodeObject>> = match &callable {
        Object::Code(c) => Some(c.clone()),
        Object::Function(f) => Some(f.code.borrow().clone()),
        _ => None,
    };
    let (mut interp, globals) = take_interp(id, true)?;
    push_current_id(id);
    let result = match code {
        Some(code) => interp.exec_module_in(&code, globals).map(|_| Object::None),
        None => {
            let builtins = interp.builtins_dict();
            interp.call(&callable, &[], &[], &builtins)
        }
    };
    pop_current_id();
    put_back_interp(id, interp);
    result.map(|_| Object::None)
}

// ---------- channels ----------

fn c_create(args: &[Object]) -> Result<Object, RuntimeError> {
    // CPython-3.13 `unboundop` default (the `_interpchannels`
    // frontend always passes one; the legacy `_xxsubinterpreters`
    // surface never did).
    let default_unboundop = match args.first() {
        Some(Object::Int(op)) if (1..=3).contains(op) => *op,
        None | Some(Object::None) => 1,
        Some(Object::Int(op)) => {
            return Err(value_error(format!("unsupported unboundop {op}")));
        }
        Some(other) => {
            return Err(type_error(format!(
                "channel_create: unboundop must be int, not '{}'",
                other.type_name()
            )))
        }
    };
    let mut reg = registry()
        .lock()
        .map_err(|_| runtime_error("sub-interpreter registry poisoned"))?;
    let id = reg.next_channel;
    reg.next_channel += 1;
    reg.channels
        .insert(id, ChannelEntry::new(default_unboundop));
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
    channel_cv().notify_all();
    Ok(Object::None)
}

fn channel_not_found(id: u64) -> RuntimeError {
    value_error(format!("channel id {id} does not exist"))
}

fn channel_closed(id: u64) -> RuntimeError {
    // The exact phrase test__interpchannels regex-matches
    // (`test_recv_sending_interp_destroyed`); the frozen shim retypes
    // it into ChannelClosedError.
    runtime_error(format!("channel {id} is closed"))
}

/// `channel_send(cid, obj, unboundop, blocking, timeout)` — queue
/// `obj`. A blocking send waits (like CPython's `_waiting_acquire`)
/// until the object is *received*, the channel is closed under it
/// (ChannelClosedError), or the timeout elapses (TimeoutError, and the
/// unreceived item is withdrawn — test_send_timeout expects the
/// channel empty afterwards).
fn c_send(args: &[Object]) -> Result<Object, RuntimeError> {
    let id = read_id(args.first(), "channel_send")?;
    let value = args.get(1).cloned().unwrap_or(Object::None);
    if !is_shareable(&value) {
        return Err(shareable_error(value.type_name()));
    }
    let unboundop = match args.get(2) {
        Some(Object::Int(op)) if (1..=3).contains(op) => Some(*op),
        None | Some(Object::None) => None,
        Some(other) => {
            return Err(type_error(format!(
                "channel_send: unboundop must be int, not '{}'",
                other.type_name()
            )))
        }
    };
    let blocking = args.get(3).is_some_and(Object::is_truthy);
    let timeout = match args.get(4) {
        None | Some(Object::None) => None,
        Some(Object::Float(t)) => Some(*t),
        Some(Object::Int(t)) => Some(*t as f64),
        Some(other) => {
            return Err(type_error(format!(
                "channel_send: timeout must be a number, not '{}'",
                other.type_name()
            )))
        }
    };
    // CPython converts to cross-interpreter data at send time: the
    // receiver gets a *rebuilt* object, never the sender's
    // (test_send_recv_main asserts `obj is not orig`). Buffer views
    // and channel IDs deliberately pass through as-is.
    let value = xid_rebuild(&value);
    let me = current_id();
    let token = {
        let mut reg = registry()
            .lock()
            .map_err(|_| runtime_error("sub-interpreter registry poisoned"))?;
        let entry = reg
            .channels
            .get_mut(&id)
            .ok_or_else(|| channel_not_found(id))?;
        if entry.closed || entry.closed_send || entry.send_released.contains(&me) {
            return Err(channel_closed(id));
        }
        let op = unboundop.unwrap_or(entry.default_unboundop);
        let seq = entry.next_seq;
        entry.next_seq += 1;
        entry.buffer.push_back(ChannelItem {
            value,
            unboundop: op,
            sender: me,
            unbound: None,
            seq,
        });
        entry.send_assoc.insert(me);
        seq
    };
    if !blocking {
        return Ok(Object::None);
    }
    // Drop the GIL for the wait (`Py_BEGIN_ALLOW_THREADS`): the
    // receiving thread needs to run Python code to drain the item.
    crate::gil::allow_threads_then(|| {
        let deadline =
            timeout.map(|t| std::time::Instant::now() + std::time::Duration::from_secs_f64(t));
        let mut guard = registry()
            .lock()
            .map_err(|_| runtime_error("sub-interpreter registry poisoned"))?;
        loop {
            let Some(entry) = guard.channels.get_mut(&id) else {
                // Destroyed while waiting — the item is gone with it.
                return Err(channel_closed(id));
            };
            if !entry.buffer.iter().any(|it| it.seq == token) {
                if entry.closed {
                    // A force-close dropped the pending item before anyone
                    // received it (test_send_closed_while_waiting).
                    return Err(channel_closed(id));
                }
                return Ok(Object::None);
            }
            if entry.closed {
                return Err(channel_closed(id));
            }
            if let Some(dl) = deadline {
                let now = std::time::Instant::now();
                if now >= dl {
                    entry.buffer.retain(|it| it.seq != token);
                    return Err(crate::error::timeout_error("channel send timed out"));
                }
                let (g, _) = channel_cv()
                    .wait_timeout(guard, dl - now)
                    .map_err(|_| runtime_error("sub-interpreter registry poisoned"))?;
                guard = g;
            } else {
                // Periodic re-check guards against a missed notify.
                let (g, _) = channel_cv()
                    .wait_timeout(guard, std::time::Duration::from_millis(100))
                    .map_err(|_| runtime_error("sub-interpreter registry poisoned"))?;
                guard = g;
            }
        }
    })
}

/// `channel_recv(cid[, default])` → `(obj, unboundop)` — pop the next
/// item. Items whose sending interpreter has been destroyed come back
/// "unbound": the value is replaced per the item's unbound op and the
/// op is reported in the second slot (CPython `_channelitem_popped`).
fn c_recv(args: &[Object]) -> Result<Object, RuntimeError> {
    let id = read_id(args.first(), "channel_recv")?;
    let default = args.get(1).cloned();
    let me = current_id();
    let mut reg = registry()
        .lock()
        .map_err(|_| runtime_error("sub-interpreter registry poisoned"))?;
    let entry = reg
        .channels
        .get_mut(&id)
        .ok_or_else(|| channel_not_found(id))?;
    if entry.closed || entry.closed_recv || entry.recv_released.contains(&me) {
        return Err(channel_closed(id));
    }
    if let Some(item) = entry.buffer.pop_front() {
        entry.recv_assoc.insert(me);
        if entry.closed_send && entry.buffer.is_empty() {
            // The last pending item drained out of a "closing" channel
            // (CPython's closing → closed step).
            entry.closed = true;
            entry.closed_recv = true;
            entry.hidden = true;
        }
        channel_cv().notify_all();
        let (value, op) = match item.unbound {
            // The object didn't survive its interpreter: the receiver
            // gets (None, op) and the wrapper resolves the op
            // (UNBOUND singleton / ItemInterpreterDestroyed).
            Some(op) => (Object::None, Object::Int(op)),
            None => (item.value, Object::None),
        };
        return Ok(Object::new_tuple(vec![value, op]));
    }
    if entry.closed_send {
        entry.closed = true;
        entry.closed_recv = true;
        entry.hidden = true;
        return Err(channel_closed(id));
    }
    if let Some(d) = default {
        return Ok(Object::new_tuple(vec![d, Object::None]));
    }
    Err(runtime_error(format!("channel {id} is empty")))
}

fn c_list_all(_args: &[Object]) -> Result<Object, RuntimeError> {
    let reg = registry()
        .lock()
        .map_err(|_| runtime_error("sub-interpreter registry poisoned"))?;
    let mut ids: Vec<u64> = reg
        .channels
        .iter()
        .filter(|(_, e)| !e.hidden)
        .map(|(k, _)| *k)
        .collect();
    ids.sort_unstable();
    Ok(Object::new_list(
        ids.into_iter().map(|i| Object::Int(i as i64)).collect(),
    ))
}

/// `channel_incref(cid)` — a ChannelID Python object was created for
/// this channel (CPython's `_channels_incr_id_object`).
fn c_incref(args: &[Object]) -> Result<Object, RuntimeError> {
    let id = read_id(args.first(), "channel_incref")?;
    let mut reg = registry()
        .lock()
        .map_err(|_| runtime_error("sub-interpreter registry poisoned"))?;
    let entry = reg
        .channels
        .get_mut(&id)
        .ok_or_else(|| channel_not_found(id))?;
    entry.objcount += 1;
    Ok(Object::None)
}

/// `channel_decref(cid)` — a ChannelID object died. The last one
/// destroys the channel (CPython `_channels_drop_id_object`). An
/// already-destroyed channel is ignored (the object may outlive an
/// explicit `destroy()`).
fn c_decref(args: &[Object]) -> Result<Object, RuntimeError> {
    let id = read_id(args.first(), "channel_decref")?;
    let mut reg = registry()
        .lock()
        .map_err(|_| runtime_error("sub-interpreter registry poisoned"))?;
    if let Some(entry) = reg.channels.get_mut(&id) {
        entry.objcount -= 1;
        if entry.objcount <= 0 {
            reg.channels.remove(&id);
            channel_cv().notify_all();
        }
    }
    Ok(Object::None)
}

/// `channel_get_count(cid)` — the number of queued items
/// (test_interpreters test_channels' unbound-item accounting).
fn c_get_count(args: &[Object]) -> Result<Object, RuntimeError> {
    let id = read_id(args.first(), "channel_get_count")?;
    let reg = registry()
        .lock()
        .map_err(|_| runtime_error("sub-interpreter registry poisoned"))?;
    let entry = reg.channels.get(&id).ok_or_else(|| channel_not_found(id))?;
    if entry.closed {
        return Err(channel_closed(id));
    }
    Ok(Object::Int(entry.buffer.len() as i64))
}

/// `channel_close(cid, send, recv, force)` — close the named ends (or
/// both when neither is named), process-globally. An unforced close of
/// the recv end (or of both) with items still queued raises
/// ChannelNotEmptyError; closing only the send end leaves the channel
/// draining ("closing" state).
fn c_close(args: &[Object]) -> Result<Object, RuntimeError> {
    let id = read_id(args.first(), "channel_close")?;
    let truthy = |o: Option<&Object>| o.is_some_and(Object::is_truthy);
    let mut send_end = truthy(args.get(1));
    let mut recv_end = truthy(args.get(2));
    let force = truthy(args.get(3));
    if !send_end && !recv_end {
        send_end = true;
        recv_end = true;
    }
    let mut reg = registry()
        .lock()
        .map_err(|_| runtime_error("sub-interpreter registry poisoned"))?;
    let entry = reg
        .channels
        .get_mut(&id)
        .ok_or_else(|| channel_not_found(id))?;
    if entry.closed {
        return Err(channel_closed(id));
    }
    if !force && recv_end && !entry.buffer.is_empty() {
        return Err(runtime_error(format!(
            "channel {id} may not be closed if not empty (try force=True)"
        )));
    }
    if force {
        entry.buffer.clear();
    }
    if send_end {
        entry.closed_send = true;
    }
    if recv_end {
        entry.closed_recv = true;
    }
    if entry.buffer.is_empty() {
        // Any fully-drained close (either end) closes the channel
        // outright (test_close_empty runs all four end combinations).
        entry.closed = true;
        entry.closed_send = true;
        entry.closed_recv = true;
        entry.hidden = true;
    }
    channel_cv().notify_all();
    Ok(Object::None)
}

/// `channel_release(cid, send, recv)` — drop the *current*
/// interpreter's association with the named ends (both when neither is
/// named). When no interpreter remains associated with either end the
/// channel closes for everyone (CPython `_channels_drop_interpreter`).
fn c_release(args: &[Object]) -> Result<Object, RuntimeError> {
    let id = read_id(args.first(), "channel_release")?;
    let truthy = |o: Option<&Object>| o.is_some_and(Object::is_truthy);
    let mut send_end = truthy(args.get(1));
    let mut recv_end = truthy(args.get(2));
    if !send_end && !recv_end {
        send_end = true;
        recv_end = true;
    }
    let me = current_id();
    let mut reg = registry()
        .lock()
        .map_err(|_| runtime_error("sub-interpreter registry poisoned"))?;
    let entry = reg
        .channels
        .get_mut(&id)
        .ok_or_else(|| channel_not_found(id))?;
    if entry.closed {
        return Err(channel_closed(id));
    }
    if send_end {
        entry.send_assoc.remove(&me);
        entry.send_released.insert(me);
    }
    if recv_end {
        entry.recv_assoc.remove(&me);
        entry.recv_released.insert(me);
    }
    if entry.unassociated() {
        // The last associated interpreter let go; pending items are
        // dropped like a forced close, but the channel stays listed
        // (test_channel_list_all_released).
        entry.force_close();
    }
    channel_cv().notify_all();
    Ok(Object::None)
}

/// `channel_list_interpreters(cid, send)` — the interpreters currently
/// associated with the named end.
fn c_list_interpreters(args: &[Object]) -> Result<Object, RuntimeError> {
    let id = read_id(args.first(), "channel_list_interpreters")?;
    let send_end = args.get(1).is_some_and(Object::is_truthy);
    let reg = registry()
        .lock()
        .map_err(|_| runtime_error("sub-interpreter registry poisoned"))?;
    let entry = reg.channels.get(&id).ok_or_else(|| channel_not_found(id))?;
    if entry.closed || (send_end && entry.closed_send) || (!send_end && entry.closed_recv) {
        return Err(channel_closed(id));
    }
    let ids = if send_end {
        &entry.send_assoc
    } else {
        &entry.recv_assoc
    };
    Ok(Object::new_list(
        ids.iter().map(|i| Object::Int(*i as i64)).collect(),
    ))
}

/// Called when an interpreter is destroyed: its queued items become
/// "unbound" and its channel-end associations are dropped, closing any
/// channel left with no associated interpreters (CPython's
/// `_channels_drop_interpreter` from `interp_destroy`).
fn channels_drop_interpreter(reg: &mut Registry, interp: u64) {
    for entry in reg.channels.values_mut() {
        if entry.closed {
            continue;
        }
        entry.buffer.retain_mut(|item| {
            if item.sender == interp && item.unbound.is_none() {
                // UNBOUND_REMOVE (1) drops the item outright; the other
                // ops clear the value and are reported by recv
                // (test_interpreters test_channels
                // test_send_cleared_with_subinterpreter).
                if item.unboundop == 1 {
                    return false;
                }
                item.unbound = Some(item.unboundop);
                item.value = Object::None;
            }
            true
        });
        let was_associated = entry.send_assoc.remove(&interp) | entry.recv_assoc.remove(&interp);
        if was_associated && entry.unassociated() {
            entry.force_close();
        }
    }
    channel_cv().notify_all();
}

/// The queue counterpart of [`channels_drop_interpreter`]: items put by
/// the dying interpreter become "unbound" — removed outright for
/// UNBOUND_REMOVE (op 1), otherwise cleared with the op reported by
/// `queue_get` (test_interpreters test_queues
/// test_put_cleared_with_subinterpreter).
fn queues_drop_interpreter(reg: &mut Registry, interp: u64) {
    for entry in reg.queues.values_mut() {
        entry.buffer.retain_mut(|item| {
            if item.sender == interp && item.unbound.is_none() {
                if item.unboundop == 1 {
                    return false;
                }
                item.unbound = Some(item.unboundop);
                item.value = Object::None;
            }
            true
        });
    }
}

/// `channel_get_defaults(cid)` — the per-channel default unbound op
/// recorded at `channel_create` time (CPython 3.13
/// `_interpchannels.get_channel_defaults`).
fn c_get_defaults(args: &[Object]) -> Result<Object, RuntimeError> {
    let id = read_id(args.first(), "channel_get_defaults")?;
    let reg = registry()
        .lock()
        .map_err(|_| runtime_error("sub-interpreter registry poisoned"))?;
    let entry = reg
        .channels
        .get(&id)
        .ok_or_else(|| value_error(format!("channel id {id} does not exist")))?;
    Ok(Object::Int(entry.default_unboundop))
}

/// `channel_get_info(cid)` — `(closed, closing, count)` for the
/// frozen `_interpchannels` shim to wrap in its `ChannelInfo`
/// (CPython's is a full struct sequence; the wrapper only reads
/// `closed`/`closing`, plus `count` for `ChannelNotEmptyError`).
fn c_get_info(args: &[Object]) -> Result<Object, RuntimeError> {
    let id = read_id(args.first(), "channel_get_info")?;
    let reg = registry()
        .lock()
        .map_err(|_| runtime_error("sub-interpreter registry poisoned"))?;
    let entry = reg
        .channels
        .get(&id)
        .ok_or_else(|| value_error(format!("channel id {id} does not exist")))?;
    Ok(Object::new_tuple(vec![
        Object::Bool(entry.closed || entry.closed_recv),
        Object::Bool(entry.closed_send && !entry.closed),
        Object::Int(entry.buffer.len() as i64),
    ]))
}

// ------------------------------------------------------------------
// Cross-interpreter queues (`_interpqueues` backend). Errors use
// distinctive messages ("queue is empty" / "queue is full" /
// "does not exist") that the frozen shim retypes into its
// QueueEmpty/QueueFull/QueueNotFoundError hierarchy.
// ------------------------------------------------------------------

fn q_lock() -> Result<std::sync::MutexGuard<'static, Registry>, RuntimeError> {
    registry()
        .lock()
        .map_err(|_| runtime_error("sub-interpreter registry poisoned"))
}

fn q_arg_i64(args: &[Object], idx: usize, what: &str) -> Result<i64, RuntimeError> {
    args.get(idx)
        .and_then(Object::as_i64)
        .ok_or_else(|| type_error(format!("{what} must be an int")))
}

fn q_not_found(id: i64) -> RuntimeError {
    value_error(format!(
        "queue id {id} does not exist or has already been destroyed"
    ))
}

fn q_create(args: &[Object]) -> Result<Object, RuntimeError> {
    let maxsize = q_arg_i64(args, 0, "queue_create: maxsize")?;
    let default_fmt = q_arg_i64(args, 1, "queue_create: fmt")?;
    let default_unboundop = q_arg_i64(args, 2, "queue_create: unboundop")?;
    let mut reg = q_lock()?;
    let id = reg.next_queue;
    reg.next_queue += 1;
    reg.queues.insert(
        id,
        QueueEntry {
            buffer: std::collections::VecDeque::new(),
            maxsize,
            default_fmt,
            default_unboundop,
            bindings: 0,
        },
    );
    Ok(Object::Int(id as i64))
}

fn q_destroy(args: &[Object]) -> Result<Object, RuntimeError> {
    let id = q_arg_i64(args, 0, "queue_destroy: qid")?;
    let mut reg = q_lock()?;
    if reg.queues.remove(&(id as u64)).is_none() {
        return Err(q_not_found(id));
    }
    Ok(Object::None)
}

fn q_list_all(_args: &[Object]) -> Result<Object, RuntimeError> {
    let reg = q_lock()?;
    let mut ids: Vec<u64> = reg.queues.keys().copied().collect();
    ids.sort_unstable();
    Ok(Object::new_list(
        ids.into_iter()
            .map(|i| {
                let e = &reg.queues[&i];
                Object::new_tuple(vec![
                    Object::Int(i as i64),
                    Object::Int(e.default_fmt),
                    Object::Int(e.default_unboundop),
                ])
            })
            .collect(),
    ))
}

fn q_get_defaults(args: &[Object]) -> Result<Object, RuntimeError> {
    let id = q_arg_i64(args, 0, "queue_get_defaults: qid")?;
    let reg = q_lock()?;
    let e = reg
        .queues
        .get(&(id as u64))
        .ok_or_else(|| q_not_found(id))?;
    Ok(Object::new_tuple(vec![
        Object::Int(e.default_fmt),
        Object::Int(e.default_unboundop),
    ]))
}

fn q_bind(args: &[Object]) -> Result<Object, RuntimeError> {
    let id = q_arg_i64(args, 0, "queue_bind: qid")?;
    let mut reg = q_lock()?;
    let e = reg
        .queues
        .get_mut(&(id as u64))
        .ok_or_else(|| q_not_found(id))?;
    e.bindings += 1;
    Ok(Object::None)
}

fn q_release(args: &[Object]) -> Result<Object, RuntimeError> {
    let id = q_arg_i64(args, 0, "queue_release: qid")?;
    let mut reg = q_lock()?;
    let e = reg
        .queues
        .get_mut(&(id as u64))
        .ok_or_else(|| q_not_found(id))?;
    // Releasing an unbound queue is an error (test_interpreters
    // test_queues LowLevelTests.test_bind_release "release without
    // binding" — the shim retypes into QueueError).
    if e.bindings <= 0 {
        return Err(runtime_error(format!("queue {id} is not bound")));
    }
    e.bindings -= 1;
    // Last wrapper released: the queue is destroyed (CPython's
    // `queue_release` drops the registry entry with the final ref).
    if e.bindings <= 0 {
        reg.queues.remove(&(id as u64));
    }
    Ok(Object::None)
}

fn q_get_maxsize(args: &[Object]) -> Result<Object, RuntimeError> {
    let id = q_arg_i64(args, 0, "queue_get_maxsize: qid")?;
    let reg = q_lock()?;
    let e = reg
        .queues
        .get(&(id as u64))
        .ok_or_else(|| q_not_found(id))?;
    Ok(Object::Int(e.maxsize))
}

fn q_is_full(args: &[Object]) -> Result<Object, RuntimeError> {
    let id = q_arg_i64(args, 0, "queue_is_full: qid")?;
    let reg = q_lock()?;
    let e = reg
        .queues
        .get(&(id as u64))
        .ok_or_else(|| q_not_found(id))?;
    Ok(Object::Bool(
        e.maxsize > 0 && e.buffer.len() as i64 >= e.maxsize,
    ))
}

fn q_get_count(args: &[Object]) -> Result<Object, RuntimeError> {
    let id = q_arg_i64(args, 0, "queue_get_count: qid")?;
    let reg = q_lock()?;
    let e = reg
        .queues
        .get(&(id as u64))
        .ok_or_else(|| q_not_found(id))?;
    Ok(Object::Int(e.buffer.len() as i64))
}

fn q_put(args: &[Object]) -> Result<Object, RuntimeError> {
    let id = q_arg_i64(args, 0, "queue_put: qid")?;
    let value = args.get(1).cloned().unwrap_or(Object::None);
    let fmt = q_arg_i64(args, 2, "queue_put: fmt")?;
    let unboundop = q_arg_i64(args, 3, "queue_put: unboundop")?;
    // fmt 0 is `_SHARED_ONLY`: enforce shareability like channel_send.
    // fmt 1 (`_PICKLED`) arrives as bytes from the wrapper's
    // `pickle.dumps`, which is always shareable.
    if fmt == 0 && !is_shareable(&value) {
        return Err(shareable_error(value.type_name()));
    }
    // Value-decouple like the channel path: the getter must never see
    // the putter's object (test_interpreters test_queues
    // test_put_get_same_interpreter asserts `obj is not orig`).
    let value = xid_rebuild(&value);
    let sender = current_id();
    let mut reg = q_lock()?;
    let e = reg
        .queues
        .get_mut(&(id as u64))
        .ok_or_else(|| q_not_found(id))?;
    if e.maxsize > 0 && e.buffer.len() as i64 >= e.maxsize {
        return Err(runtime_error("queue is full"));
    }
    e.buffer.push_back(QueueItem {
        value,
        fmt,
        unboundop,
        sender,
        unbound: None,
    });
    Ok(Object::None)
}

fn q_get(args: &[Object]) -> Result<Object, RuntimeError> {
    let id = q_arg_i64(args, 0, "queue_get: qid")?;
    let mut reg = q_lock()?;
    let e = reg
        .queues
        .get_mut(&(id as u64))
        .ok_or_else(|| q_not_found(id))?;
    let Some(item) = e.buffer.pop_front() else {
        return Err(runtime_error("queue is empty"));
    };
    // An item whose putting interpreter has been destroyed reports its
    // unbound op instead of a value (the wrapper resolves it —
    // test_interpreters test_queues test_put_cleared_with_subinterpreter).
    match item.unbound {
        Some(op) => Ok(Object::new_tuple(vec![
            Object::None,
            Object::Int(item.fmt),
            Object::Int(op),
        ])),
        None => Ok(Object::new_tuple(vec![
            item.value,
            Object::Int(item.fmt),
            Object::None,
        ])),
    }
}

/// RFC 0068 WS3 — the PEP 684 interpreter-ID lifetime bookkeeping
/// CPython keeps on `PyInterpreterState` (`id_refcount` +
/// `requires_idref`), queried natively by `_testinternalcapi`
/// (test_capi.test_misc InterpreterIDTests) and driven by the frozen
/// `_interpreters.incref/decref`. `None` means "no such interpreter";
/// the main interpreter (id 0) reports as existing, runtime-linked.
pub(crate) fn id_exists(id: u64) -> bool {
    if id == 0 {
        return true;
    }
    registry()
        .lock()
        .map(|reg| reg.interps.contains_key(&id))
        .unwrap_or(false)
}

/// An interpreter ID guaranteed never to be handed out by `create()`
/// (CPython's fixture returns `INT64_MAX`).
pub(crate) fn unused_id() -> u64 {
    i64::MAX as u64
}

pub(crate) fn id_refcount(id: u64) -> Option<i64> {
    if id == 0 {
        return Some(1);
    }
    let reg = registry().lock().ok()?;
    reg.interps.get(&id).map(|e| e.refcount)
}

pub(crate) fn id_linked(id: u64) -> Option<bool> {
    if id == 0 {
        return Some(true);
    }
    let reg = registry().lock().ok()?;
    reg.interps.get(&id).map(|e| e.linked)
}

pub(crate) fn id_set_linked(id: u64, linked: bool) -> Option<()> {
    if id == 0 {
        return Some(());
    }
    let mut reg = registry().lock().ok()?;
    reg.interps.get_mut(&id).map(|e| e.linked = linked)
}

pub(crate) fn id_incref(id: u64) -> Option<i64> {
    if id == 0 {
        return Some(1);
    }
    let mut reg = registry().lock().ok()?;
    reg.interps.get_mut(&id).map(|e| {
        e.refcount += 1;
        e.refcount
    })
}

/// Decrement and report `(new_refcount, linked)`; the *caller* (the
/// frozen `_interpreters.decref`) destroys the interpreter when a
/// linked refcount reaches 0, so thread finalization runs in Python.
pub(crate) fn id_decref(id: u64) -> Option<(i64, bool)> {
    if id == 0 {
        return Some((1, true));
    }
    let mut reg = registry().lock().ok()?;
    reg.interps.get_mut(&id).map(|e| {
        e.refcount = (e.refcount - 1).max(0);
        (e.refcount, e.linked)
    })
}

/// `_xxsubinterpreters._incref(id)` — bump the PEP 684 lifetime refcount.
fn i_incref(args: &[Object]) -> Result<Object, RuntimeError> {
    let id = read_id(args.first(), "_incref")?;
    id_incref(id)
        .map(|_| Object::None)
        .ok_or_else(|| value_error(format!("interpreter id {id} does not exist")))
}

/// `_xxsubinterpreters._decref(id)` — returns `(new_refcount, linked)`.
fn i_decref(args: &[Object]) -> Result<Object, RuntimeError> {
    let id = read_id(args.first(), "_decref")?;
    id_decref(id)
        .map(|(count, linked)| Object::new_tuple(vec![Object::Int(count), Object::Bool(linked)]))
        .ok_or_else(|| value_error(format!("interpreter id {id} does not exist")))
}

/// `_xxsubinterpreters._link(id, linked)` — set the destroy-on-decref latch.
fn i_link(args: &[Object]) -> Result<Object, RuntimeError> {
    let id = read_id(args.first(), "_link")?;
    let linked = args.get(1).is_none_or(crate::object::Object::is_truthy);
    id_set_linked(id, linked)
        .map(|()| Object::None)
        .ok_or_else(|| value_error(format!("interpreter id {id} does not exist")))
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
    /// The interpreter that was *current* when each sub-interpreter
    /// entry was pushed — the host chain. The bottom entry is the
    /// main interpreter, which gh-144601's import path needs: a
    /// `PyInit` failure inside a sub-interpreter is reported through
    /// the **main** interpreter's `sys.unraisablehook`.
    static HOST_PTR_STACK: RefCell<Vec<Option<*mut crate::Interpreter>>> = const { RefCell::new(Vec::new()) };
}

fn push_current_id(id: u64) {
    CURRENT_ID_STACK.with(|cell| cell.borrow_mut().push(id));
    HOST_PTR_STACK.with(|cell| {
        cell.borrow_mut()
            .push(crate::vm_singletons::current_interpreter_ptr())
    });
}

fn pop_current_id() {
    CURRENT_ID_STACK.with(|cell| {
        let _ = cell.borrow_mut().pop();
    });
    HOST_PTR_STACK.with(|cell| {
        let _ = cell.borrow_mut().pop();
    });
}

fn current_id() -> u64 {
    CURRENT_ID_STACK.with(|cell| cell.borrow().last().copied().unwrap_or(0))
}

/// `True` while executing inside a sub-interpreter (`run_string` /
/// `run_func` / `exec` on this thread).
pub fn in_subinterpreter() -> bool {
    current_id() != 0
}

/// The main interpreter hosting the outermost sub-interpreter entry
/// on this thread, or `None` when not inside one. Callers may only
/// use the pointer while the sub-interpreter call is still on the
/// stack (the host is parked, live, and exclusively ours under the
/// GIL).
pub fn main_host_interpreter_ptr() -> Option<*mut crate::Interpreter> {
    HOST_PTR_STACK.with(|cell| cell.borrow().first().copied().flatten())
}

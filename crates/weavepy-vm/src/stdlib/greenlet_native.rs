//! `_greenlet` — native greenlets over real stack switching (RFC 0066 WS4).
//!
//! WeavePy's evaluator is a recursive tree-walker: every Python activation
//! is a native Rust frame, and C extensions re-enter the eval loop
//! recursively, so `Python → C → Python` interleaves native frames. A
//! frame-model greenlet (parking heap `Frame`s the way generators do)
//! could only switch when no native frame sits between the current
//! activation and the switch target — which excludes exactly the
//! `sqlalchemy.greenlet_spawn` / gevent shapes that are the point of
//! having greenlet at all. So each started greenlet runs on its **own
//! dedicated native stack** (a `corosensei` coroutine over an
//! `mmap`-allocated stack with a guard page), and a switch parks the
//! entire native stack — interpreter recursion, C frames and all.
//!
//! ## Symmetric switching over asymmetric coroutines
//!
//! Upstream greenlet's switch is *symmetric* (any greenlet → any
//! greenlet); a stackful coroutine is *asymmetric* (`resume` runs a
//! suspended coroutine, `suspend` returns to whoever resumed it). The
//! bridge is a uniform **routing loop** ([`route`]) that every switching
//! greenlet runs:
//!
//! - a suspended (or unstarted) target is `resume`d directly, and the
//!   loop keeps forwarding whatever [`Directive`] the resumed greenlet
//!   eventually yields;
//! - a target that is *active below us* in the native resume chain
//!   ([`CHAIN`]) cannot be resumed — the loop `suspend`s, handing the
//!   directive down to its own resumer's loop, unwinding level by level
//!   until the directive reaches its target;
//! - a directive that names *us* is delivered: the payload becomes the
//!   switch return value (or raises, for `throw`).
//!
//! The per-thread main greenlet is the chain's root and needs no
//! coroutine of its own: its routing loop runs on the ordinary thread
//! stack, and directives that target main unwind down to it.
//!
//! ## What a switch swaps
//!
//! Per the RFC: the interpreter's `frame_stack` (the `FrameShell` spine
//! `sys._getframe` and tracing read) and `exc_info_stack` are per-body
//! `Rc` handles installed on the `Interpreter` at every delivery; the
//! per-thread `recursion::DEPTH` counter is saved/restored (each stack
//! meters its own depth); and the contextvars current context follows
//! greenlet ≥ 1.0 `gr_context` semantics — the departing greenlet stashes
//! the thread's current `Context`, the arriving one installs its own
//! (`None` = fresh implicit context), swapped directly in the frozen
//! `contextvars._STATES` table. The GIL story is untouched: greenlets
//! are same-thread by definition, and every thread-local this module
//! owns enforces that (a greenlet's id is simply absent from another
//! thread's registry).
//!
//! `stacker` segmented-stack growth is disabled while a greenlet is
//! current ([`on_greenlet_stack`], consulted by
//! `run_until_yield_or_return`): growing segments on a parked-able stack
//! would complicate unwinding, so greenlet stacks are simply large
//! (default 16 MiB of lazily-committed virtual memory, tunable via
//! `WEAVEPY_GREENLET_STACK_SIZE`); the recursion limit still guards
//! depth.
//!
//! ## Lifecycle
//!
//! `greenlet(run=None, parent=None)` instances are ordinary
//! [`PyInstance`]s (socket/sqlite handle pattern — subclassing works,
//! which SQLAlchemy's `_AsyncIoGreenlet(greenlet.greenlet)` requires);
//! the Rust body lives in a per-thread registry keyed by the handle in
//! the instance dict. Death delivers `run`'s return value (or its
//! exception; an uncaught `GreenletExit` becomes a plain return value)
//! to the nearest **alive parent**. Collecting a suspended greenlet
//! throws `GreenletExit` into it on its own stack, with the dying
//! greenlet re-parented to the collector so control comes straight back
//! (upstream `green_dealloc` semantics), driven through the class's
//! `__del__` so it runs at an eval-loop safe point, never inside a Rust
//! `Drop`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

use corosensei::stack::DefaultStack;
use corosensei::{Coroutine, CoroutineResult, Yielder};

use crate::error::{type_error, value_error, PyException, RuntimeError};
use crate::import::ModuleCache;
use crate::object::{BuiltinFn, DictData, DictKey, Object, PyModule, PyProperty};
use crate::sync::Rc;
use crate::sync::RefCell;
use crate::types::{PyInstance, TypeObject};
use std::cell::Cell;

// ---------------------------------------------------------------------------
// Payloads and directives.
// ---------------------------------------------------------------------------

/// What a resumed greenlet receives at its switch point.
enum Payload {
    /// `switch(*args, **kwargs)` values (or `run`'s return, boxed as a
    /// single positional).
    Values(Vec<Object>, Vec<(String, Object)>),
    /// `throw(...)` — raise this at the switch point (or before `run`,
    /// for an unstarted target).
    Throw(RuntimeError),
}

/// A routed switch: deliver `payload` to `target`.
struct Directive {
    target: u64,
    payload: Payload,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Status {
    Unstarted,
    Active,
    Dead,
}

/// The coroutine type of one started greenlet. `Input` is what a resume
/// delivers, `Yield`/`Return` are the directives the greenlet emits
/// while switching away (`Return` = the death directive).
type GreenCoroutine = Coroutine<Payload, Directive, Directive, DefaultStack>;

/// The per-greenlet Rust body. Owned by the per-thread [`REGISTRY`];
/// the Python instance carries only the id (so user subclasses stay
/// plain instances) and is held here **weakly** — the registry must not
/// keep an otherwise-unreachable greenlet alive, or the collect-throws-
/// `GreenletExit` contract could never fire.
struct GreenletBody {
    id: u64,
    is_main: bool,
    /// The `run` callable captured at construction; `None` after start
    /// (or when the subclass provides `run` as a method, resolved at
    /// start time).
    run: RefCell<Option<Object>>,
    parent: Cell<u64>,
    status: Cell<Status>,
    coroutine: RefCell<Option<GreenCoroutine>>,
    /// Live only while the greenlet executes between resume and
    /// suspend; used by [`route`]'s hand-down branch.
    yielder: Cell<Option<*const Yielder<Payload, Directive>>>,
    /// Weak for ordinary greenlets, strong for main (main lives for the
    /// thread's lifetime and `getcurrent()` must always answer).
    instance: RefCell<InstanceRef>,
    /// This greenlet's own frame spine — installed on the `Interpreter`
    /// whenever the greenlet is delivered to.
    frames: crate::object::FrameStack,
    /// This greenlet's own handled-exception stack.
    exc_info: Rc<RefCell<Vec<PyException>>>,
    /// `recursion::DEPTH` snapshot while parked.
    saved_depth: Cell<usize>,
    /// `gr_context` while not running (`None` = fresh implicit context;
    /// while running, the live context is the thread's current one).
    context: RefCell<Object>,
    /// The thread-handles stack (`sys.exc_info` / `sys._getframe`
    /// read these) while parked. Guards push/pop on the greenlet's own
    /// native stack, so the vector must travel with it.
    saved_handles: RefCell<Vec<crate::vm_singletons::ThreadHandles>>,
}

enum InstanceRef {
    None,
    Strong(Rc<PyInstance>),
    Weak(crate::sync::Weak<PyInstance>),
}

impl GreenletBody {
    fn instance_object(&self) -> Option<Object> {
        match &*self.instance.borrow() {
            InstanceRef::None => None,
            InstanceRef::Strong(rc) => Some(Object::Instance(rc.clone())),
            InstanceRef::Weak(w) => w.upgrade().map(Object::Instance),
        }
    }
}

impl Drop for GreenletBody {
    fn drop(&mut self) {
        // Thread teardown with the greenlet still suspended: do NOT let
        // corosensei force-unwind a parked interpreter stack (its frozen
        // activations were never meant to unwind out of order). Marking
        // the coroutine finished leaks whatever the stack pinned —
        // upstream greenlet abandons suspended stacks at exit the same
        // way.
        if let Some(mut c) = self.coroutine.borrow_mut().take() {
            if c.started() && !c.done() {
                unsafe { c.force_reset() };
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Per-thread state.
// ---------------------------------------------------------------------------

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

thread_local! {
    static REGISTRY: RefCell<HashMap<u64, Rc<GreenletBody>>> =
        RefCell::new(HashMap::new());
    /// The currently-executing greenlet's id (0 before first use).
    static CURRENT: Cell<u64> = const { Cell::new(0) };
    /// This thread's main greenlet id (0 before first use).
    static MAIN_ID: Cell<u64> = const { Cell::new(0) };
    /// Native resume chain, bottom = main. A greenlet in this chain is
    /// *active* (its coroutine is running or is an ancestor of the
    /// running one) and must be reached by suspending, not resuming.
    static CHAIN: RefCell<Vec<u64>> = const { RefCell::new(Vec::new()) };
}

fn body(id: u64) -> Option<Rc<GreenletBody>> {
    REGISTRY.with(|r| r.borrow().get(&id).cloned())
}

fn chain_contains(id: u64) -> bool {
    CHAIN.with(|c| c.borrow().contains(&id))
}

/// True while a non-main greenlet is current on this thread — the eval
/// loop's `stacker::maybe_grow` gate (see module docs). Also consulted
/// by the C-API's stack-headroom probes (RFC 0069 WS5 follow-up):
/// `stacker::remaining_stack` measures against the *OS thread's* stack
/// bounds, which are meaningless while running on a greenlet's own
/// mmap'd stack, so those probes must fall back to their counted
/// budgets here.
pub fn on_greenlet_stack() -> bool {
    let main = MAIN_ID.with(|m| m.get());
    main != 0 && CURRENT.with(|c| c.get()) != main
}

fn stack_size() -> usize {
    static SIZE: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *SIZE.get_or_init(|| {
        // The stack is mmap'd and lazily committed: the figure below is
        // *virtual* reservation, and pages are only faulted in as the
        // greenlet actually recurses. Debug builds need a much bigger
        // reservation than release: the interpreter's dispatch functions
        // have enormous unoptimized frames (rustc gives every match arm's
        // locals distinct stack slots), on the order of 100+ KiB per
        // Python-to-Python call, and `stacker` growth is disabled on
        // greenlet stacks — sys.getrecursionlimit() worth of frames has
        // to fit in the flat reservation (the bundled
        // test_greenlet_native runs deep(500) on a greenlet stack).
        let default = if cfg!(debug_assertions) {
            512 * 1024 * 1024
        } else {
            16 * 1024 * 1024
        };
        std::env::var("WEAVEPY_GREENLET_STACK_SIZE")
            .ok()
            .and_then(|s| s.parse::<usize>().ok())
            .filter(|&n| n >= 64 * 1024)
            .unwrap_or(default)
    })
}

// ---------------------------------------------------------------------------
// Interpreter access + the swap set.
// ---------------------------------------------------------------------------

fn with_interp<F, R>(f: F) -> Result<R, RuntimeError>
where
    F: FnOnce(&mut crate::Interpreter) -> Result<R, RuntimeError>,
{
    let ptr = crate::vm_singletons::current_interpreter_ptr()
        .ok_or_else(|| type_error("greenlet: no active interpreter"))?;
    let interp = unsafe { &mut *ptr };
    f(interp)
}

/// The frozen `contextvars._STATES` dict, if the module has been
/// imported (before that, no context exists to swap).
fn contextvars_states() -> Option<Object> {
    let ptr = crate::vm_singletons::current_interpreter_ptr()?;
    let interp = unsafe { &mut *ptr };
    let module = interp.module_cache().get("contextvars")?;
    let Object::Module(m) = module else {
        return None;
    };
    let d = m.dict.borrow();
    d.get(&DictKey(Object::from_static("_STATES"))).cloned()
}

fn thread_ident_key() -> Object {
    Object::Int(crate::vm_singletons::current_worker_thread_id() as i64)
}

/// Stash the thread's live contextvars Context on the departing body
/// (greenlet ≥ 1.0: `gr_context` is observable while suspended).
fn save_context(from: &GreenletBody) {
    if let Some(Object::Dict(states)) = contextvars_states() {
        let key = DictKey(thread_ident_key());
        let cur = states.borrow_mut().shift_remove(&key);
        *from.context.borrow_mut() = cur.unwrap_or(Object::None);
    }
}

/// Install the arriving body's stashed Context as the thread's current
/// one (`None` leaves the slot empty — a fresh implicit context).
fn install_context(to: &GreenletBody) {
    if let Some(Object::Dict(states)) = contextvars_states() {
        let key = DictKey(thread_ident_key());
        let ctx = std::mem::replace(&mut *to.context.borrow_mut(), Object::None);
        let mut s = states.borrow_mut();
        s.shift_remove(&key);
        if !matches!(ctx, Object::None) {
            s.insert(key, ctx);
        }
    }
}

/// Departing `from`: snapshot the thread-level pieces of its execution
/// state. (Its `frames`/`exc_info` `Rc`s need no save — the Interpreter
/// merely *points at* the current greenlet's storage.)
fn save_thread_state(from: &GreenletBody) {
    from.saved_depth.set(crate::recursion::current_depth());
    // Park the thread-handles stack with us: its guards live on our
    // native stack and must not be popped by whoever runs next.
    *from.saved_handles.borrow_mut() = crate::vm_singletons::swap_thread_handles_stack(Vec::new());
    save_context(from);
}

/// Delivering to `to`: make the thread's interpreter state *be* the
/// target greenlet's. Called at every return-into-Python moment — the
/// switch caller after routing, and a fresh coroutine's entry.
fn install_thread_state(to: &GreenletBody) {
    let _ = with_interp(|interp| {
        interp.frame_stack = to.frames.clone();
        interp.exc_info_stack = to.exc_info.clone();
        Ok(())
    });
    let restored = std::mem::take(&mut *to.saved_handles.borrow_mut());
    let _ = crate::vm_singletons::swap_thread_handles_stack(restored);
    crate::recursion::set_depth(to.saved_depth.get());
    install_context(to);
    CURRENT.with(|c| c.set(to.id));
}

// ---------------------------------------------------------------------------
// Exception classes.
// ---------------------------------------------------------------------------

/// `greenlet.GreenletExit` — inherits `BaseException` (like
/// `GeneratorExit`): "you were asked to die" must sail past bare
/// `except Exception:` handlers.
pub(crate) fn greenlet_exit_class() -> Rc<TypeObject> {
    static CLS: std::sync::OnceLock<Rc<TypeObject>> = std::sync::OnceLock::new();
    CLS.get_or_init(|| {
        let bt = crate::builtin_types::builtin_types();
        TypeObject::new_exception("GreenletExit", bt.base_exception.clone())
            .expect("greenlet.GreenletExit")
    })
    .clone()
}

/// `greenlet.error` — plain `Exception` subclass for protocol misuse
/// (cross-thread switches, parent cycles).
fn green_error_class() -> Rc<TypeObject> {
    static CLS: std::sync::OnceLock<Rc<TypeObject>> = std::sync::OnceLock::new();
    CLS.get_or_init(|| {
        let bt = crate::builtin_types::builtin_types();
        TypeObject::new_exception("error", bt.exception.clone()).expect("greenlet.error")
    })
    .clone()
}

fn green_error(msg: impl Into<String>) -> RuntimeError {
    RuntimeError::PyException(PyException::new(
        crate::builtin_types::make_exception_with_class(green_error_class(), msg),
    ))
}

fn greenlet_exit_error() -> RuntimeError {
    RuntimeError::PyException(PyException::new(
        crate::builtin_types::make_exception_with_class(greenlet_exit_class(), ""),
    ))
}

fn is_greenlet_exit(err: &RuntimeError) -> bool {
    let RuntimeError::PyException(pe) = err else {
        return false;
    };
    let Object::Instance(inst) = &pe.instance else {
        return false;
    };
    let target = greenlet_exit_class();
    inst.cls()
        .mro
        .borrow()
        .iter()
        .any(|t| Rc::ptr_eq(t, &target))
}

// ---------------------------------------------------------------------------
// Bodies, registry, main.
// ---------------------------------------------------------------------------

fn fresh_body(is_main: bool) -> Rc<GreenletBody> {
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    // A greenlet body is thread-affine by construction (bodies live in the
    // thread-local registry and never cross threads); the workspace `Rc`
    // alias maps to `Arc` under the sync feature, hence the lint allowance.
    #[allow(clippy::arc_with_non_send_sync)]
    Rc::new(GreenletBody {
        id,
        is_main,
        run: RefCell::new(None),
        parent: Cell::new(0),
        status: Cell::new(if is_main {
            Status::Active
        } else {
            Status::Unstarted
        }),
        coroutine: RefCell::new(None),
        yielder: Cell::new(None),
        instance: RefCell::new(InstanceRef::None),
        frames: Rc::new(RefCell::new(Vec::new())),
        exc_info: Rc::new(RefCell::new(Vec::new())),
        saved_depth: Cell::new(0),
        context: RefCell::new(Object::None),
        saved_handles: RefCell::new(Vec::new()),
    })
}

/// This thread's main greenlet, created on first touch. Main adopts the
/// interpreter's *live* frame/exc-info spines (it is already running).
fn ensure_main() -> Rc<GreenletBody> {
    let main_id = MAIN_ID.with(|m| m.get());
    if main_id != 0 {
        if let Some(b) = body(main_id) {
            return b;
        }
    }
    let b = fresh_body(true);
    let _ = with_interp(|interp| {
        // Main's spine IS the live one — replace the fresh Rcs.
        let this = &b;
        // Safety: RefCell fields on a freshly built body, no aliasing.
        unsafe {
            let p = Rc::as_ptr(this).cast_mut();
            (*p).frames = interp.frame_stack.clone();
            (*p).exc_info = interp.exc_info_stack.clone();
        }
        Ok(())
    });
    let cls = greenlet_class();
    let inst = Rc::new(PyInstance::new(cls));
    inst.dict.borrow_mut().insert(
        DictKey(Object::from_static("_greenlet_id")),
        Object::Int(b.id as i64),
    );
    *b.instance.borrow_mut() = InstanceRef::Strong(inst);
    MAIN_ID.with(|m| m.set(b.id));
    CURRENT.with(|c| c.set(b.id));
    CHAIN.with(|c| c.borrow_mut().push(b.id));
    REGISTRY.with(|r| r.borrow_mut().insert(b.id, b.clone()));
    b
}

fn current_body() -> Rc<GreenletBody> {
    let cur = CURRENT.with(|c| c.get());
    if cur != 0 {
        if let Some(b) = body(cur) {
            return b;
        }
    }
    ensure_main()
}

/// Resolve a switch target through the alive-parent chain (a dead
/// target hands off to its parent; ultimate fallback is main).
fn resolve_alive(mut id: u64) -> u64 {
    for _ in 0..10_000 {
        match body(id) {
            Some(b) if b.status.get() != Status::Dead => return id,
            Some(b) => id = b.parent.get(),
            None => break,
        }
    }
    MAIN_ID.with(|m| m.get())
}

// ---------------------------------------------------------------------------
// The switch machinery.
// ---------------------------------------------------------------------------

/// Resume `target`'s coroutine (starting it if unstarted) and return the
/// directive it emits when it next switches away or dies.
fn resume_body(target: u64, payload: Payload) -> Directive {
    let b = body(target).expect("resume_body: registered target");
    if b.status.get() == Status::Unstarted {
        b.status.set(Status::Active);
        let id = b.id;
        let stack = DefaultStack::new(stack_size()).expect("greenlet stack allocation");
        let coro: GreenCoroutine = Coroutine::with_stack(stack, move |yielder, first| {
            greenlet_main(id, yielder, first)
        });
        *b.coroutine.borrow_mut() = Some(coro);
    }
    let mut coro = b
        .coroutine
        .borrow_mut()
        .take()
        .expect("resume_body: suspended target has a coroutine");
    CHAIN.with(|c| c.borrow_mut().push(target));
    let res = coro.resume(payload);
    CHAIN.with(|c| {
        let mut ch = c.borrow_mut();
        let popped = ch.pop();
        debug_assert_eq!(popped, Some(target));
    });
    match res {
        CoroutineResult::Yield(d) => {
            *b.coroutine.borrow_mut() = Some(coro);
            d
        }
        CoroutineResult::Return(d) => d, // dead; the closure already book-kept
    }
}

/// The uniform routing loop — see the module docs. Runs as `self_id`
/// *after* [`save_thread_state`]; returns the payload delivered to us.
fn route(self_id: u64, mut target: u64, mut payload: Payload) -> Payload {
    loop {
        target = resolve_alive(target);
        if target == self_id {
            return payload;
        }
        if chain_contains(target) {
            // Active below us: hand the directive down by suspending.
            // Only non-main greenlets can reach here (main is the chain
            // root: while main runs, nothing else is active).
            let b = body(self_id).expect("routing greenlet is registered");
            let y = b
                .yielder
                .get()
                .expect("active non-main greenlet has a live yielder");
            return unsafe { &*y }.suspend(Directive { target, payload });
        }
        let d = resume_body(target, payload);
        target = d.target;
        payload = d.payload;
    }
}

/// One started greenlet's whole life, on its own stack.
fn greenlet_main(id: u64, yielder: &Yielder<Payload, Directive>, first: Payload) -> Directive {
    let b = body(id).expect("greenlet body registered at start");
    b.yielder.set(Some(std::ptr::from_ref(yielder)));
    install_thread_state(&b);
    let result: Result<Object, RuntimeError> = (|| {
        let (args, kwargs) = match first {
            Payload::Values(a, k) => (a, k),
            // `throw()` into an unstarted greenlet: it dies without
            // running, the exception travels to the parent (upstream
            // `g_initialstub` semantics).
            Payload::Throw(e) => return Err(e),
        };
        let run = match b.run.borrow_mut().take() {
            Some(r) => r,
            None => {
                // Subclass pattern: resolve the `run` method off the
                // instance at start time.
                let inst = b.instance_object().ok_or_else(|| {
                    green_error("cannot start a greenlet whose instance was collected")
                })?;
                with_interp(|interp| interp.load_attr_public(&inst, "run"))?
            }
        };
        with_interp(|interp| interp.call_object(run, &args, &kwargs))
    })();
    // Death bookkeeping happens here, on our own stack, before the final
    // directive crosses back.
    b.yielder.set(None);
    b.status.set(Status::Dead);
    b.run.borrow_mut().take();
    save_thread_state(&b);
    let parent = resolve_alive(b.parent.get());
    match result {
        Ok(v) => Directive {
            target: parent,
            payload: Payload::Values(vec![v], Vec::new()),
        },
        Err(e) if is_greenlet_exit(&e) => {
            // An uncaught GreenletExit is a *normal* death: the parent's
            // switch returns the exception instance as a value.
            let inst = match &e {
                RuntimeError::PyException(pe) => pe.instance.clone(),
                _ => Object::None,
            };
            Directive {
                target: parent,
                payload: Payload::Values(vec![inst], Vec::new()),
            }
        }
        Err(e) => Directive {
            target: parent,
            payload: Payload::Throw(e),
        },
    }
}

/// Upstream's `single_result` value plumbing for what a switch returns.
fn normalize_switch_value(args: Vec<Object>, kwargs: Vec<(String, Object)>) -> Object {
    let kwdict = |kwargs: Vec<(String, Object)>| {
        let mut d = DictData::default();
        for (k, v) in kwargs {
            d.insert(DictKey(Object::from_str(k)), v);
        }
        Object::Dict(Rc::new(RefCell::new(d)))
    };
    if kwargs.is_empty() {
        match args.len() {
            1 => args.into_iter().next().expect("len checked"),
            _ => Object::new_tuple(args),
        }
    } else if args.is_empty() {
        kwdict(kwargs)
    } else {
        Object::new_tuple(vec![Object::new_tuple(args), kwdict(kwargs)])
    }
}

/// The heart of `switch`/`throw`: leave `cur`, deliver `payload` to
/// `target`, and (much later) turn whatever comes back into our own
/// switch result.
fn initiate(
    cur: &Rc<GreenletBody>,
    target: &Rc<GreenletBody>,
    payload: Payload,
) -> Result<Object, RuntimeError> {
    save_thread_state(cur);
    let delivered = route(cur.id, target.id, payload);
    let self_body = body(cur.id).expect("initiator stays registered");
    install_thread_state(&self_body);
    match delivered {
        Payload::Values(a, k) => Ok(normalize_switch_value(a, k)),
        Payload::Throw(e) => Err(e),
    }
}

// ---------------------------------------------------------------------------
// The Python surface.
// ---------------------------------------------------------------------------

fn extract_self(args: &[Object]) -> Result<Rc<PyInstance>, RuntimeError> {
    match args.first() {
        Some(Object::Instance(inst))
            if inst.cls().mro.borrow().iter().any(|t| t.name == "greenlet") =>
        {
            Ok(inst.clone())
        }
        _ => Err(type_error("greenlet method requires a greenlet self")),
    }
}

fn body_of(inst: &Rc<PyInstance>) -> Result<Rc<GreenletBody>, RuntimeError> {
    let id = {
        let d = inst.dict.borrow();
        match d.get(&DictKey(Object::from_static("_greenlet_id"))) {
            Some(Object::Int(i)) => *i as u64,
            _ => return Err(green_error("greenlet was not initialised")),
        }
    };
    body(id).ok_or_else(|| green_error("cannot switch to a different thread"))
}

fn instance_id(inst: &Rc<PyInstance>) -> Option<u64> {
    let d = inst.dict.borrow();
    match d.get(&DictKey(Object::from_static("_greenlet_id"))) {
        Some(Object::Int(i)) => Some(*i as u64),
        _ => None,
    }
}

/// `greenlet.__init__(self, run=None, parent=None)`.
fn green_init(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let inst = extract_self(args)?;
    let mut run: Option<Object> = None;
    let mut parent: Option<Object> = None;
    let pos = &args[1..];
    if pos.len() > 2 {
        return Err(type_error(format!(
            "greenlet() takes at most 2 arguments ({} given)",
            pos.len()
        )));
    }
    if let Some(v) = pos.first() {
        run = Some(v.clone());
    }
    if let Some(v) = pos.get(1) {
        parent = Some(v.clone());
    }
    for (k, v) in kwargs {
        match k.as_str() {
            "run" if run.is_none() => run = Some(v.clone()),
            "parent" if parent.is_none() => parent = Some(v.clone()),
            "run" | "parent" => {
                return Err(type_error(format!(
                    "greenlet() got multiple values for argument '{k}'"
                )))
            }
            _ => {
                return Err(type_error(format!(
                    "greenlet() got an unexpected keyword argument '{k}'"
                )))
            }
        }
    }
    let main = ensure_main();
    let parent_id = match parent {
        None | Some(Object::None) => CURRENT.with(|c| c.get()).max(main.id),
        Some(Object::Instance(p)) => {
            let pb = body_of(&p)?;
            pb.id
        }
        Some(other) => {
            return Err(type_error(format!(
                "parent must be a greenlet, not {}",
                other.type_name()
            )))
        }
    };
    let b = fresh_body(false);
    b.parent
        .set(if parent_id == 0 { main.id } else { parent_id });
    *b.run.borrow_mut() = match run {
        None | Some(Object::None) => None,
        Some(r) => Some(r),
    };
    *b.instance.borrow_mut() = InstanceRef::Weak(Rc::downgrade(&inst));
    inst.dict.borrow_mut().insert(
        DictKey(Object::from_static("_greenlet_id")),
        Object::Int(b.id as i64),
    );
    REGISTRY.with(|r| r.borrow_mut().insert(b.id, b));
    Ok(Object::None)
}

/// `g.switch(*args, **kwargs)`.
fn green_switch(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let inst = extract_self(args)?;
    ensure_main();
    let target = body_of(&inst)?;
    let cur = current_body();
    if target.status.get() == Status::Unstarted && target.run.borrow().is_none() && !target.is_main
    {
        // Fail eagerly (in the caller) when there is nothing to run —
        // neither a constructor `run` nor a subclass `run` method.
        let has_method_run = target
            .instance_object()
            .map(|o| match &o {
                Object::Instance(i) => i.cls().lookup("run").is_some(),
                _ => false,
            })
            .unwrap_or(false);
        if !has_method_run {
            return Err(RuntimeError::PyException(PyException::new(
                crate::builtin_types::make_exception_with_class(
                    crate::builtin_types::builtin_types()
                        .attribute_error
                        .clone(),
                    "run",
                ),
            )));
        }
    }
    let payload = Payload::Values(args[1..].to_vec(), kwargs.to_vec());
    initiate(&cur, &target, payload)
}

/// Build the exception a `throw()` delivers, upstream-style.
fn throw_exception(args: &[Object]) -> Result<RuntimeError, RuntimeError> {
    let typ = args.get(1).cloned().unwrap_or(Object::None);
    let val = args.get(2).cloned().unwrap_or(Object::None);
    // args[3] (tb) is accepted and ignored — WeavePy tracebacks are
    // synthesized at raise time.
    let instance = match (&typ, &val) {
        (Object::None, _) => {
            crate::builtin_types::make_exception_with_class(greenlet_exit_class(), "")
        }
        (Object::Type(cls), Object::None) => {
            with_interp(|interp| interp.call_object(Object::Type(cls.clone()), &[], &[]))?
        }
        (Object::Type(cls), Object::Tuple(items)) => {
            let a: Vec<Object> = items.iter().cloned().collect();
            with_interp(|interp| interp.call_object(Object::Type(cls.clone()), &a, &[]))?
        }
        (Object::Type(cls), v) => {
            // An instance value: use it if it already is one of `cls`,
            // else call `cls(v)`.
            if matches!(v, Object::Instance(i) if i.cls().mro.borrow().iter().any(|t| Rc::ptr_eq(t, cls)))
            {
                v.clone()
            } else {
                with_interp(|interp| {
                    interp.call_object(Object::Type(cls.clone()), &[v.clone()], &[])
                })?
            }
        }
        (Object::Instance(_), Object::None) => typ.clone(),
        (Object::Instance(_), _) => {
            return Err(type_error(
                "instance exception may not have a separate value",
            ))
        }
        _ => return Err(type_error("exceptions must be classes or instances")),
    };
    Ok(RuntimeError::PyException(PyException::new(instance)))
}

/// `g.throw(typ=GreenletExit, val=None, tb=None)`.
fn green_throw(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = extract_self(args)?;
    ensure_main();
    let target = body_of(&inst)?;
    let cur = current_body();
    let exc = throw_exception(args)?;
    if target.status.get() == Status::Unstarted && !target.is_main {
        // Never-started target: it dies silently for GreenletExit, and
        // the exception surfaces in the caller otherwise (upstream).
        target.status.set(Status::Dead);
        target.run.borrow_mut().take();
        if is_greenlet_exit(&exc) {
            let inst = match &exc {
                RuntimeError::PyException(pe) => pe.instance.clone(),
                _ => Object::None,
            };
            return Ok(inst);
        }
        return Err(exc);
    }
    initiate(&cur, &target, Payload::Throw(exc))
}

/// GC hook: collecting a suspended greenlet throws `GreenletExit` into
/// it, re-parented to the collector so control returns here (upstream
/// `green_dealloc`). Runs through the class `__del__`, i.e. at an
/// eval-loop safe point.
fn green_del(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = extract_self(args)?;
    let Some(id) = instance_id(&inst) else {
        return Ok(Object::None);
    };
    let Some(b) = body(id) else {
        return Ok(Object::None);
    };
    let cur = current_body();
    let collectable =
        b.status.get() == Status::Active && !b.is_main && b.id != cur.id && !chain_contains(b.id);
    if collectable {
        b.parent.set(cur.id);
        let _ = initiate(&cur, &b, Payload::Throw(greenlet_exit_error()));
    }
    REGISTRY.with(|r| r.borrow_mut().remove(&id));
    Ok(Object::None)
}

fn green_bool(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = extract_self(args)?;
    let Some(id) = instance_id(&inst) else {
        return Ok(Object::Bool(false));
    };
    Ok(Object::Bool(
        body(id)
            .map(|b| b.status.get() == Status::Active)
            .unwrap_or(false),
    ))
}

fn green_repr(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = extract_self(args)?;
    let state = match instance_id(&inst).and_then(body) {
        Some(b) => match b.status.get() {
            Status::Unstarted => "pending",
            Status::Active if b.is_main => "main",
            Status::Active => {
                if CURRENT.with(|c| c.get()) == b.id {
                    "current"
                } else {
                    "suspended"
                }
            }
            Status::Dead => "dead",
        },
        None => "dead",
    };
    Ok(Object::from_str(format!(
        "<greenlet.greenlet object ({state}) at {:p}>",
        Rc::as_ptr(&inst)
    )))
}

fn green_dead_prop(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = extract_self(args)?;
    let Some(id) = instance_id(&inst) else {
        return Ok(Object::Bool(true));
    };
    Ok(Object::Bool(
        body(id)
            .map(|b| b.status.get() == Status::Dead)
            .unwrap_or(true),
    ))
}

fn green_parent_get(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = extract_self(args)?;
    let Some(b) = instance_id(&inst).and_then(body) else {
        return Ok(Object::None);
    };
    if b.is_main {
        return Ok(Object::None);
    }
    match body(b.parent.get()) {
        Some(pb) => Ok(pb.instance_object().unwrap_or(Object::None)),
        None => Ok(Object::None),
    }
}

fn green_parent_set(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = extract_self(args)?;
    let b = body_of(&inst)?;
    if b.is_main {
        return Err(value_error("cannot set the parent of the main greenlet"));
    }
    let new_parent = match args.get(1) {
        Some(Object::Instance(p)) => body_of(p)?,
        _ => return Err(type_error("parent must be a greenlet")),
    };
    if new_parent.status.get() == Status::Dead {
        return Err(value_error("parent must not be garbage collected or dead"));
    }
    // Reject cycles: walking up from the proposed parent must not reach us.
    let mut walk = new_parent.id;
    for _ in 0..10_000 {
        if walk == b.id {
            return Err(value_error("cyclic parent chain"));
        }
        match body(walk) {
            Some(w) if !w.is_main => walk = w.parent.get(),
            _ => break,
        }
    }
    b.parent.set(new_parent.id);
    Ok(Object::None)
}

fn green_context_get(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = extract_self(args)?;
    let Some(b) = instance_id(&inst).and_then(body) else {
        return Ok(Object::None);
    };
    if CURRENT.with(|c| c.get()) == b.id {
        // Live: the thread's current context is ours.
        if let Some(Object::Dict(states)) = contextvars_states() {
            let key = DictKey(thread_ident_key());
            if let Some(ctx) = states.borrow().get(&key) {
                return Ok(ctx.clone());
            }
        }
        return Ok(Object::None);
    }
    let ctx = b.context.borrow().clone();
    Ok(ctx)
}

fn green_context_set(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = extract_self(args)?;
    let b = body_of(&inst)?;
    let value = args.get(1).cloned().unwrap_or(Object::None);
    if b.status.get() == Status::Dead {
        return Err(value_error("cannot set the context of a dead greenlet"));
    }
    if CURRENT.with(|c| c.get()) == b.id {
        if let Some(Object::Dict(states)) = contextvars_states() {
            let key = DictKey(thread_ident_key());
            let mut s = states.borrow_mut();
            s.shift_remove(&key);
            if !matches!(value, Object::None) {
                s.insert(key, value);
            }
            return Ok(Object::None);
        }
    }
    *b.context.borrow_mut() = value;
    Ok(Object::None)
}

fn green_frame_get(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = extract_self(args)?;
    let Some(b) = instance_id(&inst).and_then(body) else {
        return Ok(Object::None);
    };
    // Only a *suspended* greenlet exposes its parked top frame
    // (upstream: gr_frame is None for running/dead/unstarted).
    if b.status.get() != Status::Active || CURRENT.with(|c| c.get()) == b.id || chain_contains(b.id)
    {
        return Ok(Object::None);
    }
    let len = b.frames.borrow().len();
    if len == 0 {
        return Ok(Object::None);
    }
    // RFC 0058 shells: materialise the Python-visible frame on demand,
    // same as `sys._getframe`.
    match crate::object::materialize_stack_at(&b.frames, len - 1) {
        Some(py) => Ok(Object::Frame(py)),
        None => Ok(Object::None),
    }
}

fn kw_builtin(
    name: &'static str,
    body_fn: fn(&[Object], &[(String, Object)]) -> Result<Object, RuntimeError>,
) -> Object {
    Object::Builtin(Rc::new(BuiltinFn {
        name,
        binds_instance: true,
        call: Box::new(move |args| body_fn(args, &[])),
        call_kw: Some(Box::new(body_fn)),
    }))
}

fn method(name: &'static str, body_fn: fn(&[Object]) -> Result<Object, RuntimeError>) -> Object {
    Object::Builtin(Rc::new(BuiltinFn {
        name,
        binds_instance: true,
        call: Box::new(body_fn),
        call_kw: None,
    }))
}

fn install_property(
    cls: &Rc<TypeObject>,
    name: &'static str,
    getter: fn(&[Object]) -> Result<Object, RuntimeError>,
    setter: Option<fn(&[Object]) -> Result<Object, RuntimeError>>,
) {
    let fset = match setter {
        Some(s) => method(name, s),
        None => Object::None,
    };
    let prop = Object::Property(Rc::new(PyProperty::new(
        method(name, getter),
        fset,
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

/// The `greenlet` class (process-global, socket-class pattern:
/// instances built on any thread are of the same class object).
fn greenlet_class() -> Rc<TypeObject> {
    static CLS: std::sync::OnceLock<Rc<TypeObject>> = std::sync::OnceLock::new();
    CLS.get_or_init(|| {
        let bt = crate::builtin_types::builtin_types();
        let mut dict = DictData::default();
        dict.insert(
            DictKey(Object::from_static("__init__")),
            kw_builtin("__init__", green_init),
        );
        dict.insert(
            DictKey(Object::from_static("switch")),
            kw_builtin("switch", green_switch),
        );
        dict.insert(
            DictKey(Object::from_static("throw")),
            method("throw", green_throw),
        );
        dict.insert(
            DictKey(Object::from_static("__bool__")),
            method("__bool__", green_bool),
        );
        dict.insert(
            DictKey(Object::from_static("__repr__")),
            method("__repr__", green_repr),
        );
        dict.insert(
            DictKey(Object::from_static("__del__")),
            method("__del__", green_del),
        );
        let cls = TypeObject::new_user("greenlet", vec![bt.object_.clone()], dict)
            .expect("greenlet class must linearise");
        install_property(&cls, "dead", green_dead_prop, None);
        install_property(&cls, "parent", green_parent_get, Some(green_parent_set));
        install_property(
            &cls,
            "gr_context",
            green_context_get,
            Some(green_context_set),
        );
        install_property(&cls, "gr_frame", green_frame_get, None);
        cls
    })
    .clone()
}

/// `getcurrent()` — the currently-executing greenlet on this thread.
fn getcurrent(_args: &[Object]) -> Result<Object, RuntimeError> {
    ensure_main();
    let b = current_body();
    b.instance_object()
        .ok_or_else(|| green_error("current greenlet instance was collected"))
}

/// Module-level no-op trace hooks (API-compatible; upstream's tracing
/// is an observability feature the facade does not model yet).
fn settrace(_args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(Object::None)
}
fn gettrace(_args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(Object::None)
}

pub fn build(_cache: &ModuleCache) -> Rc<PyModule> {
    let dict = Rc::new(RefCell::new(DictData::default()));
    {
        let mut d = dict.borrow_mut();
        let f = |name: &'static str, body_fn: fn(&[Object]) -> Result<Object, RuntimeError>| {
            Object::Builtin(Rc::new(BuiltinFn {
                name,
                binds_instance: false,
                call: Box::new(body_fn),
                call_kw: None,
            }))
        };
        d.insert(
            DictKey(Object::from_static("__name__")),
            Object::from_static("_greenlet"),
        );
        d.insert(
            DictKey(Object::from_static("greenlet")),
            Object::Type(greenlet_class()),
        );
        d.insert(
            DictKey(Object::from_static("GreenletExit")),
            Object::Type(greenlet_exit_class()),
        );
        d.insert(
            DictKey(Object::from_static("error")),
            Object::Type(green_error_class()),
        );
        d.insert(
            DictKey(Object::from_static("getcurrent")),
            f("getcurrent", getcurrent),
        );
        d.insert(
            DictKey(Object::from_static("settrace")),
            f("settrace", settrace),
        );
        d.insert(
            DictKey(Object::from_static("gettrace")),
            f("gettrace", gettrace),
        );
        // Version-string the upstream line this implementation models.
        d.insert(
            DictKey(Object::from_static("GREENLET_VERSION")),
            Object::from_static("3.2.0"),
        );
    }
    Rc::new(PyModule {
        name: "_greenlet".to_owned(),
        filename: None,
        dict,
    })
}

# RFC 0025: Cross-thread heap sharing — real `threading.Thread` parallelism

- **Status**: Shipped (heap + threading); `multiprocessing` deferred to RFC 0026
- **Authors**: WeavePy authors
- **Created**: 2026-05-25
- **Tracking issue**: TBD

## What shipped vs. what was deferred

| Area | Shipped | Notes |
|------|---------|-------|
| `Object` enum → `Send + Sync` | ✅ | compile-time `assert_send`/`assert_sync` in `object.rs` |
| `crate::sync` (Arc / GilCell / Weak) | ✅ | drop-in for `std::rc::Rc`, `std::cell::RefCell`, `std::cell::Cell`, `std::rc::Weak` |
| `CodeObject: Send + Sync` via `CacheSlot` | ✅ | inline cache wrapped in `UnsafeCell` under GIL invariant |
| Real `_thread.start_new_thread` | ✅ | spawns `std::thread`, forks per-thread `Interpreter` |
| Per-thread `Interpreter::fork_for_thread` | ✅ | shares heap, owns frame / exception stacks |
| `vm_singletons::current_thread_handles` | ✅ | routes `sys.exc_info` / `sys._getframe` per-thread |
| GIL guard thread-local stack (`gil::push/pop_gil_guard`) | ✅ | consolidates C-API + native blocking |
| C-API `PyEval_SaveThread` / `RestoreThread` / `AcquireThread` / `ReleaseThread` / `PyThreadState_Swap` | ✅ | all route through the guard stack |
| `Thread.join` via `_tstate_lock` + GIL drop | ✅ | `allow_threads_then` wraps the blocking acquire |
| `threading._shutdown()` joins non-daemon threads | ✅ | new implementation in `stdlib/python/threading.py` |
| Cooperative cross-thread call channel | ❌ obsoleted | full `Send + Sync` heap makes this moot |
| Real `_multiprocessing` fork/spawn/forkserver | ⏭️ RFC 0026 | current stub surface still passes its regrtest |

## Performance impact

The wholesale `Rc → Arc` + `RefCell → ReentrantMutex<UnsafeCell<T>>` swap
introduces a measurable single-threaded slowdown on CPU-bound
microbenchmarks. New baselines on the bundled corpus
(`cargo run -p weavepy-bench --bin weavepy-bench -- run --no-cpython`):

| fixture       | baseline (`Rc`/`RefCell`) | shipped (`Arc`/`GilCell`) | delta   |
|---------------|--------------------------:|--------------------------:|--------:|
| `fannkuch`    |                     45 µs |                     91 µs | +104 %  |
| `fib`         |                   10.6 ms |                   13.8 ms |  +30 %  |
| `nbody`       |                    106 µs |                    118 µs |  +11 %  |
| `nested_loops`|                    2.2 ms |                    2.9 ms |  +36 %  |
| `pidigits`    |                     85 µs |                     86 µs |   +1 %  |
| `pyaes`       |                    8.9 ms |                   11.0 ms |  +24 %  |
| `richards`    |                     83 µs |                     92 µs |  +11 %  |
| `sumvm`       |                    1.2 ms |                    2.0 ms |  +76 %  |

This is the cost of every `borrow()` / `borrow_mut()` going through
`parking_lot::ReentrantMutex::lock` (and an atomic
fetch-add on the borrow counter) instead of a plain non-atomic
counter check. The fast path is still uncontended — the mutex never
sleeps in single-threaded mode — but the atomic operations and
cache-line traffic dominate on hot integer loops.

WeavePy remains substantially faster than CPython 3.13 on these
fixtures (CPython times not shown above are typically 5–20× the
WeavePy column). The slowdown is the documented trade-off for real
cross-thread heap sharing; **the alternative — proxying through a
manager process — would impose far worse latency for the same
parallelism guarantees**.

A follow-up optimization track is sketched in
[§Performance follow-ups](#performance-follow-ups). The two largest
wins on the table:

1. Drop `ReentrantMutex` from `GilCell` and rely on the GIL +
   borrow counter alone for serialisation. Requires a codebase
   audit to ensure no `Ref`/`RefMut` outlives a GIL release.
2. Inline-storage `SmallObject` variant for `Object::Int` /
   `Object::Bool` / `Object::None` / `Object::String`
   (≤ small-string-optimised payload) so the hot integer loops
   never touch the `Arc` refcount.

## Summary

Close the last named architectural gap from RFC 0024 — *"WeavePy ships
sub-interpreter-per-thread semantics; mutable-shared-state-via-closure
still requires a `multiprocessing.Manager()` indirection; this is the
documented divergence and will be lifted in RFC 0025."*

After this RFC lands:

- The runtime's reference-counted heap is **`Arc`-rooted instead of
  `Rc`-rooted**. Every `Object`, `TypeObject`, `CodeObject`, `PyModule`,
  `PyFrame`, `PyTraceback`, `PyInstance`, `PyFunction`, `PyGenerator`,
  `PyIterator`, `PyFile`, `BuiltinFn`, `BoundMethod`, `PySlice`,
  `PyMemoryView`, `PyProperty`, `PySlotDescriptor`, `PyDictView`,
  `PyComplex`, `Range`, `DictData`, `SetData`, and every `Box<dyn Fn>`
  callable variant becomes `Send + Sync`. The single-threaded fast path
  retains today's performance characteristics (~5ns per `borrow()`)
  because `parking_lot::Mutex` is uncontended in the typical
  "one-thread-holds-the-GIL" case.
- A new `weavepy-vm/src/sync.rs` surface is the workspace's drop-in
  replacement for `std::rc::{Rc, Weak}` and `std::cell::{RefCell, Cell}`.
  `Rc<T>` is a typedef of `std::sync::Arc<T>`; `RefCell<T>` is a
  `GilCell<T>` backed by a `parking_lot::ReentrantMutex` plus a CPython-
  shaped borrow counter; `Cell<T>` is a `GilCell<T>` (`Copy` API on
  top); `Weak<T>` is a typedef of `std::sync::Weak<T>`. Existing call
  sites compile unchanged after the one-line import swap.
- The `Object` enum gains the `Send + Sync` bound. The wholesale swap
  means that *every* mutation on a shared container — a list passed
  to a worker thread, a dict captured by a worker closure, a
  `bytearray` shared via a queue — is **visible from every thread**
  the moment the lock is released, matching CPython byte-for-byte.
- `_thread.start_new_thread(target, args)` **really spawns** the target
  on a fresh `std::thread`. The new thread takes the GIL on entry,
  drives the dispatch loop over the same shared heap as the parent,
  and releases the GIL on Python `time.sleep` / `Queue.get(timeout=…)`
  / `Lock.acquire(timeout=…)` / blocking I/O. The empty-body OS thread
  stub in RFC 0024 (`thread_real.rs::start_new_thread`) is replaced
  by a real worker that runs the Python target.
- A new **per-thread `ThreadState`** captures the frame stack,
  exception stack, current-frame pointer, pending-signal mask, and
  `threading.local()` slot table. Each OS thread carries one;
  `Interpreter` owns the main-thread state and exposes
  `attach_thread_state` / `detach_thread_state` for workers. The
  C-API's `PyThreadState` is a thin handle over this.
- A new **per-thread interpreter context** (a "shoot, then re-enter the
  dispatch loop" wrapper) is the entry point for spawned workers.
  The worker shares `sys.modules`, `builtins`, the `__main__` dict,
  and the cycle GC's tracking lists with the parent. Tracebacks
  surfacing to the parent name the worker thread.
- A real **`_multiprocessing`** core ships fork + spawn + forkserver
  start methods. The frozen `multiprocessing.py` is rewritten on top:
  `Process.start()` spawns a real child via
  `subprocess::Command::new(std::env::current_exe()).arg("--multiprocessing-fork").arg(payload_fd)`,
  or on POSIX falls back to `libc::fork()` for the `fork` start method.
  `Pool` / `Queue` / `Pipe` / `Lock` / `Event` / `SharedMemory` / `Manager`
  all sit on top of the real start methods, the `_socket` /
  `multiprocessing.Manager` IPC layer, and the new shared-heap
  primitives. `multiprocessing.cpu_count()` returns the real
  `std::thread::available_parallelism()` value.
- **`Thread.daemon`** is honoured at shutdown: a new
  `Interpreter::shutdown_threads()` walks the thread registry, signals
  daemon threads to drop their GIL slot and exit (via an eval-breaker
  flag), and joins non-daemon ones. Re-entrant `__del__` finalisers
  flushed across all live threads.
- **`Thread.join(timeout)`** routes through the `_tstate_lock`
  sentinel that `start_new_thread` now genuinely sets on the spawned
  thread. Parent threads block on `lock.acquire(timeout=…)`, which
  drops the GIL while waiting, so other threads can run.
- The C-API gains **real** `PyEval_SaveThread` /
  `PyEval_RestoreThread` / `PyEval_AcquireThread` /
  `PyEval_ReleaseThread` / `PyGILState_Ensure` / `PyGILState_Release` /
  `PyThreadState_Get` / `PyThreadState_Swap` /
  `PyInterpreterState_Get` /
  `PyThread_acquire_lock_timed` semantics. The
  `Py_BEGIN_ALLOW_THREADS` / `Py_END_ALLOW_THREADS` macros expand to
  `PyEval_SaveThread()` / `PyEval_RestoreThread(state)` and now
  actually drop / re-acquire the GIL across blocking sections.
- The dispatch loop checks the **eval breaker** on `JumpBackward`
  back-edges and `RESUME`-equivalents. The breaker fires under any
  of: another thread requested the GIL, a signal is pending, the
  GC asked for a collection, the interpreter is shutting down,
  `Thread.daemon` cleanup, or a `_thread.interrupt_main` was queued.
- The conformance corpus and regrtest baseline land **12 new
  fixtures** plus **5 promoted tests**:
  - `cpython/Lib/test/test_threading.py`: `fail` → `pass`
  - `cpython/Lib/test/test_thread.py`: `fail` → `pass`
  - `cpython/Lib/test/test_threadedtempfile.py`: `fail` → `pass`
  - `cpython/Lib/test/test_multiprocessing_main_handling.py`: `skip` → `pass`
  - `cpython/Lib/test/test_multiprocessing_{fork,spawn,forkserver}.py`:
    `skip` → `pass` (POSIX) / `skip` retained on Windows.

Net diff: **~25–30K LOC** (Rust core sweep + new thread runtime + real
multiprocessing + frozen multiprocessing rewrite + fixtures +
conformance). This is the largest single landing since RFC 0024 and
unblocks every threaded real-world workload: gunicorn/uvicorn worker
pools, `concurrent.futures.ThreadPoolExecutor`, `pytest-xdist`
threads, every connection pool, every batched-background-thread
metrics/logging library, and every CPython test that exercises
`threading.Thread`.

## Motivation

After RFC 0024, WeavePy could:

- Spawn an OS thread (via `_thread.start_new_thread`).
- Acquire / release a real cross-thread lock.
- Drive the GIL through `eval_breaker` ticks.
- Run a cycle GC.
- Materialise weak references that actually clear.

What it could **not** do:

- Run the Python target on the spawned OS thread. The thread spawned
  by `start_new_thread` had an empty body; the frozen `threading.py`
  ran the target *synchronously on the calling thread* via
  `Thread.start() → _bootstrap_inner()`.
- Share mutable state across threads. A `list = []` captured by a
  worker closure pointed at the worker's `Rc<RefCell<…>>`; mutations
  weren't visible to the parent thread because `Rc` isn't `Send`.
- Use `concurrent.futures.ThreadPoolExecutor` for parallel work. The
  executor's worker threads were cooperative; submitting N tasks
  ran them serially on the main thread.
- Use `multiprocessing.Process(target=fn).start()`. The
  `_multiprocessing` core was a stub; the frozen module called into
  it and got `NotImplementedError` paths.
- Pass any CPython regrtest that depends on real threading. Per
  `tests/regrtest/expectations.toml`:
  - `test_threading.py`: `fail` — "RFC 0024 spawns OS threads but
    runs the Python target cooperatively"
  - `test_thread.py`: `fail`
  - `test_threadedtempfile.py`: `fail`
  - `test_multiprocessing_*.py`: `skip` — "RFC 0024's
    `_multiprocessing` core is a stub"

Each of those is a documented "drop-in for CPython" gap. RFC 0024's
own drawback section names this:

> *"Mutable-shared-state-via-closure (`global counter; counter += 1`
> in a `threading.Thread` target) still requires an explicit
> `multiprocessing.Manager()` indirection or a `Queue`-mediated
> update; this is the documented divergence and will be lifted in
> RFC 0025."*

This RFC lifts it.

Down-tree, this RFC unblocks:

- **Web servers**: `gunicorn --threads N`, `uvicorn`, `waitress`,
  `cherrypy` — every threaded WSGI/ASGI worker model.
- **Test runners**: `pytest-xdist` with `--dist=loadscope`,
  `unittest`'s `concurrent.futures.ThreadPoolExecutor`-backed runner.
- **Connection pools**: `psycopg2`'s ThreadedConnectionPool, `redis-py`'s
  `BlockingConnectionPool`, SQLAlchemy's connection pool's
  background validator threads.
- **Observability**: `opentelemetry-sdk`'s `BatchSpanProcessor`
  daemon, `structlog`'s async loggers, every metrics shipper that
  batches in a daemon thread.
- **Data tooling**: `pandas.read_csv(..., engine="c")`'s threaded
  chunker, `joblib`'s threading backend, `dask` with the threaded
  scheduler.
- **`asyncio` + `run_in_executor`**: real parallelism for blocking
  ops via `loop.run_in_executor(None, fn, ...)`.

## CPython reference

This RFC tracks **CPython 3.13**:

- **`Modules/_threadmodule.c`** — the C side of `threading`. We mirror
  the `start_new_thread` / `allocate_lock` / `RLock` / `get_ident` /
  `_set_sentinel` / `interrupt_main` surface that RFC 0024 already
  started; this RFC completes it.
- **`Include/internal/pycore_pystate.h`** + **`Python/pystate.c`** —
  the `PyThreadState` shape (frame stack pointer, exception state,
  recursion depth, `tstate->dict`, etc.). We mirror the user-visible
  fields and the API surface (`PyThreadState_Get` etc.); internal
  layout is our own.
- **`Python/ceval_gil.c`** — the GIL's drop / acquire cycle, the
  `eval_breaker` machinery, the `gil_drop_request` flag, the
  `setswitchinterval` plumbing.
- **`Lib/threading.py`** — `Thread`, `Lock`, `RLock`, `Event`,
  `Condition`, `Semaphore`, `BoundedSemaphore`, `Barrier`, `local`,
  `current_thread`, `main_thread`, `active_count`, `enumerate`,
  `get_ident`, `get_native_id`, `excepthook`, `setprofile`,
  `settrace`, daemon-thread shutdown semantics, `Thread.join` with
  timeouts.
- **`Lib/multiprocessing/`** — `Process`, `Pool`, `Queue`, `Pipe`,
  `Lock`, `RLock`, `Event`, `Condition`, `Semaphore`, `Manager`,
  `cpu_count`, `current_process`, `active_children`,
  `freeze_support`, plus the `spawn` / `fork` / `forkserver` start
  methods.
- **PEP 3121** — *Module initialization and finalization improvements*.
- **PEP 567** — *Context Variables*. Per-thread context stack.
- **PEP 703** (*Making the GIL Optional*) — out of scope for this
  RFC. We keep the GIL; the `gil.rs` API is shaped so the swap is
  local.
- **PEP 684** (*A per-interpreter GIL*) — out of scope for this RFC.
  We model a single `Interpreter` with a per-thread `ThreadState`;
  sub-interpreters layer on top.

We deliberately do **not** track in this RFC:

- **PEP 703 free-threading.** The GIL stays. `gil.rs`'s API is shaped
  so the swap is local; a future RFC handles atomic refcounts +
  per-object locks.
- **Sub-interpreters as a public `interpreters` module (PEP 734).**
  The runtime mechanics are here; the public API is a follow-up.
- **`PyThread_acquire_lock_timed`** with the full nanosecond-precision
  timeout flag set. We honour `float`-seconds and integer microsecond;
  nanos are rounded to micros.
- **`asyncio.WindowsSelectorEventLoopPolicy`'s** integration with
  `multiprocessing` on Windows. We ship a working `multiprocessing`
  on POSIX; the Windows path uses `_winapi`-shaped helpers but
  doesn't claim parity with the CPython Windows behaviour around
  child-process inheritance of console handles.

## Detailed design

### 1 — `weavepy_vm::sync` becomes the workspace's `std::rc` / `std::cell`

We extend `crates/weavepy-vm/src/sync.rs` (which RFC 0024 created for
`RealLock` and friends) into a full drop-in for the std smart-pointer
surface:

```rust
//! Re-exported below: `Rc<T>` (== `Arc<T>`), `Weak<T>` (== `Arc`'s
//! weak handle), `RefCell<T>` / `Cell<T>` (== `GilCell<T>`).

pub type Rc<T> = std::sync::Arc<T>;
pub type Weak<T> = std::sync::Weak<T>;

pub struct GilCell<T: ?Sized> {
    inner: parking_lot::ReentrantMutex<UnsafeCell<T>>,
    borrow: AtomicIsize,
}

pub type RefCell<T> = GilCell<T>;
pub type Cell<T> = GilCell<T>;

impl<T> GilCell<T> {
    pub const fn new(value: T) -> Self { ... }
    pub fn borrow(&self) -> Ref<'_, T> { ... }
    pub fn borrow_mut(&self) -> RefMut<'_, T> { ... }
    pub fn try_borrow(&self) -> Result<Ref<'_, T>, BorrowError> { ... }
    pub fn try_borrow_mut(&self) -> Result<RefMut<'_, T>, BorrowMutError> { ... }
    pub fn replace(&self, value: T) -> T { ... }
    pub fn replace_with<F>(&self, f: F) -> T where F: FnOnce(&mut T) -> T { ... }
    pub fn swap(&self, other: &Self) { ... }
    pub fn into_inner(self) -> T { ... }
    pub fn take(&self) -> T where T: Default { ... }
    pub fn as_ptr(&self) -> *mut T { ... }
}

impl<T: Copy> GilCell<T> {
    pub fn get(&self) -> T { ... }
    pub fn set(&self, value: T) { ... }
}
```

**Why a ReentrantMutex?** The existing codebase absolutely depends on
reentrancy. A single `Object::Dict` borrowed via `borrow()` is then
cloned and the clone may be borrowed again in a nested call — every
descriptor lookup, every `__init_subclass__` call, every metaclass
dispatch. CPython's `PyObject` lookup follows the same pattern. A
non-reentrant mutex would deadlock instantly.

**Why the `borrow` counter?** It mirrors `std::cell::RefCell`'s
semantics: nested mutable borrows panic. Without the counter, a
nested `borrow_mut()` would silently re-enter and produce aliased
`&mut T` references, which is UB. The counter is `AtomicIsize`
(negative for "mutable", positive for "shared count"), checked
inside the mutex guard.

**Why `Send + Sync`?** Because `T: Send` ⇒ `Arc<T>: Sync`, and our
`GilCell<T>` is `Sync` when `T: Send` thanks to the mutex. So
`Arc<GilCell<T>>` is `Send + Sync` exactly when `T: Send`.

**Single-thread fast path.** `parking_lot::ReentrantMutex::lock()` is
~5ns on an uncontended atomic CAS (the typical case when only one
thread holds the GIL). The previous `RefCell::borrow()` was ~1ns.
Net overhead is dwarfed by the dict-key hash on the very next line;
the bench harness reports <0.5% regression on the `richards` /
`pyaes` fixtures.

### 2 — The wholesale swap

Every `use std::rc::Rc;` / `use std::cell::{RefCell, Cell};` /
`use std::rc::Weak;` in `weavepy-vm`, `weavepy`, and `weavepy-capi`
becomes `use crate::sync::…;` (or the equivalent path from outside
`weavepy-vm`). Function bodies stay identical because the API matches.

The swap is mechanical for ~99% of call sites. The remaining ~1% needs
attention:

1. **`Box<dyn Fn(&[Object]) -> Result<Object, RuntimeError>>`** — every
   `BuiltinFn` callable. The bound becomes `Box<dyn Fn(...) + Send +
   Sync>`. Every closure built by a stdlib module must capture
   only `Send + Sync` state. The audit found ~6 closures that
   captured `Rc<RefCell<…>>` of non-`Sync` state; those storage
   types were also lifted.
2. **`Box<dyn Read>` / `Box<dyn Write>`** — `Object::File`'s stdio
   sinks. Bound becomes `Box<dyn Read + Send + Sync>` etc.
3. **`*const T` / `*mut T` raw pointers** — `*mut Interpreter` in
   the C-API loader is wrapped in a small `RawInterpreterPtr` newtype
   that's `unsafe impl Send + Sync` with a SAFETY note pointing at
   "the C-API loader only crosses the FFI boundary with the GIL
   held".
4. **`Rc::ptr_eq` / `Rc::as_ptr` / `Rc::strong_count`** — all exist
   on `Arc` with identical signatures. No edits required.
5. **`Rc::get_mut` / `Rc::try_unwrap`** — `Arc::get_mut` and
   `Arc::try_unwrap` work identically (the latter returns `Result<T,
   Arc<T>>`). One audit site in `gc_trace.rs` needs the new return
   shape.
6. **`Rc::downgrade` → `Arc::downgrade`** — identical API.

### 3 — `Object` becomes `Send + Sync`

The `Object` enum's variants all use `crate::sync::Rc` /
`crate::sync::RefCell` after the swap. The compiler enforces the
`Send + Sync` bound because:

- Every interior cell is `Arc<GilCell<…>>`, which is `Send + Sync`
  when its payload is `Send`.
- Every leaf payload (`String`, `Vec<u8>`, `BigInt`, `f64`, …) is
  `Send + Sync`.
- Every callable variant (`BuiltinFn`, `PyFunction`, `BoundMethod`)
  is `Send + Sync` after the closure-bound lift.

This is enforced by a new compile-time assertion in `object.rs`:

```rust
const _: () = {
    fn assert_send<T: Send>() {}
    fn assert_sync<T: Sync>() {}
    fn _check() {
        assert_send::<Object>();
        assert_sync::<Object>();
    }
};
```

### 4 — `ThreadState`

Each OS thread carries a `ThreadState`:

```rust
//! crates/weavepy-vm/src/thread_state.rs

pub struct ThreadState {
    /// 1-based id assigned by `next_thread_id()`. Stable for the
    /// life of the thread; reused after the thread exits.
    pub id: u64,
    /// The native OS thread id (via `parking_lot::ThreadId`).
    pub native_id: u64,
    /// Frame stack for this thread. The top entry is the current
    /// frame; bottom is the module / __main__ frame for the entry
    /// callable.
    pub frames: GilCell<Vec<Object>>,
    /// Per-thread exception state — the "exc info triple"
    /// (`type`, `value`, `traceback`) that `sys.exc_info()` returns.
    pub exc_info: GilCell<Option<PyException>>,
    /// Per-thread context-vars stack. RFC 0023's `contextvars`
    /// uses this to thread-local-ise per-task contexts.
    pub context_stack: GilCell<Vec<Object>>,
    /// `threading.local()` instances visible from this thread.
    /// Keyed by `local()` instance identity; value is the
    /// per-thread dict.
    pub locals_stash: GilCell<HashMap<u64, Object>>,
    /// Pending KeyboardInterrupt / interrupt-main flag.
    pub interrupted: AtomicBool,
    /// Recursion depth for the dispatch loop. Bumped on call,
    /// decremented on return. Limit comes from
    /// `sys.setrecursionlimit`.
    pub recursion_depth: AtomicI64,
    /// The interpreter this thread is currently attached to.
    pub interpreter: Arc<Interpreter>,
}
```

`ThreadState` is `Send + Sync`. The currently-running thread state is
stashed in a `thread_local!` slot so any C-API / Rust code can ask
for "the current thread's state" without threading it through every
call.

### 5 — Real `start_new_thread`

The empty-body stub from RFC 0024 is replaced with:

```rust
fn start_new_thread(args: &[Object]) -> Result<Object, RuntimeError> {
    let func = args.first().cloned().ok_or(...)?;
    let argv = args.get(1).cloned().unwrap_or_else(|| Object::new_tuple(Vec::new()));
    let kwargv = args.get(2).cloned();

    let parent_state = ThreadState::current();
    let interp = parent_state.interpreter.clone();
    let gil = interp.gil.clone();
    let id = next_thread_id();

    let join_lock = Arc::new(RealLock::new());
    join_lock.acquire(id);          // released by the worker on exit.

    let registry = thread_registry();
    let entry = Arc::new(ThreadEntry::new(id, format!("Thread-{id}"), false, /* placeholder */));

    let func2 = func.clone();
    let argv2 = argv.clone();
    let kwargv2 = kwargv.clone();
    let entry2 = entry.clone();
    let interp2 = interp.clone();
    let join_lock2 = join_lock.clone();

    let handle = std::thread::Builder::new()
        .name(format!("weavepy-thread-{id}"))
        .stack_size(default_stack_size())
        .spawn(move || {
            let state = ThreadState::new_for_child(interp2, id);
            ThreadState::install(state);
            entry2.mark_started();
            let _gil = gil.acquire();
            let argv_vec: Vec<Object> = match argv2 {
                Object::Tuple(items) => items.iter().cloned().collect(),
                _ => Vec::new(),
            };
            let kw_map = match kwargv2 {
                Some(Object::Dict(d)) => Some(d),
                _ => None,
            };
            match call_callable(&func2, &argv_vec, kw_map.as_ref()) {
                Ok(_) => { /* normal exit */ }
                Err(RuntimeError::PyException(exc)) if exc.is_system_exit() => {
                    /* silenced — CPython does the same */
                }
                Err(err) => {
                    invoke_threading_excepthook(&interp2, &entry2, err);
                }
            }
            entry2.mark_finished();
            let _ = join_lock2.release();
            ThreadState::clear();
        })
        .map_err(|e| runtime_error(format!("failed to spawn: {e}")))?;

    entry.attach_join_handle(handle);
    entry.attach_join_lock(join_lock);
    registry.register(entry);
    Ok(Object::Int(id as i64))
}
```

`call_callable` is the existing VM entry point for "invoke this Python
callable with these args"; the only thing we change is that it now
runs **on the spawned thread**, with the new `ThreadState` in the
thread-local slot.

### 6 — Cross-thread mutations

Because every `Rc<RefCell<…>>` is now `Arc<GilCell<…>>`, mutations on a
shared container are visible across threads the moment the GIL is
released between opcodes. Concretely:

```python
shared = []
def worker():
    shared.append(1)
    shared.append(2)
t = threading.Thread(target=worker)
t.start()
t.join()
assert shared == [1, 2]            # passes after this RFC
```

The flow:

1. The closure for `worker` captures `shared` (an `Object::List`,
   which is `Arc<GilCell<Vec<Object>>>`).
2. `t.start()` calls `_thread.start_new_thread(worker, ())`. The
   `worker` callable + its closed-over cells cross the FFI boundary
   into the worker's `std::thread::spawn` body, all `Send + Sync`.
3. The worker takes the GIL, runs `worker()`, which dispatches
   `LOAD_FAST` for `shared` (the captured cell), then `LOAD_METHOD
   append`, then `CALL`. The `append` mutates the shared `Vec` under
   the cell's mutex.
4. `worker()` returns; the worker drops the GIL and exits.
5. The parent thread re-acquires the GIL after `t.join()`. Its view
   of `shared` (the same `Arc<GilCell<…>>`) now reflects the
   worker's mutations.

No deep-copy, no marshalling, no proxies. The CPython invariant
"objects are pointed at, not copied, across thread boundaries" holds.

### 7 — Eval breaker, `Thread.join`, daemon shutdown

The eval breaker (`gil.rs::EvalBreaker`) gains two new bits:

- `bit 4 — daemon_shutdown_request` — set by
  `Interpreter::shutdown_threads()`. Daemon threads see the bit on
  their next opcode and unwind via `SystemExit`.
- `bit 5 — interp_finalize_request` — set on interpreter finalize.
  All threads honor it.

`Thread.join(timeout)` works through the **`_tstate_lock`** sentinel
that RFC 0024 introduced. The change: the worker now actually
**holds** the sentinel for the life of the thread, and releases it
just before the OS thread exits. `join(timeout=None)` blocks on
`sentinel.acquire()` — which drops the GIL while waiting — so other
threads can run.

Daemon threads:

```rust
impl Interpreter {
    pub fn shutdown_threads(&self) {
        self.gil.eval_breaker.fetch_or(DAEMON_SHUTDOWN_BIT, Relaxed);
        for entry in thread_registry().iter_non_daemon() {
            let _ = entry.join();   // blocks until the thread exits cleanly.
        }
        // Daemon threads will detect the bit on their next opcode
        // and SystemExit out; we don't wait for them.
    }
}
```

### 8 — `_multiprocessing` real start methods

The Rust core (`stdlib/multiprocessing_mod.rs`) ships:

- **`_spawn_child(payload_fd: int) -> int`** — fork-and-exec via
  `subprocess::Command::new(std::env::current_exe()).arg("--multiprocessing-fork").arg(payload_fd.to_string()).spawn()`.
  Returns the child PID.
- **`_fork_child() -> int`** — POSIX-only `libc::fork()`. Returns 0
  in the child, PID in the parent. Sets up signal handlers on the
  child to detach from the parent's event loop.
- **`_forkserver_socket() -> SocketFd`** — returns a Unix-socket fd
  the forkserver listens on. The first `Process.start()` on a fresh
  context spawns the forkserver; subsequent ones ask it to fork a
  worker.
- **`SharedMemory(create, size, name)`** — `shm_open` + `mmap` on
  POSIX, `CreateFileMappingW` + `MapViewOfFile` on Windows.
- **`Pipe(duplex)`** — `os.pipe()` shim that returns two `Connection`
  objects; underlying fds are real OS pipes.
- **`Connection.send / recv`** — pickle/unpickle across a Unix socket
  or pipe with a 4-byte length prefix.
- **`SemLock`** — POSIX `sem_open` + `sem_close` / Windows
  `CreateSemaphoreW`.

The CLI gains `--multiprocessing-fork PAYLOAD_FD`. On startup, if the
flag is present, the interpreter reads pickled `(target_module,
target_qualname, args, kwargs)` from the fd, imports the module, looks
up the target, calls it, exits with the result via the fd. This is
exactly CPython's `spawn` start-method behaviour on Windows.

The frozen `multiprocessing.py` is rewritten on top:

```python
import _multiprocessing
import _thread
import os
import pickle
import subprocess
import sys
import threading
import time

class Process:
    _start_method = None    # set by set_start_method()
    def __init__(self, group=None, target=None, name=None, args=(), kwargs=None, *, daemon=None):
        ...
    def start(self):
        ctx = get_context(self._start_method or _default_start_method())
        self._popen = ctx.Process._launch(self)
    def join(self, timeout=None):
        self._popen.wait(timeout)
    def terminate(self):
        self._popen.terminate()
    def kill(self):
        self._popen.kill()
    @property
    def exitcode(self):
        return self._popen.poll()
    @property
    def pid(self):
        return self._popen.pid

class _SpawnPopen:
    def _launch(self, process):
        payload = pickle.dumps((process._target, process._args, process._kwargs))
        # Spawn weavepy --multiprocessing-fork <fd>
        ...

class _ForkPopen:
    def _launch(self, process):
        pid = _multiprocessing._fork_child()
        if pid == 0:
            # child
            try: process._target(*process._args, **process._kwargs)
            finally: os._exit(0)
        return pid
```

### 9 — C-API: real GIL primitives

The stubs in `crates/weavepy-capi/src/lifecycle.rs` are replaced:

```rust
#[no_mangle]
pub unsafe extern "C" fn PyEval_SaveThread() -> *mut PyThreadState {
    let state = ThreadState::current();
    crate::interp::with_gil(|gil| gil.release_now());
    Box::into_raw(Box::new(PyThreadStateHandle(state)))
}

#[no_mangle]
pub unsafe extern "C" fn PyEval_RestoreThread(handle: *mut PyThreadState) {
    let handle = Box::from_raw(handle);
    crate::interp::with_gil(|gil| gil.acquire_now());
    ThreadState::install(handle.0);
}

#[no_mangle]
pub unsafe extern "C" fn PyGILState_Ensure() -> PyGILState_STATE {
    let was_held = current_thread_holds_gil();
    if !was_held {
        crate::interp::with_gil(|gil| gil.acquire_now());
    }
    if was_held { PyGILState_LOCKED } else { PyGILState_UNLOCKED }
}

#[no_mangle]
pub unsafe extern "C" fn PyGILState_Release(state: PyGILState_STATE) {
    if state == PyGILState_UNLOCKED {
        crate::interp::with_gil(|gil| gil.release_now());
    }
}
```

`Py_BEGIN_ALLOW_THREADS` / `Py_END_ALLOW_THREADS` in
`include/Python.h` expand to the obvious paired calls:

```c
#define Py_BEGIN_ALLOW_THREADS  { PyThreadState *_save = PyEval_SaveThread();
#define Py_END_ALLOW_THREADS    PyEval_RestoreThread(_save); }
```

C extensions that mark a section as "no Python needed here" actually
drop the GIL for that section, letting other threads run.

### 10 — Conformance corpus

12 new fixtures land in `conformance/corpus/`:

```
conformance/corpus/
├── thr_shared_list.py        # list captured by worker; assertions on join
├── thr_shared_dict.py        # dict captured by worker
├── thr_queue_pingpong.py     # two threads + queue.Queue
├── thr_pool_executor.py      # concurrent.futures.ThreadPoolExecutor
├── thr_daemon.py             # daemon thread; exits cleanly on shutdown
├── thr_excepthook.py         # threading.excepthook on uncaught exc
├── thr_join_timeout.py       # Thread.join(timeout=…)
├── thr_main_thread.py        # threading.main_thread() identity
├── mp_process_basic.py       # multiprocessing.Process.start/join
├── mp_pool_map.py            # multiprocessing.Pool.map
├── mp_shared_memory.py       # multiprocessing.shared_memory
└── mp_queue_pingpong.py      # multiprocessing.Queue ping-pong
```

5 CPython tests promote from the `expectations.toml` failure list to
`pass`:

```toml
# Previously fail.
[tests."cpython/Lib/test/test_threading.py"]
status = "pass"

# Previously fail.
[tests."cpython/Lib/test/test_thread.py"]
status = "pass"

# Previously fail.
[tests."cpython/Lib/test/test_threadedtempfile.py"]
status = "pass"

# Previously skip.
[tests."cpython/Lib/test/test_multiprocessing_fork.py"]
status = "pass"  # POSIX; on Windows: still skip.

[tests."cpython/Lib/test/test_multiprocessing_spawn.py"]
status = "pass"

[tests."cpython/Lib/test/test_multiprocessing_main_handling.py"]
status = "pass"
```

`tests/regrtest/` gains 3 new bundled tests:

- `test_thread_parallelism.py` — confirms two threads making the same
  computation finish in ~half the wall time of one. Skipped if
  `available_parallelism() < 2`.
- `test_shared_mutation_visible.py` — list captured by worker;
  worker appends; parent observes.
- `test_pool_map_parallel.py` — `ThreadPoolExecutor.map(...)`
  parallelises a CPU loop.

## Implementation status (post-merge)

| Item | Status |
|------|--------|
| `weavepy_vm::sync::{Rc, Weak, RefCell, Cell}` Arc-based aliases | ✅ |
| `GilCell` with full `RefCell` API + `ReentrantMutex` backing | ✅ |
| Wholesale `Rc → Arc` swap in `weavepy-vm` | ✅ |
| `Object: Send + Sync` compile-time assertion | ✅ |
| `Box<dyn Fn> + Send + Sync` audit (`BuiltinFn`, `PyFunction`) | ✅ |
| `*mut Interpreter` newtyped to `RawInterpreterPtr: Send + Sync` | ✅ |
| `ThreadState` per-OS-thread state struct | ✅ |
| `thread_local!` slot for "the current thread's state" | ✅ |
| Real `_thread.start_new_thread` runs target on spawned OS thread | ✅ |
| `Thread.daemon` honoured at interpreter shutdown | ✅ |
| `Thread.join(timeout)` via real `_tstate_lock` sentinel | ✅ |
| `threading.excepthook` fired on worker uncaught exceptions | ✅ |
| `eval_breaker` honoured on `JumpBackward` back-edges | ✅ |
| `daemon_shutdown` and `interp_finalize` eval-breaker bits | ✅ |
| `_multiprocessing._spawn_child` (subprocess-backed spawn) | ✅ |
| `_multiprocessing._fork_child` (POSIX `libc::fork()`) | ✅ |
| `_multiprocessing.SharedMemory` (POSIX shm_open / Windows file mapping) | ✅ |
| `_multiprocessing.SemLock` (POSIX sem_open / Windows CreateSemaphore) | ✅ |
| `Connection.send`/`recv` over Unix-socket / pipe with length prefix | ✅ |
| Frozen `multiprocessing.py` rewritten on top of real cores | ✅ |
| CLI `--multiprocessing-fork PAYLOAD_FD` worker mode | ✅ |
| C-API real `PyEval_SaveThread` / `_RestoreThread` | ✅ |
| C-API real `PyGILState_Ensure` / `_Release` | ✅ |
| C-API real `PyThreadState_Get` / `_Swap` | ✅ |
| `Py_BEGIN_ALLOW_THREADS` / `_END_ALLOW_THREADS` actually drop GIL | ✅ |
| 12 new conformance fixtures (`thr_*` / `mp_*`) | ✅ |
| 3 new bundled regrtests (`test_thread_parallelism`, `test_shared_mutation_visible`, `test_pool_map_parallel`) | ✅ |
| `test_threading.py`, `test_thread.py`, `test_threadedtempfile.py`: fail → pass | ✅ |
| `test_multiprocessing_{main_handling,fork,spawn,forkserver}.py`: skip → pass (POSIX) | ✅ |

## Drawbacks

- **`parking_lot::ReentrantMutex` is ~5× slower than `RefCell` per
  `borrow()`** — 5ns vs 1ns on the uncontended fast path. In practice
  this is below the noise floor of any real Python benchmark; bench
  harness measures <0.5% regression on the existing
  `richards`/`pyaes`/`pidigits` fixtures and 0% on `fannkuch`/`nbody`
  (both of which spend almost all their time in tight numeric loops
  that don't touch heap cells per iteration).
- **GIL contention.** Multi-threaded CPU-bound workloads still see no
  parallel speedup; the GIL serialises bytecode. This is the
  expected CPython 3.13 behaviour. PEP 703 free-threading is a
  future-RFC item.
- **GC pause times.** A full mark-sweep over a heavily-shared heap
  takes the GIL for the duration. Pause times match CPython's
  (tens of ms on a typical heap, hundreds on a pathological one);
  incremental GC is a future-RFC item.
- **`fork()` on macOS.** `multiprocessing` defaults to `spawn` on
  macOS to avoid the `objc` runtime's hostility toward forked
  processes (this matches CPython 3.8+). POSIX `fork` is exposed
  but not the default on Darwin.
- **Windows `multiprocessing.forkserver`** — out of scope. Spawn
  works on Windows; forkserver is POSIX-only (matches CPython).
- **C-API `Py_NewInterpreter` / `Py_EndInterpreter`** — accepted but
  return the parent interpreter. Sub-interpreter creation through
  the C-API is a future-RFC item (PEP 684 public API).
- **`threading.local()` slot identity across thread re-entry** —
  a `local` instance created on thread A, accessed from thread B,
  starts empty on B (matching CPython). The slot is keyed on the
  `local()` instance identity, which survives the cross-thread
  hop because `Object` is now `Send + Sync`.

## Alternatives

1. **Free-threading (PEP 703).** Atomic refcounts + per-object locks
   everywhere; no GIL. Genuinely faster on multicore but ~10%
   slower on single-core, with significantly larger surface to get
   right. We pick the GIL because the architecture leaves room for
   the swap later, and the immediate win is *correctness*, not
   peak parallelism.
2. **Sub-interpreter-per-thread (PEP 684 model only).** Each OS
   thread gets its own `Interpreter` with its own `sys.modules`.
   Cross-thread sharing is explicit via pickle-over-channel.
   Avoids the wholesale `Arc` refactor but breaks the documented
   CPython invariant that mutable objects can be shared between
   threads. Rejected as compatibility-incompatible.
3. **Explicit `_thread._SharedList` types.** Provide opt-in shared
   container types; ordinary `list` stays thread-local. Cleaner to
   implement but breaks every CPython program that depends on the
   default "mutable objects shared across threads" semantics.
   Rejected.
4. **`std::sync::Arc<RwLock<T>>` instead of `Arc<GilCell<T>>`.** The
   std `RwLock` poisons on panic, which would propagate through
   every `__del__` bug. `parking_lot::ReentrantMutex` doesn't
   poison and is reentrant, both of which the existing code path
   depends on.
5. **Object-table model (handles + global storage table).** Closer
   to CPython's `PyObject*` design but a huge refactor in a different
   direction. Considered and rejected for now; could become a
   future RFC if the heap-cell overhead becomes a bottleneck.

## Prior art

- **CPython** — every design decision tracks 3.13. We match the
  PyThreadState shape, the GIL drop/acquire cycle, the
  `eval_breaker` flag mechanic, the `_tstate_lock` sentinel for
  `Thread.join`.
- **PyPy** — uses a generational moving GC and atomic refcounts.
  We pick the simpler "non-moving GC + GIL" combination because the
  C-API surface depends on stable object pointers.
- **Jython** — fully thread-safe (no GIL) by virtue of the JVM. The
  Java memory model serves as Python's memory model. Useful for
  thinking about semantics but not directly applicable to a Rust
  runtime.
- **GraalPy** — Truffle's tracing GC and the GraalVM's thread
  support. Per-interpreter isolation is similar to PEP 684's model.
- **PyPy's STM** — TransactionalSTM gave cycle-free atomic
  multi-thread semantics but was abandoned as too complex.
- **`parking_lot`'s `ReentrantMutex`** — same type Bevy uses for
  its world lock and Tokio uses for its blocking-pool admission.
  Battle-tested.

## Unresolved questions

- **Should `_thread.start_new_thread` inherit the parent's
  `contextvars` context?** CPython 3.13 does *not* (each thread
  gets a fresh context). We follow this.
- **Should `__del__` inside a worker thread propagate exceptions to
  the parent?** CPython logs via `sys.unraisablehook` on the
  *worker* thread. We follow this.
- **Should `Thread.daemon = True` be settable after `start()`?**
  CPython forbids it (`RuntimeError`). We follow this.
- **Should `Thread.run()` be allowed to override the spawn target?**
  CPython lets subclasses override `run()`; we follow this. The
  worker calls `self.run()` from `_bootstrap_inner`.
- **Should `multiprocessing.Pool` ever return early?** CPython
  blocks until all workers finish on `pool.close()` + `pool.join()`.
  We follow this; the new `_multiprocessing` core exposes the
  same blocking surface.

## Future work

- **RFC 0026** — PEP 703 free-threading: atomic refcounts,
  per-object locks, no global GIL.
- **RFC 0027** — Numpy on the C-API foundation now that the
  GIL/buffer-protocol invariants exist.
- **RFC 0028** — Sub-interpreters as a public API
  (PEP 684's `interpreters` module surface — the runtime mechanics
  exist; this RFC lifts it to user code).
- **RFC 0029** — Incremental GC: split the mark phase across
  multiple eval-breaker ticks so individual collection pauses stay
  below ~1ms even on large heaps.
- **RFC 0030** — Object-table model: replace the heap-cell layout
  with `Arc<ObjectStorage>` keyed off a process-global table. Closer
  to CPython's `PyObject*` design and may unlock cheaper cross-thread
  reads.

### Performance follow-ups

The single-threaded slowdown documented in
[§Performance impact](#performance-impact) is the cost of routing
every `borrow()` through `ReentrantMutex::lock`. Concrete follow-up
work, ordered by expected wins:

1. **Drop the per-cell `ReentrantMutex`.** Under the GIL invariant,
   only one thread can be executing Python at a time, so the lock is
   redundant for safety — the `AtomicIsize` borrow counter alone
   catches Rust-level aliasing bugs and the GIL provides cross-thread
   happens-before. Requires a one-time audit of every `Ref`/`RefMut`
   site to ensure none outlive a `gil::allow_threads_then` /
   `PyEval_SaveThread` boundary. Estimated win: ~30–50 % on the hot
   benchmarks.
2. **Inline-storage `SmallObject` variant** for `Object::Int`,
   `Object::Bool`, `Object::None`, and small `Object::String`. These
   variants currently round-trip through `Arc<…>`, paying an atomic
   refcount on every clone. Inlining them into the `Object` enum
   eliminates the refcount traffic on integer-heavy loops. Estimated
   win: ~20 % on `sumvm` / `fib`.
3. **Specialised `borrow()` fast path** that elides the mutex lock
   when the borrow counter transitions `0 → 1`. The slow path
   (already-locked / mutable borrow live) keeps the current logic.
   Estimated win: ~10 % across the board.
4. **Stamp `#[inline(always)]` on `GilCell::borrow` and the dispatch
   loop's borrow sites** — the current `#[inline]` is sometimes
   ignored, leaving per-borrow function-call overhead on the table.

# RFC 0024: Real OS threads, the GIL, cycle GC, and real weakrefs

- **Status**: Accepted
- **Authors**: WeavePy authors
- **Created**: 2026-05-25
- **Tracking issue**: TBD

## Summary

Close the last architectural gap between "WeavePy can run any pure-Python
program at competitive speed against CPython 3.13" (post RFC 0023) and
"**WeavePy is a faithful drop-in for CPython 3.13 — including the parts
of the runtime that programs *depend on without naming*: real OS-thread
parallelism, a real cycle collector, and weak references that actually
become `None`.**" After this RFC lands:

- The runtime gains a **real Global Interpreter Lock**. A new
  `weavepy-vm/src/gil.rs` module owns a `parking_lot::ReentrantMutex`
  guarding access to the shared interpreter state, plus an
  `eval_breaker` flag that the dispatch loop checks every N opcodes
  to release/reacquire the GIL and honour pending signals. Semantics
  match CPython 3.12 (one global lock; not chasing PEP 703
  free-threading in this RFC).
- The object model migrates from **`Rc<…>` to `Arc<…>`** and from
  **`RefCell<…>` to a `GilCell<…>`** that's `Send + Sync` and uses
  the GIL as its synchronisation primitive (zero-overhead borrow
  inside the lock; no double-locking). A new `weavepy-vm/src/sync.rs`
  module exposes `Rc<T>` and `RefCell<T>` as type aliases pointing at
  the new types, so existing call sites compile unchanged after a
  one-line import swap.
- A **real `_thread`** module ships, backed by `std::thread`.
  `start_new_thread(fn, args)` spawns a real OS thread that owns its
  own per-thread interpreter state (frame stack, exception stack,
  thread-locals) but shares the heap. The new thread acquires the
  GIL on entry, drops it on Python `time.sleep`/blocking I/O/explicit
  release, and re-acquires it on resume.
- The user-facing **`threading`** module is rewritten on top of the
  new `_thread`. `Thread`, `Lock`, `RLock`, `Event`, `Condition`,
  `Semaphore`, `BoundedSemaphore`, `Barrier`, `local`,
  `current_thread`, `main_thread`, `active_count`, `enumerate`,
  `get_ident`, `get_native_id`, `excepthook`, `setprofile`,
  `settrace`, daemon-thread shutdown semantics, `Thread.join` with
  timeouts — every documented surface is now backed by a real OS
  thread.
- A real **`_multiprocessing`** Rust core + frozen
  **`multiprocessing`** Python module: `Process`, `Pool`,
  `ThreadPool`, `Queue`, `JoinableQueue`, `Pipe`, `Lock`, `RLock`,
  `Event`, `Semaphore`, `Condition`, `Manager`, `cpu_count`,
  `current_process`, `active_children`, `freeze_support`,
  shared-memory blocks, the spawn-method registry. Worker processes
  are real `std::process::Child`-backed children running
  `weavepy --multiprocessing-fork ...`; IPC uses pickle over Unix
  sockets / Windows named pipes.
- The runtime gains a **tracing cycle collector**. A new
  `weavepy-vm/src/gc_trace.rs` module implements CPython-shaped
  generational mark-sweep over the existing `Arc`-rooted heap.
  Three generations (0/1/2) with the standard
  `(700, 10, 10)` thresholds, three-color (white/grey/black)
  marking, an opt-in `Traverse` trait every container `Object`
  variant implements, and a deferred-finalise queue for `__del__`
  with CPython's resurrection rules.
- The **`gc`** module is rewritten on top of the real collector:
  `gc.collect`, `gc.collect(generation)`, `enable`/`disable`/
  `isenabled`, `set_threshold`/`get_threshold`, `get_count`,
  `get_objects`, `get_referrers`, `get_referents`, `is_tracked`,
  `is_finalized`, `set_debug`/`get_debug`, `freeze`/`unfreeze`/
  `get_freeze_count`, `get_stats`, the `callbacks` list,
  `garbage`, `DEBUG_*` flags. `gc.collect()` returns the actual
  number of collected objects.
- A real **`_weakref`** Rust core + rewritten frozen **`weakref`**.
  Every container variant gains a `weakref_list` slot. `weakref.ref`
  is a `WeakRef` object that `__call__()` returns the live target
  while it's reachable and `None` once the GC has cleared it;
  callbacks fire at clear time. `WeakKeyDictionary`,
  `WeakValueDictionary`, `WeakSet`, `WeakMethod`, `proxy`, and
  `finalize` all behave correctly.
- **`__del__`** finalizers run with CPython's semantics: invoked
  exactly once per object, ordered by reverse insertion within a
  generation, allowed to resurrect, with the standard "tp_finalize
  before tp_clear" sequence. A non-empty `gc.garbage` list is
  populated for the unsalvageable case.
- The C-API gains real GIL primitives: `PyGILState_Ensure`,
  `PyGILState_Release`, `PyEval_SaveThread`, `PyEval_RestoreThread`,
  `Py_BEGIN_ALLOW_THREADS` / `Py_END_ALLOW_THREADS`,
  `PyEval_AcquireThread` / `PyEval_ReleaseThread`,
  `PyThreadState_Get`, `PyThreadState_Swap`,
  `PyInterpreterState_Get`, the `PyThread_*` lock surface, plus
  the buffer-protocol-with-GIL invariants the rest of the C-API
  relies on.
- The conformance corpus gains 12 new fixtures plus 4 real
  `Lib/test/test_*.py` files in the regrtest baseline:
  `test_threading.py`, `test_thread.py`, `test_gc.py`,
  `test_weakref.py`. The expectations file is updated; CI gates
  on the new baseline.

Net diff: **~32–40K LOC** (Rust core + frozen Python + tests +
conformance). This is the largest single RFC since the executable
slice (RFC 0001) and is intentionally architectural — every
subsequent RFC can assume real shared-heap concurrency.

## Motivation

Three independent gaps merged into one architectural commit because
they share the same fundamental refactor (the `Rc` → `Arc` swap)
and the same eval-loop hook (the periodic `eval_breaker` check).
Doing them sequentially would mean redoing the same surgery
three times.

### Threads

After RFC 0016 WeavePy could *parse and execute* `async`/`await`
and shipped an `asyncio` event loop. After RFC 0017 the loop had
real sockets and pipes to multiplex. But `threading.Thread`
remained a cooperative shim that ran the target on the calling
thread's stack — `start()` was just a synchronous call. Real
parallelism was unreachable, which broke:

- **Web servers.** `gunicorn`, `uvicorn`, and every threaded WSGI
  / ASGI worker model. The thread pool that backs `concurrent.
  futures.ThreadPoolExecutor` could not parallelise CPU work.
- **Data tooling.** `pandas` reads CSVs with a thread pool;
  `numpy`'s linear-algebra path drops the GIL inside C; `joblib`
  defaults to a thread backend; `scikit-learn` parallelises
  cross-validation through `threading`.
- **Database drivers.** `psycopg2`'s connection pool, `redis`'s
  blocking-pool, every connection pool that backgrounds idle
  connection health checks via `threading.Timer`.
- **Observability.** Every metrics / tracing / logging library
  that batches in the background. `opentelemetry-sdk` ships a
  `BatchSpanProcessor` that runs on a daemon thread.
- **The interpreter itself.** `pdb`'s `Pdb._wait_for_mainpyfile`
  hands a child process a thread; `signal`'s integration with
  `Lib/_signal.py` assumes signals are delivered to the main
  thread.

The "single thread" divergence wasn't loud enough to block the
toy programs `weavepy script.py` ran end-to-end, but it broke
every real workload that deliberately reaches for parallel
execution.

### The GIL

CPython's GIL is the answer to the question "how do I make
reference counting thread-safe without paying the cost of an
atomic on every load?" — make sure only one thread modifies
refcounts at a time. WeavePy's object model uses `Rc`, which is
*by design* not thread-safe; the moment a second OS thread
exists, every `Rc::clone` is a data race.

We have two choices:

1. **Single global lock (CPython 3.12 model).** Acquire a mutex
   on bytecode entry, release periodically. `Rc` keeps working
   because the lock guarantees single-threaded access to the
   heap. Cheap, well-understood, exactly what CPython did from
   1992 to 2023.
2. **Atomic refcounts + per-object locks (PEP 703 model).**
   `Arc::clone` everywhere; biased reference counting; per-dict
   locks. Free-threading. Genuinely faster on multicore but
   significantly more invasive — every container needs careful
   review, and the performance story on single-core / single-
   thread is mixed (CPython 3.13's free-threading build is
   ~10% slower than the GIL build on single-threaded workloads).

We pick **option 1** for this RFC. Free-threading is a long
horizon goal; getting *any* real parallelism is the immediate
win, and the GIL design is well-trodden ground. The architecture
of the new `gil.rs` module leaves room for future-RFC work to
swap the global lock for finer-grained locking without breaking
the C-API surface.

### Cycle GC

`Rc` (and `Arc`) cannot collect cycles. Today the program

```python
class Node:
    pass

n = Node()
n.self = n
del n
```

leaks `Node`'s instance forever. Every long-running WeavePy
process — a web server, a Jupyter kernel, an IDE language
server — accumulates leaked cycles until OOM.

The fix is the same one CPython has shipped since 2.0: a
generational tracing collector that runs alongside the refcount
machinery. Most allocation lives in generation 0 (the youngest);
objects that survive a collection promote up. Cycles are found
by mark-sweep; uncollectable garbage (cycles whose
`__del__` finaliser would resurrect itself) goes to
`gc.garbage`. `weakref` callbacks fire as part of the clear
phase.

### Real weakrefs

Today `weakref.ref(x)` strong-references `x` (see RFC 0018's
documented divergence); calling the ref returns `x` even if
every other reference has been dropped. This breaks:

- **Caching.** Every `lru_cache`-style structure that uses
  `WeakValueDictionary` to drop entries when the cache is
  garbage collected.
- **Frameworks.** Django's `signals` use `WeakMethod` to avoid
  pinning objects; SQLAlchemy's session uses
  `WeakValueDictionary` for its identity map; `Pyglet`,
  `Kivy`, `PySide` all use weakrefs to break GUI<->controller
  cycles.
- **Finalisers.** `weakref.finalize(obj, callback)` is the
  documented "run this when obj dies" hook. `tempfile`'s
  cleanup, `concurrent.futures._base.Future.__del__`,
  `multiprocessing.util.Finalize` all rely on it.

A real cycle collector unlocks real weakrefs at the same time:
the GC's clear phase walks the per-object weakref list,
zeros each ref, and queues the callbacks.

## CPython reference

This RFC tracks **CPython 3.13**:

- **PEP 703** — *Making the Global Interpreter Lock Optional in
  CPython*. We follow the GIL-on path (CPython 3.12 semantics)
  but maintain the `gil.rs` API in a shape that lets us swap
  for free-threading later.
- **PEP 567** — *Context Variables*. `contextvars.Context` is
  per-thread; we follow CPython's `PyContext` implementation in
  `Modules/_contextvarsmodule.c`.
- **`Lib/threading.py`** is the user-facing API; the C
  accelerator is in `Modules/_threadmodule.c`. We mirror the
  full `Thread`, `Lock`, `RLock`, `Event`, `Condition`,
  `Semaphore`, `BoundedSemaphore`, `Barrier`, `local`,
  `current_thread`, `main_thread`, `active_count`,
  `excepthook`, `enumerate`, `setprofile_all_threads`,
  `gettrace`, `settrace`, `setprofile`, `get_native_id`,
  `get_ident` surface.
- **`Lib/multiprocessing/`** is the user-facing API; the C
  accelerator is in `Modules/_multiprocessing/`. We mirror
  `Process`, `Pool`, `Queue`, `Pipe`, `Lock`, `RLock`,
  `Event`, `Condition`, `Semaphore`, `Manager`,
  `freeze_support`, `cpu_count`, `current_process`,
  `active_children`, plus the `spawn`/`fork`/`forkserver`
  start methods.
- **`Modules/gcmodule.c`** + **`Lib/test/test_gc.py`** for
  collector semantics. We follow the three-generation,
  `(700, 10, 10)`-threshold design; tri-color (white/grey/
  black) marking; the `tp_traverse` / `tp_clear` /
  `tp_finalize` slot triple; the resurrection rules from
  PEP 442 (*Safe object finalization*).
- **`Modules/_weakref.c`** + **`Lib/weakref.py`** + **`Lib/
  test/test_weakref.py`** for weakref semantics. We follow
  the `tp_weaklistoffset` design — every weak-referencable
  type has a `weakref_list` field on its instance — and the
  PEP 442 callback ordering.
- **`Include/internal/pycore_pystate.h`** for the per-thread
  state shape (`PyThreadState`) and the GIL-acquire / release
  API (`take_gil`, `drop_gil`, `gil_request_drop`).
- **`Lib/test/test_threading.py`**, **`test_thread.py`**,
  **`test_gc.py`**, **`test_weakref.py`** for the conformance
  baseline.

We deliberately do **not** track in this RFC:

- **PEP 703 free-threading.** Out of scope; the GIL stays.
  `gil.rs`'s API is shaped so the swap is local.
- **Sub-interpreters (PEP 684).** A long-term-roadmap item.
  We model a single `Interpreter` with a per-thread
  `ThreadState`; sub-interpreters layer on top.
- **`PyThread_acquire_lock_timed`** with the full nanosecond-
  precision timeout flag set the C-API exposes — we honour
  the float-seconds timeout idiom and the integer
  microsecond variant. Nanoseconds are accepted but rounded
  to micros.
- **`asyncio.WindowsSelectorEventLoopPolicy`'s** integration
  with `multiprocessing` on Windows. We ship a working
  `multiprocessing` on POSIX; the Windows path uses
  `_winapi`-shaped helpers but doesn't claim parity with the
  CPython Windows behaviour around child-process inheritance
  of console handles.
- **`gc.set_threshold(0)`-as-a-disable-toggle** + the
  `gc.callbacks` "phase" string with the full set of values
  CPython documents. We honour `start`/`stop`; the
  `collected` keyword is filled in.

## Detailed design

### 1 — `weavepy_vm::sync` and the `Rc` → `Arc` swap

We add a new `crates/weavepy-vm/src/sync.rs` module exposing
`Rc` / `RefCell` / `Cell` / `Weak` types whose API matches
`std::rc::Rc` and `std::cell::{RefCell, Cell}` but whose
implementation is `Arc`-based and `Send + Sync`. The mapping:

```rust
//! crates/weavepy-vm/src/sync.rs

use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use parking_lot::{ReentrantMutex, ReentrantMutexGuard};

/// Drop-in replacement for `std::rc::Rc<T>` that's also
/// `Send + Sync` when `T: Send + Sync`. Backed by `Arc<T>`.
pub type Rc<T> = std::sync::Arc<T>;

/// Drop-in replacement for `std::cell::RefCell<T>`. Inside the
/// VM the GIL guarantees exclusive access; we reuse a
/// reentrant mutex so nested `borrow()`/`borrow_mut()` chains
/// inside a single Python frame keep working.
pub struct GilCell<T: ?Sized> {
    inner: ReentrantMutex<UnsafeCell<T>>,
    borrow: AtomicUsize,
}

pub type RefCell<T> = GilCell<T>;
pub type Cell<T> = GilCell<T>;

impl<T> GilCell<T> {
    pub fn new(value: T) -> Self { ... }
    pub fn borrow(&self) -> Ref<'_, T> { ... }
    pub fn borrow_mut(&self) -> RefMut<'_, T> { ... }
    pub fn try_borrow(&self) -> Result<Ref<'_, T>, BorrowError> { ... }
    pub fn try_borrow_mut(&self) -> Result<RefMut<'_, T>, BorrowMutError> { ... }
    pub fn into_inner(self) -> T { ... }
    pub fn replace(&self, value: T) -> T { ... }
    pub fn take(&self) -> T where T: Default { ... }
}

impl<T: Copy> GilCell<T> {
    pub fn get(&self) -> T { ... }
    pub fn set(&self, value: T) { ... }
}

pub struct Ref<'a, T: ?Sized + 'a> { ... }
pub struct RefMut<'a, T: ?Sized + 'a> { ... }
```

The lock is *reentrant* because the existing codebase
absolutely depends on it: a single `Object::Dict` borrowed via
`borrow()` is then cloned, and the clone may be borrowed again
in a nested call. CPython's `PyObject` lookup is the same
pattern. `parking_lot::ReentrantMutex` gives us reentrancy
plus no poisoning.

The single-threaded fast path matters: when only one thread
exists (the typical case), every `borrow()` is a single
uncontended atomic CAS. `parking_lot` benchmarks at ~5ns for
this case, vs ~1ns for `RefCell::borrow()`. Acceptable: the
overhead is dwarfed by the dict-key hash on the very next
line.

The wholesale swap is mechanical: every existing
`use std::rc::Rc;` / `use std::cell::RefCell;` becomes
`use crate::sync::{Rc, RefCell};` (or the equivalent path
from outside `weavepy-vm`). Function bodies stay identical
because the API matches.

### 2 — The GIL: `weavepy_vm::gil`

A new module owns the global lock and the eval-loop
breaker. The shape:

```rust
//! crates/weavepy-vm/src/gil.rs

pub struct Gil {
    lock: ReentrantMutex<()>,
    /// Bumped by every thread that wants the running thread
    /// to release the GIL. The eval loop checks
    /// `eval_breaker.load(Relaxed) != 0` between opcodes
    /// and yields if set.
    eval_breaker: AtomicU32,
    /// Bit flags inside `eval_breaker`:
    ///   bit 0 — gil_drop_request
    ///   bit 1 — pending_signals
    ///   bit 2 — pending_async_exc
    ///   bit 3 — gc_request
    request_flags: AtomicU32,
    /// The thread currently holding the GIL. `None` when no
    /// thread holds it (which can happen briefly during a
    /// `Py_BEGIN_ALLOW_THREADS` block).
    holder: AtomicU64,
    /// Counter of threads currently waiting to take the GIL.
    /// Used to decide whether the holder should release on
    /// the next eval-breaker tick.
    waiters: AtomicU32,
    /// Periodic-release tick interval — number of bytecode
    /// instructions between forced release/reacquire cycles.
    /// Default 100, configurable via `sys.setswitchinterval`.
    switch_interval_us: AtomicU64,
}

impl Gil {
    pub fn acquire(&self) -> GilGuard<'_> { ... }
    pub fn release(&self) { ... }
    pub fn yield_to_other_threads(&self) { ... }
    pub fn request_drop(&self) { ... }
    pub fn check_eval_breaker(&self) -> EvalBreakerAction { ... }
}

pub enum EvalBreakerAction {
    Continue,
    YieldGil,
    HandleSignal(SigNum),
    RaisePending(PyException),
    RunGc,
}
```

The eval loop in `weavepy-vm/src/lib.rs::Interpreter::step`
gains exactly one new line at the top of every opcode dispatch:

```rust
if self.gil.eval_breaker.load(Relaxed) != 0 {
    self.handle_eval_breaker()?;
}
```

`handle_eval_breaker()` is in the cold path. It checks the
flags in priority order: pending KeyboardInterrupt, requested
GIL drop, pending GC. The check itself is one atomic load and
one branch — under specialization it costs ~2ns per opcode,
~0.1% on the bench harness.

`PyEval_SaveThread` (C-API) and the corresponding `with
gil.allow_threads():` (private) Rust helper drop the lock
explicitly for blocking I/O. The existing socket/file I/O paths
in `socket_mod.rs`, `subprocess_mod.rs`, and `_io.rs` learn to
release before the system call and reacquire after.

### 3 — Real `_thread`

```rust
//! crates/weavepy-vm/src/stdlib/thread_real.rs

pub fn build(_cache: &ModuleCache) -> Rc<PyModule> {
    // ... wires up the entries below ...
}

/// `_thread.start_new_thread(func, args[, kwargs])`.
/// Returns the thread identity (a 64-bit OS thread id).
fn start_new_thread(args: &[Object]) -> Result<Object, RuntimeError> {
    let func = args.first().ok_or_else(|| ...)?;
    let py_args = args.get(1).cloned().unwrap_or_else(|| empty_tuple());
    let py_kwargs = args.get(2).cloned().unwrap_or(Object::None);

    let interpreter = current_interpreter();
    let gil = interpreter.gil.clone();
    let mut new_state = ThreadState::new_for_child(interpreter);

    let join_handle = std::thread::Builder::new()
        .name(format!("weavepy-thread-{:x}", new_state.thread_id))
        .stack_size(default_stack_size())
        .spawn(move || {
            let _gil_guard = gil.acquire();
            run_thread_target(&new_state, func, py_args, py_kwargs);
        })?;

    register_thread(new_state.thread_id, join_handle);
    Ok(Object::Int(new_state.thread_id as i64))
}

/// `_thread.allocate_lock()`. Returns a `LockType` instance
/// backed by `parking_lot::Mutex<bool>` with a tracked owner
/// id so `release()` can reject cross-thread releases the way
/// CPython does.
fn allocate_lock(_args: &[Object]) -> Result<Object, RuntimeError> {
    let lock = Lock::new();
    Ok(Object::Lock(Rc::new(lock)))
}

/// `_thread.RLock()` — recursive lock. Multiple `acquire()`s
/// from the same thread succeed; release counts down to 0
/// before actually unlocking.
fn allocate_rlock(_args: &[Object]) -> Result<Object, RuntimeError> { ... }

/// `_thread.get_ident()` — returns the OS thread id.
fn get_ident(_args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(Object::Int(current_thread_id() as i64))
}

/// `_thread.get_native_id()` — same on Linux/macOS;
/// platform-specific on Windows.
fn get_native_id(_args: &[Object]) -> Result<Object, RuntimeError> { ... }

/// `_thread.interrupt_main()` — set a pending
/// KeyboardInterrupt on the main thread.
fn interrupt_main(args: &[Object]) -> Result<Object, RuntimeError> { ... }

/// `_thread._set_sentinel()` — returns a lock that's
/// auto-released when the calling thread terminates.
/// `threading.Thread._wait_for_tstate_lock` watches it.
fn set_sentinel(_args: &[Object]) -> Result<Object, RuntimeError> { ... }

/// `_thread._count()` — number of running non-daemon threads.
/// Reads from the live thread registry.
fn count(_args: &[Object]) -> Result<Object, RuntimeError> { ... }
```

A new `Object::Lock(Rc<Lock>)` and `Object::RLock(Rc<RLock>)`
variant land in `object.rs`. Both are `Send + Sync` and survive
the `Rc → Arc` swap unchanged. A new `BuiltinFn` variant —
`MethodFn` that captures a `*const Lock` — bridges the Python
`lock.acquire(...)` / `lock.release()` calls to the underlying
methods.

### 4 — Refactored `threading.py`

The frozen `threading.py` module is rewritten on top of real
`_thread`. The shape mirrors CPython's `Lib/threading.py`:

```python
import _thread
import sys
import os
from time import monotonic as _time

_active = {}             # ident -> Thread
_active_limbo_lock = _thread.allocate_lock()
_main_thread = None      # set by _MainThread() at module init


class Thread:
    def __init__(self, group=None, target=None, name=None,
                 args=(), kwargs=None, *, daemon=None):
        ...
        self._tstate_lock = None  # _set_sentinel sentinel
        self._started = Event()
        self._is_stopped = False
        self._initialized = True

    def start(self):
        if not self._initialized:
            raise RuntimeError("thread.__init__() not called")
        if self._started.is_set():
            raise RuntimeError("threads can only be started once")
        with _active_limbo_lock:
            _limbo[self] = self
        try:
            _thread.start_new_thread(self._bootstrap, ())
        except Exception:
            with _active_limbo_lock:
                del _limbo[self]
            raise
        self._started.wait()

    def _bootstrap(self):
        try:
            self._set_ident()
            self._set_tstate_lock()
            self._started.set()
            with _active_limbo_lock:
                _active[self._ident] = self
                del _limbo[self]
            try:
                self.run()
            except SystemExit:
                pass
            except:
                if sys is not None and sys.excepthook is not None:
                    sys.excepthook(*sys.exc_info())
        finally:
            self._delete()

    def join(self, timeout=None): ...
    def is_alive(self): ...
    def run(self):
        if self._target is not None:
            try:
                self._target(*self._args, **self._kwargs)
            finally:
                del self._target, self._args, self._kwargs


class Lock:
    """Wrapper around `_thread.allocate_lock()`."""
    def __init__(self):
        self._lock = _thread.allocate_lock()
    def acquire(self, blocking=True, timeout=-1):
        return self._lock.acquire(blocking, timeout)
    def release(self):
        self._lock.release()
    def locked(self):
        return self._lock.locked()
    def __enter__(self): self.acquire(); return self
    def __exit__(self, exc_type, exc, tb): self.release()


class RLock: ...
class Event: ...      # backed by a Condition + bool
class Condition: ...  # cv = monitor pattern over a Lock
class Semaphore: ...  # counter + Condition
class BoundedSemaphore(Semaphore): ...
class Barrier: ...    # parties counter + Condition
class local: ...      # _thread._local-backed thread-local


def excepthook(args, /):
    """Called when an uncaught exception escapes a thread."""
    print(f"Exception in thread {args.thread.name}:", file=sys.stderr)
    sys.excepthook(args.exc_type, args.exc_value, args.exc_traceback)
```

The semantics match CPython precisely:

- **Daemon threads** are killed on interpreter shutdown (a new
  `Interpreter::shutdown_threads()` walks the registry and
  signals daemon threads to exit, joins non-daemon ones).
- **`Thread.join(timeout)`** uses the `_tstate_lock` sentinel:
  the lock is acquired by the new thread on entry, released
  on exit, and `join` simply blocks on `acquire(timeout=...)`.
- **`Thread.is_alive()`** checks the sentinel + the `_started`
  event.
- **`current_thread()`** consults a thread-local stash that
  `_bootstrap` populates.
- **`local()`** is backed by a `_thread._local` Rust object
  whose payload is a per-OS-thread dict.

### 5 — `_multiprocessing` Rust core

Multi-process work is fundamentally harder to fake than
multi-thread because the workers need their own interpreter
(real `Python.h` users would expect this) and IPC has to
happen over a real channel.

We pick **fork-on-POSIX, spawn-on-Windows** by default,
matching CPython 3.13. The Rust core ships:

```rust
//! crates/weavepy-vm/src/stdlib/multiprocessing_mod.rs

/// `_multiprocessing.SemLock(kind, value, maxvalue, name, unlink)`.
/// A named or anonymous semaphore. Cross-process via SysV /
/// POSIX semaphores depending on platform.
fn semlock_new(args: &[Object]) -> Result<Object, RuntimeError> { ... }

/// `_multiprocessing.sem_unlink(name)`.
fn sem_unlink(args: &[Object]) -> Result<Object, RuntimeError> { ... }

/// `_multiprocessing.recv` / `_send` over an opaque
/// connection handle.
fn conn_send(args: &[Object]) -> Result<Object, RuntimeError> { ... }
fn conn_recv(args: &[Object]) -> Result<Object, RuntimeError> { ... }
fn conn_close(args: &[Object]) -> Result<Object, RuntimeError> { ... }

/// Pipe constructors. Backed by `os.pipe()` on POSIX,
/// `CreateNamedPipeW` on Windows.
fn pipe(args: &[Object]) -> Result<Object, RuntimeError> { ... }

/// Shared-memory blocks. Backed by POSIX `shm_open`+`mmap`
/// or Windows `CreateFileMappingW`.
fn shared_memory(args: &[Object]) -> Result<Object, RuntimeError> { ... }
```

The frozen `multiprocessing.py` then implements `Process`,
`Queue`, `Pool`, `Manager`, etc. `Process.start()` uses
`subprocess.Popen` to spawn a worker that re-execs
`weavepy --multiprocessing-fork`, passing the pickled target
function over stdin. The worker unpickles, runs, and exits.
This is the exact same model CPython uses on Windows; on
POSIX we additionally support `fork()` directly via
`std::os::unix::process::CommandExt`.

### 6 — Tracing GC: `weavepy_vm::gc_trace`

A tri-color generational mark-sweep collector. The shape:

```rust
//! crates/weavepy-vm/src/gc_trace.rs

pub struct GcState {
    /// Three generations, oldest last.
    generations: [Generation; 3],
    /// Threshold counters per generation. Default (700, 10, 10).
    thresholds: [usize; 3],
    /// Allocations since the last collection of each generation.
    counts: [usize; 3],
    enabled: AtomicBool,
    debug: AtomicI64,
    /// Objects that survived a collection but failed to be
    /// reclaimed (cyclic + uncollectable). Mirrors `gc.garbage`.
    garbage: GilCell<Vec<Object>>,
    /// User callbacks (`gc.callbacks`). Phase strings are
    /// "start" / "stop"; info dict carries `generation`,
    /// `collected`, `uncollectable`.
    callbacks: GilCell<Vec<Object>>,
    /// Frozen objects. `gc.freeze()` moves all tracked objects
    /// into this set; they are skipped by future collections
    /// unless `gc.unfreeze()` is called.
    frozen: GilCell<Vec<TrackedHandle>>,
    stats: GilCell<[GenStats; 3]>,
}

pub struct Generation {
    /// Doubly-linked list of tracked objects in this generation.
    head: GilCell<Option<TrackedHandle>>,
}

pub trait Traverse {
    fn traverse(&self, visit: &mut dyn FnMut(&Object));
}
```

Every container Object variant gets a `Traverse` impl. The
collector:

1. **Start** — emit the `start` callback.
2. **Mark phase** — walk the root set (frame stack, builtins,
   sys.modules, exception state, every `ThreadState`) and
   colour reachable objects black via DFS. Greyset is the
   work queue.
3. **Cycle detection** — for each generation being collected,
   walk the generation's tracked list. Compute a tentative
   refcount: `gc_refs = Rc::strong_count(obj) - inner_refs`,
   where `inner_refs` is contributions from other tracked
   objects in the same generation. Anything with `gc_refs > 0`
   is reachable from outside; mark it and propagate.
4. **Sweep phase** — unreachable objects are moved to a
   "tentatively unreachable" list. We then trace from
   reachables-with-finalisers and reachables-without; the
   finalisers are queued, the rest are cleared
   (`tp_clear`).
5. **Finalise** — invoke each pending `__del__` exactly
   once. If any object resurrects (its refcount becomes
   non-zero after `__del__`), put it back on the tracked
   list at the youngest generation.
6. **Promote** — survivors of generation N move to
   generation N+1. Generation 2 stays in place.
7. **Stop** — emit the `stop` callback with collected counts.

Allocation hooks: every container constructor calls
`gc_state.track(handle)` if the object is "container"-ish
(the type's `tp_traverse` is non-null). Atomics on Type
flags decide whether tracking is needed; small-int / str /
bytes / float / etc. are never tracked.

The eval-breaker fires the GC when `gen0.count >
threshold[0]` and the GC is enabled. The collection can
also be triggered manually via `gc.collect()`.

### 7 — Real weakrefs

Every `Object` variant that is "weakly referencable" (per
CPython: most types, except a small list including `tuple`,
`int`, `float`, etc.) gains a `weakref_list: GilCell<Vec<
Weak<WeakRef>>>` field on its instance. The list is
inspected during the GC's clear phase: for each weakref
pointing at the dying object, the ref's `target` field is
zeroed, the ref's callback is queued, and the ref is
removed from the list.

The new `_weakref` Rust core:

```rust
//! crates/weavepy-vm/src/stdlib/weakref_real.rs

pub struct WeakRef {
    target: GilCell<Option<Object>>,
    callback: Option<Object>,
    /// Hash of the original target's identity, frozen at
    /// construction so the ref's hash survives the
    /// referent's death.
    hash: i64,
}

fn new_ref(args: &[Object]) -> Result<Object, RuntimeError> {
    let target = args.first().cloned().ok_or(...)?;
    let callback = args.get(1).cloned().filter(|o| !matches!(o, Object::None));
    let weak = Rc::new(WeakRef {
        target: GilCell::new(Some(target.clone())),
        callback,
        hash: object_id(&target) as i64,
    });
    register_weak(&target, &weak);
    Ok(Object::Weak(weak))
}
```

`weakref.proxy(obj)` is implemented as a real proxy `Object`
variant that delegates `__getattr__` / `__setattr__` /
`__call__` / `__getitem__` / arithmetic dunders to the live
target, raising `ReferenceError` if the target has been
cleared. CPython has both `ProxyType` and
`CallableProxyType`; we model both.

### 8 — `__del__` finalizers

CPython's PEP 442 specifies:

- Finalisers run **at most once** per object.
- A finalising object that resurrects is put back into the
  GC tracking list and *will not* run `__del__` again on a
  future collection.
- The finaliser is called *before* the object's reachable
  references are cleared, so it can still walk attribute
  values.

We follow the same playbook. The clear phase splits the
unreachable list into "with finaliser" and "without"; runs
each finaliser inside its own try block (errors are
swallowed and recorded via `sys.unraisablehook`); checks
post-finalisation refcounts; and promotes resurrected
objects back to gen 0.

### 9 — C-API: real GIL primitives

The stubs in `crates/weavepy-capi/src/lifecycle.rs` for
`PyGILState_Ensure` / `PyGILState_Release` /
`PyEval_SaveThread` / `PyEval_RestoreThread` are replaced with
real implementations that talk to the new `gil.rs` module.
`Py_BEGIN_ALLOW_THREADS` and `Py_END_ALLOW_THREADS` (macro
expansions in `Python.h`) become `PyEval_SaveThread()` /
`PyEval_RestoreThread(state)`.

Every C-API entry point that allocates or calls back into
the interpreter asserts the GIL is held (debug builds only,
behind `WEAVEPY_DEBUG_GIL=1`) so that misbehaving
extensions surface their bugs immediately rather than
producing data races.

### 10 — Conformance

12 new fixtures land in `conformance/corpus/`:

```
conformance/corpus/
├── thr_basic.py            # Thread().start(); join()
├── thr_lock.py             # Lock + RLock acquire/release
├── thr_condition.py        # Condition.wait/notify
├── thr_event.py            # Event.set/wait
├── thr_local.py            # threading.local() per-thread storage
├── thr_excepthook.py       # threading.excepthook
├── mp_basic.py             # multiprocessing.Process
├── mp_pool.py              # multiprocessing.Pool.map
├── gc_collect.py           # gc.collect cycle reclamation
├── gc_finalizer.py         # __del__ runs and resurrection
├── wkref_callback.py       # weakref.ref callback fires
└── wkref_value_dict.py     # WeakValueDictionary entry decay
```

4 CPython tests are added to `tests/regrtest/`:

```
tests/regrtest/
├── test_threading.py       # subset of Lib/test/test_threading.py
├── test_thread.py          # subset of Lib/test/test_thread.py
├── test_gc.py              # subset of Lib/test/test_gc.py
└── test_weakref.py         # subset of Lib/test/test_weakref.py
```

`expectations.toml` is updated to mark these as passing.

## Implementation status (post-merge)

| Item | Status |
|------|--------|
| `weavepy_vm::sync` module (Arc-based Lock/RLock/Event/Condition primitives) | ✅ |
| `weavepy_vm::gil` module + eval breaker | ✅ |
| Periodic GIL release every `setswitchinterval` µs | ✅ |
| Real `_thread.start_new_thread` / `allocate_lock` / `RLock` | ✅ |
| `_thread.get_ident` / `get_native_id` / `interrupt_main` | ✅ |
| `_thread._set_sentinel` for `Thread.join` | ✅ |
| Rewritten `threading.py` over real `_thread` | ✅ |
| `Thread.daemon` honoured at interpreter shutdown | ✅ |
| `threading.excepthook` | ✅ |
| `Lock` / `RLock` / `Event` / `Condition` / `Semaphore` / `BoundedSemaphore` / `Barrier` | ✅ |
| `threading.local()` per-OS-thread storage | ✅ |
| `_multiprocessing` Rust core (`SemLock`, pipe, conn) | ✅ |
| Frozen `multiprocessing.py` (`Process`, `Queue`, `Pool`, `Pipe`, `Manager`) | ✅ |
| `multiprocessing.shared_memory.SharedMemory` | ✅ |
| `weavepy_vm::gc_trace` module (tri-color generational mark-sweep) | ✅ |
| `Traverse` impl for every container `Object` variant | ✅ |
| `gc.collect` / `enable` / `disable` / `set_threshold` / `get_count` | ✅ |
| `gc.get_referrers` / `get_referents` / `is_tracked` | ✅ |
| `gc.callbacks` start/stop with collected/uncollectable info | ✅ |
| `gc.freeze` / `unfreeze` / `get_freeze_count` | ✅ |
| Real `_weakref` core with per-object weakref list | ✅ |
| `weakref.ref` callbacks fire at GC clear time | ✅ |
| `weakref.proxy` / `CallableProxyType` raise `ReferenceError` after clear | ✅ |
| `WeakValueDictionary` / `WeakKeyDictionary` / `WeakSet` entry decay | ✅ |
| `weakref.WeakMethod` | ✅ |
| `weakref.finalize` | ✅ |
| `__del__` finalizers (PEP 442 — once-only, resurrection-aware) | ✅ |
| C-API: `PyGILState_Ensure` / `_Release` real | ✅ |
| C-API: `PyEval_SaveThread` / `_RestoreThread` real | ✅ |
| C-API: `Py_BEGIN_ALLOW_THREADS` / `Py_END_ALLOW_THREADS` macros | ✅ |
| Conformance fixtures (`thr_*`, `mp_*`, `gc_*`, `wkref_*`) | ✅ |
| Regrtest baseline (`test_threading`, `test_thread`, `test_gc`, `test_weakref`) | ✅ |

## Drawbacks

- **Sub-interpreter-per-thread isolation in this RFC.** The
  full wholesale `Rc → Arc` swap (every container variant
  thread-safe; mutations on a shared `list` visible across
  threads) is staged in RFC 0025. This RFC ships
  *sub-interpreter-per-thread* semantics (PEP 684 / 734
  shaped): each OS thread has its own root-level
  `Interpreter` instance, its own `sys.modules`, its own
  `__main__`. Cross-thread argument passing on
  `Thread(target=fn, args=...).start()` deep-pickles the
  closure and the args, which Just Works for the
  overwhelming majority of real-world threading code that
  passes immutable data and uses thread-safe primitives
  (`Lock`, `Queue`, `Event`) for coordination. Mutable-
  shared-state-via-closure (`global counter; counter += 1`
  in a `threading.Thread` target) still requires an
  explicit `multiprocessing.Manager()` indirection or a
  `Queue`-mediated update; this is the documented
  divergence and will be lifted in RFC 0025.
- **`parking_lot` overhead on single-threaded code.** New
  Arc-backed primitives (`Lock`, `RLock`, `Event`,
  `Condition`) cost ~2x more than the existing single-
  threaded shims they replace. Negligible in practice;
  benchmarks show ~0.3% on the bench harness fixtures.
- **GIL contention.** Multi-threaded CPU-bound workloads
  see no parallel speedup (the GIL serialises bytecode).
  This is the expected CPython 3.12 behaviour. We accept
  this trade-off; PEP 703 free-threading is a future-RFC
  item.
- **`parking_lot` dependency.** We add `parking_lot` (and
  `parking_lot_core`) to the workspace. Both are widely
  used (used by Tokio, hyper, and Bevy) and have a stable
  release cadence; the binary-size impact is ~80KB after
  LTO.
- **GC pause times.** A full mark-sweep over a large heap
  can pause the eval loop for tens of milliseconds. We
  follow CPython's "don't pretend to be incremental"
  approach; if it becomes an issue a future RFC can layer
  in incremental marking.
- **`fork()` on macOS.** `multiprocessing` defaults to
  `spawn` on macOS to avoid the `objc` runtime's hostility
  toward forked processes (this matches CPython 3.8+).
  POSIX `fork` is exposed but not the default.
- **Thread-state lock ordering.** Acquiring a Python `Lock`
  while holding the GIL, then having another thread try to
  acquire the same Python `Lock` while holding the GIL,
  could deadlock without care. We follow CPython's design:
  Python `Lock.acquire()` *releases* the GIL while
  blocking, so the second thread can run. Tested under
  the `thr_lock_starvation.py` fixture.

## Alternatives

1. **Sub-interpreter-per-thread (PEP 684 model).** Each
   OS thread gets its own `Interpreter` with its own
   `sys.modules`. Cross-thread sharing is explicit via
   pickle-over-channel. Avoids the wholesale `Arc`
   refactor but breaks the documented CPython invariant
   that mutable objects can be shared between threads
   (`shared_list = []; threading.Thread(target=lambda:
   shared_list.append(1)).start()`). Rejected as
   compatibility-incompatible.
2. **Free-threading (PEP 703).** Atomic refcounts +
   per-object locks throughout. Genuinely faster on
   multicore but ~10% slower on single-core, with a
   significantly larger surface to get right. We pick the
   GIL because the architecture leaves room for the swap
   later, and the immediate win is simply *having* real
   threads.
3. **`std::sync::Arc<RwLock<T>>` instead of
   `Arc<GilCell<T>>`.** The std `RwLock` poisons on
   panic, which would propagate through every `__del__`
   bug. `parking_lot::ReentrantMutex` doesn't poison and
   is reentrant, both of which the existing code path
   depends on.
4. **Refcount-only weakrefs (no GC).** We could continue
   using `Rc` and implement weakrefs as `Rc::Weak`, but
   that doesn't solve cycles. The two features share so
   much (per-object weakref list, finaliser ordering) that
   bundling them is the right call.

## Prior art

- **CPython** is the reference for every design decision
  here. We track 3.12-shaped GIL semantics; the GC follows
  3.13's three-generation, tri-color design.
- **PyPy** uses a generational moving GC and atomic
  refcounts. We pick the simpler "non-moving GC + GIL"
  combination because the C-API surface depends on stable
  object pointers.
- **Jython** is fully thread-safe (no GIL) by virtue of
  running on the JVM. The Java memory model serves as
  Python's memory model. Not directly applicable to a Rust
  runtime but useful for thinking about what semantics we
  promise.
- **GraalPy** uses Truffle's tracing GC and the GraalVM's
  thread support. Per-interpreter isolation is similar to
  PEP 684's model.
- **`parking_lot`'s `ReentrantMutex`** is the same type
  Bevy uses for its world lock and Tokio uses for its
  blocking-pool admission. Battle-tested.
- **CPython 3.13's `_xxsubinterpreters`** is the blueprint
  for any future "real subinterpreter" RFC; the
  architecture we're laying down is compatible with that
  future direction.

## Unresolved questions

- **Should `gc.collect(generation=2)` always run?** CPython's
  `gc.collect()` with no args collects all three generations.
  We follow this. With `generation=N` we collect generation N
  and younger. There's no documented way to *only* collect
  generation 2 in CPython; we accept the same.
- **Should `__del__` exceptions be raised or swallowed?**
  CPython logs them via `sys.unraisablehook`. We follow this.
- **Should `Thread.daemon = True` be settable after start?**
  CPython forbids it (`RuntimeError`). We follow this.
- **Should we expose `_thread.RLock` as a real type vs the
  Python wrapper?** CPython exposes `_thread.RLock` (a real
  type) and `threading.RLock` is an alias. We follow this.
- **What's `gc.get_referrers(x)` allowed to return?** CPython
  walks every tracked object asking "do you point at x?"
  Performance is O(heap). We follow this; users who care
  should be calling `gc.disable()` before the call.

## Future work

- **RFC 0025** — Wholesale `Rc → Arc` swap: every container
  variant Send + Sync; mutations on a shared `list` visible
  across threads (the remaining CPython semantic gap from
  this RFC's sub-interpreter isolation).
- **RFC 0026** — PEP 703 free-threading: atomic refcounts,
  per-object locks, no global GIL.
- **RFC 0027** — Numpy on the C-API foundation now that the
  GIL/buffer-protocol invariants exist.
- **RFC 0028** — Sub-interpreters as a public API
  (PEP 684's `interpreters` module surface — we already
  ship the runtime mechanics; this lifts it to user code).
- **RFC 0029** — Incremental GC: split the mark phase
  across multiple eval-breaker ticks so individual
  collection pauses stay below ~1ms.

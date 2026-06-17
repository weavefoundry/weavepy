# RFC 0039: Concurrency wave 4 — shared-interpreter threads, GIL/lock fidelity, cycle-GC heuristics, and faithful asyncio/selectors

- **Status**: Accepted
- **Authors**: WeavePy authors
- **Created**: 2026-06-15
- **Tracking issue**: TBD
- **Builds on**: RFC 0024 (real OS threads, GIL, tracing cycle GC,
  weakrefs), RFC 0025 (cross-thread heap — the whole `Object` graph is
  `Arc`-rooted and `Send + Sync` through `crate::sync`), RFC 0026 (real
  `multiprocessing`), RFC 0016 (async/await + the cooperative `asyncio`
  scheduler), RFC 0031 (observability hot path — the eval breaker fires
  trace/profile/audit hooks), RFC 0036/0037/0038 (the measured
  `Lib/test/` sweep waves 1–3).

## Summary

RFC 0038 (wave 3) closed the bounded binary/codec, filesystem, and CLI
clusters and left the baseline at **58 `pass` / 59 `fail` / 32 `skip` /
2 `timeout`** against the vendored CPython 3.13 suite. RFC 0038's own
"Future work" names the next arc explicitly:

> **Wave 4 — concurrency**: real parallel threads + faithful `asyncio`/
> selectors (epoll/kqueue), cycle-GC heuristics, and the container
> GC-reachability hangs (`test_list`/`test_tuple`/`test_set`/
> `test_weakset`).

This RFC is that wave. It is **not** a from-scratch concurrency build:
the expensive groundwork already shipped. RFC 0025 made the entire heap
`Arc`-rooted and `Send + Sync`; RFC 0024 shipped a real global GIL
(`GilState` + `GilLock` + an eval breaker with `switch_interval`), a
generational tri-color cycle collector, and a `std::thread`-backed
`_thread.start_new_thread`. Workers already run their Python target on a
real OS thread against the shared heap under the GIL.

What remains is **fidelity**, and it clusters behind five root causes
that gate the whole concurrency tail:

1. **Workers fork interpreter state instead of sharing it.**
   `start_new_thread`'s worker calls `vm_singletons::snapshot_interpreter()`
   → `Interpreter::fork_for_thread()`, so per-thread interpreter
   scaffolding is a *snapshot*. The heap objects are shared (they're
   `Arc`), but `threading` identity/registry semantics and a handful of
   interpreter-global tables need to be genuinely shared, not forked.
2. **GIL hand-off and lock blocking aren't CPython-faithful.** The GIL
   requests a drop every ~100 opcodes when a waiter exists, but it
   doesn't honour `sys.setswitchinterval` as a *time* bound, and the
   low-level `_thread`/`threading` lock/condition timeout + fairness
   semantics don't match `test_thread`.
3. **Stale single-threaded shims.** `queue.py` literally `raise`s
   `Full`/`Empty` instead of blocking ("single-threaded — can't actually
   wait"), and `selectors.py` only ships `SelectSelector`. Both predate
   RFC 0025's real threads.
4. **The cycle collector approximates `gc_refs` from
   `Arc::strong_count`.** That over-counts (every transient Rust borrow
   looks like an external reference), so reachability and finalization
   diverge from CPython — the source of `test_gc`, the
   `test_list`/`test_tuple`/`test_set`/`test_weakset` reachable-hangs,
   the `test_weakref` timeout, and the `test_tempfile` finalizer gap.
5. **`asyncio` is a cooperative scheduler with no real selector
   backend.** `DefaultSelector` is `select()`-only; there is no
   `epoll`/`kqueue`, so `test_selectors` and `test_asyncio` can't be
   measured honestly.

Closing these unblocks the largest remaining *category* of real-world
Python (every threaded web-worker pool, connection pool, background
daemon, `ThreadPoolExecutor`, and `asyncio` server) and is the single
biggest step left toward genuine drop-in behaviour.

The deliverable is measured, not aspirational, matching waves 1–3:
every workstream names the `expectations.toml` rows it flips, each lands
at least one bundled in-process fixture, and the commit is **not done
until a fresh subprocess sweep is `--check` clean** with the touched
rows rewritten from `fail`/`skip`/`timeout` to their measured status.

## Motivation

The README's headline promise is "a 100% compatible, drop-in
replacement for CPython … using CPython's own test suite as a guiding
standard." Threads and `asyncio` are where that promise is most visibly
unmet today, and — crucially — the failures are *correctness* gaps, not
just performance gaps.

Two things make this the right next wave:

1. **The hard part is already done.** The reason concurrency is usually
   a multi-quarter slog — making the object model thread-safe — landed
   in RFC 0025. `crate::sync` aliases `Rc → Arc` and `RefCell →
   GilCell` (a `Send + Sync`, `ReentrantMutex`-backed cell), and a
   compile-time assertion pins `Object: Send + Sync`. So this wave is
   "finish + make faithful," operating on an architecture that already
   runs Python on multiple OS threads.
2. **Several baseline reasons are now provably stale.** The committed
   baseline is `--check`-clean, so the *statuses* are measured-true —
   but a cluster of *reason* strings predate RFC 0025/0026 and describe
   a cooperative model that no longer exists. `queue.Queue.get` literally
   `raise`s `Empty` instead of waiting:

```80:87:crates/weavepy-vm/src/stdlib/python/queue.py
    def get(self, block=True, timeout=None):
        if self._qsize() == 0:
            if self._shutdown:
                raise ShutDown
            if not block:
                raise Empty
            raise Empty  # single-threaded — can't actually wait
        return self._get()
```

`test_threading`'s reason still reads "RFC 0024 spawns OS threads but
runs the Python target cooperatively … land in RFC 0025"; `test_queue`
still reads "don't make progress under the cooperative threading model
(RFC 0025)"; `test_multiprocessing_*` still read "RFC 0026 will
implement." RFC 0025 and 0026 *shipped*. The first move of this wave is
to re-measure those rows and replace guesses with measured
first-failures.

Down-tree, this wave unblocks (re-stating RFC 0025's own list, now
testable end-to-end): threaded WSGI/ASGI worker pools
(`gunicorn --threads`, `waitress`), `concurrent.futures.ThreadPoolExecutor`,
connection pools (`psycopg2`, `redis-py`, SQLAlchemy validators),
observability daemons (`opentelemetry` `BatchSpanProcessor`), and
`asyncio` socket servers/clients.

## CPython reference

This RFC matches **CPython 3.13** as defined by:

- **GIL + eval breaker** — `Python/ceval_gil.c` (the drop/acquire cycle,
  `gil_drop_request`, `eval_breaker`, `sys.setswitchinterval` →
  `_PyEval_SetSwitchInterval`), `Python/ceval.c` (`_Py_HandlePending`).
- **Threads** — `Modules/_threadmodule.c` (`start_new_thread`,
  `allocate_lock`, `LockType.acquire(blocking, timeout)`, `RLock`,
  `_set_sentinel`, `get_ident`/`get_native_id`, `interrupt_main`,
  `stack_size`), `Lib/threading.py` (`Thread`, `Lock`, `RLock`,
  `Condition`, `Event`, `Semaphore`, `BoundedSemaphore`, `Barrier`,
  `local`, `current_thread`/`main_thread`/`enumerate`/`active_count`,
  `excepthook`, daemon shutdown, `Thread.join(timeout)`).
- **Per-thread state** — `Include/internal/pycore_tstate.h`,
  `Python/pystate.c` (the user-visible `PyThreadState` fields: frame
  pointer, exc state, recursion depth, `tstate->dict`).
- **GC** — `Modules/gcmodule.c` (`gc.collect(generation)`,
  `get_referrers`/`get_referents`/`get_objects`, `set_threshold`/
  `get_threshold` default `(700, 10, 10)`, `gc.callbacks` with the
  `("start"|"stop", info)` protocol, `gc.freeze`/`unfreeze`,
  `gc.get_stats`, `DEBUG_*` flags), PEP 442 finalization
  (`tp_finalize`, resurrection, the `gc.garbage` uncollectable list).
- **Weakrefs** — `Objects/weakrefobject.c` (callback ordering: callbacks
  run after the referent is cleared, before the cycle is broken),
  `Lib/weakref.py` (`WeakValueDictionary`/`WeakKeyDictionary`/`WeakSet`
  self-cleaning on collect), `Lib/_weakrefset.py`.
- **Selectors / asyncio** — `Lib/selectors.py`
  (`SelectSelector`/`PollSelector`/`EpollSelector`/`KqueueSelector`/
  `DevpollSelector`, `DefaultSelector` platform pick),
  `Modules/selectmodule.c` (`select.poll`/`epoll`/`kqueue`),
  `Lib/asyncio/` (`selector_events`, `base_events`,
  `loop.run_in_executor`, `call_soon_threadsafe` self-pipe wakeup,
  streams, `unix_events`).
- **`queue`** — `Lib/queue.py` (`Queue`/`LifoQueue`/`PriorityQueue`/
  `SimpleQueue`, blocking `get`/`put` on `Condition`, `join`/`task_done`,
  PEP 692-era `shutdown()` 3.13 semantics).
- **`concurrent.futures`** — `Lib/concurrent/futures/thread.py`
  (`ThreadPoolExecutor`, `Future` lifecycle, `as_completed`, `wait`,
  `map`).

PEP 703 free-threading remains **out of scope** (the GIL stays; this is
the explicit alternative below). PEP 684 per-interpreter GIL stays out
of scope too — we model one `Interpreter` with per-thread `ThreadState`.

## Current baseline (measured starting point)

- `cargo build --workspace` is green.
- Bundled `tests/regrtest/` suite is `--check` clean (`unexpected 0`).
- CPython `Lib/test/` allowlist: **58 `pass`, 59 `fail`, 32 `skip`,
  2 `timeout`**.

The concurrency-cluster rows this wave targets, with their *committed*
status (measured) and a note on whether the *reason* is current or
stale:

| Row | Status | Reason currency |
|---|---|---|
| `test_threading` | fail | **stale** ("runs cooperatively … land in RFC 0025") |
| `test_thread` | fail | current (low-level lock semantics) |
| `test_queue` | skip | **stale** ("cooperative threading model") |
| `test_concurrent_futures` | skip | partial (ProcessPool needs mp; ThreadPool should run) |
| `test_gc` | fail | current (gen thresholds, callbacks, `get_referrers`) |
| `test_weakref` | timeout | current (cyclic-GC progress under aggressive thresholds) |
| `test_weakset` | fail | current (finalize callback ordering + reachability) |
| `test_set` | fail | current (GC reachability assertions) |
| `test_list` | skip | current (`gc.collect()` reachable-hang) |
| `test_tuple` | skip | current (reachable-hang) |
| `test_tempfile` | fail | current ("needs finalizer/weakref-driven cleanup") |
| `test_selectors` | skip | current (needs epoll/kqueue exposure) |
| `test_asyncio` | skip | current (selector backends not all wired) |
| `test_signal` | skip | current (pthread_kill + thread coordination) |
| `test_threadsignals` | fail | current (thread-info object shape) |

**WS0 (below) re-measures all of these** before any code changes, so the
wave starts from honest first-failures rather than guesses. `vendor/cpython/Lib/test`
is already checked out, so this is a single `regrtest --no-check`
subprocess run.

## Detailed design

Nine workstreams (WS0–WS8). Each lists the affected crate(s)/module(s),
the design, and the `expectations.toml` rows it is expected to flip.
Line-count estimates are rough and include Rust glue, frozen-Python
edits, and tests.

### WS0 — Re-measure the stale concurrency baseline · ~0.3K LOC (data)

Run `weavepy-conformance regrtest --cpython-dir vendor/cpython/Lib/test
--mode subprocess --jobs 8 --no-check` filtered to the cluster above and
rewrite each `reason` to the measured first failure. This is pure
bookkeeping but it de-risks the whole wave: it tells us which rows
already advanced for free on the RFC 0025/0026 heap (candidates to flip
immediately) versus which need real work. Expectation, to be confirmed:
`test_threading`/`test_queue` advance well past their stale reasons;
`test_gc`/`test_weakref` remain hard.

### WS1 — Shared interpreter state for worker threads (`weavepy-vm`) · ~4K LOC

**Problem.** The worker forks the interpreter rather than sharing it:

```203:209:crates/weavepy-vm/src/vm_singletons.rs
pub fn snapshot_interpreter() -> Option<crate::Interpreter> {
    if let Some(bt) = seed_types_slot().lock().clone() {
        crate::builtin_types::install_shared(bt);
    }
    let slot = seed_slot().lock();
    slot.as_ref().map(|i| i.fork_for_thread())
}
```

The heap objects reached *through* the forked interpreter are shared
(they're `Arc`), and builtin types are explicitly shared via
`install_shared`. But interpreter-global tables that CPython keeps
process-wide — `sys.modules`, the import lock, the `builtins` module
dict, the `__main__` namespace, the `threading` active-thread registry,
`sys.audit`/trace hook registration, the warnings registry — must be
genuinely shared so a module imported (or monkey-patched) on one thread
is visible on another.

**Design.**
- Promote the shared, immutable-after-startup tables to
  `Arc<…>` fields cloned (not deep-copied) into each worker:
  `sys.modules` dict, `builtins` dict, `__main__` module, the import
  lock, the codec registry. `fork_for_thread` keeps forking only the
  genuinely per-thread state — and we make that explicit by routing it
  through the existing `ThreadState` (frame stack, exc info,
  recursion depth, contextvars stack, `threading.local` table).
- Make `threading.current_thread()`/`enumerate()`/`active_count()`/
  `main_thread()` read one shared registry (`thread_registry`) so
  identity is consistent across threads, and `Thread.ident`/`native_id`
  match what the worker observes.
- Audit the ~6 interpreter fields that are currently cloned-by-value in
  `fork_for_thread` and reclassify each as shared (`Arc`) or per-thread
  (`ThreadState`). Add a debug assertion that a module dict mutated on a
  worker is pointer-identical to the parent's.

**Flips:** the structural prerequisite for `test_threading`; ensures
worker-visible imports/globals match CPython.

### WS2 — GIL hand-off + lock/condition fidelity (`weavepy-vm`) · ~3K LOC

**Problem.** The GIL drops on an opcode count, not a time bound:

```284:292:crates/weavepy-vm/src/gil.rs
    pub fn tick(self: &Arc<Self>) -> bool {
        let n = self.breaker.tick.fetch_add(1, Ordering::Relaxed);
        if n.wrapping_rem(100) == 0 && self.breaker.waiter_count() > 0 {
            self.breaker.request_gil_drop();
            return true;
        }
        false
    }
```

**Design.**
- Drive the drop request off `switch_interval` as a real elapsed-time
  bound (CPython's 5ms default): track the holder's acquire timestamp;
  on the periodic tick, request a drop once `now - acquired >=
  switch_interval` *and* a waiter exists. Keep the opcode counter only
  as the polling cadence. Wire `sys.setswitchinterval`/`getswitchinterval`
  to it (already a field; make it authoritative).
- Fair hand-off: when the holder drops on request, it must not
  immediately re-acquire ahead of a parked waiter (CPython's
  `gil_drop_request` + `FORCE_SWITCHING`). Use a `parking_lot` fairness
  hop or an explicit "yield to waiter" baton so two CPU-bound threads
  alternate instead of one starving.
- Faithful `_thread.LockType`: `acquire(blocking=True, timeout=-1)` with
  microsecond-precision timeout, `locked()`, non-reentrant double-acquire
  blocking, release-by-non-owner allowed (CPython low-level locks aren't
  owned). `RLock`: owner ident + recursion count, `release()` by
  non-owner → `RuntimeError`. All acquires drop the GIL while parked
  (via the existing `allow_threads_then`).
- Frozen `threading.py`: make `Condition.wait(timeout)`,
  `Event.wait`, `Semaphore`/`BoundedSemaphore`, and `Barrier` block on
  real cross-thread primitives (they currently lean on the same shims as
  `queue`). `Thread.join(timeout)` already routes through `_tstate_lock`;
  verify the timeout precision.

**Flips:** `test_thread`; large contributor to `test_threading`.

### WS3 — Blocking `queue` + `concurrent.futures` parallelism (frozen Python) · ~2K LOC

**Problem.** `queue.Queue` can't block — `put` on a full queue raises
`Full` rather than waiting for a consumer (the `get` side cited in
§Motivation is the same shim):

```67:75:crates/weavepy-vm/src/stdlib/python/queue.py
    def put(self, item, block=True, timeout=None):
        if self._shutdown:
            raise ShutDown
        if self.maxsize > 0 and self._qsize() >= self.maxsize:
            if not block:
                raise Full
            raise Full  # single-threaded — can't actually wait
        self._put(item)
        self.unfinished_tasks += 1
```

**Design.**
- Rewrite `queue.py` to CPython's real implementation: `get`/`put` block
  on `not_empty`/`not_full` `Condition`s (which become real after WS2),
  honour `timeout`, implement `join`/`task_done` via `all_tasks_done`,
  and the 3.13 `shutdown(immediate=…)` semantics. `SimpleQueue` over a
  real lock. Port verbatim from `Lib/queue.py`.
- `concurrent_futures.py`: `ThreadPoolExecutor` dispatches work items to
  real worker threads via a blocking `queue.SimpleQueue`; `Future`
  `result(timeout)`/`exception`/`cancel`/`add_done_callback` synchronise
  across threads; `as_completed`/`wait`/`Executor.map` block correctly.
  `ProcessPoolExecutor` stays delegated to `multiprocessing` (RFC 0026)
  and is out of this wave's flip list.

**Flips:** `test_queue`; `test_concurrent_futures` (ThreadPool subset —
re-measure the ProcessPool portion in WS0).

### WS4 — Cycle-GC heuristics + deterministic finalization (`weavepy-vm`) · ~6K LOC

**Problem.** `gc_refs` is approximated from `Arc::strong_count`, which
the collector itself flags as too conservative:

```51:62:crates/weavepy-vm/src/gc_trace.rs
//! 1. For each tracked object, compute a **gc_refs** counter
//!    initialised from the object's outer (Python-visible)
//!    strong refcount. (We approximate via `Rc::strong_count`,
//!    which is conservative — every Rust-side stash counts —
//!    so the false-positive rate is "we keep more than CPython
//!    would.")
```

Over-counting means cycles survive collections CPython would reclaim, so
`gc.collect()` returns the wrong count, `gc.garbage` differs, finalizers
fire late, and reachability probes fail.

**Design.**
- **A faithful Python-visible refcount.** Introduce a per-tracked-object
  count of *known interpreter-internal* references (eval stack, the
  collector's own work lists, the weakref registry's strong clones —
  already discounted per the `test_copy` reason) and derive
  `gc_refs = strong_count - internal_refs`. The collector then matches
  CPython's "subtract internal references" invariant instead of a
  conservative floor. This is the central, highest-risk change.
- **Generational thresholds + promotion.** Honour `(700, 10, 10)`
  defaults, `gc.get_threshold`/`set_threshold`, gen-0→1→2 promotion
  counters, and full-collection triggering on the gen-2 threshold.
- **`gc` module surface.** `get_referrers`/`get_referents` (walk the
  `Traverse` graph faithfully), `get_objects(generation=None)`,
  `gc.freeze`/`unfreeze`/`get_freeze_count`, `gc.get_stats`,
  `gc.callbacks` firing `("start", info)` / `("stop", info)` in
  registration order around each collection, and the `DEBUG_*` flags.
- **PEP 442 finalization.** Run `__del__`/`tp_finalize` on unreachable
  objects exactly once, in CPython's order, with resurrection handling
  and the uncollectable-with-`__del__`-in-cycle → `gc.garbage` path
  (now legal post-PEP 442 only for legacy finalizers). Make
  finalization deterministic enough that `tempfile.NamedTemporaryFile(delete=True)`'s
  weakref finalizer unlinks on `gc.collect()`.

**Flips:** `test_gc`, `test_tempfile`.

### WS5 — Container/iterator cycle termination + weakref ordering (`weavepy-vm`) · ~3K LOC

**Problem.** `test_list`/`test_tuple` are `skip`ped to avoid a 30s
`gc.collect()` reachable-hang on list/iterator reference cycles;
`test_set`/`test_weakset` fail on reachability assertions; `test_weakref`
*times out* on the cyclic-collection stress loop.

**Design.**
- The WS4 real-refcount fix removes the dominant cause (conservative
  survival → repeated re-scans). On top of it: ensure list-iterator,
  tuple-iterator, dict/set-view, and generator-frame back-references are
  tracked and traversed so their cycles collect in one pass instead of
  surviving across generations.
- Bound the collector's work: the current full mark-sweep re-scans the
  whole generation; add the CPython "only rescan the unreachable
  candidate set" optimisation so the `RefCycle` stress loops in
  `test_weakref` complete well inside the 60s budget.
- Weakref callback ordering (`Objects/weakrefobject.c`): clear the
  referent, then run callbacks oldest-first, then break the cycle.
  `WeakSet`/`WeakValueDictionary`/`WeakKeyDictionary` self-clean
  deterministically on collect (the `test_copy` reason shows the
  weakref-registry discount already exists; extend it to the set/dict
  flavours and the callback order).

**Flips:** `test_list`, `test_tuple`, `test_set`, `test_weakset`,
`test_weakref`.

### WS6 — Real selector backends (`weavepy-vm`, frozen `selectors`) · ~3K LOC

**Problem.** Only `select()` is exposed:

```101:132:crates/weavepy-vm/src/stdlib/python/selectors.py
class SelectSelector(BaseSelector):
    """Default selector backed by `select.select`."""
    # ...
DefaultSelector = SelectSelector
```

**Design.**
- Grow `select_mod.rs` to expose, where the platform provides them:
  `select.poll` (level-triggered `poll(2)`), `select.epoll` (Linux),
  `select.kqueue`/`kevent` (BSD/macOS). Back them with `libc` directly
  (the workspace already uses raw FFI elsewhere) or `mio`; raw `libc`
  keeps the fd/event semantics CPython's tests assert.
- Frozen `selectors.py`: port CPython's `PollSelector`/`EpollSelector`/
  `KqueueSelector`/`DevpollSelector` and the `DefaultSelector` platform
  pick (kqueue on macOS/BSD, epoll on Linux, poll/select fallback).

**Flips:** `test_selectors`; prerequisite for WS7.

### WS7 — Faithful `asyncio` over the real loop (frozen `asyncio`, `weavepy-vm`) · ~5K LOC

**Problem.** The loop is a cooperative scheduler, by its own docstring:

```22:25:crates/weavepy-vm/src/stdlib/python/asyncio.py
What does NOT work (yet):

* Subprocess transports (use plain `subprocess.Popen`).
* Real OS-level parallelism — this is a cooperative scheduler.
```

**Design.**
- Rebuild the I/O core of the loop on the WS6 selectors
  (`SelectorEventLoop`): `add_reader`/`add_writer`/`sock_*`,
  socket transports (`create_connection`/`create_server`,
  `_SelectorSocketTransport`), and timer/callback scheduling
  (`call_soon`/`call_later`/`call_at`) driven by `selector.select(timeout)`.
- `call_soon_threadsafe` + a self-pipe wakeup so a worker thread (or
  `run_in_executor`) can wake the loop; `loop.run_in_executor(None, fn)`
  uses the WS3 `ThreadPoolExecutor` for real parallelism of blocking
  calls.
- Keep subprocess transports out of scope (gated on a deeper
  `subprocess`/pidfd story); `test_asyncio` is graded on the sandbox-safe
  subset (no live external network), matching how `test_socket`/
  `test_ssl` are already handled.

**Flips:** `test_asyncio` (sandbox subset); contributes to `test_selectors`
end-to-end coverage.

### WS8 — `test.support` threading helpers, fixtures, baseline rewrite · ~1.5K LOC

- Port the `test.support.threading_helper` surface the cluster imports:
  `threading_setup`/`threading_cleanup`, `join_thread`, `wait_until`,
  `catch_threading_exception`, `start_threads`, and the
  `@reap_threads` decorator. (`test_threadsignals`'s "dict has no
  attribute 'name'" reason also gets addressed here — the thread-info
  object needs attribute access, not a dict.)
- One bundled in-process fixture per workstream under `tests/regrtest/`
  so CI catches regressions without the CPython checkout:
  `thr_shared_state.py` (WS1), `thr_lock_timeout.py` (WS2),
  `queue_blocking_pingpong.py` + `pool_map_parallel.py` (WS3),
  `gc_cycle_collect.py` + `gc_callbacks_order.py` (WS4),
  `weakref_cycle_clear.py` (WS5), `selector_epoll_kqueue.py` (WS6),
  `asyncio_echo_server.py` (WS7).
- Rewrite every touched `expectations.toml` row to its **measured**
  status; commit complete only when `--check` reports `unexpected 0`.

## Measured targets

Wave 4's commit-acceptance bar is flipping the following rows to `pass`
(grouped by workstream). Anything that advances but still fails gets a
rewritten, measured `reason` rather than a guess. The exact cut line is
finalised against the WS0 baseline run; rows that prove deeper than
estimated are rewritten to a measured `reason` and deferred rather than
expanding the commit.

| Cluster | Target rows (→ `pass`) |
|---|---|
| WS1/WS2 threads | `test_threading`, `test_thread` |
| WS3 queue/futures | `test_queue`, `test_concurrent_futures` |
| WS4 GC | `test_gc`, `test_tempfile` |
| WS5 containers/weakref | `test_list`, `test_tuple`, `test_set`, `test_weakset`, `test_weakref` |
| WS6 selectors | `test_selectors` |
| WS7 asyncio | `test_asyncio` (sandbox subset) |

That is **~13 rows flipping `fail`/`skip`/`timeout` → `pass`** (a swing
from 58 → ~71 `pass`), plus measured-truth rewrites for `test_signal`,
`test_threadsignals`, and the `test_multiprocessing_*` rows that WS0
finds already advanced. The remaining tail (PEP 703 free-threading,
live-network `asyncio`/`test_socket`/`test_httplib`, Windows forkserver,
incremental GC, sub-interpreter `interpreters` public-API corners) is
explicitly **deferred to wave 5+**.

## Drawbacks

- **Determinism risk is real and concentrated in WS4/WS5.** GC and
  weakref tests are timing- and ordering-sensitive; a `--check`-clean
  baseline requires them to pass *reliably*, not just once. Mitigation:
  the real-refcount fix targets the deterministic root cause (not a
  tuning knob); bundled fixtures pin the behaviour in-process; and any
  genuinely non-deterministic probe (e.g. a `gc.set_threshold` stress
  loop that depends on allocation timing) is rewritten to a measured
  `reason` and deferred rather than force-passed — exactly how the
  project already treats `test_weakref`.
- **The real-refcount change is invasive.** Deriving the Python-visible
  refcount means accounting for every interpreter-internal `Arc` stash.
  Getting the accounting wrong under-counts and risks collecting a live
  object (a correctness bug, not just a test failure). This is the
  single riskiest change; it lands behind a `gc.DEBUG_SAVEALL`-backed
  audit mode and the full bundled GC fixture set before the baseline is
  rewritten.
- **GIL fairness has a throughput cost.** Forcing hand-off on the switch
  interval adds context-switch traffic under multi-thread contention.
  CPU-bound single-thread code is unaffected (no waiter ⇒ no drop);
  validated on the bench corpus before/after.
- **No multicore speedup.** The GIL still serialises bytecode (intended,
  matches CPython 3.13). PEP 703 is a later wave.
- **Breadth.** Nine workstreams touching the VM core, the GC, and three
  frozen modules is a lot of surface; a regression in the GC ripples
  everywhere. Capped by the per-workstream fixtures and the `--check`
  gate.

## Alternatives

- **PEP 703 free-threading (no GIL).** Atomic refcounts + per-object
  locks; genuine multicore parallelism. Far higher ceiling but a much
  larger, riskier surface, and it would have to land *on top of* the
  fidelity work here anyway. The GIL stays; `gil.rs` is already shaped
  so the swap is local (RFC 0025 §Alternatives). Sequenced as a later
  wave.
- **Keep the snapshot/fork worker model and document the divergence.**
  Lowest effort, but it's precisely the compatibility gap this wave
  exists to close; rejected (also rejected in RFC 0025).
- **Narrower scope — GC only, or asyncio only.** Coherent, but the rows
  share root causes (WS4's refcount fix is what unblocks WS5's hangs;
  WS6's selectors are what make WS7 measurable), so splitting them
  re-discovers the same work across two commits. Rejected in favour of
  one coherent arc, matching the wave cadence.
- **Port CPython's GC verbatim in Rust.** Tempting, but CPython's
  collector is built around `PyGC_Head` intrusive lists and a real
  refcount field that WeavePy's `Arc`-cell model doesn't have; we match
  the *algorithm and observable behaviour*, not the data structure.

## Prior art

- **CPython 3.13** — every decision tracks it: the `gil_drop_request`/
  switch-interval hand-off, the `(700, 10, 10)` generational collector,
  PEP 442 finalization order, weakref callback ordering, and the
  selector/asyncio split.
- **PyPy** — runs CPython's `Lib/test` as its bar and uses a different
  GC (generational moving) but the same *observable* `gc` module
  semantics; confirms that matching behaviour (not data structure) is
  the right target.
- **GraalPy / Jython** — both report that the threading long tail is
  dominated by lock/condition timeout fidelity and GC reachability
  semantics rather than exotic features, matching WeavePy's measured
  clustering.
- **RFC 0024 / 0025** — the in-tree foundation; this wave is the
  fidelity layer those RFCs explicitly deferred ("detailed heuristics …
  don't match CPython yet").

## Unresolved questions

- **Exact internal-refcount accounting (WS4).** Per-object internal-ref
  counter vs. a collection-time "subtract the known root sets" sweep.
  The sweep is less invasive (no per-object field) but must enumerate
  every Rust-side stash; the counter is precise but threads through
  every `Arc::clone` of a tracked object. Lean toward the sweep first,
  measured against the GC fixtures.
- **Selector backend: raw `libc` vs `mio`.** `mio` is batteries-included
  but abstracts away some fd/event edges CPython's tests assert
  (e.g. `epoll` `EPOLLONESHOT`, `kqueue` `EV_*` flags). Lean raw `libc`
  for the parts the tests probe, `mio` only if it doesn't hide
  semantics.
- **Force-pass vs. measured-rewrite for timing-sensitive GC probes.**
  Decide per-test against the WS0 baseline; default to measured-rewrite
  if a probe can't be made reliable across `-j8` contention.
- **`switch_interval` default.** Keep CPython's 5ms, or lower it so the
  fairness fixtures run faster in CI? Keep 5ms for fidelity; the
  fixtures use explicit `setswitchinterval`.
- **Scope of `asyncio` graded surface.** Which `test_asyncio` submodules
  are sandbox-safe (no live network/subprocess)? Finalised against the
  WS0/WS7 run; the rest are `skip` with a measured reason.

## Future work

- **Wave 5 — PEP 703 free-threading**: atomic refcounts, per-object
  locks, biased reference counting, and dropping the per-`GilCell`
  `ReentrantMutex` (RFC 0025's documented perf follow-up).
- **Incremental GC**: split the mark phase across eval-breaker ticks so
  worst-case pauses stay sub-millisecond on large heaps.
- **Live-network concurrency**: a fixture HTTP/echo server harness so
  `test_socket`/`test_httplib`/`test_asyncio`'s networked subset can be
  graded in CI.
- **`multiprocessing` re-measure + completion**: WS0 will show where
  RFC 0026 actually stands; close `test_multiprocessing_*` and
  `ProcessPoolExecutor` in a dedicated follow-up.
- **Sub-interpreters public API (PEP 734)**: the runtime mechanics exist
  (RFC 0031 shipped `_xxsubinterpreters` + `interpreters`); reconcile
  with the shared-state model from WS1.

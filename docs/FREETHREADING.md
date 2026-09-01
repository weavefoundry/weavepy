# Free-threading (PEP 703) in WeavePy — design note

- **Status**: Living document, landed as RFC 0076 WS10 (a debt owed
  since RFC 0066). Describes the invariants the WS11 `-X gil=0` /
  `PYTHON_GIL=0` runtime mode builds on and the audit list it must
  burn down; kept honest by the WS12 measured lane
  (`tests/regrtest/expectations-gil0.toml`). Written from the code as
  it stands — every claim cites the module that makes it true.

## 1. The heap model audit

The single largest piece of CPython's PEP 703 diff — biased reference
counting, immortalization, deferred refcounting, and the split
`ob_refcnt` object layout — **does not exist in WeavePy and is not
needed**, because the heap has been atomically refcounted since the
RFC 0024/0025 cross-thread waves:

- `crates/weavepy-vm/src/sync.rs` defines
  `pub type Rc<T> = std::sync::Arc<T>` and
  `pub type Weak<T> = std::sync::Weak<T>`. Every `Object` variant,
  every `TypeObject`, every `CodeObject` is reached through these
  aliases, so every refcount operation in the VM is already an atomic
  RMW. There is no plain-`Rc` heap tier to migrate and no second
  object layout to ship — which is why WS11 is one binary with a
  runtime flag, not a `weavepy-3.13t` ABI.
- Interior mutability is uniformly `sync::GilCell` (aliased as the
  workspace's `RefCell`/`Cell`). A `GilCell` is a hand-rolled
  cross-thread **reentrant mutex** (`owner: AtomicU64` holding
  `gil::current_thread_id()`, CAS on fresh acquire, relaxed-load
  reentry — see `GilCell::lock_acquire`) plus a CPython-shaped borrow
  counter (`borrow: AtomicIsize`). Uncontended acquisition is one CAS;
  same-thread reentry is one relaxed load. Every container payload —
  `Object::List(Rc<RefCell<Vec<Object>>>)`,
  `Object::Dict(Rc<RefCell<DictData>>)`, `Object::Set`,
  `Object::ByteArray` (`crates/weavepy-vm/src/object.rs`), instance
  dicts, and `TypeObject`'s mutable fields — already sits behind one.
- `Object` is `Send + Sync` by construction (compile-time assertion in
  `object.rs`); worker threads share the heap by `Arc` cloning, and
  `Interpreter::fork_for_thread` (`crates/weavepy-vm/src/lib.rs`)
  shares builtins, the module cache, stdout, and the hook objects
  across threads today, under GIL scheduling.

Consequence: in WeavePy the GIL is purely a **scheduling** device — it
serializes bytecode so that unsynchronized *multi-step* invariants
hold, but it protects neither refcounts nor the memory safety of
individual cell accesses. Free-threading is a locking-discipline
project (sections 2–3), not a heap rewrite — the structural advantage
over CPython this note exists to record.

## 2. What the GIL guards today

The GIL itself is `gil::GilState` (`crates/weavepy-vm/src/gil.rs`): a
process-wide reentrant `sync::GilLock` (`parking_lot::ReentrantMutex`)
plus the `EvalBreaker`, a per-thread `GIL_GUARD_STACK`
(`push_gil_guard`/`pop_gil_guard`/`allow_threads_then`), the
per-instruction `yield_checkpoint`/`maybe_yield_gil` hand-off gated by
CPython's 5 ms switch interval (`GIL_HELD_SINCE`,
`EvalBreaker::switch_interval_ns`, `sys.setswitchinterval`), and fork
reinitialization (`reinit_after_fork_in_child`). The C-API reaches it
through `PyGILState_Ensure`/`PyEval_SaveThread`
(`crates/weavepy-capi/src/lifecycle.rs`), which push/pop the same
guard stack; per-thread `PyThreadState` bodies are already thread-local
(`crates/weavepy-capi/src/pystate.rs`, `TSTATE`).

The WS11 mode switch is landed as hooks at the top of the "RFC 0076
WS11" section of `gil.rs`: `set_free_threading` /
`set_free_threading_forced` / `free_threading_enabled` /
`free_threading_requested` / `reenable_gil_for_extension` /
`gil_reenabled_by_extension`, plus the PEP 741 `SetInt("gil", 0)`
plumbing in `crates/weavepy-capi/src/pep741.rs` (forwarded as the
`-X gil=N` xoption).

**WS11 status: landed.** The scheduler change is in
`GilState::acquire`/`try_acquire`: under `free_threading_enabled()`
they hand out a `lock_free` `GilGuard` that holds no lock and skips
the holder/depth bookkeeping, `GilGuard::allow_threads` and
`maybe_yield_gil` short-circuit, and the tier-2 JIT pins execution to
tiers 0/1 for the whole run (`JitState::new`). `sys._is_gil_enabled()`
reports truthfully, the CLI accepts `-X gil=0` / `PYTHON_GIL=0`
(xoption beats env; `PYTHON_GIL=0` marks the mode *forced*), and the
`Py_mod_gil` contract is applied at extension import in
`crates/weavepy-capi/src/module.rs::note_extension_gil_declaration`
(single-phase in `loader.rs`, the slot in `run_multiphase_init`):
a non-declaring extension re-enables the GIL with CPython 3.13t's
RuntimeWarning unless the mode was forced. Measured on a 2-thread
compute workload the mode turns a 0.77× threading slowdown into a
1.58× speedup (interpreter tiers; the JIT-off cost applies to serial
throughput as designed). `tests/regrtest/test_rfc0076_gil0.py` covers
the flag surfaces, threaded execution, and both extension-contract
paths; the WS12 `--gil0` conformance lane
(`weavepy-conformance regrtest --gil0`, baseline
`tests/regrtest/expectations-gil0.toml`) grades the bundled
thread fixtures plus CPython's `test_thread` / `test_threading` /
`test_threading_local` / `test_queue` / `test_threadedtempfile`
under the mode — 10/10 passing at the measured baseline.

The inventory of what the GIL's serialization actually protects, from
a sweep of `static` state in `crates/weavepy-vm/src`:

### 2a. Already safe without the GIL (real locks / atomics)

- **Import lock** — `stdlib/imp_mod.rs::IMPORT_LOCK`: an owner-tracked
  reentrant `parking_lot::Mutex` + `Condvar`, acquired with the GIL
  *released* (`import_lock_acquire` goes through
  `gil::allow_threads_then`), fork-reinitialized
  (`import_lock_reinit_in_child`). Its semantics do not depend on the
  GIL at all.
- **Thread registry** — `thread_registry.rs::ThreadRegistry`:
  `RwLock<BTreeMap>` + atomics, with its own fork story
  (`reset_after_fork_in_child`).
- **Async exceptions** — `gil.rs::async_exc_map` (a `Mutex`-guarded
  map) with the `ASYNC_EXC_COUNT` relaxed fast gate.
- **Descriptor/type side tables** — `descr_registry.rs`:
  `BUILTIN_MODULE`, `NATIVE_DESCR_ACCESSOR`, `SURFACE_ONLY`,
  `DEFAULT_NEW`, `LIVE_C_DOC` are all `parking_lot::RwLock` maps
  already (the sharded-lock shape section 3 asks for).
- **Tracing/audit hooks** — `trace.rs`: `Mutex`-held hook objects with
  `AtomicBool` presence gates; `capi_watchers.rs` likewise.
- **Interpreter seed** — `vm_singletons.rs::INTERPRETER_SEED`,
  `WORKER_THREAD_ID`, `SEED_BUILTIN_TYPES`: `OnceLock<Mutex<…>>`.
  Pending-call queues (`PENDING_PY_CALLS_MAIN`/`_ANY`) are `Mutex`ed
  with atomic mirror counters.
- **Threading primitives** — `sync.rs::RealLock`/`RealRLock`/
  `RealEvent`/`RealSemaphore`/`RealCondition`/`RealBarrier` are real
  `Mutex`/`Condvar` constructions backing `threading.*`; nothing here
  assumes the GIL.
- Misc. flag state (`signal_mod.rs` trip slots, `warnings_mod.rs::
  STATE`, `ext_loader.rs::REGISTRY`, `hashlib_mod.rs::CTOR_CACHE`,
  `faulthandler` tables) — all `Mutex`/atomic already.

### 2b. GilCell-protected containers — sound under `gil=0`, with a generalized discipline

Every dict/list/set/bytearray body, instance dict, and mutable
`TypeObject` field is a `GilCell`, i.e. **already a per-object
critical section**: a `borrow_mut()` on thread A blocks thread B's
`borrow()` on the same cell, GIL or no GIL. Single mutations (one
`insert`, one `push`, one attribute store) are data-race-free under
`gil=0` with no further work — this is what "per-object critical
sections" means in the RFC, and WeavePy already has the lock word (the
`owner`/`borrow` atomics) rather than needing header padding.

What the GIL adds on top of the cell mutex is *scheduling* soundness
for borrows that outlive a re-entry into Python, and that discipline
must be generalized:

- `gil.rs::NO_YIELD_DEPTH` / `no_gil_handoff()` (RFC 0039 WS5) and
  `sync.rs::cell_guards_live()` / `LIVE_CELL_GUARDS` (RFC 0047 wave 5)
  exist to prevent one specific deadlock: handing off the **GIL**
  while a cell mutex is held (a container `__hash__`/`__eq__`/`repr`
  re-entering Python mid-borrow), so the next GIL holder parks on the
  cell forever. `maybe_yield_gil` consults both and refuses to yield.
  Under `gil=0` that GIL↔cell inversion vanishes — but the borrows it
  guarded become *real cross-thread blocking windows*, and new
  inversion pairs appear that the GIL made unreachable (section 3 and
  the risk list below).
- `GilCell::get`/`set` for `T: Copy` (the interpreter's hottest
  operation, RFC 0065 WS2) read/write directly under the owner lock in
  release builds. The safety argument in the source cites "no GIL
  hand-off can occur while the lock is held"; the argument survives
  `gil=0` because the owner lock itself excludes cross-thread access —
  but the comment's invariants are re-audited, not assumed, in WS11.

### 2c. Genuinely GIL-assumed state — the WS11 audit list

These are the places where correctness (not just performance) rides on
the GIL's serialization. Each one must be fixed, fenced, or disabled
before `gil=0` runs bytecode concurrently:

1. **Tier-1 inline caches** — `weavepy-compiler/src/bytecode.rs::
   CacheSlot` is a bare `UnsafeCell<InlineCache>` with
   `unsafe impl Send + Sync` whose SAFETY note is *explicitly tied to
   the GIL* ("get/set are only called by the dispatch loop while the
   GIL is held"). `CodeObject.caches` (`CacheTable`) is shared
   cross-thread via `Arc<CodeObject>`, so under `gil=0` two threads
   warming the same call site is an instant data race, and
   `InlineCache`'s largest variant (~24 bytes) exceeds portable atomic
   width. This is the CAS-publication item in section 4.
2. **The cycle collector** — `gc_trace.rs::GC_STATE` is deliberately
   process-global ("safe because every mutation of a tracked object
   and every collection happens under the GIL", per its own doc
   comment), and the mark phase seeds reachability from
   `Rc::strong_count` **snapshots**. A concurrent mutator changing
   counts mid-mark can make live objects look like garbage — the exact
   race `vm_singletons::clear_thread_python_tls` documents for TLS
   teardown. `gil=0` needs a stop-the-world (or safepoint-quiesced)
   collection window; the eval breaker (`EvalBreaker::request_gc`) is
   the natural rendezvous.
3. **The interned-string pool** — `stdlib/sys.rs::INTERN_POOL` is
   `thread_local!`. Under the GIL this is a benign divergence (each
   thread's `sys.intern` is internally consistent); under `gil=0`,
   cross-thread `is` identity of interned strings — and
   `marshal`'s `str_is_interned` round-trip — becomes observably
   per-thread. Needs a sharded process-global pool.
4. **The built-in type registry** — `builtin_types.rs::BUILTIN_TYPES`
   is `thread_local!`, with cross-thread identity maintained by seed
   adoption (`vm_singletons::install_seed_builtin_types` /
   `SEED_BUILTIN_TYPES`). The adoption protocol is ordered by "workers
   are spawned after `Interpreter::default()` publishes the seed" —
   a GIL-era liveness assumption to re-verify when threads start
   without the GIL choreography.
5. **Type-object invalidation** — `types.rs::bump_attr_version` walks
   the subclass tree performing per-field `Cell` sets; `instance_plan`
   is stamped with the observed version. Each individual set is a
   locked `GilCell` access, but the *multi-field, multi-type*
   invalidation is not atomic: under `gil=0` a reader can observe a
   bumped version with a stale plan or vice versa. Needs
   epoch-publication ordering (section 3).
6. **Hot-gate ordering** — `hot_gates.rs::LOOP_GEN` is `Relaxed`
   everywhere, justified in-source by "cross-thread visibility rides
   the GIL hand-off, whose lock is a full barrier". No hand-off, no
   barrier: the `gil=0` mode must either upgrade the orderings or
   prove eventual visibility suffices per consumer.
7. **Compiled-code caches** — `frozen_code_cache.rs::CACHE` is
   `thread_local!` and its own module doc already says "the
   free-threaded build will replace this with a `Mutex` or a sharded
   cache". Correct today (each thread compiles its own), merely
   wasteful; same posture for `pycache.rs`.
8. **Compound Python-level operations** — the switch-interval
   discipline (`GIL_HELD_SINCE` in `gil.rs`) hides inherently
   non-atomic bytecode triples like a racing `x += 1` / `x -= 1`
   (`test_multiprocessing.test_release_task_refs`). Under `gil=0`
   these races are always exposed. CPython 3.13t accepts the same
   behavior shift; the WS12 race-regression fixtures make it measured
   rather than anecdotal.
9. **Mode truthfulness plumbing** — `sys._is_gil_enabled()` hardcoded
   `True` (`stdlib/sys.rs`), the CLI `gil=0` fatal
   (`weavepy-cli/src/lib.rs`); both must route through
   `gil::free_threading_enabled` (as
   `pep741.rs::runtime_int("gil")` already does).

Known GilCell borrow-across-yield assumptions that were sound under
serialization and become real hazards under `gil=0`:

- **Cell↔cell lock-order inversion.** `GilCell::swap` (`sync.rs`)
  takes two `borrow_mut`s in caller order; more generally any dunder
  that re-enters Python while holding cell A can block on cell B held
  by a peer that wants A. Under the GIL, `NO_YIELD_DEPTH` plus
  serialization made a cross-thread cycle unreachable; under `gil=0`
  it is a plain ABBA deadlock. Mitigation candidates: address-ordered
  acquisition for the two-cell operations, and a bounded
  `try_borrow`-with-backoff for re-entrant paths.
- **Cell↔user-lock inversion.** A `__hash__` waiting on a
  `threading.Lock` while its caller holds the container's cell was
  safe under the GIL (`allow_threads_then` released the GIL, and
  serialization guaranteed the lock holder wasn't mid-operation on
  that cell). Under `gil=0` the lock holder may be parked on it.
- **Fork consistency.** The orphan-steal heuristic in
  `GilCell::lock_contended` (requires exactly one live OS thread)
  stays sound, but the fork reinit path
  (`gil::reinit_after_fork_in_child`,
  `GilCell::reinit_lock_after_fork`) assumes the forking thread held
  the GIL and thus that every cell payload is consistent. Under
  `gil=0`, `os.fork` needs an explicit stop-the-world before the
  snapshot — CPython 3.13t has the same obligation.

## 3. The locking discipline per class of state

Per the RFC 0076 WS11 charter, one discipline per class:

1. **Containers: per-object critical sections.** `GilCell` *is* the
   critical section — uncontended cost is one CAS
   (`GilCell::lock_acquire`), which is also the default-mode cost
   today, so the bench gate's "no default-mode regression" criterion
   is structurally satisfied. `gil=0` work is not adding locks but
   bounding borrow scopes: mutation paths must not hold a cell across
   a re-entry into arbitrary Python unless the re-entrant call is part
   of the same logical operation (hash/eq during insert — CPython's
   per-object critical section does exactly this), and multi-cell
   operations acquire in address order. Read paths stay on the
   reentrant lock initially; lock-free/seqlock reads are a measured
   optimization, not a correctness prerequisite.
2. **Runtime tables: sharded locks.** `descr_registry.rs` already
   demonstrates the target shape (`parking_lot::RwLock` maps). The
   intern pool (2c.3) and the built-in type registry seed path (2c.4)
   move to the same shape, sharded by key hash where contention
   warrants. The import lock keeps CPython's per-module-future
   semantics unchanged (it never depended on the GIL, §2a).
3. **Caches: epoch / CAS publication.** `TypeObject::attr_version`
   becomes the epoch: writers bump it with release ordering *after*
   completing the invalidation walk; readers acquire-load the version,
   read the derived state (`instance_plan`, cache fingerprints), and
   re-check the version — a seqlock read. Tier-1 `CacheSlot` writes
   become CAS-published (repacked `InlineCache` or a version-guarded
   double-word protocol); a torn read must be impossible, a stale read
   merely deopts, which the cooldown machinery already tolerates.
   `hot_gates` orderings get audited per-consumer (2c.6).

## 4. The JIT posture

**Tier-2 native entry is disabled under `gil=0` in this wave.** The
precedent is CPython 3.13t disabling its specializing interpreter; the
grounds are in the code:

- The tier-2 layer is thread-confined by design: `tier2.rs`'s module
  doc says "everything here runs under the GIL on a single thread",
  the `JitState` (engine, compile cache, `compile_gen`) lives in a
  `thread_local!` (`tier2.rs::JIT`), and compiled function pointers
  never cross threads. There is no cross-thread publication path to
  audit in `weavepy-jit/src/runtime.rs` — the `JitFrame` ABI is
  filled and read by the owning thread only.
- What breaks under `gil=0` is not publication but **guard validity
  windows**: burned-in `AttrGuard`/`MethodEntry` fingerprints
  (`tier2.rs`) check `attr_version` at entry and per access, assuming
  world changes are serialized with execution. A peer thread mutating
  a class mid-native-frame invalidates assumptions between
  checkpoints. Epoch-guarded native code is real work — it is the
  "re-enabling the JIT under free-threading" future RFC, chartered by
  WS12's measurements, not this wave.
- Tier-1 stays enabled under `gil=0` once its cache writes are
  CAS-published (§3.3). The `jit_hint` fast-out and hot-counter paths
  are per-thread already.

Gate wiring: the tier-2 entry check gains one relaxed
`gil::free_threading_enabled()` load, false in default mode, so the
default-mode bench envelope is untouched.

## 5. The extension contract

CPython 3.13t's contract, adopted verbatim (RFC 0076 WS11):

- An extension module declares free-threading support with the
  `Py_mod_gil` module slot set to `Py_MOD_GIL_NOT_USED`. Slot parsing
  lands in the weavepy-capi module-init path (multi-phase init reads
  the slot table; single-phase modules can never declare and always
  trigger the fallback).
- Importing a **non-declaring** extension under `gil=0` re-enables the
  GIL for the rest of the process and emits a `RuntimeWarning` naming
  the module. The runtime half is landed:
  `gil::reenable_gil_for_extension()` flips the process-wide
  `GIL_REENABLED` flag exactly once and returns whether this call
  performed the re-enable (the import path then emits the warning);
  `gil::free_threading_enabled()` and `sys._is_gil_enabled()` report
  the re-enabled state truthfully via `gil_reenabled_by_extension()`.
- **Force-override**: `PYTHON_GIL=0` in the environment keeps the GIL
  off even when a non-declaring extension is imported — the warning
  still fires, the re-enable does not, and any resulting crash is on
  the user's head (CPython's documented semantics). The current
  `reenable_gil_for_extension` re-enables whenever the mode was
  requested; threading the env-var-vs-xoption distinction through it
  is a WS11 item.
- Entry points: `-X gil=0` / `PYTHON_GIL=0` at the CLI, and
  `PyInitConfig_SetInt("gil", 0)` for embedders — `pep741.rs` records
  the option and forwards it as the `gil=0` xoption, one source of
  truth with the CLI; `PyConfig_GetInt("gil")` reads back
  `!free_threading_enabled()` at runtime.
- **Out of scope, stated**: a `Py_GIL_DISABLED` ABI tag and `cp313t`-
  analog wheels (the object layout does not change, so a second ABI
  would be ceremony), and per-interpreter GILs (the RFC 0075 own-GIL
  coercion stands).

## 6. The measured plan from experimental mode to default

The mode graduates on measurements, not assertions (RFC 0076 WS12 and
acceptance criterion 4):

1. **The scoped conformance lane.** `cargo run -p weavepy-conformance
   -- regrtest --gil0 …` runs the threading/concurrency family
   (`test_threading`, `test_thread`, `test_concurrent_futures`,
   `test_queue`, `test_asyncio` submodules, the `test_importlib`
   parallel-import legs) plus new bundled race-regression fixtures
   (concurrent dict/list mutation, racing type creation, import
   storms) under `-X gil=0`, graded against a **measured**
   `tests/regrtest/expectations-gil0.toml` baseline. The full
   550-label sweep is deliberately *not* the experimental gate; the
   default-mode sweep must stay at `unexpected 0`, proving the mode's
   plumbing (lock words, CAS caches) costs default mode nothing.
2. **The thread-scaling fixture.** A `threads=8`
   embarrassingly-parallel pure-Python fixture pair
   (`fixtures/parallel_scaling.py`, run by `weavepy-bench scaling`)
   lands in `weavepy-bench`: the GIL build must report ~1× scaling,
   the `gil=0` run must report >1× — the mode's reason to exist as a
   number, per the RFC's "measured, not marketing" clause.
   *Measured 2026-08 (macOS arm64, 8 threads, integer `+`/`*`/`%`
   kernel):* the default build reports **0.90×** (the GIL
   serializes; join/switch overhead), `-X gil=0` reports **3.26×**.
   Two methodology notes baked into the fixture: the kernel is
   warmed at full size before the serial leg (an in-flight tier-2
   compile otherwise reports as fake parallel "scaling" on the
   default build), and thread spawn stays outside the timed window
   (a JIT-hot kernel is milliseconds of work; spawn otherwise
   dominates). One measured contention find: the bitwise operators
   (`^`, `>>`, `&`) run ~5× slower serially than `+`/`*`/`%` *and*
   serialize fully across threads under `gil=0` (0.83–1.06× at 2–8
   threads) — a contended dispatch path, not the GIL; a wave-12
   burn target alongside the §2c audit list. The 3.14
   `test_free_threading/` directory is cherry-picked for portable
   labels by the WS13 gap sweep.
3. **Graduation criteria (experimental → supported → default)**, each
   step its own RFC with its own baseline: (a) the scoped lane at
   `unexpected 0` with the race fixtures stable across repeated runs;
   (b) the §2c audit list burned to zero with the default-mode bench
   envelope held (the uncontended `GilCell` CAS and CAS-published
   caches must hold the committed floors); (c) the full-label sweep
   measured under `gil=0` and its delta enumerated; (d) the JIT
   re-enabled by its chartered RFC before any default flip. CPython's
   arc (PEP 703 experimental in 3.13 → PEP 779 supported in 3.14) is
   the pacing reference: no default flips until the numbers say so.

## Appendix: load-bearing symbols

| Concern | Symbols | Location |
| --- | --- | --- |
| Atomic heap | `Rc = Arc`, `GilCell`, `cell_guards_live` | `weavepy-vm/src/sync.rs` |
| GIL + mode flags | `GilState`, `maybe_yield_gil`, `no_gil_handoff`, `set_free_threading`, `free_threading_enabled`, `reenable_gil_for_extension` | `weavepy-vm/src/gil.rs` |
| GIL-assumed caches | `CacheSlot` / `CacheTable` | `weavepy-compiler/src/bytecode.rs` |
| GIL-assumed collector | `GC_STATE` | `weavepy-vm/src/gc_trace.rs` |
| Per-thread pools | `INTERN_POOL`; `BUILTIN_TYPES` + `SEED_BUILTIN_TYPES` | `stdlib/sys.rs`; `builtin_types.rs` / `vm_singletons.rs` |
| Cache epoch | `TypeObject::attr_version` | `weavepy-vm/src/types.rs` |
| Import lock | `IMPORT_LOCK` | `weavepy-vm/src/stdlib/imp_mod.rs` |
| Thread-confined tier-2 | `JIT` thread-local `JitState` | `weavepy-vm/src/tier2.rs` |
| C-API surface | `PyGILState_Ensure`, `TSTATE`, `PyInitConfig_SetInt("gil", …)` | `weavepy-capi/src/{lifecycle,pystate,pep741}.rs` |

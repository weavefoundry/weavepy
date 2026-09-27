# Dispatch, allocation, and memory performance

This pass reduces the time and memory WeavePy spends on dispatch, allocation,
cache bookkeeping, compilation, and garbage collection. It's a sequence of small
runtime increments, each measured against its immediate predecessor in paired
comparisons, plus several Python/C API compatibility fixes found during
validation. Candidates that didn't justify their costs were held; see
[Rejected experiments](#rejected-experiments).

Against the merge base, `82f7237`, the initial CI benchmark suite's geometric
means at exact head `7416faa` are 0.934 on Linux, 0.916 on Windows, and 0.953 on
ARM macOS. Windows `sumvm` is 0.981, resolving the 1.219 ratio retained at
`15d6ba2`. On ARM macOS, DeltaBlue moves from 1.227 initially to 0.983 on the
gate's standard retry, and JSON moves from 1.199 to 1.008; both batches remain
in the CI log. CI benchmark workloads, baselines, and thresholds are unchanged.
These results don't establish universal superiority over CPython: startup, peak
memory, and many individual workloads still trail it.

Local measurements use an Intel macOS (x86-64) desktop, Rust 1.94.0, standard
release settings, and CPython 3.14.5 built with PGO, LTO, and the tail-call
interpreter, with its GIL enabled and its experimental JIT unavailable. Each
increment compares frozen candidate and reference executables in paired,
interleaved cycles that alternate process order and discard a warmup cycle.
Each binary has its own warmed frozen cache, verified unchanged during timing.
Owned builds, tests, and profiles finish before timing; the desktop remained
active, and its load was recorded. Ratios are medians of paired
candidate/reference ratios, so lower is better, and aggregates are geometric
means. Suite workload means cover the 23 timed fixtures of the unchanged
24-fixture suite; process elapsed time, CPU, and peak RSS also include the
startup fixture. Focused probes usually use seven cycles, repeats eleven, and
startup batches 31. Because each increment is compared with its own
predecessor, ratios from different increments aren't additive. Raw samples,
profiles, frozen binaries, source snapshots, and held experiments were kept
under `target/performance/`, outside Git; the reusable probes are committed
under `tools/`.

## Implementation

### JIT attribute access

- Borrow intermediate results in guarded attribute chains. Adjacent reads of
  two to eight guarded fields run in one helper that pins only the final
  result, and a borrowed walk extends ordinary-instance chains to 32 cached
  fields. Replaced chains no longer retain discarded intermediate nodes
  through pins. Class versions, field names and indices, and result lanes stay
  guarded, and a miss replays from the first read with its original receiver.
- Accept polymorphic instance entries and owner-checked native-module entries
  in the borrowed chain helper. Module attribute chains no longer exhaust the
  soft pin limit; re-classed modules, shared cells, and conflicting borrows
  keep the original path.
- Reuse one advisory pin index per attribute guard when a read returns the
  same allocation. The hint owns nothing and is validated against the current
  activation, which removes reconstruction exits caused by pin pressure.
- Cache callback-free dynamic lookups for ordinary instances, plain classes,
  and imported modules, and finish simple properties and ordinary
  `__getattr__` functions through a bounded borrowed evaluator. A read that
  could run Python exits first and retries with a complete caller frame, which
  also fixes callbacks that saw stale loop locals. The helper borrows its
  pinned receiver and moves completed results instead of cloning them.
- Reuse an immediately repeated scalar read of the same field and local
  receiver within one block; the first read keeps all of its guards.
- Store attributes through one raw dictionary entry using the guard's interned
  name and cached hash. Keys that need Python equality resume in the
  interpreter before storing.

### Calls and small functions

- Recognize pure leaves that return an argument, a constant, a small integer,
  or an argument's attribute, and decode small field predicates such as
  `left.value < right.value` once. Both paths avoid the general evaluator's
  operand stack; stale caches, descriptors, and rich comparisons fall back.
- Read cached slots, not just instance-dictionary fields, in direct getters,
  decoded predicates, and general pure-leaf evaluation through one borrowed
  reader. No Python callback or owner release crosses a borrowed reference.
- Borrow the function pin and compilation artifact for certified scalar leaves,
  and reuse the same lookup to hand an owning resolution to the existing
  native-call machinery. Private code-identity maps use the VM's Fx word fold
  with address mixing instead of SipHash.
- Complete compiled bound methods that add an exact integer to one instance
  field without another frame, argument buffer, or pin table. The compiled
  artifact owns the update plan, and a result-lane mismatch never replays the
  store.
- Guard native `math` calls by module dictionary, key, hash, entry index, and
  function identity, and let bounded scalar leaves use `sqrt`, `fabs`, `sin`,
  and `cos` intrinsics. Numeric deoptimization restarts the pure body in an
  ordinary frame, preserving exceptions and tracebacks.
- Retry globals used by specialized keyword constructor calls as ordinary
  guarded objects instead of rejecting JIT analysis of the whole function, so
  later numeric loops compile. Argument binding and errors stay in the helper.
- Run saved zero-argument native methods for exact lists, dictionaries, sets,
  and strings in the quiet leaf loop after checking callable identity and
  receiver shape. Rejection caches hold weak references and check the registry
  generation, so neither keeps a method or receiver alive.
- Borrow the receiver as a one-element argument slice in generic and cached
  native bound calls, skip scratch vectors for zero-argument calls, and
  initialize fused-call integer scratch only when it's used.

### Interpreter dispatch

- Read up to eight consecutive cached instance fields through the root local
  in the quiet interpreter loop, borrowing intermediates and retaining only the
  final result. A miss after two completed reads materializes that prefix.
- Offer interpreter subscripts to a registered native `__getitem__` fast half
  that declines arguments needing callbacks, and cache its resolution per
  instruction behind the class attribute version and registration generation.
  Potentially last-reference containers are rejected before any read.
- Validate and take a suspended generator frame under one state borrow.
- Dispatch exact `set.pop` and scalar `set.remove` through the leaf builtin
  dispatcher. Remove requires both the lookup and the matched stored value to
  be leaves; subclasses, callbacks, and observers keep the full call path.

### Weak references and garbage collection

- Store exact 64-bit weakrefs' getter and callback in a shared fixed slot
  layout, share type-level call and representation methods, and drop the
  private alive and clear closures. Proxies, subclasses, class overrides, and
  32-bit targets keep dictionaries, and callbacks stay visible to GC traversal.
- Index weak-reference identities by hash, preserving registration order and
  newest-first clearing, and skip targets already tracked by the collector
  before snapshotting, so excluded targets aren't upgraded or cloned.
- Shrink collector handles from 72 to 64 bytes with four-byte position hints,
  stop suspect-queue eviction scans at the first zero budget, and subtract the
  evicted entry from the active count instead of recounting.
- Claim each allocation's `finalize_ran` flag atomically at dispatch. This
  fixes duplicate `__del__` calls under concurrent reclamation.
- Use a one-word shared owner for fixed slot layouts, reducing `SlotStorage`
  from 40 to 32 bytes and `PyInstance` from 136 to 128 bytes.
- Read an already published instance dictionary before allocation-only
  metadata, and allocate a class attribute cache's 32-entry table on first
  fill instead of embedding it.
- Accept a shared instance whose exact deferred-tracking flag is set in the
  pure-call release guard, so a stale collector-filter match no longer rejects
  a safe release. Final-owner releases are still rejected.

### JIT code and worker lifetime

- Use weak handles for return-type memoization and discount weak-registry
  clones during tier-cache eviction, so retired code metadata can be released.
  Live functions and native dependencies still count as owners.
- Retain process-unique class-version tokens in attribute and method guards
  without a strong class reference, so compiled readers no longer keep dead
  classes alive.
- Release a per-thread JIT engine's executable mappings when it's destroyed.
  Suspended generators moved to another thread materialize their state instead
  of entering the former worker's code.
- Query stack bounds on the caller's stack before embedding's first stack
  switch, so the pinned stacker Windows backend can't restore a temporary
  fiber's bounds and overestimate the caller's remaining stack.

### Standard library, parser, and compiler

- Give bounded `functools.lru_cache` wrappers dense recency links for exact
  integer and string keys, then for admitted native tuple keys (at most 64
  objects and eight tuple levels) and typed keys. The first unsupported key
  restores logical order and selects the callback-capable path. Private
  metadata reads use borrowed string keys, and counters update under one lock.
- Replace a removed set entry with the final entry instead of shifting the
  rest, and pop the final stored entry without calling hash or equality.
- Use the fast hasher and a bounded ancestor stack in the native pickle
  encoder, and resolve classes defined in exact `types.ModuleType` modules
  natively instead of falling back to the Python pickler.
- Validate ordinary Python AST fields and construct AST nodes from parser specs
  natively, with the Python path handling hooks, unusual inputs, observers, and
  GIL-disabled execution. Eval parsing builds only the expression spec.
- Transfer freshly parsed trees to the compiler, which folds them in place, and
  release the main module's tree before execution. Bytecode is byte-for-byte
  unchanged across the repository corpus.
- Answer parser location queries from a lazily built newline index instead of
  recounting every preceding newline after each statement.

### Startup and imports

- Import `site` once instead of calling `site.main()` again, which executed
  `.pth` lines twice, and initialize the process locale once.
- Require more evidence of sustained work before compiling during fresh module
  execution: import checkpoints count toward 16 times their normal interval,
  and nested scopes restore their predecessor on errors.
- Defer OSR for a single straight-line scalar range loop with fewer than 65,536
  remaining bytecode instructions. The deferral clears at the next call, and an
  explicit `WEAVEPY_JIT_THRESHOLD` bypasses the heuristic.
- Restore natural hot-code admission during startup at `7416faa`. The pass had
  deferred compilation throughout `site` initialization, which moved compiler
  initialization into the first timed workload. Single `site` initialization,
  nested scopes, and the import budget remain.

## Results

The following focused workloads are representative. Each ratio compares an
increment with its immediate predecessor, not with the merge base.

| Workload | Increment | JIT work | JIT work / CPython |
| --- | --- | ---: | ---: |
| Numeric loop after a keyword constructor | Keyword constructors | 0.056 | 0.052 |
| Linked-instance attribute reads | Attribute pins | 0.392 | 1.307 |
| Eight-field dictionary chain | Deep attribute chains | 0.069 | 0.954 |
| Warm `__getattr__` reads | Dynamic attributes | 0.125 | 1.787 |
| Warm Python-module chain | Module chains | 0.374 | 1.112 |
| `sin` and `cos` scalar leaf | Math calls | 0.342 | 0.439 |
| One-argument scalar callback | Borrowed scalar calls | 0.651 | 1.130 |
| Sustained slot predicate | Pure slot reads | 0.235 | 1.739 |
| Sustained Richards-style updates | Compiled field updates | 0.479 | 1.615 |
| Saved list length | Saved native leaf calls | 0.595 | 3.158 |
| Native deque indexed reads | Interpreter subscripts | 0.421 | 5.923 |

Library and compiler workloads improve in both execution modes. Ratios are
again against each increment's predecessor.

| Workload | Increment | JIT work | Interpreter work |
| --- | --- | ---: | ---: |
| Scalar LRU cycling, capacity 4,096 | Scalar LRU | 0.117 | 0.112 |
| Tuple-key LRU, capacity 4,096 | Native tuple LRU | 0.167 | 0.169 |
| Typed LRU, capacity 4,096 | Typed LRU | 0.188 | 0.189 |
| Set discard, 100,000 elements | Set removals | 0.0025 | 0.0023 |
| Set pop, 100,000 elements | Set dispatch | 0.494 | 0.523 |
| Warmed dynamic-module pickle | Dynamic modules | 0.00897 | 0.00943 |
| Text-heavy pickle encoding | Pickle memo | 0.584 | 0.571 |
| 20,000 generated functions | Parser locations | 0.108 | 0.106 |
| Python AST compilation, 2,000 functions | AST field checks | 0.323 | 0.331 |
| 20,000 compiled statements | Owned AST compilation | 0.829 | 0.858 |
| Standard generator pipeline | Generator resumes | 0.954 | 0.947 |

Garbage collection and retained-memory probes show the largest memory gains.
Population rows use 100,000 objects.

| Workload | Increment | JIT work | JIT peak RSS |
| --- | --- | ---: | ---: |
| Weak-reference population | Weakref snapshots | 0.496 | 0.931 |
| Weak-reference callbacks | Weakref snapshots | 0.480 | 0.933 |
| Self-cycles | Suspect eviction | 0.848 | 0.988 |
| Weak references, repeated | Fixed weakref storage | 0.886 | 0.786 |
| Weak-set cleanup, repeated | Fixed weakref storage | 0.868 | 0.809 |
| One-slot instances | Thin slot layouts | 0.976 | 0.963 |
| Integer-subclass instances | Compact GC positions | 0.979 | 0.971 |
| 10,000 classes with cold caches | Lazy class caches | 0.838 | 0.719 |
| Readers of classes with 64 KiB payloads | JIT class guards | 0.985 | 0.715 |
| Watched JIT code churn | JIT code lifetime | 1.080 | 0.760 |
| 1,000 compiling worker threads | JIT worker lifetime | 0.997 | 0.276 |

The startup admission rollback trades process startup for first-use work.
Against `15d6ba2`, fifteen paired cycles of the original numeric fixtures give
these results. Warm work is the second invocation in one process; its process
elapsed time includes both invocations.

| Fixture | Cold work | Warm work | Cold process elapsed | Warm process elapsed |
| --- | ---: | ---: | ---: | ---: |
| `sumvm` | 0.856 | 0.985 | 1.031 | 1.012 |
| `nested_loops` | 0.908 | 0.998 | 1.051 | 1.044 |
| `jitloop` | 0.932 | 1.001 | 1.018 | 1.041 |

Most increments leave the unchanged suite's local geometric means near their
predecessor's; the gains are concentrated in the targeted workloads. For
example, the fixed-storage increment's JIT suite means against `02339c7` are
1.002/0.999/1.004/0.995 for work, process elapsed, CPU, and RSS.

## Costs and regressions

The following costs remain. They're recorded rather than discarded because
later or aggregate measurements look favorable.

- Against `15d6ba2`, the startup rollback's repeated ordinary JIT startup
  elapsed/CPU/RSS ratios are 1.123/1.175/1.201. Isolated startup is
  1.129/1.173/1.196, and imports are 1.058/1.069/1.074. No-site startup, which
  doesn't enter the changed scope, repeats at 1.033/1.101 elapsed/CPU; that
  cost isn't attributed to a proven cause. Repeated ordinary startup takes
  1.840 times CPython's elapsed time and 1.461 times its peak RSS.
- The same rollback's full-suite JIT work/elapsed/CPU/RSS means are
  0.970/1.034/1.045/1.055 against `15d6ba2`. Repeated PyAES interpreter work
  is 1.097, and dictionary JIT peak RSS in the selected repeats is 1.154. The
  rollback resolves an admission tradeoff; it doesn't make the compiler faster.
- Against `02339c7`, fixed weakref storage retains dictionary-work costs of
  1.063/1.037 in JIT/interpreter mode at one million iterations, with CPU
  ratios of 1.063/1.039. Subclass calls at one million calls cost 1.031/1.034.
- Explicit `reference.__call__()` costs 1.324 in both modes, and explicit or
  saved weakref `repr` takes 2.09 to 2.34 times its previous work, because both
  now retain the wrapper owner or perform the full representation.
- Short first range calls cost repeated short calls: two- and eight-call
  checks repeat at 1.224 and 1.132 times their predecessor.
- Populated typed LRU wrappers retain about 3.8% more peak RSS at 10,000
  wrappers. Several increments also retain no-site JIT startup CPU costs, such
  as 1.054 for pure slot reads and 1.045 for compact GC positions.
- Large weakref populations still take 16.04 times CPython's work and 3.76
  times its peak RSS; callbacks take 25.62 times its work, weak-key lookup
  28.99 times, and weak-set cleanup 28.58 times. DeltaBlue's local workload
  still takes about 6.7 times CPython's time.
- At the fixed-storage increment, full-suite means against CPython are
  1.023/1.384/1.239/1.552 for JIT work/elapsed/CPU/RSS and
  2.178/1.997/1.889/1.316 for the interpreter. After the rollback, JIT means are
  0.987/1.434/1.295/1.625.

The startup costs above are relative to the deferral that the rollback
removed, not to the merge base. A separate 31-cycle `tools/bench_startup.py`
comparison of release builds of `7416faa` and `82f7237` on the same host shows
no net startup regression. Values are head/merge-base medians of paired
elapsed/CPU/peak RSS ratios.

| Launch | JIT | Interpreter |
| --- | ---: | ---: |
| `-c pass` | 0.976/0.994/0.971 | 0.951/0.970/0.967 |
| `-S -c pass` | 0.996/0.987/0.981 | 1.004/0.985/0.980 |
| `-I -c pass` | 0.970/0.961/0.971 | 0.976/0.959/0.969 |
| Import `json`, `datetime`, `collections`, `pathlib` | 0.925/0.923/0.964 | 0.990/0.988/0.974 |

Ordinary JIT startup still takes 1.423 times CPython's elapsed time and 1.429
times its peak RSS, and the import row takes 2.534 times its elapsed time.

## Rejected experiments

These candidates were measured and held. Their sources, binaries, and samples
remain in research storage; the committed fixtures and probes retain coverage.

- **Checked integer lowering.** Cranelift's overflow-checked arithmetic
  reduced compilation work and code size, and `jitloop` improved to 0.760
  cold. A single-executable diagnostic still showed execution costs from each
  new operation alone, such as 1.251 for addition in `while_sum`.
- **Polymorphic attributes.** Borrowing cached polymorphic field results cost
  1.019 in full-suite JIT work. Adding lazy polymorphic tables improved JIT work
  to 0.984, but not overall peak RSS, and retained startup and JIT-kernel costs.
- **Conditional field getters.** The last of four candidates reduced string
  selector work to 0.763 to 0.777, but dictionary operations still cost
  1.037/1.030 in JIT/interpreter work, without an application-level gain.
- **Borrowed field arguments.** Complete field callers improved to 0.652 to
  0.739 sustained JIT work, but descriptor and hook fallbacks cost up to 7.7%,
  including after an early cache-check refinement.
- **Interpreter field updates.** Three revisions halved interpreter Richards
  work, but the final one still cost 1.064 for mutable class values and 1.024
  for large-integer fields in sustained JIT work.
- **Native field updates.** A native-entry version reached 0.593 sustained
  Richards-style JIT work, but retained slot, setter, and mutable-class-value
  fallback costs. The compiled update plans above supersede it.
- **Profile-guided optimization.** An expanded profile gave 0.932 JIT work and
  0.893 peak RSS across the suite, but warm `pidigits` repeated at 1.239 and
  AES at 1.087. Windows distribution would also need `python314.dll` trained.
- **Other held candidates.** A native identity operation slowed DeltaBlue by
  11.0%. The escaped-local release extension, an earlier import-budget
  prototype, and weakref dictionary reservation experiments retained
  unrelated costs that their gains didn't justify.

`tools/bench_cold_jit.py` isn't an optimization. It compares first and second
workload calls, with compilation traces, and runs on Windows only after the
benchmark gate fails. Its Windows results showed cold `sumvm` work of 1.265
against warm work of 1.011, pointing to the first-use compilation cost that the
startup admission rollback removes.

## Compatibility fixes

- **C API class descriptors.** Native SQLAlchemy 2.1.1 failed because the fast
  C attribute lookup returned a class's Python descriptor unchanged. Class
  lookups now use the VM protocol, which calls `__get__(None, owner)`, and the
  legacy optional helpers distinguish missing attributes from descriptor
  errors. A Rust integration test calls the C APIs directly, since Windows
  `ctypes` import still requires `_ctypes.COMError`.
- **Lazy-iterator C slots.** Native lazy adapters such as
  `itertools.chain.from_iterable` mapped to a C type without `tp_iter` or
  `tp_iternext`. Cython reads those slots directly, so native SQLAlchemy result
  iteration returned no rows. The adapters now use `PySeqIter_Type`.
- **Union method binding.** `Union.__class_getitem__` is now a classmethod
  that requires exactly one item, fixing SQLAlchemy's compiled import.
  Together, these fixes let the native SQLAlchemy Core, synchronous ORM, and
  asynchronous query probes and the native Alembic migration checks pass.
  Native extensions can reenable the GIL, so this doesn't establish
  free-threaded native SQLAlchemy execution.
- **Concurrency.** Admitted bounded tuple keys no longer lose LRU entries with
  the GIL disabled, and upstream `test_lru_cache_threaded` now passes in that
  mode. Counter updates no longer lose increments. Finalizer claiming fixes the
  GIL-disabled `test_gc` `test_trashcan_threads` failure, and the process
  locale is no longer initialized concurrently.
- **Callback frames.** Deque leaf calls are guarded so that Python `__index__`
  coercion runs with its caller's frame published, and class-key equality
  callbacks also see a published caller frame.
- **Other behavior.** Set pop no longer calls hash or equality. Saved weakref
  calls and `repr` retain their wrapper owner, and the C API shim clears every
  watcher, including proxies, without publishing callbacks.

## Validation

The final head passes 414 VM tests, nine comparison-harness tests, 34 C API
unit tests, direct weakref C API integration, the explicit 1 MiB embedding
lifecycle, Clippy, no-default compilation, changed-file formatting, and the
repository artifact check. Its frozen CLI passes 112 regression runs, ten
additional GIL-disabled runs, 476 probe shapes, eight observer runs, and twelve
native SQLAlchemy/Alembic checks. All 28 checks in CI for exact head `7416faa`
pass, including all three benchmark gates, all nine ecosystem shards, platform
Rust tests, blocking regressions, GIL-disabled lanes, distribution checks,
formatting, Clippy, MSRV, the artifact policy, and conformance reporting.

Upstream validation is qualified. At the fixed-storage increment, all 60
selected group commands exit successfully, including 137 `test_weakref` tests
with seven skips per mode. The strict log audit detects an ignored GIL-disabled
`SET_FUNCTION_ATTRIBUTE on a shared function` exception. An identical-bytecode
oracle reproduces this GC-snapshot/function-construction race three of three
times on both that increment's reference and candidate. A fresh check of the
merge base `82f7237` reproduces it two of three times, versus three of three on
`7416faa`; CPython passes. The race predates this pass, but its frequency
isn't established as unchanged.

Existing differences also remain. Generic weakref reflection differs from
CPython: `vars(ref)` exposes an empty dictionary and `ref.__getstate__()` the
two native fields, whereas CPython rejects the former and returns `None` from
the latter. Callback attributes after cyclic or silent clearing still differ,
as does the lifetime of a weakref watching a temporary native method. Mixed
bound-method and class populations can need a second explicit collection where
CPython clears them in one.

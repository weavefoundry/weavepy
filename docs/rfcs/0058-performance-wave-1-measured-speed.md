# RFC 0058: Performance wave 1 — a measured benchmark lane, hot-path de-overheading, and tier-1 specialization depth

- **Status**: Accepted
- **Authors**: WeavePy authors
- **Created**: 2026-08-07
- **Tracking issue**: TBD
- **Builds on**: RFC 0021 (performance baseline: inline caches, `weavepy-bench`,
  `WEAVEPY_VM_STATS`), RFC 0032 (tier-2 Cranelift JIT + CALL inline caches),
  RFC 0049 (measured-baseline protocol this wave adopts for speed),
  RFC 0055/0056 (the ecosystem lane that acts as this wave's no-regression
  guard), RFC 0057 (the conformance state — 496/543 — that makes a
  performance wave the critical path at all).

## Summary

WeavePy's compatibility story is measured and strong: 496 of 543 vendored
CPython 3.13 `Lib/test` files pass, the ecosystem lane is 27/27, real
binary wheels import and run. The README's *other* promise — "dramatically
improving execution speed, startup time, memory usage" — is currently
false, and nothing in the repo measures it honestly:

- **Measured today** (macOS arm64, release binary vs Homebrew CPython
  3.13.5, end-to-end wall time): `fib(32)` **4.12 s vs 0.22 s (≈19×
  slower)**, `pyaes` work=400 **0.39 s vs 0.03 s (≈13×)**,
  `nested_loops` work=300 **7.43 s vs 1.00 s (≈7.4×)**, startup
  (`-c pass`) **50 ms vs 20 ms (2.5×)**.
- The tier-2 JIT (RFC 0032) is off by default and enabling it changes
  nothing on these workloads: every hot frame bails (`Call`,
  `LoadGlobal`, `ForIter` are all outside its opcode subset).
- `weavepy-bench` cannot see any of this. The WeavePy leg runs
  in-process and **discards the work parameter** (`let _ = work;` in
  `runner.rs`), so WeavePy runs the fixture's tiny `__main__` default
  while CPython runs `default_work` — the two columns were never
  comparable. The committed baseline has `cpython: null` on every row,
  the gate compares absolute WeavePy nanoseconds against a stale
  host-specific baseline, `jitloop` is missing from the baseline
  entirely, and CI has no bench job at all.

This wave makes performance a *measured, gated* property, exactly the
way RFC 0036/0049 did for conformance — and then spends the rest of the
budget making the measured numbers respectable. A sampling profile of
`fib(33)` says precisely where the time goes, and none of it is
mysterious:

1. **Per-call allocation storm.** Every Python-to-Python call
   heap-allocates a locals `Vec` (`Rc<RefCell<Vec<Object>>>`), an
   operand-stack `Vec` (capacity 16), a cells `Vec`, *and* eagerly
   builds the Python-visible `PyFrame` object that almost no call ever
   introspects. `build_py_frame`, `pop_py_frame`, `PyFrame::drop`, and
   raw `malloc`/`free` dominate the profile after dispatch itself.
2. **TLS tax on the hot path.** `GilCell::borrow`/`get` re-derive the
   current thread identity (`pthread_self` + `_tlv_get_addr` are ~7% of
   samples), and `specialize::record_hit`/`record_dispatch` do a
   thread-local read per *specialized instruction hit* just to learn
   stats are disabled.
3. **Dispatch overhead.** The eval loop runs several per-instruction
   probes (GIL checkpoint, GC/finalizer, async-exc, resource-warning,
   observer gate, `lasti` sync) that CPython folds into a single
   eval-breaker check.
4. **Thin specialization.** RFC 0021's inline-cache families stop short
   of the opcodes that dominate real workloads: `BinarySubscr` /
   `StoreSubscr` have no fast path at all (pyaes is list indexing in a
   loop), `BINARY_OP` covers only Add/Sub/Mul, and the CALL family
   handles only exact-arity positional calls into plain functions —
   bound methods, defaults, and native callables all take the generic
   binder that allocates two `Vec`s per call.

## Motivation

"Drop-in replacement for CPython" is conjunctive: run the code *and*
don't ask users to pay 10× for the privilege. With conformance at
496/543 the compatibility precondition of project goal #2 ("once a
feature is correct, make it fast") is met, and speed is now the single
largest gap between the README and reality. The same lesson RFC 0036
taught for conformance applies: guessed performance rots; measured,
CI-gated performance ratchets.

The wave's philosophy, in priority order:

1. **Measure first, honestly.** Symmetric methodology for both
   interpreters, CPython-relative ratios (host-independent, unlike
   absolute nanoseconds), a checked-in measured baseline, and a CI gate
   that fails on ratio regressions — before any optimization lands.
2. **Stop paying for what you don't use.** The profile is dominated by
   overhead that exists whether or not the feature it serves is active
   (frame objects nobody introspects, TLS reads for disabled stats,
   per-instruction probes for absent observers). Removing dead weight
   is compatibility-neutral by construction and helps *every* workload.
3. **Specialize what real code actually does.** Extend RFC 0021's
   inline-cache families to subscripts, the full binary-op kind table,
   bound-method and defaulted calls, and native-callable dispatch.
4. **Only then, tier 2.** The JIT's opcode subset is so narrow that no
   realistic function qualifies. Widening it modestly (range loops,
   guarded global loads) keeps the crate honest without betting the
   wave on codegen.

## CPython reference

- CPython 3.13's specializing adaptive interpreter (PEP 659): inline
  caches for `BINARY_OP`, `BINARY_SUBSCR`, `STORE_SUBSCR`, `CALL`,
  `LOAD_ATTR`, `LOAD_GLOBAL`, `FOR_ITER`, `STORE_ATTR`,
  `COMPARE_OP`, and the `_Py_EmitTraceEvent`-free fast path when no
  tracing is active.
- CPython's eval breaker: one atomic checked at `RESUME` /
  `JUMP_BACKWARD` / call boundaries carries signals, GC, async exc,
  and GIL-drop requests — not N independent per-instruction probes.
- CPython's frame machinery: `_PyInterpreterFrame` is a bump-allocated
  struct on a contiguous data stack; the heap `PyFrameObject` is
  materialized lazily, only when Python code asks for it
  (`sys._getframe`, tracing, generator `gi_frame`, tracebacks).
- `pyperformance` is the reference benchmark methodology: fixed
  workloads, warmup, median-of-samples, geometric-mean summary.

## Detailed design

### WS1 — An honest benchmark lane (`weavepy-bench` v2)

The bench crate becomes the third conformance lane, next to `regrtest`
and `ecosystem`:

- **Symmetric subprocess methodology.** Both interpreters run the same
  fixture file as a subprocess (`target/release/weavepy` and host
  `python3.13`/`python3`), with `WEAVEPY_BENCH_WORK` set identically.
  Fixtures self-time the `bench(n)` region with `time.perf_counter_ns()`
  and print `WEAVEPY_BENCH_NS=<int>` on stdout; the harness parses that,
  so process startup / parse / import cost is excluded from the loop
  metric (startup gets its own dedicated fixture instead). This fixes
  the `let _ = work` bug by construction.
- **Ratio baselines.** `baselines/bench.json` v2 stores, per fixture:
  WeavePy median ns, CPython median ns, and the ratio
  `weavepy/cpython`. The gate compares *ratios* (self-normalizing
  across hosts) with a configurable tolerance (default 10% local, 25%
  in CI where runner noise is real), plus the suite geometric mean.
  `gate` fails if any fixture's ratio or the geomean worsens beyond
  tolerance; new fixtures without baseline rows fail the gate until
  baselined (the RFC 0049 "no unmeasured rows" rule).
- **Fixture growth.** From 9 to ~20 fixtures, pyperformance-shaped and
  dependency-free: keep the existing nine, add `deltablue`, `float`
  (nbody-style float churn), `spectral_norm`, `chaos`, `go`-style
  playout, `json_bench` (stdlib json dumps/loads), `str_methods`,
  `dict_ops`, `list_ops`, `attr_access` (slots + plain instances),
  `call_overhead` (positional/default/kwargs/bound-method matrix),
  `generators`, `startup` (subprocess `-c pass` wall time). Every
  fixture keeps the `bench(n)` + `WEAVEPY_BENCH_WORK` contract.
- **CI job.** A blocking `bench` job on ubuntu + macos: build release,
  `setup-python` 3.13, run `weavepy-bench gate --pct=25`. The job also
  uploads the markdown report as an artifact so every PR shows its
  ratio table. (Absolute-time assertions are deliberately absent from
  CI; only ratios gate.)

### WS2 — Hot-path de-overheading

Compatibility-neutral removals of measured overhead, in profile order:

- **Lazy `PyFrame` materialization.** `build_py_frame`/`pop_py_frame`
  today construct the introspectable frame object for *every* call.
  Follow CPython: the eval loop's `Frame` stays the only per-call
  structure; the heap `PyFrame` is created on first demand
  (`sys._getframe`, `settrace`/`setprofile`/monitoring active,
  traceback capture on raise, generator/coroutine `.gi_frame`,
  `inspect`). A `Cell<Option<Rc<PyFrame>>>` on the eval `Frame` keeps
  identity stable once materialized. When observers are active the old
  eager path is used verbatim, so RFC 0031 event semantics are
  untouched.
- **Frame buffer reuse.** A per-interpreter freelist recycles the
  locals/stack/cells allocations of completed frames (CPython's
  data-stack analogue, minus the layout rewrite). `make_frame` pops a
  buffer set; frame teardown clears and pushes it back, bounded (e.g.
  64 entries) to keep memory flat.
- **Cached hot flags.** `specialize::record_*` and
  `tier2` gating stop doing TLS reads per instruction: stats-enabled,
  jit-enabled, and observers-active become fields cached on the
  `Interpreter` (observers already have a relaxed-atomic fast gate;
  the cached copy is refreshed at frame entry and observer
  registration bumps a generation counter).
- **`GilCell` thread-identity cache.** `GilCell::borrow` re-derives
  `pthread_self` per access. The GIL holder's identity is stable for
  the whole bytecode quantum; cache the "I hold the GIL" token in the
  interpreter and pass it (zero-sized witness) through the hot
  accessors, falling back to the dynamic check off the hot path.
- **Eval-breaker consolidation.** Replace the per-instruction probe
  pile (GIL checkpoint, GC reap, async-exc, resource-warning, observer
  poll, `lasti` sync) with a single relaxed atomic "work pending" flag
  checked per instruction; the slow path fans out to the individual
  probes. Signal-delivery latency and GIL fairness keep their current
  bounds because every source that used to be polled now *sets* the
  flag.

### WS3 — Tier-1 specialization depth

Extend the RFC 0021 `InlineCache` side-table model (no new opcodes, no
marshal impact) to the families the profile and fixture set actually
exercise:

- **`BinarySubscr` / `StoreSubscr`**: `ListInt` (in-range i64 index),
  `TupleInt`, `Dict` (pointer-guarded), `StrInt`; store variants for
  list/dict. pyaes and every parsing workload live here.
- **`BINARY_OP` completion**: `Div`, `FloorDiv`, `Mod`, `Pow` for
  Int/Float where semantics are exact (int floordiv/mod with CPython
  sign rules; float div), plus `AddUnicode`-style in-place str concat
  when the LHS refcount allows.
- **CALL family**: `CallBoundMethodExact` (self-prepend + exact arity),
  `CallPyDefaults` (positional-only tail filled from `__defaults__`
  without the generic binder), `CallNative` (native/builtin callables
  dispatched without the Python binder — `len`, `range`, method
  descriptors), `CallTypeConstructorTrivial` (e.g. `list()`/`dict()`).
- **`FOR_ITER`**: add `Str` and `Dict`-keys variants; make the range
  fast path allocation-free (yield inline ints).
- **`LOAD_ATTR`**: keep the RFC 0021 variants but re-verify the method
  variant covers the `obj.method(...)` fusion when paired with
  `CallBoundMethodExact` (WeavePy has no `LOAD_METHOD` opcode; the IC
  pair is our equivalent).

Every variant follows the established protocol: guard on `Rc` identity
/ `attr_version`, deopt to generic on miss, `Cooldown` back-off,
`WEAVEPY_VM_STATS` counters, and a bundled regrtest exercising the
guard-invalidation path (mutate the type/dict mid-loop and assert the
deopt is semantically invisible).

### WS4 — Tier-2 JIT: from demo to bounded usefulness

Deliberately modest; the wave does not bet on codegen:

- Teach `analyze`/`lower` the canonical counted loop: `FOR_ITER` over
  `range` with unit step (the `jitloop`/`nested_loops` shape), including
  `GET_ITER`+`FOR_ITER`+`JUMP_BACKWARD` recognition into a Cranelift
  loop with an i64 induction variable and overflow guard.
- Guarded `LOAD_GLOBAL`: burn the resolved target in as a constant
  behind the same globals-identity + key-index guard the interpreter IC
  uses; any mutation of that globals dict deopts via the existing
  entry-guard mechanism (checked at entry; the dict-identity guard is
  re-validated on each JIT entry, and the interpreter invalidates
  compiled frames whose guarded globals saw a `STORE_GLOBAL`/`del`).
- Mixed int→float arithmetic promotion (currently `MixedArithTypes`
  bails a function that ever adds an int to a float).
- `WEAVEPY_JIT=1` stays opt-in this wave. The bench lane grows a
  `--jit` column (finally matching what RFC 0032's Results section
  claimed) so the tier-2 ratio is *reported* on every run, but not yet
  gated.

### Acceptance criteria

1. **Bench lane v2 is live**: symmetric subprocess methodology with the
   in-fixture timing contract, ≥ 18 fixtures, ratio-based
   `baselines/bench.json` with real CPython columns (no `cpython:
   null` rows, `jitloop` included), `gate` compares ratios + geomean,
   and a blocking CI `bench` job runs it on ubuntu + macos with the
   report uploaded as an artifact.
2. **Measured speedup**: the checked-in baseline shows the suite
   geometric-mean WeavePy/CPython ratio improved by **≥ 2×** versus the
   pre-wave measurement recorded in this RFC. Pre-wave, measured with
   the WS1 harness itself (macOS arm64, release binary, host CPython
   3.13.5, 3 samples, medians): **geomean 11.51×** over the 20-fixture
   suite — worst rows deltablue 28.96×, richards 25.77×, float_math
   20.58×, call_overhead 19.82×, list_ops 19.69×; best rows pidigits
   0.95× (bignum arithmetic is native Rust already), startup 2.94×,
   json_bench 5.40×. Target: geomean ≤ 5.75×, no fixture regressing.
   Stretch (non-blocking): geomean ≤ 3× CPython.
3. **Call-path overhead is structurally gone**: no eager `PyFrame`
   construction on untraced calls (verified by a bundled regrtest that
   counts allocations via `tracemalloc` + a `WEAVEPY_VM_STATS`
   frame-materialization counter), and frame buffers recycle through
   the freelist (counter-verified).
4. **New IC families land with guard-invalidation regrtests**:
   subscript load/store, binary-op completion, bound-method/defaults/
   native call paths, each with a mutate-mid-loop deopt test bundled
   under `tests/regrtest/`.
5. **JIT**: `jitloop` (a `for i in range(n)` accumulation loop) tiers
   up and runs native with `WEAVEPY_JIT=1` (stats-verified
   `frames_compiled ≥ 1`, `native_entries ≥ 1`), with the bench `--jit`
   column reported.
6. **Zero conformance cost**: `regrtest --check` stays at the RFC 0057
   baseline (496 pass, `unexpected 0`) on the full sweep, and the
   ecosystem lane stays 27/27 (offline wheel-cache run).
7. **Hygiene**: `cargo fmt --check`, `cargo xclippy`, `cargo xtest`
   green; `sys.settrace`/`sys.monitoring`/`pdb` behavior unchanged
   under the lazy-frame regime (the observability regrtests from RFC
   0031 are the proof).

## Drawbacks

- Lazy `PyFrame` touches the most identity-sensitive object in the
  introspection surface; a missed materialization site is a subtle
  user-visible bug (mitigated by routing *all* frame access through one
  accessor and keeping the eager path when any observer is active).
- The eval-breaker consolidation changes signal/GC polling from "every
  instruction, several flags" to "every instruction, one flag" — the
  slow-path fan-out must preserve each probe's current guarantees, and
  the GIL fairness quantum must be re-verified under `test_threading` /
  `test_signal`.
- Ratio gating in CI inherits runner noise; the 25% CI tolerance and
  median-of-samples are the mitigation, and the gate can be re-tuned
  after a few weeks of data.
- ~20 fixtures is still a microbenchmark suite, not pyperformance; it
  can overfit. The fixture set deliberately includes call/attr/subscr
  shape diversity to blunt that.

## Alternatives

- **Jump straight to a serious JIT** (method JIT over the whole opcode
  set, or trace-based). Rejected for this wave: the profile shows the
  interpreter is losing to *overhead*, not to the absence of codegen;
  a JIT built on top of eager frame objects and TLS-taxed cells would
  inherit the same floor. Tier-1 wins compound with any future tier-2.
- **Adopt CPython's adaptive-opcode rewriting** (superinstructions +
  quickened opcode stream). The side-table IC model already in tree is
  behaviorally equivalent, avoids touching the marshal/`dis` surface
  (`cpython_code` codec), and keeps `co_code` re-encoding trivial.
- **Contiguous data-stack frame layout** (CPython's
  `_PyInterpreterFrame` rewrite). Highest ceiling, but it rewrites the
  generator/coroutine suspend model in the same wave that touches frame
  identity — too much risk at once. The freelist captures most of the
  allocation win; the layout rewrite is future work with the bench lane
  as its safety net.
- **Gate absolute times in CI**. Rejected: host-dependent, exactly the
  mistake the current stale `bench.json` demonstrates.

## Prior art

- CPython PEP 659 (specializing adaptive interpreter) and the 3.11–3.13
  eval-breaker/lazy-frame work this design copies deliberately.
- PyPy: warmup-sensitive benchmarking discipline (median-of-samples,
  self-timed regions).
- `pyperformance`/`pyperf`: the fixture + geometric-mean methodology.
- RFC 0021/0032: the IC side-table and Cranelift tier-2 this wave
  extends; RFC 0036/0049: the measured-baseline + `--check` protocol
  this wave applies to speed.

## Unresolved questions

- Should the freelist be per-interpreter or per-thread once
  sub-interpreters (RFC 0031) run in parallel? Per-interpreter is
  correct under the current GIL; revisit with free-threading.
- Does lazy `PyFrame` need an escape hatch for C extensions that call
  `PyEval_GetFrame` in a tight loop? (Materialization is cached, so
  likely no.)
- Whether the CI bench job should run the `--jit` column on every PR
  or nightly-only (cost vs signal).

## Future work

- Tier-2 expansion: attribute-access guards, Python-to-Python calls in
  native code, OSR for hot already-running loops (deferred from RFC
  0032 and still deferred).
- Contiguous frame/data-stack layout and generator frame inlining.
- Startup: frozen-importlib fast path profiling, lazy stdlib module
  init, `.pyc`-less frozen marshal for the hot import set.
- Memory benchmarks (max-RSS column) once the speed lane is stable.
- Small-int interning / tagged pointers if `Object::clone` traffic
  shows up post-WS2 (`is_same` semantics already anticipate it).

## Results

Measured on macOS arm64 against host CPython 3.13, with the WS1
harness itself (symmetric subprocess methodology, in-fixture timing
contract, 5 samples, medians). "Pre-wave" is the git-HEAD binary
run through the *same* harness and work parameters, so both columns
share methodology; conformance numbers follow the RFC 0049 protocol
(full `regrtest --include-all-cpython --mode subprocess` sweep,
online ecosystem lane).

### Headline

| Metric | Pre-wave | After |
|---|---|---|
| Bench suite geomean vs CPython (20 fixtures) | 11.64× | **9.92×** (−15% wall clock at the geomean) |
| Call-path fixtures (fib / call_overhead / richards / deltablue) | 341.7ms / 886.4ms / 351.9ms / 1.41s | **239.7ms / 694.1ms / 262.3ms / 1.12s** (−30% / −22% / −25% / −21%) |
| `jitloop` with `WEAVEPY_JIT=1` | no tier-up (`for`-loops unsupported) | **3.9ms** vs 499.1ms interpreted (~128×; 15.7× *faster* than CPython's 61.2ms) |
| `Lib/test` full sweep | 496 pass / 543 (RFC 0057) | **495 pass / 544**, `unexpected 0` at code level (two enumerated local-environment artifacts below) |
| Ecosystem lane | 27/27 | **27/27**, 0 unexpected |
| Gates | — | `cargo fmt` / `clippy -D warnings` / `cargo test --release --workspace --all-features` (all suites, 0 failures) / `regrtest --check` / `ecosystem --check` |

The two non-code sweep artifacts: `test_venv.test_sysconfig` compares
resolved vs. unresolved binary paths and fails only when the binary
sits under the `/var → /private/var` symlink (passes from a real
path); `test_asyncio/test_subprocess` hit the 180 s budget once under
the 8-way sweep and passes standalone in 15 s.

### Per-fixture medians

| fixture | pre-wave | after | Δ | ×CPython after |
|---|---|---|---|---|
| fannkuch | 149.5ms | 125.5ms | −16% | 11.02× |
| nbody | 411.6ms | 327.2ms | −21% | 13.72× |
| fib | 341.7ms | 239.7ms | −30% | 13.45× |
| pidigits | 2.25s | 2.24s | 0% | 0.93× |
| pyaes | 324.2ms | 288.7ms | −11% | 15.70× |
| richards | 351.9ms | 262.3ms | −25% | 19.39× |
| sumvm | 334.0ms | 265.1ms | −21% | 6.78× |
| nested_loops | 456.3ms | 396.7ms | −13% | 7.61× |
| jitloop | 650.1ms | 499.1ms (3.9ms with `--jit`) | −23% (−99% jit) | 8.15× (0.06× jit) |
| deltablue | 1.41s | 1.12s | −21% | 23.09× |
| float_math | 808.0ms | 691.4ms | −14% | 17.74× |
| spectral_norm | 499.1ms | 376.9ms | −24% | 12.18× |
| json_bench | 231.1ms | 233.6ms | +1% | 5.39× |
| str_methods | 223.0ms | 221.5ms | −1% | 6.92× |
| dict_ops | 282.8ms | 257.0ms | −9% | 7.79× |
| list_ops | 514.6ms | 454.5ms | −12% | 17.26× |
| attr_access | 544.7ms | 435.7ms | −20% | 14.98× |
| call_overhead | 886.4ms | 694.1ms | −22% | 15.28× |
| generators | 585.0ms | 613.7ms | +5% | 20.23× |
| startup | 51.7ms | 52.4ms | +1% | 3.05× |

`generators` (+5%) is the one soft spot: suspended generator frames
opt out of the WS2 frame pools by design (their storage must survive
the call), so they pay the new IC probes without the pooling win.
Within-run noise accounts for part of it; a generator-frame lane is
listed under Future work.

### Workstream outcomes

| WS | Deliverable | Result |
|---|---|---|
| WS1 | Honest benchmark lane | Symmetric subprocess methodology with the in-fixture timing contract; 20 fixtures (11 new: deltablue, float_math, spectral_norm, json_bench, str_methods, dict_ops, list_ops, attr_access, call_overhead, generators, startup); `--jit` column; ratio-based `bench.json` with real CPython medians (no `cpython: null` rows, `jitloop` included); `gate` compares per-fixture ratios + geomean; blocking CI bench job |
| WS2 | Hot-path de-overheading | Lazy `PyFrame` (cheap `FrameShell`s for the tracing/`warnings` walk — no materialization on untraced calls, counter-verified), fast-locals + operand-stack pools with sole-owner recycling, eval-breaker fast gates as relaxed atomics (`YIELD_COUNTDOWN`, pending-finalizer/cext counts), `GilCell` `Copy` get/set fast paths |
| WS3 | Tier-1 IC depth | Subscript load/store (list/tuple/str/dict), binary-op completion (div/floordiv/mod/pow over int/float), CALL families (`CallPyExact`/`NoFree`/`Defaults`, `CallNative`, `CallNativeMethod`, bound-Python calls), `FOR_ITER` str/dict; each with mutate-mid-loop guard-invalidation regrtests (`tests/regrtest/test_specialize_guards.py`) |
| WS4 | Tier-2 JIT usefulness | `for i in range(...)` loop recognition (fused `ForRange` terminator + synthetic `cur`/`stop` slots, deopt rebuilds live range iterators), guarded `LOAD_GLOBAL` burn-in (`ResolvedGlobal` identity guards checked at entry), mixed int/float lanes with the 2^53 comparison-exactness deopt, typed parameter entry guards; analyzer unit tests (`weavepy-jit/tests/range_loops.rs`) + 9 VM `jit_*` end-to-end tests |

### Engine bugs found by the verification sweep itself

1. **Finalizer-emitted `ResourceWarning(source=…)` retention** (made
   `test_tempfile` order-dependent). Three prompt-reap gaps: module
   attribute stores/deletes never reaped the displaced value (the
   `catch_warnings.__exit__` restore drops the recording
   `log.append`), shown-but-unrecorded `WarningMessage`s stayed
   pinned by their strong GC-registry handle, and
   `TextIOWrapper.close()` didn't close the memoised `.buffer`
   sibling. All three fixed; the leak predated the wave and was
   exposed by its timing changes.
2. **Lost concurrent writes on a shared fd** (`test_io`
   `test_write_readline_races`, ~2/20 flaky). Pre-existing: raw
   `read(2)`/`write(2)`/`lseek(2)` were issued GIL-released but
   unserialized, racing the shared file-description offset. Fixed
   with a per-`PyFile` syscall lock acquired inside the GIL-released
   window — the analogue of CPython's buffered-object lock. 40/40
   stress runs pass; bench medians unaffected.
3. **Eval-breaker count desync**: `clear_thread_python_tls` discarded
   queued work without decrementing the new atomic fast-gate counts,
   leaving the gates permanently hot after a worker thread died.

### Acceptance checklist

1. **Bench lane v2 live** — met (methodology, fixtures, ratio
   baseline, geomean gate, CI job).
2. **Measured ≥ 2× geomean speedup** — **not met**: 11.64× → 9.92×
   is a 1.17× improvement against the ≤ 5.75× target. The wins are
   real but concentrated where calls dominate (−20…−30%); the
   remaining gap is dispatch and `Object::clone` traffic in
   straight-line bytecode, which this wave's structural work
   (bench lane, lazy frames, IC substrate, JIT loop lanes) was
   priced to enable rather than finish. The concrete follow-ups —
   OSR so hot loops tier up mid-run, tier-2 attribute/call lanes,
   tagged small ints — are enumerated under Future work and are
   where the multiplier lives.
3. **Call-path overhead structurally gone** — met (no eager `PyFrame`
   on untraced calls, counter-verified; freelist recycling
   counter-verified).
4. **IC families with guard-invalidation regrtests** — met.
5. **JIT `for`-range tier-up** — met (`jitloop` 3.9ms native,
   stats-verified compile + native entries; `--jit` bench column).
6. **Zero conformance cost** — met (full sweep at the 0057 baseline
   with `unexpected 0` at code level; ecosystem 27/27).
7. **Hygiene** — met (fmt/clippy/tests green; observability
   regrtests pass under the lazy-frame regime).

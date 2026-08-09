# RFC 0059: Performance wave 2 — dispatch de-taxing, a unified eval breaker, and a JIT that runs real functions

- **Status**: Accepted
- **Authors**: WeavePy authors
- **Created**: 2026-08-08
- **Tracking issue**: TBD
- **Builds on**: RFC 0058 (the measured bench lane, frame pools, IC depth,
  and range-loop JIT lanes this wave extends), RFC 0032 (tier-2 Cranelift
  JIT), RFC 0021 (inline caches, `WEAVEPY_VM_STATS`), RFC 0049/0057 (the
  conformance baseline and protocol that act as this wave's no-regression
  guard), RFC 0055/0056 (the ecosystem lane, same role).

## Summary

RFC 0058 built the honest measurement rails and then missed its own
headline number: the acceptance bar was a ≥ 2× geomean improvement
(≤ 5.75× of CPython) and the wave landed at **9.92×**. Its Results
section is candid about where the residual lives — "dispatch and
`Object::clone` traffic in straight-line bytecode" — and enumerates the
follow-ups. This wave is those follow-ups, plus the profile evidence
that makes them concrete. A fresh sampling profile of the two workload
poles (`fib(33)` for the call path, `nested_loops(400)` for
straight-line bytecode) on the post-0058 binary shows, in on-CPU-sample
order:

1. **The probe chain ahead of every instruction.** Before `step`
   decodes an opcode, the eval loop runs seven independent gates: a
   `fetch_sub` RMW on the shared `YIELD_COUNTDOWN` atomic, two
   finalizable/suspect loads plus a thread-local `MAYBE_DEAD` take, a
   pending-C-ext-drop load, a resource-warning TLS probe, an
   unconditional `shell.lasti.store` per instruction, a
   `has_materialized` load, and an observer-count load. Each is
   individually "one relaxed atomic"; together they are a measurable
   fraction of `run_until_yield_or_return_impl`'s 35–40% self time.
2. **The `pthread_self` tax.** `gil::current_thread_id()` calls
   `libc::pthread_self()` through a dyld stub on **every**
   `GilCell::borrow`/`borrow_mut`/`get`/`set` — and the interpreter
   does several cell borrows per instruction (locals, caches, dict
   reads). `pthread_self` + `_tlv_get_addr` account for ~10% of
   samples in the loop profile and ~13% in the call profile.
3. **An imprecise `maybe_dead` gate.** `mark_maybe_dead()` is set
   whenever the operand stack shrinks — including when the displaced
   value is a plain `i64` int that can never carry a finalizer. In
   `nested_loops` (a loop that allocates nothing) the
   `total = total + …` store displaces an int every iteration, and
   because *some* `__del__`-bearing object is alive somewhere in any
   real program, every iteration pays a full
   `reap_dead_finalizable` scan: `GcState::reap_dead_finalizable_locked`
   is the #4 non-dispatch symbol in the loop profile.
4. **Stats probes that survive being disabled.**
   `specialize::record_dispatch` runs per instruction and
   `record_hit` per specialized hit; both resolve a `OnceLock` and a
   thread-local before discovering `WEAVEPY_VM_STATS` is off. They are
   visible in both profiles (~2%).
5. **A JIT that cannot run a single bench fixture's hot function
   except `jitloop`.** Tier-2 (RFC 0032 + 0058 WS4) enters only whole
   functions at `pc == 0`, and bails on any `Call` (except the fused
   `range` iterator), any attribute, any subscript. `fib` — three
   int compares, two subtractions, two calls — is `NotJitable` because
   of the two calls. The `JitFrame.entry_pc` field for on-stack
   replacement was added in RFC 0032 and is dead: the VM always passes
   0, the lowerer never reads it. A function that is *already* running
   its hot loop never tiers up at all.

Where the ceiling is: `jitloop` under `WEAVEPY_JIT=1` runs **15.7×
faster than CPython**. The machinery is sound; its opcode diet is the
bottleneck. This wave (a) deletes the interpreter's per-instruction
dead weight, and (b) teaches the JIT the two shapes that dominate real
Python — **calling another Python function** and **being already inside
a hot loop** (OSR) — so the call-path fixtures stop being permanently
interpreter-bound.

## Motivation

Same as RFC 0058's, sharpened by its outcome: the README promises
"dramatically improving execution speed", conformance is at 495/544
with the ecosystem lane 27/27, and the bench lane now measures speed
honestly — at 9.92× slower than CPython at the geomean. RFC 0058
retired the structural excuses (eager frames, missing IC families, an
unusable bench harness). What's left is exactly the two things this
wave targets: the interpreter pays rent per instruction for features
that are dormant, and the tier-2 JIT — the only component that has
demonstrated *faster-than-CPython* execution — cannot compile a
function with a call in it.

The wave's philosophy, in priority order:

1. **Precision before machinery.** The `maybe_dead` gate, the stats
   probes, and the thread-id derivation are not architectural problems;
   they are imprecise gates. Making them precise is
   compatibility-neutral by construction (the slow paths are unchanged,
   only entered less).
2. **One breaker, not seven probes.** CPython folds signal checks, GC,
   async exceptions, and GIL-drop requests into a single eval-breaker
   word checked at `RESUME`/`JUMP_BACKWARD`. WeavePy's granular probes
   each cost little and collectively cost a lot; they collapse into one
   relaxed load that is zero when nothing is pending.
3. **The JIT earns its keep on calls.** A tier-2 that can execute
   `fib`, `call_overhead`, and the numeric kernels of real code — not
   just closed-form loops — is where the geomean multiplier lives.
   OSR removes the "must be re-called to tier up" restriction that
   makes the JIT useless for script-level `for` loops.
4. **Measured, gated, honest.** Every claim lands in
   `baselines/bench.json` under the RFC 0058 methodology, the CI gate
   ratchets, and the full regrtest + ecosystem sweeps must hold at
   baseline (`unexpected 0`).

## CPython reference

- CPython 3.13's eval breaker: `_PyInterpreterFrame` execution checks
  one word (`eval_breaker`) at `RESUME` and `JUMP_BACKWARD`;
  signal-pending, GC-pending, async-exc, and GIL-drop requests are bits
  in that word, set by the requesting thread, consumed by the owner.
- CPython's refcount-driven finalization: `tp_dealloc` runs the instant
  the count hits zero — but only objects whose type has a finalizer do
  finalization work. Dropping an `int` never probes the GC.
- PEP 659's specializing interpreter assumes stats are compiled out in
  release builds (`Py_STATS` is a build flag, not a runtime probe).
- CPython 3.13's JIT (PEP 744) and every serious Python JIT
  (PyPy, GraalPy) treat Python-to-Python calls and OSR as the first
  two capabilities after straight-line arithmetic, for the same reason
  this wave does.

## Detailed design

### WS1 — Hot-path de-taxing (precision fixes)

**WS1a: cached thread identity.** `gil::current_thread_id()` gains a
thread-local `Cell<u64>` cache (const-initialized to 0, filled on first
use). The hot path becomes one `_tlv_get_addr` + load + branch instead
of a dyld-stub call into libpthread. Every `GilCell` operation
inherits the win. `clear_thread_python_tls` does not need to clear it —
a thread's pthread id is stable for its lifetime.

**WS1b: a precise `maybe_dead` gate.** `mark_maybe_dead()` becomes
conditional on the *dropped value*: a new
`Object::may_anchor_finalizable()` predicate returns `false` for the
scalar/leaf variants that can neither carry a `__del__` nor transitively
own something that does (`None`/`Unbound`/`Bool`/`Int`/`Long`/`Float`/
`Complex`/`Str`/`WStr`/`Bytes`/`Range`/`Code`/`Slice` and friends), and
`true` for anything that can reach the heap graph (`Instance`, `List`,
`Dict`, `Tuple`, `Generator`, `Foreign`, …). The eval loop's post-step
"stack shrank ⇒ maybe dead" heuristic is replaced by marking at the
drop sites themselves (`pop`/`truncate`/`mem::replace` wrappers), so an
int-displacing store in a hot loop no longer schedules finalizer scans.
The reap machinery itself is untouched — the gate just stops lying.

**WS1c: stats without probes.** `specialize::record_dispatch`/
`record_hit`/`record_miss` and friends compile down to a single
relaxed load of a `static STATS_ENABLED: AtomicBool` (set once at
startup from `WEAVEPY_VM_STATS`) with the recording body behind
`#[cold]`. The `OnceLock` + thread-local dance runs only when stats
are actually on.

**WS1d: lazy top-frame `lasti`.** The unconditional per-instruction
`shell.lasti.store(frame.pc)` moves to the points where another party
can actually observe a frame's `lasti`: call dispatch (the frame stops
being top), exception raise, observer events, generator suspension, and
`FrameShell::materialize`-time sync of the *top* shell from the live
interpreter `pc`. Between those points nobody can read the value.
Non-top frames already hold their frozen call-site `pc` exactly as
today.

### WS2 — One eval breaker

A single `static EVAL_BREAKER: AtomicU32` with bits for:

| bit | meaning | setter |
|---|---|---|
| `GIL_REQUESTED` | another thread is parked waiting for the GIL | GIL `acquire` slow path |
| `PENDING_FINALIZERS` | untracked-finalizable queue non-empty | `vm_singletons::push_*` |
| `PENDING_CEXT` | C-ext drop queue non-empty | cpyext release hooks |
| `ASYNC_EXC` | `PyThreadState_SetAsyncExc` pending | `gil::set_async_exc` |
| `SIGNALS` | signal arrived, handlers due | signal shim |
| `RESOURCE_WARNINGS` | finalizer-emitted warnings queued | warning queue |
| `FINALIZING` | daemon-thread kill on interpreter exit | shutdown |
| `TRACE_ERR` | an observer callback raised | trace machinery |

The eval loop's prologue becomes:

```text
countdown -= 1                       # plain local, no atomic
if countdown == 0 or EVAL_BREAKER != 0:   # one relaxed load
    run_slow_probes()                # the exact chain that exists today
    countdown = 128
```

plus the two probes that must stay per-instruction for semantic
fidelity, both already cheap and made precise by WS1:

- the prompt-finalization gate (`has_any_finalizable() &&
  take_maybe_dead()`) — CPython frees at refcount-zero *between*
  bytecodes, and the conformance suite observes `__del__` timing;
- the observer gate (`OBSERVER_COUNT` relaxed load) — `sys.settrace`
  line events are per-instruction when active.

Every existing setter keeps its current queue/flag; it additionally
sets its breaker bit, and the consumer clears the bit when the queue
drains. The granular probes are *unchanged* in the slow path, so
behavior under contention/tracing/finalization is identical — the fast
path just stops paying for their absence. The GIL cooperative yield
keeps its 128-instruction cadence via the local countdown (the current
shared `YIELD_COUNTDOWN` RMW per instruction goes away; a waiter now
also sets `GIL_REQUESTED`, so hand-off latency actually *improves*
from ≤128 instructions to ~immediate).

### WS3 — Tier-2 JIT: calls and OSR

Two capabilities, in dependency order:

**WS3a: Python-to-Python calls in native code.** `analyze` accepts
`Call` instructions whose callee is a `LOAD_GLOBAL`-resolved plain
Python function (the existing `ResolvedGlobal` burn-in machinery,
extended with a `PyFunction` case + identity guard) or the function
being compiled itself (direct recursion — the `fib` shape). Arguments
and return lanes are the existing `JitType` lattice (`Int`/`Float`/
`Bool`). Lowering emits a call through a new VM-provided runtime
helper:

```text
jit_call_py(ctx, callee_handle, argc, args: *const (bits, tag),
            out: *mut (bits, tag)) -> JitCallStatus
```

The helper reuses `tier2::try_enter`'s marshal path: if the callee is
`Compiled` and its entry guards pass, it runs native-to-native (the
common steady state — `fib` recursion never leaves machine code except
through the helper's thin trampoline); otherwise it boxes the args,
runs the callee through the ordinary interpreter call path, and
unboxes the result. A non-scalar result (bigint promotion, `None`,
object) returns `Unrepresentable`, which deopts the *caller* at the
call's `cache_pc` with the result value carried on the spill stack —
the deopt contract RFC 0058 established for range iterators, extended
with a one-object "materialized result" slot. Reentrancy runs under
the interpreter's existing recursion guard (the helper enters through
a `stacker::maybe_grow` checkpoint and honors `Py_SetRecursionLimit`
semantics via the same counter the interpreter uses).

Analysis changes: `Call` with `k` positional scalar-typed args pushes
the callee's *declared-scalar* return lane. Return-type inference is
deliberately simple and safe: the abstract interpreter assumes
`Unknown` unless the callee is `self` (recursion — the return lane is
the join of the function's own `Return` lanes, computed by the same
fixpoint) or a callee already compiled with a known scalar return. An
`Unknown` return bails the enclosing block into
`UnsupportedOpcode(CALL)` exactly as today, so the analyzer stays a
conservative subset.

**WS3b: on-stack replacement.** The dead `JitFrame.entry_pc` comes
alive. The interpreter's `JumpBackward` hotness hook, on crossing the
threshold while the frame is *already running* (today it only bumps a
counter consumed at the next fresh call), asks tier-2 for an **OSR
entry** at the loop-head `pc`: `analyze` already computes per-block
`livein` types from its fixpoint, so a second Cranelift entry block is
emitted per recognized loop head that (a) validates the loop-head
livein pack exactly like today's parameter entry guards, and (b) jumps
into the loop body block. The VM packs the *current* locals (plus the
synthetic `cur`/`stop` slots when the loop head is a fused `ForRange` —
recovered from the live range iterator on the operand stack), enters at
`entry_pc`, and the existing deopt/return contract applies unchanged.
OSR compilation shares the code cache: one `CompiledFrame` per
`CodeObject` with a table of guarded entry points (`pc 0` + each
`ForRange` head). Operand-stack-non-empty loop heads (other than the
single range-iterator slot that `ForRange` fuses away) stay
non-OSR-able — the analyzer's existing empty-boundary-stack invariant
already enforces this.

**WS3c: analyzer diet, minimally widened.** Only what the above needs:
`Call`(shapes from WS3a), `Return` lanes for recursion inference, and
`LoadGlobal → PyFunction` burn-in. No attributes, no subscripts, no
containers this wave — the analyzer's conservative-bail property is
the JIT's safety story and it stays intact.

### WS4 — Generator frame recycling

Generator/coroutine frames opt out of the RFC 0058 pools *while
suspended* — correct, their storage must survive. But on **exhaustion**
(`StopIteration`, `return`, `close()`, or `throw()` unwinding) the
frame's locals/stack storage is dead and today just drops. The
generator teardown path now donates both back through
`recycle_frame_allocs` under the same sole-owner (`Rc::strong_count ==
1`) rule. The `generators` fixture — the one regression RFC 0058
shipped (+5%) — is the acceptance probe.

### WS5 — Startup and memory measurement

**WS5a: startup.** Profile `weavepy -c pass` (currently 52ms vs
CPython's 17ms) and burn the top of whatever the profile says —
expected suspects from the import-machinery work: eager stdlib
singleton initialization, frozen-module decode, and encoding init.
This workstream is explicitly *measure-first*: no target beyond
"startup ratio must not exceed its current 3.05× and should move
toward ≤ 2×", because the profile hasn't been taken yet.

**WS5b: a max-RSS column.** The bench runner already spawns both legs
as subprocesses; it now records `ru_maxrss` (via `wait4` on Unix,
`PROCESS_MEMORY_COUNTERS` on Windows) per sample and lands a
`max_rss_bytes` field per row plus a `memory_ratio` in `bench.json`.
Reported in the markdown table; **not gated** this wave (the baseline
has to exist before a ratchet makes sense — same sequencing RFC 0058
used for the speed gate).

## Compatibility

- WS1/WS2 change *when* dormant machinery is probed, never what it
  does: every slow path is byte-identical, and the conformance suite's
  `__del__`-timing, tracing, signal, and GIL tests are the acceptance
  harness. The full-sweep regrtest baseline must hold at
  `unexpected 0`.
- WS3 extends the analyzer's accepted subset; everything else stays
  `NotJitable` and interpreted. Deopt paths are exercised by dedicated
  regrtests (mutating a burned-in global mid-recursion, overflow inside
  a native call, `sys.settrace` attach while native frames are live —
  which must disable tier-up exactly as today's observer gate does).
- The JIT remains **off by default** behind the `jit` feature +
  `WEAVEPY_JIT=1`, unchanged from RFC 0032/0058.
- `bench.json` gains fields; version bumps to 3; the gate reads both
  versions during the transition.

## Testing

1. `cargo test --workspace` (all existing suites) plus new unit tests:
   breaker-bit set/clear discipline, `may_anchor_finalizable`
   variant table, thread-id cache, OSR entry-guard packs, native-call
   marshal round-trips.
2. New regrtests under `tests/regrtest/`: `test_eval_breaker.py`
   (finalizer timing, async-exc delivery, signal latency under a hot
   loop), `test_jit_calls.py` + `test_jit_osr.py` (guard invalidation,
   deopt state fidelity, recursion limits, tracing attach) — the JIT
   ones auto-skip on non-`jit` builds.
3. The RFC 0049-protocol verification sweep: full
   `regrtest --include-all-cpython --mode subprocess` (must hold
   `unexpected 0`), ecosystem lane 27/27, `cargo fmt` / `clippy -D
   warnings`.
4. `cargo xbench run --update-baseline` + `gate` on the final binary;
   the JIT column re-measured with `--jit`.

## Acceptance criteria

1. **Interpreted geomean ≤ 7.0×** CPython on the 20-fixture suite
   (from 9.92×; ≥ 1.4× wall-clock improvement at the geomean), with
   no fixture regressing beyond the gate's 10%.
2. **`fib` under `WEAVEPY_JIT=1` ≤ 2.5×** CPython (from 13.45×
   interpreted; requires WS3a native recursion), and **`jitloop`
   stays ≤ 0.1×**.
3. **OSR demonstrably live**: a top-level (run-once) hot loop tiers up
   mid-execution, verified by `WEAVEPY_VM_STATS` counters and an
   end-to-end `jit_osr_*` VM test.
4. **`generators` fixture back at or below its pre-0058 median**
   (≤ 585ms at the RFC 0058 work parameter on the reference host).
5. **Startup ratio ≤ 3.05×** (no regression; movement toward 2×
   reported honestly).
6. **`bench.json` v3 with `max_rss_bytes`/`memory_ratio`** on every
   row, reported in CI.
7. **Zero conformance cost**: full-sweep `unexpected 0`, ecosystem
   27/27, fmt/clippy/tests green.

## Open questions

- Should the breaker be per-interpreter rather than global once PEP 684
  sub-interpreters run untrusted workloads concurrently? (Global is
  correct under the current GIL; revisit with free-threading, same
  answer as RFC 0058's freelist question.)
- OSR for `while` loops (non-`ForRange` backedges with empty boundary
  stacks) — the analyzer supports the shape; deferred unless it falls
  out free, to keep the wave's JIT risk bounded.
- Whether `jit_call_py`'s interpreted-callee fallback should count
  against the caller's hotness (it shouldn't dominate; measure).

## Future work

- Tier-2 attribute/subscript lanes (list element access is the next
  multiplier after calls — `pyaes`, `list_ops`).
- Superinstruction fusion in the tier-1 interpreter
  (`LOAD_FAST+LOAD_FAST`, `COMPARE_OP+POP_JUMP`) — deliberately
  deferred behind the breaker work, which changes the cost model it
  would be tuned against.
- Contiguous frame/data-stack layout (still deferred from RFC 0058).
- Memory-ratio gating once two waves of `max_rss_bytes` history exist.
- Small-int cache for boxed transitions (`Long` demotion churn) if
  post-WS2 profiles surface it.

## Results

Measured on macOS arm64 against host CPython 3.13 with the RFC 0058
harness (symmetric subprocess methodology, in-fixture timing contract,
medians). "Pre-wave" is the checked-in v2 baseline
(`crates/weavepy-bench/baselines/bench.json`, geomean 9.92×), so both
columns share methodology. Conformance follows the RFC 0049 protocol
(full `regrtest --mode subprocess` sweep).

### Headline

| Metric | Pre-wave | After |
|---|---|---|
| Bench suite geomean vs CPython (20 fixtures) | 9.92× | **8.51×** (−14% wall clock at the geomean) |
| Startup (`weavepy -c pass`-shaped fixture) | 52.4ms (3.05×) | **41.8ms (2.42×)**; frozen-module disk cache landed (WS5a) |
| Loop fixtures with `WEAVEPY_JIT=1` | `sumvm`/`nested_loops` no tier-up (whole-function entry only) | **`sumvm` 1.3ms, `nested_loops` 2.2ms, `jitloop` 3.5ms** via OSR — 30×/24×/17× *faster* than CPython |
| Max RSS (new WS5b column, reported not gated) | not measured | **~37 MiB ≈ 2.6× CPython** on most fixtures (3.5× on `generators`, 3.7× on `float_math`) |
| `Lib/test` full sweep | 432 total, pass 384 | **432 total, pass 385, unexpected 0** (`test_descr` newly passes; see GC fix below) |
| Gates | — | `cargo fmt` / `clippy -D warnings` clean; jit/vm/bench/compiler/cli suites 0 failures; `bench gate --pct=10` OK |

### Per-fixture medians (interpreted lane)

| fixture | pre-wave | after | Δ | ×CPython after |
|---|---|---|---|---|
| fannkuch | 125.5ms | 110.8ms | −12% | 9.18× |
| nbody | 327.2ms | 281.1ms | −14% | 11.94× |
| fib | 239.7ms | 220.7ms | −8% | 12.41× |
| pidigits | 2.24s | 2.19s | −2% | 0.92× |
| pyaes | 288.7ms | 243.1ms | −16% | 13.19× |
| richards | 262.3ms | 237.5ms | −9% | 17.85× |
| sumvm | 265.1ms | 179.4ms | −32% | 4.54× |
| nested_loops | 396.7ms | 266.1ms | −33% | 5.11× |
| jitloop | 499.1ms | 380.4ms | −24% | 6.20× |
| deltablue | 1.12s | 1.06s | −5% | 21.69× |
| float_math | 691.4ms | 647.5ms | −6% | 16.44× |
| spectral_norm | 376.9ms | 292.5ms | −22% | 9.44× |
| json_bench | 233.6ms | 230.3ms | −1% | 5.30× |
| str_methods | 221.5ms | 212.9ms | −4% | 6.67× |
| dict_ops | 257.0ms | 235.9ms | −8% | 7.19× |
| list_ops | 454.5ms | 408.9ms | −10% | 15.64× |
| attr_access | 435.7ms | 380.9ms | −13% | 12.63× |
| call_overhead | 694.1ms | 618.5ms | −11% | 13.84× |
| generators | 613.7ms | 515.1ms | −16% | 16.65× |
| startup | 52.4ms | 41.8ms | −20% | 2.42× |

### Workstream outcomes

- **WS1 (hot-path de-taxing)** + **WS2 (unified eval breaker)**
  delivered the broad interpreted-lane wins above: every fixture
  improved, with the loop-dominated ones (`sumvm` −32%,
  `nested_loops` −33%) gaining the most from the merged
  countdown-plus-`hot_gates` prologue and the precise `maybe_dead`
  gate. `test_eval_breaker.py` (new regrtest) covers prompt
  finalizers, cross-thread async exceptions, GIL fairness, and signal
  latency under the unified word.
- **WS3a (native Python→Python calls)**: `LOAD_GLOBAL`-resolved
  callees with inferable scalar return lanes compile to `CallPy`
  through the `wpjit_call_py` helper, with entry *and* post-call
  revalidation of global-identity and `__code__` guards, and boxed
  results routed through a deopt side channel. Covered by
  `test_jit_calls.py`. Known cost, reported honestly: for tree
  recursion (`fib`) the helper round-trip currently exceeds the
  interpreter's call cost (252ms vs 221ms with JIT on), so the JIT
  column is a small regression there; inline fast-path dispatch of
  hot callees is the enumerated follow-up.
- **WS3b (OSR at loop back-edges)** is the wave's multiplier:
  already-running hot loops tier up mid-activation, including live
  `range` iterator decomposition and reconstruction on deopt.
  `sumvm` 179.4ms → 1.3ms, `nested_loops` 266.1ms → 2.2ms under
  `WEAVEPY_JIT=1`, both beating CPython by >20×. Covered by
  `test_jit_osr.py` (overflow deopt, exceptions mid-loop, `break`
  with a live iterator).
- **WS4 (generator frame recycling)**: exhausted generators return
  their locals/stack allocations to the frame pool on every
  terminal path (return, escape, `close()`, `throw()`);
  `generators` −16% wall clock.
- **WS5a (startup)**: frozen stdlib modules now hit a persistent
  on-disk bytecode cache (FNV-1a source-validated, atomic writes,
  `WEAVEPY_FROZEN_CACHE` override) instead of re-parsing on every
  process. Warm startup 52.4ms → 41.8ms (2.42× CPython, from
  3.05×).
- **WS5b (memory rail)**: the bench harness captures `ru_maxrss` via
  `wait4`, reports `max RSS` and `×RSS` columns, and persists
  `max_rss_bytes` in the v3 report schema. Reported, not gated, as
  designed: WeavePy sits at ~2.6× CPython's RSS on most fixtures.

### Engine bug caught by the verification sweep

The full-sweep rerun flagged `test_descr.test_remove_subclass`: a
collected (White) class object stayed visible in
`__subclasses__()`. Root cause was pre-existing, not introduced by
this wave — the generation-rebuild sweep removed dead handles from
the GC index without purging their `SUSPECTS` entries, so the
suspect list's `Arc<TrackedHandle>` clone pinned the object
indefinitely; this wave's probe-cadence changes merely exposed the
timing. Fixed by purging suspect entries for White handles during
`rebuild_generations` (mirroring `untrack_id`), and `test_descr` now
passes, taking the sweep from 384 to 385 passes with `unexpected 0`.

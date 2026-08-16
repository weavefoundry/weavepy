# RFC 0067: Performance wave 5 — default-on JIT and native-to-native calls

- **Status**: Accepted
- **Authors**: WeavePy authors
- **Created**: 2026-08-15
- **Tracking issue**: TBD
- **Builds on**: RFC 0032/0058/0059/0061/0065 (the tier-2 Cranelift JIT
  and its four growth waves — this wave makes it the default and fixes
  its one measured regression), RFC 0059 WS2 / 0065 WS1 (the unified
  eval-breaker word and quiet-loop generation, which WS2 extends into
  native code), RFC 0058 (the bench lane whose gated column this wave
  re-points at the default configuration), RFC 0049/0057/0060 (the
  measured regrtest baseline as no-regression guard), RFC 0055/0056/
  0060/0066 (the ecosystem lane, same role).

## Summary

Four perf waves moved the interpreted bench geomean from 11.64× CPython
to the committed 8.04× — and left the tier-2 JIT, which runs the
numeric kernels **15–25× faster than CPython**, opt-in behind
`WEAVEPY_JIT=1` and a non-default cargo feature. The wave-4 results
section named the two blockers for flipping the default, and this wave
clears both:

1. **Calls out of native code rematerialize the entire interpreter
   activation.** A `CallPy` site today routes through `wpjit_call_py` →
   `Interpreter::call` → full argument binding → a fresh `Frame` with a
   `Vec<Object>` locals allocation → `run_frame` → the tier-2 entry path
   again: a thread-local borrow, a hash-map lookup, guard re-resolution
   against the globals dict, and **five fresh `Vec` allocations** for the
   marshal buffers — per call. That is why `fib`, whose body is fully
   inside the JIT subset, is *35% slower* under the JIT than interpreted:
   every recursive call pays ~1–2 µs of rematerialization to execute
   ~10 ns of native arithmetic. WS1 adds a **native-to-native fast
   path**: when a burned-in callee is itself compiled (including the
   self-recursive case, which is exactly `fib`), the call helper enters
   the callee's native code directly — scalar args marshaled
   register-to-buffer, pooled exchange buffers, no interpreter frame, no
   guard re-resolution on the pure-native path — and falls back to a
   *materialized* interpreter frame only when the callee deopts or
   raises mid-flight.
2. **Native code never polls the eval breaker.** A hot native loop (or,
   after WS1, a deep native call tree) runs to completion without ever
   checking `hot_gates::HOT` or offering the GIL — signals wait, other
   threads starve, `PyThreadState_SetAsyncExc` is delayed until the
   loop exits. Tolerable for an opt-in experiment; disqualifying for a
   default. WS2 gives compiled code the same cooperative discipline the
   interpreter's quiet loop has: a per-loop-iteration countdown that
   periodically calls a `wpjit_poll` helper (GIL hand-off inside the
   helper, no deopt needed) and takes the standard deopt exit at the
   loop header only when precise pending work (signals, finalizers,
   async-exc, finalization) actually exists; the call fast path polls
   the same word per call.

With both landed, WS3 flips the default: the `jit` cargo feature joins
the default feature set across `weavepy-vm` / `weavepy` / `weavepy-cli`
/ `weavepy-pylib`, and the runtime default becomes **enabled unless
`WEAVEPY_JIT=0`** (or `off`). WS4 re-points the bench lane's gated
column at the default configuration (JIT on), keeps the interpreted
column measured-but-ungated so interpreter-only progress stays tracked,
and re-baselines — with the full regrtest and ecosystem lanes green
under the default JIT as the no-regression bar, graduating RFC 0065's
advisory-only JIT sweep to the blocking configuration.

## Motivation

The README promises "dramatically improving execution speed"; the
measured reality after RFC 0065 is a 7.5×-slower-than-CPython default
binary with a 15×-faster-than-CPython JIT that nobody gets unless they
build with a non-default feature *and* export an environment variable.
Every conformance axis this project gates on — 515/548 `Lib/test`
files, 29/29 ecosystem rows, the relocatable artifact — measures the
*default* configuration; the JIT's advisory regrtest sweep has been at
`unexpected 0` since RFC 0065. The cheapest large perf lever available
is therefore not a new optimization but a *promotion*: make the fast
configuration the measured, gated, shipped one.

The two prerequisites are real engineering, not ceremony:

- The call rematerialization tax is the difference between the JIT
  helping and *hurting* on call-heavy code, and call-heavy code is the
  worst tail of the bench suite (`fib` 14.3×, `call_overhead`,
  `richards` 19.0×, `deltablue` 25.5× interpreted). A default JIT that
  slows `fib` down is not shippable.
- The breaker gap is a liveness bug the moment the JIT is on for
  everyone: `weavepy -c 'while n < 10**18: n += 1'` must respond to
  Ctrl-C, and a two-thread program with one hot numeric loop must not
  starve the other thread for the loop's lifetime. CPython checks its
  eval breaker at every `JUMP_BACKWARD`; native code needs an
  equivalent.

## CPython reference

- **Eval breaker at back edges.** CPython 3.13 reaches
  `_Py_HandlePending` via `CHECK_EVAL_BREAKER()` at `RESUME`,
  `JUMP_BACKWARD`, and call boundaries (`Python/ceval.c`,
  `Python/ceval_macros.h`); its own experimental JIT (PEP 744) compiles
  the same micro-ops, so instrumented breaker checks survive at loop
  back edges in native code too. WS2 adopts the same placement: loop
  back edges and call sites, with the GIL hand-off — which needs no
  interpreter state — handled inline in the helper, and only the
  work that *requires* the interpreter (signal handlers, pending
  finalizers, async exceptions) taking the deopt exit.
- **Calls without frame rematerialization.** CPython 3.11+'s
  `CALL_PY_EXACT_ARGS` specialization pushes the callee's
  `_PyInterpreterFrame` directly onto the datastack and continues in
  the same eval loop — no `PyEval_EvalCode` re-entry, no argument
  tuple. WS1 is the tier-2 analogue: a compiled callee's native entry
  is invoked directly with marshaled scalars, and the interpreter
  frame exists only if the callee actually deopts (CPython's
  `_PyFrame_ClearExceptCode` materialization discipline, inverted).
- **Recursion accounting.** CPython counts C-stack recursion
  (`tstate->c_recursion_remaining`) for every call regardless of tier.
  The fast path keeps WeavePy's equivalent (`recursion::enter`) on
  every native-to-native call, so `RecursionError` fires at the same
  depth in both tiers (`test_sys.test_recursionlimit` shape).

## Detailed design

### WS1 — Native-to-native calls: enter compiled callees directly

**Callee resolution (per outer entry).** `enter_compiled` resolves each
token in the activation's callee table against the tier cache: a callee
whose `CodeObject` is `Tier::Compiled` gets its `CompiledFrame` (plus
guard snapshot and its own callee table) stashed in the activation's
`CallCtx` as a `NativeCallee`; everything else stays `None` and keeps
using the interpreter path. Resolution is identity-based (the same
`Rc<CodeObject>` pointer the callee-code guard already pins), so a
`__code__` rebind can't alias. The self-recursive case — the caller's
own `CompiledEntry` — is recognized by code-pointer equality and shares
the caller's snapshot outright. Cold callees self-heal: they keep
tiering up through the interpreter path's hot counter, and the *next*
outer entry resolves them natively.

**Eligibility.** A compiled callee is native-callable when:

- every live-in slot is a parameter slot (`livein ⊆ [0, arg_count)`) —
  the analyzer already admits only exact-arity call sites, so arguments
  fill exactly these slots;
- every JIT-managed local lane is scalar (`Int`/`Float`/`Bool`) — pin
  lanes (`ListInt`/`ListFloat`/`Obj`) need a per-activation pin table
  built from a live interpreter frame, which a native call doesn't
  have. (Such frames keep the interpreter call path; the numeric
  call-tree shapes this WS targets never carry pins.)

**The fast path** (`wpjit_call_py`, before the existing slow path):

1. `recursion::enter()` — same tick the interpreter charges, so
   `RecursionError` depth is tier-independent; overflow parks the
   exception and returns `CallStatus::Raised`.
2. Per-argument lane check: each marshaled `(bits, tag)` must match the
   callee's compiled parameter lane; mismatch falls back to the
   interpreter path (which handles the general case bit-for-bit).
3. Guard check: the *self* case skips it (the caller's snapshot was
   validated at entry and is revalidated after every dirty call — see
   below); a non-self callee revalidates its own snapshot against the
   callee's `globals`/`builtins` dicts.
4. Observer check: `trace::any_observers_active()` falls back to the
   interpreter path, which fires the callee's `call`/`return` events.
   (Native code never fires events; a tracer installed mid-tree gets
   the same view it gets today for the *current* frame, and full
   fidelity for every frame entered after the check.)
5. Pooled buffers: locals / spill / tags / call-marshal buffers come
   from a per-thread free list (`tier2::BufPool`), not five `malloc`s.
6. `cf.enter` on a fresh `JitFrame` + `CallCtx` (parked/raised slots
   are per-activation; sharing them across nesting levels would
   corrupt the deopt protocol).
7. Exit translation:
   - `Returned` + tag matches the caller's expected lane → write
     through to the caller's ret slot, `CallStatus::Ok`.
   - `Returned` + tag mismatch → unpack, park, `CallStatus::Boxed`
     (the caller deopts after the call, exactly the existing protocol).
   - `Deopt` → **materialize** the interpreter frame at the deopt
     state — locals written back per lane (pooled `Vec<Object>`),
     operand stack rebuilt by the existing `rebuild_stack`, parked
     sub-call results pushed — and finish it with `run_frame` (a new
     `run_deopted_frame` entry that suppresses the spurious `call`
     event and routes a parked exception through `handle_exception`
     first). The result then flows back as `Ok`/`Boxed`/`Raised` as
     above. Deopt is the rare path; it pays interpreter cost, never
     loses state.
   - `Raised` → materialize likewise at `deopt_pc + 1` and run the
     exception machinery so the callee's frame lands in the traceback
     exactly as the interpreter path would report it.

**Guard-dirtiness discipline.** Today `wpjit_call_py` re-resolves every
caller guard after *every* call, because the callee may have rebound a
burned-in global or `__code__`. On a pure-native call tree that work is
provably unnecessary: compiled code contains no `STORE_GLOBAL`, no
attribute stores outside the guarded scalar lanes, and no opaque calls
— nothing that can rebind a global. `CallCtx` gains a `dirty` flag: the
interpreter fallback path sets it (arbitrary Python ran), a nested
native call propagates the callee's flag, and the post-call guard
revalidation runs **only when the completed call was dirty**. `fib`
does zero guard lookups per million calls; a tree that touches Python
anywhere revalidates exactly as today.

**Stats.** `JitStats` gains `native_calls` (fast-path entries),
`native_call_fallbacks` (eligible token, fast path refused: lane
mismatch / observers / recursion), and `native_call_deopts`
(materialized mid-call), surfaced through `WEAVEPY_VM_STATS` and
asserted by the new unit tests.

### WS2 — The native eval breaker: poll at back edges and calls

**The poll helper.** A new registered helper
`wpjit_poll(frame) -> i64` does, in order: `maybe_yield_gil()` (the
hand-off needs no interpreter state — another thread runs, we resume);
then returns nonzero iff precise interpreter-required work is pending —
`hot_gates::load() != 0` (signals, parked finalizers, C-ext drops,
async-exc, finalizing) or an observer generation change. Nonzero means
"take the deopt exit"; the interpreter's prologue then handles the work
with full fidelity and the loop re-enters natively via OSR once quiet.

**Back-edge placement.** Every lowered loop back edge (both the fused
`ForRange` header re-entry and ordinary backward jumps) decrements a
per-activation countdown register seeded with `JIT_POLL_STRIDE` (1024
iterations — a tight native loop covers a stride in single-digit
microseconds, far inside the 5 ms GIL switch interval and the latency
CPython's per-`JUMP_BACKWARD` check delivers in practice). On zero it
calls `wpjit_poll`; a nonzero return takes the **standard deopt exit at
the loop-header pc** — which the existing spill/rebuild machinery
already handles, since OSR entries describe exactly that state — and a
zero return resets the countdown and continues. Cost on the quiet path:
one register decrement and one predictable branch per iteration.

**Call-site placement.** The WS1 fast path calls `wpjit_poll` before
entering the callee (deep recursive trees have no back edges). A
pending-work poll at a call site doesn't need a mid-loop deopt: the
fast path just falls back to the interpreter call path for that call,
which runs the prologue naturally.

**What stays exact.** The helper never handles signals or finalizers
itself — it only reports that the interpreter must. Spurious wakeups
(a `HOT` bit consumed by another thread between poll and deopt) cost
one cold prologue pass, the same contract the interpreter's quiet loop
already documents.

### WS3 — Default-on: cargo features and the runtime switch

- `weavepy-vm`: `default = ["jit"]`; likewise `weavepy`
  (`jit = ["weavepy-vm/jit"]`), `weavepy-cli`, and `weavepy-pylib`
  (the Windows `python313.dll` ships the same default). Building
  without the JIT remains one flag away
  (`--no-default-features`), and the feature graph is unchanged —
  only the defaults move.
- Runtime: `WEAVEPY_JIT` unset now means **enabled**; `0`, `off`, or
  empty disables; any other value enables (so existing `WEAVEPY_JIT=1`
  scripts keep working). `WEAVEPY_JIT_THRESHOLD` keeps its meaning and
  its default (50).
- An unsupported host ISA (Cranelift can't target it) degrades to the
  interpreter exactly as today — `JitEngine::new()` returning `None`
  disables tier-up for the thread.

### WS4 — Measurement, gating, and the honest re-baseline

- The bench harness's **gated column measures the default binary** —
  which now has the JIT. The old `--jit` flag (add a `WEAVEPY_JIT=1`
  column) retires; a new `--interp` flag adds a `WEAVEPY_JIT=0` column
  so interpreter-only progress stays a measured, committed number
  (baseline schema v5: `weavepy` = default mode, `interp` replaces
  `jit`). The interpreted column is reported, never gated — the gate
  follows what ships.
- `baselines/bench-macos-aarch64.json` is re-measured and recommitted
  under the new schema. The headline target: **default-mode geomean ≤
  5.2× CPython** (≥ 35% over the wave-4 committed 8.04×), with `fib`
  under the default JIT **faster than interpreted** (retiring the
  wave-4 regression) and no interpreted-column regression beyond
  noise.
- Conformance: the bundled + vendored regrtest sweep and the ecosystem
  lane run with the default (JIT-on) binary and must hold
  `unexpected 0` — RFC 0065's advisory JIT sweep graduates to *the*
  blocking configuration. `WEAVEPY_JIT=0` remains available to bisect
  any future suspect straight back to the interpreter.
- New unit tests: native self-recursion (`fib` compiles, fast-path
  counter advances, output matches the interpreter), a compiled
  non-self callee, deopt-mid-callee (int overflow to bigint inside a
  native call tree), raise-mid-callee (traceback shape matches the
  interpreter path), recursion-limit parity, guard-rebind through a
  dirty call, GIL hand-off progress from a second thread during a hot
  native loop, and KeyboardInterrupt delivery into a native loop
  (subprocess, unix only).

## Acceptance criteria

1. `cargo build -p weavepy-cli` (default features) produces a binary
   with the tier-2 JIT active by default; `WEAVEPY_JIT=0` restores the
   pure interpreter.
2. `fib` and `call_overhead` under the default binary beat their
   interpreted numbers; `fib`'s wave-4 JIT regression (+35%) is
   retired.
3. Default-mode bench geomean ≤ 5.2× CPython on the committed
   macOS-aarch64 baseline; interpreted column within noise of wave 4.
4. Native loops respond to signals and hand off the GIL: the two new
   liveness tests pass, and no regrtest signal/threading row regresses.
5. Full `cargo test --workspace --all-targets --all-features`, the
   bundled regrtest sweep, and the ecosystem lane are green with the
   default (JIT-on) binary, `unexpected 0`.
6. `WEAVEPY_VM_STATS` reports the three new call-path counters.

## Drawbacks

- **Compile-time and binary-size cost for everyone.** Cranelift joins
  the default dependency graph (~35 crates, ~2 MB of the release
  binary). Accepted: the JIT is the product now, and
  `--no-default-features` remains for size-constrained embedders.
- **A breaker poll taxes the hottest loops.** The decrement/branch per
  iteration and the periodic helper call show up on `sumvm`-shaped
  kernels (measured in Results; expected low single-digit percent).
  Liveness for a default configuration is not optional, so this is
  paid deliberately.
- **More `unsafe` surface in the call helper.** The fast path
  manufactures nested `JitFrame`/`CallCtx` activations by hand. The
  same invariants as `enter_compiled` apply and are documented at each
  site; the deopt fallback reuses the existing rebuild machinery
  rather than duplicating it.
- **Deopt inside a native callee is slower than never JITting it**
  (materialize + finish interpreted). Bounded by the tier cache's
  existing discipline — a chronically deopting callee still completes
  correctly, and its *callers* stop seeing it as native only if its
  code object is invalidated; in practice the analyzer's lane
  admission keeps chronic deopt shapes out of the callee set.

## Alternatives

- **Direct machine-level self-calls** (Cranelift `call` to the
  function's own symbol with stack-allocated exchange buffers) —
  rejected for this wave: it saves the helper round-trip (~10 ns) but
  forks the deopt protocol into a second, stack-walking variant; the
  helper fast path captures the dominant win (the ~1–2 µs
  rematerialization) at a fraction of the risk. Sketched in Future
  work.
- **Trampoline-free calls via inlining** (analyze the callee's TFunc
  into the caller) — rejected: exact-arity scalar inlining is a real
  optimization but changes deopt-state bookkeeping fundamentally
  (two frames' locals in one native activation) and is unnecessary to
  clear the default-on bar.
- **Signal-based preemption instead of polling** (deliver a signal and
  patch the native code's return address) — rejected outright:
  platform-specific, incompatible with the GIL hand-off's cooperative
  invariants, and CPython itself polls.
- **Keeping the JIT opt-in and optimizing the interpreter further** —
  the wave-over-wave record (11.64 → 9.92 → 8.51 → 8.64 → 8.04) says
  interpreter levers now yield ~6–15% each; the JIT column is 10–300×
  better on the kernels it covers. The leverage argument is not close.

## Prior art

- CPython 3.11 `CALL_PY_EXACT_ARGS` / `_PyInterpreterFrame` datastack
  frames (frame push without C recursion), and 3.13's PEP 744 JIT
  keeping `CHECK_EVAL_BREAKER` at back edges.
- PyPy's JIT-to-JIT calls with virtualized frames materialized only on
  deopt ("virtualizables") — WS1's materialize-on-deopt is the same
  shape, single-tier.
- V8/SpiderMonkey interrupt checks: countdown registers polled at loop
  back edges and function entries, deopting to the runtime only when
  the interrupt bit is set.

## Unresolved questions

- Should the poll stride adapt (shorter while `threading.active_count()
  > 1`)? Deferred until a measured workload shows the fixed stride
  starving anything; 1024 iterations is ≪ the 5 ms switch interval on
  any realistic kernel.
- Whether the callee-resolution snapshot should be invalidated
  mid-activation when a dirty call compiles *new* frames (today: next
  outer entry picks them up). Left as-is — self-healing, and the
  alternative is a cache-coherence protocol for a per-activation
  table.

## Future work

- Direct Cranelift-level self-calls (drop the helper round-trip for
  the self-recursive token) once the helper path's counters show it
  matters.
- Generator/iterator frames in tier 2 (persistent native activations)
  — the largest remaining `NotJitable` class (`generators` 15.8×).
- Method calls on the RFC 0065 attribute lane (`richards`/`deltablue`
  shapes: `self.method(...)` with a class-version guard).
- Linux and Windows bench baselines graduating from
  `--allow-missing-baseline` to strict, now that the gate measures the
  shipped configuration.
- The 16-byte `Object` / tagged-pointer investigation (unchanged from
  RFC 0065's list).

## Results

Measured on macOS aarch64 (the committed baseline host), release
build, CPython 3.13 as the reference column.

**Headline: suite geomean 3.33× CPython under the default (JIT-on)
binary — from wave 4's committed 8.04× interpreted, a 59%
improvement against a ≥ 35% target.** The committed
`baselines/bench-macos-aarch64.json` is re-recorded under schema v5
(`weavepy` = default mode, `interp` = the new `WEAVEPY_JIT=0`
column; the retired `jit` column is gone) and the gate passes
against it at the CI threshold.

Selected rows (medians; default / interpreted / CPython, ratio =
default ÷ CPython):

| fixture | default | interp | CPython | ×CPython |
|---|---|---|---|---|
| fib | 30.3ms | 189.3ms | 17.8ms | **1.71×** |
| sumvm | 2.1ms | 128.7ms | 38.3ms | **0.05×** |
| nested_loops | 2.7ms | 224.6ms | 50.4ms | **0.05×** |
| jitloop | 3.4ms | 226.3ms | 61.4ms | **0.05×** |
| jitkernels | 26.3ms | 297.3ms | 35.6ms | **0.74×** |
| pidigits | 2.20s | 2.20s | 2.40s | 0.92× |
| startup | 35.5ms | 35.1ms | 18.7ms | 1.90× |

- **The wave-4 `fib` regression is retired**: 6.2× faster than
  interpreted (was **35% slower** under the opt-in JIT), on the
  strength of WS1's native self-recursion. The
  `native calls / fallbacks / deopts` counters confirm the fast
  path carries the recursion (`WEAVEPY_VM_STATS=1` reports all
  three).
- **The eval-breaker poll's cost is invisible at suite level**: the
  loop kernels still run at 0.05× (20× faster than CPython) with a
  poll every 1024 back edges. Beyond the RFC's plan, `wpjit_poll`
  also **revalidates the activation's burned-in globals** once per
  stride: a cross-thread rebind of a guarded flag (the
  spin-on-a-flag idiom, which burns as a constant) deopts within
  one stride instead of never — without this the default-on flip
  would have shipped a liveness bug for a common idiom.
- **`call_overhead` (11.6×) does not improve**: its shapes (kwargs,
  defaults, bound methods) are outside the tier-2 subset entirely.
  What the wave *did* fix is the new tax the default-on flip would
  have imposed on such code — the per-activation tier-up probe
  (thread-local borrow + pointer-keyed map lookup) is now gated by
  a `JitHint` flag denormalized onto the code object, one relaxed
  load for code the JIT already rejected. Method-call support is
  Future work.
- **Interpreted column within noise of wave 4** (committed
  alongside; `fib` interp 189.3ms vs the wave-4 committed shape).
- **Deopt backoff (landed post-review, macOS bench-gate regression)**:
  `deltablue` exposed compiled frames whose activations chronically
  side-exit (747 deopts across 6 compiled frames — marshal-in +
  native entry + frame materialization per call, all to finish in
  the interpreter anyway), costing ~4% against `WEAVEPY_JIT=0`. A
  per-code deopt budget (64, sized like the OSR failure budget)
  retires such code to `Tier::NotJitable` + `JitHint`, exactly as an
  analyzer rejection would; `deltablue` under the default JIT now
  measures at parity with the interpreter, and its healthy compiled
  frames keep their native entries.

Conformance and tests, all under the default (JIT-on) build:

- `cargo test --workspace` green (157 vm lib tests including the
  eight new WS1/WS2 tests: native self-recursion with advancing
  fast-path counters, cross-function native calls through the
  generation-cached table, deopt-mid-callee (i64 overflow → exact
  bigint), raise-mid-callee (ZeroDivisionError through a native
  caller), recursion-limit parity, argument-lane-mismatch fallback,
  cross-thread flag observation through the poll, and chronic-deopt
  retirement at exactly the budget).
- The bundled regrtest sweep and bench gate pass; the sweep now
  *is* the blocking JIT configuration (RFC 0065's advisory sweep
  graduated). `tests/regrtest/test_eval_breaker.py` grew two
  native-loop sections: SIGALRM delivery into a JIT-shaped kernel
  (handler rebinds the guarded global; the loop must observe it),
  and `_thread.interrupt_main()` raising KeyboardInterrupt *inside*
  a native loop with no other exit — which also proves the poll's
  GIL hand-off, since the poking thread could not run otherwise.
- One planned test was dropped as structurally impossible rather
  than skipped: "guard rebind through a dirty call" cannot be
  constructed today because a burned-in callee must be
  ret-lane-analyzable, and the analyzable subset contains no
  `STORE_GLOBAL` — no call the fast path can make is able to rebind
  a caller's guard same-thread. The dirty-flag discipline stays as
  a correctness belt for future subset growth (method calls will
  admit arbitrary Python); the cross-thread rebind path is what's
  reachable, and it is tested (unit + regrtest).

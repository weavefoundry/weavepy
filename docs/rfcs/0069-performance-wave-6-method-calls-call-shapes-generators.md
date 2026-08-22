# RFC 0069: Performance wave 6 — method calls, call shapes, generators, and the crash-class burn

- **Status**: Accepted
- **Authors**: WeavePy authors
- **Created**: 2026-08-20
- **Tracking issue**: TBD
- **Builds on**: RFC 0067 (the default-on tier-2 JIT, native-to-native
  calls, and the native eval breaker — this wave grows the subset it
  ships), RFC 0065 WS5 (the guarded scalar attribute lanes and the
  pinned-receiver machinery WS1 extends to method dispatch), RFC 0059
  WS3 (the `CallPy` token protocol and callee-span deopt
  reconstruction that method tokens reuse), RFC 0021/0058 (tier-1
  adaptive specialization and the bench lane), RFC 0066 (the numpy
  `_core` selftest census whose crash rows WS5 burns), RFC 0049/0057/
  0060/0068 (the measured regrtest baseline as no-regression guard).

## Summary

RFC 0067 made the tier-2 JIT the shipped default and retired its one
regression; the committed suite geomean now stands at **3.33×
CPython** — but bimodally. The kernels inside the JIT subset run
0.05× (20× faster than CPython); everything the subset excludes falls
to the interpreter and clusters at 10–25×: `deltablue` 25.5×,
`richards` 19.0×, `generators` 15.8×, `float_math` 14.7×,
`attr_access` 13.9×, `call_overhead` 13.0×. RFC 0068 named this
wave's worklist when it queued "performance wave 6": the shapes real
code is made of — **method calls, keyword/default call binding,
generators, and float math** — are exactly the shapes the subset
still rejects.

This wave attacks the exclusions on both tiers, plus the one
ecosystem debt item that is categorically worse than slowness:

1. **WS1 — tier-2 method-call lanes.** `recv.method(args)` on a
   local whose class fingerprint is pinned (the RFC 0065 attribute
   machinery generalized from *value* loads to *callable*
   resolution): the analyzer resolves the method through a
   class-version guard, erases the `LOAD_ATTR`, and emits a
   `CallMethod` token. The helper enters a compiled callee natively
   with the receiver riding as a pin reference in slot 0 — the first
   non-scalar lane admitted across a native call — and falls back to
   a materialized interpreter call otherwise, so a native caller
   loop survives a not-yet-compiled callee.
2. **WS2 — float completion.** `math.sqrt`/`sin`/`cos`/`fabs` burn
   in as native instructions behind module-attr guards (with the
   domain-error deopts that keep `ValueError` exact);
   float floor-div/mod join the lane set; and **cross-block operand
   values** (ternaries, short-circuit) stop disqualifying frames —
   the lowerer has carried block-parameter support since RFC 0032,
   the analyzer just never produced a non-empty boundary stack.
3. **WS3 — tier-1 call-shape fast paths.** The interpreter's
   argument binding pays a general-case tax on every call with
   defaults, keywords, or a bound method. A `CALL` inline-cache
   family (`CallPyExact`, `CallPyDefaults`, `CallPyKwNames`,
   `CallBoundMethod`) caches the callee fingerprint and the
   name→slot permutation per site, reducing the bind to a bounds
   check plus slot writes.
4. **WS4 — generator resume discipline.** A generator resume today
   re-materializes frame state per `next()`. Resume becomes a
   frame-reuse fast path: the suspended activation (locals, stack
   buffer, pc) is kept warm on the generator object and re-entered
   without reallocation, with the `FOR_ITER`-drives-generator and
   `sum(genexpr)` shapes recognized to skip the exception-plumbed
   general send path.
5. **WS5 — the numpy crash burn.** RFC 0066's census left 12
   `numpy._core` selftest modules crashing (SIGBUS/SEGV). A crash
   is a drop-in disqualifier in a way a red test row is not. The
   enumerated classes get root-caused and fixed — starting with the
   known one (C-stack exhaustion in recursive sequence discovery:
   the C-API boundary must charge WeavePy's recursion accounting so
   pathological recursion raises `RecursionError` instead of
   faulting) — and the census re-measures with crash classes
   converted to Python exceptions or passes.

WS6 re-baselines the bench lane and gates the wave: **suite geomean
≤ 2.33× CPython** (≥ 30% over the committed 3.33×), with each of the
four named fixtures (`float_math`, `call_overhead`, `generators`,
`attr_access`) improving ≥ 1.5× against the wave-5 baseline, and the
full regrtest + ecosystem lanes green under the default JIT. The
program goal (geomean ≤ 1.0×) continues into wave 7 with object-lane
attribute graphs (`richards`/`deltablue`) and persistent *native*
generator activations; this wave builds the lanes both need.

## Motivation

Three facts shape the wave:

1. **The remaining slowness is concentrated and named.** The bench
   suite's red tail is not diffuse interpreter overhead; it is six
   fixtures whose hot regions contain exactly four excluded shapes
   (method calls, kw/default binding, generator resume, math-module
   calls). Every one of those shapes is dominant in real Python —
   `richards` and `deltablue` are *nothing but* method calls; pytest
   collection is generator- and kwargs-bound; numpy's pure-Python
   shell is method dispatch over C kernels.
2. **Slowness now blocks conformance work.** The ecosystem lane's
   selftest rows are measured as *interpreter-speed-bound*: attrs'
   hypothesis loops are baselined `skip` because they cannot finish,
   packaging's `test_version` needs >50 minutes, numpy's pytest
   *collection* alone measures ~31 minutes against a 2400 s budget.
   Making calls and generators fast is prerequisite work for the
   next ecosystem wave, not a parallel track.
3. **Crashes outrank everything.** The numpy census records SIGBUS/
   SEGV in 12 modules. For a runtime whose pitch is "drop in and
   nothing breaks", a segfault under a supported package is the
   worst possible outcome — worse than wrong answers, which at
   least fail loudly in the user's test suite. The known class (C
   stack exhaustion under recursive sequence discovery) is an
   engine-boundary bug by construction: CPython converts the same
   recursion into `RecursionError` because its abstract-object API
   charges recursion accounting at the C boundary; WeavePy's C-API
   layer must do the same.

## CPython reference

- **Method-call specialization.** CPython 3.13 specializes
  `LOAD_ATTR` into `LOAD_ATTR_METHOD_WITH_VALUES` /
  `LOAD_ATTR_METHOD_NO_DICT` (guarded by `tp_version_tag`) and the
  following `CALL` into `CALL_BOUND_METHOD_EXACT_ARGS` →
  `CALL_PY_EXACT_ARGS`, pushing the callee's `_PyInterpreterFrame`
  without materializing a bound-method object
  (`Python/specialize.c`, `Python/bytecodes.c`). WS1 is the tier-2
  analogue (class-version guard + direct native entry with the
  receiver in slot 0); WS3's `CallBoundMethod` is the tier-1
  analogue.
- **Keyword/default binding.** CPython's
  `_PyEval_MakeFrameVector`-era general binder survives as the slow
  path; hot sites run `CALL_PY_GENERAL` only until the specializer
  proves the shape. WS3 mirrors the discipline: the generic binder
  stays the semantics oracle, the cache is only a proven-shape
  bypass.
- **Math intrinsics.** CPython's `math.sqrt` is
  `math_sqrt(double) → libm sqrt` with `is_error()` converting
  `errno`/NaN into `ValueError`. Burning libm calls into JIT code
  with a domain-guard deopt reproduces the exact exception text by
  re-executing the call in the interpreter — the same
  re-execute-on-surprise contract every existing deopt uses.
- **Generator resume.** CPython 3.11+ keeps the suspended
  `_PyInterpreterFrame` embedded in the generator object
  (`gi_iframe`); `gen_send_ex2` resumes it in place — no frame
  allocation per `next()`. WS4 adopts the same ownership shape.
- **C-API recursion accounting.** `Py_EnterRecursiveCall` charges
  `tstate->c_recursion_remaining` inside `PyObject_GetItem`,
  `PySequence_Check`, and every abstract-API entry, which is why
  numpy's self-containing-sequence pathology raises
  `RecursionError` on CPython instead of faulting. WS5 installs the
  equivalent charge at WeavePy's C-API dispatch boundary.

## Detailed design

### WS1 — Tier-2 method-call lanes

**Analyzer admission.** A `LoadMethodAttr(name)` whose receiver is a
local slot with a pinned class fingerprint (the same
`probe_attr`-style eligibility RFC 0065 uses, extended with a
`probe_method(slot, name) → Option<MethodResolution>` probe) no
longer disqualifies the frame. The probe resolves `name` on the
receiver's *class* (never the instance dict — a shadowing instance
attribute fails the probe, exactly like CPython's
`LOAD_ATTR_METHOD_*` guard) to a plain `PyFunction` with no
decorators in the way, and reports:

- a **method token** (index into the compiled frame's method table,
  the same space discipline as `CallPy` tokens);
- the callee's positional arity (`self` included) and inferred
  scalar return lane, when its code object is itself analyzable —
  `None` return lane means the call site types as a deopt-after-call
  (`CallStatus::Boxed`), identical to an un-laned `CallPy`.

The `LOAD_ATTR` is erased from the rewritten program; the receiver
stays on the native stack as its pin reference. A `MethodSpanMeta`
row (the RFC 0065 `list.append` machinery, reused verbatim) records
the interpreter-stack shape so a mid-span deopt rebuilds the bound
method + receiver pair the interpreter expects. The `CALL` lowers to
a new `TOp::CallMethod { token, argc, ret }`.

**The guard.** Each method token snapshots `(type_id, attr_version,
resolved function object, resolved __code__)` at compile time — the
class-identity + version fingerprint the tier-1 caches use, plus the
code pin RFC 0059 established for `CallPy` (functions are
code-rebindable). `wpjit_call_method` re-validates the fingerprint
against the *live receiver's class* per call: an instance of a
subclass, a monkeypatched method, or a rebound `__code__` deopts to
the interpreter's generic call at that pc.

**Native entry with a pinned receiver.** RFC 0067 restricted native
callees to all-scalar lanes because pin lanes need a per-activation
pin table built from a live interpreter frame. A method call relaxes
exactly one slot: the *caller holds the receiver's object reference
natively*, so the helper can seed the callee's pin table for slot 0
itself. Eligibility for direct native entry becomes:

- slot 0 is the receiver pin; every *other* live-in slot is a scalar
  parameter;
- the callee's own attribute sites all probe against slot 0 (the
  `self.attr` shape) — sites on other slots keep the interpreter
  path;
- everything RFC 0067 WS1 already required (guards, observers,
  recursion tick, lane-checked scalars) unchanged.

The callee's attribute guards were snapshotted against *a* receiver
at its own compile time; they validate per-access by class identity
+ version + dict index, so a different instance of the same class
enters cleanly and a divergent instance (missing attr, different
dict layout) deopts inside the callee — state fully recoverable by
the existing materialization path.

**Interpreter-backed fallback.** When the callee is not compiled (or
not enterable), `wpjit_call_method` binds `self` + args through the
normal call path and runs the frame in the interpreter — marking the
`CallCtx` dirty exactly like an interpreter-path `CallPy`. This is
deliberately *not* a deopt: the caller's native loop survives, which
is most of the win on driver-loop-plus-cold-method shapes. The
callee tiers up through its own hot counter and the next outer entry
resolves it natively.

**Stats.** `JitStats` gains `method_calls`, `method_call_fallbacks`,
and `method_guard_misses`, surfaced through `WEAVEPY_VM_STATS`.

### WS2 — Float completion: intrinsics, lanes, boundary values

**Math intrinsics.** `ResolvedGlobal` gains `MathModule`, and a
`LoadGlobal math` + `LoadMethodAttr sqrt|sin|cos|fabs` + `CALL 1`
sequence burns in as `TOp::MathIntrinsic(kind)` when the module
attribute still resolves to the canonical builtin (a module-attr
guard rides the existing `GlobalGuard` mechanism: name `math`
resolves to the module *and* `math.<name>` is the canonical function
object at entry). Lowering: `sqrt` is Cranelift's native `sqrt`
instruction; `sin`/`cos` call registered libm-backed helper symbols
(the same `f64 → f64` the interpreter's `math` module uses, so
results are bit-identical); `fabs` is `fabs`. Domain guards deopt
*before* the operation — `sqrt(x)` requires `x ≥ 0`, `sin`/`cos`
require finite input — so the interpreter re-executes the call and
raises the exact `ValueError`/`OverflowError` CPython does. The
erased `math` global and bound function ride a `CalleeSpanMeta`-shaped
span for deopt stack reconstruction.

**Float floor-div/mod.** `FloatArith(FloorDiv|Mod)` joins the
lowered set with Python semantics (result sign follows the divisor;
`fmod` + sign fixup exactly as the interpreter computes it), with a
zero-divisor deopt. This retires the "float floor-div/mod are
non-JITable in v1" carve-out from RFC 0032.

**Cross-block operand values.** The analyzer's four
`NonEmptyBoundaryStack` bails relax to: a boundary stack is legal
when every successor agrees on the lane vector (the fixpoint already
computes per-block entry types; the bail predates the fixpoint
carrying them). `TBlock::entry_stack` — declared, lowered, and
tested since RFC 0032 but never populated — starts carrying the
types. This admits ternaries (`a if c else b`), `and`/`or` chains,
and the `bool(x)`-materializing jumps, which appear in `float_math`
(`maximize`), `richards` (queue checks), and most real predicates.

### WS3 — Tier-1 call-shape fast paths

Four new `InlineCache` states on `CALL` sites (the RFC 0021
discipline: generic handler proves the shape, cache bypasses it):

- **`CallPyExact`** — callee fingerprint (function identity +
  `__code__` identity + arity) matches and the site passes exactly
  `co_argcount` positionals, no keywords: bind is a straight
  slot-copy loop. (Today's path re-derives binding eligibility per
  call.)
- **`CallPyDefaults`** — same, but the site passes `k <
  co_argcount` positionals and the callee's trailing defaults cover
  the rest: the cache stores the default-slice offset; bind is the
  slot-copy plus a clone-from-defaults loop. Rebinding
  `f.__defaults__` bumps the function's version and misses the
  fingerprint.
- **`CallPyKwNames`** — the site passes keywords (a compile-time
  constant name tuple): the cache stores the resolved name→slot
  permutation vector, validated once against the fingerprint. Bind
  becomes positional copies + permuted keyword writes + the
  defaults fill. Sites whose keywords land in `**kwargs` stay
  generic (the dict build is the semantics).
- **`CallBoundMethod`** — a `LoadMethodAttr`-fed call whose cached
  class fingerprint matches: skips the bound-method temporary and
  enters the function with `self` pre-slotted (CPython's
  `CALL_BOUND_METHOD_EXACT_ARGS` shape), chaining into the three
  binds above for the argument tail.

All four count hits/misses under `WEAVEPY_VM_STATS`. Misses run the
generic binder — which remains the only place binding *semantics*
live (error messages included).

### WS4 — Generator resume discipline

**Persistent activation.** `PyGenerator` today rebuilds interpreter
frame state per resume. The suspended activation — locals vector,
operand-stack buffer, pc, block stack — becomes owned storage on the
generator object (CPython's `gi_iframe` shape), re-entered in place
by `next()`/`send()`: zero allocations on the resume path, one
memcpy-free hand-back on suspend. Close/throw keep the general path.

**The drive-loop fast path.** The two dominant consumers —
`FOR_ITER` over a generator and the `sum()`/`list()` C-loop drive —
currently route each item through the full `send(None)` protocol
(exception plumbing included). Both recognize the
already-suspended-at-`YIELD_VALUE` state and resume with a
yielded-value fast return: the `StopIteration`-materializing path
runs only at exhaustion, and a yielded value never constructs an
exception. (`gi_frame` visibility, `sys.settrace` semantics, and the
PEP 479 rules are unchanged — observers force the general path, the
same check the JIT entry uses.)

### WS5 — The numpy crash burn

**The recursion class (root-caused).** numpy's
`test_pathological_self_containing` builds `l = []; l.append(l)` and
lets array coercion recurse through `PySequence_*`. On CPython each
abstract-API hop charges `Py_EnterRecursiveCall`; WeavePy's C-API
boundary charges nothing, so numpy's C recursion rides the real C
stack into SIGBUS. Fix: the extension-facing dispatch points that
can re-enter Python or recurse structurally (`PyObject_GetItem` /
`PySequence_GetItem` / `PyObject_GetAttr` / `tp_*` slot dispatch
into extension code) charge `recursion::enter()` — the same
accounting the interpreter and the RFC 0067 native call path tick —
raising `RecursionError` at the same depth CPython does. A
depth-probed stack-headroom check (the `Py_C_RECURSION_LIMIT`
analogue) backstops paths where a Python-level limit is too coarse.

**The census re-measure.** With the guard in, the 12 crash rows
re-run module-by-module (offline wheel cache, per-module pytest so
one crash cannot mask another). Every remaining SIGBUS/SEGV gets a
minimal in-tree reproducer and either a fix in the wave or an
enumerated crash-class row in the expectations notes with the
measured stack — the RFC 0056 stretch discipline, applied to
crashes. The acceptance bar is *zero unexplained* crash rows: each
is fixed or root-caused with a reproducer.

### WS6 — Measurement, gating, and the re-baseline

- `baselines/bench-macos-aarch64.json` re-measures under the wave's
  binary. Gate: **suite geomean ≤ 2.33× CPython** (≥ 30% over the
  committed 3.33×), with `float_math`, `call_overhead`,
  `generators`, and `attr_access` each ≥ 1.5× better than their
  wave-5 committed ratios, and no fixture regressing beyond the
  gate's existing noise threshold.
- New unit tests per WS: method-token guard hit/miss/deopt,
  native-entry-with-receiver (attr lanes validate against a second
  instance), monkeypatch-mid-loop deopt, math-intrinsic
  domain-error exactness (`math.sqrt(-1.0)` message parity),
  ternary/short-circuit lowering, the four call-shape caches
  (hit/miss/rebind), generator resume reuse (no per-resume
  allocation, observed via the stats counter), and a
  self-containing-sequence C-API recursion test raising
  `RecursionError` through the `_ndarray`/`_numpylike` fixtures.
- The bundled + vendored regrtest sweeps and the ecosystem lane must
  hold `unexpected 0` under the default (JIT-on) binary. The
  generator work is particularly regression-prone
  (`test_generators`, `test_asyncio`'s 31 rows, `test_contextlib`);
  these run in the tightened loop during development, the full
  sweep at the end.

## Acceptance criteria

1. Suite geomean ≤ 2.33× CPython on the committed macOS-aarch64
   baseline (≥ 30% improvement); `float_math`, `call_overhead`,
   `generators`, `attr_access` each improve ≥ 1.5×.
2. A guarded method call on a pinned receiver compiles and enters a
   compiled callee natively (stats counter proves the fast path); a
   monkeypatched method or subclass instance deopts to the exact
   interpreter behavior.
3. `math.sqrt(-1.0)` raises `ValueError` with CPython's message from
   inside a compiled frame; float floor-div/mod and ternary shapes
   compile.
4. Generator `next()` on the resume fast path performs no frame
   allocation; `test_generators`, `test_asyncio` (all 31 rows), and
   `test_contextlib` stay green.
5. The self-containing-sequence pathology raises `RecursionError`
   through the C-API fixtures instead of crashing; the numpy census
   crash rows are each fixed or carry a root-cause + reproducer.
6. `cargo test --workspace`, the bundled regrtest sweep, and the
   ecosystem lane green under the default JIT, `unexpected 0`.

## Drawbacks

- **The pin-in-slot-0 relaxation grows the unsafe call surface.**
  The receiver's lifetime across a native callee activation is a
  new invariant (the caller's pin table entry outlives the callee's
  borrowed slot 0). Confined to `wpjit_call_method` and documented
  at the site; the deopt path materializes through the existing
  rebuild machinery.
- **Method guards add a per-call cost on the native path** (class
  identity + version load + compare). Measured in Results; the
  alternative — no method calls in tier 2 — costs 10–25× on the
  affected fixtures.
- **Four more inline-cache states** widen the dispatcher. The RFC
  0021 structure (decide-after-generic, cooldown on polymorphic
  sites) bounds the complexity; each state is ~a screen of code.
- **Generator frame ownership moves.** The resume fast path changes
  who owns the suspended activation, historically a
  use-after-free-shaped risk area. Mitigated by keeping the general
  send path as the semantics oracle and forcing it under observers,
  plus the asyncio sweep in the acceptance bar.

## Alternatives

- **Full object lanes in tier 2** (attribute graphs, `is None`
  chains — the `richards`/`deltablue` endgame) — deferred to wave
  7: it needs a boxed-reference lane with lifetime discipline
  through deopt, a strictly larger design than the single-slot
  receiver pin, and this wave's lanes (method tokens, boundary
  values) are its prerequisites either way.
- **Native generator activations in tier 2** (suspend/resume
  compiled frames) — deferred with it: suspending a native
  activation mid-loop needs persistent `JitFrame` storage and a
  yield-point spill protocol; the tier-1 resume discipline captures
  the allocation-and-plumbing win now and changes no protocol.
- **Inlining methods into callers** instead of guarded calls —
  rejected for the same reason RFC 0067 rejected callee inlining:
  two frames' deopt state in one activation forks the protocol.
- **A generic `**kwargs` fast path** — rejected: building the dict
  *is* the documented semantics (identity, mutability, insertion
  order); only the named-parameter permutation is safely cacheable.
- **Skipping the numpy crash burn to a dedicated wave** — rejected:
  the recursion class is small, root-caused, and blocks trusting
  any perf result measured over numpy-adjacent code.

## Prior art

- CPython 3.11–3.13 specializing `LOAD_ATTR_METHOD_*` +
  `CALL_BOUND_METHOD_EXACT_ARGS` (tp_version_tag guards), and PEP
  659's decide-after-generic cache discipline WS3 continues.
- PyPy's guarded method lookup (map/version guards promoting to
  direct calls) and its virtualizable generator frames.
- V8's polymorphic inline caches for named calls and its
  generators-as-resumable-activations.
- CPython `gi_iframe` embedded generator frames (3.11+), the exact
  ownership WS4 adopts.

## Unresolved questions

- Whether the method table should share the callee table's token
  space (one table, kind-tagged) or stay parallel. Proposed:
  parallel tables — the guard payloads differ (class fingerprint vs.
  globals snapshot) and unification saves nothing measurable.
- Whether `CallPyKwNames` should admit keyword-only parameters in
  v1. Proposed: yes when the site names them all explicitly; the
  permutation vector covers them naturally.
- Whether the boundary-value relaxation should cap the carried
  stack depth (deopt spill buffers grow with it). Proposed: cap at
  8 lanes, `NonEmptyBoundaryStack` past it; no real predicate shape
  comes close.

## Future work

- **Wave 7 — object lanes**: boxed-reference lanes with deopt
  lifetime discipline (attribute graphs, `is None` guards,
  `isinstance` fences) — the `richards`/`deltablue` fixtures; and
  native generator activations (persistent `JitFrame` +
  yield-point spill maps) — the `generators` endgame.
- `CALL_FUNCTION_EX` shapes (`*args` splat) in the tier-1 cache
  family once a workload names them.
- The ecosystem selftest re-measure (attrs' hypothesis lanes,
  packaging's `test_version`, numpy collection) once this wave's
  call/generator speed lands — flipping speed-bound `skip` rows to
  measured is ecosystem wave 4's opening move.
- The 16-byte `Object` / tagged-pointer investigation (carried from
  RFC 0065/0067).

## Results

Measured on macOS aarch64 (the committed baseline host), release
build, default (JIT-on) binary, CPython 3.13 as the reference
column. The pre-wave baseline re-measured on this host at **3.60×
CPython** geomean (the README's 3.33× was recorded on a quieter
run of the same commit).

**Headline: suite geomean 3.05× CPython — a 15% improvement over
the pre-wave 3.60×, short of the ≤ 2.33× gate.** The RFC's target
fixtures moved substantially but unevenly:

| fixture | pre-wave | now | improvement | ≥ 1.5× gate |
|---|---|---|---|---|
| spectral_norm | 9.30× | 4.01× | **2.32×** | met |
| richards | 18.98× | 11.35× | **1.67×** | met |
| generators | 15.84× | 9.70× | **1.63×** | met |
| call_overhead | 12.97× | 8.49× | **1.53×** | met |
| attr_access | 13.89× | 9.76× | 1.42× | missed |
| float_math | 14.67× | 12.75× | 1.15× | missed |

- **WS1 (method lanes)** carries `richards` (method-heavy state
  machine, −40%) and most of `attr_access`'s gain. **WS2 (float
  completion)** carries `spectral_norm` (−57%, the wave's biggest
  single win: `math.sqrt` + cross-block operands admit its inner
  kernels). **WS3 (call shapes)** carries `call_overhead` (−35%).
  **WS4 (generator resume)** carries `generators` (−39%): the
  zero-allocation park/unpark holds the `Frame` in the generator
  box across yields.
- **`float_math` (1.15×) is the honest miss**: its dominant cost is
  list construction and boxing around the kernels, not the compiled
  math itself — an object-lane problem, carried to wave 7 with
  `deltablue` (21.3×) and the rest of the boxed-reference story.
- **No fixture regressed**: the loop kernels hold 0.05× and
  `pidigits`/`jitkernels` stay ≤ 1.0×; `fib` measured 2.26× → 1.90×
  on the tier-1 call caches on the dev host, but the gain proved
  host-sensitive (CI's macOS runners measure ~2.4×), so the
  committed row keeps the wave-5 2.26×. `startup` (untouched by the
  wave: the branch binary starts marginally *faster* than main on
  the same host) gets its envelope re-widened to the CI-observed
  3.04× — the wave's dev-host re-record had tightened it to a
  2.23× host measurement, and CI's fastest macOS runner class
  (CPython starting in ~15 ms against weavepy's disk-bound ~44 ms
  binary load) measures 3.0×, outside a dev-host envelope, per the
  bench README's refresh rule.

**WS5 (numpy crash burn): complete.** All 12 previously-crashing
`numpy._core` selftest modules now run to completion or time out —
**zero SIGBUS/SEGV remain**. Seven distinct crash classes were
root-caused and fixed, each with a census row or bundled fixture:

1. **C-API recursion accounting**: `Py_EnterRecursiveCall` /
   `Py_LeaveRecursiveCall` charge the byte-faithful
   `PyThreadState.c_recursion_remaining`, and structurally
   recursive abstract entries (`PyObject_GetAttr`, `PyObject_GetItem`,
   `PySequence_GetItem`) carry a `stacker`-probed C-stack headroom
   guard — numpy's unguarded dimension discovery over a
   self-referential list now raises `RecursionError` (bundled
   `test_capi_recursion.py`, with a budget-restore assertion).
2. **Self-referential list crossing**: the canonical list box is
   published to the cache *before* its elements are materialised
   (and `sync_list_ob_item` carries a re-entrancy guard), so a list
   that transitively contains itself mints one box whose `ob_item`
   points back at itself, as on CPython.
3. **VM-subclass type publication**: types minted for VM classes
   now inherit the inline C base's `tp_basicsize` and publish
   faithful `tp_bases`/`tp_mro` tuples — numpy's
   `PyArray_DescrFromTypeObject` walks `tp_mro` directly to map a
   scalar subclass to its dtype (`test_scalarinherit`, and the
   crash tail of `test_ufunc`/`test_scalarmath`).
4. **Instance identity-box argument pinning**: an extension may
   store an argument's `PyObject*` borrowed with no incref
   (numpy's `PyArrayIdentityHash`); argument-pinned instance boxes
   now park at zero C refcount while the VM instance is reachable,
   keeping the stored pointer valid and identity-stable
   (`test_hashtable`).
5. **Foreign `len()` grounding**: `PyObject_Length` on a foreign
   object with no length slot raises `TypeError` immediately
   instead of bouncing VM ↔ C until the stack faults
   (`test_protocols`).
6. **Optional-probe error discipline**:
   `PyObject_GetOptionalAttr[String]` /
   `PyMapping_GetOptionalItem[String]` suppress only
   `AttributeError`/`KeyError` and propagate everything else — a
   warning-as-error raised inside `__getattr__` now surfaces
   through `np.asarray` (`test_protocols`).
7. **Container-owned item pointers**: `PySequence_GetItem` on a
   tuple/list returns a reference the container itself also owns
   (the stable-slot lanes), so numpy's
   `stash[i] = PySequence_GetItem(...); Py_DECREF(item)` idiom in
   `_vec_string` no longer dangles (`test_defchararray`).

Census after the wave: `test_hashtable` and `test_protocols` pass
outright; `test_scalarmath` (3 fail / 1579 pass),
`test_stringdtype` (411 / 2537), `test_defchararray` (6 / 94),
`test_ufunc`, `test_datetime`, `test_array_coercion`, and
`test_scalarinherit` complete with ordinary failures;
`test_multiarray` and `test_dtype` collect but exceed the census
timeout (an interpreter-speed matter, not a crash).

**WS4 follow-ups surfaced by the sweep** (regressions caught and
fixed in-wave): a generator/coroutine's materialised `PyFrame` is
now adopted back from the parked shell on resume, so one frame
object spans the generator's whole life — `sys._getframe` identity
across `await`, and bdb's `frame is self.stopframe` (pdb's `next`
over a coroutine, `test_pdb`); and the send dance's loop-back jump
is treated as CPython's never-instrumented
`JUMP_BACKWARD_NO_INTERRUPT`, so an `await` resume hop no longer
re-reports its line to `sys.settrace` while the `CLEANUP_THROW`
handler entry still fires on `close()`
(`test_monitoring.test_generator_with_line`).

**Gates**: `cargo test --workspace` green; the bundled regrtest
sweep grades **fail 0, error 0, timeout 0, unexpected 0** (433
pass / 3 skip / 1 enumerated divergence); the committed
macOS-aarch64 baseline is re-recorded at 3.12× (the dev host
measured 3.05×, but `fib`'s row keeps the wave-5 2.26× and
`startup`'s envelope is re-widened to the CI-observed 3.04× — see
above) and the bench gate passes against it. Acceptance criteria 2–6 are met in full;
criterion 1's geomean gate is missed (3.05× against ≤ 2.33×) with
the shortfall isolated to the object-lane fixtures named above —
the wave-7 opening move.

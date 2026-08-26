# RFC 0074: Performance wave 10 — frame coverage: object globals, the opaque-call lane, dict-view iteration, and the attribute-residue burn

- **Status**: Accepted
- **Authors**: WeavePy authors
- **Created**: 2026-08-25
- **Tracking issue**: TBD
- **Builds on**: RFC 0073 (dict/str lanes, comprehension shapes,
  persistent generator activations — and the measured-results section
  whose "frame-coverage work first" verdict is this wave's charter),
  RFC 0071 (the object call ABI and class-constructor callees), RFC
  0070 (the nullable object lane and pin discipline), RFC 0069
  (method lanes, call-shape ICs), RFC 0067 (default-on JIT), RFC
  0049/0068 (the measured regrtest baseline as the no-regression
  guard).

## Summary

RFC 0073 landed the dict lanes, the string lanes, the comprehension
shapes, and persistent generator parking — and then measured that
seven of its ten gated fixtures **did not move**, because their hot
`bench` frames never compile. The committed macOS-aarch64 baseline
sits at geomean **2.85× CPython**, and a fresh `WEAVEPY_JIT_TRACE`
census on the red fixtures (this wave's opening measurement, re-run
on the wave-9 interpreter) decomposes the entire remaining band into
frame-level rejections, not missing lanes:

- `UnsupportedOpcode("LOAD_GLOBAL")` — a global that is a class
  (`Strength` in deltablue's five `__init__`s and every solver
  method), a string constant, a `**kwargs`-taking Python function
  (call_overhead's `bench`), or any C builtin outside
  `range`/`len`/`math.*` disqualifies the **whole frame**.
- `UnsupportedOpcode("CALL (callee escapes)")` — a callee the
  analyzer cannot burn as a typed `PyFunc` token (a bound method on
  an object-lane value, a class-attribute function like
  `Strength.weaker`, any builtin) poisons the frame even where the
  call's *result* is only ever passed along.
- `UnsupportedOpcode("FOR_ITER (non-range shape)")` — `d.items()`
  loops (dict_ops' `bench`), `enumerate`/`zip`, and every iterator
  that is not a recognized range/list/dict-keys/`iter(x) is x` shape.
- `ProbeMiss("LOAD_ATTR shape")` / `UnsupportedOpcode("LOAD_ATTR
  receiver")` — attribute loads whose value wears no scalar lane and
  whose receiver arrives through an unsupported path (deltablue's
  `execute`/`recalculate`, the genexpr `.0` chains).
- `UnsupportedSignature` — `*args`/`**kwargs` *callees* are
  correctly non-compilable, but today their callers reject too.

The wave's thesis is the inverse of every wave before it: **stop
making coverage the callee's problem**. A frame should compile
whenever its *own* loop structure is representable, treating anything
it merely *touches* — an opaque global, an unknowable callee, an
unrecognized iterator — as an object-lane value handled through one
generic, guarded helper each. Five workstreams:

1. **WS1 — object globals.** `ResolvedGlobal` gains `ConstStr` (a
   str global burned as a pinned constant) and `ObjGlobal` (anything
   else — a class, a module, a C builtin, a `**kwargs`-taking Python
   function — burned as an identity-guarded object-lane pin). A
   `LOAD_GLOBAL` never again rejects a frame: the guard discipline is
   the existing `GlobalGuard` re-validation, and the pin enters
   through a memoizing `wpjit_global_obj` helper (the `PushConstStr`
   pattern). `LenBuiltin`/`RangeBuiltin`/`MathModule` keep their
   special lowerings; everything else is now *representable*.
2. **WS2 — the opaque-call lane.** A new `TOp::CallDyn { argc, ret }`
   pops an object-lane callee plus `argc` lane-tagged arguments and
   routes the call through the interpreter's own generic call
   machinery via a new `wpjit_call_dyn` helper — real frame, real
   binding (defaults, `*args`, `**kwargs`, C builtins, classes,
   bound methods), real raise propagation through `CallStatus::
   Raised` — then re-enters native code with the result as a fresh
   object-lane pin. The callee rides the *native* stack (no erased
   span metadata: a deopt spills it like any pin). Method-form calls
   on object-lane receivers get the same treatment through
   `TOp::CallDynMethod { name, argc, ret }` (a `LOAD_ATTR`-method +
   `CALL` fused site: one helper performs the bound lookup and the
   call, mirroring tier-1's `CALL` on a `LoadMethod` result). This
   single lane retires `CALL (callee escapes)` as a frame-killer and
   admits calls to `*args`/`**kwargs` functions, builtins
   (`isinstance`, `min`, `abs`, `list`, …), classes, and staticmethod
   shapes like `Strength.weaker(s1, s2)`.
3. **WS3 — dict-view iteration.** `for k, v in d.items()` (and
   `.keys()`/`.values()`) on a pinned exact dict compiles: the
   method-call-feeding-`GET_ITER` shape is recognized at plan time,
   captured through a widened `wpjit_dict_iter_new` (kind ∈ keys/
   values/items — the same checked view iterators the interpreter
   builds, carrying the live mutation guard), and stepped through
   the existing `TTerm::ForIter` protocol; the `FOR_ITER; UNPACK_
   SEQUENCE 2; STORE_FAST; STORE_FAST` items prologue is fused so the
   pair lands in two var slots without a materialized tuple.
   `enumerate(xs)` over a pinned list lowers to the ForList counter
   pattern (index and element are both already native); `zip(xs, ys)`
   over two pinned lists lowers to a fused dual-list header
   (stretch). Everything else iterable falls back to `IterCapture`
   — which WS2's object lane now feeds far more often, because the
   iterable can *arrive* through opaque calls.
4. **WS4 — the attribute-residue burn.** The census's two attr
   rejections close: (a) `LOAD_ATTR shape` — an attribute whose
   observed value is a pinned container/str/dict or a plain instance
   now grades to the corresponding lane (wave 9 stopped at scalars +
   `Obj`-instances on some paths); (b) `LOAD_ATTR receiver` — a
   method-form load on a receiver that itself arrived from an
   object-lane source (a `CallDyn` result, a `ForIter` element, a
   genexpr `.0` element) admits through the existing
   `attr_fingerprint_obj` runtime re-validation rather than
   requiring compile-time provenance. Both ride the established
   fingerprint + per-access re-validation discipline; no new guard
   kinds.
5. **WS5 — `%`-formatting and string-shape residuals.** `TOp::StrMod`
   lowers exact-`str` `%` (tuple or scalar rhs) through a helper
   that calls the interpreter's own printf-style formatter;
   `TOp::StrSlice` mirrors `ListSlice` for the `s[a:b]` shape
   (ASCII fast path, deopt otherwise). Both are census-measured in
   str_methods and json_bench.

Two disciplines hold the wave together. First, **`CallDyn` is a
compiled-code *pause*, not a deopt**: the helper enters the
interpreter for exactly one call, with the eval-breaker, tracing
gates, and prompt-reap discipline of a normal interpreter call, and
native execution resumes at the next instruction — the deopt budget
is never charged, so a frame that makes an opaque call per iteration
stays native forever. Second, **observability is unchanged**: the
interpreter call the helper makes is a real call (real frame on the
stack, `sys._getframe` exact, tracing fires if enabled — and a frame
compiled while tracing was off already deopts on activation, per the
RFC 0068 exactness rows).

**Gates** (against wave-9 committed rows, envelope rules per the
bench README): suite geomean improves on 2.85× with **≤ 2.2× as the
target**; `deltablue` ≥ 2.5× faster than its committed row,
`richards` ≥ 2.0×, `call_overhead` ≥ 1.8×, `list_ops` ≥ 1.6×,
`dict_ops` ≥ 1.5×, `pyaes` ≥ 1.5×, `str_methods` ≥ 1.4×, `nbody`
≥ 1.4×, `fannkuch` ≥ 1.3×, `json_bench` ≥ 1.3×; the loop kernels
hold ≤ 0.06×; no fixture regresses outside its committed envelope.
Post-wave, the two ecosystem residuals enumerated as
interpreter-speed-bound (numpy's `--pyargs numpy._core` collection
budget, attrs' hypothesis-dominated selftest) are **re-measured**
and their expectation rows rewritten from the fresh evidence.

## Motivation

1. **Wave 9 measured exactly this.** Its results section is
   unambiguous: "their hot `bench` frames reject wholesale on shapes
   this wave did not scope — unburnable `LOAD_GLOBAL`s (str globals,
   the `list` builtin, a `**kwargs` callee poisoning the whole
   caller frame), `d.items()`/`list(d)` iteration, slices, and
   `%`-formatting. The dict and str *lanes* are exercised and
   correct; the fixtures need frame-coverage work first." Wave 10
   with any other scope would strand three waves of landed lanes.
2. **Whole-frame rejection compounds every miss.** RFC 0070 named
   the lesson and wave 9's comprehension work re-learned it: one
   unsupported shape anywhere in a function disqualifies every loop
   in it. The census shows the fixtures' hot frames rejecting on
   *incidental* shapes — deltablue's constraint solver rejects on
   `LOAD_GLOBAL Strength` before its object-lane attribute traffic
   (which waves 7–9 made fast) is even reached.
3. **Opaque calls are how real Python is shaped.** The ecosystem
   corpus is not scalar kernels: it calls builtins, constructs
   classes, and passes through `*args` wrappers on every path.
   A JIT whose admission requires *transitively typed* callees
   caps out on benchmarks; one that can pause for an opaque call
   and keep the loop native is the architecture PyPy, V8, and
   every method-JIT converged on (the "call the runtime" slow
   path inside compiled code).
4. **The speed-bound ecosystem residuals are waiting.** numpy's
   selftest row cannot pass as one pytest run at the current
   interpreter speed (collection alone measured 2170.6s of the
   2400s budget), and attrs' hypothesis lanes pace at ~1 test/s.
   Both expectation rows name interpreter speed as the blocker;
   both re-measure after this wave.
5. **Program goal.** Conformance is at zero (RFC 0068), the
   ecosystem lane is 39/40 (RFC 0072), and 2.85× is the last
   first-order gap in the drop-in claim. This wave does not close
   it, but frame coverage is the highest-leverage remaining step:
   it converts the JIT from "fast on frames shaped like the JIT"
   to "fast on frames shaped like Python".

## CPython reference

- **`CALL` (3.13 generic path)**: CPython's un-specialized `CALL`
  routes anything callable through `_PyObject_Vectorcall` — the
  semantics `wpjit_call_dyn` must match are the interpreter's own
  `Call` opcode, which WeavePy already implements faithfully; the
  helper *is* that path, entered from native code. No new call
  semantics are introduced anywhere in this wave.
- **`CALL_BUILTIN_FAST` / `CALL_BUILTIN_CLASS` /
  `CALL_METHOD_DESCRIPTOR_*`** (3.13 specializations): evidence that
  CPython treats "callee is opaque native code" as a *specializable*
  case, not a disqualifier. `CallDyn` is deliberately one rung more
  generic: correctness-first, with per-callee-kind fast paths named
  as future work once the census shows which kinds dominate.
- **`FOR_ITER` over `dict_itemiterator`**: CPython iterates views
  through `dictiter_iternextitem` with the `di_used != ma_used`
  RuntimeError guard; the 3.13 compiler pairs it with
  `UNPACK_SEQUENCE 2` for the tuple-target form. WS3 keeps exactly
  that object (WeavePy's own checked `DictItems` iterator) and only
  fuses the unpack when the target is two plain `STORE_FAST`s.
- **`FORMAT_VALUE`/`BINARY_OP %` on str**: CPython's
  `PyUnicode_Format` is the printf-style formatter WS5's helper
  calls (WeavePy's `str.__mod__` implementation, already
  conformance-tested by `test_format`/`test_str`).
- **`LOAD_GLOBAL` specialization** (`LOAD_GLOBAL_MODULE` /
  `LOAD_GLOBAL_BUILTIN`): CPython burns a keys-version-guarded index
  and re-validates per access; WeavePy's existing `GlobalGuard`
  identity re-validation is the same contract, and WS1 extends the
  *representable value set*, not the guard discipline.
- Acceptance harnesses: the bundled `tests/regrtest/` sweep
  (`unexpected 0` required), the bench suite's committed baseline
  envelopes, and the ecosystem lane's offline `--wheels` run.

## Detailed design

### WS1 — object globals: `LOAD_GLOBAL` stops rejecting frames

**`ResolvedGlobal` additions** (`crates/weavepy-jit/src/ir.rs`):

- `ConstStr { idx: u32 }` — the global's value is an exact `str`;
  the embedder interns it in the compiled frame's constant-object
  table (same space the guard snapshot owns) and the load lowers to
  the existing `PushConstStr` machinery (memoized pin per
  activation, cap-pressure deopt). Guarded by identity like every
  burned global.
- `ObjGlobal { token: u32 }` — any other object. The embedder
  snapshots the resolved object into a per-frame **global-object
  table** (parallel to the callee table); the load lowers to a new
  `TOp::PushGlobalObj { token }` calling `wpjit_global_obj`, which
  pins the table entry (memoized per activation like `PushConstStr`,
  so a loop re-loading the same global holds one pin). The entry
  guard re-validates identity on every native entry and at
  eval-breaker strides, exactly as `PyFunc` tokens do today — a
  rebound global deopts within one stride (the RFC 0067 discipline).

**What resolves to what.** The embedder's resolver keeps its
existing special cases in priority order — `range`, `len`, canonical
`math`, typed `PyFunc` (including ctor recognition) — and then,
instead of `Opaque`, grades: exact `str` values → `ConstStr`;
`int`/`float`/`bool` → the existing const burns; **everything
else** → `ObjGlobal`. `Opaque` remains only for names that fail to
resolve at all (a truly missing global compiles to nothing — the
frame stays interpreted and raises `NameError` exactly).

**Object-lane uses.** A `PushGlobalObj` result is an ordinary
object-lane value: it can be stored to an `Obj` local, passed to
`CallDyn`/`CallPy` argument slots, receive `AttrGet`/`AttrSet`
through the existing fingerprint discipline (classes and modules
have attr-versioned dicts already — the same machinery instance
attributes use), be the receiver of `CallDynMethod`, feed
`IterCapture`, and be returned. The one *new* affordance the
analyzer gets: a `PushGlobalObj` of a **class** feeding a `CALL`
grades as a class-constructor callee when the class qualifies under
the RFC 0071 rules (so `Point(x, y)` keeps its fast construction
lane even though `Point` now also works everywhere else); a class
that does not qualify simply rides `CallDyn`.

### WS2 — the opaque-call lane

**The op.** `TOp::CallDyn { argc: u8, ret: JitType }` — stack
`[callee, arg0 … argN-1]` (callee below the arguments, interpreter
order). Arguments are staged through the marshal buffer with
per-slot `SlotTag`s (the mixed `BuildList` discipline), so scalar
lanes cross without pre-boxing pins. `ret` is `Obj` in v1 (a fresh
pin, `None` as `-1`); the analyzer types the result `Obj` and lets
downstream shapes (attr sites, `ForIter` capture, `is None` fences)
consume it through their existing runtime re-validation.

**The helper.** `wpjit_call_dyn(ctx, callee_pin, argc)`:

1. Boxes the staged arguments by tag (existing marshal decode).
2. Enters the interpreter's generic call chokepoint — the same
   function the `Call` opcode dispatches to — under the ordinary
   eval-breaker/tracing/recursion accounting. This is a *real* call:
   a Python callee gets a real frame (which may itself tier up and
   run native — the RFC 0067 native-to-native path is unaffected);
   a C builtin runs its native body; a class allocates and runs
   `__init__`; raises propagate as `CallStatus::Raised` with the
   exception parked exactly as `CallPy` parks it.
3. Registers the result as a fresh pin (prompt-reap discipline on
   displacement, `None` → `-1`) and returns to native code.

**No span metadata.** Unlike the erased `PyFunc` callees, the
`CallDyn` callee is a live native-stack value (a pin), so a deopt
during argument computation spills it like any operand — the
`CalleeSpanMeta` interp-depth rebuild machinery is not involved.
This is the simplification that makes the lane small: the analyzer
keeps the callee on the model stack, emission keeps it on the
native stack, and the existing spill format does the rest.

**Method form.** `TOp::CallDynMethod { name: u32, argc: u8, ret:
JitType }` fuses the method-form `LOAD_ATTR` + `CALL` pair when the
receiver is an object-lane value and no burned resolution applies
(the existing `CallMethod`/`CallStrMethod` lanes keep priority).
The helper performs the interpreter's own bound-method lookup +
call (`LoadMethod` semantics: descriptor protocol, `__getattr__`,
exact `AttributeError`). Between the fused pair no native
instruction executes, so no span is open; a deopt at the site
itself re-executes the `LOAD_ATTR` generically. `name` indexes a
new `TFunc::dyn_method_names` table (compile-time strings, no
guards — the lookup is per-call by construction).

**What stays out.** `CALL_FUNCTION_EX` (the `f(*t)`/`f(**d)` caller
shape) stays interpreted — same verdict as wave 9 WS5. Callee-side
`*args`/`**kwargs` binding is *not* reimplemented: it happens inside
the interpreter call the helper makes. Keyword-carrying `CALL_KW`
sites whose callee is opaque stay interpreted this wave (the
kwnames permutation requires a known signature; a `CallDynKw`
carrying the names tuple is enumerated future work).

**Return-lane refinement (in-scope stretch).** A `CallDyn` whose
callee is a burned `ObjGlobal` builtin with a known scalar result
(`len`-like: `ord`, `abs` on int lane) may refine `ret` to the
scalar lane with a runtime re-validation (lane surprise → deopt
after the call, result spilled). Admitted only where the census
shows it pays; `Obj` is always the sound default.

### WS3 — dict-view iteration

**Recognition.** At plan time, the shape `LOAD_FAST d; LOAD_ATTR
items (method form); CALL 0; GET_ITER; FOR_ITER` over a local whose
lane grades pinned-dict becomes a **dict-view loop**: the
`LOAD_ATTR`/`CALL` pair erases, `GET_ITER` lowers to the widened
`wpjit_dict_iter_new(kind)` capture (kind ∈ Keys/Values/Items —
materializing the interpreter's own checked view iterator, exactly
as wave 9's direct-dict form materializes `DictKeys`), and the loop
rides `TTerm::ForIter` unchanged. Deopt/raise semantics are
therefore word-for-word wave 9's: structural mutation raises
CPython's exact RuntimeError through the checked step; a mid-loop
deopt re-inserts the live iterator object.

**The items unpack.** When the `FOR_ITER` is immediately followed by
`UNPACK_SEQUENCE 2; STORE_FAST k; STORE_FAST v` (the dominant
`for k, v in d.items()` form), the step helper writes the key and
value into two var slots directly (trained lanes: the dict's key
lane and value lane) and the tuple is never materialized — CPython's
own `FOR_ITER`+`UNPACK_SEQUENCE` specialization pair does the same.
Any other consumer of the items tuple takes the generic path: the
pair materializes as a real 2-tuple riding the object lane.

**`enumerate` over a pinned list.** `for i, x in enumerate(xs)`
lowers to the `ForList` counter pattern with the existing index
synthetic slot doubling as `i` (the fused `UNPACK_SEQUENCE 2` writes
`i` from the index and `x` from the element). Admitted for the
zero-`start` form; `enumerate(xs, start)` adds the burned offset.

**`zip` (stretch).** `for a, b in zip(xs, ys)` over two pinned
lists lowers to a dual-header stepping both indices with one bound
check each. Measured whatever its color; falls to `CallDyn` +
`IterCapture` otherwise (zip objects satisfy `iter(x) is x`).

### WS4 — the attribute-residue burn

- **`LOAD_ATTR shape`** (the probe found a value it could not
  lane): attribute values grading `List*`/`Dict`/`Str`/`Bytes`
  pinned lanes are admitted at *all* attr sites (wave 9 admitted
  them on some receiver paths only); a value that is a plain
  instance grades `Obj` unconditionally (today some chain shapes
  reject instead). The helpers already re-validate value lanes per
  access, so this is admission-side only.
- **`LOAD_ATTR receiver`** (the receiver's provenance defeated the
  probe): a receiver that is a `CallDyn` result, an object-lane
  `ForIter` element, or a genexpr `.0` element admits attribute
  sites through the *runtime* fingerprint (`attr_fingerprint_obj`
  re-validation per access) with the compile-time lane taken from
  an exemplar probe when one exists (the RFC 0073 element-residue
  `ELEM_SENTINEL` discipline) and `Obj` otherwise.
- Attribute loads on **`ObjGlobal` classes and modules** (WS1) ride
  the same site machinery: class attr-version + dict-index
  fingerprints for `Strength.weaker`-shaped loads (whose result
  then feeds `CallDyn` — or better, when the resolved attribute is
  a plain function, the site grades a burned `PyFunc`-style token
  so `Strength.weaker(s1, s2)` compiles as a *typed* native call
  with the class's attr-version as its guard).

### WS5 — `%`-formatting and string residuals

- `TOp::StrMod` — `BINARY_OP %` with an exact-`str` lhs and a rhs
  that is a tuple-of-lanes literal, a scalar lane, or an object-lane
  pin. The helper calls the interpreter's `str.__mod__` body;
  the result must be exact `str` (a `WStr` result deopts, the
  `CallStrMethod` discipline); raises take the `Raised` exit.
- `TOp::StrSlice { start: bool, stop: bool }` — the `s[a:b]` shape
  mirroring `ListSlice` (erased `BUILD_SLICE` with `None` step;
  CPython clamping; ASCII O(1) byte slicing, non-ASCII receivers
  deopt at pin like `StrGetItem`).
- f-string `FORMAT_SIMPLE`/`FORMAT_WITH_SPEC` on lane-typed values
  (int/float/str with empty spec) lower to the existing
  `BuildString` inputs via a `wpjit_format_value` helper; non-empty
  specs stay interpreted.

### WS6 — re-baseline, re-measure, gates

Per the RFC 0049 protocol: the bench lane re-records
`crates/weavepy-bench/baselines/bench-macos-aarch64.json` under the
default JIT (envelope rules per the bench README — only genuinely
moved rows adopt new ratios); the full regrtest sweep re-runs at
`unexpected 0` under the default JIT with `test_dict`,
`test_ordered_dict`, `test_enumerate`, `test_str`, `test_format`,
`test_sys_settrace`, `test_monitoring`, `test_generators`,
`test_gc`, `test_weakref` explicitly re-verified; the ecosystem lane
re-runs offline and holds its baseline (39 pass / 1 enumerated
grpcio fail). The numpy selftest collection time and the attrs
selftest runtime are re-measured on the wave-10 interpreter and
their expectation rows rewritten from the fresh numbers (pass,
budget-fit, or refreshed reason — whatever the measurement says).

**Affected crates**: `weavepy-jit` (`analyze.rs`, `ir.rs`,
`lower.rs`, `runtime.rs`), `weavepy-vm` (`tier2.rs`). No bytecode,
compiler, object-model-layout, or C-API changes.

## Acceptance criteria

1. Suite geomean improves on the committed 2.85×; target ≤ 2.2×.
2. Against wave-9 committed rows: `deltablue` ≥ 2.5×, `richards`
   ≥ 2.0×, `call_overhead` ≥ 1.8×, `list_ops` ≥ 1.6×, `dict_ops`
   ≥ 1.5×, `pyaes` ≥ 1.5×, `str_methods` ≥ 1.4×, `nbody` ≥ 1.4×,
   `fannkuch` ≥ 1.3×, `json_bench` ≥ 1.3×.
3. Loop kernels hold ≤ 0.06×; no fixture regresses outside its
   committed envelope.
4. `cargo test --workspace` green, with new unit tests covering at
   minimum: object-global burning (class/module/builtin/str
   globals, rebind-observed-within-a-stride, missing-global
   NameError exactness), `CallDyn` (C builtins, classes,
   `*args`/`**kwargs` callees, bound methods, raising callees,
   `None` results, result-pin prompt reaping, recursion-limit
   exactness, a compiled caller whose `CallDyn` callee itself runs
   native), `CallDynMethod` (descriptor protocol, `__getattr__`,
   exact `AttributeError`, shadowing instance attributes),
   dict-view loops (items/keys/values, fused unpack, mutation →
   exact RuntimeError, mid-loop deopt rebuilding the live view
   iterator, empty dict, nested view loops), `enumerate` fusion,
   `StrMod` (%d/%s/%f/%r/%x, tuple and scalar rhs, raising formats,
   WStr deopt), `StrSlice` (clamping, negative bounds, non-ASCII
   deopt), and the attr-residue admissions (pinned-container
   attribute values, CallDyn-result receivers).
5. The bundled regrtest sweep grades fail 0 / error 0 / timeout 0 /
   unexpected 0 under the default JIT.
6. The ecosystem lane holds its baseline offline; the numpy and
   attrs selftest rows are re-measured and rewritten from evidence.
7. `cargo fmt` / `clippy -D warnings` green.

> **Measured results (as landed).** Suite geomean held at **2.86×**
> against the committed 2.85× — flat within the run-to-run envelope,
> so the committed baseline stands unrefreshed (no row genuinely
> moved). Criteria 1–2 are therefore **not met**: the gated fixtures'
> hot frames still reject wholesale, but now on shapes *behind* the
> ones this wave burned. A fresh `deltablue` census shows the new
> front line: `LOAD_DEREF` (closure cells), `CALL (callee escapes)`
> (callables stored into containers/attributes), `BUILD_LIST (shape)`
> (heterogeneous element lanes), `TO_BOOL lane` (truthiness on
> object-lane values), and residual `LOAD_ATTR shape` probe misses —
> the enumerated head of wave 11. The *lanes themselves* are
> exercised and correct: object globals, `CallDyn`, `DynAttrGet/Set`,
> `dict.items()` / `enumerate` pair loops, and `StrMod`/`StrSlice`
> all compile, run, and deopt-test green (criterion 4's core list;
> the wave's smoke fixtures compile natively and match CPython
> byte-for-byte). Criterion 3 is met (loop kernels 0.05–0.06×, no
> fixture outside its envelope), criterion 5 is met (438-row sweep,
> **unexpected 0**), criterion 6's baseline-hold is met (39 pass /
> 1 enumerated grpcio fail, offline), and criterion 7 is met.
> The wave's most valuable landing was unplanned: broader frame
> coverage made `_test_multiprocessing.get_value` hot enough to
> compile, exposing a **latent wave-6-era correctness bug** — a
> burned method site's guard miss returned `CallStatus::Reject`,
> whose deopt rebuild re-binds the open method span with a fresh
> attribute load that *fails* on a receiver that never matched the
> guard, fabricating a `None` callee (`TypeError: 'NoneType' object
> is not callable` where CPython raises `AttributeError`). The fix
> replaces the reject with a *surprise-receiver lane*:
> `wpjit_call_method` resolves the attribute generically through the
> interpreter — raising the exact `AttributeError` — and calls the
> bound result through the shared result protocol, so polymorphic
> receivers now continue natively instead of deopting. Regression
> test `jit_method_guard_miss_raises_exact_attribute_error` pins the
> shape; the multiprocessing spawn/forkserver packages (413 tests
> each) went red → green on it.

## Drawbacks

- **`CallDyn` re-enters the interpreter from native code on the hot
  path.** This is a designed cost: the census says the alternative
  is the whole frame staying interpreted. The lane's economics are
  monitored (a `WEAVEPY_JIT_TRACE` dyn-call counter) and per-kind
  fast paths (builtin fast-call, bound-method direct entry) are
  enumerated future work once the counters say which kinds dominate.
- **Reentrancy is now structural.** A `CallDyn` callee can mutate
  anything: rebind the globals the frame burned, mutate the dict a
  view loop iterates, invalidate attr fingerprints. Every existing
  guard already re-validates per access or per stride precisely
  because tier-1 faced the same reentrancy; the new tests pin the
  nasty cases (callee rebinds a burned global mid-loop, callee
  mutates the iterated dict → exact RuntimeError, callee
  invalidates a method fingerprint → deopt).
- **Pin-table pressure grows.** Object globals, dyn-call results,
  and view iterators all pin. The existing cap-pressure deopt keeps
  correctness; the memoized-pin discipline (`PushConstStr` pattern)
  keeps loops bounded. The bench suite's memory-adjacent fixtures
  (`json_bench`, `pyaes`) watch the aggregate.
- **The analyzer grows again** (~6.2K lines pre-wave). Mitigation
  unchanged: no new guard disciplines — identity guards, fingerprint
  re-validation, and deopt economy cover every new shape — and each
  WS lands rejection-path tests.

## Alternatives

- **Keep growing typed lanes instead (a `set` lane, a bytes-write
  lane, …)**: rejected — wave 9 measured that lanes without frame
  admission do not move fixtures. Coverage first, then lanes have
  somewhere to run.
- **Trace-based compilation (compile hot loops across frame
  boundaries, PyPy-style)**: rejected as a wave — it is a different
  architecture. The method-JIT + opaque-call design reaches the same
  frames at a fraction of the risk, and nothing in it precludes
  tracing later.
- **Inline caches inside `CallDyn` (per-site callee memoization)**:
  deferred, not rejected — v1 measures the generic helper first; the
  per-kind fast paths land against measured counters, not
  speculation (the RFC 0073 lesson applied to calls).
- **Burning `**kwargs` callees with a synthesized binding shim**:
  rejected — `CallEx`-faithful kwargs binding is exactly the code
  RFC 0073 declined to reimplement in a lane; the interpreter does
  it once, correctly, inside the helper.

## Prior art

- **CPython 3.13's `CALL` specialization family** — the tiered
  "generic call, then specialize by callee kind" structure WS2
  copies (with WeavePy's tier-2 as the beneficiary instead of the
  adaptive interpreter).
- **V8's `CallRuntime` / JSC's operation calls** — compiled code
  calling into the runtime for one operation and continuing natively
  is the standard method-JIT escape hatch; `CallDyn` is that shape
  under a GIL'd refcounted VM.
- **PyPy's residual calls** — the JIT emits `residual_call` for
  unknowable callees and keeps tracing the caller; the economics
  (caller stays compiled, callee is a black box) are identical.
- **RFC 0070–0073's pin discipline** — every new value in this wave
  rides the existing nullable object lane; the wave adds zero new
  value representations.

## Unresolved questions

- **Should `CallDyn` results feeding `ForIter` capture re-probe
  eagerly?** A `CallDyn` returning a generator admits `IterCapture`
  today (`iter(x) is x`); returning a list would want a lane probe.
  Proposed: v1 captures opaquely, and the list case is measured
  before adding a re-probe.
- **`CallDynMethod` on lane-typed receivers** (an `Int`-lane value
  calling `.bit_length()`): boxing the receiver per call is a
  pessimization risk. Proposed: out of scope; lane receivers keep
  their typed method lanes (`CallStrMethod` et al.) and everything
  else was already `Obj`.
- **The dyn-call counter's alarm threshold** — at what per-loop
  dyn-call density does compiled execution stop paying? The wave
  lands the counters and the answer comes from the re-baseline; a
  profitability gate (reject only when *every* loop instruction is
  a dyn call) is the documented fallback if a fixture regresses.
- **Windows/Linux committed bench baselines** (RFC 0067 carry-over):
  still open; this wave keeps the macOS baseline as the committed
  gate and does not block on the other platforms.

## Future work

- **Per-kind `CallDyn` fast paths** (builtin fast-call without the
  full interpreter prologue; bound-method direct entry; `CallDynKw`
  for keyword sites) against the landed counters.
- **WS6 allocation elision** (deferred from wave 9, still gated on
  this wave's coverage making its candidates compile) and **WS4
  Phase B generator guard epochs** — both now have their
  prerequisites and re-enter the plan next wave with fresh profiles.
- **`set` lanes and dict/list method lanes** over the wave-9
  receiver-agnostic native-method machinery.
- **The numpy selftest cluster burn** (ecosystem wave 5's opening
  move) on the faster interpreter this wave delivers.

# RFC 0073: Performance wave 9 — comprehension frames, dict and string lanes, persistent generator activations, and call-shape completion

- **Status**: Accepted
- **Authors**: WeavePy authors
- **Created**: 2026-08-25
- **Tracking issue**: TBD
- **Builds on**: RFC 0071 (the object call ABI, class-constructor
  callees, `ListObj`/`ForList`/`ForIter`, resume entries, and — most
  importantly — the measured-outcome section whose rejection analysis
  is this wave's worklist), RFC 0070 (the nullable object lane and pin
  discipline), RFC 0069 (method lanes, call-shape ICs, zero-allocation
  park/unpark), RFC 0065 WS5 (guard fingerprints), RFC 0067
  (default-on JIT), RFC 0049/0068 (the measured regrtest baseline as
  the no-regression guard).

## Summary

RFC 0071 landed the object lane's consumers and then did something
more valuable than hitting its gates: it *measured why it missed
them*. The committed macOS-aarch64 baseline sits at geomean
**2.957× CPython**, and the wave-8 `WEAVEPY_JIT_TRACE` rejection
analysis decomposes the red tail into five named shapes:

1. **Comprehension shapes** — the PEP-709 *inlined* list/dict/set
   comprehension loop pattern (`LOAD_FAST_AND_CLEAR` + `SWAP` +
   accumulator-on-stack `FOR_ITER`) that the analyzer grades
   `UnsupportedOpcode("FOR_ITER (non-range shape)")` today, the
   genexpr `.0` iterator-parameter frames, and `BUILD_LIST` literals
   past the 16-element cap — the dominant residue of `richards`
   11.7×, `list_ops` 13.1×, `deltablue` 22.3×, and `pyaes` 12.2×.
2. **Per-yield pack/unpack** — wave 8's resume entries removed the
   interpreted post-yield stretch but admitting yield-dense bodies
   still measured 40% *slower* than the interpreter; RFC 0071 promoted
   **persistent native generator activations** from Alternatives to
   "the required next step" for `generators` 10.1×.
3. **Dict lanes** — explicitly deferred twice; `dict_ops` 6.3× and
   `json_bench` 5.4× are the next band down and are pure dict traffic.
4. **String write lanes and `str` method calls** — deferred in wave
   8's WS6; `str_methods` 6.4× is entirely native-method dispatch on
   `str` receivers.
5. **The residual `LOAD_ATTR` receiver shapes** in constructor-heavy
   code (a fresh instance probed before its `__init__` stores exist) —
   `float_math`'s 12.4× measured blocker — plus the defaults/kwnames
   call shapes tier-1 already specializes (`CallPyDefaults`,
   `CallPyKwNames`) that tier-2 still rejects: `call_overhead`'s 8.9×.

This wave burns that list, in that order. Seven workstreams:

1. **WS1 — comprehension shapes and the collection-literal burn.**
   The analyzer learns the inlined-comprehension loop shape (PEP 709,
   which WeavePy's compiler emits for list/dict/set comprehensions):
   `LOAD_FAST_AND_CLEAR`'s save/clear of the target local, the `SWAP`
   restore epilogue, the accumulator living *on the stack* across the
   `FOR_ITER` loop, and `LIST_APPEND`/`SET_ADD`/`MAP_ADD` at stack
   depth. Genexprs (the one form still compiled as a separate `.0`
   code object) become admissible generator bodies: `.0` rides the
   object lane (or a pinned list lane when entry packing sees one)
   feeding `TTerm::ForIter`. `BUILD_LIST`'s 16-element shape cap is
   replaced by a loop-free lowering for arbitrary literal lengths,
   `BuildList` accepts mixed lanes (a per-element tag array), and
   `BUILD_TUPLE` joins for the common pack-and-return shape.
2. **WS2 — dict lanes.** `JitType::Dict` joins the pinned lanes,
   trained by tier-1's `SubscrDict`/`StoreSubscrDict`/`ForIterDict`
   ICs: `DictGet`/`DictSet` (exact-`str` and `Int` key lanes only —
   custom `__eq__`/`__hash__` never qualifies), `DictContains`,
   `DictLen`, `BuildMap` for literals, and `TTerm::ForDict` walking
   entry indices with the live `DictWatch`/PEP 509 discipline (any
   structural mutation deopts; the interpreter re-raises CPython's
   "changed size during iteration" wording). Helpers run the same
   `FxBuildHasher` probes the interpreter uses (`StrKey`
   allocation-free lookups); missing keys raise `KeyError` through
   `CallStatus::Raised`.
3. **WS3 — string write lanes and the native-method call lane.**
   `TOp::StrConcat` (the `BinOpAddStr` shape), `TOp::StrGetItem`
   (ASCII O(1), the `SubscrStrInt` shape), `TOp::StrHash` (feeding
   WS2's key probes), and — the load-bearing piece — a **native-method
   call lane**: `MethodResolution` gains a `Builtin` variant
   fingerprinted by `BuiltinFn` identity, so `s.split(...)`,
   `.join`, `.replace`, `.upper`/`.lower`/`.title`,
   `.startswith`/`.endswith`, `.count`, `.find` on a `Str`-lane
   receiver compile to a direct builtin invocation with lane-typed
   arguments and a pinned result. This is the tier-2 analogue of
   tier-1's `CallNativeMethod` IC.
4. **WS4 — persistent native generator activations.** The wave-7/8
   yield discipline (full writeback + `JitStatus::Yielded` +
   re-packing on resume) becomes the *cold* path. A generator whose
   body compiles keeps a **parked native activation** across yields:
   the `JitFrame` buffer set (locals, spill, pin table) lives in a
   heap box owned by the `PyGenerator`; a yield stores the yielded
   value and resume pc and returns without materializing the
   interpreter frame; the next `send` re-enters after an epoch-cheap
   guard check with **zero packing**. The interpreter `Frame` remains
   canonical-on-observation: `gi_frame`, `throw()`, `close()`,
   `f_locals`, active tracing, and deopt all force materialization
   (the existing writeback path) and drop the parked activation. The
   parked pin table registers with the cycle GC (`Traverse` over its
   `Object`s) and drains through prompt reaping when the generator
   finalizes.
5. **WS5 — call-shape completion.** Tier-2's callee protocol admits
   the two call shapes tier-1 already specializes: **defaults splice**
   (`CallPyDefaults` — missing trailing positionals filled from the
   live `__defaults__`, re-read at call time so rebinding is exact)
   and **kwnames permutation** (`CallPyKwNames` — the burned
   permutation maps keyword arguments to parameter slots, guarded by
   function identity). The `f(*local_tuple)` splat of a tuple built in
   the same frame lands as a stretch (statically-known arity unpacks
   to a positional call; everything else stays `CallEx`-interpreted).
6. **WS6 — allocation elision v1 (stretch).** Scalar replacement of
   non-escaping instances: a class-constructor callee (RFC 0071 WS2)
   whose result never escapes the compiling frame (no call argument,
   no store to the heap, no return, no identity use) is replaced by
   per-field lanes; deopt materializes the instance from a
   `VirtualInstance` span record (class + field lanes) exactly where
   the interpreter expects it. Measured whatever its color — if the
   escape analysis proves a rabbit hole, the enumerated fallback is
   to land the span format and the analysis behind a default-off env
   knob and carry the enablement to wave 10.
7. **WS7 — re-baseline and gates.** The committed baseline re-records
   under the default JIT; regrtest holds `unexpected 0`; the
   ecosystem lane holds its baseline offline.

**Gates** (against wave-8 committed rows, envelope rules per the bench
README): suite geomean improves on 2.957× with **≤ 2.5× as the
target**; `generators` ≥ 1.8× faster than its committed row,
`richards` ≥ 1.6×, `list_ops` ≥ 1.6×, `pyaes` ≥ 1.4×, `dict_ops`
≥ 1.5×, `str_methods` ≥ 1.5×, `deltablue` ≥ 1.3×, `float_math`
≥ 1.3×, `call_overhead` ≥ 1.25×, `json_bench` ≥ 1.2×; the loop
kernels hold ≤ 0.06×; no fixture regresses outside its committed
envelope.

## Motivation

1. **The worklist is measured, not conjectured.** Wave 8's
   measured-outcome section names every blocking shape with the
   fixture it blocks and the trace evidence behind it. This is the
   first perf wave since 0058 that starts from a rejection census
   rather than a design thesis; the risk profile is correspondingly
   lower.
2. **Comprehensions are everywhere.** The inlined loop shape is not
   a fixture quirk: every list/dict/set comprehension in real Python
   compiles into its enclosing frame (PEP 709), and today a single
   comprehension anywhere in a function disqualifies the *whole
   function* — whole-function granularity compounds the loss (RFC
   0070's lesson). Admitting the shape lifts admission rates across
   the ecosystem corpus, not just four bench rows; genexpr `.0`
   frames cover the remaining form.
3. **Dict traffic is the next measured band.** With calls,
   construction, and collections landed, `dict_ops` 6.3× and
   `json_bench` 5.4× decompose almost entirely into interpreted
   `BINARY_SUBSCR`/`STORE_SUBSCR`/`FOR_ITER` over dicts. Tier-1
   already proves the shapes are stable (the ICs hit); tier-2 just
   cannot express them.
4. **The generator verdict is in.** Wave 8 measured the resume-entry
   design against yield-dense bodies and it lost: per-resume packing
   costs more than the work between yields. The persistent activation
   is not speculative — it is the enumerated conclusion of two waves
   of measurement, and the ownership machinery it needs (pins,
   prompt-reap discipline, GC traversal) all exists.
5. **Program goal.** With conformance at zero and the ecosystem lane
   39/40, the geomean is the drop-in claim's last first-order gap.
   2.957 → ≤ 2.5 does not close it, but it burns the measured
   blockers standing between here and the ≤ 1.0× program goal, and it
   directly unblocks two standing ecosystem residuals (numpy's
   collection budget, attrs' hypothesis lanes) that are enumerated as
   interpreter-speed-bound.

## CPython reference

- **Comprehension inlining (PEP 709, CPython 3.12+)**: CPython
  inlines list/dict/set comprehensions into the enclosing frame;
  *genexprs remain separate code objects with the `.0` parameter*.
  WeavePy's compiler matches this faithfully (`comp_inline_eligible`
  in `weavepy-compiler`): inlined comprehensions emit
  `LOAD_FAST_AND_CLEAR` (save the target local and clear it,
  `Unbound` included), run the `FOR_ITER` loop with the accumulator
  *below* the iterator on the stack, and restore the saved locals
  with a `SWAP`+`STORE_FAST` epilogue; a per-loop exception-table
  entry restores them on a raise. WS1's admission problem is
  therefore *in-frame*: the analyzer must recognize this loop shape
  (today it grades `FOR_ITER (non-range shape)`), not admit a new
  frame kind. Only genexprs need the `.0`-parameter frame treatment.
- **`FOR_ITER_DICT` does not exist in CPython** — dict iteration
  specializes at `GET_ITER`/`FOR_ITER` over `dict_keyiterator` etc.;
  the safety contract is the version check in `dictiter_iternext`
  (`di_used != d->ma_used` → RuntimeError). WS2's `ForDict` keeps
  exactly that: entry-index stepping + live watch, mutation → deopt →
  the interpreter raises CPython's message.
- **`BINARY_SUBSCR_DICT` / `STORE_SUBSCR_DICT`** (3.13): guarded
  exact-dict subscripts; the missing-key path raises `KeyError`
  without deopting the specialization. WS2 mirrors both properties.
- **`CALL_METHOD_DESCRIPTOR_*`** (3.13's `CALL` specializations for
  `METH_O`/`METH_FASTCALL` method descriptors): the direct-invoke
  discipline WS3's native-method lane compiles, including the
  exact-type receiver guard.
- **`BINARY_OP_ADD_UNICODE`** (3.13): guarded exact-`str` `+`. WS3's
  `StrConcat` is the same guard; WeavePy does not attempt CPython's
  in-place-realloc refinement (`Rc<str>` is immutable-shared).
- **Generator resumption**: CPython resumes generators by continuing
  the *same* `_PyInterpreterFrame` (`gi_iframe`) — there is no
  pack/unpack at all. WS4 approaches that steady state from the other
  side: the parked native activation plays the role of the live
  frame, and the interpreter `Frame` materializes only when observed
  (`gi_frame`, `throw`, tracing), which CPython's own
  `frame_getframe` lazy `PyFrameObject` materialization legitimizes
  as a pattern.
- **`CALL_PY_WITH_DEFAULTS`** (3.13) and `CALL_KW`'s kwnames
  handling: WS5's two shapes, specialized identically (function
  version guard; defaults read from the live function object).

## Detailed design

### WS1 — comprehension shapes and the collection-literal burn

**The inlined shape** (list/dict/set comprehensions — PEP 709,
matched by WeavePy's compiler). The emitted pattern for
`[expr for x in iterable]` inside a function is:

```
LOAD_FAST_AND_CLEAR x     ; push old x (Unbound ok), clear slot
SWAP 2 (as needed)        ; saved value(s) buried under the loop
<iterable>; GET_ITER
BUILD_LIST 0              ; accumulator — *below* the iterator
  ... arranged so the stack across the loop is [saved…, acc, iter]
FOR_ITER → exit
  STORE_FAST x; <elt>; LIST_APPEND 2; JUMP_BACKWARD
exit: END_FOR; POP_TOP    ; skipped by the interpreter's skip_end_for
SWAP 2; STORE_FAST x      ; restore saved x (Unbound re-empties)
```

with an exception-table entry restoring the saved locals on a raise
inside the loop. The analyzer today rejects at `FOR_ITER` because
the loop's boundary stack is non-empty (the range/list rewrites
require an empty stack) and `LOAD_FAST_AND_CLEAR`, `LIST_APPEND`,
`SET_ADD`, `MAP_ADD`, and `END_FOR` have no `TOp` mappings.

Admission pieces, all in-frame:

- **`LoadFastAndClear`/restore discipline.** `LOAD_FAST_AND_CLEAR`
  types as: push the local's current lane value *or* a distinguished
  cleared token, and mark the slot `Unbound`-bearing. Because the
  saved value is written straight back by the paired epilogue
  `STORE_FAST` and never otherwise consumed, the analyzer models the
  save/restore pair as a *slot-state* effect rather than a data
  flow: the saved value parks in a synthetic spill slot (a
  `SlotTag`-tagged `JitFrame` entry so deopt can rebuild the exact
  interpreter stack mid-comprehension), and the epilogue restores
  it. After the restore, the target local re-types as `Unknown`/
  unbound — matching the interpreter, where a completed
  comprehension leaves `x` unbound.
- **Loop shapes with a non-empty boundary stack.** The
  `plan_rewrite` loop recognizers (`RangeLoopMeta`/`ListLoopMeta`/
  `IterLoopMeta`) drop the empty-stack requirement for comprehension
  loops: the accumulator (and any outer saved values) live in typed
  stack slots across the loop, exactly as the IR already models
  values live across blocks. The deopt span format records them so
  a mid-loop side exit rebuilds `[saved…, acc, iter-state]`
  faithfully.
- **Accumulator ops.** `LIST_APPEND depth` maps to a new
  `TOp::ListAppend` that appends to the list lane value at stack
  depth (keeping it on the stack — unlike the method-call `append`
  lane which consumes a receiver); `SET_ADD` and `MAP_ADD` get
  siblings (`MAP_ADD` rides WS2's dict machinery). The accumulator's
  collection lane refines from the appended element lanes
  (`ListInt`/`ListFloat`/`ListObj`), so a comprehension result feeds
  follow-on list lanes with no boxing cliff.
- **`END_FOR`/`POP_TOP`.** The interpreter's exhausted-`FOR_ITER`
  branch pops the iterator and skips both (`skip_end_for`); the
  analyzer marks them no-ops on the loop-exit edge, mirroring the
  statement-level loop rewrite.
- **The raise path.** The per-loop exception-table entry exists to
  restore saved locals when the body raises. Compiled bodies keep
  the existing discipline — a raise inside the loop deopts
  (`JitStatus::Raised` is for complete native raises; comprehension
  bodies side-exit instead), the deopt span rebuilds the interpreter
  stack including the saved values, and the *interpreter's* handler
  performs the restore. No native unwind machinery is added.
- **Scope for v1.** Single-generator comprehensions (one `FOR_ITER`)
  with any number of `if` filters; nested/multi-generator
  comprehensions admit only when the inner loop also matches
  (otherwise the function is rejected as today — measured fixtures
  are all single-generator). Async comprehensions stay rejected.

**Genexpr `.0` frames.** Genexprs remain separate code objects whose
only parameter is `.0`, currently rejected by the generator-body
gate before signature grading. With WS4's persistent activations,
genexpr bodies admit like any generator: `.0` seeds from
`Probes::param` (a pinned list lane or the object lane feeding
`TTerm::ForIter`), and a genexpr consumed by a compiled `ForIter`
loop is a compiled generator driven by a compiled consumer.

**`BUILD_LIST` past the cap.** The 16-element shape cap on
`BuildList` becomes a two-regime lowering: literal lengths ≤ 16 keep
the straight-line stores; longer literals lower to a helper call
(`wpjit_build_list_n`) that bulk-copies from the spill area — no
shape cap, one helper call, still no interpreter round trip.
`BuildList` accepts **mixed lanes**: the per-element tag array (the
existing `SlotTag` vocabulary) rides the spill buffer, and the helper
boxes each element by tag. `BUILD_TUPLE` gains the same two-regime
lowering (`wpjit_build_tuple_n`) — tuples are immutable, so no lane
tracking is needed beyond construction; the result rides the object
lane.

**The `LOAD_ATTR` receiver residue** (float_math's measured blocker):
a fresh instance flowing out of a class-constructor callee is probed
by `attr_fingerprint_obj` *before* its `__init__` stores exist, so
the site trains `NewKey`-shaped misses and rejects. The fix: sites
whose receiver is the result of a class-constructor callee resolve
their fingerprint against the class's **post-construction canonical
shape** (the same shared-keys construction order the RFC 0071 insert
shape records) instead of the pre-`__init__` snapshot. No new guard
kind — the site burns the same `(rc_id, attr_version)` fingerprint
and the same key index it would have learned one call later.

Implementation showed the constructor case is one of a *family* of
receiver residues, all sharing the discipline "the probe is a
prediction; the burned fingerprint re-validates per access":

- **Self-body residue** — inside a compiled `__init__` itself, a load
  that follows the same body's new-key stores resolves against the
  body's own store order (`AttrSiteMeta::self_ctor`); a body entered
  with a non-empty instance dict fails the runtime key-at-index check
  and deopts.
- **Element residue** — a receiver local with *no live value* whose
  values provably come from elements of a live list local (a loop
  target over that list, or a subscript result bound from it) probes
  an **exemplar element** instead: the site's probe path roots at the
  list slot through a reserved sentinel segment (`ELEM_SENTINEL`),
  which `walk_attr_path` resolves to the first instance in the live
  list's head. The guard snapshot walks the same path, so inference,
  emission, and snapshot agree.
- **Fluent method returns** — a burned-in method whose body the
  nested analysis cannot type (typically it reads attributes off a
  non-`self` parameter the nested view has no live value for) still
  gets an object-lane return prediction when every `RETURN_VALUE`
  syntactically returns local 0 (`returns_self_syntactically`, the
  `return self` builder idiom); object-lane results cross the call
  boundary as fresh pins, so the prediction is unconditionally sound.
- **Retriable probe misses** — an *environmental* analysis failure (a
  receiver local unbound in the triggering activation) reports the
  new `JitVerdict::ProbeMiss` instead of a structural rejection: the
  embedder charges only the failing entry pc
  (`CacheEntry::probe_misses`) and keeps the code object `Cold`, so a
  later entry point — where the local is live — still compiles.
- **Unbound locals at OSR entry** — each `OsrEntry` carries the slots
  that may be read before written from that entry
  (`unassigned_reads`, a per-entry definite-assignment complement
  computed over the compiled CFG). An unbound *object-lane* local not
  listed there is admissible: it enters as a pinned `Unbound` (so a
  deopt writes back exactly the unbound state) and is provably
  overwritten before any native read. This is what lets a
  once-called `bench(n)`-shaped frame OSR-enter at its *first* loop
  while later-bound locals are still unbound, instead of compiling
  only the last loop.
- **In-activation native-table refresh** — an activation that entered
  mid-warmup (before its callees'/methods' bodies compiled) no longer
  pays the interpreter fallback until it happens to re-enter: on a
  fallback, `CallCtx::refresh_tables` re-resolves the native
  callee/method tables when the compile generation moved (once per
  generation move) and retries the native path. On `float_math` this
  cut method-call fallbacks from ~62k to ~100 of 200k calls.

### WS2 — dict lanes

**The lane.** `JitType::Dict` joins the pinned lanes: entry packing
pins exact-`dict` values (subclasses stay `Unknown`), `probe_list_lane`
gains a dict sibling (`probe_dict_lane`) grading key/value lanes from
a bounded sample walk exactly as list probes do — admitted key lanes
are exact-`str` (including `WStr` rejection: surrogate-bearing keys
stay interpreted) and `Int`; value lanes are `Obj` or a scalar lane
when uniform. A dict whose sampled keys mix lanes still admits with
`Unknown` *key polymorphism* only for `ForDict` (iteration never
hashes); subscripts require a single trained key lane.

**Ops.**

- `TOp::DictGet { key_lane }` — `wpjit_dict_get` runs the
  interpreter's own allocation-free probe (`StrKey` for str keys,
  direct `DictKey(Object::Int)` for ints) against the pinned
  `DictData`. Hit → value in the trained lane (pin for `Obj`).
  Miss → `KeyError` through `CallStatus::Raised` (no deopt — missing
  keys are control flow in real code, exactly why
  `BINARY_SUBSCR_DICT` doesn't despecialize on them). A key whose
  runtime lane disagrees with training deopts.
- `TOp::DictSet { key_lane }` — insert-or-replace through the same
  chokepoint discipline as `builtins::dict_insert`: the helper bumps
  `DictWatch`/PEP 509/C-API watcher state exactly as the interpreter
  does (it calls the same functions), and the displaced value routes
  through the prompt-reap discipline `wpjit_attr_set` established
  (reapable displaced temporary → deopt-before-store).
- `TOp::DictContains { negate }` — the `in`/`not in` shape → `Bool`.
- `TOp::DictLen` — native length from the pinned `IndexMap`.
- `TOp::BuildMap { n }` — literal construction via `wpjit_build_map`
  (bulk insert from spill, tags per element pair). `MAP_ADD` for
  dictcomp accumulators rides the same helper as a single-pair
  variant.
- `TTerm::ForDict { dict_slot, index_tmp, kind, var_slots, body,
  exit }` — `kind ∈ {Keys, Values, Items}`. Entry-index stepping with
  a per-step live check of (a) the entry index against the current
  `IndexMap` length and (b) the `DictWatch` structural counter
  snapshotted at loop entry; either moving → deopt, and the
  interpreter's `PyIterator::DictKeys` machinery re-raises the exact
  CPython RuntimeError. The dict-view iterator object is never
  materialized natively; the deopt-span format records (dict, index,
  kind) and rebuilds the live iterator exactly as `ForList` rebuilds
  list iterators.

**What stays out.** Non-str/int key lanes, dict subclasses,
`d.get`/`d.setdefault`/`d.pop` method calls (WS3's native-method lane
admits only `str` receivers this wave — dict methods are named future
work), split-key `__dict__` interactions (instance dicts already have
their own `AttrGet`/`AttrSet` discipline and never enter the dict
lane), and `**` unpacking.

### WS3 — string write lanes and the native-method call lane

**Write ops.**

- `TOp::StrConcat` — guarded exact-`str` `+`: `wpjit_str_concat`
  allocates the joined `Rc<str>` and pins it. Chained concatenation
  compiles as repeated pins; the quadratic-append antipattern is no
  worse than tier-1 (same allocations, fewer dispatches).
- `TOp::StrGetItem` — the `SubscrStrInt` shape: O(1) byte indexing
  when the pinned str is ASCII (`str_char_len == byte_len`, burned at
  pin time as a lane bit), single-codepoint `Rc<str>` result, pinned.
  Non-ASCII receivers never enter the lane (deopt at pin).
- `TOp::StrHash` — `py_str_hash` through a helper, feeding WS2's
  key probes and guard-shaped uses; cached-hash fast path inline when
  the `Rc<str>` payload carries one.
- `TOp::BuildString { n }` — the f-string/`BuildString` shape over
  str-lane parts via `wpjit_build_string`.

**The native-method call lane.** `MethodResolution::Builtin {
fn_addr, arity_shape }`: when the method probe resolves a `Str`-lane
receiver's method to a `BuiltinFn` (via the same
`type_surface`-installed tables tier-1's `CallNativeMethod` hits),
the site burns the `BuiltinFn`'s function-pointer identity and the
trained argument lanes. `wpjit_call_native_method` re-validates
receiver lane + fn identity, marshals lane-typed arguments (str pins,
ints, bools), invokes the builtin directly (no `Object` argument
vector when the arity shape allows a stack-borrowed slice), and tags
the return: `Str` (pinned), `Int`, `Bool`, `ListObj` (pinned —
`split` returns a list of strings graded `ListObj`; a follow-on
`ForList` consumes it natively). A builtin that raises routes through
`CallStatus::Raised`. Keyword-taking invocations (`sep=`,
`maxsplit=`) admit only the positional spellings this wave.

`str.join` gets one special affordance: when its argument is a
`ListObj` pin, the helper takes the fast path `builtins::str_join`
already has for list arguments — no iterator protocol round trip.

> **Implementation note (as landed).** The lane burned a static
> `StrMethod` discriminant per site instead of extending
> `MethodResolution` with a `Builtin` variant: exact `str`'s method
> table is immutable (builtin types reject attribute stores), so no
> fn-identity fingerprint or per-call revalidation beyond the
> receiver pin is needed, and the site table
> (`TFunc::str_method_sites`) stays out of the embedder's guarded
> `MethodTable` entirely. `wpjit_str_method` resolves the builtin
> bodies once per process (memoized from the same `lookup_method`
> surface tier-1 hits) and dispatches with an `Object` argument
> vector — the stack-borrowed-slice refinement is future work.
> `str.join` rejects (deopts to the generic call) unless its argument
> is a materialized list/tuple, mirroring the dispatch-chain routing
> `builtin_needs_interp` enforces for tier-1.

**Receiver discipline.** Only exact-`str` receivers this wave. The
lane structure (fn-identity tokens + lane-typed marshaling) is
deliberately receiver-agnostic so dict/list methods can join later,
but each receiver type needs its own arity audit before admission —
enumerated future work, not scope creep.

### WS4 — persistent native generator activations

**Representation.** `PyGenerator` gains a `native: GilCell<Option<
Box<NativeActivation>>>` (None for interpreted generators, populated
on the first compiled yield). `NativeActivation` owns:

- the `JitFrame` buffer set: locals array, tag array, stack spill,
  pin table (`PinTable` moves out of `CallCtx` into the box for
  generator activations),
- the resume pc and the yielded-value staging slot,
- a **guard epoch**: the snapshot of every fingerprint the compiled
  body depends on (the same set `enter_compiled` validates today),
  plus the global invalidation counter tier-2 bumps on any guard-
  relevant mutation. Epoch match ⇒ re-entry validates in O(1); epoch
  mismatch ⇒ full re-validation (the current path), refresh or
  materialize.

**Yield (hot path).** `TTerm::Yield` in a persistent-eligible body
stores the yielded value + resume pc into the box and returns
`JitStatus::YieldedParked` — **no locals writeback, no stack spill,
no pin drain**. `generator_send` sees the parked activation, and on
the next resume: epoch check, seed the sent value, re-enter at the
resume entry with the boxed buffers — no packing. The wave-8 resume
entries are reused verbatim as the entry points; what changes is that
their entry packing is skipped when a parked activation exists.

**Materialization (the canonical escape).** The interpreter `Frame`
inside the generator box goes stale between yields; every observer
forces materialization first:

- `gi_frame` / `gi_suspended` / PEP 667 `f_locals` reads and writes,
- `generator_throw` / `gen_method_close` (exception injection is
  always interpreted, unchanged from wave 8),
- tracing/monitoring activation (the `observers_active` gate),
- deopt inside the body (the ordinary side exit does the full
  writeback and drops the box),
- generator finalization (`__del__`/GC): drain the pin table through
  prompt reaping, then ordinary teardown.

Materialization is exactly the writeback the wave-8 `Yielded` path
performs on every yield — it moves from per-yield to per-observation.
A materialized generator falls back to the wave-8 discipline for the
rest of its life (no re-parking this wave; re-parking is a named
future refinement if profiles want it).

**GC.** The parked pin table holds strong `Object` references across
suspensions, so `NativeActivation` implements `Traverse` and the
generator's existing trace hook visits it (a suspended interpreted
frame's stack is traced the same way today). Reference cycles through
a parked generator collect; the activation's pins drain through
`maybe_prompt_reap_replaced` at materialization/finalization, keeping
the RFC 0068 finalizer promptness on the natively-held references.

**Admission.** Sync generators only (coroutines/async generators
stay rejected — unchanged posture; the send/throw surface soaks one
more wave). The wave-7 profitability gate (yield-free native cycle)
**retires**: with zero per-yield packing the yield-dense shape wins
by construction, and the `generators` fixture is the re-measurement
criterion (gate ≥ 1.8×). `WEAVEPY_JIT_TRACE` gains a
parked/materialized counter pair so the fallback rate is observable.

**Implementation notes (as landed).** The shipped shape diverges from
the sketch above in ways worth recording:

- The box lives on the **`Frame`** (`Frame::parked_native`), not on
  `PyGenerator`: the frame already rides inside the generator's
  `GeneratorState` box, so parking on the frame lands the activation
  in the generator for free and keeps `generator_send` untouched.
- That box must be `Send + Sync` (the generator state box is), while
  the compiled artifacts are thread-local `!Send` `Rc`s. The parked
  `NativeActivation` therefore stores only raw buffers, the pin
  table, a cloned per-slot lane vector, and an **interp-free
  materialization plan** (`PlanSlot`s) computed at park time; native
  re-entry re-looks-up the compiled entry in the *resuming thread's*
  cache by a **process-unique `compile_id`** (`Artifacts::compile_id`
  — per-thread `compile_gen` counters can collide across threads). A
  cross-thread resume or a recompile misses the id and materializes.
- **No new `JitStatus`**: the wave-8 `Yielded` exit is reused and the
  park decision is made entirely VM-side (`park_plan`), so WS4 needed
  zero `weavepy-jit` changes. Park conditions: plain generator, the
  continuation is a registered resume entry (whose admission contract
  already fixes the spill to exactly the yielded value), no
  Python-visible `PyFrame` exists (one would read the shared locals
  storage while stale; `gen_py_frame` materializes before ever
  creating one), no observers, no open method spans (their rebuild
  needs an interpreter), and the residual stack is exactly the
  live-loop/erased-object inserts at contiguous depths.
- **Guard epochs are Phase B, deferred**: a parked resume re-runs
  `guards_hold` (plus the resume-entry and sent-lane checks) and
  skips only the marshal-in/decompose/writeback work. The O(1) epoch
  fast path remains named future work, to be taken up if the
  `generators` fixture profile says validation dominates.
- A parked activation that yields again **re-parks in place** (same
  box, same buffers, zero moves); only re-parking after a
  *materialization* is deferred as described above.
- **Admission gate updated**: RFC 0071 WS5's profitability rule
  (yield-dense bodies without a native cycle are `Trivial`) was
  measured against the pre-parking per-yield round trip and would
  have kept the parking machinery from ever firing on trailing-yield
  loops — the exact shape the `generators` fixture is made of
  (`_naturals`, `_squared`, the genexpr). The gate now admits any
  generator body with a resume entry; OSR-only bodies with no cycle
  keep the old rejection. On the fixture this compiles all four
  bodies and the full pipeline parks: 1.35M yields → 1.35M parks →
  1.35M parked resumes, 0 materializations, 0 deopts.
- Stats: `gen_parks` / `gen_parked_resumes` / `gen_materialized`
  (markdown report + `gen_park_stats_for_test`).

### WS5 — call-shape completion

**Defaults splice.** `NativeCallee`/`MethodEntry` gain a
`defaults_shape: Option<{ min_argc, param_count }>`. A call site with
`min_argc ≤ argc < param_count` compiles: `wpjit_call_py` reads the
live `__defaults__` tuple at call time (function identity is already
guarded; defaults *content* is deliberately not burned — rebinding
`f.__defaults__` is observed exactly as tier-1's `CallPyDefaults`
observes it, by reading through the function object), splices the
missing trailing arguments, and proceeds through the existing entry
packing. Sites whose spliced defaults have stable lanes train those
lanes; a default whose runtime lane disagrees deopts at entry packing
like any argument.

**Kwnames permutation.** Call sites carrying `kwnames` burn the
tier-1 `CallPyKwNames` permutation (≤ 8 kwargs, ≤ 16 slots — the IC's
own caps): the analyzer emits the permuted positional order directly
into the marshal buffers, so by the time `wpjit_call_py` runs there
is no keyword machinery at all — the permutation happened at compile
time, guarded by function identity (a rebound global/callee already
deopts through the existing fingerprint).

**Splat (stretch).** `f(*t)` where `t` is a `BUILD_TUPLE` result in
the same frame with statically-known arity lowers to the plain
positional call (the tuple never materializes). Any other `CallEx`
shape (dynamic tuples, `**kwargs`, str-subclass keys) stays
interpreted — `CallEx`'s faithful-kwargs machinery (raw-key
ride-alongs, `kw_from_dict`) is exactly the code one does not
reimplement in a lane.

> **Implementation note (WS5, as landed).**
>
> - **Defaults splice** landed in `try_native_call` (tier2-only, no
>   new `NativeCallee` field): the strict-arity check widened to the
>   `min_args..=arg_count` window the analyzer already admits, and the
>   unsupplied trailing slots are filled from the function's compiled
>   `defaults` tuple after lane-validating each one against the
>   callee's compiled parameter lanes (scalars pack; `Obj`/`Str`/
>   `Bytes`/`Dict` pin; a *list* default stays interpreted — its
>   identity is the mutable-default contract). A live
>   `f.__defaults__ = …` rebind lands in the slot store and refuses
>   the native path, so the interpreter's generic binder observes it
>   exactly like tier-1's `CallPyDefaults` hit guard.
> - **Kwnames** landed as a compile-time permutation, per the design:
>   a new `TOp::CallPyKw { token, argc, kwc, perm, ret }` burns
>   tier-1's 4-bit `CallPyKwNames` packing; lowering shuffles the
>   keyword values into their parameter slots while marshaling, so
>   `wpjit_call_py` is unchanged (it sees a plain positional prefix of
>   `argc + kwc` entries). The names tuple's `LOAD_CONST` is erased
>   from the trace at plan time; a new analyzer probe
>   (`Probes::kw_slot`, backed by the VM's callee table) resolves each
>   keyword name to its parameter slot, refusing unknown names,
>   positional-only slots, and constructor callees. One deliberate
>   narrowing vs. tier-1: the filled set must be exactly
>   `0..argc+kwc` (a keyword that *skips* a defaulted parameter is
>   not a contiguous prefix and stays interpreted) — this keeps the
>   helper keyword-free and composes with the defaults splice for the
>   uncovered tail. Deopt metadata is the existing `CalleeSpanMeta`
>   machinery unchanged: mid-argument exits rebuild the callee below
>   the loose values and the interpreter re-executes the real
>   `LOAD_CONST` + `CALL_KW`.
> - **Splat** is deferred (it was a stretch): the only compilable
>   shape (`BUILD_TUPLE` feeding `CALL_FUNCTION_EX` in the same
>   expression) does not occur in the fixtures or in idiomatic hot
>   code, so the lane would be dead weight. Dynamic `f(*t)` stays
>   `CallEx`-interpreted per the design.

### WS6 — allocation elision v1 (stretch)

Scope-fenced scalar replacement over the class-constructor lane:

- **Candidates**: a `CallPy`-with-ctor result whose uses are all (a)
  `AttrGet`/`AttrSet` through sites trained on the constructed class,
  (b) scalar-lane reads, (c) death (last use before any escape). Any
  call argument, return, store into a heap container, identity
  comparison, or `is None` fence against it disqualifies — escape
  analysis is a single forward pass over the already-typed `TFunc`.
- **Replacement**: the instance becomes per-field lanes (the
  `__init__` insert-shape store order gives the field list); the
  ctor call becomes the compiled `__init__` body's effects on those
  lanes when `__init__` is itself compiled and inline-eligible
  (single block of insert-shape stores — the `Point.__init__`
  shape), otherwise the candidate is rejected (v1 does not inline
  arbitrary constructors).
- **Deopt**: a `VirtualInstance` span record (class token + field
  lanes) joins the deopt-span format; materialization allocates
  through the cached `InstancePlan` and replays the canonical-order
  stores before the interpreter resumes. This is the only new deopt
  machinery, and it is the reason WS6 is a stretch: if the span
  complexity proves out of budget, the enumerated fallback is to land
  analysis + span format behind `WEAVEPY_JIT_ELIDE=1` (default off)
  and graduate it in wave 10 with the soak evidence.

> **Implementation note (WS6 — deferred to wave 10).** The wave's
> end-of-wave fixture traces made the call: the object-heavy frames
> elision would target (`deltablue`'s `__init__`s, `execute`s,
> `add_constraint`) do not *compile* yet — they reject on attribute
> probe misses, escaping callees, and unburnable `LOAD_GLOBAL`s
> before any allocation question arises. An elision lane with no
> compiling candidates is dead weight, so WS6 ships nothing and its
> budget went to the WS4 admission-gate completion (see the WS4
> note). Wave 10's frame-coverage work (opaque-callee calls, dict
> `.items()` iteration, str globals/slices) is the prerequisite that
> makes elision measurable.

### WS7 — re-baseline and gates

The bench lane re-records `crates/weavepy-bench/baselines/
bench-macos-aarch64.json` under the default JIT per the bench README
envelope rules (`gate --pct=25` stays the CI threshold; rows the wave
does not genuinely move keep their committed envelopes against
cross-machine skew). The regrtest sweep re-runs at `unexpected 0`
under the default JIT; the ecosystem lane re-runs offline and holds
its baseline (39 pass / 1 enumerated grpcio fail).

**Affected crates**: `weavepy-jit` (`analyze.rs`, `ir.rs`,
`lower.rs`, `runtime.rs`, `value.rs`), `weavepy-vm` (`tier2.rs`,
`object.rs` for the `PyGenerator` activation slot and `Traverse`,
`lib.rs`'s `generator_send`/`try_enter_resume` seam), `weavepy-bench`
(baseline). No bytecode, compiler, object-model-layout, or C-API
changes.

## Acceptance criteria

1. Suite geomean improves on the committed 2.957×; target ≤ 2.5×.
2. Against wave-8 committed rows: `generators` ≥ 1.8×, `richards`
   ≥ 1.6×, `list_ops` ≥ 1.6×, `pyaes` ≥ 1.4×, `dict_ops` ≥ 1.5×,
   `str_methods` ≥ 1.5×, `deltablue` ≥ 1.3×, `float_math` ≥ 1.3×,
   `call_overhead` ≥ 1.25×, `json_bench` ≥ 1.2×.
3. Loop kernels hold ≤ 0.06×; no fixture regresses outside its
   committed envelope.
4. `cargo test --workspace` green, with new unit tests covering at
   minimum: inlined-comprehension admission (list/set/dict forms,
   saved-local save/clear/restore including the `Unbound` case,
   filters, accumulator lane refinement, mid-loop deopt rebuilding
   the exact stack, comprehension raising mid-iteration restoring
   saved locals, nested-comprehension rejection or admission per
   scope), genexpr `.0` frames, `BuildList`/`BuildTuple` past 16 and mixed
   lanes, dict lane round trips (str and int keys, KeyError-without-
   deopt, lane-surprise deopt, displaced-value reap on `DictSet`,
   `ForDict` under insertion/deletion/resize → exact CPython
   RuntimeError, PEP 509 and watcher bumps observed, iteration-order
   exactness), string lanes (`StrConcat` including WStr rejection,
   `StrGetItem` ASCII-only discipline, `split`/`join` round trips,
   raising builtins, `str` subclass rejection), persistent
   activations (park/re-enter with zero packing asserted via stats,
   epoch-mismatch revalidation, materialize-on-`gi_frame`/`throw`/
   `close`/tracing, `__del__` promptness of pinned temporaries, cycle
   collection through a parked generator, exhaustion, `send` lane
   mismatch), defaults/kwnames call shapes (rebinding `__defaults__`
   observed, kwnames permutation exactness, TypeError shapes), and —
   if WS6 lands enabled — elision (virtual deopt materialization,
   identity-use disqualification, field-lane exactness).
5. The bundled regrtest sweep grades fail 0 / error 0 / timeout 0 /
   unexpected 0 under the default JIT, with `test_generators`,
   `test_asyncgen`, `test_sys_settrace`, `test_monitoring`,
   `test_pdb`, `test_gc`, `test_weakref`, `test_dict`,
   `test_ordered_dict`, `test_str`, `test_fstring` explicitly
   re-verified.
6. The ecosystem lane holds its baseline offline.
7. `cargo fmt` / `clippy -D warnings` green.

> **Measured results (as landed).** Suite geomean moved 2.957× →
> 2.83× (committed baseline refreshed per the envelope rules; only
> the genuinely-moved rows adopted new ratios). Per-fixture against
> the wave-8 committed rows: `float_math` **1.41×** (12.44 → 8.81 —
> WS1 comprehension shapes + the attr receiver residue),
> `generators` **1.25×** (10.06 → 8.04 — WS4 parking end to end:
> 1.35M yields / 1.35M parks / 1.35M parked resumes / 0
> materializations on the fixture), `attr_access` **1.22×** (4.58 →
> 3.75). `dict_ops`, `str_methods`, `list_ops`, `call_overhead`,
> `richards`, `pyaes`, and `deltablue` did **not** move: their hot
> `bench` frames reject wholesale on shapes this wave did not
> scope — unburnable `LOAD_GLOBAL`s (str globals, the `list`
> builtin, a `**kwargs` callee poisoning the whole caller frame),
> `d.items()` / `list(d)` iteration (WS2 covers direct dict-key
> iteration only), slices, and `%`-formatting. The dict and str
> *lanes* are exercised and correct (unit-tested, zero-deopt on
> lane-shaped code); the fixtures need frame-coverage work first.
> That coverage — an opaque-callee call lane, `.items()` iteration,
> str-constant globals, slice lanes — is the enumerated head of
> wave 10, together with WS6 (deferred, see its note) and WS4
> Phase B guard epochs. Criteria 1–2 are therefore *partially* met;
> criteria 3, 4, and 7 are met in full (workspace suite green with
> the enumerated new tests; loop kernels hold ≤ 0.06×; clippy
> clean).

- **A second suspended representation, at last.** Two waves deferred
  persistent activations precisely because the parked box is state
  the interpreter frame no longer mirrors. The mitigation is the
  materialize-on-observation discipline: every user-visible surface
  (`gi_frame`, `throw`, tracing, GC, finalizers) forces the canonical
  frame first, and the stats counters make the fallback rate
  measurable rather than assumed. The regrtest generator/tracing rows
  are the acceptance net.
- **Dict helpers must be bump-exact.** PEP 509 versions, `DictWatch`
  counters, and C-API dict watchers all observe native stores. The
  helpers call the same chokepoints the interpreter calls
  (`dict_insert`-equivalent paths), so divergence is a bug class the
  unit tests pin (watcher-observed native store is a named test), not
  a design gap.
- **The analyzer keeps growing** (~3.5K lines pre-wave). Same
  mitigation as wave 8: no new guard *discipline* — every lane reuses
  fingerprint + per-access re-validation + deopt economy, and each
  WS lands rejection-path tests, not just happy paths.
- **Compile-time and code-size growth**: dict/str lanes and
  per-yield resume entries widen the emitted IR; the existing compile
  budget and the `Trivial` grading bound it, and the bench suite's
  `startup` row watches the aggregate.

## Alternatives

- **Un-inline comprehensions in the WeavePy compiler** (compile them
  as `.0` frames so the existing callee machinery applies instead of
  teaching the analyzer the inlined shape): rejected — it regresses
  faithfulness (WeavePy deliberately matches CPython 3.13's PEP-709
  inlining: `sys._getframe` depth, tracing events, exception-table
  shapes are all observable) and trades an in-frame analysis problem
  for a cross-frame call-overhead problem PEP 709 exists to remove.
- **A general hash-table lane with burned bucket offsets** (burn the
  key's slot index like attr ICs burn key indices): rejected for v1 —
  dict key sets churn in real code far more than instance `__dict__`
  shapes; the measured fixtures are dominated by dispatch overhead,
  not probe cost, and the interpreter's own `FxBuildHasher` probe in
  a helper captures most of the win without a new invalidation
  protocol.
- **Boxing `str` methods through the existing `CallMethod` token
  protocol** (treat `BuiltinFn` as a callee): rejected — the token
  protocol assumes Python code objects (entry packing, deopt spans).
  A direct-invoke lane with fn-pointer identity is simpler and
  matches what CPython's method-descriptor specializations actually
  do.
- **Re-parking after materialization** (WS4): deferred — a
  materialized generator has been *observed*, and observed generators
  are disproportionately debugger/tracing subjects where re-parking
  buys noise. Named future work with the parked/materialized stats
  as the evidence base.
- **Full escape analysis with interprocedural reasoning** (WS6):
  rejected — v1's single-pass, single-frame, ctor-lane-only scope is
  deliberately the smallest thing that can elide `float_math`'s
  `Point`s; anything wider needs profile evidence first.

## Prior art

- CPython 3.13 specializations: `BINARY_SUBSCR_DICT`/
  `STORE_SUBSCR_DICT` (WS2), `BINARY_OP_ADD_UNICODE` (WS3),
  `CALL_METHOD_DESCRIPTOR_*` (WS3's native-method lane),
  `CALL_PY_WITH_DEFAULTS`/`CALL_KW` (WS5); PEP 709 as the
  comprehension-cost evidence base (WS1 takes the non-inlining road
  for conformance reasons — see Alternatives).
- **PyPy's virtualizables and virtuals**: parked native activations
  are virtualizable frames (the JIT owns the state; the object
  materializes on debugger access); WS6's `VirtualInstance` deopt
  records are PyPy/V8 "virtual objects" materialized at guard
  failure.
- **V8/JSC generator compilation**: suspended activations live in
  JIT-owned storage with on-demand frame reification — the shape WS4
  adopts under a GIL'd, refcounted object model.
- **Self/Smalltalk PICs**: the `BuiltinFn`-identity method lane is a
  monomorphic IC over native methods.

## Unresolved questions

- **`ForDict` over a dict that shrinks and regrows to the same
  length mid-loop**: the `DictWatch` structural counter catches it
  (any structural change bumps), but the *interpreter's* own
  detection is length-based in places; the acceptance test pins the
  CPython behavior (RuntimeError on the next `__next__`) and the
  helper follows whichever signal fires first.
- **Should `DictGet` admit polymorphic str+int key sites** (two
  trained lanes, two probes)? Proposed: no for v1; measure the
  deopt rate on `json_bench` (whose decoder dispatches on str keys
  only) before adding polymorphism.
- **Persistent-activation memory**: a parked box holds locals + pins
  + spill for the generator's life; a program holding thousands of
  suspended generators (async-ish fan-outs) grows resident set.
  Proposed: cap the parked-box population (LRU demotion to the
  materialized path) only if the ecosystem lane's memory ratios
  regress — measured, not preemptive.
- **WS5 defaults splice for bound methods** (`CallBoundMethodExact` +
  defaults): the IC exists separately in tier-1; proposed to admit it
  if `call_overhead`'s bound-method leg still dominates after the
  plain shapes land.

## Future work

- **Dict/list method lanes** over the WS3 receiver-agnostic
  native-method machinery (`d.get`, `l.append` as direct invokes).
- **Re-parking materialized generators**; **coroutine lanes** (the
  `await` shape) — the persistent activation is the substrate.
- **Genexpr body inlining as a JIT transform** once `.0` frames
  soak: inline a `Trivial`-graded genexpr body into a compiled
  consumer's `ForIter` loop, keeping frame observability via the
  deopt span.
- **WS6 graduation** (default-on elision) with soak evidence, then
  field-sensitive widening (elide through non-escaping *containers*).
- **Linux/Windows committed bench baselines** (RFC 0067 carry-over,
  still open).
- The numpy-selftest cluster burn and attrs re-measure (ecosystem
  wave 5's opening move) on the faster interpreter.

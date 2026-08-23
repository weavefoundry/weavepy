# RFC 0071: Performance wave 8 — the object call ABI, class construction, collection pipelines, and native generator resume

- **Status**: Accepted
- **Authors**: WeavePy authors
- **Created**: 2026-08-22
- **Tracking issue**: TBD
- **Builds on**: RFC 0070 (the nullable object lane, object-valued
  attribute access, `__slots__` lanes, and generator activations v1 —
  every WS here is a consumer it enumerated in Future work), RFC 0069
  (method-call lanes, call-shape caches, the callee-token protocol's
  method extension), RFC 0065 WS5 (the pin table and guard
  fingerprints), RFC 0059 WS3 (deopt-span reconstruction), RFC 0067
  (default-on JIT, native eval breaker), RFC 0049/0057/0060/0068 (the
  measured regrtest baseline as the no-regression guard).

## Summary

RFC 0070 generalized tier-2's object lane and named its own remainder
precisely: the lane exists, but almost nothing can *flow through* it.
Objects cannot cross a call boundary (`analyze` rejects any `CALL`
with a pinned argument or pinned return; `MethodRet` is scalar-or-
`None` only), classes cannot be called (`ResolvedGlobal::Opaque`
disqualifies the frame at `Point(i)`), attribute sites are keyed by
local slot so `self.my_output.determined_by` needs a hand-written
intermediate local, lists carry only unboxed scalars
(`JitType::ListInt`/`ListFloat`; `probe_list_lane` returns `None` on
the first object element), non-range `FOR_ITER` disqualifies
outright, and a generator resume pays an interpreted post-yield
stretch plus per-resume OSR guards. The committed macOS-aarch64
baseline sits at geomean **3.11× CPython**, and the red tail is
exactly these shapes: `deltablue` 22.4×, `list_ops` 12.9×,
`float_math` 12.8×, `pyaes` 12.4×, `richards` 11.4×, `nbody` 11.1×,
`generators` 9.7×.

This wave connects the lane to its consumers. Six workstreams:

1. **WS1 — the object call ABI.** Object arguments and object returns
   across `CallPy` and `CallMethod`. The marshal buffers
   (`call_args`/`call_tags`) gain `SlotTag::ObjPin` entries: the
   caller-side helper resolves its activation-local pin to the real
   `Object` and the callee's entry packing re-pins it into the
   callee's fresh `CallCtx` — pin indices never cross a boundary,
   objects do. Returns append to the *caller's* runtime pin table
   under the existing `RUNTIME_PIN_CAP` discipline (`-1` for `None`).
   `MethodRet` gains an `Obj` variant. `native_callable` /
   `native_method_callable` lift their scalar-params-only and
   slot-0-only restrictions: any parameter may ride the object lane.
2. **WS2 — the class-call construction lane.** A `LOAD_GLOBAL` that
   resolves to a `TypeObject` with a qualifying `InstancePlan`
   compiles to `TOp::CallClass`: the helper re-validates the class
   fingerprint, allocates the instance through the cached plan, and
   enters a compiled `__init__` through the WS1 ABI (receiver = the
   fresh instance) — the tier-2 analogue of CPython 3.13's
   `CALL_ALLOC_AND_ENTER_INIT`. Because `__init__` bodies are made of
   *new-key* attribute stores, `AttrSet` gains the insertion shape:
   the site records the class's shared-keys canonical construction
   order (tier-1's `StoreAttrNewKey` fingerprint), and the helper
   appends the key when the instance dict is in canonical shape at
   the recorded index, deopting otherwise.
3. **WS3 — pc-keyed attribute sites (chains).** `AttrSiteMeta.slot`
   was an analyzer provenance artifact — tier-1's inline caches are
   already per-instruction. Attribute sites re-key by bytecode pc;
   any object-lane stack value becomes a valid receiver, so
   `self.my_output.determined_by` compiles as two chained sites with
   no intermediate local.
4. **WS4 — object collection pipelines.** `JitType::ListObj` (pinned
   list, object elements: loads pin, stores route displaced values
   through prompt-reap, `-1` for `None` elements), `TTerm::ForList`
   (native `FOR_ITER` over a pinned list with per-step bounds
   re-check, mirroring CPython's `FOR_ITER_LIST`), and
   `TTerm::ForIter` with a per-step `wpjit_iter_next` helper over
   opaque iterators — exhausted temporaries drop through prompt
   reaping (the RFC 0068 `FOR_ITER` finalizer lesson is a named test).
5. **WS5 — native generator resume entry.** The instruction after
   each `YIELD_VALUE` registers as a **resume entry** — OSR packing
   at that pc with the sent value seeded on the packed stack.
   `generator_send` enters compiled code directly at the yield's
   continuation, retiring the interpreted post-yield stretch and the
   per-resume back-edge wait. The wave-7 profitability gate relaxes
   accordingly: yield-dense bodies (the `generators` fixture's tiny
   pipelines) are admitted because the per-resume cost drops to entry
   packing. `throw()`/`close()`/`gi_frame`/PEP 667 writes keep the
   interpreter path unchanged — the parked frame remains canonical.
6. **WS6 — string read lanes (v1).** `JitType::Str` and
   `JitType::Bytes` pinned lanes with burned-in helpers: `StrEq`
   (pointer-equality fast path, content-compare helper on miss),
   `StrLen`/`BytesLen`, and `BytesGetItem` (→ `Int`). Covers
   `attr_access`'s `p.c == s.c` residual and `pyaes`'s
   `key[i % klen]` indexing. String *method* calls (`split`/`join`)
   and dict lanes stay deferred (Future work).

**WS7 — re-baseline and gates** re-records the committed baseline
under the default JIT; regrtest stays at `unexpected 0` and the
ecosystem lane holds its baseline.

**Gates**: suite geomean improves on the committed 3.11×;
`float_math` ≥ 2.0× faster than its wave-7 committed row, `richards`
≥ 1.5×, `deltablue` ≥ 1.3×, `list_ops` ≥ 1.4×, `generators` ≥ 1.4×,
`pyaes` ≥ 1.2×, `attr_access` ≥ 1.1×; the loop kernels hold ≤ 0.06×;
no fixture regresses beyond its committed envelope.

## Motivation

Three facts, all measured:

1. **The lane exists but is starved.** Wave 7 shipped nullable object
   pins, object-valued attribute access, and `is None` fences —
   and `deltablue` improved only 1.2×, because the moment an object
   must *move* (into a call, out of a constructor, through a list,
   across a chain) the frame deopts or never compiles. The
   restriction map is explicit in the analyzer: `CALL (pinned
   argument)`, `CALL (pinned return)`, `FOR_ITER (non-range shape)`,
   `ResolvedGlobal::Opaque` for class globals. Every one of this
   wave's fixtures decomposes into those four rejections plus the
   resume tax.
2. **Whole-function granularity compounds each unlock, in both
   directions.** RFC 0070 observed that one missing shape
   disqualifies a whole method and thereby every native call *into*
   it. The converse holds for this wave: the object ABI (WS1) turns
   each newly-compiled method into a native-callable target for
   every already-compiled caller, and the class-call lane (WS2) is
   worth little without WS1 (a constructed `Point` that cannot be
   passed to `nxt.maximize(p)` deopts one instruction later). The
   workstreams are one connected surface, which is why they ship as
   one wave.
3. **The resume stretch is now the dominant generator cost.** Wave
   7's activations v1 made the yield exit free-ish (a writeback, no
   allocation) but left resume interpreted until the next back edge —
   and gated yield-dense bodies out entirely because that round trip
   loses. `generators` at 9.7× is three tiny bodies resumed 150 000
   times; the fixture is *all* boundary. A direct continuation entry
   is the designed endgame (RFC 0070 Future work names it verbatim)
   and the OSR machinery it needs already exists.

The cost of inaction is strategic, not just numeric: with conformance
at zero (RFC 0068) and the ecosystem lane probe-green, interpreter
speed is the last first-order gap between WeavePy and the drop-in
claim, and the program goal (geomean ≤ 1.0×) cannot be reached while
object-shaped code — most real Python — runs interpreted.

## CPython reference

- **`CALL_ALLOC_AND_ENTER_INIT`** (CPython 3.13,
  `Python/bytecodes.c`): the specialized class call that allocates
  the instance via `tp_alloc` and enters `__init__`'s frame inline,
  guarded by `tp_version_tag`, with a shim frame that discards
  `__init__`'s `None`. WS2 is the tier-2 analogue: same guard
  (WeavePy's `attr_version` + `rc_id` fingerprint), same
  allocate-then-enter shape, same `__init__`-must-return-`None`
  check.
- **Shared-keys insertion order** (`STORE_ATTR_WITH_HINT`,
  `_PyDictKeys` canonical construction): CPython specializes the
  new-key `__init__` store against the class's canonical key order.
  WS2's `AttrSet` insertion shape mirrors tier-1's `StoreAttrNewKey`
  fingerprint, which already models this.
- **`FOR_ITER_LIST` / `FOR_ITER_RANGE` / `FOR_ITER_GEN`** (3.13):
  the list specialization re-checks the index against the live
  length every step (mutation during iteration is defined behavior);
  WS4's `ForList` keeps exactly that per-step bound. `FOR_ITER_GEN`
  resumes the generator frame inline — the interpreter analogue of
  WS5's native resume entry.
- **Generator suspension** (3.11+ `gi_iframe`): the frame is left
  consistent at the yield boundary and resumed in place. WS5 changes
  *where WeavePy executes* the post-resume code, never the canonical
  suspended representation — `gi_frame`, `throw()`, `close()`, and
  PEP 667 `f_locals` writes observe the identical frame as wave 7.
- **`COMPARE_OP_STR`** (3.13): guarded exact-`str` comparison with
  the pointer-equality fast path WS6's `StrEq` burns in.
- `Py_ReprEnter`-adjacent finalization discipline: CPython frees a
  loop-exhausted temporary's iterator by refcount at exhaustion; RFC
  0068 fixed this for interpreted `FOR_ITER`, and WS4's `ForIter`
  helper must preserve it (named acceptance test).

## Detailed design

### WS1 — the object call ABI

**Argument marshaling.** Today `pack`/`lane_tag` handle
`Int`/`Float`/`Bool` and the analyzer rejects pinned args/returns.
The `JitFrame` marshal buffers (`call_args`/`call_tags`) gain
`SlotTag::ObjPin` entries carrying the caller's pin *index*; the
caller-side helpers (`wpjit_call_py`, `wpjit_call_method`) resolve
each index to the underlying `Object` (an `Arc` clone; `-1` resolves
to the `None` singleton) before entering `try_native_call`. The
callee's entry packing pins received objects into its own fresh
`CallCtx.pins` — **pin indices are activation-local and never cross
a boundary; objects do.** Identity is preserved end-to-end (the same
`Arc`), so `id()`, `is`, and mutation semantics are exact.

**Returns.** A callee returning an object writes `ret_tag =
SlotTag::ObjPin` after the caller-side helper appends the returned
`Object` to the *caller's* pin table (runtime append, bounded by
`RUNTIME_PIN_CAP`; cap hit is the ordinary deopt-and-refresh from RFC
0070). `None` returns ride the `-1` encoding with no pin behind them.
`MethodRet` gains `Obj`; `CallPy`'s `ret` lane may now be
`JitType::Obj`.

**Class knowledge does not cross the boundary.** The callee makes no
assumption about a received object's class — every attribute, method,
and `is None` use inside the callee re-validates through its own
helper (the established per-access guard discipline). Entry packing
therefore accepts *any* object into an `Obj` parameter lane. A callee
compiled with a `Float` parameter that receives an object was never
native-callable for that call shape in the first place — the
call-shape check in `try_native_call` already grades lane mismatches
as `CallStatus::Reject` (deopt at the call, interpreter re-executes).

**Admission.** `native_callable` drops its all-locals-scalar rule and
`native_method_callable` drops "no extra pin lanes beyond slot 0":
any parameter and any live-in local may be `Obj` (or, after WS4/WS6,
any pinned lane). The `is_pinned` rejections for `CALL` arguments and
returns in `analyze.rs` are deleted and replaced by lane-typed
call-shape entries in `CalleeTable`/`MethodEntry`.

**Deopt spans.** `CalleeSpanMeta`/`MethodSpanMeta` reconstruction
already rebuilds the interpreter stack mid-call; `ObjPin`-tagged span
slots rebuild the real objects from the pin table (the `SlotTag`
spill path from RFC 0070 handles this shape — the span format only
gains the tag).

**Reaping.** Objects cloned into a callee's pin table drain at callee
exit through `drain_runtime_pins`' prompt-reap discipline, exactly as
runtime attribute-load pins do today: a temporary whose last
reference dies at the boundary finalizes at the boundary, as tier-1
refcount drops would.

### WS2 — the class-call construction lane

**Resolution.** The analyzer's global resolution gains
`ResolvedGlobal::PyClass { token, argc_shape }`, produced when a
`LOAD_GLOBAL` resolves to a `TypeObject` whose cached `InstancePlan`
qualifies:

- `user_new` is `None` or `is_object_new` (no metaclass `__call__`
  interposition — the type's metaclass is `type` itself),
- `NativeKind::Plain` (no property/classmethod/value-base plans),
- a Python-level `__init__` (`init_fn` present) **or**
  `only_object_init`,
- not `abstract_error`, not `seeds_exception_args`.

The class object pins into the guard table like method-lane classes
do, fingerprinted by `rc_id` + `attr_version` (the subclass-bump
discipline from RFC 0065 covers MRO edits anywhere on the resolution
path).

**IR and helper.** `TOp::CallClass { token, argc }` pops `argc` lane-
typed arguments and pushes an `Obj` result. `wpjit_call_class`:

1. re-validates the class fingerprint (mismatch →
   `CallStatus::Reject`, deopt at the call);
2. allocates the instance through the cached `InstancePlan` (the
   same allocation `instantiate`/`type_call_default` perform, minus
   the generic `type.__call__` dispatch);
3. enters `__init__`: **natively** when `__init__`'s code is compiled
   and the call shape matches (the WS1 ABI with the fresh instance as
   receiver — the common case after warmup, since compiling the
   constructor's caller heats the constructor), otherwise through the
   ordinary interpreter call path *inside the helper* (correct, still
   no caller deopt);
4. checks `__init__` returned `None` (a non-`None` return raises
   `TypeError` through the ordinary raise path);
5. pins the instance into the caller's table and returns its index.

`__init__` raising propagates as `CallStatus::Raised` — identical to
the existing `CallPy` raise discipline (the interpreter frame
materializes mid-flight with the exact traceback).

**New-key attribute stores.** `AttrSiteMeta` (as re-keyed by WS3)
gains `insert: bool`. A store site trained against tier-1's
`StoreAttrNewKey` fingerprint records the class fingerprint plus the
expected key index in the canonical construction order.
`wpjit_attr_set` on an insert site verifies the instance dict is in
canonical shape with exactly `key_idx` keys (i.e. this store appends
at the recorded position) and appends key + value; any other dict
shape deopts and the interpreter re-executes the store. This is
deliberately narrow — it compiles the *constructor* shape (every
instance of a class built by the same `__init__` sees the same
insertion sequence) and nothing else. Dynamic post-construction
attribute creation stays interpreted.

**What stays out.** Classes with `__slots__`-only storage construct
through the same plan (their `__init__` stores are WS3 slots-lane
stores — no insert shape needed). Metaclass `__call__`, `__new__`
overrides, and `__init_subclass__`-style dynamism never qualify for
the lane and take the interpreter path with zero new risk.

### WS3 — pc-keyed attribute sites

`AttrSiteMeta.slot: u32` becomes `AttrSiteMeta.pc: u32`. Tier-1's
inline caches — the training source for every fingerprint
(`attempt_specialize_store_attr`, the load-attr probes) — are already
per-instruction, so the probe only stops *narrowing* what it can
train: any `LOAD_ATTR`/`STORE_ATTR` whose receiver is an object-lane
stack value gets a site, whether the receiver came from a local, a
prior `AttrGet`, a call return, a list element, or `FOR_ITER`. The
abstract interpreter stops requiring `obj_recv_slot` provenance
(`SE::known(lane)` without `src` is now a valid receiver), and the
runtime helper is unchanged — it always operated on the pin at TOS;
only the training key was slot-bound.

Chains fall out: `self.my_output.determined_by` is two sites, each
guarded by its own class fingerprint per access, each re-validated
every execution. No path table, no chain invalidation protocol — the
per-access re-validation discipline WeavePy has used since RFC 0065
makes chain correctness local to each link.

### WS4 — object collection pipelines

**`JitType::ListObj`.** `probe_list_lane` grades a non-empty list
whose elements are not homogeneous unboxed scalars as `ListObj`
instead of disqualifying. Element loads (`ListGet { elem: Obj }`)
pin the loaded element at runtime (`-1` for a `None` element);
element stores (`ListSet`) resolve the staged pin and route the
displaced element through the prompt-reap discipline exactly as
`AttrSet` does (reapable displaced temporary → deopt-before-store;
otherwise native drop). `ListAppend` follows. An element used where
analysis needs a scalar disqualifies at analysis (the lane carries no
unbox op this wave — mixed-scalar lists that *arithmetic* on
elements stay interpreted; mixed lists that only shuttle elements
compile).

**`TTerm::ForList { list_slot, index_tmp, var_slot, body, exit }`.**
The native `FOR_ITER` over a pinned list (any element lane): index in
a native temporary, per-step re-check against the live length via the
pinned list's header (mutation during iteration is defined — CPython
`FOR_ITER_LIST` re-checks every step and so does this), element
loaded through the lane's `ListGet` path into `var_slot`. The list
iterator *object* is never materialized in the native path; deopt
mid-loop rebuilds it at the interpreter boundary from (list, index),
which the `FOR_ITER` deopt-span format records.

**`TTerm::ForIter { iter_slot, var_slot, body, exit }` +
`wpjit_iter_next`.** For opaque iterables (generators consumed by
compiled callers, dict views, `enumerate`, zip): the iterator is
pinned at loop entry (produced by the interpreted `GET_ITER`, or by
the helper when the loop header is reached via OSR), and each step
calls `wpjit_iter_next`, which invokes the iterator protocol through
the interpreter core and returns an `ObjPin` (or scalar, when the
site trained scalar) or an exhaustion flag. **Exhaustion drops the
iterator through prompt reaping** — RFC 0068 established that CPython
frees a loop-consumed temporary's elements by refcount the instant
the loop ends, and the native path must preserve that (`__del__` on
a consumed temporary fires at exhaustion, not at the next cyclic
collection). A `StopIteration` with a value and any raise inside
`__next__` route through `CallStatus::Raised`. Per-step helper cost
makes `ForIter` slower than `ForList`; it exists to keep *frames*
compiled (one opaque loop no longer disqualifies the whole function)
and to pair with WS5 (a compiled consumer driving a compiled
generator crosses two native boundaries per element instead of two
full interpreter round trips).

### WS5 — native generator resume entry

**Resume entries.** For each `YIELD_VALUE` in an admitted generator
body, the analyzer registers the *following* instruction as a
`ResumeEntry { pc, block }` — machinery-wise an OSR entry whose entry
packing additionally seeds the sent value onto the packed operand
stack (typed by the abstract stack at that pc; the overwhelmingly
common `POP_TOP`-discards-`None` shape seeds `-1` on the object
lane). `TTerm::Yield` keeps its wave-7 semantics — writeback, park at
the yield pc, `JitStatus::Yielded`, interpreter executes the
suspension — but the yield's continuation block now exists in the
compiled CFG (reachable from the resume entry, not from the yield).

**`generator_send`.** On resume, before interpreting,
`generator_send` consults the generator's `CompiledFrame` for a
resume entry at `frame.pc + 1`-equivalent (the instruction after the
parked `YIELD_VALUE`). Hit → pack the parked frame (the same
`enter_compiled` packing OSR uses today), seed the sent value, enter
natively at the continuation block. Miss (post-yield stretch not
compiled, lane mismatch on the sent value, tracing active, guard
snapshot dirty) → interpret exactly as wave 7, with loop-header OSR
still available. `throw()` and `close()` never take the resume entry
— exception injection is always interpreted against the parked frame,
which remains the single canonical representation.

**Profitability gate.** Wave 7's cycle criterion existed because a
resume cost an interpreted stretch plus OSR guards; with direct
continuation entry the per-resume cost is one entry packing. The gate
relaxes to: admit a generator body when it has a yield-free native
cycle **or** at least one resume entry whose continuation reaches
real compiled work. The `generators` fixture — wave 7's measured
40%-slower-with-the-gate-off case — is the re-measurement criterion:
it must now *win* (gate: ≥ 1.4× vs its wave-7 row), and the gate's
final form is whatever the measurement supports. Coroutines and async
generators stay excluded (Future work; `await` is expression-yield,
and the send/throw surface should soak on sync generators for a wave
— unchanged posture from RFC 0070).

### WS6 — string read lanes (v1)

`JitType::Str` and `JitType::Bytes` join the pinned lanes (entry
packing pins exact-`str`/exact-`bytes` values; subclasses stay
`Unknown`). New ops, all guarded by exact-type at pin time and
re-validated by helpers where the value arrives at runtime (attribute
loads into a `Str`-lane site deopt on a non-str value, mirroring the
scalar-lane discipline):

- `TOp::StrEq { negate }` — pointer equality fast path (interned and
  identical strings answer inline), `wpjit_str_eq` content compare on
  pointer miss. Powers `attr_access`'s `p.c == s.c` residual and
  guard-shaped string compares generally.
- `TOp::StrLen` / `TOp::BytesLen` — native length from the pinned
  header.
- `TOp::BytesGetItem` — bounds-checked byte load → `Int` (negative
  index and out-of-range deopt). Powers `pyaes`'s `key[i % klen]`.

String *construction* (concat, slicing, `%` formatting), `str`
method calls (`split`/`join`/`replace` are native methods outside the
`CallMethod` token protocol), hashing, and dict lanes are explicitly
deferred — `str_methods` and `dict_ops` carry, and their gates are
set accordingly (no per-fixture gate this wave; envelope hold only).

### WS7 — re-baseline and gates

The bench lane re-records `crates/weavepy-bench/baselines/
bench-macos-aarch64.json` under the default JIT with the envelope-
refresh rules from the bench README (CI-observed envelopes kept where
the dev host measures inside them; `gate --pct=25` stays the CI
threshold). The regrtest sweep and the ecosystem lane run under the
default JIT and must hold their baselines (`unexpected 0`; ecosystem
36 pass / 1 enumerated gevent fail).

**Affected crates**: `weavepy-jit` (`analyze.rs`, `ir.rs`,
`lower.rs`, `runtime.rs`, `engine.rs`, `value.rs`), `weavepy-vm`
(`tier2.rs` for the helper surface and pin/marshal changes,
`specialize.rs` read-only as the training source, the generator
resume hook in `generator_send`), `weavepy-bench` (baseline). No
bytecode, object-model, or C-API changes — the wave is entirely
inside the tier-2 execution strategy, which is why the conformance
surface is a regression guard rather than a migration.

## Acceptance criteria

1. Suite geomean improves on the committed macOS-aarch64 baseline
   (wave 7's committed 3.11×).
2. Against their wave-7 committed rows: `float_math` ≥ 2.0× faster,
   `richards` ≥ 1.5×, `deltablue` ≥ 1.3×, `list_ops` ≥ 1.4×,
   `generators` ≥ 1.4×, `pyaes` ≥ 1.2×, `attr_access` ≥ 1.1×.
3. No fixture regresses outside its committed envelope; the loop
   kernels (`sumvm`/`nested_loops`/`jitloop`) hold ≤ 0.06×.
4. `cargo test --workspace` green, including new unit tests for:
   object args/returns round-trips (identity preservation, `None`
   both ways, cap-hit deopt, mid-call deopt-span rebuild with
   `ObjPin` slots, boundary prompt-reap of dying temporaries), the
   class-call lane (fingerprint reject, interpreted-`__init__`
   fallback, raising `__init__`, non-`None` return, the insert-shape
   store including its non-canonical-dict deopt), pc-keyed chain
   sites (per-link re-validation, mid-chain class mutation), `ListObj`
   load/store/displaced-reap, `ForList` under mutation-during-
   iteration, `ForIter` exhaustion finalization (`__del__` fires at
   loop end — the RFC 0068 shape, natively), resume-entry
   pack/enter round trips (`send` with values, `throw`/`close`
   bypassing the entry, `gi_frame` identity across native resumes),
   and `StrEq`/`BytesGetItem` edge cases (interning, non-ASCII,
   negative/OOB index deopt).
5. The bundled regrtest sweep grades **fail 0, error 0, timeout 0,
   unexpected 0** under the default JIT — generator, tracing, and
   finalization rows (`test_generators`, `test_sys_settrace`,
   `test_monitoring`, `test_pdb`, `test_asyncgen`, `test_gc`,
   `test_weakref`) explicitly re-verified.
6. The ecosystem lane holds its baseline offline.

## Measured outcome

Per the "measured, not aspirational" rule, this records the as-landed
state: all six workstreams shipped, the committed baseline moved from
geomean **3.107× → 2.928×**, every correctness gate is green, and
several of the red-tail per-fixture speed gates were **not met** — the
misses are enumerated below with their measured causes, and the
blocking shapes are named as deferred work rather than silently
dropped.

### What landed (and where it deviated from the design)

- **WS1 — object call ABI**: as designed. `SlotTag::ObjPin` args and
  returns across `CallPy`/`CallMethod`, `MethodRet::Obj`, identity
  preserved end-to-end, deopt spans rebuild `ObjPin` slots, and the
  compiled-callee cache feeds return lanes back into the analyzer.
- **WS2 — class construction**: landed as a *class-constructor callee*
  inside the existing `CallPy` protocol rather than a separate
  `TOp::CallClass` — the callee table records the class's burned-in
  call shape, `try_native_ctor` allocates the instance through the
  cached plan directly, and the insert-shape `AttrSet` store compiles
  `__init__`'s canonical-order appends. Same guards, one fewer IR op.
- **WS3 — attribute chains**: landed as *chain-path* sites
  (`AttrSiteMeta` keeps its root slot and gains the walked attribute
  path) rather than pure pc re-keying; chains like
  `self.my_output.determined_by` compile. Some `LOAD_ATTR` receiver
  shapes still reject (see the float_math miss below).
- **WS4 — collections**: `JitType::ListObj`, `TTerm::ForList`,
  `TTerm::ForIter` over opaque iterators (generators and builtin
  iterators ride the object lane through entry packing, OSR, resume,
  and deopt reconstruction), `BuildList`/`ListRepeat`/`ListSlice`, and
  — an enabling addition not in the draft — `TO_BOOL` on scalar lanes.
- **WS5 — native generator resume entry**: the machinery landed
  (resume entries after each `YIELD_VALUE`, `generator_send` enters
  compiled code at the continuation), **but the relaxed profitability
  gate was reverted after measurement**. Admitting yield-dense bodies
  made the `generators` fixture ~40% *slower* than the interpreter
  (91ms vs 71.5ms hand-measured): per-element entry packing + guard
  revalidation + writeback swamps the trivial work between yields.
  The wave-7 yield-free-native-cycle gate is restored; resume entries
  now benefit only bodies that also contain real compiled cycles.
- **WS6 — string read lanes**: as designed (`StrEq`, `StrLen`,
  `BytesLen`, `BytesGetItem`).

### Gate grading

Committed macOS-aarch64 rows, wave 7 → wave 8 (ratio to CPython,
lower is better):

| Fixture | Wave 7 | Wave 8 | Δ | Gate | Met |
|---|---|---|---|---|---|
| geomean | 3.107 | 2.928 | 1.06× | improve | ✓ |
| attr_access | 8.44 | 4.58 | 1.84× | ≥ 1.1× | ✓ |
| spectral_norm | 4.01 | 2.72 | 1.48× | — | ✓ |
| startup | 3.04 | 2.28 | 1.33× | — | ✓ |
| fib | 2.26 | 1.97 | 1.15× | — | ✓ |
| pidigits | 1.01 | 0.89 | 1.13× | — | ✓ (now beats CPython) |
| deltablue | 22.35 | 20.83 | 1.07× | ≥ 1.3× | ✗ |
| float_math | 12.75 | 12.44 | 1.02× | ≥ 2.0× | ✗ |
| pyaes | 12.37 | 12.15 | 1.02× | ≥ 1.2× | ✗ |
| list_ops | 12.86 | 13.13 | 0.98× | ≥ 1.4× | ✗ |
| richards | 11.35 | 11.67 | 0.97× | ≥ 1.5× | ✗ |
| generators | 9.70 | 10.06 | 0.96× | ≥ 1.4× | ✗ |
| loop kernels | ≤ 0.053 | ≤ 0.056 | — | ≤ 0.06 | ✓ |

Two caveats on the table. First, the attr_access/spectral_norm/startup
wins are larger than this wave alone plausibly explains — the wave-7
baseline had gone stale against fixes that landed between recordings,
so part of those deltas is accumulated drift now captured by the
refresh. Second, the sub-1.0 rows (richards, generators, list_ops,
call_overhead at 0.95×, jitkernels at 0.90×) were spot-checked by
hand: absolute times are stable across runs and the drift is
within run-to-run noise for these short fixtures.

**Why the misses missed**, from `WEAVEPY_JIT_TRACE` rejection
analysis, not conjecture:

- **generators**: the fixture is tiny yield-dense pipelines — exactly
  the shape the reverted WS5 gate excludes. Hand-measured, the fixture
  now runs at interpreter parity (~51ms both ways); the win the RFC
  hoped for requires **persistent native generator activations** (no
  pack/unpack per yield), which Alternatives already named as the next
  step if packing dominated. It does. Consume-side `ForIter` loops
  over generators *do* win (a compiled consumer driving `_naturals`
  measured faster than interpreted).
- **float_math**: the list shapes landed (`[None] * n`, slicing,
  object-lane loops all compile) and the JIT-vs-interpreter hand
  measurement shows ~20% (42.6ms vs 52.9ms) — but the fixture ratio
  barely moved because `bench`/`__init__` still reject on residual
  `LOAD_ATTR` receiver shapes (a fresh instance probed before its
  `__init__` stores exist). That is the narrowed WS3's known residue.
- **richards / list_ops / deltablue**: dominated by list/dict
  comprehension and genexpr frames (`LOAD_FAST .0` iterator-parameter
  shapes) and, for list_ops, `BUILD_LIST` literals beyond the
  16-element shape cap — none of which this wave's narrowed WS4
  admits.
- **pyaes**: `BytesGetItem` landed but the hot loops also need the
  comprehension shapes above.

### Correctness gates (all green)

- `cargo test --workspace` green, including the new unit tests:
  object arg/return round trips, class-constructor callees, chain
  sites, `ListObj`/`ForList`/`ForIter` (clean, raise propagation,
  lane-surprise deopt, OSR mid-loop), `BuildList`/`ListRepeat`/
  `ListSlice`, `[None] * n`, `TO_BOOL`, and the reverted-gate
  generator expectations (yield-dense bodies stay interpreted,
  results exact).
- Regrtest sweep: **437 rows — 433 pass / 0 fail / 0 error / 0
  timeout / 3 skip / 1 enumerated divergence — unexpected 0.** The
  named rows (`test_generators`, `test_sys_settrace`,
  `test_monitoring`, `test_pdb`, `test_asyncgen`, `test_gc`,
  `test_weakref`) all pass.
- Ecosystem lane (offline wheels): **36 pass / 1 enumerated gevent
  fail / 0 unexpected** — baseline held.

### Deferred, by name

1. **Persistent native generator activations** — promoted from
   Alternatives to the required next step for the `generators`
   fixture; the resume-entry-only design measurably loses on
   yield-dense bodies.
2. **Comprehension/genexpr frames** (`LOAD_FAST .0` shapes) — the
   richards/list_ops/deltablue residue.
3. **The remaining `LOAD_ATTR` receiver shapes** in constructor-heavy
   code (float_math's `bench`/`__init__`).
4. **`BUILD_LIST` literals past the 16-element cap** and mixed-lane
   `BuildList`.
5. String methods and dict lanes — unchanged from Future work.

## Drawbacks

- **The analyzer grows again.** `analyze.rs` is already 3.5K lines;
  this wave adds lanes, two loop terminators, resume entries, and a
  call-shape generalization. Mitigation: the guard *discipline* does
  not change — every new surface reuses the fingerprint + per-access
  re-validation + deopt economy, and each WS lands with unit tests
  against its rejection paths, not just its happy paths.
- **More native-held references.** Object args, returns, list
  elements, and iterators all pin; a hot loop can hold up to
  `RUNTIME_PIN_CAP` objects live per activation where tier-1 would
  have dropped them sooner. The cap bounds it and the drain reaps
  promptly at exit, but peak liveness inside one activation is
  measurably higher — acceptable, and identical in kind to wave 7's
  accepted trade.
- **The insert-shape store is a semantic sharp edge.** Appending a
  dict key natively must byte-match CPython's shared-keys
  construction order or `__dict__` iteration order diverges. The
  design keeps it narrow (canonical-shape-or-deopt) and the
  acceptance tests pin iteration order explicitly.
- **Resume entries multiply compiled entry points.** Each yield adds
  an entry with its own packing; code size and compile time grow for
  yield-heavy bodies. The profitability gate and the existing compile
  budget bound this.

## Alternatives

- **Passing pin indices across call boundaries** (a shared pin table
  per native call *stack*): rejected. Tables would need lifetimes
  spanning activations, cap accounting becomes global, and deopt of
  an inner activation could strand outer indices. Resolving to the
  real `Object` at the boundary costs one `Arc` clone and keeps every
  table activation-local — the wave-7 ownership story, unchanged.
- **A general `CALL` lane for arbitrary callables** (bound methods,
  builtins, closures as object-lane values): deferred. The class-call
  and existing token protocols cover the measured fixtures; a
  fully-dynamic callee lane needs a callee-identity guard per call
  site and is not required by any fixture in view.
- **Path-keyed chain sites with a chain-invalidation protocol** (the
  RFC 0070 Future-work phrasing): superseded by the simpler pc-keyed
  re-keying. Per-access re-validation already makes each link
  independently correct; a chain table would add an invalidation
  protocol for zero additional soundness.
- **Materializing list iterators in the native path** (pin the
  iterator, bump its index via helper): rejected for lists — the
  (list, index) pair is strictly cheaper and rebuilds the iterator
  exactly on deopt. Kept for opaque iterators (`ForIter`), where the
  iterator *is* the state.
- **Persistent native generator activations** (no pack/unpack per
  yield): still deferred, same reasoning as RFC 0070 — it requires
  pins outliving `CallCtx` and a second canonical suspended
  representation. The resume entry removes the interpreted stretch
  while keeping one representation; if packing still dominates
  `generators` profiles after this wave, the persistent activation
  is the named next step.
- **Inlining `__init__` bodies into the caller's compiled code**
  (true `CALL_ALLOC_AND_ENTER_INIT` inlining): deferred until the
  class-call lane soaks. Entering the compiled `__init__` through
  the WS1 ABI captures most of the win without cross-function IR.
- **Dict lanes this wave**: deferred with string *write* lanes. The
  8–22× cluster decomposes into calls, construction, collections,
  and resume; `dict_ops` (6.1×) and `json_bench` (5.2×) are the next
  band down and deserve their own measured wave.

## Prior art

- CPython 3.13's specialization family is the direct template:
  `CALL_ALLOC_AND_ENTER_INIT` (WS2), `STORE_ATTR_WITH_HINT` +
  shared-keys append (WS2's insert shape), `FOR_ITER_LIST`/
  `FOR_ITER_GEN` (WS4/WS5), `COMPARE_OP_STR` (WS6).
- PyPy's **list strategies** (int/float/object element storage chosen
  per list, promoted on demand) are the closest analogue to the
  `ListInt`/`ListFloat`/`ListObj` lane split, and its virtualizables
  inform the (list, index) iterator virtualization in `ForList`.
- V8's elements kinds (`PACKED_SMI_ELEMENTS` → `PACKED_ELEMENTS`
  transitions) mirror the same per-collection lane economy; V8/JSC
  resumable activations are the endgame WS5 approaches while keeping
  the interpreter frame canonical.
- Self/Smalltalk polymorphic inline caches: the class-call lane is a
  monomorphic constructor IC, as RFC 0070 anticipated.

## Unresolved questions

- **Should `CallClass` admit classes with inherited (non-own)
  `__init__`?** Proposed: yes — RFC 0070 already answered this for
  the deferred lane: the instance plan resolves through the MRO and
  the `attr_version` subclass-bump discipline guards every class on
  the resolution path. To be confirmed by the WS2 tests.
- **The `ForIter` scalar shape**: when an opaque iterator yields
  unboxed scalars (a compiled generator of ints driving a compiled
  consumer), should `wpjit_iter_next` return the scalar lane directly
  instead of an `ObjPin`? Proposed: yes when the site trains scalar —
  the helper already tags its return; measurement decides whether the
  dual-lane site is worth the polymorphism.
- **Resume-entry admission under lane drift**: a generator resumed
  with a sent value of a different lane than the training shape
  (rare: typed `send()` protocols). Proposed: lane-mismatch at the
  entry is a pack miss → interpret this resume, no invalidation;
  revisit only if a real workload alternates lanes.
- **Whether the relaxed generator gate should keep any floor at
  all** (e.g. minimum body size). Proposed: keep `Trivial` grading
  for bodies whose continuation reaches no compiled work, decide the
  rest by the `generators` fixture and the regrtest generator rows.

## Future work

- **Dict lanes and string write lanes**: dict subscript/`in`/iteration
  helpers keyed by tier-1's dict ICs (`dict_ops`, `json_bench`), str
  concat/slice/hash and a native-method call lane for `str` builtins
  (`str_methods`' remaining tail).
- **Allocation elision** (scalar replacement of non-escaping
  instances): the class-call lane makes escape analysis meaningful —
  a `Point(i)` consumed only by compiled attribute reads need never
  allocate. `float_math`'s endgame beyond this wave.
- **Persistent native generator activations** and **coroutine
  lanes** (the `await` shape) once the resume entry has soaked a
  wave.
- **`CALL_FUNCTION_EX` / `*args` / kwargs-dict call shapes** — the
  `call_overhead` residual.
- **The 16-byte `Object` thinning** stays available to any wave that
  measures a first-order need (unchanged posture since RFC 0070).
- **Linux/Windows committed bench baselines** graduating `gate` from
  advisory to strict off-macOS (RFC 0067 carry-over).
- The ecosystem selftest re-measure (numpy collection, attrs'
  hypothesis lanes) — ecosystem wave 4's opening move, further
  unblocked by this wave's call/collection/resume speed.

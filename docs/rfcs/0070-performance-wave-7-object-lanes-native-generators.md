# RFC 0070: Performance wave 7 — object lanes, native generator activations, and the slots/attr-graph completion

- **Status**: Accepted
- **Authors**: WeavePy authors
- **Created**: 2026-08-22
- **Tracking issue**: TBD
- **Builds on**: RFC 0069 (method-call lanes, call-shape caches, the
  generator park/unpark discipline, and the boundary-value relaxation —
  every WS here extends a lane it built), RFC 0065 WS5 (the pin table,
  the guarded scalar attribute lanes, and the pinned-list helpers this
  wave generalizes), RFC 0059 WS3 (the callee-token protocol and
  deopt-span reconstruction), RFC 0067 (the default-on JIT and the
  native eval breaker), RFC 0021/0058 (tier-1 adaptive specialization
  and the bench lane), RFC 0049/0057/0060/0068 (the measured regrtest
  baseline as the no-regression guard).

## Summary

RFC 0069 moved the suite geomean from 3.60× to 3.05× (committed 3.16×
after CI envelope re-widening) and named its own shortfall precisely:
the object-lane fixtures. The bimodal split persists — kernels inside
the tier-2 subset run at 0.05× CPython while the fixtures whose hot
regions are *object graphs* cluster at 8–27×: `deltablue` 27.3×,
`list_ops` 12.9×, `float_math` 12.75×, `richards` 11.4×,
`attr_access` 9.8×, `generators` 9.7×. Wave 6's Future work section
queued this wave verbatim: **object lanes** (boxed-reference lanes
with deopt lifetime discipline — attribute graphs, `is None` guards,
class fences) and **native generator activations**.

One investigation closes this wave rather than opening it: the
long-carried **16-byte `Object` / tagged-pointer** item (RFC
0061/0065/0067/0069). The ground truth is that scalars are *already*
unboxed — `Object::Int(i64)` / `Float(f64)` / `Bool` / `None` are
inline enum payloads, `1.5 + 2.5` allocates nothing, and RFC 0065
measured the 24→16-byte shrink (thinning the four fat-slice variants)
as third-order against Arc traffic on heap values. `float_math`'s
14.7× was never float boxing; it is `Point(i)` construction, method
calls carrying object arguments, and object-element list traffic —
all *lane* problems. The representation investigation is therefore
**retired in favor of lane coverage**, with the mechanical 16-byte
thinning left available to any future wave that measures a need.

The wave has four workstreams:

1. **WS1 — object lanes in tier 2.** The single wave-6 `Obj` lane
   (the pinned method receiver in slot 0) generalizes: any number of
   object-typed locals and stack values ride the lane, and the lane
   is **nullable** — the machine value `-1` stands for the `None`
   singleton — which admits the `is None` / `is not None` fences
   `deltablue` and `richards` are built from (`IsNone` lowers to one
   integer compare). On top of the lane: **object-valued attribute
   loads and stores** (guarded by the same class-identity +
   `attr_version` fingerprints the scalar sites use), with loaded
   objects pinned at runtime and displaced store values routed
   through the interpreter's prompt-reap discipline. Object
   arguments/returns across calls, the class-call construction lane,
   object-element list subscripts, and `FOR_ITER` object pipelines
   are enumerated in Future work — each is a further consumer of the
   same lane, deferred on measurement, not on mechanism.
2. **WS2 — native generator activations (v1).** The analyzer's
   blanket generator rejection lifts for the profitable shape: a
   yield becomes a **deopt-shaped side exit** (locals written back,
   the operand stack spilled with the yielded value on top, the
   frame parked *at* the `YIELD_VALUE` pc, a new
   `JitStatus::Yielded` returned) and the interpreter executes the
   suspension itself; a resume interprets the short post-yield
   stretch and re-enters native code at the next loop back edge
   through the existing OSR hook. A **profitability gate** admits
   only bodies whose CFG contains a yield-free native cycle — an
   inner loop that does real work between suspensions — because the
   per-resume OSR round trip loses to the interpreter on
   yield-per-iteration bodies. Because the parked state is always a
   valid interpreter frame, `close()` / `throw()` / `gi_frame`
   introspection / debugger attach need no new protocol — they
   simply find the frame the yield exit wrote.
3. **WS3 — `__slots__` attribute lanes.** The attr-site probe and
   helpers gain a second storage kind: the `PyInstance.slots` side
   table. Same guard discipline (class fingerprint + `attr_version` +
   index + name check), different storage root — `attr_access`'s
   `Slotted` half stops disqualifying frames.
4. **WS4 — re-baseline and gates.** The committed macOS-aarch64
   baseline re-records under the default JIT; regrtest and ecosystem
   stay green.

**Gates**: suite geomean improves against the committed 3.16×;
`deltablue` improves **≥ 1.2×** and `attr_access` ≥ 1.1× against
their wave-6 rows; `generators` and the loop kernels hold their
envelopes (the generator v1 is gated to never engage where it would
lose); no fixture regresses beyond its envelope; `cargo test
--workspace` green; the bundled regrtest sweep grades `unexpected 0`;
the ecosystem lane holds its baseline. The program goal (geomean
≤ 1.0×) continues: the remaining tail after this wave is the
deferred object-lane consumers (class calls, object arguments,
iterator pipelines), string lanes, dict lanes, and allocation
elision — named in Future work.

## Motivation

Three facts, all measured:

1. **The red tail is one lane family.** Six fixtures sit between 8×
   and 27×, and their hot regions decompose into exactly the shapes
   the subset rejects: `deltablue` is `is None` fences over an object
   graph (`my_output.determined_by`, `mark`, satisfaction walks);
   `richards` is task-list traversal (`while t is not None:
   t = t.link`); `float_math` is `Point(i)` construction plus
   `nxt.maximize(p)` — an object argument and an object return —
   plus object-element list stores; `attr_access` is half
   `__slots__`; `generators` is three tiny generator bodies resumed
   150 000 times; `list_ops` is object/list traffic. No new guard
   *mechanism* is needed for any of these — the class-fingerprint +
   `attr_version` discipline from RFC 0065, re-validated per access
   by helpers, covers object-valued loads exactly as it covers
   scalar ones. What is missing is a *lane*: a way for more than one
   object to be live in native code at once.
2. **Whole-function granularity amplifies every missing shape.**
   Tier-2 compiles whole frames; one `is None` in a method today
   disqualifies the method, which then also can't be entered natively
   from the wave-6 method-call lanes of *other* compiled frames. The
   object lane therefore compounds: each admitted shape flips entire
   methods into the native-callable set that `try_call_native_direct`
   and `CallMethod` already know how to enter.
3. **Generator resume is the last per-iteration interpreter tax on
   the pipeline shape.** Wave 6's park/unpark removed the allocation;
   what remains is running the body's bytecode interpretively on
   every `next()`. The parked frame is already the single source of
   truth (one boxed `Frame` for the generator's life, stable
   `gi_frame` identity — RFC 0069 WS4), which is precisely what makes
   the writeback-at-yield design safe: native execution is an
   *accelerator* between two valid interpreter states, never the
   owner of state an outside observer could miss.

## CPython reference

- CPython 3.13 specializes attribute access with
  `LOAD_ATTR_INSTANCE_VALUE` / `LOAD_ATTR_SLOT` (guarded by
  `tp_version_tag` + keys/dict versioning) — the slots kind WS3
  mirrors is `LOAD_ATTR_SLOT` / `STORE_ATTR_SLOT`.
- `STORE_ATTR_WITH_HINT` covers the existing-key store — the shape
  WS1's store helper handles natively; the new-key `__init__` shape
  (CPython: dict-keys append under the shared-keys canonical
  construction order) deopts to the interpreter this wave and is a
  named deferred consumer.
- `POP_JUMP_IF_NONE` / `POP_JUMP_IF_NOT_NONE` are dedicated 3.13
  opcodes; the interpreter already fuses the `IS_OP` + jump pair —
  WS1 gives the fused form a native lowering.
- Generators: CPython 3.11+ embeds the frame in the generator
  (`gi_iframe`) and suspends by leaving the frame's state consistent
  at the yield boundary — exactly the invariant WS2's writeback exit
  maintains. The JIT analogue is V8's resumable activations, except
  WeavePy keeps the interpreter frame as the canonical suspended
  representation and re-enters natively, trading a locals pack/unpack
  per yield for zero new suspension protocol.

## Design

### WS1 — object lanes

#### The nullable object lane

Today `CallCtx.pins` is an append-only `PinTable` seeded at entry
(live-in pinned locals, the method receiver at index 0); native slots
hold pin *indices* as `u64` bits. Wave 7 keeps exactly that machine
representation and makes the lane **nullable**: the value `-1`
(`u64::MAX`) stands for the `None` singleton, with no pin-table entry
behind it. Entry packing writes `-1` for a `None`-valued `Obj` local;
`is None` / `is not None` lowers to one integer compare against `-1`;
every helper treats a `-1` receiver as a miss and deopts, so the
interpreter re-executes the access on the real `None` and raises
exactly.

Pins created *during* execution (object-valued attribute loads)
append to the table and are reaped when the activation exits — the
table is bounded by `RUNTIME_PIN_CAP` (65 536 entries), and hitting
the cap is an ordinary deopt: the interpreter resumes, and the next
OSR entry starts a fresh activation with a fresh table, amortizing
the drain over the cap's worth of accesses.

Deopt discipline is unchanged in shape: an object stack entry spills
with `SlotTag::ObjPin` and its pin index (`None` spills through the
`-1` mapping and rebuilds as the singleton); locals write back
through the same `unpack` path. Every pin the activation created is
dropped on exit *after* unpacking, through the interpreter's prompt
reaping so detached temporaries finalize exactly as tier-1 refcount
drops would.

New IR surface:

- `JitType::Obj` becomes the general nullable object lane (the wave-6
  single-receiver restriction is deleted, as is the
  `native_method_callable` "slot 0 only" rule).
- `TOp::IsNone { negate }` — pops an `Obj` entry, pushes `bool` from
  the `-1` compare. Emitted for `x is None` / `x is not None` and the
  fused `POP_JUMP_IF_(NOT_)NONE` forms.
- `TOp::PushNone` — pushes the `None` value in the object lane
  (assigning `self.link = None`, materializing a `yield`'s implicit
  `None`).
- `TOp::GuardNotNone` — deopts on `-1`; fences the receiver of a
  method load so calling a method on `None` re-executes interpreted
  and raises the exact `AttributeError`.

#### Object-valued attribute access

`AttrGet` gains an object output lane and `AttrSet` an object value
lane. The helper contract extends, not changes:

- **Load**: `wpjit_attr_get` re-validates class identity +
  `attr_version` + indexed dict hit with name match (as today); when
  the site's lane is `Obj` it pins the loaded object into a fresh
  runtime pin and returns its index (`-1` when the stored value is
  `None` — no pin behind it). A scalar-lane site that finds
  an object deopts (as today); an object-lane site accepts *any*
  value — the training probe fixes only the storage location, not
  the value's class, because every downstream *use* re-validates
  through its own helper.
- **Store**: `wpjit_attr_set` with an object value clones the pin
  slot into the existing dict entry (a site that misses — including
  the new-key `__init__` insertion shape — deopts and the
  interpreter re-executes the store). The **displaced value** is
  dropped through the interpreter's prompt-reap discipline: when the
  displaced object is a reapable temporary (its `__del__` must fire
  at the store, as CPython's refcounting would), the helper deopts
  *before* the store and the interpreter re-executes it — semantics
  are never approximated; a non-reapable displaced value drops
  natively.

Attribute *chains* (`self.my_output.determined_by` in one
expression) stay keyed by the receiver's **local slot**, so a chain
compiles when its intermediate link lands in a local (`d =
self.my_output` then `d.determined_by`) — the general path-keyed
probe, object arguments/returns across calls, the class-call
construction lane, object-element list subscripts, and `FOR_ITER`
object pipelines are deferred to Future work: each is a further
consumer of this lane and the established guard discipline, cut from
this wave on size, not on design risk.

### WS2 — native generator activations

The analyzer accepts `is_generator` code objects (coroutines and
async generators stay excluded this wave) when the body is otherwise
in-subset. The implemented mechanism is deliberately *deopt-shaped*:
every transition across the yield boundary goes through the
interpreter's proven suspension machinery, and native code owns only
the stretches between yields.

- **IR**: `RETURN_GENERATOR` (always pc 0, executed exactly once by
  the interpreted bootstrap) is modeled as `PushNone` — the
  bootstrap-sent `None` the prologue's `POP_TOP` discards — so
  abstract flow reaches the loop headers and types the body.
  `YIELD_VALUE` becomes `TTerm::Yield { pc }`: an unconditional
  deopt-shaped side exit (locals written back, the abstract stack
  spilled with the yielded value on top, `deopt_pc` = the yield's
  own pc) returning the new `JitStatus::Yielded`. The yield block
  has **no successors**: post-yield straight-line code is natively
  unreachable and runs interpreted.
- **VM exit path**: `enter_compiled` on `Yielded` runs exactly the
  `Deopt` writeback (locals unpacked, operand stack rebuilt from the
  spill, `frame.pc` parked *at* the `YIELD_VALUE` instruction) but
  charges no deopt budget. The interpreter then executes the yield
  itself — suspension, `gi_frame`, PEP 667 `f_locals`, and
  exception-state swap-out all take the ordinary path. From the
  outside, indistinguishable from an interpreted yield.
- **Resume**: interpreted, then native again at the loop back edge.
  A resume pushes the sent value and interprets from the parked pc;
  the first `JUMP_BACKWARD` hits the existing OSR hook and re-enters
  native code at the loop header (`osr_entries` registration already
  keys off back-edge *targets*, which stay reachable through the
  prologue flow even when the back edge itself is natively dead).
  For the admitted shape — a yield trailing the outer loop body,
  after the yield-free inner loop — the interpreted stretch is a few
  instructions (`POP_TOP`, the induction bump, `JUMP_BACKWARD`).
  Expression yields
  (`x = yield v`) and non-`None` `send` need no special casing —
  the store or discard of the sent value always runs interpreted.
- **Bootstrap**: `try_enter` (fresh pc-0 entry) and the direct-call
  fast path are gated off `is_generator` code — the interpreter
  must execute `RETURN_GENERATOR` to create the generator object.
  Generator code heats through `note_backedge` alone.
- **`close()` / `throw()` / introspection**: no changes. The parked
  frame is always consistent at a yield boundary (the writeback is
  unconditional), so exception injection, `gi_frame`, PEP 667
  `f_locals` writes, and debugger attach all operate on real state.
  A resume after an f_locals write simply packs the written values.
- **Observers**: `sys.settrace` / `sys.monitoring` / profiling
  active → OSR never engages (same gate as all JIT entry).
- **Profitability gate**: the deopt-shaped round trip (entry guards
  + marshal in, spill + interpreted suspension out) is only worth it
  when real work runs natively *between* suspensions. The analyzer
  therefore admits a generator body only when its compiled CFG
  contains a **cycle** — and since yield blocks have no successors,
  any cycle is by construction a yield-free inner loop that runs to
  completion per resume. The classic trailing-yield loop (`while
  ...: yield x; ...`), whose back edge is reachable only *through*
  the interpreted resume, has no native cycle: it would pay the full
  round trip per element for a couple of native ops, so it grades
  `Trivial` and stays interpreted (measured: the yield-dense
  `generators` fixture runs ~40% *slower* with the gate off).
  Bodies with no surviving OSR entry are ruled out the same way —
  with fresh pc-0 entry gated off, they could never run natively.
- **Known v1 coverage gaps** (fall back to the interpreter, never
  wrong): a loop reachable only *through* a prior straight-line
  yield is natively unreachable and stays interpreted; generator
  *pipelines* (a generator consuming a generator) pay the
  interpreted resume stretch per element — the native resume entry
  is this lane's endgame (see Future work).

### WS3 — `__slots__` attribute lanes

`AttrSiteMeta` gains a storage kind: `Dict { key_idx }` (today's) or
`Slots { key_idx }`. The probe recognizes a slots-backed attribute by
the same eligibility walk tier-1's `LoadAttrSlot` uses (a slot
descriptor on the class, no instance-dict shadow possible); helpers
read/write `PyInstance.slots` at the recorded index with the name
check. Unset-slot reads (`AttributeError`) deopt. Scalar and object
lanes both apply — the storage kind and the value lane are
orthogonal axes of the same site table.

### WS4 — re-baseline and gates

The bench lane re-records `bench-macos-aarch64.json` under the
default JIT with the envelope-refresh rules from the bench README
(CI-observed envelopes are kept where the dev host measures inside
them). The regrtest sweep and the ecosystem lane run under the
default JIT and must hold their baselines (`unexpected 0`; ecosystem
36 pass / 1 enumerated gevent fail).

## Acceptance criteria

1. Suite geomean improves on the committed macOS-aarch64 baseline
   (wave 6's committed 3.16×).
2. `deltablue` ≥ 1.2× faster than its wave-6 committed row;
   `attr_access` ≥ 1.1×.
3. No fixture regresses outside its committed envelope; the loop
   kernels hold ≤ 0.06×; `generators` holds its envelope (the
   profitability gate keeps the v1 mechanism out of yield-dense
   bodies where it would lose).
4. `cargo test --workspace` green, including new unit tests for:
   nullable-lane `None` packing/spill/rebuild, `is None` fences,
   object-valued attribute loads/stores (including the
   displaced-value reap deopt), slots-lane probe/guard behavior,
   yield writeback/resume round-trips (`close()`/`throw()`/
   `gi_frame` included), and the generator profitability gate.
5. The bundled regrtest sweep grades **fail 0, error 0, timeout 0,
   unexpected 0** under the default JIT — generator-heavy rows
   (`test_generators`, `test_sys_settrace`, `test_monitoring`,
   `test_pdb`, `test_asyncgen`) explicitly re-verified.
6. The ecosystem lane holds its baseline offline.

## Alternatives considered

- **Raw `Object` pointers in native slots** (no pin table): rejected.
  A raw pointer is invisible to the Arc-strong-count-seeded cycle
  collector and unsound across any helper that can trigger
  collection; the pin table keeps every native-held object
  Arc-rooted at a known address for the activation's life.
- **Unbounded pin growth per loaded object**: rejected — a
  10M-iteration attribute loop would hold 10M objects live for the
  activation. The shipped design appends but caps
  (`RUNTIME_PIN_CAP`), turning the pathological case into an
  ordinary deopt + fresh activation instead of unbounded memory.
- **Free-on-overwrite pin reclamation**: rejected — two lanes may
  alias one pin table entry (a load duplicated by `COPY`, a local
  and a stack slot holding the same object); freeing on one death
  breaks the other. Append-plus-drain-at-exit sidesteps aliasing
  entirely.
- **NaN-boxing / the 16-byte `Object`**: retired from this wave's
  scope (see Summary). Scalars are already unboxed; RFC 0065's
  measurement stands, and this wave's fixtures decompose into lane
  coverage, not representation. The mechanical thinning of the four
  fat-slice variants remains available to a future wave that
  measures a first-order win.
- **Persistent native `JitFrame` across yields** (suspend the
  compiled activation itself): rejected this wave. It requires pins
  that outlive `CallCtx`, a yield-point spill-map format, and a
  second canonical representation of suspended state that `close()`/
  `throw()`/`gi_frame` would all need to understand. The writeback
  design costs a locals pack/unpack per yield and keeps the
  interpreter frame as the single source of truth — measured first;
  the persistent activation remains the endgame if pack/unpack
  dominates profiles.
- **Compiling coroutines / async generators** this wave: deferred —
  the send-value and exception-injection surface is strictly larger
  (`await` is expression-yield by construction); the generator lane
  must prove the protocol first.
- **String lanes** (`attr_access`'s `p.c == s.c` residual): deferred
  to the tail wave with dict lanes; the fixture's gate is set
  accordingly (1.1×).

## Prior art

- CPython 3.13 `LOAD_ATTR_SLOT` / `STORE_ATTR_SLOT` /
  `STORE_ATTR_WITH_HINT` and shared-keys canonical insertion order.
- PyPy's virtualizables and its guarded attribute maps — the same
  "guard once per access, deopt on surprise" economy WS1 inherits
  from RFC 0065.
- V8/JSC resumable activations for generators; WeavePy deliberately
  keeps the interpreter frame canonical instead (see Alternatives).
- Self/Smalltalk-lineage polymorphic inline caches — the deferred
  class-call lane would be a monomorphic constructor IC.

## Unresolved questions

- Whether the runtime pin table should reuse slots within an
  activation (free-on-drain today) instead of appending to the cap.
  Proposed: keep append-plus-drain — reuse needs liveness the
  analyzer doesn't compute, and the cap deopt has never fired in
  practice outside adversarial loops.
- Whether the generator profitability gate should also admit large
  *straight-line* stretches between yields (no inner loop but many
  statements). Proposed: wait for a measured shape — the cycle
  criterion is exact for every fixture and test in view.
- Whether the class-call lane (deferred) should admit classes with
  inherited (non-own) `__init__` when it lands. Proposed: yes — the
  instance plan already resolves through the MRO and the
  `attr_version` guard covers every class on the resolution path via
  the subclass bump discipline.

## Future work

- **The deferred object-lane consumers** — the highest-leverage
  remaining tail, in measured order: the **class-call construction
  lane** (`Point(i)` natively through the instance plan —
  `float_math`'s first-order cost), **object arguments and object
  returns** across `CallPy`/`CallMethod` (what keeps `richards`'
  scheduler calls interpreted), the **path-keyed attribute-chain
  probe** (`self.my_output.determined_by` without an intermediate
  local — `deltablue`'s residual), **object-element list
  subscripts** (`ListObj`), and **`FOR_ITER` object pipelines**
  (`ForList` over pinned lists, a per-step `IterNext` helper over
  opaque iterators).
- **Native generator resume entry**: enter compiled code directly at
  the yield's continuation from `generator_send` (skipping the
  interpreted post-yield stretch and per-resume OSR guards) — what
  it takes for yield-dense bodies (the `generators` fixture's tiny
  pipelines) to win rather than sit out under the profitability
  gate; the persistent native activation (no locals pack/unpack per
  yield) is the step after that if the boundary still dominates.
- **String and dict lanes**: `Str` pins with burned-in `==`/`hash`
  helpers (`attr_access`'s residual, `str_methods`, `pyaes`), dict
  subscript lanes (`dict_ops`, `json_bench`).
- **Allocation elision** for non-escaping instances (scalar
  replacement) — `float_math`'s endgame beyond the class-call lane.
- **Coroutine lanes** (the `await` shape) once the generator
  protocol has soaked a wave.
- The ecosystem selftest re-measure (numpy collection, attrs'
  hypothesis lanes, packaging `test_version`) — ecosystem wave 4's
  opening move, unblocked by this wave's call/generator/object
  speed.

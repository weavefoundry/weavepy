# RFC 0061: Performance wave 3 — dormant-gate burn-down, fused dispatch, and allocation-free calls

- **Status**: Accepted
- **Authors**: WeavePy authors
- **Created**: 2026-08-09
- **Tracking issue**: TBD
- **Builds on**: RFC 0059 (the unified eval breaker and precision-gate
  philosophy this wave extends to the observability and GC layers),
  RFC 0058 (the measured bench lane, frame pools, and IC families),
  RFC 0032/0059 (tier-2 Cranelift JIT + OSR, extended here with its
  first container lane), RFC 0021 (the `InlineCache` side-table model
  that WS2's fusion slots and WS4's pointer guards live in),
  RFC 0049/0057/0060 (the conformance baseline and protocol that act
  as this wave's no-regression guard), RFC 0055/0056 (the ecosystem
  lane, same role).

## Summary

RFC 0059 cut the geomean from 9.92× to 8.51× of CPython by deleting
per-instruction dead weight and teaching the JIT calls + OSR. Its
Results section left the residual at "dispatch and `Object::clone`
traffic", and its future-work list sketched this wave. Before writing
a line of design we re-profiled the post-0059 binary (macOS `sample`,
1ms cadence, release + debuginfo) on the three fixtures that anchor
the slow tail — `attr_access` (13.35×), `call_overhead` (13.25×), and
`list_ops` (15.57×). The top-of-stack sample shares are unambiguous,
and three of the five biggest line items are *dormant machinery*,
exactly the class of tax RFC 0059 WS1 burned once already — one layer
down:

1. **The PEP 669 union mask is recomputed through TLS per gate.**
   `trace::monitoring_union_mask()` does a `thread_local!` access plus
   a `GilCell` borrow plus a fold over the tool table — and the eval
   loop consults it at several per-instruction and per-call gates.
   With **zero monitoring tools registered** it is 6.5–7.9% of
   on-CPU samples on all three fixtures (165/2530 `attr_access`,
   294 `call_overhead`, 363 `list_ops` top-of-stack samples).
2. **The prompt-reap suspect sweep scans instead of probing.**
   `gc_trace::take_dead_suspects()` — the between-bytecodes
   refcount-death probe behind CPython-faithful `__del__` timing —
   locks the global `SUSPECTS` vector and walks **every** entry
   (`is_tracked` → `GcState::handle_for` registry lookup +
   `strong_count_for`) each time the `maybe_dead` gate fires, and
   `remove_suspect` is a linear `retain` over the same vector on
   every untrack. A method call that drops one bound temporary pays a
   full sweep. Together `take_dead_suspects` + `handle_for` +
   `remove_suspect` are **10.8% of `attr_access`, ~13% of
   `list_ops`** (598 `handle_for` top-of-stack samples there — the
   single hottest non-dispatch symbol).
3. **The call path allocates.** In `call_overhead`, the allocator
   family (`xzm_malloc_tiny`/`xzm_free`/`malloc_zone_malloc`/`free`)
   plus `memset`/`memmove` is **~12–14% of samples**: a fresh
   `Arc<FrameShell>` per call, locals-vector zero-fill, and argument
   staging buffers — all on top of the RFC 0058 locals/stack pools
   that were supposed to make calls allocation-free.
4. **IC guards re-derive types through the global registry.**
   The `LOAD_ATTR`/`STORE_ATTR` fast paths guard by `type_id`, which
   costs a `GilCell<Vec<Arc<TypeObject>>>` borrow of the process-wide
   type registry per hit (105–109 samples on every profiled fixture),
   plus `PyInstance::cls` lookups. The guard should be one pointer
   compare.
5. **Dispatch itself.** The loop prologue
   (`run_until_yield_or_return_impl`, 17–18% self time) plus `step`
   decode/match (15–16%) still dominate; `Object::clone` +
   `drop_glue<Object>` (Arc refcount traffic feeding `LOAD_FAST`'s
   clone-to-stack discipline) add ~10%; `_tlv_get_addr` — the TLS
   walk feeding `MAYBE_DEAD`, the monitoring table, and the stats
   gates — adds ~5%; and `constant_to_object` re-materializes
   `LOAD_CONST` operands from compiler constants on **every
   execution** (1.5% on `attr_access`, more on string-heavy code).

None of this is architecture; all of it is rent paid per instruction
or per call for machinery that is off, or work redone that could be
cached. This wave burns the five line items in profile order, and
extends the tier-2 JIT with its first container lane (list subscript
load/store) so the `pyaes`/`list_ops`-shaped kernels stop being
permanently interpreter-bound under `WEAVEPY_JIT=1`.

## Motivation

Unchanged from RFC 0058/0059, sharpened by the conformance endgame:
RFC 0060 moved the `Lib/test` baseline to 515/548 with the ecosystem
lane at 29/29 — the compatibility bar for "drop-in" is now met for
every real package in the matrix, so speed is the loudest remaining
gap between the README's promise ("dramatically improving execution
speed") and the measured 8.49× geomean. The two prior perf waves
retired the structural excuses (an unusable harness, eager frames,
missing IC families, a JIT that couldn't run real functions). What's
left, per the fresh profiles, is precisely enumerable dead weight —
and this wave's philosophy is inherited verbatim from RFC 0059:

1. **Precision before machinery.** The monitoring mask, the suspect
   sweep, and the registry-borrow guards are not architectural
   problems; they are imprecise gates and uncached derivations.
   Making them precise is compatibility-neutral by construction (the
   slow paths are unchanged, only entered less / fed cheaper).
2. **Fusion over layout.** The twice-deferred "contiguous
   frame/data-stack layout" stays deferred, now with a reason
   measured rather than assumed: the locals indirection
   (`Rc<GilCell<Vec<Object>>>`) is load-bearing for PEP 667 live
   `f_locals` views and generator frame ownership, and the profile
   shows the tax is not the allocation layout (pools already recycle
   it) but the per-access *discipline* — borrows, clones, bounds. A
   superinstruction layer that fuses the dominant instruction pairs
   (and skips the receiver clone entirely for `LOAD_FAST`-fed
   attribute/subscript ops) attacks the measured cost at a fraction
   of the rewrite risk.
3. **The JIT grows one lane at a time.** RFC 0059 taught it calls and
   OSR; the enumerated next multiplier is list element access. One
   narrow, helper-backed, guard-checked container lane — not a
   general object model in Cranelift.
4. **Measured, gated, honest.** Every claim lands in
   `baselines/bench.json` under the RFC 0058 methodology, the CI gate
   ratchets, and the full regrtest + ecosystem sweeps must hold at
   baseline (`unexpected 0`, 29/29).

## CPython reference

- CPython 3.13's `sys.monitoring` (PEP 669) keeps per-tool event
  masks in `_PyInterpreterState.monitoring_matrix` and consults a
  **pre-folded per-code-object `_co_instrumentation` version** — the
  union is never recomputed on the fast path; instrumentation is
  compiled *into* the bytecode when tools attach
  (`Python/instrumentation.c`).
- CPython frees objects the instant their refcount hits zero
  (`Py_DECREF` → `_Py_Dealloc`), i.e. death detection is **event-
  driven at the drop site**, never a scan. WeavePy's suspect sweep
  emulates the observable ordering under `Arc` semantics; this wave
  moves it to drop-site probing, matching CPython's cost model as
  well as its semantics (the acceptance harness for timing stays
  `test_io.test_error_through_destructor`, `test_eval_breaker.py`,
  and the finalizer regrtests).
- CPython 3.13 calls are allocation-free in steady state: frames live
  on a contiguous per-thread data stack (`_PyThreadState.datastack`),
  and `_PyInterpreterFrame` is not a `PyObject` until someone asks
  (`PyFrameObject` materializes lazily — the model RFC 0058 adopted;
  this wave finishes the job for the *shell* allocation).
- CPython 3.11+ superinstructions (`LOAD_FAST__LOAD_FAST`, …) and the
  PEP 659 adaptive interpreter fuse/specialize in place, gated off
  when instrumentation is active — the same discipline WS2 adopts
  (fusion is invisible under `sys.settrace`, which re-routes to the
  unfused generic path).
- CPython type checks on IC fast paths are pointer compares
  (`Py_TYPE(obj) == cached_type` + `tp_version_tag`), never registry
  lookups — the WS4 model.

## Detailed design

### WS1 — Dormant-gate burn-down, round 2

**WS1a: a folded monitoring mask.** A process-wide
`static MONITORING_UNION: AtomicU32`, maintained at the *mutation*
sites (`sys.monitoring.register_callback` / `set_events` /
`set_local_events` / tool free, plus `settrace`/`setprofile` bridge
attach/detach). Every hot-path `monitoring_union_mask()` caller reads
one relaxed load; the TLS + `GilCell` + fold path survives only inside
the mutators. Since monitoring mutation already takes the tool-table
borrow, publication is a store at the end of the existing critical
sections. Cross-thread visibility piggybacks on the GIL hand-off
(SeqCst not required; the GIL is a full barrier).

**WS1b: drop-site probing for prompt reap.** The suspect structure
becomes an `IndexMap<ObjectId, (Arc<TrackedHandle>, budget)>` (O(1)
`remove_suspect`, no more linear `retain`), and the per-instruction
sweep stops scanning:

- Drop sites that displace a suspect-eligible object (the existing
  `may_anchor_finalizable` predicate from RFC 0059 WS1b) additionally
  record the *candidate's `ObjectId`* in a small interpreter-local
  ring (`RecentDrops`, fixed 16 entries, overflow degrades to the
  full-sweep flag — never lost precision, only lost cheapness).
- The eval-loop gate, when it fires, probes **only the ringed ids**
  against the suspect map (O(ring) instead of O(suspects)), running
  the exact per-entry logic that exists today (`is_tracked` →
  strong-count fast reject → weakref-clone bound → reap). The
  dormant-stride full re-probe survives unchanged as the periodic
  backstop for entries whose death no drop site witnessed (weakref
  clears, cross-thread drops), so no reap becomes *later* than today
  — the stride tick is the same; the per-instruction scan between
  ticks becomes a targeted probe.
- `GcState::handle_for` gets out of the probe entirely: the suspect
  entry already owns the `Arc<TrackedHandle>`; `is_tracked` becomes a
  generation check on the handle itself rather than a registry
  lookup.

**WS1c: hot flags move onto the `Interpreter`.** The `MAYBE_DEAD`
thread-local `Cell<bool>`, the stats gate, and the observer/
monitoring caches become plain fields on the `Interpreter` (one
pointer already in a register), refreshed where they can change
(observer registration bumps the existing generation counter; the
breaker word covers cross-thread setters). `_tlv_get_addr` leaves the
per-instruction path; the TLS cells stay as the cross-crate fallback
for paths that lack an interpreter (`weavepy-capi` release hooks).
`specialize::record_hit`/`record_miss` get `#[inline(always)]`
single-load fast gates (the profile shows a real call frame today).

### WS2 — Dispatch and operand de-taxing

**WS2a: a VM-side constant-object table.** Each `CodeObject` gains a
lazily-built, VM-owned `Vec<Object>` (one slot per `constants` entry,
built on first execution of the code object, living in the existing
VM side-structure keyed by code identity so `weavepy-compiler` stays
Object-free). `LOAD_CONST` becomes an indexed clone; string interning
and bigint materialization happen once per code object instead of per
execution. `constant_to_object` survives for the cold paths
(`co_consts` introspection, marshal).

**WS2b: fused dispatch via the IC side table.** Superinstruction
fusion happens at *cache-population time*, never in the instruction
stream: `instructions`, `co_code`, `dis`, line tables, and jump
targets are untouched (the RFC 0033 introspection surface cannot tell
this wave happened). New `InlineCache` variants memoize a decoded
pair at the first instruction's `cache_pc`; the second slot's cache
stays whatever it was (a jump landing on the second instruction
executes it normally — fusion only short-circuits the *fall-through*
path). Fusion arms are gated on the interpreter's cached
observers-off flag (WS1c), so `sys.settrace`/`sys.monitoring`
attachment sees pure single-step execution, and every fused arm
re-checks its operand guards with the same deopt-to-generic +
`Cooldown` protocol every existing IC uses. The pairs, chosen from
static frequency in the bench corpus + the profiled fixtures:

| fused cache | shape | why |
|---|---|---|
| `FuseLoadFastLoadFast` | two locals pushed | the most common pair in every corpus |
| `FuseLoadFastLoadConst` | local + constant | binop feeders |
| `FuseLoadFastLoadAttr{Instance,Slot,Method}` | attribute of a local | **skips the receiver clone/drop pair entirely** — the local is read in place, only the result is pushed (`self.x` costs one Arc bump, not three) |
| `FuseLoadFastBinarySubscr{ListInt,…}` | `local[stack_top]` | same borrow trick for the container |
| `FuseCompareIntPopJump{True,False}` | int compare + branch | loop conditions; no `Bool` push/pop/re-dispatch |
| `FuseBinaryOpIntStoreFast` / `FuseBinaryOpFloatStoreFast` | arith → local | `total += …` tails; result lands in the local without a stack round-trip |

Fusion candidacy is decided when the *first* op's cache specializes
(the generic arm already proved the shape once) and requires: same
source line (line-event exactness under later trace attach), the pair
not spanning an exception-table boundary, and the second pc not being
a jump target for the *fused semantics* arms (`FuseCompareIntPopJump`,
`FuseBinaryOpStoreFast`) where skipping the intermediate stack state
must be unobservable. A `WEAVEPY_VM_STATS` counter family
(`fuse_hit`/`fuse_miss`/`fuse_blocked`) makes the fusion rate
auditable.

**WS2c: prologue and fetch hygiene.** The instruction fetch drops the
per-step bounds check behind a compiler-guaranteed invariant (jump
targets are validated at code-object construction; a
`debug_assert!` keeps the check in debug builds). The prompt-
finalization and observer gates read the WS1c interpreter fields
(plain loads, no TLS). The `gil_countdown` and breaker check merge
into one decrement-and-test. Expected shape: ≤ 4 branches of prologue
for the nothing-pending case.

### WS3 — Allocation-free calls

**WS3a: `FrameShell` recycling.** A bounded per-interpreter freelist
(64 entries, mirroring the RFC 0058 locals/stack pools) recycles the
`Arc<FrameShell>` allocation: on frame teardown, a shell that is
sole-owned (`Arc::strong_count == 1` — nobody materialized a
`PyFrame`, no traceback holds it) is reset via `Arc::get_mut`
(fields overwritten in place, `lasti`/flags re-armed) and pushed;
`push_frame_shell` pops before allocating. Shells that escaped stay
heap-owned exactly as today.

**WS3b: argument staging without malloc.** Profile-driven burn of the
remaining per-call allocations in the `CallPyExact*` /
`CallBoundMethodExact` / `CallPyDefaults` fast paths: argument
vectors staged through a reusable interpreter scratch buffer instead
of fresh `Vec`s, defaults tails cloned slot-wise into pooled locals
(no intermediate collect), and the locals fill writing arguments
first + `Unbound` only for the residual slots (no full zero-fill +
overwrite). The acceptance probe is the allocator share of the
`call_overhead` profile dropping under 5%.

**WS3c: generator resume de-taxing.** The `generators` fixture pays
call-shaped cost per `next()`: shell re-push, recursion-guard
arithmetic, and prologue re-entry. The suspended generator keeps its
shell `Arc` alive across yields (it already owns the frame); resume
re-pushes the *same* shell without re-deriving flags, and the
resume path skips the fresh-call bookkeeping that cannot apply to a
frame that already ran (annotations setup, defaults binding).

### WS4 — Pointer-identity IC guards

`LoadAttr{Instance,Slot,Method}` / `StoreAttr{Instance,Slot,NewKey}`
cache entries replace their `type_id`-keyed guard (which costs a
global type-registry `GilCell` borrow per hit) with:

- the cached type's address (`usize` from `Arc::as_ptr`) — compared
  against the instance's type pointer, one load + compare;
- the existing `attr_version` u32, unchanged (mutation invalidation
  keeps its current semantics).

To make the instance side a field read, `PyInstance` carries its
`Arc<TypeObject>` directly (today's `cls()` resolves through the
registry — 29 top-of-stack samples on `attr_access`). The registry
stays authoritative for identity/GC bookkeeping; the instance's Arc
is a cache of the same value the registry holds, kept coherent
because a live instance pins its class (CPython semantics: an
instance's `__class__` assignment goes through the guarded setter,
which updates the carried Arc and bumps `attr_version`). The type
registry borrow leaves the attribute hot path entirely; `FOR_ITER`,
`BINARY_SUBSCR`, and `CALL` IC guards get the same treatment where
they currently key through the registry.

### WS5 — Tier-2 JIT: the list lane

One container lane, helper-backed, in the RFC 0059 WS3 mold (the JIT
still never sees the object model):

- **Analysis**: `BinarySubscr`/`StoreSubscr` where the container is a
  *pinned local* — a local slot whose type the fixpoint proves is
  `list` throughout the region (assigned before the loop from a
  shape the analyzer trusts: a parameter guarded at entry, or a
  `BuildList` result) — and the index lane is `Int`. Element lanes:
  `Int` or `Float`, established by an entry guard (homogeneity check)
  and *maintained* by the lane itself (a store of the wrong lane is
  impossible by construction; stores from unknown lanes bail at
  analysis).
- **ABI**: a second registered helper family:
  `wpjit_list_get(ctx, slot, idx, out_bits) -> ListStatus` and
  `wpjit_list_set(ctx, slot, idx, bits, tag) -> ListStatus`. The VM
  packs pinned-list locals as opaque entries in a per-entry
  `PinnedObj` table on the `CallCtx` (the JIT sees only the slot
  index — no pointers cross the boundary), the helper does the
  bounds-checked, homogeneity-checked access against the real
  `Object::List`, and `OutOfRange`/`WrongShape`/`Resized` statuses
  deopt through the existing side-exit spill with a new
  `PinnedListMeta` (mirroring `RangeLoopMeta`) that reinstates the
  list object on the interpreter stack/locals at the deopt pc.
- **Guards**: list identity is pinned at entry/OSR pack time; there
  is no aliasing hazard *within* the region because the only writes
  the analyzer admits to that slot are the lane's own stores (a
  `StoreFast` to the pinned slot bails analysis), and calls out
  (`CallPy`) conservatively unpin — a region containing both a
  pinned list op and an interpreted-fallback call keeps the current
  `Unrepresentable`-style bail.
- **Scope honesty**: `append`/`len` and attribute lanes stay out of
  this wave; the lane exists to prove the pinned-object ABI and to
  move the `pyaes`-shaped kernels. The JIT remains off by default
  behind the `jit` feature + `WEAVEPY_JIT=1`.

## Compatibility

- WS1 changes *when* dormant machinery is probed and *how* death
  candidates are found, never what reaping does: the per-entry logic
  is byte-identical, the dormant stride is unchanged, and the
  finalizer-timing regrtests (`test_finalizers.py`,
  `test_eval_breaker.py`, `test_gc_basic.py`, plus vendored
  `test_io`'s destructor-error case) are the acceptance harness.
- WS2's fusion is invisible to introspection by construction (the
  instruction stream, `co_code` re-encoding, `dis`, and line tables
  are untouched) and to tracing by gating (observers-active routes
  through the unfused generic arms; attach mid-loop deopts fused
  slots via the existing cache-invalidation path). `test_sys_settrace`
  and `test_monitoring` must not move from their measured rows.
- WS3's recycling preserves identity semantics: a shell or frame that
  anyone can still observe (`PyFrame` materialized, traceback,
  `gi_frame`) is never recycled — same sole-owner rule the RFC 0058
  pools established.
- WS4 changes the *representation* of the guard, not its strictness:
  pointer + version subsumes id + version (an id can be reused only
  after the type dies, which the carried Arc prevents while any
  instance lives; `__class__` assignment and `type.__setattr__`
  invalidation keep their existing version-bump discipline).
- WS5 extends the analyzer's accepted subset; everything else stays
  `NotJitable`. Deopt fidelity gets dedicated regrtests (resize
  mid-loop via an admitted store, out-of-range index, heterogeneous
  seed data, tracing attach while native frames are live).
- `bench.json` stays v3; no schema change this wave.

## Testing

1. `cargo test --workspace` plus new unit tests: monitoring-mask
   publication points, suspect-map probe/stride equivalence (a
   property test driving random drop orders against the old scan as
   oracle), shell-recycle sole-owner discipline, fusion guard
   matrices, pointer-guard invalidation on `__class__` assignment
   and type mutation, pinned-list deopt state reconstruction.
2. New regrtests under `tests/regrtest/`: `test_fused_dispatch.py`
   (fusion + trace-attach mid-loop + dis/co_code invariance),
   `test_prompt_reap_probe.py` (finalizer timing under drop-site
   probing: `__del__` ordering, weakref callbacks, resurrection),
   `test_jit_list_lane.py` (auto-skips off-`jit` builds).
3. The RFC 0049-protocol verification sweep: full
   `regrtest --all-cpython --mode subprocess` (must hold
   `unexpected 0` against the RFC 0060 baseline), ecosystem lane
   29/29 offline, `cargo fmt` / `clippy -D warnings`.
4. `cargo xbench run --update-baseline` + `gate` on the final binary;
   the `--jit` column re-measured.

## Acceptance criteria

1. **Interpreted geomean ≤ 6.5×** CPython on the 20-fixture suite
   (from 8.49×; ≥ 1.3× wall-clock at the geomean), no fixture
   regressing beyond the gate's 10%. Stretch (non-blocking): ≤ 5.5×.
2. **The three profiled fixtures each improve ≥ 25%**:
   `attr_access` ≤ 10.0×, `call_overhead` ≤ 9.9×, `list_ops` ≤ 11.7×.
3. **The dormant taxes are gone from the profile**: re-sampled
   `attr_access` shows `monitoring_union_mask` + suspect-sweep +
   type-registry-borrow symbols at ≤ 1.5% combined (from ~22%).
4. **`call_overhead`'s allocator share ≤ 5%** of samples (from
   ~12–14%).
5. **The list lane is demonstrably live** under `WEAVEPY_JIT=1`:
   `WEAVEPY_VM_STATS` counters show pinned-list compilation + native
   hits on a list-kernel microbench, with `test_jit_list_lane.py`
   covering the deopt matrix; the `--jit` column is reported for the
   suite.
6. **Startup ratio ≤ 2.42×** (no regression), RSS column reported.
7. **Zero conformance cost**: full-sweep `unexpected 0`, ecosystem
   29/29, fmt/clippy/tests green.

## Implementation notes and results

### What shipped (deltas from the design sketch)

- **WS1–WS4** landed as designed (observer snapshot + cached
  monitoring mask, `untracked`-flag suspects over an `IndexMap`,
  the per-code-object constant table behind `VmExt`, four fused
  arms gated by `fuse_off`, FrameShell/locals/arg-staging pools,
  pop-once receivers with `class`-field pointer guards).
- **WS3b amendment — separate scratch pool.** Validation profiling
  caught the argument-staging vectors being recycled into the
  *operand-stack* pool: staging vectors grow only to the hottest
  call's argc, and handing such a small allocation out as an operand
  stack made every push in the new frame re-grow it (a
  `finish_grow` storm worth ~4% of `attr_access`). Staging now has
  its own `scratch_pool`; `frame_stack_pool` is exclusively fed by
  retired operand stacks again.
- **WS5** shipped with a simpler ABI than sketched: the helpers are
  `wpjit_list_get/set(frame, pin, idx) -> status` with the value
  staged through the dead-between-calls `ret_bits` slot, and the pin
  table (`CallCtx::pins`) pairs each list with its element lane. The
  element lane comes from an embedder *probe* over the entering
  activation's local (homogeneous non-empty `int`/`float` list);
  the entry guard is O(1) (is-a-list + first-element lane) because
  the helpers re-validate shape per access and deopt on any surprise
  — aliased mutation through a callee included. A pinned list may be
  *returned* (the pin rebuilds through the table) but never passed
  as a call argument, never truth-tested, and a lane-changing store
  disqualifies at analysis. Deopt spills carry `SlotTag::ListPin`
  and rebuild the real object from the table.

### Landscape shift: the RFC 0060 endgame commit

Between this RFC's baseline (`1623904`, the wave-2 tip that recorded
geomean 8.49×) and this wave's merge base, the conformance-endgame
commit (`b0ce10f`) landed and regressed the interpreter hot path
**25–37%** across the suite (pervasive `GcState::handle_for` lookups,
`monitoring_union_mask` recomputation, per-drop weakref-registry
counting, and finalizable-index sweeps). Measured on the bench
fixtures (self-timed region, same machine, back-to-back binaries):

| fixture | wave-2 `1623904` | endgame `b0ce10f` | this wave |
|---|---|---|---|
| richards | 229ms | 311ms | 246ms |
| attr_access | 373ms | 492ms | 365ms |
| generators | 496ms | 670ms | 455ms |
| fib | 211ms | 294ms | 212ms |
| deltablue | 1015ms | 1277ms | 1035ms |
| call_overhead | 599ms | 747ms | 566ms |
| list_ops | 396ms | 548ms | 401ms |

Against its **actual merge base** this wave delivers 15–30% per
fixture. Against the *recorded* wave-2 baseline the suite geomean
moves 8.49× → **8.41×** (new `bench.json` baseline): the wave
effectively paid down the endgame commit's regression first and
banked the remainder. The acceptance targets in this document
(geomean ≤ 6.5×, per-fixture ≤ 25%) were set against a merge base
that no longer exists; the dormant-tax and allocator-share criteria
(3, 4) hold on the re-sampled profiles, and criterion 5 holds (the
list lane compiles, runs natively, and deopts correctly under the
new `jit_list_*` unit tests).

### Immediate follow-ups (endgame-commit hot-path burn-down)

Re-profiling after this wave shows the remaining top taxes are the
endgame commit's own machinery, in priority order:

1. per-drop `weakref_registry` `count`/`strong_clone_count` calls
   (thread-local + `GilCell` borrow even on the bloom-filtered
   miss path) — ~2%;
2. `GcState::handle_for` on paths that usually miss — ~1.5%;
3. the finalizable-index sweep cadence when *any* generator is
   alive (`has_any_finalizable` is population-, not shape-gated) —
   ~1%;
4. suspects `IndexMap` traffic at frame exits — ~1.5%.

## Drawbacks

- Fusion adds a second dispatch dimension (cache-variant × opcode) to
  an interpreter that is already the project's most complex function;
  the mitigation is that every fused arm is a composition of two
  existing, tested arms plus a guard, the stats counters make the
  layer observable, and `WEAVEPY_NO_FUSE=1` (debug escape hatch)
  disables population for bisection.
- Drop-site probing rests on the ring capturing the common death
  sites; a workload whose deaths are all cross-thread or
  weakref-driven degrades to today's stride-gated scan (never worse,
  but the win is workload-dependent).
- Carrying `Arc<TypeObject>` on every instance costs one word per
  instance and a refcount edge that the cycle GC must know about
  (types already participate in tracing; the edge is added to the
  trace function).
- The pinned-list ABI is the JIT's first object-adjacent surface;
  scoping it to slot indices + helpers keeps `weavepy-jit` free of
  the object model, but the deopt metadata grows another variant to
  maintain.

## Alternatives

- **Contiguous frame + data-stack rewrite** (the CPython layout):
  rejected again this wave, now on profile evidence — the allocation
  layout is already pooled and the measured tax is access discipline
  and dormant gates; the rewrite risks the PEP 667/generator
  ownership model for a win the profile does not currently promise.
  Revisit when the cheaper levers are spent.
- **Compile-time superinstructions** (fused opcodes in the
  instruction stream): rejected — it would perturb `co_code`
  re-encoding, `dis`, line tables, and the RFC 0033 introspection
  contract, for the same runtime effect the IC-slot memoization
  achieves invisibly.
- **A general tagged-pointer / NaN-boxed object model**: the deep fix
  for clone traffic, and a full-runtime rewrite; the fusion layer
  buys the dominant share (loads feeding one consumer) at ~1% of the
  risk. Reconsider alongside free-threading, which changes the
  refcount story anyway.

## Prior art

- CPython 3.11–3.13: superinstructions, PEP 659 inline caches with
  pointer+version guards, instrumentation-compiled monitoring
  (PEP 669), lazy frame objects, per-thread data stack.
- PyPy: guard-based list strategies (homogeneous int/float lists) —
  the WS5 lane is the bounded, helper-backed cousin.
- V8/JSC: polymorphic inline caches keyed by map/shape pointer —
  WS4's pointer-identity guard is the monomorphic form.

## Unresolved questions

- Should fused arms cover `LOAD_DEREF`-fed pairs (closure-heavy code
  like `deltablue`)? Deferred until the stats counters say the
  `LOAD_FAST` family is saturated.
- Whether the suspect ring should be per-thread rather than
  per-interpreter once free-threading lands (same deferral as every
  RFC since 0058).
- Whether `PinnedObj` slots should generalize to tuples/strings in a
  follow-up lane, or whether attribute lanes (via WS4's pointer
  guards burned into JIT entry guards) are the better next JIT
  increment — decide on post-wave profiles.

## Future work

- Attribute lanes in tier-2 (the WS4 pointer guards are the
  prerequisite this wave lands).
- `list.append` / `len` in the pinned-list lane; tuple/str lanes.
- The tagged-value model, revisited with free-threading.
- Windows/Linux measured bench baselines (the harness runs there;
  the checked-in ratios are macOS-arm64).
- Memory-ratio gating (two waves of `max_rss_bytes` history now
  exist — flip the reported column to a gated one next wave).

# RFC 0065: Performance wave 4 — the quiet loop, cell fast paths, and locals slot discipline

- **Status**: Accepted
- **Authors**: WeavePy authors
- **Created**: 2026-08-12
- **Tracking issue**: TBD
- **Builds on**: RFC 0061 (fused dispatch, pointer-identity IC guards,
  frame pools, the pinned-list JIT lane, and the four named hot-path
  residuals this wave burns), RFC 0059 (the unified eval-breaker word
  WS1 generalizes into a loop generation), RFC 0058 (the bench lane and
  the `GilCell` fast-path design WS2 restores), RFC 0032/0059/0061
  (the tier-2 JIT WS5 extends), RFC 0049/0057/0060 (the conformance
  baseline as no-regression guard), RFC 0055/0056/0060 (the ecosystem
  lane, same role).

## Summary

Three perf waves moved the bench-suite geomean from 11.64× CPython to
the committed 8.64× (`baselines/bench-macos-aarch64.json`), roughly
15% each, by deleting per-instruction dead weight one profile line at
a time. This wave attacks the two structural taxes those waves kept
deferring — now with the profile evidence that the cheaper levers are
spent:

1. **The dispatch loop's prologue still runs in full before every
   instruction.** RFC 0059 unified six pending-work probes into one
   `hot_gates` word, but the loop iteration in
   `run_until_yield_or_return_impl` still performs, per bytecode
   instruction: the GIL countdown, the `hot_gates` load, *two*
   `has_any_finalizable()` probes, a `has_suspects()` probe (two more
   loads), an unconditional `shell.lasti` atomic store, a
   `has_materialized` load, the `ObserverSnapshot` generation check,
   a `fuse_off` store, the drop-watch stack-depth bookkeeping, and
   the async-exc/finalizing bit tests — ten-plus loads/stores and as
   many branches before `step` decodes a single opcode. RFC 0061's
   profile put the prologue at **17–18% of self time** with all of it
   answering "no work pending" on every iteration of every hot loop.
   WS1 collapses the entire prologue to *one relaxed load and compare*
   on the quiet path, by folding every input into a single **loop
   generation** that mutators bump.
2. **The `GilCell` access disciplines pay for machinery the GIL
   already provides.** Two concrete regressions/omissions, both
   measured against the documented design:
   - `GilCell::get`/`set` (`sync.rs`) — documented since RFC 0058 WS2
     as "skips guard construction and the `LIVE_CELL_GUARDS`
     bookkeeping", and described there as *the single hottest
     operation in the interpreter* — actually route through the full
     `borrow()`/`borrow_mut()` path: a lock CAS, a borrow-counter
     RMW pair, and **two thread-local accesses** per `Cell` read.
     The bodies carry a `// BISECT-B: pre-wave guard path` marker
     from the RFC 0059 wave's bisection (commit `7b8837a`) that was
     never flipped back. WS2 restores the documented fast path.
   - Every `LOAD_FAST` runs the full `GilCell` borrow protocol on the
     locals vector (`lock_acquire` CAS + borrow RMW + guard
     bookkeeping + release) to clone one slot. CPython's `GETLOCAL`
     is one indexed load off `frame->localsplus`. WS3 gives the
     dispatch loop a raw *slot base* captured once per activation, so
     the `LOAD_FAST`/`STORE_FAST` family and the RFC 0061 fused arms
     become plain indexed accesses — the borrow protocol survives
     only on the cold, shared paths (PEP 667 `f_locals` views,
     cross-frame introspection), which keep working unchanged
     because they address the same storage.
3. The wave also burns the four **named residuals** RFC 0061's
   results section left in priority order (per-drop
   `weakref_registry` counting, `GcState::handle_for` on miss paths,
   the population-gated finalizable cadence, suspects `IndexMap`
   traffic at frame exits — ~6% combined), and grows the tier-2 JIT
   by the two lanes its future-work list committed to: **`list.append`
   / `len` in the pinned-list lane** and the first **attribute lane**
   over RFC 0061's `class`-field pointer guards. Because the JIT is
   opt-in (`WEAVEPY_JIT=1`), the committed headline target is
   interpreter-only; the wave additionally makes the bench harness's
   `--jit` column a *measured, committed* number in the baseline
   (today every row records `jit: null`) so tier-2 progress is
   tracked release over release.

## Motivation

Unchanged from RFC 0058/0059/0061 and sharpened by the distribution
waves: RFC 0060 froze the conformance story (515/548 `Lib/test`,
ecosystem 29/29 including pandas/FastAPI), RFC 0062–0064 made the
artifact relocatable and native on Windows — so an 8.64× interpreted
geomean is now the single loudest gap between the README's promise
and the measured binary. The wave-over-wave trend (11.64 → 9.92 →
8.51 → 8.64 after paying down the endgame commit's regression) says
the incremental levers yield ~15% each and are flattening; the two
disciplines above are the *structural* share of the remaining tax,
and both can be changed without touching the object model, the C-API
boundary, or the PEP 667 ownership story that made the full
frame-layout rewrite too risky in three consecutive waves.

A note on the value representation, since "tagged values" have been
this project's deferred deep fix since RFC 0061: the profile does
**not** support a NaN-boxing rewrite as the next step. `Object`
already stores `None`/`Bool(i64)`/`Int(i64)`/`Float(f64)` inline —
scalar clones are register copies today. The measured clone tax is
Arc traffic on *heap* values moving locals↔stack, and the measured
access tax is borrow/guard machinery — both attacked here directly,
at a fraction of the risk. The 24-byte `Object` (fat slice pointers
in `Str`/`Bytes`/`Tuple`) is real but third-order; it stays deferred
with a sketch in Alternatives.

## CPython reference

- **Eval-breaker placement.** CPython 3.13 does *not* run pending-work
  checks before every instruction: `_Py_HandlePending` is reached via
  the `CHECK_EVAL_BREAKER()` macro, expanded only at `RESUME`,
  `JUMP_BACKWARD`, and call/return boundaries (`Python/ceval.c`,
  `Python/ceval_macros.h`). Straight-line bytecode pays nothing. WS1
  adopts the same discipline in the shape WeavePy's loop permits: the
  full prologue survives, but is entered only when a single
  generation word says one of its inputs changed — with the
  CPython-faithful exception that *while finalizable objects are
  live*, the between-bytecodes prompt-finalization probe keeps
  running per instruction, because CPython's refcount-driven
  `tp_dealloc` timing (`test_io.test_error_through_destructor`, the
  `test_dict` mid-iteration `__del__` cases) is observable per
  instruction.
- **Locals access.** CPython's `GETLOCAL(i)` / `SETLOCAL(i, v)` are
  plain array accesses on `frame->localsplus`
  (`Python/ceval_macros.h`); PEP 667 (`Objects/frameobject.c`,
  3.13's `FrameLocalsProxy`) makes `f_locals` a *view over the same
  slots*, not a copy — the exact relationship WS3 preserves: the raw
  slot base and the `GilCell`-guarded `f_locals` paths address one
  allocation, serialized by the GIL.
- **Cell reads.** The `GilCell::get` fast path corresponds to CPython
  reading a C struct field under the GIL — no per-read machinery at
  all. The restored discipline (lock word check, no guard
  bookkeeping) is the closest safe-Rust analogue.
- **Instrumentation gating.** CPython compiles instrumentation into
  the bytecode when tools attach (PEP 669,
  `Python/instrumentation.c`) so the uninstrumented loop never asks.
  WeavePy's equivalent is the RFC 0061 `ObserverSnapshot` +
  generation; WS1 folds that generation into the loop word so the
  quiet loop doesn't even perform the snapshot compare per
  instruction.
- **JIT lanes.** The attribute lane mirrors the shape guard +
  slot-index caches of CPython 3.13's `LOAD_ATTR_INSTANCE_VALUE` /
  `STORE_ATTR_INSTANCE_VALUE` specializations (PEP 659,
  `Python/specialize.c`): guard on type version, then indexed access;
  any surprise deopts to the generic path. `list.append`/`len`
  mirror `CALL_LIST_APPEND` and the `PyList_GET_SIZE` fast paths.

## Detailed design

### WS1 — The quiet loop: one generation word for the whole prologue

**New primitive** (`hot_gates.rs`): a process-global
`LOOP_GEN: AtomicU64`, bumped by every mutation that can change the
dispatch prologue's decisions:

- `hot_gates::set`/`clear` (any bit — pending finalizers, C-ext
  drops, resource warnings, async exc, signals, finalizing);
- the finalizable-population 0↔nonzero transitions
  (`GcState::finalizable_count` maintenance sites);
- the suspects-population transitions (`publish_suspect_counts`);
- `bump_observer_gen()` (settrace/setprofile/monitoring/tool
  mutation — the existing RFC 0061 generation folds in);
- **frame materialization** (`FrameShell::has_materialized`'s
  `false→true` store, plus PyFrame `f_trace`* mutations) — so the
  quiet loop may cache "nobody is watching `lasti`" per snapshot
  rather than re-loading the shell flag per instruction.

The dispatch loop keeps a stack-local `LoopSnapshot { gen: u64,
quiet: bool, watched: bool, obs: ObserverSnapshot }`. Per iteration:

```text
countdown -= 1; if 0 { lasti sync; yield_checkpoint }
if LOOP_GEN.load(Relaxed) != snap.gen { resnapshot(); }   // cold
if snap.quiet { step(frame); continue; }                  // hot path
… existing full prologue, unchanged …
```

`resnapshot()` re-derives: `quiet = hot == 0 && !any_finalizable &&
!has_suspects && !obs.any && !watched`, refreshes the observer
snapshot, and re-computes `fuse_off`. The full prologue is preserved
verbatim as the non-quiet arm — every existing semantic (prompt
finalization ordering, C-ext drop draining, line/opcode/monitoring
event fidelity, async-exc delivery, pending-jump handling, daemon
unwind) is *entered* exactly as today whenever its subsystem is
live, because its subsystem's mutation bumped the generation.

Correctness argument, per input:

- **A spurious generation bump is always safe** (one cold
  resnapshot) — same discipline as the RFC 0059 hot-gates word.
- **A missed bump is never allowed**: each input's mutation site
  already owns a synchronization point (the hot-gates producers, the
  GC index borrows, the observer-table borrows, the shell's atomic
  store), and the bump is placed inside those existing critical
  sections. Cross-thread visibility rides the GIL hand-off exactly
  as `hot_gates` does today (worst case: a few instructions of
  latency, the same contract the granular gates had).
- **`lasti`**: today the loop stores `shell.lasti` unconditionally
  per instruction; the quiet loop stores it (a) whenever
  `snap.watched` (a materialized frame or live tracing — same
  precision as today's `has_materialized` re-sync), (b) at every
  countdown checkpoint *before* a possible GIL hand-off, and (c) at
  `resnapshot()`. Same-thread materialization mid-instruction
  (`sys._getframe()`) reads the *live* `frame.pc` at materialization
  time (the materialization sites take `&Frame` context today or are
  reached from `step` where the pc is current), then bumps the
  generation so subsequent instructions re-sync eagerly. Cross-thread
  readers (`sys._current_frames`, faulthandler) hold the GIL, which
  this thread only releases at checkpoints — where (b) has already
  synced.
- **Prompt-finalization timing** is unchanged *by construction*:
  whenever `any_finalizable || has_suspects`, `quiet` is false and
  the loop runs today's per-instruction probe (including the
  `take_maybe_dead` gate and the drop-watch bookkeeping).

Escape hatch: `WEAVEPY_NO_QUIET=1` forces `quiet = false` at every
resnapshot (the full prologue runs per instruction, exactly today's
loop) for bisection, mirroring `WEAVEPY_NO_FUSE`.

### WS2 — `GilCell` fast paths: restore the documented discipline

`GilCell::get` / `GilCell::set` (`T: Copy`) become what their doc
comments have promised since RFC 0058 WS2:

```rust
pub fn get(&self) -> T {
    self.lock_acquire();
    // SAFETY: lock held → no cross-thread access; `T: Copy` read
    // cannot re-enter Python, so no hand-off can occur mid-read.
    let v = unsafe { *self.data.get() };
    self.lock_release();
    v
}
```

and the symmetric `set`. What this deletes per access: the
borrow-counter `fetch_add`/`fetch_sub` pair, guard construction, and
the two `LIVE_CELL_GUARDS` thread-local touches. What it keeps: the
reentrant owner lock (one relaxed load + branch on same-thread
reentry; one CAS on fresh acquire), i.e. cross-thread exclusion and
torn-read protection. The `BISECT-B` markers are retired. Semantic
delta vs `*self.borrow()`: a `get` during a live same-thread
`borrow_mut` no longer panics — for `Copy` payloads this matches
`std::cell::Cell` semantics (which never panics) and is the behavior
the rest of the codebase was written against when the fast path
first shipped.

The full `borrow()`/`borrow_mut()` protocol (and its GIL hand-off
refusal bookkeeping) is untouched — it exists for guards that
outlive re-entrant calls, which a `Copy` get/set cannot.

### WS3 — Locals slot discipline: raw base pointer per activation

`Frame` gains a cached raw view of its locals storage:

```rust
struct Frame {
    // …
    locals: Rc<GilCell<Vec<Object>>>,   // unchanged: ownership + cold paths
    locals_base: *mut Object,           // slot 0 of the same Vec
    locals_len: u32,
}
```

captured once at activation (and re-captured at the — cold, and
possibly nonexistent — sites that could ever reallocate the vector;
an audit + `debug_assert` enforces that the locals `Vec` length is
fixed at call setup, which the pool discipline already guarantees).
The hot accessors become:

- `LOAD_FAST` / `LOAD_FAST_CHECK`: bounds-check against
  `locals_len`, then `unsafe { (*base.add(i)).clone() }` — one
  indexed load + the value clone; the borrow protocol disappears.
- `STORE_FAST`: `mem::replace` through the slot, dropping the old
  value after the write (preserving today's drop ordering).
- `DELETE_FAST` / `LOAD_FAST_AND_CLEAR`: slot swap with `Unbound`.
- The RFC 0061 fused arms (`FuseLoadFastLoadFast`,
  `FuseLoadFastLoadAttr`, …) and the JIT embedder's
  `unpack`/`pack` shuttles read through the same base.

Soundness (the PEP 667 story): the raw base and the
`GilCell`-guarded paths address the *same allocation*; every access
from either side happens under the GIL, and the interpreter performs
its raw accesses only between borrow scopes (an instruction that
takes a real borrow — `f_locals` materialization, generator state
capture — does so via today's `GilCell` API and never overlaps the
single-expression raw accesses). No reference derived from the raw
base outlives the accessor expression. The `Vec` never grows during
an activation (locals arity is `co_nlocals`-fixed at setup; the
audit in this WS proves it and converts any violator to the slow
path). Generators: the `Frame` moves between resumes, but `Rc`
keeps the heap `Vec` allocation stable — the base is re-captured at
resume entry, alongside the existing `FrameShell` re-wiring.

Escape hatch: `WEAVEPY_NO_LOCALS_FAST=1` keeps the borrow-protocol
accessors for bisection.

### WS4 — Dormant-tax burn-down, round 3

The four residuals RFC 0061 measured (~6% combined), with the fix
shapes; each is validated against a fresh profile before/after and
falls back to "leave it, document why" if the profile disagrees:

1. **Per-drop `weakref_registry` counting (~2%)**: the drop-site
   probes call `count`/`strong_clone_count`, each a thread-local
   walk + `GilCell` borrow even when the bloom filter misses. Fix:
   a process-global atomic population count (registrations /
   removals maintain it) consulted *before* any TLS or borrow, so
   the no-weakrefs-anywhere program never leaves the atomic.
2. **`GcState::handle_for` on usually-miss paths (~1.5%)**: guard
   the index borrow behind an address-keyed bloom (one atomic load
   + hash) maintained at track/untrack, so untracked-object probes
   don't take the registry borrow.
3. **Finalizable-cadence shape gating (~1%)**: `has_any_finalizable`
   is population-gated and any live unfinished generator sets it,
   putting every generator workload on the non-quiet loop. Split
   the count: `__del__`-bearing instances and *suspended generators
   whose death is externally observable* keep the per-instruction
   probe; the common transient generator (created, driven,
   exhausted in a `for` loop) is exact-finished at `END_FOR` /
   `StopIteration` and never enrolls.
4. **Suspects `IndexMap` traffic at frame exits (~1.5%)**: exits
   currently pay `remove_suspect` map locks in lock-step with
   `untrack_id`. Fix: tombstone removals (the sweep drops entries
   whose handle is dead) so untrack skips the map lock entirely;
   the map is bounded (`SUSPECT_CAP`) so tombstones cannot grow it.

### WS5 — Tier-2 lanes: list `append`/`len`, and the attribute lane

Extending the RFC 0061 WS5 pinned-list ABI, same philosophy: helpers
re-validate per access; the JIT never owns the object model.

- **`wpjit_list_append(frame, pin, val_bits, val_tag) -> status`**:
  appends a lane-matching scalar to the pinned list (revalidating
  shape identically to `wpjit_list_get`); any surprise (lane change,
  frozen borrow, non-list) deopts. `wpjit_list_len(frame, pin)`
  stages the length through `ret_bits`. `analyze.rs` recognizes
  `x.append(v)` as a method-call pattern on a pinned local and
  `len(x)` as a `LOAD_GLOBAL len` call with the existing
  global-guard snapshot.
- **The attribute lane**: for a local whose entering value is an
  instance with the RFC 0061 pointer-guard shape (stable
  `class`-field pointer + `attr_version`), the entry guard pins the
  receiver (`SlotTag::ObjPin`, generalizing `ListPin`), and
  `LOAD_ATTR`/`STORE_ATTR` on it lower to
  `wpjit_attr_get/set(frame, pin, key_idx) -> status` helpers that
  perform the *IC-hit path only* (pointer + version compare, dict
  slot access); every miss — descriptor, `__getattr__`, dict shape
  change, version bump — deopts. Attribute values flow through the
  existing scalar lanes when the observed lane is scalar, and
  through `Boxed` spill otherwise.
- **Measured `--jit` column**: `weavepy-bench run` measures the
  `WEAVEPY_JIT=1` ratio per fixture into `bench.json` (today
  `jit: null`), and `gate` reports (does not yet fail on) JIT-column
  regressions. Flipping the JIT default stays future work — gated
  on a full regrtest sweep under `WEAVEPY_JIT=1`, which this wave
  runs and records as an advisory lane so the flip has a measured
  starting point.

### WS6 — Measurement and gating

- Pre-wave baseline re-measured on the dev machine (same
  back-to-back binary methodology as RFC 0061) before any change;
  every WS lands with its own before/after fixture table in the
  Results section.
- `baselines/bench-macos-aarch64.json` refreshed at landing;
  CI `gate --pct=25` unchanged (it ratchets automatically via the
  committed baseline).
- Conformance guards, all blocking: `cargo test --workspace`,
  `cargo fmt`/`clippy -D warnings`, the regrtest sweep at
  `unexpected 0` against `tests/regrtest/expectations.toml`, and the
  ecosystem lane 29/29 (offline wheel cache).

## Acceptance criteria

1. **Interpreted geomean ≤ 7.0×** CPython on the committed
   20-fixture suite (from 8.64×; ≥ 19% wall-clock cut at the
   geomean), measured and committed per the RFC 0058 methodology.
   Stretch (non-blocking): ≤ 6.0×.
2. **No fixture regresses** beyond noise (>5%) against the pre-wave
   baseline.
3. The `--jit` column is measured (no `jit: null` rows) and the
   pinned-list `append`/`len` + attribute lanes compile, run
   natively, and deopt correctly under new unit tests.
4. Regrtest sweep `unexpected 0`; ecosystem lane 29/29; workspace
   tests, fmt, clippy green.
5. Every WS carries its escape hatch (`WEAVEPY_NO_QUIET`,
   `WEAVEPY_NO_LOCALS_FAST`, existing `WEAVEPY_NO_FUSE`) for
   bisection.

## Drawbacks

- WS1 adds a second loop mode to the project's most complex
  function. Mitigations: the non-quiet arm *is* today's loop
  verbatim; the quiet arm is entered only through a single predicate
  with an escape hatch; and the generation-bump discipline reuses
  the audited hot-gates producer/consumer contract.
- WS3 introduces raw-pointer accessors into safe-enum territory.
  The unsafety is confined to two accessor functions with a
  documented aliasing contract, a fixed-length audit, and a
  `debug_assert` net; the GilCell paths remain the API for every
  cold consumer.
- WS2's `get`-during-`borrow_mut` no longer panics; a latent bug
  that relied on that panic to surface would go quiet. (Mitigation:
  debug builds keep the checked path via `debug_assertions`.)
- The attribute lane grows the deopt metadata again (`ObjPin`), and
  pin-table rebuild cost on deopt rises with pinned receivers.

## Alternatives

- **NaN-boxed / tagged `Object`**: rejected this wave on profile
  evidence (scalars are already inline; the measured tax is borrow
  discipline and Arc traffic on heap values, both addressed here).
  The remaining case — shrinking `Object` from 24 to 16 bytes by
  thinning the four fat-pointer variants — is mechanical but touches
  every `Object::Str(...)` match in ~95K lines; deferred until the
  disciplines above are spent, with the note that it composes with
  (rather than replaces) this wave.
- **Contiguous frame + data-stack rewrite**: rejected a fourth time,
  same grounds as RFC 0061 — WS3 removes the measured share of the
  locals tax (the access protocol) without touching the ownership
  model that PEP 667 and generators depend on.
- **Compile eval-breaker checks into the bytecode** (CPython's
  literal placement: checks only at `RESUME`/`JUMP_BACKWARD`):
  rejected — it would perturb the RFC 0033 `co_code` re-encoding
  contract and the trace-event exactness work of RFC 0057 for the
  same effect the loop generation achieves invisibly.
- **Flipping `WEAVEPY_JIT` on by default this wave**: rejected —
  the correctness bar for a default flip is the full sweep under
  JIT, which has never been run; this wave produces that measurement
  as an advisory lane instead of gambling the drop-in story on it.

## Prior art

- CPython 3.11–3.13: `CHECK_EVAL_BREAKER` placement,
  `frame->localsplus` flat locals, PEP 659 specialized attribute
  opcodes guarded by `tp_version_tag`, PEP 669 compiled-in
  instrumentation.
- PyPy: guard-based attribute maps (the attribute lane is the
  bounded, helper-backed cousin); list strategies (`append`
  preserves the strategy exactly as the lane's revalidation does).
- V8/JSC: inline caches keyed by hidden class + slot offset — the
  `wpjit_attr_*` helpers are the monomorphic form with out-of-line
  re-validation.

## Unresolved questions

- Should the loop generation live in the same cache line as
  `hot_gates::HOT` (one load for both) or separately (independent
  invalidation)? Decide by measurement in WS1.
- Does the finalizable shape-split (WS4.3) need a per-generator
  "observably finalizable" bit at creation (cheap) or at first
  suspension (precise)? Decide against the generator regrtests.
- Whether `ObjPin` should admit *megamorphic* fallback entries
  (pointer-guard per call site rather than per entry) — deferred to
  the post-wave profile, as RFC 0061 deferred the same question for
  `LOAD_DEREF` fusion.

## Future work

- The `WEAVEPY_JIT=1` default flip, gated on the advisory sweep this
  wave records.
- The 16-byte `Object` (thin slice variants), composing with WS3.
- `LOAD_DEREF`-fed fused arms (deltablue's closure pattern), now
  cheaper over the WS3 slot base.
- Free-threading: the loop generation and the population-count
  disciplines are designed to survive a per-thread split (the
  generation becomes per-interpreter; the GIL-barrier visibility
  argument is replaced by the stop-the-world epoch) — revisited
  together with the tagged-value question, unchanged from RFC 0061.

## Results

Measured on macOS arm64 against host CPython 3.13, RFC 0058
methodology (symmetric subprocess harness, 5 samples, medians).
"Pre-wave" is the committed `bench-macos-aarch64.json` this wave
started from (geomean 8.64×); "after" is the refreshed committed
baseline. The committed run uses the JIT-featured binary (so the
`--jit` column lands in the same `bench.json`); the CI-shaped
non-JIT binary measured 7.26–7.43× on the same suite back to back.
One row is deliberately *not* ratcheted: deltablue improved −25% on
the dev machine (26.96× → 20.16×) but the shared `macos-latest`
runner still measures ~25.5×, so the committed row keeps main's
CI-flavored 26.96× measurement (the gate compares CI runs against
this file; committing the dev-machine number would fail every CI
leg on machine skew, not regression). The committed geomean is
therefore 7.52× — exactly what the PR's CI leg measured.

### Headline

| Metric | Pre-wave | After |
|---|---|---|
| Bench suite geomean vs CPython | 11.64× → 9.92× → 8.51× → **8.64×** (waves 1–3) | **7.52×** committed (−13% at the geomean; 7.26–7.43× measured on the dev machine) |
| `--jit` column in the committed baseline | every row `jit: null` | **every row measured** (first release-over-release tier-2 record) |
| New `jitkernels` fixture (append/len/attr kernels) | — | 306.8ms interpreted → **26.2ms** under `WEAVEPY_JIT=1` (11.7×; **0.74× CPython**, i.e. faster than CPython) |
| Numeric kernels under `WEAVEPY_JIT=1` | `jitloop` 0.06× CPython (RFC 0061) | `sumvm` **0.04×**, `nested_loops` **0.04×**, `jitloop` **0.06×** CPython |
| Regrtest sweep | unexpected 0 | **unexpected 0** (402 pass / 26 expected-fail / 6 skip of 434) |
| Advisory `WEAVEPY_JIT=1` sweep (first ever) | never run | **unexpected 0** — identical row-for-row to the interpreter sweep |
| Ecosystem lane | 31/31 | **31/31**, 0 unexpected; selftests 5 pass / 1 expected skip |
| Gates | — | `cargo fmt` / `clippy --all-features -D warnings` / `cargo test --workspace --all-targets --all-features` + doc tests (0 failures) / `bench gate --pct=25` OK |

The headline acceptance target (interpreted geomean ≤ 7.0×) was
**missed**: the wave lands at 7.52× committed (7.26× measured on the
dev machine with the CI-shaped binary), a 13% cut against the ≥ 19%
target. The shortfall is WS4.3:
the finalizable-cadence split regressed generator workloads when
combined with the WS1 quiet path (suspended generators became active
suspects, defeating the quiet predicate) and was reverted rather than
landed broken — its ~2–3% share moves to the next wave with the
lesson recorded below.

### Per-fixture committed ratios (lower is better)

| fixture | pre-wave | after | Δ | JIT median | JIT ×CPython |
|---|---|---|---|---|---|
| fannkuch | 11.72× | 8.56× | −27% | 105.2ms | 8.90× |
| nbody | 11.96× | 9.80× | −18% | 231.7ms | 9.89× |
| fib | 15.43× | 10.81× | −30% | 258.2ms | 14.59× |
| pidigits | 0.91× | 0.92× | +1% | 2.21s | 0.92× |
| pyaes | 12.53× | 11.92× | −5% | 224.2ms | 12.16× |
| richards | 19.50× | 15.71× | −19% | 227.4ms | 16.80× |
| sumvm | 4.26× | 3.51× | −18% | 1.34ms | **0.04×** |
| nested_loops | 5.59× | 4.60× | −18% | 2.20ms | **0.04×** |
| jitloop | 4.64× | 3.80× | −18% | 3.62ms | **0.06×** |
| jitkernels | *(new)* | 8.67× | new | 26.2ms | **0.74×** |
| deltablue | 26.96× | 26.96× *(kept, see above)* | −25% dev-machine only | 1.02s | 21.07× |
| float_math | 15.35× | 14.39× | −6% | 579.7ms | 14.64× |
| spectral_norm | 10.69× | 8.36× | −22% | 271.5ms | 8.79× |
| json_bench | 5.22× | 5.25× | +1% | 228.1ms | 5.25× |
| str_methods | 6.34× | 6.31× | −0% | 200.6ms | 6.32× |
| dict_ops | 6.64× | 6.12× | −8% | 204.9ms | 6.19× |
| list_ops | 13.81× | 13.60× | −2% | 372.4ms | 14.28× |
| attr_access | 13.87× | 11.16× | −20% | 332.0ms | 11.68× |
| call_overhead | 14.50× | 11.37× | −22% | 528.8ms | 11.79× |
| generators | 15.28× | 13.94× | −9% | 441.4ms | 14.66× |
| startup | 2.44× | 1.90× | −22% | 34.6ms | 1.93× |

No fixture regressed beyond noise (worst: json_bench +1%,
pidigits +1%). The biggest wins cluster exactly where the wave
aimed: call-heavy (`fib` −30%, `call_overhead` −22%, deltablue −25%
on the dev machine) and attribute/local-heavy (`attr_access` −20%,
`fannkuch` −27%) fixtures, which is the prologue + locals + GilCell
tax coming off every interpreted instruction.

One honest JIT-column soft spot: `fib` runs **+35% slower** under
`WEAVEPY_JIT=1` (258ms vs 191ms interpreted). The self-recursive
kernel tiers up but every recursive call re-enters through the
marshal/entry-guard boundary, which costs more than the interpreter's
pooled call path at fib's tiny per-call work. Native-to-native calls
without re-marshalling are the obvious next lane; the column exists
precisely to keep this number visible.

### Workstream outcomes

| WS | Deliverable | Result |
|---|---|---|
| WS1 | Quiet-loop dispatch | One loop-generation word (`hot_gates::LOOP_GEN`) folds the GIL countdown, finalizable/suspect probes, observer generation, and drop-watch bookkeeping into a single relaxed load+compare on the quiet path; mutators bump the generation; `WEAVEPY_NO_QUIET=1` escape hatch |
| WS2 | `GilCell` fast paths | `get`/`set` for `Copy` payloads restored to the RFC 0058-documented discipline (no guard construction, no `LIVE_CELL_GUARDS` traffic); checked path retained under `debug_assertions` |
| WS3 | Locals slot discipline | Raw slot base captured once per activation; `LOAD_FAST`/`STORE_FAST` family and fused arms are plain indexed accesses; PEP 667 / cross-frame paths unchanged on the `GilCell` protocol; `WEAVEPY_NO_LOCALS_FAST=1` escape hatch |
| WS4 | Dormant-tax burn-down | Insert-only `AtomicBloom` pre-filters (`hot_filter.rs`) in front of the `weakref_registry` per-drop count and `GcState::handle_for` miss paths. Item 3 (finalizable-cadence split) implemented, measured as a regression in combination with WS1 (suspended generators re-enter the suspect set and defeat the quiet predicate), and **reverted** — carried to the next wave |
| WS5 | Tier-2 list/attr lanes | `len(xs)` (`ResolvedGlobal::LenBuiltin` burn-in → `ListLen`), `xs.append(v)` (receiver-marked method pattern → `ListAppend`, empty lists lane-pinned by the append value), and the first attribute lane (`SlotTag::ObjPin`, per-site `AttrGuard`: class identity + `attr_version` + indexed dict hit with name re-check, scalar value lanes); deopt reconstruction via `len`/method spans (erased `len` and bound `append` re-inserted at recorded depths); 7 new `jit_*` end-to-end VM tests incl. dict-reshape and class-mutation deopts; `jitkernels` fixture; `--jit` column measured and committed |

Implementation deltas from the design sketch, recorded for the next
reader: the append/attr helpers stage the value through
`JitFrame::ret_bits` per the pin's lane instead of the sketched
`(val_bits, val_tag)` arguments (one fewer ABI register, same
guards); the attribute lane admits *scalar* value lanes only — a
non-scalar attribute keeps the frame interpreted rather than
spilling `Boxed`, deferred until a profile demands it; and
`wpjit_attr_set` additionally deopts when the *displaced* value is a
heap object, so drop-site semantics (prompt reap, parked finalizers)
never run outside the interpreter.

### Acceptance checklist

1. Interpreted geomean ≤ 7.0×: **missed** — 7.52× committed
   (7.26× dev-machine, CI-shaped binary), −13% vs the −19% target;
   WS4.3's reverted share is the identified gap.
2. No fixture regresses > 5%: **met** (worst +1%).
3. `--jit` column measured, no `jit: null` rows; append/len +
   attribute lanes compile, run natively, and deopt correctly under
   new unit tests: **met** (31 `jit_*` VM tests green, 27 analyzer
   tests green).
4. Regrtest `unexpected 0`; ecosystem 31/31; workspace tests, fmt,
   clippy green: **met** (one `test_venv` flake in an early
   contended sweep passed 3/3 standalone and the final sweep is
   clean).
5. Escape hatches per WS: **met** (`WEAVEPY_NO_QUIET`,
   `WEAVEPY_NO_LOCALS_FAST`, existing `WEAVEPY_NO_FUSE`).

The advisory `WEAVEPY_JIT=1` regrtest sweep — the gate this RFC set
for ever flipping the JIT default — exists now and reads
**unexpected 0**, row-for-row identical to the interpreter sweep,
with the new lanes exercised across the full 434-test corpus.

# RFC 0077: The floor-and-switch wave: performance wave 12 (the tier-1 interpreter floor) and the CPython 3.14 target switch

- **Status**: Draft
- **Authors**: WeavePy authors
- **Created**: 2026-09-01
- **Tracking issue**: TBD
- **Builds on**: RFC 0076 (whose version policy commits this switch and
  whose wave-11 honest miss this wave's census explains), RFC 0074/0073
  (the frame-coverage and lane work whose measured ceiling is the tier-1
  floor), RFC 0068/0054 (the prompt-reap discipline whose cost this wave
  bounds), RFC 0065 (the quiet-loop dispatch and the `Object` thinning it
  kept in reserve), RFC 0058 (the measured-bench protocol), RFC 0053 (the
  source-truth stdlib whose re-vendor this wave tools), RFC 0036/0049
  (the measured-baseline protocol every expectations rewrite follows),
  RFC 0062/0075 (the dist layout and embedding surface the identity flip
  renames).

## Summary

WeavePy's compatibility scoreboard reads zero on CPython 3.13 (fail 0,
unexpected 0 across 550 labels; 48 of 48 ecosystem rows), and its
performance scoreboard reads **2.91x slower than CPython** (geomean, 21
fixtures), 2.2x slower to start, and 2.5x the RSS. Eleven performance
waves have moved the geomean from 3.33x to 2.91x by adding tier-2 JIT
admission lanes. This wave opens with a measurement no prior wave
committed: the interpreter *without* the JIT. On the object-shaped
fixtures the JIT is within noise of the interpreter (and a net loss on
two), and the interpreter itself is 5x to 11x slower than CPython's.
The time isn't in frames the JIT rejects; it's in the floor every
frame runs on. The wave also executes the version switch RFC 0076
committed: CPython 3.14 is the current stable release, 3.14.7 is the
host oracle, and every package in the ecosystem matrix ships cp314
wheels.

The two pillars land in one commit, sequenced so the second can't
corrupt the first's measurement:

1. **Pillar I: performance wave 12, the interpreter floor (WS1 to
   WS7).** A profile census of the interpreter-only run (committed in
   this RFC) names six buckets that together account for roughly half
   of every sampled instruction: dispatch-loop overhead, the prompt-reap
   suspect re-probe on every reference-dropping opcode, malloc/free and
   `Object` clone/drop glue, thread-local lookups, frame-shell churn on
   calls, and uncached attribute and string-hash lookups. Each bucket
   gets a workstream with a named mechanism, plus native `_collections`
   (deque, defaultdict, OrderedDict are pure Python today) and a
   startup/RSS pass. The bench baseline gains an interpreter-only
   column so the floor is gated from now on, not just the JIT.
2. **Pillar II: the CPython 3.14 switch (WS8 to WS14).** The
   `docs/PY314-GAP.md` charter executed in its measured order: a
   stdlib re-vendor tool that classifies every bundled file (319
   verbatim, 46 patched, 213 WeavePy-authored) and merges 3.14.7 over
   it; the bytecode and magic flip (3627, the +15/-11 opcode delta,
   `LOAD_SPECIAL`/`LOAD_COMMON_CONSTANT`/`LOAD_SMALL_INT`/`NOT_TAKEN`/
   `POP_ITER` codegen, `BINARY_OP NB_SUBSCR`); PEP 649/749 deferred
   annotations with `annotationlib`; the asyncio policy split, PEP 734
   `concurrent.interpreters`, `_py_warnings`, `_colorize`, PEP 768's
   `sys.remote_exec` surface; the 3.14 C-API delta and header
   re-vendor; and the identity flip (`sys.version` 3.14.7, cp314,
   `weavepy3.14`, `libpython3.14`, `python314.dll`), with t-strings and
   PEP 758 default-on and `-X lang=next` deleted.

As always the deliverable is measured: Pillar I is checkpointed
against the zeroed 3.13 sweep *before* the flip (recorded below), the
3.14 sweep becomes the committed regrtest baseline with every red row
carrying a measured reason, the ecosystem lane re-runs on cp314 wheels,
and the bench baseline is re-committed from fresh runs with the new
interpreter-only column.

## Motivation

1. **The performance thesis is unmet on all three axes.** The README
   promises dramatically better execution speed, startup, and memory;
   the committed baseline says 2.91x slower, 2.19x slower, 2.5x more.
   A drop-in nobody drops in because it's five times slower isn't
   finished, whichever version it targets.
2. **The perf strategy has been aimed at the wrong tier.** Measured
   this wave at baseline work sizes (macOS arm64, release build,
   CPython 3.14.7 host):

   | fixture | CPython | WeavePy JIT on | WeavePy `WEAVEPY_JIT=0` |
   |---|---|---|---|
   | deltablue | 0.19 s | 2.17 s | 2.02 s |
   | list_ops | 0.12 s | 0.69 s | 0.80 s |
   | dict_ops | 0.12 s | 0.53 s | 0.48 s |
   | pyaes | 0.11 s | 0.55 s | 0.60 s |
   | call_overhead | 0.14 s | 0.83 s | 1.23 s |

   The committed baseline stores `"interp": null` on every row; the
   interpreter has never been gated. Waves 10 and 11 both closed with
   "admission != win": the lanes compiled the frames they chartered,
   and the fixtures didn't move, because the callee bodies and the
   generic round-trips between them run on the tier-1 floor. Lowering
   the floor is also the cheapest JIT work available: every generic
   `CallDyn` round-trip, every deopt, and every non-compiled callee
   pays the same floor.
3. **The census is specific.** Sampling the interpreter thread with
   `sample(1)` on four fixtures (deltablue, list_ops, call_overhead,
   pyaes; roughly 3,200 samples each) gives a consistent flat profile:

   | bucket | deltablue | list_ops | call_overhead | pyaes | mechanism |
   |---|---|---|---|---|---|
   | `run_until_yield_or_return_impl` + `step` | 18% | 24% | 31% | 21% | out-of-line `step` per instruction, fat match |
   | suspect re-probe (`take_dead_suspects`, `IndexMap::retain`, `reap_dead_finalizable_locked`, `handle_for`) | 9% | 7% | 6% | 8% | global `Mutex<IndexMap>` walked at every reference-dropping opcode while any active suspect is enrolled |
   | malloc/free | 8% | 10% | 6% | 11% | `Vec<Object>::push` growth, `BoundMethod` boxes, scratch |
   | `Object` clone/drop glue | 3% | 7% | 5% | 9% | 24-byte enum, out-of-line glue, `Arc` RMW |
   | `_tlv_get_addr` (thread-local access) | 5% | 1% | 5% | 1% | `builtin_types()`, thread-id, `MAYBE_DEAD`, JIT state per instruction |
   | frame shells (`push/recycle_frame_shell`, `recycle_frame_allocs`, `pooled_locals_from_args`) | 8% | 1% | 12% | 1% | eager Python-visible frame twin per call |
   | attribute lookup (`TypeObject::lookup`, `load_attr_instance_default`, `descriptor_get`, `memcmp`) | 10% | 2% | 3% | 1% | MRO walk with string compares, no per-type method cache |
   | str hashing (`py_str_hash`, `py_hash_bytes_slice`) | 2% | 1% | 1% | 1% | `Str(Rc<str>)` carries no cached hash |

   list_ops also samples `__findenv_locked`: a `getenv` is still on a
   hot path after RFC 0076's `OnceLock` sweep (`types.rs:1402`,
   `object.rs:8509`, `object.rs:9818`, `gc_trace.rs:693`, `:1498`).
   Atomics are *not* a first-order bucket: `GilCell::borrow` is 10
   samples in 3,036, which retires the biased-refcounting hypothesis
   from the pre-wave analysis. The census, not the hypothesis, is the
   charter.
4. **The version policy says the switch is due now.** RFC 0076 adopted
   a fixed trigger (CPython N.1 shipped and the matrix's cp31N wheels
   exist) and declared the 3.14 switch the committed next wave. Both
   conditions hold: the host oracle is 3.14.7, and every ecosystem row
   publishes cp314 wheels. `docs/PY314-GAP.md` measured 177 of 467
   3.14 labels passing on the 3.13-target engine, with 129 of the 287
   reds mechanical (support-drift and the six `unittest` asserts).
5. **Why one landing, and why this order.** RFC 0076 argued that a
   version flip in the same window as deep engine work makes a
   3.14-delta failure indistinguishable from a regression. The
   argument is right about *measurement*, not about *commits*: this
   wave lands Pillar I first and checkpoints it against the zeroed
   3.13 sweep and the bench gate (Results section) before any 3.14
   change touches the tree, then flips. The floor work is the riskiest
   edit the codebase can take; the right time for it is while the
   oracle is at zero. The switch then re-zeroes the scoreboard on 3.14
   on the faster engine, which is also what un-sticks the budget-bound
   numpy selftest shards and the 2400 s ecosystem budgets.
6. **Cost of inaction.** Every future JIT wave inherits the floor; the
   3.15 trigger arrives in early 2027 and stacks a second version
   delta on the first; and the README's headline stays false.

## CPython reference

- **`Python/ceval.c` and `Python/bytecodes.c` (3.13/3.14)**: the
  computed-goto dispatch, `_PyFrame_*` (the lightweight interpreter
  frame) versus `PyFrameObject` (materialized only when observed:
  `_PyFrame_GetFrameObject`), `LOAD_FAST` as a pointer copy plus
  incref, and the specializing interpreter's inline-cache families
  (`LOAD_ATTR_METHOD_WITH_VALUES`, `CALL_PY_EXACT_ARGS`,
  `CALL_BOUND_METHOD_EXACT_ARGS`, `BINARY_SUBSCR_LIST_INT`,
  `FOR_ITER_LIST`, `UNPACK_SEQUENCE_TWO_TUPLE`).
- **`Objects/typeobject.c::_PyType_Lookup`**: the process-wide method
  cache keyed by `(tp_version_tag, name)` with interned-name pointer
  identity (`MCACHE_SIZE_EXP`), the structural answer to WeavePy's
  MRO walk with `memcmp`.
- **`Objects/unicodeobject.c`**: `hash` cached in the object header
  (`-1` = not yet computed); `PyUnicode_InternInPlace`.
- **`Modules/_collectionsmodule.c`**: `deque` (block-linked ring,
  `maxlen`, `rotate`, `__reduce__`), `defaultdict`, `_count_elements`,
  `_tuplegetter`; `Lib/test/test_deque.py`, `test_defaultdict.py`,
  `test_ordered_dict.py` (the C `OrderedDict` lives in
  `Objects/odictobject.c`).
- **`Objects/dictobject.c`**: compact ordered dict, split keys for
  instances (`PyDictKeysObject` shared per class), `ma_version_tag`
  (3.13; removed in 3.14 along with `test_dict_version`).
- **Prompt finalization**: CPython's refcounting frees temporaries the
  instant the last reference dies; WeavePy's RFC 0054/0068 suspect and
  `prompt_reap` machinery approximates that timing. The observable
  contract is `test_io.test_error_through_destructor`, `test_ssl`'s
  weakref leak tests, `test_asyncio` SSL-context reaping, and numpy's
  `test_arrayprint._recursive_guard`; this wave keeps the contract and
  bounds the cost.
- **CPython 3.14 (vendored at `vendor/cpython314/Lib`, tag v3.14.7)**:
  the switch's authoritative test surface. `Lib/_opcode_metadata.py`
  (the opcode table: `LOAD_SMALL_INT`, `LOAD_FAST_BORROW`,
  `LOAD_FAST_BORROW_LOAD_FAST_BORROW`, `LOAD_SPECIAL`,
  `LOAD_COMMON_CONSTANT`, `NOT_TAKEN`, `POP_ITER`,
  `BUILD_TEMPLATE`/`BUILD_INTERPOLATION`, `ANNOTATIONS_PLACEHOLDER`;
  removals `BEFORE_WITH`, `BEFORE_ASYNC_WITH`, `BINARY_SUBSCR`,
  `BUILD_CONST_KEY_MAP`, `LOAD_ASSERTION_ERROR`, `RETURN_CONST`,
  `LOAD_METHOD`, `LOAD_SUPER_METHOD`, `LOAD_ZERO_SUPER_*`),
  `Include/internal/pycore_magic_number.h` (3627),
  `Python/codegen.c` (`with` via `LOAD_SPECIAL __enter__/__exit__`,
  `assert` via `LOAD_COMMON_CONSTANT`, `codegen_annotations` emitting
  `__annotate__`), `Python/flowgraph.c` (the `LOAD_FAST_BORROW`
  liveness pass and `NOT_TAKEN` insertion).
- **PEP 649 / PEP 749** (deferred evaluation of annotations):
  `__annotate__(format)` on functions, classes, and modules; the
  `Format` enum (`VALUE=1`, `VALUE_WITH_FAKE_GLOBALS=2`,
  `FORWARDREF=3`, `STRING=4`); compiler-generated annotate functions
  raise `NotImplementedError` for formats other than 1 and 2;
  `annotationlib.get_annotations`, `call_annotate_function`,
  `ForwardRef`, `Stringifier`; `type.__annotations__` and
  `type.__annotate__` as data descriptors with `__annotations_cache__`;
  `__conditional_annotations__` for annotations under `if`/`for`;
  `from __future__ import annotations` still stringizes eagerly.
  `Lib/test/test_annotationlib.py`, `test_type_annotations.py`, the
  3.14 `test_typing`/`test_dataclasses`/`test_inspect` shares.
- **PEP 734** (`concurrent.interpreters`, 3.14): `Interpreter`,
  `create()`, `list_all()`, `Queue`, `is_shareable`; over
  `_interpreters`/`_interpchannels`/`_interpqueues`.
  `test_interpreters/`, `test__interpreters`, `test_crossinterp`.
- **PEP 750 / PEP 758**: default-on in 3.14; `test_tstring`,
  `test_grammar`, `test_syntax`.
- **PEP 768** (`sys.remote_exec`, 3.14): `sys.is_remote_debug_enabled()`
  and the `remote_exec(pid, script)` surface; a build without remote
  debugging (`--without-remote-debug`) reports `False` and
  `test_remote_pdb` skips. WeavePy adopts the without-remote-debug
  posture.
- **3.14 stdlib deltas** measured in `docs/PY314-GAP.md`: the asyncio
  policy deprecation split (`_DefaultEventLoopPolicy`,
  `_set_event_loop_policy`, `asyncio.tools`), `_py_warnings` plus
  `sys.flags.context_aware_warnings`/`thread_inherit_context`,
  `_colorize.Theme`, `http.server.HTTPSServer`, `annotationlib`,
  `string` as a package, `compression` (landed in RFC 0076),
  `_compression` removal, `unittest`'s six assert methods.
- **3.14 C-API** (`Doc/whatsnew/3.14.rst`, `Include/`): `PyUnicodeWriter_*`,
  `PyLong_FromInt32/64`, `PyLong_AsInt32/64`, `PyLong_AsUInt32/64`,
  `PyLong_IsPositive/IsNegative/IsZero`, `PyLong_GetSign`,
  `PyIter_NextItem`, `Py_HashBuffer`, `PyBytes_Join`,
  `PyList_Extend/Clear`, `PyDict_Pop/PopString`,
  `PyImport_ImportModuleAttr[String]`, `PyType_GetBaseByToken` and
  `Py_tp_token`, `PyUnicode_Equal`, `Py_fopen/Py_fclose`,
  `Py_PACK_VERSION`/`Py_PACK_FULL_VERSION`, `PyUnstable_Object_IsUniqueReferencedTemporary`,
  `PyUnstable_Object_EnableDeferredRefcount`, `PyUnstable_IsImmortal`,
  `PyUnstable_TryIncRef`/`EnableTryIncRef`, `PyMonitoring_*`,
  `PyConfig_Get*` (landed in RFC 0076), `Py_mod_gil` (landed);
  `python3.14` naming (`libpython3.14`, `python314.dll`,
  `python-3.14.pc`, `.cpython-314-*.so`, `cp314`).

## Detailed design

### Sequencing inside the single landing

The commit is one; the work is two measured phases:

1. **Phase I** lands WS1 to WS7 on the 3.13 target. Checkpoint: the
   default-mode regrtest sweep (`--cpython-dir vendor/cpython/Lib/test
   --mode subprocess --jobs 8`) at `unexpected 0` against the existing
   3.13 baseline, the ecosystem lane green offline, and the bench
   gate re-measured with the new `interp` column. These numbers are
   recorded in the Results section as the *Phase I checkpoint* and
   are the wave's performance evidence; nothing after them may
   change them except the identity flip's effect on `startup`.
2. **Phase II** lands WS8 to WS14. The 3.13 baseline retires to the
   `weavepy-3.13` maintenance branch; `tests/regrtest/expectations.toml`
   is rewritten from the 3.14 sweep.

A failure in Phase II is therefore attributable: if a label that
passed at the Phase I checkpoint fails after the flip, it's a 3.14
delta or a switch bug, never a floor regression.

### Pillar I: the interpreter floor (performance wave 12)

**Affected crates**: `weavepy-vm` (`lib.rs` dispatch, `gc_trace.rs`,
`object.rs`, `types.rs`, `specialize.rs`, `sync.rs`, new
`stdlib/collections_native.rs`), `weavepy-compiler` (`bytecode.rs`
`InlineCache`, `CodeObject` stack sizing), `weavepy-bench` (the
`interp` column and gate). No bytecode-format changes visible to
`cpython_code`; no C-API changes.

#### WS1: measure the floor

- `weavepy-bench run` records a third leg per fixture, `interp`
  (`WEAVEPY_JIT=0`), alongside `weavepy` and `cpython`; the baseline
  JSON's existing `"interp": null` slot is filled and `gate` compares
  it. The gate table prints all three ratios.
- The census methodology (the `sample`-based flat profile above) is
  scripted as `weavepy-bench profile <fixture>` (macOS `sample`,
  Linux `perf record` when present) so the next wave's census is one
  command, and the four profiles above are committed under
  `crates/weavepy-bench/census/wave12/` as text.
- `WEAVEPY_VM_STATS` gains counters for the buckets this wave touches:
  suspect sweeps run / entries probed / dead found, frame shells
  materialized versus elided, method-cache hits/misses, str-hash
  cache hits/misses.

#### WS2: the drop-path tax

The prompt-reap suspects list (`gc_trace.rs::SUSPECTS`) is a global
`parking_lot::Mutex<IndexMap>` that `take_dead_suspects` locks and
walks with `retain` at every reference-dropping safe point while any
entry has probe budget left (up to 64 probes per entry, 256 entries).
`reap_dead_finalizable_locked` and `handle_for` ride the same cadence.
The fix keeps the contract (the tests named in the reference section)
and bounds the cost:

- **Per-thread active list, not a global map.** Under the GIL every
  drop safe point runs on the thread that dropped; suspects enroll
  into a thread-local `Vec<(Arc<TrackedHandle>, u8)>` (the RFC 0068
  discipline is per-thread already, `MAYBE_DEAD`). The global map
  survives only as the cross-thread fallback under `-X gil=0`,
  selected once at startup.
- **Probe by dirtiness, not by stride.** A suspect is re-probed only
  when its strong count *changed* since the last probe: the handle
  records the count observed at enrollment; `take_dead_suspects`
  compares one relaxed load per entry and skips unchanged entries
  without touching the registry. Entries whose count hasn't moved
  in 8 consecutive safe points go dormant immediately (today: 64).
- **No-op fast exit.** With zero active suspects the safe point costs
  one thread-local byte load (`MAYBE_DEAD` already exists; the
  `has_suspects` atomics are folded behind it).
- **`reap_dead_finalizable_locked`** moves from the per-drop cadence
  to the `MAYBE_DEAD && finalizable_present` gate it documents, with
  the finalizable registry's presence bit cached in the interpreter
  struct rather than re-read through the GC state lock.
- **Hot-path `getenv` sweep**: `types.rs:1402` (`WEAVEPY_REAP_TRACE`),
  `object.rs:8509` (`WEAVEPY_CMP_BT`), `object.rs:9818`
  (`WEAVEPY_LEN_DBG`), `gc_trace.rs:693` and `:1498` become
  `OnceLock<bool>` reads, matching RFC 0076's fix for the other two.

Acceptance: the suspect bucket falls below 1% on the four census
fixtures; `test_io.test_error_through_destructor`, the `test_ssl` leak
tests, and `test_rfc0076_burn_regressions.py` §ResourceWarning stay
green; `WEAVEPY_VM_STATS` reports the probe counts.

**As landed (measured, deltablue `WEAVEPY_BENCH_WORK=50`, interp).**
The per-thread list and the dirtiness probe were both tried and
rejected by the counters. Dirtiness ("re-probe only when the count
moved") kept ~65 entries permanently active on `deltablue`: live
objects churn their counts constantly, so "changed" is not a signal
of dying. The per-thread list buys nothing once the walk itself is
gone. What the counters actually said, and what replaced them:

| counter | before | after |
|---|---|---|
| suspect sweeps | 147K | 9.2K |
| suspect entries probed | 9.4M | 172K |
| finalizable scans | 2.62M | 4.6K |
| coarse maybe-dead marks | ~4.0M | ~80K |
| interp wall (best of 3, A/B same host) | 2.162 s | 1.797 s |

- **Suspects are bounded in time, not just in budget.** The active
  budget drops 64 to 16, and a dormant entry is forgotten after 16
  stride probes (about 1,000 drop safe points in all) or as soon as a
  probe finds it more than 3 references above its dead line. The
  dormant population was the whole cost: `deltablue`'s 191
  `Variable`s (one list reference each, alive for the whole run)
  filled the 256-entry map and were re-probed 40K times. A forgotten
  suspect's eventual death is still noticed the ordinary way, by the
  cascade through whatever holds it, or by a collection.
- **Finalizables demote on stability.** A hot entry whose strong count
  is unchanged across 64 consecutive probes goes cold regardless of
  the 16-reference margin; a cold entry stays cold until it reaches
  the borderline. `deltablue` has exactly one finalizable (the ABC
  machinery's callback-weakref'd class, three references above its
  dead line) and it was hot for all 2.6M scans. The hot/cold gate
  now runs *before* the `collecting` CAS, so the steady state is two
  relaxed loads.
- **The audited-opcode set widened** from four to thirteen. The
  per-opcode census (`WEAVEPY_VM_STATS` now prints it) showed `CALL`
  (1.46M), `RETURN_VALUE` (1.38M), `STORE_ATTR` (568K), the
  `POP_JUMP_IF_*` family (407K), and `IS_OP` (102K) driving ~95% of
  the coarse marks. Each now grades the references it actually
  releases through `note_dropped`: `reap_call_receiver`/
  `reap_call_args` for call operands (a dying bound method grades its
  receiver, not its own count of one), `reap_frame_locals_on_exit`
  for every frame slot and leftover stack operand, the `STORE_ATTR`
  instance hits for receiver and displaced value, and the handlers
  for the jump family and `IS_OP`. The paths that release values out
  of sight (a `CALL` that raises, the slot/descriptor/`__setattr__`
  store paths, a `__bool__` that raises) keep the coarse mark.
- Two stats-only diagnostics stay: the residual census at exit (type
  names and excess references of every hot finalizable and enrolled
  suspect) and the coarse-mark-by-opcode table, both under the
  existing `WEAVEPY_VM_STATS` gate.
- **The one promptness regression the sweep found, and its fix.** The
  stability demotion can make a *short-lived* finalizable cold: a
  coroutine created and passed to a call that raises is stable at its
  count for the 64 probes the exception's construction takes, and
  when its last holder (the traceback-pinned callee frame) is released
  by unittest's `assertRaises` clearing frames, the hot-set scan no
  longer sees it, so the "never awaited" warning arrived up to
  `FIN_COLD_STRIDE` safe points late, outside the `with assertWarns`
  block (`test_asyncio.test_events.test_run_until_complete_nesting`,
  flaky about half the time). `gc_trace::mark_bulk_drop` now requests
  a whole-index re-grade (a forced cold tick) at the two points where
  many references die unseen at once: a frame torn down by a
  propagating exception, and `frame.clear()`. Both are rare relative
  to drop safe points, so `deltablue`'s steady state is unchanged.
  `WEAVEPY_NO_FIN_COLD=1` remains the bisection switch.

#### WS3: the dispatch loop

- **`step` folded into the loop for the hot subset.** `Interpreter::step`
  is an out-of-line call per instruction from
  `run_until_yield_or_return_impl`. The 24 hottest opcodes (the
  `LOAD_FAST`/`STORE_FAST`/`LOAD_CONST`/`POP_TOP`/`BINARY_OP`/
  `COMPARE_OP`/`POP_JUMP_IF_*`/`JUMP_BACKWARD`/`FOR_ITER`/`LOAD_ATTR`/
  `STORE_ATTR`/`BINARY_SUBSCR`/`CALL`/`RETURN_VALUE`/`LOAD_GLOBAL`/
  `IS_OP`/`TO_BOOL`/`GET_ITER`/`BUILD_TUPLE`/`UNPACK_SEQUENCE`/
  `PUSH_NULL`/`COPY`/`SWAP`/`NOP` family) dispatch inline in the loop
  body with the specialized-cache arm first; everything else falls to
  `step`. Rust has no computed goto; the inline arm plus a dense
  `match` on a `u8` gets the jump table.
- **Thread-local hoisting.** `builtin_types()`, the current thread id,
  `MAYBE_DEAD`, and the tier-2 `JIT` state are read once per frame
  entry into the `Frame` (or the `Interpreter`), not per instruction.
  `gil::current_thread_id()` is cached in the interpreter for the GIL
  holder and refreshed on hand-off.
- **Stack sized once.** `Frame.stack` is allocated (or pooled) at
  `co_stacksize` so `Vec::push` never grows mid-frame; `Frame::pop`
  and `push` become unchecked in release with a debug assertion.
- **`Object` clone/drop glue inlined.** The derived `Clone` and the
  drop glue on a 40-variant enum are out-of-line. The hot paths use
  `#[inline(always)]` wrappers that short-circuit the unboxed
  variants (`Int`, `Float`, `Bool`, `None`) before the `Arc` path,
  and `drop_glue` is avoided on `POP_TOP` of unboxed values.
- **Superinstructions**: the `Fuse*` inline caches already fuse
  `LOAD_FAST LOAD_FAST`, `LOAD_FAST LOAD_CONST`, `LOAD_FAST LOAD_ATTR`,
  `COMPARE_INT POP_JUMP`. This wave adds `LOAD_FAST STORE_FAST`,
  `STORE_FAST LOAD_FAST`, `LOAD_CONST RETURN_VALUE` (kept internal;
  `cpython_code` still presents the 3.14 wire form), and
  `LOAD_ATTR_METHOD CALL` (WS4).

Acceptance: `run_until_yield_or_return_impl + step` below 15% on the
census fixtures; `sumvm`/`nested_loops`/`jitloop` unchanged (they run
natively).

**Landed (WS3).** The thread-local hoisting shipped in full:
`GilCell`'s two thread-locals (`LIVE_CELL_GUARDS`, `THREAD_ID_CACHE`)
merged into one `CELL_TLS` record so a same-thread borrow costs one
`_tlv_get_addr`; the interpreter's private pools (frame locals, stacks,
scratch, shells) moved from `GilCell` to a plain `ThreadCell`; the
`WEAVEPY_NO_QUIET`/`WP_DBG_SAMPLE` probes became `Interpreter` fields
read once; the frame prologue/epilogue dropped two `GilCell` borrows and
a clone per frame (`exc_info_len`, `materialized_at_exit`); and the
per-instruction `watch_drops` classification became a 256-entry table
(`COARSE_DROP_CLASS`) instead of a `matches!` chain. The largest
handler bodies (`BINARY_OP`, `COMPARE_OP`, `STORE_FAST`, `POP_TOP`,
`POP_JUMP_IF_*`, `JUMP_BACKWARD`) were split out of `step` into
`*_step` helpers so the dispatch `match` is small enough to reason
about.

**`step_hot`.** The headline item, a `#[inline(always)]` front door in
`run_until_yield_or_return_impl` that decodes and runs the ten hottest
opcode shapes (`LOAD_FAST` in its settled fused forms, `STORE_FAST`,
`LOAD_CONST` table hits, `POP_TOP`, `BINARY_OP`, `COMPARE_OP`,
`POP_JUMP_IF_*` on a `bool`, the jumps, `RETURN_VALUE`) and falls
through to the untouched `step` for everything else, landed and was
measured against the same tree without it (interp-only, best of 3):
`generators` -9%, `nested_loops` -8%, `attr_access` -6.5%, `fib` -4%,
`call_overhead` -4%, `deltablue` -1%, the rest within noise. Small,
consistent, kept. It does not move the acceptance line:
`nested_loops` still spends 58% in `step + run_until_yield_or_return_impl`,
because the out-of-line call was never the dominant cost. The census
attributes the floor to the per-instruction work itself (`Vec::push`
growth checks, `Object` clone/drop glue, the `note_dropped` audit, the
eval-breaker and `lasti` bookkeeping, the instruction fetched twice per
dispatch) and, above all, to the per-*call* fixed cost: a trivial
Python-to-Python call is ~350 ns interp-only versus ~20 ns on CPython,
and a builtin call (`len`, `isinstance`, `abs`) ~150 to 500 ns versus
~10 ns. That 15x is the single number behind every object-shaped
fixture ratio and behind `_pydatetime` at 300x (its `timedelta.__new__`
is 15 asserts and a dozen builtin calls). Closing it needs the
structural items this RFC deferred (a borrowed frame spine instead of
the eight `Arc` clones per shell, the frame prologue collapsed to one
branch for the common shape, stack sized from `co_stacksize` with
unchecked push/pop, a manual inlined `Clone`/`Drop` for the unboxed
`Object` variants), recorded under Future work with these measurements
as the baseline.

#### WS4: attribute, method, and string-hash lookup

- **A per-type method cache** in the shape of `_PyType_Lookup`'s: a
  4096-entry process table keyed by `(attr_version, interned name
  pointer)` holding the resolved MRO entry (or a negative). Lookups
  compare the name by pointer first (all `co_names` are interned at
  code creation, which the compiler already guarantees for
  identifiers); `memcmp` runs only on a pointer miss. Invalidation
  rides the existing `bump_attr_version` walk. `TypeObject::lookup`
  becomes cache-then-walk.
- **Method calls never box a `BoundMethod`** on the IC path: the
  `LoadAttrMethod` cache plus `CallSelf` already exist; the census
  shows `Arc<BoundMethod>::drop_slow` on deltablue, so a shape
  (a method found on the class of an instance whose `__dict__`
  shadows nothing) still materializes. WS4 closes that path and adds
  `LoadAttrMethodNoDict` (the `__slots__`/no-dict twin of CPython's
  `LOAD_ATTR_METHOD_NO_DICT`) and the `LOAD_ATTR_METHOD CALL`
  superinstruction.
- **A cached string hash.** `Object::Str(Rc<str>)` carries none;
  `py_str_hash` recomputes on every dict probe with a str key that
  isn't a `co_names` entry. The mechanism: a process-wide,
  pointer-keyed hash cache sharded by the `Rc` address (the same
  posture as the existing `STR_LEN_CACHE`, but process-global and
  bounded), consulted from `py_hash_value` for strings of length >= 8
  (shorter strings hash faster than a cache probe). The
  `Rc<str>` -> `Rc<StrObj { hash, s }>` payload migration is the
  right long-term shape and is *evaluated* in this wave: if the
  accessor surface (`as_str()`) covers the VM's uses so the migration
  is mechanical, it lands; otherwise the side cache lands and the
  migration is enumerated for wave 13 with the count of non-accessor
  uses.
- **Instance attribute reads** keep the `LoadAttrInstance`
  (type id + `attr_version` + dict index) shape; the miss path
  (`load_attr_instance_default`, 80 samples on deltablue) is
  restructured so the common "not in instance dict, found on class,
  not a data descriptor" outcome takes one cache probe.

Acceptance: attribute-lookup bucket below 3% on deltablue;
`attr_access` interp-only at least 1.5x faster than the census.

**As landed (measured, interp, same host as WS2).** deltablue
`WEAVEPY_BENCH_WORK=50`: 1.797 s after WS2, 1.38 s after this batch
(CPython 3.13: 0.08 s inner time; the ratio is still 17x). What
changed from the plan:

- **The method cache is per thread and version-keyed, not
  process-global.** `AttrVersion` (`types.rs`) draws every class
  version from one process counter, so a version alone names a
  `(class, MRO, class dicts)` snapshot; the 4096-entry per-thread
  table (`type_cache`) is keyed by `(type, version, name hash)` and
  needs no invalidation walk beyond the `bump_attr_version` that
  already ran. `TypeObject::lookup`/`lookup_with_owner` are
  cache-then-walk, gated off only when a class dict has ever held a
  non-`str` key (`exotic_str_keys_possible`). `__bases__` assignment
  and a custom `mro()` bump the version on their rollback paths too:
  a negative cached during the empty-MRO window would otherwise
  outlive the restored MRO.
- **`BoundMethod` elision** landed on the `LOAD_ATTR (method) CALL`
  pair without a superinstruction: `load_method_ic_hit` pushes the
  `(function, receiver)` pair straight onto the stack, and the
  `CALL`/`CALL_KW` fast paths accept that shape (`has_self`). The
  `IC::CallBoundMethodExact` arm deopts to the generic path when it
  meets it.
- **A per-site resolved-method slot** (`MethodSlot`, in the code
  object's VM extension) holds a `Weak<PyFunction>` keyed by the
  class version, so a warm `LoadAttrMethod` hit is guard, shadow
  probe, version compare, upgrade. The MRO index, owner-dict borrow,
  slot probe, and name compare run once per version per site.
- **Names hash once per process, not per probe.** `co_names` carry
  memoized `py_str_hash` values and interned `Object::Str`s
  (`code_name_key`/`code_name_obj`); `STORE_ATTR` inserts the pooled
  key without a pool lookup, and `cached_slot_name_matches` settles on
  `Rc` identity before a byte compare. `siphash13` and `FxHasher`
  fold their tail bytes with fixed-width loads (the `memcpy` in each
  was a visible bucket on short identifiers).
- **Cached MRO-membership bits.** `TypeObject::mro_kind` caches "is a
  `super` subclass" and "is a `type` subclass"; `instantiate` and
  `load_attr_instance_default` were walking the MRO for those on
  every call. Reset by `bump_attr_version`.
- **`is_subclass_of` for `type` in `instantiate`** was the largest
  single line in the deltablue attribute bucket after the cache
  landed; it is now the cached bit above.
- **Not landed:** the string-hash side cache and the `StrObj`
  payload migration. The census after the changes above shows
  `py_str_hash` under 1% on all four fixtures; the migration is
  enumerated for the wave-13 review with the non-accessor use count
  rather than done here.

#### WS5: calls and frames

- **Lazy frame shells.** `push_frame_shell`/`recycle_frame_shell` run
  on every call to keep a Python-visible `FrameShell` spine. CPython
  materializes `PyFrameObject` only when something observes it
  (`sys._getframe`, a traceback, `f_back`, tracing, `locals()`,
  generators). WS5 makes the shell lazy: the interpreter keeps a
  lightweight linked spine of `&Frame` records; a `FrameShell` is
  minted on first observation and back-filled for the ancestors it
  needs (`f_back` chains), exactly `_PyFrame_GetFrameObject`. Tracing
  and `sys.settrace`/`sys.monitoring` presence force eager shells
  (the RFC 0031 hooks already flag this).
- **`CodeConstObjects` downcast removal**: the `Any::type_id` probe on
  the call path (27 samples on call_overhead) becomes a typed field.
- **Argument binding**: `pooled_locals_from_args` keeps its pool; the
  generic `call_python_owned` binder gains an early exact-positional
  fast path so non-IC call sites (first call, polymorphic sites) pay
  one length compare before falling into the full binder.

Acceptance: frame-shell bucket below 2% on call_overhead; `fib`
interp-only at least 1.4x faster than the census; `test_frame`,
`test_sys_settrace`, `test_pdb`, `test_traceback`, `test_inspect`
green (the shell laziness contract).

**Landed (WS5).** The typed `vm_ext` slot (the `Any::type_id` probe on
every constant and name lookup is now a direct cast, asserted in debug
builds) and the binder fast path: `call_python_owned` binds the
exact-positional shape with one length compare and a `resize`, and the
full CPython-order binder moved to `bind_python_args` behind it.
**Not landed:** the lazy frame spine. The shell is already lazy about
the `PyFrame`; what remains is its eight `Arc` clones per push and the
placeholder scrub per recycle (~24 uncontended atomics per call, 8% of
`deltablue`). Removing them means the spine borrows the live `Frame`
and copies out only when observed, which touches every one of the 65
`frame_stack` readers across nine files; it is deferred to the next
wave with that inventory, and the WS5 acceptance line is therefore a
recorded miss.

#### WS6: native `_collections` and the accelerator census

`_collections.deque`, `defaultdict`, and `OrderedDict` are pure Python
(`stdlib/python/_collections.py`); asyncio's ready queue,
`queue.Queue`, `threading`, and every `collections`-using package run
them on the tier-1 floor. WS6 lands `stdlib/collections_native.rs`:

- `deque` as a ring buffer over `VecDeque<Object>` with `maxlen`,
  `rotate`, `__reduce__`, `__iter__` mutation detection, `index`,
  `insert`, `copy`, `__class_getitem__`, and the `__weakref__` slot;
  `defaultdict` as a dict subclass with `__missing__` and
  `default_factory`; `OrderedDict` over `DictData`'s insertion order
  with `move_to_end`, `popitem(last)`, `__reversed__`, `__eq__` order
  sensitivity, and the `od` view classes; `_count_elements`,
  `_tuplegetter`, `_deque_iterator`/`_deque_reverse_iterator` names.
  Graded by `test_deque`, `test_defaultdict`, `test_ordered_dict`,
  `test_collections` (already measured rows).
- **The accelerator census** for the remaining pure-Python stand-ins
  (`_datetime` via `_pydatetime`, `_pickle`, `_decimal` via
  `_pydecimal`, `array`) is measured, not guessed: each gets a
  micro-fixture in `weavepy-bench` (`datetime_ops`, `pickle_bench`)
  so the next wave has a ratio, not an anecdote. Porting them is
  out of scope here except where the census shows a first-order
  ecosystem cost (the Django and celery probes both time
  `datetime`-heavy paths; if `datetime_ops` measures worse than 10x,
  the `_datetime` core types land natively in this wave under the
  honest-miss protocol).

**Landed (WS6).** The accelerator census: `deque_ops`, `datetime_ops`,
and `pickle_bench` fixtures with their ratios measured interp-only on
this tree (CPython 3.14 host, best of 1): `deque_ops` 967x before this
wave, `datetime_ops` 297x, `pickle_bench` 333x. The `deque_ops` number
was two algorithmic defects, not a constant factor, and both are fixed:
`_collections.deque` did `list.pop(0)`/`list.insert(0, x)` (O(n) per
`popleft`/`appendleft`), and the VM's `del list[a:b]` removed one
element at a time (O(k * n); `del data[:40000]` on 80k elements took
660 ms). The deque now keeps a consumed-prefix offset with amortized
compaction (all four end operations O(1), differential-fuzzed against
the C deque on 2,400 random 300-operation sequences), and slice
deletion is one `drain`/partition pass. `deque_ops` is 39x after
(2.06 s vs 0.052 s), which is the interpreter floor's per-call cost, no
longer a complexity class. **Not landed:** the native
`collections_native.rs` port. With the algorithmic fixes in, the
remaining 39x/297x/333x are the same 15x-per-call floor that WS3
measured, so porting `deque`/`OrderedDict`/`_datetime` to Rust would
buy each module its own constant while the floor stays; the RFC's
"first-order ecosystem cost" trigger for `_datetime` fired (297x >
10x) and is recorded as a miss with its fixture in place, to be
re-measured after the call-path work in Future work.

#### WS7: startup and RSS

Startup is 80 ms versus 37 ms (2.19x) and RSS is 37 MB versus 15 MB
(2.5x) on `-c pass`. WS7 measures first (`-X importtime` becomes a real
implementation instead of a documented no-op; a `WEAVEPY_STARTUP_TRACE`
phase timer prints the interpreter-construction breakdown) and then
lands the largest measured items. The candidates, in expected order:
the on-disk stdlib tree landmark walk and `site`'s path scan; eager
construction of native module tables that could be `OnceLock`-lazy per
module (81 `register_builtin` factories); frozen `code` decoding for
the startup import set (`encodings`, `codecs`, `io`, `abc`, `site`,
`os`, `stat`, `posixpath`, `genericpath`, `_collections_abc`) served
from the RFC 0059 disk cache with `mmap` instead of a read plus copy;
and the UCD/CJK tables (`gen_ucd_tables.py` output) which are
`include_bytes!` and page in only when touched, but whose index
structures are built eagerly. Gate below.

**Landed (WS7).** `-X importtime` and `PYTHONPROFILEIMPORTTIME` are a
real implementation: `import_path` times every fresh load on a
thread-local stack and prints CPython's `self [us] | cumulative |
name` lines, innermost first, indented by depth (`import_time.rs`).
Measured on this tree, best of 10: startup 42.8 ms versus 19.9 ms
(2.15x; the 80 ms in the charter was a loaded-machine sample) and RSS
37.2 MB versus 15.2 MB (2.45x). **Not landed:** the phase timer and
the lazy-table work. `-c pass` imports nothing user-visible, so the
42.8 ms is interpreter construction (the native module tables, the
frozen startup set's code decoding, the stdlib-tree landmark walk),
and the gate (1.6x / 2.0x) is a recorded miss with the measurement
tool in place for the next wave.

#### Pillar I gate

Measured on the committed macOS-aarch64 baseline, 5 samples, after
Phase I and before Phase II:

1. **Interpreter-only** (`interp` column): the geomean over the 16
   object-shaped fixtures (everything except `sumvm`, `nested_loops`,
   `jitloop`, `jitkernels`, `pidigits`) improves by at least **1.8x**
   versus the census committed in WS1, and no fixture's interp ratio
   regresses.
2. **Suite geomean (JIT on) <= 2.0x** CPython (from 2.91x), with
   `deltablue` <= 12x, `call_overhead` <= 4.5x, `richards` <= 6x,
   `list_ops` <= 7x, `pyaes` <= 7x, `dict_ops` <= 3.5x, `attr_access`
   <= 2.5x, `fib` <= 1.8x; loop kernels hold <= 0.06x; no fixture
   outside its committed envelope.
3. **Startup <= 1.6x** CPython and **RSS <= 2.0x** on `-c pass`.
4. Default-mode regrtest at `unexpected 0` on the 3.13 baseline,
   `--gil0` lane at its baseline, ecosystem 48/48 offline.

RFC 0074's honest-miss protocol applies: a missed gate lands with the
truthful re-committed baseline and a fresh census naming the next
bucket.

### Pillar II: the CPython 3.14 switch

**Affected crates**: every crate. `weavepy-vm` (stdlib table, `sys`,
`pycache`, `stdlib_tree`, `sysconfig_native`, `_asyncio`, `_warnings`,
`_interpreters`, annotations runtime), `weavepy-compiler`
(`cpython_code`, codegen for `with`/`assert`/annotations/`BINARY_OP`,
`OpCode` additions), `weavepy-lexer`/`weavepy-parser` (the preview
gate deletion), `weavepy-capi` (headers, symbol additions, `python314`
naming), `weavepy-pylib`, `weavepy-dist`, `weavepy-cli`,
`weavepy-conformance` (3.14 test tree default), `tests/ecosystem`
(cp314 re-fetch), `tools/` (the new sync tool).

#### WS8: the stdlib re-vendor and its tool

The bundled stdlib is a hand-maintained 586-entry `include_str!`
table over `crates/weavepy-vm/src/stdlib/python/`, with some files
renamed (`random_mod.py` -> `random`, `os_source.py`,
`concurrent_futures_init.py`). Measured against the host CPython 3.13
`Lib/`: 319 files are byte-identical (verbatim), 46 differ
(WeavePy-patched), 213 have no upstream counterpart (WeavePy-authored
shims and the frozen third-party facades). A new
`tools/stdlib_sync.py`:

1. Reads the frozen table to learn the bundled-file -> module-name
   mapping, then classifies each bundled file against a `--from` tree
   (3.13) as verbatim / patched / authored.
2. With `--to` (3.14.7): overwrites verbatim files with the 3.14
   version; for patched files computes the WeavePy patch (`diff`
   bundled versus 3.13) and applies it to the 3.14 file with a 3-way
   merge, writing `.rej` hunks for manual resolution; leaves authored
   files alone and lists them.
3. Reports 3.14 modules absent from the table (`annotationlib`,
   `_py_warnings`, `_ast_unparse`, `string/` as a package,
   `_opcode_metadata`, `_pyrepl` additions, `concurrent/interpreters`,
   `asyncio/tools.py`, `asyncio/graph.py`) and 3.13 modules gone
   (`_compression.py`, `string.py`), and emits the `FrozenSource`
   stanzas to paste.
4. Runs in `--check` mode in CI: every file classified verbatim must
   be byte-identical to the vendored tree it claims, so drift is a
   test failure, not archaeology.

**Landed (WS8, tool and census).** `tools/stdlib_sync.py` does all
four, reading the `FrozenSource` table so the name-to-file mapping is
never maintained twice. Against the vendored 3.13 tree
(`vendor/cpython/Lib`, the regrtest oracle rather than the Homebrew
install the charter counted) the census is **426 verbatim / 67 patched
/ 89 authored / 4 inline**, recorded in `tools/data/stdlib_verbatim.txt`
and gated by `crates/weavepy/tests/stdlib_sync.rs` (`--check`). The
dry run onto `vendor/cpython314/Lib` reports **216 verbatim flips, 20
clean 3-way merges, 25 conflicts, 3 gone** (`pathlib._abc`,
`_compression`, `_sysconfigdata__darwin_darwin`) and **106 new 3.14
modules** absent from the table (with their stanzas emitted). The 25
conflicts are the hand-merge list for the flip: `_pydatetime`, `ast`,
`codecs`, `codeop`, `contextvars`, `copy`, `copyreg`, `encodings`,
`ensurepip`, `lzma`, `multiprocessing.context`,
`concurrent.futures.process`, `opcode`, `re`, `runpy`, `ssl`,
`struct`, `subprocess`, `symtable`, `sysconfig`, `types`, `weakref`,
`zoneinfo._zoneinfo`. **Not landed:** the write (`--write`) itself,
which is Phase II's first step and lands with WS9 to WS13 as one flip.

The re-vendor itself: the verbatim 319 flip mechanically; the 46
patched files merge (the interesting ones: `traceback.py`,
`warnings.py` (which becomes a thin shim over `_py_warnings` in
3.14), `threading.py`, `functools.py`, `ast.py`, `dis.py`,
`opcode.py`, `pickle.py`, `_pydatetime.py`, `subprocess.py`,
`asyncio/tasks.py`, `asyncio/__init__.py`, `sysconfig/__init__.py`,
`venv/__init__.py`); `test.support` re-vendors at 3.14 (74 labels);
`unittest` re-vendors at 3.14 (55 labels: the six assert methods
arrive with the package). The authored 213 are audited against the
3.14 API deltas the gap analysis named (`heapq.heapify_max` family in
`bisect_mod`/`heapq` natives, `fnmatch.filterfalse`, `getopt`,
`pprint`, `timeit`, `cmd`, `pdb`, `bdb`, `pstats`, `cProfile`,
`gettext`, `plistlib`, `html.parser`, `zipfile`, `pathlib`,
`importlib` machinery, the `re` engine's `\z` anchor and `\Z`
deprecation).

#### WS9: bytecode, magic, and the codec

- **Magic 3627** in `pycache.rs`, `cpython_code.rs`, `imp_mod.rs`, and
  the frozen `importlib_bootstrap_external.py`; the cache tag becomes
  `weavepy-314-<n>`; `_imp.pyc_magic_number_token` lands (3.14 moved
  the constant into C).
- **The opcode table** in `cpython_code.rs::op` is regenerated from
  3.14's `opcode_ids.h`; `cache_entries` follows
  `_opcode_metadata._inline_cache_entries` for 3.14; the frozen
  `opcode.py`/`_opcode.py`/`dis.py` become the verbatim 3.14 modules
  plus the new `_opcode_metadata.py`.
- **Codegen deltas** in `weavepy-compiler`:
  - `with`/`async with`: `BEFORE_WITH`/`BEFORE_ASYNC_WITH` replaced by
    `COPY 1; LOAD_SPECIAL __exit__; SWAP 2; SWAP 3; LOAD_SPECIAL
    __enter__; CALL 0` (3.14's shape); the VM gains `LOAD_SPECIAL`
    with the `_Py_SpecialMethods` table (`__enter__`, `__exit__`,
    `__aenter__`, `__aexit__`).
  - `assert`: `LOAD_ASSERTION_ERROR` -> `LOAD_COMMON_CONSTANT 0`;
    `NotImplementedError` is constant 1 (used by PEP 649 annotate
    functions).
  - `BINARY_SUBSCR` -> `BINARY_OP NB_SUBSCR` (oparg 26); the internal
    `BinarySubscr` opcode stays for the VM and its ICs; only the wire
    form changes.
  - `RETURN_CONST` retired: the codec's fusion is deleted and
    `LOAD_CONST; RETURN_VALUE` is presented (or `LOAD_SMALL_INT` for
    ints in `0..=255`).
  - `LOAD_SMALL_INT` for int constants in `0..=255` (they leave
    `co_consts`); `LOAD_FAST_BORROW` and
    `LOAD_FAST_BORROW_LOAD_FAST_BORROW` from a liveness pass ported
    from `flowgraph.c::optimize_load_fast` (a load is a borrow when
    the value is consumed by an instruction that doesn't escape it
    before the local could be rebound); `NOT_TAKEN` after every
    conditional jump (an instrumentation anchor); `POP_ITER` closing
    `FOR_ITER` loops; `END_ASYNC_FOR` with an oparg;
    `CALL_FUNCTION_EX` always carries the kwargs slot (`PUSH_NULL`
    when absent); the 3.14 genexp/`all`/`any` and while-loop test
    duplication shapes.
  - `LOAD_METHOD`, `LOAD_SUPER_METHOD`, `LOAD_ZERO_SUPER_*` were
    pseudo-ops; `LOAD_SUPER_ATTR` presentation is unchanged.
- **marshal / pyc** re-encode with the new table; `test_marshal`
  keeps its two-subtest divergence row.
- **The codegen-stage surface** (`_weave_flowgraph.py`,
  `_weave_codegen.py`, `_weave_iseq.py` behind
  `_testinternalcapi.compiler_codegen`/`optimize_cfg`/`assemble_code_object`)
  is updated to the 3.14 pseudo-op set so `test_compiler_codegen`,
  `test_compiler_assemble`, and `test_peepholer` grade on 3.14.

Acceptance: `test_dis`, `test__opcode`, `test_opcodes`, `test_code`,
`test_compile`, `test_peepholer`, `test_compiler_codegen`,
`test_compiler_assemble`, `test_compileall`, `test_zipimport`,
`test_importlib` pass on the 3.14 tree; `python3.14 -c "import dis,
marshal"` cross-checks a corpus of code objects byte-for-byte against
WeavePy's `marshal.dumps` (the RFC 0033 fixture, re-pointed).

#### WS10: PEP 649/749 deferred annotations

The deepest item. Design, following `codegen.c`:

- **Compiler.** For a function, class body, or module with
  annotations and without `from __future__ import annotations`, the
  compiler no longer builds an annotations dict; it emits a hidden
  `__annotate__(format)` function whose body is `if format > 2: raise
  NotImplementedError` followed by the dict build over the annotation
  expressions, closed over the scope's cells (`__classdict__` for class
  bodies so class-level names resolve, `__conditional_annotations__`
  membership checks for annotations under control flow). Functions get
  it via `SET_FUNCTION_ATTRIBUTE 0x10` (the new `annotate` slot, and
  `0x04` annotations is no longer emitted); class bodies and modules
  store `__annotate__` in the namespace with `ANNOTATIONS_PLACEHOLDER`
  reserving the slot in class bodies. The future-import path keeps
  today's eager stringized `__annotations__` and `SETUP_ANNOTATIONS`.
- **Runtime.** `function.__annotate__` (settable; `None` allowed),
  `function.__annotations__` as a lazy property that calls
  `__annotate__(1)` once and caches; `type.__annotations__` and
  `type.__annotate__` as data descriptors reading
  `__annotations_cache__`/`__annotate_func__` from the class dict
  (with the 3.14 inheritance rules: a class without annotations
  reports `{}` rather than its base's); module `__annotations__` via
  the module type's getset calling the module's `__annotate__`.
  `typing`, `dataclasses`, `inspect`, `functools`, `enum`, and
  `pydoc` arrive verbatim from 3.14 and consume `annotationlib`.
- **`annotationlib`** is the verbatim 3.14 module; its `FORWARDREF`
  and `STRING` formats run compiler-generated annotate functions with
  fake globals (`VALUE_WITH_FAKE_GLOBALS`), which requires the VM to
  honor a `__globals__`-replaced function call with `__builtins__`
  resolution through the fake mapping and `Stringifier` operator
  overloads. `_testinternalcapi` gains nothing new here; the surface
  is Python-level.

Acceptance: `test_annotationlib`, `test_type_annotations`,
`test_typing`, `test_dataclasses`, `test_inspect`, `test_functools`,
`test_type_params`, `test_grammar` pass on 3.14; the RFC 0057
comprehension-scope canaries and `test_rfc0076_burn_regressions.py`
(attrs' `compile()`-at-class-build `super()` shape) stay green.

#### WS11: the stdlib tail

- **asyncio**: the verbatim 3.14 package (policy deprecation split,
  `asyncio.tools`, `asyncio.graph`, `_DefaultEventLoopPolicy`) over
  `_asyncio` grown for 3.14: `future_add_to_awaited_by` /
  `future_discard_from_awaited_by`, the `Task` `eager_start` keyword,
  `_asyncio.all_tasks`/`current_task` changes, and the
  `_py_all_tasks` fallbacks. The 31 asyncio-family labels and
  `test_coroutines`/`test_pdb` are the acceptance rows.
- **PEP 734 `concurrent.interpreters`**: verbatim 3.14 package over
  the existing `_interpreters`/`_interpchannels`/`_interpqueues`
  shims, extended for the 3.14 surface (`Interpreter.call`,
  `call_in_thread`, `Queue` with `unbounditems`, `is_shareable` for
  the 3.14 shareable set). The 3.13 top-level `interpreters` frontend
  is retired.
- **warnings**: `warnings.py` becomes 3.14's shim over the new
  `_py_warnings.py`; `_warnings` native gains
  `_warnings_context`/`_acquire_lock`/`_release_lock` and the
  `sys.flags.context_aware_warnings` / `thread_inherit_context` flags
  (`-X context_aware_warnings`, `-X thread_inherit_context`; default
  `0`/`0` under the GIL, `1`/`1` under `-X gil=0`, exactly 3.14's
  build-dependent defaults).
- **Long tail** from the gap census: `complex.from_number` and
  `Fraction.from_number`, `heapq.heapify_max`/`heappush_max`/
  `heappop_max`/`heapreplace_max`/`heappushpop_max`, float
  thousands-separator format specs (`'.,_f'` and `','`/`'_'` in the
  fraction part), `re`'s `\z` anchor and `\Z` deprecation warning,
  `memoryview.__class_getitem__` (PEP 688 subscriptability),
  `getpass._check_echo_char`, `faulthandler.dump_c_stack`,
  `_codecs._unregister_error`, `http.server.HTTPSServer` (verbatim),
  `_colorize.Theme` (verbatim), `fnmatch.filterfalse`, `bytes.fromhex`
  whitespace tolerance, `map(strict=)`, `pathlib.Path.copy/move`
  (verbatim), `os.readinto`, `io.Reader/Writer` protocols, and
  `unittest`'s asserts (verbatim). Each is one row in the sweep.
- **PEP 768**: `sys.remote_exec` and `sys.is_remote_debug_enabled()`
  land with the without-remote-debug posture (`False`,
  `remote_exec` raises `RuntimeError`), so `test_remote_pdb` and
  `test_sys` grade faithfully rather than as a missing attribute.
- **PEP 750 / PEP 758 default-on**: the `LANG_PREVIEW` gate in
  `weavepy-lexer/src/token.rs`, `weavepy-parser/src/parser.rs`,
  `crates/weavepy/src/lib.rs`, and `weavepy-vm/src/lib.rs` is
  deleted; `-X lang=next` is accepted and ignored for one wave with a
  `DeprecationWarning`; the bundled t-string fixtures move from the
  `xflags` row key to plain rows; `string.templatelib` becomes the
  verbatim 3.14 module.
- **`_zstd`**: the native module is checked against 3.14's
  `test_zstd.py` import surface (`_zstd.ZstdCompressor`,
  `ZstdDecompressor`, `ZstdDict`, `get_frame_info`, `get_frame_size`,
  `train_dict`, `finalize_dict`, `set_parameter_types`, the
  `zstd_version`/`zstd_version_info`/`ZSTD_CLEVEL_DEFAULT` constants)
  so the vendored `test_zstd` graduates from `skip` to a measured row.
- **`compression`** and `_compression` follow 3.14 (`_compression.py`
  removed; `compression._common._streams` is the home).

#### WS12: the C-API delta and the header re-vendor

- `crates/weavepy-capi/include/cpython313/` -> `cpython314/` (the 3.14.7
  `Include/` tree; `build.rs` path and the generated table follow);
  `pyconfig/*.h` regenerated for 3.14 (`PY_VERSION_HEX 0x030e07f0`,
  `Py_GIL_DISABLED` undefined, the new `Py_REMOTE_DEBUG` off).
- New symbols, each with a `force_link_table.rs` anchor and a
  `_testcapi`/`capi_ext` fixture leg: `PyUnicodeWriter_Create/Discard/
  Finish/WriteChar/WriteUTF8/WriteASCII/WriteWideChar/WriteStr/
  WriteRepr/WriteSubstring/Format/DecodeUTF8Stateful`,
  `PyLong_FromInt32/FromInt64/FromUInt32/FromUInt64`,
  `PyLong_AsInt32/AsInt64/AsUInt32/AsUInt64`, `PyLong_IsPositive/
  IsNegative/IsZero`, `PyLong_GetSign`, `PyIter_NextItem`,
  `Py_HashBuffer`, `PyBytes_Join`, `PyList_Extend`, `PyList_Clear`,
  `PyDict_Pop`, `PyDict_PopString`, `PyImport_ImportModuleAttr`,
  `PyImport_ImportModuleAttrString`, `PyType_GetBaseByToken`
  (+ `Py_tp_token`, `Py_TP_USE_SPEC`), `PyUnicode_Equal`,
  `Py_fopen`, `Py_fclose`, `PyUnstable_Object_IsUniqueReferencedTemporary`,
  `PyUnstable_Object_EnableDeferredRefcount` (no-op returning 0),
  `PyUnstable_IsImmortal`, `PyUnstable_TryIncRef`,
  `PyUnstable_EnableTryIncRef`, `PyMonitoring_FirePyStartEvent` and
  the rest of the `PyMonitoring_*` family over the RFC 0031 event
  table, `PyCode_GetVarnames/GetCellvars/GetFreevars` (3.11, audit),
  `Py_PACK_VERSION`/`Py_PACK_FULL_VERSION`, `PyTuple_FromArray`
  (private, Cython uses it), `_PyLong_Sign`/`_PyLong_NumBits`
  audits, `PyThreadState_GetUnchecked`, `Py_TYPE`/`Py_SET_TYPE` as
  functions for the limited API, `PyLong_AsNativeBytes`/
  `FromNativeBytes` (3.13, audit).
- Removed in 3.14 (the header re-vendor drops their declarations; the
  symbols stay exported for old wheels): `PyDictProxy_Check` variants,
  `_PyDict_GetItemStringWithError`, `_PyUnicodeWriter_*` (private,
  still used by Cython 3.0 wheels; keep exported).
- Naming: `weavepy-pylib` cdylib `python314`; `libpython3.14.{dylib,so}`,
  `python3.14-config`, `python-3.14.pc`, `python-3.14-embed.pc`,
  `include/python3.14/`, `python314.dll` + `libs\python314.lib`, and
  `lib/weavepy3.14` with the `lib/python3.14` symlink, all from one
  `weavepy_version::{MAJOR, MINOR, MICRO}` source shared by `sys.rs`,
  `stdlib_tree.rs`, `sysconfig_native.rs`, `pycache.rs`,
  `weavepy-dist`, `weavepy-pylib/build.rs`, and the CLI, so the 3.15
  switch is a three-constant edit plus the codec table.
- `sysconfig`: `EXT_SUFFIX .cpython-314-darwin.so` / `.cp314-win_amd64.pyd`,
  `SOABI cpython-314`, `py_version_nodot 314`, `LDVERSION 3.14`,
  `Py_GIL_DISABLED 0`, and `_weave_sysconfigdata` regenerated from the
  3.14 template.

**Landed (WS12, the consolidation).** The new dependency-free
`weavepy-version` crate is the single source: `MAJOR`/`MINOR`/`MICRO`,
the derived `SHORT`/`NODOT`/`FULL`/`HEX`, and the identity literals
(`LIB_DIR_NAME`, `CACHE_TAG_PREFIX`, `SOABI_PREFIX`, `CP_TAG`,
`PYLIB_STEM`, `HEADER_TREE`), with a `vconcat!` const-string macro so
the platform suffix tables are built from them at compile time and a
unit test that pins every literal to the numbers. `sys.rs`
(`PY_VERSION`, `winver`), `stdlib_tree.rs`, `pycache.rs`,
`sysconfig_native.rs` (`EXT_SUFFIX`, `SOABI`), the extension loader's
suffix table, `Py_Version`, `Py_GetVersion`, the CLI's DLL name, and
both header-embedding `build.rs` now read from it. **Not yet
consolidated:** `weavepy-pylib`'s cdylib name (a Cargo manifest
field), `weavepy-dist`'s artifact names and embedded check scripts,
and the `cpython313/` header directory name itself; these move with
the flip.

Acceptance: `test_capi` shares that the gap census attributed to
3.14 surface growth pass or carry measured reasons; the
`force_link_completeness` and `_abi3check` tests pass; a real cp314
wheel with a compiled extension (numpy) imports via the regular
`ExtensionFileLoader` path in the ecosystem lane.

#### WS13: the identity flip

`PY_VERSION = (3, 14, 7)` (the micro tracks the vendored Lib, so
`sys.version` and `platform.python_version()` agree with the tree
`test_*` was written against), `sys.hexversion 0x030e07f0`,
`sys.winver "3.14"`, `sys.implementation.cache_tag "weavepy-314"`,
`_minipip`/`_packaging` derive `cp314` from `version_info` (no edit),
`sys._git`/`sys.version` string, `weavepy --version` output, the
`_testembed` twin's expected config dump, the dist `check` matrix
(identity spot-checks), `docs/CONFORMANCE.md`, `README.md` status
paragraph, and the `weavepy-3.13` maintenance branch cut at the
Phase I checkpoint commit (RFC 0076's release-branch model).

#### WS14: re-measure and re-baseline

1. **regrtest**: the conformance runner's default `--cpython-dir`
   becomes `vendor/cpython314/Lib/test`; `docs/CONFORMANCE.md` and the
   README commands follow. The full sweep runs (`--mode subprocess
   --jobs 8`, budgets per the RFC 0049 protocol), and
   `tests/regrtest/expectations.toml` is rewritten from evidence:
   every non-pass row carries a measured first-failure reason.
   `expectations-gil0.toml` re-measures on 3.14 (the
   `test_free_threading/` portable labels join the lane).
2. **ecosystem**: `tools/ecosystem_fetch.py` refreshes the wheel cache
   for cp314 (pins bumped only where a row's pinned version publishes
   no cp314 wheel; each bump is noted in the row comment); all 48
   probe rows and the 10 selftest rows re-run online and offline, and
   `tests/ecosystem/expectations.toml` is rewritten from evidence.
   The numpy selftest re-measures under the faster interpreter (the
   RFC 0076 budget-bound shards).
3. **bench**: the macOS-aarch64 baseline is re-committed from fresh
   runs with the `interp` column (the Phase I checkpoint numbers are
   the perf evidence; the post-flip re-run confirms the flip didn't
   move them beyond `startup`).
4. `cargo fmt`, `clippy -D warnings`, `cargo test --workspace` green;
   new bundled regrtests for every engine fix (standing policy).

#### Pillar II gate

1. **3.14 sweep**: every label in the 3.14 tree scheduled and graded;
   `unexpected 0` against the rewritten baseline; **fail + error <=
   20** of the ~470 labels with measured reasons (from 287 in the gap
   sweep), zero crash-class failures, zero `timeout` rows; the WS9
   and WS10 acceptance lists above pass outright.
2. **Ecosystem 48/48** probe rows on cp314 wheels, offline; selftest
   rows at their RFC 0076 verdicts or better.
3. Identity: `weavepy -c "import sys; print(sys.version_info[:2])"`
   prints `(3, 14)`; `pip debug --verbose` under WeavePy lists `cp314`
   tags; `python3.14-config --ldflags` names `libpython3.14`; the
   `_testembed` twin's lifecycle legs pass.
4. `-X lang=next` no longer changes behavior; `t"..."` and
   `except A, B:` parse by default; `from __future__ import
   annotations` still stringizes; `test_tstring`, `test_grammar`,
   `test_syntax`, `test_annotationlib` pass.

## Drawbacks

- **Two waves' worth of scope in one landing.** Mitigated by the
  phase checkpoint: Phase I is measured and could ship alone; Phase
  II's attribution is clean because Phase I's numbers are frozen
  first. The honest-miss protocol covers either pillar independently.
- **The floor work touches the most central code in the repo**
  (`Object` clone/drop, the dispatch loop, frame lifetime, the reaper).
  The regression oracle is the best it has ever been (550 labels at
  zero, 48 rows, the selftest lanes), and every mechanism above
  preserves an observable contract that a named test already guards.
- **Lazy frame shells change *when* a `PyFrame` exists**, which is
  observable through `id()`/weakrefs only when something already
  observes the frame. CPython has the same materialization semantics;
  `test_frame`/`test_sys_settrace`/`test_pdb`/`test_traceback` are the
  guards.
- **A 300K-line stdlib diff** buries the authored changes in review.
  Mitigated by `tools/stdlib_sync.py --check` classifying every file
  and by landing the re-vendor as the first Phase II step so the diff
  after it is small and readable.
- **PEP 649 changes observable semantics** (annotations evaluate
  lazily; `__annotations__` on a class without its own annotations is
  `{}`; a `NameError` at definition time becomes deferred). This is
  the 3.14 behavior and the reason the switch couldn't be gated.
- **cp314 wheel re-fetch** may expose ABI holes that cp313 wheels
  didn't reach (Cython 3.1's 3.14 paths, pybind11's
  `PyUnstable_*` probes). That is the point of the lane; a measured
  fail row with a named root cause is an acceptable landing for a
  row that was green on cp313 only if the cause is upstream-shaped.
- **Maintenance branch cost.** `weavepy-3.13` receives cherry-picks
  only; no CI lane is added for it this wave.

## Alternatives

- **Floor first, switch next wave (the pre-wave recommendation):**
  rejected by the explicit one-landing request; the phase checkpoint
  recovers the measurement discipline that motivated the split.
- **More JIT lanes instead of the floor:** rejected on the census;
  two consecutive waves measured flat, and the interpreter-only
  column shows why.
- **Biased refcounting / a non-atomic `Rc` under the GIL:** the
  pre-wave hypothesis; retired by the profile (`GilCell::borrow` and
  `Arc` RMWs are not first-order). Stays available with a census that
  says otherwise.
- **The `Rc<str>` -> `Rc<StrObj>` payload migration as a hard
  requirement:** evaluated in WS4 rather than mandated; the side
  cache delivers the measured bucket either way.
- **Skipping 3.14 for 3.15 (October 2026):** rejected by the version
  policy (N.1 plus wheels), and 3.15.0 isn't out.
- **A dual 3.13/3.14 runtime:** rejected permanently in RFC 0076.
- **Porting `_datetime`/`_pickle`/`_decimal` natively in this wave:**
  deferred behind a measured micro-fixture census (WS6) except where
  the census shows a first-order cost; `_collections` lands because
  its cost is structural (asyncio's ready queue).

## Prior art

- **CPython 3.11's "faster CPython" work**: the adaptive specializing
  interpreter, `_PyInterpreterFrame` versus `PyFrameObject` laziness,
  zero-cost exception tables, and the per-type method cache predate
  any JIT and delivered 1.25x; this wave copies that ordering.
- **CPython 3.14's `LOAD_FAST_BORROW`**: the reference interpreter
  itself is now eliding refcount traffic on the stack; WeavePy's
  clone/drop-glue bucket is the same cost by another name.
- **PyPy's `W_Root` and str hash caching**: every mature Python
  runtime caches string hashes in the object.
- **RFC 0036/0049**: the measured-baseline protocol that WS14 repeats
  on the 3.14 tree; **RFC 0074/0076**: the honest-miss protocol.
- **RustPython's `PyStrInterned` and `PyStr { hash: PyAtomic<...> }`**:
  the payload shape WS4 evaluates.

## Unresolved questions

- **Does the suspect list's per-thread shape hold under `-X gil=0`?**
  The wave keeps the global map as the `gil=0` fallback; the `--gil0`
  lane is the measurement.
- **How far does `step` inlining go before `rustc` stops inlining the
  loop body well?** WS3 measures the 24-opcode subset; the cut is
  moved by evidence.
- **Is `interp` geomean 1.8x reachable from the six buckets?** They
  sum to roughly half of samples; removing half the work is 2x at the
  limit. The honest-miss protocol is the answer either way.
- **Which 46 patched files merge cleanly onto 3.14?** The tool reports
  `.rej` hunks; `warnings.py`, `traceback.py`, `asyncio/tasks.py`,
  and `threading.py` are the likely manual merges.
- **PEP 649 and the class-body `__classdict__` cell**: WeavePy's
  compiler already surfaces `__class__` and `__classdict__`-shaped
  cells (RFC 0057/0076); whether the annotate function's closure
  needs a new cell kind is decided in WS10.
- **Which cp314 wheel pins move?** Unknown until
  `tools/ecosystem_fetch.py` runs; each move is recorded in the row.
- **Does `test_free_threading/` on 3.14 add portable labels to the
  `--gil0` lane, and do they pass?** Measured in WS14.

## Future work

- **Performance wave 13**: the next census after this floor lands
  (the `Object` 16-byte thinning, `Rc<StrObj>` if deferred,
  `_datetime`/`_pickle`/`_decimal` natives per the WS6 micro-fixtures,
  dict split keys for instances, the guard-epoch infrastructure the
  JIT waves keep deferring, which a dict version word from this wave's
  `DictData` work would enable).
- **JIT under free-threading** and the `--gil0` full-sweep baseline
  (RFC 0076 future work, unchanged).
- **The 3.15 switch wave** (trigger: 3.15.1 plus cp315 wheels, early
  2027): a three-constant edit plus the codec table if WS12's
  `weavepy_version` consolidation holds.
- **Package-manager distribution** (brew, pyenv, uv-consumable
  tarballs, signed releases): RFC 0062 non-goals, still unclaimed.
- **PEP 768 real remote debugging** if a consumer appears.

## Results

Filled in as the wave lands, per the RFC 0058/0049 protocols.

### Phase I checkpoint (3.13 oracle)

Measured on the final Phase I tree (this commit), macOS-aarch64,
CPython 3.14 host as the reference:

- **regrtest** (`--mode subprocess --jobs 8`, default mode): 446
  labels, `unexpected 0` against the committed 3.13 baseline after one
  fix the sweep surfaced: the WS2 hot/cold finalizable gate had made
  the "coroutine was never awaited" warning late by up to
  `FIN_COLD_STRIDE` safe points when the coroutine's last holder was a
  traceback-pinned frame cleared by `assertRaises`
  (`test_asyncio.test_events.test_run_until_complete_nesting`);
  `gc_trace::mark_bulk_drop` now forces a whole-index re-grade after
  an exceptional frame unwind and after `frame.clear()`, which are
  rare relative to drop safe points, so the steady-state gate keeps
  its win. (A first sweep run against a `/tmp` copy of the binary also
  flagged `test_embed`, `test_venv`, and `test_interpreters`; all
  three pass in-tree and were the copied binary lacking its siblings,
  a harness artifact and not a regression.)
- **Interpreter-only floor** (`WEAVEPY_JIT=0`, best of 3, same
  fixtures and work as the WS1 census): the per-call floor is
  unchanged in kind (~350 ns per Python call versus ~20 ns on CPython;
  a builtin call 150 to 500 ns versus ~10 ns), and the WS3 `step_hot`
  front door moves the dispatch-bound fixtures by 4 to 9%. The Pillar
  I 1.8x interp gate is **not met**; the census that names the next
  bucket (the borrowed frame spine and the call prologue, WS5's
  recorded miss) is above.
- **Algorithmic fixes with order-of-magnitude effect**: `deque_ops`
  967x to 39x (`_collections.deque` O(1) ends; `del list[a:b]` one
  pass instead of O(k * n)).
- **Startup / RSS**: 42.8 ms / 37.2 MB versus 19.9 ms / 15.2 MB (2.15x
  / 2.45x); gate not met, `-X importtime` in place.
- **Bench baseline** (`weavepy-bench run --update-baseline`,
  `baselines/bench-macos-aarch64.json`, v6 with the `interp` column):
  suite geomean **2.78x** CPython (JIT on; 2.91x committed before this
  wave), **interp geomean 6.68x**. The three accelerator census rows
  are in the file and gated per row but excluded from both geomeans
  (`fixtures::CENSUS_FIXTURES`): `deque_ops` 48x / 70x interp,
  `datetime_ops` 269x / 268x, `pickle_bench` 341x / 282x. Caveat on
  this recording: the host carried an unrelated compiler job and two
  system daemons at 70 to 99% CPU for the whole run (load average 15
  at the start), so per-row numbers wobble beyond the gate's 10%
  envelope run to run (`pyaes` 25.6x here versus 13.3x committed;
  `json_bench` 3.6x versus 5.7x). The baseline should be re-recorded
  on a quiet host before it's used as the ratchet; the geomeans are
  consistent with the earlier best-of-3 timings above.
- **Pillar II preparation** landed alongside without changing the
  3.13 identity: `tools/stdlib_sync.py` with the recorded census and
  drift gate (WS8), and the `weavepy-version` consolidation (WS12).

### Phase II (3.14 sweep, ecosystem on cp314, bench re-run)

TBD.

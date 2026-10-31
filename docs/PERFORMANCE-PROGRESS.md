# Performance progress

This note tracks the work to make WeavePy significantly faster than CPython
3.14 on realistic workloads, measured by wall time with a profile-guided
build, plus peak memory and startup. It records where WeavePy stands, the
plan, what's done, and what's next, so a new session can pick up the work.

## Where WeavePy stands

Measured on October 29, 2026, at `faa68a33` (PR #102,
`perf/step-change`, stacked on PR #101), on the same host and CPython,
both WeavePy binaries profile-guided and interleaved with CPython in one
idle session. The base is PR #101's PGO binary (`ba518cf0`).

| Measure | PR #101 | PR #102 |
| --- | ---: | ---: |
| Warm time, 33 workloads (geomean) | 1.113 | 1.098 |
| Cold time (first call, including compilation) | 1.366 | 1.261 |
| Peak resident memory | 1.670 | 1.688 |
| Instructions (quick build) | 1.156 | 1.145 |

Against PR #101, cold time is 0.923 (`tomllib_loads` 0.73, `go` 0.81,
`text_processing` 0.82, `nqueens` 0.84, `async_tree` and
`generators_tree` 0.85), warm time 0.987 (`generators_tree` 0.84,
`nqueens` 0.86, `coroutines`, `pickle_roundtrip` and `stdlib_utils`
0.91), and peak memory 1.011: start-up and imports are smaller (`-c pass`
9.95 to 9.16 MB, the harness's imports 16.7 to 15.8 MB), but a workload
that compiles keeps about 2 MB more for the background compiler. Eleven
workloads beat CPython's warm time (`pickle_roundtrip` 0.56,
`json_roundtrip` 0.68, `scimark` 0.70, `decimal_telco` 0.71), and
`coroutines`, `graph_search` and `text_processing` are within 1% of it.

The earlier checkpoints follow.

Measured on October 28, 2026, at `e4112942` (PR #101,
`perf/beat-cpython-everywhere`), on the macOS x86-64 development host
(Intel Core i9-9980HK), against CPython 3.14.8, with both WeavePy binaries
profile-guided and every run interleaved with CPython's in one session (the
machine idle). The base is `main` at `5cfae8d9`. Ratios are WeavePy over
CPython; below 1.00 means WeavePy is faster or smaller.

| Measure | Base `5cfae8d9` | PR #101 |
| --- | ---: | ---: |
| Warm time, 33 workloads (geomean) | 1.208 | 1.104 |
| Cold time (first call, including compilation) | 1.447 | 1.359 |
| Peak resident memory | 1.751 | 1.639 |
| Instructions (quick build) | 1.245 | 1.157 |

Against the base, warm time is 0.914, cold time 0.939 and peak memory
0.936. CPython's own peak memory moves between sessions (this session's
base measured 1.751 times it, an earlier one 1.648), so memory is best
compared with the base in the same run. Twelve workloads beat CPython's
warm time (`pickle_roundtrip` 0.60, `json_roundtrip` and `scimark` 0.67,
`decimal_telco` 0.71, `unpack_sequence` 0.74, `xml_etree` 0.79,
`float_points`, `regex_mix` and `sqlite_queries` 0.84, `graph_search` 0.92,
`raytrace` 0.93, `sorting` 0.96) and `bytes_codecs` is even. The worst are
`async_tree` 1.92, `context_managers` 1.89, `stdlib_utils` 1.78, `deepcopy`
1.65, `logging_mix` 1.52, `tomllib_loads` 1.46, and `go` and
`class_creation` 1.44.

The earlier checkpoint follows.

Measured on October 20, 2026, at `4a2ca7f` (`main`), on the macOS x86-64
development host (Intel Core i9-9980HK), against CPython 3.14.8. The WeavePy
binary is profile-guided (`tools/pgo/build.sh`). Ratios are WeavePy over
CPython; below 1.00 means WeavePy is faster or smaller.

| Measure | Ratio |
| --- | ---: |
| Warm time, 33 workloads (geomean) | 1.28 |
| Cold time (first call, including compilation) | 1.51 |
| Peak resident memory | 1.71 |
| `-c pass` startup | 0.74 (19.2 ms against 25.9 ms) |
| Importing `json`, `re`, `collections`, `dataclasses` | 1.11 time, 1.38 memory |

All the work below is merged (PRs #86 to #99, October 23). A PGO build of
`main` at `1291398f` (everything but the last three, #97, #93 and #94),
measured head to head with the `4a2ca7f` baseline in one session:

| Measure | Baseline | Merged |
| --- | ---: | ---: |
| Warm time against CPython (geomean) | 1.276 | 1.203 |
| Cold time against CPython | 1.506 | 1.437 |
| Peak memory against the baseline | 1.000 | 0.960 |
| Instructions against CPython (quick build) | 1.318 | about 1.25 |

The warm time is 0.951 of the baseline's: `closures_decorators` 0.62,
`async_tree` 0.77, `coroutines` 0.81, `context_managers` 0.88,
`exceptions` 0.88, `logging_mix` 0.91, `nqueens` 0.92, `sudoku` 0.93.
CPython's own peak memory moves between sessions (the baseline measured
1.71 times it on October 20 and 1.79 on October 23), so memory is compared
with the baseline directly. #97 takes the 26 call micro-ops' geomean from
1.41 to 1.26 times CPython, and #94 a direct call about 88 instructions
lower (`go` 0.96).

Eight workloads beat CPython's warm time (`pickle_roundtrip` 0.57,
`json_roundtrip` 0.69, `decimal_telco` 0.71, `scimark` 0.72). The worst
are `context_managers` 2.11, `async_tree` 1.99, `stdlib_utils` 1.95,
`deepcopy` 1.93, `mini_interpreter` 1.65, `tomllib_loads` 1.61, and
`logging_mix` and `go` 1.57.

Commands: `tools/pybench/run.py --weavepy BIN` for time and memory; see
`tools/pybench/README.md`.

## Where the gap comes from

A callgrind sweep of all 33 workloads (one steady-state call each, a Linux
`quick` build of `4a2ca7f`) attributes WeavePy's instructions to functions.
In units of CPython's whole instruction count for the same workload, the
mean is 1.32, split as follows:

| Bucket | Share | Where it's large |
| --- | ---: | --- |
| Python calls and returns (activation binding, switches, frameless leaves) | 0.17 | `go` 0.47, `mini_interpreter` 0.47, `deepcopy` 0.46, `raytrace` 0.41, `closures_decorators` 0.41 |
| Compiled code itself (frame JIT, tier 2) | 0.14 | `go` 0.55, `raytrace` 0.38 |
| Container operations (subscripts, iteration, comparisons) | 0.12 | `nqueens`, `sudoku`, `unpack_sequence` 0.36 |
| Native libraries (regular expressions, SQLite, decimal, JSON) | 0.12 | |
| Reference counting | 0.10 | |
| Allocation | 0.09 | |
| Attribute and method lookup | 0.08 | `mini_interpreter` 0.27 |
| Core loop dispatch (code the JIT doesn't cover) | 0.07 | `stdlib_utils` 0.40, `class_creation` 0.27, `logging_mix` 0.20 |
| Generators and coroutines | 0.06 | `coroutines` 0.66, `generators_tree` 0.61, `nqueens` 0.27 |
| Cycle collector | 0.04 | `async_tree` 0.35 |
| Everything else, spread over hundreds of functions | 0.25 | |

The profile is flat: no function other than the core loop takes more than
4% of the suite. Two structural facts explain most of it:

- **A call costs about five times CPython's.** A direct call between two
  compiled functions retires about 480 instructions in its helper alone
  (`frame_jit::h_call_direct` on `go`, 27% of that workload), against about
  100 for CPython's whole call and return. The cost isn't in one place: the
  call binds a pooled `InlineAct`, writes about 15 of its fields, pushes it
  on the core loop's switch, enters the caller on the lazy spine's pending
  list, builds a 17-field `State` for the callee's native code, and on
  return checks, releases, parks, and restores all of that. Because
  activations are expensive, the VM grew parallel cheaper paths (frameless
  pure leaves, site leaves, constructor lanes, builtin lanes), each hot at
  once, which is what makes WeavePy's hot code footprint 2.8 times CPython's
  and its L1 instruction-cache misses five to seven times CPython's.
- **Compiled code calls helpers for most operations.** The frame JIT emits
  a call to an out-of-line helper for attribute loads and stores, method
  loads, subscripts, iteration, comparisons, and even reference-count
  increments (`h_clone`), so a compiled operation still costs a call and the
  helper's generic checks.

Memory has its own causes: the JIT costs about 2.5 MiB even for `-c pass`
(12.6 MiB against 10.1 MiB without it), an instance takes 128 bytes plus a
separately allocated block for its attribute values, and every container
slot is a 16-byte `Object`.

## Plan

Ranked by expected gain on the warm-time geomean. Each item is its own
branch and pull request.

1. **One cheap activation protocol.** Done for compiled callers and callees
   (see Done: direct calls on per-thread records). Left: route the core loop's
   inline calls through records, and delete the frameless-leaf machinery
   (`leaf_plan`, site leaves) once native callers (`sorted`'s keys) no longer
   need it.
2. **Generators and coroutines on the same records.** A resume links the
   generator's own record and a yield unlinks it. Expected: 3% to 4%;
   `coroutines`, `generators_tree`, `async_tree`, and `nqueens` 15% to 30%.
3. **In-line JIT code for the hottest helper operations.** Reference-count
   updates, attribute loads and stores by class version and slot, method
   loads, integer and float comparisons that branch, and list, tuple, and
   dict subscripts. Expected: about 5%.
4. **Instance layout.** Store attribute values in the instance's own
   allocation and shrink `PyInstance`. Expected: 2% to 3% time, 5% to 10%
   peak memory on object-heavy workloads.
5. **Memory.** The JIT's fixed cost, code-object size, and the collector's
   per-object bookkeeping. Target: peak memory under 1.4 times CPython's.
6. **The cycle collector.** `async_tree` spends a quarter of its time in
   young collections.
7. **Core loop dispatch** for code the JIT doesn't cover (`stdlib_utils`,
   `logging_mix`, `class_creation`).

## Done

- **One bind for every direct call** (PR #86, `perf/cheap-calls`). A call
  between compiled functions, positional or keyword, with defaults or
  cells, binds through one path (`frame_jit::direct_enter`) into an
  ordinary `InlineAct`; `core_bind_kw_direct`, `bind_call_cells` and the
  `DIRECT` variant of `core_bind_plain` are gone. Instructions are within
  0.3% of before. This replaced a first attempt, light activations that
  skipped the switch and the pending list (`go` 0.94 of before), because
  anything that switches activations had to reify them first: that kept
  constructors, generator resumes and builtin lanes off them, needed a
  frame swap and 15 reify points, and borrowed code a reassigned
  `__code__` could free mid-call.
- **In-line subscripts of stack-held sequences and closure-cell reads**
  (PR #87, `perf/jit-coverage`). Instructions: `nqueens` 0.958,
  `sudoku` 0.985, `generators_tree` 0.990.
- **Cheaper exceptions** (PR #88, `perf/exceptions`). An unwound frame's
  object is built only when something reads it, a raise settles its
  handler and class checks once, and exceptions are built with their
  slots in place. A raise caught one frame up costs 6,275 instructions,
  down from 8,179 (CPython 2,984). Instructions: `exceptions` 0.901,
  `context_managers` 0.979, geomean 0.992.
- **Cheaper young collections** (PR #89, `perf/gc-young`). A static
  visitor in place of a `dyn` closure, no reference counting during the
  walk, direct walks of generator frames and futures, one fewer pass.
  The collector's cost on `async_tree` fell about 43% (the workload 0.92);
  geomean 0.994.
- **Cheaper generator creation, `send` and teardown** (PR #90,
  `perf/gen-lifecycle`). `send` takes the fast step, finished frames and
  generator objects are recycled whole. `send` costs 1,243 instructions,
  down from 1,963 (CPython 985). Instructions: `coroutines` 0.853,
  `generators_tree` 0.976, geomean 0.991.
- **Chunked frame-JIT compiles** (PR #91, `perf/jit-memory`). A body that
  lowers to more than 2,400 IR instructions compiles in chunks, cut where
  the fewest loops cross (weighted by nesting) and chained without the
  core loop. Peak RSS falls by 1 to 4 MB (`tomllib_loads` 31.6 to 27.8
  MB), the suite's RSS ratio from 1.773 to 1.725 on a quick build, at
  neutral warm time (0.995). Even-sized chunks cut through loops and cost
  `tomllib_loads` 6.5%: a crossing costs about 100 cycles.
- **Frameless container helpers** (PR #92, `perf/subscripts`). The
  commonest subscripts, item stores and `in` tests run through small
  helpers picked in line by tag, shared by compiled code and the core
  loop. `d[str]` costs 169 instructions, down from 217 (CPython 175);
  `defaultdict` `d[k] += 1` 681, down from 1,257 (CPython 714).
  Instructions: `sudoku` 0.892, `nqueens` 0.958, geomean 0.988. It adds
  about 1,100 lines.

- **RFC 0078 stage 1** (PR #93, `perf/activation-stack`). A frame's
  operand stack and locals are types of their own (`frame_slots.rs`):
  `FrameStack` with the operations frames use, `FrameLocals` with each
  way the VM reaches the locals named (`shared()` for frame objects and
  shells, unguarded `slots()` and `as_mut_ptr()`, and `storage()`, the
  vector itself, at 29 binding sites that stage 2 replaces). Neutral
  (instructions 0.997).

- **RFC 0078 stage 2: lazy direct calls** (PR #94, `perf/activation-records`,
  stacked on #93 and #86). A direct call's callee is bound but not
  registered with the core loop or the spine until its native code leaves
  for the core loop, a helper uses the switch for it, or something links a
  newer activation onto the spine or reads it (`register_lazy`,
  `Interpreter::settle_lazy`). About 88 instructions fewer per direct
  call: `go` 0.960, `tomllib_loads` 0.982, geomean 0.997.

- **`functools.lru_cache`** (PR #95, `perf/lru-cache`): state in the
  wrapper's slots, hits answered at the call site, misses run as ordinary
  calls with a pending store. A hit costs 614 instructions against
  CPython's 859; `closures_decorators` 0.690.
- **Library and compile paths** (PR #96, `perf/stdlib-paths`): bound-method
  expansion, class attributes read through instances cached at the site,
  never-run blocks left to the core loop, `isdisjoint`, struct sequences,
  `str()` with a Python `__str__`. `logging_mix` 0.943, `tomllib_loads`
  0.952.
- **Call shapes** (PR #97, `perf/call-shapes`): one fill for extended
  parameters, one by-name keyword binder, `partial` inline, static and
  class methods cached. The 26 call micro-ops' geomean goes from 1.42 to
  1.20 times CPython.
- **`with`, locks and asyncio** (PR #98, `perf/with-async`): lock methods
  on the lock types, the running task per thread, `del obj.attr` and
  `LOAD_SPECIAL` cached and compiled, native-method load caches, idle
  collections pacing the older generations. `async_tree` 0.864,
  `context_managers` 0.907.

**Merged** (October 23). The agents' PRs went first, then the RFC 0078
stack (#86, #93, #94) on top of them. CI's bench gate caught two
regressions the workload suite doesn't cover, both fixed before merging:
`dict_ops` 1.3 to 1.4 times slower in #96 (never-run blocks pruned in code
a loop made hot mid-call; pruning now applies only to code compiled from
call heat) and `call_overhead` 2.2 to 2.6 times slower in #97 (charging
keyword calls to tier 2's call budget retired mixed loops to tier 1; that
commit was reverted). #99 pins python-dateutil in its ecosystem selftest,
whose 3.0.0 release broke every run's shard 2 of 3. Merging #97 onto #86's
single bind path took a port of its extended-parameter binding into
`direct_enter`.

**Checkpoint, PRs #86 to #92 merged together** (PGO, October 21, before
#91's cost-aware cuts): against CPython, warm 1.24 to 1.27 (two runs),
cold 1.45, peak RSS 1.62 (from 1.276, 1.506 and 1.710). Head to head with
`main`'s PGO build in one session the warm geomean is 0.991; wall time on
this host moves by several percent between runs, so only the instruction
counts (about 0.95 of `main`) and RSS are firm. `tomllib_loads`' 12%
slowdown in that run was the even-sized chunks, since fixed.

- **PR #101, `perf/beat-cpython-everywhere`** (October 2026). Instructions
  1.245 to 1.157 times CPython's (0.929 of the base); the largest moves
  are `sqlite_queries` 1.39 to 0.92, `coroutines` 1.45 to 1.13, `sorting`
  1.26 to 1.03, `mini_interpreter` 1.78 to 1.50, `stdlib_utils` 2.16 to
  1.86, `deepcopy` 1.70 to 1.49, `raytrace` 1.36 to 1.18, `chaos` 1.11 to
  1.00. In order of effect:
  - *Lazily awaited coroutines.* `await f(...)` of a plain coroutine
    function runs its body as a lazy direct call from a parked frame box;
    the coroutine object is made only if the body suspends, raises or is
    observed (`frame_jit::lazy_await`, `Interpreter::lazy_coro_materialize`).
    An await costs about 900 instructions against CPython's 1,007 (2,093
    before).
  - *Native libraries.* Every prepared sqlite3 statement is cached (a
    cursor used to finalize its own, so `executemany` reparsed per row);
    tuple sort keys, bisect and heapq compare in place; base64 a group at a
    time; regex alternatives that can't start are skipped; one-pass
    printf flags; ASCII translate; struct and int/bytes without staging;
    bytes keys stay native in dicts and `lru_cache`.
  - *Calls.* Leaf methods run in place from compiled code at about
    CPython's cost (`h_leaf_method`); leaf callees run in place at every
    frame-JIT site; keyword calls collect `**kwargs` and `dict(k=v)` from
    the names tuple; spread and empty-`**kwargs` forwarding calls bind as
    direct calls; `type(x)` and `LIST_EXTEND` stay in compiled code;
    callees of compiled code compile after about 500 switched calls.
  - *Construction.* Proven store-only classes construct from the call
    site (`h_call_ctor`); a Python `__new__` and its `__init__` run as
    inline activations; native methods called through their class, `super()`
    sites and `object.__new__` take lanes.
  - *Attributes and dunders.* Python `__getitem__`, comparisons and binary
    operators are called directly from compiled code; class attributes are
    read through instances in line; the class leaf-attribute cache places
    keys independently of build layout, and its special-method keys no
    longer evict each other; the type cache reads without a borrowed
    thread-local; always-true instances answer truth tests in line.
  - *Tier 2.* No compiles that run nothing natively; pair loops of
    objects with little typed work go to the frame JIT (scalar pairs and
    arithmetic-heavy ones stay); repeatedly failing entries hand calls
    back; cheaper string index, tuple builds and sequence steps.
  - *Generators.* A generator expression's generator is made at its
    `MAKE_FUNCTION`; `itertools.chain` of sequences drains in one pass.
  - *Memory and start-up.* mimalloc maps its 4 MiB pages and large objects
    from the OS (peak RSS 1.694 to 1.648 times CPython's on a quick build;
    `async_tree` rises); regalloc2 is vendored with tighter capacity hints
    (heap peak about 1 MB lower on JIT-heavy workloads); the code
    generator warms when the first code gets halfway hot (`-c pass` peak
    RSS 14.0 to 11.4 MB).
  - *Correctness, found along the way* (each with a regression test): a
    tier-2 constructor that skipped a Python `__new__`; `object.__init__`
    and `object.__new__` checks called unbound; built-in subclass reprs in
    containers; a class's truth ignoring its metaclass; late `__del__` after
    attribute reads and truth tests; tier-2 pins outliving weak references;
    a warm pure leaf given a surplus positional; a cached class attribute
    that was an instance's shadowing value (`test_tarfile`); compiled list
    `+=` building a new list. `tests/regrtest/test_override_fast_paths.py`
    and `test_value_fast_paths.py` are differential harnesses that run
    overrides and values hot under five JIT settings against CPython's
    output.
- **PR #102, `perf/step-change`** (October 2026), stacked on PR #101.
  Cold time 0.923 of PR #101's, warm 0.987, instructions 0.990, peak
  memory 1.011 (PGO, one session):
  - *Background code generation.* The frame JIT lowers a hot body on the
    interpreter's thread and queues the Cranelift function for one worker
    thread, which generates the machine code while the interpreter keeps
    running the body; the interpreter installs finished code at its
    warm-up checks (`frame_jit/queue.rs`). The queue holds at most 8 jobs
    or 24,000 IR instructions, `fork` waits for the worker to finish and
    exit, and `WEAVEPY_JIT_SYNC=1` compiles synchronously (the VM's unit
    tests and the regression tests that force early compiles set it). A
    body of 160 or more instructions is lowered lean at once instead of
    after a full lowering that always came out too large.
  - *Lazy generator resumes.* A compiled generator body resumed from
    compiled code or the core loop runs from its consumer's native code,
    its activation a record on the native stack, as direct calls run
    (`for`, `next()`, `send()`, `yield from`, `await`); a parked
    `yield from` chain resumes at its innermost generator, and chains up to
    three deep level by level; inlined `list`, `tuple`, `all` and `any` of a
    generator expression drain it in one step; a hot loop-free generator
    body now compiles. Instructions: `generators_tree` 0.836 of PR #101's,
    `nqueens` 0.899, `context_managers` 0.949, `sudoku` 0.962,
    `coroutines` 0.965.
  - *Code layout.* The checked-in order file had gone stale (484 of its
    5,050 names matched, since names embed crate hashes); the host targets
    now link through `scripts/link-ordered.sh`, which remaps the order onto
    each link's own symbols, and the order covers start-up and what 19
    workloads run (`scripts/extend-order-file.py`). On x86-64 Linux, lld
    gets it too (`--symbol-ordering-file`).

- **Direct calls on per-thread records** (`frame_jit/records.rs`). A direct
  call between compiled functions runs on a record from a per-thread stack:
  its `State` and a parked activation slot, kept between calls with their
  per-thread fields and buffers in place. No `State` is rebuilt, no slot comes
  from a pool, and the callee's code takes no count (a `__code__` assignment
  settles first). The function stays in its slot on the caller's stack; the
  recursion guard, the caller's sync and the switch and spine links wait until
  something looks (`records::commit`). A compiled body records its entry on
  its code (`JitHint::set_direct`), so a call site caches nothing and a
  megamorphic site costs what a monomorphic one does. Compiled code enters
  through `h_direct_enter`, calls the callee's native code itself and finishes
  through `h_direct_leave`; a clean return writes nothing to the state
  (`REC_RET`). Constructors run `__init__` on a record, and native bodies, leaf
  and generator spreads leave a lazy callee lazy unless Python code looks. A
  leaf callee compiles and runs through its body when compiled code calls it,
  so `h_leaf_method` is gone. Against the branch it merged (quick builds):
  instructions 0.976 (1.113 to 1.086 of CPython), `go` 0.77, `tomllib_loads`
  0.90, `context_managers` 0.90, `raytrace` 0.93, `mini_interpreter` 0.93,
  `deepcopy` 0.94, `coroutines` and `chaos` 0.95; `collections_mix` 1.03,
  `oo_patterns` and `unpack_sequence` 1.02. Wall time over `context_managers`,
  `deepcopy`, `go`, `mini_interpreter` and `raytrace` 0.856.

- **Small callees in line** (`frame_jit/inline.rs`). A `CALL`, a fused method
  call, an operator method or a property getter whose site settled on a small
  callee lowers its body into the caller, the callee's frame on the caller's
  operand stack, guarded by the callee's code or the receiver's class version.
  Anything that needs a frame builds the real activations and leaves for the
  core loop (`h_inline_exit`, `h_inline_left`), and a site that leaves too often
  turns itself off. `WEAVEPY_FRAME_JIT_INLINE=0` turns it off. On top of records
  (quick builds, off against on): instructions 0.974 (1.054 to 1.026 of
  CPython), warm wall 0.975, whole-process cycles 0.988, peak memory 1.012.
  `raytrace` 0.70 instructions and 0.73 wall, `oo_patterns` 0.95 and 0.87, `go`
  0.86 and 0.91, `chaos` 0.91 and 0.91, `mini_interpreter` 0.89 and 0.95; the
  others are within noise, and `mini_interpreter`'s memory grows 10%. Didn't pay:
  a larger compile budget (320 against 160 instructions a caller), longer bodies
  (100 against 64), and a recursive callee in line at a polymorphic site.

## What the measurements say

- **The gap is spread thin.** A direct call costs about 480 instructions,
  a core-loop call and return about 400, generator creation and teardown
  about 1,200, a dict subscript from compiled code about 200, a young
  collection about 2,000 per object. Line-level profiles of each show no
  single cost: 60 or more steps of 1 to 10 instructions each (field
  writes, flag checks, reference counts, pool and list bookkeeping). Each
  local fix is worth 1% to 5% on a few workloads, well under 1% on the
  geomean.
- **What would move the geomean is a different execution core**: frames
  allocated on a per-thread stack and linked to their callers (no pending
  list, no pools, no separate native `State`), one activation record that
  the interpreter, the frame JIT and generators all share, and a JIT that
  inlines small callees and reads attributes by shape. That's a rewrite of
  `quiet_run`, the core loop's switch, `frame_jit`'s calling convention,
  generators and frame objects, not an incremental change.
- **Tried and dropped**: compiling `DELETE_ATTR` natively (it lets
  `contextlib`'s `__enter__` compile, but the compiled `next(self.gen)`
  costs more than the interpreter's); the single-pass register allocator
  for lean bodies (peak memory 1.6 to 4.6 MiB lower, warm time 3% to 20%
  higher).
- **Memory.** WeavePy's peak RSS is 1.71 times CPython's (PGO). Three
  causes, measured on `tomllib_loads` with an allocation profiler and
  `task_info`:
  - **The frame JIT's compile peak.** The largest single compile's
    working memory, about 1.2 KB per IR instruction (Cranelift's register
    allocator and VCode), adds 4 to 7 MB to the heap peak; the JIT's
    retained data is small (about 230 KB of code). PR #91 bounds it for
    large bodies; bodies of 1,200 to 2,400 IR instructions still set the
    peak.
  - **The binary's touched pages.** 8.3 to 9.9 MiB file-backed against
    CPython's 6.0 to 6.3: a run touches about 8 MiB of the binary's 25 MB
    of code, scattered, since the macOS linker ignores LLVM's hot and cold
    sections. With tier 2 off, start-up touches 2.4 MiB fewer of them.
  - **Code objects.** About 2,100 code objects load for `tomllib_loads`
    (376 bytes each plus their vectors and strings, about 2 to 2.5 MiB);
    most nested functions never run.

  The code generator's warm-up thread costs only about 0.1 MB.
- **The call protocol, measured again.** Frame-JIT direct calls are 85% to
  95% of all calls in the call-heavy workloads (`go` 540,000 per run).
  After stage 2 a direct call still retires about 416 instructions in its
  helpers, spread thinly (19 building the callee's `State`, 20 in the entry
  helper's prologue and epilogue, about 30 of admission checks, about 80 of
  reference counts and moves). Tier 2 already makes native calls at about
  100 instructions with frameless activations; stage 3 (compiled code
  guarding, binding and entering a warm callee in line) is the remaining
  structural step. The frameless leaf paths cost 490 (pure) to 1,040
  (effect) instructions a call, no less than an activation, but turning
  them off costs 1.25 to 1.7 times as much where native code calls them
  (`sorted`'s keys, `oo_patterns`), so they stay until such calls are
  cheap.
- **The worst workloads are dominated by their own library paths**, not
  calls: in `closures_decorators` the native `functools.lru_cache` is 17%
  of the instructions; decorator wrappers' `f(*args, **kwargs)` costs 5.2
  times CPython's and `functools.partial` 6.2 times.
- **Tried and dropped: a linker order file.** Ordering the binary's
  functions by how many training programs run them (start-up code first)
  cut file-backed pages by 0.4 to 0.6 MiB and left warm time unchanged
  (0.997), less than a simulation predicted (1.4 MiB), since the kernel
  reads file pages in clusters and the 20 MB of constants don't move. Not
  worth a second training pass in the PGO build. Ordering by total
  profile counts made it worse (large hot functions first).

- **RFC 0078 stage 3 was measured and dropped.** A lane for a warm site's
  callee (`h_call_warm`, positional arguments, no defaults or cells) costs
  447 instructions a call against the general path's 455: removing
  branches buys nothing, since the cost is the work (guards 34, entry and
  decode 26, spills 23, binding 52, `State` 31, `direct_done` 33,
  moves and reference counts about 120, locals 42, `strong_word` 25). A
  full compiled entry would leave about 330 (roughly 2% on `go`) for 150
  to 200 more IR instructions at every call site, and wouldn't reach lean
  bodies' calls (267,000 per `go` call) or recursive ones (150,000).
- **What a direct call costs on records.** A call and return of a one-argument
  function costs about 210 instructions over the same statements in line
  (CPython about 165), against about 540 before; `fib` 315 a call (CPython
  481). What's left, roughly: the entry helper 70 (guards 25, argument moves,
  the frame's handles and the call's notes), the return helper 50 plus 8 a
  local, the callee's prologue and epilogue 18, and the calling code's moves
  and spills about 40. CPython moves a call's arguments into the new frame too,
  so arguments in place wouldn't reach its layout: they'd save 4 instructions
  an argument, and a contiguous stack for locals needs every path that grows or
  takes a frame's vectors to change. It wasn't built. A call that registers
  costs far more, and each registers its whole chain of callers: an instruction
  native code leaves to the core loop does that (`Rect.__init__`'s polymorphic
  attribute stores, `DictUpdate`, `PushExcInfo`), as does a
  generator drained for a spread (`deepcopy`'s `callable(*args)`, with the
  callers it registers about 14% of its direct calls). `WEAVEPY_BURST_STATS` names the site of each registration.
- **Most of `deepcopy`'s gap isn't the call protocol.** Copying a small nested
  list cost 42,000 instructions against CPython's 27,200, and records took off
  only 2,000. Where the rest goes wasn't attributed.
- **Generators now resume lazily, as direct calls run.** A compiled
  generator body resumed from compiled code or the core loop (`for`,
  `next()`, `send()`, `yield from`, `await`, and a parked `yield from`
  chain's innermost generator) runs from its consumer's native code with
  its activation a record on the native stack, bound and pushed on the
  switch only if something needs the core loop's view; inlined
  `list`, `tuple`, `all` and `any` of a generator expression drain it in
  one fast step. A native caller's step of a compiled body (`enumerate`,
  `zip`, an asyncio task's step) now enters its native code from a switch
  made for it, without a stretch of the quiet loop. What remains is thin and
  flat: making, yielding once and exhausting a generator costs about 2,240
  instructions on Linux (CPython's about 1,130), of which creation is about
  750, the two resumes 800, compiled code 290 and teardown 420, each 30 or
  more steps of 5 to 60 instructions.
- **One allocation per generator wouldn't pay in instructions.** A
  short-lived generator's blocks are pooled, so its allocator cost is
  already near zero, and all of `async_tree`'s allocations are 5% of its
  instructions. A suspended generator's five blocks take 390 bytes (CPython's
  250), most of it the 16-byte `Object` slots and the frame's own fields, and
  the locals' `Rc` can't merge while frame objects and shells share it.
- **The collector's cost was its candidates' number, not its visit.** A
  young pass costs about 570 instructions per object (CPython's about 330),
  and `async_tree` spent no more instructions on it than CPython's collector
  does. Its survivors are a live task tree, which each pass scans and frees
  nothing of, so waiting longer after passes that find nothing removes the
  whole cost. Its peak RSS rose from 66 to 75 MB, but that's when the
  background compiles land against the tree's peak, not the objects: with
  the frame JIT off it's 70.7 against 69.6 MB, and with synchronous compiles
  75.7 against 74.3 MB.
- **Compiles are a third of `stdlib_utils`' and `logging_mix`' gap in
  `run.py`'s window.** The frame JIT emits 20 to 35 IR instructions per
  bytecode instruction and Cranelift spends about 11,000 host instructions
  per IR instruction. A single-pass allocator, lean lowering everywhere and
  other thresholds each trade warm time for cold.
- **Background compilation's costs.** It moves about two thirds of a
  compile (Cranelift's code generation) off the interpreter's thread; the
  lowering stays (a third). CPU time rises about 2%. A workload that compiled
  peaked about 2 MB higher, as the worker's free pages stayed resident on top
  of the interpreter's heap. Earlier attempts to collect that heap showed
  nothing because on macOS `MADV_FREE_REUSABLE` lowers the footprint but not
  the resident set, so no purge ever moved peak RSS there. See "Peak memory"
  below.
- **Memory on Linux.** mimalloc's arenas are backed by transparent huge
  pages there (`madvise` mode, the distributions' default), which is much
  of why Linux peak RSS is about 3.6 times CPython's (the CI's `startup`
  fixture peaks at 32 MiB). Memory mapped for one object isn't, so mapping
  large pages from the OS (PR #101's `arena_max_object_size`) made
  `deque_ops` take 6,000 page faults against 770 and run 20% slower; that
  setting now applies off Linux only. Turning THP off for the process
  (`PR_SET_THP_DISABLE`) takes `startup` to 20 MiB but brings the faults
  back (7,500 on `deque_ops`) and is inherited by child processes, so it
  isn't in.
- **Peak memory, where it goes.** The profiler (`--features alloc-profile`,
  now with `<file>.vm` and `<file>.classes`: the regions, and mimalloc's
  resident bytes by thread and size class, at the resident set's high-water
  mark) on `go`, before and after the memory work, in MB:

  | | Before | After | CPython |
  | --- | ---: | ---: | ---: |
  | Peak resident set | 24.5 | 20.8 | 14.5 |
  | File-backed (binary, shared libraries) | 9.3 | 9.8 | 6.3 |
  | Dirty (the footprint) | 15.1 | 11.0 | 8.2 |
  | mimalloc resident at the peak | 12.6 | 8.1 | |
  | Interpreter's live heap | 7.7 | 5.9 | |
  | Interpreter's free blocks in its pages | 1.9 | 1.0 | |
  | Background compiler's free pages | 3.1 | 0.1 | |
  | Other dirty (`__DATA`, thread stacks, JIT code, system `malloc`) | 2.5 | 2.4 | |

  ("Before" is the tree before this work and its frame-JIT rewrite, which
  halved the IR a compile takes.) Over the suite on a quick build, this work
  takes peak RSS from 1.578 to 1.423 times CPython's, on the tree with that
  rewrite, at 1.010 of the instructions.

  What changed it:
  - *Function bodies decode on first read.* A code object is a header and a
    body behind `Deref`; a cached module's nested bodies stay encoded until
    something reads them, and the code cache's mapping is read through a
    second mapping removed afterward. The harness's imports fell from 15.3 to
    13.6 MB (CPython 13.2), since most functions a module defines never run. A
    stub costs 160 bytes, down from 384 plus its vectors. `Deref`'s check is
    about three instructions at each of a few million reads a call of
    `coroutines` (1.7% there, 0.6% on the geomean).
  - *Compile memory has an arena of its own.* The worker and compiles on the
    interpreter's thread allocate there, the worker returns it to the OS after
    10 ms idle, and the interpreter's first arena declines the purges this
    forces. Forcing purges of the first arena as well, after every compile,
    made `stdlib_utils` 30% slower warm on Linux, as its free ranges faulted
    back in. Purged ranges are mapped over on macOS, the only way to take them
    out of the resident set at once.
  - *Smaller things.* Thread-local storage per thread fell from 53 to 18 KB
    (a hash memo and specialization statistics are allocated on first use),
    `tracing` isn't set up without `WEAVEPY_LOG`, and macOS's arenas purge
    after 50 ms instead of a second.

  What's left over CPython on `go` is about 3.5 MB of file-backed pages (the
  interpreter's touched code; JIT off barely lowers it) and 2.8 MB of
  dirty memory, about 0.8 MB of it the frame JIT's. mimalloc's options (retained
  pages, commit on demand, arena size, page map commit) don't move the peak.
  What remains of the heap is thin: `PyFunction` (216 bytes, about 1,300 to
  3,300 of them), code stubs, the VM's per-code extension (672 bytes), the
  type cache and the collector's tracked list are 100 to 300 KB each.
- **Tried and dropped in the memory work**: delaying inline cache storage until
  a code object's second activation or a run of writes (0.8% less RSS, but
  `closures_decorators` 56% more instructions: compiles and leaf plans read
  what the caches hold when they're made); frame-JIT chunks of 1,200 IR
  instructions (2.7% less RSS, 1.1% more instructions, `unpack_sequence` 18%);
  a queue bound of one job and tier-2 compiles waiting for the worker (no
  change); mapping blocks of 128 KiB or more one by one (no change except
  `float_points`); LLVM's hot-cold splitting without a profile (the code
  barely changes); `MADV_RANDOM` on the binary (this macOS has no fault-around:
  a file-backed page in the resident set was touched).
- **Tried and dropped in PR #102**: tier-2 compiles in the background
  (while a constructor's compile was pending, interpreted constructors
  built instances in another shape and the compiled loop side-exited on
  them: `float_points` 25% to 35% slower); decoding instructions lazily
  from the mapped code cache (0.6 MB less heap, 0.5 MB more file pages,
  0.9% more instructions); a lower frame-JIT threshold (`class_creation`
  8% slower cold, 6.5 MB more memory); resuming `yield from` chains of any
  depth level by level (O(depth) work a value: `generators_tree` 4.4
  times CPython's).
- **Tried and dropped in PR #101**: instance values in the instance's own
  allocation (the object-heavy workloads hold 300 to 650 live instances at
  their peak); mimalloc v2 (mixed, instructions up); opt-level `s` for
  Cranelift (compiles 4% to 7% slower); an indexed young set for
  generators (below noise).

## Next

1. Generators: `async_tree` and `context_managers` are now mostly general
   calls, attribute stores and asyncio's own loop, not generator mechanics.
   `next(map(next, [g]))` loses a finished generator's `StopIteration` value,
   and `gc.callbacks` doesn't fire for automatic collections.
2. Cold time: the lowering left on the interpreter's thread (a third of a
   compile) and the time a body runs interpreted before its code is
   ready; a much cheaper baseline compiler or compiled code kept across
   processes.
3. Peak memory: the file-backed pages (3.5 MB over CPython's: the
   interpreter's code is spread over many large functions, so hot-cold
   splitting with a profile is the next thing to try), and dropping a body
   again when its code stopped running (bodies, VM extensions and inline
   caches of code run at import are about 2 MB).
4. Builtins take keywords as `&[(String, Object)]`, so every keyword call
   of one allocates a `String` per keyword; a calling convention over the
   names tuple would remove it.
5. Tier 2 holds an object that already has weak references, read through
   an attribute, until its activation exits.

## Measurement notes

- Instruction counts (`/usr/bin/time -l`, or callgrind in the `wp-cg:latest`
  container) guide the work; wall time on a PGO build decides it. Pin
  `PYTHONHASHSEED=0` and unset `PYTHONDONTWRITEBYTECODE` for whole-process
  timings.
- Callgrind counts each iteration of `rep stos` as an instruction, so it
  overstates `memset` (zlib's state setup in `bytes_codecs`), and it shifts
  the JIT's time-budgeted compiles into later calls (`stdlib_utils`,
  `logging_mix` show compilation in steady state there, but not on macOS).

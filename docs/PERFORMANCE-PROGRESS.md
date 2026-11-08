# Performance progress

This note tracks the work to make WeavePy significantly faster than CPython
3.14 on realistic workloads, measured by wall time with a profile-guided
build, plus peak memory and startup. It records where WeavePy stands, the
plan, what's done, and what's next, so a new session can pick up the work.

## Where WeavePy stands

Measured on November 8, 2026, at `1cef5111` (PR #107), a PGO build against
CPython 3.14 on the same host, interleaved in one session: warm time 0.885,
cold time 1.031, peak memory 1.258. Since November 6's 0.916 the warm time
moved most on `exceptions` (1.02 now), `class_creation` (1.04),
`stdlib_utils` and `tomllib_loads` (1.05 and 1.06). The worst warm times
are `async_tree` 1.50, `context_managers` 1.19, `deepcopy` 1.13,
`logging_mix` 1.11 and `closures_decorators` 1.10; the best `scimark`
0.51, `pickle_roundtrip` and `unpack_sequence` 0.59, `float_points` 0.62
and `raytrace` 0.64. On release builds (not PGO) the suite took 0.981 of
`a60298cb`'s cycles.

Measured in the evening of November 6, 2026, at `e6cd488b` (PR #107), a PGO
build against CPython 3.14 on the same host: warm time 0.916, cold time 1.033,
peak memory 1.268, instructions 0.940 (on a release build). Since `02c5de6e`
`class_creation` moved from 1.31 to 1.05 warm and 1.60 to 1.30 cold, as `exec`
reuses the code `dataclasses` and `namedtuple` compile, and compiles cost less
throughout. The worst warm times are `async_tree` 1.52, `context_managers`
1.22, `tomllib_loads` 1.21, `deepcopy` 1.19, `stdlib_utils` and `logging_mix`
1.17; the worst cold times `async_tree` 2.02 and `stdlib_utils` 1.55. With
in-line string compares and indexing (`289c1dd8`) the next PGO run read warm
0.917, cold 1.022 and peak memory 1.278, `tomllib_loads` 1.09 warm.

Measured later on November 6, 2026, at `02c5de6e` (PR #107), a PGO build
against CPython 3.14 on the same host, interleaved in one session: warm time
0.934, cold time 1.049, peak memory 1.278. Since `d07a0969` the moves are
`context_managers` 1.40 to 1.25, `logging_mix` 1.23 to 1.16, `deepcopy` 1.21
to 1.17, `oo_patterns` 1.02 to 0.95 and `chaos` 0.89 to 0.84. The worst warm
times are `async_tree` 1.61, `class_creation` 1.31, `context_managers` 1.25,
`tomllib_loads` and `deepcopy` 1.17, `logging_mix` 1.16 and `stdlib_utils`
1.14.

Measured on November 6, 2026, at `d07a0969` (PR #107), a PGO build against
CPython 3.14 on the same host, interleaved in one session: warm time 0.948
(geomean of 33 workloads), cold time 1.059, peak memory 1.262, and 0.961 of
CPython's instructions. Since November 4's `11c639e5` (0.994 warm, 1.125
cold) the worst have moved most: `exceptions` 1.30 to 1.10, `stdlib_utils`
1.31 to 1.15, `logging_mix` 1.36 to 1.23, `deepcopy` 1.35 to 1.21 and
`class_creation` 1.43 to 1.34. The worst warm times are now `async_tree`
1.61, `context_managers` 1.40, `class_creation` 1.34, `logging_mix` 1.23,
`deepcopy` 1.21, `tomllib_loads` 1.17, `stdlib_utils` 1.15 and `nqueens`
1.15; the best are `pickle_roundtrip` 0.57, `scimark` 0.61,
`unpack_sequence` 0.62, `json_roundtrip` 0.67 and `raytrace` 0.68. Peak
memory is highest where many instances live (`async_tree` 1.60,
`generators_tree` 1.50, `class_creation` 1.44): an instance with three
attributes takes about 200 bytes against CPython's 130, its values a block
of their own beside a 112-byte `PyInstance`.

Measured later on November 4, 2026, at `11c639e5` (PR #107) with the
iterator drain on top, a PGO build against CPython 3.14 on the same host,
interleaved in one session: warm time 0.994 (geomean of 33 workloads), cold
time 1.125, peak memory 1.252, and 0.987 of the morning's PGO build's warm
time (`unpack_sequence` 0.88, `exceptions` 0.90, `context_managers` 0.93,
`sudoku` 0.94). The worst warm times are `async_tree` 1.62,
`context_managers` 1.46, `class_creation` 1.43, `logging_mix` 1.36,
`deepcopy` 1.35, `stdlib_utils` 1.31 and `exceptions` 1.30; the best are
`pickle_roundtrip` 0.58, `scimark` 0.60, `unpack_sequence` 0.62,
`json_roundtrip` 0.67 and `raytrace` 0.71.

Measured on November 4, 2026, at `fde9136e` (PR #107), a PGO build against
CPython 3.14 on the same host, interleaved in one session: warm time 1.001
(geomean of 33 workloads), cold time 1.117, peak memory 1.288. Against the
same day's PGO build of `68eb753c` it reads 1.010 with every workload's
instructions unchanged (the day's fixes are paths the suite barely runs), so
the difference is layout noise. The worst warm times are `async_tree` 1.59,
`context_managers` 1.54, `class_creation` 1.42, `stdlib_utils` 1.39,
`deepcopy` 1.38, `logging_mix` 1.37 and `exceptions` 1.36; the best are
`pickle_roundtrip` 0.59, `scimark` 0.62, `json_roundtrip` 0.66,
`unpack_sequence` 0.68 and `decimal_telco` 0.73. Instructions alone don't
explain the worst: `oo_patterns` retires 1.05 times CPython's instructions
and takes 1.18 times its time, `stdlib_utils` 1.20 and 1.39.

Measured on November 3, 2026, at `197a820f` (PR #107, `perf/beyond-parity`),
on the same host and CPython, both WeavePy binaries profile-guided and
interleaved with CPython in one session. The base is `main`'s PGO build
(`24afc77b`, PR #102).

| Measure | `main` | PR #107 | PR #107 / `main` |
| --- | ---: | ---: | ---: |
| Warm time, 33 workloads (geomean) | 1.023 | 1.015 | 0.992 |
| Cold time (first call, including compilation) | 1.211 | 1.134 | 0.937 |
| Peak resident memory | 1.492 | 1.275 | 0.855 |

The JITs generate code through `weavepy-codegen`, WeavePy's own one-pass code
generator, in place of Cranelift (see "Done"). Peak memory falls on every
workload, by 2 to 3 MB on most. The worst warm times are `class_creation`
1.74, `context_managers` 1.57, `async_tree` 1.47, `deepcopy` 1.43,
`logging_mix` 1.39, `stdlib_utils` 1.36 and `exceptions` 1.32; the best are
`pickle_roundtrip` 0.58, `scimark` 0.65, `json_roundtrip` 0.66, `unpack_sequence`
0.73 and `raytrace` 0.74. `class_creation` read 1.17 times `main`'s in this run
though its instructions are even. On the CI bench gate's fixtures the PR's
suite geomean is 0.912 of its merge base's on Ubuntu.

The earlier checkpoints follow.

Measured on October 30, 2026, at `24afc77b` (PR #102, `perf/step-change`,
stacked on PR #101), on the same host and CPython, both WeavePy binaries
profile-guided and interleaved with CPython in one idle session. The base is
PR #101's PGO binary (`ba518cf0`).

| Measure | PR #101 | PR #102 | PR #102 / PR #101 |
| --- | ---: | ---: | ---: |
| Warm time, 33 workloads (geomean) | 1.089 | 1.001 | 0.919 |
| Cold time (first call, including compilation) | 1.334 | 1.191 | 0.893 |
| Peak resident memory | 1.577 | 1.432 | 0.908 |
| Instructions (quick build) | 1.156 | 1.037 | 0.897 |

A second suite run in the same session measured warm 1.011, cold 1.169 and
peak memory 1.463, so warm time is at parity with CPython's within this
machine's noise, not past it. Against PR #101, warm time moved most on `go`
and `raytrace` 0.69, `deepcopy` 0.73, `tomllib_loads` 0.76, `generators_tree`
0.81, `chaos` 0.82, `coroutines` 0.83 and `context_managers` 0.84. Sixteen
workloads beat CPython's warm time in that run (`pickle_roundtrip` 0.56,
`decimal_telco` 0.58, `oo_patterns` 0.63, `scimark` 0.65, `json_roundtrip`
0.68, `raytrace` and `unpack_sequence` under 0.75), though single workloads
moved by up to 40% between the two runs (`oo_patterns` 0.63 and 1.04). The worst
are `async_tree` 1.67, `context_managers` 1.66, `stdlib_utils` 1.50, `nqueens`
1.49, `logging_mix` 1.44, `class_creation`, `deepcopy` and `exceptions` 1.41.
Some workloads read slower than PR #101 in that run (`decimal_telco` 1.22,
`mini_interpreter` 1.15, `closures_decorators`, `nqueens` and `sorting` 1.09),
though their instructions are lower or within 1% and `decimal_telco` read 0.58
and 0.72 of CPython's in the two runs, so single wall-time ratios here are noisy.
Linux's peak memory is higher than these macOS numbers (see "Memory on Linux").

The earlier checkpoints follow.

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

- **f-strings compiled, frameless property setters, and big ints in
  compiled arithmetic** (PR #107). Instructions on Linux.
  - The frame JIT runs `CONVERT_VALUE`, spec-less `FORMAT_VALUE` and
    `BUILD_STRING` itself, through the core loop's own arms. The compiler
    turns `"%s:%s" % (a, b)` into those instructions, as CPython's does,
    and a body holding one had too many exits to compile, so it ran in
    the core loop at every call. `Shape.describe` went from 3,468 to 1,419
    (CPython 3.13: 1,369) and `oo_patterns` 4% fewer. The suite's
    loop-free bodies now have no instruction the native code hands back.
  - A property whose Python setter is the setter shape (`self._x =
    value`) runs it frameless: its one store either happens or doesn't, so
    a decline leaves nothing to undo. `s.width = s.width + 1` went from
    3,173 to 2,081 (CPython 1,379).
  - A product, sum or difference of machine ints that leaves the word,
    and arithmetic on big ints, run the bignum arithmetic in place, in the
    core loop and in compiled code, where they used to leave for the
    general handler (`statistics`' exact ratios: 4.6% fewer).
  - A class whose `__init__` has no store-only shape skips that proof at
    every construction once refuted.
  - Compiled arithmetic takes a `bool` operand as its int value in place
    (`&`, `|` and `^` of two bools stay the core loop's, which keeps them
    a `bool`). It used to leave for the core loop: `getattr(s, "r", 0) +
    hasattr(s, "_w")` went from 1,860 to 1,536.
  - `zlib.compress` keeps its compressor between calls with the same
    level and window, and resets it (`deflateReset`, the same stream a
    fresh one writes) instead of allocating and zeroing a new one's
    tables, which for a short input took more than the compression:
    `bytes_codecs` 4% faster (warm time on macOS; valgrind, which counts
    the zeroing a byte at a time, put it at 15%).
  - A native caller's callback into a compiled function (`map(f, xs)`, a
    sort key) enters the body's native code straight from its lean frame,
    with an activation switch made for it, as a native caller's generator
    step already did, instead of through a burst of the core loop. An
    adapter also skips its pure-leaf attempt, and the item's peek, for a
    callback whose code isn't a leaf. For `map(f, xs)` with an `f` that
    isn't a leaf, that's 3,905 to 3,358 instructions an item (CPython
    3.13: 1,191), and `statistics` 3% fewer.
  - Measured and left: those callbacks still cost 2.8 times CPython's,
    mostly in the adapter's step and the lean frame around the call. Running
    a compiled body with no switch at all stops at its first call, which
    needs one to raise through.
  - A property whose Python setter is one store (`self._w = value`) runs
    in place from compiled code too: where the plain attribute store
    declines, a second helper runs the setter frameless, as the core loop
    already did, instead of leaving the native code for the interpreter.
    A property store costs 1,251 instructions instead of 2,078 (CPython
    3.13: 1,379).
  - A class whose `__init__` takes `*args`, `**kwargs` or keyword-only
    parameters is constructed from compiled code by binding the call
    straight into the `__init__`'s native body, as a plain `__init__`
    already was, instead of through the interpreter's constructor lane.
    `InitVa(i, 2)` with `__init__(self, *args)` costs 2,730 instructions
    instead of 3,582 (CPython 3.13: 1,729); `collections_mix` takes 11%
    fewer instructions (its named tuples' `__new__` lanes) and
    `context_managers` 3% fewer (`suppress(KeyError)`).
  - A regular expression's compiled program is kept on disk beside the
    frozen code (`_sre._cache_get`, keyed by the pattern, its type and its
    flags, invalidated by the regular-expression compiler's sources), so a
    later process skips the pure-Python parser and compiler. A pattern
    that might warn as it parses compiles every time, so its warning still
    shows; a process stores at most 256 and a cache at most 4,096. Imports
    that compile patterns get much cheaper: `decimal` 86 to 69 million
    instructions, `statistics` 112 to 75 million (Linux, CPython 3.13: 46
    and 73 million), and `datetime`, `pathlib`, `statistics` and
    `urllib.parse` together 141 to 102 million. The suite's per-call
    instruction counts leave imports out, so they don't show it.
  - `list(gen)`, `tuple(gen)`, `set(gen)` and `frozenset(gen)` of a fresh
    generator with a simple body, called from compiled code, drain the
    generator's fast steps straight into the new container, skipping the
    constructor lanes between the call and the generator; a set's items
    insert directly while they hash and compare natively. Where the drain
    stops short, what it took goes back in the generator's place (a list,
    or a `chain` of it and the generator) for the ordinary path. Any other
    builtin's collection of a fresh generator (`"".join`, `min`) takes the
    same fast drain first. `set(x for x in ())` costs 3,269 instructions
    instead of 4,141, and `list(genexp)` of eight items 4,606 instead of
    5,497 (CPython 3.13: 2,362 and 4,686).
  - Compiled code runs `RERAISE` itself, as the general loop's arm does
    (the traceback kept, no chaining, a cleanup handler's original raise
    offset back as the frame's line), so an `except` clause that raises
    anew leaves its frame natively instead of through the interpreter. A
    missing attribute that the default lookup certainly misses raises its
    `AttributeError` from the compiled read, without the interpreter's
    resolution passes. A bound method called with a spread tuple or list
    (`f(*args)`, `f = obj.method`) binds its function straight from the
    spread. A caught `AttributeError` costs 3,849 instructions instead of
    6,667 (CPython 3.13: 4,360), the benchmark's `guarded` miss 16,411
    instead of 17,418, and the method spread 1,286 instead of 2,144
    (CPython: 996).
  - `super(C, cls).name` with a class for the receiver (a class method's,
    `__new__`'s, as `fractions` makes its values) resolves in place from
    compiled code, the attribute unbound, cached per site for the
    receiver's version, instead of leaving for the interpreter. An
    operator between two instances of one class calls the forward method
    directly even when the class defines the reflected one too, which
    CPython never tries for operands of one type. A class with a Python
    `__new__` and `object`'s `__init__` is constructed by entering
    `__new__`'s native body directly, and a class called with a spread
    tuple or list (`type(self)(*segments)`) as a `CALL` of the items. A
    generator's body inlines its small callees, as a function's does.
    `Fraction(1, 3) + Fraction(2, 7)` costs 7,625 instructions instead of
    15,591, `Fraction(i)` 4,717 instead of 9,565, and a genexp whose body
    calls a small function 9,672 instead of 10,846 for eight items.
  - A method call of a value an instance holds itself (`self.fn(x)`, a
    function set per instance) remembers the value's position in the
    instance's values under the class's version, instead of resolving the
    name again each time; and `==` or `!=` of two strings in compiled code
    settles without an ordering. `mini_interpreter` takes 1% fewer
    instructions.
  - Measured and left: widening a method site's polymorphic entries from
    three to seven made `mini_interpreter` slightly slower.
  - A class with a Python `__new__` and its own `__init__` (`pathlib`'s
    paths) runs that `__init__` straight from `__new__`'s native return,
    on the call's operands rebuilt, instead of through the core loop's
    continuation; `C(i, 2)` of such a class costs 4,972 instructions
    instead of 5,758.
  - Compiled code runs `WITH_EXCEPT_START` (a `with` block whose body
    raised) as a direct call of the manager's Python `__exit__`, instead
    of leaving for the interpreter at the handler; a bytearray's `+=` of
    bytes extends it in place from compiled code, as the core loop does.
    `with suppress(KeyError): {}[i]` costs 7,286 instructions instead of
    8,416 (CPython 3.13: 5,117), and `bytes_codecs` takes 1.3% fewer.
  - A live frame's `f_lineno` read from compiled code (`logging`'s
    `findCaller` walking its callers) settles in place with the running
    activation's own instruction, instead of through the full attribute
    lookup: `logging_mix` takes 0.2% fewer instructions.
  - A store-only `__init__` may store fresh empty containers too
    (`self.items = []`, `self.index = {}`), so it builds the instance in
    one pass instead of through the leaf evaluator, and a fresh empty list
    is deferred by the cycle collector without its element scan.
    `collections.deque` declares its slots in the order its `__init__`
    sets them, so it lays them out like any slotted class instead of
    keeping a name with each value. `C()` whose `__init__` stores one list
    costs 1,996 instructions instead of 2,602, three stores with a list 2,218 instead of 3,042, `deque()` 3,316
    instead of 4,481 and `contextlib.ExitStack()` 5,630 instead of 6,794.
  - Compiled code reads a field of an instance local in line when its
    site settled on one class (an instance's values laid out over its
    class's, or a laid-out `__slots__` member), and stores over a field's
    scalar in line when the value going in is a scalar or the instance
    is already tracked by the cycle collector, the helpers taking only
    what a guard turns away. Inlined bodies already read fields this way;
    their stores do too now. A store of a function's attribute from a
    local (`wrapper.__wrapped__ = f`, as `functools.wraps` and
    `ExitStack.callback` make it) or a class's no longer leaves for the
    interpreter. `self.x += 1` in a method costs 537 instructions instead
    of 705, `go` takes 5% to 8% fewer, `generators_tree` 3.7% fewer,
    `context_managers` 3% fewer and `mini_interpreter` 1.3% fewer, and
    the closure factory `ExitStack.callback` uses 4,575 instead of 5,968.
    (A store-only `__init__` also records once whether it makes a fresh
    container, instead of asking each call.)
  - A field read of an instance on the stack (`self.board.size`,
    `node.left.value`) is made in line too, the value taking the
    receiver's slot and the receiver's reference going by a plain
    decrement while another owner holds it: `go` takes 1% fewer
    instructions.
  - A constructor's store of a field its class's instances add next
    (the site's `StoreAttrNewKey` shape) appends it in line, after the
    first, when the values have room and the value needs no write
    barrier: `oo_patterns` takes 2.1% fewer instructions, `logging_mix`
    1.7% and `context_managers` 1%.
  - `object.__new__(cls)` in a user `__new__` (`pathlib`'s paths) runs
    from compiled code as the core loop's leaf call makes it, instead of
    registering the activation for the core loop's lane, so the class's
    `__init__` runs natively after it; and a certain attribute miss (an
    unset `__slots__` member that a `try` catches, `pathlib`'s cached
    parse) raises in place even at a site that also calls getters. A
    `pathlib` join and four properties cost 83,347 instructions instead
    of 116,859, and `stdlib_utils` takes 5.7% fewer.
  - An arithmetic site the core loop has seen produce or take a big int
    (`statistics`' exact sums square numerators past a machine word) is
    compiled through the helper that takes every shape, instead of the
    numeric path, which leaves compiled code whenever its result isn't an
    `int` or `float`. Big ints also compare natively (with each other, or
    with a machine int by their sign), and a temporary one that dies in
    the comparison no longer sends it to the core loop. `d[k] += a * a`
    over such values costs 2,109 instructions instead of 2,948, and
    `statistics.mean`, `median` and `pstdev` of 200 floats 3.4% fewer.
  - A scalar builtin of big ints (`math.gcd` of `fractions`' terms) runs
    as a leaf from compiled code, as for machine ints, instead of
    publishing its caller for the core loop's lane: `statistics.mean`,
    `median` and `pstdev` of 200 floats take another 1.4% fewer
    instructions.
  - A site that concatenates sequences or bytes (`buf += packed` of a
    `bytearray`, `a + b` of lists) is noted as a heap value's too, so it
    compiles through the general helper instead of the numeric path that
    leaves compiled code for every non-number: `bytes_codecs`'s packing
    loop costs 4,686 instructions per iteration instead of 5,131.
  - A body too large for one compile runs in chunks, and a raise caught
    by a handler in another chunk (a `with` block's in a loop, whose
    handler is laid out past the loop) now goes on in that chunk, as a
    jump between chunks does, instead of leaving for the core loop:
    `context_managers`'s `bench` left its loop's chunk at every raise,
    twice an iteration, and `context_managers` takes 4% fewer
    instructions. (Weighing the exception edges in where the chunks are
    cut got the same there, but moved `tomllib_loads`'s cuts into a hot
    loop: 1.4% more.)
  - A store through a native property's setter whose fast half runs no
    Python code (asyncio's `Task._log_destroy_pending`, which `gather`
    sets on every child) runs from compiled code, instead of leaving for
    the interpreter: `async_tree` takes 1.9% fewer instructions. The
    setter's full half now takes a value's truth as CPython does,
    running its `__bool__` (it read any instance as true).
  - A builtin called with keywords whose call may run Python code
    (`sorted(xs, key=lambda v: -v)`, `ElementTree.SubElement(parent, tag,
    id=...)`) goes through the core loop's lane for it with the activation
    published, the result in place, instead of leaving compiled code:
    `closures_decorators` and `xml_etree` take 1% fewer instructions.
  - A plain class called with keywords (`C(i, b=2)`, `gather`'s
    `_GatheringFuture(children, loop=loop)`) makes its instance and enters
    its `__init__`'s native code directly from compiled code, the keywords
    bound by the permutation the function call sites use (found for the
    `__init__`, the instance its first positional), instead of leaving at
    the `CALL_KW` and running the `__init__` in the core loop: `C(i, b=2)`
    costs 2,679 instructions instead of 4,531.
  - Open: `test_set` fails about once in a hundred runs, a set missing
    members after a `discard` (`test_discard_absent`, and
    `test_set_membership` after `discard(set(range(3)))`). It predates this
    round, doesn't repeat under the failing run's hash seed, and 480,000
    random discards against a model and runs with collections every few
    allocations found nothing; the dict-map removal (backward-shift
    deletion, then the moved entry re-pointed) reads correct.
  - Next for `exceptions` and `try`-heavy code: a compiled function that
    catches an exception registers its activation for the core loop's
    catch, which needs the activation's frame shell for the traceback
    entry, so the function's return and its caller's then run switched
    (`exceptions` registers `lookup` and `guarded` once per raise).
  - Measured and left: compiling, when its calls make it hot, code whose
    loops have never repeated (`urllib.parse._checknetloc` returns before
    its loop, and stays in the core loop since tier 2 never sees the loop
    run) cut `stdlib_utils` by 1.2%, but `raytrace`'s `Scene.trace` meets
    the test too (its first rays miss the scene and return before the
    light loop), and taking it from tier 2 cost `raytrace` 8.4%.
  - Compiled callees run directly inside their compiled callers' calls up
    to 128 levels deep instead of 48 (the main thread reserves 1 GiB of
    stack and a worker 64 MiB), so a recursion through `lru_cache`
    misses or a solver's search stays native longer before publishing
    its levels for the core loop: `go` takes 0.6% fewer instructions,
    `closures_decorators` and `sudoku` 0.5%.
  - Measured and left: not inlining a recursive call at all, instead of
    once (`go`'s `Square.find` exits its unrolled level whenever the chain
    runs deeper, binding every level for the core loop, 5,779 times a
    call), cost `go` 3% more instructions: the frames the unrolled level
    saves outweigh the exits.
  - Measured and left: tracking an untracked instance through a small
    helper when an in-line store puts a heap value in it (the write
    barrier), instead of leaving the store to the full helper, changed
    nothing measurable.
  - Measured and left: loading a method in line from its site's first
    cache (the class version, then the instance's values checked for the
    name) took `h_method` from 3.3% of `go` to 0.3%, but the compiled
    guards cost as much as the helper's hit had, and `go` took 0.8% more.
  - Measured and left: the hot code footprint is about three times
    CPython's (204 KB of lines run at least once per iteration on
    `context_managers`, against 66 KB), but neither of the obvious
    layout levers moves it. The frame JIT's code for a loop is about 73%
    dense in its hot lines, so moving its cold blocks out would save a
    quarter of the JIT's share at most. About 38% of the hot
    instructions are source lines compiled in more than one function,
    but nearly all of those are reference counts, drops and JIT code,
    which belong in line. Turning the leaf evaluator off changed neither
    instructions nor simulated misses on `class_creation`, `deepcopy`
    or `logging_mix`.
  - Measured and left: compiling code in the frame JIT whose loops tier 2
    has left alone for three hot spells (`pathlib`'s `_parse_path`, whose
    probes keep missing) cut the `pathlib` loop by 4% and enum creation by
    4.5%, but it also took float loops that tier 2 compiles better from
    it: `scimark` took 19% more instructions and `raytrace` 8% more.
  - Measured and left: a builtin that takes keywords and calls back into
    Python (`sorted(xs, key=f)`) through the core loop's call lane from
    compiled code costs more than leaving for the interpreter there.
  - A `BINARY_OP` between two locals that the core loop has run to a
    string (`path += sep + b`) compiles to the helper with its result on
    the stack, instead of expecting a number in line and leaving for the
    interpreter on every other result. `posixpath.join`'s loop of three
    parts costs 4,182 instructions instead of 5,417 (CPython 3.13: 4,016),
    and `path = a; path += sep + b` 1,136 instead of 2,743 (CPython:
    1,478).
  - Fixed: a call from compiled code that spread more arguments than the
    caller's stack had room for, through the core loop's lane, grew the
    stack's buffer under the compiled code, which had loaded its address
    on entry and went on writing to the freed one (`test_pathlib`'s deep
    `glob`, once `posixpath.join` stopped leaving compiled code). Such a
    call now leaves the rest of the activation to the core loop.
  - Measured and left: a `pathlib` path's `base / name` still costs about
    twice CPython's (15,309 instructions against 7,673), spread over the
    calls `__truediv__`, `with_segments`, `__new__` and `__init__` make.
  - Compiled code indexes a list or tuple by an `int` in line, and stores
    into a list over a scalar the same way, where it used to call the
    frameless helpers: the container's tag, an unborrowed list, the index
    in range from either end, the item copied with its count bumped, and
    an owned container's reference given up by a plain decrement (a call's
    result, `self.items[i]`). The core loop serves these subscripts
    without the cache machinery, so a site's cache now settles on the
    shape there while it's still empty, for the compile to read.
    `nqueens` takes 4% fewer instructions, `chaos` 2.7%, `unpack_sequence`
    1.4% and `go` 1.2%. Fixed before it landed: the layout probe read the
    sample vector's capacity as its length when the two were equal, so a
    list with room to spare read past its end from the end.
  - Compiles cost 18% fewer instructions (on `stdlib_utils`'s first call,
    102 million to 84 million): the constant folder skips the
    instructions it can't fold instead of copying each one's operands, the
    simplifier walks the blocks in place instead of copying their
    instruction lists, liveness finds the values that cross blocks and each
    block's upward-exposed uses in one pass with flat per-block lists and
    looks at a block again only when a block it branches to changed, the
    walks over an instruction's uses are in line, a function's integer
    constants are made once (each use rematerializes it), and the register
    state a branch hands its successor reuses a vector. The first call of
    `stdlib_utils` compiles about 35,000 IR instructions, and code
    generation was 15% of it, about 2,500 instructions per IR instruction.
    `class_creation`, which compiles the functions `dataclass` and
    `namedtuple` generate through its warm runs, takes 4.2% fewer
    instructions.
    A second round takes it to 80 million (22% below where it started): the
    simplifier resolves aliases, forwards jumps and counts uses in one pass,
    the folder keeps its operands in a small array and rebuilds each block's
    list in the vector the last one gave up, a call drops what the callee
    clobbers by walking the clobbered registers' bits instead of all 64, a
    parallel move reuses its scratch vectors, and a merge edge lists the
    values live into its block only when it's the first edge in.
  - `exec` and `eval` of a source string reuse the code they compiled from
    the same source under the same compile options (the caller's
    `__future__` flags, the optimization level), up to 256 sources of 16 KB
    or less, unless its parse warned (a warning is emitted at each
    compile). `dataclasses` builds the same `__init__`, `__repr__` and
    `__eq__` source for every class with the same fields, and `namedtuple`
    the same `__new__`, and compiling it cost more than the rest of making
    the class. The cache keeps the encoded form (as the frozen modules'
    cache does), and each run decodes code of its own, bodies on first use:
    reusing the code objects themselves kept what a run's caches and
    compiled code hold (the functions it called) alive with them.
  - Compiled code reads a class attribute through an instance (`self.flag`,
    a class-level default) and a method as a value (`f = self.method`,
    `self._log(msg, args, **kwargs)`, whose `CALL_FUNCTION_EX` takes no
    method load) from the site's own caches, where it used to take the
    core loop's whole resolution at every read: a scalar the site's stamp
    remembers, or the function its method slot resolved at the class's
    version, bound to the receiver, either one only while the instance
    doesn't shadow the name. A scalar's read costs 231 instructions a call
    instead of 511, and `logging`'s `warning(msg, *args, **kwargs)` shape
    3,637 instead of 4,290.
  - `==` and `!=` whose operands are locals or constants, past the
    numbers compared in line, compare two exact strings where they lie
    through a helper that takes no reference: the generic compare site
    copied both operands to the stack and released them after. `tomllib`'s `char == "["` tests take 4.1% of its
    instructions off.
  - Compiled code indexes a pure-ASCII string by an `int` in line as it
    does a list: the byte read where it lies and its single-character
    string taken from the shared table of them, its count bumped, an owned
    string's reference given up by a plain decrement. The core loop
    settles a string site's cache on the shape too. `tomllib`'s
    `char = src[pos]` takes another 1.4% of its instructions off.
  - The collector's registry keeps its entries in chunks of 1,024 that
    never move, instead of one vector that doubled, copying every entry
    into a buffer twice the size while the old one was still there (the
    copy set the peak of a program with many live objects). `async_tree`
    peaks 2.5% to 5% lower and a pending task takes 3,339 bytes instead of
    3,530; instructions are unchanged.
  - A lazily made coroutine's frame (`lazy_coro_bind`'s box) gets an
    operand stack sized to what its code and native code need, as a
    generator's does, instead of room for 16: a suspended coroutine keeps
    it, and a task tree holds thousands. A pending task takes 3,179 bytes
    instead of 3,385, and `async_tree` peaks 5% lower.
  - The linker order file (`crates/weavepy-cli/weavepy.order`) is rebuilt
    from callgrind profiles of start-up and of every pybench workload
    (`scripts/extend-order-file.py`). A third of the functions `go` runs
    had joined the binary since the file was last made, and they sat
    scattered through the text among code no workload touches, so each
    run faulted in pages for a few functions apiece. Peak RSS falls 2.3%
    (the geometric mean of the 33 workloads, every one lower, and
    `sqlite_queries` 7.6%).
  - An instance is a 96-byte allocation instead of 128: its slot storage
    is one tagged word instead of four (a laid-out `__slots__` block now
    holds its layout's owner and values in one allocation, and the per-key
    and packed forms are boxed), and a C extension instance's native body
    pointer lives in a side table behind a one-byte flag. Compiled code
    reads a slot through the word: one more dependent load. Asyncio's
    futures lay their state handle out alone, so finding the state
    compares no names. An instance with no attributes takes 117 bytes
    instead of 149, and one with three takes 182 instead of 214;
    `float_points` peaks 8.4% lower, `oo_patterns` 4.2% and
    `collections_mix` 2.9%. Instructions are within 0.5% except
    `decimal_telco` (1.7% more: a packed decimal's fields sit behind the
    word) and `context_managers` (0.5% fewer).
  - Asyncio's handles and future iterators and `contextvars`' contexts and
    tokens are built with their slots laid out in place, instead of with
    their class's empty slots that the constructor then replaced, and a
    laid-out block takes its values in one copy. `async_tree` runs 1.9%
    fewer instructions.
  - A collector registry entry is 24 bytes instead of 32: it keeps its weak
    handle as the handle's pointer and tag, the tag beside the entry's
    generation and flags, instead of as a `WeakObject`, whose tag took a
    word of its own. Every tracked object costs 8 bytes less (a pending
    asyncio task tracks seven); instructions are unchanged.
  - `async with` over a coroutine `__aenter__` or `__aexit__` runs the
    method's body in line, as an `await` of a coroutine function's call
    does: that path recognized only `GET_AWAITABLE 0`, and `async with`
    marks its two awaits 1 and 2 (the argument only words the error for a
    result that isn't awaitable, which a coroutine never is). An
    uncontended `async with asyncio.Lock()` takes 396 ns instead of 693
    (CPython 351), a trivial async manager 208 ns instead of 504 (CPython
    227), and `async_tree` runs 0.8% fewer instructions.
  - `%(key)s` conversions probe a plain dict with the key's text instead of
    making a `String` and then a string object of it per key (`logging`'s
    `%(asctime)s %(levelname)s ...` record format): `logging_mix` runs 0.2%
    fewer instructions.
  - Compiled code calls an instance's native method (a `deque`'s `append`,
    a future's `done`) directly where its method site cached one: a helper
    checks the class version, the default attribute lookup and that the
    instance holds no attributes of its own, and calls the method on the
    arguments' places, which the code found as it compiled. The fused
    method call it replaces looked the site's tables up twice and decoded
    the argument loads again on every call. A deque `append` and `popleft`
    take 54.5 ns instead of 94.3 (the frame compiler's code alone; CPython
    36.5); `async_tree` runs 2.4% fewer instructions and `graph_search`
    1.9%.
  - Unpacking a tuple only the stack holds moves its items out, instead of
    copying each and then releasing the tuple's own references.
  - Tier 2's hot loops read and store the items of `dict` subclasses that
    keep `dict`'s methods (a `defaultdict`, a `Counter`) directly, as the
    frame compiler's code does, instead of through the general subscript:
    `d[k] += 1` on a `defaultdict` takes 65.5 ns instead of 189.4 (CPython
    68.9).
  - A method load through an instance that found a `staticmethod` or a
    `classmethod` keeps the wrapped function on its site, under the class
    version, and loads it again from there while the instance can't shadow
    the name, instead of running the full method load every time. A site
    learns only from a result shaped as a descriptor's, and remembers a
    version whose attribute isn't one, so polymorphic plain-method sites
    pay nothing. A `classmethod` call through an instance takes 44.9 ns
    instead of 58.5 (CPython 53.5), a `staticmethod` call 24.9 instead of
    45.8 (CPython 46.7), with the frame compiler's code alone.
  - Compiled code reads a frame's or a code object's plain attributes
    (`f_back`, `f_code`, `co_filename`: `logging`'s caller search) ahead of
    the instance caches it would otherwise try first: `logging_mix` runs
    0.4% fewer instructions.
  - Asyncio's `Handle` class lays its slots out in the order its native
    constructor writes them, when `install_handle` finds the class's
    storage undecided (asyncio's classes otherwise keep the per-key form).
    `call_soon`'s handles and `Handle(...)`'s then share one layout, so
    compiled code's guarded slot reads find it: `_run_once`'s
    `handle._cancelled` missed every time. With a deque's laid-out slots
    matched by name identity (the ready queue's `append` and `popleft`)
    and a built-in value's `__class__` read without the instance caches
    (`asyncio.isfuture` of a coroutine), `async_tree` runs 2.3% fewer
    instructions.
  - An attribute site that has read a class's attribute through an
    instance tries that first on its slow paths: `o.k` of a class constant
    takes 38.6 ns instead of 48.3 (container build; CPython about 19).
  - Compiled code reads an atom (a number, a string, `None`) a class holds
    from the attribute site's memo of it while the class and its version
    are the ones it was read under, where the class's metaclass reads
    attributes as `type` does (`ABCMeta` included) and doesn't shadow the
    name. Such reads (`type(self).__doc__` in `contextlib`'s generator
    managers) left compiled code for the interpreter every time.
  - A built-in container's or string's method read as a value
    (`append = y.append`, `memo.get` in `copy.deepcopy`) is bound from the
    site's memo of the method for that kind of receiver (the types never
    change) instead of looked up each time. With the class atoms above,
    `context_managers` runs 2.4% fewer instructions and `deepcopy` 1.4%.
  - The class read remembers a declined name too (an `Enum` member, a
    function), so that `w.kind is not Kind.C` doesn't repeat the
    metaclass and class lookups each time it misses: `comprehensions` ran
    3% more instructions without it.
  - `cls.__new__` of a class that inherits a builtin constructor
    (`object.__new__`, as `copy`'s reconstruction reads it) is read from the
    same memo: 54 ns becomes 29 (container build; CPython 28).
  - Method calls on ints, bools, floats, tuples and bytes take the memoized
    method for their kind as lists and strings do (`x.bit_length()`,
    `f.is_integer()`, `t.count(v)`, `b.decode()`): four such calls take
    249 ns instead of 360.
  - `o.__dict__` of an instance whose class keeps its default managed dict
    returns the dict from the site's memo of that class's answer instead of
    two MRO walks and a type-name test each time: 57.5 ns becomes 31
    (CPython 32), and `deepcopy` runs 0.9% fewer instructions.
  - The `AttributeError` of an attribute certainly missing is built with
    one allocation for its message and the code's interned name for its
    `name` field, and an unset `__slots__` member's class is named from the
    memoized qualified name; whether a name is such a member is memoized
    on the class. `Object::from_str` of a `&str` no longer copies it twice.
    The `pathlib` shape (a property that catches the `AttributeError` of
    its cache slot once per object) runs 5% fewer instructions, and
    `stdlib_utils` and `logging_mix` 0.5% fewer.
  - A keyword call of a function with cell variables (`parse_qs` calling
    `parse_qsl`, whose nested `_unquote` captures its arguments) binds
    through the site's permutation and runs directly: the binders made the
    cells from the bound locals all along, but the specializer refused such
    callees. `stdlib_utils` runs 0.5% fewer instructions.
  - `FOR_ITER` steps a list's or a range's iterator in compiled code
    instead of calling `h_for_iter`, and a store over a local (or a
    `POP_TOP` of a value) that another owner keeps gives up its reference
    by a plain decrement in line instead of calling `h_store`: a loop of
    `x = values[s]` over a list takes 247 instructions an iteration instead
    of 293, `sudoku` 2.2% fewer and `comprehensions` 2.5%.
  - `d[k]` of a dict by a `str` key whose hash is known probes the key's
    home slot in compiled code, the value copied when the slot holds the
    same string or an equal one (a key built apart from the dict's own,
    as `sudoku`'s units are, compared by a small helper), anything else
    through the helper as before: the loop above takes 230 instructions
    an iteration.
  - `len(x)` at a site settled on the builtin `len` reads a list's,
    tuple's or string's length in compiled code (the callee checked by its
    payload word, both references given up by plain decrements) instead of
    calling `h_site_leaf`: `t += len(values[s])` in that loop takes 342
    instructions an iteration instead of 467.
  - Fixed three of the CI bench gate's regressions against `main` (its A/B
    of this branch's tier 2 against Cranelift's):
    - Tier 2's checked `int` arithmetic uses the code generator's overflow
      operations (`add`, `sub` or `imul` and `jo`) instead of computing
      the overflow test from the result: `jitloop` runs 96 million
      instructions instead of 122 (`main` 115), `nested_loops` 73 instead
      of 108 (100), `sumvm` 46 instead of 56 (70).
    - The pin memo tier 2 reuses an object's pin by is 64 entries in sets
      of four instead of 32 in pairs: three of `richards`' eight tasks
      sharing a pair made every miss pin anew, and the table grew by
      reallocation (127 million instructions instead of 164; `main` 131).
    - A constructor's leaf `__init__` runs its plan's native code, as any
      effect leaf's does (its stores buffered, landing at the end), instead
      of always the plan interpreter, when the plan calls something:
      `float_math` runs 718 million instructions instead of 800 (`main`
      706). (A constructor that only computes and stores stays
      interpreted, where its stores land at once: buffering them made
      `mini_interpreter`'s `BinOp.__init__` 2% dearer.)
  - A class's constant read off a class value (`BinaryConstraint.FORWARD`
    in `deltablue`, the receiver on the stack) is answered from the site's
    memo by `h_attr_stack` itself, and the memo fills before the core
    loop's stamp is tried: `deltablue` runs 6% fewer instructions.
  - The in-line release, dict probe and `len` go only where code is likely
    hot: anywhere in a body of up to 160 instructions, else inside a loop.
    Every site's extra code cost `class_creation`, which compiles large
    cold functions (`namedtuple`, `inspect.signature`), 4% more
    instructions; this takes back a third of that.
  - Linux start-up no longer waits out an RCU grace period: the process
    registered for expedited private `membarrier`s when its first VM thread
    started, after the large-stack thread existed, and the kernel registers
    a multi-threaded process by `synchronize_rcu()`. The registration now
    happens at the first bias revocation, the only place the barrier is
    used (a program that never shares cells never registers): `startup.py`
    takes 11.4 ms instead of 32.7 in the container (CPython 11.5), against
    12 ms of CPU either way. CI's bench gate read 13.6 ms against 10.4.
  - `str % dict` whose one conversion is a bare `%r` or `%s` (the dict
    itself is the value: `logger.error("error: %r", {"i": i})`, which
    `LogRecord.getMessage` formats) renders the dict's repr natively when
    its keys and values are plain, instead of leaving compiled code:
    `logging_mix` runs 2.4% fewer instructions.
  - A natively served `datetime`'s, `date`'s or `timedelta`'s public field
    (`current.hour`) is read off its packed state at the start of the
    attribute site's miss path, out of line, instead of after the instance
    probes and the core loop's cached reads: the bench gate's
    `datetime_ops` runs 6% fewer instructions with 14% fewer simulated
    instruction-cache misses, and 8% faster in the container. (CI's Linux
    gate read it 16% to 20% slower than `main`, though fewer instructions
    ran; macOS release builds timed it level.) The next CI run read it
    1.06 on Linux.
  - Cycles, not instructions, are now the gap. `tools/pybench/run.py
    --cycles` counts a warm call's CPU cycles as `--instructions` counts its
    instructions. On a release build (not PGO), on a loaded machine, the
    suite took 1.10 of CPython's cycles against 0.925 of its instructions
    (`exceptions` 1.73, `class_creation` 1.53, `logging_mix` 1.51). The
    simulated cache shows why: a warm call of `nqueens`, `tomllib_loads`,
    `exceptions`, `context_managers`, `deepcopy` or `async_tree` takes 2.5
    to 4.7 times CPython's L1 instruction misses (data misses about 2
    times, branch mispredicts level), spread thinly over a hundred or more
    functions each run about once a call. Compiled code is a fifth of them
    on `nqueens` (`permutations` alone is 16.5 KB). An LLVM inline
    threshold of 100 left the misses within 3% and added 1 to 5% more
    instructions.
  - Compiled code lays out its slow paths after the hot code: a guard's way
    out (`branch_out`), and `BINARY_OP`'s and `COMPARE_OP`'s fallback
    through their helpers, are cold blocks, and the code generator also
    treats as cold any block only cold blocks branch to (a slow path's
    own blocks). `nqueens` takes 5% fewer simulated instruction misses;
    the others moved by under 1%.
  - The code generator is a little cheaper: liveness marks each
    upward-exposed use's value live back along the predecessors to its
    definition, in one pass, instead of iterating a bitset dataflow to a
    fixed point; dropping a value from the live list is O(1); and a
    comparison checks whether its branch follows from its own position
    instead of searching the block. Code generation is 14% of a short
    `deltablue` run's instructions; this takes 1.3% off it.
  - A compiled `d[key]` that misses an exact dict raises its `KeyError`
    from the subscript's own helper instead of re-running the subscript
    through the generic container path first (one dict probe fewer, and
    no `core_container_op`): a caught `KeyError` takes 2,480 instructions
    instead of 2,618 (CPython 1,604). What's left of it is a long tail:
    freeing the exception instance (about 1,000 instructions: its slots,
    `args` tuple and traceback), the unwind and the handler's three
    exception ops, each 50 to 300.
  - Where the warm calls' instructions go, by category, over the suite (a
    container profile of each workload): the core loop's and the frame
    JIT's call machinery about 25%, compiled code 16%, releasing and
    dropping objects 9%, allocation 5%, generators 4% (`generators_tree`
    55%, `coroutines` 48%), the collector's bookkeeping 2% (about 250
    instructions for each list or dict born holding only scalars). Against
    CPython 3.14, a generator's whole life (made, run to one yield, run
    out, freed) takes 2,526 instructions to 1,265, and a `yield from`
    delegation 5,611 to 2,414; a 2-tuple's 432 to 251 and a 1-list's 644
    to 376, while compiled loops, calls and attribute reads take a
    fraction of CPython's.
  - Linux CI's binaries start in about 22 MiB against macOS's 11, the size
    of an unordered build in the container (15.4 ordered, 22.8 unordered).
    `scripts/link-ordered.py` now notes whether it ordered the link, and the
    bench gate's job prints the note: CI's link is ordered (6,136 symbols,
    rustc 1.99.1), so the runner's kernel maps more per fault than the
    container's (larger page-cache folios, likely), not a missing layout.
  - Tier 2 leaves attribute-bound loops to the frame JIT: a loop whose
    attribute reads and writes outnumber its typed arithmetic and compares
    isn't compiled (`WEAVEPY_T2_ATTRS=0` turns the rule off). Each tier-2
    attribute access is a helper call that redoes its guards (about 50
    instructions; `wpjit_attr_get`), where the frame JIT reads or writes
    the field in line, so `attr_access`'s loop took 1,529 instructions an
    iteration in tier 2 against 1,371 in the frame JIT. Calls don't count
    against a loop: a driver loop calling compiled functions (`fannkuch`'s,
    `nbody`'s, `richards`') stays native-to-native in tier 2, which a rule
    that counted them broke (2.1, 1.7 and 1.9 times slower). Over the bench
    gate's fixtures (best of 5 and 8 runs on this Mac): `attr_access` 0.867,
    `deltablue` 0.961, `call_overhead` 0.974, the rest level; the suite's
    instructions are level (`float_points` 0.98). With tier 2 off entirely,
    `deltablue` runs another 7.5% faster: tier 2 compiles 28 of its small
    methods for nothing (13.7 ms) and re-analyzes bodies it can't type for
    each call site (`output` 98 times).
  - Compiled code is 17% smaller on x86-64 (`deepcopy`'s 54.5 KB to 45.2,
    `context_managers`' 78.6 KB to 65.2). Code memory now comes from one
    region reserved within a 32-bit displacement of the binary's own code,
    where the system places it there (`WPCG_FAR=1` declines it): a helper
    call is a 5-byte `call rel32` instead of a 10-byte address load and an
    indirect call, an address in reach (a helper, a global, and on macOS
    most of the heap) a 7-byte `lea`, and a load from a global (the
    release gate a reference count's decrement checks) one RIP-relative
    load. The displacements are written as the code is installed. Branches
    whose targets lie within a signed byte take the 2-byte forms, a
    reference count's increment is one `add qword [mem], 1`, and a compare
    with zero is a `test`. Instructions are level, and simulated L1
    instruction misses moved by under 1% on Linux (the compiled code's own
    by 1% to 8%): the hot code a warm call runs is several times the
    32 KB cache, so the misses track the whole footprint, of which compiled
    code is a seventh. With the class-read memo below, a release build on
    this Mac took 0.986 of `a60298cb`'s cycles over the suite
    (`coroutines` 0.92, `context_managers` and `stdlib_utils` 0.94; single
    workloads move by about 3% between runs) and 0.993 of its
    instructions.
  - A class's function read through a class (`cls.__enter__`,
    `T.method`, as `contextlib.ExitStack.enter_context` and `super`-free
    delegation do) is memoized at its site like the class's constants:
    the site holds the function without owning it, valid while the class's
    attribute version (never reused, and moved by any change to what the
    class's attributes resolve to) stands, so a replaced function still
    dies when the class lets it go. A dunder name `type` doesn't define
    (`__enter__`) is memoized too, and a local class receiver checks the
    memo before the general miss path. The read takes 165 instructions
    instead of 377 (CPython 246), 184 through a global instead of 620
    (CPython 279); an `ExitStack` with two callbacks 2.5% fewer. A name
    `type` itself serves (`__name__`, `__text_signature__`, the getsets
    and members CPython's `type` has) is never memoized, as its data
    descriptor wins over the class's own entry (`test_inspect` caught a
    first version that memoized `__text_signature__`). An instance of the
    class itself that isn't a descriptor (an `Enum` member, `Kind.C`) is
    memoized the same way, borrowed: 184 instructions instead of 695
    (CPython 321), and `x is not Kind.C` 290 instead of 895 (CPython 672).
  - A class attribute read through instances of several classes
    (`self.unit` in a base class's method, as `oo_patterns`' `describe`
    reads it for each kind of shape) took the general lookup on every
    switch of class: the site's class-attribute stamp holds one class.
    Each frame-JIT attribute site now also keeps four classes' values,
    keyed by the instance's class's attribute version and borrowed as the
    class memo's are, for values of built-in types no access binds (a
    string, a number, `None`, an exact container). Over three classes the
    read takes 404 instructions instead of 1,101, over two 383 instead of
    1,747 (with tier 2 off; tier 2's own attribute reads, in a loop it
    compiles, are unchanged). A class whose read the memo can't serve (a
    named tuple's field, a descriptor's value) gets an entry saying so: a
    first version looked the name up again on every such read, and
    `collections_mix` ran 9% more instructions. Over the suite the
    instructions are level (`comprehensions` 0.98, `oo_patterns` 0.99).
  - Tried and dropped: tracking an instance that is about to hold a value
    that could close a cycle (every raised exception, once its traceback
    is stored) by putting it in its young set directly, without
    `track`'s scan of what it holds. A raised and caught custom exception
    took 4% more instructions.
  - `random` is now faster than CPython's C module, method by method.
    Counting which compiled attribute and method sites fall through every
    memo to the general lookup, across the suite, put `Random`'s methods
    in five workloads (`chaos`, `sorting`, `scimark`, `go`, and
    `random.random()`). The generator's state lived as a `bytearray` in
    the instance's `__dict__`, which made the dictionary real, so every
    method load on a `Random` took the general path (and `vars(r)` showed
    `_mt_state`, which CPython's doesn't): it now lives in a slot no
    descriptor names. And a native method bound to its receiver that the
    leaf registry vouches runs no Python code (`random.random` is a
    module's saved `Random().random`) is called in place from compiled
    code, as the core loop now calls it too, instead of leaving compiled
    code for the core loop's general call. Instructions a call, before,
    after and CPython's: `random.random()` 3,013, 917 and 573;
    `r.random()` 1,078, 650 and 725; `r.getrandbits(8)` 1,046, 619 and
    597; `r._randbelow(10)` 2,742, 1,350 and 1,503; `r.randrange(10)`
    3,992, 2,724 and 2,381.
  - Tried and dropped: a per-thread tuple free list that every
    construction and last release used (CPython's), replacing the
    interpreter's own pool, which only the core loop's paths reach. A
    tuple built and dropped in a loop took 54 more instructions (the
    thread-local access and the deep-release guard cost more than
    mimalloc's allocation), and a caught exception 1% to 3% more.
  - Fixed: the code generator's differential fuzzer let the compiler put
    the generated function's address in a callee-saved register the test
    sequence seeds before the call, so in release builds the test jumped to
    the seed and crashed (debug builds, as CI runs them, passed).
  - Where the first call's time goes, on `stdlib_utils`: of its roughly
    160 million instructions over a warm call, about 107 million are
    compiling (75 million code generation, 11 million frame-JIT lowering, 9
    million tier-2 analysis). With the frame JIT off the first call takes
    as long, the interpreted code costing what the compiles save, so
    cheaper compiles are what shorten it. A background compile thread would
    hide them on a machine with a core to spare, but it cost 2 to 3 MB of
    peak memory when the frame JIT had one, and memory is the metric
    WeavePy trails most in.

- **Polymorphic attribute sites, yields folded in place, and call-dense
  loops back to the frame JIT** (PR #107). Instructions on Linux unless
  noted.
  - Tier 2 counts a framed call of a loop-free callee toward its
    call-density judgment, so a loop dense with method calls, constructors
    or calls taking objects retires to the frame JIT, whose direct calls
    run them about 3 times cheaper. On macOS, `o.setx(i)` went from 950 to
    321 instructions (CPython 497), `h(o, i)` from 1,281 to 361 (CPython
    513) and `O(i, 2)` from 2,251 to 1,576 (CPython 1,232).
  - A base-class method that reads the fields of several subclasses'
    instances misses its site's one-class field cache every time the class
    changes. Compiled reads now check the core loop's polymorphic entries
    for the site, and keep a second class's field beside the first.
  - A class constant read through the class (`Cls.CONST`) is answered from
    its site's stamp, in an inlined body as in a plain one, and a fused
    global read remembers the shape instead of looking the global up again.
  - Together, these took `deltablue` from 24.5 million instructions an
    iteration to 20.5 million (CPython 3.13: 17.1 million).
  - A class's `dict`, `list` or `set` read through an instance
    (`self.PREC`) is cached as the site's class entry, as a tuple was:
    `mini_interpreter` 6% fewer.
  - Compiled reads also take the site's instance-dict hint for a published
    dict (`LogRecord`'s, once `record.__dict__` is read), a named tuple's
    field from its site's stamp, and a frame's or code object's fields
    (`f.f_back`, `co.co_name`) before the general path. The named tuple's
    field took `collections_mix` 5% under its instructions before this
    work, and `urlsplit` 2%. A read the site has nothing for leaves after
    two loads, which kept the checks from costing a miss more than they
    save.
  - A generator's compiled body hands each yield its draining consumer
    takes (`list()`, `tuple()`, `set()`, `sum()`, `all()`, `any()`) to the
    consumer's sink and goes on, where it used to leave its native code at
    every yield. `set()` of an eight-item genexp went from 8,857 to 7,982,
    and `nqueens` 6% fewer.
  - An instance released through the last-release hook (compiled code's
    releases) goes back to the instance pool, as one the core loop releases
    does: `P()` of a slotted class went from 1,313 to 1,111.
  - The VM's unit tests, built optimized, hung in about one run in three
    on macOS. Every test thread took the cells' lock-free path, which only
    debug builds limited to the first registered thread. Two threads'
    plain updates of its guard count could lose one, and the next thread
    to register then spun forever, holding the GIL. Test builds now limit
    it too (12 runs out of 12 clean).
  - Measured and left: an in-line field read in plain compiled bodies
    (the inlined bodies' own) didn't pay for its code, and materializing
    `f_back` for `logging`'s `findCaller` stays about 742 instructions a
    frame (CPython builds a frame object too, for less).

- **Python `__hash__` and `__bool__` in place, and an earlier frame JIT**
  (PR #107). Calls from native code into Python stay the widest everyday
  gap (see "Next", item 11), measured again on macOS: a dict lookup keyed by
  an instance with a Python `__hash__` 2,659 instructions against CPython's
  1,033, `if x:` with a Python `__bool__` 2,748 against 1,002.
  - A class's Python `__bool__` that evaluates as a pure leaf answers the
    truth test in place, as a `__len__` did (`if x:` 2,567 to 400 on
    Linux), and a dict read of an instance key whose `__hash__` is a pure
    leaf hashes it in place and finds the key itself, as CPython's probe
    tries identity before `__eq__` (2,611 to 1,699; any other key with that
    hash takes the full path, which compares).
  - The same pure-leaf `__hash__` serves `hash(x)` (a frozen dataclass's
    2,898 to 2,109), `x in d` and `x in s`, and `d[x]` from compiled code
    and tier 2 (1,709 to 1,111).
  - The frame JIT's threshold is 200 times a code's length, from 500: over
    the suite 0.996 of the instructions (`class_creation` 0.94,
    `collections_mix` 0.93, `async_tree` 1.02) and 0.995 of the warm time.
    100 and 50 gave 0.996 and 0.995, with `async_tree` or `generators_tree`
    4% worse.
  - A lowering reserves its IR tables from the body's length (the function
    goes to code generation, so each lowering started from empty tables and
    grew them by doubling): 0.3% off a short `go` run.
  - Compiled code tests `bytes`, `bytearray`, sets, frozensets and big ints
    for truth, and concatenates two `bytes`, in place.
  - Measured and left: with both JITs off the core loop retires 1.45 times
    CPython's instructions (`go` 2.95, `mini_interpreter` 2.46), so code
    that runs before it compiles pays that; a generator's item costs 1.3 to
    1.45 times CPython's (`gen_fast_resume` and `gen_native_yield` about 310
    instructions a resume, spread over checks, the frame's move out of its
    state and back, and the native state's set-up); and a native caller's
    callback through tier 2's entry about 3,000 instructions.

- **Methods cached in locals, and `re`'s opcodes as ints** (PR #107).
  Compiling a regular expression (`re._parser.parse`) retired 2.5 times
  CPython's instructions (998,000 against 395,000 for a number pattern);
  `-X importtime` blames such compiles for much of `json.scanner`'s,
  `textwrap`'s and `tokenize`'s import time. Fixed (instructions on Linux,
  964,000 to 659,000 a parse, `test_re` 16% faster; the suite is
  unchanged, 1.001 of the instructions, as it barely compiles patterns):
  - A method cached in a local (`sourceget = source.get`, as `re`, `json`
    and `tokenize` do) went through the core loop's general call every
    time from compiled code: a bound method wasn't a direct call's callee,
    and an inlined body's guard wanted a function. Both unpack it into the
    function and its receiver first, as CPython's
    `CALL_BOUND_METHOD_EXACT_ARGS` does (8.8% fewer).
  - `re`'s opcodes are an `int` subclass (`_NamedIntConstant`) that keeps
    `int`'s hash, equality and ordering. Such an instance now hashes and
    compares as its value in the native set and dict probes (`op in
    _REPEATCODES` registered the whole call chain every time), and orders
    as its value in `min` and `max`, which also take big ints now
    (`min(hi, MAXWIDTH)`, `MAXWIDTH` being `1 << 64`).
  - `hasattr(cls, name)` of a name a class with a builtin base doesn't have
    (`asyncio.isfuture`'s `hasattr(type(obj), '_asyncio_future_blocking')`)
    is remembered per class version, as it was for builtin classes only.
  - Tried and dropped: compiling call-heated code whose loops never ran
    (tier 2 takes loops by their back edges, so `parse` stayed interpreted
    until about 5,400 calls). `raytrace`'s `Scene.trace` first sees rays
    that miss and return before its loop, and compiled that early its hit
    path left for the core loop for good: 10% more instructions.
  - `_uniq`'s `list(dict.fromkeys(items))` runs in place: `dict.fromkeys`
    over a list or tuple of keys the native probes settle is a leaf (as
    `int.from_bytes` was), those probes take tuples of `None`, ints,
    strings, bytes, `int`-like instances and such tuples (`(RANGE, (48,
    57))`), and `list(d)` of an exact dict is its keys.
  - An instance whose class has a Python `__len__` and no `__bool__`
    (`not item` of a `SubPattern`) is tested natively, its `__len__`
    evaluated as a pure leaf; `obj[a:b]` of an instance with a Python
    `__getitem__` calls it directly from compiled code, as a subscript
    did; and compiled code builds `x[a:b:c]`'s slice (`BUILD_SLICE` left
    compiled code altogether). Parse: 774,000 to 659,000 instructions.
  - `s * n` of a `str`, `bytes` or `bytearray` copied once per repeat;
    doubling the copy takes 2.5% off importing a dozen common modules
    (`b"\0" * 65536` was 65,536 calls), and reading a cached module's
    identifiers checks their text as UTF-8 once instead of at every read
    (0.9% more).
  - A frameless leaf may build a tuple of up to three items (`return (x,
    1)`, a sort key's `(-x, x)`): `map` over such a function cost 4.3
    times CPython's instructions per item through tier 2's native entry,
    and a third of that now. (A native caller's callback that isn't a
    leaf still pays tier 2's entry, about six times a frame-JIT call's:
    `statistics`' `_sum` over `map(_exact_ratio, ...)`, 1.8 times CPython.)
  - Still open: `subpattern[-1] = ...` through a Python `__setitem__`
    leaves compiled code (a switched call's result would need discarding,
    which direct calls don't do), and a raise caught in compiled code
    registers once a parse.

- **Fewer registered direct calls, and frameless bodies with raise guards**
  (PR #107). A frame-JIT direct callee runs lazily until something needs
  the core loop's view, and then it registers with every lazy caller above
  it; `WEAVEPY_BURST_STATS` counts those per workload (`deepcopy` 103,000 a
  call, `exceptions` 79,000, `logging_mix` 69,000, `closures_decorators`
  68,000) and names each site. Fixed here (instructions on Linux):
  - `deepcopy`: `copy._reconstruct` spread a generator of the arguments'
    copies into the constructor (`func(*(deepcopy(a) for a in args))`), and
    draining a generator whose body runs Python code registers. Our `copy`
    builds a list in the same order instead: 10.6% fewer instructions.
  - `logging`: `StreamHandler.flush`'s `if self.stream:` left compiled code
    for a `StringIO` (a native object the compiled truth test didn't know),
    registering the four-frame handler chain once a record; it now answers
    native objects that are always true. A `warning()` call: 5.9% fewer.
  - `f(*args, **kwargs)` forwarding a non-empty `kwargs` to a frameless leaf
    (`closures_decorators`' decorated `area(i, h=2, scale=0.5)`) binds the
    keywords on the frame instead of registering.
  - `x.__class__` of a receiver no class can override (a coroutine in
    `asyncio.isfuture`) reads its type in the compiled code.
  - A body whose only impure instructions sit on paths bound for a `raise`
    (`if bad: raise ValueError(...)`) is a frameless leaf now: such a path
    declines before anything it does is seen, and the ordinary call raises.
    `sorted(xs, key=k)` with a guarded `k`: 43,024 to 17,283 instructions
    (an unguarded key 14,543).
  - The generic dispatch's check whether a builtin needs the interpreter (65
    name comparisons) is memoized by the name's address.
  - Compiled code builds tuples of up to 16 items (it left for the core loop
    past three: `findCaller`'s `return co.co_filename, f.f_lineno, ...`,
    another 1.8% off a `warning()`), and runs `DICT_UPDATE` (`{**a, **b}`
    32% fewer instructions, a closure's `cb(*args, **kwds)` 10%, an
    `ExitStack` with a callback 3%).
  - Tried and dropped: evaluating an `enum.property` `__get__` frameless
    from the core loop's descriptor lane (its evaluations declined, and that
    lane doesn't count declines toward giving up: 4.7% more on
    `Color.RED.value`).
  - A memory-safety fix the `__class__` read uncovered: with `isfuture` no
    longer leaving compiled code, `test_eager_task_factory` crashed (on
    Linux, at exit in mimalloc's free lists; valgrind found reads of a
    returned native frame's `LeanAct` from the pending list). A lazily
    running activation whose native body runs Python code (`create_task`
    starting an eager task) registered only once that code's own compiled
    calls were done, so its pending entries went on top of newer ones, and
    a newer entry's exit, finding someone else's on top, left its own
    behind. A state now remembers the lazy activation that was running when
    it took `lazy_top` (`State::outer_lazy`), and registering it registers
    that one first. The same crash followed any binary of the branch once
    `isfuture` ran to its end in compiled code.

- **Calls into builtins from frameless code and `functools.partial`** (PR
  #107). Found by a sweep of about 70 everyday operations against CPython
  3.14 (instructions on macOS, before and after, with CPython's):
  - `partial(operator.add, 1)(2)`: 3,944 to 977 (602). The core loop's
    partial lane took only Python callees, so a builtin one went through
    the generic call; it now runs a builtin's leaf half over the merged
    arguments. `operator`'s arithmetic functions have leaf halves for two
    plain ints or floats, so `operator.add(1, 2)` itself fell from 675 to
    508 (473).
  - `hash(obj)` with `def __hash__(self): return hash(self.x)`: 3,460 to
    2,210 (966), and storing such an object as a dict key 5,463 to 4,238
    (1,006). `hash`'s fast half wasn't on the list a frameless body may
    call, so the body ran framed, through a nested interpreter loop. A body
    whose first frameless evaluations all gave up (its global sites can
    resolve only after those first calls) now retries one call in 1,024,
    where it used to stop for good.
  - `{x for x in xs}`: `SET_ADD` probed the set twice for a new key; a
    comprehension of eight ints went from 5,820 to 5,272 (Linux).
  - `list(d.items())` of 20 entries: 15,521 to 7,591 (Linux; CPython 3.14
    about 7,200 on macOS). Draining a view stepped its iterator, with the
    mutation guards, an item at a time; nothing runs that could change the
    dict while a view drains, so the entries copy over in one pass. A
    native iterator drains under one borrow too (`list(enumerate(xs))` 6.6%
    fewer instructions).
  - Compiles: the code generator's SSA tables and the tier-2 analyzer's maps,
    all keyed by small integers, hashed with SipHash (2.5% of a cold
    `stdlib_utils` call); they now use an Fx hash. A cold call of
    `class_creation` retires 3.8% fewer instructions, `stdlib_utils` 1.8%.

- **`contextmanager`, exception construction and `len()` in compiled code**
  (PR #107). Instructions a call on Linux (callgrind), before and after:
  - A `with` of a `contextlib.contextmanager` function: 13,458 to 11,410.
    `__enter__` deleted three attributes (`del self.args, self.kwds,
    self.func`), and a deletion gives the instance private attribute names
    (an allocation each), so every later `self.gen` read missed the compiled
    code's layout guard (about 1,300 instructions of misses a `with`). Our
    `contextlib` now clears them to `None`, which releases them as early.
    `del` on an instance attribute stays this costly elsewhere (see Next).
  - `E("m")` of a Python exception subclass with neither `__init__` nor
    `__new__`: 2,486 to 1,816 (CPython 3.13 about 1,200); one whose
    `__init__` calls `super().__init__()`: 4,940 to 4,149. Compiled code's
    constructor call declined exception classes and fell back to the generic
    call, which reached the same native construction through four more
    layers. It now builds the instance and its `args` in place, and seeds
    `args` before entering a Python `__init__`.
  - `len(local)` in frame-JIT code (a generator body, say): the site fused
    the call but never filled its global cache, so every execution missed
    into the full load (about 330 instructions). The cache now remembers the
    fused shape, and the guard's hit computes the length.

  Over the suite the release build retires 0.992 of the instructions it did
  before (`unpack_sequence` 0.87, `context_managers` 0.92, `sudoku` 0.96,
  `exceptions` 0.98), and no workload more.

- **Slow paths found by timing everyday operations against CPython** (PR #107).
  Instructions a call, before and after, with CPython 3.14's:
  - Reading an attribute of an instance whose class defines `__setattr__` (a
    frozen dataclass, say): 692 to 83 (244). The specializer treated a custom
    `__setattr__` as overriding reads too, so every such read missed its
    cache; only a custom `__getattribute__` (or a `super` proxy) now keeps a
    read off the cached shapes.
  - `D()` for a `dict`, `list` or `set` subclass that keeps its base's
    allocator and `__init__`: 4,169 to 1,756 (1,168). The instance plan now
    knows that call is the allocation with an empty payload, where it built a
    bound `dict.__init__` and called it with no arguments.
  - `f"{s!r}{x!r}"` of a string and a float: 3,787 to 1,729 (3,503). The core
    loop converted ints in line but sent strings and floats to the full step.
  - `import decimal` no longer imports `_pydecimal` for its rounding-mode
    strings, which the compiler interns anyway (87 to 83 million), and the
    intern table hashes with Fx rather than SipHash.

- **`ast.parse` without the spec, and cheaper class creation** (PR #107).
  `compile(..., PyCF_ONLY_AST)` built a tree of spec dicts that
  `ast._from_spec` then walked, calling `cls.__new__` (a Python frame) and
  `setattr` per node and field: 78% of `ast.parse`. While the `ast` module,
  its builder and its node classes are as defined, the parse now builds the
  instances itself, with the same attributes in the same order, and anything
  patched (a class's `__new__` or `__setattr__`, a data descriptor, a
  finalizer, the registry, a tracer) still sees every call through the spec
  path. Boxing `Arguments` in `Lambda` brought `Expr` from over 200 bytes to
  72. `ast.parse` of CPython's `ast.py` went from 16.6 ms to 1.75 ms (CPython
  3.32 ms), and of a three-line source from 108 us to 14.5 us (CPython 17.3
  us). For classes: `type.__prepare__` and a no-op `object.__init_subclass__`
  aren't called; a metaclass's function read through a class, `cls.__class__`
  and `isinstance`/`issubclass` under a metaclass that keeps `type`'s hooks
  take leaf paths; `getattr(cls, '__dunder__', default)` proves a miss without
  building an `AttributeError`; and the frame JIT runs set literals. A class's
  base kept a dead `Weak` for every discarded subclass, which pinned each
  `TypeObject`'s memory; those are now pruned. `class K: pass` went from 7.9
  us to 4.7 us (CPython 5.8 us).

- **A code generator of our own** (PR #107, `perf/beyond-parity`). The frame
  JIT ran Cranelift at `opt_level=none`, paying about 11,000 host instructions
  and a kilobyte of working memory per IR instruction for register allocation,
  and tier 2's optimizer no longer paid for itself (scimark 309M instructions a
  call with it, 317M without). `crates/weavepy-codegen` takes the same SSA
  input through Cranelift's builder names and emits x86-64 (System V and Win64)
  or AArch64 code in one linear pass: block-local register allocation, register
  state carried down single-predecessor chains, merge blocks entered in the
  first edge's state, a few values pinned in callee-saved registers, and guard
  exits laid out of line. A simplification pass folds constants, divides by
  constants with magic numbers, drops trivial phis and identities, and threads
  jumps through empty blocks. All three JITs use it, so Cranelift and the
  vendored regalloc2 are gone (the binary is 6 MB smaller), as are the
  background compile queue, the Cranelift warm-up thread and the separate
  compile heap. Compiles cost about a quarter of what they did, so the frame
  JIT's threshold falls from 2,000 to 500 heat per instruction. A differential
  fuzzer compiles random functions, compares them with an IR interpreter, and
  checks that callee-saved registers survive each call.

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
- **PR #102, `perf/step-change`** (October 2026), stacked on PR #101. Warm time
  0.919 of PR #101's, cold 0.893, peak memory 0.908, instructions 0.897 (PGO, one
  session; instructions on a quick build). Direct calls on records and small
  callees in line have their own entries below. In the order they landed:
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
  - *A smaller frame JIT.* Most object operations call one helper through a
    site pointer, helpers write their own exit state behind one shared tail, and
    chunks come from a static estimate: IR 0.434 of before, machine code 0.462,
    lowering time 0.24, code generation 0.42. Instructions 0.982, peak memory
    about 0.96, wall time about even.
  - *Reference counts.* Every counted variant's pointer addresses its strong
    count, and with the bias held an update is one plain instruction, so a clone
    or release is a compare and an `inc` or `dec`. Instructions 0.969, with no
    workload higher.
  - *Lazy function bodies.* A cached function's body decodes when it first runs,
    so an unrun function costs 160 bytes rather than 384 and its vectors, and
    compile memory has an arena of its own that goes back to the OS. Peak memory
    0.902, instructions 1.010.
  - *The young collector and generators' ends.* Young sets record their position
    in the object, passes that find nothing back off, a native caller's step
    enters native code directly, and the end of a generator builds no
    `StopIteration` to catch. `async_tree` 0.86, `generators_tree` 0.96.
  - *Comparisons, class attributes and raising.* Comparisons that need no dunder
    stay in compiled code (`a != b` 6,884 instructions to 591), `__class__`,
    `__mro__` and friends are answered at the attribute site, and `raise C` costs
    3,403 against 5,953. `exceptions` 0.93, `class_creation` 0.96.
  - *Linux's first arena.* Its first 16 MiB go without huge pages (see "Memory on
    Linux").
  - *Correctness, each with a regression test.* A frame's values release in
    CPython's order, the last local first, on every path (they went forward);
    `gc.callbacks` fires for automatic collections (it never did); a
    `StopIteration`'s value survives native iterators; `KeyError` from `%` and
    `str.format` holds the key, not its repr; `1.0 == x` and `True == x` no longer
    run an `int` subclass's `__eq__` first; tier 2's `FOR_ITER` runs a finished
    generator's finalizers before the next statement.
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

- **Layout doesn't fix the instruction-cache misses** (PGO builds, November
  4). Hot-cold splitting (`-Cllvm-args=-hot-cold-split=true`) on top of the
  profile measured 0.994 of the plain PGO build's warm time, inside this
  machine's noise, and left the resident code pages where they were (7.1 MB
  after importing a dozen common modules, against CPython's 4.7 MB). Putting
  the functions a sampling profile of all 33 workloads found hot ahead of the
  start-up order took the resident pages to 7.5 MB. Building the core loop's
  two copies (`leaf_core::<true>` and `<false>`) as one cut `exceptions`'s
  simulated misses by 2%. The misses aren't in a few functions that layout
  could pack: `deepcopy` retires the same instructions as CPython 3.13 (58.0
  million a call against 59.8 million) but takes 4.3 times the misses (3.34
  million against 0.77 million) and 3.2 times the data-cache misses, spread
  over about 200 helpers at 0.2% to 0.5% each. Each call walks that many
  distinct functions, each with its own prologue and specialized code, so the
  lever is fewer distinct paths, not where they sit. Nor is it code size alone:
  a release build at `opt-level = "s"` (14% less text, 0.6 MB fewer resident
  code pages) ran 1.076 times the `opt-level = 3` build's warm time, slower on
  nearly every workload.

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
  isn't in. What is in: the first arena's first 16 MiB are mapped with
  `MADV_NOHUGEPAGE`, which only this mapping sees. Small programs stay lean and
  larger heaps get huge pages past that (`startup` 33 to about 25 MiB on the CI's
  Ubuntu runners, `deltablue` 64 to 38 MiB, `float_math` 2,598 faults against
  6,645 for a first arena without any), though Linux is still about 2.8 times
  CPython's at start-up against 0.9 on macOS.
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
- **Tried and dropped again (November 4)**: allocating a code object's inline
  cache table only when one of its instructions asks for a cache a second
  time, so run-once code never allocates it (174 KB of `import re, json`'s 399
  KB of tables). Peak RSS fell 0.9% over the suite, but a site whose first
  write was the only one it ever made stayed empty for good: a leaf body's
  attribute site served afterward from its field slot left `oo_patterns`
  compiled generically (2% more instructions), and blocks that ran once before
  a compile counted as never run (`nqueens` 35% more until fixed).
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
- **Instructions per cycle isn't a placement problem.** Simulated L1 instruction
  misses fell to 0.67 of the round's baseline (`go` 0.22, `raytrace` 0.57,
  `deepcopy` 0.80), but through the other work in this round: fewer instructions
  and smaller code. A contiguous code arena near the binary's text left misses
  unchanged (it needs an RWX mapping and a vendored `cranelift-jit`, so it wasn't
  kept), though Cranelift does start each finalize on a fresh page. Near, direct
  helper calls didn't lower cycles on `go` or `oo_patterns`. Ordering the binary's
  hot functions first moved misses between -0.8% and +4%: the 8-way L1 is
  capacity-bound, as `context_managers` touches about 2,700 distinct lines per
  iteration against CPython's 1,100. Data-side misses are spread over calls,
  allocation and clones, with no single offender. Sinking a frame's address
  computations into the block that uses them saves 0.2% and wasn't taken.
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

1. Exceptions, which explain most of `exceptions`, `context_managers` and
   `collections_mix` (140,000, 80,000 and 20,000 raises a call). A raise
   caught in its own frame costs 2,377 instructions against CPython 3.13's
   1,407, spread over a dozen helpers (`handle_exception_with` about 270,
   the exception-info ops through `h_container` about 260, building the
   instance about 300). A raise out of a compiled callee costs 6,431 against
   2,536: the caller switches to the callee rather than calling it directly,
   so the raise unwinds through the core loop and the caller's handler runs
   interpreted. That case now unwinds where the call helper sees the raise
   and the caller's compiled code catches it (6,049 instructions); the
   suite's raises come from inside handlers, whose cleanup covers them, so
   they still take the long way. But instructions aren't where most of the
   time goes: against CPython, `exceptions` retires 1.19 times the
   instructions, `context_managers` 1.29 and `deepcopy` 1.23, yet each takes
   about twice the time. Cachegrind (32 KB 8-way L1i) puts `exceptions` at a
   7.99% instruction-cache miss rate against CPython's 3.12% (239 million
   misses against 77 million), spread over hundreds of functions (the core
   loop's `quiet_run` and `leaf_burst`, `handle_exception_with`, compiled
   code, `Object`'s drop glue, `h_call`); the data cache misses 3.2% against
   1.2%. Footprint, through layout (a profile-guided hot/cold split) and
   fewer distinct paths for the same operation, is the larger lever.
2. `async_tree` and `context_managers`, the two worst workloads: mostly general
   calls, attribute stores and asyncio's own loop now, not generator mechanics.
   Exits that register a call and its whole chain of callers (`Rect.__init__`'s
   polymorphic stores, `DictUpdate`, `PushExcInfo`) want helper fallbacks.
3. Cold time: the lowering left on the interpreter's thread (a third of a
   compile) and the time a body runs interpreted before its code is
   ready; a much cheaper baseline compiler or compiled code kept across
   processes.
4. Peak memory: the file-backed pages (3.5 MB over CPython's: the
   interpreter's code is spread over many large functions, so hot-cold
   splitting with a profile is the next thing to try), and dropping a body
   again when its code stopped running (bodies, VM extensions and inline
   caches of code run at import are about 2 MB).
5. Builtins take keywords as `&[(String, Object)]`, so every keyword call
   of one allocates a `String` per keyword; a calling convention over the
   names tuple would remove it.
6. Tier 2 holds an object that already has weak references, read through
   an attribute, until its activation exits.
7. In-line reference counts in compiled code, now a compare, a flag test and an
   `inc` (the compile-cost rewrite took them out of line, and a count is about
   1.5% of the suite in `h_clone`), and freelists, which weren't reached.
8. The code cache has no checksum: a damaged artifact that still decodes gives
   wrong names or errors until it's replaced, and a function body that doesn't
   decode fails its run (the cache is then discarded). Reading every page to
   check one works against decoding bodies lazily.
9. Smaller gaps measured and left: `enum.Enum("E", "A B C")` takes 200 us
   (CPython 63 us), with no single cause (`EnumType.__prepare__` 11.5 us
   against 2.0, `EnumDict()` 1.4 us against 0.3, a dict subclass's
   `super().__init__()` 0.8 us against 0.1); `hash()` of a builtin function
   190 ns against 55; and an operand loaded from a global loses the in-line
   paths a local gets (`d[i] = i` 56 ns against 20). An in-line
   `LOAD_GLOBAL` hit path cost as many instructions as `h_global` and made
   the suite 1.4% slower through code size.
10. Divergences from CPython found and left: a tuple or list dropped outside the
   tuple freelist releases its items forward (CPython's go in reverse), as do
   operands of some native calls (`D("a") == D("b")`); closing a generator while
   its frame object is held unwinds its stack at once; an instance holding only
   scalars doesn't count toward a collection.
11. Calls from native code into Python are the widest remaining gap in
   everyday operations: a dict lookup keyed by an instance with a Python
   `__hash__` costs 341 ns against CPython's 60 (3,590 instructions, through
   `specialized_binary_subscr`, the reentrant probe and `pure_leaf_call`),
   `if x:` with a Python `__bool__` 286 against 54, `hash(x)` of a frozen
   dataclass 4,048 instructions against 1,736 (its `__hash__` runs in a nested
   interpreter: `call_python_bound`, `run_frame_lean`, `quiet_run`), an enum
   member's `.value` 336 ns against 132 (two inline calls, `__get__` then
   `fget`), and a `__getattr__` miss 375 against 107. Compiled code calling
   the same method directly takes 8.6 ns (CPython 29), so the cost is the
   protocol, not the call.
12. Tier 2 takes small loops that compiled callers then can't call
   directly. A function whose only loop is a filtered comprehension
   (`[x for x in s.split("/") if x]`, `pathlib`'s `_parse_path`) never gets a
   frame-JIT body: its callers' declines count heat only for leaves, call
   heat defers code with a loop to tier 2, and a callee tier 2 compiled is
   declined anyway. Each call then goes out to the core loop and the general
   frame run into tier 2: 11,100 instructions a call against CPython's
   2,700, and 4,500 with tier 2 off (`WEAVEPY_TIER2=0`). Tier 2 still pays
   for itself (off, the suite runs 1.051 times as long: `scimark` 1.59,
   `tomllib_loads` 1.43, `deepcopy` 1.25). Counting every declined call and
   letting compiled callers call such code directly halved those calls but
   was neutral on the suite (1.003; `raytrace` retired 10% more
   instructions, as loops tier 2 runs better moved to the frame JIT): the
   choice wants a loop's iterations per entry, which nothing records yet.
13. Smaller gaps measured and left: `import decimal` costs 83 million
   instructions against 14 (CPython's `_decimal` is C; ours executes a
   6,700-line module), `cProfile` runs about 90 times slower than CPython's
   (38 us an event), a bound method read without a call 153 ns against 61,
   `SimpleNamespace` attributes 128 against 45, and `Enum("E", "A B C")` 2.1
   times CPython's instructions.
14. `del obj.attr` takes the instance off its class's shared attribute names:
   the instance gets a private copy of them (`SplitValues::remove_str`), and
   every compiled or cached read of it misses from then on. CPython leaves
   the value's place empty and keeps the shared keys. Doing that here means
   every reader of split values, the JITs' in-line loads among them, treating
   an `Unbound` value as absent.
15. Memory, by subsystem. After `import re, json` the live heap is 2.2 MB
   above start-up, against CPython's whole 1.8 MB rise in RSS; the rest of
   ours is touched code pages. The largest heap parts: the VM's per-code
   extension (about 700 bytes for each code object that runs, plus its
   constants and names as objects, so string constants are held twice), code
   stubs and decoding (550 KB), inline cache tables (340 KB, 16 bytes an
   instruction of any code that runs, module bodies included), function
   objects (270 KB) and type objects (220 KB). The per-instance costs are
   larger than CPython's too: an empty instance takes 129 bytes (81), one
   with five attributes 224 (about 113), an empty list 112 (64) and an empty
   dict 159 (64); the collector's side tables cost a tracked list or dict
   about 64 bytes, where CPython's header is 16.

## Measurement notes

- Instruction counts (`/usr/bin/time -l`, or callgrind in the `wp-cg:latest`
  container) guide the work; wall time on a PGO build decides it. Pin
  `PYTHONHASHSEED=0` and unset `PYTHONDONTWRITEBYTECODE` for whole-process
  timings.
- Callgrind counts each iteration of `rep stos` as an instruction, so it
  overstates `memset` (zlib's state setup in `bytes_codecs`), and it shifts
  the JIT's time-budgeted compiles into later calls (`stdlib_utils`,
  `logging_mix` show compilation in steady state there, but not on macOS).

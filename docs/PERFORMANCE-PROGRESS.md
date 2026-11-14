# Performance progress

This note tracks the work to make WeavePy significantly faster than CPython
3.14 on realistic workloads, measured by wall time with a profile-guided
build, plus peak memory and startup. It records where WeavePy stands, the
plan, what's done, and what's next, so a new session can pick up the work.

## Where WeavePy stands

Measured on November 13, 2026 (this PR, quick builds, no fresh PGO run):
the benchmark suite's figures below still hold, but they aren't the whole
picture. A battery of 53 everyday operations
(`tools/pybench/everyday.py`: file and directory work, text, data
formats, containers, classes, threads, common library modules) put
WeavePy at 1.83 times CPython's time on average when it was first run,
with `class_features` and `enum_dataclass_ops` near 8 times,
`threading_queue_ops` 5.8, `os_listdir_walk` 4.8 and a dozen more past 3.
Before that, reading a file a line at a time was 360 times slower than
CPython and writing one 45 times, which no benchmark here had touched.
The I/O gaps are closed (see "Done"), as are the first few of the
battery's; most of it is still open and is the work's next target (see
"Next"). The battery hasn't been rerun as a whole since those fixes, and
its first run was on a loaded machine, so treat its ratios as a ranking.

Measured later on November 12, 2026, at `d1f48c5c` (this PR), a PGO build
from a fresh training run under Rust 1.100 against CPython 3.14.8 on the
same host: warm time 0.845, cold time 0.947, peak RSS 1.140. Against the
build below (`11c293b4`) in the same run the suite's time is 0.996: the
commits since are about operations the suite doesn't run (see "Done"), and
it shows. Whole processes take 0.90 of CPython's time (0.899 against
0.907 for the earlier build in one interleaved run), `-c pass` 0.70 (19.5
ms against 27.7). Peak RSS is 1.131 of CPython's and peak footprint 1.208
in one run of all three, against 1.112 and 1.199 for the earlier build:
all of that difference is `async_tree`, whose peak footprint reads 49 MB
in this build and 40 MB in the earlier one (see "What the measurements
say"). WeavePy is ahead on 22 of the 33 workloads warm and 18 cold. The
best warm times are `scimark` 0.52, `pickle_roundtrip` 0.55,
`json_roundtrip` and `unpack_sequence` 0.62, `raytrace` 0.63 and
`float_points` 0.64; the worst `async_tree` 1.28, `context_managers`
1.14, `collections_mix` 1.09, `deepcopy` 1.07 and `logging_mix` 1.06. The
worst cold times are `async_tree` 1.42, `stdlib_utils` 1.35,
`logging_mix` 1.24 and `tomllib_loads` 1.23.

Measured on November 12, 2026, at `11c293b4` (this PR), a PGO build from a
fresh training run against CPython 3.14.8 on the same host: warm time
0.854, cold time 0.961, peak RSS 1.124. The build two commits earlier
(`3765dc75`, the same training, before the early release of finished
code) read 0.845, 0.945 and 1.215 an hour before: the times differ by
what two runs do, and an interleaved A/B of the release puts them level.
Whole processes (start-up, import, the first call and three more) take
0.90 of CPython's time, `-c pass` 0.74, and a script that imports a
module of its own 0.75 (0.86 before: see "Done"). Peak footprint is 1.22
of CPython's (1.23 for `3765dc75` in the same session; CPython's own
footprint read lower when the ratio was first measured, at 1.29).
Against the build this PR started the day with (`672cca59`) the cold
time moved (0.977 then), with whole processes (0.95 then); peak RSS was
1.08 there for a release that cost short processes time (see "Done"),
1.22 without it, and is 1.12 now. WeavePy is ahead on 23 of the 33
workloads warm and 18 cold. The best warm times are `scimark` 0.50,
`pickle_roundtrip` 0.57, `raytrace` and `unpack_sequence` 0.60, and
`float_points` and `json_roundtrip` 0.62; the worst `async_tree` 1.19 to
1.25, `context_managers` 1.13, `class_creation` 1.08 to 1.10, and
`deepcopy` and `closures_decorators` 1.07 to 1.10. The worst cold times
are `async_tree` 1.38, `logging_mix` 1.37, `stdlib_utils` 1.30,
`tomllib_loads` 1.27 and `mini_interpreter` 1.25.

Measured earlier on November 12, 2026, at `672cca59` (this PR), a PGO build against
CPython 3.14.8 on the same host: warm time 0.854, cold time 0.977, peak RSS
1.081 (1.266 on November 10). Whole processes (start-up, import, the first
call and three more) take 0.95 of CPython's time, and `-c pass` 0.73. Peak
footprint, the private memory a process dirties, is 1.29 of CPython's and
is the memory figure to watch from here: see "Done". Against the build
this PR started from, in the same session, warm time is 0.858 against
0.863 and cold 0.973 against 0.978, so the time ratios moved little; what
changed is where the memory and the first-call time are known to go. The
best warm times are `scimark` 0.50, `pickle_roundtrip` 0.54, `raytrace`
0.62, `json_roundtrip` and `unpack_sequence` 0.63; the worst `async_tree`
1.25, `context_managers` 1.14, `deepcopy` 1.13 and `closures_decorators`
1.07. The worst cold times are `async_tree` 1.53, `stdlib_utils` 1.29,
`logging_mix` 1.26, `class_creation` and `tomllib_loads` 1.25.

Measured on November 10, 2026, at `57960cea` (PR #107, merged into `main`
as `0d8f6b62` on November 11), a PGO build against CPython 3.14.8 on the
same host: warm time 0.873, cold time 0.980, peak memory 1.266. The best
warm times are `scimark` 0.51, `pickle_roundtrip` and `unpack_sequence`
0.59, `float_points` 0.62 and `raytrace` 0.64; the worst `async_tree`
1.27, `logging_mix` 1.15, `context_managers` 1.14 and `deepcopy` 1.12.
The checkpoints under "Done" record the later builds, whose ratios to
CPython aren't comparable because CPython's own numbers moved between
sessions.

Measured on November 9, 2026, at `b88adc0f` (PR #107), a PGO build against
CPython 3.14 on the same host: warm time 0.900, cold time 1.024, peak memory
1.257. Against the November 8 PGO build (`1cef5111`), measured directly in
the same session, peak RSS is 0.981 (all 33 workloads lower: `sqlite_queries`
0.96, `scimark` 0.975), and warm time is level but for `nqueens`, whose
1.59 is the alignment cliff described under "Done" (this binary's path puts
pybench's environment on it; the November 8 binary's didn't). CPython's own
peak RSS measured about 2% lower this session, so the memory ratio reads
level. The worst warm times are `async_tree` 1.46, `context_managers` 1.21,
`logging_mix` 1.14 and `deepcopy` 1.11; the worst cold `async_tree` 1.83,
`stdlib_utils` 1.45 and `logging_mix` 1.38.

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

- **Comparison methods, descriptors, thread-locals and queues** (this PR,
  November 13). The everyday battery's class and thread cases, taken one
  operation at a time:
  - Comparison methods. A `COMPARE_OP` whose left operand's class has a
    Python `__eq__`, `__lt__` or other comparison method ran it as a
    nested general call unless the body was a pure leaf. Compiled code
    now calls a method that has native code as a direct activation, the
    way `BINARY_OP` calls an operator method. The operands wait on the
    caller's stack under the call, so `NotImplemented` goes on to the
    reflected method, identity or the `TypeError` without the method
    running twice, and the instruction's fused `TO_BOOL` takes the truth
    of an answer that isn't a `bool`. `NotImplemented` against `None`, a
    number or a string is settled in the helper (`x == None` for a
    dataclass). `a == b` with a dataclass-shaped `__eq__`: 6,860 to
    2,200 instructions (CPython 1,920); a dataclass's `==` 5,190 to
    2,070 (1,980) and `<` 6,470 to 2,320 (2,020); `a == None` 4,780 to
    1,450 (1,060).
  - The same bodies elsewhere. Two tuples built for a comparison left
    compiled code every time (the helper answered only for operands
    something else still held): `(a.x, a.y) == (b.x, b.y)` 2,040 to
    1,040 instructions (1,010). `x.__class__` is one helper call, in
    inlined bodies too, where it took the attribute-miss path at about
    700 instructions a read: `a.__class__ is b.__class__` 640 to 360
    (330), and a plain call of the `__eq__` body above 3,990 to 1,550
    (1,410), `a + b` with that body 4,650 to 1,520 (1,770).
  - Python descriptors. A data descriptor written in Python has its
    `__get__` called directly from the compiled attribute site, where
    the read went through the miss path, the property path and the core
    loop's lane first: `Color.RED.name` 3,810 to 1,790 instructions
    (1,500).
  - Enum classes. A metaclass's `__getitem__` is called directly from
    the compiled subscript site, and a class's own containers read
    through a class under a metaclass (`cls._member_map_`) are
    remembered per site under both versions: `Color["RED"]` 3,470 to
    1,480 instructions (610), `Color(1)` 6,270 to 4,660 (2,670).
  - `threading.local`. Its attribute hooks are builtins. Each thread
    remembers, weakly, the namespace the shim gave it for a `local`
    object, so only a thread's first touch runs Python; compiled code
    calls the read hook in place. A read 4,330 to 1,020 instructions
    (850), a store 5,650 to 2,260 (960).
  - `queue.SimpleQueue`. `get`, `get_nowait` and `put_nowait` are
    builtins beside `put`, and all of them read the queue's fields and
    its deque in place instead of through six general attribute reads a
    call. Put and get 13,850 to 3,040 instructions (900), the `nowait`
    pair 19,100 to 3,140 (800). An empty queue or a timeout still goes
    to the shim's Python.
  - `datetime` methods written in Python. `astimezone()` took 333,000
    instructions a call (CPython 24,500). `_pydatetime`'s methods read
    `self._year`, `self._tzinfo` and the rest by their slot names, and a
    natively packed value has no slot to read, so some fifty reads a
    call missed every compiled path and took the general handler. The
    native field read serves the slot names as it serves the public ones:
    337,000 to 200,000 instructions, `replace(tzinfo=...).timestamp()`
    15,500 to 11,900 (4,700), and the battery's datetime case retires
    27% fewer. What's left of `astimezone` is the Python around seven
    `localtime` calls, with no single cost past 10%.
  - A regression sweep of the battery by instruction count, with the
    compiler synchronous so that a background compile's timing doesn't
    move a case (it moved `bytes_ops` by 5% between builds that are
    identical with it synchronous), found two regressions from earlier
    in this PR. A compiled attribute site asked on every miss whether its
    read was a Python descriptor's (it asks once a class version now),
    and `set(xs)` of a long list went through the pure-constructor path,
    which built the set unsized: 142,800 instructions against 116,600
    before for `set(xs) & set(ys)` of 300 and 200 ints (119,600 now; the
    path sizes its set and leaves inputs past 64 items to the general
    constructor). Left alone: `xs.sort(key=lambda x: -x)` retires 4%
    more than at the start of the phase (179,000 against 172,700 for 300
    items; CPython 199,800).
  - Fixed on the way, each found by a new test checked against CPython:
    `COMPARE_OP` releases its right operand first, as CPython does (two
    dying temporaries' finalizers show the order); a pure-leaf
    comparison whose operand died with a finalizer ran it late; a class
    that gains or loses `__get__`, `__set__` or `__delete__` after its
    instances are other classes' attributes left stale instance-field
    caches behind (every class's version moves now: CPython's
    specializer refuses such descriptors instead); a `threading.local`
    subclass ran `__init__` twice on the creating thread.
- **File objects, standard streams and the first everyday cliffs** (this
  PR, November 12 and 13). Found by timing ordinary scripts instead of
  the suite:
  - Buffered reads. A line read took one `read(2)` per byte. Lines now
    come out of a read-ahead the backend fills a block at a time (any
    stream but a raw `FileIO`); a seekable descriptor gets the unread
    bytes back before anything else looks at its offset, `tell()`
    subtracts them, and a pipe keeps them parked. `StringIO` and
    `BytesIO` scan their buffer in place. Reading 20,000 lines: 1,049 ms
    to 2.7 ms (CPython 2.9).
  - Buffered writes. Text files and read/write binary files hold their
    writes as binary writers already did; a text stream on a terminal or
    opened with `buffering=1` flushes at each line end. Streams still
    holding bytes at shutdown are flushed then (a file kept alive by a
    reference cycle lost its tail before). Writing 20,000 lines: 72 ms
    to 2.1 ms (CPython 1.6).
  - Standard output holds a 128 KiB block when it isn't a terminal, as
    CPython's does, flushed at `flush()`, by `input()`, after each
    interactive statement and at exit. 300,000 `print` calls into a
    file: 1.38 s to 0.23 s (CPython 0.20). A broken pipe now reports as
    in CPython (`Exception ignored while flushing sys.stdout`, status
    120). `sys.stdin` iteration: 0.39 s to 0.10 s for 300,000 lines.
  - Lines and writes handed over in place. A stream works out once what
    its encoding, error handler and newline mode mean; a line whole in
    the buffer is copied out once with no decoder, without publishing
    the loop's frame, and `write()` appends to a buffer that already
    holds writes. Both are leaf halves, and a leaf site now serves a
    bound method held in a local (`write = f.write`). Per line, less the
    loop: iterating a text file 960 instructions (CPython 838),
    `f.write(s)` 556 (579), `print(a, b, file=f)` 4,054 (5,214). Text
    `read(n)` read a byte per call: 39,000 to 2,150 for five characters
    (1,200).
  - The shapes those halves declined, and what a decline cost. A leaf
    whose fast half declines used to fall to the full call handler,
    where an unregistered builtin's body runs in the core loop's lane:
    registering `write` and `readline` halves made every stream they
    didn't cover slower (a CRLF file's `readline()` 3,630 to 5,620
    instructions, a latin-1 file's `write()` 4,330 to 5,820). A declined
    leaf now runs its body through the lane, and the halves cover what
    they declined: line ends of every kind under every newline mode
    (translated and tallied in place), latin-1 and ASCII text, writes
    whose `\n` becomes `\r\n` or `\r`, and `readline(limit)`. A CRLF
    `readline()` is 1,510 instructions (CPython 1,040), a latin-1
    `write()` 990 (990), a `newline="\r\n"` `write()` 1,090 (2,670).
    A text `readline(n)` also counts characters now, as CPython's does;
    it counted bytes, and a limit inside a multi-byte character raised
    `UnicodeDecodeError`.
  - `os.DirEntry` built seven closures per entry and ran a `stat` for
    every `is_dir()`, `is_file()` and `is_symlink()`. Its methods are
    the type's, it answers from the directory read's `d_type`, and it
    keeps the `stat()` results it has fetched. `os.scandir` and
    `os.walk`: 4.3 to 1.5 times CPython's instructions, and no `stat`
    per entry.
  - A native method under a dunder name read through an instance
    (`lock.__enter__`) took the full attribute path every time, which is
    what `threading.Condition` does on each `with`, under every
    `queue.Queue` and `threading.Event`. `lock.__enter__()` 4,415 to 375
    instructions (CPython 730), `with cond:` 13,050 to 4,600 (2,400), a
    queue's put and get 49,600 to 32,200 (11,800).
  - Attribute hooks. A class's Python `__getattribute__`, or its
    `__getattr__` once the default lookup certainly misses, runs as an
    inline activation (CPython's `LOAD_ATTR_GETATTRIBUTE_OVERRIDDEN`),
    and `object.__getattribute__(obj, name)` has a leaf half for a plain
    instance's own attributes and slots. A constant hook 3,466 to 1,845
    instructions (CPython 590), one that delegates to
    `object.__getattribute__` 9,844 to 2,786 (1,240), the bare call
    4,019 to 657 (950), a `threading.local` read 11,400 to 4,160 (740).
    A Python `__setattr__` runs inline the same way (6,790 to 5,190
    against 2,000: what's left is the `object.__setattr__` call inside
    it, which still takes the general path).
  - `cls.__name__` and `cls.__module__` are remembered at a compiled
    site for the class's version: 1,180 to 200 instructions (CPython
    440), `type(obj).__name__` 1,446 to 496 (672).
  - `obj.__class__` took the general attribute path at a compiled site:
    1,360 to 438 instructions (CPython 433), and `other.__class__ is
    self.__class__` 2,507 to 681 (363). A tuple or list holding lists
    compares natively, as ones holding only scalars and tuples did:
    `(1, 2, []) == (1, 2, [])` 2,819 to 627 (580). Both are in every
    dataclass comparison (`==` 6,820 to 5,230 against 1,910: the rest is
    the `__eq__` call itself, which only a pure-leaf body avoids).
  - Zero-argument `super().name` was fast only for plain methods. A
    plain value a base class holds is read in place (5,540 to 1,630
    instructions against CPython's 1,020) and a class method is bound to
    the receiver's class in place, from an instance method or a class
    method (`super().create()` 6,970 to 2,640 against 1,710). A property
    through `super()` still takes the full path (6,370 against 1,190).
  - Float keys and plain tuples. A dict or set probe by a finite float
    takes the native probe that `str` and small `int` keys had (`d[2.5]`
    2,455 to 1,050 instructions against 600; a compiled site still
    leaves for the core loop to do it), and `hash()` of a tuple of any
    plain scalars, or of `bytes`, is a leaf (`hash((1, 2.5))` 1,386 to
    500 against 580, `hash(b"...")` 1,497 to 370 against 560).
  - Builtin constructors at a call site. A sweep of every builtin called
    with plain arguments found the ones still taking the general call:
    `bool(container)` 3,020 to 860 instructions (CPython 910),
    `slice(a, b)` 3,070 to 1,060 (1,160), `set(xs)` 3,510 to 1,980
    (1,640), `frozenset(xs)` 4,110 to 2,490 (1,660), `vars(obj)` 4,520
    to 905 (1,390), `pow(b, e, m)` 9,020 to 700 (1,290). Still general:
    `globals()` 7.9 times CPython, `object()` 3.4, `bytes(n)` 3,
    `dict(a=1)` and `dict(d)` 2.9 and 2.6, `setattr` 2.4. (A leaf half
    that declines costs more than no leaf half: the call then takes the
    general path, not the native-body lane. `pow`'s covers every plain
    numeric shape for that reason. It also fixed `pow(10.0, 400)`, which
    returned `inf` where `10.0 ** 400` raised `OverflowError`.)
  - A metaclass's Python `__getitem__` (`Color["RED"]`) runs as an
    inline activation, as an instance's class's does (3,860 to 3,465
    against 620: its body's `cls._member_map_` read and the exit from
    compiled code to reach it are what's left).
  - The csv reader re-read its dialect's eight attributes for every
    row, allocated a key string per state access, and stepped a
    character at a time: 3.4 to 2.0 times CPython.
  - Also: `input()` flushes standard output whether or not it has a
    prompt, and a read to the end after a sized read no longer stops at
    what was read ahead (both found by CPython's tests once output was
    buffered).
- **Everyday shapes the suite doesn't run** (this PR, November 12). The
  33 workloads are tuned to: the general interpreter's `step` is under
  0.25% of any of them. `tools/pybench/microops.py --instructions
  --locals` runs 153 single operations, and there WeavePy's geometric
  mean is 0.97 of CPython's instructions with 61 operations above 1.3
  (see "What the measurements say"). Fixed so far:
  - Keyword calls of native callees. A builtin type's method called with
    keywords (`xs.sort(reverse=True)`, `s.split(maxsplit=1)`) or a builtin
    type (`int(s, base=2)`, `enumerate(xs, start=1)`) left compiled code
    for the general interpreter on every call: 3,700 instructions for the
    `sort` against CPython's 1,080. The builtin keyword lane
    (`Interpreter::core_builtin_kw_lane`) takes them as it took builtin
    functions, and keeps its keyword pairs in a pooled vector whose name
    strings a site's next call finds already written (it allocated a
    `String` per keyword per call). On a build without PGO the `sort` now
    takes 2,540 instructions, `split` 2,780 (3,940), `min(xs, key=abs)`
    3,270 (4,240). The rest is the general call's dispatch by builtin
    name and the natives' own argument handling.
  - Tier 2 kept loops it ran badly. It didn't count a keyword call of a
    class or of a native callee against a loop, on the premise that tier
    1 paid the same generic binder. The frame JIT has since learned to
    bind a class's `__init__` directly and to run native keyword calls in
    place, so a loop of `Point(i, y=2, z=3)` stayed in tier 2 at 7,100
    instructions an iteration, against 2,400 in the frame JIT and
    CPython's 3,400. Those calls are charged like any other round trip
    now, and a loop dense in them retires to the frame JIT within one
    poll interval.
  - Locked instructions. `Rc`'s counts are plain loads and stores while
    one thread runs, but `get_mut`, `make_mut`, `try_unwrap` and
    `into_inner` still went through `Arc`'s locked compare-and-swap, and
    a weak handle's clone and drop through its locked increment and
    decrement: about twenty cycles each on this x86, counted as one
    instruction. With the bias held a sole owner now reads its two counts
    and a weak handle updates its count in place (`rc::sole_owner`,
    `Weak`'s `Drop`); `ThinArc::get_mut`, a frame slot's upgrade and a
    traceback's hold on its frame shell do the same. Instance recycling,
    the tuple pool, frame shell recycling, sequence unpacking and the
    deferred-container list each ran one or two per operation: a warm
    call of `mini_interpreter` executed 261,000 locked instructions and
    now executes 900, `sudoku` 196,000 and 400, `tomllib_loads` 207,000
    and 4,700. A loop over `enumerate()` ran two per element (the tuple
    pool's), a seventh of its cycles.
    `target/wt/target/pc-cg/lockcount.py` counts them per function
    (callgrind per instruction, against the binary's disassembly).
  - Loops tier 2 ran worse than the frame JIT. A compiled loop over a
    dict or one of its views took the general iterator step for every key
    (the iterator cloned, a checkpoint, the checked step: 640 instructions
    a key); it takes the core loop's in-place step now, as list, tuple and
    range iterators did, and `for k in d` over ten keys went from 7,900
    instructions a pass to 4,500. An OSR entry and its exit cost about
    4,000 instructions, and the verdict on a loop's entries asked only
    that they average eight iterations: `for x in sorted(xs): pass` over
    ten items ran 7,400 instructions a pass through its entries against
    the frame JIT's 3,400. The minimum now scales with the compiled
    code's size (`osr_min_trips`: 64 iterations for an empty body, eight
    from fifteen statements), so such loops are left to the frame JIT.
  - Tried and reverted: counting a keyword call of a pure leaf against
    its loop (`f(i, c=4)` with a keyword-only parameter runs 1,040
    instructions in tier 2's frameless evaluation against 220 at the
    frame JIT's call site, so a loop of only that call went from 1,040 to
    320). A loop that makes one such call among six other calls still
    runs better in tier 2: the bench gate's `call_overhead` took 15% more
    instructions once the call retired its loop, and 45% more time on
    CI's Linux runner. The frameless evaluation itself is what to make
    cheaper.
  - The frame JIT's `FOR_ITER` helper hands a dict's and a dict view's
    keys and values and a reversed list's items to a step of their own
    (`h_for_iter_view`) instead of the general step: ten of a dict's keys
    3,370 instructions a pass (3,590 before, CPython 1,950), its values
    3,690 (3,920), `reversed(xs)` 3,840 (4,140). Stepped in the first
    helper itself they ran 230 fewer still, but that helper's larger
    frame cost every other loop: a string's 11% more instructions an
    item, `enumerate`'s 3%. The battery caught it after the push.
  - Over the battery's 153 operations the geometric mean of WeavePy's
    instructions over CPython's went from 1.045 to 1.030 on builds
    without PGO (`OrderedDict` iteration 36,000 to 18,600, a tuple
    literal 590 to 460, `list.sort` of ten 4,070 to 3,380, `Color(1)`
    7,000 to 6,150). Most of what these commits fix isn't among its
    cases.
  - A dying tuple whose items other owners keep (`enumerate`'s `(i, x)`,
    a call's `*args`, `zip`'s pairs) gives their counts up in place and
    frees its allocation, instead of going through the trashcan that
    guards deep recursion and the slice's drop glue (about 100
    instructions before its items). An item the tuple is the last owner
    of ends the walk and the nested release takes over, so deep nests and
    finalizers go as before. `(i, i, i)` built and dropped: 540
    instructions to 450 without PGO (CPython 480); `zip` over ten pairs
    11,430 to 10,720.
  - A function's keyword-only defaults are matched to its parameters
    once. A positional call of a function with keyword-only parameters
    asked twice whether its compiled defaults line up with them,
    comparing every name each time (250 instructions a call for five).
    The answer is kept in a byte of the function's padding
    (`PyFunction::kwonly_memo`), reset with its code. `f(i)` of `def f(a,
    *rest, x=1, y=2, z=3, w=4, v=5)`: 1,650 instructions to 1,170 (CPython
    1,240); `Color(1)` on an `Enum`, whose metaclass's `__call__` has
    five: 8,850 to 7,920; `json.dumps` of a small dict 6% fewer.
  - Tried and dropped: a tuple allocated and filled directly instead of
    through `Arc`'s slice constructor (no fewer instructions for a call's
    `*args`), and skipping the tuple pool's clearing walk when its bucket
    is full (35 instructions an element less for `zip`, as many more for
    `enumerate`, which borrows the pool once more).
  - A module that names `__spec__` or `__loader__` in its own code gets
    the pair before its body runs. The lazy pair of the commit before
    (see "A program's own modules import without `importlib`") covered
    attribute reads, but a module that read `__spec__` as a global found
    the `builtins` module's, since a name its dict lacks is looked up
    there next: pydantic's `__init__` builds its lazy imports from
    `__spec__.parent`, and failed to import (CI's ecosystem shards had
    been failing on it and on Pillow's self-test since that commit).
  - Two bugs the new tests found: `list.sort` ignored an unknown keyword,
    and raised `TypeError` for a `list` subclass's instance when called
    with keywords (`super().sort(reverse=True)`).

- **The binary's one-time code leaves the resident set** (this PR).
  Peak RSS on macOS, PGO builds, against CPython 3.14.8.
  - A run executes far more code than it keeps running. Traced on Linux
    with callgrind, `deepcopy` executes 3.2 MB of code in all, on 7.0 MB of
    pages, and one warm call 0.28 MB of it: start-up alone runs 1.5 MB (on
    3.5 MB of pages), and imports and compiles most of the rest. Every such
    page stayed resident. On a PGO build the process had 8.8 MB of the
    binary's code resident after a small workload, against CPython's 4.8
    MB, which was most of the gap in peak RSS.
  - The process now releases its code and constants' pages
    (`crates/weavepy-cli/src/text_trim.rs`): on macOS it maps the binary's
    file over its `__TEXT` segment again, on Linux it advises the image's
    read-only segments away. What the program still runs comes back a soft
    fault a page, about 2.3 microseconds each (5,000 instructions, most of
    them the kernel's), so a release costs two or three milliseconds. A
    traced process is left alone (a debugger's breakpoints are private
    copies of code pages), and `WEAVEPY_TEXT_TRIM=0` turns it off.
  - When to release took two tries. The first rule was a timer: at 10 MB,
    then after waits that doubled from 2 ms. In a 160 ms process that was
    six releases, each in the middle of start-up or an import, where nearly
    all of the 5 MB it dropped came straight back: 3,500 more page faults a
    process, 2,400 of them before the first call. The suite's whole
    processes took 1.5% longer for it, the benchmark modules' imports 28%,
    and a script that only imports 3% to 8% (`import logging` 64.5 ms
    against 60.5).
  - The rule now waits for imports to be over: no module body running and
    none started or finished for a millisecond (the runtime stamps each,
    `weavepy_vm::import_activity`), the resident set within a sixteenth of
    its highest, and 3 MB of file-backed memory gained, over what the last
    release left if imports ran since and otherwise over what had come
    back. Waits that double between releases, from 16 ms, bound a program
    that keeps importing. Most programs release once, a millisecond or two
    after their last import, and a script that only imports never does.
    Whole processes take about 1% longer than with no release, first calls
    1% to 2%, warm calls the same, and peak RSS is 0.87 of theirs (the
    timer's was 0.765, for the costs above).
  - And it waits for the process to be 100 ms old. The rule above fired a
    millisecond after a short script's last import, which is where CI's
    bench gate times its kernels: those of a millisecond or two ran 15%
    longer on Linux (`sumvm` 1.1 to 1.5 ms, `nested_loops` 2.2 to 2.6),
    and on Apple Silicon start-up took a quarter longer (12.9 to 16.1 ms).
    A release is two or three milliseconds for memory a short process is
    about to give back anyway, so the first one now waits until that's a
    fiftieth of the time the process has taken. Over the suite's
    half-second processes the release then takes 4% off peak RSS instead
    of 13% (most of them peak in their first 100 ms); a process that
    lives longer releases as before.
  - Peak RSS came to 1.12 of CPython's before the wait (1.30 with no
    release, and 0.99 with the timer), but RSS counts clean, file-backed
    pages the kernel can drop at no cost. By peak footprint (`/usr/bin/time -l`'s
    figure: private, dirty memory) WeavePy takes 1.29 of CPython's whatever
    the rule, and that's the gap that is left: see "What the measurements
    say".
  - The runtime ticks for it (`set_memory_tick_hook`): when a cycle
    collection comes due, every sixteenth time the eval breaker's countdown
    runs out, and when the JIT's worker goes idle
    (`crates/weavepy-cli/src/memory.rs`).
  - `tools/pgo/order.py` writes the linker's order file for a PGO build
    from its own training run. The checked-in file's names matched 5,291
    of the PGO build's 23,600 functions, and some of the hottest (the
    generic drop of an `Object`) weren't in it. The trainer runs each
    benchmark twice, for three calls and for seven: what the longer run
    executed at least 1,000 times more is what a program keeps running
    (2,523 functions), and comes first, the functions most benchmarks share
    ahead of each benchmark's own. The rest of what the training ran
    follows, start-up's in its first-use order.
  - `tools/pgo/build.sh` builds only the `weavepy` binary: the
    `_testembed` twin was a second fat-LTO link in each of its two builds.
  - A process past 32 MB keeps freed arena ranges resident for 500 ms
    instead of 50: `async_tree` builds and drops about 13 MB a call, which
    was purged and faulted back in every time.
- **Tier 2 analyzes a large body only after work in proportion to its
  length** (this PR). Tier 2 tries code after fifty calls or loop
  iterations, whatever its size, and an analysis costs four to seven
  microseconds an instruction, more when it carves loops out and starts
  over. Over the suite's first calls that's 741 attempts, 140 of which
  compile, and 3.7% of the time; with tier 2 off the first calls take the
  same time in all (1.002: `scimark` 1.51, `tomllib_loads` 1.37 and
  `nqueens` 1.19 against `float_points` 0.90, `regex_mix` 0.91 and
  `chaos` 0.92) and the imports 0.96 of it. The largest single costs were
  bodies it then rejects: `regex_mix` spent 15 of its first call's 64 ms
  on `re`'s compiler (`_parse`'s 1,700 instructions, `_optimize_charset`
  six times over), and `dataclasses._process_class` took 3 ms. A body
  past 256 instructions now needs sixteen calls or iterations for each
  instruction past that, in any phase. `class_creation`'s first call
  retires 13.5% fewer instructions and `regex_mix`'s 6.6%; the other
  workloads and the bench fixtures retire what they did. Asking every
  body for work in proportion to its length (eight an instruction) saved
  more there (20% and 7%) but moved which entries tier 2 compiled and
  when the frame JIT took over elsewhere: `chaos` retired 4% more
  instructions over four calls, `tomllib_loads` and `stdlib_utils` 2% to
  3%.
- **Code a program is done with is released at once, off its thread**
  (this PR). The 100 ms wait above gave back most of what the release
  had bought on short processes, because a full release costs the
  program: the system call, then a fault for every page of code it still
  runs. A profile-ordered binary knows which code that is.
  `tools/pgo/order.py` now puts its hot group (what the training's
  programs keep running: 2,583 functions, 7.8 MB) between two marker
  functions, `weavepy_text_steady_start` and `weavepy_text_steady_end`,
  and the runtime releases only what lies past the second, with the
  constants: what start-up, imports and compiles ran. That needs no
  wait. It happens once, a millisecond after imports go quiet, on a
  short-lived thread of its own (the call takes 0.16 ms there, 0.6 on
  Linux), and the full releases follow by their own rule, counting from
  what this one left. Over the suite it frees about 3.6 MB a process:
  peak RSS is 0.92 of what it is without, and wall time, first calls and
  warm calls are the same to within three parts in a thousand, in two
  interleaved pairs. A binary linked without such an order (a plain
  release build, whose checked-in order file has no markers) has the two
  functions side by side and is left as it was;
  `WEAVEPY_TEXT_TRIM_REST=0` turns it off.
- **A program's own modules import without `importlib`** (this PR). The
  native importer loads a source file itself, but then built the module's
  `__spec__` and `__loader__` before running its body, and building them
  is what imports the machinery: `importlib`, its two bootstrap halves,
  `zipimport` (and `time`), `importlib.util` and `types`. That was 4.9 ms
  on a program's first import of a file of its own, where the import
  itself takes 0.4 (CPython takes 0.3 for all of it: its machinery is part
  of start-up). `-c pass` doesn't show it, and the suite's harness paid
  it in every process. The pair is now built before the body only when
  `importlib.machinery` is already loaded (then it's a call); otherwise
  it's built when something reads `module.__spec__` or
  `module.__loader__`, as the frozen standard library's modules have
  theirs built and as `__main__`'s loader already was. Until then the
  module's `__dict__` holds neither name, where CPython's holds both: a
  program that reads them as dictionary entries without ever importing
  `importlib` (or `inspect`, `pkgutil`, `unittest`, ..., which do) sees
  the difference. A script that imports one module of its own starts in
  0.75 of CPython's time, where it took 0.86 (22.1 ms against 25.4, with
  CPython at 29.7).
- **A busy worker is noticed before a compile's worth is weighed** (this
  PR). The frame JIT keeps one compile in flight, so while the worker
  generates code every other hot function's trigger comes back a
  twentieth of its heat later. Each of those weighed the code first, a
  pass over every instruction: nothing on an idle machine, but where the
  worker gets no core of its own (under valgrind, `go` weighed code
  39,000 times a call, 4% of its instructions) the interpreter's thread
  spent its time deciding again. Two compiles in flight instead of one
  would have code ready sooner (first calls retire 0.3% to 0.9% fewer
  instructions), for 1.2 MB more peak footprint on `go` and 0.4 MB on
  `tomllib_loads`: left at one.
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
  - `x.__reduce_ex__(proto)` with `object`'s own method, for a plain
    instance whose class keeps `object`'s `__reduce__`, at protocol 2 or
    more (what `copy.deepcopy` calls through its saved bound method), is
    reduced in place from compiled code instead of leaving it for the core
    loop's lane: `deepcopy` 1% fewer instructions. Branch relaxation keys
    its dropped fixups by index instead of hashing their offsets.
  - Tried and dropped: recycling an empty `**kwargs` dictionary from a
    finished call for the next (the pool the pure-leaf path uses): level
    within noise, mimalloc's allocation being cheap; and a four-class
    cache for how instances answer a boolean context (`deque`'s truth in
    `ExitStack.__exit__`'s loop): level, the one-class cache wasn't
    thrashing there.
  - Smaller generators, frames and collector entries. A peak-heap census
    of the worst-memory workloads put suspended coroutines first: in
    `async_tree` they were 36% of the peak heap, about 530 bytes each in
    five allocations against CPython's 220 in one. Three layouts shrink
    without changing what holds what, each to a smaller mimalloc class: a
    `PyGenerator` holds its code as an `Rc<CodeObject>` and its rare
    extras (a set name, an origin, an async generator's finalizer) behind
    one set-once pointer, 64 bytes instead of 80; a frame's operand stack
    keeps its depth and capacity as 32-bit counts, so the boxed `Frame` is
    112 bytes instead of 120 (128 with rounding); and a collector registry
    entry drops its collection-time count and node number, which a
    collection now keeps in arrays of its own, by node (a candidate's node
    is its generation's base plus its position in the list), 16 bytes
    instead of 24, CPython's per-object header. A plain generator takes
    400 bytes instead of 440, and a task parked on a future 1,352 instead
    of 1,534 (CPython 823). Peak RSS falls on all 33 workloads, 1.6% on
    the geometric mean (`chaos` 4.0%, `async_tree` 3.2%, `go` 2.9%), at
    0.998 of the instructions.
  - A future's and a task's callbacks and awaiters are two-word vectors
    (`compact_vec.rs`: a buffer and 32-bit length and capacity), which
    puts their state in the 192-byte allocation class instead of 224: a
    future takes 397 bytes instead of 418, and `async_tree` peaks about 1%
    lower.
  - A function's defaults, keyword defaults and closure are compact
    vectors too, which puts a `PyFunction` (176 bytes) in the 192-byte
    class instead of 224: peak RSS 0.2% to 0.8% lower where many
    functions live (`stdlib_utils`, `logging_mix`).
  - The linker order file is rebuilt from callgrind profiles of start-up
    and of every workload at its full size, twice over (a single short
    run profiles the compiles, not the compiled code's helpers): the 26
    commits since November 6 had added hot functions (`class_poly_hit`,
    `reduce_ex_plain`, the code generator's branch relaxation) that sat
    scattered among cold code. Peak RSS falls about 1.2% across the
    suite, at 0.997 of the cycles.
  - The collector's cache of what a child object is (a candidate, a
    temporary, or neither) is two-way set associative instead of
    direct-mapped. Every suspended generator's frame refers to its module's
    and the builtins' dictionaries; when those two happened to share a
    line, each visit evicted the other and took the registry's hash
    lookup, and a full collection over 20,000 parked generators cost 18.0
    million instructions instead of 12.7, depending only on where the
    dictionaries were allocated. A collection's node numbers (from the
    registry change above) are read without bounds checks, and by a running
    counter in the passes that walk the candidates in order, which takes
    back the 7% that change had added to a collection.
  - A class's `__doc__`, read through a compiled site as a string or
    `None`, is memoized with the class's other attributes (its version
    moves when the docstring is assigned). Counting which attribute sites
    fall through to the general lookup in `context_managers` found one
    site making nearly all of them: `contextmanager`'s
    `type(self).__doc__`, at every construction, which the class memo
    resolved and then declined, as a dunder that isn't a function.
    `type(o).__doc__` takes 31 ns instead of 94 for an `ABCMeta` class
    (CPython 39), and `context_managers` runs 3% fewer instructions.
  - A compiled `x.send(v)` site keeps a generator's or coroutine's `send`
    as its builtin method once it has loaded one (the kind's one unbound
    `send`, with the receiver as its self, as the core loop loads it), so
    later loads skip the general method lookup. Tallying, across the suite,
    which method sites fall through to that lookup put `coroutines`'
    `avg.send(i)` and `coro.send(None)` first. `g.send(i)` takes 77 ns
    instead of 90 (CPython 125), and `coroutines` runs 3% fewer
    instructions. (The same tally's polymorphic `node.eval(env)` sites in
    `mini_interpreter` see six classes against four entries, but five
    polymorphic entries instead of three changed its instructions by
    0.015%: those loads are served cheaply further down the same path.)
  - A builtin type's call from compiled code (`int(text)`, `float(text)`,
    `range(n)`, `ValueError(m)`) that the site was compiled without
    knowing (a loop that grew hot in its first call compiles before the
    core loop notes the type) is constructed in place by the constructor
    helper, as the type site's helper does, and noted then; it went through
    the general call's layers (a loop of five `int(text)` calls took 5,008
    instructions a pass, now 3,653). `int` and `float` parsing no longer copy their text when
    nothing in it needs normalizing (ASCII without underscores) and check
    the `inf`/`nan` spellings only when a letter is present. `float(text)`
    takes 92 ns instead of 150 (CPython 85), a failed `int(text)` caught
    as `ValueError` 2% fewer instructions in `exceptions`.
  - A small function whose loop tier 2 compiles (`def f(): return [x * 2
    for x in t]`), called from a frame-compiled loop, took three times
    CPython's time (739 ns against 240). Its activations ran tier 2's code
    from the lean call path, where the frame JIT never counted them, so the
    inliner judged `f` "not warm" and the caller left for the core loop at
    every call (the core loop's step, the full `CALL` handler, then tier
    2's entry: about 2,600 of the call's 7,800 instructions). The inliner
    now counts a callee tier 2 compiled as warm, and the core loop notes
    such a callee in the call site's shapes when it declines an inline
    activation for it (a function made anew at each call of its caller was
    otherwise never known to the site, since every call after the first
    few ran tier 2's code). The caller takes `f` in line as it does without
    tier 2: 3,414 instructions a call instead of 7,774 (CPython 3,494), and
    7,761 to 3,362 for a nested `f`; 286 ns instead of 724 (CPython 235),
    and a lambda's the same (753 to 290 ns). A callee's tier 2 code still runs when
    the interpreter or tier 2 calls it, and a body too long to inline keeps
    it too.
  - Tier 2's step of a list or tuple iterator into a scalar lane (its
    commonest opaque loop) runs in a small function of its own, reading the
    iterator and the list through guard-free views, with the rest of the
    general step behind it: about 100 instructions an element became about
    50, mostly the register saves of the large function and an out-of-line
    `pack`. A comprehension over a tuple in a loop tier 2 compiles takes
    3,476 instructions a pass instead of 4,086 (CPython 3,287), 286 ns
    instead of 324 (CPython 228). The suite's
    instructions are level with both changes (geomean 1.000).
  - Tried first and kept as findings: with tier 2 off the suite runs 4.4%
    more instructions (`scimark` 1.71, `tomllib_loads` 1.49, `logging_mix`
    1.20, `async_tree` 1.15, `nqueens` 1.11, the rest within 4%); with tier
    2 declining loops that aren't over a `range`, `f` took 307 ns but
    `raytrace` 15% more time; declining only short bodies left the suite
    level. Calling tier 2's code in place from the core loop (skipping the
    full handler) would need the lean callers flushed first, since the
    callee can run any Python, so the inline path is the cheaper fix. Two
    builds of the same source can differ by 2 to 3% in `async_tree`'s
    instructions (1.02 with tier 2 off, where the change can't act).
  - An `Rc`'s release of its last reference runs out of line
    (`Rc::drop_last`); only the shared release (a decrement that leaves the
    object alive) stays in line at every drop. `Object`'s destructor and
    every function that drops a counted reference took the finalizer test,
    the nested release and the free in line, once per variant: the binary's
    text is 172 KB smaller, and the suite runs 0.3% fewer instructions
    (none more). Simulated instruction-cache misses moved within layout
    noise (`async_tree` 1% fewer, `context_managers` and `exceptions` 2%
    more).
  - Where `async_tree`'s and `context_managers`' time goes, with
    instructions near CPython's: cachegrind on release builds counts four
    times CPython's instruction-cache misses (`async_tree` 33.5 million a
    warm call against CPython 3.13's 8.1, `context_managers` 69 against
    20), and twelve times its last-level misses in `async_tree`. A fifth of
    them are in compiled code (an 8% miss rate), the rest spread thinly
    over hundreds of functions (an event-loop step runs the loop's handle,
    the context, the task's step, the generator's resume, compiled code and
    `call_soon` in turn). A parked task takes 1,238 bytes against 811: its
    context copy alone is two allocations (96 and 56 bytes) where
    CPython's shares one immutable mapping.
  - The frame JIT generates machine code on a worker thread. A compile's
    lowering reads the code's caches and the objects they hold, so it stays
    on the interpreter's thread, but its functions are plain data: the
    worker generates their machine code (about two-thirds of a compile's
    time) while the code goes on running as it was, and the code's next
    activation or back edge installs and publishes it. At most one job is
    in flight (a compile that gets hot while the worker is busy waits for
    its heat's next trigger), the worker hands its allocator pages back
    whenever it runs out of work, and installing stays with the
    interpreter's thread, so the worker holds no lock of the VM's; a forked
    child drops the compiles its parent's worker had (a fork handler bumps
    a generation) and compiles them again, and `os.fork()`'s many-threads
    warning doesn't count the worker. Against generating in line, the same
    binary's first calls take 0.972 of the time (`stdlib_utils` 0.90,
    `chaos` 0.90, `scimark` 0.91, `raytrace` 0.92, `class_creation` 0.92),
    warm instructions are identical, and peak memory is 1.4% higher
    (`raytrace` 8%, `chaos` 5%): the worker's scratch and the
    interpreter's allocations are now alive at once. Two jobs in flight
    cost 2.5% and an unbounded queue held each waiting compile's functions
    (40 compiles at once held 2.4 MB). `WEAVEPY_FRAME_JIT_SYNC=1`
    generates in line.
  - Tier 2's definite-assignment analyses (a region's live-in slots, and
    what interpreted code leaves an OSR root to read) run their fixpoints
    over bit sets, each block's stores collected once, instead of cloning
    and intersecting hash sets on every pass. In `class_creation`'s first
    call tier 2's analysis was 9% of all instructions (it analyzes the
    fresh methods `dataclasses` builds), the two fixpoints 18M of them; the
    whole first run now takes 1.8% fewer instructions (`comprehensions`
    0.6%, the rest level).
  - Tried and dropped: one out-of-line copy of the quiet loop
    (`quiet_run_from`), which `#[inline(always)]` copies into its four
    callers (about 25 KB each: calls, generator steps on both sides of the
    stack-growth check, and native callers' callbacks). The text shrank 82
    KB, but simulated instruction-cache misses fell only 1% to 2% and the
    warm time rose 0.8% (`context_managers` 9.7%, `async_tree` 3.9% less):
    the copies are rarely in the cache together, and each caller's copy
    folds its constant arguments.
  - Cachegrind of a build that generates code on a worker thread needs
    `WEAVEPY_FRAME_JIT_SYNC=1`: valgrind runs one thread at a time, so the
    worker's compiles land far later and `async_tree`'s warm call counted
    817 million instructions instead of 566.
  - Where the asyncio and generator gap is, and what would close it: on
    the PGO build a `sleep(0)` round trip runs 1.10 of CPython's
    instructions but 1.58 of its time, and cachegrind counts 4.5 times
    CPython's instruction-cache misses (about 83 KB of code touched a step
    against 18.5 KB). CPython runs a coroutine's whole send chain in one
    eval loop; here a step passes through some thirty helpers (the task
    step, `generator_send_lean`, `lazy_await`, `lazy_gen_resume`,
    `core_gen_make`, `gen_native_yield`, `retire_gen_frame`, ...), each
    building its own state: a resume from compiled code builds and drops a
    30-field native `State` and a resume record, and a trivial generator's
    life costs 2,380 instructions (creation about 790 over a dozen helpers,
    each resume about 470, the end about 340) against CPython's 1,210. No
    one piece is large: the lever is a lighter resume protocol, a compiled
    consumer entering a parked generator's compiled body directly (as
    `FOR_ITER_GEN` pushes a frame) with the generator's state kept in a
    form the native code reloads cheaply. Smaller findings: `findCaller`
    runs 1.41 of CPython's instructions (frame materialization, about
    1,900 a call), and `ExitStack` 1.47 and `suppress` 1.21 (constructor
    and `*args` binding, closure creation, spread thinly).
  - Which generator paths the workloads take, per warm call (callgrind
    call counts): the frame JIT's lazy resume carries nearly all of them
    (`generators_tree` 640,000 resumes, 295,000 of them a collapsed `yield
    from` chain's; `sudoku` 130,000; `nqueens` 40,000; `async_tree` 15,500
    by fast step, 15,500 lazily and 18,700 through the task step's
    `generator_send_lean`), and in `sudoku` and `nqueens` every
    `h_for_iter_gen` first tries a fast step that declines. Resumes aren't
    where the generator gap is: summing a depth-15 tree through recursive
    `yield from` costs 2,901 instructions an item against CPython's 3,123
    (a collapsed chain resumes its innermost generator in constant time,
    where CPython's sends pass every level), and a flat generator 864
    against 1,045. What costs is each generator's life outside its body:
    creation about 870 instructions over a dozen helpers, the end about
    230, the object's free about 170, against CPython's few hundred in all.
    Forcing the creation helpers in line changed that by -1% to +3%, so
    function boundaries aren't the cost either: the work is spread over
    the pools (the generator object, the frame box, the locals and the
    stack each kept apart), the reference counts a frame takes and drops
    (code, globals, builtins, cells), and the collector's bookkeeping
    (about 150). Pooling a finished generator's frame per code object, with
    its references kept, would leak: the pooled frame holds the module's
    globals, which lead back to the function and its code, so code made at
    run time (`dataclasses`' methods) would never be freed, and breaking
    that loop gives back most of the saving.
  - A task's context costs nothing until something uses it. A task keeps a
    copy of its creator's context, which CPython makes as one small object
    over a shared immutable mapping; here a `Context` is an instance and a
    block of slots (152 bytes, two allocations), made for every task and
    entered and left on every step. A task now keeps the copied mapping
    itself, and its steps run over it as an unnamed current context
    (`contextvars_native::run_in_snapshot`). The `Context` is built only
    when Python code can see it (a `ContextVar.set` or `reset`, the task's
    `get_context()`, a handle's `get_context()`, a context-switch watcher,
    a loop whose `call_soon` is Python code), and the task keeps the one
    built during a step; a step scheduled before it was built runs in it
    too. A future's done callbacks scheduled natively keep their copied
    mapping the same way. `ContextVar.get` and `decimal`'s context lookup
    read the mapping directly. Over a tree of 7,776 sleeping tasks each
    parked leaf takes 2,420 bytes instead of 2,745 (CPython 1,530), and
    the tree runs in 0.89 of the time; in the suite `async_tree` takes
    0.93 of the previous build's time and its peak memory ratio falls from
    1.47 to 1.39, the rest level (geomean 0.990). The rest of a parked
    task is its three coroutines (`leaf`, `sleep` and `__sleep0`, about
    1,090 bytes over nine allocations; a lazily awaited coroutine that
    suspends keeps its activation's box and a stack sized for compiled
    code), its state (184 bytes reached through a handle slot and a
    registry), and the loop's `Handle` (216 bytes) and step callable.
  - `f(*args, **kwargs)` of a native method bound to its receiver runs
    from compiled code. A wrapper forwarding to a saved `log.append` took
    1,132 ns a call against CPython's 159: a spread call of a bound native
    method found no in-place path. A bound leaf method now runs in place,
    its receiver first as a `CALL` passes it: 291 ns. A non-leaf method
    (`ExitStack.callback(log.clear)`) still takes the core loop's lane,
    494 ns against 119. The suite's instructions are level: no workload
    forwards to a native method in its loop. (Sending a forwarding wrapper's
    non-leaf native callee through `CALL_FUNCTION_EX`'s lane too, instead
    of leaving compiled code at its `BUILD_MAP`, broke `test_io`'s
    interrupted-write test, whose `write` then finished before the alarm,
    and was taken back out.)
  - A blocking native call (`select`, `time.sleep`, a lock) puts the
    thread's lazily running activations on the spine first only when
    something could read its frames while it blocks: another thread
    (which turns shared reference counts on before it starts, as
    faulthandler's watchdog does) or a faulthandler dump armed for a
    signal. An event loop's `select` blocks once a step, and its pending
    callers (`KqueueSelector.select`, `BaseEventLoop._run_once`)
    registered each time: a `sleep(0)` step runs 39,700 instructions
    instead of 41,500 (CPython 33,400, most of it the system call on both).
  - Where the rest of a step goes: a `sleep(0)` round trip, the loop's
    iteration included, runs 24,300 instructions over 1,916 distinct cache
    lines of code (120 KB, four times the L1 instruction cache) in 232
    functions, none above 96 lines. The direct call `_run_once` makes to
    `handle._run()` still registers once a step (the lazily awaited
    `sleep`'s coroutine suspends), and a few compiled sites take their slow
    helper every step (`h_method`'s `method_slow`, `attr_stack_slow`, an
    instance's truth).
  - A class's scalar attribute read through an instance (`self.LIMIT`)
    runs in compiled code. It took 238 instructions a read against
    CPython's 63: compiled code called the attribute helper, which missed
    its field cache, called its slow half, and found the class's value in
    the site's memo after proving by a name lookup that the instance
    doesn't shadow it. The site now remembers what proves that without a
    lookup (the class's shared names and how many precede the attribute's,
    as a leaf method site does), and a site the interpreter has seen read
    a class's scalar through an instance checks its first memo entry in
    line: the class's version, no published `__dict__`, values unborrowed
    and laid over the proved names, then the value's copy. 111
    instructions (a field's read takes 99). The suite is level.
  - A positional call of a function with keyword-only parameters, all
    defaulted (`def f(a, *, key=None)`), lowers in line like any other:
    the inliner refused every callee with extended parameters, so such a
    call took the frameless-leaf evaluator, matching each keyword-only
    default to its parameter by name on every call. The plan now binds them
    with the positional defaults in one helper (a rebound
    `__kwdefaults__` declines it), and the defaults, which the compiler
    lists in the parameters' order, are taken in that order when the names
    line up. A call of `def f(a, b=2, *, c=3): return a` takes 352
    instructions instead of 927 (CPython about 595), one whose body calls
    `len` 598 instead of 1,391 (about 905). A keyword-only parameter given
    by keyword still takes the evaluator: 1,026 against about 550.
  - `functools.reduce` reads a tuple's items, or a list's by index, without
    an iterator, and calls a native function's leaf half (`operator.add`)
    before the general call: ten additions take 8,457 instructions instead
    of 11,673. CPython's take 3,632, and each element still costs about
    400 here.
  - A subscript site that has run an instance's Python `__getitem__`
    calls it first from compiled code. The method was the last resort:
    compiled code tried the native containers' helpers in turn (six of
    them, each checking and declining) before the direct call. `o[1]` takes
    439 instructions instead of 727 (CPython about 258), and a
    `__getitem__` that indexes its own list 789 instead of 1,097 (about
    359).
  - Calling a native function that isn't a leaf from compiled code asked
    the leaf registry about it seven times over (two hash lookups each)
    and matched its name against the core loop's list of about 150 lane
    names twice, failing every comparison for a name the list doesn't
    hold. The registry's last miss is memoized (by address and registry
    generation), and the lane verdict by the name's address, as
    `builtin_needs_interp`'s is; `operator`'s leaf halves add or
    multiply two small ints or floats without the general operation's
    promotions. Calling `functools.reduce` over a pair takes 4,070
    instructions instead of 5,170, and ten additions through it 5,392
    instead of 8,457 (CPython 3,632). Calling such a function still costs
    about 3,000 instructions over CPython's: `vars(o)` takes 4,695 against
    915.
  - On a PGO build at `8ebbbdeb` (November 10, 2026): 1.000 of the
    `57960cea` build's warm time (`stdlib_utils` 0.94 to 0.96,
    `class_creation`, `deepcopy` and `async_tree` 0.95; `generators_tree`
    and `float_points` 1.07, traced to the cache-table change tried and
    dropped below) and 0.991 of its peak memory. CPython's own numbers moved
    between the two sessions (its peak memory 0.84 of the earlier
    session's, its times up to 1.15 of them), so this run's ratios to it
    (warm 0.789, cold 0.884, peak memory 1.497) aren't comparable with the
    earlier checkpoint's.
  - On a PGO build at `57960cea` (November 10, 2026): warm time 0.873 of
    CPython's, cold 0.980 (from 1.007), peak memory 1.266 (1.253 the
    previous session; CPython's own moves between sessions), and 0.991 of
    the previous PGO build's time. `async_tree` takes 1.27 of CPython's
    time (from 1.45, 0.92 of the previous build). The worst warm times are
    `async_tree` 1.27, `logging_mix` 1.15, `context_managers` 1.14,
    `deepcopy` 1.12 (its instructions unchanged; its time moved 2 to 5%
    between runs), `closures_decorators` 1.08 and `collections_mix` 1.07.
  - Fixed: a signal that a native function trips (`os.kill` of this
    process) now runs its handler as the call returns when the core loop's
    lane runs the function, as CPython's calls of C functions check the
    eval breaker, with the activation's frame for the handler; it ran at
    the next instruction boundary the general loop reached. And
    `str.format`'s native body, which direct calls and compiled code use,
    now raises CPython's errors for an empty attribute (`'{0.}'`), for
    switching between automatic and manual numbering (`'{}{1}'`), and for
    a format spec nested twice, looks up attributes through the
    interpreter (`'{0.real}'`), and reruns any template it rejects through
    the interpreter's formatter for the exact error; only a spread call
    (`f(*args)`) reached that formatter before.
  - Tried and dropped: letting every native function whose body is the
    whole call (`native_call_ic_safe`) take the core loop's lane from
    compiled code instead of the allowlist of about 150 names. Calling
    `functools.reduce` over a pair took 1,295 instructions instead of
    4,070 (CPython 1,122), but `test_unittest`'s
    `testAssertRaisesRefcount` found two references to the function
    `assertRaises` called left behind after the class's earlier tests
    had warmed the paths (some native among them keeps the frames alive
    that `traceback.clear_frames` should clear).
  - A keyword call of a native function or method costs about 3,000
    instructions over CPython's: `xs.sort(key=None)` takes 3,770 against
    880, `s.split(maxsplit=1)` 4,016 against 1,488, `int("ff", base=16)`
    3,957 against 1,989. Compiled code leaves for the full handler there,
    but sending those calls through the core loop's lane instead (its
    general call takes keywords) saved only `sort`'s exit (3,361): the
    cost is the general keyword call itself (the `String` per keyword, the
    dispatch by name), which is the next thing to make cheaper there.
    Fixed on the way: `str.split` and `str.rsplit` ignored a keyword they
    don't take (`'a'.split(bogus=1)`).
  - Tried and dropped: `LOAD_GLOBAL`'s cache hit checked in compiled code
    instead of in `h_global` (1.1% of the suite's instructions, averaged
    over the workloads): the suite came out level (1.001), `raytrace`
    1% lower and `class_creation` 3% higher, its fresh code compiling the
    longer sequence at every global load.
  - Where the suite's instructions go, by function, averaged over the 33
    workloads' warm calls (callgrind): nothing takes more than 1.3%
    (`Object`'s drop glue, `mi_free`, the allocation, `h_call_site`,
    `h_site_leaf`, `h_global`, `core_store_init`), and the first fifty
    together take about 30%.
  - A spread call of a builtin, or of a native method bound to its
    receiver, from compiled code (`callback(*args, **kwds)` of a saved
    `log.clear`, as `ExitStack.callback` wraps it) spreads the items over
    the call's operands and runs as a `CALL` of them would: a leaf half in
    place, any other body the core loop's lane takes through that body.
    Only a leaf builtin's spread ran on the frame, collecting the items
    and the operands into two vectors; the rest left for the core loop's
    general call. `cb(*a)` of a bound `list.clear` takes 931 instructions
    instead of 2,394 (CPython 172), `cb(*a, **k)` with an empty `k` 924
    instead of 5,098 (779), `len(*a)` 617 instead of 1,232 (491), and an
    `ExitStack` with a callback 24,432 instead of 27,026 (16,391). Fixed
    on the way: builtins that take no keywords (`abs`, `len`, `divmod`,
    `hash` and others) ignored any they were given; they now raise
    CPython's `TypeError`.
  - Less memory per code object that runs: the per-site side tables of
    code that never warms (most of it) cost a word each instead of four,
    and three rarely built values (a generator's names, `co_name` and its
    neighbors as objects, `co_positions()`) a word instead of up to seven.
    The heap after `import logging` holds 6.4 MB more than at start-up
    (sampled): about 3 MB of it for the code that ran (its decoded bodies,
    constants, names, caches and tables), 1 MB for classes and 0.5 MB for
    functions.
  - Tried and dropped: allocating the inline cache table (16 bytes an
    instruction) only once the code runs an instruction again, dropping
    the writes of code that runs once from top to bottom. With the boxed
    tables above it cut peak memory after `import logging, io` by 248 KB
    (of 17,232; CPython 13,316), but blocks that ran once before a compile
    then looked as if they never ran (their sites unresolved), so the frame
    JIT compiled them as exits: `generators_tree` retired 5% more
    instructions and `float_points` 3%.
  - A keyword call lowers in line when its keywords aren't the callee's
    next parameters in order: a keyword past a defaulted parameter
    (`f(a, c=3)`), keywords in another order, or a keyword-only parameter
    given by keyword. The plan maps each argument to its parameter, and one
    helper fills the rest from the defaults before any argument is
    written (a rebound `__defaults__` or `__kwdefaults__` declines it). Only
    keywords that continued the positional ones lowered in line before;
    the rest took the frameless-leaf evaluator or a direct call. `f("ab",
    c=3)` of `def f(a, b=1, c=2): return len(a)` takes 477 instructions
    instead of 1,314 (CPython 841).
  - Where `stdlib_utils`' time goes: `pathlib` and `urllib.parse` take
    about 1.9 times CPython's time on their own (1.25 to 1.35 times its
    instructions), and `datetime`, native here, a tenth of it. A
    `urllib.parse` keyword call of a long function (`parse_qsl(...,
    _stacklevel=2)`) costs about 700 instructions in the direct call's
    binding; in `pathlib` much of the work runs in the core loop, its
    short comprehension loops left to tier 2 (item 12 below).
  - A function whose loops run a few iterations a call and call no Python
    function or method natively (a filtered comprehension over a short
    split, `pathlib`'s `_parse_path`) gets a frame-JIT body, and compiled
    callers call it directly, though tier 2 compiled it. Tier 2's compile
    now records the iterations a call it measured for such code; at 16 or
    fewer, a compiled caller's direct call that finds no body compiles one,
    and the direct call no longer declines that callee (tier 2 keeps the
    interpreter's calls and its own). Each such call went out to the core
    loop and into tier 2's framed entry, which costs several thousand
    instructions. `[x for x in s.split("/") if x]` as a function takes
    4,735 instructions a call instead of 7,041 (CPython about 4,400), and
    `PurePosixPath(...).parts` of a new path 37,925 instead of 41,991;
    `stdlib_utils` retires 2% fewer instructions, and the rest of the suite
    is level. A loop that calls Python code stays with tier 2, which runs
    its calls better: with it, `raytrace` (whose `first_hit` loops over
    four objects calling a method) retired 9% more instructions. (A first
    version also counted loop-free code as short and compiled
    `with_segments`, which takes `*args`, for one local too many, so its
    every call found no body: `p / "x"` took 42% longer.)
  - Reading a protocol dunder or a native method off a class is memoized
    at the site: `dict.__setitem__`, an inherited `D.__setitem__` and
    `type(self).__setitem__` (as `OrderedDict.update` checks at every call),
    `list.append` read for a call. The class-attribute fast paths admitted
    only a short list of dunders, though `type` defines none of the
    container, iteration, comparison and arithmetic ones as a data
    descriptor, and kept a native method found on the class only for
    `__new__`. `dict.__setitem__` takes 187 instructions instead of 2,283
    (CPython 301), `OrderedDict()` 5,473 instead of 7,519 (CPython's is C:
    1,162), and `list.append(lst, x)` 1,663 instead of 1,932 (756): calling
    a native method unbound still takes the core loop's lane.
  - Construction costs left: `object()` takes 2,194 instructions against
    848, `defaultdict(list)` 5,073 against 1,989 (the general type call, a
    native `__init__` reached through a bound method), and `deque()` 4,018
    against 1,910 (its `__init__` is Python, over four slots).
  - `super().__init__()` or `dict.__init__(self, ...)` in a `dict`
    subclass's `__init__` runs `dict.__init__`'s body in the core loop's
    lane (it reads the receiver's payload itself), and a compiled call of
    a builtin the lane runs (`object.__setattr__(self, name, value)`) runs
    its body as the settled-builtin helper does, leaving a lazily running
    caller lazy. `dict.__init__` on a dict payload went to the full call,
    and the compiled call registered its caller first. Constructing a
    `dict` subclass whose `__init__` calls `super().__init__()` takes 3,247
    instructions instead of 7,667 (CPython 3,107), one calling
    `dict.__init__(self)` 3,350 instead of 7,475 (2,664), and a
    `__slots__` class setting two fields through `object.__setattr__`
    5,104 instead of 5,602 (3,859).
  - Three everyday operations off the slow path, found by a sweep of 57
    common operations against CPython: `getattr(point, "x")` of a named
    tuple reads the field through its tuplegetter (1,957 instructions to
    724; CPython 546), `dict.setdefault` with a `str` or `int` key is a
    leaf half that probes once (1,577 to 1,105; 784; an unused default
    that could finalize, `d.setdefault(k, Noisy())`, keeps the full path,
    which runs its `__del__` before the next statement), and a
    `for i, x in enumerate(xs)` loop over a list, tuple or range retires
    its iterator at the end in compiled code instead of leaving for the
    core loop (a five-item loop 4,369 to 2,741; 2,594). `comprehensions`
    retires 5% fewer instructions. Still behind in the sweep:
    `functools.partial(f, 1)` construction (3.0 times CPython's
    instructions: a native class's general construction, its slots set by
    name), `struct.pack` (2.7), a small `json.loads` (1.9),
    `io.StringIO(...).read()` (1.8), `A & B` of small sets (1.5), and
    `list(enumerate(xs))` and `zip` loops (1.4 to 1.6).
  - `json.loads` and `json.dumps` called from compiled code run the native
    scanner and encoder in the core loop's lane: a decoder's `scan_once`
    and an encoder's `_iterencode` are builtins the lane didn't know, so
    every call left compiled code, registering the lazily running
    `loads`, `decode` and `raw_decode` on the way. `json.loads` of a
    one-key document takes 14,961 instructions instead of 21,544 (CPython
    12,153), of a three-key one 18,053 instead of 25,114 (16,451), and a
    small `json.dumps` 27,841 instead of 30,236 (23,233).
  - Calls from native code into Python code, measured again: a plain
    function or a pure leaf is at parity (`map(f, xs)`), but a method that
    changes state (`map(obj.step, xs)`) costs about 2,800 instructions a
    call against CPython's 1,300, most of it tier 2's native entry
    (`try_call_native_direct`: about 550 of its own, the pooled argument
    buffers, guards and a dozen reference-counted clones into its context).
    Iterating an instance with a Python `__next__` costs 32,882
    instructions for five items against 10,255, and a dict lookup keyed by
    a frozen dataclass instance 15,281 against 3,351.
  - Why caught exceptions cost more inside a function another compiled
    one calls: catching runs the handler search with the activation on the
    switch (`h_catch` settles it), so a direct call whose body catches an
    exception registers, and so does every lazily running caller above it.
    In `exceptions` the `lookup` and `guarded` calls register 119,000
    times a warm call. Raising out of a callee and catching in the caller
    runs 6,700 instructions against CPython's 3,650, of which the crossing
    costs 3,090 against 1,270. A traceback entry refers to its
    activation's shell, which a direct call doesn't have; catching without
    registering needs entries that name the record and are made whole only
    if they outlive the call (on its return or raise), so a clean return
    checks for them.
  - A dict probe keyed by an instance whose `__hash__` and `__eq__` are
    small Python functions (a frozen dataclass) settles in the leaf
    evaluator when the stored key is an equal instance of the same class,
    not only the same one: its `__eq__` runs as a pure leaf in the table
    walk. Before, the probe hashed the key up to three times (compiled
    code's quick probe, the subscript's, then the full probe's) and ran
    `__eq__` through the full comparison. A compiled leaf plan reads
    `obj.__class__` of a plain instance directly instead of through the
    general attribute read, whose field cache never holds it (the
    dataclass `__eq__` compares `other.__class__ is self.__class__`).
    Warm, `d[K(1, "a")]` takes 4,144 instructions instead of 12,085
    (CPython 3,139), and `k == other` 1,831 instead of 3,571 (1,795).
    `hash(obj)` of an instance hashed by identity is a leaf half (2,624 to
    370; CPython 429).
  - `operator.attrgetter("x")` called on an instance reads the attribute
    as `getattr`'s leaf half does, and an adapter's callback or a
    `sorted` key that's an `attrgetter` or `itemgetter`, or a pure-leaf
    function, runs in place. `sorted(xs, key=attrgetter("x"))` over 100
    instances takes 143,000 instructions instead of 251,000 (CPython
    75,000), and `sum(map(attrgetter("x"), xs))` 135,000 instead of
    313,000 (46,000). Calling the getter from compiled code still costs
    3,600 a call against 790, most of it tier 2's native entry.
  - A loop tier 2 didn't take no longer stays interpreted. The frame JIT
    leaves code whose loops tier 2 may still take to it, and tier 2 stops
    asking only once every loop header has missed; in a function run
    once whose main loop it rejected, the loops of branches that never
    ran never miss, so the main loop ran in the core loop for good. A
    loop that got hot sixteen times at its back edge, in code for which
    tier 2 holds no compiled code, now compiles in the frame JIT. (Heat
    from calls doesn't count: tier 2 rejected `raytrace`'s `trace` at
    first and compiled its loop later, and taking it then cost 9%.) In
    such a loop, `all(x for x in t)` takes 4,059 instructions instead of
    8,532 (CPython 4,471) and making a generator 2,633 instead of 3,597
    (2,130). The suite's instructions and cycles are level (`float_points`,
    whose `bench` loops tier 2 rejects, retires 4% more instructions in
    0.99 of the cycles).
  - Tier 2's dynamic call of a native method bound to its exact builtin
    receiver (`groups[k].append(x)`, `parts[i].strip()`, a method read off
    a value it can't type) runs the call as the core loop's leaf lane does,
    for the kinds whose releases can't finalize anything (not a removal).
    It went through the general call with an activation shell, about 600
    instructions more. `groups[k].append(i)` with `groups` a
    `defaultdict(list)` takes 2,751 instructions an iteration instead of
    3,380 (CPython 1,853); the suite is level (`raytrace` 0.98). Tier 2's
    other dynamic calls still pay that shell: constructing a Python class
    (`ChainMap(...)`, 14,200 instructions an iteration against 9,100)
    and a dunder's dispatch.
  - A subscript of an instance whose class's `__getitem__` is a native
    method with a registered fast half (a `ChainMap`'s) runs the half
    first, found through the class's memo, as the core loop's lane does.
    The general subscript (tier 2's, a native caller's) looked the method
    up by name, built a bound method and called it. `cm["a"] + cm["b"]`
    takes 5,093 instructions instead of 7,435 (CPython 5,394) and 1,859
    cycles instead of 2,652 (1,979); building a `ChainMap` and reading
    two keys, 11,798 instead of 14,245 (9,132).
  - A module's attribute read at a compiled site (`os.path`,
    `sys.modules`, `time.time_ns` read as a value) and a module function's
    method-form load (`os.path.basename(p)`) answer in the fast helpers
    from the site's module entry while the module's dict is unchanged.
    The site's entry for a module was read but never filled, so every
    read took the slow path, and method-form loads reached the module
    entry only past `method_slow`'s preamble. Found by counting slow-path
    reads by site in `logging_mix`, where `LogRecord.__init__`,
    `findCaller` and `posixpath` read modules a dozen times a record.
    Instructions: `logging_mix`, `stdlib_utils`, `deepcopy`,
    `graph_search` and `tomllib_loads` 0.99, the suite level.
  - A compiled method call of a builtin function kept on the class and
    called through an instance (`logging.Formatter`'s
    `self.converter(t)`, `converter = time.localtime`) loads it from the
    site's descriptor memo, as a `staticmethod`'s function is, with the
    same version and shadowing checks. It went to the slow path every
    time. A method descriptor kept on a class (`app = list.append`) still
    binds, through the full path.
  - `k in d.keys()` with a plain `int` or `str` key probes the dict
    natively in the commonest-shapes helper compiled code and the core
    loop share, freeing the temporary view there when that frees nothing
    else (its dict lives on; no subclass instance rides on it). The view
    went to the general membership, which also left compiled code for the
    core loop. Over a loop, 469 instructions an iteration instead of 1,565
    (CPython 888) and 170 cycles instead of 501 (259). `len()` of a
    dict view is a leaf call too, as of the dict: `len(d.values())` takes
    656 instructions instead of 1,679 (1,053) and 219 cycles instead of
    515 (337).
  - The inliner no longer takes a callee whose loops tier 2 runs better
    (`tier2_takes_calls`: tier 2 compiled it, and its loops run more
    than a few iterations a call or call Python code), as direct calls
    already declined it. Counting every tier-2 compiled callee as warm
    (`714ecf5f`) inlined `jitkernels`' 200-iteration kernels into the
    frame JIT's `bench`, where their loops ran as the frame JIT lowers
    them: the bench gate's `jitkernels` took 35 to 47% longer on every
    platform (279M to 367M instructions). The short comprehension that
    change was for still goes in line (7,811 instructions a call against
    21,147 before it).
  - The Ubuntu bench gate's `float_math` (1.11 to 1.27 of the merge base
    over twelve CI runs) and `deque_ops` (1.05 to 1.15) were real, not
    runner noise. A throwaway workflow timed builds along the branch's
    history on `ubuntu-latest` runners, interleaved on one machine, and
    bisected each to commits; neither reproduces on this development
    host (an i9-9980HK), and the runners expose no hardware counters.
    - `float_math`: `d30367eb` counted tier 2's framed calls of loop-free
      callees toward its call-density judgment, which hands the loop to
      the frame JIT. `float_math`'s constructor and methods work on float
      fields and `math` calls, which tier 2 keeps in their lanes and the
      frame JIT boxes: the frame JIT's code took 1.27 to 1.36 times the
      merge base's time on AMD EPYC runners and 1.16 on an Intel Xeon
      (Ice Lake), against 0.89 to 0.96 for tier 2's. (On the development
      host the frame JIT's is 7% faster.) A callee whose body keeps
      floats in their lanes (a float local, field site or return, or a
      `math` intrinsic) no longer counts toward the judgment:
      `float_math` takes 0.95 to 0.99 of the merge base's time on the
      runners, from 1.14 to 1.16.
    - `deque_ops`: `f33a6f89` gave a deque a `__slots__` layout, whose
      lookup by name ran a slow path (17% more instructions), and
      `146c040b`'s fast path compared the four names at every native
      call, about 40M of the fixture's 640M instructions with the
      iterator's lookups. The deque's and its iterator's native methods
      now remember the layout they found led by their names
      (`LayoutMemo`) and compare its address: 576M instructions, and
      0.83 to 0.88 of the merge base's time on the runners, from 1.08 to
      1.18.
    - With both, the bench gate passed on three runners (two EPYC 9V45,
      one EPYC 9V74), its suite at 0.903 to 0.937 of the merge base.
  - Tried and dropped: calling a Python `__next__` from the core loop's
    `FOR_ITER` without a bound method. It skipped tier 2's direct entry,
    which the bound-method call reaches, and a warm five-item loop took
    28,800 instructions instead of 18,200 (CPython 6,100).
  - On a PGO build at `0a538196` (November 9, 2026): warm time 0.875 of
    CPython's, cold 1.007, peak memory 1.253 (`nqueens` 0.98, from 1.59 on
    the previous PGO build). The worst warm times are `async_tree` 1.45,
    `context_managers` 1.19, `logging_mix` 1.10, `deepcopy`,
    `collections_mix` and `sudoku` 1.09. Peak memory isn't heap: in
    `tomllib_loads` the live heap peaks at 6.3 MB of 19.2 MB resident
    (CPython 14.1), the frame JIT's share is about 1.5 MB though its code
    is 100 to 400 KB per workload, and the purge delay doesn't change it.
    In `async_tree` the collector costs 25 ms of the first call (CPython's
    10), and with it off the first call still takes 107 ms against 59: a
    `sleep(0)` round trip runs 19,100 instructions against 15,500 and
    16,200 cycles against 9,400, and a trivial generator's life 2,493
    instructions against 1,210 (creation about 790, each resume about 450,
    the end about 340).
  - Where a first run's extra time goes (callgrind, the cold call less a
    warm one): in `async_tree` 61% is the cycle collector, whose young
    collections grow with the pending task tree (2,000 to 33,000
    candidates as each finds nothing) until one of generation 1 walks all
    67,000 live objects, at about 1,000 instructions a candidate against
    CPython's few hundred at most. In `stdlib_utils`, `logging_mix` and
    `tomllib_loads` a third or more is code generation (spread over a dozen
    passes, no line hot) and most of the rest runs interpreted before the
    compiles.
  - `nqueens`' layout cliff was the allocator's, not the hardware's.
    At some environment sizes (padding of 128 bytes, and the pybench
    runner's frozen-cache path) every warm iteration after the first took
    about 160 ms instead of 95 and, counted natively on macOS, 12% more
    instructions; mimalloc's statistics showed 127,400 pages set up in a
    run instead of 141. mimalloc 3 keeps a page that empties only when it's
    the only page in its size class's queue, and by default it also keeps
    up to two full pages in that queue (`page_full_retain`), so the page a
    burst of short-lived objects spilled into was freed each time the
    burst died and set up again for the next one. Whether a burst crosses a
    page boundary depends on where the long-lived objects sit, which the
    environment's size shifts. The interpreter now sets
    `page_full_retain` to 0 (unless `MIMALLOC_PAGE_FULL_RETAIN` is set), so
    full pages leave the queue and the emptied page stays for the next
    burst. Over 32 environment sizes the instructions now vary by 0.6% at
    most (they varied by 7.5%); in the suite `nqueens` takes 0.84 of the
    previous build's instructions and 0.63 of its time (1.00 and 1.13 of
    CPython's), and the rest is level (time 0.987 of the previous build's
    geomean, peak memory 1.0016, instructions within 1%). Callgrind on
    Linux never showed the cliff because valgrind's own layout differs, and
    an earlier timing sweep there that seemed to was contention.
  - Tried and dropped: trimming a long-lived suspended generator's
    operand stack to its code's own depth when the collector compacts it
    (every native entry makes the native code's room again first).
    `async_tree` peaked 1 MB higher: each trim and regrowth moves the
    stack between allocation classes, and the pages of both stay partly
    used.
  - Where the rest of the memory is: imports are at parity (`import
    asyncio` 19.5 MB against 20.1, and 2,000 small functions loaded and
    run take the same footprint as CPython's), so the gap is what running
    code holds. A `Future` takes 419 bytes against 186, a loop `Handle` 505
    against 193, and an instance with three values 165 against CPython's
    96: values are 16-byte `Object`s against CPython's 8-byte pointers, and
    each object's parts (an instance's values, a future's state, a frame's
    locals and stack) are separate allocations, so the remaining gains are
    structural.
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

- **A call from native code into Python costs four or five times a call
  from compiled code** (November 13). The same `__eq__` body costs 2,200
  instructions called directly from a compiled `COMPARE_OP`, 4,080 when
  tier 2 runs the loop and compares through its generic helper, and
  about 400 an element in `x in xs` against CPython's 160. The nested
  entry is `call` to `call_python` to `call_python_bound` (190
  instructions), tier 2's direct-entry probes (`try_call_native_direct`,
  `direct_entry_for`, `callee_guards_hold`, `guards_hold`: 310),
  `pooled_locals_from_args` and `lean_frame` (120), `run_frame_lean_impl`
  (190), `quiet_native_step` (170) and `retire_lean_frame` (140): about
  1,500 instructions around a body a direct call enters in 300.
  Everything native that calls back pays it: `list.__contains__`,
  `sorted(key=...)`, `min` and `max`, `map`, dict lookups through a
  Python `__hash__` or `__eq__`, and tier 2's generic operations.
- **Tier 2 takes loops it runs slower than the frame JIT would.** `if a
  == b: k += 1` over two instances: 4,080 instructions an iteration
  with tier 2, 2,400 with `WEAVEPY_TIER2=0`. The counter makes the loop
  tier 2's, and the instance work in it then goes through helpers that
  make nested calls. The same loop without the counter was the frame
  JIT's all along.
- **`dataclasses.asdict` and `replace` have no cliff.** 43,600 and
  32,200 instructions a call against CPython's 15,600 and 12,000, and
  the profile is flat: fourteen class-attribute lookups by name at 115
  instructions each (`hasattr(type(obj), "__dataclass_fields__")` and
  its kin), thirty-five global reads at 39, four general calls at 370, a
  generator and three comprehensions. Nothing is past 4% of the total.
- **The suite is not the workload** (November 13). The everyday battery's
  first run, WeavePy's time over CPython's, best of three runs a case, on
  a loaded machine:

  | Case | Ratio | Case | Ratio |
  | --- | ---: | --- | ---: |
  | `class_features` | 7.9 | `os_stat_exists` | 2.7 |
  | `enum_dataclass_ops` | 7.6 | `csv_ops` | 2.5 |
  | `threading_queue_ops` | 5.8 | `bigint_ops` | 2.2 |
  | `os_listdir_walk` | 4.8 | `glob_fnmatch` | 2.2 |
  | `file_read_binary_chunks` | 4.8 | `collections_ops` | 2.2 |
  | `context_managers_generators` | 4.0 | `asyncio_ops` | 1.9 |
  | `inspect_typing_ops` | 3.8 | `shlex_ops` | 1.9 |
  | `email_html_ops` | 3.6 | `decimal_fraction_ops` | 1.7 |
  | `configparser_ops` | 3.5 | `math_ops` | 1.7 |
  | `exceptions_ops` | 3.5 | `difflib_ops` | 1.6 |
  | `datetime_time_ops` | 3.4 | `heapq_bisect` | 1.5 |
  | `bytes_ops` | 3.4 | `urllib_parse_ops` | 1.5 |
  | `pathlib_ops` | 3.2 | `logging_ops` | 1.4 |
  | `argparse_ops` | 3.0 | `pickle_copy_ops` | 1.3 |
  | `uuid_secrets_ops` | 2.8 | `sort_ops` | 1.2 |
  | `weakref_gc_ops` | 2.7 | `str_format_ops` | 1.1 |
  | `itertools_functools` | 2.7 | `json_ops` | 0.72 |

  Ahead: `re_ops` 0.74, `json_ops` 0.72, `xml_etree_ops` 0.66,
  `subprocess_ops` 0.61, `list_dict_basics` 0.87, `sqlite_ops` 0.90.
  Geometric mean over 53: 1.83.
- **Where a slow case's time goes is usually one operation.** Broken down
  by instruction count against CPython, a case's loop is mostly at 1.0 to
  1.5 with one or two operations far out: for `class_features`,
  two-argument `super(C, o).attr` (8 times), `getattr(o, "prop")` (3.4),
  `type(o).__name__` (3.2), `issubclass` against an ABC (2.5); for the
  threading case, `lock.__enter__` read by name; for directory walks, a
  `stat` per entry. Each was a general path taken where a cached or
  direct one was missing, 3,000 to 10,000 instructions flat across the
  attribute and call machinery with no hot function.
- **The battery's wall-clock ratios are worse than its instruction
  ratios.** `class_features`' loop, measured directly after this PR's
  fixes: 35,500 instructions and 27,000 cycles an iteration against
  CPython's 14,600 and 5,800 (2.4 times the instructions, 4.6 times the
  cycles; 1.32 instructions a cycle against 2.51). It isn't warm-up: the
  eighth run of the loop takes what the first does. And it's more than
  the sum of its parts: each of its seventeen operations alone runs at
  2 to 3 instructions a cycle and they add up to 13,600 cycles, half of
  what the loop takes with all of them in it. The general paths are
  where the instruction-cache footprint is (see the next entries), so a
  loop that crosses several of them pays again for each. By cycles
  against CPython, alone: two-argument `super(C, o).attr` 14 times
  (10,100 instructions), `type(o).__name__` of a class with an ABC base
  4.9, `getattr(o, "prop")` 4.0, `o.prop = v` through a setter into a
  slot 3.1, `3 in o` 2.5, `issubclass(C, AnABC)` 2.4.
- A second run of the battery after this PR's fixes (also on a loaded
  machine): 1.81 over 54 cases. `os_listdir_walk` 4.8 to 1.6,
  `class_features` 7.9 to 5.1, `enum_dataclass_ops` 7.6 to 5.7,
  `context_managers_generators` 4.0 to 2.8, `exceptions_ops` 3.5 to 2.7,
  `file_read_binary_chunks` 4.8 to 2.9; the library cases
  (`configparser`, `inspect` and `typing`, `email`, `datetime`,
  `weakref`) didn't move, as nothing here touched them.
- Per-operation instruction counts still open after this PR's fixes
  (WeavePy against CPython): `Color["RED"]` 3,860 against 620, `Color(1)`
  6,180 against 2,660, a dataclass `==` 6,820 against 1,850 and `<` 8,170
  against 2,040, `dataclasses.asdict` 47,200 against 15,100, a
  `__setattr__` hook 6,790 against 1,830, `queue.SimpleQueue` put and get
  13,500 against 830 (CPython's is C, ours the Python fallback), a
  thread's start and join 11,900 against 1,850, `f.read(8192)` 2.6
  times, `b"%d" % n` formatting 2.5 times, `dict(zip(a, b))` 2.1 times,
  `*args` forwarding 2.1 times, `os.stat` 1.6 times.
- **A libc `atexit` hook costs a short process 800 page faults on macOS.**
  Registering one for standard output's buffer added 5% to a small
  script's instructions (3 MB more resident at exit); the bench fixtures
  showed it as 4.6% across the board. The interpreter's exits flush
  explicitly instead (`vm_singletons::exit_process`).

- **The suite is saturated; single operations aren't** (November 12, the
  PGO build at `11c293b4`).
  - Every workload that trails CPython warm retires fewer instructions
    than CPython and more cycles: `async_tree` 0.85 of the instructions
    at 1.62 instructions a cycle against 2.37, `class_creation` 0.73 at
    0.73 against 1.06, `tomllib_loads` 0.88 at 1.48 against 1.88,
    `context_managers` 0.91 at 1.92 against 2.43, `exceptions` 0.86 at
    1.99 against 2.49, `sudoku` 1.01 at 2.08 against 2.33
    (`target/pc/warmic.py`). So instruction work on those workloads has
    run out; what's left is how much code and data a unit of work
    touches.
  - A simulation with this machine's second-level cache (256 KB, 4-way)
    as the last level puts numbers on it for a build without PGO: a warm
    call of `async_tree` takes 3.2 million instruction misses there
    against CPython 3.13's 0.25, `tomllib_loads` 6.7 against 0.8,
    `class_creation` 5.9 against 1.4, with the data misses level except
    `async_tree`'s third-level ones (0.55 million against 0.08). A
    machine with a larger second-level cache (most newer ones) pays less
    of this.
  - `async_tree`'s third-level misses are memory: its peak footprint is
    39.7 MB against 28.6, about 3.2 KB a task against 1.8. A coroutine is
    five allocations (the object 80 bytes, the frame 112, the shared
    locals cell 48, the locals and the operand stack), a future 336 bytes
    against 187, an event-loop handle 440 against 193, a task 1,082
    against 704. One allocation per frame, and a future's task-only
    fields kept apart, are what would close it.
  - `async_tree`'s peak footprint has two values, and the PGO build at
    `d1f48c5c` reads the high one: 49 MB in most runs against 40 MB for
    the build at `11c293b4`, for the same code built without PGO (quick
    and plain release builds: 40), and for a PGO build of the earlier
    code with the same profile and toolchain (40 to 41). The difference
    is transient (the regions read alike once the calls are over) and
    goes away with `WEAVEPY_TEXT_TRIM_REST=0` (39.8), with tier 2 off
    (41.5), or with a purge delay of 50 ms or less (38.9, for 13% more
    cycles and half again the page faults). So it's the allocator taking
    fresh arena ranges while freed ones wait out the 500 ms a large
    heap's ranges stay resident (see `alloc::note_resident`), tipped by
    when this build's first calls allocate. `float_points` reads 5%
    higher the same way. Not understood further than that.
  - Locked instructions aren't the suite's gap: 1 in 1,200 to 1 in
    650,000 instructions a warm call before the fix above, at most 3% of
    a workload's cycles (`context_managers`, most of it `threading`'s
    locks, which CPython pays too).
  - Single operations are where WeavePy is still behind. Over the 153
    operations the geometric mean is 0.97 of CPython's instructions.
    The largest gaps, in instructions an operation over CPython's:
    `zip` over two tuples 440 an element, `Color(1)` on an `Enum` 3,960,
    a dict comprehension of 20 items 2,400, `enumerate` 180 an element,
    `dict.update` and `dict.copy` of 20 items 1,800 and 1,360, a
    `functools.partial` call 890, `Color.RED.value` 1,850, an
    `OrderedDict` store 730 in tier 2 (150 in the frame JIT),
    `str.lower()` of 176 characters 640, `f(*t)` 620, a list
    comprehension 600, a raise caught in the same frame 570, a `*args`
    call 310. What several share: a small tuple costs about 250
    instructions to build and free (CPython about 100), a new container's
    registration with the collector 126 (`GcState::defer_container`), an
    attribute read that finds a Python descriptor takes the full miss
    path every time (about 700 instructions before `__get__` runs), and
    tier 2's `for` over a value it has no type for calls a 66-instruction
    helper an element.

- **Where an object-heavy program loses to CPython** (November 12,
  `deltablue`, the one bench fixture that does: 1.5 times CPython's time
  in CI). On a quiet machine a run of 50 iterations takes 99 ms against
  68, in two parts.
  - Steady state is 1.63 ms an iteration against 1.33, on 0.93 of the
    instructions: 2.3 instructions a cycle against 3.0. Cachegrind's
    simulation gives 287,000 instruction-cache misses an iteration
    against 14,000 (CPython's whole loop fits its first-level cache, a
    0.08% miss rate), and 185,000 data-cache misses against 53,000.
    Compiled code is 38% of the instructions and 44% of those misses: the
    program's 1,064 bytecode instructions become 47,000 IR instructions
    and 220 KB of machine code, since a callee's whole tree is lowered
    again into each caller (each of three `__init__`s carries 13 to 17
    KB). Inlining still pays for itself: without it the program retires
    44% more instructions, because a call that isn't in line costs about
    250.
  - A quarter of the instructions are attribute reads that go through a
    helper: 53,000 calls an iteration at about 72 instructions each. They
    are reads in inlined bodies at sites that have seen several classes
    (160 of 252 field sites: a base class's methods run on four
    subclasses), which the in-line read declines, though the site's
    one-entry cache hits 98% of the time in the helper; and class
    constants (`Strength.REQUIRED`: a global read's 37 instructions, then
    69 in `h_attr_stack`'s class memo).
  - The first 50 iterations cost 17.5 ms over steady state. With compiles
    synchronous the run retires 1,121 million instructions, of which
    code generation is 106 million (the worker's, natively), lowering
    about 33 million and tier-2 attempts 25 million: 2,250 instructions
    of code generation an IR instruction, spread over simplification,
    liveness, the generator's own loop and 35,000 reallocations.
- **A whole-program view of the same thing** (`context_managers`, a quick
  build): 5.6 times CPython's instruction-cache misses a call and 5.4
  times its data-cache misses, at similar branch misprediction rates. The
  code run at least 1,000 times a call covers 208 KB in 390 functions
  (compiled code about 30 KB of it), and no source line has more than
  2.4% of the data misses: they're reads of per-site records, code
  objects' side tables, counts and free lists.
- **The frame JIT's first-call page faults are heap growth, not churn.**
  `chaos`'s first call takes 1,955 page faults; 452 with the frame JIT
  off and 1,320 with its compiles synchronous. Neither a ten-second purge
  delay nor keeping large blocks in the arenas changes that, and the heap
  ends 2.7 MB larger for nine functions' 97 KB of code. The worker's
  forced collection when it goes idle is 400 of the faults, for 1 MB less
  resident. Handing each lowered function's tables back for the next
  lowering saved 95 faults and no instructions, and was dropped.
- **A fifth of an import is a code object's first use.** `import
  logging` retires 139 million instructions over start-up's 20 million
  (Linux, a quick build; CPython 3.13, whose `traceback` doesn't build
  dataclasses, retires 48 million over its 35). The profile is flat
  (the core loop 7%, then nothing above 2%), but two inclusive costs
  stand out: decoding the 966 bodies the import runs, 15,200 instructions
  each (570 a constant, 1,350 a name table), and building their VM
  extensions, 15,300 each (each constant made an object, each name
  hashed, then interned at 240 instructions). Names and string constants
  are decoded into the compiler's form and then made objects one code
  object at a time; a table of interned objects per module, indexed as
  the cache file indexes its identifiers, would do it once. Class
  creation is 40% inclusive (441 classes, with what their bodies and
  metaclasses run).
- **Tried and dropped: an in-line field read at polymorphic sites.**
  Reading the class version and position from the site's cache as the
  code runs (the helper for anything else) took `deltablue`'s helper
  calls from 33,000 to 3,700 an iteration, but the in-line read is itself
  about 43 instructions (nine guards and a counted copy), so the program
  retired 3.3% fewer instructions at equal inlining and, at the default
  budget, 4.5% more: the larger IR put twelve more callees over the
  inlining budget. What a field read needs first is fewer guards (one
  word on the instance that says "split, over the class's names,
  unborrowed, no `__dict__` published" in place of five loads).

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
- **Tier-2 analyses keep the loops a plan can't compile** (this PR).
  Carving finds the loops tier 2 doesn't compile one re-analysis at a time,
  and each attempt on a code object (unseeded, seeded, with and without
  typed list accumulators, from each loop that asks) found the same ones
  again: `dataclasses._init_fn` was carved 58 times in one
  `class_creation` process, and half of the suite's 1,300 analysis passes
  were repeats. The plan reads the code and its globals, not the
  requesting activation, so its leading failures are the same for every
  attempt: the code's cache entry keeps them (`weavepy_jit::Carved`) and
  each analysis starts from there. A signature tier 2 doesn't compile is
  rejected before the loop forest is built. The suite's compile outcomes
  are unchanged, and the time in compile attempts is down by a fifth
  (`class_creation` 18.7 to 11.3 ms, traced). On callgrind those attempts
  were 40 of the 50 million instructions `class_creation`'s first call
  retires over a warm one.
- **Module and class bodies bind their names in the fast loop** (this
  PR). The core loop took `STORE_NAME` only to rebind a module-level name
  and `LOAD_NAME` only at module scope; a new name, and every name in a
  class body, ended the burst: one full step and a fresh entry each. A
  module-level `x = 1; y = x` pair took 2,900 instructions against
  CPython's 730 and takes 850; a class with two methods goes from 43,400
  to 39,200 (CPython 64,000). The leaf arms that make a function also hand
  the `STORE_NAME` after it back to the core loop now, a quarter off a
  `def` (3,970 to 2,880 instructions on a plain release build). The
  imports measured retire 3% fewer instructions in all. `itertools`, a Python module over native cores here, also
  stopped defining its nineteen pure-Python fallback classes when the
  native ones exist (1.3 ms to import, now 0.85).
- **Wall time trails instructions through instructions per cycle.** On a
  PGO build WeavePy retires fewer instructions than CPython on all but
  three workloads (`nqueens`, `sudoku`, `logging_mix`), yet takes longer on
  about ten. Whole runs, both interpreters, on a quiet machine:
  `class_creation` 0.78 of CPython's instructions at 0.74 instructions a
  cycle against 1.09; `async_tree` 0.93 at 1.46 against 1.98;
  `context_managers` 0.92 at 1.81 against 2.20; `deepcopy` 0.98 at 1.77
  against 1.98. `async_tree` also takes 46,000 page faults against 10,000.
  So what moves wall time on those is how much distinct code and data a
  unit of work touches, more than how many instructions it retires.
- **Footprint, not RSS, is the memory gap.** Peak RSS counts the binary's
  clean code pages, which the release above moves in and out at will;
  `/usr/bin/time -l`'s peak footprint counts what a process dirties. At
  `-c pass` WeavePy's is 3.6 MB against CPython's 5.0, but each import adds
  more: `import logging` 6.7 MB against 4.3, `import json` 2.2 against 1.3,
  `import decimal` (a Python module here) 3.0 against 0.6. After `import
  logging` the allocation profile (`WEAVEPY_ALLOC_PROFILE`) has 7.5 MB
  live: strings 11%, dict tables 13%, decoded code objects 16% and their
  VM extensions 5%, inline-cache tables 11%, functions 7%, types 5%, with
  no single owner above that. In the suite, though, most of the gap opens
  in the first call, and it's the baseline JIT's: when a benchmark's
  module is imported the footprints are close (`chaos` 6.7 MB against 6.5,
  `stdlib_utils` 9.4 against 8.3, `class_creation` 8.5 against 8.1), and
  the first call then adds 3.0, 2.6 and 5.4 MB where CPython's adds 0.0,
  0.1 and 1.2. With `WEAVEPY_FRAME_JIT=0` it adds 0.9, 0.8 and 3.5, and
  takes 690, 600 and 1,340 page faults instead of 2,200, 3,000 and 3,060.
  `chaos` compiles nine functions into 97 KB of machine code; the 2 MB is
  what compiling them passes through: 1.2 MB of heap that is gone again
  afterwards (0.7 MB of it the IR builder's instruction vectors, grown by
  doubling), 0.5 MB that stays (records and tables), and the rest pages
  the allocator and the worker thread touched once.
- **A soft page fault costs 2.3 microseconds here** (5,000 instructions,
  7,300 cycles, measured as the difference the release makes). A
  workload's first call takes 1,900 of them against CPython's 480 with no
  release at all (text first touched, a heap that grows 5.6 MB against
  1.9 in `class_creation`), about 3 ms of an 80 to 100 ms call. Warm calls
  take about as many as CPython's over the suite.
- **Imports cost about 1.4 times CPython's instructions**, hidden by a
  start-up that costs 0.6 of them. Module by module (`-X importtime`),
  the common set `collections, functools, re, os, json, typing,
  dataclasses, enum, logging, argparse, subprocess, pathlib` takes 72 ms
  of import time against 53: `_colorize` 11.3 against 5.2 (its
  dataclasses), `typing` 5.6 against 3.0, `ast` 2.7 against 0.5, `enum` 3.1
  against 1.8. A code object's first use costs about 30,000 instructions
  (decoding its body from the cache 15,000, building its constants and
  name tables 15,000), a fifth of `import logging`; a `def` cost 3,000
  instructions against 1,500 before the change above, and an annotated one
  still costs 11,400 against 3,900.
  Modules that are C in CPython and Python here add their own
  (`itertools`, `_collections`, `_stat`, `codecs`).
- **Tried and dropped in the memory work (November 11)**:
  - *Hot-cold splitting* (`-hot-cold-split`, with and without a threshold
    of 0 and eight parameters): 8,194 regions leave their functions, and
    the functions programs keep running shrink from 7.9 to 7.6 MB (6.9 with
    the aggressive settings), with peak RSS level or slightly up. The
    profile says 65% of the blocks in executed functions never ran, but
    they're small and scattered (a bounds check's panic, an error return),
    not regions worth a call.
  - *Releasing only the code after a hot group* in the order file, between
    two marker functions. Start-up and imports run plenty of the hot
    functions' code once too, so the group stayed 70% resident (5.6 of 7.8
    MB); releasing everything and letting the working set fault back does
    better (`go` 15.4 against 13.7 MB) and needs no layout.
  - *A last-in-first-out cache of small blocks in front of mimalloc*, owned
    by the interpreter's thread. A hit saves 1.7% to 2.9% of a workload's
    instructions over `mi_malloc` and `mi_free`, but the checks every
    allocation and release then make cost 3% to 4%, so the suite retired
    more with it (`comprehensions` 3.5%). mimalloc already extends a page's
    free list 4 KiB at a time, so a steady churn stays within a few
    kilobytes of a size class without it.
  - *A longer purge delay for every process* (1,000 ms against 50): warm
    time 0.994 and peak RSS 1.005 over the suite. It's now 500 ms, past 32
    MB only.
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

What the everyday battery points at (`tools/pybench/everyday.py`; see
"What the measurements say"):

- A lean entry for a call from native code into Python (see "What the
  measurements say"): a callee with native code bound and run as a
  compiled caller's direct call binds and runs it, for
  `list.__contains__` and the other element comparisons, key functions,
  `map` and `filter`, hashing, and tier 2's generic operations. It is
  about 1,500 instructions a call today against 300.
- Tier 2 leaving a loop to the frame JIT when the loop's work is on
  instances (or calling comparison and operator methods directly, as
  the frame JIT does).
- `enum`: `Color(1)` (4,660 instructions against 2,670: the metaclass's
  `__call__` and `__new__`'s value lookup) and flag arithmetic (13,300
  against 7,700).
- `dataclasses.asdict` and `replace`, which need the pieces rather than
  a path: class-attribute lookups by name (`hasattr(cls, name)`,
  `getattr(obj, name)` with a computed name), global reads in compiled
  code, comprehensions.
- `__setattr__` hooks and `object.__setattr__(self, name, value)`, which
  a frozen dataclass's `__init__` calls once a field: the store-side twin
  of the attribute-read hooks. A `threading.local` store from compiled
  code is the same gap (2,260 instructions against 960: it leaves for
  the core loop).
- Builtin methods a Python class holds (`SimpleQueue.put` and `get`: 3,040
  instructions a pair against 900): the method load and the call take
  the general dispatch. And a `threading.local`'s creation (28,900
  against 3,900), thread start and join.
- `getattr(obj, name)` of a property, two-argument `super()`,
  `issubclass` against an ABC, a property through `super()`.
- `f.read(n)` in a loop (1.6 times CPython after this PR dropped an
  `fcntl` per read: the bytes are still read into a vector and copied
  into the `bytes` object) and large single writes, which go through the
  write buffer instead of past it; `os.stat` and `os.path.exists`.
- Block reads off Unix: the read-ahead's fill asks a descriptor for a
  block only under `cfg(unix)`, so a line read on Windows still takes a
  byte per read (as it did everywhere before this PR). The generic fill
  can ask a disk file for a block the same way; it needs a Windows
  machine to test on.
- `bytes` `%` formatting; `dict(zip(a, b))`; `*args` forwarding.
- Exceptions raised in a callee and caught in its caller (12,400
  instructions against 6,800 for `os.environ[missing]`), which `argparse`
  does 25 to 50 times a parse.
- Then the library cases the battery ranks next, each of which needs its
  own breakdown: `inspect` and `typing`, `email`, `configparser`,
  `datetime` (`strptime`, `strftime`, `astimezone`), `pathlib`,
  `argparse`, `uuid`, `weakref`.

What the single-operation battery points at (see "What the measurements
say"), in the order the gaps would close the most:

- A class whose metaclass defines `__call__` in Python (`Color(1)`):
  4,660 instructions against 2,670.
- Small tuples: an allocation written in place and a release that
  doesn't go through the nested-release trashcan when every element is a
  scalar or shared; `zip` and `enumerate` reusing their result tuple
  when the loop has dropped it, as CPython's do.
- `zip`'s per-element iterator steps (about 450 instructions against
  `enumerate`'s 135).
- `dict.copy` and `dict.update`: a copy of a dict whose values can't be
  part of a cycle needn't scan them again, and the entries' releases
  could run as one loop.
- Calls with `*args` and `**kwargs` (wrappers and decorators): the call
  site's gate cascade (`h_call_site` 336 instructions) and the argument
  tuple.
- Tier 2's generic `for`: tuple and list iterators stepped in the
  helper's first lines.
- The heap's peak when a large one is rebuilt (`async_tree`: 40 or 49 MB
  by the build): either the allocator reuses a freed range ahead of a
  fresh one while its purge is pending, or the long purge delay applies
  only once a range has been reused.
- The locked instructions that remain: a function's name is a
  `std::sync::Arc<str>` cloned at every `def` (`closures_decorators`
  150,000 a call), a closure's cells sit in a `OnceLock`, and
  `threading`'s locks take two a call where CPython's take one.

What the `deltablue` measurements above point at, ahead of the list below:

- Compiled code that is smaller and does less per operation: a field
  read's guards in one compare, class constants and module globals read
  in line as borrowed values (a global is 37 instructions through its
  helper in an inlined body), and inlining by how hot a call site is
  rather than by size alone, so that a constructor's tree isn't lowered
  into every subclass's `__init__`.
- Tier 2's compiles of loop-free callees wait for a native caller: 26 of
  `deltablue`'s and most of `class_creation`'s 128 attempts are methods
  compiled in case a tier-2 loop calls them, in programs with no such
  loop. (A caller's analysis would have to ask for the callee and try
  again, since it types a call from the callee's compiled return lane.)
- A tighter steady group for the early release: 4.2 MB of its 7.8 MB is
  resident when a program's imports end, since it's the union of what
  33 programs keep running and start-up runs much of it too. Profiles
  dumped at the end of the imports would tell what only start-up and
  imports run from what a program goes on running.

Two items the November 12 measurements put ahead of the list below:

- Footprint (1.29 of CPython's). First what a baseline compile passes
  through, about 2 MB a process and 1,500 to 2,400 page faults in the
  first call: IR vectors sized for the function instead of grown by
  doubling, a smaller IR instruction, scratch kept between compiles
  rather than faulted in again, and less set-up per compile. Then what an
  import leaves behind: code objects (8-byte instructions, a line and a
  column table per body, a constants vector and its object twin),
  inline-cache tables allocated at first miss for code that runs once, and
  strings held both as compiler names and as interned objects.
- The import path: a code object's first use (30,000 instructions),
  `def` (twice CPython's instructions, three times with annotations) and
  the modules that are Python here and C there. `_colorize`'s dataclasses
  are 6 ms of every program that imports `traceback`. An annotated `def`
  retires 8,150 instructions where a plain one retires 2,000: attaching
  `__annotate__` builds the function's attribute dict (three seeded
  entries and two interned names) and renaming the annotate function
  builds a second one, so a field for it, and a name computed when it's
  read, would take about half of that and 300 bytes a function.
- Much of the footprint is representation: a dict entry is 40 bytes
  against CPython's 24 (16-byte values), and a code object's strings live
  twice, as the compiler's constants and names and as the VM's objects.

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

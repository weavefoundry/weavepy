# Performance progress

This note tracks the work to make WeavePy significantly faster than CPython
3.14 on realistic workloads, measured by wall time with a profile-guided
build, plus peak memory and startup. It records where WeavePy stands, the
plan, what's done, and what's next, so a new session can pick up the work.

## Where WeavePy stands

Measured on November 19, 2026, at `c69d14ad` (PR #112), a PGO build from a
fresh training run against CPython 3.14.8 (load averages between 2 and 5).
The everyday battery: 0.966 times CPython's time over three runs (0.956,
0.957 and 0.984), against 1.258 at `83efee9b` four days earlier. WeavePy is
ahead on 28 of the 54 cases, past one and a half times CPython's time on 5
and past twice on one: `threading_queue_ops` 2.2, `argparse_ops` 1.9,
`inspect_typing_ops` 1.7, `email_html_ops` 1.6, `exceptions_ops` 1.5. A plain
release build of the same tree reads 1.18: the profile-guided layout is
worth a fifth of the battery's time (see the instruction-cache figures
under "Done"). The suite, with the `83efee9b` PGO build in the same run:
warm time 0.843 (that build 0.851), cold time 0.957 (0.952), peak RSS 1.183
(1.144) by the suite's runner and 1.172 (1.174) with footprint 1.252
(1.269) by three runs a workload under `/usr/bin/time`; the new build runs
the suite in 0.990 of the older one's warm time. The published 1.13 for
peak RSS doesn't reproduce with either build today: CPython's own figure
moves between sessions.

Measured on November 20, 2026, at `f96980aa` (PR #112), a PGO build beside
the `c69d14ad` one in the same runs: the battery in 0.996 of that build's
time (best of three runs a case), `threading_queue_ops` at 1.9 times
CPython's time where it was 2.2. Against CPython this session read both
builds about a tenth slower than the day before's (1.04 to 1.12 a run for
each), so the 0.97 above stands as the last quiet-machine figure. What the
day's work moves is outside the battery: a program with a second thread
(under 1% slower than without one, from 13% to 55%), a thread back from a
blocking call (the GIL after one switch interval, not two), a contended lock
(handed to its waiter after 5 ms), and attribute-heavy loops in tier 2 (the
`attr_access` fixture at 0.66 of its instructions), and dicts that lose keys
from the front or the middle (an LRU cache, a queue of pending work: one to
two orders of magnitude slower than CPython before, level to twice its time
now).

Measured on November 15, 2026, at `83efee9b` (PR #112), a PGO build
against CPython 3.14.8 on a host that wasn't idle (load averages between
2 and 9). The everyday battery: 1.258 times CPython's time over three runs
(1.236, 1.258 and 1.265), within the earlier build's spread; by
instructions (synchronous compiles, quick builds) 1.282 times CPython's
against 1.297 at the start of the day. WeavePy is ahead on 21 cases (20
before), past twice CPython's time on 9 and past three times on none (2
before): `file_read_binary_chunks` 2.9, `class_features` 2.9,
`enum_dataclass_ops` 2.8, `email_html_ops` 2.7, `configparser_ops` 2.7,
`threading_queue_ops` 2.7, `inspect_typing_ops` 2.7, `bigint_ops` 2.2 and
`argparse_ops` 2.1. The suite in the same session: warm time 0.842, cold
time 0.952, peak RSS 1.129. The commits after `83efee9b` (see "Done")
weren't in the build.

Measured on November 15, 2026, at `98339f92` (PR #112), two PGO builds of
that commit on the same idle host against CPython 3.14.8: one from the
usual training, one from a training that also runs the everyday battery
(the training now does, see "Done"). The everyday battery: 1.228 times
CPython's time over two runs (1.228 and 1.227) for the battery-trained
build, 1.265 (1.252 and 1.278) for the other, against 1.357 at
`2ce79fa5`. WeavePy is ahead on 20 cases (17 before), past twice CPython's
time on 9 (12 before) and past three times on 2: `enum_dataclass_ops` 3.1
and `class_features` 3.1, then `email_html_ops` 2.8,
`file_read_binary_chunks` 2.8 (its time is in the reads),
`inspect_typing_ops` 2.6, `threading_queue_ops` 2.6 (3.4 before),
`configparser_ops` 2.5 and `argparse_ops` 2.5. The suite in the same
session, battery-trained build: warm time 0.847, cold time 0.955, peak RSS
1.135, and 1.000 of the other build's time, so the battery in the
training costs the suite nothing. The commits after `98339f92` (see
"Done") weren't in either build.

Measured on November 14, 2026, at `2ce79fa5` (PR #112), a PGO build from a
fresh training run against CPython 3.14.8 on the same idle host, beside
the November 13 PGO build PR #110's figures came from. The everyday
battery: 1.357 times CPython's time over two runs (1.345 and 1.369),
against 1.431 for the earlier build in the same session. WeavePy is
ahead on 17 cases (16 before), past twice CPython's time on 12 (14
before) and past three times on 5: `inspect_typing_ops` 4.0,
`threading_queue_ops` 3.4, `class_features` 3.3, `weakref_gc_ops` 3.2
and `file_read_binary_chunks` 3.0 (1.5 by instructions: its time is in
the reads). The cases this PR went after moved the most:
`datetime_time_ops` 2.02 to 0.75, `bytes_ops` 2.36 to 1.36, `csv_ops`
2.15 to 1.40, `threading_queue_ops` 4.11 to 3.38, `glob_fnmatch` 1.61 to
1.36. The suite in the same session: warm time 0.849, cold time 0.949,
peak RSS 1.123, and 1.004 of the earlier build's time, so the battery's
fixes cost the suite nothing. The commits after `2ce79fa5` (see "Done")
weren't in that build; by instructions on quick builds the battery
stands at 1.34 times CPython's against 1.45 at the start of this PR.

Measured on November 13, 2026, at `4e230310` (this PR), a PGO build from a
fresh training run against CPython 3.14.8 on the same idle host. The
benchmark suite: warm time 0.850, cold time 0.959, peak RSS 1.137, and
0.996 of the time of the last PGO build measured here (`d1f48c5c`) in the
same run, so nothing in between cost the suite anything. WeavePy is ahead
on 22 of the 33 workloads warm and 17 cold; the best warm times are
`scimark` 0.51, `pickle_roundtrip` 0.57 and `raytrace` 0.60, the worst
`async_tree` 1.25, `sudoku` 1.14, `context_managers` 1.12 and `deepcopy`
1.11. Whole processes take 0.90 of CPython's time, `-c pass` 0.70 to 0.75
(see "Measurement notes" for why that is a range), peak RSS is 1.14 and
peak footprint 1.22 of CPython's (1.14 and 1.22 for the earlier build in
the same run).

The everyday battery (`tools/pybench/everyday.py`, 54 cases), which is
where this part of the PR worked: 1.44 times CPython's time on average
over two runs (1.45 and 1.43), against 1.79 for the earlier build in the
same session. WeavePy is ahead on 12 cases (9 before), past twice
CPython's time on 14 (18 before) and past three times on 5 (11 before):
`threading_queue_ops` 4.2, `inspect_typing_ops` 3.9, `weakref_gc_ops`
3.3, `class_features` 3.3 and `enum_dataclass_ops` 3.3. The cases this PR
went after moved the most: reading a file's lines 117 to 0.88, writing
them 13.6 to 0.84, `os_listdir_walk` 3.6 to 1.5, `print_to_buffer` 1.9 to
1.0, `csv_ops` 3.3 to 1.9, `threading_queue_ops` 5.8 to 4.2,
`enum_dataclass_ops` 4.1 to 3.3, `datetime_time_ops` 2.6 to 2.0,
`argparse_ops` 3.2 to 2.5, `math_ops` 1.4 to 0.93. The library cases
nobody has broken down yet (`inspect` and `typing`, `weakref`, `email`,
`configparser`, `asyncio`) are where they were. So: ahead of CPython on
the suite, still well behind it on a good part of ordinary library code,
and "Next" below is ordered by that.

Measured earlier on November 13, 2026 (this PR, quick builds, no fresh PGO run):
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

- **Container operators, iteration adapters, and class method sites**
  (PR #112, November 17). Ratios are instructions, WeavePy's over
  CPython's; cycles where noted.
  - A `BINARY_OP` that built a heap value on the generic path didn't mark
    its site, so compiled code took the numbers' helper and left at the
    next instruction (`d1 | d2`, `k & s`, `b * 3`). The site is marked
    now, and compiled code serves dict union, `bytes` repetition and
    `bytes % args` natively. `plain_compare` answers two exact dicts' `==`
    (1,135 cycles to 215, CPython 160), two sets' comparisons (1,444 to
    710, against 400) and a keys view against a set (1,083 to 753,
    against 542) when no key or value can run Python.
  - `zip` replaced its result tuple whenever it built one, so `for t in
    zip(xs, ys)`, which still holds the last tuple, never reused one; it
    keeps the tuple before as a spare now, indexes tuple and list cursors
    directly, and stops in compiled code when its release frees nothing
    that could finalize: a ten-item `for t in zip(t, t)` 3,878 cycles to
    2,460 (1,770), three sources unpacked 5,366 to 3,788 (1,917).
    `list(islice(lst, 10))` takes the slice at once (7,879 instructions to
    4,620, against 3,679), a collecting consumer takes a `yield from` over
    a sequence in one run (12,506 to 8,171, against 5,490), and
    `dict(zip(ks, gen))` takes its pairs straight from the sources
    (16,066 to 11,355, against 6,247).
  - A method load on a user class receiver resolved `C.s(x)` and
    `C.make()` through the type lanes every time: the site remembers a
    class's plain function, static or class method now, keyed by the
    class's and its metaclass's versions, a metaclass method of the same
    name being a non-data descriptor the class overrides (`C.s(1)` 919 to
    297, against 678; `Color(1)` 5,491 to 3,917, against 2,672).
    `object.__setattr__` stores into a plain instance natively, which a
    frozen dataclass's `__init__` calls per field (`Key(i, "x")` 5,135 to
    3,956, against 3,392), and the current interpreter, which 248 builtin
    sites look up, is one thread-local load.
  - A frameless call of a pure leaf with keyword-only parameters cached no
    call shape, so the frame JIT never knew the callee: `fkw(i)` for `def
    fkw(a, b=2, *, c=3)` 901 to 356 (677). `None` and `bool` keys take the
    native dict probe (argparse's `registry[None] = cls`), a list's
    unit-step slice assignment splices in place, the type cache compares
    names up to 32 bytes in line (17-byte dunders went to `memcmp`), and
    tier 2 stops analyzing code from its back edges once it has handed the
    code to the frame JIT (argparse's `_parse_known_args` was analyzed
    ten times, twice each).
  - Later the same day. `decimal` keeps the context it found last under a
    `contextvars` generation, which every change of the current context
    and every mapping write moves (unique across threads): `D1 * D2` 1,466
    to 1,306 (1,028), and `decimal_telco` in the suite 1.17 to 1.07.
    Exited threads leave their type cache and literal-string cache to the
    next threads instead of zeroing and dropping them (a thread's start
    and join 728,556 to 523,657, against 182,851). A named tuple's field
    read off a local answers from its site's stamp first (two reads 1,076
    to 382, against 821). `MAP_ADD` and `SET_ADD` have their own helpers,
    as `LIST_APPEND` had, and the three read a local's value in place
    (`{x: x for x in t}` 6,803 to 5,994; `{x for x in t}` 5,842 to
    4,659). Keyword dicts and dict literals insert keys whose hashes
    differ pairwise without probing, and every container compiled code or
    the core loop builds defers its collector tracking without scanning
    its elements (a collection promotes the deferred ones that can anchor
    a cycle first): `{'a': i, 'b': 2}` 1,331 to 1,119 (937). `bytes.strip`
    tests membership through a table, and `dict(zip(ks, gen))` takes its
    pairs straight from the sources. Tier 2 gives up OSR entries that
    average fewer than 16 iterations (the floor was 8): `difflib`'s
    `find_longest_match` entered its inner loop about nine times a pass,
    paying a hand-off and a frame rebuild each time while the frame JIT,
    which had left the code to tier 2, kept its outer loop interpreted;
    `SequenceMatcher(...).ratio()` ran 10,265,084 instructions to
    4,195,398 (CPython 4,487,196).
  - That evening. Compiled code stores into an `OrderedDict` through the
    leaf half of its own `__setitem__`, which the class caches under its
    attribute version, instead of the general store: `od[k] = v` in a
    tier-2 loop 1,325 to 897 (701), in a called function 1,178 to 947
    (996). The frame JIT's direct call of a built-in subclass instance's
    native method takes small int arguments (`od.move_to_end(5)` 786 to
    492, against 434). A method site remembers a callable an instance holds
    in a materialized dict, by position and name, as it did for a split one:
    a `threading.Condition` over a lock with `_release_save` assigns its
    names out of the class's order, so `self._is_owned()` took the full
    method load (`notify()` 2,427 to 1,751, against 1,057). Reading a native
    method off such an instance (`self._lock.__exit__`, to call with
    `*args`) answers from the site's cached builtin before three slow
    paths, and the leaf registry remembers its last hit: `with cond:` 4,312
    to 3,124 (2,415), a `Queue` put and get 16,368 to 14,016 (11,764).
    `test_set` failed about once in 40 runs: a set mutated from an
    element's `__eq__` while another operation held it queued the mutation
    by address alone, so a later set at the same address replayed a stale
    `clear()` on its next `add`. The queue now holds a weak handle and drops
    a dead set's ops. `re.match(compiled, s)` went through `re._compile`,
    which raises and catches a `KeyError` before handing a compiled pattern
    back; the native cache front now returns it (16,572 to 1,703, against
    7,626; `ConfigParser.write` 3.46 M to 1.60 M, against 1.94 M). A method
    site keeps a native stream's method for its kind (three `StringIO`
    writes and a `getvalue()` 8,450 to 7,737, against 4,663), and a subclass
    of `str`, `int`, `float`, `tuple`, `bytes` or `frozenset` that keeps its
    base's `__new__` wraps an argument of the base's exact type directly
    (4,995 to 2,254, against 1,409).
  - Loops over opaque iterators. Tier 2 stepped a set through its fully
    general iterator step and dict keys after that step's setup; it now
    steps list, tuple, range, set and dict iterators in place for any lane
    (10 set items 6,691 to 3,188, against 1,978; 10 keys 5,498 to 4,553,
    against 1,976). `for a, b in pairs` reads each pair where it sits (4,821
    to 3,069, against 1,657), `enumerate` over a list or tuple reads its
    pairs in place in both JITs (a tier-2 `i * x` sum 7,565 to 4,293,
    against 6,342), and an exhausted `reversed(xs)` leaves compiled code's
    loop in place instead of through the interpreter (3,891 to 2,192,
    against 2,230). The frame JIT runs these loops for less than tier 2
    still does, but handing tier 2's code with such loops to it cost
    `pyaes` 2.6 times its instructions, `deque_ops` 1.6 and `richards` 1.5:
    their int work wins back more than the iterator steps cost.
  - Container methods. A frozenset of atomic values isn't registered with
    the collector (it can't change, so it can never close a cycle;
    `gc.is_tracked` registers it on asking): `frozenset((1, 2, 3))` 2,181
    to 1,446 (1,432). `dict.update` from a plain dict whose keys run no
    code, `dict.copy`, and `set.add` and `set.update` of natively hashed
    values have leaf halves, which call sites run in place instead of
    through the general call (`set.update` also cloned the whole set as a
    snapshot on every call): `x = {}; x.update(sd)` 2,237 to 1,771 (1,500),
    `x = set(); x.update(t)` 3,598 to 1,978 (1,700).
  - Strings and collection. `str.partition` and `rpartition` return the
    separator and an unsplit string themselves, as CPython does, and build
    their pieces from slices; `splitlines` and `split` scan short ASCII
    strings by byte; one-character pieces are the shared strings
    (`partition` 1,266 to 838, against 987; `splitlines` 1,574 to 1,215,
    against 997). `list(enumerate(xs))` over a list or tuple builds its
    pairs at once (6,843 to 4,853, against 4,521). `min`, `max`, `sorted`
    and `list.sort` call a leaf builtin key's body directly and read a
    built-in value's length natively for `key=len` (`min(l, key=abs)` 5,515
    to 3,587, against 3,573; `sorted(w, key=len)` 6,715 to 5,767, against
    4,510), and `hex`, `oct` and `bin` of a machine int format in a stack
    buffer (1,354 to 793, against 786).
  - Leaks found on the way. A cycle through a list or dict that a builtin
    built was never collected when the builtin didn't register it: list
    slices and starred targets in the core loop, `list.copy()`, `dir()`,
    `json.loads`, `re.findall`, `dict.fromkeys`, the generic `dict | dict`,
    `+` and `*` of lists, and every binder's `**kwargs` dict. They now
    defer their tracking as other built containers do (`json_bench` pays
    4% for it). A `**kwargs` dict that its code only ever feeds to a `**`
    spread (a decorator's wrapper) can't outlive its call and skips the
    registration; one that may escape pays about 170 instructions.
  - Builtin subclasses and getters. A compiled call site settled on a
    container's or string's method declined an instance of a subclass (a
    `Counter`, an `OrderedDict`, a `class L(list)`) and sent it through
    the general call, which found the method's receiver shim, checked the
    receiver and unwrapped it. The site now runs its kernel on the
    instance's native payload (a dict view keeps the instance), and
    `len()` and `in` read the payload when the class keeps the base's own
    `__len__` or `__contains__`: `d.get(k)` 1,990 to 432 (CPython 671), a
    list subclass's `append` and `pop` 3,521 to 652 (767), `len(d)` 2,014
    to 400 (359), `k in d` 1,666 to 319 (573), `d.items()` 1,801 to 861
    (880). A site settled on `itemgetter`'s or `attrgetter`'s native
    `__call__` takes the instance itself, which it had passed to the
    general call to rewrite: `ig(t)` 737 to 477 (484). A differential
    sweep against CPython found `round(True)` returning `True` and
    `float.hex` and `int.bit_length` accepting an argument.
  - Bytes and binary files. A sized read of a buffered binary file made
    one `read(2)` per call into a vector, then copied that into the
    `bytes` object; a read of less than half a buffer is now served from
    the stream's read-ahead, which one `read(2)` of a buffer's worth fills
    (CPython's `BufferedReader` does the same, and line reads already
    shared it), and a larger read lands in the result's own storage. A
    read to the end of a regular file is sized by `fstat` and read into
    the result directly instead of through 64 KiB chunks and two copies,
    a write of at least a buffer's worth with nothing staged goes to the
    descriptor without passing through the write buffer, and `bytes * n`
    builds in the result's storage (a fill for one byte). Reading 4 MB in
    8 KiB chunks went from 5.9 million instructions to 3.6 million
    (CPython 1.7 million), reading it whole from 12.8 to 4.4 (1.5),
    writing it from 8.6 to 4.2 (4.2), and `b"x" * 4_000_000` from 9.4 to
    4.7 (1.0). The rest of the last two was page faults: mimalloc maps
    blocks over 2 MiB from the OS and unmaps them when freed (see
    `alloc.rs`), where macOS's allocator keeps a freed large block for
    reuse, so each round faulted its 4 MB in again. The allocator now
    keeps up to four such blocks (32 MiB each, 64 MiB in all) for an
    allocation of about their size, freeing any idle past the purge delay
    at the runtime's memory ticks. In wall time, `b"x" * 4_000_000` went
    from 1.9 ms to 0.08 (CPython 0.33), a 4 MB whole-file read from 2.3 to
    0.22 (0.52), and `bytearray(4_000_000)` from 1.4 to 0.08 (0.31), with
    the suite's peak RSS unchanged. `[0] * 1_000_000` also copied each
    item through an iterator: a unit of values without a reference count
    now copies bitwise, doubling (58 million instructions to 5.1, against
    13.2; 10.3 ms to 1.3, against 1.3). `bytes.split()` and `bytes.hex()`
    without options build their pieces and digits directly (5,284 to
    3,546 against 3,386, and 2,197 to 1,377 against 1,185).
  - Loops calling a function held in a local. Tier 2 compiled such a loop
    and called the function through its generic call helper, about 800
    instructions more a call than the frame JIT's direct entry: `f(1, 2,
    3)` of a local `f` ran 932 instructions an iteration under tier 2 and
    114 with it off (CPython 515). The analysis now records which of a
    loop's dynamic calls have a Python callee, and a loop whose typed
    arithmetic and guarded calls don't repay them (24 typed operations,
    or one guarded call, for each) is left to the frame JIT: 932 to 198.
    A float-heavy loop with such a call stays in tier 2, which saves it
    about 30 instructions a float operation. Fixtures, suite and battery
    are flat. A call of a global function tier 2 can't compile (one with
    keyword-only parameters, `*args` or `**kwargs`) took the same generic
    path; the analysis now recognizes those too, by the `LOAD_GLOBAL`
    ahead of the call's self-or-null slot (`fkw(i, c=4)` 1,115 to 350,
    against 697). Only callees of at most 64 instructions count: the
    first version of the rule also turned away `nbody`'s driver loop,
    whose callee is a long function tier 2 compiles itself (+37%).
    Tier 2 now compiles functions with keyword-only parameters too: they
    are locals the caller's binding fills before the body runs, so the
    analysis seeds and guards them like the positional ones. A loop in
    `def f(n, *, scale=2, off=1)` ran under the frame JIT at 67 ms for
    twenty calls of 200,000 steps (plus a float loop of the same shape)
    and runs at 36 ms in tier 2 (CPython 351). Direct native calls still
    take only positional callees.
  - Helpers that call helpers. A loop calling a short global function
    that calls another (`g_mid(t)` with `return g_leaf(x)`) ran 1,045
    instructions an iteration (CPython 932): tier 2 took the loop and
    called `g_mid` through the frameless evaluator, which interpreted
    both bodies. The analysis now asks, for each guarded call of a
    global, whether its callee would run that way (a short loop-free
    function tier 2 hasn't compiled as a scalar leaf and that isn't one
    of the evaluator's tiny shapes), and the call-bound rule counts such
    calls as it counts dynamic ones. The frame JIT then took the loop,
    but lowered only `g_mid` in line: `g_leaf` had run only nested in
    `g_mid`'s frameless evaluations, which counted nothing toward its
    warmth, and its call went through the inlined body's call helper
    (377 instructions). A callee of a callee lowered in line now counts
    as warm once the evaluator has built its plan (a counter in the
    evaluator's in-place call path instead cost `call_overhead` 10% of
    its cycles at the same instruction count). The chain runs at 342
    (a closure chain 833 to 358), `g_two` with a branch 651 to 252, and a
    float loop around a tiny callee keeps tier 2's 155. Code whose
    loops have run but that tier 2 still holds nothing for (`argparse`'s
    `_get_values`, whose comprehensions it only ever left cold) went on
    waiting for tier 2 for good; the frame JIT now takes it after 16
    such triggers. The battery runs 0.6% fewer instructions (`email`,
    `traceback` and generator cases 5%); fixtures are flat.
  - Constructors. A sweep of 45 common constructions against CPython
    found builtin types two to four times slower to build: their calls
    went through the type call, the instance plan and `__new__`/`__init__`
    dispatch even when they only make an object. A builtin type now
    carries a pure constructor for the arguments that run no Python code
    (the pure arms `builtin_ctor_pure` already had, one per type, so a
    type without one misses at a load instead of after twenty
    comparisons): `object()` 2,314 to 815 instructions (CPython 727),
    `threading.Lock()` 2,797 to 1,547 (965), `io.BytesIO()` 3,875 to 2,015
    (972), `io.StringIO()` 4,014 to 2,124 (2,028), `complex(1, 2)` 3,115 to
    724 (1,525), `memoryview(b)` 2,816 to 1,189 (1,523).
    `threading.RLock()` forwarded an empty `*args, **kwargs` to the native
    lock: 8,398 to 3,032. Still slow: `array.array` (a pure-Python module,
    9.6 times CPython), and classes partly in Python (`defaultdict(list)`
    2.9, `OrderedDict()` 4.6, `functools.partial` 2.9, `itemgetter` 2.8).
    `os.scandir` and `os.listdir` validate names as UTF-8 before the
    lossy decoder, which cost about 300 instructions a name.
    `OrderedDict()` returns from `__init__` when there's nothing to add
    (4,762 to 3,077, against 1,039).
  - Smaller single operations, from a sweep of 80 common statements.
    `sum(d.values())` and `sum(d.keys())` read the dict in place (3,734 to
    1,548 for three items, against 1,678). The pure `zip` steps, which
    only produce an item when every source has one, take strict zips too,
    as the end where `strict=` is checked stays the general step's (a
    `for a, b in zip(x, y, strict=True)` loop over five items 16,909 to
    7,255, against 4,973). `del xs[a:b]` of a unit step drains by its
    bounds instead of building every index, and compiled code deletes
    such a run itself when its items release without running code
    (`L.extend(x); del L[-5:]` 2,873 to 1,460, against 1,399); the items
    now release last first, as CPython's do. Finished, joined threads
    leave the runtime's thread registry, which every reap scanned in full
    (thread start and join 663k to 545k instructions over a thousand
    threads, still 3.2 times CPython's).
  - A second sweep, of 104 statements over the standard library's
    smaller types and builtins. Complex arithmetic left compiled code for
    the core loop's operator dispatch each time; the frame JIT's
    binary-operation helper now computes `+ - * /` of an exact `complex`
    with a number itself (`c1 * c2` 2,008 instructions to 584, against
    540). `setattr(obj, name, v)` of a plain instance is a leaf builtin
    that stores into the instance dictionary as the generic path does,
    the name interned (1,953 to 1,211, against 699). `functools.reduce`
    joins the builtins compiled code and the core loop run in place
    instead of leaving for the full call path (`reduce(operator.add,
    xs)` 4,641 to 1,916, against 2,033). `sorted(d)` reads a dict's keys
    directly rather than through an iterator (4,121 to 2,160);
    `{**d}` and `f(**kw)` copy the source's table into the empty target
    whole; `str.center`, `ljust`, `rjust` and `zfill` build in one
    allocation and return an exact `str` that needs no padding as itself
    (`center` 2,208 to 868, against 923); `bytes.join` of a list or tuple
    of `bytes` reads the parts in place (3,153 to 2,316); and
    `struct.unpack` of `bytes` skips the buffer adapter (2,536 to 2,074).
    `itertools.accumulate` and `product` stay 2.4 and 2.0 times CPython's:
    their iterator steps spread over a dozen helpers. Container methods
    without an in-place lane paid the method-call protocol, about 250
    instructions around a cheap body: `dict.copy` and `set.copy` now run
    as opaque bodies compiled code calls directly, `dict.pop` of a `str`
    or `int` key is a leaf (778 to 588, against 542), and `list.remove`
    has a kernel when the items up to the match compare natively and the
    removed one releases without running code (2,005 to 1,329, against
    1,289; the full path scans the same way first). `dict.pop()` and
    `dict.copy(1)` now raise CPython's messages.
  - Keyword calls. Compiled code constructing a class with keywords
    (`Parameter(name, kind, default=d)`) worked out the keywords'
    permutation onto `__init__`'s parameters, comparing every name, on
    each call (about 330 instructions); a per-thread memo keyed by the
    function, its code, the names tuple and the argument count keeps it,
    asking afresh for a function whose defaults may have been reassigned.
    The keyword binder also reads keyword-only defaults by position when
    they line up with the parameters, as positional calls already did.
    `P("a", 1, default=i)` with two keyword-only parameters 3,294
    instructions to 2,838 (CPython 3,365).
  - User-class protocols, from a third sweep of 83 statements. `bool(x)`
    of an instance took the type call's dispatch and several method
    lookups (a plain instance 3,933 instructions to CPython's 422): it is
    the truth test now, the in-place `__bool__`/`__len__` evaluation
    first and `PyObject_IsTrue`'s protocol after (465; through a Python
    `__len__` 4,534 to 1,398). A missing attribute that a pure-leaf
    `__getattr__` answers ran it as an activation behind a cascade of
    lanes that each missed; compiled code and the core loop now evaluate
    it frameless first (3,869 to 1,319, against 1,010). Frameless
    evaluation also runs `x in c` and `not in` over a native container
    (anything that could run code declines), so a `__contains__` that
    tests its own list evaluates in place: `2 in box` 3,759 to 1,047
    (1,026). `__bool__ = None` now raises CPython's "cannot be interpreted
    as a boolean". `int(text, base)` of plain digits is the pure
    constructor's (`int('ff', 16)` 2,503 to 814, against 986), and
    `"".join(reversed(s))` reads the string's characters into the
    snapshot directly and joins the rest of it at once (10,403 to 7,664,
    against 4,362); `str.join`'s error for a non-`str` item now names its
    index, as CPython's does. `list(obj)` of an instance whose `__iter__`
    returns `iter(self.items)` ran `__len__` (the length hint) and
    `__iter__` as full calls and stepped the iterator one item at a time;
    both now evaluate as pure leaves when they can (`iter()` joins the
    builtins a frameless evaluation may call: it mutates nothing), and a
    list's or tuple's iterator drains at once (8,138 instructions to
    5,073, against 2,736). An `__iter__` that returns a list or another
    non-iterator now raises CPython's `TypeError` on every path; a `for`
    loop used to iterate it. Each loop over a dict registered its
    iterator's mutation watch in a thread-local hash map and removed it
    after (about 200 instructions a loop); the first eight watched dicts
    now sit in a vector scanned in place, the map holding any beyond
    (`[k for k in d]` of three keys 2,541 to 2,410 under callgrind).
    `min(xs, key=len)` ran `len` through the general call for each item,
    the key path taking only some of the leaf builtins' kinds; it takes
    every kind now, and compares machine-int keys directly (5,756 to
    3,929, against 3,864). `format(x, 'g')` computed `str(x)`, a shortest
    repr nothing used, before parsing the spec, then formatted the fixed
    form a second time after the scientific one: a plain `g`, `G` or
    `.Ng` spec of a float now goes straight to the general format, which
    lays the fixed form out from the scientific digits (rounded at the
    same place), and `format()` of an exact `float`, `int` or `str` is a
    leaf builtin (5,272 to 3,874, against 2,417). A non-`str` spec raises
    CPython's message. `list(zip(a, b, c))` over lists and tuples stepped
    the zip one tuple at a time through the pure steps; it drains in one
    pass now, then takes the end step CPython's does (one more item from
    each source before the first exhausted one), so the sources are left
    exactly as stepping leaves them (9,056 to 6,069, against 4,556).
  - A fifth sweep (74 statements over I/O buffers, dates, regular
    expressions, `itertools` and `functools` variants). `date.fromisoformat`
    was the pure-Python parse (14,759 instructions to CPython's 1,513); the
    exact class's `YYYY-MM-DD` and `YYYYMMDD` now parse natively beside
    `datetime.fromisoformat`'s, every other spelling and every error the
    Python method's (1,056). `math.fsum` and `math.dist` stepped their
    argument through the interpreter's iterator, publishing the
    interpreter for each item; a list or tuple of floats and ints is read
    in place (7,043 to 2,850, against 1,590), and their `TypeError` now
    reads as CPython's. `str.maketrans` joins the builtins run in place
    (7,615 to 5,968), and `sum(reversed(xs))` of ints adds the rest of the
    reversed snapshot at once (3,558 to 2,286, against 1,755). A `for`
    loop over a Python iterator class ran its `__next__` as a full
    activation through the general step for every item, compiled code
    having no lane for it (440 instructions an item to CPython's 136): a
    `__next__` that evaluates as a pure or effect leaf (`self.i += 1;
    return x`, its stores committed at the return) now runs frameless from
    compiled loops and from the native consumers (`list`, `sum`), its
    `raise StopIteration` still the full path's (248 an item; `list(it)`
    of ten items 3,279 to 2,738). The core loop's `FOR_ITER` takes the
    same shortcut, so a cold loop over one runs 282 an item instead of
    532.
  - A native class method read through a class (`dict.fromkeys`,
    `itertools.chain.from_iterable`, which `itertools` installs over the
    Python one) resolved through every frame JIT read lane before the
    general one bound it, about 3,000 instructions a read to CPython's
    700. The site's class memo now holds the descriptor and binds a fresh
    method at each read, as `classmethod.__get__` does, and a native
    static method's function (`str.maketrans`) is memoized as a builtin
    is (645 instructions, 0.93 of CPython's). `dict.fromkeys` of an ASCII
    string, a dict, or a set builds in place as a list's keys do (6,821
    to 1,582, against 2,352).
  - `islice` steps its source in place when the source is a plain list
    or tuple cursor, but a `chain` source always took the general step,
    about 480 instructions an item through both adapters. A `chain` whose
    current source is a cursor with an item left, or whose next source is
    a list or tuple with items (the spent cursor's release freeing
    nothing), now steps in place for one-source adapters, so
    `list(islice(chain(xs, xs), 4))` runs 10,011 instructions instead of
    13,365 (CPython 4,837).
  - A Python descriptor with `__get__` but no `__set__` or `__delete__`
    (a non-data descriptor, `functools.cached_property` among them) left
    every read through its instances to the full path, since the
    instance's own entry may shadow it: a read that ran its `__get__`
    took 5,235 instructions to CPython's 924, and one that found the
    cached value 936 to 409. The class attribute cache now records such a
    descriptor beside the data descriptors, the instance's own entry
    answers ahead of it in the in-place lanes, and on a certain miss its
    `__get__` runs through the data descriptors' call lane (1,002 and
    395 instructions; a fresh object's first `cached_property` read
    11,087 to 5,952, against 3,899).
  - `itertools.starmap` had no in-place step, so each item took the
    general one (about 2,000 instructions). Over a list or tuple of
    tuples with a callback that runs as a pure leaf it now steps as
    `map` does: `list(starmap(operator.add, pairs))` of eight pairs
    18,403 to 8,135 (CPython 4,572).
  - `dict(**d)` copied the keyword mapping the call had just built,
    passing each key through a Rust string on the way. That mapping is
    held by nothing else, so with all its keys exact strings it is now
    the new dict (5,948 instructions to 2,367, against 2,443). An exact
    string's `template.format(**d)` likewise formats from that mapping as
    `format_map` does, in place, when its values render plainly (8,523 to
    7,013, against 3,943); every error stays the general call's.
  - `getattr(obj, name)` with no default declined its in-place lane for
    every built-in receiver, so `getattr(s, 'upper')()` took the full
    call (3,013 instructions to CPython's 1,291). A string's or
    container's method now binds in place as the attribute load binds it
    (1,589).
  - `member in SomeEnum` bound `EnumType.__contains__` to the class and
    called the bound method generically; a metaclass's plain-function
    `__contains__` is now called with the class directly, through the
    dunder lane that evaluates pure leaves in place (4,455 instructions to
    2,718, against 1,141).
  - Python 3.14's `heapq.nsmallest` and `nlargest` call the new max-heap
    accelerators (`heapify_max`, `heapreplace_max`), which the native
    body lane didn't list, so every call asked twice whether it could
    take one and then went through the full call. They join `heappush`
    and the rest, and a call site remembers a leaf builtin called
    unbound the first time (`heapq.nsmallest(2, xs)` 27,568 instructions
    to 18,258, against 11,260).
  - A crash fixed on the way, present since before this PR:
    `test_statistics.TestKDE` segfaulted in most runs outside the gate.
    `statistics.kde`'s cumulative `cdf` runs `sum(W(...) for x_i in
    supported)` from compiled callers; the generator's call of `W` put
    its frame on the pending list before the lazily running callers
    (`p_expected`, `cdf`) registered theirs above it. When `W` returned
    the generator's entry wasn't on top and stayed behind, naming an
    activation gone from the native stack, and a later flush wrote
    through it (found with memcheck on a system-allocator build). An
    entry now leaves the list from wherever it sits.
  - `BytesIO.write` had an in-place half only for buffered file writers,
    so every write to a `BytesIO` took the full native body (1,464
    instructions to CPython's 545); a write at the stream's end with
    nothing exported now appends in place (850). `getvalue()` of an open
    `BytesIO` or `StringIO` gets a leaf half too (1,097 to 922).
  - `OrderedDict` is a Python class over a native order dict that starts
    on the first store. The store's leaf half declined while there was
    none, so every fresh `OrderedDict`'s first store took the full path;
    it now starts the order in place (three stores into a fresh one
    12,212 instructions to 7,246, against 2,956). Construction from an
    iterable unpacks an exact 2-tuple directly, the other shapes still
    stepping as CPython's `odict` does for its errors (from a generator of
    five pairs 47,414 to 36,109, against 10,146). `list(od)` of an exact
    `OrderedDict` called its `len` and its `__iter__` through the generic
    protocol and stepped the order's iterator; it takes the order's keys
    at once now (8,647 to 3,392, against 2,437), and `list(od.items())`
    and the other views take their entries likewise where every key looks
    up natively (14,502 to 6,980, against 4,469).
  - `obj.__dict__` reached the reader that answers it from the site's
    memo only after a dozen other lanes (properties, class values,
    published dicts) declined; a site that has read the managed dict
    for the instance's class at its version now reads it first (811
    instructions to 255 in the container). `for k in d` over a plain dict
    steps its keys directly once the iterator's watch is set, without
    the views' and `OrderedDict`'s checks (20 keys 5,167 to 4,341).
  - `SomeEnum(value)` ran `EnumType.__call__` and `Enum.__new__` in
    Python for every lookup (`inspect.Parameter` converts its kind that
    way, `re` its flags): 5,642 instructions for `Color(1)` to CPython's
    2,659. Where a class's metaclass `__call__` and its `__new__` are
    `enum`'s own and it has members, a member of the class comes back as
    it is, and a value that hashes natively is looked up in
    `_value2member_map_` (the verdict and the maps remembered under the
    class's and metaclass's versions); anything else is the Python
    call's (`Color(1)` 1,530, `Color(member)` 960 to 2,521).
  - A dataclass's `__eq__`, evaluated in place, read `other.__class__`
    and `self.__class__` through the general instance-attribute
    resolution each time; the in-place read answers `__class__` first
    where `object`'s descriptor does (`dc == dc2` 2,362 instructions to
    1,944 in the container).
  - `itertools.takewhile`, `dropwhile` and `filterfalse` had no in-place
    step; over a list or tuple cursor with a predicate that runs as a
    pure leaf they step as `filter` does, `takewhile` consuming the item
    that stops it as CPython's does (`list(takewhile(lambda x: x < 3,
    xs))` 8,673 instructions to 6,181 in the container).
  - A compiled `for i, x in enumerate(xs)` stepped its pairs in place
    but took its end through the general step, which in a generator
    expression materialized the generator's frame shell and finished the
    run in the general loop, once per generator. The end of a list or
    tuple cursor now retires the iterator in place as those cursors'
    ends do, detaching from the list where nothing its release frees
    could finalize (`sum(i * v for i, v in enumerate(xs))` of five
    items 8,697 instructions to 6,226 in the container). The core loop's
    `enumerate` arm ends in place the same way (`_strptime`'s loop).
  - The battery's slow-step census (`WEAVEPY_BURST_STATS=1` over every
    case) showed slices of `bytearray`s and of `str` subclasses (`re`'s
    `_plain_slice` over `email`'s header tokens) taking the full step at
    every call. The core loop's slice arms take a `bytearray` (a new
    one), and a `str`, `list` or `tuple` subclass keeping its base's
    `__getitem__` (the base's exact slice, as CPython's). `str.encode`'s
    leaf half reads a `str` subclass's text too (`email.utils`'s
    `_has_surrogates` over header values), and the core loop's
    `GET_ITER` iterates a `list` or `tuple` subclass's payload when its
    class inherits the base's `__iter__`, as compiled code already did
    (`email`'s token lists). `os.stat`, `lstat`, `fspath`, `getcwd`,
    `access`, `listdir` and `locale.setlocale` join the native-body lane
    (through the interpreter's call, for a path-like's `__fspath__`), so
    a call no longer takes the full step and its frame shell
    (`os.path.exists(p)` about 4,000 instructions fewer, 1.34 to 1.12 of
    CPython's; `locale.getlocale()` 1.24 to 1.05). So do `fsdecode`,
    `fsencode` and `dir`, the hash constructors and `zlib`'s functions,
    `sys.audit`, `weakref`'s dead-entry callback, `json`'s encoder
    constructor and the import system's module locks, the rest of the
    census's builtin calls (`hashlib_zlib` 1% fewer instructions,
    `file_small_open_close` 2%, `os_stat_exists` 12%). `with open(p) as
    f` loaded the file's `__enter__` and `__exit__` through the full step;
    `LOAD_SPECIAL` takes a native file's own methods in place (another
    1.5% of `file_small_open_close`). A function-level `import m` or
    `from m import x` of an imported module, in place in compiled code
    already, is the core loop's helper's too, for interpreted callers
    (`shlex.__init__`'s imports at every construction).
  - A loop over a Python iterator class whose `__next__` isn't a leaf
    (`email.feedparser`'s `BufferedSubFile`) took the full step for
    every item, compiled code exiting to the core loop to take it. The
    `FOR_ITER` lane calls such a `__next__` with the activation waiting
    on the pending list, its `StopIteration` the loop's end, and compiled
    code takes the lane in place (`Parser().parsestr(msg)` 8% fewer
    instructions).
  - Tier 2's item store into an exact dict went through the generic
    store and rechecked every guard afterward, where its item read had a
    leaf path; a store by a key that hashes and compares natively now
    goes straight into the dict (`d[k] += 1` in a compiled loop 1,276
    instructions to 1,024, against 743).
  - `defaultdict` construction and misses ran its native `__init__` and
    `__missing__` through the generic paths: `defaultdict()` 4,817
    instructions to CPython's 1,484, a miss 6,888 to 2,105. The exact
    class's plain calls build the instance in place through its own
    constructor hook, which the native-constructor lane now consults for
    Python classes too, and compiled code's item read stores and returns
    a miss's value where the factory is a common builtin type, as
    `__missing__` does (1,972 and 2,720; `defaultdict(list)` with one
    miss 11,452 to 4,034, against 3,192). `OrderedDict()` of the exact
    class, whose `__init__` does nothing without arguments, builds through
    the same hook (2,983 to 1,636, against 1,049), and so does
    `functools.partial(func, *args)` over a callable that isn't itself a
    partial, laid out as its native `__new__` lays it (4,837 to 2,317,
    against 1,801), and `deque()` and `deque(xs)` of a list or tuple, laid
    out as its `__init__` lays it (3,713 to 1,960 and 5,694 to 2,326,
    against 2,283 and 4,228).
  - `email`'s header parsing ran three of its common steps through the
    full handler on every call. A compiled pattern's `findall` declined a
    str subclass's instance (the parser's tokens), so `_validate_xtext`
    cost 43,662 instructions to CPython's 2,945; `findall` and `split`
    now read the instance as its string and return exact strs, as
    CPython's slices do (2,671). `super().value` of a property built the
    super proxy each time; the getter now runs as an inline activation in
    the core loop and as a direct call from compiled code, its site
    remembering the property for the receiver's class (6,885 to 2,106,
    against 1,201). The core loop's subscript arm had slice cases for
    `bytearray`s and str, list and tuple subclasses that its own type
    check never let through, so `token[:]` took the slow step (2,895 to
    1,718, against 831). Setting a `From` header on a new message went
    from 1,092,000 instructions to 941,000 (CPython 553,000).
  - A getter like `return list(self.defects)` stopped evaluating in place
    once its leaf plan compiled: the compiled plan's call step lacked the
    interpreted plan's builtin-constructor case, so every call declined,
    and 64 declines in a row withdraw a function's leaf verdict for good.
    With the case added, the read costs 1,683 instructions instead of
    1,944 (CPython 1,089). Inside a generator's step, which can't call a
    getter that runs as an activation, the miss had also walked every
    other attribute read before declining; it now declines at once.
    Together, `sum((x.all_defects for x in xs), [])` over four items went
    from 36,700 instructions to 14,800 (CPython 7,700). `sum()` of lists
    extends the list it built after the first item instead of building a
    new one per item, as the intermediate totals are unobservable.
  - `os.walk` stepped its directory listing with `next(scandir_it)`
    through the general call; `next()` now steps a scandir iterator in
    place, and `os.scandir` takes the builtin lane (a warm walk of a
    42-entry tree from 764,000 instructions to 738,000, against 565,000).
    A walk's remaining time is mostly system calls.
  - Only about half of the linker order file's names still matched the
    binary's functions, so start-up on Linux touched 10.5 MB of code (the
    kernel maps 64 KiB around each page a fault touches) and peaked at
    17 MB against CPython's 9. Extending the order with callgrind
    profiles of start-up, import mixes, the bench fixtures and ten
    battery cases (`scripts/extend-order-file.py`) halves the resident
    code: peak RSS goes from 17.1 to 12.0 MB at start-up and from 23.6
    to 17.4 MB after importing `json`, `re` and `collections`, and
    start-up is about 3% faster. On macOS, which faults single pages,
    peak RSS drops 2-4%.
  - `inspect.signature` read a function's `__defaults__`,
    `__kwdefaults__` and `__code__`, and probed the names it lacks
    (`getattr(func, "__signature__", None)`, `hasattr(func,
    "__wrapped__")`), through the full attribute lookup. The fields now
    read in place, and a miss the full lookup found on a function that
    stores nothing under the name is remembered on the function type
    (`inspect.signature` of a one-parameter function from 105,600
    instructions to 87,100; `f.__defaults__, f.__kwdefaults__` from 4,870
    to 2,733, against 1,136).
  - Compiling a regular expression that isn't in the on-disk program
    cache cost about 2.7 million instructions per pattern, against
    CPython's 580,000. Three steps took the general paths every time:
    `int(c)` over the program's named opcode constants (an `int`
    subclass), `charmap[i] = 1` on the charset's `bytearray`, and
    iteration over the parser's `SubPattern`, which has `__getitem__` but
    no `__iter__`, so each item went through the sequence protocol's
    Python iterator and a Python `__getitem__` call. `int()` of an `int`
    subclass that keeps `int`'s `__int__` now answers in place, the core
    loop stores bytes into a `bytearray`, and `SubPattern` iterates its
    list (400 uncached patterns from 1,101 million instructions to 930
    million, against 233 million: the parser's two largest functions
    still run interpreted). A start-up also no longer calls `globals()`
    195 times in `posix`'s re-export loop (0.9% of its instructions).
  - Compiled code read a receiver that isn't a local (`p.x` of a global)
    through a slower helper than a local's: a `__slots__` member, a
    class's plain value through an instance, and a named tuple's field
    were each found only after the helper had tried the other kinds. The
    stack helper now checks the slot the site recorded and, first, the
    class value and field its local twin checks (a loop reading a global
    slotted instance's field from 520 instructions an iteration to 251,
    a class value from 390 to 302, a named tuple field from 699 to 288;
    CPython 282, 278 and 417). A `functools.partial` call found its
    fields by comparing their names on every call; it now remembers the
    slot layout as `deque`'s methods do (1,870 instructions a call to
    1,693, against 939).
  - A `for` loop over a generator first tries the generator's native
    step, which has no switch and so can't call Python code: a body that
    subscripts an object with a Python `__getitem__` (as
    `collections.abc.Sequence.__iter__` does, behind `x in path.parents`)
    stopped partway at every item and finished on the general path. A
    body whose steps keep stopping now skips them, and the loop resumes it
    through its switched path, which calls the method directly (one step
    in 256 retries). A five-item generator of that shape went from 33,800
    instructions to 19,500 (CPython 7,600) and `Path.relative_to` from
    337,000 to 303,000 (169,000); the lazy resume's own cost is most of
    what's left.
  - A struct sequence (`os.stat_result`, `time.struct_time`,
    `sys.version_info`, `os.terminal_size`) keeps its fields in its
    instance dict, but its class answers each name with a property whose
    getter is a native function, so `st.st_mode` was a descriptor call
    through the general attribute path. The attribute caches now treat
    those properties as the instance's own field, the fields' keys are the
    interned names compiled code holds, and the type rejects `del` of a
    field as CPython does ("readonly attribute"). A struct sequence
    compares with a tuple as its own tuple, without a method call. Reads
    went from about 2,100 instructions to 500-800 (CPython 270-390) and
    `sys.version_info >= (3, 8)` from 3,740 to 2,310 (700).
  - `shutil.get_terminal_size()`, which argparse's help formatter calls
    for every formatter it makes, cost 62,600 instructions against
    CPython's 29,500. Three of its steps left compiled code: the
    `os.environ` misses (`_Environ.__getitem__` subscripts a dict keyed by
    `bytes`, whose missing key only the interpreter raised), the
    `OSError` from `os.get_terminal_size()` (a builtin the core loop
    didn't call in place), and `os.terminal_size(fallback)`, which went
    through the general class call into the type's `__init__`. A missing
    `bytes` key now raises where the code catches it, the call runs in the
    core loop's lane, and a struct sequence built from a tuple or list of
    plain values is made directly (6,000 instructions to 3,640, against
    1,930). The call as a whole now costs 49,300.
  - Releasing containers. A dying dict dropped each key and value through
    a call to its drop glue, even an `int` key, and a list did the same for
    each item unless all of them were scalars; a tuple from the free list
    was refilled through checks a parked tuple can't fail. A dict now
    gives each entry up in line (a plain decrement for a value someone
    else still holds), a list does the same for items it doesn't own last
    (a dying one still goes through the nested release), and a pooled
    tuple's slots are written directly. `d.copy()` of a 100-entry dict
    with its release went from 8,270 instructions to 5,010 (CPython
    4,400), `dict(d)` of 20 entries from 3,080 to 2,500 (1,910),
    `list(t)` of 100 strings from 5,810 to 3,760 (4,490), and a loop over
    `enumerate` of ten items from 6,400 to 5,760 (4,120).
  - Dunder reads that took the full attribute lookup. A never-raised
    exception's `__traceback__`, `__cause__` and `__context__` are unset
    slots, which the slot reads in compiled code and the interpreter
    declined, so each read cost about 4,000 instructions (CPython 200);
    they now answer the slot descriptor's default. A bound method's
    `__self__`, `__func__`, `__name__`, `__qualname__` and `__doc__` read
    in place (1,690 instructions to 490 compiled, 1,750 to 615
    interpreted, against 530), and `cls.__qualname__` joins the per-site
    memo `__name__` has, now that assigning it moves the class's version
    (1,380 to 230, against 420; interpreted, the core loop reads a class's
    names directly, 1,700 to 1,080). `cls.__mro__` and `__bases__` still
    build a tuple per read (3.4 and 4 times CPython's cost) because
    keeping one on the class needs the collector to see it.
  - A class now keeps the tuples `__mro__` and `__bases__` read as, while
    its links stay the same (the collector visits and clears them with the
    MRO), so `C.__mro__ is C.__mro__` as in CPython and a read no longer
    allocates (1,480 instructions to 1,090 compiled, 1,100 to 760
    interpreted).
  - Native code calling back into Python. `max(xs, key=attrgetter("x"))`
    called the getter through the general call, a bound method for each
    item, where `sorted` already evaluated such keys in place; `min` and
    `max` now do too (25,750 instructions for ten items to 8,960, against
    5,350; `itemgetter` 18,500 to 4,870, against 3,970). A dict or set
    store of a key whose `__hash__` is Python (an `Enum` member, a frozen
    dataclass) left compiled code at every store, and every hash ran a
    reentrant call; a `__hash__` that evaluates as a pure leaf now runs in
    place, and compiled code stores the key with that hash when it meets
    the entry by identity or the slot is free (`x[Color.RED] = v` 3,780
    to 1,500, against 1,230; a ten-entry dict comprehension of such keys
    35,900 to 16,700, against 10,300).
  - `"%r" % x` sent every `%r` through the general conversion, which
    formats through a spec with alignment even for a bare `int`; an `int`,
    `bool`, `None` or `str` now renders in place (`"P(%r)" % 5` from 2,820
    instructions to 1,140, against 1,730). A container's repr found each
    item's Python `__repr__` by walking the class's MRO and called it as a
    bound method; it now calls the memoized function directly, and a
    pure-leaf `__hash__` is tried before the native kinds' checks
    (`repr(p)` of an instance whose `__repr__` is `"P(%r)" % self.x` from
    6,230 to 4,500, against 2,390; `set(xs)` of ten such keys from 22,500
    to 19,700, against 8,800).
  - `functools.cmp_to_key(f)` built a new class for every call (the
    pure-Python fallback's, which the native function forwarded to), so
    `sorted(xs, key=cmp_to_key(f))` of ten items cost 253,000 instructions
    against CPython's 18,000. It now returns a key factory of one shared
    class. And `self._cmp(a, b)` in that class's methods, a call of a
    function a `__slots__` member holds, left compiled code at every call:
    the method load now reads a laid-out slot member and remembers its
    position. The sort now costs 88,000 instructions; CPython's C key
    wrapper makes no Python calls but `f`'s, which the rest of the gap
    is.
  - Native modules that call back into Python (`bisect`'s `key`, about a
    hundred other call sites) went through `Interpreter::call_object`,
    the embedder's entry, which publishes the interpreter and activates
    its thread handles (cloning four of them) for every call. From a frame
    of the same interpreter both are in effect already, so the call goes
    straight on (`bisect_left` with a `key` over a 34-item list from
    8,090 instructions to 6,310, against 4,050). Every comparison of two
    instances also paid 28 instructions to rule out a struct sequence;
    only instances of builtin classes check now.
  - A memory leak: every `threading.Thread` (each makes a
    `_thread._ThreadHandle`) left its handle's state, a lock among it, in
    a process-wide registry that nothing ever removed from, so a program
    that keeps starting threads grew without bound and paid an ever larger
    map's inserts. The registry now holds the states weakly (the handle's
    methods keep them alive) and sweeps dead entries when it doubles.
  - Iterator classes written in Python. `next(it)` of one went from the
    compiled call site through the builtin's full call path to a bound
    `__next__` call, about 900 instructions more than a `for` loop's step
    of the same iterator; it now steps a frameless `__next__` as `FOR_ITER`
    does (`self.i += 1; return self.i`: 2,046 instructions to 1,574,
    against 1,124). And a bare `raise StopIteration` that ends a
    frameless `__next__` built and unwound the exception through a full
    call: when a `for` loop, `list()` or `next(it, default)` is the
    catcher and nothing could see it (no store buffered, no tracer), the
    step now reports the end itself. Ending a loop over a used-up
    iterator went from 7,600 instructions to 1,700 (CPython 3,240), a
    20-item loop over a fresh one from 1,424 an item to 1,082 (1,080),
    and `list()` of it from 1,635 to 1,332 (1,107).
  - The frozen `itertools` classes' `__next__` (`return
    next(self._core)`) is now native, so `c.__next__()` of a `count`, as
    `threading` names its threads, costs 850 instructions instead of
    2,783 (CPython 554). Their `__iter__` returned the native core rather
    than the object, so a subclass's own `__next__` was skipped by `for`
    loops and `islice`; it returns the object now, as CPython's do.
  - `reversed(s)` of a string took the general type call, which built
    a new native constructor object for the call and released it again
    (taking the descriptor registry's write lock on the way out, though
    its comment promised a read probe), and copied the characters one
    `chars()` step at a time. Its pure constructor now admits `str` and
    `bytes`, the type-call path reuses one constructor per type, the
    registry's release probes under the read lock first, and an ASCII
    string's characters are copied by index: `reversed(s)` from 4,158
    instructions to 1,557 (CPython 907), `''.join(reversed(s))` from
    7,547 to 4,984 (4,340).
  - An `accumulate` without a function ended each loop through the
    general step (its pure step declined at the end): it now ends in
    place, as `zip`'s does (`for` over five items 5,297 to 4,155 against
    2,600), and so does a `pairwise`. A compiled `list(x)`, `tuple(x)`,
    `set(x)`, `dict(x)` or `str(x)` whose pure constructor declined (`x`
    a `map`, a generator) tried the same attempt again and three more
    before reaching the container lane; it goes there directly now, about
    380 instructions a call (`list(accumulate(xs))` 6,837 to 5,643
    against 3,147, `list(pairwise(xs))` 8,105 to 6,837 against 3,760).
  - `yield from xs` of a list in compiled code left at
    `GET_YIELD_FROM_ITER` and at the delegation's end in every generator:
    a three-item one cost 12,515 instructions against 2,303. Compiled code
    now takes a builtin sequence's iterator in place and steps a list's or
    tuple's cursor without leaving (5,734, the same as the equivalent
    `for` loop of yields), and a class whose `__iter__` is such a
    generator iterates in 6,299 instructions instead of 13,055 (2,538).
  - Keyword calls of the `itertools` classes (`product(xs, repeat=2)`,
    `accumulate(xs, initial=0)`) left compiled code at `CALL_KW` and bound
    their keywords through the general call, about 2,700 instructions:
    the adapter is now built in place (5,945 to 3,074, CPython 3,354).
    The combinatoric adapters also build each tuple straight from its
    items, without an intermediate vector (`list(product(xs, xs))` 12,046
    to 9,524).
  - `iter(obj)` of a class with a Python `__iter__` looked the method up
    twice uncached and called it as a bound method; it now calls the
    cached function directly, as `for` loops do (3,159 instructions to
    2,273, against 1,284). `struct.unpack` was a Python wrapper over the
    native one; like `pack`, it now reads a `bytes` or `bytearray` buffer
    natively and keeps the wrapper for other buffers and errors (2,088
    to 1,229, against 1,061).
  - A dict iterator under `zip` or `enumerate` (`heapq.nlargest(n,
    d.items())`, as `Counter.most_common` calls it) took the general
    step for every item, since only sequence cursors counted as ready to
    step in place. A dict iterator past its first, checked, step now
    counts too while its dict is unchanged (`zip(range(3), it)` over a
    dict's items 15,122 instructions to 12,227, `most_common(3)` 38,153
    to 33,766, against 18,348).
  - `isinstance(x, C)` for an ABC `C` asked, on every call, whether the
    metaclass keeps `type`'s `__instancecheck__`, by a string lookup
    through its MRO; the answer is now remembered per metaclass version
    (`isinstance(o, Base)` 1,238 instructions to 1,009, against 2,179;
    `issubclass(Impl, Base)` 2,442 to 1,922, against 2,052). A
    class-like instance's plain Python `__instancecheck__` or
    `__subclasscheck__` (the `typing` aliases') is called directly rather
    than as a fresh bound method (`isinstance(i, typing.Hashable)` 8,130
    to 7,753, against 4,649; the rest is its two Python calls).
  - A compiled `obj.attr = value` stored in line only over a scalar; one
    that replaced an object (`node.next = other`, `self.current = node`)
    took a helper, about 124 instructions more, even when another owner
    kept the old value. The in-line store now gives the old value up by a
    plain decrement when it isn't the last reference. A site that first
    added its attribute (a loop's first pass) settled on appending and
    took the helper for every later pass's replacement; it now replaces
    in line too. Replacing an object in 100 instances went from 30,957
    instructions to 17,297 (CPython 17,669), and a second pass's stores
    from 54,605 to 29,666 (29,001).
  - Fixed: the core loop's instance attribute stores left a finalizer the
    replaced value queued for the next safe point, so `holder.d =
    Dying()` ran the old object's `__del__` one statement late (at the
    next call) in every tier.
  - `dict(x)` from compiled code reached the general type call after the
    container lane declined it; the lane builds it now (a mapping, a
    lazy iterable of pairs, or an eager container).
  - A function's first call decides whether it is a frameless leaf; the
    decision allocated four vectors to walk the body, about 1,000
    instructions of a first call's 6,000 (CPython's first call costs about
    150). It walks the body (at most 64 instructions) in stack buffers
    now, and only generator-shaped code scans its body for a generator
    start: a first call of a fresh function went from 7,937 instructions
    to 6,611. The rest is building the code's tables and the first pass's
    site caches, which CPython does when it quickens the code.
  - Fixed: the frozen standard library's code cache named its artifacts by
    module alone, so two builds whose embedded sources differ (a merge
    base and a PR in the bench gate, a developer's branches) recompiled
    and replaced each other's artifacts at every start, about 14 million
    instructions a run (it also made whole-process A/B counts of two
    builds misleading). Artifacts are now named by module and source
    hash, and a write removes the module's day-old artifacts for other
    sources.
  - Stepping an instance whose class has a native `__next__` (an
    `io.StringIO` under `csv.reader`, a `for` loop over one) looked the
    method up by name on every step; it is found once per class version
    now. The CSV reader also updates its per-row state (`line_num` and
    its re-entry flag) in place rather than re-inserting it: reading 200
    rows went from 1,568,659 instructions to 1,482,021 (CPython 912,416).
  - Fixed: a `StopIteration` thrown into a generator that never started
    was converted to `RuntimeError`; CPython's PEP 479 conversion covers
    only the body past its start, so it escapes as it is there.
  - `setattr(obj, name, value)` of an attribute already there interned
    and hashed the name and stored it through a dictionary insert; it now
    replaces the value at the position it found (1,193 instructions to
    891, against 726). Fixed alongside: when the replaced value was its
    last reference, its `__del__` ran one statement late; that case now
    takes the full path, which runs it at once.
  - `format(x, 'g')` (and `%g`, the type-omitted form with a precision)
    copied the digits through four strings; it lays them out from the one
    scientific formatting and strips zeros in place (3,850 instructions to
    3,283, against 2,416). A test pins the output for 6,000 values and 15
    specs to CPython's.
  - A compiled `len(obj)` site whose leaf half declined an instance went
    through the builtin's lane to reach a frameless `__len__`; it
    evaluates it in place now (1,670 instructions to 1,245, against
    1,004). Operator and subscript dunder calls enter a warm leaf's
    compiled plan rather than its interpreted one.
  - The bench gate's A/B legs ran in a fixed order, PR, base, CPython,
    so every PR sample followed another program's run and every base
    sample a near-identical one's; a PR that changed no code could fail
    it, and this PR's Linux gate flagged `fib` and `richards` three runs
    in a row while both builds executed the same instruction counts with
    the same simulated cache misses and branch mispredictions (the PR's
    slightly fewer). The legs now run in a balanced order (PR, base,
    base, PR, then the mirror), two samples each a round.
  - Where the everyday battery's worst cases stand, measured on
    `argparse_ops` (2.1 times CPython's instructions): its 60 iterations
    are 85% warm-up. In steady state an iteration costs 2.1M
    instructions against CPython's 1.5M (1.37 times); iterations 2 to 60
    average 4.8M against 1.7M. Interpreted, an iteration costs 2.9M (the
    core loop averages 39 instructions a bytecode: its pc and
    instruction pointer live on the stack, so dispatch alone is about
    ten), and through iteration 60 the JITs add about 1M an iteration
    more than they save: tier 2's analysis of code it then rejects is
    about 8% of the case, the frame JIT's compiles 2%. `import argparse`
    costs 30M to CPython's 11M, spread over decoding cached code,
    interning its names and running module bodies, with nothing over
    3%.
  - A sweep of 198 more single statements (string, `re`, dict, list,
    call, `os.path`, `pathlib`, `datetime` and small standard-library
    operations) reads a geomean of 0.95 times CPython's instructions.
    From its tail: tier 2's generic `BINARY_OP` helper always took the
    interpreter's full operator dispatch, where compiled frames have
    direct helpers; it now builds two lists' or tuples' concatenation, two
    dicts' union, sets' algebra and `bytes` concatenation off its
    borrowed operands (`xs + [y]` in a loop 2,992 instructions to 2,251,
    against 1,461; `d | e` 3,261 to 2,340, against 1,727). `len()` of a
    range joined the builtin's fast half (2,026 to 1,053, against 980),
    and `dict(mapping, **kw)` copies the mapping once and interns its
    keyword names (4,329 to 3,712, against 1,560; the keyword call's own
    route is the rest).
  - Fixed: compiled code's check that an attribute certainly misses (ahead
    of calling `__getattr__`) probed the instance dictionary with a
    comparison that runs a colliding key's Python `__eq__`, from a
    frameless activation: the `__eq__` saw its caller's caller in
    `sys._getframe`, and ran four times a lookup instead of once. The
    probe now runs no Python and leaves such a dictionary to the full
    lookup. This was the bug that a stricter generic-share limit for tier
    2 had exposed; that limit still isn't adopted, since it now costs
    the `nbody` fixture 38% (tier 2 runs its boxed float arithmetic
    faster than the frame JIT does).
  - Tier 2's analysis runs on the thread that runs the program, and a
    trace of `argparse` building and running a parser 180 times
    (`WEAVEPY_JIT_TRACE=1`, about 200 ms in all) showed 44 ms of it
    there: 104 functions analyzed to compile 12 trivial ones. Three
    things cut that to about 20 ms (5% of the battery case's
    instructions, with the bench fixtures unchanged):
    - A method site that doesn't resolve types the method's body, a
      whole analysis of its own, and the answer wasn't kept: the analyzer
      asked again on every pass over every such site (12 analyses of one
      callee in a 22-instruction function). An attempt now remembers
      the sites that didn't resolve, as it did those that did (44 ms to
      32).
    - An analysis starts over for each loop it carves out, each retry
      with other assumptions and each callee it types, and nothing
      bounded the product: `_parse_known_args2`, 169 instructions, took
      160 passes an attempt, twice. Each attempt now has a work budget,
      counted in instructions passed over: 1,024 units and eight passes
      over the code itself, plus what the code's own calls and loop
      iterations have paid for (one unit for every eight). The costliest
      kernel among the bench fixtures uses half its allowance. An attempt
      that runs out stands as a rejection, so the frame JIT takes the
      code at once; code whose calls run long loops (16 iterations a
      call) waits instead, interpreted, until its work pays for twice the
      budget. `WEAVEPY_T2_FREE`, `WEAVEPY_T2_PASSES` and `WEAVEPY_T2_WPP`
      set the three numbers.
    - The plan's set of jump targets is a bitmap instead of a hash set
      built on every pass (the hottest line of the analysis; 1.2% of the
      case).
    What's left is mostly the cost of running the analyzer cold: about
    150 microseconds an attempt, most of it instruction-cache misses in
    code that last ran thousands of calls ago, on 100 attempts that
    reject. Cheaper rates were tried and cost the short numeric fixtures
    too much (work alone paying for analysis: `spectral_norm` 2.1 times
    slower, `nbody` 1.7), as did a budget summed over a function's
    attempts (`difflib`'s `find_longest_match` compiles on its second
    loop entry, after a first that fails).
  - A field of a natively served value (`dt.hour`) has no site memo, so
    every read takes the compiled site's slow path, where two lanes
    added on this branch (a bound native method's memo, a property's
    cached getter) had gone ahead of it: the `datetime_ops` fixture ran
    4.7% more instructions than the merge base on Linux. The native
    field is read first again (352.5M instructions to 340M).
  - Fixed: the `generators` fixture ran either 667M or 763M instructions,
    run to run, on any build (and always the higher with synchronous
    compiles), which the bench gate read as a regression one run in two.
    `sum(gen)` drains a generator's whole body in one step; the body gets
    hot and compiles during that step, and its native code needs more
    value-stack room than the step had sized the stack for. Every native
    entry was then turned away, and after 32 at one instruction the entry
    was given up for the rest of the run; how many entries were lost
    depended on where the step's periodic check happened to end it. The
    step now grows the stack when the native code arrives, and an entry a
    small stack turned away no longer counts against the entry (680M in
    every run of a quick build, 673M synchronous).
  - A compiled attribute read keeps two classes' field positions in line
    and up to four more in the site's table, but the table was consulted
    last: a third class's read went through the whole miss chain (the
    property, class-attribute and descriptor lanes, then the table, then
    an attempt to memoize the value as a class attribute, which a field
    never is) at about 1,000 instructions. `argparse` reads `action.dest`
    and its neighbors over three action classes: 120 such reads an
    iteration, 7% of its steady state. The table is now the first thing
    the slow path tries for an instance (an entry is for a class version
    at which the name resolved to the instance, so nothing ahead of it
    would answer otherwise): 1.908M instructions an iteration to 1.796M.
  - `getattr(cls, name[, default])`, which `inspect`, `typing`, `functools`
    and `dataclasses` call on every object they handle, cost three to
    four times CPython's instructions whenever the answer wasn't a plain
    hit: a missing name with a default (`getattr(cls, "__wrapped__",
    None)`) 1,761 against 579, `getattr(cls, "__name__")` 2,444 against
    558. Each call classified the name against two lists of dunders by
    comparing strings (some forty comparisons), asked `type` itself for
    the name, walked the class's MRO three more times, and for
    `__name__` gave up and took the full call. Now the two lists are one
    hashed table, `type`'s answer is remembered by name, `__name__` and
    `__qualname__` are read where they live, and the whole answer is
    kept for a class under plain `type` by its attribute version and the
    name object. The memo owns only scalars and builtin functions; any
    other value it borrows from the Python class whose dictionary holds
    it, so a value a class drops dies when it does. All six shapes
    measured now cost 610 to 630 instructions (CPython 560 to 600); in
    the `inspect_typing_ops` case this helper was 6% of the instructions.
  - Programs with a second thread. The battery's slowest case on a
    profile-guided build is `threading_queue_ops` (2.6 times CPython's
    time): a thread started and joined costs 180 microseconds against
    60, and an item handed to a worker through a `queue.Queue` 20,900
    instructions against 14,200. In the hand-off, `Condition.__exit__`'s
    `self._lock.__exit__(*args)` read a native method off a temporary
    and found the site's memo, then threw the answer away: the receiver
    could only leave by a plain decrement, which is never allowed once
    reference counts are shared between threads, so each read went on
    through the whole miss chain (1,750 instructions, twice an item).
    The read now releases the receiver as the core loop releases any
    operand when the plain decrement doesn't apply: 22,200 instructions
    an item to 19,300.
  - What a second thread costs everything else, measured by running
    single-threaded battery cases with one idle thread alive: 13% to 55%
    more cycles (`list_dict_basics` 13%, `collections_ops` 26%,
    `class_features` 34%, `enum_dataclass_ops` 55%), where CPython's
    don't move. Keeping reference counts plain in an experiment took
    back four fifths of it, the borrow counters' bias the rest: counts
    go atomic for every thread as soon as a second one registers (a
    locked increment or decrement costs about twenty cycles on x86), and
    compiled code's in-line releases and clones, which test that flag,
    call their helpers instead (5% to 20% more instructions). Under the
    GIL the counts could stay plain if no thread ever touched one
    without holding it; that isn't established today (118 blocks run
    with the GIL released, and a thread's start and its thread-local
    state's teardown run outside it), so it is the next piece of work
    of its size rather than a change to make in passing.
  - First steps toward plain counts with several threads. An audit build
    (a check, in the shared-count paths, that the updating thread holds
    the GIL) run over the thread tests found no update outside the GIL
    while a program runs, and two places at its edges. The main thread
    gave the GIL back when the main module returned and ran
    `threading._shutdown`, the exit handlers and the finalizers without
    it, beside daemon threads that still held it: it now holds the GIL
    from the main module through shutdown, as CPython's finalization
    does (a join inside gives it up while it waits). And a thread's
    thread-local caches were dropped by its destructors after it had
    given the GIL back: its entry in the traceback dumper's registry
    (its frame and exception stacks) and its interned names are now
    released by the worker while it still holds the GIL, and the
    docstring cache, the static-string table, the empty tuple and the
    `getattr` memo take the GIL to drop. The sixty-odd other
    thread-local caches that hold objects or strings (class handles,
    key tables, registries, memos) now take it too, and the command
    line's run holds the GIL from before the interpreter is built until
    it is released. With that, the audit over 24 groups of CPython's
    tests (threads, queues, futures, asyncio, sockets, subprocesses,
    multiprocessing, the C API) records no update outside the GIL by
    any thread while it lives, and none at a worker's exit; what is
    left is the main thread's last steps after the run (reading the
    exit status off the exception, its own destructors at process exit)
    and a fatal-error dump from a foreign thread. Still to do before
    counts can stay plain with several threads: those last steps, the
    same audit on Linux and Windows (their blocking calls differ), a
    flag of its own for "other threads exist" (the shared-counts flag
    stands in for it in two places), and a test job that runs the audit.
  - Reference counts stay plain with several threads. Under the GIL a
    count needs no atomic update as long as every thread that touches an
    object holds the GIL while it does, and that now holds by
    construction: a worker takes the GIL before its first touch of an
    object and keeps it on its guard stack through its thread-local
    destructors (the stack is the first thread-local the thread uses,
    so the last one destroyed), and no block that runs with the GIL
    released builds or drops an object (all 118 were read; one, a
    blocking `channel_send`, built its exceptions and dropped a
    withdrawn item inside the block, and now hands the outcome back
    out). Counts turn atomic, under the GIL, only when something will
    touch one outside it: free-threading, a thread the VM didn't start,
    the traceback watchdog, or a thread whose outermost GIL scope ends
    while others still run (an embedder reading a result, the command
    line's last steps beside daemon threads). Windows keeps atomic
    counts until its blocking paths get the same audit, and
    `WEAVEPY_ATOMIC_COUNTS=1` restores them anywhere. "Other threads
    exist" is a flag of its own now, for the three places that asked
    the shared-counts flag that question (frames made whole before a
    blocking call, retired code objects, pickle's pins). An audit
    build that checks the GIL at every count update ran over 36 groups
    of CPython's tests and the bundled thread tests with no plain
    update outside the GIL, on macOS and on Linux. On a release build
    one idle thread now costs the four battery cases measured before 3%
    to 8% more cycles (`list_dict_basics` 3%, `enum_dataclass_ops` 4%,
    `class_features` 5%, `collections_ops` 8%) where atomic counts
    cost 12% to 30%, and an item through a `queue.Queue` with three
    consumers 19,300 instructions to 16,800 (CPython 14,200). What is
    left is the cell borrows' lock word, which a second thread still
    turns on.
  - A thread back from a blocking call waited two switch intervals for
    a busy thread, and `sys.setswitchinterval` changed neither. The
    hand-off wants both clocks run out, the holder's turn and the
    waiter's wait, but the waiter's only started at the first
    checkpoint after the holder's had expired: 10 ms where CPython
    waits about 6. The two now run side by side, and the interval a
    program sets is the one the checkpoints use: 5.2 ms at the default,
    0.2 ms at 10 microseconds (CPython 6.2 and 0.18). A test that
    sleeps beside two busy threads took 21.5 s and takes 0.23 s
    (CPython 0.29 s).
  - A thread's interpreter shares its parent's table of leaf builtins
    instead of copying it (two hash maps and a vector of handles built
    at each start and dropped at each exit, a twentieth of a start and
    join). Starting and joining a thread: 522,000 instructions to
    463,000 with the change above (CPython 192,000, kernel included;
    in user space the cost is spread over the thread module's Python,
    a fresh allocator heap and interpreter for each thread, and their
    teardown).
  - Cell borrows stay lock-free with several threads too. A borrow took
    its cell's lock word (a compare-exchange, a thread-local read and a
    release store) from the moment a second thread registered: 93,000
    locked instructions a call of `collections_ops` with an idle thread
    alive, all of them these. Under the GIL the lock word orders nothing
    the GIL doesn't, and the borrow counter alone keeps a shared and an
    exclusive borrow apart, across threads as within one, so borrows
    now take the lock only once some thread may touch a cell without
    the GIL (the cases that turn counts atomic). The count of live
    lock-free guards, which decides whether a thread may hand the GIL
    over at a checkpoint, is the GIL holder's: a thread's own leave it
    when it really gives the GIL up and return with it. The lock word
    was also a deadlock: a thread blocked inside a set's membership
    test (its `__eq__` sleeping, writing or taking a lock) held the
    set's word, and another thread adding to the set spun on it with
    the GIL held, forever. Now the add waits for the first thread with
    the GIL released and then happens (CPython lets it through at once;
    the deferred-operation path that covers a nested mutation would
    have dropped it). The audit build, which now checks the GIL at
    every lock-free borrow as well, found none outside it over 45
    groups of CPython's tests on macOS and the thread modules on Linux.
    One idle thread no longer shows in the four cases: 0% to 0.6% more
    instructions, cycles within the 2% the runs spread over (3% to 8%
    more cycles before this, 13% to 55% two days ago).
  - A lock's waiter is handed the lock. Past five milliseconds a
    contended `Lock` or `RLock` acquire waited on the lock itself, GIL
    released, and got it only by waking in the gap between the
    holder's release and its next acquire. That gap is a few bytecodes,
    and it shrank as threaded code got faster: the first acquire
    against a holder that retakes the lock at once went from 10 to 40
    ms to 80 to 450 ms, and the hand-off test started to fail one run
    in three. A release now keeps the lock for a waiter that is past
    its turn (nobody else can take it meanwhile), as CPython's lock
    hands itself to a waiter of over a millisecond, and the waiter goes
    from its grace period to that wait without coming back for the GIL:
    5 ms for that first acquire (CPython 6 to 8).
  - A method that adds to a `__slots__` member runs in line. Tier 2 ran
    `self.a += 1; return self.a` in line only when `a` was an instance
    dictionary field; for a slot member each call went through the
    method helper (538 instructions against 128). The in-line form now
    covers a member slot of a class whose instances have no dictionary:
    126 instructions, a slotted `tick()` loop 4.2 times as fast.
  - Tier 2 keeps loops whose attribute traffic it handles in line. Its
    admission rule left any loop with more attribute operations than
    typed ones to the frame JIT, on the grounds that each attribute
    access costs tier 2 a helper call. Scalar fields of split-layout
    instances and laid-out slot members have been read and written in
    line for a while, so those accesses no longer count: the
    `attr_access` fixture's loop runs 1,183 instructions an iteration
    in tier 2 against 1,333 in the frame JIT (362 cycles against about
    400; CPython 3,329 and 1,115), the fixture 0.885 of its
    instructions. Both figures are still a hundred instructions an
    attribute operation: each access redoes its receiver's guard chain
    (about 60 instructions), and the two string field reads go through
    a helper (172 each); the next entry takes those on.
  - Tier 2's attribute accesses check less each time. Three cuts in
    the in-line path, measured on the `attr_access` fixture's loop (two
    scalar reads, two float fields read and rewritten, two `tick()`
    updates, two string fields compared):
    a string, bytes or object field that still holds the value its site
    read last finds that value's pin again in line (the helper's own
    first answer, 172 instructions each time before): 1,182 to 972
    instructions an iteration;
    an instance validated by one access of a local's field (an instance
    pin with no published dictionary, its values split over its class's
    names) stays validated for the accesses of that local that follow
    in the block, through in-line stores and field updates too, until
    something that can run code or reshape it (a helper's store, any
    other call, the local's rebinding): 972 to 874, a field read then
    written 161 to 118;
    and a site's class version and index are constants in its code
    instead of loads from the guard table (a guard never changes once
    made): 874 to 786. Cycles went 348 to 228 an iteration; the frame
    JIT ran this loop at 1,333 instructions and about 400 cycles, and
    CPython runs it at 3,329 and 1,115.
  - Integers past 64 bits. The battery's `uuid_secrets_ops` (1.6 times
    CPython's time) spends its time making and reading 128-bit values,
    and each step cost two to four times CPython's:
    `int(text, 16)` of 32 digits took 7,300 instructions, because the
    constructor's quick arms try a machine-int parse, decline on its
    overflow, and the full constructor's layers each try it again
    before the big value is built; the arms now build it themselves
    (2,800, 602 ns to 218; CPython 160 ns), as they do for long decimal
    text (456 ns to 207; CPython 143). Hex text is then tried before the
    general parse and read through a digit table (218 ns to 124).
    Arithmetic of a big value with a machine int promoted both sides,
    copying the big one: sums, differences, products, right shifts and
    masks now read it in place (`big + 1` 977 instructions to 664).
    `to_bytes` built a byte-order string and a copy of the value before
    looking at its arguments; a non-negative value now goes straight
    into the result, at any length. And a method called on a big
    `int`, a float, a tuple or a `bytes` object went through the method
    memo at every call from compiled code, whose per-site cache held
    lists, dicts, sets and strs only: the site now keeps the method for
    the receiver's kind, as it did for streams
    (`big.bit_length()` 804 instructions to 589).
  - Removing a key from a dict cost time in proportion to the keys behind
    it. The table keeps positions dense (the i-th key is entry i, which
    the iterators and the position caches rely on), so a removal moved
    every later entry down and renumbered the slot table: a cache that
    evicts its oldest key, a table of pending requests, an LRU touch
    (`v = d.pop(k); d[k] = v`), `OrderedDict.popitem(last=False)` and
    `move_to_end` were all quadratic. At 20,000 live keys, in ns an
    operation (CPython's in brackets): `OrderedDict` LRU 77,900 (195),
    `move_to_end` 87,100 (87), pop and re-insert 41,000 (155), add and pop
    the oldest 40,000 (107), a `WeakValueDictionary` window 45,600 (829).
    The removals a program makes in bulk (`del d[k]`, `d.pop(k)`, an
    `OrderedDict`'s, a dead weak value's) now leave a tombstone in the
    entry list, and positions stay dense through a rank index over the
    tombstones: a bitmap, a Fenwick tree over it that is built only once
    a tombstone sits past the front run (a queue's tombstones are all at
    the front, where a position is its entry less the run), and the last
    translation, which a walk by position continues from. The table
    closes the gaps in one pass when its entry list fills or tombstones
    outnumber keys, so each removal pays a constant share. Removals
    elsewhere (a namespace's `del`, a class's attribute) still shift, so
    a table that never takes a bulk removal has no tombstone and pays one
    test per positional access; the bench fixtures read 1.000 of their
    instructions. The same operations now: 378, 158, 143, 129 and 4,560
    ns, flat from 1,000 keys to 20,000 (`dict` FIFO by `next(iter(d))`
    282 ns, where CPython's own scan past its tombstones takes 6,400).
    The index's pointer first took the table from 48 to 56 bytes, which
    cost 16 bytes a dict in the allocator's size classes (a million
    two-key dicts 262 MB to 277 MB). The slot table is now one word, a
    pointer whose length is the shift's (compiled code tests the shift
    for a table without slots, where it tested the length), so a table
    is 48 bytes again and a dict takes what it took (262 MB); the
    fixtures read 0.997 of their instructions, a probe's mask coming
    from the shift it already loads.
  - What is left on those loops is the call, not the dict. Tier 2 ran a
    method call on a builtin value as two generic helpers: a load that
    allocated a bound method and pinned it for the rest of the
    activation, and a call that looked the builtin's kind up again
    (`d[i] = i; d.pop(i - 1000)` 2,088 instructions an iteration against
    the frame JIT's 1,247 and CPython's 1,248, and 169 ns against 108).
    A builtin value's method table never changes and its lookup runs no
    code, so for a method the core loop's leaf lane runs in place the
    load now leaves the receiver where the bound method would go and the
    call runs the builtin on it; an exit between the two rebuilds the
    bound method the interpreter expects. 1,384 instructions (119 to 129
    ns). The frame JIT still runs such a loop in 1,118: the two helper
    calls cost about 270 more than its method site, and a loop of
    container calls has no typed work for tier 2 to win that back with.
    So tier 2 now leaves a loop to the frame JIT when it makes such a
    call for fewer than four typed operations (`WEAVEPY_T2_METHODS`): the
    pop loop runs in 1,102 instructions, under CPython's 1,248, and pop
    and re-insert went 138 ns to 92 (CPython 130), a queue worker's loop
    4% faster, with the suite's loops that mix such calls with typed
    work (`chaos`, `go`, `raytrace`) within 1% either way.
    `OrderedDict` calls were the next share: the frame JIT tried its
    fused call (receiver, simple arguments, call in one helper) on every
    method call of a local, and a call with a keyword or a computed
    argument walked the instructions, declined, tried the native method
    a second time, and then ran through the method site anyway, about
    450 instructions each. Compiled code now makes the attempt only
    where the arguments can be what it serves, the second try is gone,
    and the fused arguments fold `%`, `//` and `>>` of two ints as they
    did `+`, `-`, `*` and the bit operations (`od.move_to_end(i % n)`
    1,594 instructions to 1,288, CPython 938; add and
    `popitem(last=False)` 3,515 to 3,039, CPython 2,201; with a plain
    key `move_to_end` is 1,050 against 941 and a store 824 against
    1,120). A keyword call of a native method (`popitem(last=False)`)
    also built a vector of cloned arguments for the method's leaf half,
    which now reads them in place (the add-and-pop loop 313 ns to 272;
    CPython 190). And `set.discard` and `set.remove` probed the set
    once to see whether the call could run without Python and then ran
    the general body, which probes again inside a comparison scope and
    replays the deferred-operation queue: a `str`, `int` or `float` key
    that one native probe settles is now removed by that probe
    (`s.discard(k); s.add(k)` 2,252 instructions to 1,348, 197 ns to
    111; CPython 1,530 and 122). Weak-value dictionaries (22,400
    instructions an insertion and expiry against CPython's 7,900, spread
    over the weak reference class's Python-level `__new__`, `__init__`
    and callback) are what is left of this group.
  - A scaling sweep (about 110 container and string operations timed at
    n and 8n keys under both interpreters, flagging a cost per operation
    that grows where CPython's doesn't) found more of the dict removal's
    kind, each an everyday idiom:
    Set operations copied their operands. A comparison (`a <= b`,
    `a == b`) took a snapshot of both sets first, so sets of different
    sizes took time in proportion to the larger to be told apart; the
    methods (`intersection`, `difference`, `issubset`, `issuperset`,
    the three `_update`s) built a new set from each operand, hashing
    every key again; and `a -= b`, `a &= b` and `a ^= b` computed
    `a - b` whole and copied it back into `a`. At 32,000 keys against
    three: `small <= big` 19.9 µs (CPython 57 ns), `big -= small` 1.56
    ms (243 ns), `big.intersection(small)` 1.46 ms (244 ns),
    `small.issubset(big)` 875 µs (103 ns), `big == other` of another
    size 37 µs (36 ns). The algebra now runs on the tables themselves
    (`setalg`): a stored key carries its hash, so nothing is hashed
    again and no `__hash__` runs; a pair of keys that needs a Python
    `__eq__` defers, which leaves the operation to the old path with
    nothing changed; and each operation walks the side it has to, the
    smaller one for an intersection, a subset test or a disjointness
    test, the right one for an in-place difference (planned with both
    sets only read, then applied by position). The same figures: 83 ns,
    331 ns, 162 ns, 111 ns and 61 ns, on a quick build. The method
    forms now keep the elements CPython keeps where equal keys differ
    (`{1, 2}.intersection({1.0, 2.0})` is `{1.0, 2.0}`, as the operator
    already gave).
    A dict's keys view did the same against a set: `small & d.keys()`
    built the 32,000 keys as a set first (1.18 ms; CPython 242 ns; now
    456 ns), as did `d.keys() >= small` and `isdisjoint`; they probe the
    dict's table with the set's stored hashes. `small - d.keys()`, which
    CPython answers by walking the dict (375 µs), takes 416 ns.
    A string built by `s += piece` in a loop copied itself at every
    append once the loop was compiled: the core loop grew a `str` local
    in place when nothing else held it, but the compiled binary
    operation didn't know its result was about to be stored back into
    its left operand's local. Its site now carries that local, and the
    helper lets the local go and appends where the string is (649 ns an
    append at 32,000 appends, growing with the length, to 50 ns flat;
    CPython 104 to 118). A concatenation whose left operand is a
    temporary nothing else holds (`a + b + c`'s second `+`) grows that
    one too.
    A function with n parameters took n² to parse and to compile: both
    duplicate-parameter checks scanned a vector, and every use of a
    local searched the names for its index. CPython's own
    `test_collections` makes a 150,000-field `namedtuple`, whose
    constructor is such a function: 117 s, now 1.5 s (CPython 1.9).
    And `int("x12")` in a `try`, the everyday test for a number, ran the
    big-integer parser at each of the three layers that reach the
    constructor's quick arms before the full constructor rejected it
    (the arms had started building big values a few commits back): the
    arms decline text that isn't digits at its first byte, 4% of the
    `exceptions` workload's instructions.
  - The sweep's constant factors, and two more cliffs found beside them.
    Copying a sequence's items moved each one through an iterator: an
    object returned by value travels as a tag byte and fifteen more,
    stored and reloaded unaligned, about twenty instructions an item.
    `tuple(xs)`, `a + b`, `t * n`, `(*xs,)`, slices, `list(xs)`,
    `xs.copy()`, `extend` and the snapshots `min`, `max` and `sorted`
    take now copy the items whole and take their counts in a second
    pass, and a vector that becomes a tuple hands its items over in one
    copy. At 1,000 ints: `tuple(xs)` 8.0 µs to 1.0 (CPython 1.9),
    `t + t` 10.1 to 1.9 (2.8), `t * 2` 9.9 to 2.4 (2.6), `(*xs,)` 6.4
    to 1.5 (3.6), `t[::-1]` 5.1 to 1.6 (1.6). A dict's or a set's
    entries copy the same way (`dict(d)` and `d.copy()` 5.6 µs to 3.7;
    CPython 3.2), and a list whose items all outlive it is released
    without rewriting its slots.
    `for x in reversed(xs)` in a loop tier 2 compiled stepped its
    iterator through the interpreter's general `next`, 41 ns an item
    (the same loop over the list itself 3.5): the reversed list's
    iterator steps in place, as a list's does (7 ns; CPython 22).
    A tuple's hash walked its items twice and reduced each int modulo
    the hash prime, and `hash()` first checked each item's hashability
    through a call: one pass, an int below the prime is its own hash,
    and scalars are checked in line (34 instructions an item to 13;
    `hash(t + (i,))` of 1,000 ints 9.0 µs to 2.8, CPython 3.4).
    A `memoryview` slice left compiled code for the interpreter and
    built four vectors on the way to a view that needs none (3,530
    instructions to 1,536; 283 ns to 152, CPython 72: the view's own
    copy, with its format string, is most of what is left).
    `str.count` of one character ran the substring search per
    occurrence (29 µs to 1.8 on a 33,600-character text, CPython 11.9).
    The first cliff: `b[i:j] = data` on a `bytearray` rebuilt the whole
    buffer as a list of int objects, spliced that, and wrote every byte
    back, whatever the slice's size, and `del b[i:j]` removed the
    slice's bytes one at a time, moving the tail for each. On a 1 MB
    buffer a four-byte store took 9.1 ms (CPython 171 ns) and
    `del b[:16]` 388 µs (72 ns). Both work on the bytes now: a store of
    the same length writes in place, any other moves the tail once, and
    an extended slice is stored or closed up in one pass (237 ns, and
    23 µs for the delete, which still moves the megabyte: CPython keeps
    a start offset, and that is what is left here).
    The second: merging dicts. `d.update(other)` gave up its one-pass
    merge when `d` held more than 32 keys (checking them for Python
    `__eq__` was thought too dear) and then stored key by key through a
    snapshot of `other`; `d.update(d)` did all of that to change
    nothing; `dict(pairs)` and `d.update(pairs)` made an iterator and a
    vector for every pair; and an items view was read as pairs like any
    iterable. The merge now probes with each key's stored kind and
    stops only at a key that needs Python (the keys before it are in,
    and the general path merges them again harmlessly), a dict merged
    into itself returns at once, a list or tuple of exact pairs is
    walked in place under one borrow, `zip` of two plain sequences
    builds its dict first, and an items view merges as its dict. At
    1,000 keys: `d.update(other)` with half the keys shared 68 µs to 27
    (CPython 34), `d |= other` the same, `d.update(d)` 46 µs to 66 ns
    (63), `dict(pairs)` 110 µs to 33 (31), `d.update(pairs)` 74 to 26
    (31), `d.update(other.items())` 103 to 4.0 (36), `dict(d.items())`
    131 to 4.4 (36), `d.update(zip(ks, vs))` 92 to 24 (40).
    Left: a generator of pairs (`d.update((k, v) for ...)` 136 µs
    against 63, the generator's resumption from native code),
    `dict(d, **kw)` (the keyword-spread call's own machinery, about
    6,000 instructions), small-dict merges at 1.3 to 2 times CPython
    (the new container's tracking and release), `list.pop(0)` and
    `xs * 2` (16-byte items against 8), and the `bytearray` start
    offset.
  - Bytes-like values were copied whole before anything read them. A
    second sweep, of 90 operations on 200,000-item buffers of each kind
    (`bytes`, `bytearray`, `memoryview`, `array`, and the containers),
    put `array[i]` at 870 times CPython's time, `struct.unpack_from` at
    61 times and `bytearray.decode` at 33. One accessor was behind most
    of it: the value's bytes as an owned vector, so a call that reads
    four bytes of a buffer first copied the buffer; and `array` is a
    Python class over a `bytearray`, each item read through
    `struct.unpack_from`, so indexing a large array copied all of it per
    item. The readers now borrow: `struct` reads and writes the buffer
    where it is, with native halves for `unpack_from` and `pack_into`
    (over a 200,000-byte `bytearray`: 5.1 µs to 109 ns, CPython 101;
    706 ns to 76, CPython 105), hashing, `zlib`, the codecs' functions,
    `int.from_bytes` and `str(b, encoding)` take a `bytes` object's own
    bytes, a new `bytes` is built from a slice in one copy where it took
    two (`a + b` and `bytes(bytearray)` at 200,000 bytes 11.2 µs to 4.6,
    CPython 4.6), and the codec engine returns well-formed UTF-8, ASCII
    and Latin-1 text as it is, where it decoded every byte to a code
    point and encoded the lot again (`bytearray.decode()` of 200,000
    bytes 636 µs to 15, CPython 26).
    The `array` class's item methods are native for plain indexes and
    values, as CPython's C type's are, each falling back to the Python
    method it replaces for anything else (other index and value types,
    every error): `a[i]` 26.7 µs to 61 ns on 200,000 ints (CPython 44),
    `a[i] = x` 1.5 µs to 56 ns (49), `a.append(x)` 503 ns to 66 (76),
    `a[i:j]` 3.4 µs to 526 ns (148), iteration 1.5 µs an item to 174 ns
    (63), and `tolist`, `count`, `index`, `in` and `fromlist` unpack or
    pack every item in one call.
    A `memoryview`'s plain byte reads and writes skip the shape and
    format machinery (`view[i]` 235 ns to 48, CPython 36; a run store
    375 ns to 225). `frozenset(a_set)`, `set(a_frozenset)` and
    `set(a_dict)` take the source's table with its stored hashes (11.9
    ms to 1.3 for 200,000 keys, CPython 2.4); `dict(zip(xs, ys))` keeps
    its direct build past 4,096 pairs (28 ms to 3.5 at 200,000, CPython
    9.0); list and tuple comparison settles two machine ints in line
    (425 µs to 211 for 200,000 items, CPython 192); `range(n)[k]` stays
    at machine width when it can.
    Two wrong answers came out of the new tests: `struct.unpack_from`
    read a non-int offset (an object with `__index__`, or a `str`) as
    zero, and unpacked from a released view; and a strided view is now
    refused with `BufferError`, as CPython refuses it, not gathered.
    Left: an array's slices and iteration (3.6 and 2.8 times CPython: a
    slice of an instance leaves compiled code, and the iterator's step
    goes through its slots), `view[i] = x` and `range(n)[k]` in compiled
    code (4 and 2.9 times, the same exit), `bytearray` methods (no
    native lane: `append` 130 ns against 41), SHA-256 itself (2 times:
    the hash's throughput, not its argument), and about forty other
    callers of the copying accessor (`io` has thirteen).
  - A third sweep, of 130 I/O, codec, text and library operations on
    large inputs, found the same kinds of cost in more places.
    `zip(*rows)` was quadratic in its rows: each step checked that no
    source was another's alias by comparing every pair, and a transpose
    has a source per row (20,000 rows of three: 404 ms, CPython 2.1).
    The answer can't change, so the zip keeps it from its first native
    step (sorted addresses when there are many sources): 2.3 ms. The
    grouping idiom that passes one iterator several times
    (`zip(*[it] * n)`) is the aliased case, and steps as before.
    A string slice with a negative bound or a step spread the whole
    string into a vector of code points first, so `line[-20:]` and
    `text[:-1]` cost the string's length: on 226,000 ASCII characters,
    `s[-20:]` 295 µs to 60 ns (CPython 63), `s[:-1]` 641 µs to 12 µs
    (5.8), `s[::-1]` 651 µs to 15 µs (101). ASCII text is sliced by its
    bytes.
    UTF-16 and UTF-32 went through the general codec engine's code
    point vectors even for well-formed text: a `str` is encoded straight
    from its text, ASCII as a byte and its zeros, and well-formed input
    is decoded in one pass (226,000 characters: encode 809 µs to 93,
    CPython 75; decode 878 µs to 166, CPython 67). Results match
    CPython's on 447 cases of well-formed and malformed input under
    every error handler.
    `x in a_bytearray` copied the buffer before searching it (5.2 µs
    to 0.17 for a hit near the front of 200,000 bytes; CPython 1.5).
    `list(b)`, `memoryview.tolist()`, `bytes(a_list)` and
    `bytearray(a_list)` converted item by item through the general
    protocols (5.5 ms, 10.7 ms and 1.5 ms for 200,000 bytes; now about
    0.2 ms each, CPython 0.8 to 0.9). `struct.iter_unpack`'s iterator
    steps natively (17.2 ms to 5.7 for 50,000 records, CPython 1.75).
    `bytearray` has a native method lane for `append`, `extend`, `pop`
    and `decode` (`append` 77 ns to 63, CPython 36), and compiled code
    reads `b[i]` and `len(b)` and stores `view[i] = x` in line (`b[i]`
    189 ns to 25, CPython 31; the store 204 to 48, CPython 42).
    `range(n)[k]` in compiled code 210 ns to 40 (77), `slice.indices`
    at machine width 252 ns to 104 (130), a forward stride slice of
    bytes checks its bounds once (`b[::2]` 137 µs to 52, CPython 25),
    and `bytes.translate` maps in one pass (173 µs to 121, CPython 70).
    Left from this sweep: `BytesIO.getvalue()` copies the buffer each
    call (CPython hands out its own bytes object until the next write),
    `re` on a `bytearray` or `memoryview` widens the whole subject per
    call, `BytesIO` reads and writes at 2 to 3 times CPython,
    `iter_unpack` at 3 times, UTF-16 decoding at 2.5 times.
    One thing the native `array` methods broke, which the ecosystem lane
    caught: the C API's `PySequence_Check` took an instance for a
    sequence only when its class's `__getitem__` was a Python function,
    so Pillow's `putdata(array(...))` refused the array. A natively
    served `__getitem__` on a class that declares itself a sequence
    counts too, as CPython's own `array` type does.
  - Iterating byte strings and arrays. `for b in data` over a `bytes` or
    `bytearray` left compiled code at every step for the interpreter's
    general `next` (39 ns a byte, CPython 21); both tiers now step them
    in place (8 ns). `sum`, `min` and `max` of one take its bytes
    directly (`sum` 22 ns a byte to 0.2; CPython 5.3). The array
    iterator's native step and the item methods read their instance's
    slots where the class lays them out, not by name (`for x in a` 72 ns
    an item to 42, CPython 35), and `list(a)`, `tuple(a)` and
    `sorted(a)` unpack the array at once when its `__iter__` is its own
    (91 ns an item to 21, CPython 15). An ASCII string's run with a
    negative bound (`s[:-1]`) is copied once from the text, not through
    a vector of bytes (14 µs to 6 for 226,000 characters, CPython 6.2).
  - `io.BytesIO` reads, `readinto` and writes after a seek went through
    the file layer's general path (its sibling stream, read-ahead and
    write-buffer checks, and a vector per read): `read(64)` 164 ns to 68
    (CPython 62), `readinto` 154 to 54 (44), a write over earlier bytes
    165 to 64 (46), and a `bytearray` written 146 to 62 (51). Each is
    now a fast half on the stream's buffer, declining a closed stream,
    one with bytes parked ahead, an exported buffer, and a stream read
    into its own buffer.
  - A fourth sweep, of 55 interpreter-level operations at n and 8n
    (compiling large and deeply nested code, calls with many arguments,
    deep inheritance, recursion, tracebacks, formatting, library
    algorithms), found two more growth cliffs. Compiling functions
    nested `d` deep cost about `d` cubed: each enclosing scope asked for
    each nested function's free names, recomputed from the whole subtree
    every time (80 nested closures, 47 ms to compile, CPython 2.1). The
    free names are kept for the compilation, by the function's place in
    the tree: 3.6 ms. And `deque.rotate` made room at the front one
    rotation's worth at a time, so every few rotations shifted the whole
    deque item by item (`rotate(n // 3)` on 4,000 items: 17.6 µs, CPython
    0.6); the room is now as large as the deque, made by moving its
    items once, and a rotation trades its items with the free slots as
    one block (4.0 µs). `extend` and `extendleft` of a list or tuple are
    native (`extendleft` of two items 309 ns to 194, CPython 93); a
    generator still goes in item by item, so an exception partway leaves
    the items before it, as CPython does, which needed the dispatch's
    by-name rule for `extend` (materialize the iterable first) to leave
    a plain class's native methods alone. Generator chains, which cost
    more per level the deeper they go (16 levels: 85 ns a level-step,
    CPython 16), are left for now.
  - `re` over a `bytearray` or a `memoryview` (how h11 and urllib3 parse
    a socket's buffer) converted the whole subject to code points on
    every call, so a search near the front of 200 KB cost 28 µs (CPython
    0.27) and a `findall` over it 24 ms (0.17). The bodies that run no
    Python code (`search`, `match`, `fullmatch`, `findall`, `split`,
    `finditer`, and `sub` with a bytes replacement) now read a
    `bytearray`, or a plain view's window of a `bytes` or `bytearray`,
    where its bytes are, and return `bytes` pieces as CPython does (0.47
    µs and 0.24 ms). Results equal the old ones on 524 cases over five
    buffer kinds.
    `bytes.fromhex` read its text as a vector of characters: ASCII text
    is now read by its bytes through a digit table (51,200 bytes: 6.3 ms
    to 1.4 for five, CPython 1.4 for five; errors still come from the
    character reader, so their positions are unchanged).
  - `{**d}` and `f(**kw)` into an empty dict inserted the source's keys
    one at a time, rehashing and growing the table (200,000 int keys
    13.3 ms, str keys 19.9 ms; CPython 1.9 and 2.8): both the core
    loop's arm and the full handler now copy the source's table whole
    when they can, and merge a display's `**d` into a non-empty dict as
    `dict.update` does (1.9 and 3.4 ms). A deque's `in`, `count` and
    `index` compare plain items (numbers, strings, bytes, `None`)
    natively before the Python loop that any other item needs (`x in d`
    of 200,000 ints 2.5 ms to 1.9; CPython 2.5), and `index`'s error
    reads as CPython's.
  - A sweep of 75 class-protocol operations against CPython (named
    tuples, dataclasses, enums, getters, dunders, iterators) put
    `operator.itemgetter` on a named tuple at 13.7x (479 ns, CPython
    35): its fast half took only exact tuples, lists and dicts, so a
    subclass declined through seven call lanes to the full body. It now
    reads a subclass's payload when the class keeps the base's
    `__getitem__` (96 ns). `methodcaller` read its three fields by name
    and built a bound method and keyword list for every call; it now
    reads them at their slots and calls a plain instance's Python method
    with the instance first (502 ns to 358; CPython 99). `str(x)` of an
    instance with only a Python `__repr__` looked `__str__` up along the
    MRO and built a bound method to probe for `__repr__` (217 ns to 148;
    CPython 64), and `f'{x!r}'` went through a bound method and a Rust
    string (241 ns to 125; CPython 58): both call the function directly
    now, and `str(x)`'s error for a non-string result is CPython's.
  - A frameless `__next__` could end at a bare `raise StopIteration`
    only with no attribute store buffered, so the usual shape (`self.i
    += 1` ahead of the exhaustion test) ran the whole last step as a
    frame, raised, and caught (`list(It())` of three items 1.87 µs,
    CPython 0.62): the buffered stores now land as a return lands them,
    and the step ends as exhausted (0.93 µs). `next(it)` without a
    default still declines on such a step, since its full call raises
    the exception with its traceback and would repeat the stores.
  - A named tuple built with a default filled in (`P(1)` of
    `namedtuple(..., defaults=...)` or a `typing.NamedTuple` with a
    default) or with keywords (`P(x=1, y=2)`) ran the generated
    `__new__` as a frame and went through `object.__new__`'s seeding
    (743 and 914 ns; CPython 228 and 340): the construction lanes now
    place keywords by field name and take the rest from `__new__`'s
    defaults as they stand, building the tuple directly (256 and 279
    ns). A rebound `__defaults__` or `__new__`, and every call that
    binds otherwise, still runs the function.
  - The frameless `f(*args)` path took only plain functions given
    their exact arity, so `obj.m(*args)` (a method read as an attribute
    first) ran as a frame, and a spread short of the arity went through
    the keyword binder: a bound method's receiver now binds first, and
    trailing parameters come straight from the positional defaults
    (`obj.m(*args)` 243 ns to 136, CPython 87; `f(*(1,))` with a
    default 84 to 74, CPython 45).
  - `a > b` of a class that defines only `__lt__` looked up `__gt__`,
    found `object`'s, bound and called it to get `NotImplemented`, then
    went through the general protocol to the reflected `b.__lt__(a)`
    (341 ns, CPython 130; `max()` over 50 such instances 11.7 µs,
    CPython 5.6). `object`'s orderings are now recognized per class and
    skipped, and two instances of one class call the reflected method
    directly, in compiled code as a frameless leaf (73 ns; `max()` 5.5
    µs). `weakref.ref` calls have a leaf half that calls the referent's
    deref closure in place (78 ns to 45, CPython 26), and `r(1)` raises
    CPython's `TypeError` instead of ignoring the argument.
  - `for x in itertools.chain(xs, ys)` stepped each item through six
    layers (the core loop's lazy step, the zip check, the itertools
    machine's dispatch, then a peek and a skip of the active cursor):
    349 instructions an item (two 100-item lists 6.3 µs, CPython 2.1).
    The core loop's lazy step now takes `chain` first, and the active
    list, tuple or range cursor is read and stepped in one call (173
    instructions; 4.2 µs).
  - A compiled frameless leaf (a getter, a `__hash__`, a comparison)
    cached where an instance keeps a field only for split layouts. An
    instance whose `__dict__` was materialized (a `cached_property`
    computed, `vars(obj).update(...)`, anything `copy` or `pickle`
    rebuilt) resolved the field by name and checked its class on every
    read, about 500 instructions (`hash(obj)` of `return hash(self.x)`
    151 ns against 105 for a split instance). Its position in the
    published dictionary is cached too and checked by name as read
    (116 ns).
  - `shlex.split` reads its input one character at a time
    (`instream.read(1)` of a `StringIO`), and each read took the text
    layer's whole path: the characters' bytes copied out, checked as
    UTF-8, newline-translated and un-bridged into a new string, about
    650 instructions. A read of ASCII characters with no `\r` now copies
    them straight out of the buffer as a leaf half (`shlex.split` of a
    five-word command 25.5 µs to 21, CPython 14). A read past the end
    keeps the position a seek left, and a closed `StringIO`'s error
    drops the period, as CPython's does.
  - Compiled code ran a container method's kernel in line only for a
    receiver in a local (`x.append(v)`): one an expression left on the
    stack (`self.items.append(v)`, `d[k].append(v)`) loaded the method
    through the method-site helper and called it through the call-site
    helper, about 145 instructions more (`o.l.append(1)` 34 ns, CPython
    24; `d[k].append(1)` 40, CPython 31). The stack's receiver now has its
    type checked in line too and the kernel called with the result in the
    slot above; the receiver is released as the full call releases its
    operands (a temporary's finalizers run before the next statement) and
    the result moved into its slot (28.5 and 32.4 ns).
  - `OrderedDict.fromkeys` stored its keys one subscript at a time in
    Python (270 keys 21.2 µs; CPython's C loop 12.7). An exact
    `OrderedDict` now takes a list's, tuple's or str's keys through
    `dict.fromkeys` and its native update in one pass (10.0 µs);
    subclasses keep the loop for their own `__setitem__`, and an
    unhashable key still raises from the loop, which words the error as
    CPython's `OrderedDict` does.
  - Tier 2's `len()` and `next()` of an instance with a native leaf
    dunder (`len(deque)`) looked the dunder up along the MRO on every
    call (190 of the 270 instructions a `len(dq)` cost): the class now
    caches the builtin it resolves per version.
  - UTF-16 decoding of ASCII text checked each unit, gathered the low
    bytes into a vector, then validated and copied them into the string
    (110 KB 47 µs, CPython 19): it now checks eight bytes at a time and
    packs each word's four low bytes straight into the string's
    allocation (22.5 µs).
  - The native pickler and unpickler knew `bytes` but not `bytearray`,
    so protocol 5 (the default) sent any data holding one to the Python
    pickler (64 KB `dumps` 35.8 µs and `loads` 39 µs; CPython 2.2 and
    1.5). Both now handle `BYTEARRAY8`, memoized as CPython's pickler
    memoizes it and framed as it frames one (8.2 and 3.2 µs; the bytes
    written are the Python pickler's exactly).
  - `int.from_bytes` of a short `bytearray` or `memoryview` took the full
    method (2,000 four-byte memoryview slices 1.3 ms, CPython 0.46): the
    leaf half now reads them in place (0.39 ms), and a released view
    raises `ValueError` as CPython's does. `struct.iter_unpack`'s step
    reads its iterator's state at the class's slot places instead of by
    name (50,000 items 6.1 ms to 4.8; CPython 2.0), and a `bytes`
    subclass reaches the core through a view instead of failing.
    `array("d", floats)` checked its items' types in a Python loop:
    a native check now (25,000 floats 967 µs to 299; CPython 429).
  - `zlib.crc32`/`adler32` and `hashlib`'s constructors and `update`
    copied a `bytearray`'s or `memoryview`'s whole buffer before reading
    it: they read it in place now (`crc32` of 200 KB 13.2 µs to 7.5,
    CPython 8.4). `adler32` took a modulo per byte; it reduces once per
    5,552-byte block, as zlib does (473 µs to 72; CPython 12), and
    reduces its starting value even for no data, as zlib does. A released
    view raises `ValueError` and a strided one `BufferError` in all of
    them, as CPython's do. (SHA-256 itself is the `sha2` crate's portable
    code, about half OpenSSL's AVX2 speed on this machine.)
  - Iterating a memoryview made an object of every element first, then
    walked that snapshot (`for b in memoryview(data)` over 200 KB 9.6 ms,
    CPython 4.9). A plain byte view of a `bytes` object now steps as the
    bytes do, sharing their storage when it covers them (1.4 ms), and one
    over a whole `bytearray` steps the bytearray live, so a store made
    mid-loop is seen, as CPython's iterator sees it (the snapshot missed
    it). A released view's error reads as CPython's in `iter`, `tobytes`
    and `tolist`.
  - `bytes.fromhex` now decodes runs of whole pairs, a bad digit found
    from the table's high bits, before looking at whitespace (400,000
    digits 520 µs to 347; CPython 263). `b"".join` of `bytearray` parts
    reads them in place instead of copying each into a part list first
    (2,000 parts 53 µs to 22.6; CPython 26).
  - `memoryview.tobytes()` copied a contiguous view's bytes twice (200 KB
    17.9 µs, CPython 5.8): once now (6.0 µs). A strided one-dimensional
    view is gathered in one read of its buffer instead of a buffer read
    per element (`mv[::2].tobytes()` 3.0 ms to 0.37; CPython 0.64).
    `bytearray.extend` of a list or tuple of ints took the full method
    (four ints onto 200 KB 563 ns, CPython about 220): the leaf half takes
    them, all checked first (171 ns).
  - `next(iter(d))` (a dict's first key) took `next`'s full call for a
    dict or dict view's iterator, and `iter()` of a view the general
    builtin path (`next(iter(d))` 188 ns, CPython 78; `iter(d.values())`
    106 ns, CPython 61). `next`'s leaf half now steps a dict iterator as
    `for` does, leaving a changed dict and the end to the full call, and
    `iter`'s takes views and `mappingproxy` objects (118 ns and 64;
    `next(iter(d.items()))` 291 ns to 180, CPython 141).
  - Compiled code called an instance's `__getitem__` directly only when
    it was a Python function, so every subscript or slice of an `array`
    (whose `__getitem__` is a native method with a leaf half) left for the
    core loop, and tier 2 took the general subscript path for it. Both
    tiers now call a plain class's native leaf half in place, released as
    the full call releases its operands (`a[i:i + 4]` 5,050 instructions
    to 2,070 and 502 ns to 217, CPython 168; `a[i]` 84 ns to 59, CPython
    63). The slice reads its source's seven slots by position once the
    class layout matches, instead of by name. `a + b` and `copy.copy(a)`
    ran Python methods: they're native now, and compiled `+` of two
    instances of one class calls a native `__add__`'s half in place
    (`a + b` 1,075 ns to 202, CPython 85; `copy.copy` 673 to 350, CPython
    209). `len()` of an instance with a native `__len__` caches the half
    itself rather than finding it again on every call.
  - `operator.methodcaller`'s native call had no leaf half, so compiled
    code left for the core loop on every call, and the native read its
    fields, the method and a bound method through the general paths
    (`methodcaller("lower")("ABC")` 535 ns, CPython 78). A leaf half now
    runs a native value's leaf method (a `str`, `bytes`, `list`, `dict`,
    `set` or `int` receiver, positional arguments only) in place (106 ns;
    `dict.get` through one 573 to 108, CPython 59; `sorted(words,
    key=methodcaller("lower"))` 377 to 129 ns an item, CPython 105). When
    the half declines (a Python method of an instance, or an `itemgetter`
    of a Python `__getitem__`), compiled code runs the native body in
    place with the instance first, as the core loop's rewrite of the call
    passes it, instead of leaving for the core loop (`methodcaller("f",
    1)(obj)` 515 ns to 184, CPython 75).
  - A store through a class's own `__setattr__` looked the hook up along
    the MRO on every store, twice when it was Python (`o.x = i` with a
    one-line Python hook 3,670 instructions, CPython 1,230). The class
    now caches its Python and native hooks per version (3,385). The rest
    is the hook's activation in the core loop: compiled code leaves for
    it on every store.
  - Building an `itemgetter`, `attrgetter` or `methodcaller` ran its Python
    `__init__`, whose `*items` takes a frame (`itemgetter(1)` 321 ns, CPython
    121; `attrgetter` 416, CPython 151; `methodcaller` 389, CPython 108).
    The exact classes now have pure constructors that lay their slots out
    at once (189, 178 and 206 ns); a subclass, keywords to `methodcaller`, a
    name that isn't a `str`, or a class changed since registration take the
    Python `__init__`.
  - A keyword call of a native method or function took the core loop's
    keyword lane: the names copied into strings, the operands into
    vectors, and the general call (`xs.sort(key=None)` 279 ns, CPython 90;
    `sorted(xs, reverse=True)` 335, CPython 207). `list.sort` and `sorted`
    now have keyword halves for no key and a plain flag, bound as the
    existing keyword halves bind (with keyword-only and positional-only
    parameters marked, so `xs.sort(None)` and `sorted(iterable=xs)` still
    raise), and the sort they share also takes tuples of ints and strings
    (`pairs.sort()`). `xs.sort(reverse=True)` 286 ns to 108, `sorted(xs,
    key=None)` 335 to 186. The per-call lookup of a builtin's keyword
    half took a lock each time: a per-thread cache answers it.
  - `list(product(...))` and its kin stepped each row through four layers
    of the lazy-iterator step (`list(product(range(5), repeat=3))` 592
    instructions a row, CPython about 200). `list()` of a combinatoric
    iterator (`product`, `permutations`, `combinations`,
    `combinations_with_replacement`, a bounded `repeat`) now steps the
    machine to its end directly (478). The rest is the tuple itself: a
    per-thread free list for small tuples, parked at their last release,
    made no difference (the allocator's own free and malloc cost what the
    list's push and pop do).
  - More keyword calls in place: compiled code builds `dict(a=1, ...)` from
    the keywords as the core loop's arm does (400 ns to 126 in the
    container, CPython 107), and `s.encode(encoding=...)` and
    `s.split(sep=..., maxsplit=...)` take keyword halves (263 to 117, and
    361 to 214). Keyword halves can mark a parameter `?name`, optional but
    positional too, so `s.split(maxsplit=1)` binds without its `sep`.
  - The keyword sorts also take a key whose keys compute without running
    code: `len` of strings and built-in containers, an `itemgetter(i)` of
    tuples or lists (its index read once for the whole sort), or an
    instance whose native `__call__` has a leaf half (`attrgetter`). The
    keys sort as CPython decorates them, stable in both directions
    (`sorted(items, key=itemgetter(1))` of ten pairs 6,530 instructions to
    4,300, 995 ns to 600; CPython 392). `sorted` takes a dict or a dict's
    view too (`sorted(d.items(), key=itemgetter(1), reverse=True)`).
  - A callback that calls an instance with a native `__call__` (`map(itemgetter(1),
    xs)`, a `key=attrgetter("n")`) looked `__call__` up along the class's
    MRO and found its leaf half again for every item, and cloned both
    operands (893 instructions an item for `map(itemgetter(1), pairs)`).
    The class now caches the half per version, and the call passes views
    of the operands (608).
  - `random.randint`, `randrange` and `choice` ran `random.py`'s Python
    methods over the native `getrandbits` (`randint(1, 100)` 340 ns, CPython
    171). For the `Random` class itself, unchanged since import, with an
    instance that doesn't override `_randbelow` or `getrandbits` and plain
    int arguments (a list, tuple or ASCII string for `choice`), they're
    native now, drawing as `_randbelow_with_getrandbits` draws, so a seeded
    stream is the same as CPython's (108, 94 and 109 ns; CPython 171, 157
    and 167). `shuffle` of a list is native the same way (100 items 13.5
    µs to 3.0; CPython 13.7). Anything else calls the Python method, as
    does every call while a trace or profile hook is set (a debugger steps
    into `randint`: `test_pdb` does). (A native `uniform` was slower than
    the compiled Python one, whose `self.random()` compiled code already
    calls natively: its guards cost more than the call.)
  - `copy.deepcopy` walked every container through `copy.py`'s Python
    dispatch (a small nested dict 6.3 µs, CPython 3.8; fifty rows of dicts,
    lists and tuples 228 µs, CPython 153). A tree of exact dicts, lists and
    tuples over `None`, bools, ints, floats, strings and bytes, with no memo
    given, is now copied natively as `copy`'s own table copies it: shared
    parts stay shared, cycles are kept, and a tuple whose items all copy to
    themselves is returned itself (1.4 µs and 22 µs). Anything else in the
    tree, a memo, a tree over 200 deep, or a trace hook takes the Python
    function.
  - `Counter.most_common(n)` ran `heapq.nlargest` in Python over the
    items, with an `itemgetter` key (ten counts, `most_common(3)` 4.4 µs,
    CPython 1.8). An exact `Counter` whose counts are all ints now sorts
    them natively, largest first and equal counts in insertion order, as
    `nlargest` orders them.
  - A disabled level's `logger.debug(...)` ran two Python calls, `debug`
    with its `*args` and `**kwargs`, then `isEnabledFor` (214 ns, CPython
    141). `Logger`'s five level methods are native now: on a logger whose
    class keeps `logging`'s own `isEnabledFor`, with the logger disabled or
    `False` cached for the level, no keywords and no trace hook, they
    return at once; everything else is the Python method (119 ns; `info`
    186 to 107, CPython 106). And compiled calls of a native method on an
    instance ran it in place only when the instance had no attributes at
    all: now when it has none of the method's name (a `Logger`, the
    `random` module's `Random`).
  - A call binding a `*args` tuple moved the surplus arguments into it one
    at a time through an iterator, each by way of the stack: they move in
    one copy now (`f(*t)` of three items 1,166 instructions to 1,120).
  - A builtin type called with keywords naming its next positional
    parameters in order (`int(text, base=16)`, `enumerate(xs, start=1)`,
    `str(b, encoding=...)`, `bytes(s, encoding=...)`) took the general
    keyword call (341 ns for that `enumerate`, CPython 145; `int` 300,
    CPython 201). Compiled code calls it as the positional call it is, the
    keyword values already in the positional places (131 and 75 ns in the
    container's build, where the positional calls take 122 and 58).
    `zip(xs, ys, strict=True)` and `map(f, xs, strict=True)` of a plain
    `bool` flag are built natively the same way (396 ns to 202 in the
    container; CPython 321).
  - `b.decode(errors=...)` and `b.decode(encoding=...)` of bytes and
    bytearrays take a keyword half too (270 ns to 165 in the container;
    CPython 103), and the full method now rejects an unknown keyword with
    CPython's `TypeError`, where it ignored it. `max(xs, default=0)`,
    `min(...)` likewise (no key: the positional half, or the default for an
    empty list or tuple; 298 ns to 133 in the container) and `round(x,
    ndigits=2)` (its positional half, 394 to 223).
  - `BytesIO.getvalue()` copied the whole buffer on every call (34 µs for
    a 1 MB stream, CPython 23 ns, which hands out its bytes object until
    the next write). The stream keeps its last result now and returns it
    again until a write, a truncate, a re-`__init__`, a `__setstate__` or
    a `getbuffer()` (whose view can write in place) changes the buffer,
    and a stream made from a bytes object starts with that object (70 to
    90 ns a call at any size).
  - `l.sort()` of a list of floats compared them through the general
    comparison (6,963 µs for 20,000 floats; `sorted()` of the same list
    1,645, CPython 2,118 for both): it compares two floats directly now
    (1,628 µs), as `sorted()` did.
  - `pickle.dumps(1)` took 1,768 ns (CPython 674) and `pickle.loads` of a
    small stream 1,751 (CPython 344). Each call ran two Python frames
    (the stand-in built-in object's `__call__` with its `*args` and
    `**kwargs`, then the Python `dumps`) before the native encoder, and
    the encoder's guard then checked the code and defaults of 52 Python
    functions (30% of the call's instructions). `dumps` and `loads` are
    built-in functions now, as CPython's are, running the native engine
    with no Python frame and anything it declines in the Python pickler
    directly. And the guards check each function once per change to any
    function's code or defaults (a count kept for this, never reset),
    and otherwise only which functions the classes and tables hold (789
    ns and 1,049). The tables themselves aren't walked either while their
    dicts' mutation stamps hold (the unpickler's has 70 entries, a third
    of the call's instructions), and `loads`'s default `encoding` and
    `errors` are made once: 760 ns and 756.
  - `list(filter(None, xs))` and `filter(bool, xs)` over a list or tuple
    stepped the filter once an item (153 instructions each; 5,564 µs for
    200,000 ints, CPython 2,552). When every item left has a truth that
    runs no code, the items kept are counted, then copied at once, and the
    source is stepped past its end as stepping it leaves it (4,164 µs to
    1,848, or 5.3 ns an item for 1,000 items, CPython 13.4).
  - `struct` unpacks of one or two numbers built a list and then the tuple
    from it, and counted the format's values on every call: the tuple is
    built directly now, the count kept with the compiled format
    (`iter_unpack` 1,249 instructions an item to 1,061).
  - A `Vec` of plain data became a `bytes` (or another shared slice) one
    element at a time: it's one copy now (`pickle.dumps` of 200 KB of
    bytes 21.4 µs to 18.5).
  - A stepped slice (`b[::2]` of 200 KB) took its items through a
    stepping iterator, a capacity check and a bounds check each: it writes
    them from an exact-length range now, indexed unchecked within the run
    the count proves (54.5 µs to 26.2, CPython 26.9; bytes 29.5, CPython
    33.8). `bytearray * n` copied the operand before repeating it (17.2
    µs to 11.3, CPython 13.0), and `bytearray.translate` copied the
    receiver and looked bytes up in a table checked at every byte (155 µs
    to 72, CPython 88): both read the bytearray in place now, translate
    after it has its table and deletion set, as CPython reads them.
  - `BytesIO.getbuffer()` and every other `bytearray` export registered
    itself in a map hashed with SipHash (three hashes a view, about a
    tenth of `getbuffer()[0]`), now the interpreter's fast hasher; and a
    memoryview's format is a shared string, so a slice of a view no longer
    allocates a copy of it.
  - `BytesIO.write(view[i:i + 64])` took the full method, which copied
    the view's bytes out first, and slicing the view took five checked
    cell borrows to copy its layout (2,828 instructions a write and
    slice). A one-dimensional view of unsigned bytes now writes from where
    its bytes are, and the view's layout is read guard-free (545 µs for
    2,000 writes to 367, CPython 265). The full method now raises
    CPython's `BufferError` for a strided view and `ValueError` for a
    released one, where it wrote the first and accepted the second.
  - Every native entry into the interpreter (an iterator step from
    `csv`, a callback from a C extension) pushed the interpreter's pointer
    and its four thread handles onto thread-local stacks and popped them
    after, about 230 instructions an entry and six entries a `csv` row.
    When the interpreter and its handles are already on top, as for a
    nested entry, nothing is pushed or popped (a 5,000-row `csv` round
    trip 7.27 ms to 6.98).
  - `csv.writer.writerow` stepped every row through the interpreter's
    iterator protocol, one entry a field, and made each number's text a
    Python string before joining it. A list or tuple of strings, numbers,
    bools and `None`s is read at once now (writing those runs no code, so
    a live iteration would see the same items), and numbers and bools are
    joined as text (5,000 rows 3.49 ms to 2.49, CPython 2.92).
  - A compiled `for v in d.values()` stepped the view through the general
    dict iterator (251 instructions a value, CPython 139), where keys had
    a step of their own: values take the same direct step now, the dict
    unchanged since the first step and its length checked (189).
  - `format(x, "g")`, `"e"` and `%g` rounded every float through Rust's
    exact formatting, which for most values falls back to big-number
    arithmetic (two thirds of `format(1.5, "g")`'s 3,938 instructions,
    CPython 2,291). For a zero or normal float at no more than 15
    significant digits whose shortest round-trip digits fit, those digits
    padded with zeros are the correctly rounded ones (the shortest form is
    within half an ulp, under half a unit of the 15th digit), so they're
    used as they are (3,237). A test checks every precision from 0 to 20
    across the exponent range against `decimal`'s rounding of the exact
    binary value.
  - `decimal.localcontext()` took 54 µs (CPython 0.7): nearly all of it
    `Context.copy()`, which ran `Context.__init__`'s nine validating
    attribute setters and signal-dict checks over a context's values,
    already valid (243,000 instructions). The copy sets them as they are
    now and builds its own flag and trap mappings (56,000; `copy()` 43 µs
    to under 10, `localcontext()` to 16).
  - `list(accumulate(xs))` with no function over a list or tuple stepped
    the adapter once an item through four layers (741 instructions an item
    of a ten-item list, CPython 424). Its running totals of machine ints or
    floats are computed at once now, the adapter committed as its last step
    leaves it only when every total is native (359). And `zip` of three to
    eight list or tuple cursors checks and steps them in one pass, not two
    (`for x, y, z in zip(a, a, a)` 1,064 instructions a step to 1,025).
  - A compiled call of a `functools.partial` read the partial's stored
    function, arguments and keywords up to three times (for a pure leaf,
    then a builtin, then a bound call), each through a thread-local and a
    slot-layout check: they're read once now and the path chosen from the
    function's kind (`partial(f, 1, c=3)()` 3,021 instructions to 2,766,
    CPython 2,191).
  - `f.read(4096)` of a buffered binary file took the full native method
    (its argument parsing, checks and dispatch layers, 1,700 instructions
    beside the copy) even when the bytes were in the read-ahead already.
    The method's leaf half now serves such a read from the read-ahead, an
    open, readable file with nothing written to flush (2,635 instructions
    a read on average to 1,932, half the reads refilling the buffer).
  - The macOS `torch` probe in CI twice panicked with "cell is already
    borrowed" in the builtin-method memo, a cache shared by the whole
    process. Its lookups now go without it when it is borrowed, and a
    store that finds it borrowed is skipped, so no lookup can panic there.
  - A `re` search whose pattern starts with a literal stepped the subject
    one unit at a time to the prefix's first character, with a bounds and
    an end check at each (about ten instructions a byte; `search` of a
    four-byte literal 254 bytes into a bytearray 4,434 instructions,
    CPython 3,074). Over bytes, and text matched as its ASCII bytes, that
    scan is `memchr` now (1,963).
  - `str.rfind` stepped back one candidate first byte at a time (2.48
    million instructions for `text.rfind("line 5 ")` over 200 KB whose
    match is near the start, CPython 0.83 million), and `bytes.rfind` used
    the reverse substring searcher, which has no vector prefilter. Both run
    the forward searcher over windows taken from the end now, 256 bytes and
    then four times larger each time, overlapping by the needle's length
    less one (134,000); one-byte needles use `memrchr`, and short
    haystacks a plain scan.
  - `BytesIO.tell()` and `seek(n)` took the full native methods (925 and
    1,119 instructions a call, CPython 340 and 459): an open `BytesIO`
    with nothing read ahead or written to flush is served by leaf halves
    now, `seek` for an absolute position not below zero (574 and 608).
    And every compiled `f.method(...)` of a native stream first tried the
    fused local-method call, which serves instances and containers only,
    and then found the stream's method past the slow loader's other lanes:
    other receivers decline the fused call at once now, and the loader
    reads a stream's cached method first (`BytesIO.write(b)` 968
    instructions to 889, `read(64)` 881 to 763, `tell()` 574 to 490;
    CPython 627, 545 and 340).
  - A `for` loop over a csv reader reached its native `__next__` through
    the interpreter on every row, since the reader may call Python (its
    input iterator, or `float` for the numeric quoting styles): 8,460
    instructions a row for a five-field line from a list, CPython 4,250.
    The reader has a leaf half now. It takes a line from a list or tuple
    iterator, or from a text stream that holds it in its buffer, when the
    line is one whole record. A line of plain and quoted fields is cut
    into its fields directly, with `memchr` finding each delimiter and
    closing quote, and any other line runs through the parser in place;
    the stream's line is consumed only once the record is read. Rows cost
    3,150 from a list, 3,810 for quoted fields (9,020 before, CPython
    4,590), 3,670 from a `StringIO` (9,020, CPython 5,000), and 3,890
    from a file (9,230, CPython 5,040).
  - The rows of `product`, `permutations`, `combinations` and
    `combinations_with_replacement` were built by cloning each item out of
    its pool through an iterator, which moves every object by way of the
    stack (126 instructions for a three-item row). Rows gather their items
    by address now and copy each into the tuple whole (85), so
    `list(combinations(range(8), 3))` costs 24,950 instructions on x86
    (27,250 before).
  - A `for` loop over `struct.iter_unpack("<I", b)` cost 1,100
    instructions a record (CPython 560). The iterator's leaf half read its
    slots under one borrow, cloned the format and buffer out, and then
    borrowed the slots again to move the offset; it reads and moves them
    under a single borrow now. And a dropped tuple was cleared slot by
    slot for the tuple free list before the list was found full, which a
    loop that only drops tuples keeps it: a full bucket takes nothing now,
    and a tuple of scalars is freed as it is (one holding objects still
    releases them last first, as CPython's tuple does). Records cost 900
    (756 on x86, 971 before).
  - `bytes.startswith` and `endswith` (and `bytearray`'s) went through
    the full method's arity and window checks on every call (922
    instructions for `ba.startswith(b"\x00\x01")`, CPython 600). One
    `bytes` or `bytearray` affix with no window runs as a leaf half now.
  - UTF-16 encoding of text that isn't all ASCII pushed each unit of
    `encode_utf16()` onto a growing vector, and decoding pushed each
    character of `char::decode_utf16` onto a `String`: 316,000
    instructions to encode `"héllo wörld " * 1000` (CPython 30,000),
    326,000 for 8,000 CJK characters (84,000), and 448,000 to decode the
    first (57,000). The encoder sizes its output exactly now (characters
    counted a lane of bytes at a time), spreads eight ASCII bytes into
    their units at once, and writes the rest from the UTF-8 directly; the
    decoder takes four ASCII units at once and writes each character's
    UTF-8 itself (176,000, 260,000 and 184,000). UTF-32 encoding sizes its
    output once too (251,000 to 205,000, CPython 32,000). CPython keeps
    such text as one or two bytes a character, so its codecs copy units
    where these decode UTF-8.
  - `with f.getbuffer() as v` (or any `with` over a `memoryview`) loaded
    `__enter__` and `__exit__` through the generic attribute path, which
    built a fresh method object and a bound method for each, and called
    them through the general native lanes: 7,700 instructions, CPython
    2,400. A view's methods are shared per name now, as a file's are, and
    `LOAD_SPECIAL` of a file or view leaves the type's method and the
    receiver for the call, in compiled code too (3,260; `getbuffer()`
    with its block 3,490, 8,490 before).
  - `zlib`, `bz2`, `lzma` and `compression.zstd` kept every compressor's
    and decompressor's native state in a table keyed by the handle on the
    instance, and never let one go: 100,000 `zlib.compressobj()` calls
    left 6.4 GB resident (CPython 11 MB), 100,000 `decompressobj()` calls
    1.7 GB, and 20,000 `BZ2Compressor()` 118 MB. The tables are one type
    now (`stdlib::handle_table`): the classes' instances let their state
    go as they die, and the states of any that died unseen (a subclass's,
    one on another thread) go when the table has doubled since its last
    sweep. The same loops peak at 9.5 to 27 MB, under CPython's 11 to
    29 MB. (A compiled regular expression's program is still kept for
    good, about 1 KB a distinct pattern: the matcher reads programs
    through `'static` references from a lock-free table.)
  - `hashlib` kept every hasher's state in a process-wide table it never
    pruned: 300,000 short-lived `sha256()` objects left 156 MB resident.
    A hasher's state goes with it now (9.6 MB). A method call on a hasher
    went through the dispatch loop's general native lanes (5,850
    instructions for `h.update(16 bytes)`, CPython 1,090); `update`,
    `digest` and `hexdigest` have leaf halves now (1,330), and
    `sha256(b"hello world").hexdigest()` costs 8,690 (10,220, CPython
    9,910). SHA-256, SHA-384 and SHA-512 run on `ring`, already linked
    for TLS, whose assembly takes the CPU's vector or SHA extensions:
    518 MB/s for SHA-256 on this Intel Mac, which has no SHA extensions
    (283 before, CPython's OpenSSL 563), and 743 for SHA-512 (530, 843).
    `ring`'s SHA-1 is slower than the portable one, so SHA-1 stays.
  - A `partial`'s fields were read through a thread-local holding their
    names even when the slot layout the call remembers answers without
    them; the remembered layout is tried first now (a few dozen
    instructions a call).
  - `f.readinto(b)` of a buffered binary file gave the read-ahead back to
    the descriptor (a seek), read `len(b)` bytes into a fresh vector with
    its own system call, and copied them over: 7,800 instructions a call
    for a 4,096-byte `bytearray` (CPython 2,300), where `read(4096)` is
    served from the read-ahead (2,500). A read of less than half the
    buffer goes through the read-ahead now, one `read(2)` refilling it a
    buffer at a time with the target borrowed only to copy each run, and
    a leaf half copies from the read-ahead when it holds the whole target
    (3,000; 2,200 into a `memoryview`, which cost 8,000).
  - `BytesIO.getvalue()` of an unchanged buffer returned its cached value
    after taking a guarded borrow of the stream's state, its buffer, and
    the cache each (730 instructions a call, CPython 350). The leaf half
    reads them through guard-free views first now (575).
  - A loop over a list of objects (instances, strings, tuples) that tier
    2 entered by OSR pinned each element in its activation's table, which
    holds them until the activation leaves: `for p in pts: pass` cost 290
    instructions an element where the frame JIT, which counts references
    instead, takes 39. Tier 2 declines such a loop now unless the loop
    itself has enough typed arithmetic or makes a typed Python call to
    repay the step (`richards`' `for t in tasks: t.run()` keeps it; a call
    that built the list doesn't count), as it already declined
    tuple-target loops. Over 512 instances, `t += p.x` costs 164
    instructions an element (550 before, CPython 325), `out.append(p)` 173
    (478, 233), `if p is not prev` 211 (468, 408) and `t += p.norm()` 396
    (495, 1,219); the `float_math` fixture runs 4% fewer instructions. The
    pins' reuse memo screens its entries by an identity tag too, so a miss
    reads no pins (a loop over 4,096 repeated instances in tier 2, 427 to
    331).
  - Every instance's death scanned the classes whose instances own native
    state (the compressors and hashers) once one had registered, which
    importing `zlib` or `hashlib` does: 52 instructions an instance. A
    per-class bit screens the scan now, and the class is read without a
    borrow (a dying instance has no other owner).
  - `obj in s` for an instance hashed by identity (a plain class's, its
    `__hash__` `object`'s) went through the leaf membership test's general
    path, which asked twice whether the item hashes natively and compared
    the found key out of line: 767 instructions a test in a loop over a
    set (CPython 282), 659 over a dict's keys. The hot path finds such an
    instance natively now, by its pointer hash and identity (443 and 342;
    a miss 568 to 327, CPython 233).
  - A `__hash__` of the usual shape, `return hash((self.a, self.b))` (a
    frozen dataclass's too), ran as a leaf plan that built the tuple, called
    `hash` through the general builtin lane and hashed the tuple: 2,077
    instructions for `hash(k)` (CPython 1,794). The plan translator fuses a
    `CALL` of the global `hash` on the tuple the instruction before built
    into one operation that hashes the items as the tuple would, with no
    tuple, when the callee is still the builtin (anything else, or an item
    whose hash could run code, declines to the full call). `hash(k)` costs
    1,380, a frozen dataclass's 1,238 (1,933, CPython 1,773); `k in s` 1,468
    (2,160, CPython 1,505), `d[k]` 1,778 (2,466, 1,530), `set(keys)` of 64
    such keys 1,533 a key (2,234, 1,349), and a dict of dataclass keys read
    with equal keys 3,397 (4,102, 2,669). A loop testing 64 such keys
    against a set costs 1,541 a key (3,007, CPython 1,449), the rest from
    leaving that loop to the frame JIT (see the loops over objects above).
  - A compiled leaf plan's read of a `__slots__` member ran every check of
    the split-dictionary read first, failed them, and then loaded the
    instance, its class and the site's entry again for the slot read. The
    entry's slot bit picks the slot read first now, on the values already
    loaded: about 38 instructions a read. A slotted class's `hash(k)` with
    `hash((s.x, s.y))` costs 1,326 (1,380, CPython 1,792); a dict read with
    an equal key, which runs `__eq__`'s four reads, 2,764 (2,976, 2,453);
    and the loop testing 64 keys against a set 1,428 a key (1,541, 1,407).
  - A dataclass's `__eq__` tests `other.__class__ is self.__class__`. A
    compiled leaf plan read each `__class__` through a helper that put a
    new reference to the class in the evaluation's scratch, then tested
    the two with another helper (about 290 instructions with the
    scratch's release). The translator fuses the two reads of locals and
    the test into one operation that compares the classes of two
    instances whose classes keep `object`'s `__class__`, and reads them as
    before otherwise. A dict read with an equal dataclass key costs 3,150
    (3,393, CPython 2,664), and `a == b` of two dataclasses 1,243 (CPython
    1,435).
  - A call of a `partial` with two or more stored arguments (one goes in
    the call's empty self slot instead) bound them through the general
    keyword binder, walking the partial's empty keyword dict, and took a
    scratch vector and built a four-slot buffer it used only on other
    paths: 1,807 instructions for `partial(g, 1, 2)(i)` (CPython 908). A
    call with no keywords that passes exactly the parameters binds them in
    order now, and the vector and buffer are made only where they're used
    (1,638; a partial with a stored keyword 1,849, CPython 2,038).
  - Exceptions printed and inspected took the generic paths:
    - `str(e)` and `repr(e)` built a bound method for the native
      `BaseException.__str__` and `__repr__` and called it through the
      full call machinery, and `f"{e}"` left the frame JIT for the
      interpreter. The class's native method is now remembered per class
      version and called with the instance. `str(e)` and `f"{e}"` compute
      the text in place when every argument's text is native (a string,
      a number, bytes, `None`), checking `KeyError` by class identity
      rather than by comparing names along the MRO.
    - A frame-JIT site reading `e.args`, `e.__context__` or another
      exception pseudo-slot reached its lane only after every other
      instance lane missed, and an unset slot's default walked the MRO.
      Such a site now reads the slot first, at its laid-out place, with
      the default remembered.

    Instructions per operation (CPython 3.14 in the last column):

    | Operation | Before | Now | CPython |
    |---|---|---|---|
    | `str(e)` | 2,347 | 1,003 | 440 |
    | `f"{e}"` | 2,198 | 658 | 866 |
    | `repr(e)` | 3,556 | 2,859 | 2,295 |
    | `e.args` | 663 | 336 | 424 |
    | `e.__context__`, unset | 869 | 352 | 412 |

    Two old faults went with it: a lone argument whose `__str__` raised
    gave `str(e)` its repr instead of raising, and `f"{e}"` of an
    argument with a lone surrogate came out as a private-use character.
  - Loops that unpack three or four items, and tier 2's dict loops, took
    the general steps:
    - `for a, b, c in zip(xs, ys, zs)` built (or refilled) a result
      tuple, then unpacked it through the generic helper, and `for a, b,
      c in rows` stepped the list and unpacked each row the same way. The
      frame JIT fused only `FOR_ITER` with an `UNPACK_SEQUENCE 2`. It now
      fuses three and four as well: a `zip` of plain sequences, or a list
      or tuple whose next item is an exact tuple of that size, writes the
      items straight onto the stack.
    - A tier-2 loop over a dict's int keys or values went through two
      helpers and the interpreter's dict step for each key. It now steps
      the dict in the scalar lane, with the same checks.

    Instructions per step:

    | Loop | Before | Now | CPython |
    |---|---|---|---|
    | `zip` of three lists | 753 | 280 | 370 |
    | a list of triples | 568 | 167 | 167 |
    | a dict's int keys, tier 2 | 281 | 130 | 130 |
  - A float's `e` and `g` formats formatted it twice: the shortest digits
    first, padded with zeros if they fit, then the exact digits when they
    didn't (any value with a long mantissa, `1234.5678`). The shortest try
    is for round values (`0.5`, `100.0`), whose exact digits sit on a
    rounding boundary the fast exact algorithm can't settle, so it's now
    made only for a value whose mantissa ends in many zero bits. A spec of
    a number no longer computes its `str` first (only the string
    presentation reads it), and the exponent's normalizing and the
    unsigned result's sign make no extra copies. `format(1234.5678, 'e')`
    takes 2,935 instructions (6,283, CPython 3,153), and `'g'` 2,884
    (4,817, CPython 2,459).
  - Every one-byte `bytes` result was a new allocation (CPython shares
    one object per byte value, `bytes([97]) is b"a"`), so splitting into
    short pieces allocated each one. Those objects are now shared as in
    CPython. A compiled `sep.join(parts)` of `bytes` went through the
    general call, which the dispatch takes for `join` because an
    arbitrary iterable may run code. A list or tuple argument now calls
    the native body, which reads the parts in place.
    `b"a b c d".split(b" ")` takes 1,596 instructions (2,078, CPython
    1,098), and `b" ".join([b, b])` 1,824 (2,146, CPython 1,307).
  - `obj[key]` of a plain instance whose class's `__getitem__` is a Python
    function called the method through the frame JIT's direct-call
    records, where `obj.__getitem__(key)` had its body lowered in line,
    and tier 2 took a loop with such a subscript and an `int` sum,
    calling the method through the interpreter's subscript. A subscript
    site now records the method it called. The frame JIT lowers it in
    line, as it lowers an operator method, under a guard on the class's
    version and with no guard on the key; the method's `NotImplemented`
    is a value like any other. Tier 2 counts such a subscript as a
    dynamic Python call when it weighs a loop's typed work. `seq[1]`
    takes 193 instructions (888, CPython 516), and `t += seq[1]` 237
    (1,376, CPython 699).
  - Tier 2 likewise took a loop that counted `if a < b` of instances with
    a Python `__lt__`, making each comparison through the interpreter's
    rich comparison (helper, callee guards, then the method), about 1,000
    instructions. A comparison at a site that has seen instance operands
    now counts as a dynamic Python call too, so such a loop runs in the
    frame JIT: 817 instructions a step (1,448, CPython 1,177).
  - `deque.rotate` found the deque's list, head and state counter through
    the guarded slot path, by name each time, where the end operations
    take them unguarded in one pass over the slot layout. It now takes
    them that way too: `d.rotate(1)` costs 636 instructions (982,
    CPython 542).
  - A property setter shared by several classes left compiled code at
    every store after the first class: the setter's own `self._p = v`
    took a `__slots__` member's store only through its site's cache, so
    an instance of another slotted class (or of one that also has a
    `__dict__`) declined, ran the setter as a frame, and finished the
    iteration interpreted. The everyday battery's `class_features` runs
    its case three times, each defining its classes afresh, so two runs
    in three went that way. A store the cache doesn't serve now takes
    the member's place when the class stores the name in a `__slots__`
    member by the default `__setattr__` (per class version), and the
    in-line `__getitem__` (above) falls back to the site's direct call,
    not to an exit, when its class guard fails. `class_features` takes
    479 million instructions (505). The fallback first ran its verdict
    for every declined store, a property's included, which cost the
    bench gate's `richards` 2% here and about 16% on CI's runners. It
    now looks for the name in the class's slot layout first, and a store
    it makes records the site's slot shortcut, so the next one takes the
    cached path: a property set `o.p = i` costs 1,192 instructions
    (1,362 before these changes).
  - Two more sites left compiled code on every run of a battery case: a
    `BUILD_STRING` of more than eight parts (`UUID.__str__`'s f-string
    has nine), and `urljoin`'s `segments[1:-1] = filter(None,
    segments[1:-1])`, whose `STORE_SLICE` took only a list or tuple. The
    compiled join now takes up to sixteen parts, and a slice store drains
    a `filter(None, ...)` or `filter(bool, ...)` over a plain list or
    tuple in place when its items' truth runs no code.
  - `for i, x in enumerate([a, b])` left compiled code at its end on
    every run (`_strptime` begins with such a loop): an `enumerate` only
    the stack held, over a temporary list only it held, declined to
    retire at its end, though a plain list cursor in the same position
    retires and frees its plain items. It now retires as that cursor
    does: a call of a function with that loop costs 3,342 instructions
    (4,904, CPython 2,658), and the battery's `datetime_time_ops` 1.5%
    fewer.
  - A keyword call of a native method cost two to three times its
    positional form (`x.to_bytes(2, byteorder="big")` 2,038 instructions,
    `x.to_bytes(2, "big")` 687). The compiled call's lane for a native
    callee now runs the body on its operands where they sit on the stack,
    as a positional call's does, and calls the body directly when the
    interpreter's dispatch would only end up there; a keyword half (the
    registered builtins that bind keywords by position) keeps each site's
    binding in a small per-thread memo instead of matching names on every
    call; and `to_bytes`, `from_bytes`, `str.rsplit` and `str.splitlines`
    have halves. `x.to_bytes(2, byteorder="big")` takes 1,313
    instructions (CPython 1,046), `s.splitlines(keepends=True)` 1,714
    (2,513, CPython 1,394), `sorted(xs, reverse=True)` 1,742 (1,989,
    CPython 2,153), and tier 2's `dict(a=1, b=2)` 1,542 (1,748), its check
    now ahead of the function binders that always declined it. Three
    differences from CPython turned up and are fixed: a list released its
    items first to last (CPython's `list_dealloc` goes last to first),
    `to_bytes` and `from_bytes` took `signed` positionally, and both
    ignored an unknown keyword.
  - `getattr(obj, name, default)` of a missing attribute took the full
    call on a module (7,015 instructions, CPython 1,039), which built and
    formatted the `AttributeError` it then dropped, and on a string or
    container (2,062 and 2,039), whose method lookup missed and declined.
    The compiled call's leaf now answers a module's miss with the default
    when the module has no `__getattr__` (885), and a string's or
    container's with it once the full path has recorded the miss for the
    class, as it already did for other built-in values (768 and 764);
    `hasattr` of a module takes the same answer. `int.bit_count`,
    `bytes.find` and `bytes.hex` became leaves too, and `b.hex(":")`
    writes its separator as it writes the digits.
  - `sum(xs)` of a list or tuple with floats declined the compiled call's
    leaf, which took only machine ints, and went through the full call,
    whose switch from the integer loop to the compensated float loop
    added through the generic binary operation: `sum([1.5, 2.5, 3.5])`
    cost 1,746 instructions (CPython 1,331). The leaf now falls back to
    the numeric sum, which runs no code, and the switch adds the float
    directly: 649.
  - `max(d, key=d.get)` and `sorted(xs, key=abs)` called their key per
    item through the full call (4,990 and 3,825 instructions, CPython
    3,072 and 2,770): a builtin or bound method the leaf registry vouches
    for (`abs`, `d.get`, `str.lower`) now computes the keys natively, and
    `min` and `max` take them when they're all ints, all floats or all
    strings (2,916 and 2,920). `types.SimpleNamespace(a=1, b=2)` took the
    full constructor (4,792, CPython 2,760) and now builds its dictionary
    from the keyword names as `dict(a=1, b=2)` does (2,300);
    `d.update(a=1, b=2)` stores the name objects through the leaf's
    insert (3,031 to 1,796, CPython 1,882).
  - `s.split()` on whitespace sized its list for a quarter of the text's
    length, so `"a b c d"` grew it twice, and allocated every field: a
    one-character field is now the shared string, as CPython's is (and
    as the separator split's already was), and a short string's list
    takes its fields' bound, half its length. `"-".join(s.split())` went
    from 1.44 times CPython's instructions to 1.27.
  - `operator.neg`, `pos`, `invert` and the six comparisons had no leaf
    halves (the arithmetic functions did), so `sorted(xs,
    key=operator.neg)` called the key per item through the full call:
    4,743 instructions against CPython's 2,891. A plain int or float now
    takes the native operation in place (2,930), and `operator.lt(i, 5)`
    costs 410 against 601. Float receivers join the leaf method table,
    so `x.is_integer()` and `x.as_integer_ratio()` run in place (568 to
    456, CPython 406).
  - Tier 2 loaded a global object (`type`, `int`, a module) through its
    helper at every use, about 40 instructions a load each iteration,
    though the helper pins one object per activation. The compiled code
    now keeps the pin it got for the rest of the run: `type(x) is int`
    in a tier-2 loop went from 693 instructions to 635.
  - Compiled code runs a local list's, dict's, set's or str's method in
    place through a kernel when it has one (`append`, `get`, `strip`),
    and otherwise took the fused method call, which decodes the call's
    instructions and finds the method again on every call. A method with
    a fast half (`sort`, `dict.pop(k, d)`, `str.encode`), `list.pop(i)`,
    and a body that's a leaf whole now go through the native method site
    instance methods use, the receiver's tag checked in line: on a short
    list `xs.sort()` went from 1,728 instructions to 1,377 (CPython
    1,298), `xs.pop(0)` from 1,484 to 1,237, and `d.pop(k, None)` from
    616 to 319. The sort itself skips the run stack (an allocation) for
    a list shorter than a minimum run, as CPython's does.
  - A native classmethod called through its class
    (`itertools.chain.from_iterable(xs)`) left compiled code for the
    interpreter on every call: the builtin lane admitted a bound native
    method only over an instance. A native function bound to a class now
    runs through the lane's full call, the class first:
    `list(chain.from_iterable(xs))` went from 6,280 instructions to 3,620
    (CPython 3,475).
  - `struct.iter_unpack` was three Python frames before the iterator
    existed, `list()` of it called its Python `__length_hint__` and found
    the native step again on every item, and its end ran the Python
    `__next__` just to raise `StopIteration`. A `bytes` or `bytearray`
    buffer's iterator is now built natively, the hint is native, the end
    raises natively, and `list()` of an iterator whose `__next__` has a
    native fast half calls the half in a loop: building one went from
    5,663 instructions to 1,463 (CPython 1,215), and `list()` of a
    two-record one from 18,602 to 8,284. A `str` buffer is now refused
    when the iterator is made, as CPython refuses it, not at its first
    step.
  - `itertools.zip_longest` had no step that runs no code, so each item
    copied the iterator's state (its sources' vector) to protect it from
    reentrant code. Over plain list and tuple cursors a step now reads
    each source's next item, or the fill value, in place, lets go of a
    source that finishes as the general step does (when releasing it
    runs nothing), and ends as it ends: `list(zip_longest(a, b))` of a
    three-item list and a one-item list went from 10,567 instructions to
    7,961 (CPython 4,403).
  - Importing a module decoded each code object's names through the
    process-wide name pool (a lock and a hash of the text) even when the
    module's other functions had decoded the same name (`self`), hashed
    each name's text again for the code's lookup table though the pooled
    string caches its hash, and checked every string constant's whole
    text for ASCII before finding its first non-name character (a
    docstring's first space). A module's names are now pooled once per
    module, a code object's names find their interned strings through a
    small memo keyed by the pooled name, the hashes come from the
    interned strings, and the constant check stops at its first non-name
    character: `import dataclasses`, `inspect`, `logging`, `json` and
    `typing` took 2.8% fewer instructions together (1.31 to 1.27 times
    CPython's). Registering a native builtin as a leaf also swept the
    whole registry for dead entries each time, which made a module's
    registrations quadratic (about 4,000 instructions each by the time
    `datetime` registers its own); the sweep now runs when the registry
    has doubled.
  - A scan of the idiom sweeps for sites where compiled code leaves for
    the interpreter on every iteration found four more. Set algebra
    (`a | b`) ran in the core loop's set lane, which didn't note its
    heap result, so the site compiled as a number site and left after
    every set; it notes it now (1,647 instructions to 1,487, CPython
    1,298). Iterating an `io.StringIO` left at `GET_ITER`, which knew
    only iterators and builtin sequences: an open file is now its own
    iterator there (8,658 to 7,522). `copy.deepcopy` of a plain tree and
    `Counter.most_common(n)` call native helpers that weren't
    registered as leaves, so each call left: registered, `deepcopy` of
    a small dict went from 9,819 to 4,265 (CPython 18,346) and
    `most_common(1)` from 7,547 to 2,690 (CPython 6,791).
  - A method whose first call from a site changes its class (a class
    attribute set lazily, as `string.Template`'s compiled pattern is)
    left that site without a method slot: the core loop's fused call
    took the calls after, and compiled code's method load, finding no
    slot, left for the interpreter on every call. The load now resolves
    a plain function by the class's own lookup and keeps it in the
    site's in-line cache: such a call went from 2,491 instructions to
    357 (CPython 654), and `Template.substitute(a=1, b=2, c=3)` from
    27,396 to 24,899.
  - A sixth idiom sweep (`target/mb/i6/i6.py`, 62 cases) ranked the
    next gaps. `sorted(xs, key=cmp_to_key(f))` made a key object per
    item and ran a Python `__lt__` per comparison; a sort whose key is
    the frozen `functools` key factory (an immutable class now, as
    CPython's `KeyWrapper` is) compares the items with `f(a, b) < 0`
    directly, a pure-leaf `f` in place: three items 33,160 instructions
    to 6,509 (CPython 8,470). A dict subclass's miss (`Counter`'s
    `c[w]`) left compiled code to call `__missing__`: the subscript's
    direct-call lane now calls a plain Python `__missing__` when a `str`
    or `int` key is settled absent, and the core loop marks the site so
    `c[w] += 1`, whose copies of `c` and `w` the lane writes out first,
    compiles with it (`Counter()` and nine increments 52,722 to 19,942,
    CPython 23,663; one miss 4,813 to 805, CPython 1,045).
  - `(k, v) in d.items()`, `v in d.values()` and `k in d.keys()` answer
    in compiled code when no comparison can run Python (1,773 to 689,
    CPython 860), and an unhashable key there raises `TypeError` as
    CPython does (it answered `False`). `zip(*rows)` and other builtin
    types called with a spread build in place instead of leaving at
    `CALL_FUNCTION_EX` (3,722 to 1,627, CPython 1,986). `list()` of
    `pairwise` or `batched` over a list or tuple takes the windows at
    once (eight items 8,657 to 5,300 and 9,085 to 5,137, CPython 4,681
    and 4,250), and `batched` steps such a source in place. `pairwise`,
    `batched` and `zip_longest` now step a finished list iterator to its
    end, so one the caller still holds sees no later appends, as in
    CPython (it returned them).
  - `list.extend()` and `set.update()` over a generator or an iterator
    class collected every item before adding any, so a generator that
    read the receiver saw it unchanged (`xs.extend(len(xs) for _ in
    range(3))` gave `[0, 0, 0]`, CPython `[0, 1, 2]`). They now add
    each item as it comes; a generator's lean resume moves each yield
    straight into the list, or into the set when it hashes with no code
    run, which is also faster: nine items 8,384 instructions to 7,820
    for `extend` and 11,962 to 8,523 for `update` (CPython 5,371 and
    6,402). A `functools.partial` over a builtin type with stored
    keywords (`partial(int, base=16)`) took the generic call; its
    keywords now fill the parameters they name for the type's native
    constructor, as a keyword call of the type does: 4,835 to 1,861
    (CPython 3,171). `list(map(f, xs))` with a leaf builtin `f`
    (`str.upper`, `abs`) resolves its body once instead of per item,
    steps a list or tuple source in one view, and an unbound method's
    receiver check takes an exact builtin value by its type's address
    (four words 7,561 to 6,654, CPython 4,066). `list()`, `tuple()`,
    `set()` and `sorted()` of an enum class whose metaclass iterates it
    with `enum`'s own `EnumType.__iter__` read the members from
    `_member_map_` in `_member_names_` order, as that generator yields
    them, instead of calling it and resuming the generator per member
    (none while a tracer or profiler would see it): `list(Color)` of
    two members 11,311 to 3,297 (CPython 5,731). A compiled loop's
    `GET_ITER` of such a class, which left for the interpreter, now
    takes `map(cls._member_map_.__getitem__, cls._member_names_)`, the
    same items read as lazily from the same live tables (a member added
    mid-loop is still seen), stepped in place: `for m in Color` over
    three members 11,362 to 5,932 (CPython 4,580). `len()` of a class
    whose metaclass `__len__` evaluates as a pure leaf runs it in place
    instead of through the builtin lane and a bound method: `len(Color)`
    3,260 to 2,127 (CPython 1,000).
  - A seventh sweep (`target/mb/i7/i7.py`, 58 stdlib calls) found
    native functions taking the general call. `math.prod` and
    `math.fsum` of a list or tuple of plain numbers have fast halves
    (`prod` of four ints 6,766 to 1,345, CPython 1,475), and the
    `unicodedata` functions, whose bodies only read the character
    database, are leaves (`category("a")` 3,772 to 790, CPython 755);
    their argument errors now read as CPython's. A `csv.reader` built
    its default dialect from fresh string keys every call: the keys are
    built once, and a reader with no dialect or format parameters shares
    one default dialect (15,112 to 5,665, CPython 3,631). A store into a
    `SimpleNamespace`'s attribute left compiled code: it goes into the
    namespace's dictionary in place (1,409 to 496, CPython 542).
  - A reader's row step has a leaf half, but a new reader declined it
    until its first row had run the full parser, and `list()` stops
    trying a fast half once it declines: every row of `list(csv.reader(
    lines))` took the full path, and the end raised through it. A new
    reader now starts with no row in progress, and the half steps a
    list's or tuple's iterator to its end itself: two rows 30,186 to
    16,881 (CPython 7,911). `os.fspath()` of a `pathlib` path whose
    class keeps `pathlib`'s own `__fspath__` and `__str__` returns its
    cached string in place (4,183 to 2,021, CPython 1,898). The
    `binascii` encoders and decoders, whose bodies run no code over a
    `bytes`, `bytearray` or `str` argument, took the general builtin call
    unless called with keywords: they have fast halves now, for keyword
    calls too (`b2a_base64(b)` 4,054 to 1,081, CPython 1,113;
    `base64.b64encode` 3,184 to 2,150 and `b64decode` 4,541 to 3,428,
    CPython 1,938 and 3,016).
  - A function made with a dict subclass's `__missing__` as its globals
    raises a gate under which every builtin lookup takes the slow path,
    and `annotationlib` makes one whenever it evaluates annotations with
    forward references (`typing.NamedTuple`, a dataclass's string
    annotations). The gate drops when the owners die, but only the full
    lookup counted toward pruning them, and only a finalizer drain
    pruned: a loop that ran its builtins through the fast paths kept the
    gate up for good, so defining one `NamedTuple` made every leaf call
    that reads a builtin take a frame (`hash(q)` with a Python
    `__hash__` 1,436 instructions to 2,964). The fast paths' gate now
    counts too, and every 1,024th ask prunes the owners nothing holds
    in place (letting one go only queues its finalizers): `hash(q)` in
    the protocol sweep 2,930 to 1,414 (CPython 1,066), a frozen
    dataclass's `hash(f)` 3,405 to 1,363 (CPython 1,755). An owner in a
    reference cycle still waits for the next collection.
  - An eighth sweep (`target/mb/i8/i8.py`, object-oriented idioms) found
    property setters that check their value first (`if v < -273: raise
    ValueError` before `self._c = v`) taking a frame on every store: the
    store's leaf lane took only the plain `self._x = value` shape. Any
    setter that is an effect leaf now runs in place through the general
    evaluation, its stores committed at the return (`t.c = i` 3,988 to
    1,405, CPython 1,916). A method load that misses on an instance tries
    a pure-leaf `__getattr__` in place, as an attribute read does. Such
    a hook is usually `return getattr(self._target, name)`, and leaf
    evaluation refused any call of `getattr`: it now admits it, since
    its leaf body answers only what runs no code (or a pure-leaf
    getter) and declines the rest. A wrapper's delegated `w.count(1)`
    went from 9,598 to 3,066 (CPython 2,001). A `TypedDict` class's
    metaclass sets `__call__ = dict`, and a type binds no receiver, so
    `TD(a=1)` is `dict(a=1)`; compiled code left the loop for it each
    call. A class whose metaclass's `__call__` is a builtin type is now
    called as that type (5,472 to 1,882, CPython 2,230).
  - A Python `__setitem__` can't run as a leaf (leaf evaluation buffers
    attribute stores but not item stores), so every store into a mapping
    wrapper took a frame: `c[k] = v` with `self.data[k] = v` as the
    method cost 3,414 instructions (CPython 1,083). A class whose
    `__setitem__` is exactly `self.<attr>[key] = value; return None`
    (`UserDict`'s, a wrapper's), or `self.<attr>[<i>][key] = value` for
    a small int (`ChainMap`'s `self.maps[0]`), now stores into that dict
    in place when the store runs no code (1,256; `UserDict` 1,257,
    CPython 1,012; a `ChainMap` 3,527 to 1,394, CPython 1,212).
    `x in SomeEnum` ran `EnumType.__contains__` in a frame (its `try`
    keeps it from being a leaf); when the metaclass keeps `enum`'s own
    method, the membership test now answers as it does, in place: a
    member of the class, else a scan of the class's value lists (a
    `Flag` class, whose `_missing_` runs, takes the call), and
    `SomeEnum[name]` reads the member map the same way, each verdict kept
    per class and metaclass version. `Color.RED in Color` 2,730 to 870
    (CPython 1,244); `Color["RED"]` 1,333 to 514 (CPython 612).
  - Arithmetic and comparisons over an instance of an `int` or `float`
    subclass (`IntEnum.A + 1`, `I(2) < 3`) left compiled code at every
    operator for the general path, about 3,500 instructions each
    (CPython about 400). When the class takes the operator's forward
    and reflected methods from its built-in base unchanged and defines
    no in-place one (a verdict kept per class version), CPython runs the
    base's own slot over the payload, so compiled code now runs the
    operator over the payloads through the numbers' own arms: `a + 1`
    3,528 to 497 (CPython 403), `a < 2` 3,439 to 537 (CPython 416).
    `IntFlag`'s own `|` and any Python override still take the call.
  - Text of `str`, `int` and `float` subclass instances took the general
    path. An f-string looked up and bound the base's `__format__` and
    called it through the full call path (about 3,000 instructions;
    CPython about 1,300), and `str()` repeated its lookups on each call.
    Two verdicts kept per class version now settle them. The first is
    whether `str(x)` is the payload's text: a `__str__` that is the
    base's own method stored in a Python class (`IntEnum`'s and
    `StrEnum`'s), or a number's `__str__` and `__repr__` both from the
    base. The second is whether `__format__` is the payload type's own.
    An enum member whose class keeps `enum`'s own `Enum.__str__` (and
    `Enum.__format__`) renders `Class.name` from its dict the same way
    (unless a tracer or profiler is watching). `str.join` reads `str`
    subclass items as their payloads, as CPython's join does, and `%`
    with bare `%s` conversions and the `!s` conversion that compiled
    `"%s-%s" % (a, b)` becomes take these texts too. `f"{I(5)}"` 3,855
    to 753 (CPython 1,475); `f"{StrEnum.A}"` 3,106 to 766 (CPython
    1,227); `str(I(5))` 2,743 to 765 (CPython 879);
    `f"{Color.RED}"` 14,818 to 1,447 (CPython 3,130); `str(Color.RED)`
    3,783 to 1,255 (CPython 1,942); `"-".join` of `StrEnum` members
    3,385 to 1,177 (CPython 1,185); `"%s" % StrEnum.A` 2,399 to 1,023
    (CPython 1,031).
  - Enum members as dict keys hashed through `Enum.__hash__`
    (`hash(self._name_)`), a leaf evaluation on every probe, and
    `member.name` and `member.value` called `enum.property.__get__`,
    which calls the getter. A member whose class keeps `enum`'s own
    `__hash__` now hashes its `_name_` in place. When the descriptor's
    class keeps `enum`'s own `__get__` and its getter is `Enum.name` or
    `Enum.value`, the read takes the member's `_name_` or `_value_` from
    its dict. Both decline while a tracer or profiler is watching. A
    function returning `d[member]` went from 1,896 to 1,337 (CPython
    1,437), one returning `member.name` from 2,798 to 1,798 (CPython
    1,556), and one returning `member.value` from 2,116 to 1,773 (CPython
    1,553).
  - `array.array` is a Python class whose item methods the `_struct`
    module serves natively, but its construction still ran the Python
    `__new__` (with `*rest` and `**kwargs` packed), `_setup` and a
    `**kwargs` `__init__`. `array.array("i")` cost 9,027 instructions
    (CPython 1,535). The class now has a pure constructor: for a numeric
    typecode, and a list of plain ints or floats it holds, the instance
    is laid out in place as `_setup` lays one out, while the class keeps
    its own `__new__` and `__init__` (and no tracer or profiler is
    watching). `array.array("i")` is now 1,704 instructions and
    `array.array("i", [1, 2, 3])` 3,080, down from 20,899 (CPython 3,039).
    `__new__`'s parameters are now positional-only, so
    `array.array("i", initializer=[1])` raises CPython's `TypeError`.
  - The enum member reads above found `_name_`, `_value_` and the
    descriptor's `fget` by scanning the dict's names, 181 instructions a
    name. They now use the names' hashes, computed once, and the
    positions they last had in a split layout. The interpreter's
    attribute path takes the same in-place read. A function returning
    `member.name` is now 1,569 (CPython 1,556), `member.value` 1,594
    (CPython 1,561), and `d[member]` 1,216 (CPython 1,434).
  - A csv reader keeps its state in its instance dict, and each row's
    fast step found five entries by hash (about 90 instructions each).
    It now reads them at the positions the constructor inserts them,
    checking the key there, so `next(reader)` went from 2,423 to 1,838
    (CPython 1,494). `list()` of an instance looked up `__len__` and
    `__length_hint__` on the class each time to decide what to probe;
    their absence is now remembered per class version.
  - `dataclasses.asdict` recursed through `_asdict_inner` in Python, a
    call, a `fields()` tuple and a `getattr` per field (24,387
    instructions for three fields, one a list; CPython 15,074). With
    the default `dict` factory, a tree of plain dataclass instances
    (fields read from their dicts, no descriptor of the name on the
    class), exact lists, tuples and dicts, and plain scalars now copies
    natively (`_weave_collections._asdict_plain`): 7,461. Any other value
    (a namedtuple, a set, an enum, a slots dataclass), a custom factory,
    or a watching tracer or profiler takes the Python copy.
  - `itertools.tee([1, 2, 3])` cost 38,843 instructions (CPython 2,478).
    Each branch ran the Python `_tee.__new__`, whose native core call
    left compiled code every time: a module builtin that the in-place
    builtin lane doesn't list runs through the interpreter. `tee` now
    builds the shared data object and its branches natively
    (`tee_native`, listed for the lane). Each branch is still a `_tee`
    instance, which can be weakly referenced as CPython's tee objects
    can, over a native core. `_tee.__next__` gained a leaf half that
    steps that core over the shared buffer, or a source whose step runs
    no code, in place. Creation went to 9,308, and draining two
    three-item branches from 66,691 to 28,744 (CPython 5,178).
    `math.comb` and `math.perm` of machine ints now multiply in 128-bit
    integers, falling back to big ints on overflow: `comb(10, 3)` went
    from 3,744 to 630 (CPython 683). `itertools.compress` over two
    sequences steps in place when its selectors are plain bools, ints
    or `None` (`list(compress(...))` of four items 8,123 to 6,207, CPython
    3,325). `bisect` calls a key that runs no code (an `itemgetter`) in
    place (6,951 to 4,119, CPython 2,340). `OrderedDict(pairs)` and
    `OrderedDict(d)` with `str` or `int` keys build in place.
  - `math.isqrt` of a machine int takes the float root's estimate and
    corrects it to the exact floor, where it went through big ints, and
    `math.gcd` folds machine words without collecting them first
    (`gcd(84, 36)` and `isqrt(1000)` together went from 1,689 to 1,194,
    CPython 1,160). `dict()` of a generator or iterator of pairs no longer
    allocates two throwaway globals dicts or copies its pairs into a
    second list, and sizes the dict once (`dict((k, v) for k, v in
    pairs)` of ten pairs went from 15,945 to 14,508, CPython 10,769).
  - `WeakKeyDictionary` and `WeakSet` lookups ran their Python methods,
    which make a `ref` of the key and probe the table through the
    reentrant path (weak references compare through their referents):
    `wkd[k]` cost 13,908 instructions (CPython 1,996), `k in weakset`
    12,930. `functools.singledispatch` looks its dispatch cache up this
    way on every call, so a dispatched call cost 19,575 (CPython 3,646).
    For a key that compares and hashes by identity (a class, a function,
    a plain instance), the entry is the stored live reference whose
    referent is the key itself. The tables' `__getitem__`, `get`,
    `__contains__`, a `__setitem__` of a present key and a `WeakSet.add`
    of a present item now find it natively. Other keys, and any lookup
    while a tracer or profiler watches, take the Python methods. `wkd[k]`
    went to 1,455, `weakset.add(x)` from 9,647 to 1,026, and a
    singledispatch call to 4,184. A `WeakValueDictionary` store builds a
    `KeyedRef` (a `ref` subclass with a `key` slot), and that ran its
    Python `__new__` and `__init__`. While the class keeps them, it is
    now built in place (`wvd[k] = v` 10,194 to 6,847, CPython 5,995).
  - A `threading.local` attribute store left compiled code every time:
    the interpreter stored it in place through the class's native
    `__setattr__` hook, but the compiled store helpers had no such arm.
    Both now do (`tl.x = i` 1,966 to 726, CPython 954). `SimpleQueue`'s
    native `put` and `get` found `_queue` and `_locked` by scanning the
    instance's names. They now use the names' hashes and remembered
    positions (`PyInstance::attr_get_memo`), and a put and get went from
    3,626 to 2,415 instructions in one run (CPython 945).
  - An adapter whose source is a `cycle` (`islice(cycle(xs), n)`) took
    the general step for every item, since only plain cursors, counters,
    repeaters and chains counted as sources whose step runs no code. A
    `cycle` over a plain cursor, or replaying its saved items, now does
    too: `list(islice(cycle("ab"), 5))` went from 12,154 to 9,882
    (CPython 4,806). A `defaultdict` miss filled natively looked
    `__missing__` up on the class each time to check that it is the
    native one. That check is now remembered per class version, and
    counting eleven words with `d[w] += 1` went from 19,473 to 17,291
    (CPython 15,140). `Struct.unpack` and `Struct.pack` read the
    instance's `_fmt` by scanning its names on every call. They now use
    the name's hash and remembered position (`Struct("<ihd").unpack(b)`
    from about 2,300 to about 1,700 instructions, CPython about 1,050;
    these runs are noisy). `random`'s native methods check that the
    instance holds no `_randbelow` or `getrandbits` of its own, which
    scanned all its names for each miss. The check now goes by the names'
    hashes (`randint` 1,063 to 958, CPython 2,494).
    `ContextVar.get` borrows the var's class state in place instead of
    cloning it through the receiver check (817 to 588, CPython 431).
  - `enumerate` over a `str` left compiled code once per loop: its end
    step only knew list, tuple and range cursors. `enumerate(s, 1)` was
    also declined by the `int(text, base)` constructor arm, which
    returned for any `[str, int]` call. Both now run in place
    (`for j, c in enumerate("abc", 1)` 3,511 to 2,108, CPython 2,286).
    `set.issubset`, `issuperset`, `isdisjoint`, `union`, `intersection`,
    `difference` and `symmetric_difference` with set arguments were
    dispatched by name through the interpreter (about 600 instructions a
    call). They now have leaf halves, and a union of several sets folds
    over their tables (`a.issubset(S)` 1,271 to 713, CPython 597;
    `S1.union(S2, {99})` 7,847 to 4,513, CPython 4,940). `set(s)` copies
    another set's table, and `dict.popitem`, `bytes.join`, `hex`, `oct`
    and `bin` run as leaves. An in-place `s |= t` of sets runs in compiled
    code (2,460 to 1,256, CPython 1,588), and `s &= t` hands back the
    replaced table whole instead of collecting its keys.
  - `list(map(max, a, b))` resolved its callback on every step; a map
    over two plain cursors now resolves it once (6,517 to 4,816, CPython
    3,978), and `max`/`min` of two ints or floats skip gathering a list
    (`max(x, y)` 389, CPython 635). A map over an instance with a native
    `__call__` (`map(itemgetter(0), pairs)`) runs its fast half per item.
    `itertools.product("ab", repeat=2)` left compiled code on every call
    because a `str` couldn't be a pure pool (11,091 to 5,929, CPython
    5,990).
  - `heapq`'s sifts took a guarded borrow of the list for every
    comparison and swap, each running the biased-guard protocol. Two
    native scalars are now compared and swapped under one view of the
    list (a push and a pop 2,120 to 1,860, CPython 1,863). `Counter(list)`
    counts a run of plain keys under one view of its dict with one change
    notice (13,061 to 11,699, CPython 11,090). `zip`'s bulk drain reads
    its cursors through views (`list(zip(*m))` 5,464 to 4,656, CPython
    4,118), and `zip_longest` builds rows of up to four sources in
    arrays.
  - `list(obj)` of an instance whose `__iter__` returns a native iterator
    stepped it through the general loop; it now drains under one borrow
    (`list(It(5))` 6,331 to 5,762, CPython 2,890, most of the rest being
    the `__iter__` frame). Pure `bytes` and `bytearray` methods (`upper`,
    `split`, `replace` and about thirty more) run as leaves when every
    operand is plain data (three calls 4,164 to 3,900). `BytesIO`'s
    `getvalue` and in-place writes read the buffer through views (on
    Linux 3,231 to 3,014 for a create, two writes and a `getvalue`).
    The leaf JIT's builtin calls copy their arguments once.
  - A class attribute read through an instance (`self.LIMIT`) went to
    the compiled code's slow attribute helper before the class memo
    answered it (1.38 times CPython for an int), and a module value was
    never memoized at all (`pathlib`'s `self.parser`, read several times
    in every path operation, 2.2 times). Modules, ranges, complex
    numbers and bytearrays are now memoized as plain values, and the
    fast helpers consult the memo once the field caches miss: both reads
    are about level with CPython (265 to 311 instructions, CPython about
    280). `list(enumerate(gen))` collects the generator as `list(gen)`
    does and numbers the items (8,264 against CPython's 5,893, from
    12,081). `sum`, `min`, `max`, `any`, `all`, `list` and `tuple` of an
    iterable the interpreter steps skip about fifty name comparisons in
    the call dispatcher, and a drain of an adapter over a generator no
    longer tries a pure step that can't succeed.
  - A method, staticmethod or classmethod read as a value through an
    instance (`cb = self.handle`) was never memoized, and the two
    wrappers' reads re-resolved the name each time, scanning the
    instance's attributes: a classmethod read cost 6,169 instructions
    (CPython 638) and a staticmethod's 2,432 (CPython 442). The class
    memo now keeps the class's own entry for these too and binds or
    unwraps it on each read: 717, 325, and a plain method read 1,280 to
    590 (CPython 642).
  - Every `list(xs)`, `tuple(xs)`, `dict(pairs)` or `set(xs)` of a list
    or tuple first scanned the registry of struct sequence types (about
    150 instructions); the core containers now skip it. `bool(x)` found
    its constructor arm after a dozen other classes' and now finds it
    first; `list("hello")` and `set("hello")` collect the characters
    directly (3,074 to 2,129 and 3,286 to 2,727, CPython 1,698 and
    1,789); `dict.fromkeys(range(n))` builds in place where it left
    compiled code on every call (8,543 to 2,901, CPython 3,353); and
    `xs.clear()` of plain items runs as a leaf. `list.index(x, start)`
    with int bounds scans natively (1,957 to 833, CPython 1,515).
  - `collections.deque` was 7 to 8 times CPython in several everyday
    shapes. `list(dq)` of 20 items stepped a Python-level iterator per
    item (19,887 instructions, CPython 2,420); a deque iterator's rest is
    now copied at once, and its `__next__` has a leaf half (7,863). A
    bounded `dq.extend((a, b, c))` left compiled code on every call: a
    native method of a class written in Python, with a name the call
    dispatcher handles (`extend`), had no in-place lane on a plain
    instance. It now takes the dispatcher's path in place (6,631 to
    3,404, CPython 924). `OrderedDict` equality compares plain keys'
    order natively (still 7 times CPython: its `__eq__` is a Python method
    that isn't a leaf, so the comparison leaves compiled code).
    `key in chainmap` left compiled code at every test, though
    `ChainMap.__contains__` is native; over exact dicts it is now decided
    in place (3,887 to 983, CPython 1,550). `itertools.chain`'s bulk drain
    takes `str` sources (`list(chain([1], (2,), "ab"))` 7,749 to 4,611,
    CPython 4,125).
  - `functools.wraps` ran `update_wrapper`'s Python loop of `getattr`
    and `setattr` calls on function objects (44,019 instructions, CPython
    14,595). For two plain functions and the default attribute lists the
    same steps now run natively (30,254). A function's lazily planted
    `__name__` and `__qualname__`, and a generator's names, are interned
    through the per-code-name memo instead of hashing the text each
    time.
  - A list iterator that ran out swapped in a freshly allocated empty
    list to detach from its source, and freed it when the iterator died,
    so every loop over a list paid an allocation at its end. Exhausted
    list and reverse iterators now share one empty store per thread.
    The frame JIT's step retires a loop's own exhausted list cursor
    without its general helper, and tier 2 builds list and tuple cursors
    directly (a loop over an empty list 870 to 557 instructions, CPython
    438; over a two-item tuple 818 to 693, CPython 591).
  - `for i, (x, y) in enumerate(zip(xs, ys))` built an `(i, pair)`
    tuple per item and unpacked it twice, because the fused pair step
    only knew `enumerate` over a sequence. It now steps `enumerate` over
    any native adapter, handing over the index and the adapter's item,
    and unpacking a `zip` result the adapter still holds skips the tuple
    recycler (a 100-item loop 123,095 to 76,757 instructions, CPython
    98,531). `str.split` by a separator of two to sixteen bytes scans
    for its first byte with `memchr` instead of setting up the two-way
    searcher (`s.split(", ")` 3,310 to 2,519, CPython 2,527).
  - A call that spreads arguments, `f(a, *rest)`, compiles to
    `BUILD_LIST`, `LIST_EXTEND` and `LIST_TO_TUPLE`. The frame JIT now
    fuses the last two into one helper that builds the tuple from the
    list's items and an exact tuple or list in a single allocation, and
    such a list is no longer registered with the collector, since only
    the stack holds it. A `**kwargs` dict that is only read (a truth
    test, `in`, a loop, `len`, `get`, `pop`) also skips registration.
    Thirty spread calls fell from 72,635 to 64,227 instructions (CPython
    58,488), thirty keyword calls from 74,482 to 70,957 (CPython 62,013).
  - `getattr`, `hasattr` and `attrgetter` declined their fast half for
    any attribute the class supplies through a descriptor, so a
    `__slots__` member went through the full lookup, about four layers
    and five class lookups deep (`getattr(obj, "a", None)` 4,191
    instructions, CPython 515; `list(map(attrgetter("a"), objs))` over 50
    slotted objects 204,564, CPython 20,214). The fast half now reads a
    slot member directly, giving the default for an unset one when no
    `__getattr__` could answer, and remembers per class version and name
    that the class supplies nothing or a slot, which skips the class
    lookup on later reads of any instance (`getattr` of a slot 719,
    `hasattr` 757, CPython 610 and 590; the 50-object map 40,941, CPython
    20,485; over plain objects 40,315 to 37,547).
  - That memo kept a reference to each name in a `RefCell`ed vector, so
    a probe cost about 70 instructions. Its entries are now plain words
    in the thread's static block, keyed by the class version and the
    name's bytes (names of up to 16 bytes), and a probe costs about 50;
    `attrgetter`'s fast half also reads its name without cloning the
    getter's tuple (the 50-object map over plain objects 37,072 to
    32,872 instructions under callgrind).
  - Iterating an object that defines only `__getitem__` (the legacy
    sequence protocol) goes through the frozen `_seqtools._SeqIter`,
    whose `__next__` catches `IndexError`, so every item ran two full
    Python frames (3,917 instructions an item, CPython 801). The
    frameless iterator step now recognizes that class, unchanged since
    it was made, and evaluates the sequence's `__getitem__` as a pure
    leaf with the index, ending the loop at its bare `raise IndexError`
    or `raise StopIteration` and dropping the sequence as CPython's
    iterator does (1,311 an item).
  - Sorting objects by a Python `__lt__` ran each comparison as a full
    call (about 3,000 instructions, twice CPython's), because the
    frameless evaluation declined: it read `__slots__` members only
    through the interpreter's specialized cache, which a body the frame
    JIT runs never fills, and it couldn't compare tuples (`return
    (self.x, self.y) < (o.x, o.y)`). Its attribute fallback now reads a
    slot member directly, and its comparison takes two tuples of scalars
    and strings item by item as CPython does, declining for anything else
    (sorting 40 slotted objects 2.31 to 1.11 times CPython; by a tuple
    key 2.01 to 1.06; `min` 2.01 to 1.40).
  - A read that a class's pure `__getattr__` answers (`return
    self.d.get(k, 0)`) walked the slow path's whole chain of site memos
    before reaching the hook, then looked `__getattr__` up along the MRO
    (about 2,100 instructions a read, twice CPython's). The hook is now
    found once per class version, and a site the hook answered marks the
    class version so its next miss tries the hook first, which still
    checks that the default lookup misses (2.09 to 1.50 times CPython).
  - A frameless `__hash__` returning `hash(self.a)` called the builtin
    through the general native-call path. A leaf plan already fused
    `hash((a, b))`; its call of the builtin `hash` with one argument that
    hashes natively now computes the hash in place, in both the plan's
    interpreter and its compiled form (`set()` of 50 such objects 2.12 to
    1.77 times CPython; a dict comprehension keyed by them 1.51 to 1.30).
  - `list(g)` of an `itertools.groupby` group stepped the grouper one
    item at a time through the adapter dispatch (about 250 instructions an
    item). A group of a key-function-free `groupby` over a plain list or
    tuple now drains in one loop, leaving the next group's first item as
    the groupby's current value, as the item-by-item steps leave it
    (`[(k, len(list(g))) for k, g in groupby(xs)]` over 50 items 1.99 to
    1.68 times CPython).
  - A loop over a descending range stepped through the general helpers:
    the frame JIT's in-place step took only positive steps, and tier 2's
    opaque-iterator step had no range case. Both now step a range either
    way (`for i in reversed(range(50))` 1.61 to 0.96 times CPython;
    `for i in range(49, -1, -1)`, which tier 2 doesn't count natively,
    3.00 to 1.65). A frameless evaluation also raises an int to a power
    below 64 (`min(xs, key=lambda x: (x - 25) ** 2)` declined to a full
    call for every item: 2.07 to 0.62).
  - `format(obj, spec)` and an f-string's `{obj:spec}` called a class's
    Python `__format__` through a bound method and a frame (about 10
    allocations a call). A warm pure-leaf `__format__` now runs frameless,
    found once per class version, and `format()`'s own fast half counts as
    pure, so a body that formats a field (`return format(self.v, spec)`)
    qualifies (`f"{p:>4}"` 1.67 to 1.18 times CPython).
  - The fast halves of `round`, `pow`, `repr`, `hex`, `oct` and `bin`,
    which take only native numbers or strings, now count as pure, and a
    frameless evaluation adds two strings (a new string it holds), so
    callbacks using them no longer decline to a full call per item
    (`sorted(xs, key=lambda x: round(x, 1))` 1.04 to 0.77 times CPython;
    `min(xs, key=lambda x: pow(x - 25, 2))` 1.69 to 1.00; `map(lambda x:
    hex(x)[2:], xs)` 1.88 to 1.32; `map(lambda x: repr(x) + "!", xs)`
    1.61 to 1.14).
  - The `str` methods the leaf table registers as opaque (`count`,
    `zfill`, `title`, `rjust`, `partition`, `index`, the `is...` tests and
    the rest) read their immutable receiver and run no code, but the
    frameless evaluator's purity test rejected the kind, so a callback
    calling one declined to a full call per item. On an exact `str`
    receiver they now count as pure, as do the fast halves of the `bytes`
    methods (`map(lambda s: s.zfill(8), ws)` 2.05 to 1.38 times CPython;
    `sorted(ws, key=lambda w: w.count("a"))` 2.11 to 1.37).
  - Widening that exposed an older fault: a frameless evaluation's
    subscript returned a pointer into a container the evaluation had
    built itself, so `return s.split(".")[0]` read the item after the
    list was freed (a wrong result after warm-up, then a crash). An item
    of a container the evaluation holds is now held too.
  - `list(zip(*rows))` over four to eight lists built each tuple through
    an intermediate vector; it now fills the tuple's own allocation
    directly (`list(zip(*m))` of an 8x8 matrix 1.64 to 1.19 times
    CPython).
  - `list(chain(gen, gen))` resumed each generator once an item through
    the adapter's general step (about 1,500 instructions an item, 3.5
    times CPython), while `list(gen)` folds a generator's yields straight
    into the list. A chain being drained now drains a generator source
    the same way, whole, then moves on as a source's end moves it (0.90
    times CPython); a raise leaves the generator active, as stepping it
    would.
  - `list(islice(gen, n))` took each item through the adapter's general
    step, which snapshots and writes back its state every item. Draining
    one over a generator now steps the generator in a row with the state
    read and written back once (the skipped items first, then the stop
    bound, as before), 3.32 to 1.84 times CPython; the rest is the
    generator's resume per item.
  - `list(zip(gen_a, gen_b))` likewise steps each row's sources in order,
    stopping at the first source's end with that row's earlier items
    dropped as CPython drops them, instead of the adapter's general step
    per row (2.74 to 2.02 times CPython).
  - The bench gate's macOS leg reads `attr_access` 14% to 20% over the
    merge base run after run (and `str_methods` 11% to 22%), at the 15%
    limit, while on x86 both builds execute the same instruction counts
    and time the same. For the next run that flags a fixture, the gate
    job now prints each flagged fixture under five tier settings for
    both binaries, with instructions and cycles on macOS
    (`tools/bench_gate_counts.py`): equal counts with unequal times are
    the runner or the layout, unequal counts name the tier.
  - Looked at and left: the frame JIT waits 128 call heats before taking
    code whose call entry tier 2 missed and whose loops haven't run
    (`argparse`'s `take_action` and `_format_action`, the case's hottest
    functions, are still interpreted after 2,500 iterations). Taking
    them at the first heat changes the case by under 1.5%: their bodies
    are calls and attribute reads that cost about the same compiled.
  - Why wall time trails instruction counts on the battery's worst
    cases, from a cache and branch simulation of `argparse` in steady
    state (cachegrind, iterations 1,500 to 2,500): an iteration takes
    99,500 first-level instruction-cache misses to CPython's 28,600,
    38,600 data misses to 16,700 and 47,500 mispredicted branches to
    31,900, on 1.91M instructions to 1.27M. At 12 cycles an instruction
    miss, 8 a data miss and 18 a misprediction that models the measured
    cycles of both (3.2M and 1.5M an iteration): the instruction misses
    are 40% of WeavePy's time and most of its distance from CPython. They
    are spread over 1,400 functions, the largest (compiled code) 11%, at
    about one miss every 19 instructions, which is every cache line the
    program fetches: the working set of an iteration is far beyond the
    32 KB cache, so what counts is how many instructions run, not which.
    Compiling the runtime with a quarter of the inlining (LLVM's
    threshold at 60) changed nothing (2.9% more instructions, 0.9% more
    misses).
  - Looked at and left: thread start-up (a started and joined thread
    costs about 470,000 instructions to CPython's 150,000; the main thread
    waits on the GIL hand-off and the kernel's thread teardown, and a
    smaller worker stack reserve changed nothing), the rest of
    `threading.Thread()` (`_ThreadHandle()` 6,800 against 1,400, `Event()`
    12,200 against 7,800), and `Enum` class creation (2.2 times CPython's
    instructions, spread over `enum.py`'s Python code).
  - What's left. Battery geomean 1.178 instructions and 1.052 cycles
    (from 1.197 and about 1.08 at the day's start). The worst cases are
    flat profiles of interpreted library code: argparse's per-iteration
    cost is 2.3 times CPython's with both JITs off or on, since it runs
    mostly cold code, where the core loop's call and return cost about
    700 instructions to CPython's 200. Exceptions (a raise caught in the
    caller 2.1, same frame 1.7) spread over about 40 helpers, creating the
    instance alone about 680 instructions to 250; a generator resume's
    trampoline is 291 instructions of checks and setup, `FOR_ITER`'s
    dispatch to it 80 more.

- **Generator drains, a warm-up gate, and email's token tree** (PR #112,
  November 16, evening). Ratios are instructions, WeavePy's over
  CPython's.
  - An inlined `any(...)`, `all(...)`, `list(...)` or `tuple(...)` loop
    over a generator expression drained it in place only when the fast
    steps run its body; a body that calls a function (`any(pred(x) for x
    in xs)`) was resumed lazily, about 1,000 instructions an item. Its
    lean resume now folds the yields into the consumer, as `sum()`'s
    does: `any(...)` over 30 items ran 34,480 instructions to 12,354
    (CPython 19,519), `tuple(...)` 36,822 to 15,050 (21,752).
  - The core loop's calls of a pure leaf took a full lean activation for
    the callee's first 24 calls, so tier 2's warm compile could give
    compiled callers a direct lane. Their evaluation already counts each
    call toward that compile, as the frame JIT's leaf sites' do, so the
    gate only cost code called a few times each, such as the methods
    `dataclasses` makes for each class: `enum_dataclass_ops` 6% fewer
    instructions. It showed up as tier 2 costing the battery 1.3% of its
    instructions (`WEAVEPY_JIT=0`), while `WEAVEPY_TIER2=0`, which keeps
    the gates, cost more; a sweep over the gates that ask whether the JIT
    is on found this one. The other gates in the dunder lanes (`__eq__`,
    `__hash__`, `__lt__`) keep the wait: dropping theirs cost
    `oo_patterns` 3%, its leaf plans compiling before their field caches
    warmed.
  - `email`'s token tree reads `x.value` and `x.all_defects` over two node
    classes, each with a property. The site's getter cache held one class,
    so each switch took the full handler; it keeps two earlier getters by
    class version now, and compiled code calls a cached getter that runs
    as an activation right after the leaf attempt, past four lanes a
    property always wins over. `str(x)` of a `str` subclass's instance
    whose class keeps `str`'s own `__str__` (remembered per class version)
    is its payload, from the pure constructor lane: 1,795 instructions to
    541 (CPython 814). A tree's `value` ran 210,471 instructions to
    138,758 (62,258).
  - `dir()` gathered names into a `BTreeSet<String>`, copying each and
    allocating it again as a `str`: it now collects the dict keys' own
    strings and sorts once (`inspect.getmembers` of a class 211,206
    instructions to 190,518, CPython 136,682). `s.encode(encoding="utf-8")`
    took the codec lookup, normalizing the name each call (3,975 to 2,198,
    against 1,494), and now raises CPython's `TypeError`s for unknown or
    repeated keywords, which it ignored. A one-base class's MRO is the
    base's with the class in front, no merge (`type()` 17,599 to 16,376).
  - Where the time goes. `email`'s `HeaderRegistry` builds a new class
    with `type()` for every header it makes, so every header's lookups
    start on a cold class; WeavePy's `type()` is cheaper than CPython's
    (17,537 instructions to 121,421), but an address header still costs
    1.05 million instructions to CPython's 546,000, at 3.7 times its
    cycles. In argparse, `parse_args` runs 2,200 bytecodes and 62 Python
    calls a parse: interpreted, WeavePy spends about 150 instructions a
    bytecode to CPython's 75, and 724 a call in the core loop's call and
    return alone; compiled and warm it's 1.6 times CPython's. A plain
    `for v in gen()` loop spends 685 instructions an item to CPython's
    400, 291 of them in the resume's checks (`gen_native_next`).
  - Across seven of the worst cases (`argparse`, `email`, `configparser`,
    `enum_dataclass`, `inspect_typing`, `class_features`, `collections`;
    synchronous compiles), compiling is 8.3% of the instructions, the core
    loop's own code 10.9%, calls 10%, allocation 10.3%, attribute paths
    10.3%, and the rest is spread over hundreds of functions: the largest
    single one, `leaf_burst`, is 7.7%. Cachegrind puts `argparse_ops` at a
    3.8% instruction-cache miss rate (32.7 million misses in 858 million
    instructions); compiled code takes 1.6% of the misses and the VM's own
    code 92%, so for short cases the footprint is the interpreter's and the
    runtime's, not the JITs'. The core loop's dispatch jump mispredicts 1.9
    million times there.
  - Threads. A two-thread ping-pong through `queue.Queue` costs 173,884
    instructions and 249,454 cycles a round trip to CPython's 65,984 and
    67,867, with twice the context switches. While a second thread runs,
    every container's cell borrow takes an atomic compare-and-swap
    instead of the biased check; starting and joining a bare thread costs
    281,455 instructions to 86,833 (per-thread tables zeroed and dropped,
    the allocator's per-thread heap, the interpreter's fork).

- **Configparser's constructions, compiled patterns, and where the short
  cases spend** (PR #112, November 16, later). Ratios are instructions,
  WeavePy's over CPython's.
  - `re.match(p, s)` with a compiled pattern runs `re._compile`, which
    probes its cache with `(type(p), p, flags)` and catches the
    `KeyError`. A pattern hashed through the interpreter (a `__hash__`
    call that built and hashed a tuple), and a tuple holding one wasn't a
    key compiled code could probe, so the miss left for the core loop,
    which hashed the key again before raising. A pattern now hashes
    natively while its class keeps the native `__hash__`, and the miss
    raises in place: `re._compile(p, 0)` ran 18,402 instructions to 11,430
    (CPython 6,129), and `ConfigParser.write` 19% fewer.
  - `functools.partial(f, k=v)` left compiled code (no keyword lane for a
    Python class with a native `__new__`) and set its five slots one probe
    at a time: 8,338 instructions to 5,749 (2,216). `for k in mapping`
    over an instance whose `__iter__` is a Python function left too; it's
    called directly now, its result checked as `iter()` checks it (5,812
    to 2,814, against 1,786). Together, a `SectionProxy` ran 58,882 to
    47,197 (24,155).
  - A value type's subclass with a Python `__new__` (`configparser`'s
    `_Line(str)`) went through the generic construction, with `__new__`
    and `__init__` each a nested run: it now takes the inline activations
    a plain class's does (10,000 instructions to 5,435, CPython 4,063).
    The configparser case ran 7% fewer instructions.
  - `(x,) * n` and `[x] * n` of a short sequence repeat in place in
    compiled code and the core loop (argparse's metavar formatter left 13
    times a parse).
  - `dataclasses.replace` ends in `self.__class__(**changes)`, which had
    no lane for a class: it left compiled code and took the generic call.
    The mapping's keywords now bind to `__init__` as a keyword call's do,
    and the call runs inline (27,452 instructions to 19,239, CPython
    11,984; cycles 3.9 times CPython's to 2.1). The keyword and mapping
    constructions share one body.
  - `super(C, obj).__class__` built the proxy's dict one probe at a time,
    and the class read, finding the proxy's last reference on the stack,
    declined to release it and took the attribute miss path: it releases
    it now, a queued finalizer running before the next instruction
    (4,269 instructions to 1,785, CPython 1,236).
  - A `list` or `tuple` subclass's `self[0]` (`email`'s `TokenList`) and
    `for entry in os.scandir(...)` (`glob`) were slow steps; they read the
    payload and step the iterator in place. Branch relaxation in the code
    generator found each branch's place by binary search on every pass:
    32% less of the compiles' finishing work.
  - The battery stands at 1.233 times CPython's instructions on quick
    builds (1.237 this morning): `class_features` ran 10% fewer,
    `configparser_ops` 8%, `enum_dataclass_ops` 5%, `email_html_ops` 3%.
  - Why the short cases trail. Argparse's case runs each operation 180
    times. CPython's instructions an iteration are flat from the first
    (1.5 million); WeavePy's fall from 5.6 million over the first 20 to
    2.3 million after 600, with the frame JIT still compiling functions
    for the first time after 300 iterations (functions run once an
    iteration reach the threshold late). Turning either JIT tier off, or
    both, leaves the case's instructions and time where they are: the
    gap is the interpreter and the runtime. Even warm, an iteration takes
    2.3 times CPython's cycles on 1.5 times its instructions: cachegrind
    finds 5% of instructions missing the instruction cache and 13% of
    conditional branches mispredicted, both spread over the interpreter,
    the helpers and compiled code. Tier 2 analyzed 144 functions in the
    case and compiled 11; the rejections (`dynamic call (loop-free)`
    after a full analysis, and probe misses that carve loop by loop) took
    most of its 44 ms of analysis, about 64 million instructions.
    And calls from native code into Python (`map(partial(cls, k=v), xs)`)
    cost about 1,200 instructions of nested-run overhead each, with
    keyword names copied to `String`s at every hop: 11,300 instructions an
    item to CPython's 6,200.

- **Builtin subclasses, generator steps, and a misread fixture gate** (PR
  #112, November 16). Ratios are instructions, WeavePy's over CPython's.
  - The local fixture A/B tools ran every bench fixture at a work size of
    one, so a tier-2 policy that weighed a loop's dynamic calls three times
    read even locally and cost `nbody` 35% (the bench gate flagged it 41%
    to 56% slower on all three platforms). The policy is reverted, and the
    tools take the gate's work sizes from `fixtures.rs`.
  - `email`'s header parser builds `TokenList`, a `list` subclass, and
    `Terminal`, a `str` subclass, and runs generator expressions over them.
    `super().__init__()` reaching `list.__init__` or `set.__init__` left
    compiled code for the core loop on every call (only `dict.__init__`
    was admitted), `GET_ITER` of a list subclass left too, a property read
    in a generator's fast step declined outright although a pure getter
    runs no activation, and `TO_BOOL` of a `str` subclass left because the
    native truth admitted only one kind of builtin `__len__`. With those
    fixed, and attribute sites remembering the pure getter they last ran,
    a `TokenList()` ran 11,607 instructions to 5,375 (CPython 3,296),
    `TokenList.value` 35,107 to 13,154 (6,339), and the email case 12%
    fewer.
  - An explicit `super(C, obj)` as a value built through every call lane
    (5,540 instructions to 2,224 against CPython's 979), and its
    `__class__` took the generic attribute path (9,790 to 3,951).
  - `object.__new__` walked a builtin subclass's MRO five times a call; one
    walk now answers all of its questions, memoized by class version.
    `os.stat` laid its result's nineteen fields into a dict one probe at a
    time and read ten back by name (13,457 instructions to 9,272).
  - Objects whose `__dict__` was published (`copy`, `pickle`, `vars()`,
    and `dir()`, which configparser's `ConverterMapping` runs on its
    parser) lost compiled code's field reads: every attribute read went
    the whole miss path, about 800 instructions. `dir()` no longer
    publishes, and attribute sites remember where a name sat in a
    published dict (two reads of a copied object: 1,875 instructions to
    509, CPython 896). Class attributes read through an instance now
    memoize objects without `__get__` too (configparser's compiled
    patterns), and native `re` objects lay their slots over their
    classes' own layouts. `ConfigParser.read_string` ran 10% fewer
    instructions, `ConfigParser.get` 20%.
  - `typing`'s subscripts (`List[int]`, `Optional[str]`) went through
    `_tp_cache`, whose `f(*args, **kwds)` forward to an `lru_cache`
    wrapper had no lane in compiled code, whose cache probe hashed only
    builtin scalars, and whose subscript site, compiled before the core
    loop ran it, had no direct `__getitem__` call: `typing.Optional[str]`
    ran 13,031 instructions to 3,164 (CPython 2,683).
  - The frame JIT now takes code tier 2 failed to compile three times
    (argparse's `_parse_known_args` had stayed interpreted for good), and
    `ref.__new__` and `_sre.exec_match` over a `str` run in place (a
    100-entry `WeakValueDictionary`: 1,241,587 instructions to 877,688).
  - Where compiled code still left for the core loop, per site: an
    exception's `args` (its own slot layout), an f-string's `!r` of a
    list, a generator expression's dict view at its end, an exhausted
    tuple of objects (enum's `for flag in self, other`), and an `IntFlag`
    operator (a builtin subclass's Python method). Enum flag operations
    ran 14% fewer instructions, a dataclass's `repr` 25%,
    `dataclasses.asdict` 24%, `e.args` 44%.
  - Native helpers: `binascii.hexlify` ran a branch and a growing push a
    digit (33 instructions a byte, now under CPython's), and `upper()`,
    `lower()` and `casefold()` sent every character of non-ASCII text
    through the Unicode tables (a mixed 400-character string: 36% fewer
    instructions).
  - Tried and dropped: native `LOAD_GLOBAL` hits in compiled code (the
    helper costs as much as its guards would in line), native reads of
    polymorphic split fields (their IR pushed `deltablue`'s hot functions
    over the inline budget), and a Linux-only symbol order (the release
    build's fat LTO renames most of what the profiles name; start-up RSS
    moved 2%), and a lower frame-JIT threshold (60: the battery ran 1% more
    instructions). The battery stands at 1.252 times CPython's
    instructions on quick builds, against 1.285 at the start of the day;
    `class_features` ran 21% fewer instructions, `email_html_ops` 13%,
    `configparser_ops` 8%, `enum_dataclass_ops` 7%.

- **Tier 2's wasted work, a start-up deadlock, and more cliffs** (PR #112,
  November 15). Ratios are instructions, WeavePy's over CPython's unless
  stated, with synchronous compiles.
  - Tier 2 cost the everyday battery more than it gave it: with tier 2
    off, the battery ran 2% fewer instructions, and no case ran more than
    3% more. Three causes, each fixed. The analysis found each pc's
    innermost loop by searching the whole loop forest per pc, once per
    analysis and carving step, which made a failed attempt on a big
    function expensive (the argparse case ran 13% more instructions with
    tier 2 than without; now the two are even). A recompile at a loop the
    compiled frame couldn't enter dropped the frame first, and when the new
    analysis missed its probes the code went cold and compiled the same
    frame again on the next call (difflib's `find_longest_match`, four
    compiles a run, 7% of the case). And the frame JIT leaves code with
    loops to tier 2, which left two kinds of code interpreted for good:
    code whose call entry tier 2 had already missed (it never attempts
    that entry again; `urllib.parse._checknetloc` ran interpreted on every
    call) and compiled code whose activations mostly side-exit or fail
    their entry guards, which waited for absolute budgets of 64. Call heat
    now goes to the frame JIT once tier 2 has missed the call entry and
    the code has got hot that way 128 times with none of its loops run
    (taking it at the first such heat cost raytrace 8%: `Scene.trace`'s
    loop runs only for rays that hit something, which the first rows
    don't, and tier 2 compiles that loop by OSR later), and compiled code
    retires once its misses reach 16 and outnumber its finished native
    runs two to one. With tier 2 off the battery now runs
    0.8% fewer instructions instead of 2%; the enum and dataclass case (8%)
    and the threading and queue case (6%) are what's left. Tier 2 makes a
    loop's dynamic calls through general helpers where the frame JIT runs
    a Python callee in line (a keyword call in a range loop: 1,080
    instructions an iteration under tier 2, 265 under the frame JIT), but
    weighing those calls three times in the generic share cost the
    `nbody` fixture 35% and `generators` 16%: a driver loop the frame JIT
    takes runs its callee's float loop there too, where tier 2 entered it
    by OSR. Still open: a share of 10% instead of 20% would send more loops
    to the frame JIT (the battery 0.4% fewer instructions, the suite even),
    but there a Python `__eq__` that a native instance-dict lookup runs
    sees the wrong caller frame in an activation's first lookup: the core
    loop publishes its activation only before the calls it makes itself.
  - `os.environ.get(name)` and `name in os.environ` read the backing store.
    `Mapping`'s versions find a missing variable through `__getitem__`,
    which raises and catches two `KeyError`s, and programs probe unset
    variables constantly (argparse's color and gettext checks probe 22 for
    each parser): a miss is 0.31 of CPython's instructions instead of 2.2
    times. An `ArgumentParser()` went from 2.66 to 1.96.
  - Natively built instances (a `re.Match`, a `Decimal`, a `datetime`, an
    ElementTree iterator) get their slots at birth instead of an all-unset
    block that was dropped at once: an allocation, a fill and a free less
    per object. A trivial `re.match` went from 1.25 to 0.92.
  - `super().__repr__()` and the other names WeavePy keeps outside the type
    dicts (`__str__`, `__dir__`, `type.mro`, `__prepare__`, the builtin
    bases' dunders) resolve through `LOAD_SUPER_ATTR`'s direct path instead
    of building the `super` proxy and reading it back: 5.2 to 2.2.
  - Builtin subclasses. A `str`, `int`, `float`, `complex`, `bytes` or
    `tuple` subclass's `__slots__` are laid out as `object`'s are: any
    builtin base but `object` kept members in the keyed form, which
    compiled code never reads in line, so each member read of a
    `configparser` `_Line` (a `str` subclass) took the attribute miss path
    (3.40 to 0.41). Constructing a builtin subclass's instance asks the
    type cache whether `__init__` is overridden instead of probing each
    class dict with a fresh key, and the collector's "not due yet" answer
    comes from one load (a `str` subclass's construction, 4.1 to 3.4).
  - `yield from` a range, tuple or list iterator steps it in place: the
    core loop's `SEND` took the full step for any delegate but a
    generator or an asyncio future, so every item went through the
    general handler (`yield from range(100)`, 3.40 to 1.27). A list's end
    keeps the general path, since it detaches the list and may free a
    subclass's owner.
  - Smaller ones: `object.__setattr__(self, name, value)` (a frozen
    dataclass's `__init__`) passes the name object through a per-thread
    memo to the intern pool instead of copying and re-interning its text,
    and a method site keeps a builtin type's unbound or class method
    (`object.__setattr__`, `dict.fromkeys`) for its version; `lru_cache`
    keys holding a plain instance or a user class take the native lookup
    (`typing`'s `_tp_cache`); `OrderedDict.__setitem__` called as a method
    settles through its leaf half; a `weakref.ref` subclass's instances
    share the class's `__call__` and `__repr__` instead of two closures
    each (a `KeyedRef` per `WeakValueDictionary` item); and a thread's
    interpreter fork takes its parent's leaf builtin table instead of
    rebuilding it (a thread's start and join, about 10% fewer
    instructions).
  - Fixed: a process's first thread could deadlock at start-up. The thread
    that revokes the cell bias spun, holding the GIL, until the first
    thread's lock-free guards drained; a count that never drained hung
    torch's DataLoader on macOS (its first queue feeder thread never
    reached Python code), and the ecosystem shard ran out its 30 minutes.
    While the GIL guards every cell the revoker can't race a guard holder,
    so past 50 ms it goes on. A temporary torch-only CI job found it: the
    ecosystem harness now sends a timed-out probe `SIGABRT` first, so a
    probe with `faulthandler` on dumps every thread, and keeps the output's
    tail in the row's reason.
  - Fixed: `reversed()` of an iterator, a set or a native adapter raises
    `TypeError`, as CPython's does; it read the source to the end first,
    so `reversed(itertools.count())` hung.
  - Looked at and left: an explicit `super` object (`super().__class__`,
    which CPython doesn't compile to `LOAD_SUPER_ATTR` either) is about 8
    times CPython's, through a dozen general layers, and rare; setting an
    `EmailMessage` header is 3.4 times CPython's, a flat profile of
    polymorphic attribute sites and frames with nothing dominant.

- **Threads that have exited, lock hand-offs, and more cliffs** (PR #112,
  November 15). Ratios are WeavePy's instructions over CPython's, before
  and after, with synchronous compiles.
  - Single-thread reference counting comes back. Starting any thread made
    reference counts atomic and cell borrows locked for the rest of the
    process, and frames register before every native body so other
    threads can look. A `queue.Queue` put and get after one finished
    thread cost 26,500 instructions instead of 16,000. A `join()` now
    joins the OS threads of the workers that have finished, and once none
    is left (and no thread the VM didn't start ever took the GIL) the
    joining thread restores the bias. The everyday battery runs every
    case in one process, so the six cases after `threading_queue_ops`
    paid for it: by wall time on quick builds, `asyncio_ops` 0.69 of the
    earlier build's, `print_to_buffer` 0.74, `weakref_gc_ops` 0.79,
    `sqlite_ops` 0.80, `inspect_typing_ops` 0.81.
  - Code that runs while a second thread is alive. Every release that
    wasn't the last took the last owner's checks out of line, compiled
    calls that bind defaults weren't entered directly, a direct call's
    return released through the slow path, and lazily running frames
    registered before every native body in case another thread looked.
    Releases that aren't the last are now one locked update, the direct
    call protocol makes locked updates instead of declining, and frames
    register first only for a watchdog that reads them without the GIL:
    every release of the GIL (C code's `PyEval_SaveThread` included)
    settles them first. A queue put and get with an idle second thread
    alive: 26,430 instructions to 18,695 (16,380 with none, CPython's
    11,800). `threading_queue_ops` 1.82 to 1.64.
  - Lock hand-offs. A contended acquire took the lock as soon as it was
    free, while the GIL holder ran on; the holder then blocked at its next
    acquire of the same lock, so a producer and a consumer sharing a
    queue's mutex switched threads at every operation (34,000 context
    switches for 60,000 items against CPython's 45). The waiter now waits
    for the lock to come free with the GIL released and takes it only
    once the GIL is back, as CPython's does. `threading_queue_ops` 2.79 to
    1.82.
  - `yield from` two levels deep. The direct chain step declined a
    delegate whose own body no fast step runs, though a delegate resting
    in a `yield from` of its own steps without running its body, so each
    level resumed as an activation: 4.45 to 1.71 (one level was 1.85,
    three or more 1.0 or less).
  - A sweep of common idioms (`target/mb/idioms.py`-style loops, one
    function a case) found `datetime.datetime.now()` at 6.1 times
    CPython's instructions: `_pydatetime` ran `fromtimestamp` in Python
    (`time.localtime()` unpacked, the constructor, and the fold probe's two
    more `localtime` calls and datetime arithmetic). `now`, `fromtimestamp`
    and `date.today` are native for the exact classes with no `tzinfo` or
    an exact `timezone`, following `_fromtimestamp` step for step, with
    results identical to the Python methods' across DST gaps and folds in
    eight zones, and bound by the dispatch loop without the descriptor:
    `now()` 6.14 to 1.15, `date.today()` 0.56. f-string fields with a
    format spec left compiled code for the core loop at every field, and
    the formatter rendered `str(value)` before reading the spec:
    `f"{x:.2f}"` 1.49 to 0.46. `dict((k, v) for ...)` copied each pair into
    a vector and set up a key-comparison scope per insert: 1.40 to 1.01.
    `str.startswith` of a tuple took the generic call; a method read off a
    built-in value hashed its name into the method memo at every call (164
    instructions, 68 with a per-thread cache of each call site's last
    answer); `zip` of three to eight sequences stepped each source through
    the general checks (2.41 to 2.17).
  - Two more idiom sweeps (about 90 operations: binary formats, hashing,
    iteration helpers, application-level library calls). The cliffs, with
    their ratios before and after: `struct.pack` 4.30 to about 1.0 (a
    Python function forwarding `*v` to the packer, which copied and
    SipHashed the format text each call; now a native that packs plain
    values in place and calls the Python function for anything else);
    unpacking a `namedtuple` 4.27 to 0.96 (compiled code left for the core
    loop, which iterated the tuple subclass); `str.format_map` 3.72 to
    1.17 (always the interpreter's format engine, by a generic call);
    `list(reversed(xs))` 2.11 to 1.08 and `xs[::-1]` 1.94 to 1.10;
    `next(itertools.count())` 1.90 to 0.90 (`next` wasn't a leaf);
    `md5(b).hexdigest()` 2.06 to 1.53 (each byte formatted through
    `core::fmt`); a `csv` writer's `writerow` reading the dialect's
    attributes by name every row; a sort key's native `__call__` looked
    up per item; `heapq` fetching the interpreter before comparisons that
    don't need it.
  - Python dunders from native code. `str(x)`, `len(x)`, f-string
    formatting and `x in obj` of a plain instance call the class's
    `__str__`, `__len__` or `__contains__` without a bound method, in
    place when it's a pure leaf; a class without `__format__` is cached as
    such. The frame JIT's fused `len(local)` read declined instances, so
    each `len(obj)` reran the whole global load. `str(t)` 2.14 to 1.03,
    `f"{t}"` 2.31 to 1.36, `len(t)` 1.43 to 1.05, `3 in obj` 1.86 to
    0.68.
  - Smaller ones: a Python `__next__` called without a bound method by
    `for` and `next()` (and `iter()` of a builtin iterator subclass's
    instance is the instance, which fixed a subclass's `__next__` being
    skipped); `BUILD_SLICE` and slice subscripts in pure leaves; native
    `re.Pattern` hashing and equality; a keyword leaf half for
    `bytes.translate`; `list.sort()`'s leaf half comparing strings and
    ints directly; a property with a native getter read in the compiled
    attribute site.
  - The PGO training runs the everyday battery too (`tools/pgo/train.sh`),
    its profile merged with the rest rather than into the order file's
    hot group: the library paths it runs get branch weights and inlining.
    Two builds of one commit: the battery at 0.96 and 0.98 of the other's
    time over two runs, the suite at 1.000.
  - Fixed on the way: a method leaf called unbound with no receiver
    (`list.pop()`) indexed an empty argument list and panicked
    (`test_types.test_call_unbound_crash`).
  - Tried and dropped: `LOAD_GLOBAL`'s cache hit emitted in line. A loop
    reading a module global went from 182 instructions an iteration to
    116, but in real code (`dataclasses.asdict`, whose 35 global reads a
    call were all helper calls) the in-line guards cost what the helper
    did, for more code.

- **Operation-level cliffs in the everyday battery** (PR #112, November
  14). Each case broken into single operations, each operation's
  instructions counted against CPython's with synchronous compiles
  (`WEAVEPY_FRAME_JIT_SYNC=1`), and the outliers profiled in the
  container. Ratios are WeavePy's instructions over CPython's, before and
  after.
  - `bytes`. Methods read their receiver in place instead of copying it;
    a short search skips `memmem`'s set-up (`replace` scanned with a
    `memcmp` at every position); slices, indexing, plain `%` formatting,
    `bytes(n)` and `bytearray(b)` have fast paths. `replace` 3.08 to
    1.13, `b[5:20]` 3.08 to 0.93, `b"%d:%s" % args` 2.69 to 1.44,
    `bytes(10)` 2.60 to 0.78.
  - `datetime`. `astimezone` ran `_pydatetime`'s local-time search in
    Python (six or seven `time.localtime` calls and datetime arithmetic);
    `replace`, `timestamp` and `astimezone` are now native for exact
    classes with no `tzinfo` or a fixed-offset `timezone`, following
    `_mktime` and `_local_timezone` step for step, with results identical
    to the Python methods' across DST gaps and folds in five zones.
    `astimezone(utc)` 9.32 to 0.86.
  - `OSError`. Its family's builtin `__init__` kept every construction
    off the leaf path; the leaf path's instance already holds what that
    `__init__` sets. `OSError(errno, msg)` 6.22 to 1.98. `gettext`
    raises and catches one per catalog lookup, about 50 per
    `argparse.ArgumentParser()`.
  - Dunder reads by name. `getattr(cls, name)` and `hasattr(cls, name)`
    declined every dunder off a short list, so `dataclasses`'
    `hasattr(type(obj), "__dataclass_fields__")` took the full
    type-attribute path; a dunder `type` doesn't define and the full path
    doesn't synthesize now resolves in the class's MRO. 6.06 to 1.51.
  - Comparison methods resolved once. `x in xs` and sorting instances of
    one class with a Python `__eq__` or `__lt__` went through the whole
    rich-comparison dispatch per element; the method is now resolved once
    per scan or sort. `x in xs` 2.14 to 1.55, sorting 1.51 to 0.90.
  - Builtins called through the generic path. A native function compiled
    code calls without a leaf registration leaves for the interpreter's
    general call, about 2,500 instructions. `WEAVEPY_BURST_STATS`'
    "generic call" lines, summed over the battery minus an empty run,
    rank them; those whose plain shapes run no code now have leaf halves:
    `os.urandom`, `int.from_bytes` (one builtin per thread instead of a
    fresh one per read), `binascii.hexlify`, `round`, `repr` of scalars,
    `list.sort()` of strings or numbers, `weakref.ref.__init__`, and
    `str`, `tuple` and `int` `__new__` of a value of the type. A builtin
    method read off its type (`str.upper`) is a receiver-checking shim,
    which now takes the method's leaf kind. `secrets.token_hex(8)` 2.29
    to 0.97, `f = str.upper; f(s)` 2.49 to 0.96.
  - Smaller ones: `weakref.ref(x)` as a pure constructor (3.19 to 1.53);
    `dict(zip(xs, ys))` and `dict(d)` built directly (2.42 to 0.95);
    `functools.partial` callbacks unwrapped in `map` (5.18 to 1.09);
    `itertools.chain` of ranges drained at once (2.20 to 0.36); a
    property read through `getattr` in the leaf lane (3.36 to 1.03);
    `type(o).__name__` remembered under `ABCMeta` (3.45 to 0.56); the
    csv writer's dialect read once per `writerows` and plain fields
    copied whole (2.26 to 1.52); `os.stat`'s field names shared through
    a per-thread cache of literal strings (1.53 to 1.40); `json.dumps`'s
    `sort_keys` sorting `str` keys in place; a thread's type cache taken
    from zeroed memory, and leaf-builtin registrations no longer making
    every interpreter rebuild its map.
  - Tried and dropped: leaving loops whose work is Python calls on
    objects to the frame JIT. It cut one shape 4x (2,470 instructions an
    iteration to 573) and was neutral on the suite and the battery, but
    it handed loops whose calls tier 2 makes natively to the frame JIT,
    and the bench gate read `nbody` at 1.6 times its base.

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
  - A review of the PR found that the first of those two fixes had
    switched the descriptor call off. The site remembered "no descriptor
    here" for the class's version whenever the direct call declined, and
    the first call always declines, because the `__get__` it warms has no
    native code yet: `Color.RED.name` was back at 3,880 instructions a
    read at `4e230310`, which is the build the battery figures above were
    measured on. The site now remembers only a lookup that finds no such
    descriptor: 1,760 (CPython 1,510), and a sweep of the battery against
    the build before shows no case more than 1.5% up. The same review led
    to four more changes. A module loaded from a file before
    `importlib.machinery` is (a `-c` program's own imports) gets its
    `__spec__` and `__loader__` when its namespace is handed out as a
    dict (`vars(m)`, `m.__dict__`, `globals()`, `dir(m)`), where
    `'__spec__' in vars(m)` had become false; a program that never asks
    still skips the 4.9 ms. A text stream's `read(n)` counts decoded
    characters for every codec: it took a Latin-1 `\xe9` for the lead
    byte of a three-byte character, split a Shift JIS character in two,
    counted a byte-order mark, and read `a\r\nb` as `a\n\nb` when two
    `read(2)` calls split the `\r\n`, all since before this PR (16,200
    generated read sequences over 18 codecs now match CPython). The
    attribute hooks called from compiled sites are handed the code
    object's own name string, not a new one a call, and the `Arc`
    allocation's layout is worked out in one place for the five paths
    that free one by hand.
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

- **The battery's whole-case profile is flat** (November 14, callgrind
  over all 54 cases with synchronous compiles). The core loop's own body
  is 4.9% of the instructions, allocation and release about 7%, and no
  other function passes 2%; the layers around a native caller's call into
  Python are 2.5%, and compiling about 2% (9% to 14% in the shortest
  cases). What gave way was operation-level: a case broken into single
  operations and each counted against CPython shows one or two at 3 to
  9 times, a general path taken where a cached or native one was
  missing. Two measuring traps: a breakdown script's loops must each sit
  in a function of their own (one function with an `if` per case runs
  them interpreted, 3 times slower), and short runs under load measure
  loops still waiting for their background compile.
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

What PR #112's breakdowns left (instructions against CPython, quick
builds, synchronous compiles):

- `typing` subscripts (`typing.List[int]` 6.1, `Optional[str]` 5.8): the
  native subscript calls `_tp_cache`'s Python wrapper, which forwards
  `*args, **kwds` to the `lru_cache`, through the nested native-to-Python
  entry.
- `email`'s header parsing (setting one header 3.0) and `configparser`
  (`write` 2.8, `read_string` 2.0): flat profiles of interpreted library
  code, the core loop's call and return about 1,000 instructions a call.
- The remaining builtins called through the generic path, ranked by the
  battery's "generic call" counts: `getattr` misses (an `AttributeError`
  raised and caught), `str.join` and `list.extend` over generators,
  `next` of a generator, `print` with keywords, `StringIO.write`,
  `setattr` on plain instances, `len` and `hash` through Python methods.
- A raise caught in its caller (2.2) and three frames up (2.4).
- Thread start and join (4.6 times CPython's instructions): each thread
  zeroes and later drops a 4,096-entry type cache (128 KB) and tier 2
  analyzes again what another thread compiled. And code that runs while a
  second thread is alive still costs about 15% more than with one: the
  frame JIT's in-line reference-count updates go to helpers when counts
  are shared (the code generator has no atomic instructions), and cell
  borrows take the locked path.
- Keyword calls of a function with keyword-only parameters in a tier-2
  loop: tier 2's static binding requires `kwonly_count == 0`, so `f(i,
  c=4)` goes through its dynamic keyword helper (1,240 instructions
  against CPython's 1,060), where the frame JIT alone inlines the call in
  330.
- Exceptions raised in a callee (2.1 times CPython's instructions) and in
  the same frame (1.6): about 40 layered helpers of 50 to 300 instructions
  each (normalization, the instance, its traceback, frame shells for the
  traceback, the handler search per frame), no single one dominant.
- Generators: a resume through `gen_native_next` is about 300
  instructions, `lazy_gen_resume` (a body that calls) about 360, and
  creating one about 1,000, against CPython's 150 or so a resume (`for x
  in gen` 1.63, `all(f(x) for x in xs)` 1.40, a generator created and
  drained 1.94). Plan item 2.
- `decimal`: each operation finds the current context through the
  `contextvars` mapping and the context's fields, about 390 instructions
  of a multiplication's 1,600 (CPython 1,100).
- `dataclasses.asdict` (2.3) and `replace` (2.3), `IntFlag` `|` (1.7),
  a dataclass `repr` (1.9): flat profiles of interpreted library code.
- The frame JIT's generic call helper, `h_call` (about 250 to 300
  instructions before the callee runs: a chain of shape checks for
  `lru_cache` wrappers, `__call__`, `__reduce_ex__`, leaf builtins, then
  the general path), which an instance with a native `__call__`
  (`operator.itemgetter` 1.6), a builtin with keywords and
  `functools.partial` (1.8) all take. A call site that remembered its
  callee's shape could skip to it.
- `pickle.dumps` of a small dict (1.8): every call re-verifies about 13
  module functions the engine depends on (their code and defaults); a
  global epoch bumped by `__code__`, `__defaults__` and `__kwdefaults__`
  assignments would let it check a stamp instead.
- `string.Template.substitute` (1.9), `itertools.groupby` (1.8),
  `urllib.parse.quote` (1.6), `bisect.insort` (1.6), `random.randint`
  (1.7): Python-level library code over callbacks.
- Tier 2's method calls on built-in values it can't type: the counting
  idiom `d[k] = d.get(k, 0) + 1` runs 1.9 times CPython's instructions in
  tier 2 against 0.94 in the frame JIT alone, because tier 2 reads
  `d.get` dynamically, builds a bound method every iteration and calls it
  through its dynamic helper. A typed `DictGet` with a default (the
  analyzer already lowers `d[k]` on a dict lane) would close it.
- Two-argument `super(C, obj)` used other than for an immediate attribute
  read (5.95), and `super(...).__class__` (7.8).

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

- Start-up timings of two binaries differ by up to 8% for reasons that
  aren't in them. The same PGO binary read 21.5 ms for `-c pass` in one
  interleaved run and 19.7 ms two hours later, while the binary beside
  it went from 19.6 to 21.6 across a later pair of runs; a build's
  instruction count for the same start-up didn't move. Non-PGO builds
  of the commits in between were flat (75.3 M to 75.6 M instructions,
  cycles within 1.5%). Compare start-up by instructions and cycles
  (`/usr/bin/time -l`, least of fifteen runs), and treat a wall-clock
  gap under 10% between two binaries as unmeasured.
- A regression sweep of the everyday battery runs each case with
  `WEAVEPY_FRAME_JIT_SYNC=1` and compares raw instruction totals between
  two builds. With the background compiler on, the moment a compile
  lands moves a case by 5% between builds that are identical with it
  synchronous.
- Instruction counts (`/usr/bin/time -l`, or callgrind in the `wp-cg:latest`
  container) guide the work; wall time on a PGO build decides it. Pin
  `PYTHONHASHSEED=0` and unset `PYTHONDONTWRITEBYTECODE` for whole-process
  timings.
- Callgrind counts each iteration of `rep stos` as an instruction, so it
  overstates `memset` (zlib's state setup in `bytes_codecs`), and it shifts
  the JIT's time-budgeted compiles into later calls (`stdlib_utils`,
  `logging_mix` show compilation in steady state there, but not on macOS).

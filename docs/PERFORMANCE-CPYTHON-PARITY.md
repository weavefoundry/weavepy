# CPython parity performance

This branch works toward making WeavePy faster than CPython 3.14 on every
benchmark fixture and on the everyday costs outside them: startup, imports,
and memory. This report records where it stands, how the numbers were
measured, and what still trails CPython.

For the earlier passes, see [Performance measurements](PERFORMANCE.md) and
the reports it links to.

## Method

Timings come from the CI bench gate (`weavepy-bench gate`), run on the
macOS x86-64 development host (Intel Core i9-9980HK). It compares the
branch's release build with a release build of `main` (the merge base) and
with CPython 3.14.8, interleaving the three interpreters' runs on the same
machine. Each fixture's time is the median of its samples, measured by the
fixture's own timer (`WEAVEPY_BENCH_NS`), which excludes startup and
imports. Values below 1.00 mean the branch is faster.

Instruction counts come from `/usr/bin/time -l` ("instructions retired")
on the host, or from callgrind in a Linux container when a per-function or
per-line breakdown is needed. A per-operation cost is the difference between
two work sizes divided by the difference in work, which cancels startup.
Instruction counts are deterministic enough to compare builds on a busy
machine; they don't capture cache or branch behavior, so a wall-time
checkpoint confirms them.

## Benchmark fixtures

Geometric mean over the 24 fixtures: **0.50** of CPython's time and
**0.75** of `main`'s.

| Fixture | Work | Branch | `main` | CPython | vs `main` | vs CPython |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `deltablue` | 50 | 244.3 ms | 292.5 ms | 80.5 ms | 0.83 | 3.03 |
| `generators` | 300,000 | 64.0 ms | 76.8 ms | 58.0 ms | 0.83 | 1.10 |
| `dict_ops` | 100,000 | 68.1 ms | 82.2 ms | 64.2 ms | 0.83 | 1.06 |
| `pickle_bench` | 40 | 9.7 ms | 15.9 ms | 10.2 ms | 0.61 | 0.95 |
| `float_math` | 100,000 | 86.8 ms | 112.8 ms | 92.5 ms | 0.77 | 0.94 |
| `deque_ops` | 200,000 | 65.7 ms | 134.5 ms | 74.7 ms | 0.49 | 0.88 |
| `json_bench` | 150 | 76.8 ms | 93.3 ms | 96.8 ms | 0.82 | 0.79 |
| `list_ops` | 10,000 | 36.4 ms | 38.7 ms | 46.0 ms | 0.94 | 0.79 |
| `startup` | 1 | 19.4 ms | 20.6 ms | 24.5 ms | 0.94 | 0.79 |
| `fannkuch` | 100,000 | 16.8 ms | 24.4 ms | 22.4 ms | 0.69 | 0.75 |
| `nbody` | 20,000 | 37.9 ms | 50.1 ms | 50.9 ms | 0.76 | 0.74 |
| `str_methods` | 15,000 | 48.7 ms | 83.1 ms | 70.5 ms | 0.59 | 0.69 |
| `call_overhead` | 150,000 | 67.3 ms | 109.3 ms | 97.5 ms | 0.62 | 0.69 |
| `attr_access` | 200,000 | 37.8 ms | 64.2 ms | 57.0 ms | 0.59 | 0.66 |
| `pyaes` | 400 | 19.5 ms | 19.1 ms | 30.4 ms | 1.02 | 0.64 |
| `pidigits` | 500,000 | 1,580 ms | 1,680 ms | 2,570 ms | 0.94 | 0.61 |
| `richards` | 50,000 | 11.7 ms | 20.3 ms | 21.5 ms | 0.58 | 0.55 |
| `datetime_ops` | 60,000 | 30.7 ms | 84.2 ms | 60.8 ms | 0.36 | 0.51 |
| `jitkernels` | 2,000 | 21.6 ms | 34.9 ms | 58.8 ms | 0.62 | 0.37 |
| `fib` | 27 | 8.9 ms | 9.3 ms | 24.6 ms | 0.96 | 0.36 |
| `spectral_norm` | 100 | 12.8 ms | 13.3 ms | 63.9 ms | 0.97 | 0.20 |
| `nested_loops` | 120 | 7.3 ms | 7.4 ms | 86.2 ms | 0.99 | 0.08 |
| `jitloop` | 1,000 | 8.7 ms | 8.9 ms | 111.7 ms | 0.99 | 0.08 |
| `sumvm` | 2,000,000 | 4.3 ms | 4.4 ms | 85.8 ms | 0.99 | 0.05 |

The largest gains over `main` came from these changes:

- **A baseline frame compiler** (`frame_jit`) runs loops that tier 2
  declines, such as loops of method calls and container operations, in
  native code. It compiles a loop only when most of its instructions stay
  native, since every instruction that leaves for the core loop costs a
  round trip.
- **Native containers and values.** `collections.deque`, `datetime` values,
  `pickle`, compiled regular expressions and their matches, and `map` and
  `filter` over native containers run natively instead of through their
  pure-Python implementations.
- **Cheaper calls and exceptions.** Small functions that native code calls
  (`key=lambda t: t[1]`) run without an activation, tracebacks are built
  only when something reads them, and the core loop catches exceptions
  without leaving.
- **Class bodies and metaclass reads.** Class creation and metaclass
  attribute reads no longer pass through the general call path.

## Startup and imports

Whole-process instructions retired and peak memory, warm stdlib cache:

| Command | WeavePy | CPython | WeavePy RSS | CPython RSS |
| --- | ---: | ---: | ---: | ---: |
| `-c pass` | 77M | 103M | 11.7 MiB | 10.5 MiB |
| `import json` | 177M | 141M | 24.3 MiB | 12.6 MiB |
| `import dataclasses` | 282M | 183M | 32.9 MiB | 13.9 MiB |
| `import logging` | 421M | 241M | 38.8 MiB | 16.0 MiB |
| `import asyncio` | 577M | 350M | 48.0 MiB | 19.1 MiB |
| `import unittest` | 407M | 253M | 40.4 MiB | 16.0 MiB |

Startup is cheaper than CPython's, and imports retire 14% to 19% fewer
instructions than when this report was first written. Importing large parts
of the standard library still costs 1.3 to 1.8 times as much as in CPython.
These changes did most of the work:

- Compiled stdlib modules are cached in a WeavePy-native code format
  (`weavepy_compiler::native_code`, now version 3), which loads without
  unmarshalling CPython bytecode and transcoding it back into WeavePy
  instructions. Column tables stay encoded until something reads them.
- Slicing a tuple, `bytes`, `bytearray`, or a string with surrogates no longer
  copies the whole source sequence. `re.compile` with `IGNORECASE` slices a
  64K-entry charset map 256 times, which had made `import logging` cost 1.19B
  instructions.
- The collector's per-drop sweep of suspected-dead objects skips entries that
  have used up their probe budget, so it no longer walks up to 256 entries at
  every drop during imports.
- The opcode shim builds its stack-effect tables on first use, and the frame
  compiler stays within tier 2's start-up and import budget.

Peak memory is where WeavePy trails most: two to two and a half times
CPython's after a large import, and up to twice CPython's on the fixtures. An
allocation-site profile attributes most of the difference to compiled code
objects: their per-code structure, per-instruction line tables, and the
materialized constants and names built on first use.

## What still trails CPython

`deltablue` (3.03) and `generators` (1.10) are still slower than CPython,
and `dict_ops` is at parity (between 0.92 and 1.06 across runs). They
spend their time in operations that CPython's specialized bytecodes do in
a few dozen instructions. Per operation, with
the default settings, measured in a loop that tier 2 compiles:

| Operation | WeavePy | CPython |
| --- | ---: | ---: |
| Instance attribute read (`o.x`) | 64 | 37 |
| Class attribute read through an instance (`o.K`) | 357 | 36 |
| Global read | under 5 | 20 |
| `isinstance(o, C)` | 1,635 | 271 |
| Call of a small non-leaf function | 1,389 | 763 |

Global reads and the arithmetic around them compile to native code. The
rest leave compiled code for a general helper: a class attribute read
through an instance resolves through the class on every read, and
`isinstance` and calls to Python functions go through the generic call
path. Teaching tier 2 and the frame compiler those three shapes is the
next step for `deltablue`.

The interpreter's structure adds to these costs where nothing compiles:

- **Loop state.** The core loop keeps about 17 values live across its arms,
  which contain calls; x86-64 has six callee-saved registers, so most of the
  state lives on the stack and every dispatch reloads it.
- **Data layout.** An instance field read passes the site's slot table, the
  class version, the class's shared keys, and the instance's split values,
  each behind its own pointer; CPython checks a type version and reads an
  inline slot.
- **Calls.** An inline activation binds cells, pools its locals vector,
  grades its locals for collector bookkeeping on exit, and switches the
  running frame, several hundred instructions per call and return.

`deltablue` also runs at a lower IPC than CPython (2.13 against 2.47): its
interpreter loops overflow the instruction cache, and their single dispatch
branches predict poorly.

## Validation

The branch passes the workspace's unit, integration, and doc tests, the
bundled regression suite (272 tests), its free-threaded lane, the tools'
tests, and the bench gate against `main` with no fixture regressing.

The curated CPython 3.14 gate (616 suites, among them `test_re`,
`test_pickle`, `test_deque`, `test_datetime`, `test_generators`,
`test_exceptions`, `test_threading`, `test_signal`, `test_asyncio`, and
`test_sys_settrace`) passes except where `main` fails the same way on the
development host: `test_capi`, `test_genexps`, `test_tempfile`, and
`test_utf8_mode` fail there too, and the `multiprocessing` spawn and
forkserver suites time out on both.

New regression tests cover the frame compiler, frameless subscripts,
native `map` and `filter`, packed `datetime` values, deque lanes in
compiled code, unraisable `OSError` reports, and interrupts that arrive
while code only calls and catches.

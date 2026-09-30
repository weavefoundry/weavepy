# CPython parity performance

This branch works toward making WeavePy faster than CPython 3.14 on every
benchmark fixture and on the everyday costs outside them: startup, imports,
and memory. This report records where it stands, how the numbers were
measured, and what still trails CPython.

For the earlier passes, see [Performance measurements](PERFORMANCE.md) and
the reports it links to.

## Method

Timings compare a profile-guided release build of the branch
(`tools/pgo_build.py --skip-regrtest`, JIT on) with CPython 3.14.7 on the
macOS x86-64 development host (Intel Core i9-9980HK). Both executables are
native x86-64.

Wall-time ratios come from paired, interleaved runs. Each cycle runs every
fixture once under each interpreter at the harness work sizes, alternating
which goes first; one warmup cycle is discarded and five are measured. A
fixture's ratio is the median of its per-cycle WeavePy/CPython ratios, using
each fixture's own timer (`WEAVEPY_BENCH_NS`), which excludes startup and
imports. Values below 1.00 mean WeavePy is faster.

Instruction counts come from `/usr/bin/time -l` ("instructions retired")
on the host, or from callgrind in a Linux container when a per-function or
per-line breakdown is needed. A per-operation cost is the difference between
two work sizes divided by the difference in work, which cancels startup.
Instruction counts are deterministic enough to compare builds on a busy
machine; they don't capture cache or branch behavior, so a wall-time
checkpoint confirms them.

## Benchmark fixtures

Geometric mean over the 23 timed fixtures: **0.592**.

| Fixture | Work | WeavePy | CPython | Ratio |
| --- | ---: | ---: | ---: | ---: |
| `deltablue` | 50 | 213.8 ms | 67.8 ms | 3.15 |
| `deque_ops` | 200,000 | 97.9 ms | 64.1 ms | 1.53 |
| `pickle_bench` | 40 | 11.6 ms | 8.5 ms | 1.34 |
| `generators` | 300,000 | 59.3 ms | 49.6 ms | 1.19 |
| `datetime_ops` | 60,000 | 50.7 ms | 52.7 ms | 0.98 |
| `str_methods` | 15,000 | 57.0 ms | 59.9 ms | 0.95 |
| `float_math` | 100,000 | 78.0 ms | 81.6 ms | 0.94 |
| `dict_ops` | 100,000 | 55.9 ms | 61.7 ms | 0.90 |
| `call_overhead` | 150,000 | 69.8 ms | 82.2 ms | 0.84 |
| `fannkuch` | 100,000 | 17.1 ms | 20.9 ms | 0.83 |
| `richards` | 50,000 | 16.1 ms | 20.3 ms | 0.80 |
| `nbody` | 20,000 | 33.0 ms | 42.9 ms | 0.77 |
| `attr_access` | 200,000 | 35.4 ms | 47.1 ms | 0.77 |
| `list_ops` | 10,000 | 33.8 ms | 45.1 ms | 0.75 |
| `json_bench` | 150 | 57.4 ms | 81.1 ms | 0.71 |
| `pidigits` | 500,000 | 1,421.0 ms | 2,167.8 ms | 0.67 |
| `pyaes` | 400 | 15.8 ms | 28.9 ms | 0.54 |
| `jitkernels` | 2,000 | 24.3 ms | 49.0 ms | 0.48 |
| `fib` | 27 | 8.0 ms | 21.6 ms | 0.37 |
| `spectral_norm` | 100 | 12.1 ms | 57.7 ms | 0.21 |
| `nested_loops` | 120 | 6.2 ms | 69.3 ms | 0.09 |
| `jitloop` | 1,000 | 7.5 ms | 92.0 ms | 0.08 |
| `sumvm` | 2,000,000 | 3.8 ms | 72.5 ms | 0.05 |

Times are the medians of each interpreter's samples; ratios are the medians
of the paired ratios, so they needn't equal the quotient of the two times.

## Startup and imports

Whole-process instructions retired, warm stdlib cache:

| Command | WeavePy | CPython |
| --- | ---: | ---: |
| `-c pass` | 91M | 127M |
| `import json` | 205M | 153M |
| `import dataclasses` | 349M | 196M |
| `import logging` | 513M | 258M |
| `import asyncio` | 689M | 365M |
| `import unittest` | 496M | 270M |

Startup is cheaper than CPython's. Importing large parts of the standard
library still costs about twice as much. Three changes on this branch cut
those costs by 20% to 60%:

- Compiled stdlib modules are cached in a WeavePy-native code format
  (`weavepy_compiler::native_code`), which loads without unmarshalling CPython
  bytecode and transcoding it back into WeavePy instructions.
- Slicing a tuple, `bytes`, `bytearray`, or a string with surrogates no longer
  copies the whole source sequence. `re.compile` with `IGNORECASE` slices a
  64K-entry charset map 256 times, which had made `import logging` cost 1.19B
  instructions.
- The collector's per-drop sweep of suspected-dead objects skips entries that
  have used up their probe budget, so it no longer walks up to 256 entries at
  every drop during imports.

Peak memory after these imports is about twice CPython's (for example,
23 MiB against 12 MiB for `import json`). An allocation-site profile
attributes most of the difference to compiled code objects: their per-code
structure, per-instruction line and column tables, and the materialized
constants and names built on first use.

## What still trails CPython

The four slower fixtures share a cause: operations that CPython's specialized
bytecodes do in 10 to 30 instructions take WeavePy two to four times as many.
Per operation, with the JIT off:

| Operation | WeavePy | CPython |
| --- | ---: | ---: |
| Instance attribute read (`o.x`) | 160 | 64 |
| Class attribute read through an instance (`o.K`) | 240 | 60 |
| Global read | 150 | 50 |
| `isinstance(o, C)` | 980 | 240 |
| Call of a small non-leaf function | 1,700 | 440 |

The costs come from the interpreter's structure rather than from any single
slow path:

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

Every change on the branch passes the VM unit tests (420), the bundled
regression suite (248), the semantics comparison scripts, and the CPython
regression suites most exposed to it. The changes described above were
checked with, among others, `test_bytes`, `test_tuple`, `test_slice`,
`test_str`, `test_re`, `test_marshal`, `test_import`, `test_code`,
`test_compile`, `test_logging`, `test_gc`, `test_weakref`, `test_io`,
`test_ssl`, `test_asyncio`, `test_scope`, `test_global`, `test_module`,
`test_builtin`, `test_sys_settrace`, and `test_monitoring`.

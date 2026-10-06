# Realistic workload performance

This branch measures WeavePy against CPython 3.14 on 33 realistic
workloads, not just the bench gate's fixtures, and works down the gaps they
expose. This report records where the branch stands, how the numbers were
measured, what changed, and what still trails CPython. It's an interim
checkpoint: the work continues, and the gaps listed at the end are the next
targets.

For the bench gate fixtures, startup, and imports, see
[CPython parity performance](PERFORMANCE-CPYTHON-PARITY.md).

## Method

The workloads live in `tools/pybench/benchmarks`. Each module defines
`bench(n)` and a default `WORK`; `tools/pybench/harness.py` imports it,
calls `bench` once to warm up (reported as `cold`), then times further
calls. The workloads cover generators and coroutines, asyncio, exceptions,
context managers, object-oriented code, closures and decorators,
`deepcopy`, `pickle`, `json`, `re`, `xml.etree`, `sqlite3`, `decimal`,
`struct` and codecs, `tomllib`, `logging`, and a few classic kernels.

Two measurements back each number:

- **Instructions retired** (`tools/pybench/run.py --instructions`) for the
  steady-state `bench` call: the difference between a run that calls it
  once and one that calls it twice, from `/usr/bin/time -l`. The counts are
  deterministic enough to compare builds on a busy machine, so they guided
  the work. They come from `quick` profile builds.
- **Wall time and peak resident memory** (`tools/pybench/run.py`) from
  release builds, interleaving the interpreters' runs; each time is the
  median of the warm calls. A stray process occupied one core during this
  checkpoint's timing run, so the times are less exact than the counts.

The host is a macOS x86-64 machine (Intel Core i9-9980HK). CPython is
3.14.8. "Start" is the merge base with `main`, `18a9ea4`.

## Results

Geometric means over the 33 workloads:

| Measure | Branch vs CPython | Branch vs start (`main`) |
| --- | ---: | ---: |
| Instructions | 2.10 | 0.52 |
| Wall time (warm) | 2.53 | 0.48 |
| Peak resident memory | 1.98 | |

At the start, WeavePy retired 4.04 times CPython's instructions on this
suite. Startup now beats CPython outright: `weavepy -c pass` takes 19.7 ms
(median of 20 runs) against CPython's 25.8 ms, retires 81 million
instructions against 117 million, and peaks at 10.3 MB against 11.0 MB.

Per workload, ordered by the instruction ratio (lower is better; below
1.00 beats CPython):

| Workload | Instructions vs CPython | Instructions vs start | Time vs CPython | Time vs `main` | Peak RSS vs CPython |
| --- | ---: | ---: | ---: | ---: | ---: |
| `json_roundtrip` | 0.77 | 0.98 | 0.81 | 0.97 | 1.61 |
| `pickle_roundtrip` | 0.81 | 0.99 | 0.77 | 0.95 | 1.66 |
| `scimark` | 0.93 | 0.28 | 1.23 | 0.35 | 1.96 |
| `regex_mix` | 1.05 | 0.95 | 1.12 | 0.96 | 1.75 |
| `xml_etree` | 1.09 | 0.09 | 1.30 | 0.07 | 2.01 |
| `decimal_telco` | 1.15 | 0.01 | 1.05 | 0.00 | 2.21 |
| `unpack_sequence` | 1.20 | 0.83 | 1.33 | 0.73 | 1.79 |
| `sorting` | 1.42 | 0.88 | 1.36 | 0.83 | 1.90 |
| `class_creation` | 1.62 | 0.82 | 1.96 | 0.83 | 1.93 |
| `collections_mix` | 1.63 | 0.10 | 2.45 | 0.07 | 1.87 |
| `chaos` | 1.73 | 0.75 | 2.32 | 0.67 | 2.21 |
| `float_points` | 1.74 | 0.94 | 1.59 | 0.94 | 1.75 |
| `text_processing` | 1.74 | 0.69 | 1.77 | 0.63 | 2.19 |
| `comprehensions` | 2.05 | 0.69 | 2.25 | 0.65 | 4.23 |
| `oo_patterns` | 2.09 | 0.66 | 2.90 | 0.60 | 1.87 |
| `exceptions` | 2.23 | 0.97 | 4.60 | 1.12 | 1.67 |
| `graph_search` | 2.49 | 0.69 | 2.79 | 0.69 | 1.73 |
| `coroutines` | 2.79 | 0.34 | 3.15 | 0.28 | 1.60 |
| `sudoku` | 2.79 | 0.75 | 2.78 | 0.69 | 1.83 |
| `nqueens` | 2.81 | 0.65 | 3.65 | 0.54 | 1.85 |
| `tomllib_loads` | 2.84 | 0.55 | 3.50 | 0.57 | 2.26 |
| `raytrace` | 2.89 | 0.81 | 3.84 | 0.83 | 1.91 |
| `mini_interpreter` | 2.95 | 0.78 | 3.51 | 0.78 | 1.82 |
| `go` | 2.97 | 0.82 | 3.26 | 0.71 | 2.05 |
| `logging_mix` | 3.08 | 0.79 | 3.68 | 0.74 | 2.03 |
| `stdlib_utils` | 3.11 | 0.61 | 3.24 | 0.58 | 2.34 |
| `closures_decorators` | 3.16 | 0.73 | 4.27 | 0.70 | 1.67 |
| `bytes_codecs` | 3.29 | 0.72 | 4.29 | 0.68 | 1.93 |
| `sqlite_queries` | 3.37 | 1.00 | 3.73 | 1.01 | 1.86 |
| `context_managers` | 3.39 | 0.64 | 6.75 | 0.62 | 1.93 |
| `deepcopy` | 3.61 | 0.42 | 5.85 | 0.37 | 2.15 |
| `async_tree` | 3.62 | 0.29 | 5.12 | 0.25 | 2.83 |
| `generators_tree` | 4.05 | 0.25 | 4.65 | 0.23 | 2.23 |

The instruction counts come from commit `f14c313` less its last two
changes (the core-loop raise and the `sqlite3` cursor step); the times come
from an earlier release build of the same series. The exception-heavy and
`sqlite3` rows are the ones those changes move.

## What changed

The work followed one loop: find where a workload leaves the interpreter's
fast paths (the core loop's slow-step and handoff counters,
`WEAVEPY_BURST_STATS=1`), or where its time goes (sampled profiles and
per-function instruction estimates), then remove the cost at its source.

**Generators and coroutines.** A resumed `yield from` chain now descends
with one lean `SEND` hop per level and climbs back with the yielded value in
hand, without dispatching each consumer's own yield; a level costs about 470
instructions instead of 1,010. A generator's `return` finishes it in the
core loop, a fast generator step can run a body to its `return`, and
`iter(obj)` and `await obj` make an instance's generator `__iter__` or
`__await__` in place. Creating a generator no longer interns its names or
allocates for its arguments, and finishing a young one skips the cycle
collector's index, which holds no entry for it.

**Calls.** A function with a loop runs inline until the JIT has compiled it,
then calls enter the compiled form when the loop averages at least two
iterations a call. Builtins registered as leaves run in place over spread
arguments (`max(*xs)`), container constructors over one iterable run in a
lane with the caller published, and `super()` resolves in place under any
metaclass.

**Core-loop coverage.** String comparisons, f-string conversion and
assembly, `bool` operands in integer arithmetic, `int()` of a float, bytes
concatenation and in-place `bytearray +=`, a zero float raised to a power,
the `except` clause's entry, test, and exit, and `raise` of an exception
instance or builtin class all run in the core loop instead of a full step.

**Attributes and keys.** Dictionaries and sets key by functions,
generator-family objects, modules, and tuples of ints and strings without
calling into Python; class attributes resolve through metaclasses such as
`ABCMeta` and `EnumType`; `cls.__doc__`, frame and code attributes, and an
instance's class-level native methods (`re.Pattern.match`) read in place; a
natively built module's keys become the interned names on their first
cached read.

**Representation.** Cloning or releasing an object bumps one strong-count
word found from its tag, with no per-variant dispatch. Scalars are built as
two whole words, and inline cache reads no longer decode through an
out-of-line closure; both had stalled on store forwarding.
`Rc::downgrade` bumps the weak count in place while the single-thread bias
holds.

**Libraries.** `_struct`, `os.fspath`, `_thread.get_ident`, parts of
`_asyncio`, and `sqlite3` cursor iteration run in place when they can't run
Python code.

**Memory.** The tier-2 cache no longer keeps every executed code object
alive, fewer results stay pinned, per-instruction side tables wait until
code is warm, line tables decode lazily, mimalloc reserves 64 MiB arenas,
and function slot storage is boxed on first use: suite peak memory fell
about 19% and the import set's 27%.

## What still trails

- **Interpreter dispatch.** A simple bytecode costs about 35 to 50
  instructions in the core loop against CPython's 15. The loop is one very
  large function, so its state lives in stack slots and every dispatch
  reloads it. Code the frame JIT compiles runs 2 to 10 times faster, but a
  round trip from native code to the core loop costs more than interpreting
  the instruction, so the compile heuristics rightly decline code with many
  such exits.
- **Exceptions.** Raising and catching costs about three times CPython's
  instructions and more than four times its time: each exception builds a
  slot table, a traceback object and a legacy traceback entry, and the
  catch materializes the frame's shell.
- **Generators.** A `yield from` level still costs about four times
  CPython's, and creating and finishing a generator about three times.
- **Instructions per cycle.** Wall-time ratios run above instruction ratios
  (2.53 against 2.10): store-forwarding stalls on object moves, the core
  loop's single dispatch branch, and pointer-heavy object layouts.
- **Memory.** Peak resident memory is about twice CPython's: per-object
  headers, the cycle collector's per-object bookkeeping, and decoded code
  objects dominate.

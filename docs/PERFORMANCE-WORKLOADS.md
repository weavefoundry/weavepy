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
  median of the warm calls. The release binary is profile-guided
  (`tools/pgo/build.sh`), as CPython's own release builds are.

The host is a macOS x86-64 machine (Intel Core i9-9980HK). CPython is
3.14.8. "Start" is the merge base with `main`, `18a9ea4`.

## Results

Geometric means over the 33 workloads:

| Measure | Branch vs CPython | Branch vs start (`main`) |
| --- | ---: | ---: |
| Instructions | 1.85 | 0.46 |
| Wall time (warm) | 1.87 | |
| Wall time (cold) | 2.05 | |
| Peak resident memory | 1.78 | |

At the start, WeavePy retired 4.04 times CPython's instructions on this
suite, and an earlier checkpoint measured 2.53 times CPython's warm time.
Against a plain release build of the same source, the profile-guided build
runs the workloads 17% faster; trained on half of them only, it ran the
other half 12% faster. Startup beats CPython outright: `weavepy -c pass`
retires 81 million instructions against CPython's 105 million.

Per workload, ordered by the instruction ratio (lower is better; below
1.00 beats CPython):

| Workload | Instructions vs CPython | Instructions vs start | Time vs CPython | Peak RSS vs CPython |
| --- | ---: | ---: | ---: | ---: |
| `json_roundtrip` | 0.77 | 1.00 | 0.71 | 1.47 |
| `pickle_roundtrip` | 0.81 | 0.98 | 0.63 | 1.54 |
| `scimark` | 0.89 | 0.27 | 0.85 | 1.69 |
| `float_points` | 0.92 | 0.49 | 0.84 | 1.60 |
| `regex_mix` | 1.04 | 0.95 | 0.85 | 1.62 |
| `xml_etree` | 1.05 | 0.09 | 1.13 | 1.74 |
| `decimal_telco` | 1.14 | 0.01 | 0.78 | 2.05 |
| `unpack_sequence` | 1.16 | 0.81 | 1.22 | 1.66 |
| `sorting` | 1.37 | 0.85 | 1.21 | 1.75 |
| `class_creation` | 1.55 | 0.78 | 1.64 | 1.81 |
| `sqlite_queries` | 1.56 | 0.47 | 1.49 | 1.66 |
| `collections_mix` | 1.59 | 0.10 | 1.77 | 1.78 |
| `chaos` | 1.66 | 0.72 | 1.87 | 2.08 |
| `text_processing` | 1.74 | 0.69 | 1.53 | 2.09 |
| `comprehensions` | 1.76 | 0.60 | 1.71 | 1.90 |
| `bytes_codecs` | 1.77 | 0.39 | 1.79 | 1.86 |
| `exceptions` | 1.91 | 0.84 | 2.86 | 1.52 |
| `oo_patterns` | 2.06 | 0.65 | 2.17 | 1.79 |
| `raytrace` | 2.32 | 0.65 | 2.33 | 1.80 |
| `graph_search` | 2.38 | 0.66 | 2.14 | 1.66 |
| `sudoku` | 2.49 | 0.70 | 2.34 | 1.70 |
| `closures_decorators` | 2.53 | 0.59 | 3.47 | 1.57 |
| `tomllib_loads` | 2.54 | 0.49 | 2.48 | 2.11 |
| `coroutines` | 2.56 | 0.31 | 2.76 | 1.44 |
| `nqueens` | 2.63 | 0.61 | 2.81 | 1.73 |
| `go` | 2.72 | 0.72 | 2.62 | 1.87 |
| `mini_interpreter` | 2.85 | 0.75 | 2.86 | 1.71 |
| `stdlib_utils` | 2.89 | 0.57 | 2.59 | 2.16 |
| `logging_mix` | 3.02 | 0.78 | 3.24 | 1.90 |
| `context_managers` | 3.03 | 0.56 | 4.29 | 1.78 |
| `async_tree` | 3.17 | 0.25 | 3.71 | 2.27 |
| `deepcopy` | 3.26 | 0.38 | 4.02 | 2.02 |
| `generators_tree` | 3.86 | 0.24 | 3.76 | 1.86 |

The instruction counts come from a `quick` build of commit `9a3d979`, the
times and memory from a profile-guided release build of the same commit.
Six workloads now beat CPython's time: `pickle_roundtrip`,
`json_roundtrip`, `decimal_telco`, `float_points`, `scimark`, and
`regex_mix`.

## What changed

The work followed one loop: find where a workload leaves the interpreter's
fast paths (the core loop's slow-step and handoff counters,
`WEAVEPY_BURST_STATS=1`), or where its time goes (sampled profiles and
per-function instruction estimates), then remove the cost at its source.

**Since the previous checkpoint.** The profile-guided build (above) is the
largest single step. Beyond it:

- *Tier-2 heuristics.* A loop header the compiled frame can't enter no
  longer spends the code's OSR budget (which had left `float_points`' warm
  calls interpreted: 1.74 to 0.92 times CPython's instructions); code whose
  loops run fewer than two iterations a call goes to the frame JIT instead
  of holding its back edges; and a compiled loop that is mostly calls into
  the interpreter, or that keeps raising, retires when it exits instead of
  only at a poll.
- *Exceptions.* User exception classes construct through the lean
  `__init__` path with `args` seeded as `BaseException_new` seeds it, and
  `super().__init__` reaches `BaseException.__init__` in the core loop
  (constructing one: 15k to 6.7k instructions).
- *Objects.* The last release of an object frees it without `Arc`'s locked
  decrements; deleting an instance attribute skips three class lookups; a
  suspended generator outside any handler of its own closes at its last
  release instead of through the finalizer queue.
- *Libraries.* `lru_cache` reads its own fields by position and probes its
  cache under one comparison scope (`closures_decorators`: 3.09 to 2.53
  times CPython's instructions); `getattr` of a dunder nothing supplies
  answers the default directly (`copy`'s `__deepcopy__` probe); compiled
  leaf methods read fields past unrelated buffered stores.

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

- **Calls.** A call to a function the frameless leaf paths can't evaluate
  costs about 1,700 instructions against CPython's 450 (a loop, a store to
  a global, or a nested non-leaf call is enough): the inline activation's
  binding and teardown, and the core loop's reload at each call and return.
  This is the broadest remaining cost.
- **Interpreter dispatch.** A simple bytecode costs about 35 to 50
  instructions in the core loop against CPython's 15. The loop is one very
  large function, so its state lives in stack slots and every dispatch
  reloads it.
- **Generators.** A `yield from` level still costs about four times
  CPython's, and creating a generator about three times (four allocations
  against CPython's one).
- **Exceptions.** Raising and catching a builtin exception costs about 2.3
  times CPython's instructions: each raise builds a slot table, a traceback
  object and a legacy traceback entry, and the catch materializes the
  frame's shell.
- **Instructions per cycle.** The binary carries 47 MB of code, and these
  workloads touch hundreds of its functions per iteration; profile-guided
  layout recovered part of the instruction-cache cost.
- **Memory.** Peak resident memory is about 1.8 times CPython's: per-object
  headers, the cycle collector's per-object bookkeeping, and decoded code
  objects dominate.

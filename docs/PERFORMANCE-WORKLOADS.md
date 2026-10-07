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
| Instructions | 1.75 | 0.44 |
| Wall time (warm) | 1.73 | |
| Wall time (cold) | 1.94 | |
| Peak resident memory | 1.89 | |

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
| `json_roundtrip` | 0.77 | 1.00 | 0.70 | 1.44 |
| `pickle_roundtrip` | 0.80 | 0.97 | 0.64 | 1.57 |
| `scimark` | 0.87 | 0.27 | 0.77 | 1.79 |
| `float_points` | 0.90 | 0.48 | 0.82 | 1.55 |
| `unpack_sequence` | 1.01 | 0.71 | 1.04 | 1.71 |
| `xml_etree` | 1.04 | 0.09 | 1.09 | 1.75 |
| `regex_mix` | 1.04 | 0.95 | 0.83 | 1.66 |
| `decimal_telco` | 1.14 | 0.01 | 0.78 | 2.11 |
| `sorting` | 1.35 | 0.83 | 1.19 | 1.97 |
| `chaos` | 1.36 | 0.59 | 1.60 | 2.36 |
| `oo_patterns` | 1.52 | 0.48 | 1.60 | 1.94 |
| `collections_mix` | 1.53 | 0.10 | 1.67 | 1.75 |
| `sqlite_queries` | 1.54 | 0.46 | 1.49 | 1.73 |
| `class_creation` | 1.64 | 0.82 | 1.69 | 1.87 |
| `bytes_codecs` | 1.73 | 0.38 | 1.71 | 1.61 |
| `text_processing` | 1.74 | 0.69 | 1.55 | 2.09 |
| `raytrace` | 1.76 | 0.49 | 1.67 | 1.88 |
| `comprehensions` | 1.77 | 0.60 | 1.64 | 4.08 |
| `exceptions` | 1.92 | 0.85 | 2.52 | 1.57 |
| `graph_search` | 2.19 | 0.61 | 1.93 | 1.73 |
| `sudoku` | 2.28 | 0.64 | 2.14 | 1.76 |
| `go` | 2.32 | 0.61 | 2.34 | 1.96 |
| `nqueens` | 2.37 | 0.55 | 2.44 | 1.78 |
| `closures_decorators` | 2.43 | 0.57 | 2.88 | 1.76 |
| `coroutines` | 2.45 | 0.30 | 2.48 | 1.48 |
| `mini_interpreter` | 2.47 | 0.65 | 2.45 | 1.93 |
| `tomllib_loads` | 2.47 | 0.48 | 2.47 | 2.27 |
| `context_managers` | 2.85 | 0.53 | 3.93 | 1.83 |
| `stdlib_utils` | 2.87 | 0.57 | 2.50 | 2.29 |
| `deepcopy` | 2.96 | 0.34 | 3.61 | 2.08 |
| `logging_mix` | 3.03 | 0.78 | 3.24 | 2.03 |
| `async_tree` | 3.10 | 0.24 | 3.73 | 2.20 |
| `generators_tree` | 3.75 | 0.23 | 3.59 | 2.14 |

The instruction counts come from a `quick` build of commit `c2e5d2c`, the
times and memory from a profile-guided release build of the same commit.
Six workloads beat CPython's time: `pickle_roundtrip`, `json_roundtrip`,
`scimark`, `decimal_telco`, `float_points`, and `regex_mix`.

## What changed

The work followed one loop: find where a workload leaves the interpreter's
fast paths (the core loop's slow-step and handoff counters,
`WEAVEPY_BURST_STATS=1`), or where its time goes (sampled profiles and
per-function instruction estimates), then remove the cost at its source.

**Since the previous checkpoint.** Most of this round moved work out of
the core loop and into the frame JIT's native code, where a compiled
function had been leaving for the interpreter at each instruction it
didn't cover:

- *Direct calls.* A compiled function's call of a plain Python function
  whose body is compiled now binds the callee's activation, runs its
  native code, and finishes the return in one helper, with each call
  site's checks cached; the core loop's switch to the callee and back is
  gone. With calls this cheap, every hot loop-free body compiles (`go`:
  10% fewer instructions).
- *Operators, properties and `super()`.* An instance operator whose class
  defines a plain Python method (`Vector.__sub__`) calls that method
  directly, its `NotImplemented` still the operator's `TypeError`; a
  property's getter runs directly, or in place when it's a pure leaf;
  `super().name` resolves in native code (`raytrace`: 17% fewer).
- *More instructions in native code.* Closures and generator expressions
  made in a loop, starred unpacking, `LIST_TO_TUPLE`, a comprehension's
  variable in a function with cells, iteration over strings, bytes and
  sets, tuple and list concatenation, and calls of builtin types
  (`range(a, b)`) no longer leave native code (`unpack_sequence`: 11%
  fewer). A constructor whose `__init__` runs frameless lets the caller's
  native code go on.
- *Construction.* An exception class that keeps `BaseException`'s
  `__init__` constructs without a full call (6,400 to 2,800 instructions),
  and `cls(*args)` takes the core loop's construction shape.
- *Collections and keys.* A young collection re-examines only the
  containers deferred since the last one, and dict subscripts take tuples
  of ints and strings as keys in native code.

**Generators and coroutines.** A resumed `yield from` chain now descends
with one lean `SEND` hop per level and climbs back with the yielded value in
hand, without dispatching each consumer's own yield; a level costs about 470
instructions instead of 1,010. A generator's `return` finishes it in the
core loop, a fast generator step can run a body to its `return`, and
`iter(obj)` and `await obj` make an instance's generator `__iter__` or
`__await__` in place. Creating a generator no longer interns its names or
allocates for its arguments, and finishing a young one skips the cycle
collector's index, which holds no entry for it.

**Tier 2.** A loop header the compiled frame can't enter doesn't spend the
code's OSR budget, code whose loops run fewer than two iterations a call
goes to the frame JIT, and a compiled loop that is mostly calls into the
interpreter, or that keeps raising, retires when it exits.

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
User exception classes with an `__init__` of their own construct through
the lean path with `args` seeded as `BaseException_new` seeds it.

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
holds. The last release of an object frees it without `Arc`'s locked
decrements, and a suspended generator outside any handler of its own
closes at its last release instead of through the finalizer queue.

**Libraries.** `_struct`, `os.fspath`, `_thread.get_ident`, parts of
`_asyncio`, and `sqlite3` cursor iteration run in place when they can't run
Python code. `lru_cache` reads its own fields by position and probes its
cache under one comparison scope, and `getattr` of a dunder nothing
supplies answers the default directly.

**Memory.** The tier-2 cache no longer keeps every executed code object
alive, fewer results stay pinned, per-instruction side tables wait until
code is warm, line tables decode lazily, mimalloc reserves 64 MiB arenas,
and function slot storage is boxed on first use: suite peak memory fell
about 19% and the import set's 27%.

## What still trails

- **Calls.** A direct call between compiled functions still costs about
  three times CPython's (the activation's binding and teardown); a call the
  frame JIT can't make directly, of an uncompiled callee or one that leaves
  native code, costs about 1,500 instructions against CPython's 300. This is
  the broadest remaining cost.
- **Interpreter dispatch.** A simple bytecode costs about 35 to 50
  instructions in the core loop against CPython's 15. The loop is one very
  large function, so its state lives in stack slots and every dispatch
  reloads it.
- **Generators.** A `yield from` level still costs about four times
  CPython's, and creating a generator about three times (four allocations
  against CPython's one).
- **Exceptions.** Raising and catching a builtin exception costs about 2.3
  times CPython's instructions, and a raise that crosses a call about three
  times: each raise builds a slot table, a traceback object and a legacy
  traceback entry, and the catch materializes the frames' shells. Exception
  handlers run in the core loop, not in native code.
- **Instructions per cycle.** The binary carries 47 MB of code, and these
  workloads touch hundreds of its functions per iteration; profile-guided
  layout recovered part of the instruction-cache cost.
- **Memory.** Peak resident memory is about 1.9 times CPython's: per-object
  headers, the cycle collector's per-object bookkeeping, decoded code
  objects, and compiled bodies dominate. Garbage only a collection frees
  also builds up across calls: `comprehensions` peaks at about four times
  CPython's memory over five calls, against twice over one.

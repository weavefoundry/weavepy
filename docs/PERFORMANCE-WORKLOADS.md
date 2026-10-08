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
| Instructions | 1.66 | 0.42 |
| Wall time (warm) | 1.63 | |
| Wall time (cold) | 1.87 | |
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
| `json_roundtrip` | 0.76 | 0.99 | 0.72 | 1.48 |
| `pickle_roundtrip` | 0.80 | 0.97 | 0.63 | 1.52 |
| `scimark` | 0.87 | 0.27 | 0.78 | 1.77 |
| `float_points` | 0.90 | 0.48 | 0.81 | 1.51 |
| `xml_etree` | 0.97 | 0.08 | 1.03 | 1.66 |
| `unpack_sequence` | 0.98 | 0.67 | 0.94 | 1.70 |
| `regex_mix` | 1.05 | 0.95 | 0.81 | 1.65 |
| `decimal_telco` | 1.15 | 0.01 | 0.77 | 2.11 |
| `oo_patterns` | 1.28 | 0.42 | 1.27 | 1.93 |
| `collections_mix` | 1.33 | 0.09 | 1.32 | 1.83 |
| `sorting` | 1.35 | 0.83 | 1.17 | 1.82 |
| `chaos` | 1.35 | 0.58 | 1.56 | 2.24 |
| `sqlite_queries` | 1.41 | 0.42 | 1.30 | 1.76 |
| `graph_search` | 1.46 | 0.41 | 1.32 | 1.73 |
| `class_creation` | 1.63 | 0.81 | 1.69 | 1.87 |
| `comprehensions` | 1.69 | 0.56 | 1.58 | 3.59 |
| `text_processing` | 1.73 | 0.68 | 1.49 | 2.13 |
| `bytes_codecs` | 1.74 | 0.38 | 1.69 | 1.70 |
| `raytrace` | 1.74 | 0.48 | 1.69 | 1.89 |
| `exceptions` | 1.89 | 0.86 | 2.42 | 1.71 |
| `go` | 2.26 | 0.61 | 2.24 | 1.95 |
| `sudoku` | 2.28 | 0.63 | 2.14 | 1.72 |
| `closures_decorators` | 2.28 | 0.54 | 2.65 | 1.75 |
| `nqueens` | 2.33 | 0.54 | 2.44 | 1.78 |
| `mini_interpreter` | 2.40 | 0.64 | 2.29 | 1.98 |
| `coroutines` | 2.41 | 0.30 | 2.54 | 1.46 |
| `tomllib_loads` | 2.43 | 0.48 | 2.55 | 2.44 |
| `generators_tree` | 2.69 | 0.17 | 2.67 | 1.85 |
| `context_managers` | 2.73 | 0.50 | 3.89 | 1.89 |
| `stdlib_utils` | 2.84 | 0.57 | 2.59 | 2.37 |
| `deepcopy` | 2.86 | 0.33 | 3.37 | 2.07 |
| `logging_mix` | 2.87 | 0.72 | 2.84 | 2.04 |
| `async_tree` | 2.88 | 0.22 | 3.52 | 2.65 |

The instruction counts come from a `quick` build of commit `8178aa3`, the
times and memory from a profile-guided release build of the same commit.
Seven workloads beat CPython's time: `pickle_roundtrip`, `json_roundtrip`,
`decimal_telco`, `scimark`, `float_points`, `regex_mix`, and
`unpack_sequence`.

## What changed

The work followed one loop: find where a workload leaves the interpreter's
fast paths (the core loop's slow-step and handoff counters,
`WEAVEPY_BURST_STATS=1`), or where its time goes (sampled profiles and
per-function instruction estimates), then remove the cost at its source.

**Since the previous checkpoint.** This round hunted the generic paths that
fast paths fell back to, and the places where hot code still left the core
loop for a full step (which first pushes a frame shell for every waiting
caller, several thousand instructions a few calls deep):

- *Recursive generators.* Resuming a `yield from` chain now resumes its
  innermost generator directly; the levels between stay parked in their
  `SEND`s in a `Delegating` state Python sees as running, and a yield
  comes straight back (`generators_tree`: 28% fewer instructions).
- *Comparisons.* Tuples and lists of ints, floats, strings and such tuples
  compare in place rather than copying both sequences and dispatching
  every element (`graph_search`: 29% fewer), and an instance's Python
  `__lt__` is called directly across sibling classes (`oo_patterns`: 7%).
- *Callbacks.* A Python function that native code calls back (a sort key,
  a `partial`, `lru_cache`, `**kwargs` forwarding) runs as a lean
  activation even while a builtin lane has its caller published, rather
  than as a general frame (`closures_decorators`: 7% fewer). `map` and
  `filter` over Python functions step in such a lane too, and so do
  keyword and spread calls of classes and functions the inline paths
  decline.
- *Fewer full steps.* Truth tests of native objects, a stream's methods,
  `str % dict` over plain values, property setters, `del` of list items
  and dict keys, `float()` and `int()` of plain values, `async with`'s
  special methods, module and container class attributes, and several
  native builtins (`os.getpid`, `time.localtime`, `strftime`, a
  namedtuple's `_replace`) now run in the core loop (`collections_mix`:
  11%, `logging_mix`: 8%).
- *Libraries and the collector.* SQLite connections skip the per-call
  mutex while the GIL serializes them (`sqlite_queries`: 9%), tier 2's
  dict stores probe once, and young collections that keep finding no
  cycles back off (`async_tree`: 7%, `comprehensions`: 6%).

**The round before.** Most of that round moved work out of
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
  core loop runs inline, from code the frame JIT doesn't cover (coroutine
  bodies, functions it declines), costs about twice CPython's plus two
  switches of the loop's state. This is the broadest remaining cost.
- **Interpreter dispatch.** A simple bytecode costs about 35 to 50
  instructions in the core loop against CPython's 15. The loop is one very
  large function, so its state lives in stack slots and every dispatch
  reloads it.
- **Generators.** Creating a generator and running it to its end costs
  about three times CPython's (four allocations against CPython's one),
  though a resumed `yield from` chain now costs a short walk per level.
- **The cycle collector.** It keeps tracked objects in a side index, so an
  object that outlives the young set pays an index entry and its removal,
  and each collection candidate costs several hash probes: a collection
  costs several times CPython's per object.
- **Exceptions.** Raising and catching a builtin exception costs about 2.5
  times CPython's instructions, and a missing attribute's `AttributeError`
  about three times: each raise builds a slot table, a traceback object and
  a legacy traceback entry, a failing lookup tries several resolution paths
  before the general one, and the handler's operations move the large
  exception record by value. Deleting an instance attribute converts the
  instance's shared-key storage into a full dictionary (ten times
  CPython's cost), which contextlib's generator managers do three times
  per `with`.
- **Instructions per cycle.** The binary carries 47 MB of code, and these
  workloads touch hundreds of its functions per iteration; profile-guided
  layout recovered part of the instruction-cache cost.
- **Memory.** Peak resident memory is about 1.9 times CPython's: per-object
  headers, the cycle collector's per-object bookkeeping, decoded code
  objects, and compiled bodies dominate. Garbage only a collection frees
  also builds up across calls: `comprehensions` peaks at about 3.6 times
  CPython's memory over five calls, against twice over one.

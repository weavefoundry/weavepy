# RFC 0014: I/O and a batteries-included stdlib

- **Status**: Accepted
- **Authors**: WeavePy authors
- **Created**: 2026-05-20
- **Tracking issue**: TBD

## Summary

Close the gap between "the language works" (post RFC 0012) and
"real Python scripts run." After this RFC lands:

- The object model gains `Object::Set`, `Object::FrozenSet`,
  `Object::Bytes`, `Object::ByteArray`, and `Object::File`. All four
  support literals where appropriate (`{1, 2}`, `b"..."`), the
  full method surface CPython exposes, and the operators
  (`|`/`&`/`-`/`^` for sets; `+`/`*` for bytes; subscription and
  slicing for both).
- `str.format()` and the `%` formatting operator are wired
  end-to-end. f-strings (RFC 0005) and `format()` now share the
  same spec parser.
- A working `open()` returns a Python file object backed by
  pluggable [`FileBackend`]s (disk, in-memory, or wrapping the
  host's stdin/stdout/stderr). `sys.stdin`, `sys.stdout`, and
  `sys.stderr` — explicitly deferred in RFC 0012 — are filled in
  here.
- A Rust-backed `io` module exposes `StringIO`, `BytesIO`, and
  the SEEK constants.
- A bundle of stdlib modules is shipped, split between
  Rust-backed (`io`, `re`, `json`, `random`, `time`) and frozen
  pure-Python sources compiled and executed inside WeavePy itself
  (`collections`, `itertools`, `functools`, `contextlib`,
  `pathlib`, `argparse`).
- Several language features the stdlib relies on are filled in
  along the way: the `del` statement, `*args`/`**kwargs` argument
  unpacking at the call site (`f(*xs, **kw)`), list slice
  assignment (`xs[1:3] = ys` and the strided form), the
  `for`–`else` semantics, and keyword-only argument defaults.

The combination is what the project calls "Option 2" in the
roadmap: drop in `from collections import Counter` or
`json.dumps(...)` and have it work.

## Motivation

After RFC 0012 the import system worked but the world it imported
into was still bare. Real-world Python scripts — even small ones —
trip immediately:

- Anything that reads or writes a file needs `open()`. Without it
  even a one-liner like `print(open("data.txt").read())` fails.
- Anything that touches data structures past `list`/`dict`/`tuple`
  needs `set`/`frozenset` or one of the `collections` containers.
- `b"..."` and byte slicing appear the moment a script reads
  binary data or talks to a subprocess.
- Most logging, error reporting, and CLI tooling routes through
  `str.format()` or `%`-formatting. f-strings cover a lot, but
  `logger.info("%s %s", a, b)` is still ubiquitous.
- Once `sys.stdout`/`sys.stderr` are Python file objects, the
  rest of the stdlib (especially `argparse`'s `parser.error()`,
  `traceback`, `pprint`) starts to work without per-module
  workarounds.
- The conformance corpus and the `_pkg` integration fixtures
  cannot exercise multi-file packages until at least
  `collections`, `itertools`, and `functools` exist.

Down-tree, this RFC unblocks:

- The conformance harness's "Stage B" (running CPython
  `regrtest`-style files), which depends on `unittest` (which
  depends on `io`, `traceback`, `linecache`, `argparse`).
- Any future REPL that wants to redirect input/output.
- An eventual `pip`-style installer and any CI tooling, both of
  which assume `argparse`, `json`, `re`, and `pathlib`.

## CPython reference

This RFC tracks **CPython 3.13**:

- **`set` / `frozenset`** — `Objects/setobject.c` and the language
  reference §"Set types — set, frozenset". The set algebra is in
  PEP 218 (its motivating PEP is older but still definitive).
- **`bytes` / `bytearray`** — `Objects/bytesobject.c`,
  `Objects/bytearrayobject.c`, PEP 358 (the original `bytes`
  proposal). The method surface is identical between `bytes` and
  `bytearray`; the latter adds in-place mutators.
- **String formatting** — PEP 3101 (`str.format`) and PEP 3101's
  later updates for `Formatter`. CPython's implementation lives in
  `Objects/stringlib/unicode_format.h` and `Objects/unicodeobject.c`.
  `%`-formatting is documented under `printf`-style String
  Formatting in the language reference.
- **File I/O** — `Lib/_pyio.py` (the pure-Python reference) and
  `Modules/_io/` (the C accelerator). The protocol — `read`,
  `write`, `seek`, `close`, `__enter__`/`__exit__` — is in PEP 3116.
- **`io`** — same source as above. We follow the `RawIOBase` /
  `BufferedIOBase` / `TextIOBase` hierarchy in spirit but flatten
  it: a single `PyFile` type plays all three roles, dispatching on
  its `FileBackend`.
- **`collections`** — `Lib/collections/__init__.py`. The frozen
  Python source we ship is a substantially simplified rewrite of
  that file (see "Detailed design" for what we deliberately omit).
- **`itertools`** — `Modules/itertoolsmodule.c`. We follow the
  semantics of `chain`, `cycle`, `count`, `islice`, etc. as
  documented in the language reference. Our implementation is pure
  Python; CPython's is C for speed.
- **`functools`** — `Lib/functools.py`. Our `lru_cache` follows
  the surface (`maxsize=None`, `cache_clear()`, `cache_info()`)
  but is a simpler implementation.
- **`re`** — `Lib/re/__init__.py` and the language reference
  "Regular expression operations". We do not implement CPython's
  pattern bytecode; instead we delegate to the Rust `regex` crate,
  which means we *intentionally diverge* on some edge cases (see
  Drawbacks).
- **`json`** — `Lib/json/__init__.py`. Our implementation wraps
  `serde_json`; the surface is `loads`, `dumps`, `load`, `dump`.
- **`random`** — `Lib/random.py`. We do *not* use Mersenne
  Twister; we use SplitMix64. The seeded sequence is therefore
  different from CPython's. The surface is `seed`, `random`,
  `uniform`, `randint`, `randrange`, `choice`, `choices`,
  `shuffle`, `sample`, `gauss`.
- **`time`** — `Modules/timemodule.c` and `Lib/time.py`. Backed by
  `std::time` and `chrono`.
- **`pathlib`** — `Lib/pathlib.py`. PEP 428. We ship a "lite"
  version covering `PurePath` and the most common `Path`
  operations.
- **`argparse`** — `Lib/argparse.py`. PEP 389. A substantial
  subset; help output formatting and exit codes match CPython.

We deliberately do **not** track:

- CPython's full `Lib/_pyio.py` buffer hierarchy. WeavePy's `PyFile`
  is conceptually a buffered text/binary file but is implemented
  as one type with a backend enum, not three layered classes.
- The `io.IOBase` / `io.RawIOBase` / `io.BufferedReader` /
  `io.TextIOWrapper` class hierarchy. We only ship `StringIO` and
  `BytesIO` from `io`. The rest is a Python-surface concern that
  would benefit users *implementing* custom backends; nothing in
  the slice needs it.
- CPython's `multiprocessing`, `asyncio`, `socket`, `subprocess`,
  `threading`, `pickle`. Each is a major surface in its own right.
- `traceback`, `logging`, `unittest`, `warnings`. These exist on
  top of what this RFC ships and are tracked as follow-up RFCs.

## Detailed design

### Object model extension

```rust
pub enum Object {
    // ... existing variants ...
    Bytes(Rc<[u8]>),
    ByteArray(Rc<RefCell<Vec<u8>>>),
    Set(Rc<RefCell<SetData>>),
    FrozenSet(Rc<SetData>),
    File(Rc<PyFile>),
}
```

- `SetData` is an `IndexSet<DictKey>` so iteration order is
  insertion order. This matches CPython's *actual* observable
  behavior on small sets and avoids surprising users; the language
  reference does not specify iteration order, so we are free.
- `DictKey` reuses the dict-key hashing path; sets and dict keys
  share an equivalence class as in CPython.
- `Bytes` is immutable; `ByteArray` is `Rc<RefCell<...>>`. Slicing
  produces a new `Bytes`/`ByteArray`.
- `PyFile` carries a `FileBackend` enum:

```rust
enum FileBackend {
    Disk(...),       // wraps std::fs::File with a mode and buffer
    InMemory(...),   // wraps Vec<u8> for tests and StringIO/BytesIO
    Stdin, Stdout, Stderr,  // forwarded to interpreter sinks
}
```

The `PyFile` object exposes the standard `read`, `write`, `seek`,
`tell`, `flush`, `close`, `readline`, `readlines`,
`__enter__`/`__exit__` methods. Text and binary mode are handled
by tagging the `PyFile` with an encoding (`None` for binary).

### Set literals, comprehensions, and operators

The parser already recognised `{}` as a dict literal and `{a, b}`
as a set literal. The compiler now emits:

- `BuildSet(n)` — pops `n` values, builds an `Object::Set`.
- `SetAdd(depth)` — used by set comprehensions; mirrors
  `ListAppend` / `MapAdd`.

The VM implements the set algebra:

- `|` → union  
- `&` → intersection  
- `-` → difference  
- `^` → symmetric difference  
- `in` / `not in` — membership lookup  
- `<` / `<=` / `>=` / `>` — proper subset / subset / superset /
  proper superset

Set methods (`add`, `discard`, `remove`, `pop`, `union`,
`intersection`, `difference`, `symmetric_difference`,
`issubset`, `issuperset`, `update`, `intersection_update`,
`difference_update`, `symmetric_difference_update`, `copy`) live
under `builtins::lookup_method`. Frozenset reuses the read-only
methods and rejects the mutators.

### Bytes / bytearray

`b"..."` literals are already produced by the parser as
`Constant::Bytes`. The compiler converts them to `Object::Bytes`
in `constant_to_object`.

The mutable variant gains `append`, `extend`, `clear`, `pop`,
`insert`, `reverse`, plus the read-only methods every `bytes`
exposes (`decode`, `hex`, `split`, `splitlines`, `strip`,
`startswith`, `endswith`, `upper`, `lower`, `replace`, `find`,
`rfind`, `index`, `rindex`, `count`, `join`).

`str.encode(...)` produces `Bytes`; `bytes.decode(...)` produces
`str`. Both default to UTF-8.

Slicing falls through the same `binary_subscr` path as lists:
`b[0]` returns the byte as an `int`, `b[1:3]` returns a new
`Bytes`.

We **do not** ship `bytes.fromhex` in this RFC. It is a
classmethod, and our class-method support is still incomplete (a
classmethod on a built-in type is the open question; instance
methods on built-ins work fine). Tracked in "Future work".

### String formatting

Two entry points share a single field-spec parser:

- `str.format(*args, **kwargs)` — replaces `{...}` placeholders.
- `%-formatting` (`"%s %d" % (a, b)`) — backed by
  `crate::percent_format`.

The field-spec parser handles:

- Implicit (`{}`) and explicit (`{0}`, `{name}`) field names.
- Attribute and subscript trailers (`{a.b}`, `{a[0]}`).
- Conversions: `!s` (string), `!r` (repr), `!a` (ascii repr).
- Format spec mini-language: alignment (`<`/`>`/`^`/`=`), sign
  (`+`/`-`/` `), `#` alternate form, `0` zero-fill, width,
  precision, type (`b`/`o`/`d`/`x`/`X`/`e`/`E`/`f`/`F`/`g`/`G`/
  `n`/`s`/`%`).

The VM-aware variant `Vm::do_str_format` dispatches `!s` and `!r`
through the user-visible `__str__` and `__repr__` methods — the
plain `Object::to_str()` / `Object::repr()` path can't see them.
The same routing is now applied when the built-in `str()` /
`repr()` is called on an instance, so `str(obj)` honours
`__str__`.

f-strings (RFC 0005) reuse the same `format_via_spec` helper, so
`f"{x:.2f}"` and `"{:.2f}".format(x)` go through identical code.

### File I/O

`open(path, mode="r", encoding=None)` is a built-in. It builds an
`Object::File` whose backend is `FileBackend::Disk` and whose
encoding is the explicit value or UTF-8 by default. Modes are the
standard CPython set (`r`/`w`/`a`/`r+`/`w+`/`a+`/`x`, optionally
suffixed `b` for binary). Errors map to `OSError`.

`with open(...) as f:` works because `PyFile` provides
`__enter__` / `__exit__` directly; no descriptor protocol is
needed.

`sys.stdin`, `sys.stdout`, and `sys.stderr` are `Object::File`s
whose backend is `FileBackend::Stdin`/`Stdout`/`Stderr`. They
forward writes through the interpreter's installed sink (the
same one `print` uses) so a test harness can capture them.

### The `io` module

`StringIO` and `BytesIO` are built directly on top of `PyFile`
with an `InMemory` backend. They support `read`, `write`, `seek`,
`getvalue`, `truncate`, plus the context-manager protocol.

`io.SEEK_SET = 0`, `SEEK_CUR = 1`, `SEEK_END = 2` mirror the
constants.

We do **not** ship the `IOBase` class hierarchy. If a user wants
to subclass `IOBase`, the right answer today is to wrap a
`StringIO` and forward the calls. This is a known gap; tracked.

### Frozen Python stdlib modules

A new `FrozenSource` is registered with the `ModuleCache`:

```rust
pub struct FrozenSource {
    pub name: &'static str,
    pub source: &'static str,
    pub filename: &'static str,
}
```

On first import, the interpreter compiles and runs the embedded
source in a fresh module dict, then caches the module in
`sys.modules`. This lets us ship pure-Python implementations of
stdlib modules without writing a Rust binding for every one of
them.

Modules shipped this way: **`collections`**, **`itertools`**,
**`functools`**, **`contextlib`**, **`pathlib`**, **`argparse`**.

The frozen `collections` is intentionally simpler than CPython's:

- `deque`, `OrderedDict`, `defaultdict`, `Counter`, `ChainMap`,
  and `namedtuple` are implemented.
- They **compose** an internal `_data` dict rather than
  *inheriting* from the built-in `dict`. WeavePy's MRO does not
  currently support inheriting from built-in container types, so
  the standard CPython pattern `class OrderedDict(dict)` does not
  work here. We surface the same public API; iteration through
  the mapping protocol still works.
- `namedtuple` likewise composes a `_values` tuple instead of
  inheriting from `tuple`.

The frozen `itertools` follows the same composition pattern where
needed. A few of its functions (notably `permutations`,
`combinations`, `combinations_with_replacement`) are implemented
using a `_take` helper that materialises a generator expression
into a tuple before yielding; this works around a current
compiler quirk where `yield tuple(genexp)` inside a generator
function does not compile correctly. The behaviour is identical;
the workaround can be removed when the compiler quirk is fixed.

The frozen `functools.lru_cache` wraps the cached function in a
`_LruCacheWrapper` *class instance* instead of a Python function.
WeavePy's Python function objects do not yet support arbitrary
attribute assignment (`wrapper.cache_clear = ...`), so we expose
`cache_clear` and `cache_info` as instance methods on the wrapper
class. Behaviour matches `@functools.lru_cache` for the supported
parameters (`maxsize=None`, `typed=False`).

### Rust-backed Python stdlib modules

Five modules are implemented in Rust for performance or because
the underlying machinery doesn't fit in pure Python:

- **`io`** (`stdlib/io.rs`). Builds `StringIO`, `BytesIO` from
  the `PyFile` machinery.
- **`re`** (`stdlib/re.rs`). Wraps the Rust `regex` crate.
  Exposes `compile`, `match`, `search`, `fullmatch`, `findall`,
  `finditer`, `sub`, `subn`, `split`, `escape`. `Pattern` and
  `Match` are class instances backed by `thread_local!` cached
  `TypeObject`s.
- **`json`** (`stdlib/json.rs`). Wraps `serde_json`. Exposes
  `loads`, `dumps`, `load`, `dump`. Converts between
  `serde_json::Value` and `Object`.
- **`random`** (`stdlib/random.rs`). SplitMix64 RNG. Exposes
  `seed`, `random`, `uniform`, `randint`, `randrange`, `choice`,
  `choices`, `shuffle`, `sample`, `gauss`.
- **`time`** (`stdlib/time.rs`). Wraps `std::time` and `chrono`.
  Exposes `time`, `time_ns`, `monotonic`, `perf_counter`, `sleep`,
  `strftime`, `localtime`, `gmtime`, `mktime`.

### Language features

These were filled in to satisfy the stdlib modules above:

- **`del` statement**. Parser produces `StmtKind::Delete(targets)`.
  Compiler emits `DeleteName` / `DeleteFast` / `DeleteAttr` /
  `DeleteSubscr` depending on the target.
- **`*args` / `**kwargs` at call sites**. A new opcode
  `CallEx(flags)` accepts a tuple of positional args and a dict
  of keyword args. The compiler builds those structures using the
  existing tuple/dict opcodes when it sees `*xs` / `**kw` in a
  call.
- **List slice assignment** (`xs[1:3] = [10, 20]` and the strided
  form `xs[::2] = [...]`). `Vm::store_subscr` recognises the
  `(List, Slice)` pair and routes to `apply_slice_assignment`,
  which `splice`s for the common `step == 1` case and assigns
  index-by-index for strided slices (lengths must match).
- **`for`–`else` semantics**. The compiler now tracks whether a
  loop frame is a `for` loop and, when `break` fires, emits a
  `PopTop` before the jump so the iterator left on the stack by
  `FOR_ITER` is cleaned up. The `break` jump also targets *past*
  the `else` block, matching CPython.
- **Keyword-only argument defaults** (`def f(*args, kw=10)`). The
  compiler builds a `{name: default}` dict and passes it to
  `MakeFunction` via flag bit `0x02`. The VM's `call_python`
  applies positional kwargs first, then positional defaults right-
  aligned to the parameter list, then keyword-only defaults by
  name. Missing required keyword-only args raise `TypeError`.
- **`dict(mapping)`**. `Vm::instantiate` recognises the case
  where the sole argument to `dict()` is an instance with a
  `keys()` method (the standard mapping protocol) and walks it
  via `keys()` + `__getitem__()`. Built-in `Object::Dict`
  arguments take a direct fast path. This unblocks
  `dict(defaultdict_instance)` and similar.

### Exception `__str__` / `__repr__`

`BaseException` now has `__str__` and `__repr__` installed on the
class dict; they extract the canonical `args` tuple and produce
the CPython-shaped string. `str(exc)` and `repr(exc)` therefore
work for any user-defined exception subclass.

## Drawbacks

- **`re` is the Rust `regex` crate.** It is a different regex
  engine from CPython's. Most patterns work identically, but a
  handful diverge: backreferences are unsupported, lookaround is
  limited, and Unicode property classes follow a different table.
  We accept this because (a) the Rust engine is fast and well
  tested, (b) writing a CPython-compatible engine from scratch is
  a multi-month project, and (c) almost no real code relies on
  these edge cases. The divergence is *documented*, not silent.
- **`random` does not match CPython's sequence.** Same seed, same
  call sequence, different numbers. Code that relies on stable
  PRNG output across implementations will break here. CPython is
  one of the few interpreters that pins itself to Mersenne
  Twister; we did not want to inherit the maintenance cost.
- **Frozen stdlib modules can't inherit from built-in containers.**
  Anywhere CPython does `class Foo(dict)` or `class Foo(tuple)`,
  we composed instead. The behaviour matches; the `isinstance(x,
  dict)` / `isinstance(x, tuple)` check does not. We surface a
  documented divergence and plan to lift it when general
  inheritance from built-ins lands.
- **No `IOBase` hierarchy.** Custom file types must wrap an
  existing `PyFile`. This is a known gap for the slice of users
  who subclass `io.IOBase`.
- **No `Lib/_pyio.py` text/binary buffering split.** A single
  `PyFile` handles both. Edge cases around partial reads on
  unbuffered streams may diverge.
- **Frozen Python code increases interpreter binary size.** Each
  module is on the order of a few KB of source bundled into the
  binary via `include_str!`. Total cost is currently under 100KB
  uncompressed, which is acceptable for a non-static-linked-LLVM
  interpreter.

## Alternatives

- **Vendor CPython's actual `Lib/collections/__init__.py` etc.**
  Rejected for now: those modules use built-in inheritance, slot
  descriptors, and several C-accelerator imports we do not yet
  support. Re-implementing them in WeavePy-compatible Python is
  the smaller change. We will re-evaluate per-module as the MRO
  and slot-descriptor stories mature.
- **Implement `re` as a pure-Python NFA**. Rejected: the
  performance hit for everyday string operations would be severe,
  and the maintenance cost dwarfs the regex-crate divergence.
- **Defer `str.format()`**. Rejected: even the stdlib modules we
  ship here (`argparse`, `pathlib`, `functools.wraps`) use it.
- **Ship `multiprocessing` / `asyncio`.** Out of scope; each
  needs its own RFC and they assume an event loop / process model
  we have not specified.
- **Pickle and shelve.** Out of scope; they require a stable
  object-graph serialisation format that we have not designed yet.

## Prior art

- **RustPython** ships a similar mix of Rust-backed and
  pure-Python stdlib modules. They wrap regex with `regex` too,
  and accept the same divergences. Their `collections` likewise
  cannot inherit from built-in `dict` and uses composition.
- **PyPy** ships a near-complete pure-Python stdlib reuse with
  full inheritance support; they get there by implementing the C
  API for built-in types. Out of scope for the slice.
- **MicroPython** ships a much smaller `ucollections` and skips
  most stdlib modules entirely. Their `re` is a hand-rolled
  subset; CPython divergences are wider.
- **GraalPy** runs a vendored CPython stdlib unchanged by
  emulating the C extension API. The most CPython-compatible of
  the alternatives, at the cost of a large Java/native runtime.

## Unresolved questions

- **Should `re` carry a divergence list in the docs?** Right now
  the only mention of the divergence is this RFC. A short
  "Known divergences" page anchored from `docs/CONFORMANCE.md`
  would help library authors.
- **Argument unpacking inside class bodies.** We compile `f(*xs)`
  in function bodies and module scope. Class-body usage works in
  the cases we tested; we have not yet stress-tested it under
  metaclass-heavy scenarios.
- **Encoding errors.** `open("...", "r", encoding="utf-8")`
  raises on invalid UTF-8 (`UnicodeDecodeError`); we have not yet
  exposed the `errors=` parameter (`"strict"`/`"replace"`/
  `"ignore"`). Easy to add when needed.
- **Random `state` and reproducible sequences.** CPython exposes
  `random.getstate()` / `setstate()` for reproducible runs. We do
  not. Adding them requires committing to a SplitMix64-state
  serialisation that we cannot easily change later.

## Future work

- **Encoding `errors=` parameter** on `open()`, `bytes.decode`,
  `str.encode`.
- **`bytes.fromhex` / `int.from_bytes`** — both classmethods on
  built-in types. Blocked on a small bit of object-model work to
  let classmethods live on a built-in `TypeObject`.
- **`io.IOBase` and the buffered/text class hierarchy**, so user
  code can subclass `IOBase` directly.
- **`traceback` and `logging`**, both of which need full
  `sys.excepthook` and frame walking.
- **`unittest`**, which depends on the above two plus `argparse`.
- **`subprocess`** — separate RFC; needs a process model and a
  way to plumb file descriptors.
- **Inherit from built-in containers** so that
  `class OrderedDict(dict)` in vendored CPython source works
  unchanged. Tracked alongside slot-descriptor work.
- **CPython-compatible PRNG.** If we ever care about reproducible
  seeded sequences, implement Mersenne Twister behind a feature
  flag.

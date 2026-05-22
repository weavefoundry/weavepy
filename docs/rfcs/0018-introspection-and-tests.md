# RFC 0018: Introspection, test infrastructure, and exception groups

- **Status**: Accepted
- **Authors**: WeavePy authors
- **Created**: 2026-05-22
- **Tracking issue**: TBD

## Summary

Close the gap between "modern Python — language + stdlib + asyncio
+ OS interface — runs" (post RFC 0017) and "**CPython's own test
suite — the project's stated spec — runs**." After this RFC lands:

- The runtime exposes a real **frame and code introspection surface**:
  `sys._getframe`, `sys.exc_info`, `sys.excepthook`, `sys.unraisablehook`,
  and a proper traceback object hierarchy where `__traceback__`,
  `tb_frame`, `tb_lineno`, `tb_next`, `frame.f_locals`, `frame.f_globals`,
  `frame.f_code`, `frame.f_lineno`, `frame.f_back` all read live
  values off the interpreter's frame stack.
- **PEP 654 — exception groups** — lands end-to-end: a new
  `BaseExceptionGroup` / `ExceptionGroup` exception hierarchy, the
  `except*` clause in the parser, a new compiler shape, and the VM
  semantics that splits the group between matching and non-matching
  branches.
- **`weakref`** ships (Rust core + frozen wrapper): `ref`, `proxy`,
  `WeakSet`, `WeakValueDictionary`, `WeakKeyDictionary`, `finalize`,
  `getweakrefcount`, `getweakrefs`. Sufficient for the patterns
  `logging` and `unittest.mock` reach for.
- **`gc`** module exposes the small set of knobs Python programs
  reach for (`collect`, `get_count`, `disable`, `enable`,
  `isenabled`, `get_threshold`, `set_threshold`, `get_objects`).
  The VM doesn't actually collect cycles; the module is shaped so
  user code can ask the questions without raising.
- **`datetime`** lands: `date`, `time`, `datetime`, `timedelta`,
  `timezone`, `tzinfo`. The Rust `_datetime` shim provides the
  system clock + epoch conversions; the user-visible surface is
  frozen Python, matching CPython's split.
- **`contextvars`** gains its full PEP 567 surface:
  `ContextVar(name, default=MISSING)`, `Token`, `Context`,
  `copy_context`, `Context.run`. Frozen Python on top of a tiny
  state-stack helper, plus the asyncio loop is patched to copy and
  re-enter the calling task's context on every step.
- **`linecache`**, **`warnings`**, **`traceback`**, **`inspect`**,
  **`logging`**, **`unittest`**, **`unittest.mock`** all ship as
  frozen Python modules on top of the introspection hooks above.
- **`runpy`** (`run_module`, `run_path`) is added so `python -m
  pkg.mod` works through the CLI in a follow-up commit.
- The asyncio loop gains **`TaskGroup`** and **`asyncio.timeout`**
  — both standardised in CPython 3.11 — built on
  `BaseExceptionGroup` semantics.

The combination is what the project calls "Option 1" in the
roadmap: drop in `python -m unittest discover`, `logging.basicConfig`,
`@functools.wraps` on a decorator, `traceback.format_exc()`, or
`unittest.mock.patch("…")` and have it work.

## Motivation

After RFC 0017 the project could run a five-line HTTP server, a
five-line subprocess wrapper, and most of the CPython network
stack. What it could *not* run was anyone's tests — `unittest`,
`mock`, `traceback`, `logging`, and the introspection primitives
that depend on real frame objects were all absent. The
conformance harness's "Stage B" (running CPython's `Lib/test/*.py`
files under WeavePy) was therefore still blocked by exactly the
modules the project's stated north star demands.

The other half of the story is correctness: every previous RFC has
relied on the in-tree fixture suite to gate landing. That suite is
now 55 files and ~3K lines of Python — large enough that bugs are
escaping detection. The path to a meaningful conformance number is
*running CPython's tests*, which means `unittest` has to work.

Down-tree, this RFC unblocks:

- Stage B of the conformance harness — running selected
  `Lib/test/test_*.py` files under WeavePy and comparing pass/fail
  against the CPython oracle.
- Real-world deployment surface — `logging` is in essentially every
  long-running Python program; `traceback` runs every error path;
  `datetime` is unavoidable.
- The introspection patches `pdb` will need (deferred to a
  follow-up RFC), so a sensible Python REPL / debugger can land
  next.
- `pytest` itself (it ships its own runner but reaches for
  `unittest.mock`, `inspect`, `traceback`, `linecache`, `logging`
  unmodified).

## CPython reference

This RFC tracks **CPython 3.13**:

- **Frame and code object introspection** — `Python/frameobject.c`,
  `Lib/dis.py`, `Lib/inspect.py`'s `currentframe`/`stack`/`getframeinfo`,
  and the documented attributes on `types.FrameType` and `types.TracebackType`.
- **PEP 654** — exception groups, `BaseExceptionGroup`,
  `ExceptionGroup`, and the `except*` clause. We follow the
  PEP and `Lib/test/test_exception_group.py` for behaviour.
- **`sys`** — `_getframe`, `exc_info`, `excepthook`,
  `unraisablehook`, `__excepthook__`, `__unraisablehook__`,
  `settrace`/`setprofile` (stubs).
- **`weakref`** — `Lib/weakref.py` plus `Modules/_weakref.c`. The
  surface (`ref`, `proxy`, `WeakSet`, `WeakValueDictionary`,
  `WeakKeyDictionary`, `finalize`).
- **`gc`** — `Modules/gcmodule.c`. We expose the knobs without
  actually implementing tracing GC; cycles are still leaked.
- **`datetime`** — `Modules/_datetimemodule.c` and `Lib/datetime.py`.
  The Python-visible surface from `Lib/datetime.py` is rebuilt
  on top of a Rust `_datetime` shim that provides the system clock,
  monotonic time, and the leap-aware epoch arithmetic.
- **`contextvars`** — PEP 567. `Lib/contextvars.py` + the C
  accelerator. We follow the public surface but use a flat
  per-context dict as the state representation.
- **`linecache`** — `Lib/linecache.py`. The implementation is a
  straight port: source caching keyed on filename, with checksums
  to invalidate when the source is unmodified on disk.
- **`warnings`** — `Lib/warnings.py`. The `warn`, `filterwarnings`,
  `simplefilter`, `catch_warnings`, `resetwarnings`, `showwarning`
  surface.
- **`traceback`** — `Lib/traceback.py`. `format_exc`, `print_exc`,
  `walk_tb`, `walk_stack`, `TracebackException`, `StackSummary`,
  `FrameSummary`.
- **`inspect`** — `Lib/inspect.py`. `signature`, `Parameter`,
  `Signature`, `currentframe`, `stack`, `getframeinfo`, `getmro`,
  `isfunction`, `isclass`, `ismodule`, `iscoroutine`,
  `getsource`/`getsourcefile`, `getmembers`.
- **`logging`** — `Lib/logging/__init__.py` and
  `Lib/logging/handlers.py`. `Logger`, `Handler`, `Formatter`,
  `LogRecord`, `basicConfig`, `getLogger`, levels, `StreamHandler`,
  `FileHandler`, `NullHandler`, `RotatingFileHandler`.
- **`unittest`** — `Lib/unittest/case.py`, `Lib/unittest/main.py`,
  `Lib/unittest/runner.py`, `Lib/unittest/loader.py`,
  `Lib/unittest/suite.py`, `Lib/unittest/result.py`. `TestCase`,
  the full `assertX` family, `TestSuite`, `TestLoader`,
  `TextTestRunner`, `defaultTestLoader.discover`, `skip`/`skipIf`/
  `expectedFailure`, `setUp`/`tearDown`/`setUpClass`/`tearDownClass`,
  `main`.
- **`unittest.mock`** — `Lib/unittest/mock.py`. `Mock`, `MagicMock`,
  `patch`, `patch.object`, `call`, `ANY`, `sentinel`,
  `_Call`, `PropertyMock`. We support the patterns that real test
  code reaches for; the deep wrapper-chain magic is approximate.
- **`runpy`** — `Lib/runpy.py`. `run_module`, `run_path`.
- **`asyncio.TaskGroup` / `asyncio.timeout`** — CPython 3.11.
  `Lib/asyncio/taskgroups.py`, `Lib/asyncio/timeouts.py`.

We deliberately do **not** track:

- **PEP 657 (fine-grained traceback)**. Tracebacks point at line
  level, not column. `traceback.print_exc()` produces the familiar
  `^^^^` carets at line granularity only.
- **`pdb` / `bdb`**. Deferred to a later RFC that builds an
  interactive debugger on top of these introspection hooks.
- **`unittest.TestCase.subTest`** with full traceback splitting per
  iteration. We accept the call and run the body inline; sub-test
  reports merge into the enclosing test's result.
- **`unittest.IsolatedAsyncioTestCase`** — runs async tests on a
  fresh event loop. Deferred: the wiring is straightforward but
  there's a non-trivial interaction with the existing
  `asyncio.run` (which closes the loop on return).
- **`logging.handlers.QueueHandler` / `QueueListener`**. The
  cooperative scheduling model doesn't give us a useful threaded
  receiver; the handler exists but the listener is approximated.
- **`logging.config.fileConfig` / `dictConfig`** — the structured
  configurators. Manual `basicConfig` and direct handler wiring
  work; the table-driven configuration variants are deferred.
- **`inspect.getfullargspec` for built-in functions**. We expose
  signatures for Python-defined callables only.
- **PEP 695 type-parameter syntax** in introspection
  (`__type_params__`). The runtime field doesn't exist yet.

## Detailed design

### Crate-by-crate scope

#### `weavepy-vm` (Rust additions)

| Surface | File | LOC (approx.) |
|---------|------|--------------:|
| Frame/code/traceback objects | `lib.rs`, `object.rs` | +1500 |
| `sys` introspection extensions | `stdlib/sys.rs` | +250 |
| `_weakref` module | `stdlib/weakref_mod.rs` | 300 |
| `gc` module | `stdlib/gc_mod.rs` | 130 |
| `_datetime` shim | `stdlib/datetime_mod.rs` | 250 |
| `_contextvars` shim | `stdlib/contextvars_mod.rs` | 200 |
| `ExceptionGroup` / `BaseExceptionGroup` types | `builtin_types.rs` | +120 |

#### `weavepy-parser` / `weavepy-compiler`

| Surface | File | LOC (approx.) |
|---------|------|--------------:|
| `except*` AST and parser branch | `parser/ast.rs`, `parser/parser.rs` | +250 |
| `except*` compilation | `compiler/lib.rs` | +200 |
| `CheckEGMatch` opcode + dispatch | `compiler/bytecode.rs`, `vm/lib.rs` | +150 |

#### Frozen Python modules

| Module | Source file | LOC (approx.) |
|--------|-------------|--------------:|
| `weakref` (Python wrapper) | `stdlib/python/weakref.py` | 250 |
| `linecache` | `stdlib/python/linecache.py` | 130 |
| `warnings` | `stdlib/python/warnings.py` | 400 |
| `datetime` | `stdlib/python/datetime.py` | 1400 |
| `contextvars` | `stdlib/python/contextvars.py` | 150 |
| `traceback` | `stdlib/python/traceback.py` | 600 |
| `inspect` | `stdlib/python/inspect.py` | 900 |
| `logging` | `stdlib/python/logging.py` | 1300 |
| `logging.handlers` | `stdlib/python/logging_handlers.py` | 400 |
| `unittest` (package) | `stdlib/python/unittest_*.py` | 2500 |
| `unittest.mock` | `stdlib/python/unittest_mock.py` | 1800 |
| `runpy` | `stdlib/python/runpy.py` | 130 |
| `code`/`codeop` | `stdlib/python/code_mod.py` | 200 |

#### Asyncio integration

| Patch | File | LOC (approx.) |
|-------|------|--------------:|
| `TaskGroup` | `stdlib/python/asyncio.py` | +250 |
| `timeout` context manager | as above | +120 |
| Context propagation between tasks | as above | +60 |

#### Totals

~3K LOC Rust, ~10.5K LOC frozen Python, ~430 LOC asyncio additions,
plus ~1.5K LOC of new fixtures. Net diff ≈ **15–22K LOC** on a
generous count.

### Frame and traceback object model

Every WeavePy `Frame` (internal Rust struct) is now reachable as a
`PyFrame` (Python-visible object) via two paths:

```rust
pub struct PyFrame {
    pub code: Rc<CodeObject>,
    pub globals: Rc<RefCell<DictData>>,
    pub locals_snapshot: RefCell<Object>, // Object::Dict, lazily built
    pub lineno: Cell<u32>,
    pub back: RefCell<Option<Rc<PyFrame>>>,
    pub builtins: Rc<RefCell<DictData>>,
}
```

`PyFrame::from_frame(&Frame)` snapshots the current frame and
populates `back` from the interpreter's frame stack. Locals are
lazy because the snapshot is expensive — `frame.f_locals` does the
work on first access, subsequent accesses return the cached dict.

A `PyTraceback` mirrors the Python `types.TracebackType`:

```rust
pub struct PyTraceback {
    pub frame: Rc<PyFrame>,
    pub lineno: u32,
    pub next: RefCell<Option<Rc<PyTraceback>>>,
}
```

`PyException::traceback` is converted from the current
`Vec<TracebackEntry>` (used for fast appending in the unwind loop)
to a chained `Option<Rc<PyTraceback>>` lazily when user code asks
for `__traceback__`. This avoids paying for the chain when the
exception isn't introspected.

Two new `Object` variants — `Frame(Rc<PyFrame>)` and
`Traceback(Rc<PyTraceback>)` — join the enum. Both expose
attributes through the existing `LoadAttr` path.

### `sys` introspection extensions

```python
sys._getframe(depth=0)        # raises ValueError if depth exceeds stack
sys.exc_info()                # (type, value, traceback) of current handler
sys.excepthook(type, value, tb)        # default print to stderr
sys.unraisablehook(unraisable)         # __del__-style errors
sys.__excepthook__                     # cached original
sys.settrace(func)                     # no-op stub today
sys.setprofile(func)                   # no-op stub today
sys.getsizeof(obj, default=None)       # best-effort
```

The interpreter keeps a stack of "currently handled" exceptions,
distinct from `frame.exc_handlers` (the WHICH-handler runs stack).
`sys.exc_info()` peeks the most recent handled exception or
returns `(None, None, None)` if there is none. This is hooked into
the existing `PushExcInfo` / `PopExcept` machinery — the same data
that powers `raise` (no arg) inside an `except:` clause.

`sys.excepthook` defaults to a Rust builtin that formats the
exception via the new `traceback.print_exception` (when imported)
or falls back to a minimal renderer. The CLI calls
`sys.excepthook` on the top-level exception instead of formatting
inline, so user overrides take effect.

### PEP 654: `BaseExceptionGroup` / `ExceptionGroup`

Two new built-in exception types join the registry:

```text
BaseException
├── BaseExceptionGroup              (PEP 654)
│   └── ExceptionGroup              (PEP 654; also derives from Exception)
└── Exception
    ├── ExceptionGroup              (sibling-through-MRO)
    └── ... existing ...
```

`ExceptionGroup` has dual inheritance from both `BaseExceptionGroup`
and `Exception`. The MRO works because both ultimately derive from
`BaseException`. The constructor signature is
`BaseExceptionGroup(msg, exceptions)`:

```python
eg = BaseExceptionGroup("partial", [TypeError("a"), ValueError("b")])
eg.message       # "partial"
eg.exceptions    # (TypeError("a"), ValueError("b"))
eg.split(TypeError)
# (BaseExceptionGroup("partial", [TypeError("a")]),
#  BaseExceptionGroup("partial", [ValueError("b")]))
eg.subgroup(TypeError)
# BaseExceptionGroup("partial", [TypeError("a")])
eg.derive([])    # subclasses can override; default re-builds same class
```

The factory in `builtin_types::make_exception_with_class` is
extended to recognise `BaseExceptionGroup` / `ExceptionGroup` and
treat the second positional argument as the `exceptions` tuple
(stored both on `args[1]` and as the named attribute).

### `except*` parser and compiler

The parser sees `except*` as a sequence of `Except` + `*` + a type
expression. The AST grows a `is_star: bool` field on the existing
`ExceptHandler` node so `Try` keeps a single handler list:

```rust
pub struct ExceptHandler {
    pub span: Span,
    pub type_: Option<Expr>,
    pub name: Option<String>,
    pub body: Vec<Stmt>,
    pub is_star: bool,   // RFC 0018
}
```

Mixing `except` and `except*` in the same `try` is a `SyntaxError`,
matching CPython. The parser checks this and emits a diagnostic.

The compiler lowers a `try / except* X1: body1 / except* X2: body2`
to:

```text
    SETUP_HANDLER egH
    <body>
    JUMP_FORWARD done
egH:
    PUSH_EXC_INFO          ; exception is on the stack
    ; iterate handlers, splitting the group each time
    LOAD_FAST exc
    LOAD_NAME X1
    CHECK_EG_MATCH         ; pops type, peeks exc; pushes (matched, rest)
    STORE_FAST _matched
    STORE_FAST exc         ; rest becomes the new "remaining" exc
    LOAD_FAST _matched
    POP_JUMP_IF_FALSE next1
    STORE_FAST e1           ; bind matched group to user variable
    <body1>
    DELETE_FAST e1
next1:
    LOAD_FAST exc
    LOAD_NAME X2
    CHECK_EG_MATCH
    STORE_FAST _matched
    STORE_FAST exc
    LOAD_FAST _matched
    POP_JUMP_IF_FALSE next2
    STORE_FAST e2
    <body2>
    DELETE_FAST e2
next2:
    LOAD_FAST exc          ; whatever remains
    DUP
    LOAD_CONST None
    COMPARE_OP ==
    POP_JUMP_IF_TRUE finally  ; all matched — keep going
    RERAISE_EG              ; raise the leftover group
finally:
    POP_EXCEPT
done:
```

`CHECK_EG_MATCH` is a new opcode. It pops the type from TOS, peeks
the exception group below it, and *splits* the group: it pushes
`(matched_group, remaining_group)` as two stack values. `matched_group`
is `None` if no exceptions in the group matched. `remaining_group`
is `None` if everything matched.

If the exception caught by `try` is **not** a group, the opcode
synthesises a singleton group around it (the semantics match
CPython: a bare `except* ValueError` matches a plain `ValueError`).

### `weakref`

We model `weakref` as a *best-effort* weak reference: it stores an
`Rc` to its referent today (so it doesn't actually become invalid
when the strong references drop in this interpreter's reference
counting), but the surface is correct. When (in a future RFC) the
object model gains real tracing GC, we can flip the storage to
`Weak<…>` without changing the user-visible API.

Rust core:

```rust
pub struct PyWeakRef {
    pub object: RefCell<Option<Object>>,
    pub callback: Object,
    pub alive: Cell<bool>,
}
```

`ref(obj, callback=None)` returns a `PyWeakRef`. Calling the ref
(`r()`) returns the original object (or `None` if cleared via
`finalize`). `proxy(obj)` returns the original (a true proxy
requires `__getattribute__` instrumentation we don't yet have).

Python wrapper adds `WeakSet`, `WeakValueDictionary`,
`WeakKeyDictionary`, `finalize`. `finalize` registers a
finalisation callback; the callback fires when `finalize.detach()`
is called or the holding `finalize` object goes out of scope —
emulating "destructor" semantics without real GC participation.

### `gc`

```python
gc.collect(generation=2)   # returns 0 (no cycle collection)
gc.get_count()             # returns (0, 0, 0)
gc.get_threshold()         # returns (700, 10, 10)
gc.set_threshold(*args)    # no-op
gc.disable() / enable() / isenabled()  # toggles a Python-visible flag
gc.get_objects()           # returns []  (we don't track all live)
gc.is_finalized(obj)       # always False
```

The module exists primarily so user code can `import gc; gc.collect()`
without `ModuleNotFoundError`.

### `datetime`

`_datetime` (Rust) provides:

```rust
_datetime.now_components() -> (year, month, day, hour, minute, second, microsecond, tz_offset_seconds)
_datetime.utc_components() -> ...
_datetime.monotonic_ns() -> i64
_datetime.from_timestamp(ts, tz_aware) -> components
_datetime.epoch_from_components(year, ...) -> f64 seconds
```

Frozen Python builds `date`, `time`, `datetime`, `timedelta`,
`timezone`, `tzinfo` on top of these primitives. The leap-aware
date arithmetic follows the Gregorian calendar — the same one
CPython uses — and matches `datetime.fromisoformat` /
`datetime.isoformat` for the common cases. Sub-microsecond
precision is not supported (CPython's isn't either).

### `contextvars`

`_contextvars` (Rust) maintains a stack of context dicts and the
"current" context pointer. The user-visible API is frozen Python:

```python
ctx = contextvars.copy_context()
ctx.run(fn, *args, **kwargs)

var = contextvars.ContextVar("name", default=...)
token = var.set(value)
var.get(default=...)
var.reset(token)
```

The asyncio loop is patched so that:

- Every `loop.call_soon(cb, *args)` records the calling context.
- The scheduled callback runs inside that context.
- `Task.__init__` records the creating context and re-enters it on
  every `_step`.

This is how `logging.LogRecord.context` survives across
`await` boundaries.

### `linecache`

Straight port of `Lib/linecache.py`. Caches `(mtime, size, lines)`
keyed on filename; `getline(filename, lineno)` walks the cache and
re-reads from disk on stat-mismatch.

### `warnings`

`Lib/warnings.py` is large; the WeavePy port covers the public
surface:

- `warn(message, category=UserWarning, stacklevel=1, source=None)`
- `warn_explicit`
- `filterwarnings`, `simplefilter`, `resetwarnings`
- `catch_warnings()` (context manager that snapshots and restores
  the filter list)
- `showwarning(message, category, filename, lineno, file=None, line=None)`
- Categories: `Warning`, `UserWarning`, `DeprecationWarning`,
  `PendingDeprecationWarning`, `SyntaxWarning`, `RuntimeWarning`,
  `FutureWarning`, `ImportWarning`, `BytesWarning`,
  `ResourceWarning`, `EncodingWarning`.

### `traceback`

Built on top of the new `Frame`/`Traceback` objects:

- `format_exc(limit=None, chain=True)` — current exception, chain.
- `print_exc(limit=None, file=None, chain=True)` — to `sys.stderr`.
- `walk_tb(tb)`, `walk_stack(frame=None)` — generators.
- `StackSummary` / `FrameSummary` — frozen-Python classes
  carrying filename / lineno / name / line text.
- `TracebackException(type, value, tb, limit=None, lookup_lines=True,
  capture_locals=False, compact=False)` — the canonical
  exception-rendering helper used by `unittest`.

`traceback.format_exception_only` returns the formatted
`Type: message` line plus any `__notes__` annotations
(PEP 678 — `BaseException.add_note(...)` is shipped here).

### `inspect`

`inspect` is the largest of the frozen modules. The supported
surface:

- Predicates: `isfunction`, `ismethod`, `isbuiltin`, `isclass`,
  `ismodule`, `isgenerator`, `iscoroutine`, `isawaitable`,
  `isasyncgen`.
- Source: `getsource`, `getsourcefile`, `getsourcelines`,
  `findsource` (off `linecache`).
- Frames: `currentframe`, `stack`, `getouterframes`,
  `getframeinfo`, `FrameInfo` namedtuple.
- Signature: `Signature(parameters)`, `Parameter(name, kind,
  default, annotation)`, `signature(callable)`,
  `Parameter.POSITIONAL_ONLY` / `POSITIONAL_OR_KEYWORD` /
  `VAR_POSITIONAL` / `KEYWORD_ONLY` / `VAR_KEYWORD`.
- Module / class introspection: `getmembers`,
  `getmembers_static`, `getmro`, `getmodule`, `getfile`,
  `getdoc`.

The signature extractor reads the underlying `CodeObject` attributes
(`argcount`, `kwonlyargcount`, `varnames`, `flags`, `posonlyargcount`)
that the VM exposes; new compiler work to populate
`co_posonlyargcount` accurately is part of this RFC.

### `logging`

The frozen `logging` follows `Lib/logging/__init__.py` closely:

- `Logger`, `Manager`, `Handler`, `Formatter`, `Filter`,
  `LogRecord`, `PlaceHolder`, `RootLogger`.
- Levels: `DEBUG`, `INFO`, `WARNING`, `ERROR`, `CRITICAL`,
  `WARN`, `NOTSET`.
- `getLogger(name)`, `basicConfig(**kwargs)`, `getLevelName`,
  `addLevelName`, the module-level convenience helpers
  (`info`, `warning`, etc.).
- Handlers: `StreamHandler`, `FileHandler`, `NullHandler`.
- `logging.handlers` ships `RotatingFileHandler`,
  `TimedRotatingFileHandler` (best-effort), `MemoryHandler`,
  `QueueHandler` (sync), `WatchedFileHandler`.

`Formatter` supports `%`, `{`, and `$` styles. `LogRecord` carries
the full PEP 282 + PEP 3101 attribute set used by canonical format
strings.

### `unittest`

The package ships as several frozen modules — `unittest` itself is
the `__init__.py`-equivalent surface, plus internal submodules:

- `unittest.case` → `TestCase`, `IsolatedAsyncioTestCase` (stub),
  `_ShouldStop`, `SkipTest`, the full assert family.
- `unittest.suite` → `TestSuite`, `BaseTestSuite`.
- `unittest.loader` → `TestLoader`, `defaultTestLoader`,
  `_make_failed_load_tests`.
- `unittest.runner` → `TextTestRunner`, `TextTestResult`.
- `unittest.result` → `TestResult`.
- `unittest.main` → `main` / `TestProgram`.
- `unittest.mock` → see below.

The `assertX` family is comprehensive: `assertEqual`,
`assertNotEqual`, `assertTrue`, `assertFalse`, `assertIs`,
`assertIsNot`, `assertIsNone`, `assertIsNotNone`, `assertIn`,
`assertNotIn`, `assertIsInstance`, `assertNotIsInstance`,
`assertRaises`, `assertRaisesRegex`, `assertWarns`,
`assertWarnsRegex`, `assertAlmostEqual`, `assertNotAlmostEqual`,
`assertGreater`, `assertGreaterEqual`, `assertLess`,
`assertLessEqual`, `assertRegex`, `assertNotRegex`,
`assertCountEqual`, `assertMultiLineEqual`, `assertSequenceEqual`,
`assertListEqual`, `assertTupleEqual`, `assertSetEqual`,
`assertDictEqual`, plus `fail`.

The runner produces CPython-shaped output: a per-test `.` / `F` /
`E` / `s` summary, then per-failure traceback, then `Ran N tests in
T.Ts`, then `OK` / `FAILED (failures=…)`. Output stability with
CPython is good enough that `python -m unittest discover ...`
under WeavePy produces the same summary line as under CPython for
the test fixtures.

### `unittest.mock`

The `Mock` / `MagicMock` / `patch` family:

- `Mock(spec=None, side_effect=None, return_value=DEFAULT, **kwargs)`:
  auto-creating attribute access, call recording, configurable
  return value or side-effect.
- `MagicMock` — Mock with all magic methods auto-populated as
  Mocks.
- `_Call`, `call`, `call.func.attr(...)` builders for assertion.
- `patch(target, ...)` and `patch.object(obj, attr, ...)` —
  context-manager and decorator forms.
- `sentinel.NAME` — unique singletons for test parameterisation.
- `ANY` — equality wildcard.
- `PropertyMock` — used as the value of a class attribute to
  override a property's `__get__`/`__set__`.
- `seal(mock)` — freeze further auto-attribute creation.

The deep magic of CPython's `_MockBase` (auto-spec, side-effect
introspection of `wraps`) is approximated; the common patterns
(`mock.return_value = X`, `mock.assert_called_with(...)`,
`patch("module.fn", new=...)`) work.

### asyncio additions

`TaskGroup`:

```python
async with asyncio.TaskGroup() as tg:
    t1 = tg.create_task(coro1())
    t2 = tg.create_task(coro2())
# All children awaited on exit. Exceptions are wrapped in
# ExceptionGroup; the group is raised at the end of `async with`
# on the parent.
```

`asyncio.timeout(delay)` and `asyncio.timeout_at(when)`:

```python
async with asyncio.timeout(5):
    await long_running()
# Raises TimeoutError after delay (or wraps in CancelledError
# inside the body).
```

Both rely on `BaseExceptionGroup` for the multi-failure case.

### Compiler — `co_posonlyargcount` propagation

The existing compiler tracks `posonly_argcount` via a flag on
`Arguments` but didn't publish it on `CodeObject`. We add
`co_posonlyargcount` (and `co_kwonlyargcount`, which was already
on the AST but not surfaced) so `inspect.signature` can produce
the right `POSITIONAL_ONLY` parameters.

### Tests

15 new end-to-end fixtures land:

| Fixture | Description |
|---------|-------------|
| `56_introspection.py` | `sys._getframe`, `frame.f_lineno`, `traceback` walk |
| `57_traceback.py` | `traceback.format_exc`, `walk_tb`, chained exceptions |
| `58_inspect.py` | `signature`, `getmembers`, `getmro`, `getsource` |
| `59_unittest_basic.py` | `TestCase`, `assertEqual`, `setUp/tearDown` |
| `60_unittest_mock.py` | `Mock`, `MagicMock`, `patch`, `call.assert_called` |
| `61_datetime.py` | `date`, `datetime`, `timedelta`, `timezone` arithmetic |
| `62_warnings.py` | `warn`, `filterwarnings`, `catch_warnings` |
| `63_weakref.py` | `ref`, `WeakValueDictionary`, `finalize` |
| `64_exception_group.py` | `BaseExceptionGroup`, `except*` matching |
| `65_asyncio_taskgroup.py` | `TaskGroup`, `asyncio.timeout`, partial-fail propagation |
| `66_contextvars.py` | `ContextVar`, `copy_context`, asyncio task propagation |
| `67_runpy.py` | `run_path`, `run_module` |
| `68_gc.py` | `gc.collect`, knob accessors |
| `69_unittest_discovery.py` | `TestLoader.discover` + run on a sibling test file |
| `70_logging.py` | `getLogger`, `basicConfig`, `StreamHandler`, `Formatter` |

## Drawbacks

- **No cycle collection.** `gc.collect()` returns 0 because the
  interpreter uses reference counting; reference cycles still
  leak. User code that relies on `gc.collect()` to break a cycle
  before raising on a finalizer will not see the side effect. The
  module is shaped so the call doesn't raise; the behavioural
  divergence is documented.
- **`weakref` is a strong reference today.** The surface is right
  (the ref-callable returns the referent; `WeakValueDictionary`
  acts as a value-keyed mapping; `finalize` registers a callback).
  Real weak semantics await the GC refactor.
- **`sys.settrace` / `setprofile` are no-ops.** A real tracing
  hook means the VM dispatch loop pays a per-instruction
  function-call cost; we defer until the tracing JIT / debugger
  RFC.
- **`inspect.signature` reads the runtime code object directly**,
  but for callables wrapped by user decorators that don't preserve
  `__wrapped__`, the signature reported is the wrapper's, not the
  wrapped function's. `functools.wraps` already sets `__wrapped__`,
  so the common case works.
- **`unittest.IsolatedAsyncioTestCase`** ships as a stub that
  raises `unittest.SkipTest` if instantiated. The wiring exists;
  the loop-management interaction with `asyncio.run` is a
  follow-up.
- **`logging.config.fileConfig` / `dictConfig`** absent. Direct
  handler wiring is what the test suite needs; the table-driven
  configurators are out of scope.
- **`traceback` carets are line-precision only.** PEP 657's
  fine-grained location annotations require code-object support
  we don't ship in this commit.

## Alternatives

- **Vendor CPython's actual `Lib/unittest/*`.** Rejected: those
  modules use `inspect.getfullargspec` heavily on the test
  methods, multiple `__class_getitem__` on built-ins, and
  `unittest.main`'s `argparse` interaction relies on parts of
  `_pyio` we don't ship. The frozen port is smaller, more
  focused, and we still match the user-visible behaviour for the
  patterns real test code uses.
- **Skip `mock` and require users to bring `pytest`.** Rejected:
  `unittest.mock` is in the standard library since 3.3; every
  Python test corpus assumes it works.
- **Build `inspect` on top of an `ast` module instead of on
  introspecting `CodeObject`.** Rejected: shipping a full `ast`
  reflection of WeavePy's AST is its own RFC (and arguably belongs
  alongside an in-Python compiler). The code-object route is
  smaller and matches what `inspect.signature` actually does.
- **Defer PEP 654.** Tempting (it's the biggest single addition).
  Rejected because `asyncio.TaskGroup` is the natural way to wire
  the whole "shipping concurrency" story together, and `TaskGroup`
  needs `BaseExceptionGroup`.
- **Implement `logging` as a Rust core.** Rejected: the public
  shape is large, callback-heavy, and CPython itself ships it in
  Python. We follow the precedent.

## Prior art

- **CPython 3.13** — the conformance target.
- **PyPy** — ships the same `Lib/unittest`, `Lib/logging`,
  `Lib/inspect` unchanged because their built-in inheritance is
  complete. We rewrite the public surface because ours isn't.
- **RustPython** — implements `unittest` / `logging` / `inspect`
  with a similar split (Rust shim + frozen Python). Their
  signature extractor reads `co_*` directly the way ours does.
- **MicroPython** — `umock` is a much smaller subset; `inspect`
  is absent. Useful comparison for "what's the minimum to ship
  and feel like Python."

## Unresolved questions

- **`unittest.main()` arg parsing under `python -m`.** The
  package-as-script flow needs `runpy.run_module(__name__ ==
  "__main__")` to fire `unittest.main()`; we get there via
  `runpy.run_module`, but the `argparse` interaction inside
  `unittest.main` is opinionated about being argv-position-0.
  Tracked.
- **`Frame.f_locals` mutation.** CPython makes `f_locals` a *copy*
  of the function's locals — writes don't propagate back. We
  follow that. `class`/`exec`/module-scope frames *do* persist
  writes; we model that distinction by checking the code's `kind`.
- **`traceback.format_exc()` on a coroutine never awaited.**
  CPython emits `RuntimeWarning: coroutine 'X' was never awaited`
  on garbage collection; we don't have finalisers, so the warning
  is silent. Not blocking.
- **`logging.Logger.findCaller`** walks up the stack looking for
  the first frame *outside* the `logging` module. Our
  `sys._getframe` returns frames in the right order; the filename
  check works because frozen modules carry a synthetic name (e.g.
  `<frozen logging>`).

## Future work

- **`pdb` / `bdb`** — interactive debugger built on the
  introspection hooks here.
- **PEP 657 fine-grained tracebacks** — column-precise `^^^^`
  carets. Requires code-object support and a column-aware
  compiler.
- **Real `weakref` semantics** — paired with tracing GC.
- **`logging.config.dictConfig`** — the structured configurator.
- **`unittest.IsolatedAsyncioTestCase`** with full loop management.
- **`asyncio.Runner`** API — CPython 3.11+ alternative to
  `asyncio.run` with explicit context management.
- **`inspect.cleandoc`, `inspect.getclosurevars`** — rarely-used
  helpers that round out the inspect surface.
- **`unittest.mock.create_autospec`** — auto-derive a mock that
  matches a target's signature. Requires `inspect.signature`
  on built-in callables.

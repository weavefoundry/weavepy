# RFC 0004: Exceptions, `try` / `except` / `finally`, `with`, tracebacks

- **Status**: Accepted
- **Authors**: WeavePy authors
- **Created**: 2026-05-20
- **Tracking issue**: TBD

## Summary

Wire exceptions end-to-end. After this RFC lands:

- Python code can raise (`raise`, `raise X from Y`), catch
  (`except`, `except as`), match by type and inheritance, run cleanup
  (`finally`), and use context managers (`with`, `with … as …`).
- The whole exception **class hierarchy** is in place
  (`BaseException` / `Exception` / `TypeError` / `ValueError` / …).
- The internal `PyException` carrier becomes a real Python exception
  *instance* — same value the user sees in `except E as e:`.
- The `__cause__` / `__context__` chaining attributes work for
  `raise X from Y` and implicit chaining inside `except` blocks.
- Tracebacks are rendered CPython-style with the file, line, and
  the offending source slice for every frame.

This RFC is co-landed with RFC 0003 (classes) because exception
types are classes — the inheritance walk for `except T:` matching
*is* `issubclass(type(exc), T)`.

## Motivation

After RFC 0001 we had `PyException { kind: &'static str, message: String }`
and helpers like `type_error()`. That was enough to bubble out an
informative diagnostic, but Python code couldn't intercept it: no
`try` block, no `except` clause, no `raise` from user code.

This is the single biggest blocker for running real Python code.
CPython's stdlib raises and catches exceptions on virtually every
hot path; `unittest.TestCase` and the conformance harness's intended
`regrtest` mode both require working `try`/`except`/`finally`.

## CPython reference

This RFC tracks **CPython 3.13**:

- `Python/compile.c` — exception-table emission (PEP 657-style
  out-of-line lookup), `RAISE_VARARGS`, `CHECK_EXC_MATCH`,
  `PUSH_EXC_INFO`, `POP_EXCEPT`, `RERAISE`.
- `Objects/exceptions.c` — the `BaseException` hierarchy and its
  attribute layout (`.args`, `.__cause__`, `.__context__`,
  `.__traceback__`, `.__suppress_context__`).
- `Python/ceval.c` — exception propagation through call frames,
  re-raise semantics, `with`-statement unwinding.
- The "Exceptions" chapter of the language reference for the catch
  ordering rule (most-specific clause first; tuple matches).

## Detailed design

### Exception instance object

The `PyException` carrier loses its old `&'static str` kind. It now
wraps an `Object::Instance` of an exception class:

```rust
pub struct PyException {
    pub instance: Object,                 // Object::Instance whose class' MRO contains BaseException
    pub traceback: Vec<TracebackEntry>,   // accumulated by the dispatch loop
}
pub struct TracebackEntry {
    pub filename: String,
    pub funcname: String,
    pub lineno: u32,           // 1-based; 0 when unknown
}
```

The Rust-level constructors (`type_error`, `value_error`, …) reach
into a thread-local `BUILTIN_TYPES` registry to build an instance of
the right exception class. This keeps the call sites unchanged while
turning every error into a real catchable instance.

`RuntimeError::PyException(PyException)` carries that bundle through
the dispatch loop; nothing else about the error-flow code changes.

### Exception class hierarchy

A small fixed hierarchy is registered into `BUILTIN_TYPES` at
interpreter startup:

```
BaseException
├── SystemExit
├── KeyboardInterrupt
├── GeneratorExit
└── Exception
    ├── StopIteration
    ├── ArithmeticError
    │   ├── ZeroDivisionError
    │   └── OverflowError
    ├── AssertionError
    ├── AttributeError
    ├── ImportError
    │   └── ModuleNotFoundError
    ├── LookupError
    │   ├── IndexError
    │   └── KeyError
    ├── NameError
    │   └── UnboundLocalError
    ├── OSError
    │   ├── FileNotFoundError
    │   └── PermissionError
    ├── RuntimeError
    │   ├── NotImplementedError
    │   └── RecursionError
    ├── SyntaxError
    │   └── IndentationError
    ├── TypeError
    └── ValueError
        └── UnicodeError
```

This is the subset CPython's stdlib raises on the hot path. The full
hierarchy (`Warning`, `BlockingIOError`, …) lands when the stdlib
bootstrap (future RFC) needs them.

`BaseException.__init__(self, *args)` stores `args` on the instance;
`__str__` joins them; the catch machinery binds the *instance* (not
the class) into `except E as e:`.

### Exception tables

Each `CodeObject` grows an `exception_table: Vec<ExcHandler>`:

```rust
pub struct ExcHandler {
    /// Instruction range covered by this handler [start, end).
    pub start: u32,
    pub end:   u32,
    /// First instruction of the handler block.
    pub handler: u32,
    /// Stack depth to restore before entering the handler.
    pub depth:  u32,
    /// Pushed onto the handler when the wrapped block is a `finally`
    /// (so the handler knows to re-raise after running).
    pub push_lasti: bool,
}
```

When a `RuntimeError::PyException` bubbles up to the dispatch loop,
the VM looks up the *current* PC in this table:

- If a handler matches, unwind the stack to `depth`, push the
  exception instance, jump to `handler`.
- If nothing matches, propagate to the calling frame.

This is CPython 3.11+'s model — out-of-line exception handlers, no
runtime block stack. Faster and simpler than the older SETUP_FINALLY
chain.

### Compile shapes

`try: <body> except T: <h>` compiles to roughly:

```
                  body...                  ; ┐
                  JUMP_FORWARD end         ; │
handler:          PUSH_EXC_INFO            ; │  exception table entry
                  CHECK_EXC_MATCH <T>      ; │  start..body_end → handler
                  POP_JUMP_IF_FALSE next   ; │  depth=stack_height_at_start
                  STORE_FAST e             ; ┘
                  h...
                  POP_EXCEPT
                  JUMP_FORWARD end
next:             RERAISE                  ; unmatched: propagate
end:              ...
```

`finally` blocks compile by *duplicating* the finally body — once on
the normal-exit path and once on the exception path, the latter
emitting a `RERAISE` at the bottom. (This matches what CPython 3.13
does for non-async `finally`.)

`with cm as x: body` lowers to:

```
                  <cm>
                  COPY                    # cm twice for __exit__ later
                  LOAD_ATTR __exit__
                  SWAP 2
                  LOAD_ATTR __enter__
                  CALL 0
                  STORE_FAST x
                  ; exception table covers the body
                  body...
                  LOAD_CONST None
                  LOAD_CONST None
                  LOAD_CONST None
                  CALL __exit__ 3
                  POP_TOP
                  JUMP_FORWARD end
handler:          WITH_EXCEPT_START       # call __exit__(exc_type, exc_val, exc_tb)
                  POP_JUMP_IF_TRUE swallow
                  RERAISE
swallow:          POP_TOP
                  POP_EXCEPT
end:              ...
```

### `raise`

```
raise            -> RAISE_VARARGS 0   # re-raise current
raise X          -> RAISE_VARARGS 1   # raise instance/class
raise X from Y   -> RAISE_VARARGS 2   # raise X with __cause__=Y
```

`RAISE_VARARGS 1` accepts either an instance (raise it directly) or a
class (`raise TypeError` instantiates `TypeError()` and raises that).

### Implicit chaining

When an exception is raised *inside* an `except` block, CPython sets
the new exception's `__context__` to the one currently being handled.
We track the active exception via a small stack on the interpreter
(`active_exceptions: Vec<Object>`); `PUSH_EXC_INFO` and `POP_EXCEPT`
maintain it.

### Iterator protocol unification

With real `StopIteration` available, the iterator protocol unifies:

- `GET_ITER` looks up `__iter__` on the receiver's type. If absent,
  falls back to the built-in iterator machinery (lists, tuples,
  str, dict, range).
- `FOR_ITER` calls `__next__` on the iterator. If that raises
  `StopIteration`, the dispatch loop catches it inline, drops the
  iterator, and jumps to the loop exit.
- `next(it[, default])` runs the same protocol and falls back to
  `default` on `StopIteration` if provided, else re-raises.

### Traceback rendering

The dispatch loop maintains a stack of `(filename, funcname, lineno)`
entries — one per frame on the call stack. When an exception escapes
to `weavepy::Error::format`, it walks them in reverse order and
prints:

```
Traceback (most recent call last):
  File "<filename>", line 12, in <module>
    foo(1, 2)
  File "<filename>", line 3, in foo
    raise ValueError("nope")
ValueError: nope
```

The line content is sliced from the original source when available,
matching CPython's behavior. Frames without source info fall back to
the funcname only.

For `raise X from Y` we additionally print CPython's chained line:

```
The above exception was the direct cause of the following exception:
```

For implicit `__context__` chains:

```
During handling of the above exception, another exception occurred:
```

## Drawbacks

- **No `try / except*` (PEP 654 exception groups).** The slice
  parses but rejects them with a `NotImplemented` pointing here.
  Exception groups land with the async work (RFC 0006) where they're
  most useful.
- **Linenos are approximate.** The compiler emits one line table entry
  per AST node, not per bytecode instruction. Acceptable: stdlib code
  doesn't notice; CPython itself adopted PEP 657's more precise model
  later. Tightening is a follow-up.
- **No bidirectional `__traceback__`.** We attach a list of frames to
  the exception, but the per-frame Python-visible `__traceback__`
  object (which the `traceback` module walks) is deferred — current
  rendering bypasses it.

## Alternatives

- **Keep the older SETUP_FINALLY / POP_BLOCK block stack.** Rejected:
  CPython moved away from it because exception tables are both faster
  and simpler. Adopting the old model now would mean rewriting it
  later.
- **Skip exception classes; keep `kind: &'static str`.** Considered.
  Rejected because `except SomeBase:` matching depends on
  inheritance — we'd have to fake a parallel "kind tree" anyway.
  Folding it into the type system is strictly less code.
- **Defer `with` to a later RFC.** `with` is ~50 lines once exceptions
  exist; splitting it off would just delay context managers (which
  every test runner uses) for no benefit.

## Unresolved questions

- Whether `RecursionError` should be raised from the dispatch loop on
  a configurable stack-depth limit. Today we let the host stack
  overflow. Tracked; not blocking.
- The exact text of multi-line traceback messages around chained
  exceptions. We match CPython 3.13 for the obvious cases; obscure
  cases (e.g. `__suppress_context__` toggled by user code mid-flow)
  may diverge until we cover them in conformance.

## Future work

- **RFC 0011**: exception groups (`try/except*`), PEP 654.
- **RFC 0009**: full `traceback` module compatibility, including
  Python-visible traceback objects.
- **RFC 0006**: generator exceptions, `GeneratorExit`, async
  cancellation, `KeyboardInterrupt` plumbing.

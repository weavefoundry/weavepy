# RFC 0006: Generators, `yield`, and `yield from`

- **Status**: Accepted (synchronous half)
- **Authors**: WeavePy authors
- **Created**: 2026-05-21
- **Tracking issue**: TBD

## Summary

Wire up Python's generator machinery — `yield`, `yield from`,
generator functions, and real lazy generator expressions. After this
RFC lands:

- `def f():\n  yield 1\n  yield 2` returns a `generator` object whose
  `__next__` / `send` / `throw` / `close` methods drive the frame.
- `yield from sub_gen()` delegates iteration faithfully, propagating
  `send` values and `StopIteration.value` exactly as CPython does.
- Generator expressions (`(x*x for x in xs)`) execute lazily —
  consuming them via `next()` advances the underlying frame one yield
  at a time, rather than eagerly building a list (which is what the
  slice did pre-RFC).
- The `iter()` / `next()` builtins gain proper generator support;
  `for x in gen` iterates without copying.
- `sys.getrecursionlimit()` and friends are unchanged — generators
  do not interact with the Python recursion limit.

**Out of scope for this RFC** and tracked separately as
**RFC 0006-B**:

- `async def`, `await`, `async for`, `async with`.
- The `asyncio` event loop and `asyncio.run` / `gather` / `sleep`.
- `Coroutine` and `AsyncGenerator` runtime types.

We split the RFC because the *suspendable-frame* machinery is shared
between sync generators and async coroutines, but the surface area
(scheduling, event loop, transports) for async is large enough to
deserve its own design pass.

## Motivation

Iteration in real Python is generator-based far more often than
class-based. Every `__iter__` that yields, every `itertools` helper,
every "lazy stream" pattern, every `for line in file` (under the
hood — `_io.TextIOWrapper.__iter__` is a generator) flows through
`yield`. Without generators we can't run:

- `os.walk` (yields directory tuples).
- `csv.reader` (yields rows).
- `re.finditer` (yields match objects).
- Most of `itertools` (vendored as pure Python with `yield`).
- Most Django / FastAPI / pytest helpers — anything that streams.

The slice's class-based `__iter__` / `__next__` is enough for
hand-rolled iterators (we exercise this in `14_iter_protocol.py`),
but it's an order of magnitude more code to write than `yield x` and
nobody does it in modern Python.

## CPython reference

Tracks **CPython 3.13**:

- `Objects/genobject.c` — `genobject_send`, `gen_throw`,
  `gen_close`, and the suspended-frame layout.
- `Python/ceval.c` — `YIELD_VALUE` / `RESUME` / `SEND` /
  `GET_YIELD_FROM_ITER` dispatch.
- `Python/compile.c` — generator detection (`co_flags & CO_GENERATOR`),
  emission for `yield from`.
- Language reference, "Yield expressions" — semantics of
  `send()`, `throw()`, `close()`, `StopIteration.value`.

The slice **does not track**:

- Coroutines, asynchronous generators, `await` (RFC 0006-B).
- `GeneratorExit` propagation through `close()` (we raise it but
  don't run finally handlers on close — bug-for-bug parity is a
  follow-up).
- Generator pickling / `gi_running` introspection (deferred).

## Detailed design

### AST additions

```rust
ExprKind::Yield(Option<Box<Expr>>),       // yield, yield expr
ExprKind::YieldFrom(Box<Expr>),           // yield from expr
```

`yield` is an *expression* in Python; the value it returns is what
`generator.send(v)` passed in. The parser treats it as a top-level
expression (it doesn't bind in operator-precedence chains except via
parentheses).

`dump_module` extends to render
`Yield(value=...)` and `YieldFrom(value=...)`.

### Parser

`yield` and `yield from` are added as legal start-of-expression
keywords inside `parse_atom` (matching CPython). They produce
`ExprKind::Yield` / `ExprKind::YieldFrom`. Outside a function body
the **compiler** rejects them with `SyntaxError`; the parser is
permissive because top-level `yield` does appear in expression
contexts that share the parser path (e.g. parenthesized yield).

The existing `parse_statement` path for `Keyword::Yield` is replaced:
it now dispatches to `parse_simple_statement` which handles `yield`
as an expression statement.

### Code-object flag

A new `CodeObject::is_generator: bool` is set during compile when
*any* `yield` or `yield from` appears inside the function body
(directly, not inside nested functions). Generator expressions
produce a code object with `is_generator = true` and a single
parameter `.0` (the outer iterator), matching CPython.

### Compiler

New opcodes:

```rust
OpCode::YieldValue,        // pop value, suspend frame, return to caller
OpCode::GetYieldFromIter,  // pop iterable → push iter()
OpCode::Send,              // delegated send into a sub-iterator
OpCode::ReturnGenerator,   // emitted in generator prologue
```

For a regular `yield x`:

```
LOAD_FAST   x
YIELD_VALUE
                            ; on resume, the sent value is at TOS
```

For `yield from sub`:

```
LOAD_FAST   sub
GET_YIELD_FROM_ITER
LOAD_CONST  None
loop:
  SEND  end_loop          ; sends value to sub-iter; jumps on StopIteration
  YIELD_VALUE             ; re-yields the value
  JUMP_BACKWARD loop
end_loop:
  END_FOR                 ; cleanup
```

This mirrors CPython's lowered shape; the only deviation is that we
keep the same `{ op, arg }` instruction encoding the rest of the slice
uses rather than CPython's 16-bit packed form.

When a code object has `is_generator`, the compiler emits a
`RETURN_GENERATOR` at the very start of the body (right after
`RESUME 0`). The VM intercepts this and stops executing the frame
immediately, instead constructing a `Generator` object that captures
the frame and returns it to the caller. The caller's `next()` /
`send()` calls then resume the frame from where `RETURN_GENERATOR`
left off (which is the first real instruction of the body).

Generator expressions are compiled like comprehensions but without
the eager list-building wrapper: the inner code object yields each
element instead of appending to a list.

### VM: suspendable frames

Today `Frame` owns its stack and is consumed by `run_frame`. To
support resume, we change the model so that:

- A `PyGenerator` owns its `Frame` (in an `RefCell`).
- `next(gen)` / `send(gen, v)` borrows the frame, runs it until the
  next `YIELD_VALUE` or function return, then returns the value (or
  raises `StopIteration` on return).
- On `YIELD_VALUE`, `run_frame` returns a new `StepOutcome::Yield(v)`
  variant which the caller (the generator object's send-loop) maps
  to a Python value. The frame's `pc` already points past the yield,
  so resume just continues the dispatch loop.
- On normal return (final value pushed → `ReturnValue`), `send` /
  `next` raises `StopIteration(value)`.

`PyGenerator` schematic:

```rust
pub struct PyGenerator {
    pub name: String,
    pub qualname: String,
    pub frame: RefCell<Frame>,
    /// `True` while a `send` call is in-flight; used to detect
    /// the "ValueError: generator already executing" condition.
    pub running: Cell<bool>,
    /// `True` once the generator has exhausted (returned, raised,
    /// or been closed). Further `send` raises StopIteration.
    pub closed: Cell<bool>,
}
```

`Object::Generator(Rc<PyGenerator>)` joins the object enum. Its
`type_name()` is `"generator"`; identity is `Rc::ptr_eq`; iteration
goes through `__next__` (the existing `make_iter` path picks this up
via a new `Object::Generator` arm).

### `iter()` / `next()` builtins

`iter(gen)` returns the generator itself (its `__iter__` returns
`self`, like CPython). `next(gen)` calls `gen.send(None)`. A new
two-arg `next(gen, default)` returns the default on `StopIteration`.

### Generator expressions executed lazily

The slice currently compiles `(x*x for x in xs)` as a list-comprehension
body that returns a list. After this RFC, the inner code object's
shape is:

```
RESUME 0
RETURN_GENERATOR    ; suspends; caller gets a generator
RESUME 0            ; (re-entered when send/next is called)
LOAD_FAST  .0       ; outer iterator
loop:
FOR_ITER  end_loop
... bind target ...
... apply filters ...
LOAD_FAST  <elt>
YIELD_VALUE
POP_TOP             ; discard the sent value
JUMP_BACKWARD loop
end_loop:
LOAD_CONST None
RETURN_VALUE
```

We reuse `compile_comprehension` with a `CompKind::Generator` branch
that emits this shape rather than the list-building shape.

### Errors

| Source | Error |
|--------|-------|
| `yield` at module scope | `SyntaxError` "yield outside function" |
| `yield from` of a non-iterable | `TypeError` at runtime |
| `send` to a freshly-created generator with non-`None` | `TypeError` |
| `next()` on an exhausted generator | `StopIteration` (already supported) |

### Thread safety

Out of scope. The slice is single-threaded; the `gi_running` guard
is the only related concern and it's purely intra-thread.

## Drawbacks

- **The `Frame` type changes shape**, which touches every opcode
  handler. We bound the blast radius by introducing the new
  `StepOutcome::Yield` variant and keeping the dispatch function
  loop-shaped — only `YIELD_VALUE` and `RETURN_GENERATOR` short-
  circuit out.
- **Generators retain their entire frame** including locals and
  closure cells. That's expected (CPython does the same) but worth
  noting because it means a stalled generator pins its inputs.
- **No `GeneratorExit` finally cleanup yet.** A `.close()` call
  raises `GeneratorExit` inside the generator, but if the generator
  catches and ignores it we don't currently force-terminate. Tracked
  but not blocking.

## Alternatives

- **CPS-transform generators at compile time** (à la Cheney-on-MTA
  or Stackless Python). Faster but trades complexity. Rejected for
  this RFC; reconsider if the suspendable-frame model becomes a
  perf hot spot.
- **Skip `yield from` in this RFC.** Considered: the lowering is
  small and is heavily used by stdlib code (itertools, contextlib,
  asyncio). Including it.

## Prior art

- **CPython 3.13** — the reference. Our frame-suspend model is a
  near-clone of `genobject.c`.
- **RustPython** — implements generators on a re-entrant interpreter
  loop; the strategy we use.
- **PyPy** — generators via a transformed interpreter; outside the
  scope of WeavePy's architecture today.

## Unresolved questions

- Whether `Generator` should expose `gi_frame` / `gi_code` /
  `gi_running` attributes for introspection now or in a follow-up.
  We expose names but treat them as opaque for now.

## Future work

- **RFC 0006-B**: `async def`, `await`, `async for`, `async with`,
  minimal `asyncio` event loop.
- **RFC 0006-C**: asynchronous generators (`async def f(): yield`).
- **RFC 0011**: bytecode compaction may revisit `SEND` to use the
  16-bit oparg form once the encoding lands.

# RFC 0016: Concurrency and asynchrony

- **Status**: Accepted
- **Authors**: WeavePy authors
- **Created**: 2026-05-21
- **Tracking issue**: TBD

## Summary

Close the gap between "the object model and stdlib work" (post RFC
0014 + 0015) and "modern Python — the async/await half of the
ecosystem — actually runs." After this RFC lands:

- The language gains the full `async` / `await` surface: `async
  def`, `await expr`, `async for`, `async with`, and `async`
  comprehensions all parse, compile, and execute. The lexer's
  `Async` / `Await` keyword tokens (reserved since the lexer first
  shipped) finally have semantics behind them.
- The runtime gains two new core object kinds: `Object::Coroutine`
  (the result of calling an `async def`) and `Object::AsyncGenerator`
  (the result of calling an `async def` that contains `yield`).
  Both implement the awaitable / async-iterable protocols and
  share the suspended-frame infrastructure that powers generators.
- The compiler gains the canonical bytecode shape CPython 3.13
  uses for await: `GET_AWAITABLE`, `SEND`, `END_SEND`,
  `YIELD_VALUE`, plus `GET_AITER` / `GET_ANEXT` / `END_ASYNC_FOR`
  for `async for` and `BEFORE_ASYNC_WITH` for `async with`.
- A new `StopAsyncIteration` built-in exception type joins the
  hierarchy. The async-for opcode loop catches it the same way
  the regular-for loop catches `StopIteration`.
- A working `asyncio` is shipped as a frozen Python module on top
  of the runtime: an event loop with `sleep` / `gather` / `wait` /
  `wait_for` / `run` / `run_until_complete` / `create_task` /
  `current_task`, `Task` and `Future` (both promise-style), and
  the `Lock` / `Event` / `Semaphore` / `Queue` primitives.
- A pragmatic cooperative `threading` is shipped on top of the
  asyncio loop. `Thread`, `Lock`, `RLock`, `Event`, `Condition`,
  `Semaphore`, `BoundedSemaphore`, `current_thread`,
  `main_thread`, `active_count`, `local` all work for cooperative
  workloads. CPU-bound parallelism is explicitly deferred — see
  "Drawbacks" below.
- A frozen `queue` (`Queue` / `LifoQueue` / `PriorityQueue` /
  `Empty` / `Full`) shares the cooperative model.
- A frozen `concurrent.futures` (`Future`, `Executor`,
  `ThreadPoolExecutor`) bridges the cooperative threading model
  into the futures API used by `asyncio.wrap_future` and a lot of
  third-party glue code.
- A Rust `_thread` shim provides the low-level primitives
  `threading` depends on (`allocate_lock`, `get_ident`,
  `start_new_thread`).

The combination is what the project calls "Option 1" in the
roadmap: drop in an `async def` function, an `asyncio.run(...)`,
or a `threading.Thread(target=fn).start()` and have it work.

## Motivation

After RFC 0015 the object model was effectively complete: any
non-trivial pure-Python library that *only* used classes,
exceptions, descriptors, metaclasses, dataclasses, enums, ABCs,
and typing now ran. The next cliff was *concurrency*. Modern
Python is overwhelmingly async-flavoured:

- The web framework story is async-first. FastAPI, Starlette,
  Litestar, Quart, BlackSheep — all `async def`. The
  synchronous frameworks (Flask, Django) are increasingly
  async-aware internally (`async def` view functions, ASGI).
- The HTTP client story is async-first. `httpx` (async +
  sync), `aiohttp`, `anyio`, `trio`. Even the venerable
  `requests` is in maintenance mode.
- The database story is async-first. SQLAlchemy 2.x's async
  session, `asyncpg`, `aiomysql`, `motor`, `aioredis`, every
  modern ORM.
- The test framework story is async-aware. `pytest-asyncio`,
  `unittest.IsolatedAsyncioTestCase`, `anyio`'s test backend.
- The stdlib itself uses async for `subprocess.asyncio`, the
  asyncio-based `subprocess`, `asyncio.streams`, etc.

WeavePy's parser before this RFC raised `SyntaxError: 'async'
is not implemented in the slice (RFC 0006-B)`. Every
`async def` was a non-starter. That's the single largest
"modern Python won't run" cliff the project had.

Down-tree, this RFC unblocks:

- The next-tier networking RFC (sockets, ssl, http,
  subprocess) is dramatically simpler when the event loop
  exists — `asyncio.open_connection`, `asyncio.subprocess`,
  `asyncio.create_subprocess_shell` all sit on top of it.
- `unittest.IsolatedAsyncioTestCase`, blocked entirely today.
- The conformance harness's Stage B can include any
  `Lib/test/test_asyncio_*.py` file the moment we have an
  event loop.
- Anything that uses `@functools.wraps` on an async function
  (now works, because `async def` produces a real
  function-object-with-attrs).

## CPython reference

This RFC tracks **CPython 3.13**:

- **PEP 492** — *Coroutines with async and await syntax*. The
  defining PEP for the surface syntax.
- **PEP 525** — *Asynchronous generators*. `async def` with
  `yield` produces an asynchronous generator, consumable by
  `async for`.
- **PEP 530** — *Asynchronous comprehensions*. `[x async for x in
  it]` inside an `async def`.
- **PEP 567** — *Context Variables*. `contextvars.ContextVar`. We
  ship the minimal surface (`ContextVar`, `Context`, `copy_context`)
  to support `asyncio` task-local state.
- **PEP 3156 / asyncio** — design document for the event loop.
  The public API surface lives at `Lib/asyncio/`; the canonical
  reference is `Lib/asyncio/base_events.py` and
  `Lib/asyncio/tasks.py`.
- **`threading`** — `Lib/threading.py` is the user-facing API;
  the C accelerator is in `Modules/_threadmodule.c`. We follow
  the high-level surface (`Thread`, `Lock`, `RLock`, `Event`,
  `Condition`, `Semaphore`, `BoundedSemaphore`, `local`,
  `current_thread`, `main_thread`, `active_count`) and the
  low-level `_thread` surface (`allocate_lock`, `get_ident`,
  `start_new_thread`).
- **`queue`** — `Lib/queue.py`. `Queue`, `LifoQueue`,
  `PriorityQueue`, `SimpleQueue`, `Empty`, `Full`.
- **`concurrent.futures`** — `Lib/concurrent/futures/__init__.py`,
  `_base.py`, `thread.py`. `Future`, `Executor`,
  `ThreadPoolExecutor`.

Bytecode-level reference: CPython 3.13's `Python/compile.c`
(search for `compiler_async_*`) plus the `dis` module
documentation for `GET_AWAITABLE`, `SEND`, `END_SEND`,
`GET_AITER`, `GET_ANEXT`, `END_ASYNC_FOR`, `BEFORE_ASYNC_WITH`.

We deliberately do **not** track in this RFC:

- **Real OS-thread parallelism.** `_thread.start_new_thread`
  schedules on the asyncio loop instead of spawning an OS
  thread. CPython's GIL serializes Python bytecode anyway, so
  the observable difference for a typical workload is small,
  but CPU-bound `threading.Thread` users see no speedup. Lifting
  this requires refactoring `Object` to use `Arc<…>` /
  `Arc<Mutex<…>>` instead of `Rc<…>` / `Rc<RefCell<…>>`; that
  refactor is its own RFC.
- **`asyncio` I/O multiplexing.** Until WeavePy ships real
  sockets and pipes, the event loop's `selectors`-based
  readiness wait is a no-op. `asyncio.open_connection` and
  friends will land in the next RFC alongside the `socket`
  module.
- **`asyncio.subprocess`**, **`asyncio.streams`**,
  **`asyncio.SSL`** — same blocker.
- **`signal.set_wakeup_fd`**, signal-driven event loops, and
  Ctrl-C handling integrated with the loop. The loop today
  exits cleanly on completion or `Task.cancel()`; signal
  integration follows the `signal` module.
- **`trio`**, **`anyio`'s trio backend**, **`curio`**. These
  expect specific scheduler invariants we don't promise.
- **`asyncio.add_reader`** / **`asyncio.add_writer`** — same
  no-OS-fd blocker as the I/O multiplexer.
- **`gather(return_exceptions=False)` cancellation propagation
  through arbitrary task graphs**. We honour `return_exceptions`
  and basic propagation; the deep CPython invariants around
  "uncancel" and parent-task cancellation messages are not
  faithfully replicated.

## Detailed design

### Lexer

No changes. The `async` and `Await` keyword tokens have been
reserved since the lexer first landed.

### Parser

Five new constructs land:

1. **`async def` function** is parsed by `parse_async_compound`
   when an `async` keyword token introduces a `def`. The result
   is a new `StmtKind::AsyncFunctionDef` carrying the same
   payload as `FunctionDef`.
2. **`async for`** is allowed only inside the body of an
   `async def`; the parser doesn't enforce that — the compiler
   does. The AST node is `StmtKind::AsyncFor`.
3. **`async with`** mirrors `with`: `StmtKind::AsyncWith`.
4. **`await expr`** parses with the precedence of a unary
   operator (between unary and the rest of the prefix operators,
   matching CPython). The AST node is `ExprKind::Await(Box<Expr>)`.
5. **`async for` clauses inside comprehensions** set the
   `Comprehension.is_async` flag (already present from earlier
   RFCs but unused until now).

The two parser entry points that the new constructs hook into are
`parse_simple_stmt` (which can see `async ...`) and
`parse_unary_expr` (which can see `await ...`). Both check the
upcoming token and route to the appropriate branch.

### AST

```rust
pub enum StmtKind {
    // ... existing variants ...
    AsyncFunctionDef {
        name: String,
        args: Arguments,
        body: Vec<Stmt>,
        decorator_list: Vec<Expr>,
    },
    AsyncFor {
        target: Expr,
        iter: Expr,
        body: Vec<Stmt>,
        orelse: Vec<Stmt>,
    },
    AsyncWith {
        items: Vec<WithItem>,
        body: Vec<Stmt>,
    },
}

pub enum ExprKind {
    // ... existing variants ...
    Await(Box<Expr>),
}
```

`Comprehension.is_async: bool` already exists from RFC 0005's
infrastructure but had no producer until now.

### Compiler

Four new opcodes, all modelled directly on CPython 3.13:

```rust
pub enum OpCode {
    // ... existing opcodes ...
    GetAwaitable,     // TOS = TOS.__await__()
    Send,             // (sub-TOS = iter, TOS = sent_value) -> TOS = yielded
                      //   StopIteration: pop both, push value, jump arg
    EndSend,          // pop second-from-top (iterator), keep TOS (result)
    GetAiter,         // TOS = TOS.__aiter__()
    GetAnext,         // peek aiter, push aiter.__anext__()
    EndAsyncFor,      // pop aiter and exception after StopAsyncIteration
    BeforeAsyncWith,  // TOS = cm; push __aexit__, push __aenter__()
}
```

`async def` is compiled like a generator function (with
`RETURN_GENERATOR` at entry, and `YIELD_VALUE` for any
`yield`) — the only difference is a single bit on the code
object's flags (`CO_COROUTINE` or `CO_ASYNC_GENERATOR`) which
the VM consults when constructing the resulting object.

`await expr` lowers to:

```text
    <expr>
    GET_AWAITABLE
    LOAD_CONST None
loop:
    SEND end
    YIELD_VALUE
    JUMP loop
end:
    END_SEND       # discard iterator, keep result
```

`async for target in expr: body` lowers to:

```text
    <expr>
    GET_AITER          # stack: [aiter]
loop:
    GET_ANEXT          # stack: [aiter, awaitable]
    GET_AWAITABLE      # stack: [aiter, __await__]
    LOAD_CONST None
inner:
    SEND end_inner
    YIELD_VALUE
    JUMP inner
end_inner:
    END_SEND           # stack: [aiter, value]
    STORE_FAST target
    <body>
    JUMP loop
caught_StopAsyncIteration:
    END_ASYNC_FOR      # pop aiter + exc
```

`async with cm as v: body` lowers to:

```text
    <cm>
    BEFORE_ASYNC_WITH  # stack: [__aexit__, awaitable(__aenter__)]
    GET_AWAITABLE
    LOAD_CONST None
loop1:
    SEND end1
    YIELD_VALUE
    JUMP loop1
end1:
    END_SEND            # stack: [__aexit__, v]
    STORE_FAST v
    <body>
    LOAD_CONST None
    LOAD_CONST None
    LOAD_CONST None
    PUSH_NULL            # call __aexit__(None, None, None) — normal exit
    CALL 3
    GET_AWAITABLE
    LOAD_CONST None
loop2:
    SEND end2
    YIELD_VALUE
    JUMP loop2
end2:
    END_SEND
    POP_TOP
```

(The exceptional exit path mirrors the synchronous `with`'s
`WITH_EXCEPT_START` shape with the awaitable indirection wrapped
around it.)

Async comprehensions (`[x async for x in it]`) compile inside a
synthetic `async def` whose body iterates with `async for`. The
*containing* function (caller of the comprehension) `await`s the
synthetic coroutine — which is why async comprehensions are only
legal inside an `async def` in the first place.

### VM

Two new object kinds:

```rust
pub enum Object {
    // ... existing variants ...
    Coroutine(Rc<PyCoroutine>),
    AsyncGenerator(Rc<PyAsyncGenerator>),
}

pub struct PyCoroutine {
    pub frame: RefCell<Option<Frame>>,
    pub state: Cell<GeneratorState>,
    pub name: String,
    pub qualname: String,
}

pub struct PyAsyncGenerator {
    pub frame: RefCell<Option<Frame>>,
    pub state: Cell<GeneratorState>,
    pub name: String,
}
```

A coroutine is fundamentally a generator with a different *type*
and a different exception (`StopIteration` becomes the
coroutine's "I'm done" signal, same as for generators). The
runtime reuses `PyGenerator`'s suspension machinery wholesale —
`run_until_yield_or_return` is shared.

Awaitable protocol: an object is **awaitable** if it is a
`Coroutine`, a generator with the `CO_ITERABLE_COROUTINE` flag,
or has a `__await__` method that returns an iterator. The
`GetAwaitable` opcode consults this list in order; failure raises
`TypeError: object X can't be used in 'await' expression`.

Async iterable: `Object::AsyncGenerator` directly. Any other
object's `__aiter__` is called by `GetAiter`; it must return an
async iterator.

Async iterator: an object whose `__anext__` returns an awaitable.
`GetAnext` calls `__anext__`; the resulting awaitable is consumed
by the surrounding `await`-shaped opcode sequence.

`Vm::send_coroutine` and `Vm::send_async_generator` are thin
shims over `Vm::send_generator` that pass the right "what to
raise on completion" sentinel (`StopIteration` for coroutines and
`StopAsyncIteration` for async generators, with the return value
in the `value` attribute).

### Built-in types

Two new exception types join the registry:

```text
BaseException
└── Exception
    ├── StopIteration
    └── StopAsyncIteration          (new)
```

(Note: `StopAsyncIteration` is a sibling of `StopIteration` in
CPython 3.13, not a subclass.)

Three new built-in types are exposed in the global namespace:

- `coroutine` — the class of `Object::Coroutine`. Used by
  `isinstance(x, types.CoroutineType)` and `inspect.iscoroutine`.
- `async_generator` — the class of `Object::AsyncGenerator`. Used
  by `isinstance(x, types.AsyncGeneratorType)` and
  `inspect.isasyncgen`.
- The existing `generator` class gets a `CO_ITERABLE_COROUTINE`
  flag accessor (used by `asyncio.iscoroutine`'s fallback path).

### Frozen `asyncio` (~750 LOC)

The frozen module ships a complete-enough event loop to drive
real async code:

- `BaseEventLoop` with a heap of `(deadline, callback)` pairs
  and a FIFO ready queue. `run_forever` / `run_until_complete`
  drain the queues until either the future is done or the loop
  is `stop()`ped. `_select` is a no-op for now — there are no
  file descriptors to watch.
- `new_event_loop`, `get_event_loop`, `get_running_loop`,
  `set_event_loop`, `_get_running_loop`. The module keeps the
  loop in a module-level thread-local-ish slot (we have one
  thread so it's just a module global).
- `Future` — a state machine with `result`, `exception`,
  `set_result`, `set_exception`, `done`, `cancelled`,
  `cancel`, `add_done_callback`, `remove_done_callback`,
  `__await__`. Yields itself once when not done, returns the
  result when done.
- `Task(coro)` — a `Future` that drives a coroutine. The
  loop's step-Task callback resumes the coroutine, handles
  what it yields (must be a `Future` or another awaitable),
  and reschedules itself on the awaited future's
  done-callback.
- `sleep(delay, result=None)` — returns a `Future` whose
  done-callback is scheduled via `loop.call_later(delay, ...)`.
  `sleep(0)` schedules via `call_soon` and yields once so the
  loop can run other tasks.
- `gather(*aws, return_exceptions=False)` — returns a `Future`
  that resolves to the list of results when all awaitables
  complete (or eagerly rejects on the first exception when
  `return_exceptions=False`).
- `wait(aws, timeout=None, return_when=ALL_COMPLETED)` —
  splits awaitables into `(done, pending)` sets.
- `wait_for(coro, timeout)` — like `wait` but for a single
  awaitable; raises `TimeoutError` on expiry.
- `run(coro, *, debug=None)` — boots a fresh loop, runs the
  coroutine to completion, returns its result, closes the loop.
- `create_task(coro, *, name=None)`, `current_task`,
  `all_tasks`.
- `Lock`, `Event`, `Semaphore`, `BoundedSemaphore`,
  `Condition`. All implemented as small Python classes with
  `__aenter__` / `__aexit__` (for `Lock`, `Semaphore`) and the
  obvious `acquire` / `release` / `wait` / `set` / `clear`
  surface.
- `Queue`, `LifoQueue`, `PriorityQueue` — async-flavoured
  versions of the synchronous `queue` module's classes.
- `iscoroutine`, `iscoroutinefunction`, `ensure_future`,
  `wrap_future`, `as_completed`, `to_thread`.
- `CancelledError(BaseException)` — coroutine cancellation
  signal. Inherits from `BaseException` (not `Exception`) to
  match CPython's "should not be swallowed by `except
  Exception`" invariant.

### Frozen `threading` (~280 LOC) + Rust `_thread` shim (~120 LOC)

The cooperative scheduler runs all "threads" on the asyncio loop.
`_thread.start_new_thread(fn, args)` does roughly:

```python
loop = asyncio.get_event_loop()  # auto-creates if missing
async def _runner():
    fn(*args)
loop.create_task(_runner())
```

`threading.Thread.start()` wraps the same call with the
high-level `Thread` semantics (name, daemon flag, join via
awaiting an `asyncio.Event`).

`_thread` exposes (Rust):

- `allocate_lock()` → `_thread.lock` (an `asyncio.Lock`-flavoured
  object)
- `get_ident()` → the current task's id (or `1` outside a task)
- `start_new_thread(fn, args, kwargs=None)` → schedules a task
- `_count()` → number of live "threads"
- `error` → alias for `RuntimeError`
- `LockType` → the lock type

`threading` wraps `_thread` with the `Thread` class plus all the
high-level primitives: `Lock`, `RLock`, `Event`, `Condition`,
`Semaphore`, `BoundedSemaphore`, `local`, `current_thread`,
`main_thread`, `active_count`, `enumerate`. The `RLock` recursive
counter is a per-task counter keyed on `_thread.get_ident()`.

### Frozen `queue` (~180 LOC)

`Queue(maxsize=0)`, `LifoQueue`, `PriorityQueue`. Each uses an
internal `_unfinished_tasks` counter for `task_done()` /
`join()`. Backed by `collections.deque` (already shipped). The
`get(block, timeout)` / `put(block, timeout)` paths use
`threading.Condition`, which under the cooperative model is an
`asyncio.Condition`-flavoured wrapper that yields to the loop on
wait.

### Frozen `concurrent.futures` (~220 LOC)

`Future` — independent of `asyncio.Future` (different lineage,
slightly different API). `result`, `exception`, `cancel`,
`done`, `add_done_callback`, `set_result`, `set_exception`,
`set_running_or_notify_cancel`.

`Executor` — abstract base. `submit`, `map`, `shutdown`.

`ThreadPoolExecutor(max_workers=None)` — under the cooperative
model, "submit" creates an asyncio task that resolves the
future. `as_completed` and `wait` mirror the asyncio versions.

### Built-in `__build_class__` and `__build_async_class__`

No new build-class — `async def` is compiled into the same code
shape as `def`, just with a flag bit set. The flag is consulted
by `Vm::make_function` to decide which `Object` variant to
construct on call.

## Drawbacks

- **Cooperative-only threading.** `threading.Thread` doesn't
  actually run in parallel; it's an asyncio task with a `Thread`
  wrapper. CPU-bound code that uses `threading` for parallelism
  sees no speedup. This is honest about WeavePy's stage (the
  alternative is lying about parallelism), and CPython's own
  GIL serializes Python bytecode anyway, so the observable
  difference for I/O-bound or stdlib-only workloads is small.
  Lifting this requires the `Rc → Arc` refactor mentioned in
  "Future work".
- **No I/O multiplexing in the loop.** `asyncio.sleep` and
  `Future`-based suspensions work; `asyncio.open_connection`
  doesn't because there are no sockets to multiplex yet. The
  loop's `_select` step is a no-op. This lands when the
  socket module does.
- **`asyncio.subprocess` isn't shipped.** It depends on
  pipes-as-streams, which depends on real fd integration.
- **`asyncio.Task.uncancel`** and the deep cancellation
  invariants from PEP 657 aren't faithfully replicated. The
  surface (`cancel`, `cancelled`, `CancelledError`) works for
  the common case.
- **`threading.local`** is fake-local — there's only one OS
  thread. Reads and writes hit a single dict. This is
  observable if user code spawns "threads", expects local
  state isolation, and *also* depends on the threads running
  truly in parallel. (Practically: nobody does.)
- **`PyAsyncGenerator.aclose` / `athrow`** ship a minimal
  implementation. CPython's full async-generator finalisation
  semantics (`asend` / `athrow` interleaving, the
  `__aiter__` / `__anext__` re-entrancy rules) are
  approximate.
- **Bytecode shape divergence.** Our `await` lowering uses
  fewer opcodes than CPython 3.13 (we collapse some of
  CPython's `RESUME` / `END_SEND` / `POP_TOP` choreography).
  The conformance harness's `dis` phase will report a
  mismatch on most async fixtures; the observable behaviour
  matches.

## Alternatives

- **Real OS threads with `Arc`-based `Object`.** The "correct"
  long-term answer. Rejected for this slice because it touches
  every file in the VM and most files in the stdlib, and we
  want async/await *first* — most modern Python is async, not
  threaded.
- **Skip threading entirely.** Tempting (we only have an event
  loop). Rejected because `concurrent.futures.ThreadPoolExecutor`
  is the standard way to bridge sync code into asyncio
  (`loop.run_in_executor`, `asyncio.to_thread`), and many
  unrelated stdlib modules (`logging`'s `QueueHandler`,
  `unittest`'s parallel runner) import `threading`
  unconditionally.
- **Implement `asyncio` in Rust.** Faster but couples WeavePy
  to a specific scheduler design and makes user-side patching
  (a common pattern with `uvloop`-style alternatives)
  impossible. CPython ships asyncio in Python; we do the same.
- **Compile `await` to a single opcode that drives the
  awaitable to completion.** Smaller bytecode but wrong: the
  coroutine has to *yield* between awaits so the scheduler can
  interleave. Our compilation matches CPython's at the shape
  level.

## Prior art

- **CPython 3.13** — the conformance target.
  `Lib/asyncio/` is the canonical reference for the event loop
  surface; `Python/compile.c` for the bytecode shape.
- **MicroPython** — ships `uasyncio` (a minimal asyncio
  compatible with the CPython surface) on an event loop
  designed for embedded targets. Similar philosophy to our
  cooperative-only model. We borrow nothing directly but the
  approach is validating.
- **Trio / curio** — alternative event-loop designs (structured
  concurrency, cancel scopes). Out of scope for our slice but
  worth noting that *not* tracking them is deliberate: the
  stdlib's `asyncio` is what 99% of the ecosystem uses.
- **RustPython** — implements coroutines and asyncio with a
  similar shape (Python event loop, Rust object model). Our
  scheduler is simpler than theirs because we don't yet
  multiplex I/O.

## Unresolved questions

- **`gather` cancellation semantics under partial completion.**
  CPython 3.13's behaviour is subtle when one of the futures
  inside a `gather` is cancelled; the parent's cancellation
  message propagates differently depending on `return_exceptions`.
  We honour the common case; the corner cases are documented
  but not exercised.
- **`__del__` on coroutines that were never awaited.** CPython
  emits a `RuntimeWarning: coroutine 'X' was never awaited`. We
  don't yet have weakref finalisation hooks, so this warning
  is silent. Tracked.
- **`Task.__step` re-entrancy under deeply nested
  `await loop.run_in_executor(...)`.** Works for the typical
  case; the deeply-nested re-entrancy paths in CPython's
  scheduler aren't fully ported.
- **Selectors / `loop.add_reader` / `loop.add_writer`.** The
  hooks exist in the loop but raise `NotImplementedError`
  until sockets ship.

## Future work

- **Real OS threads via `Arc<…>` `Object`.** The largest
  remaining concurrency gap. Unlocks `multiprocessing.Pool`
  (which can sit on top of cooperative threads with one extra
  layer), CPU-bound parallelism, and `concurrent.futures.\
  ProcessPoolExecutor`.
- **`socket` + `ssl`** so the event loop has something to
  poll. Unblocks `asyncio.open_connection`,
  `asyncio.start_server`, real network I/O.
- **`asyncio.subprocess` + pipes** for shell-out from async
  code.
- **`contextvars`** beyond the minimal surface — full
  `Context.run`, decimal-style context inheritance.
- **`uvloop`-style alternative event loop** as an installable
  package (proves the event-loop swappability story).
- **`PEP 654` exception groups** (`ExceptionGroup`,
  `except*`) — currently parsed as syntax error;
  `asyncio.TaskGroup` would benefit.
- **`asyncio.Runner`** API (CPython 3.11+) — slightly
  different shape from `asyncio.run`; nice-to-have.
- **`async for` over Rust-backed async iterators** (e.g. a
  Rust-implemented `aiofiles`-style file iterator) — needs
  the awaitable protocol to extend to native callables.

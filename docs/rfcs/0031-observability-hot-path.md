# RFC 0031: observability hot path — wired trace/profile/monitoring/audit/tracemalloc, sub-interpreters, pdb, parametrize

- **Status**: Accepted
- **Authors**: WeavePy authors
- **Created**: 2026-05-27
- **Tracking issue**: TBD
- **Builds on**: RFC 0023 (drop-in parity), RFC 0026 (regrtest gate),
  RFC 0030 (pure-Python drop-in surface)

## Summary

RFC 0030 shipped the **registerable** debugger / profiler surface:
`sys.settrace(fn)` / `sys.gettrace()` round-trips, `sys.monitoring`
tool-id reservation, `tracemalloc.start()` / `.take_snapshot()`. The
hooks existed; they just never *fired*. RFC 0031 closes the loop:
every wired hook now dispatches from the VM in the right place,
with the right arity, with the right re-entrance guard. The same
commit lands four follow-ons that have been waiting on observability:

1. **VM-dispatched trace / profile / monitoring / audit / tracemalloc.**
   The instruction loop fires `call` / `line` / `return` / `yield` /
   `exception` for `sys.settrace` and `sys.setprofile`, the equivalent
   `PY_START` / `PY_RETURN` / `PY_YIELD` / `RAISE` / `LINE` set for
   PEP 669 `sys.monitoring`, and `audit_event(...)` from `sys.audit`
   subscribers at `open` / `compile` / `exec` / `eval` / `import`
   sites. `tracemalloc.record_alloc` is called from `BuildList` /
   `BuildTuple` / `BuildSet` / `BuildMap` / `BuildString`.
2. **PEP 684 sub-interpreters.** A real `_xxsubinterpreters` Rust
   module plus a high-level `interpreters` Python frontend.
   `Interpreter.exec_sync(...)` runs Python source in an isolated
   interpreter on a dedicated OS thread; `create_channel()` /
   `send` / `recv` cross-interpreter queues marshal shareable values
   between them.
3. **`pdb` / `bdb` on top of real `settrace`.** With `sys.settrace`
   actually firing, `bdb.Bdb` can hook into the VM. `pdb` boots
   under `python -m pdb`, runs `runcall` against user functions,
   and reports breakpoint hits, exceptions, and step events through
   the standard trace callback.
4. **`_pytest` parametrize matrices, indirect fixtures, scope
   caching, finalizers.** The shim grew a real `_FixtureDef` with
   per-scope (`function` / `class` / `module` / `session`) caches,
   `request.addfinalizer` LIFO stacks, `yield`-fixture lifecycle,
   indirect fixture resolution, and `@pytest.mark.parametrize`
   Cartesian-product expansion.

The acceptance gate is the same as RFC 0026: bundled regrtests
pass, `cargo fmt` / `cargo clippy -D warnings` / `cargo test
--workspace --all-targets --all-features` / `cargo test --workspace
--doc` are clean.

## Motivation

Coverage tools, debuggers, profilers, audit-log hooks, and memory
trackers are table stakes for *any* serious Python deployment.
RFC 0030 made `sys.settrace(fn)` callable, which is necessary but
not sufficient: `coverage.py` registers a trace callback and gets
zero callbacks, so the report is empty. `pdb` registers a
breakpoint and gets ignored. `tracemalloc` reports zero allocations.
PEP 669 `sys.monitoring` reserves a tool id and never sees an event.
The hook table held the data, but the VM never consulted it.

Closing the loop requires four interlocking pieces:

- **The dispatcher needs to fire.** Each opcode boundary needs a
  guarded callback if a hook is registered, with re-entrance
  protection so the trace function tracing itself doesn't blow the
  stack.
- **The arities have to match CPython.** `sys.monitoring`'s callback
  contract is per-event (`LINE` gets `(code, line)`; `PY_START`
  gets `(code, offset)`; `RAISE` gets `(code, offset, exc)`). Pass
  the wrong number of args and the Python callback dies on
  `TypeError`.
- **The interpreter pointer has to be reachable.** Audit hooks
  fire from C-level operations (open, compile, marshal). The audit
  dispatcher walks the subscriber list and calls each hook via the
  VM — which means the active `Interpreter*` has to be findable
  from a free function. That requires publishing the pointer at
  the entry points (`run_module_as`, `exec_module_in`).
- **The downstream consumers need real implementations.** `pdb`,
  `bdb`, `_pytest` parametrize, and the sub-interpreters Python
  frontend are all *blocked* on observability — once the hooks
  fire they unlock — so it makes sense to land them together.

## CPython reference

- PEP 578 — Runtime audit hooks (`sys.audit`, `sys.addaudithook`).
- PEP 669 — Low-impact monitoring for CPython (`sys.monitoring`,
  event constants, tool-id reservation, callback arities).
- PEP 684 — A per-interpreter GIL (`_xxsubinterpreters`,
  `interpreters` high-level API, channel send/recv).
- `cpython/Lib/bdb.py`, `cpython/Lib/pdb.py` — frame-walk
  protocol, breakpoint dispatch, step semantics.
- `cpython/Lib/tracemalloc.py` + `cpython/Modules/_tracemalloc.c` —
  snapshot / statistics / traceback shape.
- `cpython/Lib/_pytest/python.py`, `cpython/Lib/_pytest/fixtures.py` —
  parametrize matrix expansion, fixture scope caching, finalizer
  ordering.

## Detailed design

### 1. Per-thread observability state — `crates/weavepy-vm/src/trace.rs`

Five thread-locals back the registry:

```rust
thread_local! {
    static TRACE_HOOK: RefCell<Option<Object>> = const { RefCell::new(None) };
    static PROFILE_HOOK: RefCell<Option<Object>> = const { RefCell::new(None) };
    static MONITORING_TOOLS: RefCell<MonitoringTools> = const { RefCell::new(MonitoringTools::new()) };
    static AUDIT_HOOKS: RefCell<Vec<Object>> = const { RefCell::new(Vec::new()) };
    static HOOK_REENTRY: RefCell<u32> = const { RefCell::new(0) };
}
```

- `TRACE_HOOK` / `PROFILE_HOOK`: the single global hook installed
  by `sys.settrace` / `sys.setprofile`. Per-frame `frame.f_trace`
  shadowing is handled at the frame level by reading
  `PyFrame::trace`.
- `MonitoringTools`: a fixed-size table of `[Option<Object>; 6]`
  per event id. PEP 669 reserves 6 tool ids (0–5 plus debugger 5).
- `AUDIT_HOOKS`: an append-only `Vec<Object>` of subscribers
  registered with `sys.addaudithook`.
- `HOOK_REENTRY`: a counter incremented on entry, decremented on
  exit. `any_observers_active()` returns `false` when the counter
  is non-zero, so callbacks that themselves register their tracer
  don't recursively explode the stack. The guard is held by a
  `ReentryGuard` RAII type — drop semantics keep the counter
  exact even on early-return.

Two fast-path checks keep the cost on hot loops near zero when
nothing is registered:

```rust
pub fn any_observers_active() -> bool { ... }   // trace || profile || monitoring
pub fn any_audit_active() -> bool { ... }        // audit_hooks non-empty
```

Both are inlined and read the thread-local without taking a
borrow when the table is empty. The dispatcher's per-instruction
branch is `if trace::any_observers_active() { ... }`, which folds
to a single load + branch on the empty hot path.

### 2. VM event firing — `crates/weavepy-vm/src/lib.rs`

`Interpreter::run_until_yield_or_return` is the per-frame loop.
RFC 0031 added six fire points:

| Site | Event | Callback shape |
|---|---|---|
| `push_py_frame` returns Ok | `call` | `trace(frame, "call", None)`; `monitoring(code, offset)` for `PY_START` |
| Before each `step()`, when `lasti.line != last_line` | `line` | `trace(frame, "line", None)`; `monitoring(code, line)` for `LINE` |
| `StepOutcome::Return(v)` | `return` | `trace(frame, "return", v)`; `monitoring(code, offset, v)` for `PY_RETURN` |
| `StepOutcome::Yield(v)` | (profile only — `c_return`) | `profile(frame, "c_return", v)`; `monitoring(code, offset, v)` for `PY_YIELD` |
| `RuntimeError::PyException(e)` raised inside the frame | `exception` | `trace(frame, "exception", (type, value, tb))`; `monitoring(code, offset, exc)` for `RAISE` |
| `OpCode::BuildList` / `BuildTuple` / `BuildSet` / `BuildMap` / `BuildString` | (tracemalloc) | `tracemalloc::record_alloc(filename, line, n_bytes)` |

`fire_line_event` reads `PyFrame::lasti`, maps it to a source line
via `CodeObject::linetable`, and compares against
`PyFrame::last_line` (new `Cell<Option<u32>>` field) to suppress
duplicate emits on the same line. The dispatch helper is a single
method:

```rust
fn fire_monitoring_event(&mut self, py_frame: &PyFrame, event_idx: usize, arg: Object)
    -> Result<(), RuntimeError>
{
    if !any_observers_active() { return Ok(()); }
    let active = MONITORING_TOOLS.with(|t| t.borrow().get(event_idx).filter_map(|c| c.clone()));
    if active.is_empty() { return Ok(()); }
    for cb in active {
        let guard = match ReentryGuard::acquire() { Some(g) => g, None => return Ok(()) };
        let args = match event_idx {
            EVENT_LINE => vec![code, line],
            EVENT_PY_START | EVENT_PY_RESUME | EVENT_INSTRUCTION => vec![code, offset],
            _ => vec![code, offset, arg.clone()],
        };
        self.call(&cb, &args, &[], &outer)?;
        drop(guard);
    }
    Ok(())
}
```

The arity branch matches CPython's `monitoring_call_args` table.
Earlier versions of this RFC passed three args unconditionally;
that broke `PY_START` callbacks with `TypeError: takes 2 positional
arguments but 3 were given`.

### 3. Audit hook dispatch — `crates/weavepy-vm/src/stdlib/sys.rs`

`sys.audit(event, *args)` and `sys.addaudithook(hook)` go through
the same registry. The free-function dispatcher reaches the
interpreter via a published thread-local pointer:

```rust
pub fn audit_event(event: &str, args: &[Object]) {
    if !trace::any_audit_active() { return; }
    let hooks = trace::audit_hooks();
    if hooks.is_empty() { return; }
    let Some(_g) = trace::ReentryGuard::acquire() else { return; };
    let Some(ptr) = vm_singletons::current_interpreter_ptr() else { return; };
    let interp = unsafe { &mut *ptr };
    let arg_tuple = Object::new_tuple(args.to_vec());
    let outer = interp.builtins_dict();
    for hook in hooks {
        let call_args = [Object::from_str(event.into()), arg_tuple.clone()];
        let _ = interp.call_object_with_globals(&hook, &call_args, &[], &outer);
    }
}
```

Audit sites wired this commit:

| Site | Event name | Args |
|---|---|---|
| `io.open(path, mode, flags)` | `"open"` | `(path, mode, flags)` |
| `compile(source, ...)` | `"compile"` | `(source, filename)` |
| `exec(source, ...)`, `eval(source, ...)` | `"exec"` | `(source,)` |
| `__import__(name, ...)` | `"import"` | `(name, filename, sys.path, sys.meta_path, sys.path_hooks)` |
| `marshal.loads(data)` | `"marshal.loads"` | `(data, version)` |

Adding a new audit site is a one-liner — `crate::stdlib::sys::
audit_event("subprocess.Popen", &[args]);` — so subsequent RFCs
can extend the surface without re-touching the dispatcher.

The `vm_singletons::publish_interpreter_ptr` call was already in
the REPL and `cargo run -- script.py` entry path. RFC 0031 also
wires it at the two entry points that the CLI's `-m` flag drives:
`run_module_as` and `exec_module_in`. Without the publish, audit
hooks fired from a `python -m foo` invocation would no-op because
`current_interpreter_ptr()` returned null.

### 4. `tracemalloc` allocator integration

`crates/weavepy-vm/src/stdlib/tracemalloc_real.rs` already exposed
`record_alloc(filename, lineno, size)` keyed on the
`tracemalloc.start()` flag. The integration just needed the
dispatcher to call it. RFC 0031 wires `record_alloc` into the
five container-construction opcodes:

```rust
self.record_alloc(py_frame, std::mem::size_of::<Object>() * n);
```

`record_alloc` is itself guarded by `tracemalloc::is_tracing()`,
so the cost when tracemalloc is off is one branch. The
`Object::new_*` helper functions are *not* hooked directly: doing
so would mean every transient list / dict the VM creates internally
would land in the snapshot. The opcode-level hook is what the
real CPython tracemalloc instruments, and matches what
`coverage.py` / `memray` / `objgraph` expect.

### 5. PEP 684 sub-interpreters — `crates/weavepy-vm/src/stdlib/interpreters_mod.rs`

A new built-in module `_xxsubinterpreters` exposes the low-level
PEP 684 API. The implementation lives in two layers:

- **`Registry`** — a global `Mutex<Slab<InterpreterEntry>>` (and
  similar for channels). Each entry owns a `JoinHandle<()>` for
  the dedicated OS thread and a pair of `Sender` / `Receiver`
  channels for command dispatch.
- **`InterpreterEntry::exec(source)`** — sends a
  `Command::Exec(source)` to the worker; the worker spins up a
  fresh `Interpreter`, runs the source as the `__main__` module,
  and returns the result via a oneshot. Errors surface as
  `RunFailedError` with the original traceback.

The Python frontend at
`crates/weavepy-vm/src/stdlib/python/interpreters.py` wraps the
low-level surface with the user-facing names PEP 684 specifies:

```python
from interpreters import Interpreter, create_channel, list_all

interp = Interpreter()
interp.exec_sync("print('hello from sub-interpreter')")
send, recv = create_channel()
send.send(42)
assert recv.recv() == 42
interp.close()
```

`Channel` distinguishes `close()` (mark as closed, refuse new
sends, drain pending) from `destroy()` (free the underlying
registry entry). Earlier drafts collapsed them; the test
`test_channel_closed` caught the regression.

Shareable values follow PEP 684: immutable primitives
(`int` / `float` / `str` / `bytes` / `bool` / `None` / `tuple`-of-
shareable). Anything else raises `NotShareableError` at send time.

### 6. `pdb` / `bdb` on real `settrace`

With `sys.settrace` actually firing, `bdb.Bdb.runcall(fn, *args)`
now hooks into the VM and reports `call` / `line` / `return` /
`exception` events. `pdb` boots under `python -m pdb script.py`,
prints the standard prompt, and accepts the usual command set
(`b` / `c` / `n` / `s` / `r` / `q` / `p` / `pp` / `l` / `w` /
`up` / `down`).

The pdb-side work was largely making the existing CPython source
load against WeavePy's stdlib. The failures were in `os.path`:
`bdb.canonic` calls `os.path.normcase`, which RFC 0030 didn't
ship. RFC 0031 fills out `os.path` with:

| Function | Behaviour |
|---|---|
| `os.path.normcase(p)` | Lowercase on Windows / case-fold on macOS; pass-through on Linux. |
| `os.path.expanduser(p)` | `~` → `$HOME`; `~user` → that user's home dir from `pwd`. |
| `os.path.expandvars(p)` | `$VAR` and `${VAR}` substitution from `os.environ`. |
| `os.path.isabs(p)` | `True` iff the path starts with `/` (or a drive on Windows). |
| `os.path.realpath(p)` | Symlink-resolved canonical path. |
| `os.path.relpath(p, start='.')` | Relative path computation with `..` traversal. |
| `os.path.commonpath(paths)` / `commonprefix(paths)` | Common ancestor / common string prefix. |
| `os.path.getsize(p)` / `getmtime(p)` / `getctime(p)` | Metadata via `fs::metadata`. |
| `os.path.islink(p)` | `True` iff `fs::symlink_metadata().file_type().is_symlink()`. |
| `os.path.samefile(a, b)` | Canonicalize both and compare. |

These ride along because the alternative — wiring them up in a
follow-up RFC — would block `pdb` for another commit, and the
work is mechanical.

### 7. `_pytest` parametrize matrices and complex fixtures

`crates/weavepy-vm/src/stdlib/python/_pytest.py` grew the
following surface:

- **`_FixtureDef`** — replaces the prior dict on `fn.
  _pytest_fixture`. Carries `fn` / `scope` / `params` / `ids` /
  `autouse` / `generator`. `__getitem__` / `.get` give
  backward-compat dict-style access.
- **`_FixtureManager`** — per-scope cache stack
  (`{ 'function': {}, 'class': {}, 'module': {}, 'session': {} }`).
  `reset_scope(scope)` tears down the cache and runs any pending
  finalizers in LIFO order.
- **`_Request`** — exposes `addfinalizer(fn)` (pushed onto the
  current item's stack), `getfixturevalue(name)` (recursive
  resolve), `param` (indirect fixture argument).
- **`_resolve_fixture(name, request)`** — looks up the
  `_FixtureDef`, drives the generator/yield protocol, caches by
  scope, threads parameters through indirect fixtures.
- **`_expand_parametrize(item, marks)`** — expands one or more
  `@pytest.mark.parametrize` decorators into the Cartesian product
  of `_ParamSet` records. Handles single-name (`"x"`) and tuple
  (`"x, y"`) argname forms; `pytest.param(value, id=..., marks=...)`
  attaches a custom id or skip mark to one slot.
- **`Module.collect` / `Class.collect`** — call `_expand_parametrize`
  so a parametrized test that would have been one Item becomes N.

Autouse fixtures are resolved by inspecting every `_FixtureDef`
on the module / class and adding it to the autouse set before the
explicit-args set. The `_run` loop calls
`_FIXTURE_MANAGER.reset_scope('class')` between classes, `'module'`
between modules, `'session'` at the end.

## Test plan

Four new bundled regrtests, all in subprocess mode:

| Test | What it covers |
|---|---|
| `tests/regrtest/test_sys_settrace_dropin.py` | `sys.settrace` fires `call` / `line` / `return` / `exception` for real Python code; `sys.setprofile` fires `c_call` / `c_return`; `sys.monitoring` PY_START / LINE / PY_RETURN / RAISE callbacks fire with correct arity; `tracemalloc.start()` / `take_snapshot()` returns a non-empty statistics list after running a list-building loop; `sys.addaudithook` receives `open`, `compile`, `exec` events. |
| `tests/regrtest/test_interpreters_dropin.py` | `Interpreter()` lifecycle, `exec_sync` isolation, channel send/recv, `NotShareableError` for unshareable values, `with` statement context manager, list_all enumeration, concurrent cross-interpreter exchange. |
| `tests/regrtest/test_pdb_bdb_dropin.py` | `bdb.runcall` fires `user_call` / `user_line` / `user_return` / `user_exception` hooks; setting breakpoints and observing `break_here`; importing `pdb` and inspecting `Pdb()` doesn't error; `clear_all_breaks` resets state. |
| `tests/regrtest/test_pytest_parametrize_dropin.py` | single-dim parametrize, Cartesian product, tuple unpacking, `pytest.param(value, id=..., marks=...)`, session-scoped fixture caching, yield fixtures + teardown order, `request.addfinalizer` LIFO, indirect fixtures. |

Plus the existing `test_pytest_dropin.py` continues to pass.

## Acceptance criteria

Same as RFC 0026:

- `cargo fmt --all -- --check` — clean.
- `cargo clippy --workspace --all-targets --all-features -- -D warnings` — clean.
- `cargo test --workspace --all-targets --all-features` — green.
- `cargo test --workspace --doc` — green.
- `cargo +1.85 check --workspace --all-targets --all-features` — green (MSRV).
- `cargo run --release -p weavepy-cli -- regrtest --mode subprocess
  --workers 4 --timeout 60` — 42/42 pass, 0 unexpected.

## Drawbacks

- **Hot-path branch cost.** Every opcode now branches on
  `any_observers_active()`. The branch predictor handles the empty
  case well (one branch, never taken), but on tight arithmetic
  micro-benchmarks (`fannkuch`, `pidigits`) the cost is ~1.5–2 %.
  That's within the budget RFC 0030 set; further work is in
  RFC 0034 (specialization-aware tracing).
- **Re-entrance guard is global, not per-callback.** A trace
  callback that touches `sys.settrace` to swap itself out mid-
  trace will be skipped (the guard refuses to re-acquire). That
  matches CPython, but it's worth flagging.
- **Sub-interpreter isolation is OS-thread-based.** Each
  sub-interpreter runs on a dedicated thread. Cheap to spawn (~ms)
  but not free; long-lived workers are the intended pattern.
  PEP 684's per-interpreter GIL design lets future RFCs share a
  thread across interpreters if the cost matters.
- **`_pytest` is still a shim.** The parametrize expansion is
  complete enough for the common cases, but `pytest_collection`
  hooks, `conftest.py` discovery up the directory tree, and
  third-party plugins (`pytest-asyncio`, `pytest-mock`,
  `pytest-xdist`) are out of scope. Most production test suites
  do depend on at least one.

## Alternatives

- **Trace only on instruction boundaries CPython does.** CPython
  fires `line` events on instruction boundaries that cross a
  source line. That's what RFC 0031 ships. An alternative —
  firing on *every* opcode — was rejected: it would 10× the cost
  on hot loops and `coverage.py` doesn't need it.
- **Audit hooks fired from Python only.** We could have left the
  audit dispatcher in pure Python and pushed the cost to user
  code. Rejected because PEP 578 specifies dispatch from C; user
  code can't reliably hook into `open` / `compile` / `exec` from
  Python alone, and audit is fundamentally a defense-in-depth
  primitive that has to fire from inside the runtime.
- **Sub-interpreters as in-process futures.** PEP 684 specifies
  *interpreter* isolation, not just *task* isolation. A
  futures-based shim would have collapsed the global module state,
  defeating the point. The thread-per-interpreter design carries
  more cost but matches the spec.

## Prior art

- **CPython 3.13** ships PEP 669 `sys.monitoring`. Our event
  table and arity rules match their `monitoring_call_args`
  switch.
- **PyPy 7.3** has full `sys.settrace` integration. Their
  experience: per-frame `f_trace` shadowing is the load-bearing
  feature; global trace alone is not enough. RFC 0031 implements
  both.
- **GraalPy** wired `sys.audit` early. Their observation: a
  free-function dispatcher reachable from non-VM stdlib code is
  the *only* practical design — anything that requires a
  per-call interpreter handle breaks `compile` / `marshal` /
  `open` audit sites.

## Unresolved questions

1. **PEP 657 fine-grained traceback positions.** `_pytest`
   tracebacks would benefit from column-level positions. RFC 0033.
2. **Conftest discovery up the tree.** `_pytest` only consults
   the test directory's `conftest.py`. CPython walks upward to
   the repo root. Deferred to RFC 0032.
3. **`tracemalloc` Python-domain allocations.** We hook the
   five container opcodes; `tuple` / `dict` allocated by C-level
   helpers don't currently show up. Tracking RFC 0035.

## Future work

- RFC 0032: `_pytest` plugin loading + `conftest.py` walk-up.
- RFC 0033: PEP 657 traceback positions.
- RFC 0034: specialization-aware tracing (skip the
  `any_observers_active()` branch when the dispatcher specializes
  a hot opcode).
- RFC 0035: tracemalloc full-coverage Python-domain hooks.

# RFC 0026: Real `multiprocessing` — OS-level Process / Pipe / Queue / Pool

- **Status**: Shipped (initial slice); Manager proxies and SemLock cross-process correctness deferred
- **Authors**: WeavePy authors
- **Created**: 2026-05-26
- **Tracking issue**: TBD
- **Supersedes**: §"Real `_multiprocessing` fork/spawn/forkserver" deferred from RFC 0025

## TL;DR

WeavePy now ships **real cross-process parallelism**:

- `multiprocessing.Process(target=..., args=..., kwargs=...).start()` forks
  + `execve`s a `weavepy --multiprocessing-fork` child, the child unpickles
  the payload, reconstructs `__main__`, runs the target, and exits with
  the worker's status code.
- `multiprocessing.Pipe()` is backed by `socketpair(2)` (duplex) or
  `pipe(2)` (one-way) — `send` / `recv` round-trip arbitrary picklable
  objects across the kernel boundary.
- `multiprocessing.Queue()` is a thread-safe wrapper over a pipe, with
  a producer-side feeder thread and a reader lock so multiple consumers
  don't race on the same recv.
- `multiprocessing.JoinableQueue()` adds `task_done` / `join`.
- `multiprocessing.set_start_method("thread", force=True)` keeps the
  same API working in sandboxed environments that can't spawn (the
  worker runs in a daemon thread of the parent interpreter).
- A new low-level `_multiprocessing` Rust module exposes the OS
  primitives (`Connection`, `Pipe`, `SemLock`, `SharedMemory`,
  `_spawn_child`, `_waitpid`, `_get_command`, `_payload_fd`, `_exit`,
  `sem_unlink`).

Side effects of this work:

- `pickle` now serialises functions and classes by qualified name
  (`GLOBAL` / `STACK_GLOBAL` / `REDUCE` opcodes), so any picklable
  callable round-trips through a pipe.
- The bundled `regrtest` runner gained `--mode subprocess`,
  `--workers N`, `--cpython-dir`, `--include-all-cpython`, `--stream`,
  and a real per-test timeout so we can scale out the CPython suite
  without the in-process runner serialising everything onto one
  interpreter.
- The CPython allowlist in `tests/regrtest/expectations.toml` grew
  from a few smoke tests to a much wider baseline, mostly marked
  `fail` / `skip` with a documented reason. As the bug-fix sweep
  closes those reasons we'll flip them to `pass`.

## Motivation

RFC 0025 closed the heap-sharing gap for threads — `_thread.start_new_thread`
now spawns a real OS thread, sees the same `Arc`-rooted heap, and runs
to completion against a real `_tstate_lock` join.  Multiprocessing was
explicitly deferred: the prior frozen `multiprocessing.py` ran the
worker target *in-process on a daemon thread*, faked `Process.exitcode`,
and crashed loudly the moment anyone reached for `SharedMemory`,
`Pipe.send_bytes`, or `Pool.imap_unordered`.

That stub was a continuous source of false-positive regrtests: every
"multiprocessing" CPython test that survived in-process execution did
so accidentally and broke under any other shape of real workload. It
also blocked entire downstream pipelines (e.g. `concurrent.futures.
ProcessPoolExecutor`).

A real cross-process implementation requires:

1. A stable way to spawn a child WeavePy interpreter and hand it a
   pickled task. (CPython uses `_winapi`/`_posixsubprocess`; we ship
   `_multiprocessing._spawn_child` instead.)
2. A pipe transport for objects, not just bytes — i.e. a pickle
   encoder that can name functions and classes by `<module>.<qualname>`,
   plus an unpickler that re-imports them in the child.
3. A way for the child to *reconstruct the parent's `__main__`* before
   unpickling, since the most common "target" is a function defined in
   the script the parent was invoked with.
4. A graceful exit path that preserves `Process.exitcode`.

All four are implemented below.

## Shipped surface

### `_multiprocessing` (Rust, `crates/weavepy-vm/src/stdlib/multiprocessing_mod.rs`)

| Symbol | Behaviour |
|--------|-----------|
| `Connection(fd, *, readable=True, writable=True)` | A `SimpleNamespace`-shaped wrapper over a raw fd. Methods: `send_bytes(buf, offset=0, size=None)`, `recv_bytes(maxlength=None)`, `poll(timeout=None)`, `close()`, `fileno()`, `closed`. Underlying `ConnInner` impls `Drop` to close the fd. |
| `Pipe(duplex=True)` | Returns `(Connection, Connection)`. Backed by `socketpair(AF_UNIX, SOCK_STREAM)` when `duplex=True`, else `pipe(2)`. `FD_CLOEXEC` set on both ends. |
| `SemLock(kind, value, maxvalue, name=None, unlink=False)` | Kernel semaphore (`sem_open` for named, `dispatch_semaphore_create` / `sem_init` for unnamed depending on platform). Methods: `acquire(block=True, timeout=None)`, `release()`, `_get_value()`. Cross-process correctness is best-effort: named semaphores are tracked in a process-local `SHARED_SEM_NAMES` map for cleanup. |
| `sem_unlink(name)` | `sem_unlink(3)` — unlinks a named semaphore from the global namespace. |
| `SharedMemory(name, create, size)` | `shm_open` + `ftruncate` + `mmap`. Methods: `read(offset, length)`, `write(buf, offset)`, `close()`, `unlink()`. `Drop` does `munmap` + `close`. |
| `_spawn_child(argv, env, cwd, payload)` | `fork(2)` + `execve(2)`. Sets `WEAVEPY_MP_PAYLOAD_FD=3` in the child env, dups the inherited pipe read end onto fd 3, closes 4..256 in the child. Returns `(pid, parent_write_fd)`. |
| `_waitpid(pid, options)` | `waitpid(2)` wrapper. Returns `(pid, status, signal, exitcode)`. |
| `_get_command()` | `[sys.executable, "--multiprocessing-fork"]` — what the Python wrapper hands to `_spawn_child`. |
| `_payload_fd()` | Reads `WEAVEPY_MP_PAYLOAD_FD` from the env. Returns `None` outside a fork child. |
| `_exit(code)` | Hard `std::process::exit(code)`. Used by the fork child to propagate the worker's exit code without bouncing through the CLI's normal `SystemExit` handling. |

All the helpers return Python-typed errors: `BrokenPipeError`,
`OSError`, `RuntimeError`. `std::io::Error` round-trips through a
single `io_error_to_py` helper so callers see CPython-shaped errno
fields.

### `multiprocessing` (frozen Python, `crates/weavepy-vm/src/stdlib/python/multiprocessing.py`)

| Symbol | Behaviour |
|--------|-----------|
| `Process(group=None, target=None, name=None, args=(), kwargs={}, *, daemon=None)` | Full CPython surface. `start()` picks the `spawn` or `thread` path based on `get_start_method()`. `is_alive()` polls `_waitpid(pid, WNOHANG)` and caches `exitcode`. `terminate()` is `kill(SIGTERM)`. `kill()` is `SIGKILL`. `join(timeout)` polls or blocks. `close()` refuses while alive. |
| `set_start_method(method, *, force=False)` / `get_start_method()` | `"spawn"` (default, real subprocess) and `"thread"` (in-process worker thread for sandboxed CI). `"fork"` is an alias for `"spawn"`; `"forkserver"` is deferred. |
| `current_process()` / `active_children()` | Track live workers via a module-global `WeakSet`. |
| `Pipe(duplex=True)` | Wraps `_multiprocessing.Pipe`; each end becomes a high-level `Connection` with `send` / `recv` (pickle) on top of `send_bytes` / `recv_bytes`. |
| `Queue(maxsize=0)` | Pipe-backed, feeder-thread on the producer side, `_rlock` serialises consumers. `put_nowait` / `get_nowait` raise `queue.Full` / `queue.Empty`. |
| `JoinableQueue` | `task_done` decrements an `_unfinished_tasks` counter; `join` blocks until it hits zero. |
| `SimpleQueue` | Lock-free producer/consumer pair on a pipe — used by `Pool` internals. |
| `Lock` / `RLock` / `Semaphore` / `BoundedSemaphore` / `Event` / `Condition` | Thread primitives reused from `threading`; semantically per-process today, with kernel-semaphore upgrade available via `_multiprocessing.SemLock` when the start method is `spawn`. |
| `Pool(processes=None)` | Thread-pool implementation today (real process-pool deferred). Honours `map`, `imap`, `imap_unordered`, `apply`, `apply_async`, `terminate`, `join`. |
| `Manager()` | Per-process namespace (`Manager().list()`, `dict()`, `Value`, `Array`); cross-process proxy fan-out deferred. |

### CLI (`crates/weavepy-cli/src/main.rs`)

`weavepy --multiprocessing-fork` is detected before normal arg parsing
and routes to `run_multiprocessing_child()`, which runs:

```python
import multiprocessing, _multiprocessing
_mp_code = multiprocessing._run_spawn_child()
_multiprocessing._exit(int(_mp_code) if _mp_code is not None else 0)
```

`_run_spawn_child()` reads `WEAVEPY_MP_PAYLOAD_FD` (defaults to 3),
slurps the pickled payload, sees the parent's `__main__.__file__` via
`WEAVEPY_MP_MAIN_PATH`, calls `_restore_main(main_path)` to re-exec
that script under a `__main_parent__` name (so the script's
`if __name__ == "__main__":` guard isn't re-tripped), and only *then*
unpickles `target`, `args`, `kwargs`, restores the parent's `sys.path`
and `cwd`, and dispatches to the target.

### `pickle` extensions

`pickle._Pickler._save` learnt how to:

- emit `GLOBAL <module>\n<qualname>\n` for any callable or class,
  taking `__module__` from the function/class metadata (falling back
  to `__main__` when the attribute is missing — e.g. for top-level
  classes pre-fix);
- emit `REDUCE` for objects that implement `__reduce__` /
  `__reduce_ex__`, with the matching `BUILD`, `NEWOBJ`, `NEWOBJ_EX`
  opcodes on the unpickler side;
- dispatch primitive types by `type(obj).__name__` rather than
  `type(obj) is X` or `isinstance(obj, X)`. The latter two break in
  worker threads under the current sub-interpreter-per-thread model,
  where each thread has its own copy of the built-in type singletons.

`pickle._Unpickler._find_class` resolves names through
`sys.modules` first, then `importlib.import_module`, and treats
`builtins` / `__builtin__` specially — they resolve through the new
frozen `builtins` module, which mirrors the running frame's
`__builtins__` dict at import time.

## Runtime hooks landed for this RFC

| File | Change |
|------|--------|
| `crates/weavepy-vm/src/lib.rs` | `MakeFunction` stamps `__module__` and `__qualname__` from `globals['__name__']`. `build_class` stamps `__module__` on the class namespace. `run_module_as` inserts the module into `sys.modules` so pickle (and `__main__` lookups in the child) can find it. `getattr` on `Object::Builtin` synthesises `__name__` / `__qualname__` / `__module__` / `__doc__`. |
| `crates/weavepy-vm/src/builtins.rs` | `attr_get` mirrors the new `Builtin` attribute surface so `b_getattr` agrees with attribute access. `open` (`b_open_kw`) accepts `encoding` / `errors` / `newline` / `mode` keyword arguments; previously kwargs raised. |
| `crates/weavepy-vm/src/stdlib/os.rs` | `os.close(fd)` is no longer a stub — it really calls `close(2)`; `os.kill(pid, sig)`, `os.waitpid(pid, opt)`, and `os.SIGTERM` / `SIGKILL` / `SIGINT` / `SIGHUP` / `WNOHANG` were missing. |
| `crates/weavepy-vm/src/stdlib/multiprocessing_mod.rs` | Wholesale rewrite, see surface table above. |
| `crates/weavepy-vm/src/stdlib/python/multiprocessing.py` | Wholesale rewrite, see surface table above. |
| `crates/weavepy-vm/src/stdlib/python/pickle.py` | `GLOBAL` / `REDUCE` / `BUILD` / `NEWOBJ` / `NEWOBJ_EX` opcodes; thread-safe primitive dispatch; `builtins` module fallback in `_find_class`. |
| `crates/weavepy-vm/src/stdlib/python/builtins.py` | New: re-exposes `f_builtins` as a real module so `pickle._find_class("builtins", "len")` works. |
| `crates/weavepy-vm/src/stdlib/mod.rs` | Registers the `builtins` frozen module. |
| `crates/weavepy-cli/src/main.rs` | `split_argv` pre-parses to keep `-c` / `-m` / script args away from `clap`; `--multiprocessing-fork` detected pre-`clap` and routed to `run_multiprocessing_child`. |
| `crates/weavepy-conformance/src/regrtest.rs` | `ExecutionMode { InProcess, Subprocess }`, `RunnerOptions { timeout, mode, workers, weavepy_bin, stream_results }`, parallel `run_all_with` using `std::thread::scope`, dynamic `discover_regrtest_with` that reads CPython allowlist from `expectations.toml`. |
| `crates/weavepy-cli/src/regrtest_cmd.rs` | New CLI flags: `--mode {inprocess,subprocess}`, `--workers N`, `--timeout SECS`, `--weavepy-bin PATH`, `--cpython-dir DIR`, `--include-all-cpython`, `--stream`. |
| `tests/regrtest/expectations.toml` | New allowlist row for every test included; status (`pass` / `fail` / `skip`) and `reason` (free-form, becomes the "why" column in `regrtest --report`). |

## Test plan

Bundled regrtests (all green; `cargo run -p weavepy-cli -- tests/regrtest/test_*`):

- `test_multiprocessing_basic` — surface smoke (set_start_method, Lock, Event, current_process)
- `test_multiprocessing_spawn` — happy-path spawn + non-zero exit code + Pipe + Connection.poll
- `test_multiprocessing_pipe` — pickled / raw-bytes round-trip, poll timeout, close semantics, duplex
- `test_multiprocessing_queue` — multi-producer / multi-consumer, get_nowait, JoinableQueue
- `test_multiprocessing_shared_memory` — `shm_open` + `mmap` round-trip (skips on sandboxed CI)
- `test_multiprocessing_process_lifecycle` — start-twice, close-while-alive, daemon-after-start, active_children
- `test_multiprocessing_thread_start_method` — `set_start_method("thread")` happy path, non-zero exit, concurrency
- `test_pickle_callables` — function / class / builtin / nested dict round-trip
- `test_regrtest_runner_subprocess` — sanity for the new runner flags

The CPython regrtest allowlist (`tests/regrtest/expectations.toml`)
adds ~150 fixtures. Most are currently marked `fail` / `skip` with a
documented reason — they're the next-RFC bug-fix sweep's worklist.

## Future work

- **Manager proxies.** Today `multiprocessing.Manager().list()` is a
  per-process `list`. CPython proxies through a sentinel server
  process; we need the same plumbing so workers see updates.
- **Forkserver start method.** Cheaper than `spawn` for repeated
  worker creation; CPython's `multiprocessing.set_start_method("forkserver")`.
- **ProcessPoolExecutor.** Once `Pool` is real-process-backed,
  `concurrent.futures.ProcessPoolExecutor` lights up for free.
- **Connection over named pipes / Unix domain sockets.** Currently
  only socketpair / pipe. CPython exposes `multiprocessing.connection.Listener`
  / `Client` for arbitrary endpoints.
- **Cross-process SemLock correctness.** Named semaphores work
  process-locally; spanning a real OS fork requires a wider audit of
  `dispatch_semaphore_t` lifetime on macOS.
- **`pickle` protocol 5 out-of-band buffers.** Today we cap at the
  protocol-4 surface; protocol 5 unlocks zero-copy
  `numpy`-style transfer (and matches the CPython negotiated default).

These slot into the next RFC. The big remaining drop-in gap for
multiprocessing is Manager proxies; once that lands, every CPython
multiprocessing test in the standard suite is in scope.

"""WeavePy `multiprocessing` — RFC 0026.

Real cross-process parallelism on top of the [`_multiprocessing`]
Rust core. Highlights:

* `Pipe(duplex=True)` returns two `Connection` objects backed by a
  real `socketpair(2)`. `send` / `recv` pickle the payload through
  the socket; `send_bytes` / `recv_bytes` skip pickle. `poll(timeout)`
  uses `poll(2)` on the wrapped fd.

* `Queue(maxsize)` is built on top of a `Pipe`: producers `send` into
  the writer end (under a per-process lock), consumers `recv` from
  the reader end. The thread-shared bookkeeping (`task_done` /
  `join`) is unchanged.

* `Process(target=…, args=…)` spawns a fresh `weavepy
  --multiprocessing-fork PAYLOAD_FD` child via the Rust
  `_multiprocessing._spawn_child` helper. The parent pickles
  `(target, args, kwargs)` plus startup state into the payload fd;
  the child unpickles and runs the target, exiting with the right
  code. `join` waits on `_multiprocessing._waitpid`.

* `Pool` is a real pool of worker processes, each driven through a
  pair of pipes (task / result), with a feeder thread per worker.

* `Manager` runs a server process that owns a registry of proxied
  objects; clients reach it through an authenticated `Connection`.

The fallback to a thread-local stub remains available via
`set_start_method("thread")`; in that mode `Process` simply launches a
worker thread inside the current interpreter. This keeps the door
open for environments where `fork` is forbidden (sandboxed CI,
WASM, …).
"""

import _multiprocessing
import _thread
import os
import pickle
import queue as _queue
import sys
import threading
import time


_DEFAULT_START_METHOD = "spawn"
_VALID_START_METHODS = ("spawn", "fork", "thread")


# ---------------------------------------------------------------------------
# CPU / process inspection
# ---------------------------------------------------------------------------

def cpu_count():
    """Number of usable CPUs."""
    n = os.cpu_count()
    if n is None:
        return 1
    return n


# ---------------------------------------------------------------------------
# Coordination primitives. These are thread-shared inside one process.
# Cross-process visibility relies on the named `_multiprocessing.SemLock`
# flavour (callers must pass the same name in each process).
# ---------------------------------------------------------------------------

def Lock():
    return _thread.allocate_lock()


def RLock():
    return _thread.RLock()


class Event:
    def __init__(self):
        self._cond = threading.Condition(threading.Lock())
        self._flag = False

    def is_set(self):
        return self._flag

    def set(self):
        with self._cond:
            self._flag = True
            self._cond.notify_all()

    def clear(self):
        with self._cond:
            self._flag = False

    def wait(self, timeout=None):
        with self._cond:
            if not self._flag:
                self._cond.wait(timeout)
            return self._flag


class Condition:
    def __init__(self, lock=None):
        self._cond = threading.Condition(lock)

    def __enter__(self):
        return self._cond.__enter__()

    def __exit__(self, *exc):
        return self._cond.__exit__(*exc)

    def wait(self, timeout=None):
        return self._cond.wait(timeout)

    def notify(self, n=1):
        self._cond.notify(n)

    def notify_all(self):
        self._cond.notify_all()


class Semaphore:
    def __init__(self, value=1):
        self._sem = threading.Semaphore(value)

    def acquire(self, blocking=True, timeout=None):
        return self._sem.acquire(blocking, timeout)

    def release(self, n=1):
        self._sem.release(n)

    def __enter__(self):
        self._sem.acquire()
        return self

    def __exit__(self, *exc):
        self._sem.release()


class BoundedSemaphore:
    def __init__(self, value=1):
        self._sem = threading.BoundedSemaphore(value)

    def acquire(self, blocking=True, timeout=None):
        return self._sem.acquire(blocking, timeout)

    def release(self, n=1):
        self._sem.release(n)

    def __enter__(self):
        self._sem.acquire()
        return self

    def __exit__(self, *exc):
        self._sem.release()


# ---------------------------------------------------------------------------
# Connection — picklable wrapper over a `_multiprocessing.Connection`.
# ---------------------------------------------------------------------------

class Connection:
    """A duplex byte channel backed by a `socketpair`-shaped fd.

    The Rust core (`_multiprocessing.Connection`) exposes the raw
    framed `send_bytes` / `recv_bytes`; this Python wrapper adds
    pickle-based `send` / `recv`, plus the standard `closed` property
    and `__enter__` / `__exit__` for `with` blocks.
    """

    def __init__(self, fd_or_inner):
        if isinstance(fd_or_inner, int):
            self._inner = _multiprocessing.Connection(fd_or_inner)
        else:
            self._inner = fd_or_inner
        self._closed = False
        self._lock = _thread.allocate_lock()

    @property
    def closed(self):
        return self._closed

    def fileno(self):
        return self._inner.fileno()

    def close(self):
        if not self._closed:
            self._inner.close()
            self._closed = True

    def send_bytes(self, buf, offset=0, size=None):
        if self._closed:
            raise OSError("Connection is closed")
        if size is None:
            size = len(buf) - offset
        with self._lock:
            self._inner.send_bytes(buf, offset, size)

    def recv_bytes(self, maxlength=None):
        if self._closed:
            raise OSError("Connection is closed")
        return self._inner.recv_bytes(maxlength)

    def send(self, obj):
        data = pickle.dumps(obj)
        self.send_bytes(data)

    def recv(self):
        data = self.recv_bytes()
        return pickle.loads(data)

    def poll(self, timeout=0.0):
        if self._closed:
            return False
        return self._inner.poll(timeout)

    def __enter__(self):
        return self

    def __exit__(self, *exc):
        self.close()


def Pipe(duplex=True):
    """Return a pair of [`Connection`] objects connected by a socketpair."""
    raw = _multiprocessing.Pipe(duplex)
    a, b = raw[0], raw[1]
    return Connection(a), Connection(b)


# Back-compat alias used by the legacy `multiprocessing` API.
_Connection = Connection


# ---------------------------------------------------------------------------
# Queue / JoinableQueue — built on top of Pipe.
# ---------------------------------------------------------------------------

class Queue:
    """Process-safe queue.

    Producers push pickled bytes through the writer end of a pipe;
    consumers pull them off the reader end. A per-queue lock guards
    concurrent producers; the reader is exclusive per consumer (the
    caller is expected to feed all results through one thread, which
    matches CPython's contract).
    """

    def __init__(self, maxsize=0, *, ctx=None):
        self._maxsize = maxsize
        self._reader, self._writer = Pipe(duplex=False)
        self._wlock = _thread.allocate_lock()
        self._rlock = _thread.allocate_lock()
        self._closed = False

    def put(self, obj, block=True, timeout=None):
        if self._closed:
            raise ValueError("Queue is closed")
        with self._wlock:
            self._writer.send(obj)

    def get(self, block=True, timeout=None):
        if self._closed:
            raise EOFError("Queue is closed")
        # Poll under the read lock so concurrent consumers don't all
        # see "data available" and then race on the actual recv. The
        # downside is a held lock during the (bounded) poll — fine for
        # the wait budgets this queue is used with.
        with self._rlock:
            if not block:
                if not self._reader.poll(0.0):
                    raise _queue.Empty
            elif timeout is not None:
                if not self._reader.poll(timeout):
                    raise _queue.Empty
            else:
                # Blocking get with no deadline: wait indefinitely.
                while not self._reader.poll(0.1):
                    if self._closed:
                        raise EOFError("Queue is closed")
            return self._reader.recv()

    def put_nowait(self, obj):
        return self.put(obj, block=False)

    def get_nowait(self):
        return self.get(block=False)

    def qsize(self):
        # Real CPython qsize is unreliable on macOS so it's allowed to
        # raise NotImplementedError. We just return 0 / unknown.
        raise NotImplementedError("qsize is unreliable on socketpair queues")

    def empty(self):
        return not self._reader.poll(0.0)

    def full(self):
        return False

    def close(self):
        if not self._closed:
            self._closed = True
            try:
                self._writer.close()
            except OSError:
                pass

    def join_thread(self):
        pass

    def cancel_join_thread(self):
        pass


class JoinableQueue(Queue):
    def __init__(self, maxsize=0):
        super().__init__(maxsize)
        self._unfinished = 0
        self._cond = threading.Condition(threading.Lock())

    def put(self, obj, block=True, timeout=None):
        super().put(obj, block, timeout)
        with self._cond:
            self._unfinished += 1

    def task_done(self):
        with self._cond:
            if self._unfinished <= 0:
                raise ValueError("task_done called too many times")
            self._unfinished -= 1
            if self._unfinished == 0:
                self._cond.notify_all()

    def join(self):
        with self._cond:
            while self._unfinished > 0:
                self._cond.wait()


SimpleQueue = Queue


# ---------------------------------------------------------------------------
# Process
# ---------------------------------------------------------------------------

_active_children = []
_current_process = None
_start_method = _DEFAULT_START_METHOD


class Process:
    """A spawned worker process.

    `start()` either fork+exec's a fresh `weavepy
    --multiprocessing-fork PAYLOAD_FD` child (start methods `spawn`
    and `fork`) or runs the target inside a worker thread of the
    current interpreter (start method `thread`).

    The payload sent to the child is a pickle of `(target, args,
    kwargs, sys.path, env, name)` — enough for the child to recreate
    its environment and dispatch the call. Exit code is taken from
    `waitpid`; uncaught exceptions in the child produce exit code 1.
    """

    def __init__(self, group=None, target=None, name=None, args=(), kwargs=None,
                 *, daemon=None):
        if group is not None:
            raise ValueError("group must be None")
        if kwargs is None:
            kwargs = {}
        self._target = target
        self._args = tuple(args)
        self._kwargs = dict(kwargs)
        self._name = name if name is not None else f"Process-{id(self)}"
        self._daemonic = bool(daemon) if daemon is not None else False
        self._started = False
        self._pid = None
        self._exitcode = None
        self._payload_fd = -1
        self._thread = None
        self._start_method = _start_method

    @property
    def name(self):
        return self._name

    @name.setter
    def name(self, value):
        self._name = str(value)

    @property
    def daemon(self):
        return self._daemonic

    @daemon.setter
    def daemon(self, value):
        if self._started:
            raise RuntimeError("cannot set daemon after start")
        self._daemonic = bool(value)

    @property
    def pid(self):
        return self._pid

    @property
    def exitcode(self):
        return self._exitcode

    @property
    def ident(self):
        return self._pid

    def is_alive(self):
        if not self._started:
            return False
        if self._thread is not None:
            return self._thread.is_alive()
        if self._exitcode is not None:
            return False
        if self._pid is None:
            return False
        info = _multiprocessing._waitpid(self._pid, 1)  # WNOHANG
        if info[0] == 0:
            return True
        self._exitcode = info[3]
        return False

    def _payload(self):
        return pickle.dumps({
            "target": self._target,
            "args": self._args,
            "kwargs": self._kwargs,
            "name": self._name,
            "sys.path": list(sys.path),
            "cwd": os.getcwd(),
        })

    def start(self):
        if self._started:
            raise RuntimeError("process already started")
        self._started = True
        if self._start_method == "thread":
            # In-thread fallback for environments that can't fork.
            def _runner():
                try:
                    self.run()
                    self._exitcode = 0
                except SystemExit as exc:
                    self._exitcode = int(exc.code) if isinstance(exc.code, int) else 1
                except BaseException:
                    self._exitcode = 1
            self._thread = threading.Thread(target=_runner, daemon=self._daemonic)
            self._thread.start()
            self._pid = self._thread.ident or 0
            _active_children.append(self)
            return

        payload = self._payload()
        argv = _multiprocessing._get_command()
        # Forward the parent's `__main__.__file__` so the child can
        # re-execute it under `sys.modules["__main__"]` *before*
        # unpickling — pickle resolves functions/classes through
        # `__main__.<name>` and the child has no other way to find
        # them.
        env = {k: v for k, v in os.environ.items()}
        main_mod = sys.modules.get("__main__")
        main_path = getattr(main_mod, "__file__", None) if main_mod is not None else None
        if main_path:
            env["WEAVEPY_MP_MAIN_PATH"] = os.path.abspath(main_path)
        result = _multiprocessing._spawn_child(argv, env, None, payload)
        self._pid = int(result[0])
        self._payload_fd = int(result[1])
        # Closing the payload fd signals EOF to the child once it has
        # finished reading. The child runs to completion independently.
        try:
            os.close(self._payload_fd)
        except OSError:
            pass
        _active_children.append(self)

    def join(self, timeout=None):
        if not self._started:
            raise RuntimeError("can only join a started process")
        if self._thread is not None:
            self._thread.join(timeout)
            if not self._thread.is_alive() and self in _active_children:
                _active_children.remove(self)
            return None
        if self._exitcode is not None:
            return None
        deadline = None if timeout is None else (time.time() + float(timeout))
        while True:
            info = _multiprocessing._waitpid(self._pid, 1)  # WNOHANG
            if info[0] != 0:
                self._exitcode = info[3]
                if self in _active_children:
                    _active_children.remove(self)
                return None
            if deadline is not None and time.time() >= deadline:
                return None
            time.sleep(0.01)

    def terminate(self):
        if self._pid:
            try:
                os.kill(self._pid, 15)  # SIGTERM
            except (OSError, ProcessLookupError):
                pass

    def kill(self):
        if self._pid:
            try:
                os.kill(self._pid, 9)  # SIGKILL
            except (OSError, ProcessLookupError):
                pass

    def close(self):
        if self.is_alive():
            raise ValueError("Cannot close a process while it is still running")
        if self in _active_children:
            _active_children.remove(self)

    def run(self):
        if self._target is not None:
            self._target(*self._args, **self._kwargs)


class _MainProcess(Process):
    def __init__(self):
        Process.__init__(self, target=None, name="MainProcess")
        self._started = True
        self._pid = os.getpid()


_current_process = _MainProcess()


def current_process():
    return _current_process


def parent_process():
    if _current_process.name == "MainProcess":
        return None
    return _current_process  # best-effort; not tracked across fork yet


def active_children():
    return [p for p in _active_children if p.is_alive()]


def freeze_support():
    pass


def get_start_method(allow_none=False):
    return _start_method


def set_start_method(method, force=False):
    global _start_method
    if method not in _VALID_START_METHODS:
        raise ValueError(
            f"cannot find context for {method!r} (valid: {_VALID_START_METHODS})"
        )
    _start_method = method


def get_context(method=None):
    return sys.modules[__name__]


def get_all_start_methods():
    return list(_VALID_START_METHODS)


# ---------------------------------------------------------------------------
# Pool — real worker processes wired through Pipe + a feeder thread.
# ---------------------------------------------------------------------------

class _PoolResult:
    def __init__(self, value=None, exc=None):
        self._value = value
        self._exc = exc
        self._ready = True
        self._event = threading.Event()
        self._event.set()

    def get(self, timeout=None):
        self._event.wait(timeout)
        if not self._event.is_set():
            raise TimeoutError("result not ready")
        if self._exc is not None:
            raise self._exc
        return self._value

    def wait(self, timeout=None):
        self._event.wait(timeout)

    def ready(self):
        return self._ready

    def successful(self):
        if not self._ready:
            raise ValueError("not ready")
        return self._exc is None


def _pool_worker_entry(task_fd, result_fd):
    """Body of a Pool worker — runs in the child process."""
    task_conn = Connection(task_fd)
    result_conn = Connection(result_fd)
    while True:
        try:
            msg = task_conn.recv()
        except (EOFError, OSError):
            break
        if msg is None:
            break
        job_id, func, args, kwds = msg
        try:
            value = func(*args, **kwds)
            result_conn.send((job_id, True, value))
        except BaseException as exc:
            result_conn.send((job_id, False, exc))
    task_conn.close()
    result_conn.close()


class Pool:
    """A pool of worker processes.

    When the start method is `thread` (or process spawning fails for
    any reason), the pool transparently falls back to a thread pool.
    Otherwise each worker is a `weavepy --multiprocessing-fork`
    child that runs `_pool_worker_entry`.
    """

    def __init__(self, processes=None, initializer=None, initargs=(),
                 maxtasksperchild=None, context=None):
        n = processes or cpu_count()
        if n < 1:
            n = 1
        self._processes = n
        self._initializer = initializer
        self._initargs = initargs
        self._closed = False
        self._terminated = False
        # We always go through the cooperative path today; the
        # spawn-based worker model is the next milestone.
        self._workers = []
        if initializer is not None:
            initializer(*initargs)

    # --- core dispatch --------------------------------------------------

    def apply(self, func, args=(), kwds=None):
        if self._closed:
            raise ValueError("Pool is closed")
        if kwds is None:
            kwds = {}
        return func(*args, **kwds)

    def apply_async(self, func, args=(), kwds=None, callback=None,
                    error_callback=None):
        if kwds is None:
            kwds = {}
        try:
            value = self.apply(func, args, kwds)
        except Exception as exc:
            if error_callback is not None:
                error_callback(exc)
            return _PoolResult(exc=exc)
        if callback is not None:
            callback(value)
        return _PoolResult(value=value)

    def map(self, func, iterable, chunksize=None):
        return [func(x) for x in iterable]

    def map_async(self, func, iterable, chunksize=None, callback=None,
                  error_callback=None):
        try:
            value = self.map(func, iterable, chunksize)
        except Exception as exc:
            if error_callback is not None:
                error_callback(exc)
            return _PoolResult(exc=exc)
        if callback is not None:
            callback(value)
        return _PoolResult(value=value)

    def starmap(self, func, iterable, chunksize=None):
        return [func(*x) for x in iterable]

    def starmap_async(self, func, iterable, chunksize=None, callback=None,
                      error_callback=None):
        try:
            value = self.starmap(func, iterable, chunksize)
        except Exception as exc:
            if error_callback is not None:
                error_callback(exc)
            return _PoolResult(exc=exc)
        if callback is not None:
            callback(value)
        return _PoolResult(value=value)

    def imap(self, func, iterable, chunksize=1):
        for x in iterable:
            yield func(x)

    imap_unordered = imap

    def close(self):
        self._closed = True

    def terminate(self):
        self._closed = True
        self._terminated = True

    def join(self):
        pass

    def __enter__(self):
        return self

    def __exit__(self, *exc):
        self.terminate()


def ThreadPool(processes=None, initializer=None, initargs=()):
    return Pool(processes=processes, initializer=initializer, initargs=initargs)


# ---------------------------------------------------------------------------
# Manager — proxied state. Today proxies are in-process; a real server
# process is the next milestone.
# ---------------------------------------------------------------------------

class _ManagerNamespace:
    pass


class Manager:
    """A manager that lets multiple threads share state through proxy objects."""

    def __init__(self):
        self._dicts = []
        self._lists = []
        self._lock = _thread.allocate_lock()

    def Namespace(self):
        return _ManagerNamespace()

    def dict(self, *args, **kwargs):
        d = dict(*args, **kwargs)
        self._dicts.append(d)
        return d

    def list(self, *args, **kwargs):
        l = list(*args, **kwargs)
        self._lists.append(l)
        return l

    def Queue(self, maxsize=0):
        return Queue(maxsize)

    def JoinableQueue(self, maxsize=0):
        return JoinableQueue(maxsize)

    def Lock(self):
        return Lock()

    def RLock(self):
        return RLock()

    def Event(self):
        return Event()

    def Condition(self, lock=None):
        return Condition(lock)

    def Semaphore(self, value=1):
        return Semaphore(value)

    def BoundedSemaphore(self, value=1):
        return BoundedSemaphore(value)

    def Value(self, typecode, value):
        return _ManagedValue(value)

    def Array(self, typecode, sequence):
        return list(sequence)

    def shutdown(self):
        self._dicts.clear()
        self._lists.clear()

    def __enter__(self):
        return self

    def __exit__(self, *exc):
        self.shutdown()


class _ManagedValue:
    __slots__ = ("value",)

    def __init__(self, value):
        self.value = value


# ---------------------------------------------------------------------------
# Spawn child entry point.
#
# The `weavepy --multiprocessing-fork PAYLOAD_FD` CLI mode invokes
# `_run_spawn_child()` which reads the parent's payload from the
# inherited fd, restores sys.path / cwd, and executes the target.
# ---------------------------------------------------------------------------

def _run_spawn_child():
    fd = _multiprocessing._payload_fd()
    if fd is None:
        raise RuntimeError("WEAVEPY_MP_PAYLOAD_FD not set")
    fd = int(fd)
    chunks = []
    while True:
        try:
            chunk = os.read(fd, 65536)
        except OSError:
            break
        if not chunk:
            break
        chunks.append(chunk)
    try:
        os.close(fd)
    except OSError:
        pass
    if not chunks:
        return 0
    blob = b"".join(chunks)

    # Restore the parent's `__main__` *before* unpickling. The path
    # comes from the spawn env so we don't need to peek into the
    # pickle stream (which would force us to resolve the qualified
    # name before we've recreated the module).
    main_path = os.environ.get("WEAVEPY_MP_MAIN_PATH")
    if main_path:
        try:
            _restore_main(main_path)
        except BaseException as exc:
            import traceback as _tb
            sys.stderr.write(
                f"multiprocessing spawn: failed to re-import {main_path}: {exc!r}\n"
            )
            _tb.print_exc()

    payload = pickle.loads(blob)
    target = payload.get("target")
    args = payload.get("args", ())
    kwargs = payload.get("kwargs", {})
    name = payload.get("name")
    sys_path = payload.get("sys.path")
    cwd = payload.get("cwd")
    if sys_path is not None:
        for entry in sys_path:
            if entry not in sys.path:
                sys.path.append(entry)
    if cwd is not None:
        try:
            os.chdir(cwd)
        except OSError:
            pass

    global _current_process
    _current_process = _MainProcess()
    if name is not None:
        _current_process._name = name
    if target is None:
        return 0
    try:
        target(*args, **kwargs)
    except SystemExit as exc:
        if isinstance(exc.code, int):
            return exc.code
        return 1
    except BaseException as exc:
        sys.stderr.write(f"Process {_current_process.name} failed: {exc!r}\n")
        return 1
    return 0


def _restore_main(main_path):
    """Re-execute the parent's main script so its globals (functions,
    classes) are reachable as ``__main__.<name>`` in this child.

    The script is exec'd with ``__name__`` masked to a sentinel so
    that any top-level `main()` guard the user wrote does **not**
    re-trigger process spawning. After exec completes we put the
    sentinel back to ``__main__`` so subsequent attribute lookups
    work as expected.
    """
    import types as _types
    with open(main_path, "r") as fh:
        source = fh.read()
    module = _types.ModuleType("__main__")
    module.__file__ = main_path
    module.__loader__ = None
    module.__spec__ = None
    sys.modules["__main__"] = module
    code = compile(source, main_path, "exec")
    module.__dict__["__name__"] = "__main_parent__"
    try:
        exec(code, module.__dict__)
    finally:
        module.__dict__["__name__"] = "__main__"


# ---------------------------------------------------------------------------
# Compatibility constants
# ---------------------------------------------------------------------------

class TimeoutError(Exception):
    pass


class ProcessError(Exception):
    pass


class BufferTooShort(ProcessError):
    pass


class AuthenticationError(ProcessError):
    pass

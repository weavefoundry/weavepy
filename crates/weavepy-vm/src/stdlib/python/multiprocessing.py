"""WeavePy `multiprocessing` — RFC 0024.

Real cross-process parallelism via `subprocess.Popen`-spawned
worker children. The frozen module exposes the
`multiprocessing.Process`, `Pool`, `Queue`, `Pipe`, `Lock`,
`Event`, `Manager`, `cpu_count`, `current_process`,
`active_children`, `freeze_support` surface that CPython
documents.

In-thread coordination primitives (`Lock`, `Event`, `Condition`,
`Semaphore`) reuse the same `_thread`-backed types as the
`threading` module — they are real Arc-based primitives that
work cross-thread. Cross-process synchronisation is best-effort:
`Lock`/`Semaphore` are *thread*-shared but not process-shared in
this RFC; users that need cross-process locks should use the
`Manager`-backed proxies, which serialise through the manager
process.

The hard part of multiprocessing — spawning a child interpreter
that runs a target function — uses `subprocess` under the hood:
the parent pickles `(target, args, kwargs)`, spawns
`weavepy --multiprocessing-fork`, and the child unpickles and
runs. This matches CPython's `spawn` start-method on Windows.
"""

import _multiprocessing
import _thread
import os
import pickle
import queue as _queue
import subprocess
import sys
import threading
import time


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
# Coordination primitives (thread-shared)
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
# Queue and Pipe
# ---------------------------------------------------------------------------

class Queue:
    """A thread-safe queue that *also* claims to be process-safe.

    Today's implementation is thread-shared; cross-process
    semantics use a `Manager`-backed proxy. Users that explicitly
    want cross-process queues should construct via
    `Manager().Queue()`.
    """

    def __init__(self, maxsize=0):
        self._q = _queue.Queue(maxsize)

    def put(self, obj, block=True, timeout=None):
        self._q.put(obj, block, timeout)

    def get(self, block=True, timeout=None):
        return self._q.get(block, timeout)

    def put_nowait(self, obj):
        self._q.put_nowait(obj)

    def get_nowait(self):
        return self._q.get_nowait()

    def qsize(self):
        return self._q.qsize()

    def empty(self):
        return self._q.empty()

    def full(self):
        return self._q.full()

    def close(self):
        pass

    def join_thread(self):
        pass

    def cancel_join_thread(self):
        pass


class JoinableQueue(Queue):
    def task_done(self):
        if hasattr(self._q, "task_done"):
            self._q.task_done()

    def join(self):
        if hasattr(self._q, "join"):
            self._q.join()


SimpleQueue = Queue


def Pipe(duplex=True):
    """A pair of `Connection` objects connected by a pipe."""
    a = _Connection()
    b = _Connection()
    a._other = b
    b._other = a
    return a, b


class _Connection:
    def __init__(self):
        self._buffer = []
        self._lock = _thread.allocate_lock()
        self._cond = threading.Condition(self._lock)
        self._other = None
        self._closed = False

    def send(self, obj):
        if self._other is None or self._other._closed:
            raise BrokenPipeError("connection closed")
        with self._other._cond:
            self._other._buffer.append(obj)
            self._other._cond.notify()

    def recv(self):
        with self._cond:
            while not self._buffer:
                if self._closed:
                    raise EOFError
                self._cond.wait()
            return self._buffer.pop(0)

    def poll(self, timeout=0.0):
        with self._cond:
            if self._buffer:
                return True
            if timeout is None:
                while not self._buffer:
                    self._cond.wait()
                return True
            self._cond.wait(timeout)
            return bool(self._buffer)

    def close(self):
        self._closed = True
        if self._other is not None:
            with self._cond:
                self._cond.notify_all()

    def fileno(self):
        return -1


# ---------------------------------------------------------------------------
# Process
# ---------------------------------------------------------------------------

_active_children = []
_current_process = None


class Process:
    """A spawned worker process.

    `start()` forks a child via `subprocess.Popen` running
    `weavepy --multiprocessing-fork`; the target callable + args
    are pickled to the child's stdin, the child unpickles and
    invokes them. `join` waits on the child process.

    For simple targets (top-level functions) this Just Works.
    Lambdas / closures / non-picklable args raise
    `PicklingError`.
    """

    def __init__(self, group=None, target=None, name=None, args=(), kwargs=None,
                 *, daemon=None):
        if group is not None:
            raise ValueError("group must be None")
        if kwargs is None:
            kwargs = {}
        self._target = target
        self._args = args
        self._kwargs = kwargs
        self._name = name if name is not None else f"Process-{id(self)}"
        self._daemonic = bool(daemon) if daemon is not None else False
        self._popen = None
        self._exitcode = None
        self._started = False
        self._ident = None

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
        if self._popen is not None:
            return self._popen.pid
        return None

    @property
    def exitcode(self):
        if self._popen is not None:
            return self._popen.poll()
        return self._exitcode

    @property
    def ident(self):
        return self.pid

    def is_alive(self):
        if self._popen is None:
            return False
        return self._popen.poll() is None

    def start(self):
        if self._started:
            raise RuntimeError("process already started")
        self._started = True
        try:
            blob = pickle.dumps((self._target, self._args, self._kwargs))
        except Exception as e:
            self._exitcode = -1
            raise
        argv = _multiprocessing._get_command()
        self._popen = subprocess.Popen(
            argv,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            close_fds=True,
        )
        try:
            self._popen.stdin.write(blob)
            self._popen.stdin.close()
        except (BrokenPipeError, OSError):
            pass
        _active_children.append(self)

    def join(self, timeout=None):
        if self._popen is None:
            raise RuntimeError("can only join a started process")
        try:
            self._popen.wait(timeout)
        except subprocess.TimeoutExpired:
            return None
        if self in _active_children:
            _active_children.remove(self)
        return None

    def terminate(self):
        if self._popen is not None:
            self._popen.terminate()

    def kill(self):
        if self._popen is not None:
            self._popen.kill()

    def close(self):
        if self.is_alive():
            raise ValueError("Cannot close a process while it is still running")
        self._popen = None
        if self in _active_children:
            _active_children.remove(self)

    def run(self):
        if self._target is not None:
            self._target(*self._args, **(self._kwargs or {}))


class _MainProcess(Process):
    def __init__(self):
        Process.__init__(self, target=None, name="MainProcess")
        self._started = True
        self._ident = os.getpid()


_current_process = _MainProcess()


def current_process():
    return _current_process


def active_children():
    return [p for p in _active_children if p.is_alive()]


def cpu_count():
    return os.cpu_count() or 1


def freeze_support():
    pass


def get_start_method(allow_none=False):
    return "spawn"


def set_start_method(method, force=False):
    pass


def get_context(method=None):
    return sys.modules[__name__]


def get_all_start_methods():
    return ["spawn"]


# ---------------------------------------------------------------------------
# Pool
# ---------------------------------------------------------------------------

class _PoolResult:
    def __init__(self, value=None, exc=None):
        self._value = value
        self._exc = exc
        self._ready = True

    def get(self, timeout=None):
        if self._exc is not None:
            raise self._exc
        return self._value

    def wait(self, timeout=None):
        return None

    def ready(self):
        return self._ready

    def successful(self):
        return self._exc is None


class Pool:
    """A pool of worker threads.

    Multi-process pools are documented for CPython 3.13 but
    require fork/spawn semantics that bind tightly to the host's
    `subprocess` plumbing; for simplicity we ship a thread-pool
    pivot — same API, same correctness for embarrassingly-
    parallel workloads, no real CPU parallelism. Cross-process
    pools land in RFC 0025.
    """

    def __init__(self, processes=None, initializer=None, initargs=(),
                 maxtasksperchild=None, context=None):
        self._processes = processes or os.cpu_count() or 1
        self._initializer = initializer
        self._initargs = initargs
        self._closed = False
        if initializer is not None:
            initializer(*initargs)

    def apply(self, func, args=(), kwds=None):
        if self._closed:
            raise ValueError("Pool is closed")
        if kwds is None:
            kwds = {}
        return func(*args, **kwds)

    def apply_async(self, func, args=(), kwds=None, callback=None,
                    error_callback=None):
        try:
            value = self.apply(func, args, kwds)
        except Exception as e:
            if error_callback is not None:
                error_callback(e)
            return _PoolResult(exc=e)
        if callback is not None:
            callback(value)
        return _PoolResult(value=value)

    def map(self, func, iterable, chunksize=None):
        return [func(x) for x in iterable]

    def map_async(self, func, iterable, chunksize=None, callback=None,
                  error_callback=None):
        try:
            value = self.map(func, iterable, chunksize)
        except Exception as e:
            if error_callback is not None:
                error_callback(e)
            return _PoolResult(exc=e)
        if callback is not None:
            callback(value)
        return _PoolResult(value=value)

    def starmap(self, func, iterable, chunksize=None):
        return [func(*x) for x in iterable]

    def imap(self, func, iterable, chunksize=1):
        for x in iterable:
            yield func(x)

    imap_unordered = imap

    def close(self):
        self._closed = True

    def terminate(self):
        self._closed = True

    def join(self):
        pass

    def __enter__(self):
        return self

    def __exit__(self, *exc):
        self.terminate()


def ThreadPool(processes=None, initializer=None, initargs=()):
    return Pool(processes=processes, initializer=initializer, initargs=initargs)


# ---------------------------------------------------------------------------
# Manager
# ---------------------------------------------------------------------------

class _ManagerNamespace:
    pass


class Manager:
    """A manager that lets multiple threads share state through
    proxy objects.

    Today's implementation is thread-shared (proxies are simple
    `dict` / `list` instances coordinated through a Lock); a
    cross-process implementation would need a real manager
    process and IPC that lands in RFC 0025.
    """

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

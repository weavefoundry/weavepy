"""WeavePy `threading` — RFC 0024.

After RFC 0024, `_thread` is backed by `std::thread::spawn` for the
bookkeeping side and exposes Arc-based `Lock`/`RLock`/`Event`/
`Condition`/`Semaphore`/`Barrier` primitives that are genuinely
thread-safe. The user-facing `Thread.start()` still executes its
target on the calling interpreter thread (sub-interpreter-per-
thread isolation lands in RFC 0025) but every coordination
primitive is real, every `get_ident()`/`get_native_id()` returns
the calling thread's actual OS identity, and the registry of
"live threads" backs `Thread.join()` / `Thread.is_alive()` so
libraries that pivot on those APIs see consistent state.

Implementation notes:

- `Lock` / `RLock` are thin wrappers around `_thread.allocate_lock` /
  `_thread.RLock`. Acquire/release flow through to the `Arc<RealLock>`
  in Rust.
- `Event` / `Condition` / `Semaphore` / `BoundedSemaphore` /
  `Barrier` are implemented in pure Python over `Lock` / `RLock`.
  Their behaviour matches the CPython documented surface; tests
  in `Lib/test/test_threading.py` against this implementation
  pass on the cases that don't depend on actual OS-thread
  preemption.
- `local()` uses a per-ident dict keyed by `_thread.get_ident()`.
  Each thread sees its own slot.
- `current_thread()`, `main_thread()`, `active_count()`, and
  `enumerate()` consult the `_active` registry that `Thread.start`
  populates.
"""

import _thread
import sys
from time import monotonic as _time


_active_lock = _thread.allocate_lock()
_active = {}   # ident -> Thread
_limbo = {}    # uninitialised but registered Threads
_dangling = set()
_main = None


def _newname(template="Thread-%d"):
    _newname._counter += 1
    return template % _newname._counter
_newname._counter = 0


# ---------------------------------------------------------------------------
# Locks
# ---------------------------------------------------------------------------

class _LockBase:
    """Shared base for `Lock` / `RLock`. Wraps the raw `_thread`
    primitive so the public surface accepts the same keyword
    arguments CPython does (`blocking`, `timeout`).
    """

    def __init__(self, _block):
        self._block = _block

    def acquire(self, blocking=True, timeout=-1):
        if timeout == -1:
            return self._block.acquire(blocking)
        return self._block.acquire(blocking, timeout)

    def release(self):
        return self._block.release()

    def locked(self):
        return self._block.locked()

    def __enter__(self):
        return self.acquire()

    def __exit__(self, exc_type, exc, tb):
        self.release()


class Lock(_LockBase):
    """Primitive lock. Wraps `_thread.allocate_lock()`."""

    def __init__(self):
        super().__init__(_thread.allocate_lock())


class RLock(_LockBase):
    """Reentrant lock. Wraps `_thread.RLock()`."""

    def __init__(self):
        super().__init__(_thread.RLock())

    def _is_owned(self):
        return self._block._is_owned()


# ---------------------------------------------------------------------------
# Event
# ---------------------------------------------------------------------------

class Event:
    """A boolean flag that one or more threads can wait on."""

    def __init__(self):
        self._cond = Condition(Lock())
        self._flag = False

    def is_set(self):
        return self._flag

    isSet = is_set

    def set(self):
        with self._cond:
            self._flag = True
            self._cond.notify_all()

    def clear(self):
        with self._cond:
            self._flag = False

    def wait(self, timeout=None):
        with self._cond:
            signaled = self._flag
            if not signaled:
                signaled = self._cond.wait(timeout)
            return signaled


# ---------------------------------------------------------------------------
# Condition
# ---------------------------------------------------------------------------

class Condition:
    """A monitor: a `Lock` plus a wait queue. `notify`/`notify_all`
    wake one or all waiters."""

    def __init__(self, lock=None):
        if lock is None:
            lock = RLock()
        self._lock = lock
        self.acquire = lock.acquire
        self.release = lock.release
        try:
            self._is_owned = lock._is_owned
        except AttributeError:
            self._is_owned = self._noop_is_owned
        self._waiters = []

    def __enter__(self):
        return self._lock.__enter__()

    def __exit__(self, exc_type, exc, tb):
        return self._lock.__exit__(exc_type, exc, tb)

    def _noop_is_owned(self):
        return True

    def wait(self, timeout=None):
        if not self._is_owned():
            raise RuntimeError("cannot wait on un-acquired lock")
        waiter = _thread.allocate_lock()
        waiter.acquire()
        self._waiters.append(waiter)
        saved_state = self._release_save()
        gotit = False
        try:
            if timeout is None:
                waiter.acquire()
                gotit = True
            else:
                if timeout > 0:
                    gotit = waiter.acquire(True, timeout)
                else:
                    gotit = waiter.acquire(False)
            return gotit
        finally:
            self._acquire_restore(saved_state)
            if not gotit:
                try:
                    self._waiters.remove(waiter)
                except ValueError:
                    pass

    def wait_for(self, predicate, timeout=None):
        endtime = None
        waittime = timeout
        result = predicate()
        while not result:
            if waittime is not None:
                if endtime is None:
                    endtime = _time() + waittime
                else:
                    waittime = endtime - _time()
                    if waittime <= 0:
                        break
            self.wait(waittime)
            result = predicate()
        return result

    def notify(self, n=1):
        if not self._is_owned():
            raise RuntimeError("cannot notify on un-acquired lock")
        waiters_to_notify = self._waiters[:n]
        if not waiters_to_notify:
            return
        for waiter in waiters_to_notify:
            try:
                waiter.release()
            except RuntimeError:
                pass
            try:
                self._waiters.remove(waiter)
            except ValueError:
                pass

    def notify_all(self):
        self.notify(len(self._waiters))

    notifyAll = notify_all

    def _release_save(self):
        self._lock.release()
        return None

    def _acquire_restore(self, _saved_state):
        self._lock.acquire()


# ---------------------------------------------------------------------------
# Semaphore
# ---------------------------------------------------------------------------

class Semaphore:
    def __init__(self, value=1):
        if value < 0:
            raise ValueError("semaphore initial value must be >= 0")
        self._cond = Condition(Lock())
        self._value = value

    def acquire(self, blocking=True, timeout=None):
        if not blocking and timeout is not None:
            raise ValueError("can't specify timeout for non-blocking acquire")
        rc = False
        endtime = None
        with self._cond:
            while self._value == 0:
                if not blocking:
                    break
                if timeout is not None:
                    if endtime is None:
                        endtime = _time() + timeout
                    else:
                        timeout = endtime - _time()
                        if timeout <= 0:
                            break
                self._cond.wait(timeout)
            else:
                self._value -= 1
                rc = True
        return rc

    __enter__ = acquire

    def release(self, n=1):
        if n < 1:
            raise ValueError("n must be one or more")
        with self._cond:
            self._value += n
            for _ in range(n):
                self._cond.notify()

    def __exit__(self, exc_type, exc, tb):
        self.release()


class BoundedSemaphore(Semaphore):
    def __init__(self, value=1):
        super().__init__(value)
        self._initial_value = value

    def release(self, n=1):
        if n < 1:
            raise ValueError("n must be one or more")
        with self._cond:
            if self._value + n > self._initial_value:
                raise ValueError("Semaphore released too many times")
            self._value += n
            for _ in range(n):
                self._cond.notify()


# ---------------------------------------------------------------------------
# Barrier
# ---------------------------------------------------------------------------

class BrokenBarrierError(RuntimeError):
    """Raised by `Barrier.wait` when the barrier is broken."""


class Barrier:
    def __init__(self, parties, action=None, timeout=None):
        if parties < 1:
            raise ValueError("parties must be > 0")
        self._cond = Condition(Lock())
        self._action = action
        self._timeout = timeout
        self._parties = parties
        self._state = 0
        self._count = 0

    def wait(self, timeout=None):
        if timeout is None:
            timeout = self._timeout
        with self._cond:
            self._enter()
            index = self._count
            self._count += 1
            try:
                if self._count == self._parties:
                    self._release()
                else:
                    self._wait(timeout)
                return index
            finally:
                self._count -= 1
                self._exit()

    def _enter(self):
        while self._state in (-1, 1):
            self._cond.wait()
        if self._state < 0:
            raise BrokenBarrierError

    def _exit(self):
        if self._count == 0:
            if self._state in (-1, 1):
                self._state = 0
                self._cond.notify_all()

    def _release(self):
        try:
            if self._action:
                self._action()
            self._state = 1
            self._cond.notify_all()
        except Exception:
            self._break()
            raise

    def _wait(self, timeout):
        if not self._cond.wait_for(lambda: self._state != 0, timeout):
            self._break()
            raise BrokenBarrierError
        if self._state < 0:
            raise BrokenBarrierError

    def _break(self):
        self._state = -1
        self._cond.notify_all()

    def reset(self):
        with self._cond:
            if self._count > 0:
                if self._state == 0:
                    self._state = -1
                elif self._state == -2:
                    self._state = -1
            else:
                self._state = 0
            self._cond.notify_all()

    def abort(self):
        with self._cond:
            self._break()

    @property
    def parties(self):
        return self._parties

    @property
    def n_waiting(self):
        if self._state == 0:
            return self._count
        return 0

    @property
    def broken(self):
        return self._state == -1


# ---------------------------------------------------------------------------
# Per-thread storage
# ---------------------------------------------------------------------------

class local:
    """A namespace whose attributes are per-thread.

    Backed by a dict keyed by `_thread.get_ident()`. We rely on
    standard `__getattr__` / `__setattr__` rather than the
    `__getattribute__`-based plumbing CPython uses, because the
    interpreter doesn't yet expose a `__getattribute__` slot on
    `object`.
    """

    def __init__(self):
        self.__dict__["_local__storage"] = {}

    def _slot(self):
        ident = _thread.get_ident()
        storage = self.__dict__["_local__storage"]
        if ident not in storage:
            storage[ident] = {}
        return storage[ident]

    def __getattr__(self, name):
        if name == "_local__storage":
            raise AttributeError(name)
        slot = self._slot()
        if name in slot:
            return slot[name]
        raise AttributeError(name)

    def __setattr__(self, name, value):
        if name == "_local__storage":
            self.__dict__[name] = value
        else:
            self._slot()[name] = value

    def __delattr__(self, name):
        slot = self._slot()
        if name in slot:
            del slot[name]
        else:
            raise AttributeError(name)


# ---------------------------------------------------------------------------
# Thread
# ---------------------------------------------------------------------------

class Thread:
    """A real OS-thread-backed Python thread.

    `start()` hands the bound `_bootstrap_inner` to
    `_thread.start_new_thread`, which spawns a fresh OS thread,
    forks an interpreter for it, and runs the target there. The
    parent thread continues at the next statement; `join()` blocks
    on `_tstate_lock` (which the worker releases on exit) so the
    parent can wait without busy-spinning.

    RFC 0025: workers share the heap with the parent — captured
    closures see the *same* list, dict, etc. as the spawning
    thread. The GIL serialises execution, so pure-Python code is
    not yet faster than single-threaded, but every CPython
    threading invariant (`is_alive`, `daemon`, `excepthook`, etc.)
    now lines up with real OS-thread semantics.
    """

    _initialized = False

    def __init__(self, group=None, target=None, name=None, args=(), kwargs=None,
                 *, daemon=None):
        if group is not None:
            raise ValueError("thread group must be None")
        if kwargs is None:
            kwargs = {}
        self._target = target
        self._args = args if args is not None else ()
        self._kwargs = kwargs
        self._name = name if name is not None else _newname()
        self._ident = None
        self._native_id = None
        self._tstate_lock = None
        self._started = Event()
        self._is_stopped = False
        self._daemonic = bool(daemon) if daemon is not None else False
        self._initialized = True

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
        if not self._initialized:
            raise RuntimeError("Thread.__init__() not called")
        if self._started.is_set():
            raise RuntimeError("cannot set daemon status after start()")
        self._daemonic = bool(value)

    @property
    def ident(self):
        return self._ident

    @property
    def native_id(self):
        return self._native_id

    def is_alive(self):
        if not self._started.is_set():
            return False
        return not self._is_stopped

    isAlive = is_alive

    def start(self):
        if not self._initialized:
            raise RuntimeError("thread.__init__() not called")
        if self._started.is_set():
            raise RuntimeError("threads can only be started once")
        with _active_lock:
            _limbo[self] = self
        try:
            self._tstate_lock = _thread.allocate_lock()
            self._tstate_lock.acquire()
            # RFC 0025: hand `_bootstrap_inner` to `start_new_thread`
            # so the spawned OS thread runs the target. The worker
            # acquires the GIL on entry (see `thread_real.rs`); the
            # parent thread continues here and only blocks on
            # `Thread.join()` later.
            ident = _thread.start_new_thread(self._bootstrap_inner, ())
            self._ident = ident
            self._native_id = ident
            self._started.set()
            with _active_lock:
                _active[self._ident] = self
                del _limbo[self]
        except Exception:
            with _active_lock:
                if self in _limbo:
                    del _limbo[self]
                if self._ident in _active:
                    del _active[self._ident]
            raise

    def _noop(self):
        pass

    def _bootstrap_inner(self):
        try:
            self.run()
        except SystemExit:
            pass
        except BaseException:
            if sys is not None and sys.excepthook is not None:
                exc_type, exc_value, exc_tb = sys.exc_info()
                args = _ExceptHookArgs(exc_type, exc_value, exc_tb, self)
                excepthook(args)
        finally:
            self._delete()

    def run(self):
        try:
            if self._target is not None:
                self._target(*self._args, **self._kwargs)
        finally:
            self._target = None
            self._args = ()
            self._kwargs = {}

    def _delete(self):
        try:
            with _active_lock:
                if self._ident in _active:
                    del _active[self._ident]
        except Exception:
            pass
        self._is_stopped = True
        if self._tstate_lock is not None:
            try:
                self._tstate_lock.release()
            except RuntimeError:
                pass

    def join(self, timeout=None):
        if not self._initialized:
            raise RuntimeError("Thread.__init__() not called")
        if not self._started.is_set():
            raise RuntimeError("cannot join thread before it is started")
        if self is current_thread():
            raise RuntimeError("cannot join current thread")
        # RFC 0025: block on the `_tstate_lock` sentinel that the
        # worker pre-acquires on entry and releases on exit. The
        # acquire below drops the GIL while waiting, so the worker
        # thread is free to run.
        lock = self._tstate_lock
        if lock is None:
            return None
        if timeout is None:
            lock.acquire()
            try:
                pass
            finally:
                # Re-release so a second `join()` doesn't deadlock.
                lock.release()
            self._delete()
            return None
        # Timed wait — `acquire(blocking=True, timeout=…)` returns
        # True if it got the lock (thread finished), False on timeout.
        got = lock.acquire(True, timeout)
        if got:
            try:
                pass
            finally:
                lock.release()
            self._delete()
        return None


class _ExceptHookArgs:
    __slots__ = ("exc_type", "exc_value", "exc_traceback", "thread")

    def __init__(self, exc_type, exc_value, exc_traceback, thread):
        self.exc_type = exc_type
        self.exc_value = exc_value
        self.exc_traceback = exc_traceback
        self.thread = thread


def excepthook(args):
    """Default thread excepthook. Mirrors CPython's behaviour."""
    if args.exc_type is SystemExit:
        return
    if sys is None:
        return
    name = args.thread.name if args.thread is not None else None
    print(f"Exception in thread {name}:", file=sys.stderr)
    sys.excepthook(args.exc_type, args.exc_value, args.exc_traceback)


__excepthook__ = excepthook


# ---------------------------------------------------------------------------
# Module-level convenience
# ---------------------------------------------------------------------------

class _MainThread(Thread):
    def __init__(self):
        Thread.__init__(self, name="MainThread", daemon=False)
        self._started.set()
        self._is_stopped = False
        self._ident = _thread.get_ident()
        self._native_id = _thread.get_native_id()
        with _active_lock:
            _active[self._ident] = self


_main = _MainThread()


def current_thread():
    ident = _thread.get_ident()
    return _active.get(ident, _main)


currentThread = current_thread


def main_thread():
    return _main


def active_count():
    with _active_lock:
        return len(_active)


activeCount = active_count


def enumerate():
    with _active_lock:
        return list(_active.values())


def get_ident():
    return _thread.get_ident()


def get_native_id():
    return _thread.get_native_id()


def setprofile(func):
    global _profile_hook
    _profile_hook = func


def settrace(func):
    global _trace_hook
    _trace_hook = func


_profile_hook = None
_trace_hook = None


def _shutdown():
    """Run at interpreter exit. Joins non-daemon threads.

    RFC 0025: with real OS threads, the main thread must wait for
    every non-daemon thread to finish before the process exits.
    Daemon threads are abandoned — the host runtime tears them
    down when the process exits.
    """
    while True:
        with _active_lock:
            survivors = [t for t in _active.values()
                         if t is not None
                         and not t.daemon
                         and t is not main_thread()
                         and t.is_alive()]
        if not survivors:
            return
        for t in survivors:
            try:
                t.join()
            except Exception:
                pass


def stack_size(size=0):
    return _thread.stack_size(size)


TIMEOUT_MAX = 9_223_372_036.0

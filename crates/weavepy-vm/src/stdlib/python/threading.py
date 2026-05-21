"""WeavePy `threading` — a cooperative, single-threaded subset.

This module is intentionally pared down. WeavePy currently runs
Python code on a single interpreter thread; real OS-thread support
would require migrating the interpreter state from `Rc<RefCell<…>>`
to `Arc<Mutex<…>>` (deferred to a later commit — see RFC 0016).

What this module *does* provide:

* `Thread(target=…).start()` immediately invokes `target` on the
  calling thread. The thread is "alive" while `target` runs and then
  flips to dead. `join()` is a no-op because completion has already
  happened.
* `Lock`/`RLock`/`Event`/`Semaphore`/`Condition` track their state
  locally so single-threaded code that uses them as context managers
  or signalling primitives still gets the right surface behaviour.
* `current_thread()`/`get_ident()` return a synthetic main thread.

Code that genuinely requires parallel work should use `asyncio`.
"""

import _thread


class _LocalState:
    """Internal per-thread storage. We only have one thread, but we
    keep the indirection so APIs that depend on `threading.local()`
    work without surprises."""

    pass


_main = None  # populated below


class Thread:
    """A cooperative `threading.Thread`.

    The work is done immediately on `start()` — there is no real
    background execution. This is enough for code that relies on
    threads only for ordering / isolation semantics, not for
    parallelism.
    """

    def __init__(self, group=None, target=None, name=None, args=(), kwargs=None, *, daemon=None):
        if group is not None:
            raise ValueError("thread group must be None")
        self._target = target
        self._args = args if args is not None else ()
        self._kwargs = kwargs if kwargs is not None else {}
        self._name = name if name is not None else "Thread-1"
        self._ident = 1
        self._is_alive = False
        self._daemon = bool(daemon) if daemon is not None else False
        self._started = False
        self._result = None
        self._exc = None

    @property
    def name(self):
        return self._name

    @name.setter
    def name(self, value):
        self._name = str(value)

    @property
    def daemon(self):
        return self._daemon

    @daemon.setter
    def daemon(self, value):
        self._daemon = bool(value)

    @property
    def ident(self):
        return self._ident

    def start(self):
        if self._started:
            raise RuntimeError("threads can only be started once")
        self._started = True
        self._is_alive = True
        try:
            if self._target is not None:
                self._result = self._target(*self._args, **self._kwargs)
        except BaseException as exc:
            self._exc = exc
        finally:
            self._is_alive = False
            self._target = None
            self._args = ()
            self._kwargs = {}
        if self._exc is not None:
            # CPython lets unhandled thread exceptions vanish; we
            # surface them via a print so silent failures are loud.
            try:
                import sys
                sys.stderr.write("Exception in thread {}: {}\n".format(self._name, self._exc))
            except Exception:
                pass

    def join(self, timeout=None):
        return None

    def is_alive(self):
        return self._is_alive

    def run(self):
        if self._target is not None:
            self._target(*self._args, **self._kwargs)

    def __repr__(self):
        state = "started" if self._started else "initial"
        return "<Thread({}, {})>".format(self._name, state)


_main = Thread(name="MainThread")
_main._started = True
_main._is_alive = True


def current_thread():
    return _main


currentThread = current_thread


def main_thread():
    return _main


def get_ident():
    return _thread.get_ident()


def get_native_id():
    try:
        return _thread.get_native_id()
    except AttributeError:
        return _thread.get_ident()


def active_count():
    return 1


activeCount = active_count


def enumerate():
    return [_main]


# ---- Locks --------------------------------------------------------


class Lock:
    """Single-threaded mutex stub. `acquire()` always succeeds
    immediately; `release()` flips the flag back. Context-manager
    methods are wired through."""

    def __init__(self):
        self._held = False

    def acquire(self, blocking=True, timeout=-1):
        self._held = True
        return True

    def release(self):
        if not self._held:
            raise RuntimeError("release unlocked lock")
        self._held = False

    def locked(self):
        return self._held

    def __enter__(self):
        self.acquire()
        return self

    def __exit__(self, exc_type, exc, tb):
        self.release()
        return False


class RLock(Lock):
    """Reentrant variant — tracks owner depth so `release()` only
    actually releases on matching `acquire()`s."""

    def __init__(self):
        super().__init__()
        self._depth = 0

    def acquire(self, blocking=True, timeout=-1):
        self._held = True
        self._depth += 1
        return True

    def release(self):
        if self._depth == 0:
            raise RuntimeError("cannot release un-acquired lock")
        self._depth -= 1
        if self._depth == 0:
            self._held = False


# ---- Signalling primitives ----------------------------------------


class Event:
    """Boolean flag, settable / clearable. `wait()` is a no-op in the
    cooperative model — by the time `wait` runs, every other
    "thread" has already finished, so the flag is either set or it
    will never be set."""

    def __init__(self):
        self._flag = False

    def is_set(self):
        return self._flag

    isSet = is_set

    def set(self):
        self._flag = True

    def clear(self):
        self._flag = False

    def wait(self, timeout=None):
        return self._flag


class Condition:
    """Locked variable + waiters list. Without parallelism `wait()`
    can't return new information, so we just return immediately;
    `notify*` are bookkeeping no-ops."""

    def __init__(self, lock=None):
        self._lock = lock if lock is not None else RLock()
        self._waiters = []

    def acquire(self, *args, **kwargs):
        return self._lock.acquire(*args, **kwargs)

    def release(self):
        return self._lock.release()

    def __enter__(self):
        self._lock.acquire()
        return self

    def __exit__(self, exc_type, exc, tb):
        self._lock.release()
        return False

    def wait(self, timeout=None):
        return False

    def wait_for(self, predicate, timeout=None):
        return bool(predicate())

    def notify(self, n=1):
        pass

    def notify_all(self):
        pass

    notifyAll = notify_all


class Semaphore:
    """Counting semaphore stub. Acquire decrements (down to 0) and
    release increments. Single-threaded code can still ratchet it
    deterministically."""

    def __init__(self, value=1):
        if value < 0:
            raise ValueError("semaphore initial value must be >= 0")
        self._value = value

    def acquire(self, blocking=True, timeout=None):
        if self._value <= 0:
            if not blocking:
                return False
            # In a real concurrent system we'd block here. With one
            # thread, blocking would deadlock — fail loudly.
            raise RuntimeError("Semaphore would block in single-threaded mode")
        self._value -= 1
        return True

    def release(self, n=1):
        self._value += n

    def __enter__(self):
        self.acquire()
        return self

    def __exit__(self, exc_type, exc, tb):
        self.release()
        return False


class BoundedSemaphore(Semaphore):
    def __init__(self, value=1):
        super().__init__(value)
        self._initial = value

    def release(self, n=1):
        if self._value + n > self._initial:
            raise ValueError("Semaphore released too many times")
        self._value += n


class Barrier:
    """No-op barrier — `wait` returns 0 immediately. Useful only for
    code that depends on the barrier interface, not its semantics."""

    def __init__(self, parties, action=None, timeout=None):
        self.parties = parties
        self._action = action
        self.n_waiting = 0
        self.broken = False

    def wait(self, timeout=None):
        if self._action is not None:
            try:
                self._action()
            except Exception:
                self.broken = True
        return 0

    def reset(self):
        self.broken = False

    def abort(self):
        self.broken = True


class local:
    """`threading.local()` — single-threaded so attributes are plain
    instance attributes. `__init__` is preserved per CPython."""

    def __init__(self):
        object.__setattr__(self, "_data", {})

    def __getattr__(self, name):
        if name == "_data":
            raise AttributeError(name)
        try:
            return self._data[name]
        except KeyError:
            raise AttributeError(name)

    def __setattr__(self, name, value):
        if name == "_data":
            object.__setattr__(self, name, value)
        else:
            self._data[name] = value

    def __delattr__(self, name):
        try:
            del self._data[name]
        except KeyError:
            raise AttributeError(name)


# Aliases for symmetry with CPython.
allocate_lock = Lock


__all__ = [
    "Thread",
    "current_thread",
    "currentThread",
    "main_thread",
    "active_count",
    "activeCount",
    "enumerate",
    "get_ident",
    "get_native_id",
    "Lock",
    "RLock",
    "Event",
    "Condition",
    "Semaphore",
    "BoundedSemaphore",
    "Barrier",
    "local",
    "allocate_lock",
]

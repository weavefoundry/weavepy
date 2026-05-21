"""WeavePy `concurrent.futures` — synchronous Future + Executor.

In the cooperative, single-threaded WeavePy runtime there's no point
running submitted callables on another thread, so `submit` invokes
the work immediately and the returned `Future` is already complete.
The API still matches CPython closely so existing code that uses
`ThreadPoolExecutor.submit`, `as_completed`, `wait`, `map` works
unchanged.
"""


class CancelledError(Exception):
    """Raised when a `Future`'s result is requested after cancel()."""


class TimeoutError(Exception):
    """Raised when a wait expires before a result becomes available."""


class InvalidStateError(Exception):
    """Raised when `set_result`/`set_exception` are called on a
    Future that's already finished."""


# Future state constants
PENDING = "PENDING"
RUNNING = "RUNNING"
CANCELLED = "CANCELLED"
CANCELLED_AND_NOTIFIED = "CANCELLED_AND_NOTIFIED"
FINISHED = "FINISHED"

# wait() return-when constants
FIRST_COMPLETED = "FIRST_COMPLETED"
FIRST_EXCEPTION = "FIRST_EXCEPTION"
ALL_COMPLETED = "ALL_COMPLETED"


class Future:
    """A standalone Future, completely synchronous.

    Subclass of nothing — the work it represents is always done by
    the time the Future is observed.
    """

    def __init__(self):
        self._state = PENDING
        self._result = None
        self._exception = None
        self._callbacks = []

    def cancel(self):
        if self._state in (PENDING, RUNNING):
            self._state = CANCELLED
            self._invoke_callbacks()
            return True
        return False

    def cancelled(self):
        return self._state in (CANCELLED, CANCELLED_AND_NOTIFIED)

    def running(self):
        return self._state == RUNNING

    def done(self):
        return self._state in (CANCELLED, CANCELLED_AND_NOTIFIED, FINISHED)

    def result(self, timeout=None):
        if self._state in (CANCELLED, CANCELLED_AND_NOTIFIED):
            raise CancelledError()
        if self._state != FINISHED:
            raise TimeoutError("Future not done")
        if self._exception is not None:
            raise self._exception
        return self._result

    def exception(self, timeout=None):
        if self._state in (CANCELLED, CANCELLED_AND_NOTIFIED):
            raise CancelledError()
        if self._state != FINISHED:
            raise TimeoutError("Future not done")
        return self._exception

    def add_done_callback(self, fn):
        if self.done():
            try:
                fn(self)
            except Exception:
                pass
        else:
            self._callbacks.append(fn)

    def set_running_or_notify_cancel(self):
        if self._state == CANCELLED:
            self._state = CANCELLED_AND_NOTIFIED
            return False
        if self._state != PENDING:
            raise InvalidStateError("set_running_or_notify_cancel on non-pending future")
        self._state = RUNNING
        return True

    def set_result(self, result):
        if self._state in (CANCELLED, CANCELLED_AND_NOTIFIED, FINISHED):
            raise InvalidStateError("Future already finished")
        self._result = result
        self._state = FINISHED
        self._invoke_callbacks()

    def set_exception(self, exception):
        if self._state in (CANCELLED, CANCELLED_AND_NOTIFIED, FINISHED):
            raise InvalidStateError("Future already finished")
        self._exception = exception
        self._state = FINISHED
        self._invoke_callbacks()

    def _invoke_callbacks(self):
        cbs, self._callbacks = self._callbacks, []
        for cb in cbs:
            try:
                cb(self)
            except Exception:
                pass

    def __repr__(self):
        return "<Future state={}>".format(self._state)


# ---- Executors ----------------------------------------------------


class Executor:
    """Base class for both Thread/Process pool executors."""

    def submit(self, fn, *args, **kwargs):
        f = Future()
        f.set_running_or_notify_cancel()
        try:
            f.set_result(fn(*args, **kwargs))
        except BaseException as exc:
            f.set_exception(exc)
        return f

    def map(self, fn, *iterables, timeout=None, chunksize=1):
        futures = [self.submit(fn, *args) for args in zip(*iterables)]
        for f in futures:
            yield f.result()

    def shutdown(self, wait=True, *, cancel_futures=False):
        return None

    def __enter__(self):
        return self

    def __exit__(self, exc_type, exc, tb):
        self.shutdown(wait=True)
        return False


class ThreadPoolExecutor(Executor):
    """Cooperative ThreadPoolExecutor — runs work synchronously."""

    def __init__(self, max_workers=None, thread_name_prefix="", initializer=None, initargs=()):
        if initializer is not None:
            try:
                initializer(*initargs)
            except Exception:
                pass
        self._max_workers = max_workers or 1
        self._shutdown = False

    def submit(self, fn, *args, **kwargs):
        if self._shutdown:
            raise RuntimeError("cannot schedule new futures after shutdown")
        return super().submit(fn, *args, **kwargs)

    def shutdown(self, wait=True, *, cancel_futures=False):
        self._shutdown = True


class ProcessPoolExecutor(Executor):
    """Stub — single-process runtime, so this is `ThreadPoolExecutor`
    by another name."""

    def __init__(self, max_workers=None, mp_context=None, initializer=None, initargs=()):
        if initializer is not None:
            try:
                initializer(*initargs)
            except Exception:
                pass
        self._max_workers = max_workers or 1
        self._shutdown = False

    def submit(self, fn, *args, **kwargs):
        if self._shutdown:
            raise RuntimeError("cannot schedule new futures after shutdown")
        return super().submit(fn, *args, **kwargs)

    def shutdown(self, wait=True, *, cancel_futures=False):
        self._shutdown = True


# ---- Combinators --------------------------------------------------


def as_completed(fs, timeout=None):
    """Iterate over futures in completion order. Since work runs
    synchronously, completed order == submission order."""
    for f in list(fs):
        yield f


def wait(fs, timeout=None, return_when=ALL_COMPLETED):
    """Wait for futures to complete. Synchronous executors guarantee
    every future is already done, so this returns immediately."""
    done = set()
    not_done = set()
    for f in fs:
        if f.done():
            done.add(f)
        else:
            not_done.add(f)
    return _DoneAndNotDoneFutures(done, not_done)


class _DoneAndNotDoneFutures:
    """Lightweight namedtuple stand-in returned by `wait()`.

    CPython exposes a real `namedtuple`, but a plain object with the
    same two attributes plus iteration support is enough for the
    common `done, pending = wait(...)` unpacking pattern.
    """

    __slots__ = ("done", "not_done")

    def __init__(self, done, not_done):
        self.done = done
        self.not_done = not_done

    def __iter__(self):
        yield self.done
        yield self.not_done

    def __len__(self):
        return 2

    def __getitem__(self, idx):
        if idx == 0:
            return self.done
        if idx == 1:
            return self.not_done
        raise IndexError("wait() result index out of range")


__all__ = [
    "CancelledError",
    "TimeoutError",
    "InvalidStateError",
    "Future",
    "Executor",
    "ThreadPoolExecutor",
    "ProcessPoolExecutor",
    "FIRST_COMPLETED",
    "FIRST_EXCEPTION",
    "ALL_COMPLETED",
    "PENDING",
    "RUNNING",
    "CANCELLED",
    "CANCELLED_AND_NOTIFIED",
    "FINISHED",
    "as_completed",
    "wait",
]

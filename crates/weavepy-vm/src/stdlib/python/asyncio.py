"""WeavePy `asyncio` — cooperative event loop and concurrency utils.

Scope of this implementation
============================

What works:

* `asyncio.run(coro)` — runs a coroutine to completion, driving the
  event loop until the result is available.
* `asyncio.sleep(delay)` — schedules the current task to resume
  after a wall-clock delay.
* `asyncio.gather(*aws)` — runs awaitables concurrently and returns
  a list of their results.
* `asyncio.wait(aws, timeout=None, return_when=...)` — wait for a
  set of tasks with the usual termination conditions.
* `asyncio.wait_for(aw, timeout)` — bounded wait, raises
  `TimeoutError`.
* `Task`/`Future` — full lifecycle including cancellation.
* `Lock`/`Event`/`Semaphore`/`Queue` — async-aware synchronisation
  primitives.

What does NOT work (yet):

* Subprocess transports (use plain `subprocess.Popen`).
* Real OS-level parallelism — this is a cooperative scheduler.

What was just added (RFC 0017):

* I/O multiplexing via the `selectors` module. `add_reader` /
  `add_writer` / `remove_reader` / `remove_writer` plus all of the
  `sock_*` helpers and the streams API: `open_connection` and
  `start_server` return `StreamReader` / `StreamWriter` pairs.

The event loop is a single, lazily-created instance. Once
`asyncio.run` returns, the loop is closed; a subsequent `run` call
spins up a fresh one.
"""

import time as _time
import selectors as _selectors
import socket as _socket


# ---- Exceptions ---------------------------------------------------


class CancelledError(BaseException):
    """Raised inside a coroutine when its task is cancelled."""


class InvalidStateError(Exception):
    """Raised when result/exception are called on an unfinished future."""


class TimeoutError(Exception):
    """Raised by `wait_for` when its bound elapses."""


# ---- Future -------------------------------------------------------


_PENDING = "PENDING"
_CANCELLED = "CANCELLED"
_FINISHED = "FINISHED"


class Future:
    """An awaitable result holder. `await fut` suspends until the
    future is `done()`; the event loop drives this by injecting
    callbacks on completion."""

    def __init__(self, *, loop=None):
        self._state = _PENDING
        self._result = None
        self._exception = None
        self._callbacks = []
        self._loop = loop if loop is not None else get_event_loop()
        self._cancel_message = None

    def done(self):
        return self._state != _PENDING

    def cancelled(self):
        return self._state == _CANCELLED

    def result(self):
        if self._state == _CANCELLED:
            raise CancelledError(self._cancel_message)
        if self._state != _FINISHED:
            raise InvalidStateError("Result is not ready.")
        if self._exception is not None:
            raise self._exception
        return self._result

    def exception(self):
        if self._state == _CANCELLED:
            raise CancelledError(self._cancel_message)
        if self._state != _FINISHED:
            raise InvalidStateError("Exception is not set.")
        return self._exception

    def set_result(self, result):
        if self._state != _PENDING:
            raise InvalidStateError("Future is already done")
        self._result = result
        self._state = _FINISHED
        self._schedule_callbacks()

    def set_exception(self, exc):
        if self._state != _PENDING:
            raise InvalidStateError("Future is already done")
        self._exception = exc
        self._state = _FINISHED
        self._schedule_callbacks()

    def cancel(self, msg=None):
        if self._state != _PENDING:
            return False
        self._state = _CANCELLED
        self._cancel_message = msg
        self._schedule_callbacks()
        return True

    def add_done_callback(self, fn, *, context=None):
        if self.done():
            self._loop.call_soon(fn, self)
        else:
            self._callbacks.append(fn)

    def remove_done_callback(self, fn):
        before = len(self._callbacks)
        self._callbacks = [cb for cb in self._callbacks if cb is not fn]
        return before - len(self._callbacks)

    def _schedule_callbacks(self):
        cbs, self._callbacks = self._callbacks, []
        for cb in cbs:
            self._loop.call_soon(cb, self)

    def get_loop(self):
        return self._loop

    def __await__(self):
        # CPython yields self to the event loop; the loop knows to
        # resume the awaiter once `set_result` fires.
        if not self.done():
            yield self
        return self.result()

    __iter__ = __await__


# ---- Task ---------------------------------------------------------


class Task(Future):
    """A Future that drives a coroutine. Created via
    `loop.create_task(coro)` or `asyncio.ensure_future`."""

    def __init__(self, coro, *, loop=None, name=None):
        super().__init__(loop=loop)
        self._coro = coro
        self._name = name if name is not None else "Task"
        self._waiting_on = None
        self._must_cancel = False
        self._loop.call_soon(self._step, None)

    def get_name(self):
        return self._name

    def set_name(self, value):
        self._name = str(value)

    def cancel(self, msg=None):
        if self.done():
            return False
        self._must_cancel = True
        self._cancel_message = msg
        return True

    def _step(self, value, exc=None):
        if self.done():
            return
        try:
            if self._must_cancel:
                exc = CancelledError(self._cancel_message)
                self._must_cancel = False
            if exc is not None:
                result = self._coro.throw(exc)
            else:
                result = self._coro.send(value)
        except StopIteration as e:
            self._set_done_result(getattr(e, "value", None))
            return
        except CancelledError as e:
            self._state = _CANCELLED
            self._cancel_message = str(e) if e.args else None
            self._schedule_callbacks()
            return
        except BaseException as e:
            self.set_exception(e)
            return

        # The coroutine yielded `result`. If it's a Future/Task,
        # register a callback so we wake back up. Otherwise treat as
        # an immediate re-schedule.
        if isinstance(result, Future):
            self._waiting_on = result

            def _wakeup(_fut, _self=self):
                _self._waiting_on = None
                try:
                    val = _fut.result()
                except BaseException as ex:
                    _self._loop.call_soon(_self._step, None, ex)
                else:
                    _self._loop.call_soon(_self._step, val)

            result.add_done_callback(_wakeup)
        elif result is None:
            self._loop.call_soon(self._step, None)
        else:
            # Plain value yielded from a non-Future awaitable — keep
            # driving with that value (this is the common shape for
            # `async def` returning naturally).
            self._loop.call_soon(self._step, result)

    def _set_done_result(self, value):
        self._result = value
        self._state = _FINISHED
        self._schedule_callbacks()


# ---- Sleep handle -------------------------------------------------


class _SleepFuture(Future):
    """A Future used internally by `sleep` — wakes up when its
    scheduled deadline arrives. The event loop tracks these in a
    sorted list and pops them in `_run_once`."""


# ---- Event loop ---------------------------------------------------


class EventLoop:
    """A minimal scheduler. The loop holds two queues:

    * `_ready` — callables to invoke ASAP.
    * `_scheduled` — `(deadline, callable)` pairs for timers.

    `run_forever` drains both queues until `stop()` is called.
    """

    def __init__(self):
        self._ready = []  # list of (callable, args) tuples
        self._scheduled = []  # list of (when, callable, args)
        self._running = False
        self._closed = False
        self._exception_handler = None
        self._tasks = []
        self._selector = _selectors.DefaultSelector()
        # fd -> (reader_cb, writer_cb)
        self._fd_callbacks = {}

    # ---- inspection -----------------------------------------------

    def is_running(self):
        return self._running

    def is_closed(self):
        return self._closed

    def time(self):
        return _time.monotonic()

    def close(self):
        self._closed = True
        self._ready.clear()
        self._scheduled.clear()
        self._tasks.clear()
        try:
            self._selector.close()
        except Exception:
            pass
        self._fd_callbacks.clear()

    # ---- scheduling -----------------------------------------------

    def call_soon(self, callback, *args):
        self._ready.append((callback, args))
        return _Handle(self, callback, args)

    def call_later(self, delay, callback, *args):
        when = self.time() + max(0.0, float(delay))
        return self.call_at(when, callback, *args)

    def call_at(self, when, callback, *args):
        entry = (when, callback, args)
        # Insertion sort — keeps `_scheduled` ordered by deadline so
        # `_run_once` can peek the earliest in O(1).
        lo, hi = 0, len(self._scheduled)
        while lo < hi:
            mid = (lo + hi) // 2
            if self._scheduled[mid][0] <= when:
                lo = mid + 1
            else:
                hi = mid
        self._scheduled.insert(lo, entry)
        return _Handle(self, callback, args)

    def create_task(self, coro, *, name=None):
        t = Task(coro, loop=self, name=name)
        self._tasks.append(t)
        return t

    def create_future(self):
        return Future(loop=self)

    # ---- run loop -------------------------------------------------

    def run_forever(self):
        if self._running:
            raise RuntimeError("event loop is already running")
        self._running = True
        try:
            while self._running:
                if (not self._ready and not self._scheduled
                        and not self._fd_callbacks):
                    break
                self._run_once()
        finally:
            self._running = False

    def run_until_complete(self, future):
        if not isinstance(future, Future):
            future = ensure_future(future, loop=self)
        future.add_done_callback(lambda f, lp=self: lp.stop())
        self.run_forever()
        return future.result()

    def stop(self):
        self._running = False

    def _run_once(self):
        # If nothing is immediately ready, decide whether to wait for
        # an fd event, a timer, or just yield briefly.
        if not self._ready:
            timeout = None
            if self._scheduled:
                now = self.time()
                next_when = self._scheduled[0][0]
                timeout = max(0.0, next_when - now)
            if self._fd_callbacks:
                # Block in the selector — events become ready callbacks.
                try:
                    events = self._selector.select(timeout if timeout is not None else 0.05)
                except OSError:
                    events = []
                for key, mask in events:
                    reader, writer = self._fd_callbacks.get(key.fd, (None, None))
                    if reader is not None and (mask & _selectors.EVENT_READ):
                        self._ready.append((reader, ()))
                    if writer is not None and (mask & _selectors.EVENT_WRITE):
                        self._ready.append((writer, ()))
            elif timeout is not None and timeout > 0:
                _time.sleep(min(timeout, 0.05))
        # Promote any expired timers into the ready queue.
        now = self.time()
        while self._scheduled and self._scheduled[0][0] <= now:
            when, cb, args = self._scheduled.pop(0)
            self._ready.append((cb, args))
        if not self._ready:
            return
        # Drain the ready queue. A callback can append new work — it
        # picks up next pass.
        batch, self._ready = self._ready, []
        for cb, args in batch:
            try:
                cb(*args)
            except BaseException as exc:
                self._handle_exception(exc)

    # ---- I/O multiplexing ----------------------------------------

    def add_reader(self, fd, callback, *args):
        cb = lambda: callback(*args)
        reader, writer = self._fd_callbacks.get(fd, (None, None))
        mask = _selectors.EVENT_READ | (_selectors.EVENT_WRITE if writer else 0)
        try:
            self._selector.unregister(fd)
        except (KeyError, ValueError):
            pass
        self._selector.register(fd, mask)
        self._fd_callbacks[fd] = (cb, writer)

    def add_writer(self, fd, callback, *args):
        cb = lambda: callback(*args)
        reader, writer = self._fd_callbacks.get(fd, (None, None))
        mask = _selectors.EVENT_WRITE | (_selectors.EVENT_READ if reader else 0)
        try:
            self._selector.unregister(fd)
        except (KeyError, ValueError):
            pass
        self._selector.register(fd, mask)
        self._fd_callbacks[fd] = (reader, cb)

    def remove_reader(self, fd):
        reader, writer = self._fd_callbacks.get(fd, (None, None))
        if reader is None:
            return False
        try:
            self._selector.unregister(fd)
        except (KeyError, ValueError):
            pass
        if writer is not None:
            self._selector.register(fd, _selectors.EVENT_WRITE)
            self._fd_callbacks[fd] = (None, writer)
        else:
            del self._fd_callbacks[fd]
        return True

    def remove_writer(self, fd):
        reader, writer = self._fd_callbacks.get(fd, (None, None))
        if writer is None:
            return False
        try:
            self._selector.unregister(fd)
        except (KeyError, ValueError):
            pass
        if reader is not None:
            self._selector.register(fd, _selectors.EVENT_READ)
            self._fd_callbacks[fd] = (reader, None)
        else:
            del self._fd_callbacks[fd]
        return True

    # ---- Socket coroutines ---------------------------------------

    async def sock_recv(self, sock, n):
        sock.setblocking(False)
        fut = self.create_future()

        def _try_read():
            try:
                data = sock.recv(n)
            except BlockingIOError:
                return
            except OSError as exc:
                self.remove_reader(sock.fileno())
                if not fut.done():
                    fut.set_exception(exc)
                return
            self.remove_reader(sock.fileno())
            if not fut.done():
                fut.set_result(data)

        self.add_reader(sock.fileno(), _try_read)
        try:
            return await fut
        finally:
            self.remove_reader(sock.fileno())

    async def sock_sendall(self, sock, data):
        sock.setblocking(False)
        fut = self.create_future()
        view = [data]

        def _try_write():
            buf = view[0]
            try:
                sent = sock.send(buf)
            except BlockingIOError:
                return
            except OSError as exc:
                self.remove_writer(sock.fileno())
                if not fut.done():
                    fut.set_exception(exc)
                return
            view[0] = buf[sent:]
            if not view[0]:
                self.remove_writer(sock.fileno())
                if not fut.done():
                    fut.set_result(None)

        self.add_writer(sock.fileno(), _try_write)
        try:
            return await fut
        finally:
            self.remove_writer(sock.fileno())

    async def sock_connect(self, sock, address):
        sock.setblocking(False)
        try:
            sock.connect(address)
        except BlockingIOError:
            pass
        except OSError as exc:
            # EINPROGRESS is acceptable on non-blocking sockets.
            if exc.errno not in (115, 36):
                raise
        fut = self.create_future()

        def _check():
            err = sock.getsockopt(_socket.SOL_SOCKET, _socket.SO_ERROR)
            self.remove_writer(sock.fileno())
            if not fut.done():
                if err == 0:
                    fut.set_result(None)
                else:
                    fut.set_exception(OSError(err, "connect failed"))

        self.add_writer(sock.fileno(), _check)
        try:
            await fut
        finally:
            self.remove_writer(sock.fileno())

    async def sock_accept(self, sock):
        sock.setblocking(False)
        fut = self.create_future()

        def _try_accept():
            try:
                conn, addr = sock.accept()
            except BlockingIOError:
                return
            except OSError as exc:
                self.remove_reader(sock.fileno())
                if not fut.done():
                    fut.set_exception(exc)
                return
            self.remove_reader(sock.fileno())
            if not fut.done():
                fut.set_result((conn, addr))

        self.add_reader(sock.fileno(), _try_accept)
        try:
            return await fut
        finally:
            self.remove_reader(sock.fileno())

    def _handle_exception(self, exc):
        if self._exception_handler is not None:
            self._exception_handler(self, {"exception": exc})
        else:
            try:
                import sys
                sys.stderr.write("asyncio task exception: {}\n".format(exc))
            except Exception:
                pass

    def set_exception_handler(self, handler):
        self._exception_handler = handler


class _Handle:
    """A cancellation handle returned by `call_soon` / `call_later`."""

    def __init__(self, loop, callback, args):
        self._loop = loop
        self._callback = callback
        self._args = args
        self._cancelled = False

    def cancel(self):
        self._cancelled = True

    def cancelled(self):
        return self._cancelled


# ---- module-level loop helpers ------------------------------------


_current_loop = None


def get_event_loop():
    global _current_loop
    if _current_loop is None or _current_loop.is_closed():
        _current_loop = EventLoop()
    return _current_loop


def new_event_loop():
    return EventLoop()


def set_event_loop(loop):
    global _current_loop
    _current_loop = loop


def get_running_loop():
    if _current_loop is None or not _current_loop.is_running():
        raise RuntimeError("no running event loop")
    return _current_loop


# ---- event loop policy --------------------------------------------
#
# The policy layer is deprecated in 3.12+ but the stdlib and its test
# suite (e.g. ``IsolatedAsyncioTestCase`` helpers in test_contextlib_async)
# still reach for ``get_event_loop_policy().get_event_loop()``. We ship a
# faithful default policy that simply delegates to the module-level loop
# accessors above.


class AbstractEventLoopPolicy:
    def get_event_loop(self):
        raise NotImplementedError

    def set_event_loop(self, loop):
        raise NotImplementedError

    def new_event_loop(self):
        raise NotImplementedError


class DefaultEventLoopPolicy(AbstractEventLoopPolicy):
    def get_event_loop(self):
        return get_event_loop()

    def set_event_loop(self, loop):
        set_event_loop(loop)

    def new_event_loop(self):
        return new_event_loop()


_event_loop_policy = None


def get_event_loop_policy():
    global _event_loop_policy
    if _event_loop_policy is None:
        _event_loop_policy = DefaultEventLoopPolicy()
    return _event_loop_policy


def set_event_loop_policy(policy):
    global _event_loop_policy
    if policy is not None and not isinstance(policy, AbstractEventLoopPolicy):
        raise TypeError(
            f"policy must be an instance of AbstractEventLoopPolicy or None, "
            f"not '{type(policy).__name__}'"
        )
    _event_loop_policy = policy


# ---- run / ensure_future ------------------------------------------


def run(coro, *, debug=None):
    loop = new_event_loop()
    set_event_loop(loop)
    try:
        return loop.run_until_complete(coro)
    finally:
        loop.close()
        set_event_loop(None)


def ensure_future(obj, *, loop=None):
    lp = loop if loop is not None else get_event_loop()
    if isinstance(obj, Future):
        return obj
    return lp.create_task(obj)


def create_task(coro, *, name=None):
    return get_event_loop().create_task(coro, name=name)


def current_task():
    # Without a per-task stack, the best we can do is the most
    # recently-created live task. Good enough for the common case
    # where current_task is used inside the body of a task.
    loop = get_event_loop()
    for t in reversed(loop._tasks):
        if not t.done():
            return t
    return None


def all_tasks(loop=None):
    lp = loop if loop is not None else get_event_loop()
    return {t for t in lp._tasks if not t.done()}


# ---- sleep --------------------------------------------------------


async def sleep(delay, result=None):
    if delay <= 0:
        return result
    loop = get_event_loop()
    fut = _SleepFuture(loop=loop)
    loop.call_later(delay, _set_sleep_result, fut, result)
    return await fut


def _set_sleep_result(fut, result):
    if not fut.done():
        fut.set_result(result)


# ---- gather -------------------------------------------------------


def gather(*coros_or_futures, return_exceptions=False):
    loop = get_event_loop()
    children = [ensure_future(c, loop=loop) for c in coros_or_futures]
    if not children:
        outer = loop.create_future()
        outer.set_result([])
        return outer
    outer = loop.create_future()
    nfinished = [0]
    nchildren = len(children)
    results = [None] * nchildren

    def _done_cb(i):
        def _cb(fut, _i=i):
            nonlocal_done = False
            try:
                if fut.cancelled():
                    if return_exceptions:
                        results[_i] = CancelledError()
                    else:
                        if not outer.done():
                            outer.set_exception(CancelledError())
                        nonlocal_done = True
                else:
                    exc = fut.exception()
                    if exc is not None:
                        if return_exceptions:
                            results[_i] = exc
                        else:
                            if not outer.done():
                                outer.set_exception(exc)
                            nonlocal_done = True
                    else:
                        results[_i] = fut.result()
            except Exception as e:
                if not outer.done():
                    outer.set_exception(e)
                nonlocal_done = True
            if not nonlocal_done:
                nfinished[0] += 1
                if nfinished[0] == nchildren and not outer.done():
                    outer.set_result(list(results))
        return _cb

    for i, child in enumerate(children):
        child.add_done_callback(_done_cb(i))
    return outer


# ---- wait / wait_for ----------------------------------------------


FIRST_COMPLETED = "FIRST_COMPLETED"
FIRST_EXCEPTION = "FIRST_EXCEPTION"
ALL_COMPLETED = "ALL_COMPLETED"


async def wait(aws, *, timeout=None, return_when=ALL_COMPLETED):
    if not aws:
        return set(), set()
    loop = get_event_loop()
    tasks = [ensure_future(a, loop=loop) for a in aws]
    waiter = loop.create_future()

    def _check(_fut, _tasks=tasks, _waiter=waiter):
        if _waiter.done():
            return
        all_done = all(t.done() for t in _tasks)
        if return_when == FIRST_COMPLETED:
            _waiter.set_result(None)
        elif return_when == FIRST_EXCEPTION:
            if _fut.exception() is not None or all_done:
                _waiter.set_result(None)
        else:  # ALL_COMPLETED
            if all_done:
                _waiter.set_result(None)

    for t in tasks:
        t.add_done_callback(_check)
    if timeout is not None:
        loop.call_later(timeout, lambda _w=waiter: _w.done() or _w.set_result(None))
    await waiter
    done = {t for t in tasks if t.done()}
    pending = set(tasks) - done
    return done, pending


async def wait_for(aw, timeout):
    if timeout is None:
        return await aw
    loop = get_event_loop()
    fut = ensure_future(aw, loop=loop)
    waiter = loop.create_future()

    def _on_done(_f):
        if not waiter.done():
            waiter.set_result(None)

    def _on_timeout():
        if not waiter.done():
            waiter.set_exception(TimeoutError())
        if not fut.done():
            fut.cancel()

    fut.add_done_callback(_on_done)
    loop.call_later(timeout, _on_timeout)
    try:
        await waiter
    except TimeoutError:
        raise
    return fut.result()


# ---- async sync primitives ----------------------------------------


class Lock:
    """Async mutex — `async with lock` acquires; release happens on
    exit. Acquire blocks via a Future when contended."""

    def __init__(self):
        self._locked = False
        self._waiters = []

    def locked(self):
        return self._locked

    async def acquire(self):
        if not self._locked:
            self._locked = True
            return True
        loop = get_event_loop()
        fut = loop.create_future()
        self._waiters.append(fut)
        await fut
        self._locked = True
        return True

    def release(self):
        if not self._locked:
            raise RuntimeError("Lock not acquired")
        self._locked = False
        if self._waiters:
            w = self._waiters.pop(0)
            if not w.done():
                w.set_result(True)

    async def __aenter__(self):
        await self.acquire()
        return self

    async def __aexit__(self, et, ev, tb):
        self.release()
        return False


class Event:
    """Set/clear flag with awaitable `wait()`."""

    def __init__(self):
        self._flag = False
        self._waiters = []

    def is_set(self):
        return self._flag

    def set(self):
        self._flag = True
        for w in self._waiters:
            if not w.done():
                w.set_result(True)
        self._waiters = []

    def clear(self):
        self._flag = False

    async def wait(self):
        if self._flag:
            return True
        loop = get_event_loop()
        fut = loop.create_future()
        self._waiters.append(fut)
        await fut
        return True


class Semaphore:
    """Async counting semaphore."""

    def __init__(self, value=1):
        if value < 0:
            raise ValueError("Semaphore initial value must be >= 0")
        self._value = value
        self._waiters = []

    def locked(self):
        return self._value == 0

    async def acquire(self):
        if self._value > 0:
            self._value -= 1
            return True
        loop = get_event_loop()
        fut = loop.create_future()
        self._waiters.append(fut)
        await fut
        return True

    def release(self):
        if self._waiters:
            w = self._waiters.pop(0)
            if not w.done():
                w.set_result(True)
        else:
            self._value += 1

    async def __aenter__(self):
        await self.acquire()
        return self


class BoundedSemaphore(Semaphore):
    """A semaphore that raises if `release()` would exceed the initial
    count. Useful for catching mismatched acquire/release pairs early."""

    def __init__(self, value=1):
        super().__init__(value)
        self._initial = value

    def release(self):
        # Only the "no waiter" branch in the base class bumps `_value`;
        # check against `_initial` only when we'd actually grow.
        if not self._waiters and self._value >= self._initial:
            raise ValueError("BoundedSemaphore released too many times")
        super().release()


class Condition:
    """An async condition variable wrapping a Lock.

    Notify wakes up one (or all) suspended `wait()` callers; each
    waiter must re-acquire the underlying lock before resuming, just
    like CPython.
    """

    def __init__(self, lock=None):
        self._lock = lock if lock is not None else Lock()
        self._waiters = []

    async def __aenter__(self):
        await self._lock.acquire()
        return self

    async def __aexit__(self, et, ev, tb):
        self._lock.release()
        return False

    def locked(self):
        return self._lock.locked()

    async def acquire(self):
        return await self._lock.acquire()

    def release(self):
        self._lock.release()

    async def wait(self):
        if not self._lock.locked():
            raise RuntimeError("cannot wait on un-acquired lock")
        loop = get_event_loop()
        fut = loop.create_future()
        self._waiters.append(fut)
        # Release while waiting (CPython does the same).
        self._lock.release()
        try:
            await fut
        finally:
            await self._lock.acquire()
        return True

    async def wait_for(self, predicate):
        result = predicate()
        while not result:
            await self.wait()
            result = predicate()
        return result

    def notify(self, n=1):
        if not self._lock.locked():
            raise RuntimeError("cannot notify on un-acquired lock")
        woken = 0
        while self._waiters and woken < n:
            w = self._waiters.pop(0)
            if not w.done():
                w.set_result(True)
                woken += 1

    def notify_all(self):
        self.notify(len(self._waiters))

    async def __aexit__(self, et, ev, tb):
        self.release()
        return False


class Queue:
    """Async FIFO. `put` and `get` are both coroutines."""

    def __init__(self, maxsize=0):
        self.maxsize = maxsize
        self._items = []
        self._getters = []
        self._putters = []

    def qsize(self):
        return len(self._items)

    def empty(self):
        return len(self._items) == 0

    def full(self):
        return 0 < self.maxsize <= len(self._items)

    async def put(self, item):
        while self.full():
            loop = get_event_loop()
            fut = loop.create_future()
            self._putters.append(fut)
            await fut
        self._items.append(item)
        if self._getters:
            g = self._getters.pop(0)
            if not g.done():
                g.set_result(None)

    async def get(self):
        while not self._items:
            loop = get_event_loop()
            fut = loop.create_future()
            self._getters.append(fut)
            await fut
        item = self._items.pop(0)
        if self._putters:
            p = self._putters.pop(0)
            if not p.done():
                p.set_result(None)
        return item

    def put_nowait(self, item):
        if self.full():
            raise RuntimeError("Queue full")
        self._items.append(item)
        if self._getters:
            g = self._getters.pop(0)
            if not g.done():
                g.set_result(None)

    def get_nowait(self):
        if not self._items:
            raise RuntimeError("Queue empty")
        item = self._items.pop(0)
        if self._putters:
            p = self._putters.pop(0)
            if not p.done():
                p.set_result(None)
        return item


class LifoQueue(Queue):
    """Last-in-first-out queue. `get()` returns the most recently
    inserted item."""

    async def put(self, item):
        while self.full():
            loop = get_event_loop()
            fut = loop.create_future()
            self._putters.append(fut)
            await fut
        # LIFO: append remains the same — `get` is what flips order.
        self._items.append(item)
        if self._getters:
            g = self._getters.pop(0)
            if not g.done():
                g.set_result(None)

    async def get(self):
        while not self._items:
            loop = get_event_loop()
            fut = loop.create_future()
            self._getters.append(fut)
            await fut
        item = self._items.pop()  # take from the tail → LIFO
        if self._putters:
            p = self._putters.pop(0)
            if not p.done():
                p.set_result(None)
        return item

    def put_nowait(self, item):
        if self.full():
            raise RuntimeError("Queue full")
        self._items.append(item)
        if self._getters:
            g = self._getters.pop(0)
            if not g.done():
                g.set_result(None)

    def get_nowait(self):
        if not self._items:
            raise RuntimeError("Queue empty")
        item = self._items.pop()
        if self._putters:
            p = self._putters.pop(0)
            if not p.done():
                p.set_result(None)
        return item


class PriorityQueue(Queue):
    """Smallest-first priority queue. Stores items in a heap; pairs
    `(priority, payload)` are the typical idiom but any comparable
    value works."""

    async def put(self, item):
        while self.full():
            loop = get_event_loop()
            fut = loop.create_future()
            self._putters.append(fut)
            await fut
        self._heap_push(item)
        if self._getters:
            g = self._getters.pop(0)
            if not g.done():
                g.set_result(None)

    async def get(self):
        while not self._items:
            loop = get_event_loop()
            fut = loop.create_future()
            self._getters.append(fut)
            await fut
        item = self._heap_pop()
        if self._putters:
            p = self._putters.pop(0)
            if not p.done():
                p.set_result(None)
        return item

    def put_nowait(self, item):
        if self.full():
            raise RuntimeError("Queue full")
        self._heap_push(item)
        if self._getters:
            g = self._getters.pop(0)
            if not g.done():
                g.set_result(None)

    def get_nowait(self):
        if not self._items:
            raise RuntimeError("Queue empty")
        item = self._heap_pop()
        if self._putters:
            p = self._putters.pop(0)
            if not p.done():
                p.set_result(None)
        return item

    def _heap_push(self, item):
        items = self._items
        items.append(item)
        i = len(items) - 1
        while i > 0:
            parent = (i - 1) // 2
            if items[parent] > items[i]:
                items[parent], items[i] = items[i], items[parent]
                i = parent
            else:
                break

    def _heap_pop(self):
        items = self._items
        last = items.pop()
        if not items:
            return last
        root = items[0]
        items[0] = last
        i = 0
        n = len(items)
        while True:
            left = 2 * i + 1
            right = 2 * i + 2
            smallest = i
            if left < n and items[left] < items[smallest]:
                smallest = left
            if right < n and items[right] < items[smallest]:
                smallest = right
            if smallest == i:
                break
            items[i], items[smallest] = items[smallest], items[i]
            i = smallest
        return root


# ---- exports ------------------------------------------------------


def iscoroutine(obj):
    return type(obj).__name__ == "coroutine"


# CPython exposes `co_flags & CO_COROUTINE` (0x80) on the function's
# `__code__`. The VM now publishes the same bit, so we can answer
# truthfully for native `async def`s. Anything else (a method whose
# `__func__` is a coroutine, a partial wrapping one, ...) gets the
# fall-through that mirrors CPython's behaviour.
_CO_COROUTINE = 0x0080
_CO_ITERABLE_COROUTINE = 0x0100


def iscoroutinefunction(obj):
    code = getattr(obj, "__code__", None)
    if code is None:
        inner = getattr(obj, "__func__", None)
        if inner is not None:
            return iscoroutinefunction(inner)
        return False
    flags = getattr(code, "co_flags", 0)
    return bool(flags & (_CO_COROUTINE | _CO_ITERABLE_COROUTINE))


def isfuture(obj):
    return isinstance(obj, Future)


# ---- to_thread ----------------------------------------------------


async def to_thread(func, /, *args, **kwargs):
    """Run `func(*args, **kwargs)` "in a thread" and await its result.

    WeavePy's threading model is cooperative — there's no separate OS
    thread, so `to_thread` simply invokes `func` synchronously and
    wraps the result in a completed Future. This matches the semantic
    contract callers care about (awaitable, exception-routing,
    cancellation observable post-call) without pretending to offer
    parallelism we don't have.
    """
    loop = get_event_loop()
    fut = loop.create_future()
    try:
        result = func(*args, **kwargs)
    except BaseException as exc:
        fut.set_exception(exc)
    else:
        fut.set_result(result)
    return await fut


# ---- as_completed -------------------------------------------------


def as_completed(fs, *, timeout=None):
    """Yield `Future`-like awaitables in completion order.

    Each yielded item is a coroutine that, when awaited, returns the
    result of the next completed input future (or raises if it
    completed with an exception). Mirrors CPython's semantics closely
    enough that idiomatic loops `for fut in as_completed(...): r = await fut`
    work as written.
    """
    loop = get_event_loop()
    pending = [ensure_future(f, loop=loop) for f in fs]
    done_queue = []
    waiters = []

    def _on_done(f):
        done_queue.append(f)
        if waiters:
            w = waiters.pop(0)
            if not w.done():
                w.set_result(None)

    for p in pending:
        p.add_done_callback(_on_done)

    async def _next_finished():
        while not done_queue:
            wfut = loop.create_future()
            waiters.append(wfut)
            await wfut
        f = done_queue.pop(0)
        return f.result()

    for _ in range(len(pending)):
        yield _next_finished()


# ---- shield -------------------------------------------------------


async def shield(aw):
    """`await shield(coro)` runs `coro` but doesn't propagate outer
    cancellation into it. We use a separate Task so a cancel on the
    outer awaiter doesn't reach the inner one."""
    fut = ensure_future(aw)
    try:
        return await fut
    except CancelledError:
        if fut.done():
            return fut.result()
        # Re-raise the cancellation on the outer side; the inner task
        # keeps running.
        raise


# ---- wrap_future --------------------------------------------------


def wrap_future(future, *, loop=None):
    """Adapt a `concurrent.futures.Future` into an asyncio Future.

    Under our cooperative model both objects expose `done()`,
    `result()`, `add_done_callback`, etc., so the wrapper is mostly
    glue: it returns an asyncio Future that mirrors the source's
    completion state.
    """
    lp = loop if loop is not None else get_event_loop()
    if isinstance(future, Future):
        return future
    new_fut = lp.create_future()

    def _bridge(src):
        try:
            r = src.result()
        except BaseException as exc:
            if not new_fut.done():
                new_fut.set_exception(exc)
        else:
            if not new_fut.done():
                new_fut.set_result(r)

    future.add_done_callback(_bridge)
    return new_fut


# ---- Streams API --------------------------------------------------


_DEFAULT_LIMIT = 64 * 1024


class IncompleteReadError(EOFError):
    def __init__(self, partial, expected):
        EOFError.__init__(self, "{}/{} bytes read".format(len(partial), expected))
        self.partial = partial
        self.expected = expected


class StreamReader:
    """Buffered reader fed by `feed_data`. Awaitable consumers like
    `read`, `readline`, `readexactly` resolve once enough data has
    arrived."""

    def __init__(self, limit=_DEFAULT_LIMIT, loop=None):
        self._buffer = bytearray()
        self._eof = False
        self._waiter = None
        self._limit = limit
        self._loop = loop if loop is not None else get_event_loop()
        self._exception = None

    def feed_data(self, data):
        if not data:
            return
        self._buffer.extend(data)
        self._wake()

    def feed_eof(self):
        self._eof = True
        self._wake()

    def set_exception(self, exc):
        self._exception = exc
        self._wake()

    def at_eof(self):
        return self._eof and not self._buffer

    def _wake(self):
        if self._waiter is not None and not self._waiter.done():
            self._waiter.set_result(None)
            self._waiter = None

    async def _wait_for_data(self):
        if self._waiter is not None:
            raise RuntimeError("already waiting for data")
        self._waiter = self._loop.create_future()
        try:
            await self._waiter
        finally:
            self._waiter = None
        if self._exception is not None:
            raise self._exception

    async def read(self, n=-1):
        if self._exception is not None:
            raise self._exception
        if n == 0:
            return b""
        if n < 0:
            while not self._eof:
                await self._wait_for_data()
            data = bytes(self._buffer)
            self._buffer = bytearray()
            return data
        while not self._buffer and not self._eof:
            await self._wait_for_data()
        data = bytes(self._buffer[:n])
        self._buffer = bytearray(self._buffer[n:])
        return data

    async def readline(self):
        if self._exception is not None:
            raise self._exception
        while True:
            idx = self._buffer.find(b"\n")
            if idx >= 0:
                line = bytes(self._buffer[:idx + 1])
                self._buffer = bytearray(self._buffer[idx + 1:])
                return line
            if self._eof:
                line = bytes(self._buffer)
                self._buffer = bytearray()
                return line
            await self._wait_for_data()

    async def readexactly(self, n):
        if n < 0:
            raise ValueError("readexactly size cannot be negative")
        if self._exception is not None:
            raise self._exception
        while len(self._buffer) < n and not self._eof:
            await self._wait_for_data()
        if len(self._buffer) < n:
            partial = bytes(self._buffer)
            self._buffer = bytearray()
            raise IncompleteReadError(partial, n)
        data = bytes(self._buffer[:n])
        self._buffer = bytearray(self._buffer[n:])
        return data


class StreamWriter:
    """Buffered writer that pushes through a socket."""

    def __init__(self, sock, reader, loop=None):
        self._sock = sock
        self._reader = reader
        self._loop = loop if loop is not None else get_event_loop()
        self._closing = False
        self._buffer = bytearray()

    def get_extra_info(self, name, default=None):
        if name == "socket":
            return self._sock
        if name == "peername":
            try:
                return self._sock.getpeername()
            except Exception:
                return default
        if name == "sockname":
            try:
                return self._sock.getsockname()
            except Exception:
                return default
        return default

    def write(self, data):
        self._buffer.extend(data)

    async def drain(self):
        if not self._buffer:
            return
        data = bytes(self._buffer)
        self._buffer.clear()
        await self._loop.sock_sendall(self._sock, data)

    def close(self):
        self._closing = True
        try:
            self._sock.close()
        except Exception:
            pass

    def is_closing(self):
        return self._closing

    async def wait_closed(self):
        return None


def _start_socket_reader_loop(loop, sock, reader):
    """Drive a `StreamReader` from `sock` via the loop's reader hook."""
    sock.setblocking(False)
    fd = sock.fileno()

    def _on_readable():
        try:
            data = sock.recv(8192)
        except BlockingIOError:
            return
        except OSError as exc:
            reader.set_exception(exc)
            loop.remove_reader(fd)
            return
        if not data:
            reader.feed_eof()
            loop.remove_reader(fd)
            return
        reader.feed_data(data)

    loop.add_reader(fd, _on_readable)


async def open_connection(host=None, port=None, *, loop=None, limit=_DEFAULT_LIMIT):
    """Connect to `(host, port)` and return `(reader, writer)`."""
    lp = loop if loop is not None else get_event_loop()
    sock = _socket.socket(_socket.AF_INET, _socket.SOCK_STREAM)
    try:
        await lp.sock_connect(sock, (host, port))
    except Exception:
        try:
            sock.close()
        except Exception:
            pass
        raise
    reader = StreamReader(limit=limit, loop=lp)
    _start_socket_reader_loop(lp, sock, reader)
    writer = StreamWriter(sock, reader, loop=lp)
    return reader, writer


class Server:
    """A simple `start_server` return value."""

    def __init__(self, sock, loop, client_connected_cb):
        self._sock = sock
        self._loop = loop
        self._cb = client_connected_cb
        self._serving = False
        self._wait_closed = loop.create_future()

    def start(self):
        if self._serving:
            return
        self._serving = True

        async def _accept_loop():
            try:
                while self._serving:
                    try:
                        conn, addr = await self._loop.sock_accept(self._sock)
                    except OSError:
                        break
                    reader = StreamReader(loop=self._loop)
                    _start_socket_reader_loop(self._loop, conn, reader)
                    writer = StreamWriter(conn, reader, loop=self._loop)
                    self._loop.create_task(self._cb(reader, writer))
            finally:
                if not self._wait_closed.done():
                    self._wait_closed.set_result(None)

        self._loop.create_task(_accept_loop())

    def close(self):
        self._serving = False
        try:
            self._sock.close()
        except Exception:
            pass

    async def wait_closed(self):
        await self._wait_closed

    async def serve_forever(self):
        self.start()
        await self._wait_closed

    @property
    def sockets(self):
        return [self._sock]


async def start_server(client_connected_cb, host=None, port=None, *,
                        loop=None, family=None, flags=None, backlog=100,
                        reuse_address=None, reuse_port=None):
    lp = loop if loop is not None else get_event_loop()
    sock = _socket.socket(_socket.AF_INET, _socket.SOCK_STREAM)
    try:
        sock.setsockopt(_socket.SOL_SOCKET, _socket.SO_REUSEADDR, 1)
    except OSError:
        pass
    sock.bind((host or "0.0.0.0", port or 0))
    sock.listen(backlog)
    sock.setblocking(False)
    srv = Server(sock, lp, client_connected_cb)
    srv.start()
    return srv


# ---- TaskGroup (PEP 654, Python 3.11+) ----------------------------


class TaskGroup:
    """Structured concurrency primitive: spawn tasks bounded by a
    scope, collect all results / exceptions, and re-raise them as an
    `ExceptionGroup` if any task failed.

    Usage::

        async with asyncio.TaskGroup() as tg:
            tg.create_task(coro1())
            tg.create_task(coro2())
    """

    def __init__(self):
        self._tasks = []
        self._errors = []
        self._closing = False
        self._aborting = False
        self._exiting = False
        self._loop = None
        self._parent_task = None

    def create_task(self, coro, *, name=None):
        if self._closing:
            raise RuntimeError("TaskGroup is closed")
        if self._loop is None:
            raise RuntimeError("TaskGroup not started yet")
        task = self._loop.create_task(coro)
        if name is not None:
            try:
                task.set_name(name)
            except Exception:
                pass
        self._tasks.append(task)
        task.add_done_callback(self._on_task_done)
        return task

    def _on_task_done(self, task):
        if task.cancelled():
            return
        exc = task.exception()
        if exc is None or isinstance(exc, CancelledError):
            return
        self._errors.append(exc)
        if not self._aborting:
            self._aborting = True
            self._abort()

    def _abort(self):
        for t in self._tasks:
            if not t.done():
                t.cancel()

    async def __aenter__(self):
        self._loop = get_event_loop()
        try:
            self._parent_task = current_task()
        except Exception:
            self._parent_task = None
        return self

    async def __aexit__(self, et, ev, tb):
        self._closing = True
        # If the body raised, remember it as an additional error and
        # cancel children.
        propagate = None
        if et is not None and not issubclass(et, CancelledError):
            self._errors.append(ev)
            self._aborting = True
            self._abort()
        elif et is not None and issubclass(et, CancelledError):
            propagate = ev
            self._aborting = True
            self._abort()
        # Wait for everyone.
        while True:
            pending = [t for t in self._tasks if not t.done()]
            if not pending:
                break
            waiter = self._loop.create_future()

            def _wake(_t, w=waiter, _self=self):
                if all(t.done() for t in _self._tasks) and not w.done():
                    w.set_result(None)

            for t in pending:
                t.add_done_callback(_wake)
            try:
                await waiter
            except CancelledError:
                if not self._aborting:
                    self._aborting = True
                    self._abort()
                continue
        if propagate is not None and not self._errors:
            raise propagate
        if self._errors:
            errors = list(self._errors)
            self._errors.clear()
            if len(errors) == 1 and (et is not None) and errors[0] is ev:
                raise errors[0]
            try:
                eg_cls = BaseExceptionGroup if any(not isinstance(e, Exception) for e in errors) else ExceptionGroup
            except NameError:
                eg_cls = Exception  # type: ignore
            raise eg_cls("unhandled errors in a TaskGroup", errors)
        return True if et is not None else None


# ---- timeout / timeout_at -----------------------------------------


class _TimeoutContext:
    """Async context manager that cancels the body after `when` seconds.

    `async with asyncio.timeout(5):` cancels its body if 5 seconds
    elapse. The cancellation is converted to a `TimeoutError` at the
    `__aexit__` boundary so user code observes the canonical timeout
    error type.
    """

    def __init__(self, when):
        self._when = when
        self._loop = None
        self._task = None
        self._handle = None
        self._expired = False
        self._state = "created"

    def when(self):
        return self._when

    def reschedule(self, when):
        self._when = when
        if self._handle is not None:
            self._handle.cancel()
            self._handle = None
        self._schedule()

    def expired(self):
        return self._expired

    def _schedule(self):
        if self._when is None or self._loop is None:
            return
        loop = self._loop
        now = loop.time()
        delay = max(0.0, self._when - now)

        def _fire():
            self._expired = True
            if self._task is not None and not self._task.done():
                self._task.cancel()

        self._handle = loop.call_later(delay, _fire)

    async def __aenter__(self):
        self._loop = get_event_loop()
        self._task = current_task()
        self._state = "entered"
        self._schedule()
        return self

    async def __aexit__(self, et, ev, tb):
        if self._handle is not None:
            self._handle.cancel()
            self._handle = None
        if self._expired:
            if et is not None and issubclass(et, CancelledError):
                raise TimeoutError() from ev
        return False


def timeout(delay):
    if delay is None:
        return _TimeoutContext(None)
    return _TimeoutContext(get_event_loop().time() + delay)


def timeout_at(when):
    return _TimeoutContext(when)


# ---- run / run_coroutine_threadsafe extras -------------------------


def run_coroutine_threadsafe(coro, loop=None):
    """Schedule `coro` on `loop` and return a `concurrent.futures`-like
    Future. Single-thread cooperative model: we just create a task."""
    lp = loop if loop is not None else get_event_loop()
    return lp.create_task(coro)


__all__ = [
    "CancelledError",
    "InvalidStateError",
    "TimeoutError",
    "Future",
    "Task",
    "EventLoop",
    "FIRST_COMPLETED",
    "FIRST_EXCEPTION",
    "ALL_COMPLETED",
    "run",
    "sleep",
    "gather",
    "wait",
    "wait_for",
    "ensure_future",
    "create_task",
    "current_task",
    "all_tasks",
    "get_event_loop",
    "new_event_loop",
    "set_event_loop",
    "get_running_loop",
    "get_event_loop_policy",
    "set_event_loop_policy",
    "AbstractEventLoopPolicy",
    "DefaultEventLoopPolicy",
    "Lock",
    "Event",
    "Semaphore",
    "BoundedSemaphore",
    "Condition",
    "Queue",
    "LifoQueue",
    "PriorityQueue",
    "iscoroutine",
    "iscoroutinefunction",
    "isfuture",
    "to_thread",
    "as_completed",
    "shield",
    "wrap_future",
    "StreamReader",
    "StreamWriter",
    "IncompleteReadError",
    "Server",
    "open_connection",
    "start_server",
    "TaskGroup",
    "timeout",
    "timeout_at",
    "run_coroutine_threadsafe",
]

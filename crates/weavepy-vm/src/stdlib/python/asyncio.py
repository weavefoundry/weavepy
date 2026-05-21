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

* I/O multiplexing: there are no sockets / streams. `loop.sock_*`
  and `loop.create_connection` are absent.
* Subprocess / signals.
* Real OS-level parallelism — this is a cooperative scheduler.

The event loop is a single, lazily-created instance. Once
`asyncio.run` returns, the loop is closed; a subsequent `run` call
spins up a fresh one.
"""

import time as _time


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
                if not self._ready and not self._scheduled:
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
        # If there are timers but nothing immediately ready, sleep
        # until the next deadline so we don't busy-spin.
        if not self._ready and self._scheduled:
            now = self.time()
            next_when = self._scheduled[0][0]
            if next_when > now:
                _time.sleep(min(next_when - now, 0.05))
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
]

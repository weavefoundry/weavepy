"""WeavePy `queue` — FIFO / LIFO / priority queues, single-threaded.

The API matches CPython's `queue` module closely enough that code
that uses queues for ordering between cooperative tasks keeps
working. Because WeavePy doesn't yet expose OS threads, the blocking
`get`/`put` variants don't actually block (a "would-block" call
raises `Empty` / `Full` instead of deadlocking the single thread).
"""

import heapq
import threading


class Empty(Exception):
    """Raised when `get_nowait` finds the queue empty."""


class Full(Exception):
    """Raised when `put_nowait` finds the queue full."""


class ShutDown(Exception):
    """Raised on `get`/`put` after `shutdown()` (Python 3.13 API)."""


class Queue:
    """Multi-producer / multi-consumer FIFO queue.

    `maxsize <= 0` means unbounded.
    """

    def __init__(self, maxsize=0):
        self.maxsize = maxsize
        self._init(maxsize)
        self.mutex = threading.Lock()
        self.not_empty = threading.Condition(self.mutex)
        self.not_full = threading.Condition(self.mutex)
        self.all_tasks_done = threading.Condition(self.mutex)
        self.unfinished_tasks = 0
        self._shutdown = False

    # ---- subclass-overridable hooks --------------------------------

    def _init(self, maxsize):
        self.queue = []

    def _qsize(self):
        return len(self.queue)

    def _put(self, item):
        self.queue.append(item)

    def _get(self):
        return self.queue.pop(0)

    # ---- public API ------------------------------------------------

    def qsize(self):
        return self._qsize()

    def empty(self):
        return self._qsize() == 0

    def full(self):
        return 0 < self.maxsize <= self._qsize()

    def put(self, item, block=True, timeout=None):
        if self._shutdown:
            raise ShutDown
        if self.maxsize > 0 and self._qsize() >= self.maxsize:
            if not block:
                raise Full
            raise Full  # single-threaded — can't actually wait
        self._put(item)
        self.unfinished_tasks += 1

    def put_nowait(self, item):
        return self.put(item, block=False)

    def get(self, block=True, timeout=None):
        if self._qsize() == 0:
            if self._shutdown:
                raise ShutDown
            if not block:
                raise Empty
            raise Empty  # single-threaded — can't actually wait
        return self._get()

    def get_nowait(self):
        return self.get(block=False)

    def task_done(self):
        if self.unfinished_tasks <= 0:
            raise ValueError("task_done() called too many times")
        self.unfinished_tasks -= 1

    def join(self):
        # No real waiting possible — succeeds when every put has a
        # matching task_done.
        if self.unfinished_tasks != 0:
            raise RuntimeError("queue.join: pending unfinished tasks")

    def shutdown(self, immediate=False):
        self._shutdown = True
        if immediate:
            self._init(self.maxsize)
            self.unfinished_tasks = 0


class LifoQueue(Queue):
    """Stack: last-in, first-out."""

    def _init(self, maxsize):
        self.queue = []

    def _put(self, item):
        self.queue.append(item)

    def _get(self):
        return self.queue.pop()


class PriorityQueue(Queue):
    """Heap-backed priority queue. Items are ordered ascending."""

    def _init(self, maxsize):
        self.queue = []

    def _put(self, item):
        heapq.heappush(self.queue, item)

    def _get(self):
        return heapq.heappop(self.queue)


class SimpleQueue:
    """Unbounded, thread-safe-ish FIFO without task tracking."""

    def __init__(self):
        self._items = []

    def put(self, item, block=True, timeout=None):
        self._items.append(item)

    def put_nowait(self, item):
        return self.put(item, block=False)

    def get(self, block=True, timeout=None):
        if not self._items:
            raise Empty
        return self._items.pop(0)

    def get_nowait(self):
        return self.get(block=False)

    def qsize(self):
        return len(self._items)

    def empty(self):
        return len(self._items) == 0


__all__ = [
    "Empty",
    "Full",
    "ShutDown",
    "Queue",
    "LifoQueue",
    "PriorityQueue",
    "SimpleQueue",
]

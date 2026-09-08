"""Stand-in for CPython's C accelerator module `_queue`
(Modules/_queuemodule.c): the `Empty` exception and the unbounded,
thread-safe `SimpleQueue`.

This follows the C module's design, not `queue._PySimpleQueue`'s. The
difference is reentrancy, which CPython names as the reason the C
version exists: `_PySimpleQueue` counts items with a
`threading.Semaphore`, so `put` takes the semaphore's Condition lock,
and a `put` that runs *inside* a `get` on the same thread deadlocks on
it. Signal handlers do exactly that. gunicorn's arbiter handlers are
`SIG_QUEUE.put_nowait(sig)`, and its main loop spends its life in
`SIG_QUEUE.get(timeout=1.0)` / `get_nowait()`; a SIGCHLD landing while
the loop is inside `get`'s `with self._cond:` hung the master for good
(the RFC 0077 ecosystem lane's gunicorn_gevent SIGTERM shutdown on
Linux). The C module instead keeps one bare `PyThread_type_lock` as a
wake gate: a waiting `get` holds it while the queue is empty and `put`
releases it. `put` never blocks and never acquires anything, so it is
safe to run from a handler at any point of `get`.
"""

import _thread
from collections import deque
from time import monotonic as time

__all__ = ['Empty', 'SimpleQueue']


class Empty(Exception):
    'Exception raised by Queue.get(block=0)/get_nowait().'


class SimpleQueue:
    '''Simple, unbounded FIFO queue.

    Reentrant: `put` may run from a signal handler.
    '''

    def __init__(self):
        self._queue = deque()
        # The wake gate (`self->lock` / `self->locked` in
        # _queuemodule.c). `_locked` is True while a `get` has taken the
        # gate because the queue was empty; `put` then releases it. A
        # bare `_thread.lock` is deliberately not owner-checked, so a
        # putter on another thread (or in a handler) may release it.
        self._lock = _thread.allocate_lock()
        self._locked = False

    # Native builtin (`_weave_queue.simplequeue_put`): appends to
    # `self._queue` and opens the gate when a getter holds it, never
    # blocking. Being a builtin matters — `SimpleQueue.put.__get__(inst)`
    # must produce a `builtin_function_or_method`, matching the C
    # accelerator (test_types.test_method_descriptor_crash).
    from _weave_queue import simplequeue_put as put

    def get(self, block=True, timeout=None):
        '''Remove and return an item from the queue.

        If optional args 'block' is true and 'timeout' is None (the
        default), block if necessary until an item is available. If
        'timeout' is a non-negative number, it blocks at most 'timeout'
        seconds and raises the Empty exception if no item was available
        within that time. Otherwise ('block' is false), return an item if
        one is immediately available, else raise the Empty exception
        ('timeout' is ignored in that case).
        '''
        if timeout is not None and timeout < 0:
            raise ValueError("'timeout' must be a non-negative number")
        endtime = None
        while not self._queue:
            if not block:
                raise Empty
            if timeout is not None:
                if endtime is None:
                    endtime = time() + timeout
                else:
                    timeout = endtime - time()
                    if timeout <= 0:
                        raise Empty
            # `simplequeue_get`: a non-blocking try first, then wait on
            # the gate for the remaining budget. A `put` (from any
            # thread, or from a handler that interrupts this wait)
            # releases it; a wake with the queue still empty just loops.
            if not self._lock.acquire(False):
                if timeout is None:
                    self._lock.acquire()
                elif not self._lock.acquire(True, timeout):
                    raise Empty
            self._locked = True
        return self._queue.popleft()

    def put_nowait(self, item):
        '''Put an item into the queue without blocking.

        This is exactly equivalent to `put(item, block=False)` and is only
        provided for compatibility with the Queue class.
        '''
        return self.put(item, block=False)

    def get_nowait(self):
        '''Remove and return an item from the queue without blocking.

        Only get an item if one is immediately available. Otherwise
        raise the Empty exception.
        '''
        return self.get(block=False)

    def empty(self):
        '''Return True if the queue is empty, False otherwise (not reliable!).'''
        return len(self._queue) == 0

    def qsize(self):
        '''Return the approximate size of the queue (not reliable!).'''
        return len(self._queue)

    __class_getitem__ = classmethod(__import__('types').GenericAlias)

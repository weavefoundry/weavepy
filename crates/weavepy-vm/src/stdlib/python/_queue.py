"""Stand-in for CPython's C accelerator module `_queue`
(Modules/_queuemodule.c): the `Empty` exception and the unbounded,
thread-safe `SimpleQueue`. The implementation mirrors
`queue._PySimpleQueue` — CPython documents the two as semantically
identical (the C version exists for speed and reentrancy only).
"""

import threading
from collections import deque
from time import monotonic as time

__all__ = ['Empty', 'SimpleQueue']


class Empty(Exception):
    'Exception raised by Queue.get(block=0)/get_nowait().'


class SimpleQueue:
    '''Simple, unbounded FIFO queue.

    This pure Python implementation is not reentrant.
    '''

    def __init__(self):
        self._queue = deque()
        self._count = threading.Semaphore(0)

    # Native builtin (`_weave_queue.simplequeue_put`): appends to
    # `self._queue` and releases `self._count`, never blocking. Being a
    # builtin matters — `SimpleQueue.put.__get__(inst)` must produce a
    # `builtin_function_or_method`, matching the C accelerator
    # (test_types.test_method_descriptor_crash).
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
        if not self._count.acquire(block, timeout):
            raise Empty
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

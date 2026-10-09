"""Temporaries a compiled loop holds die before its explicit collection.

Tier 2 keeps the objects its dynamic calls return in a table for the life
of the native activation. A call's result that the loop drops at once (a
`queue.get()` passed straight to `weakref.ref`) must still be gone once
that loop runs `gc.collect()`, as in the interpreter (the shape of
CPython's test_queue `test_references`). The queue is filled by a helper,
whose own table dies when it returns.
"""

import gc
import queue
import unittest
import weakref


class Item:
    pass


def drained(q, n):
    alive = 0
    for _ in range(n):
        wr = weakref.ref(q.get())
        gc.collect()
        alive += wr() is not None
    return alive


def fill(q, n, cls=Item):
    for _ in range(n):
        q.put(cls())


def drained_with_local_class(q, n):
    class Local:
        pass

    fill(q, n, Local)
    alive = 0
    for _ in range(n):
        wr = weakref.ref(q.get())
        gc.collect()
        alive += wr() is not None
    return alive


class PinnedTemporariesCollectTest(unittest.TestCase):
    def test_native_queue(self):
        for _ in range(50):
            q = queue.SimpleQueue()
            fill(q, 20)
            self.assertEqual(drained(q, 20), 0)

    def test_python_queue(self):
        for _ in range(50):
            q = queue._PySimpleQueue()
            fill(q, 20)
            self.assertEqual(drained(q, 20), 0)

    def test_class_defined_in_function(self):
        for _ in range(50):
            self.assertEqual(drained_with_local_class(queue.SimpleQueue(), 20), 0)


if __name__ == "__main__":
    unittest.main()

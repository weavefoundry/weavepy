"""Functions whose ordinary path is pure evaluate without a frame.

A function with a `try` statement qualifies when its ordinary path (not
its handlers) is pure: the frameless evaluation declines as soon as
something would raise, and the ordinary call then runs the handler. A
handler's effects must happen exactly once, and an exception the
function doesn't catch must keep its traceback.
"""

import traceback
import unittest


class Lazy:
    __slots__ = ("_cached", "computed")

    def __init__(self):
        self.computed = 0

    @property
    def value(self):
        try:
            return self._cached
        except AttributeError:
            self.computed += 1
            self._cached = 42
            return self._cached


def lookup(d, k):
    try:
        return d[k]
    except KeyError:
        return -1


def strict(d, k):
    try:
        return d[k]
    except TypeError:
        return -2


class LeafWithHandlersTest(unittest.TestCase):
    def test_cached_property(self):
        obj = Lazy()
        for _ in range(1000):
            self.assertEqual(obj.value, 42)
        self.assertEqual(obj.computed, 1)

    def test_handler_runs_on_a_miss(self):
        d = {"a": 1}
        total = 0
        for i in range(2000):
            total += lookup(d, "a" if i % 3 else "b")
        self.assertEqual(total, 1333 - 667)

    def test_uncaught_keeps_traceback(self):
        d = {"a": 1}
        for _ in range(500):
            self.assertEqual(strict(d, "a"), 1)
        try:
            strict(d, "missing")
        except KeyError as e:
            names = [f.name for f in traceback.extract_tb(e.__traceback__)]
        else:
            self.fail("KeyError not raised")
        self.assertEqual(names[-1], "strict")


if __name__ == "__main__":
    unittest.main()
